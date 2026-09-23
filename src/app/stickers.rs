use super::{App, Mode, Screen};
use crate::{
    actions::Action,
    event::{NetworkEvent, TelegramCommand},
    model::{
        Attachment, AttachmentKind, Delivery, Message, StickerOverview, StickerRef, StickerSetRef,
        sanitize_terminal_line,
    },
};
use chrono::Utc;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::PathBuf,
};

const MAX_THUMB_DOWNLOADS: usize = 4;

#[derive(Clone, Default)]
pub struct State {
    pub panel: Option<Panel>,
    /// Sidebar rows from the last render: x start/end, y, section index.
    pub hit_rows: Vec<(u16, u16, u16, usize)>,
    /// Grid cells from the last render: left, right, top, bottom, sticker index.
    pub hit_cells: Vec<(u16, u16, u16, u16, usize)>,
    recent: Vec<StickerRef>,
    favorites: Vec<StickerRef>,
    sets: Vec<StickerSetRef>,
    documents: BTreeMap<i64, SetDocuments>,
    thumbs: BTreeMap<i64, Thumb>,
    loading_thumbs: BTreeSet<i64>,
    queued_thumbs: VecDeque<StickerRef>,
    next: u64,
}

#[derive(Clone)]
pub struct Panel {
    pub section: usize,
    pub selected: usize,
    pub top: usize,
    pub section_top: usize,
    /// Grid geometry from the last render, so key navigation matches visuals.
    pub grid_cols: usize,
    pub grid_rows: usize,
    pub loading: Option<u64>,
    pub error: Option<String>,
}

impl Panel {
    fn new() -> Self {
        Self {
            section: 0,
            selected: 0,
            top: 0,
            section_top: 0,
            grid_cols: 0,
            grid_rows: 0,
            loading: None,
            error: None,
        }
    }
}

#[derive(Clone)]
enum SetDocuments {
    Loading(u64),
    /// Cached documents on display while Telegram revalidates the row.
    Validating(u64, Vec<StickerRef>),
    Loaded(Vec<StickerRef>),
    Failed(String),
}

#[derive(Clone)]
pub enum Thumb {
    Loading,
    Ready(PathBuf),
    Failed,
}

/// What the grid can show for one sidebar section.
pub enum SectionView<'a> {
    Loading,
    Failed(&'a str),
    Ready(&'a [StickerRef]),
}

impl State {
    #[must_use]
    pub fn section_count(&self) -> usize {
        2 + self.sets.len()
    }

    #[must_use]
    pub fn section_title(&self, index: usize) -> String {
        match index {
            0 => "Recent".to_owned(),
            1 => "Favorites".to_owned(),
            _ => self
                .sets
                .get(index.saturating_sub(2))
                .map_or_else(|| "Stickers".to_owned(), |set| set.title.clone()),
        }
    }

    #[must_use]
    pub fn section_view(&self, index: usize) -> SectionView<'_> {
        match index {
            0 => SectionView::Ready(&self.recent),
            1 => SectionView::Ready(&self.favorites),
            _ => match self
                .sets
                .get(index.saturating_sub(2))
                .and_then(|set| self.documents.get(&set.id))
            {
                Some(SetDocuments::Loaded(stickers) | SetDocuments::Validating(_, stickers)) => {
                    SectionView::Ready(stickers)
                }
                Some(SetDocuments::Failed(error)) => SectionView::Failed(error),
                _ => SectionView::Loading,
            },
        }
    }

    #[must_use]
    pub fn thumb(&self, document_id: i64) -> Option<&Thumb> {
        self.thumbs.get(&document_id)
    }

    pub fn queue_thumb(&mut self, sticker: &StickerRef) {
        if sticker.thumb_size.is_none()
            || self.thumbs.contains_key(&sticker.id)
            || self.loading_thumbs.contains(&sticker.id)
            || self
                .queued_thumbs
                .iter()
                .any(|queued| queued.id == sticker.id)
        {
            return;
        }
        self.queued_thumbs.push_back(sticker.clone());
    }
}

impl App {
    fn next_sticker_request(&mut self) -> u64 {
        self.stickers.next = self.stickers.next.wrapping_add(1).max(1);
        self.stickers.next
    }

    pub(super) fn begin_stickers(&mut self) -> Vec<TelegramCommand> {
        if self.screen != Screen::Main
            || self.mode != Mode::Compose
            || self.active_chat_id.is_none()
        {
            return Vec::new();
        }
        // File references expire, so every open refetches the lists; only set
        // documents and finished thumbnails are reused within the session.
        self.stickers.recent.clear();
        self.stickers.favorites.clear();
        self.stickers.sets.clear();
        self.stickers
            .thumbs
            .retain(|_, thumb| !matches!(thumb, Thumb::Failed));
        self.stickers.panel = Some(Panel::new());
        self.mode = Mode::Stickers;
        self.status_message = None;
        self.load_stickers()
    }

    fn close_stickers(&mut self) {
        self.stickers.panel = None;
        self.stickers.queued_thumbs.clear();
        if self.mode == Mode::Stickers {
            self.mode = Mode::Compose;
        }
    }

    fn load_stickers(&mut self) -> Vec<TelegramCommand> {
        let request_id = self.next_sticker_request();
        let Some(panel) = &mut self.stickers.panel else {
            return Vec::new();
        };
        if panel.loading.is_some() {
            return Vec::new();
        }
        panel.loading = Some(request_id);
        panel.error = None;
        vec![TelegramCommand::LoadStickers {
            request_id,
            cached: None,
        }]
    }

    fn ensure_sticker_section(&mut self) -> Vec<TelegramCommand> {
        let Some(panel) = &self.stickers.panel else {
            return Vec::new();
        };
        let Some(set) = panel
            .section
            .checked_sub(2)
            .and_then(|index| self.stickers.sets.get(index))
            .cloned()
        else {
            return Vec::new();
        };
        if matches!(
            self.stickers.documents.get(&set.id),
            Some(SetDocuments::Loading(_) | SetDocuments::Validating(..) | SetDocuments::Loaded(_))
        ) {
            return Vec::new();
        }
        let request_id = self.next_sticker_request();
        self.stickers
            .documents
            .insert(set.id, SetDocuments::Loading(request_id));
        vec![TelegramCommand::LoadStickerSet {
            set,
            request_id,
            cached: None,
        }]
    }

    pub(super) fn sticker_binding(
        &mut self,
        action: &Action,
        count: usize,
    ) -> Option<Vec<TelegramCommand>> {
        if *action == Action::Stickers {
            return Some(self.begin_stickers());
        }
        if self.mode != Mode::Stickers {
            return None;
        }
        let mut commands = Vec::new();
        match action {
            Action::Cancel => self.close_stickers(),
            Action::Refresh => {
                commands = self.load_stickers();
                commands.extend(self.ensure_sticker_section());
            }
            Action::Open | Action::Send => commands = self.send_selected_sticker(),
            Action::StickerSetNext => commands = self.switch_sticker_section(true, count),
            Action::StickerSetPrevious => commands = self.switch_sticker_section(false, count),
            Action::Up
            | Action::Down
            | Action::Left
            | Action::Right
            | Action::PageUp
            | Action::PageDown
            | Action::Home
            | Action::End => self.move_sticker_selection(action, count),
            Action::Quit | Action::Redraw => return None,
            _ => {}
        }
        Some(commands)
    }

    fn switch_sticker_section(&mut self, next: bool, count: usize) -> Vec<TelegramCommand> {
        let total = self.stickers.section_count();
        if total == 0 {
            return Vec::new();
        }
        let step = count.max(1) % total;
        let Some(panel) = &mut self.stickers.panel else {
            return Vec::new();
        };
        panel.section = if next {
            (panel.section + step) % total
        } else {
            (panel.section + total - step) % total
        };
        panel.selected = 0;
        panel.top = 0;
        self.ensure_sticker_section()
    }

    pub(super) fn select_sticker_section(&mut self, index: usize) -> Vec<TelegramCommand> {
        if self.mode != Mode::Stickers || index >= self.stickers.section_count() {
            return Vec::new();
        }
        if let Some(panel) = &mut self.stickers.panel {
            panel.section = index;
            panel.selected = 0;
            panel.top = 0;
        }
        self.ensure_sticker_section()
    }

    fn move_sticker_selection(&mut self, action: &Action, count: usize) {
        let Some(section) = self.stickers.panel.as_ref().map(|panel| panel.section) else {
            return;
        };
        let len = match self.stickers.section_view(section) {
            SectionView::Ready(stickers) => stickers.len(),
            _ => 0,
        };
        let Some(panel) = &mut self.stickers.panel else {
            return;
        };
        if len == 0 {
            panel.selected = 0;
            panel.top = 0;
            return;
        }
        let cols = panel.grid_cols.max(1);
        let page = (panel.grid_rows.max(1) * cols).max(cols);
        let step = match action {
            Action::Left | Action::Right => count.max(1),
            Action::Up | Action::Down => cols.saturating_mul(count.max(1)),
            _ => page.saturating_mul(count.max(1)),
        };
        panel.selected = match action {
            Action::Left | Action::Up | Action::PageUp => panel.selected.saturating_sub(step),
            Action::Home => 0,
            Action::End => len - 1,
            _ => panel.selected.saturating_add(step).min(len - 1),
        };
    }

    pub(super) fn click_sticker_cell(&mut self, index: usize) -> Vec<TelegramCommand> {
        let Some(panel) = &mut self.stickers.panel else {
            return Vec::new();
        };
        if panel.selected == index {
            return self.send_selected_sticker();
        }
        panel.selected = index;
        Vec::new()
    }

    fn send_selected_sticker(&mut self) -> Vec<TelegramCommand> {
        if self.mode != Mode::Stickers {
            return Vec::new();
        }
        let Some(chat_id) = self.active_chat_id else {
            return Vec::new();
        };
        let sticker = self.stickers.panel.as_ref().and_then(|panel| {
            match self.stickers.section_view(panel.section) {
                SectionView::Ready(stickers) => stickers.get(panel.selected).cloned(),
                _ => None,
            }
        });
        let Some(sticker) = sticker else {
            return Vec::new();
        };
        if let Some(reason) = self.draft_restriction(chat_id) {
            self.status_message = Some(format!("{reason} · :info for details"));
            return Vec::new();
        }
        let reply_to = self.draft_data_mut(chat_id).reply.take();
        let local_id = self.next_pending_id;
        self.next_pending_id = self.next_pending_id.checked_sub(1).unwrap_or(-1);
        let pending = Message {
            reactions: None,
            poll: None,
            entities: Vec::new(),
            notification: None,
            mention: None,
            edited_at: None,
            pinned: false,
            id: local_id,
            chat_id,
            sender_username: None,
            sender: "You".to_owned(),
            reply_to: reply_to.clone(),
            text: String::new(),
            timestamp: Utc::now(),
            outgoing: true,
            delivery: Delivery::Pending,
            attachment: Some(Attachment {
                source_id: Some(sticker.id),
                kind: AttachmentKind::Sticker,
                file_name: Some(sticker_file_name(&sticker.mime_type)),
                mime_type: Some(sticker.mime_type.clone()),
                size: None,
                fallback_emoji: (!sticker.emoji.is_empty()).then(|| sticker.emoji.clone()),
            }),
            links: Vec::new(),
            buttons: Vec::new(),
        };
        let mut commands = self.receive_message(pending, true, false);
        commands.push(TelegramCommand::SendSticker {
            chat_id,
            local_id,
            sticker,
            reply_to: reply_to.map(|reply| reply.message_id),
        });
        self.close_stickers();
        self.status_message = None;
        commands
    }

    fn clamp_sticker_selection(&mut self) {
        let total = self.stickers.section_count();
        if let Some(panel) = self.stickers.panel.as_mut()
            && panel.section >= total
        {
            panel.section = 0;
            panel.selected = 0;
            panel.top = 0;
        }
        let Some(section) = self.stickers.panel.as_ref().map(|panel| panel.section) else {
            return;
        };
        let len = match self.stickers.section_view(section) {
            SectionView::Ready(stickers) => stickers.len(),
            _ => 0,
        };
        if let Some(panel) = self.stickers.panel.as_mut() {
            panel.selected = panel.selected.min(len.saturating_sub(1));
            if len == 0 {
                panel.top = 0;
            }
        }
    }

    pub(super) fn observe_stickers(
        &mut self,
        event: &NetworkEvent,
    ) -> Option<Vec<TelegramCommand>> {
        match event {
            NetworkEvent::StickersLoaded {
                request_id,
                validated,
                result,
            } => Some(self.stickers_loaded(*request_id, *validated, result)),
            NetworkEvent::StickerSetLoaded {
                request_id,
                set_id,
                validated,
                result,
            } => {
                self.sticker_set_loaded(*request_id, *set_id, *validated, result);
                Some(Vec::new())
            }
            NetworkEvent::StickerThumbDownloaded {
                document_id,
                result,
                ..
            } => {
                self.stickers.loading_thumbs.remove(document_id);
                let thumb = match result {
                    Ok(path) => Thumb::Ready(path.clone()),
                    Err(_) => Thumb::Failed,
                };
                self.stickers.thumbs.insert(*document_id, thumb);
                Some(Vec::new())
            }
            NetworkEvent::StickerSendFailed {
                chat_id,
                local_id,
                error,
            } => {
                if let Some(message) = self.messages.get_mut(chat_id).and_then(|messages| {
                    messages.iter_mut().find(|message| message.id == *local_id)
                }) {
                    message.delivery = Delivery::Failed;
                    if self.active_chat_id == Some(*chat_id) {
                        self.selected_message = Some(*local_id);
                        self.selected_action = 0;
                    }
                    self.status_message = Some(format!(
                        "Sticker not sent: {}",
                        sanitize_terminal_line(error)
                    ));
                }
                Some(Vec::new())
            }
            _ => None,
        }
    }

    fn stickers_loaded(
        &mut self,
        request_id: u64,
        validated: bool,
        result: &Result<StickerOverview, String>,
    ) -> Vec<TelegramCommand> {
        if self
            .stickers
            .panel
            .as_ref()
            .is_none_or(|panel| panel.loading != Some(request_id))
        {
            return Vec::new();
        }
        match result {
            Ok(overview) => {
                self.stickers.recent.clone_from(&overview.recent.items);
                self.stickers
                    .favorites
                    .clone_from(&overview.favorites.items);
                self.stickers.sets.clone_from(&overview.sets.items);
                if validated && let Some(panel) = &mut self.stickers.panel {
                    panel.loading = None;
                    panel.error = None;
                }
                self.clamp_sticker_selection();
                self.ensure_sticker_section()
            }
            Err(error) => {
                // Revalidation failed; cached sections stay on display and the
                // footer carries the error instead.
                if let Some(panel) = &mut self.stickers.panel {
                    panel.loading = None;
                    panel.error = Some(sanitize_terminal_line(error));
                }
                Vec::new()
            }
        }
    }

    fn sticker_set_loaded(
        &mut self,
        request_id: u64,
        set_id: i64,
        validated: bool,
        result: &Result<crate::model::StickerSection<StickerRef>, String>,
    ) {
        let cached = match self.stickers.documents.get(&set_id) {
            Some(SetDocuments::Loading(current)) if *current == request_id => None,
            Some(SetDocuments::Validating(current, documents)) if *current == request_id => {
                Some(documents.clone())
            }
            _ => return,
        };
        let state = match result {
            Ok(section) if !validated => {
                SetDocuments::Validating(request_id, section.items.clone())
            }
            Ok(section) => SetDocuments::Loaded(section.items.clone()),
            Err(error) => match cached {
                Some(documents) => {
                    if let Some(panel) = &mut self.stickers.panel {
                        panel.error = Some(sanitize_terminal_line(error));
                    }
                    SetDocuments::Loaded(documents)
                }
                None => SetDocuments::Failed(sanitize_terminal_line(error)),
            },
        };
        self.stickers.documents.insert(set_id, state);
        self.clamp_sticker_selection();
    }

    /// Called after drawing: visible cells queue their thumbnails, and a small
    /// download budget keeps at most a few transfers in flight.
    pub fn request_sticker_thumbs(&mut self) -> Vec<TelegramCommand> {
        if self.mode != Mode::Stickers || self.stickers.panel.is_none() {
            return Vec::new();
        }
        let mut commands = Vec::new();
        while self.stickers.loading_thumbs.len() < MAX_THUMB_DOWNLOADS {
            let Some(sticker) = self.stickers.queued_thumbs.pop_front() else {
                break;
            };
            if self.stickers.thumbs.contains_key(&sticker.id) {
                continue;
            }
            let request_id = self.next_sticker_request();
            self.stickers.loading_thumbs.insert(sticker.id);
            self.stickers.thumbs.insert(sticker.id, Thumb::Loading);
            commands.push(TelegramCommand::DownloadStickerThumb {
                request_id,
                sticker,
            });
        }
        commands
    }
}

fn sticker_file_name(mime_type: &str) -> String {
    match mime_type {
        "application/x-tgsticker" => "sticker.tgs",
        "video/webm" => "sticker.webm",
        _ => "sticker.webp",
    }
    .to_owned()
}
