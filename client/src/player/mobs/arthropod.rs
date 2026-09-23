//! What the two procedural arthropods share: eight legs of two bones each, stepped in an
//! alternating tetrapod and solved to their tips, drawn from tapered boxes.
//!
//! The cave spider (`spider.rs`) and the scorpion (`scorpion.rs`) are different animals in
//! every proportion, so nothing here holds a size: each caller passes its own lengths,
//! strides and thicknesses, read out of its own `body` row. What is shared is the mechanism —
//! how a knee is solved, how a planted foot keeps pace with the ground, how a bone is drawn
//! and posed — so a fix to any of it is one fix for both species.
use std::f32::consts::{PI, TAU};

use super::*;
use crate::player::shapes::hexahedron;

/// Eight legs, numbered front to back in pairs, left before right: leg `2p` is the left leg
/// of pair `p`, `2p + 1` the right.
pub(super) const LEGS: usize = 8;

/// -1 for a left leg, +1 for a right.
pub(super) fn side(leg: usize) -> f32 {
    if leg.is_multiple_of(2) { -1.0 } else { 1.0 }
}

/// Which half of the alternating tetrapod a leg belongs to: L1, R2, L3, R4 step together,
/// and R1, L2, R3, L4 step together half a cycle later. Four feet are always down.
pub(super) fn group(leg: usize) -> usize {
    (leg / 2 + leg % 2) % 2
}

/// Where one leg's tip sits relative to its rest, `phase` radians into the gait, for a
/// stride of `stride` and a lift of `lift`, both in blocks.
///
/// A foot on the ground slides back under the body at exactly the rate the body travels
/// forward, so it stays planted on the terrain; a foot in the air swings forward and lands a
/// quarter stride ahead of rest. The two groups are half a cycle apart.
pub(super) fn stride_offset(leg: usize, phase: f32, stride: f32, lift: f32) -> Vec3 {
    let shift = if group(leg) == 0 { 0.0 } else { 0.5 };
    let u = (phase / TAU + shift).rem_euclid(1.0);
    if u < 0.5 {
        let t = u / 0.5;
        Vec3::new(0.0, 0.0, -stride / 4.0 + stride / 2.0 * t)
    } else {
        let t = (u - 0.5) / 0.5;
        let eased = t * t * (3.0 - 2.0 * t);
        Vec3::new(
            0.0,
            lift * (PI * t).sin(),
            stride / 4.0 - stride / 2.0 * eased,
        )
    }
}

/// The knee that joins `hip` to `foot` with bones `upper` and `lower` long, bent upwards and
/// outwards (`outward` is the fallback direction for a degenerate reach). The reach is
/// clamped short of both a straight and a folded leg, so a tip asked for somewhere it cannot
/// go is pointed at rather than reached, and no bone ever stretches.
pub(super) fn solve_knee(hip: Vec3, foot: Vec3, upper: f32, lower: f32, outward: Vec3) -> Vec3 {
    let delta = foot - hip;
    let reach = delta
        .length()
        .clamp((upper - lower).abs() + 1e-3, upper + lower - 1e-3);
    let direction = delta.normalize_or(outward);
    let along = (upper * upper - lower * lower + reach * reach) / (2.0 * reach);
    let rise = (upper * upper - along * along).max(0.0).sqrt();
    let up = (Vec3::Y - direction * direction.y).normalize_or(outward);
    hip + direction * along + up * rise
}

/// The bone directions of a dead arthropod's leg reaching `out` from its hip: the femur
/// raised by `raise` radians and the tibia folded back in under the body by `fold`. Directions
/// rather than a target, because a folded leg is not a place two bones of unequal length can
/// be solved to reach.
pub(super) fn curled(out: Vec3, raise: f32, fold: f32) -> (Vec3, Vec3) {
    (
        (out * raise.cos() + Vec3::Y * raise.sin()).normalize(),
        (-out * fold.cos() - Vec3::Y * fold.sin()).normalize(),
    )
}

pub(super) type BoxPart = (Vec3, Vec3, Color);

pub(super) fn part(size: [f32; 3], centre: [f32; 3], colour: Color) -> BoxPart {
    (Vec3::from_array(size), Vec3::from_array(centre), colour)
}

/// A bone along +X from its joint, tapering from `from` to `to` across, in `colour`, with a
/// band of `band` just short of the far joint when one is given.
pub(super) fn bone(length: f32, from: f32, to: f32, colour: Color, band: Option<Color>) -> Mesh {
    let (a, b) = (from / 2.0, to / 2.0);
    // Authored along +Y, then turned so +Y becomes +X.
    let corners = [
        Vec3::new(-a, 0.0, a),
        Vec3::new(a, 0.0, a),
        Vec3::new(a, 0.0, -a),
        Vec3::new(-a, 0.0, -a),
        Vec3::new(-b, length, b),
        Vec3::new(b, length, b),
        Vec3::new(b, length, -b),
        Vec3::new(-b, length, -b),
    ];
    let turn = Quat::from_rotation_z(-FRAC_PI_2);
    let mut mesh = draugr_tint(hexahedron(corners), colour).rotated_by(turn);
    if let Some(band) = band {
        let size = from * 1.25;
        let ring = draugr_box(
            Vec3::new(length * 0.10, size, size),
            Vec3::new(length * 0.88, 0.0, 0.0),
            band,
        );
        merge_all(&mut mesh, [ring], "arthropod bone band");
    }
    mesh
}

/// A rotation of `rotation` about `pivot`.
pub(super) fn around(pivot: Vec3, rotation: Quat) -> Mat4 {
    Mat4::from_translation(pivot) * Mat4::from_quat(rotation) * Mat4::from_translation(-pivot)
}

/// A bone from `from` to `to`, drawn from a mesh authored along +X.
pub(super) fn bone_transform(from: Vec3, to: Vec3) -> Transform {
    let direction = (to - from).normalize_or(Vec3::X);
    Transform::from_translation(from).with_rotation(Quat::from_rotation_arc(Vec3::X, direction))
}

/// Which of `limbs` twitches at `clock` for a creature standing still, and how far through
/// the twitch it is. A fixed pseudo-random schedule per creature, so a crowd does not twitch
/// in step and a test can know when one will.
pub(super) fn twitch(clock: f32, seed: u64, limbs: usize) -> Option<(usize, f32)> {
    const LENGTH: f32 = 0.22;
    let period = 1.7 + (seed % 13) as f32 * 0.1;
    let cycle = (clock / period).floor();
    let within = (clock - cycle * period).max(0.0);
    if within >= LENGTH {
        return None;
    }
    let mixed = (seed ^ (cycle as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15))
        .wrapping_mul(0xbf58_476d_1ce4_e5b9);
    let limb = ((mixed >> 29) % limbs as u64) as usize;
    Some((limb, (PI * within / LENGTH).sin()))
}
