//! The system clipboard, behind a trait a headless test can stand in for.
//!
//! **Why a trait and not the crate directly.** A test that pressed `Control+V` against the real
//! clipboard would read whatever the machine running it last copied -- and on a CI runner there
//! is no display to read it from at all. So every text field talks to [`TextClipboard`], which
//! holds a [`Clipboard`]: the game gets [`SystemClipboard`], and a test gets an in-memory one
//! in the same build. The decision to take `arboard` for the system half is
//! `docs/adr/0003-system-clipboard.md`.
//!
//! **Failures are ordinary and are said once.** No display server, a clipboard holding an image,
//! a clipboard holding nothing: each is a paste that does nothing, never a panic, and each kind
//! is logged the first time it happens rather than on every press of a key that will keep
//! failing the same way.

use bevy::prelude::*;

/// Somewhere text can be copied to and pasted from.
pub(super) trait Clipboard: Send + Sync + 'static {
    /// The text on the clipboard.
    fn read_text(&mut self) -> Result<String, ClipboardError>;
    /// Puts `text` on the clipboard.
    fn write_text(&mut self, text: &str) -> Result<(), ClipboardError>;
}

/// Why a clipboard operation did nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ClipboardError {
    /// There is no clipboard to talk to: no display server, or the platform refused.
    Unavailable(String),
    /// There is a clipboard and it holds no text.
    Empty,
}

impl ClipboardError {
    /// Which of [`TextClipboard`]'s once-only reports this failure belongs to.
    const fn kind(&self) -> usize {
        match self {
            Self::Unavailable(_) => 0,
            Self::Empty => 1,
        }
    }
}

impl From<arboard::Error> for ClipboardError {
    fn from(error: arboard::Error) -> Self {
        match error {
            // `arboard` answers this for an empty clipboard and for one holding something that
            // is not text; to a text field both are "nothing to paste".
            arboard::Error::ContentNotAvailable => Self::Empty,
            other => Self::Unavailable(other.to_string()),
        }
    }
}

/// The operating system's clipboard, opened the first time a field uses it.
///
/// **Opened lazily, and that is what keeps a headless run from touching a display.** The
/// resource is installed by `UiPlugin` in every app, tests included; nothing connects to X11
/// until a player -- or a test that asks for it -- presses a clipboard shortcut.
///
/// **Kept open once opened.** On X11 the text a program copied is served by that program for as
/// long as it holds the clipboard, so a copy made through an instance dropped straight after
/// would vanish before anything could paste it. A failed open is not remembered: the next
/// shortcut tries again, because a display that was not there can come back.
#[derive(Default)]
pub(super) struct SystemClipboard(Option<arboard::Clipboard>);

impl SystemClipboard {
    fn open(&mut self) -> Result<&mut arboard::Clipboard, ClipboardError> {
        let clipboard = match self.0.take() {
            Some(clipboard) => clipboard,
            None => arboard::Clipboard::new()?,
        };
        Ok(self.0.insert(clipboard))
    }
}

impl Clipboard for SystemClipboard {
    fn read_text(&mut self) -> Result<String, ClipboardError> {
        Ok(self.open()?.get_text()?)
    }

    fn write_text(&mut self, text: &str) -> Result<(), ClipboardError> {
        Ok(self.open()?.set_text(text)?)
    }
}

/// The clipboard every text field shares, and the rule that each kind of failure is said once.
#[derive(Resource)]
pub(super) struct TextClipboard {
    backend: Box<dyn Clipboard>,
    /// Whether each [`ClipboardError`] kind has been logged, indexed by [`ClipboardError::kind`].
    reported: [bool; 2],
}

impl TextClipboard {
    /// The operating system's clipboard, which opens nothing until it is used.
    pub(super) fn system() -> Self {
        Self::with(SystemClipboard::default())
    }

    /// A clipboard backed by `backend`.
    pub(super) fn with(backend: impl Clipboard) -> Self {
        Self {
            backend: Box::new(backend),
            reported: [false; 2],
        }
    }

    /// The text to paste, or `None` when there is none to be had.
    pub(super) fn paste(&mut self) -> Option<String> {
        match self.backend.read_text() {
            Ok(text) => Some(text),
            Err(error) => {
                self.report("paste", error);
                None
            }
        }
    }

    /// Copies `text`, answering whether it reached the clipboard.
    pub(super) fn copy(&mut self, text: &str) -> bool {
        match self.backend.write_text(text) {
            Ok(()) => true,
            Err(error) => {
                self.report("copy", error);
                false
            }
        }
    }

    /// Logs `error` the first time its kind happens, answering whether it did.
    fn report(&mut self, action: &str, error: ClipboardError) -> bool {
        let seen = &mut self.reported[error.kind()];
        if *seen {
            return false;
        }
        *seen = true;
        match error {
            ClipboardError::Unavailable(reason) => {
                warn!("the system clipboard is unavailable, so {action} does nothing: {reason}");
            }
            ClipboardError::Empty => info!("the clipboard holds no text to {action}"),
        }
        true
    }
}

/// A clipboard that lives in the process, for tests.
#[cfg(test)]
#[derive(Debug, Default)]
pub(super) struct MemoryClipboard(Option<String>);

#[cfg(test)]
impl MemoryClipboard {
    pub(super) fn holding(text: &str) -> Self {
        Self(Some(text.to_owned()))
    }
}

#[cfg(test)]
impl Clipboard for MemoryClipboard {
    fn read_text(&mut self) -> Result<String, ClipboardError> {
        self.0.clone().ok_or(ClipboardError::Empty)
    }

    fn write_text(&mut self, text: &str) -> Result<(), ClipboardError> {
        self.0 = Some(text.to_owned());
        Ok(())
    }
}

/// A clipboard that refuses everything, the way one does with no display server.
#[cfg(test)]
pub(super) struct FailingClipboard;

#[cfg(test)]
impl Clipboard for FailingClipboard {
    fn read_text(&mut self) -> Result<String, ClipboardError> {
        Err(ClipboardError::Unavailable("no display".to_owned()))
    }

    fn write_text(&mut self, _: &str) -> Result<(), ClipboardError> {
        Err(ClipboardError::Unavailable("no display".to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The system backend is behind the trait and connects to nothing when it is built.**
    /// Every app with `UiPlugin` holds one, so this is what lets the headless suite run on a
    /// runner with no display: the connection is made by the first shortcut, never by the
    /// constructor.
    #[test]
    fn the_system_clipboard_opens_nothing_until_a_field_uses_it() {
        assert!(SystemClipboard::default().0.is_none());
        let _installed = TextClipboard::system();
    }

    #[test]
    fn text_copied_is_the_text_pasted() {
        let mut clipboard = TextClipboard::with(MemoryClipboard::default());
        assert_eq!(clipboard.paste(), None, "an empty clipboard pastes nothing");
        assert!(clipboard.copy("X 40 | Z -12"));
        assert_eq!(clipboard.paste().as_deref(), Some("X 40 | Z -12"));
    }

    #[test]
    fn each_kind_of_failure_is_logged_once_and_nothing_panics() {
        let mut broken = TextClipboard::with(FailingClipboard);
        for _ in 0..3 {
            assert_eq!(broken.paste(), None);
            assert!(!broken.copy("lost"));
        }
        assert_eq!(broken.reported, [true, false]);

        let mut fresh = TextClipboard::with(MemoryClipboard::default());
        assert!(fresh.report("paste", ClipboardError::Empty));
        assert!(
            !fresh.report("paste", ClipboardError::Empty),
            "the second time is not logged"
        );
        assert!(
            fresh.report("copy", ClipboardError::Unavailable("no display".to_owned())),
            "but a different kind of failure still is"
        );
    }

    #[test]
    fn an_empty_clipboard_and_a_missing_one_are_told_apart() {
        assert_eq!(
            ClipboardError::from(arboard::Error::ContentNotAvailable),
            ClipboardError::Empty
        );
        assert!(matches!(
            ClipboardError::from(arboard::Error::ClipboardNotSupported),
            ClipboardError::Unavailable(_)
        ));
    }
}
