//! Tents and forges, drawn from the newest authoritative snapshot, and the two requests
//! that ask for one.
//!
//! A structure exists exactly while the newest snapshot names its id — the same rule
//! [`super::mobs`] and [`super::drops`] follow, and for the same reason: taken back by its
//! owner, collapsed under a block somebody broke, and simply out of view are one fact on
//! the wire, and this client is not entitled to tell them apart.
//!
//! **Nothing appears locally when a placement is asked for.** A `PlaceStructureRequest`
//! leaves and that is all: the server owns reach, whether the footprint is clear and
//! supported, whether the slot really holds a tent, and whether this player is allowed
//! another one. A refusal is silence, exactly as it is for a block edit, so there is no
//! ghost to withdraw and no deadline to withdraw it on.
//!
//! **Structures never move**, so they are not on the entity-motion path at all. There is
//! no position and no velocity in `StructureState` — an anchor cell and a `Facing` say
//! everything — and [`SnapshotBuffer::structures`] hands the newest snapshot's list over
//! without a `now` to sample at. A building cannot be interpolated because there is
//! nothing here to interpolate.
//!
//! ## The footprint arithmetic mirrors the server's, and must stay in step with it
//!
//! [`TENT_FOOTPRINT`], [`FORGE_FOOTPRINT`], [`TENT_HEADROOM`], [`FORGE_HEADROOM`] and
//! [`rotate_offset`] are copies of `tentFootprint`, `forgeFootprint`, `tentHeadroom`,
//! `forgeHeadroom` and `rotateOffset` in `server/internal/game/structure.go`. The server
//! validates the footprint and this side draws it; a mismatch is a tent that visibly does
//! not cover the ground the server says it covers.
//!
//! **The anchor is the ground cell**, on both sides. The structure stands in the air
//! *above* it, which is why every mesh below is authored from a base plane one block over
//! the anchor.

use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI, TAU};

use bevy::prelude::*;

use super::camera::{AimCamera, WorldCamera};
use super::combat;
use super::constants::MAX_REACH;
use super::interpolate::SnapshotBuffer;
use super::target::{ApplyTargetInput, BlockTarget, DrawTargetHighlight, cell_outline_mesh};
use super::{
    ApplySnapshots, InputCadence, InputGate, InputMode, LookState, ViewMode, merge_all,
    set_if_changed,
};
use crate::net::{
    BlockCoord, Facing, Outbound, PlaceStructureRequest, RemoveStructureRequest, Sent, Session,
    StructureKind, StructureState, encode_place_structure_request, encode_remove_structure_request,
};

/// The control that plants a structure — the same press that places a block, because a
/// tent and a stone block are the same gesture to a player.
const PLACE_BUTTON: MouseButton = MouseButton::Right;

/// The control that takes one back — the same press that mines and swings.
const REMOVE_BUTTON: MouseButton = MouseButton::Left;

/// The three items that plant a structure, as `server/internal/game/items.go` appends
/// them.
///
/// Presentation and routing only, exactly as [`combat::ITEM_RUSTY_SWORD`] is. They cannot
/// make another item plantable and they cannot make these three legal: the server reads
/// its own registry, and a placement naming a slot of stone is refused there whatever
/// these constants say.
pub(super) const ITEM_FORGE: u16 = 8;
pub(super) const ITEM_TENT: u16 = 9;
pub(super) const ITEM_CAMPFIRE: u16 = 12;

/// Modes whose UI owns the view instead of the 3D world. The same rule mobs and drops
/// obey — a snapshot-driven visual is hidden while a panel owns the screen, and hidden
/// rather than despawned, so opening a pack cannot be mistaken for anything happening in
/// the world.
const HIDDEN_INPUT_MODES: [InputMode; 3] = [InputMode::Inventory, InputMode::Loot, InputMode::Menu];

// ---------------------------------------------------------------------------
// The footprint, mirrored from the server
// ---------------------------------------------------------------------------

/// The ground cells a tent rests on, as `(dx, dz)` offsets from the anchor in the
/// canonical North orientation. Symmetric, so rotating them is a no-op — which is not a
/// reason to special-case it, because which way a tent faces is what its opening shows.
const TENT_FOOTPRINT: [[i32; 2]; 9] = [
    [-1, -1],
    [0, -1],
    [1, -1],
    [-1, 0],
    [0, 0],
    [1, 0],
    [-1, 1],
    [0, 1],
    [1, 1],
];

/// The anvil on the anchor and the hearth one step along the facing. North is -Z, so the
/// canonical hearth offset is `(0, -1)`.
const FORGE_FOOTPRINT: [[i32; 2]; 2] = [[0, 0], [0, -1]];

/// The one cell a campfire burns on, mirroring `campfireFootprint`.
///
/// A single cell, and symmetric, so rotating it is the identity — the facing is still
/// carried and still drawn, because the wire has one and the two sides derive the same
/// cells the same way whether or not the answer moves.
const CAMPFIRE_FOOTPRINT: [[i32; 2]; 1] = [[0, 0]];

/// How many cells of air each kind needs above every cell of its footprint.
///
/// Two for a tent, because a player has to be able to stand up inside the thing they
/// respawn in; one for a forge, which is waist-high furniture nobody stands inside, and
/// one for a campfire, which is lower still.
const TENT_HEADROOM: i32 = 2;
const FORGE_HEADROOM: i32 = 1;
const CAMPFIRE_HEADROOM: i32 = 1;

/// Every kind this module draws, so a fold over the whole contract has one place to read.
///
/// The one link here the compiler cannot check, and deliberately the only one:
/// [`footprint_offsets`] and [`headroom`] are exhaustive matches, so a new member of
/// [`StructureKind`] cannot compile without visiting them — and `StructureKind::from_wire`
/// admits a member only in the commit that teaches this module to draw it, so whoever adds
/// one is already standing here. A member missing from this list costs preview cells and
/// nothing worse; [`MAX_FOOTPRINT_CELLS`] is where that bound is kept.
const ALL_STRUCTURE_KINDS: [StructureKind; 3] = [
    StructureKind::Tent,
    StructureKind::Forge,
    StructureKind::Campfire,
];

/// The footprint offsets for one kind, in the canonical North orientation.
///
/// A `const fn` so [`MAX_FOOTPRINT_CELLS`] can fold over it — the ghost's capacity is then
/// derived from this match rather than from whichever array happens to be widest today.
const fn footprint_offsets(kind: StructureKind) -> &'static [[i32; 2]] {
    match kind {
        StructureKind::Tent => &TENT_FOOTPRINT,
        StructureKind::Forge => &FORGE_FOOTPRINT,
        StructureKind::Campfire => &CAMPFIRE_FOOTPRINT,
    }
}

/// How many cells of air one kind needs above each of its footprint cells.
fn headroom(kind: StructureKind) -> i32 {
    match kind {
        StructureKind::Tent => TENT_HEADROOM,
        StructureKind::Forge => FORGE_HEADROOM,
        StructureKind::Campfire => CAMPFIRE_HEADROOM,
    }
}

/// The most cells any footprint in this contract covers — today the tent's 3x3.
///
/// **Folded over [`ALL_STRUCTURE_KINDS`], not read off one array.** Written as
/// `TENT_FOOTPRINT.len()` this tracked the tent widening and nothing else while claiming
/// to track every kind: a member arriving with a wider footprint would have left it at
/// nine, and [`footprint_preview`] would have indexed past the end of a fixed-size array —
/// a panic on every frame the thing was in hand, in the one module whose whole argument is
/// that the preview is a picture and never a verdict.
///
/// What is machine-checked now is that this covers every kind [`ALL_STRUCTURE_KINDS`]
/// names. What is not, and cannot be, is that the list names every member — so
/// [`footprint_preview_of`] fills what fits and stops there, and a forgotten kind costs a
/// ghost drawn short instead of a client that dies while it is selected. Drawing short is
/// already the shape of this code rather than a new concession: the same number sizes the
/// pool [`spawn_footprint_ghost`] spawns once, and [`move_the_footprint_ghost`] hides
/// whatever a narrower footprint leaves over.
const MAX_FOOTPRINT_CELLS: usize = max_footprint_cells();

/// The fold behind [`MAX_FOOTPRINT_CELLS`] — a `const fn` and a `while`, because an array
/// length has to be known before there is an iterator to take a `max` of.
const fn max_footprint_cells() -> usize {
    let mut widest = 0;
    let mut index = 0;
    while index < ALL_STRUCTURE_KINDS.len() {
        let cells = footprint_offsets(ALL_STRUCTURE_KINDS[index]).len();
        if cells > widest {
            widest = cells;
        }
        index += 1;
    }
    widest
}

/// The cells the structure in hand would rest on this frame, or none.
///
/// **A picture, never a verdict.** These are the cells the *server's* footprint
/// arithmetic names for the kind, the facing and the anchor — the same arithmetic, on the
/// same inputs — and nothing here asks whether the ground under them will hold. Whether
/// it does is the server's answer and arrives as an `ActionRefused`; drawing this in one
/// colour and refusing to tint it by a local verdict is what keeps the rule in one place.
///
/// Fixed-size and `Copy`, so the frame that recomputes it allocates nothing and
/// [`set_if_changed`] can compare it: an outline whose transform is rewritten every frame
/// repropagates through the whole hierarchy for the rest of the session.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct FootprintPreview {
    cells: [IVec3; MAX_FOOTPRINT_CELLS],
    len: usize,
}

impl FootprintPreview {
    /// The cells to outline, in world block coordinates. Empty when nothing is in hand or
    /// nothing is aimed at.
    pub(super) fn cells(&self) -> &[IVec3] {
        &self.cells[..self.len]
    }

    /// Whether the ghost is being drawn at all — which is also the question the single
    /// voxel outline asks before drawing itself, since one press cannot mean two things.
    pub(super) fn active(&self) -> bool {
        self.len > 0
    }
}

/// The ground cells a structure of this kind and facing would rest on, anchored at
/// `anchor`.
///
/// The mirror of `footprintOf` in `server/internal/game/structure.go`, and mirrored down
/// to the arithmetic rather than to the answer: the same offsets through the same
/// rotation, so the two sides cannot drift into agreeing about a tent and disagreeing
/// about a forge. The headroom half is deliberately not drawn — a column of nine boxes
/// two blocks tall is a wall, not a preview — but it is what the server checks and it is
/// why [`headroom`] exists beside this.
fn footprint_preview(kind: StructureKind, facing: Facing, anchor: IVec3) -> FootprintPreview {
    footprint_preview_of(footprint_offsets(kind), facing, anchor)
}

/// The body of [`footprint_preview`], taking the offsets rather than the kind.
///
/// Split out for the one case no [`StructureKind`] in this build can reach: offsets wider
/// than [`MAX_FOOTPRINT_CELLS`]. They fill while there is room and are dropped afterwards
/// — `get_mut` rather than `cells[len]`, so a kind added to [`footprint_offsets`] and
/// forgotten in [`ALL_STRUCTURE_KINDS`] costs a short ghost rather than an
/// index-out-of-bounds every frame it is held. Nothing but a test can hand this an offset
/// list, which is exactly why it takes one.
fn footprint_preview_of(offsets: &[[i32; 2]], facing: Facing, anchor: IVec3) -> FootprintPreview {
    let mut preview = FootprintPreview::default();
    for offset in offsets {
        let next = preview.len;
        let Some(cell) = preview.cells.get_mut(next) else {
            break;
        };
        let [dx, dz] = rotate_offset(*offset, facing);
        *cell = IVec3::new(anchor.x + dx, anchor.y, anchor.z + dz);
        preview.len = next + 1;
    }
    preview
}

/// Turns a canonical (North-facing) footprint offset into the one this facing needs.
///
/// A quarter turn about the vertical axis per member, expressed in **integers**: a
/// rotation matrix in float would put a footprint cell on the wrong side of a boundary it
/// is sitting exactly on, and the server derives the same cells the same way.
fn rotate_offset(offset: [i32; 2], facing: Facing) -> [i32; 2] {
    let [dx, dz] = offset;
    match facing {
        Facing::North => [dx, dz],
        Facing::East => [-dz, dx],
        Facing::South => [-dx, -dz],
        Facing::West => [dz, -dx],
    }
}

/// The yaw a facing is drawn at, in radians about the world's up axis.
///
/// Yaw 0 looks along -Z and North *is* -Z, so North is 0. The rest follow the compass the
/// movement basis already uses: turning right lowers the yaw, so East — one quarter turn
/// to the right of North — is -π/2.
fn facing_yaw(facing: Facing) -> f32 {
    match facing {
        Facing::North => 0.0,
        Facing::East => -FRAC_PI_2,
        Facing::South => PI,
        Facing::West => FRAC_PI_2,
    }
}

/// The nearest of the four facings to a camera yaw.
///
/// **A yaw exactly on a 45° boundary takes the facing on the clockwise side** — the one
/// reached by turning right, which is the direction a decreasing yaw goes. Either answer
/// is defensible on a boundary; having one written down is what makes it testable, and
/// the alternative is a rule that changes with the sign of a rounding.
///
/// Total over any yaw the client could hold: a non-finite one has no nearest facing, and
/// North is answered rather than an `Unknown` the server would refuse. The look state is
/// kept finite and wrapped by `sample_input`, so this is a guard rather than a case.
pub(super) fn quantize_facing(yaw: f32) -> Facing {
    if !yaw.is_finite() {
        return Facing::North;
    }
    // Negated because the compass runs clockwise in yaw while the members run
    // anticlockwise; `+ 0.5` then `floor` is what puts a tie on the clockwise side,
    // where `round` would put it on whichever side the sign happened to choose.
    let quarter = (-yaw / FRAC_PI_2 + 0.5).floor().rem_euclid(4.0);
    match quarter as u8 {
        0 => Facing::North,
        1 => Facing::East,
        2 => Facing::South,
        _ => Facing::West,
    }
}

/// The axis-aligned box a structure occupies, in world units.
///
/// The footprint cells give the horizontal extent and the headroom gives the vertical
/// one, measured from the block *above* the anchor — the anchor itself is the ground the
/// thing stands on, not part of it. One block coordinate `c` spans `c..c+1`.
///
/// Widened to `i64` before the arithmetic, because the anchor is a number an untrusted
/// server chose and `i32::MAX + 1` is an overflow.
fn bounds(kind: StructureKind, facing: Facing, anchor: IVec3) -> (Vec3, Vec3) {
    let (anchor_x, anchor_y, anchor_z) = (
        i64::from(anchor.x),
        i64::from(anchor.y),
        i64::from(anchor.z),
    );

    let mut min = [i64::MAX, i64::MAX];
    let mut max = [i64::MIN, i64::MIN];
    for offset in footprint_offsets(kind) {
        let [dx, dz] = rotate_offset(*offset, facing);
        let cell = [anchor_x + i64::from(dx), anchor_z + i64::from(dz)];
        for axis in 0..2 {
            min[axis] = min[axis].min(cell[axis]);
            max[axis] = max[axis].max(cell[axis]);
        }
    }

    let base = anchor_y + 1;
    (
        Vec3::new(min[0] as f32, base as f32, min[1] as f32),
        Vec3::new(
            (max[0] + 1) as f32,
            (base + i64::from(headroom(kind))) as f32,
            (max[1] + 1) as f32,
        ),
    )
}

/// Where along a ray it enters an axis-aligned box, or `None` if it never does.
///
/// The slab method, and the *same* routine the block under the crosshair is measured with
/// — which is what makes "the nearest hit wins" a comparison of two numbers that mean the
/// same thing rather than of two approximations that nearly do.
///
/// A ray starting inside the box enters it at zero, which is the honest answer and the
/// nearest possible one. `direction` need not be normalised; it is normalised here, so the
/// result is a distance in blocks.
fn ray_box_entry(origin: Vec3, direction: Vec3, min: Vec3, max: Vec3) -> Option<f32> {
    if !origin.is_finite() || !direction.is_finite() {
        return None;
    }
    let length = direction.length();
    if length <= 0.0 {
        return None;
    }
    let direction = direction / length;

    let mut enter = 0.0f32;
    let mut exit = f32::INFINITY;
    for axis in 0..3 {
        let component = direction[axis];
        if component == 0.0 {
            // Parallel to this pair of planes: the ray is either inside the slab for its
            // whole length or never inside it at all. Dividing would give an infinity
            // whose sign depends on a zero's, which is not an answer.
            if origin[axis] < min[axis] || origin[axis] > max[axis] {
                return None;
            }
            continue;
        }
        let first = (min[axis] - origin[axis]) / component;
        let second = (max[axis] - origin[axis]) / component;
        enter = enter.max(first.min(second));
        exit = exit.min(first.max(second));
    }

    (enter <= exit).then_some(enter)
}

// ---------------------------------------------------------------------------
// What the session is looking at
// ---------------------------------------------------------------------------

/// One of this player's own structures under the crosshair.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct StructurePick {
    pub(super) structure_id: u64,
    /// How far along the aiming ray its bounds are entered, in blocks.
    pub(super) distance: f32,
}

/// The structure the mine/attack press would take back this frame, if any.
///
/// **Only ever one this session owns.** Someone else's is not a candidate at all, so
/// "another player's camp never captures the press" is a property of what can be in here
/// rather than a check every consumer has to remember to make. Removal is still refused by
/// the server and not by this resource.
///
/// Written through [`set_if_changed`] for the reason [`BlockTarget`] is: this is
/// recomputed every frame, and `ResMut` marks a resource changed on every `DerefMut`.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq)]
pub(super) struct StructureTarget(pub(super) Option<StructurePick>);

/// Whether the selected slot holds a structure this client will plant, and which.
///
/// The one predicate, read by the placement sender below and by the block-edit path in
/// [`super::target`] — which is what makes the two mutually exclusive rather than merely
/// unlikely to overlap. One press must never ask for both a voxel and a building.
pub(super) fn structure_in_hand(item_id: Option<u16>) -> Option<StructureKind> {
    match item_id? {
        ITEM_TENT => Some(StructureKind::Tent),
        ITEM_FORGE => Some(StructureKind::Forge),
        ITEM_CAMPFIRE => Some(StructureKind::Campfire),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// The plugin
// ---------------------------------------------------------------------------

/// Aims at this session's own structures and sends the two requests.
///
/// The spawn/despawn half is registered by `PlayerPlugin` instead, exactly as the mob and
/// drop renderers' are: it has to run *inside* the chain that begins with
/// `ingest_snapshots`, because the buffer it reads is filled there.
pub(super) struct StructuresPlugin;

/// Orders anything that needs this frame's structure pick after the pick was made.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct AimStructures;

/// Orders anything that reads this frame's footprint after the footprint was computed.
///
/// The single-voxel outline in [`super::target`] is what needs it: one press cannot mean
/// both a voxel and a building, so exactly one of the two outlines is drawn, and a
/// highlight that read last frame's preview would show both for a frame every time a
/// structure is selected.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct PreviewFootprint;

impl Plugin for StructuresPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<StructureTarget>()
            .init_resource::<FootprintPreview>()
            // `PlayerCameraPlugin` owns it in the game; here too, so `InputGate` resolves
            // when this module is built on its own.
            .init_resource::<ViewMode>()
            .add_systems(Startup, spawn_footprint_ghost)
            .add_systems(
                Update,
                (
                    preview_footprint
                        .in_set(PreviewFootprint)
                        // After the raycast, because the anchor is the voxel it produced,
                        // and after the camera, because the facing is quantized from the
                        // yaw this frame's look produced. Before the highlight, which
                        // hides itself on the answer.
                        .after(super::target::AimBlocks)
                        .after(AimCamera)
                        .after(super::inventory::ApplyInventory)
                        .before(DrawTargetHighlight),
                    move_the_footprint_ghost.after(PreviewFootprint),
                ),
            )
            .add_systems(
                Update,
                (
                    aim_at_structures
                        .in_set(AimStructures)
                        // After the voxel raycast, because the pick compares against the
                        // block it would otherwise be standing in front of, and before the
                        // block requests, because that is the frame's press being routed.
                        .after(super::target::AimBlocks)
                        .before(ApplyTargetInput)
                        .after(AimCamera),
                    (send_placement, send_removal)
                        .after(AimStructures)
                        // After the snapshots for the reason every other input system is: the
                        // gate they read is published there, and a frame stale is a click
                        // landing after the server said the player was dead.
                        .after(ApplySnapshots)
                        // After the tick-paced input, so the aim frame carrying this tick
                        // reaches the server before the request that names it.
                        .after(super::send_player_input),
                ),
            );
    }
}

// ---------------------------------------------------------------------------
// Aiming
// ---------------------------------------------------------------------------

/// Recomputes which of this player's own structures the crosshair is on.
///
/// Every input is optional for the reason the voxel raycast's are: this module has to work
/// in an app with no session and no camera, which is every frame before the handshake and
/// most of this module's own tests.
fn aim_at_structures(
    gate: InputGate<'_>,
    cameras: Query<&Transform, With<WorldCamera>>,
    standing: Query<&Structure>,
    block: Res<BlockTarget>,
    mut target: ResMut<StructureTarget>,
) {
    let picked = match (gate.may_aim(), cameras.iter().next()) {
        (true, Some(eye)) => pick_structure(
            eye.translation,
            *eye.forward(),
            block.0,
            standing.iter().copied(),
        ),
        _ => None,
    };

    set_if_changed(&mut target, StructureTarget(picked));
}

/// The nearest own structure the ray enters within reach and in front of the block.
///
/// Two conditions, and they are deliberately different comparisons. **Reach** is inclusive
/// — a structure entered exactly at the limit is within it, which is what the voxel
/// traversal already promises for a block entered exactly at its own. **Nearer than the
/// block** is strict, because a tie means the ray entered both at the same point and the
/// voxel is the thing that was already there.
fn pick_structure(
    origin: Vec3,
    direction: Vec3,
    block: Option<super::target::BlockHit>,
    candidates: impl Iterator<Item = Structure>,
) -> Option<StructurePick> {
    let behind = block.and_then(|hit| {
        let corner = hit.block.as_vec3();
        ray_box_entry(origin, direction, corner, corner + Vec3::ONE)
    });

    let mut nearest: Option<StructurePick> = None;
    for structure in candidates {
        if !structure.own {
            continue;
        }
        let (min, max) = bounds(structure.kind, structure.facing, structure.anchor);
        let Some(distance) = ray_box_entry(origin, direction, min, max) else {
            continue;
        };
        if distance > MAX_REACH || behind.is_some_and(|block| distance >= block) {
            continue;
        }
        if nearest.is_none_or(|held| distance < held.distance) {
            nearest = Some(StructurePick {
                structure_id: structure.structure_id,
                distance,
            });
        }
    }
    nearest
}

// ---------------------------------------------------------------------------
// Showing where it would stand
// ---------------------------------------------------------------------------

/// Marks one cell of the footprint ghost, so a query finds it without also matching the
/// structures themselves.
#[derive(Component)]
struct FootprintGhost;

/// How far the ghost stands off the cell it marks, in blocks.
///
/// The same reason [`super::target`]'s outline has one: a frame exactly coplanar with the
/// terrain's own faces flickers along its length, because the depth test then decides per
/// pixel which of two equal depths wins.
const GHOST_BLEED: f32 = 0.006;

/// The colour of the ghost, as linear RGB.
///
/// **One colour, in every state.** Nothing here is tinted by whether the placement would
/// succeed, and that is not a simplification to be improved on later: colouring a cell red
/// requires knowing the rule, and knowing the rule on this side means either a second copy
/// of it that can disagree with the server's, or waiting to be told. The server tells —
/// as an `ActionRefused` that reaches the status line — which is the whole of legacy PR 205.
///
/// A cooler amber than the single-voxel highlight, so the two are distinguishable at a
/// glance without either reading as a verdict.
const GHOST_COLOUR: Color = Color::linear_rgb(0.85, 0.78, 0.45);

/// Spawns the cell outlines the ghost is drawn from, once.
///
/// [`MAX_FOOTPRINT_CELLS`] of them, hidden, reused every frame: that is the widest
/// footprint any kind covers — the tent's nine today — and spawning to fit each kind would
/// churn entities on every hotbar press.
fn spawn_footprint_ghost(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // One mesh and one material shared by all nine, because every cell is the same box in
    // the same colour and nothing tints them apart — see [`GHOST_COLOUR`].
    let mesh = meshes.add(cell_outline_mesh(GHOST_BLEED));
    let material = materials.add(StandardMaterial {
        base_color: GHOST_COLOUR,
        // Unlit for the reason the aiming outline is: this is interface, and interface
        // that faded on the shaded side of a hill would be least legible exactly where the
        // terrain is hardest to read.
        unlit: true,
        ..default()
    });

    for _ in 0..MAX_FOOTPRINT_CELLS {
        commands.spawn((
            FootprintGhost,
            Mesh3d(mesh.clone()),
            MeshMaterial3d(material.clone()),
            Transform::default(),
            Visibility::Hidden,
        ));
    }
}

/// Works out where the structure in hand would stand this frame.
///
/// **No legality check happens here, deliberately** — the same rule [`send_placement`]
/// follows. This answers "which cells", which is arithmetic both sides perform
/// identically; it never answers "may it", which is the server's and only the server's.
fn preview_footprint(
    held: combat::HeldItem<'_>,
    block: Res<BlockTarget>,
    look: Res<LookState>,
    mut preview: ResMut<FootprintPreview>,
) {
    let next = match (held.structure(), block.0) {
        // The targeted voxel itself, not the empty cell in front of its face, because the
        // anchor is the **ground** a structure rests on — the same cell
        // [`send_placement`] names, so the preview and the request cannot disagree.
        (Some(kind), Some(hit)) => footprint_preview(kind, quantize_facing(look.yaw), hit.block),
        _ => FootprintPreview::default(),
    };
    set_if_changed(&mut preview, next);
}

/// Moves the ghost onto this frame's cells, or hides it.
///
/// Guarded on the change flag for the reason the single-voxel outline is: writing a
/// `Transform` marks the component changed and transform propagation is downstream of
/// that, so a player standing still would otherwise repropagate nine outlines every frame
/// for the rest of the session.
fn move_the_footprint_ghost(
    preview: Res<FootprintPreview>,
    mut ghosts: Query<(&mut Transform, &mut Visibility), With<FootprintGhost>>,
) {
    if !preview.is_changed() {
        return;
    }

    let cells = preview.cells();
    for (index, (mut transform, mut visibility)) in ghosts.iter_mut().enumerate() {
        match cells.get(index) {
            Some(cell) => {
                // The cell's minimum corner. A voxel is one world unit, so a world block
                // coordinate is already a world position and needs no scaling.
                transform.translation = cell.as_vec3();
                *visibility = Visibility::Visible;
            }
            // Every cell past this footprint's own. A tent's nine and a campfire's one
            // share the same entities, so the ones this kind does not use are hidden
            // rather than left standing where the last kind put them.
            None => *visibility = Visibility::Hidden,
        }
    }
}

// ---------------------------------------------------------------------------
// Asking
// ---------------------------------------------------------------------------

/// Sends one `PlaceStructureRequest` per place press, while a structure is in hand.
///
/// **No legality check happens here, deliberately** — the same rule the block edit beside
/// it follows. Whether the ground under the footprint is solid, whether the space above it
/// is clear, whether the player is close enough by the server's own reckoning and whether
/// they already have a tent are every one of them the server's answer. Intent goes out and
/// the camp appears if a snapshot says so.
fn send_placement(
    buttons: Option<Res<ButtonInput<MouseButton>>>,
    gate: InputGate<'_>,
    held: combat::HeldItem<'_>,
    block: Res<BlockTarget>,
    look: Res<LookState>,
    cadence: Res<InputCadence>,
    outbound: Option<ResMut<Outbound>>,
) {
    if !gate.may_act() {
        return;
    }
    let Some(buttons) = buttons else {
        return;
    };
    if !buttons.just_pressed(PLACE_BUTTON) {
        return;
    }
    if held.structure().is_none() {
        return;
    }
    // The targeted voxel itself, not the empty cell in front of its face: the anchor is
    // the **ground** a structure rests on, and both sides derive the footprint from it.
    let Some(hit) = block.0 else {
        return;
    };
    let Some(mut outbound) = outbound else {
        return;
    };

    let request = PlaceStructureRequest {
        slot: held.slot(),
        anchor: BlockCoord {
            x: hit.block.x,
            y: hit.block.y,
            z: hit.block.z,
        },
        // Quantized here rather than sent as a float: the contract carries four members
        // precisely so a continuous angle is resolved once, on the side that has the
        // camera, instead of on both sides with a difference to reconcile.
        facing: quantize_facing(look.yaw),
        // The counter `PlayerInput`, mining and placement all share, so the server can
        // order this against the aim frame that carries the same number.
        client_tick: cadence.client_tick,
    };

    match outbound.send(encode_place_structure_request(&request)) {
        Sent::Queued => {}
        Sent::Dropped => warn!(
            "the outbound queue was full; a placement at {:?} never reached the server",
            hit.block
        ),
        Sent::Closed => {}
    }
}

/// Sends one `RemoveStructureRequest` per press on one of this player's own structures.
///
/// `just_pressed`, never `pressed`: taking a structure back is an event, and holding the
/// button would only fill the outbound queue with requests naming an id the first one
/// already removed.
fn send_removal(
    buttons: Option<Res<ButtonInput<MouseButton>>>,
    gate: InputGate<'_>,
    target: Res<StructureTarget>,
    cadence: Res<InputCadence>,
    outbound: Option<ResMut<Outbound>>,
) {
    if !gate.may_act() {
        return;
    }
    let Some(buttons) = buttons else {
        return;
    };
    if !buttons.just_pressed(REMOVE_BUTTON) {
        return;
    }
    let Some(picked) = target.0 else {
        return;
    };
    let Some(mut outbound) = outbound else {
        return;
    };

    let request = RemoveStructureRequest {
        structure_id: picked.structure_id,
        client_tick: cadence.client_tick,
    };
    match outbound.send(encode_remove_structure_request(&request)) {
        Sent::Queued => {}
        Sent::Dropped => warn!(
            "the outbound queue was full; a removal of structure {} never reached the server",
            picked.structure_id
        ),
        Sent::Closed => {}
    }
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// How thick the canvas of a tent is drawn, in blocks.
const CANVAS_THICKNESS: f32 = 0.08;

/// Half the tent's 3×3 footprint, and the two cells of headroom it fills.
const TENT_HALF_WIDTH: f32 = 1.5;
const TENT_HALF_DEPTH: f32 = 1.5;
const TENT_HEIGHT: f32 = 2.0;

/// The anvil, in blocks: a splayed base, a narrow waist, a working face and the horn that
/// sticks out along the facing.
const ANVIL_BASE: Vec3 = Vec3::new(0.70, 0.18, 0.50);
const ANVIL_WAIST: Vec3 = Vec3::new(0.26, 0.34, 0.26);
const ANVIL_FACE: Vec3 = Vec3::new(0.80, 0.16, 0.36);
const ANVIL_HORN_RADIUS: f32 = 0.09;
const ANVIL_HORN_LENGTH: f32 = 0.36;

/// The hearth beside it and the flame standing on it.
const HEARTH_SIZE: Vec3 = Vec3::new(0.86, 0.42, 0.86);
const FLAME_BASE_RADIUS: f32 = 0.22;
const FLAME_BASE_HEIGHT: f32 = 0.30;
const FLAME_TIP_RADIUS: f32 = 0.12;
const FLAME_TIP_HEIGHT: f32 = 0.36;

/// Weathered canvas, and the brighter bolt this session's own tent is cut from. Linear
/// RGB, like everything else this client colours by hand.
const CANVAS_OWN: Color = Color::linear_rgb(0.62, 0.55, 0.40);
const CANVAS_OTHER: Color = Color::linear_rgb(0.26, 0.24, 0.21);

/// The hearth's fieldstone, in the same two shades.
const HEARTH_OWN: Color = Color::linear_rgb(0.34, 0.31, 0.29);
const HEARTH_OTHER: Color = Color::linear_rgb(0.16, 0.15, 0.15);

/// Cold forged iron, and the ember the hearth is lit by.
const IRON: Color = Color::linear_rgb(0.09, 0.09, 0.10);
const EMBER: Color = Color::linear_rgb(0.95, 0.33, 0.06);

/// How hard the flame glows. Emissive, so a hearth and a campfire read as burning from
/// their own material rather than from the scene's lighting.
const EMBER_EMISSIVE: LinearRgba = LinearRgba::rgb(6.0, 1.8, 0.35);

/// The campfire, in blocks, all of it inside the one cell the server validated.
///
/// A ring of fieldstone, two logs crossed over it and the same two-cone flame the hearth
/// stands. The ring's radius plus half a stone is what has to stay under the half-cell,
/// and [`nothing_a_structure_draws_escapes_its_own_footprint`] is what checks it.
const RING_STONES: usize = 8;
const RING_RADIUS: f32 = 0.30;
const RING_STONE: Vec3 = Vec3::new(0.17, 0.13, 0.17);
const FIRE_LOG_LENGTH: f32 = 0.70;
const FIRE_LOG_THICKNESS: f32 = 0.12;
const FIRE_FLAME_BASE_RADIUS: f32 = 0.19;
const FIRE_FLAME_BASE_HEIGHT: f32 = 0.30;
const FIRE_FLAME_TIP_RADIUS: f32 = 0.10;
const FIRE_FLAME_TIP_HEIGHT: f32 = 0.30;

/// Charred wood: what is left of a log that has been burning a while.
const CHARRED_WOOD: Color = Color::linear_rgb(0.10, 0.07, 0.05);

/// The colour, the brightness and the reach of a campfire's light.
///
/// **The reach is a presentation choice and is documented as one.** The server decides
/// where a creature may spawn, at `game.CampfireSafeRadius` — sixteen blocks, checked in
/// `spawn.go` against the structures it is holding, whatever this light reaches. This
/// number is deliberately *not* sixteen so that nobody, player or reader, can take the
/// edge of the glow for the edge of the rule: the light says **here is the fire**, and the
/// ground it keeps is not something a client is entitled to draw a boundary around.
///
/// The brightness is tuned rather than measured, and it is worth being plain about that:
/// the gates here open no window, so nothing in this repository has looked at it. It is a
/// lumen figure against Bevy's default `Exposure::BLENDER`, sized so that at a few blocks
/// it is comparable to [`super::sky`]'s night sun and much brighter close in. Tune it
/// against the sky's night constants, which are the only other lighting numbers here.
const FIRE_LIGHT_COLOUR: Color = Color::linear_rgb(1.0, 0.52, 0.16);
const FIRE_LIGHT_LUMENS: f32 = 120_000.0;
const FIRE_LIGHT_RANGE: f32 = 12.0;

/// The server's safe radius, named here only so the assertion below can refuse to let the
/// light grow into it.
///
/// **It is not read by anything, and nothing here may start reading it.** `spawn.go` owns
/// the rule and checks it against the structures the server is holding; a client copy that
/// decided anything would be the cheat vector this whole module is written to avoid. What
/// it buys is that a later tweak to [`FIRE_LIGHT_RANGE`] cannot silently make the glow
/// look like the boundary.
const SERVER_CAMPFIRE_SAFE_RADIUS: f32 = 16.0;
const _: () = assert!(FIRE_LIGHT_RANGE < SERVER_CAMPFIRE_SAFE_RADIUS);

/// How far the flicker swings and how quickly, as a fraction of [`FIRE_LIGHT_LUMENS`].
///
/// Two sines whose periods do not divide one another, so the sum does not repeat on any
/// interval a player would notice. Cheap on purpose — the issue asks for a slow flicker
/// and rules a particle system out, and this is one multiply-add per fire per frame.
const FLICKER_DEPTH: f32 = 0.12;
const FLICKER_HZ: [f32; 2] = [1.7, 4.3];

/// One live structure, keyed by the identity the server minted for it.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Structure {
    structure_id: u64,
    kind: StructureKind,
    anchor: IVec3,
    facing: Facing,
    owner_entity_id: u64,
    /// Whether `ServerWelcome` named this session's own entity as the owner. Derived once
    /// where the identity is known, so nothing downstream has to carry the session around
    /// to ask.
    ///
    /// From protocol V5 `owner_entity_id` may be `0`, which says the owner has no live
    /// session rather than that the structure is unowned — so an offline owner is simply
    /// not this session and `own` is false. The comparison needs no special case for it
    /// because `net/codec.rs` refuses a `ServerWelcome` whose own `entity_id` is 0; that
    /// check is what stops 0 from matching 0 here and handing every offline owner's camp
    /// to this player.
    own: bool,
}

impl Structure {
    fn of(state: &StructureState, local_entity_id: u64) -> Self {
        Self {
            structure_id: state.structure_id,
            kind: state.kind,
            anchor: IVec3::new(state.anchor.x, state.anchor.y, state.anchor.z),
            facing: state.facing,
            owner_entity_id: state.owner_entity_id,
            own: state.owner_entity_id == local_entity_id,
        }
    }

    /// Where the structure is drawn: the middle of the anchor cell horizontally, and the
    /// plane one block above it vertically, because the anchor is the ground it rests on.
    fn placement(&self) -> Transform {
        Transform::from_translation(Vec3::new(
            self.anchor.x as f32 + 0.5,
            self.anchor.y as f32 + 1.0,
            self.anchor.z as f32 + 0.5,
        ))
        .with_rotation(Quat::from_rotation_y(facing_yaw(self.facing)))
    }
}

/// Marks the meshes a structure is drawn from, so a query finds them without also
/// matching the bodies, the drops and the target outline.
///
/// A marker with no owner field, unlike a mob's: nothing here recolours a part after it is
/// spawned. A structure that changes is despawned and drawn again, because its kind,
/// facing and ownership each choose a different mesh or material.
#[derive(Component, Debug)]
pub(super) struct StructureVisual;

/// The shared meshes and materials every structure is drawn from.
///
/// Primitives and hand-written colours, no assets: this issue adds no model, texture or
/// animation file. Each mesh is authored in the canonical North orientation with its
/// origin on the anchor's *top* face, so the parent transform is the whole of where and
/// which way a structure stands.
#[derive(Resource, Debug)]
pub(super) struct StructureVisuals {
    tent: Handle<Mesh>,
    anvil: Handle<Mesh>,
    hearth: Handle<Mesh>,
    flame: Handle<Mesh>,
    fire_ring: Handle<Mesh>,
    fire_logs: Handle<Mesh>,
    fire_flame: Handle<Mesh>,
    /// Indexed by `usize::from(own)`, so the two shades cannot drift apart in a `match`.
    canvas: [Handle<StandardMaterial>; 2],
    stone: [Handle<StandardMaterial>; 2],
    iron: Handle<StandardMaterial>,
    ember: Handle<StandardMaterial>,
    charred: Handle<StandardMaterial>,
}

pub(super) fn create_visuals(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let canvas = |colour: Color| {
        StandardMaterial {
            base_color: colour,
            perceptual_roughness: 0.95,
            // The tent's gable is one triangle and its front is deliberately open, so the
            // inside of the shelter is a surface a player looks at. Culled canvas would
            // make the far wall disappear from inside the thing they sleep in.
            double_sided: true,
            cull_mode: None,
            ..default()
        }
    };
    let rock = |colour: Color| StandardMaterial {
        base_color: colour,
        perceptual_roughness: 0.9,
        ..default()
    };

    commands.insert_resource(StructureVisuals {
        tent: meshes.add(tent_mesh()),
        anvil: meshes.add(anvil_mesh()),
        // One cell along the facing, which is -Z in the canonical orientation, and
        // standing on the same base plane the anvil does.
        hearth: meshes.add(
            Mesh::from(Cuboid::from_size(HEARTH_SIZE)).translated_by(Vec3::new(
                0.0,
                HEARTH_SIZE.y / 2.0,
                -1.0,
            )),
        ),
        flame: meshes.add(flame_mesh()),
        fire_ring: meshes.add(fire_ring_mesh()),
        fire_logs: meshes.add(fire_logs_mesh()),
        fire_flame: meshes.add(fire_flame_mesh()),
        canvas: [
            materials.add(canvas(CANVAS_OTHER)),
            materials.add(canvas(CANVAS_OWN)),
        ],
        stone: [
            materials.add(rock(HEARTH_OTHER)),
            materials.add(rock(HEARTH_OWN)),
        ],
        iron: materials.add(StandardMaterial {
            base_color: IRON,
            perceptual_roughness: 0.45,
            metallic: 0.8,
            ..default()
        }),
        ember: materials.add(StandardMaterial {
            base_color: EMBER,
            emissive: EMBER_EMISSIVE,
            // Unlit would be flat; an emissive term on a rough surface reads as something
            // burning without the *material* having to be a light source.
            perceptual_roughness: 1.0,
            ..default()
        }),
        charred: materials.add(rock(CHARRED_WOOD)),
    });
}

/// The tent: two canvas slopes leaning together over the 3×3 footprint, a closed gable at
/// the back, and the front left open so the opening faces the way the structure does.
fn tent_mesh() -> Mesh {
    // Inset by half the canvas thickness, because a *leaning* box is wider than the line
    // it leans along: at this pitch an un-inset panel's corner would land some three
    // centimetres over the 3×3 the server validated, which is ground this tent does not
    // stand on. What the inset leaves over is the bottom edge bedding into the ground it
    // is pitched on, and that is the right direction for it to be wrong in — a tent
    // hovering above the grass would read as a bug where one touching it does not.
    let half_width = TENT_HALF_WIDTH - CANVAS_THICKNESS / 2.0;
    let height = TENT_HEIGHT - CANVAS_THICKNESS / 2.0;

    // The pitch of one slope, and how long it is from eaves to ridge. Both derived, so
    // changing the footprint or the headroom moves the canvas with them.
    let lean = height.atan2(half_width);
    let slope = half_width.hypot(height);
    let panel = Cuboid::new(slope, CANVAS_THICKNESS, 2.0 * TENT_HALF_DEPTH);

    let mut canvas = Mesh::from(panel)
        .rotated_by(Quat::from_rotation_z(lean))
        .translated_by(Vec3::new(-half_width / 2.0, height / 2.0, 0.0));

    let right = Mesh::from(panel)
        .rotated_by(Quat::from_rotation_z(PI - lean))
        .translated_by(Vec3::new(half_width / 2.0, height / 2.0, 0.0));

    // Wound so the face looks away from the opening. `Triangle3d`'s normal is
    // `(b - a) × (c - a)`, and getting it backwards is invisible until something renders
    // inside out.
    let gable = Mesh::from(Triangle3d::new(
        Vec3::new(-half_width, 0.0, TENT_HALF_DEPTH),
        Vec3::new(half_width, 0.0, TENT_HALF_DEPTH),
        Vec3::new(0.0, height, TENT_HALF_DEPTH),
    ));

    merge_all(&mut canvas, [right, gable], "tent");
    canvas
}

/// The anvil: base, waist, working face, and a horn along the facing.
fn anvil_mesh() -> Mesh {
    let base_top = ANVIL_BASE.y;
    let face_bottom = base_top + ANVIL_WAIST.y;
    let face_middle = face_bottom + ANVIL_FACE.y / 2.0;

    let mut anvil =
        Mesh::from(Cuboid::from_size(ANVIL_BASE)).translated_by(Vec3::Y * (ANVIL_BASE.y / 2.0));

    let waist = Mesh::from(Cuboid::from_size(ANVIL_WAIST))
        .translated_by(Vec3::Y * (base_top + ANVIL_WAIST.y / 2.0));
    let face = Mesh::from(Cuboid::from_size(ANVIL_FACE)).translated_by(Vec3::Y * face_middle);
    // Bevy's cone points along +Y about its middle, so a quarter turn about -X lays it
    // down along -Z — which is North, and therefore the way this structure faces.
    let horn = Mesh::from(Cone::new(ANVIL_HORN_RADIUS, ANVIL_HORN_LENGTH))
        .rotated_by(Quat::from_rotation_x(-FRAC_PI_2))
        .translated_by(Vec3::new(
            0.0,
            face_middle,
            -(ANVIL_FACE.z + ANVIL_HORN_LENGTH) / 2.0,
        ));

    merge_all(&mut anvil, [waist, face, horn], "anvil");
    anvil
}

/// The flame: two stacked cones, and nothing that moves.
///
/// Static on purpose. A flicker is a particle system or an animated material, and both are
/// out of scope for this issue; what makes it read as fire is the emissive term rather
/// than the motion.
fn flame_mesh() -> Mesh {
    let hearth_top = HEARTH_SIZE.y;
    let mut flame = Mesh::from(Cone::new(FLAME_BASE_RADIUS, FLAME_BASE_HEIGHT))
        .translated_by(Vec3::new(0.0, hearth_top + FLAME_BASE_HEIGHT / 2.0, -1.0));

    let tip = Mesh::from(Cone::new(FLAME_TIP_RADIUS, FLAME_TIP_HEIGHT)).translated_by(Vec3::new(
        0.0,
        hearth_top + FLAME_BASE_HEIGHT * 0.6 + FLAME_TIP_HEIGHT / 2.0,
        -1.0,
    ));

    merge_all(&mut flame, [tip], "flame");
    flame
}

/// The ring of fieldstone a campfire is built inside, laid around the anchor cell.
///
/// [`RING_STONES`] boxes on a circle, each turned to face the middle so the ring reads as
/// a ring rather than as eight cubes in a rough circle. The radius plus half a stone's
/// diagonal is what has to stay inside the half-cell the server validated.
fn fire_ring_mesh() -> Mesh {
    let stone = Cuboid::from_size(RING_STONE);
    let at = |index: usize| {
        let angle = TAU * index as f32 / RING_STONES as f32;
        Mesh::from(stone)
            .rotated_by(Quat::from_rotation_y(angle))
            .translated_by(Vec3::new(
                RING_RADIUS * angle.sin(),
                RING_STONE.y / 2.0,
                RING_RADIUS * angle.cos(),
            ))
    };

    let mut ring = at(0);
    merge_all(&mut ring, (1..RING_STONES).map(at), "campfire ring");
    ring
}

/// Two logs crossed over the ring, resting on the stones.
fn fire_logs_mesh() -> Mesh {
    let log = Cuboid::new(FIRE_LOG_LENGTH, FIRE_LOG_THICKNESS, FIRE_LOG_THICKNESS);
    let height = RING_STONE.y + FIRE_LOG_THICKNESS / 2.0;

    let mut logs = Mesh::from(log)
        .rotated_by(Quat::from_rotation_y(FRAC_PI_4))
        .translated_by(Vec3::Y * height);
    let across = Mesh::from(log)
        .rotated_by(Quat::from_rotation_y(-FRAC_PI_4))
        .translated_by(Vec3::Y * (height + FIRE_LOG_THICKNESS));

    merge_all(&mut logs, [across], "campfire logs");
    logs
}

/// The flame standing on the logs: the hearth's two stacked cones, smaller.
///
/// Static geometry, exactly as the hearth's is. What moves is the light beside it, and it
/// moves in brightness rather than in shape — see [`animate`].
fn fire_flame_mesh() -> Mesh {
    let base = RING_STONE.y + FIRE_LOG_THICKNESS * 2.0;
    let mut flame = Mesh::from(Cone::new(FIRE_FLAME_BASE_RADIUS, FIRE_FLAME_BASE_HEIGHT))
        .translated_by(Vec3::Y * (base + FIRE_FLAME_BASE_HEIGHT / 2.0));

    let tip = Mesh::from(Cone::new(FIRE_FLAME_TIP_RADIUS, FIRE_FLAME_TIP_HEIGHT)).translated_by(
        Vec3::Y * (base + FIRE_FLAME_BASE_HEIGHT * 0.6 + FIRE_FLAME_TIP_HEIGHT / 2.0),
    );

    merge_all(&mut flame, [tip], "campfire flame");
    flame
}

/// Where a campfire's light sits: in the flame, a little above the logs.
fn fire_light_height() -> f32 {
    RING_STONE.y + FIRE_LOG_THICKNESS * 2.0 + FIRE_FLAME_BASE_HEIGHT / 2.0
}

/// Spawns, places and despawns structures from the latest authoritative snapshot.
///
/// The newest snapshot is the existence set, exactly as it is for mobs and drops. A
/// structure whose description *changed* is despawned and drawn again rather than edited
/// in place: kind, facing and ownership each choose a different mesh or material, and
/// rebuilding a handful of children is cheaper than a patch path that has to stay
/// exhaustive as kinds are added.
pub(super) fn apply_snapshots(
    buffer: Res<SnapshotBuffer>,
    session: Option<Res<Session>>,
    mode: Res<InputMode>,
    visuals: Option<Res<StructureVisuals>>,
    mut existing: Query<(Entity, &Structure, &mut Visibility)>,
    mut commands: Commands,
) {
    let (Some(session), Some(visuals)) = (session, visuals) else {
        return;
    };

    let standing: Vec<Structure> = buffer
        .structures()
        .iter()
        .map(|state| Structure::of(state, session.0.entity_id))
        .collect();
    let visibility = structure_visibility(*mode);

    let mut drawn = Vec::with_capacity(standing.len());
    for (entity, structure, mut current_visibility) in &mut existing {
        if !standing.contains(structure) {
            // Gone from the newest answer, or no longer the thing that was drawn. Why is
            // not asked either way.
            commands.entity(entity).despawn();
            continue;
        }
        if *current_visibility != visibility {
            *current_visibility = visibility;
        }
        drawn.push(*structure);
    }

    for structure in standing {
        if drawn.contains(&structure) {
            continue;
        }
        spawn_structure(&mut commands, &visuals, structure, visibility);
        drawn.push(structure);
    }
}

fn spawn_structure(
    commands: &mut Commands,
    visuals: &StructureVisuals,
    structure: Structure,
    visibility: Visibility,
) {
    let own = usize::from(structure.own);
    let standing = commands
        .spawn((structure, structure.placement(), visibility))
        .id();

    commands.entity(standing).with_children(|parent| {
        let mut part = |mesh: Handle<Mesh>, material: Handle<StandardMaterial>| {
            parent.spawn((
                StructureVisual,
                Mesh3d(mesh),
                MeshMaterial3d(material),
                Transform::default(),
            ));
        };

        // One match on the kind, and whether it carries a light comes out of the same
        // arms rather than out of a second test that could drift from the first.
        let lit = match structure.kind {
            StructureKind::Tent => {
                part(visuals.tent.clone(), visuals.canvas[own].clone());
                false
            }
            StructureKind::Forge => {
                part(visuals.anvil.clone(), visuals.iron.clone());
                part(visuals.hearth.clone(), visuals.stone[own].clone());
                part(visuals.flame.clone(), visuals.ember.clone());
                false
            }
            StructureKind::Campfire => {
                part(visuals.fire_ring.clone(), visuals.stone[own].clone());
                part(visuals.fire_logs.clone(), visuals.charred.clone());
                part(visuals.fire_flame.clone(), visuals.ember.clone());
                true
            }
        };

        if lit {
            // A child rather than a component on the parent, so it sits in the flame
            // instead of on the ground — and so it inherits the parent's `Visibility`,
            // which is what hides it with the rest of the world when a panel owns the
            // screen. `PointLight` requires `Transform` and `Visibility`, and the default
            // `Visibility::Inherited` is exactly the behaviour wanted here.
            parent.spawn((
                FireLight {
                    // A phase per fire, derived from the identity the server minted, so
                    // two fires in one camp do not pulse in lockstep. Deterministic and
                    // free: no clock, no randomness, nothing to keep in sync.
                    phase: (structure.structure_id % 1_000) as f32 * 0.017,
                },
                PointLight {
                    color: FIRE_LIGHT_COLOUR,
                    intensity: FIRE_LIGHT_LUMENS,
                    range: FIRE_LIGHT_RANGE,
                    // Off, like every other light in this client: there is no shadow map
                    // anywhere here, and one fire is no place to start paying for one.
                    shadow_maps_enabled: false,
                    ..default()
                },
                Transform::from_translation(Vec3::Y * fire_light_height()),
            ));
        }
    });
}

/// Marks a campfire's light, and carries the phase its flicker is offset by.
#[derive(Component, Debug)]
pub(super) struct FireLight {
    phase: f32,
}

/// Flickers every campfire's light on local time.
///
/// **The only thing in this module that moves, and it decides nothing.** Brightness is not
/// a gameplay fact: the ground a fire keeps clear is `game.CampfireSafeRadius` on the
/// server and is checked there, so a light that dimmed to nothing would still leave that
/// ground exactly as safe. Nothing reads this value back out.
///
/// Written unconditionally, unlike the resources this module sets through
/// [`set_if_changed`]: the value genuinely differs every frame — that is what a flicker is
/// — so a guard would only cost a comparison, exactly as it would in [`super::mobs`]'s
/// pose easing.
pub(super) fn animate(time: Res<Time>, mut fires: Query<(&FireLight, &mut PointLight)>) {
    let seconds = time.elapsed_secs();

    for (fire, mut light) in &mut fires {
        // Two sines whose periods do not divide one another, averaged, so the sum never
        // settles into a visible beat. Bounded by construction: the swing is at most
        // FLICKER_DEPTH of the base, so no frame can be dark and none can be a flash.
        let swing: f32 = FLICKER_HZ
            .iter()
            .map(|hz| (TAU * hz * (seconds + fire.phase)).sin())
            .sum::<f32>()
            / FLICKER_HZ.len() as f32;
        light.intensity = FIRE_LIGHT_LUMENS * (1.0 + FLICKER_DEPTH * swing);
    }
}

fn structure_visibility(mode: InputMode) -> Visibility {
    if HIDDEN_INPUT_MODES.contains(&mode) {
        Visibility::Hidden
    } else {
        Visibility::Visible
    }
}

#[cfg(test)]
mod tests {
    //! No window, no display and no GPU, the same rule the rest of the client's tests
    //! follow. The footprint arithmetic, the compass and the ray-versus-box pick are plain
    //! functions and are asserted as such; the rest is driven on `MinimalPlugins` +
    //! `AssetPlugin`, where `Assets<T>` is an ordinary resource and everything short of
    //! the GPU upload exists.
    //!
    //! What a request test asserts is the **bytes that left**, because the frame is what
    //! the server acts on.

    use std::f32::consts::FRAC_PI_4;
    use std::sync::mpsc::Receiver;
    use std::time::{Duration, Instant};

    use bevy::asset::AssetPlugin;
    use bevy::input::mouse::MouseButtonInput;
    use bevy::input::{ButtonState, InputPlugin};
    use bevy::time::TimeUpdateStrategy;

    use super::*;
    use crate::net::{
        ChunkCoord, EntityState, InventoryInbox, InventoryStack, InventoryState, SessionParams,
        Snapshot, SnapshotInbox,
    };
    use crate::player::PlayerPlugin;
    use crate::player::constants::MAX_PITCH;
    use crate::player::crafting::ITEM_IRON_SWORD;
    use crate::player::target::BlockHit;
    use crate::wire::voxelheim::net as fb;
    use crate::world::{ChunkStore, VoxelChunk, palette};

    /// The chunk edge the server sends, and the one every world coordinate below is
    /// written for.
    const SIZE: u16 = 32;

    /// This session's own entity, as `ServerWelcome` names it.
    const LOCAL_ID: u64 = 7;

    /// Somebody else's.
    const OTHER_ID: u64 = 99;

    /// Where the server puts the player in these tests. The camera therefore sits at
    /// `80 + EYE_HEIGHT`, which is inside the voxel at world y 81.
    const SPAWN: [f32; 3] = [0.5, 80.0, 0.5];

    // ---------------------------------------------------------------------------
    // The footprint, and the compass it is rotated by
    // ---------------------------------------------------------------------------

    /// The rotation `server/internal/game/structure.go` performs, case for case. A
    /// footprint the two sides disagree about is a tent that visibly does not cover the
    /// ground the server says it covers.
    #[test]
    fn the_footprint_rotation_mirrors_the_servers() {
        let offset = [2, -1];
        assert_eq!(rotate_offset(offset, Facing::North), [2, -1]);
        assert_eq!(rotate_offset(offset, Facing::East), [1, 2]);
        assert_eq!(rotate_offset(offset, Facing::South), [-2, 1]);
        assert_eq!(rotate_offset(offset, Facing::West), [-1, -2]);

        // Four quarter turns are the identity, on every offset of every footprint. A
        // transposed sign would still pass a single case.
        for offsets in [
            &TENT_FOOTPRINT[..],
            &FORGE_FOOTPRINT[..],
            &CAMPFIRE_FOOTPRINT[..],
        ] {
            for offset in offsets {
                let turned = [Facing::East, Facing::South, Facing::West]
                    .iter()
                    .fold(rotate_offset(*offset, Facing::East), |turned, _| {
                        rotate_offset(turned, Facing::East)
                    });
                assert_eq!(turned, *offset, "four turns did not return {offset:?}");
            }
        }
    }

    /// The tent rests on 3×3 ground cells with two of air above each; the forge on the
    /// anchor and the cell one step along the facing, with one. **The anchor is the
    /// ground**, so the box starts a block above it.
    #[test]
    fn the_bounds_are_the_footprint_above_the_anchor() {
        let anchor = IVec3::new(4, 63, -7);

        // A tent is symmetric, so its box is the same whichever way it faces — and it is
        // still computed by the rotation rather than special-cased.
        for facing in [Facing::North, Facing::East, Facing::South, Facing::West] {
            assert_eq!(
                bounds(StructureKind::Tent, facing, anchor),
                (Vec3::new(3.0, 64.0, -8.0), Vec3::new(6.0, 66.0, -5.0)),
                "a tent facing {facing:?}"
            );
        }

        // A forge is not: the hearth is one cell along the facing, and North is -Z.
        for (facing, min, max) in [
            (
                Facing::North,
                Vec3::new(4.0, 64.0, -8.0),
                Vec3::new(5.0, 65.0, -6.0),
            ),
            (
                Facing::East,
                Vec3::new(4.0, 64.0, -7.0),
                Vec3::new(6.0, 65.0, -6.0),
            ),
            (
                Facing::South,
                Vec3::new(4.0, 64.0, -7.0),
                Vec3::new(5.0, 65.0, -5.0),
            ),
            (
                Facing::West,
                Vec3::new(3.0, 64.0, -7.0),
                Vec3::new(5.0, 65.0, -6.0),
            ),
        ] {
            assert_eq!(
                bounds(StructureKind::Forge, facing, anchor),
                (min, max),
                "a forge facing {facing:?}"
            );
        }

        // A campfire is one cell with one of air over it, whichever way it faces —
        // `campfireFootprint` is `{{0, 0}}` and rotating a single symmetric offset is the
        // identity. It is drawn facing the way it was placed all the same, because the
        // wire carries a facing and both sides derive the cells the same way.
        for facing in [Facing::North, Facing::East, Facing::South, Facing::West] {
            assert_eq!(
                bounds(StructureKind::Campfire, facing, anchor),
                (Vec3::new(4.0, 64.0, -7.0), Vec3::new(5.0, 65.0, -6.0)),
                "a campfire facing {facing:?}"
            );
        }
    }

    /// The compass is the movement basis: yaw 0 looks along -Z, and North *is* -Z.
    #[test]
    fn each_facing_points_where_the_movement_basis_says() {
        for (facing, direction) in [
            (Facing::North, Vec3::NEG_Z),
            (Facing::East, Vec3::X),
            (Facing::South, Vec3::Z),
            (Facing::West, Vec3::NEG_X),
        ] {
            let turned = Quat::from_rotation_y(facing_yaw(facing)) * Vec3::NEG_Z;
            assert!(
                turned.abs_diff_eq(direction, 1e-5),
                "{facing:?} faces {turned:?}, want {direction:?}"
            );

            // And the same rotation moves the canonical hearth offset onto the cell the
            // integer rotation names, which is what keeps the drawing and the footprint
            // describing one structure.
            let [dx, dz] = rotate_offset(FORGE_FOOTPRINT[1], facing);
            assert!(
                Vec3::new(dx as f32, 0.0, dz as f32).abs_diff_eq(direction, 1e-5),
                "the hearth of a forge facing {facing:?} is not along it"
            );
        }
    }

    #[test]
    fn a_camera_yaw_quantizes_to_the_nearest_facing() {
        // The four exact members first: a facing must survive a round trip through its
        // own yaw, or a structure would turn as soon as it was placed.
        for facing in [Facing::North, Facing::East, Facing::South, Facing::West] {
            assert_eq!(quantize_facing(facing_yaw(facing)), facing);
        }

        // Just inside each 45° boundary, on both sides of it.
        let nudge = 0.01;
        for (yaw, want) in [
            (FRAC_PI_4 - nudge, Facing::North),
            (FRAC_PI_4 + nudge, Facing::West),
            (-FRAC_PI_4 + nudge, Facing::North),
            (-FRAC_PI_4 - nudge, Facing::East),
            (3.0 * FRAC_PI_4 - nudge, Facing::West),
            (3.0 * FRAC_PI_4 + nudge, Facing::South),
            (-3.0 * FRAC_PI_4 + nudge, Facing::East),
            (-3.0 * FRAC_PI_4 - nudge, Facing::South),
        ] {
            assert_eq!(quantize_facing(yaw), want, "a yaw of {yaw}");
        }
    }

    /// A yaw exactly on a boundary is a tie, and the rule is written down rather than
    /// left to the sign of a rounding: it takes the facing reached by turning **right**,
    /// which is the direction a decreasing yaw goes.
    #[test]
    fn a_yaw_on_a_boundary_takes_the_clockwise_facing() {
        for (yaw, want) in [
            (FRAC_PI_4, Facing::North),
            (-FRAC_PI_4, Facing::East),
            (3.0 * FRAC_PI_4, Facing::West),
            (-3.0 * FRAC_PI_4, Facing::South),
        ] {
            assert_eq!(quantize_facing(yaw), want, "the boundary at {yaw}");
        }

        // And a yaw that is not a yaw still answers a facing the server will accept,
        // rather than an `Unknown` it would refuse. Unreachable through `sample_input`,
        // which keeps the look state finite and wrapped.
        for yaw in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(quantize_facing(yaw), Facing::North, "{yaw} is not a yaw");
        }
    }

    // ---------------------------------------------------------------------------
    // The pick
    // ---------------------------------------------------------------------------

    /// Nothing a structure draws escapes the ground the server validated for it, and
    /// nothing pokes through the headroom above it.
    ///
    /// The mesh and the footprint are two descriptions of one building, derived on this
    /// side from the same two constants the server derives its own from — so a tent whose
    /// canvas hung over the tenth cell would be claiming ground nobody checked was solid,
    /// and a flame that reached through the ceiling would be burning inside whatever a
    /// player later stacked on top of it.
    ///
    /// Downward is the one direction with slack, and it is named: the canvas beds half its
    /// own thickness into the ground it is pitched on, which is where a tent should touch.
    #[test]
    fn nothing_a_structure_draws_escapes_its_own_footprint() {
        for (kind, meshes) in [
            (StructureKind::Tent, vec![tent_mesh()]),
            (
                StructureKind::Forge,
                vec![
                    anvil_mesh(),
                    Mesh::from(Cuboid::from_size(HEARTH_SIZE)).translated_by(Vec3::new(
                        0.0,
                        HEARTH_SIZE.y / 2.0,
                        -1.0,
                    )),
                    flame_mesh(),
                ],
            ),
            (
                StructureKind::Campfire,
                vec![fire_ring_mesh(), fire_logs_mesh(), fire_flame_mesh()],
            ),
        ] {
            // The box the server validated, moved into the local space the meshes are
            // authored in: the middle of the anchor cell horizontally, its top face
            // vertically.
            let origin = Vec3::new(0.5, 1.0, 0.5);
            let (min, max) = bounds(kind, Facing::North, IVec3::ZERO);
            let (min, max) = (min - origin, max - origin);
            let bedding = Vec3::new(0.0, CANVAS_THICKNESS / 2.0, 0.0);

            for mesh in &meshes {
                let Some(positions) = mesh.attribute(Mesh::ATTRIBUTE_POSITION) else {
                    panic!("every part carries positions");
                };
                for vertex in positions.as_float3().expect("three floats per position") {
                    let vertex = Vec3::from_array(*vertex);
                    assert!(
                        vertex.cmpge(min - bedding).all() && vertex.cmple(max).all(),
                        "a {kind:?} draws a vertex at {vertex:?}, outside {min:?}..{max:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_ray_enters_a_box_where_the_slabs_say_it_does() {
        let min = Vec3::new(2.0, 0.0, -1.0);
        let max = Vec3::new(5.0, 2.0, 2.0);
        let eye = Vec3::new(0.5, 1.0, 0.5);

        assert_eq!(ray_box_entry(eye, Vec3::X, min, max), Some(1.5));
        assert_eq!(
            ray_box_entry(eye, Vec3::X * 4.0, min, max),
            Some(1.5),
            "the direction is normalised, so the answer is a distance in blocks"
        );
        assert_eq!(
            ray_box_entry(eye, Vec3::NEG_X, min, max),
            None,
            "a box behind the ray is not entered"
        );
        assert_eq!(
            ray_box_entry(Vec3::new(3.0, 1.0, 0.5), Vec3::X, min, max),
            Some(0.0),
            "a ray that starts inside enters at zero"
        );
        assert_eq!(
            ray_box_entry(Vec3::new(0.5, 9.0, 0.5), Vec3::X, min, max),
            None,
            "a ray parallel to a slab it is outside never enters"
        );
        for direction in [Vec3::ZERO, Vec3::new(f32::NAN, 0.0, 1.0)] {
            assert_eq!(
                ray_box_entry(eye, direction, min, max),
                None,
                "{direction:?} is not a direction"
            );
        }
    }

    fn standing(state: &StructureState) -> Structure {
        Structure::of(state, LOCAL_ID)
    }

    /// The rule the acceptance criterion names: the nearer of the two wins, and a
    /// structure standing behind the voxel the crosshair resolved does not capture the
    /// press.
    #[test]
    fn the_nearest_hit_wins_over_the_block_behind_it() {
        let eye = Vec3::new(0.5, 81.62, 0.5);
        let tent = standing(&tent_at(900, [3, 80, 0], LOCAL_ID));

        // Nothing else on the ray: the structure is picked, 1.5 blocks along.
        let picked = pick_structure(eye, Vec3::X, None, [tent].into_iter())
            .expect("an own structure straight ahead");
        assert_eq!(picked.structure_id, 900);
        assert!((picked.distance - 1.5).abs() < 1e-4, "{}", picked.distance);

        // A wall *behind* it changes nothing.
        let behind = BlockHit {
            block: IVec3::new(4, 81, 0),
            face: IVec3::NEG_X,
        };
        assert_eq!(
            pick_structure(eye, Vec3::X, Some(behind), [tent].into_iter()).map(|p| p.structure_id),
            Some(900)
        );

        // A wall *in front* of it takes the press back, because the voxel is what the ray
        // reached first and digging it is what the player was pointing at.
        let front = BlockHit {
            block: IVec3::new(1, 81, 0),
            face: IVec3::NEG_X,
        };
        assert_eq!(
            pick_structure(eye, Vec3::X, Some(front), [tent].into_iter()),
            None
        );
    }

    #[test]
    fn a_structure_whose_owner_is_offline_is_not_this_sessions_own() {
        // V5's offline owner, decided by the one comparison that reads the field. The
        // codec accepts `owner_entity_id == 0` from V5 on; what it means to a session
        // is this, and it is answered here rather than by a copy of this line in a
        // codec test.
        let offline = standing(&tent_at(900, [3, 80, 0], 0));

        assert!(
            !offline.own,
            "an owner with no live session is not this session"
        );
        assert_eq!(offline.owner_entity_id, 0);

        // And it follows through to what the button does: an offline owner's camp is
        // not a removal target.
        let eye = Vec3::new(0.5, 81.62, 0.5);
        assert_eq!(
            pick_structure(eye, Vec3::X, None, [offline].into_iter()),
            None
        );
    }

    #[test]
    fn only_this_sessions_own_structures_are_picked() {
        let eye = Vec3::new(0.5, 81.62, 0.5);
        let theirs = standing(&tent_at(900, [3, 80, 0], OTHER_ID));
        assert!(!theirs.own);
        assert_eq!(
            pick_structure(eye, Vec3::X, None, [theirs].into_iter()),
            None
        );

        // Even standing in front of one of this player's own, someone else's is skipped
        // rather than shadowing it.
        let mine = standing(&tent_at(901, [6, 80, 0], LOCAL_ID));
        assert_eq!(
            pick_structure(eye, Vec3::X, None, [theirs, mine].into_iter())
                .map(|picked| picked.structure_id),
            Some(901)
        );
    }

    #[test]
    fn a_structure_past_the_reach_is_not_picked() {
        let eye = Vec3::new(0.5, 81.62, 0.5);
        // Entered at exactly MAX_REACH, which is within it — the same inclusive boundary
        // the voxel traversal promises for a block entered exactly at its own limit.
        let edge = standing(&tent_at(900, [6, 80, 0], LOCAL_ID));
        assert_eq!(
            pick_structure(eye, Vec3::X, None, [edge].into_iter()).map(|p| p.distance),
            Some(MAX_REACH)
        );

        let far = standing(&tent_at(901, [7, 80, 0], LOCAL_ID));
        assert_eq!(pick_structure(eye, Vec3::X, None, [far].into_iter()), None);
    }

    #[test]
    fn the_nearest_of_two_own_structures_wins() {
        let eye = Vec3::new(0.5, 81.62, 0.5);
        let near = standing(&tent_at(900, [3, 80, 0], LOCAL_ID));
        let far = standing(&tent_at(901, [6, 80, 0], LOCAL_ID));

        for candidates in [[near, far], [far, near]] {
            assert_eq!(
                pick_structure(eye, Vec3::X, None, candidates.into_iter())
                    .map(|picked| picked.structure_id),
                Some(900),
                "the order the query happened to yield decided the answer"
            );
        }
    }

    // ---------------------------------------------------------------------------
    // Through the app
    // ---------------------------------------------------------------------------

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: LOCAL_ID,
            spawn: SPAWN,
            world_seed: 1,
            tick_rate: 20,
            chunk_size: SIZE,
            view_distance: 8,
            inventory_slots: 37,
            hotbar_slots: 9,
            equipment_slots: 4,
            player_token: crate::net::ANY_TOKEN,
        })
    }

    fn tent_at(structure_id: u64, anchor: [i32; 3], owner: u64) -> StructureState {
        StructureState {
            structure_id,
            kind: StructureKind::Tent,
            anchor: BlockCoord {
                x: anchor[0],
                y: anchor[1],
                z: anchor[2],
            },
            facing: Facing::North,
            owner_entity_id: owner,
        }
    }

    fn forge_at(structure_id: u64, anchor: [i32; 3], owner: u64) -> StructureState {
        StructureState {
            kind: StructureKind::Forge,
            facing: Facing::East,
            ..tent_at(structure_id, anchor, owner)
        }
    }

    fn campfire_at(structure_id: u64, anchor: [i32; 3], owner: u64) -> StructureState {
        StructureState {
            kind: StructureKind::Campfire,
            facing: Facing::South,
            ..tent_at(structure_id, anchor, owner)
        }
    }

    /// A chunk store holding one chunk of air with `solid` set, in **world** coordinates.
    fn store_with(solid: &[IVec3]) -> ChunkStore {
        let mut chunk = VoxelChunk::all_air(usize::from(SIZE));
        for voxel in solid {
            let edge = i32::from(SIZE);
            chunk.set(
                voxel.x.rem_euclid(edge) as usize,
                voxel.y.rem_euclid(edge) as usize,
                voxel.z.rem_euclid(edge) as usize,
                palette::STONE,
            );
        }

        let mut store = ChunkStore::default();
        store.insert(
            ChunkCoord {
                cx: 0,
                cy: 2,
                cz: 0,
            },
            chunk,
        );
        store
    }

    /// The player module on a headless app, aiming along +x at the world in `store`.
    ///
    /// The look state is set rather than driven through the pointer, and one snapshot
    /// places the body the camera follows — without it `follow_the_player` has nothing to
    /// attach to and the camera keeps the identity rotation, so every ray would go down -z
    /// whatever the look state said. Tick **zero**, so every [`deliver`] below is newer
    /// than it and the fixture never has to know which tick a test starts at.
    fn aiming_app(store: ChunkStore) -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .insert_resource(session())
            .insert_resource(store)
            .add_plugins(PlayerPlugin);

        // A quarter turn to the right of -z is +x, which is East on the compass above.
        *app.world_mut().resource_mut::<LookState>() = LookState {
            yaw: -FRAC_PI_2,
            pitch: 0.0,
        };
        deliver(&mut app, 0, vec![]);
        app
    }

    /// An aiming app with a mouse, an inventory and somewhere for the frames to go.
    ///
    /// The queue is deeper than any of these tests needs, so a full one can never be what
    /// makes a request go missing.
    fn clicking_app(store: ChunkStore, slot_zero: InventoryStack) -> (App, Receiver<Vec<u8>>) {
        let mut app = aiming_app(store);
        let (outbound, sent) = Outbound::to_a_test(64);
        app.add_plugins(InputPlugin).insert_resource(outbound);

        let mut stacks = vec![InventoryStack::default(); 36];
        stacks[0] = slot_zero;
        app.world_mut()
            .resource_mut::<InventoryInbox>()
            .push(InventoryState { stacks });
        (app, sent)
    }

    /// One of an item, which is every stack these tests need except the blade.
    fn one(item_id: u16) -> InventoryStack {
        InventoryStack {
            item_id,
            count: 1,
            ..Default::default()
        }
    }

    /// One blade at full health, whichever blade it is.
    fn blade_of(item_id: u16) -> InventoryStack {
        InventoryStack {
            item_id,
            count: 1,
            durability: 100,
            max_durability: 100,
        }
    }

    /// Queues a snapshot as the net thread would, always naming the local player so the
    /// camera has a body to follow.
    fn deliver(app: &mut App, tick: u32, structures: Vec<StructureState>) {
        app.world_mut().resource_mut::<SnapshotInbox>().push(
            Snapshot {
                server_tick: tick,
                entities: vec![EntityState {
                    entity_id: LOCAL_ID,
                    pos: SPAWN,
                    vel: [0.0; 3],
                    yaw: 0.0,
                }],
                structures,
                ..Default::default()
            },
            Instant::now(),
        );
    }

    /// Every structure the module has spawned, as (id, kind, whether it is this
    /// session's), sorted so a failure reads cleanly.
    fn drawn(app: &mut App) -> Vec<(u64, StructureKind, bool)> {
        let world = app.world_mut();
        let mut query = world.query::<&Structure>();
        let mut found: Vec<_> = query
            .iter(world)
            .map(|structure| (structure.structure_id, structure.kind, structure.own))
            .collect();
        found.sort_by_key(|(structure_id, _, _)| *structure_id);
        found
    }

    /// Every campfire light standing, with where it sits and how it inherits visibility.
    fn lights(app: &mut App) -> Vec<(PointLight, Transform, Visibility)> {
        let world = app.world_mut();
        let mut query =
            world.query_filtered::<(&PointLight, &Transform, &Visibility), With<FireLight>>();
        query
            .iter(world)
            .map(|(light, transform, visibility)| (*light, *transform, *visibility))
            .collect()
    }

    /// The meshes and materials every structure part is drawn from.
    fn parts(app: &mut App) -> Vec<(Handle<Mesh>, Handle<StandardMaterial>)> {
        let world = app.world_mut();
        let mut query = world
            .query_filtered::<(&Mesh3d, &MeshMaterial3d<StandardMaterial>), With<StructureVisual>>(
            );
        query
            .iter(world)
            .map(|(mesh, material)| (mesh.0.clone(), material.0.clone()))
            .collect()
    }

    /// Presses a mouse button the way a window does, so `InputPlugin`'s own system is
    /// what sets `just_pressed`. Poking the resource would arrive at Update already
    /// forgotten, because `mouse_button_input_system` clears it every frame.
    fn click(app: &mut App, button: MouseButton) {
        app.world_mut().write_message(MouseButtonInput {
            button,
            state: ButtonState::Pressed,
            window: Entity::PLACEHOLDER,
        });
    }

    /// Releases it. Needed between two clicks of the same button: `ButtonInput::press`
    /// only records `just_pressed` for a button that was not already down, so a second
    /// press with no release in between is not a second click.
    fn release(app: &mut App, button: MouseButton) {
        app.world_mut().write_message(MouseButtonInput {
            button,
            state: ButtonState::Released,
            window: Entity::PLACEHOLDER,
        });
    }

    fn drain(sent: &Receiver<Vec<u8>>) {
        while sent.try_recv().is_ok() {}
    }

    /// One placement request as the fields the server reads: anchor, facing, slot, tick.
    type Placement = ([i32; 3], fb::Facing, u8, u32);

    /// Filtered out of the encoded bytes, because this queue also carries the tick-paced
    /// input stream.
    fn placements(sent: &Receiver<Vec<u8>>) -> Vec<Placement> {
        let mut found = Vec::new();
        while let Ok(frame) = sent.try_recv() {
            let envelope = fb::root_as_envelope(&frame).expect("the client's own bytes are valid");
            let Some(request) = envelope.payload_as_place_structure_request() else {
                continue;
            };
            let anchor = request.anchor().expect("the anchor is always written");
            found.push((
                [anchor.x(), anchor.y(), anchor.z()],
                request.facing(),
                request.slot(),
                request.client_tick(),
            ));
        }
        found
    }

    /// Every request of every kind a press could have produced, named so a failure says
    /// which one went out instead of the expected silence.
    #[derive(Debug, Default, PartialEq, Eq)]
    struct Requests {
        placements: usize,
        removals: Vec<u64>,
        edits: usize,
        mines: usize,
        attacks: usize,
    }

    fn requests(sent: &Receiver<Vec<u8>>) -> Requests {
        let mut found = Requests::default();
        while let Ok(frame) = sent.try_recv() {
            let envelope = fb::root_as_envelope(&frame).expect("the client's own bytes are valid");
            match envelope.payload_type() {
                fb::Payload::PlaceStructureRequest => found.placements += 1,
                fb::Payload::RemoveStructureRequest => found.removals.push(
                    envelope
                        .payload_as_remove_structure_request()
                        .expect("the payload the tag names")
                        .structure_id(),
                ),
                fb::Payload::BlockEditRequest => found.edits += 1,
                fb::Payload::MineRequest => found.mines += 1,
                fb::Payload::AttackRequest => found.attacks += 1,
                _ => {}
            }
        }
        found
    }

    #[test]
    fn a_snapshot_with_two_structures_stands_them_up() {
        let mut app = aiming_app(store_with(&[]));
        deliver(
            &mut app,
            1,
            vec![
                tent_at(900, [3, 80, 0], LOCAL_ID),
                forge_at(901, [8, 80, 4], OTHER_ID),
            ],
        );
        app.update();

        assert_eq!(
            drawn(&mut app),
            vec![
                (900, StructureKind::Tent, true),
                (901, StructureKind::Forge, false),
            ]
        );
    }

    /// The complete-existence-set rule, the same one mobs and drops obey. Why a structure
    /// stopped being sent — taken back, collapsed, or simply out of view — is not asked.
    #[test]
    fn a_structure_omitted_by_the_newest_snapshot_is_gone() {
        let mut app = aiming_app(store_with(&[]));
        deliver(
            &mut app,
            1,
            vec![
                tent_at(900, [3, 80, 0], LOCAL_ID),
                forge_at(901, [8, 80, 4], LOCAL_ID),
            ],
        );
        app.update();
        assert_eq!(drawn(&mut app).len(), 2);

        deliver(&mut app, 2, vec![forge_at(901, [8, 80, 4], LOCAL_ID)]);
        app.update();

        assert_eq!(drawn(&mut app), vec![(901, StructureKind::Forge, true)]);
        assert_eq!(
            parts(&mut app).len(),
            3,
            "the removed tent's canvas survived its anchor"
        );
    }

    /// The drawn-mesh smoke test: a tent is one panel of canvas, a forge is an anvil, a
    /// hearth and a flame, and every structure of a kind shares the same mesh assets.
    #[test]
    fn a_tent_and_a_forge_are_drawn_from_the_shared_meshes_of_their_kind() {
        let mut app = aiming_app(store_with(&[]));
        deliver(&mut app, 1, vec![tent_at(900, [3, 80, 0], LOCAL_ID)]);
        app.update();

        let (tent, anvil, hearth, flame) = {
            let held = app.world().resource::<StructureVisuals>();
            (
                held.tent.clone(),
                held.anvil.clone(),
                held.hearth.clone(),
                held.flame.clone(),
            )
        };
        assert_eq!(
            parts(&mut app)
                .into_iter()
                .map(|(mesh, _)| mesh)
                .collect::<Vec<_>>(),
            vec![tent.clone()],
            "a tent is one piece of canvas"
        );

        deliver(
            &mut app,
            2,
            vec![
                tent_at(900, [3, 80, 0], LOCAL_ID),
                forge_at(901, [8, 80, 4], LOCAL_ID),
            ],
        );
        app.update();

        let meshes: Vec<Handle<Mesh>> = parts(&mut app).into_iter().map(|(m, _)| m).collect();
        assert_eq!(
            meshes.len(),
            4,
            "a tent and a forge draw four parts between them"
        );
        for (name, want) in [
            ("the tent's canvas", tent),
            ("the anvil", anvil),
            ("the hearth", hearth),
            ("the flame", flame),
        ] {
            assert!(meshes.contains(&want), "{name} was not drawn");
        }
    }

    /// A player has to be able to tell their own camp from somebody else's at a glance.
    #[test]
    fn a_structure_this_session_owns_is_drawn_in_different_colours() {
        let mut app = aiming_app(store_with(&[]));
        deliver(
            &mut app,
            1,
            vec![
                tent_at(900, [3, 80, 0], LOCAL_ID),
                tent_at(901, [8, 80, 4], OTHER_ID),
                forge_at(902, [12, 80, 0], LOCAL_ID),
                forge_at(903, [16, 80, 0], OTHER_ID),
            ],
        );
        app.update();

        let (canvas, stone) = {
            let visuals = app.world().resource::<StructureVisuals>();
            (visuals.canvas.clone(), visuals.stone.clone())
        };
        assert_ne!(canvas[0], canvas[1], "both tents share one bolt of canvas");
        assert_ne!(stone[0], stone[1], "both hearths share one pile of rock");

        let materials: Vec<_> = parts(&mut app).into_iter().map(|(_, m)| m).collect();
        for (name, own, other) in [
            ("canvas", &canvas[1], &canvas[0]),
            ("stone", &stone[1], &stone[0]),
        ] {
            assert!(
                materials.contains(own),
                "no structure was drawn in this session's own {name}"
            );
            assert!(
                materials.contains(other),
                "no structure was drawn in another session's {name}"
            );
        }
    }

    /// Structures never move, so they are not on the entity-motion path: the transform is
    /// the anchor, exactly, on the frame it arrives and on every frame after — where an
    /// interpolated body would be walking between two samples.
    #[test]
    fn a_structure_stands_exactly_on_its_anchor_and_never_moves() {
        let mut app = aiming_app(store_with(&[]));
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            50,
        )));
        deliver(&mut app, 1, vec![tent_at(900, [3, 80, -2], LOCAL_ID)]);
        app.update();

        let placed = |app: &mut App| {
            let world = app.world_mut();
            let mut query = world.query_filtered::<&Transform, With<Structure>>();
            *query.single(world).expect("one structure")
        };

        // The middle of the anchor cell horizontally, one block above it vertically —
        // because the anchor is the ground the tent rests on, not part of the tent.
        let want = Vec3::new(3.5, 81.0, -1.5);
        assert_eq!(placed(&mut app).translation, want);

        for tick in 2..6 {
            deliver(&mut app, tick, vec![tent_at(900, [3, 80, -2], LOCAL_ID)]);
            app.update();
            assert_eq!(
                placed(&mut app).translation,
                want,
                "the structure moved on tick {tick}"
            );
        }
        assert_eq!(
            placed(&mut app).rotation,
            Quat::from_rotation_y(facing_yaw(Facing::North))
        );
    }

    #[test]
    fn a_ui_mode_hides_the_structures_without_despawning_them() {
        let mut app = aiming_app(store_with(&[]));
        deliver(&mut app, 1, vec![tent_at(900, [3, 80, 0], LOCAL_ID)]);
        app.update();

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Chat;
        app.update();
        {
            let world = app.world_mut();
            let mut query = world.query_filtered::<&Visibility, With<Structure>>();
            assert_eq!(
                *query.single(world).expect("one structure"),
                Visibility::Visible
            );
        }

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Inventory;
        app.update();

        let world = app.world_mut();
        let mut query = world.query_filtered::<&Visibility, With<Structure>>();
        assert_eq!(
            *query.single(world).expect("one structure"),
            Visibility::Hidden,
            "an open pack left the camp drawn"
        );
        assert_eq!(drawn(&mut app).len(), 1, "hiding despawned the structure");
    }

    // ---------------------------------------------------------------------------
    // The campfire
    // ---------------------------------------------------------------------------

    /// A campfire stands up on the same path a tent and a forge do, and going out of the
    /// newest snapshot takes it — and its light — away with it.
    #[test]
    fn a_campfire_is_a_structure_like_any_other_and_its_light_goes_with_it() {
        let mut app = aiming_app(store_with(&[]));
        deliver(&mut app, 1, vec![campfire_at(900, [3, 80, 0], LOCAL_ID)]);
        app.update();

        assert_eq!(drawn(&mut app), vec![(900, StructureKind::Campfire, true)]);
        assert_eq!(
            parts(&mut app).len(),
            3,
            "a campfire is a ring, its logs and a flame"
        );
        assert_eq!(
            lights(&mut app).len(),
            1,
            "the fire was drawn without a light"
        );

        // Gone from the newest answer. Why is not asked, and the light is not a second
        // thing to remember to clean up: it is a child of the structure that went.
        deliver(&mut app, 2, vec![]);
        app.update();
        assert!(drawn(&mut app).is_empty());
        assert!(
            lights(&mut app).is_empty(),
            "the light outlived the fire it belonged to"
        );
    }

    /// The campfire draws from its own meshes, and its ring is the fieldstone a forge's
    /// hearth is built from — in this session's own shade when this session owns it.
    #[test]
    fn a_campfire_is_drawn_from_its_own_meshes_and_the_owner_s_stone() {
        let mut app = aiming_app(store_with(&[]));
        deliver(
            &mut app,
            1,
            vec![
                campfire_at(900, [3, 80, 0], LOCAL_ID),
                campfire_at(901, [8, 80, 4], OTHER_ID),
            ],
        );
        app.update();

        let (ring, logs, flame, stone, ember, charred) = {
            let held = app.world().resource::<StructureVisuals>();
            (
                held.fire_ring.clone(),
                held.fire_logs.clone(),
                held.fire_flame.clone(),
                held.stone.clone(),
                held.ember.clone(),
                held.charred.clone(),
            )
        };

        let drawn_parts = parts(&mut app);
        assert_eq!(drawn_parts.len(), 6, "two fires draw three parts each");

        let meshes: Vec<_> = drawn_parts.iter().map(|(mesh, _)| mesh.clone()).collect();
        for (name, want) in [("ring", ring), ("logs", logs), ("flame", flame)] {
            assert!(
                meshes.contains(&want),
                "the campfire's {name} was not drawn"
            );
        }

        let materials: Vec<_> = drawn_parts.iter().map(|(_, m)| m.clone()).collect();
        assert!(
            materials.contains(&stone[1]),
            "no fire in this session's own stone"
        );
        assert!(
            materials.contains(&stone[0]),
            "no fire in another session's stone"
        );
        assert!(materials.contains(&ember), "the flame is not lit");
        assert!(
            materials.contains(&charred),
            "the logs are not charred wood"
        );
    }

    /// **The light is presentation, and the number it carries is not the server's rule.**
    ///
    /// `game.CampfireSafeRadius` is sixteen blocks and is checked on the server, in
    /// `spawn.go`, against the structures it is holding. The glow deliberately does not
    /// reach that far, so nothing about where a player can see a fire can be read as
    /// where a creature may appear. A future issue that made the two equal would be
    /// stating a gameplay rule in a renderer.
    ///
    /// That half is a `const` assertion beside [`FIRE_LIGHT_RANGE`] rather than a line
    /// here, for the reason the constants module gives: a build should not be able to
    /// violate it at all. What is left for a test is that the light really carries these
    /// numbers onto an entity.
    #[test]
    fn the_fires_light_is_not_the_ground_the_server_keeps() {
        let mut app = aiming_app(store_with(&[]));
        deliver(&mut app, 1, vec![campfire_at(900, [3, 80, 0], LOCAL_ID)]);
        app.update();

        let lit = lights(&mut app);
        assert_eq!(lit.len(), 1);
        assert_eq!(lit[0].0.range, FIRE_LIGHT_RANGE);
        assert_eq!(lit[0].0.color, FIRE_LIGHT_COLOUR);
        assert!(
            !lit[0].0.shadow_maps_enabled,
            "the fire asked for a shadow map, which nothing else in this client does"
        );

        // A child of the structure, sitting in the flame rather than on the ground, and
        // inheriting the parent's visibility — which is what hides it with the rest of
        // the world when a panel owns the screen.
        assert_eq!(lit[0].1.translation, Vec3::Y * fire_light_height());
        assert_eq!(lit[0].2, Visibility::Inherited);
    }

    /// The flicker is bounded, moves on local time alone, and decides nothing.
    ///
    /// It is allowed to be a guess about what looks good; it is not allowed to reach zero
    /// or to flash, because either would read as the fire going out — and whether a fire
    /// is out is the server's answer, delivered by the structure leaving the snapshot.
    #[test]
    fn the_flicker_is_bounded_and_never_puts_the_fire_out() {
        let mut app = aiming_app(store_with(&[]));
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            17,
        )));
        deliver(&mut app, 1, vec![campfire_at(900, [3, 80, 0], LOCAL_ID)]);
        app.update();

        let (low, high) = (
            FIRE_LIGHT_LUMENS * (1.0 - FLICKER_DEPTH),
            FIRE_LIGHT_LUMENS * (1.0 + FLICKER_DEPTH),
        );
        let mut seen: Vec<f32> = Vec::new();
        for frame in 0..120 {
            app.update();
            let lit = lights(&mut app);
            assert_eq!(lit.len(), 1, "the fire went out on frame {frame}");
            let intensity = lit[0].0.intensity;
            assert!(
                (low - 1e-3..=high + 1e-3).contains(&intensity),
                "frame {frame} lit the fire at {intensity}, outside {low}..{high}"
            );
            seen.push(intensity);
        }

        // It genuinely moves — a "flicker" that held one value would pass every bound
        // above while doing nothing at all.
        let (min, max) = seen.iter().fold((f32::MAX, f32::MIN), |(lo, hi), value| {
            (lo.min(*value), hi.max(*value))
        });
        assert!(
            max - min > FIRE_LIGHT_LUMENS * FLICKER_DEPTH / 4.0,
            "the flicker moved by {} lumens over two seconds, which is not a flicker",
            max - min
        );
    }

    /// Two fires in one camp do not pulse in lockstep.
    ///
    /// **Adjacent ids on purpose.** Two fires planted one after another are what a camp
    /// actually holds, and consecutive identities are the case a phase derived from the id
    /// is most likely to collapse on.
    #[test]
    fn two_fires_flicker_out_of_phase_with_one_another() {
        let mut app = aiming_app(store_with(&[]));
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            17,
        )));
        deliver(
            &mut app,
            1,
            vec![
                campfire_at(900, [3, 80, 0], LOCAL_ID),
                campfire_at(901, [8, 80, 4], LOCAL_ID),
            ],
        );
        app.update();

        let mut apart = false;
        for _ in 0..60 {
            app.update();
            let lit = lights(&mut app);
            assert_eq!(lit.len(), 2);
            if (lit[0].0.intensity - lit[1].0.intensity).abs() > FIRE_LIGHT_LUMENS * 1e-3 {
                apart = true;
                break;
            }
        }
        assert!(apart, "two fires flickered as one");
    }

    /// A campfire in hand routes the place press to a structure request, not a block edit.
    ///
    /// The same predicate the tent and the forge go through, so the two intents stay
    /// mutually exclusive rather than merely unlikely to overlap.
    #[test]
    fn a_campfire_in_hand_asks_for_a_structure_rather_than_a_voxel() {
        assert_eq!(
            structure_in_hand(Some(ITEM_CAMPFIRE)),
            Some(StructureKind::Campfire)
        );
        assert_eq!(
            structure_in_hand(Some(ITEM_TENT)),
            Some(StructureKind::Tent)
        );
        assert_eq!(structure_in_hand(None), None);
        assert_eq!(structure_in_hand(Some(ITEM_IRON_SWORD)), None);

        let ground = IVec3::new(3, 81, 0);
        let (mut app, sent) = clicking_app(store_with(&[ground]), one(ITEM_CAMPFIRE));
        app.update();

        // **The one-cell footprint.** A campfire's is the anchor cell and nothing else,
        // which is why the single-voxel outline used to be right for it — accidentally,
        // and only for it: a tent covers nine and a forge two. Since legacy PR 205 the ghost draws
        // the footprint for every kind and the outline stands down, so what is asserted
        // here is what it always meant to assert — that the cell under the crosshair is
        // the ground the fire will stand on, and the one the press then names.
        let outlined = app
            .world()
            .resource::<BlockTarget>()
            .0
            .expect("the crosshair is on the ground")
            .block;
        assert_eq!(outlined, ground);
        assert_eq!(
            bounds(StructureKind::Campfire, Facing::North, outlined),
            (
                outlined.as_vec3() + Vec3::Y,
                outlined.as_vec3() + Vec3::ONE + Vec3::Y
            ),
            "the fire does not occupy exactly the cell above the one the outline marks"
        );

        drain(&sent);
        click(&mut app, PLACE_BUTTON);
        app.update();

        assert_eq!(
            requests(&sent),
            Requests {
                placements: 1,
                ..Requests::default()
            },
            "a campfire in hand did not ask for a structure, or asked for a voxel too"
        );

        // And the refusal feedback is the one the path already gives: silence. Nothing
        // stands up locally, because the server owns whether the placement was legal.
        assert!(
            drawn(&mut app).is_empty(),
            "asking for a campfire put one in the world without the server saying so"
        );
    }

    // ---------------------------------------------------------------------------
    // Showing where it would stand
    // ---------------------------------------------------------------------------

    /// The cells a footprint covers, as sortable keys.
    ///
    /// `Vec3` has no total order — a component could be NaN — so an ordered comparison
    /// goes through the array and says so. Nothing is rounded: a ghost that stood half a
    /// block off the grid must fail rather than be tidied into agreement.
    fn sorted(cells: impl IntoIterator<Item = Vec3>) -> Vec<[f32; 3]> {
        let mut keys: Vec<[f32; 3]> = cells.into_iter().map(|cell| cell.to_array()).collect();
        keys.sort_by(|a, b| a.partial_cmp(b).expect("no footprint cell is NaN"));
        keys
    }

    /// Every ghost cell that is being drawn, and where.
    fn ghost_cells(app: &mut App) -> Vec<Vec3> {
        let world = app.world_mut();
        let mut query =
            world.query_filtered::<(&Transform, &Visibility), With<super::FootprintGhost>>();
        let found: Vec<(Vec3, Visibility)> = query
            .iter(world)
            .map(|(transform, visibility)| (transform.translation, *visibility))
            .collect();

        assert_eq!(
            found.len(),
            MAX_FOOTPRINT_CELLS,
            "the ghost is spawned once at the widest footprint and reused"
        );
        found
            .into_iter()
            .filter(|(_, visibility)| *visibility == Visibility::Visible)
            .map(|(translation, _)| translation)
            .collect()
    }

    /// The ghost is spawned wide enough for every kind, not merely for the tent.
    ///
    /// The constant was `TENT_FOOTPRINT.len()`, which is the same number and a different
    /// claim: it tracked one array while the array it had to cover was whichever is
    /// widest. This asserts the property the name promises, over every kind
    /// [`ALL_STRUCTURE_KINDS`] carries, so a member with a wider footprint fails here
    /// rather than in [`footprint_preview`] on the frame a player selects it.
    #[test]
    fn the_ghosts_capacity_is_the_widest_footprint_of_any_kind() {
        let widest = ALL_STRUCTURE_KINDS
            .iter()
            .map(|kind| footprint_offsets(*kind).len())
            .max()
            .expect("the contract names at least one structure kind");

        assert_eq!(MAX_FOOTPRINT_CELLS, widest);
        for kind in ALL_STRUCTURE_KINDS {
            let cells = footprint_offsets(kind).len();
            assert!(
                cells <= MAX_FOOTPRINT_CELLS,
                "{kind:?} covers {cells} cells and the ghost holds {MAX_FOOTPRINT_CELLS}"
            );
            // The preview really is the whole footprint for every kind that exists — the
            // clause above is only interesting because this one holds.
            assert_eq!(
                footprint_preview(kind, Facing::North, IVec3::ZERO)
                    .cells()
                    .len(),
                cells,
                "{kind:?}"
            );
        }
    }

    /// A footprint wider than the ghost draws short instead of indexing past it.
    ///
    /// **No kind in this build can reach this**, and that is the point: the capacity is
    /// folded over [`ALL_STRUCTURE_KINDS`], so the only way past it is a kind added to
    /// [`footprint_offsets`] and forgotten in that list — the one link no compiler checks.
    /// What it costs is asserted here rather than argued in a comment: the cells that fit
    /// are the cells that would have been drawn, the rest are dropped, and the ghost pool
    /// hides the outlines nobody claimed. The old `preview.cells[preview.len] = ...` would
    /// have panicked on the tenth cell, every frame the thing was in hand.
    #[test]
    fn a_footprint_wider_than_the_ghost_is_drawn_short_rather_than_panicking() {
        let anchor = IVec3::new(-3, 70, 11);
        let wide: Vec<[i32; 2]> = (0..MAX_FOOTPRINT_CELLS as i32 + 4)
            .map(|step| [step, step * 2])
            .collect();

        let preview = footprint_preview_of(&wide, Facing::East, anchor);

        assert_eq!(preview.cells().len(), MAX_FOOTPRINT_CELLS);
        assert!(preview.active());
        let want: Vec<IVec3> = wide[..MAX_FOOTPRINT_CELLS]
            .iter()
            .map(|offset| {
                let [dx, dz] = rotate_offset(*offset, Facing::East);
                IVec3::new(anchor.x + dx, anchor.y, anchor.z + dz)
            })
            .collect();
        assert_eq!(preview.cells(), want);
    }

    /// The cells the ghost outlines are the cells the server checks, for every kind at
    /// every facing.
    ///
    /// **Derived rather than restated.** The expectation is built from
    /// [`footprint_offsets`] and [`rotate_offset`] — the two constants this module already
    /// mirrors from `server/internal/game/structure.go`, and the pair
    /// [`the_footprint_rotation_mirrors_the_servers`] pins against the server case for
    /// case. Writing the nine cells out by hand here would make this test agree with a
    /// preview that had drifted from the footprint the requests are validated against,
    /// which is the one thing it exists to catch.
    #[test]
    fn the_ghost_is_the_footprint_the_server_checks_at_every_facing() {
        let anchor = IVec3::new(4, 63, -7);
        for kind in ALL_STRUCTURE_KINDS {
            for facing in [Facing::North, Facing::East, Facing::South, Facing::West] {
                let preview = footprint_preview(kind, facing, anchor);
                let want: Vec<IVec3> = footprint_offsets(kind)
                    .iter()
                    .map(|offset| {
                        let [dx, dz] = rotate_offset(*offset, facing);
                        IVec3::new(anchor.x + dx, anchor.y, anchor.z + dz)
                    })
                    .collect();

                assert_eq!(preview.cells(), want, "{kind:?} facing {facing:?}");
                assert!(preview.active());
                // Every cell sits at the anchor's height, because a footprint is the
                // ground a structure rests on. The headroom above it is what the server
                // checks and what nothing here draws.
                for cell in preview.cells() {
                    assert_eq!(cell.y, anchor.y, "{kind:?} facing {facing:?}");
                }
            }
        }

        // The counts the issue names, so a footprint that silently lost a cell fails here
        // as well as against the server's.
        assert_eq!(
            footprint_preview(StructureKind::Tent, Facing::North, anchor)
                .cells()
                .len(),
            9
        );
        assert_eq!(
            footprint_preview(StructureKind::Forge, Facing::North, anchor)
                .cells()
                .len(),
            2
        );
        assert_eq!(
            footprint_preview(StructureKind::Campfire, Facing::North, anchor)
                .cells()
                .len(),
            1
        );
    }

    /// A tent in hand outlines all nine cells it would cover, oriented by the camera.
    ///
    /// End to end through the app: the raycast picks the anchor, the look state picks the
    /// facing, and the ghost stands on the cells the server would check. **Nothing is
    /// coloured by whether it would be allowed** — that answer is the server's and arrives
    /// as an `ActionRefused`.
    #[test]
    fn a_tent_in_hand_ghosts_all_nine_cells_it_would_cover() {
        let ground = IVec3::new(3, 81, 0);
        let (mut app, _sent) = clicking_app(store_with(&[ground]), one(ITEM_TENT));
        app.update();

        // `aiming_app` looks East, so this is the facing the request would carry too.
        let want = footprint_preview(StructureKind::Tent, Facing::East, ground)
            .cells()
            .iter()
            .map(|cell| cell.as_vec3())
            .collect::<Vec<_>>();

        let drawn = ghost_cells(&mut app);
        assert_eq!(drawn.len(), 9, "a tent covers nine cells");
        assert_eq!(sorted(drawn), sorted(want));
    }

    /// A forge's two cells are the ones that move with the camera.
    ///
    /// The tent's nine and the campfire's one are symmetric, so a rotation that did
    /// nothing at all would pass for both. The forge is the kind that would catch it.
    #[test]
    fn a_forges_ghost_turns_with_the_camera() {
        let floor = IVec3::new(0, 79, 0);
        let (mut app, _sent) = clicking_app(store_with(&[floor]), one(ITEM_FORGE));

        for (yaw, facing) in [
            (0.0, Facing::North),
            (-FRAC_PI_2, Facing::East),
            (PI, Facing::South),
            (FRAC_PI_2, Facing::West),
        ] {
            app.world_mut().resource_mut::<LookState>().yaw = yaw;
            // Aimed at the floor underfoot, so the anchor is the same voxel at every yaw
            // and only the hearth moves.
            app.world_mut().resource_mut::<LookState>().pitch = -MAX_PITCH;
            app.update();

            let want = footprint_preview(StructureKind::Forge, facing, floor)
                .cells()
                .iter()
                .map(|cell| cell.as_vec3())
                .collect::<Vec<_>>();

            let drawn = ghost_cells(&mut app);
            assert_eq!(
                drawn.len(),
                2,
                "{facing:?}: a forge covers an anvil and a hearth"
            );
            assert_eq!(sorted(drawn), sorted(want), "{facing:?}");
        }
    }

    /// Nothing is ghosted while the hand holds no structure, and nothing while the
    /// crosshair is on nothing.
    ///
    /// The second half is what keeps the ghost from being left standing where the player
    /// last looked at solid ground, which is the shape of bug the single-voxel outline's
    /// own `None` arm exists to prevent.
    #[test]
    fn nothing_is_ghosted_without_a_structure_in_hand_or_a_cell_to_stand_on() {
        let ground = IVec3::new(3, 81, 0);
        let (mut app, _sent) = clicking_app(store_with(&[ground]), blade_of(ITEM_IRON_SWORD));
        app.update();

        assert!(
            ghost_cells(&mut app).is_empty(),
            "a blade in hand ghosted a building"
        );
        assert!(!app.world().resource::<FootprintPreview>().active());

        // A structure in hand, but aimed at empty sky: the raycast produces no anchor, so
        // there is no cell for a footprint to be relative to.
        let (mut app, _sent) = clicking_app(ChunkStore::default(), one(ITEM_TENT));
        app.update();

        assert_eq!(app.world().resource::<BlockTarget>().0, None);
        assert!(
            ghost_cells(&mut app).is_empty(),
            "a tent was ghosted against no ground at all"
        );
    }

    // ---------------------------------------------------------------------------
    // Clicking
    // ---------------------------------------------------------------------------

    /// The place press carries the **targeted** voxel, because the anchor is the ground a
    /// structure rests on — not the empty cell in front of its face, which is where a
    /// *block* would go.
    #[test]
    fn a_placement_names_the_voxel_under_the_crosshair_and_the_way_the_camera_looks() {
        let wall = IVec3::new(3, 81, 0);
        let (mut app, sent) = clicking_app(store_with(&[wall]), one(ITEM_TENT));
        app.update();
        drain(&sent);

        click(&mut app, PLACE_BUTTON);
        app.update();

        let found = placements(&sent);
        assert_eq!(found.len(), 1, "one press sent {} placements", found.len());
        let tick = app.world().resource::<InputCadence>().client_tick;
        assert_eq!(
            found[0],
            ([wall.x, wall.y, wall.z], fb::Facing::East, 0, tick),
            "the anchor is the targeted voxel and the facing is the quantized camera yaw"
        );
    }

    /// The facing follows the camera and nothing else, so the anchor is held still while
    /// the player turns: aimed at the floor they are standing on, every yaw targets the
    /// same voxel and only the facing moves.
    #[test]
    fn a_placement_carries_the_camera_yaw_quantized_to_the_four_facings() {
        let floor = IVec3::new(0, 79, 0);
        let (mut app, sent) = clicking_app(store_with(&[floor]), one(ITEM_TENT));

        for (yaw, want) in [
            (0.0, fb::Facing::North),
            (-FRAC_PI_2, fb::Facing::East),
            (PI, fb::Facing::South),
            (FRAC_PI_2, fb::Facing::West),
            // Off a member but inside its quarter: the compass rounds rather than
            // demanding the player line themselves up exactly.
            (-FRAC_PI_2 + 0.3, fb::Facing::East),
        ] {
            *app.world_mut().resource_mut::<LookState>() = LookState {
                yaw,
                // Just short of straight down, which is where the look controls clamp.
                pitch: -MAX_PITCH,
            };
            app.update();
            drain(&sent);

            click(&mut app, PLACE_BUTTON);
            app.update();

            let found = placements(&sent);
            assert_eq!(found.len(), 1, "a yaw of {yaw} sent {found:?}");
            assert_eq!(
                (found[0].0, found[0].1),
                ([floor.x, floor.y, floor.z], want),
                "a yaw of {yaw} named the wrong cell or the wrong facing"
            );

            release(&mut app, PLACE_BUTTON);
            app.update();
        }
    }

    /// Nothing appears locally. The structure exists when a snapshot says it does, and a
    /// refusal is silence — so there is no ghost to withdraw and no deadline to do it on.
    #[test]
    fn a_placement_puts_nothing_in_the_world_until_a_snapshot_does() {
        let (mut app, sent) = clicking_app(store_with(&[IVec3::new(3, 81, 0)]), one(ITEM_TENT));
        app.update();
        drain(&sent);

        click(&mut app, PLACE_BUTTON);
        for _ in 0..5 {
            app.update();
        }

        assert_eq!(placements(&sent).len(), 1);
        assert!(
            drawn(&mut app).is_empty(),
            "the client stood a tent up on its own say-so"
        );
        assert!(parts(&mut app).is_empty());
    }

    /// One press, one request. A structure in hand routes the place press away from the
    /// block edit entirely, exactly as a blade routes the break press away from mining.
    #[test]
    fn a_structure_in_hand_sends_no_block_edit() {
        for item in [ITEM_TENT, ITEM_FORGE] {
            let (mut app, sent) = clicking_app(store_with(&[IVec3::new(3, 81, 0)]), one(item));
            app.update();
            drain(&sent);

            click(&mut app, PLACE_BUTTON);
            app.update();

            let found = requests(&sent);
            assert_eq!(found.placements, 1, "item {item} sent no placement");
            assert_eq!(found.edits, 0, "item {item} also asked to place a block");
        }
    }

    /// And the other half: an ordinary item still places a block, so the routing is a
    /// branch on what is held rather than the place press being taken away.
    #[test]
    fn an_ordinary_item_still_places_a_block() {
        let (mut app, sent) =
            clicking_app(store_with(&[IVec3::new(3, 81, 0)]), one(palette::STONE));
        app.update();
        drain(&sent);

        click(&mut app, PLACE_BUTTON);
        app.update();

        let found = requests(&sent);
        assert_eq!(found.edits, 1);
        assert_eq!(found.placements, 0);
    }

    #[test]
    fn the_mine_press_on_an_own_structure_asks_for_it_back_instead_of_mining() {
        // A wall behind the tent, so there is genuinely something else the press could
        // have acted on. The tent is entered 1.5 blocks along and the wall 3.5.
        let (mut app, sent) =
            clicking_app(store_with(&[IVec3::new(4, 81, 0)]), one(palette::STONE));
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            50,
        )));
        deliver(&mut app, 1, vec![tent_at(900, [3, 80, 0], LOCAL_ID)]);
        app.update();
        drain(&sent);

        click(&mut app, REMOVE_BUTTON);
        app.update();
        app.update();

        let found = requests(&sent);
        assert_eq!(found.removals, vec![900], "the press named the wrong id");
        assert_eq!(found.mines, 0, "the press also started digging the wall");
        assert_eq!(
            drawn(&mut app).len(),
            1,
            "the client took its own tent down without being told to"
        );
    }

    /// Somebody else's camp never captures the press — the wall behind it is what the
    /// player was pointing at, and it is what gets mined.
    #[test]
    fn the_mine_press_on_another_players_structure_mines_the_block_behind_it() {
        let wall = IVec3::new(4, 81, 0);
        let (mut app, sent) = clicking_app(store_with(&[wall]), one(palette::STONE));
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            50,
        )));
        deliver(&mut app, 1, vec![tent_at(900, [3, 80, 0], OTHER_ID)]);
        app.update();
        drain(&sent);

        click(&mut app, REMOVE_BUTTON);
        app.update();
        app.update();

        let found = requests(&sent);
        assert!(
            found.removals.is_empty(),
            "another player's tent was removed"
        );
        assert!(found.mines > 0, "the wall behind it was not mined");
    }

    /// A blade in hand does not turn a removal into a swing either: the structure takes
    /// the press first, and one press sends at most one request.
    ///
    /// Both blades, because `AimStructures` ordering is what decides this and it knows
    /// nothing about which blade is held — asking the second one keeps that true rather
    /// than assuming it.
    #[test]
    fn a_blade_in_hand_does_not_swing_at_this_players_own_structure() {
        for (name, item_id) in [
            ("the rusty sword", combat::ITEM_RUSTY_SWORD),
            ("the iron sword", ITEM_IRON_SWORD),
        ] {
            let (mut app, sent) = clicking_app(store_with(&[]), blade_of(item_id));
            deliver(&mut app, 1, vec![tent_at(900, [3, 80, 0], LOCAL_ID)]);
            app.update();
            drain(&sent);

            click(&mut app, REMOVE_BUTTON);
            app.update();

            let found = requests(&sent);
            assert_eq!(found.removals, vec![900], "{name}");
            assert_eq!(found.attacks, 0, "{name}: the press also swung at the tent");
        }
    }

    /// Presentation cannot make a request: a dead player asks for nothing, whatever is in
    /// their hand and whatever the crosshair is on. The server refuses either way — this
    /// is the client not firing intent into a refusal.
    #[test]
    fn a_dead_player_neither_plants_nor_removes() {
        use crate::net::{LifeState, PlayerVitals};
        use crate::player::SelfVitals;

        let (mut app, sent) = clicking_app(store_with(&[IVec3::new(4, 81, 0)]), one(ITEM_TENT));
        deliver(&mut app, 1, vec![tent_at(900, [3, 80, 0], LOCAL_ID)]);
        app.update();
        *app.world_mut().resource_mut::<SelfVitals>() = SelfVitals::from_server(PlayerVitals {
            health: 0,
            max_health: 100,
            hunger: 50,
            max_hunger: 100,
            level: 1,
            experience: 0,
            experience_to_next: 50,
            life_state: LifeState::Dead,
            respawn_ticks: 40,
            invulnerable: false,
        });
        app.update();
        drain(&sent);

        click(&mut app, PLACE_BUTTON);
        click(&mut app, REMOVE_BUTTON);
        app.update();

        assert_eq!(requests(&sent), Requests::default());
    }

    /// The rule every per-frame resource in this client follows: `ResMut` marks a resource
    /// changed on every `DerefMut`, and the pick is recomputed every frame whether the
    /// player moved or not.
    #[derive(Resource, Default)]
    struct PickChanges(Vec<bool>);

    fn log_pick_changes(target: Res<StructureTarget>, mut log: ResMut<PickChanges>) {
        log.0.push(target.is_changed());
    }

    #[test]
    fn an_idle_frame_does_not_touch_the_structure_pick() {
        let mut app = aiming_app(store_with(&[]));
        app.init_resource::<PickChanges>()
            .add_systems(Update, log_pick_changes.after(AimStructures));
        deliver(&mut app, 1, vec![tent_at(900, [3, 80, 0], LOCAL_ID)]);
        app.update();
        app.update();
        app.update();

        let seen = &app.world().resource::<PickChanges>().0;
        assert!(
            seen.last() == Some(&false),
            "an idle frame republished the pick: {seen:?}"
        );
    }

    /// Every part a structure mesh is built from survives the merge into it.
    ///
    /// **A presence test, and the footprint test beside it is not one.** That one asks
    /// whether the vertices that exist stay inside the ground the server validated, so a
    /// panel that never arrived passes it perfectly — `merge_all` reports a refused merge
    /// through the log and returns, which is the right call for a cosmetic fault and the
    /// wrong thing to discover by looking at a tent.
    ///
    /// `Mesh::merge` refuses exactly one thing: two meshes whose vertex attributes are not
    /// the same set in the same layout. Bevy's primitives all carry position, normal and UV
    /// today — `Triangle3d` and `Cuboid` included, measured rather than assumed — which is
    /// what makes `merge_all`'s "unreachable" true. This is what keeps it true: a primitive
    /// that ever stops agreeing takes the vertices with it, and the count is what notices.
    ///
    /// The expected counts are derived from the primitives rather than written down, so a
    /// change in how Bevy tessellates a cone moves both sides together.
    #[test]
    fn every_part_of_a_structure_mesh_survives_the_merge() {
        // Dimensions are irrelevant to a vertex count, so these are deliberately not the
        // real ones: what is being counted is how many vertices each *kind* of primitive
        // contributes.
        let cuboid = Mesh::from(Cuboid::new(1.0, 1.0, 1.0)).count_vertices();
        let triangle = Mesh::from(Triangle3d::new(Vec3::ZERO, Vec3::X, Vec3::Y)).count_vertices();
        let cone = Mesh::from(Cone::new(1.0, 1.0)).count_vertices();

        for (what, got, want) in [
            // Two canvas slopes and the gable at the back.
            ("tent", tent_mesh().count_vertices(), 2 * cuboid + triangle),
            // Base, waist, working face, and the horn along the facing.
            ("anvil", anvil_mesh().count_vertices(), 3 * cuboid + cone),
            // Two stacked cones.
            ("flame", flame_mesh().count_vertices(), 2 * cone),
        ] {
            assert_eq!(
                got, want,
                "the {what} mesh has {got} vertices and its parts add up to {want}: \
                 a merge was refused and the part is missing"
            );
        }
    }
}
