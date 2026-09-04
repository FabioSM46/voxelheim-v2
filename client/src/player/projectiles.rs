//! Authoritative projectiles, drawn only between the two newest snapshots.
//!
//! Position is sampled by [`SnapshotBuffer`]. Velocity never advances a body: it
//! only points the arrow and the orb trail. The newest snapshot is the complete
//! existence set, so a hit removes a projectile on that frame rather than leaving
//! a locally simulated body to contradict it.

use std::collections::HashSet;
use std::f32::consts::FRAC_PI_2;
use std::time::{Duration, Instant};

use bevy::prelude::*;

use super::interpolate::{InterpolatedProjectile, SnapshotBuffer};
use super::items::{ITEM_BONE, item_linear_rgba};
use super::{ApplySnapshots, InputMode, merge_all};
use crate::net::{ProjectileKind, Session};

/// Tip to fletching, in blocks. The mesh is authored along -Z so `looking_to`
/// points its head in the newest non-zero velocity direction.
pub(super) const ARROW_LENGTH: f32 = 0.8;
const ARROW_SHAFT_WIDTH: f32 = 0.035;
const ARROW_HEAD_LENGTH: f32 = 0.12;
const ARROW_HEAD_RADIUS: f32 = 0.07;
const ARROW_FLETCH_LENGTH: f32 = 0.10;
const ARROW_FLETCH_WIDTH: f32 = 0.09;
const ARROW_FLETCH_THICKNESS: f32 = 0.012;

const ORB_DIAMETER: f32 = 0.3;
const TRAIL_OFFSETS: [f32; 2] = [0.22, 0.38];
const TRAIL_SCALES: [f32; 2] = [0.62, 0.38];
/// Full-screen modes whose UI owns the view instead of the 3D world.
const HIDDEN_INPUT_MODES: [InputMode; 2] = [InputMode::Inventory, InputMode::Menu];

const ORB_COLOUR: Color = Color::linear_rgb(0.12, 0.95, 0.32);
const ORB_EMISSIVE: LinearRgba = LinearRgba::rgb(1.0, 8.0, 2.4);

/// One set of handles per kind, shared by every authoritative body.
#[derive(Resource, Debug)]
struct ProjectileVisuals {
    arrow_mesh: Handle<Mesh>,
    orb_mesh: Handle<Mesh>,
    arrow_material: Handle<StandardMaterial>,
    orb_material: Handle<StandardMaterial>,
    /// Two additive fades shared by every orb trail; no per-projectile asset grows.
    trail_materials: [Handle<StandardMaterial>; 2],
}

impl ProjectileVisuals {
    fn mesh(&self, kind: ProjectileKind) -> Handle<Mesh> {
        match kind {
            ProjectileKind::Arrow => self.arrow_mesh.clone(),
            ProjectileKind::EnergyOrb => self.orb_mesh.clone(),
        }
    }

    fn material(&self, kind: ProjectileKind) -> Handle<StandardMaterial> {
        match kind {
            ProjectileKind::Arrow => self.arrow_material.clone(),
            ProjectileKind::EnergyOrb => self.orb_material.clone(),
        }
    }
}

/// One server-minted projectile body. `heading` is presentation memory only: it
/// keeps a stuck arrow pointing where its last non-zero velocity pointed.
#[derive(Component, Debug, Clone, Copy)]
struct ProjectileBody {
    entity_id: u64,
    kind: ProjectileKind,
    heading: Vec3,
}

#[derive(Component, Debug, Clone, Copy)]
struct OrbTrail {
    owner: Entity,
    index: usize,
}

pub(super) struct ProjectilesPlugin;

impl Plugin for ProjectilesPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, create_visuals).add_systems(
            Update,
            apply_snapshots
                .after(super::ingest_snapshots)
                .in_set(ApplySnapshots),
        );
    }
}

fn create_visuals(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let [r, g, b, a] = item_linear_rgba(ITEM_BONE);
    let arrow_material = materials.add(StandardMaterial {
        base_color: Color::linear_rgba(r, g, b, a),
        perceptual_roughness: 0.88,
        ..default()
    });
    let orb_material = materials.add(StandardMaterial {
        base_color: ORB_COLOUR,
        emissive: ORB_EMISSIVE,
        perceptual_roughness: 0.35,
        ..default()
    });
    let trail_materials = [0.42, 0.18].map(|alpha| {
        materials.add(StandardMaterial {
            base_color: Color::linear_rgba(0.12, 0.95, 0.32, alpha),
            emissive: ORB_EMISSIVE * alpha,
            alpha_mode: AlphaMode::Add,
            ..default()
        })
    });

    commands.insert_resource(ProjectileVisuals {
        arrow_mesh: meshes.add(arrow_mesh()),
        orb_mesh: meshes.add(Mesh::from(Sphere::new(ORB_DIAMETER / 2.0))),
        arrow_material,
        orb_material,
        trail_materials,
    });
}

/// A shaft, bone point and crossed fletching notch inside one exact 0.8-block span.
fn arrow_mesh() -> Mesh {
    let shaft_length = ARROW_LENGTH - ARROW_HEAD_LENGTH - ARROW_FLETCH_LENGTH;
    let head_centre = -ARROW_LENGTH / 2.0 + ARROW_HEAD_LENGTH / 2.0;
    let shaft_centre = head_centre + ARROW_HEAD_LENGTH / 2.0 + shaft_length / 2.0;
    let fletch_centre = ARROW_LENGTH / 2.0 - ARROW_FLETCH_LENGTH / 2.0;

    let mut arrow = Mesh::from(Cuboid::from_size(Vec3::new(
        ARROW_SHAFT_WIDTH,
        ARROW_SHAFT_WIDTH,
        shaft_length,
    )))
    .translated_by(Vec3::Z * shaft_centre);
    let head = Mesh::from(Cone::new(ARROW_HEAD_RADIUS, ARROW_HEAD_LENGTH))
        .rotated_by(Quat::from_rotation_x(-FRAC_PI_2))
        .translated_by(Vec3::Z * head_centre);
    let vertical_fletch = Mesh::from(Cuboid::from_size(Vec3::new(
        ARROW_FLETCH_THICKNESS,
        ARROW_FLETCH_WIDTH,
        ARROW_FLETCH_LENGTH,
    )))
    .translated_by(Vec3::Z * fletch_centre);
    let horizontal_fletch = Mesh::from(Cuboid::from_size(Vec3::new(
        ARROW_FLETCH_WIDTH,
        ARROW_FLETCH_THICKNESS,
        ARROW_FLETCH_LENGTH,
    )))
    .translated_by(Vec3::Z * fletch_centre);
    merge_all(
        &mut arrow,
        [head, vertical_fletch, horizontal_fletch],
        "arrow",
    );
    arrow
}

fn apply_snapshots(
    buffer: Res<SnapshotBuffer>,
    session: Option<Res<Session>>,
    mode: Res<InputMode>,
    visuals: Option<Res<ProjectileVisuals>>,
    mut existing: Query<(Entity, &mut ProjectileBody, &mut Transform, &mut Visibility)>,
    mut trails: Query<(&OrbTrail, &mut Transform), Without<ProjectileBody>>,
    mut commands: Commands,
) {
    let (Some(session), Some(visuals)) = (session, visuals) else {
        return;
    };
    let interval = Duration::from_secs(1) / u32::from(session.0.tick_rate);
    let drawn = buffer.sample_projectiles(Instant::now(), interval);
    let mut placed = HashSet::with_capacity(drawn.len());
    let visibility = projectile_visibility(*mode);
    let mut trail_updates = Vec::new();

    for (entity, mut body, mut transform, mut current_visibility) in &mut existing {
        match drawn
            .iter()
            .find(|(entity_id, _)| *entity_id == body.entity_id)
        {
            Some((_, state)) if state.kind == body.kind => {
                update_body(&mut body, &mut transform, state);
                if *current_visibility != visibility {
                    *current_visibility = visibility;
                }
                if body.kind == ProjectileKind::EnergyOrb {
                    trail_updates.push((entity, body.heading));
                }
                placed.insert(body.entity_id);
            }
            Some(_) => {
                // An id changing kind is a new presentation, not an arrow whose mesh is
                // silently replaced under it. Recreate it from the newest complete fact.
                commands.entity(entity).despawn();
            }
            None => commands.entity(entity).despawn(),
        }
    }

    for (owner, heading) in trail_updates {
        for (trail, mut transform) in &mut trails {
            if trail.owner == owner {
                *transform = trail_transform(trail.index, heading);
            }
        }
    }

    for (entity_id, state) in &drawn {
        if placed.insert(*entity_id) {
            spawn_projectile(&mut commands, &visuals, *entity_id, state, visibility);
        }
    }
}

fn update_body(
    body: &mut ProjectileBody,
    transform: &mut Transform,
    state: &InterpolatedProjectile,
) {
    transform.translation = state.pos;
    if state.vel.length_squared() > f32::EPSILON {
        body.heading = state.vel.normalize();
    }
    transform.rotation = match body.kind {
        ProjectileKind::Arrow => {
            Transform::IDENTITY
                .looking_to(body.heading, Vec3::Y)
                .rotation
        }
        ProjectileKind::EnergyOrb => Quat::IDENTITY,
    };
}

fn spawn_projectile(
    commands: &mut Commands,
    visuals: &ProjectileVisuals,
    entity_id: u64,
    state: &InterpolatedProjectile,
    visibility: Visibility,
) {
    let heading = if state.vel.length_squared() > f32::EPSILON {
        state.vel.normalize()
    } else {
        Vec3::NEG_Z
    };
    let mut transform = Transform::from_translation(state.pos);
    if state.kind == ProjectileKind::Arrow {
        transform.rotation = Transform::IDENTITY.looking_to(heading, Vec3::Y).rotation;
    }
    let owner = commands
        .spawn((
            ProjectileBody {
                entity_id,
                kind: state.kind,
                heading,
            },
            Mesh3d(visuals.mesh(state.kind)),
            MeshMaterial3d(visuals.material(state.kind)),
            transform,
            visibility,
        ))
        .id();

    if state.kind == ProjectileKind::EnergyOrb {
        commands.entity(owner).with_children(|parent| {
            for index in 0..TRAIL_OFFSETS.len() {
                parent.spawn((
                    OrbTrail { owner, index },
                    Mesh3d(visuals.orb_mesh.clone()),
                    MeshMaterial3d(visuals.trail_materials[index].clone()),
                    trail_transform(index, heading),
                ));
            }
        });
    }
}

fn trail_transform(index: usize, heading: Vec3) -> Transform {
    Transform {
        translation: -heading * TRAIL_OFFSETS[index],
        scale: Vec3::splat(TRAIL_SCALES[index]),
        ..default()
    }
}

fn projectile_visibility(mode: InputMode) -> Visibility {
    if HIDDEN_INPUT_MODES.contains(&mode) {
        Visibility::Hidden
    } else {
        Visibility::Visible
    }
}

#[cfg(test)]
mod tests {
    use bevy::asset::AssetPlugin;
    use bevy::mesh::VertexAttributeValues;

    use super::*;
    use crate::net::{ProjectileState, SessionParams, Snapshot, SnapshotInbox};
    use crate::player::PlayerPlugin;

    const LOCAL_ID: u64 = 7;
    const INTERVAL: Duration = Duration::from_millis(50);

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: LOCAL_ID,
            spawn: [0.5, 64.0, 0.5],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 8,
            inventory_slots: 37,
            hotbar_slots: 9,
            equipment_slots: 4,
            player_token: crate::net::ANY_TOKEN,
            voice_range_blocks: 0.0,
        })
    }

    fn projectile(
        entity_id: u64,
        kind: ProjectileKind,
        pos: [f32; 3],
        vel: [f32; 3],
    ) -> ProjectileState {
        ProjectileState {
            entity_id,
            kind,
            pos,
            vel,
        }
    }

    fn snapshot(server_tick: u32, projectiles: Vec<ProjectileState>) -> Snapshot {
        Snapshot {
            server_tick,
            projectiles,
            ..Default::default()
        }
    }

    fn headless_player() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .insert_resource(session())
            .add_plugins(PlayerPlugin);
        app
    }

    fn deliver(app: &mut App, snapshot: Snapshot, at: Instant) {
        app.world_mut()
            .resource_mut::<SnapshotInbox>()
            .push(snapshot, at);
    }

    fn bodies(app: &mut App) -> Vec<(u64, ProjectileKind, Transform, Visibility)> {
        let world = app.world_mut();
        let mut query = world.query::<(&ProjectileBody, &Transform, &Visibility)>();
        let mut found: Vec<_> = query
            .iter(world)
            .map(|(body, transform, visibility)| {
                (body.entity_id, body.kind, *transform, *visibility)
            })
            .collect();
        found.sort_by_key(|(id, _, _, _)| *id);
        found
    }

    #[test]
    fn bodies_of_one_kind_share_the_kind_mesh_and_material() {
        let mut app = headless_player();
        deliver(
            &mut app,
            snapshot(
                1,
                vec![
                    projectile(10, ProjectileKind::Arrow, [0.0; 3], [1.0, 0.0, 0.0]),
                    projectile(11, ProjectileKind::Arrow, [2.0, 0.0, 0.0], [1.0, 0.0, 0.0]),
                ],
            ),
            Instant::now(),
        );
        app.update();

        let world = app.world_mut();
        let mut query = world
            .query_filtered::<(&Mesh3d, &MeshMaterial3d<StandardMaterial>), With<ProjectileBody>>();
        let handles: Vec<_> = query
            .iter(world)
            .map(|(mesh, material)| (mesh.0.clone(), material.0.clone()))
            .collect();
        assert_eq!(handles.len(), 2);
        assert_eq!(handles[0], handles[1]);
    }

    #[test]
    fn the_newest_snapshot_despawns_exactly_the_id_it_omits() {
        let mut app = headless_player();
        let start = Instant::now();
        deliver(
            &mut app,
            snapshot(
                1,
                vec![
                    projectile(10, ProjectileKind::Arrow, [0.0; 3], [1.0, 0.0, 0.0]),
                    projectile(11, ProjectileKind::Arrow, [2.0, 0.0, 0.0], [1.0, 0.0, 0.0]),
                ],
            ),
            start,
        );
        app.update();
        deliver(
            &mut app,
            snapshot(
                2,
                vec![projectile(
                    11,
                    ProjectileKind::Arrow,
                    [3.0, 0.0, 0.0],
                    [1.0, 0.0, 0.0],
                )],
            ),
            start + INTERVAL,
        );
        app.update();

        let drawn = bodies(&mut app);
        assert_eq!(drawn.len(), 1);
        assert_eq!(drawn[0].0, 11);
    }

    #[test]
    fn an_arrow_points_along_velocity_and_keeps_its_last_heading_when_stuck() {
        let mut app = headless_player();
        let start = Instant::now();
        let velocity = Vec3::new(3.0, -1.0, 2.0).normalize();
        deliver(
            &mut app,
            snapshot(
                1,
                vec![projectile(
                    10,
                    ProjectileKind::Arrow,
                    [0.0; 3],
                    velocity.to_array(),
                )],
            ),
            start,
        );
        app.update();
        let flying = bodies(&mut app)[0].2.forward().as_vec3();
        assert!(flying.distance(velocity) < 1e-5);

        deliver(
            &mut app,
            snapshot(
                2,
                vec![projectile(
                    10,
                    ProjectileKind::Arrow,
                    [1.0, 0.0, 0.0],
                    [0.0; 3],
                )],
            ),
            start + INTERVAL,
        );
        app.update();
        let stuck = bodies(&mut app)[0].2.forward().as_vec3();
        assert!(stuck.distance(velocity) < 1e-5);
    }

    #[test]
    fn only_full_screen_ui_modes_hide_a_projectile_without_despawning_it() {
        let mut app = headless_player();
        deliver(
            &mut app,
            snapshot(
                1,
                vec![projectile(
                    10,
                    ProjectileKind::Arrow,
                    [0.0; 3],
                    [1.0, 0.0, 0.0],
                )],
            ),
            Instant::now(),
        );
        app.update();
        assert_eq!(bodies(&mut app)[0].3, Visibility::Visible);

        for mode in [InputMode::Chat, InputMode::Loot, InputMode::Vendor] {
            *app.world_mut().resource_mut::<InputMode>() = mode;
            app.update();
            let visible = bodies(&mut app);
            assert_eq!(visible.len(), 1);
            assert_eq!(
                visible[0].3,
                Visibility::Visible,
                "the centred {mode:?} panel hid the projectile"
            );
        }

        for mode in [InputMode::Inventory, InputMode::Menu] {
            *app.world_mut().resource_mut::<InputMode>() = mode;
            app.update();
            let hidden = bodies(&mut app);
            assert_eq!(hidden.len(), 1);
            assert_eq!(
                hidden[0].3,
                Visibility::Hidden,
                "the full-screen {mode:?} mode left the projectile drawn"
            );
        }
    }

    #[test]
    fn the_orb_glows_and_the_arrow_does_not() {
        let mut app = headless_player();
        app.update();
        let world = app.world();
        let visuals = world.resource::<ProjectileVisuals>();
        let materials = world.resource::<Assets<StandardMaterial>>();
        assert_eq!(
            materials
                .get(&visuals.arrow_material)
                .expect("arrow material")
                .emissive,
            LinearRgba::BLACK
        );
        assert_eq!(
            materials
                .get(&visuals.orb_material)
                .expect("orb material")
                .emissive,
            ORB_EMISSIVE
        );
        for trail in &visuals.trail_materials {
            assert_eq!(
                materials.get(trail).expect("trail material").alpha_mode,
                AlphaMode::Add
            );
        }
    }

    #[test]
    fn an_orb_has_two_fading_children_behind_its_velocity() {
        let mut app = headless_player();
        deliver(
            &mut app,
            snapshot(
                1,
                vec![projectile(
                    10,
                    ProjectileKind::EnergyOrb,
                    [0.0; 3],
                    [2.0, 0.0, 0.0],
                )],
            ),
            Instant::now(),
        );
        app.update();

        let world = app.world_mut();
        let mut query = world.query::<(&OrbTrail, &Transform)>();
        let mut trails: Vec<_> = query
            .iter(world)
            .map(|(trail, transform)| (trail.index, transform.translation, transform.scale.x))
            .collect();
        trails.sort_by_key(|(index, _, _)| *index);
        assert_eq!(trails.len(), 2);
        assert_eq!(trails[0].1, Vec3::NEG_X * TRAIL_OFFSETS[0]);
        assert_eq!(trails[1].1, Vec3::NEG_X * TRAIL_OFFSETS[1]);
        assert!(trails[0].2 > trails[1].2);
    }

    #[test]
    fn the_arrow_mesh_is_exactly_arrow_length_along_its_forward_axis() {
        let mesh = arrow_mesh();
        let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("the arrow must carry positions");
        };
        let (low, high) = positions
            .iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(low, high), point| {
                (low.min(point[2]), high.max(point[2]))
            });
        assert!((high - low - ARROW_LENGTH).abs() < 1e-6);
        assert!(positions.len() > Mesh::from(Cuboid::from_size(Vec3::ONE)).count_vertices());
    }
}
