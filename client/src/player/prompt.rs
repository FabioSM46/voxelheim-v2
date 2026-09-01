use bevy::prelude::*;

use super::InputMode;

/// One local confirmation presented by the shared yes/no widget.
///
/// The token is deliberately opaque to the widget. The controller that opened the prompt
/// keeps the meaning beside its own state and accepts only the answer carrying this token,
/// so a later confirmation can replace an earlier one without an old click answering it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Confirmation {
    token: u64,
    title: String,
    return_mode: InputMode,
}

impl Confirmation {
    pub(crate) fn title(&self) -> &str {
        &self.title
    }
}

/// The one confirmation currently on screen.
///
/// A title, two answers supplied by `ui::prompt`, and the mode an answer returns to are
/// the whole abstraction. Nothing here knows what accepting means, which keeps the widget
/// reusable instead of making the first controller to need it part of its interface.
#[derive(Resource, Debug, Default)]
pub struct ConfirmationPrompt {
    current: Option<Confirmation>,
    next_token: u64,
}

impl ConfirmationPrompt {
    pub(crate) fn current(&self) -> Option<&Confirmation> {
        self.current.as_ref()
    }

    pub(crate) fn open(&mut self, title: String, return_mode: InputMode) -> u64 {
        self.next_token = self.next_token.wrapping_add(1).max(1);
        let token = self.next_token;
        self.current = Some(Confirmation {
            token,
            title,
            return_mode,
        });
        token
    }

    /// Takes the visible prompt and turns one UI decision into a typed answer.
    pub(crate) fn answer(&mut self, accepted: bool) -> Option<(ConfirmationAnswer, InputMode)> {
        let prompt = self.current.take()?;
        Some((
            ConfirmationAnswer {
                token: prompt.token,
                accepted,
            },
            prompt.return_mode,
        ))
    }

    pub(crate) fn clear(&mut self) {
        self.current = None;
    }
}

/// An answer from the generic widget to whichever controller owns its opaque token.
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfirmationAnswer {
    pub(crate) token: u64,
    pub(crate) accepted: bool,
}
