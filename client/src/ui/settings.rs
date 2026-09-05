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

use super::{BUTTON, CELL_EDGE, TAB_SELECTED, button_colour};
use crate::audio::{AudioControls, Voices};
use crate::player::{Appearances, InputMode};
use crate::settings::{
    AudioDevices, CONTROLS, Choices, Control, KNOBS, Knob, MonitorChoices, Settings, Tab, key_name,
};

/// Whether the screen is up, and what it is waiting for.
#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct SettingsScreen {
    /// Whether the panel is drawn. Set by the pause menu's Settings entry.
    open: bool,
    /// The control whose next key press is its new binding, while one is waiting.
    capturing: Option<Control>,
    /// The line under the panel: what was refused, or what is being waited for.
    notice: String,
    /// Whether the Monitor row's dropdown is open. [`Self::open`] and [`Self::close`] both
    /// start it shut; `switch_settings_tabs` closes it on a tab change, `settings_actions`
    /// on a graphics reset, and `read_settings_keys` gives Escape to it before Escape can
    /// close the screen.
    monitor_dropdown_open: bool,
    /// Whether the Voices panel is open. Its own lifecycle, exactly as the Monitor dropdown
    /// has one: opened from its row, closed by that row, by a tab change, and by Escape.
    voices_open: bool,
}

impl SettingsScreen {
    /// Puts the screen up. The pause menu's Settings entry is the one caller.
    pub(super) fn open(&mut self) {
        self.open = true;
        self.capturing = None;
        self.notice.clear();
        self.monitor_dropdown_open = false;
        self.voices_open = false;
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
        self.monitor_dropdown_open = false;
        self.voices_open = false;
    }
}

/// Draws the settings screen and keeps it in step with [`Settings`].
pub(super) struct SettingsScreenPlugin;

impl Plugin for SettingsScreenPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SettingsScreen>()
            .init_resource::<Settings>()
            .init_resource::<MonitorChoices>()
            .init_resource::<AudioDevices>()
            // Inserted by `AudioPlugin`, which `main.rs` adds first; this is what lets the
            // screen and its tests stand up with no audio device anywhere near them.
            .init_resource::<AudioControls>()
            .init_resource::<Voices>()
            .init_resource::<Appearances>()
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
                    monitor_select_toggle,
                    monitor_dropdown_actions,
                    // After the input mode, so the frame that closes this screen is a
                    // frame `choose_input_mode` has already declined to read.
                    read_settings_keys.after(crate::player::ApplyInputMode),
                    rebuild_monitor_options,
                    show_monitor_dropdown,
                    colour_monitor_controls,
                    voice_row_actions,
                    rebuild_voice_rows,
                    show_voices_panel,
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

/// The Monitor row's closed control. Pressing it opens or closes [`MonitorDropdownPanel`].
#[derive(Component)]
struct MonitorSelectButton;

/// The Monitor row's dropdown: an absolutely positioned overlay anchored under
/// [`MonitorSelectButton`], with its own stacking position ([`MONITOR_DROPDOWN_LAYER`]) and
/// its own open/close lifecycle rather than the panel's.
#[derive(Component)]
struct MonitorDropdownPanel;

/// The Voices panel: the overlay a speaker's rows are spawned into.
#[derive(Component)]
struct VoicesPanel;

/// One speaker's row inside it, naming whose it is so it can be despawned by entity id.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct VoiceRow(u64);

/// The line counting the speakers the panel did not draw, and how many that is.
///
/// **A component rather than a sentinel `VoiceRow`**, and the difference is a real bug rather
/// than tidiness: with the count folded into the row list, "what is drawn" and "who is drawn"
/// were the same question, so the first crowd compared equal to the eight rows already there
/// and the count was never spawned at all. It is a second thing the rebuild has to notice.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct VoiceOverflow(usize);

/// What pressing a control inside the Voices panel means.
///
/// Not a [`SettingsAction`]: these carry a speaker's entity id, and they change a resource
/// `crate::audio` owns rather than a setting. Keeping them apart is also what keeps
/// `settings_actions` from having to know that `Voices` exists.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
enum VoiceControl {
    /// Mute this speaker, or un-mute them.
    Mute(u64),
    /// Move this speaker's volume by the given number of its own steps.
    Nudge(u64, i32),
}

/// One option inside the open dropdown, naming its index into
/// [`MonitorChoices::preferences`]. Rebuilt whenever the live choices change, so an index
/// here always names [`rebuild_monitor_options`]'s current row.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct MonitorOption(usize);

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
    /// Play a second of tone through the master bus at whatever the volume is now.
    TestSpeakers,
    /// Show or hide the Voices panel.
    ToggleVoices,
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
    /// The Monitor row's closed control: the current monitor plus the open indicator, as
    /// one centred string. Distinct from `Knob(Knob::Monitor)`, which nothing spawns a
    /// text node for any more — Monitor draws as [`MonitorSelectButton`], not a stepper.
    MonitorControl,
    /// Whether the Voices panel is showing, as the word on its own button.
    VoicesPanel,
    /// One speaker's volume, as a percentage.
    VoiceLevel(u64),
    /// Whether one speaker is muted, as the word on the button that changes it.
    VoiceMuted(u64),
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

/// The height of an ordinary (non-full-width) control, in logical pixels — a stepper's `-`
/// or `+`, a toggle, a binding capture, and the Monitor select's own closed control and
/// dropdown options.
const CONTROL_BUTTON_HEIGHT: f32 = ROW_HEIGHT - 4.0;

/// The height of a full-width control — the reset at the foot of a tab, and `BACK`.
const WIDE_BUTTON: f32 = 40.0;

/// The Voices panel's stacking position, and the Monitor dropdown's reasoning verbatim: a
/// child of the row it belongs to would paint behind every row spawned after it.
const VOICES_PANEL_LAYER: i32 = 46;

/// The most speakers the Voices panel draws at once.
///
/// **A bound on a length the world chooses.** `audio/listener.rs` already caps what it
/// remembers; this caps what is *drawn*, because a panel one row per speaker tall would run
/// off the bottom of the narrowest supported viewport long before that cap. The rest are
/// counted, exactly as `ui/voice.rs` counts the speakers it cannot name.
const PANEL_VOICES: usize = 8;

/// The width of a speaker's name in the Voices panel, in logical pixels.
const VOICE_NAME_WIDTH: f32 = 200.0;

/// The width of a speaker's mute control, in logical pixels.
const VOICE_MUTE_WIDTH: f32 = 90.0;

/// And of the reading between its `-` and `+`.
const VOICE_LEVEL_WIDTH: f32 = 70.0;

/// The Monitor dropdown's stacking position. Without a `GlobalZIndex` of its own it would
/// paint inside the Monitor row's slot in the panel's tree order — behind every row spawned
/// after it, exactly where it must appear above all of them. One more than `SettingsRoot`'s
/// own 45 (see [`spawn_settings_screen`]) is enough, the same margin `ui/mod.rs`'s
/// `GlobalZIndex(31)` keeps over the HUD overlays it sits above.
const MONITOR_DROPDOWN_LAYER: i32 = 46;

/// The most rows any one tab may draw.
///
/// **The number the layout is sized from, and the reason the strip does not move.** Controls
/// is the taller tab — one knob and every entry in `CONTROLS` — and a content area sized to
/// "whatever this tab needs" would be stable purely by that coincidence, until another row
/// arrived on one of them, which is how `ui/inventory.rs` ended up with the geometry #251
/// describes. `no_tab_needs_more_rows_than_the_area_it_is_drawn_in` fails rather than the
/// panel jumping, and #399 is what made it fail: adding `Control::Consume` grew Controls to
/// eleven rows, so this number moved with it rather than the area silently overflowing.
/// #452 moved it again, to twelve, for `Control::Map`; #711 moves it to thirteen for the
/// rebindable default-mount call; #852 moves it to fourteen for `Control::Talk`.
const CONTENT_ROWS: usize = 14;

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
        Tab::Audio => "RESET AUDIO",
    }
}

/// The panel's own background, reused by the Monitor dropdown so it reads as part of the
/// same surface rather than a different piece of UI overlaid on top of it.
const PANEL_BACKGROUND: Color = Color::srgb(0.065, 0.075, 0.095);

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
                    BackgroundColor(PANEL_BACKGROUND),
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
    /// The Monitor row: a select rather than a stepper, drawn by [`spawn_monitor_select`].
    /// Not `Knob(Knob::Monitor)` — [`rows_of`] gives Monitor this variant instead, which is
    /// what keeps the generic stepper out of its row while every other knob still gets one.
    MonitorSelect,
    /// The Voices row: one button that opens the panel, plus the panel it opens. Its own
    /// variant rather than a [`Self::Toggle`] because it spawns an overlay beside the button.
    VoicesToggle,
    /// A row whose control *does* something rather than showing something: one button with
    /// a face that never changes. [`Self::Toggle`] is the shape for a value being cycled;
    /// this is the shape for a press with no state behind it at all.
    Action(&'static str, SettingsAction, &'static str),
}

impl Row {
    /// What the row is called on its left.
    const fn label(self) -> &'static str {
        match self {
            Self::Knob(knob) => knob.label(),
            Self::Toggle(label, _, _) => label,
            Self::Binding(control) => control.label(),
            Self::MonitorSelect => Knob::Monitor.label(),
            Self::VoicesToggle => "Voices",
            Self::Action(label, _, _) => label,
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
    // Every other knob gets the generic stepper; Monitor gets its own select in the exact
    // slot `KNOBS`' order already puts it in, rather than a filter-then-append that would
    // move it to the end of the tab.
    let mut rows: Vec<Row> = KNOBS
        .into_iter()
        .filter(|knob| knob.tab() == tab)
        .map(|knob| {
            if knob == Knob::Monitor {
                Row::MonitorSelect
            } else {
                Row::Knob(knob)
            }
        })
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
        Tab::Audio => rows.extend([
            // Under the knob it proves, because that is the order a player uses them in: set
            // the volume, then find out whether anything comes out.
            Row::Action("Test speakers", SettingsAction::TestSpeakers, "PLAY A TONE"),
            // Last, and a `Toggle` rather than an `Action`: the button's face *is* the state,
            // so a player can tell an open panel from a closed one without looking at it.
            Row::VoicesToggle,
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
            Row::MonitorSelect => spawn_monitor_select(controls),
            Row::VoicesToggle => spawn_voices_control(controls),
            Row::Action(_, action, face) => {
                spawn_button(
                    controls,
                    action,
                    Val::Px(STEP_BUTTON * 4.0),
                    Face::Fixed(face),
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

/// The Monitor row: one control occupying the whole stepper column, rather than the three
/// a numeric knob draws. Pressing it is [`monitor_select_toggle`]'s job; the value is
/// [`describe`]'s, through [`Reading::MonitorControl`]; the option list is built and torn
/// down by [`rebuild_monitor_options`] — this only spawns the empty [`MonitorDropdownPanel`]
/// those options are added to.
fn spawn_monitor_select(parent: &mut ChildSpawnerCommands<'_>) {
    parent
        .spawn((
            MonitorSelectButton,
            Button,
            Node {
                width: Val::Px(STEPPER_WIDTH),
                height: Val::Px(CONTROL_BUTTON_HEIGHT),
                flex_shrink: 0.0,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                border_radius: BorderRadius::all(Val::Px(3.0)),
                ..default()
            },
            BackgroundColor(BUTTON),
        ))
        .with_children(|button| {
            // Value and indicator are one string, so "centred" is one property
            // (`Justify::Center` here, `AlignItems::Center` on the button) rather than two
            // children whose combined width would need centring separately.
            button.spawn((
                Reading::MonitorControl,
                Text::new(String::new()),
                TextFont {
                    font_size: ROW_FONT,
                    ..default()
                },
                TextColor(Color::WHITE),
                TextLayout::no_wrap().with_justify(Justify::Center),
                Node {
                    width: Val::Px(STEPPER_WIDTH),
                    flex_shrink: 0.0,
                    overflow: Overflow::clip(),
                    ..default()
                },
            ));

            // Anchored directly below the control, at its exact width. `GlobalZIndex` —
            // not a plain `ZIndex` — is what lets it paint over every row beneath it rather
            // than stacking inside this row's own slot in the panel's tree order; see
            // [`MONITOR_DROPDOWN_LAYER`]. Closed by default; [`show_monitor_dropdown`] is
            // the only writer of its `Display`.
            button.spawn((
                MonitorDropdownPanel,
                GlobalZIndex(MONITOR_DROPDOWN_LAYER),
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(CONTROL_BUTTON_HEIGHT),
                    left: Val::Px(0.0),
                    width: Val::Px(STEPPER_WIDTH),
                    display: Display::None,
                    flex_direction: FlexDirection::Column,
                    border: UiRect::all(Val::Px(1.0)),
                    ..default()
                },
                BackgroundColor(PANEL_BACKGROUND),
                BorderColor::all(CELL_EDGE),
            ));
        });
}

/// The Voices row's control and the panel it opens.
///
/// [`spawn_monitor_select`]'s shape, for its reasons: an absolutely positioned overlay
/// anchored under the button, with a `GlobalZIndex` of its own so it paints over the rows
/// below rather than inside this row's slot in the panel's tree order. It starts empty —
/// [`rebuild_voice_rows`] fills it, and only when the set of speakers changes.
fn spawn_voices_control(parent: &mut ChildSpawnerCommands<'_>) {
    parent
        .spawn((
            SettingsAction::ToggleVoices,
            Button,
            Node {
                width: Val::Px(STEP_BUTTON * 4.0),
                height: Val::Px(CONTROL_BUTTON_HEIGHT),
                flex_shrink: 0.0,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                border_radius: BorderRadius::all(Val::Px(3.0)),
                ..default()
            },
            BackgroundColor(BUTTON),
        ))
        .with_children(|button| {
            button.spawn((
                Reading::VoicesPanel,
                Text::new(String::new()),
                TextFont {
                    font_size: ROW_FONT,
                    ..default()
                },
                TextColor(Color::WHITE),
                TextLayout::no_wrap().with_justify(Justify::Center),
            ));

            button.spawn((
                VoicesPanel,
                GlobalZIndex(VOICES_PANEL_LAYER),
                Node {
                    position_type: PositionType::Absolute,
                    // Above its own button rather than below it: this row is the last on the
                    // tab, so a panel hanging downwards would leave the screen.
                    bottom: Val::Px(CONTROL_BUTTON_HEIGHT),
                    right: Val::Px(0.0),
                    width: Val::Px(
                        VOICE_NAME_WIDTH
                            + VOICE_MUTE_WIDTH
                            + VOICE_LEVEL_WIDTH
                            + 2.0 * STEP_BUTTON
                            + 5.0 * CONTROL_GAP,
                    ),
                    display: Display::None,
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(2.0),
                    padding: UiRect::all(Val::Px(6.0)),
                    border: UiRect::all(Val::Px(1.0)),
                    ..default()
                },
                BackgroundColor(PANEL_BACKGROUND),
                BorderColor::all(CELL_EDGE),
            ));
        });
}

/// One speaker's row inside the open Voices panel.
fn spawn_voice_row(parent: &mut ChildSpawnerCommands<'_>, entity_id: u64, name: String) {
    parent
        .spawn((
            VoiceRow(entity_id),
            Node {
                display: Display::Flex,
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(CONTROL_GAP),
                height: Val::Px(ROW_HEIGHT),
                ..default()
            },
        ))
        .with_children(|row| {
            row.spawn((
                Text::new(name),
                TextFont {
                    font_size: ROW_FONT,
                    ..default()
                },
                TextColor(Color::srgb(0.78, 0.80, 0.84)),
                TextLayout::no_wrap(),
                Node {
                    width: Val::Px(VOICE_NAME_WIDTH),
                    flex_shrink: 0.0,
                    overflow: Overflow::clip(),
                    ..default()
                },
            ));
            spawn_voice_button(
                row,
                VoiceControl::Mute(entity_id),
                VOICE_MUTE_WIDTH,
                Reading::VoiceMuted(entity_id),
            );
            spawn_voice_button(row, VoiceControl::Nudge(entity_id, -1), STEP_BUTTON, None);
            row.spawn((
                Reading::VoiceLevel(entity_id),
                Text::new(String::new()),
                TextFont {
                    font_size: ROW_FONT,
                    ..default()
                },
                TextColor(Color::WHITE),
                TextLayout::no_wrap().with_justify(Justify::Center),
                Node {
                    width: Val::Px(VOICE_LEVEL_WIDTH),
                    flex_shrink: 0.0,
                    overflow: Overflow::clip(),
                    ..default()
                },
            ));
            spawn_voice_button(row, VoiceControl::Nudge(entity_id, 1), STEP_BUTTON, None);
        });
}

/// One pressable control inside a speaker's row.
///
/// `face` is a [`Reading`] for a button whose text *is* the state it changes, and `None` for
/// one whose face never moves — the `-` and `+`, which are drawn from the control itself.
fn spawn_voice_button(
    parent: &mut ChildSpawnerCommands<'_>,
    control: VoiceControl,
    width: f32,
    face: impl Into<Option<Reading>>,
) {
    let face = face.into();
    let fixed = match control {
        VoiceControl::Mute(_) => String::new(),
        VoiceControl::Nudge(_, steps) if steps < 0 => "-".to_owned(),
        VoiceControl::Nudge(..) => "+".to_owned(),
    };
    let mut button = parent.spawn((
        control,
        Button,
        Node {
            width: Val::Px(width),
            height: Val::Px(CONTROL_BUTTON_HEIGHT),
            flex_shrink: 0.0,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            border_radius: BorderRadius::all(Val::Px(3.0)),
            ..default()
        },
        BackgroundColor(BUTTON),
    ));
    button.with_children(|button| {
        let mut text = button.spawn((
            Text::new(fixed),
            TextFont {
                font_size: ROW_FONT,
                ..default()
            },
            TextColor(Color::WHITE),
            TextLayout::no_wrap().with_justify(Justify::Center),
        ));
        if let Some(face) = face {
            text.insert(face);
        }
    });
}

/// One option inside the open Monitor dropdown, at `index` in
/// [`MonitorChoices::preferences`]. Pressing it is [`monitor_dropdown_actions`]'s job.
fn spawn_monitor_option(parent: &mut ChildSpawnerCommands<'_>, index: usize, label: String) {
    parent
        .spawn((
            MonitorOption(index),
            Button,
            Node {
                width: Val::Percent(100.0),
                height: Val::Px(CONTROL_BUTTON_HEIGHT),
                flex_shrink: 0.0,
                align_items: AlignItems::Center,
                // Left-aligned for scanning, unlike the closed control's centred value.
                justify_content: JustifyContent::FlexStart,
                padding: UiRect::horizontal(Val::Px(8.0)),
                ..default()
            },
            BackgroundColor(BUTTON),
        ))
        .with_child((
            Text::new(label),
            TextFont {
                font_size: ROW_FONT,
                ..default()
            },
            TextColor(Color::WHITE),
            TextLayout::no_wrap(),
            Node {
                flex_grow: 1.0,
                min_width: Val::Px(0.0),
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
        (CONTROL_BUTTON_HEIGHT, ROW_FONT)
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
            // The Monitor dropdown is the same trap one row over: leaving Graphics has to
            // close it explicitly, or it goes on floating over whichever rows Controls
            // draws in its place. The Voices panel is the third of them, on the third tab.
            if screen.monitor_dropdown_open {
                screen.monitor_dropdown_open = false;
            }
            if screen.voices_open {
                screen.voices_open = false;
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
    devices: Res<AudioDevices>,
    mut audio: ResMut<AudioControls>,
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
                settings.adjust_with_choices(
                    knob,
                    steps,
                    Choices {
                        monitors: &monitors,
                        devices: &devices,
                    },
                );
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
                // A graphics reset puts the Monitor preference back too, so the dropdown
                // closes rather than floating over the value it just replaced.
                screen.monitor_dropdown_open = false;
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
            SettingsAction::ToggleVoices => {
                screen.voices_open = !screen.voices_open;
                continue;
            }
            SettingsAction::TestSpeakers => {
                // The whole of the row: a request, taken back by `audio/mod.rs` on the
                // frame it starts the tone. This screen owns no sample, no bus and no
                // device, and it sets no volume either — the tone plays at the gain
                // `follow_the_settings` has already applied from the row above.
                audio.speaker_test = true;
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
            // An overlay answers to Escape before the screen does: the first press closes
            // what was most recently opened, not the whole screen behind it. The two are
            // never open together — they are on different tabs, and a tab change closes
            // both — so the order between them decides nothing.
            if screen.monitor_dropdown_open {
                screen.monitor_dropdown_open = false;
            } else if screen.voices_open {
                screen.voices_open = false;
            } else {
                screen.close();
            }
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

/// Opens or closes the Monitor dropdown from its own control.
///
/// Not a [`SettingsAction`]: that enum's buttons are all painted by `settings_actions`'s
/// unconditional `button_colour(interaction)`, which has no notion of a *selected* colour —
/// the same reason tabs are not `SettingsAction` either. Folding this in would mean
/// [`colour_monitor_controls`] and `settings_actions` both writing this entity's
/// `BackgroundColor` in the same frame, in an order nothing pins.
fn monitor_select_toggle(
    mut buttons: Query<&Interaction, (With<MonitorSelectButton>, Changed<Interaction>)>,
    mut screen: ResMut<SettingsScreen>,
) {
    for interaction in &mut buttons {
        if *interaction == Interaction::Pressed {
            screen.monitor_dropdown_open = !screen.monitor_dropdown_open;
        }
    }
}

/// Applies a press inside the open Monitor dropdown, and closes it either way.
///
/// `monitors.preferences()` is read fresh rather than cached from spawn time: an index that
/// no longer names a live preference — the operating system dropped a display between the
/// click and this system running — is answered by doing nothing, the same refusal a missing
/// chunk or an unheld item answers elsewhere in this client.
fn monitor_dropdown_actions(
    mut options: Query<(&MonitorOption, &Interaction), Changed<Interaction>>,
    monitors: Res<MonitorChoices>,
    mut settings: ResMut<Settings>,
    mut screen: ResMut<SettingsScreen>,
) {
    for (option, interaction) in &mut options {
        if *interaction != Interaction::Pressed {
            continue;
        }
        if let Some(preference) = monitors.preferences().get(option.0).cloned() {
            settings.set_monitor(preference);
        }
        screen.monitor_dropdown_open = false;
    }
}

/// Rebuilds the dropdown's options whenever the live monitors change, and only then —
/// `ui/servers.rs`'s `rebuild_rows` gives the same reason one screen over: rebuilding every
/// frame would despawn and respawn the entity under a pointer mid-press. The panel starts
/// with no children — [`spawn_monitor_select`] builds it empty — so the first change
/// `MonitorChoices` reports, real or a test's initial insert, is what populates it.
fn rebuild_monitor_options(
    monitors: Res<MonitorChoices>,
    panels: Query<Entity, With<MonitorDropdownPanel>>,
    options: Query<Entity, With<MonitorOption>>,
    mut commands: Commands,
) {
    if !monitors.is_changed() {
        return;
    }
    for option in &options {
        commands.entity(option).despawn();
    }
    for panel in &panels {
        commands.entity(panel).with_children(|list| {
            for (index, preference) in monitors.preferences().into_iter().enumerate() {
                spawn_monitor_option(list, index, monitors.option_label(&preference));
            }
        });
    }
}

/// Gives the dropdown panel a `Display` and takes it away — never a `Visibility`, the same
/// reason [`show_the_active_settings_tab`] gives `TabPanel` one: a hidden node still
/// occupies its layout box. `Display::None` also keeps a closed dropdown out of
/// hit-testing, so a row it used to cover is clickable again the moment it closes.
fn show_monitor_dropdown(
    screen: Res<SettingsScreen>,
    mut panels: Query<&mut Node, With<MonitorDropdownPanel>>,
) {
    let next = if screen.monitor_dropdown_open {
        Display::Flex
    } else {
        Display::None
    };
    for mut node in &mut panels {
        if node.display != next {
            node.display = next;
        }
    }
}

/// Paints the closed control and every open option — the pointer's three states for both,
/// plus the one extra state [`button_colour`] has no arm for: the applied preference,
/// coloured exactly as the active tab is.
fn colour_monitor_controls(
    settings: Res<Settings>,
    monitors: Res<MonitorChoices>,
    mut button: Query<(&Interaction, &mut BackgroundColor), With<MonitorSelectButton>>,
    mut options: Query<
        (&MonitorOption, &Interaction, &mut BackgroundColor),
        Without<MonitorSelectButton>,
    >,
) {
    for (interaction, mut colour) in &mut button {
        let next = button_colour(interaction);
        if colour.0 != next {
            colour.0 = next;
        }
    }

    let selected = monitors
        .preferences()
        .iter()
        .position(|preference| preference == settings.monitor());
    for (option, interaction, mut colour) in &mut options {
        let next = if Some(option.0) == selected {
            TAB_SELECTED
        } else {
            button_colour(interaction)
        };
        if colour.0 != next {
            colour.0 = next;
        }
    }
}

/// Applies a press inside the open Voices panel.
///
/// **It changes `Voices` and never a setting**, which is the whole of why these controls are
/// not [`SettingsAction`]s: a mute is session state `crate::audio` owns, and #853 is explicit
/// that it is never written to the settings file — the snapshot carries no stable player id,
/// so a saved mute would come back attached to whoever inherited that entity number.
fn voice_row_actions(
    mut buttons: Query<(&Interaction, &VoiceControl, &mut BackgroundColor), Changed<Interaction>>,
    mut voices: ResMut<Voices>,
) {
    for (interaction, control, mut colour) in &mut buttons {
        colour.0 = button_colour(interaction);
        if *interaction != Interaction::Pressed {
            continue;
        }
        match *control {
            VoiceControl::Mute(entity_id) => voices.toggle_mute(entity_id),
            VoiceControl::Nudge(entity_id, steps) => voices.adjust_volume(entity_id, steps),
        }
    }
}

/// Rebuilds the panel's rows when the set of speakers changes, and **only** then.
///
/// `ui/servers.rs`'s `rebuild_rows` and [`rebuild_monitor_options`] give the same reason:
/// rebuilding every frame would despawn and respawn the entity under a pointer mid-press. The
/// trap here is sharper than theirs, because `Voices` is marked changed on **every frame
/// anybody speaks** — so the comparison is against the set of speakers actually drawn, not
/// against `Res::is_changed`.
fn rebuild_voice_rows(
    voices: Res<Voices>,
    appearances: Res<Appearances>,
    panels: Query<Entity, With<VoicesPanel>>,
    rows: Query<(Entity, &VoiceRow)>,
    overflow: Query<(Entity, &VoiceOverflow)>,
    mut commands: Commands,
) {
    let heard = voices.recent(std::time::Instant::now());
    let wanted: Vec<u64> = heard.iter().copied().take(PANEL_VOICES).collect();
    let hidden = heard.len() - wanted.len();

    let drawn: Vec<u64> = {
        let mut drawn: Vec<u64> = rows.iter().map(|(_, row)| row.0).collect();
        drawn.sort_unstable();
        drawn
    };
    let counted = overflow
        .iter()
        .map(|(_, count)| count.0)
        .next()
        .unwrap_or(0);
    let mut sorted = wanted.clone();
    sorted.sort_unstable();
    // **Both halves**, because they are two questions: who is drawn, and how many are not.
    // Comparing only the first is how the count came to be spawned never — the ninth speaker
    // changes `hidden` and leaves the eight rows exactly as they were.
    if sorted == drawn && hidden == counted {
        return;
    }

    for (entity, _) in &rows {
        commands.entity(entity).despawn();
    }
    for (entity, _) in &overflow {
        commands.entity(entity).despawn();
    }
    for panel in &panels {
        commands.entity(panel).with_children(|list| {
            for entity_id in &wanted {
                // Named the way every other roster names them, and drawn as an id when the
                // description has not arrived — `ui/voice.rs`'s rule, for its reason: hearing
                // somebody the client cannot name is a real state, and dropping the row would
                // leave a speaker nobody can mute.
                let name = appearances
                    .name(*entity_id)
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| format!("player {entity_id}"));
                spawn_voice_row(list, *entity_id, name);
            }
            if hidden > 0 {
                list.spawn((
                    VoiceOverflow(hidden),
                    Text::new(format!("+{hidden} more")),
                    TextFont {
                        font_size: ROW_FONT,
                        ..default()
                    },
                    TextColor(Color::srgba(0.72, 0.75, 0.80, 0.75)),
                    TextLayout::no_wrap(),
                ));
            }
        });
    }
}

/// Gives the Voices panel a `Display` and takes it away.
///
/// `Display`, never `Visibility`, for [`show_monitor_dropdown`]'s reason: a hidden node still
/// occupies its layout box, and a closed panel must be out of hit-testing so the rows it
/// covered are pressable again.
fn show_voices_panel(screen: Res<SettingsScreen>, mut panels: Query<&mut Node, With<VoicesPanel>>) {
    let next = if screen.voices_open {
        Display::Flex
    } else {
        Display::None
    };
    for mut node in &mut panels {
        if node.display != next {
            node.display = next;
        }
    }
}

/// Keeps every value on the panel in step with the settings behind it.
fn refresh_readings(
    settings: Res<Settings>,
    monitors: Res<MonitorChoices>,
    devices: Res<AudioDevices>,
    voices: Res<Voices>,
    screen: Res<SettingsScreen>,
    mut readings: Query<(&Reading, &mut Text)>,
) {
    // `Voices` is written on every frame anybody is speaking, so this is the one input that
    // is nearly always "changed" while a conversation is happening. It is still cheaper than
    // the alternative — a per-speaker reading that only refreshed when a *setting* moved
    // would sit at a stale volume for as long as nobody touched the rest of the screen.
    if !settings.is_changed()
        && !monitors.is_changed()
        && !devices.is_changed()
        && !voices.is_changed()
        && !screen.is_changed()
    {
        return;
    }
    let choices = Choices {
        monitors: &monitors,
        devices: &devices,
    };
    for (reading, mut text) in &mut readings {
        let next = describe(&settings, choices, &voices, &screen, *reading);
        if text.0 != next {
            text.0 = next;
        }
    }
}

/// What one piece of text on the panel says. A pure function of the two resources, so the
/// panel's content is testable with no window.
fn describe(
    settings: &Settings,
    choices: Choices<'_>,
    voices: &Voices,
    screen: &SettingsScreen,
    reading: Reading,
) -> String {
    match reading {
        Reading::Knob(knob) => settings.reading_with_choices(knob, choices),
        Reading::Vsync => on_or_off(settings.vsync()),
        Reading::Readout => on_or_off(settings.readout_shown()),
        Reading::ReadoutCorner => settings.readout_corner().name().to_owned(),
        // "v" stands in for a down chevron: `ascii_guard` in `ui/mod.rs` holds every
        // string here to the 95 codepoints Bevy's embedded font can draw.
        Reading::MonitorControl => {
            format!(
                "{} v",
                settings.reading_with_choices(Knob::Monitor, choices)
            )
        }
        Reading::VoicesPanel => if screen.voices_open { "HIDE" } else { "SHOW" }.to_owned(),
        Reading::VoiceLevel(entity_id) => format!("{}%", voices.volume(entity_id)),
        Reading::VoiceMuted(entity_id) => if voices.muted(entity_id) {
            "UNMUTE"
        } else {
            "MUTE"
        }
        .to_owned(),
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
    use crate::audio::{HEARD_FOR, MAX_VOICE};
    use crate::settings::{Corner, DeviceChoice, Knob, MonitorPreference};
    use crate::ui::health::DEFAULT_FONT_ADVANCE_EM;

    fn screen_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<InputMode>()
            .insert_resource(ButtonInput::<KeyCode>::default())
            .insert_resource(MonitorChoices::named(&["Main display", "Side display"]))
            // Two of each, so a knob whose bound is the machine's has somewhere to step.
            // No `AudioPlugin` and therefore no device anywhere: this is the list
            // `offer_the_output_devices` would have written, inserted by hand.
            .insert_resource(AudioDevices::named(
                &["Built-in speakers", "USB headset"],
                &["Built-in microphone", "USB headset mic"],
            ))
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

    /// Records `entity_id` as heard now, as `audio/heard.rs` does when a frame of theirs is
    /// decoded, and runs a frame.
    fn hear(app: &mut App, entity_id: u64) {
        app.world_mut()
            .resource_mut::<Voices>()
            .heard(entity_id, std::time::Instant::now());
        app.update();
    }

    /// Presses one of a speaker's controls inside the open Voices panel.
    fn press_voice(app: &mut App, wanted: VoiceControl) {
        let button = {
            let world = app.world_mut();
            let mut query = world.query::<(Entity, &VoiceControl)>();
            query
                .iter(world)
                .find(|(_, control)| **control == wanted)
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
        app.update();
    }

    /// Every speaker the Voices panel currently draws a row for, in order.
    fn voice_rows(app: &mut App) -> Vec<u64> {
        let world = app.world_mut();
        let mut query = world.query::<&VoiceRow>();
        query.iter(world).map(|row| row.0).collect()
    }

    /// Presses the Monitor row's closed control, opening or closing the dropdown.
    fn press_monitor_toggle(app: &mut App) {
        let button = {
            let world = app.world_mut();
            let mut query = world.query_filtered::<Entity, With<MonitorSelectButton>>();
            query
                .iter(world)
                .next()
                .expect("the monitor select control exists")
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

    /// Presses the dropdown option at `index` into [`MonitorChoices::preferences`].
    fn press_monitor_option(app: &mut App, index: usize) {
        let button = {
            let world = app.world_mut();
            let mut query = world.query::<(Entity, &MonitorOption)>();
            query
                .iter(world)
                .find(|(_, option)| option.0 == index)
                .map(|(entity, _)| entity)
                .unwrap_or_else(|| panic!("no dropdown option at index {index}"))
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

    /// Whether the dropdown panel is currently drawn, read from `Display` — what
    /// [`show_monitor_dropdown`] actually writes — rather than the resource flag alone.
    fn monitor_dropdown_shown(app: &mut App) -> bool {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Node, With<MonitorDropdownPanel>>();
        query
            .iter(world)
            .next()
            .map(|node| node.display != Display::None)
            .unwrap_or(false)
    }

    /// The dropdown options currently drawn, in ascending index order, as `(index, label)`.
    fn monitor_options(app: &mut App) -> Vec<(usize, String)> {
        let world = app.world_mut();
        let mut rows = world.query::<(&MonitorOption, &Children)>();
        let labelled: Vec<(usize, Entity)> = rows
            .iter(world)
            .map(|(option, children)| {
                (
                    option.0,
                    children.iter().next().expect("an option has a label"),
                )
            })
            .collect();

        let mut texts = world.query::<&Text>();
        let mut options: Vec<(usize, String)> = labelled
            .into_iter()
            .map(|(index, child)| {
                let label = texts
                    .get(world, child)
                    .map(|text| text.0.clone())
                    .unwrap_or_default();
                (index, label)
            })
            .collect();
        options.sort_by_key(|(index, _)| *index);
        options
    }

    /// Exact width under Bevy's embedded monospace FiraMono font.
    fn row_text_width(value: &str) -> f32 {
        value.chars().count() as f32 * DEFAULT_FONT_ADVANCE_EM * ROW_FONT_SIZE
    }

    #[test]
    fn every_knob_has_a_control_at_each_end_and_a_reading_between_them() {
        let mut app = screen_app();
        for knob in KNOBS {
            // Monitor draws as `Row::MonitorSelect` now, not `Row::Knob` — it has no `-`
            // or `+` button and no `Reading::Knob(Knob::Monitor)` node to read.
            // `the_window_rows_offer_the_modes_and_the_attached_monitors_by_name` and the
            // dropdown-specific tests below cover it instead.
            if knob == Knob::Monitor {
                continue;
            }
            let before = app.world().resource::<Settings>().clone();
            // **The outward press is not always `+`.** A knob whose default already sits at
            // one end of its bound cannot be nudged further that way, and `VoiceVolume` is
            // deliberately one of those — it starts at unity, because a player who cannot
            // hear their friends turns it up. Pressing `+` and asserting movement would make
            // "every knob starts below its maximum" a silent precondition of this row test,
            // true by accident until this part and false from it on.
            let mut steps = 1;
            press(&mut app, SettingsAction::Nudge(knob, steps));
            if *app.world().resource::<Settings>() == before {
                steps = -1;
                press(&mut app, SettingsAction::Nudge(knob, steps));
            }
            let after = app.world().resource::<Settings>().clone();
            assert_ne!(before, after, "{knob:?} did not move in either direction");
            let expected = after.reading_with_choices(
                knob,
                Choices {
                    monitors: app.world().resource::<MonitorChoices>(),
                    devices: app.world().resource::<AudioDevices>(),
                },
            );
            assert_eq!(reading_of(&mut app, Reading::Knob(knob)), expected);

            press(&mut app, SettingsAction::Nudge(knob, -steps));
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
            reading_of(&mut app, Reading::MonitorControl),
            "primary - Main display (1920x1080 at 0,0) v"
        );

        press(&mut app, SettingsAction::Nudge(Knob::WindowMode, 1));
        press_monitor_toggle(&mut app);
        assert!(
            monitor_dropdown_shown(&mut app),
            "the dropdown did not open"
        );
        press_monitor_option(&mut app, 1);
        assert!(
            !monitor_dropdown_shown(&mut app),
            "selecting an option left the dropdown open"
        );
        assert_eq!(
            reading_of(&mut app, Reading::Knob(Knob::WindowMode)),
            "windowed"
        );
        assert_eq!(
            reading_of(&mut app, Reading::MonitorControl),
            "Side display (1920x1080 at 1920,0) v"
        );

        press(&mut app, SettingsAction::Reset(Tab::Graphics));
        assert_eq!(
            reading_of(&mut app, Reading::Knob(Knob::WindowMode)),
            "borderless"
        );
        assert_eq!(
            reading_of(&mut app, Reading::MonitorControl),
            "primary - Main display (1920x1080 at 0,0) v"
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
    /// bound. The monitor and the two devices are deliberately excluded: hardware owns all
    /// three names, so they follow the clipped overflow policy asserted separately below.
    #[test]
    fn every_bounded_reading_fits_complete_on_one_line() {
        let monitors = MonitorChoices::named(&["Main display", "Side display"]);
        let devices = AudioDevices::default();
        let bounds = Choices {
            monitors: &monitors,
            devices: &devices,
        };
        let mut values = Vec::new();
        for knob in KNOBS.into_iter().filter(|knob| {
            knob.tab() != Tab::Controls
                && *knob != Knob::Monitor
                && *knob != Knob::OutputDevice
                && *knob != Knob::InputDevice
        }) {
            for steps in [-10_000, 10_000] {
                let mut settings = Settings::default();
                settings.adjust_with_choices(knob, steps, bounds);
                values.push(settings.reading_with_choices(knob, bounds));
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
    /// by the fixed-width, single-line control instead of widening the row.
    #[test]
    fn monitor_readings_are_complete_normally_and_clip_unbounded_names_on_one_line() {
        let mut app = screen_app();
        let normal = "primary - Main display (1920x1080 at 0,0) v";
        assert_eq!(reading_of(&mut app, Reading::MonitorControl), normal);
        assert!(row_text_width(normal) <= STEPPER_WIDTH);

        let long_name = "External-monitor-name-".repeat(32);
        *app.world_mut().resource_mut::<MonitorChoices>() =
            MonitorChoices::named(&[long_name.as_str()]);
        app.update();

        let world = app.world_mut();
        let mut readings = world.query::<(&Reading, &Text, &Node, &TextLayout)>();
        let (_, text, node, layout) = readings
            .iter(world)
            .find(|(reading, _, _, _)| **reading == Reading::MonitorControl)
            .expect("the monitor control has a reading");
        assert!(
            text.0.contains(&long_name),
            "the model value was abbreviated"
        );
        assert!(
            row_text_width(&text.0) > STEPPER_WIDTH,
            "the overflow fixture unexpectedly fits"
        );
        assert_eq!(node.width, Val::Px(STEPPER_WIDTH));
        assert_eq!(node.flex_shrink, 0.0);
        assert_eq!(node.overflow, Overflow::clip());
        assert_eq!(layout.linebreak, LineBreak::NoWrap);
        assert_eq!(layout.justify, Justify::Center);

        // And the control itself, not only its text node, kept the stepper column's exact
        // width — a long name is clipped inside the control rather than widening the row.
        assert_eq!(
            marker_node::<MonitorSelectButton>(&mut app).width,
            Val::Px(STEPPER_WIDTH)
        );
    }

    // -------------------------------------------------------------------------
    // The Monitor dropdown
    // -------------------------------------------------------------------------

    /// Closed by default, and the closed control names both the current monitor and an
    /// open indicator through one `Reading::MonitorControl` string.
    #[test]
    fn the_monitor_dropdown_starts_closed() {
        let mut app = screen_app();
        assert!(!monitor_dropdown_shown(&mut app));
        assert!(
            !app.world()
                .resource::<SettingsScreen>()
                .monitor_dropdown_open
        );
        let closed = reading_of(&mut app, Reading::MonitorControl);
        assert!(closed.contains("Main display"), "{closed}");
        assert!(closed.ends_with(" v"), "no open indicator in {closed:?}");
    }

    /// Pressing the closed control opens it, and the list holds exactly `Primary` plus
    /// every live entry `MonitorChoices` reports.
    #[test]
    fn clicking_the_closed_control_opens_a_list_of_primary_and_every_live_monitor() {
        let mut app = screen_app();
        press_monitor_toggle(&mut app);
        assert!(monitor_dropdown_shown(&mut app));
        assert!(
            app.world()
                .resource::<SettingsScreen>()
                .monitor_dropdown_open
        );
        assert_eq!(
            monitor_options(&mut app),
            vec![
                (0, "Primary".to_owned()),
                (1, "Side display (1920x1080 at 1920,0)".to_owned()),
            ]
        );

        // And it toggles: the same control closes what it opened.
        press_monitor_toggle(&mut app);
        assert!(!monitor_dropdown_shown(&mut app));
    }

    /// Selecting an option applies it to [`Settings`] and closes the list — the AC by name.
    #[test]
    fn selecting_an_option_applies_it_and_closes_the_list() {
        let mut app = screen_app();
        press_monitor_toggle(&mut app);
        let side = app.world().resource::<MonitorChoices>().preferences()[1].clone();

        press_monitor_option(&mut app, 1);

        assert!(!monitor_dropdown_shown(&mut app), "the list did not close");
        assert!(
            !app.world()
                .resource::<SettingsScreen>()
                .monitor_dropdown_open
        );
        assert_eq!(*app.world().resource::<Settings>().monitor(), side);
    }

    /// A saved monitor the operating system no longer reports stays on screen as
    /// `<value> (unavailable)` rather than being silently discarded, and it is not one of
    /// the options offered. It is replaced the moment the player picks something else.
    #[test]
    fn an_unavailable_saved_monitor_is_shown_but_not_offered_and_is_replaced_on_selection() {
        let mut app = screen_app();
        let vanished = MonitorPreference::Specific("name:6c6f7374".to_owned());
        app.world_mut()
            .resource_mut::<Settings>()
            .set_monitor(vanished.clone());
        app.update();

        let closed = reading_of(&mut app, Reading::MonitorControl);
        assert!(closed.contains("(unavailable)"), "{closed}");

        press_monitor_toggle(&mut app);
        // Exactly the two live entries — the unavailable saved preference is not a third
        // option, "(unavailable)" or otherwise, because there is nothing live behind it.
        assert_eq!(
            monitor_options(&mut app),
            vec![
                (0, "Primary".to_owned()),
                (1, "Side display (1920x1080 at 1920,0)".to_owned()),
            ]
        );

        press_monitor_option(&mut app, 0);
        assert_eq!(
            *app.world().resource::<Settings>().monitor(),
            MonitorPreference::Primary,
            "selecting Primary did not replace the unavailable preference"
        );
    }

    /// Escape takes the dropdown down first; only a second press, with nothing left
    /// waiting, closes the screen behind it.
    #[test]
    fn escape_closes_the_dropdown_before_it_closes_the_screen() {
        let mut app = screen_app();
        press_monitor_toggle(&mut app);
        assert!(monitor_dropdown_shown(&mut app));

        press_key(&mut app, KeyCode::Escape);
        assert!(
            !monitor_dropdown_shown(&mut app),
            "escape did not close the dropdown"
        );
        assert!(
            app.world().resource::<SettingsScreen>().is_open(),
            "the first escape also closed the screen"
        );

        release_keys(&mut app);
        press_key(&mut app, KeyCode::Escape);
        assert!(
            !app.world().resource::<SettingsScreen>().is_open(),
            "the second escape, with nothing waiting, did not close the screen"
        );
    }

    /// Changing tabs leaves no orphaned dropdown, exactly as it takes back a capture armed
    /// on the tab being left.
    #[test]
    fn switching_tabs_closes_an_open_monitor_dropdown() {
        let mut app = screen_app();
        // The screen opens on Controls; Monitor lives on Graphics, so open it there first —
        // otherwise switching *to* Controls, the tab already showing, would be the no-op
        // `pressing_the_tab_already_showing_leaves_a_capture_armed` exists to name.
        press_tab(&mut app, Tab::Graphics);
        press_monitor_toggle(&mut app);
        assert!(monitor_dropdown_shown(&mut app));

        press_tab(&mut app, Tab::Controls);
        assert!(
            !monitor_dropdown_shown(&mut app),
            "the dropdown survived a tab switch"
        );
        assert!(
            !app.world()
                .resource::<SettingsScreen>()
                .monitor_dropdown_open
        );
    }

    /// Resetting graphics puts the Monitor preference itself back, and leaves no dropdown
    /// open over whatever value it just replaced.
    #[test]
    fn resetting_graphics_closes_an_open_monitor_dropdown() {
        let mut app = screen_app();
        press_monitor_toggle(&mut app);
        press_monitor_option(&mut app, 1);
        press_monitor_toggle(&mut app);
        assert!(monitor_dropdown_shown(&mut app));

        press(&mut app, SettingsAction::Reset(Tab::Graphics));
        assert!(
            !monitor_dropdown_shown(&mut app),
            "the dropdown survived a graphics reset"
        );
        assert_eq!(
            app.world().resource::<Settings>().monitor(),
            &MonitorPreference::Primary
        );
    }

    /// Closing the screen — `BACK`, or losing the pause menu behind it — leaves nothing
    /// open for the next time the panel is shown.
    #[test]
    fn closing_settings_closes_an_open_monitor_dropdown() {
        let mut app = screen_app();
        press_monitor_toggle(&mut app);
        assert!(monitor_dropdown_shown(&mut app));

        press(&mut app, SettingsAction::Back);
        assert!(!monitor_dropdown_shown(&mut app));

        // And reopening the screen finds it shut, not wherever it was left.
        app.world_mut().resource_mut::<SettingsScreen>().open();
        app.update();
        assert!(!monitor_dropdown_shown(&mut app));
    }

    /// The geometry the acceptance criteria name: the closed control fills the stepper
    /// column, the dropdown is anchored directly under it at the same width, and a
    /// `GlobalZIndex` of its own sits it above every other row.
    #[test]
    fn the_dropdown_is_anchored_below_the_control_at_the_same_width_and_above_other_rows() {
        let mut app = screen_app();
        let control = marker_node::<MonitorSelectButton>(&mut app);
        assert_eq!(control.width, Val::Px(STEPPER_WIDTH));
        assert_eq!(control.align_items, AlignItems::Center);

        let panel = marker_node::<MonitorDropdownPanel>(&mut app);
        assert_eq!(panel.position_type, PositionType::Absolute);
        assert_eq!(panel.left, Val::Px(0.0));
        assert_eq!(
            panel.top, control.height,
            "the dropdown is not anchored directly below the control's own height"
        );
        assert_eq!(
            panel.width, control.width,
            "the dropdown is not the same width as the control it belongs to"
        );

        let world = app.world_mut();
        let mut layers = world.query_filtered::<&GlobalZIndex, With<MonitorDropdownPanel>>();
        let layer = layers
            .iter(world)
            .next()
            .expect("the dropdown panel carries a stacking layer");
        assert!(
            layer.0 > 45,
            "the dropdown does not outrank the settings screen it overlays: {layer:?}"
        );
    }

    /// Options read left to right rather than centred: left-aligned for scanning, and
    /// vertically centred in their row exactly as every other control on this screen is.
    #[test]
    fn dropdown_options_are_left_aligned_and_vertically_centred() {
        let mut app = screen_app();
        press_monitor_toggle(&mut app);

        let world = app.world_mut();
        let mut options = world.query_filtered::<&Node, With<MonitorOption>>();
        let mut seen = 0;
        for node in options.iter(world) {
            seen += 1;
            assert_eq!(node.justify_content, JustifyContent::FlexStart);
            assert_eq!(node.align_items, AlignItems::Center);
        }
        assert_eq!(seen, 2, "expected exactly the two live options");
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
        // `KeyB` and not `KeyV`: the latter is `Control::Talk`'s own key since #852, and
        // a capture that landed on it would be refused rather than bound.
        press_key(&mut app, KeyCode::KeyB);
        assert_eq!(
            app.world()
                .resource::<Settings>()
                .bindings()
                .key(Control::Consume),
            KeyCode::KeyB
        );
        assert_eq!(
            reading_of(&mut app, Reading::Binding(Control::Consume)),
            "b"
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
            // Monitor is drawn as `Row::MonitorSelect`, not `Row::Knob` — asserted on its
            // own just below, the same way the toggles get their own count beneath this
            // loop rather than being folded into it.
            if knob == Knob::Monitor {
                continue;
            }
            let drawn = all
                .iter()
                .filter(|row| matches!(row, Row::Knob(drawn) if *drawn == knob))
                .count();
            assert_eq!(drawn, 1, "{knob:?} has {drawn} rows");
        }
        let monitor_rows = all
            .iter()
            .filter(|row| matches!(row, Row::MonitorSelect))
            .count();
        assert_eq!(monitor_rows, 1, "Monitor has {monitor_rows} rows");
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

        // The same for the action rows, which are the other half `rows_of` writes by hand.
        let actions: Vec<&Row> = all
            .iter()
            .filter(|row| matches!(row, Row::Action(..)))
            .collect();
        assert_eq!(actions.len(), 1, "{actions:?}");
        assert!(matches!(
            actions[0],
            Row::Action(_, SettingsAction::TestSpeakers, _)
        ));
    }

    // -------------------------------------------------------------------------
    // The Audio tab
    // -------------------------------------------------------------------------

    /// Third in the strip, holding its own knobs and the speaker test and nothing the other
    /// two tabs claim.
    ///
    /// **The order is the assertion, not just the membership.** The two devices sit under
    /// the volume they feed, and the voice rows read as one sentence downwards: what the
    /// microphone is for, what opens it, and who hears the result.
    #[test]
    fn the_audio_tab_is_after_graphics_and_holds_its_own_rows() {
        assert_eq!(Tab::ALL, [Tab::Controls, Tab::Graphics, Tab::Audio]);

        let labels: Vec<&str> = rows_of(Tab::Audio).iter().map(|row| row.label()).collect();
        assert_eq!(
            labels,
            vec![
                "Master volume",
                "Output device",
                "Microphone",
                "Voice volume",
                "Voice",
                "Voice threshold",
                "Heard by",
                "Test speakers",
                "Voices"
            ]
        );
        for other in [Tab::Controls, Tab::Graphics] {
            assert!(
                !rows_of(other)
                    .iter()
                    .any(|row| row.label() == "Test speakers"),
                "an audio row landed on {other:?}"
            );
        }

        let mut app = screen_app();
        press_tab(&mut app, Tab::Audio);
        assert_eq!(shown_tabs(&mut app), vec![Tab::Audio]);
        assert_eq!(
            reading_of(&mut app, Reading::Knob(Knob::MasterVolume)),
            "80%"
        );
        assert_eq!(
            reading_of(&mut app, Reading::Knob(Knob::OutputDevice)),
            "system default"
        );

        // The knob moves, the reading follows, and the reset that owns it puts it back
        // without reaching the tab beside it.
        press(&mut app, SettingsAction::Nudge(Knob::MasterVolume, -1));
        press(&mut app, SettingsAction::Nudge(Knob::MasterVolume, -1));
        assert_eq!(
            reading_of(&mut app, Reading::Knob(Knob::MasterVolume)),
            "70%"
        );
        press(&mut app, SettingsAction::Nudge(Knob::RenderDistance, -1));
        let distance = app.world().resource::<Settings>().render_distance();

        press(&mut app, SettingsAction::Reset(Tab::Audio));
        let settings = app.world().resource::<Settings>();
        assert_eq!(settings.master_volume(), 80);
        assert_eq!(
            settings.render_distance(),
            distance,
            "RESET AUDIO reached the graphics tab"
        );
    }

    /// The device row steps through what the machine offers, the reading follows, the
    /// audio-scoped reset puts it back — and a device that goes away keeps its place in the
    /// row rather than being silently replaced by one that is present.
    #[test]
    fn the_output_device_row_offers_the_machines_devices_and_marks_an_absent_one() {
        let mut app = screen_app();
        press_tab(&mut app, Tab::Audio);

        // One press per button, because that is what the row spawns: the steppers are
        // `Nudge(knob, -1)` and `Nudge(knob, 1)` and nothing else.
        press(&mut app, SettingsAction::Nudge(Knob::OutputDevice, 1));
        press(&mut app, SettingsAction::Nudge(Knob::OutputDevice, 1));
        assert_eq!(
            app.world().resource::<Settings>().output_device(),
            &DeviceChoice::Named("USB headset".to_owned()),
            "the row stepped somewhere the machine does not offer"
        );
        assert_eq!(
            reading_of(&mut app, Reading::Knob(Knob::OutputDevice)),
            "USB headset"
        );

        // The headset is unplugged. The choice stands, and the row says why it is silent.
        *app.world_mut().resource_mut::<AudioDevices>() =
            AudioDevices::named(&["Built-in speakers"], &["Built-in microphone"]);
        app.update();
        assert_eq!(
            app.world().resource::<Settings>().output_device(),
            &DeviceChoice::Named("USB headset".to_owned()),
            "the choice was rewritten by the device going away"
        );
        assert_eq!(
            reading_of(&mut app, Reading::Knob(Knob::OutputDevice)),
            "USB headset (unavailable)"
        );

        // And the reset that owns the row puts it back without reaching the tab beside it.
        press(&mut app, SettingsAction::Nudge(Knob::RenderDistance, -1));
        let distance = app.world().resource::<Settings>().render_distance();
        press(&mut app, SettingsAction::Reset(Tab::Audio));
        let settings = app.world().resource::<Settings>();
        assert_eq!(settings.output_device(), &DeviceChoice::SystemDefault);
        assert_eq!(
            settings.render_distance(),
            distance,
            "RESET AUDIO reached the graphics tab"
        );
    }

    /// **The row is a request and nothing else.** It sets the flag `audio/mod.rs` takes
    /// back when it starts the tone; this screen owns no sample, no bus and no device, and
    /// pressing it changes not one setting.
    #[test]
    fn the_test_speakers_row_asks_the_audio_module_for_a_tone() {
        let mut app = screen_app();
        press_tab(&mut app, Tab::Audio);
        assert!(
            !app.world().resource::<AudioControls>().speaker_test,
            "something asked for a tone before the row was pressed"
        );
        let before = app.world().resource::<Settings>().clone();

        press(&mut app, SettingsAction::TestSpeakers);

        assert!(app.world().resource::<AudioControls>().speaker_test);
        assert_eq!(
            *app.world().resource::<Settings>(),
            before,
            "the speaker test moved a setting"
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

    // -------------------------------------------------------------------------
    // The Voices panel
    // -------------------------------------------------------------------------

    /// **The panel lists whoever has been heard, and the mute and volume it offers reach the
    /// resource the mixer reads.** Driven through the assembled screen — the press goes to a
    /// real button and the assertion is on `Voices`, so a control wired to nothing fails here.
    #[test]
    fn the_voices_panel_lists_who_was_heard_and_its_controls_reach_them() {
        let mut app = screen_app();
        *app.world_mut().resource_mut::<Tab>() = Tab::Audio;
        app.update();
        assert!(
            voice_rows(&mut app).is_empty(),
            "a silent session drew rows"
        );

        hear(&mut app, 7);
        hear(&mut app, 9);
        assert_eq!(voice_rows(&mut app), vec![7, 9]);

        press(&mut app, SettingsAction::ToggleVoices);
        assert_eq!(reading_of(&mut app, Reading::VoicesPanel), "HIDE");

        assert_eq!(reading_of(&mut app, Reading::VoiceLevel(7)), "100%");
        assert_eq!(reading_of(&mut app, Reading::VoiceMuted(7)), "MUTE");

        press_voice(&mut app, VoiceControl::Mute(7));
        assert!(
            app.world().resource::<Voices>().muted(7),
            "the mute button reached nothing"
        );
        assert_eq!(reading_of(&mut app, Reading::VoiceMuted(7)), "UNMUTE");
        assert!(
            !app.world().resource::<Voices>().muted(9),
            "muting one speaker muted another"
        );

        press_voice(&mut app, VoiceControl::Nudge(7, 1));
        press_voice(&mut app, VoiceControl::Nudge(7, 1));
        assert_eq!(app.world().resource::<Voices>().volume(7), 120);
        assert_eq!(reading_of(&mut app, Reading::VoiceLevel(7)), "120%");

        press_voice(&mut app, VoiceControl::Nudge(7, -1));
        assert_eq!(app.world().resource::<Voices>().volume(7), 110);
    }

    /// **A speaker's volume runs to twice unity and stops**, which is what the acceptance
    /// criterion's 0-200% means at the control rather than in the model.
    #[test]
    fn a_speakers_volume_stops_at_both_ends_from_the_panel() {
        let mut app = screen_app();
        *app.world_mut().resource_mut::<Tab>() = Tab::Audio;
        hear(&mut app, 7);
        press(&mut app, SettingsAction::ToggleVoices);

        for _ in 0..15 {
            press_voice(&mut app, VoiceControl::Nudge(7, 1));
        }
        assert_eq!(app.world().resource::<Voices>().volume(7), MAX_VOICE);
        assert_eq!(
            reading_of(&mut app, Reading::VoiceLevel(7)),
            format!("{MAX_VOICE}%")
        );

        for _ in 0..30 {
            press_voice(&mut app, VoiceControl::Nudge(7, -1));
        }
        assert_eq!(app.world().resource::<Voices>().volume(7), 0);
        assert!(
            !app.world().resource::<Voices>().muted(7),
            "turning a speaker down to nothing muted them, which is a different button"
        );
    }

    /// **The rows are rebuilt when the set of speakers changes and never merely because
    /// somebody spoke.** `Voices` is marked changed on every frame anybody is talking, so a
    /// rebuild driven by `Res::is_changed` would despawn and respawn the row under a pointer
    /// mid-press — the trap `rebuild_monitor_options` and `ui/servers.rs` both record.
    #[test]
    fn a_speaker_still_talking_does_not_have_their_row_rebuilt() {
        let mut app = screen_app();
        *app.world_mut().resource_mut::<Tab>() = Tab::Audio;
        hear(&mut app, 7);
        press(&mut app, SettingsAction::ToggleVoices);

        let before = {
            let world = app.world_mut();
            let mut query = world.query::<(Entity, &VoiceRow)>();
            query
                .iter(world)
                .find(|(_, row)| row.0 == 7)
                .map(|(entity, _)| entity)
                .expect("a row for the speaker")
        };

        // Ten more frames of that speaker talking, which marks `Voices` changed every time.
        for _ in 0..10 {
            hear(&mut app, 7);
        }

        let after = {
            let world = app.world_mut();
            let mut query = world.query::<(Entity, &VoiceRow)>();
            query
                .iter(world)
                .find(|(_, row)| row.0 == 7)
                .map(|(entity, _)| entity)
                .expect("a row for the speaker")
        };
        assert_eq!(
            before, after,
            "the row was rebuilt while its speaker was still talking"
        );

        // And a genuinely new speaker does rebuild it.
        hear(&mut app, 9);
        assert_eq!(voice_rows(&mut app), vec![7, 9]);
    }

    /// **A crowd is bounded and counted**, exactly as the HUD's speaker line is: the panel is
    /// an overlay on a screen with a narrowest supported viewport, and one row per speaker
    /// would run off the bottom of it.
    #[test]
    fn more_speakers_than_the_panel_draws_are_counted() {
        let mut app = screen_app();
        *app.world_mut().resource_mut::<Tab>() = Tab::Audio;
        press(&mut app, SettingsAction::ToggleVoices);
        for entity_id in 0..(PANEL_VOICES as u64 + 3) {
            hear(&mut app, entity_id);
        }
        assert_eq!(voice_rows(&mut app).len(), PANEL_VOICES);

        let world = app.world_mut();
        let mut query = world.query::<(&VoiceOverflow, &Text)>();
        let overflow = query.iter(world).next().map(|(_, text)| text.0.clone());
        assert_eq!(overflow, Some("+3 more".to_owned()));
    }

    /// A speaker nobody has heard for [`HEARD_FOR`] leaves the panel — which is the criterion's
    /// sixty seconds, read at the screen rather than at the model.
    #[test]
    fn a_speaker_nobody_has_heard_for_a_minute_leaves_the_panel() {
        let mut app = screen_app();
        *app.world_mut().resource_mut::<Tab>() = Tab::Audio;
        press(&mut app, SettingsAction::ToggleVoices);

        let long_ago = std::time::Instant::now() - HEARD_FOR - std::time::Duration::from_secs(1);
        app.world_mut().resource_mut::<Voices>().heard(7, long_ago);
        app.update();
        assert!(
            voice_rows(&mut app).is_empty(),
            "somebody nobody has heard for a minute was still on the panel"
        );
    }

    /// The panel is an overlay with its own lifecycle: its row opens and closes it, Escape
    /// closes it before it closes the screen, and leaving the tab closes it rather than
    /// leaving it floating over another tab's rows.
    #[test]
    fn the_voices_panel_closes_on_escape_and_on_a_tab_change() {
        let mut app = screen_app();
        *app.world_mut().resource_mut::<Tab>() = Tab::Audio;
        app.update();

        let shown = |app: &mut App| {
            let world = app.world_mut();
            let mut query = world.query_filtered::<&Node, With<VoicesPanel>>();
            query.iter(world).all(|node| node.display == Display::Flex)
        };
        assert!(!shown(&mut app), "the panel started open");

        press(&mut app, SettingsAction::ToggleVoices);
        assert!(shown(&mut app));
        press(&mut app, SettingsAction::ToggleVoices);
        assert!(!shown(&mut app), "its own row did not close it");

        press(&mut app, SettingsAction::ToggleVoices);
        press_key(&mut app, KeyCode::Escape);
        assert!(!shown(&mut app), "escape did not close the panel");
        assert!(
            app.world().resource::<SettingsScreen>().is_open(),
            "escape closed the whole screen while the panel was up"
        );

        press(&mut app, SettingsAction::ToggleVoices);
        press_tab(&mut app, Tab::Graphics);
        assert!(!shown(&mut app), "the panel outlived the tab it belongs to");
    }
}
