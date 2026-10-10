// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Methods to deal with and offer access to updates.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use grammers_mtsender::InvocationError;
use grammers_session::ErasedSession;
use grammers_session::types::{PeerId, PeerInfo, UpdateState, UpdatesState};
use grammers_session::updates::{MessageBoxes, PrematureEndReason, State, UpdatesLike};
use grammers_tl_types as tl;
use log::{trace, warn};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::timeout_at;

use super::{Client, UpdatesConfiguration};
use crate::peer::PeerMap;
use crate::update::Update;

/// How long to wait after warning the user that the updates limit was exceeded.
const UPDATE_LIMIT_EXCEEDED_LOG_COOLDOWN: Duration = Duration::from_secs(300);

// See https://core.telegram.org/method/updates.getChannelDifference.
const BOT_CHANNEL_DIFF_LIMIT: i32 = 100000;
const USER_CHANNEL_DIFF_LIMIT: i32 = 100;

/// Channel differences requested at once. Telegram Desktop requests one per
/// channel without a limit; a small bound keeps a large backlog from flooding
/// the connection that also carries the user's own requests.
const MAX_CHANNEL_DIFFERENCES: usize = 4;

/// A finished difference request, applied by the stream that owns the state.
enum Recovered {
    Account(Result<tl::enums::updates::Difference, InvocationError>),
    Channel(
        i64,
        Result<tl::enums::updates::ChannelDifference, InvocationError>,
    ),
}

async fn prepare_channel_difference(
    mut request: tl::functions::updates::GetChannelDifference,
    channel_id: i64,
    session: &ErasedSession,
    message_box: &mut MessageBoxes,
) -> Result<
    Option<tl::functions::updates::GetChannelDifference>,
    Box<dyn std::error::Error + Send + Sync>,
> {
    if let Some(PeerInfo::Channel {
        id,
        auth: Some(auth),
        ..
    }) = session.peer(PeerId::channel_unchecked(channel_id)).await?
    {
        request.channel = tl::enums::InputChannel::Channel(tl::types::InputChannel {
            channel_id: id,
            access_hash: auth.hash(),
        });
        request.limit = if session
            .peer(PeerId::self_user())
            .await?
            .map(|user| match user {
                PeerInfo::User { bot, .. } => bot.unwrap_or(false),
                _ => false,
            })
            .unwrap_or(false)
        {
            BOT_CHANNEL_DIFF_LIMIT
        } else {
            USER_CHANNEL_DIFF_LIMIT
        };
        trace!("requesting {:?}", request);
        Ok(Some(request))
    } else {
        warn!(
            "cannot getChannelDifference for {:?} as we're missing its hash",
            channel_id
        );
        message_box.end_channel_difference_for(channel_id, PrematureEndReason::Banned);
        Ok(None)
    }
}

/// Recovery work the stream performs before it returns to live updates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyncProgress {
    /// The account-wide difference is being fetched.
    pub account: bool,
    /// Channels still waiting for their difference.
    pub channels: usize,
    /// Messages recovered since this recovery began.
    pub messages: usize,
}

impl SyncProgress {
    pub fn is_idle(&self) -> bool {
        !self.account && self.channels == 0
    }
}

fn recovered_messages(updates: &[(tl::enums::Update, State)]) -> usize {
    updates
        .iter()
        .filter(|(update, _)| {
            matches!(
                update,
                tl::enums::Update::NewMessage(_) | tl::enums::Update::NewChannelMessage(_)
            )
        })
        .count()
}

/// Iterator returned by [`Client::stream_updates`].
pub struct UpdateStream {
    client: Client,
    message_box: MessageBoxes,
    // When did we last warn the user that the update queue filled up?
    // This is used to avoid spamming the log.
    last_update_limit_warn: Option<Instant>,
    buffer: VecDeque<(tl::enums::Update, State, PeerMap)>,
    updates: mpsc::UnboundedReceiver<UpdatesLike>,
    configuration: UpdatesConfiguration,
    should_get_state: bool,
    active_channel: watch::Sender<Option<(i64, i32)>>,
    active_changes: watch::Receiver<Option<(i64, i32)>>,
    progress: watch::Sender<SyncProgress>,
    /// Difference requests in flight. Live updates keep flowing meanwhile.
    recovering: JoinSet<Recovered>,
    account_requested: bool,
    channels_requested: Vec<i64>,
}

impl UpdateStream {
    /// Latest viewed channel and its initial PTS. None stops active short polling.
    /// Changing this handle does not cancel an in-flight update receive.
    pub fn active_channel(&self) -> watch::Sender<Option<(i64, i32)>> {
        self.active_channel.clone()
    }

    /// Observe difference recovery without a second update consumer. The value
    /// returns to idle, with a zero message count, once live updates resume.
    pub fn progress(&self) -> watch::Receiver<SyncProgress> {
        self.progress.subscribe()
    }

    fn publish_progress(&self, recovered: usize) {
        let (account, channels) = self.message_box.recovery();
        self.progress.send_if_modified(|current| {
            let next = if !account && channels == 0 {
                SyncProgress::default()
            } else {
                SyncProgress { account, channels, messages: current.messages + recovered }
            };
            let changed = *current != next;
            *current = next;
            changed
        });
    }

    /// Return one complete application batch and the cursor it covers.
    /// Persist every update before committing this cursor. In particular, a
    /// difference response can give several updates the same terminal PTS.
    pub async fn next_batch(&mut self) -> Result<(Vec<Update>, UpdatesState), InvocationError> {
        let first = self.next_raw().await?;
        let raw = std::iter::once(first).chain(self.buffer.drain(..));
        let updates = raw.map(|(update, state, peers)| {
            Update::from_raw(&self.client, update, state, peers)
        }).collect();
        Ok((updates, self.message_box.session_state()))
    }

    /// Restore the cursor owned by the application's durable message store.
    /// Call before polling this stream. Session peer/dialog cursors may be ahead
    /// of the messages persisted by an application and are not a checkpoint.
    pub fn restore_state(&mut self, state: UpdatesState) {
        assert!(self.buffer.is_empty(), "restore state before polling updates");
        self.message_box = MessageBoxes::load(state);
        self.should_get_state = self.message_box.is_empty();
    }

    /// Pops an update from the queue, waiting for an update to arrive first if the queue is empty.
    pub async fn next(&mut self) -> Result<Update, InvocationError> {
        let (update, state, peers) = self.next_raw().await?;
        Ok(Update::from_raw(&self.client, update, state, peers))
    }

    /// Pops an update from the queue, waiting for an update to arrive first if the queue is empty.
    ///
    /// Unlike [`Self::next`], the update is not wrapped at all, but it is still processed.
    pub async fn next_raw(
        &mut self,
    ) -> Result<(tl::enums::Update, State, PeerMap), InvocationError> {
        if self.should_get_state {
            self.should_get_state = false;
            match self
                .client
                .invoke(&tl::functions::updates::GetState {})
                .await
            {
                Ok(tl::enums::updates::State::State(state)) => {
                    self.client
                        .0
                        .session
                        .set_update_state(UpdateState::All(UpdatesState {
                            pts: state.pts,
                            qts: state.qts,
                            date: state.date,
                            seq: state.seq,
                            channels: Vec::new(),
                        }))
                        .await?;
                    self.message_box.set_state(state);
                    // A new application cache needs this initial checkpoint
                    // even if no live message arrives during the session.
                    // Signal that older content has no replayable coverage.
                    let state = self.message_box.session_state();
                    let peers = self.client.build_peer_map(Vec::new(), Vec::new()).await;
                    self.buffer.push_back((tl::enums::Update::PtsChanged, State {
                        date: state.date, seq: state.seq, message_box: None,
                    }, peers));
                }
                Err(_err) => {
                    // The account may no longer actually be logged in, or it can rarely fail.
                    // `message_box` will try to correct its state as updates arrive.
                }
            }
        }

        loop {
            if let Some(update) = self.buffer.pop_front() {
                return Ok(update);
            }
            let deadline = self.message_box.check_deadlines(); // first, as it might trigger differences
            self.publish_progress(0);
            self.request_differences().await?;

            tokio::select! {
                _ = self.active_changes.changed() => {
                    let active = *self.active_changes.borrow_and_update();
                    self.message_box.set_active_channel(active);
                }
                Some(recovered) = self.recovering.join_next(), if !self.recovering.is_empty() => {
                    // A request task only ends early by panicking.
                    let recovered = recovered.map_err(|_| InvocationError::Dropped)?;
                    self.apply_recovered(recovered).await?;
                }
                result = timeout_at(deadline.into(), self.updates.recv()) => match result {
                    Ok(Some(updates)) => self.process_socket_updates(updates).await?,
                    Ok(None) => break Err(InvocationError::Dropped),
                    Err(_) => {}
                }
            }
        }
    }

    /// Start every difference the message box asks for that is not already in
    /// flight. As in Telegram Desktop, the account difference and each
    /// channel's are independent requests, and none of them stops the socket.
    async fn request_differences(&mut self) -> Result<(), InvocationError> {
        if !self.account_requested {
            if let Some(request) = self.message_box.get_difference() {
                let client = self.client.clone();
                self.account_requested = true;
                self.recovering
                    .spawn(async move { Recovered::Account(client.invoke(&request).await) });
            }
        }
        while self.channels_requested.len() < MAX_CHANNEL_DIFFERENCES {
            let Some(request) = self
                .message_box
                .get_channel_difference_except(&self.channels_requested)
            else {
                break;
            };
            let channel_id = match &request.channel {
                tl::enums::InputChannel::Channel(channel) => channel.channel_id,
                _ => unreachable!(),
            };
            let Some(request) = prepare_channel_difference(
                request,
                channel_id,
                self.client.0.session.as_ref(),
                &mut self.message_box,
            )
            .await?
            else {
                continue;
            };
            let client = self.client.clone();
            self.channels_requested.push(channel_id);
            self.recovering.spawn(async move {
                Recovered::Channel(channel_id, client.invoke(&request).await)
            });
        }
        Ok(())
    }

    async fn apply_recovered(&mut self, recovered: Recovered) -> Result<(), InvocationError> {
        match recovered {
            Recovered::Account(response) => {
                self.account_requested = false;
                let response = response?;
                let invalidated = matches!(&response, tl::enums::updates::Difference::TooLong(_));
                let (mut updates, users, chats) = self.message_box.apply_difference(response);
                if invalidated {
                    let state = self.message_box.session_state();
                    updates.insert(0, (tl::enums::Update::PtsChanged, State {
                        date: state.date, seq: state.seq, message_box: None,
                    }));
                }
                self.publish_progress(recovered_messages(&updates));
                let peers = self.client.build_peer_map(users, chats).await;
                self.extend_update_queue(updates, peers);
            }
            Recovered::Channel(channel_id, response) => {
                self.channels_requested.retain(|id| *id != channel_id);
                let response = match response {
                    Ok(r) => r,
                    Err(e) if e.is("PERSISTENT_TIMESTAMP_OUTDATED") => {
                        // According to Telegram's docs:
                        // "Channel internal replication issues, try again later (treat this like an RPC_CALL_FAIL)."
                        // We can treat this as "empty difference" and not update the local pts.
                        // Then this same call will be retried when another gap is detected.
                        log::warn!(
                            "Getting difference for channel updates caused PersistentTimestampOutdated; ending getting difference prematurely until server issues are resolved"
                        );
                        self.message_box.end_channel_difference_for(
                            channel_id,
                            PrematureEndReason::TemporaryServerIssues,
                        );
                        return Ok(());
                    }
                    Err(e) if e.is("CHANNEL_PRIVATE") => {
                        log::info!(
                            "Account is now banned so we can no longer fetch updates for channel {}",
                            channel_id
                        );
                        self.message_box
                            .end_channel_difference_for(channel_id, PrematureEndReason::Banned);
                        return Ok(());
                    }
                    Err(InvocationError::Rpc(rpc_error)) if rpc_error.code == 500 => {
                        log::warn!("Telegram is having internal issues: {:#?}", rpc_error);
                        self.message_box.end_channel_difference_for(
                            channel_id,
                            PrematureEndReason::TemporaryServerIssues,
                        );
                        return Ok(());
                    }
                    Err(e) => return Err(e),
                };

                let invalidated = matches!(&response, tl::enums::updates::ChannelDifference::TooLong(_));
                let (mut updates, users, chats) = self
                    .message_box
                    .apply_channel_difference_for(channel_id, response);
                if invalidated {
                    let state = self.message_box.session_state();
                    updates.insert(0, (tl::types::UpdateChannelTooLong { channel_id, pts: None }.into(), State {
                        date: state.date, seq: state.seq, message_box: None,
                    }));
                }
                self.publish_progress(recovered_messages(&updates));
                let peers = self.client.build_peer_map(users, chats).await;
                self.extend_update_queue(updates, peers);
            }
        }
        Ok(())
    }

    pub(crate) async fn process_socket_updates(
        &mut self,
        updates: UpdatesLike,
    ) -> Result<(), InvocationError> {
        let mut result = Option::<(Vec<_>, Vec<_>, Vec<_>)>::None;
        match self.message_box.process_updates(updates) {
            Ok(tup) => {
                if let Some(res) = result.as_mut() {
                    res.0.extend(tup.0);
                    res.1.extend(tup.1);
                    res.2.extend(tup.2);
                } else {
                    result = Some(tup);
                }
            }
            Err(_) => return Ok(()),
        }

        if let Some((updates, users, chats)) = result {
            let peers = self.client.build_peer_map(users, chats).await;
            self.extend_update_queue(updates, peers);
        }

        Ok(())
    }

    fn extend_update_queue(
        &mut self,
        mut updates: Vec<(tl::enums::Update, State)>,
        peer_map: PeerMap,
    ) {
        if let Some(limit) = self.configuration.update_queue_limit {
            if let Some(exceeds) = (self.buffer.len() + updates.len()).checked_sub(limit + 1) {
                let exceeds = exceeds + 1;
                let now = Instant::now();
                let notify = match self.last_update_limit_warn {
                    None => true,
                    Some(instant) => now - instant > UPDATE_LIMIT_EXCEEDED_LOG_COOLDOWN,
                };

                updates.truncate(updates.len() - exceeds);
                if notify {
                    log::warn!(
                        "{} updates were dropped because the update_queue_limit was exceeded",
                        exceeds
                    );
                }

                self.last_update_limit_warn = Some(now);
            }
        }

        self.buffer
            .extend(updates.into_iter().map(|(u, s)| (u, s, peer_map.handle())));
    }

    /// Synchronize the updates state to the session.
    ///
    /// This is **not** automatically done on drop.
    pub async fn sync_update_state(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.client
            .0
            .session
            .set_update_state(UpdateState::All(self.message_box.session_state()))
            .await?;
        Ok(())
    }
}

impl Client {
    /// Returns an asynchronous stream of processed updates.
    ///
    /// The updates are guaranteed to be in order, and any gaps will be resolved.\
    /// **Important** to note that for gaps to be resolved, the peers must have been
    /// persisted in the session cache beforehand (i.e. be retrievable with [`Session::peer`]).
    /// A good way to achieve this is to use [`Self::iter_dialogs`] at least once after login.
    ///
    /// The updates are wrapped in [`crate::update::Update`] to make them more convenient to use,
    /// but their raw type is still accessible to bridge any missing functionality.
    pub async fn stream_updates(
        &self,
        updates: mpsc::UnboundedReceiver<UpdatesLike>,
        configuration: UpdatesConfiguration,
    ) -> Result<UpdateStream, Box<dyn std::error::Error + Send + Sync>> {
        let message_box = if configuration.catch_up {
            MessageBoxes::load(self.0.session.updates_state().await?)
        } else {
            // If the user doesn't want to bother with catching up on previous update, start with
            // pristine state instead.
            MessageBoxes::new()
        };
        // Don't bother getting pristine update state if we're not logged in.
        let should_get_state =
            message_box.is_empty() && self.0.session.peer(PeerId::self_user()).await?.is_some();

        let (active_channel, active_changes) = watch::channel(None);
        Ok(UpdateStream {
            progress: watch::channel(SyncProgress::default()).0,
            recovering: JoinSet::new(),
            account_requested: false,
            channels_requested: Vec::new(),
            active_channel,
            active_changes,
            client: self.clone(),
            message_box,
            last_update_limit_warn: None,
            buffer: VecDeque::new(),
            updates,
            configuration,
            should_get_state,
        })
    }
}

#[cfg(test)]
mod tests {
    use core::future::Future;

    use super::*;

    fn get_update_stream() -> UpdateStream {
        panic!()
    }

    #[test]
    fn ensure_next_update_future_impls_send() {
        if false {
            // We just want it to type-check, not actually run.
            fn typeck(_: impl Future + Send) {}
            typeck(get_update_stream().next());
        }
    }
}
