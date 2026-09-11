//! One typed line, and the rule that keeps every one of them the same.
//!
//! **A field here is a [`TextField`] and a keyboard reader, not a widget.** `bevy_ui` has no text
//! input, so a client that wants one reads `KeyboardInput` and decides what each press meant.
//! Chat was the only one for a long time and kept that reading inside itself; the map's note
//! is the second, and two copies of "which keys are text" is exactly the shape that drifts --
//! one of them grows a cursor, or a bound, or a control-character rule, and the other does not.
//!
//! What is shared is the reading, the editing and the bound. What each caller keeps is
//! everything that makes it that field: which resource holds the line, how long it may be, what
//! `Enter` sends, what closes it, and how it is drawn.
//!
//! **The logical key, never the physical one.** `Key::Character` is what the platform's
//! layout produced, so a field types the letter on the key rather than the letter a US
//! keyboard would have there. Modifiers are the one thing read from physical keys, because
//! `Shift` and `Control` are held rather than typed and `ButtonInput<KeyCode>` is where held
//! state lives.

use std::ops::Range;

use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::input::{ButtonInput, ButtonState};
use bevy::prelude::KeyCode;

/// What one key press meant for the line it was typed into.
///
/// A press that meant nothing to a text field -- a modifier, a function key, `ArrowUp` --
/// answers `None` rather than a fourth variant, because "the field ignored it" and "the field
/// changed" are different things to the caller and only one of them is an edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TextEdit {
    /// The line took the press -- a character, a deletion, a cursor move -- or the bound
    /// refused it. Either way the field stays open and the caller has nothing to do but redraw.
    Typed,
    /// `Enter`. The line is finished; the caller decides what finished means.
    Submitted,
    /// `Escape`. The line is abandoned.
    Cancelled,
}

/// The modifiers held while a key was pressed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Modifiers {
    /// Either `Shift`: a cursor move extends the selection instead of dropping it.
    pub(super) shift: bool,
    /// Either `Control`, **with no `Alt` beside it**: a letter is a shortcut, never text.
    pub(super) control: bool,
}

impl Modifiers {
    /// Reads the held modifiers, or none for an app built without `InputPlugin`.
    ///
    /// **`Control` held together with `Alt` is not a shortcut.** On a layout with `AltGr`,
    /// some platforms report that key as `Control` plus `Alt`, and it is how a player types
    /// `@` or a bracket; reading it as `Control` would turn those characters into shortcuts
    /// that swallow them.
    pub(super) fn held(keys: Option<&ButtonInput<KeyCode>>) -> Self {
        let Some(keys) = keys else {
            return Self::default();
        };
        let alt = keys.any_pressed([KeyCode::AltLeft, KeyCode::AltRight]);
        Self {
            shift: keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]),
            control: !alt && keys.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight]),
        }
    }
}

/// One editable line: its text, where the cursor is, and what is selected.
///
/// **Every index is a byte offset on a character boundary.** The bound is in bytes (see
/// [`TextField::apply_key`]) and `String` slices by bytes, so storing characters would mean
/// converting on every edit; storing bytes means the one rule to keep is that no index ever
/// lands inside a character, and every move below steps by a whole `char`.
///
/// **The selection has no direction of its own; the cursor is always one of its ends.** The
/// other end is the anchor a `Shift` move extends from, so a `Range` plus the cursor is the
/// whole state and there is no third index that could disagree with the other two. An empty
/// selection is stored as `None`, so "something is selected" has exactly one spelling.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct TextField {
    text: String,
    cursor: usize,
    selection: Option<Range<usize>>,
}

impl TextField {
    /// The line as it has been typed.
    pub(super) fn text(&self) -> &str {
        &self.text
    }

    /// Where the next character goes, in bytes.
    pub(super) const fn cursor(&self) -> usize {
        self.cursor
    }

    /// The selected bytes, when anything is selected.
    pub(super) fn selection(&self) -> Option<Range<usize>> {
        self.selection.clone()
    }

    /// Replaces the whole line, with the cursor after it and nothing selected.
    pub(super) fn set_text(&mut self, text: &str) {
        self.text.clear();
        self.text.push_str(text);
        self.cursor = self.text.len();
        self.selection = None;
    }

    /// Empties the line.
    pub(super) fn clear(&mut self) {
        self.set_text("");
    }

    /// Empties the line and hands back what it held.
    pub(super) fn take(&mut self) -> String {
        self.cursor = 0;
        self.selection = None;
        std::mem::take(&mut self.text)
    }

    /// Applies one keyboard event, keeping the line within `limit` bytes.
    ///
    /// `limit` is in **bytes and not characters**, because every bound this client mirrors is
    /// the server's and the server's are byte counts -- `MARKER_NOTE_MAX_BYTES` is 120 bytes,
    /// which is forty three-byte runes. A character that would straddle the bound is refused
    /// whole, so the line is never cut mid-codepoint and the field never holds something the
    /// server must reject.
    pub(super) fn apply_key(
        &mut self,
        key: &KeyboardInput,
        modifiers: Modifiers,
        limit: usize,
    ) -> Option<TextEdit> {
        if key.state != ButtonState::Pressed {
            return None;
        }
        match &key.logical_key {
            Key::Escape => return Some(TextEdit::Cancelled),
            Key::Enter => return Some(TextEdit::Submitted),
            Key::Backspace => {
                if !self.delete_selection() {
                    self.delete(self.previous_boundary()..self.cursor);
                }
            }
            Key::Delete => {
                if !self.delete_selection() {
                    self.delete(self.cursor..self.next_boundary());
                }
            }
            Key::ArrowLeft => self.step(self.previous_boundary(), Edge::Start, modifiers.shift),
            Key::ArrowRight => self.step(self.next_boundary(), Edge::End, modifiers.shift),
            Key::Home => self.move_cursor(0, modifiers.shift),
            Key::End => self.move_cursor(self.text.len(), modifiers.shift),
            // A letter under `Control` is a shortcut or it is nothing: `Control+V` typing a
            // `v` is the one answer that is certainly wrong.
            Key::Character(letter) if modifiers.control => return self.shortcut(letter),
            Key::Space if modifiers.control => return None,
            Key::Space => self.insert(" ", limit),
            Key::Character(text) => self.insert(text, limit),
            _ => return None,
        }
        Some(TextEdit::Typed)
    }

    /// Answers one `Control` + letter.
    ///
    /// The letter is compared without case, because `Shift` held as well turns the logical
    /// key upper-case and `Control+Shift+A` is still select-all.
    fn shortcut(&mut self, letter: &str) -> Option<TextEdit> {
        if letter.eq_ignore_ascii_case("a") {
            self.cursor = self.text.len();
            self.selection = (!self.text.is_empty()).then_some(0..self.text.len());
            return Some(TextEdit::Typed);
        }
        None
    }

    /// Inserts `typed` at the cursor, over the selection, as far as `limit` allows.
    ///
    /// **Control characters are dropped, and the first character that does not fit ends the
    /// insertion.** A control character -- a newline, a tab, a bell -- is not something the
    /// field could draw or the player meant as text. A character with no room is not skipped
    /// in favour of a narrower one after it, because what reaches the line must be a prefix of
    /// what was typed rather than a selection from it.
    ///
    /// The selection is replaced only when something replaces it: a press that inserts nothing
    /// leaves the selected text where it was.
    fn insert(&mut self, typed: &str, limit: usize) {
        let kept = self.text.len() - self.selection.as_ref().map_or(0, ExactSizeIterator::len);
        let mut room = limit.saturating_sub(kept);
        let mut accepted = String::new();
        for character in typed.chars().filter(|character| !character.is_control()) {
            if character.len_utf8() > room {
                break;
            }
            room -= character.len_utf8();
            accepted.push(character);
        }
        if accepted.is_empty() {
            return;
        }
        self.delete_selection();
        self.text.insert_str(self.cursor, &accepted);
        self.cursor += accepted.len();
    }

    /// Removes the selection, answering whether there was one.
    fn delete_selection(&mut self) -> bool {
        let Some(selection) = self.selection.take() else {
            return false;
        };
        self.delete(selection);
        true
    }

    /// Removes `range`, leaving the cursor where it began.
    fn delete(&mut self, range: Range<usize>) {
        self.cursor = range.start;
        self.text.replace_range(range, "");
    }

    /// One arrow press: without `Shift` a selection collapses to the edge the arrow points at
    /// instead of moving past it, which is what every text field a player has used does.
    fn step(&mut self, to: usize, edge: Edge, extend: bool) {
        if !extend && let Some(selection) = self.selection.take() {
            self.cursor = match edge {
                Edge::Start => selection.start,
                Edge::End => selection.end,
            };
            return;
        }
        self.move_cursor(to, extend);
    }

    /// Puts the cursor at `to`, extending the selection from its anchor or dropping it.
    fn move_cursor(&mut self, to: usize, extend: bool) {
        let anchor = self.anchor();
        self.cursor = to;
        self.selection = (extend && anchor != to).then(|| anchor.min(to)..anchor.max(to));
    }

    /// The end of the selection the cursor is not at, or the cursor when nothing is selected.
    fn anchor(&self) -> usize {
        match &self.selection {
            Some(selection) if selection.start == self.cursor => selection.end,
            Some(selection) => selection.start,
            None => self.cursor,
        }
    }

    /// The boundary one character before the cursor, or the start of the line.
    fn previous_boundary(&self) -> usize {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index)
    }

    /// The boundary one character after the cursor, or the end of the line.
    fn next_boundary(&self) -> usize {
        self.text[self.cursor..]
            .chars()
            .next()
            .map_or(self.cursor, |character| self.cursor + character.len_utf8())
    }
}

/// Which way an arrow points, for collapsing a selection onto the matching edge.
#[derive(Clone, Copy)]
enum Edge {
    Start,
    End,
}

#[cfg(test)]
mod tests {
    use super::*;

    use bevy::input::keyboard::NativeKeyCode;
    use bevy::prelude::*;

    const SHIFT: Modifiers = Modifiers {
        shift: true,
        control: false,
    };
    const CONTROL: Modifiers = Modifiers {
        shift: false,
        control: true,
    };

    fn press(key: Key) -> KeyboardInput {
        KeyboardInput {
            key_code: KeyCode::Unidentified(NativeKeyCode::Unidentified),
            logical_key: key,
            state: ButtonState::Pressed,
            text: None,
            repeat: false,
            window: Entity::PLACEHOLDER,
        }
    }

    fn typed(field: &mut TextField, limit: usize, keys: &[Key]) -> Vec<Option<TextEdit>> {
        held(field, Modifiers::default(), limit, keys)
    }

    fn held(
        field: &mut TextField,
        modifiers: Modifiers,
        limit: usize,
        keys: &[Key],
    ) -> Vec<Option<TextEdit>> {
        keys.iter()
            .map(|key| field.apply_key(&press(key.clone()), modifiers, limit))
            .collect()
    }

    fn field(text: &str) -> TextField {
        let mut field = TextField::default();
        field.set_text(text);
        field
    }

    fn character(text: &str) -> Key {
        Key::Character(text.into())
    }

    #[test]
    fn the_keys_that_are_text_are_the_only_ones_that_change_the_line() {
        let mut line = TextField::default();
        let answers = typed(
            &mut line,
            32,
            &[
                character("h"),
                character("i"),
                Key::Space,
                character("there"),
                Key::Backspace,
                Key::Shift,
                Key::ArrowUp,
                Key::F1,
            ],
        );
        assert_eq!(line.text(), "hi ther");
        assert_eq!(answers[5], None, "a modifier is not an edit");
        assert_eq!(answers[6], None, "ArrowUp stays history-agnostic here");
        assert_eq!(answers[7], None, "and a function key is nothing to a field");
    }

    #[test]
    fn a_release_is_not_a_press() {
        let mut line = TextField::default();
        let mut key = press(character("a"));
        key.state = ButtonState::Released;
        assert_eq!(line.apply_key(&key, Modifiers::default(), 32), None);
        assert!(line.text().is_empty(), "a key coming back up types nothing");
    }

    #[test]
    fn enter_and_escape_are_answers_rather_than_characters() {
        let mut line = field("a line");
        assert_eq!(
            typed(&mut line, 32, &[Key::Enter, Key::Escape]),
            [Some(TextEdit::Submitted), Some(TextEdit::Cancelled)]
        );
        assert_eq!(
            line.text(),
            "a line",
            "neither one edits the line it answers about"
        );
    }

    /// The bound is bytes, and a character that would straddle it is refused whole.
    #[test]
    fn the_byte_after_the_bound_is_refused_and_the_line_stays_a_string() {
        let mut line = field(&"a".repeat(4));
        typed(&mut line, 5, &[character("b")]);
        assert_eq!(
            line.text(),
            "aaaab",
            "the last byte there is room for still goes in"
        );
        typed(&mut line, 5, &[character("c")]);
        assert_eq!(line.text(), "aaaab", "and the one after it does not");

        // Three bytes with two left: refused whole rather than cut in half.
        let mut wide = field(&"a".repeat(3));
        typed(&mut wide, 5, &[character("\u{20ac}")]);
        assert_eq!(wide.text(), "aaa");
        assert_eq!(wide.cursor(), 3);
    }

    /// What reaches the line is a prefix of what was typed, never a selection from it.
    #[test]
    fn a_character_with_no_room_ends_the_insertion() {
        let mut line = TextField::default();
        typed(&mut line, 3, &[character("ab\u{20ac}c")]);
        assert_eq!(
            line.text(),
            "ab",
            "the euro has no room and the c after it waits"
        );
    }

    #[test]
    fn a_control_character_is_not_text() {
        let mut line = TextField::default();
        typed(&mut line, 32, &[character("a\u{7}\nb\t")]);
        assert_eq!(line.text(), "ab", "a bell, a newline and a tab are dropped");
    }

    #[test]
    fn arrows_move_by_whole_characters_and_stop_at_the_ends() {
        // "a", a two-byte e-acute, a three-byte euro, "b": boundaries at 0, 1, 3, 6 and 7.
        let mut line = field("a\u{e9}\u{20ac}b");
        let mut seen = vec![line.cursor()];
        for _ in 0..5 {
            typed(&mut line, 32, &[Key::ArrowLeft]);
            seen.push(line.cursor());
        }
        assert_eq!(seen, [7, 6, 3, 1, 0, 0], "the last press finds the start");
        for _ in 0..5 {
            typed(&mut line, 32, &[Key::ArrowRight]);
        }
        assert_eq!(line.cursor(), 7, "and the end holds the same way");
        assert!(line.selection().is_none(), "no Shift, no selection");
    }

    #[test]
    fn typing_and_deleting_happen_at_the_cursor() {
        let mut line = field("held");
        typed(
            &mut line,
            32,
            &[Key::Home, Key::ArrowRight, Key::ArrowRight],
        );
        typed(&mut line, 32, &[character("l")]);
        assert_eq!(line.text(), "helld");
        assert_eq!(line.cursor(), 3);

        typed(&mut line, 32, &[Key::Delete]);
        assert_eq!(
            line.text(),
            "held",
            "Delete takes the character after the cursor"
        );
        typed(&mut line, 32, &[Key::Backspace]);
        assert_eq!(line.text(), "hed", "and Backspace the one before it");
        assert_eq!(line.cursor(), 2);

        typed(
            &mut line,
            32,
            &[Key::End, Key::Delete, Key::Home, Key::Backspace],
        );
        assert_eq!(
            line.text(),
            "hed",
            "neither reaches past its end of the line"
        );
    }

    #[test]
    fn shift_extends_the_selection_from_its_anchor_and_back_again() {
        let mut line = field("northwest");
        held(&mut line, SHIFT, 32, &[const { Key::ArrowLeft }; 4]);
        assert_eq!(line.selection(), Some(5..9));
        assert_eq!(line.cursor(), 5, "the cursor is the end that moved");

        held(&mut line, SHIFT, 32, &[const { Key::ArrowRight }; 4]);
        assert_eq!(
            line.selection(),
            None,
            "back onto the anchor is nothing selected"
        );

        held(&mut line, SHIFT, 32, &[Key::ArrowRight]);
        assert_eq!(line.selection(), None, "the anchor was the end of the line");

        held(&mut line, SHIFT, 32, &[Key::Home]);
        assert_eq!(line.selection(), Some(0..9));
        held(&mut line, SHIFT, 32, &[Key::End]);
        assert_eq!(
            line.selection(),
            None,
            "Shift+End returns to the same anchor"
        );
    }

    #[test]
    fn an_arrow_without_shift_collapses_the_selection_onto_its_edge() {
        let mut line = field("northwest");
        held(&mut line, SHIFT, 32, &[Key::Home]);
        typed(&mut line, 32, &[Key::ArrowRight]);
        assert_eq!(
            (line.cursor(), line.selection()),
            (9, None),
            "right to the end"
        );

        held(&mut line, SHIFT, 32, &[Key::ArrowLeft, Key::ArrowLeft]);
        typed(&mut line, 32, &[Key::ArrowLeft]);
        assert_eq!(
            (line.cursor(), line.selection()),
            (7, None),
            "left to the start"
        );

        held(&mut line, SHIFT, 32, &[Key::End]);
        typed(&mut line, 32, &[Key::Home]);
        assert_eq!(
            (line.cursor(), line.selection()),
            (0, None),
            "Home drops it too"
        );
    }

    #[test]
    fn control_a_selects_everything_and_the_next_edit_acts_on_it() {
        let mut line = field("cold here");
        assert_eq!(
            held(&mut line, CONTROL, 32, &[character("a")]),
            [Some(TextEdit::Typed)]
        );
        assert_eq!(line.selection(), Some(0..9));
        typed(&mut line, 32, &[character("warm")]);
        assert_eq!(
            (line.text(), line.cursor()),
            ("warm", 4),
            "typing replaces it"
        );

        for key in [Key::Backspace, Key::Delete] {
            let mut line = field("cold here");
            held(&mut line, CONTROL, 32, &[character("A")]);
            typed(&mut line, 32, std::slice::from_ref(&key));
            assert_eq!(
                line.text(),
                "",
                "{key:?} removes the selection, not one character"
            );
        }

        let mut empty = TextField::default();
        held(&mut empty, CONTROL, 32, &[character("a")]);
        assert_eq!(
            empty.selection(),
            None,
            "an empty line has nothing to select"
        );
    }

    #[test]
    fn a_selection_frees_its_bytes_for_what_replaces_it() {
        let mut line = field("abcde");
        held(&mut line, SHIFT, 5, &[const { Key::ArrowLeft }; 3]);
        typed(&mut line, 5, &[character("\u{20ac}")]);
        assert_eq!(line.text(), "ab\u{20ac}", "three bytes out, three bytes in");

        let mut full = field("abcde");
        held(&mut full, SHIFT, 5, &[Key::ArrowLeft]);
        typed(&mut full, 5, &[character("\u{20ac}"), character("\u{7}")]);
        assert_eq!(
            full.text(),
            "abcde",
            "nothing that fits, so nothing is replaced"
        );
        assert_eq!(full.selection(), Some(4..5));
    }

    #[test]
    fn a_letter_under_control_is_never_text() {
        let mut line = field("x");
        let answers = held(&mut line, CONTROL, 32, &[character("v"), Key::Space]);
        assert_eq!(answers, [None, None]);
        assert_eq!(line.text(), "x");
    }

    #[test]
    fn replacing_or_taking_the_line_resets_the_cursor_and_the_selection() {
        let mut line = field("first");
        held(&mut line, SHIFT, 32, &[Key::Home]);
        line.set_text("second");
        assert_eq!((line.cursor(), line.selection()), (6, None));

        held(&mut line, SHIFT, 32, &[Key::Home]);
        assert_eq!(line.take(), "second");
        assert_eq!(line, TextField::default());

        line.set_text("third");
        line.clear();
        assert_eq!(line, TextField::default());
    }

    #[test]
    fn modifiers_are_read_from_held_keys_and_altgr_is_not_control() {
        assert_eq!(Modifiers::held(None), Modifiers::default());

        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::ShiftRight);
        keys.press(KeyCode::ControlLeft);
        assert_eq!(
            Modifiers::held(Some(&keys)),
            Modifiers {
                shift: true,
                control: true
            }
        );

        keys.press(KeyCode::AltRight);
        assert!(
            !Modifiers::held(Some(&keys)).control,
            "Control with Alt is how some layouts spell AltGr"
        );
    }
}
