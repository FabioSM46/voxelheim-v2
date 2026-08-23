//! The settings screen, behind the pause menu.
//!
//! Every control here writes one field of [`Settings`] and nothing else: no socket, no
//! copy of a server's answer, no outcome. The values, their bounds and their steps live in
//! `crate::settings`; this is the panel that says "up" and "down" to them.
//!
//! **While it is up, `ui/mod.rs`'s `choose_input_mode` reads no key at all** and
//! [`read_settings_keys`] runs after it — so the press that closes this screen cannot also
//! resume play, and the key being bound cannot also fire the control it is taken from.

use bevy::prelude::*;

use super::{BUTTON, button_colour};
use crate::player::InputMode;
use crate::settings::{CONTROLS, Control, KNOBS, Knob, Settings, key_name};

/// Whether the screen is up, and what it is waiting for.
#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct SettingsScreen {
    /// Whether the panel is drawn. Set by the pause menu's Settings entry.
    open: bool,
    /// The control whose next key press is its new binding, while one is waiting.
    capturing: Option<Control>,
    /// The line under the panel: what was refused, or what is being waited for.
    notice: String,
}

impl SettingsScreen {
    /// Puts the screen up. The pause menu's Settings entry is the one caller.
    pub(super) fn open(&mut self) {
        self.open = true;
        self.capturing = None;
        self.notice.clear();
    }

    /// Whether the panel is drawn. `ui/menu.rs` reads it to stand down while it is, and
    /// `ui/mod.rs` reads it to leave the keyboard alone.
    pub(super) const fn is_open(&self) -> bool {
        self.open
    }

    /// Takes the screen down, forgetting anything it was waiting for.
    fn close(&mut self) {
        self.open = false;
        self.capturing = None;
        self.notice.clear();
    }
}

/// Draws the settings screen and keeps it in step with [`Settings`].
pub(super) struct SettingsScreenPlugin;

impl Plugin for SettingsScreenPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SettingsScreen>()
            .init_resource::<Settings>()
            .add_systems(Startup, spawn_settings_screen)
            // Chained, and the order is what makes a press readable on the frame it
            // happened: the two systems that change something run before the one that draws
            // it. Unchained they may run in either order, which shows up as a rebinding
            // whose row still names the old key.
            .add_systems(
                Update,
                (
                    show_settings_screen,
                    settings_actions,
                    // After the input mode, so the frame that closes this screen is a
                    // frame `choose_input_mode` has already declined to read.
                    read_settings_keys.after(crate::player::ApplyInputMode),
                    refresh_readings,
                )
                    .chain(),
            );
    }
}

/// Marks the whole overlay.
#[derive(Component)]
struct SettingsRoot;

/// What pressing a control on this screen means.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsAction {
    /// Move one numeric setting by the given number of its own steps.
    Nudge(Knob, i32),
    /// Wait for the next key press and give it to this control.
    Capture(Control),
    /// Back to the pause menu.
    Back,
}

/// What a piece of text on this screen is showing.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
enum Reading {
    Knob(Knob),
    Binding(Control),
    /// The line under the panel.
    Notice,
}

/// The width of one column of rows, in logical pixels.
const COLUMN: f32 = 300.0;

/// The width of a stepper's `-` or `+` button, in logical pixels.
const STEP_BUTTON: f32 = 30.0;

/// The height of every control on this screen, in logical pixels.
const ROW_HEIGHT: f32 = 28.0;

/// Font size for a row's label and its reading.
const ROW_FONT: FontSize = FontSize::Px(15.0);

fn spawn_settings_screen(mut commands: Commands) {
    commands
        .spawn((
            SettingsRoot,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(Color::srgba(0.012, 0.016, 0.024, 0.96)),
            Visibility::Hidden,
            // Above the pause menu's 40: the two are never up together, and a screen that
            // sorted *under* the one it replaces would be a panel nobody could press.
            GlobalZIndex(45),
        ))
        .with_children(|overlay| {
            overlay
                .spawn((
                    Node {
                        display: Display::Flex,
                        flex_direction: FlexDirection::Column,
                        row_gap: Val::Px(10.0),
                        padding: UiRect::all(Val::Px(24.0)),
                        border_radius: BorderRadius::all(Val::Px(8.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.065, 0.075, 0.095)),
                ))
                .with_children(|panel| {
                    panel.spawn((
                        Text::new("SETTINGS"),
                        TextFont {
                            font_size: FontSize::Px(26.0),
                            ..default()
                        },
                        TextColor(Color::WHITE),
                        TextShadow::default(),
                        Node {
                            align_self: AlignSelf::Center,
                            margin: UiRect {
                                bottom: Val::Px(6.0),
                                ..default()
                            },
                            ..default()
                        },
                    ));

                    panel
                        .spawn(Node {
                            width: Val::Px(COLUMN),
                            display: Display::Flex,
                            flex_direction: FlexDirection::Column,
                            row_gap: Val::Px(6.0),
                            ..default()
                        })
                        .with_children(spawn_rows);

                    panel.spawn((
                        Reading::Notice,
                        Text::new(String::new()),
                        TextFont {
                            font_size: ROW_FONT,
                            ..default()
                        },
                        // The aiming outline's amber, the colour `ui/status.rs` answers a
                        // refusal in. A refused rebinding is the same kind of answer.
                        TextColor(Color::linear_rgb(1.0, 0.72, 0.25)),
                        Node {
                            align_self: AlignSelf::Center,
                            min_height: Val::Px(20.0),
                            ..default()
                        },
                    ));

                    spawn_button(
                        panel,
                        SettingsAction::Back,
                        Val::Percent(100.0),
                        Face::Fixed("BACK"),
                    );
                });
        });
}

/// Every row on the panel: the numbers, each between a `-` and a `+`, then one button per
/// rebindable control whose face is the key it currently answers to.
fn spawn_rows(column: &mut ChildSpawnerCommands<'_>) {
    for knob in KNOBS {
        spawn_row(column, knob.label(), |controls| {
            spawn_button(
                controls,
                SettingsAction::Nudge(knob, -1),
                Val::Px(STEP_BUTTON),
                Face::Fixed("-"),
            );
            spawn_reading(controls, Reading::Knob(knob));
            spawn_button(
                controls,
                SettingsAction::Nudge(knob, 1),
                Val::Px(STEP_BUTTON),
                Face::Fixed("+"),
            );
        });
    }
    for control in CONTROLS {
        spawn_row(column, control.label(), |controls| {
            spawn_button(
                controls,
                SettingsAction::Capture(control),
                Val::Px(STEP_BUTTON * 4.0),
                Face::Value(Reading::Binding(control)),
            );
        });
    }
}

/// A labelled row whose controls `fill` puts on the right.
fn spawn_row(
    column: &mut ChildSpawnerCommands<'_>,
    label: &str,
    fill: impl FnOnce(&mut ChildSpawnerCommands<'_>),
) {
    column
        .spawn(Node {
            display: Display::Flex,
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::SpaceBetween,
            height: Val::Px(ROW_HEIGHT),
            ..default()
        })
        .with_children(|row| {
            row.spawn((
                Text::new(label.to_owned()),
                TextFont {
                    font_size: ROW_FONT,
                    ..default()
                },
                TextColor(Color::srgb(0.78, 0.80, 0.84)),
            ));
            row.spawn(Node {
                display: Display::Flex,
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(6.0),
                ..default()
            })
            .with_children(fill);
        });
}

/// The number between a `-` and a `+`.
fn spawn_reading(parent: &mut ChildSpawnerCommands<'_>, reading: Reading) {
    parent.spawn((
        reading,
        Text::new(String::new()),
        TextFont {
            font_size: ROW_FONT,
            ..default()
        },
        TextColor(Color::WHITE),
        Node {
            width: Val::Px(72.0),
            justify_content: JustifyContent::Center,
            ..default()
        },
    ));
}

/// One pressable control on this screen.
///
/// The three shapes differ only in width and in what is written on them: a `-` or `+`
/// beside a number, a wide face that *is* the value it changes, and the entry at the foot
/// of the panel. `face` carries the difference.
fn spawn_button(
    parent: &mut ChildSpawnerCommands<'_>,
    action: SettingsAction,
    width: Val,
    face: Face,
) {
    let (height, font) = match width {
        Val::Percent(_) => (40.0, FontSize::Px(18.0)),
        _ => (ROW_HEIGHT - 4.0, ROW_FONT),
    };
    let mut button = parent.spawn((
        action,
        Button,
        Node {
            width,
            height: Val::Px(height),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            border_radius: BorderRadius::all(Val::Px(3.0)),
            ..default()
        },
        BackgroundColor(BUTTON),
    ));
    let text = (
        Text::new(face.label()),
        TextFont {
            font_size: font,
            ..default()
        },
        TextColor(Color::WHITE),
    );
    match face {
        Face::Fixed(_) => button.with_child(text),
        Face::Value(reading) => button.with_child((reading, text)),
    };
}

/// What is written on a button: a label that never changes, or a value that does.
#[derive(Debug, Clone, Copy)]
enum Face {
    Fixed(&'static str),
    Value(Reading),
}

impl Face {
    /// The text to spawn with. A [`Face::Value`] starts empty and is filled by
    /// [`refresh_readings`] on the first frame.
    fn label(self) -> String {
        match self {
            Self::Fixed(label) => label.to_owned(),
            Self::Value(_) => String::new(),
        }
    }
}

/// Shows the panel while the screen is open, and takes it down with the pause menu.
fn show_settings_screen(
    mode: Res<InputMode>,
    mut screen: ResMut<SettingsScreen>,
    mut roots: Query<&mut Visibility, With<SettingsRoot>>,
) {
    // Leaving `Menu` is leaving this screen: a disconnect, a death or a resume all take
    // the pause menu away, and a settings panel that outlived it would be a screen the
    // player is standing behind while the world runs.
    if screen.is_open() && *mode != InputMode::Menu {
        screen.close();
    }

    let next = if screen.is_open() {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    for mut visibility in &mut roots {
        if *visibility != next {
            *visibility = next;
        }
    }
}

type SettingsButton<'a> = (&'a Interaction, &'a SettingsAction, &'a mut BackgroundColor);

/// Applies a press.
fn settings_actions(
    mut buttons: Query<SettingsButton<'_>, (Changed<Interaction>, With<Button>)>,
    mut settings: ResMut<Settings>,
    mut screen: ResMut<SettingsScreen>,
) {
    for (interaction, action, mut colour) in &mut buttons {
        colour.0 = button_colour(interaction);
        if *interaction != Interaction::Pressed {
            continue;
        }
        match *action {
            SettingsAction::Nudge(knob, steps) => settings.adjust(knob, steps),
            SettingsAction::Capture(control) => {
                // A second press on the row that is already waiting takes the request
                // back. It is the cancel `Escape` used to be, moved onto the mouse that
                // armed the capture in the first place — see [`read_settings_keys`].
                if screen.capturing == Some(control) {
                    screen.capturing = None;
                    screen.notice.clear();
                } else {
                    screen.capturing = Some(control);
                    screen.notice = format!("press a key for {}", control.label().to_lowercase());
                }
                continue;
            }
            SettingsAction::Back => {
                screen.close();
                continue;
            }
        }
        // A press that changed a value answers whatever the last one was refused for.
        if screen.capturing.is_none() && !screen.notice.is_empty() {
            screen.notice.clear();
        }
    }
}

/// Gives the next key press to the control that is waiting, or closes the screen.
///
/// **`Escape` is a key like any other while a capture is waiting**, and that is deliberate
/// rather than incidental. `crate::settings` offers it, [`Control::Menu`] *starts* on it,
/// and the file round-trips it — so a screen that swallowed every press of it would be a
/// screen that could never put the pause menu back where a player found it. The model would
/// go on saying the key was free while the panel silently disagreed, which is the one shape
/// of bug a settings screen must not have.
///
/// What the screen keeps for itself is the *other* state. With nothing waiting, `Escape`
/// takes the panel down, so a player who has bound the pause menu somewhere unfortunate
/// always has a way out of here. A capture is taken back by pressing its own row again,
/// which is the same mouse that armed it and is on screen the whole time — `BACK` closes
/// the panel outright and forgets the capture too.
fn read_settings_keys(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    mut settings: ResMut<Settings>,
    mut screen: ResMut<SettingsScreen>,
) {
    if !screen.is_open() {
        return;
    }
    let Some(keys) = keys else {
        return;
    };

    let Some(control) = screen.capturing else {
        if keys.just_pressed(KeyCode::Escape) {
            screen.close();
        }
        return;
    };
    let Some(pressed) = keys.get_just_pressed().next().copied() else {
        return;
    };

    match settings.rebind(control, pressed) {
        Ok(()) => {
            screen.capturing = None;
            screen.notice.clear();
        }
        // The capture stays open on a refusal: the player asked to rebind something and
        // has not managed to yet, so the screen keeps waiting and says why the last key
        // did not do it. The binding they were trying to change is untouched.
        Err(refusal) => screen.notice = refusal.sentence(),
    }
}

/// Keeps every value on the panel in step with the settings behind it.
fn refresh_readings(
    settings: Res<Settings>,
    screen: Res<SettingsScreen>,
    mut readings: Query<(&Reading, &mut Text)>,
) {
    if !settings.is_changed() && !screen.is_changed() {
        return;
    }
    for (reading, mut text) in &mut readings {
        let next = describe(&settings, &screen, *reading);
        if text.0 != next {
            text.0 = next;
        }
    }
}

/// What one piece of text on the panel says. A pure function of the two resources, so the
/// panel's content is testable with no window.
fn describe(settings: &Settings, screen: &SettingsScreen, reading: Reading) -> String {
    match reading {
        Reading::Knob(knob) => settings.reading(knob),
        Reading::Binding(control) if screen.capturing == Some(control) => "...".to_owned(),
        Reading::Binding(control) => key_name(settings.bindings().key(control))
            .unwrap_or("unbound")
            .to_owned(),
        Reading::Notice => screen.notice.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Knob;

    fn screen_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<InputMode>()
            .insert_resource(ButtonInput::<KeyCode>::default())
            .add_plugins(SettingsScreenPlugin);
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Menu;
        app.world_mut().resource_mut::<SettingsScreen>().open();
        app.update();
        app
    }

    fn press(app: &mut App, wanted: SettingsAction) {
        let button = {
            let world = app.world_mut();
            let mut query = world.query::<(Entity, &SettingsAction)>();
            query
                .iter(world)
                .find(|(_, action)| **action == wanted)
                .map(|(entity, _)| entity)
                .unwrap_or_else(|| panic!("no control for {wanted:?}"))
        };
        *app.world_mut()
            .entity_mut(button)
            .get_mut::<Interaction>()
            .expect("a button has an interaction") = Interaction::Pressed;
        app.update();
        *app.world_mut()
            .entity_mut(button)
            .get_mut::<Interaction>()
            .expect("a button has an interaction") = Interaction::None;
    }

    /// One press of `key`, from nothing held.
    ///
    /// `reset_all` and not `clear`: `ButtonInput::press` marks a key *just* pressed only
    /// if it was not already held, and no `InputPlugin` is built here to release it — so a
    /// second press of the same key would otherwise be no press at all.
    fn press_key(app: &mut App, key: KeyCode) {
        {
            let mut input = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
            input.reset_all();
            input.press(key);
        }
        app.update();
    }

    /// Lets go of everything held, so the next `app.update()` sees no press at all.
    ///
    /// A test that presses a button after pressing a key needs it: nothing in this app
    /// clears `ButtonInput` between frames, so a key stays *just* pressed for every later
    /// update and would be read as the answer to the capture the button just armed.
    fn release_keys(app: &mut App) {
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .reset_all();
    }

    fn reading_of(app: &mut App, wanted: Reading) -> String {
        let world = app.world_mut();
        let mut query = world.query::<(&Reading, &Text)>();
        query
            .iter(world)
            .find(|(reading, _)| **reading == wanted)
            .map(|(_, text)| text.0.clone())
            .unwrap_or_else(|| panic!("no reading for {wanted:?}"))
    }

    #[test]
    fn every_knob_has_a_control_at_each_end_and_a_reading_between_them() {
        let mut app = screen_app();
        for knob in KNOBS {
            let before = app.world().resource::<Settings>().clone();
            press(&mut app, SettingsAction::Nudge(knob, 1));
            let after = app.world().resource::<Settings>().clone();
            assert_ne!(before, after, "{knob:?} did not move");
            assert_eq!(
                reading_of(&mut app, Reading::Knob(knob)),
                after.reading(knob)
            );

            press(&mut app, SettingsAction::Nudge(knob, -1));
            assert_eq!(
                *app.world().resource::<Settings>(),
                before,
                "{knob:?} did not come back"
            );
        }
    }

    #[test]
    fn a_capture_takes_the_next_free_key() {
        let mut app = screen_app();
        press(&mut app, SettingsAction::Capture(Control::Forward));
        assert_eq!(
            reading_of(&mut app, Reading::Binding(Control::Forward)),
            "..."
        );

        press_key(&mut app, KeyCode::KeyT);
        assert_eq!(
            app.world()
                .resource::<Settings>()
                .bindings()
                .key(Control::Forward),
            KeyCode::KeyT
        );
        assert_eq!(
            reading_of(&mut app, Reading::Binding(Control::Forward)),
            "t"
        );
        assert_eq!(reading_of(&mut app, Reading::Notice), "");
    }

    /// The refusal the issue asks for, all the way through the screen: the player cannot
    /// press their way into a state where a control has no key.
    #[test]
    fn a_capture_that_would_strand_a_control_is_refused_and_says_so() {
        let mut app = screen_app();
        press(&mut app, SettingsAction::Capture(Control::Menu));
        press_key(&mut app, KeyCode::KeyW);

        let settings = app.world().resource::<Settings>().clone();
        assert_eq!(settings.bindings().key(Control::Menu), KeyCode::Escape);
        assert_eq!(settings.bindings().key(Control::Forward), KeyCode::KeyW);
        let notice = reading_of(&mut app, Reading::Notice);
        assert!(notice.contains("unreachable"), "{notice}");

        // Still waiting, so the next key can be a good one.
        press_key(&mut app, KeyCode::KeyG);
        assert_eq!(
            app.world()
                .resource::<Settings>()
                .bindings()
                .key(Control::Menu),
            KeyCode::KeyG
        );
        assert_eq!(reading_of(&mut app, Reading::Notice), "");
    }

    /// With nothing waiting, `Escape` is the way out of this screen — the one state it is
    /// kept for.
    #[test]
    fn escape_closes_the_screen_when_nothing_is_waiting() {
        let mut app = screen_app();
        press_key(&mut app, KeyCode::Escape);
        assert!(!app.world().resource::<SettingsScreen>().is_open());
    }

    /// `crate::settings` offers `Escape` and [`Control::Menu`] starts on it, so this screen
    /// has to be able to hand it back — otherwise a player who moved the pause menu could
    /// never move it home, and the model would go on offering a key the panel refused.
    /// While the menu still answers to `Escape` it is refused like any other taken key.
    #[test]
    fn escape_is_bindable_through_the_screen_once_the_menu_has_left_it() {
        let mut app = screen_app();

        press(&mut app, SettingsAction::Capture(Control::Jump));
        press_key(&mut app, KeyCode::Escape);
        assert_eq!(
            app.world()
                .resource::<Settings>()
                .bindings()
                .key(Control::Jump),
            KeyCode::Space,
            "escape stranded the pause menu instead of being refused"
        );
        let notice = reading_of(&mut app, Reading::Notice);
        assert!(notice.contains("unreachable"), "{notice}");
        assert!(app.world().resource::<SettingsScreen>().is_open());

        // Moving the menu off `Escape` is what frees it.
        release_keys(&mut app);
        press(&mut app, SettingsAction::Capture(Control::Menu));
        press_key(&mut app, KeyCode::KeyM);
        assert_eq!(
            app.world()
                .resource::<Settings>()
                .bindings()
                .key(Control::Menu),
            KeyCode::KeyM
        );

        // And now the key this screen used to swallow reaches the control that asked for it.
        release_keys(&mut app);
        press(&mut app, SettingsAction::Capture(Control::Jump));
        press_key(&mut app, KeyCode::Escape);
        assert_eq!(
            app.world()
                .resource::<Settings>()
                .bindings()
                .key(Control::Jump),
            KeyCode::Escape,
            "escape was swallowed instead of captured"
        );
        assert_eq!(
            reading_of(&mut app, Reading::Binding(Control::Jump)),
            "escape"
        );
        assert!(
            app.world().resource::<SettingsScreen>().is_open(),
            "the captured escape also closed the screen"
        );
    }

    /// A capture is taken back by pressing its own row again — the cancel `Escape` used to
    /// be, on the mouse that armed it.
    #[test]
    fn pressing_a_waiting_row_again_takes_the_capture_back() {
        let mut app = screen_app();
        press(&mut app, SettingsAction::Capture(Control::Jump));
        assert_eq!(reading_of(&mut app, Reading::Binding(Control::Jump)), "...");

        press(&mut app, SettingsAction::Capture(Control::Jump));
        assert_eq!(
            reading_of(&mut app, Reading::Binding(Control::Jump)),
            "space"
        );
        assert_eq!(reading_of(&mut app, Reading::Notice), "");

        // Nothing is waiting again, so `Escape` is the way out rather than a binding.
        press_key(&mut app, KeyCode::Escape);
        assert!(!app.world().resource::<SettingsScreen>().is_open());
        assert_eq!(
            app.world()
                .resource::<Settings>()
                .bindings()
                .key(Control::Jump),
            KeyCode::Space
        );
    }

    #[test]
    fn back_closes_the_screen_and_leaves_the_settings_alone() {
        let mut app = screen_app();
        press(&mut app, SettingsAction::Nudge(Knob::LookSensitivity, 1));
        let changed = app.world().resource::<Settings>().clone();

        press(&mut app, SettingsAction::Back);
        assert!(!app.world().resource::<SettingsScreen>().is_open());
        assert_eq!(*app.world().resource::<Settings>(), changed);
    }

    #[test]
    fn leaving_the_pause_menu_takes_the_screen_with_it() {
        let mut app = screen_app();
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        app.update();
        assert!(!app.world().resource::<SettingsScreen>().is_open());
    }
}
