use chrono::Local;
use qrcode::{Color as QrColor, QrCode};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, HighlightSpacing, List, ListItem, ListState, Paragraph, Wrap,
};
use ratatui::Frame;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::app::{
    AppState, AttachmentState, AuthPhase, Focus, MessageAction, Mode, QrRenderMode, Screen,
};
use crate::config::DownloadBehavior;
use crate::event::ConnectionStatus;
use crate::input::TextInput;
use crate::model::{AttachmentKind, Delivery, Message, MessageButtonKind};

const ACCENT: Color = Color::Rgb(216, 180, 254);
const MUTED: Color = Color::DarkGray;
const SUCCESS: Color = Color::Rgb(126, 211, 166);
const WARNING: Color = Color::Rgb(245, 194, 107);
const DANGER: Color = Color::Rgb(242, 139, 130);
const MESSAGE_ID_COLUMN_WIDTH: usize = 12;
// Terminal QR renderers such as qr-cli use a compact two-module border. A
// quiet zone is still required for reliable finder-pattern detection, but a
// full four-module print margin makes short-lived login tokens unnecessarily
// large in an 80 x 24 terminal.
const QR_QUIET_ZONE: usize = 2;

pub fn render(frame: &mut Frame<'_>, app: &mut AppState) {
    let area = frame.area();
    app.clear_message_hit_regions();
    if area.width < 40 || area.height < 10 {
        frame.render_widget(
            Paragraph::new("Terminal too small\nminimum 40 × 10")
                .alignment(Alignment::Center)
                .style(Style::default().fg(WARNING)),
            area,
        );
        return;
    }

    match &app.screen {
        Screen::Connecting => render_connecting(frame, area, app),
        Screen::Auth(phase) => render_auth(frame, area, app, phase),
        Screen::Main => render_main(frame, area, app),
        Screen::Fatal(message) => render_fatal(frame, area, app, message),
    }
}

fn render_connecting(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    let spinner = spinner(app.tick);
    let mut lines = vec![
        Line::from(Span::styled(
            format!("Termgram · Account {}", app.active_account()),
            Style::default().fg(ACCENT).bold(),
        )),
        Line::from(""),
        Line::from(format!("{spinner}  Connecting to Telegram…")),
        Line::from(Span::styled(
            "F2 next account · F3 add account · Ctrl+C quits",
            Style::default().fg(MUTED),
        )),
    ];
    if let Some(message) = &app.status_message {
        lines.push(Line::from(Span::styled(
            message.as_str(),
            Style::default().fg(WARNING),
        )));
    }
    let height = clamp_u16(lines.len());
    let body = Paragraph::new(lines).alignment(Alignment::Center);
    frame.render_widget(body, vertically_centered(area, height));
}

fn render_auth(frame: &mut Frame<'_>, area: Rect, app: &AppState, phase: &AuthPhase) {
    if let AuthPhase::Qr { url } = phase {
        render_qr_auth(frame, area, app, url);
        return;
    }
    let width = area.width.min(72);
    let height = 14_u16.min(area.height.saturating_sub(2));
    let popup = centered(area, width, height);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(ACCENT))
            .title(format!(
                " Termgram · Account {} · Sign in ",
                app.active_account()
            )),
        popup,
    );
    let inner = popup.inner(ratatui::layout::Margin {
        vertical: 1,
        horizontal: 2,
    });
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Min(2),
        Constraint::Length(1),
    ])
    .split(inner);

    let (title, detail, masked): (&str, String, bool) = match phase {
        AuthPhase::Phone => (
            "Phone number",
            "Use international format, for example +81 90 1234 5678.".to_owned(),
            false,
        ),
        AuthPhase::Qr { .. } => unreachable!("QR authentication has a dedicated view"),
        AuthPhase::Code { phone } => (
            "Login code",
            format!("Telegram sent a code for {phone}."),
            false,
        ),
        AuthPhase::Password { hint } => (
            "Two-step verification",
            hint.clone()
                .unwrap_or_else(|| "Enter your Telegram 2FA password.".to_owned()),
            true,
        ),
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(title, Style::default().bold())),
            Line::from(Span::styled(detail, Style::default().fg(MUTED))),
        ])
        .wrap(Wrap { trim: true }),
        chunks[0],
    );
    render_input(
        frame,
        chunks[1],
        app.auth_input(),
        masked,
        if app.auth_is_submitting() {
            "Submitted"
        } else {
            "Enter to continue"
        },
        !app.auth_is_submitting(),
    );
    if let Some(message) = &app.status_message {
        frame.render_widget(
            Paragraph::new(message.as_str()).style(Style::default().fg(DANGER)),
            chunks[2],
        );
    } else if let Some(message) = app.auth_progress_label() {
        frame.render_widget(
            Paragraph::new(format!("{}  {message}", spinner(app.tick)))
                .style(Style::default().fg(WARNING)),
            chunks[2],
        );
    }
    let footer = if matches!(phase, AuthPhase::Phone) {
        "Tab QR · F2 next account · F3 add account · Ctrl+C quits"
    } else {
        "Esc starts over · F2 next account · F3 add account · Ctrl+C quits"
    };
    frame.render_widget(
        Paragraph::new(footer).style(Style::default().fg(MUTED)),
        chunks[3],
    );
}

fn render_qr_auth(frame: &mut Frame<'_>, area: Rect, app: &AppState, url: &str) {
    let Ok(code) = QrCode::new(url.as_bytes()) else {
        render_qr_unavailable(
            frame,
            area,
            "Could not render this QR code. Press Esc to use your phone number.",
        );
        return;
    };

    let modules = code.width().saturating_add(QR_QUIET_ZONE * 2);
    let mode = app.qr_render_mode();
    let (qr_width, qr_height) = qr_dimensions(modules, mode);
    let required_height = qr_height.saturating_add(1);
    if area.width < qr_width || area.height < required_height {
        let (mode_name, alternative) = match mode {
            QrRenderMode::Compact => ("Compact", "full-cell"),
            QrRenderMode::Compatible => ("Full-cell", "compact"),
        };
        let message = format!(
            "{mode_name} QR needs at least {qr_width} × {required_height} terminal cells. Resize, press Tab for {alternative} mode, or Esc for phone sign-in."
        );
        render_qr_unavailable(frame, area, &message);
        return;
    }

    let view_y = area
        .y
        .saturating_add(area.height.saturating_sub(required_height) / 2);
    let (guidance, style) = if let Some(message) = &app.status_message {
        (
            format!("{message} · Tab display · F2 next · F3 add · Esc phone"),
            Style::default().fg(DANGER),
        )
    } else {
        let alternative = match mode {
            QrRenderMode::Compact => "full",
            QrRenderMode::Compatible => "compact",
        };
        (
            format!(
                "{} Telegram: Devices→Link Desktop · Tab {alternative} · F2 next · F3 add · Esc phone",
                spinner(app.tick),
            ),
            Style::default().fg(WARNING).bold(),
        )
    };
    frame.render_widget(
        Paragraph::new(guidance)
            .alignment(Alignment::Center)
            .style(style),
        Rect::new(area.x, view_y, area.width, 1),
    );
    let qr_x = area
        .x
        .saturating_add(area.width.saturating_sub(qr_width) / 2);
    render_qr_code(
        frame,
        Rect::new(qr_x, view_y.saturating_add(1), qr_width, qr_height),
        &code,
        QR_QUIET_ZONE,
        mode,
    );
}

fn render_qr_unavailable(frame: &mut Frame<'_>, area: Rect, message: &str) {
    let popup = centered(area, area.width.min(72), area.height.min(10));
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Termgram · QR sign in",
                Style::default().fg(ACCENT).bold(),
            )),
            Line::from(""),
            Line::from(message),
            Line::from(""),
            Line::from(Span::styled("Ctrl+C quits", Style::default().fg(MUTED))),
        ])
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true }),
        popup,
    );
}

fn qr_dimensions(modules: usize, mode: QrRenderMode) -> (u16, u16) {
    match mode {
        QrRenderMode::Compact => (clamp_u16(modules), clamp_u16(modules.saturating_add(1) / 2)),
        QrRenderMode::Compatible => (clamp_u16(modules), clamp_u16(modules)),
    }
}

fn render_qr_code(
    frame: &mut Frame<'_>,
    area: Rect,
    code: &QrCode,
    quiet_zone: usize,
    mode: QrRenderMode,
) {
    match mode {
        QrRenderMode::Compact => render_compact_qr_code(frame, area, code, quiet_zone),
        QrRenderMode::Compatible => render_compatible_qr_code(frame, area, code, quiet_zone),
    }
}

/// Pair two QR rows into each terminal row using the same four-glyph mapping
/// as established terminal QR tools: space, upper half, lower half, and full
/// block. A single black foreground on a white background avoids depending on
/// the user's terminal theme or on background-colored half-block tricks.
fn render_compact_qr_code(frame: &mut Frame<'_>, area: Rect, code: &QrCode, quiet_zone: usize) {
    let modules = code.width().saturating_add(quiet_zone * 2);
    let mut lines = Vec::with_capacity(modules.saturating_add(1) / 2);
    for top_y in (0..modules).step_by(2) {
        let mut spans = Vec::with_capacity(modules);
        for x in 0..modules {
            let top = qr_module(code, x, top_y, quiet_zone);
            let bottom = if top_y + 1 < modules {
                qr_module(code, x, top_y + 1, quiet_zone)
            } else {
                QrColor::Light
            };
            spans.push(Span::styled(
                qr_pair_symbol(top, bottom),
                Style::default()
                    .fg(Color::Rgb(0, 0, 0))
                    .bg(Color::Rgb(255, 255, 255)),
            ));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// Draw every QR module as one background-colored ASCII space. Terminal font
/// block-glyph support is irrelevant and the fallback stays reasonably sized;
/// scanners compensate for the terminal cell's rectangular aspect ratio.
fn render_compatible_qr_code(frame: &mut Frame<'_>, area: Rect, code: &QrCode, quiet_zone: usize) {
    let modules = code.width().saturating_add(quiet_zone * 2);
    let mut lines = Vec::with_capacity(modules);
    for y in 0..modules {
        let mut spans = Vec::with_capacity(modules);
        for x in 0..modules {
            let color = qr_terminal_color(qr_module(code, x, y, quiet_zone));
            spans.push(Span::styled(" ", Style::default().fg(color).bg(color)));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn qr_module(code: &QrCode, x: usize, y: usize, quiet_zone: usize) -> QrColor {
    let data_x = x.checked_sub(quiet_zone);
    let data_y = y.checked_sub(quiet_zone);
    match (data_x, data_y) {
        (Some(data_x), Some(data_y)) if data_x < code.width() && data_y < code.width() => {
            code[(data_x, data_y)]
        }
        _ => QrColor::Light,
    }
}

const fn qr_terminal_color(color: QrColor) -> Color {
    match color {
        QrColor::Dark => Color::Rgb(0, 0, 0),
        QrColor::Light => Color::Rgb(255, 255, 255),
    }
}

const fn qr_pair_symbol(top: QrColor, bottom: QrColor) -> &'static str {
    match (top, bottom) {
        (QrColor::Dark, QrColor::Dark) => "█",
        (QrColor::Dark, QrColor::Light) => "▀",
        (QrColor::Light, QrColor::Dark) => "▄",
        (QrColor::Light, QrColor::Light) => " ",
    }
}

fn render_main(frame: &mut Frame<'_>, area: Rect, app: &mut AppState) {
    let narrow = area.width < 96;
    let show_conversation_only = narrow && app.narrow_conversation && app.active_chat_id.is_some();
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(composer_height(app, area.width)),
        Constraint::Length(1),
    ])
    .split(area);

    render_header(frame, rows[0], app);
    if show_conversation_only {
        render_conversation(frame, rows[1], app);
    } else if narrow {
        render_chats(frame, rows[1], app);
    } else {
        let panes = Layout::horizontal([Constraint::Percentage(32), Constraint::Percentage(68)])
            .split(rows[1]);
        render_chats(frame, panes[0], app);
        render_conversation(frame, panes[1], app);
    }
    render_composer(frame, rows[2], app, show_conversation_only || !narrow);
    render_footer(frame, rows[3], app, narrow);

    if app.mode == Mode::Help {
        render_help(frame, area);
    } else if app.mode == Mode::Settings {
        render_settings(frame, area, app);
    } else if app.mode == Mode::Accounts {
        render_accounts(frame, area, app);
    }
}

fn render_header(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    let (label, color) = match app.connection {
        ConnectionStatus::Connecting => (format!("{} connecting", spinner(app.tick)), WARNING),
        ConnectionStatus::Online => ("● online".to_owned(), SUCCESS),
        ConnectionStatus::Reconnecting => (format!("{} reconnecting", spinner(app.tick)), WARNING),
        ConnectionStatus::Offline => ("● offline".to_owned(), DANGER),
    };
    let user = app.user_name.as_deref().unwrap_or("Telegram");
    let right_width = clamp_u16(UnicodeWidthStr::width(label.as_str()));
    let chunks = Layout::horizontal([
        Constraint::Min(1),
        Constraint::Length(right_width.saturating_add(1)),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" Termgram", Style::default().fg(ACCENT).bold()),
            Span::styled(
                format!(" · Account {}", app.active_account()),
                Style::default().fg(MUTED),
            ),
            Span::styled(format!("  {user}"), Style::default().fg(MUTED)),
        ])),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new(label)
            .alignment(Alignment::Right)
            .style(Style::default().fg(color)),
        chunks[1],
    );
}

fn render_chats(frame: &mut Frame<'_>, area: Rect, app: &mut AppState) {
    app.set_chat_pane_region((area.x, area.right(), area.y, area.bottom()));
    let focused = app.focus == Focus::Chats && app.mode != Mode::Compose;
    let title = match app.mode {
        Mode::Filter => format!(" Chats · /{} ", app.filter.value()),
        _ => " Chats ".to_owned(),
    };
    let block = pane_block(title, focused);
    let visible = app.filtered_chat_indices();
    let viewport_height = usize::from(area.height.saturating_sub(2)).max(1);
    let selected = app.selected_chat.min(visible.len().saturating_sub(1));
    let viewport_start = selected.saturating_add(1).saturating_sub(viewport_height);
    let viewport_end = viewport_start
        .saturating_add(viewport_height)
        .min(visible.len());
    let now = Local::now();
    let items = visible[viewport_start..viewport_end]
        .iter()
        .enumerate()
        .map(|(offset, &index)| {
            let chat = &app.chats[index];
            let selected = viewport_start.saturating_add(offset) == app.selected_chat;
            let unread = if chat.unread > 0 {
                format!(" {}", chat.unread)
            } else {
                String::new()
            };
            let time = chat.activity_label(now);
            let width = usize::from(area.width.saturating_sub(4));
            let suffix_width =
                UnicodeWidthStr::width(time.as_str()) + UnicodeWidthStr::width(unread.as_str());
            let title = truncate_cells(
                &chat.title,
                width.saturating_sub(suffix_width).saturating_sub(1),
            );
            let gap = width
                .saturating_sub(UnicodeWidthStr::width(title.as_str()))
                .saturating_sub(suffix_width)
                .max(1);
            let style = if selected {
                Style::default().fg(Color::Black).bg(ACCENT).bold()
            } else if chat.unread > 0 {
                Style::default().bold()
            } else {
                Style::default()
            };
            ListItem::new(Line::from(vec![
                Span::styled(title, style),
                Span::styled(" ".repeat(gap), style),
                Span::styled(time, style),
                Span::styled(unread, style),
            ]))
        });
    let list = if visible.is_empty() {
        List::new(vec![ListItem::new(Span::styled(
            if app.mode == Mode::Filter {
                "  No chats match"
            } else {
                "  No conversations"
            },
            Style::default().fg(MUTED),
        ))])
    } else {
        List::new(items.collect::<Vec<_>>())
    };
    let mut state = ListState::default()
        .with_selected((!visible.is_empty()).then_some(selected.saturating_sub(viewport_start)));
    let hit_regions = (viewport_start..viewport_end)
        .enumerate()
        .map(|(offset, position)| {
            (
                area.x.saturating_add(1),
                area.right().saturating_sub(1),
                area.y.saturating_add(1).saturating_add(clamp_u16(offset)),
                position,
            )
        })
        .collect();
    app.set_chat_hit_regions(hit_regions);
    frame.render_stateful_widget(
        list.block(block)
            .highlight_symbol("› ")
            .highlight_spacing(HighlightSpacing::Always)
            .highlight_style(Style::default().fg(Color::Black).bg(ACCENT).bold()),
        area,
        &mut state,
    );
}

#[allow(clippy::too_many_lines)]
fn render_conversation(frame: &mut Frame<'_>, area: Rect, app: &mut AppState) {
    app.set_conversation_pane_region((area.x, area.right(), area.y, area.bottom()));
    let title = app.active_chat().map_or_else(
        || " Conversation ".to_owned(),
        |chat| format!(" {} ", chat.title),
    );
    let block = pane_block(
        title,
        app.focus == Focus::Conversation || app.mode == Mode::Compose,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(chat_id) = app.active_chat_id else {
        app.set_message_hit_regions(Vec::new());
        frame.render_widget(
            Paragraph::new("Select a chat and press Enter")
                .alignment(Alignment::Center)
                .style(Style::default().fg(MUTED)),
            vertically_centered(inner, 1),
        );
        return;
    };
    let messages: &[Message] = app.messages.get(&chat_id).map_or(&[], Vec::as_slice);
    if messages.is_empty() {
        app.set_message_hit_regions(Vec::new());
        app.message_scroll = 0;
        app.new_messages_while_scrolled = 0;
        app.new_messages_to_anchor = 0;
        let text = if app.loading_history {
            format!("{}  Loading messages…", spinner(app.tick))
        } else {
            "No messages yet — press i to write one".to_owned()
        };
        frame.render_widget(
            Paragraph::new(text)
                .alignment(Alignment::Center)
                .style(Style::default().fg(MUTED)),
            vertically_centered(inner, 1),
        );
        return;
    }

    let message_width = usize::from(inner.width.saturating_sub(2).max(1));
    let mut lines = Vec::new();
    let mut layouts = Vec::with_capacity(messages.len());
    for message in messages {
        let start = lines.len();
        let actions = app.message_actions(message);
        let rendered = message_lines(
            message,
            message_width,
            app.attachment_state(chat_id, message.id),
            app.settings().download_behavior,
            app.selected_message == Some(message.id),
            app.selected_action,
            app.settings().show_message_ids,
            &actions,
        );
        let body_action = rendered.body_action;
        let body_height = rendered.body_height;
        let hit_rows = rendered
            .action_rows
            .into_iter()
            .map(|(row, action)| (start.saturating_add(row), Some(action)))
            .chain((0..body_height).map(move |row| (start.saturating_add(row), body_action)))
            .collect();
        lines.extend(rendered.lines);
        layouts.push(MessageLayout {
            id: message.id,
            start,
            height: lines.len().saturating_sub(start),
            hit_rows,
        });
    }
    let available = usize::from(inner.height);
    let max_scroll = lines.len().saturating_sub(available);
    // Convert each new entry to its actual rendered row height once. Keeping
    // that pending count separate from the badge count prevents every redraw
    // from moving an already-anchored viewport again.
    let rows_to_anchor = layouts
        .iter()
        .rev()
        .take(app.new_messages_to_anchor.min(layouts.len()))
        .map(|layout| layout.height)
        .sum::<usize>();
    app.new_messages_to_anchor = 0;
    app.message_scroll = app
        .message_scroll
        .saturating_add(rows_to_anchor)
        .min(max_scroll);
    let mut scroll = max_scroll.saturating_sub(app.message_scroll);
    if let Some(anchor_id) = app.viewport_anchor_message {
        if let Some(anchor_start) = layouts
            .iter()
            .find(|layout| layout.id == anchor_id)
            .map(|layout| layout.start)
        {
            scroll = anchor_start
                .saturating_add(app.viewport_anchor_row)
                .min(max_scroll);
            app.message_scroll = max_scroll.saturating_sub(scroll);
        } else {
            app.viewport_anchor_message = None;
            app.viewport_anchor_row = 0;
        }
    }
    if app.message_scroll > 0 {
        if let Some(layout) = layouts
            .iter()
            .find(|layout| scroll < layout.start.saturating_add(layout.height))
        {
            app.viewport_anchor_message = Some(layout.id);
            app.viewport_anchor_row = scroll.saturating_sub(layout.start);
        } else if let Some(layout) = layouts.last() {
            app.viewport_anchor_message = Some(layout.id);
            app.viewport_anchor_row = 0;
        }
    } else {
        app.viewport_anchor_message = None;
        app.viewport_anchor_row = 0;
    }
    let visible_lines = lines
        .into_iter()
        .skip(scroll)
        .take(available)
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(Text::from(visible_lines)), inner);
    let hit_regions = layouts
        .iter()
        .flat_map(|layout| {
            layout.hit_rows.iter().filter_map(move |&(row, action)| {
                (row >= scroll && row < scroll.saturating_add(available)).then_some((
                    inner.x,
                    inner.right(),
                    inner
                        .y
                        .saturating_add(clamp_u16(row.saturating_sub(scroll))),
                    (layout.id, action),
                ))
            })
        })
        .collect();
    app.set_message_hit_regions(hit_regions);
    render_new_message_badge(frame, inner, app.new_messages_while_scrolled);
}

struct MessageLayout {
    id: i32,
    start: usize,
    height: usize,
    hit_rows: Vec<(usize, Option<usize>)>,
}

fn render_new_message_badge(frame: &mut Frame<'_>, area: Rect, count: usize) {
    if count == 0 {
        return;
    }
    let label = format!(" ↓ {count} new ");
    let width = clamp_u16(UnicodeWidthStr::width(label.as_str()));
    let badge = Rect::new(
        area.right().saturating_sub(width),
        area.bottom().saturating_sub(1),
        width,
        1,
    );
    frame.render_widget(
        Paragraph::new(label).style(Style::default().fg(Color::Black).bg(ACCENT)),
        badge,
    );
}

fn render_composer(frame: &mut Frame<'_>, area: Rect, app: &AppState, enabled: bool) {
    let active = app.mode == Mode::Compose;
    let title = if !enabled || app.active_chat_id.is_none() {
        " Message · select a chat ".to_owned()
    } else if let Some(reply) = app.active_reply_target() {
        let sender = reply.sender.as_deref().unwrap_or("unknown");
        if active {
            format!(
                " Reply to #{} {sender} · Enter send · Esc cancel ",
                reply.message_id
            )
        } else {
            format!(" Reply draft to #{} {sender} · i resume ", reply.message_id)
        }
    } else if active {
        " Message · Enter send · Esc keep draft ".to_owned()
    } else {
        " Message · i to compose ".to_owned()
    };
    let block = pane_block(title, active);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if !enabled || app.active_chat_id.is_none() {
        frame.render_widget(
            Paragraph::new("Open a conversation to write a message")
                .style(Style::default().fg(MUTED)),
            inner,
        );
        return;
    }
    let empty = TextInput::new();
    let input = app.active_draft().unwrap_or(&empty);
    let (row, column) = input_cursor(input, inner.width.max(1));
    let vertical_scroll = row.saturating_sub(inner.height.saturating_sub(1));
    let placeholder = input.is_empty() && !active;
    let text = if placeholder {
        Text::from("Write a message…")
    } else {
        Text::from(
            editor_lines(input.value(), inner.width.max(1))
                .into_iter()
                .map(Line::from)
                .collect::<Vec<_>>(),
        )
    };
    frame.render_widget(
        Paragraph::new(text)
            .style(if placeholder {
                Style::default().fg(MUTED)
            } else {
                Style::default()
            })
            .scroll((vertical_scroll, 0)),
        inner,
    );
    if active {
        let x = inner
            .x
            .saturating_add(column)
            .min(inner.right().saturating_sub(1));
        let visible_row = row.saturating_sub(vertical_scroll);
        let y = inner
            .y
            .saturating_add(visible_row)
            .min(inner.bottom().saturating_sub(1));
        frame.set_cursor_position(Position::new(x, y));
    }
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &AppState, narrow: bool) {
    let content = if let Some(message) = &app.status_message {
        Line::from(Span::styled(
            format!(" {message}"),
            Style::default().fg(WARNING),
        ))
    } else if let Some(version) = app.available_update() {
        Line::from(Span::styled(
            format!(" Update {version} available · run tg update"),
            Style::default().fg(SUCCESS),
        ))
    } else if app.mode == Mode::Compose {
        let prefix = app.active_reply_target().map_or_else(String::new, |reply| {
            format!(" Reply #{}  · ", reply.message_id)
        });
        Line::from(format!(
            "{prefix} Enter send  ·  drop files to attach  ·  Ctrl+J newline  ·  Esc"
        ))
    } else if app.mode == Mode::Settings {
        Line::from(" ↑↓ select  ·  Enter toggle  ·  Esc close settings")
    } else if app.mode == Mode::Accounts {
        Line::from(" ↑↓ select  ·  Enter switch/add  ·  Esc close accounts")
    } else if narrow && app.narrow_conversation {
        Line::from(" [/ ] select · R reply · o/O action · r target · Esc chats")
    } else {
        Line::from(" ↑↓/jk move · [/] select · R reply · o/O action · r target · ? help · q quit")
    };
    frame.render_widget(
        Paragraph::new(content).style(Style::default().fg(MUTED)),
        area,
    );
}

fn render_help(frame: &mut Frame<'_>, area: Rect) {
    let popup = centered(area, area.width.min(72), area.height.min(27));
    frame.render_widget(Clear, popup);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT))
        .title(" Keyboard shortcuts ");
    let lines = vec![
        Line::from(vec![
            Span::styled("Navigation", Style::default().bold()),
            Span::raw("  ↑↓ or j/k · Enter opens"),
        ]),
        Line::from("Tab             switch chats / conversation"),
        Line::from("PgUp/PgDn/Home/End  scroll / oldest / latest"),
        Line::from("o/O + Enter     select and activate message actions"),
        Line::from("[ / ]           select older / newer message (latest first)"),
        Line::from("R               compose a reply to selected/latest message"),
        Line::from("r               jump to a selected reply's target"),
        Line::from("Right click     reply to a message under the pointer"),
        Line::from("l               follow selected/first link in a message"),
        Line::from("/               filter chats (from chat list)"),
        Line::from("s / a           settings / accounts"),
        Line::from("F2 / F3         next / add account"),
        Line::from(""),
        Line::from(vec![
            Span::styled("Writing", Style::default().bold()),
            Span::raw("     i or Enter starts"),
        ]),
        Line::from("Enter / Ctrl+J  send / insert a new line"),
        Line::from("Ctrl+A/E/W/U    start / end / delete word / clear"),
        Line::from("Esc             cancel reply, then keep draft and leave"),
        Line::from("Drop / paste    send existing local file paths"),
        Line::from(""),
        Line::from("? / Esc         close help"),
        Line::from("q / Ctrl+C      quit"),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(Color::White)),
        popup,
    );
}

fn render_settings(frame: &mut Frame<'_>, area: Rect, app: &mut AppState) {
    let popup = centered(area, area.width.min(68), 17_u16.min(area.height));
    frame.render_widget(Clear, popup);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT))
        .title(" Termgram settings ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let settings = app.settings();
    let rows = [
        (
            "Automatic update checks",
            if settings.automatic_update_checks {
                "On"
            } else {
                "Off"
            },
        ),
        ("Release channel", settings.release_channel.label()),
        ("Downloads", settings.download_behavior.label()),
        (
            "Message ID column",
            if settings.show_message_ids {
                "Shown"
            } else {
                "Hidden"
            },
        ),
    ];
    let width = usize::from(inner.width.saturating_sub(4));
    let mut lines = vec![
        Line::from(Span::styled(
            "Only non-sensitive preferences are stored locally.",
            Style::default().fg(MUTED),
        )),
        Line::from(""),
    ];
    let mut hit_regions = Vec::with_capacity(rows.len());
    let mut row = inner.y.saturating_add(2);
    for (index, (label, value)) in rows.into_iter().enumerate() {
        let selected = index == app.settings_selection();
        let prefix = if selected { "› " } else { "  " };
        let content_width = width.saturating_sub(UnicodeWidthStr::width(prefix));
        let gap = content_width
            .saturating_sub(UnicodeWidthStr::width(label))
            .saturating_sub(UnicodeWidthStr::width(value))
            .max(1);
        let style = if selected {
            Style::default().fg(Color::Black).bg(ACCENT).bold()
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(
            format!("{prefix}{label}{}{value}", " ".repeat(gap)),
            style,
        )));
        hit_regions.push((inner.x, inner.right(), row, index));
        row = row.saturating_add(1);
        if index == 2 {
            lines.push(Line::from(Span::styled(
                match settings.download_behavior {
                    DownloadBehavior::TempOnly => {
                        "  Temp download only; Termgram never reveals files."
                    }
                    DownloadBehavior::RevealOnActivation => {
                        "  Second activation reveals; Termgram never executes files."
                    }
                },
                Style::default().fg(MUTED),
            )));
            row = row.saturating_add(1);
        }
    }
    lines.extend([
        Line::from(""),
        Line::from(Span::styled(
            "↑↓/j/k select · Enter/Space toggle · Esc close",
            Style::default().fg(MUTED),
        )),
    ]);
    app.set_settings_hit_regions(hit_regions);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn render_accounts(frame: &mut Frame<'_>, area: Rect, app: &mut AppState) {
    let row_count = u16::from(app.account_count()).saturating_add(7);
    let popup = centered(area, area.width.min(58), row_count.min(area.height));
    frame.render_widget(Clear, popup);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT))
        .title(" Telegram accounts ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let mut lines = vec![
        Line::from(Span::styled(
            "Only the selected account stays connected.",
            Style::default().fg(MUTED),
        )),
        Line::from(""),
    ];
    let mut hit_regions = Vec::with_capacity(usize::from(app.account_count()) + 1);
    for account in 1..=app.account_count() {
        let selected = app.account_selection() == usize::from(account - 1);
        let active = account == app.active_account();
        let label = if active {
            format!(
                "Account {account}  ·  {}  (active)",
                app.user_name.as_deref().unwrap_or("signing in")
            )
        } else {
            format!("Account {account}")
        };
        lines.push(account_row(&label, selected));
        hit_regions.push((
            inner.x,
            inner.right(),
            inner.y.saturating_add(1).saturating_add(u16::from(account)),
            usize::from(account - 1),
        ));
    }
    let add_selected = app.account_selection() == usize::from(app.account_count());
    let add_label = if app.account_count() < crate::config::MAX_ACCOUNTS {
        "+ Add account".to_owned()
    } else {
        format!("Account limit reached ({})", crate::config::MAX_ACCOUNTS)
    };
    lines.push(account_row(&add_label, add_selected));
    hit_regions.push((
        inner.x,
        inner.right(),
        inner
            .y
            .saturating_add(2)
            .saturating_add(u16::from(app.account_count())),
        usize::from(app.account_count()),
    ));
    lines.extend([
        Line::from(""),
        Line::from(Span::styled(
            "↑↓/j/k select · Enter switch · 1-8 direct · Esc close",
            Style::default().fg(MUTED),
        )),
    ]);
    app.set_account_hit_regions(hit_regions);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn account_row(label: &str, selected: bool) -> Line<'static> {
    let prefix = if selected { "› " } else { "  " };
    let style = if selected {
        Style::default().fg(Color::Black).bg(ACCENT).bold()
    } else {
        Style::default()
    };
    Line::from(Span::styled(format!("{prefix}{label}"), style))
}

fn render_fatal(frame: &mut Frame<'_>, area: Rect, app: &AppState, message: &str) {
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Termgram stopped",
                Style::default().fg(DANGER).bold(),
            )),
            Line::from(""),
            Line::from(message),
            Line::from(""),
            Line::from(Span::styled(
                format!(
                    "Account {} · F2 next · F3 add · q/Ctrl+C quit",
                    app.active_account()
                ),
                Style::default().fg(MUTED),
            )),
        ])
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true }),
        vertically_centered(area, 7),
    );
}

fn render_input(
    frame: &mut Frame<'_>,
    area: Rect,
    input: &TextInput,
    masked: bool,
    title: &str,
    enabled: bool,
) {
    let value = if masked {
        "•".repeat(input.grapheme_count())
    } else {
        input.value().to_owned()
    };
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if enabled { ACCENT } else { MUTED }))
        .title(format!(" {title} "));
    let inner = block.inner(area);
    frame.render_widget(Paragraph::new(value).block(block), area);
    if enabled {
        let cursor = if masked {
            clamp_u16(input.cursor_grapheme())
        } else {
            clamp_u16(input.cursor_display_width())
        };
        frame.set_cursor_position(Position::new(
            inner
                .x
                .saturating_add(cursor)
                .min(inner.right().saturating_sub(1)),
            inner.y,
        ));
    }
}

struct RenderedMessage {
    lines: Vec<Line<'static>>,
    body_height: usize,
    body_action: Option<usize>,
    action_rows: Vec<(usize, usize)>,
}

#[allow(clippy::too_many_arguments)]
fn message_lines(
    message: &Message,
    width: usize,
    attachment_state: AttachmentState,
    download_behavior: DownloadBehavior,
    selected: bool,
    selected_action: usize,
    show_message_ids: bool,
    actions: &[MessageAction],
) -> RenderedMessage {
    let time = message
        .timestamp
        .with_timezone(&Local)
        .format("%H:%M")
        .to_string();
    let sender = pad_cells(&truncate_cells(&message.sender, 12), 12);
    let prefix = format!("{time} {sender} │ ");
    let prefix_width = UnicodeWidthStr::width(prefix.as_str());
    let delivery_width = if message.outgoing { 3 } else { 0 };
    let id_reserve = usize::from(show_message_ids) * MESSAGE_ID_COLUMN_WIDTH;
    let body_width = width
        .saturating_sub(prefix_width)
        .saturating_sub(delivery_width)
        .saturating_sub(id_reserve)
        .max(8);
    let mut body = message_body(message, attachment_state, download_behavior);
    if let Some(reply) = &message.reply_to {
        let sender = reply.sender.as_deref().unwrap_or("unknown");
        body = format!("↩ #{} {}  {body}", reply.message_id, sender);
    }
    let wrapped = wrap_cells(&body, body_width);
    let mut result = Vec::new();
    for (index, part) in wrapped.into_iter().enumerate() {
        let line_prefix = if index == 0 {
            prefix.clone()
        } else {
            format!("{}│ ", " ".repeat(prefix_width.saturating_sub(2)))
        };
        let selection = selected.then_some(Color::DarkGray);
        let mut body_style = if message.outgoing {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };
        if let Some(background) = selection {
            body_style = body_style.bg(background);
        }
        let mut prefix_style = Style::default().fg(MUTED);
        if let Some(background) = selection {
            prefix_style = prefix_style.bg(background);
        }
        let mut spans = vec![
            Span::styled(line_prefix, prefix_style),
            Span::styled(part, body_style),
        ];
        if index == 0 && message.outgoing {
            let (mark, color) = match message.delivery {
                Delivery::Pending => (" …", WARNING),
                Delivery::Sent => (" ✓", MUTED),
                Delivery::Read => (" ✓✓", SUCCESS),
                Delivery::Failed => (" !", DANGER),
            };
            let mut delivery_style = Style::default().fg(color);
            if let Some(background) = selection {
                delivery_style = delivery_style.bg(background);
            }
            spans.push(Span::styled(mark, delivery_style));
        }
        let mut line = Line::from(spans);
        if show_message_ids && index == 0 {
            let label = format!("#{}", message.id);
            let gap = width
                .saturating_sub(line.width())
                .saturating_sub(UnicodeWidthStr::width(label.as_str()));
            line.spans.push(Span::raw(" ".repeat(gap)));
            line.spans.push(Span::styled(label, prefix_style));
        }
        result.push(line);
    }
    let body_height = result.len();
    let body_action = actions
        .iter()
        .position(|action| matches!(action, MessageAction::Attachment | MessageAction::Reply));
    let continuation = format!("{}│ ", " ".repeat(prefix_width.saturating_sub(2)));
    let action_rows = append_message_actions(
        &mut result,
        message,
        actions,
        selected,
        selected_action,
        body_width,
        &continuation,
    );
    RenderedMessage {
        lines: result,
        body_height,
        body_action,
        action_rows,
    }
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
        let background = selected.then_some(Color::DarkGray);
        let mut prefix_style = Style::default().fg(MUTED);
        let mut action_style = Style::default().fg(ACCENT);
        if underlined {
            action_style = action_style.add_modifier(Modifier::UNDERLINED);
        }
        if let Some(background) = background {
            prefix_style = prefix_style.bg(background);
            action_style = action_style.bg(background);
        }
        lines.push(Line::from(vec![
            Span::styled(prefix.to_owned(), prefix_style),
            Span::styled(part, action_style),
        ]));
        action_rows.push((row, action_index));
    }
}

fn message_body(
    message: &Message,
    state: AttachmentState,
    download_behavior: DownloadBehavior,
) -> String {
    let Some(attachment) = &message.attachment else {
        return message.text.clone();
    };
    if attachment.kind == AttachmentKind::Sticker {
        let fallback = attachment.fallback_emoji.as_deref().unwrap_or("◻");
        return if message.text.is_empty() {
            format!("{fallback}  [sticker]")
        } else {
            format!("{fallback}  {}", message.text)
        };
    }

    let kind = match attachment.kind {
        AttachmentKind::Photo => "photo",
        AttachmentKind::File => "file",
        AttachmentKind::Video => "video",
        AttachmentKind::Audio => "audio",
        AttachmentKind::Sticker => "sticker",
        AttachmentKind::Other => "attachment",
    };
    let mut label = format!("[{kind}]");
    if let Some(name) = &attachment.file_name {
        label.push(' ');
        label.push_str(name);
    }
    if let Some(size) = attachment.size {
        label.push_str(" · ");
        label.push_str(&human_size(size));
    }
    let action = match state {
        AttachmentState::Ready => "click/Enter to download",
        AttachmentState::Downloading => "downloading…",
        AttachmentState::Downloaded => match download_behavior {
            DownloadBehavior::TempOnly => "downloaded to temp",
            DownloadBehavior::RevealOnActivation => "click/Enter to reveal",
        },
    };
    label.push_str(" · ");
    label.push_str(action);
    if message.text.is_empty() {
        label
    } else {
        format!("{label}\n{}", message.text)
    }
}

fn human_size(size: u64) -> String {
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

fn pane_block<'a>(title: String, focused: bool) -> Block<'a> {
    Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if focused { ACCENT } else { MUTED }))
        .title(title)
}

fn composer_height(app: &AppState, width: u16) -> u16 {
    if app.active_chat_id.is_none() {
        return 3;
    }
    let usable = width.saturating_sub(2).max(1);
    let lines = app
        .active_draft()
        .map_or(1, |draft| editor_lines(draft.value(), usable).len());
    u16::try_from(lines.clamp(1, 4))
        .unwrap_or(4)
        .saturating_add(2)
}

fn editor_lines(value: &str, width: u16) -> Vec<String> {
    let width = usize::from(width.max(1));
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut column = 0_usize;

    for grapheme in value.graphemes(true) {
        if grapheme == "\n" {
            lines.push(current);
            current = String::new();
            column = 0;
            continue;
        }

        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if column.saturating_add(grapheme_width) > width && !current.is_empty() {
            lines.push(current);
            current = String::new();
            column = 0;
        }
        current.push_str(grapheme);
        column = column.saturating_add(grapheme_width);
        if column >= width {
            lines.push(current);
            current = String::new();
            column %= width;
        }
    }
    lines.push(current);
    lines
}

fn input_cursor(input: &TextInput, width: u16) -> (u16, u16) {
    let before = &input.value()[..input.cursor()];
    let mut row = 0_u16;
    let mut column = 0_u16;
    for grapheme in before.graphemes(true) {
        if grapheme == "\n" {
            row = row.saturating_add(1);
            column = 0;
            continue;
        }
        let cell_width = clamp_u16(UnicodeWidthStr::width(grapheme));
        if column.saturating_add(cell_width) > width {
            row = row.saturating_add(1);
            column = 0;
        }
        column = column.saturating_add(cell_width);
        if column >= width {
            row = row.saturating_add(column / width);
            column %= width;
        }
    }
    (row, column)
}

fn truncate_cells(value: &str, max: usize) -> String {
    if UnicodeWidthStr::width(value) <= max {
        return value.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut result = String::new();
    let target = max.saturating_sub(1);
    for grapheme in value.graphemes(true) {
        let width = UnicodeWidthStr::width(result.as_str()) + UnicodeWidthStr::width(grapheme);
        if width > target {
            break;
        }
        result.push_str(grapheme);
    }
    result.push('…');
    result
}

fn pad_cells(value: &str, width: usize) -> String {
    let current = UnicodeWidthStr::width(value);
    format!("{value}{}", " ".repeat(width.saturating_sub(current)))
}

fn wrap_cells(value: &str, max: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for source_line in value.split('\n') {
        let mut current = String::new();
        let mut current_width = 0_usize;
        for word in source_line.split_inclusive(char::is_whitespace) {
            let word_width = UnicodeWidthStr::width(word);
            if current_width.saturating_add(word_width) > max && !current.is_empty() {
                lines.push(current.trim_end().to_owned());
                current.clear();
                current_width = 0;
            }
            if word_width > max {
                for grapheme in word.graphemes(true) {
                    let grapheme_width = UnicodeWidthStr::width(grapheme);
                    if current_width.saturating_add(grapheme_width) > max && !current.is_empty() {
                        lines.push(current);
                        current = String::new();
                        current_width = 0;
                    }
                    current.push_str(grapheme);
                    current_width = current_width.saturating_add(grapheme_width);
                }
            } else {
                current.push_str(word);
                current_width = current_width.saturating_add(word_width);
            }
        }
        lines.push(current.trim_end().to_owned());
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn spinner(tick: u64) -> &'static str {
    const FRAMES: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
    let index = usize::try_from(tick % u64::try_from(FRAMES.len()).unwrap_or(1)).unwrap_or(0);
    FRAMES[index]
}

fn clamp_u16(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(width.min(area.width)),
            Constraint::Fill(1),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(height.min(area.height)),
            Constraint::Fill(1),
        ])
        .split(horizontal[1])[1]
}

fn vertically_centered(area: Rect, height: u16) -> Rect {
    Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height.min(area.height)),
        Constraint::Fill(1),
    ])
    .split(area)[1]
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use qrcode::Color as QrColor;
    use ratatui::style::Color;
    use ratatui::{backend::TestBackend, Terminal};
    use unicode_width::UnicodeWidthStr;

    use super::{editor_lines, input_cursor, qr_pair_symbol, render, truncate_cells, wrap_cells};
    use crate::{
        app::{AppState, Focus, Mode, Screen},
        config::{DownloadBehavior, ReleaseChannel, Settings},
        event::{AuthPrompt, ConnectionStatus},
        input::{KeyAction, TextInput},
        model::{
            Attachment, AttachmentKind, Chat, ChatKind, Delivery, Message, MessageButton,
            MessageButtonKind, MessageLink, ReplyInfo,
        },
    };

    fn populated_app() -> AppState {
        populated_app_with_settings(Settings::default())
    }

    fn populated_app_with_settings(settings: Settings) -> AppState {
        let mut app = AppState::with_ephemeral_settings(settings);
        app.screen = Screen::Main;
        app.connection = ConnectionStatus::Online;
        app.user_name = Some("Me".to_owned());
        app.chats.push(Chat {
            id: 7,
            title: "Alice 東京".to_owned(),
            kind: ChatKind::Direct,
            unread: 2,
            last_message: "hello from the terminal".to_owned(),
            last_activity: Some(Utc::now()),
        });
        app.active_chat_id = Some(7);
        app.messages.insert(
            7,
            vec![Message {
                id: 11,
                chat_id: 7,
                sender: "Alice".to_owned(),
                reply_to: None,
                text: "hello from the terminal 🙂".to_owned(),
                timestamp: Utc::now(),
                outgoing: false,
                delivery: Delivery::Read,
                attachment: None,
                links: Vec::new(),
                buttons: Vec::new(),
            }],
        );
        app
    }

    fn render_text(app: &AppState, width: u16, height: u16) -> String {
        let mut app = app.clone();
        render_text_mut(&mut app, width, height)
    }

    fn render_text_mut(app: &mut AppState, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render(frame, app))
            .expect("render succeeds");
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn truncation_respects_terminal_cells() {
        let result = truncate_cells("東京 terminal", 6);
        assert!(UnicodeWidthStr::width(result.as_str()) <= 6);
        assert!(result.ends_with('…'));
    }

    #[test]
    fn wrapping_never_exceeds_width() {
        let result = wrap_cells("hello 世界 and-a-very-long-token", 8);
        assert!(result
            .iter()
            .all(|line| UnicodeWidthStr::width(line.as_str()) <= 8));
    }

    #[test]
    fn unicode_helpers_do_not_split_grapheme_clusters() {
        let family = "👩‍👩‍👧‍👦";
        let wrapped = wrap_cells(&format!("ab{family}cd"), 4);
        assert_eq!(wrapped.concat(), format!("ab{family}cd"));

        let truncated = truncate_cells(&format!("a{family}bcdef"), 4);
        assert_eq!(truncated, format!("a{family}…"));
    }

    #[test]
    fn wrapping_preserves_a_trailing_newline() {
        assert_eq!(wrap_cells("first\n", 20), ["first", ""]);
    }

    #[test]
    fn composer_uses_cell_wrapping_instead_of_word_wrapping() {
        let input = TextInput::from_value("hello world");
        assert_eq!(editor_lines(input.value(), 8), ["hello wo", "rld"]);
        assert_eq!(input_cursor(&input, 8), (1, 3));

        let unicode = TextInput::from_value("東京🙂a");
        assert_eq!(editor_lines(unicode.value(), 5), ["東京", "🙂a"]);
        assert_eq!(input_cursor(&unicode, 5), (1, 3));
    }

    #[test]
    fn reply_composer_keeps_its_target_visible() {
        let mut app = populated_app();
        app.focus = Focus::Conversation;
        app.handle_action(KeyAction::Character('R'));

        let output = render_text(&app, 100, 24);
        assert!(output.contains("Reply to #11 Alice"));
        assert!(output.contains("Reply #11"));
    }

    #[test]
    fn wide_layout_renders_chat_timeline_and_composer() {
        let app = populated_app();
        let output = render_text(&app, 120, 36);

        assert!(output.contains("Termgram"));
        assert!(output.contains("Alice"));
        assert!(output.contains("hello from the terminal"));
        assert!(output.contains("Message · i to compose"));
    }

    #[test]
    fn settings_overlay_renders_defaults_and_safety_language() {
        let mut app = populated_app();
        app.mode = Mode::Settings;
        let output = render_text(&app, 100, 30);

        assert!(output.contains("Termgram settings"));
        assert!(output.contains("Automatic update checks"));
        assert!(output.contains("Stable"));
        assert!(output.contains("Reveal on activation"));
        assert!(output.contains("never executes files"));
    }

    #[test]
    fn settings_overlay_reflects_prerelease_and_temp_only() {
        let mut app = AppState::with_settings(
            Settings {
                automatic_update_checks: false,
                release_channel: ReleaseChannel::Prerelease,
                download_behavior: DownloadBehavior::TempOnly,
                show_message_ids: false,
                ..Settings::default()
            },
            std::env::temp_dir().join("unused-termgram-settings.conf"),
        );
        app.screen = Screen::Main;
        app.mode = Mode::Settings;
        let output = render_text(&app, 100, 30);

        assert!(output.contains("Off"));
        assert!(output.contains("Prerelease"));
        assert!(output.contains("Temp only"));
        assert!(output.contains("never reveals files"));
    }

    #[test]
    fn accounts_overlay_marks_active_slot_and_offers_an_isolated_new_one() {
        let mut app = populated_app_with_settings(Settings {
            active_account: 2,
            account_count: 3,
            ..Settings::default()
        });
        app.handle_action(KeyAction::Character('a'));
        let output = render_text(&app, 100, 30);

        assert!(output.contains("Telegram accounts"));
        assert!(output.contains("Only the selected account stays connected"));
        assert!(output.contains("Account 1"));
        assert!(output.contains("Account 2  ·  Me  (active)"));
        assert!(output.contains("Account 3"));
        assert!(output.contains("+ Add account"));
        assert!(output.contains("1-8 direct"));
    }

    #[test]
    fn replies_render_target_metadata_and_optional_right_id_column() {
        let mut app = populated_app();
        let message = app.messages.get_mut(&7).unwrap().first_mut().unwrap();
        message.reply_to = Some(ReplyInfo {
            message_id: 42,
            chat_id: 7,
            sender: Some("Bob".to_owned()),
        });

        let without_column = render_text(&app, 100, 30);
        assert!(without_column.contains("↩ #42 Bob"));
        assert!(!without_column.contains("#11"));

        let settings = Settings {
            show_message_ids: true,
            ..Settings::default()
        };
        let mut app = populated_app_with_settings(settings);
        let message = app.messages.get_mut(&7).unwrap().first_mut().unwrap();
        message.reply_to = Some(ReplyInfo {
            message_id: 42,
            chat_id: 7,
            sender: Some("Bob".to_owned()),
        });
        let with_column = render_text(&app, 100, 30);
        assert!(with_column.contains("↩ #42 Bob"));
        assert!(with_column.contains("#11"));
    }

    #[test]
    fn update_hint_yields_to_transient_status_messages() {
        let mut app = populated_app();
        app.set_available_update("0.1.9");
        let hint = render_text(&app, 100, 30);
        assert!(hint.contains("Update 0.1.9 available · run tg update"));

        app.status_message = Some("Message failed".to_owned());
        let status = render_text(&app, 100, 30);
        assert!(status.contains("Message failed"));
        assert!(!status.contains("Update 0.1.9 available"));
    }

    #[test]
    fn attachment_and_sticker_fallbacks_render_as_actionable_terminal_rows() {
        let mut app = populated_app();
        app.messages.get_mut(&7).unwrap().extend([
            Message {
                id: 12,
                chat_id: 7,
                sender: "Alice".to_owned(),
                reply_to: None,
                text: "receipt".to_owned(),
                timestamp: Utc::now(),
                outgoing: false,
                delivery: Delivery::Read,
                attachment: Some(Attachment {
                    kind: AttachmentKind::Photo,
                    file_name: Some("image.jpg".to_owned()),
                    mime_type: Some("image/jpeg".to_owned()),
                    size: Some(2048),
                    fallback_emoji: None,
                }),
                links: Vec::new(),
                buttons: Vec::new(),
            },
            Message {
                id: 13,
                chat_id: 7,
                sender: "Alice".to_owned(),
                reply_to: None,
                text: String::new(),
                timestamp: Utc::now(),
                outgoing: false,
                delivery: Delivery::Read,
                attachment: Some(Attachment {
                    kind: AttachmentKind::Sticker,
                    file_name: None,
                    mime_type: None,
                    size: None,
                    fallback_emoji: Some("🙂".to_owned()),
                }),
                links: Vec::new(),
                buttons: Vec::new(),
            },
        ]);

        let output = render_text(&app, 120, 36);
        assert!(output.contains("[photo] image.jpg · 2.0 KiB · click/Enter to download"));
        // TestBackend retains the continuation cell for a wide emoji, so the
        // exact amount of padding between these two tokens is backend-specific.
        assert!(output.contains("🙂"));
        assert!(output.contains("[sticker]"));
    }

    #[test]
    fn message_links_and_inline_buttons_render_as_distinct_actions() {
        let mut app = populated_app();
        let message = app.messages.get_mut(&7).unwrap().first_mut().unwrap();
        message.links = vec![MessageLink {
            label: "Project site".to_owned(),
            url: "https://example.com/docs".to_owned(),
        }];
        message.buttons = vec![
            MessageButton {
                label: "Confirm".to_owned(),
                index: 0,
                kind: MessageButtonKind::Callback,
            },
            MessageButton {
                label: "Pay".to_owned(),
                index: 1,
                kind: MessageButtonKind::Unsupported,
            },
        ];

        let output = render_text_mut(&mut app, 120, 36);
        assert!(output.contains("↗ Project site → https://example.com/docs"));
        assert!(output.contains("● [ Confirm ]"));
        assert!(output.contains("× [ Pay ] · graphical client required"));
        assert!(app.message_hit_region_count() >= 2);
    }

    #[test]
    fn code_auth_explains_how_to_restart_sign_in() {
        let mut app = AppState::new();
        app.screen = Screen::Auth(crate::app::AuthPhase::Code {
            phone: "+81 90".to_owned(),
        });

        let output = render_text(&app, 72, 20);
        assert!(output.contains("Esc starts over"));
    }

    #[test]
    fn auth_submission_progress_is_visible_for_phone_code_and_password() {
        let mut app = AppState::new();
        app.handle_network(crate::event::NetworkEvent::Auth(AuthPrompt::Phone));
        app.handle_action(KeyAction::Character('+'));
        app.handle_action(KeyAction::Character('1'));
        app.handle_action(KeyAction::Enter);
        let phone = render_text(&app, 72, 20);
        assert!(phone.contains("Requesting a login code…"));
        assert!(phone.contains("Submitted"));

        app.handle_network(crate::event::NetworkEvent::Auth(AuthPrompt::Code {
            phone: "+1".to_owned(),
        }));
        app.handle_action(KeyAction::Character('1'));
        app.handle_action(KeyAction::Enter);
        let code = render_text(&app, 72, 20);
        assert!(code.contains("Checking login code…"));
        assert!(code.contains("Submitted"));

        app.handle_network(crate::event::NetworkEvent::Auth(AuthPrompt::Password {
            hint: None,
        }));
        for character in "hunter2".chars() {
            app.handle_action(KeyAction::Character(character));
        }
        app.handle_action(KeyAction::Enter);
        let password = render_text(&app, 72, 20);
        assert!(password.contains("Checking 2FA password…"));
        assert!(password.contains("Submitted"));
        assert!(!password.contains("hunter2"));
    }

    #[test]
    fn qr_login_renders_scannable_blocks_without_exposing_its_token() {
        // 32 bytes encoded without padding, matching Telegram's real token
        // size and the QR dimensions seen in an end-to-end login smoke test.
        let secret_url = "tg://login?token=AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";
        let mut app = AppState::new();
        app.handle_network(crate::event::NetworkEvent::Auth(AuthPrompt::Qr {
            url: secret_url.to_owned(),
        }));

        let output = render_text(&app, 80, 24);
        assert!(output.contains("Devices→Link Desktop"));
        assert!(output.contains("Tab full"));
        assert!(output.contains("Esc phone"));
        assert!(output.contains('▀'));
        assert!(!output.contains(secret_url));
        assert_eq!(qr_pair_symbol(QrColor::Dark, QrColor::Dark), "█");
        assert_eq!(qr_pair_symbol(QrColor::Dark, QrColor::Light), "▀");
        assert_eq!(qr_pair_symbol(QrColor::Light, QrColor::Dark), "▄");
        assert_eq!(qr_pair_symbol(QrColor::Light, QrColor::Light), " ");

        let small = render_text(&app, 40, 10);
        assert!(small.contains("Compact QR needs at least"));
        assert!(!small.contains(secret_url));

        assert!(app.handle_action(KeyAction::Tab).is_empty());
        let too_short = render_text(&app, 80, 24);
        assert!(too_short.contains("Full-cell QR needs at least"));
        assert!(too_short.contains("Tab"));
        assert!(too_short.contains("compact mode"));

        let backend = TestBackend::new(100, 50);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render(frame, &mut app))
            .expect("render succeeds");
        let buffer = terminal.backend().buffer();
        let compatible_text: String = buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(compatible_text.contains("Tab compact"));
        assert!(!compatible_text.contains('▀'));
        assert!(!compatible_text.contains(secret_url));
        assert!(buffer
            .content()
            .iter()
            .any(|cell| cell.bg == Color::Rgb(0, 0, 0)));
        assert!(buffer
            .content()
            .iter()
            .any(|cell| cell.bg == Color::Rgb(255, 255, 255)));
    }

    #[test]
    fn narrow_layout_switches_between_list_and_conversation() {
        let mut app = populated_app();
        let list = render_text(&app, 70, 24);
        assert!(list.contains("Alice"));
        assert!(!list.contains("hello from the terminal"));

        app.narrow_conversation = true;
        let conversation = render_text(&app, 70, 24);
        assert!(conversation.contains("Alice"));
        assert!(conversation.contains("hello from the terminal"));
        assert!(conversation.contains("Esc chats"));
    }

    #[test]
    fn non_conversation_and_too_small_frames_clear_pointer_targets() {
        let mut app = populated_app();
        let mut photo = app.messages.get(&7).unwrap()[0].clone();
        photo.id = 12;
        photo.attachment = Some(Attachment {
            kind: AttachmentKind::Photo,
            file_name: Some("photo.jpg".to_owned()),
            mime_type: Some("image/jpeg".to_owned()),
            size: Some(1),
            fallback_emoji: None,
        });
        app.messages.get_mut(&7).unwrap().push(photo);

        render_text_mut(&mut app, 120, 24);
        assert!(app.message_hit_region_count() > 0);
        assert!(app.chat_hit_region_count() > 0);
        render_text_mut(&mut app, 39, 9);
        assert_eq!(app.message_hit_region_count(), 0);
        assert_eq!(app.chat_hit_region_count(), 0);

        app.narrow_conversation = false;
        render_text_mut(&mut app, 70, 24);
        assert_eq!(app.message_hit_region_count(), 0);
        assert!(app.chat_hit_region_count() > 0);
    }

    #[test]
    fn chat_list_scrolls_to_keep_selection_visible() {
        let mut app = populated_app();
        for index in 0_i64..30 {
            app.chats.push(Chat {
                id: 100 + index,
                title: format!("Overflow chat {index:02}"),
                kind: ChatKind::Direct,
                unread: 0,
                last_message: String::new(),
                last_activity: None,
            });
        }
        app.selected_chat = app.chats.len() - 1;

        let output = render_text(&app, 70, 16);
        assert!(output.contains("Overflow chat 29"));
    }

    #[test]
    fn incoming_multiline_message_does_not_move_detached_viewport() {
        let mut app = populated_app();
        let messages = (0_i32..30)
            .map(|id| Message {
                id,
                chat_id: 7,
                sender: "Alice".to_owned(),
                reply_to: None,
                text: format!("message-{id:02}"),
                timestamp: Utc::now(),
                outgoing: false,
                delivery: Delivery::Read,
                attachment: None,
                links: Vec::new(),
                buttons: Vec::new(),
            })
            .collect::<Vec<_>>();
        app.messages.insert(7, messages);
        app.narrow_conversation = true;
        app.message_scroll = 5;

        let before = render_text_mut(&mut app, 70, 24);
        let anchor = (0_i32..30)
            .rev()
            .find(|id| before.contains(&format!("message-{id:02}")))
            .expect("at least one message is visible before arrival");

        app.messages
            .get_mut(&7)
            .expect("active history")
            .push(Message {
                id: 30,
                chat_id: 7,
                sender: "Alice".to_owned(),
                reply_to: None,
                text: "new-one\nnew-two\nnew-three".to_owned(),
                timestamp: Utc::now(),
                outgoing: false,
                delivery: Delivery::Read,
                attachment: None,
                links: Vec::new(),
                buttons: Vec::new(),
            });
        app.new_messages_while_scrolled = 1;
        app.new_messages_to_anchor = 1;

        let after = render_text_mut(&mut app, 70, 24);
        assert!(after.contains(&format!("message-{anchor:02}")));
        assert!(!after.contains("new-three"));
        assert!(after.contains("1 new"));
        assert_eq!(app.message_scroll, 8);
        assert_eq!(app.new_messages_to_anchor, 0);

        let anchored_scroll = app.message_scroll;
        let second_redraw = render_text_mut(&mut app, 70, 24);
        assert!(second_redraw.contains(&format!("message-{anchor:02}")));
        assert_eq!(app.message_scroll, anchored_scroll);
    }

    #[test]
    fn histories_taller_than_u16_rows_reach_both_bottom_and_top() {
        let mut app = populated_app();
        app.narrow_conversation = true;
        app.messages.insert(
            7,
            vec![Message {
                id: 99,
                chat_id: 7,
                sender: "Alice".to_owned(),
                reply_to: None,
                text: format!("oldest\n{}newest", "filler\n".repeat(66_000)),
                timestamp: Utc::now(),
                outgoing: false,
                delivery: Delivery::Read,
                attachment: None,
                links: Vec::new(),
                buttons: Vec::new(),
            }],
        );

        let bottom = render_text_mut(&mut app, 70, 24);
        assert!(bottom.contains("newest"));
        assert!(!bottom.contains("oldest"));

        app.message_scroll = usize::MAX;
        let top = render_text_mut(&mut app, 70, 24);
        assert!(top.contains("oldest"));
        assert!(!top.contains("newest"));
    }
}
