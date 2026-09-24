mod chat_info;
mod deletion;
mod editing;
mod entities;
mod folders;
mod forwarding;
mod invites;
mod local;
mod media_cache;
mod members;
mod message_actions;
mod notifications;
mod pins;
mod polls;
mod reactions;
mod reads;
mod requests;
mod search;
mod sharing;
mod stickers;
mod telemetry;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use grammers_client::client::{LoginToken, PasswordToken, UpdatesConfiguration};
use grammers_client::media::{Media, PhotoSize};
use grammers_client::message::{InputMessage, Message as TelegramMessage};
use grammers_client::peer::{Dialog, Peer, User};
use grammers_client::sender::SenderPoolHandle;
use grammers_client::tl::{self, enums::Dialog as RawDialog};
use grammers_client::update::Update;
use grammers_client::{Client, InvocationError, SenderPool, SignInError};
use grammers_session::Session;
use grammers_session::storages::SqliteSession;
use grammers_session::types::{
    ChannelKind, PeerId, PeerInfo, PeerKind, PeerRef, UpdateState, UpdatesState,
};
use grammers_session::updates::UpdatesLike;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::config::Config;
use crate::event::{AuthPrompt, ConnectionStatus, NetworkEvent, TelegramCommand};
use crate::model::{
    Attachment, AttachmentKind, Chat, ChatId, ChatKind, Delivery, Message, MessageButton,
    MessageButtonKind, MessageLink, ReplyInfo, sanitize_terminal_line, sanitize_terminal_text,
};

pub(crate) const HISTORY_LIMIT: usize = 80;
const COMMAND_QUEUE_CAPACITY: usize = 32;
const EVENT_QUEUE_CAPACITY: usize = 64;
const TRANSIENT_SENDER_NAME_LIMIT: usize = 256;
const MESSAGE_SENDER_CACHE_LIMIT: usize = 512;
const UNRESOLVED_REFRESH_COOLDOWN: Duration = Duration::from_secs(5);
const MAX_CONCURRENT_TRANSFERS: usize = 3;
const MAX_PENDING_TRANSFERS: usize = 32;
const QR_MIGRATION_LIMIT: usize = 4;
const MIN_QR_REFRESH_DELAY: Duration = Duration::from_millis(500);
const DEFAULT_QR_REFRESH_DELAY: Duration = Duration::from_secs(30);
const MAX_QR_REFRESH_DELAY: Duration = Duration::from_secs(120);
const QR_RESTART_DELAY: Duration = Duration::from_secs(1);

pub struct TelegramHandle {
    pub measure_latency: tokio::sync::watch::Sender<bool>,
    pub commands: mpsc::Sender<TelegramCommand>,
    pub events: mpsc::Receiver<NetworkEvent>,
    pub task: tokio::task::JoinHandle<()>,
    pub active_chat: tokio::sync::watch::Sender<Option<ChatId>>,
}

#[derive(Default)]
struct MetadataRefresh {
    in_flight: bool,
    dirty: bool,
}

#[derive(Default)]
struct WorkerCache {
    cloud_search: Option<(u64, tokio::task::AbortHandle)>,
    search_sender: Option<PeerRef>,
    notify_revision: u64,
    notification_peers: HashMap<ChatId, PeerRef>,
    notification_peer_order: VecDeque<ChatId>,
    transfer_slots: Option<Arc<tokio::sync::Semaphore>>,
    upload_order: HashMap<ChatId, tokio::sync::oneshot::Receiver<()>>,
    dialogs: MetadataRefresh,
    folders: MetadataRefresh,
    dialog_pins: MetadataRefresh,
    message_pins: MetadataRefresh,
    active_chat: Option<ChatId>,
    self_id: i64,
    channel_pts: HashMap<ChatId, i32>,
    peers: HashMap<ChatId, PeerRef>,
    /// Peers opened explicitly through supported Telegram links remain usable
    /// even when they are not part of the account's dialog snapshot.
    linked_peers: HashSet<ChatId>,
    /// Highest message identifier already represented by each dialog snapshot.
    top_messages: HashMap<ChatId, i32>,
    read_outbox: HashMap<ChatId, i32>,
    names: HashMap<PeerId, String>,
    /// Handles share the name cache's bounds and eviction lifecycle.
    usernames: HashMap<PeerId, String>,
    /// Names belonging to the current complete dialog snapshot are never
    /// evicted by the bounded transient sender-name cache.
    dialog_name_ids: HashSet<PeerId>,
    transient_name_order: VecDeque<PeerId>,
    /// A small sender index makes reply labels useful without fetching every
    /// reply target separately while loading history.
    message_senders: HashMap<(ChatId, i32), String>,
    message_sender_order: VecDeque<(ChatId, i32)>,
    /// Broadcasts are intentionally hidden; unresolved channel-shaped peers are
    /// hidden until a later dialog refresh can identify them as a group.
    hidden_broadcasts: HashSet<ChatId>,
    /// Megagroups share Telegram's channel-shaped identifier space. Remember
    /// known groups so short updates do not need an RPC merely to classify them.
    visible_channel_groups: HashSet<ChatId>,
    /// One account-wide cooldown prevents distinct unresolved peers from each
    /// triggering a complete dialog scan. Once it expires, any unresolved peer
    /// can retry the refresh.
    last_unresolved_refresh: Option<Instant>,
    /// Created lazily so text-only sessions never create a media directory.
    download_dir: Option<PathBuf>,
    media_root: PathBuf,
    cache_owner: Option<Arc<std::fs::File>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum TelegramLink {
    Public {
        username: String,
        message_id: Option<i32>,
    },
    Private {
        chat_id: ChatId,
        message_id: i32,
    },
}

enum CodeOutcome {
    Authorized,
    Password(Box<PasswordToken>),
    Restart,
}

enum LoginAttempt {
    Phone { phone: String, token: LoginToken },
    Qr,
    Restart,
}

enum AuthInterruption {
    None,
    Restart,
    Shutdown,
}

enum TransferCompletion {
    Send {
        chat_id: ChatId,
        local_id: i32,
        attachment: crate::staging::Attachment,
        caption: String,
        reply_to: Option<i32>,
        result: Box<Result<TelegramMessage, String>>,
    },
    Preview {
        chat_id: ChatId,
        message_id: i32,
        request_id: u64,
        result: Result<PathBuf, String>,
    },
    StickerThumb {
        request_id: u64,
        document_id: i64,
        result: Result<PathBuf, String>,
    },
    Download {
        request_id: u64,
        chat_id: ChatId,
        message_id: i32,
        result: Result<PathBuf, String>,
    },
}

#[must_use]
pub fn spawn(config: Config) -> TelegramHandle {
    let (measure_tx, measure_rx) = tokio::sync::watch::channel(config.measure_latency);
    let (active_tx, active_rx) = tokio::sync::watch::channel(None);
    let (command_tx, command_rx) = mpsc::channel(COMMAND_QUEUE_CAPACITY);
    let (event_tx, event_rx) = mpsc::channel(EVENT_QUEUE_CAPACITY);
    let task = tokio::spawn(async move {
        if let Err(error) = Box::pin(local::serve(
            config,
            command_rx,
            event_tx.clone(),
            active_rx,
            measure_rx,
        ))
        .await
        {
            let _ = event_tx
                .send(NetworkEvent::Fatal(describe_worker_error(&error)))
                .await;
        }
    });
    TelegramHandle {
        measure_latency: measure_tx,
        active_chat: active_tx,
        commands: command_tx,
        events: event_rx,
        task,
    }
}

fn describe_worker_error(error: &anyhow::Error) -> String {
    for cause in error.chain() {
        if let Some(InvocationError::Session(source)) = cause.downcast_ref::<InvocationError>() {
            return format!("Telegram session storage failed: {source}");
        }
    }
    format!("{error:#}")
}

#[allow(clippy::too_many_lines)]
async fn run(
    config: Config,
    mut commands: mpsc::Receiver<TelegramCommand>,
    events: mpsc::Sender<NetworkEvent>,
    mut bootstrap: local::Bootstrap,
    mut active_chat: tokio::sync::watch::Receiver<Option<ChatId>>,
    measure_latency: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    config.prepare_session_dir()?;
    events
        .send(NetworkEvent::Status(ConnectionStatus::Connecting))
        .await
        .ok();

    let session = Arc::new(
        SqliteSession::open(&config.session_path)
            .await
            .with_context(|| format!("failed to open {}", config.session_path.display()))?,
    );
    config.protect_session_file()?;
    let mut connection_params = grammers_mtsender::ConnectionParams::default();
    if let Some(proxy) = &config.proxy {
        connection_params.proxy_url = Some(proxy.url.clone());
        connection_params.proxy_fallback = proxy.fallback;
    }
    let SenderPool {
        runner,
        handle,
        mut updates,
    } = SenderPool::with_configuration(session.clone(), config.api_id, connection_params);
    let client = Client::new(handle.clone());
    // JoinSet aborts the sender on account switches and initialization failures,
    // including when the owner itself is aborted during shutdown.
    let mut pool_tasks = JoinSet::new();
    let mut observations = JoinSet::new();
    pool_tasks.spawn(runner.run());

    let result: Result<()> = Box::pin(async {
        let initially_authorized = client.is_authorized().await?;
        let me = if initially_authorized {
            client.get_me().await?
        } else {
            loop {
                authenticate(
                    &client,
                    config.api_id,
                    &config.api_hash,
                    &session,
                    &handle.thin,
                    &mut updates,
                    &mut commands,
                    &events,
                )
                .await?;

                let me = client.get_me().await?;
                match take_auth_interruption(&mut commands) {
                    AuthInterruption::None => break me,
                    AuthInterruption::Restart => {
                        client
                            .sign_out()
                            .await
                            .context("could not cancel the newly authorized Telegram session")?;
                    }
                    // The completed session remains available on the next
                    // launch, but never show Ready after an explicit shutdown.
                    AuthInterruption::Shutdown => bail!("login cancelled"),
                }
            }
        };
        let user_id = me.id().bare_id().context("Telegram account has no stable identifier")?;
        if bootstrap.account_id.is_some_and(|previous| previous != user_id) {
            bootstrap.cursor = crate::cache::SyncCursor::default();
            bootstrap.chats.clear();
            events.send(NetworkEvent::CacheAccountReset { user_id }).await?;
        }
        events.send(NetworkEvent::AccountIdentity { user_id }).await?;
        let user_name = safe_name(Some(&me.full_name()), "You");
        events.send(NetworkEvent::Ready { user_name }).await.ok();
        observations.spawn(telemetry::observe(client.clone(), session.clone(), events.clone(), measure_latency));

        let mut media_root = config.session_path.as_os_str().to_os_string();
        media_root.push(".media");
        let mut cache = WorkerCache { cache_owner: Some(bootstrap.cache_owner.clone()), media_root: PathBuf::from(media_root), self_id: user_id, folders: MetadataRefresh { dirty: true, ..MetadataRefresh::default() }, ..WorkerCache::default() };
        cache_sender_name(&mut cache, me.id(), safe_name(Some(&me.full_name()), "You"));
        cache_username(&mut cache, me.id(), me.username());
        for chat in bootstrap.chats {
            if let Some(id) = PeerId::from_bot_api_dialog_id(chat.id) {
                if let Some(peer) = session.peer_ref(id).await? { cache.peers.insert(chat.id, peer); }
                if let Some(top) = chat.last_message_id { cache.top_messages.insert(chat.id, top); }
                cache.names.insert(id, chat.title);
                if chat.kind == ChatKind::Group && id.kind() == PeerKind::Channel {
                    cache.visible_channel_groups.insert(chat.id);
                }
            }
        }
        let cursor = bootstrap.cursor;
        for &(id, pts) in &cursor.channels {
            cache.channel_pts.insert(PeerId::channel_unchecked(id).bot_api_dialog_id_unchecked(), pts);
        }
        events
            .send(NetworkEvent::Status(ConnectionStatus::Online))
            .await
            .ok();

        let mut updates = client
            .stream_updates(
                updates,
                UpdatesConfiguration {
                    catch_up: true,
                    // Never truncate catch-up batches; losing payloads while advancing
                    // PTS would leave the view permanently stale.
                    update_queue_limit: None,
                },
            )
            .await
            .map_err(anyhow::Error::from_boxed)?;
        updates.restore_state(UpdatesState {
            pts: cursor.pts, qts: cursor.qts, date: cursor.date, seq: cursor.seq,
            channels: cursor.channels.into_iter().map(|(id, pts)| grammers_session::types::ChannelState { id, pts }).collect(),
        });
        let active_channel = updates.active_channel();
        cache.active_chat = *active_chat.borrow();
        cache.message_pins.dirty = true;
        signal_active_channel(&cache, *active_chat.borrow_and_update(), &active_channel);
        let mut recovering = false;
        let mut transfers = JoinSet::new();
        let mut requests = JoinSet::new();
        cache.dialog_pins.dirty = true;
        requests::refresh_dialogs(&client, &mut cache, &events, &mut requests).await?;
        requests::refresh_pending(&client, &mut cache, &events, &mut requests).await?;

        'worker: loop {
            requests::refresh_pending(&client, &mut cache, &events, &mut requests).await?;
            // Grammers can change its message-box state before awaiting peer
            // storage. Keep this future alive when commands or RPCs complete.
            let next = updates.next_batch();
            tokio::pin!(next);
            let update = loop {
                tokio::select! {
                    update = &mut next => break update,
                    changed = active_chat.changed() => {
                        if changed.is_err() { break 'worker; }
                        cache.active_chat = *active_chat.borrow();
                        cache.message_pins.dirty = true;
                        signal_active_channel(&cache, *active_chat.borrow_and_update(), &active_channel);
                    }
                    command = commands.recv(), if requests.len() < requests::MAX_REQUESTS => {
                        let Some(command) = command else { break 'worker; };
                        if handle_command(command, &client, &mut cache, &events,
                            &mut transfers, &mut requests).await? {
                            break 'worker;
                        }
                    }
                    completion = requests.join_next(), if !requests.is_empty() => {
                        match completion {
                            Some(Ok(completion)) => {
                                requests::complete(completion, &mut cache, &events).await?;
                                signal_active_channel(&cache, *active_chat.borrow(), &active_channel);
                            }
                            Some(Err(error)) if error.is_cancelled() => {}
                            Some(Err(error)) => bail!("Telegram request task failed: {error}"),
                            None => {}
                        }
                    }
                    completion = transfers.join_next(), if !transfers.is_empty() => {
                        match completion {
                            Some(Ok(completion)) => process_transfer_completion(completion, &mut cache, &events).await?,
                            Some(Err(error)) => bail!("Telegram transfer task failed: {error}"),
                            None => {}
                        }
                    }
                }
                requests::refresh_pending(&client, &mut cache, &events, &mut requests).await?;
            };
            if matches!(update, Err(InvocationError::Dropped)) {
                bail!("Telegram update stream closed");
            }
            match update {
                Ok((batch, state)) => {
                    for update in batch {
                        Box::pin(process_update(Ok(update), &session, &client, &mut cache,
                            &events, &mut recovering, &mut requests)).await?;
                    }
                    events.send(NetworkEvent::SyncCheckpoint(crate::cache::SyncCursor {
                        pts: state.pts, qts: state.qts, date: state.date, seq: state.seq,
                        channels: state.channels.into_iter().map(|channel| (channel.id, channel.pts)).collect(),
                    })).await?;
                }
                Err(error) => {
                    Box::pin(process_update(Err(error), &session, &client, &mut cache,
                        &events, &mut recovering, &mut requests)).await?;
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
        }

        requests.shutdown().await;
        transfers.shutdown().await;

        Ok(())
    })
    .await;
    observations.shutdown().await;
    handle.quit();
    let _ = pool_tasks.join_next().await;
    result
}

fn signal_active_channel(
    cache: &WorkerCache,
    chat_id: Option<ChatId>,
    sender: &tokio::sync::watch::Sender<Option<(i64, i32)>>,
) {
    let value = chat_id.and_then(|id| {
        let pts = *cache.channel_pts.get(&id)?;
        let peer = cache.peers.get(&id)?;
        (peer.id.kind() == PeerKind::Channel).then(|| (peer.id.bare_id_unchecked(), pts))
    });
    sender.send_if_modified(|current| {
        if *current == value {
            false
        } else {
            *current = value;
            true
        }
    });
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
async fn process_update(
    update: Result<Update, InvocationError>,
    session: &SqliteSession,
    client: &Client,
    cache: &mut WorkerCache,
    events: &mpsc::Sender<NetworkEvent>,
    recovering: &mut bool,
    requests: &mut JoinSet<requests::Completion>,
) -> Result<()> {
    match update {
        Ok(Update::NewMessage(update)) => {
            restore_online_status(events, recovering).await;
            let message = update.into_inner();
            if message.mentioned() {
                cache.dialogs.dirty = true;
            }
            let Some(hidden) = is_hidden_broadcast(session, &message, cache).await? else {
                if requests.len() < requests::MAX_REQUESTS && begin_unresolved_refresh(cache) {
                    requests::refresh_dialogs(client, cache, events, requests).await?;
                }
                events
                    .send(NetworkEvent::CacheMessage(map_message(&message, cache)?))
                    .await?;
                return Ok(());
            };
            if hidden {
                return Ok(());
            }
            let chat_id = peer_id(&message)?;
            if !cache.peers.contains_key(&chat_id) {
                cache.dialogs.dirty = true;
            }
            if let Some(peer) = message
                .peer_ref()
                .await
                .map_err(anyhow::Error::from_boxed)?
            {
                cache.peers.insert(chat_id, peer);
            }
            if message.mentioned()
                && let Some(sender) = message.sender()
                && let Ok(Some(peer)) = sender.to_ref().await
                && let Some(id) = peer.id.bot_api_dialog_id().filter(|id| *id > 0)
            {
                if cache.notification_peers.insert(id, peer).is_none() {
                    cache.notification_peer_order.push_back(id);
                }
                while cache.notification_peer_order.len() > 256 {
                    if let Some(old) = cache.notification_peer_order.pop_front() {
                        cache.notification_peers.remove(&old);
                    }
                }
            }
            let message_id = message.id();
            let after_dialog_snapshot = advance_dialog_watermark(cache, chat_id, message_id);
            let message = map_message(&message, cache)?;
            let event = if after_dialog_snapshot {
                NetworkEvent::NewMessage(message)
            } else {
                NetworkEvent::MessageUpdated(message)
            };
            events.send(event).await.ok();
        }
        Ok(Update::MessageEdited(update)) => {
            restore_online_status(events, recovering).await;
            let message = update.into_inner();
            cache.message_pins.dirty |= message.pinned();
            if message.mentioned() {
                cache.dialogs.dirty = true;
            }
            let Some(hidden) = is_hidden_broadcast(session, &message, cache).await? else {
                if requests.len() < requests::MAX_REQUESTS && begin_unresolved_refresh(cache) {
                    requests::refresh_dialogs(client, cache, events, requests).await?;
                }
                events
                    .send(NetworkEvent::CacheMessage(map_message(&message, cache)?))
                    .await?;
                return Ok(());
            };
            if hidden {
                return Ok(());
            }
            events
                .send(NetworkEvent::MessageUpdated(map_message(&message, cache)?))
                .await
                .ok();
        }
        Ok(Update::MessageDeleted(update)) => {
            cache.dialogs.dirty = true;
            cache.message_pins.dirty = true;
            restore_online_status(events, recovering).await;
            let channel_id = update
                .channel_id()
                .and_then(PeerId::channel)
                .and_then(PeerId::bot_api_dialog_id);
            events
                .send(NetworkEvent::MessagesDeleted {
                    channel_id,
                    message_ids: update.into_messages(),
                })
                .await
                .ok();
        }
        Ok(Update::Raw(update)) => {
            restore_online_status(events, recovering).await;
            match &update.raw {
                tl::enums::Update::MessageReactions(update) => {
                    if let Some(chat) = PeerId::from(update.peer.clone()).bot_api_dialog_id() {
                        events
                            .send(NetworkEvent::ReactionsChanged(crate::reactions::Update {
                                chat,
                                message: update.msg_id,
                                summary: reactions::summary(&update.reactions),
                            }))
                            .await?;
                    }
                }
                tl::enums::Update::MessagePoll(update) => {
                    events
                        .send(NetworkEvent::PollChanged(polls::update(update)))
                        .await?;
                }
                tl::enums::Update::PinnedMessages(update) => {
                    if let Some(chat_id) = PeerId::from(update.peer.clone()).bot_api_dialog_id() {
                        events
                            .send(NetworkEvent::MessagePinsChanged {
                                chat_id,
                                message_ids: update.messages.clone(),
                                pinned: update.pinned,
                            })
                            .await?;
                        cache.message_pins.dirty |= cache.active_chat == Some(chat_id);
                    }
                }
                tl::enums::Update::PinnedChannelMessages(update) => {
                    let chat_id =
                        PeerId::channel_unchecked(update.channel_id).bot_api_dialog_id_unchecked();
                    events
                        .send(NetworkEvent::MessagePinsChanged {
                            chat_id,
                            message_ids: update.messages.clone(),
                            pinned: update.pinned,
                        })
                        .await?;
                    cache.message_pins.dirty |= cache.active_chat == Some(chat_id);
                }
                tl::enums::Update::DialogPinned(_) | tl::enums::Update::PinnedDialogs(_) => {
                    cache.dialog_pins.dirty = true;
                }
                tl::enums::Update::DialogFilter(_)
                | tl::enums::Update::DialogFilterOrder(_)
                | tl::enums::Update::DialogFilters => {
                    cache.folders.dirty = true;
                }
                tl::enums::Update::Channel(update) => {
                    events
                        .send(NetworkEvent::ChatInfoInvalidated {
                            chat_id: PeerId::channel_unchecked(update.channel_id)
                                .bot_api_dialog_id_unchecked(),
                        })
                        .await?;
                    cache.dialogs.dirty = true;
                }
                tl::enums::Update::Chat(update) => {
                    events
                        .send(NetworkEvent::ChatInfoInvalidated {
                            chat_id: PeerId::chat_unchecked(update.chat_id)
                                .bot_api_dialog_id_unchecked(),
                        })
                        .await?;
                    cache.dialogs.dirty = true;
                }
                tl::enums::Update::ChatDefaultBannedRights(update) => {
                    if let Some(chat_id) = PeerId::from(update.peer.clone()).bot_api_dialog_id() {
                        events
                            .send(NetworkEvent::ChatInfoInvalidated { chat_id })
                            .await?;
                    }
                }
                tl::enums::Update::NotifySettings(update) => {
                    cache.notify_revision = cache.notify_revision.wrapping_add(1);
                    cache.dialogs.dirty = true;
                    events
                        .send(NetworkEvent::NotificationSettingsChanged)
                        .await?;
                    if let tl::enums::NotifyPeer::Peer(target) = &update.peer
                        && let Some(chat_id) = PeerId::from(target.peer.clone()).bot_api_dialog_id()
                        && let tl::enums::PeerNotifySettings::Settings(settings) =
                            &update.notify_settings
                        && let Some(until) = settings.mute_until
                    {
                        events
                            .send(NetworkEvent::ChatMuteChanged {
                                chat_id,
                                until: i64::from(until),
                            })
                            .await?;
                    }
                }
                tl::enums::Update::ReadMessagesContents(update) => {
                    cache.dialogs.dirty = true;
                    events
                        .send(NetworkEvent::MessageContentsRead {
                            channel_id: None,
                            message_ids: update.messages.clone(),
                        })
                        .await?;
                }
                tl::enums::Update::ChannelReadMessagesContents(update) => {
                    cache.dialogs.dirty = true;
                    events
                        .send(NetworkEvent::MessageContentsRead {
                            channel_id: Some(
                                PeerId::channel_unchecked(update.channel_id)
                                    .bot_api_dialog_id_unchecked(),
                            ),
                            message_ids: update.messages.clone(),
                        })
                        .await?;
                }
                tl::enums::Update::PeerSettings(_) => {
                    cache.dialogs.dirty = true;
                }
                tl::enums::Update::DialogUnreadMark(update) => {
                    if update.saved_peer_id.is_none()
                        && let tl::enums::DialogPeer::Peer(peer) = &update.peer
                        && let Some(chat_id) = PeerId::from(peer.peer.clone()).bot_api_dialog_id()
                    {
                        events
                            .send(NetworkEvent::ChatUnreadChanged {
                                chat_id,
                                unread: update.unread,
                            })
                            .await?;
                    }
                    cache.dialogs.dirty = true;
                }
                tl::enums::Update::FolderPeers(update) => {
                    cache.dialogs.dirty = true;
                    cache.dialog_pins.dirty = true;
                    for peer in &update.folder_peers {
                        let tl::enums::FolderPeer::Peer(peer) = peer;
                        if let Some(chat_id) = PeerId::from(peer.peer.clone()).bot_api_dialog_id() {
                            events
                                .send(NetworkEvent::ArchiveChanged {
                                    chat_id,
                                    archived: peer.folder_id == 1,
                                })
                                .await?;
                        }
                    }
                }
                tl::enums::Update::ReadHistoryInbox(read) => {
                    if let Some(chat_id) = PeerId::from(read.peer.clone()).bot_api_dialog_id() {
                        events
                            .send(NetworkEvent::UnreadChanged {
                                chat_id,
                                max_id: read.max_id,
                                unread: u32::try_from(read.still_unread_count.max(0))
                                    .unwrap_or_default(),
                            })
                            .await?;
                    }
                }
                tl::enums::Update::ReadChannelInbox(read) => {
                    events
                        .send(NetworkEvent::UnreadChanged {
                            max_id: read.max_id,
                            chat_id: PeerId::channel_unchecked(read.channel_id)
                                .bot_api_dialog_id_unchecked(),
                            unread: u32::try_from(read.still_unread_count.max(0))
                                .unwrap_or_default(),
                        })
                        .await?;
                }
                tl::enums::Update::PtsChanged => {
                    cache.dialogs.dirty = true;
                    cache.folders.dirty = true;
                    cache.dialog_pins.dirty = true;
                    cache.message_pins.dirty = true;
                    events
                        .send(NetworkEvent::CacheInvalidated { chat_id: None })
                        .await?;
                }
                tl::enums::Update::ChannelTooLong(update) => {
                    let chat_id =
                        PeerId::channel_unchecked(update.channel_id).bot_api_dialog_id_unchecked();
                    cache.dialogs.dirty = true;
                    cache.message_pins.dirty |= cache.active_chat == Some(chat_id);
                    events
                        .send(NetworkEvent::CacheInvalidated {
                            chat_id: Some(chat_id),
                        })
                        .await?;
                }
                grammers_client::tl::enums::Update::ReadHistoryOutbox(read) => {
                    if let Some(chat_id) = PeerId::from(read.peer.clone()).bot_api_dialog_id() {
                        cache
                            .read_outbox
                            .entry(chat_id)
                            .and_modify(|max_id| *max_id = (*max_id).max(read.max_id))
                            .or_insert(read.max_id);
                        events
                            .send(NetworkEvent::MessagesRead {
                                chat_id,
                                max_id: read.max_id,
                            })
                            .await
                            .ok();
                    }
                }
                grammers_client::tl::enums::Update::ReadChannelOutbox(read) => {
                    let chat_id =
                        PeerId::channel_unchecked(read.channel_id).bot_api_dialog_id_unchecked();
                    cache
                        .read_outbox
                        .entry(chat_id)
                        .and_modify(|max_id| *max_id = (*max_id).max(read.max_id))
                        .or_insert(read.max_id);
                    events
                        .send(NetworkEvent::MessagesRead {
                            chat_id,
                            max_id: read.max_id,
                        })
                        .await
                        .ok();
                }
                _ => {}
            }
        }
        Ok(_) => restore_online_status(events, recovering).await,
        Err(error) => {
            if !*recovering {
                events
                    .send(NetworkEvent::Status(ConnectionStatus::Reconnecting))
                    .await
                    .ok();
                events
                    .send(NetworkEvent::Error(format!(
                        "Telegram update stream: {}",
                        describe_worker_error(&error.into())
                    )))
                    .await
                    .ok();
                *recovering = true;
            }
        }
    }
    Ok(())
}

fn advance_dialog_watermark(cache: &mut WorkerCache, chat_id: ChatId, message_id: i32) -> bool {
    let is_after_snapshot = cache
        .top_messages
        .get(&chat_id)
        .is_none_or(|top| message_id > *top);
    cache
        .top_messages
        .entry(chat_id)
        .and_modify(|top| *top = (*top).max(message_id))
        .or_insert(message_id);
    is_after_snapshot
}

async fn restore_online_status(events: &mpsc::Sender<NetworkEvent>, recovering: &mut bool) {
    if *recovering {
        events
            .send(NetworkEvent::Status(ConnectionStatus::Online))
            .await
            .ok();
        *recovering = false;
    }
}

#[allow(clippy::too_many_arguments)]
async fn authenticate(
    client: &Client,
    api_id: i32,
    api_hash: &str,
    session: &SqliteSession,
    sender: &SenderPoolHandle,
    updates: &mut mpsc::UnboundedReceiver<UpdatesLike>,
    commands: &mut mpsc::Receiver<TelegramCommand>,
    events: &mpsc::Sender<NetworkEvent>,
) -> Result<()> {
    'login: loop {
        let outcome = match request_login_attempt(client, api_hash, commands, events).await? {
            LoginAttempt::Phone { phone, token } => {
                events
                    .send(NetworkEvent::Auth(AuthPrompt::Code {
                        phone: phone.trim().to_owned(),
                    }))
                    .await?;
                sign_in_with_code(client, token, commands, events).await?
            }
            LoginAttempt::Qr => {
                sign_in_with_qr(
                    client, api_id, api_hash, session, sender, updates, commands, events,
                )
                .await?
            }
            LoginAttempt::Restart => continue,
        };

        let mut password_token = match outcome {
            CodeOutcome::Authorized => return Ok(()),
            CodeOutcome::Restart => continue,
            CodeOutcome::Password(token) => *token,
        };
        if restart_auth_requested(commands)? {
            continue;
        }
        events
            .send(NetworkEvent::Auth(AuthPrompt::Password {
                hint: password_token.hint().map(ToOwned::to_owned),
            }))
            .await?;
        loop {
            let password = match commands.recv().await {
                Some(TelegramCommand::SubmitPassword(password)) => password,
                Some(TelegramCommand::RestartAuth) => continue 'login,
                Some(TelegramCommand::Shutdown) | None => bail!("login cancelled"),
                _ => continue,
            };
            match client
                .check_password(password_token, password.as_bytes())
                .await
            {
                Ok(_) => match authorized_outcome(client, commands).await? {
                    CodeOutcome::Authorized => return Ok(()),
                    CodeOutcome::Restart => continue 'login,
                    CodeOutcome::Password(_) => unreachable!("authorized login cannot need 2FA"),
                },
                Err(SignInError::InvalidPassword(token)) => {
                    password_token = token;
                    if restart_auth_requested(commands)? {
                        continue 'login;
                    }
                    events
                        .send(NetworkEvent::Error("Incorrect 2FA password".to_owned()))
                        .await?;
                }
                Err(error) => {
                    if restart_auth_requested(commands)? {
                        continue 'login;
                    }
                    return Err(anyhow!(error).context("Telegram 2FA failed"));
                }
            }
        }
    }
}

async fn request_login_attempt(
    client: &Client,
    api_hash: &str,
    commands: &mut mpsc::Receiver<TelegramCommand>,
    events: &mpsc::Sender<NetworkEvent>,
) -> Result<LoginAttempt> {
    events.send(NetworkEvent::Auth(AuthPrompt::Phone)).await?;
    loop {
        match commands.recv().await {
            Some(TelegramCommand::StartQrAuth) => return Ok(LoginAttempt::Qr),
            Some(TelegramCommand::SubmitPhone(phone)) if !phone.trim().is_empty() => {
                match Box::pin(client.request_login_code(phone.trim(), api_hash)).await {
                    Ok(token) => {
                        if restart_auth_requested(commands)? {
                            return Ok(LoginAttempt::Restart);
                        }
                        return Ok(LoginAttempt::Phone { phone, token });
                    }
                    Err(error) => {
                        if restart_auth_requested(commands)? {
                            return Ok(LoginAttempt::Restart);
                        }
                        events
                            .send(NetworkEvent::Error(format!(
                                "Could not request a login code: {error}"
                            )))
                            .await?;
                    }
                }
            }
            Some(TelegramCommand::Shutdown) | None => bail!("login cancelled"),
            _ => {}
        }
    }
}

// Keep the short-lived token, migration, update, expiry, and cancellation
// transitions together so secret ownership is visible in one state machine.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn sign_in_with_qr(
    client: &Client,
    api_id: i32,
    api_hash: &str,
    session: &SqliteSession,
    sender: &SenderPoolHandle,
    updates: &mut mpsc::UnboundedReceiver<UpdatesLike>,
    commands: &mut mpsc::Receiver<TelegramCommand>,
    events: &mpsc::Sender<NetworkEvent>,
) -> Result<CodeOutcome> {
    let mut displayed_token: Option<Vec<u8>> = None;
    loop {
        if restart_auth_requested(commands)? {
            return Ok(CodeOutcome::Restart);
        }
        let response = match client
            .invoke(&tl::functions::auth::ExportLoginToken {
                api_id,
                api_hash: api_hash.to_owned(),
                except_ids: Vec::new(),
            })
            .await
        {
            Ok(response) => response,
            Err(error) if error.is("SESSION_PASSWORD_NEEDED") => {
                return Ok(CodeOutcome::Password(Box::new(
                    get_password_token(client).await?,
                )));
            }
            Err(error) if error.is("AUTH_RESTART") => {
                tokio::time::sleep(QR_RESTART_DELAY).await;
                continue;
            }
            Err(error) => {
                if restart_auth_requested(commands)? {
                    return Ok(CodeOutcome::Restart);
                }
                events
                    .send(NetworkEvent::Error(format!(
                        "Could not start QR login: {error}. Press Tab to try again"
                    )))
                    .await?;
                return Ok(CodeOutcome::Restart);
            }
        };

        let migrated = match follow_qr_migration(client, session, sender, response).await {
            Ok(response) => response,
            Err(error) => {
                if restart_auth_requested(commands)? {
                    return Ok(CodeOutcome::Restart);
                }
                events
                    .send(NetworkEvent::Error(format!(
                        "Could not continue QR login: {error:#}. Press Tab to try again"
                    )))
                    .await?;
                return Ok(CodeOutcome::Restart);
            }
        };
        match migrated {
            QrLoginResponse::Token(token) => {
                if restart_auth_requested(commands)? {
                    return Ok(CodeOutcome::Restart);
                }
                if displayed_token.as_deref() != Some(token.token.as_slice()) {
                    let url = qr_login_url(&token.token);
                    events
                        .send(NetworkEvent::Auth(AuthPrompt::Qr { url }))
                        .await?;
                    displayed_token = Some(token.token.clone());
                }

                let refresh = tokio::time::sleep(qr_refresh_delay(token.expires));
                tokio::pin!(refresh);
                loop {
                    tokio::select! {
                        command = commands.recv() => match command {
                            Some(TelegramCommand::RestartAuth) => {
                                return Ok(CodeOutcome::Restart);
                            }
                            Some(TelegramCommand::Shutdown) | None => bail!("login cancelled"),
                            _ => {}
                        },
                        update = updates.recv() => match update {
                            Some(update) if contains_login_token_update(&update) => break,
                            Some(_) => {}
                            None => bail!("Telegram update channel closed during QR login"),
                        },
                        () = &mut refresh => break,
                    }
                }
            }
            QrLoginResponse::Authorized(authorization) => {
                match take_auth_interruption(commands) {
                    AuthInterruption::Restart => return cancel_authorized_login(client).await,
                    AuthInterruption::Shutdown => {
                        complete_qr_login(client, session, *authorization).await?;
                        bail!("login cancelled");
                    }
                    AuthInterruption::None => {}
                }
                complete_qr_login(client, session, *authorization).await?;
                return authorized_outcome(client, commands).await;
            }
            QrLoginResponse::Password => {
                return Ok(CodeOutcome::Password(Box::new(
                    get_password_token(client).await?,
                )));
            }
            QrLoginResponse::Refresh => {}
        }
    }
}

enum QrLoginResponse {
    Token(tl::types::auth::LoginToken),
    Authorized(Box<tl::enums::auth::Authorization>),
    Password,
    Refresh,
}

async fn follow_qr_migration(
    client: &Client,
    session: &SqliteSession,
    sender: &SenderPoolHandle,
    mut response: tl::enums::auth::LoginToken,
) -> Result<QrLoginResponse> {
    let mut imported_dc = None;
    for _ in 0..QR_MIGRATION_LIMIT {
        match response {
            tl::enums::auth::LoginToken::Token(token) => {
                if let Some(dc_id) = imported_dc {
                    switch_home_dc(session, sender, dc_id).await?;
                }
                return Ok(QrLoginResponse::Token(token));
            }
            tl::enums::auth::LoginToken::Success(success) => {
                if let Some(dc_id) = imported_dc {
                    switch_home_dc(session, sender, dc_id).await?;
                }
                return Ok(QrLoginResponse::Authorized(Box::new(success.authorization)));
            }
            tl::enums::auth::LoginToken::MigrateTo(migration) => {
                imported_dc = Some(migration.dc_id);
                response = match client
                    .invoke_in_dc(
                        migration.dc_id,
                        &tl::functions::auth::ImportLoginToken {
                            token: migration.token,
                        },
                    )
                    .await
                {
                    Ok(response) => response,
                    Err(error) if error.is("SESSION_PASSWORD_NEEDED") => {
                        switch_home_dc(session, sender, migration.dc_id).await?;
                        return Ok(QrLoginResponse::Password);
                    }
                    Err(error)
                        if error.is("AUTH_TOKEN_EXPIRED")
                            || error.is("AUTH_TOKEN_ALREADY_ACCEPTED")
                            || error.is("AUTH_RESTART") =>
                    {
                        return Ok(QrLoginResponse::Refresh);
                    }
                    Err(error) => {
                        return Err(anyhow!(error).context("QR login data-center migration failed"));
                    }
                };
            }
        }
    }
    bail!("Telegram returned too many QR login data-center migrations")
}

async fn switch_home_dc(
    session: &SqliteSession,
    sender: &SenderPoolHandle,
    new_dc_id: i32,
) -> Result<()> {
    let old_dc_id = session.home_dc_id()?;
    if old_dc_id != new_dc_id {
        session.set_home_dc_id(new_dc_id).await?;
        sender.disconnect_from_dc(old_dc_id);
        // `invoke_in_dc` may have opened the target while it was still a
        // secondary connection. Recreate it after changing home so updates
        // from the newly-authorized account are forwarded normally.
        sender.disconnect_from_dc(new_dc_id);
    }
    Ok(())
}

async fn complete_qr_login(
    client: &Client,
    session: &SqliteSession,
    authorization: tl::enums::auth::Authorization,
) -> Result<()> {
    let tl::enums::auth::Authorization::Authorization(authorization) = authorization else {
        bail!("this account must first register with an official Telegram client")
    };

    // Keep this in lockstep with grammers' private `complete_login`: initialize
    // update state and persist the current user's peer before normal requests.
    let update_state = client
        .invoke(&tl::functions::updates::GetState {})
        .await
        .ok();
    let user = User::from_raw(client, authorization.user);
    let auth = user
        .to_ref()
        .await
        .map_err(|error| anyhow!("QR login returned an unusable user: {error}"))?
        .context("QR login returned no user reference")?
        .auth;
    session
        .cache_peer(&PeerInfo::User {
            id: user.id().bare_id_unchecked(),
            auth: Some(auth),
            bot: Some(user.is_bot()),
            is_self: Some(true),
        })
        .await?;
    if let Some(tl::enums::updates::State::State(state)) = update_state {
        session
            .set_update_state(UpdateState::All(UpdatesState {
                pts: state.pts,
                qts: state.qts,
                date: state.date,
                seq: state.seq,
                channels: Vec::new(),
            }))
            .await?;
    }
    Ok(())
}

async fn get_password_token(client: &Client) -> Result<PasswordToken> {
    let password = client
        .invoke(&tl::functions::account::GetPassword {})
        .await
        .context("could not load Telegram 2FA parameters")?;
    Ok(PasswordToken::new(password.into()))
}

fn contains_login_token_update(update: &UpdatesLike) -> bool {
    let UpdatesLike::Updates(updates) = update else {
        return false;
    };
    match updates {
        tl::enums::Updates::UpdateShort(update) => {
            matches!(update.update, tl::enums::Update::LoginToken)
        }
        tl::enums::Updates::Combined(updates) => updates
            .updates
            .iter()
            .any(|update| matches!(update, tl::enums::Update::LoginToken)),
        tl::enums::Updates::Updates(updates) => updates
            .updates
            .iter()
            .any(|update| matches!(update, tl::enums::Update::LoginToken)),
        _ => false,
    }
}

fn qr_refresh_delay(expires: i32) -> Duration {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let expires = u64::try_from(expires).unwrap_or_default();
    if expires >= now {
        let delay = Duration::from_secs(expires - now);
        if delay > MAX_QR_REFRESH_DELAY {
            DEFAULT_QR_REFRESH_DELAY
        } else {
            delay.max(MIN_QR_REFRESH_DELAY)
        }
    } else if now - expires <= MAX_QR_REFRESH_DELAY.as_secs() {
        // A token that expired while this prompt was being delivered should be
        // rotated promptly, without turning a badly skewed clock into a flood.
        MIN_QR_REFRESH_DELAY
    } else {
        DEFAULT_QR_REFRESH_DELAY
    }
}

fn qr_login_url(token: &[u8]) -> String {
    format!("tg://login?token={}", base64_url_no_pad(token))
}

fn base64_url_no_pad(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let (chunks, remainder) = bytes.as_chunks::<3>();
    for chunk in chunks {
        encoded.push(char::from(ALPHABET[usize::from(chunk[0] >> 2)]));
        encoded.push(char::from(
            ALPHABET[usize::from(((chunk[0] & 0x03) << 4) | (chunk[1] >> 4))],
        ));
        encoded.push(char::from(
            ALPHABET[usize::from(((chunk[1] & 0x0f) << 2) | (chunk[2] >> 6))],
        ));
        encoded.push(char::from(ALPHABET[usize::from(chunk[2] & 0x3f)]));
    }
    match remainder {
        [first] => {
            encoded.push(char::from(ALPHABET[usize::from(first >> 2)]));
            encoded.push(char::from(ALPHABET[usize::from((first & 0x03) << 4)]));
        }
        [first, second] => {
            encoded.push(char::from(ALPHABET[usize::from(first >> 2)]));
            encoded.push(char::from(
                ALPHABET[usize::from(((first & 0x03) << 4) | (second >> 4))],
            ));
            encoded.push(char::from(ALPHABET[usize::from((second & 0x0f) << 2)]));
        }
        [] => {}
        _ => unreachable!(),
    }
    encoded
}

async fn sign_in_with_code(
    client: &Client,
    token: LoginToken,
    commands: &mut mpsc::Receiver<TelegramCommand>,
    events: &mpsc::Sender<NetworkEvent>,
) -> Result<CodeOutcome> {
    loop {
        let code = loop {
            match commands.recv().await {
                Some(TelegramCommand::SubmitCode(code)) => break code,
                Some(TelegramCommand::RestartAuth) => return Ok(CodeOutcome::Restart),
                Some(TelegramCommand::Shutdown) | None => bail!("login cancelled"),
                _ => {}
            }
        };
        match client.sign_in(&token, code.trim()).await {
            Ok(_) => return authorized_outcome(client, commands).await,
            Err(SignInError::PasswordRequired(password)) => {
                if restart_auth_requested(commands)? {
                    return Ok(CodeOutcome::Restart);
                }
                return Ok(CodeOutcome::Password(Box::new(password)));
            }
            Err(SignInError::InvalidCode) => {
                if restart_auth_requested(commands)? {
                    return Ok(CodeOutcome::Restart);
                }
                events
                    .send(NetworkEvent::Error(
                        "Incorrect or expired login code".to_owned(),
                    ))
                    .await?;
            }
            Err(SignInError::SignUpRequired) => {
                bail!("this phone number must first register with an official Telegram client")
            }
            Err(error) => {
                if restart_auth_requested(commands)? {
                    return Ok(CodeOutcome::Restart);
                }
                return Err(anyhow!(error).context("Telegram sign-in failed"));
            }
        }
    }
}

fn take_auth_interruption(commands: &mut mpsc::Receiver<TelegramCommand>) -> AuthInterruption {
    loop {
        match commands.try_recv() {
            Ok(TelegramCommand::RestartAuth) => return AuthInterruption::Restart,
            Ok(TelegramCommand::Shutdown) | Err(mpsc::error::TryRecvError::Disconnected) => {
                return AuthInterruption::Shutdown;
            }
            // The application suppresses all commands except cancellation while
            // an authentication RPC is in flight. Discard any programmatic
            // stale input rather than applying it to the next phase.
            Ok(_) => {}
            Err(mpsc::error::TryRecvError::Empty) => return AuthInterruption::None,
        }
    }
}

fn restart_auth_requested(commands: &mut mpsc::Receiver<TelegramCommand>) -> Result<bool> {
    match take_auth_interruption(commands) {
        AuthInterruption::None => Ok(false),
        AuthInterruption::Restart => Ok(true),
        AuthInterruption::Shutdown => bail!("login cancelled"),
    }
}

async fn authorized_outcome(
    client: &Client,
    commands: &mut mpsc::Receiver<TelegramCommand>,
) -> Result<CodeOutcome> {
    match take_auth_interruption(commands) {
        AuthInterruption::None => Ok(CodeOutcome::Authorized),
        AuthInterruption::Restart => cancel_authorized_login(client).await,
        // Do not sign out on process shutdown. The completed session remains
        // valid locally and will be reused on the next launch, but Ready is not
        // emitted after the user has asked Termgram to exit.
        AuthInterruption::Shutdown => bail!("login cancelled"),
    }
}

async fn cancel_authorized_login(client: &Client) -> Result<CodeOutcome> {
    client
        .sign_out()
        .await
        .context("could not cancel the newly authorized Telegram session")?;
    Ok(CodeOutcome::Restart)
}

fn apply_dialogs(
    dialogs: Vec<Dialog>,
    defaults: (i64, i64),
    cache: &mut WorkerCache,
) -> Result<Vec<Chat>> {
    let mut chats = Vec::new();
    let mut visible_chat_ids = HashSet::new();
    let mut dialog_name_ids = HashSet::new();
    let mut hidden_broadcasts = HashSet::new();
    let mut visible_channel_groups = HashSet::new();
    for dialog in dialogs {
        if matches!(&dialog.raw, RawDialog::Folder(_)) {
            continue;
        }
        let id = dialog
            .peer_id()
            .bot_api_dialog_id()
            .context("dialog has no stable identifier")?;
        // Broadcast channels require additional sponsored-message behavior under
        // Telegram's API terms. Termgram's focused messaging scope is people and
        // groups, so channels are deliberately not exposed.
        if matches!(dialog.peer(), Peer::Channel(_)) {
            hidden_broadcasts.insert(id);
            continue;
        }
        visible_chat_ids.insert(id);
        dialog_name_ids.insert(dialog.peer_id());
        if matches!(dialog.peer(), Peer::Group(_)) && dialog.peer_id().kind() == PeerKind::Channel {
            visible_channel_groups.insert(id);
        }
        cache.peers.insert(id, dialog.peer_ref());
        cache
            .names
            .insert(dialog.peer_id(), peer_display_name(dialog.peer()));
        cache_username(cache, dialog.peer_id(), dialog.peer().username());
        let RawDialog::Dialog(raw) = &dialog.raw else {
            unreachable!("folder placeholders are filtered above")
        };
        if let Some(pts) = raw.pts {
            cache.channel_pts.insert(id, pts);
        }
        let (unread, top_message, read_outbox) = (
            u32::try_from(raw.unread_count.max(0)).unwrap_or(u32::MAX),
            raw.top_message,
            raw.read_outbox_max_id,
        );
        cache
            .top_messages
            .entry(id)
            .and_modify(|top| *top = (*top).max(top_message))
            .or_insert(top_message);
        cache
            .read_outbox
            .entry(id)
            .and_modify(|max_id| *max_id = (*max_id).max(read_outbox))
            .or_insert(read_outbox);
        let last_message = dialog.last_message.as_ref();
        chats.push(Chat {
            read_inbox_max_id: Some(raw.read_inbox_max_id.max(0)),
            membership: folders::membership(&dialog, defaults),
            id,
            title: if id == cache.self_id {
                "Saved Messages".to_owned()
            } else {
                peer_display_name(dialog.peer())
            },
            kind: match dialog.peer() {
                Peer::User(_) => ChatKind::Direct,
                Peer::Group(_) => ChatKind::Group,
                Peer::Channel(_) => unreachable!("broadcast channels are filtered above"),
            },
            unread,
            last_message_id: (top_message > 0).then_some(top_message),
            last_message: last_message.map(message_preview).unwrap_or_default(),
            last_activity: last_message.map(TelegramMessage::date),
        });
    }
    reconcile_dialog_snapshot(
        cache,
        &visible_chat_ids,
        dialog_name_ids,
        hidden_broadcasts,
        visible_channel_groups,
    );
    Ok(chats)
}

fn reconcile_dialog_snapshot(
    cache: &mut WorkerCache,
    visible_chat_ids: &HashSet<ChatId>,
    dialog_name_ids: HashSet<PeerId>,
    hidden_broadcasts: HashSet<ChatId>,
    visible_channel_groups: HashSet<ChatId>,
) {
    cache.peers.retain(|chat_id, _| {
        visible_chat_ids.contains(chat_id) || cache.linked_peers.contains(chat_id)
    });
    cache
        .top_messages
        .retain(|chat_id, _| visible_chat_ids.contains(chat_id));
    cache
        .read_outbox
        .retain(|chat_id, _| visible_chat_ids.contains(chat_id));
    cache.hidden_broadcasts = hidden_broadcasts;
    cache.visible_channel_groups.retain(|chat_id| {
        cache.linked_peers.contains(chat_id) || visible_channel_groups.contains(chat_id)
    });
    cache.visible_channel_groups.extend(visible_channel_groups);
    cache.dialog_name_ids = dialog_name_ids;

    cache.transient_name_order.retain(|peer_id| {
        !cache.dialog_name_ids.contains(peer_id) && cache.names.contains_key(peer_id)
    });
    let transient_name_ids: HashSet<_> = cache.transient_name_order.iter().copied().collect();
    cache.names.retain(|peer_id, _| {
        cache.dialog_name_ids.contains(peer_id)
            || transient_name_ids.contains(peer_id)
            || peer_id
                .bot_api_dialog_id()
                .is_some_and(|chat_id| cache.linked_peers.contains(&chat_id))
    });
    cache
        .usernames
        .retain(|peer_id, _| cache.names.contains_key(peer_id));
    trim_transient_sender_names(cache);
}

#[allow(clippy::too_many_lines)]
async fn handle_command(
    command: TelegramCommand,
    client: &Client,
    cache: &mut WorkerCache,
    events: &mpsc::Sender<NetworkEvent>,
    transfers: &mut JoinSet<TransferCompletion>,
    requests: &mut JoinSet<requests::Completion>,
) -> Result<bool> {
    let slots = cache
        .transfer_slots
        .get_or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_TRANSFERS)))
        .clone();
    match command {
        command @ (TelegramCommand::SearchCloud(_)
        | TelegramCommand::SearchMentions { .. }
        | TelegramCommand::LoadCloudContext { .. }
        | TelegramCommand::CancelSearch) => {
            if let Some((request_id, handle)) = cache.cloud_search.take() {
                handle.abort();
                events
                    .send(NetworkEvent::CloudSearchCancelled { request_id })
                    .await?;
            }
            let chat_id = match &command {
                TelegramCommand::SearchCloud(request) => Some(request.chat_id),
                TelegramCommand::LoadCloudContext { chat_id, .. }
                | TelegramCommand::SearchMentions { chat_id, .. } => Some(*chat_id),
                _ => None,
            };
            if let (Some(chat_id), Some(request_id)) = (chat_id, command.cloud_search_id()) {
                events
                    .send(NetworkEvent::CloudSearchLoading {
                        chat_id,
                        request_id,
                    })
                    .await?;
                requests::spawn(command, client, cache, requests);
            }
        }
        command @ (TelegramCommand::LoadChatInfo { .. }
        | TelegramCommand::LoadMembers { .. }
        | TelegramCommand::PreviewInvite { .. }
        | TelegramCommand::JoinInvite { .. }
        | TelegramCommand::LoadReactions { .. }
        | TelegramCommand::ChangeReaction { .. }
        | TelegramCommand::RefreshReactions { .. }
        | TelegramCommand::LoadPoll { .. }
        | TelegramCommand::VotePoll { .. }
        | TelegramCommand::RefreshPoll { .. }
        | TelegramCommand::ResolveAlertSettings { .. }
        | TelegramCommand::ReviewForward { .. }
        | TelegramCommand::ForwardMessage { .. }
        | TelegramCommand::OpenSaved { .. }
        | TelegramCommand::CopyMessage { .. }
        | TelegramCommand::ReviewDeletion { .. }
        | TelegramCommand::DeleteMessage { .. }
        | TelegramCommand::LoadEdit { .. }
        | TelegramCommand::EditMessage { .. }
        | TelegramCommand::LoadOlder { .. }
        | TelegramCommand::ChangeDialogPin { .. }
        | TelegramCommand::SetArchived { .. }
        | TelegramCommand::LoadPinnedMessages { .. }
        | TelegramCommand::LoadPinnedContext { .. }
        | TelegramCommand::ChangeMessagePin { .. }
        | TelegramCommand::LoadHistory { .. }
        | TelegramCommand::LoadMessage { .. }
        | TelegramCommand::LoadReplyPreviews { .. }
        | TelegramCommand::LoadStickers { .. }
        | TelegramCommand::LoadStickerSet { .. }
        | TelegramCommand::SendMessage { .. }
        | TelegramCommand::SendSticker { .. }
        | TelegramCommand::ResolveTelegramLink { .. }
        | TelegramCommand::ActivateButton { .. }
        | TelegramCommand::SetChatUnread { .. }
        | TelegramCommand::MarkRead { .. }
        | TelegramCommand::ReadMentions { .. }
        | TelegramCommand::SetChatMute { .. }) => {
            if let TelegramCommand::LoadPinnedMessages {
                chat_id,
                request_id,
                ..
            }
            | TelegramCommand::LoadPinnedContext {
                chat_id,
                request_id,
                ..
            } = &command
            {
                events
                    .send(NetworkEvent::PinnedMessagesLoading {
                        chat_id: *chat_id,
                        request_id: *request_id,
                    })
                    .await?;
            }
            if let TelegramCommand::LoadHistory {
                chat_id,
                request_id,
                ..
            }
            | TelegramCommand::LoadOlder {
                chat_id,
                request_id,
                ..
            }
            | TelegramCommand::LoadMessage {
                chat_id,
                request_id,
                ..
            } = &command
            {
                events
                    .send(NetworkEvent::HistoryLoading {
                        chat_id: *chat_id,
                        request_id: *request_id,
                    })
                    .await?;
            }
            if let TelegramCommand::LoadReplyPreviews {
                chat_id,
                request_id,
                ..
            } = &command
            {
                events
                    .send(NetworkEvent::ReplyPreviewsLoading {
                        chat_id: *chat_id,
                        request_id: *request_id,
                    })
                    .await?;
            }
            if let TelegramCommand::LoadReactions { request_id, .. } = &command {
                events
                    .send(NetworkEvent::ReactionsLoading {
                        request_id: *request_id,
                    })
                    .await?;
            }
            if let TelegramCommand::LoadPoll { request_id, .. } = &command {
                events
                    .send(NetworkEvent::PollLoading {
                        request_id: *request_id,
                    })
                    .await?;
            }
            requests::spawn(command, client, cache, requests);
        }
        TelegramCommand::SendAttachment {
            chat_id,
            local_id,
            attachment,
            caption,
            reply_to,
        } => {
            let Some(peer) = cache.peers.get(&chat_id).copied() else {
                events
                    .send(NetworkEvent::AttachmentSendFailed {
                        chat_id,
                        local_id,
                        attachment,
                        caption,
                        reply_to,
                        error: "conversation is missing its Telegram peer reference".to_owned(),
                    })
                    .await?;
                return Ok(false);
            };
            if transfers.len() >= MAX_PENDING_TRANSFERS {
                events
                    .send(NetworkEvent::AttachmentSendFailed {
                        chat_id,
                        local_id,
                        attachment,
                        caption,
                        reply_to,
                        error: "Telegram transfer queue is full".to_owned(),
                    })
                    .await?;
                return Ok(false);
            }
            let client = client.clone();
            let cache_owner = cache.cache_owner.clone();
            cache.upload_order.retain(|_, previous| {
                matches!(
                    previous.try_recv(),
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty)
                )
            });
            let (finished, gate) = tokio::sync::oneshot::channel::<()>();
            let after = cache.upload_order.insert(chat_id, gate);
            transfers.spawn(async move {
                // Link tasks in admission order, before they are scheduled.
                // Dropping the sender also releases the next item on cancellation.
                let _finished = finished;
                if let Some(previous) = after {
                    let _ = previous.await;
                }
                let permit = Arc::new(
                    slots
                        .acquire_owned()
                        .await
                        .expect("transfer queue is never closed"),
                );
                let _cache_owner = cache_owner.clone();
                let result = async {
                    let original = attachment.path.clone();
                    let digest = attachment.digest;
                    // Blocking validation retains its permit if this async task
                    // is cancelled; cancellation cannot create unbounded copies.
                    let copy_permit = permit.clone();
                    let upload = tokio::task::spawn_blocking(move || {
                        let _permit = copy_permit;
                        crate::staging::upload_copy(&original, digest)
                    })
                    .await
                    .context("attachment validation worker failed")??;
                    upload_attachment(
                        &client,
                        peer,
                        &upload.path,
                        &caption,
                        attachment.as_photo,
                        reply_to,
                    )
                    .await
                }
                .await
                .map_err(|error| chat_info::send_error(&error));
                TransferCompletion::Send {
                    chat_id,
                    local_id,
                    attachment,
                    caption,
                    reply_to,
                    result: Box::new(result),
                }
            });
        }
        TelegramCommand::DownloadPreview {
            chat_id,
            message_id,
            request_id,
            thumbnail,
        } => {
            let preparation = async {
                let peer = cache
                    .peers
                    .get(&chat_id)
                    .copied()
                    .context("conversation is missing its Telegram peer reference")?;
                if transfers.len() >= MAX_PENDING_TRANSFERS {
                    bail!("Telegram transfer queue is full; close and retry");
                }
                let directory = ensure_download_dir(cache).await?;
                Ok::<_, anyhow::Error>((peer, directory))
            }
            .await;
            match preparation {
                Ok((peer, directory)) => {
                    let client = client.clone();
                    let cache_owner = cache.cache_owner.clone();
                    transfers.spawn(async move {
                        let _permit = slots
                            .acquire_owned()
                            .await
                            .expect("transfer queue is never closed");
                        let _cache_owner = cache_owner.clone();
                        let result = if thumbnail {
                            download_preview_thumbnail(
                                &client,
                                peer,
                                chat_id,
                                message_id,
                                directory,
                                cache_owner,
                            )
                            .await
                        } else {
                            download_attachment(
                                &client,
                                peer,
                                chat_id,
                                message_id,
                                directory,
                                None,
                                cache_owner,
                            )
                            .await
                        }
                        .map_err(|error| format!("{error:#}"));
                        TransferCompletion::Preview {
                            chat_id,
                            message_id,
                            request_id,
                            result,
                        }
                    });
                }
                Err(error) => {
                    events
                        .send(NetworkEvent::PreviewDownloadFailed {
                            chat_id,
                            message_id,
                            request_id,
                            error: format!("{error:#}"),
                        })
                        .await?;
                }
            }
        }
        TelegramCommand::DownloadStickerThumb {
            request_id,
            sticker,
        } => {
            let preparation = async {
                if transfers.len() >= MAX_PENDING_TRANSFERS {
                    bail!("Telegram transfer queue is full; close and retry");
                }
                let directory = ensure_download_dir(cache).await?;
                Ok::<_, anyhow::Error>(directory)
            }
            .await;
            match preparation {
                Ok(directory) => {
                    let client = client.clone();
                    let cache_owner = cache.cache_owner.clone();
                    let document_id = sticker.id;
                    transfers.spawn(async move {
                        let _permit = slots
                            .acquire_owned()
                            .await
                            .expect("transfer queue is never closed");
                        let _cache_owner = cache_owner.clone();
                        let result =
                            stickers::download_thumb(&client, &sticker, directory, cache_owner)
                                .await
                                .map_err(|error| format!("{error:#}"));
                        TransferCompletion::StickerThumb {
                            request_id,
                            document_id,
                            result,
                        }
                    });
                }
                Err(error) => {
                    events
                        .send(NetworkEvent::StickerThumbDownloaded {
                            request_id,
                            document_id: sticker.id,
                            result: Err(format!("{error:#}")),
                        })
                        .await?;
                }
            }
        }
        TelegramCommand::DownloadAttachment {
            chat_id,
            message_id,
            request_id,
            media_id,
        } => {
            let Some(peer) = cache.peers.get(&chat_id).copied() else {
                events
                    .send(NetworkEvent::AttachmentDownloadFailed {
                        request_id,
                        chat_id,
                        message_id,
                        error: "conversation is missing its Telegram peer reference".to_owned(),
                    })
                    .await?;
                return Ok(false);
            };
            if transfers.len() >= MAX_PENDING_TRANSFERS {
                events
                    .send(NetworkEvent::AttachmentDownloadFailed {
                        request_id,
                        chat_id,
                        message_id,
                        error: "Telegram transfer queue is full".to_owned(),
                    })
                    .await?;
                return Ok(false);
            }
            let directory = ensure_download_dir(cache).await?;
            let client = client.clone();
            let cache_owner = cache.cache_owner.clone();
            transfers.spawn(async move {
                let _permit = slots
                    .acquire_owned()
                    .await
                    .expect("transfer queue is never closed");
                let _cache_owner = cache_owner.clone();
                let result = download_attachment(
                    &client,
                    peer,
                    chat_id,
                    message_id,
                    directory,
                    media_id,
                    cache_owner,
                )
                .await
                .map_err(|error| format!("{error:#}"));
                TransferCompletion::Download {
                    request_id,
                    chat_id,
                    message_id,
                    result,
                }
            });
        }
        TelegramCommand::PrepareAttachments(_)
        | TelegramCommand::CopyText(_)
        | TelegramCommand::SearchCached(_)
        | TelegramCommand::LoadCachedContext { .. } => {
            unreachable!("local searches never reach Telegram")
        }
        TelegramCommand::RefreshFolders => {
            cache.folders.dirty = true;
        }
        TelegramCommand::RefreshDialogs => {
            cache.dialog_pins.dirty = true;
            cache.message_pins.dirty = true;
            requests::refresh_dialogs(client, cache, events, requests).await?;
        }
        TelegramCommand::RefreshDialogPins => cache.dialog_pins.dirty = true,
        TelegramCommand::Shutdown => return Ok(true),
        TelegramCommand::StartQrAuth
        | TelegramCommand::SubmitPhone(_)
        | TelegramCommand::SubmitCode(_)
        | TelegramCommand::SubmitPassword(_)
        | TelegramCommand::RestartAuth
        | TelegramCommand::SwitchAccount { .. } => {}
    }
    Ok(false)
}

async fn activate_inline_button(
    client: &Client,
    peer: PeerRef,
    message_id: i32,
    button_index: u16,
) -> Result<(Option<String>, Option<String>)> {
    if message_id <= 0 {
        bail!("invalid Telegram message identifier")
    }
    let mut messages = client
        .get_messages_by_id(peer, &[message_id])
        .await
        .context("could not refresh button markup")?;
    let message = messages
        .pop()
        .flatten()
        .context("button message is unavailable")?;
    let Some(tl::enums::ReplyMarkup::ReplyInlineMarkup(markup)) = message.reply_markup() else {
        bail!("message no longer has inline buttons")
    };
    let button = markup
        .rows
        .into_iter()
        .flat_map(|row| match row {
            tl::enums::KeyboardButtonRow::Row(row) => row.buttons,
        })
        .nth(usize::from(button_index))
        .context("button no longer exists")?;
    match button {
        tl::enums::KeyboardButton::Url(button) => Ok((None, Some(button.url))),
        tl::enums::KeyboardButton::WebView(button) => Ok((None, Some(button.url))),
        tl::enums::KeyboardButton::SimpleWebView(button) => Ok((None, Some(button.url))),
        tl::enums::KeyboardButton::Callback(button) if button.requires_password => {
            bail!("this callback requires password confirmation in a graphical client")
        }
        tl::enums::KeyboardButton::Callback(button) => {
            bot_callback(client, peer, message_id, Some(button.data), false).await
        }
        tl::enums::KeyboardButton::Game(_) => {
            bot_callback(client, peer, message_id, None, true).await
        }
        _ => bail!("this button type is not supported in the terminal"),
    }
}

async fn bot_callback(
    client: &Client,
    peer: PeerRef,
    message_id: i32,
    data: Option<Vec<u8>>,
    game: bool,
) -> Result<(Option<String>, Option<String>)> {
    let answer = client
        .invoke(&tl::functions::messages::GetBotCallbackAnswer {
            game,
            peer: peer.into(),
            msg_id: message_id,
            data,
            password: None,
        })
        .await
        .context("Telegram rejected the bot callback")?;
    let tl::enums::messages::BotCallbackAnswer::Answer(answer) = answer;
    Ok((answer.message, answer.url))
}

async fn upload_attachment(
    client: &Client,
    peer: PeerRef,
    path: &Path,
    caption: &str,
    as_photo: bool,
    reply_to: Option<i32>,
) -> Result<TelegramMessage> {
    let uploaded = client
        .upload_file(path)
        .await
        .with_context(|| format!("could not upload {}", path.display()))?;
    let input = if as_photo {
        InputMessage::new().text(caption).photo(uploaded)
    } else {
        InputMessage::new().text(caption).document(uploaded)
    }
    .reply_to(reply_to);
    Box::pin(client.send_message(peer, input))
        .await
        .map_err(anyhow::Error::from)
}

#[allow(clippy::too_many_lines)]
async fn process_transfer_completion(
    completion: TransferCompletion,
    cache: &mut WorkerCache,
    events: &mpsc::Sender<NetworkEvent>,
) -> Result<()> {
    match completion {
        TransferCompletion::Preview {
            chat_id,
            message_id,
            request_id,
            result,
        } => {
            events
                .send(match result {
                    Ok(path) => NetworkEvent::PreviewDownloaded {
                        chat_id,
                        message_id,
                        request_id,
                        path,
                    },
                    Err(error) => NetworkEvent::PreviewDownloadFailed {
                        chat_id,
                        message_id,
                        request_id,
                        error,
                    },
                })
                .await?;
        }
        TransferCompletion::StickerThumb {
            request_id,
            document_id,
            result,
        } => {
            events
                .send(NetworkEvent::StickerThumbDownloaded {
                    request_id,
                    document_id,
                    result,
                })
                .await?;
        }
        TransferCompletion::Send {
            chat_id,
            local_id,
            attachment,
            caption,
            reply_to,
            result,
        } => match *result {
            Ok(message) if message.id() <= 0 => {
                attachment.discard_owned();
                events
                    .send(NetworkEvent::MessageAccepted { chat_id, local_id })
                    .await?;
            }
            Ok(message) => {
                attachment.discard_owned();
                events
                    .send(NetworkEvent::MessageSent {
                        local_id,
                        message: map_message(&message, cache)?,
                    })
                    .await?;
            }
            Err(error) => {
                events
                    .send(NetworkEvent::AttachmentSendFailed {
                        chat_id,
                        local_id,
                        attachment,
                        caption,
                        reply_to,
                        error: format!("Could not send attachment: {error}"),
                    })
                    .await?;
            }
        },
        TransferCompletion::Download {
            request_id,
            chat_id,
            message_id,
            result,
        } => match result {
            Ok(path) => {
                events
                    .send(NetworkEvent::AttachmentDownloaded {
                        request_id,
                        chat_id,
                        message_id,
                        path,
                    })
                    .await?;
            }
            Err(error) => {
                events
                    .send(NetworkEvent::AttachmentDownloadFailed {
                        request_id,
                        chat_id,
                        message_id,
                        error: format!("Could not download attachment: {error}"),
                    })
                    .await?;
            }
        },
    }
    Ok(())
}

async fn download_attachment(
    client: &Client,
    peer: PeerRef,
    chat_id: ChatId,
    message_id: i32,
    directory: PathBuf,
    expected_media_id: Option<i64>,
    cache_owner: Option<Arc<std::fs::File>>,
) -> Result<PathBuf> {
    let mut messages = client
        .get_messages_by_id(peer, &[message_id])
        .await
        .context("could not refresh the Telegram message")?;
    let message = messages
        .pop()
        .flatten()
        .context("the message is no longer available")?;
    let media = message
        .media()
        .context("the message does not contain downloadable media")?;
    if !matches!(
        media,
        Media::Photo(_) | Media::Document(_) | Media::Sticker(_)
    ) {
        bail!("this type of Telegram media is not downloadable")
    }

    if expected_media_id.is_some()
        && attachment_from_media(&media).and_then(|attachment| attachment.source_id)
            != expected_media_id
    {
        bail!("Attachment changed; refresh the message before opening it");
    }
    let suggested_name = attachment_from_media(&media).map_or_else(
        || "attachment".to_owned(),
        |attachment| attachment.display_name().to_owned(),
    );
    let partial = media_cache::temporary(&directory)?;
    client
        .download_media(&media, partial.as_ref() as &Path)
        .await
        .context("Telegram media transfer failed")?;
    media_cache::finish(
        partial,
        directory,
        chat_id,
        message_id,
        &suggested_name,
        cache_owner,
    )
    .await
}

/// Use the SDK's Downloadable thumbnail implementation, including cached and
/// stripped JPEG reconstruction; Telegram vector paths are not raster images.
async fn download_preview_thumbnail(
    client: &Client,
    peer: PeerRef,
    chat_id: ChatId,
    message_id: i32,
    directory: PathBuf,
    cache_owner: Option<Arc<std::fs::File>>,
) -> Result<PathBuf> {
    let message = client
        .get_messages_by_id(peer, &[message_id])
        .await?
        .pop()
        .flatten()
        .context("the message is no longer available")?;
    let media = message.media().context("the message no longer has media")?;
    let thumbs = match media {
        Media::Sticker(sticker) => sticker.document.thumbs(),
        Media::Document(document) => document.thumbs(),
        Media::Photo(photo) => photo.thumbs(),
        _ => bail!("this attachment has no static preview"),
    };
    let thumbnail = thumbs
        .into_iter()
        .filter(|thumb| !matches!(thumb, PhotoSize::Empty(_) | PhotoSize::Path(_)))
        .filter(|thumb| thumb.size() > 0)
        .max_by_key(PhotoSize::size)
        .context("Telegram did not provide a static thumbnail for this sticker")?;
    let partial = media_cache::temporary(&directory)?;
    client
        .download_media(&thumbnail, partial.as_ref() as &Path)
        .await
        .context("Telegram preview transfer failed")?;
    media_cache::finish(
        partial,
        directory,
        chat_id,
        message_id,
        "preview.jpg",
        cache_owner,
    )
    .await
}

async fn ensure_download_dir(cache: &mut WorkerCache) -> Result<PathBuf> {
    if let Some(path) = &cache.download_dir {
        return Ok(path.clone());
    }
    let root = media_cache::prepare(
        cache.media_root.clone(),
        cache.self_id,
        cache.cache_owner.clone(),
    )
    .await?;
    cache.download_dir = Some(root.clone());
    Ok(root)
}

async fn unique_download_path(
    directory: &Path,
    chat_id: ChatId,
    message_id: i32,
    suggested_name: &str,
) -> Result<PathBuf> {
    let safe_name = sanitize_download_name(suggested_name);
    let safe_name = if safe_name.is_empty() {
        "attachment".to_owned()
    } else {
        safe_name
    };
    let base = format!("{chat_id}_{message_id}_{safe_name}");
    let initial = directory.join(&base);
    if !tokio::fs::try_exists(&initial).await? {
        return Ok(initial);
    }

    let path = Path::new(&safe_name);
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("attachment");
    let extension = path.extension().and_then(|extension| extension.to_str());
    for suffix in 2..=10_000_u32 {
        let file_name = match extension {
            Some(extension) if !extension.is_empty() => {
                format!("{chat_id}_{message_id}_{stem}-{suffix}.{extension}")
            }
            _ => format!("{chat_id}_{message_id}_{stem}-{suffix}"),
        };
        let candidate = directory.join(file_name);
        if !tokio::fs::try_exists(&candidate).await? {
            return Ok(candidate);
        }
    }
    bail!("could not choose a unique temporary file name")
}

fn sanitize_download_name(value: &str) -> String {
    const MAX_NAME_BYTES: usize = 120;
    let mut safe = String::with_capacity(value.len().min(MAX_NAME_BYTES));
    for character in value.chars() {
        if safe.len().saturating_add(character.len_utf8()) > MAX_NAME_BYTES {
            break;
        }
        match character {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => safe.push('_'),
            character if character.is_control() || is_invisible_format(character) => {}
            character => safe.push(character),
        }
    }
    let mut safe = safe
        .trim()
        .trim_matches(|character| character == '.' || character == ' ')
        .to_owned();
    if safe.is_empty() {
        return safe;
    }
    let base = Path::new(&safe)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(&safe);
    if is_windows_reserved_base(base) {
        safe.insert(0, '_');
    }
    safe
}

fn is_invisible_format(character: char) -> bool {
    matches!(
        character,
        '\u{00ad}'
            | '\u{034f}'
            | '\u{061c}'
            | '\u{115f}'
            | '\u{1160}'
            | '\u{17b4}'
            | '\u{17b5}'
            | '\u{180b}'..='\u{180f}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{3164}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{feff}'
            | '\u{ffa0}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0000}'..='\u{e0fff}'
    )
}

fn is_windows_reserved_base(base: &str) -> bool {
    let base = base.trim_end_matches(['.', ' ']).to_ascii_uppercase();
    matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || base
            .strip_prefix("COM")
            .or_else(|| base.strip_prefix("LPT"))
            .is_some_and(|number| {
                matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
}

async fn resolve_telegram_link(
    client: &Client,
    private_link: Option<(PeerRef, String)>,
    url: &str,
) -> Result<(Chat, PeerRef, Option<TelegramMessage>)> {
    let target = parse_telegram_link(url)?;
    let (chat, peer, message_id) = match target {
        TelegramLink::Public {
            username,
            message_id,
        } => {
            let resolved = client
                .resolve_username(&username)
                .await
                .with_context(|| format!("could not resolve @{username}"))?
                .with_context(|| format!("@{username} does not exist"))?;
            let id = resolved
                .id()
                .bot_api_dialog_id()
                .context("resolved chat has no stable identifier")?;
            if matches!(&resolved, Peer::Channel(_)) {
                bail!("broadcast channels are outside Termgram's messaging scope")
            }
            let peer = resolved
                .to_ref()
                .await
                .map_err(anyhow::Error::from_boxed)?
                .context("Telegram did not provide an addressable peer reference")?;
            let chat = Chat {
                read_inbox_max_id: None,
                membership: crate::folders::ChatMembership::default(),
                id,
                title: peer_display_name(&resolved),
                kind: match &resolved {
                    Peer::User(_) => ChatKind::Direct,
                    Peer::Group(_) => ChatKind::Group,
                    Peer::Channel(_) => unreachable!("broadcast channels are rejected above"),
                },
                unread: 0,
                last_message_id: None,
                last_message: String::new(),
                last_activity: None,
            };
            (chat, peer, message_id)
        }
        TelegramLink::Private {
            chat_id,
            message_id,
        } => {
            let (peer, title) = private_link.context(
                "private message links can only open groups already present in your conversations",
            )?;
            let chat = Chat {
                read_inbox_max_id: None,
                membership: crate::folders::ChatMembership::default(),
                id: chat_id,
                title,
                kind: ChatKind::Group,
                unread: 0,
                last_message_id: None,
                last_message: String::new(),
                last_activity: None,
            };
            (chat, peer, Some(message_id))
        }
    };

    let message = if let Some(message_id) = message_id {
        let mut messages = client
            .get_messages_by_id(peer, &[message_id])
            .await
            .context("could not retrieve linked message")?;
        let telegram_message = messages
            .pop()
            .flatten()
            .context("linked message is unavailable")?;
        Some(telegram_message)
    } else {
        None
    };
    Ok((chat, peer, message))
}

fn parse_telegram_link(value: &str) -> Result<TelegramLink> {
    let value = value.trim();
    if let Some(query) = value.strip_prefix("tg://resolve?") {
        let username = query_value(query, "domain").context("link has no Telegram username")?;
        validate_username(username)?;
        let message_id = query_value(query, "post")
            .map(parse_message_id)
            .transpose()?;
        return Ok(TelegramLink::Public {
            username: username.to_owned(),
            message_id,
        });
    }
    if let Some(query) = value.strip_prefix("tg://privatepost?") {
        let channel =
            query_value(query, "channel").context("private link has no channel identifier")?;
        let post = query_value(query, "post").context("private link has no post identifier")?;
        return Ok(TelegramLink::Private {
            chat_id: private_chat_id(channel)?,
            message_id: parse_message_id(post)?,
        });
    }

    let without_scheme = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
        .unwrap_or(value);
    let without_scheme = without_scheme
        .strip_prefix("www.")
        .unwrap_or(without_scheme);
    let (host, path) = without_scheme
        .split_once('/')
        .context("not a Telegram chat link")?;
    if !matches!(host.to_ascii_lowercase().as_str(), "t.me" | "telegram.me") {
        bail!("not a t.me or telegram.me link")
    }
    let path = path
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .trim_matches('/');
    let mut parts: Vec<_> = path.split('/').filter(|part| !part.is_empty()).collect();
    if parts
        .first()
        .is_some_and(|part| part.eq_ignore_ascii_case("s"))
    {
        parts.remove(0);
    }
    if parts
        .first()
        .is_some_and(|part| part.eq_ignore_ascii_case("c"))
    {
        if parts.len() < 3 {
            bail!("private Telegram message link is incomplete")
        }
        return Ok(TelegramLink::Private {
            chat_id: private_chat_id(parts[1])?,
            message_id: parse_message_id(parts.last().copied().unwrap_or_default())?,
        });
    }
    let username = parts
        .first()
        .copied()
        .context("link has no Telegram username")?;
    if username.starts_with('+') || username.eq_ignore_ascii_case("joinchat") {
        bail!("invite links cannot be opened without joining a conversation")
    }
    validate_username(username)?;
    let message_id = if parts.len() > 1 {
        Some(parse_message_id(parts.last().copied().unwrap_or_default())?)
    } else {
        None
    };
    Ok(TelegramLink::Public {
        username: username.to_owned(),
        message_id,
    })
}

fn query_value<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name && !value.is_empty()).then_some(value)
    })
}

fn validate_username(username: &str) -> Result<()> {
    if username.len() > 64
        || username.is_empty()
        || !username
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        bail!("invalid Telegram username")
    }
    Ok(())
}

fn parse_message_id(value: &str) -> Result<i32> {
    let id: i32 = value
        .parse()
        .context("invalid Telegram message identifier")?;
    if id <= 0 {
        bail!("invalid Telegram message identifier")
    }
    Ok(id)
}

fn private_chat_id(value: &str) -> Result<ChatId> {
    let channel: i64 = value
        .parse()
        .context("invalid private Telegram channel identifier")?;
    if channel <= 0 {
        bail!("invalid private Telegram channel identifier")
    }
    1_000_000_000_000_i64
        .checked_add(channel)
        .and_then(i64::checked_neg)
        .context("private Telegram channel identifier is too large")
}

fn map_message(message: &TelegramMessage, cache: &mut WorkerCache) -> Result<Message> {
    let chat_id = peer_id(message)?;
    let media = message.media();
    let (text, entities) = entities::map(
        message.text(),
        message.fmt_entities().map_or(&[], Vec::as_slice),
    );
    let (name, sender_username) = resolve_sender(message, cache);
    let sender = if message.outgoing() {
        "You".to_owned()
    } else {
        name
    };
    let reply_sender = username_or_sender(sender_username.as_deref(), &sender);
    let reply_to = reply_info(message, chat_id, cache);
    cache_message_sender(cache, chat_id, message.id(), reply_sender);
    Ok(Message {
        reactions: reactions::map(message),
        poll: polls::map(message),
        entities,
        notification: Some(crate::notifications::Metadata {
            sender: message
                .sender_id()
                .and_then(PeerId::bot_api_dialog_id)
                .filter(|id| *id > 0),
            silent: message.silent(),
            protected: matches!(&message.raw, tl::enums::Message::Message(raw) if raw.noforwards),
        }),
        mention: message.mentioned().then(|| crate::model::Mention {
            unread: message.media_unread() && !message.outgoing(),
            requires_playback: notifications::requires_playback(message),
        }),
        edited_at: message.edit_date(),
        pinned: message.pinned(),
        id: message.id(),
        chat_id,
        sender,
        sender_username,
        reply_to,
        // Telegram stores the Unicode fallback for custom-emoji entities in
        // the raw message string. Keep that text instead of trying to render
        // the custom document in a terminal.
        text,
        timestamp: message.date(),
        outgoing: message.outgoing(),
        delivery: if message.outgoing()
            && cache
                .read_outbox
                .get(&chat_id)
                .is_some_and(|max_id| message.id() <= *max_id)
        {
            Delivery::Read
        } else if message.outgoing() {
            Delivery::Sent
        } else {
            Delivery::Read
        },
        attachment: media.as_ref().and_then(attachment_from_media),
        links: message_links(message),
        buttons: message_buttons(message),
    })
}

fn reply_info(
    message: &TelegramMessage,
    current_chat_id: ChatId,
    cache: &WorkerCache,
) -> Option<ReplyInfo> {
    let grammers_client::tl::enums::Message::Message(raw) = &message.raw else {
        return None;
    };
    let grammers_client::tl::enums::MessageReplyHeader::Header(header) = raw.reply_to.as_ref()?
    else {
        return None;
    };
    let message_id = header.reply_to_msg_id?;
    let chat_id = header
        .reply_to_peer_id
        .as_ref()
        .and_then(|peer| PeerId::from(peer).bot_api_dialog_id())
        .unwrap_or(current_chat_id);
    Some(ReplyInfo {
        message_id,
        chat_id,
        sender: cache.message_senders.get(&(chat_id, message_id)).cloned(),
    })
}

fn hydrate_reply_senders(messages: &mut [Message], cache: &WorkerCache) {
    for message in messages {
        hydrate_reply_sender(message, cache);
    }
}

fn hydrate_reply_sender(message: &mut Message, cache: &WorkerCache) {
    let Some(reply) = &mut message.reply_to else {
        return;
    };
    if reply.sender.is_none() {
        reply.sender = cache
            .message_senders
            .get(&(reply.chat_id, reply.message_id))
            .cloned();
    }
}

fn cache_message_sender(cache: &mut WorkerCache, chat_id: ChatId, message_id: i32, sender: String) {
    let key = (chat_id, message_id);
    if !cache.message_senders.contains_key(&key) {
        cache.message_sender_order.push_back(key);
    }
    cache.message_senders.insert(key, sender);
    while cache.message_sender_order.len() > MESSAGE_SENDER_CACHE_LIMIT {
        let Some(stale) = cache.message_sender_order.pop_front() else {
            break;
        };
        cache.message_senders.remove(&stale);
    }
}

fn resolve_sender(message: &TelegramMessage, cache: &mut WorkerCache) -> (String, Option<String>) {
    let Some(sender_id) = message.sender_id() else {
        return ("Unknown".to_owned(), None);
    };
    if let Some(peer) = message.sender() {
        let name = peer_display_name(peer);
        cache_sender_name(cache, sender_id, name.clone());
        cache_username(cache, sender_id, peer.username());
        return (name, cache.usernames.get(&sender_id).cloned());
    }
    if let Some(name) = cache.names.get(&sender_id) {
        return (name.clone(), cache.usernames.get(&sender_id).cloned());
    }
    // Short updates may omit the sender object. Never make a network round
    // trip on the update path just to obtain a display name.
    ("Unknown".to_owned(), None)
}

fn username_or_sender(username: Option<&str>, sender: &str) -> String {
    let username = sanitize_terminal_line(username.unwrap_or_default());
    if username.is_empty() {
        sender.to_owned()
    } else {
        format!("@{username}")
    }
}

fn cache_username(cache: &mut WorkerCache, peer_id: PeerId, username: Option<&str>) {
    if let Some(username) = username
        .map(sanitize_terminal_line)
        .filter(|name| !name.is_empty())
    {
        cache.usernames.insert(peer_id, username);
    } else {
        cache.usernames.remove(&peer_id);
    }
}

fn cache_sender_name(cache: &mut WorkerCache, peer_id: PeerId, name: String) {
    if !cache.dialog_name_ids.contains(&peer_id) && !cache.names.contains_key(&peer_id) {
        cache.transient_name_order.push_back(peer_id);
    }
    cache.names.insert(peer_id, name);
    trim_transient_sender_names(cache);
}

fn trim_transient_sender_names(cache: &mut WorkerCache) {
    while cache.transient_name_order.len() > TRANSIENT_SENDER_NAME_LIMIT {
        let Some(peer_id) = cache.transient_name_order.pop_front() else {
            break;
        };
        if !cache.dialog_name_ids.contains(&peer_id) {
            cache.names.remove(&peer_id);
            cache.usernames.remove(&peer_id);
        }
    }
}

fn begin_unresolved_refresh(cache: &mut WorkerCache) -> bool {
    let now = Instant::now();
    if cache
        .last_unresolved_refresh
        .is_some_and(|last| now.saturating_duration_since(last) < UNRESOLVED_REFRESH_COOLDOWN)
    {
        false
    } else {
        cache.last_unresolved_refresh = Some(now);
        true
    }
}

async fn is_hidden_broadcast(
    session: &SqliteSession,
    message: &TelegramMessage,
    cache: &mut WorkerCache,
) -> Result<Option<bool>> {
    let chat_id = peer_id(message)?;
    if cache.hidden_broadcasts.contains(&chat_id) {
        return Ok(Some(true));
    }
    if cache.visible_channel_groups.contains(&chat_id) {
        return Ok(Some(false));
    }
    if let Some(peer) = message.peer() {
        return match peer {
            Peer::Channel(_) => {
                cache.hidden_broadcasts.insert(chat_id);
                cache.visible_channel_groups.remove(&chat_id);
                Ok(Some(true))
            }
            Peer::Group(_) => {
                if message.peer_id().kind() == PeerKind::Channel {
                    cache.visible_channel_groups.insert(chat_id);
                }
                Ok(Some(false))
            }
            Peer::User(_) => Ok(Some(false)),
        };
    }
    if message.peer_id().kind() != PeerKind::Channel {
        return Ok(Some(false));
    }

    let info = session.peer(message.peer_id()).await?;
    match info {
        Some(PeerInfo::Channel {
            kind: Some(ChannelKind::Broadcast),
            ..
        }) => {
            cache.hidden_broadcasts.insert(chat_id);
            Ok(Some(true))
        }
        Some(PeerInfo::Channel { kind: Some(_), .. }) => {
            cache.visible_channel_groups.insert(chat_id);
            Ok(Some(false))
        }
        _ => Ok(None),
    }
}

fn peer_id(message: &TelegramMessage) -> Result<ChatId> {
    message
        .peer_id()
        .bot_api_dialog_id()
        .context("message has no stable peer identifier")
}

fn message_preview(message: &TelegramMessage) -> String {
    if let Some(poll) = polls::map(message) {
        return format!(
            "[{}] {}",
            if poll.definition.quiz { "quiz" } else { "poll" },
            poll.definition.question.preview()
        );
    }
    let media = message.media();
    let (text, entities) = entities::map(
        message.text(),
        message.fmt_entities().map_or(&[], Vec::as_slice),
    );
    message_preview_with_media(&crate::entities::conceal(&text, &entities), media.as_ref())
}

fn message_links(message: &TelegramMessage) -> Vec<MessageLink> {
    const MAX_LINKS: usize = 32;
    let raw_text = message.text();
    let Some(entities) = message.fmt_entities() else {
        return Vec::new();
    };
    let mut links = Vec::new();
    for entity in entities.iter().take(MAX_LINKS.saturating_mul(2)) {
        let label = utf16_entity_text(raw_text, entity.offset(), entity.length())
            .map(sanitize_terminal_line)
            .unwrap_or_default();
        let target = match entity {
            tl::enums::MessageEntity::Url(_) => normalize_message_url(&label),
            tl::enums::MessageEntity::TextUrl(entity) => normalize_message_url(&entity.url),
            _ => None,
        };
        let Some(url) = target else {
            continue;
        };
        let label = if label.is_empty() { url.clone() } else { label };
        if !links
            .iter()
            .any(|existing: &MessageLink| existing.url == url && existing.label == label)
        {
            links.push(MessageLink { label, url });
        }
        if links.len() == MAX_LINKS {
            break;
        }
    }
    links
}

fn message_buttons(message: &TelegramMessage) -> Vec<MessageButton> {
    const MAX_BUTTONS: usize = 64;
    let Some(tl::enums::ReplyMarkup::ReplyInlineMarkup(markup)) = message.reply_markup() else {
        return Vec::new();
    };
    markup
        .rows
        .into_iter()
        .flat_map(|row| match row {
            tl::enums::KeyboardButtonRow::Row(row) => row.buttons,
        })
        .take(MAX_BUTTONS)
        .enumerate()
        .filter_map(|(index, button)| {
            let kind = match &button {
                tl::enums::KeyboardButton::Url(_)
                | tl::enums::KeyboardButton::WebView(_)
                | tl::enums::KeyboardButton::SimpleWebView(_) => MessageButtonKind::Url,
                tl::enums::KeyboardButton::Callback(button) if !button.requires_password => {
                    MessageButtonKind::Callback
                }
                tl::enums::KeyboardButton::Game(_) => MessageButtonKind::Game,
                _ => MessageButtonKind::Unsupported,
            };
            let label = sanitize_terminal_line(&button.text());
            let index = u16::try_from(index).ok()?;
            Some(MessageButton { label, index, kind })
        })
        .collect()
}

fn normalize_message_url(value: &str) -> Option<String> {
    const MAX_URL_BYTES: usize = 8 * 1024;
    let value = value.trim();
    if value.is_empty()
        || value.len() > MAX_URL_BYTES
        || value.chars().any(char::is_control)
        || value.chars().any(char::is_whitespace)
    {
        return None;
    }
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("tg://") {
        Some(value.to_owned())
    } else if lower.starts_with("www.") || value.contains('.') {
        Some(format!("https://{value}"))
    } else {
        None
    }
}

fn utf16_entity_text(text: &str, offset: i32, length: i32) -> Option<&str> {
    let start_units = usize::try_from(offset).ok()?;
    let length_units = usize::try_from(length).ok()?;
    let end_units = start_units.checked_add(length_units)?;
    let mut units = 0;
    let mut start = None;
    let mut end = None;
    for (byte, character) in text.char_indices() {
        if units == start_units {
            start = Some(byte);
        }
        if units == end_units {
            end = Some(byte);
            break;
        }
        units += character.len_utf16();
        if units > start_units && start.is_none() || units > end_units {
            return None;
        }
    }
    if units == start_units && start.is_none() {
        start = Some(text.len());
    }
    if units == end_units && end.is_none() {
        end = Some(text.len());
    }
    text.get(start?..end?)
}

fn message_preview_with_media(text: &str, media: Option<&Media>) -> String {
    let text = sanitize_terminal_text(text);
    let label = match media {
        None | Some(Media::WebPage(_)) => "",
        Some(Media::Photo(_)) => "[photo]",
        Some(Media::Document(document)) => match document.mime_type() {
            Some(kind) if kind.starts_with("video/") => "[video]",
            Some(kind) if kind.starts_with("audio/") => "[audio]",
            _ => "[file]",
        },
        Some(Media::Sticker(sticker)) => {
            let emoji = sanitize_terminal_line(sticker.emoji());
            return match (emoji.is_empty(), text.is_empty()) {
                (true, true) => "[sticker]".to_owned(),
                (true, false) => format!("[sticker] {text}"),
                (false, true) => format!("[sticker] {emoji}"),
                (false, false) => format!("[sticker] {emoji} {text}"),
            };
        }
        Some(Media::Contact(_)) => "[contact]",
        Some(Media::Poll(_)) => "[poll]",
        Some(Media::Geo(_) | Media::GeoLive(_) | Media::Venue(_)) => "[location]",
        Some(Media::Dice(_)) => "[dice]",
        Some(_) => "[media]",
    };
    match (label.is_empty(), text.is_empty()) {
        (true, _) => text,
        (false, true) => label.to_owned(),
        (false, false) => format!("{label} {text}"),
    }
}

fn attachment_from_media(media: &Media) -> Option<Attachment> {
    match media {
        Media::Photo(photo) => Some(Attachment {
            source_id: Some(photo.id()),
            kind: AttachmentKind::Photo,
            file_name: Some("photo.jpg".to_owned()),
            mime_type: Some("image/jpeg".to_owned()),
            size: photo.size().and_then(|size| u64::try_from(size).ok()),
            fallback_emoji: None,
        }),
        Media::Document(document) => {
            let mime_type = document.mime_type().map(ToOwned::to_owned);
            let kind = match mime_type.as_deref() {
                Some(mime) if mime.starts_with("video/") => AttachmentKind::Video,
                Some(mime) if mime.starts_with("audio/") => AttachmentKind::Audio,
                _ => AttachmentKind::File,
            };
            Some(Attachment {
                source_id: Some(document.id()),
                kind,
                file_name: safe_media_name(document.name()),
                mime_type,
                size: document.size().and_then(|size| u64::try_from(size).ok()),
                fallback_emoji: None,
            })
        }
        Media::Sticker(sticker) => Some(Attachment {
            source_id: Some(sticker.document.id()),
            kind: AttachmentKind::Sticker,
            file_name: safe_media_name(sticker.document.name())
                .or_else(|| Some(default_sticker_name(sticker.document.mime_type()).to_owned())),
            mime_type: sticker.document.mime_type().map(ToOwned::to_owned),
            size: sticker
                .document
                .size()
                .and_then(|size| u64::try_from(size).ok()),
            fallback_emoji: {
                let emoji = sanitize_terminal_line(sticker.emoji());
                (!emoji.is_empty()).then_some(emoji)
            },
        }),
        _ => None,
    }
}

fn safe_media_name(value: Option<&str>) -> Option<String> {
    value
        .map(sanitize_download_name)
        .filter(|name| !name.is_empty())
}

fn default_sticker_name(mime_type: Option<&str>) -> &'static str {
    match mime_type {
        Some("application/x-tgsticker") => "sticker.tgs",
        Some("video/webm") => "sticker.webm",
        _ => "sticker.webp",
    }
}

fn safe_name(value: Option<&str>, fallback: &str) -> String {
    let value = sanitize_terminal_line(value.unwrap_or(fallback));
    if value.is_empty() {
        fallback.to_owned()
    } else {
        value
    }
}

fn peer_display_name(peer: &Peer) -> String {
    match peer {
        Peer::User(user) => safe_name(
            Some(&user.full_name()),
            if user.deleted() {
                "Deleted account"
            } else {
                "Unknown"
            },
        ),
        _ => safe_name(peer.name(), "Unknown"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use grammers_client::tl;
    use grammers_session::types::PeerId;
    use grammers_session::updates::UpdatesLike;
    use tokio::sync::mpsc;

    use crate::event::TelegramCommand;
    use crate::model::{Delivery, Message, ReplyInfo};

    use super::{
        AuthInterruption, DEFAULT_QR_REFRESH_DELAY, MESSAGE_SENDER_CACHE_LIMIT,
        MIN_QR_REFRESH_DELAY, TRANSIENT_SENDER_NAME_LIMIT, TelegramLink,
        UNRESOLVED_REFRESH_COOLDOWN, WorkerCache, advance_dialog_watermark, base64_url_no_pad,
        begin_unresolved_refresh, cache_message_sender, cache_sender_name, cache_username,
        contains_login_token_update, hydrate_reply_sender, normalize_message_url,
        parse_telegram_link, qr_login_url, qr_refresh_delay, reconcile_dialog_snapshot,
        sanitize_download_name, take_auth_interruption, username_or_sender, utf16_entity_text,
    };

    #[test]
    fn qr_login_url_uses_unpadded_url_safe_base64() {
        assert_eq!(base64_url_no_pad(b""), "");
        assert_eq!(base64_url_no_pad(b"f"), "Zg");
        assert_eq!(base64_url_no_pad(b"fo"), "Zm8");
        assert_eq!(base64_url_no_pad(b"foo"), "Zm9v");
        assert_eq!(base64_url_no_pad(&[0xfb, 0xff, 0xff]), "-___");
        assert_eq!(qr_login_url(&[0xfb, 0xff, 0xff]), "tg://login?token=-___");
    }

    #[test]
    fn qr_expiry_handles_recent_expiry_and_implausible_clock_skew() {
        assert_eq!(qr_refresh_delay(0), DEFAULT_QR_REFRESH_DELAY);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let just_expired = i32::try_from(now.saturating_sub(1)).unwrap_or_default();
        assert_eq!(qr_refresh_delay(just_expired), MIN_QR_REFRESH_DELAY);
        let far_future = i32::try_from(now.saturating_add(600)).unwrap_or(i32::MAX);
        assert_eq!(qr_refresh_delay(far_future), DEFAULT_QR_REFRESH_DELAY);
    }

    #[test]
    fn message_entity_slices_use_telegram_utf16_offsets() {
        assert_eq!(utf16_entity_text("a🙂b", 1, 2), Some("🙂"));
        assert_eq!(utf16_entity_text("a🙂b", 2, 1), None);
        assert_eq!(
            normalize_message_url("example.com/a"),
            Some("https://example.com/a".to_owned())
        );
        assert_eq!(normalize_message_url("javascript:alert(1)"), None);
    }

    #[test]
    fn recognizes_login_token_updates_in_short_and_batched_envelopes() {
        let short = UpdatesLike::Updates(tl::enums::Updates::UpdateShort(tl::types::UpdateShort {
            update: tl::enums::Update::LoginToken,
            date: 0,
        }));
        assert!(contains_login_token_update(&short));

        let batch = UpdatesLike::Updates(tl::enums::Updates::Updates(tl::types::Updates {
            updates: vec![tl::enums::Update::LoginToken],
            users: Vec::new(),
            chats: Vec::new(),
            date: 0,
            seq: 0,
        }));
        assert!(contains_login_token_update(&batch));
        assert!(!contains_login_token_update(&UpdatesLike::Updates(
            tl::enums::Updates::TooLong,
        )));
    }

    #[test]
    fn cancellation_wins_over_stale_auth_input_before_a_new_prompt() {
        let (commands, mut receiver) = mpsc::channel(4);
        commands.try_send(TelegramCommand::StartQrAuth).unwrap();
        commands.try_send(TelegramCommand::RestartAuth).unwrap();

        assert!(matches!(
            take_auth_interruption(&mut receiver),
            AuthInterruption::Restart
        ));
    }

    #[test]
    fn shutdown_is_distinct_from_restart_at_authorization_boundary() {
        let (commands, mut receiver) = mpsc::channel(2);
        commands.try_send(TelegramCommand::Shutdown).unwrap();

        assert!(matches!(
            take_auth_interruption(&mut receiver),
            AuthInterruption::Shutdown
        ));
    }

    #[test]
    fn dialog_watermark_distinguishes_replay_live_and_duplicate_updates() {
        let mut cache = WorkerCache::default();
        cache.top_messages.insert(7, 100);

        assert!(!advance_dialog_watermark(&mut cache, 7, 99));
        assert!(!advance_dialog_watermark(&mut cache, 7, 100));
        assert!(advance_dialog_watermark(&mut cache, 7, 101));
        assert!(!advance_dialog_watermark(&mut cache, 7, 101));
        assert_eq!(cache.top_messages.get(&7), Some(&101));

        assert!(advance_dialog_watermark(&mut cache, 8, 42));
        assert!(!advance_dialog_watermark(&mut cache, 8, 42));
    }

    #[test]
    fn unresolved_dialog_refresh_is_globally_throttled_and_can_retry() {
        let mut cache = WorkerCache::default();

        assert!(begin_unresolved_refresh(&mut cache));
        assert!(!begin_unresolved_refresh(&mut cache));

        cache.last_unresolved_refresh =
            Instant::now().checked_sub(UNRESOLVED_REFRESH_COOLDOWN + Duration::from_millis(1));
        assert!(begin_unresolved_refresh(&mut cache));
    }

    #[test]
    fn parses_public_and_private_telegram_message_links() {
        assert_eq!(
            parse_telegram_link("https://t.me/example_user/42?single").unwrap(),
            TelegramLink::Public {
                username: "example_user".to_owned(),
                message_id: Some(42),
            }
        );
        assert_eq!(
            parse_telegram_link("tg://resolve?domain=example_user&post=7").unwrap(),
            TelegramLink::Public {
                username: "example_user".to_owned(),
                message_id: Some(7),
            }
        );
        assert_eq!(
            parse_telegram_link("https://t.me/c/123456/9").unwrap(),
            TelegramLink::Private {
                chat_id: -1_000_000_123_456,
                message_id: 9,
            }
        );
    }

    #[test]
    fn rejects_non_telegram_and_invite_links() {
        assert!(parse_telegram_link("https://example.com/person/42").is_err());
        assert!(parse_telegram_link("https://t.me/+invitehash").is_err());
        assert!(parse_telegram_link("https://t.me/user/not-a-message").is_err());
    }

    #[test]
    fn download_name_cannot_escape_temp_directory() {
        assert_eq!(
            sanitize_download_name("../../secret\\payload:\u{1b}.txt"),
            "_.._secret_payload_.txt"
        );
        assert_eq!(sanitize_download_name(".."), "");
    }

    #[test]
    fn download_name_is_safe_on_windows_and_hides_no_extension() {
        assert_eq!(
            sanitize_download_name("bad:*?\"<>|name.txt"),
            "bad_______name.txt"
        );
        assert_eq!(sanitize_download_name("CON"), "_CON");
        assert_eq!(sanitize_download_name("con.txt"), "_con.txt");
        assert_eq!(sanitize_download_name("Lpt9.log"), "_Lpt9.log");
        assert_eq!(sanitize_download_name("COM0.log"), "COM0.log");
        assert_eq!(
            sanitize_download_name("invoice\u{202e}gpj.exe"),
            "invoicegpj.exe"
        );
    }

    #[test]
    fn download_name_limit_never_splits_utf8() {
        let safe = sanitize_download_name(&"界".repeat(100));
        assert!(safe.len() <= 120);
        assert!(safe.is_char_boundary(safe.len()));
        assert_eq!(safe.chars().count(), 40);
    }

    #[test]
    fn transient_sender_names_are_bounded_without_evicting_dialog_names() {
        let mut cache = WorkerCache::default();
        let dialog_peer = PeerId::user_unchecked(1);
        cache.dialog_name_ids.insert(dialog_peer);
        cache.names.insert(dialog_peer, "Current dialog".to_owned());

        let last_sender = i64::try_from(TRANSIENT_SENDER_NAME_LIMIT + 11)
            .expect("test cache limit fits in a Telegram user identifier");
        for sender in 2..=last_sender {
            let sender_id = PeerId::user_unchecked(sender);
            cache_sender_name(&mut cache, sender_id, format!("Sender {sender}"));
            cache_username(&mut cache, sender_id, Some(&format!("user_{sender}")));
        }

        assert_eq!(
            cache.transient_name_order.len(),
            TRANSIENT_SENDER_NAME_LIMIT
        );
        assert_eq!(cache.names.len(), TRANSIENT_SENDER_NAME_LIMIT + 1);
        assert_eq!(cache.usernames.len(), TRANSIENT_SENDER_NAME_LIMIT);
        assert!(!cache.usernames.contains_key(&PeerId::user_unchecked(2)));
        cache_username(&mut cache, PeerId::user_unchecked(last_sender), None);
        assert!(
            !cache
                .usernames
                .contains_key(&PeerId::user_unchecked(last_sender))
        );
        assert_eq!(
            cache.names.get(&dialog_peer).map(String::as_str),
            Some("Current dialog")
        );
        assert!(!cache.names.contains_key(&PeerId::user_unchecked(2)));
        assert!(
            cache
                .names
                .contains_key(&PeerId::user_unchecked(last_sender))
        );
    }

    #[test]
    fn reply_sender_is_hydrated_from_bounded_message_index() {
        let mut cache = WorkerCache::default();
        cache_message_sender(&mut cache, 7, 41, "Alice".to_owned());
        let mut message = Message {
            reactions: None,
            poll: None,
            entities: Vec::new(),
            notification: None,
            mention: None,
            edited_at: None,
            pinned: false,
            id: 42,
            chat_id: 7,
            sender_username: None,
            sender: "Bob".to_owned(),
            reply_to: Some(ReplyInfo {
                message_id: 41,
                chat_id: 7,
                sender: None,
            }),
            text: "hello".to_owned(),
            timestamp: Message::timestamp_from_unix(0),
            outgoing: false,
            delivery: Delivery::Read,
            attachment: None,
            links: Vec::new(),
            buttons: Vec::new(),
        };

        hydrate_reply_sender(&mut message, &cache);

        assert_eq!(
            message.reply_to.and_then(|reply| reply.sender),
            Some("Alice".to_owned())
        );
    }

    #[test]
    fn reply_labels_prefer_a_sanitized_username_and_fall_back_to_display_name() {
        assert_eq!(
            username_or_sender(Some("alice_name\u{1b}[31m"), "Alice Example"),
            "@alice_name"
        );
        assert_eq!(username_or_sender(None, "Alice Example"), "Alice Example");
        assert_eq!(username_or_sender(Some("\u{7}"), "Alice"), "Alice");
    }

    #[test]
    fn message_sender_index_stays_bounded_and_updates_in_place() {
        let mut cache = WorkerCache::default();
        cache_message_sender(&mut cache, 1, 1, "Old".to_owned());
        cache_message_sender(&mut cache, 1, 1, "New".to_owned());
        for id in 2..=i32::try_from(MESSAGE_SENDER_CACHE_LIMIT + 1).unwrap() {
            cache_message_sender(&mut cache, 1, id, format!("Sender {id}"));
        }

        assert_eq!(cache.message_senders.len(), MESSAGE_SENDER_CACHE_LIMIT);
        assert_eq!(cache.message_sender_order.len(), MESSAGE_SENDER_CACHE_LIMIT);
        assert!(!cache.message_senders.contains_key(&(1, 1)));
        assert!(
            cache
                .message_senders
                .contains_key(&(1, i32::try_from(MESSAGE_SENDER_CACHE_LIMIT + 1).unwrap()))
        );
    }

    #[test]
    fn reply_sender_hydration_is_scoped_to_the_conversation() {
        let mut cache = WorkerCache::default();
        cache_message_sender(&mut cache, 8, 41, "Other chat".to_owned());
        let mut message = Message {
            reactions: None,
            poll: None,
            entities: Vec::new(),
            notification: None,
            mention: None,
            edited_at: None,
            pinned: false,
            id: 42,
            chat_id: 7,
            sender_username: None,
            sender: "Bob".to_owned(),
            reply_to: Some(ReplyInfo {
                message_id: 41,
                chat_id: 7,
                sender: None,
            }),
            text: "hello".to_owned(),
            timestamp: Message::timestamp_from_unix(0),
            outgoing: false,
            delivery: Delivery::Read,
            attachment: None,
            links: Vec::new(),
            buttons: Vec::new(),
        };

        hydrate_reply_sender(&mut message, &cache);

        assert_eq!(message.reply_to.and_then(|reply| reply.sender), None);
    }

    #[test]
    fn complete_dialog_snapshot_prunes_stale_state_and_keeps_transient_names() {
        let mut cache = WorkerCache::default();
        let current_peer = PeerId::user_unchecked(7);
        let stale_peer = PeerId::user_unchecked(8);
        let sender_peer = PeerId::user_unchecked(9);
        let current_chat = current_peer.bot_api_dialog_id_unchecked();
        let stale_chat = stale_peer.bot_api_dialog_id_unchecked();

        cache
            .peers
            .insert(current_chat, current_peer.to_ambient_ref());
        cache.peers.insert(stale_chat, stale_peer.to_ambient_ref());
        cache.top_messages.insert(current_chat, 11);
        cache.top_messages.insert(stale_chat, 12);
        cache.read_outbox.insert(current_chat, 9);
        cache.read_outbox.insert(stale_chat, 10);
        cache.dialog_name_ids.insert(stale_peer);
        cache.names.insert(current_peer, "Current".to_owned());
        cache.names.insert(stale_peer, "Stale".to_owned());
        cache_sender_name(&mut cache, sender_peer, "Recent sender".to_owned());
        cache.hidden_broadcasts.insert(-100);
        cache.visible_channel_groups.insert(-101);

        reconcile_dialog_snapshot(
            &mut cache,
            &HashSet::from([current_chat]),
            HashSet::from([current_peer]),
            HashSet::from([-200]),
            HashSet::from([-201]),
        );

        assert!(cache.peers.contains_key(&current_chat));
        assert!(!cache.peers.contains_key(&stale_chat));
        assert!(cache.top_messages.contains_key(&current_chat));
        assert!(!cache.top_messages.contains_key(&stale_chat));
        assert!(cache.read_outbox.contains_key(&current_chat));
        assert!(!cache.read_outbox.contains_key(&stale_chat));
        assert!(cache.names.contains_key(&current_peer));
        assert!(!cache.names.contains_key(&stale_peer));
        assert!(cache.names.contains_key(&sender_peer));
        assert_eq!(cache.hidden_broadcasts, HashSet::from([-200]));
        assert_eq!(cache.visible_channel_groups, HashSet::from([-201]));
    }

    #[test]
    fn dialog_snapshot_preserves_explicitly_linked_group_peer_and_name() {
        let mut cache = WorkerCache::default();
        let linked_peer = PeerId::channel_unchecked(44);
        let linked_chat = linked_peer.bot_api_dialog_id_unchecked();
        cache
            .peers
            .insert(linked_chat, linked_peer.to_ambient_ref());
        cache.linked_peers.insert(linked_chat);
        cache.visible_channel_groups.insert(linked_chat);
        cache.names.insert(linked_peer, "Linked group".to_owned());

        reconcile_dialog_snapshot(
            &mut cache,
            &HashSet::new(),
            HashSet::new(),
            HashSet::new(),
            HashSet::new(),
        );

        assert!(cache.peers.contains_key(&linked_chat));
        assert!(cache.visible_channel_groups.contains(&linked_chat));
        assert_eq!(
            cache.names.get(&linked_peer).map(String::as_str),
            Some("Linked group")
        );
    }
}
