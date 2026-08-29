//! The heading strip across the top of the gameplay HUD.
//!
//! Pure presentation, and deliberately the thinnest kind there is: it reads
//! [`LookState::yaw`] — the client's own look state, the one thing in `player/` that is
//! not the server's answer — and turns it into a row of ticks that slides under a fixed
//! pointer. Nothing here decides anything, nothing here is sent, and there is no second
//! heading resource: a compass that kept its own facing would be a second answer to a
//! question `LookState` already answers, and the two would drift the first time one of
//! them missed a frame.
//!
//! ## The one piece of arithmetic worth stating
//!
//! `LookState::yaw` is a rotation about the world's up axis with `0` looking along `-Z`,
//! applied as `Quat::from_rotation_y(yaw)` in `player/camera.rs`. The world's compass is
//! the one `player/sky.rs` and `player/structures.rs` already mirror from the server —
//! **North is -Z, East is +X, South is +Z, West is -X**. Rotating `-Z` by `yaw` about `+Y`
//! gives `(-sin yaw, 0, -cos yaw)`, which is `sin(-yaw)` east of due north: **a positive
//! yaw turns west, so the compass bearing is the yaw negated.** The server's own movement
//! tests say the same thing from the other side — `yawEast = -π/2`.
//!
//! Every mark's place on the strip is the *wrapped* difference between its bearing and the
//! player's, which is what makes crossing North a step rather than a jump: at a heading of
//! 359°, North is one degree to the right, not three hundred and fifty-nine to the left.
//! [`offset_degrees`] is where that wrap lives and it is the only place it happens.
//!
//! ## Why nothing here is culled by `Visibility`
//!
//! The strip is a fixed-width window with [`Overflow::clip`], and a mark whose offset puts
//! it outside that window is simply outside it. Hiding marks individually would have meant
//! setting `Visibility::Visible` on a child, and in Bevy that is **unconditional** — a
//! child marked `Visible` draws even when its parent is `Hidden`, so every mark would have
//! had to learn about the mode gate on the root as well. One clip rectangle and one
//! visibility, on the root, is the whole of it.

use bevy::prelude::*;
use bevy::ui::FocusPolicy;

use crate::net::Session;
use crate::player::{AimCamera, InputMode, LookState, PlayerStats};

use super::storm::Storm;

/// Distance from the top of the window, in logical pixels.
const TOP: f32 = 12.0;

/// The visible width of the strip, in logical pixels.
const STRIP_WIDTH: f32 = 420.0;

/// The height of the strip, in logical pixels: a tick and a label under it.
const STRIP_HEIGHT: f32 = 34.0;

/// How far a degree of heading moves a mark, in logical pixels.
///
/// [`STRIP_WIDTH`] divided by this is the span the player can read at once — 105°, a
/// little under a third of the circle, so the two cardinals either side of the heading are
/// on screen at nearly every bearing.
const PIXELS_PER_DEGREE: f32 = 4.0;

/// The spacing between marks, in degrees.
///
/// Fifteen divides 45 and therefore 90, so every cardinal and every intercardinal lands on
/// a mark rather than between two of them.
const MARK_STEP_DEGREES: u16 = 15;

/// How many marks the strip carries. A full circle at [`MARK_STEP_DEGREES`].
const MARK_COUNT: u16 = 360 / MARK_STEP_DEGREES;

/// The width each mark centres its tick and label inside, in logical pixels.
const MARK_SLOT: f32 = 44.0;

/// The fixed centre pointer's width, in logical pixels.
const POINTER_WIDTH: f32 = 3.0;

const TICK_WIDTH: f32 = 2.0;
const TICK_HEIGHT: f32 = 8.0;
const CARDINAL_TICK_WIDTH: f32 = 3.0;
const CARDINAL_TICK_HEIGHT: f32 = 13.0;
const LABEL_SIZE: f32 = 13.0;
const CARDINAL_LABEL_SIZE: f32 = 16.0;
const READING_SIZE: f32 = 15.0;
const LABEL_GAP: f32 = 2.0;
const READING_GAP: f32 = 3.0;

/// The strip's backdrop, dark enough to read a white tick against a bright sky.
const STRIP_BACKGROUND: Color = Color::srgba(0.025, 0.03, 0.04, 0.78);

/// Ordinary ticks and their labels.
const TICK: Color = Color::srgba(0.82, 0.84, 0.88, 0.85);

/// North, East, South and West, and the labels under them.
const CARDINAL: Color = Color::WHITE;

/// The fixed centre pointer and the reading under it.
///
/// The aiming outline's warm amber, the same colour `ui/status.rs` gives a notice: the two
/// pieces of interface that say "this, here" share it.
const POINTER: Color = Color::linear_rgb(1.0, 0.72, 0.25);

/// Above the world, below the panels. The layer the other permanent HUD pieces sit on.
const HUD_LAYER: i32 = 12;

/// Draws the heading strip and keeps it under the player's facing.
pub(super) struct CompassUiPlugin;

impl Plugin for CompassUiPlugin {
    fn build(&self, app: &mut App) {
        // `PlayerPlugin` owns both in the game. Initialising them here is what keeps this
        // module's headless contract complete when it is built on its own — the same
        // reasoning `CrosshairPlugin` states for `ViewMode`.
        app.init_resource::<InputMode>()
            .init_resource::<LookState>()
            // `PlayerPlugin` owns this one too, and for the same reason: the coordinates
            // under the reading are a readout of what the server said, so this module has
            // to be buildable on its own without the plugin that fills it in.
            .init_resource::<PlayerStats>()
            .init_resource::<Storm>()
            .add_systems(Startup, spawn_compass)
            .add_systems(
                Update,
                (
                    // After the camera is aimed, and that set is itself ordered after
                    // `sample_input` — which is the system that writes the yaw this reads.
                    // Without it the strip is free to run first and draw the facing of the
                    // frame before, which on a fast turn is a compass that trails the view
                    // it is supposed to describe. Ordering against a set no headless test
                    // builds is a no-op, so this costs those tests nothing.
                    refresh_compass.after(AimCamera),
                    // Unordered against `refresh_player_stats`, deliberately: the
                    // position it reads is the server's answer interpolated for display,
                    // so a frame of lag in a three-integer readout is invisible, and an
                    // ordering against a system this module's headless tests never build
                    // would be a no-op there anyway.
                    refresh_coordinates,
                    refresh_storm_countdown.after(super::storm::IngestStorm),
                    show_compass,
                ),
            );
    }
}

/// The strip, its pointer and its reading. Shown and hidden as one node.
#[derive(Component)]
struct CompassRoot;

/// The fixed-width window the marks slide behind.
#[derive(Component)]
struct CompassWindow;

/// One tick on the strip, at the bearing it names in degrees clockwise from North.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct CompassMark(u16);

/// The fixed centre pointer.
#[derive(Component)]
struct CompassPointer;

/// The numeric heading under the pointer.
#[derive(Component)]
struct CompassReading;

/// Where the player stands, in blocks, under the heading.
#[derive(Component)]
struct CoordinatesReading;

/// The server-anchored Fimbulvetr countdown under the coordinates.
#[derive(Component)]
struct StormCountdown;

/// The bearing the player faces, in degrees clockwise from North, in `[0, 360)`.
///
/// See the module comment for why this is the yaw negated. A yaw that is not finite
/// answers due North rather than propagating a `NaN` into a layout: `sample_input` keeps
/// the yaw finite and wrapped, so this is a guard rather than a case.
fn heading_degrees(yaw: f32) -> f32 {
    if !yaw.is_finite() {
        return 0.0;
    }
    let degrees = (-yaw).to_degrees().rem_euclid(360.0);
    // `rem_euclid` can answer exactly 360.0 for a value a hair below zero, once the
    // subtraction rounds. The strip does not care, but the reading would print `360°`.
    if degrees >= 360.0 { 0.0 } else { degrees }
}

/// How far `mark` sits from `heading`, in degrees, wrapped into `[-180, 180)`.
///
/// The wrap is the whole reason the strip crosses North without jumping between its ends:
/// the shorter way round is always the one taken, so a mark never travels the long way to
/// reach a place one degree away.
fn offset_degrees(mark: f32, heading: f32) -> f32 {
    (mark - heading + 180.0).rem_euclid(360.0) - 180.0
}

/// Where a mark's slot starts inside the window, in logical pixels from its left edge.
///
/// A pure function of the two bearings, so the placement is testable without a window —
/// which is what the continuity across North is actually asserted on.
fn mark_left(mark: u16, heading: f32) -> f32 {
    let offset = offset_degrees(f32::from(mark), heading);
    STRIP_WIDTH / 2.0 + offset * PIXELS_PER_DEGREE - MARK_SLOT / 2.0
}

/// The label a mark carries, or the empty string for a plain tick.
fn mark_label(degrees: u16) -> &'static str {
    match degrees {
        0 => "N",
        45 => "NE",
        90 => "E",
        135 => "SE",
        180 => "S",
        225 => "SW",
        270 => "W",
        315 => "NW",
        _ => "",
    }
}

/// Whether a mark is one of the four the acceptance criteria name.
fn is_cardinal(degrees: u16) -> bool {
    matches!(degrees, 0 | 90 | 180 | 270)
}

/// What the fixed centre indicator reads.
///
/// Degrees rather than a cardinal word, because the strip above it already spells the
/// cardinal out and this is the half that is still readable between two of them. Rounded
/// to a whole degree and taken modulo 360, so a heading a hair under North reads `000 deg`
/// rather than `360 deg`.
///
/// `deg` spelled out, because `°` is U+00B0 and Bevy's `default_font` is a 95-glyph ASCII
/// subset of FiraMono: the degree sign draws as nothing at all, and a reading that silently
/// loses its unit is worse than one that spends three more columns saying it.
fn center_reading(yaw: f32) -> String {
    let rounded = heading_degrees(yaw).round() as u16 % 360;
    format!("{rounded:03} deg")
}

/// The block the player is standing in, on the axis `value` measures.
///
/// `floor` and not a truncating cast, because the two disagree over exactly the half of
/// the world that is negative: block `-1` spans `-1.0..0.0`, so `-0.5` is in it, and a
/// cast toward zero would put it in block `0` — two adjacent blocks sharing a name, and
/// only west and south of the origin. A yaw that is not finite reads as due North a few
/// lines above, and this is the same guard for the same reason: `net/codec.rs` refuses a
/// non-finite coordinate before one can reach a `Transform`, so this is a guard rather
/// than a case.
fn block_coordinate(value: f32) -> i32 {
    if !value.is_finite() {
        return 0;
    }
    value.floor() as i32
}

/// What the coordinates line reads for a position.
///
/// Three integers in the order a player says them: the two that name a place on the map
/// first, and the altitude last. `alt` rather than `Y`, because the axis letters are the
/// world's and the word is the player's — and because the world map, when it arrives,
/// shows X and Z under its cursor and no third axis at all. Block coordinates and not
/// chunk coordinates: this is the number one player reads out to another.
///
/// **`pub(super)` so the map's side panel prints the same line from the same function.**
/// Two copies of this format string would be two lines that agree until one of them is
/// retouched, and the HUD and the map are read within a second of each other.
pub(super) fn coordinates_reading(position: Vec3) -> String {
    format!(
        "X {} | Z {} | alt {}",
        block_coordinate(position.x),
        block_coordinate(position.z),
        block_coordinate(position.y),
    )
}

fn spawn_compass(mut commands: Commands) {
    commands
        .spawn((
            CompassRoot,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(TOP),
                left: Val::Px(0.0),
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(READING_GAP),
                ..default()
            },
            // Every node this module spawns carries it, and the omission would be silent:
            // a node with no policy blocks, and this one is a full-width band across the
            // top of the screen. Anything under it — a menu, a panel, a button — would
            // stop answering the pointer with nothing on screen to explain why.
            FocusPolicy::Pass,
            Visibility::Hidden,
            GlobalZIndex(HUD_LAYER),
        ))
        .with_children(|root| {
            root.spawn((
                CompassWindow,
                Node {
                    width: Val::Px(STRIP_WIDTH),
                    height: Val::Px(STRIP_HEIGHT),
                    overflow: Overflow::clip(),
                    ..default()
                },
                BackgroundColor(STRIP_BACKGROUND),
                FocusPolicy::Pass,
            ))
            .with_children(|window| {
                for step in 0..MARK_COUNT {
                    let degrees = step * MARK_STEP_DEGREES;
                    let cardinal = is_cardinal(degrees);
                    window
                        .spawn((
                            CompassMark(degrees),
                            Node {
                                position_type: PositionType::Absolute,
                                top: Val::Px(0.0),
                                left: Val::Px(mark_left(degrees, 0.0)),
                                width: Val::Px(MARK_SLOT),
                                height: Val::Percent(100.0),
                                flex_direction: FlexDirection::Column,
                                align_items: AlignItems::Center,
                                ..default()
                            },
                            FocusPolicy::Pass,
                        ))
                        .with_children(|mark| {
                            mark.spawn((
                                Node {
                                    width: Val::Px(if cardinal {
                                        CARDINAL_TICK_WIDTH
                                    } else {
                                        TICK_WIDTH
                                    }),
                                    height: Val::Px(if cardinal {
                                        CARDINAL_TICK_HEIGHT
                                    } else {
                                        TICK_HEIGHT
                                    }),
                                    ..default()
                                },
                                BackgroundColor(if cardinal { CARDINAL } else { TICK }),
                                FocusPolicy::Pass,
                            ));
                            let label = mark_label(degrees);
                            if label.is_empty() {
                                return;
                            }
                            mark.spawn((
                                Node {
                                    margin: UiRect::top(Val::Px(LABEL_GAP)),
                                    ..default()
                                },
                                Text::new(label),
                                TextFont {
                                    font_size: FontSize::Px(if cardinal {
                                        CARDINAL_LABEL_SIZE
                                    } else {
                                        LABEL_SIZE
                                    }),
                                    ..default()
                                },
                                TextColor(if cardinal { CARDINAL } else { TICK }),
                                TextLayout::no_wrap(),
                                FocusPolicy::Pass,
                            ));
                        });
                }

                // Spawned last so it draws over the marks sliding under it.
                window.spawn((
                    CompassPointer,
                    Node {
                        position_type: PositionType::Absolute,
                        top: Val::Px(0.0),
                        left: Val::Px((STRIP_WIDTH - POINTER_WIDTH) / 2.0),
                        width: Val::Px(POINTER_WIDTH),
                        height: Val::Percent(100.0),
                        ..default()
                    },
                    BackgroundColor(POINTER),
                    FocusPolicy::Pass,
                ));
            });

            root.spawn((
                CompassReading,
                Text::new(center_reading(0.0)),
                TextFont {
                    font_size: FontSize::Px(READING_SIZE),
                    ..default()
                },
                TextColor(POINTER),
                TextLayout::no_wrap(),
                TextShadow::default(),
                FocusPolicy::Pass,
            ));

            // A third child of the same column rather than a second root: `TOP`, the
            // `HUD_LAYER` ordering, the one `Visibility` `show_compass` writes and the
            // pointer-pass guarantee are then stated once, and the column's `row_gap`
            // already puts this a `READING_GAP` under the heading.
            //
            // Empty, not a position. Nothing has said where this player is until the
            // first snapshot names them, and `0, 0, 0` would be an answer — the same
            // distinction `ui/status.rs` draws when it writes `player -`.
            root.spawn((
                CoordinatesReading,
                Text::default(),
                TextFont {
                    font_size: FontSize::Px(READING_SIZE),
                    ..default()
                },
                TextColor(POINTER),
                TextLayout::no_wrap(),
                TextShadow::default(),
                FocusPolicy::Pass,
            ));

            // The third Text child in this column, after the coordinates. Empty and
            // hidden until the latest server warning says the storm is within a minute
            // or raging; no local weather state is consulted.
            root.spawn((
                StormCountdown,
                Text::default(),
                TextFont {
                    font_size: FontSize::Px(READING_SIZE),
                    ..default()
                },
                TextColor(POINTER),
                TextLayout::no_wrap(),
                TextShadow::default(),
                FocusPolicy::Pass,
                Visibility::Hidden,
            ));
        });
}

/// Slides every mark to where the current heading puts it, and rewrites the reading.
///
/// Both writes are conditional. `ResMut`-style change detection on a `Node` costs a UI
/// relayout, and a compass that rewrote its twenty-four lefts on a frame the player did
/// not turn would pay for one every frame.
fn refresh_compass(
    look: Res<LookState>,
    mut marks: Query<(&CompassMark, &mut Node)>,
    mut readings: Query<&mut Text, With<CompassReading>>,
) {
    let heading = heading_degrees(look.yaw);
    for (mark, mut node) in &mut marks {
        let left = Val::Px(mark_left(mark.0, heading));
        if node.left != left {
            node.left = left;
        }
    }
    let reading = center_reading(look.yaw);
    for mut text in &mut readings {
        if text.0 != reading {
            text.0 = reading.clone();
        }
    }
}

/// Rewrites the coordinates line when the player crosses into another block.
///
/// The same discipline `refresh_compass` keeps, and here the saving is larger: the
/// position underneath is interpolated, so it moves every frame the player does, while
/// the three integers it floors to change a couple of times a second at a walk. Comparing
/// the rendered string is comparing the triple — it is a pure function of nothing else
/// — so one comparison serves for both.
fn refresh_coordinates(
    stats: Res<PlayerStats>,
    mut readings: Query<&mut Text, With<CoordinatesReading>>,
) {
    let next = stats.position.map(coordinates_reading).unwrap_or_default();
    for mut text in &mut readings {
        if text.0 != next {
            text.0 = next.clone();
        }
    }
}

/// Rewrites the countdown only when its displayed whole second changes.
///
/// `Storm` owns the last server warning and its receive instant. This system reads that
/// presentation state and nothing else: in particular, a blizzard weather snapshot does
/// not manufacture a countdown and a countdown does not manufacture weather.
fn refresh_storm_countdown(
    storm: Option<Res<Storm>>,
    mut readings: Query<(&mut Text, &mut Visibility), With<StormCountdown>>,
) {
    let next = storm.and_then(|storm| storm.countdown_at(std::time::Instant::now()));
    let visibility = if next.is_some() {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    let next = next.unwrap_or_default();
    for (mut text, mut shown) in &mut readings {
        if text.0 != next {
            text.0 = next.clone();
        }
        if *shown != visibility {
            *shown = visibility;
        }
    }
}

/// The compass is up exactly while there is a world to be facing something in.
///
/// The same gate the experience bar and the party rows use: a live session, and a mode in
/// which the player is looking at the world rather than at a panel. It is a second answer
/// to the same question `FocusPolicy::Pass` answers structurally — a hidden node with a
/// blocking policy would still eat the pointer, which is why the policy is the one that
/// matters and this is only about what is worth drawing.
fn show_compass(
    mode: Res<InputMode>,
    session: Option<Res<Session>>,
    mut roots: Query<&mut Visibility, With<CompassRoot>>,
) {
    let next = if session.is_some() && matches!(*mode, InputMode::Playing | InputMode::Chat) {
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

#[cfg(test)]
mod tests {
    use std::f32::consts::{FRAC_PI_2, PI};
    use std::time::Instant;

    use super::*;
    use crate::net::{SessionParams, StormPhase, StormWarning};

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 1,
            spawn: [0.0; 3],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 3,
            inventory_slots: 5,
            hotbar_slots: 4,
            equipment_slots: 1,
            player_token: crate::net::ANY_TOKEN,
        })
    }

    fn compass_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(session())
            .add_plugins(CompassUiPlugin);
        app.update();
        app
    }

    fn face(app: &mut App, yaw: f32) {
        app.world_mut().resource_mut::<LookState>().yaw = yaw;
        app.update();
    }

    fn reading(app: &mut App) -> String {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Text, With<CompassReading>>();
        query.single(world).expect("one reading").0.clone()
    }

    fn coordinates(app: &mut App) -> String {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Text, With<CoordinatesReading>>();
        query
            .single(world)
            .expect("one coordinates reading")
            .0
            .clone()
    }

    fn storm_countdown(app: &mut App) -> (String, Visibility) {
        let world = app.world_mut();
        let mut query = world.query_filtered::<(&Text, &Visibility), With<StormCountdown>>();
        let (text, visibility) = query.single(world).expect("one storm countdown");
        (text.0.clone(), *visibility)
    }

    fn stand(app: &mut App, position: Option<Vec3>) {
        app.world_mut().resource_mut::<PlayerStats>().position = position;
        app.update();
    }

    fn root_visibility(app: &mut App) -> Visibility {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Visibility, With<CompassRoot>>();
        *query.single(world).expect("one compass root")
    }

    fn left_of(app: &mut App, degrees: u16) -> f32 {
        let world = app.world_mut();
        let mut query = world.query::<(&CompassMark, &Node)>();
        let (_, node) = query
            .iter(world)
            .find(|(mark, _)| mark.0 == degrees)
            .unwrap_or_else(|| panic!("a mark at {degrees}°"));
        match node.left {
            Val::Px(px) => px,
            other => panic!("mark {degrees}° is placed as {other:?}"),
        }
    }

    #[test]
    fn the_four_cardinal_yaws_map_to_their_bearings() {
        // The convention the whole module rests on: yaw 0 looks along -Z, which is North,
        // and a positive yaw turns west. The server's movement tests say the same thing
        // from the other side — `yawEast = -π/2`.
        for (yaw, bearing, name) in [
            (0.0, 0.0, "north"),
            (-FRAC_PI_2, 90.0, "east"),
            (PI, 180.0, "south"),
            (FRAC_PI_2, 270.0, "west"),
        ] {
            let actual = heading_degrees(yaw);
            assert!(
                (actual - bearing).abs() < 0.01,
                "{name}: yaw {yaw} gave {actual}, expected {bearing}"
            );
        }
        // -π is the other spelling of the same facing, and `LookState` is wrapped into
        // (-π, π] so both are reachable through the wrap.
        assert!((heading_degrees(-PI) - 180.0).abs() < 0.01);
    }

    #[test]
    fn a_yaw_outside_one_turn_is_brought_back_into_the_circle() {
        // Not reachable through `sample_input`, which wraps, and answered anyway: a
        // bearing outside [0, 360) would put every mark on the strip at a place no
        // wrapped heading ever puts it.
        use std::f32::consts::TAU;
        assert!((heading_degrees(-FRAC_PI_2 - TAU) - 90.0).abs() < 0.01);
        assert!((heading_degrees(FRAC_PI_2 + TAU) - 270.0).abs() < 0.01);
        assert_eq!(heading_degrees(f32::NAN), 0.0);
        assert_eq!(heading_degrees(f32::INFINITY), 0.0);
    }

    #[test]
    fn north_stays_a_step_away_on_both_sides_of_the_wrap() {
        // Just below and just above the boundary. The wrap is what makes these two
        // half-degree offsets instead of one of them being 359.5.
        // A positive yaw turns west, so it is the one that lands just under 360.
        let below = heading_degrees(0.5_f32.to_radians());
        let above = heading_degrees(-0.5_f32.to_radians());
        assert!(
            below > 359.0,
            "expected a heading just under 360, got {below}"
        );
        assert!(above < 1.0, "expected a heading just over 0, got {above}");

        let from_below = offset_degrees(0.0, below);
        let from_above = offset_degrees(0.0, above);
        assert!(
            from_below.abs() < 1.0 && from_above.abs() < 1.0,
            "North jumped across the wrap: {from_below} / {from_above}"
        );
        // Opposite sides of the pointer, which is what "scrolls through" means.
        assert!(from_below > 0.0 && from_above < 0.0);
    }

    #[test]
    fn every_mark_takes_the_short_way_round() {
        for mark in (0u16..360).step_by(usize::from(MARK_STEP_DEGREES)) {
            for heading in 0u16..360 {
                let offset = offset_degrees(f32::from(mark), f32::from(heading));
                assert!(
                    (-180.0..180.0).contains(&offset),
                    "mark {mark}° at heading {heading}° offset by {offset}"
                );
            }
        }
    }

    #[test]
    fn the_strip_slides_continuously_through_north() {
        // Swept a tenth of a degree at a time from just west of North to just east of it.
        // A strip that jumped between its ends would show up here as one step of several
        // hundred pixels; the honest step is a tenth of a degree of travel.
        let step = 0.1_f32;
        let tolerance = step * PIXELS_PER_DEGREE * 1.5;
        let mut previous = mark_left(0, 355.0);
        let mut heading = 355.0_f32;
        while heading < 365.0 {
            heading += step;
            let wrapped = heading % 360.0;
            let left = mark_left(0, wrapped);
            let travelled = (left - previous).abs();
            assert!(
                travelled <= tolerance,
                "North jumped {travelled}px at heading {wrapped}"
            );
            previous = left;
        }
    }

    #[test]
    fn the_mark_matching_the_heading_sits_under_the_pointer() {
        let centre = (STRIP_WIDTH - POINTER_WIDTH) / 2.0;
        for (yaw, degrees) in [
            (0.0, 0u16),
            (-FRAC_PI_2, 90),
            (PI, 180),
            (FRAC_PI_2, 270),
            (-FRAC_PI_2 / 2.0, 45),
        ] {
            let mut app = compass_app();
            face(&mut app, yaw);
            // The slot is `MARK_SLOT` wide and its tick is centred in it, so the slot's
            // left edge sits half a slot left of the pointer.
            let expected = centre + POINTER_WIDTH / 2.0 - MARK_SLOT / 2.0;
            let actual = left_of(&mut app, degrees);
            assert!(
                (actual - expected).abs() < 0.5,
                "{degrees}° sat at {actual}, expected {expected}"
            );
        }
    }

    #[test]
    fn the_four_cardinals_are_labelled_and_the_intercardinals_with_them() {
        assert_eq!(mark_label(0), "N");
        assert_eq!(mark_label(90), "E");
        assert_eq!(mark_label(180), "S");
        assert_eq!(mark_label(270), "W");
        assert_eq!(mark_label(45), "NE");
        assert_eq!(mark_label(135), "SE");
        assert_eq!(mark_label(225), "SW");
        assert_eq!(mark_label(315), "NW");
        assert_eq!(mark_label(15), "");

        // Every labelled bearing has to land on a mark, or the label is written on a tick
        // that does not exist.
        for labelled in [0u16, 45, 90, 135, 180, 225, 270, 315] {
            assert_eq!(
                labelled % MARK_STEP_DEGREES,
                0,
                "{labelled}° is between marks"
            );
        }

        let mut app = compass_app();
        let world = app.world_mut();
        let mut query = world.query::<(&CompassMark, &Node)>();
        let mut present: Vec<u16> = query.iter(world).map(|(mark, _)| mark.0).collect();
        present.sort_unstable();
        assert_eq!(present.len(), usize::from(MARK_COUNT));
        for labelled in [0u16, 45, 90, 135, 180, 225, 270, 315] {
            assert!(present.contains(&labelled), "no mark at {labelled}°");
        }
    }

    #[test]
    fn the_centre_reading_is_the_heading_the_look_state_names() {
        let mut app = compass_app();
        for (yaw, expected) in [
            (0.0, "000 deg"),
            (-FRAC_PI_2, "090 deg"),
            (PI, "180 deg"),
            (FRAC_PI_2, "270 deg"),
        ] {
            face(&mut app, yaw);
            assert_eq!(reading(&mut app), expected, "yaw {yaw}");
        }
        // A hair west of North reads as North rather than as a full turn.
        face(&mut app, 0.001);
        assert_eq!(reading(&mut app), "000 deg");
    }

    #[test]
    fn the_compass_never_blocks_the_pointer() {
        // Every node, not only the root: `FocusPolicy` is per-node, and a labelled tick
        // with a blocking policy would take the pointer off whatever is under the strip.
        let mut app = compass_app();
        let world = app.world_mut();
        let mut query = world.query::<(Entity, &Node, Option<&FocusPolicy>)>();
        let found: Vec<(Entity, Option<FocusPolicy>)> = query
            .iter(world)
            .map(|(entity, _, policy)| (entity, policy.copied()))
            .collect();
        assert!(!found.is_empty(), "the compass spawned no nodes");
        for &(entity, policy) in &found {
            assert_eq!(
                policy,
                Some(FocusPolicy::Pass),
                "{entity} blocks the pointer"
            );
        }

        // The walk above is over `&Node`, so a new text child is covered by it only while
        // `Text` still brings a `Node` with it. Naming the coordinates node here is what
        // turns that structural fact into an assertion rather than a silently narrower
        // sweep: drop the node and this fails, instead of the walk quietly skipping it.
        let world = app.world_mut();
        let mut named = world.query_filtered::<Entity, With<CoordinatesReading>>();
        let coordinates = named.single(world).expect("one coordinates reading");
        assert!(
            found.iter().any(|&(entity, _)| entity == coordinates),
            "the coordinates reading was not among the nodes walked"
        );

        let world = app.world_mut();
        let mut named = world.query_filtered::<Entity, With<StormCountdown>>();
        let countdown = named.single(world).expect("one storm countdown");
        assert!(
            found.iter().any(|&(entity, _)| entity == countdown),
            "the storm countdown was not among the nodes walked"
        );
    }

    #[test]
    fn the_storm_line_is_hidden_until_the_server_statement_has_a_countdown() {
        let mut app = compass_app();
        assert_eq!(
            storm_countdown(&mut app),
            (String::new(), Visibility::Hidden)
        );

        app.world_mut().resource_mut::<Storm>().receive(
            StormWarning {
                phase: StormPhase::Raging,
                seconds_until: 299,
            },
            Instant::now(),
        );
        app.update();
        assert_eq!(
            storm_countdown(&mut app),
            ("Fimbulvetr | 4:59".to_owned(), Visibility::Visible)
        );

        app.world_mut().resource_mut::<Storm>().receive(
            StormWarning {
                phase: StormPhase::Passed,
                seconds_until: 0,
            },
            Instant::now(),
        );
        app.update();
        assert_eq!(
            storm_countdown(&mut app),
            (String::new(), Visibility::Hidden)
        );
    }

    #[test]
    fn the_compass_is_up_while_playing_and_down_over_a_panel() {
        let mut app = compass_app();
        assert_eq!(root_visibility(&mut app), Visibility::Visible);

        for mode in [InputMode::Inventory, InputMode::Loot, InputMode::Menu] {
            *app.world_mut().resource_mut::<InputMode>() = mode;
            app.update();
            assert_eq!(
                root_visibility(&mut app),
                Visibility::Hidden,
                "mode {mode:?}"
            );
        }

        // Typing is still looking at the world, exactly as the experience bar and the
        // party rows read it.
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Chat;
        app.update();
        assert_eq!(root_visibility(&mut app), Visibility::Visible);
    }

    #[test]
    fn a_position_reads_as_the_block_it_is_standing_in() {
        assert_eq!(
            coordinates_reading(Vec3::new(0.0, 64.9, -0.5)),
            "X 0 | Z -1 | alt 64"
        );
        // Whole numbers, positive on every axis.
        assert_eq!(
            coordinates_reading(Vec3::new(123.0, 67.0, 45.0)),
            "X 123 | Z 45 | alt 67"
        );
        // The one the issue writes out, and the one that says `alt` is the Y axis and Z
        // is the middle field: a reading that transposed them would pass every symmetric
        // case above and fail here.
        assert_eq!(
            coordinates_reading(Vec3::new(123.4, 67.8, -45.6)),
            "X 123 | Z -46 | alt 67"
        );
        // Far out and far down, with no zero padding and no thousands separator.
        assert_eq!(
            coordinates_reading(Vec3::new(-4096.0, -12.25, 8192.75)),
            "X -4096 | Z 8192 | alt -13"
        );
        // `floor`, not a cast toward zero: every one of these is inside block -1, and a
        // truncating cast would name three of them 0.
        for value in [-0.001_f32, -0.5, -0.999] {
            assert_eq!(block_coordinate(value), -1, "{value} left block -1");
        }
        assert_eq!(block_coordinate(-1.0), -1);
        assert_eq!(block_coordinate(0.0), 0);
        // The guard `heading_degrees` keeps, for the same reason and against the same
        // unreachable input: a saturating cast would otherwise print 2147483647 blocks.
        assert_eq!(block_coordinate(f32::NAN), 0);
        assert_eq!(block_coordinate(f32::INFINITY), 0);
        assert_eq!(block_coordinate(f32::NEG_INFINITY), 0);
    }

    #[test]
    fn the_coordinates_are_empty_until_the_server_has_placed_the_player() {
        let mut app = compass_app();
        // `PlayerStats::position` is `None` before the first snapshot names this session's
        // own entity, and an empty line is the only honest reading of that. `X 0 | Z 0 |
        // alt 0` would be a place, and it is the one place a player might actually be.
        assert_eq!(coordinates(&mut app), "");

        stand(&mut app, Some(Vec3::new(123.4, 67.8, -45.6)));
        assert_eq!(coordinates(&mut app), "X 123 | Z -46 | alt 67");

        // And back: a session that ends takes the position with it.
        stand(&mut app, None);
        assert_eq!(coordinates(&mut app), "");
    }

    #[test]
    fn moving_inside_one_block_does_not_rewrite_the_line() {
        // Observed from inside a system, because `App::update` ends with `clear_trackers`
        // and a check from outside is always false. A system's first run compares against
        // a last-run tick of zero, so everything in the world reads as changed to it —
        // the probe's first frame is therefore spent, and the assertion that matters is
        // the one on the frame after.
        #[derive(Resource, Default)]
        struct Rewritten(bool);

        let mut app = compass_app();
        app.init_resource::<Rewritten>();
        app.add_systems(
            Update,
            (|texts: Query<Ref<'_, Text>, With<CoordinatesReading>>,
              mut seen: ResMut<Rewritten>| {
                seen.0 = texts.iter().any(|text| text.is_changed());
            })
            .after(refresh_coordinates),
        );

        // The probe's spent frame, and the one that puts the player somewhere. Only the
        // text is asserted here: the change flag on this frame is the tick-zero artefact
        // above, not a measurement.
        stand(&mut app, Some(Vec3::new(12.1, 64.1, -3.9)));
        assert_eq!(coordinates(&mut app), "X 12 | Z -4 | alt 64");

        // A stride inside the same block. The interpolated position moves every frame the
        // player does; the three integers it floors to do not, and neither may the text.
        stand(&mut app, Some(Vec3::new(12.9, 64.9, -3.1)));
        assert!(
            !app.world().resource::<Rewritten>().0,
            "a step inside one block rewrote the coordinates"
        );
        assert_eq!(coordinates(&mut app), "X 12 | Z -4 | alt 64");

        // One block east, and the line moves again — the check above is not simply
        // asserting that nothing ever writes.
        stand(&mut app, Some(Vec3::new(13.0, 64.9, -3.1)));
        assert!(
            app.world().resource::<Rewritten>().0,
            "crossing into the next block left the coordinates behind"
        );
        assert_eq!(coordinates(&mut app), "X 13 | Z -4 | alt 64");
    }

    #[test]
    fn the_coordinates_are_hidden_and_shown_with_the_compass() {
        // Not a second gate: the reading inherits, so `show_compass` writing the root is
        // the whole mechanism. What is asserted is that it is still a child of that root
        // and still inherits — a `Visibility::Visible` on it would draw over a menu,
        // unconditionally, which is the trap the module comment already names for marks.
        let mut app = compass_app();
        let world = app.world_mut();
        let mut roots = world.query_filtered::<Entity, With<CompassRoot>>();
        let root = roots.single(world).expect("one compass root");

        let mut query = world.query_filtered::<(&ChildOf, &Visibility), With<CoordinatesReading>>();
        let (parent, visibility) = query.single(world).expect("one coordinates reading");
        assert_eq!(parent.parent(), root, "the reading hangs off another node");
        assert_eq!(*visibility, Visibility::Inherited);

        // And the root does go down over a panel and with no session, which is what it
        // then inherits. The two existing tests below say the same for the compass; this
        // one is here so the coordinates are named in it.
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Inventory;
        app.update();
        assert_eq!(root_visibility(&mut app), Visibility::Hidden);
    }

    #[test]
    fn no_session_means_no_compass() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(CompassUiPlugin);
        app.update();
        assert_eq!(root_visibility(&mut app), Visibility::Hidden);
    }
}
