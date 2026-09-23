//! Small, ordered status segments inspired by Yazi's status.lua and Codex's
//! finite status-line items. Ratatui owns cell layout; no upstream code is copied.

use super::{ACCENT, DANGER, MUTED, SUCCESS, WARNING, clamp_u16, truncate_cells};
use crate::{
    app::{AppState, AttachmentState, Focus, MessageAction, Mode},
    event::ConnectionStatus,
    keymap::Context,
    model::Delivery,
    statusline::Item,
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

struct Segment {
    text: String,
    style: Style,
    priority: u8,
    right: bool,
}

pub(super) fn render(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    if area.is_empty() || !app.keymap.statusline.enabled {
        return;
    }
    let mut segments = app
        .keymap
        .statusline
        .left
        .iter()
        .map(|item| (*item, false))
        .chain(app.keymap.statusline.right.iter().map(|item| (*item, true)))
        .filter_map(|(item, right)| {
            segment(
                item,
                right,
                app,
                area.width < crate::sidebar::MIN_SPLIT_WIDTH,
            )
        })
        .collect::<Vec<_>>();
    let width = usize::from(area.width);
    while total_width(&segments) > width {
        let Some((index, entry)) = segments
            .iter()
            .enumerate()
            .min_by_key(|(_, entry)| entry.priority)
        else {
            break;
        };
        if entry.priority >= 90 {
            let excess = total_width(&segments).saturating_sub(width);
            let remaining = entry.text.width().saturating_sub(excess);
            if remaining == 0 {
                segments.remove(index);
            } else {
                segments[index].text = truncate_cells(&entry.text, remaining);
            }
            continue;
        }
        segments.remove(index);
    }
    let right_width = segments
        .iter()
        .filter(|segment| segment.right)
        .map(|segment| segment.text.width() + 1)
        .sum::<usize>();
    let columns = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(clamp_u16(right_width.min(width))),
    ])
    .split(area);
    for (right, column) in [(false, columns[0]), (true, columns[1])] {
        let mut remaining = usize::from(column.width);
        let mut spans = Vec::new();
        for segment in segments.iter().filter(|segment| segment.right == right) {
            if remaining == 0 {
                break;
            }
            let text = truncate_cells(&segment.text, remaining.saturating_sub(1));
            remaining = remaining.saturating_sub(text.width() + 1);
            spans.push(Span::styled(text, segment.style));
            spans.push(Span::raw(" "));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), column);
    }
}

fn total_width(segments: &[Segment]) -> usize {
    segments
        .iter()
        .map(|segment| segment.text.width() + 1)
        .sum()
}

fn segment(item: Item, right: bool, app: &AppState, narrow: bool) -> Option<Segment> {
    let (text, color, priority) = match item {
        Item::Mode => (
            match app.mode {
                Mode::Compose => " INSERT ",
                Mode::Edit => " EDIT ",
                Mode::ForwardPrompt => " FORWARD ",
                Mode::Poll => " POLL ",
                Mode::Reactions => " REACT ",
                Mode::Stickers => " STICKER ",
                Mode::DeletePrompt => " DELETE ",
                Mode::Command => " COMMAND ",
                Mode::Filter | Mode::Search => " SEARCH ",
                Mode::Navigate if app.focus == Focus::Chats => " CHATS ",
                Mode::Navigate if app.selected_message.is_some() => " SELECT ",
                Mode::Navigate => " NORMAL ",
                _ => " VIEW ",
            }
            .to_owned(),
            ACCENT,
            100,
        ),
        Item::App => ("Termgram".to_owned(), ACCENT, 10),
        Item::Account => (
            format!(
                "{} · {}",
                app.active_account(),
                app.user_name.as_deref().unwrap_or("Telegram")
            ),
            Color::Reset,
            60,
        ),
        Item::Connection => {
            let (label, color) = match app.connection {
                ConnectionStatus::Connecting => ("connecting", WARNING),
                ConnectionStatus::Online => ("online", SUCCESS),
                ConnectionStatus::Reconnecting => ("reconnecting", WARNING),
                ConnectionStatus::Offline => ("offline", DANGER),
            };
            (label.to_owned(), color, 80)
        }
        Item::Latency => (
            app.metrics
                .latency
                .filter(|sample| !sample.expired() && app.connection == ConnectionStatus::Online)
                .map_or_else(
                    || "ping —".to_owned(),
                    |sample| format!("{} ms", sample.elapsed.as_millis()),
                ),
            MUTED,
            30,
        ),
        Item::Dc => (
            app.metrics
                .dc_id
                .filter(|_| app.connection == ConnectionStatus::Online)
                .map_or_else(|| "DC —".to_owned(), |dc| format!("DC {dc}")),
            MUTED,
            20,
        ),
        Item::Position => {
            let text = if app.active_chat_id.is_none() {
                String::new()
            } else if app.message_scroll == 0 {
                "live".to_owned()
            } else if app.new_messages_while_scrolled > 0 {
                format!("+{} new", app.new_messages_while_scrolled)
            } else {
                format!("↑{} rows", app.message_scroll)
            };
            (text, MUTED, 40)
        }
        Item::Message => message_metadata(app)?,
        Item::Notifications => (app.focused_mute_label()?, MUTED, 35),
        Item::Context => (context(app, narrow), MUTED, 90),
    };
    if text.is_empty() {
        return None;
    }
    let style = if item == Item::Mode {
        Style::default()
            .bg(if app.mode == Mode::Compose {
                Color::Green
            } else {
                Color::Blue
            })
            .fg(Color::Black)
            .bold()
    } else {
        Style::default().fg(color)
    };
    Some(Segment {
        text,
        style,
        priority,
        right,
    })
}

fn selected_context(app: &AppState) -> Option<String> {
    if app.mode == Mode::Navigate
        && app.focus == Focus::Conversation
        && app.selected_message.is_some()
    {
        let hint = |action| app.keymap.hint(Context::Conversation, action);
        let message = app.inspected_message()?;
        let mut hints = Vec::new();
        if app.has_newer_history() {
            hints.push(format!("{} continue", hint("message_down")));
            hints.push(format!("{} latest", hint("latest")));
        }
        if app.message_actions(message).get(app.selected_action) == Some(&MessageAction::Reply) {
            hints.push(format!("{} original", hint("open")));
        }
        match app.message_actions(message).get(app.selected_action) {
            Some(MessageAction::Reactions) => hints.push(format!("{} react", hint("open"))),
            Some(MessageAction::Poll) => hints.push(format!("{} poll", hint("open"))),
            Some(MessageAction::Spoilers) => hints.push(format!(
                "{} {} spoiler",
                hint("open"),
                if app.spoilers_revealed(message) {
                    "hide"
                } else {
                    "reveal"
                }
            )),
            Some(MessageAction::ExpandQuote) => hints.push(format!(
                "{} {} quote",
                hint("open"),
                if app.quotes_expanded(message) {
                    "collapse"
                } else {
                    "expand"
                }
            )),
            _ => {}
        }
        if let Some(attachment) = &message.attachment {
            if attachment.supports_preview() {
                hints.push(format!("{} preview", hint("preview")));
            }
            let manager = if cfg!(target_os = "macos") {
                "Finder"
            } else if cfg!(windows) {
                "Explorer"
            } else {
                "files"
            };
            hints.push(format!("{} {manager}", hint("reveal")));
        }
        hints.push(format!("{} reply", hint("compose")));
        if message.id > 0 && !message.text.is_empty() {
            hints.push(format!("{} copy", hint("copy_text")));
        }
        if message.id > 0 {
            hints.push(format!("{} forward", hint("forward_message")));
        }
        if message.outgoing && message.id > 0 {
            hints.push(format!("{} edit", hint("edit_message")));
        }
        if hints.len() == 1 {
            hints.push(format!("{} clear", hint("cancel")));
        }
        return Some(hints.join(" · "));
    }
    None
}

#[allow(clippy::too_many_lines)]
fn context(app: &AppState, narrow: bool) -> String {
    if app.mode == Mode::Reactions {
        let hint = |action| app.keymap.hint(Context::Reactions, action);
        if narrow {
            return format!("{} toggle · {} close", hint("open"), hint("cancel"));
        }
        return format!(
            "{} toggle · {} remove mine · {} refresh · {} close",
            hint("open"),
            hint("clear_reactions"),
            hint("refresh"),
            hint("cancel")
        );
    }

    if app.mode == Mode::Stickers {
        let hint = |action| app.keymap.hint(Context::Stickers, action);
        if narrow {
            return format!("{} send · {} close", hint("open"), hint("cancel"));
        }
        return format!(
            "{} send · {}/{} sets · {} refresh · {} close",
            hint("open"),
            hint("sticker_set_previous"),
            hint("sticker_set_next"),
            hint("refresh"),
            hint("cancel")
        );
    }

    if app.mode == Mode::Poll {
        let hint = |action| app.keymap.hint(Context::Poll, action);
        if narrow {
            return format!("{} vote · {} close", hint("send"), hint("cancel"));
        }
        return format!(
            "{} choose · {} vote · {} retract · {} refresh · {} close",
            hint("toggle_poll_answer"),
            hint("send"),
            hint("retract_vote"),
            hint("refresh"),
            hint("cancel")
        );
    }
    if app.mode == Mode::ForwardPrompt {
        return format!(
            "{} forward · {} cancel",
            app.keymap.hint(Context::Forward, "send"),
            app.keymap.hint(Context::Forward, "cancel")
        );
    }
    if app.mode == Mode::ChatInfo {
        let hint = |action| app.keymap.hint(Context::Overlay, action);
        return format!(
            "{}/{} scroll · {} refresh · {} close",
            hint("up"),
            hint("down"),
            hint("refresh"),
            hint("cancel")
        );
    }
    if app.mode == Mode::Invite {
        let hint = |action| app.keymap.hint(Context::Overlay, action);
        return format!(
            "{}/{} select · {} confirm · {} close",
            hint("up"),
            hint("down"),
            hint("open"),
            hint("cancel")
        );
    }
    if app.mode == Mode::DeletePrompt {
        let hint = |action| app.keymap.hint(Context::Overlay, action);
        return format!(
            "{}/{} scope · {} confirm · {} close",
            hint("up"),
            hint("down"),
            hint("open"),
            hint("cancel")
        );
    }
    if app.mode == Mode::Edit {
        let hint = |action| app.keymap.hint(Context::Edit, action);
        return format!(
            "{} save · {} keep/close · {} discard",
            hint("send"),
            hint("cancel"),
            hint("discard_edit")
        );
    }
    if app.mode == Mode::Help {
        return if app.help.editing {
            format!(
                "{} keep filter · {} clear search",
                app.keymap.hint(Context::Input, "open"),
                app.keymap.hint(Context::Input, "cancel")
            )
        } else {
            format!(
                "{} search shortcuts · {} close/clear",
                app.keymap.hint(Context::Help, "filter"),
                app.keymap.hint(Context::Help, "cancel")
            )
        };
    }
    if app.mode == Mode::Status {
        return format!(
            "{}/{} scroll · {} close",
            app.keymap.hint(Context::Overlay, "up"),
            app.keymap.hint(Context::Overlay, "down"),
            app.keymap.hint(Context::Overlay, "cancel")
        );
    }
    if app.mode == Mode::Command {
        let hint = |action| app.keymap.hint(Context::Command, action);
        return format!(
            "{} complete · {} run · {} cancel",
            hint("complete_next"),
            hint("open"),
            hint("cancel")
        );
    }
    if app.mode == Mode::Compose
        && let Some(reason) = app.active_chat_id.and_then(|id| app.draft_restriction(id))
    {
        return format!("{reason} · :info");
    }
    if app.status_message.is_some() {
        return String::new();
    }
    if app.mode == Mode::Compose {
        let hint = |action| app.keymap.hint(Context::Compose, action);
        return format!(
            "{} send · {} stickers · {} close",
            hint("send"),
            hint("stickers"),
            hint("cancel")
        );
    }
    if let Some(hint) = selected_context(app) {
        return hint;
    }
    if let Some(version) = app.available_update() {
        return format!("Update {version} available · run tg update");
    }
    if app.mode != Mode::Navigate {
        return String::new();
    }
    let context = if app.focus == Focus::Chats {
        Context::Chats
    } else {
        Context::Conversation
    };
    if app.focus == Focus::Conversation && app.has_newer_history() {
        return format!(
            "{} continue · {} latest · {} commands",
            app.keymap.hint(context, "message_down"),
            app.keymap.hint(context, "latest"),
            app.keymap.hint(context, "command")
        );
    }
    let back = if app.sidebar_hidden {
        format!(
            "{} chats · ",
            app.keymap.hint(Context::Conversation, "toggle_sidebar")
        )
    } else if narrow && app.narrow_conversation {
        format!(
            "{} chats · ",
            app.keymap.hint(Context::Conversation, "cancel")
        )
    } else if app.focus == Focus::Chats && app.folders.len() > 1 {
        format!(
            "{} {} folders · ",
            app.keymap.hint(Context::Chats, "folder_previous"),
            app.keymap.hint(Context::Chats, "folder_next")
        )
    } else {
        String::new()
    };
    format!(
        "{back}{} commands · {} help",
        app.keymap.hint(context, "command"),
        app.keymap.hint(context, "help")
    )
}

fn message_metadata(app: &AppState) -> Option<(String, Color, u8)> {
    use chrono::{Datelike, Local};
    let message = app.inspected_message()?;
    let timestamp = message.timestamp.with_timezone(&Local);
    let now = Local::now();
    let mut parts = Vec::new();
    if app.settings().show_message_ids {
        parts.push(format!("#{}", message.id));
    }
    parts.push(
        timestamp
            .format(if timestamp.date_naive() == now.date_naive() {
                "%H:%M"
            } else if timestamp.year() == now.year() {
                "%m-%d %H:%M"
            } else {
                "%Y-%m-%d %H:%M"
            })
            .to_string(),
    );
    if let Some(summary) = &message.reactions {
        parts.push(super::reactions::status(summary));
    }
    if let Some(poll) = &message.poll {
        parts.push(super::polls::status(poll));
    }
    let mut color = MUTED;
    if message.outgoing {
        let (state, tone) = match message.delivery {
            Delivery::Pending => ("sending", WARNING),
            Delivery::Sent => ("sent", MUTED),
            Delivery::Read => ("read", SUCCESS),
            Delivery::Failed => ("failed", DANGER),
        };
        color = tone;
        parts.push(state.to_owned());
    }
    if message
        .mention
        .as_ref()
        .is_some_and(|mention| mention.unread)
    {
        parts.push("@ unread mention".to_owned());
        color = ACCENT;
    }
    if let Some(edited) = message.edited_at {
        parts.push(format!(
            "edited {}",
            edited.with_timezone(&Local).format("%H:%M")
        ));
    }
    if message.pinned {
        parts.push(format!(
            "{} pinned",
            super::icons::Icons(app.keymap.nerd_font).pin()
        ));
    }
    if let Some(reply) = &message.reply_to {
        parts.push(format!("reply #{}", reply.message_id));
        let status = app.reply_preview_status(reply);
        if !status.is_empty() {
            parts.push(status.to_owned());
        }
    }
    if let Some(attachment) = &message.attachment {
        // Actual filenames matter; Telegram's generated photo.jpg does not.
        if let Some(name) = &attachment.file_name
            && !(attachment.kind == crate::model::AttachmentKind::Photo && name == "photo.jpg")
        {
            parts.push(name.clone());
        }
        if let Some(size) = attachment.size {
            parts.push(super::transcript::human_size(size));
        }
        match app.attachment_state(message.chat_id, message.id) {
            AttachmentState::Downloading => parts.push("downloading".to_owned()),
            AttachmentState::Downloaded => parts.push("downloaded".to_owned()),
            AttachmentState::Ready => {}
        }
        if let Some(preview) = app.media_previews.get(&(message.chat_id, message.id))
            && !preview.status.is_empty()
        {
            parts.push(preview.status.clone());
        }
    }
    Some((parts.join(" · "), color, 90))
}
