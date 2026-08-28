//! The settings file: where it is, what it says, and what an unreadable one costs.
//!
//! **The convention, followed rather than reused.** One file under the XDG data directory,
//! replaced by a temporary file and a rename so a half-written one is never observed — the
//! discipline `net/session.rs` established for the identity file and `net/tickets.rs`
//! reuses for the ticket cache. Those functions are `pub(in crate::net)`, and that fence
//! keeps credential paths inside `net`; this file is not a credential, so a smaller writer
//! of its own beats widening it.
//!
//! **No length rule, because this one is meant to be read.** `net/tickets.rs` treats a file
//! of the wrong length as "not a ticket" and stops there; this is the one file a player
//! might reasonably open in an editor, so it is text, every line stands alone, and a line
//! that makes no sense costs **that one setting** its default and a line in the log.
//! Nothing here panics.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use bevy::prelude::KeyCode;

use super::{Bindings, Control, Corner, Settings, key_from_name, key_name};

/// The environment variable naming the XDG data directory.
///
/// Guarded the way [`Environment::read`] is, and only because that is its one reader — a
/// name a test build has no way to look up is a name a test build has no use for.
#[cfg(not(test))]
const XDG_DATA_HOME: &str = "XDG_DATA_HOME";

/// The environment variable naming the home directory.
#[cfg(not(test))]
const HOME: &str = "HOME";

/// Where the data directory sits under a home directory when nothing names one.
const DEFAULT_DATA_HOME: &[&str] = &[".local", "share"];

/// The settings file, under the data directory.
///
/// Beside `voxelheim/identity/` and `voxelheim/account/` rather than inside either: those
/// hold one credential per server or per service, and this is one file for this client.
const SETTINGS_PATH: &[&str] = &["voxelheim", "settings"];

/// Distinguishes the temporary files two concurrent writers would otherwise both name.
static WRITE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// The environment the settings path is derived from.
///
/// Read once and passed as a value, for the reason `net/session.rs`'s copy is: mutating a
/// process environment is `unsafe` in Rust 2024, so a derivation that read it directly
/// could not be tested at all.
#[derive(Debug, Default)]
pub(super) struct Environment {
    xdg_data_home: Option<String>,
    home: Option<String>,
}

impl Environment {
    /// What this process was started with.
    ///
    /// **Absent from a test build**, for the reason `net/session.rs`'s copy is absent from
    /// one: #230 was a test suite writing into the developer's own `$XDG_DATA_HOME`, and
    /// #232 closed it by making the question uncallable rather than by remembering to
    /// inject an environment at each call site. This module arrived with a second way to
    /// ask it, which would have reopened the hole in a file nobody had looked at yet — so
    /// it carries the same `#[cfg]`, and [`default_environment`] is where the two builds
    /// differ.
    #[cfg(not(test))]
    pub(super) fn read() -> Self {
        Self {
            xdg_data_home: std::env::var(XDG_DATA_HOME).ok(),
            home: std::env::var(HOME).ok(),
        }
    }

    /// An environment whose data directory is `path`.
    #[cfg(test)]
    fn rooted_at(path: &Path) -> Self {
        Self {
            xdg_data_home: Some(path.to_string_lossy().into_owned()),
            home: None,
        }
    }
}

/// The environment the settings file is named from when nobody names a directory.
///
/// One function with two bodies, exactly as `net/session.rs` has since #232. A shipped
/// client falls back to the process environment, the only place a player's data directory
/// is written down. A test build falls back to [`Environment::default`], which names
/// neither variable — [`data_home`] answers `None` for it and [`settings_path`] therefore
/// answers `None` too, so a test that forgot to name a file reads and writes **nothing,
/// nowhere** instead of the developer's own settings. That is already a state this module
/// supports rather than a special case: `SettingsPlugin` holds `Option<PathBuf>` because an
/// environment naming no data directory is a legitimate way to run the client.
#[cfg(not(test))]
pub(super) fn default_environment() -> Environment {
    Environment::read()
}

/// See the shipped half above: under `cargo test` there is no process environment to fall
/// back to, so the fallback names nowhere.
#[cfg(test)]
pub(super) fn default_environment() -> Environment {
    Environment::default()
}

/// The XDG data directory, or `None` when the environment names none.
///
/// A relative `XDG_DATA_HOME` is ignored rather than resolved, which is what the XDG base
/// directory specification says to do with one.
fn data_home(env: &Environment) -> Option<PathBuf> {
    let xdg = env
        .xdg_data_home
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
    if let Some(xdg) = xdg {
        return Some(xdg);
    }

    let home = env
        .home
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let mut path = PathBuf::from(home);
    path.extend(DEFAULT_DATA_HOME);
    Some(path)
}

/// Where the settings live, or `None` when the environment names no data directory —
/// in which case the settings still work and simply do not survive the process.
pub(super) fn settings_path(env: &Environment) -> Option<PathBuf> {
    let mut path = data_home(env)?;
    path.extend(SETTINGS_PATH);
    Some(path)
}

/// The settings in `path`, plus a line for the log for everything that was ignored.
///
/// **Every failure is "the defaults".** An absent file is a first launch and says nothing;
/// one that cannot be opened, or holds lines nothing can read, gives the defaults and one
/// complaint per reason.
pub(super) fn load(path: &Path) -> (Settings, Vec<String>) {
    match fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => (Settings::default(), Vec::new()),
        Err(err) => (
            Settings::default(),
            vec![format!(
                "cannot read the settings in {}: {err}; using the defaults",
                path.display()
            )],
        ),
    }
}

/// Replaces `path` with `settings`, or leaves it exactly as it was.
pub(super) fn save(path: &Path, settings: &Settings) -> Result<(), String> {
    write_atomically(path, render(settings).as_bytes())
        .map_err(|err| format!("cannot save the settings in {}: {err}", path.display()))
}

/// The file this client writes for `settings`.
///
/// Deterministic, so a settings file only changes when a setting does — which is what
/// makes the round trip in the tests an equality rather than a comparison of meanings.
fn render(settings: &Settings) -> String {
    let mut out = String::new();
    out.push_str("# Voxelheim client settings. Rewritten whenever a setting changes.\n");
    out.push_str("# A line this client cannot read costs that one setting its default.\n");
    // `{}` and never a fixed number of places. Rust's `Display` for `f32` writes the
    // shortest string that reads back as the same float, where rounding to a set number of
    // places would make a reload a *different* setting from the one that was saved —
    // silently, and only for a value that landed a fraction away from a round number.
    // `every_setting_survives_a_restart` is what caught it.
    out.push_str(&format!("look-sensitivity {}\n", settings.look_sensitivity));
    out.push_str(&format!("render-distance {}\n", settings.render_distance));
    out.push_str(&format!("field-of-view {}\n", settings.field_of_view));
    out.push_str(&format!("brightness {}\n", settings.brightness));
    out.push_str(&format!("fog-start {}\n", settings.fog_start));
    out.push_str(&format!("frame-cap {}\n", settings.frame_cap));
    out.push_str(&format!("vsync {}\n", on_or_off(settings.vsync)));
    out.push_str(&format!("readout {}\n", on_or_off(settings.readout_shown)));
    out.push_str(&format!(
        "readout-corner {}\n",
        settings.readout_corner.name()
    ));
    for control in super::CONTROLS {
        // Unreachable: `Bindings::rebind` refuses a key the table does not name, and
        // every default is in it — `every_default_binding_is_a_key_this_screen_will_bind`
        // is what holds the second half. Writing the default rather than the key keeps a
        // future table change from producing a file that cannot be read back.
        let name = key_name(settings.bindings.key(control)).unwrap_or("");
        out.push_str(&format!("bind {} {name}\n", control.name()));
    }
    out
}

/// How a flag is spelled in the file.
const fn on_or_off(flag: bool) -> &'static str {
    if flag { "on" } else { "off" }
}

/// The settings `text` describes, plus a line for the log for everything ignored.
fn parse(text: &str) -> (Settings, Vec<String>) {
    let mut settings = Settings::default();
    let mut complaints = Vec::new();
    // Collected rather than applied line by line: see `Bindings::from_pairs` for why a
    // saved configuration cannot be read back one rebinding at a time.
    let mut named: Vec<(Control, KeyCode)> = Vec::new();

    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let Some(key) = fields.next() else {
            continue;
        };
        let value = fields.next().unwrap_or_default();
        let extra = fields.next();

        // The line number, never the line: echoing a file's contents into a log is a
        // habit worth not having, even where nothing secret can be in it.
        let mut refuse = |what: &str| {
            complaints.push(format!(
                "line {} of the settings file does not name {what}; using the default",
                number + 1
            ));
        };

        match key {
            "look-sensitivity" => match value.parse::<f32>() {
                Ok(parsed) if parsed.is_finite() => settings.look_sensitivity = parsed,
                _ => refuse("a mouse sensitivity"),
            },
            "render-distance" => match value.parse::<u8>() {
                Ok(parsed) => settings.render_distance = parsed,
                Err(_) => refuse("a render distance"),
            },
            "field-of-view" => match value.parse::<f32>() {
                Ok(parsed) if parsed.is_finite() => settings.field_of_view = parsed,
                _ => refuse("a field of view"),
            },
            "brightness" => match value.parse::<f32>() {
                Ok(parsed) if parsed.is_finite() => settings.brightness = parsed,
                _ => refuse("a brightness"),
            },
            "fog-start" => match value.parse::<f32>() {
                Ok(parsed) if parsed.is_finite() => settings.fog_start = parsed,
                _ => refuse("a fog start"),
            },
            "frame-cap" => match value.parse::<u16>() {
                Ok(parsed) => settings.frame_cap = parsed,
                Err(_) => refuse("a frame cap"),
            },
            "vsync" => match flag(value) {
                Some(parsed) => settings.vsync = parsed,
                None => refuse("on or off"),
            },
            "readout" => match flag(value) {
                Some(parsed) => settings.readout_shown = parsed,
                None => refuse("on or off"),
            },
            "readout-corner" => match Corner::from_name(value) {
                Some(parsed) => settings.readout_corner = parsed,
                None => refuse("a corner"),
            },
            "bind" => match (Control::from_name(value), extra.and_then(key_from_name)) {
                (Some(control), Some(bound)) => named.push((control, bound)),
                _ => refuse("a control and a key"),
            },
            _ => refuse("a setting this client knows"),
        }
    }

    // All or none, and never a partial assignment: a file that puts two controls on one
    // key describes a state the refusal rule exists to make unreachable, and there is no
    // honest way to guess which half of it the player meant.
    if !named.is_empty() {
        match Bindings::from_pairs(&named) {
            Some(bindings) => settings.bindings = bindings,
            None => complaints.push(
                "the settings file does not describe one key per control; keeping the \
                 default bindings"
                    .to_owned(),
            ),
        }
    }

    // Whatever the file said, the resource this hands back is inside every bound the
    // settings module promises its readers.
    settings.clamp();
    (settings, complaints)
}

/// The flag `value` spells, if it spells one.
fn flag(value: &str) -> Option<bool> {
    match value {
        "on" | "true" | "yes" => Some(true),
        "off" | "false" | "no" => Some(false),
        _ => None,
    }
}

/// Replaces `path` with `bytes`, or leaves it exactly as it was.
///
/// Temporary file, flush, rename — `net/session.rs`'s three steps, for its reason: without
/// the flush the directory entry can reach the disk ahead of the bytes it points at. What
/// it does *not* take is that one's `0600` and `create_new`, which belong to a bearer
/// credential rather than to a list of preferences.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;

    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} does not name a file", path.display()),
        )
    })?;
    let temporary = parent.join(format!(
        ".{}.{}.{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        WRITE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));

    let written = fs::File::create(&temporary).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    if let Err(err) = written {
        let _ = fs::remove_file(&temporary);
        return Err(err);
    }
    if let Err(err) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(err);
    }
    Ok(())
}

/// A directory of one test's own, removed when the test ends.
///
/// Hand-rolled for the reason `net/session.rs`'s is — the dependency budget is three crates
/// and `tempfile` is not one — and copied rather than borrowed because that one is
/// `pub(in crate::net)`, the same fence this module's writer works around.
#[cfg(test)]
pub(super) struct Scratch(PathBuf);

#[cfg(test)]
impl Scratch {
    pub(super) fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "voxelheim-{label}-{}-{}",
            std::process::id(),
            WRITE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("a scratch directory under the temp dir");
        Self(path)
    }

    pub(super) fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

#[cfg(test)]
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{Knob, RebindRefusal};
    use bevy::prelude::KeyCode;

    /// Settings that differ from the defaults in every field, so a round trip that lost
    /// one shows it.
    fn every_field_moved() -> Settings {
        let mut settings = Settings::default();
        settings.adjust(Knob::LookSensitivity, 4);
        settings.adjust(Knob::RenderDistance, -3);
        settings.adjust(Knob::FieldOfView, 3);
        settings.adjust(Knob::Brightness, -2);
        settings.adjust(Knob::FogStart, 2);
        settings.adjust(Knob::FrameCap, 3);
        settings.toggle_vsync();
        settings.toggle_readout();
        settings.cycle_readout_corner();
        for (control, key) in [
            (Control::Forward, KeyCode::F6),
            (Control::Menu, KeyCode::KeyG),
            (Control::Jump, KeyCode::Escape),
            (Control::Consume, KeyCode::KeyV),
        ] {
            settings.rebind(control, key).expect("a free key");
        }
        settings
    }

    #[test]
    fn every_setting_survives_a_restart() {
        let scratch = Scratch::new("settings-round-trip");
        let path = scratch.join("settings");
        let settings = every_field_moved();
        assert_ne!(settings, Settings::default(), "the fixture moved nothing");

        // Written twice, so the second is a *replacement*: `write_atomically` renames a
        // temporary over the file, and one left behind would be litter in a data directory
        // — the assertion `net/tickets.rs` makes about its own writer.
        assert_eq!(save(&path, &Settings::default()), Ok(()));
        assert_eq!(save(&path, &settings), Ok(()));
        let (reloaded, complaints) = load(&path);
        assert_eq!(complaints, Vec::<String>::new());
        assert_eq!(reloaded, settings);

        let strays: Vec<_> = fs::read_dir(scratch.join("."))
            .expect("the scratch directory")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(strays.is_empty(), "{strays:?}");
    }

    #[test]
    fn a_missing_file_is_a_first_launch_and_not_a_complaint() {
        let scratch = Scratch::new("settings-missing");
        let (settings, complaints) = load(&scratch.join("nothing"));
        assert_eq!(settings, Settings::default());
        assert_eq!(complaints, Vec::<String>::new());
    }

    #[test]
    fn a_file_nobody_can_parse_is_the_defaults_and_never_a_panic() {
        let scratch = Scratch::new("settings-corrupt");
        let path = scratch.join("settings");
        fs::write(&path, "\u{0}\u{1}not a settings file at all\n{{{\n").expect("a scratch file");

        let (settings, complaints) = load(&path);
        assert_eq!(settings, Settings::default());
        assert!(!complaints.is_empty(), "a corrupt file said nothing");
    }

    #[test]
    fn one_bad_line_costs_one_setting_and_leaves_the_rest() {
        let scratch = Scratch::new("settings-one-bad-line");
        let path = scratch.join("settings");
        fs::write(
            &path,
            "bind forward f6\nlook-sensitivity banana\nbind jump g\n",
        )
        .expect("a scratch file");

        let (settings, complaints) = load(&path);
        assert_eq!(settings.bindings().key(Control::Forward), KeyCode::F6);
        assert_eq!(settings.bindings().key(Control::Jump), KeyCode::KeyG);
        assert!(
            (settings.look_sensitivity() - Settings::default().look_sensitivity()).abs()
                < f32::EPSILON
        );
        assert_eq!(complaints.len(), 1, "{complaints:?}");
        assert!(complaints[0].contains("line 2"), "{complaints:?}");
        assert!(
            !complaints[0].contains("banana"),
            "a complaint carried the file's contents: {complaints:?}"
        );
    }

    #[test]
    fn a_hand_edited_value_outside_its_bound_is_clamped_rather_than_believed() {
        let scratch = Scratch::new("settings-out-of-range");
        let path = scratch.join("settings");

        for (line, expected) in [
            ("look-sensitivity 900", super::super::MAX_LOOK_SENSITIVITY),
            ("look-sensitivity -900", super::super::MIN_LOOK_SENSITIVITY),
        ] {
            fs::write(&path, format!("{line}\n")).expect("a scratch file");
            let (settings, _) = load(&path);
            assert!(
                (settings.look_sensitivity() - expected).abs() < f32::EPSILON,
                "{line} read back as {}",
                settings.look_sensitivity()
            );
        }

        fs::write(
            &path,
            "render-distance 255\nfield-of-view -5\nbrightness 100\nfog-start 9\n\
             frame-cap 5000\n",
        )
        .expect("a scratch file");
        let (settings, _) = load(&path);
        assert_eq!(
            settings.render_distance(),
            super::super::MAX_RENDER_DISTANCE
        );
        assert!(settings.field_of_view() >= super::super::MIN_FIELD_OF_VIEW);
        assert!(settings.brightness() <= super::super::MAX_BRIGHTNESS);
        assert!(settings.fog_start() <= super::super::MAX_FOG_START);
        assert!(settings.frame_cap() <= super::super::MAX_FRAME_CAP);
    }

    /// Zero is "uncapped" rather than the bottom of the range, so the clamp has to leave it
    /// alone — a file that says `frame-cap 0` describes a client that does not pace itself,
    /// not one asking for thirty frames a second.
    #[test]
    fn an_uncapped_frame_rate_survives_the_clamp_the_others_go_through() {
        let scratch = Scratch::new("settings-uncapped");
        let path = scratch.join("settings");
        fs::write(&path, "frame-cap 0\n").expect("a scratch file");

        let (settings, complaints) = load(&path);
        assert_eq!(complaints, Vec::<String>::new(), "{complaints:?}");
        assert_eq!(settings.frame_cap(), 0);
        assert_eq!(settings.reading(Knob::FrameCap), "uncapped");
    }

    /// A file cannot smuggle in the state the refusal rule exists to prevent, and the
    /// bindings in it are read as one assignment: all of them, or none.
    #[test]
    fn a_file_that_binds_two_controls_to_one_key_is_refused_whole() {
        let scratch = Scratch::new("settings-collision");
        let path = scratch.join("settings");
        fs::write(&path, "bind forward f6\nbind back f6\n").expect("a scratch file");

        let (settings, complaints) = load(&path);
        assert_eq!(*settings.bindings(), Settings::default().bindings().clone());
        assert_eq!(complaints.len(), 1, "{complaints:?}");

        // And nothing reachable from the screen can produce that state either.
        let mut settings = settings;
        settings
            .rebind(Control::Forward, KeyCode::F6)
            .expect("f6 is free");
        assert_eq!(
            settings.rebind(Control::Back, KeyCode::F6),
            Err(RebindRefusal::WouldUnbind(Control::Forward))
        );
    }

    #[test]
    fn a_pre_chat_settings_file_keeps_every_named_binding_when_t_was_already_used() {
        let scratch = Scratch::new("settings-before-chat");
        let path = scratch.join("settings");
        fs::write(
            &path,
            "bind forward t\nbind back s\nbind left a\nbind right d\n\
             bind jump space\nbind inventory e\nbind menu escape\n",
        )
        .expect("a scratch file");

        let (settings, complaints) = load(&path);
        assert_eq!(complaints, Vec::<String>::new(), "{complaints:?}");
        assert_eq!(settings.bindings().key(Control::Forward), KeyCode::KeyT);
        assert_eq!(settings.bindings().key(Control::Chat), KeyCode::KeyW);
        assert_eq!(settings.bindings().key(Control::Back), KeyCode::KeyS);
        assert_eq!(settings.bindings().key(Control::Inventory), KeyCode::KeyE);
        assert_eq!(settings.bindings().key(Control::Interact), KeyCode::KeyF);
        assert_eq!(settings.bindings().key(Control::Menu), KeyCode::Escape);
    }

    /// **Two controls that trade keys still load** — read one rebinding at a time, the
    /// second of the pair lands on a key the *defaults* still hold and is refused. See
    /// `Bindings::from_pairs`.
    #[test]
    fn a_saved_configuration_that_trades_two_keys_reads_back_whole() {
        let scratch = Scratch::new("settings-swap");
        let path = scratch.join("settings");
        let mut settings = Settings::default();
        settings
            .rebind(Control::Menu, KeyCode::KeyG)
            .expect("g is free");
        settings
            .rebind(Control::Jump, KeyCode::Escape)
            .expect("escape is free once the menu has left it");

        assert_eq!(save(&path, &settings), Ok(()));
        let (reloaded, complaints) = load(&path);
        assert_eq!(complaints, Vec::<String>::new(), "{complaints:?}");
        assert_eq!(reloaded.bindings().key(Control::Jump), KeyCode::Escape);
        assert_eq!(reloaded.bindings().key(Control::Menu), KeyCode::KeyG);
    }

    /// [`Control::Map`] starts on `M`, moves like any other control, and the file carries
    /// it under its own name.
    ///
    /// The render and the parse are both driven by `CONTROLS`, so this asserts the row is
    /// really there rather than that a loop exists: a control missing from the file would
    /// silently take its default again on every load.
    #[test]
    fn the_map_binding_is_written_down_and_read_back() {
        let scratch = Scratch::new("settings-map-binding");
        let path = scratch.join("settings");

        let mut settings = Settings::default();
        assert_eq!(settings.bindings().key(Control::Map), KeyCode::KeyM);
        settings
            .rebind(Control::Map, KeyCode::KeyN)
            .expect("n is bindable and free");

        assert_eq!(save(&path, &settings), Ok(()));
        let written = fs::read_to_string(&path).expect("the file that was just written");
        assert!(written.contains("bind map n"), "{written}");

        let (reloaded, complaints) = load(&path);
        assert_eq!(complaints, Vec::<String>::new(), "{complaints:?}");
        assert_eq!(reloaded.bindings().key(Control::Map), KeyCode::KeyN);
    }

    /// A settings file written before [`Control::Consume`] existed still loads, and the
    /// new control lands on its own default because nothing in that file is using it.
    ///
    /// This is the ordinary half of growing a closed control set: the file names every
    /// control it knew about, `Bindings::from_pairs` fills in the one it did not, and no
    /// complaint reaches the log because nothing about the file was wrong.
    #[test]
    fn a_pre_consume_settings_file_loads_and_the_new_control_takes_its_default() {
        let scratch = Scratch::new("settings-before-consume");
        let path = scratch.join("settings");
        fs::write(
            &path,
            "bind forward w\nbind back s\nbind left a\nbind right d\n\
             bind jump space\nbind chat t\nbind interact f\nbind inventory e\n\
             bind menu escape\n",
        )
        .expect("a scratch file");

        let (settings, complaints) = load(&path);
        assert_eq!(complaints, Vec::<String>::new(), "{complaints:?}");
        assert_eq!(settings.bindings().key(Control::Consume), KeyCode::KeyC);
        assert_eq!(*settings.bindings(), *Settings::default().bindings());
    }

    /// **The delicate half: an older file that already spent `C` keeps it.**
    ///
    /// A player who moved a control onto `C` before this one existed saved a file that is
    /// complete and correct for the set it was written against. The named binding wins —
    /// anything else would silently undo a choice the player made — and the omitted
    /// control takes the first free key the omitted-control rule in `Bindings::from_pairs`
    /// offers, which here is the key the named binding vacated. Neither control is left
    /// unreachable and neither shares a key, which is the invariant the whole set is read
    /// at once to preserve.
    #[test]
    fn an_older_file_that_already_uses_c_keeps_it_and_consume_takes_another_free_key() {
        let scratch = Scratch::new("settings-c-taken");
        let path = scratch.join("settings");
        fs::write(
            &path,
            "bind forward c\nbind back s\nbind left a\nbind right d\n\
             bind jump space\nbind chat t\nbind interact f\nbind inventory e\n\
             bind menu escape\n",
        )
        .expect("a scratch file");

        let (settings, complaints) = load(&path);
        assert_eq!(complaints, Vec::<String>::new(), "{complaints:?}");
        assert_eq!(
            settings.bindings().key(Control::Forward),
            KeyCode::KeyC,
            "the file's own binding lost to a control it had never heard of"
        );
        let consume = settings.bindings().key(Control::Consume);
        assert_ne!(consume, KeyCode::KeyC);
        assert!(
            crate::settings::key_name(consume).is_some(),
            "{consume:?} is not a key this screen offers"
        );

        // One offered key per control and one control per key, still.
        let keys: Vec<KeyCode> = crate::settings::CONTROLS
            .into_iter()
            .map(|control| settings.bindings().key(control))
            .collect();
        let mut distinct = keys.clone();
        distinct.sort_by_key(|key| crate::settings::key_name(*key).unwrap_or_default());
        distinct.dedup();
        assert_eq!(distinct.len(), keys.len(), "two controls share a key");

        // And it survives being written back out and read again.
        assert_eq!(save(&path, &settings), Ok(()));
        let (again, complaints) = load(&path);
        assert_eq!(complaints, Vec::<String>::new(), "{complaints:?}");
        assert_eq!(again, settings);
    }

    #[test]
    fn the_path_sits_under_the_data_directory_the_other_two_files_use() {
        let scratch = Scratch::new("settings-path");
        let env = Environment::rooted_at(&scratch.0);
        let path = settings_path(&env).expect("a path");
        assert!(path.ends_with("voxelheim/settings"), "{path:?}");

        // No data directory is no file, and the client still runs — it simply forgets.
        assert_eq!(settings_path(&Environment::default()), None);
    }
}
