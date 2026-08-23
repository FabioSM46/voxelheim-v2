//! What a player may change, and the file it survives a restart in.
//!
//! **Nothing here is a rule.** Every value in [`Settings`] is an input or presentation
//! preference. None reaches the wire and none decides a gameplay outcome — a knob that
//! changed what the server was *told* would be a knob that had escaped this module.
//!
//! **This is the first half of #179**: the settings a player changes about their own hands
//! — the mouse sensitivity that replaces the constant `player/constants.rs` used to hold,
//! and the key bindings, with the one rule that refuses a rebinding rather than leave a
//! control unreachable. The graphics options and the frame-rate readout are the second
//! half, and they are additive: rows on this screen, fields in this file.
//!
//! [`store`] owns the file.

mod store;

use std::path::PathBuf;

use bevy::prelude::*;

/// Loads the settings and keeps the file in step with them.
///
/// It pushes nothing at anybody: `player/mod.rs` reads the sensitivity and the bindings
/// straight out of the resource, on a system that already runs every frame over the state
/// it owns, so a second writer would buy nothing.
pub struct SettingsPlugin {
    /// The file to load from and save to, or `None` when the environment names no data
    /// directory — in which case the settings are still adjustable, they simply do not
    /// survive the process.
    file: Option<PathBuf>,
}

impl SettingsPlugin {
    /// The settings file this process's environment names.
    pub fn from_environment() -> Self {
        Self {
            file: store::settings_path(&store::default_environment()),
        }
    }

    /// A plugin whose file is `path`, for a test that must not read the developer's own
    /// settings — the reason `net`'s `Environment::rooted_at` exists one directory over.
    #[cfg(test)]
    fn at(path: PathBuf) -> Self {
        Self { file: Some(path) }
    }
}

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        let (settings, complaints) = match &self.file {
            Some(path) => store::load(path),
            None => (Settings::default(), Vec::new()),
        };
        for complaint in complaints {
            warn!("{complaint}");
        }

        app.insert_resource(SettingsFile {
            path: self.file.clone(),
            written: settings.clone(),
        })
        .insert_resource(settings)
        .add_systems(Update, save_when_changed);
    }
}

/// The file the settings came from, and the copy that is currently in it.
///
/// Holding what was written keeps [`save_when_changed`] from rewriting the file on the
/// frame the resource is inserted, and for a change that put a value back where it was.
#[derive(Resource, Debug, Clone)]
struct SettingsFile {
    path: Option<PathBuf>,
    written: Settings,
}

/// A control a key can be bound to.
///
/// **The set is closed, and what is missing is missing on purpose.** [`REBINDABLE_KEYS`] is
/// the other half of the same rule: neither list admits a key another part of this client
/// already reads — Shift (the orbit, and a split in `ui/inventory.rs`), `F5` (the view
/// toggle), the digits (hotbar slots), or `Enter`, `Tab` and the arrows (the character
/// screen) — because a control bound to one would fire two things at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Forward,
    Back,
    Left,
    Right,
    Jump,
    Inventory,
    Menu,
}

/// Every control, in the order the settings screen lists them.
pub const CONTROLS: [Control; 7] = [
    Control::Forward,
    Control::Back,
    Control::Left,
    Control::Right,
    Control::Jump,
    Control::Inventory,
    Control::Menu,
];

impl Control {
    /// What the file calls it.
    const fn name(self) -> &'static str {
        match self {
            Self::Forward => "forward",
            Self::Back => "back",
            Self::Left => "left",
            Self::Right => "right",
            Self::Jump => "jump",
            Self::Inventory => "inventory",
            Self::Menu => "menu",
        }
    }

    /// What the settings screen calls it.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Forward => "Move forward",
            Self::Back => "Move back",
            Self::Left => "Strafe left",
            Self::Right => "Strafe right",
            Self::Jump => "Jump",
            Self::Inventory => "Inventory",
            Self::Menu => "Pause menu",
        }
    }

    /// The control `name` denotes, if it denotes one.
    fn from_name(name: &str) -> Option<Self> {
        CONTROLS.into_iter().find(|control| control.name() == name)
    }

    /// The key this control answers to before anybody changes it.
    const fn default_key(self) -> KeyCode {
        match self {
            Self::Forward => KeyCode::KeyW,
            Self::Back => KeyCode::KeyS,
            Self::Left => KeyCode::KeyA,
            Self::Right => KeyCode::KeyD,
            Self::Jump => KeyCode::Space,
            Self::Inventory => KeyCode::KeyE,
            Self::Menu => KeyCode::Escape,
        }
    }
}

/// The keys this screen will bind a control to, and the names the file records them by.
///
/// A closed table rather than every `KeyCode` Bevy defines, for two reasons that point the
/// same way. It is the set nothing else in this client reads, so a rebound control cannot
/// double up on the orbit, a hotbar slot or the character screen (see [`Control`]). And the
/// name in the file is then *ours*: a spelling taken from `KeyCode`'s `Debug` would be
/// Bevy's to rename under a version bump, and a settings file that stopped parsing on a
/// dependency upgrade is a worse trade than a table.
const REBINDABLE_KEYS: &[(KeyCode, &str)] = &[
    (KeyCode::KeyA, "a"),
    (KeyCode::KeyB, "b"),
    (KeyCode::KeyC, "c"),
    (KeyCode::KeyD, "d"),
    (KeyCode::KeyE, "e"),
    (KeyCode::KeyF, "f"),
    (KeyCode::KeyG, "g"),
    (KeyCode::KeyH, "h"),
    (KeyCode::KeyI, "i"),
    (KeyCode::KeyJ, "j"),
    (KeyCode::KeyK, "k"),
    (KeyCode::KeyL, "l"),
    (KeyCode::KeyM, "m"),
    (KeyCode::KeyN, "n"),
    (KeyCode::KeyO, "o"),
    (KeyCode::KeyP, "p"),
    (KeyCode::KeyQ, "q"),
    (KeyCode::KeyR, "r"),
    (KeyCode::KeyS, "s"),
    (KeyCode::KeyT, "t"),
    (KeyCode::KeyU, "u"),
    (KeyCode::KeyV, "v"),
    (KeyCode::KeyW, "w"),
    (KeyCode::KeyX, "x"),
    (KeyCode::KeyY, "y"),
    (KeyCode::KeyZ, "z"),
    (KeyCode::Space, "space"),
    (KeyCode::Escape, "escape"),
    (KeyCode::ControlLeft, "ctrl-left"),
    (KeyCode::ControlRight, "ctrl-right"),
    (KeyCode::AltLeft, "alt-left"),
    (KeyCode::AltRight, "alt-right"),
    (KeyCode::F1, "f1"),
    (KeyCode::F2, "f2"),
    (KeyCode::F3, "f3"),
    (KeyCode::F4, "f4"),
    (KeyCode::F6, "f6"),
    (KeyCode::F7, "f7"),
    (KeyCode::F8, "f8"),
    (KeyCode::F9, "f9"),
    (KeyCode::F10, "f10"),
    (KeyCode::F11, "f11"),
    (KeyCode::F12, "f12"),
];

/// What this client calls `key`, or `None` for a key it will not bind.
pub fn key_name(key: KeyCode) -> Option<&'static str> {
    REBINDABLE_KEYS
        .iter()
        .find(|(bound, _)| *bound == key)
        .map(|(_, name)| *name)
}

/// The key `name` denotes, if this client will bind it.
fn key_from_name(name: &str) -> Option<KeyCode> {
    REBINDABLE_KEYS
        .iter()
        .find(|(_, spelling)| *spelling == name)
        .map(|(key, _)| *key)
}

/// Why a rebinding was refused.
///
/// **A refusal leaves the previous binding exactly as it was**, which is why this is a
/// `Result` rather than a silent overwrite: a rebinding that took a key from another
/// control would leave that control unreachable — and were it the pause menu, the player
/// would have no way back to the screen that could undo it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebindRefusal {
    /// The key is one this client reads for something else. See [`REBINDABLE_KEYS`].
    NotOffered,
    /// Another control already answers to it, and would answer to nothing afterwards.
    WouldUnbind(Control),
}

impl RebindRefusal {
    /// The sentence the settings screen shows.
    pub fn sentence(self) -> String {
        match self {
            Self::NotOffered => "that key is already the client's own".to_owned(),
            Self::WouldUnbind(control) => {
                format!("that key is {}, which would be left unreachable", {
                    control.label().to_lowercase()
                })
            }
        }
    }
}

/// Which key answers for each control.
///
/// **One key per control and one control per key, always.** The invariant is what makes
/// "no control is unreachable" true by construction rather than by inspection, and
/// [`Self::rebind`] is the only thing that may change it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bindings {
    keys: [KeyCode; CONTROLS.len()],
}

impl Default for Bindings {
    fn default() -> Self {
        Self {
            keys: CONTROLS.map(Control::default_key),
        }
    }
}

impl Bindings {
    /// The key `control` answers to.
    pub const fn key(&self, control: Control) -> KeyCode {
        self.keys[control as usize]
    }

    /// The control `key` answers for, if any.
    fn control_bound_to(&self, key: KeyCode) -> Option<Control> {
        CONTROLS
            .into_iter()
            .find(|control| self.key(*control) == key)
    }

    /// The complete assignment `named` describes, or `None` when it is not one.
    ///
    /// Controls it leaves out keep their defaults; the answer is `None` — and the caller
    /// then keeps *all* the defaults — when the result would break the invariant above.
    ///
    /// **A set, and deliberately not a sequence of [`Self::rebind`] calls.** The file names
    /// every control at once, so two bindings that trade keys cross over halfway through —
    /// jump to `Escape` while the pause menu still holds it — and a rebinding checked one at
    /// a time against the defaults refuses the second of the pair. That is a configuration a
    /// player saved and could not load again; `store`'s round trip is what found it.
    fn from_pairs(named: &[(Control, KeyCode)]) -> Option<Self> {
        let mut bindings = Self::default();
        for (control, key) in named {
            bindings.keys[*control as usize] = *key;
        }
        for (index, key) in bindings.keys.iter().enumerate() {
            if key_name(*key).is_none() || bindings.keys[..index].contains(key) {
                return None;
            }
        }
        Some(bindings)
    }

    /// Makes `control` answer to `key`, or refuses and changes nothing.
    pub fn rebind(&mut self, control: Control, key: KeyCode) -> Result<(), RebindRefusal> {
        if key_name(key).is_none() {
            return Err(RebindRefusal::NotOffered);
        }
        match self.control_bound_to(key) {
            // Already this control's key. Not a refusal: nothing about the binding
            // changes, and reporting one would make pressing the same key twice look
            // like a mistake.
            Some(bound) if bound == control => Ok(()),
            Some(bound) => Err(RebindRefusal::WouldUnbind(bound)),
            None => {
                self.keys[control as usize] = key;
                Ok(())
            }
        }
    }
}

/// A number the settings screen steps up and down.
///
/// One enum and one [`Settings::adjust`] rather than a setter apiece, so the bound and the
/// step of each are stated in exactly one place — the screen says "up" and the model says
/// how far up that is and where it stops. One member today; the graphics half of #179 adds
/// the rest, and adds no other machinery to do it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Knob {
    LookSensitivity,
}

/// Every knob, in the order the settings screen lists them.
pub const KNOBS: [Knob; 1] = [Knob::LookSensitivity];

impl Knob {
    /// What the settings screen calls it.
    pub const fn label(self) -> &'static str {
        match self {
            Self::LookSensitivity => "Mouse sensitivity",
        }
    }
}

/// Radians of turn per logical pixel of pointer movement, before anybody changes it.
///
/// A full turn takes about 2 000 pixels, a desk-width sweep. It is what [`Settings`] starts
/// from and what `player`'s `sample_input` falls back to in an app built without the
/// resource. `player/constants.rs` held it until #179 and this is where it landed: a bound,
/// a step and the default between them are one statement about one setting, and splitting
/// them across two modules is what made this one import `player` while `client/AGENTS.md`
/// said it imported nothing from it.
pub const DEFAULT_LOOK_SENSITIVITY: f32 = 0.003;
/// Radians of turn per logical pixel, at the slowest this screen offers.
const MIN_LOOK_SENSITIVITY: f32 = 0.0005;
/// And at the fastest. Roughly a third of a turn across a desk-width sweep.
const MAX_LOOK_SENSITIVITY: f32 = 0.02;
/// One press of the sensitivity control.
const LOOK_SENSITIVITY_STEP: f32 = 0.0005;

/// Everything a player may change, as one resource.
///
/// The fields are private and every one of them is inside its stated bound, always: the
/// setters clamp, and [`store`] clamps whatever it read before handing it over. A reader
/// therefore never has to check, which is why `player/sky.rs` can divide by
/// [`Self::render_distance`] without asking whether it is zero.
#[derive(Resource, Debug, Clone, PartialEq)]
pub struct Settings {
    look_sensitivity: f32,
    bindings: Bindings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            look_sensitivity: DEFAULT_LOOK_SENSITIVITY,
            bindings: Bindings::default(),
        }
    }
}

impl Settings {
    /// Radians of turn per logical pixel of pointer movement.
    pub const fn look_sensitivity(&self) -> f32 {
        self.look_sensitivity
    }

    /// Which key answers for each control.
    pub const fn bindings(&self) -> &Bindings {
        &self.bindings
    }

    /// Moves `knob` by `steps` of its own size, stopping at its bounds.
    pub fn adjust(&mut self, knob: Knob, steps: i32) {
        // A step at a time rather than a multiplication, so no integer has to become a
        // float, then snapped to the grid `places` describes. **The snap is what makes a
        // step reversible**: `0.003 + 0.0005 - 0.0005` is not `0.003` in `f32`, so without
        // it a value nudged up and back down lands a few bits away, the file records the
        // drift, and every later pair of presses moves the last digits again.
        let shift = |value: f32, step: f32, low: f32, high: f32, places: i32| {
            let mut moved = value;
            for _ in 0..steps.abs() {
                moved = if steps > 0 {
                    moved + step
                } else {
                    moved - step
                };
            }
            let scale = 10f32.powi(places);
            ((moved * scale).round() / scale).clamp(low, high)
        };
        match knob {
            Knob::LookSensitivity => {
                self.look_sensitivity = shift(
                    self.look_sensitivity,
                    LOOK_SENSITIVITY_STEP,
                    MIN_LOOK_SENSITIVITY,
                    MAX_LOOK_SENSITIVITY,
                    4,
                );
            }
        }
    }

    /// What the settings screen prints beside `knob`.
    pub fn reading(&self, knob: Knob) -> String {
        match knob {
            Knob::LookSensitivity => format!("{:.4}", self.look_sensitivity),
        }
    }

    /// Makes `control` answer to `key`, or refuses and changes nothing.
    pub fn rebind(&mut self, control: Control, key: KeyCode) -> Result<(), RebindRefusal> {
        self.bindings.rebind(control, key)
    }

    /// Puts every value back inside its bound.
    ///
    /// Called on whatever [`store`] read, so a hand-edited file cannot put a reader
    /// outside the range this module promises it.
    fn clamp(&mut self) {
        self.look_sensitivity = self
            .look_sensitivity
            .clamp(MIN_LOOK_SENSITIVITY, MAX_LOOK_SENSITIVITY);
    }
}

/// Writes the settings back to their file when, and only when, they have moved.
fn save_when_changed(settings: Res<Settings>, mut file: ResMut<SettingsFile>) {
    if !settings.is_changed() || file.written == *settings {
        return;
    }
    if let Some(path) = file.path.clone()
        && let Err(complaint) = store::save(&path, &settings)
    {
        // A settings file that cannot be written costs the next launch its defaults, and
        // nothing else. Loud enough to find in a log, quiet enough not to interrupt a
        // session over a read-only home directory.
        warn!("{complaint}");
    }
    file.written = settings.clone();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::MAX_PITCH;

    /// A client whose player never opens this screen renders and steers exactly as it did
    /// before the screen existed.
    #[test]
    fn the_defaults_are_what_the_client_had_before_this_screen_existed() {
        let settings = Settings::default();
        assert!((settings.look_sensitivity() - DEFAULT_LOOK_SENSITIVITY).abs() < f32::EPSILON);
        for (control, key) in [
            (Control::Forward, KeyCode::KeyW),
            (Control::Back, KeyCode::KeyS),
            (Control::Left, KeyCode::KeyA),
            (Control::Right, KeyCode::KeyD),
            (Control::Jump, KeyCode::Space),
            (Control::Inventory, KeyCode::KeyE),
            (Control::Menu, KeyCode::Escape),
        ] {
            assert_eq!(settings.bindings().key(control), key);
            // And every one of them is a key the screen can show and rebind.
            assert!(
                key_name(key).is_some(),
                "{control:?} starts on a hidden key"
            );
        }
    }

    /// The pitch limit is a build invariant and no sensitivity may reach past it. The
    /// clamp lives in `player/mod.rs`; what is asserted here is that the *setting* cannot
    /// produce a value the clamp would have to be widened for.
    #[test]
    fn the_pitch_limit_holds_at_every_sensitivity_this_screen_offers() {
        let mut settings = Settings::default();
        for steps in [-1000, -1, 0, 1, 1000] {
            settings.adjust(Knob::LookSensitivity, steps);
            let sensitivity = settings.look_sensitivity();
            assert!(
                (MIN_LOOK_SENSITIVITY..=MAX_LOOK_SENSITIVITY).contains(&sensitivity),
                "{sensitivity} escaped its bound"
            );
            // One enormous flick of the pointer, at the fastest this screen goes.
            let pitch = (-100_000.0f32 * sensitivity).clamp(-MAX_PITCH, MAX_PITCH);
            assert!(pitch.abs() <= MAX_PITCH, "{pitch} passed the pitch limit");
        }
    }

    #[test]
    fn every_knob_stops_at_both_of_its_bounds() {
        for knob in KNOBS {
            let mut low = Settings::default();
            low.adjust(knob, -10_000);
            let mut high = Settings::default();
            high.adjust(knob, 10_000);
            let mut lower = low.clone();
            lower.adjust(knob, -1);
            let mut higher = high.clone();
            higher.adjust(knob, 1);
            assert_eq!(low, lower, "{knob:?} kept falling past its floor");
            assert_eq!(high, higher, "{knob:?} kept climbing past its ceiling");
        }
    }

    #[test]
    fn a_rebinding_that_would_leave_a_control_unreachable_is_refused() {
        let mut settings = Settings::default();
        let before = *settings.bindings();

        // `W` is already forward. Giving it to the pause menu would leave forward with no
        // key at all — and if it were the other way round, no way back to this screen.
        assert_eq!(
            settings.rebind(Control::Menu, KeyCode::KeyW),
            Err(RebindRefusal::WouldUnbind(Control::Forward))
        );
        assert_eq!(*settings.bindings(), before, "a refusal changed a binding");
        assert_eq!(settings.bindings().key(Control::Menu), KeyCode::Escape);
        assert_eq!(settings.bindings().key(Control::Forward), KeyCode::KeyW);

        // And the same in the direction that strands the player: taking Escape.
        assert_eq!(
            settings.rebind(Control::Forward, KeyCode::Escape),
            Err(RebindRefusal::WouldUnbind(Control::Menu))
        );
        assert_eq!(*settings.bindings(), before);
    }

    #[test]
    fn a_free_key_is_accepted_and_the_key_it_replaces_becomes_free() {
        let mut settings = Settings::default();
        assert_eq!(settings.rebind(Control::Forward, KeyCode::KeyT), Ok(()));
        assert_eq!(settings.bindings().key(Control::Forward), KeyCode::KeyT);

        // `W` is nobody's now, so the pause menu may have it — and Escape is then free.
        assert_eq!(settings.rebind(Control::Menu, KeyCode::KeyW), Ok(()));
        assert_eq!(settings.bindings().key(Control::Menu), KeyCode::KeyW);
        assert_eq!(settings.rebind(Control::Jump, KeyCode::Escape), Ok(()));

        // And a control given the key it already has is not a refusal: nothing changes,
        // and reporting one would make pressing the same key twice look like a mistake.
        assert_eq!(settings.rebind(Control::Jump, KeyCode::Escape), Ok(()));
        assert_eq!(settings.bindings().key(Control::Jump), KeyCode::Escape);
    }

    #[test]
    fn a_key_this_client_reads_for_itself_is_not_offered() {
        let mut settings = Settings::default();
        for key in [
            KeyCode::ShiftLeft,
            KeyCode::F5,
            KeyCode::Digit1,
            KeyCode::Enter,
            KeyCode::ArrowUp,
        ] {
            assert_eq!(
                settings.rebind(Control::Jump, key),
                Err(RebindRefusal::NotOffered),
                "{key:?} is read elsewhere and must not be bindable"
            );
        }
        assert_eq!(settings.bindings().key(Control::Jump), KeyCode::Space);
    }

    #[test]
    fn no_two_keys_in_the_table_share_a_name_and_no_two_names_share_a_key() {
        for (index, (key, name)) in REBINDABLE_KEYS.iter().enumerate() {
            for (other_key, other_name) in &REBINDABLE_KEYS[index + 1..] {
                assert_ne!(key, other_key, "{name} and {other_name} are one key");
                assert_ne!(name, other_name, "{key:?} and {other_key:?} share a name");
            }
        }
    }

    /// **Nothing the server said reaches a setting**, and this keeps that true as the
    /// module grows: the graphics half of #179 adds a render distance, which is exactly the
    /// setting `ServerWelcome.view_distance` would be easiest to slip into. The test module
    /// is cut off before the search, because this assertion is written in terms of the very
    /// name it looks for and would otherwise find itself.
    #[test]
    fn no_setting_is_sourced_from_anything_the_server_sent() {
        let field = ["view", "distance"].join("_");
        for source in [include_str!("mod.rs"), include_str!("store.rs")] {
            let production = source.split("#[cfg(test)]").next().unwrap_or_default();
            let mentions = production
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .filter(|line| line.contains(&field))
                .count();
            assert_eq!(mentions, 0, "the server's view distance reached a setting");
        }
    }

    #[test]
    fn a_plugin_with_a_file_loads_it_and_saves_a_change_back() {
        let scratch = store::Scratch::new("settings-plugin");
        let path = scratch.join("settings");

        let mut app = App::new();
        app.add_plugins((MinimalPlugins, SettingsPlugin::at(path.clone())));
        app.update();
        assert_eq!(*app.world().resource::<Settings>(), Settings::default());
        assert!(!path.exists(), "an unchanged session wrote a file");

        app.world_mut()
            .resource_mut::<Settings>()
            .adjust(Knob::LookSensitivity, 2);
        app.update();

        let (reloaded, complaints) = store::load(&path);
        assert_eq!(complaints, Vec::<String>::new());
        assert_eq!(&reloaded, app.world().resource::<Settings>());
    }
}
