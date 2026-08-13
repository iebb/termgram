//! Terminal key normalization and Unicode-aware single-line text editing.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const MAX_INPUT_BYTES: usize = 16 * 1024;

/// A key after terminal-specific details have been normalized.
///
/// Application modes decide what a character means. For example, `q` is a
/// quit shortcut in navigation mode, but ordinary text in a draft.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyAction {
    Quit,
    Character(char),
    Enter,
    Escape,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Newline,
    Clear,
    DeleteWord,
    Redraw,
    NextAccount,
    AddAccount,
}

/// Convert a crossterm key event into an action understood by the app.
///
/// Key releases and modified characters (except Ctrl-C) are deliberately
/// ignored. This prevents shortcuts such as Ctrl-Q from unexpectedly entering
/// text or triggering the plain `q` navigation shortcut.
#[must_use]
pub fn key_action(event: KeyEvent) -> Option<KeyAction> {
    if event.kind == KeyEventKind::Release {
        return None;
    }

    if event.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(event.code, KeyCode::Char('c' | 'C'))
    {
        return Some(KeyAction::Quit);
    }

    if event.modifiers.contains(KeyModifiers::CONTROL) {
        return match event.code {
            KeyCode::Char('j' | 'J') => Some(KeyAction::Newline),
            KeyCode::Char('a' | 'A') => Some(KeyAction::Home),
            KeyCode::Char('e' | 'E') => Some(KeyAction::End),
            KeyCode::Char('u' | 'U') => Some(KeyAction::Clear),
            KeyCode::Char('w' | 'W') => Some(KeyAction::DeleteWord),
            KeyCode::Char('l' | 'L') => Some(KeyAction::Redraw),
            _ => None,
        };
    }

    let unsupported_modifiers = KeyModifiers::CONTROL
        | KeyModifiers::ALT
        | KeyModifiers::SUPER
        | KeyModifiers::HYPER
        | KeyModifiers::META;
    if event.modifiers.intersects(unsupported_modifiers) {
        return None;
    }

    match event.code {
        KeyCode::Char(character) => Some(KeyAction::Character(character)),
        KeyCode::Enter if event.modifiers.contains(KeyModifiers::SHIFT) => Some(KeyAction::Newline),
        KeyCode::Enter => Some(KeyAction::Enter),
        KeyCode::Esc => Some(KeyAction::Escape),
        KeyCode::Tab => Some(KeyAction::Tab),
        KeyCode::BackTab => Some(KeyAction::BackTab),
        KeyCode::Backspace => Some(KeyAction::Backspace),
        KeyCode::Delete => Some(KeyAction::Delete),
        KeyCode::Left => Some(KeyAction::Left),
        KeyCode::Right => Some(KeyAction::Right),
        KeyCode::Up => Some(KeyAction::Up),
        KeyCode::Down => Some(KeyAction::Down),
        KeyCode::PageUp => Some(KeyAction::PageUp),
        KeyCode::PageDown => Some(KeyAction::PageDown),
        KeyCode::Home => Some(KeyAction::Home),
        KeyCode::End => Some(KeyAction::End),
        KeyCode::F(2) => Some(KeyAction::NextAccount),
        KeyCode::F(3) => Some(KeyAction::AddAccount),
        _ => None,
    }
}

/// A UTF-8 text buffer whose cursor always rests on a grapheme boundary.
///
/// `cursor()` is a byte offset, which makes slicing the returned `value()`
/// inexpensive. Movement and deletion operate on user-perceived characters,
/// so a combining sequence, flag, or joined emoji is never split.
#[derive(Clone, Default, Eq, PartialEq)]
pub struct TextInput {
    value: String,
    cursor: usize,
}

impl std::fmt::Debug for TextInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TextInput")
            .field("value", &self.value)
            .field("cursor", &self.cursor)
            .finish()
    }
}

impl TextInput {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            value: String::new(),
            cursor: 0,
        }
    }

    #[must_use]
    pub fn from_value(value: impl Into<String>) -> Self {
        let mut value = value.into();
        let end = grapheme_prefix_bytes(&value, MAX_INPUT_BYTES);
        value.truncate(end);
        let cursor = value.len();
        Self { value, cursor }
    }

    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Return the cursor as a UTF-8 byte offset into [`Self::value`].
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    #[must_use]
    pub fn grapheme_count(&self) -> usize {
        self.value.graphemes(true).count()
    }

    #[must_use]
    pub fn cursor_grapheme(&self) -> usize {
        self.value[..self.cursor].graphemes(true).count()
    }

    /// Terminal cell width of the text before the cursor.
    #[must_use]
    pub fn cursor_display_width(&self) -> usize {
        UnicodeWidthStr::width(&self.value[..self.cursor])
    }

    pub fn set_value(&mut self, value: impl Into<String>) {
        self.value = value.into();
        let end = grapheme_prefix_bytes(&self.value, MAX_INPUT_BYTES);
        self.value.truncate(end);
        self.cursor = self.value.len();
    }

    pub fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
    }

    /// Remove and return the complete value, resetting the cursor.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.value)
    }

    pub fn insert(&mut self, character: char) {
        let mut encoded = [0_u8; 4];
        self.insert_str(character.encode_utf8(&mut encoded));
    }

    pub fn insert_str(&mut self, text: &str) {
        let available = MAX_INPUT_BYTES.saturating_sub(self.value.len());
        if text.is_empty() || available == 0 {
            return;
        }

        let accepted_bytes = grapheme_prefix_bytes(text, available);
        let text = &text[..accepted_bytes];
        let intended_cursor = self.cursor + accepted_bytes;
        self.value.insert_str(self.cursor, text);
        self.cursor = boundary_at_or_after(&self.value, intended_cursor);
    }

    /// Delete the grapheme immediately before the cursor.
    pub fn backspace(&mut self) -> bool {
        let Some(previous) = previous_boundary(&self.value, self.cursor) else {
            return false;
        };
        self.value.drain(previous..self.cursor);
        self.cursor = previous;
        true
    }

    /// Delete the grapheme at the cursor.
    pub fn delete(&mut self) -> bool {
        let Some(next) = next_boundary(&self.value, self.cursor) else {
            return false;
        };
        self.value.drain(self.cursor..next);
        true
    }

    pub fn move_left(&mut self) -> bool {
        let Some(previous) = previous_boundary(&self.value, self.cursor) else {
            return false;
        };
        self.cursor = previous;
        true
    }

    pub fn move_right(&mut self) -> bool {
        let Some(next) = next_boundary(&self.value, self.cursor) else {
            return false;
        };
        self.cursor = next;
        true
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.value.len();
    }

    /// Delete whitespace and the word immediately before the cursor.
    pub fn delete_word_before(&mut self) -> bool {
        let original = self.cursor;
        while let Some(previous) = previous_boundary(&self.value, self.cursor) {
            let grapheme = &self.value[previous..self.cursor];
            if !grapheme.chars().all(char::is_whitespace) {
                break;
            }
            self.cursor = previous;
        }
        while let Some(previous) = previous_boundary(&self.value, self.cursor) {
            let grapheme = &self.value[previous..self.cursor];
            if grapheme.chars().all(char::is_whitespace) {
                break;
            }
            self.cursor = previous;
        }
        if self.cursor == original {
            return false;
        }
        self.value.drain(self.cursor..original);
        true
    }
}

fn grapheme_prefix_bytes(value: &str, limit: usize) -> usize {
    value
        .graphemes(true)
        .map(str::len)
        .scan(0_usize, |used, length| {
            *used = used.saturating_add(length);
            Some(*used)
        })
        .take_while(|&used| used <= limit)
        .last()
        .unwrap_or(0)
}

fn previous_boundary(value: &str, cursor: usize) -> Option<usize> {
    value[..cursor]
        .grapheme_indices(true)
        .next_back()
        .map(|(index, _)| index)
}

fn next_boundary(value: &str, cursor: usize) -> Option<usize> {
    if cursor == value.len() {
        return None;
    }

    value[cursor..]
        .grapheme_indices(true)
        .nth(1)
        .map_or(Some(value.len()), |(index, _)| Some(cursor + index))
}

fn boundary_at_or_after(value: &str, offset: usize) -> usize {
    value
        .grapheme_indices(true)
        .map(|(index, _)| index)
        .find(|&index| index >= offset)
        .unwrap_or(value.len())
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    use super::{key_action, KeyAction, TextInput};

    #[test]
    fn ctrl_c_is_quit_and_plain_q_remains_a_character() {
        assert_eq!(
            key_action(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(KeyAction::Quit)
        );
        assert_eq!(
            key_action(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
            Some(KeyAction::Character('q'))
        );
    }

    #[test]
    fn function_keys_select_account_actions() {
        assert_eq!(
            key_action(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE)),
            Some(KeyAction::NextAccount)
        );
        assert_eq!(
            key_action(KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE)),
            Some(KeyAction::AddAccount)
        );
    }

    #[test]
    fn releases_and_other_control_combinations_are_ignored() {
        let release = KeyEvent {
            code: KeyCode::Enter,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: KeyEventState::NONE,
        };
        assert_eq!(key_action(release), None);
        assert_eq!(
            key_action(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)),
            None
        );
    }

    #[test]
    fn edits_combining_graphemes_as_one_character() {
        let mut input = TextInput::from_value("a\u{301}b");
        assert_eq!(input.grapheme_count(), 2);
        assert!(input.move_left());
        assert_eq!(input.cursor(), "a\u{301}".len());
        assert!(input.backspace());
        assert_eq!(input.value(), "b");
        assert_eq!(input.cursor(), 0);
    }

    #[test]
    fn edits_joined_emoji_without_splitting_it() {
        let family = "👨‍👩‍👧‍👦";
        let mut input = TextInput::from_value(format!("{family}!"));
        assert!(input.move_home_or_false_for_test());
        assert!(input.delete());
        assert_eq!(input.value(), "!");
    }

    #[test]
    fn inserts_at_the_cursor_and_reports_terminal_width() {
        let mut input = TextInput::from_value("界b");
        input.move_home();
        assert!(input.move_right());
        input.insert('a');
        assert_eq!(input.value(), "界ab");
        assert_eq!(input.cursor_display_width(), 3);
        assert_eq!(input.cursor_grapheme(), 2);
    }

    #[test]
    fn take_resets_both_value_and_cursor() {
        let mut input = TextInput::from_value("draft");
        assert_eq!(input.take(), "draft");
        assert!(input.is_empty());
        assert_eq!(input.cursor(), 0);
    }

    #[test]
    fn input_is_bounded_without_splitting_a_grapheme() {
        let family = "👨‍👩‍👧‍👦";
        let mut input = TextInput::new();
        input.insert_str(&"a".repeat(super::MAX_INPUT_BYTES - family.len() + 1));
        input.insert_str(family);

        assert!(input.value().len() <= super::MAX_INPUT_BYTES);
        assert!(!input.value().contains(family));
        assert_eq!(input.cursor(), input.value().len());
    }

    #[test]
    fn control_bindings_are_normalized() {
        assert_eq!(
            key_action(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL)),
            Some(KeyAction::Newline)
        );
        assert_eq!(
            key_action(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL)),
            Some(KeyAction::DeleteWord)
        );
    }

    #[test]
    fn deletes_a_unicode_word_and_preceding_space() {
        let mut input = TextInput::from_value("hello 世界  ");
        assert!(input.delete_word_before());
        assert_eq!(input.value(), "hello ");
    }

    // Keeps the public movement API deliberately unit-returning for ergonomic
    // Home/End handling while making the test's intent readable.
    trait MoveHomeForTest {
        fn move_home_or_false_for_test(&mut self) -> bool;
    }

    impl MoveHomeForTest for TextInput {
        fn move_home_or_false_for_test(&mut self) -> bool {
            self.move_home();
            true
        }
    }
}
