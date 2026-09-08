//! The accepted guardian geometry, divided at the joints that now move it.
//! Root placement is never animation output. Feet retain bounded world-space
//! contacts while the snapshot moves/turns the body; all secondary motion is cosmetic.
use super::*;
use bosses::{BONE, FROST, FUR, IRON, boxes};

pub(super) mod choreography;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Segment {
    Pelvis,
    Thorax,
    Neck,
    Head,
    Jaw,
    UpperFrontLeft,
    LowerFrontLeft,
    UpperFrontRight,
    LowerFrontRight,
    UpperRearLeft,
    LowerRearLeft,
    UpperRearRight,
    LowerRearRight,
    Tail,
    ChainLeft,
    ChainMiddle,
    ChainRight,
    CollarLeft,
    CollarRight,
}

pub(super) const SEGMENTS: [Segment; 19] = [
    Segment::Pelvis,
    Segment::Thorax,
    Segment::Neck,
    Segment::Head,
    Segment::Jaw,
    Segment::UpperFrontLeft,
    Segment::LowerFrontLeft,
    Segment::UpperFrontRight,
    Segment::LowerFrontRight,
    Segment::UpperRearLeft,
    Segment::LowerRearLeft,
    Segment::UpperRearRight,
    Segment::LowerRearRight,
    Segment::Tail,
    Segment::ChainLeft,
    Segment::ChainMiddle,
    Segment::ChainRight,
    Segment::CollarLeft,
    Segment::CollarRight,
];

type BoxPart = (Vec3, Vec3, Color);
fn part(size: [f32; 3], centre: [f32; 3], colour: Color) -> BoxPart {
    (Vec3::from_array(size), Vec3::from_array(centre), colour)
}

/// Coordinates stay in the original feet-centred frame. Splitting the shoulder
/// and leg cuboids adds internal faces, never changes their exterior surface.
fn geometry(segment: Segment) -> Vec<BoxPart> {
    use Segment::*;
    match segment {
        Pelvis => vec![part([0.86, 0.48, 0.50], [0.0, 0.85, 0.38], FUR)],
        Thorax => {
            // Overlap the neck internally; touching planes open a visible seam when breathing.
            let mut result = vec![part([1.20, 0.80, 0.57], [0.0, 1.18, -0.015], FUR)];
            for (x, height) in [(-0.45, 0.25), (-0.2, 0.38), (0.08, 0.30), (0.34, 0.34)] {
                result.push(part(
                    [0.20, height - 0.045, 0.43],
                    [x, 1.42 + (height - 0.045) / 2.0, -0.02],
                    FUR,
                ));
                result.push(part(
                    [0.15, 0.045, 0.30],
                    [x, 1.42 + height - 0.023, -0.02],
                    FROST,
                ));
            }
            result.push(part([0.07, 0.28, 0.08], [-0.58, 1.37, -0.12], IRON));
            result
        }
        Neck => vec![part([1.20, 0.80, 0.17], [0.0, 1.18, -0.345], FUR)],
        CollarLeft | CollarRight => vec![part(
            [0.375, 0.20, 0.58],
            [
                if segment == CollarLeft {
                    -0.1875
                } else {
                    0.1875
                },
                1.0,
                -0.25,
            ],
            IRON,
        )],
        Head => vec![
            part([0.62, 0.46, 0.40], [0.0, 0.97, -0.56], FUR),
            part([0.05, 0.15, 0.06], [-0.20, 0.73, -0.765], BONE),
            part([0.06, 0.11, 0.06], [0.20, 0.75, -0.765], BONE),
            part([0.13, 0.19, 0.12], [-0.23, 1.24, -0.48], FUR),
            part([0.13, 0.10, 0.12], [0.23, 1.20, -0.48], FUR),
            part(
                [0.07, 0.04, 0.025],
                [-0.17, 1.03, -0.775],
                Color::srgb(0.72, 0.43, 0.12),
            ),
            part(
                [0.07, 0.04, 0.025],
                [0.17, 1.03, -0.775],
                Color::srgb(0.72, 0.43, 0.12),
            ),
        ],
        Jaw => vec![part([0.50, 0.13, 0.35], [0.0, 0.69, -0.60], FUR)],
        Tail => vec![part([0.18, 0.35, 0.20], [0.0, 0.67, 0.69], FUR)],
        ChainLeft | ChainMiddle | ChainRight => {
            let x = match segment {
                ChainLeft => -0.35,
                ChainRight => 0.35,
                _ => 0.0,
            };
            vec![part([0.07, 0.30, 0.07], [x, 0.74, -0.50], IRON)]
        }
        _ => {
            let (leg, lower) = limb(segment).expect("limb segment");
            let foot = rest_foot(leg);
            if lower {
                vec![
                    part([0.30, 0.36, 0.30], [foot.x, 0.41, foot.z], FUR),
                    // Reserve the underside occupied by the existing claw. The old
                    // overlapping cuboids put fur and bone on the same sole plane;
                    // rolling the corpse exposed depth fighting on that plane.
                    part([0.34, 0.24, 0.36], [foot.x, 0.18, foot.z], FUR),
                    part([0.34, 0.06, 0.29], [foot.x, 0.03, foot.z + 0.035], FUR),
                    part(
                        [0.03, 0.06, 0.07],
                        [foot.x - 0.155, 0.03, foot.z - 0.145],
                        FUR,
                    ),
                    part(
                        [0.03, 0.06, 0.07],
                        [foot.x + 0.155, 0.03, foot.z - 0.145],
                        FUR,
                    ),
                    part([0.28, 0.06, 0.08], [foot.x, 0.03, foot.z - 0.15], BONE),
                ]
            } else {
                vec![part([0.30, 0.36, 0.30], [foot.x, 0.75, foot.z], FUR)]
            }
        }
    }
}

pub(super) fn meshes() -> Vec<(Segment, Mesh)> {
    SEGMENTS
        .into_iter()
        .map(|s| (s, boxes(&geometry(s))))
        .collect()
}

pub(super) fn visuals(
    meshes_out: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) -> SpeciesVisuals {
    let material = materials.add(StandardMaterial::from_color(Color::WHITE));
    let parts: Vec<_> = meshes()
        .into_iter()
        .map(|(s, m)| (s, meshes_out.add(m)))
        .collect();
    SpeciesVisuals {
        body: parts[0].1.clone(),
        head: parts[3].1.clone(),
        legs: None,
        arms: None,
        eyes: None,
        body_material: material.clone(),
        head_material: material,
        king_parts: None,
        guardian_parts: Some(parts),
    }
}

fn limb(segment: Segment) -> Option<(usize, bool)> {
    use Segment::*;
    Some(match segment {
        UpperFrontLeft => (0, false),
        LowerFrontLeft => (0, true),
        UpperFrontRight => (1, false),
        LowerFrontRight => (1, true),
        UpperRearLeft => (2, false),
        LowerRearLeft => (2, true),
        UpperRearRight => (3, false),
        LowerRearRight => (3, true),
        _ => return None,
    })
}

fn rest_foot(leg: usize) -> Vec3 {
    Vec3::new(
        if leg.is_multiple_of(2) { -0.58 } else { 0.58 },
        0.0,
        if leg < 2 { -0.34 } else { 0.39 },
    )
}
fn around(pivot: Vec3, rotation: Quat) -> Mat4 {
    Mat4::from_translation(pivot) * Mat4::from_quat(rotation) * Mat4::from_translation(-pivot)
}

#[derive(Debug, Clone, Copy)]
struct Foot {
    contact: Vec3,
    from: Vec3,
    to: Vec3,
    swing: f32,
}

/// A contact is presentation history, discarded on a discontinuity. It is never
/// terrain collision, locomotion intent or a replacement for snapshot position.
#[derive(Debug)]
pub(super) struct Motion {
    last: Vec3,
    yaw: f32,
    feet: [Foot; 4],
    next_pair: usize,
    audio_serial: u64,
    audio_contact: Option<Vec3>,
    pub(super) fast_travel: bool,
    pub(super) stage: u8,
    pub(super) transforms: [Transform; 19],
}
impl Motion {
    pub(super) fn new(position: Vec3, yaw: f32) -> Self {
        let rotation = Quat::from_rotation_y(yaw);
        Self {
            last: position,
            yaw,
            feet: std::array::from_fn(|i| {
                let contact = position + rotation * rest_foot(i);
                Foot {
                    contact,
                    from: contact,
                    to: contact,
                    swing: 1.0,
                }
            }),
            next_pair: 0,
            audio_serial: 0,
            audio_contact: None,
            fast_travel: false,
            stage: 1,
            transforms: [Transform::IDENTITY; 19],
        }
    }

    pub(super) fn sample(
        &mut self,
        position: Vec3,
        yaw: f32,
        action: MobAction,
        elapsed: Duration,
        down: f32,
        delta: Duration,
    ) {
        self.audio_contact = None;
        let displacement = position - self.last;
        let turn = (yaw - self.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
            - std::f32::consts::PI;
        // A streaming/teleport correction is not a huge stride. Vertical movement
        // follows the server (including vertical corrections), not a locally animated jump.
        if displacement.length() > 0.75
            || turn.abs() > 0.8
            || displacement.y.abs() > 0.04
            || down > 0.0
        {
            let serial = self.audio_serial;
            let travel = self.fast_travel;
            let stage = self.stage;
            *self = Self::new(position, yaw);
            self.audio_serial = serial;
            self.fast_travel = travel;
            self.stage = stage;
        }
        let rotation = Quat::from_rotation_y(yaw);
        let dt = delta.as_secs_f32().min(0.05);
        let speed =
            (displacement.xz().length() + turn.abs() * 0.7) / delta.as_secs_f32().max(0.001);
        let swing_seconds = (0.16 / (1.0 + speed / 1.5)).clamp(0.025, 0.16);
        let moving = displacement.xz().length() > 0.0001 || turn.abs() > 0.0001;
        let gait_allowed = matches!(action, MobAction::Idle | MobAction::Chase) || self.fast_travel;
        let mut targets = [Vec3::ZERO; 4];
        let swinging = self.feet.iter().any(|foot| foot.swing < 1.0);
        if !swinging && down == 0.0 && gait_allowed {
            let pair = if self.next_pair == 0 { [0, 3] } else { [1, 2] };
            let error = pair
                .iter()
                .map(|&i| {
                    self.feet[i]
                        .contact
                        .distance(position + rotation * rest_foot(i))
                })
                .fold(0.0, f32::max);
            if error > if moving { 0.12 } else { 0.025 } {
                let travel =
                    Vec3::new(displacement.x, 0.0, displacement.z).clamp_length_max(0.08) * 2.5;
                for i in pair {
                    let foot = &mut self.feet[i];
                    foot.from = foot.contact;
                    foot.to = position + rotation * rest_foot(i) + travel;
                    foot.to.y = position.y;
                    foot.swing = 0.0;
                }
                self.next_pair ^= 1;
            }
        }
        let mut landed = Vec3::ZERO;
        let mut landings = 0;
        for (i, foot) in self.feet.iter_mut().enumerate() {
            let mut contact_finished = false;
            if !gait_allowed || down > 0.0 {
                foot.contact = position + rotation * rest_foot(i);
                foot.swing = 1.0;
            } else if foot.swing < 1.0 {
                foot.swing = (foot.swing + dt / swing_seconds).min(1.0);
                contact_finished = foot.swing == 1.0;
                let t = foot.swing * foot.swing * (3.0 - 2.0 * foot.swing);
                foot.contact = foot.from.lerp(foot.to, t)
                    + Vec3::Y * (std::f32::consts::PI * foot.swing).sin() * 0.12;
            }
            targets[i] = rotation.inverse() * (foot.contact - position);
            // The visual legs have a finite reach even if a render hitch skips a
            // whole support exchange. Replant instead of stretching geometry.
            if targets[i].distance(rest_foot(i)) > if self.fast_travel { 0.58 } else { 0.48 } {
                contact_finished = false;
                targets[i] = rest_foot(i);
                foot.contact = position + rotation * targets[i];
                foot.swing = 1.0;
            }
            if contact_finished {
                landed += foot.contact;
                landings += 1;
            }
        }
        if landings > 0 {
            self.audio_serial = self.audio_serial.wrapping_add(1);
            self.audio_contact = Some(landed / landings as f32);
        }
        self.last = position;
        self.yaw = yaw;
        self.transforms = pose(targets, action, elapsed, down);
    }
}

/// Solve the two rigid bones between a lowered hip and a planted paw. The paw
/// rocks onto its lowest sole edge; iterating its small vertical lift closes the
/// chain again instead of stretching a bone or detaching the knee.
fn leg_matrices(leg: usize, target: Vec3, drop: f32) -> (Mat4, Mat4) {
    let foot = rest_foot(leg);
    if target.abs_diff_eq(foot, 0.000001) && drop == 0.0 {
        return (Mat4::IDENTITY, Mat4::IDENTITY);
    }
    let rest_hip = foot + Vec3::Y * 0.90;
    let hip = rest_hip - Vec3::Y * drop;
    let rest_knee = foot + Vec3::Y * 0.57;
    let mut lift = 0.0;
    let mut lower = Mat4::IDENTITY;
    for _ in 0..12 {
        let planted = target + Vec3::Y * lift;
        let delta = planted - hip;
        let distance = delta.length().clamp(0.241, 0.89999);
        let direction = delta.normalize_or_zero();
        let along = (0.33_f32.powi(2) - 0.57_f32.powi(2) + distance.powi(2)) / (2.0 * distance);
        let bend = (Vec3::Z - direction * direction.z).normalize_or_zero();
        let knee =
            hip + direction * along + bend * (0.33_f32.powi(2) - along.powi(2)).max(0.0).sqrt();
        let rotation = Quat::from_rotation_arc(Vec3::Y, (knee - planted).normalize_or_zero());
        lift = -[-0.17, 0.17]
            .into_iter()
            .flat_map(|x| {
                [-0.19, 0.18]
                    .into_iter()
                    .map(move |z| (rotation * Vec3::new(x, 0.0, z)).y)
            })
            .fold(f32::INFINITY, f32::min);
        lower = Mat4::from_translation(target - foot + Vec3::Y * lift) * around(foot, rotation);
    }
    let knee = lower.transform_point3(rest_knee);
    let upper = Mat4::from_translation(-Vec3::Y * drop)
        * around(
            rest_hip,
            Quat::from_rotation_arc(-Vec3::Y, (knee - hip).normalize_or_zero()),
        );
    (upper, lower)
}

pub(super) fn pose(
    feet: [Vec3; 4],
    action: MobAction,
    elapsed: Duration,
    down: f32,
) -> [Transform; 19] {
    let breathing = if down == 0.0 {
        (elapsed.as_secs_f32() * 2.2).sin() * 0.008
    } else {
        0.0
    };
    let drop = feet
        .iter()
        .enumerate()
        .map(|(i, p)| p.distance(rest_foot(i)))
        .fold(0.0, f32::max)
        .min(0.30)
        * 0.4;
    let lowered = Mat4::from_translation(-Vec3::Y * drop);
    let torso = lowered * around(Vec3::new(0.0, 0.95, 0.2), Quat::from_rotation_x(breathing));
    let neck = torso
        * around(
            Vec3::new(0.0, 1.0, -0.28),
            Quat::from_rotation_x(-breathing),
        );
    let head = neck;
    let legs: [_; 4] = std::array::from_fn(|i| leg_matrices(i, feet[i], drop));
    let fall = around(Vec3::new(0.0, 0.9, 0.0), Quat::from_rotation_z(1.50 * down));
    let mut matrices = SEGMENTS.map(|segment| {
        use Segment::*;
        let local = match segment {
            Pelvis => lowered,
            Thorax => torso,
            Neck | CollarLeft | CollarRight => neck,
            Head => head,
            Jaw => {
                head * around(
                    Vec3::new(0.0, 0.73, -0.43),
                    Quat::from_rotation_x(2.0 * breathing),
                )
            }
            Tail => {
                lowered
                    * around(
                        Vec3::new(0.0, 0.84, 0.64),
                        Quat::from_rotation_y(if down == 0.0 {
                            (elapsed.as_secs_f32() * 1.7).sin() * 0.10
                        } else {
                            0.20 * down
                        }),
                    )
            }
            ChainLeft | ChainMiddle | ChainRight => {
                neck * around(
                    Vec3::new(0.0, 0.89, -0.5),
                    Quat::from_rotation_x(if down == 0.0 {
                        breathing * 4.0
                    } else {
                        -0.25 * down
                    }),
                )
            }
            _ => {
                let (index, lower) = limb(segment).expect("limb");
                let joint = if lower { legs[index].1 } else { legs[index].0 };
                joint
                    * around(
                        rest_foot(index) + Vec3::Y * 0.57,
                        Quat::from_rotation_x(if lower { 0.45 * down } else { -0.25 * down }),
                    )
            }
        };
        fall * local
    });
    // The whole rest box bounds the live pose; on death, tuck then ground the
    // actual segment bounds. This is visual floor contact, not world collision.
    if down > 0.0 {
        let lowest = SEGMENTS
            .iter()
            .zip(&matrices)
            .map(|(&s, m)| {
                bounds(s)
                    .into_iter()
                    .map(|p| m.transform_point3(p).y)
                    .fold(f32::INFINITY, f32::min)
            })
            .fold(f32::INFINITY, f32::min);
        for matrix in &mut matrices {
            *matrix = Mat4::from_translation(-Vec3::Y * lowest) * *matrix;
        }
    }
    let _ = action; // The encounter pass supplies attacks after timeline reconciliation.
    matrices.map(Transform::from_matrix)
}

/// Conservative per-segment bounds, computed without allocating geometry in a frame.
fn bounds(segment: Segment) -> [Vec3; 8] {
    use Segment::*;
    let (lo, hi) = match segment {
        Pelvis => (Vec3::new(-0.43, 0.61, 0.13), Vec3::new(0.43, 1.09, 0.63)),
        Thorax => (Vec3::new(-0.615, 0.78, -0.30), Vec3::new(0.60, 1.8, 0.27)),
        Neck => (Vec3::new(-0.60, 0.78, -0.43), Vec3::new(0.60, 1.58, -0.26)),
        CollarLeft | CollarRight => {
            let x = if segment == CollarLeft {
                -0.1875
            } else {
                0.1875
            };
            (
                Vec3::new(x - 0.1875, 0.9, -0.54),
                Vec3::new(x + 0.1875, 1.1, 0.04),
            )
        }
        Head => (
            Vec3::new(-0.31, 0.655, -0.795),
            Vec3::new(0.31, 1.335, -0.36),
        ),
        Jaw => (
            Vec3::new(-0.25, 0.625, -0.775),
            Vec3::new(0.25, 0.755, -0.425),
        ),
        Tail => (Vec3::new(-0.09, 0.495, 0.59), Vec3::new(0.09, 0.845, 0.79)),
        ChainLeft | ChainMiddle | ChainRight => {
            let x = match segment {
                ChainLeft => -0.35,
                ChainRight => 0.35,
                _ => 0.0,
            };
            (
                Vec3::new(x - 0.035, 0.59, -0.535),
                Vec3::new(x + 0.035, 0.89, -0.465),
            )
        }
        _ => {
            let (i, lower) = limb(segment).expect("limb");
            let p = rest_foot(i);
            if lower {
                (
                    p + Vec3::new(-0.17, 0.0, -0.19),
                    p + Vec3::new(0.17, 0.59, 0.18),
                )
            } else {
                (
                    p + Vec3::new(-0.15, 0.57, -0.15),
                    p + Vec3::new(0.15, 0.93, 0.15),
                )
            }
        }
    };
    std::array::from_fn(|i| {
        Vec3::new(
            if i & 1 == 0 { lo.x } else { hi.x },
            if i & 2 == 0 { lo.y } else { hi.y },
            if i & 4 == 0 { lo.z } else { hi.z },
        )
    })
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(super) fn posed_meshes(action: MobAction, elapsed: Duration, down: f32) -> Vec<Mesh> {
    meshes()
        .into_iter()
        .zip(pose(std::array::from_fn(rest_foot), action, elapsed, down))
        .map(|((_, mesh), transform)| mesh.transformed_by(transform))
        .collect()
}

#[cfg(test)]
mod capture;

#[cfg(test)]
mod system_tests;

// This accessor lives with the Vargr rig so other boss modules need no audio coupling.
// A stamp describes this render frame's completed cosmetic support exchange, not a
// collision or an authoritative hit. Missing geometry simply means no gait sound.
impl Mob {
    pub(in crate::player) fn guardian_audio_contact(&self) -> Option<(u64, u64, Vec3)> {
        let motion = self.guardian_motion.as_ref()?;
        (self.kind == MobKind::VargrGuardian).then_some(())?;
        Some((self.entity_id, motion.audio_serial, motion.audio_contact?))
    }
}

#[cfg(test)]
mod audio_tests;
