//! What a player may change, and the file it survives a restart in.
//!
//! **Nothing here is a rule.** Every value in [`Settings`] is an input or presentation
//! preference. None reaches the wire and none decides a gameplay outcome — a knob that
//! changed what the server was *told* would be a knob that had escaped this module.
//!
//! **Render distance is where that would be easiest to get wrong**, and it is the one the
//! issue singles out. `ServerWelcome.view_distance` is how far the server *streams*, and it
//! is never read into [`Settings::render_distance`] — the setting is this client's own
//! number, loaded from this client's own file, exactly as `world/mod.rs`'s
//! `BACKLOG_VIEW_DISTANCE` is. What the server's number does is cap what can be *drawn*, in
//! `player/sky.rs`, because a client cannot draw chunks it was never sent; that is a ceiling
//! applied at the moment of drawing, not a value copied into a setting.
//!
//! **Every setting belongs to exactly one [`Tab`], and that is what makes a reset scopable.**
//! [`Settings::reset`] puts one tab back to its defaults and leaves every other tab exactly
//! where the player left it. Writing `Settings::default()` back would look correct on the tab
//! being reset and would silently clear the other one — which is why the grouping is stated
//! here, beside the defaults, rather than on the screen that draws the button.
//!
//! [`store`] owns the file.

mod store;

use std::path::PathBuf;

use bevy::prelude::*;
use bevy::window::{
    Monitor, MonitorSelection, PresentMode, PrimaryMonitor, PrimaryWindow, WindowMode,
};
use bevy::winit::{UpdateMode, WinitSettings};

use crate::net::MAX_VIEW_DISTANCE;

/// Loads the settings, keeps the file in step with them, and applies the ones the renderer
/// reads from a component rather than from [`Settings`] itself.
///
/// The halves that are *not* here: `player/mod.rs` reads the sensitivity and the bindings
/// straight out of the resource, `player/sky.rs` reads the three numbers the fog and the
/// ambient term are built from, and `ui/status.rs` reads the readout's two. All of them
/// already run every frame over the state they own, so a second writer would buy nothing.
pub struct SettingsPlugin {
    /// The file to load from and save to, or `None` when the environment names no data
    /// directory — in which case the settings are still adjustable, they simply do not
    /// survive the process.
    file: Option<PathBuf>,
    settings: Settings,
    complaints: Vec<String>,
}

impl SettingsPlugin {
    /// The settings file this process's environment names.
    pub fn from_environment() -> Self {
        Self::from_file(store::settings_path(&store::default_environment()))
    }

    /// A plugin whose file is `path`, for a test that must not read the developer's own
    /// settings — the reason `net`'s `Environment::rooted_at` exists one directory over.
    #[cfg(test)]
    fn at(path: PathBuf) -> Self {
        Self::from_file(Some(path))
    }

    fn from_file(file: Option<PathBuf>) -> Self {
        let (settings, complaints) = match &file {
            Some(path) => store::load(path),
            None => (Settings::default(), Vec::new()),
        };
        Self {
            file,
            settings,
            complaints,
        }
    }

    /// The mode the primary window is created in. A named monitor is resolved on update,
    /// after winit has created its monitor entities.
    pub fn initial_window_mode(&self) -> WindowMode {
        self.settings.window_mode().bevy(MonitorSelection::Primary)
    }
}

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        for complaint in &self.complaints {
            warn!("{complaint}");
        }

        app.insert_resource(SettingsFile {
            path: self.file.clone(),
            written: self.settings.clone(),
        })
        .insert_resource(self.settings.clone())
        .init_resource::<MonitorChoices>()
        // The audio module's supervisor threads fill this; the settings module owns it
        // because it is the two device knobs' bound, and a bound belongs beside the knob it
        // stops. An app built without `AudioPlugin` offers the system default and nothing
        // else, which is the truth on a machine with no audio at all.
        .init_resource::<AudioDevices>()
        .add_systems(
            Update,
            (
                refresh_monitor_choices,
                save_when_changed,
                apply_to_the_display,
            )
                .chain(),
        );
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

/// One half of the settings screen, and the scope of one reset.
///
/// **The grouping is the model's and not the screen's**, because [`Settings::reset`] is what
/// needs it: "put Graphics back" has to name the graphics fields and no others, and a screen
/// that owned the answer would be a second place for it to be wrong. `ui/settings.rs` draws
/// the strip from [`Self::ALL`] and asks [`Knob::tab`] which rows go where.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// The mouse sensitivity and the key bindings. What the screen opens on.
    #[default]
    Controls,
    /// The eight graphics values and the frame-rate readout.
    Graphics,
    /// How loud the game is, and the speaker test that proves it.
    Audio,
}

impl Tab {
    /// Every tab, in the order the strip draws them.
    ///
    /// A hand-written list, for the reason `ui/inventory.rs`'s `InventoryTab::ALL` is one:
    /// no stable Rust enumerates an enum's variants. What keeps it honest is that
    /// [`Self::label`] and [`Settings::reset`] both match with no wildcard arm, so a third
    /// tab is a build failure until it has a name and a set of defaults of its own.
    pub const ALL: [Self; 3] = [Self::Controls, Self::Graphics, Self::Audio];

    /// What a player reads on the tab.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Controls => "CONTROLS",
            Self::Graphics => "GRAPHICS",
            Self::Audio => "AUDIO",
        }
    }
}

/// Where a readout may be put.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Corner {
    /// Under the debug lines, which own the top-left corner already.
    TopLeft,
    #[default]
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    /// The next corner clockwise from this one.
    pub const fn next(self) -> Self {
        match self {
            Self::TopLeft => Self::TopRight,
            Self::TopRight => Self::BottomRight,
            Self::BottomRight => Self::BottomLeft,
            Self::BottomLeft => Self::TopLeft,
        }
    }

    /// What the settings screen calls it, and what the file records.
    pub const fn name(self) -> &'static str {
        match self {
            Self::TopLeft => "top-left",
            Self::TopRight => "top-right",
            Self::BottomLeft => "bottom-left",
            Self::BottomRight => "bottom-right",
        }
    }

    /// The corner `name` denotes, if it denotes one.
    fn from_name(name: &str) -> Option<Self> {
        [
            Self::TopLeft,
            Self::TopRight,
            Self::BottomLeft,
            Self::BottomRight,
        ]
        .into_iter()
        .find(|corner| corner.name() == name)
    }
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
    Chat,
    Interact,
    Inventory,
    Consume,
    Menu,
    Map,
    Mount,
    /// Speak. Held while [`VoiceMode::PushToTalk`] is chosen; unread while
    /// [`VoiceMode::VoiceActivation`] or [`VoiceMode::Off`] is.
    Talk,
}

/// Every control, in the order the settings screen lists them.
///
/// **The order is the enum's order, and that is a requirement rather than a tidiness.**
/// [`Bindings`] indexes its keys by `control as usize` while [`Bindings::default`] builds
/// that array with `CONTROLS.map`, so a control listed here out of its declaration order
/// would silently hand every control below it somebody else's key.
pub const CONTROLS: [Control; 13] = [
    Control::Forward,
    Control::Back,
    Control::Left,
    Control::Right,
    Control::Jump,
    Control::Chat,
    Control::Interact,
    Control::Inventory,
    Control::Consume,
    Control::Menu,
    Control::Map,
    Control::Mount,
    Control::Talk,
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
            Self::Chat => "chat",
            Self::Interact => "interact",
            Self::Inventory => "inventory",
            Self::Consume => "consume",
            Self::Menu => "menu",
            Self::Map => "map",
            Self::Mount => "mount",
            Self::Talk => "talk",
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
            Self::Chat => "Chat",
            Self::Interact => "Interact",
            Self::Inventory => "Inventory",
            Self::Consume => "Consume item",
            Self::Menu => "Pause menu",
            Self::Map => "World map",
            Self::Mount => "Call mount",
            Self::Talk => "Push to talk",
        }
    }

    /// The control `name` denotes, if it denotes one.
    fn from_name(name: &str) -> Option<Self> {
        CONTROLS.into_iter().find(|control| control.name() == name)
    }

    /// The key this control answers to before anybody changes it.
    ///
    /// **A default is what an *unnamed* control gets, never what overrides a named one.**
    /// `Talk` starts on `KeyV`, which was free before it existed — so a settings file
    /// written by an older client may legally hold `bind consume v` and no `talk` line at
    /// all. [`Bindings::from_pairs`] resolves that by applying the file's bindings as a set
    /// and giving each control the file left out the first key nothing else holds: the
    /// player keeps `consume v`, `Talk` arrives somewhere free, and no control is left
    /// unreachable. `a_settings_file_older_than_the_talk_control_keeps_every_binding_it_saved`
    /// in `store` is what holds that, over exactly such a file.
    const fn default_key(self) -> KeyCode {
        match self {
            Self::Forward => KeyCode::KeyW,
            Self::Back => KeyCode::KeyS,
            Self::Left => KeyCode::KeyA,
            Self::Right => KeyCode::KeyD,
            Self::Jump => KeyCode::Space,
            Self::Chat => KeyCode::KeyT,
            Self::Interact => KeyCode::KeyF,
            Self::Inventory => KeyCode::KeyE,
            Self::Consume => KeyCode::KeyC,
            Self::Menu => KeyCode::Escape,
            Self::Map => KeyCode::KeyM,
            Self::Mount => KeyCode::KeyZ,
            Self::Talk => KeyCode::KeyV,
        }
    }
}

/// The horse a player chose as their default; learned state remains server-owned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultMount {
    Black,
    Brown,
    Grey,
}

impl DefaultMount {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Black => "black-horse",
            Self::Brown => "brown-horse",
            Self::Grey => "grey-horse",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "black-horse" => Some(Self::Black),
            "brown-horse" => Some(Self::Brown),
            "grey-horse" => Some(Self::Grey),
            _ => None,
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
    /// Controls it leaves out keep their defaults when those keys are free. If a named
    /// binding already uses a newly added control's default, the named binding wins and
    /// the omitted control takes the first free default before falling back to another
    /// offered key. That is how an older complete settings file survives a new control.
    /// The answer is `None` when two controls named by the file share a key.
    ///
    /// **A set, and deliberately not a sequence of [`Self::rebind`] calls.** The file names
    /// every control at once, so two bindings that trade keys cross over halfway through —
    /// jump to `Escape` while the pause menu still holds it — and a rebinding checked one at
    /// a time against the defaults refuses the second of the pair. That is a configuration a
    /// player saved and could not load again; `store`'s round trip is what found it.
    fn from_pairs(named: &[(Control, KeyCode)]) -> Option<Self> {
        let mut keys = [None; CONTROLS.len()];
        for (control, key) in named {
            key_name(*key)?;
            keys[*control as usize] = Some(*key);
        }
        for (index, key) in keys.iter().enumerate() {
            if key.is_some() && keys[..index].contains(key) {
                return None;
            }
        }

        for control in CONTROLS {
            let index = control as usize;
            if keys[index].is_some() {
                continue;
            }
            let free = std::iter::once(control.default_key())
                .chain(CONTROLS.map(Control::default_key))
                .chain(REBINDABLE_KEYS.iter().map(|(key, _)| *key))
                .find(|candidate| !keys.iter().flatten().any(|key| key == candidate))?;
            keys[index] = Some(free);
        }

        Some(Self {
            keys: keys.map(|key| key.expect("every omitted control was assigned a free key")),
        })
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
/// how far up that is and where it stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Knob {
    LookSensitivity,
    WindowMode,
    Monitor,
    RenderDistance,
    FieldOfView,
    Brightness,
    FogStart,
    FrameCap,
    MasterVolume,
    OutputDevice,
    VoiceMode,
    VoiceActivationThreshold,
}

/// Every knob, in the order the settings screen lists them.
pub const KNOBS: [Knob; 12] = [
    Knob::LookSensitivity,
    Knob::WindowMode,
    Knob::Monitor,
    Knob::RenderDistance,
    Knob::FieldOfView,
    Knob::Brightness,
    Knob::FogStart,
    Knob::FrameCap,
    Knob::MasterVolume,
    Knob::OutputDevice,
    Knob::VoiceMode,
    Knob::VoiceActivationThreshold,
];

impl Knob {
    /// What the settings screen calls it.
    pub const fn label(self) -> &'static str {
        match self {
            Self::LookSensitivity => "Mouse sensitivity",
            Self::WindowMode => "Window mode",
            Self::Monitor => "Monitor",
            Self::RenderDistance => "Render distance",
            Self::FieldOfView => "Field of view",
            Self::Brightness => "Brightness",
            Self::FogStart => "Fog starts at",
            Self::FrameCap => "Frame cap",
            Self::MasterVolume => "Master volume",
            Self::OutputDevice => "Output device",
            Self::VoiceMode => "Voice",
            Self::VoiceActivationThreshold => "Voice threshold",
        }
    }

    /// Which tab this knob is listed on, and which reset therefore puts it back.
    ///
    /// No wildcard arm, so a thirteenth knob has to say where it belongs before it builds —
    /// which is the same thing as saying which reset owns it.
    pub const fn tab(self) -> Tab {
        match self {
            Self::LookSensitivity => Tab::Controls,
            Self::WindowMode
            | Self::Monitor
            | Self::RenderDistance
            | Self::FieldOfView
            | Self::Brightness
            | Self::FogStart
            | Self::FrameCap => Tab::Graphics,
            Self::MasterVolume
            | Self::OutputDevice
            | Self::VoiceMode
            | Self::VoiceActivationThreshold => Tab::Audio,
        }
    }
}

/// The two window modes this client offers. The closed enum is the bound,
/// [`WINDOW_MODE_STEP`] is one press, and [`DEFAULT_WINDOW_MODE`] is the initial value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayMode {
    BorderlessFullscreen,
    Windowed,
}

const WINDOW_MODES: [DisplayMode; 2] = [DisplayMode::BorderlessFullscreen, DisplayMode::Windowed];
const WINDOW_MODE_STEP: i32 = 1;
const DEFAULT_WINDOW_MODE: DisplayMode = DisplayMode::BorderlessFullscreen;

impl DisplayMode {
    /// What the screen and settings file call this mode.
    pub const fn name(self) -> &'static str {
        match self {
            Self::BorderlessFullscreen => "borderless",
            Self::Windowed => "windowed",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        WINDOW_MODES.into_iter().find(|mode| mode.name() == name)
    }

    fn bevy(self, monitor: MonitorSelection) -> WindowMode {
        match self {
            Self::BorderlessFullscreen => WindowMode::BorderlessFullscreen(monitor),
            Self::Windowed => WindowMode::Windowed,
        }
    }
}

/// Which attached display the player chose.
///
/// `Specific` holds an opaque, settings-file-safe identity. It survives while a monitor is
/// absent; resolution to a Bevy entity stays separate so a fallback cannot rewrite it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MonitorPreference {
    Primary,
    Specific(String),
}

const DEFAULT_MONITOR: MonitorPreference = MonitorPreference::Primary;
const MONITOR_STEP: i32 = 1;

/// One display currently reported by the operating system.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MonitorChoice {
    entity: Entity,
    identity: String,
    label: String,
    primary: bool,
}

/// The dynamic bound of the monitor knob.
///
/// The primary choice is always present. Every other choice comes from a live [`Monitor`]
/// component, so the screen cannot offer a bare index or a display that does not exist.
#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub struct MonitorChoices {
    attached: Vec<MonitorChoice>,
}

impl MonitorChoices {
    /// Every option the settings screen's monitor dropdown offers, in the order
    /// [`Self::moved`] steps through: `Primary`, then every live display that is not the
    /// primary one. An unavailable saved preference is deliberately absent — it stays
    /// selected until the player replaces it, but is not reselectable from this list.
    pub(crate) fn preferences(&self) -> Vec<MonitorPreference> {
        let mut preferences = vec![MonitorPreference::Primary];
        preferences.extend(
            self.attached
                .iter()
                .filter(|choice| !choice.primary)
                .map(|choice| MonitorPreference::Specific(choice.identity.clone())),
        );
        preferences
    }

    fn moved(&self, current: &MonitorPreference, steps: i32) -> MonitorPreference {
        let choices = self.preferences();
        let current = match current {
            MonitorPreference::Primary => 0,
            MonitorPreference::Specific(identity) => self
                .attached
                .iter()
                .find(|choice| choice.identity == *identity)
                .and_then(|choice| {
                    if choice.primary {
                        Some(0)
                    } else {
                        choices.iter().position(|candidate| candidate == current)
                    }
                })
                .unwrap_or(0),
        };
        let moved = (current as i64)
            .saturating_add(i64::from(steps).saturating_mul(i64::from(MONITOR_STEP)))
            .clamp(0, choices.len().saturating_sub(1) as i64) as usize;
        choices[moved].clone()
    }

    fn label(&self, selected: &MonitorPreference) -> String {
        match selected {
            MonitorPreference::Primary => self
                .attached
                .iter()
                .find(|choice| choice.primary)
                .map(|choice| format!("primary - {}", choice.label))
                .unwrap_or_else(|| "primary".to_owned()),
            MonitorPreference::Specific(identity) => self
                .attached
                .iter()
                .find(|choice| choice.identity == *identity)
                .map(|choice| choice.label.clone())
                .unwrap_or_else(|| {
                    format!(
                        "{} (unavailable)",
                        monitor_name_from_identity(identity)
                            .unwrap_or_else(|| "display".to_owned())
                    )
                }),
        }
    }

    /// What one dropdown option reads. Unlike [`Self::label`], every preference
    /// [`Self::preferences`] offers is live, so there is no `(unavailable)` case — that
    /// suffix belongs to the closed control, which may still show a saved choice this
    /// list no longer contains.
    pub(crate) fn option_label(&self, preference: &MonitorPreference) -> String {
        match preference {
            MonitorPreference::Primary => "Primary".to_owned(),
            MonitorPreference::Specific(identity) => self
                .attached
                .iter()
                .find(|choice| choice.identity == *identity)
                .map(|choice| choice.label.clone())
                .unwrap_or_default(),
        }
    }

    fn selection(&self, selected: &MonitorPreference) -> MonitorSelection {
        match selected {
            MonitorPreference::Primary => MonitorSelection::Primary,
            MonitorPreference::Specific(identity) => self
                .attached
                .iter()
                .find(|choice| choice.identity == *identity)
                .map(|choice| MonitorSelection::Entity(choice.entity))
                // A missing monitor changes only the applied value. The resource still
                // holds `Specific`, so `save_when_changed` has nothing to erase.
                .unwrap_or(MonitorSelection::Primary),
        }
    }

    #[cfg(test)]
    pub(crate) fn named(names: &[&str]) -> Self {
        Self {
            attached: names
                .iter()
                .enumerate()
                .map(|(index, name)| MonitorChoice {
                    entity: Entity::from_raw_u32(index as u32 + 1).unwrap(),
                    identity: monitor_identity(
                        Some(name),
                        1920,
                        1080,
                        index as i32 * 1920,
                        0,
                        false,
                    ),
                    label: format!("{name} (1920x1080 at {},0)", index * 1920),
                    primary: index == 0,
                })
                .collect(),
        }
    }
}

/// Builds a settings-file-safe identity for one attached monitor.
fn monitor_identity(
    name: Option<&str>,
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    duplicate_name: bool,
) -> String {
    let encoded_name = hex_encode(name.unwrap_or_default());
    if !encoded_name.is_empty() && !duplicate_name {
        format!("name:{encoded_name}")
    } else {
        format!("display:{encoded_name}:{width}:{height}:{x}:{y}")
    }
}

/// Whether `identity` is one this module could have produced.
fn valid_monitor_identity(identity: &str) -> bool {
    let mut fields = identity.split(':');
    let kind = fields.next();
    let valid_name = fields.next().is_some_and(|name| {
        name.len() % 2 == 0
            && name.as_bytes().iter().all(u8::is_ascii_hexdigit)
            && decode_hex(name).is_some_and(|bytes| String::from_utf8(bytes).is_ok())
    });
    if kind == Some("name") {
        return valid_name && fields.next().is_none();
    }
    let valid = kind == Some("display")
        && valid_name
        && fields
            .next()
            .and_then(|value| value.parse::<u32>().ok())
            .is_some_and(|value| value > 0)
        && fields
            .next()
            .and_then(|value| value.parse::<u32>().ok())
            .is_some_and(|value| value > 0)
        && fields
            .next()
            .and_then(|value| value.parse::<i32>().ok())
            .is_some()
        && fields
            .next()
            .and_then(|value| value.parse::<i32>().ok())
            .is_some();
    valid && fields.next().is_none()
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair).ok()?;
            u8::from_str_radix(text, 16).ok()
        })
        .collect()
}

fn monitor_name_from_identity(identity: &str) -> Option<String> {
    let encoded = identity.split(':').nth(1)?;
    let decoded = decode_hex(encoded)?;
    let name = String::from_utf8(decoded).ok()?;
    Some(ascii_monitor_name(Some(&name)))
}

fn ascii_monitor_name(name: Option<&str>) -> String {
    ascii_shown(name.unwrap_or_default(), "unnamed display")
}

/// `name` as this client's only font can draw it, or `empty` when nothing is left.
///
/// **A hardware name is not this client's string**, and `ui/mod.rs`'s `ascii_guard` cannot
/// see one: it reads the literals this crate compiles, and a monitor or card name arrives at
/// runtime in whatever encoding the platform likes. The embedded font draws 95 codepoints
/// and lays every other one out with zero advance, so anything else becomes a visible `?`
/// rather than an invisible gap in a row a player is choosing from.
fn ascii_shown(name: &str, empty: &str) -> String {
    let shown: String = name
        .trim()
        .chars()
        .map(|character| {
            if character.is_ascii_graphic() || character == ' ' {
                character
            } else {
                '?'
            }
        })
        .collect();
    if shown.is_empty() {
        empty.to_owned()
    } else {
        shown
    }
}

/// `name`'s bytes as lowercase hex — what turns a platform-given name into one settings-file
/// field. A sound card called `HDA Intel PCH: ALC295 Analog` is four whitespace-separated
/// fields written plainly, and the parser would read the first. [`monitor_identity`] already
/// chose this encoding for the same problem, so the device reuses it rather than inventing a
/// second escape.
fn hex_encode(name: &str) -> String {
    name.as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Which device the player chose, on either side of the sound card.
///
/// **`SystemDefault` is not "whichever device happens to be default now"** but a standing
/// instruction to follow the system: `audio/device.rs` reopens when the host starts calling
/// something else its default, which is what a player who never opened this tab expects when
/// a headset goes in. [`Self::Named`] is the opposite instruction — this device and no other
/// — so unplugging that headset leaves the client silent and retrying rather than quietly
/// moving to the speakers.
///
/// **One type for both knobs, because it is one statement.** The output and the input are the
/// same question asked of two halves of the same card, and the difference between them is
/// entirely in what `audio/device.rs` does with the answer — not in what a player may say.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DeviceChoice {
    #[default]
    SystemDefault,
    /// One device, under the name its host gives it. Never shortened or normalised: it is
    /// the key `audio/device.rs` matches an enumerated device against.
    Named(String),
}

/// What the file calls "no device in particular".
const SYSTEM_DEFAULT_DEVICE: &str = "default";

/// The `output-device` or `input-device` line's value for `device`.
fn device_field(device: &DeviceChoice) -> String {
    match device {
        DeviceChoice::SystemDefault => SYSTEM_DEFAULT_DEVICE.to_owned(),
        DeviceChoice::Named(name) => format!("name:{}", hex_encode(name)),
    }
}

/// The device `field` denotes, if it denotes one.
///
/// An unreadable field is `None` and costs this one setting its default, which is the whole
/// of [`store`]'s rule for a line nothing can read.
fn device_from_field(field: &str) -> Option<DeviceChoice> {
    if field == SYSTEM_DEFAULT_DEVICE {
        return Some(DeviceChoice::SystemDefault);
    }
    let name = String::from_utf8(decode_hex(field.strip_prefix("name:")?)?).ok()?;
    if name.is_empty() {
        return None;
    }
    Some(DeviceChoice::Named(name))
}

/// One side of the card's dynamic bound: what the host currently offers there.
///
/// [`MonitorChoices`]' shape, for its reason — a knob whose values are the machine's still
/// needs one place saying what stepping may reach.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DeviceList {
    present: Vec<String>,
}

impl DeviceList {
    /// What the knob steps through: the system default, then every device the host named.
    ///
    /// A saved device that is not present is deliberately absent, exactly as an unavailable
    /// monitor is: it stays selected until the player replaces it, and stepping cannot land
    /// back on it.
    fn choices(&self) -> Vec<DeviceChoice> {
        let mut choices = vec![DeviceChoice::SystemDefault];
        choices.extend(self.present.iter().cloned().map(DeviceChoice::Named));
        choices
    }

    /// What the knob's reading says for `selected`.
    fn label(&self, selected: &DeviceChoice) -> String {
        match selected {
            DeviceChoice::SystemDefault => "system default".to_owned(),
            DeviceChoice::Named(name) if self.present.iter().any(|found| found == name) => {
                ascii_shown(name, "unnamed device")
            }
            // Chosen, saved, and not here now — a headset unplugged, or a file carried to
            // another machine. Said out loud, because the alternative is a row naming a
            // device while the client is silent for no reason a player can see.
            DeviceChoice::Named(name) => {
                format!("{} (unavailable)", ascii_shown(name, "unnamed device"))
            }
        }
    }

    /// The device `steps` presses away from `current`.
    fn moved(&self, current: &DeviceChoice, steps: i32) -> DeviceChoice {
        let choices = self.choices();
        let at = choices
            .iter()
            .position(|choice| choice == current)
            .unwrap_or(0) as i64;
        let moved = at
            .saturating_add(i64::from(steps))
            .clamp(0, choices.len().saturating_sub(1) as i64) as usize;
        choices[moved].clone()
    }

    #[cfg(test)]
    fn named(names: &[&str]) -> Self {
        Self {
            present: names.iter().map(|name| (*name).to_owned()).collect(),
        }
    }
}

/// The dynamic bound of the device knobs: what the host offers on each side of the card.
///
/// **One resource and not one per side**, because one module fills them: `audio/mod.rs`
/// writes it from its supervisors' last enumerations, the only code here allowed to ask a
/// host anything. The microphone's list joins the loudspeaker's here in part 2 of #853.
#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub struct AudioDevices {
    outputs: DeviceList,
}

impl AudioDevices {
    /// Every output device the host named, as of the last enumeration.
    pub fn outputs(&self) -> &DeviceList {
        &self.outputs
    }

    /// Replaces the output list with what the audio module last enumerated.
    pub fn offer_outputs(&mut self, present: Vec<String>) {
        self.outputs.present = present;
    }

    #[cfg(test)]
    pub(crate) fn named(outputs: &[&str]) -> Self {
        Self {
            outputs: DeviceList::named(outputs),
        }
    }
}

/// What the microphone is for.
///
/// **Three values and no fourth**, and the closed enum is the knob's whole bound. `Off` is a
/// standing instruction rather than an absence of one: `audio/` opens no capture device
/// while it is chosen, so a player who turns voice off is not merely muted — nothing on this
/// machine is listening. The two live modes differ only in what starts a transmission, which
/// is why the threshold below belongs to one of them and not to both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VoiceMode {
    /// Nothing is captured and nothing is sent.
    Off,
    /// Transmit while [`Control::Talk`] is held.
    #[default]
    PushToTalk,
    /// Transmit while the gated capture level is above
    /// [`Settings::voice_activation_threshold_db`].
    VoiceActivation,
}

/// Every voice mode, in the order the knob steps through them.
///
/// Quietest first, so stepping up is stepping towards being heard more often — the
/// direction every other knob on this tab moves in.
const VOICE_MODES: [VoiceMode; 3] = [
    VoiceMode::Off,
    VoiceMode::PushToTalk,
    VoiceMode::VoiceActivation,
];

/// One press of the voice-mode control.
const VOICE_MODE_STEP: i32 = 1;

/// What voice is set to before anybody changes it.
///
/// Push to talk, deliberately, and `docs/adr/0001-voice-transport.md` is why: this client
/// runs no echo canceller, so an open microphone beside a loudspeaker is a feedback path.
/// A key that has to be held is the mitigation, and a default that starts there is the
/// mitigation being on for the player who never opens this tab.
const DEFAULT_VOICE_MODE: VoiceMode = VoiceMode::PushToTalk;

impl VoiceMode {
    /// What the file calls it.
    const fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::PushToTalk => "push-to-talk",
            Self::VoiceActivation => "voice-activation",
        }
    }

    /// What the settings screen prints beside the knob.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::PushToTalk => "push to talk",
            Self::VoiceActivation => "voice activation",
        }
    }

    /// The mode `name` denotes, if it denotes one.
    fn from_name(name: &str) -> Option<Self> {
        VOICE_MODES.into_iter().find(|mode| mode.name() == name)
    }
}

/// The quietest level voice activation may be asked to trigger on, in dBFS.
///
/// Below this a domestic room's own noise floor is above the threshold on most microphones,
/// so the setting would be an open microphone wearing the name of a gate.
const MIN_VOICE_ACTIVATION_THRESHOLD: f32 = -60.0;
/// And the loudest. Above this a normal speaking voice at a normal distance never opens it,
/// which is a microphone that appears to be broken.
const MAX_VOICE_ACTIVATION_THRESHOLD: f32 = -10.0;
/// One press of the voice-activation control, in decibels.
const VOICE_ACTIVATION_THRESHOLD_STEP: f32 = 2.0;
/// Where it starts: above a quiet room, below a voice.
const DEFAULT_VOICE_ACTIVATION_THRESHOLD: f32 = -40.0;

/// The dynamic bounds of the knobs whose values belong to the machine, not this module.
///
/// One borrow rather than an argument per bound that every caller keeps in the right order,
/// and named for what it is, so a further such knob widens this struct instead of every
/// signature.
#[derive(Clone, Copy, Debug)]
pub struct Choices<'a> {
    pub monitors: &'a MonitorChoices,
    pub devices: &'a AudioDevices,
}

/// An empty device list, so [`Choices::with_monitors`] can hand out a borrow of one.
#[cfg(test)]
static NO_AUDIO_DEVICES: AudioDevices = AudioDevices {
    outputs: DeviceList {
        present: Vec::new(),
    },
};

#[cfg(test)]
impl<'a> Choices<'a> {
    /// The bounds of a knob that has nothing to do with a sound card. Test-only: every
    /// shipped caller reads both resources out of the world, and a production path quietly
    /// offering no device would be a knob stuck on the system default with nothing saying why.
    pub(crate) const fn with_monitors(monitors: &'a MonitorChoices) -> Self {
        Self {
            monitors,
            devices: &NO_AUDIO_DEVICES,
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

/// The closest the horizon may be drawn, in chunks. Below two the world is a corridor.
const MIN_RENDER_DISTANCE: u8 = 2;
/// The furthest, in chunks. The protocol's own ceiling, so the setting can always ask for
/// everything the most generous server would stream and never for more than one could.
///
/// **A bound, not a value.** It is the one thing this module takes from `net`, and it is a
/// compile-time constant of the contract rather than anything a server said — what a server
/// said is applied in `player/sky.rs`, as a ceiling on drawing.
const MAX_RENDER_DISTANCE: u8 = MAX_VIEW_DISTANCE;
/// What the client draws to before anybody changes it, in chunks. `world/mod.rs` sizes its
/// decode backlog from the same client-chosen number.
const DEFAULT_RENDER_DISTANCE: u8 = 8;

/// The narrowest vertical field of view this screen offers, in degrees.
const MIN_FIELD_OF_VIEW: f32 = 40.0;
/// The widest. Past this the edges of the frame distort more than the extra view is worth.
const MAX_FIELD_OF_VIEW: f32 = 110.0;
/// One press of the field-of-view control, in degrees.
const FIELD_OF_VIEW_STEP: f32 = 5.0;
/// Bevy's own default perspective, in degrees — π/4. The default is this number so that a
/// client whose player never opens this screen renders exactly as it did before it existed.
const DEFAULT_FIELD_OF_VIEW: f32 = 45.0;

/// The dimmest the ambient term may be scaled to.
const MIN_BRIGHTNESS: f32 = 0.5;
/// And the brightest. Past double, `player/sky.rs`'s night stops reading as night at all.
const MAX_BRIGHTNESS: f32 = 2.0;
/// One press of the brightness control.
const BRIGHTNESS_STEP: f32 = 0.05;

/// The earliest the fog may begin, as a fraction of the distance at which it is total.
const MIN_FOG_START: f32 = 0.1;
/// And the latest. At 1.0 there is no fade at all, only an edge.
const MAX_FOG_START: f32 = 0.95;
/// One press of the fog control.
const FOG_START_STEP: f32 = 0.05;
/// Where the fog begins by default. `player/sky.rs` held this as a constant until this
/// screen existed: clear for the near half, dissolving across the far half.
const DEFAULT_FOG_START: f32 = 0.5;

/// The slowest frame cap this screen offers, in frames per second.
const MIN_FRAME_CAP: u16 = 30;
/// And the fastest.
const MAX_FRAME_CAP: u16 = 240;
/// One press of the frame-cap control, in frames per second.
const FRAME_CAP_STEP: u16 = 10;
/// The frame cap that means "no cap" — and the default, because the app loop runs
/// continuously until somebody asks it not to.
const NO_FRAME_CAP: u16 = 0;

/// Silence, and a real value rather than a floor to stop short of: a player who wants the
/// game muted has to be able to say so with this knob.
const MIN_MASTER_VOLUME: u8 = 0;
/// Unity gain, and the top. Nothing amplifies past what the mixer was handed —
/// `audio/mixer.rs` clamps its own gains, and a knob asking for more asks for clipping.
const MAX_MASTER_VOLUME: u8 = 100;
/// One press of the master volume control.
const MASTER_VOLUME_STEP: u8 = 5;
/// What the game starts at: audible, with room above it for a quiet recording.
const DEFAULT_MASTER_VOLUME: u8 = 80;

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
    default_mount: Option<DefaultMount>,
    window_mode: DisplayMode,
    monitor: MonitorPreference,
    readout_shown: bool,
    readout_corner: Corner,
    render_distance: u8,
    field_of_view: f32,
    vsync: bool,
    frame_cap: u16,
    brightness: f32,
    fog_start: f32,
    master_volume: u8,
    output_device: DeviceChoice,
    voice_mode: VoiceMode,
    voice_activation_threshold: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            look_sensitivity: DEFAULT_LOOK_SENSITIVITY,
            bindings: Bindings::default(),
            default_mount: None,
            window_mode: DEFAULT_WINDOW_MODE,
            monitor: DEFAULT_MONITOR,
            readout_shown: false,
            readout_corner: Corner::default(),
            render_distance: DEFAULT_RENDER_DISTANCE,
            field_of_view: DEFAULT_FIELD_OF_VIEW,
            vsync: true,
            frame_cap: NO_FRAME_CAP,
            brightness: 1.0,
            fog_start: DEFAULT_FOG_START,
            master_volume: DEFAULT_MASTER_VOLUME,
            output_device: DeviceChoice::SystemDefault,
            voice_mode: DEFAULT_VOICE_MODE,
            voice_activation_threshold: DEFAULT_VOICE_ACTIVATION_THRESHOLD,
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

    /// Which learned mount the player chose in the mounts tab, if any.
    pub const fn default_mount(&self) -> Option<DefaultMount> {
        self.default_mount
    }

    /// Remembers the selection made from an authoritative learned-mount row.
    pub const fn set_default_mount(&mut self, mount: DefaultMount) {
        self.default_mount = Some(mount);
    }

    /// Whether the window is borderless fullscreen or decorated and resizable.
    pub const fn window_mode(&self) -> DisplayMode {
        self.window_mode
    }

    /// Which attached display the window should use.
    pub const fn monitor(&self) -> &MonitorPreference {
        &self.monitor
    }

    /// Whether the frame-rate readout is on screen.
    pub const fn readout_shown(&self) -> bool {
        self.readout_shown
    }

    /// Which corner it sits in.
    pub const fn readout_corner(&self) -> Corner {
        self.readout_corner
    }

    /// How far the client draws before the world fades out, in chunks.
    ///
    /// **The client's own number.** `ServerWelcome.view_distance` is never read into it —
    /// what that number does is cap the *drawing*, in `player/sky.rs`, because chunks the
    /// server never sent cannot be drawn however far this says.
    pub const fn render_distance(&self) -> u8 {
        self.render_distance
    }

    /// The camera's vertical field of view, in degrees.
    pub const fn field_of_view(&self) -> f32 {
        self.field_of_view
    }

    /// Whether presentation waits for the vertical blank.
    pub const fn vsync(&self) -> bool {
        self.vsync
    }

    /// The frame cap in frames per second, or [`NO_FRAME_CAP`] for none.
    pub const fn frame_cap(&self) -> u16 {
        self.frame_cap
    }

    /// What the ambient term is scaled by.
    pub const fn brightness(&self) -> f32 {
        self.brightness
    }

    /// Where the fog begins, as a fraction of the distance at which it is total.
    pub const fn fog_start(&self) -> f32 {
        self.fog_start
    }

    /// How loud the master bus is, from 0 to 100.
    pub const fn master_volume(&self) -> u8 {
        self.master_volume
    }

    /// The gain the master bus is set to, `0.0` silent to `1.0` unity.
    ///
    /// The one conversion between the number a player reads and the number a sample is
    /// multiplied by, so `audio/` never divides by a hundred and this module never holds a
    /// gain. Linear, deliberately: a perceptual curve changes how a volume *sounds* and
    /// belongs with the feature that needs it, not with the knob that arrives first.
    pub fn master_gain(&self) -> f32 {
        f32::from(self.master_volume()) / f32::from(MAX_MASTER_VOLUME)
    }

    /// Which output device the audio module should open.
    pub const fn output_device(&self) -> &DeviceChoice {
        &self.output_device
    }

    /// What the microphone is for: nothing, a held key, or a level.
    pub const fn voice_mode(&self) -> VoiceMode {
        self.voice_mode
    }

    /// The level at which [`VoiceMode::VoiceActivation`] starts transmitting, in dBFS.
    ///
    /// A decibel and not a gain, unlike [`Self::master_gain`]: this number is compared
    /// against a meter reading rather than multiplied into a sample, and the meter that
    /// answers it works in decibels. The conversion that does not happen here is the point
    /// — there is no second place a threshold could be expressed.
    pub const fn voice_activation_threshold_db(&self) -> f32 {
        self.voice_activation_threshold
    }

    /// Moves a fixed-bound `knob` by `steps` of its own size.
    #[cfg(test)]
    pub fn adjust(&mut self, knob: Knob, steps: i32) {
        self.adjust_with_choices(
            knob,
            steps,
            Choices::with_monitors(&MonitorChoices::default()),
        );
    }

    /// Moves `knob` by `steps` of its own size, stopping at its current bounds.
    pub fn adjust_with_choices(&mut self, knob: Knob, steps: i32, choices: Choices<'_>) {
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
            Knob::WindowMode => {
                let current = WINDOW_MODES
                    .iter()
                    .position(|mode| *mode == self.window_mode)
                    .unwrap_or_default() as i64;
                let moved = current
                    .saturating_add(i64::from(steps).saturating_mul(i64::from(WINDOW_MODE_STEP)))
                    .clamp(0, WINDOW_MODES.len().saturating_sub(1) as i64)
                    as usize;
                self.window_mode = WINDOW_MODES[moved];
            }
            Knob::Monitor => self.monitor = choices.monitors.moved(&self.monitor, steps),
            Knob::RenderDistance => {
                self.render_distance = step_u8(
                    self.render_distance,
                    steps,
                    MIN_RENDER_DISTANCE,
                    MAX_RENDER_DISTANCE,
                );
            }
            Knob::FieldOfView => {
                self.field_of_view = shift(
                    self.field_of_view,
                    FIELD_OF_VIEW_STEP,
                    MIN_FIELD_OF_VIEW,
                    MAX_FIELD_OF_VIEW,
                    1,
                );
            }
            Knob::Brightness => {
                self.brightness = shift(
                    self.brightness,
                    BRIGHTNESS_STEP,
                    MIN_BRIGHTNESS,
                    MAX_BRIGHTNESS,
                    2,
                );
            }
            Knob::FogStart => {
                self.fog_start = shift(
                    self.fog_start,
                    FOG_START_STEP,
                    MIN_FOG_START,
                    MAX_FOG_START,
                    2,
                );
            }
            Knob::FrameCap => self.frame_cap = step_frame_cap(self.frame_cap, steps),
            Knob::MasterVolume => {
                self.master_volume = step_u8(
                    self.master_volume,
                    steps.saturating_mul(i32::from(MASTER_VOLUME_STEP)),
                    MIN_MASTER_VOLUME,
                    MAX_MASTER_VOLUME,
                );
            }
            Knob::OutputDevice => {
                self.output_device = choices.devices.outputs().moved(&self.output_device, steps);
            }
            Knob::VoiceMode => {
                let current = VOICE_MODES
                    .iter()
                    .position(|mode| *mode == self.voice_mode)
                    .unwrap_or_default() as i64;
                let moved = current
                    .saturating_add(i64::from(steps).saturating_mul(i64::from(VOICE_MODE_STEP)))
                    .clamp(0, VOICE_MODES.len().saturating_sub(1) as i64)
                    as usize;
                self.voice_mode = VOICE_MODES[moved];
            }
            Knob::VoiceActivationThreshold => {
                self.voice_activation_threshold = shift(
                    self.voice_activation_threshold,
                    VOICE_ACTIVATION_THRESHOLD_STEP,
                    MIN_VOICE_ACTIVATION_THRESHOLD,
                    MAX_VOICE_ACTIVATION_THRESHOLD,
                    1,
                );
            }
        }
    }

    /// Applies one of `monitors.preferences()` directly, replacing whatever was selected.
    ///
    /// The Monitor row is a select, not a stepper: a player picks a display rather than
    /// moving relative to whichever one is current, so this assigns instead of stepping
    /// through [`Self::adjust_with_choices`]. It replaces an unavailable saved preference
    /// exactly as it replaces a live one — there is no other way to leave one behind.
    pub fn set_monitor(&mut self, preference: MonitorPreference) {
        self.monitor = preference;
    }

    /// What the settings screen prints beside `knob`.
    #[cfg(test)]
    pub fn reading(&self, knob: Knob) -> String {
        self.reading_with_choices(knob, Choices::with_monitors(&MonitorChoices::default()))
    }

    /// What the settings screen prints beside `knob`, with the attached monitor names.
    pub fn reading_with_choices(&self, knob: Knob, choices: Choices<'_>) -> String {
        match knob {
            Knob::LookSensitivity => format!("{:.4}", self.look_sensitivity),
            Knob::WindowMode => self.window_mode.name().to_owned(),
            Knob::Monitor => choices.monitors.label(&self.monitor),
            Knob::RenderDistance => format!("{} chunks", self.render_distance),
            Knob::FieldOfView => format!("{:.0} deg", self.field_of_view),
            Knob::Brightness => format!("{:.2}x", self.brightness),
            Knob::FogStart => format!("{:.0}%", self.fog_start * 100.0),
            Knob::FrameCap if self.frame_cap == NO_FRAME_CAP => "uncapped".to_owned(),
            Knob::FrameCap => format!("{} fps", self.frame_cap),
            Knob::MasterVolume => format!("{}%", self.master_volume),
            Knob::OutputDevice => choices.devices.outputs().label(&self.output_device),
            Knob::VoiceMode => self.voice_mode.label().to_owned(),
            Knob::VoiceActivationThreshold => {
                format!("{:.0} dB", self.voice_activation_threshold)
            }
        }
    }

    /// Turns the vertical sync on or off.
    pub const fn toggle_vsync(&mut self) {
        self.vsync = !self.vsync;
    }

    /// Shows or hides the frame-rate readout.
    pub const fn toggle_readout(&mut self) {
        self.readout_shown = !self.readout_shown;
    }

    /// Moves the readout one corner clockwise.
    pub const fn cycle_readout_corner(&mut self) {
        self.readout_corner = self.readout_corner.next();
    }

    /// Makes `control` answer to `key`, or refuses and changes nothing.
    pub fn rebind(&mut self, control: Control, key: KeyCode) -> Result<(), RebindRefusal> {
        self.bindings.rebind(control, key)
    }

    /// Puts `tab`'s settings back to their defaults, and **only** `tab`'s.
    ///
    /// **The obvious implementation is the bug.** `*self = Self::default()` passes any test
    /// that resets one tab and then looks at that tab; what it also does is throw away every
    /// key binding a player has ever set, silently, from a button labelled "reset graphics".
    /// So each arm names its own fields, [`Tab`] is matched with no wildcard, and the two
    /// directions are asserted separately in the tests below.
    ///
    /// **The bindings go back through [`Bindings::from_pairs`]**, the same whole-assignment
    /// validation `store` reads the file with, rather than a sequence of [`Self::rebind`]
    /// calls. A reset is exactly the operation that cannot be expressed one rebinding at a
    /// time: restoring a player who traded `Escape` and `Space` crosses over halfway
    /// through, and the second of the pair would be refused against a binding the first had
    /// just taken. The answer is a validated set or nothing at all — and `nothing at all` is
    /// [`Bindings::default`], which is what a reset was asking for anyway.
    pub fn reset(&mut self, tab: Tab) {
        let defaults = Self::default();
        match tab {
            Tab::Controls => {
                self.look_sensitivity = defaults.look_sensitivity;
                self.bindings =
                    Bindings::from_pairs(&CONTROLS.map(|control| (control, control.default_key())))
                        .unwrap_or_default();
            }
            Tab::Graphics => {
                self.window_mode = defaults.window_mode;
                self.monitor = defaults.monitor;
                self.readout_shown = defaults.readout_shown;
                self.readout_corner = defaults.readout_corner;
                self.render_distance = defaults.render_distance;
                self.field_of_view = defaults.field_of_view;
                self.vsync = defaults.vsync;
                self.frame_cap = defaults.frame_cap;
                self.brightness = defaults.brightness;
                self.fog_start = defaults.fog_start;
            }
            Tab::Audio => {
                self.master_volume = defaults.master_volume;
                self.output_device = defaults.output_device.clone();
                self.voice_mode = defaults.voice_mode;
                self.voice_activation_threshold = defaults.voice_activation_threshold;
            }
        }
    }

    /// Puts every value back inside its bound.
    ///
    /// Called on whatever [`store`] read, so a hand-edited file cannot put a reader
    /// outside the range this module promises it.
    fn clamp(&mut self) {
        self.look_sensitivity = self
            .look_sensitivity
            .clamp(MIN_LOOK_SENSITIVITY, MAX_LOOK_SENSITIVITY);
        self.render_distance = self
            .render_distance
            .clamp(MIN_RENDER_DISTANCE, MAX_RENDER_DISTANCE);
        self.field_of_view = self
            .field_of_view
            .clamp(MIN_FIELD_OF_VIEW, MAX_FIELD_OF_VIEW);
        self.brightness = self.brightness.clamp(MIN_BRIGHTNESS, MAX_BRIGHTNESS);
        self.fog_start = self.fog_start.clamp(MIN_FOG_START, MAX_FOG_START);
        if self.frame_cap != NO_FRAME_CAP {
            self.frame_cap = self.frame_cap.clamp(MIN_FRAME_CAP, MAX_FRAME_CAP);
        }
        self.master_volume = self
            .master_volume
            .clamp(MIN_MASTER_VOLUME, MAX_MASTER_VOLUME);
        // `NaN` compares false against both ends, so `clamp` would pass it through
        // untouched — the rule `client/AGENTS.md` states about non-finite floats. A
        // threshold that is not a number would make every level comparison false and turn
        // voice activation into a microphone that never opens, silently.
        if !self.voice_activation_threshold.is_finite() {
            self.voice_activation_threshold = DEFAULT_VOICE_ACTIVATION_THRESHOLD;
        }
        self.voice_activation_threshold = self.voice_activation_threshold.clamp(
            MIN_VOICE_ACTIVATION_THRESHOLD,
            MAX_VOICE_ACTIVATION_THRESHOLD,
        );
    }
}

/// `value` moved `steps` places, saturating at both ends of `low..=high`.
fn step_u8(value: u8, steps: i32, low: u8, high: u8) -> u8 {
    let moved = i32::from(value).saturating_add(steps);
    let clamped = moved.clamp(i32::from(low), i32::from(high));
    u8::try_from(clamped).unwrap_or(low)
}

/// The frame cap `steps` presses away from `current`.
///
/// Zero is "uncapped" rather than "nought frames a second", so it is not simply the bottom
/// of the range: stepping down off [`MIN_FRAME_CAP`] reaches it, and stepping up off it
/// lands back on [`MIN_FRAME_CAP`] instead of on ten.
fn step_frame_cap(current: u16, steps: i32) -> u16 {
    let mut cap = current;
    for _ in 0..steps.abs() {
        cap = match (steps > 0, cap) {
            (true, NO_FRAME_CAP) => MIN_FRAME_CAP,
            (true, held) => held.saturating_add(FRAME_CAP_STEP).min(MAX_FRAME_CAP),
            (false, NO_FRAME_CAP) => NO_FRAME_CAP,
            (false, held) if held <= MIN_FRAME_CAP => NO_FRAME_CAP,
            (false, held) => held.saturating_sub(FRAME_CAP_STEP).max(MIN_FRAME_CAP),
        };
    }
    cap
}

/// Rebuilds the dynamic monitor bound from the entities winit currently exposes.
fn refresh_monitor_choices(
    monitors: Query<(Entity, &Monitor, Has<PrimaryMonitor>)>,
    mut choices: ResMut<MonitorChoices>,
) {
    let found: Vec<_> = monitors
        .iter()
        .map(|(entity, monitor, primary)| {
            (
                entity,
                monitor.name.clone(),
                monitor.physical_width,
                monitor.physical_height,
                monitor.physical_position,
                primary,
            )
        })
        .collect();
    let mut attached: Vec<MonitorChoice> = found
        .iter()
        .map(|(entity, name, width, height, position, primary)| {
            let duplicate_name = name.as_ref().is_some_and(|name| {
                found
                    .iter()
                    .filter(|(_, candidate, ..)| candidate.as_ref() == Some(name))
                    .count()
                    > 1
            });
            let identity = monitor_identity(
                name.as_deref(),
                *width,
                *height,
                position.x,
                position.y,
                duplicate_name,
            );
            let name = ascii_monitor_name(name.as_deref());
            MonitorChoice {
                entity: *entity,
                identity,
                label: format!(
                    "{name} ({}x{} at {},{})",
                    width, height, position.x, position.y
                ),
                primary: *primary,
            }
        })
        .collect();
    attached.sort_by(|left, right| {
        right
            .primary
            .cmp(&left.primary)
            .then_with(|| left.label.cmp(&right.label))
            .then_with(|| left.identity.cmp(&right.identity))
    });
    if choices.attached != attached {
        choices.attached = attached;
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

/// Pushes the five settings that live in somebody else's component rather than being read
/// out of this resource every frame.
///
/// Cameras are queried by `Camera3d` rather than by another module's marker because the world
/// view and the view-model overlay must project the hand against the same field of view. This
/// module has no business depending on either marker. `WinitSettings` is absent in every
/// headless test, and a missing resource is "there is no window to pace", not a panic.
fn apply_to_the_display(
    settings: Res<Settings>,
    monitors: Res<MonitorChoices>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
    mut projections: Query<&mut Projection, With<Camera3d>>,
    winit: Option<ResMut<WinitSettings>>,
) {
    if !settings.is_changed() && !monitors.is_changed() {
        return;
    }

    let window_mode = settings
        .window_mode()
        .bevy(monitors.selection(settings.monitor()));
    let present_mode = if settings.vsync() {
        PresentMode::Fifo
    } else {
        PresentMode::AutoNoVsync
    };
    for mut window in &mut windows {
        if window.mode != window_mode {
            window.mode = window_mode;
        }
        if window.present_mode != present_mode {
            window.present_mode = present_mode;
        }
    }

    let fov = settings.field_of_view().to_radians();
    for mut projection in &mut projections {
        // Only the perspective case: an orthographic projection has no field of view, and
        // replacing one with a perspective because a slider moved would be this module
        // deciding what kind of camera the client has.
        if let Projection::Perspective(perspective) = projection.as_mut()
            && perspective.fov != fov
        {
            perspective.fov = fov;
        }
    }

    if let Some(mut winit) = winit {
        let focused = match settings.frame_cap() {
            NO_FRAME_CAP => UpdateMode::Continuous,
            cap => UpdateMode::reactive(std::time::Duration::from_secs_f32(1.0 / f32::from(cap))),
        };
        winit.focused_mode = focused;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::MAX_PITCH;

    /// Both machine-given bounds, with something on each: a knob whose values are the
    /// platform's has one choice when the platform offered nothing, and a bounds test over
    /// one choice cannot fail.
    fn attached() -> (MonitorChoices, AudioDevices) {
        (
            MonitorChoices::named(&["Main display", "Side display"]),
            AudioDevices::named(&["Built-in speakers", "USB headset"]),
        )
    }

    /// A client whose player never opens this screen renders and steers exactly as it did
    /// before the screen existed.
    #[test]
    fn the_defaults_are_what_the_client_had_before_this_screen_existed() {
        let settings = Settings::default();
        assert!((settings.look_sensitivity() - DEFAULT_LOOK_SENSITIVITY).abs() < f32::EPSILON);
        assert!((settings.fog_start() - DEFAULT_FOG_START).abs() < f32::EPSILON);
        assert!((settings.field_of_view() - DEFAULT_FIELD_OF_VIEW).abs() < f32::EPSILON);
        assert_eq!(settings.window_mode(), DisplayMode::BorderlessFullscreen);
        assert_eq!(settings.monitor(), &MonitorPreference::Primary);
        assert_eq!(settings.render_distance(), DEFAULT_RENDER_DISTANCE);
        assert_eq!(settings.frame_cap(), NO_FRAME_CAP);
        assert!(settings.vsync());
        assert!(!settings.readout_shown());
        assert_eq!(settings.master_volume(), DEFAULT_MASTER_VOLUME);
        assert_eq!(settings.output_device(), &DeviceChoice::SystemDefault);
        for (control, key) in [
            (Control::Forward, KeyCode::KeyW),
            (Control::Back, KeyCode::KeyS),
            (Control::Left, KeyCode::KeyA),
            (Control::Right, KeyCode::KeyD),
            (Control::Jump, KeyCode::Space),
            (Control::Interact, KeyCode::KeyF),
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
        let (monitors, devices) = attached();
        let bounds = Choices {
            monitors: &monitors,
            devices: &devices,
        };
        for knob in KNOBS {
            let mut low = Settings::default();
            low.adjust_with_choices(knob, -10_000, bounds);
            let mut high = Settings::default();
            high.adjust_with_choices(knob, 10_000, bounds);
            let mut lower = low.clone();
            lower.adjust_with_choices(knob, -1, bounds);
            let mut higher = high.clone();
            higher.adjust_with_choices(knob, 1, bounds);
            assert_eq!(low, lower, "{knob:?} kept falling past its floor");
            assert_eq!(high, higher, "{knob:?} kept climbing past its ceiling");
        }
    }

    #[test]
    fn the_frame_cap_steps_off_uncapped_rather_than_through_zero() {
        assert_eq!(step_frame_cap(NO_FRAME_CAP, 1), MIN_FRAME_CAP);
        assert_eq!(step_frame_cap(MIN_FRAME_CAP, -1), NO_FRAME_CAP);
        assert_eq!(step_frame_cap(NO_FRAME_CAP, -1), NO_FRAME_CAP);
        assert_eq!(
            step_frame_cap(MIN_FRAME_CAP, 1),
            MIN_FRAME_CAP + FRAME_CAP_STEP
        );
        assert_eq!(step_frame_cap(MAX_FRAME_CAP, 1), MAX_FRAME_CAP);
    }

    #[test]
    fn the_corners_cycle_through_all_four_and_come_back() {
        let mut corner = Corner::default();
        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.push(corner);
            corner = corner.next();
        }
        assert_eq!(corner, Corner::default(), "four steps is not a full turn");
        seen.sort_by_key(|corner| corner.name());
        seen.dedup();
        assert_eq!(seen.len(), 4, "the cycle misses a corner");
    }

    /// The assertion the issue asks for by name: what the slider writes is the client's own
    /// number. Nothing in this module takes a `SessionParams`, so there is no code path on
    /// which a welcome could reach the setting; the structural half of that claim is
    /// `no_setting_is_sourced_from_anything_the_server_sent` below, which is what keeps it
    /// true as the module grows.
    #[test]
    fn the_render_distance_is_the_clients_own_number() {
        let mut settings = Settings::default();
        assert_eq!(settings.render_distance(), DEFAULT_RENDER_DISTANCE);
        settings.adjust(Knob::RenderDistance, -3);
        assert_eq!(settings.render_distance(), DEFAULT_RENDER_DISTANCE - 3);

        // And the ceiling it stops at is the protocol's own, not a server's: a slider run
        // to its end asks for everything the most generous server could ever stream.
        settings.adjust(Knob::RenderDistance, 10_000);
        assert_eq!(settings.render_distance(), MAX_VIEW_DISTANCE);
    }

    /// The volume reaches silence and unity, and [`Settings::master_gain`] is the one
    /// conversion between the number a player reads and the one a sample is multiplied by.
    #[test]
    fn the_master_volume_reaches_silence_and_unity() {
        let mut settings = Settings::default();
        assert!((settings.master_gain() - 0.8).abs() < f32::EPSILON);

        settings.adjust(Knob::MasterVolume, -100);
        assert_eq!(settings.master_volume(), MIN_MASTER_VOLUME);
        assert!((settings.master_gain() - 0.0).abs() < f32::EPSILON);
        assert_eq!(settings.reading(Knob::MasterVolume), "0%");

        settings.adjust(Knob::MasterVolume, 100);
        assert_eq!(settings.master_volume(), MAX_MASTER_VOLUME);
        assert!((settings.master_gain() - 1.0).abs() < f32::EPSILON);
        assert_eq!(settings.reading(Knob::MasterVolume), "100%");
    }

    /// The device knob steps through what the machine has, stops at both ends of it, and
    /// keeps a device the machine no longer has rather than quietly replacing it.
    #[test]
    fn the_output_device_knob_steps_through_what_the_host_offers() {
        let (monitors, devices) = attached();
        let bounds = Choices {
            monitors: &monitors,
            devices: &devices,
        };
        let mut settings = Settings::default();
        assert_eq!(settings.output_device(), &DeviceChoice::SystemDefault);
        assert_eq!(
            settings.reading_with_choices(Knob::OutputDevice, bounds),
            "system default"
        );

        settings.adjust_with_choices(Knob::OutputDevice, 1, bounds);
        assert_eq!(
            settings.output_device(),
            &DeviceChoice::Named("Built-in speakers".to_owned())
        );
        assert_eq!(
            settings.reading_with_choices(Knob::OutputDevice, bounds),
            "Built-in speakers"
        );

        settings.adjust_with_choices(Knob::OutputDevice, 5, bounds);
        assert_eq!(
            settings.output_device(),
            &DeviceChoice::Named("USB headset".to_owned()),
            "stepping past the last device kept climbing"
        );
        settings.adjust_with_choices(Knob::OutputDevice, -5, bounds);
        assert_eq!(settings.output_device(), &DeviceChoice::SystemDefault);

        // **A device that is not here now is still the player's choice.** It stays
        // selected — so a headset plugged back in is used again without anybody reopening
        // this tab — and the row says out loud that it is not there.
        let one = AudioDevices::named(&["Built-in speakers"]);
        let unplugged = DeviceChoice::Named("USB headset".to_owned());
        assert_eq!(one.outputs().label(&unplugged), "USB headset (unavailable)");
        assert!(
            !one.outputs().choices().contains(&unplugged),
            "an absent device is not something stepping can land back on"
        );

        // A name the embedded font cannot draw is shown as ASCII rather than as a row of
        // invisible zero-advance glyphs. Built rather than written down, because
        // `ui/mod.rs`'s `ascii_guard` reads what a literal *produces* and would fail this
        // file for spelling it — which is the same reason the guard cannot see the real
        // case: a device name is the platform's and arrives at runtime.
        let awkward = format!("Hoerer {}ber USB", umlaut());
        assert_eq!(
            AudioDevices::named(&[awkward.as_str()])
                .outputs()
                .label(&DeviceChoice::Named(awkward)),
            "Hoerer ?ber USB"
        );
    }

    /// One character this client's only font has no glyph for.
    fn umlaut() -> char {
        char::from_u32(0xfc).expect("u+00fc is a scalar value")
    }

    /// A device name is a platform string with spaces and punctuation in it, and the
    /// settings file is one whitespace-separated value per setting. The encoding is what
    /// keeps the second true of the first.
    #[test]
    fn a_device_name_survives_the_one_field_the_file_gives_it() {
        for device in [
            DeviceChoice::SystemDefault,
            DeviceChoice::Named("HDA Intel PCH: ALC295 Analog".to_owned()),
            DeviceChoice::Named(format!("Hoerer {}ber USB", umlaut())),
        ] {
            let field = device_field(&device);
            assert!(
                !field.contains(char::is_whitespace),
                "{field} is more than one field"
            );
            assert_eq!(device_from_field(&field), Some(device));
        }

        for nonsense in ["", "name:", "name:zz", "name:6", "speakers", "name"] {
            assert_eq!(device_from_field(nonsense), None, "{nonsense}");
        }
    }

    /// **The control the microphone is held with, end to end through the model.** It is the
    /// thirteenth entry, and where it sits is the assertion that matters: [`Bindings`]
    /// indexes by `control as usize`, so a control *inserted* rather than appended hands
    /// every control below it somebody else's key while every reading still looks right.
    #[test]
    fn talk_is_appended_last_and_starts_on_v() {
        assert_eq!(CONTROLS.len(), 13);
        assert_eq!(CONTROLS[CONTROLS.len() - 1], Control::Talk);
        assert_eq!(Control::Talk as usize, CONTROLS.len() - 1);

        let mut settings = Settings::default();
        assert_eq!(settings.bindings().key(Control::Talk), KeyCode::KeyV);
        assert_eq!(key_name(KeyCode::KeyV), Some("v"));
        assert_eq!(Control::Talk.label(), "Push to talk");

        // It is a control like any other: a key another control holds is refused, a free
        // one is taken, and the tab's own reset puts it back.
        assert_eq!(
            settings.rebind(Control::Talk, KeyCode::KeyE),
            Err(RebindRefusal::WouldUnbind(Control::Inventory))
        );
        assert_eq!(settings.bindings().key(Control::Talk), KeyCode::KeyV);
        settings
            .rebind(Control::Talk, KeyCode::KeyB)
            .expect("b is free");
        assert_eq!(settings.bindings().key(Control::Talk), KeyCode::KeyB);
        settings.reset(Tab::Controls);
        assert_eq!(settings.bindings().key(Control::Talk), KeyCode::KeyV);
    }

    /// The mode knob's whole bound: three values, stopping at each end, starting on the one
    /// that needs a key held.
    #[test]
    fn the_voice_mode_knob_steps_between_three_values_and_starts_on_push_to_talk() {
        let mut settings = Settings::default();
        assert_eq!(settings.voice_mode(), VoiceMode::PushToTalk);
        assert_eq!(settings.reading(Knob::VoiceMode), "push to talk");

        settings.adjust(Knob::VoiceMode, 1);
        assert_eq!(settings.voice_mode(), VoiceMode::VoiceActivation);
        assert_eq!(settings.reading(Knob::VoiceMode), "voice activation");

        // Past the top is the top, not a wrap: a knob that wrapped would take a player who
        // pressed once too often from "voice activation" to "off" without saying so.
        settings.adjust(Knob::VoiceMode, 4);
        assert_eq!(settings.voice_mode(), VoiceMode::VoiceActivation);

        settings.adjust(Knob::VoiceMode, -9);
        assert_eq!(settings.voice_mode(), VoiceMode::Off);
        assert_eq!(settings.reading(Knob::VoiceMode), "off");

        settings.reset(Tab::Audio);
        assert_eq!(settings.voice_mode(), VoiceMode::PushToTalk);
    }

    /// The threshold is decibels, bounded at both ends, and reversible — the property the
    /// snap in [`Settings::adjust_with_choices`] exists for.
    #[test]
    fn the_voice_activation_threshold_is_bounded_decibels_and_a_step_is_reversible() {
        let mut settings = Settings::default();
        assert!((settings.voice_activation_threshold_db() + 40.0).abs() < f32::EPSILON);
        assert_eq!(settings.reading(Knob::VoiceActivationThreshold), "-40 dB");

        settings.adjust(Knob::VoiceActivationThreshold, 1);
        assert_eq!(settings.reading(Knob::VoiceActivationThreshold), "-38 dB");
        settings.adjust(Knob::VoiceActivationThreshold, -1);
        assert!(
            (settings.voice_activation_threshold_db() + 40.0).abs() < f32::EPSILON,
            "a step up and back down landed on {}",
            settings.voice_activation_threshold_db()
        );

        settings.adjust(Knob::VoiceActivationThreshold, 100);
        assert!(
            (settings.voice_activation_threshold_db() - MAX_VOICE_ACTIVATION_THRESHOLD).abs()
                < f32::EPSILON
        );
        assert_eq!(settings.reading(Knob::VoiceActivationThreshold), "-10 dB");
        settings.adjust(Knob::VoiceActivationThreshold, -100);
        assert!(
            (settings.voice_activation_threshold_db() - MIN_VOICE_ACTIVATION_THRESHOLD).abs()
                < f32::EPSILON
        );
        assert_eq!(settings.reading(Knob::VoiceActivationThreshold), "-60 dB");

        settings.reset(Tab::Audio);
        assert!((settings.voice_activation_threshold_db() + 40.0).abs() < f32::EPSILON);
    }

    /// **A threshold that is not a number is the default, never itself.** `clamp` compares
    /// false against both ends for `NaN`, so the bound alone would pass one straight
    /// through — and a threshold nothing compares greater than is a microphone that never
    /// opens, with nothing on screen saying why.
    #[test]
    fn a_threshold_that_is_not_a_number_does_not_survive_the_clamp() {
        for hand_edited in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1_000.0, -999.0] {
            let mut settings = Settings {
                voice_activation_threshold: hand_edited,
                ..Settings::default()
            };
            settings.clamp();
            let held = settings.voice_activation_threshold_db();
            assert!(
                held.is_finite()
                    && (MIN_VOICE_ACTIVATION_THRESHOLD..=MAX_VOICE_ACTIVATION_THRESHOLD)
                        .contains(&held),
                "{hand_edited} became {held}"
            );
        }
    }

    /// Every knob is on exactly one tab, so exactly one reset owns it. Without this a knob
    /// added to the model but forgotten in [`Knob::tab`] would simply never be resettable —
    /// which no other assertion here would notice.
    #[test]
    fn every_knob_belongs_to_a_tab_and_every_tab_has_a_reset_of_its_own() {
        let (monitors, devices) = attached();
        let bounds = Choices {
            monitors: &monitors,
            devices: &devices,
        };
        for tab in Tab::ALL {
            let mut moved = Settings::default();
            for knob in KNOBS.into_iter().filter(|knob| knob.tab() == tab) {
                moved.adjust_with_choices(knob, 3, bounds);
            }
            assert_ne!(moved, Settings::default(), "{tab:?} moved no knob at all");
            moved.reset(tab);
            for knob in KNOBS.into_iter().filter(|knob| knob.tab() == tab) {
                assert_eq!(
                    moved.reading_with_choices(knob, bounds),
                    Settings::default().reading_with_choices(knob, bounds),
                    "{knob:?} survived its own tab's reset"
                );
            }
        }
    }

    /// **The assertion the obvious implementation fails.** A reset that wrote
    /// `Settings::default()` back would pass every check on the tab it was asked about and
    /// would quietly throw away the other tab — so both directions are asserted, and each
    /// starts from a state where *both* tabs have been moved.
    #[test]
    fn a_reset_puts_back_its_own_tab_and_touches_no_other() {
        let moved = || {
            let mut settings = Settings::default();
            let (monitors, devices) = attached();
            let bounds = Choices {
                monitors: &monitors,
                devices: &devices,
            };
            settings.adjust(Knob::LookSensitivity, 4);
            settings
                .rebind(Control::Forward, KeyCode::F6)
                .expect("f6 is free");
            settings.adjust(Knob::RenderDistance, -3);
            settings.adjust(Knob::FieldOfView, 2);
            settings.adjust(Knob::Brightness, -2);
            settings.adjust(Knob::FogStart, 2);
            settings.adjust(Knob::FrameCap, 3);
            settings.adjust_with_choices(Knob::WindowMode, 1, bounds);
            settings.adjust_with_choices(Knob::Monitor, 1, bounds);
            settings.adjust(Knob::MasterVolume, -3);
            settings.adjust_with_choices(Knob::OutputDevice, 2, bounds);
            settings.adjust(Knob::VoiceMode, 1);
            settings.adjust(Knob::VoiceActivationThreshold, -2);
            settings.toggle_vsync();
            settings.toggle_readout();
            settings.cycle_readout_corner();
            settings.set_default_mount(DefaultMount::Brown);
            settings
        };

        // Graphics back, controls untouched.
        let before = moved();
        let mut after = moved();
        after.reset(Tab::Graphics);
        assert_eq!(after.render_distance(), DEFAULT_RENDER_DISTANCE);
        assert_eq!(after.window_mode(), DEFAULT_WINDOW_MODE);
        assert_eq!(after.monitor(), &DEFAULT_MONITOR);
        assert_eq!(after.frame_cap(), NO_FRAME_CAP);
        assert!(after.vsync());
        assert!(!after.readout_shown());
        assert_eq!(after.readout_corner(), Corner::default());
        assert!((after.field_of_view() - DEFAULT_FIELD_OF_VIEW).abs() < f32::EPSILON);
        assert!((after.fog_start() - DEFAULT_FOG_START).abs() < f32::EPSILON);
        assert!((after.brightness() - 1.0).abs() < f32::EPSILON);
        assert_eq!(
            after.bindings(),
            before.bindings(),
            "resetting graphics cleared a key binding"
        );
        assert!(
            (after.look_sensitivity() - before.look_sensitivity()).abs() < f32::EPSILON,
            "resetting graphics moved the mouse sensitivity"
        );
        assert_eq!(
            after.master_volume(),
            before.master_volume(),
            "resetting graphics moved the master volume"
        );
        assert_eq!(
            after.output_device(),
            before.output_device(),
            "resetting graphics moved the output device"
        );
        assert_eq!(
            after.voice_mode(),
            before.voice_mode(),
            "resetting graphics moved the voice mode"
        );

        // And the mirror: controls back, graphics untouched.
        let mut after = moved();
        after.reset(Tab::Controls);
        assert_eq!(*after.bindings(), Bindings::default());
        assert!((after.look_sensitivity() - DEFAULT_LOOK_SENSITIVITY).abs() < f32::EPSILON);
        for knob in KNOBS.into_iter().filter(|knob| knob.tab() == Tab::Graphics) {
            assert_eq!(
                after.reading(knob),
                before.reading(knob),
                "resetting controls moved {knob:?}"
            );
        }
        assert_eq!(after.vsync(), before.vsync());
        assert_eq!(after.readout_shown(), before.readout_shown());
        assert_eq!(after.readout_corner(), before.readout_corner());
        assert_eq!(after.window_mode(), before.window_mode());
        assert_eq!(after.monitor(), before.monitor());
        assert_eq!(after.default_mount(), before.default_mount());
        assert_eq!(after.master_volume(), before.master_volume());
        assert_eq!(after.output_device(), before.output_device());
        assert_eq!(after.voice_mode(), before.voice_mode());

        // And the third tab: audio back, the other two untouched.
        let mut after = moved();
        after.reset(Tab::Audio);
        assert_eq!(after.master_volume(), DEFAULT_MASTER_VOLUME);
        assert_eq!(after.output_device(), &DeviceChoice::SystemDefault);
        assert_eq!(after.voice_mode(), DEFAULT_VOICE_MODE);
        assert!(
            (after.voice_activation_threshold_db() - DEFAULT_VOICE_ACTIVATION_THRESHOLD).abs()
                < f32::EPSILON
        );
        assert_eq!(
            after.bindings(),
            before.bindings(),
            "resetting audio cleared a key binding"
        );
        for knob in KNOBS.into_iter().filter(|knob| knob.tab() == Tab::Graphics) {
            assert_eq!(
                after.reading(knob),
                before.reading(knob),
                "resetting audio moved {knob:?}"
            );
        }
        assert_eq!(after.vsync(), before.vsync());
        assert_eq!(after.readout_corner(), before.readout_corner());
    }

    /// **A reset is a whole assignment or nothing**, which is why it goes through
    /// [`Bindings::from_pairs`]. Two controls that have traded keys cannot be restored one
    /// rebinding at a time — the first of the pair takes a key the second still holds — so a
    /// reset built out of [`Settings::rebind`] calls would leave a control on whatever it
    /// happened to be on, and could leave two controls sharing one key.
    #[test]
    fn a_reset_restores_two_controls_that_have_traded_keys() {
        let mut settings = Settings::default();
        settings
            .rebind(Control::Menu, KeyCode::KeyG)
            .expect("g is free");
        settings
            .rebind(Control::Jump, KeyCode::Escape)
            .expect("escape is free once the menu has left it");
        settings
            .rebind(Control::Menu, KeyCode::Space)
            .expect("space is free once jump has left it");
        assert_eq!(settings.bindings().key(Control::Jump), KeyCode::Escape);
        assert_eq!(settings.bindings().key(Control::Menu), KeyCode::Space);

        settings.reset(Tab::Controls);
        assert_eq!(settings.bindings().key(Control::Jump), KeyCode::Space);
        assert_eq!(settings.bindings().key(Control::Menu), KeyCode::Escape);

        // And whatever it restored is a legal assignment: one offered key per control, and
        // no control left unreachable.
        let mut keys = CONTROLS.map(|control| settings.bindings().key(control));
        for key in keys {
            assert!(key_name(key).is_some(), "{key:?} is not an offered key");
        }
        keys.sort_by_key(|key| key_name(*key).unwrap_or_default());
        let mut distinct = keys.to_vec();
        distinct.dedup();
        assert_eq!(distinct.len(), CONTROLS.len(), "a reset shared a key");
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
        assert_eq!(settings.rebind(Control::Forward, KeyCode::F6), Ok(()));
        assert_eq!(settings.bindings().key(Control::Forward), KeyCode::F6);

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

    /// The consume control starts on `C`, trades keys under the same invariants as any
    /// other, and comes back with a Controls reset.
    ///
    /// It is the newest member of the closed set, so it is also the one whose default is
    /// most likely to collide with a key something else already reads. `key_name` is the
    /// other half of that claim: a control that started on a key the screen will not bind
    /// could never be rebound off it.
    #[test]
    fn consuming_starts_on_c_and_a_reset_puts_it_back() {
        let mut settings = Settings::default();
        assert_eq!(settings.bindings().key(Control::Consume), KeyCode::KeyC);
        assert!(key_name(KeyCode::KeyC).is_some(), "consume starts hidden");
        assert_eq!(Control::Consume.label(), "Consume item");

        // A key another control holds is refused, and refusing changes nothing.
        assert_eq!(
            settings.rebind(Control::Consume, KeyCode::KeyE),
            Err(RebindRefusal::WouldUnbind(Control::Inventory))
        );
        assert_eq!(settings.bindings().key(Control::Consume), KeyCode::KeyC);

        // A free one is taken, and the key it left becomes free for somebody else. Not
        // `KeyV`: `Control::Talk` has answered to it since #852.
        settings
            .rebind(Control::Consume, KeyCode::KeyB)
            .expect("b is free");
        assert_eq!(settings.bindings().key(Control::Consume), KeyCode::KeyB);
        settings
            .rebind(Control::Chat, KeyCode::KeyC)
            .expect("c is free once consume has left it");

        settings.reset(Tab::Controls);
        assert_eq!(settings.bindings().key(Control::Consume), KeyCode::KeyC);
        assert_eq!(settings.bindings().key(Control::Chat), KeyCode::KeyT);
    }

    /// [`CONTROLS`] lists the controls in their declaration order, because [`Bindings`]
    /// indexes by `control as usize` while [`Bindings::default`] fills that array with
    /// `CONTROLS.map`. A control listed out of order hands every control below it the
    /// wrong default, and nothing else in this module would say so.
    #[test]
    fn the_control_list_is_in_the_same_order_as_the_enum() {
        for (index, control) in CONTROLS.into_iter().enumerate() {
            assert_eq!(
                control as usize, index,
                "{control:?} is listed at {index} and declared at {}",
                control as usize
            );
        }
        for control in CONTROLS {
            assert_eq!(
                Bindings::default().key(control),
                control.default_key(),
                "{control:?} does not start on its own default"
            );
            assert_eq!(
                Control::from_name(control.name()),
                Some(control),
                "{control:?} does not read back from its own file name"
            );
        }
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

    fn monitor(name: &str, width: u32, x: i32) -> Monitor {
        Monitor {
            name: Some(name.to_owned()),
            physical_height: 1080,
            physical_width: width,
            physical_position: IVec2::new(x, 0),
            refresh_rate_millihertz: Some(60_000),
            scale_factor: 1.0,
            video_modes: Vec::new(),
        }
    }

    #[test]
    fn a_missing_saved_monitor_falls_back_without_erasing_the_choice() {
        let scratch = store::Scratch::new("settings-monitor-fallback");
        let path = scratch.join("settings");
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, SettingsPlugin::at(path.clone())));
        app.world_mut()
            .spawn((monitor("Main display", 1920, 0), PrimaryMonitor));
        let side = app
            .world_mut()
            .spawn(monitor("Side display", 2560, 1920))
            .id();
        let window = app
            .world_mut()
            .spawn((Window::default(), PrimaryWindow))
            .id();

        app.update();
        assert_eq!(
            app.world().entity(window).get::<Window>().unwrap().mode,
            WindowMode::BorderlessFullscreen(MonitorSelection::Primary)
        );

        let choices = app.world().resource::<MonitorChoices>().clone();
        app.world_mut()
            .resource_mut::<Settings>()
            .adjust_with_choices(Knob::Monitor, 1, Choices::with_monitors(&choices));
        let saved = app.world().resource::<Settings>().monitor().clone();
        assert!(matches!(saved, MonitorPreference::Specific(_)));
        app.update();
        assert_eq!(
            app.world().entity(window).get::<Window>().unwrap().mode,
            WindowMode::BorderlessFullscreen(MonitorSelection::Entity(side))
        );

        app.world_mut().entity_mut(side).despawn();
        app.update();
        assert_eq!(
            app.world().entity(window).get::<Window>().unwrap().mode,
            WindowMode::BorderlessFullscreen(MonitorSelection::Primary),
            "a disconnected display did not fall back to the primary"
        );
        assert_eq!(
            app.world().resource::<Settings>().monitor(),
            &saved,
            "the fallback erased the preference instead of only changing the applied monitor"
        );
        let (reloaded, complaints) = store::load(&path);
        assert_eq!(complaints, Vec::<String>::new(), "{complaints:?}");
        assert_eq!(
            reloaded.monitor(),
            &saved,
            "falling back rewrote the saved preference"
        );
    }

    /// What the dropdown offers, and what each entry reads — both answered from live
    /// monitors only, which is what keeps the closed control the only place an unavailable
    /// saved preference is still visible.
    #[test]
    fn the_dropdown_offers_primary_and_every_live_display_by_its_own_label() {
        let monitors = MonitorChoices::named(&["Main display", "Side display"]);
        let preferences = monitors.preferences();
        assert_eq!(preferences.len(), 2, "{preferences:?}");
        assert_eq!(preferences[0], MonitorPreference::Primary);
        assert!(matches!(preferences[1], MonitorPreference::Specific(_)));

        assert_eq!(
            monitors.option_label(&MonitorPreference::Primary),
            "Primary"
        );
        assert_eq!(
            monitors.option_label(&preferences[1]),
            "Side display (1920x1080 at 1920,0)"
        );

        // A preference the dropdown does not offer answers the empty string here rather
        // than inventing a name — `(unavailable)` is the closed control's own suffix.
        let vanished = MonitorPreference::Specific("name:6e6f7065".to_owned());
        assert_eq!(monitors.option_label(&vanished), "");
    }

    /// The select assigns directly; it does not step. And it is the only way to leave an
    /// unavailable saved preference behind — the model has no other setter for this field.
    #[test]
    fn set_monitor_replaces_the_preference_directly_including_an_unavailable_one() {
        let monitors = MonitorChoices::named(&["Main display", "Side display"]);
        let mut settings = Settings::default();
        assert_eq!(settings.monitor(), &MonitorPreference::Primary);

        let side = monitors.preferences()[1].clone();
        settings.set_monitor(side.clone());
        assert_eq!(settings.monitor(), &side);

        let unavailable = MonitorPreference::Specific("name:6c6f7374".to_owned());
        settings.set_monitor(unavailable.clone());
        assert_eq!(settings.monitor(), &unavailable);

        settings.set_monitor(MonitorPreference::Primary);
        assert_eq!(settings.monitor(), &MonitorPreference::Primary);
    }

    #[test]
    fn the_initial_window_uses_the_saved_mode_before_the_first_update() {
        let scratch = store::Scratch::new("settings-initial-window-mode");
        let path = scratch.join("settings");
        let monitors = MonitorChoices::default();
        let mut settings = Settings::default();
        settings.adjust_with_choices(Knob::WindowMode, 1, Choices::with_monitors(&monitors));
        store::save(&path, &settings).expect("save a window mode");

        assert_eq!(
            SettingsPlugin::at(path).initial_window_mode(),
            WindowMode::Windowed
        );
        assert_eq!(
            SettingsPlugin::from_file(None).initial_window_mode(),
            WindowMode::BorderlessFullscreen(MonitorSelection::Primary)
        );
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
