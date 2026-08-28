//! One typed line, and the rule that keeps every one of them the same.
//!
//! **A field here is a `String` and a keyboard reader, not a widget.** `bevy_ui` has no text
//! input, so a client that wants one reads `KeyboardInput` and decides what each press meant.
//! Chat was the only one for a long time and kept that reading inside itself; the map's note
//! is the second, and two copies of "which keys are text" is exactly the shape that drifts --
//! one of them grows a paste, or a bound, or a control-character rule, and the other does not.
//!
//! What is shared is the reading and the bound. What each caller keeps is everything that
//! makes it that field: which resource holds the line, how long it may be, what `Enter` sends
//! and what closes it.
//!
//! **The logical key, never the physical one.** `Key::Character` is what the platform's
//! layout produced, so a field types the letter on the key rather than the letter a US
//! keyboard would have there.

use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};

/// What one key press meant for the line it was typed into.
///
/// A press that meant nothing to a text field -- a modifier, a function key, an arrow --
/// answers `None` rather than a fourth variant, because "the field ignored it" and "the field
/// changed" are different things to the caller and only one of them is an edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TextEdit {
    /// The line took the press, or the bound refused it. Either way the field stays open and
    /// the caller has nothing to do but redraw.
    Typed,
    /// `Enter`. The line is finished; the caller decides what finished means.
    Submitted,
    /// `Escape`. The line is abandoned.
    Cancelled,
}

/// Applies one keyboard event to `line`, keeping it within `limit` bytes.
///
/// `limit` is in **bytes and not characters**, because every bound this client mirrors is the
/// server's and the server's are byte counts -- `MARKER_NOTE_MAX_BYTES` is 120 bytes, which is
/// forty three-byte runes. A character that would straddle the bound is refused whole, so the
/// line is never cut mid-codepoint and the field never holds something the server must reject.
pub(super) fn apply_key(key: &KeyboardInput, line: &mut String, limit: usize) -> Option<TextEdit> {
    if key.state != ButtonState::Pressed {
        return None;
    }
    match &key.logical_key {
        Key::Escape => Some(TextEdit::Cancelled),
        Key::Enter => Some(TextEdit::Submitted),
        Key::Backspace => {
            // `pop`, which takes a whole `char`: a truncation by bytes would leave the line
            // holding half of a rune, which is not a `String`.
            line.pop();
            Some(TextEdit::Typed)
        }
        Key::Space => {
            push_character(line, ' ', limit);
            Some(TextEdit::Typed)
        }
        Key::Character(text) => {
            for character in text.chars() {
                push_character(line, character, limit);
            }
            Some(TextEdit::Typed)
        }
        _ => None,
    }
}

/// Adds one character to `line` when it is text and there is room for it.
///
/// A control character is dropped rather than stored: it is not something the field could draw
/// and not something the player meant to type.
fn push_character(line: &mut String, character: char, limit: usize) {
    if character.is_control() || line.len() + character.len_utf8() > limit {
        return;
    }
    line.push(character);
}

#[cfg(test)]
mod tests {
    use super::*;

    use bevy::input::keyboard::NativeKeyCode;
    use bevy::prelude::*;

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

    fn typed(line: &mut String, limit: usize, keys: &[Key]) -> Vec<Option<TextEdit>> {
        keys.iter()
            .map(|key| apply_key(&press(key.clone()), line, limit))
            .collect()
    }

    #[test]
    fn the_keys_that_are_text_are_the_only_ones_that_change_the_line() {
        let mut line = String::new();
        let answers = typed(
            &mut line,
            32,
            &[
                Key::Character("h".into()),
                Key::Character("i".into()),
                Key::Space,
                Key::Character("there".into()),
                Key::Backspace,
                Key::Shift,
                Key::ArrowLeft,
            ],
        );
        assert_eq!(line, "hi ther");
        assert_eq!(answers[5], None, "a modifier is not an edit");
        assert_eq!(answers[6], None, "and neither is an arrow");
    }

    #[test]
    fn a_release_is_not_a_press() {
        let mut line = String::new();
        let mut key = press(Key::Character("a".into()));
        key.state = ButtonState::Released;
        assert_eq!(apply_key(&key, &mut line, 32), None);
        assert!(line.is_empty(), "a key coming back up types nothing");
    }

    #[test]
    fn enter_and_escape_are_answers_rather_than_characters() {
        let mut line = "a line".to_owned();
        assert_eq!(
            apply_key(&press(Key::Enter), &mut line, 32),
            Some(TextEdit::Submitted)
        );
        assert_eq!(
            apply_key(&press(Key::Escape), &mut line, 32),
            Some(TextEdit::Cancelled)
        );
        assert_eq!(
            line, "a line",
            "neither one edits the line it answers about"
        );
    }

    /// The bound is bytes, and a character that would straddle it is refused whole.
    #[test]
    fn the_byte_after_the_bound_is_refused_and_the_line_stays_a_string() {
        let mut line = "a".repeat(4);
        typed(&mut line, 5, &[Key::Character("b".into())]);
        assert_eq!(
            line, "aaaab",
            "the last byte there is room for still goes in"
        );
        typed(&mut line, 5, &[Key::Character("c".into())]);
        assert_eq!(line, "aaaab", "and the one after it does not");

        // Three bytes with two left: refused whole rather than cut in half.
        let mut wide = "a".repeat(3);
        typed(&mut wide, 5, &[Key::Character("\u{20ac}".into())]);
        assert_eq!(wide, "aaa");
        assert!(wide.is_char_boundary(wide.len()));
    }

    #[test]
    fn a_control_character_is_not_text() {
        let mut line = String::new();
        typed(&mut line, 32, &[Key::Character("\u{7}".into())]);
        assert!(line.is_empty(), "a bell is not something a field can draw");
    }
}
