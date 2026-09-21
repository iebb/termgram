//! `@`-mention and `/`-command suggestions floating above the composer.
//!
//! The popup reuses the command picker's row layout; the draft itself stays
//! in the composer, which keeps cursor ownership single.

use super::{
    ACCENT, AppState, Clear, Frame, MUTED, Paragraph, Rect, Style, clamp_u16, pane_block,
    truncate_cells,
};
use crate::completion::Trigger;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

const MAX_ROWS: u16 = 8;

pub(super) fn render(frame: &mut Frame<'_>, app: &mut AppState) {
    let Some(popup) = app.completion_popup() else {
        return;
    };
    let Some((x, right, top, _)) = app.composer_region() else {
        return;
    };
    let count = clamp_u16(popup.candidates.len()).min(MAX_ROWS);
    let height = 2 + count;
    if top < height {
        return;
    }
    let area = Rect::new(x, top - height, right.saturating_sub(x), height);
    frame.render_widget(Clear, area);
    let title = match popup.trigger {
        Trigger::Mention => " Mention ".to_owned(),
        Trigger::Command => " Command ".to_owned(),
    };
    let block = pane_block(title, true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    // Keep the highlighted row visible, like the command candidate window.
    let selected = popup.selected.unwrap_or(0).min(popup.candidates.len() - 1);
    let start = selected
        .saturating_add(1)
        .saturating_sub(usize::from(count));
    let mut hits = Vec::with_capacity(usize::from(count));
    for (offset, (index, suggestion)) in popup
        .candidates
        .iter()
        .enumerate()
        .skip(start)
        .take(usize::from(count))
        .enumerate()
    {
        let row = Rect::new(inner.x, inner.y + clamp_u16(offset), inner.width, 1);
        let prefix = if index == selected { "› " } else { "  " };
        let width = usize::from(inner.width);
        let label_width = (width / 3).clamp(8, 28);
        let label = truncate_cells(&suggestion.label, label_width);
        let detail = truncate_cells(
            &suggestion.detail,
            width.saturating_sub(2 + label_width + 2),
        );
        let style = if index == selected {
            Style::default().reversed()
        } else {
            Style::default()
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!(
                        "{prefix}{label}{}  ",
                        " ".repeat(label_width.saturating_sub(label.width()))
                    ),
                    Style::default().fg(ACCENT),
                ),
                Span::styled(detail, Style::default().fg(MUTED)),
            ]))
            .style(style),
            row,
        );
        hits.push((row.x, row.right(), row.y, index));
    }
    app.completion_hit_regions.extend(hits);
}
