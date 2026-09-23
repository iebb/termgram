//! Sticker panel: sidebar sections plus a thumbnail grid in a centered overlay.
use super::{
    ACCENT, MUTED, WARNING, centered, clamp_u16, pane_block, spinner, truncate_cells,
    vertically_centered, wrap_cells,
};
use crate::{
    app::{
        AppState,
        stickers::{SectionView, Thumb},
    },
    keymap::Context,
    media::{MediaSlot, MediaSource},
    model::StickerRef,
};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect, Size},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

const SIDEBAR_WIDTH: u16 = 24;
const IMAGE_WIDTH: u16 = 10;
const IMAGE_HEIGHT: u16 = 5;
const CELL_WIDTH: u16 = IMAGE_WIDTH + 2;
const CELL_HEIGHT: u16 = IMAGE_HEIGHT + 2;

pub(super) fn render(frame: &mut Frame<'_>, area: Rect, app: &mut AppState) {
    if app.stickers.panel.is_none() {
        return;
    }
    app.media_slots.clear();
    let popup = centered(
        area,
        area.width.saturating_sub(4).min(100),
        area.height.saturating_sub(2).min(30),
    );
    frame.render_widget(Clear, popup);
    let chat = app
        .active_chat()
        .map_or("Stickers", |chat| chat.title.as_str());
    let block = pane_block(format!(" Stickers · {chat} "), true);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
    let panes = Layout::horizontal([
        Constraint::Length(SIDEBAR_WIDTH.min(rows[0].width)),
        Constraint::Min(1),
    ])
    .split(rows[0]);
    render_sections(frame, panes[0], app);
    render_grid(frame, panes[1], app);
    render_footer(frame, rows[1], app);
}

fn render_sections(frame: &mut Frame<'_>, area: Rect, app: &mut AppState) {
    let total = app.stickers.section_count();
    let visible = usize::from(area.height).max(1);
    let Some(panel) = &mut app.stickers.panel else {
        return;
    };
    if panel.section < panel.section_top {
        panel.section_top = panel.section;
    }
    if panel.section >= panel.section_top + visible {
        panel.section_top = (panel.section + 1).saturating_sub(visible);
    }
    let top = panel.section_top;
    let current = panel.section;
    app.stickers.hit_rows.clear();
    let lines = (top..total.min(top + visible))
        .map(|index| {
            let title = app.stickers.section_title(index);
            app.stickers.hit_rows.push((
                area.x,
                area.right(),
                area.y.saturating_add(clamp_u16(index - top)),
                index,
            ));
            Line::styled(
                truncate_cells(&format!(" {title}"), usize::from(area.width)),
                if index == current {
                    Style::default().fg(ACCENT).add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                },
            )
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), area);
}

#[allow(clippy::too_many_lines)]
fn render_grid(frame: &mut Frame<'_>, area: Rect, app: &mut AppState) {
    app.stickers.hit_cells.clear();
    let cols = (usize::from(area.width) / usize::from(CELL_WIDTH)).max(1);
    let rows = (usize::from(area.height) / usize::from(CELL_HEIGHT)).max(1);
    let per_page = cols.saturating_mul(rows);
    let Some(panel) = &mut app.stickers.panel else {
        return;
    };
    panel.grid_cols = cols;
    panel.grid_rows = rows;
    if panel.selected < panel.top {
        panel.top = panel.selected;
    }
    if panel.selected >= panel.top + per_page {
        panel.top = (panel.selected + 1).saturating_sub(per_page);
    }
    panel.top = panel.top / cols * cols;
    let section = panel.section;
    let selected = panel.selected;
    let top = panel.top;
    let loading = panel.loading.is_some();
    let error = panel.error.clone();

    let view = app.stickers.section_view(section);
    let data = match &view {
        SectionView::Ready(stickers) => *stickers,
        _ => &[],
    };
    let notice = if data.is_empty() {
        if section < 2 && loading {
            Some((format!("{} Loading stickers…", spinner(app.tick)), false))
        } else if section < 2 && error.is_some() {
            error.map(|error| (error, true))
        } else {
            match &view {
                SectionView::Loading => {
                    Some((format!("{} Loading stickers…", spinner(app.tick)), false))
                }
                SectionView::Failed(failure) => Some((
                    format!(
                        "{failure} · {} retry",
                        app.keymap.hint(Context::Stickers, "refresh")
                    ),
                    true,
                )),
                SectionView::Ready(_) => Some((
                    match section {
                        0 => "No recent stickers yet".to_owned(),
                        1 => "No favorite stickers yet".to_owned(),
                        _ => "This sticker set is empty".to_owned(),
                    },
                    false,
                )),
            }
        }
    } else {
        None
    };
    if let Some((notice, failed)) = notice {
        let width = usize::from(area.width.saturating_sub(2)).max(1);
        let lines = wrap_cells(&notice, width);
        let height = clamp_u16(lines.len());
        frame.render_widget(
            Paragraph::new(
                lines
                    .into_iter()
                    .map(|line| {
                        Line::styled(
                            line,
                            Style::default().fg(if failed { WARNING } else { MUTED }),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
            .alignment(Alignment::Center),
            vertically_centered(area, height),
        );
        return;
    }
    let visible: Vec<StickerRef> = data.iter().skip(top).take(per_page).cloned().collect();
    for (offset, sticker) in visible.iter().enumerate() {
        let index = top + offset;
        let column = clamp_u16(offset % cols);
        let row = clamp_u16(offset / cols);
        let cell = Rect::new(
            area.x.saturating_add(column * CELL_WIDTH),
            area.y.saturating_add(row * CELL_HEIGHT),
            CELL_WIDTH.min(
                area.right()
                    .saturating_sub(area.x.saturating_add(column * CELL_WIDTH)),
            ),
            CELL_HEIGHT.min(
                area.bottom()
                    .saturating_sub(area.y.saturating_add(row * CELL_HEIGHT)),
            ),
        );
        if cell.width < 3 || cell.height < 3 {
            continue;
        }
        app.stickers
            .hit_cells
            .push((cell.x, cell.right(), cell.y, cell.bottom(), index));
        let current = index == selected;
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(if current {
                BorderType::Double
            } else {
                BorderType::Rounded
            })
            .border_style(Style::default().fg(if current { ACCENT } else { MUTED }));
        let image = block.inner(cell);
        frame.render_widget(block, cell);
        if let Some(Thumb::Ready(path)) = app.stickers.thumb(sticker.id) {
            app.media_slots.push(MediaSlot {
                source: MediaSource::File(path.clone()),
                viewport: image,
                offset: 0,
                size: Size::new(image.width, image.height),
            });
        } else {
            let emoji = if sticker.emoji.is_empty() {
                "◻"
            } else {
                sticker.emoji.as_str()
            };
            frame.render_widget(
                Paragraph::new(emoji)
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(MUTED)),
                vertically_centered(image, 1),
            );
            app.stickers.queue_thumb(sticker);
        }
    }
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    let Some(panel) = &app.stickers.panel else {
        return;
    };
    let hint = |action: &str| app.keymap.hint(Context::Stickers, action);
    let close = format!("{} close ", hint("cancel"));
    let footer = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(clamp_u16(close.width())),
    ])
    .split(area);
    let (controls, failed) = if let Some(error) = &panel.error {
        (format!(" {} · {} retry ", error, hint("refresh")), true)
    } else {
        (
            format!(
                " {} send · {}/{} sets · {} refresh ",
                hint("open"),
                hint("sticker_set_previous"),
                hint("sticker_set_next"),
                hint("refresh")
            ),
            false,
        )
    };
    frame.render_widget(
        Paragraph::new(truncate_cells(&controls, usize::from(footer[0].width)))
            .style(Style::default().fg(if failed { WARNING } else { MUTED })),
        footer[0],
    );
    frame.render_widget(
        Paragraph::new(close)
            .style(Style::default().fg(MUTED))
            .alignment(Alignment::Right),
        footer[1],
    );
}
