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
use bevy::window::{PresentMode, PrimaryWindow};
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
        .add_systems(Update, (save_when_changed, apply_to_the_display));
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
    /// The six graphics values and the frame-rate readout.
    Graphics,
}

impl Tab {
    /// Every tab, in the order the strip draws them.
    ///
    /// A hand-written list, for the reason `ui/inventory.rs`'s `InventoryTab::ALL` is one:
    /// no stable Rust enumerates an enum's variants. What keeps it honest is that
    /// [`Self::label`] and [`Settings::reset`] both match with no wildcard arm, so a third
    /// tab is a build failure until it has a name and a set of defaults of its own.
    pub const ALL: [Self; 2] = [Self::Controls, Self::Graphics];

    /// What a player reads on the tab.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Controls => "CONTROLS",
            Self::Graphics => "GRAPHICS",
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
    Inventory,
    Menu,
}

/// Every control, in the order the settings screen lists them.
pub const CONTROLS: [Control; 8] = [
    Control::Forward,
    Control::Back,
    Control::Left,
    Control::Right,
    Control::Jump,
    Control::Chat,
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
            Self::Chat => "chat",
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
            Self::Chat => "Chat",
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
            Self::Chat => KeyCode::KeyT,
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
/// how far up that is and where it stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Knob {
    LookSensitivity,
    RenderDistance,
    FieldOfView,
    Brightness,
    FogStart,
    FrameCap,
}

/// Every knob, in the order the settings screen lists them.
pub const KNOBS: [Knob; 6] = [
    Knob::LookSensitivity,
    Knob::RenderDistance,
    Knob::FieldOfView,
    Knob::Brightness,
    Knob::FogStart,
    Knob::FrameCap,
];

impl Knob {
    /// What the settings screen calls it.
    pub const fn label(self) -> &'static str {
        match self {
            Self::LookSensitivity => "Mouse sensitivity",
            Self::RenderDistance => "Render distance",
            Self::FieldOfView => "Field of view",
            Self::Brightness => "Brightness",
            Self::FogStart => "Fog starts at",
            Self::FrameCap => "Frame cap",
        }
    }

    /// Which tab this knob is listed on, and which reset therefore puts it back.
    ///
    /// No wildcard arm, so a seventh knob has to say where it belongs before it builds —
    /// which is the same thing as saying which reset owns it.
    pub const fn tab(self) -> Tab {
        match self {
            Self::LookSensitivity => Tab::Controls,
            Self::RenderDistance
            | Self::FieldOfView
            | Self::Brightness
            | Self::FogStart
            | Self::FrameCap => Tab::Graphics,
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
    readout_shown: bool,
    readout_corner: Corner,
    render_distance: u8,
    field_of_view: f32,
    vsync: bool,
    frame_cap: u16,
    brightness: f32,
    fog_start: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            look_sensitivity: DEFAULT_LOOK_SENSITIVITY,
            bindings: Bindings::default(),
            readout_shown: false,
            readout_corner: Corner::default(),
            render_distance: DEFAULT_RENDER_DISTANCE,
            field_of_view: DEFAULT_FIELD_OF_VIEW,
            vsync: true,
            frame_cap: NO_FRAME_CAP,
            brightness: 1.0,
            fog_start: DEFAULT_FOG_START,
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
        }
    }

    /// What the settings screen prints beside `knob`.
    pub fn reading(&self, knob: Knob) -> String {
        match knob {
            Knob::LookSensitivity => format!("{:.4}", self.look_sensitivity),
            Knob::RenderDistance => format!("{} chunks", self.render_distance),
            Knob::FieldOfView => format!("{:.0}°", self.field_of_view),
            Knob::Brightness => format!("{:.2}x", self.brightness),
            Knob::FogStart => format!("{:.0}%", self.fog_start * 100.0),
            Knob::FrameCap if self.frame_cap == NO_FRAME_CAP => "uncapped".to_owned(),
            Knob::FrameCap => format!("{} fps", self.frame_cap),
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
                self.readout_shown = defaults.readout_shown;
                self.readout_corner = defaults.readout_corner;
                self.render_distance = defaults.render_distance;
                self.field_of_view = defaults.field_of_view;
                self.vsync = defaults.vsync;
                self.frame_cap = defaults.frame_cap;
                self.brightness = defaults.brightness;
                self.fog_start = defaults.fog_start;
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

/// Pushes the three settings that live in somebody else's component rather than being read
/// out of this resource every frame.
///
/// The camera is queried by `Camera3d` rather than by `player/camera.rs`'s own marker,
/// because this module has no business depending on that one and there is exactly one camera
/// in this client by rule. `WinitSettings` is absent in every headless test, and a missing
/// resource is "there is no window to pace", not a panic.
fn apply_to_the_display(
    settings: Res<Settings>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
    mut projections: Query<&mut Projection, With<Camera3d>>,
    winit: Option<ResMut<WinitSettings>>,
) {
    if !settings.is_changed() {
        return;
    }

    let present_mode = if settings.vsync() {
        PresentMode::Fifo
    } else {
        PresentMode::AutoNoVsync
    };
    for mut window in &mut windows {
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

    /// A client whose player never opens this screen renders and steers exactly as it did
    /// before the screen existed.
    #[test]
    fn the_defaults_are_what_the_client_had_before_this_screen_existed() {
        let settings = Settings::default();
        assert!((settings.look_sensitivity() - DEFAULT_LOOK_SENSITIVITY).abs() < f32::EPSILON);
        assert!((settings.fog_start() - DEFAULT_FOG_START).abs() < f32::EPSILON);
        assert!((settings.field_of_view() - DEFAULT_FIELD_OF_VIEW).abs() < f32::EPSILON);
        assert_eq!(settings.render_distance(), DEFAULT_RENDER_DISTANCE);
        assert_eq!(settings.frame_cap(), NO_FRAME_CAP);
        assert!(settings.vsync());
        assert!(!settings.readout_shown());
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

    /// Every knob is on exactly one tab, so exactly one reset owns it. Without this a knob
    /// added to the model but forgotten in [`Knob::tab`] would simply never be resettable —
    /// which no other assertion here would notice.
    #[test]
    fn every_knob_belongs_to_a_tab_and_every_tab_has_a_reset_of_its_own() {
        for tab in Tab::ALL {
            let mut moved = Settings::default();
            for knob in KNOBS.into_iter().filter(|knob| knob.tab() == tab) {
                moved.adjust(knob, 3);
            }
            assert_ne!(moved, Settings::default(), "{tab:?} moved no knob at all");
            moved.reset(tab);
            for knob in KNOBS.into_iter().filter(|knob| knob.tab() == tab) {
                assert_eq!(
                    moved.reading(knob),
                    Settings::default().reading(knob),
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
            settings.adjust(Knob::LookSensitivity, 4);
            settings
                .rebind(Control::Forward, KeyCode::F6)
                .expect("f6 is free");
            settings.adjust(Knob::RenderDistance, -3);
            settings.adjust(Knob::FieldOfView, 2);
            settings.adjust(Knob::Brightness, -2);
            settings.adjust(Knob::FogStart, 2);
            settings.adjust(Knob::FrameCap, 3);
            settings.toggle_vsync();
            settings.toggle_readout();
            settings.cycle_readout_corner();
            settings
        };

        // Graphics back, controls untouched.
        let before = moved();
        let mut after = moved();
        after.reset(Tab::Graphics);
        assert_eq!(after.render_distance(), DEFAULT_RENDER_DISTANCE);
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
