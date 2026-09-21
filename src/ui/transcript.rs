//! Author/metadata, reply, body and media hierarchy inspired by Codex history
//! cells. Reuses Termgram's wrapping/actions and Ratatui spans; no copied runtime.
use super::{ACCENT, MUTED, icons::Icons, truncate_cells, wrap_cells};
use crate::{
    app::{AppState, MessageAction},
    model::{Attachment, AttachmentKind, Message, MessageButtonKind},
};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use std::hash::{DefaultHasher, Hash, Hasher};

pub(super) const GUTTER_WIDTH: u16 = 2;

pub(super) struct RenderedMessage {
    pub lines: Vec<Line<'static>>,
    pub body_height: usize,
    pub body_action: Option<usize>,
    pub action_rows: Vec<(usize, usize)>,
}

pub(super) fn gutter(selected: bool) -> Span<'static> {
    Span::styled(
        if selected { "▎ " } else { "  " },
        Style::default().fg(ACCENT),
    )
}

pub(super) fn render(message: &Message, width: usize, app: &AppState) -> RenderedMessage {
    let selected = app.selected_message == Some(message.id);
    let icons = Icons(app.keymap.nerd_font);
    let actions = app.message_actions(message);
    let body_width = width.saturating_sub(usize::from(GUTTER_WIDTH)).max(1);
    let mut lines = header(message, body_width, selected);
    let mut action_rows = Vec::new();
    if let Some(reply) = &message.reply_to {
        let original = app.reply_message(reply);
        let author = original
            .map(|message| message.sender.as_str())
            .or(reply.sender.as_deref())
            .filter(|sender| !sender.eq_ignore_ascii_case("unknown"))
            .unwrap_or("Original message");
        let excerpt = original.map_or_else(String::new, |message| {
            crate::model::sanitize_terminal_line(&message.preview_text())
        });
        let label = if excerpt.is_empty() {
            format!("↩ {author}")
        } else {
            format!("↩ {author} · {excerpt}")
        };
        let reply_action = actions
            .iter()
            .position(|action| *action == MessageAction::Reply);
        let current = selected && reply_action == Some(app.selected_action);
        let quote_width = body_width.saturating_sub(4).max(1);
        let mut parts = wrap_cells(&label, quote_width);
        if parts.len() > 2 {
            parts[1] = format!(
                "{}…",
                truncate_cells(&parts[1], quote_width.saturating_sub(1))
            );
            parts.truncate(2);
        }
        for part in parts {
            if let Some(action) = reply_action {
                action_rows.push((lines.len(), action));
            }
            lines.push(Line::from(vec![
                action_gutter(selected, current),
                Span::styled("  │ ", Style::default().fg(ACCENT)),
                Span::styled(part, action_style(current)),
            ]));
        }
    }
    if let Some(attachment) = &message.attachment
        && !(message.id > 0 && attachment.supports_preview() && app.keymap.messages.images.inline())
    {
        let label = attachment_label(attachment, icons);
        let current =
            selected && actions.get(app.selected_action) == Some(&MessageAction::Attachment);
        for part in wrap_cells(&label, body_width) {
            if let Some(action) = actions
                .iter()
                .position(|action| *action == MessageAction::Attachment)
            {
                action_rows.push((lines.len(), action));
            }
            lines.push(Line::from(vec![
                action_gutter(selected, current),
                Span::styled(part, action_style(current)),
            ]));
        }
    }
    for mut row in body(message, body_width, app) {
        let action = row
            .action
            .and_then(|action| actions.iter().position(|candidate| *candidate == action));
        if let Some(index) = action {
            action_rows.push((lines.len(), index));
        }
        row.line.spans.insert(
            0,
            action_gutter(selected, selected && action == Some(app.selected_action)),
        );
        lines.push(row.line);
    }
    let body_height = lines.len();
    let body_action = actions
        .iter()
        .position(|action| *action == MessageAction::Attachment);
    action_rows.extend(append_message_actions(
        &mut lines,
        message,
        &actions,
        selected,
        app.selected_action,
        body_width,
        if selected { "▎ " } else { "  " },
    ));
    RenderedMessage {
        lines,
        body_height,
        body_action,
        action_rows,
    }
}

fn body(message: &Message, width: usize, app: &AppState) -> Vec<super::entities::Row> {
    let mut body =
        if !message.text.is_empty() || (message.attachment.is_none() && message.poll.is_none()) {
            super::entities::render(message, width, app)
        } else {
            Vec::new()
        };
    body.extend(
        super::polls::transcript(message, width, app)
            .into_iter()
            .map(|row| super::entities::Row {
                line: row.line,
                action: Some(if row.spoiler {
                    MessageAction::Spoilers
                } else {
                    MessageAction::Poll
                }),
            }),
    );
    body.extend(super::reactions::transcript(message, width));
    body
}

fn action_gutter(selected: bool, current: bool) -> Span<'static> {
    if current {
        Span::styled("› ", Style::default().fg(ACCENT).bold())
    } else {
        gutter(selected)
    }
}

fn action_style(current: bool) -> Style {
    let style = Style::default().fg(ACCENT);
    if current { style.bold() } else { style }
}

fn header(message: &Message, width: usize, selected: bool) -> Vec<Line<'static>> {
    let label = message.sender_username.as_ref().map_or_else(
        || message.sender.clone(),
        |username| format!("{} · @{username}", message.sender),
    );
    super::wrapping::ranges(&label, width, true)
        .into_iter()
        .map(|row| {
            let split = message.sender.len().clamp(row.start, row.end);
            let mut spans = vec![gutter(selected)];
            if row.start < split {
                spans.push(Span::styled(
                    label[row.start..split].to_owned(),
                    Style::default().fg(author_color(message)).bold(),
                ));
            }
            if split < row.end {
                spans.push(Span::styled(
                    label[split..row.end].to_owned(),
                    Style::default().fg(ACCENT),
                ));
            }
            Line::from(spans)
        })
        .collect()
}

fn author_color(message: &Message) -> Color {
    const PALETTE: [Color; 6] = [
        Color::Cyan,
        Color::Magenta,
        Color::Yellow,
        Color::Blue,
        Color::LightCyan,
        Color::LightMagenta,
    ];
    if message.outgoing {
        return Color::Green;
    }
    let mut hash = DefaultHasher::new();
    message.sender.hash(&mut hash);
    PALETTE[usize::try_from(hash.finish() % 6).unwrap_or_default()]
}

fn attachment_label(attachment: &Attachment, icons: Icons) -> String {
    if attachment.kind == AttachmentKind::Sticker {
        return format!(
            "{}{}  {}",
            icons.attachment(attachment.kind),
            attachment.fallback_emoji.as_deref().unwrap_or("◻"),
            if attachment.preview_uses_thumbnail() {
                "[sticker · static preview]"
            } else {
                "[sticker]"
            }
        );
    }
    let kind = match attachment.kind {
        AttachmentKind::Photo => "photo",
        AttachmentKind::File => "file",
        AttachmentKind::Video => "video",
        AttachmentKind::Audio => "audio",
        AttachmentKind::Sticker => "sticker",
        AttachmentKind::Other => "attachment",
    };
    let mut label = format!("{}[{kind}]", icons.attachment(attachment.kind));
    if let Some(name) = &attachment.file_name {
        label.push(' ');
        label.push_str(name);
    }
    label
}

#[allow(clippy::too_many_arguments)]
fn append_message_actions(
    result: &mut Vec<Line<'static>>,
    message: &Message,
    actions: &[MessageAction],
    selected: bool,
    selected_action: usize,
    body_width: usize,
    continuation: &str,
) -> Vec<(usize, usize)> {
    let mut action_rows = Vec::new();
    for (link_index, link) in message.links.iter().enumerate() {
        let Some(action_index) = actions
            .iter()
            .position(|action| *action == MessageAction::Link(link_index))
        else {
            continue;
        };
        let label = if link.label == link.url {
            format!("↗ {}", link.url)
        } else {
            format!("↗ {} → {}", link.label, link.url)
        };
        push_action_lines(
            result,
            &mut action_rows,
            continuation,
            &label,
            body_width,
            action_index,
            selected && selected_action == action_index,
            true,
        );
    }
    for (button_index, button) in message.buttons.iter().enumerate() {
        let action_index = actions
            .iter()
            .position(|action| *action == MessageAction::Button(button_index));
        let (icon, suffix) = match button.kind {
            MessageButtonKind::Url => ("↗", ""),
            MessageButtonKind::Callback => ("●", ""),
            MessageButtonKind::Game => ("▶", ""),
            MessageButtonKind::Unsupported => ("×", " · graphical client required"),
        };
        let label = format!("{icon} [ {} ]{suffix}", button.label);
        if let Some(action_index) = action_index {
            push_action_lines(
                result,
                &mut action_rows,
                continuation,
                &label,
                body_width,
                action_index,
                selected && selected_action == action_index,
                button.kind == MessageButtonKind::Url,
            );
        } else {
            result.push(Line::from(vec![
                Span::styled(continuation.to_owned(), Style::default().fg(MUTED)),
                Span::styled(label, Style::default().fg(MUTED)),
            ]));
        }
    }
    action_rows
}

#[allow(clippy::too_many_arguments)]
fn push_action_lines(
    lines: &mut Vec<Line<'static>>,
    action_rows: &mut Vec<(usize, usize)>,
    prefix: &str,
    label: &str,
    width: usize,
    action_index: usize,
    selected: bool,
    underlined: bool,
) {
    for part in wrap_cells(label, width) {
        let row = lines.len();
        let mut prefix_style = Style::default().fg(MUTED);
        let mut action_style = Style::default().fg(ACCENT);
        if underlined {
            action_style = action_style.add_modifier(Modifier::UNDERLINED);
        }
        if selected {
            prefix_style = prefix_style.fg(ACCENT).bold();
            action_style = action_style.bold();
        }
        lines.push(Line::from(vec![
            Span::styled(prefix.to_owned(), prefix_style),
            Span::styled(part, action_style),
        ]));
        action_rows.push((row, action_index));
    }
}

pub(super) fn human_size(size: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    if size >= GIB {
        decimal_size(size, GIB, "GiB")
    } else if size >= MIB {
        decimal_size(size, MIB, "MiB")
    } else if size >= KIB {
        decimal_size(size, KIB, "KiB")
    } else {
        format!("{size} B")
    }
}

fn decimal_size(size: u64, unit: u64, suffix: &str) -> String {
    let whole = size / unit;
    let decimal = size % unit * 10 / unit;
    format!("{whole}.{decimal} {suffix}")
}
