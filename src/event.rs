//! Events exchanged between the terminal, application, and Telegram worker.
//!
//! This module intentionally contains no Telegram SDK types. The network task
//! translates SDK updates into these small, testable domain events.

use std::fmt;
use std::path::PathBuf;

use yazi_term::event::{KeyEvent, MouseEvent};

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
    LoadChatInfo {
        chat_id: ChatId,
        request_id: u64,
    },
    /// One-shot member/bot-command fetch backing composer completion.
    LoadMembers {
        chat_id: ChatId,
        request_id: u64,
    },
    PreviewInvite {
        hash: String,
        request_id: u64,
    },
    JoinInvite {
        hash: String,
        title: String,
        request_needed: bool,
        request_id: u64,
    },
    LoadReactions {
        chat_id: ChatId,
        message_id: i32,
        request_id: u64,
    },
    ChangeReaction {
        chat_id: ChatId,
        message_id: i32,
        request_id: u64,
        expected: Vec<crate::reactions::Kind>,
        emoji: Option<String>,
    },
    RefreshReactions {
        chat_id: ChatId,
        message_ids: Vec<i32>,
        request_id: u64,
    },
    LoadPoll {
        chat_id: ChatId,
        message_id: i32,
        request_id: u64,
    },
    RefreshPoll {
        chat_id: ChatId,
        message_id: i32,
        hash: i64,
        request_id: u64,
    },
    VotePoll {
        chat_id: ChatId,
        message_id: i32,
        poll_id: i64,
        revision: u64,
        options: Vec<Vec<u8>>,
        request_id: u64,
    },
    /// Start a short-lived QR-code login flow from the phone prompt.
    StartQrAuth,
    SubmitPhone(String),
    SubmitCode(String),
    SubmitPassword(String),
    /// Abandon the current login token and request a fresh phone/code flow.
    RestartAuth,
    LoadOlder {
        chat_id: ChatId,
        request_id: u64,
        before_id: i32,
    },
    LoadHistory {
        chat_id: ChatId,
        request_id: u64,
        /// Oldest-to-newest page strictly after this incoming read boundary.
        after_id: Option<i32>,
    },
    /// Fetch one message for reply navigation when it is outside the bounded
    /// in-memory history window.
    LoadMessage {
        chat_id: ChatId,
        source_message_id: i32,
        message_id: i32,
        request_id: u64,
    },
    /// Resolve the visible replies in one request per target conversation.
    LoadReplyPreviews {
        chat_id: ChatId,
        message_ids: Vec<i32>,
        request_id: u64,
    },
    SendMessage {
        chat_id: ChatId,
        local_id: i32,
        text: String,
        /// Positive message identifier in this conversation when composing a
        /// native Telegram reply.
        reply_to: Option<i32>,
    },
    OpenSaved {
        request_id: u64,
    },
    ReviewForward {
        chat_id: ChatId,
        message_id: i32,
        destination: ChatId,
        request_id: u64,
    },
    ForwardMessage {
        chat_id: ChatId,
        message_id: i32,
        destination: ChatId,
        revision: [u8; 32],
        random_id: i64,
        request_id: u64,
    },
    CopyText(String),
    CopyMessage {
        chat_id: ChatId,
        message_id: i32,
        link: bool,
        request_id: u64,
    },
    ReviewDeletion {
        chat_id: ChatId,
        message_id: i32,
        request_id: u64,
    },
    DeleteMessage {
        chat_id: ChatId,
        message_id: i32,
        request_id: u64,
        revision: [u8; 32],
        scope: crate::deletion::Scope,
    },
    LoadEdit {
        chat_id: ChatId,
        message_id: i32,
        request_id: u64,
    },
    EditMessage {
        chat_id: ChatId,
        message_id: i32,
        request_id: u64,
        revision: [u8; 32],
        text: String,
    },
    PrepareAttachments(crate::staging::Request),
    /// Upload a reviewed local file and send it as Telegram media.
    SendAttachment {
        chat_id: ChatId,
        local_id: i32,
        attachment: crate::staging::Attachment,
        caption: String,
        /// Positive message identifier in this conversation when the media is
        /// sent as a reply.
        reply_to: Option<i32>,
    },
    /// Lazily download media from a known message into Termgram's managed
    /// media cache.
    DownloadAttachment {
        request_id: u64,
        media_id: Option<i64>,
        chat_id: ChatId,
        message_id: i32,
    },
    /// Download a raster image for a specific preview request. Animated stickers
    /// request Telegram's static thumbnail instead of the original animation.
    DownloadPreview {
        chat_id: ChatId,
        message_id: i32,
        request_id: u64,
        thumbnail: bool,
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
        max_id: i32,
    },
    ReadMentions {
        chat_id: ChatId,
        message_ids: Vec<i32>,
    },
    SetChatUnread {
        chat_id: ChatId,
        unread: bool,
        read_history: bool,
        request_id: u64,
    },
    RefreshDialogs,
    ResolveAlertSettings {
        key: crate::notifications::SettingsKey,
        request_id: u64,
    },
    SetChatMute {
        chat_id: ChatId,
        mute: crate::notifications::Mute,
        request_id: u64,
    },
    RefreshDialogPins,
    LoadPinnedMessages {
        chat_id: ChatId,
        before: i32,
        request_id: u64,
    },
    LoadPinnedContext {
        chat_id: ChatId,
        message_id: i32,
        request_id: u64,
    },
    ChangeMessagePin {
        chat_id: ChatId,
        message_id: i32,
        action: crate::pins::MessageAction,
        request_id: u64,
    },
    SetArchived {
        chat_id: ChatId,
        archived: bool,
        request_id: u64,
    },
    ChangeDialogPin {
        chat_id: ChatId,
        scope: crate::pins::DialogScope,
        action: crate::pins::DialogAction,
        request_id: u64,
    },
    RefreshFolders,
    SearchCached(crate::search::Request),
    SearchCloud(crate::cloud_search::Request),
    SearchMentions {
        chat_id: ChatId,
        before_id: i32,
        request_id: u64,
    },
    CancelSearch,
    LoadCloudContext {
        chat_id: ChatId,
        message_id: i32,
        request_id: u64,
    },
    LoadCachedContext {
        chat_id: ChatId,
        message_id: i32,
        request_id: u64,
    },
    /// Select a different local session slot. The runtime intercepts this
    /// command and replaces the single active Telegram worker.
    SwitchAccount {
        account: u8,
    },
    Shutdown,
}

impl TelegramCommand {
    pub(crate) fn cloud_search_id(&self) -> Option<u64> {
        match self {
            Self::SearchCloud(request) => Some(request.id),
            Self::LoadCloudContext { request_id, .. } | Self::SearchMentions { request_id, .. } => {
                Some(*request_id)
            }
            _ => None,
        }
    }

    /// Return the matching failure event so pending UI operations always settle.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn failure(self, error: String) -> Option<NetworkEvent> {
        let event = match self {
            Self::LoadChatInfo {
                chat_id,
                request_id,
            } => NetworkEvent::ChatInfoReady {
                chat_id,
                request_id,
                result: Err(error),
            },
            Self::LoadMembers {
                chat_id,
                request_id,
            } => NetworkEvent::MembersLoaded {
                chat_id,
                request_id,
                result: Err(error),
            },
            Self::PreviewInvite { request_id, .. } => NetworkEvent::InviteReady {
                request_id,
                result: Err(error),
            },
            Self::JoinInvite { request_id, .. } => NetworkEvent::InviteJoined {
                request_id,
                result: Err(error),
            },
            Self::LoadReactions {
                chat_id,
                message_id,
                request_id,
            } => NetworkEvent::ReactionsLoaded {
                chat_id,
                message_id,
                request_id,
                result: Err(error),
            },
            Self::ChangeReaction {
                chat_id,
                message_id,
                request_id,
                ..
            } => NetworkEvent::ReactionsFinished {
                chat_id,
                message_id: Some(message_id),
                request_id,
                error: Some(error),
            },
            Self::RefreshReactions {
                chat_id,
                request_id,
                ..
            } => NetworkEvent::ReactionsFinished {
                chat_id,
                message_id: None,
                request_id,
                error: Some(error),
            },
            Self::LoadPoll {
                chat_id,
                message_id,
                request_id,
            } => NetworkEvent::PollLoaded {
                chat_id,
                message_id,
                request_id,
                result: Err(error),
            },
            Self::RefreshPoll {
                chat_id,
                message_id,
                request_id,
                ..
            } => NetworkEvent::PollFinished {
                chat_id,
                message_id,
                request_id,
                voting: false,
                error: Some(error),
            },
            Self::VotePoll {
                chat_id,
                message_id,
                request_id,
                ..
            } => NetworkEvent::PollFinished {
                chat_id,
                message_id,
                request_id,
                voting: true,
                error: Some(error),
            },
            Self::ResolveAlertSettings { key, request_id } => NetworkEvent::AlertSettingsReady {
                key,
                request_id,
                result: Err(error),
            },
            Self::OpenSaved { request_id } => NetworkEvent::SavedReady {
                request_id,
                result: Err(error),
            },
            Self::ReviewForward { request_id, .. } => NetworkEvent::ForwardReady {
                request_id,
                result: Err(error),
            },
            Self::ForwardMessage { request_id, .. } => NetworkEvent::ForwardFinished {
                request_id,
                error: Some(error),
            },

            Self::CopyMessage { request_id, .. } => NetworkEvent::MessageCopyReady {
                request_id,
                result: Err(error),
            },
            Self::ReviewDeletion {
                chat_id,
                message_id,
                request_id,
            } => NetworkEvent::DeletionReady {
                chat_id,
                message_id,
                request_id,
                result: Err(error),
            },
            Self::DeleteMessage {
                chat_id,
                message_id,
                request_id,
                ..
            } => NetworkEvent::DeleteFinished {
                chat_id,
                message_id,
                request_id,
                error: Some(error),
            },
            Self::LoadEdit {
                chat_id,
                request_id,
                ..
            } => NetworkEvent::EditLoaded {
                chat_id,
                request_id,
                result: Err(error),
            },
            Self::EditMessage {
                chat_id,
                request_id,
                ..
            } => NetworkEvent::EditFinished {
                chat_id,
                request_id,
                error: Some(error),
            },
            Self::SetChatUnread {
                chat_id,
                unread,
                request_id,
                ..
            } => NetworkEvent::ChatUnreadFinished {
                chat_id,
                unread,
                request_id,
                snapshot: None,
                error: Some(error),
            },
            Self::LoadPinnedMessages {
                chat_id,
                request_id,
                ..
            }
            | Self::LoadPinnedContext {
                chat_id,
                request_id,
                ..
            } => NetworkEvent::PinnedMessagesFailed {
                chat_id,
                request_id,
                error,
            },
            Self::ChangeMessagePin {
                chat_id,
                request_id,
                ..
            } => NetworkEvent::MessagePinFinished {
                chat_id,
                request_id,
                error: Some(error),
            },
            Self::ChangeDialogPin { request_id, .. } | Self::SetArchived { request_id, .. } => {
                NetworkEvent::DialogPinFinished {
                    request_id,
                    error: Some(error),
                }
            }
            Self::LoadOlder {
                chat_id,
                request_id,
                ..
            } => NetworkEvent::OlderHistoryFailed {
                chat_id,
                request_id,
                error,
            },
            TelegramCommand::PrepareAttachments(request) => NetworkEvent::AttachmentsPrepared {
                key: request.key,
                request_id: request.id,
                result: Err(error),
            },
            TelegramCommand::SendMessage {
                chat_id,
                local_id,
                text,
                reply_to,
            } => NetworkEvent::SendFailed {
                chat_id,
                local_id,
                text,
                reply_to,
                error,
            },
            TelegramCommand::SendAttachment {
                chat_id,
                local_id,
                attachment,
                caption,
                reply_to,
            } => NetworkEvent::AttachmentSendFailed {
                chat_id,
                local_id,
                attachment,
                caption,
                reply_to,
                error,
            },
            TelegramCommand::DownloadAttachment {
                chat_id,
                message_id,
                request_id,
                ..
            } => NetworkEvent::AttachmentDownloadFailed {
                request_id,
                chat_id,
                message_id,
                error,
            },
            TelegramCommand::DownloadPreview {
                chat_id,
                message_id,
                request_id,
                ..
            } => NetworkEvent::PreviewDownloadFailed {
                chat_id,
                message_id,
                request_id,
                error,
            },
            TelegramCommand::ResolveTelegramLink { url } => NetworkEvent::LinkFailed { url, error },
            TelegramCommand::ActivateButton {
                chat_id,
                message_id,
                button_index: _,
            } => NetworkEvent::ButtonFailed {
                chat_id,
                message_id,
                error,
            },
            TelegramCommand::LoadHistory {
                chat_id,
                request_id,
                ..
            } => NetworkEvent::HistoryFailed {
                chat_id,
                request_id,
                error,
            },
            TelegramCommand::LoadMessage {
                chat_id,
                source_message_id: _,
                message_id,
                request_id,
            } => NetworkEvent::MessageLoadFailed {
                chat_id,
                message_id,
                request_id,
                error,
            },
            TelegramCommand::MarkRead { chat_id, max_id } => NetworkEvent::ReadMarkFailed {
                chat_id,
                max_id,
                error,
            },
            Self::ReadMentions {
                chat_id,
                message_ids,
            } => NetworkEvent::MentionsReadFinished {
                chat_id,
                message_ids,
                error: Some(error),
            },
            TelegramCommand::LoadReplyPreviews {
                chat_id,
                request_id,
                ..
            } => NetworkEvent::ReplyPreviewsFailed {
                chat_id,
                request_id,
                error,
            },
            TelegramCommand::SetChatMute {
                chat_id,
                request_id,
                ..
            } => NetworkEvent::ChatMuteFinished {
                chat_id,
                request_id,
                result: Err(error),
            },
            TelegramCommand::SearchCached(request) => NetworkEvent::SearchFailed {
                request_id: request.id,
                error,
            },
            TelegramCommand::SearchCloud(request) => NetworkEvent::SearchFailed {
                request_id: request.id,
                error,
            },
            TelegramCommand::LoadCloudContext { request_id, .. }
            | Self::SearchMentions { request_id, .. }
            | TelegramCommand::LoadCachedContext { request_id, .. } => {
                NetworkEvent::SearchFailed { request_id, error }
            }
            Self::CopyText(_) | TelegramCommand::RefreshFolders | Self::RefreshDialogPins => {
                NetworkEvent::Error(error)
            }
            TelegramCommand::RefreshDialogs => NetworkEvent::DialogsFailed(error),
            TelegramCommand::CancelSearch
            | TelegramCommand::SwitchAccount { .. }
            | TelegramCommand::Shutdown => return None,
            TelegramCommand::StartQrAuth
            | TelegramCommand::SubmitPhone(_)
            | TelegramCommand::SubmitCode(_)
            | TelegramCommand::SubmitPassword(_)
            | TelegramCommand::RestartAuth => NetworkEvent::Fatal(error),
        };
        Some(event)
    }
}

impl fmt::Debug for TelegramCommand {
    #[allow(clippy::too_many_lines)]
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LoadChatInfo {
                chat_id,
                request_id,
            } => formatter
                .debug_struct("LoadChatInfo")
                .field("chat_id", chat_id)
                .field("request_id", request_id)
                .finish(),
            Self::LoadMembers {
                chat_id,
                request_id,
            } => formatter
                .debug_struct("LoadMembers")
                .field("chat_id", chat_id)
                .field("request_id", request_id)
                .finish(),
            Self::PreviewInvite { request_id, .. } | Self::JoinInvite { request_id, .. } => {
                formatter
                    .debug_struct(if matches!(self, Self::PreviewInvite { .. }) {
                        "PreviewInvite"
                    } else {
                        "JoinInvite"
                    })
                    .field("request_id", request_id)
                    .finish_non_exhaustive()
            }
            Self::LoadReactions {
                chat_id,
                request_id,
                ..
            }
            | Self::ChangeReaction {
                chat_id,
                request_id,
                ..
            }
            | Self::RefreshReactions {
                chat_id,
                request_id,
                ..
            } => formatter
                .debug_struct(match self {
                    Self::LoadReactions { .. } => "LoadReactions",
                    Self::ChangeReaction { .. } => "ChangeReaction",
                    _ => "RefreshReactions",
                })
                .field("chat_id", chat_id)
                .field("request_id", request_id)
                .finish_non_exhaustive(),
            Self::LoadPoll {
                chat_id,
                message_id,
                request_id,
            }
            | Self::RefreshPoll {
                chat_id,
                message_id,
                request_id,
                ..
            }
            | Self::VotePoll {
                chat_id,
                message_id,
                request_id,
                ..
            } => formatter
                .debug_struct(match self {
                    Self::LoadPoll { .. } => "LoadPoll",
                    Self::RefreshPoll { .. } => "RefreshPoll",
                    _ => "VotePoll",
                })
                .field("chat_id", chat_id)
                .field("message_id", message_id)
                .field("request_id", request_id)
                .finish_non_exhaustive(),
            Self::SetChatUnread {
                chat_id,
                unread,
                request_id,
                read_history,
            } => formatter
                .debug_struct("SetChatUnread")
                .field("chat_id", chat_id)
                .field("unread", unread)
                .field("read_history", read_history)
                .field("request_id", request_id)
                .finish(),
            Self::LoadOlder {
                chat_id,
                request_id,
                before_id,
            } => formatter
                .debug_struct("LoadOlder")
                .field("chat_id", chat_id)
                .field("request_id", request_id)
                .field("before_id", before_id)
                .finish(),
            Self::LoadEdit {
                chat_id,
                message_id,
                request_id,
            } => formatter
                .debug_struct("LoadEdit")
                .field("chat_id", chat_id)
                .field("message_id", message_id)
                .field("request_id", request_id)
                .finish(),
            Self::EditMessage {
                chat_id,
                message_id,
                request_id,
                ..
            } => formatter
                .debug_struct("EditMessage")
                .field("chat_id", chat_id)
                .field("message_id", message_id)
                .field("request_id", request_id)
                .finish_non_exhaustive(),
            Self::OpenSaved { request_id } => formatter
                .debug_struct("OpenSaved")
                .field("request_id", request_id)
                .finish(),
            Self::ReviewForward {
                chat_id,
                message_id,
                destination,
                request_id,
            } => formatter
                .debug_struct("ReviewForward")
                .field("chat_id", chat_id)
                .field("message_id", message_id)
                .field("destination", destination)
                .field("request_id", request_id)
                .finish(),
            Self::ForwardMessage {
                chat_id,
                message_id,
                destination,
                request_id,
                ..
            } => formatter
                .debug_struct("ForwardMessage")
                .field("chat_id", chat_id)
                .field("message_id", message_id)
                .field("destination", destination)
                .field("request_id", request_id)
                .finish_non_exhaustive(),
            Self::CopyText(_) => formatter.write_str("CopyText(<redacted>)"),
            Self::CopyMessage {
                chat_id,
                message_id,
                link,
                request_id,
            } => formatter
                .debug_struct("CopyMessage")
                .field("chat_id", chat_id)
                .field("message_id", message_id)
                .field("link", link)
                .field("request_id", request_id)
                .finish(),
            Self::ReviewDeletion {
                chat_id,
                message_id,
                request_id,
            } => formatter
                .debug_struct("ReviewDeletion")
                .field("chat_id", chat_id)
                .field("message_id", message_id)
                .field("request_id", request_id)
                .finish(),
            Self::DeleteMessage {
                chat_id,
                message_id,
                request_id,
                scope,
                ..
            } => formatter
                .debug_struct("DeleteMessage")
                .field("chat_id", chat_id)
                .field("message_id", message_id)
                .field("request_id", request_id)
                .field("scope", scope)
                .finish_non_exhaustive(),
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
                after_id,
            } => formatter
                .debug_struct("LoadHistory")
                .field("after_id", after_id)
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
            Self::LoadReplyPreviews {
                chat_id,
                message_ids,
                request_id,
            } => formatter
                .debug_struct("LoadReplyPreviews")
                .field("chat_id", chat_id)
                .field("message_ids", message_ids)
                .field("request_id", request_id)
                .finish(),
            Self::SendMessage {
                chat_id,
                local_id,
                text,
                reply_to,
            } => formatter
                .debug_struct("SendMessage")
                .field("chat_id", chat_id)
                .field("local_id", local_id)
                .field("text", text)
                .field("reply_to", reply_to)
                .finish(),
            Self::PrepareAttachments(request) => formatter
                .debug_tuple("PrepareAttachments")
                .field(request)
                .finish(),
            Self::SendAttachment {
                chat_id,
                local_id,
                attachment,
                caption,
                reply_to,
            } => formatter
                .debug_struct("SendAttachment")
                .field("chat_id", chat_id)
                .field("local_id", local_id)
                .field("attachment", attachment)
                .field("caption", caption)
                .field("reply_to", reply_to)
                .finish(),
            Self::DownloadAttachment {
                chat_id,
                message_id,
                ..
            } => formatter
                .debug_struct("DownloadAttachment")
                .field("chat_id", chat_id)
                .field("message_id", message_id)
                .finish(),
            Self::DownloadPreview {
                chat_id,
                message_id,
                request_id,
                thumbnail,
            } => formatter
                .debug_struct("DownloadPreview")
                .field("chat_id", chat_id)
                .field("message_id", message_id)
                .field("request_id", request_id)
                .field("thumbnail", thumbnail)
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
            Self::MarkRead { chat_id, max_id } => formatter
                .debug_struct("MarkRead")
                .field("chat_id", chat_id)
                .field("max_id", max_id)
                .finish(),
            Self::ReadMentions {
                chat_id,
                message_ids,
            } => formatter
                .debug_struct("ReadMentions")
                .field("chat_id", chat_id)
                .field("message_ids", message_ids)
                .finish(),
            Self::SearchMentions {
                chat_id,
                before_id,
                request_id,
            } => formatter
                .debug_struct("SearchMentions")
                .field("chat_id", chat_id)
                .field("before_id", before_id)
                .field("request_id", request_id)
                .finish(),
            Self::SearchCached(request) => formatter
                .debug_struct("SearchCached")
                .field("request_id", &request.id)
                .finish_non_exhaustive(),
            Self::SearchCloud(request) => formatter
                .debug_struct("SearchCloud")
                .field("request_id", &request.id)
                .field("chat_id", &request.chat_id)
                .finish_non_exhaustive(),
            Self::LoadCloudContext {
                chat_id,
                message_id,
                request_id,
            } => formatter
                .debug_struct("LoadCloudContext")
                .field("chat_id", chat_id)
                .field("message_id", message_id)
                .field("request_id", request_id)
                .finish(),
            Self::LoadCachedContext {
                chat_id,
                message_id,
                request_id,
            } => formatter
                .debug_struct("LoadCachedContext")
                .field("chat_id", chat_id)
                .field("message_id", message_id)
                .field("request_id", request_id)
                .finish(),
            Self::ResolveAlertSettings { key, request_id } => formatter
                .debug_struct("ResolveAlertSettings")
                .field("key", key)
                .field("request_id", request_id)
                .finish(),
            Self::SetChatMute {
                chat_id,
                mute,
                request_id,
            } => formatter
                .debug_struct("SetChatMute")
                .field("chat_id", chat_id)
                .field("mute", mute)
                .field("request_id", request_id)
                .finish(),
            Self::CancelSearch => formatter.write_str("CancelSearch"),
            Self::RefreshFolders => formatter.write_str("RefreshFolders"),
            Self::RefreshDialogs => formatter.write_str("RefreshDialogs"),
            Self::RefreshDialogPins => formatter.write_str("RefreshDialogPins"),
            Self::LoadPinnedMessages {
                chat_id,
                before,
                request_id,
            } => formatter
                .debug_struct("LoadPinnedMessages")
                .field("chat_id", chat_id)
                .field("before", before)
                .field("request_id", request_id)
                .finish(),
            Self::LoadPinnedContext {
                chat_id,
                message_id,
                request_id,
            } => formatter
                .debug_struct("LoadPinnedContext")
                .field("chat_id", chat_id)
                .field("message_id", message_id)
                .field("request_id", request_id)
                .finish(),
            Self::ChangeMessagePin {
                chat_id,
                message_id,
                action,
                request_id,
            } => formatter
                .debug_struct("ChangeMessagePin")
                .field("chat_id", chat_id)
                .field("message_id", message_id)
                .field("action", action)
                .field("request_id", request_id)
                .finish(),
            Self::SetArchived {
                chat_id,
                archived,
                request_id,
            } => formatter
                .debug_struct("SetArchived")
                .field("chat_id", chat_id)
                .field("archived", archived)
                .field("request_id", request_id)
                .finish(),
            Self::ChangeDialogPin {
                chat_id,
                scope,
                action,
                request_id,
            } => formatter
                .debug_struct("ChangeDialogPin")
                .field("chat_id", chat_id)
                .field("scope", scope)
                .field("action", action)
                .field("request_id", request_id)
                .finish(),
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
    ChatInfoReady {
        chat_id: ChatId,
        request_id: u64,
        result: Result<crate::chat_info::Info, String>,
    },
    MembersLoaded {
        chat_id: ChatId,
        request_id: u64,
        result: Result<crate::completion::ChatCompletion, String>,
    },
    ChatInfoInvalidated {
        chat_id: ChatId,
    },
    InviteReady {
        request_id: u64,
        result: Result<crate::invites::Preview, String>,
    },
    InviteJoined {
        request_id: u64,
        result: Result<crate::invites::Outcome, String>,
    },
    ReactionsChanged(crate::reactions::Update),
    ReactionsLoading {
        request_id: u64,
    },
    ReactionsLoaded {
        chat_id: ChatId,
        message_id: i32,
        request_id: u64,
        result: Result<crate::reactions::Review, String>,
    },
    ReactionsFinished {
        chat_id: ChatId,
        message_id: Option<i32>,
        request_id: u64,
        error: Option<String>,
    },
    PollChanged(crate::polls::Update),
    PollLoading {
        request_id: u64,
    },
    PollLoaded {
        chat_id: ChatId,
        message_id: i32,
        request_id: u64,
        result: Result<crate::polls::Poll, String>,
    },
    PollFinished {
        chat_id: ChatId,
        message_id: i32,
        request_id: u64,
        voting: bool,
        error: Option<String>,
    },
    ChatUnreadChanged {
        chat_id: ChatId,
        unread: bool,
    },
    ChatUnreadFinished {
        chat_id: ChatId,
        unread: bool,
        request_id: u64,
        snapshot: Option<crate::read_state::Snapshot>,
        error: Option<String>,
    },
    ReplyPreviewsLoading {
        chat_id: ChatId,
        request_id: u64,
    },
    ReplyPreviews {
        chat_id: ChatId,
        request_id: u64,
        messages: Vec<Message>,
        unavailable: Vec<i32>,
        complete: bool,
    },
    ReplyPreviewsFailed {
        chat_id: ChatId,
        request_id: u64,
        error: String,
    },
    /// A primary-DC observation, scoped to this worker's account and lifetime.
    Telemetry {
        dc_id: Option<i32>,
        latency: Option<crate::statusline::Latency>,
    },
    PinnedMessagesLoading {
        chat_id: ChatId,
        request_id: u64,
    },
    PinnedMessages {
        chat_id: ChatId,
        request_id: u64,
        page: crate::pins::MessagePage,
    },
    PinnedMessagesFailed {
        chat_id: ChatId,
        request_id: u64,
        error: String,
    },
    PinnedContext {
        chat_id: ChatId,
        message_id: i32,
        request_id: u64,
        messages: Vec<Message>,
    },
    MessagePinsChanged {
        chat_id: ChatId,
        message_ids: Vec<i32>,
        pinned: bool,
    },
    MessagePinsCleared {
        chat_id: ChatId,
    },
    MessagePinFinished {
        chat_id: ChatId,
        request_id: u64,
        error: Option<String>,
    },
    DialogPins(crate::pins::DialogPins),
    ArchiveChanged {
        chat_id: ChatId,
        archived: bool,
    },
    DialogPinFinished {
        request_id: u64,
        error: Option<String>,
    },
    AttachmentDownloadStarted {
        chat_id: ChatId,
        message_id: i32,
        request_id: u64,
        media_id: Option<i64>,
    },
    CachedContext {
        request_id: u64,
        chat_id: ChatId,
        message_id: i32,
        messages: Vec<Message>,
    },
    SearchResults {
        request_id: u64,
        page: crate::search::Page,
    },
    CloudSearchLoading {
        chat_id: ChatId,
        request_id: u64,
    },
    CloudSearchCancelled {
        request_id: u64,
    },
    CloudSearchResults {
        request_id: u64,
        chat_id: ChatId,
        page: crate::cloud_search::Page,
    },
    CloudSearchContext {
        request_id: u64,
        chat_id: ChatId,
        message_id: i32,
        messages: Vec<Message>,
    },
    SearchFailed {
        request_id: u64,
        error: String,
    },
    Folders(Vec<crate::folders::Folder>),
    NotificationSettingsChanged,
    AlertSettingsReady {
        key: crate::notifications::SettingsKey,
        request_id: u64,
        result: Result<crate::notifications::Resolved, String>,
    },
    ChatMuteChanged {
        chat_id: ChatId,
        until: i64,
    },
    ChatMuteFinished {
        chat_id: ChatId,
        request_id: u64,
        result: Result<Option<i64>, String>,
    },
    UnreadChanged {
        chat_id: ChatId,
        max_id: i32,
        unread: u32,
    },
    OlderHistory {
        chat_id: ChatId,
        request_id: u64,
        before_id: i32,
        messages: Vec<Message>,
    },
    OlderHistoryFailed {
        chat_id: ChatId,
        request_id: u64,
        error: String,
    },
    CachedSnapshot {
        user_name: Option<String>,
        chats: Vec<Chat>,
    },
    AttachmentsPrepared {
        key: crate::drafts::Key,
        request_id: u64,
        result: Result<crate::staging::Prepared, String>,
    },
    CachedHistory {
        chat_id: ChatId,
        request_id: u64,
        messages: Vec<Message>,
    },
    HistoryLoading {
        chat_id: ChatId,
        request_id: u64,
    },
    SyncCheckpoint(crate::cache::SyncCursor),
    CacheAccountReset {
        user_id: i64,
    },
    AccountIdentity {
        user_id: i64,
    },
    LocalDrafts {
        user_id: i64,
        drafts: Vec<crate::drafts::Stored>,
    },
    CacheMessage(Message),
    CacheInvalidated {
        chat_id: Option<ChatId>,
    },
    Auth(AuthPrompt),
    Ready {
        user_name: String,
    },
    DialogsLoading,
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
    SavedReady {
        request_id: u64,
        result: Result<Chat, String>,
    },
    ForwardReady {
        request_id: u64,
        result: Result<crate::forwarding::Plan, String>,
    },
    ForwardFinished {
        request_id: u64,
        error: Option<String>,
    },
    MessageCopyReady {
        request_id: u64,
        result: Result<String, String>,
    },
    DeletionReady {
        chat_id: ChatId,
        message_id: i32,
        request_id: u64,
        result: Result<crate::deletion::Plan, String>,
    },
    DeleteFinished {
        chat_id: ChatId,
        message_id: i32,
        request_id: u64,
        error: Option<String>,
    },
    EditLoaded {
        chat_id: ChatId,
        request_id: u64,
        result: Result<crate::editing::Source, String>,
    },
    EditFinished {
        chat_id: ChatId,
        request_id: u64,
        error: Option<String>,
    },
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
        max_id: i32,
        snapshot: Option<crate::read_state::Snapshot>,
    },
    ReadMarkFailed {
        chat_id: ChatId,
        max_id: i32,
        error: String,
    },
    /// Content receipts have the same account/channel ID scopes as deletion.
    MessageContentsRead {
        channel_id: Option<ChatId>,
        message_ids: Vec<i32>,
    },
    MentionsReadFinished {
        chat_id: ChatId,
        message_ids: Vec<i32>,
        error: Option<String>,
    },
    MessagesRead {
        chat_id: ChatId,
        max_id: i32,
    },
    SendFailed {
        chat_id: ChatId,
        local_id: i32,
        text: String,
        reply_to: Option<i32>,
        error: String,
    },
    /// An attachment send failed. Kept distinct from text failures so the UI
    /// never retries a display label as a text message.
    AttachmentSendFailed {
        chat_id: ChatId,
        local_id: i32,
        attachment: crate::staging::Attachment,
        caption: String,
        reply_to: Option<i32>,
        error: String,
    },
    PreviewDownloaded {
        chat_id: ChatId,
        message_id: i32,
        request_id: u64,
        path: PathBuf,
    },
    PreviewDownloadFailed {
        chat_id: ChatId,
        message_id: i32,
        request_id: u64,
        error: String,
    },
    /// Non-channel message identifiers share the account-wide namespace.
    MessagesDeleted {
        channel_id: Option<ChatId>,
        message_ids: Vec<i32>,
    },
    AttachmentDownloaded {
        request_id: u64,
        chat_id: ChatId,
        message_id: i32,
        path: PathBuf,
    },
    AttachmentDownloadFailed {
        request_id: u64,
        chat_id: ChatId,
        message_id: i32,
        error: String,
    },
    LinkResolved {
        url: String,
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
