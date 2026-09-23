//! The cave spider: a procedural rig inside the box the server collides, and nothing else.
//!
//! Cephalothorax, abdomen, a pair of fangs, an eye cluster and eight legs of two segments
//! each, all built from primitives at startup — no model, texture or animation file. Every
//! dimension below is a fraction of [`body`]`(CaveSpider)`, so the drawn creature follows the
//! server's box rather than holding a size of its own.
//!
//! **Everything here is presentation of an authoritative answer.** The root stands where the
//! snapshot says and faces where it says; the gait is driven by the distance the drawn root
//! actually travelled, the rear-up and the lunge by the action the server sent, and the death
//! curl by the fall `mobs.rs` starts on `Corpse`. Nothing here decides that a spider moves,
//! bites or dies, and nothing it computes leaves the renderer.
use std::f32::consts::{PI, TAU};

use super::*;
use crate::player::shapes::hexahedron;

/// One independently posed part. Legs are numbered front to back in pairs, left before
/// right: leg `2p` is the left leg of pair `p`, `2p + 1` the right.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Segment {
    Cephalothorax,
    Abdomen,
    Fangs,
    /// Drawn in its own emissive material, so a cave with no light still shows the glint.
    Eyes,
    /// The femur: hip to knee.
    Upper(u8),
    /// The tibia and tarsus: knee to the tip on the ground.
    Lower(u8),
}

pub(super) const LEGS: usize = 8;
pub(super) const SEGMENT_COUNT: usize = 4 + 2 * LEGS;

pub(super) const SEGMENTS: [Segment; SEGMENT_COUNT] = [
    Segment::Cephalothorax,
    Segment::Abdomen,
    Segment::Fangs,
    Segment::Eyes,
    Segment::Upper(0),
    Segment::Upper(1),
    Segment::Upper(2),
    Segment::Upper(3),
    Segment::Upper(4),
    Segment::Upper(5),
    Segment::Upper(6),
    Segment::Upper(7),
    Segment::Lower(0),
    Segment::Lower(1),
    Segment::Lower(2),
    Segment::Lower(3),
    Segment::Lower(4),
    Segment::Lower(5),
    Segment::Lower(6),
    Segment::Lower(7),
];

/// Where a segment sits in [`SEGMENTS`] and in every pose array.
pub(super) fn index(segment: Segment) -> usize {
    match segment {
        Segment::Cephalothorax => 0,
        Segment::Abdomen => 1,
        Segment::Fangs => 2,
        Segment::Eyes => 3,
        Segment::Upper(leg) => 4 + usize::from(leg),
        Segment::Lower(leg) => 4 + LEGS + usize::from(leg),
    }
}

/// Dark chitin with a paler banding at the joints and a sickly marking on the abdomen, so a
/// spider reads as a spider in the little light a cave has rather than as a black blot.
const CHITIN: Color = Color::srgb(0.085, 0.075, 0.065);
const LEG_CHITIN: Color = Color::srgb(0.15, 0.12, 0.095);
const JOINT_BAND: Color = Color::srgb(0.36, 0.31, 0.24);
const MARKING: Color = Color::srgb(0.46, 0.41, 0.30);
const FANG: Color = Color::srgb(0.24, 0.06, 0.05);
const EYE_COLOUR: Color = Color::srgb(0.85, 0.10, 0.06);
/// A dim glint rather than the vargr's lamp: eight small points that catch the eye in the
/// dark without lighting anything around them.
const EYE_EMISSIVE: LinearRgba = LinearRgba::rgb(3.2, 0.32, 0.12);

/// Hips around the cephalothorax, and where each pair's tip rests, as fractions of the box
/// width. The front pair reaches forward and the back pair back, the fan every spider stands
/// in; no foot rests further out than the gait can carry it without leaving the box.
const HIP_X: f32 = 0.11;
const HIP_Z: [f32; 4] = [-0.22, -0.16, -0.10, -0.04];
const FOOT_X: [f32; 4] = [0.38, 0.43, 0.43, 0.37];
const FOOT_Z: [f32; 4] = [-0.33, -0.12, 0.10, 0.30];
/// How far along the hip-to-foot line the knee rises. Short of halfway, so the femur climbs
/// steeply and the tibia comes down in a long reach — the arch a spider's leg has.
const KNEE_ALONG: f32 = 0.42;

/// Leg thickness, as fractions of the box width.
const FEMUR: f32 = 0.045;
const TIBIA: f32 = 0.036;
const TARSUS: f32 = 0.014;

/// Heights, as fractions of the box height.
const HIP_Y: f32 = 0.42;

/// One gait cycle, as a fraction of the box width: the distance the body travels while every
/// leg lifts once. A quarter of it either side of rest is the whole of a foot's travel, which
/// is what keeps the front and back tips inside the box however long the run.
const STRIDE: f32 = 0.5;
/// How high a swinging tip lifts, as a fraction of the box height.
const LIFT: f32 = 0.18;

/// How long a spider takes to come out of the wall it was first seen against.
pub(super) const EMERGE_TIME: f32 = 0.6;
/// How far back into the burrow it starts, as a fraction of the box width.
const EMERGE_DEPTH: f32 = 0.7;
/// How far a probe looks for a wall around a newly seen spider, in blocks.
const PROBE_REACH: f32 = 1.5;

/// How fast the eased poses settle, per second.
const GAIT_RESPONSE: f32 = 10.0;
const SPEED_RESPONSE: f32 = 8.0;

/// A drawn root that jumps further than this in one frame was corrected or streamed, not
/// walked: it moves no leg and ticks nothing.
const CORRECTION: f32 = 0.75;

fn frame() -> (f32, f32) {
    let envelope = body(MobKind::CaveSpider);
    (envelope.width, envelope.height)
}

/// -1 for a left leg, +1 for a right.
fn side(leg: usize) -> f32 {
    if leg.is_multiple_of(2) { -1.0 } else { 1.0 }
}

/// Which half of the alternating tetrapod a leg belongs to: L1, R2, L3, R4 step together,
/// and R1, L2, R3, L4 step together half a cycle later. Four feet are always down.
pub(super) fn group(leg: usize) -> usize {
    (leg / 2 + leg % 2) % 2
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
    let (w, h) = frame();
    let (hip, foot) = (hip(leg), rest_foot(leg));
    let along = hip + (foot - hip) * KNEE_ALONG;
    Vec3::new(along.x, h - FEMUR * w, along.z)
}

/// The two bone lengths of one leg, fixed by its rest pose.
pub(super) fn lengths(leg: usize) -> (f32, f32) {
    let knee = rest_knee(leg);
    (knee.distance(hip(leg)), knee.distance(rest_foot(leg)))
}

/// The knee that joins `hip` to `foot` with bones of this leg's lengths, bent upwards and
/// outwards. The reach is clamped short of both a straight and a folded leg, so a tip asked
/// for somewhere it cannot go is pointed at rather than reached, and no bone ever stretches.
pub(super) fn knee(leg: usize, hip: Vec3, foot: Vec3) -> Vec3 {
    let (upper, lower) = lengths(leg);
    let delta = foot - hip;
    let reach = delta
        .length()
        .clamp((upper - lower).abs() + 1e-3, upper + lower - 1e-3);
    let direction = delta.normalize_or(Vec3::new(side(leg), 0.0, 0.0));
    let along = (upper * upper - lower * lower + reach * reach) / (2.0 * reach);
    let rise = (upper * upper - along * along).max(0.0).sqrt();
    let up = (Vec3::Y - direction * direction.y).normalize_or(Vec3::new(side(leg), 0.0, 0.0));
    hip + direction * along + up * rise
}

/// Where one leg's tip sits relative to its rest, `phase` radians into the gait.
///
/// A foot on the ground slides back under the body at exactly the rate the body travels
/// forward, so it stays planted on the terrain; a foot in the air swings forward and lands a
/// quarter stride ahead of rest. The two groups are half a cycle apart.
pub(super) fn gait_offset(leg: usize, phase: f32) -> Vec3 {
    let (w, h) = frame();
    let stride = STRIDE * w;
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
            LIFT * h * (PI * t).sin(),
            stride / 4.0 - stride / 2.0 * eased,
        )
    }
}

/// The gait phase one block of travel advances, in radians.
pub(super) fn radians_per_block() -> f32 {
    TAU / (STRIDE * frame().0)
}

type BoxPart = (Vec3, Vec3, Color);

fn part(size: [f32; 3], centre: [f32; 3], colour: Color) -> BoxPart {
    (Vec3::from_array(size), Vec3::from_array(centre), colour)
}

/// The body parts, in the feet-centred rest frame the root stands in.
fn geometry(segment: Segment) -> Vec<BoxPart> {
    let (w, h) = frame();
    match segment {
        Segment::Cephalothorax => vec![
            // The carapace, a raised crown on it, the pedicel that joins it to the abdomen,
            // and the two short palps either side of the fangs.
            part(
                [0.30 * w, 0.24 * h, 0.30 * w],
                [0.0, 0.42 * h, -0.13 * w],
                CHITIN,
            ),
            part(
                [0.20 * w, 0.06 * h, 0.20 * w],
                [0.0, 0.565 * h, -0.14 * w],
                CHITIN,
            ),
            part(
                [0.10 * w, 0.10 * h, 0.08 * w],
                [0.0, 0.44 * h, 0.04 * w],
                CHITIN,
            ),
            part(
                [0.035 * w, 0.05 * h, 0.12 * w],
                [-0.085 * w, 0.36 * h, -0.30 * w],
                LEG_CHITIN,
            ),
            part(
                [0.035 * w, 0.05 * h, 0.12 * w],
                [0.085 * w, 0.36 * h, -0.30 * w],
                LEG_CHITIN,
            ),
        ],
        Segment::Abdomen => vec![
            // Stepped rather than one slab, so the silhouette swells like an abdomen: a
            // narrower core runs longer and higher than the wide belly around it.
            part(
                [0.40 * w, 0.34 * h, 0.36 * w],
                [0.0, 0.44 * h, 0.25 * w],
                CHITIN,
            ),
            part(
                [0.30 * w, 0.44 * h, 0.44 * w],
                [0.0, 0.47 * h, 0.25 * w],
                CHITIN,
            ),
            part(
                [0.20 * w, 0.06 * h, 0.30 * w],
                [0.0, 0.71 * h, 0.25 * w],
                CHITIN,
            ),
            // A pale chevron down the back, and the spinnerets at the tail.
            part(
                [0.08 * w, 0.02 * h, 0.26 * w],
                [0.0, 0.745 * h, 0.26 * w],
                MARKING,
            ),
            part(
                [0.18 * w, 0.02 * h, 0.04 * w],
                [0.0, 0.745 * h, 0.17 * w],
                MARKING,
            ),
            part(
                [0.12 * w, 0.02 * h, 0.04 * w],
                [0.0, 0.745 * h, 0.27 * w],
                MARKING,
            ),
            part(
                [0.08 * w, 0.08 * h, 0.04 * w],
                [0.0, 0.36 * h, 0.47 * w],
                LEG_CHITIN,
            ),
        ],
        Segment::Fangs => vec![
            part(
                [0.055 * w, 0.14 * h, 0.05 * w],
                [-0.045 * w, 0.30 * h, -0.285 * w],
                FANG,
            ),
            part(
                [0.055 * w, 0.14 * h, 0.05 * w],
                [0.045 * w, 0.30 * h, -0.285 * w],
                FANG,
            ),
        ],
        Segment::Eyes => {
            // Two large forward eyes, a row of four above them and a wide-set pair on the
            // flanks: the eight-eyed cluster, on the carapace's front face.
            let front = -0.28 * w - 0.01 * w;
            let mut eyes = vec![
                part(
                    [0.05 * w, 0.06 * h, 0.02 * w],
                    [-0.04 * w, 0.49 * h, front],
                    EYE_COLOUR,
                ),
                part(
                    [0.05 * w, 0.06 * h, 0.02 * w],
                    [0.04 * w, 0.49 * h, front],
                    EYE_COLOUR,
                ),
            ];
            for x in [-0.09, -0.03, 0.03, 0.09] {
                eyes.push(part(
                    [0.03 * w, 0.035 * h, 0.02 * w],
                    [x * w, 0.565 * h, front + 0.01 * w],
                    EYE_COLOUR,
                ));
            }
            for x in [-0.125, 0.125] {
                eyes.push(part(
                    [0.02 * w, 0.03 * h, 0.03 * w],
                    [x * w, 0.52 * h, -0.24 * w],
                    EYE_COLOUR,
                ));
            }
            eyes
        }
        Segment::Upper(_) | Segment::Lower(_) => Vec::new(),
    }
}

/// A leg bone along +X from its joint, tapering from `from` to `to` across, with a pale band
/// just short of the far joint.
fn bone(length: f32, from: f32, to: f32, band: bool) -> Mesh {
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
    let mut mesh = draugr_tint(hexahedron(corners), LEG_CHITIN).rotated_by(turn);
    if band {
        let size = from * 1.25;
        let ring = draugr_box(
            Vec3::new(length * 0.10, size, size),
            Vec3::new(length * 0.88, 0.0, 0.0),
            JOINT_BAND,
        );
        merge_all(&mut mesh, [ring], "spider leg band");
    }
    mesh
}

pub(super) fn meshes() -> Vec<(Segment, Mesh)> {
    let (w, _) = frame();
    SEGMENTS
        .into_iter()
        .map(|segment| {
            let mesh = match segment {
                Segment::Upper(leg) => {
                    let (upper, _) = lengths(usize::from(leg));
                    bone(upper, FEMUR * w, TIBIA * w, true)
                }
                Segment::Lower(leg) => {
                    let (_, lower) = lengths(usize::from(leg));
                    bone(lower, TIBIA * w, TARSUS * w, false)
                }
                _ => bosses::boxes(&geometry(segment)),
            };
            (segment, mesh)
        })
        .collect()
}

pub(super) fn visuals(
    meshes_out: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) -> SpeciesVisuals {
    // Every chitin colour is a vertex colour under one neutral lit material, so a hit flash
    // and the lootable wash stay a single handle swap per part, as for the other species.
    let material = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        perceptual_roughness: 0.55,
        ..default()
    });
    let parts: Vec<_> = meshes()
        .into_iter()
        .map(|(segment, mesh)| (segment, meshes_out.add(mesh)))
        .collect();
    let eyes = parts[index(Segment::Eyes)].1.clone();
    SpeciesVisuals {
        body: parts[index(Segment::Cephalothorax)].1.clone(),
        head: parts[index(Segment::Abdomen)].1.clone(),
        legs: None,
        arms: None,
        eyes: Some(EyeVisuals {
            mesh: eyes,
            material: materials.add(StandardMaterial {
                base_color: EYE_COLOUR,
                emissive: EYE_EMISSIVE,
                perceptual_roughness: 0.2,
                ..default()
            }),
        }),
        body_material: material.clone(),
        head_material: material,
        king_parts: None,
        guardian_parts: None,
        spider_parts: Some(parts),
    }
}

fn around(pivot: Vec3, rotation: Quat) -> Mat4 {
    Mat4::from_translation(pivot) * Mat4::from_quat(rotation) * Mat4::from_translation(-pivot)
}

/// A bone from `from` to `to`, drawn from a mesh authored along +X.
fn bone_transform(from: Vec3, to: Vec3) -> Transform {
    let direction = (to - from).normalize_or(Vec3::X);
    Transform::from_translation(from).with_rotation(Quat::from_rotation_arc(Vec3::X, direction))
}

/// Everything one frame's pose is made from.
#[derive(Debug, Clone, Copy)]
pub(super) struct Pose {
    /// Where each tip is, in the root frame.
    pub(super) feet: [Vec3; LEGS],
    /// The whole body's placement: bob, rear-up, lunge and the death drop.
    pub(super) body: Mat4,
    /// Fang rotation about their root, positive opening forward.
    pub(super) fangs: f32,
    /// Abdomen swell, a small fraction.
    pub(super) breath: f32,
    /// The whole creature's offset back into the burrow it is coming out of.
    pub(super) offset: Vec3,
    /// How far into the death curl the legs are, zero to one.
    pub(super) curl: f32,
}

impl Pose {
    pub(super) fn rest() -> Self {
        Self {
            feet: std::array::from_fn(rest_foot),
            body: Mat4::IDENTITY,
            fangs: 0.0,
            breath: 0.0,
            offset: Vec3::ZERO,
            curl: 0.0,
        }
    }
}

/// The transform of every segment in the root frame.
pub(super) fn pose(pose: &Pose) -> [Transform; SEGMENT_COUNT] {
    let (w, h) = frame();
    let shift = Mat4::from_translation(pose.offset);
    let body = shift * pose.body;
    let pedicel = Vec3::new(0.0, 0.44 * h, 0.05 * w);
    let abdomen = body
        * Mat4::from_translation(pedicel)
        * Mat4::from_scale(Vec3::splat(1.0 + pose.breath))
        * Mat4::from_translation(-pedicel);
    let fangs = body
        * around(
            Vec3::new(0.0, 0.37 * h, -0.285 * w),
            Quat::from_rotation_x(pose.fangs),
        );
    let mut out = [Transform::IDENTITY; SEGMENT_COUNT];
    out[index(Segment::Cephalothorax)] = Transform::from_matrix(body);
    out[index(Segment::Eyes)] = Transform::from_matrix(body);
    out[index(Segment::Abdomen)] = Transform::from_matrix(abdomen);
    out[index(Segment::Fangs)] = Transform::from_matrix(fangs);
    for leg in 0..LEGS {
        let hip = body.transform_point3(hip(leg));
        let foot = pose.feet[leg] + pose.offset;
        let knee = knee(leg, hip, foot);
        let (mut femur, mut tibia) = ((knee - hip).normalize(), (foot - knee).normalize());
        if pose.curl > 0.0 {
            // Blended as directions, so both bones keep their lengths and the knee its joint
            // all the way into the curl.
            let (raised, folded) = curled(leg);
            femur = femur.lerp(raised, pose.curl).normalize_or(raised);
            tibia = tibia.lerp(folded, pose.curl).normalize_or(folded);
        }
        let (upper, lower) = lengths(leg);
        let knee = hip + femur * upper;
        out[index(Segment::Upper(leg as u8))] = bone_transform(hip, knee);
        out[index(Segment::Lower(leg as u8))] = bone_transform(knee, knee + tibia * lower);
    }
    out
}

/// The body's placement for the eased attack poses and the fall.
fn body_matrix(bob: f32, rear: f32, strike: f32, down: f32) -> Mat4 {
    let (w, h) = frame();
    let lift = bob - 0.20 * h * down;
    let back = 0.05 * w * rear - 0.14 * w * strike;
    let pitch = 0.35 * rear - 0.12 * strike;
    Mat4::from_translation(Vec3::new(0.0, lift, back))
        * around(Vec3::new(0.0, HIP_Y * h, 0.0), Quat::from_rotation_x(pitch))
}

/// The bone directions of a dead spider's leg: the femur raised steeply and the tibia folded
/// back in under the body, the curl every dead spider lies in. Directions rather than a
/// target, because a folded leg is not a place the two bones can be solved to reach — their
/// lengths differ by more than the fold leaves between hip and tip.
fn curled(leg: usize) -> (Vec3, Vec3) {
    let out = (rest_foot(leg) - hip(leg))
        .with_y(0.0)
        .normalize_or(Vec3::new(side(leg), 0.0, 0.0));
    let (raise, fold) = (1.25_f32, 0.72_f32);
    (
        (out * raise.cos() + Vec3::Y * raise.sin()).normalize(),
        (-out * fold.cos() - Vec3::Y * fold.sin()).normalize(),
    )
}

/// Which leg twitches at `clock` for a spider standing still, and how far through the
/// twitch it is. A fixed pseudo-random schedule per spider, so a crowd does not twitch in
/// step and a test can know when one will.
pub(super) fn twitch(clock: f32, seed: u64) -> Option<(usize, f32)> {
    const LENGTH: f32 = 0.22;
    let period = 1.7 + (seed % 13) as f32 * 0.1;
    let cycle = (clock / period).floor();
    let within = (clock - cycle * period).max(0.0);
    if within >= LENGTH {
        return None;
    }
    let mixed = (seed ^ (cycle as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15))
        .wrapping_mul(0xbf58_476d_1ce4_e5b9);
    let leg = ((mixed >> 29) % LEGS as u64) as usize;
    Some((leg, (PI * within / LENGTH).sin()))
}

/// The direction, in world space, of the wall a newly seen spider is coming out of: away
/// from the open side, weighted by how far each of eight directions stays clear. `None`
/// when no probe meets a wall, or when every side is equally closed in.
pub(super) fn burrow(position: Vec3, solid: impl Fn(IVec3) -> bool) -> Option<Vec3> {
    let (_, h) = frame();
    let height = position.y + 0.4 * h;
    let mut open = Vec3::ZERO;
    let mut walled = false;
    for step in 0..8 {
        let angle = step as f32 * TAU / 8.0;
        let direction = Vec3::new(angle.cos(), 0.0, angle.sin());
        let mut clear = PROBE_REACH;
        let mut reach = 0.25;
        while reach <= PROBE_REACH {
            let point = Vec3::new(position.x, height, position.z) + direction * reach;
            if solid(point.floor().as_ivec3()) {
                clear = reach;
                walled = true;
                break;
            }
            reach += 0.25;
        }
        open += direction * clear;
    }
    // A one-sided wall sums to about two blocks of open space pointing away from it; an even
    // pit, whose sides the quarter-block probe steps only nearly balance, to a third of one.
    let back = -open;
    (walled && back.length() > 1.0).then(|| back.normalize())
}

#[derive(Debug, Clone, Copy)]
struct Emerge {
    /// Toward the burrow, in world space.
    direction: Vec3,
    elapsed: f32,
}

/// One spider's cosmetic state between frames. Nothing here is sent or read as a fact.
#[derive(Debug)]
pub(super) struct Motion {
    last: Vec3,
    /// Radians into the gait cycle, advanced only by distance travelled.
    pub(super) phase: f32,
    /// How much of the gait is showing: one while running, easing to nothing at a stop.
    pub(super) amplitude: f32,
    /// Smoothed drawn speed, blocks per second.
    pub(super) speed: f32,
    rear: f32,
    strike: f32,
    clock: f32,
    seed: u64,
    /// Whether this spider may still come out of a wall: only on its first frame.
    fresh: bool,
    emerge: Option<Emerge>,
    pub(super) transforms: [Transform; SEGMENT_COUNT],
}

impl Motion {
    /// `fresh` is whether this first sight may be a spider coming out of its burrow: alive,
    /// unhurt and not already doing anything but standing or running.
    pub(super) fn new(position: Vec3, entity_id: u64, fresh: bool) -> Self {
        Self {
            last: position,
            phase: 0.0,
            amplitude: 0.0,
            speed: 0.0,
            rear: 0.0,
            strike: 0.0,
            clock: 0.0,
            seed: entity_id,
            fresh,
            emerge: None,
            transforms: pose(&Pose::rest()),
        }
    }

    /// Whether the spider is still coming out of its burrow.
    #[cfg(test)]
    pub(super) fn emerging(&self) -> bool {
        self.emerge.is_some()
    }

    #[allow(clippy::too_many_arguments)] // One frame of every input a pose is made from.
    pub(super) fn sample(
        &mut self,
        position: Vec3,
        yaw: f32,
        action: MobAction,
        down: f32,
        delta: Duration,
        solid: impl Fn(IVec3) -> bool,
    ) {
        let (w, h) = frame();
        let dt = delta.as_secs_f32().min(0.1);
        self.clock += dt;

        if std::mem::take(&mut self.fresh) {
            self.emerge = burrow(position, solid).map(|direction| Emerge {
                direction,
                elapsed: 0.0,
            });
        }

        let displacement = position - self.last;
        self.last = position;
        let travelled = if displacement.length() > CORRECTION {
            0.0
        } else {
            displacement.xz().length()
        };
        let alive = down == 0.0 && !matches!(action, MobAction::Dying | MobAction::Corpse);
        let gait = alive && matches!(action, MobAction::Idle | MobAction::Chase | MobAction::Flee);

        self.phase = (self.phase + travelled * radians_per_block()).rem_euclid(TAU);

        let instant = if dt > 0.0 { travelled / dt } else { 0.0 };
        self.speed += (instant - self.speed) * (1.0 - (-SPEED_RESPONSE * dt).exp());
        let response = 1.0 - (-GAIT_RESPONSE * dt).exp();
        let moving = gait && self.speed > 0.05;
        self.amplitude += (f32::from(u8::from(moving)) - self.amplitude) * response;
        let target_rear = f32::from(u8::from(alive && action == MobAction::Windup));
        let target_strike = f32::from(u8::from(alive && action == MobAction::Recovery));
        let attack = 1.0 - (-LEAN_RESPONSE * dt).exp();
        self.rear += (target_rear - self.rear) * attack;
        self.strike += (target_strike - self.strike) * attack;

        let emerged = match self.emerge.as_mut() {
            Some(emerge) if alive => {
                emerge.elapsed += dt;
                (emerge.elapsed / EMERGE_TIME).min(1.0)
            }
            _ => 1.0,
        };
        let eased = emerged * emerged * (3.0 - 2.0 * emerged);
        let tuck = 1.0 - eased;
        let offset = self.emerge.map_or(Vec3::ZERO, |emerge| {
            Quat::from_rotation_y(yaw).inverse() * emerge.direction * (EMERGE_DEPTH * w * tuck)
        });
        if emerged >= 1.0 {
            self.emerge = None;
        }

        let bob = self.amplitude * 0.035 * h * (2.0 * self.phase).sin().abs();
        let body = body_matrix(bob, self.rear, self.strike, down);
        let still = alive && self.amplitude < 0.3 && gait;
        let twitching = still.then(|| twitch(self.clock, self.seed)).flatten();
        let feet = std::array::from_fn(|leg| {
            let mut foot = rest_foot(leg) + gait_offset(leg, self.phase) * self.amplitude;
            if leg < 2 {
                // The front pair rises to threaten while the server says windup, and comes
                // down forward in the lunge.
                foot += Vec3::new(0.0, 0.45 * h, -0.06 * w) * self.rear
                    + Vec3::new(0.0, 0.04 * h, -0.10 * w) * self.strike;
            }
            if let Some((twitched, amount)) = twitching
                && twitched == leg
            {
                foot += Vec3::new(0.0, 0.12 * h, -0.03 * w) * amount;
            }
            // Folded in under the body while coming out of the wall.
            let tucked = hip(leg) + Vec3::new(side(leg) * 0.12 * w, -0.30 * h, 0.0);
            foot.lerp(tucked, tuck * 0.8)
        });
        let breath = if alive {
            0.02 * (self.clock * 2.6).sin()
        } else {
            0.0
        };
        let fangs = if alive {
            0.8 * self.rear - 0.25 * self.strike
                + twitching.map_or(0.0, |(_, amount)| 0.25 * amount)
        } else {
            0.5 * down
        };
        self.transforms = pose(&Pose {
            feet,
            body,
            fangs,
            breath,
            offset,
            curl: down,
        });
    }
}

#[cfg(test)]
pub(super) fn posed_meshes(pose_now: &Pose) -> Vec<Mesh> {
    meshes()
        .into_iter()
        .zip(pose(pose_now))
        .map(|((_, mesh), transform)| mesh.transformed_by(transform))
        .collect()
}

#[cfg(test)]
mod tests;
