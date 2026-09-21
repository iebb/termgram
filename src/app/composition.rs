//! Shared keyboard and pointer transitions into a per-chat draft.
use std::collections::BTreeMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{App, ChatId, Focus, Mode, TelegramCommand};
use crate::config::{read_preferences, write_preferences};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Navigation {
    /// Telegram user IDs, not local account-slot indices.
    accounts: BTreeMap<i64, ChatId>,
}

impl App {
    /// Resolve explicit navigation through the current folder ordering, rather
    /// than treating a raw chat index as a filtered position.
    pub(super) fn open_chat_by_id(&mut self, id: ChatId) -> Vec<TelegramCommand> {
        let Some(chat) = self.chats.iter().find(|chat| chat.id == id) else {
            return Vec::new();
        };
        self.filter.clear();
        self.folder_id = i32::from(chat.membership.archived);
        if self.folder_id == 1 && !self.folders.iter().any(|folder| folder.id == 1) {
            self.folders.push(crate::folders::Folder::archive());
        }
        self.mode = Mode::Navigate;
        self.preserve_chat_selection(Some(id));
        self.open_selected_chat()
    }

    /// Load navigation preferences without replacing malformed files.
    ///
    /// # Errors
    /// Returns read or format errors from the managed preferences file.
    pub fn load_navigation(&mut self) -> Result<()> {
        if let Some(path) = &self.settings_path {
            self.navigation = read_preferences(&path.with_file_name("navigation.json"))?;
        }
        Ok(())
    }

    pub(super) fn remember_chat(&mut self, chat_id: ChatId) {
        let Some(account) = self.account_user_id else {
            return;
        };
        if self.navigation.accounts.insert(account, chat_id) == Some(chat_id) {
            return;
        }
        let result = (|| -> Result<()> {
            let Some(path) = &self.settings_path else {
                return Ok(());
            };
            let path = path.with_file_name("navigation.json");
            let mut preferences: Navigation = read_preferences(&path)?;
            preferences.accounts.insert(account, chat_id);
            write_preferences(&path, &serde_json::to_vec_pretty(&preferences)?)?;
            self.navigation = preferences;
            Ok(())
        })();
        if let Err(error) = result {
            self.status_message = Some(format!("Could not remember this chat: {error:#}"));
        }
    }

    pub(super) fn compose_or_reply(&mut self) -> Vec<TelegramCommand> {
        if self.focus == Focus::Conversation && self.selected_message.is_some() {
            self.start_replying_to_selected()
        } else {
            self.start_composing()
        }
    }

    pub(super) fn start_composing(&mut self) -> Vec<TelegramCommand> {
        self.pending_telegram_link = None;
        let mut commands = Vec::new();
        if self.active_chat_id.is_none() {
            let remembered = self
                .account_user_id
                .and_then(|account| self.navigation.accounts.get(&account))
                .copied()
                .filter(|id| self.chats.iter().any(|chat| chat.id == *id));
            if let Some(id) = remembered {
                self.filter.clear();
                self.folder_id = self
                    .chats
                    .iter()
                    .find(|chat| chat.id == id)
                    .map_or(0, |chat| i32::from(chat.membership.archived));
                if self.folder_id == 1 && !self.folders.iter().any(|folder| folder.id == 1) {
                    self.folders.push(crate::folders::Folder::archive());
                }
                self.preserve_chat_selection(Some(id));
            }
            commands.extend(self.open_selected_chat());
        }
        let Some(chat_id) = self.active_chat_id else {
            self.status_message =
                Some("No conversation available · open a chat after synchronization".to_owned());
            return commands;
        };
        let key = self.draft_key(chat_id);
        self.drafts.entry(key).or_default();
        self.mode = Mode::Compose;
        self.focus = Focus::Conversation;
        self.narrow_conversation = true;
        self.selected_message = None;
        // Warm the member cache so `@` and `/` complete on the first keystroke.
        commands.extend(self.request_completion_data(chat_id));
        commands.extend(self.refresh_completion(chat_id));
        commands
    }
}
