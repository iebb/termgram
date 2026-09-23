//! The local store remains available while Telegram connects or authenticates.

use super::{
    COMMAND_QUEUE_CAPACITY, Config, EVENT_QUEUE_CAPACITY, NetworkEvent, Result, TelegramCommand,
    VecDeque, mpsc, run,
};
use crate::{
    cache::{Store, SyncCursor},
    model::Chat,
};

pub(super) struct Bootstrap {
    pub cursor: SyncCursor,
    pub chats: Vec<Chat>,
    pub account_id: Option<i64>,
    pub cache_owner: std::sync::Arc<std::fs::File>,
}
use tokio::{
    task::JoinSet,
    time::{Duration, MissedTickBehavior},
};

#[allow(clippy::too_many_lines)]
pub(super) async fn serve(
    config: Config,
    mut commands: mpsc::Receiver<TelegramCommand>,
    events: mpsc::Sender<NetworkEvent>,
    active_chat: tokio::sync::watch::Receiver<Option<super::ChatId>>,
    measure_latency: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    config.prepare_session_dir()?;
    let mut cache_path = config.session_path.as_os_str().to_os_string();
    cache_path.push(".cache.sqlite3");
    let mut store = Store::open(std::path::Path::new(&cache_path)).await?;
    let state_path = config.state_path.clone();
    let mut loaded_drafts = std::collections::BTreeSet::new();
    let (user_name, chats) = store.snapshot().await?;
    if let Some(user_id) = store.account_id().await? {
        events
            .send(NetworkEvent::LocalDrafts {
                user_id,
                drafts: crate::drafts::load(state_path.clone(), user_id).await?,
            })
            .await?;
        loaded_drafts.insert(user_id);
        events
            .send(NetworkEvent::AccountIdentity { user_id })
            .await?;
    }
    if !chats.is_empty() {
        events
            .send(NetworkEvent::CachedSnapshot {
                user_name,
                chats: chats.clone(),
            })
            .await?;
    }
    events
        .send(NetworkEvent::Folders(store.folders().await?))
        .await?;
    events
        .send(NetworkEvent::DialogPins(store.dialog_pins().await?))
        .await?;
    let bootstrap = Bootstrap {
        cache_owner: store.owner(),
        cursor: store.cursor().await?,
        chats,
        account_id: store.account_id().await?,
    };
    let (network_tx, network_commands) = mpsc::channel(COMMAND_QUEUE_CAPACITY);
    let (network_events, mut network_rx) = mpsc::channel(EVENT_QUEUE_CAPACITY);
    let mut tasks = JoinSet::new();
    tasks.spawn(run(
        config,
        network_commands,
        network_events,
        bootstrap,
        active_chat,
        measure_latency,
    ));
    let mut search = crate::search::Worker::new(std::path::PathBuf::from(cache_path));
    let mut pending = VecDeque::new();
    let mut preparation = crate::staging::Worker::new(state_path.clone(), store.owner());
    let mut changes = Vec::new();
    let mut ready = false;
    let mut flush = tokio::time::interval(Duration::from_millis(100));
    flush.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        search.start_pending();
        tokio::select! {
            event = preparation.next() => { if let Some(event) = event { events.send(event).await?; } },
            result = search.next(), if search.running() => { if let Some(event) = result { events.send(event).await?; } },
            command = commands.recv() => {
                let Some(mut command) = command else { break; };
                if let TelegramCommand::PrepareAttachments(request) = command {
                    if let Some(event) = preparation.start(request) { events.send(event).await?; }
                    continue;
                }
                if command.cloud_search_id().is_some() || matches!(command, TelegramCommand::CancelSearch | TelegramCommand::SearchCached(_)) {
                    search.cancel();
                    pending.retain(|queued: &TelegramCommand| queued.cloud_search_id().is_none() && !matches!(queued, TelegramCommand::CancelSearch));
                    if matches!(command, TelegramCommand::SearchCached(_)) && ready {
                        pending.push_back(TelegramCommand::CancelSearch);
                    }
                    if matches!(command, TelegramCommand::CancelSearch) && !ready { continue; }
                }
                if serve_cached(&mut command, &mut store, &mut changes, &events, &mut search).await? { continue; }
                if authentication_command(&command) {
                    pending.push_front(command);
                } else if pending.len() < COMMAND_QUEUE_CAPACITY {
                    pending.push_back(command);
                } else if let Some(event) = command.failure("Telegram command queue is busy".to_owned()) {
                    store.apply(std::slice::from_ref(&event)).await?;
                    events.send(event).await?;
                }
            }
            permit = network_tx.reserve(), if pending.front().is_some_and(|command| ready || authentication_command(command)) => {
                if let Ok(permit) = permit {
                    permit.send(pending.pop_front().expect("pending command"));
                } else { break; }
            }
            event = network_rx.recv() => {
                let Some(mut event) = event else {
                    store.apply(&changes).await?;
                    return tasks.join_next().await.transpose()?.unwrap_or(Ok(()));
                };
                match &event {
                    NetworkEvent::AccountIdentity { user_id } if loaded_drafts.insert(*user_id) => {
                        events.send(NetworkEvent::LocalDrafts { user_id: *user_id, drafts: crate::drafts::load(state_path.clone(), *user_id).await? }).await?;
                    }
                    NetworkEvent::Ready { .. } => ready = true,
                    NetworkEvent::Auth(_) => ready = false,
                    NetworkEvent::CacheAccountReset { .. } => { pending.clear(); search.cancel(); },
                    _ => {}
                }
                let cloud = matches!(event, NetworkEvent::CloudSearchResults { .. } | NetworkEvent::CloudSearchContext { .. });
                let snapshot = Store::has_message_snapshot(&event);
                if snapshot {
                    store.apply(&changes).await?;
                    changes.clear();
                    store.reconcile_cloud_search(&mut event).await?;
                    store.reconcile_snapshots(&mut event).await?;
                }
                let checkpoint = matches!(&event, NetworkEvent::SyncCheckpoint(_));
                changes.push(event.clone());
                if checkpoint || cloud || changes.len() >= 256 {
                    store.apply(&changes).await?;
                    changes.clear();
                }
                events.send(event).await?;
            }
            _ = flush.tick(), if !changes.is_empty() => {
                store.apply(&changes).await?;
                changes.clear();
            }
        }
    }
    store.apply(&changes).await?;
    tasks.shutdown().await;
    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn serve_cached(
    command: &mut TelegramCommand,
    store: &mut Store,
    changes: &mut Vec<NetworkEvent>,
    events: &mpsc::Sender<NetworkEvent>,
    search: &mut crate::search::Worker,
) -> Result<bool> {
    if matches!(
        command,
        TelegramCommand::LoadHistory { .. }
            | TelegramCommand::LoadReplyPreviews { .. }
            | TelegramCommand::LoadPinnedMessages { .. }
            | TelegramCommand::LoadPinnedContext { .. }
            | TelegramCommand::LoadOlder { .. }
            | TelegramCommand::LoadCachedContext { .. }
            | TelegramCommand::LoadStickers { .. }
            | TelegramCommand::LoadStickerSet { .. }
            | TelegramCommand::DownloadAttachment { .. }
            | TelegramCommand::SearchCached(_)
    ) {
        store.apply(changes).await?;
        changes.clear();
    }
    match command {
        TelegramCommand::LoadReplyPreviews {
            chat_id,
            message_ids,
            request_id,
        } => {
            let (messages, unavailable) = store.reply_previews(*chat_id, message_ids).await?;
            let complete = messages.len() + unavailable.len() == message_ids.len();
            events
                .send(NetworkEvent::ReplyPreviews {
                    chat_id: *chat_id,
                    request_id: *request_id,
                    messages,
                    unavailable,
                    complete,
                })
                .await?;
            return Ok(complete);
        }
        TelegramCommand::LoadPinnedMessages {
            chat_id,
            before,
            request_id,
        } => {
            let page = store.pinned_messages(*chat_id, *before).await?;
            events
                .send(NetworkEvent::PinnedMessages {
                    chat_id: *chat_id,
                    request_id: *request_id,
                    page,
                })
                .await?;
        }
        TelegramCommand::LoadPinnedContext {
            chat_id,
            message_id,
            request_id,
        } => {
            let messages = store
                .history(
                    *chat_id,
                    Some(message_id.saturating_add(1)),
                    super::HISTORY_LIMIT,
                )
                .await?;
            if messages.iter().any(|message| message.id == *message_id) {
                events
                    .send(NetworkEvent::PinnedContext {
                        chat_id: *chat_id,
                        message_id: *message_id,
                        request_id: *request_id,
                        messages,
                    })
                    .await?;
            }
        }
        TelegramCommand::DownloadAttachment {
            chat_id,
            message_id,
            request_id,
            media_id,
        } => {
            if let Some(path) = store.attachment(*chat_id, *message_id, *media_id).await? {
                events
                    .send(NetworkEvent::AttachmentDownloaded {
                        chat_id: *chat_id,
                        message_id: *message_id,
                        request_id: *request_id,
                        path,
                    })
                    .await?;
                return Ok(true);
            }
            store
                .apply(&[NetworkEvent::AttachmentDownloadStarted {
                    chat_id: *chat_id,
                    message_id: *message_id,
                    request_id: *request_id,
                    media_id: *media_id,
                }])
                .await?;
        }
        TelegramCommand::SearchCached(request) => {
            search.queue(request.clone());
            return Ok(true);
        }
        TelegramCommand::LoadStickers { request_id, cached } => {
            // The cached overview opens the panel instantly, then the network
            // worker revalidates each section with its stored hash.
            if let Some(overview) = store.sticker_overview().await? {
                events
                    .send(NetworkEvent::StickersLoaded {
                        request_id: *request_id,
                        validated: false,
                        result: Ok(overview.clone()),
                    })
                    .await?;
                *cached = Some(overview);
            }
        }
        TelegramCommand::LoadStickerSet {
            set,
            request_id,
            cached,
        } => {
            if let Some(section) = store.sticker_set(set.id).await? {
                events
                    .send(NetworkEvent::StickerSetLoaded {
                        request_id: *request_id,
                        set_id: set.id,
                        validated: false,
                        result: Ok(section.clone()),
                    })
                    .await?;
                *cached = Some(section);
            }
        }
        TelegramCommand::CancelSearch => search.cancel(),
        TelegramCommand::LoadCachedContext {
            chat_id,
            message_id,
            request_id,
        } => {
            let event = match store.context(*chat_id, *message_id).await {
                Ok(messages) => NetworkEvent::CachedContext {
                    request_id: *request_id,
                    chat_id: *chat_id,
                    message_id: *message_id,
                    messages,
                },
                Err(error) => NetworkEvent::SearchFailed {
                    request_id: *request_id,
                    error: format!("{error:#}"),
                },
            };
            events.send(event).await?;
            return Ok(true);
        }
        TelegramCommand::LoadOlder {
            chat_id,
            request_id,
            before_id,
        } => {
            if let Some(messages) = store.older_page(*chat_id, *before_id).await? {
                events
                    .send(NetworkEvent::OlderHistory {
                        chat_id: *chat_id,
                        request_id: *request_id,
                        before_id: *before_id,
                        messages,
                    })
                    .await?;
                return Ok(true);
            }
        }
        TelegramCommand::LoadHistory {
            chat_id,
            request_id,
            after_id,
        } => {
            let messages = if let Some(after) = after_id {
                store
                    .history_after(*chat_id, *after, super::HISTORY_LIMIT)
                    .await?
            } else {
                store.history(*chat_id, None, super::HISTORY_LIMIT).await?
            };
            if !messages.is_empty() {
                events
                    .send(NetworkEvent::CachedHistory {
                        chat_id: *chat_id,
                        request_id: *request_id,
                        messages,
                    })
                    .await?;
            }
        }
        _ => {}
    }
    Ok(false)
}

fn authentication_command(command: &TelegramCommand) -> bool {
    matches!(
        command,
        TelegramCommand::StartQrAuth
            | TelegramCommand::SubmitPhone(_)
            | TelegramCommand::SubmitCode(_)
            | TelegramCommand::SubmitPassword(_)
            | TelegramCommand::RestartAuth
            | TelegramCommand::Shutdown
    )
}
