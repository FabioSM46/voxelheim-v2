//! Authoritative item drops, drawn as small local-time-animated geometry.
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

use super::hands::{
    bow_mesh, sceptre_mesh, shield_mesh, sword_grip_mesh, sword_guard_base, sword_mesh_with,
};
#[cfg(test)]
use super::hands::{sword_blade_span, sword_grip_centre, sword_guard_span};
use super::interpolate::{InterpolatedDrop, SnapshotBuffer};
use super::items::{ItemShape, item_linear_rgba, item_shape};
use super::items::{Livery, item_livery, liveried_shapes};
use super::livery::Liveries;
use super::merge_all;
use super::{InputMode, bundle_strap_linear_rgba, rolled_bundle_parts};
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

/// How long a dropped sword is, in [`DROP_EDGE`]s, tip to pommel.
///
/// Unchanged from when the drop *was* one box, so nothing about how far a sword on the
/// ground reaches or how it tumbles moves — [`sword_mesh_with`] fills the same length with a
/// weapon.
const BLADE_DROP_LENGTH: f32 = 1.25;

/// The point a fist closes around, in the shared world-scale mesh.
///
/// The body attachment is seated by [`blade_guard_base`]; this is the point its tests check
/// has landed inside the rig's fist, asked of the mesh recipe rather than copied out of it.
#[cfg(test)]
pub(super) fn blade_grip_centre() -> Vec3 {
    sword_grip_centre(DROP_EDGE * BLADE_DROP_LENGTH)
}

/// The guard face a body-held sword is seated against, carried point-forward from the fist.
///
/// The rearward face of the cross guard, the one the grip enters. The body attachment puts it
/// on the fist's forward face, which is what closes the grip and the pommel inside the fist
/// and leaves the guard immediately in front of it.
///
/// The body and ground share this world-scale mesh, so the attachment asks the mesh recipe
/// where its guard is instead of holding a second copy of the furniture proportions.
pub(super) fn blade_guard_base() -> Vec3 {
    sword_guard_base(DROP_EDGE * BLADE_DROP_LENGTH)
}

/// The blade centreline used to measure how much of a carried sword projects clear.
#[cfg(test)]
pub(super) fn blade_span() -> [Vec3; 2] {
    sword_blade_span(DROP_EDGE * BLADE_DROP_LENGTH)
}

/// The cross-guard span used to prove it is visible from both body reference views.
#[cfg(test)]
pub(super) fn blade_guard_span() -> [Vec3; 2] {
    sword_guard_span(DROP_EDGE * BLADE_DROP_LENGTH)
}

/// The volume the packed-gear silhouette is allowed to occupy.
///
/// These are the old bundle box's exact dimensions. The roll and its two collars fill
/// the same bounds, so changing the silhouette cannot change how far a structure drop
/// reaches while [`animate`] tumbles it.
const BUNDLE_DROP_SIZE: Vec3 = Vec3::new(DROP_EDGE * 1.15, DROP_EDGE * 0.62, DROP_EDGE * 0.72);

/// One full turn every eight seconds: visible, but not a propeller.
const SPIN_RADIANS_PER_SECOND: f32 = TAU / 8.0;

/// Full-screen modes whose UI owns the view instead of the 3D world.
const HIDDEN_INPUT_MODES: [InputMode; 2] = [InputMode::Inventory, InputMode::Menu];

/// What decides which mesh a drop is drawn from.
///
/// **A shape *and* a livery**, because a livery decides geometry as well as colour: the rusty
/// blade is pitted and the iron one is not, so they stopped being one mesh in #417. Named
/// rather than spelled out at each of its four uses, and the pair is deliberately not an item
/// id — two items sharing both halves still share one mesh, which is what the cache is for.
type MeshKey = (ItemShape, Option<Livery>);

/// Whether one shape's drop mesh depends on the livery it wears.
///
/// **Wildcard-free, so a shape whose geometry starts reading a livery has to say so here.**
/// That is not decoration: a shape that varied and answered `false` would share a mesh with
/// the un-liveried one and render with somebody else's texture coordinates, silently.
///
/// Only the blade does today. Its loft is built *against* the field — the coordinates name
/// the livery's own band, and a livery that displaces pits the sections besides — while every
/// other arm of [`drop_mesh`] ignores the argument and lets the livery reach the mesh through
/// the material alone.
fn mesh_varies_with_livery(shape: ItemShape) -> bool {
    match shape {
        ItemShape::Blade => true,
        ItemShape::Block
        | ItemShape::Material
        | ItemShape::Bundle
        | ItemShape::Tool
        | ItemShape::Armour
        | ItemShape::Shield
        | ItemShape::Bow
        | ItemShape::Sceptre
        | ItemShape::Coin => false,
    }
}

/// The mesh key one shape and livery resolve to.
///
/// **Keyed on whether the mesh differs, which is a question about the shape rather than about
/// the livery.** #436 first answered it with `pit_depth` — a livery that displaces nothing
/// leaves the geometry alone, so surely the mesh is identical — and that was **wrong**, which
/// the review on that pull request caught: `blade_loft` writes the livery's own band into the
/// coordinates whether it displaces or not, so a blade wearing forged steel and a blade
/// wearing none are different meshes with identical positions. Collapsing them would have
/// dropped the forge marks off every dropped iron sword, silently, and the wood grain with
/// them.
///
/// What is true is narrower and belongs to the shape: only the blade's mesh is built against
/// a livery at all — see [`mesh_varies_with_livery`]. That still fixes what the collapse was
/// for. Giving the campfire a wood livery split the bundle roll the forge and the tent share
/// — three structures, one silhouette, two byte-identical meshes — because `create_visuals`
/// builds a bundle from `rolled_bundle_parts` and never looks at the livery.
///
/// The one place this rule is applied, so `create_visuals` and `mesh_for` cannot disagree
/// about which entry an item lands in, and
/// [`the_mesh_cache_separates_exactly_the_meshes_that_differ`] checks it against the builder
/// rather than against itself.
fn mesh_key(shape: ItemShape, livery: Option<Livery>) -> MeshKey {
    (shape, livery.filter(|_| mesh_varies_with_livery(shape)))
}

/// What decides which material a drop is drawn with.
///
/// The resolved colour, as it always was, plus the livery — which is a material fact, since it
/// arrives as `base_color_texture`. Keyed on the colour rather than on the item so that every
/// unknown id shares one placeholder material instead of letting a peer grow this resource by
/// walking 65,535 ids over time.
type MaterialKey = ([f32; 4], Option<Livery>);

/// The shared world-space meshes and the small set of item colours created so far.
///
/// Materials are keyed by their actual colour rather than by item id. Every
/// unknown id therefore shares the one placeholder material instead of letting a peer
/// grow this resource by walking through all 65,535 unknown ids over time.
#[derive(Resource, Debug)]
pub(super) struct DropVisuals {
    /// One mesh per shape **and livery**, shared by every drop and local body presenting as
    /// that pair — the reason a hundred dropped stones and one carried stone still cost one
    /// mesh rather than a hundred and one.
    ///
    /// **The livery is in the key because it decides geometry, not only colour.** A liveried
    /// blade is lofted through 31 rings and pitted where the field is strongest; an
    /// un-liveried one is the smooth three-section loft. They are two meshes, and a cache
    /// that could not tell them apart would hand one sword the other's silhouette. This is a
    /// widening of the existing key rather than an item-id exception smuggled into it: two
    /// items sharing a shape and a livery still share one mesh, which is what the cache is
    /// for.
    shapes: Vec<(MeshKey, Handle<Mesh>)>,
    /// The second shared mesh every bundle draws over its item-coloured roll.
    bundle_straps: Handle<Mesh>,
    /// The second shared mesh every blade draws under its item-coloured steel.
    ///
    /// **One asset for every sword, whatever its steel**, which is the whole reason the grip
    /// is a child rather than part of the blade's mesh: `hands.rs` reaches its wood by
    /// dividing `palette::LOG` out of *that* blade's colour, and a tint divided out of one
    /// steel baked into a mesh shared by two blades is right for one and silently wrong for
    /// the other. An absolute colour on its own mesh needs no division and no cache key.
    blade_grip: Handle<Mesh>,
    /// One material per colour **and livery**. The livery is a material fact — it arrives as
    /// `base_color_texture` — so this key needed the same widening for the same reason.
    materials: Vec<(MaterialKey, Handle<StandardMaterial>)>,
    /// The one image every liveried surface samples, held here so `material_for` needs no
    /// second argument and cannot be handed a different one.
    livery_image: Handle<Image>,
}

impl DropVisuals {
    /// The mesh one item id is drawn from.
    ///
    /// Read through [`item_shape`] — the same table the held view model and the inventory
    /// cell read, and this is its third reader rather than a second opinion. An id with no
    /// row answers [`ItemShape::Material`], which that function documents as the least
    /// wrong guess.
    pub(super) fn mesh_for(&self, item_id: u16) -> Handle<Mesh> {
        let key = mesh_key(item_shape(item_id), item_livery(item_id));
        self.shapes
            .iter()
            .find(|(candidate, _)| *candidate == key)
            .map(|(_, mesh)| mesh.clone())
            .unwrap_or_else(|| {
                // Unreachable: `create_visuals` builds one entry per `ItemShape::ALL` with no
                // livery, and one more for every pair `items::liveried_shapes` reports — so
                // every pair an item can present as has an entry. Answered rather than
                // unwrapped, because a drop is the last thing that should take the window
                // down, and an invisible pelt is a smaller bug than a crash.
                error!("no drop mesh for {key:?}");
                Handle::default()
            })
    }

    /// The second mesh and material one shape is drawn from, when it has one.
    ///
    /// **The one place that pairing lives**, so a drop and a body's fist cannot disagree
    /// about what a sword's grip is made of. A strap is the part of a bundle that is not its
    /// canvas; a grip is the part of a sword that is not its steel — and neither wears the
    /// item's own livery, because the livery belongs to the part this one is *not*.
    pub(super) fn second_piece_for(
        &mut self,
        shape: ItemShape,
        materials: &mut Assets<StandardMaterial>,
    ) -> Option<(Handle<Mesh>, Handle<StandardMaterial>)> {
        // **A grip wears a livery of its own, and a strap does not.** The livery belongs to
        // the *material*, so the part whose material is wood carries wood's — which is what
        // makes the grain on a dropped grip the same field the hand's grip reads. A strap is
        // worked leather, and worked leather has none.
        let (mesh, colour, livery) = match shape {
            ItemShape::Bundle => (self.bundle_straps.clone(), bundle_strap_linear_rgba(), None),
            ItemShape::Blade => (
                self.blade_grip.clone(),
                palette::linear_rgba(palette::LOG),
                Some(Livery::Wood),
            ),
            _ => return None,
        };
        Some((mesh, self.material_for_colour(colour, livery, materials)))
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
        // White preserves the shield mesh's separate bark and iron vertex colours.
        let colour = if matches!(
            item_id,
            super::crafting::ITEM_WOODEN_SHIELD | super::crafting::ITEM_WOODEN_SCEPTRE
        ) {
            [1.0; 4]
        } else {
            item_linear_rgba(item_id)
        };
        self.material_for_colour(colour, item_livery(item_id), materials)
    }

    fn material_for_colour(
        &mut self,
        colour: [f32; 4],
        livery: Option<Livery>,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        let key = (colour, livery);
        if let Some((_, material)) = self
            .materials
            .iter()
            .find(|(candidate, _)| *candidate == key)
        {
            return material.clone();
        }

        let [r, g, b, a] = colour;
        let material = materials.add(StandardMaterial {
            base_color: Color::linear_rgba(r, g, b, a),
            // **The same image the hand and the cell sample**, so agreement between the
            // surfaces is handle identity rather than a convention. An item with no livery
            // gets no texture at all and is the material it always was — the neutral band
            // exists for the meshes that share a material with a liveried one, and nothing
            // shares this one.
            base_color_texture: livery.map(|_| self.livery_image.clone()),
            perceptual_roughness: 0.85,
            ..default()
        });
        self.materials.push((key, material.clone()));
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
    /// Only the roll/body follows a changed item id; bundle straps keep their brown.
    item_coloured: bool,
}

#[derive(SystemParam)]
pub(super) struct DropAssets<'w> {
    visuals: Option<ResMut<'w, DropVisuals>>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
}

pub(super) fn create_visuals(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    liveries: Res<Liveries>,
) {
    let (_, bundle_straps) = rolled_bundle_parts(BUNDLE_DROP_SIZE);
    let build = |shape: ItemShape, livery: Option<Livery>| {
        if shape == ItemShape::Bundle {
            rolled_bundle_parts(BUNDLE_DROP_SIZE).0
        } else {
            drop_mesh(shape, livery)
        }
    };
    // Built from `ItemShape::ALL`, so a fifth shape gets a drop mesh by existing — and then
    // from the table, so every shape-and-livery pair an item actually presents as gets one
    // too. **Not the cross product**: a mesh for a combination no item is would be minted,
    // never drawn, and would make the count of entries say nothing about the count of things
    // that can be drawn.
    let mut shapes: Vec<(MeshKey, Handle<Mesh>)> = ItemShape::ALL
        .into_iter()
        .map(|shape| ((shape, None), meshes.add(build(shape, None))))
        .collect();
    for (shape, livery) in liveried_shapes() {
        let key = mesh_key(shape, Some(livery));
        // A livery that displaces nothing resolves to the entry the un-liveried shape already
        // holds — see [`mesh_key`] — so there is nothing to mint.
        if shapes.iter().any(|(seen, _)| *seen == key) {
            continue;
        }
        shapes.push((key, meshes.add(build(shape, Some(livery)))));
    }

    commands.insert_resource(DropVisuals {
        shapes,
        bundle_straps: meshes.add(bundle_straps),
        blade_grip: meshes.add(sword_grip_mesh(DROP_EDGE * BLADE_DROP_LENGTH)),
        materials: Vec::new(),
        livery_image: liveries.material_image(),
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
/// than which colour it is. So the arm calls [`sword_mesh_with`] with a drop-scale length
/// and gets the same weapon at a different size. Nothing is shared but the shape: this
/// module still mints its own asset, at its own scale, with its own materials, and the four
/// other arms below still author their own geometry.
///
/// Wildcard-free, so a fifth [`ItemShape`] does not compile until it can be dropped — the
/// same guarantee `ui::icon::parts` and `hands::item_mesh` already give.
///
/// **The livery reaches the blade and nothing else**, which is not an exception so much as
/// where the only liveried geometry is: a livery is a surface an item's *material* wears, and
/// only [`sword_mesh_with`] changes shape because of one. Every other arm ignores it, which
/// is why they take no argument and why a second liveried shape would have to say so here.
fn drop_mesh(shape: ItemShape, livery: Option<Livery>) -> Mesh {
    match shape {
        // A voxel on the ground is a small voxel.
        ItemShape::Block => Mesh::from(Cuboid::from_size(Vec3::splat(DROP_EDGE))),
        // A stub of something: rounded, so a pelt and a stone are not the same silhouette.
        ItemShape::Material => Mesh::from(Capsule3d::new(DROP_EDGE * 0.30, DROP_EDGE * 0.62)),
        // Long, thin, and lying at an angle — see `spin_and_bob`, which is what turns it.
        // The length is what it was when this was one box, so nothing about how far a
        // dropped sword reaches or how it tumbles moves.
        ItemShape::Blade => sword_mesh_with(DROP_EDGE * BLADE_DROP_LENGTH, livery),
        // A carried structure is packed gear: one horizontal canvas roll with two raised
        // collars. The four structures share this silhouette and their item-table colour
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
        ItemShape::Armour => armour_mesh(),
        ItemShape::Shield => shield_mesh(DROP_EDGE * 2.0),
        ItemShape::Bow => bow_mesh(DROP_EDGE * BLADE_DROP_LENGTH),
        ItemShape::Sceptre => sceptre_mesh(DROP_EDGE * BLADE_DROP_LENGTH),
        // A coin lies where it fell: a disc a third of a drop across and a tenth of one
        // thick. Its own silhouette rather than the material stub's, which is what
        // `every_shape_is_drawn_from_its_own_silhouette` insists on and what stops a purse
        // on the ground looking like a pile of ore.
        ItemShape::Coin => Mesh::from(Cylinder::new(DROP_EDGE * 0.34, DROP_EDGE * 0.10)),
    }
}

/// A compact cuirass silhouette: one body plate and two raised shoulders.
fn armour_mesh() -> Mesh {
    let body_size = Vec3::new(DROP_EDGE * 0.64, DROP_EDGE * 0.72, DROP_EDGE * 0.22);
    let shoulder_size = Vec3::new(DROP_EDGE * 0.28, DROP_EDGE * 0.18, DROP_EDGE * 0.28);
    let mut armour = Mesh::from(Cuboid::from_size(body_size));
    let shoulders = [-1.0, 1.0].map(|side| {
        Mesh::from(Cuboid::from_size(shoulder_size)).translated_by(Vec3::new(
            side * DROP_EDGE * 0.34,
            DROP_EDGE * 0.27,
            0.0,
        ))
    });
    merge_all(&mut armour, shoulders, "dropped armour");
    armour
}

/// One rolled, strapped load for every carried structure.
///
/// The complete geometry is merged for shape tests; the running renderer keeps the roll
/// and straps as two shared meshes so the first can take the item's colour and the second
/// can remain brown. Both visual children receive the same cosmetic transform.
fn bundle_mesh() -> Mesh {
    let (mut roll, straps) = rolled_bundle_parts(BUNDLE_DROP_SIZE);
    merge_all(&mut roll, [straps], "dropped packed-gear bundle");
    roll
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
            if visual.owner == owner && visual.item_coloured {
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
            DropVisual {
                owner,
                item_coloured: true,
            },
            Mesh3d(mesh),
            MeshMaterial3d(material),
            Transform::default(),
        ));
        // The second child, for the shapes whose colour is not all the item's — see
        // [`DropVisuals::second_piece_for`], which the body's fist reads too.
        if let Some((mesh, material)) =
            visuals.second_piece_for(item_shape(state.item_id), materials)
        {
            parent.spawn((
                DropVisual {
                    owner,
                    item_coloured: false,
                },
                Mesh3d(mesh),
                MeshMaterial3d(material),
                Transform::default(),
            ));
        }
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
            inventory_slots: 37,
            hotbar_slots: 9,
            equipment_slots: 4,
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

    /// **A dropped sword is drawn in two pieces, and the second one is wood.**
    ///
    /// This is the divergence #419 pinned and could not close: the grip was turned on the
    /// ground and still steel, because `hands.rs` reaches its wood by dividing `palette::LOG`
    /// out of *that* blade's own colour, and `DropVisuals` shares one mesh per shape and
    /// livery between blades — so a tint divided out of one steel would be right for one
    /// sword and silently wrong for the other.
    ///
    /// A second child with an absolute wood material needs no division and no cache key. Read
    /// off the entities the running app actually spawns, and asserted for **both** blades,
    /// because one alone cannot tell a shared absolute colour from a coincidence.
    #[test]
    fn a_dropped_sword_carries_its_grip_in_wood() {
        let mut app = headless_player();
        deliver(
            &mut app,
            snapshot(
                1,
                vec![],
                vec![
                    drop(
                        10,
                        [1.0, 64.0, 2.0],
                        crate::player::combat::ITEM_RUSTY_SWORD,
                    ),
                    drop(
                        11,
                        [3.0, 65.0, 4.0],
                        crate::player::crafting::ITEM_IRON_SWORD,
                    ),
                ],
            ),
            Instant::now(),
        );
        app.update();

        let world = app.world_mut();
        let log = palette::linear_rgba(palette::LOG);
        let mut anchors = world.query::<(&DroppedItem, &Children)>();
        let found: Vec<(u16, Vec<Entity>)> = anchors
            .iter(world)
            .map(|(drop, children)| (drop.item_id, children.iter().collect()))
            .collect();
        assert_eq!(found.len(), 2, "the two swords did not both spawn");

        for (item_id, children) in found {
            assert_eq!(
                children.len(),
                2,
                "item {item_id} is drawn in {} pieces, not a blade and a grip",
                children.len()
            );
            let colours: Vec<[f32; 4]> = children
                .iter()
                .map(|child| {
                    let handle = world
                        .get::<MeshMaterial3d<StandardMaterial>>(*child)
                        .expect("every piece draws a material")
                        .0
                        .clone();
                    world
                        .resource::<Assets<StandardMaterial>>()
                        .get(&handle)
                        .expect("the piece's material")
                        .base_color
                        .to_linear()
                        .to_f32_array()
                })
                .collect();
            assert!(
                colours.iter().any(|colour| {
                    (0..3).all(|channel| (colour[channel] - log[channel]).abs() < 1e-5)
                }),
                "item {item_id} has no piece drawn in palette::LOG: {colours:?}"
            );
            assert!(
                colours.iter().any(|colour| {
                    let steel = item_linear_rgba(item_id);
                    (0..3).all(|channel| (colour[channel] - steel[channel]).abs() < 1e-5)
                }),
                "item {item_id} has no piece drawn in its own steel: {colours:?}"
            );
        }
    }

    /// **A dropped blade's mesh reads its own material's band**, which is the consequence the
    /// key exists to protect and the one a collapsed pair loses.
    ///
    /// The rule and the builder agreeing is one thing;
    /// [`the_mesh_cache_separates_exactly_the_meshes_that_differ`] checks that. What a player
    /// would actually have seen is this: a dropped iron sword resolving the un-liveried mesh
    /// and rendering with neutral coordinates, so its forge marks are simply absent — no error,
    /// no red test, a blade that looks a little plain.
    #[test]
    fn a_dropped_blade_reads_its_own_bands() {
        let mut app = headless_player();
        app.update();
        let world = app.world_mut();
        let visuals = world.resource::<DropVisuals>();
        let neutral = super::super::livery::neutral_uv();

        for item_id in [
            crate::player::combat::ITEM_RUSTY_SWORD,
            crate::player::crafting::ITEM_IRON_SWORD,
        ] {
            let livery = item_livery(item_id).expect("both blades wear one");
            let handle = visuals.mesh_for(item_id);
            let mesh = world
                .resource::<Assets<Mesh>>()
                .get(&handle)
                .expect("the blade's drop mesh");
            let Some(bevy::mesh::VertexAttributeValues::Float32x2(uvs)) =
                mesh.attribute(Mesh::ATTRIBUTE_UV_0)
            else {
                panic!("item {item_id}'s drop mesh must carry Float32x2 coordinates");
            };

            let sampled: Vec<[f32; 2]> = uvs.iter().copied().filter(|uv| *uv != neutral).collect();
            assert!(
                !sampled.is_empty(),
                "item {item_id}'s dropped blade samples nothing, so its livery is invisible \
                 on the ground"
            );
            for uv in &sampled {
                assert!(
                    super::super::livery::band_holds(livery, *uv),
                    "item {item_id}'s dropped blade samples {uv:?}, outside {livery:?}'s band"
                );
            }
        }
    }

    /// **The mesh cache distinguishes exactly the meshes that differ**, no more and no fewer.
    ///
    /// [`mesh_key`] collapses entries deliberately — a livery that changes nothing about a
    /// mesh should not mint a byte-identical duplicate of it — and the danger in that is
    /// collapsing two that are *not* identical, which loses one of them silently: the drop
    /// resolves the wrong entry and renders with somebody else's coordinates.
    ///
    /// So this asks the **builder**, not the rule. For every shape and every livery it builds
    /// both meshes and compares them, then requires the key to separate them exactly when
    /// they differ. A rule that is right by accident and a rule that is right fail this test
    /// differently.
    #[test]
    fn the_mesh_cache_separates_exactly_the_meshes_that_differ() {
        let build = |shape: ItemShape, livery: Option<Livery>| {
            if shape == ItemShape::Bundle {
                rolled_bundle_parts(BUNDLE_DROP_SIZE).0
            } else {
                drop_mesh(shape, livery)
            }
        };
        let readable = |mesh: &Mesh| {
            let positions = match mesh.attribute(Mesh::ATTRIBUTE_POSITION) {
                Some(bevy::mesh::VertexAttributeValues::Float32x3(values)) => values.clone(),
                _ => Vec::new(),
            };
            let uvs = match mesh.attribute(Mesh::ATTRIBUTE_UV_0) {
                Some(bevy::mesh::VertexAttributeValues::Float32x2(values)) => values.clone(),
                _ => Vec::new(),
            };
            (positions, uvs)
        };

        let mut wrong: Vec<String> = Vec::new();
        for shape in ItemShape::ALL {
            let plain = readable(&build(shape, None));
            for livery in Livery::ALL {
                let worn = readable(&build(shape, Some(livery)));
                let differ = worn != plain;
                let separated = mesh_key(shape, Some(livery)) != mesh_key(shape, None);
                if separated != differ {
                    wrong.push(format!(
                        "{shape:?}+{livery:?}: builds {} / cache {}",
                        if differ { "different" } else { "identical" },
                        if separated { "separates" } else { "collapses" }
                    ));
                }
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// **A grip is grained on both surfaces, and it is the same grain.**
    ///
    /// The hand reaches `palette::LOG` by dividing it out of the blade's own steel and the
    /// world by an absolute material — two arrangements, deliberately, because a shared mesh
    /// cannot carry a per-item division. What #436 adds is that the *grain* is the same field
    /// read from the same band either way, so a grip cannot be wood in one place and grained
    /// in the other.
    ///
    /// Asserted through the image the running app holds, not through the generator, which is
    /// what makes it agreement rather than two copies of one formula.
    #[test]
    fn a_grip_is_grained_wherever_it_is_drawn() {
        let mut app = headless_player();
        deliver(
            &mut app,
            snapshot(
                1,
                vec![],
                vec![drop(
                    12,
                    [1.0, 64.0, 2.0],
                    crate::player::combat::ITEM_RUSTY_SWORD,
                )],
            ),
            Instant::now(),
        );
        app.update();

        let world = app.world_mut();
        let image = world.resource::<Liveries>().material_image();

        // The world's grip: its material carries the image, and its mesh reads the wood band.
        let mut anchors = world.query::<(&DroppedItem, &Children)>();
        let children: Vec<Entity> = anchors
            .iter(world)
            .next()
            .expect("the dropped sword")
            .1
            .iter()
            .collect();
        let log = palette::linear_rgba(palette::LOG);
        let grip = children
            .into_iter()
            .find(|child| {
                let handle = world
                    .get::<MeshMaterial3d<StandardMaterial>>(*child)
                    .expect("every piece draws a material")
                    .0
                    .clone();
                let colour = world
                    .resource::<Assets<StandardMaterial>>()
                    .get(&handle)
                    .expect("the piece's material")
                    .base_color
                    .to_linear()
                    .to_f32_array();
                (0..3).all(|channel| (colour[channel] - log[channel]).abs() < 1e-5)
            })
            .expect("the dropped sword has a grip");

        let material = world
            .get::<MeshMaterial3d<StandardMaterial>>(grip)
            .expect("the grip draws a material")
            .0
            .clone();
        assert_eq!(
            world
                .resource::<Assets<StandardMaterial>>()
                .get(&material)
                .expect("the grip's material")
                .base_color_texture
                .clone(),
            Some(image),
            "the dropped grip's material carries no livery image, so its wood has no grain"
        );

        // And the mesh it draws reads the wood band rather than any other. The held grip is
        // the same `grip_mesh` at another scale, and `a_held_grip_reads_the_wood_band` asserts
        // it off the hand's own composition — the two together are what make the grain one
        // fact rather than two that agree.
        let mesh_handle = world
            .get::<Mesh3d>(grip)
            .expect("the grip draws a mesh")
            .0
            .clone();
        let meshes = world.resource::<Assets<Mesh>>();
        let dropped = meshes.get(&mesh_handle).expect("the grip's mesh");
        let Some(bevy::mesh::VertexAttributeValues::Float32x2(uvs)) =
            dropped.attribute(Mesh::ATTRIBUTE_UV_0)
        else {
            panic!("the dropped grip must carry Float32x2 texture coordinates");
        };
        assert!(!uvs.is_empty(), "the dropped grip carries no coordinates");
        for uv in uvs {
            assert!(
                super::super::livery::band_holds(Livery::Wood, *uv),
                "the dropped grip samples {uv:?}, outside wood's own band"
            );
        }
    }

    /// **Every other shape is drawn in the pieces it always was**, which is the regression a
    /// second child invites: one extra entity per drop, everywhere.
    #[test]
    fn only_a_blade_and_a_bundle_are_drawn_in_two_pieces() {
        let mut app = headless_player();
        app.update();
        let world = app.world_mut();
        let mut materials = world.remove_resource::<Assets<StandardMaterial>>().unwrap();
        let mut visuals = world.remove_resource::<DropVisuals>().unwrap();
        for shape in ItemShape::ALL {
            let second = visuals.second_piece_for(shape, &mut materials).is_some();
            let want = matches!(shape, ItemShape::Blade | ItemShape::Bundle);
            assert_eq!(
                second, want,
                "{shape:?} has a second piece = {second}, want {want}"
            );
        }
        world.insert_resource(visuals);
        world.insert_resource(materials);
    }

    /// **The four surfaces that draw one sword resolve the same `Handle<Image>`.**
    ///
    /// This is the criterion the whole of #418 exists for. Before it, the rust was reached by
    /// `if item_id == ITEM_RUSTY_SWORD` inside `hands::item_mesh` and nowhere else: the
    /// ground drop, the third-person fist and the inventory cell each drew clean steel, the
    /// four surfaces already disagreed, and **nothing measured it**. Choosing an asset over a
    /// per-vertex patina is what makes agreement assertable in one line — handle identity
    /// rather than a convention somebody maintains.
    ///
    /// **Read off the running app**, not from a generator called four times: the hand's
    /// handle comes off the real view-model entity's material, the cell's off a real
    /// `ImageNode` component, and the ground and the body share `DropVisuals::material_for`
    /// because they are literally the same call — `refresh_body_held_item` reaches it through
    /// `BodyHeldAssets::presentation`, which
    /// `the_local_body_holds_the_authoritative_selected_item_at_world_scale`
    /// pins.
    #[test]
    fn all_four_surfaces_that_draw_a_sword_sample_one_image() {
        let mut app = headless_player();
        app.update();

        // 1. The first-person hand. Its material is shared by the fist, the arm and whatever
        //    is held, which is why the neutral band exists at all.
        let world = app.world_mut();
        let mut view_models =
            world.query_filtered::<&MeshMaterial3d<StandardMaterial>, With<crate::player::hands::HeldItem>>();
        let hand_material = view_models
            .iter(world)
            .next()
            .expect("the view model has a material")
            .0
            .clone();
        let hand = world
            .resource::<Assets<StandardMaterial>>()
            .get(&hand_material)
            .expect("the hand's material")
            .base_color_texture
            .clone()
            .expect("the hand's material carries the livery image");

        // **Both liveried materials, not only the rusty one.** #420 is the generalisation,
        // and its whole claim is that the second material costs a row — so the test the
        // arrangement exists for has to cover the second material, or it measures the first
        // one twice.
        let liveries = world.resource::<Liveries>().clone();
        for item_id in [
            crate::player::combat::ITEM_RUSTY_SWORD,
            crate::player::crafting::ITEM_IRON_SWORD,
        ] {
            // 2 and 3. The ground drop and the third-person body, which are one call.
            let mut materials = world.remove_resource::<Assets<StandardMaterial>>().unwrap();
            let drop_material = world
                .resource_mut::<DropVisuals>()
                .material_for(item_id, &mut materials);
            let drop = materials
                .get(&drop_material)
                .expect("the drop's material")
                .base_color_texture
                .clone()
                .expect("a liveried drop's material carries the livery image");
            world.insert_resource(materials);

            // 4. The inventory cell, read off a real `ImageNode` rather than the resource.
            //
            // **The world holds no image node before this iteration spawns one**, asserted
            // rather than assumed, because the count below is what tells the cell apart from
            // whatever the previous iteration left behind. `EntityWorldMut::despawn` takes
            // its `Children` with it in Bevy 0.19 — its own doc says "this will recursively
            // despawn `Children`", and `despawn_recursive` no longer exists — so the cleanup
            // at the end of the loop is enough. This is the line that says so.
            let mut existing = world.query::<&ImageNode>();
            assert_eq!(
                existing.iter(world).count(),
                0,
                "an earlier iteration left image nodes behind, so item {item_id}'s count is \
                 not its own"
            );
            let icon = crate::ui::icon::StackIcon {
                shape: item_shape(item_id),
                colour: Color::WHITE,
                livery: item_livery(item_id),
            };
            let host = world.spawn_empty().id();
            world.commands().entity(host).with_children(|host| {
                crate::ui::icon::spawn(host, icon, Some(&liveries));
            });
            world.flush();
            let mut images = world.query::<&ImageNode>();
            let cell: Vec<ImageNode> = images.iter(world).cloned().collect();
            assert_eq!(
                cell.len(),
                1,
                "item {item_id}'s cell drew {} image nodes, and a blade has one liveried \
                 rectangle",
                cell.len()
            );

            assert_eq!(hand, drop, "item {item_id}: the hand and the drop differ");
            assert_eq!(
                drop, cell[0].image,
                "item {item_id}: the drop and the cell differ"
            );

            // **And the cell reads this material's own band**, which is the half handle
            // identity stops covering once one image holds two materials: four surfaces
            // sharing a handle and sampling different rows of it is the same divergence one
            // level down.
            let livery = item_livery(item_id).expect("a liveried blade");
            assert_eq!(
                cell[0].rect,
                Some(crate::player::field_rect(livery)),
                "item {item_id}'s cell samples the wrong band of the shared image"
            );
            world.entity_mut(host).despawn();
            world.flush();
        }
    }

    /// **An item with no livery draws exactly as it did**, in every surface — which is the
    /// regression this change could most easily cause, because it is a whole inventory drawn
    /// wrong rather than one sword.
    ///
    /// Swept over every item the client knows rather than over the sword, and the `None` case
    /// is the one that matters here: no texture on its material, no image node in its cell,
    /// and the same mesh it would have had before the key was widened.
    #[test]
    fn an_item_with_no_livery_is_untouched_on_every_surface() {
        let mut app = headless_player();
        app.update();
        let world = app.world_mut();
        let liveries = world.resource::<Liveries>().clone();
        let mut materials = world.remove_resource::<Assets<StandardMaterial>>().unwrap();

        let mut plain = 0;
        for item_id in crate::player::known_item_ids() {
            let livery = item_livery(item_id);
            let material = world
                .resource_mut::<DropVisuals>()
                .material_for(item_id, &mut materials);
            let texture = materials
                .get(&material)
                .expect("every item resolves a material")
                .base_color_texture
                .is_some();
            assert_eq!(
                texture,
                livery.is_some(),
                "item {item_id} has a livery = {}, and its drop material carries a texture = \
                 {texture}",
                livery.is_some()
            );

            let icon = crate::ui::icon::StackIcon {
                shape: item_shape(item_id),
                colour: Color::WHITE,
                livery,
            };
            let host = world.spawn_empty().id();
            world.commands().entity(host).with_children(|host| {
                crate::ui::icon::spawn(host, icon, Some(&liveries));
            });
            world.flush();
            let mut images = world.query::<&ImageNode>();
            let drawn = images.iter(world).count();
            // **A cell draws a livery only where its picture has a rectangle for one.** An
            // iron helm names `ForgedSteel` and its cell is a plate and two shoulders with
            // no edge to mark — see `ui::icon::draws_a_livery`, which is the drawing
            // decision this reads rather than a second opinion about it.
            let reachable =
                livery.is_some() && crate::ui::icon::draws_a_livery(item_shape(item_id));
            assert_eq!(
                drawn > 0,
                reachable,
                "item {item_id} can reach a livery in a cell = {reachable} and drew {drawn} \
                 image nodes"
            );
            world.entity_mut(host).despawn();
            world.flush();

            if livery.is_none() {
                plain += 1;
            }
        }
        world.insert_resource(materials);
        // Most of the table, which is the shape the "a livery has to earn its place" rule
        // gives it — and the reason this sweep is worth running at all. A count would have to
        // move every time a material earns one, which is exactly what it should not do.
        assert!(
            plain * 2 > crate::player::known_item_ids().count(),
            "only {plain} items with no livery, so this sweeps nothing"
        );
    }

    /// **The iron sword on the ground is the smooth blade and the rusty one is pitted**, which
    /// is the pair the widened mesh key exists for.
    ///
    /// They share `ItemShape::Blade`, so a cache that could not tell them apart would hand one
    /// of them the other's silhouette — and it would be the *shared* asset, so the body's fist
    /// would be wrong in the same breath.
    #[test]
    fn the_two_blades_are_two_meshes_and_every_other_shape_is_still_one() {
        let mut app = headless_player();
        app.update();
        let world = app.world_mut();
        let visuals = world.resource::<DropVisuals>();
        let rusty = visuals.mesh_for(crate::player::combat::ITEM_RUSTY_SWORD);
        let iron = visuals.mesh_for(crate::player::crafting::ITEM_IRON_SWORD);
        assert_ne!(
            rusty, iron,
            "both blades resolve one mesh, so the pitted and the smooth blade are the same drop"
        );

        let meshes = world.resource::<Assets<Mesh>>();
        let count = |handle: &Handle<Mesh>| {
            meshes
                .get(handle)
                .expect("a blade's drop mesh")
                .count_vertices()
        };
        assert!(
            count(&rusty) > count(&iron) * 4,
            "the rusty blade is {} vertices against the iron blade's {}, which is not a \
             subdivision",
            count(&rusty),
            count(&iron)
        );

        // And the widening did not turn the cache into one entry per item: every pair of
        // items that shares a shape *and* a livery still shares one mesh, which is what a
        // shape-keyed cache was for.
        let visuals = world.resource::<DropVisuals>();
        let mut shared = 0;
        for left in crate::player::known_item_ids() {
            for right in crate::player::known_item_ids() {
                if left >= right {
                    continue;
                }
                if (item_shape(left), item_livery(left)) != (item_shape(right), item_livery(right))
                {
                    continue;
                }
                shared += 1;
                assert_eq!(
                    visuals.mesh_for(left),
                    visuals.mesh_for(right),
                    "items {left} and {right} share a shape and a livery and not a mesh"
                );
            }
        }
        assert!(
            shared > 10,
            "only {shared} sharing pairs, so this sweeps nothing"
        );
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
                .find(|(candidate, _)| *candidate == (shape, None))
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
        let mesh = drop_mesh(ItemShape::Bundle, None);
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

    /// Tent, forge and campfire share the roll and straps, while keeping their own colour.
    #[test]
    fn bundled_structures_share_roll_and_brown_straps() {
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
        let mut drawn = world.query::<(&DropVisual, &Mesh3d, &MeshMaterial3d<StandardMaterial>)>();
        let presentations: Vec<_> = drawn
            .iter(world)
            .map(|(visual, mesh, material)| {
                (
                    visual.owner,
                    visual.item_coloured,
                    mesh.0.clone(),
                    material.0.clone(),
                )
            })
            .collect();
        assert_eq!(
            presentations.len(),
            6,
            "each bundle is a roll and its straps"
        );

        let rolls: Vec<_> = presentations
            .iter()
            .filter(|(_, item_coloured, _, _)| *item_coloured)
            .collect();
        let straps: Vec<_> = presentations
            .iter()
            .filter(|(_, item_coloured, _, _)| !*item_coloured)
            .collect();
        assert_eq!(rolls.len(), 3);
        assert_eq!(straps.len(), 3);
        assert!(
            rolls.iter().all(|(_, _, mesh, _)| *mesh == rolls[0].2),
            "the three bundles did not share one roll mesh"
        );
        assert!(
            straps.iter().all(|(_, _, mesh, _)| *mesh == straps[0].2),
            "the three bundles did not share one strap mesh"
        );
        for (index, (_, _, _, material)) in rolls.iter().enumerate() {
            for (_, _, _, other) in &rolls[index + 1..] {
                assert_ne!(material, other, "two Bundle colours became one material");
            }
        }
        assert!(
            straps
                .iter()
                .all(|(_, _, _, material)| *material == straps[0].3),
            "the straps do not share one brown material"
        );

        let materials = world.resource::<Assets<StandardMaterial>>();
        let expected = bundle_strap_linear_rgba();
        let [r, g, b, a] = expected;
        assert_eq!(
            materials
                .get(&straps[0].3)
                .expect("the strap material exists")
                .base_color,
            Color::linear_rgba(r, g, b, a)
        );
        for owner in rolls.iter().map(|(owner, _, _, _)| *owner) {
            assert!(
                straps
                    .iter()
                    .any(|(strap_owner, _, _, _)| *strap_owner == owner),
                "a bundle roll has no straps"
            );
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
            .find(|(key, _)| *key == (ItemShape::Blade, None))
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
    fn only_inventory_and_menu_hide_a_drop_without_removing_it() {
        let mut app = headless_player();
        deliver(
            &mut app,
            snapshot(1, vec![], vec![drop(10, [0.0, 64.0, 0.0], palette::STONE)]),
            Instant::now(),
        );
        app.update();
        assert_eq!(only_anchor_visibility(&mut app), Visibility::Visible);

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Chat;
        app.update();
        assert_eq!(only_anchor_visibility(&mut app), Visibility::Visible);

        for mode in [InputMode::Loot, InputMode::Vendor] {
            *app.world_mut().resource_mut::<InputMode>() = mode;
            app.update();
            assert_eq!(
                only_anchor_visibility(&mut app),
                Visibility::Visible,
                "the centred {mode:?} panel hid the drop"
            );
        }

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
