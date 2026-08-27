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
use crate::player::{AimCamera, InputMode, LookState};

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
/// to a whole degree and taken modulo 360, so a heading a hair under North reads `000°`
/// rather than `360°`.
fn center_reading(yaw: f32) -> String {
    let rounded = heading_degrees(yaw).round() as u16 % 360;
    format!("{rounded:03}°")
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

    use super::*;
    use crate::net::SessionParams;

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
            (0.0, "000°"),
            (-FRAC_PI_2, "090°"),
            (PI, "180°"),
            (FRAC_PI_2, "270°"),
        ] {
            face(&mut app, yaw);
            assert_eq!(reading(&mut app), expected, "yaw {yaw}");
        }
        // A hair west of North reads as North rather than as a full turn.
        face(&mut app, 0.001);
        assert_eq!(reading(&mut app), "000°");
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
        for (entity, policy) in found {
            assert_eq!(
                policy,
                Some(FocusPolicy::Pass),
                "{entity} blocks the pointer"
            );
        }
    }

    #[test]
    fn the_compass_is_up_while_playing_and_down_over_a_panel() {
        let mut app = compass_app();
        assert_eq!(root_visibility(&mut app), Visibility::Visible);

        for mode in [InputMode::Inventory, InputMode::Menu] {
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
    fn no_session_means_no_compass() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(CompassUiPlugin);
        app.update();
        assert_eq!(root_visibility(&mut app), Visibility::Hidden);
    }
}
