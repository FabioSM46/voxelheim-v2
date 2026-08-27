//! The first-person held item: a camera child, never a world entity.
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
use bevy::ecs::system::SystemParam;
use bevy::light::NotShadowCaster;
use bevy::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;

use super::SelfVitals;
use super::camera::{ViewMode, WorldCamera};
use super::combat::{ITEM_RUSTY_SWORD, SwingSent};
use super::crafting::{ITEM_BOW, ITEM_WOODEN_SCEPTRE};
use super::inventory::{ApplyInventory, Inventory, SelectedSlot};
use super::items::{self, ItemShape};
use super::target::{ApplyMiningFeedback, ApplyTargetInput, BlockTarget, MiningFeedback};
use super::{HeldItemSurface, InputMode, held_item_surface, stack_item_id};
use super::{bundle_strap_linear_rgba, merge_all, rolled_bundle_parts};
use crate::net::{PLACEHOLDER_APPEARANCE, Session};
#[cfg(test)]
use crate::world::palette;

/// Close to the near plane and small enough to remain inside the camera's free
/// view-space pocket even when terrain touches the player capsule.
///
/// **The height is derived, not tuned.** With the fist [`HAND_SIZE`] now is, this is the
/// placement that puts the *complete* box inside a 16:9 frame at the default field of view
/// while keeping every corner of it in the lower-right quadrant, clear of the vertical
/// centre line and so of the crosshair — which the old `-0.075` did not: the fist's centre
/// sat past the bottom edge of the frustum and half the box was hard-clipped off screen
/// (#384). [`the_whole_fist_sits_in_the_lower_right_of_a_16_by_9_frame`] projects the real
/// vertices through the real rest pose rather than trusting this comment.
///
/// **The depth may not shrink.** Moving the hand toward the camera is the one way to
/// re-inflate it on screen without touching [`HAND_SIZE`], so the assertion below pins it.
const BASE_TRANSLATION: Vec3 = Vec3::new(0.10, -0.050, -0.18);

/// The fist may not be brought nearer the camera than #384 left it.
const _: () = assert!(
    BASE_TRANSLATION.z <= -0.18,
    "the fist was re-inflated by moving it toward the camera"
);

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
const HAND_SIZE: Vec3 = Vec3::splat(0.024);

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
/// sits at [`BASE_TRANSLATION`]`.z` and the camera's near plane is at `0.1`, which leaves
/// eight centimetres of headroom — and the composition rotates about its *own* origin, so
/// everything below that origin swings toward the eye during an overhead cut. At the tightest
/// reachable pose — [`OVERHEAD_PITCH_RADIANS`] on top of the rest pitch, with the placement
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

/// And how wide the forearm swells back to below it.
///
/// Under one, so the fist still overhangs the whole limb: what must never happen is an arm
/// broader than the hand on the end of it.
const FOREARM_WIDTH: f32 = 0.93;

/// The wrist is narrower than the fist and the forearm never outgrows it.
const _: () = assert!(
    WRIST_WIDTH < FOREARM_WIDTH && FOREARM_WIDTH < 1.0,
    "the limb below the fist is not a wrist swelling into a forearm"
);

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

/// How much darker a rust mark is than the iron it sits on.
///
/// **A multiplier, not a colour**, and that is what keeps `player/items.rs` the one answer
/// to which colour an item presents as. The blade's vertices carry white — identity
/// — everywhere but the marks, so the base that comes through is whatever that table says.
/// Change the sword's item colour and the rust follows it, because it is a shade *of* it.
///
/// Warm and dark: red kept, green and blue pulled down, which is what turns a pale iron into
/// oxide rather than into grey.
const RUST_TINT: [f32; 4] = [0.72, 0.38, 0.22, 1.0];
const BLOCK_EDGE: f32 = 0.055;
const MATERIAL_RADIUS: f32 = 0.020;
const MATERIAL_LENGTH: f32 = 0.050;

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

/// How long one attack swing plays for, whichever of the three shapes is playing.
///
/// A one-shot, unlike the mining loop above, which repeats while the server reports
/// progress: an attack is an event the server judges once, so its feedback happens once.
///
/// **One duration for all three shapes, and that is a decision rather than a convenience.**
/// A cut that took longer than a thrust would put the drawn shape into the *timing* of the
/// hand, and timing is the one presentation channel a cooldown also lives in. Three arcs
/// that differ in geometry alone cannot be read as three tempos, so nothing a player sees
/// here can be mistaken for the server changing its mind about how often a blade swings.
const ATTACK_SWING_TIME: Duration = Duration::from_millis(220);

/// The overhead cut: how far it carries the blade down and over.
///
/// Unchanged from when this was the only swing there was, so the arc a player already knows
/// is still one of the three and is still the first one drawn.
const OVERHEAD_PITCH_RADIANS: f32 = 0.9;

/// The lateral slash: how far it sweeps across the view, and how far the edge turns over
/// into that sweep.
///
/// Two terms because one of them is what makes it a slash rather than a pan — a blade held
/// upright and moved sideways reads as a wiper blade, and the roll is what puts an edge on
/// the front of the motion.
const LATERAL_YAW_RADIANS: f32 = 1.05;
const LATERAL_ROLL_RADIANS: f32 = 0.75;

/// The thrust: how far it drives along the view, and how far the tip levels out of the rest
/// pose's lean on the way.
///
/// **The reach is the shape and the level-out is a detail**, which is deliberately the
/// opposite balance to [`OVERHEAD_PITCH_RADIANS`] above. The two arcs share the pitch axis,
/// so if they shared its magnitude as well a thrust would read as a smaller chop; what tells
/// them apart is that one is almost all rotation and the other almost all travel.
///
/// Along -Z, the direction [`MINE_PUNCH_DISTANCE`] already established for *toward the thing
/// being hit*, and the opposite of [`PLACE_BUMP_DISTANCE`]'s draw-back.
const THRUST_REACH: f32 = 0.11;
const THRUST_LEVEL_RADIANS: f32 = 0.35;

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
/// of why the blade reads as bevelled: six side faces per span instead of four, so the light
/// catches a different pair as the hand turns.
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

/// The grip the hand closes on: leather, narrower than everything around it.
const GRIP_SIZE: Vec3 = Vec3::new(0.014, 0.024, 0.014);

/// The pommel: brass, wider than the grip, which is what stops the sword ending in a stub.
const POMMEL_SIZE: Vec3 = Vec3::new(0.018, 0.010, 0.017);

/// How far the blade's root is buried in the guard.
///
/// Half the guard, so the blade's own end cap sits *inside* the guard's volume rather than
/// flush with its top face. Flush would be two coplanar quads facing the same way, which is
/// the flicker rule 2 in `client/AGENTS.md` names for the body rig — and the reason a rust
/// mark stands proud of the blade rather than sitting on it.
const BLADE_TANG: f32 = GUARD_SIZE.y / 2.0;

/// How many rust marks the rusty blade carries.
///
/// **Several small ones rather than three large ones**, which is the difference between
/// oxide and damage: rust takes hold in freckles across a blade, and three patches at fixed
/// heights read as somebody having hit it with something.
const RUST_MARKS: u32 = 14;

/// The longest side of one mark, before [`scatter`] varies it down.
const RUST_MARK_SIZE: f32 = 0.010;

/// How much of each end of the blade stays clear of rust.
///
/// The whole mark, not its centre: a mark's own length is taken out of the range before it
/// is placed, so nothing overhangs the tip or disappears into the guard.
const RUST_MARK_MARGIN: f32 = 0.05;

/// How far a mark stands proud of the blade's surface, as a fraction of
/// [`BLADE_THICKNESS`].
///
/// The same twentieth #175 used, and for the same reason: two surfaces sharing a plane is
/// where a renderer has to choose, and it chooses per frame.
const RUST_MARK_PROUD: f32 = 0.05;

/// How deep a mark is bedded into the blade, as a fraction of the surface's own offset from
/// the mid-plane at that point.
///
/// **Both bounds are load-bearing and neither is a taste.** A mark is an axis-aligned box on
/// a surface that tilts away from it across the bevel, so the surface under one end of the
/// mark sits lower than under its middle; bedding it shallower than that drop would leave
/// the far end floating off the blade. Under one, so the mark can never reach through to the
/// other face and appear on both. The arithmetic that makes the first bound hold is in
/// [`rusted_blade_mesh`], and [`every_rust_mark_stays_on_the_blade_it_freckles`] measures it.
const RUST_MARK_SINK: f32 = 0.6;

/// The seed the marks are scattered from.
///
/// **Deterministic, so the same sword looks the same every run** — a blade whose freckles
/// moved between sessions would be the one thing about it a player could not learn.
const RUST_SEED: u32 = 0x5EED_0204;

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
        build.quad(face);
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
    .translated_by(Vec3::Y * (wrist_top + wrist_bottom) / 2.0)
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
    .translated_by(Vec3::NEG_Y * 0.5)
}

/// How far the forearm reaches below the view model's origin, for a composition the
/// animations have carried `along_view` from its resting depth.
///
/// **The arm keeps the length it is drawn at, not the length it is modelled at.** The whole
/// composition translates along the view — a thrust and a cast carry it [`THRUST_REACH`]
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
    // `along_view` is added to the model's `Z` and [`BASE_TRANSLATION`]`.z` is negative, so
    // reaching *away* from the eye is a negative offset over a negative base: the ratio is
    // the model's depth over its resting depth, and it is above one exactly when the
    // animation has pushed the composition out.
    ARM_REACH * (1.0 + along_view / BASE_TRANSLATION.z).max(1.0)
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
    /// Flat rather than smooth, deliberately: six faces per span that each catch the light
    /// separately is the whole reason the section is a hexagon, and averaging the normals at
    /// the ridge would put a soft gradient exactly where the highlight should break.
    fn quad(&mut self, corners: [Vec3; 4]) {
        let [a, b, c, d] = corners;
        // From the diagonals rather than from one triangle's two edges: a quad lofted
        // between sections of different widths is not exactly planar, and the diagonals
        // give the normal both of its triangles are nearest to instead of the first one's.
        let normal = (c - a).cross(d - b).normalize_or_zero();
        let first = self.push(corners.into_iter().zip(UNIT_UVS), normal);
        self.indices
            .extend([first, first + 1, first + 3, first + 1, first + 2, first + 3]);
    }

    /// One flat-shaded polygon, as a fan from its first corner.
    ///
    /// The corners must already be wound so that `normal` is the outward one; the caller
    /// reverses them for the end that faces the other way.
    fn fan(&mut self, corners: [Vec3; 6], normal: Vec3) {
        // The cap is never seen — the root is buried in the guard and the tip is a tenth of
        // a section — so its texture coordinates carry no information and say so.
        let first = self.push(corners.into_iter().zip([[0.0, 0.0]; 6]), normal);
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

/// One texture coordinate per corner of a quad, in the order [`MeshBuild::quad`] walks them.
///
/// Nothing samples them — this client has no texture and `client/AGENTS.md` says the palette
/// is the whole material system — but the attribute has to be *present*, because a merge
/// drops any attribute one side is missing and leaves the buffers unequal lengths.
const UNIT_UVS: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];

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
/// Read by [`rusted_blade_mesh`] so a mark sits on the surface the blade actually has there
/// rather than on the one it has at the guard.
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
fn blade_surface(section: BladeSection, z: f32) -> f32 {
    let ridge = section.half_width * BLADE_RIDGE_FRACTION;
    let across = z.abs();
    if across <= ridge {
        section.half_thickness
    } else {
        section.half_thickness * (section.half_width - across) / (section.half_width - ridge)
    }
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
pub(super) fn sword_mesh(length: f32) -> Mesh {
    let base = blade_base();
    let sections = blade_sections();

    let mut build = MeshBuild::default();
    for pair in sections.windows(2) {
        let [lower, upper] = pair else {
            unreachable!("windows(2) yields pairs")
        };
        let low = lower.perimeter();
        let high = upper.perimeter();
        for corner in 0..low.len() {
            let next = (corner + 1) % low.len();
            build.quad([low[corner], low[next], high[next], high[corner]]);
        }
    }
    // The two ends. The root's winding is reversed because its face looks the other way,
    // and a cap wound like the tip's would be culled from outside and visible from within.
    let mut root = sections[0].perimeter();
    root.reverse();
    build.fan(root, Vec3::NEG_Y);
    build.fan(sections[2].perimeter(), Vec3::Y);
    let mut sword = build.finish();

    // The furniture, in boxes, down from the base. Each sits directly under the last: two
    // solid boxes meeting on a plane present that plane's two quads back to back, and a
    // back-facing quad is culled — which is why *these* joins need no overlap and the
    // blade's root, whose cap would face the same way as the guard's, does.
    let guard = Mesh::from(Cuboid::from_size(GUARD_SIZE))
        .translated_by(Vec3::Y * (base - GUARD_SIZE.y / 2.0));
    let grip = Mesh::from(Cuboid::from_size(GRIP_SIZE))
        .translated_by(Vec3::Y * (base - GUARD_SIZE.y - GRIP_SIZE.y / 2.0));
    let pommel = Mesh::from(Cuboid::from_size(POMMEL_SIZE))
        .translated_by(Vec3::Y * (base - GUARD_SIZE.y - GRIP_SIZE.y - POMMEL_SIZE.y / 2.0));
    merge_all(&mut sword, [guard, grip, pommel], "sword");

    // Uniform, so the normals computed above stay unit vectors — `Mesh::scale_by` leaves
    // them alone for exactly that case and rebuilds them for every other.
    sword.scaled_by(Vec3::splat(length / SWORD_LENGTH))
}

/// A deterministic value in `0.0..1.0` for one rust mark and one of its dimensions.
///
/// **A seeded hash rather than a crate and rather than a table of hand-placed numbers.**
/// Fourteen scattered boxes are not worth a fourth dependency (`client/AGENTS.md` is
/// explicit about the budget), and an integer hash is reproducible on every platform, which
/// is what [`RUST_SEED`]'s promise of the same sword every run actually requires.
fn scatter(mark: u32, channel: u32) -> f32 {
    let mut bits = mark
        .wrapping_mul(0x9E37_79B1)
        .wrapping_add(channel.wrapping_mul(0x85EB_CA6B))
        ^ RUST_SEED;
    bits ^= bits >> 16;
    bits = bits.wrapping_mul(0x7FEB_352D);
    bits ^= bits >> 15;
    bits = bits.wrapping_mul(0x846C_A68B);
    bits ^= bits >> 16;
    // The top 24 bits over their own range: every value of that width is exactly
    // representable in an f32, so the division is the only rounding anywhere in here.
    (bits >> 8) as f32 / 16_777_216.0
}

/// The rusty sword: [`sword_mesh`] with oxide on the blade.
///
/// **Two colours on one mesh and one material**, which is what the cost note in
/// `client/AGENTS.md` asks for — the alternative was a second entity per held item, or a
/// material per item rather than one cached handle per resolved colour.
///
/// The vertices carry `Mesh::ATTRIBUTE_COLOR`, which `StandardMaterial` multiplies into its
/// `base_color`; `world/render.rs` has drawn the whole terrain that way since it existed, so
/// this is the established mechanism rather than a new one. White is identity — the iron
/// that comes through is whatever `player/items.rs` says the sword presents as — and the
/// marks carry [`RUST_TINT`], so they are a shade *of* that base rather than a second
/// opinion about it.
///
/// **Fourteen small marks scattered from a seed, where there used to be three large ones at
/// hand-picked heights.** Three patches a seventh of the blade tall read as damage; oxide is
/// freckles. Each is bedded into the surface it sits on rather than laid over it, which is
/// what lets a mark straddle the ridge and the bevel without either floating clear of the
/// blade or reaching through to the far face.
fn rusted_blade_mesh() -> Mesh {
    let mut merged = plain(sword_mesh(SWORD_LENGTH));
    let base = blade_base();
    let proud = BLADE_THICKNESS * RUST_MARK_PROUD;

    let marks = (0..RUST_MARKS).map(|mark| {
        // The longest side, half to all of `RUST_MARK_SIZE`. The whole mark is kept out of
        // the margin at each end rather than merely its centre, so nothing overhangs the
        // tip or disappears into the guard however large it came out.
        let length = RUST_MARK_SIZE * (0.5 + 0.5 * scatter(mark, 0));
        let lowest = base + BLADE_LENGTH * RUST_MARK_MARGIN + length / 2.0;
        let highest = base + BLADE_LENGTH * (1.0 - RUST_MARK_MARGIN) - length / 2.0;
        // **One mark per stratum of the blade, jittered inside its own** — rather than
        // fourteen independent draws over the whole length. Fourteen samples of a hash
        // clump: the first cut of this left the top third and the bottom tenth bare and put
        // nine marks in the middle, which reads as a band rather than as weathering.
        // Stratifying makes *spread over the blade* a property of the placement instead of a
        // hope about the seed, and the jitter is what keeps it from being a row.
        let stratum = (mark as f32 + scatter(mark, 1)) / RUST_MARKS as f32;
        let y = lowest + (highest - lowest) * stratum;

        // **Two bounds, and they are what keep a mark from overhanging the edge it sits
        // beside.** The mark spans at most a quarter of the local half-width to each side of
        // its centre, and its centre stays inside half of it — so the blade's surface can
        // fall away *across* the bevel under the mark by at most `0.38 × half_thickness`.
        //
        // They are not what makes the bedding below sufficient, which is what this comment
        // used to claim: the fall-off across the bevel is only one of the two directions the
        // surface drops in, and the bedding answers both. See `footing`.
        let section = blade_at(y);
        let width = (length * 0.5).min(section.half_width * 0.5);
        let room = (section.half_width * 0.5 - width / 2.0).max(0.0);
        let z = room * (scatter(mark, 2) * 2.0 - 1.0);

        // Alternating faces, so a blade turning in the hand shows freckles on whichever one
        // it presents rather than a stripe down one side of it.
        let face = if mark % 2 == 0 { 1.0 } else { -1.0 };
        let surface = blade_surface(section, z);
        // **Bedded from the shallowest surface under the whole mark, rather than from the one
        // under its centre.** The blade thins along its length as well as across the bevel,
        // and on the point it does so fast enough to outrun `RUST_MARK_SINK`: measured on the
        // fourteenth mark, bedded to 0.00122 from the section at its own centre while the
        // surface under its upper, outer corner is 0.00088 — so that corner floated 0.00034
        // clear of the blade it is meant to be sunk into, and a fleck of rust hung off the
        // point with daylight behind it.
        //
        // **Which corner answers is never in doubt**, which is what makes one sample enough:
        // the surface falls as `y` rises and as `|z|` grows, so the highest and farthest
        // corner is the shallowest of the four. On the flat this changes nothing — the
        // section at the mark's top and the section at its centre are the same numbers there,
        // and `RUST_MARK_SINK` still decides — so the deeper bedding is spent only where the
        // taper actually takes the surface away.
        let footing = blade_surface(blade_at(y + length / 2.0), z.abs() + width / 2.0)
            .min(surface * (1.0 - RUST_MARK_SINK));
        let sink = surface - footing;
        rusted(
            Mesh::from(Cuboid::from_size(Vec3::new(sink + proud, length, width)))
                .translated_by(Vec3::new(face * (surface + (proud - sink) / 2.0), y, z)),
        )
    });
    merge_all(&mut merged, marks, "rusted blade");
    merged
}

/// One mesh with every vertex at identity, so the material's own colour comes through.
///
/// The attribute has to be present on *both* sides of a merge: `Mesh::merge` refuses to join
/// a mesh carrying an attribute to one that does not, and the halves would silently disagree
/// about what white means if it did not.
fn plain(mesh: Mesh) -> Mesh {
    tinted(mesh, [1.0, 1.0, 1.0, 1.0])
}

/// One mesh with every vertex carrying [`RUST_TINT`].
fn rusted(mesh: Mesh) -> Mesh {
    tinted(mesh, RUST_TINT)
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
/// Most item meshes have no colour attribute and receive the resolved colour whole. The
/// rusty blade carries white and [`RUST_TINT`]; multiplying those by the item colour keeps
/// `player/items.rs` the one answer to what the steel is while retaining the oxide as a
/// shade of it.
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
/// it. The rusty sword remains the one item-level exception: rust belongs to that blade,
/// not to every item sharing its shape.
fn item_mesh(item_id: u16, shape: ItemShape) -> Mesh {
    if item_id == ITEM_RUSTY_SWORD {
        return rusted_blade_mesh();
    }
    match shape {
        ItemShape::Block => Mesh::from(Cuboid::from_size(Vec3::splat(BLOCK_EDGE))),
        ItemShape::Material => Mesh::from(Capsule3d::new(MATERIAL_RADIUS, MATERIAL_LENGTH)),
        ItemShape::Blade => sword_mesh(SWORD_LENGTH),
        ItemShape::Bundle => {
            let (mut roll, straps) = rolled_bundle_parts(BUNDLE_SIZE);
            merge_all(&mut roll, [straps], "held packed-gear bundle");
            roll
        }
        ItemShape::Tool => tool_mesh(),
        ItemShape::Armour => armour_mesh(),
        ItemShape::Shield => shield_mesh(0.065),
        ItemShape::Bow => bow_mesh(BOW_LENGTH),
        ItemShape::Sceptre => sceptre_mesh(SCEPTRE_LENGTH),
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
    let mut held = tinted(fist_mesh(), skin);
    merge_all(&mut held, [tinted(wrist_mesh(), skin)], "hand and wrist");
    let (Some(item_id), Some(shape), Some(item_colour)) =
        (appearance.item_id, appearance.shape, appearance.item_colour)
    else {
        return held;
    };

    let item = if shape == ItemShape::Bundle {
        coloured_bundle_mesh(item_colour)
    } else if matches!(shape, ItemShape::Shield | ItemShape::Sceptre) {
        item_mesh(item_id, shape)
    } else {
        coloured(item_mesh(item_id, shape), item_colour)
    }
    .translated_by(item_translation(shape));
    merge_all(&mut held, [item], "hand and held item");
    held
}

/// The forearm bar in the player's own skin.
///
/// One asset for both hands: the section, the colour and the unit length are identical, and
/// the only thing that differs between the right hand's arm and the off-hand shield's is the
/// transform each entity carries. A second asset would be a second answer to the same
/// authoritative skin colour.
fn skinned_forearm_mesh(skin_colour: u32) -> Mesh {
    tinted(forearm_mesh(), linear_rgb(skin_colour))
}

pub(super) struct HandsPlugin;

impl Plugin for HandsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<HandAnimation>()
            .init_resource::<SelfVitals>()
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
                    attach_to_camera,
                    ApplyDeferred,
                    refresh_held_item,
                    animate_view_model,
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

/// Which of the three arcs an attack draws.
///
/// **Presentation, and it is worth being exact about how far that goes.** The shape is
/// chosen in this module, from a counter in [`HandAnimation`] that [`swing_pose`] is the
/// only reader of; it reaches no request, no predicate and no other module. `super::combat`
/// routes the left button on the item id and sends the same `AttackRequest` whichever arc is
/// about to play, and the server judges the blow against its own registry — so which picture
/// played cannot change reach, damage, cooldown or what was asked for. It is the rule
/// `client/AGENTS.md` states for the item table, arriving by a different door: drawing an
/// item as a blade no more swings it than holding it as one does, and drawing a thrust
/// reaches no further than drawing a cut.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum SwingShape {
    /// Down and over: the arc this file had when it had one.
    #[default]
    Overhead,
    /// Across the view, with the edge turning over into the sweep.
    Lateral,
    /// Straight along the view, with the tip levelling as it goes.
    Thrust,
    /// The string hand drawing back. Chosen only for a bow request.
    Draw,
    /// A short forward presentation thrust, never a blade arc.
    Cast,
}

impl SwingShape {
    /// Every shape, for the sweeps that must cover the whole vocabulary.
    ///
    /// The same hand-written list, for the same reason, as `items::ItemShape::ALL`: no
    /// stable Rust enumerates variants. And as there, the list is not what makes a shape
    /// *drawn* — [`swing_pose`] and [`Self::after`] both match with no wildcard arm, so a
    /// fourth variant fails to build until it has been given an arc and a place in the
    /// rotation. What the list buys is the other half: a sweep that catches an arm filled
    /// in with a copy of its neighbour.
    ///
    /// `#[cfg(test)]` because nothing in the running client enumerates the shapes — the
    /// rotation walks them one at a time and never needs the set. That is where
    /// `ItemShape::ALL` also sat until a runtime reader turned up for it, and the day one
    /// turns up here the attribute comes off rather than the list changing.
    #[cfg(test)]
    const ALL: [Self; 5] = [
        Self::Overhead,
        Self::Lateral,
        Self::Thrust,
        Self::Draw,
        Self::Cast,
    ];

    /// The three blade arcs, excluding the bow's one-shot draw pose.
    #[cfg(test)]
    const BLADE_ARCS: [Self; 3] = [Self::Overhead, Self::Lateral, Self::Thrust];

    /// The shape that follows this one.
    ///
    /// **A fixed rotation rather than a random pick**, and the acceptance criterion is why:
    /// what a player must stop seeing is the same arc twice in a row, and random repeats.
    /// A cycle also makes *consecutive swings differ* a property one test can hold, rather
    /// than a distribution somebody has to sample.
    ///
    /// Exhaustive with no wildcard, so a fourth shape cannot be added without deciding
    /// where in the rotation it goes — the compiler's half of the guarantee, exactly as
    /// `items::ItemShape` arranges for the two renderers.
    fn after(self) -> Self {
        match self {
            Self::Overhead => Self::Lateral,
            Self::Lateral => Self::Thrust,
            Self::Thrust => Self::Overhead,
            Self::Draw => Self::Overhead,
            Self::Cast => Self::Overhead,
        }
    }
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
/// and now holds three times over.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct SwingPose {
    /// About the camera's X axis. **Negative carries the blade over toward what is being
    /// hit** — the convention [`mine_punch`]'s caller set and the one this file keeps, so a
    /// third and a fourth animation never have to argue about which way *out* is.
    pitch: f32,
    /// About Y: across the view. Positive turns the blade toward -X, which is the far side
    /// of the screen from the hand — [`BASE_TRANSLATION`] puts it on the right — so a slash
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
/// One envelope for all three — `sin(fraction * PI)`, out and back, zero at both ends — and
/// three sets of terms to apply it to. The shapes are told apart by *which* degree of freedom
/// each one is mostly made of: the cut is pitch, the slash is yaw, the thrust is reach. That
/// is what `each_shape_leads_with_a_channel_of_its_own` pins, and it is a stronger statement
/// than "the three poses differ", which three near-identical arcs would also satisfy.
fn swing_pose(shape: SwingShape, elapsed: Duration) -> SwingPose {
    let fraction = (elapsed.as_secs_f32() / ATTACK_SWING_TIME.as_secs_f32()).clamp(0.0, 1.0);
    let arc = (fraction * PI).sin();
    match shape {
        SwingShape::Overhead => SwingPose {
            pitch: -arc * OVERHEAD_PITCH_RADIANS,
            ..default()
        },
        SwingShape::Lateral => SwingPose {
            yaw: arc * LATERAL_YAW_RADIANS,
            roll: -arc * LATERAL_ROLL_RADIANS,
            ..default()
        },
        SwingShape::Thrust => SwingPose {
            pitch: -arc * THRUST_LEVEL_RADIANS,
            reach: -arc * THRUST_REACH,
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
            reach: -arc * THRUST_REACH,
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

    /// The attack swing playing right now, if one is. Started by a `SwingSent` message and
    /// by nothing else, so it plays exactly when a request left this client — whether that
    /// request later hits, misses or is refused.
    attack: Option<Swing>,

    /// Which shape the *next* swing will take.
    ///
    /// **The alternation is one field of local presentation state, and it is advanced by a
    /// request leaving rather than by any answer to one.** That is what makes it survive a
    /// swing the server refuses: a refusal is silence on this side — nothing comes back for
    /// a blow that is declined, the same silence a refused block edit produces — so there is
    /// no answer to wait for and none is waited for. Three clicks the server declines draw
    /// three different arcs, because all three requests left.
    ///
    /// It outlives the swing it belongs to on purpose. [`Self::attack`] is `None` between
    /// swings, so a cursor kept inside it would forget which arc had just played and the
    /// next press could repeat it.
    ///
    /// Nothing outside this module can read the field — [`HandAnimation`] is private — and
    /// nothing inside it consults the field for anything but which arc to draw.
    next_swing: SwingShape,
}

fn spawn_view_model(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
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
            Transform::from_translation(BASE_TRANSLATION),
            Visibility::Hidden,
            NotShadowCaster,
        ))
        .with_child(arm(forearm_mesh_handle.clone(), material.clone()));
    commands
        .spawn((
            OffHandShield { skin_colour },
            ViewModel,
            Mesh3d(shield_mesh_handle),
            MeshMaterial3d(material.clone()),
            Transform::from_translation(Vec3::new(-BASE_TRANSLATION.x, -0.035, -0.16))
                .with_rotation(Quat::from_rotation_z(-0.48)),
            Visibility::Hidden,
            NotShadowCaster,
        ))
        // The off-hand entity carries no animation of its own, so its arm never leaves the
        // resting length. It is still the same bar under the same transform, which is what
        // keeps the two hands one limb rather than two.
        .with_child(arm(forearm_mesh_handle, material));
    commands.insert_resource(visuals);
}

/// Attaches to the one camera after both startup systems have materialised.
fn attach_to_camera(
    mut commands: Commands,
    cameras: Query<Entity, With<WorldCamera>>,
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
    mode: Res<InputMode>,
    view: Res<ViewMode>,
    mut assets: HandAssets<'_>,
    mut held: HeldItemViewModelQuery<'_, '_>,
    mut shields: OffHandShieldViewModelQuery<'_, '_>,
    vitals: Res<SelfVitals>,
) {
    let (appearance, skin_colour) = subject.read();
    let view_mesh = assets.visuals.mesh.clone();
    // **The view term, and it was missing.** This model is a child of the camera, sitting
    // [`BASE_TRANSLATION`] in front of it — a first-person conceit and nothing else. #172
    // moved the camera four blocks back for the third-person view and gave every other such
    // conceit the term that removes it there: `InputGate::may_aim`, `InputGate::may_act`,
    // `ui::crosshair::show_crosshair` and `show_the_local_body`. This one was missed, so the
    // thing a player was holding floated between the camera and their own character (#194).
    //
    // Hidden rather than despawned, which is what the neighbouring test's name has always
    // said: a view toggle that removed the model would rebuild a mesh and a material on a
    // key press, and `animate_view_model` drives a transform on this same entity — so a
    // hidden model is a hidden animation, with nothing further to gate.
    let visible = if held_item_surface(*mode, *view, subject.session.is_some())
        == HeldItemSurface::ViewModel
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
    buttons: Option<Res<'w, ButtonInput<MouseButton>>>,
    target: Res<'w, BlockTarget>,
    feedback: Res<'w, MiningFeedback>,
    swings: MessageReader<'w, 's, SwingSent>,
}

impl HandIntent<'_, '_> {
    /// Whether gameplay input counts this frame. A mode transition belongs to the UI for
    /// the whole of it, which is how `target::send_block_edits` reads the same thing.
    fn playing(&self) -> bool {
        *self.mode == InputMode::Playing && !self.mode.is_changed()
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
        self.swings.read().next().map(|swing| swing.item_id)
    }
}

fn animate_view_model(
    time: Res<Time>,
    mut intent: HandIntent<'_, '_>,
    mut animation: ResMut<HandAnimation>,
    mut held: Query<(Entity, &HeldItem, &mut Transform), Without<Forearm>>,
    mut forearms: Query<(&ChildOf, &mut Transform), With<Forearm>>,
) {
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
    if let Some(item_id) = intent.swing_sent() {
        let shape = if item_id == ITEM_BOW {
            SwingShape::Draw
        } else if item_id == ITEM_WOODEN_SCEPTRE {
            SwingShape::Cast
        } else {
            next_animation.next_swing
        };
        next_animation.attack = Some(Swing {
            shape,
            elapsed: Duration::ZERO,
        });
        if item_id != ITEM_BOW && item_id != ITEM_WOODEN_SCEPTRE {
            next_animation.next_swing = next_animation.next_swing.after();
        }
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
    if *animation != next_animation {
        *animation = next_animation;
    }

    // The one transform the hand's own arm carries. It is read once and written into the
    // child below rather than being a second reading of the animation.
    let arm = forearm_transform(&next_animation);
    for (entity, item, mut transform) in &mut held {
        let next = presented_transform(&next_animation, item.shape);
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

fn presented_transform(animation: &HandAnimation, shape: Option<ItemShape>) -> Transform {
    let mut transform = animated_transform(animation);
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

fn animated_transform(animation: &HandAnimation) -> Transform {
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
        translation: BASE_TRANSLATION + Vec3::Z * along_view(animation),
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

    use super::super::crafting::ITEM_IRON_SWORD;
    use super::super::target::BlockHit;
    use super::*;
    use crate::net::{
        Appearance as PlayerLook, AppearanceInbox, InventoryStack, PlayerAppearance, SessionParams,
    };
    use crate::player::items::{ITEM_LOG, ITEM_RAW_COAL, ITEM_RAW_IRON, ITEM_STONE};
    use crate::player::{PlayerPlugin, combat, crafting, structures};

    /// Deliberately unlike every item swatch, so skin vertices can be identified in a
    /// composite without mistaking part of the item for the hand.
    const TEST_SKIN: u32 = 0x00E3_C4A0;

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

    /// **The rusty sword is iron with rust on it**, not one flat colour.
    ///
    /// Asserted as *two* vertex tints on one mesh, and as the marks being a shade of the
    /// base rather than a colour beside it: white is identity, so the iron that comes
    /// through is whatever `player/items.rs` says the sword presents as. That is what keeps
    /// that table the one answer — change the sword's colour and the rust follows it.
    #[test]
    fn the_rusty_sword_carries_iron_and_rust_on_one_mesh() {
        let rusted = rusted_blade_mesh();
        let plain = sword_mesh(SWORD_LENGTH);

        let marks = tints(&rusted);
        assert_eq!(
            marks.len(),
            2,
            "the rusty blade carries {} tints, want iron and rust: {marks:?}",
            marks.len()
        );
        assert!(
            marks.contains(&[255, 255, 255, 255]),
            "no vertex carries identity, so the item's own colour never shows through"
        );
        let rust = RUST_TINT.map(|channel| (channel * 255.0).round() as u8);
        assert!(marks.contains(&rust), "no vertex carries the rust tint");

        // And the iron sword is not rusty: it is the same `ItemShape::Blade` and must not
        // inherit one blade's condition. It carries no vertex colours at all — an absent
        // attribute is how a mesh takes its material's colour whole, which is what every
        // other held shape does and what the rusted blade opts out of.
        assert_eq!(
            tints(&plain),
            Vec::<[u8; 4]>::new(),
            "the plain blade carries vertex colours, so it is no longer simply its material"
        );
        assert!(
            rusted.count_vertices() > plain.count_vertices(),
            "the rusty sword has no mark geometry of its own"
        );
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

        let sword = positions(&sword_mesh(SWORD_LENGTH));

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
        let sword = positions(&sword_mesh(SWORD_LENGTH));

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

    /// **The rust is many small marks bedded into the blade.**
    ///
    /// It was three patches, each an eighth of the sword tall and more than half the blade
    /// wide, at hand-picked heights — which reads as damage rather than as oxide. What this
    /// pins is the shape of the replacement: [`RUST_MARKS`] of them, none longer than
    /// [`RUST_MARK_SIZE`], spread over the blade rather than banded across a third of it, and
    /// each *bedded into* the face it sits on rather than laid over it.
    ///
    /// That last clause is the one that needs measuring, because it is the only part not
    /// obvious from reading the constants. A mark is an axis-aligned box on a surface that
    /// tilts away across the bevel, so there are two ways to get it wrong and they fail in
    /// opposite directions: bedded too shallow and the far end lifts off the blade, bedded
    /// too deep and it comes through on the other face. Both are checked against the surface
    /// the blade actually has under each mark.
    #[test]
    fn every_rust_mark_is_bedded_into_the_blade_it_freckles() {
        // What three marks used to be, so "smaller" is measured against something rather than
        // asserted about nothing: 13% of the sword's length by 55% of the blade's width.
        const WAS_LONG: f32 = 0.115 * 0.13;
        const WAS_WIDE: f32 = 0.030 * 0.55;
        const {
            assert!(
                RUST_MARKS > 3 && RUST_MARK_SIZE < WAS_LONG && RUST_MARK_SIZE < WAS_WIDE,
                "the rust is not more numerous and smaller than the three patches it replaced"
            );
        }

        let mesh = rusted_blade_mesh();

        let Some(VertexAttributeValues::Float32x3(all)) = mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("the rusted blade must carry Float32x3 positions");
        };
        let Some(VertexAttributeValues::Float32x4(colours)) = mesh.attribute(Mesh::ATTRIBUTE_COLOR)
        else {
            panic!("the rusted blade must carry Float32x4 colours");
        };
        // Quantised for the reason `tints` quantises: this picks vertices out by identity
        // rather than measuring them.
        let rust = RUST_TINT.map(|channel| (channel * 255.0).round() as u8);
        let marked: Vec<[f32; 3]> = all
            .iter()
            .zip(colours)
            .filter(|(_, colour)| colour.map(|channel| (channel * 255.0).round() as u8) == rust)
            .map(|(position, _)| *position)
            .collect();

        // One mark is one box and `merge` appends, so the tinted vertices arrive in whole
        // marks, in the order they were built.
        let per_mark = Mesh::from(Cuboid::from_size(Vec3::ONE)).count_vertices();
        assert_eq!(
            marked.len(),
            RUST_MARKS as usize * per_mark,
            "the rust is {} vertices, which is not {RUST_MARKS} boxes of {per_mark}",
            marked.len()
        );

        let proud = BLADE_THICKNESS * RUST_MARK_PROUD;
        let base = blade_base();
        let mut faces = [false; 2];
        let mut centres: Vec<f32> = Vec::new();
        for (index, one) in marked.chunks(per_mark).enumerate() {
            let (low_x, high_x) = extent(one, 0);
            let (low_y, high_y) = extent(one, 1);
            let (low_z, high_z) = extent(one, 2);

            let longest = [high_x - low_x, high_y - low_y, high_z - low_z]
                .into_iter()
                .fold(0.0, f32::max);
            assert!(
                (RUST_MARK_SIZE * 0.5 - 1e-6..=RUST_MARK_SIZE + 1e-6).contains(&longest),
                "mark {index} is {longest} on its longest side, outside half to all of \
                 {RUST_MARK_SIZE}"
            );

            // Inside the blade lengthwise and off the last few per cent at each end: a mark
            // overhanging the tip blunts it, one inside the guard is invisible.
            assert!(
                low_y > base + BLADE_LENGTH * RUST_MARK_MARGIN - 1e-6
                    && high_y < base + BLADE_LENGTH * (1.0 - RUST_MARK_MARGIN) + 1e-6,
                "mark {index} spans y {low_y}..{high_y}, outside the blade's rustable length"
            );

            // On one face rather than wrapped across both: that is what alternating faces
            // means, and a mark straddling the mid-plane would satisfy every other clause.
            assert!(
                low_x * high_x > 0.0,
                "mark {index} spans x {low_x}..{high_x}, so it wraps the blade rather than \
                 sitting on one face of it"
            );
            faces[usize::from(high_x > 0.0)] = true;

            let section = blade_at((low_y + high_y) / 2.0);
            let centre = (low_z + high_z) / 2.0;
            let surface = blade_surface(section, centre);
            let outer = low_x.abs().max(high_x.abs());
            let inner = low_x.abs().min(high_x.abs());
            assert!(
                (outer - (surface + proud)).abs() < 1e-6,
                "mark {index} reaches {outer} from the mid-plane where the blade's surface is \
                 at {surface}, so it is not bedded into the face it sits on"
            );

            // **The shallowest corner, not the middle.** The surface falls in two directions
            // under a mark — across the bevel as `|z|` grows, and along the blade as it
            // tapers toward the point — so the corner that decides whether the mark is bedded
            // is the highest and the farthest, and the section under *that* is the one to ask.
            // Measuring the middle instead is what let the fourteenth mark float 0.00034 clear
            // of the point while this test passed: at its own centre the blade is 0.00242 deep
            // and it was bedded to 0.00122, which looks bedded until you look 0.0038 higher up,
            // where the blade has thinned to 0.00088. Both sections are checked; they are the
            // same number for every mark on the flat, and differ only where the taper is real.
            let far = centre.abs() + (high_z - low_z) / 2.0;
            let top = blade_at(high_y);
            for (where_, at) in [("its centre", section), ("its upper edge", top)] {
                assert!(
                    far < at.half_width,
                    "mark {index} reaches {far} across a blade half {} wide at {where_}, so it \
                     overhangs an edge",
                    at.half_width
                );
                assert!(
                    inner <= blade_surface(at, far) + 1e-9,
                    "mark {index} is bedded to {inner} where the blade's surface under its far \
                     edge at {where_} is {}, so it floats clear of the blade",
                    blade_surface(at, far)
                );
            }
            assert!(
                inner > 0.0,
                "mark {index} reaches through the mid-plane, so it shows on the far face too"
            );

            centres.push((low_y + high_y) / 2.0);
        }

        assert_eq!(faces, [true; 2], "every mark is on one face of the blade");

        let (lowest, highest) = (
            centres.iter().copied().fold(f32::INFINITY, f32::min),
            centres.iter().copied().fold(f32::NEG_INFINITY, f32::max),
        );
        assert!(
            highest - lowest > BLADE_LENGTH * 0.6,
            "the marks span {} of a {BLADE_LENGTH} blade, so they are a band rather than \
             weathering",
            highest - lowest
        );
        let mut heights: Vec<i32> = centres.iter().map(|y| (y * 1e6) as i32).collect();
        heights.sort_unstable();
        heights.dedup();
        assert_eq!(
            heights.len(),
            RUST_MARKS as usize,
            "two marks share a height, so the scatter is not scattering"
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
        let mesh = sword_mesh(SWORD_LENGTH);

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
            read(&rusted_blade_mesh()),
            read(&rusted_blade_mesh()),
            "two builds of one sword put the rust in different places"
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
                Some(ItemShape::Blade) => SwingShape::BLADE_ARCS.map(Some).to_vec(),
                Some(ItemShape::Bow) => vec![Some(SwingShape::Draw)],
                Some(ItemShape::Sceptre) => vec![Some(SwingShape::Cast)],
                _ => Vec::new(),
            };
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
                        let transform = presented_transform(&animation, appearance.shape);
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
    /// test rather than the table: holding the iron sword must not produce the rusted mesh.
    #[test]
    fn only_the_rusty_sword_is_drawn_rusted() {
        for (item_id, want_rusted) in [(ITEM_RUSTY_SWORD, true), (ITEM_IRON_SWORD, false)] {
            let appearance = selected_appearance(Some(InventoryStack {
                item_id,
                count: 1,
                ..Default::default()
            }));
            let mesh = held_mesh(TEST_SKIN, appearance);
            let item_colour = appearance.item_colour.expect("an item has a colour");
            let rust = std::array::from_fn(|channel| item_colour[channel] * RUST_TINT[channel]);
            let rust = rust.map(|channel| (channel * 255.0).round() as u8);
            assert_eq!(
                tints(&mesh).contains(&rust),
                want_rusted,
                "item {item_id} carries a rust tint = {}, want {want_rusted}",
                tints(&mesh).contains(&rust)
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
    #[test]
    fn the_wrist_steps_in_from_the_fist_in_the_projected_outline() {
        let rest = presented_transform(&HandAnimation::default(), None);
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
        // The forearm at rest swells back out below it, so the step is the wrist's alone.
        let arm = positions(&placed_forearm(&HandAnimation::default()));
        let (arm_low, arm_high) = extent(&arm, 0);
        let (limb_low, limb_high) = extent(&wrist, 0);
        assert!(
            arm_high - arm_low > limb_high - limb_low,
            "the forearm spans {} across against a wrist of {}, so the limb below the fist \
             does not swell back out",
            arm_high - arm_low,
            limb_high - limb_low
        );

        let (fist_left, fist_right) = span(&positions(&fist_mesh()));
        let (wrist_left, wrist_right) = span(&wrist);
        assert!(
            wrist_left > fist_left && wrist_right < fist_right,
            "the wrist projects to {wrist_left}..{wrist_right} and the fist to \
             {fist_left}..{fist_right}, so the outline has no step in it on both sides"
        );

        // And the step is worth seeing rather than a rounding error. At the default field of
        // view on 1080 lines one pixel spans `2·tan(fov/2)/1080` of this projection; the
        // narrower edge of the step has to be several of them, or the silhouette is one
        // rectangle whatever the constant says.
        let field_of_view = crate::settings::Settings::default().field_of_view();
        let pixel = 2.0 * (field_of_view.to_radians() / 2.0).tan() / 1080.0;
        let step = (wrist_left - fist_left).min(fist_right - wrist_right);
        assert!(
            step > 4.0 * pixel,
            "the wrist steps in by {step}, under four pixels at {field_of_view}° on 1080 lines"
        );
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
        let rest = presented_transform(&HandAnimation::default(), Some(ItemShape::Blade));
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
        let viewport_height =
            2.0 * BASE_TRANSLATION.z.abs() * (field_of_view.to_radians() / 2.0).tan();
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
        let rest = presented_transform(&HandAnimation::default(), None);

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
                    ..Default::default()
                };
                let pose = presented_transform(&animation, held);
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

        // The tightest pose the view model reaches: a full overhead cut with the placement
        // bump at its peak. Both terms carry the model toward the eye.
        let pitch = REST_PITCH_RADIANS - OVERHEAD_PITCH_RADIANS;
        let roll = REST_ROLL_RADIANS - 0.18;
        // **[`BLADE_NEAR_PLANE_CLEARANCE`] is part of the bound**, and it is not an
        // optimism. An overhead cut is only ever drawn with a sword in hand — `combat.rs`
        // routes the left button on the item id and the three arcs belong to the blades —
        // so the pose that reaches nearest the camera is always the one this offset has
        // already pushed back. It is the same pairing
        // [`every_held_arrangement_clears_the_near_plane_through_every_swing`] sweeps, which
        // is the test that would catch it if the routing ever widened.
        let depth = BASE_TRANSLATION.z + PLACE_BUMP_DISTANCE - BLADE_NEAR_PLANE_CLEARANCE;
        // How much of a point's own `-Y` that pose turns into camera-space `+Z`.
        let toward_camera = -pitch.sin() * roll.cos();
        // The limb's own half-section spends part of the headroom before its length does.
        let section = pitch.sin() * roll.sin() * (HAND_SIZE.x * FOREARM_WIDTH / 2.0)
            + pitch.cos() * (HAND_SIZE.z / 2.0);
        let permitted = (-near - depth - section) / toward_camera;

        assert!(
            ARM_REACH <= permitted,
            "the forearm reaches {ARM_REACH} below the origin and the near plane at {near} \
             permits {permitted}"
        );
        // And it is not left needlessly short: an arm well inside the bound would be a
        // shorter arm than the frame can be filled with, for no reason anybody wrote down.
        assert!(
            ARM_REACH > permitted * 0.9,
            "the forearm reaches {ARM_REACH} where {permitted} was available, so it is short \
             of the frame for no stated reason"
        );

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
    #[test]
    fn the_forearm_joins_the_fist_to_the_bottom_edge_with_no_gap() {
        const ASPECT: f32 = 16.0 / 9.0;
        const COLUMNS: usize = 33;
        const STEP: f32 = 0.0005;

        let field_of_view = crate::settings::Settings::default().field_of_view();
        let half_height = (field_of_view.to_radians() / 2.0).tan();

        let walk = |name: &str, animation: &HandAnimation, held: Option<ItemShape>| {
            let pose = presented_transform(animation, held);
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
            // The wrist's own band, inset off its two silhouette edges: a convex outline is a
            // single point at its extreme abscissa, which no sampled column can be inside.
            let half_width = HAND_SIZE.x * WRIST_WIDTH / 2.0;
            let wrist_top = -HAND_SIZE.y / 2.0 + ARM_OVERLAP;
            let mut edges: Vec<f32> = [-half_width, half_width]
                .into_iter()
                .flat_map(|x| {
                    [-HAND_SIZE.z / 2.0, HAND_SIZE.z / 2.0].map(move |z| {
                        [wrist_top - WRIST_LENGTH, wrist_top].map(|y| project(Vec3::new(x, y, z)).x)
                    })
                })
                .flatten()
                .collect();
            edges.sort_by(f32::total_cmp);
            let (left, right) = (edges[0], edges[edges.len() - 1]);
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
        // **And at the peak of a thrust**, which is the frame #394 was filed about and the one
        // where the limb is longest. A join that opens when the arm stretches would be the new
        // way to reintroduce exactly the defect #389 closed, and it is invisible to the
        // end-cap measurements below: those ask where the arm *ends*, not whether it is
        // continuous on the way there. The hand is walked empty in both passes and the blade's
        // pose is used for the second, which is the strict pairing: a held item can only add
        // cover, and the offset it brings is the one a thrust is really drawn with.
        walk(
            "at the peak of a thrust",
            &HandAnimation {
                attack: Some(Swing {
                    shape: SwingShape::Thrust,
                    elapsed: ATTACK_SWING_TIME / 2,
                }),
                ..Default::default()
            },
            Some(ItemShape::Blade),
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
                "through an overhead cut",
                widest(
                    Some(SwingShape::Overhead),
                    Some(ItemShape::Blade),
                    true,
                    false,
                ),
                54.0,
            ),
            (
                "through a lateral slash",
                widest(
                    Some(SwingShape::Lateral),
                    Some(ItemShape::Blade),
                    true,
                    false,
                ),
                53.0,
            ),
            (
                "through a bow draw",
                widest(Some(SwingShape::Draw), Some(ItemShape::Bow), true, false),
                60.0,
            ),
            (
                "through a thrust",
                widest(
                    Some(SwingShape::Thrust),
                    Some(ItemShape::Blade),
                    true,
                    false,
                ),
                53.0,
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
        // **This assertion was inverted by #394, not deleted.** It read `< default` and it
        // passed, which is what a test looks like when it has been written around a defect
        // instead of against one: the property it pinned was *the arm's end is visible during a
        // thrust*. Turning it over leaves the same statement pinned from the side that is worth
        // pinning, and a change that reintroduces the defect fails here by name.
        assert!(
            widest(
                Some(SwingShape::Thrust),
                Some(ItemShape::Blade),
                true,
                false
            ) > default,
            "a thrust shows the arm's end at the default field of view again, which is the \
             defect #394 was filed about"
        );
        // **And a cast is still clipped at the narrowest, which is what stops the thrust's
        // new floor from reading as a licence.** The two arcs carry the model the same
        // distance away and answer within a degree and a half of each other; if a later
        // change walks this one under the narrowest field of view too, the reach-away pair
        // has moved rather than one number having been re-recorded.
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
    /// all land. The invariance is not exact — [`BASE_TRANSLATION`]`.y` puts the whole
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
            let pose = presented_transform(animation, held);
            let origin = pose.transform_point(Vec3::ZERO);
            let cap =
                pose.transform_point(Vec3::new(0.0, -drawn_arm_reach(along_view(animation)), 0.0));
            origin.y / -origin.z - cap.y / -cap.z
        };

        let rest = projected_reach(&HandAnimation::default(), None);
        assert!(rest > 0.0, "the arm does not reach below the hand at rest");

        for (name, shape, held) in [
            ("a thrust", SwingShape::Thrust, Some(ItemShape::Blade)),
            ("a sceptre cast", SwingShape::Cast, Some(ItemShape::Sceptre)),
        ] {
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
        // constant length loses a quarter of its reach at the peak of a thrust.
        let peak = HandAnimation {
            attack: Some(Swing {
                shape: SwingShape::Thrust,
                elapsed: ATTACK_SWING_TIME / 2,
            }),
            ..Default::default()
        };
        let pose = presented_transform(&peak, Some(ItemShape::Blade));
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
            let grip = part_corners(GRIP_SIZE.z / 2.0, grip_low, grip_high);
            let pommel = part_corners(POMMEL_SIZE.z / 2.0, pommel_low, pommel_high);
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
            let presentation =
                presented_transform(&HandAnimation::default(), Some(ItemShape::Blade));
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
        let grip = part(GRIP_SIZE.z / 2.0, grip_low, guard_low);
        let pommel = part(POMMEL_SIZE.z / 2.0, pommel_low, grip_low);

        for (name, part, size) in [("grip", &grip, GRIP_SIZE), ("pommel", &pommel, POMMEL_SIZE)] {
            assert!(!part.is_empty(), "no {name} corners were selected");
            for axis in 0..3 {
                let (low, high) = extent(part, axis);
                assert!(
                    (high - low - size[axis]).abs() < EPSILON,
                    "the {name} selection spans {} on axis {axis} and the part is {}",
                    high - low,
                    size[axis]
                );
            }
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

        let mut arcs: Vec<Option<SwingShape>> = SwingShape::BLADE_ARCS.map(Some).to_vec();
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
                            ..Default::default()
                        };
                        let pose = presented_transform(&animation, Some(ItemShape::Blade));
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
        // lands within a couple of micrometres of it. The worst the sweep produces is
        // `1.98e-6`.
        //
        // Ten micrometres is the recorded ceiling. At the default 45° vertical field of view
        // one pixel of a 1080-line viewport spans about 0.14 mm at the hand's depth, so the
        // ceiling is a fourteenth of a pixel — it cannot absorb a protrusion anybody could
        // see, and the `const` palm containment beside [`GRIP_SIZE`] is why there is none to
        // absorb: a point inside a convex solid is behind that solid's surface from every
        // viewpoint outside it.
        const GRAZE: f32 = 1e-5;
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

        let rest = presented_transform(&HandAnimation::default(), Some(ItemShape::Blade));
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
        let colours = tints(meshes.get(&mesh.0).expect("the held mesh"));
        let skin = linear_rgb(TEST_SKIN).map(|channel| (channel * 255.0).round() as u8);
        let stone =
            items::item_linear_rgba(ITEM_STONE).map(|channel| (channel * 255.0).round() as u8);
        assert!(colours.contains(&skin), "the mesh has no local skin colour");
        assert!(
            colours.contains(&stone),
            "the mesh has no item-table colour"
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
    fn the_view_model_is_parented_to_the_only_world_camera() {
        let mut app = app();
        let parent = held(&mut app).2;
        assert!(
            app.world().entity(parent).contains::<WorldCamera>(),
            "the held item was left in world space"
        );
        let Projection::Perspective(projection) = app
            .world()
            .get::<Projection>(parent)
            .expect("the world camera has a projection")
        else {
            panic!("the world camera is perspective");
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
            -BASE_TRANSLATION.z - largest_depth / 2.0 > projection.near,
            "the held mesh crosses the camera near plane"
        );
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
        let resting = animated_transform(&HandAnimation::default());
        let swinging = animated_transform(&HandAnimation {
            mine_elapsed: Duration::from_millis(50),
            bump_elapsed: None,
            ..Default::default()
        });
        let bumping = animated_transform(&HandAnimation {
            mine_elapsed: Duration::ZERO,
            bump_elapsed: Some(PLACE_BUMP_TIME / 2),
            ..Default::default()
        });

        assert_ne!(swinging.rotation, resting.rotation, "mining did not swing");
        assert_eq!(
            animated_transform(&HandAnimation {
                mine_elapsed: Duration::ZERO,
                bump_elapsed: None,
                ..Default::default()
            }),
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

    /// The three items that plant an entity rather than a voxel. The hand is where a
    /// player sees which of them the place press is about to ask for, so a bundle is its
    /// own shape rather than another cube.
    #[test]
    fn a_tent_a_forge_and_a_campfire_are_held_as_bundles() {
        let bundles = [
            structures::ITEM_TENT,
            structures::ITEM_FORGE,
            structures::ITEM_CAMPFIRE,
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

        // Three bundles, three colours: canvas, iron and firewood are what a player is
        // carrying, and two that looked alike would be slots they had to count to tell
        // apart.
        for (first, second) in [(0, 1), (0, 2), (1, 2)] {
            assert_ne!(
                carried[first].item_colour, carried[second].item_colour,
                "items {} and {} are carried in the same colour",
                bundles[first], bundles[second]
            );
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
        let colours = tints(&held_mesh(TEST_SKIN, appearance));
        let canvas = appearance
            .item_colour
            .expect("the tent has a canvas colour")
            .map(|channel| (channel * 255.0).round() as u8);
        let straps = bundle_strap_linear_rgba().map(|channel| (channel * 255.0).round() as u8);
        assert!(colours.contains(&canvas), "the roll lost the tent colour");
        assert!(colours.contains(&straps), "the two straps are not brown");
        assert_ne!(canvas, straps, "the straps disappeared into the canvas");
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
        animated_transform(&HandAnimation {
            attack: Some(Swing {
                shape,
                elapsed: ATTACK_SWING_TIME.mul_f32(fraction),
            }),
            ..Default::default()
        })
    }

    /// One swing per message, on the frame the request left — and every shape settles.
    ///
    /// Swept over [`SwingShape::ALL`] rather than over the one arc this used to be: three
    /// shapes are three chances to leave the hand leaning, and the whole reason the pose is
    /// four loose terms added to rest is that each of them returns to zero.
    #[test]
    fn a_sent_swing_moves_the_view_model_and_then_settles() {
        let resting = animated_transform(&HandAnimation::default());

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

    /// **Three shapes, and each leads with a degree of freedom the other two do not.**
    ///
    /// The acceptance criterion asks for an overhead cut, a lateral slash and a thrust —
    /// three *different* things, not one arc scaled three ways. So what is asserted is not
    /// merely that the poses differ, which three near-identical arcs would also satisfy,
    /// but that each shape moves its own named channel furthest: the cut is pitch, the slash
    /// is yaw, the thrust is reach. A fourth shape that copied one of them would land on a
    /// channel already spoken for and this would fail.
    #[test]
    fn each_shape_leads_with_a_channel_of_its_own() {
        let peak: Vec<(SwingShape, SwingPose)> = SwingShape::BLADE_ARCS
            .into_iter()
            .map(|shape| (shape, swing_pose(shape, ATTACK_SWING_TIME / 2)))
            .collect();

        for (shape, name, channel, of) in [
            (
                SwingShape::Overhead,
                "the cut",
                "pitch",
                (|pose: &SwingPose| pose.pitch.abs()) as fn(&SwingPose) -> f32,
            ),
            (SwingShape::Lateral, "the slash", "yaw", |pose| {
                pose.yaw.abs()
            }),
            (SwingShape::Thrust, "the thrust", "reach", |pose| {
                pose.reach.abs()
            }),
        ] {
            let mine = peak
                .iter()
                .find(|(candidate, _)| *candidate == shape)
                .map(|(_, pose)| of(pose))
                .expect("every shape has a peak pose");
            assert!(mine > 0.0, "{name} does not move in {channel} at all");
            for (other, other_pose) in &peak {
                if *other == shape {
                    continue;
                }
                assert!(
                    of(other_pose) < mine,
                    "{name} was supposed to own {channel}, and {other:?} moves it as far"
                );
            }
        }

        // And no two poses are the same pose, which the channel argument implies but which
        // a reader should not have to derive.
        for (index, (shape, pose)) in peak.iter().enumerate() {
            for (other, other_pose) in &peak[index + 1..] {
                assert_ne!(pose, other_pose, "{shape:?} and {other:?} draw one arc");
            }
        }
    }

    /// The rotation visits all three and never repeats one back to back.
    ///
    /// Held over twice the length of the cycle, because a rotation that alternated between
    /// two shapes and dropped the third would satisfy "no two in a row" perfectly.
    #[test]
    fn the_rotation_never_draws_one_shape_twice_running() {
        let mut shape = SwingShape::default();
        let mut drawn = vec![shape];
        for _ in 0..(SwingShape::BLADE_ARCS.len() * 2) {
            shape = shape.after();
            assert_ne!(
                shape,
                *drawn.last().expect("the first shape is already in"),
                "the rotation repeated a shape: {drawn:?}"
            );
            drawn.push(shape);
        }
        for shape in SwingShape::BLADE_ARCS {
            assert!(
                drawn.contains(&shape),
                "{shape:?} is in the vocabulary and never drawn: {drawn:?}"
            );
        }
    }

    /// **A punch, not a wobble.** The hand reaches for the block, comes back, and the
    /// cycle closes on rest so the loop repeats from the same place however long it runs.
    #[test]
    fn the_mining_punch_reaches_for_the_block_and_comes_back() {
        let cycle = Duration::from_secs_f32(1.0 / MINE_PUNCHES_PER_SECOND);
        let resting = animated_transform(&HandAnimation::default());
        let extended = animated_transform(&HandAnimation {
            mine_elapsed: cycle / 2,
            ..Default::default()
        });

        // Away from the camera is -Z, so the fist reaches for what it is breaking.
        assert!(
            extended.translation.z < resting.translation.z,
            "the punch never carried the hand toward the block: {} against {} at rest",
            extended.translation.z,
            resting.translation.z
        );

        // And the other way from a placement, which draws back from the block it just set
        // down. Two animations sharing an axis have to be told apart at a glance.
        let bumping = animated_transform(&HandAnimation {
            bump_elapsed: Some(PLACE_BUMP_TIME / 2),
            ..Default::default()
        });
        assert!(
            bumping.translation.z > resting.translation.z,
            "the placement bump now travels the same way as the mining punch"
        );

        // Nothing is left extended or leaning at the end of one punch. Compared with a
        // tolerance for the reason the attack arc above is: `cos(TAU)` is an ulp from one.
        let closed = animated_transform(&HandAnimation {
            mine_elapsed: cycle,
            ..Default::default()
        });
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
            let at = animated_transform(&HandAnimation {
                mine_elapsed: cycle.mul_f32(f32::from(step) / 64.0),
                ..Default::default()
            });
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
            // `BlockTargetPlugin`, the swing message from `CombatPlugin`, the mouse from
            // Bevy's input plugin, and the pack from `InventoryPlugin`.
            .init_resource::<BlockTarget>()
            .init_resource::<ButtonInput<MouseButton>>()
            .add_message::<SwingSent>()
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

    /// **The alternation is driven by the request leaving, and by nothing coming back.**
    ///
    /// There is no session here, no snapshot, no inbound frame of any kind — which is
    /// exactly the state a player is in when the server refuses a swing, because a refused
    /// blow produces no reply at all. Six presses still draw six arcs and the rotation still
    /// visits all three, because what advanced it was the asking.
    ///
    /// The two halves are asserted separately on purpose. *No two in a row* is the
    /// criterion; *all three appear* is what stops a rotation that quietly dropped one from
    /// satisfying it.
    #[test]
    fn every_swing_takes_the_next_shape_with_no_answer_from_any_server() {
        const STEP: Duration = Duration::from_millis(16);

        let mut app = hand_only_app();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(STEP));

        let mut drawn = Vec::new();
        for press in 0..(SwingShape::BLADE_ARCS.len() * 2) {
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

        for pair in drawn.windows(2) {
            assert_ne!(
                pair[0], pair[1],
                "two swings running drew one arc: {drawn:?}"
            );
        }
        for shape in SwingShape::BLADE_ARCS {
            assert!(drawn.contains(&shape), "{shape:?} never played: {drawn:?}");
        }

        // The half that makes the paragraph above mean anything: nothing ever answered.
        assert!(
            app.world().get_resource::<Session>().is_none(),
            "a session turned up, so this test says nothing about a refused swing"
        );
    }

    #[test]
    fn a_bow_request_draws_the_string_without_advancing_the_blade_rotation() {
        const STEP: Duration = Duration::from_millis(16);

        let mut app = hand_only_app();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(STEP));
        let before = app.world().resource::<HandAnimation>().next_swing;
        app.world_mut().write_message(SwingSent {
            item_id: crafting::ITEM_BOW,
        });
        app.update();

        let animation = *app.world().resource::<HandAnimation>();
        assert_eq!(
            animation.attack.expect("the bow played nothing").shape,
            SwingShape::Draw
        );
        assert_eq!(animation.next_swing, before);
        let pose = swing_pose(SwingShape::Draw, ATTACK_SWING_TIME / 2);
        assert!(
            pose.reach > 0.0,
            "the draw did not pull back toward the camera"
        );
    }

    #[test]
    fn a_sceptre_request_casts_forward_without_advancing_the_blade_rotation() {
        const STEP: Duration = Duration::from_millis(16);

        let mut app = hand_only_app();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(STEP));
        let before = app.world().resource::<HandAnimation>().next_swing;
        app.world_mut().write_message(SwingSent {
            item_id: crafting::ITEM_WOODEN_SCEPTRE,
        });
        app.update();

        let animation = *app.world().resource::<HandAnimation>();
        assert_eq!(
            animation.attack.expect("the sceptre played nothing").shape,
            SwingShape::Cast
        );
        assert_eq!(animation.next_swing, before);
        let pose = swing_pose(SwingShape::Cast, ATTACK_SWING_TIME / 2);
        assert!(
            pose.reach < 0.0,
            "the cast did not thrust toward the target"
        );
        assert_eq!(pose.yaw, 0.0, "the cast became a blade arc");
    }

    /// A second press inside a running arc restarts the swing *and* takes the next shape.
    ///
    /// Two clicks are two swings, and the criterion is about consecutive attacks rather
    /// than about consecutive completed animations — a restart that redrew the same arc
    /// would be the repetition this issue exists to remove, arriving through the one door
    /// the rotation could have been left open at.
    #[test]
    fn a_swing_cut_short_by_the_next_press_still_changes_shape() {
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

        assert_ne!(
            first.shape, second.shape,
            "the interrupted swing was redrawn as the same shape"
        );
        assert_eq!(
            second.elapsed, STEP,
            "the second press continued the first arc instead of restarting it"
        );
    }
}
