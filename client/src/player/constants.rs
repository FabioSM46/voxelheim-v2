//! The numbers movement needs on this side.
//!
//! # These mirror the server, and must stay in sync with it
//!
//! [`PLAYER_WIDTH`] and [`PLAYER_HEIGHT`] are copies of `game.PlayerWidth` and
//! `game.PlayerHeight` in `server/internal/game/constants.go`. **Change one and change
//! the other**: the server collides a box of that size and this module draws a body
//! inside it, so a mismatch is a body that visibly does not fit the space the server says
//! it fits.
//!
//! They are also the **grid a character is cut on**: [`super::appearance`] divides them
//! into twelfths across and thirty-sixths up rather than holding a size of its own, so a
//! change here moves every part of every character with it.
//!
//! # What deliberately is *not* mirrored here
//!
//! The server's `WalkSpeed`, `Gravity` and `JumpImpulse`. There is no client-side
//! prediction in this issue, so nothing here integrates anything — and a duplicated
//! constant with no reader is a synchronisation hazard that buys nothing. Prediction is
//! the issue that will need them, and it is the issue that should copy them across.
//!
//! Nothing in this file is on the wire. The client learns the tick rate from
//! `ServerWelcome` and every position from `EntitySnapshot`; how fast a player walks is
//! not something it is told, because it is not something it decides.

use std::f32::consts::FRAC_PI_2;

/// The edge of the player's square footprint, in blocks. Mirrors `game.PlayerWidth`.
pub const PLAYER_WIDTH: f32 = 0.6;

/// How tall the player's body is, in blocks. Mirrors `game.PlayerHeight`.
pub const PLAYER_HEIGHT: f32 = 1.8;

/// How far above the feet the camera sits, in blocks.
///
/// Client-owned, not a mirror: the server decides where the body is, and where the eyes
/// are inside it is a rendering question — `schemas/player.fbs` says as much about yaw.
/// Written as a fraction of the body so it cannot drift outside it.
pub const EYE_HEIGHT: f32 = PLAYER_HEIGHT * 0.9;

/// Radians of turn per logical pixel of pointer movement.
///
/// Not a setting yet, because there is no settings menu to put it in. A full turn takes
/// about 2 000 pixels, which is a comfortable desk-width sweep.
pub const LOOK_SENSITIVITY: f32 = 0.003;

/// How far the camera may tilt up or down, in radians.
///
/// Just short of straight up and straight down. At exactly ±π/2 the yaw and the view
/// direction become degenerate — every yaw looks the same — and the view flips as the
/// pitch crosses it.
pub const MAX_PITCH: f32 = FRAC_PI_2 - 0.01;

/// How far the player can aim, in blocks, measured from the eye along the view ray.
///
/// # This one bounds a *request*, and the server bounds the edit
///
/// It decides which voxel gets an outline and therefore which voxel a click asks about.
/// It is not the rule: `schemas/world.fbs` puts reach on the server, which checks it
/// against the position it computed rather than anything the client said, and refuses an
/// edit beyond it in silence. So the two numbers are allowed to differ, and the failure
/// is asymmetric — a client reaching **further** than the server offers the player
/// outlines on blocks that will not break, which is the confusing direction. **This
/// value must therefore not exceed the server's**, and when they disagree this is the one
/// to change.
///
/// 4.5 blocks is a little over two body heights and roughly an arm's length past the
/// player's own footprint, so a wall can be dug into from where a player is standing and
/// a block can be placed on the ground at their feet without aiming down at it.
///
/// # The magnitudes agreeing is not the same as the reaches agreeing
///
/// The server's copy arrives with the server-side edit issue, and it measures a different
/// segment: **body centre to voxel centre**, where this measures **eye to the point the
/// ray enters the voxel**. Both endpoints move this side's answer *down* relative to the
/// server's for a target above the player — the eye is [`EYE_HEIGHT`] above the feet where
/// the body centre is half [`PLAYER_HEIGHT`], and an entry point is nearer than a centre —
/// so an identical number on both sides still leaves this side the more permissive of the
/// two by up to about a block. Reconciling *what is measured* needs both halves merged and
/// belongs to its own issue; until then the outline is occasionally optimistic at the
/// extreme of its range, which shows up as a click that does nothing.
pub const MAX_REACH: f32 = 4.5;

/// How far behind the eyes the third-person camera sits, in blocks.
///
/// Far enough that the whole character is on screen — the rig is one block wide and
/// [`PLAYER_HEIGHT`] tall — and near enough that the boom rarely has anywhere to be cut
/// short indoors, which is the failure a longer one produces constantly.
///
/// **It is not a reach and nothing measures a gameplay distance from it.** Aiming is off
/// in the view this belongs to, precisely so this number and [`MAX_REACH`] can never need
/// to agree.
pub const BOOM_LENGTH: f32 = 4.0;

/// How far in front of a wall the boom stops when the camera would otherwise be inside it.
///
/// The near plane is what this is really about: a camera exactly on a face renders the
/// voxel behind it clipped open, so the stop is pulled forward by more than nothing.
pub const BOOM_CLEARANCE: f32 = 0.25;

/// How fast the camera swings back behind the character once the orbit is released, as
/// the fraction of the remaining angle left after one second.
///
/// Exponential rather than a fixed duration, because the distance to travel is whatever
/// the player happened to orbit to. `settle_the_orbit` snaps the last
/// [`ORBIT_SETTLED`] radians rather than approaching for ever.
pub const ORBIT_RETURN_PER_SECOND: f32 = 0.0005;

/// The angle below which the returning camera is simply placed at rest.
///
/// Half a tenth of a degree: far below what a frame can show, and the difference between
/// an animation that ends and one that only ever gets closer.
pub const ORBIT_SETTLED: f32 = 0.001;

// The relationships between the numbers above, checked at compile time rather than by a
// test. Each is a property of the *build* rather than of a run: a camera outside the body
// it is following, or a pitch limit at exactly vertical, are states no build should be able
// to produce, and a `const` assert says so where a test would only find out afterwards.
//
// The body's own proportions are not among them any more. They were, while the body was
// one capsule inscribed in the collision box; a rig of a dozen-odd boxes has relationships
// a `const` expression cannot state, so `super::appearance` asserts them as tests instead
// — which is also where the parts that deliberately leave the box are named.

/// A camera above the head renders from outside the thing it follows; one at the feet
/// renders from inside the ground.
const _: () = assert!(EYE_HEIGHT > 0.0 && EYE_HEIGHT < PLAYER_HEIGHT);

/// At exactly ±π/2 the view direction is the up axis, every yaw looks identical, and the
/// image flips as the pitch crosses it.
const _: () = assert!(MAX_PITCH > 0.0 && MAX_PITCH < FRAC_PI_2);

/// A reach shorter than the drop from the eyes to the player's own feet cannot target
/// the block being stood on — which is the first edit every player tries.
const _: () = assert!(MAX_REACH > EYE_HEIGHT);
