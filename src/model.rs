use serde::{Deserialize, Serialize};

use chrono::{DateTime, Local, TimeZone, Utc};
use std::path::Path;

pub type ChatId = i64;

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
pub struct Chat {
    pub id: ChatId,
    pub title: String,
    pub kind: ChatKind,
    #[serde(default)]
    pub membership: crate::folders::ChatMembership,
    pub unread: u32,
    /// None means an older cache without a server read boundary.
    #[serde(default)]
    pub read_inbox_max_id: Option<i32>,
    pub last_message: String,
    #[serde(default)]
    pub last_message_id: Option<i32>,
    pub last_activity: Option<DateTime<Utc>>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChatKind {
    Direct,
    Group,
    Channel,
}

/// Downloadable media attached to a Telegram message.
///
/// This deliberately contains metadata only. Telegram's file reference stays
/// in the network layer and is refreshed on demand before a download.
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
pub struct Attachment {
    #[serde(default)]
    pub source_id: Option<i64>,
    pub kind: AttachmentKind,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
    pub size: Option<u64>,
    /// Unicode fallback supplied by Telegram for stickers.
    pub fallback_emoji: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachmentKind {
    Photo,
    File,
    Video,
    Audio,
    Sticker,
    Other,
}

impl Attachment {
    /// Media that can be shown in the terminal without opening an external app.
    #[must_use]
    pub fn supports_preview(&self) -> bool {
        matches!(self.kind, AttachmentKind::Photo | AttachmentKind::Sticker)
            || self
                .mime_type
                .as_deref()
                .is_some_and(|mime| mime.starts_with("image/"))
    }

    /// Telegram supplies raster thumbnails for animated/vector stickers.
    #[must_use]
    pub fn preview_uses_thumbnail(&self) -> bool {
        self.kind == AttachmentKind::Sticker
            && matches!(
                self.mime_type.as_deref(),
                Some("application/x-tgsticker" | "video/webm")
            )
    }

    /// A safe display name and download-file hint.
    #[must_use]
    pub fn display_name(&self) -> &str {
        self.file_name.as_deref().unwrap_or(match self.kind {
            AttachmentKind::Photo => "photo.jpg",
            AttachmentKind::File | AttachmentKind::Other => "attachment",
            AttachmentKind::Video => "video",
            AttachmentKind::Audio => "audio",
            AttachmentKind::Sticker => "sticker",
        })
    }

    /// Infer whether a dragged file should be sent as a compressed photo.
    #[must_use]
    pub fn path_looks_like_photo(path: &Path) -> bool {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(
                    extension.to_ascii_lowercase().as_str(),
                    "jpg" | "jpeg" | "png" | "webp"
                )
            })
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
pub struct Mention {
    pub unread: bool,
    /// Voice, round-video or self-destructing content needs explicit consumption.
    pub requires_playback: bool,
}

/// A sendable sticker reference. File references expire after some days, so
/// panels revalidate the cached lists against Telegram and a failed send
/// retries once with a fresh reference instead of persisting them.
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
pub struct StickerRef {
    pub id: i64,
    pub access_hash: i64,
    pub file_reference: Vec<u8>,
    pub emoji: String,
    pub mime_type: String,
    /// Telegram's static raster thumbnail size label, when the document has one.
    pub thumb_size: Option<String>,
    /// The sticker's own set, used to refresh an expired file reference.
    pub set_id: Option<i64>,
    pub set_access_hash: Option<i64>,
}

/// An installed sticker set listed in the panel's sidebar.
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
pub struct StickerSetRef {
    pub id: i64,
    pub access_hash: i64,
    pub title: String,
}

/// One sticker section with Telegram's revalidation hash. This JSON shape is
/// also the local cache row format.
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
pub struct StickerSection<T> {
    pub hash: i64,
    pub items: Vec<T>,
}

impl<T> Default for StickerSection<T> {
    fn default() -> Self {
        Self {
            hash: 0,
            items: Vec::new(),
        }
    }
}

/// Recent and favorite stickers plus the installed set list, kept in the local
/// cache and revalidated with Telegram's hash on every panel open.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StickerOverview {
    pub recent: StickerSection<StickerRef>,
    pub favorites: StickerSection<StickerRef>,
    pub sets: StickerSection<StickerSetRef>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
pub struct Message {
    #[serde(default)]
    pub reactions: Option<crate::reactions::Summary>,
    #[serde(default)]
    pub poll: Option<crate::polls::Poll>,
    #[serde(default)]
    pub entities: Vec<crate::entities::Entity>,
    #[serde(skip)]
    pub notification: Option<crate::notifications::Metadata>,
    #[serde(default)]
    pub mention: Option<Mention>,
    #[serde(default)]
    pub pinned: bool,
    pub id: i32,
    pub chat_id: ChatId,
    pub sender: String,
    /// Public handle without @, when supplied by Telegram; absent in older caches.
    #[serde(default)]
    pub sender_username: Option<String>,
    /// The message this one replies to, when Telegram exposes a stable target.
    ///
    /// `sender` is deliberately optional: resolving the target author must not
    /// turn loading a lightweight history page into one RPC per reply.
    pub reply_to: Option<ReplyInfo>,
    pub text: String,
    pub timestamp: DateTime<Utc>,
    #[serde(default)]
    pub edited_at: Option<DateTime<Utc>>,
    pub outgoing: bool,
    pub delivery: Delivery,
    pub attachment: Option<Attachment>,
    /// HTTP(S) and Telegram targets extracted from message entities. Hidden
    /// `textUrl` targets live here instead of being appended to the body.
    pub links: Vec<MessageLink>,
    /// Inline bot buttons rendered below the message. Callback payloads stay
    /// in the Telegram worker and are looked up by the stable flattened index.
    pub buttons: Vec<MessageButton>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
pub struct MessageLink {
    pub label: String,
    pub url: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
pub struct MessageButton {
    pub label: String,
    pub index: u16,
    pub kind: MessageButtonKind,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageButtonKind {
    Url,
    Callback,
    Game,
    Unsupported,
}

impl MessageButtonKind {
    #[must_use]
    pub const fn is_supported(self) -> bool {
        matches!(self, Self::Url | Self::Callback | Self::Game)
    }
}

/// Lightweight metadata used to render and navigate a reply.
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq)]
pub struct ReplyInfo {
    pub message_id: i32,
    /// Stable dialog identifier for the target. Telegram permits replies to a
    /// message from another peer, where message IDs alone are ambiguous.
    pub chat_id: ChatId,
    /// Best-effort terminal-safe `@username`, falling back to the sender's
    /// display name when Telegram does not expose a username.
    pub sender: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq)]
pub enum Delivery {
    Pending,
    Sent,
    Read,
    Failed,
}

impl Chat {
    #[must_use]
    pub fn activity_label(&self, now: DateTime<Local>) -> String {
        let Some(activity) = self.last_activity else {
            return String::new();
        };
        let local = activity.with_timezone(&Local);
        if local.date_naive() == now.date_naive() {
            local.format("%H:%M").to_string()
        } else {
            local.format("%b %d").to_string()
        }
    }
}

impl Message {
    #[must_use]
    pub fn has_spoilers(&self) -> bool {
        self.poll.as_ref().is_some_and(|poll| {
            poll.texts().any(|text| {
                text.entities.iter().any(|entity| {
                    entity.kind == crate::entities::Kind::Spoiler && entity.valid_for(&text.text)
                })
            })
        }) || self.entities.iter().any(|entity| {
            entity.kind == crate::entities::Kind::Spoiler && entity.valid_for(&self.text)
        })
    }

    /// Summary surfaces never reveal spoilers, even after an explicit reveal
    /// in the transcript. Copy and edit intentionally use the complete text.
    #[must_use]
    pub fn preview_text(&self) -> String {
        if self.text.is_empty() {
            if let Some(poll) = &self.poll {
                return format!(
                    "[{}] {}",
                    if poll.definition.quiz { "quiz" } else { "poll" },
                    poll.definition.question.preview()
                );
            }
            self.attachment
                .as_ref()
                .map_or("Message", Attachment::display_name)
                .to_owned()
        } else {
            crate::entities::conceal(&self.text, &self.entities).into_owned()
        }
    }

    pub(crate) fn acknowledge_contents(&mut self, channel: Option<ChatId>, ids: &[i32]) {
        if channel.map_or(self.chat_id > -1_000_000_000_000, |id| self.chat_id == id)
            && ids.contains(&self.id)
            && let Some(mention) = &mut self.mention
        {
            mention.unread = false;
        }
    }

    #[must_use]
    pub fn timestamp_from_unix(timestamp: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(timestamp, 0)
            .single()
            .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
    }
}

#[must_use]
pub fn sanitize_terminal_text(value: &str) -> String {
    sanitize_text_with_offsets(value, |_, _| {})
}

/// Report valid raw UTF-16 boundaries and their sanitized UTF-8 positions. A
/// surrogate's midpoint is never reported; stripped controls have zero width.
pub(crate) fn sanitize_text_with_offsets(
    value: &str,
    mut boundary: impl FnMut(usize, usize),
) -> String {
    let mut clean = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    let mut units = 0;
    boundary(0, 0);
    while let Some(character) = chars.next() {
        units += character.len_utf16();
        if character == '\u{1b}' {
            boundary(units, clean.len());
            if chars.peek() == Some(&'[') {
                chars.next();
                units += 1;
                boundary(units, clean.len());
                for next in chars.by_ref() {
                    units += next.len_utf16();
                    boundary(units, clean.len());
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            continue;
        }
        match character {
            '\n' => clean.push(character),
            '\t' => clean.push_str("    "),
            character if character.is_control() => {}
            character => clean.push(character),
        }
        boundary(units, clean.len());
    }
    clean
}

/// Remove terminal controls and flatten text that must stay on one UI row.
#[must_use]
pub fn sanitize_terminal_line(value: &str) -> String {
    sanitize_terminal_text(value)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Attachment, sanitize_terminal_line, sanitize_terminal_text};

    #[test]
    fn strips_terminal_control_characters() {
        assert_eq!(
            sanitize_terminal_text("safe\u{1b}[31mred\u{1b}[0m\nnext\u{7}"),
            "safered\nnext"
        );
    }

    #[test]
    fn line_sanitizer_flattens_untrusted_names() {
        assert_eq!(
            sanitize_terminal_line(" Alice\n\tBob \u{1b}[2J "),
            "Alice Bob"
        );
    }

    #[test]
    fn recognizes_common_dragged_photo_extensions_case_insensitively() {
        assert!(Attachment::path_looks_like_photo(Path::new("holiday.JPEG")));
        assert!(Attachment::path_looks_like_photo(Path::new("image.webp")));
        assert!(!Attachment::path_looks_like_photo(Path::new("archive.zip")));
        assert!(!Attachment::path_looks_like_photo(Path::new("README")));
    }
}
