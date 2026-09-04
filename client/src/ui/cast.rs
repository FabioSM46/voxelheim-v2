//! The cast bar: the fourth vital bar, stacked above health.
//!
//! **Nothing here decides anything.** The fill, the visibility and the reading are all
//! [`SnapshotBuffer::self_cast`] — the server's progress byte, drawn as it arrives. There
//! is no local timer, no interpolation and no prediction of when a cast starts, advances
//! or ends: absence of a cast in the newest snapshot is absence of the bar.
//!
//! This module joins the bar vocabulary [`super::health`] exports rather than keeping a
//! second set of numbers that happens to agree with it: [`vital_bar_root`],
//! [`vital_bar_track`], [`vital_bar_label`] and [`vital_bar_label_transform`] give this bar
//! the same width, height, border, corner radius and horizontal centring as health, hunger
//! and experience by construction. It is the top of the stack —
//! [`CAST_BAR_BOTTOM`] sits one bar and the shared gap above [`HEALTH_BAR_BOTTOM`] — which
//! is what lets it appear and disappear without moving anything below it: every bar is
//! positioned by its own absolute `bottom`, so the three vitals never read this module at
//! all.

use bevy::prelude::*;

use super::CELL_EDGE;
use super::health::{
    BAR_HEIGHT, BAR_LABEL_SIZE, DEFAULT_FONT_ADVANCE_EM, HEALTH_BAR_BOTTOM, TRACK_INNER_WIDTH,
    VITAL_BAR_GAP, vital_bar_label, vital_bar_label_transform, vital_bar_root, vital_bar_track,
};
use crate::net::{CastKind, Session};
use crate::player::SnapshotBuffer;

/// Distance from the bottom of the window to the cast bar: the top of the stack, one bar
/// and the shared gap above health. Written in terms of [`HEALTH_BAR_BOTTOM`] and not as a
/// literal, so a change to any bar beneath it moves this one with no edit here.
const CAST_BAR_BOTTOM: f32 = HEALTH_BAR_BOTTOM + BAR_HEIGHT + VITAL_BAR_GAP;

/// The layer the other three vital bars draw on (`ui/health.rs`, `ui/hunger.rs`,
/// `ui/experience.rs`), and not a lane of its own: this is the fourth bar in the same
/// stack, not a separate overlay.
const CAST_LAYER: i32 = 12;

/// The empty part of the track, shared visually with the other three vital bars.
const BAR_TRACK: Color = Color::srgba(0.055, 0.065, 0.080, 0.94);

/// What a running cast is drawn in — distinct from health's red, hunger's amber and
/// experience's blue.
const BAR_FILL: Color = Color::srgb(0.83, 0.62, 0.22);

/// The widest percentage suffix the reading ever draws: two spaces, three digits and a
/// percent sign. `u16::from(progress) * 100 / 255` maxes at `100` when `progress` is the
/// wire's own ceiling of `255`, so no cast kind's reading can draw a wider suffix than this.
const CAST_PERCENT_SUFFIX_CHARS: usize = "  100%".len();

/// Every [`CastKind`] this bar draws a label for, so the fit bound below folds over the
/// whole contract rather than the one member on the wire today — the same fold
/// `player::structures::ALL_STRUCTURE_KINDS` uses. [`cast_label`]'s match is exhaustive
/// with no wildcard arm, so a member missing here costs an under-measured bound rather
/// than a silent gap: nothing enforces that this list is complete, but nothing can add a
/// member to [`CastKind`] without a label either, which is where a reviewer notices.
const ALL_CAST_KINDS: [CastKind; 1] = [CastKind::Mount];

/// The longest label any [`CastKind`] can draw, folded over [`ALL_CAST_KINDS`].
const fn longest_cast_label_chars() -> usize {
    let mut widest = 0;
    let mut index = 0;
    while index < ALL_CAST_KINDS.len() {
        let len = cast_label(ALL_CAST_KINDS[index]).len();
        if len > widest {
            widest = len;
        }
        index += 1;
    }
    widest
}

/// The longest reading this bar can be asked to draw, in characters: the widest label plus
/// the percentage suffix above.
const LONGEST_CAST_READING_CHARS: f32 =
    (longest_cast_label_chars() + CAST_PERCENT_SUFFIX_CHARS) as f32;

/// The reading is drawn inside the track on one line, exactly like the other three vital
/// bars, so this is a build-time bound rather than something a screenshot would have to
/// catch: a label that outgrew the track would wrap, clip or spill over the world with
/// nothing at runtime saying so.
const _: () = assert!(
    LONGEST_CAST_READING_CHARS * DEFAULT_FONT_ADVANCE_EM * BAR_LABEL_SIZE <= TRACK_INNER_WIDTH,
    "the longest cast reading must fit across the track - widen BAR_WIDTH"
);

/// The bar and everything inside it. Hidden by `Display::None` on the node itself rather
/// than a `Visibility` component: this bar's very existence on screen — not merely its
/// paint — is a projection of whether a cast is running, and every frame answers that
/// question fresh from the newest snapshot.
#[derive(Component)]
struct CastRoot;

#[derive(Component)]
struct CastTrack;

/// The filled part. Its width is exactly the server's `progress / 255`.
#[derive(Component)]
struct CastFill;

/// The reading drawn inside the track.
#[derive(Component)]
struct CastLabel;

pub(super) struct CastUiPlugin;

impl Plugin for CastUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SnapshotBuffer>()
            .add_systems(Startup, spawn_cast_ui)
            .add_systems(Update, refresh_cast);
    }
}

fn spawn_cast_ui(mut commands: Commands) {
    let mut root = vital_bar_root(CAST_BAR_BOTTOM);
    // Absent a cast this bar draws nothing at all, not merely nothing visible — `display`
    // itself starts at `None` rather than the `Flex` `vital_bar_root` hands back, and
    // `refresh_cast` is what turns it into `Flex` on the frame a cast begins.
    root.display = Display::None;

    commands
        .spawn((CastRoot, root, GlobalZIndex(CAST_LAYER)))
        .with_children(|root| {
            root.spawn((
                CastTrack,
                vital_bar_track(),
                BackgroundColor(BAR_TRACK),
                BorderColor::all(CELL_EDGE),
            ))
            .with_children(|track| {
                track.spawn((
                    CastFill,
                    Node {
                        // Zero until the server names a running cast. A bar that started
                        // part-filled would be this client asserting progress nobody sent.
                        width: Val::Percent(0.0),
                        height: Val::Percent(100.0),
                        ..default()
                    },
                    BackgroundColor(BAR_FILL),
                ));
                track.spawn((
                    CastLabel,
                    vital_bar_label(),
                    vital_bar_label_transform(),
                    Text::new(String::new()),
                    TextFont {
                        font_size: FontSize::Px(BAR_LABEL_SIZE),
                        ..default()
                    },
                    TextColor(Color::WHITE),
                    TextLayout::no_wrap().with_justify(Justify::Center),
                    TextShadow::default(),
                ));
            });
        });
}

/// Draws the newest authoritative cast, or its absence.
///
/// `session.and_then(...)` is the server-authoritative gate: a stale buffer surviving past
/// the end of a session must not keep drawing a cast nobody is running.
fn refresh_cast(
    snapshots: Res<SnapshotBuffer>,
    session: Option<Res<Session>>,
    mut roots: Query<&mut Node, With<CastRoot>>,
    mut fills: Query<&mut Node, (With<CastFill>, Without<CastRoot>)>,
    mut labels: Query<&mut Text, With<CastLabel>>,
) {
    let state = session.and_then(|_| snapshots.self_cast());

    let display = if state.is_some() {
        Display::Flex
    } else {
        Display::None
    };
    for mut root in &mut roots {
        if root.display != display {
            root.display = display;
        }
    }

    let Some(state) = state else {
        return;
    };

    let width = Val::Percent(fill_percent(state.progress));
    for mut fill in &mut fills {
        if fill.width != width {
            fill.width = width;
        }
    }

    let reading = cast_reading(state.kind, state.progress);
    for mut text in &mut labels {
        if text.0 != reading {
            text.0.clone_from(&reading);
        }
    }
}

/// How much of the bar the server's progress fills, as a percentage of its width.
fn fill_percent(progress: u8) -> f32 {
    f32::from(progress) * 100.0 / 255.0
}

/// The name of one cast kind, with no percentage.
///
/// Exhaustive with no wildcard arm: a member cannot be added to [`CastKind`] without this
/// failing to compile until it is given a label, and [`ALL_CAST_KINDS`] is what the fit
/// bound above must also grow to keep measuring the true worst case.
const fn cast_label(kind: CastKind) -> &'static str {
    match kind {
        CastKind::Mount => "Calling mount",
    }
}

/// The complete reading this bar draws inside its track: the label plus the percentage the
/// server's progress byte answers to. Not `const`, since formatting allocates.
fn cast_reading(kind: CastKind, progress: u8) -> String {
    format!("{}  {}%", cast_label(kind), u16::from(progress) * 100 / 255)
}

#[cfg(test)]
mod tests {
    //! No window, no display and no GPU — `MinimalPlugins` and this plugin are the whole
    //! app, exactly as `ui/health.rs`'s tests are built.

    use std::time::Instant;

    use super::*;
    use crate::net::{ANY_TOKEN, CastState, SessionParams, Snapshot};
    use crate::ui::health::{
        BAR_BORDER, BAR_CORNER_RADIUS, BAR_WIDTH, EXPERIENCE_BAR_BOTTOM, HUNGER_BAR_BOTTOM,
    };

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 1,
            spawn: [0.0; 3],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 3,
            inventory_slots: 37,
            hotbar_slots: 9,
            equipment_slots: 4,
            player_token: ANY_TOKEN,
            voice_range_blocks: 0.0,
        })
    }

    fn hud() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(session())
            .add_plugins(CastUiPlugin);
        app.update();
        app
    }

    /// Delivers a snapshot carrying (or omitting) a running cast, exactly as an accepted
    /// server snapshot does. `tick` must strictly increase across calls in one test:
    /// `SnapshotBuffer::accept` refuses a tick that is not newer than the one it holds.
    fn deliver_cast(app: &mut App, tick: u32, cast: Option<CastState>) {
        app.world_mut().resource_mut::<SnapshotBuffer>().accept(
            Snapshot {
                server_tick: tick,
                self_cast: cast,
                ..Default::default()
            },
            Instant::now(),
        );
        app.update();
    }

    fn node<T: Component>(app: &mut App) -> Node {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Node, With<T>>();
        query.single(world).expect("one matching node").clone()
    }

    fn root_display(app: &mut App) -> Display {
        node::<CastRoot>(app).display
    }

    fn fill_width(app: &mut App) -> Val {
        node::<CastFill>(app).width
    }

    fn label(app: &mut App) -> String {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Text, With<CastLabel>>();
        query.single(world).expect("one cast label").0.clone()
    }

    #[test]
    fn the_root_is_hidden_absent_a_cast_and_shown_while_one_runs_then_hidden_again() {
        let mut app = hud();
        assert_eq!(root_display(&mut app), Display::None);

        deliver_cast(
            &mut app,
            1,
            Some(CastState {
                kind: CastKind::Mount,
                progress: 40,
            }),
        );
        assert_eq!(root_display(&mut app), Display::Flex);

        deliver_cast(&mut app, 2, None);
        assert_eq!(root_display(&mut app), Display::None);
    }

    #[test]
    fn a_cast_never_shows_without_a_session() {
        let mut app = hud();
        deliver_cast(
            &mut app,
            1,
            Some(CastState {
                kind: CastKind::Mount,
                progress: 10,
            }),
        );
        assert_eq!(root_display(&mut app), Display::Flex);

        app.world_mut().remove_resource::<Session>();
        app.update();
        assert_eq!(root_display(&mut app), Display::None);
    }

    #[test]
    fn the_fill_and_reading_are_exactly_the_servers_progress() {
        let mut app = hud();
        deliver_cast(
            &mut app,
            1,
            Some(CastState {
                kind: CastKind::Mount,
                progress: 128,
            }),
        );
        assert_eq!(fill_width(&mut app), Val::Percent(fill_percent(128)));
        assert_eq!(
            label(&mut app),
            format!("Calling mount  {}%", 128u16 * 100 / 255)
        );

        deliver_cast(
            &mut app,
            2,
            Some(CastState {
                kind: CastKind::Mount,
                progress: 255,
            }),
        );
        assert_eq!(fill_width(&mut app), Val::Percent(100.0));
        assert_eq!(label(&mut app), "Calling mount  100%");
    }

    #[test]
    fn the_track_is_the_shared_vital_geometry_and_the_reading_lives_inside_it() {
        let mut app = hud();
        let track = node::<CastTrack>(&mut app);
        assert_eq!(track.width, Val::Px(BAR_WIDTH));
        assert_eq!(track.height, Val::Px(BAR_HEIGHT));
        assert_eq!(track.border, UiRect::all(Val::Px(BAR_BORDER)));
        assert_eq!(
            track.border_radius,
            BorderRadius::all(Val::Px(BAR_CORNER_RADIUS))
        );
        assert_eq!(track, vital_bar_track());

        let root = node::<CastRoot>(&mut app);
        assert_eq!(root.left, Val::Px(0.0));
        assert_eq!(root.right, Val::Px(0.0));
        assert_eq!(root.align_items, AlignItems::Center);
        assert_eq!(root.justify_content, JustifyContent::Center);
        assert_eq!(root.bottom, Val::Px(CAST_BAR_BOTTOM));

        let label_node = node::<CastLabel>(&mut app);
        assert_eq!(label_node, vital_bar_label());

        // The reading is a child of the track, drawn last so it sits over the fill —
        // inside the track rather than a separate line above it.
        let world = app.world_mut();
        let mut tracks = world.query_filtered::<Entity, With<CastTrack>>();
        let track_entity = tracks.single(world).expect("one cast track");
        let mut labels = world.query_filtered::<Entity, With<CastLabel>>();
        let label_entity = labels.single(world).expect("one cast label");
        let parent = world
            .get::<ChildOf>(label_entity)
            .expect("the cast reading is parented to its track");
        assert_eq!(parent.parent(), track_entity);
        let children = world
            .get::<Children>(track_entity)
            .expect("the cast track has children");
        assert_eq!(children.last(), Some(&label_entity));
    }

    #[test]
    fn the_cast_bar_sits_above_health_and_the_three_vitals_are_unmoved() {
        assert_eq!(
            HUNGER_BAR_BOTTOM,
            EXPERIENCE_BAR_BOTTOM + BAR_HEIGHT + VITAL_BAR_GAP
        );
        assert_eq!(
            HEALTH_BAR_BOTTOM,
            HUNGER_BAR_BOTTOM + BAR_HEIGHT + VITAL_BAR_GAP
        );
        assert_eq!(
            CAST_BAR_BOTTOM,
            HEALTH_BAR_BOTTOM + BAR_HEIGHT + VITAL_BAR_GAP
        );
    }

    #[test]
    fn every_cast_kind_fits_its_track_at_full_progress() {
        for kind in ALL_CAST_KINDS {
            let reading = cast_reading(kind, u8::MAX);
            let width = reading.chars().count() as f32 * DEFAULT_FONT_ADVANCE_EM * BAR_LABEL_SIZE;
            assert!(
                width <= TRACK_INNER_WIDTH,
                "{reading:?} does not fit the track at {BAR_LABEL_SIZE}px"
            );
        }
    }
}
