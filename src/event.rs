//! Events exchanged between the terminal, application, and Telegram worker.
//!
//! This module intentionally contains no Telegram SDK types. The network task
//! translates SDK updates into these small, testable domain events.

use std::path::PathBuf;

use crossterm::event::{KeyEvent, MouseEvent};

use crate::model::{Chat, ChatId, Message};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthPrompt {
    Phone,
    Code { phone: String },
    Password { hint: Option<String> },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ConnectionStatus {
    #[default]
    Connecting,
    Online,
    Reconnecting,
    Offline,
}

/// Commands sent from the application to the Telegram worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TelegramCommand {
    SubmitPhone(String),
    SubmitCode(String),
    SubmitPassword(String),
    /// Abandon the current login token and request a fresh phone/code flow.
    RestartAuth,
    LoadHistory {
        chat_id: ChatId,
        request_id: u64,
    },
    /// Fetch one message for reply navigation when it is outside the bounded
    /// in-memory history window.
    LoadMessage {
        chat_id: ChatId,
        source_message_id: i32,
        message_id: i32,
        request_id: u64,
    },
    SendMessage {
        chat_id: ChatId,
        local_id: i32,
        text: String,
    },
    /// Upload a local path and send it as Telegram media.
    SendAttachment {
        chat_id: ChatId,
        local_id: i32,
        path: PathBuf,
        caption: String,
        /// Compress image input as a Telegram photo; otherwise preserve it as
        /// a document.
        as_photo: bool,
    },
    /// Lazily download media from a known message into Termgram's temporary
    /// download directory.
    DownloadAttachment {
        chat_id: ChatId,
        message_id: i32,
    },
    /// Resolve a Telegram public/private message URL to an in-app target.
    ResolveTelegramLink {
        url: String,
    },
    MarkRead {
        chat_id: ChatId,
    },
    RefreshDialogs,
    Shutdown,
}

/// SDK-independent updates sent from the Telegram worker to the application.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetworkEvent {
    Auth(AuthPrompt),
    Ready {
        user_name: String,
    },
    Dialogs(Vec<Chat>),
    History {
        chat_id: ChatId,
        request_id: u64,
        messages: Vec<Message>,
    },
    HistoryFailed {
        chat_id: ChatId,
        request_id: u64,
        error: String,
    },
    MessageLoaded {
        chat_id: ChatId,
        message_id: i32,
        request_id: u64,
        message: Message,
    },
    MessageLoadFailed {
        chat_id: ChatId,
        message_id: i32,
        request_id: u64,
        error: String,
    },
    DialogsFailed(String),
    NewMessage(Message),
    /// A replayed or edited message that must not change unread state.
    MessageUpdated(Message),
    MessageSent {
        local_id: i32,
        message: Message,
    },
    /// Telegram accepted a send but did not return the final message object.
    /// The matching live update will reconcile the optimistic entry later.
    MessageAccepted {
        chat_id: ChatId,
        local_id: i32,
    },
    ReadMarked {
        chat_id: ChatId,
    },
    ReadMarkFailed {
        chat_id: ChatId,
        error: String,
    },
    MessagesRead {
        chat_id: ChatId,
        max_id: i32,
    },
    SendFailed {
        chat_id: ChatId,
        local_id: i32,
        text: String,
        error: String,
    },
    /// An attachment send failed. Kept distinct from text failures so the UI
    /// never retries a display label as a text message.
    AttachmentSendFailed {
        chat_id: ChatId,
        local_id: i32,
        path: PathBuf,
        caption: String,
        as_photo: bool,
        error: String,
    },
    AttachmentDownloaded {
        chat_id: ChatId,
        message_id: i32,
        path: PathBuf,
    },
    AttachmentDownloadFailed {
        chat_id: ChatId,
        message_id: i32,
        error: String,
    },
    LinkResolved {
        chat: Chat,
        message: Option<Message>,
    },
    LinkFailed {
        url: String,
        error: String,
    },
    Status(ConnectionStatus),
    Error(String),
    Fatal(String),
}

/// Inputs consumed by [`crate::app::App::update`].
// Keeping the network value inline avoids heap allocation on every Telegram
// update; the larger link-result variant is rare and the event is short-lived.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Network(NetworkEvent),
    Paste(String),
    TerminalFocus(bool),
    Tick,
}
