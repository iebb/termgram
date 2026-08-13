//! Events exchanged between the terminal, application, and Telegram worker.
//!
//! This module intentionally contains no Telegram SDK types. The network task
//! translates SDK updates into these small, testable domain events.

use std::fmt;
use std::path::PathBuf;

use crossterm::event::{KeyEvent, MouseEvent};

use crate::model::{Chat, ChatId, Message};

#[derive(Clone, Eq, PartialEq)]
pub enum AuthPrompt {
    Phone,
    /// A short-lived Telegram login URL to render locally as a QR code.
    ///
    /// The URL embeds a login token and must never be written to logs or
    /// persisted. A later prompt replaces it when Telegram rotates the token.
    Qr {
        url: String,
    },
    Code {
        phone: String,
    },
    Password {
        hint: Option<String>,
    },
}

impl fmt::Debug for AuthPrompt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Phone => formatter.write_str("Phone"),
            Self::Qr { .. } => formatter
                .debug_struct("Qr")
                .field("url", &"<redacted>")
                .finish(),
            Self::Code { .. } => formatter
                .debug_struct("Code")
                .field("phone", &"<redacted>")
                .finish(),
            Self::Password { hint } => formatter
                .debug_struct("Password")
                .field("hint", hint)
                .finish(),
        }
    }
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
#[derive(Clone, Eq, PartialEq)]
pub enum TelegramCommand {
    /// Start a short-lived QR-code login flow from the phone prompt.
    StartQrAuth,
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
    /// Activate a flattened inline-keyboard button. The worker refetches the
    /// current markup so callback data never enters the UI model or logs.
    ActivateButton {
        chat_id: ChatId,
        message_id: i32,
        button_index: u16,
    },
    MarkRead {
        chat_id: ChatId,
    },
    RefreshDialogs,
    /// Select a different local session slot. The runtime intercepts this
    /// command and replaces the single active Telegram worker.
    SwitchAccount {
        account: u8,
    },
    Shutdown,
}

impl fmt::Debug for TelegramCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StartQrAuth => formatter.write_str("StartQrAuth"),
            Self::SubmitPhone(_) => formatter
                .debug_tuple("SubmitPhone")
                .field(&"<redacted>")
                .finish(),
            Self::SubmitCode(_) => formatter
                .debug_tuple("SubmitCode")
                .field(&"<redacted>")
                .finish(),
            Self::SubmitPassword(_) => formatter
                .debug_tuple("SubmitPassword")
                .field(&"<redacted>")
                .finish(),
            Self::RestartAuth => formatter.write_str("RestartAuth"),
            Self::LoadHistory {
                chat_id,
                request_id,
            } => formatter
                .debug_struct("LoadHistory")
                .field("chat_id", chat_id)
                .field("request_id", request_id)
                .finish(),
            Self::LoadMessage {
                chat_id,
                source_message_id,
                message_id,
                request_id,
            } => formatter
                .debug_struct("LoadMessage")
                .field("chat_id", chat_id)
                .field("source_message_id", source_message_id)
                .field("message_id", message_id)
                .field("request_id", request_id)
                .finish(),
            Self::SendMessage {
                chat_id,
                local_id,
                text,
            } => formatter
                .debug_struct("SendMessage")
                .field("chat_id", chat_id)
                .field("local_id", local_id)
                .field("text", text)
                .finish(),
            Self::SendAttachment {
                chat_id,
                local_id,
                path,
                caption,
                as_photo,
            } => formatter
                .debug_struct("SendAttachment")
                .field("chat_id", chat_id)
                .field("local_id", local_id)
                .field("path", path)
                .field("caption", caption)
                .field("as_photo", as_photo)
                .finish(),
            Self::DownloadAttachment {
                chat_id,
                message_id,
            } => formatter
                .debug_struct("DownloadAttachment")
                .field("chat_id", chat_id)
                .field("message_id", message_id)
                .finish(),
            Self::ResolveTelegramLink { url } => formatter
                .debug_struct("ResolveTelegramLink")
                .field("url", url)
                .finish(),
            Self::ActivateButton {
                chat_id,
                message_id,
                button_index,
            } => formatter
                .debug_struct("ActivateButton")
                .field("chat_id", chat_id)
                .field("message_id", message_id)
                .field("button_index", button_index)
                .finish(),
            Self::MarkRead { chat_id } => formatter
                .debug_struct("MarkRead")
                .field("chat_id", chat_id)
                .finish(),
            Self::RefreshDialogs => formatter.write_str("RefreshDialogs"),
            Self::SwitchAccount { account } => formatter
                .debug_struct("SwitchAccount")
                .field("account", account)
                .finish(),
            Self::Shutdown => formatter.write_str("Shutdown"),
        }
    }
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
    ButtonActivated {
        chat_id: ChatId,
        message_id: i32,
        message: Option<String>,
        url: Option<String>,
    },
    ButtonFailed {
        chat_id: ChatId,
        message_id: i32,
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

#[cfg(test)]
mod tests {
    use super::{AuthPrompt, TelegramCommand};

    #[test]
    fn debug_output_redacts_authentication_credentials() {
        let qr_secret = "tg://login?token=top-secret";
        let qr = format!(
            "{:?}",
            AuthPrompt::Qr {
                url: qr_secret.to_owned(),
            }
        );
        assert!(!qr.contains(qr_secret));
        assert!(qr.contains("redacted"));

        let phone_secret = "+15551234";
        let code_prompt = format!(
            "{:?}",
            AuthPrompt::Code {
                phone: phone_secret.to_owned(),
            }
        );
        assert!(!code_prompt.contains(phone_secret));
        assert!(code_prompt.contains("redacted"));

        for (command, secret) in [
            (
                TelegramCommand::SubmitPhone("+15551234".to_owned()),
                "+15551234",
            ),
            (TelegramCommand::SubmitCode("12345".to_owned()), "12345"),
            (
                TelegramCommand::SubmitPassword("correct horse".to_owned()),
                "correct horse",
            ),
        ] {
            let debug = format!("{command:?}");
            assert!(!debug.contains(secret));
            assert!(debug.contains("redacted"));
        }
    }
}
