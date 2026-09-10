//! Essential hazard boundaries. No particles, flashing, audio or optional effects
//! carry information: reduced effects therefore preserve the entire cue surface.
//! Dashed means announced; continuous double lines mean contact on this tick.

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use super::{EncounterPresentation, MoveKey, reconcile};
use crate::net::{HazardShape, HazardVolume};

const ARC_STEPS: usize = 48;
const EDGE_STEPS: usize = 12;
const WIDTH: f32 = 0.065;

#[derive(Resource)]
struct CueMaterials {
    announced: Handle<StandardMaterial>,
    contact: Handle<StandardMaterial>,
}

#[derive(Component)]
struct HazardCue {
    key: MoveKey,
    index: usize,
    volume: HazardVolume,
    contact: bool,
}

pub(super) fn register(app: &mut App) {
    app.add_systems(Startup, setup)
        .add_systems(Update, refresh.after(reconcile));
}

fn setup(mut commands: Commands, mut materials: ResMut<Assets<StandardMaterial>>) {
    let mut material = |color| {
        materials.add(StandardMaterial {
            base_color: color,
            unlit: true,
            fog_enabled: false,
            cull_mode: None,
            ..default()
        })
    };
    commands.insert_resource(CueMaterials {
        announced: material(Color::srgb(1.0, 0.82, 0.35)),
        contact: material(Color::srgb(1.0, 0.40, 0.30)),
    });
}

fn refresh(
    mut commands: Commands,
    presentation: Res<EncounterPresentation>,
    materials: Res<CueMaterials>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut cues: Query<(
        Entity,
        &mut HazardCue,
        &Mesh3d,
        &mut MeshMaterial3d<StandardMaterial>,
        &mut Transform,
    )>,
) {
    if !presentation.is_changed() {
        return;
    }
    // The product of protocol bounds is 4 * 8 * 16 = 512 meshes/entities at most.
    // There is no historical cache: removed instances explicitly release their asset.
    let mut held = Vec::with_capacity(cues.iter().len());
    for (entity, mut cue, mesh, mut material, mut transform) in &mut cues {
        let next = presentation
            .0
            .iter()
            .find(|one| one.key == cue.key)
            .and_then(|one| one.hazards().get(cue.index).map(|volume| (one, *volume)));
        let Some((one, volume)) = next else {
            meshes.remove(mesh.0.id());
            commands.entity(entity).despawn();
            continue;
        };
        let contact = one.damaging();
        if cue.volume != volume || cue.contact != contact {
            if let Some(mut asset) = meshes.get_mut(&mesh.0) {
                *asset = boundary_mesh(volume, contact);
            }
            cue.volume = volume;
            cue.contact = contact;
        }
        material.0 = if contact {
            materials.contact.clone()
        } else {
            materials.announced.clone()
        };
        *transform = placement(volume, one.boss_kind);
        held.push((cue.key, cue.index));
    }
    for one in &presentation.0 {
        for (index, volume) in one.hazards().iter().copied().enumerate() {
            if held.contains(&(one.key, index)) {
                continue;
            }
            let contact = one.damaging();
            commands.spawn((
                HazardCue {
                    key: one.key,
                    index,
                    volume,
                    contact,
                },
                Mesh3d(meshes.add(boundary_mesh(volume, contact))),
                MeshMaterial3d(if contact {
                    materials.contact.clone()
                } else {
                    materials.announced.clone()
                }),
                placement(volume, one.boss_kind),
            ));
        }
    }
}

pub(super) fn placement(volume: HazardVolume, boss: crate::net::MobKind) -> Transform {
    let [x, y, z] = volume.origin;
    // Server hazardAnchor centres melee/lanes on the boss and areas on their own
    // height. Project those fixed anchors onto the arena floor, not the moving boss.
    let offset = match volume.shape {
        HazardShape::Cone { .. } | HazardShape::Line { .. } => {
            super::super::mobs::body(boss).height / 2.0
        }
        HazardShape::Disc | HazardShape::Ring { .. } => volume.height / 2.0,
    };
    Transform::from_xyz(x, y - offset + 0.045, z)
}

/// At most 192 strips (768 vertices), independent of radius or distance. Lines
/// retain the server's direction; discs/rings have no invented facing.
fn boundary_mesh(volume: HazardVolume, contact: bool) -> Mesh {
    let mut edges = Vec::new();
    let forward = Vec2::new(volume.direction[0], volume.direction[2]).normalize_or_zero();
    let side = Vec2::new(-forward.y, forward.x);
    let point = |angle: f32, radius: f32| Vec2::new(angle.cos(), angle.sin()) * radius;
    let arc = |edges: &mut Vec<(Vec2, Vec2)>, start: f32, end: f32, radius: f32| {
        for step in 0..ARC_STEPS {
            let a = start + (end - start) * step as f32 / ARC_STEPS as f32;
            let b = start + (end - start) * (step + 1) as f32 / ARC_STEPS as f32;
            edges.push((point(a, radius), point(b, radius)));
        }
    };
    let line = |edges: &mut Vec<(Vec2, Vec2)>, a: Vec2, b: Vec2| {
        for step in 0..EDGE_STEPS {
            edges.push((
                a.lerp(b, step as f32 / EDGE_STEPS as f32),
                a.lerp(b, (step + 1) as f32 / EDGE_STEPS as f32),
            ));
        }
    };
    match volume.shape {
        HazardShape::Disc => arc(&mut edges, 0.0, std::f32::consts::TAU, volume.radius),
        HazardShape::Ring { inner_radius } => {
            arc(&mut edges, 0.0, std::f32::consts::TAU, volume.radius);
            if inner_radius > 0.0 {
                arc(&mut edges, std::f32::consts::TAU, 0.0, inner_radius);
            }
        }
        HazardShape::Cone { half_angle } => {
            let bearing = forward.y.atan2(forward.x);
            arc(
                &mut edges,
                bearing - half_angle,
                bearing + half_angle,
                volume.radius,
            );
            line(
                &mut edges,
                Vec2::ZERO,
                point(bearing - half_angle, volume.radius),
            );
            line(
                &mut edges,
                point(bearing + half_angle, volume.radius),
                Vec2::ZERO,
            );
        }
        HazardShape::Line { half_width } => {
            let corners = [
                -side * half_width,
                forward * volume.radius - side * half_width,
                forward * volume.radius + side * half_width,
                side * half_width,
            ];
            for i in 0..4 {
                line(&mut edges, corners[i], corners[(i + 1) % 4]);
            }
        }
    }
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for (a, b) in edges {
        let direction = (b - a).normalize_or_zero();
        let normal = Vec2::new(-direction.y, direction.x);
        let b = if contact { b } else { a.lerp(b, 0.65) };
        for offset in if contact {
            &[0.0, WIDTH * 2.5][..]
        } else {
            &[0.0][..]
        } {
            let shift = normal * *offset;
            let half = normal * WIDTH / 2.0;
            let start = vertices.len() as u32;
            for p in [
                a + shift - half,
                b + shift - half,
                b + shift + half,
                a + shift + half,
            ] {
                vertices.push([p.x, 0.0, p.y]);
            }
            indices.extend([start, start + 2, start + 1, start, start + 3, start + 2]);
        }
    }
    let count = vertices.len();
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vertices)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; count])
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0, 0.0]; count])
    .with_inserted_indices(Indices::U32(indices))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::mesh::VertexAttributeValues;

    fn volume(shape: HazardShape) -> HazardVolume {
        HazardVolume {
            shape,
            origin: [0.0, 1.0, 0.0],
            direction: [0.0, 0.0, -1.0],
            radius: 5.0,
            height: 2.0,
        }
    }

    #[test]
    fn every_shape_has_bounded_geometry_and_a_non_colour_contact_distinction() {
        for shape in [
            HazardShape::Disc,
            HazardShape::Ring { inner_radius: 3.0 },
            HazardShape::Cone { half_angle: 0.7 },
            HazardShape::Line { half_width: 1.0 },
        ] {
            let preparation = boundary_mesh(volume(shape), false);
            let contact = boundary_mesh(volume(shape), true);
            assert!(preparation.count_vertices() > 0);
            assert_eq!(contact.count_vertices(), preparation.count_vertices() * 2);
            assert!(contact.count_vertices() <= 768);
            let VertexAttributeValues::Float32x3(points) =
                contact.attribute(Mesh::ATTRIBUTE_POSITION).unwrap()
            else {
                panic!("positions");
            };
            assert!(points.iter().flatten().all(|x| x.is_finite()));
            if let HazardShape::Ring { inner_radius } = shape {
                assert!(
                    points
                        .iter()
                        .all(|p| Vec2::new(p[0], p[2]).length() >= inner_radius - 0.25),
                    "the safe middle must remain clear"
                );
            }
            if let HazardShape::Line { half_width } = shape {
                assert!(
                    points
                        .iter()
                        .all(|p| p[0].abs() <= half_width + 0.25 && p[2] <= 0.25 && p[2] >= -5.25)
                );
            }
        }
    }

    #[test]
    fn maximum_live_hazards_reuse_assets_then_release_every_mesh() {
        use crate::net::{EncounterTimelineInbox, MAX_LIVE_ENCOUNTERS, Snapshot};
        use crate::player::encounters::{
            project,
            tests::{snapshot, timeline},
        };
        let mut timelines = Vec::new();
        let mut snap = Snapshot {
            server_tick: 110,
            ..default()
        };
        for encounter in 0..MAX_LIVE_ENCOUNTERS {
            let mut one = timeline();
            one.encounter_id += encounter as u64;
            one.boss_entity_id += encounter as u64;
            let mut mob = snapshot(110).mobs.remove(0);
            mob.entity_id = one.boss_entity_id;
            snap.mobs.push(mob);
            one.moves = (0..8)
                .map(|index| {
                    let mut announced = one.moves[0].clone();
                    announced.move_instance_id += index;
                    announced.hazards = vec![announced.hazards[0]; 16];
                    announced
                })
                .collect();
            timelines.push(one);
        }
        let mut inbox = EncounterTimelineInbox::default();
        for one in timelines {
            inbox.push(one);
        }
        let projected = project(inbox.live(), Some(&snap));
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .insert_resource(EncounterPresentation(projected.clone()));
        register(&mut app);
        app.update();
        assert_eq!(app.world().resource::<Assets<Mesh>>().len(), 512);
        let original: Vec<_> = app.world().resource::<Assets<Mesh>>().ids().collect();
        // No Settings, audio device, particle emitter or light exists in this app.
        // Essential geometry survives the smallest presentation configuration.
        for _ in 0..4 {
            app.world_mut().resource_mut::<EncounterPresentation>().0 = projected.clone();
            app.update();
            assert_eq!(
                app.world()
                    .resource::<Assets<Mesh>>()
                    .ids()
                    .collect::<Vec<_>>(),
                original
            );
        }
        assert_eq!(app.world().resource::<Assets<StandardMaterial>>().len(), 2);
        app.world_mut()
            .resource_mut::<EncounterPresentation>()
            .0
            .clear();
        app.update();
        assert_eq!(app.world().resource::<Assets<Mesh>>().len(), 0);
        let world = app.world_mut();
        assert_eq!(world.query::<&HazardCue>().iter(world).count(), 0);
        world.resource_mut::<EncounterPresentation>().0 = projected;
        app.update();
        assert_eq!(app.world().resource::<Assets<Mesh>>().len(), 512);
        crate::player::reset_world(app.world_mut());
        app.update();
        assert_eq!(app.world().resource::<Assets<Mesh>>().len(), 0);
    }
}
