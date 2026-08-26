//! What the player is aiming at, and asking the server to change it.
//!
//! ## The client asks; it never applies
//!
//! Holding left produces tick-paced `MineRequest`s; clicking right produces one
//! `BlockEditRequest`. Neither carries an outcome. The voxel changes when the server's
//! `BlockUpdate` arrives — through `world::ChunkStore::apply_block`, which is the only
//! writer there is — or it does not change at all. There is no prediction here to roll
//! back, no pending-edit list to reconcile, and no code path from a mouse button to a
//! voxel.
//!
//! That is not a gap. A predicted edit is a guess that has to be corrected by an
//! authoritative answer, and a refusal may be silence — so prediction here would need
//! a timeout to decide a guess had been wrong, which is a design rather than a detail.
//! The client that never guesses needs none of it.
//!
//! ## The raycast is a grid traversal, not a sampled ray
//!
//! [`raycast`] steps from voxel boundary to voxel boundary (Amanatides & Woo), so it
//! visits **every** voxel the ray passes through, in order, and no others. Marching
//! along the ray at fixed intervals is the tempting alternative and it is wrong in two
//! ways that both read to a player as "the game ignored my click": a step longer than
//! the thinnest geometry walks straight through a wall, and at a grazing angle the
//! sample that lands inside a voxel is often not the first voxel the ray entered — so
//! the highlight sits one block off, and the block that breaks is not the one that was
//! lit up.
//!
//! ## The one edge from `player` to `world`
//!
//! Aiming is a question about voxels, so this module reads `world::ChunkStore`. It is
//! this module's only edge into the world module, and it is a read: the store is asked
//! what is solid, and told nothing.

use std::time::{Duration, Instant};

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use super::camera::{AimCamera, WorldCamera};
use super::combat;
use super::constants::MAX_REACH;
use super::interpolate::SnapshotBuffer;
use super::inventory::{ApplyInventory, Inventory, SelectedSlot};
use super::mobs;
use super::{InputCadence, InputGate, SelfVitals, ViewMode, set_if_changed, tick_interval};
use crate::net::{
    BlockCoord, BlockEditRequest, EditAction, MineProgress, MineProgressInbox, MineRequest,
    Outbound, Sent, Session, encode_block_edit_request, encode_mine_request,
};
use crate::world::ChunkStore;

/// The control that breaks the targeted block.
const BREAK_BUTTON: MouseButton = MouseButton::Left;

/// The control that places a block against the targeted face.
const PLACE_BUTTON: MouseButton = MouseButton::Right;

/// The colour of the outline, as linear RGB.
///
/// Chosen to sit outside `world/palette.rs` entirely — nothing in the terrain is warm —
/// so the frame reads as an overlay rather than as a block of some type the player has
/// not seen before.
const HIGHLIGHT_COLOUR: Color = Color::linear_rgb(1.0, 0.72, 0.25);

/// The fully-mined end of the outline tint, also in linear RGB.
const HIGHLIGHT_COMPLETE: [f32; 3] = [0.35, 0.85, 1.0];

/// How many missing server reports the presentation tolerates before it clears.
///
/// This is only a liveness bound for stale pixels. It never advances progress and
/// contains no hardness: the displayed fraction remains exactly the last byte the
/// server sent for all of these ticks.
const PROGRESS_SILENCE_TICKS: u32 = 8;

/// Cross-section of one edge bar of the outline, in blocks.
const HIGHLIGHT_THICKNESS: f32 = 0.03;

/// How far the outline stands off the block it marks, in blocks.
///
/// Small and non-zero. Zero would put the frame exactly coplanar with the terrain's
/// own faces, where the depth test decides per pixel which of two equal depths wins
/// and the outline flickers along its length.
const HIGHLIGHT_BLEED: f32 = 0.004;

/// The most voxels one [`raycast`] will visit.
///
/// Termination does not rest on it: a normalised direction has no component greater
/// than one, so every step advances the ray by at least a block and the reach bound is
/// what ends the walk — 4.5 blocks of reach is at most a couple of dozen steps. The cap
/// is here so that a `reach` somebody passes carelessly cannot turn an aiming query
/// into a frame that never ends. Reaching it answers "nothing targeted", which is the
/// safe answer.
const MAX_STEPS: usize = 256;

/// Aims at a voxel, draws the outline, and asks the server to change it.
pub struct BlockTargetPlugin;

/// Orders UI consumers after the newest authoritative progress has been applied.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ApplyMiningFeedback;

/// Orders the view-model animation after this frame's target and buttons were read.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct ApplyTargetInput;

/// Orders the aiming outline, so the footprint ghost can be computed before it.
///
/// One press cannot mean both a voxel and a building, so exactly one of the two overlays
/// is drawn — and the highlight is the one that stands down. It reads
/// [`super::structures::FootprintPreview`] to know, and a preview computed afterwards
/// would leave both on screen for a frame every time a structure is selected.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct DrawTargetHighlight;

/// Orders anything that needs this frame's voxel after the raycast produced it.
///
/// [`super::structures`] is the reason it is named: its pick compares against the block
/// this set resolves, and a pick that ran first would compare against last frame's — which
/// is a structure that captures a press while the player is already looking past it.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct AimBlocks;

impl Plugin for BlockTargetPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BlockTarget>()
            .init_resource::<HealTargetHint>()
            .init_resource::<MiningInput>()
            // `PlayerPlugin` owns this in the game. Initialising it here too keeps this
            // module's headless contract complete when it is built on its own.
            .init_resource::<SelfVitals>()
            .init_resource::<ViewMode>()
            .init_resource::<MiningFeedback>()
            .init_resource::<MineProgressInbox>()
            .add_systems(Startup, spawn_highlight)
            .add_systems(
                Update,
                // Chained: the outline and the request both read the target this
                // frame's raycast produced, not last frame's. And after the camera is
                // aimed, because the ray starts at the camera — a ray cast before it
                // moved would target what the player was looking at a frame ago,
                // which is a highlight that lags the crosshair.
                (
                    aim_at_a_block.in_set(AimBlocks),
                    aim_at_a_healing_target,
                    update_mining_feedback.in_set(ApplyMiningFeedback),
                    (
                        move_the_highlight.in_set(DrawTargetHighlight),
                        send_block_edits.in_set(ApplyTargetInput),
                    ),
                )
                    .chain()
                    .after(AimCamera)
                    .after(ApplyInventory)
                    // Redundant today and kept anyway: `AimCamera` is itself ordered
                    // after `ApplySnapshots`, so these already ran after the vitals were
                    // published. That is an ordering this module needs, routed through a
                    // constraint that exists for the camera's benefit — the day the
                    // raycast stops depending on where the camera is, `.after(AimCamera)`
                    // goes and the death gate silently starts reading last frame's answer.
                    // One line to say what this module actually requires.
                    .after(super::ApplySnapshots)
                    .after(super::send_player_input)
                    .after(crate::world::ingest_world_updates),
            );
    }
}

/// Whether the sceptre's presentation ray meets another player before any mob.
///
/// This is a drawing hint only. It is derived from interpolated snapshots and never
/// gates or alters the attack request the server judges.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HealTargetHint(pub bool);

/// One voxel the ray hit, and the face it entered through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockHit {
    /// The solid voxel, in world block coordinates.
    pub block: IVec3,
    /// The outward unit axis of the face the ray came in through — so `block + face`
    /// is the empty voxel in front of it.
    ///
    /// Zero in exactly one case: the ray started *inside* a solid voxel, so there is
    /// no face it was entered through. See [`Self::place_target`].
    pub face: IVec3,
}

impl BlockHit {
    /// The voxel a placed block would occupy: the empty one on the near side of the
    /// face that was hit.
    ///
    /// `None` when the eye is inside a solid voxel. There is no face to place against
    /// then, and the only candidate — the voxel itself — is the one already occupied,
    /// so the honest answer is that there is nowhere to put a block rather than a
    /// request the server would refuse.
    pub fn place_target(&self) -> Option<IVec3> {
        (self.face != IVec3::ZERO).then(|| self.block + self.face)
    }
}

/// The voxel the player is currently aiming at, if any.
///
/// A resource rather than a component, because it is one fact about the session and
/// three systems read it. Written through [`set_if_changed`], which matters more here
/// than almost anywhere: this is recomputed every frame, and `ResMut` marks a resource
/// changed on every `DerefMut` — so an unconditional write would keep the outline's
/// transform and every other consumer permanently "changed" on a player standing
/// still.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BlockTarget(pub Option<BlockHit>);

/// The last authoritative progress still valid for the block under the crosshair.
///
/// The timestamp is presentation liveness only. [`Self::progress`] never changes
/// between server reports: silence holds the byte as-is, then removes it.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MiningFeedback {
    report: Option<MineProgress>,
    received_at: Duration,
}

impl MiningFeedback {
    /// The exact server byte, or zero when no feedback is currently valid.
    pub fn progress(&self) -> u8 {
        self.report.map_or(0, |report| report.progress)
    }

    fn fraction(&self) -> f32 {
        f32::from(self.progress()) / f32::from(u8::MAX)
    }

    #[cfg(test)]
    pub(crate) fn for_test(progress: u8) -> Self {
        Self {
            report: (progress != 0).then_some(MineProgress {
                pos: BlockCoord { x: 0, y: 0, z: 0 },
                progress,
            }),
            received_at: Duration::ZERO,
        }
    }
}

/// What the crosshair resolves to this frame: a voxel, and whether one of this player's
/// own structures is standing in front of it.
///
/// A bundle rather than two parameters, for the reason [`combat::HeldItem`] is one:
/// [`send_block_edits`] was already at the argument bound, and *what does this frame's
/// press act on* is one question that should have one place to be asked.
#[derive(SystemParam)]
struct Aim<'w> {
    block: Res<'w, BlockTarget>,
    structure: Res<'w, super::structures::StructureTarget>,
}

impl Aim<'_> {
    /// The voxel under the crosshair, whatever else is in front of it.
    fn hit(&self) -> Option<BlockHit> {
        self.block.0
    }

    /// Whether one of this player's own structures has taken the mine/attack press.
    ///
    /// It can only ever be their **own**: someone else's is not a candidate for the pick
    /// at all, so another player's camp cannot swallow a click that was meant for the wall
    /// behind it.
    fn structure_captures_the_press(&self) -> bool {
        self.structure.0.is_some()
    }
}

/// Local held-button state. It remembers enough to cancel exactly one old target
/// and enough cadence state to emit `active=true` no faster than player input.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
struct MiningInput {
    target: Option<IVec3>,
    observed_tick: u32,
}

/// Marks the outline entity, so a query finds it without also matching the bodies.
#[derive(Component)]
struct TargetHighlight;

// ---------------------------------------------------------------------------
// The raycast
// ---------------------------------------------------------------------------

/// The first solid voxel a ray enters within `reach`, and the face it entered through.
///
/// An exact grid traversal — Amanatides & Woo. The state is one voxel index, the sign
/// of the step on each axis, and for each axis the ray parameter at which the *next*
/// boundary on it is crossed; each iteration advances along whichever axis crosses
/// soonest. That is what makes the sequence of visited voxels exactly the sequence the
/// ray passes through: no voxel is skipped however thin, and none is visited out of
/// order however shallow the angle.
///
/// `reach` is measured along the ray, and a voxel counts as reached when the ray
/// *enters* it within the limit. `solid` is asked about each voxel in turn, so it is
/// also the seam a test uses to record the traversal itself.
///
/// A zero, `NaN` or infinite direction has no voxel sequence and answers `None` rather
/// than a plausible-looking hit. The direction does not have to be normalised — it is
/// normalised here, which is what makes `reach` a distance in blocks.
pub fn raycast(
    origin: Vec3,
    direction: Vec3,
    reach: f32,
    mut solid: impl FnMut(IVec3) -> bool,
) -> Option<BlockHit> {
    if !origin.is_finite() || !direction.is_finite() || !reach.is_finite() || reach < 0.0 {
        return None;
    }
    let length = direction.length();
    if length <= 0.0 {
        return None;
    }
    let direction = direction / length;

    // `floor`, never a cast: `-0.5 as i32` truncates to 0, and the voxel containing
    // -0.5 is -1. Half the world is on that side of the origin.
    let mut voxel = origin.floor().as_ivec3();

    let mut step = [0i32; 3];
    let mut next = [f32::INFINITY; 3];
    let mut stride = [f32::INFINITY; 3];
    for axis in 0..3 {
        let component = direction[axis];
        if component == 0.0 {
            // The ray never crosses a boundary on this axis, so it never steps on it.
            // Infinity is the honest parameter for "not in this direction", and it
            // simply never wins the comparison below.
            continue;
        }
        step[axis] = if component > 0.0 { 1 } else { -1 };
        // How far along the ray one whole block on this axis is.
        stride[axis] = 1.0 / component.abs();
        // The boundary the ray leaves this voxel through on this axis, and the ray
        // parameter at which it gets there. Added in `f32`, not in `i32`: a position far
        // enough out that `floor` saturates would make `voxel + 1` an overflow, and this
        // is arithmetic on a number the server chose.
        let boundary = voxel[axis] as f32 + if component > 0.0 { 1.0 } else { 0.0 };
        next[axis] = (boundary - origin[axis]) / component;
    }

    // The face the *current* voxel was entered through. Zero for the voxel the ray
    // starts in, which it did not enter at all.
    let mut face = IVec3::ZERO;

    for _ in 0..MAX_STEPS {
        if solid(voxel) {
            return Some(BlockHit { block: voxel, face });
        }

        // The axis whose boundary comes soonest. Ties are broken towards the lower
        // axis, which is a choice with no wrong answer: the ray passes exactly through
        // an edge, and either voxel it could step into is one it genuinely touches.
        let mut axis = 0;
        for candidate in 1..3 {
            if next[candidate] < next[axis] {
                axis = candidate;
            }
        }
        // Past the reach before entering the next voxel, so that voxel is not aimed at
        // — the boundary crossing *is* the entry, which is what makes the reach test
        // exact rather than approximately one block wide.
        if next[axis] > reach {
            return None;
        }

        // Checked, because the world ends where `i32` does. A position far enough out
        // that `floor` saturated would otherwise step past `i32::MAX` — an overflow on a
        // number the server chose, which is a panic in a debug build and a wrap in a
        // release one. There is nothing beyond that voxel to aim at either way.
        voxel[axis] = voxel[axis].checked_add(step[axis])?;
        // Entered through the face opposite the direction of travel.
        face = IVec3::ZERO;
        face[axis] = -step[axis];
        next[axis] += stride[axis];
    }

    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AimBodyKind {
    OtherPlayer,
    Mob,
}

#[derive(Debug, Clone, Copy)]
struct AimBody {
    kind: AimBodyKind,
    feet: Vec3,
    width: f32,
    height: f32,
}

/// Returns true exactly when the nearest intersected body is another player.
fn first_body_is_player(origin: Vec3, direction: Vec3, bodies: &[AimBody]) -> bool {
    if !origin.is_finite() || !direction.is_finite() || direction.length_squared() == 0.0 {
        return false;
    }
    let direction = direction.normalize();
    bodies
        .iter()
        .filter_map(|body| {
            let half = body.width / 2.0;
            let min = body.feet + Vec3::new(-half, 0.0, -half);
            let max = body.feet + Vec3::new(half, body.height, half);
            ray_box_distance(origin, direction, min, max).map(|distance| (distance, body.kind))
        })
        .min_by(|left, right| left.0.total_cmp(&right.0))
        .is_some_and(|(_, kind)| kind == AimBodyKind::OtherPlayer)
}

/// Slab intersection distance for an already-normalised ray.
fn ray_box_distance(origin: Vec3, direction: Vec3, min: Vec3, max: Vec3) -> Option<f32> {
    let mut near: f32 = 0.0;
    let mut far = f32::INFINITY;
    for axis in 0..3 {
        if direction[axis] == 0.0 {
            if origin[axis] < min[axis] || origin[axis] > max[axis] {
                return None;
            }
            continue;
        }
        let mut a = (min[axis] - origin[axis]) / direction[axis];
        let mut b = (max[axis] - origin[axis]) / direction[axis];
        if a > b {
            std::mem::swap(&mut a, &mut b);
        }
        near = near.max(a);
        far = far.min(b);
        if near > far {
            return None;
        }
    }
    (far >= 0.0).then_some(near.max(0.0))
}

/// Recomputes the sceptre crosshair hint from the same interpolated snapshot drawn now.
fn aim_at_a_healing_target(
    session: Option<Res<Session>>,
    buffer: Option<Res<SnapshotBuffer>>,
    inventory: Res<Inventory>,
    selected: Res<SelectedSlot>,
    cameras: Query<&Transform, With<WorldCamera>>,
    mut hint: ResMut<HealTargetHint>,
) {
    let next = (|| {
        let session = session.as_deref()?;
        if combat::attack_item_in_hand(&inventory, &selected)
            != Some(super::crafting::ITEM_WOODEN_SCEPTRE)
        {
            return None;
        }
        let buffer = buffer.as_deref()?;
        let eye = cameras.iter().next()?;
        let interval = tick_interval(session.0.tick_rate);
        let now = Instant::now();
        let mut bodies = Vec::new();
        bodies.extend(
            buffer
                .sample(now, interval)
                .into_iter()
                .filter(|(id, _)| *id != session.0.entity_id)
                .map(|(_, player)| AimBody {
                    kind: AimBodyKind::OtherPlayer,
                    feet: player.pos,
                    width: super::constants::PLAYER_WIDTH,
                    height: super::constants::PLAYER_HEIGHT,
                }),
        );
        bodies.extend(
            buffer
                .sample_mobs(now, interval)
                .into_iter()
                .map(|(_, mob)| {
                    let body = mobs::body(mob.kind);
                    AimBody {
                        kind: AimBodyKind::Mob,
                        feet: mob.pos,
                        width: body.width,
                        height: body.height,
                    }
                }),
        );
        Some(first_body_is_player(
            eye.translation,
            *eye.forward(),
            &bodies,
        ))
    })()
    .unwrap_or(false);
    set_if_changed(&mut hint, HealTargetHint(next));
}

// ---------------------------------------------------------------------------
// Aiming
// ---------------------------------------------------------------------------

/// Recomputes what the player is aiming at.
///
/// Every input is optional, because this module has to work in an app that has no
/// session, no streamed world and no camera — which is every frame before the
/// handshake completes, and every one of this module's own tests. Nothing to aim from
/// or at is "no target", never a panic.
fn aim_at_a_block(
    gate: InputGate<'_>,
    session: Option<Res<Session>>,
    store: Option<Res<ChunkStore>>,
    cameras: Query<&Transform, With<WorldCamera>>,
    mut target: ResMut<BlockTarget>,
) {
    // A player the server says is dead aims at nothing, so nothing is outlined and the
    // request below has no voxel to name. Presentation, not authority: the server refuses
    // a dead player's edit whether or not this crate drew a box round the block first.
    let aimed = match (gate.may_aim(), session, store, cameras.iter().next()) {
        (true, Some(session), Some(store), Some(eye)) => {
            let size = usize::from(session.0.chunk_size);
            raycast(eye.translation, *eye.forward(), MAX_REACH, |voxel| {
                store.solid_at(
                    BlockCoord {
                        x: voxel.x,
                        y: voxel.y,
                        z: voxel.z,
                    },
                    size,
                )
            })
        }
        _ => None,
    };

    set_if_changed(&mut target, BlockTarget(aimed));
}

// ---------------------------------------------------------------------------
// The outline
// ---------------------------------------------------------------------------

fn spawn_highlight(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((
        TargetHighlight,
        Mesh3d(meshes.add(edge_frame(HIGHLIGHT_BLEED))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: HIGHLIGHT_COLOUR,
            // Unlit on purpose: the outline is a piece of interface, and one that
            // faded on the shaded side of a hill would be least legible exactly where
            // the terrain is hardest to read.
            unlit: true,
            ..default()
        })),
        // The block's minimum corner. A voxel is one world unit — the mesher works in
        // blocks and `chunk_origin` multiplies chunk coordinates by `chunk_size` — so a
        // world block coordinate is already a world position and needs no scaling.
        Transform::default(),
        // Nothing has been aimed at before the first frame's raycast.
        Visibility::Hidden,
    ));
}

/// Moves the outline onto the targeted block, or hides it.
///
/// Guarded on the resource's change flag rather than run unconditionally: writing a
/// `Transform` marks the component changed, and transform propagation is downstream of
/// that flag. A player standing still would otherwise repropagate the outline every
/// frame for the rest of the session.
fn move_the_highlight(
    target: Res<BlockTarget>,
    feedback: Res<MiningFeedback>,
    // Optional so this module still stands on its own: its own tests build
    // `BlockTargetPlugin` without `StructuresPlugin`, and a plugin that panicked without
    // its sibling would be untestable exactly where the aiming rules live. Absent means
    // no structure is ever in hand, which is what a build with no structures is.
    preview: Option<Res<super::structures::FootprintPreview>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut outlines: Query<
        (
            &mut Transform,
            &mut Visibility,
            &MeshMaterial3d<StandardMaterial>,
        ),
        With<TargetHighlight>,
    >,
) {
    let ghosting = preview.as_ref().is_some_and(|preview| preview.active());
    let stale = target.is_changed()
        || feedback.is_changed()
        || preview.is_some_and(|preview| preview.is_changed());
    if !stale {
        return;
    }

    for (mut transform, mut visibility, material) in &mut outlines {
        match target.0 {
            // A structure in hand means the footprint ghost is showing where the whole of
            // it would stand, and one cell of that is where this frame would be. Drawing
            // both would say the anchor is special, which it is not: it is one of the
            // cells the server checks, and the tent's nine are all the same question.
            Some(_) if ghosting => *visibility = Visibility::Hidden,
            Some(hit) => {
                transform.translation = hit.block.as_vec3();
                *visibility = Visibility::Visible;
            }
            None => *visibility = Visibility::Hidden,
        }
        if let Some(mut material) = materials.get_mut(&material.0) {
            material.base_color = highlight_colour(feedback.fraction());
        }
    }
}

fn highlight_colour(progress: f32) -> Color {
    let progress = progress.clamp(0.0, 1.0);
    let base = [1.0, 0.72, 0.25];
    Color::linear_rgb(
        base[0] + (HIGHLIGHT_COMPLETE[0] - base[0]) * progress,
        base[1] + (HIGHLIGHT_COMPLETE[1] - base[1]) * progress,
        base[2] + (HIGHLIGHT_COMPLETE[2] - base[2]) * progress,
    )
}

/// Accepts only the server's progress, holds it unchanged during brief silence,
/// and clears it when it no longer describes the block under the crosshair.
fn update_mining_feedback(
    time: Res<Time>,
    session: Option<Res<Session>>,
    target: Res<BlockTarget>,
    mut inbox: ResMut<MineProgressInbox>,
    mut feedback: ResMut<MiningFeedback>,
) {
    let now = time.elapsed();
    if let Some(report) = inbox.take().into_iter().last() {
        if report.progress == 0 {
            set_if_changed(&mut feedback, MiningFeedback::default());
            return;
        }
        set_if_changed(
            &mut feedback,
            MiningFeedback {
                report: Some(report),
                received_at: now,
            },
        );
    }

    let Some(report) = feedback.report else {
        return;
    };
    let still_targeted = target
        .0
        .is_some_and(|hit| hit.block == IVec3::new(report.pos.x, report.pos.y, report.pos.z));
    let still_fresh = session.is_some_and(|session| {
        now.saturating_sub(feedback.received_at)
            < tick_interval(session.0.tick_rate) * PROGRESS_SILENCE_TICKS
    });
    if !still_targeted || !still_fresh {
        set_if_changed(&mut feedback, MiningFeedback::default());
    }
}

/// The outline of one voxel, standing off it by `bleed`, in block-local space.
///
/// The one shape both overlays in this crate are drawn from: this module's aiming outline
/// and the footprint ghost in [`super::structures`]. Shared rather than mirrored, because
/// a second frame authored beside this one would be a second answer to "what does an
/// outlined cell look like" — and the winding and normals of a box are the part that is
/// invisible until something renders inside out.
pub(super) fn cell_outline_mesh(bleed: f32) -> Mesh {
    edge_frame(bleed)
}

/// The twelve edges of one block, as a single triangle mesh in block-local space.
///
/// A frame of bars rather than a line-list wireframe, and rather than a translucent
/// cube. Lines would need the material's face culling turned off — wgpu rejects a cull
/// mode on a non-triangle topology — and would be one pixel wide at any distance; a
/// filled overlay would tint the block and so change the colour the palette uses to
/// say what the block *is*. Bars go through the same triangle pipeline the terrain
/// already uses, and the block they mark stays exactly the colour it was.
///
/// Twelve `Cuboid`s merged rather than hand-written buffers: the winding and normals of
/// a box are Bevy's problem, and getting them wrong is invisible until something is
/// rendered inside out.
fn edge_frame(bleed: f32) -> Mesh {
    let bars = edge_bars(bleed);
    let mut frame = bar(bars[0]);

    for edge in &bars[1..] {
        if let Err(err) = frame.merge(&bar(*edge)) {
            // Unreachable: every bar is a `Cuboid`, so all twelve carry the same
            // attributes in the same layout, which is the only thing `merge` refuses.
            // Reported rather than unwrapped — an outline missing an edge is a cosmetic
            // fault, and taking the window down over one would not be.
            error!("the target outline is missing an edge: {err}");
        }
    }

    frame
}

/// One edge bar, as its own mesh.
fn bar((centre, size): (Vec3, Vec3)) -> Mesh {
    Mesh::from(Cuboid::new(size.x, size.y, size.z)).translated_by(centre)
}

/// Where each of the twelve bars sits and how big it is, in block-local space.
///
/// The block spans 0..1 on each axis and the frame is grown past that by `bleed`, so each
/// bar runs the full inflated length along one axis and is [`HIGHLIGHT_THICKNESS`] across
/// on the other two, centred on one of the four edges parallel to it.
///
/// The bleed is an argument rather than [`HIGHLIGHT_BLEED`] because two overlays are drawn
/// from this shape now — this module's aiming outline and the footprint ghost in
/// [`super::structures`] — and they never coincide on a cell, so they need not stand off it
/// by the same amount.
fn edge_bars(bleed: f32) -> [(Vec3, Vec3); 12] {
    let low = -bleed;
    let high = 1.0 + bleed;

    let mut bars = [(Vec3::ZERO, Vec3::ZERO); 12];
    let mut placed = 0;
    for axis in 0..3 {
        let across = [(axis + 1) % 3, (axis + 2) % 3];
        for corner in [[low, low], [high, low], [low, high], [high, high]] {
            let mut centre = [0.0f32; 3];
            let mut size = [HIGHLIGHT_THICKNESS; 3];

            centre[axis] = 0.5;
            size[axis] = high - low;
            for (offset, value) in across.iter().zip(corner) {
                centre[*offset] = value;
            }

            bars[placed] = (Vec3::from_array(centre), Vec3::from_array(size));
            placed += 1;
        }
    }

    bars
}

// ---------------------------------------------------------------------------
// Asking
// ---------------------------------------------------------------------------

/// Sends held mining intent and one placement request per right click.
///
/// **No legality check happens here, deliberately.** Whether the target is breakable,
/// whether the mine target is breakable, whether the place target is really empty,
/// whether the player is close enough by the server's own reckoning — every one of
/// those is the server's answer, and a client that pre-judged them would be doing the
/// server's job while still having to accept being overruled. Intent goes out and the
/// world changes if the server says so.
fn send_block_edits(
    buttons: Option<Res<ButtonInput<MouseButton>>>,
    gate: InputGate<'_>,
    aim: Aim<'_>,
    cadence: Res<InputCadence>,
    held: combat::HeldItem<'_>,
    outbound: Option<ResMut<Outbound>>,
    mut mining: ResMut<MiningInput>,
) {
    let mut outbound = outbound;
    let tick_advanced = mining.observed_tick != cadence.client_tick;
    if tick_advanced {
        mining.observed_tick = cadence.client_tick;
    }

    // A mode transition belongs to the UI for the whole frame. Leaving play still
    // cancels the old target below, while entering play cannot inherit its key event.
    //
    // Dying reads the same way, and the cancel that falls out of it is the point: the
    // held mining target becomes `None`, so one `active = false` request goes out for the
    // voxel that was being mined instead of the intent simply going quiet.
    let playing = gate.may_act();
    // A blade in hand means the left button is a swing, and `super::combat` owns it. One
    // predicate rather than two conditions that happen to agree: a click must never send
    // both a mining frame and an attack, and reading the same function is what makes that
    // structural instead of a coincidence to keep in step.
    //
    // One of this player's own structures standing in front of the voxel takes the same
    // button for the same reason, and `super::structures` owns that one: a press cannot
    // both start digging a wall and ask for the tent pitched against it.
    let swinging = held.attack_item().is_some() || aim.structure_captures_the_press();
    let desired = buttons
        .as_deref()
        .filter(|buttons| playing && !swinging && buttons.pressed(BREAK_BUTTON))
        .and_then(|_| aim.hit().map(|hit| hit.block));

    if mining.target != desired {
        if let Some(old) = mining.target {
            send_mining(&mut outbound, old, false, held.slot(), cadence.client_tick);
        }
        mining.target = desired;
    }
    if tick_advanced && let Some(pos) = mining.target {
        send_mining(&mut outbound, pos, true, held.slot(), cadence.client_tick);
    }

    // Right stays a one-shot block edit. It remains a request only: neither the
    // inventory nor the voxel store changes until the server answers.
    let Some(buttons) = buttons else {
        return;
    };
    let Some(hit) = aim.hit() else {
        return;
    };
    if !playing || !buttons.just_pressed(PLACE_BUTTON) {
        return;
    }
    // A structure in hand means the same press plants a camp, and `super::structures`
    // owns it. The same one-predicate rule the left button follows above: a click must
    // never ask for a voxel and a building at once.
    if held.structure().is_some() {
        return;
    }
    let Some(pos) = hit.place_target() else {
        return;
    };
    let Some(outbound) = outbound.as_deref_mut() else {
        return;
    };
    let action = EditAction::Place;
    let frame = encode_block_edit_request(&BlockEditRequest {
        pos: BlockCoord {
            x: pos.x,
            y: pos.y,
            z: pos.z,
        },
        action,
        slot: held.slot(),
        // The client's own counter, shared with `PlayerInput` rather than a second
        // one of this module's own: the contract asks for "the client's tick
        // counter", and two counters could not be ordered against each other at
        // all — which is the only thing the server is allowed to use it for.
        client_tick: cadence.client_tick,
    });

    match outbound.send(frame) {
        Sent::Queued => {}
        Sent::Dropped => {
            warn!("the outbound queue was full; a {action:?} at {pos:?} never reached the server")
        }
        Sent::Closed => {}
    }
}

/// Sends one mining control frame.
///
/// **The slot is which, never what.** The server reads its own inventory for the item and
/// its own table for what that item is worth against the block — a client that named a tool
/// would be naming its own mining speed. It is the same `held.slot()` the block edit beside
/// it sends, and mining was the one action on this wire that named no slot until #185.
fn send_mining(
    outbound: &mut Option<ResMut<'_, Outbound>>,
    pos: IVec3,
    active: bool,
    slot: u8,
    client_tick: u32,
) {
    let Some(outbound) = outbound.as_deref_mut() else {
        return;
    };
    let request = MineRequest {
        pos: BlockCoord {
            x: pos.x,
            y: pos.y,
            z: pos.z,
        },
        active,
        client_tick,
        slot,
    };
    match outbound.send(encode_mine_request(&request)) {
        Sent::Queued => {}
        Sent::Dropped => warn!(
            "the outbound queue was full; mining intent active={active} at {pos:?} never reached the server"
        ),
        Sent::Closed => {}
    }
}

#[cfg(test)]
mod tests {
    //! Tests for block targeting.
    //!
    //! No display, no GPU and no window, the same rule the rest of the client's tests
    //! follow. The raycast is a plain function over a closure, so most of what matters here
    //! needs no app at all; the systems are driven on `MinimalPlugins` + `AssetPlugin`, where
    //! `Assets<T>` is an ordinary resource and everything short of the GPU upload exists.
    //!
    //! The closure the raycast asks about voxels is also the seam these tests use to watch
    //! the traversal itself, which is what makes "it steps through the grid" an assertion
    //! rather than a claim in a comment.

    use std::f32::consts::FRAC_PI_2;
    use std::sync::mpsc::Receiver;
    use std::time::{Duration, Instant};

    use bevy::asset::AssetPlugin;
    use bevy::input::mouse::MouseButtonInput;
    use bevy::input::{ButtonState, InputPlugin};
    use bevy::time::TimeUpdateStrategy;

    use super::*;
    use crate::net::{
        ChunkCoord, EntityState, InventoryInbox, InventoryStack, InventoryState, SessionParams,
        Snapshot, SnapshotInbox, WorldInbox, WorldUpdate,
    };
    use crate::player::crafting::ITEM_IRON_SWORD;
    use crate::player::{InputMode, Inventory, LookState, PlayerPlugin};
    use crate::wire::voxelheim::net as fb;
    use crate::world::{VoxelChunk, WorldPlugin, palette};

    /// The chunk edge the server sends, and the one every world coordinate below is written
    /// for.
    const SIZE: u16 = 32;

    /// This session's own entity, as `ServerWelcome` names it.
    const LOCAL_ID: u64 = 7;

    /// Where the server puts the player in these tests. The camera therefore sits at
    /// `80 + EYE_HEIGHT`, which is inside the voxel at world y 81.
    const SPAWN: [f32; 3] = [0.5, 80.0, 0.5];

    /// The voxel the camera's eye is inside, given [`SPAWN`].
    const EYE_VOXEL: IVec3 = IVec3::new(0, 81, 0);

    // ---------------------------------------------------------------------------
    // The raycast
    // ---------------------------------------------------------------------------

    /// A world in which exactly these voxels are solid.
    fn only(solid: &[IVec3]) -> impl Fn(IVec3) -> bool + '_ {
        move |voxel| solid.contains(&voxel)
    }

    /// The centre of a voxel, which is where a ray starts when a test wants no boundary
    /// arithmetic in the way.
    fn middle_of(voxel: IVec3) -> Vec3 {
        voxel.as_vec3() + Vec3::splat(0.5)
    }

    #[test]
    fn healing_aim_is_green_only_when_the_first_body_is_another_player() {
        let eye = Vec3::new(0.0, 1.0, 0.0);
        let player = AimBody {
            kind: AimBodyKind::OtherPlayer,
            feet: Vec3::new(0.0, 0.0, -5.0),
            width: super::super::constants::PLAYER_WIDTH,
            height: super::super::constants::PLAYER_HEIGHT,
        };
        let mob = AimBody {
            kind: AimBodyKind::Mob,
            feet: Vec3::new(0.0, 0.0, -3.0),
            width: 0.9,
            height: 1.4,
        };

        assert!(first_body_is_player(eye, Vec3::NEG_Z, &[player]));
        assert!(
            !first_body_is_player(eye, Vec3::NEG_Z, &[player, mob]),
            "a nearer mob must keep the default crosshair"
        );
        assert!(!first_body_is_player(eye, Vec3::NEG_Z, &[]));
    }

    #[test]
    fn a_block_straight_ahead_is_hit_on_the_face_the_ray_came_in_through() {
        let target = IVec3::new(0, 0, -3);
        let hit = raycast(
            middle_of(IVec3::ZERO),
            Vec3::NEG_Z,
            MAX_REACH,
            only(&[target]),
        );

        assert_eq!(
            hit,
            Some(BlockHit {
                block: target,
                // Approached from +Z, so that is the face standing between the player and
                // the block — and the side a placed block would go on.
                face: IVec3::Z,
            })
        );
    }

    #[test]
    fn a_block_behind_the_player_is_not_hit() {
        // The same block, the opposite direction. A raycast that took the absolute value of
        // anything would pass this test the wrong way round.
        let target = IVec3::new(0, 0, -3);

        assert_eq!(
            raycast(middle_of(IVec3::ZERO), Vec3::Z, MAX_REACH, only(&[target])),
            None
        );
    }

    #[test]
    fn nothing_within_reach_is_no_target() {
        assert_eq!(
            raycast(middle_of(IVec3::ZERO), Vec3::NEG_Z, MAX_REACH, only(&[])),
            None
        );
    }

    #[test]
    fn a_voxel_one_step_beyond_the_reach_is_not_targeted() {
        // The reach is measured to where the ray *enters* a voxel, so the boundary is exact
        // rather than a block wide. From the middle of voxel 0 along +x, voxel n is entered
        // at n - 0.5: voxel 5 at 4.5, voxel 6 at 5.5.
        let origin = middle_of(IVec3::ZERO);
        let near = IVec3::new(5, 0, 0);
        let far = IVec3::new(6, 0, 0);

        assert_eq!(
            raycast(origin, Vec3::X, MAX_REACH, only(&[near])).map(|hit| hit.block),
            Some(near),
            "a voxel entered exactly at the limit is within it"
        );
        assert_eq!(
            raycast(origin, Vec3::X, MAX_REACH, only(&[far])),
            None,
            "one voxel further is a whole block past the limit"
        );
        assert_eq!(
            raycast(origin, Vec3::X, 4.4, only(&[near])),
            None,
            "and the near voxel goes out of reach as soon as the limit is short of its face"
        );
    }

    #[test]
    fn the_traversal_visits_exactly_the_voxels_the_ray_passes_through() {
        // The property that separates a grid traversal from a sampled one, asserted
        // directly: the closure is asked about every voxel the ray crosses, in order, and
        // about no others.
        //
        // The ray leaves the middle of a column at a 45-degree angle in x/y from an origin
        // deliberately off-centre in y, so no two boundary crossings coincide and the
        // expected sequence has no ties in it.
        let mut visited = Vec::new();
        let hit = raycast(
            Vec3::new(0.5, 0.25, 0.5),
            Vec3::new(1.0, 1.0, 0.0),
            5.0,
            |voxel| {
                visited.push(voxel);
                false
            },
        );

        assert_eq!(hit, None, "nothing in that world is solid");
        assert_eq!(
            visited,
            vec![
                IVec3::new(0, 0, 0),
                IVec3::new(1, 0, 0),
                IVec3::new(1, 1, 0),
                IVec3::new(2, 1, 0),
                IVec3::new(2, 2, 0),
                IVec3::new(3, 2, 0),
                IVec3::new(3, 3, 0),
                IVec3::new(4, 3, 0),
            ],
            "the staircase the grid describes, one crossing at a time"
        );

        // Stated structurally as well, because the list above would still be satisfiable by
        // a lucky sampler on this one ray. Every step is one block on exactly one axis —
        // a sampler skips voxels, which shows up as a step of two or a diagonal.
        for pair in visited.windows(2) {
            let delta = pair[1] - pair[0];
            let travelled = delta.x.abs() + delta.y.abs() + delta.z.abs();
            assert_eq!(
                travelled, 1,
                "{:?} to {:?} is not a single step through one face",
                pair[0], pair[1]
            );
        }

        // And no voxel is asked about twice, which a sampler with a step shorter than a
        // block does constantly.
        let mut once = visited.clone();
        once.sort_unstable_by_key(|voxel| (voxel.x, voxel.y, voxel.z));
        once.dedup();
        assert_eq!(once.len(), visited.len(), "a voxel was visited twice");
    }

    #[test]
    fn a_grazing_ray_reports_the_face_the_grid_says_it_entered_through() {
        // Two voxels the same ray passes through, one after the other, entered through
        // faces on *different* axes — (2,2,0) from below and (3,2,0) from the side. Which
        // one a click places against is the difference between building on top of a wall
        // and building beside it, and a point sample inside a voxel cannot tell them apart
        // at all: it knows where it landed, not how it got in.
        let ray = |solid: IVec3| {
            raycast(
                Vec3::new(0.5, 0.25, 0.5),
                Vec3::new(1.0, 1.0, 0.0),
                5.0,
                only(&[solid]),
            )
        };

        assert_eq!(
            ray(IVec3::new(2, 2, 0)),
            Some(BlockHit {
                block: IVec3::new(2, 2, 0),
                face: IVec3::NEG_Y,
            }),
            "the ray climbed into it, so the face is the one underneath"
        );
        assert_eq!(
            ray(IVec3::new(3, 2, 0)),
            Some(BlockHit {
                block: IVec3::new(3, 2, 0),
                face: IVec3::NEG_X,
            }),
            "the ray walked into this one, so the face is the one facing back along it"
        );
    }

    #[test]
    fn a_wall_crossed_at_a_shallow_angle_is_hit_in_the_voxel_the_grid_names() {
        // The failure that made an exact traversal a requirement rather than a preference,
        // arranged so that no step size gets away with it.
        //
        // The ray leaves (0.5, 0.5, 0.98) almost parallel to a one-voxel-thick wall at
        // x = 1, and reaches it ten blocks along z: it enters the wall inside voxel z = 10
        // at t = 10.0125 and leaves that z row for the next one at t = 10.0325, a fiftieth
        // of a block later. A sampler therefore reports the wall one block further along z
        // than the ray entered it unless one of its samples happens to fall in a window
        // 0.02 wide — a step of one block misses that window, and so does a step of a
        // tenth. Shortening the step does not fix the class of bug; it only moves where the
        // off-by-one lands.
        let wall: Vec<IVec3> = (0..16).map(|z| IVec3::new(1, 0, z)).collect();

        assert_eq!(
            raycast(
                Vec3::new(0.5, 0.5, 0.98),
                Vec3::new(1.0, 0.0, 20.0),
                12.0,
                only(&wall),
            ),
            Some(BlockHit {
                block: IVec3::new(1, 0, 10),
                face: IVec3::NEG_X,
            })
        );
    }

    #[test]
    fn a_ray_down_an_axis_never_steps_sideways() {
        // The zero-component branch: an axis the ray does not travel along has no boundary
        // to cross, and a `1 / 0` that became a step would drift the traversal off the
        // column the player is looking down.
        let floor = IVec3::new(0, -2, 0);
        let mut visited = Vec::new();
        let hit = raycast(middle_of(IVec3::ZERO), Vec3::NEG_Y, MAX_REACH, |voxel| {
            visited.push(voxel);
            voxel == floor
        });

        assert_eq!(
            hit,
            Some(BlockHit {
                block: floor,
                face: IVec3::Y,
            })
        );
        assert_eq!(
            visited,
            vec![IVec3::ZERO, IVec3::new(0, -1, 0), floor],
            "straight down, one voxel at a time"
        );
    }

    #[test]
    fn the_voxel_the_eye_is_inside_is_reported_with_no_face_to_place_against() {
        // A player whose head is in a block. The server should not put them there, but a
        // client that answered "nothing targeted" would let them dig their way out of
        // nowhere, and one that invented a face would place a block inside the voxel that
        // is already occupied.
        let inside = IVec3::new(0, 0, 0);
        let hit = raycast(middle_of(inside), Vec3::NEG_Z, MAX_REACH, only(&[inside]))
            .expect("the voxel the ray starts in is a hit");

        assert_eq!(hit.block, inside);
        assert_eq!(hit.face, IVec3::ZERO, "there is no face it came in through");
        assert_eq!(
            hit.place_target(),
            None,
            "and therefore nowhere to place a block"
        );
    }

    #[test]
    fn a_place_target_is_the_empty_voxel_in_front_of_the_face() {
        for face in [
            IVec3::X,
            IVec3::NEG_X,
            IVec3::Y,
            IVec3::NEG_Y,
            IVec3::Z,
            IVec3::NEG_Z,
        ] {
            let hit = BlockHit {
                block: IVec3::new(4, 70, -2),
                face,
            };
            assert_eq!(
                hit.place_target(),
                Some(hit.block + face),
                "a block placed on the {face:?} face belongs one step that way"
            );
        }
    }

    #[test]
    fn a_direction_that_is_not_a_direction_has_no_target() {
        // Unreachable through the camera, whose rotation is a unit quaternion over a look
        // state the pointer handler keeps finite. Answered anyway, because the alternative
        // to a total function here is a `NaN` that compares false against every bound and
        // walks the traversal off the grid.
        let origin = middle_of(IVec3::ZERO);
        let ahead = [IVec3::new(0, 0, -1)];
        let anywhere = only(&ahead);

        for direction in [
            Vec3::ZERO,
            Vec3::new(f32::NAN, 0.0, -1.0),
            Vec3::new(0.0, f32::INFINITY, -1.0),
        ] {
            assert_eq!(
                raycast(origin, direction, MAX_REACH, &anywhere),
                None,
                "{direction:?} is not a direction"
            );
        }

        assert_eq!(
            raycast(
                Vec3::new(f32::NAN, 0.0, 0.0),
                Vec3::NEG_Z,
                MAX_REACH,
                &anywhere
            ),
            None,
            "and a position that is not a position is not somewhere to aim from"
        );
        for reach in [-1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(
                raycast(origin, Vec3::NEG_Z, reach, &anywhere),
                None,
                "{reach} is not a reach"
            );
        }
    }

    #[test]
    fn a_ray_starting_below_the_origin_lands_in_the_voxel_that_contains_it() {
        // `floor`, not a cast. Truncation would put every position in -1..0 into voxel 0,
        // so a player standing just west of the origin would aim one block east of where
        // they were looking — and only on that side of the world.
        let target = IVec3::new(-1, -1, -4);
        let hit = raycast(
            Vec3::new(-0.5, -0.5, -0.5),
            Vec3::NEG_Z,
            MAX_REACH,
            only(&[target]),
        );

        assert_eq!(hit.map(|hit| hit.block), Some(target));
    }

    #[test]
    fn a_ray_at_the_end_of_the_world_stops_rather_than_overflowing() {
        // `spawn` and every snapshot position are checked for *finiteness*, not for a
        // range, so a server can legitimately place a player at 1e30 — where `floor`
        // saturates the voxel index at `i32::MAX` and the next step off it would overflow.
        // A debug build panics on that and a release build wraps to the far side of the
        // world; both are worse than "there is nothing out there to aim at".
        for far in [1e30f32, -1e30] {
            for direction in [Vec3::X, Vec3::NEG_X, Vec3::Y, Vec3::NEG_Z] {
                assert_eq!(
                    raycast(Vec3::splat(far), direction, MAX_REACH, |_| false),
                    None,
                    "{far} along {direction:?}"
                );
            }
        }
    }

    #[test]
    fn the_step_ceiling_cannot_cut_a_ray_the_client_actually_casts() {
        // The cap exists so a careless `reach` cannot hang a frame; it must never be what
        // ends a real query. The worst case is a diagonal ray, which crosses at most three
        // boundaries per block travelled.
        let worst_case = 3.0 * (MAX_REACH + 1.0);

        assert!(
            worst_case < MAX_STEPS as f32,
            "a {MAX_REACH}-block reach can take {worst_case} steps, against a cap of {MAX_STEPS}"
        );
    }

    // ---------------------------------------------------------------------------
    // The outline
    // ---------------------------------------------------------------------------

    #[test]
    fn the_outline_is_twelve_bars_around_one_block() {
        // Twelve, and not six faces or one cube: a filled overlay would tint the block and
        // so change the colour the palette uses to say what the block is.
        let bars = edge_bars(HIGHLIGHT_BLEED);
        let mut centres: Vec<[i64; 3]> = bars
            .iter()
            // Compared as integers scaled by 1000, because two bars either share a centre
            // or are a whole edge apart — there is nothing here for a float epsilon to be
            // about.
            .map(|(centre, _)| {
                [
                    (centre.x * 1000.0) as i64,
                    (centre.y * 1000.0) as i64,
                    (centre.z * 1000.0) as i64,
                ]
            })
            .collect();
        centres.sort_unstable();
        centres.dedup();
        assert_eq!(centres.len(), 12, "two bars sit on the same edge");

        for (centre, size) in bars {
            // One long axis and two thin ones. A bar thick on two axes would be a slab and
            // would hide the block behind it.
            let long = [size.x, size.y, size.z]
                .iter()
                .filter(|extent| **extent > HIGHLIGHT_THICKNESS)
                .count();
            assert_eq!(long, 1, "bar at {centre:?} has size {size:?}");
            assert!(
                (size.max_element() - (1.0 + 2.0 * HIGHLIGHT_BLEED)).abs() < 1e-6,
                "bar at {centre:?} does not span the block: {size:?}"
            );
        }
    }

    #[test]
    fn the_outline_mesh_is_every_bar_merged_into_one() {
        // One mesh and therefore one entity and one draw call, rather than twelve children
        // whose transforms would have to be propagated every time the target moved.
        let bar = Mesh::from(Cuboid::new(1.0, 1.0, 1.0));
        let frame = edge_frame(HIGHLIGHT_BLEED);

        assert_eq!(
            frame.count_vertices(),
            bar.count_vertices() * 12,
            "the merge dropped a bar"
        );
        assert_eq!(
            frame.indices().map(|indices| indices.len()),
            bar.indices().map(|indices| indices.len() * 12),
        );

        // Inside the block it marks, plus the bleed and half a bar. A frame that escaped
        // further would be drawn inside the neighbouring block.
        let slack = HIGHLIGHT_BLEED + HIGHLIGHT_THICKNESS / 2.0;
        let Some(positions) = frame.attribute(Mesh::ATTRIBUTE_POSITION) else {
            panic!("the outline carries positions");
        };
        for vertex in positions.as_float3().expect("three floats per position") {
            for axis in vertex {
                assert!(
                    (-slack..=1.0 + slack).contains(axis),
                    "vertex {vertex:?} escapes the block it marks"
                );
            }
        }
    }

    // ---------------------------------------------------------------------------
    // Aiming, through the app
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

    /// A chunk store holding one chunk of air with `solid` set, in **world** coordinates.
    ///
    /// Only coordinates inside the chunk that holds the spawn column are expressible, which
    /// is all these tests need and keeps the fixture honest about what the session has been
    /// streamed.
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
    /// The look state is set rather than driven through the pointer, and one snapshot places
    /// the body the camera follows — without it `follow_the_player` has nothing to attach to
    /// and the camera keeps the identity rotation, so every ray would go down -z whatever the
    /// look state said.
    fn aiming_app(store: ChunkStore) -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .insert_resource(session())
            .insert_resource(store)
            .add_plugins(PlayerPlugin);

        *app.world_mut().resource_mut::<LookState>() = LookState {
            // A quarter turn to the right of -z is +x, which is also the server's `right`.
            yaw: -FRAC_PI_2,
            pitch: 0.0,
        };
        app.world_mut().resource_mut::<SnapshotInbox>().push(
            Snapshot {
                server_tick: 1,
                entities: vec![EntityState {
                    entity_id: LOCAL_ID,
                    pos: SPAWN,
                    vel: [0.0, 0.0, 0.0],
                    yaw: 0.0,
                }],
                drops: vec![],
                ..Default::default()
            },
            Instant::now(),
        );

        app
    }

    fn target(app: &App) -> BlockTarget {
        *app.world().resource::<BlockTarget>()
    }

    /// The outline's transform and whether it is being drawn.
    fn outline(app: &mut App) -> (Vec3, Visibility) {
        let world = app.world_mut();
        let mut query = world.query_filtered::<(&Transform, &Visibility), With<TargetHighlight>>();
        let found: Vec<(Vec3, Visibility)> = query
            .iter(world)
            .map(|(transform, visibility)| (transform.translation, *visibility))
            .collect();

        assert_eq!(found.len(), 1, "exactly one outline exists");
        found[0]
    }

    fn outline_colour(app: &mut App) -> Color {
        let handle = {
            let world = app.world_mut();
            let mut query =
                world.query_filtered::<&MeshMaterial3d<StandardMaterial>, With<TargetHighlight>>();
            query.single(world).expect("one outline").0.clone()
        };
        app.world()
            .resource::<Assets<StandardMaterial>>()
            .get(&handle)
            .expect("the outline material")
            .base_color
    }

    fn deliver_progress(app: &mut App, pos: IVec3, progress: u8) {
        app.world_mut()
            .resource_mut::<MineProgressInbox>()
            .push(MineProgress {
                pos: BlockCoord {
                    x: pos.x,
                    y: pos.y,
                    z: pos.z,
                },
                progress,
            });
    }

    #[test]
    fn the_block_the_camera_looks_at_is_targeted_and_outlined() {
        // End to end through the app: the camera is placed at the authoritative position's
        // eye height, aimed by the local look state, and the first solid voxel along that
        // ray is the one the outline sits on.
        let wall = IVec3::new(3, 81, 0);
        let mut app = aiming_app(store_with(&[wall]));
        app.update();

        assert_eq!(
            target(&app),
            BlockTarget(Some(BlockHit {
                block: wall,
                face: IVec3::NEG_X,
            }))
        );
        assert_eq!(
            outline(&mut app),
            (wall.as_vec3(), Visibility::Visible),
            "the outline sits on the block's minimum corner, and is being drawn"
        );
    }

    #[test]
    fn the_nearest_block_wins_and_nothing_behind_it_is_targeted() {
        // Two walls on the same ray. A raycast that returned the last hit rather than the
        // first would let a player dig through the block in front of them.
        let near = IVec3::new(2, 81, 0);
        let far = IVec3::new(3, 81, 0);
        let mut app = aiming_app(store_with(&[near, far]));
        app.update();

        assert_eq!(target(&app).0.map(|hit| hit.block), Some(near));
    }

    /// A structure in hand replaces the single-voxel outline with its footprint.
    ///
    /// **One press cannot mean two things, so one overlay is drawn.** The footprint ghost
    /// is `super::structures`' and is asserted there; what this pins is the half that
    /// belongs here — the aiming outline stands down while a building is being shown, and
    /// comes back the moment the hand holds something that places a voxel.
    ///
    /// The anchor is one of the cells the ghost covers, so leaving both up would say the
    /// anchor is special. It is not: the server checks all nine of a tent's the same way.
    #[test]
    fn a_structure_in_hand_replaces_the_outline_with_its_footprint() {
        let wall = IVec3::new(3, 81, 0);
        let mut app = aiming_app(store_with(&[wall]));
        hold(&mut app, stack_of(crate::player::structures::ITEM_TENT));
        app.update();

        assert_eq!(
            target(&app),
            BlockTarget(Some(BlockHit {
                block: wall,
                face: IVec3::NEG_X,
            })),
            "the raycast still runs; only what is drawn on the answer changed"
        );
        assert_eq!(
            outline(&mut app).1,
            Visibility::Hidden,
            "the aimed voxel is still outlined under a tent's ghost"
        );

        // Back to a voxel in hand, and the outline returns. A structure that hid it for
        // the rest of the session would be worse than one that never hid it.
        hold(&mut app, stack_of(palette::STONE));
        app.update();
        assert_eq!(
            outline(&mut app),
            (wall.as_vec3(), Visibility::Visible),
            "the outline did not come back when the tent left the hand"
        );
    }

    #[test]
    fn a_block_past_the_reach_is_not_outlined() {
        // The camera's eye is at x = 0.5, so a wall at x = 6 is entered 5.5 blocks along —
        // past a reach of 4.5. Nothing is targeted, and the outline is not left standing on
        // whatever was targeted last.
        let mut app = aiming_app(store_with(&[IVec3::new(6, 81, 0)]));
        app.update();

        assert_eq!(target(&app), BlockTarget(None));
        assert_eq!(outline(&mut app).1, Visibility::Hidden);
    }

    #[test]
    fn an_empty_world_is_no_target_and_no_outline() {
        let mut app = aiming_app(store_with(&[]));
        app.update();

        assert_eq!(target(&app), BlockTarget(None));
        assert_eq!(outline(&mut app).1, Visibility::Hidden);
    }

    #[test]
    fn a_voxel_in_a_chunk_this_session_does_not_hold_is_not_targeted() {
        // Aiming at what the server has streamed, and nothing else. An empty store holds no
        // chunk at all, so every voxel along the ray answers "not solid" — which is the same
        // answer as air, and the honest one: this client knows nothing about those voxels.
        let mut app = aiming_app(ChunkStore::default());
        app.update();

        assert_eq!(target(&app), BlockTarget(None));
    }

    #[test]
    fn nothing_is_targeted_without_a_session() {
        // `chunk_size` arrives in the welcome, and without it a world block coordinate
        // cannot be resolved to a voxel in a chunk. A client that guessed 32 would aim at
        // the wrong voxels against any other server.
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .insert_resource(store_with(&[IVec3::new(3, 81, 0)]))
            .add_plugins(PlayerPlugin);
        app.update();

        assert_eq!(target(&app), BlockTarget(None));
        assert_eq!(outline(&mut app).1, Visibility::Hidden);
    }

    /// Records what a consumer with change detection would have seen, one entry per frame.
    #[derive(Resource, Default)]
    struct TargetChanges(Vec<bool>);

    fn log_target_changes(target: Res<BlockTarget>, mut log: ResMut<TargetChanges>) {
        log.0.push(target.is_changed());
    }

    #[test]
    fn an_idle_frame_does_not_touch_the_target() {
        // The rule `an_idle_frame_marks_neither_the_stats_nor_the_store_changed` holds
        // render.rs to, applied to the resource this module added — and it is the resource
        // most exposed to it, because the raycast runs every frame whether the player moved
        // or not. `ResMut` marks a resource changed on every `DerefMut`, so an
        // unconditional write here would repropagate the outline's transform for the rest of
        // the session.
        //
        // Observed from inside a system, because `App::update()` ends each frame with
        // `World::clear_trackers()`: an `is_changed()` check from outside is always false and
        // would pass whatever this system did.
        let mut app = aiming_app(store_with(&[IVec3::new(3, 81, 0)]));
        app.init_resource::<TargetChanges>()
            .add_systems(Update, log_target_changes.after(aim_at_a_block));

        app.update();
        assert_eq!(
            app.world().resource::<TargetChanges>().0,
            vec![true],
            "the frame that first found a target did change it"
        );

        app.world_mut().resource_mut::<TargetChanges>().0.clear();
        for _ in 0..6 {
            app.update();
        }
        assert_eq!(
            app.world().resource::<TargetChanges>().0,
            vec![false; 6],
            "an idle frame rewrote the target"
        );
    }

    #[test]
    fn losing_the_target_is_a_change_and_keeping_it_is_not() {
        // The other half of `set_if_changed`: it must not be so eager that a target which
        // genuinely went away leaves the outline standing where it was.
        let mut app = aiming_app(store_with(&[IVec3::new(3, 81, 0)]));
        app.update();
        assert!(target(&app).0.is_some());

        // The server unloads the chunk the wall was in.
        app.world_mut()
            .resource_mut::<ChunkStore>()
            .unload(ChunkCoord {
                cx: 0,
                cy: 2,
                cz: 0,
            });
        app.update();

        assert_eq!(target(&app), BlockTarget(None));
        assert_eq!(outline(&mut app).1, Visibility::Hidden);
    }

    #[test]
    fn authoritative_progress_tints_the_outline_and_zero_clears_it() {
        let wall = IVec3::new(3, 81, 0);
        let mut app = aiming_app(store_with(&[wall]));
        app.update();
        assert_eq!(outline_colour(&mut app), HIGHLIGHT_COLOUR);

        deliver_progress(&mut app, wall, 128);
        app.update();
        assert_eq!(app.world().resource::<MiningFeedback>().progress(), 128);
        assert_eq!(
            outline_colour(&mut app),
            highlight_colour(f32::from(128u8) / f32::from(u8::MAX))
        );

        deliver_progress(&mut app, wall, 0);
        app.update();
        assert_eq!(app.world().resource::<MiningFeedback>().progress(), 0);
        assert_eq!(outline_colour(&mut app), HIGHLIGHT_COLOUR);
    }

    #[test]
    fn silence_holds_the_servers_progress_then_clears_without_advancing_it() {
        let wall = IVec3::new(3, 81, 0);
        let mut app = aiming_app(store_with(&[wall]));
        tick_each_update(&mut app);
        app.update();

        deliver_progress(&mut app, wall, 97);
        app.update();
        for _ in 0..PROGRESS_SILENCE_TICKS - 1 {
            app.update();
            assert_eq!(
                app.world().resource::<MiningFeedback>().progress(),
                97,
                "silence locally advanced or cleared the server byte too early"
            );
        }
        app.update();
        assert_eq!(
            app.world().resource::<MiningFeedback>().progress(),
            0,
            "stale feedback survived its liveness window"
        );
    }

    #[test]
    fn an_authoritative_block_update_to_air_clears_feedback_that_frame() {
        let wall = IVec3::new(3, 81, 0);
        let mut app = aiming_app(store_with(&[wall]));
        app.add_plugins(WorldPlugin);
        app.update();
        deliver_progress(&mut app, wall, 220);
        app.update();
        assert_eq!(app.world().resource::<MiningFeedback>().progress(), 220);

        // Through the real world inbox and ingest system: no test-only store edit
        // stands in for the server response this criterion is about.
        app.world_mut()
            .resource_mut::<WorldInbox>()
            .push(WorldUpdate::Block {
                pos: BlockCoord {
                    x: wall.x,
                    y: wall.y,
                    z: wall.z,
                },
                block_id: palette::AIR,
            });
        app.update();

        assert_eq!(target(&app), BlockTarget(None));
        assert_eq!(app.world().resource::<MiningFeedback>().progress(), 0);
    }

    // ---------------------------------------------------------------------------
    // Clicking
    // ---------------------------------------------------------------------------

    /// Presses a mouse button the way a window does, so `InputPlugin`'s own system is what
    /// sets `just_pressed`.
    ///
    /// Writing the message rather than pressing the resource directly is the difference
    /// between testing a click and testing a field: `mouse_button_input_system` clears
    /// `just_pressed` at the start of every frame, so a resource poked before `update()`
    /// would arrive at the Update schedule already forgotten.
    fn click(app: &mut App, button: MouseButton) {
        app.world_mut().write_message(MouseButtonInput {
            button,
            state: ButtonState::Pressed,
            window: Entity::PLACEHOLDER,
        });
    }

    fn release(app: &mut App, button: MouseButton) {
        app.world_mut().write_message(MouseButtonInput {
            button,
            state: ButtonState::Released,
            window: Entity::PLACEHOLDER,
        });
    }

    /// An aiming app with a mouse and somewhere to send, plus the far end of the queue.
    ///
    /// The queue is deliberately deeper than any of these tests needs, so a full one can
    /// never be what makes a request go missing.
    fn clicking_app(store: ChunkStore) -> (App, Receiver<Vec<u8>>) {
        let mut app = aiming_app(store);
        let (outbound, sent) = Outbound::to_a_test(64);
        app.add_plugins(InputPlugin).insert_resource(outbound);
        let mut stacks = vec![
            InventoryStack {
                item_id: 0,
                count: 0,
                ..Default::default()
            };
            36
        ];
        stacks[0] = InventoryStack {
            item_id: palette::STONE,
            count: 1,
            ..Default::default()
        };
        stacks[1] = InventoryStack {
            item_id: palette::DIRT,
            count: 1,
            ..Default::default()
        };
        app.world_mut()
            .resource_mut::<InventoryInbox>()
            .push(InventoryState { stacks });
        (app, sent)
    }

    /// One edit request as the fields the server will read: position, action, slot, tick.
    type Edit = ([i32; 3], fb::EditAction, u8, u32);

    /// Every edit request waiting on the queue, read out of the encoded bytes.
    ///
    /// Out of the bytes rather than out of anything the client kept, because the frame is
    /// what the server acts on. **Filtered**, because this queue also carries the input
    /// stream: `send_player_input` shares it, and a frame that took longer to build than one
    /// server tick would otherwise drop a `PlayerInput` between a click and its assertion.
    fn edits(sent: &Receiver<Vec<u8>>) -> Vec<Edit> {
        let mut found = Vec::new();
        while let Ok(frame) = sent.try_recv() {
            let envelope = fb::root_as_envelope(&frame).expect("the client's own bytes are valid");
            let Some(request) = envelope.payload_as_block_edit_request() else {
                continue;
            };
            let pos = request.pos().expect("the position is always written");
            found.push((
                [pos.x(), pos.y(), pos.z()],
                request.action(),
                request.slot(),
                request.client_tick(),
            ));
        }
        found
    }

    /// The single edit request a click produced.
    fn one_edit(sent: &Receiver<Vec<u8>>) -> Edit {
        let found = edits(sent);
        assert_eq!(
            found.len(),
            1,
            "one click must produce exactly one request; got {found:?}"
        );
        found[0]
    }

    /// One mining intent as the exact fields the server reads.
    type Mine = ([i32; 3], bool, u32);

    fn mines(sent: &Receiver<Vec<u8>>) -> Vec<Mine> {
        let mut found = Vec::new();
        while let Ok(frame) = sent.try_recv() {
            let envelope = fb::root_as_envelope(&frame).expect("the client's own bytes are valid");
            let Some(request) = envelope.payload_as_mine_request() else {
                continue;
            };
            let pos = request.pos().expect("the position is always written");
            found.push((
                [pos.x(), pos.y(), pos.z()],
                request.active(),
                request.client_tick(),
            ));
        }
        found
    }

    fn one_mine(sent: &Receiver<Vec<u8>>) -> Mine {
        let found = mines(sent);
        assert_eq!(
            found.len(),
            1,
            "one tick must produce one intent: {found:?}"
        );
        found[0]
    }

    fn tick_each_update(app: &mut App) {
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            50,
        )));
    }

    /// One of an item, as a stack the server would send.
    fn stack_of(item_id: u16) -> InventoryStack {
        InventoryStack {
            item_id,
            count: 1,
            ..Default::default()
        }
    }

    /// Puts one stack in slot 0, as one more complete `InventoryState` from the server.
    fn hold(app: &mut App, stack: InventoryStack) {
        let mut stacks = vec![InventoryStack::default(); 36];
        stacks[0] = stack;
        app.world_mut()
            .resource_mut::<InventoryInbox>()
            .push(InventoryState { stacks });
    }

    /// One blade at the given wear, whichever blade it is.
    fn blade_of(item_id: u16, durability: u16) -> InventoryStack {
        InventoryStack {
            item_id,
            count: 1,
            durability,
            max_durability: 100,
        }
    }

    #[test]
    fn a_blade_in_hand_sends_a_swing_instead_of_mining() {
        // The mutual exclusion, from the mining side, and over **both** blades. `super::combat`
        // asserts the other half — that the same click does send a swing — and both read the
        // one predicate, which is what makes "never both" a property of a function rather
        // than of two conditions that happen to agree.
        for (name, item_id) in [
            ("the rusty sword", combat::ITEM_RUSTY_SWORD),
            ("the iron sword", ITEM_IRON_SWORD),
        ] {
            let wall = IVec3::new(3, 81, 0);
            let (mut app, sent) = clicking_app(store_with(&[wall]));
            hold(&mut app, blade_of(item_id, 100));

            tick_each_update(&mut app);
            app.update();
            let _ = mines(&sent);
            assert_eq!(
                target(&app).0.map(|hit| hit.block),
                Some(wall),
                "{name}: the outline still tracks the voxel; only the click's meaning changed"
            );

            click(&mut app, BREAK_BUTTON);
            app.update();
            app.update();

            assert!(
                mines(&sent).is_empty(),
                "{name}: a click with a blade in hand also asked to mine"
            );
        }
    }

    /// A blade worn through mines, because the server would refuse the swing.
    ///
    /// The far side of the same predicate, and the case where being wrong is worst: an
    /// honest client that asked anyway would spend the press on a certain refusal, and a
    /// refusal is silence — so the player would see a click that did nothing while standing
    /// in front of a block they meant to break. Both blades, because the rule is the item
    /// registry's and not one sword's.
    #[test]
    fn a_blade_worn_through_mines_instead_of_swinging() {
        for (name, item_id) in [
            ("the rusty sword", combat::ITEM_RUSTY_SWORD),
            ("the iron sword", ITEM_IRON_SWORD),
        ] {
            let wall = IVec3::new(3, 81, 0);
            let (mut app, sent) = clicking_app(store_with(&[wall]));
            hold(&mut app, blade_of(item_id, 0));

            tick_each_update(&mut app);
            app.update();
            let _ = mines(&sent);

            click(&mut app, BREAK_BUTTON);
            app.update();

            let (pos, active, _tick) = one_mine(&sent);
            assert_eq!(pos, [wall.x, wall.y, wall.z], "{name}");
            assert!(active, "{name}: the mining intent went out inactive");
        }
    }

    #[test]
    fn mining_asks_for_the_targeted_voxel_and_changes_nothing_locally() {
        // The rule the whole module exists to keep, and both halves of it in one test: the
        // click puts a request on the wire, and the voxel it names is still exactly as solid
        // as it was afterwards. The only writer is a `BlockUpdate`, and a click is not one.
        let wall = IVec3::new(3, 81, 0);
        let (mut app, sent) = clicking_app(store_with(&[wall]));
        tick_each_update(&mut app);
        app.update();
        let _ = mines(&sent);
        assert_eq!(target(&app).0.map(|hit| hit.block), Some(wall));

        click(&mut app, BREAK_BUTTON);
        app.update();

        let (pos, active, _tick) = one_mine(&sent);
        assert_eq!(pos, [wall.x, wall.y, wall.z]);
        assert!(active);

        let store = app.world().resource::<ChunkStore>();
        assert!(
            store.solid_at(
                BlockCoord {
                    x: wall.x,
                    y: wall.y,
                    z: wall.z,
                },
                usize::from(SIZE)
            ),
            "mining broke a block that no BlockUpdate ever mentioned"
        );
        assert_eq!(
            target(&app).0.map(|hit| hit.block),
            Some(wall),
            "and the block is still there to be aimed at"
        );
    }

    #[test]
    fn a_place_asks_for_the_empty_voxel_on_the_face_that_was_hit() {
        // The other coordinate, and the reason a hit carries a face at all. Asking for the
        // targeted voxel instead would ask to place a block where one already is — refused
        // by the server, and indistinguishable from a click that did nothing.
        let wall = IVec3::new(3, 81, 0);
        let (mut app, sent) = clicking_app(store_with(&[wall]));
        app.update();

        click(&mut app, PLACE_BUTTON);
        app.update();

        let (pos, action, slot, _tick) = one_edit(&sent);
        assert_eq!(
            pos,
            [2, 81, 0],
            "a place goes one step back along the ray, not into the block that was hit"
        );
        assert_eq!(action, fb::EditAction::Place);
        assert_eq!(slot, 0);
        assert_eq!(
            app.world().resource::<Inventory>().count(palette::STONE),
            1,
            "a request changed a count before any InventoryState arrived"
        );
    }

    #[test]
    fn held_mining_sends_once_per_tick_and_one_cancel_on_release() {
        let wall = IVec3::new(3, 81, 0);
        let (mut app, sent) = clicking_app(store_with(&[wall]));
        tick_each_update(&mut app);
        app.update();
        let _ = mines(&sent);

        click(&mut app, BREAK_BUTTON);
        for _ in 0..4 {
            app.update();
        }
        release(&mut app, BREAK_BUTTON);
        app.update();
        app.update();

        let found = mines(&sent);
        assert_eq!(found.len(), 5, "four ticks plus exactly one cancellation");
        assert!(
            found[..4]
                .iter()
                .all(|(pos, active, _)| { *pos == [wall.x, wall.y, wall.z] && *active })
        );
        assert_eq!(
            found[4],
            ([wall.x, wall.y, wall.z], false, found[4].2),
            "release did not cancel the old voxel exactly once"
        );
    }

    #[test]
    fn changing_target_sends_one_cancel_for_the_old_voxel() {
        let near = IVec3::new(2, 81, 0);
        let far = IVec3::new(3, 81, 0);
        let (mut app, sent) = clicking_app(store_with(&[near, far]));
        tick_each_update(&mut app);
        app.update();
        let _ = mines(&sent);

        click(&mut app, BREAK_BUTTON);
        app.update();
        app.world_mut().resource_mut::<ChunkStore>().apply_block(
            BlockCoord {
                x: near.x,
                y: near.y,
                z: near.z,
            },
            palette::AIR,
            usize::from(SIZE),
        );
        for _ in 0..3 {
            app.update();
        }

        let found = mines(&sent);
        assert_eq!(
            found
                .iter()
                .filter(|(pos, active, _)| *pos == [near.x, near.y, near.z] && !*active)
                .count(),
            1,
            "the old target was not cancelled exactly once: {found:?}"
        );
        assert!(
            found
                .iter()
                .any(|(pos, active, _)| *pos == [far.x, far.y, far.z] && *active),
            "held mining did not continue on the new target: {found:?}"
        );
    }

    #[test]
    fn a_click_with_nothing_targeted_asks_for_nothing() {
        // No target, no request, and above all no panic: these systems run every frame
        // whether the player is looking at anything or not.
        let (mut app, sent) = clicking_app(store_with(&[]));
        app.update();

        for button in [BREAK_BUTTON, PLACE_BUTTON] {
            click(&mut app, button);
            app.update();
        }

        assert_eq!(target(&app), BlockTarget(None));
        assert!(
            edits(&sent).is_empty(),
            "a request was sent for no target at all"
        );
    }

    #[test]
    fn a_click_from_inside_a_block_breaks_it_and_places_nothing() {
        // The degenerate hit, through the app. The eye is inside the voxel, so a break has a
        // target and a place has none — and the place must be dropped here rather than sent
        // for the server to refuse, because the only coordinate it could name is the
        // occupied one.
        let (mut app, sent) = clicking_app(store_with(&[EYE_VOXEL]));
        tick_each_update(&mut app);
        app.update();
        let _ = mines(&sent);
        assert_eq!(target(&app).0.map(|hit| hit.face), Some(IVec3::ZERO));

        click(&mut app, PLACE_BUTTON);
        app.update();
        assert!(
            edits(&sent).is_empty(),
            "a place with no face to place against was sent anyway"
        );

        click(&mut app, BREAK_BUTTON);
        app.update();
        let (pos, active, ..) = one_mine(&sent);
        assert_eq!(
            (pos, active),
            ([EYE_VOXEL.x, EYE_VOXEL.y, EYE_VOXEL.z], true),
            "a player whose head is in a block can still ask to dig it out"
        );
    }

    #[test]
    fn mining_carries_the_same_client_tick_the_input_stream_uses() {
        // One counter for the client rather than one per message kind: the contract's only
        // use for it is ordering and staleness, and two independent counters could not be
        // ordered against each other at all.
        let (mut app, sent) = clicking_app(store_with(&[IVec3::new(3, 81, 0)]));
        tick_each_update(&mut app);
        app.update();
        app.update();
        let ticks = app.world().resource::<InputCadence>().client_tick;
        assert!(ticks > 0, "no input was sent, so there is no tick to share");
        let _ = mines(&sent);

        click(&mut app, BREAK_BUTTON);
        app.update();

        let (.., tick) = one_mine(&sent);
        assert_eq!(
            tick,
            app.world().resource::<InputCadence>().client_tick,
            "mining carried a tick of its own instead of the client's"
        );
    }

    #[test]
    fn the_selected_slot_is_the_one_sent_with_a_place_request() {
        let wall = IVec3::new(3, 81, 0);
        let (mut app, sent) = clicking_app(store_with(&[wall]));
        app.update();
        *app.world_mut().resource_mut::<SelectedSlot>() = SelectedSlot(1);

        click(&mut app, PLACE_BUTTON);
        app.update();

        let (_, action, slot, _) = one_edit(&sent);
        assert_eq!(action, fb::EditAction::Place);
        assert_eq!(slot, 1);
        assert_eq!(
            app.world().resource::<Inventory>().count(palette::STONE),
            1,
            "choosing and requesting another block changed the server-sent stack"
        );
    }

    #[test]
    fn non_playing_modes_disable_targeting_and_block_requests() {
        for mode in [InputMode::Chat, InputMode::Inventory, InputMode::Menu] {
            let wall = IVec3::new(3, 81, 0);
            let (mut app, sent) = clicking_app(store_with(&[wall]));
            app.update();
            assert!(target(&app).0.is_some());

            *app.world_mut().resource_mut::<InputMode>() = mode;
            click(&mut app, BREAK_BUTTON);
            app.update();

            assert_eq!(target(&app), BlockTarget(None), "mode {mode:?}");
            assert_eq!(outline(&mut app).1, Visibility::Hidden, "mode {mode:?}");
            assert!(
                edits(&sent).is_empty(),
                "mode {mode:?} leaked a block request"
            );
        }
    }

    /// Replaces the vitals exactly as an accepted snapshot does.
    fn say_dead(app: &mut App, dead: bool) {
        let life_state = if dead {
            crate::net::LifeState::Dead
        } else {
            crate::net::LifeState::Alive
        };
        app.insert_resource(SelfVitals::from_server(crate::net::PlayerVitals {
            health: if dead { 0 } else { 100 },
            max_health: 100,
            hunger: 100,
            max_hunger: 100,
            level: 1,
            experience: 0,
            experience_to_next: 50,
            life_state,
            respawn_ticks: if dead { 60 } else { 0 },
            invulnerable: false,
            blocking: false,
        }));
    }

    #[test]
    fn a_dead_player_targets_nothing_mines_nothing_and_places_nothing() {
        // Death is read here exactly as a UI mode is, and for the same reason: the request
        // would be refused by the server anyway, so sending it buys nothing and outlining a
        // block that cannot break is a promise this client cannot keep.
        let wall = IVec3::new(3, 81, 0);
        let (mut app, sent) = clicking_app(store_with(&[wall]));
        app.update();
        assert!(target(&app).0.is_some());

        say_dead(&mut app, true);
        click(&mut app, BREAK_BUTTON);
        app.update();

        assert_eq!(target(&app), BlockTarget(None));
        assert_eq!(outline(&mut app).1, Visibility::Hidden);
        assert!(edits(&sent).is_empty(), "a dead player asked for an edit");

        click(&mut app, PLACE_BUTTON);
        app.update();
        assert!(edits(&sent).is_empty(), "a dead player asked for a place");

        // And the server bringing them back restores aiming on the frame it says so.
        say_dead(&mut app, false);
        app.update();
        assert!(target(&app).0.is_some());
    }

    #[test]
    fn dying_mid_swing_cancels_the_held_mining_intent_exactly_once() {
        // The gate makes the held target `None`, and the cancellation falls out of the same
        // path a release takes: the server is told the intent stopped rather than left to
        // infer it from silence.
        let wall = IVec3::new(3, 81, 0);
        let (mut app, sent) = clicking_app(store_with(&[wall]));
        tick_each_update(&mut app);
        app.update();
        let _ = mines(&sent);

        click(&mut app, BREAK_BUTTON);
        for _ in 0..2 {
            app.update();
        }
        say_dead(&mut app, true);
        app.update();
        app.update();

        let found = mines(&sent);
        assert_eq!(found.len(), 3, "two ticks of mining plus one cancellation");
        assert!(found[..2].iter().all(|(_, active, _)| *active));
        assert_eq!(
            found[2],
            ([wall.x, wall.y, wall.z], false, found[2].2),
            "dying did not cancel the held voxel exactly once"
        );
    }
}
