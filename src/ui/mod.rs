mod appearance;
mod chat_info;
mod chats;
mod commands;
mod completion;
mod deletion;
mod editing;
mod entities;
mod forwarding;
mod help;
mod icons;
mod invites;
mod pins;
mod polls;
mod preview;
mod reactions;
mod search;
mod staging;
mod statusline;
mod transcript;
mod wrapping;
use chrono::Local;
use qrcode::{Color as QrColor, QrCode};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use wrapping::wrap_cells;

use crate::app::{AppState, AuthPhase, Focus, Mode, QrRenderMode, Screen};
use crate::config::DownloadBehavior;
use crate::input::TextInput;
use crate::keymap::Context;
use crate::model::{AttachmentKind, Message};

const ACCENT: Color = Color::Rgb(216, 180, 254);
const MUTED: Color = Color::DarkGray;
const SUCCESS: Color = Color::Rgb(126, 211, 166);
const WARNING: Color = Color::Rgb(245, 194, 107);
const DANGER: Color = Color::Rgb(242, 139, 130);
// Terminal QR renderers such as qr-cli use a compact two-module border. A
// quiet zone is still required for reliable finder-pattern detection, but a
// full four-module print margin makes short-lived login tokens unnecessarily
// large in an 80 x 24 terminal.
const QR_QUIET_ZONE: usize = 2;

pub fn render(frame: &mut Frame<'_>, app: &mut AppState) {
    app.set_visible_polls(Vec::new());
    app.set_visible_reactions(Vec::new());
    app.reactions.hit_rows.clear();
    app.polls.hit_rows.clear();
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

fn account_controls(app: &AppState, context: Context) -> String {
    let hint = |action| app.keymap.hint(context, action);
    format!(
        "{} quit · {} next · {} add account",
        hint("quit"),
        hint("next_account"),
        hint("add_account")
    )
}

fn overlay_controls(app: &AppState, action: &str) -> String {
    let hint = |action| app.keymap.hint(Context::Overlay, action);
    format!(
        "{} {action} · {} close\n{}/{} select",
        hint("open"),
        hint("cancel"),
        hint("up"),
        hint("down")
    )
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
            account_controls(app, Context::Global),
            Style::default().fg(MUTED),
        )),
    ];
    if let Some(message) = &app.status_message {
        lines.push(Line::from(Span::styled(
            message.as_str(),
            Style::default().fg(WARNING),
        )));
    }
    render_centered_notice(frame, area, lines);
}

fn auth_controls(app: &AppState, phase: &AuthPhase) -> String {
    let hint = |action| app.keymap.hint(Context::Input, action);
    format!(
        "{} · {}",
        if matches!(phase, AuthPhase::Phone) {
            format!("{} QR", hint("focus"))
        } else {
            format!("{} starts over", hint("cancel"))
        },
        account_controls(app, Context::Input)
    )
}

fn render_auth(frame: &mut Frame<'_>, area: Rect, app: &AppState, phase: &AuthPhase) {
    if let AuthPhase::Qr { url } = phase {
        render_qr_auth(frame, area, app, url);
        return;
    }
    let width = area.width.min(72);
    let text_width = width.saturating_sub(4);
    let hint = |action| app.keymap.hint(Context::Input, action);
    let footer = auth_controls(app, phase);
    let footer_height = wrapped_height(&footer, text_width);
    let status_height = app
        .status_message
        .as_deref()
        .map_or(2, |message| wrapped_height(message, text_width).max(2));
    let height = 14_u16
        .max(
            8_u16
                .saturating_add(footer_height)
                .saturating_add(status_height),
        )
        .min(area.height);
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
    let detail_height = inner.height.saturating_sub(4 + footer_height).clamp(1, 3);
    let chunks = Layout::vertical([
        Constraint::Length(detail_height),
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(footer_height),
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
    let submit_hint = format!("{} to continue", hint("open"));
    render_input(
        frame,
        chunks[1],
        app.auth_input(),
        masked,
        if app.auth_is_submitting() {
            "Submitted"
        } else {
            &submit_hint
        },
        !app.auth_is_submitting(),
    );
    if let Some(message) = &app.status_message {
        render_notice(frame, chunks[2], message, DANGER);
    } else if let Some(message) = app.auth_progress_label() {
        render_notice(
            frame,
            chunks[2],
            &format!("{}  {message}", spinner(app.tick)),
            WARNING,
        );
    }
    render_notice(frame, chunks[3], &footer, MUTED);
}

fn render_qr_auth(frame: &mut Frame<'_>, area: Rect, app: &AppState, url: &str) {
    let display = app.keymap.hint(Context::Input, "focus");
    let cancel = app.keymap.hint(Context::Input, "cancel");
    let accounts = account_controls(app, Context::Input);
    let Ok(code) = QrCode::new(url.as_bytes()) else {
        render_qr_unavailable(
            frame,
            area,
            app,
            &format!("Could not render this QR code. {cancel} for phone sign-in."),
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
            "{mode_name} QR needs at least {qr_width} × {required_height} terminal cells. Resize, {display} for {alternative} mode, or {cancel} for phone sign-in."
        );
        let message = app
            .status_message
            .as_deref()
            .map_or_else(|| message.clone(), |error| format!("{error}\n\n{message}"));
        render_qr_unavailable(frame, area, app, &message);
        return;
    }

    let (guidance, style) = if let Some(message) = &app.status_message {
        (
            format!("{message} · {display} display · {cancel} phone · {accounts}"),
            Style::default().fg(DANGER),
        )
    } else {
        let alternative = match mode {
            QrRenderMode::Compact => "full",
            QrRenderMode::Compatible => "compact",
        };
        (
            format!(
                "{} Telegram: Devices→Link Desktop · {display} {alternative} · {cancel} phone · {accounts}",
                spinner(app.tick),
            ),
            Style::default().fg(WARNING).bold(),
        )
    };
    let guidance_height = wrapped_height(&guidance, area.width);
    let required_height = qr_height.saturating_add(guidance_height);
    if required_height > area.height {
        render_qr_unavailable(
            frame,
            area,
            app,
            &format!(
                "{guidance}\n\nResize the terminal to show the QR code, or {cancel} for phone sign-in."
            ),
        );
        return;
    }
    let view_y = area.y.saturating_add((area.height - required_height) / 2);
    frame.render_widget(
        Paragraph::new(notice_lines(&guidance, area.width, guidance_height))
            .alignment(Alignment::Center)
            .style(style),
        Rect::new(area.x, view_y, area.width, guidance_height),
    );
    let qr_x = area
        .x
        .saturating_add(area.width.saturating_sub(qr_width) / 2);
    render_qr_code(
        frame,
        Rect::new(
            qr_x,
            view_y.saturating_add(guidance_height),
            qr_width,
            qr_height,
        ),
        &code,
        QR_QUIET_ZONE,
        mode,
    );
}

fn render_qr_unavailable(frame: &mut Frame<'_>, area: Rect, app: &AppState, message: &str) {
    let popup = centered(area, area.width.min(72), area.height);
    render_centered_notice(
        frame,
        popup,
        vec![
            Line::from(Span::styled(
                "Termgram · QR sign in",
                Style::default().fg(ACCENT).bold(),
            )),
            Line::from(""),
            Line::from(message),
            Line::from(""),
            Line::from(Span::styled(
                format!("{} quit", app.keymap.hint(Context::Input, "quit")),
                Style::default().fg(MUTED),
            )),
        ],
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

#[allow(clippy::too_many_lines)]
fn render_main(frame: &mut Frame<'_>, area: Rect, app: &mut AppState) {
    let narrow = area.width < crate::sidebar::MIN_SPLIT_WIDTH;
    let conversation_only =
        app.active_chat_id.is_some() && (app.sidebar_hidden || (narrow && app.narrow_conversation));
    let sidebar_width = app.keymap.sidebar.width_for(area.width);
    let editor_width = if narrow || conversation_only {
        area.width
    } else {
        area.width.saturating_sub(sidebar_width)
    };
    let composer_height = composer_height(app, editor_width);
    let notice_height = app.status_message.as_deref().map_or(0, |message| {
        wrapped_height(message, area.width)
            .min(area.height.saturating_sub(composer_height + 6).max(1))
    });
    let rows = Layout::vertical([
        Constraint::Min(5),
        Constraint::Length(notice_height),
        Constraint::Length(u16::from(app.keymap.statusline.enabled)),
    ])
    .split(area);

    let conversation_area = if !narrow && !conversation_only {
        let panes = Layout::horizontal([Constraint::Length(sidebar_width), Constraint::Min(48)])
            .split(rows[0]);
        chats::render(frame, panes[0], app);
        panes[1]
    } else {
        rows[0]
    };
    let content = Layout::vertical([Constraint::Min(3), Constraint::Length(composer_height)])
        .split(conversation_area);
    if narrow && !conversation_only {
        chats::render(frame, content[0], app);
    } else {
        render_conversation(frame, content[0], app);
    }
    render_composer(frame, content[1], app, conversation_only || !narrow);
    if app.mode == Mode::Compose {
        completion::render(frame, app);
    }
    if let Some(message) = &app.status_message {
        render_notice(frame, rows[1], message, WARNING);
    }
    statusline::render(frame, rows[2], app);

    if app.mode == Mode::ChatInfo {
        chat_info::render(frame, area, app);
    } else if app.mode == Mode::Invite {
        invites::render(frame, area, app);
    } else if app.mode == Mode::ForwardPrompt {
        forwarding::render(frame, area, app);
    } else if app.mode == Mode::DeletePrompt {
        deletion::render(frame, area, app);
    } else if app.mode == Mode::Reactions {
        reactions::render(frame, area, app);
    } else if app.mode == Mode::Poll {
        polls::render(frame, area, app);
    } else if app.mode == Mode::Edit {
        editing::render(frame, area, app);
    } else if app.mode == Mode::Command {
        commands::render(frame, area, app);
    } else if app.mode == Mode::Status {
        commands::render_status(frame, area, app);
    } else if app.mode == Mode::Attachments {
        staging::render(frame, area, app);
    } else if app.mode == Mode::Preview {
        preview::render(frame, area, app);
    } else if app.mode == Mode::Colors {
        appearance::render_colors(frame, area, app);
    } else if app.mode == Mode::PinnedMessages {
        pins::render(frame, area, app);
    } else if app.mode == Mode::PinPrompt {
        pins::render_prompt(frame, area, app);
    } else if app.mode == Mode::Search {
        search::render_search(frame, area, app);
    } else if app.mode == Mode::Help {
        help::render(frame, area, app);
    } else if app.mode == Mode::Settings {
        render_settings(frame, area, app);
    } else if app.mode == Mode::Accounts {
        render_accounts(frame, area, app);
    }
    if matches!(
        app.mode,
        Mode::ChatInfo
            | Mode::Invite
            | Mode::Search
            | Mode::Poll
            | Mode::Reactions
            | Mode::Edit
            | Mode::DeletePrompt
            | Mode::ForwardPrompt
            | Mode::Command
            | Mode::Status
            | Mode::Colors
            | Mode::Help
            | Mode::Settings
            | Mode::Accounts
            | Mode::PinnedMessages
            | Mode::PinPrompt
    ) {
        app.media_slots.clear();
    }
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
    )
    .title_style(
        Style::default().fg(app.active_chat_id.map_or(Color::Reset, |id| {
            app.color(crate::appearance::Target::Chat(id))
        })),
    );
    let mut inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(chat_id) = app.active_chat_id else {
        app.set_message_hit_regions(Vec::new());
        frame.render_widget(
            Paragraph::new(format!(
                "Select a chat · {} open",
                app.keymap.hint(Context::Chats, "open")
            ))
            .alignment(Alignment::Center)
            .style(Style::default().fg(MUTED)),
            vertically_centered(inner, 1),
        );
        return;
    };
    if let Some((message, count)) = app.pinned_summary()
        && inner.height > 1
    {
        let label = format!(
            "{} {} pins{} · {}",
            icons::Icons(app.keymap.nerd_font).pin(),
            app.keymap.hint(Context::Conversation, "pins"),
            count.map_or(String::new(), |n| format!(" ({n})")),
            crate::model::sanitize_terminal_line(&message.preview_text()),
        );
        frame.render_widget(
            Paragraph::new(truncate_cells(&label, usize::from(inner.width)))
                .style(Style::default().fg(ACCENT)),
            Rect { height: 1, ..inner },
        );
        inner.y = inner.y.saturating_add(1);
        inner.height = inner.height.saturating_sub(1);
    }
    let messages: &[Message] = app.messages.get(&chat_id).map_or(&[], Vec::as_slice);
    if messages.is_empty() {
        app.set_message_hit_regions(Vec::new());
        app.message_scroll = 0;
        app.new_messages_while_scrolled = 0;
        app.new_messages_to_anchor = 0;
        let text = if app.loading_history {
            format!("{}  Loading messages…", spinner(app.tick))
        } else {
            "No messages yet".to_owned()
        };
        frame.render_widget(
            Paragraph::new(text)
                .alignment(Alignment::Center)
                .style(Style::default().fg(MUTED)),
            vertically_centered(inner, 1),
        );
        return;
    }

    let reflow = app.viewport_width != inner.width;
    app.viewport_width = inner.width;
    let message_width = usize::from(inner.width.max(1));
    let mut lines = Vec::new();
    let mut layouts = Vec::with_capacity(messages.len());
    let mut media_rows = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        let start = lines.len();
        if app.unread_separator() == Some(message.id) {
            lines.push(Line::styled(
                "  ── Unread messages ──",
                Style::default().fg(ACCENT),
            ));
        }
        let content_start = lines.len();
        let rendered = transcript::render(message, message_width, app);
        let body_action = rendered.body_action;
        let body_height = rendered.body_height;
        let mut hit_rows: Vec<_> = (0..body_height)
            .map(|row| (content_start.saturating_add(row), None))
            .collect();
        // Pointer lookup walks backward: specific action rows must win over
        // the enclosing message (a reply quote may accompany an attachment).
        hit_rows.extend(
            rendered
                .action_rows
                .into_iter()
                .map(|(row, action)| (content_start.saturating_add(row), Some(action))),
        );
        lines.extend(rendered.lines);
        if message.id > 0
            && app.keymap.messages.images.inline()
            && let Some(attachment) = message.attachment.as_ref().filter(|a| a.supports_preview())
        {
            let height = inner
                .height
                .saturating_sub(u16::from(app.new_messages_while_scrolled > 0))
                .min(if attachment.kind == AttachmentKind::Sticker {
                    6
                } else {
                    10
                });
            let width = inner.width.saturating_sub(transcript::GUTTER_WIDTH).min(
                if attachment.kind == AttachmentKind::Sticker {
                    24
                } else {
                    48
                },
            );
            if height > 0 && width > 0 {
                media_rows.push((message.id, lines.len(), width, height));
                hit_rows.extend(
                    (lines.len()..lines.len() + usize::from(height)).map(|row| (row, body_action)),
                );
                lines.extend((0..height).map(|_| {
                    Line::from(transcript::gutter(app.selected_message == Some(message.id)))
                }));
            }
        }
        let background = app
            .keymap
            .messages
            .background(index % 2 == 1, app.terminal_background);
        for line in &mut lines[start..] {
            line.style = line.style.bg(background);
            line.spans.push(Span::raw(
                " ".repeat(message_width.saturating_sub(line.width())),
            ));
        }
        lines.extend((0..app.keymap.messages.spacing).map(|_| Line::default()));
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
        if let Some(layout) = layouts.iter().find(|layout| layout.id == anchor_id) {
            let row = if app.viewport_anchor_row >= layout.height
                || (reflow && app.viewport_anchor_row == layout.height.saturating_sub(1))
            {
                // Reflow may remove the old physical row. Keep this message
                // visible from its header instead of landing on its separator.
                0
            } else {
                app.viewport_anchor_row
            };
            scroll = layout.start.saturating_add(row).min(max_scroll);
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
    for (message_id, row, width, height) in media_rows {
        if row >= scroll.saturating_add(available) || row + usize::from(height) <= scroll {
            continue;
        }
        let offset = if row >= scroll {
            i16::try_from(row - scroll).unwrap_or(i16::MAX)
        } else {
            -i16::try_from(scroll - row).unwrap_or(i16::MAX)
        };
        let viewport = Rect::new(
            inner.x.saturating_add(transcript::GUTTER_WIDTH),
            inner.y,
            width,
            inner
                .height
                .saturating_sub(u16::from(app.new_messages_while_scrolled > 0)),
        );
        app.media_slots.push(crate::media::MediaSlot {
            source: crate::media::MediaSource::Message {
                chat_id,
                message_id,
            },
            viewport,
            offset,
            size: ratatui::layout::Size::new(width, height),
        });
        let pending = app
            .media_previews
            .get(&(chat_id, message_id))
            .is_none_or(|preview| preview.path.is_none());
        if pending {
            let y = inner
                .y
                .saturating_add(clamp_u16(row.saturating_sub(scroll)));
            frame.render_widget(
                Paragraph::new("▧").style(Style::default().fg(MUTED)),
                Rect::new(viewport.x, y, width, 1),
            );
        }
    }
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
    // The new-message badge covers the last physical row. It cannot count as
    // displaying the end of a message underneath it.
    let read_bottom = scroll
        .saturating_add(available.saturating_sub(usize::from(app.new_messages_while_scrolled > 0)));
    let visible = layouts
        .iter()
        .filter(|layout| {
            layout.start < read_bottom
                && layout.start.saturating_add(layout.height) > scroll
                && layout.start.saturating_add(layout.height) <= read_bottom
        })
        .filter_map(|layout| {
            messages
                .iter()
                .find(|message| message.id == layout.id && message.id > 0 && !message.outgoing)
        })
        .collect::<Vec<_>>();
    let visible_polls = layouts
        .iter()
        .filter(|layout| {
            layout.start < read_bottom && layout.start.saturating_add(layout.height) > scroll
        })
        .filter_map(|layout| {
            messages
                .iter()
                .find(|message| message.id == layout.id && message.poll.is_some() && message.id > 0)
        })
        .map(|message| (message.chat_id, message.id))
        .take(32)
        .collect();
    let visible_reactions = layouts
        .iter()
        .filter(|layout| {
            layout.start < read_bottom && layout.start.saturating_add(layout.height) > scroll
        })
        .filter_map(|layout| {
            messages.iter().find(|message| {
                message.id == layout.id
                    && message.id > 0
                    && !message.outgoing
                    && message.reactions.is_some()
            })
        })
        .map(|message| (message.chat_id, message.id))
        .take(100)
        .collect();
    let visible_read = visible.iter().map(|message| message.id).max();
    let mentions = visible
        .iter()
        .filter(|message| {
            message
                .mention
                .as_ref()
                .is_some_and(|mention| mention.unread && !mention.requires_playback)
        })
        .map(|message| message.id)
        .take(100)
        .collect();
    app.set_visible_polls(visible_polls);
    app.set_visible_reactions(visible_reactions);
    app.set_visible_mentions(mentions);
    app.set_visible_read_boundary(chat_id, visible_read.or((available > 0).then_some(0)));
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

fn render_composer(frame: &mut Frame<'_>, area: Rect, app: &mut AppState, enabled: bool) {
    app.set_composer_region((area.x, area.right(), area.y, area.bottom()));
    let active = app.mode == Mode::Compose;
    let title = app.active_reply_target().map_or_else(String::new, |reply| {
        format!(
            " Reply to #{} {} ",
            reply.message_id,
            app.reply_message(reply)
                .map(|message| message.sender.as_str())
                .or(reply.sender.as_deref())
                .unwrap_or("Original message")
        )
    });
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
    let inner = staging::composer_summary(frame, inner, app);
    let empty = TextInput::new();
    let input = app.active_draft().unwrap_or(&empty);
    let (row, column) = input_cursor(input, inner.width.max(1));
    let vertical_scroll = row.saturating_sub(inner.height.saturating_sub(1));
    let placeholder = input.is_empty();
    let text = if placeholder {
        use crate::keymap::Context;
        let hint = if let Some(reason) = app.active_chat_id.and_then(|id| app.draft_restriction(id))
        {
            reason
        } else if app.keymap.ghost_text.is_empty() {
            String::new()
        } else if active {
            app.keymap
                .ghost_text
                .replace("{send}", &app.keymap.hint(Context::Compose, "send"))
                .replace("{newline}", &app.keymap.hint(Context::Compose, "newline"))
                .replace("{cancel}", &app.keymap.hint(Context::Compose, "cancel"))
        } else if app.focus == Focus::Chats {
            format!("{} to compose", app.keymap.hint(Context::Chats, "compose"))
        } else if app.selected_message.is_some() {
            format!(
                "{} to reply",
                app.keymap.hint(Context::Conversation, "compose")
            )
        } else {
            format!(
                "{} to compose",
                app.keymap.hint(Context::Conversation, "compose")
            )
        };
        Text::from(crate::model::sanitize_terminal_line(&hint))
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
            "Message IDs",
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
                    DownloadBehavior::CacheOnly => "  Keep downloads for reuse.",
                    DownloadBehavior::RevealOnActivation => {
                        "  Activate a downloaded file to reveal it."
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
            overlay_controls(app, "toggle"),
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
            overlay_controls(app, "switch"),
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
    render_centered_notice(
        frame,
        area,
        vec![
            Line::from(Span::styled(
                "Termgram stopped",
                Style::default().fg(DANGER).bold(),
            )),
            Line::from(""),
            Line::from(message),
            Line::from(""),
            Line::from(Span::styled(
                format!(
                    "Account {} · {}",
                    app.active_account(),
                    account_controls(app, Context::Chats)
                ),
                Style::default().fg(MUTED),
            )),
        ],
    );
}

fn wrapped_height(message: &str, width: u16) -> u16 {
    clamp_u16(wrap_cells(message, usize::from(width.max(1))).len())
}

// Use the same cell-aware wrapping for measurement and rendering. A bounded
// viewport must indicate overflow instead of silently dropping error details.
fn notice_lines(message: &str, width: u16, height: u16) -> Vec<Line<'static>> {
    let mut lines = wrap_cells(message, usize::from(width.max(1)));
    if lines.len() > usize::from(height) {
        lines.truncate(usize::from(height));
        if let Some(last) = lines.last_mut() {
            *last = truncate_cells("… Resize terminal for more", usize::from(width));
        }
    }
    lines.into_iter().map(Line::from).collect()
}

fn render_notice(frame: &mut Frame<'_>, area: Rect, message: &str, color: Color) {
    frame.render_widget(
        Paragraph::new(notice_lines(message, area.width, area.height))
            .style(Style::default().fg(color)),
        area,
    );
}

fn render_centered_notice(frame: &mut Frame<'_>, area: Rect, lines: Vec<Line<'_>>) {
    let mut wrapped = Vec::new();
    for line in lines {
        let style = line.spans.first().map_or(line.style, |span| span.style);
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        wrapped.extend(
            notice_lines(&text, area.width, u16::MAX)
                .into_iter()
                .map(|line| line.style(style)),
        );
    }
    if wrapped.len() > usize::from(area.height) {
        wrapped.truncate(usize::from(area.height));
        if let Some(last) = wrapped.last_mut() {
            *last = Line::from("… Resize terminal for more");
        }
    }
    let height = clamp_u16(wrapped.len());
    frame.render_widget(
        Paragraph::new(wrapped).alignment(Alignment::Center),
        vertically_centered(area, height),
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
        .saturating_add(u16::from(
            app.preparing_attachments() || !app.draft_attachments().is_empty(),
        ))
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
    use ratatui::{Terminal, backend::TestBackend};
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

    #[test]
    #[allow(clippy::too_many_lines)]
    fn entities_keep_styles_and_require_explicit_spoiler_and_quote_actions() {
        use crate::{
            app::MessageAction,
            entities::{Entity, Kind},
            event::NetworkEvent,
        };
        use ratatui::style::Modifier;
        use yazi_term::event::{Modifiers, MouseButton, MouseEvent, MouseEventKind};
        let mut app = populated_app();
        app.sidebar_hidden = true;
        app.focus = Focus::Conversation;
        app.narrow_conversation = true;
        app.selected_message = Some(11);
        let mut message = app.active_messages()[0].clone();
        message.text =
            "Bold 🙂\n  let answer = 42;  \nquote-one\nquote-two\nquote-three\nquote-four\nSECRET"
                .to_owned();
        let quote = message.text.find("quote-one").unwrap();
        let secret = message.text.find("SECRET").unwrap();
        let code = message.text.find("  let").unwrap();
        message.entities = vec![
            Entity {
                range: 0..4,
                kind: Kind::Bold,
            },
            Entity {
                range: 0..4,
                kind: Kind::Italic,
            },
            Entity {
                range: code..quote,
                kind: Kind::Pre {
                    language: "rust".to_owned(),
                },
            },
            Entity {
                range: quote..secret,
                kind: Kind::Quote { collapsed: true },
            },
            Entity {
                range: secret..message.text.len(),
                kind: Kind::Spoiler,
            },
        ];
        message.links = vec![MessageLink {
            label: "SECRET".to_owned(),
            url: "https://secret.example".to_owned(),
        }];
        let encoded = serde_json::to_string(&message).unwrap();
        let restored: Message = serde_json::from_str(&encoded).unwrap();
        assert_eq!(restored.entities, message.entities);
        assert!(!restored.preview_text().contains("SECRET"));
        let mut old: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        old.as_object_mut().unwrap().remove("entities");
        assert!(
            serde_json::from_value::<Message>(old)
                .unwrap()
                .entities
                .is_empty()
        );
        app.handle_network(NetworkEvent::MessageUpdated(message.clone()));
        assert!(!app.chats[0].last_message.contains("SECRET"));
        for width in [40, 100] {
            let mut terminal = Terminal::new(TestBackend::new(width, 32)).unwrap();
            terminal.draw(|frame| render(frame, &mut app)).unwrap();
            let buffer = terminal.backend().buffer();
            let text: String = buffer
                .content()
                .iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect();
            assert!(!text.contains("SECRET") && !text.contains("secret.example"));
            assert!(!text.contains("quote-three"));
            assert!(text.contains("│   let answer = 42;  "));
            let bold_row = (0..32)
                .find(|&y| {
                    (0..width)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                        .contains("Bold")
                })
                .unwrap();
            let bold = (0..width)
                .find(|&x| buffer[(x, bold_row)].symbol() == "B")
                .unwrap();
            assert!(
                buffer[(bold, bold_row)]
                    .modifier
                    .contains(Modifier::BOLD | Modifier::ITALIC)
            );
            let row = (0..32)
                .find(|&y| (0..width).any(|x| buffer[(x, y)].symbol() == "▨"))
                .unwrap();
            app.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 6,
                row,
                modifiers: Modifiers::empty(),
            });
            assert!(app.spoilers_revealed(app.inspected_message().unwrap()));
            assert!(render_text_mut(&mut app, width, 32).contains("SECRET"));
            app.handle_action(KeyAction::Enter);
            assert!(!app.spoilers_revealed(app.inspected_message().unwrap()));
        }
        app.selected_action = app
            .message_actions(&message)
            .iter()
            .position(|action| *action == MessageAction::ExpandQuote)
            .unwrap();
        app.handle_action(KeyAction::Enter);
        assert!(render_text_mut(&mut app, 100, 32).contains("quote-four"));
        app.handle_key(&yazi_term::event::KeyEvent::new(
            yazi_term::event::KeyCode::Char(':'),
            Modifiers::empty(),
        ));
        for ch in "spoiler".chars() {
            app.handle_action(KeyAction::Character(ch));
        }
        assert!(app.handle_action(KeyAction::Enter).is_empty());
        assert!(app.spoilers_revealed(app.inspected_message().unwrap()));
        // A live edit cannot inherit permission to reveal different content.
        message.text.push('!');
        app.handle_network(NetworkEvent::MessageUpdated(message));
        assert!(!app.spoilers_revealed(app.inspected_message().unwrap()));
        assert!(!render_text_mut(&mut app, 100, 32).contains("SECRET"));
    }

    #[test]
    fn unread_separator_keeps_message_hit_rows_on_the_message() {
        use crate::event::NetworkEvent;
        use yazi_term::event::{Modifiers, MouseButton, MouseEvent, MouseEventKind};
        let mut app = populated_app();
        let messages = app.active_messages().to_vec();
        app.handle_action(KeyAction::Enter);
        app.handle_network(NetworkEvent::History {
            chat_id: 7,
            request_id: 1,
            messages,
        });
        let text = render_text_mut(&mut app, 80, 24);
        assert!(text.contains("Unread messages"));
        assert_eq!(app.unread_separator(), Some(11));
        app.sidebar_hidden = true;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let separator = (0..24)
            .find(|&y| {
                (0..80)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
                    .contains("Unread messages")
            })
            .unwrap();
        let click = |row| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 8,
            row,
            modifiers: Modifiers::empty(),
        };
        app.handle_mouse(click(separator));
        assert_eq!(app.selected_message, None);
        app.handle_mouse(click(separator + 1));
        assert_eq!(app.selected_message, Some(11));
    }

    #[test]
    fn receipts_follow_visible_rows_and_require_an_unobscured_focused_frame() {
        use crate::event::{NetworkEvent, TelegramCommand};
        let mut app = populated_app();
        app.sidebar_hidden = true;
        app.focus = Focus::Conversation;
        let first = app.active_messages()[0].clone();
        let mut later = first.clone();
        later.id = 12;
        later.text = "Long incoming message\n".repeat(40);
        app.messages.insert(7, vec![first, later]);
        app.viewport_anchor_message = Some(11);
        assert!(app.request_visible_read().is_empty());

        app.mode = Mode::Help;
        render_text_mut(&mut app, 80, 18);
        assert!(app.request_visible_read().is_empty());
        app.mode = Mode::Navigate;
        app.terminal_focused = false;
        render_text_mut(&mut app, 80, 18);
        assert!(app.request_visible_read().is_empty());
        app.terminal_focused = true;
        render_text_mut(&mut app, 80, 18);
        assert_eq!(
            app.request_visible_read(),
            vec![TelegramCommand::MarkRead {
                chat_id: 7,
                max_id: 11,
            }]
        );
        assert!(app.request_visible_read().is_empty());
        app.handle_network(NetworkEvent::ReadMarked {
            chat_id: 7,
            max_id: 11,
            snapshot: None,
        });
        assert!(app.request_visible_read().is_empty());

        // A smaller terminal replaces the transcript. Old frame evidence dies.
        render_text_mut(&mut app, 10, 3);
        assert!(app.request_visible_read().is_empty());
    }

    #[test]
    fn command_popup_adapts_and_pointer_completion_does_not_execute() {
        use yazi_term::event::{
            KeyCode, KeyEvent, Modifiers, MouseButton, MouseEvent, MouseEventKind,
        };
        for (width, height) in [(40, 10), (80, 24), (160, 42)] {
            let mut app = populated_app();
            app.handle_key(&KeyEvent::new(KeyCode::Char(':'), Modifiers::empty()));
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| render(frame, &mut app)).unwrap();
            assert!(!app.commands.hit_regions.is_empty());
            let (x, _, y, _) = app.commands.hit_regions[0];
            assert!(
                app.handle_mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: x,
                    row: y,
                    modifiers: Modifiers::empty()
                })
                .is_empty()
            );
            assert_eq!(app.mode, Mode::Command);
            assert!(!app.commands.input.is_empty());
            app.commands.input.set_value("界".repeat(100));
            terminal.draw(|frame| render(frame, &mut app)).unwrap();
            app.handle_key(&KeyEvent::new(KeyCode::Escape, Modifiers::empty()));
            terminal.draw(|frame| render(frame, &mut app)).unwrap();
            assert!(app.commands.hit_regions.is_empty());
            assert_eq!(app.mode, Mode::Navigate);
        }
    }

    #[test]
    fn compact_messages_keep_full_authors_and_use_theme_relative_backgrounds() {
        let mut app = populated_app();
        app.focus = Focus::Conversation;
        app.sidebar_hidden = true;
        let mut first = app.active_messages()[0].clone();
        first.sender = "Long author name with 中文 and a complete ApellidoFinal".to_owned();
        first.sender_username = Some("complete_username_1234567890".to_owned());
        first.text = "First body".to_owned();
        let mut second = first.clone();
        second.id += 1;
        second.sender = "Second author".to_owned();
        second.sender_username = None;
        second.text = "Second body".to_owned();
        app.messages.insert(7, vec![first, second]);
        for background in [[25, 28, 35], [240, 240, 240]] {
            app.terminal_background = Some(background);
            let mut terminal = Terminal::new(TestBackend::new(40, 20)).unwrap();
            terminal.draw(|frame| render(frame, &mut app)).unwrap();
            let buffer = terminal.backend().buffer();
            let rows: Vec<_> = (0..20)
                .map(|y| (0..40).map(|x| buffer[(x, y)].symbol()).collect::<String>())
                .collect();
            assert!(rows.iter().any(|line| line.contains("ApellidoFinal")));
            assert!(
                rows.iter()
                    .any(|line| line.contains("@complete_username_1234567890"))
            );
            let first = rows
                .iter()
                .position(|line| line.contains("First body"))
                .unwrap();
            assert!(rows[first + 1].contains("Second author"));
            let y = u16::try_from(first + 1).unwrap();
            assert_ne!(buffer[(3, y)].bg, Color::Reset);
            assert_eq!(buffer[(3, y)].bg, buffer[(38, y)].bg);
        }
    }

    #[test]
    fn transcript_separates_metadata_reply_and_body_and_aligns_media() {
        use crate::event::TelegramCommand;
        use yazi_term::event::{Modifiers, MouseButton, MouseEvent, MouseEventKind};
        let mut app = populated_app_with_settings(Settings {
            show_message_ids: true,
            ..Settings::default()
        });
        app.focus = Focus::Conversation;
        app.sidebar_hidden = true;
        app.selected_message = Some(11);
        let message = &mut app.messages.get_mut(&7).unwrap()[0];
        message.outgoing = true;
        message.sender = "Me".to_owned();
        message.text = "Readable body 你好".to_owned();
        message.reply_to = Some(ReplyInfo {
            message_id: 42,
            chat_id: 7,
            sender: Some("Bob".to_owned()),
        });
        message.attachment = Some(Attachment {
            source_id: None,
            kind: AttachmentKind::Photo,
            file_name: None,
            mime_type: None,
            size: None,
            fallback_emoji: None,
        });
        for width in [40, 100] {
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            terminal.draw(|frame| render(frame, &mut app)).unwrap();
            let buffer = terminal.backend().buffer();
            let row = |y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            };
            assert!(row(1).contains("Me"));
            assert!(!row(1).contains("#11"));
            assert!(!row(1).contains("Readable body"));
            assert!(row(2).contains("│ ↩ Bob"));
            assert!(row(3).contains("Readable body"));
            assert_eq!(buffer[(3, 3)].fg, Color::Reset);
            assert_eq!(buffer[(1, 3)].symbol(), "▎");
            assert!(!row(3).contains("photo"));
            assert_eq!(app.media_slots.len(), 1);
            assert_eq!(app.media_slots[0].viewport.x, 3);
            assert!(app.media_slots[0].viewport.right() < width);
        }
        let commands = app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 8,
            row: 2,
            modifiers: Modifiers::empty(),
        });
        assert_eq!(app.selected_action, 0);
        assert!(render_text_mut(&mut app, 100, 24).contains("│ ↩ Bob"));
        assert!(app.handle_action(KeyAction::Enter).is_empty());
        assert!(matches!(
            commands.first(),
            Some(TelegramCommand::LoadMessage { message_id: 42, .. })
        ));
    }

    #[test]
    fn loaded_reply_excerpt_is_two_rows_and_click_opens_its_exact_target() {
        use crate::event::{NetworkEvent, TelegramCommand};
        use yazi_term::event::{Modifiers, MouseButton, MouseEvent, MouseEventKind};
        let mut app = populated_app();
        app.focus = Focus::Conversation;
        app.sidebar_hidden = true;
        let mut original = app.active_messages()[0].clone();
        original.id = 10;
        original.sender = "Full original author".to_owned();
        original.text = "An excerpt with 中文 content ".repeat(50);
        app.messages.get_mut(&7).unwrap()[0].reply_to = Some(ReplyInfo {
            chat_id: 7,
            message_id: 10,
            sender: None,
        });
        render_text_mut(&mut app, 70, 24);
        let commands = app.request_visible_replies();
        let [TelegramCommand::LoadReplyPreviews { request_id, .. }] = commands.as_slice() else {
            panic!("one batch expected")
        };
        app.handle_network(NetworkEvent::ReplyPreviews {
            chat_id: 7,
            request_id: *request_id,
            messages: vec![original],
            unavailable: Vec::new(),
            complete: true,
        });
        let text = render_text_mut(&mut app, 70, 24);
        assert!(text.contains("Full original author · An excerpt"));
        assert_eq!(text.matches("  │ ").count(), 2);
        assert!(!text.contains("unknown"));
        assert_eq!(app.active_messages().len(), 1);
        assert!(
            app.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 8,
                row: 2,
                modifiers: Modifiers::empty()
            })
            .is_empty()
        );
        assert_eq!(app.selected_message, Some(10));
        assert_eq!(app.viewport_anchor_message, Some(10));
    }

    #[test]
    fn reflow_clamps_the_anchor_to_its_message_when_wrapping_shrinks() {
        let mut app = populated_app();
        app.narrow_conversation = true;
        let mut message = app.active_messages()[0].clone();
        message.text = "long content ".repeat(300);
        let mut messages = vec![message.clone()];
        for id in 12..32 {
            let mut following = message.clone();
            following.id = id;
            following.text = format!("message {id}");
            messages.push(following);
        }
        app.messages.insert(7, messages);
        app.message_scroll = 1;
        app.viewport_anchor_message = Some(11);
        app.viewport_anchor_row = 70;
        render_text_mut(&mut app, 40, 14);
        assert_eq!(app.viewport_anchor_message, Some(11));
        assert!(render_text_mut(&mut app, 120, 14).contains("long content"));
        assert_eq!(app.viewport_anchor_message, Some(11));
        assert_eq!(app.viewport_anchor_row, 0);
        app.sidebar_hidden = true;
        render_text_mut(&mut app, 120, 14);
        assert_eq!(app.viewport_anchor_message, Some(11));
    }

    #[test]
    fn sidebar_toggle_keeps_drafts_and_clears_hidden_pointer_targets() {
        use yazi_term::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};
        let key = KeyEvent::new(KeyCode::Fn(4), Modifiers::empty());
        let mut app = populated_app();
        render_text_mut(&mut app, 100, 24);
        app.handle_action(KeyAction::Character('i'));
        app.update(crate::event::AppEvent::Paste("draft 你好".to_owned()));
        app.handle_key(&key);
        assert!(app.sidebar_hidden);
        assert_eq!(app.mode, Mode::Compose);
        assert_eq!(app.chat_hit_region_count(), 0);
        render_text_mut(&mut app, 100, 24);
        assert_eq!(app.chat_hit_region_count(), 0);
        let mut repeat = key.clone();
        repeat.kind = KeyEventKind::Repeat;
        app.handle_key(&repeat);
        assert!(app.sidebar_hidden);
        app.handle_key(&key);
        render_text_mut(&mut app, 100, 24);
        assert!(!app.sidebar_hidden);
        assert_eq!(app.mode, Mode::Compose);
        assert!(app.chat_hit_region_count() > 0);
        render_text_mut(&mut app, 40, 10);
        app.handle_key(&key);
        assert_eq!(app.mode, Mode::Navigate);
        assert_eq!(app.focus, Focus::Chats);
        assert_eq!(app.active_draft().unwrap().value(), "draft 你好");
        render_text_mut(&mut app, 40, 10);
        assert!(app.chat_hit_region_count() > 0);
        app.handle_key(&key);
        assert!(app.sidebar_hidden);
        app.handle_action(KeyAction::Tab);
        assert!(!app.sidebar_hidden);
    }

    #[test]
    fn chat_columns_keep_alignment_and_semantic_colors_with_unicode_and_icons() {
        let mut app = populated_app();
        let mut second = app.chats[0].clone();
        second.id = 8;
        second.title = "很长的群聊名称 with emoji 🙂".to_owned();
        second.unread = 10_000;
        app.chats.push(second);
        app.keymap
            .colors
            .chats
            .insert(7, crate::appearance::TerminalColor::Red);
        for nerd_font in [false, true] {
            app.keymap.nerd_font = nerd_font;
            let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
            terminal.draw(|frame| render(frame, &mut app)).unwrap();
            let buffer = terminal.backend().buffer();
            let first_time = (19..24)
                .map(|x| buffer[(x, 1)].symbol())
                .collect::<String>();
            let second_time = (19..24)
                .map(|x| buffer[(x, 2)].symbol())
                .collect::<String>();
            assert_eq!(first_time, second_time);
            assert!(first_time.contains(':'));
            assert_eq!(buffer[(19, 1)].fg, Color::Cyan);
            assert_eq!(buffer[(28, 1)].fg, Color::Yellow);
            assert_eq!(buffer[(28, 1)].symbol(), "2");
            assert_eq!(
                (25..29)
                    .map(|x| buffer[(x, 2)].symbol())
                    .collect::<String>(),
                "999+"
            );
            assert_eq!(buffer[(if nerd_font { 5 } else { 3 }, 1)].fg, Color::Red);
        }
    }

    #[test]
    fn statusline_respects_lua_order_and_keeps_mode_and_selection_hints_when_narrow() {
        let mut app = populated_app();
        app.keymap = crate::keymap::Keymap::parse(
            "return {statusline={left={'mode','account'},right={'dc','connection'}}}",
        )
        .unwrap();
        app.metrics.dc_id = Some(4);
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let last_row = terminal.backend().buffer().content()[2300..]
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(last_row.starts_with(" CHATS "));
        assert!(last_row.contains("1 · Me"));
        assert!(last_row.find("DC 4").unwrap() < last_row.find("online").unwrap());
        app.keymap = crate::keymap::Keymap::default();
        app.focus = Focus::Conversation;
        app.selected_message = Some(11);
        app.messages.get_mut(&7).unwrap()[0].attachment = Some(Attachment {
            source_id: None,
            kind: AttachmentKind::Photo,
            file_name: None,
            mime_type: None,
            size: None,
            fallback_emoji: None,
        });
        let narrow = render_text(&app, 40, 10);
        assert!(narrow.contains("SELECT"));
        assert!(narrow.contains("o preview"));
        assert!(
            narrow.contains("O Finder")
                || narrow.contains("O Explorer")
                || narrow.contains("O files")
        );
        app.keymap.statusline.enabled = false;
        app.status_message = Some("Failed to send".to_owned());
        let hidden = render_text(&app, 40, 10);
        assert!(!hidden.contains("SELECT"));
        assert!(hidden.contains("Failed to send"));
    }

    #[test]
    fn pinned_overlay_keeps_navigation_visible_in_a_small_terminal() {
        let mut app = populated_app();
        app.mode = Mode::PinnedMessages;
        app.message_pins.chat = Some(7);
        app.message_pins.page.messages = app.active_messages().to_vec();
        let rendered = render_text(&app, 40, 10);
        assert!(rendered.contains("Pinned messages"));
        assert!(rendered.contains("<Enter> open"));
        assert!(rendered.contains("<Esc> close"));
        assert!(rendered.contains("<C-n> next"));
    }

    #[test]
    fn nerd_font_opt_in_preserves_labels_and_plain_font_fallback() {
        let mut app = populated_app();
        app.folders.push(crate::folders::Folder::archive());
        app.folder_id = 1;
        app.chats[0].membership.archived = true;
        app.pins.dialogs.archive = vec![7];
        let message = &mut app.messages.get_mut(&7).unwrap()[0];
        message.pinned = true;
        message.attachment = Some(Attachment {
            source_id: None,
            kind: AttachmentKind::File,
            file_name: Some("notes.txt".to_owned()),
            mime_type: None,
            size: None,
            fallback_emoji: None,
        });
        assert!(!app.keymap.nerd_font);
        let plain = render_text(&app, 100, 24);
        assert!(plain.contains("Archive"));
        assert!(plain.contains("notes.txt"));
        assert!(
            !plain
                .chars()
                .any(|ch| ('\u{e000}'..='\u{f8ff}').contains(&ch))
        );
        app.keymap = crate::keymap::Keymap::parse("return { nerd_font = true }").unwrap();
        let icons = render_text(&app, 100, 24);
        for glyph in ['\u{f187}', '\u{f075}', '\u{f08d}', '\u{f15b}'] {
            assert!(icons.contains(glyph));
        }
        // TestBackend retains the blank continuation cell of each wide glyph.
        assert!(icons.contains("Alice 東 京"));
        assert!(icons.contains("notes.txt"));
        assert!(icons.contains("P pins"));
        assert!(render_text(&app, 40, 10).contains("Archive"));
        app.keymap = crate::keymap::Keymap::parse("return { nerd_font = false }").unwrap();
        assert_eq!(render_text(&app, 100, 24), plain);
        assert!(crate::keymap::Keymap::parse("return { nerd_font = 'yes' }").is_err());
    }

    fn populated_app_with_settings(settings: Settings) -> AppState {
        let mut app = AppState::with_ephemeral_settings(settings);
        app.screen = Screen::Main;
        app.connection = ConnectionStatus::Online;
        app.user_name = Some("Me".to_owned());
        app.chats.push(Chat {
            read_inbox_max_id: Some(0),
            membership: crate::folders::ChatMembership::default(),
            id: 7,
            title: "Alice 東京".to_owned(),
            kind: ChatKind::Direct,
            unread: 2,
            last_message_id: None,
            last_message: "hello from the terminal".to_owned(),
            last_activity: Some(Utc::now()),
        });
        app.active_chat_id = Some(7);
        app.messages.insert(
            7,
            vec![Message {
                reactions: None,
                poll: None,
                entities: Vec::new(),
                notification: None,
                mention: None,
                edited_at: None,
                pinned: false,
                id: 11,
                chat_id: 7,
                sender_username: None,
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
    fn visible_composer_click_enters_input_and_resets_with_the_frame() {
        use yazi_term::event::{Modifiers, MouseButton, MouseEvent, MouseEventKind};

        let mut app = populated_app();
        app.active_chat_id = None;
        render_text_mut(&mut app, 100, 24);
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 70,
            row: 21,
            modifiers: Modifiers::empty(),
        });
        assert_eq!(app.active_chat_id, Some(7));
        assert_eq!(app.mode, Mode::Compose);
        app.update(crate::event::AppEvent::Paste("你好🙂".to_owned()));
        assert_eq!(app.active_draft().unwrap().value(), "你好🙂");
        app.handle_action(KeyAction::Escape);
        render_text_mut(&mut app, 30, 8);
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 70,
            row: 21,
            modifiers: Modifiers::empty(),
        });
        assert_eq!(app.mode, Mode::Navigate);
    }

    #[test]
    fn expanded_preview_fits_small_windows_and_keeps_close_visible_on_error() {
        let mut app = populated_app();
        app.messages.get_mut(&7).unwrap()[0].attachment = Some(Attachment {
            source_id: None,
            kind: AttachmentKind::Photo,
            file_name: None,
            mime_type: Some("image/jpeg".to_owned()),
            size: None,
            fallback_emoji: None,
        });
        app.mode = Mode::Preview;
        app.selected_message = Some(11);
        app.status_message = Some("Cannot load image. ".repeat(20));
        for (width, height) in [(100, 24), (40, 10)] {
            let output = render_text_mut(&mut app, width, height);
            assert!(output.contains("<Esc> close"));
            assert!(output.contains("Cannot load image"));
            assert_eq!(app.media_slots.len(), 1);
            let slot = &app.media_slots[0];
            assert!(slot.viewport.right() <= width);
            assert!(slot.viewport.bottom() < height);
            assert_eq!(slot.size.width, width - 2);
        }
    }

    #[test]
    fn composer_hint_tracks_the_effective_send_binding() {
        let mut app = populated_app();
        app.mode = Mode::Compose;
        app.keymap = crate::keymap::Keymap::parse("return { keymap={{context='compose',on={'<Enter>'},run='newline'}, {context='compose',on={'<C-s>'},run='send'}} }").unwrap();
        let text = render_text(&app, 100, 24);
        assert!(text.contains("<C-s> to send"));
        assert!(!text.contains("Enter send"));
        app.mode = Mode::Navigate;
        app.focus = Focus::Chats;
        assert!(render_text(&app, 100, 24).contains("i to compose"));
    }

    #[test]
    fn narrow_search_keeps_submit_and_close_controls_visible() {
        let mut app = populated_app();
        app.mode = Mode::Search;
        app.search.editing = true;
        let output = render_text(&app, 40, 14);
        assert!(output.contains("<Enter> search"));
        assert!(output.contains("<Esc> close"));
    }

    #[test]
    fn login_errors_wrap_and_grow_for_all_phone_auth_phases() {
        let error = "Could not request a login code: request error: rpc error 500: AUTH_RESTART";
        for prompt in [
            AuthPrompt::Phone,
            AuthPrompt::Code {
                phone: "+123456789".to_owned(),
            },
            AuthPrompt::Password { hint: None },
        ] {
            let mut app = AppState::with_ephemeral_settings(Settings::default());
            app.handle_network(crate::event::NetworkEvent::Auth(prompt));
            app.status_message = Some(error.to_owned());
            for (width, height) in [(40, 16), (72, 20), (160, 50)] {
                let output = render_text(&app, width, height);
                assert!(
                    output.contains("AUTH_RESTART"),
                    "{width}x{height}: {output}"
                );
                assert!(output.contains("<C-c> quit"), "{output}");
                assert!(output.contains("<Enter> to continue"));
            }
            app.status_message = Some(format!("{} END_OF_ERROR", "连接失败 retry ".repeat(30)));
            let output = render_text(&app, 72, 40);
            assert!(output.contains("END_OF_ERROR"));
            assert!(output.contains("<C-c> quit"), "{output}");
            let small = render_text(&app, 40, 10);
            assert!(small.contains("Resize terminal for more"));
            assert!(small.contains("<C-c> quit"));
        }
    }

    #[test]
    fn connection_and_chat_errors_show_the_end_of_long_messages() {
        let error = format!("{} END_OF_ERROR", "连接失败 request failed ".repeat(6));
        for screen in [Screen::Connecting, Screen::Main] {
            let mut app = populated_app();
            app.screen = screen;
            app.status_message = Some(error.clone());
            for width in [40, 80, 120] {
                let output = render_text(&app, width, 24);
                assert!(output.contains("END_OF_ERROR"), "{width}: {output}");
            }
            app.status_message = Some("failed ".repeat(200));
            assert!(render_text(&app, 40, 10).contains("Resize terminal for more"));
        }
    }

    #[test]
    fn fatal_errors_use_available_height_instead_of_seven_rows() {
        let mut app = populated_app();
        app.screen = Screen::Fatal(format!("{} END_OF_ERROR", "request failed ".repeat(30)));
        let output = render_text(&app, 40, 24);
        assert!(output.contains("END_OF_ERROR"));
        assert!(output.contains("q quit"));
        assert!(output.contains("quit"));
        assert!(render_text(&app, 40, 10).contains("Resize terminal for more"));
    }

    #[test]
    fn qr_errors_wrap_without_clipping_the_code_or_disappearing_on_small_screens() {
        let mut app = AppState::with_ephemeral_settings(Settings::default());
        let url = "tg://login?token=AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";
        app.handle_network(crate::event::NetworkEvent::Auth(AuthPrompt::Qr {
            url: url.to_owned(),
        }));
        app.status_message = Some(format!("{} END_OF_ERROR", "QR login failed ".repeat(8)));
        let output = render_text(&app, 80, 30);
        assert!(output.contains("END_OF_ERROR"));
        assert!(output.contains('▀'));
        assert!(!output.contains(url));
        let short = render_text(&app, 80, 20);
        assert!(short.contains("END_OF_ERROR"));
        assert!(!short.contains('▀'));
        let narrow = render_text(&app, 40, 20);
        assert!(narrow.contains("END_OF_ERROR"));
        assert!(!narrow.contains(url));
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
        assert!(
            result
                .iter()
                .all(|line| UnicodeWidthStr::width(line.as_str()) <= 8)
        );
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
        assert_eq!(output.matches("Reply to #11 Alice").count(), 1);
    }

    #[test]
    fn wide_layout_renders_chat_timeline_and_composer() {
        let app = populated_app();
        let output = render_text(&app, 120, 36);

        assert!(output.contains("Termgram"));
        assert!(output.contains("Alice"));
        assert!(output.contains("hello from the terminal"));
        assert!(output.contains("i to compose"));
    }

    #[test]
    fn settings_overlay_renders_current_download_behavior() {
        let mut app = populated_app();
        app.mode = Mode::Settings;
        let output = render_text(&app, 100, 30);

        assert!(output.contains("Termgram settings"));
        assert!(output.contains("Automatic update checks"));
        assert!(output.contains("Stable"));
        assert!(output.contains("Reveal on activation"));
        assert!(output.contains("Activate a downloaded file to reveal it"));
    }

    #[test]
    fn settings_overlay_reflects_prerelease_and_cache_only() {
        let mut app = AppState::with_settings(
            Settings {
                automatic_update_checks: false,
                release_channel: ReleaseChannel::Prerelease,
                download_behavior: DownloadBehavior::CacheOnly,
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
        assert!(output.contains("Keep in cache"));
        assert!(output.contains("Keep downloads for reuse"));
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
        assert!(output.contains("<Enter> switch"));
    }

    #[test]
    fn replies_render_target_metadata_and_optional_statusline_ids() {
        let mut app = populated_app();
        let message = app.messages.get_mut(&7).unwrap().first_mut().unwrap();
        message.reply_to = Some(ReplyInfo {
            message_id: 42,
            chat_id: 7,
            sender: Some("Bob".to_owned()),
        });

        let without_column = render_text(&app, 100, 30);
        assert!(without_column.contains("│ ↩ Bob"));
        assert!(!without_column.contains("#11"));

        let settings = Settings {
            show_message_ids: true,
            ..Settings::default()
        };
        let mut app = populated_app_with_settings(settings);
        app.focus = Focus::Conversation;
        app.selected_message = Some(11);
        let message = app.messages.get_mut(&7).unwrap().first_mut().unwrap();
        message.reply_to = Some(ReplyInfo {
            message_id: 42,
            chat_id: 7,
            sender: Some("Bob".to_owned()),
        });
        let with_column = render_text(&app, 100, 30);
        assert!(with_column.contains("│ ↩ Bob"));
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
                reactions: None,
                poll: None,
                entities: Vec::new(),
                notification: None,
                mention: None,
                edited_at: None,
                pinned: false,
                id: 12,
                chat_id: 7,
                sender_username: None,
                sender: "Alice".to_owned(),
                reply_to: None,
                text: "receipt".to_owned(),
                timestamp: Utc::now(),
                outgoing: false,
                delivery: Delivery::Read,
                attachment: Some(Attachment {
                    source_id: None,
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
                reactions: None,
                poll: None,
                entities: Vec::new(),
                notification: None,
                mention: None,
                edited_at: None,
                pinned: false,
                id: 13,
                chat_id: 7,
                sender_username: None,
                sender: "Alice".to_owned(),
                reply_to: None,
                text: String::new(),
                timestamp: Utc::now(),
                outgoing: false,
                delivery: Delivery::Read,
                attachment: Some(Attachment {
                    source_id: None,
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

        let output = render_text_mut(&mut app, 120, 36);
        assert!(!output.contains("[photo]"));
        assert!(!output.contains("[sticker]"));
        assert_eq!(app.media_slots.len(), 2);
        assert_eq!(app.request_visible_media().len(), 2);
        app.message_scroll = 3;
        render_text_mut(&mut app, 120, 20);
        assert!(
            app.media_slots
                .iter()
                .all(|s| s.size.height <= s.viewport.height)
        );
        app.mode = Mode::Help;
        render_text_mut(&mut app, 120, 36);
        assert!(app.media_slots.is_empty());
        app.mode = Mode::Navigate;
        render_text_mut(&mut app, 30, 8);
        assert!(app.media_slots.is_empty());
    }

    #[test]
    fn image_placeholder_mode_shows_labels_and_no_media_slots() {
        use crate::transcript::Images;
        let mut app = populated_app();
        app.keymap.messages.images = Images::Placeholder;
        app.messages.get_mut(&7).unwrap().push(Message {
            reactions: None,
            poll: None,
            entities: Vec::new(),
            notification: None,
            mention: None,
            edited_at: None,
            pinned: false,
            id: 12,
            chat_id: 7,
            sender_username: None,
            sender: "Alice".to_owned(),
            reply_to: None,
            text: String::new(),
            timestamp: Utc::now(),
            outgoing: false,
            delivery: Delivery::Read,
            attachment: Some(Attachment {
                source_id: None,
                kind: AttachmentKind::Photo,
                file_name: Some("image.jpg".to_owned()),
                mime_type: Some("image/jpeg".to_owned()),
                size: Some(2048),
                fallback_emoji: None,
            }),
            links: Vec::new(),
            buttons: Vec::new(),
        });
        let output = render_text_mut(&mut app, 120, 36);
        assert!(output.contains("[photo] image.jpg"));
        assert!(app.media_slots.is_empty());
        assert!(app.request_visible_media().is_empty());
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
        assert!(output.contains("<Esc> starts over"));
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
        assert!(output.contains("<Tab> full"));
        assert!(output.contains("<Esc> phone"));
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
        assert!(compatible_text.contains("<Tab> compact"));
        assert!(!compatible_text.contains('▀'));
        assert!(!compatible_text.contains(secret_url));
        assert!(
            buffer
                .content()
                .iter()
                .any(|cell| cell.bg == Color::Rgb(0, 0, 0))
        );
        assert!(
            buffer
                .content()
                .iter()
                .any(|cell| cell.bg == Color::Rgb(255, 255, 255))
        );
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
        assert!(conversation.contains("<Esc> chats"));
    }

    #[test]
    fn non_conversation_and_too_small_frames_clear_pointer_targets() {
        let mut app = populated_app();
        let mut photo = app.messages.get(&7).unwrap()[0].clone();
        photo.id = 12;
        photo.attachment = Some(Attachment {
            source_id: None,
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
                read_inbox_max_id: Some(0),
                membership: crate::folders::ChatMembership::default(),
                id: 100 + index,
                title: format!("Overflow chat {index:02}"),
                kind: ChatKind::Direct,
                unread: 0,
                last_message_id: None,
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
                reactions: None,
                poll: None,
                entities: Vec::new(),
                notification: None,
                mention: None,
                edited_at: None,
                pinned: false,
                id,
                chat_id: 7,
                sender_username: None,
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
                reactions: None,
                poll: None,
                entities: Vec::new(),
                notification: None,
                mention: None,
                edited_at: None,
                pinned: false,
                id: 30,
                chat_id: 7,
                sender_username: None,
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
        assert_eq!(app.message_scroll, 9); // Header and three body lines, no separator.
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
                reactions: None,
                poll: None,
                entities: Vec::new(),
                notification: None,
                mention: None,
                edited_at: None,
                pinned: false,
                id: 99,
                chat_id: 7,
                sender_username: None,
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
    #[test]
    fn visible_mentions_require_a_focused_frame_and_do_not_consume_voice_messages() {
        let mut app = populated_app();
        app.focus = Focus::Conversation;
        app.chats[0].read_inbox_max_id = Some(100);
        let mut text = app.active_messages()[0].clone();
        text.mention = Some(crate::model::Mention {
            unread: true,
            requires_playback: false,
        });
        let mut voice = text.clone();
        voice.id = 12;
        voice.text = "voice reply".into();
        voice.mention.as_mut().unwrap().requires_playback = true;
        app.messages.insert(7, vec![text, voice]);
        assert!(
            app.request_visible_read().is_empty(),
            "loading is not viewing"
        );
        app.terminal_focused = false;
        render_text_mut(&mut app, 100, 24);
        assert!(app.request_visible_read().is_empty());
        app.terminal_focused = true;
        app.mode = Mode::Search;
        render_text_mut(&mut app, 100, 24);
        assert!(
            app.request_visible_read().is_empty(),
            "covered conversation is not visible"
        );
        app.mode = Mode::Navigate;
        render_text_mut(&mut app, 100, 24);
        assert_eq!(
            app.request_visible_read(),
            [crate::event::TelegramCommand::ReadMentions {
                chat_id: 7,
                message_ids: vec![11]
            }]
        );
        assert!(
            app.request_visible_read().is_empty(),
            "one receipt in flight per chat"
        );
        app.handle_network(crate::event::NetworkEvent::MessageContentsRead {
            channel_id: None,
            message_ids: vec![11],
        });
        assert!(!app.active_messages()[0].mention.as_ref().unwrap().unread);
        assert!(app.active_messages()[1].mention.as_ref().unwrap().unread);
        app.handle_network(crate::event::NetworkEvent::MentionsReadFinished {
            chat_id: 7,
            message_ids: vec![11],
            error: None,
        });
        render_text_mut(&mut app, 100, 24);
        assert!(app.request_visible_read().is_empty());
    }
}
