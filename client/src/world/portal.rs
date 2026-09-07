//! Spectral thresholds drawn only where streamed voxels name a portal heart.
//! The index never generates terrain or decides where a crossing leads.
use std::collections::HashMap;
use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use super::{ChunkStore, VoxelChunk, palette};
use crate::net::{BlockCoord, ChunkCoord, Session};
use crate::player::WorldCamera;

const MAX_VISIBLE: usize = 8;
const DRAW_DISTANCE: f32 = 96.0;
const SPARKS: usize = 24;
const GRID_X: usize = 40;
const GRID_Y: usize = 24;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PortalSite {
    pub arch: BlockCoord,
    pub bottom: Vec3,
    pub along_z: bool,
}

#[derive(Resource, Default)]
pub(crate) struct PortalSites {
    chunks: HashMap<ChunkCoord, (Arc<VoxelChunk>, Vec<BlockCoord>)>,
    pub sites: Vec<PortalSite>,
}

impl PortalSites {
    /// Deliberately conservative hint radius; the server rechecks body-to-voxel reach.
    pub fn nearest(&self, feet: [f32; 3]) -> Option<BlockCoord> {
        let feet = Vec3::from_array(feet);
        self.sites
            .iter()
            .filter_map(|site| {
                let centre = Vec3::new(
                    site.arch.x as f32 + 0.5,
                    site.arch.y as f32,
                    site.arch.z as f32 + 0.5,
                );
                let distance = feet.distance_squared(centre);
                (distance <= 3.0 * 3.0).then_some((distance, site.arch))
            })
            .min_by(|a, b| {
                a.0.total_cmp(&b.0)
                    .then_with(|| (a.1.x, a.1.y, a.1.z).cmp(&(b.1.x, b.1.y, b.1.z)))
            })
            .map(|(_, arch)| arch)
    }
}

#[derive(Component)]
pub(crate) struct PortalVisual(PortalSite);
#[derive(Component)]
struct Spark {
    index: usize,
}
#[derive(Resource)]
struct PortalAssets {
    veil: Handle<Mesh>,
    cube: Handle<Mesh>,
    mist: Handle<StandardMaterial>,
    rune: Handle<StandardMaterial>,
    last_frame: u64,
}

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PortalUpdate;

pub(super) struct PortalPlugin;
impl Plugin for PortalPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PortalSites>()
            .add_systems(Startup, prepare_assets)
            .add_systems(
                Update,
                (index_portals, reconcile_visuals, animate)
                    .chain()
                    .in_set(PortalUpdate)
                    .after(super::ingest_world_updates),
            );
    }
}

fn prepare_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(PortalAssets {
        veil: meshes.add(veil_mesh(0.0)),
        cube: meshes.add(Cuboid::new(1.0, 1.0, 1.0)),
        mist: materials.add(StandardMaterial {
            base_color: Color::WHITE,
            unlit: true,
            alpha_mode: AlphaMode::Blend,
            cull_mode: None,
            double_sided: true,
            ..default()
        }),
        rune: materials.add(StandardMaterial {
            base_color: Color::srgb(0.16, 0.95, 0.72),
            emissive: LinearRgba::new(0.12, 1.4, 0.85, 1.0),
            unlit: true,
            ..default()
        }),
        last_frame: u64::MAX,
    });
}

fn index_portals(
    store: Res<ChunkStore>,
    session: Option<Res<Session>>,
    mut index: ResMut<PortalSites>,
) {
    if session.is_none() {
        index.chunks.clear();
        index.sites.clear();
        return;
    }
    index
        .chunks
        .retain(|coord, _| store.chunks.contains_key(coord));
    for (&coord, chunk) in &store.chunks {
        if index
            .chunks
            .get(&coord)
            .is_some_and(|(old, _)| Arc::ptr_eq(old, chunk))
        {
            continue;
        }
        let size = chunk.size();
        let mut hearts = Vec::new();
        for (i, &block) in chunk.blocks.iter().enumerate() {
            if block != palette::PORTAL_HEART {
                continue;
            }
            hearts.push(BlockCoord {
                x: coord.cx * size as i32 + (i % size) as i32,
                y: coord.cy * size as i32 + (i / (size * size)) as i32,
                z: coord.cz * size as i32 + ((i / size) % size) as i32,
            });
        }
        index.chunks.insert(coord, (Arc::clone(chunk), hearts));
    }
    let size = session.unwrap().0.chunk_size as usize;
    let sites = index
        .chunks
        .values()
        .flat_map(|(_, hearts)| hearts)
        .filter_map(|&arch| {
            let x = BlockCoord {
                x: arch.x + 1,
                ..arch
            };
            let z = BlockCoord {
                z: arch.z + 1,
                ..arch
            };
            let along_z = if palette::is_portal(store.block_at(x, size)) {
                false
            } else if palette::is_portal(store.block_at(z, size)) {
                true
            } else {
                return None;
            };
            let below = store.block_at(
                BlockCoord {
                    y: arch.y - 1,
                    ..arch
                },
                size,
            );
            let bottom_y = arch.y - i32::from(palette::is_portal(below));
            // Need the whole local frame before presenting an effect or interaction.
            for side in [-3, 3] {
                let p = BlockCoord {
                    x: arch.x + if along_z { 0 } else { side },
                    y: bottom_y,
                    z: arch.z + if along_z { side } else { 0 },
                };
                if store.block_at(p, size) != palette::RUNE_STONE {
                    return None;
                }
            }
            Some(PortalSite {
                arch,
                bottom: Vec3::new(arch.x as f32 + 0.5, bottom_y as f32, arch.z as f32 + 0.5),
                along_z,
            })
        })
        .collect();
    index.sites = sites;
}

fn reconcile_visuals(
    mut commands: Commands,
    sites: Res<PortalSites>,
    assets: Res<PortalAssets>,
    cameras: Query<&GlobalTransform, With<WorldCamera>>,
    visuals: Query<(Entity, &PortalVisual)>,
) {
    let eye = cameras.iter().next().map(|t| t.translation());
    let mut wanted: Vec<_> = sites
        .sites
        .iter()
        .copied()
        .filter(|site| {
            eye.is_some_and(|eye| eye.distance_squared(site.bottom) < DRAW_DISTANCE * DRAW_DISTANCE)
        })
        .collect();
    wanted.sort_by(|a, b| {
        eye.unwrap()
            .distance_squared(a.bottom)
            .total_cmp(&eye.unwrap().distance_squared(b.bottom))
    });
    wanted.truncate(MAX_VISIBLE);
    for (entity, old) in &visuals {
        if !wanted.contains(&old.0) {
            commands.entity(entity).despawn();
        }
    }
    for site in wanted {
        if visuals.iter().any(|(_, v)| v.0 == site) {
            continue;
        }
        let rotation = if site.along_z {
            Quat::from_rotation_y(std::f32::consts::FRAC_PI_2)
        } else {
            Quat::IDENTITY
        };
        commands
            .spawn((
                PortalVisual(site),
                Transform::from_translation(site.bottom).with_rotation(rotation),
                Visibility::default(),
            ))
            .with_children(|parent| {
                parent.spawn((
                    PointLight {
                        color: Color::srgb(0.15, 0.85, 0.65),
                        intensity: 120_000.0,
                        range: 7.0,
                        shadow_maps_enabled: false,
                        ..default()
                    },
                    Transform::from_xyz(0.0, 1.7, 0.8),
                ));

                parent.spawn((
                    Mesh3d(assets.veil.clone()),
                    MeshMaterial3d(assets.mist.clone()),
                    Transform::default(),
                ));
                // Each rune is cut from five angular strokes; repeated on both faces.
                for side in [-1.0, 1.0] {
                    for (x, y) in [
                        (-3.0, 0.6),
                        (-3.0, 1.6),
                        (3.0, 0.6),
                        (3.0, 1.6),
                        (-2.0, 2.6),
                        (2.0, 2.6),
                        (0.0, 3.5),
                    ] {
                        for (a, b) in rune_strokes() {
                            let a = Vec3::new(x + a.x, y + a.y, side * 0.515);
                            let b = Vec3::new(x + b.x, y + b.y, side * 0.515);
                            parent.spawn((
                                Mesh3d(assets.cube.clone()),
                                MeshMaterial3d(assets.rune.clone()),
                                Transform::from_translation((a + b) * 0.5)
                                    .with_rotation(Quat::from_rotation_arc(
                                        Vec3::Y,
                                        (b - a).normalize(),
                                    ))
                                    .with_scale(Vec3::new(0.035, a.distance(b), 0.018)),
                            ));
                        }
                    }
                }
                for index in 0..SPARKS {
                    parent.spawn((
                        Spark { index },
                        Mesh3d(assets.cube.clone()),
                        MeshMaterial3d(assets.rune.clone()),
                        spark_transform(index, 0.0),
                    ));
                }
            });
    }
}

fn rune_strokes() -> [(Vec2, Vec2); 5] {
    [
        (Vec2::new(0.0, -0.28), Vec2::new(0.0, 0.28)),
        (Vec2::new(0.0, 0.28), Vec2::new(0.22, 0.07)),
        (Vec2::new(0.22, 0.07), Vec2::new(0.0, -0.08)),
        (Vec2::new(0.0, 0.05), Vec2::new(-0.2, 0.22)),
        (Vec2::new(0.0, -0.12), Vec2::new(0.2, -0.28)),
    ]
}

fn animate(
    time: Res<Time>,
    mut assets: ResMut<PortalAssets>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut sparks: Query<(&Spark, &mut Transform)>,
    visuals: Query<(), With<PortalVisual>>,
) {
    if visuals.is_empty() {
        return;
    }
    let frame = (time.elapsed_secs_f64() * 20.0) as u64;
    if assets.last_frame == frame {
        return;
    }
    assets.last_frame = frame;
    let phase = (time.elapsed_secs_f64() % 3600.0) as f32;
    if let Some(mut mesh) = meshes.get_mut(&assets.veil) {
        let (positions, colors) = veil_vertices(phase);
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    }
    for (spark, mut transform) in &mut sparks {
        *transform = spark_transform(spark.index, phase);
    }
}

fn spark_transform(index: usize, time: f32) -> Transform {
    let seed = index as f32 * 2.399_963;
    let y = (index as f32 / SPARKS as f32 * 3.6 + time * (0.17 + (index % 3) as f32 * 0.03))
        .rem_euclid(3.6);
    let x = seed.sin() * (2.2 - 0.45 * (y - 2.0).max(0.0)) + (seed + time * 0.5).sin() * 0.10;
    let z = (seed + time * 0.35).cos() * 0.42;
    let scale = 0.018 + 0.025 * (std::f32::consts::PI * y / 3.6).sin().max(0.0);
    Transform::from_xyz(x, y, z)
        .with_scale(Vec3::splat(scale))
        .with_rotation(Quat::from_rotation_z(seed + time * 0.4))
}

fn veil_vertices(time: f32) -> (Vec<[f32; 3]>, Vec<[f32; 4]>) {
    let mut positions = Vec::with_capacity((GRID_X + 1) * (GRID_Y + 1));
    let mut colors = Vec::with_capacity(positions.capacity());
    for row in 0..=GRID_Y {
        let v = row as f32 / GRID_Y as f32;
        let y = v * 3.0;
        let half_width = 2.5 - (y - 2.0).max(0.0);
        for col in 0..=GRID_X {
            let u = col as f32 / GRID_X as f32 * 2.0 - 1.0;
            let x = u * half_width;
            let p = Vec2::new(u, (v - 0.5) * 2.0);
            let radius = p.length();
            let angle = p.y.atan2(p.x);
            let curl = (angle * 3.0 - radius * 10.0 + time * 1.15).sin() * 0.5 + 0.5;
            let threads =
                (x * 10.0 + y * 6.0 + time * 1.8 + (y * 4.0 - time).sin()).sin() * 0.5 + 0.5;
            let edge =
                (u.abs().powi(12) + (1.0 - (v * std::f32::consts::PI).sin()).powi(6)).min(1.0);
            let glow = 0.12 + curl.powi(4) * 0.52 + threads.powi(12) * 0.12 + edge * 0.4;
            positions.push([
                x,
                y,
                (x * 2.4 + time).sin() * (y * 2.0 - time * 0.7).sin() * 0.045,
            ]);
            colors.push([
                0.012 + glow * 0.09,
                0.055 + glow * 0.65,
                0.085 + glow * 0.50,
                0.88 + edge * 0.10,
            ]);
        }
    }
    (positions, colors)
}

fn veil_mesh(time: f32) -> Mesh {
    let (positions, colors) = veil_vertices(time);
    let mut indices = Vec::with_capacity(GRID_X * GRID_Y * 6);
    for y in 0..GRID_Y {
        for x in 0..GRID_X {
            let a = (y * (GRID_X + 1) + x) as u32;
            let b = a + 1;
            let d = a + (GRID_X + 1) as u32;
            let c = d + 1;
            indices.extend([a, b, c, a, c, d]);
        }
    }
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(
        Mesh::ATTRIBUTE_NORMAL,
        vec![[0.0, 0.0, 1.0]; positions.len()],
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors)
    .with_inserted_indices(Indices::U32(indices))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn veil_moves_inside_its_arch_and_keeps_its_topology() {
        let (a, colors) = veil_vertices(0.0);
        let (b, next) = veil_vertices(0.7);
        assert_ne!(a, b);
        assert_ne!(colors, next);
        assert_eq!(a.len(), (GRID_X + 1) * (GRID_Y + 1));
        for p in a.into_iter().chain(b) {
            assert!(p.iter().all(|v| v.is_finite()));
            assert!(p[1] >= 0.0 && p[1] <= 3.0);
            assert!(p[0].abs() <= 2.5 - (p[1] - 2.0).max(0.0) + 0.001);
            assert!(p[2].abs() < 0.05);
        }
        for c in colors {
            assert!(c.iter().all(|v| (0.0..=1.0).contains(v)));
        }
        assert_eq!(veil_mesh(0.0).indices().unwrap().len(), GRID_X * GRID_Y * 6);
    }
    #[test]
    fn sparks_stay_bounded_even_after_hours() {
        for time in [0.0, 0.5, 99.0, 3599.0] {
            for i in 0..SPARKS {
                let t = spark_transform(i, time);
                assert!(t.translation.x.abs() < 2.4 && t.translation.z.abs() < 0.5);
                assert!((0.0..3.6).contains(&t.translation.y));
            }
        }
    }
    fn session() -> Session {
        Session(crate::net::SessionParams {
            clock: default(),
            entity_id: 7,
            spawn: [8.5, 1.0, 11.5],
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
    fn fixture(along_z: bool) -> VoxelChunk {
        let mut chunk = VoxelChunk::all_air(32);
        for y in 0..=4 {
            for x in 5..=11 {
                let b = if y == 0 || y == 4 || x == 5 || x == 11 || (y == 3 && (x == 6 || x == 10))
                {
                    palette::RUNE_STONE
                } else if x == 8 && y == 2 {
                    palette::PORTAL_HEART
                } else {
                    palette::PORTAL_VEIL
                };
                if along_z {
                    chunk.set(8, y, x, b);
                } else {
                    chunk.set(x, y, 8, b);
                }
            }
        }
        chunk
    }
    fn headless() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .init_resource::<ChunkStore>()
            .insert_resource(session())
            .add_plugins(PortalPlugin);
        app.world_mut().spawn((
            WorldCamera,
            GlobalTransform::from_translation(Vec3::new(8.5, 2.0, 16.0)),
        ));
        app
    }
    #[test]
    fn streamed_hearts_rotate_reuse_assets_and_die_with_their_world() {
        for along_z in [false, true] {
            let mut app = headless();
            app.world_mut().resource_mut::<ChunkStore>().insert(
                ChunkCoord {
                    cx: 0,
                    cy: 0,
                    cz: 0,
                },
                fixture(along_z),
            );
            app.update();
            app.update();
            let sites = app.world().resource::<PortalSites>();
            assert_eq!(sites.sites.len(), 1);
            assert_eq!(sites.sites[0].along_z, along_z);
            assert_eq!(
                sites.nearest([8.5, 1.0, 10.0]),
                Some(BlockCoord { x: 8, y: 2, z: 8 })
            );
            assert_eq!(sites.nearest([80.0, 1.0, 10.0]), None);
            let mesh_count = app.world().resource::<Assets<Mesh>>().len();
            let material_count = app.world().resource::<Assets<StandardMaterial>>().len();
            for _ in 0..20 {
                app.update();
            }
            assert_eq!(
                app.world_mut()
                    .query::<&PortalVisual>()
                    .iter(app.world())
                    .count(),
                1
            );
            assert_eq!(
                app.world_mut().query::<&Spark>().iter(app.world()).count(),
                SPARKS
            );
            assert_eq!(app.world().resource::<Assets<Mesh>>().len(), mesh_count);
            assert_eq!(
                app.world().resource::<Assets<StandardMaterial>>().len(),
                material_count
            );
            app.world_mut()
                .resource_mut::<ChunkStore>()
                .unload(ChunkCoord {
                    cx: 0,
                    cy: 0,
                    cz: 0,
                });
            app.update();
            app.update();
            assert!(app.world().resource::<PortalSites>().sites.is_empty());
            assert_eq!(
                app.world_mut().query::<&Spark>().iter(app.world()).count(),
                0
            );
            app.world_mut().resource_mut::<ChunkStore>().insert(
                ChunkCoord {
                    cx: 0,
                    cy: 0,
                    cz: 0,
                },
                fixture(along_z),
            );
            app.update();
            crate::world::transition::new_session(app.world_mut(), 2);
            assert!(app.world().resource::<PortalSites>().sites.is_empty());
            assert_eq!(
                app.world_mut()
                    .query::<&PortalVisual>()
                    .iter(app.world())
                    .count(),
                0
            );
            app.world_mut().resource_mut::<ChunkStore>().insert(
                ChunkCoord {
                    cx: 0,
                    cy: 0,
                    cz: 0,
                },
                fixture(along_z),
            );
            app.update();
            app.world_mut().remove_resource::<Session>();
            app.update();
            app.update();
            assert!(app.world().resource::<PortalSites>().sites.is_empty());
            assert_eq!(
                app.world_mut()
                    .query::<&PortalVisual>()
                    .iter(app.world())
                    .count(),
                0
            );
        }
    }
    /// Optional real-render regression and art-review capture. No GPU is required by CI.
    #[test]
    #[ignore = "requires a render adapter; optional VOXELHEIM_PORTAL_CAPTURE PNG path"]
    fn capture_the_runic_portal_through_the_real_renderer() {
        use bevy::camera::RenderTarget;
        use bevy::core_pipeline::tonemapping::Tonemapping;
        use bevy::render::RenderApp;
        use bevy::render::render_resource::{
            CachedPipelineState, Extent3d, PipelineCache, TextureDimension, TextureFormat,
            TextureUsages,
        };
        use bevy::render::view::screenshot::{Screenshot, save_to_disk};
        use bevy::window::ExitCondition;
        use std::time::Duration;
        let mut app = App::new();
        app.add_plugins(
            DefaultPlugins
                .build()
                .disable::<bevy::winit::WinitPlugin>()
                .disable::<bevy::render::pipelined_rendering::PipelinedRenderingPlugin>()
                .set(WindowPlugin {
                    primary_window: None,
                    exit_condition: ExitCondition::DontExit,
                    ..default()
                }),
        )
        .init_resource::<ChunkStore>()
        .insert_resource(session())
        .add_plugins(PortalPlugin);
        while app.plugins_state() != bevy::app::PluginsState::Ready {
            std::thread::sleep(Duration::from_millis(10));
        }
        app.finish();
        app.cleanup();
        let mut image = Image::new_uninit(
            Extent3d {
                width: 1200,
                height: 900,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::RENDER_WORLD,
        );
        image.texture_descriptor.usage |=
            TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC;
        let target = app.world_mut().resource_mut::<Assets<Image>>().add(image);
        app.world_mut().spawn((
            WorldCamera,
            Camera3d::default(),
            AmbientLight {
                brightness: 480.0,
                ..default()
            },
            Camera {
                clear_color: Color::srgb(0.012, 0.02, 0.028).into(),
                ..default()
            },
            RenderTarget::Image(target.clone().into()),
            Tonemapping::AcesFitted,
            Transform::from_xyz(10.3, 3.0, 17.7).looking_at(Vec3::new(8.5, 2.1, 8.5), Vec3::Y),
        ));
        let chunk = fixture(false);
        app.world_mut().resource_mut::<ChunkStore>().insert(
            ChunkCoord {
                cx: 0,
                cy: 0,
                cz: 0,
            },
            chunk.clone(),
        );
        let cube = app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(Cuboid::new(1.0, 1.0, 1.0));
        let stone = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial {
                base_color: Color::linear_rgb(0.038, 0.065, 0.072),
                perceptual_roughness: 0.92,
                ..default()
            });
        let floor = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial {
                base_color: Color::srgb(0.16, 0.18, 0.20),
                perceptual_roughness: 0.95,
                ..default()
            });
        for y in 0..=4 {
            for x in 5..=11 {
                if chunk.block([x, y, 8]) == palette::RUNE_STONE {
                    app.world_mut().spawn((
                        Mesh3d(cube.clone()),
                        MeshMaterial3d(stone.clone()),
                        Transform::from_xyz(x as f32 + 0.5, y as f32 + 0.5, 8.5),
                    ));
                }
            }
        }
        // A modest antechamber makes the spectral floor illumination reviewable.
        app.world_mut().spawn((
            Mesh3d(cube.clone()),
            MeshMaterial3d(floor.clone()),
            Transform::from_xyz(8.5, 0.0, 8.5).with_scale(Vec3::new(14.0, 1.0, 14.0)),
        ));
        app.world_mut().spawn((
            Mesh3d(cube),
            MeshMaterial3d(floor),
            Transform::from_xyz(8.5, 3.0, 5.5).with_scale(Vec3::new(14.0, 7.0, 1.0)),
        ));
        app.world_mut().spawn((
            PointLight {
                intensity: 55000.0,
                range: 25.0,
                color: Color::srgb(0.55, 0.67, 0.8),
                ..default()
            },
            Transform::from_xyz(6.0, 6.0, 13.0),
        ));
        for _ in 0..100 {
            app.update();
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = std::env::var_os("VOXELHEIM_PORTAL_CAPTURE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("voxelheim-runic-portal.png"));
        app.world_mut()
            .spawn(Screenshot::image(target))
            .observe(save_to_disk(output.clone()));
        for _ in 0..60 {
            app.update();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(output.exists(), "render capture was not written");
        let cache = app
            .get_sub_app(RenderApp)
            .unwrap()
            .world()
            .resource::<PipelineCache>();
        assert!(
            cache.pipelines().next().is_some(),
            "no render pipeline compiled"
        );
        for pipeline in cache.pipelines() {
            if let CachedPipelineState::Err(error) = &pipeline.state {
                panic!("portal pipeline failed: {error}");
            }
        }
    }
}
