//! Pure, single-owner application state and transitions.

mod alerts;
mod appearance;
mod attachments;
pub(crate) mod chat_info;
mod commands;
mod completion;
mod composition;
pub(crate) mod configuration;
mod deletion;
mod drafts;
mod editing;
mod entities;
mod forwarding;
pub(crate) mod help;
pub(crate) mod invites;
mod message_pins;
mod message_views;
mod notifications;
mod pins;
pub(crate) mod polls;
pub(crate) mod reactions;
mod reads;
mod replies;
pub(crate) mod search;
mod sharing;
mod staging;
pub(crate) mod stickers;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::Utc;
use yazi_term::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};

use crate::{
    actions::Action,
    config::{DownloadBehavior, MAX_ACCOUNTS, Settings},
    event::{AppEvent, AuthPrompt, ConnectionStatus, NetworkEvent, TelegramCommand},
    input::{KeyAction, TextInput},
    keymap::Context,
    model::{
        Attachment, AttachmentKind, Chat, ChatId, Delivery, Message, MessageLink, ReplyInfo,
        sanitize_terminal_line, sanitize_terminal_text,
    },
};

const PAGE_STEP: usize = 10;
const MAX_MESSAGES_PER_CHAT: usize = 160;
const MAX_CACHED_CHATS: usize = 12;

type MessageHitRegion = (u16, u16, u16, (i32, Option<usize>));

/// Download state for an attachment displayed in the conversation.
#[derive(Clone, Debug)]
pub struct MediaPreview {
    pub chat_id: ChatId,
    pub message_id: i32,
    pub request_id: u64,
    pub path: Option<PathBuf>,
    pub status: String,
    pub loading: bool,
    thumbnail: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachmentState {
    Ready,
    Downloading,
    Downloaded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageAction {
    Attachment,
    Reply,
    Spoilers,
    ExpandQuote,
    Poll,
    Reactions,
    Link(usize),
    Button(usize),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Screen {
    Connecting,
    Auth(AuthPhase),
    Main,
    Fatal(String),
}

#[derive(Clone, Eq, PartialEq)]
pub enum AuthPhase {
    Phone,
    /// A transient login credential rendered only as a QR code. Never expose
    /// the underlying URL as text, status, or clipboard content.
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

/// Terminal-cell strategy used to draw the transient login QR code.
///
/// Compact mode fits a typical 80 x 24 terminal by using Unicode half blocks.
/// Compatible mode avoids block glyphs entirely and draws one background cell
/// per module, at the cost of needing a taller terminal.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum QrRenderMode {
    #[default]
    Compact,
    Compatible,
}

impl QrRenderMode {
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Compact => Self::Compatible,
            Self::Compatible => Self::Compact,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuthProgress {
    RequestCode,
    StartQr,
    CheckCode,
    CheckPassword,
    WaitQr,
    Restart,
}

impl std::fmt::Debug for AuthPhase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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

impl From<AuthPrompt> for AuthPhase {
    fn from(prompt: AuthPrompt) -> Self {
        match prompt {
            AuthPrompt::Phone => Self::Phone,
            AuthPrompt::Qr { url } => Self::Qr { url },
            AuthPrompt::Code { phone } => Self::Code {
                phone: sanitize_terminal_line(&phone),
            },
            AuthPrompt::Password { hint } => Self::Password {
                hint: hint.map(|value| sanitize_terminal_line(&value)),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Mode {
    #[default]
    Navigate,
    Compose,
    Edit,
    DeletePrompt,
    Invite,
    ChatInfo,
    ForwardPrompt,
    Poll,
    Reactions,
    Stickers,
    Command,
    Status,
    Attachments,
    Preview,
    Filter,
    Search,
    PinnedMessages,
    PinPrompt,
    Colors,
    Help,
    Settings,
    Accounts,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Focus {
    #[default]
    Chats,
    Conversation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReplyRequest {
    source_chat: ChatId,
    source_message: i32,
    target_chat: ChatId,
    target_message: i32,
    request_id: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AttachmentRetry {
    attachment: crate::staging::Attachment,
    caption: String,
    reply_to: Option<ReplyInfo>,
}

/// All mutable UI state. It owns no terminal or network resources.
#[derive(Clone)]
// These flags describe independent terminal, network, and viewport state; a
// single enum would permit invalid combinations instead of preventing them.
#[allow(clippy::struct_excessive_bools)]
pub struct App {
    pub screen: Screen,
    pub mode: Mode,
    pub focus: Focus,
    pub connection: ConnectionStatus,
    pub metrics: crate::statusline::Metrics,
    pub notifications: notifications::State,
    alerts: alerts::State,
    pub user_name: Option<String>,
    pub account_user_id: Option<i64>,
    pub appearance: crate::appearance::Preferences,
    navigation: composition::Navigation,
    pub color_picker: Option<appearance::Picker>,
    pub chats: Vec<Chat>,
    pub folders: Vec<crate::folders::Folder>,
    pub folder_id: i32,
    pub pins: pins::State,
    pub message_pins: message_pins::State,
    pub polls: polls::State,
    pub reactions: reactions::State,
    pub stickers: stickers::State,
    replies: replies::State,
    /// Position in the filtered chat list, never a persistent model identity.
    pub selected_chat: usize,
    pub active_chat_id: Option<ChatId>,
    pub messages: BTreeMap<ChatId, Vec<Message>>,
    /// Actionable message selected for Enter activation.
    pub selected_message: Option<i32>,
    /// Action within the selected message. Messages may contain several links
    /// and inline bot buttons, so selection cannot be message-only.
    pub selected_action: usize,
    text_visibility: BTreeMap<(ChatId, i32), entities::Visibility>,
    pub filter: TextInput,
    pub search: search::State,
    pub commands: commands::State,
    /// Active `@`/`/` completion popup in the composer.
    completion: Option<completion::Popup>,
    /// Per-chat member/command data loaded on first use.
    chat_completion: BTreeMap<ChatId, crate::completion::ChatCompletion>,
    /// Chats whose member fetch was already queued this session.
    completion_requested: BTreeSet<ChatId>,
    /// Rendered popup rows: x start/end, y, candidate index.
    pub completion_hit_regions: Vec<(u16, u16, u16, usize)>,
    next_completion_request_id: u64,
    pub attachment_draft: staging::State,
    pub editing: editing::State,
    pub deletion: deletion::State,
    pub invites: invites::State,
    pub chat_info: chat_info::State,
    pub sharing: sharing::State,
    pub forwarding: forwarding::State,
    pub narrow_conversation: bool,
    pub sidebar_hidden: bool,
    pub should_quit: bool,
    pub status_message: Option<String>,
    pub media_previews: BTreeMap<(ChatId, i32), MediaPreview>,
    pub media_slots: Vec<crate::media::MediaSlot>,
    next_preview_request_id: u64,
    available_update: Option<String>,
    pub tick: u64,
    pub loading_history: bool,
    /// Number of rendered terminal rows between the detached viewport and the latest item.
    pub message_scroll: usize,
    /// Timeline entries appended while the viewport is detached.
    pub new_messages_while_scrolled: usize,
    /// Newly appended entries whose rendered height has not yet been folded
    /// into [`Self::message_scroll`]. The renderer consumes this exactly once.
    pub new_messages_to_anchor: usize,
    /// Semantic top-of-viewport anchor used to survive wrapping changes.
    pub viewport_anchor_message: Option<i32>,
    pub viewport_anchor_row: usize,
    /// Last rendered conversation width, used to detect a wrapping change.
    pub viewport_width: u16,
    pub terminal_focused: bool,
    pub terminal_background: Option<[u8; 3]>,
    settings: Settings,
    settings_path: Option<PathBuf>,
    settings_selection: usize,
    account_selection: usize,
    auth_input: TextInput,
    /// An authentication operation is awaiting a worker response. This keeps
    /// the form visibly responsive without retaining or echoing secrets.
    auth_progress: Option<AuthProgress>,
    qr_render_mode: QrRenderMode,
    /// Ignore late prompts from an authentication attempt after the user has
    /// explicitly restarted it, until the worker confirms the phone phase.
    auth_restart_pending: bool,
    drafts: BTreeMap<crate::drafts::Key, crate::drafts::Draft>,
    draft_accounts: BTreeSet<i64>,
    draft_modified_accounts: BTreeSet<i64>,
    drafts_dirty: bool,
    retry_message_ids: BTreeMap<ChatId, i32>,
    retry_attachments: BTreeMap<(ChatId, i32), AttachmentRetry>,
    mode_before_help: Mode,
    mode_before_settings: Mode,
    mode_before_accounts: Mode,
    force_redraw: bool,
    pub keymap: crate::keymap::Keymap,
    pub configuration: configuration::State,
    pub help: help::State,
    next_pending_id: i32,
    next_history_request_id: u64,
    active_history_request: Option<(ChatId, u64)>,
    history_changes: BTreeMap<i32, Option<Message>>,
    history_read_max: i32,
    older_motion: Option<(ChatId, u64, usize)>,
    browsing_older: bool,
    changed_dialogs: BTreeSet<ChatId>,
    active_reply_request: Option<ReplyRequest>,
    history_target_message: Option<i32>,
    reads: reads::State,
    refresh_dialogs_pending: bool,
    downloading_attachments: BTreeMap<(ChatId, i32), u64>,
    next_download_request_id: u64,
    reveal_after_download: BTreeSet<(ChatId, i32)>,
    downloaded_attachments: BTreeMap<(ChatId, i32), PathBuf>,
    pending_telegram_link: Option<String>,
    /// Chats reached through links are kept until a dialog snapshot contains
    /// them, so a refresh cannot collapse the newly opened conversation.
    linked_chat_ids: BTreeSet<ChatId>,
    /// Rendered message rows from the last frame. `None` selects the body;
    /// `Some` identifies an activatable attachment/link/button action.
    message_hit_regions: Vec<MessageHitRegion>,
    /// Rendered chat rows: x start/end, y, filtered-list position.
    chat_hit_regions: Vec<(u16, u16, u16, usize)>,
    /// Rendered settings and account rows: x start/end, y, selection index.
    settings_hit_regions: Vec<(u16, u16, u16, usize)>,
    account_hit_regions: Vec<(u16, u16, u16, usize)>,
    /// Frame-local pane bounds used to route wheel events by pointer location.
    chat_pane_region: Option<(u16, u16, u16, u16)>,
    composer_region: Option<(u16, u16, u16, u16)>,
    conversation_pane_region: Option<(u16, u16, u16, u16)>,
}

pub type AppState = App;

impl Default for App {
    #[allow(clippy::too_many_lines)]
    fn default() -> Self {
        Self {
            screen: Screen::Connecting,
            mode: Mode::Navigate,
            focus: Focus::Chats,
            connection: ConnectionStatus::Connecting,
            metrics: crate::statusline::Metrics::default(),
            notifications: notifications::State::default(),
            alerts: alerts::State::default(),
            user_name: None,
            account_user_id: None,
            appearance: crate::appearance::Preferences::default(),
            navigation: composition::Navigation::default(),
            color_picker: None,
            chats: Vec::new(),
            folders: vec![crate::folders::Folder::all()],
            folder_id: 0,
            pins: pins::State::default(),
            message_pins: message_pins::State::default(),
            polls: polls::State::default(),
            reactions: reactions::State::default(),
            stickers: stickers::State::default(),
            replies: replies::State::default(),
            selected_chat: 0,
            active_chat_id: None,
            messages: BTreeMap::new(),
            selected_message: None,
            selected_action: 0,
            text_visibility: BTreeMap::new(),
            filter: TextInput::new(),
            search: search::State::default(),
            commands: commands::State::default(),
            completion: None,
            chat_completion: BTreeMap::new(),
            completion_requested: BTreeSet::new(),
            completion_hit_regions: Vec::new(),
            next_completion_request_id: 1,
            attachment_draft: staging::State::default(),
            editing: editing::State::default(),
            deletion: deletion::State::default(),
            invites: invites::State::default(),
            chat_info: chat_info::State::default(),
            sharing: sharing::State::default(),
            forwarding: forwarding::State::default(),
            narrow_conversation: false,
            sidebar_hidden: false,
            should_quit: false,
            status_message: None,
            media_previews: BTreeMap::new(),
            media_slots: Vec::new(),
            next_preview_request_id: 1,
            available_update: None,
            tick: 0,
            loading_history: false,
            message_scroll: 0,
            new_messages_while_scrolled: 0,
            new_messages_to_anchor: 0,
            viewport_anchor_message: None,
            viewport_anchor_row: 0,
            viewport_width: 0,
            terminal_focused: true,
            terminal_background: None,
            settings: Settings::default(),
            settings_path: None,
            settings_selection: 0,
            account_selection: 0,
            auth_input: TextInput::new(),
            auth_progress: None,
            qr_render_mode: QrRenderMode::default(),
            auth_restart_pending: false,
            drafts: BTreeMap::new(),
            draft_accounts: BTreeSet::new(),
            draft_modified_accounts: BTreeSet::new(),
            drafts_dirty: false,
            retry_message_ids: BTreeMap::new(),
            retry_attachments: BTreeMap::new(),
            mode_before_help: Mode::Navigate,
            mode_before_settings: Mode::Navigate,
            mode_before_accounts: Mode::Navigate,
            force_redraw: false,
            keymap: crate::keymap::Keymap::default(),
            configuration: configuration::State::default(),
            help: help::State::default(),
            next_pending_id: -1,
            next_history_request_id: 1,
            active_history_request: None,
            history_changes: BTreeMap::new(),
            history_read_max: 0,
            older_motion: None,
            browsing_older: false,
            changed_dialogs: BTreeSet::new(),
            active_reply_request: None,
            history_target_message: None,
            reads: reads::State::default(),
            refresh_dialogs_pending: false,
            downloading_attachments: BTreeMap::new(),
            next_download_request_id: 1,
            reveal_after_download: BTreeSet::new(),
            downloaded_attachments: BTreeMap::new(),
            pending_telegram_link: None,
            linked_chat_ids: BTreeSet::new(),
            message_hit_regions: Vec::new(),
            chat_hit_regions: Vec::new(),
            settings_hit_regions: Vec::new(),
            account_hit_regions: Vec::new(),
            chat_pane_region: None,
            composer_region: None,
            conversation_pane_region: None,
        }
    }
}

impl App {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct state with preferences loaded by the runtime and a path used
    /// for subsequent atomic saves. Keeping disk access outside `new` makes
    /// reducer tests and embedders deterministic.
    #[must_use]
    pub fn with_settings(settings: Settings, settings_path: PathBuf) -> Self {
        Self {
            settings,
            settings_path: Some(settings_path),
            ..Self::default()
        }
    }

    /// Construct state with in-memory preferences when persistence is not
    /// available. The settings overlay remains usable for the current run.
    #[must_use]
    pub fn with_ephemeral_settings(settings: Settings) -> Self {
        Self {
            settings,
            ..Self::default()
        }
    }

    #[must_use]
    pub const fn settings(&self) -> &Settings {
        &self.settings
    }

    #[must_use]
    pub const fn settings_selection(&self) -> usize {
        self.settings_selection
    }

    #[must_use]
    pub const fn account_selection(&self) -> usize {
        self.account_selection
    }

    #[must_use]
    pub const fn active_account(&self) -> u8 {
        self.settings.active_account
    }

    #[must_use]
    pub const fn account_count(&self) -> u8 {
        self.settings.account_count
    }

    /// Record a background update result without displacing active errors or
    /// messaging feedback. The footer shows this when no transient status is
    /// present.
    pub fn set_available_update(&mut self, version: &str) {
        let version = sanitize_terminal_line(version.trim());
        self.available_update =
            (self.settings.automatic_update_checks && !version.is_empty()).then_some(version);
    }

    pub fn clear_available_update(&mut self) {
        self.available_update = None;
    }

    #[must_use]
    pub fn available_update(&self) -> Option<&str> {
        self.available_update.as_deref()
    }

    #[must_use]
    pub fn sync_target(&self) -> Option<ChatId> {
        (self.terminal_focused && self.screen == Screen::Main && self.focus == Focus::Conversation)
            .then_some(self.active_chat_id)
            .flatten()
    }

    pub fn update(&mut self, event: AppEvent) -> Vec<TelegramCommand> {
        match event {
            AppEvent::Key(key) => self.handle_key(&key),
            AppEvent::Mouse(mouse) => self.handle_mouse(mouse),
            AppEvent::Network(event) => self.handle_network(event),
            AppEvent::Paste(text) => self.handle_paste(&text),
            AppEvent::TerminalFocus(focused) => {
                self.terminal_focused = focused;
                if focused && self.focus == Focus::Conversation && self.message_scroll == 0 {
                    self.reach_bottom()
                } else {
                    Vec::new()
                }
            }
            AppEvent::Tick => {
                if self
                    .metrics
                    .latency
                    .is_some_and(crate::statusline::Latency::expired)
                {
                    self.metrics.latency = None;
                }
                if self.keymap.expire()
                    && self
                        .status_message
                        .as_deref()
                        .is_some_and(|status| status.starts_with("Keys: "))
                {
                    self.status_message = None;
                }
                self.tick = self.tick.wrapping_add(1);
                Vec::new()
            }
        }
    }

    /// Frame-local pointer targets must never survive a frame that cannot
    /// render them (for example a terminal-too-small warning).
    pub fn clear_message_hit_regions(&mut self) {
        self.media_slots.clear();
        self.reads.visible = None;
        self.reads.visible_mentions.clear();
        self.message_hit_regions.clear();
        self.chat_hit_regions.clear();
        self.settings_hit_regions.clear();
        self.account_hit_regions.clear();
        self.commands.hit_regions.clear();
        self.completion_hit_regions.clear();
        self.invites.hit_regions.clear();
        self.attachment_draft.hit_regions.clear();
        self.stickers.hit_rows.clear();
        self.stickers.hit_cells.clear();
        self.chat_pane_region = None;
        self.composer_region = None;
        self.conversation_pane_region = None;
    }

    #[cfg(test)]
    pub(crate) fn message_hit_region_count(&self) -> usize {
        self.message_hit_regions.len()
    }

    #[cfg(test)]
    pub(crate) fn chat_hit_region_count(&self) -> usize {
        self.chat_hit_regions.len()
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> Vec<TelegramCommand> {
        use crate::keymap::{Context, Resolution};
        let context = if matches!(self.screen, Screen::Auth(_)) {
            Context::Input
        } else {
            match self.mode {
                Mode::Compose => Context::Compose,
                Mode::Edit => Context::Edit,
                Mode::ForwardPrompt => Context::Forward,
                Mode::Poll => Context::Poll,
                Mode::Reactions => Context::Reactions,
                Mode::Stickers => Context::Stickers,
                Mode::Command => Context::Command,
                Mode::Help if self.help.editing => Context::Input,
                Mode::Help => Context::Help,
                Mode::Attachments => Context::Attachments,
                Mode::Preview => Context::Preview,
                Mode::Filter => Context::Input,
                Mode::Search => Context::Search,
                Mode::PinnedMessages => Context::Pins,
                Mode::ChatInfo
                | Mode::Invite
                | Mode::DeletePrompt
                | Mode::PinPrompt
                | Mode::Settings
                | Mode::Accounts
                | Mode::Colors
                | Mode::Status => Context::Overlay,
                Mode::Navigate if self.focus == Focus::Chats => Context::Chats,
                Mode::Navigate => Context::Conversation,
            }
        };
        match self.keymap.feed(context, key) {
            Resolution::Action { run, count } => {
                if ((self.mode == Mode::Invite && run == Action::Open)
                    || (self.mode == Mode::Reactions
                        && matches!(run, Action::Open | Action::Send | Action::ClearReactions))
                    || (self.mode == Mode::Stickers && matches!(run, Action::Open | Action::Send))
                    || matches!(
                        run,
                        Action::Reveal | Action::ToggleSidebar | Action::PasteClipboard
                    )
                    || (matches!(self.mode, Mode::ForwardPrompt | Mode::Poll)
                        && run == Action::Send))
                    && key.kind == yazi_term::event::KeyEventKind::Repeat
                {
                    return Vec::new();
                }
                if self
                    .status_message
                    .as_deref()
                    .is_some_and(|status| status.starts_with("Keys: "))
                {
                    self.status_message = None;
                }
                return self.run_action(&run, count);
            }
            Resolution::Pending(hint) => {
                self.status_message = (!hint.is_empty()).then(|| format!("Keys: {hint}"));
                return Vec::new();
            }
            Resolution::Unbound => {}
        }
        if (matches!(
            context,
            Context::Compose | Context::Edit | Context::Input | Context::Command
        ) || (context == Context::Search && self.search.editing))
            && key.kind != yazi_term::event::KeyEventKind::Release
            && let Some(text) = key.text(&mut [0; 4])
        {
            return text
                .chars()
                .filter(|c| !c.is_control())
                .flat_map(|c| self.handle_action(KeyAction::Character(c)))
                .collect();
        }
        Vec::new()
    }

    #[cfg(test)]
    fn run_binding(&mut self, run: &str, count: usize) -> Vec<TelegramCommand> {
        self.run_action(&Action::parse(run).expect("known test action"), count)
    }

    #[allow(clippy::too_many_lines)]
    fn run_action(&mut self, run: &Action, count: usize) -> Vec<TelegramCommand> {
        if let Some(commands) = self.help_binding(run, count) {
            return commands;
        }
        if let Some(commands) = self.chat_info_binding(run, count) {
            return commands;
        }
        if let Some(commands) = self.invite_binding(run) {
            return commands;
        }
        if let Some(commands) = self.reaction_binding(run, count) {
            return commands;
        }
        if let Some(commands) = self.sticker_binding(run, count) {
            return commands;
        }
        if let Some(commands) = self.poll_binding(run, count) {
            return commands;
        }
        if let Some(commands) = self.forwarding_binding(run) {
            return commands;
        }
        if let Some(commands) = self.deletion_binding(run, count) {
            return commands;
        }
        if let Some(commands) = self.editing_binding(run) {
            return commands;
        }
        if let Some(commands) = self.attachment_binding(run) {
            return commands;
        }
        if let Some(commands) = self.command_binding(run) {
            return commands;
        }
        self.older_motion = None;
        if let Some(commands) = self.pin_binding(run.name(), count) {
            return commands;
        }
        if self.mode == Mode::Search
            && let Some(commands) = self.search_binding(run.name(), count)
        {
            return commands;
        }
        if let Action::Jump(alias) = run {
            if let Some(id) = self.keymap.chats.get(alias).copied() {
                if self.chats.iter().any(|chat| chat.id == id) {
                    return self.open_chat_by_id(id);
                }
                self.status_message = Some(format!(
                    "Chat alias {alias} is not in cached conversations yet"
                ));
            }
            return Vec::new();
        }
        match run {
            Action::CommandLine => return self.begin_command(),
            Action::ReloadConfig => {
                self.request_config_reload();
                return Vec::new();
            }
            Action::Attachments => return self.open_attachments(),
            Action::Mentions => return self.focused_mentions(),
            Action::FirstUnread => return self.first_unread(),
            Action::MarkRead => return self.mark_focused_chat(false),
            Action::MarkUnread => return self.mark_focused_chat(true),
            Action::MuteChat => return self.mute_focused_chat(crate::notifications::Mute::Forever),
            Action::UnmuteChat => return self.mute_focused_chat(crate::notifications::Mute::Off),
            Action::CopyText => return self.copy_message(false),
            Action::CopyLink => return self.copy_message(true),
            Action::PasteClipboard => return self.paste_clipboard(),
            Action::Attach => {
                self.begin_command();
                if self.mode == Mode::Command {
                    self.commands.input.set_value("attach ");
                }
                return Vec::new();
            }
            Action::ToggleSidebar => {
                if self.screen != Screen::Main
                    || !matches!(self.mode, Mode::Navigate | Mode::Compose)
                {
                    return Vec::new();
                }
                if self.active_chat_id.is_none() {
                    self.status_message = Some("Open a chat before hiding the sidebar".to_owned());
                    return Vec::new();
                }
                let narrow = self
                    .conversation_pane_region
                    .is_some_and(|(left, right, _, _)| {
                        right.saturating_sub(left) < crate::sidebar::MIN_SPLIT_WIDTH
                    });
                self.sidebar_hidden = self.chat_pane_region.is_some();
                if self.sidebar_hidden {
                    self.focus = Focus::Conversation;
                    self.narrow_conversation = true;
                } else {
                    if narrow {
                        self.mode = Mode::Navigate;
                    }
                    if self.mode == Mode::Navigate {
                        self.focus = Focus::Chats;
                        self.narrow_conversation = false;
                    }
                }
                self.clear_message_hit_regions();
                self.force_redraw = true;
                return Vec::new();
            }
            Action::Archive => return self.toggle_archive(),
            Action::Pin if self.focus == Focus::Conversation => {
                return self.begin_message_pin(false);
            }
            Action::Pin | Action::PinUp | Action::PinDown => {
                return self.change_chat_pin(run.name());
            }
            Action::Pins => return self.open_pinned_messages(),
            Action::Search => return self.open_search(),
            Action::ChatColor => return self.begin_color_picker(false),
            Action::FolderColor => return self.begin_color_picker(true),
            Action::FolderNext | Action::FolderPrevious => {
                let current = self
                    .folders
                    .iter()
                    .position(|folder| folder.id == self.folder_id)
                    .unwrap_or(0);
                let total = self.folders.len();
                if total > 0 {
                    let index = if *run == Action::FolderNext {
                        (current + 1) % total
                    } else {
                        (current + total - 1) % total
                    };
                    self.folder_id = self.folders[index].id;
                    self.selected_chat = 0;
                    self.focus = Focus::Chats;
                    self.narrow_conversation = false;
                    self.mode = Mode::Navigate;
                }
                return Vec::new();
            }
            Action::ChatInfo => {
                let chat = if self.focus == Focus::Chats {
                    self.selected_chat_entry()
                } else {
                    self.chats
                        .iter()
                        .find(|chat| Some(chat.id) == self.active_chat_id)
                };
                self.status_message = chat.map(|chat| {
                    format!(
                        "{} · chat ID {} · folder ID {}",
                        chat.title, chat.id, self.folder_id
                    )
                });
                return Vec::new();
            }
            Action::Help | Action::Settings | Action::Accounts if self.screen == Screen::Main => {
                if (*run == Action::Help && self.mode == Mode::Help)
                    || (*run == Action::Settings && self.mode == Mode::Settings)
                    || (*run == Action::Accounts && self.mode == Mode::Accounts)
                {
                    return self.handle_main(KeyAction::Escape);
                }
                let character = match run {
                    Action::Help => '?',
                    Action::Settings => 's',
                    _ => 'a',
                };
                return self.handle_navigation(KeyAction::Character(character));
            }
            Action::MessageUp => return self.move_messages_up(count),
            Action::MessageDown => {
                let messages = self.active_messages();
                if messages.is_empty() {
                    return Vec::new();
                }
                let index = self
                    .selected_message
                    .and_then(|id| messages.iter().position(|message| message.id == id))
                    .unwrap_or(messages.len() - 1);
                let target = index.saturating_add(count).min(messages.len() - 1);
                let id = messages[target].id;
                self.select_message_id(id, true);
                if target == self.active_messages().len().saturating_sub(1) {
                    return self.next_unread_page();
                }
                return Vec::new();
            }
            Action::Up if self.mode == Mode::Navigate => return self.move_up(count),
            Action::Down if self.mode == Mode::Navigate => return self.move_down(count),
            Action::PageUp => return self.move_up(PAGE_STEP.saturating_mul(count)),
            Action::PageDown => return self.move_down(PAGE_STEP.saturating_mul(count)),
            Action::Latest
                if (self.browsing_older || self.reads.entry.is_some())
                    && self.focus == Focus::Conversation =>
            {
                return self.latest_history();
            }
            Action::Latest => return self.move_to_end(),
            Action::Oldest => return self.move_to_start(),
            Action::Refresh => {
                let mut commands = self.request_dialog_refresh();
                commands.push(TelegramCommand::RefreshFolders);
                return commands;
            }
            Action::Filter => {
                self.focus = Focus::Chats;
                self.mode = Mode::Filter;
                return Vec::new();
            }
            Action::Compose => return self.compose_or_reply(),
            Action::Preview => return self.preview_selected_media(),
            Action::CompleteNext | Action::CompletePrevious if self.mode == Mode::Compose => {
                self.move_completion(*run == Action::CompleteNext);
                return Vec::new();
            }
            Action::Send => {
                // While the completion popup is up, Enter accepts the
                // highlighted row instead of sending — like official clients.
                if self.mode == Mode::Compose && self.accept_completion() {
                    return Vec::new();
                }
                self.completion = None;
                return self
                    .active_chat_id
                    .map_or_else(Vec::new, |id| self.send_draft(id));
            }
            Action::Reply => return self.start_replying_to_selected(),
            Action::ReplyTarget => return self.navigate_to_selected_reply(),
            Action::OpenLink => return self.activate_selected_link(),
            Action::NextAction => return self.select_actionable_message(true),
            Action::PreviousAction => return self.select_actionable_message(false),
            Action::Reveal => return self.reveal_selected_attachment(),
            Action::Spoilers | Action::ExpandQuote => {
                self.toggle_text_details(*run == Action::Spoilers);
                return Vec::new();
            }
            Action::Noop => return Vec::new(),
            _ => {}
        }
        let action = match run {
            Action::Quit => KeyAction::Quit,
            Action::Help => KeyAction::Character('?'),
            Action::Settings => KeyAction::Character('s'),
            Action::Accounts => KeyAction::Character('a'),
            Action::NextAccount => KeyAction::NextAccount,
            Action::AddAccount => KeyAction::AddAccount,
            Action::Open => KeyAction::Enter,
            Action::Cancel => KeyAction::Escape,
            Action::Focus => KeyAction::Tab,
            Action::Newline => KeyAction::Newline,
            Action::Redraw => KeyAction::Redraw,
            Action::Up => KeyAction::Up,
            Action::Down => KeyAction::Down,
            Action::Home => KeyAction::Home,
            Action::End => KeyAction::End,
            Action::Left => KeyAction::Left,
            Action::Right => KeyAction::Right,
            Action::Backspace => KeyAction::Backspace,
            Action::Delete => KeyAction::Delete,
            Action::Clear => KeyAction::Clear,
            Action::DeleteWord => KeyAction::DeleteWord,
            _ => return Vec::new(),
        };
        self.handle_action(action)
    }

    fn move_messages_up(&mut self, count: usize) -> Vec<TelegramCommand> {
        let messages = self.active_messages();
        if messages.is_empty() {
            return Vec::new();
        }
        let index = self
            .selected_message
            .and_then(|id| messages.iter().position(|message| message.id == id))
            .unwrap_or(messages.len() - 1);
        let target = index.saturating_sub(count);
        let id = messages[target].id;
        self.select_message_id(id, true);
        if count <= index {
            return Vec::new();
        }
        let Some(chat_id) = self.active_chat_id else {
            return Vec::new();
        };
        let request_id = self.next_history_request_id;
        self.next_history_request_id = request_id.wrapping_add(1).max(1);
        self.active_history_request = None;
        self.history_changes.clear();
        self.older_motion = Some((chat_id, request_id, count - index));
        self.status_message = Some("Loading earlier messages…".to_owned());
        vec![TelegramCommand::LoadOlder {
            chat_id,
            request_id,
            before_id: id,
        }]
    }

    fn finish_older(
        &mut self,
        chat_id: ChatId,
        request_id: u64,
        before_id: i32,
        mut messages: Vec<Message>,
    ) -> Vec<TelegramCommand> {
        let Some((chat, request, remaining)) = self.older_motion else {
            return Vec::new();
        };
        if chat != chat_id || request != request_id || self.active_chat_id != Some(chat_id) {
            return Vec::new();
        }
        self.older_motion = None;
        messages.retain(|message| {
            message.id < before_id
                && !self
                    .history_changes
                    .get(&message.id)
                    .is_some_and(Option::is_none)
        });
        for message in &mut messages {
            if let Some(Some(current)) = self.history_changes.get(&message.id) {
                *message = current.clone();
            }
            sanitize_message(message);
        }
        if messages.is_empty() {
            self.status_message = Some("Start of available history".to_owned());
            return Vec::new();
        }
        let current = self.messages.remove(&chat_id).unwrap_or_default();
        let pending = current
            .iter()
            .filter(|message| message.id < 0)
            .cloned()
            .collect::<Vec<_>>();
        let mut combined = messages;
        combined.extend(current.into_iter().filter(|message| message.id > 0));
        combined.sort_by_key(|message| (message.timestamp, message.id));
        combined.dedup_by_key(|message| message.id);
        combined.truncate(MAX_MESSAGES_PER_CHAT.saturating_sub(pending.len()));
        combined.extend(pending);
        if self.reads.entry.is_some() || self.reads.forward.is_some() {
            self.reads.forward = combined
                .iter()
                .filter(|message| message.id > 0)
                .map(|message| message.id)
                .max()
                .map(|id| (chat_id, id));
        }
        self.messages.insert(chat_id, combined);
        self.browsing_older = true;
        self.loading_history = false;
        self.selected_message = Some(before_id);
        self.move_messages_up(remaining)
    }

    #[allow(clippy::too_many_lines)]
    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> Vec<TelegramCommand> {
        if matches!(self.mode, Mode::Status | Mode::Help) {
            let scroll = if self.mode == Mode::Status {
                &mut self.configuration.status_scroll
            } else {
                &mut self.help.scroll
            };
            match mouse.kind {
                MouseEventKind::ScrollUp => *scroll = scroll.saturating_sub(3),
                MouseEventKind::ScrollDown => *scroll = scroll.saturating_add(3),
                _ => {}
            }
            return Vec::new();
        }
        if self.mode == Mode::ChatInfo {
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    self.chat_info_binding(&Action::Up, 3);
                }
                MouseEventKind::ScrollDown => {
                    self.chat_info_binding(&Action::Down, 3);
                }
                _ => {}
            }
            return Vec::new();
        }
        if self.mode == Mode::Invite {
            if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && let Some(index) =
                    pointer_row_hit(&self.invites.hit_regions, mouse.column, mouse.row)
            {
                self.invites.selected = index;
            }
            return Vec::new();
        }
        if matches!(
            mouse.kind,
            MouseEventKind::Down(_) | MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) {
            self.older_motion = None;
        }
        if !matches!(self.screen, Screen::Main) {
            return Vec::new();
        }
        if self.mode == Mode::Reactions {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(index) =
                        pointer_row_hit(&self.reactions.hit_rows, mouse.column, mouse.row)
                        && let Some(panel) = &mut self.reactions.panel
                    {
                        panel.selected = index;
                    }
                }
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    let action = if mouse.kind == MouseEventKind::ScrollUp {
                        Action::Up
                    } else {
                        Action::Down
                    };
                    return self.reaction_binding(&action, 3).unwrap_or_default();
                }
                _ => {}
            }
            return Vec::new();
        }
        if self.mode == Mode::Stickers {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(index) =
                        pointer_row_hit(&self.stickers.hit_rows, mouse.column, mouse.row)
                    {
                        return self.select_sticker_section(index);
                    }
                    if let Some(index) = self
                        .stickers
                        .hit_cells
                        .iter()
                        .rev()
                        .find(|&&(left, right, top, bottom, _)| {
                            (left..right).contains(&mouse.column)
                                && (top..bottom).contains(&mouse.row)
                        })
                        .map(|&(_, _, _, _, index)| index)
                    {
                        return self.click_sticker_cell(index);
                    }
                }
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    let action = if mouse.kind == MouseEventKind::ScrollUp {
                        Action::Up
                    } else {
                        Action::Down
                    };
                    return self.sticker_binding(&action, 3).unwrap_or_default();
                }
                _ => {}
            }
            return Vec::new();
        }
        if self.mode == Mode::Poll {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(index) =
                        pointer_row_hit(&self.polls.hit_rows, mouse.column, mouse.row)
                    {
                        if let Some(review) = &mut self.polls.review {
                            review.selected = index;
                        }
                        self.toggle_poll_answer();
                    }
                }
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    if let Some(review) = &mut self.polls.review {
                        review.follow = false;
                        if mouse.kind == MouseEventKind::ScrollUp {
                            review.top = review.top.saturating_sub(3);
                        } else {
                            review.top = review.top.saturating_add(3);
                        }
                    }
                }
                _ => {}
            }
            return Vec::new();
        }
        if self.mode == Mode::Attachments {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(index) =
                        pointer_row_hit(&self.attachment_draft.hit_regions, mouse.column, mouse.row)
                    {
                        self.attachment_draft.selected = index;
                    }
                }
                MouseEventKind::ScrollUp => {
                    self.attachment_binding(&Action::Up);
                }
                MouseEventKind::ScrollDown => {
                    self.attachment_binding(&Action::Down);
                }
                _ => {}
            }
            return Vec::new();
        }
        if self.mode == Mode::Command {
            if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && let Some(index) =
                    pointer_row_hit(&self.commands.hit_regions, mouse.column, mouse.row)
            {
                self.click_command(index);
            }
            return Vec::new();
        }
        if self.mode == Mode::Compose
            && mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && let Some(index) =
                pointer_row_hit(&self.completion_hit_regions, mouse.column, mouse.row)
        {
            self.click_completion(index);
            return Vec::new();
        }
        if matches!(
            self.mode,
            Mode::Help
                | Mode::Edit
                | Mode::DeletePrompt
                | Mode::ForwardPrompt
                | Mode::Status
                | Mode::Search
                | Mode::Colors
                | Mode::PinnedMessages
                | Mode::PinPrompt
                | Mode::Preview
        ) {
            return Vec::new();
        }
        if self.mode == Mode::Settings {
            return match mouse.kind {
                MouseEventKind::ScrollUp => self.handle_settings(KeyAction::Up),
                MouseEventKind::ScrollDown => self.handle_settings(KeyAction::Down),
                MouseEventKind::Down(MouseButton::Left) => {
                    let Some(selection) =
                        pointer_row_hit(&self.settings_hit_regions, mouse.column, mouse.row)
                    else {
                        return Vec::new();
                    };
                    self.settings_selection = selection;
                    self.toggle_selected_setting();
                    Vec::new()
                }
                _ => Vec::new(),
            };
        }
        if self.mode == Mode::Accounts {
            return match mouse.kind {
                MouseEventKind::ScrollUp => self.handle_accounts(KeyAction::Up),
                MouseEventKind::ScrollDown => self.handle_accounts(KeyAction::Down),
                MouseEventKind::Down(MouseButton::Left) => {
                    let Some(selection) =
                        pointer_row_hit(&self.account_hit_regions, mouse.column, mouse.row)
                    else {
                        return Vec::new();
                    };
                    self.account_selection = selection;
                    self.handle_accounts(KeyAction::Enter)
                }
                _ => Vec::new(),
            };
        }
        match mouse.kind {
            MouseEventKind::ScrollUp
                if pointer_in_region(self.chat_pane_region, mouse.column, mouse.row) =>
            {
                if self.mode != Mode::Compose {
                    self.focus = Focus::Chats;
                }
                self.move_chat_up(3)
            }
            MouseEventKind::ScrollDown
                if pointer_in_region(self.chat_pane_region, mouse.column, mouse.row) =>
            {
                if self.mode != Mode::Compose {
                    self.focus = Focus::Chats;
                }
                self.move_chat_down(3)
            }
            MouseEventKind::ScrollUp
                if pointer_in_region(self.conversation_pane_region, mouse.column, mouse.row) =>
            {
                self.focus = Focus::Conversation;
                self.narrow_conversation = true;
                self.move_up(3)
            }
            MouseEventKind::ScrollDown
                if pointer_in_region(self.conversation_pane_region, mouse.column, mouse.row) =>
            {
                self.focus = Focus::Conversation;
                self.narrow_conversation = true;
                self.move_down(3)
            }
            MouseEventKind::Down(MouseButton::Right) => self.handle_reply_click(mouse),
            MouseEventKind::Down(MouseButton::Left) => {
                if pointer_in_region(self.composer_region, mouse.column, mouse.row) {
                    return self.start_composing();
                }
                if let Some(selection) =
                    pointer_row_hit(&self.chat_hit_regions, mouse.column, mouse.row)
                {
                    self.selected_chat = selection;
                    self.focus = Focus::Chats;
                    self.mode = Mode::Navigate;
                    self.narrow_conversation = false;
                    self.selected_message = None;
                    return self.open_selected_chat();
                }
                if let Some((message_id, action_index)) =
                    pointer_row_hit(&self.message_hit_regions, mouse.column, mouse.row)
                {
                    self.focus = Focus::Conversation;
                    self.narrow_conversation = true;
                    self.mode = Mode::Navigate;
                    self.select_message_id(message_id, false);
                    self.selected_action = action_index.unwrap_or(0);
                    if action_index.is_some()
                        && self
                            .active_messages()
                            .iter()
                            .find(|message| message.id == message_id)
                            .is_some_and(|message| {
                                matches!(
                                    self.message_actions(message).get(self.selected_action),
                                    Some(
                                        MessageAction::Reply
                                            | MessageAction::Spoilers
                                            | MessageAction::ExpandQuote
                                    )
                                )
                            })
                    {
                        return self.activate_selected_message();
                    }
                    return Vec::new();
                }
                if pointer_in_region(self.conversation_pane_region, mouse.column, mouse.row) {
                    self.focus = Focus::Conversation;
                    self.narrow_conversation = true;
                    self.mode = Mode::Navigate;
                    self.selected_message = None;
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn handle_reply_click(&mut self, mouse: MouseEvent) -> Vec<TelegramCommand> {
        let Some((message_id, _)) =
            pointer_row_hit(&self.message_hit_regions, mouse.column, mouse.row)
        else {
            return Vec::new();
        };
        self.focus = Focus::Conversation;
        self.narrow_conversation = true;
        self.select_message_id(message_id, false);
        self.start_replying_to_selected()
    }

    pub fn handle_action(&mut self, action: KeyAction) -> Vec<TelegramCommand> {
        if action == KeyAction::Quit
            || (action == KeyAction::Character('q')
                && matches!(self.screen, Screen::Connecting | Screen::Fatal(_)))
        {
            return self.quit();
        }
        match action {
            KeyAction::NextAccount => return self.switch_to_next_account(),
            KeyAction::AddAccount => return self.add_account(),
            _ => {}
        }
        match self.screen {
            Screen::Connecting | Screen::Fatal(_) => Vec::new(),
            Screen::Auth(_) => self.handle_auth(action),
            Screen::Main => self.handle_main(action),
        }
    }

    #[allow(clippy::too_many_lines)]
    pub fn handle_network(&mut self, event: NetworkEvent) -> Vec<TelegramCommand> {
        self.observe_chat_info(&event);
        if let Some(commands) = self.observe_reactions(&event) {
            return commands;
        }
        if let Some(commands) = self.observe_polls(&event) {
            return commands;
        }
        if let Some(commands) = self.observe_stickers(&event) {
            return commands;
        }
        self.observe_alerts(&event);
        self.observe_search(&event);
        self.observe_deletion(&event);
        self.observe_forward(&event);
        self.update_reply_previews(&event);
        self.track_snapshot_changes(&event);
        if let NetworkEvent::MessageUpdated(message) = &event {
            self.update_pin_preview(message);
        }
        match event {
            event @ (NetworkEvent::InviteReady { .. } | NetworkEvent::InviteJoined { .. }) => {
                self.observe_invite(event)
            }
            NetworkEvent::SavedReady { request_id, result } => self.saved_ready(request_id, result),
            NetworkEvent::ForwardReady { request_id, result } => {
                self.forward_ready(request_id, result);
                Vec::new()
            }
            NetworkEvent::ForwardFinished { request_id, error } => {
                self.forward_finished(request_id, error);
                Vec::new()
            }
            NetworkEvent::MessageCopyReady { request_id, result } => {
                self.message_copy_ready(request_id, result)
            }
            NetworkEvent::DeletionReady {
                chat_id,
                message_id,
                request_id,
                result,
            } => {
                self.deletion_ready(chat_id, message_id, request_id, result);
                Vec::new()
            }
            NetworkEvent::DeleteFinished {
                chat_id,
                message_id,
                request_id,
                error,
            } => {
                self.deletion_finished(chat_id, message_id, request_id, error);
                Vec::new()
            }
            NetworkEvent::EditLoaded {
                chat_id,
                request_id,
                result,
            } => {
                self.edit_loaded(chat_id, request_id, result);
                Vec::new()
            }
            NetworkEvent::EditFinished {
                chat_id,
                request_id,
                error,
            } => {
                self.edit_finished(chat_id, request_id, error);
                Vec::new()
            }
            NetworkEvent::Telemetry { dc_id, latency } => {
                self.metrics.dc_id = dc_id;
                self.metrics.latency = latency.filter(|sample| {
                    self.connection == ConnectionStatus::Online
                        && self.keymap.statusline.measures_latency()
                        && sample.started >= self.metrics.reset_at
                        && !sample.expired()
                });
                Vec::new()
            }
            NetworkEvent::PinnedMessages {
                chat_id,
                request_id,
                page,
            } => self.finish_pinned_messages(chat_id, request_id, page),
            NetworkEvent::PinnedMessagesFailed {
                chat_id,
                request_id,
                error,
            } => {
                self.fail_pinned_messages(chat_id, request_id, &error);
                Vec::new()
            }
            NetworkEvent::PinnedContext {
                chat_id,
                message_id,
                request_id,
                messages,
            } => {
                self.finish_pinned_context(chat_id, message_id, request_id, messages);
                Vec::new()
            }
            NetworkEvent::MessagePinsChanged {
                chat_id,
                message_ids,
                pinned,
            } => self.pin_messages_changed(chat_id, Some(&message_ids), pinned),
            NetworkEvent::MessagePinsCleared { chat_id } => {
                self.pin_messages_changed(chat_id, None, false)
            }
            NetworkEvent::MessagePinFinished {
                chat_id,
                request_id,
                error,
            } => self.finish_message_pin(chat_id, request_id, error),
            NetworkEvent::ArchiveChanged { chat_id, archived } => {
                let selected_id = self.selected_chat_entry().map(|chat| chat.id);
                if let Some(chat) = self.chats.iter_mut().find(|chat| chat.id == chat_id) {
                    chat.membership.archived = archived;
                }
                self.preserve_chat_selection(selected_id);
                Vec::new()
            }
            NetworkEvent::DialogPins(pins) => {
                let selected_id = self.selected_chat_entry().map(|chat| chat.id);
                self.pins.dialogs = pins;
                self.preserve_chat_selection(selected_id);
                Vec::new()
            }
            NetworkEvent::DialogPinFinished { request_id, error } => {
                self.finish_dialog_pin(request_id, error);
                Vec::new()
            }
            NetworkEvent::AccountIdentity { user_id } => {
                self.account_user_id = Some(user_id);
                Vec::new()
            }
            NetworkEvent::LocalDrafts { user_id, drafts } => {
                self.restore_drafts(user_id, drafts);
                Vec::new()
            }
            NetworkEvent::AttachmentsPrepared {
                key,
                request_id,
                result,
            } => {
                self.attachments_prepared(key, request_id, result);
                Vec::new()
            }
            NetworkEvent::SearchResults { request_id, page } => {
                self.finish_search(request_id, Ok(search::Results::Local(page)));
                Vec::new()
            }
            NetworkEvent::CloudSearchResults {
                request_id, page, ..
            } => {
                self.finish_search(request_id, Ok(search::Results::Cloud(page)));
                Vec::new()
            }
            NetworkEvent::ChatMuteChanged { chat_id, until } => {
                self.apply_chat_mute(chat_id, until);
                Vec::new()
            }
            NetworkEvent::ChatMuteFinished {
                chat_id,
                request_id,
                result,
            } => {
                self.finish_chat_mute(chat_id, request_id, result);
                Vec::new()
            }
            NetworkEvent::MembersLoaded {
                chat_id,
                request_id: _,
                result,
            } => {
                // A failed fetch keeps `completion_requested` set so typing
                // cannot loop requests; transcript senders still complete.
                if let Ok(data) = result {
                    self.chat_completion.insert(chat_id, data);
                }
                self.refresh_completion(chat_id)
            }
            NetworkEvent::SearchFailed { request_id, error } => {
                self.finish_search(request_id, Err(error));
                Vec::new()
            }
            NetworkEvent::CloudSearchContext {
                request_id,
                chat_id,
                message_id,
                messages,
            }
            | NetworkEvent::CachedContext {
                request_id,
                chat_id,
                message_id,
                messages,
            } => {
                self.open_search_context(request_id, chat_id, message_id, messages);
                Vec::new()
            }
            NetworkEvent::OlderHistory {
                chat_id,
                request_id,
                before_id,
                messages,
            } => self.finish_older(chat_id, request_id, before_id, messages),
            NetworkEvent::OlderHistoryFailed {
                chat_id,
                request_id,
                error,
            } => {
                if self
                    .older_motion
                    .is_some_and(|(chat, request, _)| chat == chat_id && request == request_id)
                {
                    self.older_motion = None;
                    self.status_message = Some(error);
                }
                Vec::new()
            }
            NetworkEvent::Folders(mut folders) => {
                let selected_id = self.selected_chat_entry().map(|chat| chat.id);
                if !folders.iter().any(|folder| folder.id == 1) {
                    folders.push(crate::folders::Folder::archive());
                }
                self.folders = folders;
                if !self
                    .folders
                    .iter()
                    .any(|folder| folder.id == self.folder_id)
                {
                    self.folder_id = 0;
                }
                self.preserve_chat_selection(selected_id);
                Vec::new()
            }
            NetworkEvent::UnreadChanged {
                chat_id,
                max_id,
                unread,
            } => {
                if let Some(chat) = self.chats.iter_mut().find(|chat| chat.id == chat_id) {
                    crate::read_state::inbox_update(chat, max_id, unread);
                }
                self.clamp_chat_selection();
                Vec::new()
            }
            NetworkEvent::ChatUnreadChanged { chat_id, unread } => {
                if let Some(chat) = self.chats.iter_mut().find(|chat| chat.id == chat_id) {
                    crate::read_state::unread_mark(chat, unread);
                }
                self.clamp_chat_selection();
                Vec::new()
            }
            NetworkEvent::ChatUnreadFinished {
                chat_id,
                unread,
                request_id,
                snapshot,
                error,
            } => {
                if error.is_none()
                    && let Some(chat) = self.chats.iter_mut().find(|chat| chat.id == chat_id)
                {
                    crate::read_state::unread_mark(chat, unread);
                    if let Some(snapshot) = &snapshot {
                        crate::read_state::acknowledge(chat, snapshot.max_id, Some(snapshot));
                    }
                }
                self.finish_chat_unread(chat_id, request_id, unread, error);
                Vec::new()
            }
            NetworkEvent::CachedSnapshot { user_name, chats } => {
                self.user_name = user_name;
                self.screen = Screen::Main;
                self.replace_dialogs(chats);
                self.status_message = Some("Cached conversations · connecting…".to_owned());
                Vec::new()
            }
            NetworkEvent::CachedHistory {
                chat_id,
                request_id,
                messages,
            } => {
                if self.active_history_request == Some((chat_id, request_id)) && !self.reads.paging
                {
                    self.merge_history(chat_id, messages, self.history_target_message);
                    self.position_unread_entry(chat_id, false);
                }
                Vec::new()
            }
            NetworkEvent::ChatInfoReady { .. }
            | NetworkEvent::ChatInfoInvalidated { .. }
            | NetworkEvent::ReactionsChanged(_)
            | NetworkEvent::ReactionsLoading { .. }
            | NetworkEvent::ReactionsLoaded { .. }
            | NetworkEvent::ReactionsFinished { .. }
            | NetworkEvent::StickersLoaded { .. }
            | NetworkEvent::StickerSetLoaded { .. }
            | NetworkEvent::StickerThumbDownloaded { .. }
            | NetworkEvent::StickerSendFailed { .. }
            | NetworkEvent::PollChanged(_)
            | NetworkEvent::PollLoading { .. }
            | NetworkEvent::PollLoaded { .. }
            | NetworkEvent::PollFinished { .. }
            | NetworkEvent::CloudSearchLoading { .. }
            | NetworkEvent::CloudSearchCancelled { .. }
            | NetworkEvent::HistoryLoading { .. }
            | NetworkEvent::ReplyPreviewsLoading { .. }
            | NetworkEvent::ReplyPreviews { .. }
            | NetworkEvent::ReplyPreviewsFailed { .. }
            | NetworkEvent::PinnedMessagesLoading { .. }
            | NetworkEvent::SyncCheckpoint(_)
            | NetworkEvent::AttachmentDownloadStarted { .. }
            | NetworkEvent::NotificationSettingsChanged
            | NetworkEvent::AlertSettingsReady { .. }
            | NetworkEvent::CacheMessage(_) => Vec::new(),
            NetworkEvent::CacheAccountReset { .. } => {
                self.reset_for_account_switch(self.active_account());
                Vec::new()
            }
            NetworkEvent::CacheInvalidated { chat_id } => {
                self.downloading_attachments
                    .retain(|(id, _), _| chat_id.is_some_and(|chat_id| *id != chat_id));
                self.reveal_after_download
                    .retain(|(id, _)| chat_id.is_some_and(|chat_id| *id != chat_id));
                self.downloaded_attachments
                    .retain(|(id, _), _| chat_id.is_some_and(|chat_id| *id != chat_id));
                if let Some(chat_id) = chat_id {
                    self.messages.remove(&chat_id);
                } else {
                    self.messages.clear();
                }
                let mut commands = self.invalidate_message_pins(chat_id);
                commands.extend(self.request_dialog_refresh());
                if let Some(active) = self.active_chat_id
                    && chat_id.is_none_or(|id| id == active)
                {
                    let request_id = self.next_history_request_id;
                    self.next_history_request_id = request_id.wrapping_add(1).max(1);
                    self.active_history_request = Some((active, request_id));
                    self.history_changes.clear();
                    self.history_read_max = 0;
                    self.loading_history = true;
                    commands.push(TelegramCommand::LoadHistory {
                        chat_id: active,
                        request_id,
                        after_id: None,
                    });
                }
                commands
            }
            NetworkEvent::Auth(prompt) => {
                if self.auth_restart_pending && !matches!(&prompt, AuthPrompt::Phone) {
                    return Vec::new();
                }
                if matches!(&prompt, AuthPrompt::Phone) {
                    self.auth_restart_pending = false;
                }
                let preserve_qr_error = matches!(&prompt, AuthPrompt::Phone)
                    && (matches!(
                        self.auth_progress,
                        Some(AuthProgress::StartQr | AuthProgress::WaitQr)
                    ) || matches!(self.screen, Screen::Auth(AuthPhase::Qr { .. })))
                    && self.status_message.is_some();
                self.auth_input.clear();
                self.auth_progress =
                    matches!(&prompt, AuthPrompt::Qr { .. }).then_some(AuthProgress::WaitQr);
                self.screen = Screen::Auth(prompt.into());
                if !preserve_qr_error {
                    self.status_message = None;
                }
                Vec::new()
            }
            NetworkEvent::Ready { user_name } => {
                self.auth_progress = None;
                self.auth_restart_pending = false;
                self.user_name = Some(sanitize_terminal_line(&user_name));
                self.screen = Screen::Main;
                self.connection = ConnectionStatus::Online;
                self.status_message = None;
                Vec::new()
            }
            NetworkEvent::DialogsLoading => {
                self.refresh_dialogs_pending = true;
                self.changed_dialogs.clear();
                Vec::new()
            }
            NetworkEvent::Dialogs(chats) => {
                self.refresh_dialogs_pending = false;
                self.replace_dialogs(chats);
                Vec::new()
            }
            NetworkEvent::DialogsFailed(error) => {
                self.refresh_dialogs_pending = false;
                self.status_message = Some(sanitize_terminal_line(&error));
                Vec::new()
            }
            NetworkEvent::History {
                chat_id,
                request_id,
                messages,
            } => self.finish_history(chat_id, request_id, Ok(messages)),
            NetworkEvent::HistoryFailed {
                chat_id,
                request_id,
                error,
            } => self.finish_history(chat_id, request_id, Err(error)),
            NetworkEvent::MessageLoaded {
                chat_id,
                message_id,
                request_id,
                message,
            } => self.finish_reply_navigation(chat_id, message_id, request_id, Ok(message)),
            NetworkEvent::MessageLoadFailed {
                chat_id,
                message_id,
                request_id,
                error,
            } => self.finish_reply_navigation(chat_id, message_id, request_id, Err(error)),
            NetworkEvent::NewMessage(message) => self.receive_message(message, true, true),
            NetworkEvent::MessageUpdated(message) => {
                self.update_message(message);
                Vec::new()
            }
            NetworkEvent::MessageSent { local_id, message } => {
                self.confirm_message(local_id, message)
            }
            NetworkEvent::MessageAccepted { chat_id, local_id } => {
                self.accept_message(chat_id, local_id);
                Vec::new()
            }
            NetworkEvent::MessageContentsRead {
                channel_id,
                message_ids,
            } => {
                self.contents_read(channel_id, &message_ids);
                Vec::new()
            }
            NetworkEvent::MentionsReadFinished {
                chat_id,
                message_ids,
                error,
            } => {
                self.mentions_read_finished(chat_id, &message_ids, error);
                Vec::new()
            }
            NetworkEvent::ReadMarked {
                chat_id,
                max_id,
                snapshot,
            } => {
                self.read_finished(chat_id, max_id, false);
                if let Some(chat) = self.chats.iter_mut().find(|chat| chat.id == chat_id) {
                    crate::read_state::acknowledge(chat, max_id, snapshot.as_ref());
                }
                Vec::new()
            }
            NetworkEvent::ReadMarkFailed {
                chat_id,
                max_id,
                error,
            } => {
                self.read_finished(chat_id, max_id, true);
                self.status_message = Some(sanitize_terminal_line(&error));
                Vec::new()
            }
            NetworkEvent::MessagesRead { chat_id, max_id } => {
                if let Some(messages) = self.messages.get_mut(&chat_id) {
                    for message in messages {
                        if message.outgoing && message.id > 0 && message.id <= max_id {
                            message.delivery = Delivery::Read;
                        }
                    }
                }
                Vec::new()
            }
            NetworkEvent::SendFailed {
                chat_id,
                local_id,
                text,
                reply_to,
                error,
            } => self.fail_message(chat_id, local_id, &text, reply_to, &error),
            NetworkEvent::AttachmentSendFailed {
                chat_id,
                local_id,
                attachment,
                caption,
                reply_to,
                error,
            } => {
                let reply_to = self
                    .messages
                    .get(&chat_id)
                    .and_then(|messages| messages.iter().find(|message| message.id == local_id))
                    .and_then(|message| message.reply_to.clone())
                    .or_else(|| reply_to.and_then(|id| self.reply_info_for_target(chat_id, id)));
                if let Some(message) = self
                    .messages
                    .get_mut(&chat_id)
                    .and_then(|messages| messages.iter_mut().find(|message| message.id == local_id))
                {
                    message.delivery = Delivery::Failed;
                    self.retry_attachments.insert(
                        (chat_id, local_id),
                        AttachmentRetry {
                            attachment,
                            caption,
                            reply_to,
                        },
                    );
                    if self.active_chat_id == Some(chat_id) {
                        self.selected_message = Some(local_id);
                        self.selected_action = 0;
                    }
                    self.status_message = Some(format!(
                        "Attachment not sent: {} · Enter retries",
                        sanitize_terminal_line(&error)
                    ));
                }
                Vec::new()
            }
            NetworkEvent::PreviewDownloaded {
                chat_id,
                message_id,
                request_id,
                path,
            } => {
                if let Some(preview) =
                    self.media_previews
                        .get_mut(&(chat_id, message_id))
                        .filter(|preview| {
                            preview.chat_id == chat_id
                                && preview.message_id == message_id
                                && preview.request_id == request_id
                        })
                {
                    if !preview.thumbnail {
                        self.downloaded_attachments
                            .insert((chat_id, message_id), path.clone());
                    }
                    preview.path = Some(path);
                    preview.loading = false;
                    preview.status = if preview.thumbnail {
                        "Animated sticker · static preview".to_owned()
                    } else {
                        String::new()
                    };
                }
                Vec::new()
            }
            NetworkEvent::PreviewDownloadFailed {
                chat_id,
                message_id,
                request_id,
                error,
            } => {
                if self
                    .media_previews
                    .get(&(chat_id, message_id))
                    .is_some_and(|preview| {
                        preview.chat_id == chat_id
                            && preview.message_id == message_id
                            && preview.request_id == request_id
                    })
                {
                    self.media_preview_failed(chat_id, message_id, &error);
                }
                Vec::new()
            }
            NetworkEvent::MessagesDeleted {
                channel_id,
                message_ids,
            } => {
                self.remove_deleted_messages(channel_id, &message_ids);
                self.pinned_messages_deleted(channel_id, &message_ids)
            }
            NetworkEvent::AttachmentDownloaded {
                request_id,
                chat_id,
                message_id,
                path,
            } => {
                if self.downloading_attachments.get(&(chat_id, message_id)) != Some(&request_id) {
                    return Vec::new();
                }
                self.downloading_attachments.remove(&(chat_id, message_id));
                self.downloaded_attachments
                    .insert((chat_id, message_id), path.clone());
                if self.reveal_after_download.remove(&(chat_id, message_id)) {
                    self.reveal_download(&path);
                    return Vec::new();
                }
                let location = sanitize_terminal_line(&path.display().to_string());
                self.status_message = Some(match self.settings.download_behavior {
                    DownloadBehavior::CacheOnly => format!("Downloaded to {location}"),
                    DownloadBehavior::RevealOnActivation => {
                        format!("Downloaded to {location} · activate again to reveal")
                    }
                });
                Vec::new()
            }
            NetworkEvent::AttachmentDownloadFailed {
                request_id,
                chat_id,
                message_id,
                error,
            } => {
                if self.downloading_attachments.get(&(chat_id, message_id)) != Some(&request_id) {
                    return Vec::new();
                }
                self.downloading_attachments.remove(&(chat_id, message_id));
                self.reveal_after_download.remove(&(chat_id, message_id));
                self.status_message = Some(format!(
                    "Download failed: {}",
                    sanitize_terminal_line(&error)
                ));
                Vec::new()
            }
            NetworkEvent::LinkResolved { url, chat, message } => {
                if self.pending_telegram_link.as_deref() != Some(url.as_str()) {
                    return Vec::new();
                }
                self.pending_telegram_link = None;
                self.open_resolved_link(chat, message)
            }
            NetworkEvent::LinkFailed { url, error } => {
                if self.pending_telegram_link.as_deref() != Some(url.as_str()) {
                    return Vec::new();
                }
                self.pending_telegram_link = None;
                self.status_message = Some(format!(
                    "Could not open Telegram link: {}",
                    sanitize_terminal_line(&error)
                ));
                Vec::new()
            }
            NetworkEvent::ButtonActivated {
                chat_id: _,
                message_id: _,
                message,
                url,
            } => {
                let message = message.map(|value| sanitize_terminal_line(&value));
                if let Some(url) = url {
                    let commands = self.activate_url(&url);
                    if let Some(message) = message.filter(|value| !value.is_empty()) {
                        self.status_message = Some(message);
                    }
                    commands
                } else {
                    self.status_message = Some(
                        message
                            .filter(|value| !value.is_empty())
                            .unwrap_or_else(|| "Button activated".to_owned()),
                    );
                    Vec::new()
                }
            }
            NetworkEvent::ButtonFailed {
                chat_id: _,
                message_id: _,
                error,
            } => {
                self.status_message =
                    Some(format!("Button failed: {}", sanitize_terminal_line(&error)));
                Vec::new()
            }
            NetworkEvent::Status(status) => {
                self.connection = status;
                if status != ConnectionStatus::Online {
                    self.metrics = crate::statusline::Metrics::default();
                }
                if status == ConnectionStatus::Online {
                    self.status_message = None;
                }
                Vec::new()
            }
            NetworkEvent::Error(message) => {
                if self.auth_restart_pending && matches!(self.screen, Screen::Auth(_)) {
                    return Vec::new();
                }
                if matches!(self.screen, Screen::Auth(_))
                    && !matches!(
                        self.auth_progress,
                        Some(AuthProgress::StartQr | AuthProgress::WaitQr)
                    )
                {
                    self.auth_progress = None;
                }
                self.status_message = Some(sanitize_terminal_line(&message));
                Vec::new()
            }
            NetworkEvent::Fatal(message) => {
                self.screen = Screen::Fatal(sanitize_terminal_text(&message));
                Vec::new()
            }
        }
    }

    #[must_use]
    pub const fn auth_input(&self) -> &TextInput {
        &self.auth_input
    }

    #[must_use]
    pub fn auth_display_value(&self) -> String {
        if matches!(self.screen, Screen::Auth(AuthPhase::Password { .. })) {
            "•".repeat(self.auth_input.grapheme_count())
        } else {
            self.auth_input.value().to_owned()
        }
    }

    #[must_use]
    pub fn auth_cursor_display_width(&self) -> usize {
        if matches!(self.screen, Screen::Auth(AuthPhase::Password { .. })) {
            self.auth_input.cursor_grapheme()
        } else {
            self.auth_input.cursor_display_width()
        }
    }

    #[must_use]
    pub const fn auth_is_submitting(&self) -> bool {
        self.auth_progress.is_some() || matches!(self.screen, Screen::Auth(AuthPhase::Qr { .. }))
    }

    #[must_use]
    pub const fn qr_render_mode(&self) -> QrRenderMode {
        self.qr_render_mode
    }

    /// A fixed, non-secret progress label for the current authentication
    /// request. Values entered by the user are deliberately never included.
    #[must_use]
    pub const fn auth_progress_label(&self) -> Option<&'static str> {
        match self.auth_progress {
            Some(AuthProgress::RequestCode) => Some("Requesting a login code…"),
            Some(AuthProgress::StartQr) => Some("Preparing QR sign-in…"),
            Some(AuthProgress::CheckCode) => Some("Checking login code…"),
            Some(AuthProgress::CheckPassword) => Some("Checking 2FA password…"),
            Some(AuthProgress::WaitQr) => Some("Waiting for approval in Telegram…"),
            Some(AuthProgress::Restart) => Some("Returning to phone sign-in…"),
            None => None,
        }
    }

    #[must_use]
    pub fn filtered_chat_indices(&self) -> Vec<usize> {
        let query = self.filter.value().to_lowercase();
        let folder = self
            .folders
            .iter()
            .find(|folder| folder.id == self.folder_id);
        let now = chrono::Utc::now().timestamp();
        let mut indices: Vec<_> = self
            .chats
            .iter()
            .enumerate()
            .filter_map(|(index, chat)| {
                (folder.is_none_or(|folder| folder.contains(chat, now))
                    && (query.is_empty()
                        || chat.title.to_lowercase().contains(&query)
                        || chat.last_message.to_lowercase().contains(&query)))
                .then_some(index)
            })
            .collect();
        indices.sort_by_key(|&index| {
            (
                self.chat_pin_position(self.chats[index].id)
                    .unwrap_or(usize::MAX),
                std::cmp::Reverse(self.chats[index].last_activity),
            )
        });
        indices
    }

    #[must_use]
    pub fn visible_chats(&self) -> Vec<&Chat> {
        self.filtered_chat_indices()
            .into_iter()
            .map(|index| &self.chats[index])
            .collect()
    }

    #[must_use]
    pub fn selected_chat_entry(&self) -> Option<&Chat> {
        let index = *self.filtered_chat_indices().get(self.selected_chat)?;
        self.chats.get(index)
    }

    #[must_use]
    pub fn active_chat(&self) -> Option<&Chat> {
        let id = self.active_chat_id?;
        self.chats.iter().find(|chat| chat.id == id)
    }

    #[must_use]
    pub fn active_messages(&self) -> &[Message] {
        self.active_chat_id
            .and_then(|id| self.messages.get(&id))
            .map_or(&[], Vec::as_slice)
    }

    #[must_use]
    pub fn active_draft(&self) -> Option<&TextInput> {
        self.active_chat_id.and_then(|id| self.draft_for(id))
    }

    #[must_use]
    pub fn active_reply_target(&self) -> Option<&ReplyInfo> {
        self.active_chat_id
            .and_then(|id| self.draft_data(id))
            .and_then(|draft| draft.reply.as_ref())
    }

    /// Explicit selection, otherwise the last visible message in the conversation.
    #[must_use]
    pub fn inspected_message(&self) -> Option<&Message> {
        if self.focus != Focus::Conversation {
            return None;
        }
        let id = self
            .selected_message
            .or_else(|| self.message_hit_regions.last().map(|region| region.3.0))?;
        self.active_messages()
            .iter()
            .find(|message| message.id == id)
    }

    #[must_use]
    pub fn draft_for(&self, chat_id: ChatId) -> Option<&TextInput> {
        self.draft_data(chat_id).map(|draft| &draft.input)
    }

    #[must_use]
    pub fn attachment_state(&self, chat_id: ChatId, message_id: i32) -> AttachmentState {
        if self
            .downloaded_attachments
            .contains_key(&(chat_id, message_id))
        {
            AttachmentState::Downloaded
        } else if self
            .downloading_attachments
            .contains_key(&(chat_id, message_id))
        {
            AttachmentState::Downloading
        } else {
            AttachmentState::Ready
        }
    }

    #[must_use]
    pub fn message_is_actionable(&self, message: &Message) -> bool {
        !self.message_actions(message).is_empty()
    }

    #[must_use]
    pub fn message_actions(&self, message: &Message) -> Vec<MessageAction> {
        let mut actions = Vec::new();
        if message.reply_to.is_some() {
            actions.push(MessageAction::Reply);
        }
        if (message.id > 0 && message.attachment.is_some())
            || self
                .retry_attachments
                .contains_key(&(message.chat_id, message.id))
        {
            actions.push(MessageAction::Attachment);
        }
        if message.poll.is_some() {
            actions.push(MessageAction::Poll);
        }
        if message.has_spoilers() {
            actions.push(MessageAction::Spoilers);
        }
        if message.entities.iter().any(|entity| {
            matches!(
                entity.kind,
                crate::entities::Kind::Quote { collapsed: true }
            ) && entity.valid_for(&message.text)
        }) {
            actions.push(MessageAction::ExpandQuote);
        }
        if !message.has_spoilers() || self.spoilers_revealed(message) {
            actions.extend((0..message.links.len()).map(MessageAction::Link));
        }
        actions.extend(
            message
                .buttons
                .iter()
                .enumerate()
                .filter(|(_, button)| button.kind.is_supported())
                .map(|(index, _)| MessageAction::Button(index)),
        );
        if message.id > 0
            && message
                .reactions
                .as_ref()
                .is_some_and(|summary| !summary.counts.is_empty())
        {
            actions.push(MessageAction::Reactions);
        }
        actions
    }

    /// Replace row hit regions after rendering the current conversation.
    pub fn set_message_hit_regions(&mut self, regions: Vec<MessageHitRegion>) {
        self.message_hit_regions = regions;
    }

    pub fn set_chat_hit_regions(&mut self, regions: Vec<(u16, u16, u16, usize)>) {
        self.chat_hit_regions = regions;
    }

    pub fn set_settings_hit_regions(&mut self, regions: Vec<(u16, u16, u16, usize)>) {
        self.settings_hit_regions = regions;
    }

    pub fn set_account_hit_regions(&mut self, regions: Vec<(u16, u16, u16, usize)>) {
        self.account_hit_regions = regions;
    }

    pub const fn set_chat_pane_region(&mut self, region: (u16, u16, u16, u16)) {
        self.chat_pane_region = Some(region);
    }

    pub const fn set_composer_region(&mut self, region: (u16, u16, u16, u16)) {
        self.composer_region = Some(region);
    }

    #[must_use]
    pub const fn composer_region(&self) -> Option<(u16, u16, u16, u16)> {
        self.composer_region
    }

    pub const fn set_conversation_pane_region(&mut self, region: (u16, u16, u16, u16)) {
        self.conversation_pane_region = Some(region);
    }

    pub fn clear_status(&mut self) {
        self.status_message = None;
    }

    pub const fn set_narrow_conversation(&mut self, visible: bool) {
        self.narrow_conversation = visible;
    }

    pub fn take_force_redraw(&mut self) -> bool {
        std::mem::take(&mut self.force_redraw)
    }

    #[must_use]
    pub fn needs_animation(&self) -> bool {
        if self
            .metrics
            .latency
            .is_some_and(crate::statusline::Latency::expired)
        {
            return true;
        }
        if self.keymap.pending() {
            return true;
        }
        match self.screen {
            Screen::Connecting | Screen::Auth(AuthPhase::Qr { .. }) => true,
            Screen::Main => {
                self.loading_history
                    || self.search.loading
                    || self
                        .stickers
                        .panel
                        .as_ref()
                        .is_some_and(|panel| panel.loading.is_some())
                    || self.media_previews.values().any(|preview| preview.loading)
                    || matches!(
                        self.connection,
                        ConnectionStatus::Connecting | ConnectionStatus::Reconnecting
                    )
            }
            Screen::Auth(_) => self.auth_progress.is_some(),
            Screen::Fatal(_) => false,
        }
    }

    fn handle_paste(&mut self, text: &str) -> Vec<TelegramCommand> {
        if self.screen == Screen::Main && self.mode == Mode::Help && self.help.editing {
            self.help.query.insert_str(&sanitize_terminal_line(text));
            self.help.edited();
            return Vec::new();
        }
        if self.screen == Screen::Main && self.mode == Mode::Command {
            self.paste_command(text);
            return Vec::new();
        }
        let normalized = sanitize_terminal_text(&text.replace("\r\n", "\n").replace('\r', "\n"));
        if self.screen == Screen::Main
            && matches!(self.mode, Mode::Navigate | Mode::Compose)
            && self.focus == Focus::Conversation
            && self.active_chat_id.is_some()
        {
            if self.keymap.attachments.auto_attach_paths {
                return self.prepare_attachments(normalized, true);
            }
            if self.keymap.attachments.auto_attach_images
                && crate::staging::image_path_candidate(&normalized)
            {
                return self.paste_image_paths(normalized);
            }
            self.mode = Mode::Compose;
        }
        match self.screen {
            Screen::Auth(
                AuthPhase::Phone | AuthPhase::Code { .. } | AuthPhase::Password { .. },
            ) if self.auth_progress.is_none() => {
                self.auth_input.insert_str(&normalized.replace('\n', ""));
            }
            Screen::Main if self.mode == Mode::Edit && !self.message_edit_pending() => {
                if let Some(chat) = self.active_chat_id
                    && let Some(edit) = &mut self.draft_data_mut(chat).edit
                {
                    edit.input.insert_str(&normalized);
                }
            }
            Screen::Main if self.mode == Mode::Compose => {
                if let Some(chat_id) = self.active_chat_id {
                    self.draft_data_mut(chat_id).input.insert_str(&normalized);
                    return self.refresh_completion(chat_id);
                }
            }
            Screen::Main if self.mode == Mode::Search && self.search.editing => {
                self.search.query.insert_str(&normalized.replace('\n', ""));
                return self.search_edited();
            }
            Screen::Main if self.mode == Mode::Filter => {
                self.filter
                    .insert_str(&normalized.split_whitespace().collect::<Vec<_>>().join(" "));
                self.selected_chat = 0;
            }
            _ => {}
        }
        Vec::new()
    }

    fn handle_auth(&mut self, action: KeyAction) -> Vec<TelegramCommand> {
        if action == KeyAction::Escape {
            if self.auth_restart_pending {
                self.auth_input.clear();
                self.status_message = None;
                return Vec::new();
            }
            let operation_pending = self.auth_progress.is_some();
            self.auth_input.clear();
            self.auth_progress = None;
            self.status_message = None;
            if operation_pending
                || matches!(
                    self.screen,
                    Screen::Auth(
                        AuthPhase::Qr { .. } | AuthPhase::Code { .. } | AuthPhase::Password { .. }
                    )
                )
            {
                self.screen = Screen::Auth(AuthPhase::Phone);
                self.auth_restart_pending = true;
                self.auth_progress = Some(AuthProgress::Restart);
                return vec![TelegramCommand::RestartAuth];
            }
            return Vec::new();
        }
        if action == KeyAction::Tab
            && self.auth_progress.is_none()
            && matches!(self.screen, Screen::Auth(AuthPhase::Phone))
        {
            self.auth_input.clear();
            self.auth_progress = Some(AuthProgress::StartQr);
            self.status_message = None;
            return vec![TelegramCommand::StartQrAuth];
        }
        if matches!(action, KeyAction::Tab | KeyAction::BackTab)
            && matches!(self.screen, Screen::Auth(AuthPhase::Qr { .. }))
        {
            self.qr_render_mode = self.qr_render_mode.toggled();
            self.force_redraw = true;
            return Vec::new();
        }
        if self.auth_progress.is_some() {
            if action == KeyAction::Redraw {
                self.force_redraw = true;
            }
            return Vec::new();
        }
        match action {
            KeyAction::Character(character) => self.auth_input.insert(character),
            KeyAction::Backspace => _ = self.auth_input.backspace(),
            KeyAction::Delete => _ = self.auth_input.delete(),
            KeyAction::Left => _ = self.auth_input.move_left(),
            KeyAction::Right => _ = self.auth_input.move_right(),
            KeyAction::Home => self.auth_input.move_home(),
            KeyAction::End => self.auth_input.move_end(),
            KeyAction::Clear => self.auth_input.clear(),
            KeyAction::DeleteWord => _ = self.auth_input.delete_word_before(),
            KeyAction::Redraw => self.force_redraw = true,
            KeyAction::Enter => return self.submit_auth(),
            _ => {}
        }
        Vec::new()
    }

    fn submit_auth(&mut self) -> Vec<TelegramCommand> {
        if self.auth_input.is_empty() {
            return Vec::new();
        }
        let value = self.auth_input.take();
        let command = match self.screen {
            Screen::Auth(AuthPhase::Phone) => TelegramCommand::SubmitPhone(value.trim().to_owned()),
            Screen::Auth(AuthPhase::Code { .. }) => {
                TelegramCommand::SubmitCode(value.trim().to_owned())
            }
            Screen::Auth(AuthPhase::Password { .. }) => TelegramCommand::SubmitPassword(value),
            _ => return Vec::new(),
        };
        let empty = match &command {
            TelegramCommand::SubmitPhone(value) | TelegramCommand::SubmitCode(value) => {
                value.is_empty()
            }
            _ => false,
        };
        if empty {
            Vec::new()
        } else {
            self.auth_progress = Some(match &command {
                TelegramCommand::SubmitPhone(_) => AuthProgress::RequestCode,
                TelegramCommand::SubmitCode(_) => AuthProgress::CheckCode,
                TelegramCommand::SubmitPassword(_) => AuthProgress::CheckPassword,
                _ => unreachable!("only authentication submissions reach this point"),
            });
            self.status_message = None;
            vec![command]
        }
    }

    fn handle_main(&mut self, action: KeyAction) -> Vec<TelegramCommand> {
        match self.mode {
            Mode::Navigate => self.handle_navigation(action),
            Mode::Compose => self.handle_compose(action),
            Mode::Edit => self.edit_message_input(action),
            Mode::Command => self.edit_command(action),
            Mode::Status => {
                match action {
                    KeyAction::Escape => self.mode = Mode::Navigate,
                    KeyAction::Up => {
                        self.configuration.status_scroll =
                            self.configuration.status_scroll.saturating_sub(1);
                    }
                    KeyAction::Down => {
                        self.configuration.status_scroll =
                            self.configuration.status_scroll.saturating_add(1);
                    }
                    _ => {}
                }
                Vec::new()
            }
            Mode::Preview => {
                if action == KeyAction::Escape {
                    self.mode = Mode::Navigate;
                    self.force_redraw = true;
                }
                Vec::new()
            }
            Mode::Filter => self.handle_filter(action),
            Mode::Search => self.edit_search(action),
            Mode::ChatInfo
            | Mode::Invite
            | Mode::Attachments
            | Mode::PinnedMessages
            | Mode::PinPrompt
            | Mode::DeletePrompt
            | Mode::ForwardPrompt
            | Mode::Poll
            | Mode::Reactions
            | Mode::Stickers => Vec::new(),
            Mode::Colors => self.handle_colors(action),
            Mode::Help => self.handle_help(action),
            Mode::Settings => self.handle_settings(action),
            Mode::Accounts => self.handle_accounts(action),
        }
    }

    fn handle_navigation(&mut self, action: KeyAction) -> Vec<TelegramCommand> {
        match action {
            KeyAction::Character('q') => self.quit(),
            KeyAction::Character('?') => {
                self.open_help();
                Vec::new()
            }
            KeyAction::Character('s') => {
                self.mode_before_settings = self.mode;
                self.mode = Mode::Settings;
                self.settings_selection = 0;
                self.status_message = None;
                Vec::new()
            }
            KeyAction::Character('a') => {
                self.mode_before_accounts = self.mode;
                self.mode = Mode::Accounts;
                self.account_selection = usize::from(self.settings.active_account - 1);
                self.status_message = None;
                Vec::new()
            }
            KeyAction::Character('/') if self.focus == Focus::Chats => {
                self.mode = Mode::Filter;
                self.clamp_chat_selection();
                Vec::new()
            }
            KeyAction::Character('/') => {
                _ = self.start_plain_composing();
                self.insert_active('/');
                Vec::new()
            }
            KeyAction::Character('i') => self.compose_or_reply(),
            KeyAction::Tab | KeyAction::BackTab => {
                self.toggle_focus();
                if self.focus == Focus::Conversation && self.message_scroll == 0 {
                    self.reach_bottom()
                } else {
                    Vec::new()
                }
            }
            KeyAction::Escape if self.selected_message.is_some() => {
                self.selected_message = None;
                Vec::new()
            }
            KeyAction::Escape | KeyAction::Left => {
                self.sidebar_hidden = false;
                self.narrow_conversation = false;
                self.focus = Focus::Chats;
                self.selected_message = None;
                Vec::new()
            }
            KeyAction::Enter if self.focus == Focus::Chats => self.open_selected_chat(),
            KeyAction::Enter if self.selected_message.is_some() => self.activate_selected_message(),
            KeyAction::Enter => self.start_composing(),
            KeyAction::Right => {
                if self.active_chat_id.is_some() {
                    self.focus = Focus::Conversation;
                    self.narrow_conversation = true;
                    if self.message_scroll == 0 {
                        self.reach_bottom()
                    } else {
                        Vec::new()
                    }
                } else {
                    self.open_selected_chat()
                }
            }
            KeyAction::Character('o') if self.focus == Focus::Conversation => {
                self.preview_selected_media()
            }
            KeyAction::Character('O') if self.focus == Focus::Conversation => {
                self.reveal_selected_attachment()
            }
            KeyAction::Character('l') if self.focus == Focus::Conversation => {
                self.activate_selected_link()
            }
            KeyAction::Character('r') if self.focus == Focus::Conversation => {
                self.navigate_to_selected_reply()
            }
            KeyAction::Character('R') if self.focus == Focus::Conversation => {
                self.start_replying_to_selected()
            }
            KeyAction::Character('[') if self.focus == Focus::Conversation => {
                self.select_message(false)
            }
            KeyAction::Character(']') if self.focus == Focus::Conversation => {
                self.select_message(true)
            }
            KeyAction::Up | KeyAction::Character('k') => self.move_up(1),
            KeyAction::Down | KeyAction::Character('j') => self.move_down(1),
            KeyAction::PageUp => self.move_up(PAGE_STEP),
            KeyAction::PageDown => self.move_down(PAGE_STEP),
            KeyAction::Home | KeyAction::Character('g') => self.move_to_start(),
            KeyAction::End | KeyAction::Character('G') => self.move_to_end(),
            KeyAction::Redraw => {
                self.force_redraw = true;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn handle_compose(&mut self, action: KeyAction) -> Vec<TelegramCommand> {
        let Some(chat_id) = self.active_chat_id else {
            self.mode = Mode::Navigate;
            return Vec::new();
        };
        if action == KeyAction::Escape {
            if self.dismiss_completion() {
                return Vec::new();
            }
            if self.draft_data_mut(chat_id).reply.take().is_some() {
                self.selected_message = None;
                self.status_message = Some("Reply cancelled · draft kept".to_owned());
                return Vec::new();
            }
            self.mode = Mode::Navigate;
            self.completion = None;
            return Vec::new();
        }
        if action == KeyAction::Enter {
            if self.accept_completion() {
                return Vec::new();
            }
            self.completion = None;
            return self.send_draft(chat_id);
        }
        let draft = &mut self.draft_data_mut(chat_id).input;
        match action {
            KeyAction::Character(character) => draft.insert(character),
            KeyAction::Newline => draft.insert('\n'),
            KeyAction::Backspace => _ = draft.backspace(),
            KeyAction::Delete => _ = draft.delete(),
            KeyAction::Left => _ = draft.move_left(),
            KeyAction::Right => _ = draft.move_right(),
            KeyAction::Home => draft.move_home(),
            KeyAction::End => draft.move_end(),
            KeyAction::Clear => draft.clear(),
            KeyAction::DeleteWord => _ = draft.delete_word_before(),
            KeyAction::Redraw => self.force_redraw = true,
            _ => {}
        }
        self.refresh_completion(chat_id)
    }

    fn handle_filter(&mut self, action: KeyAction) -> Vec<TelegramCommand> {
        match action {
            KeyAction::Escape => {
                self.filter.clear();
                self.selected_chat = 0;
                self.mode = Mode::Navigate;
                Vec::new()
            }
            KeyAction::Enter => {
                let commands = self.open_selected_chat();
                if !commands.is_empty() {
                    let active_id = self.active_chat_id;
                    self.filter.clear();
                    self.selected_chat = active_id
                        .and_then(|id| self.chats.iter().position(|chat| chat.id == id))
                        .unwrap_or(0);
                    self.mode = Mode::Navigate;
                }
                commands
            }
            KeyAction::Character(character) => {
                self.filter.insert(character);
                self.selected_chat = 0;
                Vec::new()
            }
            KeyAction::Backspace => {
                _ = self.filter.backspace();
                self.clamp_chat_selection();
                Vec::new()
            }
            KeyAction::Delete => {
                _ = self.filter.delete();
                self.clamp_chat_selection();
                Vec::new()
            }
            KeyAction::Left => {
                _ = self.filter.move_left();
                Vec::new()
            }
            KeyAction::Right => {
                _ = self.filter.move_right();
                Vec::new()
            }
            KeyAction::Up => self.move_chat_up(1),
            KeyAction::Down => self.move_chat_down(1),
            KeyAction::PageUp => self.move_chat_up(PAGE_STEP),
            KeyAction::PageDown => self.move_chat_down(PAGE_STEP),
            KeyAction::Home => {
                self.filter.move_home();
                Vec::new()
            }
            KeyAction::End => {
                self.filter.move_end();
                Vec::new()
            }
            KeyAction::Clear => {
                self.filter.clear();
                self.selected_chat = 0;
                Vec::new()
            }
            KeyAction::DeleteWord => {
                _ = self.filter.delete_word_before();
                self.clamp_chat_selection();
                Vec::new()
            }
            KeyAction::Redraw => {
                self.force_redraw = true;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn handle_settings(&mut self, action: KeyAction) -> Vec<TelegramCommand> {
        if action == KeyAction::Character('q') {
            return self.quit();
        }
        match action {
            KeyAction::Escape | KeyAction::Character('s') => {
                self.mode = self.mode_before_settings;
                self.status_message = None;
            }
            KeyAction::Up | KeyAction::Character('k') => {
                self.settings_selection = self.settings_selection.saturating_sub(1);
            }
            KeyAction::Down | KeyAction::Character('j') => {
                self.settings_selection = self.settings_selection.saturating_add(1).min(3);
            }
            KeyAction::Enter | KeyAction::Left | KeyAction::Right | KeyAction::Character(' ') => {
                self.toggle_selected_setting();
            }
            KeyAction::Character('u') => {
                self.settings_selection = 0;
                self.toggle_selected_setting();
            }
            KeyAction::Character('c') => {
                self.settings_selection = 1;
                self.toggle_selected_setting();
            }
            KeyAction::Character('d') => {
                self.settings_selection = 2;
                self.toggle_selected_setting();
            }
            KeyAction::Character('m') => {
                self.settings_selection = 3;
                self.toggle_selected_setting();
            }
            KeyAction::Redraw => self.force_redraw = true,
            _ => {}
        }
        Vec::new()
    }

    fn handle_accounts(&mut self, action: KeyAction) -> Vec<TelegramCommand> {
        if action == KeyAction::Character('q') {
            return self.quit();
        }
        let add_row = usize::from(self.settings.account_count);
        match action {
            KeyAction::Escape | KeyAction::Character('a') => {
                self.mode = self.mode_before_accounts;
                self.status_message = None;
                Vec::new()
            }
            KeyAction::Up | KeyAction::Character('k') => {
                self.account_selection = self.account_selection.saturating_sub(1);
                Vec::new()
            }
            KeyAction::Down | KeyAction::Character('j') => {
                self.account_selection = self.account_selection.saturating_add(1).min(add_row);
                Vec::new()
            }
            KeyAction::Enter => {
                if self.account_selection == add_row {
                    self.add_account()
                } else {
                    let account = u8::try_from(self.account_selection.saturating_add(1))
                        .unwrap_or(MAX_ACCOUNTS);
                    self.activate_account(account, self.settings.account_count)
                }
            }
            KeyAction::Character(character) if character.is_ascii_digit() => {
                let Some(account) = character
                    .to_digit(10)
                    .and_then(|value| u8::try_from(value).ok())
                else {
                    return Vec::new();
                };
                if account == 0 || account > self.settings.account_count {
                    self.status_message = Some("That account slot has not been added".to_owned());
                    Vec::new()
                } else {
                    self.activate_account(account, self.settings.account_count)
                }
            }
            KeyAction::Redraw => {
                self.force_redraw = true;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn switch_to_next_account(&mut self) -> Vec<TelegramCommand> {
        if self.settings.account_count < 2 {
            self.status_message = Some(format!(
                "Only one account · {} to add another",
                self.keymap
                    .hint(crate::keymap::Context::Global, "add_account")
            ));
            return Vec::new();
        }
        let account = if self.settings.active_account == self.settings.account_count {
            1
        } else {
            self.settings.active_account.saturating_add(1)
        };
        self.activate_account(account, self.settings.account_count)
    }

    fn add_account(&mut self) -> Vec<TelegramCommand> {
        if self.settings.account_count >= MAX_ACCOUNTS {
            self.status_message = Some(format!("Account limit reached ({MAX_ACCOUNTS})"));
            return Vec::new();
        }
        let account = self.settings.account_count.saturating_add(1);
        self.activate_account(account, account)
    }

    fn activate_account(&mut self, account: u8, account_count: u8) -> Vec<TelegramCommand> {
        if account == self.settings.active_account && account_count == self.settings.account_count {
            if self.mode == Mode::Accounts {
                self.mode = self.mode_before_accounts;
            }
            self.status_message = Some(format!("Account {account} is already active"));
            return Vec::new();
        }
        if account == 0 || account > account_count || account_count > MAX_ACCOUNTS {
            self.status_message = Some("Invalid account slot".to_owned());
            return Vec::new();
        }

        let previous = self.settings;
        self.settings.active_account = account;
        self.settings.account_count = account_count;
        if let Some(path) = self.settings_path.as_deref()
            && let Err(error) = self.settings.save_to(path)
        {
            self.settings = previous;
            self.status_message = Some(format!(
                "Could not save account selection: {}",
                sanitize_terminal_line(&error.to_string())
            ));
            return Vec::new();
        }

        self.reset_for_account_switch(account);
        vec![TelegramCommand::SwitchAccount { account }]
    }

    fn reset_for_account_switch(&mut self, account: u8) {
        self.cancel_alerts();
        let settings = self.settings;
        let settings_path = self.settings_path.clone();
        let available_update = self.available_update.clone();
        let terminal_focused = self.terminal_focused;
        let terminal_background = self.terminal_background;
        let qr_render_mode = self.qr_render_mode;
        let appearance = self.appearance.clone();
        let navigation = self.navigation.clone();
        let sidebar_hidden = self.sidebar_hidden;
        let mut drafts = std::mem::take(&mut self.drafts);
        drafts.retain(|key, _| key.account != 0);
        let draft_accounts = std::mem::take(&mut self.draft_accounts);
        let draft_modified_accounts = std::mem::take(&mut self.draft_modified_accounts);
        let drafts_dirty = self.drafts_dirty;
        let mut commands = std::mem::take(&mut self.commands);
        commands.reset_session();
        let configuration = self.configuration.clone();
        let mut keymap = self.keymap.clone();
        keymap.reset();
        *self = Self {
            drafts,
            draft_accounts,
            draft_modified_accounts,
            drafts_dirty,
            appearance,
            navigation,
            sidebar_hidden,
            commands,
            keymap,
            configuration,
            settings,
            settings_path,
            available_update,
            terminal_focused,
            terminal_background,
            qr_render_mode,
            status_message: Some(format!("Switching to Account {account}…")),
            force_redraw: true,
            ..Self::default()
        };
    }

    fn toggle_selected_setting(&mut self) {
        let previous = self.settings;
        let previous_available_update = self.available_update.clone();
        match self.settings_selection {
            0 => {
                self.settings.automatic_update_checks = !self.settings.automatic_update_checks;
                if !self.settings.automatic_update_checks {
                    self.available_update = None;
                }
            }
            1 => {
                self.settings.release_channel = self.settings.release_channel.toggled();
            }
            2 => {
                self.settings.download_behavior = self.settings.download_behavior.toggled();
            }
            3 => {
                self.settings.show_message_ids = !self.settings.show_message_ids;
            }
            _ => return,
        }
        let Some(path) = self.settings_path.as_deref() else {
            self.status_message = Some("Settings changed for this session".to_owned());
            return;
        };
        if let Err(error) = self.settings.save_to(path) {
            self.settings = previous;
            self.available_update = previous_available_update;
            self.status_message = Some(format!(
                "Could not save settings: {}",
                sanitize_terminal_line(&error.to_string())
            ));
        } else {
            self.status_message = Some("Settings saved".to_owned());
        }
    }

    fn quit(&mut self) -> Vec<TelegramCommand> {
        if self.should_quit {
            return Vec::new();
        }
        self.should_quit = true;
        vec![TelegramCommand::Shutdown]
    }

    fn toggle_focus(&mut self) {
        self.focus = if self.active_chat_id.is_some() && self.focus == Focus::Chats {
            Focus::Conversation
        } else {
            Focus::Chats
        };
        self.narrow_conversation = self.focus == Focus::Conversation;
        if self.focus == Focus::Chats {
            self.sidebar_hidden = false;
        }
    }

    fn start_plain_composing(&mut self) -> Vec<TelegramCommand> {
        if let Some(chat_id) = self.active_chat_id {
            self.draft_data_mut(chat_id).reply.take();
        }
        self.start_composing()
    }

    fn insert_active(&mut self, character: char) {
        if let Some(chat_id) = self.active_chat_id {
            self.draft_data_mut(chat_id).input.insert(character);
        }
    }

    fn send_draft(&mut self, chat_id: ChatId) -> Vec<TelegramCommand> {
        if let Some(reason) = self.draft_restriction(chat_id) {
            self.status_message = Some(format!("{reason} · draft kept · :info for details"));
            return Vec::new();
        }
        if self.preparing_attachments() {
            self.status_message = Some(
                "Wait for attachment preparation or cancel it in the attachment list".to_owned(),
            );
            return Vec::new();
        }
        if self
            .draft_data(chat_id)
            .is_some_and(|draft| !draft.attachments.is_empty())
        {
            return self.send_staged_attachments(chat_id);
        }
        let draft = &mut self.draft_data_mut(chat_id).input;
        if draft.value().trim().is_empty() {
            return Vec::new();
        }
        let text = draft.take();
        let reply_to = self.draft_data_mut(chat_id).reply.take();
        let replacing_failed = self.retry_message_ids.remove(&chat_id);
        if let Some(failed_id) = replacing_failed
            && let Some(messages) = self.messages.get_mut(&chat_id)
        {
            messages.retain(|message| message.id != failed_id);
        }
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
            text: text.clone(),
            timestamp: Utc::now(),
            outgoing: true,
            delivery: Delivery::Pending,
            attachment: None,
            links: plain_message_links(&text),
            buttons: Vec::new(),
        };
        let mut commands = self.receive_message(pending, replacing_failed.is_none(), false);
        self.status_message = None;
        commands.push(TelegramCommand::SendMessage {
            chat_id,
            local_id,
            text,
            reply_to: reply_to.map(|reply| reply.message_id),
        });
        commands
    }

    fn send_staged_attachments(&mut self, chat_id: ChatId) -> Vec<TelegramCommand> {
        let mut draft = std::mem::take(self.draft_data_mut(chat_id));
        self.draft_data_mut(chat_id).edit = draft.edit.take();
        let mut caption = draft.input.value().to_owned();
        let mut reply_to = draft.reply;
        let total = draft.attachments.len();
        let mut commands = Vec::new();
        for (index, staged) in draft.attachments.into_iter().enumerate() {
            let path = &staged.path;
            let local_id = self.next_pending_id;
            self.next_pending_id = self.next_pending_id.checked_sub(1).unwrap_or(-1);
            let mut attachment = attachment_from_path(path);
            attachment.size = Some(staged.size);
            let as_photo = staged.as_photo;
            attachment.kind = if as_photo {
                AttachmentKind::Photo
            } else {
                AttachmentKind::File
            };
            let item_caption = if index == 0 {
                std::mem::take(&mut caption)
            } else {
                String::new()
            };
            let item_reply = if index == 0 { reply_to.take() } else { None };
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
                reply_to: item_reply.clone(),
                text: item_caption.clone(),
                timestamp: Utc::now(),
                outgoing: true,
                delivery: Delivery::Pending,
                attachment: Some(attachment),
                links: plain_message_links(&item_caption),
                buttons: Vec::new(),
            };
            commands.extend(self.receive_message(pending, true, false));
            commands.push(TelegramCommand::SendAttachment {
                chat_id,
                local_id,
                attachment: staged,
                caption: item_caption,
                reply_to: item_reply.map(|reply| reply.message_id),
            });
        }
        self.status_message = Some(format!("Sending {total} attachment(s)…"));
        commands
    }

    fn select_message(&mut self, forward: bool) -> Vec<TelegramCommand> {
        let message_ids = self
            .active_messages()
            .iter()
            .filter(|message| message.id > 0)
            .map(|message| message.id)
            .collect::<Vec<_>>();
        if message_ids.is_empty() {
            self.status_message = Some("No sent messages are loaded".to_owned());
            return Vec::new();
        }
        let current = self
            .selected_message
            .and_then(|id| message_ids.iter().position(|candidate| *candidate == id));
        let index = match (current, forward) {
            (Some(index), true) => index.saturating_add(1).min(message_ids.len() - 1),
            (Some(index), false) => index.saturating_sub(1),
            (None, _) => message_ids.len() - 1,
        };
        self.select_message_id(message_ids[index], true);
        Vec::new()
    }

    fn select_message_id(&mut self, message_id: i32, anchor: bool) {
        self.selected_message = Some(message_id);
        self.selected_action = 0;
        if anchor {
            self.viewport_anchor_message = Some(message_id);
            self.viewport_anchor_row = 0;
            self.message_scroll = 1;
        }
        self.status_message = None;
    }

    fn start_replying_to_selected(&mut self) -> Vec<TelegramCommand> {
        if self.mode == Mode::Preview && self.selected_message.is_none() {
            self.status_message = Some(format!(
                "Previewed message is no longer available · {} close",
                self.keymap.hint(Context::Preview, "cancel")
            ));
            return Vec::new();
        }
        let Some(chat_id) = self.active_chat_id else {
            return Vec::new();
        };
        let target = self
            .messages
            .get(&chat_id)
            .and_then(|messages| {
                self.selected_message.map_or_else(
                    || messages.iter().rev().find(|message| message.id > 0),
                    |id| messages.iter().find(|message| message.id == id),
                )
            })
            .cloned();
        let Some(target) = target else {
            self.status_message = Some("No sent message is available to reply to".to_owned());
            return Vec::new();
        };
        if target.id <= 0 {
            self.status_message =
                Some("Wait for this message to finish sending before replying".to_owned());
            return Vec::new();
        }
        self.draft_data_mut(chat_id).reply = Some(ReplyInfo {
            message_id: target.id,
            chat_id,
            sender: Some(target.sender),
        });
        self.mode = Mode::Compose;
        self.focus = Focus::Conversation;
        self.selected_message = Some(target.id);
        self.selected_action = 0;
        self.status_message = None;
        Vec::new()
    }

    fn reply_info_for_target(&self, chat_id: ChatId, message_id: i32) -> Option<ReplyInfo> {
        (message_id > 0).then(|| ReplyInfo {
            message_id,
            chat_id,
            sender: self.messages.get(&chat_id).and_then(|messages| {
                messages
                    .iter()
                    .find(|message| message.id == message_id)
                    .map(|message| message.sender.clone())
            }),
        })
    }

    fn select_actionable_message(&mut self, forward: bool) -> Vec<TelegramCommand> {
        let actionable = self
            .active_messages()
            .iter()
            .flat_map(|message| {
                self.message_actions(message)
                    .into_iter()
                    .enumerate()
                    .map(move |(action, _)| (message.id, action))
            })
            .collect::<Vec<_>>();
        if actionable.is_empty() {
            self.selected_message = None;
            self.selected_action = 0;
            self.status_message =
                Some("No replies, files, links, or buttons in loaded messages".to_owned());
            return Vec::new();
        }
        let selected = self.selected_message.and_then(|id| {
            actionable
                .iter()
                .position(|candidate| *candidate == (id, self.selected_action))
        });
        let index = match (selected, forward) {
            (Some(index), true) => index.saturating_add(1).min(actionable.len() - 1),
            (Some(index), false) => index.saturating_sub(1),
            (None, true) => 0,
            (None, false) => actionable.len() - 1,
        };
        let (message_id, action) = actionable[index];
        self.selected_message = Some(message_id);
        self.selected_action = action;
        self.viewport_anchor_message = Some(message_id);
        self.viewport_anchor_row = 0;
        self.message_scroll = 1;
        self.status_message = None;
        Vec::new()
    }

    fn activate_selected_message(&mut self) -> Vec<TelegramCommand> {
        let Some(chat_id) = self.active_chat_id else {
            return Vec::new();
        };
        let Some(message_id) = self.selected_message else {
            return Vec::new();
        };
        let Some(message) = self
            .messages
            .get(&chat_id)
            .and_then(|messages| messages.iter().find(|message| message.id == message_id))
            .cloned()
        else {
            self.selected_message = None;
            return Vec::new();
        };

        let actions = self.message_actions(&message);
        let Some(action) = actions.get(self.selected_action).copied() else {
            self.selected_action = 0;
            return Vec::new();
        };

        if action == MessageAction::Attachment {
            return self.activate_attachment(chat_id, message_id, &message);
        }
        if action == MessageAction::Reactions {
            return self.begin_reactions();
        }
        if action == MessageAction::Poll {
            return self.begin_poll();
        }
        if action == MessageAction::Reply {
            return self.navigate_to_selected_reply();
        }
        if matches!(action, MessageAction::Spoilers | MessageAction::ExpandQuote) {
            self.toggle_text_details(action == MessageAction::Spoilers);
            return Vec::new();
        }
        if let MessageAction::Link(index) = action {
            let Some(link) = message.links.get(index) else {
                return Vec::new();
            };
            return self.activate_url(&link.url);
        }
        let MessageAction::Button(index) = action else {
            return Vec::new();
        };
        let Some(button) = message.buttons.get(index) else {
            return Vec::new();
        };
        if !button.kind.is_supported() {
            self.status_message =
                Some("This button requires a graphical Telegram client".to_owned());
            return Vec::new();
        }
        self.status_message = Some(format!("Activating {}…", button.label));
        vec![TelegramCommand::ActivateButton {
            chat_id,
            message_id,
            button_index: button.index,
        }]
    }

    fn activate_attachment(
        &mut self,
        chat_id: ChatId,
        message_id: i32,
        message: &Message,
    ) -> Vec<TelegramCommand> {
        if let Some(retry) = self.retry_attachments.get(&(chat_id, message_id)).cloned() {
            if let Some(message) = self
                .messages
                .get_mut(&chat_id)
                .and_then(|messages| messages.iter_mut().find(|message| message.id == message_id))
            {
                message.delivery = Delivery::Pending;
            }
            self.retry_attachments.remove(&(chat_id, message_id));
            self.status_message = Some("Retrying attachment…".to_owned());
            return vec![TelegramCommand::SendAttachment {
                chat_id,
                local_id: message_id,
                attachment: retry.attachment,
                caption: retry.caption,
                reply_to: retry.reply_to.map(|reply| reply.message_id),
            }];
        }

        if message.id > 0
            && let Some(attachment) = message.attachment.as_ref().filter(|a| a.supports_preview())
        {
            self.mode = Mode::Preview;
            self.focus = Focus::Conversation;
            self.status_message = None;
            // Explicit activation can retry a failed inline download while the
            // same renderer expands the preview into the available viewport.
            if self
                .media_previews
                .get(&(chat_id, message_id))
                .is_some_and(|p| !p.loading && p.path.is_none())
            {
                self.media_previews.remove(&(chat_id, message_id));
            }
            return self.request_media_preview(chat_id, message_id, attachment);
        }

        if message.attachment.is_some() && message.id > 0 {
            if let Some(path) = self
                .downloaded_attachments
                .get(&(chat_id, message_id))
                .cloned()
            {
                if path.is_file() {
                    let location = sanitize_terminal_line(&path.display().to_string());
                    if self.settings.download_behavior == DownloadBehavior::CacheOnly {
                        self.status_message = Some(format!(
                            "Downloaded to {location} · reveal is disabled in settings"
                        ));
                    } else if let Err(error) = reveal_path(&path) {
                        self.status_message = Some(format!(
                            "Could not reveal attachment: {}",
                            sanitize_terminal_line(&error.to_string())
                        ));
                    } else {
                        self.status_message = Some(format!("Revealed {location}"));
                    }
                    return Vec::new();
                }
                self.downloaded_attachments.remove(&(chat_id, message_id));
            }
            return self.queue_download(
                chat_id,
                message_id,
                message
                    .attachment
                    .as_ref()
                    .and_then(|attachment| attachment.source_id),
            );
        }

        Vec::new()
    }

    fn request_media_preview(
        &mut self,
        chat_id: ChatId,
        message_id: i32,
        attachment: &Attachment,
    ) -> Vec<TelegramCommand> {
        if self.media_previews.contains_key(&(chat_id, message_id)) {
            return Vec::new();
        }
        let thumbnail = attachment.preview_uses_thumbnail();
        let path = (!thumbnail)
            .then(|| self.downloaded_attachments.get(&(chat_id, message_id)))
            .flatten()
            .filter(|path| path.is_file())
            .cloned();
        let request_id = self.next_preview_request_id;
        self.next_preview_request_id = self.next_preview_request_id.wrapping_add(1);
        let loading = path.is_none();
        self.media_previews.insert(
            (chat_id, message_id),
            MediaPreview {
                chat_id,
                message_id,
                request_id,
                path,
                status: if loading && thumbnail {
                    "Loading static sticker preview…"
                } else if loading {
                    "Loading image…"
                } else {
                    ""
                }
                .to_owned(),
                loading,
                thumbnail,
            },
        );
        if loading {
            vec![TelegramCommand::DownloadPreview {
                chat_id,
                message_id,
                request_id,
                thumbnail,
            }]
        } else {
            Vec::new()
        }
    }

    /// Request only visible attachments, with a small shared download budget.
    pub fn request_visible_media(&mut self) -> Vec<TelegramCommand> {
        self.media_previews.retain(|&(chat_id, message_id), _| {
            self.messages
                .get(&chat_id)
                .is_some_and(|messages| messages.iter().any(|m| m.id == message_id))
        });
        let mut budget =
            2_usize.saturating_sub(self.media_previews.values().filter(|p| p.loading).count());
        let mut commands = Vec::new();
        for slot in self.media_slots.clone() {
            let crate::media::MediaSource::Message {
                chat_id,
                message_id,
            } = slot.source
            else {
                continue;
            };
            if budget == 0 {
                break;
            }
            let Some(attachment) = self
                .messages
                .get(&chat_id)
                .and_then(|messages| messages.iter().find(|m| m.id == message_id))
                .and_then(|message| message.attachment.clone())
            else {
                continue;
            };
            let requested = self.request_media_preview(chat_id, message_id, &attachment);
            budget = budget.saturating_sub(requested.len());
            commands.extend(requested);
        }
        commands
    }

    pub fn media_preview_failed(&mut self, chat_id: ChatId, message_id: i32, error: &str) {
        let context = if self.mode == Mode::Preview {
            Context::Preview
        } else {
            Context::Conversation
        };
        if let Some(preview) = self.media_previews.get_mut(&(chat_id, message_id)) {
            preview.path = None;
            preview.loading = false;
            preview.status = format!(
                "Preview unavailable · {} retry: {}",
                self.keymap.hint(context, "preview"),
                sanitize_terminal_line(error)
            );
        }
    }

    fn remove_deleted_messages(&mut self, channel_id: Option<ChatId>, message_ids: &[i32]) {
        let affected_chat =
            |id: ChatId| channel_id.map_or(id > -1_000_000_000_000, |channel| channel == id);
        for chat in &mut self.chats {
            if affected_chat(chat.id)
                && chat
                    .last_message_id
                    .is_some_and(|id| message_ids.contains(&id))
            {
                chat.last_message.clear();
                chat.last_message_id = None;
                if self.refresh_dialogs_pending {
                    self.changed_dialogs.insert(chat.id);
                }
            }
        }
        self.media_previews.retain(|&(chat_id, message_id), _| {
            !affected_chat(chat_id) || !message_ids.contains(&message_id)
        });
        for (&chat_id, messages) in &mut self.messages {
            if affected_chat(chat_id) {
                messages.retain(|message| !message_ids.contains(&message.id));
            }
        }
        self.downloaded_attachments
            .retain(|&(chat_id, message_id), _| {
                !affected_chat(chat_id) || !message_ids.contains(&message_id)
            });
        self.downloading_attachments
            .retain(|&(chat_id, message_id), _| {
                !affected_chat(chat_id) || !message_ids.contains(&message_id)
            });
        self.reveal_after_download.retain(|&(chat_id, message_id)| {
            !affected_chat(chat_id) || !message_ids.contains(&message_id)
        });
        if self.active_chat_id.is_some_and(affected_chat)
            && self
                .selected_message
                .is_some_and(|id| message_ids.contains(&id))
        {
            self.selected_message = None;
            self.selected_action = 0;
        }
    }

    fn activate_selected_link(&mut self) -> Vec<TelegramCommand> {
        let Some(message) = self
            .selected_message
            .and_then(|message_id| {
                self.active_messages()
                    .iter()
                    .find(|message| message.id == message_id)
            })
            .cloned()
        else {
            self.status_message = Some(format!(
                "Select a link with {} first",
                self.keymap
                    .hint(crate::keymap::Context::Conversation, "next_action")
            ));
            return Vec::new();
        };
        if message.has_spoilers() && !self.spoilers_revealed(&message) {
            self.status_message =
                Some("Reveal the message's spoilers before opening links".to_owned());
            return Vec::new();
        }
        let links = &message.links;
        let selected = self
            .message_actions(&message)
            .get(self.selected_action)
            .and_then(|action| match action {
                MessageAction::Link(index) => links.get(*index),
                _ => None,
            })
            .or_else(|| links.first());
        let Some(link) = selected else {
            self.status_message = Some("Selected message has no link".to_owned());
            return Vec::new();
        };
        self.activate_url(&link.url)
    }

    fn activate_url(&mut self, url: &str) -> Vec<TelegramCommand> {
        if let Some(hash) = crate::invites::hash(url) {
            return self.begin_invite(hash);
        }
        if telegram_link(url).is_some() {
            self.pending_telegram_link = Some(url.to_owned());
            self.status_message = Some("Opening Telegram link…".to_owned());
            return vec![TelegramCommand::ResolveTelegramLink {
                url: url.to_owned(),
            }];
        }
        match open_external_url(url) {
            Ok(()) => self.status_message = Some("Opened link in your browser".to_owned()),
            Err(error) => {
                self.status_message = Some(format!(
                    "Could not open link: {}",
                    sanitize_terminal_line(&error.to_string())
                ));
            }
        }
        Vec::new()
    }

    fn navigate_to_selected_reply(&mut self) -> Vec<TelegramCommand> {
        let Some(chat_id) = self.active_chat_id else {
            return Vec::new();
        };
        let Some(message) = self
            .selected_message
            .and_then(|message_id| {
                self.messages
                    .get(&chat_id)
                    .and_then(|messages| messages.iter().find(|message| message.id == message_id))
            })
            .cloned()
        else {
            self.status_message = Some(format!(
                "Select a reply with {} first",
                self.keymap
                    .hint(crate::keymap::Context::Conversation, "next_action")
            ));
            return Vec::new();
        };
        let Some(reply) = message.reply_to else {
            self.status_message = Some("Selected message is not a reply".to_owned());
            return Vec::new();
        };
        if self.reply_is_unavailable(&reply) {
            self.status_message = Some("Original message is no longer available".to_owned());
            return Vec::new();
        }
        if let Some(target) = self.reply_message(&reply).cloned() {
            if reply.chat_id == chat_id {
                upsert_message_preserving(
                    self.messages.entry(chat_id).or_default(),
                    target,
                    reply.message_id,
                );
                self.focus_reply_target(reply.message_id);
                return Vec::new();
            }
            if let Some(chat) = self
                .chats
                .iter()
                .find(|candidate| candidate.id == reply.chat_id)
                .cloned()
            {
                return self.open_resolved_link(chat, Some(target));
            }
        }
        if self.active_reply_request.is_some_and(|request| {
            request.source_chat == chat_id
                && request.source_message == message.id
                && request.target_chat == reply.chat_id
                && request.target_message == reply.message_id
        }) {
            self.status_message = Some(format!("Loading reply #{}…", reply.message_id));
            return Vec::new();
        }
        let request_id = self.next_history_request_id;
        self.next_history_request_id = self.next_history_request_id.wrapping_add(1).max(1);
        self.active_reply_request = Some(ReplyRequest {
            source_chat: chat_id,
            source_message: message.id,
            target_chat: reply.chat_id,
            target_message: reply.message_id,
            request_id,
        });
        self.status_message = Some(format!("Loading reply #{}…", reply.message_id));
        vec![TelegramCommand::LoadMessage {
            chat_id,
            source_message_id: message.id,
            message_id: reply.message_id,
            request_id,
        }]
    }

    fn focus_reply_target(&mut self, message_id: i32) {
        self.clear_unread_navigation();
        self.selected_message = Some(message_id);
        self.selected_action = 0;
        self.viewport_anchor_message = Some(message_id);
        self.viewport_anchor_row = 0;
        // A non-zero detached state instructs the renderer to honor the
        // semantic anchor instead of snapping back to the latest message.
        self.message_scroll = 1;
        self.status_message = Some(format!("Reply target #{message_id}"));
    }

    fn open_resolved_link(
        &mut self,
        mut chat: Chat,
        message: Option<Message>,
    ) -> Vec<TelegramCommand> {
        self.pending_telegram_link = None;
        let chat_id = chat.id;
        let selected_message = message.as_ref().map(|message| message.id);
        let mut known_chat = self.chats.iter().position(|chat| chat.id == chat_id);
        chat.title = sanitize_terminal_line(&chat.title);
        chat.last_message = sanitize_terminal_text(&chat.last_message);
        if let Some(index) = known_chat {
            // A username lookup has no dialog counters or ordering. Preserve
            // those server-owned fields instead of replacing them with zeros.
            self.chats[index].title = chat.title;
        } else {
            self.chats.push(chat);
            known_chat = Some(self.chats.len() - 1);
            self.linked_chat_ids.insert(chat_id);
        }
        if let Some(index) = known_chat {
            self.folder_id = i32::from(self.chats[index].membership.archived);
            self.filter.clear();
            self.preserve_chat_selection(Some(chat_id));
        }
        // Pin the destination before insertion so the bounded global cache
        // cannot evict an older exact link/reply target during navigation.
        self.active_chat_id = Some(chat_id);
        self.selected_message = selected_message;
        self.selected_action = 0;
        if let Some(message) = message {
            self.update_message(message);
        }
        self.focus = Focus::Conversation;
        self.narrow_conversation = true;
        self.mode = Mode::Navigate;
        self.loading_history = true;
        self.message_scroll = 0;
        self.new_messages_while_scrolled = 0;
        self.new_messages_to_anchor = 0;
        self.viewport_anchor_message = selected_message;
        self.viewport_anchor_row = 0;
        if selected_message.is_some() {
            self.message_scroll = 1;
        }
        let request_id = self.next_history_request_id;
        self.next_history_request_id = self.next_history_request_id.wrapping_add(1).max(1);
        self.active_history_request = Some((chat_id, request_id));
        self.older_motion = None;
        self.browsing_older = false;
        self.history_changes.clear();
        self.history_read_max = 0;
        self.history_target_message = selected_message;
        self.status_message = None;
        self.remember_chat(chat_id);
        self.prepare_unread_entry(chat_id, false);
        vec![TelegramCommand::LoadHistory {
            chat_id,
            request_id,
            after_id: None,
        }]
    }

    fn open_selected_chat(&mut self) -> Vec<TelegramCommand> {
        let Some(chat_id) = self.selected_chat_entry().map(|chat| chat.id) else {
            return Vec::new();
        };
        self.pending_telegram_link = None;
        self.active_chat_id = Some(chat_id);
        self.completion = None;
        self.remember_chat(chat_id);
        self.selected_message = None;
        self.focus = Focus::Conversation;
        self.narrow_conversation = true;
        self.loading_history = true;
        self.message_scroll = 0;
        self.new_messages_while_scrolled = 0;
        self.new_messages_to_anchor = 0;
        self.clear_viewport_anchor();
        let request_id = self.next_history_request_id;
        self.next_history_request_id = self.next_history_request_id.wrapping_add(1).max(1);
        self.active_history_request = Some((chat_id, request_id));
        self.older_motion = None;
        self.browsing_older = false;
        self.history_changes.clear();
        self.history_read_max = 0;
        self.history_target_message = None;
        let after_id = self.prepare_unread_entry(chat_id, true);
        self.position_unread_entry(chat_id, false);
        vec![TelegramCommand::LoadHistory {
            chat_id,
            request_id,
            after_id,
        }]
    }

    fn move_up(&mut self, amount: usize) -> Vec<TelegramCommand> {
        if self.focus == Focus::Chats {
            self.move_chat_up(amount)
        } else {
            self.selected_message = None;
            self.clear_viewport_anchor();
            self.message_scroll = self.message_scroll.saturating_add(amount);
            Vec::new()
        }
    }

    fn move_down(&mut self, amount: usize) -> Vec<TelegramCommand> {
        if self.focus == Focus::Chats {
            self.move_chat_down(amount)
        } else {
            self.selected_message = None;
            self.clear_viewport_anchor();
            self.message_scroll = self.message_scroll.saturating_sub(amount);
            if self.message_scroll == 0 {
                return self.reach_bottom();
            }
            Vec::new()
        }
    }

    fn move_chat_up(&mut self, amount: usize) -> Vec<TelegramCommand> {
        self.selected_chat = self.selected_chat.saturating_sub(amount);
        Vec::new()
    }

    fn move_chat_down(&mut self, amount: usize) -> Vec<TelegramCommand> {
        let length = self.filtered_chat_indices().len();
        self.selected_chat = self
            .selected_chat
            .saturating_add(amount)
            .min(length.saturating_sub(1));
        Vec::new()
    }

    fn move_to_start(&mut self) -> Vec<TelegramCommand> {
        if self.focus == Focus::Chats {
            self.selected_chat = 0;
        } else {
            self.selected_message = None;
            self.clear_viewport_anchor();
            self.message_scroll = usize::MAX;
        }
        Vec::new()
    }

    fn move_to_end(&mut self) -> Vec<TelegramCommand> {
        if self.focus == Focus::Chats {
            self.selected_chat = self.filtered_chat_indices().len().saturating_sub(1);
            Vec::new()
        } else {
            self.selected_message = None;
            self.clear_viewport_anchor();
            self.message_scroll = 0;
            self.reach_bottom()
        }
    }

    fn reach_bottom(&mut self) -> Vec<TelegramCommand> {
        self.new_messages_while_scrolled = 0;
        self.new_messages_to_anchor = 0;
        self.clear_viewport_anchor();
        self.next_unread_page()
    }

    fn clamp_chat_selection(&mut self) {
        let length = self.filtered_chat_indices().len();
        self.selected_chat = self.selected_chat.min(length.saturating_sub(1));
    }

    /// Overlay events received after a snapshot request. A slower history RPC
    /// must not resurrect deleted messages or undo a live edit/read receipt.
    fn track_snapshot_changes(&mut self, event: &NetworkEvent) {
        let history_chat = self
            .active_history_request
            .map(|(chat_id, _)| chat_id)
            .or_else(|| self.older_motion.map(|(chat_id, _, _)| chat_id));
        let history_chat = self.pin_context_chat().or(history_chat);
        match event {
            NetworkEvent::NewMessage(message)
            | NetworkEvent::MessageUpdated(message)
            | NetworkEvent::MessageSent { message, .. } => {
                if history_chat == Some(message.chat_id) {
                    self.history_changes
                        .insert(message.id, Some(message.clone()));
                }
                if self.refresh_dialogs_pending {
                    self.changed_dialogs.insert(message.chat_id);
                }
            }
            NetworkEvent::MessagesDeleted {
                channel_id,
                message_ids,
            } => {
                if let Some(chat_id) = history_chat
                    && channel_id.map_or(chat_id > -1_000_000_000_000, |id| id == chat_id)
                {
                    for id in message_ids {
                        self.history_changes.insert(*id, None);
                    }
                }
            }
            NetworkEvent::MessagesRead { chat_id, max_id } if history_chat == Some(*chat_id) => {
                self.history_read_max = self.history_read_max.max(*max_id);
            }
            NetworkEvent::ReadMarked { chat_id, .. }
            | NetworkEvent::ChatUnreadChanged { chat_id, .. }
            | NetworkEvent::ChatUnreadFinished { chat_id, .. }
            | NetworkEvent::UnreadChanged { chat_id, .. }
                if self.refresh_dialogs_pending =>
            {
                self.changed_dialogs.insert(*chat_id);
            }
            _ => {}
        }
    }

    fn replace_dialogs(&mut self, mut chats: Vec<Chat>) {
        let selected_id = self.selected_chat_entry().map(|chat| chat.id);
        let transient_linked = self
            .chats
            .iter()
            .filter(|chat| {
                self.linked_chat_ids.contains(&chat.id)
                    && !chats.iter().any(|replacement| replacement.id == chat.id)
            })
            .cloned()
            .collect::<Vec<_>>();
        chats.extend(transient_linked);
        for chat in &mut chats {
            if self.changed_dialogs.contains(&chat.id) {
                if let Some(current) = self.chats.iter().find(|current| current.id == chat.id) {
                    chat.last_message.clone_from(&current.last_message);
                    chat.last_activity = current.last_activity;
                    chat.last_message_id = current.last_message_id;
                    chat.unread = current.unread;
                    chat.membership.unread_mark = current.membership.unread_mark;
                    chat.read_inbox_max_id = chat.read_inbox_max_id.max(current.read_inbox_max_id);
                } else if let Some(message) = self
                    .messages
                    .get(&chat.id)
                    .and_then(|messages| messages.last())
                    && chat
                        .last_activity
                        .is_none_or(|timestamp| message.timestamp >= timestamp)
                {
                    chat.last_message = message.preview_text();
                    chat.last_message_id = Some(message.id);
                    chat.last_activity = Some(message.timestamp);
                }
            }
        }
        for current in &self.chats {
            if self.changed_dialogs.contains(&current.id)
                && !chats.iter().any(|chat| chat.id == current.id)
            {
                chats.push(current.clone());
            }
        }
        if !self.changed_dialogs.is_empty() {
            chats.sort_by_key(|chat| std::cmp::Reverse(chat.last_activity));
        }
        self.changed_dialogs.clear();
        self.linked_chat_ids
            .retain(|chat_id| chats.iter().any(|chat| chat.id == *chat_id));
        for chat in &mut chats {
            chat.title = sanitize_terminal_line(&chat.title);
            chat.last_message = sanitize_terminal_text(&chat.last_message);
        }
        self.chats = chats;
        self.selected_chat = selected_id
            .and_then(|id| {
                self.filtered_chat_indices()
                    .iter()
                    .position(|&index| self.chats[index].id == id)
            })
            .unwrap_or(0);
        if self
            .active_chat_id
            .is_some_and(|id| !self.chats.iter().any(|chat| chat.id == id))
        {
            self.active_chat_id = None;
            self.selected_message = None;
            self.message_hit_regions.clear();
            self.focus = Focus::Chats;
            self.narrow_conversation = false;
            self.message_scroll = 0;
            self.new_messages_while_scrolled = 0;
            self.new_messages_to_anchor = 0;
            self.clear_viewport_anchor();
            self.active_history_request = None;
            self.active_reply_request = None;
            self.history_target_message = None;
            self.loading_history = false;
            self.mode = Mode::Navigate;
        }
        self.prune_message_cache();
        self.clamp_chat_selection();
    }

    fn receive_message(
        &mut self,
        mut message: Message,
        count_as_new: bool,
        reconcile_accepted: bool,
    ) -> Vec<TelegramCommand> {
        sanitize_message(&mut message);
        let chat_id = message.chat_id;
        let outgoing = message.outgoing;
        let preview = message.preview_text();
        let text = message.text.clone();
        let reply_to = message.reply_to.as_ref().map(|reply| reply.message_id);
        let timestamp = message.timestamp;
        let active = self.active_chat_id == Some(chat_id);
        let outside_forward_page = active
            && self
                .reads
                .forward
                .is_some_and(|(chat, after)| chat == chat_id && message.id > after);
        let detached = active && (self.message_scroll > 0 || outside_forward_page);
        let message_id = message.id;
        let was_new = self
            .messages
            .get(&chat_id)
            .is_none_or(|messages| !messages.iter().any(|current| current.id == message_id));
        let was_new = was_new
            && (!outside_forward_page
                || self
                    .active_chat()
                    .and_then(|chat| chat.last_message_id)
                    .is_none_or(|id| message_id > id));
        let reconciled_local_id = (reconcile_accepted && was_new && message_id > 0 && outgoing)
            .then(|| {
                self.messages
                    .get_mut(&chat_id)
                    .and_then(|messages| remove_matching_optimistic(messages, &message))
            })
            .flatten();
        if let Some(local_id) = reconciled_local_id {
            self.clear_reconciled_retry(chat_id, local_id, &text, reply_to);
        }
        let retained = if outside_forward_page {
            // Keep a contiguous history window while reading forward. The
            // coordinator already persisted the arrival for the later page.
            true
        } else {
            let messages = self.messages.entry(chat_id).or_default();
            if let Some(preserved_id) = active.then_some(self.selected_message).flatten() {
                upsert_message_preserving(messages, message, preserved_id);
            } else {
                upsert_message(messages, message);
            }
            messages.iter().any(|current| current.id == message_id)
        };
        let genuinely_new = was_new && retained && reconciled_local_id.is_none();
        self.prune_message_cache();
        if genuinely_new && count_as_new && detached {
            self.new_messages_while_scrolled = self.new_messages_while_scrolled.saturating_add(1);
            if !outside_forward_page {
                self.new_messages_to_anchor = self.new_messages_to_anchor.saturating_add(1);
            }
        }

        let selected_id = self.selected_chat_entry().map(|chat| chat.id);
        let Some(chat_index) = self.chats.iter().position(|chat| chat.id == chat_id) else {
            return self.request_dialog_refresh();
        };
        {
            let chat = &mut self.chats[chat_index];
            if message_id < 0 || chat.last_message_id.is_none_or(|last| message_id >= last) {
                chat.last_message = preview;
                if message_id > 0 {
                    chat.last_message_id = Some(message_id);
                }
                chat.last_activity = Some(timestamp);
            }
            if genuinely_new
                && count_as_new
                && !outgoing
                && chat.read_inbox_max_id.is_none_or(|seen| message_id > seen)
            {
                chat.unread = chat.unread.saturating_add(1);
            }
        }
        let chat = self.chats.remove(chat_index);
        self.chats.insert(0, chat);
        self.selected_chat = selected_id
            .and_then(|id| {
                self.filtered_chat_indices()
                    .iter()
                    .position(|&index| self.chats[index].id == id)
            })
            .unwrap_or(0);
        self.clamp_chat_selection();

        Vec::new()
    }

    fn confirm_message(&mut self, local_id: i32, message: Message) -> Vec<TelegramCommand> {
        let chat_id = message.chat_id;
        let attachment_send = message.attachment.is_some()
            || self
                .messages
                .get(&chat_id)
                .and_then(|messages| messages.iter().find(|current| current.id == local_id))
                .is_some_and(|current| current.attachment.is_some())
            || self.retry_attachments.contains_key(&(chat_id, local_id));
        self.retry_attachments.remove(&(chat_id, local_id));
        let replaced_pending = self.messages.get_mut(&chat_id).is_some_and(|messages| {
            let original_len = messages.len();
            messages.retain(|current| current.id != local_id);
            messages.len() != original_len
        });
        let commands = self.receive_message(message, !replaced_pending, false);
        if attachment_send && self.selected_message == Some(local_id) {
            self.selected_message = None;
        }
        if attachment_send && self.active_chat_id == Some(chat_id) {
            self.status_message = None;
        }
        commands
    }

    fn finish_history(
        &mut self,
        chat_id: ChatId,
        request_id: u64,
        result: Result<Vec<Message>, String>,
    ) -> Vec<TelegramCommand> {
        if self.active_history_request != Some((chat_id, request_id)) {
            return Vec::new();
        }
        match result {
            Ok(messages) => {
                let mut messages = messages;
                let forward = self.reads.paging
                    || self
                        .reads
                        .entry
                        .as_ref()
                        .is_some_and(|entry| entry.chat_id == chat_id && !entry.confirmed);
                if forward {
                    let last = messages
                        .iter()
                        .filter(|message| message.id > 0)
                        .map(|message| message.id)
                        .max();
                    self.reads.forward = last
                        .filter(|last| {
                            self.active_chat()
                                .and_then(|chat| chat.last_message_id)
                                .is_some_and(|top| top > *last)
                                && messages.len() >= crate::telegram::HISTORY_LIMIT
                        })
                        .map(|id| (chat_id, id));
                    self.browsing_older = self.reads.forward.is_some();
                }
                messages.retain(|message| !self.history_changes.contains_key(&message.id));
                messages.extend(
                    self.history_changes
                        .values()
                        .flatten()
                        .filter(|message| {
                            self.reads.forward.is_none_or(|(_, end)| message.id <= end)
                        })
                        .cloned(),
                );
                for message in &mut messages {
                    if message.outgoing && message.id > 0 && message.id <= self.history_read_max {
                        message.delivery = Delivery::Read;
                    }
                }
                if self.reads.paging {
                    let mut combined = self.active_messages().to_vec();
                    combined.retain(|message| {
                        !self
                            .history_changes
                            .get(&message.id)
                            .is_some_and(Option::is_none)
                    });
                    for message in messages {
                        upsert_message(&mut combined, message);
                    }
                    self.merge_history(chat_id, combined, self.history_target_message);
                } else {
                    self.merge_history(chat_id, messages, self.history_target_message);
                    self.position_unread_entry(chat_id, true);
                }
                self.status_message = None;
            }
            Err(error) => self.status_message = Some(sanitize_terminal_line(&error)),
        }
        self.active_history_request = None;
        self.history_changes.clear();
        self.reads.paging = false;
        self.history_target_message = None;
        self.loading_history = false;
        Vec::new()
    }

    fn finish_reply_navigation(
        &mut self,
        chat_id: ChatId,
        message_id: i32,
        request_id: u64,
        result: Result<Message, String>,
    ) -> Vec<TelegramCommand> {
        let Some(request) = self.active_reply_request else {
            return Vec::new();
        };
        if request.source_chat != chat_id
            || request.target_message != message_id
            || request.request_id != request_id
        {
            return Vec::new();
        }
        let source_still_matches = self.reply_source_matches(request);
        if !source_still_matches {
            self.active_reply_request = None;
            if self
                .status_message
                .as_deref()
                .is_some_and(|status| status.starts_with("Loading reply #"))
            {
                self.status_message =
                    Some("Reply target changed before loading completed".to_owned());
            }
            return Vec::new();
        }
        if self.active_chat_id != Some(request.source_chat)
            || self.selected_message != Some(request.source_message)
            || self.mode != Mode::Navigate
        {
            self.active_reply_request = None;
            if self
                .status_message
                .as_deref()
                .is_some_and(|status| status.starts_with("Loading reply #"))
            {
                self.status_message = None;
            }
            return Vec::new();
        }
        self.active_reply_request = None;
        if self.reply_is_unavailable(&ReplyInfo {
            chat_id: request.target_chat,
            message_id: request.target_message,
            sender: None,
        }) {
            self.status_message = Some("Original message is no longer available".to_owned());
            return Vec::new();
        }
        match result {
            Ok(mut message)
                if message.id == request.target_message
                    && message.chat_id == request.target_chat =>
            {
                sanitize_message(&mut message);
                let sender = message.sender.clone();
                if let Some(messages) = self.messages.get_mut(&request.source_chat) {
                    for cached in messages.iter_mut() {
                        if let Some(reply) = &mut cached.reply_to
                            && reply.message_id == request.target_message
                            && reply.chat_id == message.chat_id
                            && reply.sender.is_none()
                        {
                            reply.sender = Some(sender.clone());
                        }
                    }
                }
                if message.chat_id == request.source_chat {
                    if let Some(messages) = self.messages.get_mut(&request.source_chat) {
                        upsert_message_preserving(messages, message, request.target_message);
                    } else {
                        self.messages.insert(request.source_chat, vec![message]);
                    }
                } else if let Some(chat) = self
                    .chats
                    .iter()
                    .find(|candidate| candidate.id == message.chat_id)
                    .cloned()
                {
                    return self.open_resolved_link(chat, Some(message));
                } else {
                    self.status_message = Some(
                        "Reply target belongs to a conversation that is not loaded".to_owned(),
                    );
                    return Vec::new();
                }
                if self.active_chat_id == Some(request.source_chat) {
                    self.focus_reply_target(request.target_message);
                }
                self.prune_message_cache();
            }
            Ok(_) => {
                self.status_message = Some("Telegram returned the wrong reply target".to_owned());
            }
            Err(error) => {
                self.status_message = Some(format!(
                    "Could not load reply #{}: {}",
                    request.target_message,
                    sanitize_terminal_line(&error)
                ));
            }
        }
        Vec::new()
    }

    fn reply_source_matches(&self, request: ReplyRequest) -> bool {
        self.messages
            .get(&request.source_chat)
            .and_then(|messages| {
                messages
                    .iter()
                    .find(|message| message.id == request.source_message)
            })
            .and_then(|message| message.reply_to.as_ref())
            .is_some_and(|reply| {
                reply.chat_id == request.target_chat && reply.message_id == request.target_message
            })
    }

    fn fail_message(
        &mut self,
        chat_id: ChatId,
        local_id: i32,
        text: &str,
        reply_to: Option<i32>,
        error: &str,
    ) -> Vec<TelegramCommand> {
        let reply_to = self
            .messages
            .get(&chat_id)
            .and_then(|messages| messages.iter().find(|message| message.id == local_id))
            .and_then(|message| message.reply_to.clone())
            .or_else(|| reply_to.and_then(|id| self.reply_info_for_target(chat_id, id)));
        let failed_visible = self
            .messages
            .get_mut(&chat_id)
            .and_then(|messages| messages.iter_mut().find(|message| message.id == local_id))
            .is_some_and(|pending| {
                pending.delivery = Delivery::Failed;
                true
            });
        // A matching outgoing update may have arrived before an ambiguous
        // transport error. Do not restore a stale draft in that case.
        if !failed_visible {
            return Vec::new();
        }
        let has_new_reply = self
            .draft_data(chat_id)
            .is_some_and(|draft| draft.reply.is_some());
        let draft = &mut self.draft_data_mut(chat_id).input;
        let retry_available = draft.is_empty() && !has_new_reply;
        if retry_available {
            draft.set_value(sanitize_terminal_text(text));
            self.retry_message_ids.insert(chat_id, local_id);
            if let Some(reply_to) = reply_to {
                self.draft_data_mut(chat_id).reply = Some(reply_to);
            }
        }
        let error = sanitize_terminal_line(error);
        let active = self.active_chat_id == Some(chat_id);
        let chat_title = self
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .map_or_else(|| "conversation".to_owned(), |chat| chat.title.clone());
        self.status_message = Some(if retry_available && active {
            format!("Message not sent: {error} · Enter retries")
        } else if active {
            format!("Message not sent: {error}")
        } else {
            format!("Message to {chat_title} not sent: {error}")
        });
        Vec::new()
    }

    fn accept_message(&mut self, chat_id: ChatId, local_id: i32) {
        let attachment_send = self
            .messages
            .get(&chat_id)
            .and_then(|messages| messages.iter().find(|message| message.id == local_id))
            .is_some_and(|message| message.attachment.is_some())
            || self.retry_attachments.contains_key(&(chat_id, local_id));
        self.retry_attachments.remove(&(chat_id, local_id));
        if let Some(message) = self
            .messages
            .get_mut(&chat_id)
            .and_then(|messages| messages.iter_mut().find(|message| message.id == local_id))
        {
            message.delivery = Delivery::Sent;
        }
        if attachment_send && self.selected_message == Some(local_id) {
            self.selected_message = None;
        }
        if attachment_send && self.active_chat_id == Some(chat_id) {
            self.status_message = None;
        }
    }

    fn update_message(&mut self, mut message: Message) {
        sanitize_message(&mut message);
        let chat_id = message.chat_id;
        let message_id = message.id;
        let media_id = message
            .attachment
            .as_ref()
            .and_then(|attachment| attachment.source_id);
        let same_media = media_id.is_some()
            && self
                .messages
                .get(&chat_id)
                .and_then(|messages| messages.iter().find(|current| current.id == message_id))
                .and_then(|current| current.attachment.as_ref())
                .and_then(|attachment| attachment.source_id)
                == media_id;
        if !same_media {
            self.media_previews.remove(&(chat_id, message_id));
            self.downloaded_attachments.remove(&(chat_id, message_id));
            if self
                .downloading_attachments
                .remove(&(chat_id, message_id))
                .is_some()
            {
                self.status_message = Some(format!(
                    "Attachment changed · {} to try again",
                    self.keymap
                        .hint(crate::keymap::Context::Conversation, "reveal")
                ));
            }
            self.reveal_after_download.remove(&(chat_id, message_id));
        }
        let text = message.text.clone();
        let preview = message.preview_text();
        let timestamp = message.timestamp;
        let reply_to = message.reply_to.as_ref().map(|reply| reply.message_id);
        let reconciled_local_id = {
            let messages = self.messages.entry(chat_id).or_default();
            let was_new = !messages.iter().any(|current| current.id == message_id);
            (was_new && message_id > 0 && message.outgoing)
                .then(|| remove_matching_optimistic(messages, &message))
                .flatten()
        };
        if let Some(local_id) = reconciled_local_id {
            self.clear_reconciled_retry(chat_id, local_id, &text, reply_to);
        }
        let preserved = (self.active_chat_id == Some(chat_id))
            .then_some(self.selected_message)
            .flatten();
        if !self
            .reads
            .forward
            .is_some_and(|(chat, end)| chat == chat_id && message_id > end)
        {
            let messages = self.messages.entry(chat_id).or_default();
            if let Some(preserved_id) = preserved {
                upsert_message_preserving(messages, message, preserved_id);
            } else {
                upsert_message(messages, message);
            }
        }
        if let Some(chat) = self.chats.iter_mut().find(|chat| chat.id == chat_id)
            && chat.last_message_id.map_or_else(
                || {
                    chat.last_activity
                        .is_none_or(|activity| timestamp >= activity)
                },
                |id| message_id == id,
            )
        {
            chat.last_message = preview;
        }
        self.prune_message_cache();
    }

    fn merge_history(
        &mut self,
        chat_id: ChatId,
        messages: Vec<Message>,
        preserved_message: Option<i32>,
    ) {
        let mut local = self
            .messages
            .get(&chat_id)
            .into_iter()
            .flatten()
            .filter(|message| message.id < 0)
            .cloned()
            .collect::<Vec<_>>();
        let preserved = preserved_message.and_then(|message_id| {
            self.messages
                .get(&chat_id)
                .and_then(|messages| messages.iter().find(|message| message.id == message_id))
                .cloned()
        });
        let mut combined = normalized_messages(messages);
        let mut reconciled = Vec::new();
        for server_message in combined.iter().filter(|message| message.outgoing) {
            if let Some(local_id) = remove_matching_optimistic(&mut local, server_message) {
                reconciled.push((
                    local_id,
                    server_message.text.clone(),
                    server_message
                        .reply_to
                        .as_ref()
                        .map(|reply| reply.message_id),
                ));
            }
        }
        for pending in local {
            upsert_message(&mut combined, pending);
        }
        if let Some(preserved) = preserved {
            let preserved_id = preserved.id;
            upsert_message_preserving(&mut combined, preserved, preserved_id);
        }
        self.messages.insert(chat_id, combined);
        for (local_id, text, reply_to) in reconciled {
            self.clear_reconciled_retry(chat_id, local_id, &text, reply_to);
        }
        self.prune_message_cache();
    }

    fn clear_reconciled_retry(
        &mut self,
        chat_id: ChatId,
        local_id: i32,
        text: &str,
        reply_to: Option<i32>,
    ) {
        let attachment_retry = self.retry_attachments.remove(&(chat_id, local_id));
        if let Some(retry) = &attachment_retry {
            retry.attachment.discard_owned();
        }
        let attachment_retry = attachment_retry.is_some();
        if self.selected_message == Some(local_id) {
            self.selected_message = None;
        }
        let attachment_status = self.status_message.as_deref().is_some_and(|status| {
            status.starts_with("Sending ")
                || status.starts_with("Retrying attachment")
                || status.starts_with("Attachment not sent")
        });
        if self.active_chat_id == Some(chat_id) && (attachment_retry || attachment_status) {
            self.status_message = None;
        }
        if self.retry_message_ids.get(&chat_id) == Some(&local_id) {
            self.retry_message_ids.remove(&chat_id);
            if self
                .draft_for(chat_id)
                .is_some_and(|draft| draft.value() == text)
            {
                self.draft_data_mut(chat_id).input.clear();
                if self
                    .draft_data(chat_id)
                    .and_then(|draft| draft.reply.as_ref())
                    .map(|reply| reply.message_id)
                    == reply_to
                {
                    self.draft_data_mut(chat_id).reply.take();
                }
            }
            if self.active_chat_id == Some(chat_id) {
                self.status_message = None;
            }
        }
    }

    fn request_dialog_refresh(&mut self) -> Vec<TelegramCommand> {
        if self.refresh_dialogs_pending {
            Vec::new()
        } else {
            self.refresh_dialogs_pending = true;
            vec![TelegramCommand::RefreshDialogs]
        }
    }

    fn clear_viewport_anchor(&mut self) {
        self.viewport_anchor_message = None;
        self.viewport_anchor_row = 0;
    }

    fn prune_message_cache(&mut self) {
        self.downloaded_attachments
            .retain(|&(chat_id, message_id), _| {
                self.messages
                    .get(&chat_id)
                    .is_some_and(|messages| messages.iter().any(|message| message.id == message_id))
            });
        self.downloading_attachments
            .retain(|&(chat_id, message_id), _| {
                self.messages
                    .get(&chat_id)
                    .is_some_and(|messages| messages.iter().any(|message| message.id == message_id))
            });
        self.reveal_after_download
            .retain(|key| self.downloading_attachments.contains_key(key));
        if self.messages.len() <= MAX_CACHED_CHATS {
            return;
        }
        let active = self.active_chat_id;
        let mut keep = self
            .chats
            .iter()
            .take(MAX_CACHED_CHATS)
            .map(|chat| chat.id)
            .collect::<BTreeSet<_>>();
        if let Some(chat_id) = active {
            keep.insert(chat_id);
        }
        keep.extend(self.retry_message_ids.keys().copied());
        keep.extend(self.retry_attachments.keys().map(|(chat_id, _)| *chat_id));
        keep.extend(self.messages.iter().filter_map(|(&chat_id, messages)| {
            messages
                .iter()
                .any(|message| message.id < 0)
                .then_some(chat_id)
        }));
        self.messages.retain(|chat_id, _| keep.contains(chat_id));
    }
}

fn sanitize_message(message: &mut Message) {
    message.sender = sanitize_terminal_line(&message.sender);
    message.sender_username = message
        .sender_username
        .take()
        .map(|name| sanitize_terminal_line(&name))
        .filter(|name| !name.is_empty());
    message.text = sanitize_terminal_text(&message.text);
    if let Some(reply) = &mut message.reply_to {
        reply.sender = reply
            .sender
            .take()
            .map(|sender| sanitize_terminal_line(&sender));
    }
    if let Some(attachment) = &mut message.attachment {
        attachment.file_name = attachment
            .file_name
            .take()
            .map(|name| sanitize_terminal_line(&name));
        attachment.mime_type = attachment
            .mime_type
            .take()
            .map(|mime| sanitize_terminal_line(&mime));
        attachment.fallback_emoji = attachment
            .fallback_emoji
            .take()
            .map(|emoji| sanitize_terminal_line(&emoji));
    }
    for link in &mut message.links {
        link.label = sanitize_terminal_line(&link.label);
        link.url = sanitize_terminal_line(&link.url);
    }
    for button in &mut message.buttons {
        button.label = sanitize_terminal_line(&button.label);
    }
    for inferred in plain_message_links(&message.text) {
        if !message.links.iter().any(|link| link.url == inferred.url) {
            message.links.push(inferred);
        }
    }
}

fn plain_message_links(text: &str) -> Vec<MessageLink> {
    text.split_whitespace()
        .filter_map(|word| {
            let label = word.trim_matches(|character: char| {
                matches!(character, '<' | '>' | '(' | ')' | '[' | ']' | '"' | '\'')
            });
            let label = label.trim_end_matches(|character: char| {
                matches!(character, '.' | ',' | ';' | ':' | '!' | '?')
            });
            if label.is_empty() || label.len() > 8 * 1024 || label.chars().any(char::is_control) {
                return None;
            }
            let lower = label.to_ascii_lowercase();
            let url = if lower.starts_with("https://")
                || lower.starts_with("http://")
                || lower.starts_with("tg://")
            {
                label.to_owned()
            } else if lower.starts_with("t.me/")
                || lower.starts_with("www.t.me/")
                || lower.starts_with("telegram.me/")
                || lower.starts_with("www.telegram.me/")
            {
                format!("https://{label}")
            } else {
                return None;
            };
            Some(MessageLink {
                label: label.to_owned(),
                url,
            })
        })
        .take(32)
        .collect()
}

/// Extract the first internal Telegram chat/message link without treating
/// arbitrary URLs as commands or shell input.
#[must_use]
pub fn telegram_link(text: &str) -> Option<String> {
    text.split_whitespace().find_map(|word| {
        let candidate = word.trim_matches(|character: char| {
            matches!(
                character,
                '(' | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '<'
                    | '>'
                    | ','
                    | '.'
                    | '!'
                    | '?'
                    | ';'
                    | ':'
                    | '\''
                    | '"'
            )
        });
        let lower = candidate.to_ascii_lowercase();
        let normalized = if lower.starts_with("https://t.me/")
            || lower.starts_with("http://t.me/")
            || lower.starts_with("https://telegram.me/")
            || lower.starts_with("http://telegram.me/")
            || lower.starts_with("tg://join?")
            || lower.starts_with("tg://resolve?")
            || lower.starts_with("tg://privatepost?")
        {
            candidate.to_owned()
        } else if lower.starts_with("t.me/")
            || lower.starts_with("www.t.me/")
            || lower.starts_with("telegram.me/")
            || lower.starts_with("www.telegram.me/")
        {
            format!("https://{candidate}")
        } else {
            return None;
        };
        let lower = normalized.to_ascii_lowercase();
        let has_target = if lower.starts_with("tg://") {
            lower.split_once('?').is_some_and(|(_, query)| {
                query.split('&').any(|field| {
                    field.starts_with("domain=")
                        || field.starts_with("channel=")
                        || field.starts_with("invite=")
                })
            })
        } else {
            lower
                .find(".me/")
                .is_some_and(|start| !lower[start + 4..].trim_matches('/').is_empty())
        };
        has_target.then_some(normalized)
    })
}

fn pointer_row_hit<T: Copy>(regions: &[(u16, u16, u16, T)], column: u16, row: u16) -> Option<T> {
    regions
        .iter()
        .rev()
        .find(|&&(start, end, hit_row, _)| hit_row == row && (start..end).contains(&column))
        .map(|&(_, _, _, value)| value)
}

fn pointer_in_region(region: Option<(u16, u16, u16, u16)>, column: u16, row: u16) -> bool {
    region.is_some_and(|(left, right, top, bottom)| {
        (left..right).contains(&column) && (top..bottom).contains(&row)
    })
}

fn attachment_from_path(path: &Path) -> Attachment {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let (kind, mime_type) = match extension.as_str() {
        "jpg" | "jpeg" => (AttachmentKind::Photo, Some("image/jpeg")),
        "png" => (AttachmentKind::Photo, Some("image/png")),
        "webp" => (AttachmentKind::Photo, Some("image/webp")),
        "heic" | "heif" => (AttachmentKind::Photo, Some("image/heic")),
        "mp4" => (AttachmentKind::Video, Some("video/mp4")),
        "mp3" => (AttachmentKind::Audio, Some("audio/mpeg")),
        "ogg" | "oga" => (AttachmentKind::Audio, Some("audio/ogg")),
        "pdf" => (AttachmentKind::File, Some("application/pdf")),
        "txt" => (AttachmentKind::File, Some("text/plain")),
        "zip" => (AttachmentKind::File, Some("application/zip")),
        _ => (AttachmentKind::File, None),
    };
    Attachment {
        source_id: None,
        kind,
        file_name: path
            .file_name()
            .and_then(|name| name.to_str())
            .map(sanitize_terminal_line),
        mime_type: mime_type.map(str::to_owned),
        size: None,
        fallback_emoji: None,
    }
}

fn reveal_path(path: &Path) -> std::io::Result<()> {
    if !path.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "downloaded file no longer exists",
        ));
    }
    let mut child = reveal_path_platform(path)?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(target_os = "macos")]
fn reveal_path_platform(path: &Path) -> std::io::Result<std::process::Child> {
    Command::new("open").arg("-R").arg("--").arg(path).spawn()
}

#[cfg(target_os = "linux")]
fn reveal_path_platform(path: &Path) -> std::io::Result<std::process::Child> {
    let parent = path.parent().unwrap_or(path);
    Command::new("xdg-open").arg(parent).spawn()
}

#[cfg(target_os = "windows")]
fn reveal_path_platform(path: &Path) -> std::io::Result<std::process::Child> {
    let selection = format!("/select,{}", path.display());
    Command::new("explorer.exe").arg(selection).spawn()
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn reveal_path_platform(_path: &Path) -> std::io::Result<std::process::Child> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "revealing files is unsupported on this platform",
    ))
}

fn open_external_url(url: &str) -> std::io::Result<()> {
    let lower = url.to_ascii_lowercase();
    if url.len() > 8 * 1024
        || url.chars().any(char::is_control)
        || url.chars().any(char::is_whitespace)
        || !(lower.starts_with("https://") || lower.starts_with("http://"))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "only HTTP(S) links can be opened",
        ));
    }
    open_external_url_platform(url).map(|_| ())
}

#[cfg(target_os = "macos")]
fn open_external_url_platform(url: &str) -> std::io::Result<std::process::Child> {
    Command::new("open").arg("-u").arg(url).spawn()
}

#[cfg(target_os = "linux")]
fn open_external_url_platform(url: &str) -> std::io::Result<std::process::Child> {
    Command::new("xdg-open").arg(url).spawn()
}

#[cfg(target_os = "windows")]
fn open_external_url_platform(url: &str) -> std::io::Result<std::process::Child> {
    Command::new("explorer.exe").arg(url).spawn()
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn open_external_url_platform(_url: &str) -> std::io::Result<std::process::Child> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "opening links is unsupported on this platform",
    ))
}

fn normalized_messages(mut messages: Vec<Message>) -> Vec<Message> {
    for message in &mut messages {
        sanitize_message(message);
    }
    let mut by_id = BTreeMap::new();
    for message in messages {
        by_id.insert(message.id, message);
    }
    let mut messages = by_id.into_values().collect::<Vec<_>>();
    sort_and_cap_messages(&mut messages);
    messages
}

fn upsert_message(messages: &mut Vec<Message>, message: Message) {
    if let Some(existing) = messages
        .iter_mut()
        .find(|existing| existing.id == message.id)
    {
        *existing = message;
    } else {
        messages.push(message);
    }
    sort_and_cap_messages(messages);
}

fn upsert_message_preserving(messages: &mut Vec<Message>, message: Message, preserved_id: i32) {
    if let Some(existing) = messages
        .iter_mut()
        .find(|existing| existing.id == message.id)
    {
        *existing = message;
    } else {
        messages.push(message);
    }
    messages.sort_by_key(|message| (message.timestamp, message.id));
    while messages.len() > MAX_MESSAGES_PER_CHAT {
        let remove = messages
            .iter()
            .position(|message| message.id != preserved_id)
            .unwrap_or(0);
        messages.remove(remove);
    }
}

fn sort_and_cap_messages(messages: &mut Vec<Message>) {
    messages.sort_by_key(|message| (message.timestamp, message.id));
    let overflow = messages.len().saturating_sub(MAX_MESSAGES_PER_CHAT);
    if overflow > 0 {
        messages.drain(..overflow);
    }
}

fn remove_matching_optimistic(messages: &mut Vec<Message>, server: &Message) -> Option<i32> {
    let matching = messages
        .iter()
        .enumerate()
        .filter(|(_, current)| {
            current.id < 0
                && current.outgoing
                && current.text == server.text
                && current
                    .reply_to
                    .as_ref()
                    .map(|reply| (reply.chat_id, reply.message_id))
                    == server
                        .reply_to
                        .as_ref()
                        .map(|reply| (reply.chat_id, reply.message_id))
                && attachments_match(current.attachment.as_ref(), server.attachment.as_ref())
                && server
                    .timestamp
                    .signed_duration_since(current.timestamp)
                    .num_seconds()
                    .unsigned_abs()
                    <= 300
        })
        .min_by_key(|(_, current)| (current.timestamp, std::cmp::Reverse(current.id)))
        .map(|(index, _)| index);
    matching.map(|index| messages.remove(index).id)
}

fn attachments_match(left: Option<&Attachment>, right: Option<&Attachment>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) if left.kind != right.kind => false,
        (Some(left), Some(_)) if left.kind == AttachmentKind::Photo => true,
        (Some(left), Some(right)) => {
            left.file_name.is_none()
                || right.file_name.is_none()
                || left.file_name == right.file_name
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use chrono::{TimeZone, Utc};
    use yazi_term::event::{KeyEvent, Modifiers, MouseButton, MouseEvent, MouseEventKind};

    use super::{
        App, AttachmentState, AuthPhase, Focus, MAX_CACHED_CHATS, MAX_MESSAGES_PER_CHAT, Mode,
        QrRenderMode, Screen,
    };
    use crate::{
        config::{DownloadBehavior, MAX_ACCOUNTS, ReleaseChannel, Settings},
        event::{AppEvent, AuthPrompt, NetworkEvent, TelegramCommand},
        input::KeyAction,
        model::{
            Attachment, AttachmentKind, Chat, ChatKind, Delivery, Message, MessageButton,
            MessageButtonKind, MessageLink, ReplyInfo,
        },
    };

    fn chat(id: i64, title: &str) -> Chat {
        Chat {
            read_inbox_max_id: Some(0),
            membership: crate::folders::ChatMembership::default(),
            id,
            title: title.to_owned(),
            kind: ChatKind::Direct,
            unread: 3,
            last_message_id: None,
            last_message: format!("from {title}"),
            last_activity: None,
        }
    }

    fn message(id: i32, chat_id: i64, text: &str, outgoing: bool) -> Message {
        Message {
            reactions: None,
            poll: None,
            entities: Vec::new(),
            notification: None,
            mention: None,
            edited_at: None,
            pinned: false,
            id,
            chat_id,
            sender_username: None,
            sender: if outgoing { "Me" } else { "Them" }.to_owned(),
            reply_to: None,
            text: text.to_owned(),
            timestamp: Utc.timestamp_opt(i64::from(id.max(0)), 0).unwrap(),
            outgoing,
            delivery: Delivery::Sent,
            attachment: None,
            links: super::plain_message_links(text),
            buttons: Vec::new(),
        }
    }

    fn ready_app() -> App {
        let mut app = App::new();
        app.handle_network(NetworkEvent::Ready {
            user_name: "Ada".to_owned(),
        });
        app.handle_network(NetworkEvent::Dialogs(vec![
            chat(1, "Alpha"),
            chat(2, "Beta"),
            chat(3, "Gamma"),
        ]));
        app
    }

    fn open_first(app: &mut App) {
        // General interaction fixtures start in an already-read conversation.
        // Unread-entry tests open an explicit server boundary separately.
        app.chats
            .iter_mut()
            .find(|chat| chat.id == 1)
            .unwrap()
            .unread = 0;
        app.handle_action(KeyAction::Enter);
        app.handle_network(NetworkEvent::History {
            chat_id: 1,
            request_id: 1,
            messages: (1..=20)
                .map(|id| message(id, 1, &format!("message {id}"), false))
                .collect(),
        });
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn reactions_use_reviewed_identity_explicit_keys_and_bounded_refresh() {
        let mut app = ready_app();
        open_first(&mut app);
        app.connection = crate::event::ConnectionStatus::Online;
        app.selected_message = Some(20);
        let review = crate::reactions::example();
        app.messages
            .get_mut(&1)
            .unwrap()
            .last_mut()
            .unwrap()
            .reactions = Some(review.summary.clone());
        let commands = app.run_binding("reactions", 1);
        let [TelegramCommand::LoadReactions { request_id, .. }] = commands.as_slice() else {
            panic!("load choices")
        };
        assert_eq!(app.mode, Mode::Reactions);
        assert!(app.run_binding("open", 1).is_empty());
        app.handle_network(NetworkEvent::ReactionsLoaded {
            chat_id: 1,
            message_id: 20,
            request_id: *request_id,
            result: Ok(review.clone()),
        });
        for width in [40, 110] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 24)).unwrap();
            terminal
                .draw(|frame| crate::ui::render(frame, &mut app))
                .unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>();
            assert!(text.contains("Reactions"));
            assert!(text.contains("Love"));
            assert!(text.contains('●'));
        }
        let (x, _, y, _) = app.reactions.hit_rows[1];
        assert!(
            app.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x,
                row: y,
                modifiers: Modifiers::empty()
            })
            .is_empty(),
            "click only selects"
        );
        let commands = app.run_binding("open", 1);
        let [
            TelegramCommand::ChangeReaction {
                request_id,
                expected,
                emoji,
                ..
            },
        ] = commands.as_slice()
        else {
            panic!("explicit toggle")
        };
        assert_eq!(emoji.as_deref(), Some("❤"));
        assert_eq!(*expected, review.summary.chosen());
        assert!(app.run_binding("open", 1).is_empty());
        app.handle_network(NetworkEvent::ReactionsFinished {
            chat_id: 1,
            message_id: Some(20),
            request_id: *request_id,
            error: Some("Timeout".to_owned()),
        });
        assert!(
            app.run_binding("open", 1).is_empty(),
            "refresh before uncertain retry"
        );
        let commands = app.run_binding("refresh", 1);
        let [TelegramCommand::LoadReactions { request_id, .. }] = commands.as_slice() else {
            panic!("refresh")
        };
        app.handle_network(NetworkEvent::ReactionsLoaded {
            chat_id: 1,
            message_id: 20,
            request_id: *request_id,
            result: Ok(review),
        });
        let commands = app.run_binding("clear_reactions", 1);
        let [
            TelegramCommand::ChangeReaction {
                request_id,
                emoji: None,
                ..
            },
        ] = commands.as_slice()
        else {
            panic!("remove mine")
        };
        app.run_binding("cancel", 1);
        assert!(
            app.run_binding("reactions", 1).is_empty(),
            "retain pending write after close"
        );
        app.handle_network(NetworkEvent::ReactionsFinished {
            chat_id: 1,
            message_id: Some(20),
            request_id: *request_id,
            error: Some("not applied".to_owned()),
        });
        assert!(app.status_message.as_ref().unwrap().contains("not applied"));
        let commands = app.request_visible_reactions();
        let [
            TelegramCommand::RefreshReactions {
                chat_id,
                request_id,
                message_ids,
            },
        ] = commands.as_slice()
        else {
            panic!("batch visible reactions")
        };
        assert_eq!(message_ids, &[20]);
        assert!(app.request_visible_reactions().is_empty());
        app.handle_network(NetworkEvent::ReactionsFinished {
            chat_id: *chat_id,
            message_id: None,
            request_id: *request_id,
            error: None,
        });
        app.set_visible_reactions(Vec::new());
        app.set_visible_reactions(vec![(1, 20)]);
        assert!(
            app.request_visible_reactions().is_empty(),
            "redrawing does not bypass the refresh interval"
        );
        app.terminal_focused = false;
        assert!(app.next_reaction_deadline().is_none());
        let commands = app.run_binding("reactions", 1);
        let [TelegramCommand::LoadReactions { request_id, .. }] = commands.as_slice() else {
            panic!("new review")
        };
        app.handle_network(NetworkEvent::MessagesDeleted {
            channel_id: None,
            message_ids: vec![20],
        });
        app.handle_network(NetworkEvent::ReactionsLoaded {
            chat_id: 1,
            message_id: 20,
            request_id: *request_id,
            result: Ok(crate::reactions::example()),
        });
        assert!(!app.reactions.panel.as_ref().unwrap().ready);
        assert!(
            app.reactions
                .panel
                .as_ref()
                .unwrap()
                .error
                .as_ref()
                .unwrap()
                .contains("deleted")
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn sticker_panel_navigates_loads_lazily_and_sends_with_reply() {
        fn sticker(id: i64, emoji: &str) -> crate::model::StickerRef {
            crate::model::StickerRef {
                id,
                access_hash: id.saturating_mul(7),
                file_reference: vec![1, 2, 3],
                emoji: emoji.to_owned(),
                mime_type: "image/webp".to_owned(),
                thumb_size: Some("m".to_owned()),
                set_id: Some(99),
                set_access_hash: Some(3),
            }
        }
        fn section<T>(hash: i64, items: Vec<T>) -> crate::model::StickerSection<T> {
            crate::model::StickerSection { hash, items }
        }
        fn overview(
            recent: Vec<crate::model::StickerRef>,
            sets: Vec<crate::model::StickerSetRef>,
        ) -> crate::model::StickerOverview {
            crate::model::StickerOverview {
                recent: section(11, recent),
                favorites: crate::model::StickerSection::default(),
                sets: section(22, sets),
            }
        }
        let mut app = ready_app();
        open_first(&mut app);
        app.connection = crate::event::ConnectionStatus::Online;
        assert!(
            app.run_binding("stickers", 1).is_empty(),
            "the panel opens from the composer only"
        );
        app.run_binding("compose", 1);
        app.draft_data_mut(1).input.set_value("draft text");
        app.draft_data_mut(1).reply = Some(ReplyInfo {
            message_id: 7,
            chat_id: 1,
            sender: None,
        });
        let commands = app.run_binding("stickers", 1);
        let [TelegramCommand::LoadStickers { request_id, .. }] = commands.as_slice() else {
            panic!("every open revalidates the lists")
        };
        assert_eq!(app.mode, Mode::Stickers);
        let pack = crate::model::StickerSetRef {
            id: 99,
            access_hash: 3,
            title: "Pack".to_owned(),
        };
        // A stale cached event is ignored before this request exists.
        app.handle_network(NetworkEvent::StickersLoaded {
            request_id: request_id.wrapping_add(1),
            validated: false,
            result: Ok(overview(vec![sticker(99, "🧯")], Vec::new())),
        });
        assert_eq!(app.stickers.section_count(), 2);
        // The local cache arrives first and keeps the revalidation pending.
        app.handle_network(NetworkEvent::StickersLoaded {
            request_id: *request_id,
            validated: false,
            result: Ok(overview(
                (1..=12).map(|id| sticker(id, "😀")).collect(),
                vec![pack.clone()],
            )),
        });
        assert_eq!(app.stickers.section_count(), 3);
        assert!(
            app.stickers.panel.as_ref().unwrap().loading.is_some(),
            "the cached payload keeps the refreshing indication"
        );
        // Telegram's validated answer replaces the sections and settles it.
        app.handle_network(NetworkEvent::StickersLoaded {
            request_id: *request_id,
            validated: true,
            result: Ok(overview(
                (1..=12).map(|id| sticker(id, "😀")).collect(),
                vec![pack],
            )),
        });
        assert!(app.stickers.panel.as_ref().unwrap().loading.is_none());
        assert_eq!(app.stickers.section_count(), 3);
        // Grid movement follows the rendered column count.
        let panel = app.stickers.panel.as_mut().unwrap();
        panel.grid_cols = 4;
        panel.grid_rows = 2;
        app.run_binding("down", 1);
        assert_eq!(app.stickers.panel.as_ref().unwrap().selected, 4);
        app.run_binding("up", 1);
        app.run_binding("right", 1);
        assert_eq!(app.stickers.panel.as_ref().unwrap().selected, 1);
        app.run_binding("page_down", 1);
        assert_eq!(app.stickers.panel.as_ref().unwrap().selected, 9);
        app.run_binding("home", 1);
        assert_eq!(app.stickers.panel.as_ref().unwrap().selected, 0);
        // Set sections load lazily on entry, cached documents first.
        app.run_binding("sticker_set_next", 1);
        assert_eq!(app.stickers.panel.as_ref().unwrap().section, 1);
        let commands = app.run_binding("sticker_set_next", 1);
        let [
            TelegramCommand::LoadStickerSet {
                set, request_id, ..
            },
        ] = commands.as_slice()
        else {
            panic!("lazy set load")
        };
        assert_eq!(set.id, 99);
        app.handle_network(NetworkEvent::StickerSetLoaded {
            request_id: *request_id,
            set_id: 99,
            validated: false,
            result: Ok(section(5, vec![sticker(50, "🦊")])),
        });
        app.handle_network(NetworkEvent::StickerSetLoaded {
            request_id: *request_id,
            set_id: 99,
            validated: true,
            result: Ok(section(6, vec![sticker(50, "🦊"), sticker(51, "🐻")])),
        });
        assert!(app.run_binding("sticker_set_previous", 1).is_empty());
        assert!(app.run_binding("sticker_set_previous", 1).is_empty());
        assert_eq!(app.stickers.panel.as_ref().unwrap().section, 0);
        // Render the overlay to expose rows and cells to the mouse.
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(text.contains("Recent"));
        assert!(text.contains("Favorites"));
        assert!(text.contains("Pack"));
        assert!(text.contains("😀"), "cells without thumbnails show emoji");
        // Visible cells queue their thumbnails under a small download budget.
        let commands = app.request_sticker_thumbs();
        assert_eq!(commands.len(), 4);
        assert!(
            commands
                .iter()
                .all(|command| matches!(command, TelegramCommand::DownloadStickerThumb { .. }))
        );
        assert!(
            app.request_sticker_thumbs().is_empty(),
            "the budget caps in-flight downloads"
        );
        app.handle_network(NetworkEvent::StickerThumbDownloaded {
            request_id: 0,
            document_id: 1,
            result: Ok(std::path::PathBuf::from("/tmp/sticker_1.jpg")),
        });
        let commands = app.request_sticker_thumbs();
        assert_eq!(commands.len(), 1, "a finished transfer frees its slot");
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();
        assert_eq!(app.media_slots.len(), 1);
        assert!(
            matches!(
                &app.media_slots[0].source,
                crate::media::MediaSource::File(path) if path == std::path::Path::new("/tmp/sticker_1.jpg")
            ),
            "a ready thumbnail registers an inline image"
        );
        let (x, _, y, section) = app.stickers.hit_rows[1];
        assert_eq!(section, 1);
        assert!(
            app.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x,
                row: y,
                modifiers: Modifiers::empty()
            })
            .is_empty()
        );
        assert_eq!(app.stickers.panel.as_ref().unwrap().section, 1);
        let (x, _, y, _) = app.stickers.hit_rows[0];
        assert!(
            app.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x,
                row: y,
                modifiers: Modifiers::empty()
            })
            .is_empty()
        );
        assert_eq!(app.stickers.panel.as_ref().unwrap().section, 0);
        // A click selects; clicking the selection sends without re-uploading.
        let (left, _, top, _, index) = app.stickers.hit_cells[1];
        assert_eq!(index, 1);
        assert!(
            app.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: left,
                row: top,
                modifiers: Modifiers::empty()
            })
            .is_empty(),
            "first click selects"
        );
        assert_eq!(app.stickers.panel.as_ref().unwrap().selected, 1);
        let commands = app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: left,
            row: top,
            modifiers: Modifiers::empty(),
        });
        let [
            TelegramCommand::SendSticker {
                chat_id,
                local_id,
                sticker: sent,
                reply_to,
            },
        ] = commands.as_slice()
        else {
            panic!("second click sends")
        };
        assert_eq!(
            (*chat_id, *local_id, sent.id, *reply_to),
            (1, -1, 2, Some(7))
        );
        assert_eq!(app.mode, Mode::Compose);
        let pending = app
            .messages
            .get(&1)
            .unwrap()
            .iter()
            .find(|message| message.id == *local_id)
            .unwrap();
        assert_eq!(pending.delivery, Delivery::Pending);
        let attachment = pending.attachment.as_ref().unwrap();
        assert_eq!(attachment.kind, AttachmentKind::Sticker);
        assert_eq!(attachment.fallback_emoji.as_deref(), Some("😀"));
        assert_eq!(pending.reply_to.as_ref().unwrap().message_id, 7);
        assert_eq!(app.draft_data(1).unwrap().input.value(), "draft text");
        assert!(app.draft_data(1).unwrap().reply.is_none());
        // A failed send marks only the optimistic message; the draft is untouched.
        app.handle_network(NetworkEvent::StickerSendFailed {
            chat_id: 1,
            local_id: *local_id,
            error: "Timeout".to_owned(),
        });
        let pending = app
            .messages
            .get(&1)
            .unwrap()
            .iter()
            .find(|message| message.id == *local_id)
            .unwrap();
        assert_eq!(pending.delivery, Delivery::Failed);
        assert!(app.status_message.as_ref().unwrap().contains("Timeout"));
        assert_eq!(app.draft_data(1).unwrap().input.value(), "draft text");
        // Cancel closes back to the composer.
        let commands = app.run_binding("stickers", 1);
        let [TelegramCommand::LoadStickers { .. }] = commands.as_slice() else {
            panic!("reopen revalidates")
        };
        assert_eq!(app.mode, Mode::Stickers);
        app.run_binding("cancel", 1);
        assert_eq!(app.mode, Mode::Compose);
        assert!(app.stickers.panel.is_none());
        // A failed revalidation keeps the cached sections on display.
        let commands = app.run_binding("stickers", 1);
        let [TelegramCommand::LoadStickers { request_id, .. }] = commands.as_slice() else {
            panic!("reopen revalidates")
        };
        app.handle_network(NetworkEvent::StickersLoaded {
            request_id: *request_id,
            validated: false,
            result: Ok(overview(
                vec![sticker(1, "😀"), sticker(2, "🦊")],
                Vec::new(),
            )),
        });
        app.handle_network(NetworkEvent::StickersLoaded {
            request_id: *request_id,
            validated: true,
            result: Err("Timeout".to_owned()),
        });
        let panel = app.stickers.panel.as_ref().unwrap();
        assert!(panel.loading.is_none());
        assert_eq!(panel.error.as_deref(), Some("Timeout"));
        assert_eq!(app.stickers.section_count(), 2);
        match app.stickers.section_view(0) {
            crate::app::stickers::SectionView::Ready(stickers) => {
                assert_eq!(
                    stickers.len(),
                    2,
                    "cached sections survive a failed revalidation"
                );
            }
            _ => panic!("cached sections stay ready"),
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn poll_review_requires_explicit_submission_and_keeps_errors_readable() {
        let mut app = ready_app();
        open_first(&mut app);
        app.connection = crate::event::ConnectionStatus::Online;
        app.selected_message = Some(20);
        let poll = crate::polls::example();
        app.messages.get_mut(&1).unwrap().last_mut().unwrap().poll = Some(poll.clone());
        let commands = app.run_binding("poll", 1);
        let [TelegramCommand::LoadPoll { request_id, .. }] = commands.as_slice() else {
            panic!("refresh before voting")
        };
        assert_eq!(app.mode, Mode::Poll);
        assert!(app.run_binding("send", 1).is_empty());
        app.handle_network(NetworkEvent::PollLoaded {
            chat_id: 1,
            message_id: 20,
            request_id: *request_id,
            result: Ok(poll.clone()),
        });
        for width in [40, 110] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 24)).unwrap();
            terminal
                .draw(|frame| crate::ui::render(frame, &mut app))
                .unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>();
            assert!(text.contains("Choose your drink"));
            assert!(text.contains("anonymous"));
            assert!(!text.contains('%'), "hide results before voting");
        }
        let (x, _, y, _) = app.polls.hit_rows[0];
        assert!(
            app.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x,
                row: y,
                modifiers: Modifiers::empty()
            })
            .is_empty()
        );
        assert!(app.run_binding("down", 1).is_empty());
        assert!(app.run_binding("toggle_poll_answer", 1).is_empty());
        assert_eq!(app.polls.review.as_ref().unwrap().choices.len(), 2);
        let commands = app.run_binding("send", 1);
        let [
            TelegramCommand::VotePoll {
                request_id,
                options,
                revision,
                ..
            },
        ] = commands.as_slice()
        else {
            panic!("explicit send")
        };
        assert_eq!(options, &[vec![0, 255], vec![1, 128]]);
        assert_eq!(*revision, poll.definition.revision());
        assert!(app.run_binding("send", 1).is_empty(), "one pending request");
        let error = "Vote status is uncertain; refresh the poll before retrying";
        app.handle_network(NetworkEvent::PollFinished {
            chat_id: 1,
            message_id: 20,
            request_id: *request_id,
            voting: true,
            error: Some(error.to_owned()),
        });
        assert_eq!(app.polls.review.as_ref().unwrap().choices.len(), 2);
        assert!(
            app.run_binding("send", 1).is_empty(),
            "refresh after uncertain RPC"
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 24)).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(text.contains("Vote status is uncertain"));
        let commands = app.run_binding("refresh", 1);
        let [TelegramCommand::LoadPoll { request_id, .. }] = commands.as_slice() else {
            panic!("refresh")
        };
        let mut voted = poll.clone();
        voted.results.counts.push(crate::polls::Count {
            option: vec![0, 255],
            chosen: true,
            voters: Some(2),
            correct: false,
        });
        app.handle_network(NetworkEvent::PollLoaded {
            chat_id: 1,
            message_id: 20,
            request_id: *request_id,
            result: Ok(voted),
        });
        assert!(app.run_binding("retract_vote", 1).is_empty());
        let commands = app.run_binding("send", 1);
        let [
            TelegramCommand::VotePoll {
                request_id,
                options,
                ..
            },
        ] = commands.as_slice()
        else {
            panic!("explicit retract")
        };
        assert!(options.is_empty());
        app.run_binding("cancel", 1);
        assert_eq!(app.mode, Mode::Navigate);
        assert!(
            app.run_binding("poll", 1).is_empty(),
            "pending vote retained after closing"
        );
        assert!(
            app.handle_network(NetworkEvent::PollFinished {
                chat_id: 1,
                message_id: 20,
                request_id: *request_id,
                voting: true,
                error: None
            })
            .is_empty()
        );
        assert!(app.polls.review.is_none());
        let commands = app.run_binding("poll", 1);
        let [TelegramCommand::LoadPoll { request_id, .. }] = commands.as_slice() else {
            panic!("open again")
        };
        app.handle_network(NetworkEvent::MessagesDeleted {
            channel_id: None,
            message_ids: vec![20],
        });
        app.handle_network(NetworkEvent::PollLoaded {
            chat_id: 1,
            message_id: 20,
            request_id: *request_id,
            result: Ok(poll),
        });
        assert!(!app.polls.review.as_ref().unwrap().ready);
        assert!(
            app.polls
                .review
                .as_ref()
                .unwrap()
                .error
                .as_ref()
                .unwrap()
                .contains("deleted")
        );
    }

    #[test]
    fn poll_refresh_is_bounded_visible_and_focus_aware() {
        let mut app = ready_app();
        open_first(&mut app);
        app.connection = crate::event::ConnectionStatus::Online;
        for message in app.messages.get_mut(&1).unwrap() {
            message.poll = Some(crate::polls::example());
        }
        app.set_visible_polls((1..=20).map(|id| (1, id)).collect());
        let commands = app.request_visible_polls();
        assert_eq!(commands.len(), 2);
        assert!(app.request_visible_polls().is_empty());
        assert!(app.next_poll_deadline().is_none());
        for command in commands {
            let TelegramCommand::RefreshPoll {
                chat_id,
                message_id,
                request_id,
                ..
            } = command
            else {
                unreachable!()
            };
            app.handle_network(NetworkEvent::PollFinished {
                chat_id,
                message_id,
                request_id,
                voting: false,
                error: None,
            });
        }
        app.terminal_focused = false;
        assert!(app.request_visible_polls().is_empty());
        assert!(app.next_poll_deadline().is_none());
        app.terminal_focused = true;
        app.set_visible_polls(Vec::new());
        assert!(app.request_visible_polls().is_empty());
        assert!(app.next_poll_deadline().is_none());
    }

    #[test]
    fn unread_entry_verifies_cached_position_and_pages_without_live_message_gaps() {
        let mut app = ready_app();
        app.chats[0].read_inbox_max_id = Some(10);
        app.chats[0].last_message_id = Some(200);
        app.chats[0].unread = 190;
        assert_eq!(
            app.handle_action(KeyAction::Enter),
            vec![TelegramCommand::LoadHistory {
                chat_id: 1,
                request_id: 1,
                after_id: Some(10),
            }]
        );
        app.handle_network(NetworkEvent::CachedHistory {
            chat_id: 1,
            request_id: 1,
            messages: vec![message(150, 1, "cached gap", false)],
        });
        app.set_visible_read_boundary(1, Some(150));
        assert!(app.request_visible_read().is_empty());
        app.handle_network(NetworkEvent::History {
            chat_id: 1,
            request_id: 1,
            messages: (11..=90).map(|id| message(id, 1, "page", false)).collect(),
        });
        assert_eq!(app.unread_separator(), Some(11));
        assert_eq!(app.viewport_anchor_message, Some(11));
        app.handle_network(NetworkEvent::ReadMarked {
            chat_id: 1,
            max_id: 20,
            snapshot: None,
        });
        assert_eq!(app.unread_separator(), Some(11));
        app.handle_network(NetworkEvent::NewMessage(message(201, 1, "live", false)));
        app.handle_network(NetworkEvent::MessageUpdated(message(
            201,
            1,
            "live edit",
            false,
        )));
        assert!(
            !app.active_messages()
                .iter()
                .any(|message| message.id == 201)
        );
        assert_eq!(app.new_messages_to_anchor, 0);
        app.selected_message = Some(90);
        let next = app.run_binding("message_down", 1);
        assert!(matches!(
            next.as_slice(),
            [TelegramCommand::LoadHistory {
                after_id: Some(90),
                ..
            }]
        ));
        let request_id = app.active_history_request.unwrap().1;
        app.handle_network(NetworkEvent::History {
            chat_id: 1,
            request_id,
            messages: (91..=170).map(|id| message(id, 1, "next", false)).collect(),
        });
        assert!(app.active_messages().iter().any(|message| message.id == 90));
        assert!(
            app.active_messages()
                .iter()
                .any(|message| message.id == 170)
        );
        assert!(
            !app.active_messages()
                .iter()
                .any(|message| message.id == 201)
        );
        assert!(matches!(
            app.run_binding("latest", 1).as_slice(),
            [TelegramCommand::LoadHistory { after_id: None, .. }]
        ));
        assert_eq!(app.unread_separator(), None);
        assert_eq!(app.message_scroll, 0);
    }

    #[test]
    fn unread_reminder_keeps_its_captured_target_and_never_rewinds_receipts() {
        let mut app = ready_app();
        app.chats[0].read_inbox_max_id = Some(20);
        app.chats[0].last_message_id = Some(20);
        app.chats[0].unread = 0;
        app.run_binding("command", 1);
        app.commands.input.set_value("mark-unread");
        app.chats.reverse();
        app.selected_chat = 0;
        let outgoing = app.run_binding("open", 1);
        let [
            TelegramCommand::SetChatUnread {
                chat_id: 1,
                unread: true,
                read_history: false,
                request_id,
            },
        ] = outgoing.as_slice()
        else {
            panic!("captured target")
        };
        let request_id = *request_id;
        assert!(
            !app.chats
                .iter()
                .find(|chat| chat.id == 1)
                .unwrap()
                .membership
                .unread_mark
        );
        app.handle_network(NetworkEvent::ChatUnreadFinished {
            chat_id: 1,
            unread: true,
            request_id,
            snapshot: None,
            error: None,
        });
        let chat = app.chats.iter().find(|chat| chat.id == 1).unwrap();
        assert!(chat.membership.unread_mark);
        assert_eq!(chat.read_inbox_max_id, Some(20));
        let open = app.open_chat_by_id(1);
        let [TelegramCommand::LoadHistory { request_id, .. }] = open.as_slice() else {
            panic!("history request")
        };
        app.handle_network(NetworkEvent::History {
            chat_id: 1,
            request_id: *request_id,
            messages: vec![message(20, 1, "already read", false)],
        });
        app.set_visible_read_boundary(1, Some(20));
        assert!(matches!(
            app.request_visible_read().as_slice(),
            [TelegramCommand::SetChatUnread {
                chat_id: 1,
                unread: false,
                read_history: false,
                ..
            }]
        ));
    }

    #[test]
    fn mute_commands_capture_chat_and_wait_for_authoritative_settings() {
        use crate::notifications::Mute;
        let mut app = ready_app();
        app.run_binding("command", 1);
        app.commands.input.set_value("mute 8h");
        app.chats.reverse();
        app.selected_chat = 0;
        let commands = app.run_binding("open", 1);
        let [
            TelegramCommand::SetChatMute {
                chat_id: 1,
                mute: Mute::EightHours,
                request_id,
            },
        ] = commands.as_slice()
        else {
            panic!("mute original target")
        };
        assert_eq!(
            app.chats
                .iter()
                .find(|chat| chat.id == 1)
                .unwrap()
                .membership
                .mute_until,
            0
        );
        assert!(
            app.set_chat_mute(1, Mute::Off).is_empty(),
            "serialize settings changes for one chat"
        );
        let until = i64::from(Mute::EightHours.until(chrono::Utc::now().timestamp()));
        app.handle_network(NetworkEvent::ChatMuteFinished {
            chat_id: 1,
            request_id: *request_id,
            result: Ok(Some(until)),
        });
        assert_eq!(
            app.chats
                .iter()
                .find(|chat| chat.id == 1)
                .unwrap()
                .membership
                .mute_until,
            until
        );
        app.preserve_chat_selection(Some(1));
        assert!(app.focused_mute_label().unwrap().starts_with("Muted until"));
        app.run_binding("command", 1);
        app.commands.input.set_value("mute unsupported");
        assert!(app.run_binding("open", 1).is_empty());
        assert_eq!(app.mode, Mode::Command);
        app.commands.input.set_value("unmute");
        let commands = app.run_binding("open", 1);
        let [
            TelegramCommand::SetChatMute {
                request_id,
                mute: Mute::Off,
                ..
            },
        ] = commands.as_slice()
        else {
            panic!("unmute")
        };
        app.handle_network(NetworkEvent::ChatMuteFinished {
            chat_id: 1,
            request_id: *request_id,
            result: Err("failed".to_owned()),
        });
        assert_eq!(
            app.chats
                .iter()
                .find(|chat| chat.id == 1)
                .unwrap()
                .membership
                .mute_until,
            until
        );
        let commands = app.run_binding("unmute_chat", 1);
        let [TelegramCommand::SetChatMute { request_id, .. }] = commands.as_slice() else {
            panic!("retry")
        };
        app.handle_network(NetworkEvent::ChatMuteChanged {
            chat_id: 1,
            until: i64::from(i32::MAX),
        });
        app.handle_network(NetworkEvent::ChatMuteFinished {
            chat_id: 1,
            request_id: *request_id,
            result: Ok(None),
        });
        assert_eq!(app.focused_mute_label().as_deref(), Some("Muted forever"));
        app.handle_network(NetworkEvent::ChatMuteChanged {
            chat_id: 1,
            until: 0,
        });
        assert!(app.focused_mute_label().is_none());
        app.connection = crate::event::ConnectionStatus::Offline;
        assert!(app.run_binding("mute_chat", 1).is_empty());
    }

    #[test]
    fn commands_keep_stable_targets_and_never_guess_incomplete_commands() {
        let mut app = ready_app();
        app.account_user_id = Some(42);
        app.run_binding("command", 1);
        app.commands.input.set_value("pin");
        assert!(app.run_binding("open", 1).is_empty());
        assert_eq!(app.mode, Mode::Command);
        assert_eq!(app.commands.input.value(), "pin ");
        app.commands.input.set_value("pin chat");
        app.chats.reverse();
        app.selected_chat = 0;
        assert!(matches!(
            app.run_binding("open", 1).as_slice(),
            [TelegramCommand::ChangeDialogPin {
                chat_id: 1,
                action: crate::pins::DialogAction::Set(true),
                ..
            }]
        ));
        app.pins.dialogs.main = vec![1];
        app.preserve_chat_selection(Some(1));
        app.run_binding("command", 1);
        app.commands.input.set_value("pin chat");
        assert!(app.run_binding("open", 1).is_empty());
        assert_eq!(app.status_message.as_deref(), Some("Already pinned"));
        app.run_binding("command", 1);
        app.commands.input.set_value("qui");
        assert!(app.run_binding("open", 1).is_empty());
        assert!(!app.should_quit);
        app.commands.input.set_value("archive");
        app.account_user_id = Some(43);
        assert!(app.run_binding("open", 1).is_empty());
        assert!(
            app.commands
                .notice
                .as_deref()
                .unwrap()
                .contains("account changed")
        );
        app.run_binding("cancel", 1);
        app.chats
            .iter_mut()
            .find(|chat| chat.id == 2)
            .unwrap()
            .membership
            .archived = true;
        app.run_binding("command", 1);
        app.commands.input.set_value("chat 2");
        app.run_binding("open", 1);
        assert_eq!(app.active_chat_id, Some(2));
        app.browsing_older = true;
        app.run_binding("command", 1);
        app.commands.input.set_value("latest");
        assert!(
            app.run_binding("open", 1)
                .iter()
                .any(|command| matches!(command, TelegramCommand::LoadHistory { chat_id: 2, .. }))
        );
        assert_eq!(app.active_chat_id, Some(2));
    }

    #[test]
    fn command_completion_history_and_paste_preserve_editor_context() {
        use yazi_term::event::KeyCode;
        let mut app = ready_app();
        app.account_user_id = Some(42);
        open_first(&mut app);
        app.message_scroll = 4;
        app.selected_message = Some(8);
        app.draft_data_mut(1).input = crate::input::TextInput::from_value("Unsent draft");
        app.handle_key(&KeyEvent::new(KeyCode::Char(':'), Modifiers::empty()));
        assert_eq!(app.mode, Mode::Command);
        app.commands.input.set_value("sta");
        app.handle_key(&KeyEvent::new(KeyCode::Tab, Modifiers::empty()));
        assert_eq!(app.commands.input.value(), "status");
        app.handle_key(&KeyEvent::new(KeyCode::Char('c'), Modifiers::CONTROL));
        assert_eq!(app.mode, Mode::Navigate);
        assert!(!app.should_quit);
        assert_eq!(app.selected_message, Some(8));
        assert_eq!(app.message_scroll, 4);
        assert_eq!(app.active_draft().unwrap().value(), "Unsent draft");
        app.run_binding("command", 1);
        app.commands.input.set_value("search ");
        app.update(AppEvent::Paste("\\b中文\\s+  ".to_owned()));
        let outgoing = app.run_binding("open", 1);
        assert!(
            matches!(&outgoing[0], TelegramCommand::SearchCached(request) if request.pattern == "\\b中文\\s+  ")
        );
        app.run_binding("cancel", 1);
        app.run_binding("command", 1);
        app.commands.input.set_value("se");
        app.handle_key(&KeyEvent::new(KeyCode::Up, Modifiers::empty()));
        assert_eq!(app.commands.input.value(), "search \\b中文\\s+  ");
        app.handle_key(&KeyEvent::new(KeyCode::Down, Modifiers::empty()));
        assert_eq!(app.commands.input.value(), "se");
        app.run_binding("cancel", 1);
        app.account_user_id = Some(43);
        app.run_binding("command", 1);
        app.handle_key(&KeyEvent::new(KeyCode::Up, Modifiers::empty()));
        assert!(app.commands.input.is_empty());
        app.update(AppEvent::Paste("quit\nquit".to_owned()));
        assert!(!app.should_quit);
        assert!(!app.commands.input.value().contains('\n'));
        app.run_binding("cancel", 1);
        app.run_binding("compose", 1);
        app.handle_key(&KeyEvent::new(KeyCode::Char(':'), Modifiers::empty()));
        assert!(app.active_draft().unwrap().value().ends_with(':'));
    }

    #[test]
    fn restored_drafts_preserve_cursor_reply_and_account_identity() {
        let mut app = ready_app();
        let saved = crate::drafts::Stored {
            edit: None,
            attachments: Vec::new(),
            chat: 1,
            topic: 0,
            text: "a界🙂 old draft".to_owned(),
            cursor: 1,
            reply: Some(ReplyInfo {
                chat_id: 1,
                message_id: 5,
                sender: Some("Original author".to_owned()),
            }),
        };
        app.handle_network(NetworkEvent::LocalDrafts {
            user_id: 100,
            drafts: vec![saved.clone()],
        });
        app.handle_network(NetworkEvent::AccountIdentity { user_id: 100 });
        open_first(&mut app);
        assert_eq!(app.active_draft().unwrap().cursor(), 1);
        assert_eq!(app.active_reply_target().unwrap().message_id, 5);
        app.run_binding("compose", 1);
        app.handle_action(KeyAction::Character('X'));
        assert_eq!(app.active_draft().unwrap().value(), "aX界🙂 old draft");
        app.handle_network(NetworkEvent::CacheInvalidated { chat_id: None });
        assert_eq!(app.active_reply_target().unwrap().message_id, 5);
        app.reset_for_account_switch(2);
        app.handle_network(NetworkEvent::LocalDrafts {
            user_id: 200,
            drafts: vec![crate::drafts::Stored {
                text: "Second account".to_owned(),
                ..saved.clone()
            }],
        });
        app.handle_network(NetworkEvent::AccountIdentity { user_id: 200 });
        assert_eq!(app.draft_for(1).unwrap().value(), "Second account");
        let pending = app.take_draft_snapshot().unwrap();
        assert_eq!(pending[&100][0].text, "aX界🙂 old draft");
        assert!(
            !pending.contains_key(&200),
            "merely loading another account must not rewrite it"
        );
        app.reset_for_account_switch(1);
        app.handle_network(NetworkEvent::LocalDrafts {
            user_id: 100,
            drafts: vec![saved],
        });
        app.handle_network(NetworkEvent::AccountIdentity { user_id: 100 });
        assert_eq!(app.draft_for(1).unwrap().value(), "aX界🙂 old draft");
        assert_eq!(app.draft_for(1).unwrap().cursor(), 2);
        app.draft_data_mut(1).input.clear();
        app.draft_data_mut(1).reply = None;
        assert!(app.take_draft_snapshot().unwrap()[&100].is_empty());
    }

    #[test]
    fn counted_message_motion_continues_across_a_history_page() {
        let mut app = ready_app();
        app.handle_action(KeyAction::Enter);
        app.handle_network(NetworkEvent::History {
            chat_id: 1,
            request_id: 1,
            messages: (81..=160)
                .map(|id| message(id, 1, "wrapped\nmessage", false))
                .collect(),
        });
        app.keymap = crate::keymap::Keymap::parse(
            "return {keymap={{context='conversation',on={'<C-u>'},run='message_up',count=100}}}",
        )
        .unwrap();
        let commands = app.handle_key(&KeyEvent::new(
            yazi_term::event::KeyCode::Char('u'),
            Modifiers::CONTROL,
        ));
        let TelegramCommand::LoadOlder {
            request_id,
            before_id,
            ..
        } = commands[0]
        else {
            panic!("load older page");
        };
        assert_eq!(before_id, 81);
        app.handle_network(NetworkEvent::OlderHistory {
            chat_id: 1,
            request_id,
            before_id,
            messages: (1..=80).map(|id| message(id, 1, "older", false)).collect(),
        });
        assert_eq!(app.selected_message, Some(60));
        assert!(
            app.run_binding("latest", 1)
                .iter()
                .any(|command| matches!(command, TelegramCommand::LoadHistory { .. }))
        );
    }

    #[test]
    fn cached_conversations_are_usable_before_network_authentication() {
        let mut app = App::new();
        app.handle_network(NetworkEvent::CachedSnapshot {
            user_name: Some("Ada".to_owned()),
            chats: vec![chat(1, "Cached")],
        });
        assert_eq!(app.screen, Screen::Main);
        assert_eq!(app.connection, crate::event::ConnectionStatus::Connecting);
        app.handle_action(KeyAction::Enter);
        app.handle_network(NetworkEvent::CachedHistory {
            chat_id: 1,
            request_id: 1,
            messages: vec![message(1, 1, "available offline", false)],
        });
        assert_eq!(app.active_messages()[0].text, "available offline");
        assert!(
            app.loading_history,
            "cached content remains visible during reconciliation"
        );
    }

    #[test]
    fn global_overlay_toggles_restore_the_composer_and_draft() {
        use yazi_term::event::KeyCode;
        let mut app = ready_app();
        open_first(&mut app);
        app.start_composing();
        app.handle_action(KeyAction::Character('x'));
        app.keymap = crate::keymap::Keymap::parse(
            r"return {keymap={
            {context='global',on={'<F4>'},run='help'},
            {context='global',on={'<F5>'},run='settings'},
            {context='global',on={'<F6>'},run='accounts'},
        }}",
        )
        .unwrap();
        for (key, mode) in [(4, Mode::Help), (5, Mode::Settings), (6, Mode::Accounts)] {
            let event = KeyEvent::new(KeyCode::Fn(key), Modifiers::empty());
            app.handle_key(&event);
            assert_eq!(app.mode, mode);
            app.handle_key(&event);
            assert_eq!(app.mode, Mode::Compose);
            assert_eq!(app.active_draft().unwrap().value(), "x");
        }
    }

    #[test]
    fn password_is_masked_and_submitted_without_trimming() {
        let mut app = App::new();
        app.handle_network(NetworkEvent::Auth(AuthPrompt::Password {
            hint: Some("pet".to_owned()),
        }));
        app.handle_action(KeyAction::Character('界'));
        app.handle_action(KeyAction::Character('x'));
        assert_eq!(app.auth_display_value(), "••");
        assert_eq!(app.auth_cursor_display_width(), 2);
        assert_eq!(
            app.handle_action(KeyAction::Enter),
            vec![TelegramCommand::SubmitPassword("界x".to_owned())]
        );
        assert_eq!(
            app.screen,
            Screen::Auth(AuthPhase::Password {
                hint: Some("pet".to_owned())
            })
        );
        assert_eq!(app.auth_progress_label(), Some("Checking 2FA password…"));
        assert!(app.auth_is_submitting());
    }

    #[test]
    fn auth_submissions_show_phase_specific_progress_and_block_duplicate_input() {
        let mut app = App::new();
        app.handle_network(NetworkEvent::Auth(AuthPrompt::Phone));
        app.handle_action(KeyAction::Character('+'));
        app.handle_action(KeyAction::Character('1'));
        assert_eq!(
            app.handle_action(KeyAction::Enter),
            vec![TelegramCommand::SubmitPhone("+1".to_owned())]
        );
        assert_eq!(app.auth_progress_label(), Some("Requesting a login code…"));
        assert!(app.auth_input().is_empty());
        app.handle_action(KeyAction::Character('9'));
        assert!(app.auth_input().is_empty());
        assert!(app.handle_action(KeyAction::Enter).is_empty());

        app.handle_network(NetworkEvent::Auth(AuthPrompt::Code {
            phone: "+1".to_owned(),
        }));
        app.handle_action(KeyAction::Character('1'));
        app.handle_action(KeyAction::Character('2'));
        assert_eq!(
            app.handle_action(KeyAction::Enter),
            vec![TelegramCommand::SubmitCode("12".to_owned())]
        );
        assert_eq!(app.auth_progress_label(), Some("Checking login code…"));

        app.handle_network(NetworkEvent::Error("Incorrect code".to_owned()));
        assert!(!app.auth_is_submitting());
        assert_eq!(app.status_message.as_deref(), Some("Incorrect code"));
        app.handle_action(KeyAction::Character('3'));
        assert_eq!(app.auth_input().value(), "3");
    }

    #[test]
    fn tab_starts_qr_login_and_escape_drops_the_transient_token() {
        use yazi_term::event::{KeyCode, KeyEvent};
        let tab = KeyEvent::new(KeyCode::Tab, Modifiers::empty());
        let back_tab = KeyEvent::new(KeyCode::Tab, Modifiers::SHIFT);
        let escape = KeyEvent::new(KeyCode::Escape, Modifiers::empty());
        let mut app = App::new();
        app.handle_network(NetworkEvent::Auth(AuthPrompt::Phone));
        app.handle_action(KeyAction::Character('+'));
        assert_eq!(app.handle_key(&tab), vec![TelegramCommand::StartQrAuth]);
        assert!(app.auth_input().is_empty());
        assert_eq!(app.auth_progress_label(), Some("Preparing QR sign-in…"));

        let secret_url = "tg://login?token=do-not-print";
        app.handle_network(NetworkEvent::Auth(AuthPrompt::Qr {
            url: secret_url.to_owned(),
        }));
        assert_eq!(
            app.auth_progress_label(),
            Some("Waiting for approval in Telegram…")
        );
        assert!(app.needs_animation());
        assert!(!format!("{:?}", app.screen).contains(secret_url));
        assert_eq!(app.qr_render_mode(), QrRenderMode::Compact);
        assert!(app.handle_key(&tab).is_empty());
        assert_eq!(app.qr_render_mode(), QrRenderMode::Compatible);
        assert!(app.handle_key(&back_tab).is_empty());
        assert_eq!(app.qr_render_mode(), QrRenderMode::Compact);
        assert_eq!(
            app.auth_progress_label(),
            Some("Waiting for approval in Telegram…")
        );
        assert_eq!(app.handle_key(&escape), vec![TelegramCommand::RestartAuth]);
        assert_eq!(app.screen, Screen::Auth(AuthPhase::Phone));
        assert!(app.auth_is_submitting());
        assert_eq!(
            app.auth_progress_label(),
            Some("Returning to phone sign-in…")
        );
        app.handle_network(NetworkEvent::Auth(AuthPrompt::Phone));
        assert!(!app.auth_is_submitting());
    }

    #[test]
    fn qr_start_error_survives_the_worker_returning_to_phone_login() {
        let mut app = App::new();
        app.handle_network(NetworkEvent::Auth(AuthPrompt::Phone));
        app.handle_action(KeyAction::Tab);
        app.handle_network(NetworkEvent::Error(
            "Could not start QR login: unavailable".to_owned(),
        ));
        app.handle_network(NetworkEvent::Auth(AuthPrompt::Phone));

        assert_eq!(app.screen, Screen::Auth(AuthPhase::Phone));
        assert_eq!(
            app.status_message.as_deref(),
            Some("Could not start QR login: unavailable")
        );
        assert!(!app.auth_is_submitting());
        app.handle_action(KeyAction::Character('+'));
        assert_eq!(app.auth_input().value(), "+");

        app.handle_action(KeyAction::Escape);
        assert!(app.status_message.is_none());
    }

    #[test]
    fn auth_prompt_metadata_cannot_inject_terminal_content() {
        let mut app = App::new();
        app.handle_network(NetworkEvent::Auth(AuthPrompt::Code {
            phone: "+81\u{1b}[2J\n90".to_owned(),
        }));
        assert_eq!(
            app.screen,
            Screen::Auth(AuthPhase::Code {
                phone: "+81 90".to_owned()
            })
        );

        app.handle_network(NetworkEvent::Auth(AuthPrompt::Password {
            hint: Some("first\u{1b}[31m\nsecond".to_owned()),
        }));
        assert_eq!(
            app.screen,
            Screen::Auth(AuthPhase::Password {
                hint: Some("first second".to_owned())
            })
        );
    }

    #[test]
    fn auth_credentials_are_redacted_from_debug_output() {
        let secret = "tg://login?token=secret-value";
        let phase = AuthPhase::Qr {
            url: secret.to_owned(),
        };
        assert!(!format!("{phase:?}").contains(secret));
        assert!(format!("{phase:?}").contains("<redacted>"));

        let phase = AuthPhase::Code {
            phone: "+1 555 0100".to_owned(),
        };
        assert!(!format!("{phase:?}").contains("555"));
    }

    #[test]
    fn escape_restarts_code_or_password_auth_but_only_clears_phone_input() {
        let mut app = App::new();
        app.handle_network(NetworkEvent::Auth(AuthPrompt::Code {
            phone: "+81 90".to_owned(),
        }));
        app.handle_action(KeyAction::Character('1'));
        app.status_message = Some("bad code".to_owned());
        assert_eq!(
            app.handle_action(KeyAction::Escape),
            vec![TelegramCommand::RestartAuth]
        );
        assert_eq!(app.screen, Screen::Auth(AuthPhase::Phone));
        assert!(app.auth_input().is_empty());
        assert!(app.status_message.is_none());

        app.handle_network(NetworkEvent::Auth(AuthPrompt::Phone));
        app.handle_action(KeyAction::Character('+'));
        assert!(app.handle_action(KeyAction::Escape).is_empty());
        assert_eq!(app.screen, Screen::Auth(AuthPhase::Phone));
        assert!(app.auth_input().is_empty());
    }

    #[test]
    fn escape_cancels_a_pending_phone_request_before_starting_over() {
        let mut app = App::new();
        app.handle_network(NetworkEvent::Auth(AuthPrompt::Phone));
        app.handle_action(KeyAction::Character('+'));
        app.handle_action(KeyAction::Character('1'));
        app.handle_action(KeyAction::Enter);
        assert!(app.auth_is_submitting());

        assert_eq!(
            app.handle_action(KeyAction::Escape),
            vec![TelegramCommand::RestartAuth]
        );
        assert_eq!(app.screen, Screen::Auth(AuthPhase::Phone));
        assert!(app.auth_is_submitting());
        assert!(app.auth_input().is_empty());

        app.handle_action(KeyAction::Character('9'));
        assert!(app.auth_input().is_empty());
        assert!(app.handle_action(KeyAction::Tab).is_empty());
        assert!(app.handle_action(KeyAction::Enter).is_empty());
        assert_eq!(
            app.auth_progress_label(),
            Some("Returning to phone sign-in…")
        );

        app.handle_network(NetworkEvent::Auth(AuthPrompt::Phone));
        assert!(!app.auth_is_submitting());
        app.handle_action(KeyAction::Character('9'));
        assert_eq!(app.auth_input().value(), "9");
    }

    #[test]
    fn restart_ignores_late_auth_prompts_until_phone_confirmation() {
        let mut app = App::new();
        app.handle_network(NetworkEvent::Auth(AuthPrompt::Phone));
        app.handle_action(KeyAction::Character('+'));
        app.handle_action(KeyAction::Character('1'));
        app.handle_action(KeyAction::Enter);
        app.handle_action(KeyAction::Escape);

        app.handle_network(NetworkEvent::Auth(AuthPrompt::Code {
            phone: "+1".to_owned(),
        }));
        app.handle_network(NetworkEvent::Auth(AuthPrompt::Qr {
            url: "tg://login?token=stale".to_owned(),
        }));
        app.handle_network(NetworkEvent::Error("stale failure".to_owned()));
        assert_eq!(app.screen, Screen::Auth(AuthPhase::Phone));
        assert!(app.status_message.is_none());

        app.handle_network(NetworkEvent::Auth(AuthPrompt::Phone));
        app.handle_network(NetworkEvent::Auth(AuthPrompt::Code {
            phone: "+2".to_owned(),
        }));
        assert_eq!(
            app.screen,
            Screen::Auth(AuthPhase::Code {
                phone: "+2".to_owned()
            })
        );
    }

    #[test]
    fn ready_does_not_duplicate_the_workers_dialog_load() {
        let mut app = App::new();
        assert!(
            app.handle_network(NetworkEvent::Ready {
                user_name: "Ada".to_owned()
            })
            .is_empty()
        );
        assert_eq!(app.screen, Screen::Main);
    }

    #[test]
    fn chat_selection_survives_reordering_by_identity() {
        let mut app = ready_app();
        app.selected_chat = 1;
        app.handle_network(NetworkEvent::Dialogs(vec![
            chat(3, "Gamma"),
            chat(1, "Alpha"),
            chat(2, "Beta"),
        ]));
        assert_eq!(app.selected_chat_entry().map(|chat| chat.id), Some(2));
    }

    #[test]
    fn composer_is_multiline_unicode_aware_and_optimistic() {
        let mut app = ready_app();
        open_first(&mut app);
        app.handle_action(KeyAction::Character('i'));
        app.handle_action(KeyAction::Character('界'));
        app.handle_action(KeyAction::Newline);
        app.handle_action(KeyAction::Character('🙂'));
        let commands = app.handle_action(KeyAction::Enter);
        assert!(matches!(
            commands.as_slice(),
            [TelegramCommand::SendMessage { chat_id: 1, local_id: -1, text, reply_to: None }] if text == "界\n🙂"
        ));
        let pending = app.active_messages().last().unwrap();
        assert_eq!(pending.delivery, Delivery::Pending);
        assert_eq!(app.mode, Mode::Compose);
    }

    #[test]
    fn composer_completion_mentions_commands_and_dismissal() {
        let mut app = ready_app();
        open_first(&mut app);
        app.handle_action(KeyAction::Character('i'));
        // Entering compose prefetches member data once for this chat.
        assert!(app.completion_popup().is_none());
        app.handle_network(NetworkEvent::MembersLoaded {
            chat_id: 1,
            request_id: 1,
            result: Ok(crate::completion::ChatCompletion {
                members: vec![
                    crate::completion::Member {
                        name: "Alice".to_owned(),
                        username: "alice".to_owned(),
                        bot: false,
                    },
                    crate::completion::Member {
                        name: "Robo".to_owned(),
                        username: "robo_bot".to_owned(),
                        bot: true,
                    },
                ],
                commands: vec![crate::completion::BotCommand {
                    bot: Some("robo_bot".to_owned()),
                    command: "roll".to_owned(),
                    description: "Roll a die".to_owned(),
                }],
            }),
        });

        // '@' opens the popup and queues no second fetch.
        assert!(app.handle_action(KeyAction::Character('@')).is_empty());
        assert_eq!(app.completion_popup().unwrap().candidates.len(), 2);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(text.contains("Mention"));
        assert!(text.contains("@alice"));
        assert!(text.contains("@robo_bot"));
        app.handle_action(KeyAction::Character('r'));
        let popup = app.completion_popup().unwrap();
        assert_eq!(popup.candidates.len(), 1);
        assert_eq!(popup.candidates[0].insert, "@robo_bot");
        // Esc only dismisses the popup; the draft and mode are unchanged.
        app.handle_action(KeyAction::Escape);
        assert!(app.completion_popup().is_none());
        assert_eq!(app.mode, Mode::Compose);
        assert_eq!(app.active_draft().unwrap().value(), "@r");
        // A different token reopens completion; Tab applies in place.
        app.handle_action(KeyAction::Backspace);
        app.handle_action(KeyAction::Backspace);
        app.handle_action(KeyAction::Character('@'));
        app.run_binding("complete_next", 1);
        assert_eq!(app.active_draft().unwrap().value(), "@alice");
        app.run_binding("complete_next", 1);
        assert_eq!(app.active_draft().unwrap().value(), "@robo_bot");
        // Enter accepts the highlighted row instead of sending.
        assert!(app.handle_action(KeyAction::Enter).is_empty());
        assert_eq!(app.active_draft().unwrap().value(), "@robo_bot ");
        assert!(app.completion_popup().is_none());

        // '/' at the start offers bot commands; Enter inserts and closes.
        app.draft_data_mut(1).input.clear();
        app.handle_action(KeyAction::Character('/'));
        app.handle_action(KeyAction::Character('r'));
        let popup = app.completion_popup().unwrap();
        assert_eq!(popup.candidates[0].insert, "/roll");
        assert!(app.handle_action(KeyAction::Enter).is_empty());
        assert_eq!(app.active_draft().unwrap().value(), "/roll ");
        // With no token and no popup, Enter sends normally again.
        let commands = app.handle_action(KeyAction::Enter);
        assert!(matches!(
            commands.as_slice(),
            [TelegramCommand::SendMessage { text, .. }] if text == "/roll "
        ));
    }

    #[test]
    fn associated_text_uses_layout_text_without_executing_shortcuts() {
        use yazi_term::event::{KeyCode, KeyEvent, KeyEventKind};

        let mut app = ready_app();
        open_first(&mut app);
        app.handle_action(KeyAction::Character('i'));
        let mut key = KeyEvent {
            code: KeyCode::Char('q'),
            modifiers: Modifiers::CONTROL | Modifiers::ALT,
            text: "你好🙂".into(),
            ..KeyEvent::default()
        };
        assert!(app.handle_key(&key).is_empty());
        assert_eq!(app.active_draft().unwrap().value(), "你好🙂");
        assert!(!app.should_quit);
        key.kind = KeyEventKind::Release;
        assert!(app.handle_key(&key).is_empty());
        assert_eq!(app.active_draft().unwrap().value(), "你好🙂");
    }

    #[test]
    fn message_cursor_composes_a_native_reply_and_keeps_optimistic_context() {
        let mut app = ready_app();
        open_first(&mut app);

        app.handle_action(KeyAction::Character('['));
        assert_eq!(app.selected_message, Some(20));
        app.handle_action(KeyAction::Character('['));
        assert_eq!(app.selected_message, Some(19));
        app.handle_action(KeyAction::Character(']'));
        assert_eq!(app.selected_message, Some(20));

        app.handle_action(KeyAction::Character('R'));
        assert_eq!(app.mode, Mode::Compose);
        assert_eq!(
            app.active_reply_target().map(|reply| reply.message_id),
            Some(20)
        );
        app.handle_action(KeyAction::Character('h'));
        app.handle_action(KeyAction::Character('i'));
        let commands = app.handle_action(KeyAction::Enter);
        assert_eq!(
            commands.last(),
            Some(&TelegramCommand::SendMessage {
                chat_id: 1,
                local_id: -1,
                text: "hi".to_owned(),
                reply_to: Some(20),
            })
        );
        let pending = app.active_messages().last().expect("optimistic reply");
        assert_eq!(
            pending.reply_to.as_ref().map(|reply| reply.message_id),
            Some(20)
        );
        assert!(app.active_reply_target().is_none());
    }

    #[test]
    fn failed_reply_restores_its_draft_and_exact_target_for_retry() {
        let mut app = ready_app();
        open_first(&mut app);
        app.handle_action(KeyAction::Character('R'));
        app.handle_action(KeyAction::Character('x'));
        app.handle_action(KeyAction::Enter);

        app.handle_network(NetworkEvent::SendFailed {
            chat_id: 1,
            local_id: -1,
            text: "x".to_owned(),
            reply_to: Some(20),
            error: "offline".to_owned(),
        });
        assert_eq!(app.active_draft().map(TextInput::value), Some("x"));
        assert_eq!(
            app.active_reply_target().map(|reply| reply.message_id),
            Some(20)
        );
        assert!(matches!(
            app.handle_action(KeyAction::Enter).last(),
            Some(TelegramCommand::SendMessage {
                local_id: -2,
                reply_to: Some(20),
                ..
            })
        ));
    }

    #[test]
    fn escape_cancels_reply_context_before_leaving_the_composer() {
        let mut app = ready_app();
        open_first(&mut app);
        app.handle_action(KeyAction::Character('R'));
        app.handle_action(KeyAction::Character('x'));

        app.handle_action(KeyAction::Escape);
        assert_eq!(app.mode, Mode::Compose);
        assert!(app.active_reply_target().is_none());
        assert_eq!(app.active_draft().map(TextInput::value), Some("x"));
        assert_eq!(
            app.status_message.as_deref(),
            Some("Reply cancelled · draft kept")
        );

        app.handle_action(KeyAction::Escape);
        assert_eq!(app.mode, Mode::Navigate);
        app.handle_action(KeyAction::Character('i'));
        assert_eq!(app.mode, Mode::Compose);
        assert_eq!(app.active_draft().map(TextInput::value), Some("x"));
    }

    #[test]
    fn failed_send_is_visible_and_restored_without_overwriting_newer_draft() {
        let mut app = ready_app();
        open_first(&mut app);
        app.handle_action(KeyAction::Character('i'));
        app.handle_action(KeyAction::Character('x'));
        app.handle_action(KeyAction::Enter);
        app.handle_action(KeyAction::Character('n'));
        app.handle_network(NetworkEvent::SendFailed {
            chat_id: 1,
            local_id: -1,
            text: "x".to_owned(),
            reply_to: None,
            error: "offline".to_owned(),
        });
        assert_eq!(
            app.active_messages().last().unwrap().delivery,
            Delivery::Failed
        );
        assert_eq!(app.active_draft().map(TextInput::value), Some("n"));
        assert!(app.status_message.as_deref().unwrap().contains("offline"));
    }

    #[test]
    fn retry_replaces_the_failed_timeline_entry() {
        let mut app = ready_app();
        open_first(&mut app);
        app.handle_action(KeyAction::Character('i'));
        app.handle_action(KeyAction::Character('x'));
        app.handle_action(KeyAction::Enter);
        app.handle_network(NetworkEvent::SendFailed {
            chat_id: 1,
            local_id: -1,
            text: "x".to_owned(),
            reply_to: None,
            error: "offline".to_owned(),
        });
        assert_eq!(app.active_draft().map(TextInput::value), Some("x"));

        let commands = app.handle_action(KeyAction::Enter);
        assert!(matches!(
            commands.last(),
            Some(TelegramCommand::SendMessage { local_id: -2, .. })
        ));
        assert!(!app.active_messages().iter().any(|message| message.id == -1));
        assert_eq!(app.active_messages().last().unwrap().id, -2);
    }

    #[test]
    fn history_refresh_preserves_pending_messages() {
        let mut app = ready_app();
        open_first(&mut app);
        app.handle_action(KeyAction::Character('i'));
        app.handle_action(KeyAction::Character('x'));
        app.handle_action(KeyAction::Enter);
        let request_id = 2;
        app.active_history_request = Some((1, request_id));
        app.handle_network(NetworkEvent::History {
            chat_id: 1,
            request_id,
            messages: vec![message(1, 1, "server", false)],
        });
        assert!(app.active_messages().iter().any(|message| message.id == -1));
    }

    #[test]
    fn history_snapshot_cannot_undo_live_messages_edits_deletions_or_reads() {
        let mut app = ready_app();
        app.handle_action(KeyAction::Enter);
        app.handle_network(NetworkEvent::MessageUpdated(message(1, 1, "edited", false)));
        app.handle_network(NetworkEvent::MessagesDeleted {
            channel_id: None,
            message_ids: vec![2],
        });
        app.handle_network(NetworkEvent::NewMessage(message(
            4,
            1,
            "arrived during request",
            false,
        )));
        app.handle_network(NetworkEvent::MessagesRead {
            chat_id: 1,
            max_id: 3,
        });
        app.handle_network(NetworkEvent::History {
            chat_id: 1,
            request_id: 1,
            messages: vec![
                message(1, 1, "old", false),
                message(2, 1, "deleted", false),
                message(3, 1, "sent", true),
            ],
        });
        let messages = app.active_messages();
        assert_eq!(
            messages
                .iter()
                .map(|message| message.id)
                .collect::<Vec<_>>(),
            vec![1, 3, 4]
        );
        assert_eq!(messages[0].text, "edited");
        assert_eq!(messages[1].delivery, Delivery::Read);
        assert_eq!(messages[2].text, "arrived during request");
    }

    #[test]
    fn dialog_snapshot_cannot_undo_activity_or_reads_during_refresh() {
        let mut app = ready_app();
        app.handle_network(NetworkEvent::DialogsLoading);
        app.handle_network(NetworkEvent::NewMessage(message(
            25,
            2,
            "live preview",
            false,
        )));
        let unread = app.chats.iter().find(|chat| chat.id == 2).unwrap().unread;
        app.handle_network(NetworkEvent::Dialogs(vec![
            chat(1, "Renamed"),
            chat(2, "Beta"),
        ]));
        assert_eq!(app.chats[0].id, 2);
        assert_eq!(app.chats[0].last_message, "live preview");
        assert_eq!(app.chats[0].unread, unread);
        assert_eq!(app.chats[1].title, "Renamed");
    }

    #[test]
    fn histories_and_live_updates_keep_only_the_lightweight_history_window() {
        let mut app = ready_app();
        app.handle_action(KeyAction::Enter);
        app.handle_network(NetworkEvent::History {
            chat_id: 1,
            request_id: 1,
            messages: (1..=600)
                .map(|id| message(id, 1, &format!("message {id}"), false))
                .collect(),
        });
        let history = app.messages.get(&1).expect("bounded history");
        assert_eq!(history.len(), MAX_MESSAGES_PER_CHAT);
        assert_eq!(history.first().map(|message| message.id), Some(441));
        assert_eq!(history.last().map(|message| message.id), Some(600));

        app.handle_network(NetworkEvent::NewMessage(message(601, 1, "latest", false)));
        let history = app.messages.get(&1).expect("bounded history");
        assert_eq!(history.len(), MAX_MESSAGES_PER_CHAT);
        assert_eq!(history.first().map(|message| message.id), Some(442));
        assert_eq!(history.last().map(|message| message.id), Some(601));
    }

    #[test]
    fn message_cache_evicts_inactive_chats_but_keeps_the_active_one() {
        let mut app = ready_app();
        app.chats = (1_i64..=14)
            .map(|id| chat(id, &format!("Chat {id}")))
            .collect();
        app.active_chat_id = Some(14);
        for chat_id in 1_i64..=14 {
            app.handle_network(NetworkEvent::MessageUpdated(message(
                i32::try_from(chat_id).expect("small chat id"),
                chat_id,
                "cached",
                false,
            )));
        }

        assert_eq!(app.messages.len(), MAX_CACHED_CHATS + 1);
        assert!(app.messages.contains_key(&14));
        assert!(!app.messages.contains_key(&13));
    }

    #[test]
    fn detached_history_never_jumps_and_defers_read_ack() {
        let mut app = ready_app();
        open_first(&mut app);
        app.handle_network(NetworkEvent::ReadMarked {
            chat_id: 1,
            max_id: 20,
            snapshot: None,
        });
        app.handle_action(KeyAction::PageUp);
        assert_eq!(app.message_scroll, 10);
        assert!(
            app.handle_network(NetworkEvent::NewMessage(message(21, 1, "new", false)))
                .is_empty()
        );
        assert_eq!(app.message_scroll, 10);
        assert_eq!(app.new_messages_while_scrolled, 1);
        assert_eq!(app.new_messages_to_anchor, 1);
        assert!(app.active_chat().unwrap().unread > 0);
        assert!(app.handle_action(KeyAction::End).is_empty());
        app.set_visible_read_boundary(1, Some(21));
        assert_eq!(
            app.request_visible_read(),
            vec![TelegramCommand::MarkRead {
                chat_id: 1,
                max_id: 21
            }]
        );
        assert_eq!(app.new_messages_while_scrolled, 0);
        assert_eq!(app.new_messages_to_anchor, 0);
    }

    #[test]
    fn accepted_then_echoed_outgoing_message_reconciles_without_a_new_badge() {
        let mut app = ready_app();
        open_first(&mut app);
        app.handle_action(KeyAction::Character('i'));
        app.handle_action(KeyAction::Character('x'));
        app.handle_action(KeyAction::Enter);
        app.handle_network(NetworkEvent::MessageAccepted {
            chat_id: 1,
            local_id: -1,
        });
        assert_eq!(
            app.active_messages()
                .iter()
                .find(|message| message.id == -1)
                .map(|message| message.delivery),
            Some(Delivery::Sent)
        );

        app.message_scroll = 5;
        let mut echoed = message(21, 1, "x", true);
        echoed.timestamp = Utc::now();
        app.handle_network(NetworkEvent::NewMessage(echoed));
        assert!(!app.active_messages().iter().any(|message| message.id < 0));
        assert!(app.active_messages().iter().any(|message| message.id == 21));
        assert_eq!(app.new_messages_while_scrolled, 0);
        assert_eq!(app.new_messages_to_anchor, 0);
    }

    #[test]
    fn confirming_one_of_two_identical_sends_keeps_the_other_optimistic_entry() {
        let mut app = ready_app();
        open_first(&mut app);
        app.handle_action(KeyAction::Character('i'));
        app.handle_action(KeyAction::Character('x'));
        app.handle_action(KeyAction::Enter);
        app.handle_action(KeyAction::Character('x'));
        app.handle_action(KeyAction::Enter);

        app.handle_network(NetworkEvent::MessageSent {
            local_id: -1,
            message: message(21, 1, "x", true),
        });
        assert!(app.active_messages().iter().any(|message| message.id == 21));
        assert!(app.active_messages().iter().any(|message| message.id == -2));
        assert!(!app.active_messages().iter().any(|message| message.id == -1));
    }

    #[test]
    fn background_focus_defers_read_acknowledgement() {
        let mut app = ready_app();
        open_first(&mut app);
        app.update(AppEvent::TerminalFocus(false));
        assert!(
            app.handle_network(NetworkEvent::NewMessage(message(21, 1, "new", false)))
                .is_empty()
        );
        assert!(app.active_chat().unwrap().unread > 0);
    }

    #[test]
    fn older_read_confirmation_preserves_arrivals_and_coalesces_the_next_receipt() {
        let mut app = ready_app();
        open_first(&mut app);
        app.set_visible_read_boundary(1, Some(20));
        assert_eq!(
            app.request_visible_read(),
            vec![TelegramCommand::MarkRead {
                chat_id: 1,
                max_id: 20,
            }]
        );
        app.handle_network(NetworkEvent::NewMessage(message(21, 1, "unseen", false)));
        let unread = app.active_chat().unwrap().unread;
        app.set_visible_read_boundary(1, Some(21));
        assert!(app.request_visible_read().is_empty());
        app.handle_network(NetworkEvent::ReadMarked {
            chat_id: 1,
            max_id: 20,
            snapshot: Some(crate::read_state::Snapshot {
                max_id: 20,
                unread: 0,
                top_message: 20,
            }),
        });
        assert_eq!(app.active_chat().unwrap().unread, unread);
        assert_eq!(app.active_chat().unwrap().read_inbox_max_id, Some(20));
        assert_eq!(
            app.request_visible_read(),
            vec![TelegramCommand::MarkRead {
                chat_id: 1,
                max_id: 21,
            }]
        );
        app.handle_network(NetworkEvent::ReadMarked {
            chat_id: 1,
            max_id: 21,
            snapshot: None,
        });
        app.handle_network(NetworkEvent::UnreadChanged {
            chat_id: 1,
            max_id: 20,
            unread: 8,
        });
        assert_eq!(app.active_chat().unwrap().unread, 0);
        assert_eq!(app.active_chat().unwrap().read_inbox_max_id, Some(21));
    }

    #[test]
    fn slash_in_conversation_is_a_bot_command_not_a_filter() {
        let mut app = ready_app();
        open_first(&mut app);
        app.handle_action(KeyAction::Character('/'));
        assert_eq!(app.mode, Mode::Compose);
        assert_eq!(app.active_draft().map(TextInput::value), Some("/"));
    }

    #[test]
    fn paste_normalizes_newlines_and_strips_terminal_controls() {
        let mut app = ready_app();
        open_first(&mut app);
        app.handle_action(KeyAction::Character('i'));
        app.update(AppEvent::Paste("a\r\nb\u{1b}[31m!".to_owned()));
        assert_eq!(app.active_draft().map(TextInput::value), Some("a\nb!"));
    }

    #[test]
    fn filtering_and_help_are_contextual() {
        let mut app = ready_app();
        app.handle_action(KeyAction::Character('/'));
        app.handle_action(KeyAction::Character('g'));
        assert_eq!(app.visible_chats()[0].title, "Gamma");
        app.handle_action(KeyAction::Escape);
        app.handle_action(KeyAction::Character('?'));
        assert_eq!(app.mode, Mode::Help);
        app.run_binding("cancel", 1);
        assert_eq!(app.mode, Mode::Navigate);
    }

    #[test]
    fn help_search_uses_effective_bindings_and_restores_the_draft() {
        use yazi_term::event::KeyCode;
        let mut app = ready_app();
        open_first(&mut app);
        app.start_composing();
        app.draft_data_mut(1).input.set_value("unsent draft");
        app.keymap = crate::keymap::Keymap::parse(
            r"return {keymap={{context='conversation',on={'Z'},run='latest',desc='查找[图片]'}}}",
        )
        .unwrap();
        app.run_binding("help", 1);
        app.handle_key(&KeyEvent::new(KeyCode::Char('/'), Modifiers::empty()));
        for character in "q?".chars() {
            app.handle_key(&KeyEvent::new(KeyCode::Char(character), Modifiers::empty()));
        }
        assert_eq!(app.help.query.value(), "q?");
        assert_eq!(app.mode, Mode::Help);
        app.handle_key(&KeyEvent::new(KeyCode::Char('u'), Modifiers::CONTROL));
        app.update(AppEvent::Paste("查找[图片]".to_owned()));
        assert!(
            app.help
                .matcher
                .as_ref()
                .unwrap()
                .is_match("Z — 查找[图片]")
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 12)).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .any(|cell| { cell.symbol() == "图" && cell.fg == ratatui::style::Color::Black })
        );
        app.handle_key(&KeyEvent::new(KeyCode::Enter, Modifiers::empty()));
        assert!(!app.help.editing);
        assert!(!app.help.query.is_empty());
        app.handle_key(&KeyEvent::new(KeyCode::Escape, Modifiers::empty()));
        assert!(app.help.query.is_empty());
        assert_eq!(app.mode, Mode::Help);
        app.handle_key(&KeyEvent::new(KeyCode::Escape, Modifiers::empty()));
        assert_eq!(app.mode, Mode::Compose);
        assert_eq!(app.active_draft().unwrap().value(), "unsent draft");
    }

    #[test]
    fn settings_screen_toggles_and_persists_essential_preferences() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "termgram-app-settings-{}-{nonce}",
            std::process::id()
        ));
        let path = directory.join("settings.conf");
        let mut app = App::with_settings(Settings::default(), path.clone());
        app.screen = Screen::Main;

        app.handle_action(KeyAction::Character('s'));
        assert_eq!(app.mode, Mode::Settings);
        app.handle_action(KeyAction::Enter);
        app.handle_action(KeyAction::Down);
        app.handle_action(KeyAction::Enter);
        app.handle_action(KeyAction::Down);
        app.handle_action(KeyAction::Enter);
        app.handle_action(KeyAction::Down);
        app.handle_action(KeyAction::Enter);
        assert_eq!(
            *app.settings(),
            Settings {
                automatic_update_checks: false,
                release_channel: ReleaseChannel::Prerelease,
                download_behavior: DownloadBehavior::CacheOnly,
                show_message_ids: true,
                ..Settings::default()
            }
        );
        assert_eq!(
            Settings::load_from(&path).expect("persisted settings"),
            *app.settings()
        );

        app.handle_action(KeyAction::Escape);
        assert_eq!(app.mode, Mode::Navigate);
        fs::remove_file(path).expect("remove settings");
        fs::remove_dir(directory).expect("remove settings directory");
    }

    #[test]
    fn account_picker_adds_switches_and_isolates_account_state() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "termgram-app-accounts-{}-{nonce}",
            std::process::id()
        ));
        let path = directory.join("settings.conf");
        let mut app = App::with_settings(Settings::default(), path.clone());
        app.screen = Screen::Main;
        app.user_name = Some("First".to_owned());
        app.chats.push(chat(7, "Private chat"));
        app.messages
            .insert(7, vec![message(1, 7, "account one", false)]);

        assert!(app.handle_action(KeyAction::Character('a')).is_empty());
        assert_eq!(app.mode, Mode::Accounts);
        assert_eq!(app.account_selection(), 0);
        assert!(app.handle_action(KeyAction::Down).is_empty());
        assert_eq!(
            app.handle_action(KeyAction::Enter),
            vec![TelegramCommand::SwitchAccount { account: 2 }]
        );
        assert_eq!(app.active_account(), 2);
        assert_eq!(app.account_count(), 2);
        assert_eq!(app.screen, Screen::Connecting);
        assert!(app.chats.is_empty());
        assert!(app.messages.is_empty());
        assert!(app.user_name.is_none());
        assert_eq!(
            app.status_message.as_deref(),
            Some("Switching to Account 2…")
        );
        assert_eq!(
            Settings::load_from(&path).expect("persisted account selection"),
            *app.settings()
        );

        app.handle_network(NetworkEvent::Ready {
            user_name: "Second".to_owned(),
        });
        assert_eq!(
            app.handle_action(KeyAction::NextAccount),
            vec![TelegramCommand::SwitchAccount { account: 1 }]
        );
        assert_eq!(app.active_account(), 1);

        fs::remove_file(path).expect("remove settings");
        fs::remove_dir(directory).expect("remove settings directory");
    }

    #[test]
    fn account_shortcuts_work_during_login_and_enforce_the_slot_limit() {
        let mut app = App::with_ephemeral_settings(Settings {
            active_account: 1,
            account_count: 2,
            ..Settings::default()
        });
        app.handle_network(NetworkEvent::Auth(AuthPrompt::Phone));
        app.handle_action(KeyAction::Character('+'));
        assert_eq!(
            app.handle_action(KeyAction::NextAccount),
            vec![TelegramCommand::SwitchAccount { account: 2 }]
        );
        assert!(app.auth_input().is_empty());
        assert_eq!(app.screen, Screen::Connecting);

        let mut full = App::with_ephemeral_settings(Settings {
            active_account: MAX_ACCOUNTS,
            account_count: MAX_ACCOUNTS,
            ..Settings::default()
        });
        full.screen = Screen::Fatal("network failed".to_owned());
        assert!(full.handle_action(KeyAction::AddAccount).is_empty());
        assert_eq!(full.active_account(), MAX_ACCOUNTS);
        assert_eq!(
            full.status_message.as_deref(),
            Some("Account limit reached (8)")
        );
    }

    #[test]
    fn update_notification_is_sanitized_and_kept_separate_from_status() {
        let mut app = App::new();
        app.status_message = Some("Message failed".to_owned());
        app.set_available_update(" 0.1.9\u{1b}[31m ");
        assert_eq!(app.available_update(), Some("0.1.9"));
        assert_eq!(app.status_message.as_deref(), Some("Message failed"));
        app.clear_available_update();
        assert_eq!(app.available_update(), None);

        app.settings.automatic_update_checks = false;
        app.set_available_update("0.2.0");
        assert_eq!(app.available_update(), None);
    }

    #[test]
    fn temp_only_downloads_never_reveal_remote_files() {
        let mut app = ready_app();
        app.settings.download_behavior = DownloadBehavior::CacheOnly;
        open_first(&mut app);
        let mut attachment = message(21, 1, "", false);
        attachment.attachment = Some(Attachment {
            source_id: None,
            kind: AttachmentKind::File,
            file_name: Some("remote.command".to_owned()),
            mime_type: None,
            size: Some(1),
            fallback_emoji: None,
        });
        app.messages.get_mut(&1).unwrap().push(attachment);
        let path = std::env::temp_dir().join(format!("termgram-temp-only-{}", std::process::id()));
        fs::write(&path, b"x").expect("download fixture");
        app.downloading_attachments.insert((1, 21), 1);
        app.handle_network(NetworkEvent::AttachmentDownloaded {
            request_id: 1,
            chat_id: 1,
            message_id: 21,
            path: path.clone(),
        });
        app.selected_message = Some(21);

        assert!(app.handle_action(KeyAction::Enter).is_empty());
        assert!(
            app.status_message
                .as_deref()
                .is_some_and(|message| message.contains("reveal is disabled"))
        );
        fs::remove_file(path).expect("remove fixture");
    }

    #[test]
    fn q_is_text_in_an_editor_but_quits_in_navigation() {
        let mut app = ready_app();
        app.handle_action(KeyAction::Character('/'));
        app.handle_action(KeyAction::Character('q'));
        assert_eq!(app.filter.value(), "q");
        app.handle_action(KeyAction::Escape);
        assert_eq!(
            app.handle_action(KeyAction::Character('q')),
            vec![TelegramCommand::Shutdown]
        );
    }

    #[test]
    fn control_l_requests_a_complete_redraw() {
        let mut app = ready_app();
        app.handle_action(KeyAction::Redraw);
        assert!(app.take_force_redraw());
        assert!(!app.take_force_redraw());
    }

    #[test]
    fn fatal_strings_cannot_inject_terminal_sequences() {
        let mut app = ready_app();
        app.handle_network(NetworkEvent::Fatal("bad\u{1b}[2Jnews".to_owned()));
        assert_eq!(app.screen, Screen::Fatal("badnews".to_owned()));
        assert_eq!(
            app.handle_action(KeyAction::Character('q')),
            vec![TelegramCommand::Shutdown]
        );
    }

    #[test]
    fn navigation_opens_chat_and_controls_narrow_view() {
        let mut app = ready_app();
        app.handle_action(KeyAction::Down);
        assert_eq!(
            app.handle_action(KeyAction::Enter),
            vec![TelegramCommand::LoadHistory {
                chat_id: 2,
                request_id: 1,
                after_id: Some(0),
            },]
        );
        assert_eq!(app.focus, Focus::Conversation);
        assert!(app.narrow_conversation);
        app.handle_action(KeyAction::Escape);
        assert_eq!(app.focus, Focus::Chats);
        assert!(!app.narrow_conversation);
    }

    #[test]
    fn tab_keeps_narrow_pane_visibility_in_sync_with_focus() {
        let mut app = ready_app();
        open_first(&mut app);
        assert_eq!(app.focus, Focus::Conversation);
        assert!(app.narrow_conversation);

        app.message_scroll = 3;
        app.handle_action(KeyAction::Tab);
        assert_eq!(app.focus, Focus::Chats);
        assert!(!app.narrow_conversation);
        app.handle_network(NetworkEvent::NewMessage(message(21, 1, "new", false)));
        assert_eq!(app.new_messages_to_anchor, 1);

        app.handle_action(KeyAction::Tab);
        assert_eq!(app.focus, Focus::Conversation);
        assert!(app.narrow_conversation);
    }

    #[test]
    fn clipboard_keys_and_command_prepare_without_sending() {
        use yazi_term::event::{KeyCode, KeyEventKind};
        for modifiers in [
            Modifiers::SUPER,
            Modifiers::CONTROL,
            Modifiers::ALT,
            Modifiers::CONTROL | Modifiers::ALT,
        ] {
            let mut app = ready_app();
            open_first(&mut app);
            let mut key = KeyEvent::new(KeyCode::Char('v'), modifiers);
            let commands = app.handle_key(&key);
            assert!(
                matches!(commands.as_slice(), [TelegramCommand::PrepareAttachments(request)]
                if request.input == crate::staging::Input::Clipboard && request.key.chat == 1)
            );
            assert_eq!(app.mode, Mode::Compose);
            key.kind = KeyEventKind::Repeat;
            assert!(app.handle_key(&key).is_empty());
        }
        let mut app = ready_app();
        open_first(&mut app);
        app.run_binding("command", 1);
        app.commands.input.set_value("paste");
        assert!(matches!(app.run_binding("open", 1).as_slice(),
            [TelegramCommand::PrepareAttachments(request)] if request.input == crate::staging::Input::Clipboard));
    }

    #[test]
    fn pasted_images_require_image_headers_and_keep_the_captured_draft() {
        use image::ImageEncoder;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("screen 图片 name.png");
        image::codecs::png::PngEncoder::new(fs::File::create(&path).unwrap())
            .write_image(&[120; 16], 2, 2, image::ExtendedColorType::Rgba8)
            .unwrap();
        let mut app = ready_app();
        open_first(&mut app);
        app.keymap.attachments.clipboard_as_photo = false;
        let pasted = format!("\"{}\"", path.display());
        let commands = app.update(AppEvent::Paste(pasted.clone()));
        let [TelegramCommand::PrepareAttachments(request)] = commands.as_slice() else {
            panic!("prepare image")
        };
        assert!(
            matches!(&request.input, crate::staging::Input::ImagePaths(value) if value == &pasted)
        );
        // Regular typing/pasting remains available while preparation runs.
        app.update(AppEvent::Paste("caption".to_owned()));
        app.active_chat_id = Some(2);
        let prepared = crate::staging::prepare(request).unwrap();
        assert_eq!(prepared.attachments.len(), 1);
        assert!(!prepared.attachments[0].as_photo);
        app.handle_network(NetworkEvent::AttachmentsPrepared {
            key: request.key,
            request_id: request.id,
            result: Ok(prepared),
        });
        assert_eq!(app.draft_data(1).unwrap().attachments.len(), 1);
        assert_eq!(app.draft_data(1).unwrap().input.value(), "caption");
        assert!(app.draft_attachments().is_empty());
        let invalid = directory.path().join("not-an-image.png");
        fs::write(&invalid, "ordinary file contents").unwrap();
        let fallback = crate::staging::prepare_image_paste(invalid.to_str().unwrap(), request);
        assert_eq!(fallback.text.as_deref(), invalid.to_str());
        assert!(fallback.attachments.is_empty());
        let file_url = url::Url::from_file_path(&path).unwrap().to_string();
        assert_eq!(
            crate::staging::prepare_image_paste(&file_url, request)
                .attachments
                .len(),
            1
        );
        app.keymap.attachments.auto_attach_images = false;
        assert!(app.update(AppEvent::Paste(pasted.clone())).is_empty());
        assert_eq!(app.active_draft().unwrap().value(), pasted);
    }

    #[test]
    fn plain_path_paste_remains_text_even_when_the_file_exists() {
        let mut app = ready_app();
        open_first(&mut app);
        app.mode = Mode::Navigate;
        let file = tempfile::NamedTempFile::new().expect("fixture");
        let text = format!("'{}'", file.path().display());
        assert!(app.update(AppEvent::Paste(text.clone())).is_empty());
        assert_eq!(app.mode, Mode::Compose);
        assert_eq!(app.active_draft().unwrap().value(), text);
        assert!(!app.active_messages().iter().any(|message| message.id < 0));
    }

    fn prepare_file(app: &mut App, path: &std::path::Path) {
        let commands = app.prepare_attachments(path.display().to_string(), false);
        let [TelegramCommand::PrepareAttachments(request)] = commands.as_slice() else {
            panic!("only a preparation request is emitted before confirmation");
        };
        app.handle_network(NetworkEvent::AttachmentsPrepared {
            key: request.key,
            request_id: request.id,
            result: crate::staging::prepare(request).map_err(|error| error.to_string()),
        });
    }

    #[test]
    fn staged_files_preserve_caption_reply_and_source_chat_until_explicit_send() {
        let mut app = ready_app();
        open_first(&mut app);
        app.handle_action(KeyAction::Character('R'));
        app.draft_data_mut(1).input.set_value("caption");
        let file = tempfile::NamedTempFile::new().expect("fixture");
        fs::write(file.path(), b"hello").unwrap();
        let commands = app.prepare_attachments(file.path().display().to_string(), false);
        let [TelegramCommand::PrepareAttachments(request)] = commands.as_slice() else {
            panic!("prepare");
        };
        let prepared = crate::staging::prepare(request).unwrap();
        assert!(
            app.send_draft(1).is_empty(),
            "cannot send while preparation is pending"
        );
        app.active_chat_id = Some(2);
        app.handle_network(NetworkEvent::AttachmentsPrepared {
            key: request.key,
            request_id: request.id,
            result: Ok(prepared),
        });
        assert!(app.draft_attachments().is_empty());
        assert_eq!(app.draft_data(1).unwrap().attachments.len(), 1);
        app.active_chat_id = Some(1);
        assert!(!app.active_messages().iter().any(|message| message.id < 0));
        let commands = app.send_draft(1);
        assert!(
            matches!(commands.as_slice(), [TelegramCommand::SendAttachment {
            chat_id: 1, local_id: -1, caption, reply_to: Some(20), attachment: crate::staging::Attachment { as_photo: false, .. }, ..
        }] if caption == "caption")
        );
        let pending = app.active_messages().last().expect("optimistic attachment");
        assert_eq!(pending.text, "caption");
        assert_eq!(
            pending.reply_to.as_ref().map(|reply| reply.message_id),
            Some(20)
        );
        assert!(app.draft_data(1).unwrap().is_empty());
    }

    #[test]
    fn attachment_review_reuses_preview_and_does_not_send_or_delete_originals() {
        let mut app = ready_app();
        open_first(&mut app);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("picture.png");
        image::RgbImage::new(2, 2).save(&path).unwrap();
        prepare_file(&mut app, &path);
        app.open_attachments();
        for width in [40, 100] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 15)).unwrap();
            terminal
                .draw(|frame| crate::ui::render(frame, &mut app))
                .unwrap();
            assert_eq!(app.attachment_draft.hit_regions.len(), 1);
            assert!(app.run_binding("preview", 1).is_empty());
            terminal
                .draw(|frame| crate::ui::render(frame, &mut app))
                .unwrap();
            assert!(matches!(
                app.media_slots.as_slice(),
                [crate::media::MediaSlot {
                    source: crate::media::MediaSource::File(_),
                    ..
                }]
            ));
            assert!(
                app.request_visible_media().is_empty(),
                "local preview never issues Telegram RPCs"
            );
            app.run_binding("cancel", 1);
        }
        app.run_binding("attachment_format", 1);
        assert!(!app.draft_attachments()[0].as_photo);
        app.run_binding("remove_attachment", 1);
        assert!(app.draft_attachments().is_empty());
        assert!(path.exists());
        assert!(!app.active_messages().iter().any(|message| message.id < 0));
    }

    #[test]
    fn dropped_path_never_sends_from_chat_focus_filter_or_help() {
        let mut app = ready_app();
        open_first(&mut app);
        let path =
            std::env::temp_dir().join(format!("termgram-no-send-{}.txt", std::process::id()));
        fs::write(&path, b"private").expect("create fixture");
        let paste = AppEvent::Paste(path.display().to_string());

        app.focus = Focus::Chats;
        assert!(app.update(paste.clone()).is_empty());
        app.focus = Focus::Conversation;
        app.mode = Mode::Filter;
        assert!(app.update(paste.clone()).is_empty());
        app.mode = Mode::Help;
        assert!(app.update(paste).is_empty());
        assert!(!app.active_messages().iter().any(|message| message.id < 0));
        fs::remove_file(path).expect("remove fixture");
    }

    #[test]
    fn telegram_links_are_resolved_but_ordinary_urls_are_not() {
        assert_eq!(
            super::telegram_link("see https://t.me/rustlang/42."),
            Some("https://t.me/rustlang/42".to_owned())
        );
        assert_eq!(
            super::telegram_link("<tg://resolve?domain=rustlang&post=42>"),
            Some("tg://resolve?domain=rustlang&post=42".to_owned())
        );
        assert_eq!(
            super::telegram_link("telegram.me/rustlang"),
            Some("https://telegram.me/rustlang".to_owned())
        );
        assert_eq!(super::telegram_link("https://example.com/file"), None);
    }

    #[test]
    fn caption_link_has_explicit_activation_separate_from_media() {
        let mut app = ready_app();
        open_first(&mut app);
        let mut photo = message(21, 1, "see https://t.me/rustlang/42", false);
        photo.attachment = Some(Attachment {
            source_id: None,
            kind: AttachmentKind::Photo,
            file_name: Some("photo.jpg".to_owned()),
            mime_type: Some("image/jpeg".to_owned()),
            size: Some(10),
            fallback_emoji: None,
        });
        app.handle_network(NetworkEvent::NewMessage(photo));
        app.selected_message = Some(21);

        assert_eq!(
            app.handle_action(KeyAction::Character('l')),
            vec![TelegramCommand::ResolveTelegramLink {
                url: "https://t.me/rustlang/42".to_owned()
            }]
        );
    }

    #[test]
    fn message_actions_cycle_across_links_and_inline_buttons() {
        let mut app = ready_app();
        open_first(&mut app);
        let mut actionable = message(21, 1, "actions", false);
        actionable.links = vec![MessageLink {
            label: "Rust chat".to_owned(),
            url: "https://t.me/rustlang/42".to_owned(),
        }];
        actionable.buttons = vec![MessageButton {
            label: "Confirm".to_owned(),
            index: 3,
            kind: MessageButtonKind::Callback,
        }];
        app.handle_network(NetworkEvent::NewMessage(actionable));

        app.run_binding("next_action", 1);
        assert_eq!(app.selected_message, Some(21));
        assert_eq!(app.selected_action, 0);
        assert_eq!(
            app.handle_action(KeyAction::Enter),
            vec![TelegramCommand::ResolveTelegramLink {
                url: "https://t.me/rustlang/42".to_owned(),
            }]
        );

        app.run_binding("next_action", 1);
        assert_eq!(app.selected_action, 1);
        assert_eq!(
            app.handle_action(KeyAction::Enter),
            vec![TelegramCommand::ActivateButton {
                chat_id: 1,
                message_id: 21,
                button_index: 3,
            }]
        );
    }

    #[test]
    fn configuration_reload_keeps_state_on_error_and_applies_a_complete_snapshot() {
        let mut app = ready_app();
        open_first(&mut app);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.lua");
        app.configuration.path = Some(path.clone());
        app.draft_data_mut(1).input.set_value("unsent draft");
        app.run_binding("command", 1);
        app.commands.input.set_value("config reload");
        assert!(app.run_binding("open", 1).is_empty());
        assert_eq!(app.take_config_reload(), Some(path.clone()));
        assert!(app.take_config_reload().is_none());
        app.configuration_failed(
            &crate::keymap::Keymap::reload(&path)
                .unwrap_err()
                .to_string(),
        );
        assert_eq!(app.configuration.revision, 0);
        assert!(!app.keymap.nerd_font);
        fs::write(
            &path,
            "return {nerd_font=true, ghost_text='new', statusline={right={'bogus'}}}",
        )
        .unwrap();
        let error = crate::keymap::Keymap::reload(&path).unwrap_err();
        app.configuration_failed(&error.to_string());
        assert_eq!(
            app.keymap.ghost_text,
            "{send} to send · {stickers} for stickers"
        );
        fs::write(&path, "return {nerd_font=true, ghost_text='new', statusline={right={'dc'}}, notifications={enabled=false}}").unwrap();
        app.install_configuration(crate::keymap::Keymap::reload(&path).unwrap());
        assert!(app.keymap.nerd_font);
        assert_eq!(app.keymap.ghost_text, "new");
        assert!(!app.keymap.statusline.measures_latency());
        assert!(!app.keymap.notifications.enabled);
        assert_eq!(app.configuration.revision, 1);
        assert!(app.configuration.error.is_none());
        assert_eq!(app.draft_data(1).unwrap().input.value(), "unsent draft");
        assert_eq!(app.active_chat_id, Some(1));
        assert!(
            app.help_lines()
                .iter()
                .any(|line| line.starts_with(":config reload"))
        );
        for mode in [Mode::Help, Mode::Status] {
            app.mode = mode;
            app.help.scroll = usize::MAX;
            app.configuration.status_scroll = usize::MAX;
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 12)).unwrap();
            terminal
                .draw(|frame| crate::ui::render(frame, &mut app))
                .unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>();
            if mode == Mode::Help {
                assert!(text.contains(":quit"));
            } else {
                assert!(text.contains("build"));
            }
        }
        app.reset_for_account_switch(2);
        assert_eq!(app.configuration.path, Some(path));
        assert_eq!(app.configuration.revision, 1);
    }

    #[test]
    fn chat_information_invalidates_old_permissions_and_preserves_restricted_drafts() {
        use crate::chat_info::{Info, Restriction};
        let mut app = ready_app();
        open_first(&mut app);
        app.connection = crate::event::ConnectionStatus::Online;
        let outgoing = app.request_visible_chat_info();
        let [TelegramCommand::LoadChatInfo { request_id, .. }] = outgoing.as_slice() else {
            panic!("details")
        };
        let info = Info {
            title: "Group".to_owned(),
            role: "Member".to_owned(),
            about: "Long description ".repeat(100),
            restrictions: vec![Restriction {
                content: None,
                reason: "Read only".to_owned(),
                until: 0,
            }],
            ..Info::default()
        };
        app.handle_network(NetworkEvent::ChatInfoInvalidated { chat_id: 1 });
        app.handle_network(NetworkEvent::ChatInfoReady {
            chat_id: 1,
            request_id: *request_id,
            result: Ok(info.clone()),
        });
        assert!(app.chat_info.entries.is_empty());
        let outgoing = app.request_visible_chat_info();
        let [TelegramCommand::LoadChatInfo { request_id, .. }] = outgoing.as_slice() else {
            panic!("refreshed details")
        };
        app.handle_network(NetworkEvent::ChatInfoReady {
            chat_id: 1,
            request_id: *request_id,
            result: Ok(info),
        });
        assert!(app.request_visible_chat_info().is_empty());
        app.start_composing();
        app.draft_data_mut(1).input.set_value("keep my draft");
        assert!(app.send_draft(1).is_empty());
        assert_eq!(app.draft_data(1).unwrap().input.value(), "keep my draft");
        assert!(app.status_message.as_ref().unwrap().contains("Read only"));
        app.mode = Mode::Navigate;
        app.run_binding("command", 1);
        app.commands.input.set_value("info");
        app.run_binding("open", 1);
        assert_eq!(app.mode, Mode::ChatInfo);
        for (width, height) in [(90, 28), (40, 12)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| crate::ui::render(frame, &mut app))
                .unwrap();
            app.run_binding("down", 20);
            terminal
                .draw(|frame| crate::ui::render(frame, &mut app))
                .unwrap();
            assert!(app.chat_info.scroll > 0);
        }
        app.run_binding("cancel", 1);
        assert_eq!(app.mode, Mode::Navigate);
        app.handle_network(NetworkEvent::ChatInfoInvalidated { chat_id: 1 });
        assert_eq!(app.draft_restriction(1), None);
        assert_eq!(app.draft_data(1).unwrap().input.value(), "keep my draft");
    }

    #[test]
    fn invitation_requires_confirmation_and_late_results_do_not_navigate() {
        use crate::invites::{Outcome, Preview};
        let mut app = ready_app();
        open_first(&mut app);
        app.connection = crate::event::ConnectionStatus::Online;
        let preview = Preview {
            title: "Test group".to_owned(),
            about: "A group".to_owned(),
            participants: Some(12),
            request_needed: true,
            warning: None,
            blocked: None,
            joined: None,
        };
        let outgoing = app.activate_url("https://t.me/+test_invite");
        let [TelegramCommand::PreviewInvite { request_id, .. }] = outgoing.as_slice() else {
            panic!("preview only")
        };
        let old = *request_id;
        app.run_binding("cancel", 1);
        app.handle_network(NetworkEvent::InviteReady {
            request_id: old,
            result: Ok(preview.clone()),
        });
        assert_eq!(app.mode, Mode::Navigate);
        assert!(app.invites.preview.is_none());
        app.run_binding("command", 1);
        app.commands
            .input
            .set_value("join tg://join?invite=test_invite");
        let outgoing = app.run_binding("open", 1);
        let [TelegramCommand::PreviewInvite { request_id, .. }] = outgoing.as_slice() else {
            panic!("preview")
        };
        app.handle_network(NetworkEvent::InviteReady {
            request_id: *request_id,
            result: Ok(preview),
        });
        assert_eq!(app.invites.selected, 0);
        assert_eq!(app.invite_action(), Some("Request to join"));
        for (width, height) in [(82, 24), (40, 12)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| crate::ui::render(frame, &mut app))
                .unwrap();
            assert!(!app.invites.hit_regions.is_empty());
        }
        app.run_binding("down", 1);
        let mut repeat = KeyEvent::new(yazi_term::event::KeyCode::Enter, Modifiers::empty());
        repeat.kind = yazi_term::event::KeyEventKind::Repeat;
        assert!(app.handle_key(&repeat).is_empty());
        let outgoing = app.run_binding("open", 1);
        let [
            TelegramCommand::JoinInvite {
                request_id,
                request_needed,
                ..
            },
        ] = outgoing.as_slice()
        else {
            panic!("join")
        };
        assert!(*request_needed);
        assert!(app.run_binding("open", 1).is_empty());
        app.run_binding("cancel", 1);
        app.handle_network(NetworkEvent::InviteJoined {
            request_id: *request_id,
            result: Ok(Outcome::Requested),
        });
        assert_eq!(app.mode, Mode::Navigate);
        assert_eq!(app.active_chat_id, Some(1));
        assert!(
            app.status_message
                .as_ref()
                .unwrap()
                .contains("waiting for an administrator")
        );
        assert!(!format!("{:?}", outgoing[0]).contains("test_invite"));
    }

    #[test]
    fn open_command_resolves_usernames_and_ignores_superseded_results() {
        let mut app = ready_app();
        open_first(&mut app);
        app.connection = crate::event::ConnectionStatus::Online;
        app.draft_data_mut(1).input.set_value("keep this draft");
        app.run_binding("command", 1);
        app.commands.input.set_value("open not a username");
        assert!(app.run_binding("open", 1).is_empty());
        assert_eq!(app.mode, Mode::Command);
        app.commands.input.set_value("open @alice_name");
        assert_eq!(
            app.run_binding("open", 1),
            [TelegramCommand::ResolveTelegramLink {
                url: "https://t.me/alice_name".to_owned()
            }]
        );
        app.run_binding("command", 1);
        app.commands.input.set_value("open @bob_name");
        app.run_binding("open", 1);
        app.handle_network(NetworkEvent::LinkResolved {
            url: "https://t.me/alice_name".to_owned(),
            chat: chat(90, "Alice"),
            message: None,
        });
        assert_eq!(app.active_chat_id, Some(1));
        app.handle_network(NetworkEvent::LinkFailed {
            url: "https://t.me/alice_name".to_owned(),
            error: "old failure".to_owned(),
        });
        assert!(
            !app.status_message
                .as_deref()
                .unwrap_or_default()
                .contains("old failure")
        );
        let commands = app.handle_network(NetworkEvent::LinkResolved {
            url: "https://t.me/bob_name".to_owned(),
            chat: chat(91, "Bob"),
            message: None,
        });
        assert!(matches!(
            commands.as_slice(),
            [TelegramCommand::LoadHistory { chat_id: 91, .. }]
        ));
        assert_eq!(app.active_chat_id, Some(91));
        assert_eq!(app.draft_data(1).unwrap().input.value(), "keep this draft");
        app.activate_url("https://t.me/alice_name");
        app.start_composing();
        app.handle_network(NetworkEvent::LinkResolved {
            url: "https://t.me/alice_name".to_owned(),
            chat: chat(90, "Alice"),
            message: None,
        });
        assert_eq!(app.active_chat_id, Some(91));
        assert_eq!(app.mode, Mode::Compose);
    }

    #[test]
    fn username_lookup_keeps_existing_dialog_counters_and_folder_membership() {
        let mut app = ready_app();
        let known = app.chats.iter_mut().find(|chat| chat.id == 2).unwrap();
        known.membership.archived = true;
        known.last_message_id = Some(500);
        known.last_message = "Latest message".to_owned();
        let before = known.clone();
        app.activate_url("https://t.me/beta");
        let mut lookup = chat(2, "New title");
        lookup.unread = 0;
        app.handle_network(NetworkEvent::LinkResolved {
            url: "https://t.me/beta".to_owned(),
            chat: lookup,
            message: None,
        });
        let after = app.chats.iter().find(|chat| chat.id == 2).unwrap();
        assert_eq!(after.unread, before.unread);
        assert_eq!(after.last_message_id, before.last_message_id);
        assert_eq!(after.last_message, before.last_message);
        assert_eq!(after.membership, before.membership);
        assert_eq!(after.title, "New title");
        assert_eq!(app.folder_id, 1);
    }

    #[test]
    fn linked_target_survives_history_and_dialog_refresh_with_anchor() {
        let mut app = ready_app();
        open_first(&mut app);
        let linked = message(5, 99, "linked target", false);
        let linked_chat = Chat {
            read_inbox_max_id: Some(0),
            membership: crate::folders::ChatMembership::default(),
            id: 99,
            title: "Linked group".to_owned(),
            kind: ChatKind::Group,
            unread: 0,
            last_message_id: None,
            last_message: "linked target".to_owned(),
            last_activity: None,
        };
        app.pending_telegram_link = Some("https://t.me/example/5".to_owned());
        let commands = app.handle_network(NetworkEvent::LinkResolved {
            url: "https://t.me/example/5".to_owned(),
            chat: linked_chat,
            message: Some(linked),
        });
        let request_id = commands
            .iter()
            .find_map(|command| match command {
                TelegramCommand::LoadHistory { request_id, .. } => Some(*request_id),
                _ => None,
            })
            .expect("linked history request");
        assert_eq!(app.viewport_anchor_message, Some(5));
        assert_eq!(app.selected_message, Some(5));
        app.handle_action(KeyAction::Character('i'));
        assert_eq!(
            app.active_reply_target().map(|reply| reply.message_id),
            Some(5)
        );

        app.handle_network(NetworkEvent::History {
            chat_id: 99,
            request_id,
            messages: (100..180)
                .map(|id| message(id, 99, &format!("recent {id}"), false))
                .collect(),
        });
        assert!(app.active_messages().iter().any(|message| message.id == 5));
        assert_eq!(app.viewport_anchor_message, Some(5));

        app.handle_network(NetworkEvent::Dialogs(vec![chat(1, "Alpha")]));
        assert_eq!(app.active_chat_id, Some(99));
        assert!(app.chats.iter().any(|chat| chat.id == 99));
    }

    #[test]
    fn old_link_target_survives_a_full_cached_destination() {
        let mut app = ready_app();
        open_first(&mut app);
        app.selected_message = Some(20);
        app.messages.insert(
            2,
            (1000..1000 + i32::try_from(MAX_MESSAGES_PER_CHAT).expect("small cap"))
                .map(|id| message(id, 2, "recent", false))
                .collect(),
        );

        app.pending_telegram_link = Some("https://t.me/example/5".to_owned());
        app.handle_network(NetworkEvent::LinkResolved {
            url: "https://t.me/example/5".to_owned(),
            chat: chat(2, "Beta"),
            message: Some(message(5, 2, "old exact target", false)),
        });

        assert_eq!(app.active_chat_id, Some(2));
        assert_eq!(app.selected_message, Some(5));
        assert!(
            app.active_messages()
                .iter()
                .any(|message| message.id == 5 && message.text == "old exact target")
        );
    }

    #[test]
    fn selecting_actionable_message_sets_a_visible_semantic_anchor() {
        let mut app = ready_app();
        open_first(&mut app);
        app.messages.insert(
            1,
            (1..=30)
                .map(|id| {
                    message(
                        id,
                        1,
                        if id == 2 {
                            "https://t.me/rustlang/2"
                        } else {
                            "ordinary"
                        },
                        false,
                    )
                })
                .collect(),
        );
        app.run_binding("next_action", 1);
        assert_eq!(app.selected_message, Some(2));
        assert_eq!(app.viewport_anchor_message, Some(2));
        assert!(app.message_scroll > 0);

        app.handle_action(KeyAction::PageDown);
        assert_eq!(app.selected_message, None);
        assert_eq!(app.viewport_anchor_message, None);
    }

    #[test]
    fn reply_previews_batch_visible_targets_and_keep_live_edits_and_deletions() {
        let mut app = ready_app();
        open_first(&mut app);
        let mut replies = Vec::new();
        for (id, target) in [(21, 10), (22, 10), (23, 11)] {
            let mut source = message(id, 1, "reply", false);
            source.reply_to = Some(ReplyInfo {
                chat_id: 1,
                message_id: target,
                sender: None,
            });
            replies.push(source);
        }
        app.messages.insert(1, replies);
        app.set_message_hit_regions(vec![
            (0, 80, 1, (21, None)),
            (0, 80, 2, (22, None)),
            (0, 80, 3, (23, None)),
        ]);
        let commands = app.request_visible_replies();
        let [
            TelegramCommand::LoadReplyPreviews {
                chat_id: 1,
                message_ids,
                request_id,
            },
        ] = commands.as_slice()
        else {
            panic!("one batch expected")
        };
        assert_eq!(message_ids, &[10, 11]);
        assert!(app.request_visible_replies().is_empty());
        app.handle_network(NetworkEvent::MessageUpdated(message(
            10, 1, "new edit", false,
        )));
        app.handle_network(NetworkEvent::MessagesDeleted {
            channel_id: None,
            message_ids: vec![11],
        });
        app.handle_network(NetworkEvent::ReplyPreviews {
            chat_id: 1,
            request_id: *request_id,
            messages: vec![
                message(10, 1, "stale", false),
                message(11, 1, "deleted", false),
            ],
            unavailable: Vec::new(),
            complete: true,
        });
        let first = app.messages[&1]
            .iter()
            .find(|message| message.id == 21)
            .unwrap()
            .reply_to
            .as_ref()
            .unwrap();
        assert_eq!(app.reply_message(first).unwrap().text, "new edit");
        let deleted = app.messages[&1]
            .iter()
            .find(|message| message.id == 23)
            .unwrap()
            .reply_to
            .as_ref()
            .unwrap();
        assert!(app.reply_message(deleted).is_none());
        assert!(app.reply_preview_status(deleted).contains("unavailable"));
        assert_eq!(app.new_messages_while_scrolled, 0);
    }

    #[test]
    fn reply_excerpt_enters_the_timeline_only_when_explicitly_opened() {
        let mut app = ready_app();
        open_first(&mut app);
        let mut source = message(21, 1, "reply", false);
        source.reply_to = Some(ReplyInfo {
            chat_id: 1,
            message_id: 10,
            sender: None,
        });
        app.messages.insert(1, vec![source]);
        app.set_message_hit_regions(vec![(0, 80, 1, (21, None))]);
        let commands = app.request_visible_replies();
        let [TelegramCommand::LoadReplyPreviews { request_id, .. }] = commands.as_slice() else {
            panic!("one batch expected")
        };
        app.handle_network(NetworkEvent::ReplyPreviews {
            chat_id: 1,
            request_id: *request_id,
            messages: vec![message(10, 1, "original", false)],
            unavailable: Vec::new(),
            complete: true,
        });
        assert_eq!(app.active_messages().len(), 1);
        app.selected_message = Some(21);
        app.selected_action = 0;
        assert!(app.handle_action(KeyAction::Enter).is_empty());
        assert_eq!(app.selected_message, Some(10));
        assert_eq!(app.viewport_anchor_message, Some(10));
        assert_eq!(app.active_messages().len(), 2);
    }

    #[test]
    fn reply_selection_jumps_to_a_cached_target() {
        let mut app = ready_app();
        open_first(&mut app);
        let replying = app
            .messages
            .get_mut(&1)
            .unwrap()
            .iter_mut()
            .find(|message| message.id == 20)
            .unwrap();
        replying.reply_to = Some(ReplyInfo {
            message_id: 5,
            chat_id: 1,
            sender: Some("Them".to_owned()),
        });

        assert!(app.run_binding("next_action", 1).is_empty());
        assert_eq!(app.selected_message, Some(20));
        assert!(app.handle_action(KeyAction::Character('r')).is_empty());
        assert_eq!(app.selected_message, Some(5));
        assert_eq!(app.viewport_anchor_message, Some(5));
        assert!(app.message_scroll > 0);
    }

    #[test]
    fn reply_navigation_loads_an_uncached_target_and_ignores_stale_results() {
        let mut app = ready_app();
        open_first(&mut app);
        let replying = app.messages.get_mut(&1).unwrap().last_mut().unwrap();
        replying.reply_to = Some(ReplyInfo {
            message_id: 80,
            chat_id: 1,
            sender: None,
        });
        app.run_binding("next_action", 1);
        assert_eq!(
            app.handle_action(KeyAction::Character('r')),
            vec![TelegramCommand::LoadMessage {
                chat_id: 1,
                source_message_id: 20,
                message_id: 80,
                request_id: 2,
            }]
        );
        app.handle_network(NetworkEvent::MessageLoadFailed {
            chat_id: 1,
            message_id: 80,
            request_id: 999,
            error: "stale".to_owned(),
        });
        assert!(
            app.status_message
                .as_deref()
                .is_some_and(|status| status.contains("Loading reply"))
        );

        let mut target = message(80, 1, "reply target", false);
        target.sender = "Target author".to_owned();
        app.handle_network(NetworkEvent::MessageLoaded {
            chat_id: 1,
            message_id: 80,
            request_id: 2,
            message: target,
        });
        assert_eq!(app.selected_message, Some(80));
        assert_eq!(app.viewport_anchor_message, Some(80));
        assert!(
            app.active_messages()
                .iter()
                .any(|message| message.id == 80 && message.text == "reply target")
        );
        assert_eq!(
            app.active_messages()
                .iter()
                .find(|message| message.id == 20)
                .and_then(|message| message.reply_to.as_ref())
                .and_then(|reply| reply.sender.as_deref()),
            Some("Target author")
        );
    }

    #[test]
    fn cross_chat_reply_navigation_uses_the_returned_target_conversation() {
        let mut app = ready_app();
        open_first(&mut app);
        let replying = app.messages.get_mut(&1).unwrap().last_mut().unwrap();
        replying.reply_to = Some(ReplyInfo {
            message_id: 99,
            chat_id: 2,
            sender: Some("@beta".to_owned()),
        });
        app.run_binding("next_action", 1);
        let commands = app.handle_action(KeyAction::Character('r'));
        assert_eq!(
            commands,
            vec![TelegramCommand::LoadMessage {
                chat_id: 1,
                source_message_id: 20,
                message_id: 99,
                request_id: 2,
            }]
        );

        let commands = app.handle_network(NetworkEvent::MessageLoaded {
            chat_id: 1,
            message_id: 99,
            request_id: 2,
            message: message(99, 2, "from beta", false),
        });

        assert_eq!(app.active_chat_id, Some(2));
        assert_eq!(app.selected_message, Some(99));
        assert_eq!(app.viewport_anchor_message, Some(99));
        assert!(
            commands
                .iter()
                .any(|command| matches!(command, TelegramCommand::LoadHistory { chat_id: 2, .. }))
        );
    }

    #[test]
    fn cross_chat_reply_requests_distinguish_source_and_target_identity() {
        let mut app = ready_app();
        open_first(&mut app);
        for (source_id, target_chat_id) in [(20, 2), (19, 3)] {
            app.messages
                .get_mut(&1)
                .unwrap()
                .iter_mut()
                .find(|message| message.id == source_id)
                .unwrap()
                .reply_to = Some(ReplyInfo {
                message_id: 99,
                chat_id: target_chat_id,
                sender: None,
            });
        }

        app.selected_message = Some(20);
        assert_eq!(
            app.handle_action(KeyAction::Character('r')),
            vec![TelegramCommand::LoadMessage {
                chat_id: 1,
                source_message_id: 20,
                message_id: 99,
                request_id: 2,
            }]
        );
        // Repeating exactly the same action is deduplicated.
        assert!(app.handle_action(KeyAction::Character('r')).is_empty());

        app.selected_message = Some(19);
        assert_eq!(
            app.handle_action(KeyAction::Character('r')),
            vec![TelegramCommand::LoadMessage {
                chat_id: 1,
                source_message_id: 19,
                message_id: 99,
                request_id: 3,
            }]
        );

        // The first request has been superseded even though its target has the
        // same numeric message ID.
        assert!(
            app.handle_network(NetworkEvent::MessageLoaded {
                chat_id: 1,
                message_id: 99,
                request_id: 2,
                message: message(99, 2, "from beta", false),
            })
            .is_empty()
        );
        assert_eq!(app.active_chat_id, Some(1));

        let mut target = message(99, 3, "from gamma", false);
        target.sender = "Gamma author".to_owned();
        app.handle_network(NetworkEvent::MessageLoaded {
            chat_id: 1,
            message_id: 99,
            request_id: 3,
            message: target,
        });
        assert_eq!(app.active_chat_id, Some(3));
        let source_messages = app.messages.get(&1).unwrap();
        assert_eq!(
            source_messages
                .iter()
                .find(|message| message.id == 19)
                .and_then(|message| message.reply_to.as_ref())
                .and_then(|reply| reply.sender.as_deref()),
            Some("Gamma author")
        );
        assert_eq!(
            source_messages
                .iter()
                .find(|message| message.id == 20)
                .and_then(|message| message.reply_to.as_ref())
                .and_then(|reply| reply.sender.as_deref()),
            None
        );
    }

    #[test]
    fn reply_result_is_rejected_when_source_target_changed_or_peer_is_wrong() {
        let mut app = ready_app();
        open_first(&mut app);
        let source = app.messages.get_mut(&1).unwrap().last_mut().unwrap();
        source.reply_to = Some(ReplyInfo {
            message_id: 99,
            chat_id: 2,
            sender: None,
        });
        app.selected_message = Some(20);
        app.handle_action(KeyAction::Character('r'));

        // An edit that retargets the source makes the in-flight result stale.
        app.messages
            .get_mut(&1)
            .unwrap()
            .last_mut()
            .unwrap()
            .reply_to
            .as_mut()
            .unwrap()
            .chat_id = 3;
        assert!(
            app.handle_network(NetworkEvent::MessageLoaded {
                chat_id: 1,
                message_id: 99,
                request_id: 2,
                message: message(99, 2, "stale beta", false),
            })
            .is_empty()
        );
        assert_eq!(app.active_chat_id, Some(1));
        assert!(
            app.status_message
                .as_deref()
                .is_some_and(|status| status.contains("target changed"))
        );

        // A fresh request records peer 3 and must reject the same message ID
        // returned from peer 2.
        app.handle_action(KeyAction::Character('r'));
        app.handle_network(NetworkEvent::MessageLoaded {
            chat_id: 1,
            message_id: 99,
            request_id: 3,
            message: message(99, 2, "wrong peer", false),
        });
        assert_eq!(app.active_chat_id, Some(1));
        assert!(
            app.status_message
                .as_deref()
                .is_some_and(|status| status.contains("wrong reply target"))
        );
        assert!(
            !app.messages
                .get(&2)
                .is_some_and(|messages| messages.iter().any(|message| message.id == 99))
        );

        // Leaving the source conversation while a valid request is in flight
        // must not let its eventual response hijack the active chat.
        app.handle_action(KeyAction::Character('r'));
        app.active_chat_id = Some(2);
        app.selected_message = None;
        app.handle_network(NetworkEvent::MessageLoaded {
            chat_id: 1,
            message_id: 99,
            request_id: 4,
            message: message(99, 3, "late target", false),
        });
        assert_eq!(app.active_chat_id, Some(2));
        assert!(
            !app.messages
                .get(&3)
                .is_some_and(|messages| messages.iter().any(|message| message.id == 99))
        );
    }

    #[test]
    fn failed_attachment_remains_selectable_and_retries_exact_path() {
        let mut app = ready_app();
        open_first(&mut app);
        let path = std::env::temp_dir().join(format!("termgram-retry-{}.png", std::process::id()));
        fs::write(&path, b"png").expect("create retry fixture");
        prepare_file(&mut app, &path);
        let command = app.send_draft(1);
        app.mode = Mode::Navigate;
        let TelegramCommand::SendAttachment {
            local_id,
            attachment: sent_attachment,
            ..
        } = command.into_iter().next().expect("send command")
        else {
            panic!("expected attachment send");
        };
        app.handle_network(NetworkEvent::AttachmentSendFailed {
            chat_id: 1,
            local_id,
            attachment: sent_attachment.clone(),
            caption: String::new(),
            reply_to: None,
            error: "offline".to_owned(),
        });
        assert_eq!(app.selected_message, Some(local_id));
        assert_eq!(
            app.handle_action(KeyAction::Enter),
            vec![TelegramCommand::SendAttachment {
                chat_id: 1,
                local_id,
                attachment: sent_attachment,
                caption: String::new(),
                reply_to: None,
            }]
        );
        fs::remove_file(path).expect("remove fixture");
    }

    #[test]
    fn optimistic_attachments_reconcile_by_identity_not_empty_caption() {
        let first = Message {
            reactions: None,
            poll: None,
            entities: Vec::new(),
            notification: None,
            mention: None,
            edited_at: None,
            pinned: false,
            id: -1,
            chat_id: 1,
            sender_username: None,
            sender: "You".to_owned(),
            reply_to: None,
            text: String::new(),
            timestamp: Utc::now(),
            outgoing: true,
            delivery: Delivery::Pending,
            attachment: Some(Attachment {
                source_id: None,
                kind: AttachmentKind::File,
                file_name: Some("first.txt".to_owned()),
                mime_type: Some("text/plain".to_owned()),
                size: Some(1),
                fallback_emoji: None,
            }),
            links: Vec::new(),
            buttons: Vec::new(),
        };
        let second = Message {
            reactions: None,
            poll: None,
            entities: Vec::new(),
            notification: None,
            mention: None,
            edited_at: None,
            pinned: false,
            id: -2,
            attachment: Some(Attachment {
                source_id: None,
                file_name: Some("second.txt".to_owned()),
                ..first.attachment.clone().unwrap()
            }),
            ..first.clone()
        };
        let server = Message {
            reactions: None,
            poll: None,
            entities: Vec::new(),
            notification: None,
            mention: None,
            edited_at: None,
            pinned: false,
            id: 42,
            attachment: second.attachment.clone(),
            delivery: Delivery::Sent,
            ..second.clone()
        };
        let mut pending = vec![first, second];
        assert_eq!(
            super::remove_matching_optimistic(&mut pending, &server),
            Some(-2)
        );
        assert_eq!(pending[0].id, -1);
    }

    #[test]
    fn optimistic_photo_reconciles_when_telegram_renames_it() {
        let mut local = message(-1, 1, "", true);
        local.timestamp = Utc::now();
        local.delivery = Delivery::Pending;
        local.attachment = Some(Attachment {
            source_id: None,
            kind: AttachmentKind::Photo,
            file_name: Some("holiday.png".to_owned()),
            mime_type: Some("image/png".to_owned()),
            size: Some(10),
            fallback_emoji: None,
        });
        let mut server = local.clone();
        server.id = 42;
        server.delivery = Delivery::Sent;
        server.attachment.as_mut().unwrap().file_name = Some("photo.jpg".to_owned());
        let mut pending = vec![local];

        assert_eq!(
            super::remove_matching_optimistic(&mut pending, &server),
            Some(-1)
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn attachment_success_clears_status_and_stale_selection() {
        let mut app = ready_app();
        open_first(&mut app);
        let mut local = message(-1, 1, "", true);
        local.delivery = Delivery::Pending;
        local.attachment = Some(Attachment {
            source_id: None,
            kind: AttachmentKind::File,
            file_name: Some("notes.txt".to_owned()),
            mime_type: Some("text/plain".to_owned()),
            size: Some(10),
            fallback_emoji: None,
        });
        app.messages.get_mut(&1).unwrap().push(local);
        app.selected_message = Some(-1);
        app.status_message = Some("Sending attachment…".to_owned());

        app.handle_network(NetworkEvent::MessageAccepted {
            chat_id: 1,
            local_id: -1,
        });
        assert_eq!(app.selected_message, None);
        assert_eq!(app.status_message, None);
    }

    #[test]
    fn edited_attachment_invalidates_downloaded_state() {
        let mut app = ready_app();
        open_first(&mut app);
        let mut original = message(21, 1, "", false);
        original.attachment = Some(Attachment {
            source_id: None,
            kind: AttachmentKind::File,
            file_name: Some("before.txt".to_owned()),
            mime_type: Some("text/plain".to_owned()),
            size: Some(1),
            fallback_emoji: None,
        });
        app.handle_network(NetworkEvent::NewMessage(original.clone()));
        app.downloading_attachments.insert((1, 21), 1);
        app.handle_network(NetworkEvent::AttachmentDownloaded {
            request_id: 1,
            chat_id: 1,
            message_id: 21,
            path: std::env::temp_dir().join("termgram-edited-fixture"),
        });
        assert_eq!(app.attachment_state(1, 21), AttachmentState::Downloaded);

        original.attachment.as_mut().unwrap().file_name = Some("after.txt".to_owned());
        app.handle_network(NetworkEvent::MessageUpdated(original));
        assert_eq!(app.attachment_state(1, 21), AttachmentState::Ready);
    }

    #[test]
    fn inline_media_downloads_are_bounded_and_ignore_superseded_messages() {
        let mut app = ready_app();
        open_first(&mut app);
        for id in 21..=23 {
            let mut photo = message(id, 1, "caption", false);
            photo.attachment = Some(Attachment {
                source_id: None,
                kind: AttachmentKind::Sticker,
                file_name: Some("sticker.tgs".to_owned()),
                mime_type: Some("application/x-tgsticker".to_owned()),
                size: None,
                fallback_emoji: Some("🙂".to_owned()),
            });
            app.handle_network(NetworkEvent::NewMessage(photo));
            app.media_slots.push(crate::media::MediaSlot {
                source: crate::media::MediaSource::Message {
                    chat_id: 1,
                    message_id: id,
                },
                viewport: ratatui::layout::Rect::new(0, 0, 24, 30),
                offset: 0,
                size: ratatui::layout::Size::new(24, 6),
            });
        }
        let requests = app.request_visible_media();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|c| matches!(
            c,
            TelegramCommand::DownloadPreview {
                thumbnail: true,
                ..
            }
        )));
        assert!(app.request_visible_media().is_empty());
        app.handle_action(KeyAction::Character('i'));
        assert_eq!(app.mode, Mode::Compose);
        let mut edited = app.messages[&1]
            .iter()
            .find(|m| m.id == 21)
            .unwrap()
            .clone();
        edited.text = "new caption".to_owned();
        app.handle_network(NetworkEvent::MessageUpdated(edited));
        assert_eq!(app.request_visible_media().len(), 1);
        app.handle_network(NetworkEvent::PreviewDownloaded {
            chat_id: 1,
            message_id: 21,
            request_id: 1,
            path: "stale.webp".into(),
        });
        assert!(app.media_previews[&(1, 21)].path.is_none());
        app.handle_network(NetworkEvent::PreviewDownloaded {
            chat_id: 1,
            message_id: 21,
            request_id: 3,
            path: "current.webp".into(),
        });
        assert!(
            app.media_previews[&(1, 21)]
                .status
                .contains("static preview")
        );
        assert!(!app.downloaded_attachments.contains_key(&(1, 21)));
        app.handle_network(NetworkEvent::MessagesDeleted {
            channel_id: None,
            message_ids: vec![21, 22, 23],
        });
        app.handle_network(NetworkEvent::PreviewDownloaded {
            chat_id: 1,
            message_id: 22,
            request_id: 2,
            path: "deleted.webp".into(),
        });
        assert!(app.media_previews.is_empty());
        assert!(app.request_visible_media().is_empty());
    }

    #[test]
    fn compose_restores_last_chat_by_account_across_restart() {
        use yazi_term::event::KeyCode;

        let directory =
            std::env::temp_dir().join(format!("termgram-navigation-{}", std::process::id()));
        let settings_path = directory.join("settings.conf");
        let mut first = ready_app();
        first.settings_path = Some(settings_path.clone());
        first.account_user_id = Some(101);
        first.selected_chat = 1;
        first.open_selected_chat();

        let mut restarted = ready_app();
        restarted.settings_path = Some(settings_path.clone());
        restarted.account_user_id = Some(101);
        restarted.load_navigation().unwrap();
        let commands = restarted.handle_key(&KeyEvent::new(KeyCode::Char('i'), Modifiers::empty()));
        assert!(matches!(
            commands.first(),
            Some(TelegramCommand::LoadHistory { chat_id: 2, .. })
        ));
        assert_eq!(restarted.mode, Mode::Compose);
        assert_eq!(restarted.active_chat_id, Some(2));

        let mut other = ready_app();
        other.settings_path = Some(settings_path);
        other.account_user_id = Some(102);
        other.load_navigation().unwrap();
        other.handle_key(&KeyEvent::new(KeyCode::Char('i'), Modifiers::empty()));
        assert_eq!(other.active_chat_id, Some(1));
        assert!(other.active_reply_target().is_none());
        let stored: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(directory.join("navigation.json")).unwrap())
                .unwrap();
        assert_eq!(stored["accounts"]["101"], 2);
        assert_eq!(stored["accounts"]["102"], 1);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn media_click_selects_then_explicit_keys_preview_and_reply() {
        use yazi_term::event::KeyCode;
        let click = |column, row| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: Modifiers::empty(),
        };
        let mut app = ready_app();
        open_first(&mut app);
        let mut media = message(21, 1, "image", false);
        media.attachment = Some(Attachment {
            source_id: None,
            kind: AttachmentKind::Photo,
            file_name: None,
            mime_type: Some("image/jpeg".to_owned()),
            size: None,
            fallback_emoji: None,
        });
        app.handle_network(NetworkEvent::NewMessage(media));
        app.start_composing();
        app.handle_action(KeyAction::Character('x'));
        app.set_message_hit_regions(vec![(10, 70, 8, (21, Some(0)))]);
        app.set_composer_region((0, 80, 20, 23));
        assert!(app.handle_mouse(click(20, 8)).is_empty());
        assert_eq!(app.mode, Mode::Navigate);
        assert_eq!(app.selected_message, Some(21));
        let commands = app.handle_key(&KeyEvent::new(KeyCode::Char('o'), Modifiers::empty()));
        assert_eq!(app.mode, Mode::Preview);
        assert!(matches!(
            commands.first(),
            Some(TelegramCommand::DownloadPreview { message_id: 21, .. })
        ));
        let mut deleted = app.clone();
        deleted.handle_network(NetworkEvent::MessagesDeleted {
            channel_id: None,
            message_ids: vec![21],
        });
        deleted.handle_key(&KeyEvent::new(KeyCode::Char('i'), Modifiers::empty()));
        assert_eq!(deleted.mode, Mode::Preview);
        assert!(deleted.active_reply_target().is_none());
        app.handle_key(&KeyEvent::new(KeyCode::Escape, Modifiers::empty()));
        app.handle_key(&KeyEvent::new(KeyCode::Char('i'), Modifiers::empty()));
        assert_eq!(
            app.active_reply_target().map(|reply| reply.message_id),
            Some(21)
        );
        app.handle_mouse(click(10, 21));
        assert_eq!(app.mode, Mode::Compose);
        assert_eq!(
            app.active_reply_target().map(|reply| reply.message_id),
            Some(21)
        );
        assert_eq!(app.active_draft().unwrap().value(), "x");
        app.set_chat_hit_regions(vec![(0, 30, 4, 1)]);
        app.handle_mouse(click(5, 4));
        assert_eq!(app.mode, Mode::Navigate);
        assert_eq!(app.active_chat_id, Some(2));
        assert_eq!(app.draft_for(1).unwrap().value(), "x");
    }

    #[test]
    fn telemetry_rejects_observations_from_before_disconnect_or_account_switch() {
        use crate::event::ConnectionStatus;
        use crate::statusline::Latency;
        use std::time::{Duration, Instant};
        let mut app = ready_app();
        let sample = Latency {
            started: Instant::now(),
            elapsed: Duration::from_millis(42),
        };
        app.handle_network(NetworkEvent::Telemetry {
            dc_id: Some(2),
            latency: Some(sample),
        });
        assert_eq!(app.metrics.latency, Some(sample));
        app.handle_network(NetworkEvent::Status(ConnectionStatus::Reconnecting));
        assert!(app.metrics.latency.is_none());
        app.handle_network(NetworkEvent::Status(ConnectionStatus::Online));
        app.handle_network(NetworkEvent::Telemetry {
            dc_id: Some(2),
            latency: Some(sample),
        });
        assert!(app.metrics.latency.is_none());
        let fresh = Latency {
            started: Instant::now(),
            ..sample
        };
        app.handle_network(NetworkEvent::Telemetry {
            dc_id: Some(4),
            latency: Some(fresh),
        });
        assert_eq!(app.metrics.latency, Some(fresh));
        app.reset_for_account_switch(2);
        assert!(app.metrics.dc_id.is_none());
        assert!(app.metrics.latency.is_none());
        app.connection = ConnectionStatus::Online;
        app.handle_network(NetworkEvent::Telemetry {
            dc_id: Some(4),
            latency: Some(fresh),
        });
        assert!(app.metrics.latency.is_none());
    }

    #[test]
    fn right_clicking_a_message_starts_a_reply_without_activating_its_actions() {
        let mut app = ready_app();
        open_first(&mut app);
        app.set_message_hit_regions(vec![(10, 70, 8, (20, None))]);

        assert!(
            app.update(AppEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Right),
                column: 20,
                row: 8,
                modifiers: Modifiers::empty(),
            }))
            .is_empty()
        );
        assert_eq!(app.mode, Mode::Compose);
        assert_eq!(app.selected_message, Some(20));
        assert_eq!(
            app.active_reply_target().map(|reply| reply.message_id),
            Some(20)
        );
    }

    #[test]
    fn mouse_clicks_open_chats_and_activate_overlay_rows() {
        let click = |column, row| {
            AppEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row,
                modifiers: Modifiers::empty(),
            })
        };

        let mut app = ready_app();
        app.set_chat_hit_regions(vec![(1, 30, 4, 1)]);
        let commands = app.update(click(5, 4));
        assert_eq!(app.active_chat_id, Some(2));
        assert!(matches!(
            commands.first(),
            Some(TelegramCommand::LoadHistory { chat_id: 2, .. })
        ));

        app.mode = Mode::Settings;
        app.set_settings_hit_regions(vec![(20, 70, 8, 0)]);
        let before = app.settings.automatic_update_checks;
        assert!(app.update(click(25, 8)).is_empty());
        assert_eq!(app.settings.automatic_update_checks, !before);

        app.mode = Mode::Accounts;
        app.settings.active_account = 1;
        app.settings.account_count = 2;
        app.set_account_hit_regions(vec![(20, 70, 10, 2)]);
        assert_eq!(
            app.update(click(25, 10)),
            vec![TelegramCommand::SwitchAccount { account: 3 }]
        );
        assert_eq!(app.active_account(), 3);
    }

    #[test]
    fn mouse_wheel_routes_by_pane_instead_of_keyboard_focus() {
        let mut app = ready_app();
        app.focus = Focus::Conversation;
        app.selected_chat = 2;
        app.set_chat_pane_region((0, 30, 1, 20));
        app.set_conversation_pane_region((30, 100, 1, 20));

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 10,
            row: 5,
            modifiers: Modifiers::empty(),
        });
        assert_eq!(app.focus, Focus::Chats);
        assert_eq!(app.selected_chat, 0);

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 50,
            row: 5,
            modifiers: Modifiers::empty(),
        });
        assert_eq!(app.focus, Focus::Conversation);
    }

    use crate::input::TextInput;
    #[test]
    fn folders_preserve_pins_membership_drafts_and_alias_destinations() {
        use crate::folders::Folder;
        let mut app = App {
            screen: Screen::Main,
            chats: vec![chat(1, "One"), chat(2, "Two"), chat(3, "Three")],
            ..App::default()
        };
        app.folders.push(Folder {
            id: 9,
            title: "Work".to_owned(),
            pinned: vec![2],
            include: vec![1],
            ..Folder::default()
        });
        app.active_chat_id = Some(3);
        app.draft_data_mut(3).input = TextInput::from_value("draft");
        app.run_binding("folder_next", 1);
        assert_eq!(
            app.visible_chats()
                .iter()
                .map(|chat| chat.id)
                .collect::<Vec<_>>(),
            vec![2, 1]
        );
        assert_eq!(app.active_chat_id, Some(3));
        assert_eq!(app.draft_for(3).unwrap().value(), "draft");
        app.keymap.chats.insert("three".to_owned(), 3);
        app.run_binding("jump three", 1);
        assert_eq!(app.folder_id, 0);
        assert_eq!(app.selected_chat_entry().unwrap().id, 3);
        app.handle_network(NetworkEvent::Folders(vec![Folder::all()]));
        assert_eq!(app.folder_id, 0);
    }

    #[test]
    fn server_pins_preserve_selection_and_archive_has_its_own_order_and_aliases() {
        use crate::{
            folders::Folder,
            pins::{DialogAction, DialogPins, DialogScope},
        };
        let mut app = ready_app();
        app.chats = vec![chat(1, "One"), chat(2, "Two"), chat(3, "Archived")];
        app.chats[2].membership.archived = true;
        app.folders.push(Folder::archive());
        app.selected_chat = 0;
        app.focus = Focus::Chats;
        app.handle_network(NetworkEvent::DialogPins(DialogPins {
            main: vec![2, 1],
            archive: vec![3],
        }));
        assert_eq!(
            app.visible_chats()
                .iter()
                .map(|chat| chat.id)
                .collect::<Vec<_>>(),
            vec![2, 1]
        );
        assert_eq!(app.selected_chat_entry().unwrap().id, 1);
        let commands = app.run_binding("pin_up", 1);
        assert!(matches!(
            commands.as_slice(),
            [TelegramCommand::ChangeDialogPin {
                chat_id: 1,
                scope: DialogScope::Main,
                action: DialogAction::MoveUp,
                ..
            }]
        ));
        assert!(app.run_binding("pin_up", 1).is_empty());
        app.handle_network(NetworkEvent::DialogPinFinished {
            request_id: 1,
            error: Some("PINNED_DIALOGS_TOO_MUCH".into()),
        });
        assert_eq!(app.pins.dialogs.main, vec![2, 1]);
        app.keymap.chats.insert("archived".into(), 3);
        app.run_binding("jump archived", 1);
        assert_eq!(app.folder_id, 1);
        assert_eq!(app.active_chat_id, Some(3));
        assert_eq!(app.selected_chat_entry().unwrap().id, 3);
        app.handle_network(NetworkEvent::ArchiveChanged {
            chat_id: 3,
            archived: false,
        });
        assert!(app.visible_chats().is_empty());
        app.folder_id = 0;
        assert_eq!(app.visible_chats().len(), 3);
    }

    #[test]
    fn message_pin_confirmation_defaults_and_failures_keep_the_selected_target() {
        use crate::pins::MessageAction as PinAction;
        let mut app = ready_app();
        app.active_chat_id = Some(1);
        app.chats[0].kind = crate::model::ChatKind::Direct;
        app.account_user_id = Some(99);
        app.focus = Focus::Conversation;
        app.messages
            .insert(1, vec![message(10, 1, "keep this", false)]);
        app.selected_message = Some(10);
        assert!(app.run_binding("pin", 1).is_empty());
        assert_eq!(app.mode, Mode::PinPrompt);
        let commands = app.run_binding("open", 1);
        let [
            TelegramCommand::ChangeMessagePin {
                request_id, action, ..
            },
        ] = commands.as_slice()
        else {
            panic!("pin mutation");
        };
        assert_eq!(
            *action,
            PinAction::Pin {
                notify: false,
                only_self: true
            }
        );
        assert!(app.run_binding("open", 1).is_empty());
        app.handle_network(NetworkEvent::MessagePinFinished {
            chat_id: 1,
            request_id: *request_id,
            error: Some("CHAT_ADMIN_REQUIRED".into()),
        });
        assert_eq!(app.mode, Mode::PinPrompt);
        assert!(!app.active_messages()[0].pinned);
        app.run_binding("cancel", 1);
        app.chats[0].kind = crate::model::ChatKind::Group;
        app.run_binding("pin", 1);
        assert_eq!(
            app.message_pins.prompt.as_ref().unwrap().options[0].1,
            PinAction::Pin {
                notify: true,
                only_self: false
            }
        );
    }

    #[test]
    fn pinned_navigation_preserves_drafts_and_does_not_resurrect_deleted_targets() {
        let mut app = ready_app();
        app.active_chat_id = Some(1);
        app.focus = Focus::Conversation;
        app.draft_data_mut(1).input = TextInput::from_value("unsent");
        let mut pin = message(20, 1, "target", false);
        pin.pinned = true;
        let commands = app.run_binding("pins", 1);
        let [TelegramCommand::LoadPinnedMessages { request_id, .. }] = commands.as_slice() else {
            panic!("pin list");
        };
        app.handle_network(NetworkEvent::PinnedMessages {
            chat_id: 1,
            request_id: *request_id,
            page: crate::pins::MessagePage {
                messages: vec![pin.clone()],
                total: Some(1),
                ..Default::default()
            },
        });
        let commands = app.run_binding("open", 1);
        let [TelegramCommand::LoadPinnedContext { request_id, .. }] = commands.as_slice() else {
            panic!("pin context");
        };
        app.handle_network(NetworkEvent::MessagesDeleted {
            channel_id: None,
            message_ids: vec![20],
        });
        assert!(
            app.handle_network(NetworkEvent::PinnedContext {
                chat_id: 1,
                message_id: 20,
                request_id: *request_id,
                messages: vec![pin]
            })
            .is_empty()
        );
        assert_eq!(app.mode, Mode::PinnedMessages);
        assert!(app.message_pins.error.as_ref().unwrap().contains("deleted"));
        assert!(app.active_messages().iter().all(|message| message.id != 20));
        assert_eq!(app.active_draft().unwrap().value(), "unsent");
        let commands = app.run_binding("refresh", 1);
        let [TelegramCommand::LoadPinnedMessages { request_id, .. }] = commands.as_slice() else {
            panic!("refresh pins");
        };
        let mut pin = message(21, 1, "another", false);
        pin.pinned = true;
        app.handle_network(NetworkEvent::PinnedMessages {
            chat_id: 1,
            request_id: *request_id,
            page: crate::pins::MessagePage {
                messages: vec![pin.clone()],
                total: Some(1),
                ..Default::default()
            },
        });
        let commands = app.run_binding("open", 1);
        let [TelegramCommand::LoadPinnedContext { request_id, .. }] = commands.as_slice() else {
            panic!("pin context");
        };
        assert!(
            app.handle_network(NetworkEvent::PinnedContext {
                chat_id: 1,
                message_id: 21,
                request_id: *request_id,
                messages: vec![pin]
            })
            .is_empty()
        );
        assert_eq!(app.selected_message, Some(21));
        assert!(app.message_scroll > 0);
        assert_eq!(app.active_draft().unwrap().value(), "unsent");
    }
    #[test]
    fn pin_cache_invalidation_reloads_open_lists_and_rejects_old_context() {
        for scope in [Some(1), None] {
            let mut app = ready_app();
            app.active_chat_id = Some(1);
            app.focus = Focus::Conversation;
            app.draft_data_mut(1).input = TextInput::from_value("unsent");
            let mut pin = message(20, 1, "obsolete", false);
            pin.pinned = true;
            let commands = app.run_binding("pins", 1);
            let [TelegramCommand::LoadPinnedMessages { request_id, .. }] = commands.as_slice()
            else {
                panic!("pin list");
            };
            let old_page = crate::pins::MessagePage {
                messages: vec![pin.clone()],
                total: Some(1),
                ..Default::default()
            };
            app.handle_network(NetworkEvent::PinnedMessages {
                chat_id: 1,
                request_id: *request_id,
                page: old_page.clone(),
            });
            app.handle_network(NetworkEvent::CacheInvalidated { chat_id: Some(2) });
            assert_eq!(app.message_pins.page.messages[0].id, 20);
            let commands = app.run_binding("open", 1);
            let [
                TelegramCommand::LoadPinnedContext {
                    request_id: old_request,
                    ..
                },
            ] = commands.as_slice()
            else {
                panic!("pin context");
            };
            let commands = app.handle_network(NetworkEvent::CacheInvalidated { chat_id: scope });
            let request_id = commands
                .iter()
                .find_map(|command| match command {
                    TelegramCommand::LoadPinnedMessages {
                        request_id,
                        before: 0,
                        ..
                    } => Some(*request_id),
                    _ => None,
                })
                .expect("reload the open pin list");
            assert_ne!(request_id, *old_request);
            assert!(app.message_pins.page.messages.is_empty());
            assert!(app.pinned_summary().is_none());
            app.handle_network(NetworkEvent::PinnedContext {
                chat_id: 1,
                message_id: 20,
                request_id: *old_request,
                messages: vec![pin],
            });
            app.handle_network(NetworkEvent::PinnedMessages {
                chat_id: 1,
                request_id: *old_request,
                page: old_page,
            });
            assert_eq!(app.mode, Mode::PinnedMessages);
            assert!(app.active_messages().is_empty());
            assert!(app.message_pins.page.messages.is_empty());
            app.handle_network(NetworkEvent::PinnedMessages {
                chat_id: 1,
                request_id,
                page: crate::pins::MessagePage {
                    total: Some(0),
                    ..Default::default()
                },
            });
            assert!(!app.message_pins.loading);
            assert_eq!(app.active_draft().unwrap().value(), "unsent");
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn cloud_search_pages_keep_target_sender_and_drafts_without_reading_history() {
        let mut app = ready_app();
        app.connection = crate::event::ConnectionStatus::Online;
        app.active_chat_id = Some(1);
        app.draft_data_mut(1).input = crate::input::TextInput::from_value("unsent");
        app.run_binding("command", 1);
        app.commands
            .input
            .set_value("search --cloud --media invalid text");
        assert!(app.run_binding("open", 1).is_empty());
        assert_eq!(app.mode, Mode::Command);
        app.commands
            .input
            .set_value("search --cloud --from @ada --media file release");
        let commands = app.run_binding("open", 1);
        let [TelegramCommand::SearchCloud(request)] = commands.as_slice() else {
            panic!("cloud request")
        };
        assert_eq!(
            (request.chat_id, request.query.as_str(), request.before_id),
            (1, "release", 0)
        );
        let first_id = request.id;
        let page = crate::cloud_search::Page {
            messages: vec![
                message(30, 1, "release 界🙂", false),
                message(20, 1, "old release", false),
            ],
            next: Some(20),
            total: 3,
            sender: Some(99),
        };
        app.handle_network(NetworkEvent::CloudSearchResults {
            request_id: first_id,
            chat_id: 1,
            page: page.clone(),
        });
        for (width, height) in [(40, 12), (110, 24)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| crate::ui::render(frame, &mut app))
                .unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>();
            assert!(text.contains("Telegram search"));
            if width == 110 {
                assert!(text.contains("from @ada · file"));
            }
            assert!(app.request_visible_read().is_empty());
            assert!(
                app.handle_mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: 1,
                    row: 3,
                    modifiers: Modifiers::empty()
                })
                .is_empty()
            );
            assert_eq!(app.mode, Mode::Search);
        }
        app.handle_network(NetworkEvent::MessageUpdated(message(
            30,
            1,
            "revised release",
            false,
        )));
        assert_eq!(
            app.search.page.as_ref().unwrap().messages()[0].text,
            "revised release"
        );
        app.handle_network(NetworkEvent::MessagesDeleted {
            channel_id: None,
            message_ids: vec![20],
        });
        assert_eq!(app.search.page.as_ref().unwrap().messages().len(), 1);
        app.active_chat_id = Some(2);
        let commands = app.run_binding("search_more", 1);
        let [TelegramCommand::SearchCloud(request)] = commands.as_slice() else {
            panic!("next page")
        };
        assert_eq!((request.chat_id, request.before_id), (1, 20));
        assert_eq!(
            request.filters.sender,
            Some(crate::cloud_search::Sender::Id(99))
        );
        app.handle_network(NetworkEvent::CloudSearchResults {
            request_id: first_id,
            chat_id: 1,
            page: page.clone(),
        });
        assert!(
            app.search.page.is_none(),
            "a stale page must not replace an active query"
        );
        app.handle_network(NetworkEvent::CloudSearchResults {
            request_id: request.id,
            chat_id: 1,
            page: crate::cloud_search::Page {
                messages: vec![message(10, 1, "earliest release", false)],
                next: None,
                total: 2,
                sender: Some(99),
            },
        });
        assert!(app.run_binding("search_more", 1).is_empty());
        let commands = app.run_binding("search_previous", 1);
        let [TelegramCommand::SearchCloud(request)] = commands.as_slice() else {
            panic!("previous page")
        };
        assert_eq!(request.before_id, 0);
        let current_id = request.id;
        app.handle_network(NetworkEvent::CloudSearchResults {
            request_id: current_id,
            chat_id: 1,
            page: page.clone(),
        });
        let commands = app.run_binding("open", 1);
        let [
            TelegramCommand::LoadCloudContext {
                request_id,
                chat_id: 1,
                message_id: 30,
            },
        ] = commands.as_slice()
        else {
            panic!("context")
        };
        assert!(
            app.run_binding("open", 1).is_empty(),
            "do not queue duplicate context reads"
        );
        app.handle_network(NetworkEvent::CloudSearchContext {
            request_id: *request_id,
            chat_id: 2,
            message_id: 30,
            messages: vec![message(30, 2, "wrong peer", false)],
        });
        assert_eq!(app.mode, Mode::Search);
        assert!(
            app.handle_network(NetworkEvent::CloudSearchContext {
                request_id: *request_id,
                chat_id: 1,
                message_id: 30,
                messages: vec![
                    message(29, 1, "context", false),
                    message(30, 1, "release", false)
                ]
            })
            .is_empty()
        );
        assert_eq!(
            (app.active_chat_id, app.selected_message),
            (Some(1), Some(30))
        );
        assert!(app.browsing_older);
        assert_eq!(app.draft_for(1).unwrap().value(), "unsent");
        app.run_binding("search", 1);
        assert!(app.search.is_cloud());
        app.handle_network(NetworkEvent::CacheInvalidated { chat_id: Some(1) });
        app.handle_network(NetworkEvent::CloudSearchResults {
            request_id: current_id,
            chat_id: 1,
            page,
        });
        assert!(app.search.page.is_none());
        assert!(!app.search.loading);
        app.run_binding("cancel", 1);
        app.run_binding("command", 1);
        app.commands.input.set_value(r"search \b中文\s+  ");
        let commands = app.run_binding("open", 1);
        assert!(
            matches!(commands.as_slice(), [TelegramCommand::SearchCached(request)] if request.pattern == "\\b中文\\s+  ")
        );
    }

    #[test]
    fn cloud_search_cancel_offline_and_empty_filter_remain_editable() {
        let mut app = ready_app();
        app.connection = crate::event::ConnectionStatus::Offline;
        app.active_chat_id = Some(1);
        let filters = crate::cloud_search::Filters {
            media: crate::cloud_search::Media::Photo,
            ..Default::default()
        };
        app.start_cloud_search(1, String::new(), filters);
        assert!(app.search.editing);
        assert!(matches!(
            app.run_binding("open", 1).as_slice(),
            [TelegramCommand::CancelSearch]
        ));
        assert!(app.search.error.as_ref().unwrap().contains("Connect"));
        app.connection = crate::event::ConnectionStatus::Online;
        let commands = app.run_binding("open", 1);
        let [TelegramCommand::SearchCloud(request)] = commands.as_slice() else {
            panic!("filtered empty query")
        };
        assert!(request.query.is_empty());
        assert_eq!(request.filters.media, crate::cloud_search::Media::Photo);
        app.run_binding("cancel", 1);
        app.handle_network(NetworkEvent::CloudSearchResults {
            request_id: request.id,
            chat_id: 1,
            page: crate::cloud_search::Page {
                messages: vec![],
                next: None,
                total: 0,
                sender: None,
            },
        });
        assert!(app.search.page.is_none());
        app.run_binding("search", 1);
        assert!(app.search.editing);
        assert!(!app.search.loading);
    }

    #[test]
    fn local_search_ignores_stale_results_and_preserves_drafts_and_query() {
        let mut app = ready_app();
        app.active_chat_id = Some(1);
        app.draft_data_mut(1).input = TextInput::from_value("unsent");
        app.run_binding("search", 1);
        app.update(AppEvent::Paste("error.*".to_owned()));
        let commands = app.run_binding("open", 1);
        let TelegramCommand::SearchCached(request) = &commands[0] else {
            panic!("search request");
        };
        let page = crate::search::Page {
            messages: vec![message(21, 1, "error!", false)],
            next: None,
            cached_messages: 30,
            oldest: Some(1),
            newest: Some(30),
        };
        app.handle_network(NetworkEvent::SearchResults {
            request_id: request.id.wrapping_sub(1),
            page: page.clone(),
        });
        assert!(app.search.page.is_none());
        app.handle_network(NetworkEvent::SearchResults {
            request_id: request.id,
            page,
        });
        let commands = app.run_binding("open", 1);
        let TelegramCommand::LoadCachedContext { request_id, .. } = commands[0] else {
            panic!("cached context request");
        };
        let commands = app.handle_network(NetworkEvent::CachedContext {
            request_id,
            chat_id: 1,
            message_id: 21,
            messages: vec![
                message(20, 1, "before", false),
                message(21, 1, "error!", false),
                message(22, 1, "after", false),
            ],
        });
        assert!(
            commands.is_empty(),
            "opening search context must not mark unseen history read"
        );
        assert_eq!(app.selected_message, Some(21));
        app.run_binding("search", 1);
        assert_eq!(app.search.query.value(), "error.*");
        assert_eq!(app.draft_for(1).unwrap().value(), "unsent");
    }
    #[test]
    fn colors_follow_configuration_and_isolate_accounts_and_cancel_edits() {
        use crate::appearance::{Target, TerminalColor};
        use ratatui::style::Color;
        let mut app = ready_app();
        app.handle_network(NetworkEvent::AccountIdentity { user_id: 101 });
        assert_eq!(app.color(Target::Chat(1)), Color::Reset);
        assert_eq!(
            app.color(Target::Folder(7)),
            TerminalColor::folder_default(7).color()
        );
        app.keymap =
            crate::keymap::Keymap::parse("return { colors = { chats = { [1] = 'cyan' } } }")
                .unwrap();
        assert_eq!(app.color(Target::Chat(1)), Color::Cyan);
        app.run_binding("chat_color", 1);
        app.color_picker.as_mut().unwrap().selection = 3; // Red, after follow/default/black.
        app.handle_colors(KeyAction::Enter);
        assert_eq!(app.color(Target::Chat(1)), Color::Red);
        app.handle_network(NetworkEvent::AccountIdentity { user_id: 102 });
        assert_eq!(app.color(Target::Chat(1)), Color::Cyan);
        app.handle_network(NetworkEvent::AccountIdentity { user_id: 101 });
        app.run_binding("chat_color", 1);
        app.color_picker.as_mut().unwrap().selection = 0;
        app.handle_colors(KeyAction::Escape);
        assert_eq!(app.color(Target::Chat(1)), Color::Red);
        app.run_binding("chat_color", 1);
        app.color_picker.as_mut().unwrap().selection = 0;
        app.handle_colors(KeyAction::Enter);
        assert_eq!(app.color(Target::Chat(1)), Color::Cyan);
    }
    #[test]
    fn reveal_downloads_once_and_rejects_stale_completion_after_media_edit() {
        use yazi_term::event::{KeyCode, KeyEvent, KeyEventKind};
        let mut app = ready_app();
        open_first(&mut app);
        let mut attached = message(21, 1, "photo", false);
        attached.attachment = Some(Attachment {
            source_id: Some(55),
            kind: AttachmentKind::Photo,
            file_name: None,
            mime_type: Some("image/jpeg".to_owned()),
            size: Some(4),
            fallback_emoji: None,
        });
        app.messages.insert(1, vec![attached.clone()]);
        app.selected_message = Some(21);
        let mut key = KeyEvent {
            code: KeyCode::Char('O'),
            modifiers: Modifiers::SHIFT,
            kind: KeyEventKind::Repeat,
            ..KeyEvent::default()
        };
        assert!(app.handle_key(&key).is_empty());
        key.kind = KeyEventKind::Press;
        assert!(matches!(
            app.handle_key(&key).as_slice(),
            [TelegramCommand::DownloadAttachment {
                request_id: 1,
                media_id: Some(55),
                ..
            }]
        ));
        assert!(app.handle_key(&key).is_empty());
        attached.text = "edited caption".to_owned();
        app.handle_network(NetworkEvent::MessageUpdated(attached.clone()));
        assert_eq!(app.downloading_attachments.get(&(1, 21)), Some(&1));
        attached.attachment.as_mut().unwrap().source_id = Some(56);
        app.handle_network(NetworkEvent::MessageUpdated(attached));
        assert!(!app.reveal_after_download.contains(&(1, 21)));
        assert!(matches!(
            app.handle_key(&key).as_slice(),
            [TelegramCommand::DownloadAttachment {
                request_id: 2,
                media_id: Some(56),
                ..
            }]
        ));
        let absent = std::env::temp_dir()
            .join(format!("termgram-missing-{}", std::process::id()))
            .join("photo.jpg");
        app.handle_network(NetworkEvent::AttachmentDownloaded {
            request_id: 1,
            chat_id: 1,
            message_id: 21,
            path: absent.clone(),
        });
        assert_eq!(app.downloading_attachments.get(&(1, 21)), Some(&2));
        assert!(!app.downloaded_attachments.contains_key(&(1, 21)));
        app.handle_network(NetworkEvent::AttachmentDownloaded {
            request_id: 2,
            chat_id: 1,
            message_id: 21,
            path: absent,
        });
        assert!(
            app.status_message
                .as_deref()
                .unwrap()
                .starts_with("Could not reveal attachment")
        );
        assert!(matches!(
            app.handle_key(&key).as_slice(),
            [TelegramCommand::DownloadAttachment { request_id: 3, .. }]
        ));
        app.handle_network(NetworkEvent::MessagesDeleted {
            channel_id: None,
            message_ids: vec![21],
        });
        assert!(app.reveal_after_download.is_empty());
        assert!(app.downloading_attachments.is_empty());
    }
    #[test]
    fn old_message_edits_and_latest_deletions_do_not_leave_stale_dialog_previews() {
        let mut app = ready_app();
        app.chats[0].last_message_id = Some(100);
        app.chats[0].last_activity = Some(Utc.timestamp_opt(100, 0).unwrap());
        app.chats[0].last_message = "latest".to_owned();
        app.messages.insert(1, vec![message(21, 1, "old", false)]);
        app.handle_network(NetworkEvent::MessageUpdated(message(
            21,
            1,
            "edited old",
            false,
        )));
        assert_eq!(app.chats[0].last_message, "latest");
        app.handle_network(NetworkEvent::DialogsLoading);
        app.handle_network(NetworkEvent::MessagesDeleted {
            channel_id: None,
            message_ids: vec![100],
        });
        assert!(app.chats[0].last_message.is_empty());
        let mut stale = app.chats[0].clone();
        stale.last_message_id = Some(100);
        stale.last_message = "deleted".to_owned();
        app.handle_network(NetworkEvent::Dialogs(vec![stale]));
        assert!(app.chats[0].last_message.is_empty());
        app.handle_network(NetworkEvent::DialogsLoading);
        let mut fresh = app.chats[0].clone();
        fresh.last_message_id = Some(99);
        fresh.last_message = "previous live message".to_owned();
        app.handle_network(NetworkEvent::Dialogs(vec![fresh]));
        assert_eq!(app.chats[0].last_message, "previous live message");
    }
    #[test]
    fn editing_keeps_normal_draft_and_recovers_failure_without_sending_a_new_message() {
        let mut app = ready_app();
        app.account_user_id = Some(100);
        open_first(&mut app);
        app.selected_message = Some(20);
        app.draft_data_mut(1).input.set_value("unsent draft");
        let commands = app.run_binding("edit_message", 1);
        let [TelegramCommand::LoadEdit { request_id, .. }] = commands.as_slice() else {
            panic!("load original")
        };
        app.handle_network(NetworkEvent::EditLoaded {
            chat_id: 1,
            request_id: *request_id,
            result: Ok(crate::editing::Source {
                message_id: 20,
                text: "old caption".to_owned(),
                revision: [1; 32],
                caption: true,
            }),
        });
        app.handle_action(KeyAction::Clear);
        app.update(AppEvent::Paste("new 界🙂".to_owned()));
        assert_eq!(app.active_draft().unwrap().value(), "unsent draft");
        for width in [35, 110] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 18)).unwrap();
            terminal
                .draw(|frame| crate::ui::render(frame, &mut app))
                .unwrap();
            assert!(
                app.handle_mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: 1,
                    row: 3,
                    modifiers: Modifiers::empty()
                })
                .is_empty()
            );
            assert_eq!(app.mode, Mode::Edit);
            assert_eq!(app.selected_message, Some(20));
            assert!(
                app.request_visible_read().is_empty(),
                "editor covers the timeline"
            );
        }
        let commands = app.run_binding("send", 1);
        let [
            TelegramCommand::EditMessage {
                chat_id: 1,
                message_id: 20,
                request_id,
                text,
                ..
            },
        ] = commands.as_slice()
        else {
            panic!("edit only")
        };
        assert_eq!(text, "new 界🙂");
        app.handle_network(NetworkEvent::EditFinished {
            chat_id: 1,
            request_id: *request_id,
            error: Some("MESSAGE_EDIT_TIME_EXPIRED".to_owned()),
        });
        assert_eq!(app.message_edit().unwrap().input.value(), "new 界🙂");
        app.run_binding("cancel", 1);
        assert_eq!(app.mode, Mode::Navigate);
        app.run_binding("edit_message", 1);
        let commands = app.run_binding("send", 1);
        let [TelegramCommand::EditMessage { request_id, .. }] = commands.as_slice() else {
            panic!("retry edit")
        };
        app.run_binding("cancel", 1);
        app.active_chat_id = Some(2);
        app.handle_network(NetworkEvent::EditFinished {
            chat_id: 1,
            request_id: *request_id,
            error: None,
        });
        assert!(app.draft_data(1).unwrap().edit.is_none());
        assert_eq!(app.draft_data(1).unwrap().input.value(), "unsent draft");
        assert!(
            !app.messages
                .values()
                .flatten()
                .any(|message| message.id < 0)
        );
    }

    #[test]
    fn cancelled_original_load_does_not_open_or_replace_an_edit() {
        let mut app = ready_app();
        open_first(&mut app);
        app.selected_message = Some(20);
        let commands = app.run_binding("edit_message", 1);
        let [TelegramCommand::LoadEdit { request_id, .. }] = commands.as_slice() else {
            panic!("load")
        };
        app.run_binding("cancel", 1);
        app.handle_network(NetworkEvent::EditLoaded {
            chat_id: 1,
            request_id: *request_id,
            result: Ok(crate::editing::Source {
                message_id: 20,
                text: "late".to_owned(),
                revision: [1; 32],
                caption: false,
            }),
        });
        assert_eq!(app.mode, Mode::Navigate);
        assert!(app.message_edit().is_none());
    }

    #[test]
    fn edit_message_follows_the_selection_and_protects_a_modified_edit() {
        fn source(message_id: i32, text: &str) -> crate::editing::Source {
            crate::editing::Source {
                message_id,
                text: text.to_owned(),
                revision: [1; 32],
                caption: false,
            }
        }
        let mut app = ready_app();
        open_first(&mut app);
        app.selected_message = Some(20);
        let commands = app.run_binding("edit_message", 1);
        let [TelegramCommand::LoadEdit { request_id, .. }] = commands.as_slice() else {
            panic!("load")
        };
        app.handle_network(NetworkEvent::EditLoaded {
            chat_id: 1,
            request_id: *request_id,
            result: Ok(source(20, "original 20")),
        });
        app.draft_data_mut(1)
            .edit
            .as_mut()
            .unwrap()
            .input
            .set_value("changed 20");
        app.run_binding("cancel", 1);
        assert_eq!(app.mode, Mode::Navigate);
        // A modified edit survives: pressing e on another message refuses.
        app.selected_message = Some(21);
        assert!(app.run_binding("edit_message", 1).is_empty());
        assert_eq!(app.mode, Mode::Navigate);
        assert_eq!(app.message_edit().unwrap().source.message_id, 20);
        assert_eq!(app.message_edit().unwrap().input.value(), "changed 20");
        // Resuming still works from the edited message's own selection.
        app.selected_message = Some(20);
        assert!(app.run_binding("edit_message", 1).is_empty());
        assert_eq!(app.mode, Mode::Edit);
        app.run_binding("cancel", 1);
        // An untouched edit may be replaced by editing another message.
        app.draft_data_mut(1)
            .edit
            .as_mut()
            .unwrap()
            .input
            .set_value("original 20");
        app.selected_message = Some(21);
        let commands = app.run_binding("edit_message", 1);
        let [
            TelegramCommand::LoadEdit {
                message_id: 21,
                request_id,
                ..
            },
        ] = commands.as_slice()
        else {
            panic!("load the selected message")
        };
        app.handle_network(NetworkEvent::EditLoaded {
            chat_id: 1,
            request_id: *request_id,
            result: Ok(source(21, "original 21")),
        });
        assert_eq!(app.mode, Mode::Edit);
        assert_eq!(app.message_edit().unwrap().source.message_id, 21);
        assert_eq!(app.message_edit().unwrap().input.value(), "original 21");
    }

    #[test]
    fn deletion_requires_explicit_scope_and_preserves_messages_until_synced() {
        use crate::deletion::{Plan, Scope};
        let mut app = ready_app();
        open_first(&mut app);
        app.selected_message = Some(20);
        let load = |app: &mut App| {
            let commands = app.run_binding("delete_message", 1);
            let [TelegramCommand::ReviewDeletion { request_id, .. }] = commands.as_slice() else {
                panic!("review deletion")
            };
            app.handle_network(NetworkEvent::DeletionReady {
                chat_id: 1,
                message_id: 20,
                request_id: *request_id,
                result: Ok(Plan {
                    message: message(20, 1, "target 界🙂", false),
                    revision: [2; 32],
                    scopes: vec![Scope::OnlyMe, Scope::Everyone],
                }),
            });
        };
        load(&mut app);
        assert!(
            app.run_binding("open", 1).is_empty(),
            "Enter defaults to cancel"
        );
        assert_eq!(app.mode, Mode::Navigate);
        load(&mut app);
        for (width, height) in [(35, 12), (110, 22)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| crate::ui::render(frame, &mut app))
                .unwrap();
            assert!(
                app.handle_mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: 1,
                    row: 3,
                    modifiers: Modifiers::empty(),
                })
                .is_empty()
            );
            assert_eq!(app.selected_message, Some(20));
            assert_eq!(app.mode, Mode::DeletePrompt);
            assert!(app.request_visible_read().is_empty());
        }
        app.run_binding("down", 2);
        let commands = app.run_binding("open", 1);
        let [
            TelegramCommand::DeleteMessage {
                chat_id: 1,
                message_id: 20,
                scope: Scope::Everyone,
                request_id,
                ..
            },
        ] = commands.as_slice()
        else {
            panic!("explicit scope only")
        };
        assert!(app.active_messages().iter().any(|message| message.id == 20));
        app.handle_network(NetworkEvent::DeleteFinished {
            chat_id: 1,
            message_id: 20,
            request_id: *request_id,
            error: Some("MESSAGE_DELETE_FORBIDDEN".to_owned()),
        });
        assert_eq!(app.deletion.prompt.as_ref().unwrap().selected, 0);
        assert!(app.active_messages().iter().any(|message| message.id == 20));
        app.run_binding("down", 1);
        let commands = app.run_binding("open", 1);
        let [
            TelegramCommand::DeleteMessage {
                scope: Scope::OnlyMe,
                request_id,
                ..
            },
        ] = commands.as_slice()
        else {
            panic!("retry scope")
        };
        app.run_binding("cancel", 1);
        app.active_chat_id = Some(2);
        app.handle_network(NetworkEvent::MessagesDeleted {
            channel_id: None,
            message_ids: vec![20],
        });
        app.handle_network(NetworkEvent::DeleteFinished {
            chat_id: 1,
            message_id: 20,
            request_id: *request_id,
            error: None,
        });
        assert_eq!(app.active_chat_id, Some(2));
        assert!(app.deletion.prompt.is_none());
        assert!(!app.messages[&1].iter().any(|message| message.id == 20));
    }

    #[test]
    fn cancelled_or_changed_deletion_review_cannot_delete_a_stale_target() {
        use crate::deletion::{Plan, Scope};
        let mut app = ready_app();
        open_first(&mut app);
        app.selected_message = Some(20);
        let commands = app.run_binding("delete_message", 1);
        let [TelegramCommand::ReviewDeletion { request_id, .. }] = commands.as_slice() else {
            panic!("review")
        };
        let ready = NetworkEvent::DeletionReady {
            chat_id: 1,
            message_id: 20,
            request_id: *request_id,
            result: Ok(Plan {
                message: message(20, 1, "original", false),
                revision: [2; 32],
                scopes: vec![Scope::Everyone],
            }),
        };
        app.run_binding("cancel", 1);
        app.handle_network(ready.clone());
        assert!(app.deletion.prompt.is_none());
        let commands = app.run_binding("delete_message", 1);
        let [TelegramCommand::ReviewDeletion { request_id, .. }] = commands.as_slice() else {
            panic!("new review")
        };
        app.handle_network(NetworkEvent::MessageUpdated(message(
            20, 1, "changed", false,
        )));
        let NetworkEvent::DeletionReady { result, .. } = ready else {
            unreachable!()
        };
        app.handle_network(NetworkEvent::DeletionReady {
            chat_id: 1,
            message_id: 20,
            request_id: *request_id,
            result,
        });
        app.run_binding("down", 1);
        assert_eq!(app.deletion.prompt.as_ref().unwrap().selected, 0);
        assert!(app.run_binding("open", 1).is_empty());
        assert_eq!(app.mode, Mode::Navigate);
    }
    #[test]
    fn copy_captures_the_selected_message_and_only_accepts_its_own_result() {
        let mut app = ready_app();
        open_first(&mut app);
        assert!(
            app.run_binding("copy_text", 1).is_empty(),
            "selection is explicit"
        );
        app.selected_message = Some(20);
        app.run_binding("command", 1);
        app.update(AppEvent::Paste("copy link".to_owned()));
        app.selected_message = Some(19);
        let commands = app.run_binding("open", 1);
        let [
            TelegramCommand::CopyMessage {
                chat_id: 1,
                message_id: 20,
                link: true,
                request_id,
            },
        ] = commands.as_slice()
        else {
            panic!("command must retain its original target")
        };
        assert!(
            app.run_binding("copy_text", 1).is_empty(),
            "only one preparation in flight"
        );
        assert!(
            app.handle_network(NetworkEvent::MessageCopyReady {
                request_id: request_id + 1,
                result: Ok("wrong result".to_owned()),
            })
            .is_empty()
        );
        assert!(
            app.handle_network(NetworkEvent::MessageCopyReady {
                request_id: *request_id,
                result: Err("Content is protected".to_owned()),
            })
            .is_empty()
        );
        let commands = app.run_binding("copy_text", 1);
        let [
            TelegramCommand::CopyMessage {
                request_id,
                link: false,
                ..
            },
        ] = commands.as_slice()
        else {
            panic!("retry")
        };
        app.active_chat_id = Some(2);
        let result = app.handle_network(NetworkEvent::MessageCopyReady {
            request_id: *request_id,
            result: Ok("original 界🙂\ncaption".to_owned()),
        });
        assert_eq!(
            result,
            [TelegramCommand::CopyText(
                "original 界🙂\ncaption".to_owned()
            )]
        );
        assert_eq!(app.active_chat_id, Some(2));
        assert!(!format!("{:?}", result[0]).contains("caption"));
        assert!(
            app.handle_network(NetworkEvent::MessageCopyReady {
                request_id: *request_id,
                result: Ok("duplicate".to_owned()),
            })
            .is_empty()
        );
    }
    #[test]
    #[allow(clippy::too_many_lines)]
    fn forward_review_keeps_target_drafts_and_deduplication_id_on_retry() {
        let mut app = ready_app();
        app.account_user_id = Some(100);
        open_first(&mut app);
        app.selected_message = Some(20);
        app.draft_data_mut(1).input.set_value("source draft");
        app.draft_data_mut(2).input.set_value("destination draft");
        app.run_binding("forward_message", 1);
        assert_eq!(app.mode, Mode::Command);
        app.update(AppEvent::Paste("Beta".to_owned()));
        app.selected_message = Some(19);
        let commands = app.run_binding("open", 1);
        let [
            TelegramCommand::ReviewForward {
                chat_id: 1,
                message_id: 20,
                destination: 2,
                request_id,
            },
        ] = commands.as_slice()
        else {
            panic!("captured forward target")
        };
        assert!(app.run_binding("send", 1).is_empty(), "wait for review");
        app.handle_network(NetworkEvent::ForwardReady {
            request_id: *request_id,
            result: Ok(crate::forwarding::Plan {
                message: message(20, 1, "Forward me 界🙂", false),
                destination: chat(2, "Beta 最新名字"),
                revision: [4; 32],
                random_id: 9876,
            }),
        });
        for (width, height) in [(35, 12), (110, 22)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| crate::ui::render(frame, &mut app))
                .unwrap();
            assert!(
                app.handle_mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: 1,
                    row: 3,
                    modifiers: Modifiers::empty()
                })
                .is_empty()
            );
            assert_eq!(app.mode, Mode::ForwardPrompt);
            assert!(app.request_visible_read().is_empty());
        }
        let mut repeated = KeyEvent::new(yazi_term::event::KeyCode::Enter, Modifiers::empty());
        repeated.kind = yazi_term::event::KeyEventKind::Repeat;
        assert!(
            app.handle_key(&repeated).is_empty(),
            "held Enter cannot submit a new preview"
        );
        let commands = app.run_binding("send", 1);
        let [
            TelegramCommand::ForwardMessage {
                chat_id: 1,
                message_id: 20,
                destination: 2,
                random_id: 9876,
                request_id,
                ..
            },
        ] = commands.as_slice()
        else {
            panic!("send reviewed forward")
        };
        assert!(
            app.run_binding("send", 1).is_empty(),
            "no duplicate submission"
        );
        app.handle_network(NetworkEvent::ForwardFinished {
            request_id: *request_id,
            error: Some("Network interrupted".to_owned()),
        });
        let commands = app.run_binding("send", 1);
        let [
            TelegramCommand::ForwardMessage {
                random_id: 9876,
                request_id,
                ..
            },
        ] = commands.as_slice()
        else {
            panic!("retry must reuse its ID")
        };
        app.run_binding("cancel", 1);
        app.active_chat_id = Some(3);
        app.handle_network(NetworkEvent::ForwardFinished {
            request_id: *request_id,
            error: None,
        });
        assert_eq!(app.mode, Mode::Navigate);
        assert_eq!(app.active_chat_id, Some(3));
        assert_eq!(app.draft_data(1).unwrap().input.value(), "source draft");
        assert_eq!(
            app.draft_data(2).unwrap().input.value(),
            "destination draft"
        );
        assert!(app.forwarding.review.is_none());
    }

    #[test]
    fn cancelled_or_changed_forward_previews_never_send() {
        let mut app = ready_app();
        app.account_user_id = Some(100);
        open_first(&mut app);
        app.selected_message = Some(20);
        let commands = app.run_binding("save_message", 1);
        let [
            TelegramCommand::ReviewForward {
                destination: 100,
                request_id,
                ..
            },
        ] = commands.as_slice()
        else {
            panic!("save to this account")
        };
        app.run_binding("cancel", 1);
        let plan = crate::forwarding::Plan {
            message: message(20, 1, "old", false),
            destination: chat(100, "Saved Messages"),
            revision: [1; 32],
            random_id: 42,
        };
        app.handle_network(NetworkEvent::ForwardReady {
            request_id: *request_id,
            result: Ok(plan.clone()),
        });
        assert!(app.forwarding.review.is_none());
        let commands = app.run_binding("save_message", 1);
        let [TelegramCommand::ReviewForward { request_id, .. }] = commands.as_slice() else {
            panic!("new review")
        };
        app.handle_network(NetworkEvent::ForwardReady {
            request_id: *request_id,
            result: Ok(plan),
        });
        app.handle_network(NetworkEvent::MessageUpdated(message(
            20,
            1,
            "new content",
            false,
        )));
        assert!(app.run_binding("send", 1).is_empty());
        assert!(
            app.forwarding
                .review
                .as_ref()
                .unwrap()
                .error
                .as_ref()
                .unwrap()
                .contains("changed")
        );
    }

    #[test]
    fn saved_messages_uses_account_identity_even_without_a_cached_dialog() {
        let mut app = ready_app();
        app.account_user_id = Some(100);
        let commands = app.run_binding("saved_messages", 1);
        let [TelegramCommand::OpenSaved { request_id }] = commands.as_slice() else {
            panic!("resolve own peer")
        };
        assert!(
            app.handle_network(NetworkEvent::SavedReady {
                request_id: request_id + 1,
                result: Ok(chat(101, "Other account"))
            })
            .is_empty()
        );
        let commands = app.handle_network(NetworkEvent::SavedReady {
            request_id: *request_id,
            result: Ok(chat(100, "Saved Messages")),
        });
        assert!(matches!(
            commands.as_slice(),
            [TelegramCommand::LoadHistory { chat_id: 100, .. }]
        ));
        assert_eq!(app.active_chat_id, Some(100));
        app.connection = crate::event::ConnectionStatus::Offline;
        let commands = app.run_binding("saved_messages", 1);
        assert!(
            !commands
                .iter()
                .any(|command| matches!(command, TelegramCommand::OpenSaved { .. })),
            "cached dialog opens offline"
        );
    }
    #[test]
    #[allow(clippy::too_many_lines)]
    fn unread_mentions_keep_target_drafts_and_refresh_after_other_client_reads() {
        let mut app = ready_app();
        app.connection = crate::event::ConnectionStatus::Online;
        app.active_chat_id = Some(2);
        app.draft_data_mut(2).input = crate::input::TextInput::from_value("unsent");
        app.focus = Focus::Chats;
        app.run_binding("command", 1);
        app.commands.input.set_value("mentions");
        let commands = app.run_binding("open", 1);
        let [
            TelegramCommand::SearchMentions {
                chat_id: 1,
                before_id: 0,
                request_id,
            },
        ] = commands.as_slice()
        else {
            panic!("selected chat, not open chat")
        };
        let mut mention = message(30, 1, "reply to you", false);
        mention.mention = Some(crate::model::Mention {
            unread: true,
            requires_playback: false,
        });
        app.handle_network(NetworkEvent::CloudSearchResults {
            request_id: *request_id,
            chat_id: 1,
            page: crate::cloud_search::Page {
                messages: vec![mention.clone()],
                next: Some(30),
                total: 2,
                sender: None,
            },
        });
        for width in [40, 110] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 24)).unwrap();
            terminal
                .draw(|frame| crate::ui::render(frame, &mut app))
                .unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>();
            assert!(text.contains("Unread mentions"));
            assert!(!text.contains("Pattern"));
            assert!(app.request_visible_read().is_empty());
        }
        app.handle_network(NetworkEvent::MessageContentsRead {
            channel_id: Some(-1_000_000_000_001),
            message_ids: vec![30],
        });
        assert_eq!(
            app.search.page.as_ref().unwrap().messages().len(),
            1,
            "channel IDs cannot clear private mentions"
        );
        app.handle_network(NetworkEvent::MessageContentsRead {
            channel_id: None,
            message_ids: vec![30],
        });
        assert!(app.search.page.as_ref().unwrap().messages().is_empty());
        let commands = app.run_binding("search_more", 1);
        let [
            TelegramCommand::SearchMentions {
                chat_id: 1,
                before_id: 30,
                request_id,
            },
        ] = commands.as_slice()
        else {
            panic!("stable chat and next ID")
        };
        let stale = *request_id;
        let commands = app.run_binding("refresh", 1);
        let [
            TelegramCommand::SearchMentions {
                chat_id: 1,
                before_id: 0,
                request_id,
            },
        ] = commands.as_slice()
        else {
            panic!("refresh starts at newest")
        };
        app.handle_network(NetworkEvent::SearchFailed {
            request_id: stale,
            error: "stale".into(),
        });
        assert!(app.search.loading);
        app.handle_network(NetworkEvent::CloudSearchResults {
            request_id: *request_id,
            chat_id: 1,
            page: crate::cloud_search::Page {
                messages: vec![mention.clone()],
                next: None,
                total: 1,
                sender: None,
            },
        });
        app.run_binding("search_query", 1);
        assert!(!app.search.editing);
        let commands = app.run_binding("open", 1);
        let [
            TelegramCommand::LoadCloudContext {
                chat_id: 1,
                message_id: 30,
                request_id,
            },
        ] = commands.as_slice()
        else {
            panic!("open original context")
        };
        app.handle_network(NetworkEvent::CloudSearchContext {
            request_id: *request_id,
            chat_id: 1,
            message_id: 30,
            messages: vec![mention],
        });
        assert_eq!(app.mode, Mode::Navigate);
        assert_eq!(app.selected_message, Some(30));
        assert_eq!(app.active_chat_id, Some(1));
        assert_eq!(app.draft_data(2).unwrap().input.value(), "unsent");
        app.connection = crate::event::ConnectionStatus::Offline;
        app.run_binding("mentions", 1);
        assert!(app.search.error.is_some());
        assert!(!app.search.editing);
        assert_eq!(app.run_binding("open", 1), [TelegramCommand::CancelSearch]);
    }
}
