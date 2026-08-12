use chrono::{DateTime, Local, TimeZone, Utc};
use std::path::Path;

pub type ChatId = i64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Chat {
    pub id: ChatId,
    pub title: String,
    pub kind: ChatKind,
    pub unread: u32,
    pub last_message: String,
    pub last_activity: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChatKind {
    Direct,
    Group,
    Channel,
}

/// Downloadable media attached to a Telegram message.
///
/// This deliberately contains metadata only. Telegram's file reference stays
/// in the network layer and is refreshed on demand before a download.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Attachment {
    pub kind: AttachmentKind,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
    pub size: Option<u64>,
    /// Unicode fallback supplied by Telegram for stickers.
    pub fallback_emoji: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachmentKind {
    Photo,
    File,
    Video,
    Audio,
    Sticker,
    Other,
}

impl Attachment {
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Message {
    pub id: i32,
    pub chat_id: ChatId,
    pub sender: String,
    pub text: String,
    pub timestamp: DateTime<Utc>,
    pub outgoing: bool,
    pub delivery: Delivery,
    pub attachment: Option<Attachment>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
    pub fn timestamp_from_unix(timestamp: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(timestamp, 0)
            .single()
            .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
    }
}

#[must_use]
pub fn sanitize_terminal_text(value: &str) -> String {
    let mut clean = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
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

    use super::{sanitize_terminal_line, sanitize_terminal_text, Attachment};

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
