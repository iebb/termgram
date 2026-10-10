use super::{App, ChatId, Focus, Mode, Screen, TelegramCommand};
use crate::event::ConnectionStatus;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

#[derive(Clone, Default)]
pub(super) struct State {
    pub visible: Option<(ChatId, i32)>,
    pending: BTreeMap<ChatId, i32>,
    pub visible_mentions: Vec<i32>,
    mentions_pending: BTreeMap<ChatId, Vec<i32>>,
    mentions_retry: BTreeMap<ChatId, Instant>,
    retry_after: BTreeMap<ChatId, Instant>,
    manual_pending: BTreeMap<ChatId, u64>,
    clear_marker: Option<ChatId>,
    pub entry: Option<Entry>,
    pub forward: Option<(ChatId, i32)>,
    pub paging: bool,
}

#[derive(Clone)]
pub(super) struct Entry {
    pub chat_id: ChatId,
    pub boundary: i32,
    pub message: Option<i32>,
    pub confirmed: bool,
}

impl App {
    /// Called after a successful frame. Merely loading or selecting a chat is
    /// not evidence that its messages were displayed.
    pub fn request_visible_read(&mut self) -> Vec<TelegramCommand> {
        if self.screen != Screen::Main
            || !self.terminal_focused
            || self.connection != ConnectionStatus::Online
            || !matches!(self.mode, Mode::Navigate | Mode::Compose)
        {
            return Vec::new();
        }
        let Some((chat_id, max_id)) = self
            .reads
            .visible
            .filter(|(id, _)| Some(*id) == self.active_chat_id)
        else {
            return Vec::new();
        };
        if self
            .reads
            .entry
            .as_ref()
            .is_some_and(|entry| entry.chat_id == chat_id && !entry.confirmed)
        {
            return Vec::new();
        }
        let mut commands = self.request_mention_read(chat_id);
        if self.reads.clear_marker == Some(chat_id)
            && !self.reads.manual_pending.contains_key(&chat_id)
        {
            self.reads.clear_marker = None;
            commands.extend(self.set_chat_unread_inner(chat_id, false, false));
        }
        let Some(previous) = self
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .and_then(|chat| chat.read_inbox_max_id)
        else {
            return commands;
        };
        if max_id <= previous
            || max_id <= 0
            || self.reads.pending.contains_key(&chat_id)
            || self
                .reads
                .retry_after
                .get(&chat_id)
                .is_some_and(|until| *until > Instant::now())
        {
            return commands;
        }
        self.reads.pending.insert(chat_id, max_id);
        commands.push(TelegramCommand::MarkRead { chat_id, max_id });
        commands
    }

    pub fn set_visible_mentions(&mut self, ids: Vec<i32>) {
        self.reads.visible_mentions = ids;
    }

    fn request_mention_read(&mut self, chat_id: ChatId) -> Vec<TelegramCommand> {
        if self.reads.mentions_pending.contains_key(&chat_id)
            || self
                .reads
                .mentions_retry
                .get(&chat_id)
                .is_some_and(|until| *until > Instant::now())
        {
            return Vec::new();
        }
        let message_ids = self
            .active_messages()
            .iter()
            .filter(|message| {
                message.id > 0
                    && !message.outgoing
                    && self.reads.visible_mentions.contains(&message.id)
                    && message
                        .mention
                        .as_ref()
                        .is_some_and(|mention| mention.unread && !mention.requires_playback)
            })
            .map(|message| message.id)
            .take(100)
            .collect::<Vec<_>>();
        if message_ids.is_empty() {
            return Vec::new();
        }
        self.reads
            .mentions_pending
            .insert(chat_id, message_ids.clone());
        vec![TelegramCommand::ReadMentions {
            chat_id,
            message_ids,
        }]
    }

    pub(super) fn mentions_read_finished(
        &mut self,
        chat_id: ChatId,
        ids: &[i32],
        error: Option<String>,
    ) {
        if self
            .reads
            .mentions_pending
            .get(&chat_id)
            .is_some_and(|pending| pending == ids)
        {
            self.reads.mentions_pending.remove(&chat_id);
            // The common PTS receipt may still be behind its RPC completion.
            self.reads
                .mentions_retry
                .insert(chat_id, Instant::now() + Duration::from_secs(2));
        }
        if let Some(error) = error {
            self.status_message = Some(crate::model::sanitize_terminal_line(&error));
        }
    }

    pub(super) fn contents_read(&mut self, channel_id: Option<ChatId>, ids: &[i32]) {
        let messages = self
            .messages
            .values_mut()
            .flatten()
            .chain(self.history_changes.values_mut().filter_map(Option::as_mut))
            .chain(self.message_pins.page.messages.iter_mut())
            .chain(
                self.message_pins
                    .head
                    .iter_mut()
                    .flat_map(|(_, page)| &mut page.messages),
            );
        for message in messages {
            message.acknowledge_contents(channel_id, ids);
        }
    }

    pub(super) fn read_finished(&mut self, chat_id: ChatId, max_id: i32, failed: bool) {
        if self.reads.pending.get(&chat_id) == Some(&max_id) {
            self.reads.pending.remove(&chat_id);
        }
        if failed {
            self.reads
                .retry_after
                .insert(chat_id, Instant::now() + Duration::from_secs(2));
        } else {
            self.reads.retry_after.remove(&chat_id);
        }
    }

    pub fn set_visible_read_boundary(&mut self, chat_id: ChatId, max_id: Option<i32>) {
        self.reads.visible = max_id.map(|id| (chat_id, id));
    }

    pub(super) fn set_chat_unread(
        &mut self,
        chat_id: ChatId,
        unread: bool,
    ) -> Vec<TelegramCommand> {
        self.reads.clear_marker = None;
        self.set_chat_unread_inner(chat_id, unread, !unread)
    }

    fn set_chat_unread_inner(
        &mut self,
        chat_id: ChatId,
        unread: bool,
        read_history: bool,
    ) -> Vec<TelegramCommand> {
        if self.connection != ConnectionStatus::Online {
            self.status_message = Some("Connect to Telegram first".to_owned());
            return Vec::new();
        }
        if self.reads.manual_pending.contains_key(&chat_id) {
            self.status_message = Some("This chat's read state is still updating".to_owned());
            return Vec::new();
        }
        let request_id = self.next_history_request_id;
        self.next_history_request_id = request_id.wrapping_add(1).max(1);
        self.reads.manual_pending.insert(chat_id, request_id);
        vec![TelegramCommand::SetChatUnread {
            chat_id,
            unread,
            read_history,
            request_id,
        }]
    }

    pub(super) fn finish_chat_unread(
        &mut self,
        chat_id: ChatId,
        request_id: u64,
        unread: bool,
        error: Option<String>,
    ) {
        if self.reads.manual_pending.get(&chat_id) != Some(&request_id) {
            return;
        }
        self.reads.manual_pending.remove(&chat_id);
        self.status_message = Some(error.unwrap_or_else(|| {
            if unread {
                "Chat marked unread".to_owned()
            } else {
                "Read state updated".to_owned()
            }
        }));
        self.clamp_chat_selection();
    }

    pub(super) fn mark_focused_chat(&mut self, unread: bool) -> Vec<TelegramCommand> {
        let id = if self.focus == Focus::Chats {
            self.selected_chat_entry().map(|chat| chat.id)
        } else {
            self.active_chat_id
        };
        id.map_or_else(Vec::new, |id| self.set_chat_unread(id, unread))
    }

    pub(super) fn prepare_unread_entry(&mut self, chat_id: ChatId, resume: bool) -> Option<i32> {
        self.reads.entry = None;
        self.reads.forward = None;
        self.reads.paging = false;
        self.reads.visible = None;
        self.reads.visible_mentions.clear();
        self.reads.clear_marker = self
            .chats
            .iter()
            .find(|chat| chat.id == chat_id && chat.membership.unread_mark)
            .map(|chat| chat.id);
        let boundary = self
            .chats
            .iter()
            .find(|chat| chat.id == chat_id && resume && chat.unread > 0)
            .and_then(|chat| chat.read_inbox_max_id)
            .filter(|boundary| {
                self.chats
                    .iter()
                    .find(|chat| chat.id == chat_id)
                    .and_then(|chat| chat.last_message_id)
                    .is_none_or(|last| last > *boundary)
            })?;
        self.reads.entry = Some(Entry {
            chat_id,
            boundary,
            message: None,
            confirmed: false,
        });
        self.message_scroll = 1;
        Some(boundary)
    }

    pub(super) fn position_unread_entry(&mut self, chat_id: ChatId, confirmed: bool) {
        let Some(entry) = self
            .reads
            .entry
            .as_ref()
            .filter(|entry| entry.chat_id == chat_id && !entry.confirmed)
        else {
            return;
        };
        let target = self
            .messages
            .get(&chat_id)
            .into_iter()
            .flatten()
            .find(|message| message.id > entry.boundary && !message.outgoing)
            .map(|message| message.id);
        let entry = self.reads.entry.as_mut().expect("entry checked");
        entry.message = target;
        entry.confirmed = confirmed;
        if let Some(target) = target {
            self.viewport_anchor_message = Some(target);
            self.viewport_anchor_row = 0;
            self.message_scroll = 1;
        }
    }

    #[must_use]
    pub fn unread_separator(&self) -> Option<i32> {
        self.reads
            .entry
            .as_ref()
            .filter(|entry| Some(entry.chat_id) == self.active_chat_id)
            .and_then(|entry| entry.message)
    }

    #[must_use]
    pub fn has_newer_history(&self) -> bool {
        self.reads
            .forward
            .is_some_and(|(chat, _)| Some(chat) == self.active_chat_id)
    }

    pub(super) fn clear_unread_navigation(&mut self) {
        if self.reads.paging
            || self
                .reads
                .entry
                .as_ref()
                .is_some_and(|entry| !entry.confirmed)
        {
            self.active_history_request = None;
            self.loading_history = false;
        }
        self.reads.entry = None;
        self.reads.forward = None;
        self.reads.paging = false;
        self.reads.visible = None;
        self.reads.visible_mentions.clear();
        self.reads.clear_marker = None;
    }

    pub(super) fn first_unread(&mut self) -> Vec<TelegramCommand> {
        let Some(chat_id) = self.active_chat_id else {
            return Vec::new();
        };
        if self.active_chat().is_none_or(|chat| chat.unread == 0) {
            self.status_message = Some("No unread messages in this chat".to_owned());
            return Vec::new();
        }
        let mut commands = self.open_chat_by_id(chat_id);
        let boundary = self.prepare_unread_entry(chat_id, true);
        self.position_unread_entry(chat_id, false);
        for command in &mut commands {
            if let TelegramCommand::LoadHistory { after_id, .. } = command {
                *after_id = boundary;
            }
        }
        commands
    }

    pub(super) fn latest_history(&mut self) -> Vec<TelegramCommand> {
        self.active_chat_id
            .map_or_else(Vec::new, |chat_id| self.open_chat_by_id(chat_id))
    }

    /// Continue a confirmed forward page using the same generation and cache
    /// machinery as opening history. Preserve the physical reading anchor.
    pub(super) fn next_unread_page(&mut self) -> Vec<TelegramCommand> {
        let Some((chat_id, after_id)) = self
            .reads
            .forward
            .filter(|(chat, _)| Some(*chat) == self.active_chat_id)
        else {
            return Vec::new();
        };
        if self.loading_history || self.connection != ConnectionStatus::Online {
            return Vec::new();
        }
        let request_id = self.next_history_request_id;
        self.next_history_request_id = request_id.wrapping_add(1).max(1);
        self.active_history_request = Some((chat_id, request_id));
        self.history_target_message = self.viewport_anchor_message.or(Some(after_id));
        self.viewport_anchor_message = self.history_target_message;
        self.viewport_anchor_row = 0;
        self.message_scroll = self.message_scroll.max(1);
        self.reads.paging = true;
        self.loading_history = true;
        self.history_changes.clear();
        self.history_read_max = 0;
        vec![TelegramCommand::LoadHistory {
            chat_id,
            request_id,
            after_id: Some(after_id),
        }]
    }
}
