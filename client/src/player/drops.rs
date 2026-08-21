//! Authoritative item drops, drawn as small local-time-animated cubes.
//!
//! A drop exists exactly while the newest snapshot names its id. There is no
//! pickup request, proximity check, click, fade, or prediction here: collection,
//! merging, and expiry are server outcomes, and all three look identical to this
//! module — the id disappears from the next snapshot.

use std::collections::HashSet;
use std::f32::consts::TAU;
use std::time::{Duration, Instant};

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use super::InputMode;
use super::interpolate::{InterpolatedDrop, SnapshotBuffer};
use crate::net::Session;
use crate::world::palette;

/// The side length of one drop cube, in blocks.
const DROP_EDGE: f32 = 0.25;

/// How far the cosmetic child rises above the authoritative position.
///
/// Kept non-negative so a drop resting on terrain never bobs through it.
const BOB_HEIGHT: f32 = 0.08;

/// One bob every two seconds.
const BOB_RADIANS_PER_SECOND: f32 = TAU / 2.0;

/// One full turn every eight seconds: visible, but not a propeller.
const SPIN_RADIANS_PER_SECOND: f32 = TAU / 8.0;

/// Modes whose UI owns the view instead of the 3D world.
const HIDDEN_INPUT_MODES: [InputMode; 2] = [InputMode::Inventory, InputMode::Menu];

/// The shared cube mesh and the small set of palette materials created so far.
///
/// Materials are keyed by their actual palette colour rather than by item id. Every
/// unknown id therefore shares the one placeholder material instead of letting a peer
/// grow this resource by walking through all 65,535 unknown ids over time.
#[derive(Resource, Debug)]
pub(super) struct DropVisuals {
    cube: Handle<Mesh>,
    materials: Vec<([f32; 4], Handle<StandardMaterial>)>,
}

impl DropVisuals {
    fn material_for(
        &mut self,
        item_id: u16,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        let colour = palette::linear_rgba(item_id);
        if let Some((_, material)) = self
            .materials
            .iter()
            .find(|(candidate, _)| *candidate == colour)
        {
            return material.clone();
        }

        let [r, g, b, a] = colour;
        let material = materials.add(StandardMaterial {
            base_color: Color::linear_rgba(r, g, b, a),
            perceptual_roughness: 0.85,
            ..default()
        });
        self.materials.push((colour, material.clone()));
        material
    }
}

/// The authoritative anchor for one drop and the identity that drives it.
///
/// Its transform is *only* the interpolated server position. Spin and bob live on a
/// child, so cosmetic motion cannot contaminate the position other systems or tests read.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DroppedItem {
    entity_id: u64,
    item_id: u16,
}

/// The cosmetic child of a [`DroppedItem`] anchor.
#[derive(Component)]
pub(super) struct DropVisual {
    owner: Entity,
}

#[derive(SystemParam)]
pub(super) struct DropAssets<'w> {
    visuals: Option<ResMut<'w, DropVisuals>>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
}

pub(super) fn create_visuals(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>) {
    commands.insert_resource(DropVisuals {
        cube: meshes.add(Cuboid::from_size(Vec3::splat(DROP_EDGE))),
        materials: Vec::new(),
    });
}

/// Spawns, places, and despawns drops from the latest authoritative snapshot.
///
/// The newest snapshot is the existence set. In particular, player proximity is not an
/// input: walking over a drop that the server still sends changes nothing here.
pub(super) fn apply_snapshots(
    buffer: Res<SnapshotBuffer>,
    session: Option<Res<Session>>,
    mode: Res<InputMode>,
    mut assets: DropAssets,
    mut existing: Query<(Entity, &mut DroppedItem, &mut Transform, &mut Visibility)>,
    mut drop_visuals: Query<(&DropVisual, &mut MeshMaterial3d<StandardMaterial>)>,
    mut commands: Commands,
) {
    let (Some(session), Some(mut visuals)) = (session, assets.visuals) else {
        return;
    };

    let interval = Duration::from_secs(1) / u32::from(session.0.tick_rate);
    let drawn = buffer.sample_drops(Instant::now(), interval);
    let mut placed = HashSet::with_capacity(drawn.len());
    let mut changed_items = Vec::new();
    let visibility = drop_visibility(*mode);

    for (entity, mut drop, mut transform, mut current_visibility) in &mut existing {
        match drawn
            .iter()
            .find(|(entity_id, _)| *entity_id == drop.entity_id)
        {
            Some((_, state)) => {
                transform.translation = state.pos;
                if *current_visibility != visibility {
                    *current_visibility = visibility;
                }
                if drop.item_id != state.item_id {
                    drop.item_id = state.item_id;
                    changed_items.push((entity, state.item_id));
                }
                placed.insert(drop.entity_id);
            }
            None => commands.entity(entity).despawn(),
        }
    }

    for (owner, item_id) in changed_items {
        let material = visuals.material_for(item_id, &mut assets.materials);
        for (visual, mut current) in &mut drop_visuals {
            if visual.owner == owner {
                current.0 = material.clone();
            }
        }
    }

    for (entity_id, state) in &drawn {
        if !placed.insert(*entity_id) {
            continue;
        }
        spawn_drop(
            &mut commands,
            &mut visuals,
            &mut assets.materials,
            *entity_id,
            state,
            visibility,
        );
    }
}

fn spawn_drop(
    commands: &mut Commands,
    visuals: &mut DropVisuals,
    materials: &mut Assets<StandardMaterial>,
    entity_id: u64,
    state: &InterpolatedDrop,
    visibility: Visibility,
) {
    let material = visuals.material_for(state.item_id, materials);
    let owner = commands
        .spawn((
            DroppedItem {
                entity_id,
                item_id: state.item_id,
            },
            Transform::from_translation(state.pos),
            visibility,
        ))
        .id();
    commands.entity(owner).with_children(|parent| {
        parent.spawn((
            DropVisual { owner },
            Mesh3d(visuals.cube.clone()),
            MeshMaterial3d(material),
            Transform::default(),
        ));
    });
}

fn drop_visibility(mode: InputMode) -> Visibility {
    if HIDDEN_INPUT_MODES.contains(&mode) {
        Visibility::Hidden
    } else {
        Visibility::Visible
    }
}

/// Advances only the cosmetic child. The parent remains the exact interpolated answer.
pub(super) fn animate(time: Res<Time>, mut visuals: Query<&mut Transform, With<DropVisual>>) {
    let transform = cosmetic_transform(time.elapsed_secs());
    for mut visual in &mut visuals {
        *visual = transform;
    }
}

fn cosmetic_transform(elapsed: f32) -> Transform {
    let bob = BOB_HEIGHT * (0.5 - 0.5 * (elapsed * BOB_RADIANS_PER_SECOND).cos());
    Transform {
        translation: Vec3::Y * bob,
        rotation: Quat::from_rotation_y(elapsed * SPIN_RADIANS_PER_SECOND),
        ..default()
    }
}

#[cfg(test)]
mod tests {
    use bevy::asset::AssetPlugin;
    use bevy::mesh::VertexAttributeValues;
    use bevy::time::TimeUpdateStrategy;

    use super::*;
    use crate::net::{EntityState, ItemDropState, SessionParams, Snapshot, SnapshotInbox};
    use crate::player::PlayerPlugin;

    const INTERVAL: Duration = Duration::from_millis(50);
    const LOCAL_ID: u64 = 7;

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: LOCAL_ID,
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

    fn state(entity_id: u64, pos: [f32; 3]) -> EntityState {
        EntityState {
            entity_id,
            pos,
            vel: [0.0; 3],
            yaw: 0.0,
        }
    }

    fn drop(entity_id: u64, pos: [f32; 3], item_id: u16) -> ItemDropState {
        ItemDropState {
            entity_id,
            pos,
            item_id,
            count: 1,
        }
    }

    fn snapshot(
        server_tick: u32,
        entities: Vec<EntityState>,
        drops: Vec<ItemDropState>,
    ) -> Snapshot {
        Snapshot {
            server_tick,
            entities,
            drops,
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

    fn anchors(app: &mut App) -> Vec<(u64, u16, Vec3)> {
        let world = app.world_mut();
        let mut query = world.query::<(&DroppedItem, &Transform)>();
        let mut found: Vec<_> = query
            .iter(world)
            .map(|(drop, transform)| (drop.entity_id, drop.item_id, transform.translation))
            .collect();
        found.sort_by_key(|(entity_id, _, _)| *entity_id);
        found
    }

    fn only_anchor_visibility(app: &mut App) -> Visibility {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Visibility, With<DroppedItem>>();
        *query.single(world).expect("one drop anchor")
    }

    #[test]
    fn a_snapshot_with_two_drops_spawns_two_quarter_block_cubes() {
        let mut app = headless_player();
        deliver(
            &mut app,
            snapshot(
                1,
                vec![],
                vec![
                    drop(10, [1.0, 64.0, 2.0], palette::STONE),
                    drop(11, [3.0, 65.0, 4.0], palette::DIRT),
                ],
            ),
            Instant::now(),
        );
        app.update();

        assert_eq!(
            anchors(&mut app),
            vec![
                (10, palette::STONE, Vec3::new(1.0, 64.0, 2.0)),
                (11, palette::DIRT, Vec3::new(3.0, 65.0, 4.0)),
            ]
        );

        let shared = app.world().resource::<DropVisuals>().cube.clone();
        let world = app.world_mut();
        let mut visuals = world.query_filtered::<&Mesh3d, With<DropVisual>>();
        let handles: Vec<_> = visuals.iter(world).map(|mesh| mesh.0.clone()).collect();
        assert_eq!(handles, vec![shared.clone(), shared.clone()]);

        let meshes = world.resource::<Assets<Mesh>>();
        let mesh = meshes.get(&shared).expect("the shared drop cube mesh");
        let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("the cube must carry Float32x3 positions");
        };
        for axis in 0..3 {
            let min = positions
                .iter()
                .map(|position| position[axis])
                .fold(f32::INFINITY, f32::min);
            let max = positions
                .iter()
                .map(|position| position[axis])
                .fold(f32::NEG_INFINITY, f32::max);
            assert!((max - min - DROP_EDGE).abs() < 1e-6);
        }
    }

    #[test]
    fn the_newest_snapshot_despawns_exactly_the_id_it_omits() {
        let mut app = headless_player();
        let start = Instant::now();
        deliver(
            &mut app,
            snapshot(
                1,
                vec![],
                vec![
                    drop(10, [1.0, 64.0, 2.0], palette::STONE),
                    drop(11, [3.0, 64.0, 4.0], palette::DIRT),
                ],
            ),
            start,
        );
        app.update();

        deliver(
            &mut app,
            snapshot(2, vec![], vec![drop(11, [3.0, 64.0, 4.0], palette::DIRT)]),
            start + INTERVAL,
        );
        app.update();

        assert_eq!(
            anchors(&mut app),
            vec![(11, palette::DIRT, Vec3::new(3.0, 64.0, 4.0))]
        );
        let world = app.world_mut();
        let mut visuals = world.query_filtered::<Entity, With<DropVisual>>();
        assert_eq!(visuals.iter(world).count(), 1, "the removed cube survived");
    }

    #[test]
    fn an_unknown_item_uses_the_placeholder_material_instead_of_disappearing() {
        let mut app = headless_player();
        const UNKNOWN_ITEM: u16 = u16::MAX;
        deliver(
            &mut app,
            snapshot(1, vec![], vec![drop(10, [0.0, 64.0, 0.0], UNKNOWN_ITEM)]),
            Instant::now(),
        );
        app.update();

        assert_eq!(anchors(&mut app).len(), 1);
        let world = app.world_mut();
        let mut query =
            world.query_filtered::<&MeshMaterial3d<StandardMaterial>, With<DropVisual>>();
        let handle = query.single(world).expect("one unknown drop").0.clone();
        let material = world
            .resource::<Assets<StandardMaterial>>()
            .get(&handle)
            .expect("the placeholder material");
        let [r, g, b, a] = palette::linear_rgba(UNKNOWN_ITEM);
        assert_eq!(material.base_color, Color::linear_rgba(r, g, b, a));
    }

    #[test]
    fn inventory_and_menu_hide_a_drop_without_removing_it() {
        let mut app = headless_player();
        deliver(
            &mut app,
            snapshot(1, vec![], vec![drop(10, [0.0, 64.0, 0.0], palette::STONE)]),
            Instant::now(),
        );
        app.update();
        assert_eq!(only_anchor_visibility(&mut app), Visibility::Visible);

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Inventory;
        app.update();
        assert_eq!(only_anchor_visibility(&mut app), Visibility::Hidden);
        assert_eq!(anchors(&mut app).len(), 1, "opening inventory despawned it");

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Menu;
        app.update();
        assert_eq!(only_anchor_visibility(&mut app), Visibility::Hidden);
        assert_eq!(anchors(&mut app).len(), 1, "opening the menu despawned it");

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        app.update();
        assert_eq!(only_anchor_visibility(&mut app), Visibility::Visible);
    }

    #[test]
    fn a_drop_position_uses_the_same_snapshot_blend_as_a_player() {
        let mut buffer = SnapshotBuffer::default();
        let start = Instant::now();
        buffer.accept(
            snapshot(
                1,
                vec![state(LOCAL_ID, [0.0, 64.0, 0.0])],
                vec![drop(10, [0.0, 64.0, 0.0], palette::STONE)],
            ),
            start,
        );
        let arrived = start + INTERVAL;
        buffer.accept(
            snapshot(
                2,
                vec![state(LOCAL_ID, [8.0, 64.0, 0.0])],
                vec![drop(10, [8.0, 64.0, 0.0], palette::STONE)],
            ),
            arrived,
        );

        let player = buffer.sample(arrived + INTERVAL / 2, INTERVAL)[0].1;
        let drop = buffer.sample_drops(arrived + INTERVAL / 2, INTERVAL)[0].1;
        assert_eq!(drop.pos, player.pos);
        assert!((drop.pos.x - 4.0).abs() < 1e-4, "x = {}", drop.pos.x);
        assert_eq!(drop.pos.y, 64.0);
        assert_eq!(drop.pos.z, 0.0);
    }

    #[test]
    fn walking_over_a_drop_does_not_remove_it_while_the_snapshot_keeps_it() {
        let mut app = headless_player();
        let start = Instant::now();
        let held = drop(10, [4.0, 64.0, 0.0], palette::STONE);
        deliver(
            &mut app,
            snapshot(1, vec![state(LOCAL_ID, [0.0, 64.0, 0.0])], vec![held]),
            start,
        );
        app.update();

        deliver(
            &mut app,
            snapshot(2, vec![state(LOCAL_ID, held.pos)], vec![held]),
            start + INTERVAL,
        );
        app.update();

        assert_eq!(anchors(&mut app).len(), 1);
        assert_eq!(anchors(&mut app)[0].0, held.entity_id);
    }

    #[test]
    fn spin_and_bob_use_local_time_without_moving_the_authoritative_anchor() {
        let mut app = headless_player();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            250,
        )));
        let authoritative = Vec3::new(2.0, 64.0, -3.0);
        deliver(
            &mut app,
            snapshot(
                1,
                vec![],
                vec![drop(10, authoritative.to_array(), palette::STONE)],
            ),
            Instant::now(),
        );
        app.update();

        let first = {
            let world = app.world_mut();
            let mut query = world.query_filtered::<&Transform, With<DropVisual>>();
            *query.single(world).expect("one drop visual")
        };
        app.update();
        let second = {
            let world = app.world_mut();
            let mut query = world.query_filtered::<&Transform, With<DropVisual>>();
            *query.single(world).expect("one drop visual")
        };

        assert_ne!(first.rotation, second.rotation, "the cube did not turn");
        assert_ne!(
            first.translation.y, second.translation.y,
            "the cube did not bob"
        );
        assert_eq!(anchors(&mut app)[0].2, authoritative);
    }
}
