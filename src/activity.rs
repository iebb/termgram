//! Other people's transient chat actions: typing, recording, uploading, …
//!
//! Lifetimes, cancellation and the one/two/many typing forms follow Telegram
//! Desktop's `history/view/history_view_send_action.cpp`; no source is copied.

use crate::model::ChatId;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// Chats tracked at once; the entry closest to expiry is dropped first.
const MAX_CHATS: usize = 64;
/// Distinct senders remembered per chat; larger groups show a count anyway.
const MAX_SENDERS: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Typing,
    RecordVideo,
    UploadVideo,
    RecordVoice,
    UploadVoice,
    RecordRound,
    UploadRound,
    UploadPhoto,
    UploadFile,
    ChooseLocation,
    ChooseContact,
    ChooseSticker,
    PlayGame,
    Speaking,
}

impl Action {
    /// Telegram repeats an ongoing action; without a repeat it lapses.
    const fn lifetime(self) -> Duration {
        Duration::from_secs(if matches!(self, Self::PlayGame) {
            10
        } else {
            6
        })
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Typing => "typing",
            Self::RecordVideo => "recording a video",
            Self::UploadVideo => "sending a video",
            Self::RecordVoice => "recording a voice message",
            Self::UploadVoice => "sending a voice message",
            Self::RecordRound => "recording a video message",
            Self::UploadRound => "sending a video message",
            Self::UploadPhoto => "sending a photo",
            Self::UploadFile => "sending a file",
            Self::ChooseLocation => "picking a location",
            Self::ChooseContact => "choosing a contact",
            Self::ChooseSticker => "choosing a sticker",
            Self::PlayGame => "playing a game",
            Self::Speaking => "speaking",
        }
    }
}

#[derive(Clone, Debug)]
struct Entry {
    sender_id: i64,
    name: String,
    action: Action,
    until: Instant,
}

#[derive(Clone, Debug, Default)]
pub struct State {
    chats: HashMap<ChatId, Vec<Entry>>,
}

impl State {
    /// `None` is Telegram's explicit cancel action.
    pub fn apply(
        &mut self,
        chat_id: ChatId,
        sender_id: i64,
        name: String,
        action: Option<Action>,
        now: Instant,
    ) {
        self.expire(now);
        let Some(action) = action else {
            self.remove(chat_id, |entry| entry.sender_id == sender_id);
            return;
        };
        if !self.chats.contains_key(&chat_id) && self.chats.len() >= MAX_CHATS {
            let oldest = self
                .chats
                .iter()
                .filter_map(|(id, entries)| Some((*id, entries.iter().map(|e| e.until).max()?)))
                .min_by_key(|(_, until)| *until)
                .map(|(id, _)| id);
            if let Some(oldest) = oldest {
                self.chats.remove(&oldest);
            }
        }
        let entries = self.chats.entry(chat_id).or_default();
        entries.retain(|entry| entry.sender_id != sender_id);
        if entries.len() >= MAX_SENDERS {
            entries.remove(0);
        }
        entries.push(Entry {
            sender_id,
            name,
            action,
            until: now + action.lifetime(),
        });
    }

    /// A delivered message ends its author's action before the timeout.
    pub fn message_arrived(&mut self, chat_id: ChatId, sender: &str) {
        self.remove(chat_id, |entry| entry.name == sender);
    }

    fn remove(&mut self, chat_id: ChatId, matches: impl Fn(&Entry) -> bool) {
        if let Some(entries) = self.chats.get_mut(&chat_id) {
            entries.retain(|entry| !matches(entry));
            if entries.is_empty() {
                self.chats.remove(&chat_id);
            }
        }
    }

    pub fn expire(&mut self, now: Instant) {
        self.chats.retain(|_, entries| {
            entries.retain(|entry| entry.until > now);
            !entries.is_empty()
        });
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.chats.is_empty()
    }

    /// One line for `chat_id`, most recent action first when they differ.
    #[must_use]
    pub fn label(&self, chat_id: ChatId) -> Option<String> {
        let entries = self.chats.get(&chat_id)?;
        let latest = entries.last()?;
        let same = entries
            .iter()
            .filter(|entry| entry.action == latest.action)
            .collect::<Vec<_>>();
        let action = latest.action.label();
        let text = match same.as_slice() {
            [one] => format!("{} is {action}…", one.name),
            [first, second] => format!("{} and {} are {action}…", first.name, second.name),
            many => format!("{} people are {action}…", many.len()),
        };
        let others = entries.len() - same.len();
        Some(if others == 0 {
            text
        } else {
            format!("{text} +{others}")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, State};
    use std::time::{Duration, Instant};

    #[test]
    fn actions_combine_cancel_and_expire() {
        let now = Instant::now();
        let mut state = State::default();
        state.apply(7, 1, "Ada".to_owned(), Some(Action::Typing), now);
        assert_eq!(state.label(7).as_deref(), Some("Ada is typing…"));
        state.apply(7, 2, "Bob".to_owned(), Some(Action::Typing), now);
        assert_eq!(state.label(7).as_deref(), Some("Ada and Bob are typing…"));
        state.apply(7, 2, "Bob".to_owned(), Some(Action::UploadPhoto), now);
        assert_eq!(
            state.label(7).as_deref(),
            Some("Bob is sending a photo… +1")
        );
        state.apply(7, 2, "Bob".to_owned(), None, now);
        state.message_arrived(7, "Nobody");
        assert_eq!(state.label(7).as_deref(), Some("Ada is typing…"));
        state.message_arrived(7, "Ada");
        assert!(state.is_empty());

        state.apply(7, 1, "Ada".to_owned(), Some(Action::Typing), now);
        state.expire(now + Duration::from_secs(6));
        assert!(state.label(7).is_none());
    }
}
