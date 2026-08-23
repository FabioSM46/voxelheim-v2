//! Draugr and vargr bodies, drawn from the newest authoritative snapshot.
//!
//! A mob exists exactly while the newest snapshot names its id. There is no local AI, no
//! collision, no pathfinding and no inference: **zero health is not read as death**,
//! because a draugr that walked out of the view cube and one that was killed look
//! identical from here, and only one of them is dead. The server stops sending it; this
//! module despawns it; nobody guesses which happened.
//!
//! Everything animated below is cosmetic and follows an authoritative transition rather
//! than deciding one. A windup pose plays because the server said `Windup`, and it ends
//! when the server says something else. **The two kinds share every one of those rules**:
//! a vargr leans and flashes through exactly the code a draugr does, on the timings the
//! server's registry chose, because a second copy of them here would be a second thing to
//! keep in step and would decide nothing either way.
//!
//! ## The bodies are mirrored from the server, and must stay in step with it
//!
//! [`DRAUGR_BODY`] and [`VARGR_BODY`] are copies of the `body` field of each row in
//! `server/internal/game/species.go`, which is where collision, the swing's reach and the
//! spawn separation all read it from. The server collides that box and this side draws
//! inside it, so a mismatch is a creature that visibly does not fill the space a swing
//! reaches — the same rule `PLAYER_WIDTH`/`PLAYER_HEIGHT` already follow in
//! [`super::constants`], and [`the_drawn_body_is_the_box_the_server_collides`] is what
//! holds it.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use bevy::prelude::*;

use super::interpolate::{InterpolatedMob, SnapshotBuffer};
use super::{InputMode, merge_all};
use crate::net::{MobAction, MobKind, Session};

/// The box one species occupies, in blocks: square in plan, `height` tall, standing on
/// the point the snapshot puts it at.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Body {
    width: f32,
    height: f32,
}

/// A corpse on two legs, and a beast on four.
///
/// Mirrored from `mobRegistry` in `server/internal/game/species.go`. The vargr is wider
/// and much shorter than a draugr, and the width is the half that matters most: it is
/// what puts a vargr inside a sword's reach where a draugr is not, because the server
/// measures a swing body to body.
const DRAUGR_BODY: Body = Body {
    width: 0.6,
    height: 1.8,
};
const VARGR_BODY: Body = Body {
    width: 0.9,
    height: 1.0,
};

/// The box the server collides for one kind. Total over [`MobKind`], with no wildcard
/// arm, so a third species does not compile until it has been given a body.
const fn body(kind: MobKind) -> Body {
    match kind {
        MobKind::Draugr => DRAUGR_BODY,
        MobKind::Vargr => VARGR_BODY,
    }
}

/// How much of a draugr's height the head takes.
const HEAD_EDGE: f32 = 0.34;

/// The vargr, in fractions of its own box: a torso the full width of what the server
/// collides, four short legs under it, a raised hackled ridge that reaches the top of the
/// box, and a low head thrust out along the facing.
///
/// **Low and long is the whole point of the silhouette.** A draugr is 0.6 wide and 1.8
/// tall and a remote player's capsule is the same; a vargr at 0.9 by 1.0 reads as neither
/// from any distance, which is what "tell a vargr from a draugr before it is close enough
/// to bite" asks for.
const VARGR_TORSO: Vec3 = Vec3::new(VARGR_BODY.width, 0.40, 0.62);
const VARGR_TORSO_CENTRE: Vec3 = Vec3::new(0.0, 0.42, 0.14);
const VARGR_HACKLES: Vec3 = Vec3::new(0.34, 0.38, 0.30);
const VARGR_HEAD: Vec3 = Vec3::new(0.42, 0.34, 0.34);
const VARGR_LEG: Vec3 = Vec3::new(0.16, 0.24, 0.16);
const VARGR_LEG_SPREAD: Vec3 = Vec3::new(0.26, 0.0, 0.26);

/// How long a hit flash lasts. Short enough to read as an impact rather than a state.
const FLASH_TIME: Duration = Duration::from_millis(180);

/// How far a windup leans the body back, and a recovery forward, in radians.
///
/// Bounded on purpose: the pose says *which* action the server chose, and cannot be
/// mistaken for the action itself progressing.
const WINDUP_LEAN: f32 = -0.35;
const RECOVERY_LEAN: f32 = 0.22;

/// How quickly a pose settles towards its target lean, per second.
const LEAN_RESPONSE: f32 = 12.0;

/// Modes whose UI owns the view instead of the 3D world. The same rule drops obey.
const HIDDEN_INPUT_MODES: [InputMode; 2] = [InputMode::Inventory, InputMode::Menu];

/// The undead grey a draugr is drawn in.
const DRAUGR_BODY_COLOUR: Color = Color::srgb(0.36, 0.40, 0.38);
const DRAUGR_HEAD_COLOUR: Color = Color::srgb(0.46, 0.48, 0.44);

/// The vargr's pelt: warm and dark where the draugr is cold and pale, so the two are told
/// apart by colour as well as by silhouette at the distance the difference matters.
const VARGR_BODY_COLOUR: Color = Color::srgb(0.26, 0.22, 0.20);
const VARGR_HEAD_COLOUR: Color = Color::srgb(0.38, 0.33, 0.29);

/// The red a hit flashes. Shared by every kind: an impact reads the same whatever was hit.
const FLASH_COLOUR: Color = Color::srgb(0.85, 0.20, 0.18);

// The systems below are registered by `PlayerPlugin` rather than by a plugin of their
// own, exactly as the drop renderer's are. That is not a style choice: they have to run
// *inside* the chain that begins with `ingest_snapshots`, because the buffer they sample
// is filled there. A plugin adding them to the `ApplySnapshots` set instead would order
// them against the set and not against the system, which leaves them free to run before
// the snapshot they are meant to draw has arrived — a body that never spawns.

/// The two meshes and two materials one species is drawn from.
///
/// Two of each rather than one per part, so a hit flash stays the single material swap it
/// has always been however many primitives a species is built out of: everything that is
/// not the head is merged into `body`, exactly as a structure's parts are merged into one
/// mesh per material.
#[derive(Debug, Clone)]
struct SpeciesVisuals {
    body: Handle<Mesh>,
    head: Handle<Mesh>,
    body_material: Handle<StandardMaterial>,
    head_material: Handle<StandardMaterial>,
}

/// The shared meshes and materials every body is drawn from, one entry per species.
///
/// Primitives and hand-written colours, no assets: this issue adds no model, texture or
/// animation file, and a body built from a handful of cuboids is enough to tell a draugr
/// from a vargr and both from a remote player's capsule at a glance.
///
/// Each mesh is authored with its origin at the creature's **feet**, which is where the
/// server puts the position it sends, so the parent transform is the whole of where a body
/// stands and the children carry no offset of their own.
#[derive(Resource, Debug)]
pub(super) struct MobVisuals {
    draugr: SpeciesVisuals,
    vargr: SpeciesVisuals,
    /// One flash for every kind: an impact reads the same whatever was hit.
    flash_material: Handle<StandardMaterial>,
}

impl MobVisuals {
    /// The pair one kind is drawn from. Total over [`MobKind`] with no wildcard arm, so a
    /// third species does not compile until it has been given meshes and colours.
    fn of(&self, kind: MobKind) -> &SpeciesVisuals {
        match kind {
            MobKind::Draugr => &self.draugr,
            MobKind::Vargr => &self.vargr,
        }
    }
}

/// One live mob of either kind, keyed by the identity the server gave it.
#[derive(Component, Debug)]
pub(super) struct Mob {
    entity_id: u64,
    kind: MobKind,
    action: MobAction,

    /// The health the last snapshot reported. A *decrease* is what flashes; anything
    /// else — unchanged, or the higher health of a replacement that reused nothing —
    /// does not.
    health: u16,

    /// How long the current flash has been running, if one is.
    flash: Option<Duration>,

    /// The interpolated facing the newest sample gave it.
    ///
    /// Kept here rather than read back out of the `Transform` every frame. The transform
    /// carries the lean as well, so recovering the yaw from it means an euler round trip
    /// through a rotation this module composed — and a value nothing writes is a value
    /// that never changes, which is a draugr that chases you without ever turning to
    /// look at you.
    yaw: f32,

    /// The lean the pose is easing towards, and where it has got to.
    lean: f32,
}

/// Marks the two child meshes so a flash can recolour them without touching the parent.
#[derive(Component, Debug)]
pub(super) struct MobVisual {
    owner: Entity,
    head: bool,
}

pub(super) fn create_visuals(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(MobVisuals {
        draugr: SpeciesVisuals {
            body: meshes.add(draugr_body_mesh()),
            head: meshes.add(draugr_head_mesh()),
            body_material: materials.add(StandardMaterial::from_color(DRAUGR_BODY_COLOUR)),
            head_material: materials.add(StandardMaterial::from_color(DRAUGR_HEAD_COLOUR)),
        },
        vargr: SpeciesVisuals {
            body: meshes.add(vargr_body_mesh()),
            head: meshes.add(vargr_head_mesh()),
            body_material: materials.add(StandardMaterial::from_color(VARGR_BODY_COLOUR)),
            head_material: materials.add(StandardMaterial::from_color(VARGR_HEAD_COLOUR)),
        },
        flash_material: materials.add(StandardMaterial::from_color(FLASH_COLOUR)),
    });
}

// Every mesh below reads its extents through [`body`] rather than from the constant
// beside it. That is one indirection for one reason: it makes the mirrored registry the
// single path from the server's number to the geometry, so the sweep that compares the
// two is comparing something rather than restating one constant twice.

/// The draugr's torso: one cuboid the full width of its box, standing on the feet.
fn draugr_body_mesh() -> Mesh {
    let draugr = body(MobKind::Draugr);
    let height = draugr.height - HEAD_EDGE;
    Mesh::from(Cuboid::from_size(Vec3::new(
        draugr.width,
        height,
        draugr.width,
    )))
    // The parent sits at the feet, as every entity in this game does, so the box is
    // lifted by half its own height to stand on that point.
    .translated_by(Vec3::Y * (height / 2.0))
}

/// The draugr's head: a cube on top of the torso, closing the box at its full height.
fn draugr_head_mesh() -> Mesh {
    Mesh::from(Cuboid::from_size(Vec3::splat(HEAD_EDGE)))
        .translated_by(Vec3::Y * (body(MobKind::Draugr).height - HEAD_EDGE / 2.0))
}

/// The vargr's body: the torso, the four legs under it and the hackled ridge over it.
///
/// Authored in the canonical facing — North is -Z, so the head end is -Z and the haunches
/// are +Z — and merged into one mesh, so a hit flash stays a single material swap.
fn vargr_body_mesh() -> Mesh {
    let vargr = body(MobKind::Vargr);
    let mut torso = Mesh::from(Cuboid::from_size(VARGR_TORSO)).translated_by(VARGR_TORSO_CENTRE);

    // The ridge over the shoulders, and the highest thing a vargr has: it is what takes
    // the drawn body up to the full height the server collides.
    let hackles = Mesh::from(Cuboid::from_size(VARGR_HACKLES)).translated_by(Vec3::new(
        0.0,
        vargr.height - VARGR_HACKLES.y / 2.0,
        VARGR_TORSO_CENTRE.z - VARGR_TORSO.z / 4.0,
    ));

    let leg = Cuboid::from_size(VARGR_LEG);
    let legs = [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)].map(|(sx, sz)| {
        Mesh::from(leg).translated_by(Vec3::new(
            sx * VARGR_LEG_SPREAD.x,
            VARGR_LEG.y / 2.0,
            sz * VARGR_LEG_SPREAD.z,
        ))
    });

    merge_all(
        &mut torso,
        std::iter::once(hackles).chain(legs),
        "vargr body",
    );
    torso
}

/// The vargr's head: low and thrust out along the facing, reaching the front of the box.
fn vargr_head_mesh() -> Mesh {
    Mesh::from(Cuboid::from_size(VARGR_HEAD)).translated_by(Vec3::new(
        0.0,
        VARGR_TORSO_CENTRE.y - VARGR_TORSO.y / 5.0,
        -(body(MobKind::Vargr).width - VARGR_HEAD.z) / 2.0,
    ))
}

/// Spawns, places and despawns bodies from the latest authoritative snapshot.
///
/// The newest snapshot is the existence set, exactly as it is for drops. Nothing here
/// reads health to decide whether a mob still exists.
pub(super) fn apply_snapshots(
    buffer: Res<SnapshotBuffer>,
    session: Option<Res<Session>>,
    mode: Res<InputMode>,
    visuals: Option<Res<MobVisuals>>,
    mut existing: Query<(Entity, &mut Mob, &mut Transform, &mut Visibility)>,
    mut commands: Commands,
) {
    let (Some(session), Some(visuals)) = (session, visuals) else {
        return;
    };

    let interval = Duration::from_secs(1) / u32::from(session.0.tick_rate);
    let drawn = buffer.sample_mobs(Instant::now(), interval);
    let by_id: HashMap<u64, InterpolatedMob> = drawn.iter().copied().collect();
    let mut placed = HashSet::with_capacity(drawn.len());
    let visibility = mob_visibility(*mode);

    for (entity, mut mob, mut transform, mut current_visibility) in &mut existing {
        let Some(state) = by_id.get(&mob.entity_id) else {
            // Gone from the newest answer, so gone from this world. Why is not asked.
            commands.entity(entity).despawn();
            continue;
        };

        transform.translation = state.pos;
        mob.yaw = state.yaw;
        if *current_visibility != visibility {
            *current_visibility = visibility;
        }

        // A *decrease* only. A replacement draugr arrives with a fresh identity, so its
        // full health is a new body rather than a heal — and an unchanged number is not
        // an impact however many frames it survives.
        if state.health < mob.health {
            mob.flash = Some(Duration::ZERO);
        }
        mob.health = state.health;
        mob.action = state.action;
        mob.kind = state.kind;

        placed.insert(mob.entity_id);
    }

    for (entity_id, state) in &drawn {
        if !placed.insert(*entity_id) {
            continue;
        }
        spawn_mob(&mut commands, &visuals, *entity_id, state, visibility);
    }
}

fn spawn_mob(
    commands: &mut Commands,
    visuals: &MobVisuals,
    entity_id: u64,
    state: &InterpolatedMob,
    visibility: Visibility,
) {
    let owner = commands
        .spawn((
            Mob {
                entity_id,
                kind: state.kind,
                action: state.action,
                // The first snapshot of a body is not an impact, whatever its health.
                health: state.health,
                flash: None,
                yaw: state.yaw,
                lean: lean_for(state.action),
            },
            Transform::from_translation(state.pos).with_rotation(Quat::from_rotation_y(state.yaw)),
            visibility,
        ))
        .id();

    // Which species is chosen once, here, and nothing downstream re-asks: a mob whose
    // kind *changed* is a mob the server replaced, and a replacement arrives with a fresh
    // identity, so there is no path on which a body outlives its own shape.
    let species = visuals.of(state.kind).clone();

    commands.entity(owner).with_children(|parent| {
        // No offset on either child: both meshes are authored with their origin at the
        // feet, which is the point the parent transform already stands on.
        parent.spawn((
            MobVisual { owner, head: false },
            Mesh3d(species.body),
            MeshMaterial3d(species.body_material),
            Transform::default(),
        ));
        parent.spawn((
            MobVisual { owner, head: true },
            Mesh3d(species.head),
            MeshMaterial3d(species.head_material),
            Transform::default(),
        ));
    });
}

fn mob_visibility(mode: InputMode) -> Visibility {
    if HIDDEN_INPUT_MODES.contains(&mode) {
        Visibility::Hidden
    } else {
        Visibility::Visible
    }
}

/// The lean an action poses at. Bounded, and a function of the action alone.
fn lean_for(action: MobAction) -> f32 {
    match action {
        MobAction::Windup => WINDUP_LEAN,
        MobAction::Recovery => RECOVERY_LEAN,
        MobAction::Idle | MobAction::Chase => 0.0,
    }
}

/// Runs the cosmetic half: the pose easing towards its action's lean, and the hit flash.
///
/// Local time drives both, and neither can change an action, a health or whether a body
/// exists. A flash that outlives its mob simply goes with it.
pub(super) fn animate(
    time: Res<Time>,
    visuals: Option<Res<MobVisuals>>,
    mut mobs: Query<(Entity, &mut Mob, &mut Transform)>,
    mut parts: Query<(&MobVisual, &mut MeshMaterial3d<StandardMaterial>)>,
) {
    let Some(visuals) = visuals else {
        return;
    };
    let delta = time.delta();

    let mut kinds: HashMap<Entity, MobKind> = HashMap::new();
    let mut flashing = HashSet::new();
    for (entity, mut mob, mut transform) in &mut mobs {
        // Exponential easing towards the target, so the pose is frame-rate independent
        // and never overshoots into a lean the server did not ask for. The timings the
        // pose reads are the server's — `mob.action` is a snapshot field — so a vargr
        // leans and recovers on its own species' clock without a second copy of it here.
        let target = lean_for(mob.action);
        let response = 1.0 - (-LEAN_RESPONSE * delta.as_secs_f32()).exp();
        mob.lean += (target - mob.lean) * response;

        transform.rotation = Quat::from_rotation_y(mob.yaw) * Quat::from_rotation_x(mob.lean);

        if let Some(elapsed) = mob.flash.as_mut() {
            *elapsed += delta;
            if *elapsed >= FLASH_TIME {
                mob.flash = None;
            } else {
                flashing.insert(entity);
            }
        }

        kinds.insert(entity, mob.kind);
    }

    for (part, mut material) in &mut parts {
        let Some(species) = kinds.get(&part.owner).map(|kind| visuals.of(*kind)) else {
            // The body this part hangs under was despawned this frame and the child goes
            // with it. There is nothing left to recolour.
            continue;
        };
        let next = if flashing.contains(&part.owner) {
            visuals.flash_material.clone()
        } else if part.head {
            species.head_material.clone()
        } else {
            species.body_material.clone()
        };
        if material.0 != next {
            material.0 = next;
        }
    }
}

#[cfg(test)]
mod tests {
    //! No window, no display and no GPU. Every assertion is about what the *server* said,
    //! or about a cosmetic value local time produced from it.

    use bevy::asset::AssetPlugin;
    use bevy::time::TimeUpdateStrategy;

    use super::*;
    use crate::net::{MobState, SessionParams, Snapshot, SnapshotInbox};
    use crate::player::PlayerPlugin;

    const INTERVAL: Duration = Duration::from_millis(50);

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 7,
            spawn: [0.5, 64.0, 0.5],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 8,
            inventory_slots: 36,
            hotbar_slots: 9,
            player_token: crate::net::ANY_TOKEN,
        })
    }

    fn draugr(entity_id: u64, x: f32, health: u16, action: MobAction) -> MobState {
        MobState {
            entity_id,
            kind: MobKind::Draugr,
            pos: [x, 64.0, 0.0],
            vel: [0.0; 3],
            yaw: 0.0,
            health,
            max_health: 60,
            action,
        }
    }

    /// The same body with the other kind on it, at the maximum its own row registers.
    fn vargr(entity_id: u64, x: f32, health: u16, action: MobAction) -> MobState {
        MobState {
            kind: MobKind::Vargr,
            max_health: 35,
            ..draugr(entity_id, x, health, action)
        }
    }

    fn headless() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .insert_resource(session())
            .add_plugins(PlayerPlugin);
        app
    }

    fn deliver(app: &mut App, tick: u32, mobs: Vec<MobState>) {
        app.world_mut().resource_mut::<SnapshotInbox>().push(
            Snapshot {
                server_tick: tick,
                mobs,
                ..Default::default()
            },
            Instant::now(),
        );
    }

    fn bodies(app: &mut App) -> Vec<(u64, u16, MobAction)> {
        let world = app.world_mut();
        let mut query = world.query::<&Mob>();
        let mut found: Vec<_> = query
            .iter(world)
            .map(|mob| (mob.entity_id, mob.health, mob.action))
            .collect();
        found.sort_by_key(|(entity_id, _, _)| *entity_id);
        found
    }

    fn flashing(app: &mut App) -> usize {
        let world = app.world_mut();
        let mut query = world.query::<&Mob>();
        query.iter(world).filter(|mob| mob.flash.is_some()).count()
    }

    /// Every kind the newest snapshot put a body on screen for.
    fn kinds(app: &mut App) -> Vec<(u64, MobKind)> {
        let world = app.world_mut();
        let mut query = world.query::<&Mob>();
        let mut found: Vec<_> = query
            .iter(world)
            .map(|mob| (mob.entity_id, mob.kind))
            .collect();
        found.sort_by_key(|(entity_id, _)| *entity_id);
        found
    }

    /// The meshes and materials every drawn part is built from.
    fn parts(app: &mut App) -> Vec<(Handle<Mesh>, Handle<StandardMaterial>)> {
        let world = app.world_mut();
        let mut query =
            world.query_filtered::<(&Mesh3d, &MeshMaterial3d<StandardMaterial>), With<MobVisual>>();
        query
            .iter(world)
            .map(|(mesh, material)| (mesh.0.clone(), material.0.clone()))
            .collect()
    }

    /// The box every vertex of one species' meshes fits inside, as `(min, max)`.
    fn drawn_extent(meshes: &[Mesh]) -> (Vec3, Vec3) {
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for mesh in meshes {
            let Some(positions) = mesh.attribute(Mesh::ATTRIBUTE_POSITION) else {
                panic!("every part carries positions");
            };
            for vertex in positions.as_float3().expect("three floats per position") {
                let vertex = Vec3::from_array(*vertex);
                min = min.min(vertex);
                max = max.max(vertex);
            }
        }
        (min, max)
    }

    #[test]
    fn a_snapshot_with_two_draugr_spawns_two_bodies() {
        let mut app = headless();
        deliver(
            &mut app,
            1,
            vec![
                draugr(900, 3.0, 60, MobAction::Idle),
                draugr(901, 9.0, 60, MobAction::Chase),
            ],
        );
        app.update();

        assert_eq!(
            bodies(&mut app),
            vec![(900, 60, MobAction::Idle), (901, 60, MobAction::Chase)]
        );
    }

    /// The newest snapshot is the complete existence set, exactly as it is for drops.
    #[test]
    fn a_draugr_omitted_by_the_newest_snapshot_is_gone() {
        let mut app = headless();
        deliver(&mut app, 1, vec![draugr(900, 3.0, 60, MobAction::Idle)]);
        app.update();
        assert_eq!(bodies(&mut app).len(), 1);

        deliver(&mut app, 2, vec![]);
        app.update();
        assert!(bodies(&mut app).is_empty());
    }

    /// **Zero health is not death.** The server decides whether a draugr still exists by
    /// including it, and a client that despawned on a health of zero would be inventing a
    /// transition — one it would get wrong for a mob that is merely out of the view cube.
    #[test]
    fn a_draugr_at_zero_health_is_not_despawned_by_this_client() {
        let mut app = headless();
        deliver(&mut app, 1, vec![draugr(900, 3.0, 60, MobAction::Idle)]);
        app.update();

        // Zero health, and the server still sending it. It is still there.
        deliver(&mut app, 2, vec![draugr(900, 3.0, 0, MobAction::Idle)]);
        app.update();
        assert_eq!(bodies(&mut app), vec![(900, 0, MobAction::Idle)]);

        // Repeated, so a client that filtered zero-health mobs out of the set it draws
        // has somewhere to act rather than only a first frame to survive.
        for tick in 3..7 {
            deliver(&mut app, tick, vec![draugr(900, 3.0, 0, MobAction::Idle)]);
            app.update();
            assert_eq!(
                bodies(&mut app),
                vec![(900, 0, MobAction::Idle)],
                "a draugr at zero health was despawned by this client on tick {tick}"
            );
        }

        // And a body first *seen* at zero health is drawn too: a client arriving mid-fight
        // is told what exists, not what is healthy.
        deliver(
            &mut app,
            7,
            vec![
                draugr(900, 3.0, 0, MobAction::Idle),
                draugr(901, 5.0, 0, MobAction::Idle),
            ],
        );
        app.update();
        assert_eq!(
            bodies(&mut app),
            vec![(900, 0, MobAction::Idle), (901, 0, MobAction::Idle)]
        );
    }

    #[test]
    fn only_an_authoritative_health_decrease_flashes() {
        let mut app = headless();

        // A short step by default. **The strategy matters more than it looks**: `animate`
        // runs in the same chain that set the flash, so a manual duration of FLASH_TIME
        // would advance every flash past its own window on the frame it began — and every
        // assertion below would read zero whatever the code did.
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(1)));

        deliver(&mut app, 1, vec![draugr(900, 3.0, 60, MobAction::Idle)]);
        app.update();
        assert_eq!(
            flashing(&mut app),
            0,
            "the first sight of a body is not a hit"
        );

        deliver(&mut app, 2, vec![draugr(900, 3.0, 60, MobAction::Chase)]);
        app.update();
        assert_eq!(flashing(&mut app), 0, "an unchanged health flashed");

        deliver(&mut app, 3, vec![draugr(900, 3.0, 35, MobAction::Chase)]);
        app.update();
        assert_eq!(flashing(&mut app), 1, "a health decrease did not flash");

        // It ends on local time, without another snapshot.
        app.insert_resource(TimeUpdateStrategy::ManualDuration(FLASH_TIME));
        app.update();
        assert_eq!(flashing(&mut app), 0, "the flash outlived its window");
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(1)));

        // The same body back at *higher* health — the case that separates "a decrease"
        // from "a change". Nothing about a number going up is an impact.
        deliver(&mut app, 4, vec![draugr(900, 3.0, 60, MobAction::Chase)]);
        app.update();
        assert_eq!(flashing(&mut app), 0, "a health increase flashed");
    }

    /// A draugr turns to face what it is hunting, and the *transform* is where that has
    /// to show.
    ///
    /// `sample_mobs` computed an interpolated yaw from the first commit; nothing applied
    /// it, so a body kept the facing it spawned with for its whole life. Every test in
    /// this module asserted the sampler's output or the component's fields, and none of
    /// them looked at what was actually drawn — which is exactly where it went wrong.
    #[test]
    fn a_draugr_turns_to_face_where_the_server_says() {
        fn drawn_yaw(app: &mut App) -> f32 {
            let world = app.world_mut();
            let mut query = world.query_filtered::<&Transform, With<Mob>>();
            query
                .single(world)
                .expect("one body")
                .rotation
                .to_euler(EulerRot::YXZ)
                .0
        }

        let mut app = headless();
        let mut facing = draugr(900, 3.0, 60, MobAction::Idle);
        facing.yaw = 0.0;
        deliver(&mut app, 1, vec![facing]);
        app.update();
        assert!(drawn_yaw(&mut app).abs() < 1e-4);

        // Two snapshots at the same facing, so the sample is a settled 1.5 rather than a
        // point part way along a segment.
        facing.yaw = 1.5;
        deliver(&mut app, 2, vec![facing]);
        app.update();
        deliver(&mut app, 3, vec![facing]);
        app.update();

        let drawn = drawn_yaw(&mut app);
        assert!(
            (drawn - 1.5).abs() < 1e-3,
            "the body is facing {drawn}, want the 1.5 the server sent"
        );
    }

    /// A replacement draugr arrives with a fresh identity and full health. That is a new
    /// body rather than a heal, and it must not read as one.
    #[test]
    fn a_replacement_draugr_is_a_new_body_rather_than_a_heal() {
        let mut app = headless();
        deliver(&mut app, 1, vec![draugr(900, 3.0, 10, MobAction::Idle)]);
        app.update();

        deliver(&mut app, 2, vec![draugr(901, 3.0, 60, MobAction::Idle)]);
        app.update();

        assert_eq!(bodies(&mut app), vec![(901, 60, MobAction::Idle)]);
        assert_eq!(flashing(&mut app), 0, "a fresh body flashed");
    }

    /// The pose follows the server's action and cannot advance it.
    #[test]
    fn the_pose_leans_where_the_action_says_and_nowhere_else() {
        assert_eq!(lean_for(MobAction::Idle), 0.0);
        assert_eq!(lean_for(MobAction::Chase), 0.0);
        assert_eq!(lean_for(MobAction::Windup), WINDUP_LEAN);
        assert_eq!(lean_for(MobAction::Recovery), RECOVERY_LEAN);

        let mut app = headless();
        deliver(&mut app, 1, vec![draugr(900, 3.0, 60, MobAction::Windup)]);
        app.update();

        // The action is the server's word, held until another snapshot changes it.
        app.insert_resource(TimeUpdateStrategy::ManualDuration(INTERVAL));
        for _ in 0..10 {
            app.update();
            assert_eq!(bodies(&mut app), vec![(900, 60, MobAction::Windup)]);
        }
    }

    #[test]
    fn a_ui_mode_hides_the_bodies_without_despawning_them() {
        let mut app = headless();
        deliver(
            &mut app,
            1,
            vec![
                draugr(900, 3.0, 60, MobAction::Idle),
                vargr(901, 6.0, 35, MobAction::Chase),
            ],
        );
        app.update();

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Inventory;
        app.update();

        let world = app.world_mut();
        let mut query = world.query_filtered::<&Visibility, With<Mob>>();
        let drawn: Vec<_> = query.iter(world).copied().collect();
        assert_eq!(
            drawn,
            vec![Visibility::Hidden; 2],
            "an open pack left a body drawn"
        );
        assert_eq!(bodies(&mut app).len(), 2, "hiding despawned a body");
    }

    // ---------------------------------------------------------------------------
    // The vargr
    // ---------------------------------------------------------------------------

    /// **The test the issue exists for, and the reason it was urgent.**
    ///
    /// Until legacy PR 172 a vargr never reached this module at all: `MobKind::from_wire` answered
    /// `None`, `mob_state` turned that into `DecodeError::UnknownMobEnum`, and
    /// `net/session.rs` turns any decode error into a `protocol_failure` — so the first
    /// vargr the server sent ended the connection rather than being skipped. The decoder's
    /// half is pinned in `net/codec.rs`; this is the half that makes accepting it correct,
    /// because a kind the decoder admits and the renderer cannot draw is a body with no
    /// mesh.
    #[test]
    fn a_snapshot_with_a_vargr_spawns_one_body_and_omitting_it_takes_it_away() {
        let mut app = headless();
        deliver(&mut app, 1, vec![vargr(900, 3.0, 35, MobAction::Chase)]);
        app.update();
        assert_eq!(kinds(&mut app), vec![(900, MobKind::Vargr)]);

        // The newest snapshot is the complete existence set for a vargr exactly as it is
        // for a draugr. Why it went is not asked.
        deliver(&mut app, 2, vec![]);
        app.update();
        assert!(bodies(&mut app).is_empty());
    }

    /// Two kinds in one snapshot, each drawn from its own meshes and its own colours.
    ///
    /// The parts are compared as *handles*, which is what catches the failure worth
    /// catching: a vargr silently built from the draugr's meshes would spawn the right
    /// number of children, be the right size in every component, and draw a corpse on two
    /// legs.
    #[test]
    fn a_vargr_is_drawn_from_its_own_meshes_rather_than_the_draugrs() {
        let mut app = headless();
        deliver(&mut app, 1, vec![draugr(900, 3.0, 60, MobAction::Idle)]);
        app.update();
        let drawn_draugr = parts(&mut app);
        assert_eq!(drawn_draugr.len(), 2, "a draugr is a body and a head");

        deliver(&mut app, 2, vec![vargr(901, 9.0, 35, MobAction::Idle)]);
        app.update();
        let drawn_vargr = parts(&mut app);
        assert_eq!(drawn_vargr.len(), 2, "a vargr is a body and a head");

        for part in &drawn_vargr {
            assert!(
                !drawn_draugr.contains(part),
                "a vargr reused one of the draugr's parts: {part:?}"
            );
        }
    }

    /// **The drawn body is the box the server collides**, for every kind, exactly.
    ///
    /// The numbers are mirrored from the `body` field of each row in
    /// `server/internal/game/species.go`, which is what the server's collision, its
    /// swing reach and its spawn separation all read. A drawn body smaller than that box
    /// is a creature a swing reaches through empty air; a larger one is a creature that
    /// visibly overlaps ground it does not occupy. Both are the same defect the
    /// `PLAYER_WIDTH`/`PLAYER_HEIGHT` mirror exists to prevent.
    ///
    /// Asserted on the *meshes* rather than on the constants, so a part authored at the
    /// wrong offset fails here rather than looking right in a table and wrong on screen.
    #[test]
    fn the_drawn_body_is_the_box_the_server_collides() {
        for (kind, meshes) in [
            (
                MobKind::Draugr,
                vec![draugr_body_mesh(), draugr_head_mesh()],
            ),
            (MobKind::Vargr, vec![vargr_body_mesh(), vargr_head_mesh()]),
        ] {
            let expected = body(kind);
            let (min, max) = drawn_extent(&meshes);
            let half = expected.width / 2.0;

            for (axis, got, want) in [
                ("width", max.x - min.x, expected.width),
                ("height", max.y - min.y, expected.height),
                ("depth", max.z - min.z, expected.width),
            ] {
                assert!(
                    (got - want).abs() < 1e-5,
                    "a {kind:?} is drawn {got} across in {axis}, want the {want} the server collides"
                );
            }

            // And it is centred on the point the snapshot names, standing on it: the
            // server sends the feet, so a body floating or sunk is the same mismatch one
            // axis over.
            assert!(min.y.abs() < 1e-5, "a {kind:?} does not stand on its feet");
            assert!(
                (min.x + half).abs() < 1e-5 && (max.x - half).abs() < 1e-5,
                "a {kind:?} is not centred on the position the server sends"
            );
        }
    }

    /// A vargr is unmistakable from a draugr and from a remote player's capsule.
    ///
    /// Both of those are 0.6 × 1.8 — `PLAYER_WIDTH`/`PLAYER_HEIGHT` and the draugr's row
    /// happen to agree — so "low and long" is one comparison: broader in plan and much
    /// shorter. Asserted as a *relationship* rather than as two more copies of the
    /// numbers, because what has to survive a rebalance is the silhouette being different,
    /// not either species keeping the size it has today.
    #[test]
    fn a_vargr_reads_as_neither_a_draugr_nor_a_player() {
        let vargr = body(MobKind::Vargr);
        let draugr = body(MobKind::Draugr);

        assert!(
            vargr.width > draugr.width,
            "a vargr is not broader than a draugr"
        );
        assert!(
            vargr.height < draugr.height / 1.5,
            "a vargr is not markedly shorter than a draugr"
        );
        // Taller than it is wide is what a standing figure looks like; a vargr is not one.
        assert!(
            draugr.height > draugr.width,
            "a draugr stopped being an upright figure"
        );
        assert!(
            vargr.height < vargr.width * 1.5,
            "a vargr is not low and long"
        );

        // The same shape the client draws a player's capsule at, so the third silhouette
        // in the comparison is pinned rather than remembered.
        assert_eq!(draugr.width, crate::player::constants::PLAYER_WIDTH);
        assert_eq!(draugr.height, crate::player::constants::PLAYER_HEIGHT);
    }

    /// Windup, recovery and the hit flash read on a vargr exactly as on a draugr.
    ///
    /// One code path, and this is what says so: the pose is a function of the *action*
    /// alone — the server's word — and the flash is a function of an authoritative health
    /// decrease. Neither consults the kind, and neither may, because the windup and
    /// recovery durations belong to the row in `species.go` and a second copy of them here
    /// would be a second thing to keep in step.
    #[test]
    fn windup_and_recovery_read_on_a_vargr_exactly_as_on_a_draugr() {
        let mut app = headless();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(1)));

        deliver(&mut app, 1, vec![vargr(900, 3.0, 35, MobAction::Windup)]);
        app.update();
        assert_eq!(bodies(&mut app), vec![(900, 35, MobAction::Windup)]);
        assert_eq!(
            flashing(&mut app),
            0,
            "the first sight of a body is not a hit"
        );

        // The lean is the action's, and the action is the server's: it is held until a
        // newer snapshot changes it, whatever local time does.
        app.insert_resource(TimeUpdateStrategy::ManualDuration(INTERVAL));
        for _ in 0..5 {
            app.update();
            assert_eq!(bodies(&mut app), vec![(900, 35, MobAction::Windup)]);
        }
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(1)));

        deliver(&mut app, 2, vec![vargr(900, 3.0, 35, MobAction::Recovery)]);
        app.update();
        assert_eq!(bodies(&mut app), vec![(900, 35, MobAction::Recovery)]);
        assert_eq!(flashing(&mut app), 0, "an unchanged health flashed");

        // And a decrease flashes, through the one shared material swap.
        deliver(&mut app, 3, vec![vargr(900, 3.0, 12, MobAction::Chase)]);
        app.update();
        assert_eq!(flashing(&mut app), 1, "a vargr taking a hit did not flash");
    }

    /// Zero health is not death for a vargr either, and the two kinds coexist.
    #[test]
    fn a_snapshot_carrying_both_kinds_draws_both() {
        let mut app = headless();
        deliver(
            &mut app,
            1,
            vec![
                draugr(900, 3.0, 60, MobAction::Idle),
                vargr(901, 9.0, 0, MobAction::Chase),
                vargr(902, 12.0, 35, MobAction::Windup),
            ],
        );
        app.update();

        assert_eq!(
            kinds(&mut app),
            vec![
                (900, MobKind::Draugr),
                (901, MobKind::Vargr),
                (902, MobKind::Vargr),
            ]
        );
        assert_eq!(
            bodies(&mut app),
            vec![
                (900, 60, MobAction::Idle),
                (901, 0, MobAction::Chase),
                (902, 35, MobAction::Windup),
            ],
            "a vargr at zero health was despawned by this client"
        );
    }
}
