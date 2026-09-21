//! Cosmetic castle fixtures and a bounded pool of shadowed point lights.
//! Poses, roots and lifetime come from the authoritative static-prop mechanism.
mod fixture_render;
mod models;
mod selection;
mod shadows;

use super::camera::WorldCamera;
use super::static_props::{StaticPropRoot, StaticPropSolids, StaticPropsSet};
use crate::net::{BlockCoord, ChunkCoord, Session, StaticPropKind};
use crate::world::{ChunkStore, palette};
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy::render::renderer::RenderDevice;
use fixture_render::FixtureAssets;
use models::Fixture;
use selection::{Candidate, Selection};

#[derive(Resource, Default)]
struct ShadowLayers(u32);

#[derive(Resource, Default)]
struct CandlePool {
    selection: Selection,
    slots: Vec<Entity>,
}

#[derive(Component)]
struct CandlePoint;

struct LightPose {
    id: u64,
    origin: Vec3,
    kind: Fixture,
}

pub(super) fn register(app: &mut App) {
    shadows::install_shadow_maps(app);
    app.init_resource::<ShadowLayers>()
        .init_resource::<CandlePool>()
        .add_systems(Startup, create_assets)
        .add_systems(
            Update,
            (
                refresh_shadow_layers,
                attach_fixtures,
                ApplyDeferred,
                sync_pool,
                fixture_render::animate_flames,
            )
                .chain()
                .after(StaticPropsSet::Sync),
        );
}

pub(super) fn sun_cascades() -> bevy::light::CascadeShadowConfig {
    shadows::sun_cascades()
}

fn create_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(FixtureAssets::build(&mut meshes, &mut materials));
}

fn fixture_kind(kind: StaticPropKind) -> Option<Fixture> {
    match kind {
        StaticPropKind::WallSconce => Some(Fixture::Wall),
        StaticPropKind::FloorCandelabrum => Some(Fixture::Floor),
        StaticPropKind::TableCandelabrum => Some(Fixture::Table),
        _ => None,
    }
}

fn refresh_shadow_layers(device: Option<Res<RenderDevice>>, mut layers: ResMut<ShadowLayers>) {
    layers.0 = device.map_or(0, |device| device.limits().max_texture_array_layers);
}

fn attach_fixtures(
    mut commands: Commands,
    assets: Res<FixtureAssets>,
    roots: Query<(Entity, &StaticPropRoot), Added<StaticPropRoot>>,
) {
    for (entity, root) in &roots {
        if let Some(kind) = fixture_kind(root.0.kind) {
            assets.attach(&mut commands, entity, kind, root.0.prop_id);
        }
    }
}

#[derive(SystemParam)]
struct LightingWorld<'w> {
    session: Option<Res<'w, Session>>,
    chunks: Option<Res<'w, ChunkStore>>,
    solids: Res<'w, StaticPropSolids>,
    assets: Res<'w, FixtureAssets>,
    layers: Res<'w, ShadowLayers>,
    time: Res<'w, Time>,
}

type EyeFilter = (With<WorldCamera>, Without<CandlePoint>);

fn sync_pool(
    mut commands: Commands,
    world: LightingWorld<'_>,
    mut pool: ResMut<CandlePool>,
    cameras: Query<(&Transform, &Projection), EyeFilter>,
    roots: Query<(&StaticPropRoot, &Transform, &Visibility), Without<CandlePoint>>,
    mut lights: Query<(&mut PointLight, &mut Transform, &mut Visibility), With<CandlePoint>>,
    other_lights: Query<&PointLight, Without<CandlePoint>>,
) {
    let Some(session) = world.session.as_deref() else {
        for entity in pool.slots.drain(..) {
            commands.entity(entity).despawn();
        }
        pool.selection = Selection::default();
        return;
    };
    let Some((eye, projection)) = cameras.iter().next() else {
        for (mut light, _, mut visibility) in &mut lights {
            light.intensity = 0.0;
            light.shadow_maps_enabled = false;
            *visibility = Visibility::Hidden;
        }
        pool.selection = Selection::default();
        return;
    };
    let layers = shadows::remaining_point_layers(
        world.layers.0,
        other_lights
            .iter()
            .filter(|light| light.shadow_maps_enabled)
            .count(),
    );
    let mut poses = Vec::new();
    let mut candidates = Vec::new();
    for (root, transform, visibility) in &roots {
        let Some(kind) = fixture_kind(root.0.kind) else {
            continue;
        };
        if *visibility == Visibility::Hidden {
            continue;
        }
        let origin = transform.transform_point(world.assets.light_origin(kind));
        let distance = eye.translation.distance(origin);
        if !distance.is_finite() || distance > selection::RANGE {
            continue;
        }
        poses.push(LightPose {
            id: root.0.prop_id,
            origin,
            kind,
        });
        candidates.push(Candidate {
            id: root.0.prop_id,
            distance,
            visible: in_frustum(eye, projection, origin),
        });
    }
    let seconds = world.time.elapsed_secs_f64();
    pool.selection
        .refine_visibility(seconds, &mut candidates, |id| {
            let Some(pose) = poses.iter().find(|pose| pose.id == id) else {
                return false;
            };
            world.chunks.as_deref().is_some_and(|chunks| {
                voxel_sight_clear(
                    chunks,
                    usize::from(session.0.chunk_size),
                    eye.translation,
                    pose.origin,
                ) && world.solids.line_is_clear(eye.translation, pose.origin)
            })
        });
    pool.selection.update(seconds, layers, &candidates);
    // A missing root and reduced device capacity apply on every frame, not only a ranking tick.
    pool.slots.retain(|entity| lights.get(*entity).is_ok());
    let selected = pool.selection.ids.clone();
    for (index, id) in selected.into_iter().enumerate() {
        let Some(pose) = poses.iter().find(|pose| pose.id == id) else {
            continue;
        };
        let (intensity, range) = fixture_render::light_settings(pose.kind);
        if let Some(entity) = pool.slots.get(index) {
            if let Ok((mut light, mut transform, mut visibility)) = lights.get_mut(*entity) {
                light.intensity = intensity;
                light.range = range;
                light.shadow_maps_enabled = true;
                transform.translation = pose.origin;
                *visibility = Visibility::Inherited;
            }
        } else {
            let entity = commands
                .spawn((
                    CandlePoint,
                    PointLight {
                        color: Color::srgb(1.0, 0.66, 0.32),
                        intensity,
                        range,
                        radius: 0.04,
                        shadow_maps_enabled: true,
                        ..default()
                    },
                    Transform::from_translation(pose.origin),
                    Visibility::Inherited,
                ))
                .id();
            pool.slots.push(entity);
        }
    }
    let active = pool.selection.ids.len();
    for entity in pool.slots.iter().skip(active) {
        if let Ok((mut light, _, mut visibility)) = lights.get_mut(*entity) {
            light.intensity = 0.0;
            light.shadow_maps_enabled = false;
            *visibility = Visibility::Hidden;
        }
    }
}

fn in_frustum(eye: &Transform, projection: &Projection, position: Vec3) -> bool {
    let local = eye.compute_affine().inverse().transform_point3(position);
    let clip = projection.get_clip_from_view() * local.extend(1.0);
    clip.is_finite()
        && clip.w > 0.0
        && clip.x.abs() <= clip.w
        && clip.y.abs() <= clip.w
        && clip.z >= 0.0
        && clip.z <= clip.w
}

fn voxel_sight_clear(chunks: &ChunkStore, size: usize, from: Vec3, to: Vec3) -> bool {
    let delta = to - from;
    let length = delta.length();
    let Ok(edge) = i32::try_from(size) else {
        return false;
    };
    if edge <= 0
        || !from.is_finite()
        || !to.is_finite()
        || !length.is_finite()
        || length > selection::RANGE
    {
        return false;
    }
    if length <= f32::EPSILON {
        return true;
    }
    super::target::raycast_blocks(from, delta, length, |voxel| {
        let coord = ChunkCoord {
            cx: voxel.x.div_euclid(edge),
            cy: voxel.y.div_euclid(edge),
            cz: voxel.z.div_euclid(edge),
        };
        if chunks.get(coord).is_none_or(|chunk| chunk.size() != size) {
            return palette::STONE;
        }
        chunks.block_at(
            BlockCoord {
                x: voxel.x,
                y: voxel.y,
                z: voxel.z,
            },
            size,
        )
    })
    .is_none()
}

pub(super) fn reset_world(world: &mut World) {
    crate::world::transition::despawn::<CandlePoint>(world);
    crate::world::transition::reset::<CandlePool>(world);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{ANY_TOKEN, Facing, SessionParams, StaticPropState};

    fn app() -> App {
        let mut app = App::new();
        let mut meshes = Assets::<Mesh>::default();
        let mut materials = Assets::<StandardMaterial>::default();
        let assets = FixtureAssets::build(&mut meshes, &mut materials);
        app.insert_resource(assets)
            .insert_resource(meshes)
            .insert_resource(materials)
            .init_resource::<StaticPropSolids>()
            .init_resource::<CandlePool>()
            .init_resource::<Time>()
            .insert_resource(ShadowLayers(48))
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
            }))
            .add_systems(Update, (attach_fixtures, ApplyDeferred, sync_pool).chain());
        app.world_mut().spawn((
            WorldCamera,
            Transform::from_xyz(0.0, 2.0, 4.0),
            Projection::default(),
        ));
        for id in 1..=12 {
            app.world_mut().spawn((
                StaticPropRoot(StaticPropState {
                    prop_id: id,
                    kind: StaticPropKind::FloorCandelabrum,
                    origin: BlockCoord {
                        x: 0,
                        y: 0,
                        z: -(id as i32),
                    },
                    facing: Facing::North,
                    variant: 0,
                }),
                Transform::from_xyz(0.5, 0.0, -(id as f32) + 0.5),
                Visibility::Inherited,
            ));
        }
        app
    }

    fn active(app: &mut App) -> usize {
        let world = app.world_mut();
        world
            .query_filtered::<&PointLight, With<CandlePoint>>()
            .iter(world)
            .filter(|light| light.shadow_maps_enabled && light.intensity > 0.0)
            .count()
    }

    #[test]
    fn pool_reserves_other_shadows_and_shrinks_without_waiting_for_ranking() {
        let mut app = app();
        app.world_mut().resource_mut::<ShadowLayers>().0 = (selection::MAX_LIGHTS * 6) as u32;
        app.update();
        assert_eq!(active(&mut app), selection::MAX_LIGHTS);
        app.world_mut().spawn(PointLight {
            shadow_maps_enabled: true,
            ..default()
        });
        app.update();
        assert_eq!(active(&mut app), selection::MAX_LIGHTS - 1);
        app.world_mut().resource_mut::<ShadowLayers>().0 = 12;
        app.update();
        assert_eq!(active(&mut app), 1);
        app.world_mut().resource_mut::<ShadowLayers>().0 = 5;
        app.update();
        assert_eq!(active(&mut app), 0);
        assert!(app.world().resource::<CandlePool>().slots.len() <= selection::MAX_LIGHTS);
    }

    #[test]
    fn removed_hidden_roots_and_world_exit_stop_owned_lights_immediately() {
        let mut app = app();
        app.update();
        let first = app.world().resource::<CandlePool>().selection.ids[0];
        let world = app.world_mut();
        let entity = world
            .query::<(Entity, &StaticPropRoot)>()
            .iter(world)
            .find(|(_, root)| root.0.prop_id == first)
            .unwrap()
            .0;
        world.entity_mut(entity).despawn();
        app.update();
        assert_eq!(active(&mut app), selection::MAX_LIGHTS - 1);
        assert!(
            !app.world()
                .resource::<CandlePool>()
                .selection
                .ids
                .contains(&first)
        );
        let world = app.world_mut();
        for mut visibility in world
            .query_filtered::<&mut Visibility, With<StaticPropRoot>>()
            .iter_mut(world)
        {
            *visibility = Visibility::Hidden;
        }
        app.update();
        assert_eq!(active(&mut app), 0);
        app.world_mut().remove_resource::<Session>();
        app.update();
        assert!(app.world().resource::<CandlePool>().slots.is_empty());
        assert_eq!(
            app.world_mut()
                .query_filtered::<Entity, With<CandlePoint>>()
                .iter(app.world())
                .count(),
            0
        );
    }

    #[test]
    fn frustum_uses_camera_rotation_and_rejects_behind_eye() {
        let eye = Transform::from_xyz(5.0, 2.0, 3.0).looking_at(Vec3::new(15.0, 2.0, 3.0), Vec3::Y);
        let projection = Projection::default();
        assert!(in_frustum(&eye, &projection, Vec3::new(10.0, 2.0, 3.0)));
        assert!(!in_frustum(&eye, &projection, Vec3::new(0.0, 2.0, 3.0)));
        assert!(!in_frustum(&eye, &projection, Vec3::new(5.0, 20.0, 3.0)));
    }
    #[test]
    fn visibility_uses_grille_members_and_fails_closed_on_unloaded_chunks() {
        let mut chunks = ChunkStore::default();
        let from = Vec3::new(2.5, 2.5, 1.0);
        let to = Vec3::new(2.5, 2.5, 6.0);
        assert!(!voxel_sight_clear(&chunks, 8, from, to));
        let coord = ChunkCoord {
            cx: 0,
            cy: 0,
            cz: 0,
        };
        let mut chunk = crate::world::VoxelChunk::all_air(8);
        chunk.set(2, 2, 3, palette::IRON_GRILLE_X);
        chunks.insert(coord, chunk.clone());
        assert!(voxel_sight_clear(&chunks, 8, from, to));
        assert!(!voxel_sight_clear(
            &chunks,
            8,
            from - Vec3::X * 0.25,
            to - Vec3::X * 0.25
        ));
        chunk.set(2, 2, 3, palette::STONE);
        chunks.insert(coord, chunk);
        assert!(!voxel_sight_clear(&chunks, 8, from, to));
        assert!(!voxel_sight_clear(&chunks, 16, from, to));
    }
    #[test]
    fn production_sync_attaches_same_frame_and_owns_recursive_fixture_removal() {
        let mut donor = app();
        let session = donor.world_mut().remove_resource::<Session>().unwrap();
        let mut app = App::new();
        app.insert_resource(session)
            .init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StandardMaterial>>()
            .init_resource::<super::super::SnapshotBuffer>()
            .init_resource::<super::super::InputMode>()
            .init_resource::<Time>();
        super::super::static_props::register(&mut app);
        register(&mut app);
        let pose = StaticPropState {
            prop_id: 42,
            kind: StaticPropKind::TableCandelabrum,
            origin: BlockCoord { x: 2, y: 1, z: 2 },
            facing: Facing::East,
            variant: 0,
        };
        assert!(
            app.world_mut()
                .resource_mut::<super::super::SnapshotBuffer>()
                .accept(
                    crate::net::Snapshot {
                        server_tick: 1,
                        static_props: vec![pose],
                        ..default()
                    },
                    std::time::Instant::now(),
                )
        );
        app.update();
        let world = app.world_mut();
        assert_eq!(world.query::<&StaticPropRoot>().iter(world).count(), 1);
        assert_eq!(
            world
                .query::<&fixture_render::CandleFlame>()
                .iter(world)
                .count(),
            3
        );
        assert!(world.resource_mut::<super::super::SnapshotBuffer>().accept(
            crate::net::Snapshot {
                server_tick: 2,
                static_props: vec![],
                ..default()
            },
            std::time::Instant::now(),
        ));
        app.update();
        let world = app.world_mut();
        assert_eq!(world.query::<&StaticPropRoot>().iter(world).count(), 0);
        assert_eq!(
            world
                .query::<&fixture_render::CandleFlame>()
                .iter(world)
                .count(),
            0
        );
        assert_eq!(world.query::<&Mesh3d>().iter(world).count(), 0);
    }
}
