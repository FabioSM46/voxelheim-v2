//! Over-head health bars: red over what fights, yellow over what can be hit and does not
//! fight back, green over the other players.
//!
//! **Presentation of numbers the server already sent, and nothing else.** A creature's
//! health is `MobState.health`, a player's is `EntityState.health`; the colour is a
//! client-side column of `player/mobs.rs`'s registry ([`hostility`]), and which colours are
//! drawn is the Interface tab's [`HealthBars`]. Nothing here is sent, and hiding a bar changes
//! nothing about who may be hit.
//!
//! Each bar is placed exactly as a name plate is: its anchor is projected with
//! `world_to_viewport` from the interpolated snapshot position every frame, judged against
//! the plate's reach and voxel line of sight through the same [`PlateSight`] hysteresis, and
//! hidden when the projection fails. A bar exists exactly while the newest snapshot names its
//! owner, so it is gone the frame the owner leaves it.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use bevy::prelude::*;
use bevy::ui::FocusPolicy;

use super::health::{BAR_BORDER, BAR_CORNER_RADIUS, BAR_FILL, BAR_HEIGHT, BAR_TRACK, BAR_WIDTH};
use crate::net::{BlockCoord, MobAction, MobKind, Session};
use crate::player::encounters::EncounterPresentation;
use crate::player::{
    AimCamera, ApplySnapshots, Hostility, NAME_PLATE_HEIGHT, PlateSight, SnapshotBuffer,
    WorldCamera, hostility, mob_overhead_anchor, player_overhead_anchor, step_plate_sight,
};
use crate::settings::{HealthBars, Settings};
use crate::world::ChunkStore;

/// How much of the HUD's vital bar an over-head bar is: a quarter, the same proportions at a
/// size that sits over a head without covering the next one.
const SCALE: f32 = 0.25;
const WIDTH: f32 = BAR_WIDTH * SCALE;
const HEIGHT: f32 = BAR_HEIGHT * SCALE;
const RADIUS: f32 = BAR_CORNER_RADIUS * SCALE;

/// The edge, rounded up to one logical pixel: a quarter of the HUD's two would be a
/// half-pixel line the renderer draws as nothing at all.
const BORDER: f32 = 1.0;
const _: () = assert!(
    BORDER >= BAR_BORDER * SCALE,
    "the edge is thinner than the HUD's"
);
const _: () = assert!(
    2.0 * BORDER < HEIGHT,
    "the edge leaves no room for the fill"
);

/// How far above a player's name plate their bar sits, in logical pixels.
const PLATE_CLEARANCE: f32 = 3.0;

/// The name plates' layer: a bar is the same kind of label over the world as a plate is.
const LAYER: i32 = 8;

const EDGE: Color = Color::srgba(0.0, 0.0, 0.0, 0.85);
/// Red: the HUD's own health colour, so an enemy's bar reads as the same quantity.
const HOSTILE_FILL: Color = BAR_FILL;
const PASSIVE_FILL: Color = Color::srgb(0.86, 0.70, 0.14);
const FRIEND_FILL: Color = Color::srgb(0.26, 0.70, 0.30);

pub(super) struct OverheadUiPlugin;

impl Plugin for OverheadUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<OverheadBars>().add_systems(
            Update,
            (
                collect_overhead_bars,
                sync_overhead_bars,
                position_overhead_bars,
            )
                .chain()
                // After the snapshot this frame applied, and after the camera owns this
                // frame's eye — the ordering the name plates use. Ordering against an empty
                // set is a no-op, which keeps this module testable on its own.
                .after(ApplySnapshots)
                .after(AimCamera),
        );
    }
}

/// Whose health a bar draws. Players and creatures are separate id spaces on the wire, so
/// the kind of owner is part of the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Owner {
    Player(u64),
    Mob(u64),
}

/// The colour a bar is drawn in, and the one thing the filter is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    /// Another player.
    Friend,
    /// A creature that fights.
    Hostile,
    /// A creature that can be hit and does not fight back.
    Passive,
}

impl Tone {
    const fn fill(self) -> Color {
        match self {
            Self::Friend => FRIEND_FILL,
            Self::Hostile => HOSTILE_FILL,
            Self::Passive => PASSIVE_FILL,
        }
    }

    /// Whether `filter` draws a bar of this colour: "friends" is green, "enemies" is red and
    /// yellow.
    const fn drawn_under(self, filter: HealthBars) -> bool {
        match self {
            Self::Friend => filter.draws_friends(),
            Self::Hostile | Self::Passive => filter.draws_enemies(),
        }
    }
}

/// The colour a creature's bar takes, or `None` for a kind that gets no bar.
const fn mob_tone(kind: MobKind) -> Option<Tone> {
    match hostility(kind) {
        Hostility::Hostile => Some(Tone::Hostile),
        Hostility::Passive => Some(Tone::Passive),
        Hostility::Neutral => None,
    }
}

/// One bar this frame should draw.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Wanted {
    owner: Owner,
    tone: Tone,
    /// The world point projected, and the one reach and line of sight are judged against.
    anchor: Vec3,
    /// How far above the projected point the bar's bottom edge sits, in logical pixels.
    lift: f32,
    /// The fill, the server's `health / max_health`.
    ratio: f32,
}

/// Every bar this frame should draw, as the collecting system found it.
#[derive(Resource, Debug, Default)]
struct OverheadBars(Vec<Wanted>);

/// A bar's track, keyed by the owner it draws.
#[derive(Component)]
struct OverheadBar(Owner);

/// A bar's fill.
#[derive(Component)]
struct OverheadFill(Owner);

fn ratio(health: u16, max_health: u16) -> f32 {
    if max_health == 0 {
        return 0.0;
    }
    (f32::from(health) / f32::from(max_health)).clamp(0.0, 1.0)
}

/// The bar over one creature, if it gets one.
///
/// A creature the server says is `Dying` or a `Corpse` has none: those two actions are the
/// contract's only statement of death, and a health of zero is not read as one. A boss whose
/// move reading `ui/encounters.rs` is drawing keeps that reading and gets no second bar.
fn mob_bar(
    entity_id: u64,
    kind: MobKind,
    action: MobAction,
    (health, max_health): (u16, u16),
    feet: Vec3,
    boss_reading: bool,
) -> Option<Wanted> {
    if boss_reading || matches!(action, MobAction::Dying | MobAction::Corpse) {
        return None;
    }
    Some(Wanted {
        owner: Owner::Mob(entity_id),
        tone: mob_tone(kind)?,
        anchor: mob_overhead_anchor(kind, feet),
        lift: 0.0,
        ratio: ratio(health, max_health),
    })
}

/// The bar over one player, if they get one: never over this session's own body, never over
/// a player the newest snapshot lists as dead, and above the name plate the same anchor
/// carries.
fn player_bar(
    entity_id: u64,
    local_entity_id: u64,
    dead: bool,
    health: Option<(u16, u16)>,
    feet: Vec3,
) -> Option<Wanted> {
    if entity_id == local_entity_id || dead {
        return None;
    }
    let (health, max_health) = health?;
    Some(Wanted {
        owner: Owner::Player(entity_id),
        tone: Tone::Friend,
        anchor: player_overhead_anchor(feet),
        lift: NAME_PLATE_HEIGHT + PLATE_CLEARANCE,
        ratio: ratio(health, max_health),
    })
}

/// Decides which bars this frame draws, from the interpolated snapshot and the filter.
fn collect_overhead_bars(
    settings: Option<Res<Settings>>,
    session: Option<Res<Session>>,
    snapshots: Option<Res<SnapshotBuffer>>,
    encounters: Option<Res<EncounterPresentation>>,
    mut wanted: ResMut<OverheadBars>,
) {
    let filter = settings.map_or(HealthBars::default(), |settings| settings.health_bars());
    let mut next = Vec::new();
    if let (Some(session), Some(snapshots)) = (session, snapshots)
        && filter != HealthBars::None
    {
        let now = Instant::now();
        let interval = Duration::from_secs_f32(1.0 / f32::from(session.0.tick_rate.max(1)));
        let local = session.0.entity_id;
        for (entity_id, drawn) in snapshots.sample(now, interval) {
            next.extend(player_bar(
                entity_id,
                local,
                snapshots.player_is_dead(entity_id),
                snapshots.player_health(entity_id),
                drawn.pos,
            ));
        }
        for (entity_id, mob) in snapshots.sample_mobs(now, interval) {
            let boss_reading = encounters.as_ref().is_some_and(|presentation| {
                presentation.0.iter().any(|one| one.key.boss == entity_id)
            });
            next.extend(mob_bar(
                entity_id,
                mob.kind,
                mob.action,
                (mob.health, mob.max_health),
                mob.pos,
                boss_reading,
            ));
        }
        next.retain(|one| one.tone.drawn_under(filter));
    }
    wanted.0 = next;
}

/// Spawns a bar for every new owner, despawns every bar whose owner is no longer wanted, and
/// writes each fill.
fn sync_overhead_bars(
    mut commands: Commands,
    wanted: Res<OverheadBars>,
    bars: Query<(Entity, &OverheadBar)>,
    mut fills: Query<(&OverheadFill, &mut Node, &mut BackgroundColor)>,
) {
    let mut existing = HashSet::with_capacity(wanted.0.len());
    for (entity, bar) in &bars {
        if wanted.0.iter().any(|one| one.owner == bar.0) {
            existing.insert(bar.0);
        } else {
            commands.entity(entity).despawn();
        }
    }
    for (fill, mut node, mut colour) in &mut fills {
        let Some(one) = wanted.0.iter().find(|one| one.owner == fill.0) else {
            continue;
        };
        let width = Val::Percent(one.ratio * 100.0);
        if node.width != width {
            node.width = width;
        }
        if colour.0 != one.tone.fill() {
            colour.0 = one.tone.fill();
        }
    }
    for one in wanted.0.iter().filter(|one| !existing.contains(&one.owner)) {
        spawn_bar(&mut commands, one);
    }
}

fn spawn_bar(commands: &mut Commands, one: &Wanted) {
    commands
        .spawn((
            OverheadBar(one.owner),
            PlateSight::default(),
            Node {
                position_type: PositionType::Absolute,
                width: Val::Px(WIDTH),
                height: Val::Px(HEIGHT),
                border: UiRect::all(Val::Px(BORDER)),
                border_radius: BorderRadius::all(Val::Px(RADIUS)),
                overflow: Overflow::clip(),
                ..default()
            },
            BackgroundColor(BAR_TRACK),
            BorderColor::all(EDGE),
            FocusPolicy::Pass,
            GlobalZIndex(LAYER),
            // Shown only once a camera has projected the anchor and the sight rules settle.
            Visibility::Hidden,
        ))
        .with_children(|track| {
            track.spawn((
                OverheadFill(one.owner),
                Node {
                    width: Val::Percent(one.ratio * 100.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
                BackgroundColor(one.tone.fill()),
            ));
        });
}

/// Projects each bar's anchor, for the bars the player could see.
fn position_overhead_bars(
    session: Option<Res<Session>>,
    store: Option<Res<ChunkStore>>,
    wanted: Res<OverheadBars>,
    cameras: Query<(&Camera, &Transform), With<WorldCamera>>,
    mut bars: Query<(&OverheadBar, &mut PlateSight, &mut Node, &mut Visibility)>,
) {
    let Ok((camera, camera_transform)) = cameras.single() else {
        for (_, _, _, mut visibility) in &mut bars {
            *visibility = Visibility::Hidden;
        }
        return;
    };
    let eye = camera_transform.translation;
    let camera_transform = GlobalTransform::from(*camera_transform);
    // Both or neither, and an absent pair stops no light — the plates' rule, for their reason.
    let voxels = session
        .as_deref()
        .zip(store.as_deref())
        .map(|(session, store)| (store, usize::from(session.0.chunk_size)));

    for (bar, mut sight, mut node, mut visibility) in &mut bars {
        let Some(one) = wanted.0.iter().find(|one| one.owner == bar.0) else {
            *visibility = Visibility::Hidden;
            continue;
        };
        let next = step_plate_sight(*sight, eye, one.anchor, |voxel| {
            voxels.is_some_and(|(store, size)| {
                store.solid_at(
                    BlockCoord {
                        x: voxel.x,
                        y: voxel.y,
                        z: voxel.z,
                    },
                    size,
                )
            })
        });
        if *sight != next {
            *sight = next;
        }
        if !next.shown() {
            *visibility = Visibility::Hidden;
            continue;
        }
        match camera.world_to_viewport(&camera_transform, one.anchor) {
            Ok(screen) => {
                node.left = Val::Px(screen.x - WIDTH / 2.0);
                node.top = Val::Px(screen.y - one.lift - HEIGHT);
                *visibility = Visibility::Inherited;
            }
            Err(_) => *visibility = Visibility::Hidden,
        }
    }
}

#[cfg(test)]
mod tests {
    //! No window and no GPU: `MinimalPlugins` and this plugin, fed accepted snapshots.

    use super::*;
    use crate::net::{ANY_TOKEN, EntityState, MobState, SessionParams, Snapshot};
    use crate::player::encounters::tests::timeline;
    use crate::player::encounters::{MoveKey, PresentedMove, Window};
    use crate::settings::Knob;

    const LOCAL: u64 = 1;
    const REMOTE: u64 = 2;
    const DEAD: u64 = 3;
    const DRAUGR: u64 = 10;
    const DEER: u64 = 11;
    const VILLAGER: u64 = 12;
    const KING: u64 = 13;

    /// Every kind this build decodes, with the colour the issue gives it. The `match` has no
    /// wildcard arm, so a new kind does not compile here until it has been given one.
    const fn expected_tone(kind: MobKind) -> Option<Tone> {
        match kind {
            MobKind::Draugr | MobKind::Vargr | MobKind::VargrGuardian | MobKind::DraugrKing => {
                Some(Tone::Hostile)
            }
            MobKind::Deer => Some(Tone::Passive),
            MobKind::Villager | MobKind::Horse => None,
        }
    }

    const KINDS: [MobKind; 7] = [
        MobKind::Draugr,
        MobKind::Vargr,
        MobKind::Deer,
        MobKind::Villager,
        MobKind::Horse,
        MobKind::VargrGuardian,
        MobKind::DraugrKing,
    ];

    #[test]
    fn every_kind_takes_its_colour_from_the_hostility_table() {
        for kind in KINDS {
            assert_eq!(mob_tone(kind), expected_tone(kind), "{kind:?}");
        }
        assert_eq!(Tone::Hostile.fill(), BAR_FILL, "an enemy's bar is not red");
        assert_ne!(Tone::Passive.fill(), Tone::Hostile.fill());
        assert_ne!(Tone::Friend.fill(), Tone::Hostile.fill());
        assert_ne!(Tone::Friend.fill(), Tone::Passive.fill());
    }

    #[test]
    fn the_filter_admits_bars_by_colour() {
        let tones = [Tone::Friend, Tone::Hostile, Tone::Passive];
        for (filter, drawn) in [
            (HealthBars::All, [true, true, true]),
            (HealthBars::EnemiesOnly, [false, true, true]),
            (HealthBars::FriendsOnly, [true, false, false]),
            (HealthBars::None, [false, false, false]),
        ] {
            for (tone, drawn) in tones.into_iter().zip(drawn) {
                assert_eq!(tone.drawn_under(filter), drawn, "{tone:?} under {filter:?}");
            }
        }
    }

    #[test]
    fn no_bar_over_the_local_player_a_dead_player_or_a_creature_going_down() {
        let full = Some((100, 100));
        assert!(player_bar(LOCAL, LOCAL, false, full, Vec3::ZERO).is_none());
        assert!(player_bar(REMOTE, LOCAL, true, Some((0, 100)), Vec3::ZERO).is_none());
        assert!(player_bar(REMOTE, LOCAL, false, None, Vec3::ZERO).is_none());
        let remote = player_bar(REMOTE, LOCAL, false, Some((30, 120)), Vec3::ZERO)
            .expect("a living remote player has a bar");
        assert_eq!(remote.tone, Tone::Friend);
        assert!((remote.ratio - 0.25).abs() < f32::EPSILON);
        assert_eq!(remote.anchor, player_overhead_anchor(Vec3::ZERO));

        for action in [MobAction::Dying, MobAction::Corpse] {
            assert!(
                mob_bar(DRAUGR, MobKind::Draugr, action, (0, 20), Vec3::ZERO, false).is_none(),
                "{action:?}"
            );
        }
        // Zero health is not read as death: only the action says a creature is going down.
        let zero = mob_bar(
            DRAUGR,
            MobKind::Draugr,
            MobAction::Chase,
            (0, 20),
            Vec3::ZERO,
            false,
        )
        .expect("a living draugr has a bar");
        assert_eq!(zero.ratio, 0.0);
        assert!(
            mob_bar(
                VILLAGER,
                MobKind::Villager,
                MobAction::Idle,
                (20, 20),
                Vec3::ZERO,
                false
            )
            .is_none()
        );
    }

    #[test]
    fn a_boss_showing_a_move_reading_gets_no_second_bar() {
        let idle = (MobAction::Idle, (500, 1000), Vec3::ZERO);
        assert!(mob_bar(KING, MobKind::DraugrKing, idle.0, idle.1, idle.2, true).is_none());
        let bar = mob_bar(KING, MobKind::DraugrKing, idle.0, idle.1, idle.2, false)
            .expect("a boss with no move reading has its red bar");
        assert_eq!(bar.tone, Tone::Hostile);
    }

    /// The projection gating is the plates' own, and so is its behaviour: nothing on the
    /// first frame, a clear line brings the bar in, a wall or the reach takes it away.
    #[test]
    fn a_bar_is_gated_by_the_plates_reach_line_of_sight_and_dwell() {
        let settle = |anchor: Vec3, solid: &dyn Fn(IVec3) -> bool, sight: &mut PlateSight| {
            for _ in 0..16 {
                *sight = step_plate_sight(*sight, Vec3::ZERO, anchor, solid);
            }
        };
        let near = Vec3::Z * 8.0;
        let far = Vec3::Z * 200.0;
        let clear = |_: IVec3| false;
        let wall = |voxel: IVec3| voxel.z == 4;

        let first = step_plate_sight(PlateSight::default(), Vec3::ZERO, near, clear);
        assert!(!first.shown(), "a bar appeared without its dwell");

        let mut sight = PlateSight::default();
        settle(near, &clear, &mut sight);
        assert!(sight.shown(), "a near bar on a clear line never appeared");
        settle(near, &wall, &mut sight);
        assert!(!sight.shown(), "a wall did not hide the bar");

        let mut sight = PlateSight::default();
        settle(far, &clear, &mut sight);
        assert!(!sight.shown(), "a bar beyond the plates' reach was drawn");
    }

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: LOCAL,
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

    fn player(entity_id: u64, health: u16) -> EntityState {
        EntityState {
            entity_id,
            pos: [entity_id as f32, 64.0, 0.0],
            vel: [0.0; 3],
            yaw: 0.0,
            health,
            max_health: 100,
        }
    }

    fn mob(entity_id: u64, kind: MobKind) -> MobState {
        MobState {
            entity_id,
            kind,
            pos: [entity_id as f32, 64.0, 4.0],
            vel: [0.0; 3],
            yaw: 0.0,
            health: 20,
            max_health: 40,
            action: MobAction::Idle,
            target_entity_id: 0,
        }
    }

    fn snapshot(tick: u32, remote_health: u16, mobs: Vec<MobState>) -> Snapshot {
        Snapshot {
            server_tick: tick,
            entities: vec![
                player(LOCAL, 100),
                player(REMOTE, remote_health),
                player(DEAD, 0),
            ],
            dead_players: vec![DEAD],
            mobs,
            ..Default::default()
        }
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(session())
            .init_resource::<Settings>()
            .init_resource::<SnapshotBuffer>()
            .init_resource::<EncounterPresentation>()
            .add_plugins(OverheadUiPlugin);
        app
    }

    fn deliver(app: &mut App, snapshot: Snapshot) {
        assert!(
            app.world_mut()
                .resource_mut::<SnapshotBuffer>()
                .accept(snapshot, Instant::now())
        );
        app.update();
    }

    fn owners(app: &mut App) -> HashSet<Owner> {
        let world = app.world_mut();
        world
            .query::<&OverheadBar>()
            .iter(world)
            .map(|bar| bar.0)
            .collect()
    }

    fn fill(app: &mut App, owner: Owner) -> (Val, Color) {
        let world = app.world_mut();
        world
            .query::<(&OverheadFill, &Node, &BackgroundColor)>()
            .iter(world)
            .find(|(fill, _, _)| fill.0 == owner)
            .map(|(_, node, colour)| (node.width, colour.0))
            .expect("the owner has a fill")
    }

    #[test]
    fn bars_follow_the_newest_snapshot_and_the_interface_filter() {
        let mut app = app();
        let creatures = vec![
            mob(DRAUGR, MobKind::Draugr),
            mob(DEER, MobKind::Deer),
            mob(VILLAGER, MobKind::Villager),
            mob(KING, MobKind::DraugrKing),
        ];
        deliver(&mut app, snapshot(1, 50, creatures.clone()));
        let everyone = HashSet::from([
            Owner::Player(REMOTE),
            Owner::Mob(DRAUGR),
            Owner::Mob(DEER),
            Owner::Mob(KING),
        ]);
        assert_eq!(owners(&mut app), everyone);
        assert_eq!(
            fill(&mut app, Owner::Player(REMOTE)),
            (Val::Percent(50.0), FRIEND_FILL)
        );
        assert_eq!(
            fill(&mut app, Owner::Mob(DRAUGR)),
            (Val::Percent(50.0), HOSTILE_FILL)
        );
        assert_eq!(fill(&mut app, Owner::Mob(DEER)).1, PASSIVE_FILL);
        // No camera in this app, so nothing is projected and nothing is drawn.
        let world = app.world_mut();
        assert!(
            world
                .query::<(&OverheadBar, &Visibility)>()
                .iter(world)
                .all(|(_, visibility)| *visibility == Visibility::Hidden)
        );

        // Friends only, enemies only, none, and back to all.
        app.world_mut()
            .resource_mut::<Settings>()
            .adjust(Knob::HealthBars, 2);
        app.update();
        assert_eq!(owners(&mut app), HashSet::from([Owner::Player(REMOTE)]));
        app.world_mut()
            .resource_mut::<Settings>()
            .adjust(Knob::HealthBars, -1);
        app.update();
        assert_eq!(
            owners(&mut app),
            HashSet::from([Owner::Mob(DRAUGR), Owner::Mob(DEER), Owner::Mob(KING)])
        );
        app.world_mut()
            .resource_mut::<Settings>()
            .adjust(Knob::HealthBars, 2);
        app.update();
        assert!(
            owners(&mut app).is_empty(),
            "a bar survived the None filter"
        );
        app.world_mut()
            .resource_mut::<Settings>()
            .adjust(Knob::HealthBars, -9);
        app.update();
        assert_eq!(owners(&mut app), everyone);

        // A boss drawing a move reading loses its bar while the reading is up.
        let timeline = timeline();
        app.world_mut().resource_mut::<EncounterPresentation>().0 = vec![PresentedMove {
            key: MoveKey {
                encounter: 5,
                boss: KING,
                instance: 11,
            },
            boss_kind: MobKind::DraugrKing,
            stage: 1,
            announced: timeline.moves[0].clone(),
            window: Window::Current,
            progress: 0.5,
            remaining_ticks: 10,
        }];
        app.update();
        assert!(!owners(&mut app).contains(&Owner::Mob(KING)));
        app.world_mut()
            .resource_mut::<EncounterPresentation>()
            .0
            .clear();

        // The frame the draugr leaves the snapshot its bar is gone, and a new health reading
        // reaches the fill on the same update.
        deliver(&mut app, snapshot(2, 25, vec![mob(DEER, MobKind::Deer)]));
        assert_eq!(
            owners(&mut app),
            HashSet::from([Owner::Player(REMOTE), Owner::Mob(DEER)])
        );
        assert_eq!(fill(&mut app, Owner::Player(REMOTE)).0, Val::Percent(25.0));
        let world = app.world_mut();
        assert_eq!(
            world.query::<&OverheadFill>().iter(world).count(),
            2,
            "a despawned bar left its fill behind"
        );
    }
}
