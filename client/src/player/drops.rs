//! Authoritative item drops, drawn as small local-time-animated geometry.
//!
//! A drop exists exactly while the newest snapshot names its id. There is no
//! pickup request, proximity check, click, fade, or prediction here: collection,
//! merging, and expiry are server outcomes, and all three look identical to this
//! module — the id disappears from the next snapshot.

use std::collections::HashSet;
use std::f32::consts::{FRAC_PI_2, TAU};
use std::time::{Duration, Instant};

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use super::InputMode;
use super::hands::sword_mesh;
use super::interpolate::{InterpolatedDrop, SnapshotBuffer};
use super::items::{ItemShape, item_linear_rgba, item_shape};
use super::merge_all;
use crate::net::Session;
#[cfg(test)]
use crate::world::palette;

/// The side length of one drop cube, in blocks.
const DROP_EDGE: f32 = 0.25;

/// How far the cosmetic child rises above the authoritative position.
///
/// Kept non-negative so a drop resting on terrain never bobs through it.
const BOB_HEIGHT: f32 = 0.08;

/// One bob every two seconds.
const BOB_RADIANS_PER_SECOND: f32 = TAU / 2.0;

/// How long a dropped sword is, in [`DROP_EDGE`]s, tip to pommel.
///
/// Unchanged from when the drop *was* one box, so nothing about how far a sword on the
/// ground reaches or how it tumbles moves — [`sword_mesh`] fills the same length with a
/// weapon.
const BLADE_DROP_LENGTH: f32 = 1.25;

/// The volume the packed-gear silhouette is allowed to occupy.
///
/// These are the old bundle box's exact dimensions. The roll and its two collars fill
/// the same bounds, so changing the silhouette cannot change how far a structure drop
/// reaches while [`animate`] tumbles it.
const BUNDLE_DROP_SIZE: Vec3 = Vec3::new(DROP_EDGE * 1.15, DROP_EDGE * 0.62, DROP_EDGE * 0.72);

/// The canvas roll inside [`BUNDLE_DROP_SIZE`].
const BUNDLE_ROLL_SIZE: Vec3 = Vec3::new(BUNDLE_DROP_SIZE.x, DROP_EDGE * 0.52, DROP_EDGE * 0.62);

/// One raised collar around the roll.
const BUNDLE_COLLAR_SIZE: Vec3 =
    Vec3::new(DROP_EDGE * 0.12, BUNDLE_DROP_SIZE.y, BUNDLE_DROP_SIZE.z);

/// How far each collar sits from the centre of the packed roll.
const BUNDLE_COLLAR_OFFSET: f32 = DROP_EDGE * 0.31;

/// One full turn every eight seconds: visible, but not a propeller.
const SPIN_RADIANS_PER_SECOND: f32 = TAU / 8.0;

/// Modes whose UI owns the view instead of the 3D world.
const HIDDEN_INPUT_MODES: [InputMode; 2] = [InputMode::Inventory, InputMode::Menu];

/// The shared world-space meshes and the small set of item colours created so far.
///
/// Materials are keyed by their actual colour rather than by item id. Every
/// unknown id therefore shares the one placeholder material instead of letting a peer
/// grow this resource by walking through all 65,535 unknown ids over time.
#[derive(Resource, Debug)]
pub(super) struct DropVisuals {
    /// One mesh per [`ItemShape`], shared by every drop and local body presenting as that
    /// shape — the reason a hundred dropped stones and one carried stone still cost one
    /// mesh rather than a hundred and one.
    shapes: Vec<(ItemShape, Handle<Mesh>)>,
    materials: Vec<([f32; 4], Handle<StandardMaterial>)>,
}

impl DropVisuals {
    /// The mesh one item id is drawn from.
    ///
    /// Read through [`item_shape`] — the same table the held view model and the inventory
    /// cell read, and this is its third reader rather than a second opinion. An id with no
    /// row answers [`ItemShape::Material`], which that function documents as the least
    /// wrong guess.
    pub(super) fn mesh_for(&self, item_id: u16) -> Handle<Mesh> {
        let shape = item_shape(item_id);
        self.shapes
            .iter()
            .find(|(candidate, _)| *candidate == shape)
            .map(|(_, mesh)| mesh.clone())
            .unwrap_or_else(|| {
                // Unreachable: `create_visuals` builds one entry per `ItemShape::ALL`, and
                // both matches on that enum are wildcard-free. Answered rather than
                // unwrapped, because a drop is the last thing that should take the window
                // down — and an invisible pelt is a smaller bug than a crash.
                error!("no drop mesh for {shape:?}");
                Handle::default()
            })
    }

    pub(super) fn material_for(
        &mut self,
        item_id: u16,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        // **Through the item table, never straight into the block palette.** A drop carries
        // an item id, and handing it to a block-id lookup is exactly the bug
        // `client/AGENTS.md` records for the pack cells — a log that drew snow-white in one
        // place and bark in another. It would also make item-only colours impossible.
        let colour = item_linear_rgba(item_id);
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
        // Built from `ItemShape::ALL`, so a fifth shape gets a drop mesh by existing.
        shapes: ItemShape::ALL
            .into_iter()
            .map(|shape| (shape, meshes.add(drop_mesh(shape))))
            .collect(),
        materials: Vec::new(),
    });
}

/// The geometry one shape is drawn from on the ground, at [`DROP_EDGE`] scale.
///
/// **Its own geometry rather than the held view model's, deliberately.** `player::hands`
/// composes a mesh in camera space against the near plane, sized so a fist does not fill
/// the screen and retuned for how the *hand* reads. A drop and a body-held item are things
/// in the world at world scale, so those two share this asset. All three read the table
/// that says which shape an item presents as, which is the thing that must not disagree.
///
/// **[`ItemShape::Blade`] is the one exception, and it proves the rule rather than breaking
/// it** (#204). A sword is a gladius — a bevelled blade tapering to a point, a cross guard,
/// a grip and a pommel — and *which weapon it is* is not a per-surface retuning any more
/// than which colour it is. So the arm calls [`sword_mesh`] with a drop-scale length
/// and gets the same weapon at a different size. Nothing is shared but the shape: this
/// module still mints its own asset, at its own scale, with its own materials, and the four
/// other arms below still author their own geometry.
///
/// Wildcard-free, so a fifth [`ItemShape`] does not compile until it can be dropped — the
/// same guarantee `ui::icon::parts` and `hands::item_mesh` already give.
fn drop_mesh(shape: ItemShape) -> Mesh {
    match shape {
        // A voxel on the ground is a small voxel.
        ItemShape::Block => Mesh::from(Cuboid::from_size(Vec3::splat(DROP_EDGE))),
        // A stub of something: rounded, so a pelt and a stone are not the same silhouette.
        ItemShape::Material => Mesh::from(Capsule3d::new(DROP_EDGE * 0.30, DROP_EDGE * 0.62)),
        // Long, thin, and lying at an angle — see `spin_and_bob`, which is what turns it.
        // The length is what it was when this was one box, so nothing about how far a
        // dropped sword reaches or how it tumbles moves.
        ItemShape::Blade => sword_mesh(DROP_EDGE * BLADE_DROP_LENGTH),
        // A carried structure is packed gear: one horizontal canvas roll with two raised
        // collars. The three structures share this silhouette and their item-table colour
        // tells them apart. `bundle_mesh` fills the old box's exact bounds.
        ItemShape::Bundle => bundle_mesh(),
        // A haft with a head across the top of it, merged into one mesh for the reason the
        // held one is: a drop is one entity with one transform, and the spin is on it.
        ItemShape::Tool => {
            let mut merged = Mesh::from(Cuboid::from_size(Vec3::new(
                DROP_EDGE * 0.14,
                DROP_EDGE * 1.30,
                DROP_EDGE * 0.14,
            )));
            let head = Mesh::from(Cuboid::from_size(Vec3::new(
                DROP_EDGE * 0.52,
                DROP_EDGE * 0.20,
                DROP_EDGE * 0.26,
            )))
            .translated_by(Vec3::new(0.0, DROP_EDGE * 0.65, 0.0));
            merge_all(&mut merged, [head], "dropped tool");
            merged
        }
    }
}

/// One rolled, strapped load for every carried structure.
///
/// The three primitive parts are merged before they become an asset. A drop — and the
/// local body-held mirror that shares this world-space asset — therefore remains one mesh,
/// one material, one entity and one transform under the existing spin and bob.
fn bundle_mesh() -> Mesh {
    let roll = |size: Vec3| {
        // Bevy authors a cylinder along Y. Turn it onto X and scale its unit diameter and
        // height independently so a round load can still honour the old rectangular bound.
        Mesh::from(Cylinder::new(0.5, 1.0))
            .rotated_by(Quat::from_rotation_z(FRAC_PI_2))
            .scaled_by(size)
    };

    let mut bundle = roll(BUNDLE_ROLL_SIZE);
    let collars = [-BUNDLE_COLLAR_OFFSET, BUNDLE_COLLAR_OFFSET]
        .map(|x| roll(BUNDLE_COLLAR_SIZE).translated_by(Vec3::X * x));
    merge_all(&mut bundle, collars, "dropped packed-gear bundle");
    bundle
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
    let mesh = visuals.mesh_for(state.item_id);
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
            Mesh3d(mesh),
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

    use super::super::items::{ITEM_DIRT, ITEM_STONE, ITEM_VARGR_PELT};
    use super::super::structures::{ITEM_CAMPFIRE, ITEM_FORGE, ITEM_TENT};
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
            equipment_slots: 3,
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
            durability: 0,
            max_durability: 0,
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
    fn two_drops_of_one_shape_spawn_at_quarter_block_scale_and_share_one_mesh() {
        // **Renamed from `..._two_quarter_block_cubes`, because they are not cubes any
        // more** — a drop is drawn as whatever shape its item presents as (#182). These
        // two are blocks, so they are still cubes and still share one mesh, which is the
        // half of the old test that was about the bound rather than about the geometry.
        let mut app = headless_player();
        deliver(
            &mut app,
            snapshot(
                1,
                vec![],
                vec![
                    drop(10, [1.0, 64.0, 2.0], ITEM_STONE),
                    drop(11, [3.0, 65.0, 4.0], ITEM_DIRT),
                ],
            ),
            Instant::now(),
        );
        app.update();

        assert_eq!(
            anchors(&mut app),
            vec![
                (10, ITEM_STONE, Vec3::new(1.0, 64.0, 2.0)),
                (11, ITEM_DIRT, Vec3::new(3.0, 65.0, 4.0)),
            ]
        );

        let shared = app
            .world()
            .resource::<DropVisuals>()
            .mesh_for(ITEM_STONE)
            .clone();
        let world = app.world_mut();
        let mut visuals = world.query_filtered::<&Mesh3d, With<DropVisual>>();
        let handles: Vec<_> = visuals.iter(world).map(|mesh| mesh.0.clone()).collect();
        assert_eq!(
            handles,
            vec![shared.clone(), shared.clone()],
            "two drops of one shape must share one mesh, not spawn one each"
        );

        let meshes = world.resource::<Assets<Mesh>>();
        let mesh = meshes.get(&shared).expect("the shared block mesh");
        let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("the mesh must carry Float32x3 positions");
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

    /// **A dropped thing looks like the thing it is**, which is the whole issue.
    ///
    /// Swept over `ItemShape::ALL` rather than a handful of named items, because the
    /// acceptance criterion is general: the pelt was the example and the rule is every
    /// item.
    ///
    /// **It compares the geometry, not the handles**, and the first draft of this test did
    /// the latter — which asserted nothing. `Assets::add` returns a fresh handle for
    /// identical geometry, so five distinct handles only proves `create_visuals` called it
    /// five times. Making every shape a cube passed it. The bounding box is what a player
    /// actually tells apart at a distance, so that is what this reads.
    #[test]
    fn every_shape_is_drawn_from_its_own_silhouette() {
        let mut app = headless_player();
        app.update();

        let world = app.world_mut();
        let mut boxes: Vec<(ItemShape, [f32; 3])> = Vec::new();
        for shape in ItemShape::ALL {
            let handle = world
                .resource::<DropVisuals>()
                .shapes
                .iter()
                .find(|(candidate, _)| *candidate == shape)
                .map(|(_, mesh)| mesh.clone())
                .unwrap_or_else(|| panic!("{shape:?} has no drop mesh"));
            let meshes = world.resource::<Assets<Mesh>>();
            let mesh = meshes.get(&handle).expect("the shape's mesh");
            let Some(VertexAttributeValues::Float32x3(positions)) =
                mesh.attribute(Mesh::ATTRIBUTE_POSITION)
            else {
                panic!("{shape:?} must carry Float32x3 positions");
            };

            let mut extent = [0.0f32; 3];
            for (axis, value) in extent.iter_mut().enumerate() {
                let min = positions
                    .iter()
                    .map(|position| position[axis])
                    .fold(f32::INFINITY, f32::min);
                let max = positions
                    .iter()
                    .map(|position| position[axis])
                    .fold(f32::NEG_INFINITY, f32::max);
                *value = max - min;
            }
            boxes.push((shape, extent));
        }

        for (index, (shape, extent)) in boxes.iter().enumerate() {
            for (other, other_extent) in &boxes[index + 1..] {
                let same = extent
                    .iter()
                    .zip(other_extent)
                    .all(|(a, b)| (a - b).abs() < 1e-6);
                assert!(
                    !same,
                    "{shape:?} and {other:?} are the same silhouette ({extent:?}); a drop \
                     that looked like every other drop would pass every other test here"
                );
            }
            assert!(
                extent.iter().all(|axis| *axis > 0.0),
                "{shape:?} is drawn from nothing"
            );
        }
    }

    /// A bundle is a rolled load with collars, not the unequal-sided box it replaces.
    ///
    /// The vertex count and cross-section are properties of that silhouette; the bounds
    /// are the compatibility promise. Pinning the old volume here protects the reach of a
    /// spinning drop and of the body-held world asset without pinning a generated vertex
    /// list.
    #[test]
    fn a_bundle_is_a_strapped_roll_inside_the_old_bounds() {
        let mesh = drop_mesh(ItemShape::Bundle);
        let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("the bundle must carry Float32x3 positions");
        };

        assert!(
            positions.len() > Mesh::from(Cuboid::from_size(BUNDLE_DROP_SIZE)).count_vertices(),
            "the packed bundle is {} vertices, which is still one box",
            positions.len()
        );

        let mut extents = [0.0; 3];
        for (axis, extent) in extents.iter_mut().enumerate() {
            let (low, high) = positions
                .iter()
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(low, high), point| {
                    (low.min(point[axis]), high.max(point[axis]))
                });
            *extent = high - low;
        }
        for (actual, expected) in extents.into_iter().zip(BUNDLE_DROP_SIZE.to_array()) {
            assert!(
                (actual - expected).abs() < 1e-6,
                "the bundle span {actual} moved outside its old {expected} bound"
            );
        }

        // A box has only two coordinate planes on either cross-section. The roll and its
        // raised collars have many, independent of the cylinder's tessellation count.
        for axis in [1, 2] {
            let mut coordinates: Vec<f32> = positions.iter().map(|point| point[axis]).collect();
            coordinates.sort_by(f32::total_cmp);
            coordinates.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
            assert!(
                coordinates.len() > 2,
                "bundle axis {axis} has only box faces at {coordinates:?}"
            );
        }
    }

    /// Tent, forge and campfire remain one visual kind, distinguished by palette colour.
    ///
    /// Three authoritative drops produce three visual children, each with exactly the one
    /// shared mesh and one material component that the existing spin turns as a unit. The
    /// material handles differ because `player/items.rs` names three distinct colours.
    #[test]
    fn bundled_structures_share_one_mesh_and_are_told_apart_by_colour() {
        let mut app = headless_player();
        deliver(
            &mut app,
            snapshot(
                1,
                vec![],
                vec![
                    drop(10, [0.0, 64.0, 0.0], ITEM_TENT),
                    drop(11, [1.0, 64.0, 0.0], ITEM_FORGE),
                    drop(12, [2.0, 64.0, 0.0], ITEM_CAMPFIRE),
                ],
            ),
            Instant::now(),
        );
        app.update();

        for item_id in [ITEM_TENT, ITEM_FORGE, ITEM_CAMPFIRE] {
            assert_eq!(item_shape(item_id), ItemShape::Bundle, "item {item_id}");
        }

        let world = app.world_mut();
        let mut every_visual = world.query_filtered::<Entity, With<DropVisual>>();
        assert_eq!(every_visual.iter(world).count(), 3);

        let mut drawn = world
            .query_filtered::<(&Mesh3d, &MeshMaterial3d<StandardMaterial>), With<DropVisual>>();
        let presentations: Vec<_> = drawn
            .iter(world)
            .map(|(mesh, material)| (mesh.0.clone(), material.0.clone()))
            .collect();
        assert_eq!(presentations.len(), 3);
        assert!(
            presentations
                .iter()
                .all(|(mesh, _)| *mesh == presentations[0].0),
            "the three Bundle items did not share one world-space mesh"
        );
        for (index, (_, material)) in presentations.iter().enumerate() {
            for (_, other) in &presentations[index + 1..] {
                assert_ne!(material, other, "two Bundle colours became one material");
            }
        }
    }

    /// **The sword on the ground is the same weapon as the sword in the hand** (#204).
    ///
    /// #182 made a drop look like the thing it is; this one is a case that criterion did not
    /// reach, because a sword and a bar had the same silhouette until the held view model
    /// stopped being a bar. So the arm reads the shape from `player::hands` at drop scale
    /// rather than authoring a second gladius that would drift from the first.
    ///
    /// Asserted as properties of the geometry rather than against the held mesh, because
    /// comparing the two meshes would pass for any pair of scaled copies of *anything* —
    /// a box included. A blade that tapers to a point, a guard wider than it, and a length
    /// unchanged from when this arm was one box are the three things a player would notice.
    #[test]
    fn a_dropped_sword_is_the_weapon_and_not_a_bar() {
        let mut app = headless_player();
        app.update();

        let world = app.world_mut();
        let handle = world
            .resource::<DropVisuals>()
            .shapes
            .iter()
            .find(|(shape, _)| *shape == ItemShape::Blade)
            .map(|(_, mesh)| mesh.clone())
            .expect("the blade has a drop mesh");
        let meshes = world.resource::<Assets<Mesh>>();
        let mesh = meshes.get(&handle).expect("the blade's drop mesh");
        let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("the blade's drop mesh must carry Float32x3 positions");
        };

        assert!(
            positions.len() > Mesh::from(Cuboid::from_size(Vec3::ONE)).count_vertices(),
            "a dropped sword is {} vertices, which is one box",
            positions.len()
        );

        let span = |axis: usize| -> (f32, f32) {
            positions
                .iter()
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(low, high), p| {
                    (low.min(p[axis]), high.max(p[axis]))
                })
        };
        let (bottom, top) = span(1);
        assert!(
            (top - bottom - DROP_EDGE * BLADE_DROP_LENGTH).abs() < 1e-6,
            "a dropped sword is {} long, and BLADE_DROP_LENGTH says {}",
            top - bottom,
            DROP_EDGE * BLADE_DROP_LENGTH
        );

        // It tapers to a point: what is left of the section at the very top is a fraction of
        // the widest the sword ever is. Read from the mesh's own extremes rather than at
        // named heights, so this stays a statement about the silhouette and not a second
        // copy of `hands`' constants.
        let widest = |low: f32, high: f32| {
            positions
                .iter()
                .filter(|p| p[1] >= low && p[1] <= high)
                .map(|p| p[2].abs())
                .fold(0.0f32, f32::max)
        };
        let point = widest(top - 1e-6, top);
        let anywhere = widest(bottom, top);
        assert!(
            point < anywhere * 0.2,
            "a dropped sword is {point} across at its tip against {anywhere} at its widest, so \
             it is still a bar"
        );

        // And the widest part is down at the hilt rather than out at the tip, which is what
        // a cross guard is and what tells a sword from a paddle.
        let guard = positions
            .iter()
            .filter(|p| p[2].abs() > anywhere - 1e-6)
            .map(|p| p[1])
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            guard < (bottom + top) / 2.0,
            "a dropped sword is widest at {guard}, above the middle of a sword spanning \
             {bottom}..{top}, so whatever is widest about it is not a guard"
        );
    }

    /// The colour comes through the item table, never straight into the palette.
    ///
    /// **This is the bug the issue names**, and the shape of it is recorded in
    /// `client/AGENTS.md` for the pack cells: `palette::linear_rgba` reads a *block* id,
    /// and a drop carries an *item* id. Handing one to the other is how a pelt ended up
    /// pink. The assertion is against `item_linear_rgba`, so it is the table that decides
    /// and not this test.
    #[test]
    fn a_drop_wears_the_colour_its_item_row_names() {
        let mut app = headless_player();
        // A vargr pelt: an item id whose row deliberately resolves differently from the
        // same-numbered block, which is what makes this able to fail. Reading the id
        // straight into the palette gives a different colour, and that difference is the bug.
        deliver(
            &mut app,
            snapshot(1, vec![], vec![drop(10, [0.0, 64.0, 0.0], ITEM_VARGR_PELT)]),
            Instant::now(),
        );
        app.update();

        assert_ne!(
            item_linear_rgba(ITEM_VARGR_PELT),
            palette::linear_rgba(ITEM_VARGR_PELT),
            "this test cannot fail unless the item row differs from the same-numbered block"
        );

        let world = app.world_mut();
        let mut visuals =
            world.query_filtered::<&MeshMaterial3d<StandardMaterial>, With<DropVisual>>();
        let handle = visuals
            .iter(world)
            .next()
            .expect("the drop is drawn")
            .0
            .clone();
        let materials = world.resource::<Assets<StandardMaterial>>();
        let material = materials.get(&handle).expect("the drop's material");

        let [r, g, b, a] = item_linear_rgba(ITEM_VARGR_PELT);
        assert_eq!(material.base_color, Color::linear_rgba(r, g, b, a));
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

        let remaining = anchors(&mut app);
        assert_eq!(remaining.len(), 1, "the omitted drop anchor survived");
        assert_eq!((remaining[0].0, remaining[0].1), (11, palette::DIRT));
        assert!(
            remaining[0].2.distance(Vec3::new(3.0, 64.0, 4.0)) < 1e-5,
            "the surviving drop moved to {:?}",
            remaining[0].2
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
        let [r, g, b, a] = item_linear_rgba(UNKNOWN_ITEM);
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
