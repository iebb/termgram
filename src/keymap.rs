//! Declarative Lua configuration and contextual chords. Yazi owns terminal key
//! parsing/normalization; this module only maps those keys to Termgram actions.

use crate::actions::Action;

use anyhow::{Context as _, Result, bail};
use mlua::{HookTriggers, Lua, LuaOptions, LuaSerdeExt, StdLib, VmState};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::Path,
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use yazi_config::keymap::Key;
use yazi_term::event::{KeyEvent, KeyEventKind};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Context {
    Global,
    Chats,
    Conversation,
    Compose,
    Edit,
    Forward,
    Poll,
    Reactions,
    Command,
    Attachments,
    Input,
    Overlay,
    Help,
    Preview,
    Search,
    Pins,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingSpec {
    context: Context,
    on: Vec<String>,
    run: String,
    #[serde(default = "default_count")]
    count: usize,
    #[serde(default)]
    desc: Option<String>,
}

const fn default_count() -> usize {
    1
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Configuration {
    keymap: Vec<BindingSpec>,
    chats: BTreeMap<String, i64>,
    ghost_text: Option<String>,
    nerd_font: bool,
    colors: crate::appearance::Colors,
    statusline: crate::statusline::Configuration,
    sidebar: crate::sidebar::Configuration,
    messages: crate::transcript::Configuration,
    attachments: crate::staging::Configuration,
    notifications: crate::notifications::Configuration,
}

#[derive(Clone, Debug)]
struct Binding {
    context: Context,
    keys: Vec<Key>,
    run: Action,
    count: usize,
    description: String,
}

#[derive(Clone, Debug)]
pub struct Keymap {
    bindings: Vec<Binding>,
    pub chats: BTreeMap<String, i64>,
    pub ghost_text: String,
    pub nerd_font: bool,
    pub colors: crate::appearance::Colors,
    pub statusline: crate::statusline::Configuration,
    pub sidebar: crate::sidebar::Configuration,
    pub messages: crate::transcript::Configuration,
    pub attachments: crate::staging::Configuration,
    pub notifications: crate::notifications::Configuration,
    pending: Vec<Key>,
    count: usize,
    context: Option<Context>,
    last_key: Option<Instant>,
}

#[derive(Debug, Eq, PartialEq)]
pub enum Resolution {
    Action { run: Action, count: usize },
    Pending(String),
    Unbound,
}

impl Default for Keymap {
    #[allow(clippy::too_many_lines)]
    fn default() -> Self {
        let mut result = Self {
            bindings: Vec::new(),
            chats: BTreeMap::new(),
            colors: crate::appearance::Colors::default(),
            statusline: crate::statusline::Configuration::default(),
            sidebar: crate::sidebar::Configuration::default(),
            messages: crate::transcript::Configuration::default(),
            attachments: crate::staging::Configuration::default(),
            notifications: crate::notifications::Configuration::default(),
            ghost_text: "{send} to send".to_owned(),
            nerd_font: false,
            pending: Vec::new(),
            count: 0,
            context: None,
            last_key: None,
        };
        for (context, definitions) in [
            (
                Context::Global,
                &[
                    ("<C-c>", "quit"),
                    ("<C-l>", "redraw"),
                    ("<F2>", "next_account"),
                    ("<F3>", "add_account"),
                    ("<F4>", "toggle_sidebar"),
                ][..],
            ),
            (
                Context::Chats,
                &[
                    (":", "command"),
                    ("j", "down"),
                    ("k", "up"),
                    ("<Down>", "down"),
                    ("<Up>", "up"),
                    ("<Enter>", "open"),
                    ("<Right>", "open"),
                    ("<Tab>", "focus"),
                    ("<S-Tab>", "focus"),
                    ("i", "compose"),
                    ("c", "chat_color"),
                    ("p", "pin"),
                    ("e", "archive"),
                    ("<C-k>", "pin_up"),
                    ("<C-j>", "pin_down"),
                    ("C", "folder_color"),
                    ("/", "filter"),
                    ("<C-f>", "search"),
                    ("]", "folder_next"),
                    ("[", "folder_previous"),
                    ("q", "quit"),
                    ("?", "help"),
                    ("s", "settings"),
                    ("a", "accounts"),
                    ("<PageUp>", "page_up"),
                    ("<PageDown>", "page_down"),
                    ("G", "latest"),
                    ("g g", "oldest"),
                    ("g i", "chat_info"),
                    ("g s", "saved_messages"),
                    ("g m", "mentions"),
                    ("g r", "mark_read"),
                    ("g U", "mark_unread"),
                    ("<C-r>", "refresh"),
                ][..],
            ),
            (
                Context::Conversation,
                &[
                    (":", "command"),
                    ("e", "edit_message"),
                    ("d", "delete_message"),
                    ("f", "forward_message"),
                    ("v", "poll"),
                    ("g e", "reactions"),
                    ("g S", "save_message"),
                    ("y", "copy_text"),
                    ("Y", "copy_link"),
                    ("p", "pin"),
                    ("P", "pins"),
                    ("c", "chat_color"),
                    ("/", "search"),
                    ("j", "message_down"),
                    ("k", "message_up"),
                    ("[", "message_up"),
                    ("]", "message_down"),
                    ("<Down>", "down"),
                    ("<Up>", "up"),
                    ("<PageUp>", "page_up"),
                    ("<PageDown>", "page_down"),
                    ("G", "latest"),
                    ("<End>", "latest"),
                    ("g g", "oldest"),
                    ("g i", "chat_info"),
                    ("g s", "saved_messages"),
                    ("g m", "mentions"),
                    ("<Home>", "oldest"),
                    ("g u", "first_unread"),
                    ("g r", "mark_read"),
                    ("g U", "mark_unread"),
                    ("i", "compose"),
                    ("<Enter>", "open"),
                    ("<Esc>", "cancel"),
                    ("<Left>", "cancel"),
                    ("<Tab>", "focus"),
                    ("<S-Tab>", "focus"),
                    ("q", "quit"),
                    ("?", "help"),
                    ("s", "settings"),
                    ("a", "accounts"),
                    ("o", "preview"),
                    ("g n", "next_action"),
                    ("g p", "previous_action"),
                    ("O", "reveal"),
                    ("R", "reply"),
                    ("r", "reply_target"),
                    ("l", "open_link"),
                    ("<C-r>", "refresh"),
                ][..],
            ),
            (
                Context::Preview,
                &[
                    ("<Esc>", "cancel"),
                    ("q", "cancel"),
                    ("i", "reply"),
                    ("O", "reveal"),
                    ("o", "preview"),
                ][..],
            ),
            (
                Context::Forward,
                &[
                    ("<Enter>", "send"),
                    ("<Esc>", "cancel"),
                    ("<C-c>", "cancel"),
                ][..],
            ),
            (
                Context::Edit,
                &[
                    ("<Enter>", "send"),
                    ("<S-Enter>", "newline"),
                    ("<C-j>", "newline"),
                    ("<Esc>", "cancel"),
                    ("<C-c>", "cancel"),
                    ("<C-d>", "discard_edit"),
                ][..],
            ),
            (
                Context::Compose,
                &[
                    ("<C-o>", "attachments"),
                    ("<Enter>", "send"),
                    ("<S-Enter>", "newline"),
                    ("<C-j>", "newline"),
                    ("<Tab>", "complete_next"),
                    ("<S-Tab>", "complete_previous"),
                    ("<C-n>", "complete_next"),
                    ("<C-p>", "complete_previous"),
                    ("<Up>", "complete_previous"),
                    ("<Down>", "complete_next"),
                    ("<Esc>", "cancel"),
                ][..],
            ),
            (
                Context::Input,
                &[
                    ("<Enter>", "open"),
                    ("<Esc>", "cancel"),
                    ("<Tab>", "focus"),
                    ("<S-Tab>", "focus"),
                    ("<Up>", "up"),
                    ("<Down>", "down"),
                ][..],
            ),
            (
                Context::Reactions,
                &[
                    ("<Esc>", "cancel"),
                    ("q", "cancel"),
                    ("<Enter>", "open"),
                    ("<Space>", "open"),
                    ("j", "down"),
                    ("k", "up"),
                    ("<Up>", "up"),
                    ("<Down>", "down"),
                    ("<PageUp>", "page_up"),
                    ("<PageDown>", "page_down"),
                    ("<Home>", "home"),
                    ("<End>", "end"),
                    ("u", "clear_reactions"),
                    ("<C-r>", "refresh"),
                ][..],
            ),
            (
                Context::Poll,
                &[
                    ("<Esc>", "cancel"),
                    ("q", "cancel"),
                    ("j", "down"),
                    ("k", "up"),
                    ("<Up>", "up"),
                    ("<Down>", "down"),
                    ("<PageUp>", "page_up"),
                    ("<PageDown>", "page_down"),
                    ("<Home>", "home"),
                    ("<End>", "end"),
                    ("<Space>", "toggle_poll_answer"),
                    ("<Enter>", "toggle_poll_answer"),
                    ("<C-s>", "send"),
                    ("u", "retract_vote"),
                    ("<C-r>", "refresh"),
                    ("s", "spoilers"),
                ][..],
            ),
            (
                Context::Help,
                &[
                    ("/", "filter"),
                    ("<C-f>", "filter"),
                    ("<Esc>", "cancel"),
                    ("q", "cancel"),
                    ("?", "cancel"),
                    ("j", "down"),
                    ("k", "up"),
                    ("<Up>", "up"),
                    ("<Down>", "down"),
                    ("<PageUp>", "page_up"),
                    ("<PageDown>", "page_down"),
                    ("<Home>", "home"),
                    ("<End>", "end"),
                ][..],
            ),
            (
                Context::Overlay,
                &[
                    ("<C-r>", "refresh"),
                    ("<Enter>", "open"),
                    ("<Space>", "open"),
                    ("<Esc>", "cancel"),
                    ("?", "cancel"),
                    ("j", "down"),
                    ("k", "up"),
                    ("<Up>", "up"),
                    ("<Down>", "down"),
                ][..],
            ),
            (
                Context::Search,
                &[
                    ("<Enter>", "open"),
                    ("<Esc>", "cancel"),
                    ("<Tab>", "search_scope"),
                    ("<C-n>", "search_more"),
                    ("<C-p>", "search_previous"),
                    ("<C-f>", "search_query"),
                    ("<C-r>", "refresh"),
                    ("<Up>", "up"),
                    ("<Down>", "down"),
                    ("<PageUp>", "page_up"),
                    ("<PageDown>", "page_down"),
                ][..],
            ),
            (
                Context::Pins,
                &[
                    ("<Enter>", "open"),
                    ("<Esc>", "cancel"),
                    ("?", "cancel"),
                    ("j", "down"),
                    ("k", "up"),
                    ("<Down>", "down"),
                    ("<Up>", "up"),
                    ("<PageUp>", "page_up"),
                    ("<PageDown>", "page_down"),
                    ("<C-n>", "pins_more"),
                    ("<C-p>", "pins_previous"),
                    ("<C-r>", "refresh"),
                    ("p", "pin"),
                    ("U", "unpin_all"),
                ][..],
            ),
        ] {
            for &(keys, run) in definitions {
                result
                    .insert(BindingSpec {
                        context,
                        on: keys.split(' ').map(str::to_owned).collect(),
                        run: run.to_owned(),
                        count: 1,
                        desc: None,
                    })
                    .expect("valid builtin binding");
            }
        }
        for (key, run) in [
            ("<Esc>", "cancel"),
            ("<C-c>", "cancel"),
            ("<Enter>", "open"),
            ("<Tab>", "complete_next"),
            ("<C-n>", "complete_next"),
            ("<S-Tab>", "complete_previous"),
            ("<C-p>", "complete_previous"),
            ("<Up>", "history_previous"),
            ("<Down>", "history_next"),
        ] {
            result
                .insert(BindingSpec {
                    context: Context::Command,
                    on: vec![key.to_owned()],
                    run: run.to_owned(),
                    count: 1,
                    desc: None,
                })
                .expect("valid command binding");
        }
        for (key, run) in [
            ("<Esc>", "cancel"),
            ("q", "cancel"),
            ("?", "help"),
            ("i", "compose"),
            ("j", "down"),
            ("k", "up"),
            ("<Up>", "up"),
            ("<Down>", "down"),
            ("a", "attach"),
            ("d", "remove_attachment"),
            ("<Delete>", "remove_attachment"),
            ("p", "attachment_format"),
            ("o", "preview"),
            ("<Enter>", "preview"),
            ("O", "reveal"),
        ] {
            result
                .insert(BindingSpec {
                    context: Context::Attachments,
                    on: vec![key.to_owned()],
                    run: run.to_owned(),
                    count: 1,
                    desc: None,
                })
                .expect("valid attachment binding");
        }
        for context in [
            Context::Compose,
            Context::Conversation,
            Context::Attachments,
        ] {
            for key in ["<C-v>", "<C-A-v>", "<A-v>", "<D-v>"] {
                result
                    .insert(BindingSpec {
                        context,
                        on: vec![key.to_owned()],
                        run: "paste_clipboard".to_owned(),
                        count: 1,
                        desc: None,
                    })
                    .expect("valid clipboard binding");
            }
        }
        for context in [
            Context::Compose,
            Context::Edit,
            Context::Input,
            Context::Search,
            Context::Command,
        ] {
            for (key, run) in [
                ("<C-a>", "home"),
                ("<C-e>", "end"),
                ("<C-u>", "clear"),
                ("<C-w>", "delete_word"),
                ("<Home>", "home"),
                ("<End>", "end"),
                ("<Left>", "left"),
                ("<Right>", "right"),
                ("<Backspace>", "backspace"),
                ("<Delete>", "delete"),
            ] {
                result
                    .insert(BindingSpec {
                        context,
                        on: vec![key.to_owned()],
                        run: run.to_owned(),
                        count: 1,
                        desc: None,
                    })
                    .expect("valid editor binding");
            }
        }
        result
    }
}

impl Keymap {
    /// # Errors
    /// Returns a recoverable configuration error. No partially loaded bindings
    /// are installed, and execution/memory limits keep startup responsive.
    pub fn load(path: &Path) -> Result<Self> {
        match Self::reload(path) {
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                Ok(Self::default())
            }
            result => result,
        }
    }

    /// # Errors
    /// Reload is strict: missing, unreadable or invalid files preserve the old snapshot.
    pub fn reload(path: &Path) -> Result<Self> {
        use std::io::Read as _;
        anyhow::ensure!(
            std::fs::metadata(path)?.is_file(),
            "Lua configuration must be a regular file"
        );
        let file = std::fs::File::open(path)
            .with_context(|| format!("could not open {}", path.display()))?;
        let mut source = String::new();
        file.take(64 * 1024 + 1).read_to_string(&mut source)?;
        Self::parse(&source)
            .with_context(|| format!("invalid Lua configuration at {}", path.display()))
    }

    /// # Errors
    /// Returns Lua, action, key or chord-conflict errors.
    pub fn parse(source: &str) -> Result<Self> {
        if source.len() > 64 * 1024 {
            bail!("Lua configuration exceeds 64 KiB");
        }
        let lua = Lua::new_with(
            StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8,
            LuaOptions::default(),
        )?;
        lua.set_memory_limit(8 * 1024 * 1024)?;
        let remaining = Arc::new(AtomicUsize::new(100));
        lua.set_hook(
            HookTriggers::new().every_nth_instruction(10_000),
            move |_, _| {
                if remaining.fetch_sub(1, Ordering::Relaxed) == 0 {
                    return Err(mlua::Error::runtime(
                        "configuration instruction limit exceeded",
                    ));
                }
                Ok(VmState::Continue)
            },
        )?;
        let configuration: Configuration =
            lua.from_value(lua.load(source).set_name("config.lua").eval()?)?;
        configuration.statusline.validate()?;
        configuration.sidebar.validate()?;
        configuration.messages.validate()?;
        configuration.notifications.validate()?;
        let mut keymap = Self {
            chats: configuration.chats,
            colors: configuration.colors,
            nerd_font: configuration.nerd_font,
            statusline: configuration.statusline,
            sidebar: configuration.sidebar,
            messages: configuration.messages,
            attachments: configuration.attachments,
            notifications: configuration.notifications,
            ..Self::default()
        };
        if let Some(text) = configuration.ghost_text {
            keymap.ghost_text = text;
        }
        for binding in configuration.keymap {
            keymap.insert(binding)?;
        }
        for (index, binding) in keymap.bindings.iter().enumerate() {
            for other in &keymap.bindings[index + 1..] {
                if binding.context == other.context
                    && (binding.keys.starts_with(&other.keys)
                        || other.keys.starts_with(&binding.keys))
                {
                    bail!(
                        "ambiguous key prefix: {} / {}",
                        display_keys(&binding.keys),
                        display_keys(&other.keys)
                    );
                }
            }
        }
        Ok(keymap)
    }

    fn insert(&mut self, spec: BindingSpec) -> Result<()> {
        if spec.on.is_empty() || spec.on.len() > 4 {
            bail!("a binding must contain one to four keys");
        }
        if !(1..=9999).contains(&spec.count) {
            bail!("binding count must be between 1 and 9999");
        }
        if spec.count != 1
            && !matches!(
                spec.run.as_str(),
                "up" | "down" | "message_up" | "message_down" | "page_up" | "page_down"
            )
        {
            bail!("count is only supported for navigation actions");
        }
        let keys = spec
            .on
            .iter()
            .map(|key| {
                if !key.starts_with('<') && key.chars().count() != 1 {
                    bail!("use separate keys for a chord: {key}");
                }
                Key::from_str(key)
            })
            .collect::<Result<Vec<_>>>()?;
        let action = Action::parse(&spec.run)
            .ok_or_else(|| anyhow::anyhow!("unknown action: {}", spec.run))?;
        if let Action::Jump(alias) = &action
            && !self.chats.contains_key(alias)
        {
            bail!("unknown chat alias: {alias}");
        }
        self.bindings
            .retain(|binding| !(binding.context == spec.context && binding.keys == keys));
        if action == Action::Noop {
            return Ok(());
        }
        self.bindings.push(Binding {
            context: spec.context,
            keys,
            description: spec.desc.unwrap_or_else(|| {
                let action = action.description().to_owned();
                if spec.count == 1 {
                    action
                } else {
                    format!("{action} ×{}", spec.count)
                }
            }),
            run: action,
            count: spec.count,
        });
        Ok(())
    }

    pub fn reset(&mut self) {
        self.pending.clear();
        self.count = 0;
        self.last_key = None;
    }

    #[must_use]
    pub fn pending(&self) -> bool {
        self.last_key.is_some()
    }

    pub fn expire(&mut self) -> bool {
        if self
            .last_key
            .is_some_and(|last| last.elapsed() > Duration::from_secs(1))
        {
            self.reset();
            true
        } else {
            false
        }
    }

    pub fn feed(&mut self, context: Context, event: &KeyEvent) -> Resolution {
        if event.kind == KeyEventKind::Release {
            return Resolution::Unbound;
        }
        self.expire();
        if self.context != Some(context) {
            self.reset();
            self.context = Some(context);
        }
        let Ok(key) = Key::try_from(event.clone()) else {
            return Resolution::Unbound;
        };
        if key.code == yazi_term::event::KeyCode::Escape && self.pending() {
            self.reset();
            return Resolution::Pending(String::new());
        }
        if matches!(context, Context::Chats | Context::Conversation)
            && self.pending.is_empty()
            && let Ok(digit) = key.to_string().parse::<usize>()
            && (digit != 0 || self.count != 0)
            && (self.count != 0
                || !self.bindings.iter().any(|binding| {
                    (binding.context == Context::Global || binding.context == context)
                        && binding.keys == [key]
                }))
        {
            self.count = (self.count * 10 + digit).min(9999);
            self.last_key = Some(Instant::now());
            return Resolution::Pending(self.count.to_string());
        }
        self.pending.push(key);
        for priority in [context, Context::Global] {
            if let Some(binding) = self
                .bindings
                .iter()
                .find(|binding| binding.context == priority && binding.keys == self.pending)
            {
                let result = Resolution::Action {
                    run: binding.run.clone(),
                    count: self.count.max(1).saturating_mul(binding.count).min(9999),
                };
                self.reset();
                return result;
            }
            let candidates = self
                .bindings
                .iter()
                .filter(|binding| {
                    binding.context == priority && binding.keys.starts_with(&self.pending)
                })
                .collect::<Vec<_>>();
            if !candidates.is_empty() {
                self.last_key = Some(Instant::now());
                return Resolution::Pending(
                    candidates
                        .iter()
                        .map(|binding| {
                            format!("{} {}", display_keys(&binding.keys), binding.description)
                        })
                        .collect::<Vec<_>>()
                        .join(" · "),
                );
            }
        }
        self.reset();
        Resolution::Unbound
    }

    #[must_use]
    pub fn hint(&self, context: Context, run: &str) -> String {
        let Some(action) = Action::parse(run) else {
            return "unbound".to_owned();
        };
        self.bindings
            .iter()
            .find(|binding| binding.context == context && binding.run == action)
            .or_else(|| {
                self.bindings.iter().find(|binding| {
                    binding.context == Context::Global
                        && binding.run == action
                        && !self.bindings.iter().any(|local| {
                            local.context == context
                                && (local.keys.starts_with(&binding.keys)
                                    || binding.keys.starts_with(&local.keys))
                        })
                })
            })
            .map_or_else(
                || "unbound".to_owned(),
                |binding| display_keys(&binding.keys),
            )
    }

    #[must_use]
    pub fn help(&self) -> Vec<String> {
        self.bindings
            .iter()
            .filter(|binding| binding.run != Action::Noop)
            .map(|binding| {
                format!(
                    "{:?}  {:18} {}",
                    binding.context,
                    display_keys(&binding.keys),
                    binding.description
                )
            })
            .collect()
    }
}

fn display_keys(keys: &[Key]) -> String {
    keys.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::{Action, Context, Keymap, Resolution};
    use yazi_term::event::{KeyCode, KeyEvent, Modifiers};

    #[test]
    fn lua_overrides_hints_and_keeps_counts_out_of_the_editor() {
        let mut map = Keymap::parse(
            r"return {
            chats = { work = -1000000000042 },
            keymap = {
                { context='compose', on={'<Enter>'}, run='newline' },
                { context='compose', on={'<C-s>'}, run='send' },
                { context='conversation', on={'g','w'}, run='jump work' },
            }
        }",
        )
        .unwrap();
        assert_eq!(map.hint(Context::Compose, "send"), "<C-s>");
        let key = |c| KeyEvent::new(KeyCode::Char(c), Modifiers::empty());
        assert!(matches!(
            map.feed(Context::Conversation, &key('2')),
            Resolution::Pending(_)
        ));
        assert!(matches!(
            map.feed(Context::Conversation, &key('0')),
            Resolution::Pending(_)
        ));
        assert_eq!(
            map.feed(Context::Conversation, &key('k')),
            Resolution::Action {
                run: Action::MessageUp,
                count: 20
            }
        );
        assert_eq!(map.feed(Context::Compose, &key('2')), Resolution::Unbound);
        assert!(matches!(
            map.feed(Context::Conversation, &key('g')),
            Resolution::Pending(_)
        ));
        assert_eq!(
            map.feed(Context::Conversation, &key('w')),
            Resolution::Action {
                run: Action::Jump("work".to_owned()),
                count: 1
            }
        );
        assert!(
            map.help()
                .iter()
                .any(|line| line.contains("<C-s>") && line.ends_with("Send the current draft"))
        );
    }

    #[test]
    fn invalid_configuration_is_rejected_without_hanging() {
        assert!(Keymap::parse("return {sidebar={width=0}}").is_err());
        assert!(Keymap::parse("return {sidebar={time_color='invisible'}}").is_err());
        assert!(Keymap::parse("return {statusline={right={'mode','mode'}}}").is_err());
        assert!(Keymap::parse("return {statusline={right={'imaginary'}}}").is_err());
        assert!(
            !Keymap::parse("return {statusline={enabled=false}} ")
                .unwrap()
                .statusline
                .measures_latency()
        );
        assert!(
            !Keymap::parse("return {statusline={left={'mode'},right={'dc'}}}")
                .unwrap()
                .statusline
                .measures_latency()
        );
        assert!(Keymap::parse("while true do end").is_err());
        assert!(
            Keymap::parse("return { keymap={{context='conversation',on={'g'},run='latest'}} }")
                .is_err()
        );
        assert!(
            Keymap::parse("return { keymap={{context='conversation',on={'x'},run='unknown'}} }")
                .is_err()
        );
        for count in [0, 10_000] {
            assert!(Keymap::parse(&format!(
                "return {{keymap={{{{context='conversation',on={{'<C-u>'}},run='message_up',count={count}}}}}}}"
            )).is_err());
        }
    }

    #[test]
    fn fixed_counts_and_global_hints_follow_context_precedence() {
        let mut map = Keymap::parse(
            r"return {keymap={
            {context='conversation',on={'<C-u>'},run='message_up',count=20},
            {context='global',on={'<C-s>'},run='send'},
            {context='global',on={'g','w'},run='settings'},
            {context='compose',on={'<Enter>'},run='newline'},
            {context='input',on={'<C-s>'},run='clear'},
        }}",
        )
        .unwrap();
        assert_eq!(map.hint(Context::Compose, "send"), "<C-s>");
        assert_eq!(map.hint(Context::Input, "send"), "unbound");
        assert_eq!(map.hint(Context::Conversation, "settings"), "s");
        assert!(matches!(
            map.feed(
                Context::Conversation,
                &KeyEvent::new(KeyCode::Char('2'), Modifiers::empty())
            ),
            Resolution::Pending(_)
        ));
        assert_eq!(
            map.feed(
                Context::Conversation,
                &KeyEvent::new(KeyCode::Char('u'), Modifiers::CONTROL)
            ),
            Resolution::Action {
                run: Action::MessageUp,
                count: 40
            }
        );
        assert_eq!(
            map.feed(
                Context::Conversation,
                &KeyEvent::new(KeyCode::Char('u'), Modifiers::CONTROL)
            ),
            Resolution::Action {
                run: Action::MessageUp,
                count: 20
            }
        );
    }
}
