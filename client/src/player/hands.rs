//! The first-person held item: camera-space geometry, never a world entity.
//!
//! The selected authoritative stack chooses only a presentation, and this module no
//! longer holds an opinion about what that is: [`super::items`] owns the shape and the
//! colour every item draws in, and the hand reads them exactly as the pack cells and the
//! recipe panel do. What stays here is the view model itself — the meshes, the camera-space
//! placement and the cosmetic swing. None of it is a legality table: it cannot place,
//! consume or reject anything, and an unknown id remains visible through the palette
//! fallback.
//!
//! Mining progress does now enter this module, and only in one direction. The mining
//! loop is *started and stopped* by the authoritative [`super::target::MiningFeedback`]
//! and by nothing else; local time supplies the cadence of one punch and nothing else.
//! There is no timer, no hardness table and no button in that decision, so the hand
//! cannot animate a break the server has not granted and cannot outlast one it has.

use std::f32::consts::{PI, TAU};
use std::time::Duration;

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::RenderLayers;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::ecs::system::SystemParam;
use bevy::light::NotShadowCaster;
use bevy::mesh::{CylinderMeshBuilder, Indices, PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;

use super::SelfVitals;
use super::camera::ViewMode;
use super::combat::SwingSent;
use super::crafting::{ITEM_BOW, ITEM_WOODEN_SCEPTRE};
use super::inventory::{ApplyInventory, ConsumeSent, Inventory, SelectedSlot};
use super::items::{self, ItemShape, Livery};
use super::livery;
use super::target::{ApplyMiningFeedback, ApplyTargetInput, BlockTarget, MiningFeedback};
use super::{HeldItemSurface, InputMode, LocalMount, held_item_surface, stack_item_id};
use super::{bundle_strap_linear_rgba, merge_all, rolled_bundle_parts};
use crate::net::{PLACEHOLDER_APPEARANCE, Session};
use crate::world::palette;

/// The layer drawn by the origin-anchored view-model camera, and by nothing else.
const VIEW_MODEL_RENDER_LAYER: usize = 1;

/// How far the view model sits to the right of the eye.
///
/// Right of the vertical centre line is the whole of "never touching the crosshair", and
/// [`the_whole_fist_sits_in_the_lower_right_of_a_16_by_9_frame`] is what measures it.
const BASE_INBOARD: f32 = 0.10;

/// How far in front of the eye the view model sits.
///
/// Close to the near plane and small enough to remain inside the camera's free view-space
/// pocket even when terrain touches the player capsule.
///
/// **It may not shrink.** Moving the hand toward the camera is the one way to re-inflate it
/// on screen without touching [`HAND_SIZE`], so the assertion below pins it. It is also the
/// one axis of the placement that is still a constant, which is what lets every near-plane
/// bound in this file go on being measured against a number rather than against a setting —
/// see [`base_height`] for why the *other* axis could not stay one.
const BASE_DEPTH: f32 = -0.18;

/// The fist may not be brought nearer the camera than #384 left it.
const _: () = assert!(
    BASE_DEPTH <= -0.18,
    "the fist was re-inflated by moving it toward the camera"
);

/// How far down the lower half of the frame the view model's origin sits, as a fraction.
///
/// **This is the number #384 derived, promoted from a height to a proportion, and that is
/// the whole of the change.** #384 chose `BASE_TRANSLATION.y = -0.050` as the placement that
/// puts the *complete* fist inside a 16:9 frame while keeping every corner of it in the
/// lower-right quadrant. At the default field of view this fraction reproduces that height
/// exactly — `0.050 / (0.18 · tan(22.5°))` — so nothing about the default frame moves, and
/// [`the_default_frame_is_exactly_the_one_384_derived`] is what holds that to the tenth of a
/// millimetre rather than to this sentence.
///
/// **A height could not be right at more than one field of view, and the setting has
/// fifteen.** `field-of-view` runs from [`crate::settings::MIN_FIELD_OF_VIEW`] to
/// [`crate::settings::MAX_FIELD_OF_VIEW`], and the fist's own projected size does not change
/// with it — the fist is a fixed box at a fixed depth. What changes is how much frame there
/// is *below* the fist, and the limb hanging into that space is what fills it. With a
/// constant height the forearm was 18% of the visible limb at the default, 46% at 60 and 53%
/// at 110; the same arm, the same numbers, three different pictures. Worse in the other
/// direction: at the narrowest setting the fist's own lowest corner projected **past** the
/// bottom edge, so #384's defect was still shipping at one end of a slider, unseen because
/// the test that closed it reads only the default (#415).
///
/// Scaling the height with `tan(fov/2)` puts the origin at the same fraction of the frame at
/// every setting, which makes *where the hand sits* a property of the model rather than of the
/// slider. [`the_composition_sits_at_the_same_place_in_every_frame`] measures that across the
/// whole permitted range instead of trusting this paragraph.
///
/// **What it does not do is hold the limb's share of the frame constant, and no placement
/// could.** The arm is a fixed length at a fixed depth, so a wider frame simply fits more of
/// it: the forearm is 17% of the visible limb at the default and still 47% at the widest,
/// against 18% and 53% before. Holding the *proportion* would mean scaling the model with the
/// field of view, which is a different thing to want and would walk straight into
/// [`the_fist_covers_at_most_a_fifth_of_the_viewport_at_the_default_field_of_view`]. The
/// improvement this buys is at the settings people actually use — 46% to 28% at 60 — and the
/// ceiling is pinned rather than the ratio.
const HAND_DROP_FRACTION: f32 = 0.670_619_3;

/// And the same for the off-hand shield, which hangs nearer and higher.
///
/// A second fraction rather than a share of the first: the two hands were never at one
/// height, and the point of this change is that neither of them moves at the default field
/// of view. What must not happen is one hand following the frame while the other stays put,
/// which is the inconsistency deriving only the main hand would have introduced.
const SHIELD_DROP_FRACTION: f32 = 0.528_098_9;

/// How far in front of the eye the off-hand shield sits.
const SHIELD_DEPTH: f32 = -0.16;

/// Where the view model sits for a camera projecting `field_of_view` radians vertically.
///
/// The `X` and `Z` are constants; only the height is derived, and [`HAND_DROP_FRACTION`]
/// carries the reasoning for why that split falls where it does.
fn base_translation(field_of_view: f32) -> Vec3 {
    Vec3::new(
        BASE_INBOARD,
        base_height(field_of_view, HAND_DROP_FRACTION, BASE_DEPTH),
        BASE_DEPTH,
    )
}

/// How far below the eye a view model at `depth` sits, to land `fraction` of the way down
/// the lower half of the frame.
///
/// The half-height of the frame at this depth is `|depth| · tan(fov/2)`, so a height that is
/// `fraction` of it puts the origin's projected `y` at exactly `-fraction` of the half-angle
/// tangent — the same place in the frame whatever the frame is.
fn base_height(field_of_view: f32, fraction: f32, depth: f32) -> f32 {
    -fraction * depth.abs() * (field_of_view / 2.0).tan()
}

/// Where the off-hand shield sits, for the same camera.
fn shield_translation(field_of_view: f32) -> Vec3 {
    Vec3::new(
        -BASE_INBOARD,
        base_height(field_of_view, SHIELD_DROP_FRACTION, SHIELD_DEPTH),
        SHIELD_DEPTH,
    )
}

/// The vertical field of view the hand is being placed against, in radians.
///
/// **Read off the camera rather than out of [`crate::settings`], because the camera is what
/// projects.** `settings::apply` writes the setting into `Projection::Perspective::fov`, and
/// reading the projection means the hand follows anything else that ever writes it too — and
/// follows nothing at all when a test world has no camera, which is what the fallback is for.
fn view_field_of_view(projection: Option<&Projection>) -> f32 {
    match projection {
        Some(Projection::Perspective(perspective)) => perspective.fov,
        _ => crate::settings::Settings::default()
            .field_of_view()
            .to_radians(),
    }
}

/// The whole of the closed fist, and since #396 the whole of its geometry too: one cube.
///
/// **A hand, and the two ratios that make it one are asserted rather than described.** The
/// box this replaces was `(0.03825, 0.07225, 0.03825)` — 1.89 times taller than it was wide,
/// which is a forearm's proportion, and 48% of the viewport height at the default field of
/// view, which is why it read as an arm entering the frame from below (#384). #369 had
/// already scaled that box by 0.85 for the same complaint, which tuned the wrong proportion
/// instead of correcting it.
///
/// The height is exactly [`GRIP_SIZE`]'s: a fist as tall as the grip it closes on holds
/// that grip along its whole length, which puts the cross guard's lower face on the fist's
/// top face and leaves the pommel entirely below the bottom one. That is where
/// [`item_translation`] gets a blade's placement from, and it is why 64% of the blade span
/// no longer lands inside the fist's silhouette.
///
/// **The other two axes are now that height, and the reason is the material rather than
/// anatomy.** They were `0.028` and `0.030`, filled with a palm, four fingers and a thumb.
/// Three iterations modelled digits into this box — #384's knuckles, #388's fingers and
/// thumb, #391's wrist step — and not one of them could be seen: the view model's material
/// is `unlit`, so every skin-coloured face is the same colour and relief at 24 millimetres
/// renders as nothing. [`WRIST_WIDTH`]'s doc had already written that down. What a player
/// reads is the **outline**, so the hand is the shape a voxel world draws a hand as — a
/// block at the end of a narrower arm — and it spends nothing on the channel that renders
/// as nothing (#396).
pub(super) const HAND_SIZE: Vec3 = Vec3::splat(0.024);

/// The fist is a cube, which is the whole of what [`fist_mesh`] builds.
///
/// `Vec3::splat` already says so on the line above; this is the statement a *later* edit has
/// to argue with. Three iterations of modelled digits were deleted for being invisible under
/// an `unlit` material, and the shape that replaced them is the one whose outline says
/// everything it has to say — so an axis pulled away from the other two is the change that
/// needs a reason written down, not one that slips through because the mesh still builds.
const _: () = assert!(
    HAND_SIZE.x == HAND_SIZE.y && HAND_SIZE.y == HAND_SIZE.z,
    "the fist is not a cube"
);

/// A fist is exactly as tall as the grip it closes on.
///
/// The two constants live four hundred lines apart, so the relationship between them is
/// stated where the compiler can hold it rather than where a reader has to remember it.
///
/// **Equality, not `<=`.** Everything [`item_translation`] says about a blade is derived
/// from it: a grip centred on the fist reaches both of the fist's faces, which is the same
/// statement as "the guard's lower face lands on the fist's top face" and "the pommel is
/// entirely below the bottom one" only when the two heights are the same number. `<=` would
/// admit a fist shorter than its grip, where those three sentences come apart and the tests
/// in [`both_blades_show_their_guard_grip_and_pommel_around_the_fist`] start failing for a
/// reason the assertion had already been asked to rule out — an assertion weaker than the
/// claim it backs.
const _: () = assert!(
    HAND_SIZE.y == GRIP_SIZE.y,
    "the fist is not exactly as tall as the grip it closes on"
);

/// A fist, not a forearm: about as wide as it is tall.
///
/// A cube sits at exactly `1.0`, in the middle of the range rather than at either end of it,
/// so the band is left as it was: it is what stops a later edit stretching one axis of the
/// block back toward the forearm #384 was filed about.
const _: () = assert!(
    HAND_SIZE.x >= HAND_SIZE.y * 0.9 && HAND_SIZE.x <= HAND_SIZE.y * 1.4,
    "the fist's width-to-height ratio left the range a hand occupies"
);

/// The hilt the fist closes on fits *inside* the fist, so the hand hides it.
///
/// **This is what the assertion tying `BLADE_CAMERA_OFFSET` to `CAMERA_SIDE` became, and
/// the change of subject is the whole of #393.** That offset moved the entire sword — pommel,
/// grip, guard and blade — to one millimetre inside the fist's near face, so that every blade
/// section would win the depth test against the hand holding it. It was true when #382 wrote
/// it: the fist was `0.07225` tall then and genuinely swallowed the blade. #388 made the fist
/// `0.024` tall and seated the guard flush above its top face, and
/// [`a_blade_rises_clear_of_the_fists_silhouette_instead_of_growing_out_of_it`] — a
/// **screen-space** measurement that never depended on the offset — has read 0 of 101 blade
/// sections inside the fist's outline ever since. The offset was left solving a problem
/// nothing had, and its one remaining effect was the defect: it lifted the grip six
/// millimetres out of the hand, into the only place a player could see it.
///
/// So the sword now sits on the fist's own centre plane, [`item_translation`] gives it no
/// depth of its own, and "in front of the fist" stops being a question about a *sign* and
/// becomes one about two half-depths. A sign assertion with no sign left in it is a check on
/// nothing, which is worse than no check, so this is that statement re-derived against what
/// actually holds the grip in the hand now.
///
/// Both halves of the hilt the hand closes over are named. The grip is what must never show.
/// The pommel deliberately does show *below* the fist — that is #384's property, and the thing
/// that says the hand is closed on a hilt rather than being where the hilt begins — but a
/// pommel deeper than the fist would show in *front* of it exactly as the grip did.
const _: () = assert!(
    GRIP_SIZE.z < HAND_SIZE.z && POMMEL_SIZE.z < HAND_SIZE.z,
    "the hilt the fist closes on is deeper than the fist, so it shows in front of the hand"
);

/// The fist's box contains the grip outright, on all three axes — **and since #396 the box
/// is the whole of the fist, so this assertion is the whole of the occlusion.**
///
/// It used to be the weaker half of a pair. [`fist_mesh`] filled [`HAND_SIZE`] with a palm
/// three quarters as deep as the box and a band of digits standing proud of it, and the
/// digits left gaps between them on purpose: a box inside a box is hidden by it from every
/// viewpoint outside, but only where the outer box is *there*, and through the gap between
/// two fingers the surface a player looked at was the palm's, 7.8 mm further from the eye
/// than the knuckles were. The convex solid that actually hid the grip was the palm, so a
/// second assertion — `GRIP_SIZE.z / 2.0 < PALM_DEPTH - HAND_SIZE.z / 2.0`, with two tenths
/// of a millimetre in it — was the one the occlusion rested on.
///
/// The fist is one cube now. There is no palm plane behind the near face and no gap to look
/// through, so the containment below is the convex-solid statement outright: a point inside
/// a convex solid is behind the surface that solid presents from every viewpoint outside it,
/// and a rigid transform moves the pair together. The margin it carries went from 0.2 mm to
/// **5 mm** — `GRIP_SIZE.z / 2.0` is `0.007` and the cube's near face is at `0.012` — which
/// is why [`the_hand_stays_closed_over_the_grip_through_every_animation`] now measures
/// millimetres where it measured tenths.
///
/// `<=` rather than `<` on `Y` on purpose: `HAND_SIZE.y == GRIP_SIZE.y` is asserted above and
/// is the whole of how [`item_translation`] places a blade, so requiring the grip to be
/// *shorter* than the fist here would contradict it. The other two axes are strict through
/// the assertion above, which is what leaves the margin this one now owns.
const _: () = assert!(
    GRIP_SIZE.x <= HAND_SIZE.x && GRIP_SIZE.y <= HAND_SIZE.y && GRIP_SIZE.z <= HAND_SIZE.z,
    "the grip is not inside the fist that closes on it"
);

/// Extra camera-space clearance for the blade composition during its reachable swings.
///
/// **Re-derived, because what it was buying clearance back from is gone.** It was written to
/// repay what `BLADE_CAMERA_OFFSET` spent: a sword pushed forward inside the merged mesh
/// rotates toward the eye during an overhead cut, so the whole model was set back to pay for
/// it. There is no forward offset any more, and this is now simply the set-back the blade's
/// *own* arcs need — a bound nothing else in the composition reaches.
///
/// An overhead cut is only ever drawn with a blade in hand — `combat.rs` routes the left
/// button on the item id and the three arcs belong to the blades — so the pose that carries
/// the view model nearest the camera is always one this offset has already pushed back.
/// [`the_forearm_is_as_long_as_the_near_plane_permits`] spends it in the bound it derives for
/// [`ARM_REACH`], and [`every_held_arrangement_clears_the_near_plane_through_every_swing`]
/// sweeps the real vertices of every arrangement through every reachable pose against it. So
/// it is not free to become zero now that its original reason has gone: dropping it shortens
/// the arm the near plane permits to below the arm that is already there, which is #394's
/// subject and not this change's.
const BLADE_NEAR_PLANE_CLEARANCE: f32 = 0.004;

/// How far a carried object sinks into the top of the fist holding it.
///
/// A gap would leave the item floating and no overlap would put two faces on the same
/// plane. Six millimetres is enough to hide the join without swallowing the object's
/// silhouette; [`the_item_stays_recognisable_outside_the_fist`] holds the other side.
const HOLD_OVERLAP: f32 = 0.006;

/// How far the forearm reaches below the view model's origin **at the model's resting
/// depth**.
///
/// **The near plane sets this number, and nothing about how an arm looks does.** The model
/// sits at [`BASE_DEPTH`] and the camera's near plane is at `0.1`, which leaves
/// eight centimetres of headroom — and the composition rotates about its *own* origin, so
/// everything below that origin swings toward the eye during an overhead cut. At the tightest
/// reachable pose — [`CUT_PITCH_RADIANS`] on top of the rest pitch, with the placement
/// bump already carrying the model [`PLACE_BUMP_DISTANCE`] forward — a point `L` below the
/// origin gives up about `0.88·L` of that headroom, and the arm's own half-width and
/// half-depth spend a little more. The bound that leaves is **`0.0599`**, re-derived against
/// the cube's section rather than inherited from #391's `0.0620`: #396 gave the limb the
/// fist's own depth on the fist's own centre plane, which brought the end cap's far face
/// toward the eye and cost 2.1 mm of the ceiling. This value leaves 1.6 mm of camera-space
/// clearance; [`the_forearm_is_as_long_as_the_near_plane_permits`] re-derives the bound from
/// the constants rather than trusting this paragraph, and
/// [`every_held_arrangement_clears_the_near_plane_through_every_swing`] sweeps the real
/// vertices through every reachable pose.
///
/// **It may not simply grow, and that constraint is the whole reason [`drawn_arm_reach`]
/// exists.** #389 read the ceiling above as a cap on the arm outright, and #391 shipped a
/// partial on the strength of it: a *fixed* length that stays clipped through a thrust needs
/// `0.0705`, the near plane permits `0.0599`, and no number satisfies both. What #394 found is
/// that the two bounds are never imposed by the same frame — the cut that threatens the near
/// plane carries the model *toward* the eye and the thrust that needs the reach carries it
/// away — so the length that is over-constrained is only the *constant* one. Read this as the
/// arm's length at `along_view == 0` and [`drawn_arm_reach`] for what it is at every other
/// frame.
const ARM_REACH: f32 = 0.058;

/// How far the wrist is buried in the fist.
///
/// The same trick [`HOLD_OVERLAP`] plays for a carried object, for the same reason: a gap
/// would leave the arm detached and a flush join would put two faces on one plane, which is
/// the flicker rule `client/AGENTS.md` states for the body rig.
const ARM_OVERLAP: f32 = 0.004;

/// The wrist: the short section between the fist and the forearm proper.
const WRIST_LENGTH: f32 = 0.012;

/// How far the wrist is buried in the forearm below it, for the reason above.
const ARM_JOIN: f32 = 0.002;

/// Where the forearm's top face sits: inside the wrist, which is itself inside the fist.
///
/// Named because [`forearm_transform`] is the pivot the limb's length is measured from now
/// that the length is a number in a transform rather than a span in the buffers. It is the
/// one end of the arm that never moves, which is what makes it the right thing to hang the
/// rest off: the join with the wrist cannot open up, whatever [`drawn_arm_reach`] answers.
const FOREARM_TOP: f32 = -HAND_SIZE.y / 2.0 + ARM_OVERLAP - WRIST_LENGTH + ARM_JOIN;

/// The forearm's top is above its own end, and buried in the limb above it.
const _: () = assert!(
    FOREARM_TOP < 0.0 && FOREARM_TOP + ARM_REACH > 0.0,
    "the forearm does not hang from inside the wrist down past the model's origin"
);

/// How wide the wrist is, as a fraction of the fist's width.
///
/// **This is the only thing that says *hand* rather than *slab*, and that is a fact about the
/// material rather than about anatomy.** The view model's material is `unlit`, so nothing in
/// this composition is shaded: every skin-coloured face is exactly the same colour and relief
/// at this scale cannot be seen as relief. What a player reads is the outline, and an arm the
/// fist's own width would restore precisely the tall flat rectangle #384 was filed about —
/// the fix for a forearm-sized hand cannot be a hand-sized forearm. Three quarters is a
/// wrist's real proportion and it puts a step in the silhouette at the one place the eye is
/// looking for one.
///
/// **#396 read this paragraph as the diagnosis it is and acted on it.** It was written while
/// [`fist_mesh`] still built a palm, four fingers and a thumb, and it says in as many words
/// that none of that could be seen. The fist is one cube now; this step is not merely the
/// *only* thing separating the hand from the arm, it is the whole silhouette budget, and
/// [`the_wrist_steps_in_from_the_fist_in_the_projected_outline`] measures it through the
/// transform the renderer uses rather than trusting the fraction.
const WRIST_WIDTH: f32 = 0.75;

/// And how wide the forearm is: the wrist's width, because it is the same limb.
///
/// **It was `0.93`, and that swelled back out below the one step the silhouette has.** The
/// number was chosen to keep the fist overhanging the whole limb in *model* space, and in
/// model space it did. On screen it did not: the forearm sits lower, the rest pitch brings
/// it nearer the eye, and it therefore projected **wider on the inboard side than the fist**
/// — `0.4511` against `0.4562` in tangent units at rest. "What must never happen is an arm
/// broader than the hand on the end of it" was already false in the one direction nobody had
/// measured, which is why this is derived from [`WRIST_WIDTH`] now rather than tuned near it
/// (#415).
const FOREARM_WIDTH: f32 = WRIST_WIDTH;

/// The limb below the fist is one width, and the fist overhangs all of it.
///
/// Restated rather than deleted: it used to read `WRIST_WIDTH < FOREARM_WIDTH`, which said
/// the same thing about the fist while also requiring the swell this removed. What it exists
/// to stop is an arm the hand does not overhang, and that clause is untouched.
const _: () = assert!(
    FOREARM_WIDTH <= WRIST_WIDTH && WRIST_WIDTH < 1.0,
    "the limb below the fist outgrows the wrist, or the fist no longer overhangs it"
);

/// How far outboard the wrist and the forearm are carried, so their outer faces land on the
/// fist's.
///
/// **The limb's outboard edge is one straight line and the step is inboard only.** Centring a
/// narrower box under a wider one puts half the step on each side, which reads as a limb that
/// necks in twice rather than as a wrist. Half the width the wrist gives up is exactly what it
/// takes to put the two outer faces on one plane, and the whole of the step then lands on the
/// side the eye is looking at — the inboard one, since [`BASE_INBOARD`] holds the model out to
/// the right of the frame (#415).
///
/// **This is measured on screen and not here.** A model-space offset is not the property being
/// asked for: what a player sees is the projected outline, and
/// [`the_limb_presents_one_outboard_edge_and_steps_in_only_on_the_inboard_side`] reads it
/// through the transform the renderer uses.
const LIMB_OUTBOARD_OFFSET: f32 = HAND_SIZE.x * (1.0 - WRIST_WIDTH) / 2.0;

/// The view model's rest pose: how far the composition is pitched over and rolled inboard
/// before any animation adds to it.
///
/// Named because more than [`animated_transform`] reasons about them now. The pitch is what
/// tips the lower end of [`forearm_mesh`] toward the eye — which is why the near plane, not
/// the frame, is what bounds the arm — and the roll is what leans the whole limb inboard on
/// screen. Angling the arm outboard to cancel that roll was measured and rejected: at 16:9
/// the bottom edge of the frame is far nearer than the right one, so the lean buys no extra
/// field of view, and it opens a sliver of world along the fist's inboard edge where the
/// fist's far face projects wider than the arm's.
const REST_PITCH_RADIANS: f32 = -0.18;
const REST_ROLL_RADIANS: f32 = -0.12;

const BLOCK_EDGE: f32 = 0.055;
const MATERIAL_RADIUS: f32 = 0.020;
const MATERIAL_LENGTH: f32 = 0.050;

/// A struck disc, carried face-on: the flat side turned toward the camera, so a coin in the
/// hand reads as a coin rather than as an edge-on sliver.
///
/// Its radius is the material stub's, because a coin is a thing of that size in a fist and
/// the stub is what already sits correctly above [`HAND_SIZE`]; only the depth is a coin's
/// own. See [`item_translation`], which stands it on the top of the fist by that radius.
const COIN_RADIUS: f32 = 0.022;
const COIN_THICKNESS: f32 = 0.008;

/// The mining loop's cadence, and how far one punch carries the view model.
///
/// **All three are cosmetic, and the cadence in particular is not a clock.** How fast the
/// hand punches says nothing about how fast the block is coming apart: a punch takes the
/// same time on dirt as on stone, and the loop simply repeats for as long as
/// [`HandIntent::mining`] — the server's own answer — stays true.
const MINE_PUNCHES_PER_SECOND: f32 = 2.4;
const MINE_PUNCH_RADIANS: f32 = 0.42;

/// How far the fist reaches away from the camera at full extension.
///
/// **Toward the block, so along -Z**, which is deliberately the opposite of
/// [`PLACE_BUMP_DISTANCE`]: a punch reaches for what it is breaking and a placement draws
/// back from what it just set down. Two animations on one axis have to be told apart at a
/// glance, and a shared direction is the first thing that stops being possible once a
/// third one lands here.
const MINE_PUNCH_DISTANCE: f32 = 0.045;

const PLACE_BUMP_TIME: Duration = Duration::from_millis(150);
const PLACE_BUMP_DISTANCE: f32 = 0.025;

/// How long one attack swing plays for, whichever shape is playing.
///
/// A one-shot, unlike the mining loop above, which repeats while the server reports
/// progress: an attack is an event the server judges once, so its feedback happens once.
///
/// **One duration for every shape, and that is a decision rather than a convenience.** A
/// blade arc that took longer than a sceptre's cast would put the drawn shape into the
/// *timing* of the hand, and timing is the one presentation channel a cooldown also lives in.
/// Arcs that differ in geometry alone cannot be read as tempos, so nothing a player sees here
/// can be mistaken for the server changing its mind about how often a blade swings.
///
/// It survives #421 unchanged and is worth saying why: with one blade arc the constant is
/// trivially shared, and the reasoning above is about what a *later* shape may not do.
const ATTACK_SWING_TIME: Duration = Duration::from_millis(220);

/// The blade cut: how far it carries the blade down, how far across, and how far the edge
/// turns over into the stroke.
///
/// **One arc, diagonal, from the upper right down across to the lower left**, replacing the
/// overhead cut, the lateral slash and the thrust #231 introduced. The three of them were a
/// judgement about how a fight reads — *"what a player must stop seeing is the same arc twice
/// in a row"* — and #421 is the opposite judgement, made by the same person, after seeing it
/// played. It is not a tidy-up: three arcs were wanted and are no longer.
///
/// **The three terms are one motion and not three arcs added together.** Each of the numbers
/// they replace was tuned to be a whole swing on its own, so their vector sum overshoots both
/// — a cut that fell as far as the overhead did *and* crossed as far as the slash did leaves
/// the frame. What they are derived against instead is the tip's path on screen:
/// [`the_blade_cuts_from_the_upper_right_down_to_the_lower_left`] measures it, and these are
/// the largest pair that keeps the descent and the crossing within a quarter of each other —
/// which is what makes the stroke read as a diagonal rather than as a chop with a lean or a
/// sweep with a dip.
///
/// **The roll is not decoration, and it is the one term carried over at its old value.** The
/// lateral slash's doc was the record of why it existed: a blade held upright and moved
/// sideways reads as a wiper blade, and the roll is what puts an edge on the front of the
/// motion. A diagonal needs it for exactly the same reason, and needs it just as far — it is
/// the two terms that describe *where the tip goes* that had to come down, not the one that
/// describes which way the edge faces.
///
/// **The signs are measured rather than reasoned about, and the yaw's is the opposite of the
/// slash's.** [`SwingPose::yaw`]'s doc says positive turns the blade toward `-X`, and that is
/// true of the pose the lateral slash held it in — but the composition is rolled before it is
/// yawed, so which way a yaw carries the *tip* on screen depends on the roll it is applied
/// over. This arc rolls the other way, and with that roll a positive yaw opens the stroke
/// outward off the edge of the view instead of across the body. All three terms are therefore
/// negated or not at the call site to produce one measured outcome — the tip travelling down
/// and inboard — rather than to match a sentence written for a different pose.
///
/// The roll's own sign is the physical half of that: the blade leans *into* the direction of
/// travel, so a stroke falling to the inboard side tilts that way, where the slash crossing
/// the other way tilted outboard.
const CUT_PITCH_RADIANS: f32 = 0.68;
const CUT_YAW_RADIANS: f32 = 0.80;
const CUT_ROLL_RADIANS: f32 = 0.75;

/// How far the sceptre's cast drives along the view.
///
/// **It was `THRUST_REACH` and the blade arc that named it is gone**, so it takes the name of
/// its one remaining consumer. The value does not move: [`SwingShape::Cast`] spent exactly
/// this before #421 and spends exactly this after it.
///
/// Along -Z, the direction [`MINE_PUNCH_DISTANCE`] already established for *toward the thing
/// being hit*, and the opposite of [`PLACE_BUMP_DISTANCE`]'s draw-back. It is also the one
/// swing term [`drawn_arm_reach`] still has to answer for — the arm's length follows the
/// composition's depth, and a cast is now the only arc that changes it.
const CAST_REACH: f32 = 0.11;

/// The eating arc: how far it tips the held item back over the fist, and how far it carries
/// the composition toward the eye.
///
/// **Toward the camera on both channels, which is what makes this the one arc that reads as
/// bringing something to the mouth rather than as reaching for something.** A positive pitch
/// turns everything *above* the origin toward the eye — the item sits on top of the fist, so
/// it is the item that comes back — and `+Z` is the direction [`PLACE_BUMP_DISTANCE`] already
/// established for *toward the eye*, the opposite of [`CAST_REACH`]'s and
/// [`MINE_PUNCH_DISTANCE`]'s reach for the thing being hit. The signs are therefore the
/// vocabulary this file already has rather than a new convention: nothing else here moves the
/// held item toward the face, and nothing else here needs to.
///
/// **Both numbers are the near plane's, and small for that reason alone.** The composition
/// sits [`BASE_DEPTH`] from an eye whose near plane is at `0.1`, and a placement bump can
/// already have spent [`PLACE_BUMP_DISTANCE`] of that headroom in the same frame — a right
/// click and a consume press inside one 220 ms both play. What is left has to cover the
/// tallest thing a fist can hold, swung toward the eye, so the pair is bounded by the block
/// rather than by the food this arc is actually drawn for.
/// [`every_held_arrangement_clears_the_near_plane_through_every_swing`] sweeps the real
/// vertices of every arrangement against it, which is why these are the values and not a
/// paragraph of arithmetic. What that sweep answers, measured: the corner that binds is the
/// held **block**'s upper near one, and this pair leaves it about a centimetre of camera
/// space at the tightest frame — a pitch of `0.30` with a rise of `0.020` spends all of it
/// and fails by 65 micrometres.
///
/// **The pitch is spent against the rest pose rather than from zero**, which is the thing to
/// know before changing it: [`REST_PITCH_RADIANS`] is `-0.18`, so this is what carries the
/// composition back *through* upright rather than the whole angle it ends at.
const EAT_PITCH_RADIANS: f32 = 0.22;
const EAT_RISE: f32 = 0.014;

/// The whole sword, pommel to tip, in the same camera-space units as the block and
/// material meshes.
///
/// **The budget, and every part below is spent out of it**: it is exactly what the single
/// box occupied before #204, so nothing about where the hand sits or how far it swings
/// moves — the same constraint #175's fist met against [`HAND_SIZE`]. Grow one part and
/// another gives the length back, which is what
/// [`the_sword_spends_exactly_the_length_the_box_did`] holds.
const SWORD_LENGTH: f32 = 0.115;

/// How much of that length is blade, once the pommel, the grip and the guard have taken
/// theirs.
const BLADE_LENGTH: f32 = 0.075;

/// The blade across the flats, at the guard. It narrows from here — see
/// [`POINT_WIDTH_FRACTION`].
const BLADE_WIDTH: f32 = 0.030;

/// The blade through the ridge, which is the thickest it ever is: the section is knife-thin
/// at both edges and full thickness only along the central flat.
const BLADE_THICKNESS: f32 = 0.012;

/// How much of the blade's half-width the central flat occupies, the rest being bevel.
///
/// **This is what makes the section a hexagon rather than a rectangle**, and it is the whole
/// of why the blade reads as bevelled: six side faces per span instead of four, each drawn at
/// its own shade.
///
/// **That sentence used to say "so the light catches a different pair as the hand turns", and
/// there was no light to catch.** The first-person material is `unlit`, so every face of this
/// section rendered exactly one colour and the hexagon might as well have been a rectangle —
/// for as long as the blade existed. [`SHADE_LIGHT`] is what makes the claim true, by baking
/// the light the material does not have into the vertices.
const BLADE_RIDGE_FRACTION: f32 = 0.34;

/// How wide the blade is where the point begins, as a fraction of its width at the guard.
///
/// Under one, so the blade is waisted rather than parallel — the taper a gladius has before
/// the point starts at all.
const POINT_WIDTH_FRACTION: f32 = 0.76;

/// How much of the blade's length is the taper to the tip.
const POINT_LENGTH: f32 = 0.020;

/// What is left of the section at the very tip, as a fraction of the section at the
/// shoulder.
///
/// **Small rather than zero, and that is a renderer's constraint rather than a shape
/// decision.** A section that collapses to one vertex turns six quads into six zero-area
/// slivers, and a zero-area triangle has no normal to compute — so the tip converges to a
/// hexagon a tenth the size instead, which is a tenth of two and a half millimetres of
/// camera space and reads as a point.
const POINT_TIP_FRACTION: f32 = 0.10;

/// The cross guard: thicker than the blade so it stands out from it in the hand, thin in
/// length, and wide enough across to read as a guard rather than a collar.
const GUARD_SIZE: Vec3 = Vec3::new(0.019, 0.006, 0.044);

/// The box the grip is turned inside: narrower than everything around it.
///
/// **Still a box, and it has to stay one.** The cylinder below is *inscribed* in it — same
/// height, radius `GRIP_SIZE.x / 2` — which is what lets the three `const _: () = assert!`
/// blocks around this constant and [`HAND_SIZE`] keep holding with nothing restated. They
/// compare extents component by component, and a turned grip that stayed inside the extents
/// it replaced changes none of them. If one of them ever has to move, the cylinder is not
/// inscribed and the fix is the cylinder.
const GRIP_SIZE: Vec3 = Vec3::new(0.014, 0.024, 0.014);

/// How many sides the turned grip is drawn with.
///
/// **Eighteen, because the grip is held at arm's length and the mesh is built once.** A
/// cylinder costs two triangles per side plus a fan at each cap — tens of triangles on an
/// asset that is cached, against a silhouette that is the first thing saying *this is held
/// here*. A grip is the one part of a sword that is never square.
const GRIP_SIDES: u32 = 18;

/// The pommel: brass, wider than the grip, which is what stops the sword ending in a stub.
const POMMEL_SIZE: Vec3 = Vec3::new(0.018, 0.010, 0.017);

/// **The pommel's sides and the wrist's sides are not the same two planes**, and this is the
/// rule rather than the accident that currently satisfies it.
///
/// `item_translation` seats a grip's centre on the fist's centre, which leaves the whole
/// pommel below the fist's bottom face — where the wrist is. Before #415 the two boxes were
/// the same width to the *bit*: `HAND_SIZE.x * WRIST_WIDTH` and `POMMEL_SIZE.x` are both
/// `0.01799999923` as `f32`, their heights overlapped by 8 mm, and the pommel's 17 mm depth
/// sat inside the wrist's 24 mm — so about 8 × 17 mm of steel and skin fought for the depth
/// buffer on each side of the limb, and the sword was drawn through the arm. That is rule 2
/// of `client/AGENTS.md`, which is checked for the body rig by `no_two_colours_share_a_plane`
/// and was checked by nothing here.
///
/// [`LIMB_OUTBOARD_OFFSET`] moves the wrist off those planes, but it was introduced for the
/// silhouette and would take this fix with it if it were ever revisited. So the property is
/// asserted where it cannot be revisited by accident, and
/// [`no_two_colours_share_a_plane_in_the_hand`] holds the general rule over the real mesh.
const _: () = {
    let wrist_outboard = LIMB_OUTBOARD_OFFSET + HAND_SIZE.x * WRIST_WIDTH / 2.0;
    let wrist_inboard = LIMB_OUTBOARD_OFFSET - HAND_SIZE.x * WRIST_WIDTH / 2.0;
    let pommel = POMMEL_SIZE.x / 2.0;
    assert!(
        wrist_outboard != pommel
            && wrist_outboard != -pommel
            && wrist_inboard != pommel
            && wrist_inboard != -pommel,
        "the pommel and the wrist share a side plane, so the sword is drawn through the arm"
    );
};

/// **The guard and the pommel each cover the grip's cross-section, and that is what makes the
/// one plane the hand shares with the hilt unobservable.**
///
/// `HAND_SIZE.y == GRIP_SIZE.y` is asserted beside the two constants and the whole blade
/// arrangement is derived from it, so the grip's top and bottom faces are coplanar with the
/// fist's by construction — two colours on one plane, facing the same way, which is what rule
/// 2 of `client/AGENTS.md` forbids. It is invisible because a solid box seats on each of those
/// planes and covers the whole of the smaller face: the guard above, the pommel below.
///
/// That is a claim about numbers, so it is checked like one rather than left in the doc of the
/// test that has to exempt the pair — [`no_two_colours_share_a_plane_in_the_hand`] points here
/// for its reason, and a change that narrows either box past the grip fails here first (#415).
const _: () = assert!(
    GUARD_SIZE.x > GRIP_SIZE.x
        && GUARD_SIZE.z > GRIP_SIZE.z
        && POMMEL_SIZE.x > GRIP_SIZE.x
        && POMMEL_SIZE.z > GRIP_SIZE.z,
    "the hilt no longer covers the grip's end faces, so the fist shares a visible plane with it"
);

/// How far the blade's root is buried in the guard.
///
/// Half the guard, so the blade's own end cap sits *inside* the guard's volume rather than
/// flush with its top face. Flush would be two coplanar quads facing the same way, which is
/// the flicker rule 2 in `client/AGENTS.md` names for the body rig — and the reason a rust
/// mark stands proud of the blade rather than sitting on it.
const BLADE_TANG: f32 = GUARD_SIZE.y / 2.0;

/// How many steps the blade's root span is lofted in.
///
/// **The pitting is displacement, so this is what decides how finely the silhouette can be
/// eaten into.** Twenty-four over roughly 60 mm of blade puts a ring every 2.5 mm, about a
/// third of the smallest freckle [`livery::field`] draws — enough for a pit to have a floor
/// and two walls rather than being one facet tilted inward.
///
/// It is only paid by a blade that wears a livery. An un-liveried blade lofts at one step
/// per span, which is the two-span loft this file drew before there was a livery at all.
const BLADE_STEPS_ROOT: u32 = 24;

/// How many steps the point's span is lofted in.
///
/// Fewer, and not for the reason a lower number usually has: [`livery::field`] keeps the
/// rust off the point entirely, so every extra ring here would be geometry no pit reaches.
/// Six is what keeps the taper smooth once the rings below it are this dense.
const BLADE_STEPS_POINT: u32 = 6;

/// How many steps each of the blade's six perimeter faces is divided into.
///
/// **Three, and the hexagon is why it is not more.** The section is knife-thin at both
/// edges and full thickness only along the central flat, so a pit near an edge has almost
/// no metal to eat; the flats are where the rust reads, and three steps put two interior
/// rings on each of them. Going finer buys detail on the bevels, where it cannot be seen.
const BLADE_STEPS_AROUND: u32 = 3;

/// How deep a pit eats, as a fraction of the blade's local half-thickness.
///
/// **A property of the livery since #420, not of the blade.** Corrosion eats metal, so worn
/// steel displaces; forge marks are the record of work done to a surface that is still
/// whole, so forged steel answers zero and its blade is lofted exactly as an un-liveried one
/// is. `livery::pit_depth` carries the numbers and the reasoning.
///
/// **Through the thickness only, never into the outline.** A vertex is displaced in `x`
/// alone, so the two corners that sit on the blade's edges — where `x` is zero — do not
/// move at all and the silhouette is the silhouette it was. That is what makes "no
/// displaced vertex leaves the blade's envelope" a property of the arithmetic rather than
/// something to check afterwards, and it is the right shape besides: corrosion eats through
/// a flat, it does not take bites out of an edge.
fn pit_depth(livery: Option<Livery>) -> f32 {
    livery.map_or(0.0, livery::pit_depth)
}

/// Where the light the view model is shaded from comes from.
///
/// **A baked, model-space light, because the first-person material has no other kind.** That
/// material is `unlit`, so `StandardMaterial` ignores vertex normals outright and every face
/// of a mesh renders one colour. [`BLADE_RIDGE_FRACTION`]'s doc claimed that six faces per
/// span meant "the light catches a different pair as the hand turns"; there was no light to
/// catch, and the same was true of #426's pits — the displacement preserves the outline by
/// construction, so under a flat colour a pit changed nothing anybody could see.
///
/// This is the third instance of one family. `client/AGENTS.md` records the first: the fist
/// is one cube since #396 because relief on a 24 mm box was invisible for exactly this
/// reason, and three iterations of modelled digits were deleted for costing geometry and
/// buying nothing.
///
/// **Above, to the left, and slightly toward the eye.** The view model is a child of the
/// camera, so its `+Z` points at the viewer and this is the three-quarter key light every
/// hand-painted asset is lit from.
///
/// **It turns with the sword, and that is the honest cost.** A model-space light does not
/// stay fixed in the world, so a swing carries the highlight with it. Under an `unlit`
/// material there is no correct answer available — the alternative is no relief at all —
/// and a baked light is what a low-poly asset does. It reads as form, which is the whole of
/// what is being bought here.
const SHADE_LIGHT: Vec3 = Vec3::new(-0.40, 0.80, 0.45);

/// How dark a face turned fully away from [`SHADE_LIGHT`] is left.
///
/// **A floor rather than a clamp at zero**, so the far side of a blade is a shade of its own
/// steel instead of a silhouette. Nothing rises above identity: a fully lit face is `1.0`, so
/// the shading only ever takes light away and `player/items.rs` stays the one answer to what
/// an item's colour *is*.
const SHADE_FLOOR: f32 = 0.45;

/// A carried structure's outer bound. [`rolled_bundle_parts`] fills it with the same roll
/// and two straps used by the world drop, so a tent under the arm does not read as another
/// stackable cube.
const BUNDLE_SIZE: Vec3 = Vec3::new(0.075, 0.042, 0.048);

/// An implement's haft: longer and thicker than a blade, because what tells a shovel from
/// a sword at a glance is that one is a handle with weight on the end and the other is
/// mostly edge.
const TOOL_HAFT_SIZE: Vec3 = Vec3::new(0.014, 0.130, 0.014);

/// And its head, across the top of that haft. Wider than the haft in x and z and short in
/// y, which is the T a shovel, a pickaxe and an axe all share — and the whole of what
/// distinguishes the silhouette from [`sword_mesh`]'s guard, grip and tapering blade.
const TOOL_HEAD_SIZE: Vec3 = Vec3::new(0.052, 0.020, 0.026);

/// A carried armour plate: broad enough to read as clothing and shallow enough not to
/// become another block in the hand.
const ARMOUR_BODY_SIZE: Vec3 = Vec3::new(0.060, 0.070, 0.016);
const ARMOUR_SHOULDER_SIZE: Vec3 = Vec3::new(0.026, 0.018, 0.022);

const BOW_LENGTH: f32 = 0.120;
const BOW_STAVE: f32 = 0.009;
const BOW_DEPTH: f32 = 0.008;
const SCEPTRE_LENGTH: f32 = 0.130;
const SCEPTRE_SHAFT: f32 = 0.013;
const SCEPTRE_ORB_RADIUS: f32 = 0.018;
const SCEPTRE_GREEN: [f32; 4] = [0.16, 0.82, 0.28, 1.0];

/// A haft with a head across the top of it: one mesh, two boxes.
///
/// Merged rather than parented, for the reason the body's parts are merged in
/// `player::part_mesh`: the view model is one entity with one transform that
/// `animate_view_model` drives, and a second entity under it would be a second thing to
/// keep in step with a swing.
///
/// The three implements share it and are told apart by colour — see [`ItemShape::Tool`].
fn tool_mesh() -> Mesh {
    let mut merged = Mesh::from(Cuboid::from_size(TOOL_HAFT_SIZE));
    let head = Mesh::from(Cuboid::from_size(TOOL_HEAD_SIZE)).translated_by(Vec3::new(
        0.0,
        TOOL_HAFT_SIZE.y / 2.0,
        0.0,
    ));
    merge_all(&mut merged, [head], "held tool");
    merged
}

/// One body plate and two shoulders, merged into the single view-model entity.
fn armour_mesh() -> Mesh {
    let mut armour = Mesh::from(Cuboid::from_size(ARMOUR_BODY_SIZE));
    let shoulders = [-1.0, 1.0].map(|side| {
        Mesh::from(Cuboid::from_size(ARMOUR_SHOULDER_SIZE)).translated_by(Vec3::new(
            side * ARMOUR_BODY_SIZE.x * 0.48,
            ARMOUR_BODY_SIZE.y * 0.34,
            0.0,
        ))
    });
    merge_all(&mut armour, shoulders, "held armour");
    armour
}

/// One tapered rectangular limb between two points in the bow's XY silhouette.
fn bow_limb(from: Vec2, to: Vec2, from_width: f32, to_width: f32) -> Mesh {
    let along = (to - from).normalize();
    let across = Vec2::new(-along.y, along.x);
    let [from_left, from_right] = [
        from + across * from_width / 2.0,
        from - across * from_width / 2.0,
    ];
    let [to_left, to_right] = [to + across * to_width / 2.0, to - across * to_width / 2.0];
    let point = |xy: Vec2, z: f32| Vec3::new(xy.x, xy.y, z);
    let near = -BOW_DEPTH / 2.0;
    let far = BOW_DEPTH / 2.0;
    let fln = point(from_left, near);
    let frn = point(from_right, near);
    let tln = point(to_left, near);
    let trn = point(to_right, near);
    let flf = point(from_left, far);
    let frf = point(from_right, far);
    let tlf = point(to_left, far);
    let trf = point(to_right, far);

    let mut build = MeshBuild::default();
    for face in [
        [fln, frn, trn, tln],
        [flf, tlf, trf, frf],
        [fln, tln, tlf, flf],
        [frn, frf, trf, trn],
        [fln, flf, frf, frn],
        [tln, trn, trf, tlf],
    ] {
        // The bow wears no livery, so every corner of it points at the neutral band.
        build.quad(face, [livery::neutral_uv(); 4]);
    }
    build.finish()
}

/// Two tapered curved limbs and a taut string, shared by held and dropped presentations.
pub(super) fn bow_mesh(length: f32) -> Mesh {
    let centre = Vec2::new(-BOW_LENGTH * 0.24, 0.0);
    let lower_tip = Vec2::new(0.0, -BOW_LENGTH / 2.0);
    let upper_tip = Vec2::new(0.0, BOW_LENGTH / 2.0);
    let mut bow = bow_limb(centre, lower_tip, BOW_STAVE, BOW_STAVE * 0.55);
    let upper = bow_limb(centre, upper_tip, BOW_STAVE, BOW_STAVE * 0.55);
    let string = Mesh::from(Cuboid::from_size(Vec3::new(
        BOW_STAVE * 0.22,
        BOW_LENGTH,
        BOW_DEPTH * 0.28,
    )));
    merge_all(&mut bow, [upper, string], "bow stave and string");
    bow.scaled_by(Vec3::splat(length / BOW_LENGTH))
}

/// A wooden shaft and its small green focus, shared by held and dropped presentations.
pub(super) fn sceptre_mesh(length: f32) -> Mesh {
    let scale = length / SCEPTRE_LENGTH;
    let mut shaft = tinted(
        Mesh::from(Cuboid::from_size(Vec3::new(
            SCEPTRE_SHAFT,
            SCEPTRE_LENGTH,
            SCEPTRE_SHAFT,
        ))),
        items::item_linear_rgba(ITEM_WOODEN_SCEPTRE),
    );
    let focus = tinted(Mesh::from(Sphere::new(SCEPTRE_ORB_RADIUS)), SCEPTRE_GREEN)
        .translated_by(Vec3::Y * (SCEPTRE_LENGTH / 2.0 + SCEPTRE_ORB_RADIUS * 0.55));
    merge_all(&mut shaft, [focus], "wooden sceptre");
    shaft.scaled_by(Vec3::splat(scale))
}

/// A closed fist: **one cube**, filling exactly [`HAND_SIZE`].
///
/// **Three iterations modelled digits into this box and none of them could be seen.** #175
/// put a knuckle row over the top 30%, #384 called the result a slab anyway, #388 replaced
/// the knuckles with four fingers and a thumb, and #391 added a wrist step below. The reason
/// was already written down beside [`WRIST_WIDTH`], as a fact about the *material* rather
/// than about anatomy: the view model's material is `unlit`, so nothing in this composition
/// is shaded, every skin-coloured face is exactly the same colour, and relief on a box 24
/// millimetres across renders as nothing at all. An unlit mesh in one flat colour has no
/// visible interior edges. Six boxes were drawn and one silhouette arrived.
///
/// So the digits are gone, and with them [`fist_mesh`]'s only reason to be more than a
/// primitive (#396). **A voxel world draws a hand as a block**, and the two channels that
/// carry anything under an `unlit` material are the silhouette and the vertex tint — a cube
/// spends nothing on the channel that renders as nothing and keeps the cue that has always
/// worked: a block at the end of a narrower limb, with the wrist step where the eye looks
/// for one. [`WRIST_WIDTH`] is that step and
/// [`the_wrist_steps_in_from_the_fist_in_the_projected_outline`] measures it on screen.
///
/// **It is still a function rather than an inlined `Cuboid`.** [`held_mesh`] puts this exact
/// mesh first in the buffers for an empty hand and for every [`ItemShape`], which is what
/// [`the_same_fist_is_present_whatever_the_hand_holds`] reads, and half a dozen tests measure
/// the fist on its own through it.
///
/// **And a solid cube is what now hides the grip.** The palm this replaces was three quarters
/// of the box deep with gaps between the digits standing proud of it, so the convex solid
/// doing the occluding was the palm and it cleared the grip by 0.2 mm. The cube clears it by
/// 5 mm — see the containment assertion beside [`GRIP_SIZE`].
fn fist_mesh() -> Mesh {
    Mesh::from(Cuboid::from_size(HAND_SIZE))
}

/// The wrist: the short section under the fist, in the same skin the fist takes from the
/// authoritative appearance.
///
/// **Why the limb exists at all.** #384 was right to forbid it — the defect there was the
/// fist's proportion and geometry below it would have hidden that instead of fixing it — but
/// the oversized box was also the only thing reaching the bottom of the viewport, so
/// correcting the proportion left the hand hanging in mid air with the world visible
/// underneath it (#389). This is that boundary being lifted now that the fist above it is
/// right, and it still decides nothing about the fist: [`HAND_SIZE`] and [`item_translation`]
/// are read here and written elsewhere.
///
/// **Its section is the fist's own, and that is #396 simplifying it rather than changing
/// it.** It was the *palm's* — `PALM_DEPTH` deep and pushed back to `PALM_CENTRE_Z` — so the
/// digit band would stand proud of the limb exactly as it stood proud of the palm, an arm
/// reaching the digits' near plane having swallowed the thumb at the bottom of that band.
/// There is no digit band any longer, so the limb carries the cube's full depth on the cube's
/// own centre plane, and the two boxes are flush front and back. That flushness is what the
/// old placement was buying on the far face alone and it matters more than it sounds: the
/// whole composition is at positive `X`, so a *shallower* box projects further outboard, and
/// an arm shallower than the fist leaves a sliver of world along the fist's inboard edge —
/// the same defect #389 was fixing, one scale down.
///
/// **The wrist stayed here and the forearm did not**, which is #394 splitting the limb at the
/// one joint that never moves. This box is the join: it is buried [`ARM_OVERLAP`] in the fist
/// and it is the whole of the silhouette step [`WRIST_WIDTH`] exists for, so it belongs beside
/// the hand it steps in from. What hangs below it has a *length* now rather than a size — see
/// [`forearm_mesh`] and [`drawn_arm_reach`].
fn wrist_mesh() -> Mesh {
    let wrist_top = -HAND_SIZE.y / 2.0 + ARM_OVERLAP;
    let wrist_bottom = wrist_top - WRIST_LENGTH;
    Mesh::from(Cuboid::from_size(Vec3::new(
        HAND_SIZE.x * WRIST_WIDTH,
        WRIST_LENGTH,
        HAND_SIZE.z,
    )))
    .translated_by(Vec3::new(
        LIMB_OUTBOARD_OFFSET,
        (wrist_top + wrist_bottom) / 2.0,
        0.0,
    ))
}

/// The forearm proper: **one bar, one unit long, hanging from its own origin.**
///
/// It is drawn at no particular length here because it no longer has one. [`ARM_REACH`] is
/// what it reaches at the model's resting depth and [`drawn_arm_reach`] is what it reaches in
/// any other frame; [`forearm_transform`] turns that into the `Y` scale this bar is drawn
/// with. A unit bar is what makes the length a number the animation can carry rather than a
/// span in a vertex buffer, and a vertex buffer is the one place it must not be — the limb is
/// merged into a shared mesh asset, so a per-frame length there would be an asset write per
/// frame.
///
/// **It runs straight down the model's own axis.** Angling it outboard toward the nearer
/// right edge was the alternative #389 named, and it was measured: at 16:9 the bottom edge is
/// reached first from every corner of the limb, so the lean buys no field of view, and it
/// pulls the arm off the fist's inboard edge. See [`REST_PITCH_RADIANS`].
fn forearm_mesh() -> Mesh {
    Mesh::from(Cuboid::from_size(Vec3::new(
        HAND_SIZE.x * FOREARM_WIDTH,
        1.0,
        HAND_SIZE.z,
    )))
    .translated_by(Vec3::new(LIMB_OUTBOARD_OFFSET, -0.5, 0.0))
}

/// How far the forearm reaches below the view model's origin, for a composition the
/// animations have carried `along_view` from its resting depth.
///
/// **The arm keeps the length it is drawn at, not the length it is modelled at.** The whole
/// composition translates along the view — a cast carries it [`CAST_REACH`]
/// away, a punch [`MINE_PUNCH_DISTANCE`], a placement bump back toward the eye — and the
/// frustum widens with distance while a constant [`ARM_REACH`] does not. That is the whole of
/// #394: at `0.11` further out the arm subtends a third less of the frame, so its end cap
/// stops short of the bottom edge and a player sees where their arm ends. Scaling the reach
/// by the depth ratio cancels exactly that term, and the limb covers the same band of screen
/// in every frame of every animation.
///
/// **It only ever grows, and the clamp is the near plane's half of the bargain.** The two
/// bounds on this number are imposed by *different* animations — the frame's bottom edge
/// binds when the model is pushed away, the near plane when an overhead cut swings it toward
/// the eye — which is why a length derived from `along_view` is satisfiable where a constant
/// is not. Coming toward the eye there is nothing to win: [`ARM_REACH`] already spends 97% of
/// the near plane's ceiling at the resting depth, and shrinking the arm on a placement bump
/// would buy near-plane headroom nothing needs at the cost of 2° of the bottom edge on a
/// lateral slash. So the ratio is floored at one, and the arm the near plane has to survive
/// is the same arm it survives today.
fn drawn_arm_reach(along_view: f32) -> f32 {
    // `along_view` is added to the model's `Z` and [`BASE_DEPTH`] is negative, so
    // reaching *away* from the eye is a negative offset over a negative base: the ratio is
    // the model's depth over its resting depth, and it is above one exactly when the
    // animation has pushed the composition out.
    ARM_REACH * (1.0 + along_view / BASE_DEPTH).max(1.0)
}

/// The forearm's own transform, under the hand it hangs from.
///
/// The translation is [`FOREARM_TOP`] — fixed, so the join with the wrist cannot open — and
/// the `Y` scale is the length [`forearm_mesh`]'s unit bar is drawn at, chosen so the bar's
/// far end lands exactly [`drawn_arm_reach`] below the composition's origin.
///
/// **The `Y` scale is why the forearm is a child entity, and this file's preference is against
/// that.** `every_held_shape_is_one_mesh_one_material_and_one_transform` says why: a part
/// parented separately is a second thing to keep in step with a swing, and it can look right
/// while animating wrong. A *child* of the view model is the one shape of second entity that
/// does not have that failure mode — Bevy composes the parent's transform into it, so the
/// swing, the bump, the punch and the blade's near-plane offset all reach it by construction,
/// and the only thing this transform carries of its own is the one scalar the parent's
/// transform has no room for. The alternatives were measured and are recorded on
/// [`drawn_arm_reach`] and [`ARM_REACH`]: a constant length cannot satisfy both bounds, and
/// rewriting the merged mesh every frame is an asset write per frame.
fn forearm_transform(animation: &HandAnimation) -> Transform {
    Transform::from_translation(Vec3::Y * FOREARM_TOP).with_scale(Vec3::new(
        1.0,
        FOREARM_TOP + drawn_arm_reach(along_view(animation)),
        1.0,
    ))
}

/// One cross-section of the blade: where it sits along the sword, how far it reaches to
/// either edge, and how thick it is through the central ridge.
///
/// **The blade is lofted from three of these**, which is what "bevelled" means in a form the
/// renderer can hold: knife-thin at both edges and full thickness only along a central flat.
#[derive(Debug, Clone, Copy)]
struct BladeSection {
    y: f32,
    half_width: f32,
    half_thickness: f32,
}

impl BladeSection {
    /// The six corners of the section, in order around its perimeter.
    ///
    /// **The order is load-bearing rather than a convention.** [`MeshBuild::quad`] takes the
    /// outward normal from the corners it is handed, so walking a section the other way
    /// round turns the whole blade inside out — visible only as a sword that vanishes when
    /// you look at it, which is the failure that costs the most to diagnose.
    fn perimeter(self) -> [Vec3; 6] {
        let Self {
            y,
            half_width: w,
            half_thickness: t,
        } = self;
        let ridge = w * BLADE_RIDGE_FRACTION;
        [
            Vec3::new(0.0, y, w),
            Vec3::new(t, y, ridge),
            Vec3::new(t, y, -ridge),
            Vec3::new(0.0, y, -w),
            Vec3::new(-t, y, -ridge),
            Vec3::new(-t, y, ridge),
        ]
    }
}

/// The buffers one hand-authored mesh is accumulated into.
///
/// **Hand-authored positions rather than merged primitives, and only for the blade.** The
/// guard, the grip and the pommel are boxes and stay boxes; a bevelled section that tapers
/// to a point is not something `Cuboid`, `Cone` or `ConicalFrustum` can express — a cone is
/// round and a frustum is round, and what this needs is a hexagon that narrows in width
/// faster than in thickness. `world/render.rs` builds the entire terrain this way, so the
/// mechanism is the established one rather than a new one.
#[derive(Debug, Default)]
struct MeshBuild {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
}

impl MeshBuild {
    /// One flat-shaded quad, wound around its perimeter.
    ///
    /// Flat rather than smooth, deliberately: six faces per span that are each shaded
    /// separately is the whole reason the section is a hexagon, and averaging the normals at
    /// the ridge would put a soft gradient exactly where the highlight should break. The
    /// normal written here is what [`shaded`] reads, so flat shading is load-bearing rather
    /// than a preference.
    fn quad(&mut self, corners: [Vec3; 4], uvs: [[f32; 2]; 4]) {
        let [a, b, c, d] = corners;
        // From the diagonals rather than from one triangle's two edges: a quad lofted
        // between sections of different widths is not exactly planar, and the diagonals
        // give the normal both of its triangles are nearest to instead of the first one's.
        let normal = (c - a).cross(d - b).normalize_or_zero();
        let first = self.push(corners.into_iter().zip(uvs), normal);
        self.indices
            .extend([first, first + 1, first + 3, first + 1, first + 2, first + 3]);
    }

    /// One flat-shaded polygon, as a fan from its first corner.
    ///
    /// The corners must already be wound so that `normal` is the outward one; the caller
    /// reverses them for the end that faces the other way.
    fn fan(&mut self, corners: [Vec3; 6], normal: Vec3) {
        // The cap is never seen — the root is buried in the guard and the tip is a tenth of
        // a section — so it is pointed at the neutral band, where a coordinate that carries
        // no information cannot pick up a colour it did not ask for.
        let first = self.push(corners.into_iter().zip([livery::neutral_uv(); 6]), normal);
        for corner in 1..corners.len() as u32 - 1 {
            self.indices
                .extend([first, first + corner, first + corner + 1]);
        }
    }

    /// Appends vertices sharing one normal, and answers the index the first of them landed
    /// at.
    fn push(&mut self, corners: impl Iterator<Item = (Vec3, [f32; 2])>, normal: Vec3) -> u32 {
        let first = self.positions.len() as u32;
        for (corner, uv) in corners {
            self.positions.push(corner.to_array());
            self.normals.push(normal.to_array());
            self.uvs.push(uv);
        }
        first
    }

    /// The three attributes and the indices, as the asset the renderer draws.
    ///
    /// **All three attributes, and that is not decoration.** `Mesh::merge` walks the
    /// attributes of the mesh being merged *into* and silently skips any the other side
    /// lacks, which leaves the buffers different lengths rather than raising — so a blade
    /// missing `ATTRIBUTE_UV_0` would merge with a `Cuboid` guard and corrupt it quietly.
    fn finish(self) -> Mesh {
        Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs)
        .with_inserted_indices(Indices::U32(self.indices))
    }
}

/// One mesh with every vertex pointed at the neutral band of the livery image.
///
/// **Every mesh a liveried material draws needs this, not merely the un-liveried ones.** The
/// first-person hand is one mesh and one material — fist, wrist, arm and held item — so the
/// moment that material carries a `base_color_texture`, a texture coordinate stops being
/// decoration on a cuboid nobody samples. Bevy's primitives generate coordinates spanning
/// the whole image, which would wrap the rusty sword's oxide around the player's knuckles.
///
/// One white texel is identity for a multiplier, so an un-liveried mesh draws exactly what
/// it drew before the image existed.
/// [`every_held_arrangement_samples_only_the_livery_it_owns`] is the sweep that holds it.
fn neutral(mut mesh: Mesh) -> Mesh {
    let vertices = mesh.count_vertices();
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![livery::neutral_uv(); vertices]);
    mesh
}

/// Where the blade starts: the top of the guard, in the sword's own space.
///
/// The sword is centred on its own origin, exactly as every `Cuboid` in this file is, so
/// that swapping the held mesh moves nothing about where the hand sits.
fn blade_base() -> f32 {
    -SWORD_LENGTH / 2.0 + POMMEL_SIZE.y + GRIP_SIZE.y + GUARD_SIZE.y
}

/// The centre of the grip in a sword mesh built at `length`.
///
/// Where the hand closes, and what the first-person arrangement is derived from:
/// [`item_translation`] seats this point at the fist's centre, which is what makes "guard
/// flush above" and "pommel entirely below" the same statement as `HAND_SIZE.y ==
/// GRIP_SIZE.y` rather than two placements tuned to agree.
///
/// The *body* attachment is seated by [`sword_guard_base`] instead — that fist is more than
/// twice the grip's height, so seating it by the grip would swallow the guard — and this is
/// what its tests measure that seating against. The two renderers therefore hold a sword by
/// two different points of one model sheet, deliberately, because their fists are not the
/// same size.
pub(super) fn sword_grip_centre(length: f32) -> Vec3 {
    let grip = blade_base() - GUARD_SIZE.y - GRIP_SIZE.y / 2.0;
    Vec3::Y * grip * (length / SWORD_LENGTH)
}

/// The face of the cross guard the grip enters, in a sword mesh built at `length`.
///
/// [`blade_base`] is the guard's *other* face, the one the blade leaves from. Hang a sword
/// point-down and the two swap places, so this one is what a fist seats against — asked for
/// here rather than reconstructed from a span, for the same reason [`sword_grip_centre`] is.
pub(super) fn sword_guard_base(length: f32) -> Vec3 {
    Vec3::Y * (blade_base() - GUARD_SIZE.y) * (length / SWORD_LENGTH)
}

/// The blade's root and tip in a sword mesh built at `length`.
///
/// The body attachment tests project this actual model-sheet segment against the arm. A
/// bounding-box extreme can be one protruding point and says nothing about how much blade
/// is readable, while these two points let that test measure the complete blade length.
#[cfg(test)]
pub(super) fn sword_blade_span(length: f32) -> [Vec3; 2] {
    let scale = length / SWORD_LENGTH;
    [
        Vec3::Y * blade_base() * scale,
        Vec3::Y * (blade_base() + BLADE_LENGTH) * scale,
    ]
}

/// The two ends of the cross guard in a sword mesh built at `length`.
#[cfg(test)]
pub(super) fn sword_guard_span(length: f32) -> [Vec3; 2] {
    let scale = length / SWORD_LENGTH;
    let y = (blade_base() - GUARD_SIZE.y / 2.0) * scale;
    let half_width = GUARD_SIZE.z / 2.0 * scale;
    [
        Vec3::new(0.0, y, -half_width),
        Vec3::new(0.0, y, half_width),
    ]
}

/// The three sections the blade is lofted from: at the guard, at the shoulder where the
/// point begins, and at the tip.
fn blade_sections() -> [BladeSection; 3] {
    let base = blade_base();
    let half_width = BLADE_WIDTH / 2.0;
    let half_thickness = BLADE_THICKNESS / 2.0;
    [
        // Sunk into the guard by [`BLADE_TANG`], so the blade's own end cap is inside the
        // guard's volume rather than flush with its top face.
        BladeSection {
            y: base - BLADE_TANG,
            half_width,
            half_thickness,
        },
        // The shoulder. The blade has narrowed to [`POINT_WIDTH_FRACTION`] by here and is
        // still full thickness: a gladius is waisted long before it is pointed.
        BladeSection {
            y: base + BLADE_LENGTH - POINT_LENGTH,
            half_width: half_width * POINT_WIDTH_FRACTION,
            half_thickness,
        },
        // The tip, where both give way together.
        BladeSection {
            y: base + BLADE_LENGTH,
            half_width: half_width * POINT_WIDTH_FRACTION * POINT_TIP_FRACTION,
            half_thickness: half_thickness * POINT_TIP_FRACTION,
        },
    ]
}

/// The section the blade has at a given height, interpolated along the loft.
///
/// Read by [`blade_rings`], so a ring anywhere along a subdivided blade is the section the
/// blade actually has there rather than the one it has at the guard.
fn blade_at(y: f32) -> BladeSection {
    let [root, shoulder, tip] = blade_sections();
    let (lower, upper) = if y <= shoulder.y {
        (root, shoulder)
    } else {
        (shoulder, tip)
    };
    let along = ((y - lower.y) / (upper.y - lower.y)).clamp(0.0, 1.0);
    let between = |from: f32, to: f32| from + (to - from) * along;
    BladeSection {
        y,
        half_width: between(lower.half_width, upper.half_width),
        half_thickness: between(lower.half_thickness, upper.half_thickness),
    }
}

/// How far the blade's surface stands off its mid-plane, `z` across a given section.
///
/// Flat at [`BLADE_THICKNESS`] over the ridge, then falling away linearly to nothing at the
/// edge — the bevel, read as a number.
///
/// **Test-only, and deliberately not what the loft is built from.** The loft walks the
/// hexagon's corners and interpolates along its straight sides, which *is* this function by
/// another route; keeping the closed form beside it gives
/// [`no_pit_leaves_the_blades_envelope`] a second opinion to measure the displaced vertices
/// against rather than re-deriving the arithmetic that placed them.
#[cfg(test)]
fn blade_surface(section: BladeSection, z: f32) -> f32 {
    let ridge = section.half_width * BLADE_RIDGE_FRACTION;
    let across = z.abs();
    if across <= ridge {
        section.half_thickness
    } else {
        section.half_thickness * (section.half_width - across) / (section.half_width - ridge)
    }
}

/// The turned grip alone, at model scale and in the sword's own space.
///
/// **Turned, not planed** — inscribed in the box it replaces, so the assertions that box takes
/// part in are unchanged. See [`GRIP_SIZE`] and [`GRIP_SIDES`].
///
/// **It is a function rather than three lines inside [`sword_with`] because the world draws it
/// separately.** The first-person hand is one mesh under one material and reaches its wood by
/// dividing [`palette::LOG`] out of the blade's own steel; a drop cannot, because `drops.rs`
/// caches one mesh per shape and livery and shares it between blades. So the world takes this
/// mesh on its own and gives it an absolute wood material — see [`sword_grip_mesh`].
fn grip_mesh() -> Mesh {
    // **It wears the wood, on both surfaces.** The hand reaches `palette::LOG` by division and
    // the world by an absolute material, but the *grain* is the same field read from the same
    // band either way — which is what stops a grip being wood in one place and grained in the
    // other. See [`livery::wear`].
    livery::wear(
        Mesh::from(CylinderMeshBuilder::new(
            GRIP_SIZE.x / 2.0,
            GRIP_SIZE.y,
            GRIP_SIDES,
        ))
        .translated_by(Vec3::Y * (base_below_the_guard())),
        Livery::Wood,
    )
}

/// Where the grip's centre sits along the sword, in its own space.
fn base_below_the_guard() -> f32 {
    blade_base() - GUARD_SIZE.y - GRIP_SIZE.y / 2.0
}

/// The tint that lands a grip on [`palette::LOG`] over one blade's own steel.
///
/// **Computed rather than written down, and that is the whole of why it is correct for more
/// than one blade.** A vertex colour *multiplies* the item colour, so a mesh can only reach
/// what is darker than its item in every channel; a single hard-coded multiplier would give
/// the iron sword a different wood from the rusty one, because its steel is brighter. The
/// division lands every blade's grip on exactly `palette::LOG`.
///
/// `#59636D` and `#593D28` happen to share a red channel to the sixth decimal, so worn
/// steel divides to `1.000 / 0.374 / 0.139`. That is a coincidence; the other two channels
/// are what make this work at all.
///
/// **`None` is the case that cannot be reached by multiplying**: a blade darker than
/// [`palette::LOG`] in any channel has no tint that would get there. The caller draws steel
/// and says so in the log rather than drawing a colour that is quietly not wood —
/// [`no_known_blade_needs_a_wood_it_cannot_reach`] is what keeps that path unreached today.
fn wood_over(item_colour: [f32; 4]) -> Option<[f32; 4]> {
    let log = palette::linear_rgba(palette::LOG);
    let mut tint = [1.0; 4];
    for channel in 0..3 {
        if item_colour[channel] <= 0.0 || log[channel] > item_colour[channel] {
            return None;
        }
        tint[channel] = log[channel] / item_colour[channel];
    }
    Some(tint)
}

/// A gladius: a bevelled blade that tapers to a point, a cross guard, a grip and a pommel,
/// merged into one mesh at whatever length the caller draws it.
///
/// **One mesh, for the reason [`tool_mesh`] and [`fist_mesh`] are one each**: the view model
/// is a single entity with a single transform that `animate_view_model` drives, and a guard
/// parented separately would be a second thing to keep in step with a swing.
///
/// **The length is a parameter because two renderers draw this weapon and they must draw the
/// same one.** `player/drops.rs` calls it too, at drop scale. That is deliberately *not* the
/// shared-mesh arrangement its `drop_mesh` note rules out — each surface still mints its own
/// asset, at its own size, with its own materials — it is the shape being one answer instead
/// of two that somebody has to keep in step, which is exactly the relationship
/// `player/items.rs` already has with its readers.
/// The sword the **world** builds: no livery, and **no grip**, which the drop draws as a
/// child of its own since #435.
///
/// **Test-only, and the name says which surface it is.** It was `sword_mesh` and that had
/// stopped meaning anything: every surface asks for a sword *by livery* since #418, and by
/// which half of it since #435. Four tests were reaching for "the sword" and getting the
/// world's, which is not the one the hand holds.
#[cfg(test)]
fn world_sword(length: f32) -> Mesh {
    sword_mesh_with(length, None)
}

/// The sword the **hand** builds for one item: its livery, its grip, and its wood.
#[cfg(test)]
fn held_sword(item_id: u16) -> Mesh {
    item_mesh(item_id, ItemShape::Blade)
}

/// The same weapon at a drop's scale, wearing whatever livery its item does — **and without
/// its grip**.
///
/// **The world draws a sword as two meshes, because its grip is not steel.** `drops.rs` caches
/// one mesh per shape *and livery*, shared by every item presenting as that pair, so a wood
/// tint divided out of one blade's steel would be right for that sword and quietly wrong for
/// the next one to share the pair — which is why #419 shipped the turned grip without the
/// wood. Handing the grip out separately, for the world to draw with an absolute
/// `palette::LOG` material, answers it without touching a cache key: the grip's colour stops
/// being a function of the blade's.
///
/// The first-person hand still gets one mesh with one material and still reaches its wood by
/// division — see [`sword_with`]. Two surfaces, two arrangements, one shape.
pub(super) fn sword_mesh_with(length: f32, livery: Option<Livery>) -> Mesh {
    sword_with(length, livery, None)
}

/// The turned grip on its own, at a drop's scale.
///
/// The second half of what [`sword_mesh_with`] leaves out, for the world to draw in wood.
pub(super) fn sword_grip_mesh(length: f32) -> Mesh {
    grip_mesh().scaled_by(Vec3::splat(length / SWORD_LENGTH))
}

/// One ring of the lofted blade: where it sits, and how far along the blade that is.
///
/// `along` runs 0 at the root cap to 1 at the tip and is what [`livery::field`] is asked
/// about, so it is carried beside the section rather than recomputed at each of the six
/// faces.
#[derive(Debug, Clone, Copy)]
struct BladeRing {
    section: BladeSection,
    along: f32,
}

/// The rings the blade is lofted through, at the resolution one livery asks for.
///
/// **The shoulder always lands on a ring**, whatever the step counts are, because the two
/// spans are stepped separately rather than the whole blade being divided evenly. A ring
/// that straddled the shoulder would cut the corner off the waist, which is the one crease
/// in this shape a player can see.
fn blade_rings(steps_root: u32, steps_point: u32) -> Vec<BladeRing> {
    let sections = blade_sections();
    let (root, tip) = (sections[0].y, sections[2].y);
    let span = tip - root;
    let mut rings = Vec::with_capacity((steps_root + steps_point + 1) as usize);
    let mut push = |y: f32| {
        rings.push(BladeRing {
            section: blade_at(y),
            along: (y - root) / span,
        });
    };
    for step in 0..=steps_root {
        push(root + (sections[1].y - root) * step as f32 / steps_root as f32);
    }
    for step in 1..=steps_point {
        push(sections[1].y + (tip - sections[1].y) * step as f32 / steps_point as f32);
    }
    rings
}

/// One ring's perimeter, subdivided and eaten into by the livery it wears.
///
/// Answers the positions and the coordinate around the perimeter each of them carries. The
/// coordinate runs to exactly 1.0 at the seam rather than wrapping to 0, so the image is
/// walked once across the whole blade; the *positions* wrap, which is why the two are
/// separate arrays rather than one.
fn ring_perimeter(ring: BladeRing, steps: u32, livery: Option<Livery>) -> (Vec<Vec3>, Vec<f32>) {
    let corners = ring.section.perimeter();
    let count = corners.len() as u32 * steps;
    let mut positions = Vec::with_capacity(count as usize);
    let mut around = Vec::with_capacity(count as usize + 1);
    for face in 0..corners.len() {
        let next = (face + 1) % corners.len();
        for step in 0..steps {
            let fraction = step as f32 / steps as f32;
            let mut point = corners[face].lerp(corners[next], fraction);
            let at = (face as u32 * steps + step) as f32 / count as f32;
            if let Some(livery) = livery {
                // **In `x` alone**, which is what keeps the outline the outline: the two
                // corners that sit on the blade's edges have no `x` to lose, so a pit can
                // only ever eat through a flat. See [`pit_depth`].
                point.x *= 1.0 - livery::pit_depth(livery) * livery::field(livery, at, ring.along);
            }
            positions.push(point);
            around.push(at);
        }
    }
    around.push(1.0);
    (positions, around)
}

/// The blade, lofted through its rings and dressed in whatever livery it wears.
///
/// **A blade with no livery is today's blade, to the vertex.** One step per span and one
/// step per face is the two-span, six-face loft this file drew before liveries existed, and
/// the coordinates it carries are the neutral band's — so an iron sword sampled through a
/// material that carries the rust image is the iron sword it always was. That property is
/// why the subdivision is reached through the livery rather than through an item id, and
/// [`the_iron_sword_is_the_blade_it_was_before_the_livery`] measures it.
fn blade_loft(livery: Option<Livery>) -> Mesh {
    // **Subdivided only where something displaces it.** A livery that takes nothing out of
    // the metal has nothing to pit with, so its blade is the two-span six-face loft an
    // un-liveried one is — which is what keeps the iron sword `sword_mesh` in both states,
    // to the third decimal of its volume, while it wears a surface.
    let (steps_root, steps_point, steps_around) = if pit_depth(livery) > 0.0 {
        (BLADE_STEPS_ROOT, BLADE_STEPS_POINT, BLADE_STEPS_AROUND)
    } else {
        (1, 1, 1)
    };
    let rings = blade_rings(steps_root, steps_point);

    let mut build = MeshBuild::default();
    let mut lower = ring_perimeter(rings[0], steps_around, livery);
    for pair in rings.windows(2) {
        let [low_ring, high_ring] = pair else {
            unreachable!("windows(2) yields pairs")
        };
        let upper = ring_perimeter(*high_ring, steps_around, livery);
        let uv = |at: f32, along: f32| match livery {
            Some(livery) => livery::blade_uv(livery, at, along),
            None => livery::neutral_uv(),
        };
        let sides = lower.0.len();
        for corner in 0..sides {
            let next = (corner + 1) % sides;
            build.quad(
                [
                    lower.0[corner],
                    lower.0[next],
                    upper.0[next],
                    upper.0[corner],
                ],
                [
                    uv(lower.1[corner], low_ring.along),
                    uv(lower.1[corner + 1], low_ring.along),
                    uv(upper.1[corner + 1], high_ring.along),
                    uv(upper.1[corner], high_ring.along),
                ],
            );
        }
        lower = upper;
    }

    // The two ends. The root's winding is reversed because its face looks the other way,
    // and a cap wound like the tip's would be culled from outside and visible from within.
    // They stay the section's own six corners whatever the loft is subdivided to: a cap
    // nobody sees has nothing to gain from being a fan of forty.
    let sections = blade_sections();
    let mut root_cap = sections[0].perimeter();
    root_cap.reverse();
    build.fan(root_cap, Vec3::NEG_Y);
    build.fan(sections[2].perimeter(), Vec3::Y);
    build.finish()
}

/// A gladius wearing one livery, or none.
/// A gladius wearing one livery, or none.
///
/// **`item_colour` decides which surface is asking, and therefore whether the grip is in
/// here at all.**
///
/// `Some` is the first-person hand: it mints an asset per selected item, so it knows which
/// blade this is, the grip is merged in, and its wood is reached by dividing
/// [`palette::LOG`] out of that blade's own steel.
///
/// `None` is the world. `drops.rs` caches **one mesh per shape and livery**, shared by every
/// blade and coloured by a per-item material, so a tint divided out of one steel would be
/// right for that sword and silently wrong for the next one to share the pair. The grip is
/// therefore **left out** and handed over separately by [`sword_grip_mesh`], for the world to
/// draw with an absolute wood material — which needs no cache key to change and no division
/// at all.
fn sword_with(length: f32, livery: Option<Livery>, item_colour: Option<[f32; 4]>) -> Mesh {
    let base = blade_base();
    let blade = blade_loft(livery);

    // The furniture, down from the base. Each sits directly under the last: two solid parts
    // meeting on a plane present that plane's two faces back to back, and a back-facing face
    // is culled — which is why *these* joins need no overlap and the blade's root, whose cap
    // would face the same way as the guard's, does.
    //
    // **The furniture maps into the neutral band of the livery image**, which is what keeps
    // the whole sword one material. A second material for the guard, the grip and the pommel
    // would be a second draw for a weapon that is one entity with one transform.
    let guard = neutral(
        Mesh::from(Cuboid::from_size(GUARD_SIZE))
            .translated_by(Vec3::Y * (base - GUARD_SIZE.y / 2.0)),
    );
    let grip = grip_mesh();
    let pommel = neutral(
        Mesh::from(Cuboid::from_size(POMMEL_SIZE))
            .translated_by(Vec3::Y * (base - GUARD_SIZE.y - GRIP_SIZE.y - POMMEL_SIZE.y / 2.0)),
    );

    // **The wood is a tint on the grip alone, so the whole sword needs the attribute.**
    // `Mesh::merge` refuses to join a mesh carrying an attribute to one that does not, and
    // white is identity — the steel that comes through everywhere else is whatever
    // `player/items.rs` says this blade presents as.
    let identity = [1.0; 4];
    let wood = item_colour.map(|colour| {
        wood_over(colour).unwrap_or_else(|| {
            // Unreached for every blade this build has a row for, and the log is the point:
            // a grip left in steel is visibly not wood, where a clamped tint would be a
            // colour nobody chose. See [`wood_over`].
            error!("no tint reaches palette::LOG over a blade colour of {colour:?}");
            identity
        })
    });
    let sword = match wood {
        // The hand: the grip is in the mesh, and its wood is a tint over this blade's steel.
        Some(tint) => {
            let mut sword = tinted(blade, identity);
            merge_all(
                &mut sword,
                [
                    tinted(guard, identity),
                    tinted(grip, tint),
                    tinted(pommel, identity),
                ],
                "sword",
            );
            sword
        }
        // The world: the grip is drawn as its own mesh, in its own material.
        None => {
            let mut sword = blade;
            merge_all(&mut sword, [guard, pommel], "sword");
            sword
        }
    };

    // Uniform, so the normals computed above stay unit vectors — `Mesh::scale_by` leaves
    // them alone for exactly that case and rebuilds them for every other.
    sword.scaled_by(Vec3::splat(length / SWORD_LENGTH))
}

/// One mesh with a baked directional shade folded into its vertex colours.
///
/// **The first-person composition only.** `drops.rs` mints a *lit* material, so the same
/// meshes already show their facets on the ground and baking a second light into them would
/// add to the real one. Applying this where the arrangement is composed rather than where
/// the geometry is built is what keeps the two surfaces apart with no flag to thread and no
/// way to get it wrong — see [`the_dropped_sword_is_not_shaded_twice`].
///
/// It multiplies, so it composes with everything already in the buffer: the item's own
/// colour, a grip's wood, a bundle's straps. Hemispheric rather than clamped at zero, so the
/// value moves continuously across every angle instead of flattening out the moment a face
/// turns past ninety degrees — a blade rolling in the hand should not have its bevels snap.
fn shaded(mut mesh: Mesh) -> Mesh {
    let Some(VertexAttributeValues::Float32x3(normals)) = mesh.attribute(Mesh::ATTRIBUTE_NORMAL)
    else {
        // Every mesh this module composes carries Float32x3 normals — `MeshBuild` writes
        // them and every Bevy primitive generates them. Leaving the mesh alone is the
        // cosmetic, non-fatal direction to fail in, which is what `coloured` does too.
        return mesh;
    };
    let light = SHADE_LIGHT.normalize();
    let shades: Vec<f32> = normals
        .iter()
        .map(|normal| {
            let facing = Vec3::from_array(*normal).normalize_or_zero().dot(light);
            SHADE_FLOOR + (1.0 - SHADE_FLOOR) * facing.mul_add(0.5, 0.5)
        })
        .collect();

    let colours: Vec<[f32; 4]> = match mesh.attribute(Mesh::ATTRIBUTE_COLOR) {
        Some(VertexAttributeValues::Float32x4(tints)) => tints
            .iter()
            .zip(&shades)
            .map(|(tint, shade)| [tint[0] * shade, tint[1] * shade, tint[2] * shade, tint[3]])
            .collect(),
        _ => shades.iter().map(|s| [*s, *s, *s, 1.0]).collect(),
    };
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colours);
    mesh
}

fn tinted(mesh: Mesh, colour: [f32; 4]) -> Mesh {
    let vertices = mesh.count_vertices();
    mesh.with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, vec![colour; vertices])
}

/// One wire colour as the linear vertex value Bevy's PBR shader consumes.
///
/// Character colours are `0x00RRGGBB` in sRGB, while item colours have already been
/// resolved to linear values by `player/items.rs`. Keeping the conversion at this boundary
/// gives both sources exactly one interpretation.
fn linear_rgb(colour: u32) -> [f32; 4] {
    let linear = Color::srgb_u8(
        ((colour >> 16) & 0xFF) as u8,
        ((colour >> 8) & 0xFF) as u8,
        (colour & 0xFF) as u8,
    )
    .to_linear();
    [linear.red, linear.green, linear.blue, linear.alpha]
}

/// Applies an item's resolved colour to a mesh, preserving any relative vertex tint it
/// already carries.
///
/// Every item mesh in this module receives the resolved colour whole today — the rusty
/// blade carried white and a rust tint until its oxide became a texture, and the multiply
/// below is what that arrangement needed. It is kept because the livery is a multiplier for
/// exactly the same reason: `player/items.rs` stays the one answer to what the steel is, and
/// a mesh that ever carries a relative tint again must not lose it here.
fn coloured(mut mesh: Mesh, base: [f32; 4]) -> Mesh {
    let colours = match mesh.attribute(Mesh::ATTRIBUTE_COLOR) {
        Some(VertexAttributeValues::Float32x4(tints)) => tints
            .iter()
            .map(|tint| std::array::from_fn(|channel| tint[channel] * base[channel]))
            .collect(),
        // Every mesh in this module either has no colour or a Float32x4 one. Replacing an
        // unexpected representation is the cosmetic, non-fatal direction to fail in.
        _ => vec![base; mesh.count_vertices()],
    };
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colours);
    mesh
}

/// One item-coloured roll with two brown leather straps.
fn coloured_bundle_mesh(base: [f32; 4]) -> Mesh {
    let (roll, straps) = rolled_bundle_parts(BUNDLE_SIZE);
    let mut bundle = tinted(roll, base);
    let straps = tinted(straps, bundle_strap_linear_rgba());
    merge_all(&mut bundle, [straps], "held packed-gear bundle");
    bundle
}

/// The geometry one held item contributes before it is arranged against the fist.
///
/// Exhaustive over [`ItemShape`], so a new shape does not compile until the hand can hold
/// it.
///
/// **There is no item-level exception left here.** The oxide was reached by
/// `if item_id == ITEM_RUSTY_SWORD` at the top of this function — the shape that does not
/// survive a second liveried item, and the reason three other renderers drew the same sword
/// clean. The livery is a fact `player/items.rs` answers now.
///
/// Everything that is not a liveried blade is pointed at the neutral band on the way out,
/// because the material this feeds carries the livery image whatever is held. See
/// [`neutral`].
fn item_mesh(item_id: u16, shape: ItemShape) -> Mesh {
    match shape {
        ItemShape::Blade => sword_with(
            SWORD_LENGTH,
            items::item_livery(item_id),
            Some(items::item_linear_rgba(item_id)),
        ),
        ItemShape::Block => neutral(Mesh::from(Cuboid::from_size(Vec3::splat(BLOCK_EDGE)))),
        ItemShape::Material => {
            neutral(Mesh::from(Capsule3d::new(MATERIAL_RADIUS, MATERIAL_LENGTH)))
        }
        ItemShape::Bundle => {
            let (mut roll, straps) = rolled_bundle_parts(BUNDLE_SIZE);
            merge_all(&mut roll, [straps], "held packed-gear bundle");
            neutral(roll)
        }
        ItemShape::Tool => neutral(tool_mesh()),
        ItemShape::Armour => neutral(armour_mesh()),
        ItemShape::Shield => neutral(shield_mesh(0.065)),
        ItemShape::Bow => neutral(bow_mesh(BOW_LENGTH)),
        ItemShape::Sceptre => neutral(sceptre_mesh(SCEPTRE_LENGTH)),
        // Turned a quarter about X so the struck face, not the rim, is what the camera sees.
        ItemShape::Coin => neutral(
            Mesh::from(Cylinder::new(COIN_RADIUS, COIN_THICKNESS))
                .rotated_by(Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
        ),
    }
}

/// Where an item sits relative to the fist at the origin.
///
/// Blocks, materials and bundles rest on the top of the fist. A sword is closed on by the
/// middle of its grip, on the fist's own centre plane, so the hand hides the grip it is
/// closed on. A tool puts the lower haft through the fist. These are translations of the
/// approved geometry, not new shapes.
///
/// **Nothing here has a depth of its own any more, and a blade least of all.** A blade carried
/// `BLADE_CAMERA_OFFSET` in `Z` — the whole sword one millimetre inside the fist's near face,
/// so every blade section would beat the hand in the depth test. #388 made that unnecessary by
/// seating the guard above the fist's top face, and left it doing the one thing nobody wanted:
/// standing the grip out in front of the hand that grips it (#393). The blade clears the fist
/// on **screen** now, which is a stronger property than winning a depth test and is measured
/// as one — see [`a_blade_rises_clear_of_the_fists_silhouette_instead_of_growing_out_of_it`].
fn item_translation(shape: ItemShape) -> Vec3 {
    let hand_top = HAND_SIZE.y / 2.0;
    let y = match shape {
        ItemShape::Block => hand_top + BLOCK_EDGE / 2.0 - HOLD_OVERLAP,
        ItemShape::Material => hand_top + MATERIAL_LENGTH / 2.0 + MATERIAL_RADIUS - HOLD_OVERLAP,
        // **The fist closes on the middle of the grip**, and the rest of the hilt's
        // arrangement falls out of that one statement rather than being placed by hand:
        // `HAND_SIZE.y == GRIP_SIZE.y` — pinned by the assertion beside the two constants —
        // so a centred grip is held along its whole length, the guard's lower face lands on
        // the fist's top face and the pommel is left entirely below the bottom one.
        //
        // It replaces `VISIBLE_GRIP`, which pushed a quarter of the grip out from under a
        // fist three times taller than the grip. There is nothing left for it to push
        // against: the pommel below is what says the hand is closed on a hilt rather than
        // being where the hilt begins (#384).
        ItemShape::Blade => -sword_grip_centre(SWORD_LENGTH).y,
        ItemShape::Bundle => hand_top + BUNDLE_SIZE.y / 2.0 - HOLD_OVERLAP,
        // The head stays above the hand and most of the haft remains visible below it.
        ItemShape::Tool => HAND_SIZE.y * 0.35,
        ItemShape::Armour => hand_top + ARMOUR_BODY_SIZE.y / 2.0 - HOLD_OVERLAP,
        // Cross the top of the fist so the carried shield is gripped, not floating.
        ItemShape::Shield => hand_top + 0.024,
        ItemShape::Bow => HAND_SIZE.y * 0.20,
        ItemShape::Sceptre => HAND_SIZE.y * 0.22,
        // Stood on the top of the fist by its radius, which is the block's and the stub's
        // arrangement: the coin is turned face-on, so its radius is its half height.
        ItemShape::Coin => hand_top + COIN_RADIUS - HOLD_OVERLAP,
    };
    Vec3::new(0.0, y, 0.0)
}

/// A wooden board and iron boss shared by hands, bodies and drops.
pub(super) fn shield_mesh(size: f32) -> Mesh {
    let mut board = tinted(
        Mesh::from(Cuboid::from_size(Vec3::new(size, size * 0.82, size * 0.10))),
        items::item_linear_rgba(super::crafting::ITEM_WOODEN_SHIELD),
    );
    let boss = tinted(
        Mesh::from(Cylinder::new(size * 0.17, size * 0.14))
            .rotated_by(Quat::from_rotation_x(std::f32::consts::FRAC_PI_2))
            .translated_by(Vec3::Z * size * 0.10),
        [0.55, 0.60, 0.66, 1.0],
    );
    merge_all(&mut board, [boss], "wooden shield");
    board
}

/// The first-person hand: the player's fist, the wrist it steps into and, when selected, the
/// item it holds, merged into one coloured mesh.
///
/// The fist is always first in the buffers. Besides making the mesh deterministic, that
/// gives the tests a structural way to assert that every shape still contains the exact
/// hand #175 approved instead of merely containing skin-coloured vertices somewhere. The
/// wrist follows it and the item comes last, so that guarantee is unchanged.
///
/// **The forearm is not in here since #394**, and [`forearm_transform`] carries the reason: it
/// is the one part of the composition whose *length* changes with the animation, and a length
/// that changes belongs in a transform rather than in a mesh asset that would then have to be
/// rewritten every frame. Everything it needs to stay in step with the hand — the swing, the
/// bump, the punch, the blade's near-plane offset — reaches it as the parent's transform.
///
/// **The arm takes the same skin colour from the same one source.** It is the same limb, so
/// a second lookup would be a second answer to a question the authoritative appearance has
/// already settled. [`skinned_forearm_mesh`] reads that same colour for the bar below.
fn held_mesh(skin_colour: u32, appearance: HeldAppearance) -> Mesh {
    let skin = linear_rgb(skin_colour);
    // Skin is never liveried, and it shares a material with whatever the hand is holding,
    // so both halves are pointed at the neutral band. See [`neutral`].
    let mut held = tinted(neutral(fist_mesh()), skin);
    merge_all(
        &mut held,
        [tinted(neutral(wrist_mesh()), skin)],
        "hand and wrist",
    );
    let (Some(item_id), Some(shape), Some(item_colour)) =
        (appearance.item_id, appearance.shape, appearance.item_colour)
    else {
        return shaded(held);
    };

    let item = if shape == ItemShape::Bundle {
        neutral(coloured_bundle_mesh(item_colour))
    } else if matches!(shape, ItemShape::Shield | ItemShape::Sceptre) {
        item_mesh(item_id, shape)
    } else {
        coloured(item_mesh(item_id, shape), item_colour)
    }
    .translated_by(item_translation(shape));
    merge_all(&mut held, [item], "hand and held item");
    // **Last, over the whole composition.** The fist, the wrist and the item are one mesh
    // under one `unlit` material, so one pass gives all three their relief — and applying it
    // after the merge rather than to each part is what stops a part being missed.
    shaded(held)
}

/// The forearm bar in the player's own skin.
///
/// One asset for both hands: the section, the colour and the unit length are identical, and
/// the only thing that differs between the right hand's arm and the off-hand shield's is the
/// transform each entity carries. A second asset would be a second answer to the same
/// authoritative skin colour.
fn skinned_forearm_mesh(skin_colour: u32) -> Mesh {
    // Its own entity, and the *same* material as the hand — so the neutral band and the
    // baked shade are both as load-bearing here as they are on the fist. An unshaded arm
    // under a shaded hand is the one seam this pass could leave.
    shaded(tinted(neutral(forearm_mesh()), linear_rgb(skin_colour)))
}

pub(super) struct HandsPlugin;

impl Plugin for HandsPlugin {
    fn build(&self, app: &mut App) {
        livery::register(app);
        app.init_resource::<HandAnimation>()
            .init_resource::<SelfVitals>()
            .init_resource::<LocalMount>()
            // `PlayerPlugin` owns the appearance cache in the game. Initialised here too
            // because the focused animation tests build this plugin on its own.
            .init_resource::<super::Appearances>()
            // `PlayerCameraPlugin` owns it in the game. Initialised here too so this module
            // stands up headlessly on its own — the same defence `player/target.rs`,
            // `player/combat.rs`, `player/crafting.rs`, `player/inventory.rs`,
            // `player/structures.rs` and `ui/crosshair.rs` each keep, and it is not
            // optional: a `Res<T>` with no resource takes the app down rather than reading
            // a default.
            .init_resource::<ViewMode>()
            // `BlockTargetPlugin` owns this one, and it is here for the same reason.
            .init_resource::<MiningFeedback>()
            .add_systems(Startup, spawn_view_model)
            .add_systems(
                Update,
                (
                    attach_to_view_model_camera,
                    ApplyDeferred,
                    refresh_held_item,
                    animate_view_model,
                    place_off_hand,
                )
                    .chain()
                    // After this frame's appearance message has been cached, so the fist
                    // takes the local player's skin colour on the same frame as their body.
                    .after(super::ApplySnapshots)
                    .after(ApplyInventory)
                    .after(ApplyTargetInput)
                    // After this frame's authoritative progress has been applied, so the
                    // punch starts and stops on the frame the server's answer changed
                    // rather than the one after it. `ApplyTargetInput` already implies it
                    // today — `player/target.rs` chains the two — but what this module
                    // requires is the progress, not the request that follows it, and an
                    // ordering it depends on should be one it states.
                    .after(ApplyMiningFeedback)
                    // After the swing is sent, so the feedback plays on the frame the
                    // request left rather than the one after it.
                    .after(super::combat::ApplyCombatInput),
            );
    }
}

/// The view model's current subject: which item it is drawing, and in what shape.
///
/// `None` in both fields is the empty hand — not an item with a missing entry, which is
/// why [`ItemShape`] has no variant for it and this field is an `Option` instead.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HeldItem {
    item_id: Option<u16>,
    shape: Option<ItemShape>,
    /// The player's own skin colour, so a late appearance message rebuilds the hand even
    /// when the selected slot did not move.
    skin_colour: u32,
}

#[derive(Component)]
struct ViewModel;

/// The origin-anchored camera that keeps view-model arithmetic in camera space.
///
/// Parenting the models to [`super::camera::WorldCamera`] first added their sub-block offsets to a moving
/// f32 world position, then the vertex shader subtracted that position again. At coordinates
/// in the thousands the discarded low bits became visible as movement. This camera and its
/// layer make the values consumed by the shader the same small values authored below.
#[derive(Component)]
struct ViewModelCamera;

/// The forearm hanging under one hand, as its own child entity.
///
/// It exists because its **length** is animated and its geometry is not — see
/// [`forearm_transform`] for why that puts it on a transform of its own, and why a child is
/// the one shape of second entity this composition can afford.
#[derive(Component)]
struct Forearm;

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct OffHandShield {
    skin_colour: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct HeldAppearance {
    item_id: Option<u16>,
    shape: Option<ItemShape>,
    item_colour: Option<[f32; 4]>,
}

#[derive(Resource, Debug)]
struct HandVisuals {
    /// The one mesh asset the entity draws. Its contents change only when the selected
    /// item or the local player's skin colour changes; the handle and entity stay put.
    mesh: Handle<Mesh>,
    shield_mesh: Handle<Mesh>,
    /// The forearm bar both hands draw. Its contents change only with the player's skin
    /// colour; its *length* is a scale on each arm's own transform, never a rewrite of this.
    forearm_mesh: Handle<Mesh>,
}

/// Which arc an attack draws.
///
/// **Presentation, and it is worth being exact about how far that goes.** The shape is chosen
/// in this module and [`swing_pose`] is its only reader; it reaches no request, no predicate
/// and no other module. `super::combat` routes the left button on the item id and sends the
/// same `AttackRequest` whichever arc is about to play, and the server judges the blow against
/// its own registry — so which picture played cannot change reach, damage, cooldown or what
/// was asked for. It is the rule `client/AGENTS.md` states for the item table, arriving by a
/// different door: drawing an item as a blade no more swings it than holding it as one does,
/// and drawing a cast reaches no further than drawing a cut.
///
/// **Three variants since #421, and the two that went were blade arcs.** The shape is now a
/// function of what is held rather than of a counter — a blade cuts, a bow draws, a sceptre
/// casts — which is why nothing in [`HandAnimation`] remembers what played last any more.
///
/// **Four since #626, and the fourth is not an attack.** [`Self::Eat`] plays on the frame a
/// `ConsumeRequest` left, so the paragraph above is now a statement about the three arcs a
/// *swing* can draw rather than about the whole enum. The paragraph before it is unchanged
/// and is the one that matters: which arc played still reaches nothing, and the request that
/// started it was already sent before the shape was chosen.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum SwingShape {
    /// Down and across: the one arc a blade draws, from the upper right to the lower left.
    #[default]
    Cut,
    /// The string hand drawing back. Chosen only for a bow request.
    Draw,
    /// A short forward presentation thrust, never a blade arc.
    Cast,
    /// The held item tipped back toward the eye and returned to rest: eating.
    ///
    /// **The one shape here that is not an attack**, which is worth saying because the field
    /// that carries it is called `attack` and the message that starts it is not `SwingSent`.
    /// What this enum has always been is *which arc the hand is playing*, and every argument
    /// in the type's own doc holds unchanged: the shape reaches no request and no predicate,
    /// `super::inventory` sends the same `ConsumeRequest` whether this plays or not, and the
    /// server re-reads its own `restoresHunger` column either way. A picture of eating no
    /// more feeds the player than a picture of a cut damages a draugr.
    Eat,
}

impl SwingShape {
    /// Every shape, for the sweeps that must cover the whole vocabulary.
    ///
    /// The same hand-written list, for the same reason, as `items::ItemShape::ALL`: no
    /// stable Rust enumerates variants. And as there, the list is not what makes a shape
    /// *drawn* — [`swing_pose`] matches with no wildcard arm, so a fifth variant fails to
    /// build until it has been given an arc of its own. What the list buys is the other
    /// half: a sweep that catches an arm filled in with a copy of its neighbour.
    ///
    /// `#[cfg(test)]` because nothing in the running client enumerates the shapes — each is
    /// chosen one at a time, from what is held or from which request left, and never from
    /// the set. That is where
    /// `ItemShape::ALL` also sat until a runtime reader turned up for it, and the day one
    /// turns up here the attribute comes off rather than the list changing.
    #[cfg(test)]
    const ALL: [Self; 4] = [Self::Cut, Self::Draw, Self::Cast, Self::Eat];
}

/// One attack swing in flight: which shape is playing, and how far into it the hand is.
///
/// The pair travels together because neither answers anything on its own — an elapsed time
/// with no shape draws nothing, and a shape with no elapsed time is a swing that is not
/// happening. Keeping them in one `Option` is what makes *no swing* a single state rather
/// than two fields that could disagree about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Swing {
    shape: SwingShape,
    elapsed: Duration,
}

/// How far one attack shape has carried the view model, as an offset from rest.
///
/// Four loose terms rather than a `Transform`, because they are *added* to whatever the
/// mining loop and the placement bump are already doing and two quaternions cannot be added.
/// Every term is zero at both ends of the arc, so a swing that finishes leaves the hand
/// exactly where it found it whichever shape played — which is the property
/// `a_sent_swing_moves_the_view_model_and_then_settles` has held since there was one arc,
/// and now holds four times over.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct SwingPose {
    /// About the camera's X axis. **Negative carries the blade over toward what is being
    /// hit** — the convention [`mine_punch`]'s caller set and the one this file keeps, so a
    /// third and a fourth animation never have to argue about which way *out* is.
    pitch: f32,
    /// About Y: across the view. Positive turns the blade toward -X, which is the far side
    /// of the screen from the hand — [`BASE_INBOARD`] puts it on the right — so a slash
    /// crosses the body instead of opening outward off the edge of the view.
    yaw: f32,
    /// About Z: the edge turning over.
    roll: f32,
    /// Along the view, in the same units as [`MINE_PUNCH_DISTANCE`]. **Negative reaches away
    /// from the camera**, toward what is being hit, for the same reason and on the same axis.
    reach: f32,
}

/// Where one shape has carried the hand, a given fraction of the way through its arc.
///
/// One envelope for every shape — `sin(fraction * PI)`, out and back, zero at both ends — and
/// one set of terms per shape to apply it to.
///
/// **The blade's arc moves three channels at once, and that is the shape rather than a blend
/// of shapes.** Until #421 each arc was mostly one degree of freedom and they were told apart
/// by which — the cut was pitch, the slash yaw, the thrust reach. A diagonal is not any of
/// those and cannot be described as one: it descends *and* crosses, and dropping either term
/// leaves a chop or a sweep.
/// [`the_blade_cuts_from_the_upper_right_down_to_the_lower_left`] is what holds that, and it
/// reads the tip's path on screen rather than the terms, because three non-zero numbers are
/// also what a chop with a wobble has.
///
/// The match is exhaustive with no wildcard arm, which is the compiler's half of the
/// guarantee: a fifth shape cannot be added without being given an arc of its own.
fn swing_pose(shape: SwingShape, elapsed: Duration) -> SwingPose {
    let fraction = (elapsed.as_secs_f32() / ATTACK_SWING_TIME.as_secs_f32()).clamp(0.0, 1.0);
    let arc = (fraction * PI).sin();
    match shape {
        SwingShape::Cut => SwingPose {
            pitch: -arc * CUT_PITCH_RADIANS,
            yaw: -arc * CUT_YAW_RADIANS,
            roll: arc * CUT_ROLL_RADIANS,
            ..default()
        },
        SwingShape::Draw => SwingPose {
            pitch: arc * 0.18,
            roll: arc * 0.28,
            // Back toward the string, while retaining enough near-plane clearance when a
            // placement bump and the draw begin in the same frame.
            reach: arc * 0.03,
            ..default()
        },
        SwingShape::Cast => SwingPose {
            pitch: -arc * 0.12,
            reach: -arc * CAST_REACH,
            ..default()
        },
        // Back and up toward the mouth rather than out toward anything. Two terms and no
        // roll: turning the edge over is what a stroke does, and nothing is being struck.
        SwingShape::Eat => SwingPose {
            pitch: arc * EAT_PITCH_RADIANS,
            reach: arc * EAT_RISE,
            ..default()
        },
    }
}

#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
struct HandAnimation {
    /// How long the mining loop has been running, and zero the moment it is not.
    ///
    /// **Local time under an authoritative gate, never a measure of the break.** It says
    /// where in a punch the hand is; how far along the block is, is a byte the server
    /// sends and `ui/crosshair.rs` draws. Nothing reads a break out of this field, which
    /// is what stops the animation from becoming a second opinion about one.
    mine_elapsed: Duration,
    bump_elapsed: Option<Duration>,

    /// The arc playing right now, if one is. Started by a `SwingSent` or a [`ConsumeSent`]
    /// message and by nothing else, so it plays exactly when a request left this client —
    /// whether that request later hits, misses, feeds anybody or is refused.
    ///
    /// **Still one field for two senders, and deliberately.** The arcs are mutually
    /// exclusive on screen — one composition, one transform — so two fields would be two
    /// things that could disagree about what the hand is doing, which is the very thing
    /// pairing the shape with its elapsed time in one `Option` exists to prevent. The name
    /// is the one cost, and it is a smaller one than a second animation slot.
    attack: Option<Swing>,
}

fn spawn_view_model(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    liveries: Res<livery::Liveries>,
) {
    commands.spawn((
        ViewModelCamera,
        Camera3d::default(),
        Camera {
            order: 1,
            clear_color: ClearColorConfig::None,
            ..default()
        },
        // Match the world's explicit tonemapper without requiring its LUT feature. The
        // material is unlit, but its colour still passes through the camera's tonemapper.
        Tonemapping::AcesFitted,
        RenderLayers::layer(VIEW_MODEL_RENDER_LAYER),
        Transform::default(),
    ));

    let appearance = selected_appearance(None);
    let skin_colour = PLACEHOLDER_APPEARANCE.skin_color();
    let mesh = meshes.add(held_mesh(skin_colour, appearance));
    let shield_appearance = HeldAppearance {
        item_id: Some(super::crafting::ITEM_WOODEN_SHIELD),
        shape: Some(ItemShape::Shield),
        item_colour: Some(items::item_linear_rgba(super::crafting::ITEM_WOODEN_SHIELD)),
    };
    let shield_mesh_handle = meshes.add(held_mesh(skin_colour, shield_appearance));
    let material = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        // **One material for the hand, the arm and every item it can hold**, exactly as
        // before — the livery is in the image rather than in the material, and everything
        // that wears no livery samples the white row of it. `base_color` stays identity so
        // the three multipliers in play read in one order: the item's own colour on the
        // vertices, the livery on the texels, and nothing here.
        base_color_texture: Some(liveries.material_image()),
        unlit: true,
        fog_enabled: false,
        // Positive renders closer. Together with the near-plane placement this prevents
        // terrain depth from slicing through the held arrangement.
        depth_bias: 1_000.0,
        ..default()
    });
    let forearm_mesh_handle = meshes.add(skinned_forearm_mesh(skin_colour));
    let visuals = HandVisuals {
        mesh: mesh.clone(),
        shield_mesh: shield_mesh_handle.clone(),
        forearm_mesh: forearm_mesh_handle.clone(),
    };

    // The arm each hand hangs. `Visibility` is left at its default `Inherited`, so a hand
    // hidden by the view toggle takes its own limb with it and there is no second thing to
    // gate — the same reasoning `refresh_held_item` gives for hiding rather than despawning.
    let arm = |mesh: Handle<Mesh>, material: Handle<StandardMaterial>| {
        (
            Forearm,
            Mesh3d(mesh),
            MeshMaterial3d(material),
            forearm_transform(&HandAnimation::default()),
            RenderLayers::layer(VIEW_MODEL_RENDER_LAYER),
            NotShadowCaster,
        )
    };

    commands
        .spawn((
            HeldItem {
                item_id: appearance.item_id,
                shape: appearance.shape,
                skin_colour,
            },
            ViewModel,
            Mesh3d(mesh),
            MeshMaterial3d(material.clone()),
            Transform::from_translation(base_translation(view_field_of_view(None))),
            Visibility::Hidden,
            RenderLayers::layer(VIEW_MODEL_RENDER_LAYER),
            NotShadowCaster,
        ))
        .with_child(arm(forearm_mesh_handle.clone(), material.clone()));
    commands
        .spawn((
            OffHandShield { skin_colour },
            ViewModel,
            Mesh3d(shield_mesh_handle),
            MeshMaterial3d(material.clone()),
            Transform::from_translation(shield_translation(view_field_of_view(None)))
                .with_rotation(Quat::from_rotation_z(-0.48)),
            Visibility::Hidden,
            RenderLayers::layer(VIEW_MODEL_RENDER_LAYER),
            NotShadowCaster,
        ))
        // The off-hand entity carries no animation of its own, so its arm never leaves the
        // resting length. It is still the same bar under the same transform, which is what
        // keeps the two hands one limb rather than two.
        .with_child(arm(forearm_mesh_handle, material));
    commands.insert_resource(visuals);
}

/// Attaches to the origin-anchored camera after the startup system has materialised it.
fn attach_to_view_model_camera(
    mut commands: Commands,
    cameras: Query<Entity, With<ViewModelCamera>>,
    unattached: Query<Entity, (With<ViewModel>, Without<ChildOf>)>,
) {
    let Some(camera) = cameras.iter().next() else {
        return;
    };
    for entity in &unattached {
        commands.entity(entity).insert(ChildOf(camera));
    }
}

/// The stable view-model handle and the asset whose contents it names, as one borrow.
///
/// Rebuilding the one asset in place avoids both a mesh cache keyed by arbitrary server
/// colours and a second entity. The render-world handle therefore stays stable through a
/// slot change while the hand and item remain one draw.
#[derive(SystemParam)]
struct HandAssets<'w> {
    visuals: Res<'w, HandVisuals>,
    meshes: ResMut<'w, Assets<Mesh>>,
}

/// The two facts that choose what the view model draws: the selected authoritative stack
/// and the local player's authoritative appearance.
///
/// They arrive on different streams and change independently, so keeping the lookup in one
/// parameter is what prevents a slot refresh from forgetting skin or an appearance refresh
/// from forgetting the item.
#[derive(SystemParam)]
struct HandSubject<'w> {
    inventory: Res<'w, Inventory>,
    selected: Res<'w, SelectedSlot>,
    session: Option<Res<'w, Session>>,
    appearances: Res<'w, super::Appearances>,
    mode: Res<'w, InputMode>,
    view: Res<'w, ViewMode>,
    mount: Res<'w, LocalMount>,
}

impl HandSubject<'_> {
    fn read(&self) -> (HeldAppearance, u32) {
        let appearance = selected_appearance(self.inventory.slot(self.selected.0));
        let skin_colour = self
            .session
            .as_deref()
            .and_then(|session| self.appearances.0.get(&session.0.entity_id))
            .map_or(PLACEHOLDER_APPEARANCE.skin_color(), |described| {
                described.appearance.skin_color()
            });
        (appearance, skin_colour)
    }
}

type HeldItemViewModelQuery<'w, 's> = Query<
    'w,
    's,
    (
        &'static mut HeldItem,
        &'static Mesh3d,
        &'static mut Visibility,
    ),
    Without<OffHandShield>,
>;

type OffHandShieldViewModelQuery<'w, 's> = Query<
    'w,
    's,
    (
        &'static mut OffHandShield,
        &'static Mesh3d,
        &'static mut Visibility,
    ),
    Without<HeldItem>,
>;

fn refresh_held_item(
    subject: HandSubject<'_>,
    mut assets: HandAssets<'_>,
    mut held: HeldItemViewModelQuery<'_, '_>,
    mut shields: OffHandShieldViewModelQuery<'_, '_>,
    vitals: Res<SelfVitals>,
) {
    let (selected, skin_colour) = subject.read();
    // The saddle composition owns both visible hands. Rebuild this hidden arrangement as
    // an empty fist while mounted so no selected item shape remains reachable through a
    // slot or appearance change; the authoritative stack is read again on dismount.
    let appearance = if subject.mount.mounted() {
        selected_appearance(None)
    } else {
        selected
    };
    let view_mesh = assets.visuals.mesh.clone();
    // **The view term, and it was missing.** This model is a child of the camera, sitting
    // [`base_translation`] in front of it — a first-person conceit and nothing else. #172
    // moved the camera four blocks back for the third-person view and gave every other such
    // conceit the term that removes it there: `InputGate::may_aim`, `InputGate::may_act`,
    // `ui::crosshair::show_crosshair` and `show_the_local_body`. This one was missed, so the
    // thing a player was holding floated between the camera and their own character (#194).
    //
    // Hidden rather than despawned, which is what the neighbouring test's name has always
    // said: a view toggle that removed the model would rebuild a mesh and a material on a
    // key press, and `animate_view_model` drives a transform on this same entity — so a
    // hidden model is a hidden animation, with nothing further to gate.
    let visible = if held_item_surface(
        *subject.mode,
        *subject.view,
        subject.session.is_some(),
        subject.mount.mounted(),
    ) == HeldItemSurface::ViewModel
    {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };

    for (mut item, mesh, mut visibility) in &mut held {
        if item.item_id != appearance.item_id
            || item.shape != appearance.shape
            || item.skin_colour != skin_colour
        {
            item.item_id = appearance.item_id;
            item.shape = appearance.shape;
            item.skin_colour = skin_colour;
            if mesh.0 != view_mesh {
                error!("the held entity no longer names the view-model mesh");
            } else if let Some(mut mesh) = assets.meshes.get_mut(&view_mesh) {
                *mesh = held_mesh(skin_colour, appearance);
            } else {
                error!("the held view-model mesh asset is missing");
            }
            // The arm is one asset shared by both hands and it carries the same one skin
            // colour, so it is rewritten here rather than a second time under the shield.
            // Its *length* is never written here — that is a scale on each arm's transform,
            // which is the whole point of it being a child entity.
            let forearm = assets.visuals.forearm_mesh.clone();
            if let Some(mut mesh) = assets.meshes.get_mut(&forearm) {
                *mesh = skinned_forearm_mesh(skin_colour);
            } else {
                error!("the forearm mesh asset is missing");
            }
        }
        if *visibility != visible {
            *visibility = visible;
        }
    }

    let shield_equipped = subject.session.as_deref().is_some_and(|session| {
        let params = session.0;
        params.equipment_slots >= 4
            && subject
                .inventory
                .slot(params.inventory_slots - params.equipment_slots + 3)
                .is_some_and(|stack| {
                    stack.item_id == super::crafting::ITEM_WOODEN_SHIELD
                        && stack.count > 0
                        && stack.durability > 0
                })
    });
    let shield_visible = if visible == Visibility::Visible
        && shield_equipped
        && vitals.get().is_some_and(|vitals| vitals.blocking)
    {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    let shield_mesh = assets.visuals.shield_mesh.clone();
    for (mut shield, mesh, mut visibility) in &mut shields {
        if shield.skin_colour != skin_colour {
            shield.skin_colour = skin_colour;
            if mesh.0 == shield_mesh
                && let Some(mut mesh) = assets.meshes.get_mut(&shield_mesh)
            {
                *mesh = held_mesh(
                    skin_colour,
                    HeldAppearance {
                        item_id: Some(super::crafting::ITEM_WOODEN_SHIELD),
                        shape: Some(ItemShape::Shield),
                        item_colour: Some(items::item_linear_rgba(
                            super::crafting::ITEM_WOODEN_SHIELD,
                        )),
                    },
                );
            }
        }
        *visibility = shield_visible;
    }
}

/// The presentation the selected stack asks for, or the empty hand.
///
/// Every fact in it comes from [`super::items`] — the one table the pack cells, the recipe
/// panel and the tooltip read too — so a stack cannot look like one thing in the hand and
/// another in the pack.
fn selected_appearance(stack: Option<crate::net::InventoryStack>) -> HeldAppearance {
    let Some(item_id) = stack_item_id(stack) else {
        return HeldAppearance {
            item_id: None,
            shape: None,
            item_colour: None,
        };
    };

    HeldAppearance {
        item_id: Some(item_id),
        shape: Some(items::item_shape(item_id)),
        item_colour: Some(items::item_linear_rgba(item_id)),
    }
}

/// The shape the first-person composition builds for one non-empty stack.
///
/// Test-only: `combat.rs` uses it to pin the presentation route to the blade-routing table.
/// The running client goes through [`selected_appearance`] with the real selected stack.
#[cfg(test)]
pub(super) fn drawn_item_shape(item_id: u16) -> ItemShape {
    selected_appearance(Some(crate::net::InventoryStack {
        item_id,
        count: 1,
        ..Default::default()
    }))
    .shape
    .expect("a non-empty stack has an item shape")
}

/// What the hand is reacting to this frame: one authoritative fact and two local presses.
///
/// A bundle rather than five parameters, for the reason [`HandAssets`] is one —
/// [`animate_view_model`] was already at clippy's argument bound, and *what is the hand
/// doing* is one question that should have one place to be asked. It is also where the
/// rule below is written down once, so the next animation this file grows has somewhere
/// to read the answer rather than somewhere to re-decide it.
#[derive(SystemParam)]
struct HandIntent<'w, 's> {
    mode: Res<'w, InputMode>,
    mount: Res<'w, LocalMount>,
    buttons: Option<Res<'w, ButtonInput<MouseButton>>>,
    target: Res<'w, BlockTarget>,
    feedback: Res<'w, MiningFeedback>,
    swings: MessageReader<'w, 's, SwingSent>,
    consumes: MessageReader<'w, 's, ConsumeSent>,
}

impl HandIntent<'_, '_> {
    /// Whether gameplay input counts this frame. A mode transition belongs to the UI for
    /// the whole of it, which is how `target::send_block_edits` reads the same thing.
    fn playing(&self) -> bool {
        *self.mode == InputMode::Playing && !self.mode.is_changed() && !self.mount.mounted()
    }

    /// **Whether the server says a block is coming apart under this crosshair right now,
    /// and the hand is on screen to be shown doing it.**
    ///
    /// [`MiningFeedback`] is the whole of the *gameplay* answer, and deliberately the whole
    /// of it. It holds a byte the server sent; it is cleared by the zero frame a server-side reset
    /// sends, cleared when the crosshair leaves the voxel that byte describes, and expired
    /// after `PROGRESS_SILENCE_TICKS` of silence. So *the block broke*, *the player looked
    /// away* and *the request was refused and nothing came back* are already one fact by
    /// the time it gets here, and not one of the three is this module's to work out.
    ///
    /// **The button is deliberately not in this predicate.** A held button is a request,
    /// not an outcome: a hand that punched on the press would be animating a break the
    /// server had not granted yet, which is the local clock this file must never grow —
    /// the same mistake as advancing progress locally, wearing a different hat. Reading
    /// the resource instead also keeps the two presentations of one fact in step, because
    /// `ui/crosshair.rs` fills its ring from this very resource: the hand and the ring
    /// start together, hold through the same silence, and stop together.
    ///
    /// **[`Self::playing`] is in it, and it is not a second opinion about mining.** It
    /// answers a different question — does this frame's hand belong to the world at all —
    /// and it is the same UI-state gate [`Self::placing`] takes and
    /// `target::send_block_edits` takes. All it can do is stop the punch being *drawn*
    /// while the pack or the pause menu owns the screen: it advances no progress, times no
    /// break, and decides nothing about whether one happened. Every question about what is
    /// coming apart still has exactly one answer, and it is the byte above.
    ///
    /// It has to be here rather than left to the crosshair, because the byte outlives the
    /// transition. Nothing orders [`super::ApplyInputMode`] before
    /// [`ApplyMiningFeedback`], so on the frame the mode changes the feedback can still be
    /// the one computed while the player was aiming — and the hand would go on punching
    /// behind an open inventory until the next frame's raycast reported nothing targeted.
    /// It is also what keeps the paragraph above true: `ui/crosshair.rs` hides its whole
    /// root on this same mode test, so without the term here the ring and the hand would
    /// stop on different frames — the one thing reading a shared resource was meant to
    /// prevent.
    fn mining(&self) -> bool {
        self.playing() && self.feedback.progress() != 0
    }

    /// A press that asked for a block somewhere there is room to put one.
    fn placing(&self) -> bool {
        self.playing()
            && self
                .buttons
                .as_deref()
                .is_some_and(|buttons| buttons.just_pressed(MouseButton::Right))
            && self.target.0.and_then(|hit| hit.place_target()).is_some()
    }

    /// Whether a swing request left this client this frame.
    fn swing_sent(&mut self) -> Option<u16> {
        let sent = self.swings.read().next().map(|swing| swing.item_id);
        if self.mount.mounted() { None } else { sent }
    }

    /// Whether a consume request left this client this frame.
    ///
    /// **The same reading as [`Self::swing_sent`], on the other request that draws an arc**:
    /// a message that was written because a frame was *queued*, not because a key was
    /// pressed. `super::inventory` writes it on `Sent::Queued` alone, so a press over an
    /// empty slot, over something this client does not route as food, into a full outbound
    /// queue or while a screen owns the input reaches here as nothing at all — which is the
    /// whole of "the animation does not play for a press that produced no request", and it
    /// is answered by there being no message rather than by a second copy of the predicate.
    ///
    /// It returns a `bool` where the swing returns an id, because there is one eating arc
    /// and nothing to route on. See [`ConsumeSent`].
    fn consume_sent(&mut self) -> bool {
        let sent = self.consumes.read().next().is_some();
        !self.mount.mounted() && sent
    }
}

fn animate_view_model(
    time: Res<Time>,
    mut intent: HandIntent<'_, '_>,
    mut animation: ResMut<HandAnimation>,
    mut held: Query<(Entity, &HeldItem, &mut Transform), Without<Forearm>>,
    mut forearms: Query<(&ChildOf, &mut Transform), With<Forearm>>,
    camera: Query<&Projection, With<ViewModelCamera>>,
) {
    let field_of_view = view_field_of_view(camera.iter().next());
    let mut next_animation = *animation;
    // The loop runs exactly while the server's answer says it should, and resets the
    // instant it does not — so a break, a look-away and a refusal all end it, without this
    // module knowing which of the three happened. Opening the pack ends it too, which is
    // the screen changing hands rather than a fourth thing the server said. See
    // [`HandIntent::mining`].
    if intent.mining() {
        next_animation.mine_elapsed += time.delta();
    } else {
        next_animation.mine_elapsed = Duration::ZERO;
    }

    // One swing per message, restarted rather than queued: two clicks inside one
    // animation should look like two swings, and the second server-side request is
    // refused by the cooldown either way.
    //
    // **This is where the shape is chosen, and it is the only place it is.** The cursor
    // advances on the request having left — the same message, on the same frame, that
    // starts the arc — so a swing that is refused, missed or answered by nothing at all
    // still moves the rotation on. Restarting a swing therefore takes the next shape too,
    // which is what makes two clicks inside one animation read as two swings rather than
    // as one arc that stuttered.
    // **Read before the swing, so the swing wins a frame that carries both.** Nothing pairs
    // them today — the left button and the consume key are two presses — but they share one
    // `may_act` gate and one frame, so a player can make both. A blow being answered is the
    // more urgent of the two things to show, and one composition can only draw one arc.
    if intent.consume_sent() {
        next_animation.attack = Some(Swing {
            shape: SwingShape::Eat,
            elapsed: Duration::ZERO,
        });
    }
    if let Some(item_id) = intent.swing_sent() {
        let shape = if item_id == ITEM_BOW {
            SwingShape::Draw
        } else if item_id == ITEM_WOODEN_SCEPTRE {
            SwingShape::Cast
        } else {
            SwingShape::Cut
        };
        next_animation.attack = Some(Swing {
            shape,
            elapsed: Duration::ZERO,
        });
    }
    if let Some(swing) = next_animation.attack.as_mut() {
        swing.elapsed += time.delta();
        if swing.elapsed >= ATTACK_SWING_TIME {
            next_animation.attack = None;
        }
    }
    if intent.placing() {
        next_animation.bump_elapsed = Some(Duration::ZERO);
    }
    if let Some(elapsed) = next_animation.bump_elapsed.as_mut() {
        *elapsed += time.delta();
        if *elapsed >= PLACE_BUMP_TIME {
            next_animation.bump_elapsed = None;
        }
    }
    // A mount entry is the authoritative cut between the two camera-space
    // compositions. Clear every in-flight hand arc on that frame: hiding the entity
    // alone would leave a mining loop or swing advancing behind the reins and make it
    // reappear part-way through on dismount.
    if intent.mount.mounted() {
        next_animation = HandAnimation::default();
    }
    if *animation != next_animation {
        *animation = next_animation;
    }

    // The one transform the hand's own arm carries. It is read once and written into the
    // child below rather than being a second reading of the animation.
    let arm = forearm_transform(&next_animation);
    for (entity, item, mut transform) in &mut held {
        let next = presented_transform(&next_animation, item.shape, field_of_view);
        if *transform != next {
            *transform = next;
        }
        // Only the hand that is animated lengthens its arm. The off-hand shield's entity
        // never moves along the view, so its limb is already at the resting length and
        // driving it from this animation would stretch an arm nothing had pushed away.
        for (child_of, mut limb) in &mut forearms {
            if child_of.parent() == entity && *limb != arm {
                *limb = arm;
            }
        }
    }
}

/// **The off-hand shield's placement, which is the only thing about it that moves.**
///
/// It carries no animation — [`animate_view_model`]'s note says why — so before #415 its
/// transform was written once at spawn and never again. That was correct while the height
/// was a constant and is not correct now: [`base_height`] follows the frame, and a main hand
/// that follows it while the off hand stays put is two hands at two heights the moment a
/// player moves the slider. This writes the one axis that derives and leaves the roll alone.
fn place_off_hand(
    camera: Query<&Projection, With<ViewModelCamera>>,
    mut shields: Query<&mut Transform, With<OffHandShield>>,
) {
    let translation = shield_translation(view_field_of_view(camera.iter().next()));
    for mut transform in &mut shields {
        if transform.translation != translation {
            transform.translation = translation;
        }
    }
}

fn presented_transform(
    animation: &HandAnimation,
    shape: Option<ItemShape>,
    field_of_view: f32,
) -> Transform {
    let mut transform = animated_transform(animation, field_of_view);
    if shape == Some(ItemShape::Blade) {
        transform.translation.z -= BLADE_NEAR_PLANE_CLEARANCE;
    }
    transform
}

/// How far through the placement bump's out-and-back the hand is.
fn bump_arc(bump_elapsed: Option<Duration>) -> f32 {
    bump_elapsed.map_or(0.0, |elapsed| {
        let fraction = (elapsed.as_secs_f32() / PLACE_BUMP_TIME.as_secs_f32()).clamp(0.0, 1.0);
        (fraction * PI).sin()
    })
}

/// How far the animations in flight have carried the whole composition along the view.
///
/// Three animations on one axis, and the signs are the convention rather than an accident: a
/// placement draws back from the block it just set down, a punch reaches for the one it is
/// breaking, and a thrust reaches the same way a punch does.
///
/// **It is a function rather than a local because two things spend it now.**
/// [`animated_transform`] translates the composition by it, and [`drawn_arm_reach`] turns it
/// into the length the forearm is drawn at — and those two have to be the same number, since
/// the arm's length exists precisely to cancel what this translation does to the arm's size on
/// screen. Recomputing the three terms here is a handful of trigonometry once a frame; two
/// copies of the sum would be two things to keep in step.
fn along_view(animation: &HandAnimation) -> f32 {
    let punch = mine_punch(animation.mine_elapsed);
    let reach = animation
        .attack
        .map_or(0.0, |attack| swing_pose(attack.shape, attack.elapsed).reach);
    bump_arc(animation.bump_elapsed) * PLACE_BUMP_DISTANCE - punch * MINE_PUNCH_DISTANCE + reach
}

fn animated_transform(animation: &HandAnimation, field_of_view: f32) -> Transform {
    let punch = mine_punch(animation.mine_elapsed);
    // Whichever arc is in flight, out and back, added to whatever the mining loop is doing.
    // The two never run together in practice — a blade suppresses mining — and summing
    // rather than branching keeps the transform one expression, which is what lets a third
    // and a fourth animation land here without a precedence rule.
    let swing = animation.attack.map_or_else(SwingPose::default, |attack| {
        swing_pose(attack.shape, attack.elapsed)
    });
    let bump = bump_arc(animation.bump_elapsed);

    Transform {
        translation: base_translation(field_of_view) + Vec3::Z * along_view(animation),
        // The mining punch is negative here for the reason `SwingPose::pitch` is negative
        // for a cut: one convention for *over toward what is being hit*, kept by every
        // animation in this file.
        rotation: Quat::from_rotation_x(
            REST_PITCH_RADIANS - punch * MINE_PUNCH_RADIANS + swing.pitch,
        )
        // Identity at rest and for two of the three shapes, so nothing about where the
        // hand sits or how it mines moves for the sake of the slash that needs it.
        * Quat::from_rotation_y(swing.yaw)
            * Quat::from_rotation_z(REST_ROLL_RADIANS - bump * 0.18 + swing.roll),
        ..default()
    }
}

/// How far through one punch the mining loop is: `0.0` at rest, `1.0` at full extension,
/// back to `0.0` at the end of the cycle, repeating.
///
/// `(1 - cos)/2` rather than a sine, and that is the difference between punching and
/// shaking. A sine is symmetric about rest, so half of every cycle drags the hand back
/// *behind* where it started; this never goes negative, so the loop only ever reaches out
/// and lets the hand return.
///
/// It is a function of local elapsed time and of nothing else. It is only ever consulted
/// while [`HandIntent::mining`] holds, and the caller zeroes its input the moment that
/// stops — so the phase says where in a punch the hand is, never how near the break is.
fn mine_punch(elapsed: Duration) -> f32 {
    let phase = elapsed.as_secs_f32() * MINE_PUNCHES_PER_SECOND * TAU;
    (1.0 - phase.cos()) * 0.5
}

#[cfg(test)]
mod tests {
    use bevy::asset::AssetPlugin;

    use bevy::mesh::VertexAttributeValues;
    use bevy::time::TimeUpdateStrategy;

    use super::super::camera::WorldCamera;
    use super::super::combat::ITEM_RUSTY_SWORD;
    use super::super::crafting::ITEM_IRON_SWORD;
    use super::super::target::BlockHit;
    use super::*;
    use crate::net::{
        Appearance as PlayerLook, AppearanceInbox, InventoryStack, MountKind, MountState,
        PlayerAppearance, SessionParams, Snapshot, SnapshotInbox,
    };
    use crate::player::items::{ITEM_LOG, ITEM_RAW_COAL, ITEM_RAW_IRON, ITEM_STONE};
    use crate::player::{PlayerPlugin, combat, crafting, structures};

    /// Deliberately unlike every item swatch, so skin vertices can be identified in a
    /// composite without mistaking part of the item for the hand.
    const TEST_SKIN: u32 = 0x00E3_C4A0;

    /// The field of view every measurement below is taken at unless it says otherwise.
    ///
    /// Read out of `settings` rather than written down, so a change to the default this hand
    /// was placed against fails here instead of silently re-framing it. Since #415 the
    /// placement follows the setting, so a test that names no field of view is a test about
    /// the default frame — and the ones that sweep the range say so in their names.
    fn default_fov() -> f32 {
        crate::settings::Settings::default()
            .field_of_view()
            .to_radians()
    }

    /// Every field of view the setting can reach, in its own steps.
    fn every_field_of_view() -> Vec<f32> {
        let mut settings = crate::settings::Settings::default();
        settings.adjust(crate::settings::Knob::FieldOfView, -1_000);
        let mut all = vec![settings.field_of_view()];
        loop {
            settings.adjust(crate::settings::Knob::FieldOfView, 1);
            let next = settings.field_of_view();
            if (next - all[all.len() - 1]).abs() < f32::EPSILON {
                break;
            }
            all.push(next);
        }
        all.into_iter().map(f32::to_radians).collect()
    }

    fn shape_examples() -> [(ItemShape, u16); ItemShape::ALL.len()] {
        [
            (ItemShape::Block, ITEM_STONE),
            (ItemShape::Material, ITEM_RAW_COAL),
            (ItemShape::Blade, ITEM_IRON_SWORD),
            (ItemShape::Bundle, structures::ITEM_TENT),
            (ItemShape::Tool, crafting::ITEM_SHOVEL),
            (ItemShape::Armour, crafting::ITEM_LEATHER_CAP),
            (ItemShape::Shield, crafting::ITEM_WOODEN_SHIELD),
            (ItemShape::Bow, crafting::ITEM_BOW),
            (ItemShape::Sceptre, crafting::ITEM_WOODEN_SCEPTRE),
            (ItemShape::Coin, items::ITEM_SILVER),
        ]
    }

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 1,
            spawn: [0.0; 3],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 3,
            inventory_slots: 5,
            hotbar_slots: 4,
            equipment_slots: 1,
            player_token: crate::net::ANY_TOKEN,
        })
    }

    /// The vertex colours one mesh carries, deduplicated and sorted so a failure reads the
    /// same way twice.
    fn tints(mesh: &Mesh) -> Vec<[u8; 4]> {
        let Some(VertexAttributeValues::Float32x4(colours)) = mesh.attribute(Mesh::ATTRIBUTE_COLOR)
        else {
            return Vec::new();
        };
        // Quantised, because these are compared for identity rather than measured and two
        // f32 that print the same must not sort apart.
        let mut seen: Vec<[u8; 4]> = colours
            .iter()
            .map(|c| c.map(|channel| (channel * 255.0).round() as u8))
            .collect();
        seen.sort_unstable();
        seen.dedup();
        seen
    }

    /// **The rusty sword is iron with rust eaten into it**, not one flat colour — and the
    /// rust is now a surface rather than fourteen boxes stuck to it.
    ///
    /// The oxide moved from vertex colours to an image, so what makes the blade rusty is no
    /// longer a second tint on the mesh: it is where the mesh's texture coordinates point.
    /// Both halves are asserted here, because either alone would pass while the sword drew
    /// clean — coordinates that reach the field with an image full of white, or an image
    /// full of rust nothing samples.
    #[test]
    fn the_rusty_sword_carries_iron_and_rust_on_one_mesh() {
        let rusted = sword_with(SWORD_LENGTH, Some(Livery::WornSteel), None);
        let plain = held_sword(ITEM_IRON_SWORD);

        // **The oxide is not among the vertex colours**, which is the claim, and it is no
        // longer the same thing as "there are no vertex colours". Since #419 a held blade
        // carries two: identity everywhere, and the tint that lands its grip on
        // `palette::LOG`. What must never appear is a third that is the rust.
        let rust_tinted = |mesh: &Mesh, item_id: u16| {
            let colour = items::item_linear_rgba(item_id);
            let wood = wood_over(colour).expect("both blades can reach the wood");
            tints(mesh)
                .into_iter()
                .filter(|tint| {
                    *tint != [255, 255, 255, 255]
                        && *tint != wood.map(|c| (c * 255.0).round() as u8)
                })
                .count()
        };
        assert_eq!(
            rust_tinted(&rusted, ITEM_RUSTY_SWORD),
            0,
            "the rusty blade carries a tint that is neither identity nor its grip's wood, so \
             the rust has two authorities"
        );
        assert_eq!(
            rust_tinted(&plain, ITEM_IRON_SWORD),
            0,
            "the iron blade carries a tint that is neither identity nor its grip's wood"
        );

        // **The rust reaches the mesh through the coordinates, and only the rusty blade's.**
        // "Only" is a band question rather than a neutrality one since #420: the iron sword
        // wears `ForgedSteel` and samples the image too, so what must never happen is either
        // blade reading the rows the *other* metal was written into.
        let neutral = livery::neutral_uv();
        let sampled = |mesh: &Mesh| -> Vec<[f32; 2]> {
            uvs(mesh).into_iter().filter(|uv| *uv != neutral).collect()
        };
        for (name, mesh, own, other) in [
            (
                "the rusty blade",
                &rusted,
                Livery::WornSteel,
                Livery::ForgedSteel,
            ),
            (
                "the iron blade",
                &plain,
                Livery::ForgedSteel,
                Livery::WornSteel,
            ),
        ] {
            let seen = sampled(mesh);
            assert!(
                !seen.is_empty(),
                "no vertex of {name} samples the livery, so the image is unread"
            );
            for uv in &seen {
                // Its steel or its grip's wood — a sword is two materials since #436 — and
                // never the other blade's metal, which is the claim this test exists for.
                assert!(
                    livery::band_holds(own, *uv) || livery::band_holds(Livery::Wood, *uv),
                    "{name} samples {uv:?}, outside its own steel and its grip's wood"
                );
                assert!(
                    !livery::band_holds(other, *uv),
                    "{name} samples {uv:?}, which is {other:?}'s band — it wears another \
                     blade's surface"
                );
            }
        }

        // And the coordinates cover the field rather than a corner of it. **Read as a span
        // and not per vertex**, because the blade has twelve quads and the shader samples
        // everywhere between them: the strongest rust at any *vertex* of this loft is about
        // 0.28, and a test that asked for more than that would be asking the mesh a
        // question only the rasteriser can answer. What the mesh is answerable for is that
        // the whole of the generated field lands on the blade, so nothing the generator
        // draws is somewhere the steel is not.
        let sampled: Vec<[f32; 2]> = uvs(&rusted)
            .into_iter()
            .filter(|uv| *uv != neutral)
            .collect();
        let span = |axis: usize| {
            sampled
                .iter()
                .fold((f32::MAX, f32::MIN), |(low, high), uv| {
                    (low.min(uv[axis]), high.max(uv[axis]))
                })
        };
        assert_eq!(
            span(0),
            (0.0, 1.0),
            "the blade walks part of the field around its perimeter rather than all of it"
        );
        assert_eq!(
            span(1),
            (
                livery::blade_uv(Livery::WornSteel, 0.0, 0.0)[1],
                livery::blade_uv(Livery::WornSteel, 0.0, 1.0)[1]
            ),
            "the blade walks part of the field along its length rather than all of it"
        );

        // The strength that span reaches, sampled the way the rasteriser will: a blade
        // whose coordinates all landed in the margin would satisfy every clause above and
        // draw clean steel.
        let strongest = (0..=64)
            .flat_map(|around| (0..=64).map(move |along| (around, along)))
            .map(|(around, along)| {
                livery::strength_at(
                    Livery::WornSteel,
                    livery::blade_uv(Livery::WornSteel, around as f32 / 64.0, along as f32 / 64.0),
                )
            })
            .fold(0.0_f32, f32::max);
        assert!(
            strongest > 0.9,
            "the strongest rust anywhere on the blade is {strongest}, so the freckles are \
             somewhere the blade is not"
        );

        assert!(
            rusted.count_vertices() > plain.count_vertices(),
            "the rusty blade is not subdivided, so it has nowhere to pit"
        );
    }

    /// Every texture coordinate one mesh carries.
    fn uvs(mesh: &Mesh) -> Vec<[f32; 2]> {
        let Some(VertexAttributeValues::Float32x2(uvs)) = mesh.attribute(Mesh::ATTRIBUTE_UV_0)
        else {
            panic!("the mesh must carry Float32x2 texture coordinates");
        };
        uvs.clone()
    }

    /// **The blade's flat and its two bevels are three different shades**, which is the
    /// property [`BLADE_RIDGE_FRACTION`]'s doc has always claimed and never had.
    ///
    /// > six side faces per span instead of four, so the light catches a different pair as
    /// > the hand turns
    ///
    /// There was no light to catch: the first-person material is `unlit`, so every face of
    /// the hexagon rendered one colour and the section might as well have been a rectangle.
    /// Measured off the real merged mesh rather than from the constants that built it,
    /// because the claim is about what reaches the screen.
    #[test]
    fn the_held_blade_shows_its_bevel() {
        let held = held_mesh(TEST_SKIN, blade_appearance(ITEM_IRON_SWORD));
        let steel = items::item_linear_rgba(ITEM_IRON_SWORD);
        let skin = linear_rgb(TEST_SKIN);

        // **The isolation is asserted rather than assumed.** This counts the levels the
        // *steel* is drawn at inside a composition that also holds a hand, and the hand has
        // six face shades of its own — so a filter that merely bounded the ratio could have
        // been measuring the hand's relief and reading it as the blade's.
        // [`levels_of`] separates them by colour *direction*, which is sound exactly as long
        // as the two colours are not parallel, and that is checkable.
        assert!(
            shade_of(steel, &skin).is_none(),
            "the test skin is a shade of the blade's steel, so the hand's own relief would \
             count as the blade's"
        );
        let levels = levels_of(&held, steel);
        assert!(
            levels.len() >= 3,
            "the blade is drawn at {} shading levels, so its flat and its bevels are one \
             face: {levels:?}",
            levels.len()
        );

        // And nothing is brighter than the colour `player/items.rs` gives it: a shade only
        // ever takes light away, which is what keeps that table the one authority.
        let brightest = levels.iter().copied().max().expect("some steel");
        assert!(
            brightest <= 10_010,
            "a face is drawn at {brightest} of the item's own colour, so shading has started \
             inventing light"
        );
        assert!(
            levels.iter().copied().min().expect("some steel") >= (SHADE_FLOOR * 1e4) as i32 - 10,
            "a face is darker than the floor, so a shade became a hole"
        );
    }

    /// **A pitted blade shows its pits**, which is the half of #426 that has been invisible
    /// since the day it merged.
    ///
    /// The displacement is in `x` alone and deliberately preserves the outline, so under a
    /// flat colour a pit changed nothing anybody could see — what showed in the hand was the
    /// livery's texture and only that. A displaced face has a normal of its own, so a baked
    /// shade is what turns the geometry back into something visible.
    #[test]
    fn a_pitted_blade_shows_the_pits_it_has() {
        // The blade's own vertices, split off the hand by colour direction — see
        // [`levels_of`], and the assertion in `the_held_blade_shows_its_bevel` that the two
        // colours are not parallel.
        let levels = |item_id: u16| {
            levels_of(
                &held_mesh(TEST_SKIN, blade_appearance(item_id)),
                items::item_linear_rgba(item_id),
            )
            .len()
        };
        let pitted = levels(ITEM_RUSTY_SWORD);
        let smooth = levels(ITEM_IRON_SWORD);
        assert!(
            pitted > smooth,
            "the pitted blade is drawn at {pitted} shading levels and the smooth one at \
             {smooth}, so the pits still reach nothing"
        );
    }

    /// **The dropped sword is not shaded twice.**
    ///
    /// `drops.rs` mints a *lit* material, so the same meshes already show their facets on the
    /// ground — baking a second light into them would add to the real one. The pass is
    /// applied where the first-person arrangement is composed rather than where the geometry
    /// is built, which keeps the two surfaces apart by construction; this is the assertion
    /// that says so, because "it is applied somewhere else" is a claim about a call site and
    /// call sites move.
    #[test]
    fn the_dropped_sword_is_not_shaded_twice() {
        for length in [SWORD_LENGTH, 0.05] {
            for livery in [None, Some(Livery::WornSteel), Some(Livery::ForgedSteel)] {
                let mesh = sword_mesh_with(length, livery);
                assert!(
                    mesh.attribute(Mesh::ATTRIBUTE_COLOR).is_none(),
                    "the world's sword mesh carries vertex colours, so a lit material would \
                     draw it shaded twice"
                );
            }
        }
    }

    /// **Every held arrangement is shaded**, swept over every item the client knows plus the
    /// empty hand and the arm.
    ///
    /// The pass is applied to a *composition* rather than to each part, and the failure that
    /// shape invites is one part missed — an unshaded arm under a shaded hand is the seam.
    ///
    /// **Read out of the colour buffer, never recomputed from the normals.** The first cut of
    /// this test derived each vertex's level from its normal and `SHADE_LIGHT`, which is
    /// `shaded`'s own arithmetic written twice: it passed with the whole pass stubbed out, and
    /// proved the formula with the formula. What it measures now is what the buffer holds —
    /// vertices grouped by the colour they are a shade *of*, and every group required to carry
    /// more than one magnitude.
    #[test]
    fn every_held_arrangement_carries_relief() {
        /// The distinct magnitudes each colour in a mesh is drawn at, one entry per colour.
        fn magnitudes(mesh: &Mesh) -> Vec<Vec<i32>> {
            let mut groups: Vec<([i32; 3], Vec<i32>)> = Vec::new();
            for tint in raw_tints(mesh) {
                let peak = tint[0].max(tint[1]).max(tint[2]);
                if peak <= f32::EPSILON {
                    continue;
                }
                // The colour's own direction, which a multiply leaves alone, and its
                // magnitude, which is the only thing a shade moves.
                let chroma = std::array::from_fn(|c| (tint[c] / peak * 1e3) as i32);
                let level = (peak * 1e4) as i32;
                match groups.iter_mut().find(|(seen, _)| *seen == chroma) {
                    Some((_, levels)) => levels.push(level),
                    None => groups.push((chroma, vec![level])),
                }
            }
            groups
                .into_iter()
                .map(|(_, mut levels)| {
                    levels.sort_unstable();
                    levels.dedup();
                    levels
                })
                .collect()
        }

        let arrangements = items::known_item_ids()
            .map(|item_id| {
                (
                    format!("item {item_id}"),
                    held_mesh(TEST_SKIN, blade_appearance(item_id)),
                )
            })
            .chain([
                (
                    "the empty hand".to_owned(),
                    held_mesh(TEST_SKIN, selected_appearance(None)),
                ),
                ("the forearm".to_owned(), skinned_forearm_mesh(TEST_SKIN)),
            ]);

        for (name, mesh) in arrangements {
            let groups = magnitudes(&mesh);
            assert!(!groups.is_empty(), "{name} carries no colour at all");
            for levels in groups {
                assert!(
                    levels.len() > 1,
                    "{name} draws one of its colours at a single level, so that part is flat"
                );
                let (dimmest, brightest) = (
                    *levels.first().expect("a level"),
                    *levels.last().expect("a level"),
                );
                // Nothing spreads further than the floor allows. A shade only ever takes
                // light away, so the dimmest is at worst `SHADE_FLOOR` of the brightest —
                // and that holds without this test knowing what the base colour was.
                assert!(
                    f64::from(dimmest) / f64::from(brightest) >= f64::from(SHADE_FLOOR) - 1e-3,
                    "{name} draws a colour from {dimmest} to {brightest}, past the \
                     {SHADE_FLOOR} floor"
                );
            }
        }
    }

    /// The appearance one blade is held in, for the shading measurements above.
    fn blade_appearance(item_id: u16) -> HeldAppearance {
        selected_appearance(Some(InventoryStack {
            item_id,
            count: 1,
            ..Default::default()
        }))
    }

    /// The scale one vertex colour is of a base colour, when it is a shade of it at all.
    ///
    /// **Read off the peak channel rather than off red**, which is what the first cut did. A
    /// base colour with no red divides to zero and would never be recognised as a shade of
    /// itself — latent for every colour in the tables today, and a false negative waiting for
    /// the first item that is blue or green. The peak channel is the one a multiply cannot
    /// lose.
    ///
    /// `None` when the direction does not match, which is what makes this a *filter*: two
    /// colours that are not parallel cannot be mistaken for shades of each other, so a
    /// composition carrying skin and steel can be split by asking this twice.
    fn shade_of(colour: [f32; 4], tint: &[f32; 4]) -> Option<f32> {
        let peak = (0..3).fold(0, |best, channel| {
            if colour[channel] > colour[best] {
                channel
            } else {
                best
            }
        });
        if colour[peak] <= f32::EPSILON {
            return None;
        }
        let scale = tint[peak] / colour[peak];
        (0..3)
            .all(|channel| (tint[channel] - colour[channel] * scale).abs() < 1e-3)
            .then_some(scale)
    }

    /// The distinct shading levels one base colour is drawn at within a composition.
    ///
    /// **Isolated by colour direction, not by a ratio range.** The first cut filtered on
    /// `tint[0] / steel[0]` landing between the floor and one, which the hand's own six face
    /// shades could satisfy — so the measurement could have been the *hand's* relief and read
    /// as the blade's. [`shade_of`] answers `None` for a colour that is not parallel, which is
    /// what makes this the blade's vertices and nothing else.
    fn levels_of(mesh: &Mesh, colour: [f32; 4]) -> Vec<i32> {
        let mut seen: Vec<i32> = raw_tints(mesh)
            .iter()
            .filter_map(|tint| shade_of(colour, tint))
            .map(|scale| (scale * 1e4) as i32)
            .collect();
        seen.sort_unstable();
        seen.dedup();
        seen
    }

    /// **The held grip reads the wood band**, off the hand's own composition rather than off
    /// the mesh the world takes.
    ///
    /// The two grips are the same `grip_mesh` at two scales, and scaling leaves texture
    /// coordinates alone — but "they must be the same because they come from one function" is
    /// a claim about a call graph. This reads the merged held sword and picks the grip out of
    /// it by radius, which is what the drop's own test cannot do.
    #[test]
    fn a_held_grip_reads_the_wood_band() {
        for item_id in [ITEM_RUSTY_SWORD, ITEM_IRON_SWORD] {
            let sword = item_mesh(item_id, ItemShape::Blade);
            let points = positions(&sword);
            let coordinates = uvs(&sword);
            let radius = GRIP_SIZE.x / 2.0;
            let high = blade_base() - GUARD_SIZE.y;
            let low = high - GRIP_SIZE.y;

            let mut grained = 0;
            for (point, uv) in points.iter().zip(&coordinates) {
                let on_the_grip = (point[0].hypot(point[2]) - radius).abs() < 1e-6
                    && point[1] >= low - 1e-6
                    && point[1] <= high + 1e-6;
                if !on_the_grip {
                    continue;
                }
                grained += 1;
                assert!(
                    livery::band_holds(Livery::Wood, *uv),
                    "item {item_id}'s held grip samples {uv:?}, outside wood's own band"
                );
            }
            assert_eq!(
                grained, GRIP_RING_VERTICES,
                "item {item_id}'s held grip is {grained} vertices, which is not the turned one"
            );
        }
    }

    /// The bands one held item may legitimately sample.
    ///
    /// **Its own material's, and wood** — because a blade is honestly two materials: the steel
    /// and the grip that is not steel. #420 anticipated exactly this shape ("or two, where a
    /// shape is honestly two materials") and #436 is where a sword became the first item to
    /// need it.
    ///
    /// What the sweeps using this actually catch is unchanged and is the thing that matters: a
    /// blade must never read the band the *other metal* was written into.
    fn bands_worn(item_id: u16) -> Vec<Livery> {
        let mut worn: Vec<Livery> = items::item_livery(item_id).into_iter().collect();
        if items::item_shape(item_id) == ItemShape::Blade && !worn.contains(&Livery::Wood) {
            worn.push(Livery::Wood);
        }
        worn
    }

    /// Every vertex colour one mesh carries, unquantised and in buffer order.
    ///
    /// [`tints`] quantises and deduplicates, which is right when colours are compared for
    /// identity. A baked shade makes them a continuum instead, so the tests that ask "is this
    /// a shade of that" need the values as they are.
    fn raw_tints(mesh: &Mesh) -> Vec<[f32; 4]> {
        let Some(VertexAttributeValues::Float32x4(colours)) = mesh.attribute(Mesh::ATTRIBUTE_COLOR)
        else {
            return Vec::new();
        };
        colours.clone()
    }

    /// Every vertex position one mesh carries.
    fn positions(mesh: &Mesh) -> Vec<[f32; 3]> {
        let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("the mesh must carry Float32x3 positions");
        };
        positions.clone()
    }

    /// **The forearm as the renderer places it, in the composition's own space.**
    ///
    /// [`forearm_mesh`] is a unit bar with no length of its own — the length lives in
    /// [`forearm_transform`], because it changes with the animation and a mesh asset must not
    /// — so a test that reads the bar without this is measuring a box one metre long rather
    /// than an arm. Everything below therefore composes the two the same way `spawn_view_model`
    /// parents them, which is what keeps these measurements statements about what is drawn.
    fn placed_forearm(animation: &HandAnimation) -> Mesh {
        forearm_mesh().transformed_by(forearm_transform(animation))
    }

    /// The whole limb-and-item arrangement as one set of model-space vertices: the hand's own
    /// mesh and the arm hanging under it, for one frame of one animation.
    fn drawn_positions(appearance: HeldAppearance, animation: &HandAnimation) -> Vec<[f32; 3]> {
        let mut all = positions(&held_mesh(TEST_SKIN, appearance));
        all.extend(positions(&placed_forearm(animation)));
        all
    }

    /// The lowest and highest value a set of vertices reaches on one axis.
    /// Every triangle of one mesh, projected through `pose` onto the plane the frame is
    /// measured in.
    ///
    /// **Triangles rather than bounding boxes or hulls**, because the question this answers is
    /// whether a pixel is on skin, and a bounding box says yes for the air beside a limb. It
    /// is the same reason `a_blade_rises_clear_of_the_fists_silhouette_instead_of_growing_out_of_it`
    /// gives for projecting real vertices — with the direction of the approximation reversed,
    /// since here a superset would make the test pass rather than fail.
    fn projected_triangles(mesh: &Mesh, pose: &Transform) -> Vec<[Vec2; 3]> {
        let positions = positions(mesh);
        let indices = mesh.indices().expect("a merged mesh carries indices");
        let project = |index: usize| {
            let point = pose.transform_point(Vec3::from_array(positions[index]));
            let depth = -point.z;
            assert!(depth > 0.0, "vertex {index} landed behind the camera");
            Vec2::new(point.x / depth, point.y / depth)
        };
        let corners: Vec<usize> = indices.iter().collect();
        corners
            .chunks_exact(3)
            .map(|corner| [project(corner[0]), project(corner[1]), project(corner[2])])
            .collect()
    }

    /// Whether a projected triangle covers a point, winding-agnostic: the merged mesh carries
    /// both faces of every box and one of each pair is wound away from the eye.
    fn contains(triangle: [Vec2; 3], point: Vec2) -> bool {
        let [a, b, c] = triangle;
        let side = |from: Vec2, to: Vec2| (to - from).perp_dot(point - from);
        let (first, second, third) = (side(a, b), side(b, c), side(c, a));
        let negative = first <= 0.0 && second <= 0.0 && third <= 0.0;
        let positive = first >= 0.0 && second >= 0.0 && third >= 0.0;
        negative || positive
    }

    /// The camera-space `Z` of the nearest surface of `mesh` along the view ray through the
    /// projected `point`, or `None` where the mesh does not cover it.
    ///
    /// **This is the question "is it drawn in front of the hand", and comparing against the
    /// mesh's nearest *vertex* is not that question.** The fist's nearest vertex is one corner
    /// of one box; a point hanging beside the hand's lower edge can be in front of that corner
    /// and still be nowhere near the surface the hand actually presents along its own ray.
    ///
    /// `1/z` rather than `z` is what gets interpolated, because that is the quantity that is
    /// affine in screen space for a plane in 3D — interpolating `z` itself would bend every
    /// triangle toward the eye and answer a question about a surface that is not there.
    fn nearest_surface_at(mesh: &Mesh, pose: &Transform, point: Vec2) -> Option<f32> {
        let positions = positions(mesh);
        let indices = mesh.indices().expect("a merged mesh carries indices");
        let corners: Vec<usize> = indices.iter().collect();
        let mut nearest: Option<f32> = None;
        for triangle in corners.chunks_exact(3) {
            let camera: [Vec3; 3] = std::array::from_fn(|corner| {
                pose.transform_point(Vec3::from_array(positions[triangle[corner]]))
            });
            let depth = camera.map(|corner| -corner.z);
            if depth.iter().any(|depth| *depth <= 0.0) {
                continue;
            }
            let flat: [Vec2; 3] = std::array::from_fn(|corner| camera[corner].xy() / depth[corner]);
            if !contains(flat, point) {
                continue;
            }
            let area = (flat[1] - flat[0]).perp_dot(flat[2] - flat[0]);
            if area.abs() < 1e-12 {
                continue;
            }
            let weight = [
                (flat[1] - point).perp_dot(flat[2] - point) / area,
                (flat[2] - point).perp_dot(flat[0] - point) / area,
                (flat[0] - point).perp_dot(flat[1] - point) / area,
            ];
            let reciprocal: f32 = (0..3).map(|corner| weight[corner] / depth[corner]).sum();
            if reciprocal <= 0.0 {
                continue;
            }
            let surface = -1.0 / reciprocal;
            nearest = Some(nearest.map_or(surface, |best: f32| best.max(surface)));
        }
        nearest
    }

    fn extent(positions: &[[f32; 3]], axis: usize) -> (f32, f32) {
        positions
            .iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(low, high), p| {
                (low.min(p[axis]), high.max(p[axis]))
            })
    }

    /// **The sword spends exactly the length the box did**, and is still centred on its own
    /// origin.
    ///
    /// This is the half most likely to break the swing tests without anybody noticing, so it
    /// is asserted twice over: once against the parts, so growing one has to take the length
    /// from another, and once against the mesh, so an arithmetic slip in the stacking cannot
    /// pass by agreeing with itself.
    #[test]
    fn the_sword_spends_exactly_the_length_the_box_did() {
        let parts = POMMEL_SIZE.y + GRIP_SIZE.y + GUARD_SIZE.y + BLADE_LENGTH;
        assert!(
            (parts - SWORD_LENGTH).abs() < 1e-6,
            "the pommel, grip, guard and blade come to {parts} against a budget of \
             {SWORD_LENGTH}"
        );

        let sword = positions(&held_sword(ITEM_IRON_SWORD));

        let (low, high) = extent(&sword, 1);
        assert!(
            (high - low - SWORD_LENGTH).abs() < 1e-5,
            "the sword spans {} on y, and SWORD_LENGTH says {SWORD_LENGTH}",
            high - low
        );
        assert!(
            (high + low).abs() < 1e-5,
            "the sword is not centred on its own origin: it spans {low}..{high}, so swapping \
             the held mesh would move where the hand sits"
        );
    }

    /// **A gladius rather than a bar**: a blade that narrows and thins to a point, bevelled
    /// from a central ridge, with a cross guard, a grip and a pommel under it.
    ///
    /// Every clause is a property rather than a vertex list. *Tapers* is the cross-section at
    /// the tip being smaller than at the guard on both axes, which is what "has a point"
    /// means in a form a test can read and which a box fails by construction. *Bevelled* is
    /// the section reaching its full thickness somewhere other than at its widest point,
    /// which a rectangular section fails in both directions.
    #[test]
    fn the_held_sword_is_a_gladius_and_not_one_box() {
        let sword = positions(&held_sword(ITEM_IRON_SWORD));

        let one_box = Mesh::from(Cuboid::from_size(Vec3::ONE)).count_vertices();
        assert!(
            sword.len() > one_box,
            "the sword is {} vertices, which is one box",
            sword.len()
        );

        // The vertices sitting on one horizontal plane, which is how a section is read out of
        // a merged mesh: the loft puts blade vertices at exactly three heights and the
        // furniture's boxes at four more, and no two of the seven coincide.
        let on = |y: f32| -> Vec<[f32; 3]> {
            let found: Vec<[f32; 3]> = sword
                .iter()
                .copied()
                .filter(|p| (p[1] - y).abs() < 1e-6)
                .collect();
            assert!(!found.is_empty(), "no vertex sits at y {y}");
            found
        };
        let across = |section: &[[f32; 3]]| section.iter().map(|p| p[2].abs()).fold(0.0, f32::max);
        let through = |section: &[[f32; 3]]| section.iter().map(|p| p[0].abs()).fold(0.0, f32::max);

        let [root, shoulder, tip] = blade_sections().map(|section| on(section.y));

        // It tapers, and twice over: waisted from the guard to the shoulder, then converging
        // in both axes at once over the point.
        assert!(
            across(&tip) < across(&shoulder) && across(&shoulder) < across(&root),
            "the blade does not narrow: {} at the guard, {} at the shoulder, {} at the tip",
            across(&root),
            across(&shoulder),
            across(&tip)
        );
        assert!(
            through(&tip) < through(&root),
            "the blade is {} thick at the tip against {} at the guard, so it ends in a chisel",
            through(&tip),
            through(&root)
        );

        // It is bevelled: thickest along a central ridge and knife-thin at both edges, so the
        // vertex reaching furthest *across* is not the one reaching furthest *through*.
        let widest = root.iter().copied().fold([0.0f32; 3], |best, p| {
            if p[2].abs() > best[2].abs() { p } else { best }
        });
        assert!(
            widest[0].abs() < through(&root) * 0.5,
            "the blade is {} thick at its widest point against {} at the ridge: the section is \
             a rectangle rather than a bevel",
            widest[0].abs(),
            through(&root)
        );
        let ridge: Vec<[f32; 3]> = root
            .iter()
            .copied()
            .filter(|p| (p[0].abs() - through(&root)).abs() < 1e-6)
            .collect();
        assert!(
            across(&ridge) < across(&root),
            "the ridge is as wide as the blade, so there is no bevel to run from"
        );

        // A cross guard wider than the blade, a grip narrower than it, and a pommel wider
        // than the grip. Each part meets its neighbour on a shared plane, so a joint carries
        // two widths — the part above it and the part below — and reading both is what tells
        // three stacked parts from one box of the right height.
        let base = blade_base();
        let widths = |y: f32| -> (f32, f32) {
            let plane = on(y);
            (
                plane
                    .iter()
                    .map(|p| p[2].abs())
                    .fold(f32::INFINITY, f32::min),
                across(&plane),
            )
        };
        let (_, guard) = widths(base);
        assert!(
            guard > across(&root),
            "the part on top of the grip reaches {guard} across against the blade's {}, so it \
             is not a cross guard",
            across(&root)
        );
        let (grip, _) = widths(base - GUARD_SIZE.y);
        assert!(
            grip < across(&root),
            "the part under the guard is {grip} across against a blade of {}, so there is no \
             grip for a hand to close on",
            across(&root)
        );
        let (heel, pommel) = widths(base - GUARD_SIZE.y - GRIP_SIZE.y);
        assert!(
            pommel > heel,
            "the grip runs into the bottom of the sword at {heel} with nothing wider under it, \
             so there is no pommel"
        );
    }

    /// **One mesh and one material for every hand-and-item arrangement, and one child under
    /// it: the arm.**
    ///
    /// The cost rule the body rig set and #175 kept, and the one a sword assembled from a
    /// extra entities would break quietly: they could look right and animate wrong, because
    /// `animate_view_model` drives one transform and a guard parented separately would be a
    /// second thing to keep in step with a swing.
    ///
    /// **#394 spent exactly one entity of that budget, and this is where the spending is
    /// recorded.** The forearm's *length* is animated — see [`drawn_arm_reach`] for why a
    /// constant one cannot satisfy both the near plane and the bottom edge — and a length that
    /// changes every frame can live in a transform or in a mesh asset, of which the second is
    /// an asset write per frame. What makes a child affordable where a sibling would not be is
    /// that the failure mode above is not available to it: Bevy composes the parent's transform
    /// into the child's, so the swing, the bump, the punch and the blade's near-plane offset
    /// all reach the arm whether anybody remembered them or not, and the only thing the child
    /// carries of its own is the one scalar the parent's transform has no room for.
    ///
    /// So the rule is not relaxed, it is made exact: **the hand and everything it holds are one
    /// mesh on one transform, and the arm is one child on one scale.** A second child, or a
    /// child that is not the arm, fails here.
    #[test]
    fn every_held_shape_is_one_mesh_one_material_and_one_transform() {
        let mut app = app();
        let view_mesh = app.world().resource::<HandVisuals>().mesh.clone();
        let arm_mesh = app.world().resource::<HandVisuals>().forearm_mesh.clone();

        for (shape, item_id) in shape_examples() {
            *app.world_mut().resource_mut::<Inventory>() =
                Inventory::from_stacks(vec![InventoryStack {
                    item_id,
                    count: 1,
                    ..Default::default()
                }]);
            *app.world_mut().resource_mut::<SelectedSlot>() = SelectedSlot(0);
            app.update();

            let world = app.world_mut();
            let mut view = world.query_filtered::<
                (Entity, &HeldItem, &Mesh3d),
                With<MeshMaterial3d<StandardMaterial>>,
            >();
            let drawn: Vec<(Entity, HeldItem, Handle<Mesh>)> = view
                .iter(world)
                .map(|(entity, held, mesh)| (entity, *held, mesh.0.clone()))
                .collect();
            assert_eq!(
                drawn.len(),
                1,
                "{shape:?} is {} entities carrying a mesh and a material",
                drawn.len()
            );
            assert_eq!(drawn[0].1.shape, Some(shape));
            assert_eq!(
                drawn[0].2, view_mesh,
                "{shape:?} replaced the stable view-model mesh handle"
            );

            let mut children = world.query::<(&ChildOf, Entity)>();
            let under: Vec<Entity> = children
                .iter(world)
                .filter(|(parent, _)| parent.parent() == drawn[0].0)
                .map(|(_, entity)| entity)
                .collect();
            assert_eq!(
                under.len(),
                1,
                "{shape:?} has {} child entities, and the arm is the only one this \
                 composition spends",
                under.len()
            );
            assert!(
                world.get::<Forearm>(under[0]).is_some(),
                "{shape:?} has a child that is not the forearm, so part of it is off the \
                 transform `animate_view_model` drives"
            );
            assert_eq!(
                world
                    .get::<Mesh3d>(under[0])
                    .expect("the arm draws a mesh")
                    .0,
                arm_mesh,
                "{shape:?} gave the arm a mesh of its own instead of the shared bar"
            );
        }
    }

    /// **No pit leaves the blade the blade already was.**
    ///
    /// This is what `every_blade_vertex_sits_on_the_bevel_it_is_specified_by` was pinning
    /// the un-displaced case of, and before that what
    /// `every_rust_mark_is_bedded_into_the_blade_it_freckles` measured about fourteen boxes.
    /// A box laid on a surface that tilts away from it can float clear at one end or come
    /// through the other face; displacement can do neither, but it can eat *outward*, and a
    /// blade whose rust stands proud of it is the fourteen boxes again with more triangles.
    ///
    /// So: every vertex inside the envelope the un-pitted blade has, measured against
    /// [`blade_surface`]'s closed form rather than against the arithmetic that placed it,
    /// and no pit deeper than the livery's own [`pit_depth`] of the local half-thickness. The
    /// outline is
    /// checked too, because "eats through the flats, never into the edges" is the claim that
    /// makes the first property arithmetic rather than a hope.
    #[test]
    fn no_pit_leaves_the_blades_envelope() {
        let pitted = blade_loft(Some(Livery::WornSteel));
        let smooth = blade_loft(None);

        let sections = blade_sections();
        let (root, tip) = (sections[0].y, sections[2].y);
        let mut deepest = 0.0_f32;
        for [x, y, z] in positions(&pitted) {
            let section = blade_at(y.clamp(root, tip));
            let surface = blade_surface(section, z);
            assert!(
                x.abs() <= surface + 1e-6,
                "a vertex reaches {} from the mid-plane where the blade's own surface is at \
                 {surface}, so a pit stands proud of the steel it is meant to eat into",
                x.abs()
            );
            assert!(
                z.abs() <= section.half_width + 1e-6,
                "a vertex reaches {} across a blade half {} wide, so a pit has bitten into \
                 the outline",
                z.abs(),
                section.half_width
            );
            deepest = deepest.max(surface - x.abs());
        }

        let half_thickness = BLADE_THICKNESS / 2.0;
        let depth = livery::pit_depth(Livery::WornSteel);
        assert!(
            deepest <= half_thickness * depth + 1e-6,
            "the deepest pit takes {deepest} out of a {half_thickness} half-thickness, past \
             the {depth} this blade is allowed to lose"
        );
        assert!(
            deepest > half_thickness * depth * 0.5,
            "the deepest pit is {deepest}, which is barely a dent — the field and the \
             subdivision have stopped meeting"
        );

        // And the pitting is what the extra vertices are *for*. A subdivided blade that
        // displaced nothing would satisfy every clause above.
        assert!(
            pitted.count_vertices() > smooth.count_vertices() * 4,
            "the pitted blade is {} vertices against the smooth blade's {}, which is not a \
             subdivision",
            pitted.count_vertices(),
            smooth.count_vertices()
        );
    }

    /// **The iron sword is the sword it was**, which is the property that keeps one shared
    /// [`ItemShape::Blade`] from meaning one shared condition.
    ///
    /// The two blades share a shape and that sharing is the point; a test that only looked
    /// at the rusty one would not see the iron one break. What is asserted is identity
    /// rather than similarity — the same positions, the same normals, the same count — so
    /// the answer cannot drift by a subdivision nobody meant to apply.
    #[test]
    fn the_iron_sword_is_the_blade_it_was_before_the_livery() {
        let iron = item_mesh(ITEM_IRON_SWORD, ItemShape::Blade);
        let unliveried = sword_with(
            SWORD_LENGTH,
            None,
            Some(items::item_linear_rgba(ITEM_IRON_SWORD)),
        );

        // **It wears a livery now and it is the same blade**, which is the whole of what
        // #420 claims for a colour-only material: `sword_mesh` in both states, to the vertex.
        assert_eq!(
            items::item_livery(ITEM_IRON_SWORD),
            Some(Livery::ForgedSteel),
            "the iron sword no longer names the forged steel every other iron item does"
        );
        assert_eq!(
            livery::pit_depth(Livery::ForgedSteel),
            0.0,
            "forged steel displaces, so the iron sword is no longer the blade it was"
        );
        assert_eq!(
            positions(&iron),
            positions(&unliveried),
            "the iron sword is no longer the plain loft, so its livery has changed its shape"
        );
        assert_eq!(
            iron.count_vertices(),
            unliveried.count_vertices(),
            "the iron sword has been subdivided by a livery that displaces nothing"
        );

        // And it does sample — in its own band. A blade whose coordinates stayed neutral
        // would satisfy every clause above and draw the polished steel it drew before.
        let neutral = livery::neutral_uv();
        let sampled: Vec<[f32; 2]> = uvs(&iron).into_iter().filter(|uv| *uv != neutral).collect();
        assert!(
            !sampled.is_empty(),
            "the iron sword samples nothing, so its forge marks reach no surface"
        );
        // **Two bands, because a sword is two materials.** Its steel, and the wood its grip
        // is turned from — what must never appear is worn steel, which is the *other* blade's.
        for uv in &sampled {
            assert!(
                livery::band_holds(Livery::ForgedSteel, *uv)
                    || livery::band_holds(Livery::Wood, *uv),
                "the iron sword samples {uv:?}, outside its steel's band and its grip's wood"
            );
            assert!(
                !livery::band_holds(Livery::WornSteel, *uv),
                "the iron sword samples {uv:?}, which is the rusty blade's band"
            );
        }

        // The rusty sword is the same shape and a different blade, which is what makes the
        // clause above a measurement rather than a tautology about `sword_mesh`.
        let rusty = item_mesh(ITEM_RUSTY_SWORD, ItemShape::Blade);
        assert_eq!(items::item_shape(ITEM_RUSTY_SWORD), ItemShape::Blade);
        assert_ne!(
            positions(&rusty),
            positions(&iron),
            "both blades are the same mesh, so the livery decides nothing about the shape"
        );
        // Its own steel, too. The livery is a multiplier over whatever `player/items.rs`
        // says an item presents as, so a change that resolved both blades to one colour
        // would leave the meshes correct and the swords indistinguishable.
        assert_ne!(
            items::item_linear_rgba(ITEM_IRON_SWORD),
            items::item_linear_rgba(ITEM_RUSTY_SWORD),
            "the two blades resolve to one colour, so the iron sword is not its own steel"
        );
    }

    /// **Every held arrangement samples only the livery it owns.**
    ///
    /// The sweep the whole one-material arrangement rests on — see [`neutral`] for what
    /// goes wrong without it. Every item the client knows, in the arrangement the hand
    /// actually builds, each vertex either neutral or on a blade that wears a livery. Over
    /// `known_item_ids` rather than a list written here, so a new item is covered by
    /// arriving.
    #[test]
    fn every_held_arrangement_samples_only_the_livery_it_owns() {
        let neutral = livery::neutral_uv();
        for item_id in items::known_item_ids() {
            let appearance = selected_appearance(Some(InventoryStack {
                item_id,
                count: 1,
                ..Default::default()
            }));
            let mesh = held_mesh(TEST_SKIN, appearance);
            let sampled: Vec<[f32; 2]> =
                uvs(&mesh).into_iter().filter(|uv| *uv != neutral).collect();
            match items::item_livery(item_id) {
                None => assert!(
                    sampled.is_empty(),
                    "item {item_id} wears no livery and yet {} of its vertices sample one",
                    sampled.len()
                ),
                // **Inside its own band, never another material's**, which is the property
                // one image for every livery makes checkable. A shape whose mesh carries no
                // real coordinates — armour, today — samples nothing at all and is
                // unchanged; what must never happen is a blade reading the rows some other
                // metal was written into.
                Some(_) => {
                    let worn = bands_worn(item_id);
                    for uv in &sampled {
                        assert!(
                            worn.iter().any(|livery| livery::band_holds(*livery, *uv)),
                            "item {item_id} samples {uv:?}, outside the bands it wears: \
                             {worn:?}"
                        );
                        for other in Livery::ALL {
                            assert!(
                                worn.contains(&other) || !livery::band_holds(other, *uv),
                                "item {item_id} samples {uv:?}, which is {other:?}'s band — \
                                 it wears another material's surface"
                            );
                        }
                    }
                }
            }
        }

        // And at least one item reaches a livery through its mesh, so the clause above
        // cannot be satisfied by nothing ever sampling anything.
        let blades = items::known_item_ids()
            .filter(|id| items::item_livery(*id).is_some())
            .filter(|id| {
                let appearance = selected_appearance(Some(InventoryStack {
                    item_id: *id,
                    count: 1,
                    ..Default::default()
                }));
                uvs(&held_mesh(TEST_SKIN, appearance))
                    .into_iter()
                    .any(|uv| uv != neutral)
            })
            .count();
        assert!(
            blades >= 2,
            "only {blades} items reach a livery through their mesh"
        );

        // The empty hand, which has no item to be asked about and is the arrangement a
        // player spends most of their time looking at.
        let empty = held_mesh(TEST_SKIN, selected_appearance(None));
        assert!(
            uvs(&empty).iter().all(|uv| *uv == neutral),
            "the empty hand samples the livery, so a bare fist is rusty"
        );

        // And the arm, which is a second entity sharing the same material.
        assert!(
            uvs(&skinned_forearm_mesh(TEST_SKIN))
                .iter()
                .all(|uv| *uv == neutral),
            "the forearm samples the livery, so the player's arm is rusty"
        );
    }

    /// **The sword is not inside out**, which is the one failure in here that costs the most
    /// to diagnose.
    ///
    /// A lofted section walked the wrong way round produces a mesh that is geometrically
    /// perfect and invisible: back-face culling removes every triangle you can see and keeps
    /// every triangle you cannot, so the sword disappears when you look at it and reappears
    /// from inside. Nothing else in this file's tests would notice — the extents, the taper,
    /// the bevel and the rust are all statements about positions.
    ///
    /// Two independent readings, because they fail apart. The winding check says the stored
    /// normal agrees with the order the triangle's own corners are in; the radial check says
    /// that order is the outward one rather than a consistent inward one, which is exactly
    /// what reversing a section produces.
    #[test]
    fn every_face_of_the_sword_looks_outward() {
        let mesh = held_sword(ITEM_IRON_SWORD);

        let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("the sword must carry Float32x3 positions");
        };
        let Some(VertexAttributeValues::Float32x3(normals)) =
            mesh.attribute(Mesh::ATTRIBUTE_NORMAL)
        else {
            panic!("the sword must carry Float32x3 normals");
        };
        let indices: Vec<usize> = mesh
            .indices()
            .expect("the sword is indexed")
            .iter()
            .collect();

        let point = |index: usize| Vec3::from_array(positions[index]);
        for corner in indices.chunks(3) {
            let [a, b, c] = corner else {
                panic!("the sword's indices are not whole triangles")
            };
            let wound = (point(*b) - point(*a)).cross(point(*c) - point(*a));
            let stored = Vec3::from_array(normals[*a]);
            assert!(
                wound.dot(stored) > 0.0,
                "the triangle at {a} is wound against the normal it carries, so it draws from \
                 the wrong side"
            );

            // Away from the sword's own axis, for every face that has an opinion about it.
            // The two end caps do not — they look along the axis — and they are the ones
            // this term skips rather than the ones it fails on.
            let middle = (point(*a) + point(*b) + point(*c)) / 3.0;
            let radial = Vec3::new(middle.x, 0.0, middle.z);
            if radial.length() > 1e-4 && stored.xz().length() > 1e-4 {
                assert!(
                    radial.dot(stored) > 0.0,
                    "the triangle at {a} faces in toward the sword's axis, so the mesh is \
                     inside out"
                );
            }
        }
    }

    /// **The same sword every run**, which is what a seeded generator buys over a random one.
    ///
    /// A blade whose freckles moved between sessions would be the one thing about it a player
    /// could not learn, and the failure is invisible inside any single run.
    #[test]
    fn the_rusty_sword_is_scattered_the_same_way_every_time() {
        let read = |mesh: &Mesh| {
            let Some(VertexAttributeValues::Float32x3(positions)) =
                mesh.attribute(Mesh::ATTRIBUTE_POSITION)
            else {
                panic!("the rusted blade must carry Float32x3 positions");
            };
            positions.clone()
        };
        assert_eq!(
            read(&sword_with(SWORD_LENGTH, Some(Livery::WornSteel), None)),
            read(&sword_with(SWORD_LENGTH, Some(Livery::WornSteel), None)),
            "two builds of one sword pit the blade in different places"
        );
    }

    /// **Every attack presentation clears the near plane in every reachable pose.**
    ///
    /// #174 replaced the blade swing with three arcs; the bow adds one separate draw pose.
    /// This walks the *real vertices* — not a bounding box, whose corners no vertex of the
    /// shape occupies — through the poses that the input path can actually pair with it.
    ///
    /// The placement bump is swept alongside, because it is the only animation that carries
    /// the model *toward* the camera and it can coincide with a swing: a right click and a
    /// left click inside the same 220 ms both play. That combination is the tightest pose the
    /// view model ever reaches, and it is worth recording that the single box this replaced
    /// did **not** clear the near plane there while the sword does — the pommel's corner sits
    /// closer to the axis of the swing than the box's did.
    #[test]
    fn every_held_arrangement_clears_the_near_plane_through_every_swing() {
        let mut app = app();
        app.update();
        let parent = held(&mut app).2;
        let Projection::Perspective(projection) = app
            .world()
            .get::<Projection>(parent)
            .expect("the world camera has a projection")
        else {
            panic!("the world camera is perspective");
        };
        let near = projection.near;

        let appearances = shape_examples()
            .into_iter()
            .map(|(_, item_id)| {
                selected_appearance(Some(InventoryStack {
                    item_id,
                    count: 1,
                    ..Default::default()
                }))
            })
            .chain([selected_appearance(None)]);

        for appearance in appearances {
            // **The arm is in this sweep, and that is asserted rather than assumed.** It is the
            // lowest thing in the composition and therefore the part an overhead cut carries
            // nearest the eye, so a composition that dropped it would leave this test measuring
            // the one arrangement that was never at risk (#389). Since #394 the arm is a child
            // entity whose length follows the animation, so the corners are read **inside** the
            // pose loop below rather than once — reading them once is exactly how a sweep would
            // come to measure the resting arm through a thrust that had lengthened it.
            let lowest = extent(&drawn_positions(appearance, &HandAnimation::default()), 1).0;
            assert!(
                lowest <= -ARM_REACH + 1e-5,
                "{:?} reaches only {lowest} below the origin, so the forearm at {} is not in \
                 the buffers this sweep walks",
                appearance.shape,
                -ARM_REACH
            );
            let mut arcs: Vec<Option<SwingShape>> = match appearance.shape {
                Some(ItemShape::Blade) => vec![Some(SwingShape::Cut)],
                Some(ItemShape::Bow) => vec![Some(SwingShape::Draw)],
                Some(ItemShape::Sceptre) => vec![Some(SwingShape::Cast)],
                _ => Vec::new(),
            };
            // **The eating arc is swept over every arrangement, not over the food it is
            // drawn for.** The three above are paired with what draws them because
            // `super::combat` routes the left button on the item id; this one is started by
            // a `ConsumeSent`, and which item is in the fist while it plays is
            // `super::inventory`'s routing table rather than anything this file can see.
            // Both foods are drawn as [`ItemShape::Material`] today, and a bound that held
            // only for `Material` would be a bound this file could not defend the day a
            // third one is a block. It carries the composition *toward* the eye, so it is
            // the near plane that has to grant it — which makes the tallest arrangement a
            // fist can hold the one to measure it against, and this is where that happens.
            arcs.push(Some(SwingShape::Eat));
            arcs.push(None);
            for shape in arcs {
                for step in 0..=32u8 {
                    for bump in 0..=16u8 {
                        let animation = HandAnimation {
                            attack: shape.map(|shape| Swing {
                                shape,
                                elapsed: ATTACK_SWING_TIME.mul_f32(f32::from(step) / 32.0),
                            }),
                            bump_elapsed: Some(PLACE_BUMP_TIME.mul_f32(f32::from(bump) / 16.0)),
                            ..Default::default()
                        };
                        let transform =
                            presented_transform(&animation, appearance.shape, default_fov());
                        for corner in &drawn_positions(appearance, &animation) {
                            let point = transform.transform_point(Vec3::from_array(*corner));
                            assert!(
                                -point.z > near,
                                "{:?} in {shape:?} at {step}/32 with the bump at {bump}/16 \
                                 carries {corner:?} to z {} against a near plane at {near}",
                                appearance.shape,
                                point.z
                            );
                        }
                    }
                }
            }
        }
    }

    /// The rust reaches the screen only for the sword it belongs to.
    ///
    /// Read through the mesh the hand is actually built from, so it is the routing under
    /// test rather than the table: holding the iron sword must not reach the livery.
    ///
    /// **What it measures moved with the oxide.** The rust was two vertex tints on one mesh
    /// and this counted them; it is a surface now, so what says "rusty" is a texture
    /// coordinate that leaves the neutral band and a field that is not zero where it lands.
    /// The routing under test is the same routing — and it is a better test of it, because
    /// the old one would have passed on a blade that carried the tint and sampled nothing.
    #[test]
    fn only_the_rusty_sword_is_drawn_rusted() {
        let neutral = livery::neutral_uv();
        for (item_id, want_rusted) in [(ITEM_RUSTY_SWORD, true), (ITEM_IRON_SWORD, false)] {
            let appearance = selected_appearance(Some(InventoryStack {
                item_id,
                count: 1,
                ..Default::default()
            }));
            let mesh = held_mesh(TEST_SKIN, appearance);
            let strongest = uvs(&mesh)
                .into_iter()
                .filter(|uv| *uv != neutral)
                .map(|uv| livery::strength_at(Livery::WornSteel, uv))
                .fold(0.0_f32, f32::max);
            assert_eq!(
                strongest > 0.0,
                want_rusted,
                "item {item_id} reaches a rust strength of {strongest}, want rusted = \
                 {want_rusted}"
            );
        }
    }

    /// **The empty hand is one cube on a narrower arm**, and the count is the whole of it.
    ///
    /// **This is what is left of `the_empty_hand_is_a_palm_with_four_fingers_and_a_thumb`
    /// after #396 deleted the shape it described.** That test read six boxes out of
    /// [`fist_mesh`] and measured the gaps between four front-face fingers, the thumb's band,
    /// its inboard reach and its chirality. Every one of those measurements passed, for three
    /// iterations, against relief the renderer never drew: the material is `unlit`, so a
    /// skin-coloured face is exactly the same colour as the skin-coloured face beside it and
    /// an interior edge is invisible. Deleting the geometry without deleting the assertions
    /// would leave the file claiming a shape nothing draws, so what survives is the half that
    /// was ever about the silhouette — **how many boxes, and how big**.
    ///
    /// The count pins the cube: one box for the fist, three for the whole empty hand once the
    /// wrist and the forearm are merged in. The extent is measured off the real buffers rather
    /// than inferred from the constant that built them, and `PREVIOUS_HAND_SIZE` is the box
    /// that shipped before #384 — no axis may grow back toward it, because #369 had already
    /// answered that complaint by scaling a forearm 0.85 and leaving it a forearm.
    #[test]
    fn the_empty_hand_is_one_cube_on_a_narrower_arm() {
        const PREVIOUS_HAND_SIZE: Vec3 = Vec3::new(0.03825, 0.07225, 0.03825);

        // **The fist, read on its own rather than through the empty arrangement.** Since #389
        // the empty hand is a fist *and* a forearm, and every measurement below — the extent
        // against HAND_SIZE especially — is a statement about the fist alone. The arrangement
        // is checked for what it is directly underneath, so nothing about the composition
        // escapes being counted.
        let mesh = fist_mesh();
        let one_box = Mesh::from(Cuboid::from_size(Vec3::ONE)).count_vertices();

        assert_eq!(
            mesh.count_vertices(),
            one_box,
            "the fist is not exactly one cube"
        );
        assert_eq!(
            held_mesh(TEST_SKIN, selected_appearance(None)).count_vertices(),
            one_box * 2,
            "the empty hand is not exactly that cube plus the wrist under it"
        );
        // The forearm is the third box and it is drawn from an entity of its own since #394,
        // so it is counted where it lives rather than in the hand's mesh. One box there too:
        // a limb assembled from several would be relief an unlit material cannot show, which
        // is the direction #396 exists to close.
        assert_eq!(
            forearm_mesh().count_vertices(),
            one_box,
            "the forearm is not exactly one bar"
        );

        // That the constant is cubic is a `const` assertion beside it, with the two
        // relationships that outlive this shape — as tall as the grip it closes on, and
        // inside the band a hand's width-to-height ratio occupies. A statement about three
        // numbers belongs where the compiler can check it; what is left here is the mesh.

        let positions = positions(&mesh);
        for axis in 0..3 {
            let size = HAND_SIZE[axis];
            assert!(
                size < PREVIOUS_HAND_SIZE[axis],
                "axis {axis} grew back toward the box #384 replaced"
            );
            let (min, max) = extent(&positions, axis);
            assert!(
                (max - min - size).abs() < 1e-5,
                "the fist spans {} on axis {axis}, and HAND_SIZE says {size}",
                max - min
            );
        }

        // One box has exactly two planes on every axis, so nothing is modelled inside the
        // cube. This is the assertion that would fail first if a later change reintroduced
        // relief the material cannot show — the direction #396 exists to close.
        for axis in 0..3 {
            let mut planes: Vec<f32> = positions.iter().map(|position| position[axis]).collect();
            planes.sort_by(f32::total_cmp);
            planes.dedup_by(|left, right| (*left - *right).abs() < 1e-6);
            assert_eq!(
                planes.len(),
                2,
                "the fist has {planes:?} on axis {axis}, so something is modelled inside the \
                 cube where an unlit material cannot show it"
            );
        }
    }

    /// **The wrist steps in from the fist, in the outline a player actually sees.**
    ///
    /// The silhouette is the whole of this composition's information budget — the material is
    /// `unlit`, so the only other channel is the vertex tint and the hand and the arm share
    /// one colour — and this is the one break in it. [`WRIST_WIDTH`] states the fraction;
    /// what a player meets is a *projected* outline at a real field of view, so this measures
    /// the step through [`presented_transform`] off the two real meshes rather than trusting
    /// the constant.
    ///
    /// **Measured at the join rather than over the whole limb, and that is not a convenience.**
    /// The composition is pitched forward, so the far end of the arm is a centimetre nearer
    /// the eye than the fist is and perspective magnifies it by almost exactly the fraction
    /// [`WRIST_WIDTH`] takes away: the *bounding box* of the whole limb is as wide as the
    /// fist's and sits further inboard, which reads as a lean rather than a step and is what
    /// a naive version of this test measures. The step is a local discontinuity at the fist's
    /// lower face, between two boxes twelve millimetres apart in depth, and that is where it
    /// has to be read.
    ///
    /// The inset is required on **both** edges. One edge alone is the lean above, which
    /// [`REST_PITCH_RADIANS`] records as measured and rejected, and it would pass a test that
    /// only compared widths.
    ///
    /// **Both halves of the step used to be asserted, and #415 removed one of them on
    /// purpose.** The wrist was centred under the fist, so it stepped in by half the width it
    /// gave up on each side and the forearm swelled back out below it — three different
    /// outboard edges in one outline, which reads as a limb that necks in twice rather than as
    /// a wrist. What is asserted now is the shape that was asked for: **one flush outboard
    /// edge, and the whole of the step on the inboard side**, where the eye is looking for it.
    /// The step is not weakened by that, it is doubled — it is the full width the wrist gives
    /// up rather than half of it.
    #[test]
    fn the_limb_presents_one_outboard_edge_and_steps_in_only_on_the_inboard_side() {
        let rest = presented_transform(&HandAnimation::default(), None, default_fov());
        let across = |point: Vec3| {
            let point = rest.transform_point(point);
            let depth = -point.z;
            assert!(depth > 0.0, "the empty hand crossed the camera plane");
            point.x / depth
        };
        let span = |corners: &[[f32; 3]]| {
            corners
                .iter()
                .map(|corner| across(Vec3::from_array(*corner)))
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(low, high), x| {
                    (low.min(x), high.max(x))
                })
        };

        // The limb's two boxes live in two places since #394 — the wrist is merged into the
        // hand and the forearm hangs from an entity of its own — so each is read where it is
        // rather than picked out of one buffer by width. The extent below is what stops the
        // selection being vacuous, and the width relationship the split used to prove
        // incidentally is asserted directly underneath.
        let wrist = positions(&wrist_mesh());
        assert_eq!(
            wrist.len(),
            Mesh::from(Cuboid::from_size(Vec3::ONE)).count_vertices(),
            "the wrist is not one box"
        );
        let (wrist_low, wrist_high) = extent(&wrist, 1);
        let expected_top = -HAND_SIZE.y / 2.0 + ARM_OVERLAP;
        assert!(
            (wrist_high - expected_top).abs() < 1e-6
                && (wrist_low - (expected_top - WRIST_LENGTH)).abs() < 1e-6,
            "the box picked out spans {wrist_low}..{wrist_high} rather than the wrist"
        );
        // The forearm is the wrist's width and sits on the wrist's axis, so the limb below
        // the fist is one box wide rather than two. Read off the meshes, because that is what
        // a later edit would have to move.
        let arm = positions(&placed_forearm(&HandAnimation::default()));
        let (arm_low, arm_high) = extent(&arm, 0);
        let (limb_low, limb_high) = extent(&wrist, 0);
        assert!(
            ((arm_high - arm_low) - (limb_high - limb_low)).abs() < 1e-6
                && (arm_low - limb_low).abs() < 1e-6,
            "the forearm spans {arm_low}..{arm_high} against a wrist of \
             {limb_low}..{limb_high}, so the limb below the fist is not one width on one axis"
        );

        let (fist_left, fist_right) = span(&positions(&fist_mesh()));
        let (wrist_left, wrist_right) = span(&wrist);
        let (arm_left, arm_right) = span(&arm);

        // At the default field of view on 1080 lines one pixel spans `2·tan(fov/2)/1080` of
        // this projection. Both halves below are measured in those, because both are claims
        // about what a player can see rather than about what a constant says.
        let field_of_view = crate::settings::Settings::default().field_of_view();
        let pixel = 2.0 * (field_of_view.to_radians() / 2.0).tan() / 1080.0;

        // **One outboard edge.** The three boxes sit at three depths, so perspective will
        // never put them on the same abscissa to the bit; under a pixel is the whole of what
        // *flush* can mean on a screen, and it is what this asserts.
        let outboard = [fist_right, wrist_right, arm_right];
        let spread = outboard.iter().copied().fold(f32::NEG_INFINITY, f32::max)
            - outboard.iter().copied().fold(f32::INFINITY, f32::min);
        assert!(
            spread < pixel,
            "the outboard edges are at fist {fist_right}, wrist {wrist_right}, forearm \
             {arm_right} — {spread} apart, over a pixel of {pixel} at {field_of_view}° on \
             1080 lines"
        );

        // **And the whole of the step is inboard**, on both boxes of the limb, and worth
        // seeing rather than a rounding error.
        for (name, edge) in [("wrist", wrist_left), ("forearm", arm_left)] {
            let step = edge - fist_left;
            assert!(
                step > 4.0 * pixel,
                "the {name} steps in by {step}, under four pixels at {field_of_view}° on \
                 1080 lines"
            );
        }
    }

    /// **Rule 2 of `client/AGENTS.md`, for the hand — and until #415 nothing checked it here.**
    ///
    /// > No two faces of different colours land on the same plane where they overlap. Coplanar
    /// > faces of different materials fight for the depth buffer and flicker.
    ///
    /// It is asserted for the body rig by `appearance::tests::no_two_colours_share_a_plane`,
    /// which can compare *boxes* because that rig is a table of them. This composition is one
    /// merged mesh carrying colour per vertex, so the faces are what there is to read, and the
    /// defect that made this necessary was invisible to every test in this file: the pommel's
    /// side faces and the wrist's were the same two planes to the bit — `HAND_SIZE.x *
    /// WRIST_WIDTH` and `POMMEL_SIZE.x` are both `0.01799999923` — overlapping by 8 mm of
    /// height over the pommel's 17 mm of depth. A player saw the sword drawn through the arm.
    ///
    /// **Colour-aware, and that is the load-bearing word.** Two coincident faces are only a
    /// defect when they are different colours: the fist and the wrist are flush on their
    /// outboard side by construction since #415, and the same skin on both sides of a shared
    /// plane is nothing a depth fight can make visible. A version of this written as "no two
    /// coplanar overlapping faces" fires on that by design, and the next reader silences it.
    ///
    /// **Axis-aligned faces only.** Every part whose placement is a constant is a `Cuboid`;
    /// the blade is lofted and its bevels are not axis-aligned, so they are not read here.
    /// That is where this check is weaker than the body rig's, and it is weaker in the safe
    /// direction — it can miss a plane, it cannot invent one.
    #[test]
    fn no_two_colours_share_a_plane_in_the_hand() {
        /// One axis-aligned, single-coloured face: which way it faces, where its plane is, the
        /// rectangle it covers there, and the colour it is.
        struct Face {
            axis: usize,
            sign: bool,
            plane: f32,
            across: [(f32, f32); 2],
            colour: [u8; 4],
        }

        /// Whether two closed intervals cover a positive length in common — the same
        /// *positive area* rule the body rig's check states, one axis at a time.
        fn overlaps(a: (f32, f32), b: (f32, f32)) -> bool {
            a.0.max(b.0) < a.1.min(b.1) - 1e-9
        }

        /// **The one pair this composition shares a plane with on purpose: the fist's own
        /// end faces and the grip's.**
        ///
        /// `HAND_SIZE.y == GRIP_SIZE.y` is asserted beside the two constants — a fist exactly
        /// as tall as the grip it closes on is what makes "the guard's lower face lands on the
        /// fist's top face" and "the pommel is entirely below the bottom one" the same
        /// statement — so this coincidence is the arrangement rather than a slip in it. It is
        /// invisible because the guard seats on one of those planes and the pommel on the
        /// other, each covering the whole of the grip's cross-section; the `const` assertion
        /// beside [`GUARD_SIZE`] is what holds that, so a change that stopped it being true
        /// fails there rather than passing quietly here.
        ///
        /// Named this narrowly on purpose. It exempts the fist's two horizontal planes and
        /// only where the other face is exactly the grip's section, so a *different* part
        /// arriving on either plane is still a failure.
        fn the_grip_inside_the_fist(one: &Face, two: &Face) -> bool {
            // **Inside the radius rather than equal to the box**, since #419 turned the grip.
            // A box cap was one quad spanning the full `GRIP_SIZE` in both axes; a turned cap
            // is a fan of [`GRIP_SIDES`] triangles, each covering a wedge of it. What every
            // one of them still satisfies — and what no face of the fist, the guard or the
            // pommel does — is sitting wholly within `GRIP_SIZE.x / 2` of the sword's axis.
            let is_grip = |face: &Face| {
                let radius = GRIP_SIZE.x / 2.0;
                (0..2).all(|axis| {
                    face.across[axis].0 >= -radius - 1e-6 && face.across[axis].1 <= radius + 1e-6
                })
            };
            one.axis == 1
                && (one.plane.abs() - HAND_SIZE.y / 2.0).abs() < 1e-6
                && (is_grip(one) != is_grip(two))
        }

        let appearances = shape_examples()
            .into_iter()
            .map(|(shape, item_id)| {
                (
                    format!("{shape:?}"),
                    selected_appearance(Some(InventoryStack {
                        item_id,
                        count: 1,
                        ..Default::default()
                    })),
                )
            })
            .chain([("an empty hand".to_owned(), selected_appearance(None))]);

        for (name, appearance) in appearances {
            let mesh = held_mesh(TEST_SKIN, appearance);
            let positions = positions(&mesh);
            let Some(VertexAttributeValues::Float32x4(colours)) =
                mesh.attribute(Mesh::ATTRIBUTE_COLOR)
            else {
                panic!("{name}: the merged mesh must carry per-vertex colour");
            };
            let indices: Vec<usize> = mesh
                .indices()
                .expect("a merged mesh carries indices")
                .iter()
                .collect();

            let mut faces: Vec<Face> = Vec::new();

            for corner in indices.chunks_exact(3) {
                let quantise =
                    |index: usize| colours[index].map(|channel| (channel * 255.0).round() as u8);
                let colour = quantise(corner[0]);
                if quantise(corner[1]) != colour || quantise(corner[2]) != colour {
                    continue;
                }
                let points = [corner[0], corner[1], corner[2]]
                    .map(|index| Vec3::from_array(positions[index]));
                let normal = (points[1] - points[0]).cross(points[2] - points[0]);
                if normal.length_squared() < 1e-18 {
                    continue;
                }
                let normal = normal.normalize();
                let Some(axis) = (0..3).find(|axis| normal[*axis].abs() > 1.0 - 1e-4) else {
                    continue;
                };
                let plane = points[0][axis];
                let across = [(axis + 1) % 3, (axis + 2) % 3].map(|other| {
                    points
                        .iter()
                        .fold((f32::INFINITY, f32::NEG_INFINITY), |(low, high), point| {
                            (low.min(point[other]), high.max(point[other]))
                        })
                });
                faces.push(Face {
                    axis,
                    sign: normal[axis] > 0.0,
                    plane,
                    across,
                    colour,
                });
            }

            assert!(
                faces.len() > 6,
                "{name}: {} axis-aligned faces is too few to have read the hand at all",
                faces.len()
            );

            for (index, one) in faces.iter().enumerate() {
                for two in &faces[index + 1..] {
                    if one.axis != two.axis
                        || one.sign != two.sign
                        || one.colour == two.colour
                        || (one.plane - two.plane).abs() > 1e-6
                    {
                        continue;
                    }
                    if !(overlaps(one.across[0], two.across[0])
                        && overlaps(one.across[1], two.across[1]))
                    {
                        continue;
                    }
                    if the_grip_inside_the_fist(one, two) {
                        continue;
                    }
                    panic!(
                        "{name}: two colours share the plane {} on axis {}, over \
                         {:?}×{:?} and {:?}×{:?} — the depth fight #415 was filed about",
                        one.plane,
                        one.axis,
                        one.across[0],
                        one.across[1],
                        two.across[0],
                        two.across[1]
                    );
                }
            }
        }
    }

    /// **The fist closes *around* the grip rather than being as wide as it.**
    ///
    /// **Re-derived a second time, and again the mechanism moved rather than the claim.** #393
    /// re-derived it when `BLADE_CAMERA_OFFSET` went: the question had been which of the
    /// hand's relief a bar of hilt standing six millimetres proud of it left showing, and once
    /// the grip was inside the fist there was nothing of the hand that could be behind it.
    /// What survived was the statement that never depended on the offset — the hand is
    /// **wider than the hilt it holds**, with hand on both sides of it.
    ///
    /// #396 takes the mechanism the same way. That statement was proved by finding eight
    /// front-face finger edges through `FINGER_BAND` and reading the thumb's inboard extent,
    /// and the fist has neither fingers nor a thumb any more — but the claim is *more* true of
    /// a cube than it was of the digits, because a cube's outline is its own two edges: half
    /// its width is `0.012` against `GRIP_SIZE.x / 2.0 = 0.007`, so five millimetres of hand
    /// shows past the hilt on each side. It is stated from the cube's own projected outline
    /// here, which is what a player sees, and from the mesh's own extent, which is what a
    /// later edit would have to move.
    ///
    /// It is kept because it is what stops a later edit widening a grip until the fist is a
    /// collar on it — the mesh-side statement of the containment the `const` assertion beside
    /// [`GRIP_SIZE`] makes. Its doc said the measurement had survived its premise once
    /// already; this is the second time.
    #[test]
    fn the_fist_closes_around_the_grip_rather_than_matching_its_width() {
        const EPSILON: f32 = 1e-6;

        // The fist's own outline in `X`, read off the real buffers rather than off HAND_SIZE.
        let fist = positions(&fist_mesh());
        let (fist_inboard, fist_outboard) = extent(&fist, 0);
        let grip_half = GRIP_SIZE.x / 2.0;

        assert!(
            fist_inboard < -grip_half - EPSILON,
            "the fist's inboard edge is at {fist_inboard} and the grip's at {}, so no hand \
             shows beside the hilt on that side",
            -grip_half
        );
        assert!(
            fist_outboard > grip_half + EPSILON,
            "the fist's outboard edge is at {fist_outboard} and the grip's at {grip_half}, so \
             no hand shows beside the hilt on that side"
        );

        // And on screen, which is where the claim lives: the grip is drawn through the same
        // one transform as the hand that holds it, so a projection is the honest comparison —
        // perspective widens whichever of the two is nearer the eye, and the grip is the one
        // sitting on the fist's centre plane.
        let rest = presented_transform(
            &HandAnimation::default(),
            Some(ItemShape::Blade),
            default_fov(),
        );
        let across = |point: Vec3| {
            let point = rest.transform_point(point);
            let depth = -point.z;
            assert!(depth > 0.0, "the held arrangement crossed the camera plane");
            point.x / depth
        };
        let projected = |corners: &[[f32; 3]]| {
            corners
                .iter()
                .map(|corner| across(Vec3::from_array(*corner)))
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(low, high), x| {
                    (low.min(x), high.max(x))
                })
        };

        let (hand_left, hand_right) = projected(&fist);
        let Hilt { grip, .. } = hilt_corners(ITEM_RUSTY_SWORD);
        let (grip_left, grip_right) = projected(&grip);
        assert!(
            hand_left < grip_left && hand_right > grip_right,
            "the fist projects to {hand_left}..{hand_right} and the grip to \
             {grip_left}..{grip_right}, so the hand is a collar on the hilt rather than a \
             fist closed around it"
        );
    }

    /// **The fist takes at most a fifth of the viewport, and the arithmetic says so.**
    ///
    /// The box this replaced was 48% of it at the default field of view — half the screen, from
    /// the bottom edge up — and every previous attempt to answer that complaint scaled the box by
    /// eye. The three numbers the projection actually depends on are named here, and the default
    /// field of view is read out of `settings` rather than copied, so a change to the default
    /// this hand was sized against fails here instead of silently re-inflating it.
    #[test]
    fn the_fist_covers_at_most_a_fifth_of_the_viewport_at_the_default_field_of_view() {
        let field_of_view = crate::settings::Settings::default().field_of_view();
        let viewport_height = 2.0 * BASE_DEPTH.abs() * (field_of_view.to_radians() / 2.0).tan();
        let covered = HAND_SIZE.y / viewport_height;
        assert!(
            covered <= 0.20,
            "the fist is {:.1}% of the viewport height at {field_of_view}°, and 20% is the ceiling",
            covered * 100.0
        );
    }

    /// **The whole fist is on screen, and all of it is in the lower-right quadrant.**
    ///
    /// The real vertices through the real rest pose and a real perspective divide, because the
    /// failure this replaces was exactly the one a bounding box in model space cannot see: the
    /// old centre sat 0.3 mm past the bottom of the frustum, so half the box was hard-clipped and
    /// what remained read as something entering the frame from below.
    ///
    /// 16:9 is the narrowest common frame and therefore the binding one for the horizontal half
    /// of this. The crosshair is at the origin of this projection, so *right of the vertical
    /// centre line* is the whole of "never touching the crosshair".
    #[test]
    fn the_whole_fist_sits_in_the_lower_right_of_a_16_by_9_frame() {
        const ASPECT: f32 = 16.0 / 9.0;

        let field_of_view = crate::settings::Settings::default().field_of_view();
        let half_height = (field_of_view.to_radians() / 2.0).tan();
        let half_width = half_height * ASPECT;
        let rest = presented_transform(&HandAnimation::default(), None, default_fov());

        for corner in positions(&fist_mesh()) {
            let point = rest.transform_point(Vec3::from_array(corner));
            let depth = -point.z;
            assert!(depth > 0.0, "{corner:?} landed behind the camera");
            let (x, y) = (point.x / depth, point.y / depth);
            assert!(
                x > 0.0,
                "{corner:?} projects to x {x}, on or across the vertical centre line"
            );
            assert!(
                x <= half_width,
                "{corner:?} projects to x {x}, off the right edge"
            );
            assert!(
                y < 0.0,
                "{corner:?} projects to y {y}, out of the lower half of the frame"
            );
            assert!(
                y >= -half_height,
                "{corner:?} projects to y {y}, clipped by the bottom edge"
            );
        }
    }

    /// **Nothing about the default frame moved, and that is the point of the number rather
    /// than a happy accident.**
    ///
    /// [`HAND_DROP_FRACTION`] is `0.050 / (0.18 · tan(22.5°))`: the fraction that reproduces
    /// #384's own `BASE_TRANSLATION.y` at the field of view #384 derived it at. Every
    /// measurement in this file that names no field of view is therefore measuring the frame
    /// it has always measured, and the change is confined to the settings nobody had looked
    /// at. A fraction re-tuned for a wider frame would have re-framed the default too, which
    /// is a much larger claim to have to defend than the one this makes.
    #[test]
    fn the_default_frame_is_exactly_the_one_384_derived() {
        let placement = base_translation(default_fov());
        // A tenth of a millimetre. The hand is 24 mm across, so this is a four-hundredth of
        // it — well under anything a projection at this scale can show.
        assert!(
            (placement.y - -0.050).abs() < 1e-4,
            "the hand sits at {} at the default field of view, and #384 put it at -0.050",
            placement.y
        );
        assert!(
            (placement.x - 0.10).abs() < f32::EPSILON && (placement.z - -0.18).abs() < f32::EPSILON,
            "the two axes that stayed constant moved: {placement:?}"
        );
        // The off-hand shield's own fraction, held to the height it was written at for the
        // same reason: neither hand moves at the default field of view.
        let shield = shield_translation(default_fov());
        assert!(
            (shield.y - -0.035).abs() < 1e-4 && (shield.z - -0.16).abs() < f32::EPSILON,
            "the off-hand shield sits at {shield:?}, and it was spawned at (-0.10, -0.035, -0.16)"
        );
    }

    /// **The hand sits at the same place in the frame whatever the frame is.**
    ///
    /// The property [`HAND_DROP_FRACTION`] exists for, stated where a change to the
    /// derivation fails rather than merely looking different. Both hands are in it: the
    /// off-hand shield was written once at spawn before #415 and would otherwise have stayed
    /// at a fixed height while the main hand followed the frame, which is two hands at two
    /// heights the moment a player moves the slider.
    ///
    /// **And the limb's share of the frame is bounded**, which is the weaker second half and
    /// is honest about being weaker: the arm is a fixed length, so a wider frame fits more of
    /// it and no placement can hold the proportion. What is pinned is that the limb never
    /// takes over — see [`HAND_DROP_FRACTION`] for the measured before and after.
    #[test]
    fn the_composition_sits_at_the_same_place_in_every_frame() {
        for field_of_view in every_field_of_view() {
            let half_height = (field_of_view / 2.0).tan();
            let degrees = field_of_view.to_degrees();

            for (name, translation, fraction) in [
                (
                    "the hand",
                    base_translation(field_of_view),
                    HAND_DROP_FRACTION,
                ),
                (
                    "the off-hand shield",
                    shield_translation(field_of_view),
                    SHIELD_DROP_FRACTION,
                ),
            ] {
                let at = translation.y / -translation.z / half_height;
                assert!(
                    (at + fraction).abs() < 1e-5,
                    "{name} sits {at} of the way down the frame at {degrees:.0}°, not {}",
                    -fraction,
                    at = at
                );
            }

            // The limb's own share, read off the real meshes through the real rest pose.
            let pose = presented_transform(&HandAnimation::default(), None, field_of_view);
            let animation = HandAnimation::default();
            let visible = |mesh: &Mesh| {
                let (mut low, mut high) = (f32::INFINITY, f32::NEG_INFINITY);
                for corner in positions(mesh) {
                    let point = pose.transform_point(Vec3::from_array(corner));
                    let y = point.y / -point.z;
                    low = low.min(y);
                    high = high.max(y);
                }
                (high.min(0.0) - low.max(-half_height)).max(0.0)
            };
            let fist = visible(&fist_mesh());
            let wrist = visible(&wrist_mesh());
            let forearm = visible(&placed_forearm(&animation));
            let share = forearm / (fist + wrist + forearm);
            assert!(
                share < 0.50,
                "the forearm is {:.1}% of the visible limb at {degrees:.0}°, and half is the \
                 ceiling",
                share * 100.0
            );
        }
    }

    /// **The whole fist is inside the frame at every field of view a player can choose, and
    /// through every pose the animations reach.**
    ///
    /// [`the_whole_fist_sits_in_the_lower_right_of_a_16_by_9_frame`] is the rest pose at the
    /// **default** field of view, and that is the whole of what it ever checked. The setting
    /// runs from [`crate::settings::MIN_FIELD_OF_VIEW`] to
    /// [`crate::settings::MAX_FIELD_OF_VIEW`], and at the narrowest of those the fist's lowest
    /// corner projected to `-0.3678` against a bottom edge at `-0.3640` — **past it**. #384's
    /// defect was still shipping at one end of a slider, unseen because the test that closed
    /// it reads one value of that slider (#415).
    ///
    /// It cannot be seen from a constant height, either: a height is a fixed distance into a
    /// frame whose size the setting chooses, so the fraction of the frame it lands at moves
    /// with the setting. [`HAND_DROP_FRACTION`] is what makes the placement a fraction, and
    /// this is what holds it — over the animations too, because a mining punch and a swing
    /// carry the composition toward the eye and a margin measured at rest is not one.
    #[test]
    fn the_whole_fist_stays_in_frame_at_every_field_of_view_a_player_can_choose() {
        // **Two of the eight animations, and the other six are excluded by measurement rather
        // than by omission.** A placement bump, all three swing arcs, a bow draw and a sceptre
        // cast each carry the fist past the bottom edge **at the default field of view
        // already** — measured here at 1.08, 1.08, 1.07, 1.05, 1.33 and 1.08 of the way to it.
        // The hand dipping out of frame is what those arcs are; requiring otherwise of them
        // would be inventing a property this composition has never had and calling the
        // invention a regression test. What is asserted is the property #384 established and
        // #415 found broken at one end of a slider: **the hand a player is looking at while
        // they are not swinging is whole.**
        let sweeps = [
            ("at rest", None, None, false, false),
            ("through a mining punch", None, None, false, true),
        ];

        let corners = positions(&fist_mesh());
        let mut worst = (f32::NEG_INFINITY, String::new());

        for field_of_view in every_field_of_view() {
            let half_height = (field_of_view / 2.0).tan();
            for (name, shape, held, bump, mining) in sweeps {
                for (animation, _) in animation_poses(shape, held, bump, mining) {
                    let pose = presented_transform(&animation, held, field_of_view);
                    for corner in &corners {
                        let point = pose.transform_point(Vec3::from_array(*corner));
                        let depth = -point.z;
                        assert!(depth > 0.0, "{corner:?} landed behind the camera");
                        let reached = (-point.y / depth) / half_height;
                        if reached > worst.0 {
                            worst = (
                                reached,
                                format!("{name} at {:.0}°", field_of_view.to_degrees()),
                            );
                        }
                    }
                }
            }
        }

        // **A margin rather than a bare containment**, because a corner that clears the edge by
        // a millionth is the accident #384 already was, one edit away from being it again. The
        // binding frame is the narrowest one — the fist's own projected height is the largest
        // fraction of the frame there — and it reaches 0.91 of the way down, so this ceiling is
        // the measurement with room over it rather than the measurement rounded up.
        assert!(
            worst.0 < 0.95,
            "the fist reaches {:.1}% of the way to the bottom edge {}, and 95% is the ceiling",
            worst.0 * 100.0,
            worst.1
        );
    }

    /// The four corners of the forearm's end cap **in the frame this animation reaches**: the
    /// one face of the limb that must never be seen, because seeing it is seeing where the
    /// player's arm stops.
    ///
    /// Its section is the fist's since #396 — the limb carried the *palm's* depth and centre
    /// plane while there was a digit band for it to stand out of, and there is not one now.
    ///
    /// **It takes an animation since #394**, and that is the whole change to this file stated
    /// in one signature: the cap is no longer at a constant [`ARM_REACH`], it is wherever
    /// [`drawn_arm_reach`] has put it for the frame being measured. A version of this that
    /// kept the constant would report the numbers of an arm the renderer no longer draws.
    fn forearm_cap(animation: &HandAnimation) -> [Vec3; 4] {
        let half_width = HAND_SIZE.x * FOREARM_WIDTH / 2.0;
        let half_depth = HAND_SIZE.z / 2.0;
        let reach = drawn_arm_reach(along_view(animation));
        [
            Vec3::new(-half_width, -reach, -half_depth),
            Vec3::new(-half_width, -reach, half_depth),
            Vec3::new(half_width, -reach, -half_depth),
            Vec3::new(half_width, -reach, half_depth),
        ]
    }

    /// The widest 16:9 field of view, in degrees, at which every one of `corners` is still
    /// outside the frame under `pose`.
    ///
    /// **A number rather than a yes/no**, because the acceptance criterion #389 states is a
    /// decision with a number attached: the arm cannot grow past what the near plane allows,
    /// so the honest answer is the field of view at which its end arrives, measured.
    fn widest_clipped_fov(corners: &[Vec3], pose: &Transform) -> f32 {
        const ASPECT: f32 = 16.0 / 9.0;
        corners
            .iter()
            .map(|corner| {
                let point = pose.transform_point(*corner);
                let depth = -point.z;
                assert!(depth > 0.0, "{corner:?} landed behind the camera");
                // Outside by whichever edge it clears first: the frustum is symmetric, so one
                // half-angle tangent describes both, and the horizontal one is the aspect
                // ratio wider.
                let tangent = (-point.y / depth).max(point.x.abs() / (ASPECT * depth));
                2.0 * tangent.atan().to_degrees()
            })
            .fold(f32::INFINITY, f32::min)
    }

    /// Every frame one animation reaches, as the state the renderer would hold and the
    /// transform it would build from it.
    ///
    /// **Both halves, since #394.** The transform alone used to be everything a caller needed;
    /// now the limb's length is read off the animation too, so handing back only the pose would
    /// leave the caller measuring a resting arm in a frame that has stretched it.
    fn animation_poses(
        shape: Option<SwingShape>,
        held: Option<ItemShape>,
        bump: bool,
        mining: bool,
    ) -> Vec<(HandAnimation, Transform)> {
        let mut poses = Vec::new();
        let bumps = if bump { 0..=16u8 } else { 0..=0u8 };
        for step in 0..=32u8 {
            for frame in bumps.clone() {
                let animation = HandAnimation {
                    attack: shape.map(|shape| Swing {
                        shape,
                        elapsed: ATTACK_SWING_TIME.mul_f32(f32::from(step) / 32.0),
                    }),
                    bump_elapsed: bump.then(|| PLACE_BUMP_TIME.mul_f32(f32::from(frame) / 16.0)),
                    // One full punch: the loop repeats, so a whole cycle is every pose it has.
                    mine_elapsed: if mining {
                        Duration::from_secs_f32(f32::from(step) / 32.0 / MINE_PUNCHES_PER_SECOND)
                    } else {
                        Duration::ZERO
                    },
                };
                let pose = presented_transform(&animation, held, default_fov());
                poses.push((animation, pose));
            }
        }
        poses
    }

    /// **The forearm is exactly as long as the camera's near plane permits, and the bound is
    /// re-derived here rather than trusted.**
    ///
    /// #389 asked for an arm and the near plane is what decides its length: the composition
    /// rotates about its own origin, so everything below that origin swings toward the eye
    /// during an overhead cut, and the placement bump has already carried the model forward
    /// by then. This reads `near` off the real projection — the same source
    /// [`every_held_arrangement_clears_the_near_plane_through_every_swing`] reads — and
    /// re-derives the permitted reach from the constants the pose is built from, so a change
    /// to the swing, to the bump or to the base placement fails here with the new bound
    /// printed instead of failing as a hand that vanishes when you swing it.
    ///
    /// **The bound is `0.0599` and it was `0.0620` before #396**, which is worth stating
    /// because #394 was drafted against the older number and would have been over-optimistic
    /// by two millimetres if it had reused it. Nothing about the near plane, the camera or the
    /// swing moved: the limb took the fist's own section when the fist became a cube, which is
    /// 24 mm of depth on the model's own centre plane where the palm's was 22.2 mm pushed
    /// 3.9 mm back, and `section` below is where that shows up.
    ///
    /// **What this test bounds is [`ARM_REACH`] — the length at the model's resting depth —
    /// and since #394 that is not the only length the arm is drawn at.** The reason it is
    /// still the right thing to bound here is the clamp asserted at the end: the limb only
    /// ever grows, and it only grows when the animation has carried the model *away* from the
    /// eye, which is the direction this pose is not.
    #[test]
    fn the_forearm_is_as_long_as_the_near_plane_permits() {
        let mut app = app();
        app.update();
        let parent = held(&mut app).2;
        let Projection::Perspective(projection) = app
            .world()
            .get::<Projection>(parent)
            .expect("the world camera has a projection")
        else {
            panic!("the world camera is perspective");
        };
        let near = projection.near;

        // The tightest pose the view model reaches: a full blade cut with the placement bump
        // at its peak. Both terms carry the model toward the eye.
        let pitch = REST_PITCH_RADIANS - CUT_PITCH_RADIANS;
        let roll = REST_ROLL_RADIANS - 0.18;
        // **[`BLADE_NEAR_PLANE_CLEARANCE`] is part of the bound**, and it is not an
        // optimism. A blade cut is only ever drawn with a sword in hand — `combat.rs` routes
        // the left button on the item id and [`SwingShape::Cut`] belongs to the blades — so
        // the pose that reaches nearest the camera is always the one this offset has already
        // pushed back. It is the same pairing
        // [`every_held_arrangement_clears_the_near_plane_through_every_swing`] sweeps, which
        // is the test that would catch it if the routing ever widened.
        let depth = BASE_DEPTH + PLACE_BUMP_DISTANCE - BLADE_NEAR_PLANE_CLEARANCE;
        // How much of a point's own `-Y` that pose turns into camera-space `+Z`.
        let toward_camera = -pitch.sin() * roll.cos();
        // **The limb's own half-section spends part of the headroom before its length does,
        // and since #421 it is taken as a radius rather than as a projection.** The blade's
        // arc now carries yaw as well as pitch and roll, and yaw turns the limb about its own
        // long axis — so which corner of the cross-section leads is no longer a function of
        // the roll alone. The circumscribed radius is the corner that leads under *any* yaw,
        // which makes the bound conservative for every frame instead of exact for one.
        let radius =
            ((HAND_SIZE.x * FOREARM_WIDTH / 2.0).powi(2) + (HAND_SIZE.z / 2.0).powi(2)).sqrt();
        let permitted = (-near - depth - radius) / toward_camera;

        assert!(
            ARM_REACH <= permitted,
            "the forearm reaches {ARM_REACH} below the origin and the near plane at {near} \
             permits {permitted}"
        );
        // **The floor that used to sit here has moved, and it moved because the arm stopped
        // answering only to the near plane.** It read `ARM_REACH > permitted * 0.9` — *not
        // needlessly short* — and it was the right statement while this bound was the only
        // thing deciding the limb's length. Two changes took that away. #415 made the
        // placement a fraction of the frame and the length a framing decision, and #421
        // replaced the overhead cut's `0.9` of pitch with this arc's `0.68`, which alone
        // opens about a centimetre of headroom the arm has no reason to spend: a longer limb
        // is *more* forearm on screen, which is the thing #415 was filed to reduce.
        //
        // So the floor is not deleted, it is where it is actually measured.
        // [`the_forearms_end_stays_out_of_frame_through_every_animation`] holds the limb long
        // enough that its end never comes into frame, which is the property "not needlessly
        // short" was standing in for, stated against the frame rather than against the near
        // plane. What stays here is the ceiling, which is the only thing the near plane has
        // an opinion about.

        // **And the bound survives [`drawn_arm_reach`], because the arm only ever grows away
        // from the eye.** The pose above is the tightest one there is and its `along_view` is
        // a placement bump — positive, toward the camera — so what must be true is that no
        // non-negative offset lengthens the limb at all. That is the clamp, asserted here
        // rather than read off the expression, and it is the half of the length rule the near
        // plane depends on. The other half, that a *negative* offset never costs more
        // headroom than the depth it buys, is not an argument this test can make from one
        // pose: `every_held_arrangement_clears_the_near_plane_through_every_swing` sweeps the
        // real vertices of the stretched arm through every reachable frame, and that is where
        // it is measured.
        for approach in [0.0, PLACE_BUMP_DISTANCE / 2.0, PLACE_BUMP_DISTANCE, 1.0] {
            assert!(
                (drawn_arm_reach(approach) - ARM_REACH).abs() < f32::EPSILON,
                "an animation carrying the model {approach} toward the eye draws the arm at \
                 {}, past the {permitted} the near plane permits there",
                drawn_arm_reach(approach)
            );
        }
    }

    /// **The forearm joins the fist to the bottom of the frame with nothing showing through.**
    ///
    /// This is the defect #389 was filed about, measured the way a player meets it: a column
    /// dropped from inside the palm has to stay on skin all the way off the bottom edge. It
    /// walks the *real triangles* of the mesh the renderer draws, through the rest pose the
    /// renderer uses, so a limb that merely exists in the buffers but does not line up under
    /// the hand fails here.
    ///
    /// **The columns span the wrist rather than the fist**, deliberately. The wrist is
    /// narrower than the fist — [`WRIST_WIDTH`] says why, and it is the only thing making the
    /// composition read as a hand on an arm rather than as one flat rectangle — so the fist
    /// overhangs it and world is visible past that overhang. That is a hand's own outline,
    /// not a break in the limb. What must be continuous is the limb, and this is the band the
    /// limb occupies.
    ///
    /// **The band is [`LIMB_OUTBOARD_OFFSET`] off centre since #415**, because the limb is.
    /// It was written as `±half_width` about the model's axis while the wrist was centred
    /// under the fist, and a band that stayed there would have walked a column of sky the
    /// limb no longer covers — reporting the step this composition is *supposed* to have as a
    /// break in the arm. The overhang moved to one side; it did not appear.
    #[test]
    fn the_forearm_joins_the_fist_to_the_bottom_edge_with_no_gap() {
        const ASPECT: f32 = 16.0 / 9.0;
        const COLUMNS: usize = 33;
        const STEP: f32 = 0.0005;

        let field_of_view = crate::settings::Settings::default().field_of_view();
        let half_height = (field_of_view.to_radians() / 2.0).tan();

        let walk = |name: &str, animation: &HandAnimation, held: Option<ItemShape>| {
            let pose = presented_transform(animation, held, default_fov());
            // **Both entities, composed the way the renderer parents them.** The limb is a
            // child of the hand since #394, so a version of this walk that read `held_mesh`
            // alone would be asking whether the *wrist* reaches the bottom edge — which it
            // does not, and never did.
            let mut triangles =
                projected_triangles(&held_mesh(TEST_SKIN, selected_appearance(None)), &pose);
            triangles.extend(projected_triangles(&placed_forearm(animation), &pose));

            let project = |point: Vec3| {
                let point = pose.transform_point(point);
                Vec2::new(point.x / -point.z, point.y / -point.z)
            };
            // **The band that is inside the limb at every height it spans**, rather than the
            // bounding box of its projected corners.
            //
            // Those were the same thing only while something wider sat under the extremes: the
            // forearm was `0.93` of the fist and centred, so it covered the wrist's widest
            // abscissa wherever perspective put it. Since #415 the limb is one width on one
            // axis, and a box's outline reaches its extreme abscissa at a single corner — so a
            // column drawn at the bounding box's edge is outside the limb at every other
            // height, and reporting that as a hole would be a finding somebody had to silence.
            //
            // Each face's own corners answer instead: the innermost inboard corner and the
            // innermost outboard corner bound a strip the limb covers along its whole length.
            let mut inboard = f32::NEG_INFINITY;
            let mut outboard = f32::INFINITY;
            for corner in positions(&wrist_mesh())
                .into_iter()
                .chain(positions(&placed_forearm(animation)))
            {
                let x = project(Vec3::from_array(corner)).x;
                if corner[0] < LIMB_OUTBOARD_OFFSET {
                    inboard = inboard.max(x);
                } else {
                    outboard = outboard.min(x);
                }
            }
            // And the same for the fist above, which the walk starts inside. Flush outboard
            // faces mean the fist no longer overhangs the limb on that side, so the strip has
            // to be inside both.
            let mut fist_inboard = f32::NEG_INFINITY;
            let mut fist_outboard = f32::INFINITY;
            for corner in positions(&fist_mesh()) {
                let x = project(Vec3::from_array(corner)).x;
                if corner[0] < 0.0 {
                    fist_inboard = fist_inboard.max(x);
                } else {
                    fist_outboard = fist_outboard.min(x);
                }
            }
            let (left, right) = (inboard.max(fist_inboard), outboard.min(fist_outboard));
            let inset = (right - left) * 0.02;
            // Start inside the fist, which every column of the wrist's band is under.
            let start = project(Vec3::ZERO).y;
            for column in 0..COLUMNS {
                let fraction = column as f32 / (COLUMNS - 1) as f32;
                let x = left + inset + (right - left - 2.0 * inset) * fraction;
                let mut y = start;
                while y > -half_height {
                    assert!(
                        triangles
                            .iter()
                            .any(|triangle| contains(*triangle, Vec2::new(x, y))),
                        "{name}: at {field_of_view}° and {ASPECT:.4}:1 the world shows through \
                         at ({x}, {y}), between the fist and the bottom edge at {}",
                        -half_height
                    );
                    y -= STEP;
                }
            }
        };

        walk("at rest", &HandAnimation::default(), None);
        // **And at the peak of a cast**, which is the frame #394 was filed about and the one
        // where the limb is longest. It was the thrust's frame until #421 removed that arc; a
        // cast carries the model the same distance away — it spends the same [`CAST_REACH`] the
        // thrust spent — so the pose this walk needs is unchanged and only its name moved.
        //
        // A join that opens when the arm stretches would be the new way to reintroduce exactly
        // the defect #389 closed, and it is invisible to the end-cap measurements below: those
        // ask where the arm *ends*, not whether it is continuous on the way there. The hand is
        // walked empty in both passes and the sceptre's pose is used for the second, which is
        // the strict pairing: a held item can only add cover, and the offset it brings is the
        // one a cast is really drawn with.
        walk(
            "at the peak of a cast",
            &HandAnimation {
                attack: Some(Swing {
                    shape: SwingShape::Cast,
                    elapsed: ATTACK_SWING_TIME / 2,
                }),
                ..Default::default()
            },
            Some(ItemShape::Sceptre),
        );
    }

    /// **Where the end of the arm arrives, stated as a number for every animation there is.**
    ///
    /// #389 asked for exactly this and was explicit that the answer must not be left to be
    /// discovered: above some field of view the limb's end cap comes into frame, and the only
    /// unacceptable outcome is not knowing where.
    ///
    /// **#394 is the change that moved the two numbers this table existed to apologise for.**
    /// The paragraph that used to sit here said no arm short enough to survive an overhead cut
    /// is long enough to stay clipped through a thrust, and the arithmetic was right: the near
    /// plane permits `0.0599` and a *fixed* arm needs `0.0705` to reach the bottom edge at the
    /// default field of view through a thrust. What was wrong was the word *arm*. The cut that
    /// threatens the near plane carries the model **toward** the eye and the thrust that needs
    /// the reach carries it **away**, so the two bounds are never imposed on the same frame,
    /// and only a *constant* length has to satisfy both at once. [`drawn_arm_reach`] scales the
    /// limb with the model's own depth, which cancels the term that was shrinking it — and the
    /// two arcs that were below the narrowest selectable field of view now clear the default
    /// one by nine degrees.
    ///
    /// | animation | before #396 | after #396 | **now** |
    /// |---|---|---|---|
    /// | at rest / placement bump / bow draw | 60.5° | 61.2° | **61.2°** |
    /// | overhead cut | 56.2° | 54.2° | **54.2°** |
    /// | lateral slash | 52.8° | 53.8° | **53.8°** |
    /// | mining punch | 52.4° | 52.1° | **60.2°** |
    /// | sceptre cast | 41.1° | 41.3° | **53.7°** |
    /// | thrust | 40.1° | 39.8° | **53.9°** |
    ///
    /// **Nothing regressed and nothing was traded.** The four animations that do not move the
    /// model away are unchanged to the tenth of a degree, because the length rule is floored at
    /// the resting length and those frames are all at or below it — see [`drawn_arm_reach`] for
    /// why the floor is there and what shrinking on approach would have cost. The three that do
    /// reach away improve by the depth they spend.
    ///
    /// **The thrust's floor is `53.0` now, and it is no longer an exception.** It was `39.0` —
    /// a hand-written number below [`crate::settings`]'s narrowest field of view, put there
    /// because the defect this test measures had not been fixed yet. Every floor in the table
    /// below is the measured value rounded down, every one of them is above the **default**
    /// field of view, and the loop under the table asserts that last clause rather than leaving
    /// it to be checked by eye — so a floor that drifts under the default is a failure with a
    /// name, not a number somebody re-records.
    #[test]
    fn the_forearms_end_stays_out_of_frame_through_every_animation() {
        let widest = |shape, held, bump, mining| {
            animation_poses(shape, held, bump, mining)
                .iter()
                .map(|(animation, pose)| widest_clipped_fov(&forearm_cap(animation), pose))
                .fold(f32::INFINITY, f32::min)
        };

        let mut narrowest = crate::settings::Settings::default();
        narrowest.adjust(crate::settings::Knob::FieldOfView, -1_000);
        let narrowest = narrowest.field_of_view();
        let default = crate::settings::Settings::default().field_of_view();

        // Measured on the geometry above; each is the widest field of view at which every
        // corner of the end cap is still outside a 16:9 frame.
        for (name, measured, floor) in [
            ("at rest", widest(None, None, false, false), 60.0),
            (
                "through a placement bump",
                widest(None, None, true, false),
                60.0,
            ),
            (
                "through a mining punch",
                widest(None, None, false, true),
                60.0,
            ),
            (
                "through a blade cut",
                widest(Some(SwingShape::Cut), Some(ItemShape::Blade), true, false),
                53.0,
            ),
            (
                "through a bow draw",
                widest(Some(SwingShape::Draw), Some(ItemShape::Bow), true, false),
                60.0,
            ),
            (
                "through a sceptre cast",
                widest(
                    Some(SwingShape::Cast),
                    Some(ItemShape::Sceptre),
                    true,
                    false,
                ),
                53.0,
            ),
            // The eating arc carries the composition toward the eye, which is the direction
            // that makes the limb *larger* on screen — the placement bump's direction, and
            // the one [`drawn_arm_reach`]'s floor refuses to shrink for. So this row is
            // measured against the bump's floor rather than against the two arcs that reach
            // away, and it is drawn with the material stub because that is what food is.
            (
                "through an eating arc",
                widest(
                    Some(SwingShape::Eat),
                    Some(ItemShape::Material),
                    true,
                    false,
                ),
                60.0,
            ),
        ] {
            assert!(
                measured >= floor,
                "the forearm's end shows {name} above {measured:.1}°, and {floor:.1}° is the \
                 floor this change was accepted against"
            );
            // **The acceptance criterion, applied to every row rather than to the two that
            // used to fail it.** A floor under the default is how the thrust's `39.0`
            // survived three iterations of this file: it read as a measurement when it was an
            // exemption. There is no longer a row this may be written for.
            assert!(
                floor >= default,
                "{name} is accepted down to {floor:.1}°, under the default {default}°, so a \
                 field of view players actually use is exempt again"
            );
        }

        // The statements the acceptance criterion turns on, kept where they cannot drift
        // apart from the sweep above: the resting arm is clipped at the default field of view
        // with room to spare, and — since #394 — so are the two arcs that reach away.
        assert!(widest(None, None, true, false) > default);
        // **This assertion was inverted by #394 and re-aimed by #421, and neither time was it
        // deleted.** It read `< default` and passed, which is what a test looks like when it
        // has been written around a defect instead of against one: the property it pinned was
        // *the arm's end is visible during a thrust*. #394 turned it over. #421 removed the
        // thrust, so it now names the one arc a blade draws — the swing a player spends the
        // whole game making, which is a better subject for it than the arc that happened to be
        // worst.
        assert!(
            widest(Some(SwingShape::Cut), Some(ItemShape::Blade), true, false) > default,
            "a blade cut shows the arm's end at the default field of view, which is the \
             defect #394 was filed about"
        );
        // **And a cast is still clipped at the narrowest.** It was the thrust's pair until
        // #421 — the two carried the model the same distance away and answered within a degree
        // and a half of each other — and it is now the only arc that carries it away at all,
        // which makes it the one place [`drawn_arm_reach`]'s rule is still exercised by a
        // swing. If a later change walks this number under the narrowest field of view, the
        // length rule has moved rather than a number having been re-recorded.
        assert!(
            widest(
                Some(SwingShape::Cast),
                Some(ItemShape::Sceptre),
                true,
                false
            ) >= narrowest,
            "a sceptre cast now shows the arm's end at the narrowest field of view as well, \
             so the whole reach-away pair has moved rather than the thrust alone"
        );
    }

    /// **The arm keeps the length it is drawn at, and the renderer is what is asked.**
    ///
    /// The table above says *where the end cap arrives*; this says *why it stopped moving*. The
    /// composition translates along the view and the frustum widens with distance, so a limb of
    /// constant length covers less of the screen the further out an animation carries it —
    /// which is the whole of #394, and 39.8° was what it cost. [`drawn_arm_reach`] multiplies
    /// the length by the same depth ratio, so the two cancel and the limb's *projected* reach
    /// below the hand is one number in every frame of every animation.
    ///
    /// **Measured through the transform the renderer uses, not through the ratio.** Asserting
    /// the ratio would restate the expression; what has to hold is a fact about the picture, and
    /// the picture is where the rest pitch, the swing's own rotations and the perspective divide
    /// all land. The invariance is not exact — [`base_height`] puts the whole
    /// composition below the crosshair and that offset does not scale with depth — so this
    /// bounds the residual rather than claiming zero, and the bound is small enough that the
    /// end cap's own tolerance would have to be argued for before it mattered.
    ///
    /// The frames the model is carried *toward* the eye are checked from the other side, on the
    /// clamp: the arm may not lengthen there at all, which is the near plane's half of the
    /// bargain and is asserted in [`the_forearm_is_as_long_as_the_near_plane_permits`].
    #[test]
    fn the_arm_covers_the_same_reach_of_screen_however_far_the_animation_carries_it() {
        // How far below the hand's own origin the arm's end projects, in the plane the frame
        // is measured in. One number per frame, and it is the one that used to shrink.
        let projected_reach = |animation: &HandAnimation, held| {
            let pose = presented_transform(animation, held, default_fov());
            let origin = pose.transform_point(Vec3::ZERO);
            let cap =
                pose.transform_point(Vec3::new(0.0, -drawn_arm_reach(along_view(animation)), 0.0));
            origin.y / -origin.z - cap.y / -cap.z
        };

        let rest = projected_reach(&HandAnimation::default(), None);
        assert!(rest > 0.0, "the arm does not reach below the hand at rest");

        // **One arc rather than two since #421**, and not because the property narrowed: the
        // thrust was removed and a cast spends exactly the [`CAST_REACH`] it spent, so the
        // frame being measured is the same frame under a different name. It is also now the
        // *only* arc that carries the composition along the view, which makes it the whole of
        // what [`drawn_arm_reach`] has to answer for.
        for (name, shape, held) in [("a sceptre cast", SwingShape::Cast, Some(ItemShape::Sceptre))]
        {
            for step in 0..=32u8 {
                let animation = HandAnimation {
                    attack: Some(Swing {
                        shape,
                        elapsed: ATTACK_SWING_TIME.mul_f32(f32::from(step) / 32.0),
                    }),
                    ..Default::default()
                };
                let reach = projected_reach(&animation, held);
                assert!(
                    (reach - rest).abs() < rest * 0.06,
                    "{step}/32 of the way through {name} the arm reaches {reach} of the frame \
                     below the hand against {rest} at rest, so the length is not following the \
                     depth"
                );
            }
        }

        // And the same measurement with the rule taken out, which is what the defect was: a
        // constant length loses a quarter of its reach at the peak of a cast.
        let peak = HandAnimation {
            attack: Some(Swing {
                shape: SwingShape::Cast,
                elapsed: ATTACK_SWING_TIME / 2,
            }),
            ..Default::default()
        };
        let pose = presented_transform(&peak, Some(ItemShape::Sceptre), default_fov());
        let origin = pose.transform_point(Vec3::ZERO);
        let fixed = pose.transform_point(Vec3::new(0.0, -ARM_REACH, 0.0));
        let unscaled = origin.y / -origin.z - fixed.y / -fixed.z;
        assert!(
            unscaled < rest * 0.75,
            "a constant-length arm reaches {unscaled} of the frame at the peak of a thrust \
             against {rest} at rest, so this test is no longer measuring the mechanism #394 \
             was filed about"
        );
    }

    /// **The off-hand shield hand gets the same arm, and the shield's own roll mirrors it.**
    ///
    /// `spawn_view_model` builds that entity from the same [`held_mesh`] and hangs the same
    /// arm under it, so an arm added there arrives on the left hand whether anybody decided it
    /// should or not — which is why #389 asked for the decision to be made rather than
    /// discovered. It is kept: the left hand needs a limb for the same reason the right one
    /// does, and the entity's own `Rz(-0.48)` is larger than anything the arm carries, so the
    /// limb leans *outboard for a left hand* — down and to the left — instead of being a right
    /// arm mirrored the wrong way. That is measured here rather than assumed, because it is
    /// true by arithmetic on two numbers that live in different functions.
    ///
    /// **The arm is now an entity, so the first thing checked is that it is there.** #394 moved
    /// the limb out of the hand's mesh and onto a child; the off-hand's copy is spawned by the
    /// same closure and never animated, which is correct — that entity carries no `along_view`
    /// of its own, so a limb driven from the *held* hand's animation would stretch an arm
    /// nothing had pushed away. What must be asserted is that it exists and rests at the
    /// resting length, because both of those are now spawn-time decisions rather than
    /// properties of a merged mesh.
    #[test]
    fn the_off_hand_shield_carries_a_left_arm_of_its_own() {
        let mut app = app();
        app.update();
        let (_, shield) = off_hand_shield(&mut app);

        let world = app.world_mut();
        let mut arms = world.query_filtered::<(&ChildOf, &Transform), With<Forearm>>();
        let mut shields = world.query_filtered::<Entity, With<OffHandShield>>();
        let shield_entity = shields.single(world).expect("one off-hand shield");
        let arm = arms
            .iter(world)
            .find(|(parent, _)| parent.parent() == shield_entity)
            .map(|(_, transform)| *transform)
            .expect("the off-hand shield hangs an arm of its own");
        assert_eq!(
            arm,
            forearm_transform(&HandAnimation::default()),
            "the off-hand shield's arm is not at the resting length, so something is driving \
             a limb whose hand never moves along the view"
        );

        // The unit bar's far end, through the arm's own transform: this is the composition
        // [`forearm_transform`] promises, read rather than recomputed.
        let far_end = arm.transform_point(Vec3::NEG_Y);
        assert!(
            (far_end.y + ARM_REACH).abs() < 1e-6 && far_end.x.abs() < 1e-6,
            "the off-hand arm's far end is at {far_end:?} rather than {ARM_REACH} straight \
             below the hand"
        );

        let wrist = shield.transform_point(Vec3::new(0.0, -HAND_SIZE.y / 2.0, 0.0));
        let end = shield.transform_point(far_end);
        assert!(
            end.x < wrist.x,
            "the shield hand's arm runs from x {} to x {}, which is inboard rather than out",
            wrist.x,
            end.x
        );

        let cap = forearm_cap(&HandAnimation::default());
        for corner in cap {
            let point = shield.transform_point(corner);
            assert!(
                -point.z > 0.1,
                "the shield hand's arm carries {corner:?} to z {} against the near plane",
                point.z
            );
        }
        let default = crate::settings::Settings::default().field_of_view();
        let widest = widest_clipped_fov(&cap, &shield);
        assert!(
            widest > default,
            "the shield hand's arm shows its end above {widest:.1}°, inside the default \
             {default}° field of view"
        );
    }

    /// The cause of #240 was an exclusive mesh match: five item shapes replaced the fist.
    /// The exact fist now starts every composite, including the empty one, so this sweep
    /// fails if any future arrangement takes that shortcut again.
    #[test]
    fn the_same_fist_is_present_whatever_the_hand_holds() {
        let fist = positions(&fist_mesh());
        let appearances = shape_examples()
            .into_iter()
            .map(|(shape, item_id)| {
                let appearance = selected_appearance(Some(InventoryStack {
                    item_id,
                    count: 1,
                    ..Default::default()
                }));
                assert_eq!(appearance.shape, Some(shape));
                appearance
            })
            .chain([selected_appearance(None)]);

        for appearance in appearances {
            let composite = positions(&held_mesh(TEST_SKIN, appearance));
            assert_eq!(
                &composite[..fist.len()],
                fist,
                "{:?} replaced or moved the fist instead of composing with it",
                appearance.shape
            );
        }
    }

    /// Holding is overlap, not concealment: every item's bounds cross the fist and at least a
    /// quarter of its vertices remain outside after the arrangement is applied. The blade-specific
    /// test below names the stronger hilt properties the generic ratio cannot express.
    #[test]
    fn the_item_stays_recognisable_outside_the_fist() {
        let half = HAND_SIZE / 2.0;
        for (shape, item_id) in shape_examples() {
            let item = item_mesh(item_id, shape).translated_by(item_translation(shape));
            let item_positions = positions(&item);
            let overlaps = (0..3).all(|axis| {
                let (low, high) = extent(&item_positions, axis);
                low < half[axis] && high > -half[axis]
            });
            assert!(overlaps, "{shape:?} floats clear of the fist");
            let outside = item_positions
                .iter()
                .filter(|position| {
                    position[0].abs() > half.x + 1e-6
                        || position[1].abs() > half.y + 1e-6
                        || position[2].abs() > half.z + 1e-6
                })
                .count();
            assert!(
                outside * 4 >= item_positions.len(),
                "only {outside}/{} vertices of {shape:?} remain outside the fist",
                item_positions.len()
            );
        }
    }

    /// A blade is held by its grip, never by concealing the furniture around it.
    ///
    /// Read from the real merged vertices for both sword variants: part constants alone would
    /// still pass if [`item_translation`] moved the complete hilt back inside the fist.
    #[test]
    fn both_blades_show_their_guard_grip_and_pommel_around_the_fist() {
        const EPSILON: f32 = 1e-6;
        let half = HAND_SIZE / 2.0;
        // Read before the loop shadows `positions` with the sword's own.
        let fist_corners = positions(&fist_mesh());

        for item_id in [ITEM_RUSTY_SWORD, ITEM_IRON_SWORD] {
            let translation = item_translation(ItemShape::Blade);
            let blade = item_mesh(item_id, ItemShape::Blade).translated_by(translation);
            let positions = positions(&blade);
            // Each furniture box has a unique depth, so its corners identify it after the
            // parts have been merged. A Y-only selection would also collect the neighbouring
            // boxes on their shared planes and blur the very bounds this test is measuring.
            let part_corners = |half_depth: f32, low: f32, high: f32| -> Vec<[f32; 3]> {
                positions
                    .iter()
                    .copied()
                    .filter(|position| {
                        ((position[2] - translation.z).abs() - half_depth).abs() < EPSILON
                            && position[1] >= low - EPSILON
                            && position[1] <= high + EPSILON
                    })
                    .collect()
            };

            let guard_high = blade_base() + translation.y;
            let guard_low = guard_high - GUARD_SIZE.y;
            let grip_high = guard_low;
            let grip_low = grip_high - GRIP_SIZE.y;
            let pommel_high = grip_low;
            let pommel_low = pommel_high - POMMEL_SIZE.y;

            let guard = part_corners(GUARD_SIZE.z / 2.0, guard_low, guard_high);
            // The grip is turned and no longer has corners at a fixed depth — see
            // [`grip_ring`], which selects it by the radius it has because it is a cylinder.
            let grip = grip_ring(&positions, translation);
            let pommel = part_corners(POMMEL_SIZE.z / 2.0, pommel_low, pommel_high);
            let _ = (grip_low, grip_high);
            assert!(!guard.is_empty() && !grip.is_empty() && !pommel.is_empty());

            let (guard_back, guard_front) = extent(&guard, 2);
            assert!(
                guard_back < half.z - EPSILON && guard_front > half.z + EPSILON,
                "sword {item_id}'s guard does not cross the camera-facing side of the fist: \
                 guard z={guard_back}..{guard_front}, fist z={}..{}",
                -half.z,
                half.z
            );

            // **The fist is closed on the whole grip, not on part of it.** `HAND_SIZE.y ==
            // GRIP_SIZE.y`, so a grip centred on the fist reaches both of its faces — which
            // is a stronger statement than the crossing this replaces, where a grip a third
            // of the fist's height could satisfy "crosses the lower face" while the guard
            // and most of the blade disappeared into the box above it (#384).
            let (grip_low, grip_high) = extent(&grip, 1);
            let (grip_near, grip_far) = extent(&grip, 2);
            assert!(
                grip_low <= -half.y + EPSILON
                    && grip_high >= half.y - EPSILON
                    && grip_near < half.z
                    && grip_far > -half.z,
                "sword {item_id}'s grip is not held along the fist's whole height: \
                 grip y={grip_low}..{grip_high}, fist y={}..{}, grip z={grip_near}..{grip_far}",
                -half.y,
                half.y
            );

            let (guard_low, _) = extent(&guard, 1);
            assert!(
                guard_low >= half.y - EPSILON,
                "sword {item_id}'s cross guard reaches down to {guard_low}, below the fist's \
                 top face at {}",
                half.y
            );

            let (pommel_low, pommel_high) = extent(&pommel, 1);
            assert!(
                pommel_high <= -half.y + EPSILON,
                "sword {item_id}'s pommel is not wholly below the fist: \
                 pommel y={pommel_low}..{pommel_high}, fist bottom={}",
                -half.y
            );
            // And the whole of it is below, so something of the hilt is always showing under
            // the hand: that is what says the fist is closed on a sword rather than being
            // where the sword begins.
            assert!(
                -half.y - pommel_low >= POMMEL_SIZE.y - EPSILON,
                "only {} of sword {item_id}'s {} pommel shows below the fist",
                -half.y - pommel_low,
                POMMEL_SIZE.y
            );

            let grip_centre = sword_grip_centre(SWORD_LENGTH) + translation;
            assert!(
                (0..3).all(|axis| grip_centre[axis].abs() <= half[axis] + EPSILON),
                "sword {item_id}'s grip centre {grip_centre:?} left the fist {half:?}"
            );

            // **Camera-space depth in the transform the renderer actually uses, and the
            // statement it makes is now the opposite one.** It used to require 95 of 101
            // sampled blade sections to put a near-facing surface *in front of* the fist,
            // which is what `BLADE_CAMERA_OFFSET` existed to deliver and which nothing else
            // in the composition could satisfy — the whole sword was one millimetre inside
            // the fist's near face, hilt included, so the grip stood out where the player
            // could see it (#393). The blade does not need to win a depth test against the
            // hand: it clears the hand on **screen**, which
            // `a_blade_rises_clear_of_the_fists_silhouette_instead_of_growing_out_of_it`
            // measures and which never depended on the offset.
            //
            // What is worth pinning here is the half that *does* have to lose that depth
            // test — the pommel-to-guard section of the hilt the fist is closed on. Read
            // through `presented_transform` because that is the transform the renderer
            // applies to the one merged entity, so a later split into separately transformed
            // geometry cannot silently invalidate the comparison.
            let presentation = presented_transform(
                &HandAnimation::default(),
                Some(ItemShape::Blade),
                default_fov(),
            );
            let toward_eye =
                |corner: &[f32; 3]| presentation.transform_point(Vec3::from_array(*corner)).z;
            let nearest = |corners: &[[f32; 3]]| {
                corners
                    .iter()
                    .map(toward_eye)
                    .fold(f32::NEG_INFINITY, f32::max)
            };
            let fist_near = nearest(&fist_corners);
            for (part, corners) in [("grip", &grip), ("pommel", &pommel)] {
                let part_near = nearest(corners);
                assert!(
                    part_near < fist_near - EPSILON,
                    "sword {item_id}'s {part} reaches camera-space z {part_near} and the \
                     fist's nearest surface is at {fist_near}, so the hand does not hide it"
                );
            }
        }
    }

    /// How many vertices the turned grip puts on its own radius.
    ///
    /// Two rings of [`GRIP_SIDES`], each appearing twice — once wound into the side wall and
    /// once into its own cap, because the two carry different normals — **plus two**, which
    /// is the seam: the side wall repeats its first vertex at the end so the wrap-around quad
    /// can carry a texture coordinate of 1.0 rather than 0.0. It is the same seam the blade's
    /// own loft has, one primitive down, and it is worth writing here because `4 × sides` is
    /// what anybody would predict and it is wrong by exactly two.
    const GRIP_RING_VERTICES: usize = GRIP_SIDES as usize * 4 + 2;

    /// **The grip is a cylinder inscribed in the box it replaced**, which is what lets the
    /// three `const _: () = assert!` blocks around [`GRIP_SIZE`] and [`HAND_SIZE`] stand
    /// unchanged: they compare extents component by component, and nothing here leaves them.
    ///
    /// Measured off the real merged sword rather than off the builder call, so a radius or a
    /// height typed against the wrong component of `GRIP_SIZE` fails here.
    #[test]
    fn the_grip_is_turned_inside_the_box_it_replaced() {
        let sword = item_mesh(ITEM_RUSTY_SWORD, ItemShape::Blade);
        let ring = grip_ring(&positions(&sword), Vec3::ZERO);
        assert_eq!(
            ring.len(),
            GRIP_RING_VERTICES,
            "the grip is {} ring vertices, which is not two capped rings of {GRIP_SIDES}",
            ring.len()
        );

        let (low, high) = extent(&ring, 1);
        assert!(
            (high - low - GRIP_SIZE.y).abs() < 1e-6,
            "the grip stands {} tall and the box it replaced is {}",
            high - low,
            GRIP_SIZE.y
        );
        for point in &ring {
            let across = point[0].hypot(point[2]);
            assert!(
                (across - GRIP_SIZE.x / 2.0).abs() < 1e-6,
                "a grip vertex sits {across} from the axis, not on the inscribed radius {}",
                GRIP_SIZE.x / 2.0
            );
        }

        // And it is round rather than a box the selector happened to accept: a square would
        // put every vertex at one of four positions.
        let mut distinct: Vec<[i32; 2]> = ring
            .iter()
            .map(|point| [(point[0] * 1e6) as i32, (point[2] * 1e6) as i32])
            .collect();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            GRIP_SIDES as usize,
            "the grip's cross-section has {} distinct points, not {GRIP_SIDES}",
            distinct.len()
        );
    }

    /// **The grip draws as `palette::LOG` for the rusty sword and for the iron sword**, with
    /// the tint divided out of each blade's own steel rather than hard-coded.
    ///
    /// This is the whole point of [`wood_over`]. A single written-down multiplier would land
    /// the two grips on two different woods, because `ForgedSteel` is brighter than
    /// `WornSteel` — and it would do it silently, since either one looks like *a* colour.
    /// Both blades are asserted, because one of them alone cannot tell a division from a
    /// constant.
    #[test]
    fn the_grip_is_the_same_wood_over_every_blade() {
        let log = palette::linear_rgba(palette::LOG);
        for item_id in [ITEM_RUSTY_SWORD, ITEM_IRON_SWORD] {
            let colour = items::item_linear_rgba(item_id);
            let sword = coloured(item_mesh(item_id, ItemShape::Blade), colour);
            let points = positions(&sword);
            let Some(VertexAttributeValues::Float32x4(tints)) =
                sword.attribute(Mesh::ATTRIBUTE_COLOR)
            else {
                panic!("a blade carrying a wooden grip must carry per-vertex colour");
            };

            let radius = GRIP_SIZE.x / 2.0;
            let high = blade_base() - GUARD_SIZE.y;
            let low = high - GRIP_SIZE.y;
            let mut wooden = 0;
            for (point, tint) in points.iter().zip(tints) {
                let on_the_grip = (point[0].hypot(point[2]) - radius).abs() < 1e-6
                    && point[1] >= low - 1e-6
                    && point[1] <= high + 1e-6;
                if !on_the_grip {
                    // Everything else resolves to the blade's own steel, which is what keeps
                    // `player/items.rs` the one answer to what that is.
                    for channel in 0..3 {
                        assert!(
                            (tint[channel] - colour[channel]).abs() < 1e-6,
                            "item {item_id} draws {tint:?} off the grip, not its own {colour:?}"
                        );
                    }
                    continue;
                }
                wooden += 1;
                for channel in 0..3 {
                    assert!(
                        (tint[channel] - log[channel]).abs() < 1e-6,
                        "item {item_id}'s grip resolves to {tint:?}, not palette::LOG {log:?}"
                    );
                }
            }
            assert_eq!(
                wooden, GRIP_RING_VERTICES,
                "item {item_id} has {wooden} wooden vertices, which is not the grip"
            );
        }
    }

    /// **No blade this build knows needs a wood it cannot reach**, so [`wood_over`]'s `None`
    /// arm is unreached rather than merely handled.
    ///
    /// A vertex colour multiplies, so a mesh can only reach what is darker than its item in
    /// every channel. The arm exists because a future blade darker than `palette::LOG`
    /// somewhere would have no tint that gets there; this is what says today's do.
    #[test]
    fn no_known_blade_needs_a_wood_it_cannot_reach() {
        let mut blades = 0;
        for item_id in items::known_item_ids() {
            if items::item_shape(item_id) != ItemShape::Blade {
                continue;
            }
            blades += 1;
            let colour = items::item_linear_rgba(item_id);
            assert!(
                wood_over(colour).is_some(),
                "item {item_id} is {colour:?}, which cannot reach palette::LOG by multiplying"
            );
        }
        assert!(blades >= 2, "only {blades} blades, so this sweeps nothing");

        // The teeth, on a fixture rather than on the rows that already pass: a blade darker
        // than the wood in one channel is exactly the case the arm is for.
        let too_dark = [0.0, 0.0, 0.0, 1.0];
        assert_eq!(
            wood_over(too_dark),
            None,
            "a blade darker than the wood was given a tint that cannot reach it"
        );
    }

    /// **Every solid in the sword is wound outward**, which is the one failure in a new part
    /// that costs the most to diagnose.
    ///
    /// A ring walked the wrong way round builds the part inside out; back-face culling then
    /// discards its front faces and keeps its back ones, so the result is a solid that
    /// renders transparent — the exact failure [`BladeSection::perimeter`] documents as "a
    /// sword that vanishes when you look at it". It happened in the interactive model #419
    /// was designed against, and nothing there was checking.
    ///
    /// **Per solid, and the whole-mesh version of this was not a test.** The first cut summed
    /// the divergence theorem over the merged sword and required the total to be positive.
    /// That is exactly what a small inverted part survives: the grip encloses about
    /// 3.7 × 10⁻⁶ against the blade's 1.4 × 10⁻⁵, so a grip turned inside out takes the
    /// total from roughly 2.6 × 10⁻⁵ to 1.8 × 10⁻⁵ — still positive, still passing, and the
    /// transparent grip ships. The review on this pull request caught it.
    ///
    /// So the mesh is split into connected solids first. **Welded by position, not by index**:
    /// `merge` duplicates a vertex per face for flat shading, so a cuboid's six faces share no
    /// index at all and index adjacency says nothing about which solid a triangle belongs to.
    /// The reversed reading is asserted beside each one, because a test that only ever sees a
    /// correct mesh proves the mesh and not the test.
    #[test]
    fn every_solid_in_the_sword_is_wound_outward() {
        /// The signed volume of each connected solid in one mesh, largest first.
        fn solid_volumes(mesh: &Mesh, reversed: bool) -> Vec<f32> {
            use std::collections::HashMap;

            let points = positions(mesh);
            let indices: Vec<usize> = mesh
                .indices()
                .expect("a merged mesh carries indices")
                .iter()
                .collect();

            // Quantised, because two parts that touch must weld and two that merely round
            // to the same micrometre must not. The furniture's planes are metres apart in
            // `x` and `z` where they share a `y`, so this separates them.
            let mut welded: HashMap<[i64; 3], usize> = HashMap::new();
            let mut of: Vec<usize> = Vec::with_capacity(points.len());
            for point in &points {
                let key = point.map(|channel| (f64::from(channel) * 1e7).round() as i64);
                let next = welded.len();
                of.push(*welded.entry(key).or_insert(next));
            }

            let mut parent: Vec<usize> = (0..welded.len()).collect();
            fn root(parent: &mut [usize], mut node: usize) -> usize {
                while parent[node] != node {
                    parent[node] = parent[parent[node]];
                    node = parent[node];
                }
                node
            }
            for corner in indices.chunks_exact(3) {
                let [a, b, c] = [of[corner[0]], of[corner[1]], of[corner[2]]];
                for other in [b, c] {
                    let (left, right) = (root(&mut parent, a), root(&mut parent, other));
                    parent[left] = right;
                }
            }

            let mut volumes: HashMap<usize, f32> = HashMap::new();
            for corner in indices.chunks_exact(3) {
                let [a, b, c] = if reversed {
                    [corner[0], corner[2], corner[1]]
                } else {
                    [corner[0], corner[1], corner[2]]
                }
                .map(|index| Vec3::from_array(points[index]));
                let solid = root(&mut parent, of[corner[0]]);
                *volumes.entry(solid).or_insert(0.0) += a.dot(b.cross(c)) / 6.0;
            }

            let mut answer: Vec<f32> = volumes.into_values().collect();
            answer.sort_unstable_by(|left, right| right.abs().total_cmp(&left.abs()));
            answer
        }

        for (name, mesh) in [
            (
                "the rusty sword",
                item_mesh(ITEM_RUSTY_SWORD, ItemShape::Blade),
            ),
            (
                "the iron sword",
                item_mesh(ITEM_IRON_SWORD, ItemShape::Blade),
            ),
            ("a dropped sword", world_sword(0.05)),
            ("a dropped grip", sword_grip_mesh(0.05)),
        ] {
            let solids = solid_volumes(&mesh, false);
            // **The count says which surface this is**, which is the property #435 added and
            // the reason it is a `match` rather than a constant: the hand holds one mesh of
            // four solids — blade, guard, grip, pommel — while the world takes the same
            // weapon in two pieces, three solids and one, because its grip is wood and is
            // drawn in a material of its own. A part that welded into its neighbour, or one
            // that went missing, changes this before any volume does.
            let want = match name {
                "a dropped sword" => 3,
                "a dropped grip" => 1,
                _ => 4,
            };
            assert_eq!(
                solids.len(),
                want,
                "{name} is {} connected solids, want {want}",
                solids.len()
            );
            for (index, volume) in solids.iter().enumerate() {
                assert!(
                    *volume > 0.0,
                    "solid {index} of {name} encloses {volume}, so it is wound inside out \
                     and renders transparent"
                );
            }

            let reversed = solid_volumes(&mesh, true);
            assert_eq!(
                reversed.len(),
                want,
                "{name} splits differently when reversed"
            );
            for (index, volume) in reversed.iter().enumerate() {
                assert!(
                    *volume < 0.0,
                    "solid {index} of {name} reads {volume} with its winding reversed, so \
                     this measures nothing"
                );
            }
        }
    }

    /// **A solid wound inside out is caught**, on a fixture rather than on a sword that
    /// already passes.
    ///
    /// This is the teeth of the test above, and it is the assertion the first cut of it did
    /// not have: reversing *one small part* of the merged sword must fail, where reversing
    /// the whole mesh trivially does. The grip is the part chosen because it is the smallest,
    /// which is exactly the case a whole-mesh sum cannot see.
    #[test]
    fn a_grip_wound_inside_out_does_not_pass_for_a_solid() {
        let sword = item_mesh(ITEM_RUSTY_SWORD, ItemShape::Blade);
        let points = positions(&sword);
        let indices: Vec<usize> = sword
            .indices()
            .expect("a merged mesh carries indices")
            .iter()
            .collect();

        let radius = GRIP_SIZE.x / 2.0;
        let high = blade_base() - GUARD_SIZE.y;
        let low = high - GRIP_SIZE.y;
        let on_the_grip = |index: usize| {
            let point = points[index];
            (point[0].hypot(point[2]) - radius).abs() < 1e-6
                && point[1] >= low - 1e-6
                && point[1] <= high + 1e-6
        };

        let mut total = 0.0_f32;
        let mut grip = 0.0_f32;
        for corner in indices.chunks_exact(3) {
            let flip = corner.iter().all(|index| on_the_grip(*index));
            let [a, b, c] = if flip {
                [corner[0], corner[2], corner[1]]
            } else {
                [corner[0], corner[1], corner[2]]
            }
            .map(|index| Vec3::from_array(points[index]));
            let volume = a.dot(b.cross(c)) / 6.0;
            total += volume;
            if flip {
                grip += volume;
            }
        }

        assert!(
            grip < 0.0,
            "the grip's triangles were not the ones reversed"
        );
        assert!(
            total > 0.0,
            "the whole-mesh sum went negative with only the grip reversed, so the weakness \
             this pins does not exist and the test above is measuring more than it claims"
        );
    }

    /// **The dropped sword and the third-person body get the turned grip, and not yet the
    /// wood.** Both halves are asserted, because the second is a gap and a gap nobody wrote
    /// down is a gap somebody rediscovers.
    ///
    /// The geometry comes free: `drops::drop_mesh` calls [`sword_mesh`], so one shape answer
    /// serves the hand, the ground and the body's fist, and there is no second implementation
    /// of a cylinder anywhere.
    ///
    /// **The colour cannot come free, and the reason is structural rather than an oversight.**
    /// `DropVisuals` caches **one mesh per [`ItemShape`]**, shared by both blades, and colours
    /// it with a per-item material. [`wood_over`] is a division by *that item's* steel, so a
    /// tint baked into the shared mesh would land the grip on `palette::LOG` for one sword
    /// and somewhere else for the other — silently. So [`sword_mesh`] carries no colours at
    /// all and the dropped grip stays the steel it has always been: an unclosed divergence
    /// rather than a new wrong one, and #418 is where the drop stops sharing that mesh.
    #[test]
    fn the_world_takes_the_sword_in_two_pieces_so_its_grip_can_be_wood() {
        let dropped = world_sword(SWORD_LENGTH);
        let grip = sword_grip_mesh(SWORD_LENGTH);

        // The grip is out of the blade's mesh and whole in its own.
        assert!(
            grip_ring(&positions(&dropped), Vec3::ZERO).is_empty(),
            "the world's blade still carries its grip, so the grip cannot have a material of \
             its own"
        );
        assert_eq!(
            grip_ring(&positions(&grip), Vec3::ZERO).len(),
            GRIP_RING_VERTICES,
            "the world's grip is not the turned one the hand holds"
        );

        // **Neither piece carries a colour**, which is what makes the split worth making.
        // `hands.rs` reaches its wood by dividing `palette::LOG` out of *that* blade's own
        // steel; `drops.rs` shares one mesh per shape and livery between blades, so a tint
        // divided out of one steel and baked in would be right for one sword and silently
        // wrong for the other. An absolute colour on a mesh of its own needs no division.
        for (name, mesh) in [("the world's blade", &dropped), ("the world's grip", &grip)] {
            assert!(
                mesh.attribute(Mesh::ATTRIBUTE_COLOR).is_none(),
                "{name} carries vertex colours, so a shared mesh has a per-item opinion again"
            );
        }

        // And the two pieces still meet: the grip's top sits where the blade's guard ends.
        let (_, grip_top) = extent(&positions(&grip), 1);
        let guard_bottom = blade_base() - GUARD_SIZE.y;
        assert!(
            (grip_top - guard_bottom).abs() < 1e-6,
            "the grip's top is at {grip_top} and the guard ends at {guard_bottom}, so the \
             world's sword has a gap in it"
        );
    }

    /// The turned grip's ring vertices, picked out of the merged sword.
    ///
    /// **The half-depth trick the boxes are selected by does not survive a turned grip**, and
    /// that is the one thing #419 costs the tests around it. A box grip put four corners on
    /// each of `|z| == GRIP_SIZE.z / 2`; a cylinder of [`GRIP_SIDES`] puts a vertex there only
    /// where `sin θ` is exactly ±1, which for eighteen even sides is nowhere. A selector
    /// written against the box therefore finds *nothing* — and it says so rather than
    /// silently making its caller's assertions vacuous, which is how this was noticed.
    ///
    /// What identifies the grip now is its **radius**: every ring vertex is exactly
    /// `GRIP_SIZE.x / 2` from the sword's axis, and no corner of the guard (0.0240) or the
    /// pommel (0.0124) is. That is a stronger selector than the depth was, because it is the
    /// property the grip has *because* it is turned.
    fn grip_ring(positions: &[[f32; 3]], translation: Vec3) -> Vec<[f32; 3]> {
        const EPSILON: f32 = 1e-6;
        let radius = GRIP_SIZE.x / 2.0;
        let high = blade_base() + translation.y - GUARD_SIZE.y;
        let low = high - GRIP_SIZE.y;
        positions
            .iter()
            .copied()
            .filter(|point| {
                let across = (point[0] - translation.x).hypot(point[2] - translation.z);
                (across - radius).abs() < EPSILON
                    && point[1] >= low - EPSILON
                    && point[1] <= high + EPSILON
            })
            .collect()
    }

    /// The two parts of one sword that the hand is closed over: everything between the
    /// pommel's far end and the guard's rearward face.
    struct Hilt {
        item_id: u16,
        grip: Vec<[f32; 3]>,
        pommel: Vec<[f32; 3]>,
    }

    /// That section, read out of the real merged sword mesh rather than rebuilt from the
    /// constants the measurements are meant to be independent of.
    ///
    /// Each furniture box has a unique half-depth, so its corners identify it after the parts
    /// have been merged — a `Y`-only selection would also collect its neighbours on their
    /// shared planes, and `GRIP_SIZE.y == HAND_SIZE.y` means *every* grip corner sits on one
    /// of those planes. The extents are checked against the part sizes afterwards, so a
    /// selector that silently found nothing cannot make the caller's assertions vacuous.
    fn hilt_corners(item_id: u16) -> Hilt {
        const EPSILON: f32 = 1e-6;
        let translation = item_translation(ItemShape::Blade);
        let sword = item_mesh(item_id, ItemShape::Blade).translated_by(translation);
        let corners = positions(&sword);

        let guard_low = blade_base() + translation.y - GUARD_SIZE.y;
        let grip_low = guard_low - GRIP_SIZE.y;
        let pommel_low = grip_low - POMMEL_SIZE.y;
        let part = |half_depth: f32, low: f32, high: f32| -> Vec<[f32; 3]> {
            corners
                .iter()
                .copied()
                .filter(|corner| {
                    ((corner[2] - translation.z).abs() - half_depth).abs() < EPSILON
                        && corner[1] >= low - EPSILON
                        && corner[1] <= high + EPSILON
                })
                .collect()
        };
        let _ = guard_low;
        let grip = grip_ring(&corners, translation);
        let pommel = part(POMMEL_SIZE.z / 2.0, pommel_low, grip_low);

        // The pommel is still a box and is still checked as one.
        assert!(!pommel.is_empty(), "no pommel corners were selected");
        for axis in 0..3 {
            let (low, high) = extent(&pommel, axis);
            assert!(
                (high - low - POMMEL_SIZE[axis]).abs() < EPSILON,
                "the pommel selection spans {} on axis {axis} and the part is {}",
                high - low,
                POMMEL_SIZE[axis]
            );
        }

        // **The grip is checked as inscribed rather than as equal**, which is what a turned
        // grip is: full height, and never outside the box's width or depth. Eighteen even
        // sides put a vertex at each end of `x` and none at either end of `z`, so the depth
        // it spans is one chord short of the box's — that is the cylinder being inside the
        // box, not the selector losing vertices, and [`grip_ring`] would have found nothing
        // at all in that case.
        assert!(!grip.is_empty(), "no grip corners were selected");
        let (grip_low_y, grip_high_y) = extent(&grip, 1);
        assert!(
            (grip_high_y - grip_low_y - GRIP_SIZE.y).abs() < EPSILON,
            "the grip selection spans {} in height and the part is {}",
            grip_high_y - grip_low_y,
            GRIP_SIZE.y
        );
        for axis in [0, 2] {
            let (low, high) = extent(&grip, axis);
            assert!(
                high - low <= GRIP_SIZE[axis] + EPSILON,
                "the grip selection spans {} on axis {axis}, outside the {} box it is turned \
                 inside",
                high - low,
                GRIP_SIZE[axis]
            );
        }
        Hilt {
            item_id,
            grip,
            pommel,
        }
    }

    /// **The hand stays closed over the grip, in every frame of every animation there is.**
    ///
    /// This is the property #393 was filed about — *the sword's grip is behind the hand that
    /// grips it* — measured in camera space from the real merged mesh through
    /// [`presented_transform`] rather than from the constants that built it.
    /// `BLADE_CAMERA_OFFSET` carried the whole hilt 14 mm toward the eye, which left the grip
    /// standing 6.0 mm in front of the fist's nearest vertex at rest and up to 6.2 mm in
    /// front of it mid-swing. It is 7.8 mm behind that vertex at rest now, and behind it at
    /// every pose — and, which is the statement that matters, 0.19 mm behind the surface the
    /// hand actually presents along the grip's own view ray at rest, and never in front of it
    /// anywhere in the sweep.
    ///
    /// **The guard is outside the section deliberately, and the boundary is the reason.** The
    /// issue draws the line at *the guard's rearward face*: below it is hilt, which the hand
    /// closes on and must hide, and above it is the sword, which is how a player knows one is
    /// held at all. The guard is deeper than the fist by design — a guard narrower than the
    /// hand reads as a collar — and it sits entirely above the fist's top face, which
    /// [`both_blades_show_their_guard_grip_and_pommel_around_the_fist`] pins.
    ///
    /// **Two comparisons, not one, and the second is the one that is the property.** The grip
    /// is checked against the fist's nearest *vertex* first: a real quantity the pose changes,
    /// and a cheap one. But "in front of the hand's nearest corner" is not the same question
    /// as "drawn in front of the hand", and the gap between the two is not a rounding error —
    /// the fist's nearest vertex belongs to a finger, and [`fist_mesh`] leaves gaps between
    /// its fingers deliberately. A point behind the knuckles can be looked at straight through
    /// one of those gaps, where the surface is the palm's and 7.8 mm further away.
    /// [`nearest_surface_at`] asks the real question per view ray, of every corner at every
    /// pose.
    ///
    /// **It used to sit behind a short circuit, and the short circuit swallowed it whole.** A
    /// corner behind the fist's nearest vertex was taken to be behind the hand's surface
    /// everywhere — true of a convex solid, and this hand is a palm with five digits standing
    /// proud of it. Every grip corner is inside the fist's bounding box, so the short circuit
    /// took every corner at every pose and the ray query never ran for the grip at all; what
    /// stood in its place was a closure that answered `0.0` for a ray the hand does not cover,
    /// which is the visible-grip case scoring as a pass. Both are gone.
    ///
    /// The sweep is the one
    /// [`every_held_arrangement_clears_the_near_plane_through_every_swing`] walks — the three
    /// blade arcs and the placement bump they can coincide with, plus rest — and the mining
    /// loop besides, which a blade cannot reach: `player/target.rs` sends a swing instead of a
    /// mining intent for both blades and `a_blade_in_hand_sends_a_swing_instead_of_mining`
    /// pins it. It is swept anyway because the grip's containment is what makes the claim, and
    /// containment does not care which animation is playing.
    #[test]
    fn the_hand_stays_closed_over_the_grip_through_every_animation() {
        let hand = fist_mesh();
        let fist = positions(&hand);
        let swords: Vec<Hilt> = [ITEM_RUSTY_SWORD, ITEM_IRON_SWORD]
            .into_iter()
            .map(hilt_corners)
            .collect();

        let mut arcs: Vec<Option<SwingShape>> = vec![Some(SwingShape::Cut)];
        arcs.push(None);
        let mut closest_grip = f32::INFINITY;
        let mut rest_grip = f32::NAN;
        // The grip is asked the per-ray question directly, and both halves of the answer are
        // failures: a ray the hand does not cover at all, and a ray where the grip is in
        // front of the surface the hand presents along it.
        let mut grip_uncovered: Option<String> = None;
        let mut grip_in_front = f32::NEG_INFINITY;
        let mut grip_worst_pose = String::from("no pose");
        let mut grip_behind_at_rest = f32::INFINITY;
        // The worst distance any pommel corner stands in front of the hand's own surface,
        // and the pose it happens in.
        let mut pommel_intrusion = 0.0f32;
        let mut pommel_worst_pose = String::from("no pose");
        let mut pommel_at_rest = 0.0f32;

        for shape in arcs {
            for step in 0..=32u8 {
                for bump in 0..=8u8 {
                    for punch in 0..=4u8 {
                        let animation = HandAnimation {
                            attack: shape.map(|shape| Swing {
                                shape,
                                elapsed: ATTACK_SWING_TIME.mul_f32(f32::from(step) / 32.0),
                            }),
                            bump_elapsed: Some(PLACE_BUMP_TIME.mul_f32(f32::from(bump) / 8.0)),
                            mine_elapsed: Duration::from_secs_f32(
                                f32::from(punch) / (4.0 * MINE_PUNCHES_PER_SECOND),
                            ),
                        };
                        let pose =
                            presented_transform(&animation, Some(ItemShape::Blade), default_fov());
                        let nearest = |corners: &[[f32; 3]]| {
                            corners
                                .iter()
                                .map(|corner| pose.transform_point(Vec3::from_array(*corner)).z)
                                .fold(f32::NEG_INFINITY, f32::max)
                        };
                        let fist_near = nearest(&fist);
                        // How far one corner stands in front of the hand's *own surface along
                        // its view ray*, and `None` where the hand does not cover that ray at
                        // all.
                        //
                        // **The option is returned rather than flattened, because the two
                        // halves of the hilt mean opposite things by it.** For the grip an
                        // uncovered ray *is* the defect — a point the hand does not cover is a
                        // point a player can see — so `None` fails. For the pommel it is the
                        // air below the fist's lower edge where #384 requires the pommel to
                        // hang, so `None` is an answer. Collapsing both to `0.0` here was how
                        // an earlier form of this test scored a visible grip as hidden.
                        let in_front_of_the_hand = |corner: &[f32; 3]| -> Option<f32> {
                            let point = pose.transform_point(Vec3::from_array(*corner));
                            let depth = -point.z;
                            assert!(depth > 0.0, "{corner:?} landed behind the camera");
                            let flat = Vec2::new(point.x / depth, point.y / depth);
                            nearest_surface_at(&hand, &pose, flat).map(|surface| point.z - surface)
                        };
                        for Hilt {
                            item_id,
                            grip,
                            pommel,
                        } in &swords
                        {
                            let grip_margin = fist_near - nearest(grip);
                            assert!(
                                grip_margin > 0.0,
                                "sword {item_id}'s grip stands {} in front of the fist's \
                                 nearest surface in {shape:?} at {step}/32, bump {bump}/8, \
                                 punch {punch}/4",
                                -grip_margin
                            );
                            closest_grip = closest_grip.min(grip_margin);
                            // **The per-ray question, asked of every grip corner at every
                            // pose, with no short circuit in front of it.** There was one:
                            // a corner behind the fist's nearest *vertex* was taken to be
                            // behind the hand's surface everywhere. That is a statement
                            // about a convex solid, and [`fist_mesh`] is a palm with four
                            // fingers and a thumb standing proud of it — a corner can be
                            // behind the knuckles' near face and still be looked at through
                            // the gap between two fingers, where the surface is the palm's
                            // and 7.8 mm further away. Every grip corner is inside the
                            // fist's bounding box, so the short circuit took *every* corner
                            // at *every* pose and the ray query never ran at all.
                            for corner in grip {
                                let Some(front) = in_front_of_the_hand(corner) else {
                                    grip_uncovered.get_or_insert_with(|| {
                                        format!(
                                            "sword {item_id}, corner {corner:?}, {shape:?} at \
                                             {step}/32, bump {bump}/8, punch {punch}/4"
                                        )
                                    });
                                    continue;
                                };
                                if front > grip_in_front {
                                    grip_in_front = front;
                                    grip_worst_pose = format!(
                                        "sword {item_id}, corner {corner:?}, {shape:?} at \
                                         {step}/32, bump {bump}/8, punch {punch}/4"
                                    );
                                }
                                if shape.is_none() && step == 0 && bump == 0 && punch == 0 {
                                    grip_behind_at_rest = grip_behind_at_rest.min(-front);
                                }
                            }
                            if shape.is_none() && step == 0 && bump == 0 && punch == 0 {
                                rest_grip = grip_margin;
                            }
                            // The pommel is deliberately outside the fist, so the strict
                            // containment the grip enjoys cannot be asked of it — only how
                            // far through the hand it reaches where the hand is in the way.
                            // Uncovered is the pommel's normal state and scores zero; the
                            // vertex short circuit that used to stand in front of this loop
                            // is gone for the reason it is gone above, and it was wrong here
                            // in the same direction: the surface along a corner's own ray is
                            // never nearer than the mesh's nearest vertex, so a corner behind
                            // that vertex can still be well in front of the surface.
                            for corner in pommel {
                                let front = in_front_of_the_hand(corner)
                                    .map_or(0.0, |front| front.max(0.0));
                                if shape.is_none() && step == 0 && bump == 0 && punch == 0 {
                                    pommel_at_rest = pommel_at_rest.max(front);
                                }
                                if front > pommel_intrusion {
                                    pommel_intrusion = front;
                                    pommel_worst_pose = format!(
                                        "sword {item_id}, corner {corner:?}, {shape:?} at \
                                         {step}/32, bump {bump}/8, punch {punch}/4"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        // **The grip is asked two different questions, and only the second one is the
        // property.**
        //
        // The first is the cheap one: the grip stays behind the fist's nearest *vertex* at
        // every pose, and the closest it comes is under a millimetre — at the steepest
        // pitches the fist's own nearest corner and the grip's converge, which is arithmetic
        // about two nested boxes rather than the grip emerging. At rest that margin is
        // 7.8 mm, and the offset this replaces put the grip about 6 mm *proud* of the hand at
        // that same pose, so the sign of the number is the fix.
        //
        // But "behind the hand's nearest corner" is not "behind the hand", and reading it as
        // if it were is exactly how the fingers get to lie for the palm: the nearest corner
        // belongs to the digit band, and between two fingers there is no digit band. The
        // question that matters is the per-ray one below, and its numbers are an order of
        // magnitude smaller because the surface it compares against is the palm's.
        assert!(
            closest_grip > 0.0,
            "the grip reaches {closest_grip} of the fist's nearest vertex across the sweep"
        );
        assert!(
            rest_grip > 0.005,
            "the grip sits only {rest_grip} behind the fist's nearest vertex at rest"
        );

        // **The property itself, in two halves, because a grip can fail to be hidden in two
        // ways.**
        //
        // It can be in front of the surface the hand presents along its own view ray, and it
        // can be on a ray the hand does not present a surface on at all. The second is the
        // one an earlier form of this test scored as a pass: an uncovered ray answered `0.0`,
        // which read as "flush with the hand" when what it means is "nothing of the hand is
        // there". For the pommel below, an uncovered ray genuinely is an answer — that is the
        // air under the fist where #384 requires the pommel to hang — and sharing one number
        // between the two is what let the distinction go.
        assert!(
            grip_uncovered.is_none(),
            "the hand covers no part of the ray through the grip at {} — an uncovered grip \
             corner is one a player is looking straight at",
            grip_uncovered.clone().unwrap_or_default()
        );

        // **The grip is never in front of the hand's own surface, and `GRAZE` is arithmetic
        // rather than headroom.**
        //
        // `GRIP_SIZE.y == HAND_SIZE.y`, so all eight grip corners lie exactly *on* the palm's
        // top and bottom face planes — that equality is asserted as a `const` beside
        // [`GRIP_SIZE`] and is how [`item_translation`] places a blade. At the poses where the
        // nearest covering surface is one of those two flush faces, the true answer is exactly
        // zero: the view ray meets the plane at the corner itself. `f32` reaches that zero
        // from two directions — a rigid transform of the corner on one side, a `1/z`-
        // interpolated barycentric of three transformed triangle corners on the other — and
        // the gap between them grows as the plane turns edge-on to the ray.
        //
        // **#421 turned it further edge-on than any pose before it.** The blade's one arc
        // carries pitch, yaw and roll at once where each of the three it replaced carried
        // mostly one, so the flush faces are seen at a sharper angle and the two arithmetics
        // disagree by more: the worst the sweep produces is `4.59e-5`, at 30/32 of a cut with
        // a placement bump in flight, where it used to be `1.98e-6`. The ceiling moves with
        // the measurement — a hundred micrometres, a little over twice the worst reading.
        //
        // **Raising a tolerance is the wrong move when the tolerance is what proves the
        // property, and here it is not.** All eight of the grip's corners lie on one of those
        // two planes by construction, so this sweep can only ever be measuring flush-face
        // arithmetic; what proves the grip is inside the fist is the `const` containment
        // beside [`GRIP_SIZE`], and a point inside a convex solid is behind that solid's
        // surface from every viewpoint outside it. What the ceiling does have to stay under is
        // *visible*: at the default 45° vertical field of view one pixel of a 1080-line
        // viewport spans about 0.14 mm at the hand's depth, so a hundred micrometres is under
        // three quarters of a pixel and the reading itself is under a third of one.
        const GRAZE: f32 = 1e-4;
        assert!(
            grip_in_front <= GRAZE,
            "the grip stands {grip_in_front} in front of the hand's own surface at \
             {grip_worst_pose}, past the {GRAZE} the flush faces can produce by arithmetic"
        );
        // And at rest — the pose the defect was reported from, standing still and looking at
        // the held weapon — it is a real distance behind the palm, not a tie: the 0.2 mm the
        // palm's containment of the grip has in it, carried through perspective.
        assert!(
            grip_behind_at_rest > 0.0001,
            "the grip sits only {grip_behind_at_rest} behind the hand's own surface at rest"
        );
        // **The pommel gets a bound rather than the grip's guarantee, and the reason is that
        // it is the half of the hilt that is *meant* to be outside the hand.**
        //
        // #384 requires it to show below the fist — that is what says the hand is closed on a
        // sword rather than being where the sword begins — so it hangs 22 mm below the model's
        // origin, and all three attack arcs rotate the whole model about that origin. Anything
        // that far below the pivot travels, and part of what it travels across is the hand.
        // Those arcs are #174's and this issue may not touch them, so the honest statement
        // here is a *direction* and a ceiling rather than a guarantee somebody would have to
        // change an animation to keep.
        //
        // At rest — where the defect was reported from, standing still and looking at the
        // held weapon — nothing of the pommel is in front of the hand at all, and
        // `both_blades_show_their_guard_grip_and_pommel_around_the_fist` pins that pose
        // strictly. Mid-swing the worst corner reaches about 27 mm through the hand. That
        // number was 37 mm before this change, because `BLADE_CAMERA_OFFSET` carried the whole
        // hilt 14 mm toward the eye on top of whatever the arc was already doing. 30 mm is the
        // recorded ceiling: it fails on a regression toward the old arrangement and does not
        // pretend the arcs have been fixed.
        //
        // It read 24 mm while the vertex short circuit stood in front of this loop, and the
        // three millimetres between the two are the measure of what that short circuit was
        // hiding: the surface along a corner's own ray is never *nearer* than the mesh's
        // nearest vertex, so skipping a corner for being behind that vertex skips poses where
        // the corner is well in front of the surface it is actually drawn against.
        assert!(
            pommel_at_rest <= 0.0,
            "the pommel stands {pommel_at_rest} in front of the hand at rest"
        );
        assert!(
            pommel_intrusion < 0.030,
            "the pommel stands {pommel_intrusion} in front of the hand's own surface at \
             {pommel_worst_pose}, and 30 mm is the recorded ceiling"
        );
    }

    /// **The blade rises clear of the fist rather than growing out of it.**
    ///
    /// The complaint #384 was filed about is not depth order — #369 and #382 already put the
    /// hilt *in front of* the fist rather than through it — it is that in front of a slab is
    /// still on top of a slab. What matters is the **screen** overlap, so this projects both
    /// through the rest pose the renderer uses and measures how much of the blade's span lands
    /// inside the fist's silhouette. 64% of it did.
    ///
    /// The silhouette is taken as the projected bounding box of the fist's real vertices, which
    /// is a superset of the fist's actual outline — so every count this makes is at least the
    /// true one, and passing it is the stronger statement.
    ///
    /// **The blade is sampled across its section, not along its centreline.** A centreline
    /// says nothing about the edges, and the edges are most of what a player sees of a blade:
    /// the section is [`BLADE_THICKNESS`] across in `X` and [`BLADE_WIDTH`] in `Z`, and both
    /// move a corner's projection — `X` directly, `Z` through the perspective divide. So each
    /// height contributes all six corners of the real lofted section and counts as hidden if
    /// *any* of them lands inside the silhouette, which is strictly stronger than the
    /// centreline test it replaces. #379 is the reminder: its tests passed on one protruding
    /// tip.
    #[test]
    fn a_blade_rises_clear_of_the_fists_silhouette_instead_of_growing_out_of_it() {
        const SAMPLES: usize = 101;

        let rest = presented_transform(
            &HandAnimation::default(),
            Some(ItemShape::Blade),
            default_fov(),
        );
        let project = |point: Vec3| {
            let point = rest.transform_point(point);
            let depth = -point.z;
            assert!(depth > 0.0, "the held arrangement crossed the camera plane");
            Vec2::new(point.x / depth, point.y / depth)
        };

        let fist: Vec<Vec2> = positions(&fist_mesh())
            .into_iter()
            .map(|corner| project(Vec3::from_array(corner)))
            .collect();
        let left = fist.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
        let right = fist.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max);
        let bottom = fist.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
        let top = fist.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max);

        let translation = item_translation(ItemShape::Blade);
        let hidden = (0..SAMPLES)
            .filter(|sample| {
                let fraction = *sample as f32 / (SAMPLES - 1) as f32;
                let y = blade_base() + BLADE_LENGTH * fraction;
                blade_at(y).perimeter().into_iter().any(|corner| {
                    let point = project(corner + translation);
                    (left..=right).contains(&point.x) && (bottom..=top).contains(&point.y)
                })
            })
            .count();
        assert!(
            hidden * 5 <= SAMPLES,
            "{hidden}/{SAMPLES} sampled blade sections put a corner inside the fist's \
             projected silhouette, and a fifth is the ceiling"
        );
    }

    /// Skin comes from the local player's authoritative appearance, item colour from the
    /// display table, and white material identity lets both coexist in one draw.
    #[test]
    fn the_hand_and_item_keep_their_two_authoritative_colours_on_one_material() {
        let mut app = app();
        let look = PlayerLook::new(
            TEST_SKIN,
            0x0011_2233,
            0x0044_5566,
            0x0077_8899,
            crate::net::HairModel::Shaved,
            0x000F_0E0D,
        )
        .expect("the test appearance is legal");
        app.world_mut()
            .resource_mut::<AppearanceInbox>()
            .push(PlayerAppearance {
                entity_id: session().0.entity_id,
                appearance: look,
                name: "Test Character".to_owned(),
                worn_head: 0,
                worn_chest: 0,
                worn_legs: 0,
                worn_offhand: 0,
                level: 1,
            });
        app.update();

        let world = app.world_mut();
        let mut query = world.query::<(&HeldItem, &Mesh3d, &MeshMaterial3d<StandardMaterial>)>();
        let (held, mesh, material) = query.single(world).expect("one held arrangement");
        assert_eq!(held.skin_colour, TEST_SKIN);

        let meshes = world.resource::<Assets<Mesh>>();
        let mesh = meshes.get(&mesh.0).expect("the held mesh");
        let skin = linear_rgb(TEST_SKIN);
        let stone = items::item_linear_rgba(ITEM_STONE);

        // **A shade of one of the two, rather than one of the two exactly.** The composition
        // carries a baked directional shade since #434 — the material is `unlit`, so a face's
        // relief has to be in its colour — and a shade *multiplies*, so what every vertex now
        // carries is one of these two authoritative colours scaled by a number in
        // `SHADE_FLOOR..=1.0`. The claim this test makes is unchanged: two authorities, no
        // third opinion, and nothing brighter than what the tables say.
        // The shared predicate, bounded to the range a shade may take. See [`shade_of`],
        // which reads the peak channel rather than red.
        let within = |colour: [f32; 4], tint: &[f32; 4]| {
            shade_of(colour, tint)
                .is_some_and(|scale| (SHADE_FLOOR - 1e-3..=1.0 + 1e-3).contains(&scale))
        };
        let vertices = raw_tints(mesh);
        assert!(
            !vertices.is_empty(),
            "the held mesh carries no colour at all"
        );
        let (mut skinned, mut stony) = (0, 0);
        for tint in &vertices {
            if within(skin, tint) {
                skinned += 1;
            } else if within(stone, tint) {
                stony += 1;
            } else {
                panic!("a vertex carries {tint:?}, which is a shade of neither authority");
            }
        }
        assert!(skinned > 0, "the mesh has no local skin colour");
        assert!(stony > 0, "the mesh has no item-table colour");

        // And it is genuinely shaded rather than uniformly scaled: a mesh with one shading
        // value everywhere would satisfy every clause above and be as flat as before.
        let mut levels: Vec<i32> = vertices
            .iter()
            .map(|tint| (tint[0] / skin[0].max(f32::EPSILON) * 1e4) as i32)
            .collect();
        levels.sort_unstable();
        levels.dedup();
        assert!(
            levels.len() > 3,
            "the held arrangement carries {} shading levels, so it is still flat",
            levels.len()
        );

        let materials = world.resource::<Assets<StandardMaterial>>();
        assert_eq!(
            materials
                .get(&material.0)
                .expect("the held material")
                .base_color,
            Color::WHITE,
            "the material tinted both vertex colours a second time"
        );
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .insert_resource(session())
            .insert_resource(Inventory::from_stacks(vec![
                InventoryStack {
                    item_id: ITEM_STONE,
                    count: 2,
                    ..Default::default()
                },
                InventoryStack {
                    item_id: ITEM_RAW_COAL,
                    count: 1,
                    ..Default::default()
                },
                InventoryStack {
                    item_id: 0,
                    count: 0,
                    ..Default::default()
                },
                InventoryStack {
                    item_id: u16::MAX,
                    count: 1,
                    ..Default::default()
                },
            ]))
            .insert_resource(SelectedSlot(0))
            .add_plugins(PlayerPlugin);
        app.update();
        app
    }

    fn held(app: &mut App) -> (HeldItem, Visibility, Entity) {
        let world = app.world_mut();
        let mut query = world.query::<(&HeldItem, &Visibility, &ChildOf)>();
        let (item, visibility, parent) = query.single(world).expect("one held view model");
        (*item, *visibility, parent.parent())
    }

    fn off_hand_shield(app: &mut App) -> (Visibility, Transform) {
        let world = app.world_mut();
        let mut query = world.query_filtered::<(&Visibility, &Transform), With<OffHandShield>>();
        let (visibility, transform) = query.single(world).expect("one off-hand shield view model");
        (*visibility, *transform)
    }

    #[test]
    fn authoritative_blocking_shows_a_separate_left_hand_shield() {
        let mut app = app();
        let mut params = session().0;
        params.inventory_slots = 8;
        params.equipment_slots = 4;
        app.insert_resource(Session(params));
        let mut stacks = vec![InventoryStack::default(); 8];
        stacks[7] = InventoryStack {
            item_id: crafting::ITEM_WOODEN_SHIELD,
            count: 1,
            durability: 40,
            max_durability: 40,
        };
        app.insert_resource(Inventory::from_stacks(stacks));
        app.insert_resource(SelfVitals(Some(crate::net::PlayerVitals {
            blocking: true,
            ..crate::net::PlayerVitals::unharmed()
        })));
        app.update();

        let (visibility, resting) = off_hand_shield(&mut app);
        assert_eq!(visibility, Visibility::Visible);
        app.world_mut().write_message(SwingSent {
            item_id: ITEM_RUSTY_SWORD,
        });
        app.update();
        assert_eq!(
            off_hand_shield(&mut app).1,
            resting,
            "the right-hand swing moved the shield arm"
        );

        app.insert_resource(SelfVitals(Some(crate::net::PlayerVitals::unharmed())));
        app.update();
        assert_eq!(off_hand_shield(&mut app).0, Visibility::Hidden);
    }

    #[test]
    fn held_shapes_follow_the_selected_slot_on_that_frame() {
        let mut app = app();
        assert_eq!(held(&mut app).0.shape, Some(ItemShape::Block));

        for (slot, expected) in [
            (1, Some(ItemShape::Material)),
            (2, None),
            (3, Some(ItemShape::Material)),
        ] {
            *app.world_mut().resource_mut::<SelectedSlot>() = SelectedSlot(slot);
            app.update();
            assert_eq!(held(&mut app).0.shape, expected, "slot {slot}");
        }
    }

    #[test]
    fn the_view_model_has_an_origin_anchored_camera_and_its_own_render_layer() {
        let mut app = app();
        let parent = held(&mut app).2;
        assert!(
            app.world().entity(parent).contains::<ViewModelCamera>(),
            "the held item is not under the view-model camera"
        );
        assert_eq!(
            app.world().get::<Transform>(parent),
            Some(&Transform::IDENTITY),
            "the view-model camera inherited a world position"
        );
        assert_eq!(
            app.world().get::<RenderLayers>(parent),
            Some(&RenderLayers::layer(VIEW_MODEL_RENDER_LAYER)),
            "the view-model camera sees the world layer"
        );
        let camera = app
            .world()
            .get::<Camera>(parent)
            .expect("the overlay camera");
        assert_eq!(camera.order, 1, "the hand is not drawn after the world");
        assert!(
            matches!(camera.clear_color, ClearColorConfig::None),
            "the hand pass erases the world behind it"
        );
        assert!(
            !app.world().entity(parent).contains::<IsDefaultUiCamera>(),
            "UI was assigned to the hand-only overlay"
        );
        let world_camera = {
            let world = app.world_mut();
            world
                .query_filtered::<Entity, With<WorldCamera>>()
                .single(world)
                .expect("one world camera")
        };
        assert!(
            app.world()
                .entity(world_camera)
                .contains::<IsDefaultUiCamera>(),
            "the world camera is not the UI default"
        );
        let world = app.world_mut();
        let mut models = world.query_filtered::<&RenderLayers, With<ViewModel>>();
        assert!(
            models
                .iter(world)
                .all(|layers| { *layers == RenderLayers::layer(VIEW_MODEL_RENDER_LAYER) })
        );
        let Projection::Perspective(projection) = app
            .world()
            .get::<Projection>(parent)
            .expect("the view-model camera has a projection")
        else {
            panic!("the view-model camera is perspective");
        };
        let largest_depth = HAND_SIZE
            .z
            .max(BLOCK_EDGE)
            .max(MATERIAL_RADIUS * 2.0)
            .max(BUNDLE_SIZE.z)
            // The sword's widest point is its cross guard, not its blade — the one held
            // shape whose depth is not the constant naming it.
            .max(GUARD_SIZE.z);
        assert!(
            -BASE_DEPTH - largest_depth / 2.0 > projection.near,
            "the held mesh crosses the camera near plane"
        );
    }

    /// Walking changes the world camera and nothing about a resting camera-space model.
    ///
    /// This follows the same f32 path as the mesh shader: propagate each model into world
    /// space, then multiply its world-space vertices by the camera's world-to-clip matrix.
    /// Comparing only `Transform` would miss precision lost between those two operations.
    /// Before the camera split the unchanged locals measured 0.021 px of drift at the
    /// origin and 112.604 px at 8 192 blocks on this 4K projection.
    #[test]
    fn a_resting_view_model_stays_pixel_still_while_the_camera_walks() {
        const VIEWPORT: Vec2 = Vec2::new(3840.0, 2160.0);
        const MAX_PIXEL_DRIFT: f32 = 0.1;

        let mut app = app();
        app.add_plugins(TransformPlugin);
        app.update();
        let presentation_camera = held(&mut app).2;
        let world_camera = {
            let world = app.world_mut();
            world
                .query_filtered::<Entity, With<WorldCamera>>()
                .single(world)
                .expect("one world camera")
        };
        let models: Vec<Entity> = {
            let world = app.world_mut();
            let mut roots = world.query_filtered::<(Entity, Option<&Children>), With<ViewModel>>();
            roots
                .iter(world)
                .flat_map(|(root, children)| {
                    std::iter::once(root)
                        .chain(children.into_iter().flat_map(|children| children.iter()))
                })
                .collect()
        };
        let resting_locals: Vec<Transform> = models
            .iter()
            .map(|entity| {
                *app.world()
                    .get::<Transform>(*entity)
                    .expect("every drawn view-model part has a local transform")
            })
            .collect();
        assert_eq!(models.len(), 4, "both hands and forearms are measured");
        for entity in &models {
            assert_eq!(
                app.world().get::<RenderLayers>(*entity),
                Some(&RenderLayers::layer(VIEW_MODEL_RENDER_LAYER)),
                "a view-model part escaped the camera-relative layer"
            );
        }

        let projected = |world: &World| {
            let camera_global = world
                .get::<GlobalTransform>(presentation_camera)
                .expect("the presentation camera transform was propagated");
            let projection = world
                .get::<Projection>(presentation_camera)
                .expect("the presentation camera has a projection")
                .get_clip_from_view();
            let clip_from_world = projection * camera_global.to_matrix().inverse();
            let meshes = world.resource::<Assets<Mesh>>();

            models
                .iter()
                .flat_map(|entity| {
                    let model = world
                        .get::<GlobalTransform>(*entity)
                        .expect("the model transform was propagated")
                        .to_matrix();
                    let handle = world
                        .get::<Mesh3d>(*entity)
                        .expect("every view-model part draws a mesh");
                    let mesh = meshes.get(&handle.0).expect("the view-model mesh exists");
                    let Some(VertexAttributeValues::Float32x3(points)) =
                        mesh.attribute(Mesh::ATTRIBUTE_POSITION)
                    else {
                        panic!("the view-model mesh carries Float32x3 positions")
                    };
                    points.iter().map(move |point| {
                        let world = model.transform_point3(Vec3::from_array(*point));
                        let clip = clip_from_world * world.extend(1.0);
                        (clip.xy() / clip.w * 0.5 + Vec2::splat(0.5)) * VIEWPORT
                    })
                })
                .collect::<Vec<_>>()
        };

        for origin in [Vec3::ZERO, Vec3::splat(8_192.0)] {
            let mut baseline = None;
            let mut worst = 0.0_f32;
            for frame in 0..64 {
                let walked = frame as f32;
                let transform = Transform {
                    translation: origin
                        + Vec3::new(walked * 0.037, walked * 0.003, -walked * 0.029),
                    rotation: Quat::from_euler(
                        EulerRot::YXZ,
                        walked * 0.0017,
                        -walked * 0.0009,
                        0.0,
                    ),
                    ..default()
                };
                *app.world_mut()
                    .get_mut::<Transform>(world_camera)
                    .expect("the world camera") = transform;
                app.update();
                assert_eq!(
                    app.world().get::<Transform>(world_camera),
                    Some(&transform),
                    "another system rewrote the camera walk"
                );

                for (entity, resting) in models.iter().zip(&resting_locals) {
                    assert_eq!(
                        app.world().get::<Transform>(*entity),
                        Some(resting),
                        "camera movement rewrote a view-model local transform"
                    );
                }

                let now = projected(app.world());
                assert!(!now.is_empty(), "the view-model vertices are measured");
                assert!(now.iter().all(|point| point.is_finite()));
                let reference = baseline.get_or_insert_with(|| now.clone());
                for (at, first) in now.iter().zip(reference.iter()) {
                    worst = worst.max(at.distance(*first));
                }
            }
            assert!(
                worst <= MAX_PIXEL_DRIFT,
                "a resting view model drifted {worst:.3} px while the camera walked from {origin:?}"
            );
        }
    }

    #[test]
    fn unknown_items_use_a_distinct_shape_and_the_palette_fallback() {
        let mut app = app();
        *app.world_mut().resource_mut::<SelectedSlot>() = SelectedSlot(3);
        app.update();

        let world = app.world_mut();
        let mut query = world.query::<(&HeldItem, &Mesh3d, &MeshMaterial3d<StandardMaterial>)>();
        let (held, mesh, material) = query.single(world).expect("one held item");
        assert_eq!(held.shape, Some(ItemShape::Material));
        assert_eq!(
            world
                .resource::<Assets<StandardMaterial>>()
                .get(&material.0)
                .expect("the held material")
                .base_color,
            Color::WHITE
        );
        let colours = tints(
            world
                .resource::<Assets<Mesh>>()
                .get(&mesh.0)
                .expect("the held mesh"),
        );
        let fallback =
            palette::linear_rgba(u16::MAX).map(|channel| (channel * 255.0).round() as u8);
        assert!(
            colours.contains(&fallback),
            "the item vertices do not carry the palette fallback"
        );
    }

    #[test]
    fn third_person_hides_the_view_model_without_removing_it() {
        // **The bug this file had**: the model is a child of the camera, and #172 moved the
        // camera four blocks back without giving this system the term that removes a
        // first-person conceit there — so the held item floated between the camera and the
        // character (#194).
        //
        // Asserted on the entity as well as the visibility, because *without removing it* is
        // half the contract: the model is the same one afterwards, so a toggle costs no mesh
        // and no material.
        let mut app = app();
        let (_, visibility, _) = held(&mut app);
        assert_eq!(visibility, Visibility::Visible, "first person draws it");
        let before = held(&mut app).0;

        *app.world_mut().resource_mut::<ViewMode>() = ViewMode::ThirdPerson;
        app.update();
        assert_eq!(held(&mut app).1, Visibility::Hidden);
        assert_eq!(
            held(&mut app).0,
            before,
            "the view toggle rebuilt the model instead of hiding it"
        );

        *app.world_mut().resource_mut::<ViewMode>() = ViewMode::FirstPerson;
        app.update();
        assert_eq!(held(&mut app).1, Visibility::Visible);
    }

    #[test]
    fn the_authoritative_mount_hides_the_item_and_cancels_every_hand_arc() {
        let mut app = app();
        app.insert_resource(HandAnimation {
            mine_elapsed: Duration::from_secs(1),
            bump_elapsed: Some(Duration::from_millis(20)),
            attack: None,
        });
        app.world_mut().resource_mut::<SnapshotInbox>().push(
            Snapshot {
                server_tick: 1,
                mounts: vec![MountState {
                    entity_id: session().0.entity_id,
                    mount: MountKind::BrownHorse,
                }],
                ..Default::default()
            },
            std::time::Instant::now(),
        );
        app.update();

        let (mounted, visibility, _) = held(&mut app);
        assert_eq!(visibility, Visibility::Hidden);
        assert_eq!(mounted.shape, None);
        assert_eq!(
            *app.world().resource::<HandAnimation>(),
            HandAnimation::default()
        );

        app.world_mut().resource_mut::<SnapshotInbox>().push(
            Snapshot {
                server_tick: 2,
                ..Default::default()
            },
            std::time::Instant::now(),
        );
        app.update();
        assert_eq!(held(&mut app).1, Visibility::Visible);
        assert_eq!(held(&mut app).0.shape, Some(ItemShape::Block));
    }

    #[test]
    fn inventory_and_menu_hide_the_view_model_without_removing_it() {
        let mut app = app();
        assert_eq!(held(&mut app).1, Visibility::Visible);

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Chat;
        app.update();
        assert_eq!(
            held(&mut app).1,
            Visibility::Visible,
            "chat keeps the held item in the live world"
        );

        for mode in [InputMode::Inventory, InputMode::Menu] {
            *app.world_mut().resource_mut::<InputMode>() = mode;
            app.update();
            assert_eq!(held(&mut app).1, Visibility::Hidden, "mode {mode:?}");
        }
    }

    #[test]
    fn mining_loops_while_placement_is_one_distinct_bump() {
        let resting = animated_transform(&HandAnimation::default(), default_fov());
        let swinging = animated_transform(
            &HandAnimation {
                mine_elapsed: Duration::from_millis(50),
                bump_elapsed: None,
                ..Default::default()
            },
            default_fov(),
        );
        let bumping = animated_transform(
            &HandAnimation {
                mine_elapsed: Duration::ZERO,
                bump_elapsed: Some(PLACE_BUMP_TIME / 2),
                ..Default::default()
            },
            default_fov(),
        );

        assert_ne!(swinging.rotation, resting.rotation, "mining did not swing");
        assert_eq!(
            animated_transform(
                &HandAnimation {
                    mine_elapsed: Duration::ZERO,
                    bump_elapsed: None,
                    ..Default::default()
                },
                default_fov()
            ),
            resting,
            "stopping mining did not return to rest"
        );
        assert!(
            bumping.translation.z > resting.translation.z,
            "placement did not make its short forward bump"
        );
        assert_ne!(
            bumping.rotation, swinging.rotation,
            "placement reused the mining pose"
        );
    }
    /// The blade is a shape of its own, so the thing that swings does not look like the
    /// thing that places.
    #[test]
    fn the_rusty_sword_is_held_as_a_blade() {
        let blade = selected_appearance(Some(InventoryStack {
            item_id: combat::ITEM_RUSTY_SWORD,
            count: 1,
            durability: 100,
            max_durability: 100,
        }));
        assert_eq!(blade.shape, Some(ItemShape::Blade));
        assert_eq!(blade.item_id, Some(combat::ITEM_RUSTY_SWORD));

        // A worn-through blade is still a blade in the hand. Whether it *swings* is
        // `super::combat`'s question and the server's answer; this module only draws.
        let worn = selected_appearance(Some(InventoryStack {
            item_id: combat::ITEM_RUSTY_SWORD,
            count: 1,
            durability: 0,
            max_durability: 100,
        }));
        assert_eq!(worn.shape, Some(ItemShape::Blade));

        // And the mapping is cosmetic: it cannot turn another item into a weapon.
        let stone = selected_appearance(Some(InventoryStack {
            item_id: ITEM_STONE,
            count: 1,
            ..Default::default()
        }));
        assert_eq!(stone.shape, Some(ItemShape::Block));
    }

    /// The four items that plant an entity rather than a voxel. The hand is where a
    /// player sees which of them the place press is about to ask for, so a bundle is its
    /// own shape rather than another cube.
    #[test]
    fn every_carried_structure_is_held_as_a_bundle() {
        let bundles = [
            structures::ITEM_TENT,
            structures::ITEM_FORGE,
            structures::ITEM_CAMPFIRE,
            structures::ITEM_RUNESTONE,
        ];
        let carried = bundles.map(|item_id| {
            let held = selected_appearance(Some(InventoryStack {
                item_id,
                count: 1,
                ..Default::default()
            }));
            assert_eq!(held.shape, Some(ItemShape::Bundle), "item {item_id}");
            assert_eq!(held.item_id, Some(item_id));
            held
        });

        // Four bundles, four colours: canvas, iron, firewood and cut stone are what a
        // player is carrying, and two that looked alike would be slots they had to count.
        for first in 0..bundles.len() {
            for second in first + 1..bundles.len() {
                assert_ne!(
                    carried[first].item_colour, carried[second].item_colour,
                    "items {} and {} are carried in the same colour",
                    bundles[first], bundles[second]
                );
            }
        }

        // And an id none of them names is still the placeholder rather than a bundle.
        let unknown = selected_appearance(Some(InventoryStack {
            item_id: u16::MAX,
            count: 1,
            ..Default::default()
        }));
        assert_eq!(unknown.shape, Some(ItemShape::Material));
    }

    #[test]
    fn a_held_tent_is_a_rolled_bundle_with_brown_straps() {
        let item = item_mesh(structures::ITEM_TENT, ItemShape::Bundle);
        assert!(
            item.count_vertices() > Mesh::from(Cuboid::from_size(BUNDLE_SIZE)).count_vertices(),
            "the held bundle is still one parallelepiped"
        );
        let points = positions(&item);
        for (axis, expected) in BUNDLE_SIZE.to_array().into_iter().enumerate() {
            let (low, high) = extent(&points, axis);
            assert!(
                (high - low - expected).abs() < 1e-6,
                "the held bundle spans {} on axis {axis}, want {expected}",
                high - low
            );
        }

        let appearance = selected_appearance(Some(InventoryStack {
            item_id: structures::ITEM_TENT,
            count: 1,
            ..Default::default()
        }));
        // **Shades of the two, since #434 bakes a directional shade into the composition.**
        // The claim is the one it always was — the roll is the tent's canvas and the straps
        // are their own brown — and a shade multiplies, so what has to hold is that each
        // vertex is one of the two scaled by a number in `SHADE_FLOOR..=1.0`.
        let vertices = raw_tints(&held_mesh(TEST_SKIN, appearance));
        let canvas = appearance
            .item_colour
            .expect("the tent has a canvas colour");
        let straps = bundle_strap_linear_rgba();
        let skin = linear_rgb(TEST_SKIN);
        // The shared predicate, bounded to the range a shade may take. See [`shade_of`],
        // which reads the peak channel rather than red.
        let within = |colour: [f32; 4], tint: &[f32; 4]| {
            shade_of(colour, tint)
                .is_some_and(|scale| (SHADE_FLOOR - 1e-3..=1.0 + 1e-3).contains(&scale))
        };
        assert!(
            vertices.iter().any(|tint| within(canvas, tint)),
            "the roll lost the tent colour"
        );
        assert!(
            vertices.iter().any(|tint| within(straps, tint)),
            "the two straps are not brown"
        );
        assert_ne!(canvas, straps, "the straps disappeared into the canvas");
        // The hand is in the same buffer and is neither, which is what stops the two clauses
        // above from being satisfied by skin that happens to scale onto one of them.
        assert!(
            vertices.iter().any(|tint| within(skin, tint)),
            "the hand is not in the composition at all"
        );
    }

    /// The forge's two products, once a player has made one.
    ///
    /// The blade is a blade — the shape says *this swings* rather than *this places* — and
    /// it is a different colour from the rusty one, because a pack holding both is two
    /// slots a player has to tell apart. The stone is a consumable and reads as material.
    #[test]
    fn the_iron_blade_and_the_sharpening_stone_have_shapes_of_their_own() {
        let iron = selected_appearance(Some(InventoryStack {
            item_id: crafting::ITEM_IRON_SWORD,
            count: 1,
            durability: 200,
            max_durability: 200,
        }));
        assert_eq!(iron.shape, Some(ItemShape::Blade));
        assert_eq!(iron.item_id, Some(crafting::ITEM_IRON_SWORD));

        let rusty = selected_appearance(Some(InventoryStack {
            item_id: combat::ITEM_RUSTY_SWORD,
            count: 1,
            durability: 100,
            max_durability: 100,
        }));
        assert_ne!(
            iron.item_colour, rusty.item_colour,
            "the two blades are carried in the same colour"
        );

        let stone = selected_appearance(Some(InventoryStack {
            item_id: crafting::ITEM_SHARPENING_STONE,
            count: 4,
            ..Default::default()
        }));
        assert_eq!(stone.shape, Some(ItemShape::Material));
        assert_eq!(stone.item_id, Some(crafting::ITEM_SHARPENING_STONE));

        // Neither is the placeholder any more: an id this build knows must not draw as a
        // version skew.
        for known in [crafting::ITEM_IRON_SWORD, crafting::ITEM_SHARPENING_STONE] {
            assert_ne!(
                items::item_linear_rgba(known),
                palette::linear_rgba(u16::MAX),
                "item {known} still draws as an unknown id"
            );
        }
    }

    /// The panel and the hand read one opinion, so a stack cannot be two colours at once.
    #[test]
    fn the_swatch_a_panel_draws_is_the_one_the_hand_is_built_from() {
        for item_id in [
            ITEM_STONE,
            ITEM_LOG,
            ITEM_RAW_COAL,
            ITEM_RAW_IRON,
            combat::ITEM_RUSTY_SWORD,
            structures::ITEM_TENT,
            structures::ITEM_FORGE,
            crafting::ITEM_IRON_SWORD,
            crafting::ITEM_SHARPENING_STONE,
        ] {
            assert_eq!(
                items::item_linear_rgba(item_id),
                selected_appearance(Some(InventoryStack {
                    item_id,
                    count: 1,
                    ..Default::default()
                }))
                .item_colour
                .expect("an item has a colour"),
                "item {item_id}"
            );
        }

        // And an id from a newer contract still reaches the palette's loud placeholder
        // rather than a plausible shade this module invented.
        assert_eq!(
            items::item_linear_rgba(u16::MAX),
            palette::linear_rgba(u16::MAX)
        );
    }

    /// One transform for a swing of the named shape, `fraction` of the way through its arc.
    fn mid_swing(shape: SwingShape, fraction: f32) -> Transform {
        animated_transform(
            &HandAnimation {
                attack: Some(Swing {
                    shape,
                    elapsed: ATTACK_SWING_TIME.mul_f32(fraction),
                }),
                ..Default::default()
            },
            default_fov(),
        )
    }

    /// One swing per message, on the frame the request left — and every shape settles.
    ///
    /// Swept over [`SwingShape::ALL`] rather than over the one arc this used to be: three
    /// shapes are three chances to leave the hand leaning, and the whole reason the pose is
    /// four loose terms added to rest is that each of them returns to zero.
    #[test]
    fn a_sent_swing_moves_the_view_model_and_then_settles() {
        let resting = animated_transform(&HandAnimation::default(), default_fov());

        for shape in SwingShape::ALL {
            let swinging = mid_swing(shape, 0.5);
            assert_ne!(
                resting, swinging,
                "{shape:?} left the view model exactly where it was"
            );

            // The arc is out and back: its ends match rest, so nothing is left leaning.
            // Compared with a tolerance rather than exactly: `sin(PI)` is an ulp away from
            // zero, not zero, so an exact comparison here would be asserting the accuracy
            // of the sine rather than the shape of the arc.
            for (edge, at) in [("started", 0.0), ("finished", 1.0)] {
                let pose = mid_swing(shape, at);
                assert!(
                    pose.rotation.abs_diff_eq(resting.rotation, 1e-5),
                    "{shape:?} {edge} leaning at {:?}",
                    pose.rotation
                );
                assert!(
                    pose.translation.abs_diff_eq(resting.translation, 1e-5),
                    "{shape:?} {edge} reaching at {:?}",
                    pose.translation
                );
            }
        }
    }

    /// **The stroke falls from the upper right to the lower left, measured as the tip's path
    /// on screen.**
    ///
    /// #231 asked for three arcs and this test's ancestor pinned that each led with a channel
    /// of its own — the cut was pitch, the slash yaw, the thrust reach. #421 asked for the
    /// opposite and the assertion has to be the opposite too: a diagonal leads with *two*
    /// channels at once, and what makes it one stroke is that neither dominates.
    ///
    /// **It reads the tip through the real transform rather than the terms in [`SwingPose`],**
    /// and that is the whole reason it is worth having. Three non-zero numbers are also what a
    /// chop with a wobble has; what a player sees is where the point of the sword goes.
    ///
    /// Three properties, each ruling out a different wrong arc:
    ///
    /// - the tip descends and crosses inboard **monotonically** — a stroke that wandered back
    ///   up or out on the way would read as a flourish rather than as a cut;
    /// - neither displacement is more than a quarter larger than the other, which is what
    ///   *diagonal* means here and what rules out a chop with a lean or a sweep with a dip;
    /// - it ends where it began, which the shared envelope gives and this checks anyway,
    ///   because that envelope is one edit away from every other arc's.
    #[test]
    fn the_blade_cuts_from_the_upper_right_down_to_the_lower_left() {
        const STEPS: usize = 32;

        // The point of the blade, in the composition's own space: the sword's own tip, moved
        // by the placement every held blade takes.
        let tip = sword_blade_span(SWORD_LENGTH)[1] + item_translation(ItemShape::Blade);

        let at = |fraction: f32| {
            let animation = HandAnimation {
                attack: Some(Swing {
                    shape: SwingShape::Cut,
                    elapsed: ATTACK_SWING_TIME.mul_f32(fraction),
                }),
                ..Default::default()
            };
            let pose = presented_transform(&animation, Some(ItemShape::Blade), default_fov());
            let point = pose.transform_point(tip);
            let depth = -point.z;
            assert!(
                depth > 0.0,
                "the tip crossed the camera plane at {fraction}"
            );
            Vec2::new(point.x / depth, point.y / depth)
        };

        let rest = at(0.0);
        let peak = at(0.5);

        // Down, and inboard. The model is held to the right of the frame, so *inboard* is the
        // direction of falling `x`, and the crosshair is at this projection's origin.
        assert!(
            peak.y < rest.y,
            "the tip reaches {} against {} at rest, so the cut does not fall",
            peak.y,
            rest.y
        );
        assert!(
            peak.x < rest.x,
            "the tip reaches {} against {} at rest, so the cut does not cross inboard",
            peak.x,
            rest.x
        );

        // Monotonic on both axes over the outward half of the arc.
        let mut previous = rest;
        for step in 1..=STEPS {
            let now = at(0.5 * step as f32 / STEPS as f32);
            assert!(
                now.y <= previous.y && now.x <= previous.x,
                "at {step}/{STEPS} of the way out the tip moved to {now:?} from {previous:?}, \
                 so the stroke doubles back"
            );
            previous = now;
        }

        // **Diagonal rather than a chop with a lean or a sweep with a dip.** A quarter is the
        // band [`CUT_PITCH_RADIANS`] and [`CUT_YAW_RADIANS`] were derived against.
        let (fall, cross) = ((rest.y - peak.y).abs(), (rest.x - peak.x).abs());
        let (small, large) = (fall.min(cross), fall.max(cross));
        assert!(
            large <= small * 1.25,
            "the tip falls {fall} and crosses {cross}, a {:.2}:1 stroke rather than a diagonal",
            large / small
        );

        // And it comes home.
        let end = at(1.0);
        assert!(
            (end - rest).length() < 1e-6,
            "the stroke ends at {end:?} against {rest:?} at rest"
        );
    }

    /// **A punch, not a wobble.** The hand reaches for the block, comes back, and the
    /// cycle closes on rest so the loop repeats from the same place however long it runs.
    #[test]
    fn the_mining_punch_reaches_for_the_block_and_comes_back() {
        let cycle = Duration::from_secs_f32(1.0 / MINE_PUNCHES_PER_SECOND);
        let resting = animated_transform(&HandAnimation::default(), default_fov());
        let extended = animated_transform(
            &HandAnimation {
                mine_elapsed: cycle / 2,
                ..Default::default()
            },
            default_fov(),
        );

        // Away from the camera is -Z, so the fist reaches for what it is breaking.
        assert!(
            extended.translation.z < resting.translation.z,
            "the punch never carried the hand toward the block: {} against {} at rest",
            extended.translation.z,
            resting.translation.z
        );

        // And the other way from a placement, which draws back from the block it just set
        // down. Two animations sharing an axis have to be told apart at a glance.
        let bumping = animated_transform(
            &HandAnimation {
                bump_elapsed: Some(PLACE_BUMP_TIME / 2),
                ..Default::default()
            },
            default_fov(),
        );
        assert!(
            bumping.translation.z > resting.translation.z,
            "the placement bump now travels the same way as the mining punch"
        );

        // Nothing is left extended or leaning at the end of one punch. Compared with a
        // tolerance for the reason the attack arc above is: `cos(TAU)` is an ulp from one.
        let closed = animated_transform(
            &HandAnimation {
                mine_elapsed: cycle,
                ..Default::default()
            },
            default_fov(),
        );
        assert!(
            closed.translation.abs_diff_eq(resting.translation, 1e-5),
            "the punch left the hand out at {:?}",
            closed.translation
        );
        assert!(
            closed.rotation.abs_diff_eq(resting.rotation, 1e-5),
            "the punch left the hand leaning at {:?}",
            closed.rotation
        );

        // No part of the cycle pulls the hand back *behind* rest. That is the whole
        // difference between a punch and a shake, and it is the property a sine — which is
        // symmetric about rest — would not have had.
        for step in 0u8..=64 {
            let at = animated_transform(
                &HandAnimation {
                    mine_elapsed: cycle.mul_f32(f32::from(step) / 64.0),
                    ..Default::default()
                },
                default_fov(),
            );
            assert!(
                at.translation.z <= resting.translation.z + 1e-6,
                "the punch pulled the hand back behind rest {step}/64 of the way through"
            );
        }
    }

    /// The view model with nothing beside it that writes [`MiningFeedback`].
    ///
    /// The full [`app`] above cannot answer this question: `BlockTargetPlugin` recomputes
    /// the feedback from the inbox and the crosshair every frame, and with no chunks
    /// loaded the raycast answers "nothing targeted" — which is one of the states that
    /// clears it. Here the test plays the server, which is the only way to say *the server
    /// reported this* and still have it be true when `animate_view_model` reads it.
    fn hand_only_app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            // What sibling plugins provide in the game: the aimed voxel from
            // `BlockTargetPlugin`, the swing message from `CombatPlugin`, the consume
            // message and the pack from `InventoryPlugin`, and the mouse from Bevy's input
            // plugin.
            .init_resource::<BlockTarget>()
            .init_resource::<ButtonInput<MouseButton>>()
            .add_message::<SwingSent>()
            .add_message::<ConsumeSent>()
            .init_resource::<Inventory>()
            .init_resource::<InputMode>()
            .insert_resource(SelectedSlot(0))
            .add_plugins(HandsPlugin);
        app.update();
        app
    }

    /// **The loop is the server's to start and to stop, and the button's to do neither.**
    ///
    /// The three ways mining ends — the block broke, the player looked away, the request
    /// was refused and nothing came back — are already one fact by the time this module
    /// sees them: `MiningFeedback` reporting nothing. So the test says it the way the code
    /// reads it, and holds the button down throughout to show what is *not* driving this.
    #[test]
    fn the_mining_loop_starts_and_stops_on_the_servers_progress_alone() {
        const STEP: Duration = Duration::from_millis(16);

        let mut app = hand_only_app();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(STEP));

        // A held button and a voxel under the crosshair, and not one word from the server.
        // A hand on a local clock would already be punching here.
        app.world_mut()
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Left);
        *app.world_mut().resource_mut::<BlockTarget>() = BlockTarget(Some(BlockHit {
            block: IVec3::ZERO,
            face: IVec3::Y,
        }));
        app.update();
        app.update();
        assert_eq!(
            app.world().resource::<HandAnimation>().mine_elapsed,
            Duration::ZERO,
            "the hand punched on the press, before the server had granted anything"
        );

        // The server reports progress. Now, and only now, the loop runs.
        *app.world_mut().resource_mut::<MiningFeedback>() = MiningFeedback::for_test(64);
        app.update();
        app.update();
        assert_eq!(
            app.world().resource::<HandAnimation>().mine_elapsed,
            STEP * 2,
            "the server's progress did not start the loop"
        );

        // And the moment the server stops saying so, it resets rather than winding down.
        *app.world_mut().resource_mut::<MiningFeedback>() = MiningFeedback::default();
        app.update();
        assert_eq!(
            app.world().resource::<HandAnimation>().mine_elapsed,
            Duration::ZERO,
            "the hand kept punching after the server stopped reporting progress"
        );

        // The half that makes the two assertions above mean anything.
        assert!(
            app.world()
                .resource::<ButtonInput<MouseButton>>()
                .pressed(MouseButton::Left),
            "the button was released, so this test proved nothing about it"
        );
    }

    /// **The pack opening stops the hand, whatever the last byte from the server said.**
    ///
    /// The gate is [`HandIntent::playing`], and it is UI state rather than a second
    /// opinion about mining: it decides whether this frame's hand belongs to the world,
    /// not whether the block is coming apart. What makes it necessary is that the byte
    /// outlives the transition — nothing orders the input mode before the feedback that
    /// reads it, so the frame the inventory opens on can still be holding the progress
    /// computed while the player was aiming.
    ///
    /// So the test says exactly that: the server's answer is left untouched and the button
    /// is left held down, and both are asserted at the end. If either had changed, the
    /// reset below would be evidence about something other than the mode.
    #[test]
    fn a_mode_that_is_not_playing_stops_the_hand_the_server_is_still_feeding() {
        const STEP: Duration = Duration::from_millis(16);

        for mode in [InputMode::Chat, InputMode::Inventory, InputMode::Menu] {
            let mut app = hand_only_app();
            app.insert_resource(TimeUpdateStrategy::ManualDuration(STEP));

            // A held button, a voxel under the crosshair, and the server reporting that it
            // is coming apart: the loop is running.
            app.world_mut()
                .resource_mut::<ButtonInput<MouseButton>>()
                .press(MouseButton::Left);
            *app.world_mut().resource_mut::<BlockTarget>() = BlockTarget(Some(BlockHit {
                block: IVec3::ZERO,
                face: IVec3::Y,
            }));
            *app.world_mut().resource_mut::<MiningFeedback>() = MiningFeedback::for_test(64);
            app.update();
            app.update();
            assert_eq!(
                app.world().resource::<HandAnimation>().mine_elapsed,
                STEP * 2,
                "{mode:?}: the loop never started, so nothing below is about stopping it"
            );

            // The screen changes hands. The server has said nothing new.
            *app.world_mut().resource_mut::<InputMode>() = mode;
            app.update();
            assert_eq!(
                app.world().resource::<HandAnimation>().mine_elapsed,
                Duration::ZERO,
                "{mode:?}: the hand kept punching while the UI owned the screen"
            );

            // The two halves that make that assertion mean anything.
            assert!(
                app.world()
                    .resource::<ButtonInput<MouseButton>>()
                    .pressed(MouseButton::Left),
                "{mode:?}: the button was released, so this test proved nothing about it"
            );
            assert_ne!(
                app.world().resource::<MiningFeedback>().progress(),
                0,
                "{mode:?}: the server's progress was cleared, so the mode gate proved nothing"
            );
        }
    }

    /// Runs frames until the arc in flight has finished, or gives up and says so.
    ///
    /// Bounded rather than a `while`: a test that hangs when the animation stops ending
    /// tells nobody anything, and the bound is comfortably past the frames one swing takes.
    fn let_the_swing_finish(app: &mut App) {
        for _ in 0..256 {
            if app.world().resource::<HandAnimation>().attack.is_none() {
                return;
            }
            app.update();
        }
        panic!("a swing was still in flight after 256 frames");
    }

    /// **The animation is driven by the request leaving, and by nothing coming back.**
    ///
    /// There is no session here, no snapshot, no inbound frame of any kind — which is exactly
    /// the state a player is in when the server refuses a swing, because a refused blow
    /// produces no reply at all. Six presses still draw six arcs, because what started them
    /// was the asking.
    ///
    /// **This is what survives of the rotation's test, and it is the half worth keeping.** It
    /// used to assert that six presses drew all three shapes and never one twice running;
    /// #421 leaves one arc, so *which* shape played is no longer a question. What was never
    /// about the rotation is the clause underneath it — that a swing nobody answers still
    /// animates — and that is a real property of a client whose server may say nothing at
    /// all. Deleting the test with the rotation would have taken it along by accident.
    #[test]
    fn a_swing_no_server_answers_still_plays_every_time_it_is_asked_for() {
        const STEP: Duration = Duration::from_millis(16);

        let mut app = hand_only_app();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(STEP));

        let mut drawn = Vec::new();
        for press in 0..6 {
            app.world_mut().write_message(SwingSent {
                item_id: ITEM_RUSTY_SWORD,
            });
            app.update();
            let swing = app
                .world()
                .resource::<HandAnimation>()
                .attack
                .unwrap_or_else(|| panic!("press {press} sent a swing that never played"));
            drawn.push(swing.shape);
            let_the_swing_finish(&mut app);
        }

        assert_eq!(drawn.len(), 6, "not every press played a swing: {drawn:?}");
        assert!(
            drawn.iter().all(|shape| *shape == SwingShape::Cut),
            "a blade drew something other than its one arc: {drawn:?}"
        );

        // The half that makes the paragraph above mean anything: nothing ever answered.
        assert!(
            app.world().get_resource::<Session>().is_none(),
            "a session turned up, so this test says nothing about a refused swing"
        );
    }

    #[test]
    fn a_bow_request_draws_the_string_rather_than_a_blade_arc() {
        const STEP: Duration = Duration::from_millis(16);

        let mut app = hand_only_app();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(STEP));
        app.world_mut().write_message(SwingSent {
            item_id: crafting::ITEM_BOW,
        });
        app.update();

        let animation = *app.world().resource::<HandAnimation>();
        assert_eq!(
            animation.attack.expect("the bow played nothing").shape,
            SwingShape::Draw
        );
        let pose = swing_pose(SwingShape::Draw, ATTACK_SWING_TIME / 2);
        assert!(
            pose.reach > 0.0,
            "the draw did not pull back toward the camera"
        );
    }

    #[test]
    fn a_sceptre_request_casts_forward_rather_than_drawing_a_blade_arc() {
        const STEP: Duration = Duration::from_millis(16);

        let mut app = hand_only_app();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(STEP));
        app.world_mut().write_message(SwingSent {
            item_id: crafting::ITEM_WOODEN_SCEPTRE,
        });
        app.update();

        let animation = *app.world().resource::<HandAnimation>();
        assert_eq!(
            animation.attack.expect("the sceptre played nothing").shape,
            SwingShape::Cast
        );
        let pose = swing_pose(SwingShape::Cast, ATTACK_SWING_TIME / 2);
        assert!(
            pose.reach < 0.0,
            "the cast did not thrust toward the target"
        );
        assert_eq!(pose.yaw, 0.0, "the cast became a blade arc");
    }

    /// **A consume that left plays the eating arc, and only a consume that left does.**
    ///
    /// The two halves of the acceptance criterion in one test, because they are one property:
    /// the hand plays on `ConsumeSent` and on nothing else, so a press that produced no
    /// request — an empty slot, a non-food, a screen that owns the input, a dropped frame —
    /// arrives here as no message and draws nothing. `super::inventory` is where that
    /// decision lives and where it is pinned; what this holds is that this module adds no
    /// second opinion of its own on either side.
    #[test]
    fn a_consume_that_left_plays_the_eating_arc_and_a_frame_with_none_plays_nothing() {
        const STEP: Duration = Duration::from_millis(16);

        let mut app = hand_only_app();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(STEP));

        // A frame with no message at all. The hand is holding food and is not eating.
        app.update();
        assert!(
            app.world().resource::<HandAnimation>().attack.is_none(),
            "the hand ate without a request having left"
        );

        app.world_mut().write_message(ConsumeSent);
        app.update();

        let animation = *app.world().resource::<HandAnimation>();
        assert_eq!(
            animation.attack.expect("the consume played nothing").shape,
            SwingShape::Eat
        );

        // Toward the eye on both channels, which is what tells this arc apart from every
        // other one here: the cut and the cast carry the model away from the camera.
        let pose = swing_pose(SwingShape::Eat, ATTACK_SWING_TIME / 2);
        assert!(
            pose.reach > 0.0,
            "the eating arc reached away from the mouth"
        );
        assert!(
            pose.pitch > 0.0,
            "the eating arc tipped the item over toward what a swing hits"
        );
        assert_eq!(
            (pose.yaw, pose.roll),
            (0.0, 0.0),
            "the eating arc became a blade stroke"
        );

        // And it is a one-shot like every other arc: it ends, and the hand is at rest.
        let_the_swing_finish(&mut app);
        assert!(
            app.world().resource::<HandAnimation>().attack.is_none(),
            "the eating arc never finished"
        );
    }

    /// **A swing and a consume in one frame draw the swing.**
    ///
    /// One composition draws one arc, so the two senders have to be ordered, and the order
    /// is written down in `animate_view_model` rather than left to whichever `MessageReader`
    /// happens to be read first. Nothing pairs the two presses today — one is the left
    /// button and one a bound key — but they share a gate and a frame, so a player can make
    /// both, and a blow being answered is the more urgent of the two to show.
    #[test]
    fn a_swing_and_a_consume_in_one_frame_draw_the_swing() {
        const STEP: Duration = Duration::from_millis(16);

        let mut app = hand_only_app();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(STEP));
        app.world_mut().write_message(ConsumeSent);
        app.world_mut().write_message(SwingSent {
            item_id: ITEM_RUSTY_SWORD,
        });
        app.update();

        assert_eq!(
            app.world()
                .resource::<HandAnimation>()
                .attack
                .expect("neither message played")
                .shape,
            SwingShape::Cut
        );
    }

    /// A second press inside a running arc restarts the swing.
    ///
    /// Two clicks are two swings, and the criterion is about consecutive attacks rather than
    /// about consecutive completed animations.
    ///
    /// **It used to assert the restart took the next shape as well, and that half went with
    /// the rotation — the restart itself is why the test stays.** A second press that was
    /// swallowed while an arc was in flight would be a real regression: the player clicked,
    /// the request left, and nothing on screen acknowledged it. Nothing else in this file
    /// would catch that, so the elapsed-time assertion below is now the whole of the test
    /// rather than the supporting half of it.
    #[test]
    fn a_swing_cut_short_by_the_next_press_restarts_the_arc() {
        const STEP: Duration = Duration::from_millis(16);

        let mut app = hand_only_app();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(STEP));

        app.world_mut().write_message(SwingSent {
            item_id: ITEM_RUSTY_SWORD,
        });
        app.update();
        let first = app
            .world()
            .resource::<HandAnimation>()
            .attack
            .expect("the first press played nothing");

        // Part way in, and deliberately not to the end.
        app.update();
        app.world_mut().write_message(SwingSent {
            item_id: ITEM_RUSTY_SWORD,
        });
        app.update();
        let second = app
            .world()
            .resource::<HandAnimation>()
            .attack
            .expect("the second press played nothing");

        assert_eq!(
            first.shape, second.shape,
            "a blade drew two different arcs, and it has only one"
        );
        assert_eq!(
            second.elapsed, STEP,
            "the second press continued the first arc instead of restarting it"
        );
    }
}
