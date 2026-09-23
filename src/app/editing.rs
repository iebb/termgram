use super::{App, Focus, KeyAction, Mode, Screen, TelegramCommand, TextInput};
use crate::{
    actions::Action,
    drafts::Key,
    editing::{Draft, Source},
    event::ConnectionStatus,
    model::sanitize_terminal_line,
};

#[derive(Clone, Default)]
pub struct State {
    pending: Option<(Key, u64, bool)>,
    next_request: u64,
}

impl App {
    #[must_use]
    pub fn message_edit(&self) -> Option<&Draft> {
        self.active_chat_id
            .and_then(|chat| self.draft_data(chat))?
            .edit
            .as_ref()
    }

    #[must_use]
    pub fn message_edit_pending(&self) -> bool {
        self.editing.pending.is_some()
    }

    fn begin_message_edit(&mut self) -> Vec<TelegramCommand> {
        if self.screen != Screen::Main
            || self.mode != Mode::Navigate
            || self.focus != Focus::Conversation
        {
            return Vec::new();
        }
        let Some(chat_id) = self.active_chat_id else {
            return Vec::new();
        };
        if let Some(edit) = self.message_edit() {
            let selected = self.selected_message.filter(|id| *id > 0);
            let resume = selected.is_none_or(|id| id == edit.source.message_id);
            let modified = edit.input.value() != edit.source.text;
            if resume {
                self.mode = Mode::Edit;
                self.status_message = None;
                return Vec::new();
            }
            if modified {
                self.status_message = Some(
                    "An edit is kept for another message · select it to resume or discard it"
                        .to_owned(),
                );
                return Vec::new();
            }
            // The kept edit was never modified, so loading the selected
            // message's original may replace it below.
        }
        let Some(message_id) = self.selected_message.filter(|id| *id > 0) else {
            self.status_message = Some("Select a delivered message to edit".to_owned());
            return Vec::new();
        };
        if self.editing.pending.is_some() {
            return Vec::new();
        }
        if self.connection != ConnectionStatus::Online {
            self.status_message =
                Some("Connect to Telegram to load the original message".to_owned());
            return Vec::new();
        }
        self.editing.next_request += 1;
        let request_id = self.editing.next_request;
        self.editing.pending = Some((self.draft_key(chat_id), request_id, false));
        self.mode = Mode::Edit;
        self.status_message = Some("Loading the original message…".to_owned());
        vec![TelegramCommand::LoadEdit {
            chat_id,
            message_id,
            request_id,
        }]
    }

    pub(super) fn edit_loaded(
        &mut self,
        chat_id: i64,
        request_id: u64,
        result: Result<Source, String>,
    ) {
        let Some((key, id, false)) = self.editing.pending else {
            return;
        };
        if key != self.draft_key(chat_id) || id != request_id {
            return;
        }
        self.editing.pending = None;
        match result {
            Ok(source) => {
                let input = TextInput::from_value(&source.text);
                self.draft_at_mut(key).edit = Some(Draft { source, input });
                self.status_message = None;
            }
            Err(error) => {
                self.status_message = Some(sanitize_terminal_line(&error));
                if self.mode == Mode::Edit {
                    self.mode = Mode::Navigate;
                }
            }
        }
    }

    fn save_message_edit(&mut self) -> Vec<TelegramCommand> {
        if self.editing.pending.is_some() {
            return Vec::new();
        }
        let Some(chat_id) = self.active_chat_id else {
            return Vec::new();
        };
        let Some(edit) = self.message_edit() else {
            return Vec::new();
        };
        if edit.input.value() == edit.source.text {
            self.draft_data_mut(chat_id).edit = None;
            self.mode = Mode::Navigate;
            self.status_message = Some("Message unchanged".to_owned());
            return Vec::new();
        }
        if !edit.source.caption && edit.input.value().trim().is_empty() {
            self.status_message =
                Some("A text message cannot be empty; use :delete to remove it".to_owned());
            return Vec::new();
        }
        if self.connection != ConnectionStatus::Online {
            self.status_message =
                Some("Connect to Telegram to save; your edit is kept locally".to_owned());
            return Vec::new();
        }
        let (message_id, revision, text) = (
            edit.source.message_id,
            edit.source.revision,
            edit.input.value().to_owned(),
        );
        self.editing.next_request += 1;
        let request_id = self.editing.next_request;
        self.editing.pending = Some((self.draft_key(chat_id), request_id, true));
        self.status_message = Some("Saving message edit…".to_owned());
        vec![TelegramCommand::EditMessage {
            chat_id,
            message_id,
            request_id,
            revision,
            text,
        }]
    }

    pub(super) fn edit_finished(&mut self, chat_id: i64, request_id: u64, error: Option<String>) {
        let Some((key, id, true)) = self.editing.pending else {
            return;
        };
        if key != self.draft_key(chat_id) || id != request_id {
            return;
        }
        self.editing.pending = None;
        if let Some(error) = error {
            self.status_message = Some(format!(
                "{} · edit kept locally",
                sanitize_terminal_line(&error)
            ));
        } else {
            self.draft_at_mut(key).edit = None;
            if self.active_chat_id == Some(chat_id) && self.mode == Mode::Edit {
                self.mode = Mode::Navigate;
            }
            self.status_message = Some("Message edited".to_owned());
        }
    }

    pub(super) fn editing_binding(&mut self, action: &Action) -> Option<Vec<TelegramCommand>> {
        if *action == Action::EditMessage {
            return Some(self.begin_message_edit());
        }
        if *action == Action::DiscardEdit && matches!(self.mode, Mode::Navigate | Mode::Edit) {
            if self.editing.pending.is_some() {
                self.status_message = Some("Wait for the pending edit to finish".to_owned());
            } else if let Some(chat) = self.active_chat_id {
                self.draft_data_mut(chat).edit = None;
                self.mode = Mode::Navigate;
                self.status_message = Some("Local edit discarded".to_owned());
            }
            return Some(Vec::new());
        }
        if self.mode != Mode::Edit {
            return None;
        }
        match action {
            Action::Open | Action::Send => Some(self.save_message_edit()),
            Action::Cancel => Some(self.edit_message_input(KeyAction::Escape)),
            Action::Quit
            | Action::NextAccount
            | Action::AddAccount
            | Action::Redraw
            | Action::Newline
            | Action::Left
            | Action::Right
            | Action::Home
            | Action::End
            | Action::Backspace
            | Action::Delete
            | Action::Clear
            | Action::DeleteWord => None,
            _ => Some(Vec::new()),
        }
    }

    pub(super) fn edit_message_input(&mut self, action: KeyAction) -> Vec<TelegramCommand> {
        if action == KeyAction::Escape {
            if self.editing.pending.is_some_and(|(_, _, saving)| !saving) {
                self.editing.pending = None;
            }
            self.mode = Mode::Navigate;
            self.status_message = self
                .message_edit()
                .map(|_| "Edit kept locally · e or :edit to resume".to_owned());
            return Vec::new();
        }
        if action == KeyAction::Enter {
            return self.save_message_edit();
        }
        if action == KeyAction::Redraw {
            self.force_redraw = true;
        }
        if self.editing.pending.is_some() {
            return Vec::new();
        }
        let Some(chat) = self.active_chat_id else {
            return Vec::new();
        };
        let Some(edit) = &mut self.draft_data_mut(chat).edit else {
            return Vec::new();
        };
        let input = &mut edit.input;
        match action {
            KeyAction::Character(c) => input.insert(c),
            KeyAction::Newline => input.insert('\n'),
            KeyAction::Backspace => _ = input.backspace(),
            KeyAction::Delete => _ = input.delete(),
            KeyAction::Left => _ = input.move_left(),
            KeyAction::Right => _ = input.move_right(),
            KeyAction::Home => input.move_home(),
            KeyAction::End => input.move_end(),
            KeyAction::Clear => input.clear(),
            KeyAction::DeleteWord => _ = input.delete_word_before(),
            _ => {}
        }
        Vec::new()
    }
}
