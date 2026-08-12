use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use grammers_client::client::{LoginToken, PasswordToken, UpdatesConfiguration};
use grammers_client::media::Media;
use grammers_client::message::{InputMessage, Message as TelegramMessage};
use grammers_client::peer::Peer;
use grammers_client::tl::enums::Dialog as RawDialog;
use grammers_client::update::Update;
use grammers_client::{Client, InvocationError, SenderPool, SignInError};
use grammers_session::storages::SqliteSession;
use grammers_session::types::{PeerId, PeerKind, PeerRef};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::config::Config;
use crate::event::{AuthPrompt, ConnectionStatus, NetworkEvent, TelegramCommand};
use crate::model::{
    sanitize_terminal_line, sanitize_terminal_text, Attachment, AttachmentKind, Chat, ChatId,
    ChatKind, Delivery, Message, ReplyInfo,
};

const HISTORY_LIMIT: usize = 80;
const COMMAND_QUEUE_CAPACITY: usize = 32;
const EVENT_QUEUE_CAPACITY: usize = 64;
const UPDATE_QUEUE_LIMIT: usize = 256;
const TRANSIENT_SENDER_NAME_LIMIT: usize = 256;
const MESSAGE_SENDER_CACHE_LIMIT: usize = 512;
const UNRESOLVED_REFRESH_COOLDOWN: Duration = Duration::from_secs(5);
const MAX_CONCURRENT_TRANSFERS: usize = 3;

pub struct TelegramHandle {
    pub commands: mpsc::Sender<TelegramCommand>,
    pub events: mpsc::Receiver<NetworkEvent>,
    pub task: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
struct WorkerCache {
    peers: HashMap<ChatId, PeerRef>,
    /// Peers opened explicitly through supported Telegram links remain usable
    /// even when they are not part of the account's dialog snapshot.
    linked_peers: HashSet<ChatId>,
    /// Highest message identifier already represented by each dialog snapshot.
    top_messages: HashMap<ChatId, i32>,
    read_outbox: HashMap<ChatId, i32>,
    names: HashMap<PeerId, String>,
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
    /// Created lazily so text-only sessions never touch the temporary folder.
    download_dir: Option<PathBuf>,
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

enum TransferCompletion {
    Send {
        chat_id: ChatId,
        local_id: i32,
        path: PathBuf,
        caption: String,
        as_photo: bool,
        result: Box<Result<TelegramMessage, String>>,
    },
    Download {
        chat_id: ChatId,
        message_id: i32,
        result: Result<PathBuf, String>,
    },
}

struct PartialDownload {
    path: PathBuf,
    complete: bool,
}

impl PartialDownload {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            complete: false,
        }
    }

    fn finish(mut self) -> PathBuf {
        self.complete = true;
        self.path.clone()
    }
}

impl Drop for PartialDownload {
    fn drop(&mut self) {
        if !self.complete {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[must_use]
pub fn spawn(config: Config) -> TelegramHandle {
    let (command_tx, command_rx) = mpsc::channel(COMMAND_QUEUE_CAPACITY);
    let (event_tx, event_rx) = mpsc::channel(EVENT_QUEUE_CAPACITY);
    let task = tokio::spawn(async move {
        if let Err(error) = Box::pin(run(config, command_rx, event_tx.clone())).await {
            let _ = event_tx
                .send(NetworkEvent::Fatal(format!("{error:#}")))
                .await;
        }
    });
    TelegramHandle {
        commands: command_tx,
        events: event_rx,
        task,
    }
}

#[allow(clippy::too_many_lines)]
async fn run(
    config: Config,
    mut commands: mpsc::Receiver<TelegramCommand>,
    events: mpsc::Sender<NetworkEvent>,
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
    let SenderPool {
        runner,
        handle,
        updates,
    } = SenderPool::new(session, config.api_id);
    let client = Client::new(handle.clone());
    let pool_task = tokio::spawn(runner.run());

    let result: Result<()> = Box::pin(async {
        if !client.is_authorized().await? {
            authenticate(&client, &config.api_hash, &mut commands, &events).await?;
        }

        let me = client.get_me().await?;
        let user_name = safe_name(me.first_name(), "You");
        events.send(NetworkEvent::Ready { user_name }).await.ok();

        let mut cache = WorkerCache::default();
        load_dialogs(&client, &mut cache, &events).await?;
        events
            .send(NetworkEvent::Status(ConnectionStatus::Online))
            .await
            .ok();

        let mut updates = client
            .stream_updates(
                updates,
                UpdatesConfiguration {
                    catch_up: true,
                    update_queue_limit: Some(UPDATE_QUEUE_LIMIT),
                },
            )
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        let mut recovering = false;
        let mut transfers = JoinSet::new();

        loop {
            tokio::select! {
                command = commands.recv() => {
                    let Some(command) = command else { break; };
                    match Box::pin(handle_command(
                        command,
                        &client,
                        &mut cache,
                        &events,
                        &mut transfers,
                    )).await {
                        Ok(true) => break,
                        Ok(false) => {}
                        Err(error) => {
                            let message = format!("Telegram request failed: {error:#}");
                            events
                                .send(NetworkEvent::Error(message))
                                .await
                                .ok();
                        }
                    }
                }
                update = updates.next() => {
                    if matches!(update, Err(InvocationError::Dropped)) {
                        bail!("Telegram update stream closed");
                    }
                    let update_failed = update.is_err();
                    if let Err(error) = Box::pin(process_update(
                        update,
                        &client,
                        &mut cache,
                        &events,
                        &mut recovering,
                    )).await {
                        events
                            .send(NetworkEvent::Error(format!(
                                "Could not process a Telegram update: {error:#}"
                            )))
                            .await
                            .ok();
                    }
                    if update_failed {
                        // Avoid a tight retry loop if difference recovery is temporarily failing.
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                }
                transfer = transfers.join_next(), if !transfers.is_empty() => {
                    match transfer {
                        Some(Ok(completion)) => {
                            Box::pin(process_transfer_completion(
                                completion,
                                &client,
                                &mut cache,
                                &events,
                            )).await?;
                        }
                        Some(Err(error)) if !error.is_cancelled() => {
                            events
                                .send(NetworkEvent::Error(format!(
                                    "Telegram transfer task failed: {error}"
                                )))
                                .await
                                .ok();
                        }
                        Some(Err(_)) | None => {}
                    }
                }
            }
        }

        transfers.abort_all();
        while transfers.join_next().await.is_some() {}

        updates
            .sync_update_state()
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        Ok(())
    })
    .await;
    handle.quit();
    let _ = pool_task.await;
    result
}

#[allow(clippy::too_many_lines)]
async fn process_update(
    update: Result<Update, InvocationError>,
    client: &Client,
    cache: &mut WorkerCache,
    events: &mpsc::Sender<NetworkEvent>,
    recovering: &mut bool,
) -> Result<()> {
    match update {
        Ok(Update::NewMessage(update)) => {
            restore_online_status(events, recovering).await;
            let message = update.into_inner();
            let Some(hidden) = is_hidden_broadcast(client, &message, cache).await? else {
                if begin_unresolved_refresh(cache) {
                    load_dialogs(client, cache, events).await?;
                }
                return Ok(());
            };
            if hidden {
                return Ok(());
            }
            let chat_id = peer_id(&message)?;
            if let Some(peer) = message
                .peer_ref()
                .await
                .map_err(|error| anyhow!(error.to_string()))?
            {
                cache.peers.insert(chat_id, peer);
            }
            let message_id = message.id();
            let after_dialog_snapshot = advance_dialog_watermark(cache, chat_id, message_id);
            let message = Box::pin(map_message(client, &message, cache)).await?;
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
            let Some(hidden) = is_hidden_broadcast(client, &message, cache).await? else {
                if begin_unresolved_refresh(cache) {
                    load_dialogs(client, cache, events).await?;
                }
                return Ok(());
            };
            if hidden {
                return Ok(());
            }
            events
                .send(NetworkEvent::MessageUpdated(
                    Box::pin(map_message(client, &message, cache)).await?,
                ))
                .await
                .ok();
        }
        Ok(Update::Raw(update)) => {
            restore_online_status(events, recovering).await;
            match &update.raw {
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
                        "Telegram update stream: {error}"
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

async fn authenticate(
    client: &Client,
    api_hash: &str,
    commands: &mut mpsc::Receiver<TelegramCommand>,
    events: &mpsc::Sender<NetworkEvent>,
) -> Result<()> {
    'login: loop {
        let (phone, token) = request_login_token(client, api_hash, commands, events).await?;
        events
            .send(NetworkEvent::Auth(AuthPrompt::Code {
                phone: phone.trim().to_owned(),
            }))
            .await?;

        let mut password_token = match sign_in_with_code(client, token, commands, events).await? {
            CodeOutcome::Authorized => return Ok(()),
            CodeOutcome::Restart => continue,
            CodeOutcome::Password(token) => *token,
        };
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
                Ok(_) => return Ok(()),
                Err(SignInError::InvalidPassword(token)) => {
                    password_token = token;
                    events
                        .send(NetworkEvent::Error("Incorrect 2FA password".to_owned()))
                        .await?;
                }
                Err(error) => return Err(anyhow!(error).context("Telegram 2FA failed")),
            }
        }
    }
}

async fn request_login_token(
    client: &Client,
    api_hash: &str,
    commands: &mut mpsc::Receiver<TelegramCommand>,
    events: &mpsc::Sender<NetworkEvent>,
) -> Result<(String, LoginToken)> {
    events.send(NetworkEvent::Auth(AuthPrompt::Phone)).await?;
    loop {
        let phone = loop {
            match commands.recv().await {
                Some(TelegramCommand::SubmitPhone(phone)) if !phone.trim().is_empty() => {
                    break phone;
                }
                Some(TelegramCommand::Shutdown) | None => bail!("login cancelled"),
                _ => {}
            }
        };
        match Box::pin(client.request_login_code(phone.trim(), api_hash)).await {
            Ok(token) => return Ok((phone, token)),
            Err(error) => {
                events
                    .send(NetworkEvent::Error(format!(
                        "Could not request a login code: {error}"
                    )))
                    .await?;
            }
        }
    }
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
            Ok(_) => return Ok(CodeOutcome::Authorized),
            Err(SignInError::PasswordRequired(password)) => {
                return Ok(CodeOutcome::Password(Box::new(password)));
            }
            Err(SignInError::InvalidCode) => {
                events
                    .send(NetworkEvent::Error(
                        "Incorrect or expired login code".to_owned(),
                    ))
                    .await?;
            }
            Err(SignInError::SignUpRequired) => {
                bail!("this phone number must first register with an official Telegram client")
            }
            Err(error) => return Err(anyhow!(error).context("Telegram sign-in failed")),
        }
    }
}

async fn load_dialogs(
    client: &Client,
    cache: &mut WorkerCache,
    events: &mpsc::Sender<NetworkEvent>,
) -> Result<()> {
    let mut iter = client.iter_dialogs();
    let mut chats = Vec::new();
    let mut visible_chat_ids = HashSet::new();
    let mut dialog_name_ids = HashSet::new();
    let mut hidden_broadcasts = HashSet::new();
    let mut visible_channel_groups = HashSet::new();
    while let Some(dialog) = iter.next().await? {
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
            .insert(dialog.peer_id(), safe_name(dialog.peer().name(), "Unknown"));
        let RawDialog::Dialog(raw) = &dialog.raw else {
            unreachable!("folder placeholders are filtered above")
        };
        let (unread, unread_mark, top_message, read_outbox) = (
            u32::try_from(raw.unread_count.max(0)).unwrap_or(u32::MAX),
            raw.unread_mark,
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
            id,
            title: safe_name(dialog.peer().name(), "Unknown"),
            kind: match dialog.peer() {
                Peer::User(_) => ChatKind::Direct,
                Peer::Group(_) => ChatKind::Group,
                Peer::Channel(_) => unreachable!("broadcast channels are filtered above"),
            },
            unread: unread.max(u32::from(unread_mark)),
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
    events.send(NetworkEvent::Dialogs(chats)).await?;
    Ok(())
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
    trim_transient_sender_names(cache);
}

#[allow(clippy::too_many_lines)]
async fn handle_command(
    command: TelegramCommand,
    client: &Client,
    cache: &mut WorkerCache,
    events: &mpsc::Sender<NetworkEvent>,
    transfers: &mut JoinSet<TransferCompletion>,
) -> Result<bool> {
    match command {
        TelegramCommand::LoadHistory {
            chat_id,
            request_id,
        } => {
            let result: Result<Vec<Message>> = async {
                let peer = *cache
                    .peers
                    .get(&chat_id)
                    .context("selected chat is missing its Telegram peer reference")?;
                let mut iter = client.iter_messages(peer).limit(HISTORY_LIMIT);
                let mut messages = Vec::new();
                while let Some(message) = iter.next().await? {
                    messages.push(Box::pin(map_message(client, &message, cache)).await?);
                }
                messages.reverse();
                hydrate_reply_senders(&mut messages, cache);
                Ok(messages)
            }
            .await;
            match result {
                Ok(messages) => {
                    events
                        .send(NetworkEvent::History {
                            chat_id,
                            request_id,
                            messages,
                        })
                        .await?;
                }
                Err(error) => {
                    events
                        .send(NetworkEvent::HistoryFailed {
                            chat_id,
                            request_id,
                            error: format!("Could not load history: {error:#}"),
                        })
                        .await?;
                }
            }
        }
        TelegramCommand::LoadMessage {
            chat_id,
            source_message_id,
            message_id,
            request_id,
        } => {
            let result: Result<Message> = async {
                if source_message_id <= 0 || message_id <= 0 {
                    bail!("invalid Telegram message identifier")
                }
                let peer = *cache
                    .peers
                    .get(&chat_id)
                    .context("selected chat is missing its Telegram peer reference")?;
                let mut messages = client
                    .get_messages_by_id(peer, &[source_message_id])
                    .await
                    .context("could not retrieve the replying message")?;
                let source = messages
                    .pop()
                    .flatten()
                    .context("replying message is unavailable")?;
                if source.reply_to_message_id() != Some(message_id) {
                    bail!("reply relation changed before navigation")
                }
                let telegram_message = client
                    .get_reply_to_message(&source)
                    .await
                    .context("could not retrieve reply target")?
                    .context("reply target is unavailable")?;
                let mut message = Box::pin(map_message(client, &telegram_message, cache)).await?;
                hydrate_reply_sender(&mut message, cache);
                Ok(message)
            }
            .await;
            match result {
                Ok(message) => {
                    events
                        .send(NetworkEvent::MessageLoaded {
                            chat_id,
                            message_id,
                            request_id,
                            message,
                        })
                        .await?;
                }
                Err(error) => {
                    events
                        .send(NetworkEvent::MessageLoadFailed {
                            chat_id,
                            message_id,
                            request_id,
                            error: format!("Could not load message: {error:#}"),
                        })
                        .await?;
                }
            }
        }
        TelegramCommand::SendMessage {
            chat_id,
            local_id,
            text,
        } => {
            let Some(peer) = cache.peers.get(&chat_id).copied() else {
                events
                    .send(NetworkEvent::SendFailed {
                        chat_id,
                        local_id,
                        text,
                        error: "conversation is missing its Telegram peer reference".to_owned(),
                    })
                    .await?;
                return Ok(false);
            };
            match Box::pin(client.send_message(peer, text.clone())).await {
                Ok(message) => {
                    if message.id() <= 0 {
                        events
                            .send(NetworkEvent::MessageAccepted { chat_id, local_id })
                            .await?;
                    } else {
                        events
                            .send(NetworkEvent::MessageSent {
                                local_id,
                                message: Box::pin(map_message(client, &message, cache)).await?,
                            })
                            .await?;
                    }
                }
                Err(error) => {
                    events
                        .send(NetworkEvent::SendFailed {
                            chat_id,
                            local_id,
                            text,
                            error: error.to_string(),
                        })
                        .await?;
                }
            }
        }
        TelegramCommand::SendAttachment {
            chat_id,
            local_id,
            path,
            caption,
            as_photo,
        } => {
            let Some(peer) = cache.peers.get(&chat_id).copied() else {
                events
                    .send(NetworkEvent::AttachmentSendFailed {
                        chat_id,
                        local_id,
                        path,
                        caption,
                        as_photo,
                        error: "conversation is missing its Telegram peer reference".to_owned(),
                    })
                    .await?;
                return Ok(false);
            };
            if transfers.len() >= MAX_CONCURRENT_TRANSFERS {
                events
                    .send(NetworkEvent::AttachmentSendFailed {
                        chat_id,
                        local_id,
                        path,
                        caption,
                        as_photo,
                        error: "too many Telegram transfers are already running".to_owned(),
                    })
                    .await?;
                return Ok(false);
            }
            let client = client.clone();
            transfers.spawn(async move {
                let result = upload_attachment(&client, peer, &path, &caption, as_photo)
                    .await
                    .map_err(|error| format!("{error:#}"));
                TransferCompletion::Send {
                    chat_id,
                    local_id,
                    path,
                    caption,
                    as_photo,
                    result: Box::new(result),
                }
            });
        }
        TelegramCommand::DownloadAttachment {
            chat_id,
            message_id,
        } => {
            let Some(peer) = cache.peers.get(&chat_id).copied() else {
                events
                    .send(NetworkEvent::AttachmentDownloadFailed {
                        chat_id,
                        message_id,
                        error: "conversation is missing its Telegram peer reference".to_owned(),
                    })
                    .await?;
                return Ok(false);
            };
            if transfers.len() >= MAX_CONCURRENT_TRANSFERS {
                events
                    .send(NetworkEvent::AttachmentDownloadFailed {
                        chat_id,
                        message_id,
                        error: "too many Telegram transfers are already running".to_owned(),
                    })
                    .await?;
                return Ok(false);
            }
            let directory = ensure_download_dir(cache).await?;
            let client = client.clone();
            transfers.spawn(async move {
                let result = download_attachment(&client, peer, chat_id, message_id, directory)
                    .await
                    .map_err(|error| format!("{error:#}"));
                TransferCompletion::Download {
                    chat_id,
                    message_id,
                    result,
                }
            });
        }
        TelegramCommand::ResolveTelegramLink { url } => {
            match Box::pin(resolve_telegram_link(client, cache, &url)).await {
                Ok((chat, message)) => {
                    events
                        .send(NetworkEvent::LinkResolved { chat, message })
                        .await?;
                }
                Err(error) => {
                    events
                        .send(NetworkEvent::LinkFailed {
                            url,
                            error: format!("Could not open Telegram link: {error:#}"),
                        })
                        .await?;
                }
            }
        }
        TelegramCommand::MarkRead { chat_id } => {
            let result = match cache.peers.get(&chat_id).copied() {
                Some(peer) => client.mark_as_read(peer).await,
                None => Err(InvocationError::Dropped),
            };
            match result {
                Ok(()) => events.send(NetworkEvent::ReadMarked { chat_id }).await?,
                Err(error) => {
                    events
                        .send(NetworkEvent::ReadMarkFailed {
                            chat_id,
                            error: format!("Could not mark conversation read: {error}"),
                        })
                        .await?;
                }
            }
        }
        TelegramCommand::RefreshDialogs => {
            if let Err(error) = load_dialogs(client, cache, events).await {
                events
                    .send(NetworkEvent::DialogsFailed(format!(
                        "Could not refresh conversations: {error:#}"
                    )))
                    .await?;
            }
        }
        TelegramCommand::Shutdown => return Ok(true),
        TelegramCommand::SubmitPhone(_)
        | TelegramCommand::SubmitCode(_)
        | TelegramCommand::SubmitPassword(_)
        | TelegramCommand::RestartAuth => {}
    }
    Ok(false)
}

async fn upload_attachment(
    client: &Client,
    peer: PeerRef,
    path: &Path,
    caption: &str,
    as_photo: bool,
) -> Result<TelegramMessage> {
    let uploaded = client
        .upload_file(path)
        .await
        .with_context(|| format!("could not upload {}", path.display()))?;
    let input = if as_photo {
        InputMessage::new().text(caption).photo(uploaded)
    } else {
        InputMessage::new().text(caption).document(uploaded)
    };
    Box::pin(client.send_message(peer, input))
        .await
        .map_err(anyhow::Error::from)
}

async fn process_transfer_completion(
    completion: TransferCompletion,
    client: &Client,
    cache: &mut WorkerCache,
    events: &mpsc::Sender<NetworkEvent>,
) -> Result<()> {
    match completion {
        TransferCompletion::Send {
            chat_id,
            local_id,
            path,
            caption,
            as_photo,
            result,
        } => match *result {
            Ok(message) if message.id() <= 0 => {
                events
                    .send(NetworkEvent::MessageAccepted { chat_id, local_id })
                    .await?;
            }
            Ok(message) => {
                events
                    .send(NetworkEvent::MessageSent {
                        local_id,
                        message: Box::pin(map_message(client, &message, cache)).await?,
                    })
                    .await?;
            }
            Err(error) => {
                events
                    .send(NetworkEvent::AttachmentSendFailed {
                        chat_id,
                        local_id,
                        path,
                        caption,
                        as_photo,
                        error: format!("Could not send attachment: {error}"),
                    })
                    .await?;
            }
        },
        TransferCompletion::Download {
            chat_id,
            message_id,
            result,
        } => match result {
            Ok(path) => {
                events
                    .send(NetworkEvent::AttachmentDownloaded {
                        chat_id,
                        message_id,
                        path,
                    })
                    .await?;
            }
            Err(error) => {
                events
                    .send(NetworkEvent::AttachmentDownloadFailed {
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

    let suggested_name = attachment_from_media(&media).map_or_else(
        || "attachment".to_owned(),
        |attachment| attachment.display_name().to_owned(),
    );
    let path = unique_download_path(&directory, chat_id, message_id, &suggested_name).await?;
    let partial = PartialDownload::new(path);
    client
        .download_media(&media, &partial.path)
        .await
        .context("Telegram media transfer failed")?;
    Ok(partial.finish())
}

async fn ensure_download_dir(cache: &mut WorkerCache) -> Result<PathBuf> {
    if let Some(path) = &cache.download_dir {
        return Ok(path.clone());
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("termgram-{}-{nonce}", std::process::id()));
    tokio::fs::create_dir(&root)
        .await
        .with_context(|| format!("could not create temporary directory {}", root.display()))?;
    protect_download_dir(&root).await?;
    cache.download_dir = Some(root.clone());
    Ok(root)
}

#[cfg(unix)]
async fn protect_download_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .await
        .with_context(|| format!("could not protect temporary directory {}", path.display()))
}

#[cfg(not(unix))]
async fn protect_download_dir(_path: &Path) -> Result<()> {
    Ok(())
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
    cache: &mut WorkerCache,
    url: &str,
) -> Result<(Chat, Option<Message>)> {
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
                cache.hidden_broadcasts.insert(id);
                bail!("broadcast channels are outside Termgram's messaging scope")
            }
            let peer = resolved
                .to_ref()
                .await
                .map_err(|error| anyhow!(error.to_string()))?
                .context("Telegram did not provide an addressable peer reference")?;
            cache.peers.insert(id, peer);
            cache.linked_peers.insert(id);
            cache_sender_name(cache, resolved.id(), safe_name(resolved.name(), "Unknown"));
            if matches!(&resolved, Peer::Group(_)) && resolved.id().kind() == PeerKind::Channel {
                cache.visible_channel_groups.insert(id);
            }
            let chat = Chat {
                id,
                title: safe_name(resolved.name(), "Unknown"),
                kind: match &resolved {
                    Peer::User(_) => ChatKind::Direct,
                    Peer::Group(_) => ChatKind::Group,
                    Peer::Channel(_) => unreachable!("broadcast channels are rejected above"),
                },
                unread: 0,
                last_message: String::new(),
                last_activity: None,
            };
            (chat, peer, message_id)
        }
        TelegramLink::Private {
            chat_id,
            message_id,
        } => {
            let peer = *cache.peers.get(&chat_id).context(
                "private message links can only open groups already present in your conversations",
            )?;
            if cache.hidden_broadcasts.contains(&chat_id)
                || !cache.visible_channel_groups.contains(&chat_id)
            {
                bail!("private link does not target a visible group conversation")
            }
            let peer_id = peer.id;
            let chat = Chat {
                id: chat_id,
                title: cache
                    .names
                    .get(&peer_id)
                    .cloned()
                    .unwrap_or_else(|| "Unknown".to_owned()),
                kind: ChatKind::Group,
                unread: 0,
                last_message: String::new(),
                last_activity: None,
            };
            cache.linked_peers.insert(chat_id);
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
        Some(Box::pin(map_message(client, &telegram_message, cache)).await?)
    } else {
        None
    };
    Ok((chat, message))
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

async fn map_message(
    client: &Client,
    message: &TelegramMessage,
    cache: &mut WorkerCache,
) -> Result<Message> {
    let chat_id = peer_id(message)?;
    let media = message.media();
    let (sender, reply_sender) = if message.outgoing() {
        let sender = "You".to_owned();
        let reply_sender = username_or_sender(message.sender().and_then(Peer::username), &sender);
        (sender, reply_sender)
    } else {
        resolve_sender(client, message, cache).await
    };
    let reply_to = reply_info(message, chat_id, cache);
    cache_message_sender(cache, chat_id, message.id(), reply_sender);
    Ok(Message {
        id: message.id(),
        chat_id,
        sender,
        reply_to,
        // Telegram stores the Unicode fallback for custom-emoji entities in
        // the raw message string. Keep that text instead of trying to render
        // the custom document in a terminal.
        text: message_text_with_link_fallbacks(message),
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

async fn resolve_sender(
    client: &Client,
    message: &TelegramMessage,
    cache: &mut WorkerCache,
) -> (String, String) {
    let Some(sender_id) = message.sender_id() else {
        return ("Unknown".to_owned(), "Unknown".to_owned());
    };
    if let Some(peer) = message.sender() {
        let name = safe_name(peer.name(), "Unknown");
        let reply_sender = username_or_sender(peer.username(), &name);
        cache_sender_name(cache, sender_id, name.clone());
        return (name, reply_sender);
    }
    if let Some(name) = cache.names.get(&sender_id) {
        return (name.clone(), name.clone());
    }
    let Ok(Some(sender)) = message.sender_ref().await else {
        return ("Unknown".to_owned(), "Unknown".to_owned());
    };
    let Ok(peer) = client.resolve_peer(sender).await else {
        return ("Unknown".to_owned(), "Unknown".to_owned());
    };
    let name = safe_name(peer.name(), "Unknown");
    let reply_sender = username_or_sender(peer.username(), &name);
    cache_sender_name(cache, sender_id, name.clone());
    (name, reply_sender)
}

fn username_or_sender(username: Option<&str>, sender: &str) -> String {
    let username = sanitize_terminal_line(username.unwrap_or_default());
    if username.is_empty() {
        sender.to_owned()
    } else {
        format!("@{username}")
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
    client: &Client,
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

    let Some(peer_ref) = message
        .peer_ref()
        .await
        .map_err(|error| anyhow!(error.to_string()))?
    else {
        // Unknown channel-shaped peers may be either broadcasts or megagroups.
        // Drop this update safely, but do not poison either classification cache.
        return Ok(None);
    };
    match client.resolve_peer(peer_ref).await {
        Ok(Peer::Group(group)) => {
            cache.peers.insert(chat_id, peer_ref);
            cache_sender_name(cache, group.id(), safe_name(group.title(), "Unknown"));
            cache.visible_channel_groups.insert(chat_id);
            Ok(Some(false))
        }
        Ok(Peer::Channel(_)) => {
            cache.hidden_broadcasts.insert(chat_id);
            cache.visible_channel_groups.remove(&chat_id);
            Ok(Some(true))
        }
        Ok(Peer::User(_)) => Ok(Some(false)),
        Err(_) => Ok(None),
    }
}

fn peer_id(message: &TelegramMessage) -> Result<ChatId> {
    message
        .peer_id()
        .bot_api_dialog_id()
        .context("message has no stable peer identifier")
}

fn message_preview(message: &TelegramMessage) -> String {
    let media = message.media();
    message_preview_with_media(&message_text_with_link_fallbacks(message), media.as_ref())
}

/// Telegram may hide a URL behind formatted display text. A terminal cannot
/// click that entity directly, so append supported Telegram targets as plain
/// text while leaving already-visible URLs untouched.
fn message_text_with_link_fallbacks(message: &TelegramMessage) -> String {
    let mut text = sanitize_terminal_text(message.text());
    let Some(entities) = message.fmt_entities() else {
        return text;
    };
    for entity in entities {
        let grammers_client::tl::enums::MessageEntity::TextUrl(entity) = entity else {
            continue;
        };
        let url = sanitize_terminal_line(&entity.url);
        if url.is_empty() || text.contains(&url) || parse_telegram_link(&url).is_err() {
            continue;
        }
        if !text.is_empty() {
            text.push(' ');
        }
        text.push('<');
        text.push_str(&url);
        text.push('>');
    }
    text
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
                kind,
                file_name: safe_media_name(document.name()),
                mime_type,
                size: document.size().and_then(|size| u64::try_from(size).ok()),
                fallback_emoji: None,
            })
        }
        Media::Sticker(sticker) => Some(Attachment {
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::{Duration, Instant};

    use grammers_session::types::PeerId;

    use crate::model::{Delivery, Message, ReplyInfo};

    use super::{
        advance_dialog_watermark, begin_unresolved_refresh, cache_message_sender,
        cache_sender_name, hydrate_reply_sender, parse_telegram_link, reconcile_dialog_snapshot,
        sanitize_download_name, username_or_sender, TelegramLink, WorkerCache,
        MESSAGE_SENDER_CACHE_LIMIT, TRANSIENT_SENDER_NAME_LIMIT, UNRESOLVED_REFRESH_COOLDOWN,
    };

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
        }

        assert_eq!(
            cache.transient_name_order.len(),
            TRANSIENT_SENDER_NAME_LIMIT
        );
        assert_eq!(cache.names.len(), TRANSIENT_SENDER_NAME_LIMIT + 1);
        assert_eq!(
            cache.names.get(&dialog_peer).map(String::as_str),
            Some("Current dialog")
        );
        assert!(!cache.names.contains_key(&PeerId::user_unchecked(2)));
        assert!(cache
            .names
            .contains_key(&PeerId::user_unchecked(last_sender)));
    }

    #[test]
    fn reply_sender_is_hydrated_from_bounded_message_index() {
        let mut cache = WorkerCache::default();
        cache_message_sender(&mut cache, 7, 41, "Alice".to_owned());
        let mut message = Message {
            id: 42,
            chat_id: 7,
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
        assert!(cache
            .message_senders
            .contains_key(&(1, i32::try_from(MESSAGE_SENDER_CACHE_LIMIT + 1).unwrap())));
    }

    #[test]
    fn reply_sender_hydration_is_scoped_to_the_conversation() {
        let mut cache = WorkerCache::default();
        cache_message_sender(&mut cache, 8, 41, "Other chat".to_owned());
        let mut message = Message {
            id: 42,
            chat_id: 7,
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
