//! Composer `@`-mention and `/`-command completion state.
//!
//! Like command-mode completion, the popup holds a snapshot of candidates:
//! cycling applies a row without recomputing, while any real edit rebuilds the
//! list from the token now under the cursor.

use std::collections::BTreeSet;

use super::{App, ChatId, Mode, TelegramCommand};
use crate::completion::{self, Member, Suggestion, Trigger};

/// Popup bookkeeping for the active completion token. The state is derived:
/// it is rebuilt from the draft on every edit and dropped whenever the token
/// disappears.
#[derive(Clone, Debug)]
pub struct Popup {
    pub trigger: Trigger,
    /// Byte offset of the trigger character in the current draft.
    pub start: usize,
    /// Snapshot of matching rows from the last edit or member update.
    pub candidates: Vec<Suggestion>,
    /// Row applied by the last cycle, like `commands::State::selected`.
    /// `None` means nothing has been applied yet; accept still uses row 0.
    pub selected: Option<usize>,
    /// Esc hides the popup until the token itself disappears or moves.
    pub dismissed: bool,
}

impl App {
    /// The rendered popup, when it has something to show.
    #[must_use]
    pub fn completion_popup(&self) -> Option<&Popup> {
        self.completion
            .as_ref()
            .filter(|popup| !popup.dismissed && !popup.candidates.is_empty())
    }

    /// Whether the popup is consuming Enter, Esc and selection keys.
    pub(super) fn completion_visible(&self) -> bool {
        self.completion_popup().is_some()
    }

    /// Queue the one-shot member/command fetch the first time a chat can
    /// complete. Failures leave the transcript-derived candidates working.
    pub(super) fn request_completion_data(&mut self, chat_id: ChatId) -> Option<TelegramCommand> {
        if !self.completion_requested.insert(chat_id) {
            return None;
        }
        let request_id = self.next_completion_request_id;
        self.next_completion_request_id = self.next_completion_request_id.wrapping_add(1).max(1);
        Some(TelegramCommand::LoadMembers {
            chat_id,
            request_id,
        })
    }

    /// Members from the server snapshot plus senders already in the loaded
    /// transcript, so completion works before the fetch finishes.
    fn completion_members(&self, chat_id: ChatId) -> Vec<Member> {
        let fetched = self
            .chat_completion
            .get(&chat_id)
            .map(|data| data.members.clone())
            .unwrap_or_default();
        let senders = self
            .messages
            .get(&chat_id)
            .into_iter()
            .flatten()
            .filter_map(|message| {
                Some(Member {
                    name: message.sender.clone(),
                    username: message.sender_username.clone()?,
                    bot: false,
                })
            });
        fetched.into_iter().chain(senders).collect()
    }

    fn completion_candidates(
        &self,
        chat_id: ChatId,
        trigger: Trigger,
        query: &str,
    ) -> Vec<Suggestion> {
        match trigger {
            Trigger::Mention => {
                completion::mention_suggestions(self.completion_members(chat_id), query)
            }
            Trigger::Command => {
                let commands = self
                    .chat_completion
                    .get(&chat_id)
                    .map_or(&[][..], |data| data.commands.as_slice());
                let bots = commands
                    .iter()
                    .filter_map(|entry| entry.bot.as_ref())
                    .collect::<BTreeSet<_>>()
                    .len();
                completion::command_suggestions(commands, query, bots > 1)
            }
        }
    }

    /// Rebuild the popup after an edit, a cursor move or a member update.
    /// The returned commands carry the one-shot data request, if any.
    pub(super) fn refresh_completion(&mut self, chat_id: ChatId) -> Vec<TelegramCommand> {
        let token = (self.mode == Mode::Compose && self.active_chat_id == Some(chat_id))
            .then(|| {
                self.draft_data(chat_id)
                    .and_then(|draft| completion::token(draft.input.value(), draft.input.cursor()))
            })
            .flatten();
        let Some((trigger, start)) = token else {
            self.completion = None;
            return Vec::new();
        };
        let mut commands = Vec::new();
        commands.extend(self.request_completion_data(chat_id));
        let query = self
            .draft_data(chat_id)
            .map(|draft| draft.input.value()[start + 1..draft.input.cursor()].to_owned())
            .unwrap_or_default();
        let active = self
            .completion
            .as_ref()
            .is_some_and(|popup| popup.trigger == trigger && popup.start == start);
        if active
            && self
                .completion
                .as_ref()
                .is_some_and(|popup| popup.dismissed)
        {
            return commands;
        }
        let candidates = self.completion_candidates(chat_id, trigger, &query);
        if active {
            let popup = self.completion.as_mut().expect("active popup");
            popup.candidates = candidates;
            if popup
                .selected
                .is_some_and(|index| index >= popup.candidates.len())
            {
                popup.selected = None;
            }
        } else {
            self.completion = Some(Popup {
                trigger,
                start,
                candidates,
                selected: None,
                dismissed: false,
            });
        }
        commands
    }

    /// Cycle the snapshot and apply the highlighted row in place, like
    /// `complete_command`. No trailing space is added so the token remains
    /// active and further cycling keeps working.
    pub(super) fn move_completion(&mut self, forward: bool) {
        let Some(chat_id) = self.active_chat_id else {
            return;
        };
        let Some(popup) = &mut self.completion else {
            return;
        };
        if popup.dismissed || popup.candidates.is_empty() {
            return;
        }
        let count = popup.candidates.len();
        let index = popup
            .selected
            .map_or(if forward { 0 } else { count - 1 }, |index| {
                if forward {
                    (index + 1) % count
                } else {
                    (index + count - 1) % count
                }
            });
        let insert = popup.candidates[index].insert.clone();
        let start = popup.start;
        popup.selected = Some(index);
        self.apply_completion(chat_id, start, &insert, false);
    }

    /// Accept the highlighted row, replacing the token with it plus a
    /// trailing space. Returns whether a popup row was consumed.
    pub(super) fn accept_completion(&mut self) -> bool {
        let Some(chat_id) = self.active_chat_id else {
            return false;
        };
        if !self.completion_visible() {
            return false;
        }
        let popup = self.completion.take().expect("visible popup");
        let index = popup.selected.unwrap_or(0).min(popup.candidates.len() - 1);
        let insert = popup.candidates[index].insert.clone();
        self.apply_completion(chat_id, popup.start, &insert, true);
        true
    }

    /// Hide the popup while it is visible; the same token stays dismissed.
    pub(super) fn dismiss_completion(&mut self) -> bool {
        if !self.completion_visible() {
            return false;
        }
        self.completion.as_mut().expect("visible popup").dismissed = true;
        true
    }

    /// Pointer activation accepts the clicked row immediately.
    pub(super) fn click_completion(&mut self, index: usize) {
        let Some(chat_id) = self.active_chat_id else {
            return;
        };
        let Some(popup) = &mut self.completion else {
            return;
        };
        if popup.dismissed || index >= popup.candidates.len() {
            return;
        }
        let insert = popup.candidates[index].insert.clone();
        let start = popup.start;
        self.completion = None;
        self.apply_completion(chat_id, start, &insert, true);
    }

    /// Replace the token `start..end` with `insert`, optionally adding the
    /// trailing space. `end` covers the rest of the word after the cursor, so
    /// accepting with the cursor inside a token never mangles its tail.
    fn apply_completion(&mut self, chat_id: ChatId, start: usize, insert: &str, space: bool) {
        let input = &mut self.draft_data_mut(chat_id).input;
        let mut end = input.cursor();
        for c in input.value()[end..].chars() {
            if c.is_alphanumeric() || c == '_' {
                end += c.len_utf8();
            } else {
                break;
            }
        }
        let mut value = input.value().to_owned();
        let tail = if space { " " } else { "" };
        value.replace_range(start..end, &format!("{insert}{tail}"));
        *input = crate::input::TextInput::with_cursor(value, start + insert.len() + tail.len());
    }
}
