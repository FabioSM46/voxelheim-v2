//! The scorpion: a procedural rig inside the box the server collides, and nothing else.
//!
//! A segmented carapace, two pincers of three parts and a movable finger each, eight legs of
//! two bones and a five-segment tail ending in a stinger, all built from primitives at
//! startup — no model, texture or animation file. Every dimension below is a fraction of
//! [`body`]`(Scorpion)`, so the drawn creature follows the server's box rather than holding a
//! size of its own. The legs are the spider's mechanism with the scorpion's proportions: both
//! step, solve and draw through `arthropod.rs`.
//!
//! **Everything here is presentation of an authoritative answer.** The root stands where the
//! snapshot says and faces where it says; the gait is driven by the distance the drawn root
//! actually travelled, the telegraphs and strikes by the action the server sent, and the death
//! by the fall `mobs.rs` starts on `Corpse`.
//!
//! # Which attack a windup is
//!
//! The server has two, and sends both as `MobAction::Windup` (#1291, PR #1302): a **sting**
//! behind an 1100 ms telegraph, and a **pincer swipe** behind 450 ms. The rhythm is fixed —
//! the sting always opens, then the two alternate, turning over on every committed swing — so
//! the client can tell them apart by that order, and by how long the telegraph has lasted.
//! [`Motion`] keeps the order it has watched and corrects it from the length: a windup still
//! running after [`STING_EVIDENCE`] is a sting whatever the order said, and one watched from
//! its start that ended sooner was a swipe. That is a reading of what was sent, and it decides
//! nothing: which blow lands, and whether it lands, is the server's.
//!
//! # Under the sand
//!
//! The wire has no member for burial either. A buried scorpion is sent as `Idle`, standing
//! one block below the surface it will rise to, so its whole body is inside the sand; no live
//! creature above ground is ever inside a solid block, so that is the signal — [`buried`] in
//! `mobs.rs`. It is drawn as a low mound on the surface above it, whose sand shifts as this
//! session's player comes closer. The server raises it when a live player comes within
//! [`EMERGE_RANGE`], moving it onto the surface in `Recovery` for 800 ms; the client draws
//! that as the body bursting up through the sand over those 800 ms, throwing grains outward.
use std::f32::consts::{PI, TAU};

use super::arthropod::{self, BoxPart, LEGS, bone_transform, part, side};
use super::buried;
use super::*;

/// How many segments the tail has before its stinger.
pub(super) const TAIL: usize = 5;

/// One independently posed part. Pincers and legs are numbered left before right; legs front
/// to back in pairs, as in [`arthropod`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Segment {
    /// Head shield, the seven plates of the back, the belly and the eyes: one rigid body.
    Carapace,
    /// The pedipalp's first bone, shoulder to elbow.
    Humerus(u8),
    /// Elbow to wrist.
    Forearm(u8),
    /// The chela: the swollen hand and its fixed finger.
    Claw(u8),
    /// The movable finger, hinged on the outside of the hand.
    Finger(u8),
    /// One of the five tail segments, base first.
    Tail(u8),
    /// The telson: the venom bulb and its curved barb.
    Stinger,
    /// The femur: hip to knee.
    Upper(u8),
    /// The tibia and tarsus: knee to the tip on the ground.
    Lower(u8),
    /// The low pile of sand over a buried scorpion, drawn only while it lies there and as it
    /// comes up through it.
    Mound,
    /// One grain of the sand it throws, drawn only while one is in the air.
    Grain(u8),
}

/// How many grains a scorpion throws.
pub(super) const GRAINS: usize = 10;

/// The creature's own parts, which come first in every pose array; the sand follows them.
pub(super) const RIG: usize = 1 + 4 * 2 + TAIL + 1 + 2 * LEGS;
pub(super) const SEGMENT_COUNT: usize = RIG + 1 + GRAINS;

const fn segment_at(index: usize) -> Segment {
    match index {
        0 => Segment::Carapace,
        1..=2 => Segment::Humerus((index - 1) as u8),
        3..=4 => Segment::Forearm((index - 3) as u8),
        5..=6 => Segment::Claw((index - 5) as u8),
        7..=8 => Segment::Finger((index - 7) as u8),
        9..=13 => Segment::Tail((index - 9) as u8),
        14 => Segment::Stinger,
        15..=22 => Segment::Upper((index - 15) as u8),
        23..=30 => Segment::Lower((index - 23) as u8),
        31 => Segment::Mound,
        _ => Segment::Grain((index - 32) as u8),
    }
}

pub(super) const SEGMENTS: [Segment; SEGMENT_COUNT] = {
    let mut segments = [Segment::Carapace; SEGMENT_COUNT];
    let mut index = 0;
    while index < SEGMENT_COUNT {
        segments[index] = segment_at(index);
        index += 1;
    }
    segments
};

/// Where a segment sits in [`SEGMENTS`] and in every pose array.
pub(super) fn index(segment: Segment) -> usize {
    match segment {
        Segment::Carapace => 0,
        Segment::Humerus(arm) => 1 + usize::from(arm),
        Segment::Forearm(arm) => 3 + usize::from(arm),
        Segment::Claw(arm) => 5 + usize::from(arm),
        Segment::Finger(arm) => 7 + usize::from(arm),
        Segment::Tail(ring) => 9 + usize::from(ring),
        Segment::Stinger => 9 + TAIL,
        Segment::Upper(leg) => 10 + TAIL + usize::from(leg),
        Segment::Lower(leg) => 10 + TAIL + LEGS + usize::from(leg),
        Segment::Mound => RIG,
        Segment::Grain(grain) => RIG + 1 + usize::from(grain),
    }
}

/// Server timings for the scorpion, mirrored from its `mobRegistry` row (#1291, PR #1302).
/// `tests::the_timings_are_the_servers` reads that row and fails when either side moves.
pub(super) const STING_WINDUP: f32 = 1.1;
pub(super) const STING_RECOVERY: f32 = 1.4;
pub(super) const SWIPE_WINDUP: f32 = 0.45;
pub(super) const SWIPE_RECOVERY: f32 = 0.7;
/// How long rising out of the sand takes, spent as the `Recovery` before its first attack.
pub(super) const EMERGENCE: f32 = 0.8;
/// How close a live player comes before a buried scorpion rises.
pub(super) const EMERGE_RANGE: f32 = 5.0;

/// How long a windup must have run to be a sting, whatever the order said: halfway between
/// the two telegraphs, so a snapshot's worth of jitter either side cannot flip the answer.
pub(super) const STING_EVIDENCE: f32 = (STING_WINDUP + SWIPE_WINDUP) / 2.0;

/// How long the sting takes to drive home once the windup ends, and the pincers to close.
/// The strike is the fast half of the rhythm: a small fraction of the telegraph before it.
pub(super) const STRIKE_TIME: f32 = 0.12;
const SWEEP_TIME: f32 = 0.10;

/// How far out a player starts to stir a buried scorpion's sand, in blocks: a few steps
/// before the server's [`EMERGE_RANGE`], so a player who is watching the ground gets a
/// warning before the ambush.
pub(super) const STIR_RANGE: f32 = 9.0;
/// How far under the surface the body starts as it comes up, as a fraction of its height.
const EMERGE_DEPTH: f32 = 0.9;
/// How long a thrown grain can be in the air.
pub(super) const GRAIN_TIME: f32 = 0.9;

/// The sand block's own colour, so a mound is the floor it stands on heaped up, and the
/// darker churned sand at its crest.
const SAND: Color = Color::LinearRgba(LinearRgba::rgb(
    crate::world::palette::SAND_LINEAR[0],
    crate::world::palette::SAND_LINEAR[1],
    crate::world::palette::SAND_LINEAR[2],
));
const SAND_CHURNED: Color = Color::LinearRgba(LinearRgba::rgb(
    crate::world::palette::SAND_LINEAR[0] * 0.78,
    crate::world::palette::SAND_LINEAR[1] * 0.76,
    crate::world::palette::SAND_LINEAR[2] * 0.72,
));

/// Dark bronze chitin over amber legs: a creature that reads against pale sand rather than
/// vanishing into it, with the tail darkening to a near-black stinger.
const CHITIN: Color = Color::srgb(0.26, 0.17, 0.08);
const CHITIN_DARK: Color = Color::srgb(0.17, 0.11, 0.05);
const RIDGE: Color = Color::srgb(0.36, 0.25, 0.12);
const BELLY: Color = Color::srgb(0.55, 0.43, 0.24);
const LEG_CHITIN: Color = Color::srgb(0.50, 0.36, 0.16);
const JOINT_BAND: Color = Color::srgb(0.30, 0.20, 0.09);
const CLAW: Color = Color::srgb(0.33, 0.21, 0.09);
const FINGER: Color = Color::srgb(0.12, 0.07, 0.035);
const STINGER: Color = Color::srgb(0.20, 0.07, 0.04);
const BARB: Color = Color::srgb(0.72, 0.64, 0.46);
const EYE: Color = Color::srgb(0.02, 0.02, 0.02);

/// Hips either side of the body, and where each pair's tip rests, as fractions of the box
/// width. Lower and more splayed than a spider's: a scorpion carries its body close to the
/// ground on legs that bend out rather than up.
const HIP_X: f32 = 0.11;
const HIP_Z: [f32; 4] = [-0.15, -0.07, 0.01, 0.09];
const FOOT_X: [f32; 4] = [0.30, 0.34, 0.35, 0.33];
const FOOT_Z: [f32; 4] = [-0.24, -0.08, 0.10, 0.27];
/// How far along the hip-to-foot line the knee sits: past halfway, so the femur reaches out
/// almost level and the tibia drops to the sand — a crouch, not a spider's arch.
const KNEE_ALONG: f32 = 0.55;

/// Leg thickness, as fractions of the box width.
const FEMUR: f32 = 0.034;
const TIBIA: f32 = 0.027;
const TARSUS: f32 = 0.012;

/// Heights, as fractions of the box height.
const HIP_Y: f32 = 0.30;
const KNEE_Y: f32 = 0.50;

/// One gait cycle, as a fraction of the box width, and a swinging tip's lift, as a fraction
/// of its height. A shorter stride than the spider's, for the slowest walker in the game.
const STRIDE: f32 = 0.34;
const LIFT: f32 = 0.14;

/// The pedipalp's joints at rest, as fractions of width (x, z) and height (y), left side;
/// the right mirrors them.
const SHOULDER: [f32; 3] = [-0.10, 0.30, -0.24];
const ELBOW: [f32; 3] = [-0.25, 0.36, -0.27];
const WRIST: [f32; 3] = [-0.19, 0.32, -0.30];
/// How far each claw turns in toward the other at rest, radians.
const CLAW_TOE_IN: f32 = 0.15;

/// The tail's base, behind the last plate of the back.
const TAIL_BASE: [f32; 2] = [0.36, 0.19];
/// Each tail segment's length and thickness at its base, as fractions of the box width, and
/// the stinger's length.
const TAIL_LENGTH: [f32; TAIL] = [0.075, 0.075, 0.08, 0.085, 0.09];
const TAIL_WIDTH: [f32; TAIL + 1] = [0.075, 0.07, 0.066, 0.062, 0.06, 0.056];
const STINGER_LENGTH: f32 = 0.11;
const TAIL_OVERLAP: f32 = 0.02;

/// Each tail segment's pitch at rest, the stinger's last: radians from pointing straight
/// back, rising through straight up to pointing forward. The arc up and over the back every
/// scorpion carries, with the stinger poised above the body pointing ahead.
pub(super) const TAIL_REST: [f32; TAIL + 1] = [0.40, 1.15, 1.85, 2.45, 2.95, 3.45];
/// Added in full at the top of the sting's windup: the tail drawn higher and further back.
const TAIL_COCK: [f32; TAIL + 1] = [0.30, 0.05, -0.20, -0.30, -0.35, -0.35];
/// Added in full at the bottom of the strike: the tail whipped forward over the head.
const TAIL_STRIKE: [f32; TAIL + 1] = [0.10, 0.50, 0.85, 1.00, 1.05, 1.15];
/// Added in full once dead: the tail sagging onto the sand but still curled in over the body,
/// so the corpse stays inside the box the server collides — a tail straightened out behind it
/// would overhang by a fifth of the box (review on #1319).
const TAIL_LIMP: [f32; TAIL + 1] = [-0.45, -1.10, -1.50, -1.40, -1.00, -0.60];

/// How fast the eased attack poses settle, per second: quick enough that a strike still lands
/// in its own tenth of a second, slow enough that an interrupted telegraph does not snap.
const POSE_RESPONSE: f32 = 30.0;
const GAIT_RESPONSE: f32 = 10.0;
const SPEED_RESPONSE: f32 = 8.0;

/// A drawn root that jumps further than this in one frame was corrected or streamed, not
/// walked: it moves no leg.
const CORRECTION: f32 = 0.75;

fn frame() -> (f32, f32) {
    let envelope = body(MobKind::Scorpion);
    (envelope.width, envelope.height)
}

/// A pedipalp joint for `arm`. The left arm is 0 and its x fractions are authored negative,
/// so `-side` of a left arm (+1) keeps them and the right flips them.
fn at(fraction: [f32; 3], arm: usize) -> Vec3 {
    let (w, h) = frame();
    Vec3::new(
        fraction[0] * w * -side(arm),
        fraction[1] * h,
        fraction[2] * w,
    )
}

fn shoulder(arm: usize) -> Vec3 {
    at(SHOULDER, arm)
}

pub(super) fn hip(leg: usize) -> Vec3 {
    let (w, h) = frame();
    Vec3::new(side(leg) * HIP_X * w, HIP_Y * h, HIP_Z[leg / 2] * w)
}

pub(super) fn rest_foot(leg: usize) -> Vec3 {
    let (w, _) = frame();
    Vec3::new(
        side(leg) * FOOT_X[leg / 2] * w,
        TARSUS * w / 2.0,
        FOOT_Z[leg / 2] * w,
    )
}

fn rest_knee(leg: usize) -> Vec3 {
    let (_, h) = frame();
    let (hip, foot) = (hip(leg), rest_foot(leg));
    let along = hip + (foot - hip) * KNEE_ALONG;
    Vec3::new(along.x, KNEE_Y * h, along.z)
}

/// The two bone lengths of one leg, fixed by its rest pose.
pub(super) fn lengths(leg: usize) -> (f32, f32) {
    let knee = rest_knee(leg);
    (knee.distance(hip(leg)), knee.distance(rest_foot(leg)))
}

/// Where one leg's tip sits relative to its rest, `phase` radians into the gait.
pub(super) fn gait_offset(leg: usize, phase: f32) -> Vec3 {
    let (w, h) = frame();
    arthropod::stride_offset(leg, phase, STRIDE * w, LIFT * h)
}

/// The gait phase one block of travel advances, in radians.
pub(super) fn radians_per_block() -> f32 {
    TAU / (STRIDE * frame().0)
}

/// The rigid body's parts, in the feet-centred rest frame the root stands in. Front is -Z.
fn carapace() -> Vec<BoxPart> {
    let (w, h) = frame();
    let mut parts = vec![
        // The head shield, a raised crown on it, and the chelicerae under its front edge.
        part(
            [0.24 * w, 0.20 * h, 0.19 * w],
            [0.0, 0.34 * h, -0.18 * w],
            CHITIN,
        ),
        part(
            [0.16 * w, 0.04 * h, 0.15 * w],
            [0.0, 0.46 * h, -0.18 * w],
            RIDGE,
        ),
        part(
            [0.07 * w, 0.07 * h, 0.04 * w],
            [0.0, 0.30 * h, -0.29 * w],
            CHITIN_DARK,
        ),
        // The pale belly plate under the back.
        part(
            [0.22 * w, 0.04 * h, 0.30 * w],
            [0.0, 0.25 * h, 0.04 * w],
            BELLY,
        ),
        // A ridge down the middle of the back.
        part(
            [0.03 * w, 0.02 * h, 0.29 * w],
            [0.0, 0.45 * h, 0.05 * w],
            RIDGE,
        ),
    ];
    // The two median eyes on the crown.
    for x in [-0.022, 0.022] {
        parts.push(part(
            [0.035 * w, 0.04 * h, 0.03 * w],
            [x * w, 0.49 * h, -0.21 * w],
            EYE,
        ));
    }
    // Seven overlapping plates, widest in the middle, alternately banded: the segmented back
    // that says "scorpion" before the tail does.
    const WIDTH: [f32; 7] = [0.24, 0.26, 0.27, 0.27, 0.26, 0.24, 0.21];
    for (plate, width) in WIDTH.into_iter().enumerate() {
        let colour = if plate % 2 == 0 { CHITIN } else { CHITIN_DARK };
        parts.push(part(
            [width * w, 0.20 * h, 0.045 * w],
            [0.0, 0.34 * h, (-0.07 + plate as f32 * 0.04) * w],
            colour,
        ));
    }
    parts
}

/// The chela, in its own frame: origin at the wrist, reaching forward along -Z. `arm` picks
/// which side the fixed finger sits on — the inside.
fn claw(arm: usize) -> Vec<BoxPart> {
    let (w, h) = frame();
    let inside = -side(arm);
    vec![
        part([0.11 * w, 0.15 * h, 0.10 * w], [0.0, 0.0, -0.05 * w], CLAW),
        part([0.08 * w, 0.11 * h, 0.03 * w], [0.0, 0.0, -0.11 * w], CLAW),
        part(
            [0.028 * w, 0.06 * h, 0.08 * w],
            [inside * 0.026 * w, 0.0, -0.15 * w],
            FINGER,
        ),
    ]
}

/// Where the movable finger hinges on the chela, in the claw's frame: on the outside, at the
/// front of the hand.
fn hinge(arm: usize) -> Vec3 {
    let (w, _) = frame();
    Vec3::new(side(arm) * 0.026 * w, 0.0, -0.11 * w)
}

/// The movable finger, in its own frame: origin at the hinge, reaching forward along -Z.
fn finger() -> Vec<BoxPart> {
    let (w, h) = frame();
    vec![part(
        [0.028 * w, 0.06 * h, 0.08 * w],
        [0.0, 0.0, -0.04 * w],
        FINGER,
    )]
}

/// The mound over a buried scorpion, standing on the surface: three low slabs of sand, each
/// turned a little against the one under it so the pile reads as heaped rather than built,
/// the churned crest darker, and the tips of the two claws just showing at its front edge for
/// a player who looks.
fn mound() -> Mesh {
    let (w, h) = frame();
    let slab = |size: [f32; 3], centre: [f32; 3], turn: f32, colour| {
        draugr_box(Vec3::from_array(size), Vec3::from_array(centre), colour)
            .rotated_by(Quat::from_rotation_y(turn))
    };
    let mut mound = slab(
        [0.70 * w, 0.06 * h, 0.80 * w],
        [0.0, 0.03 * h, 0.02 * w],
        0.0,
        SAND,
    );
    let rest = [
        slab(
            [0.48 * w, 0.06 * h, 0.56 * w],
            [0.03 * w, 0.08 * h, 0.0],
            0.35,
            SAND,
        ),
        slab(
            [0.26 * w, 0.05 * h, 0.30 * w],
            [-0.02 * w, 0.13 * h, -0.03 * w],
            -0.30,
            SAND_CHURNED,
        ),
        draugr_box(
            Vec3::new(0.03 * w, 0.03 * h, 0.05 * w),
            Vec3::new(-0.12 * w, 0.04 * h, -0.39 * w),
            FINGER,
        ),
        draugr_box(
            Vec3::new(0.03 * w, 0.03 * h, 0.05 * w),
            Vec3::new(0.12 * w, 0.04 * h, -0.39 * w),
            FINGER,
        ),
    ];
    merge_all(&mut mound, rest, "scorpion mound");
    mound
}

/// The telson along +X from where it joins the last segment: a swollen venom bulb, then a
/// barb that narrows to a point and curves toward local +Y, the inside of the tail's curl.
fn stinger() -> Mesh {
    let (w, _) = frame();
    let length = STINGER_LENGTH * w;
    let bulb_length = 0.065 * w;
    let mut mesh = draugr_box(
        Vec3::new(bulb_length, 0.065 * w, 0.06 * w),
        Vec3::new(bulb_length / 2.0, 0.0, 0.0),
        STINGER,
    );
    let (a, b) = (0.016 * w, 0.004 * w);
    let bend = 0.035 * w;
    let corners = [
        Vec3::new(bulb_length, -a, a),
        Vec3::new(bulb_length, a, a),
        Vec3::new(bulb_length, a, -a),
        Vec3::new(bulb_length, -a, -a),
        Vec3::new(length, bend - b, b),
        Vec3::new(length, bend + b, b),
        Vec3::new(length, bend + b, -b),
        Vec3::new(length, bend - b, -b),
    ];
    let barb = draugr_tint(crate::player::shapes::hexahedron(corners), BARB);
    merge_all(&mut mesh, [barb], "scorpion barb");
    mesh
}

pub(super) fn meshes() -> Vec<(Segment, Mesh)> {
    let (w, _) = frame();
    SEGMENTS
        .into_iter()
        .map(|segment| {
            let mesh = match segment {
                Segment::Carapace => bosses::boxes(&carapace()),
                Segment::Humerus(arm) => {
                    let arm = usize::from(arm);
                    let length = at(ELBOW, arm).distance(shoulder(arm));
                    arthropod::bone(length, 0.052 * w, 0.046 * w, CLAW, Some(JOINT_BAND))
                }
                Segment::Forearm(arm) => {
                    let arm = usize::from(arm);
                    let length = at(WRIST, arm).distance(at(ELBOW, arm));
                    arthropod::bone(length, 0.055 * w, 0.068 * w, CLAW, None)
                }
                Segment::Claw(arm) => bosses::boxes(&claw(usize::from(arm))),
                Segment::Finger(_) => bosses::boxes(&finger()),
                Segment::Tail(ring) => {
                    let ring = usize::from(ring);
                    // A little longer than the joint spacing, so each ring overlaps the next
                    // and the arch reads as one armoured tail rather than a string of blocks.
                    arthropod::bone(
                        (TAIL_LENGTH[ring] + TAIL_OVERLAP) * w,
                        TAIL_WIDTH[ring] * w,
                        TAIL_WIDTH[ring + 1] * w,
                        if ring % 2 == 0 { CHITIN } else { CHITIN_DARK },
                        None,
                    )
                }
                Segment::Stinger => stinger(),
                Segment::Upper(leg) => {
                    let (upper, _) = lengths(usize::from(leg));
                    arthropod::bone(upper, FEMUR * w, TIBIA * w, LEG_CHITIN, Some(JOINT_BAND))
                }
                Segment::Lower(leg) => {
                    let (_, lower) = lengths(usize::from(leg));
                    arthropod::bone(lower, TIBIA * w, TARSUS * w, LEG_CHITIN, None)
                }
                Segment::Mound => mound(),
                Segment::Grain(_) => draugr_box(Vec3::splat(0.035 * w), Vec3::ZERO, SAND),
            };
            (segment, mesh)
        })
        .collect()
}

pub(super) fn visuals(
    meshes_out: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) -> SpeciesVisuals {
    // Every colour is a vertex colour under one neutral lit material, so a hit flash and the
    // lootable wash stay a single handle swap per part, as for the other species. A little
    // glossier than the spider: a scorpion's shell catches the light.
    let material = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        perceptual_roughness: 0.42,
        ..default()
    });
    let parts: Vec<_> = meshes()
        .into_iter()
        .map(|(segment, mesh)| (segment, meshes_out.add(mesh)))
        .collect();
    SpeciesVisuals {
        body: parts[index(Segment::Carapace)].1.clone(),
        head: parts[index(Segment::Carapace)].1.clone(),
        legs: None,
        arms: None,
        eyes: None,
        body_material: material.clone(),
        head_material: material,
        king_parts: None,
        guardian_parts: None,
        spider_parts: None,
        scorpion_parts: Some(parts),
    }
}

/// One pincer's pose: how far it swings out (negative sweeps in), how far it lifts, and how
/// far its movable finger stands open (negative clenches).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(super) struct Arm {
    pub(super) spread: f32,
    pub(super) raise: f32,
    pub(super) open: f32,
}

/// Everything one frame's pose is made from.
#[derive(Debug, Clone, Copy)]
pub(super) struct Pose {
    /// Where each tip is, in the root frame.
    pub(super) feet: [Vec3; LEGS],
    /// The whole body's placement: bob, rear, lunge and the death drop.
    pub(super) body: Mat4,
    /// Each tail segment's pitch and the stinger's, as [`TAIL_REST`] is.
    pub(super) tail: [f32; TAIL + 1],
    /// The whole tail's swing about its base, radians, positive to the left.
    pub(super) sway: f32,
    pub(super) arms: [Arm; 2],
    /// How far into the death curl the legs are, zero to one.
    pub(super) curl: f32,
}

impl Pose {
    pub(super) fn rest() -> Self {
        Self {
            feet: std::array::from_fn(rest_foot),
            body: Mat4::IDENTITY,
            tail: TAIL_REST,
            sway: 0.0,
            arms: [Arm::default(); 2],
            curl: 0.0,
        }
    }
}

/// A tail segment's direction and the direction its curl turns toward, for `pitch` and
/// `sway`: the local X and Y of the segment's frame.
fn tail_axes(pitch: f32, sway: f32) -> (Vec3, Vec3) {
    let turn = Quat::from_rotation_y(sway);
    (
        turn * Vec3::new(0.0, pitch.sin(), pitch.cos()),
        turn * Vec3::new(0.0, pitch.cos(), -pitch.sin()),
    )
}

/// The tail's joints from base to stinger root, in the rest frame, before the body moves.
pub(super) fn tail_joints(tail: &[f32; TAIL + 1], sway: f32) -> [Vec3; TAIL + 1] {
    let (w, h) = frame();
    let mut joints = [Vec3::new(0.0, TAIL_BASE[0] * h, TAIL_BASE[1] * w); TAIL + 1];
    let mut joint = joints[0];
    for (ring, (pitch, length)) in tail.iter().zip(TAIL_LENGTH).enumerate() {
        joint += tail_axes(*pitch, sway).0 * length * w;
        joints[ring + 1] = joint;
    }
    joints
}

/// The pose of a part that is not drawn this frame: `mobs.rs` hides a scorpion part whose
/// transform is this rather than drawing it, so a zero scale never reaches the renderer.
pub(super) const HIDDEN: Transform = Transform::from_scale(Vec3::ZERO);

/// The transform of every segment in the root frame.
pub(super) fn pose(pose: &Pose) -> [Transform; SEGMENT_COUNT] {
    let body = pose.body;
    let (_, turn, _) = body.to_scale_rotation_translation();
    // The sand is not drawn unless `Motion` places it.
    let mut out = [HIDDEN; SEGMENT_COUNT];
    out[index(Segment::Carapace)] = Transform::from_matrix(body);

    for (arm, stance) in pose.arms.iter().enumerate() {
        let root = shoulder(arm);
        let swing =
            Quat::from_rotation_y(side(arm) * -stance.spread) * Quat::from_rotation_x(stance.raise);
        let elbow = root + swing * (at(ELBOW, arm) - root);
        let wrist = root + swing * (at(WRIST, arm) - root);
        let (root, elbow_at, wrist_at) = (
            body.transform_point3(root),
            body.transform_point3(elbow),
            body.transform_point3(wrist),
        );
        out[index(Segment::Humerus(arm as u8))] = bone_transform(root, elbow_at);
        out[index(Segment::Forearm(arm as u8))] = bone_transform(elbow_at, wrist_at);
        let hand = Transform::from_translation(wrist_at)
            .with_rotation(turn * swing * Quat::from_rotation_y(side(arm) * CLAW_TOE_IN));
        out[index(Segment::Claw(arm as u8))] = hand;
        out[index(Segment::Finger(arm as u8))] = hand
            * Transform::from_translation(hinge(arm))
                .with_rotation(Quat::from_rotation_y(-side(arm) * stance.open));
    }

    let joints = tail_joints(&pose.tail, pose.sway);
    for (ring, (joint, pitch)) in joints.into_iter().zip(pose.tail).enumerate() {
        let (along, curl) = tail_axes(pitch, pose.sway);
        let frame = Quat::from_mat3(&Mat3::from_cols(along, curl, along.cross(curl)));
        let segment = if ring < TAIL {
            Segment::Tail(ring as u8)
        } else {
            Segment::Stinger
        };
        out[index(segment)] =
            Transform::from_translation(body.transform_point3(joint)).with_rotation(turn * frame);
    }

    for leg in 0..LEGS {
        let hip = body.transform_point3(hip(leg));
        let foot = pose.feet[leg];
        let (upper, lower) = lengths(leg);
        let knee = arthropod::solve_knee(hip, foot, upper, lower, Vec3::new(side(leg), 0.0, 0.0));
        let (mut femur, mut tibia) = ((knee - hip).normalize(), (foot - knee).normalize());
        if pose.curl > 0.0 {
            // Blended as directions, so both bones keep their lengths all the way in.
            let out = (rest_foot(leg) - self::hip(leg))
                .with_y(0.0)
                .normalize_or(Vec3::new(side(leg), 0.0, 0.0));
            let (raised, folded) = arthropod::curled(out, 1.1, 0.6);
            femur = femur.lerp(raised, pose.curl).normalize_or(raised);
            tibia = tibia.lerp(folded, pose.curl).normalize_or(folded);
        }
        let knee = hip + femur * upper;
        out[index(Segment::Upper(leg as u8))] = bone_transform(hip, knee);
        out[index(Segment::Lower(leg as u8))] = bone_transform(knee, knee + tibia * lower);
    }
    out
}

/// The body's placement for the eased attack poses and the fall: `rear` lifts the front for
/// the sting, `lunge` drives it forward behind the strike, `down` settles it on the sand.
fn body_matrix(bob: f32, rear: f32, lunge: f32, down: f32) -> Mat4 {
    let (w, h) = frame();
    let lift = bob - 0.16 * h * down;
    let back = 0.04 * w * rear - 0.12 * w * lunge;
    let pitch = 0.16 * rear - 0.08 * lunge;
    Mat4::from_translation(Vec3::new(0.0, lift, back))
        * arthropod::around(Vec3::new(0.0, HIP_Y * h, 0.0), Quat::from_rotation_x(pitch))
}

/// Which of the two blows a windup is, or a recovery follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Attack {
    Sting,
    Swipe,
}

/// Which blow a windup that ran `elapsed` seconds was. Long enough to be a sting is a sting;
/// shorter, and watched from its start, is a swipe; shorter but first seen part of the way
/// through says nothing, and the order's guess stands.
pub(super) fn classify(guess: Attack, elapsed: f32, watched: bool) -> Attack {
    if elapsed >= STING_EVIDENCE {
        Attack::Sting
    } else if watched {
        Attack::Swipe
    } else {
        guess
    }
}

fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The attack pose's scalar controls: the tail's cock and strike, and the pincers'.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(super) struct Controls {
    pub(super) cock: f32,
    pub(super) strike: f32,
    pub(super) spread: f32,
    pub(super) raise: f32,
    pub(super) open: f32,
    pub(super) sweep: f32,
}

impl Controls {
    fn ease(&mut self, target: Self, response: f32) {
        let step = |now: &mut f32, to: f32| *now += (to - *now) * response;
        step(&mut self.cock, target.cock);
        step(&mut self.strike, target.strike);
        step(&mut self.spread, target.spread);
        step(&mut self.raise, target.raise);
        step(&mut self.open, target.open);
        step(&mut self.sweep, target.sweep);
    }
}

/// What the attack pose is `elapsed` seconds into `action`, for the blow it is.
///
/// **The sting's telegraph spans the server's whole 1100 ms windup** and its strike the first
/// [`STRIKE_TIME`] of the recovery that follows: the tail climbs and draws back for as long as
/// the player has to read it, then drives forward in a tenth of that. The swipe is the same
/// shape on the pincers and the swipe's 450 ms.
pub(super) fn controls(action: MobAction, attack: Option<Attack>, elapsed: f32) -> Controls {
    match (action, attack) {
        (MobAction::Windup, Some(Attack::Sting)) => {
            // Eased in and still climbing at the end: the tail is visibly on its way up for
            // the whole telegraph rather than arriving early and waiting.
            let t = (elapsed / STING_WINDUP).clamp(0.0, 1.0);
            let cock = t * (1.0 + t) / 2.0;
            Controls {
                cock,
                spread: 0.25 * cock,
                raise: 0.10 * cock,
                open: 0.20 * cock,
                ..default()
            }
        }
        (MobAction::Windup, Some(Attack::Swipe)) => {
            let open = smooth(elapsed / SWIPE_WINDUP);
            Controls {
                spread: 0.70 * open,
                raise: 0.35 * open,
                open: 0.70 * open,
                ..default()
            }
        }
        (MobAction::Recovery, Some(Attack::Sting)) => {
            let fired = smooth(elapsed / STRIKE_TIME);
            let strike = if elapsed < STRIKE_TIME {
                fired
            } else {
                1.0 - smooth((elapsed - STRIKE_TIME) / (STING_RECOVERY - STRIKE_TIME))
            };
            Controls {
                cock: 1.0 - fired,
                strike,
                spread: 0.25 * (1.0 - fired),
                ..default()
            }
        }
        (MobAction::Recovery, Some(Attack::Swipe)) => {
            let closed = smooth(elapsed / SWEEP_TIME);
            let sweep = if elapsed < SWEEP_TIME {
                closed
            } else {
                1.0 - smooth((elapsed - SWEEP_TIME) / (SWIPE_RECOVERY - SWEEP_TIME))
            };
            Controls {
                sweep,
                spread: 0.70 * (1.0 - closed),
                raise: 0.35 * (1.0 - closed),
                open: 0.70 * (1.0 - closed) - 0.10 * sweep,
                ..default()
            }
        }
        _ => Controls::default(),
    }
}

/// The height of the open air above a body standing at `feet`: the first voxel above its
/// middle that is not solid. A buried scorpion's mound sits on it.
pub(super) fn surface_above(feet: Vec3, solid: impl Fn(IVec3) -> bool) -> f32 {
    let (_, h) = frame();
    let mut voxel = (feet + Vec3::Y * (0.5 * h)).floor().as_ivec3();
    for _ in 0..4 {
        if !solid(voxel) {
            break;
        }
        voxel.y += 1;
    }
    voxel.y as f32
}

/// How much a buried scorpion's sand stirs for a player at `player`: nothing beyond
/// [`STIR_RANGE`], rising to all of it at the server's [`EMERGE_RANGE`], where it comes up.
/// Measured from where it would stand once risen, as the server measures.
pub(super) fn stir(risen: Vec3, player: Vec3) -> f32 {
    let (w, _) = frame();
    let distance = (player - risen).xz().length() - w / 2.0;
    ((STIR_RANGE - distance) / (STIR_RANGE - EMERGE_RANGE)).clamp(0.0, 1.0)
}

/// A fixed pseudo-random fraction for grain `grain` of scorpion `seed`, in `0..1`.
fn scatter(seed: u64, grain: usize, salt: u64) -> f32 {
    let mixed = (seed ^ (grain as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ salt)
        .wrapping_mul(0xbf58_476d_1ce4_e5b9);
    ((mixed >> 40) as f32) / (1u64 << 24) as f32
}

/// Where grain `grain` of the emergence burst is `elapsed` seconds after the sand broke, in
/// the frame of the mound's centre on the surface; `None` once it has fallen back in.
pub(super) fn thrown(seed: u64, grain: usize, elapsed: f32) -> Option<Vec3> {
    let (w, _) = frame();
    if elapsed >= GRAIN_TIME {
        return None;
    }
    let angle = grain as f32 * TAU / GRAINS as f32 + 0.5 * scatter(seed, grain, 1);
    let out = Vec3::new(angle.cos(), 0.0, angle.sin());
    let start = out * (0.12 * w * (0.5 + scatter(seed, grain, 2)));
    let across = 0.9 + 0.9 * scatter(seed, grain, 3);
    let up = 2.2 + 1.2 * scatter(seed, grain, 4);
    let at = start + out * across * elapsed + Vec3::Y * (up * elapsed - 4.9 * elapsed * elapsed);
    (at.y > -0.02).then_some(at)
}

/// Where grain `grain` hops `within` seconds into one stir of a buried scorpion's sand, for a
/// stir of `amount`; `None` for a grain sitting this one out. A small hop off the crest: the
/// sand shifting over something moving underneath it.
fn hopped(seed: u64, grain: usize, within: f32, amount: f32) -> Option<Vec3> {
    const HOP: f32 = 0.35;
    let (w, h) = frame();
    let hopping = 1 + (amount * 3.0).round() as usize;
    if amount <= 0.0 || grain >= hopping || within >= HOP {
        return None;
    }
    let angle = TAU * scatter(seed, grain, 5);
    let out = Vec3::new(angle.cos(), 0.0, angle.sin());
    let up = 0.6 + 0.9 * amount;
    let crest = Vec3::Y * (0.15 * h) + out * (0.06 * w);
    let at = crest + out * 0.35 * within + Vec3::Y * (up * within - 4.9 * within * within);
    (at.y > 0.0).then_some(at)
}

/// A scorpion coming up out of the sand.
#[derive(Debug, Clone, Copy)]
struct Emerge {
    elapsed: f32,
    /// The surface it broke, in world height.
    surface: f32,
}

/// One scorpion's cosmetic state between frames. Nothing here is sent or read as a fact.
#[derive(Debug)]
pub(super) struct Motion {
    last: Vec3,
    /// Radians into the gait cycle, advanced only by distance travelled.
    pub(super) phase: f32,
    /// How much of the gait is showing: one while walking, easing to nothing at a stop.
    pub(super) amplitude: f32,
    /// Smoothed drawn speed, blocks per second.
    pub(super) speed: f32,
    clock: f32,
    seed: u64,
    /// The action the newest frame drew, how long it has lasted here, and whether this client
    /// watched it begin.
    action: MobAction,
    pub(super) elapsed: f32,
    watched: bool,
    /// Whether the next windup is, by the order watched so far, the sting.
    sting_next: bool,
    /// The blow the current windup is, or the current recovery follows.
    pub(super) attack: Option<Attack>,
    pub(super) controls: Controls,
    /// Under the sand, and the height of the surface over it — see [`buried`].
    pub(super) buried: Option<f32>,
    emerge: Option<Emerge>,
    pub(super) transforms: [Transform; SEGMENT_COUNT],
}

impl Motion {
    /// A scorpion first seen doing `action`. With no rhythm watched yet, the sting is the
    /// guess: it is the blow that always opens.
    pub(super) fn new(position: Vec3, entity_id: u64, action: MobAction) -> Self {
        Self {
            last: position,
            phase: 0.0,
            amplitude: 0.0,
            speed: 0.0,
            clock: 0.0,
            seed: entity_id,
            action,
            elapsed: 0.0,
            watched: false,
            sting_next: true,
            attack: (action == MobAction::Windup).then_some(Attack::Sting),
            controls: Controls::default(),
            buried: None,
            emerge: None,
            transforms: pose(&Pose::rest()),
        }
    }

    /// Whether the scorpion is coming up out of the sand.
    #[cfg(test)]
    pub(super) fn emerging(&self) -> bool {
        self.emerge.is_some()
    }

    /// Follows the rhythm through one change of the server's action.
    fn observe(&mut self, action: MobAction) {
        if action == self.action {
            return;
        }
        let ended = std::mem::replace(&mut self.action, action);
        self.attack = match (ended, action) {
            (MobAction::Windup, MobAction::Recovery) => {
                let guess = self.attack.unwrap_or(Attack::Sting);
                let was = classify(guess, self.elapsed, self.watched);
                // The rhythm turns over on the swing, landed or not.
                self.sting_next = was == Attack::Swipe;
                Some(was)
            }
            (_, MobAction::Windup) => Some(if self.sting_next {
                Attack::Sting
            } else {
                Attack::Swipe
            }),
            // A windup that ended in anything but a swing was abandoned: the rhythm stands.
            _ => None,
        };
        self.elapsed = 0.0;
        self.watched = true;
    }

    /// One frame. `solid` is the streamed terrain, which is how a buried scorpion is told
    /// from a standing one; `player` is where this session's own body stands, which is what
    /// stirs the sand over one.
    pub(super) fn sample(
        &mut self,
        position: Vec3,
        action: MobAction,
        down: f32,
        delta: Duration,
        solid: impl Fn(IVec3) -> bool,
        player: Option<Vec3>,
    ) {
        let (w, h) = frame();
        let dt = delta.as_secs_f32().min(0.1);
        self.clock += dt;
        if action == self.action {
            self.elapsed += dt;
        }
        self.observe(action);
        if self.action == MobAction::Windup && self.elapsed >= STING_EVIDENCE {
            self.attack = Some(Attack::Sting);
        }

        let displacement = position - self.last;
        self.last = position;
        let travelled = if displacement.length() > CORRECTION {
            0.0
        } else {
            displacement.xz().length()
        };
        let alive = down == 0.0 && !matches!(action, MobAction::Dying | MobAction::Corpse);
        let under = alive && buried(MobKind::Scorpion, action, position, &solid);
        let gait = alive
            && !under
            && matches!(action, MobAction::Idle | MobAction::Chase | MobAction::Flee);

        // Under the sand until the frame it is not, and that frame is the emergence: the
        // server lifting it onto the surface in `Recovery`, drawn as the scorpion bursting up
        // through the sand it was lying in.
        if under {
            self.buried = Some(surface_above(position, &solid));
            self.emerge = None;
        } else if let Some(surface) = self.buried.take()
            && alive
        {
            self.emerge = Some(Emerge {
                elapsed: 0.0,
                surface,
            });
        }
        let emerged = match self.emerge.as_mut() {
            Some(emerge) if alive => {
                emerge.elapsed += dt;
                (emerge.elapsed / EMERGENCE).min(1.0)
            }
            _ => 1.0,
        };
        let rise = 1.0 - (1.0 - emerged) * (1.0 - emerged);
        let tuck = if under { 1.0 } else { 1.0 - rise };
        // Held at the height the rise has reached, whatever the interpolated root is doing
        // under it: the root lerps the whole block up in one snapshot, the body takes the
        // server's 800 ms.
        let offset = self.emerge.map_or(Vec3::ZERO, |emerge| {
            Vec3::Y * (emerge.surface - EMERGE_DEPTH * h * (1.0 - rise) - position.y)
        });

        self.phase = (self.phase + travelled * radians_per_block()).rem_euclid(TAU);
        let instant = if dt > 0.0 { travelled / dt } else { 0.0 };
        self.speed += (instant - self.speed) * (1.0 - (-SPEED_RESPONSE * dt).exp());
        let moving = gait && self.speed > 0.05;
        self.amplitude +=
            (f32::from(u8::from(moving)) - self.amplitude) * (1.0 - (-GAIT_RESPONSE * dt).exp());

        let target = if alive {
            controls(self.action, self.attack, self.elapsed)
        } else {
            Controls::default()
        };
        self.controls
            .ease(target, 1.0 - (-POSE_RESPONSE * dt).exp());
        let c = self.controls;

        let bob = self.amplitude * 0.025 * h * (2.0 * self.phase).sin().abs();
        let body = body_matrix(bob, c.cock, c.strike, down);
        let feet = std::array::from_fn(|leg| {
            let foot = rest_foot(leg) + gait_offset(leg, self.phase) * self.amplitude;
            // Folded in under the body while it lies buried and as it comes up.
            let tucked = hip(leg) + Vec3::new(side(leg) * 0.10 * w, -0.20 * h, 0.0);
            foot.lerp(tucked, tuck * 0.85)
        });

        // Standing, the tail sways slowly and one pincer now and then flexes its finger; the
        // walk rocks the tail with the stride. Neither is anything the server said.
        let still = alive && gait && self.amplitude < 0.3;
        let flex = still
            .then(|| arthropod::twitch(self.clock, self.seed, 2))
            .flatten();
        let idle_sway = 0.05 * (self.clock * 1.3 + self.seed as f32).sin();
        let walk_sway = 0.08 * self.amplitude * self.phase.sin();
        let sway = if alive && !under {
            idle_sway + walk_sway
        } else {
            0.0
        };

        let tail = std::array::from_fn(|ring| {
            TAIL_REST[ring]
                + c.cock * TAIL_COCK[ring]
                + c.strike * TAIL_STRIKE[ring]
                + (down + 0.8 * tuck).min(1.0) * TAIL_LIMP[ring]
                // A tremor at the top of the telegraph: the stinger quivering, poised.
                + c.cock * c.cock * 0.02 * (self.clock * TAU * 14.0).sin() * (ring as f32 / TAIL as f32)
        });
        // Coming up, the pincers lift clear of the sand before they settle.
        let shake = 0.35 * (PI * emerged).sin() * f32::from(u8::from(self.emerge.is_some()));
        let arms = std::array::from_fn(|arm| {
            let twitched = flex.filter(|(which, _)| *which == arm).map_or(0.0, |f| f.1);
            Arm {
                spread: c.spread - 0.45 * c.sweep + 0.15 * down - 0.30 * tuck,
                raise: c.raise + 0.12 * c.sweep - 0.20 * down - 0.15 * tuck + shake,
                open: c.open + 0.35 * twitched + shake,
            }
        });
        self.transforms = pose(&Pose {
            feet,
            body,
            tail,
            sway,
            arms,
            curl: down,
        });
        for transform in &mut self.transforms[..RIG] {
            transform.translation += offset;
        }
        self.place_sand(position, emerged, player);
        if emerged >= 1.0 {
            self.emerge = None;
        }
    }

    /// The mound over a buried scorpion, and the grains of sand it throws: shivering under a
    /// player coming close, and bursting outward as it comes up.
    fn place_sand(&mut self, position: Vec3, emerged: f32, player: Option<Vec3>) {
        let surface = self.buried.or(self.emerge.map(|emerge| emerge.surface));
        let Some(surface) = surface else {
            return;
        };
        let top = Vec3::Y * (surface - position.y);
        let (mound, grains) = if self.buried.is_some() {
            let amount = player.map_or(0.0, |player| stir(position.with_y(surface), player));
            let shiver = amount * (0.5 + 0.5 * (self.clock * 23.0).sin());
            let period = 1.3 - 0.8 * amount;
            let within = self.clock.rem_euclid(period);
            let mound = Transform::from_translation(top).with_scale(Vec3::new(
                1.0 + 0.04 * shiver,
                1.0 + 0.30 * shiver,
                1.0,
            ));
            let grains: [Option<Vec3>; GRAINS] =
                std::array::from_fn(|grain| hopped(self.seed, grain, within, amount));
            (Some(mound), grains)
        } else {
            let elapsed = emerged * EMERGENCE;
            // The mound slumps and spreads as the body comes up through it.
            let slump = smooth(emerged / 0.45);
            let mound = (slump < 1.0).then(|| {
                Transform::from_translation(top).with_scale(Vec3::new(
                    1.0 + 0.4 * slump,
                    1.0 - slump,
                    1.0 + 0.4 * slump,
                ))
            });
            let grains = std::array::from_fn(|grain| thrown(self.seed, grain, elapsed));
            (mound, grains)
        };
        if let Some(mound) = mound {
            self.transforms[index(Segment::Mound)] = mound;
        }
        for (grain, at) in grains.into_iter().enumerate() {
            if let Some(at) = at {
                self.transforms[index(Segment::Grain(grain as u8))] =
                    Transform::from_translation(top + at)
                        .with_rotation(Quat::from_rotation_y(self.clock * 9.0 + grain as f32));
            }
        }
    }
}

#[cfg(test)]
pub(super) fn posed_meshes(pose_now: &Pose) -> Vec<Mesh> {
    meshes()
        .into_iter()
        .take(RIG)
        .zip(pose(pose_now))
        .map(|((_, mesh), transform)| mesh.transformed_by(transform))
        .collect()
}

#[cfg(test)]
mod tests;
