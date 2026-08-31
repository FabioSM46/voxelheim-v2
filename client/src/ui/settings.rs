//! The settings screen, behind the pause menu.
//!
//! Every control here writes one field of [`Settings`] and nothing else: no socket, no
//! copy of a server's answer, no outcome. The values, their bounds and their steps live in
//! `crate::settings`; this is the panel that says "up" and "down" to them.
//!
//! **While it is up, `ui/mod.rs`'s `choose_input_mode` reads no key at all** and
//! [`read_settings_keys`] runs after it — so the press that closes this screen cannot also
//! resume play, and the key being bound cannot also fire the control it is taken from.
//!
//! **The screen is two tabs and the area under them never changes size.** That is a stated
//! layout decision rather than a coincidence: `ui/inventory.rs` lays its strip out above a
//! column whose height is whatever the visible half needs, so the panel — strip included —
//! moves when a player switches tabs, which is what #251 is about. Here the content area is
//! [`CONTENT_HEIGHT`] tall whichever tab is up, and
//! `no_tab_needs_more_rows_than_the_area_it_is_drawn_in` is what keeps that true when a row
//! is added.

use bevy::prelude::*;

use super::{BUTTON, TAB_SELECTED, button_colour};
use crate::player::InputMode;
use crate::settings::{CONTROLS, Control, KNOBS, Knob, MonitorChoices, Settings, Tab, key_name};

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
            .init_resource::<MonitorChoices>()
            .init_resource::<Tab>()
            .add_systems(Startup, spawn_settings_screen)
            // Chained, and the order is what makes a press readable on the frame it
            // happened: the two systems that change something run before the one that draws
            // it. Unchained they may run in either order, which shows up as a rebinding
            // whose row still names the old key.
            .add_systems(
                Update,
                (
                    show_settings_screen,
                    switch_settings_tabs,
                    show_the_active_settings_tab,
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

/// Marks the strip of tabs, whose place on the panel never changes.
#[derive(Component)]
struct TabStrip;

/// Marks the fixed-height area a tab's contents are drawn in. See [`CONTENT_HEIGHT`].
#[derive(Component)]
struct TabContent;

/// One tab in the strip: the half it selects.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct TabButton(Tab);

/// The container holding one tab's rows, shown by `Display` and never by `Visibility`.
///
/// `bevy_ui` lays a hidden node out exactly as it lays out a visible one, so a `Visibility`
/// here would leave the graphics rows occupying the area's height while the controls are up
/// — the same trap `ui/inventory.rs` and `ui/character.rs` both record for their own halves.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct TabPanel(Tab);

/// The fixed left-hand column of a settings row.
#[derive(Component)]
struct RowLabel;

/// The fixed right-hand column containing a row's buttons and readings.
#[derive(Component)]
struct RowControls;

/// What pressing a control on this screen means.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsAction {
    /// Move one numeric setting by the given number of its own steps.
    Nudge(Knob, i32),
    ToggleVsync,
    ToggleReadout,
    CycleCorner,
    /// Wait for the next key press and give it to this control.
    Capture(Control),
    /// Put one tab's settings back to their defaults — **and only that tab's**.
    Reset(Tab),
    /// Back to the pause menu.
    Back,
}

/// What a piece of text on this screen is showing.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
enum Reading {
    Knob(Knob),
    Vsync,
    Readout,
    ReadoutCorner,
    Binding(Control),
    /// The line under the panel.
    Notice,
}

/// The narrowest supported viewport, in logical pixels.
const MIN_VIEWPORT_WIDTH: f32 = 800.0;

/// The padding on each side of the panel.
const PANEL_PADDING: f32 = 24.0;

/// The width of one column of rows, in logical pixels.
const COLUMN: f32 = 700.0;

/// The fixed width of a row label, in logical pixels.
const ROW_LABEL_WIDTH: f32 = 160.0;

/// The width of a stepper's `-` or `+` button, in logical pixels.
const STEP_BUTTON: f32 = 30.0;

/// The fixed width of a knob reading, in logical pixels.
const READING_WIDTH: f32 = 440.0;

/// The gap between neighbouring controls in one row.
const CONTROL_GAP: f32 = 6.0;

/// The width reserved for a complete stepper: both buttons, both gaps and its reading.
const STEPPER_WIDTH: f32 = 2.0 * STEP_BUTTON + 2.0 * CONTROL_GAP + READING_WIDTH;

/// The separation between the label and control columns.
const ROW_COLUMN_GAP: f32 = COLUMN - ROW_LABEL_WIDTH - STEPPER_WIDTH;

const _: () = {
    assert!(ROW_COLUMN_GAP > 0.0, "the two row columns must not overlap");
    assert!(
        COLUMN + 2.0 * PANEL_PADDING <= MIN_VIEWPORT_WIDTH,
        "the settings panel must fit its narrowest supported viewport"
    );
};

/// The height of every control on this screen, in logical pixels.
const ROW_HEIGHT: f32 = 28.0;

/// The gap between two rows, in logical pixels.
const ROW_GAP: f32 = 6.0;

/// The height of a full-width control — the reset at the foot of a tab, and `BACK`.
const WIDE_BUTTON: f32 = 40.0;

/// The most rows any one tab may draw.
///
/// **The number the layout is sized from, and the reason the strip does not move.** Controls
/// is the taller tab — one knob and every entry in `CONTROLS` — and a content area sized to
/// "whatever this tab needs" would be stable purely by that coincidence, until another row
/// arrived on one of them, which is how `ui/inventory.rs` ended up with the geometry #251
/// describes. `no_tab_needs_more_rows_than_the_area_it_is_drawn_in` fails rather than the
/// panel jumping, and #399 is what made it fail: adding `Control::Consume` grew Controls to
/// eleven rows, so this number moved with it rather than the area silently overflowing.
/// #452 moved it again, to twelve, for `Control::Map`.
const CONTENT_ROWS: usize = 12;

/// The height of the area a tab's contents are drawn in, in logical pixels.
const CONTENT_HEIGHT: f32 = CONTENT_ROWS as f32 * (ROW_HEIGHT + ROW_GAP) + WIDE_BUTTON;

/// Font size for a row's label and its reading.
const ROW_FONT_SIZE: f32 = 15.0;
const ROW_FONT: FontSize = FontSize::Px(ROW_FONT_SIZE);

/// What a tab's reset button says.
///
/// Its own sentence rather than a bare `RESET`, because the one thing a player must not have
/// to guess is how far the button reaches. No wildcard arm, so a third tab has to name itself
/// here before it builds.
const fn reset_label(tab: Tab) -> &'static str {
    match tab {
        Tab::Controls => "RESET CONTROLS",
        Tab::Graphics => "RESET GRAPHICS",
    }
}

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
                        padding: UiRect::all(Val::Px(PANEL_PADDING)),
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

                    spawn_tab_strip(panel);

                    // The fixed-height area. Its height is [`CONTENT_HEIGHT`] and not
                    // "whatever the visible tab needs", which is the whole of what keeps the
                    // strip above it still when the tab changes.
                    panel
                        .spawn((
                            TabContent,
                            Node {
                                width: Val::Px(COLUMN),
                                height: Val::Px(CONTENT_HEIGHT),
                                display: Display::Flex,
                                flex_direction: FlexDirection::Column,
                                ..default()
                            },
                        ))
                        .with_children(|content| {
                            for tab in Tab::ALL {
                                content
                                    .spawn((
                                        TabPanel(tab),
                                        // Set by `show_the_active_settings_tab` from the
                                        // frame the screen is built, so this is only what it
                                        // starts as.
                                        Node {
                                            display: Display::None,
                                            flex_direction: FlexDirection::Column,
                                            row_gap: Val::Px(ROW_GAP),
                                            height: Val::Percent(100.0),
                                            ..default()
                                        },
                                    ))
                                    .with_children(|column| spawn_tab_rows(column, tab));
                            }
                        });

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

/// The strip of tabs, built from [`Tab::ALL`] so a third one appears in it by existing.
fn spawn_tab_strip(panel: &mut ChildSpawnerCommands<'_>) {
    panel
        .spawn((
            TabStrip,
            Node {
                display: Display::Flex,
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(ROW_GAP),
                ..default()
            },
        ))
        .with_children(|strip| {
            for tab in Tab::ALL {
                strip
                    .spawn((
                        TabButton(tab),
                        Button,
                        Node {
                            padding: UiRect::axes(Val::Px(14.0), Val::Px(7.0)),
                            border_radius: BorderRadius::all(Val::Px(4.0)),
                            ..default()
                        },
                        BackgroundColor(BUTTON),
                    ))
                    .with_child((
                        Text::new(tab.label()),
                        TextFont {
                            font_size: FontSize::Px(18.0),
                            ..default()
                        },
                        TextColor(Color::WHITE),
                        TextShadow::default(),
                    ));
            }
        });
}

/// One row on a tab: what it is, and therefore what sits on its right.
#[derive(Debug, Clone, Copy)]
enum Row {
    /// A number between a `-` and a `+`.
    Knob(Knob),
    /// A flag or a cycle: one button whose face is the value pressing it changes.
    Toggle(&'static str, SettingsAction, Reading),
    /// A rebindable control, whose button face is the key it answers to.
    Binding(Control),
}

impl Row {
    /// What the row is called on its left.
    const fn label(self) -> &'static str {
        match self {
            Self::Knob(knob) => knob.label(),
            Self::Toggle(label, _, _) => label,
            Self::Binding(control) => control.label(),
        }
    }
}

/// Every row `tab` draws, in order.
///
/// **One list, read by the spawner and by the test that holds a tab inside its area**, so
/// "how tall is this tab" has one answer rather than two that drift. The knobs come from
/// [`Knob::tab`], which is the same statement [`Settings::reset`] scopes itself by — a knob
/// cannot appear on one tab and be reset by the other.
fn rows_of(tab: Tab) -> Vec<Row> {
    let mut rows: Vec<Row> = KNOBS
        .into_iter()
        .filter(|knob| knob.tab() == tab)
        .map(Row::Knob)
        .collect();
    match tab {
        Tab::Controls => rows.extend(CONTROLS.into_iter().map(Row::Binding)),
        Tab::Graphics => rows.extend([
            Row::Toggle("Vertical sync", SettingsAction::ToggleVsync, Reading::Vsync),
            Row::Toggle(
                "FPS readout",
                SettingsAction::ToggleReadout,
                Reading::Readout,
            ),
            Row::Toggle(
                "Readout corner",
                SettingsAction::CycleCorner,
                Reading::ReadoutCorner,
            ),
        ]),
    }
    rows
}

/// One tab's rows, and the reset that puts exactly those rows back.
fn spawn_tab_rows(column: &mut ChildSpawnerCommands<'_>, tab: Tab) {
    for row in rows_of(tab) {
        spawn_row(column, row.label(), |controls| match row {
            Row::Knob(knob) => {
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
            }
            Row::Toggle(_, action, reading) => {
                spawn_button(
                    controls,
                    action,
                    Val::Px(STEP_BUTTON * 4.0),
                    Face::Value(reading),
                );
            }
            Row::Binding(control) => {
                spawn_button(
                    controls,
                    SettingsAction::Capture(control),
                    Val::Px(STEP_BUTTON * 4.0),
                    Face::Value(Reading::Binding(control)),
                );
            }
        });
    }

    // At the foot of its own tab, and pushed there by the free space rather than by a count
    // of rows: a tab with fewer rows than [`CONTENT_ROWS`] still puts its reset where the
    // other tab's is.
    spawn_button(
        column,
        SettingsAction::Reset(tab),
        Val::Percent(100.0),
        Face::Fixed(reset_label(tab)),
    );
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
            column_gap: Val::Px(ROW_COLUMN_GAP),
            height: Val::Px(ROW_HEIGHT),
            ..default()
        })
        .with_children(|row| {
            row.spawn((
                RowLabel,
                Text::new(label.to_owned()),
                TextFont {
                    font_size: ROW_FONT,
                    ..default()
                },
                TextColor(Color::srgb(0.78, 0.80, 0.84)),
                TextLayout::no_wrap(),
                Node {
                    width: Val::Px(ROW_LABEL_WIDTH),
                    flex_shrink: 0.0,
                    overflow: Overflow::clip(),
                    ..default()
                },
            ));
            row.spawn((
                RowControls,
                Node {
                    width: Val::Px(STEPPER_WIDTH),
                    display: Display::Flex,
                    flex_direction: FlexDirection::Row,
                    flex_shrink: 0.0,
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::FlexEnd,
                    column_gap: Val::Px(CONTROL_GAP),
                    ..default()
                },
            ))
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
        TextLayout::no_wrap().with_justify(Justify::Center),
        Node {
            width: Val::Px(READING_WIDTH),
            flex_shrink: 0.0,
            overflow: Overflow::clip(),
            ..default()
        },
    ));
}

/// One pressable control on this screen.
///
/// The three shapes differ only in width and in what is written on them: a `-` or `+` beside
/// a number, a wide face that *is* the value it changes, and a full-width entry at the foot
/// of the column it is in. `face` carries the difference.
fn spawn_button(
    parent: &mut ChildSpawnerCommands<'_>,
    action: SettingsAction,
    width: Val,
    face: Face,
) {
    let full_width = matches!(width, Val::Percent(_));
    let (height, font) = if full_width {
        (WIDE_BUTTON, FontSize::Px(18.0))
    } else {
        (ROW_HEIGHT - 4.0, ROW_FONT)
    };
    let mut button = parent.spawn((
        action,
        Button,
        Node {
            width,
            height: Val::Px(height),
            // A full-width control sits at the foot of its column: the auto margin takes
            // whatever space the rows above did not, which is what puts a tab's reset in the
            // same place whether that tab drew eight rows or three. In a column with no free
            // space — the panel `BACK` sits in — an auto margin is simply nought.
            margin: if full_width {
                UiRect {
                    top: Val::Auto,
                    ..default()
                }
            } else {
                UiRect::default()
            },
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
        TextLayout::no_wrap().with_justify(Justify::Center),
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

/// Reads the tab strip, and paints it.
///
/// **Originates nothing**, exactly as `ui/inventory.rs`'s does: a press writes a resource,
/// and which rows have a `Display` is the whole of what changes.
///
/// **The tab is not reset when the screen reopens**, which is the one place this differs from
/// that screen. A pack is a thing a player came to look at and the crafting list is a
/// detour; two settings tabs are two halves of one errand, and a player who was adjusting the
/// graphics and stepped back out to look at something should find the graphics again. Nothing
/// here depends on it either way — there is no reset for a stray press to undo.
///
/// **A waiting capture does not survive the switch**, and that is the one thing here that is
/// not merely painting. A capture is armed over a row on the tab it was armed from; carry it
/// to the other tab and the next key press rebinds a control the player can no longer see,
/// while the notice under the panel goes on asking for a key beside rows that have nothing to
/// do with it. The binding it overwrites is one nobody chose to change. `SettingsAction::Reset`
/// takes a capture back for the same reason and says so where it does it — the state stops
/// being readable, so it stops being armed.
fn switch_settings_tabs(
    mut tabs: Query<(&TabButton, &Interaction, &mut BackgroundColor)>,
    mut active: ResMut<Tab>,
    mut screen: ResMut<SettingsScreen>,
) {
    for (tab, interaction, _) in &tabs {
        if *interaction == Interaction::Pressed && *active != tab.0 {
            *active = tab.0;
            // Guarded rather than assigned unconditionally: `ResMut` marks the resource
            // changed on any deref, and a screen that reported a change on every tab press
            // would wake every reader of it for nothing.
            if screen.capturing.is_some() {
                screen.capturing = None;
            }
        }
    }

    for (tab, interaction, mut colour) in &mut tabs {
        // The selected tab keeps its own colour under the pointer too: a tab that lit up like
        // an unselected one while hovered would read as though pressing it did something, and
        // it does not.
        let next = if tab.0 == *active {
            TAB_SELECTED
        } else {
            button_colour(interaction)
        };
        if colour.0 != next {
            colour.0 = next;
        }
    }
}

/// Gives the active tab a `Display` and takes it from the others.
///
/// `Display`, never `Visibility` — see [`TabPanel`].
fn show_the_active_settings_tab(active: Res<Tab>, mut panels: Query<(&TabPanel, &mut Node)>) {
    for (panel, mut node) in &mut panels {
        let next = if panel.0 == *active {
            Display::Flex
        } else {
            Display::None
        };
        // Written only on a change, because `Mut<Node>` marks the component changed on the
        // first `DerefMut` and `bevy_ui` lays a changed node's subtree out again.
        if node.display != next {
            node.display = next;
        }
    }
}

type SettingsButton<'a> = (&'a Interaction, &'a SettingsAction, &'a mut BackgroundColor);

/// Applies a press.
fn settings_actions(
    mut buttons: Query<SettingsButton<'_>, (Changed<Interaction>, With<Button>)>,
    monitors: Res<MonitorChoices>,
    mut settings: ResMut<Settings>,
    mut screen: ResMut<SettingsScreen>,
) {
    for (interaction, action, mut colour) in &mut buttons {
        colour.0 = button_colour(interaction);
        if *interaction != Interaction::Pressed {
            continue;
        }
        match *action {
            SettingsAction::Nudge(knob, steps) => {
                settings.adjust_with_monitors(knob, steps, &monitors);
            }
            SettingsAction::ToggleVsync => settings.toggle_vsync(),
            SettingsAction::ToggleReadout => settings.toggle_readout(),
            SettingsAction::CycleCorner => settings.cycle_readout_corner(),
            SettingsAction::Reset(tab) => {
                settings.reset(tab);
                // A capture is taken back by the reset, and has to be: it was armed over a
                // binding that has just been replaced, so the next key press would answer a
                // question the player can no longer see the state of.
                screen.capturing = None;
            }
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
    monitors: Res<MonitorChoices>,
    screen: Res<SettingsScreen>,
    mut readings: Query<(&Reading, &mut Text)>,
) {
    if !settings.is_changed() && !monitors.is_changed() && !screen.is_changed() {
        return;
    }
    for (reading, mut text) in &mut readings {
        let next = describe(&settings, &monitors, &screen, *reading);
        if text.0 != next {
            text.0 = next;
        }
    }
}

/// What one piece of text on the panel says. A pure function of the two resources, so the
/// panel's content is testable with no window.
fn describe(
    settings: &Settings,
    monitors: &MonitorChoices,
    screen: &SettingsScreen,
    reading: Reading,
) -> String {
    match reading {
        Reading::Knob(knob) => settings.reading_with_monitors(knob, monitors),
        Reading::Vsync => on_or_off(settings.vsync()),
        Reading::Readout => on_or_off(settings.readout_shown()),
        Reading::ReadoutCorner => settings.readout_corner().name().to_owned(),
        Reading::Binding(control) if screen.capturing == Some(control) => "...".to_owned(),
        Reading::Binding(control) => key_name(settings.bindings().key(control))
            .unwrap_or("unbound")
            .to_owned(),
        Reading::Notice => screen.notice.clone(),
    }
}

/// How a flag reads on a button face.
fn on_or_off(flag: bool) -> String {
    if flag { "on" } else { "off" }.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{Corner, Knob};
    use crate::ui::health::DEFAULT_FONT_ADVANCE_EM;

    fn screen_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<InputMode>()
            .insert_resource(ButtonInput::<KeyCode>::default())
            .insert_resource(MonitorChoices::named(&["Main display", "Side display"]))
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

    /// Presses one tab in the strip and lets go of it.
    fn press_tab(app: &mut App, wanted: Tab) {
        let button = {
            let world = app.world_mut();
            let mut query = world.query::<(Entity, &TabButton)>();
            query
                .iter(world)
                .find(|(_, tab)| tab.0 == wanted)
                .map(|(entity, _)| entity)
                .unwrap_or_else(|| panic!("no tab for {wanted:?}"))
        };
        *app.world_mut()
            .entity_mut(button)
            .get_mut::<Interaction>()
            .expect("a tab is a button") = Interaction::Pressed;
        app.update();
        *app.world_mut()
            .entity_mut(button)
            .get_mut::<Interaction>()
            .expect("a tab is a button") = Interaction::None;
        app.update();
    }

    /// The node of the one entity carrying `C`.
    fn marker_node<C: Component>(app: &mut App) -> Node {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Node, With<C>>();
        let found: Vec<Node> = query.iter(world).cloned().collect();
        assert_eq!(found.len(), 1, "exactly one node carries this marker");
        found.into_iter().next().expect("just counted one")
    }

    /// Which tabs' rows occupy the content area, read the way `bevy_ui` decides it.
    fn shown_tabs(app: &mut App) -> Vec<Tab> {
        let world = app.world_mut();
        let mut query = world.query::<(&TabPanel, &Node)>();
        let mut tabs: Vec<Tab> = query
            .iter(world)
            .filter(|(_, node)| node.display != Display::None)
            .map(|(panel, _)| panel.0)
            .collect();
        tabs.sort_by_key(|tab| format!("{tab:?}"));
        tabs
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

    /// Exact width under Bevy's embedded monospace FiraMono font.
    fn row_text_width(value: &str) -> f32 {
        value.chars().count() as f32 * DEFAULT_FONT_ADVANCE_EM * ROW_FONT_SIZE
    }

    #[test]
    fn every_knob_has_a_control_at_each_end_and_a_reading_between_them() {
        let mut app = screen_app();
        for knob in KNOBS {
            let before = app.world().resource::<Settings>().clone();
            press(&mut app, SettingsAction::Nudge(knob, 1));
            let after = app.world().resource::<Settings>().clone();
            assert_ne!(before, after, "{knob:?} did not move");
            let expected =
                after.reading_with_monitors(knob, app.world().resource::<MonitorChoices>());
            assert_eq!(reading_of(&mut app, Reading::Knob(knob)), expected);

            press(&mut app, SettingsAction::Nudge(knob, -1));
            assert_eq!(
                *app.world().resource::<Settings>(),
                before,
                "{knob:?} did not come back"
            );
        }
    }

    #[test]
    fn the_window_rows_offer_the_modes_and_the_attached_monitors_by_name() {
        let mut app = screen_app();
        assert_eq!(
            reading_of(&mut app, Reading::Knob(Knob::WindowMode)),
            "borderless"
        );
        assert_eq!(
            reading_of(&mut app, Reading::Knob(Knob::Monitor)),
            "primary - Main display (1920x1080 at 0,0)"
        );

        press(&mut app, SettingsAction::Nudge(Knob::WindowMode, 1));
        press(&mut app, SettingsAction::Nudge(Knob::Monitor, 1));
        assert_eq!(
            reading_of(&mut app, Reading::Knob(Knob::WindowMode)),
            "windowed"
        );
        assert_eq!(
            reading_of(&mut app, Reading::Knob(Knob::Monitor)),
            "Side display (1920x1080 at 1920,0)"
        );

        press(&mut app, SettingsAction::Reset(Tab::Graphics));
        assert_eq!(
            reading_of(&mut app, Reading::Knob(Knob::WindowMode)),
            "borderless"
        );
        assert_eq!(
            reading_of(&mut app, Reading::Knob(Knob::Monitor)),
            "primary - Main display (1920x1080 at 0,0)"
        );
    }

    #[test]
    fn a_capture_takes_the_next_free_key() {
        let mut app = screen_app();
        press(&mut app, SettingsAction::Capture(Control::Forward));
        assert_eq!(
            reading_of(&mut app, Reading::Binding(Control::Forward)),
            "..."
        );

        press_key(&mut app, KeyCode::F6);
        assert_eq!(
            app.world()
                .resource::<Settings>()
                .bindings()
                .key(Control::Forward),
            KeyCode::F6
        );
        assert_eq!(
            reading_of(&mut app, Reading::Binding(Control::Forward)),
            "f6"
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

        // Moving the menu off `Escape` is what frees it. `G` and not `M`, which
        // `Control::Map` has held since #452 — a taken key is refused, which is the
        // other half of this test rather than the half it is trying to set up.
        release_keys(&mut app);
        press(&mut app, SettingsAction::Capture(Control::Menu));
        press_key(&mut app, KeyCode::KeyG);
        assert_eq!(
            app.world()
                .resource::<Settings>()
                .bindings()
                .key(Control::Menu),
            KeyCode::KeyG
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

    // -------------------------------------------------------------------------
    // The tabs
    // -------------------------------------------------------------------------

    #[test]
    fn the_screen_opens_on_the_controls_and_shows_one_tab_at_a_time() {
        let mut app = screen_app();
        assert_eq!(shown_tabs(&mut app), vec![Tab::Controls]);
        assert_eq!(*app.world().resource::<Tab>(), Tab::Controls);
    }

    #[test]
    fn pressing_a_tab_swaps_which_rows_occupy_the_area() {
        let mut app = screen_app();
        press_tab(&mut app, Tab::Graphics);
        assert_eq!(shown_tabs(&mut app), vec![Tab::Graphics]);
        press_tab(&mut app, Tab::Controls);
        assert_eq!(shown_tabs(&mut app), vec![Tab::Controls]);
    }

    /// **The acceptance criterion, and the geometry #251 records for the other screen.** The
    /// strip and the area under it are the same nodes whichever tab is up, so nothing a
    /// player is aiming at moves when they switch — only which rows have a `Display`.
    #[test]
    fn the_strip_stays_where_it_is_and_only_the_area_below_it_swaps() {
        let mut app = screen_app();
        let before = (
            marker_node::<TabStrip>(&mut app),
            marker_node::<TabContent>(&mut app),
        );
        press_tab(&mut app, Tab::Graphics);
        let after = (
            marker_node::<TabStrip>(&mut app),
            marker_node::<TabContent>(&mut app),
        );

        assert_eq!(
            before, after,
            "switching tabs moved the strip or resized the area under it"
        );
        // And the area's height is a stated constant rather than whatever this tab needed,
        // which is what makes the equality above hold for a tab that is not this pair.
        assert_eq!(before.1.height, Val::Px(CONTENT_HEIGHT));
        assert_eq!(shown_tabs(&mut app), vec![Tab::Graphics]);
    }

    /// The guard on the constant above: one row too many on either tab is a failing test
    /// here rather than a panel that quietly grows and takes the strip with it.
    #[test]
    fn no_tab_needs_more_rows_than_the_area_it_is_drawn_in() {
        for tab in Tab::ALL {
            let rows = rows_of(tab).len();
            assert!(
                rows <= CONTENT_ROWS,
                "{tab:?} draws {rows} rows into an area sized for {CONTENT_ROWS}"
            );
            assert!(rows > 0, "{tab:?} draws nothing at all");
        }
    }

    /// The horizontal contract: the whole panel fits at the supported narrow viewport,
    /// and every row gives its label and complete stepper separate fixed columns.
    #[test]
    fn the_wider_row_columns_fit_inside_an_800_pixel_viewport() {
        assert_eq!(ROW_LABEL_WIDTH + ROW_COLUMN_GAP + STEPPER_WIDTH, COLUMN);
        assert_eq!(
            STEPPER_WIDTH,
            2.0 * STEP_BUTTON + 2.0 * CONTROL_GAP + READING_WIDTH,
            "the control column does not budget both buttons and both gaps"
        );
        let mut app = screen_app();
        assert_eq!(marker_node::<TabContent>(&mut app).width, Val::Px(COLUMN));
        let world = app.world_mut();
        let expected_rows: usize = Tab::ALL.into_iter().map(|tab| rows_of(tab).len()).sum();

        let mut labels = world.query::<(&RowLabel, &Text, &Node, &TextLayout)>();
        assert_eq!(labels.iter(world).count(), expected_rows);
        for (_, text, node, layout) in labels.iter(world) {
            assert_eq!(node.width, Val::Px(ROW_LABEL_WIDTH));
            assert_eq!(node.flex_shrink, 0.0);
            assert_eq!(node.overflow, Overflow::clip());
            assert_eq!(layout.linebreak, LineBreak::NoWrap);
            assert!(
                row_text_width(&text.0) <= ROW_LABEL_WIDTH,
                "built-in row label {:?} outgrew its column",
                text.0
            );
        }

        let mut controls = world.query::<(&RowControls, &Node)>();
        assert_eq!(controls.iter(world).count(), expected_rows);
        for (_, node) in controls.iter(world) {
            assert_eq!(node.width, Val::Px(STEPPER_WIDTH));
            assert_eq!(node.flex_shrink, 0.0);
            assert_eq!(node.column_gap, Val::Px(CONTROL_GAP));
        }
    }

    /// Every value with a model-owned bound fits in the reading area at both ends of that
    /// bound. The monitor is deliberately excluded: hardware owns its name, so it follows
    /// the clipped overflow policy asserted separately below.
    #[test]
    fn every_bounded_graphics_reading_fits_complete_on_one_line() {
        let monitors = MonitorChoices::named(&["Main display", "Side display"]);
        let mut values = Vec::new();
        for knob in KNOBS
            .into_iter()
            .filter(|knob| knob.tab() == Tab::Graphics && *knob != Knob::Monitor)
        {
            for steps in [-10_000, 10_000] {
                let mut settings = Settings::default();
                settings.adjust_with_monitors(knob, steps, &monitors);
                values.push(settings.reading_with_monitors(knob, &monitors));
            }
        }
        values.extend(["on", "off"].into_iter().map(str::to_owned));
        let mut corner = Corner::TopLeft;
        for _ in 0..4 {
            values.push(corner.name().to_owned());
            corner = corner.next();
        }

        for value in values {
            assert!(
                row_text_width(&value) <= READING_WIDTH,
                "bounded graphics reading {value:?} outgrew its column"
            );
        }

        let mut app = screen_app();
        let world = app.world_mut();
        let mut readings = world.query::<(&Reading, &Node, &TextLayout)>();
        for (reading, node, layout) in readings.iter(world) {
            if *reading == Reading::Notice {
                continue;
            }
            assert_eq!(layout.linebreak, LineBreak::NoWrap, "{reading:?} may wrap");
            if matches!(reading, Reading::Knob(_)) {
                assert_eq!(node.width, Val::Px(READING_WIDTH));
                assert_eq!(node.overflow, Overflow::clip());
            }
        }
    }

    /// A normal full monitor description is visible intact. A hardware name has no bound,
    /// so an exceptional one keeps its full model value but is deterministically clipped
    /// by the fixed-width, single-line reading node instead of growing into nearby rows.
    #[test]
    fn monitor_readings_are_complete_normally_and_clip_unbounded_names_on_one_line() {
        let mut app = screen_app();
        let normal = "primary - Main display (1920x1080 at 0,0)";
        assert_eq!(reading_of(&mut app, Reading::Knob(Knob::Monitor)), normal);
        assert!(row_text_width(normal) <= READING_WIDTH);

        let long_name = "External-monitor-name-".repeat(32);
        *app.world_mut().resource_mut::<MonitorChoices>() =
            MonitorChoices::named(&[long_name.as_str()]);
        app.update();

        let world = app.world_mut();
        let mut readings = world.query::<(&Reading, &Text, &Node, &TextLayout)>();
        let (_, text, node, layout) = readings
            .iter(world)
            .find(|(reading, _, _, _)| **reading == Reading::Knob(Knob::Monitor))
            .expect("the monitor row has a reading");
        assert!(
            text.0.contains(&long_name),
            "the model value was abbreviated"
        );
        assert!(
            row_text_width(&text.0) > READING_WIDTH,
            "the overflow fixture unexpectedly fits"
        );
        assert_eq!(node.width, Val::Px(READING_WIDTH));
        assert_eq!(node.flex_shrink, 0.0);
        assert_eq!(node.overflow, Overflow::clip());
        assert_eq!(layout.linebreak, LineBreak::NoWrap);
        assert_eq!(layout.justify, Justify::Center);
    }

    /// The consume control has one row on the Controls tab, and the screen rebinds it and
    /// resets it exactly as it does any other.
    ///
    /// `rows_of` is [`CONTROLS`]-driven, so the row costs this module nothing — which is
    /// precisely why it is worth asserting end to end rather than assuming: the label, the
    /// reading, the capture and the tab-scoped reset are four separate mechanisms and the
    /// new control is the first to exercise all four without a line of its own anywhere.
    #[test]
    fn the_consume_control_has_one_row_that_captures_and_resets_like_any_other() {
        let rows = rows_of(Tab::Controls);
        let drawn: Vec<&Row> = rows
            .iter()
            .filter(|row| matches!(row, Row::Binding(Control::Consume)))
            .collect();
        assert_eq!(drawn.len(), 1, "{drawn:?}");
        assert_eq!(drawn[0].label(), "Consume item");
        assert!(
            !rows_of(Tab::Graphics)
                .iter()
                .any(|row| matches!(row, Row::Binding(Control::Consume))),
            "a control row landed on the graphics tab"
        );

        let mut app = screen_app();
        assert_eq!(
            reading_of(&mut app, Reading::Binding(Control::Consume)),
            "c"
        );

        press(&mut app, SettingsAction::Capture(Control::Consume));
        assert_eq!(
            reading_of(&mut app, Reading::Binding(Control::Consume)),
            "...",
            "the row did not arm a capture"
        );
        press_key(&mut app, KeyCode::KeyV);
        assert_eq!(
            app.world()
                .resource::<Settings>()
                .bindings()
                .key(Control::Consume),
            KeyCode::KeyV
        );
        assert_eq!(
            reading_of(&mut app, Reading::Binding(Control::Consume)),
            "v"
        );
        assert_eq!(reading_of(&mut app, Reading::Notice), "");

        release_keys(&mut app);
        press(&mut app, SettingsAction::Reset(Tab::Controls));
        assert_eq!(
            app.world()
                .resource::<Settings>()
                .bindings()
                .key(Control::Consume),
            KeyCode::KeyC,
            "Reset Controls left the consume binding where it was"
        );
        assert_eq!(
            reading_of(&mut app, Reading::Binding(Control::Consume)),
            "c"
        );
    }

    /// Every knob has a row on exactly one tab, and every rebindable control has one.
    #[test]
    fn every_setting_the_model_offers_has_a_row_somewhere() {
        let all: Vec<Row> = Tab::ALL.into_iter().flat_map(rows_of).collect();
        for knob in KNOBS {
            let drawn = all
                .iter()
                .filter(|row| matches!(row, Row::Knob(drawn) if *drawn == knob))
                .count();
            assert_eq!(drawn, 1, "{knob:?} has {drawn} rows");
        }
        for control in CONTROLS {
            let drawn = all
                .iter()
                .filter(|row| matches!(row, Row::Binding(drawn) if *drawn == control))
                .count();
            assert_eq!(drawn, 1, "{control:?} has {drawn} rows");
        }

        // **The toggles too, and they are the half this test used to miss.** `KNOBS` and
        // `CONTROLS` are lists the model owns, so sweeping them catches a row that was never
        // drawn; the three toggles are written into `rows_of` by hand and were therefore
        // covered by nothing — `the_graphics_flags_read_back_what_pressing_them_did` presses
        // the actions directly, so a row could have been deleted from the screen with every
        // test still green.
        for reading in [Reading::Vsync, Reading::Readout, Reading::ReadoutCorner] {
            let drawn = all
                .iter()
                .filter(|row| matches!(row, Row::Toggle(_, _, shown) if *shown == reading))
                .count();
            assert_eq!(drawn, 1, "{reading:?} has {drawn} rows");
        }
        // And the count, so the sweep above cannot be satisfied by a list that has grown a
        // fourth toggle nobody named here.
        let toggles = all
            .iter()
            .filter(|row| matches!(row, Row::Toggle(..)))
            .count();
        assert_eq!(
            toggles, 3,
            "the screen draws {toggles} toggles; name the new one above rather than widening \
             this number"
        );
    }

    /// Leaving the tab a capture was armed on takes the capture back.
    ///
    /// Found in review on PR #257. A capture is armed over one row; switch tabs and that row is
    /// no longer drawn, but the arm survived — so the next key press rebound a control the
    /// player could not see, while the notice under the panel went on asking for a key. The
    /// binding it overwrote was one nobody had chosen to change.
    #[test]
    fn switching_tabs_takes_back_a_capture_armed_on_the_one_being_left() {
        let mut app = screen_app();
        press(&mut app, SettingsAction::Capture(Control::Forward));
        assert_eq!(
            app.world().resource::<SettingsScreen>().capturing,
            Some(Control::Forward),
            "the capture did not arm, so this test would pass for the wrong reason"
        );

        press_tab(&mut app, Tab::Graphics);

        assert_eq!(
            app.world().resource::<SettingsScreen>().capturing,
            None,
            "a capture armed on CONTROLS survived the switch to GRAPHICS"
        );

        // And the key that would have gone to it goes nowhere: the binding it was armed over
        // is the one it started as. Asserting the state alone would not catch a capture that
        // was cleared and re-armed by the same press.
        let bindings = *app.world().resource::<Settings>().bindings();
        press_key(&mut app, KeyCode::KeyJ);
        assert_eq!(
            *app.world().resource::<Settings>().bindings(),
            bindings,
            "a key pressed after the switch still rebound something"
        );
    }

    /// Pressing the tab already showing is not leaving it, so a capture on it survives.
    ///
    /// The negative that keeps the fix above honest: clearing on every tab press would pass
    /// that test and would also cancel a capture a player armed and then clicked beside.
    #[test]
    fn pressing_the_tab_already_showing_leaves_a_capture_armed() {
        let mut app = screen_app();
        press(&mut app, SettingsAction::Capture(Control::Forward));

        press_tab(&mut app, Tab::Controls);

        assert_eq!(
            app.world().resource::<SettingsScreen>().capturing,
            Some(Control::Forward),
            "pressing the tab that was already showing cancelled the capture"
        );
    }

    /// Each tab's own panel carries its own reset button, and only its own.
    ///
    /// `press` finds a button anywhere in the world, so every `a_reset_*` test above passes on
    /// a screen where both reset buttons sit on one tab, or where one of them is drawn outside
    /// any panel at all. What those tests pin is that the *action* is scoped; this pins that
    /// the button a player can actually reach is the one for the tab they are looking at.
    #[test]
    fn each_tab_draws_its_own_reset_button_and_no_other() {
        let mut app = screen_app();
        let world = app.world_mut();

        let resets: Vec<(Entity, Tab)> = world
            .query::<(Entity, &SettingsAction)>()
            .iter(world)
            .filter_map(|(entity, action)| match action {
                SettingsAction::Reset(tab) => Some((entity, *tab)),
                _ => None,
            })
            .collect();
        assert_eq!(
            resets.len(),
            Tab::ALL.len(),
            "the screen draws {} reset buttons for {} tabs",
            resets.len(),
            Tab::ALL.len()
        );

        for (button, tab) in resets {
            // Up the tree to the panel the button is actually inside. A reset drawn outside
            // every panel reaches the end with nothing found, which is the failure this walk
            // exists to name.
            let mut at = button;
            let panel = loop {
                if let Some(panel) = world.get::<TabPanel>(at) {
                    break Some(panel.0);
                }
                match world.get::<ChildOf>(at) {
                    Some(parent) => at = parent.0,
                    None => break None,
                }
            };
            assert_eq!(
                panel,
                Some(tab),
                "the reset for {tab:?} is drawn inside {panel:?} rather than inside its own tab"
            );
        }
    }

    /// The two flags and the corner are reachable, and each says what it is.
    #[test]
    fn the_graphics_flags_read_back_what_pressing_them_did() {
        let mut app = screen_app();
        assert_eq!(reading_of(&mut app, Reading::Vsync), "on");
        press(&mut app, SettingsAction::ToggleVsync);
        assert_eq!(reading_of(&mut app, Reading::Vsync), "off");

        assert_eq!(reading_of(&mut app, Reading::Readout), "off");
        press(&mut app, SettingsAction::ToggleReadout);
        assert_eq!(reading_of(&mut app, Reading::Readout), "on");

        let first = reading_of(&mut app, Reading::ReadoutCorner);
        press(&mut app, SettingsAction::CycleCorner);
        let second = reading_of(&mut app, Reading::ReadoutCorner);
        assert_ne!(first, second, "the corner did not move");
        assert_eq!(
            second,
            app.world()
                .resource::<Settings>()
                .readout_corner()
                .name()
                .to_owned()
        );
    }

    // -------------------------------------------------------------------------
    // The per-tab reset
    // -------------------------------------------------------------------------

    /// Moves something on both tabs, so a reset that took the whole struct back would be
    /// visible from either side.
    fn move_both_tabs(app: &mut App) {
        press(app, SettingsAction::Nudge(Knob::LookSensitivity, 1));
        press(app, SettingsAction::Capture(Control::Forward));
        press_key(app, KeyCode::F6);
        release_keys(app);
        press(app, SettingsAction::Nudge(Knob::RenderDistance, -1));
        press(app, SettingsAction::ToggleVsync);
        press(app, SettingsAction::ToggleReadout);
    }

    /// **The bug this button is most likely to have**, pressed through the screen: resetting
    /// graphics must leave every binding and the sensitivity exactly where the player put
    /// them.
    #[test]
    fn resetting_graphics_leaves_the_controls_tab_alone() {
        let mut app = screen_app();
        move_both_tabs(&mut app);
        let before = app.world().resource::<Settings>().clone();

        press(&mut app, SettingsAction::Reset(Tab::Graphics));
        let after = app.world().resource::<Settings>().clone();

        assert_eq!(
            after.render_distance(),
            Settings::default().render_distance()
        );
        assert!(after.vsync(), "vsync did not come back");
        assert!(!after.readout_shown(), "the readout did not come back");
        assert_eq!(
            after.bindings(),
            before.bindings(),
            "resetting graphics cleared a key binding"
        );
        assert_eq!(
            reading_of(&mut app, Reading::Binding(Control::Forward)),
            "f6",
            "the screen shows a binding the reset should not have touched"
        );
        assert_eq!(
            after.reading(Knob::LookSensitivity),
            before.reading(Knob::LookSensitivity),
            "resetting graphics moved the mouse sensitivity"
        );
    }

    /// And the mirror, which is the half a happy-path test would have missed.
    #[test]
    fn resetting_controls_leaves_the_graphics_tab_alone() {
        let mut app = screen_app();
        move_both_tabs(&mut app);
        let before = app.world().resource::<Settings>().clone();

        press(&mut app, SettingsAction::Reset(Tab::Controls));
        let after = app.world().resource::<Settings>().clone();

        assert_eq!(
            after.bindings().key(Control::Forward),
            KeyCode::KeyW,
            "the binding did not come back"
        );
        assert_eq!(
            after.reading(Knob::LookSensitivity),
            Settings::default().reading(Knob::LookSensitivity)
        );
        assert_eq!(
            after.render_distance(),
            before.render_distance(),
            "resetting controls moved the render distance"
        );
        assert_eq!(after.vsync(), before.vsync());
        assert_eq!(after.readout_shown(), before.readout_shown());
        assert_eq!(
            reading_of(&mut app, Reading::Binding(Control::Forward)),
            "w",
            "the screen still shows the binding that was reset away"
        );
    }

    /// A reset takes back a capture it has just invalidated, so the next key press is not
    /// silently answering a question about a binding that no longer exists.
    #[test]
    fn a_reset_takes_back_a_waiting_capture() {
        let mut app = screen_app();
        press(&mut app, SettingsAction::Capture(Control::Jump));
        assert_eq!(reading_of(&mut app, Reading::Binding(Control::Jump)), "...");

        press(&mut app, SettingsAction::Reset(Tab::Controls));
        assert_eq!(
            reading_of(&mut app, Reading::Binding(Control::Jump)),
            "space"
        );
        assert_eq!(reading_of(&mut app, Reading::Notice), "");

        // Nothing is waiting, so the next key is the way out rather than a binding.
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
    fn leaving_the_pause_menu_takes_the_screen_with_it() {
        let mut app = screen_app();
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        app.update();
        assert!(!app.world().resource::<SettingsScreen>().is_open());
    }
}
