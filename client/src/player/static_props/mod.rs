//! Authoritative static furniture: one root per descriptor, shared meshes and exact
//! solid geometry used only to cut off presentation rays. The server owns movement.
mod models;

use super::camera::WorldCamera;
use super::{ApplyInputMode, ApplySnapshots, InputMode, SnapshotBuffer};
use crate::net::{Facing, Session, StaticPropKind, StaticPropState};
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

/// Lighting attaches fixture children to these same roots; removal owns all children.
#[derive(Component, Debug)]
pub(super) struct StaticPropRoot(pub StaticPropState);

/// Deferred root commands are visible to lighting systems ordered after this set.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum StaticPropsSet {
    Sync,
}

#[derive(Resource)]
struct PropVisuals(models::PropModels);

/// Exact server/model bounds, rebuilt only when the authoritative descriptor set changes.
/// The decoder caps roots at 256; the catalogue has at most 6 solid members per root.
#[derive(Resource, Default)]
pub(super) struct StaticPropSolids {
    poses: Vec<StaticPropState>,
    boxes: Vec<(Vec3, Vec3)>,
}

impl StaticPropSolids {
    fn update(&mut self, poses: &[StaticPropState]) {
        if self.poses == poses {
            return;
        }
        self.poses.clear();
        self.poses.extend_from_slice(poses);
        self.boxes.clear();
        for pose in poses {
            for &(min, max) in models::solid_members(pose.kind) {
                self.boxes.push(placed_bounds(
                    *pose,
                    Vec3::from_array(min),
                    Vec3::from_array(max),
                ));
            }
        }
    }

    pub(super) fn nearest_hit(&self, origin: Vec3, direction: Vec3, limit: f32) -> Option<f32> {
        self.boxes
            .iter()
            .filter_map(|&(min, max)| super::structures::ray_box_entry(origin, direction, min, max))
            .filter(|distance| *distance <= limit)
            .min_by(f32::total_cmp)
    }

    pub(super) fn line_is_clear(&self, from: Vec3, to: Vec3) -> bool {
        let delta = to - from;
        self.nearest_hit(from, delta, delta.length()).is_none()
    }
}

pub(super) fn register(app: &mut App) {
    app.init_resource::<StaticPropSolids>()
        .add_systems(Startup, create_visuals)
        .add_systems(
            Update,
            (reconcile, ApplyDeferred)
                .chain()
                .in_set(StaticPropsSet::Sync)
                .after(ApplySnapshots)
                .after(ApplyInputMode)
                .before(super::target::AimBlocks)
                .before(super::structures::AimStructures),
        );
}

fn create_visuals(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(PropVisuals(models::PropModels::build(
        &mut meshes,
        &mut materials,
    )));
}

#[derive(SystemParam)]
struct PropWorld<'w> {
    buffer: Res<'w, SnapshotBuffer>,
    session: Option<Res<'w, Session>>,
    mode: Res<'w, InputMode>,
    visuals: Option<Res<'w, PropVisuals>>,
    solids: ResMut<'w, StaticPropSolids>,
}

fn reconcile(
    state: PropWorld<'_>,
    cameras: Query<&Transform, With<WorldCamera>>,
    mut existing: Query<(Entity, &StaticPropRoot, &mut Visibility)>,
    mut commands: Commands,
) {
    let PropWorld {
        buffer,
        session,
        mode,
        visuals,
        mut solids,
    } = state;
    let poses = if session.is_some() && visuals.is_some() {
        buffer.static_props()
    } else {
        &[]
    };
    solids.update(poses);
    let eye = cameras.iter().next().map(|camera| camera.translation);
    let mut kept = Vec::with_capacity(poses.len());
    for (entity, root, mut visibility) in &mut existing {
        if !poses.contains(&root.0) {
            commands.entity(entity).despawn();
            continue;
        }
        let next = prop_visibility(root.0, *mode, eye);
        if *visibility != next {
            *visibility = next;
        }
        kept.push(root.0.prop_id);
    }
    let Some(visuals) = visuals.as_deref() else {
        return;
    };
    for pose in poses {
        if kept.contains(&pose.prop_id) {
            continue;
        }
        let mut root = commands.spawn((
            StaticPropRoot(*pose),
            Transform::from_translation(prop_origin(*pose))
                .with_rotation(prop_rotation(pose.facing)),
            prop_visibility(*pose, *mode, eye),
        ));
        root.with_children(|children| {
            for part in visuals.0.parts(pose.kind, pose.variant) {
                children.spawn((
                    Mesh3d(part.mesh.clone()),
                    MeshMaterial3d(part.material.clone()),
                    Transform::default(),
                ));
            }
        });
        kept.push(pose.prop_id);
    }
}

fn prop_origin(pose: StaticPropState) -> Vec3 {
    Vec3::new(
        pose.origin.x as f32 + 0.5,
        pose.origin.y as f32,
        pose.origin.z as f32 + 0.5,
    )
}
fn prop_rotation(facing: Facing) -> Quat {
    let turn = match facing {
        Facing::North => 0.,
        Facing::East => 1.,
        Facing::South => 2.,
        Facing::West => 3.,
    };
    Quat::from_rotation_y(-turn * std::f32::consts::FRAC_PI_2)
}
fn placed_bounds(pose: StaticPropState, min: Vec3, max: Vec3) -> (Vec3, Vec3) {
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for x in [min.x, max.x] {
        for z in [min.z, max.z] {
            let (x, z) = match pose.facing {
                Facing::North => (x, z),
                Facing::East => (-z, x),
                Facing::South => (-x, -z),
                Facing::West => (z, -x),
            };
            lo = lo.min(Vec3::new(x, min.y, z));
            hi = hi.max(Vec3::new(x, max.y, z));
        }
    }
    (lo + prop_origin(pose), hi + prop_origin(pose))
}
fn prop_visibility(pose: StaticPropState, mode: InputMode, eye: Option<Vec3>) -> Visibility {
    if matches!(mode, InputMode::Inventory | InputMode::Menu) {
        return Visibility::Hidden;
    }
    let fixture = matches!(
        pose.kind,
        StaticPropKind::WallSconce
            | StaticPropKind::FloorCandelabrum
            | StaticPropKind::TableCandelabrum
    );
    if !fixture
        && models::solid_members(pose.kind).is_empty()
        && eye.is_some_and(|eye| eye.distance_squared(prop_origin(pose)) > 64. * 64.)
    {
        return Visibility::Hidden;
    }
    Visibility::Inherited
}

pub(super) fn reset_world(world: &mut World) {
    crate::world::transition::despawn::<StaticPropRoot>(world);
    crate::world::transition::reset::<StaticPropSolids>(world);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{ANY_TOKEN, BlockCoord, SessionParams, Snapshot};
    use std::time::Instant;

    fn pose(id: u64, kind: StaticPropKind) -> StaticPropState {
        StaticPropState {
            prop_id: id,
            kind,
            origin: BlockCoord { x: 0, y: 0, z: 0 },
            facing: Facing::North,
            variant: 0,
        }
    }

    fn app() -> App {
        let mut app = App::new();
        app.init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StandardMaterial>>()
            .init_resource::<SnapshotBuffer>()
            .init_resource::<InputMode>()
            .insert_resource(Session(SessionParams {
                clock: Default::default(),
                entity_id: 7,
                spawn: [0.0; 3],
                world_seed: 1,
                tick_rate: 20,
                chunk_size: 32,
                view_distance: 8,
                inventory_slots: 37,
                hotbar_slots: 9,
                equipment_slots: 4,
                player_token: ANY_TOKEN,
                voice_range_blocks: 0.0,
            }));
        register(&mut app);
        app
    }

    fn accept(app: &mut App, tick: u32, poses: Vec<StaticPropState>) -> bool {
        app.world_mut().resource_mut::<SnapshotBuffer>().accept(
            Snapshot {
                server_tick: tick,
                static_props: poses,
                ..default()
            },
            Instant::now(),
        )
    }

    fn roots(app: &mut App) -> Vec<(Entity, StaticPropState)> {
        let mut roots: Vec<_> = app
            .world_mut()
            .query::<(Entity, &StaticPropRoot)>()
            .iter(app.world())
            .map(|(entity, root)| (entity, root.0))
            .collect();
        roots.sort_unstable_by_key(|(_, pose)| pose.prop_id);
        roots
    }

    #[test]
    fn complete_snapshots_preserve_unchanged_roots_and_remove_recursive_children() {
        let mut app = app();
        let table = pose(1, StaticPropKind::BanquetTable);
        let fixture = pose(2, StaticPropKind::TableCandelabrum);
        assert!(accept(&mut app, 1, vec![table, fixture]));
        app.update();
        let initial = roots(&mut app);
        assert_eq!(initial.len(), 2);
        let fixture_root = initial
            .iter()
            .find(|(_, prop)| prop.prop_id == 2)
            .unwrap()
            .0;
        let child = app.world_mut().spawn(ChildOf(fixture_root)).id();
        let mesh_count = app.world().resource::<Assets<Mesh>>().len();
        let material_count = app.world().resource::<Assets<StandardMaterial>>().len();
        app.update();
        assert_eq!(roots(&mut app), initial);
        assert!(!accept(&mut app, 0, vec![]));
        app.update();
        assert_eq!(roots(&mut app), initial);
        assert!(accept(&mut app, 2, vec![table]));
        app.update();
        assert!(app.world().get_entity(child).is_err());
        assert_eq!(roots(&mut app).len(), 1);
        assert!(accept(&mut app, 3, vec![]));
        app.update();
        assert!(roots(&mut app).is_empty());
        assert!(app.world().resource::<StaticPropSolids>().boxes.is_empty());
        assert!(accept(&mut app, 4, vec![table]));
        app.update();
        assert_eq!(app.world().resource::<Assets<Mesh>>().len(), mesh_count);
        assert_eq!(
            app.world().resource::<Assets<StandardMaterial>>().len(),
            material_count
        );
    }

    #[test]
    fn changed_pose_replaces_root_and_world_reset_accepts_reused_identity() {
        let mut app = app();
        let mut table = pose(1, StaticPropKind::BanquetTable);
        accept(&mut app, 1, vec![table]);
        app.update();
        let old = roots(&mut app)[0].0;
        table.variant = 3;
        table.facing = Facing::East;
        accept(&mut app, 2, vec![table]);
        app.update();
        assert_ne!(roots(&mut app)[0].0, old);
        assert!(app.world().get_entity(old).is_err());
        super::super::reset_world(app.world_mut());
        assert!(roots(&mut app).is_empty());
        assert!(accept(&mut app, 1, vec![table]));
        app.update();
        assert_eq!(roots(&mut app)[0].1, table);
        app.world_mut().remove_resource::<Session>();
        app.update();
        assert!(roots(&mut app).is_empty());
        assert!(app.world().resource::<StaticPropSolids>().boxes.is_empty());
    }

    #[test]
    fn full_wire_capacity_has_bounded_shared_mesh_children() {
        let mut app = app();
        accept(
            &mut app,
            1,
            (1..=256)
                .map(|id| pose(id, StaticPropKind::BanquetTable))
                .collect(),
        );
        app.update();
        assert_eq!(roots(&mut app).len(), 256);
        let world = app.world_mut();
        assert!(
            world
                .query_filtered::<&Children, With<StaticPropRoot>>()
                .iter(world)
                .all(|children| children.len() <= 5)
        );
        assert!(world.query::<&Mesh3d>().iter(world).count() <= 256 * 5);
    }

    #[test]
    fn rays_respect_table_members_and_open_leg_gaps_in_every_turn() {
        for facing in [Facing::North, Facing::East, Facing::South, Facing::West] {
            let mut table = pose(1, StaticPropKind::BanquetTable);
            table.facing = facing;
            let mut solids = StaticPropSolids::default();
            solids.update(&[table]);
            let origin = prop_origin(table);
            let rotation = prop_rotation(facing);
            let gap = origin + rotation * Vec3::new(0.0, 0.3, -4.0);
            let direction = rotation * Vec3::Z;
            assert_eq!(solids.nearest_hit(gap, direction, 8.0), None);
            let tabletop = origin + rotation * Vec3::new(0.0, 0.95, -4.0);
            assert!(solids.nearest_hit(tabletop, direction, 8.0).is_some());
            assert!(
                solids.line_is_clear(origin + Vec3::Y * 1.62, origin + Vec3::new(0.0, 1.62, 4.0))
            );
        }
    }
    #[test]
    fn a_visible_voxel_corner_survives_an_occluded_centre() {
        let mut chair = pose(1, StaticPropKind::Chair);
        chair.origin.x = 1;
        let mut solids = StaticPropSolids::default();
        solids.update(&[chair]);
        let eye = Vec3::new(0.0, 0.5, 0.5);
        let centre = Vec3::new(3.5, 0.5, 0.5);
        assert!(!solids.line_is_clear(eye, centre));
        let direction = Vec3::new(3.0, 0.9, 0.5) - eye;
        assert!(solids.nearest_hit(eye, direction, 4.0).is_none());
        assert_eq!(
            super::super::target::raycast(eye, direction, 4.0, |voxel| voxel
                == IVec3::new(3, 0, 0))
            .map(|hit| hit.block),
            Some(IVec3::new(3, 0, 0))
        );
    }
    #[test]
    fn distance_culling_hides_only_cosmetic_details_and_restores_them_nearby() {
        let near = Some(Vec3::ZERO);
        let far = Some(Vec3::splat(100.0));
        let rug = pose(1, StaticPropKind::Rug);
        assert_eq!(
            prop_visibility(rug, InputMode::Playing, far),
            Visibility::Hidden
        );
        assert_eq!(
            prop_visibility(rug, InputMode::Playing, near),
            Visibility::Inherited
        );
        for kind in [
            StaticPropKind::BanquetTable,
            StaticPropKind::Chair,
            StaticPropKind::WallSconce,
        ] {
            assert_eq!(
                prop_visibility(pose(1, kind), InputMode::Playing, far),
                Visibility::Inherited
            );
        }
    }
}
