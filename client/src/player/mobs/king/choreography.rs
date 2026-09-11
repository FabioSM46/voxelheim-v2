//! Distinct physical and spell poses sampled from server-owned windows. No root
//! motion, local combo history, inferred damage or animation-triggered release.
use super::*;
use crate::net::{EncounterMoveKind, MovePhase};
use crate::player::encounters::{PresentedMove, Window};

#[derive(Clone, Copy)]
pub(super) struct Controls {
    pub drop: f32,
    pub pitch: f32,
    pub twist: f32,
    pub head: f32,
    pub grip: Vec3,
    pub blade: Quat,
    pub two_hands: bool,
    pub free_hand: Vec3,
    /// How far the final-stage crown has slipped, from 0 (welded) to 1.
    pub crown: f32,
}

impl Default for Controls {
    fn default() -> Self {
        Self {
            drop: 0.0,
            pitch: 0.0,
            twist: 0.0,
            head: 0.0,
            grip: Vec3::new(0.42, 1.39, -0.115),
            blade: Quat::IDENTITY,
            two_hands: false,
            free_hand: Vec3::new(-0.39, 1.39, -0.02),
            crown: 0.0,
        }
    }
}

pub(super) fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

// Include both endpoints of the announced interval. A single-tick release shows
// its contact pose immediately, rather than waiting for an unannounced next tick.
fn progress(one: &PresentedMove) -> f32 {
    let ticks = one.announced.phase_ticks;
    if ticks <= 1 {
        1.0
    } else {
        ticks.saturating_sub(one.remaining_ticks) as f32 / (ticks - 1) as f32
    }
    .clamp(0.0, 1.0)
}

fn planted() -> Controls {
    Controls {
        drop: 0.37,
        grip: Vec3::new(0.08, 1.51, -0.36),
        two_hands: true,
        head: 0.14,
        ..default()
    }
}

pub(super) fn entrance(t: f32) -> Controls {
    let stand = smooth(t);
    let low = planted();
    Controls {
        drop: 0.45 * (1.0 - stand),
        grip: low.grip.lerp(Controls::default().grip, stand) + Vec3::Y * 0.08 * (1.0 - stand),
        two_hands: stand < 0.85,
        head: 0.15 * (1.0 - stand),
        ..default()
    }
}

pub(super) fn controls(one: &PresentedMove) -> Controls {
    use EncounterMoveKind::*;
    use MovePhase::*;
    let t = progress(one);
    let u = smooth(t);
    let mut p = Controls::default();
    match one.announced.kind {
        KingsSentence => {
            p.two_hands = true;
            let high = Vec3::new(0.08, 2.28, -0.40);
            let low = planted();
            match one.announced.phase {
                Telegraph => {
                    // Raise early, then hold overhead: the pause is part of the signal.
                    let load = smooth((t / 0.65).min(1.0));
                    p.grip = Vec3::new(0.08, 1.85, -0.40).lerp(high, load);
                    p.blade = Quat::from_rotation_x(2.0 + (std::f32::consts::PI - 2.0) * load);
                    p.head = -0.10;
                }
                Release => {
                    p.grip = high.lerp(low.grip, u);
                    p.blade = Quat::from_rotation_x(std::f32::consts::PI * (1.0 - u));
                    p.drop = low.drop * u;
                    p.head = 0.14 * u;
                }
                Recovery => {
                    // The blade remains planted through the earned opening, then is
                    // visibly pulled free without starting another attack.
                    let recover = smooth(((t - 0.55) / 0.45).max(0.0));
                    p = low;
                    p.drop *= 1.0 - recover;
                    p.grip = p.grip.lerp(Vec3::new(0.08, 1.70, -0.40), recover);
                }
                // The server never channels a physical Sentence. Keep the
                // neutral one-handed rest if an unexpected phase is presented.
                Channel => p.two_hands = false,
            }
        }
        ThreeTolls => {
            let step = one.announced.combo.map_or(1, |(step, _)| step);
            p.two_hands = true;
            p.grip = Vec3::new(0.08, 1.94, -0.42);
            if step < 3 {
                let side = if step == 1 { 1.0 } else { -1.0 };
                let angle = match one.announced.phase {
                    Telegraph => side * (0.35 + 0.50 * u),
                    Release => side * (0.85 - 1.55 * u),
                    Recovery => -side * 0.70 * (1.0 - u),
                    Channel => 0.0,
                };
                p.blade = Quat::from_rotation_y(angle)
                    * Quat::from_rotation_x(std::f32::consts::FRAC_PI_2);
                p.twist = side
                    * match one.announced.phase {
                        Telegraph => -0.10 * u,
                        Release => -0.10 + 0.18 * u,
                        _ => 0.08 * (1.0 - u),
                    };
                p.drop = 0.10;
            } else {
                // Third toll is a forward thrust, not another horizontal cut.
                let extend = match one.announced.phase {
                    Telegraph => 0.10,
                    Release => smooth((t * 1.6).min(1.0)),
                    Recovery => 1.0 - u,
                    Channel => 0.0,
                };
                p.grip = Vec3::new(0.08, 1.91, -0.24 - 0.36 * extend);
                p.blade = Quat::from_rotation_x(std::f32::consts::FRAC_PI_2);
                p.drop = 0.08 + 0.08 * extend;
                p.head = 0.04;
            }
        }
        Burial => {
            p = planted();
            match one.announced.phase {
                Telegraph => {
                    p.drop *= u;
                    p.grip.y += 0.38 * (1.0 - u);
                    p.blade = Quat::from_rotation_x(0.4 * (1.0 - u));
                }
                Channel => {
                    // Every pulse braces both hands on the planted weapon.
                    p.head = 0.12 + 0.10 * u;
                }
                Recovery => {
                    p.head = 0.24 * (1.0 - u);
                }
                Release => {}
            }
        }
        EdictOfTheGraves => {
            // Point across the graves with the free arm; the sword stays low.
            let point = match one.announced.phase {
                Telegraph => 0.45 + 0.55 * u,
                Channel | Release => 1.0,
                Recovery => 1.0 - u,
            };
            let pulse = one
                .announced
                .pulse
                .map_or(0.0, |(index, _)| f32::from(index % 2) * 0.15);
            p.free_hand = Vec3::new(
                -0.39 - 0.27 * point,
                1.39 + 0.67 * point,
                -0.02 - (0.55 - pulse) * point,
            );
            p.head = -0.08 * point;
        }
        SepulchreSpear => {
            // Raised crystal, straight throwing arm, then an open exposed chest.
            p.free_hand = match one.announced.phase {
                Telegraph => Vec3::new(-0.42, 2.05 + 0.45 * u, -0.32),
                Release => Vec3::new(-0.38, 2.32 - 0.35 * u, -0.32 - 0.47 * u),
                Recovery => Vec3::new(-0.60, 1.97 - 0.38 * u, -0.68 + 0.50 * u),
                Channel => Controls::default().free_hand,
            };
            p.head = -0.12;
        }
        RequiemOfTheBuried => {
            p = planted();
            p.two_hands = false;
            let chant = match one.announced.phase {
                Telegraph => 0.4 + 0.6 * u,
                Channel => 1.0,
                Recovery => 1.0 - u,
                Release => 0.0,
            };
            p.free_hand = Vec3::new(-0.30, 1.75 + 0.45 * chant, -0.47);
            p.head = -0.18 * chant;
            // A visible gesture at the current server pulse; no locally counted notes.
            if one.damaging() {
                p.free_hand.x -= 0.15;
                p.head = 0.04;
            }
        }
        BiteAndTear | CollarCharge | PredatorLeap | PrisonerClaws | BonebreakerJaws => {}
    }
    p
}

/// Arm IK retains rigid segment lengths. The hand target is bounded to that reach;
/// the blade follows the achieved weapon-hand position, never a detached target.
fn arm(left: bool, target: Vec3) -> (Mat4, Mat4, Vec3) {
    let x = if left { -0.39 } else { 0.39 };
    let shoulder = Vec3::new(x, 2.25, 0.0);
    let elbow_rest = Vec3::new(x, 1.80, 0.0);
    let hand_rest = Vec3::new(x, 1.39, -0.02);
    let upper = 0.45_f32;
    let lower = elbow_rest.distance(hand_rest);
    let delta = target - shoulder;
    let distance = delta.length().clamp(0.08, upper + lower - 0.00001);
    let direction = delta.normalize_or_zero();
    let hand = shoulder + direction * distance;
    let along = (upper * upper - lower * lower + distance * distance) / (2.0 * distance);
    let outward = if left { -Vec3::X } else { Vec3::X };
    let bend = (outward - direction * outward.dot(direction)).normalize_or_zero();
    let elbow =
        shoulder + direction * along + bend * (upper * upper - along * along).max(0.0).sqrt();
    let upper_rotation = Quat::from_rotation_arc(-Vec3::Y, (elbow - shoulder).normalize_or_zero());
    let fore_rotation = Quat::from_rotation_arc(
        (hand_rest - elbow_rest).normalize(),
        (hand - elbow).normalize_or_zero(),
    );
    (
        around(shoulder, upper_rotation),
        Mat4::from_translation(elbow - elbow_rest) * around(elbow_rest, fore_rotation),
        hand,
    )
}

pub(super) fn assemble(p: Controls, feet: [Vec3; 2]) -> [Transform; 17] {
    let torso = Mat4::from_translation(-Vec3::Y * p.drop)
        * around(
            Vec3::Y * 1.25,
            Quat::from_rotation_y(p.twist) * Quat::from_rotation_x(p.pitch),
        );
    let rest_grip = Vec3::new(0.42, 1.39, -0.115);
    let hand_offset = p.blade * Vec3::new(-0.03, 0.0, 0.095);
    let (right_upper, right_fore, right_hand) = arm(false, p.grip + hand_offset);
    let grip = right_hand - hand_offset;
    let blade = Mat4::from_translation(grip - rest_grip) * around(rest_grip, p.blade);
    let left_target = if p.two_hands {
        grip + p.blade * Vec3::new(-0.03, 0.10, 0.095)
    } else {
        p.free_hand
    };
    let (left_upper, left_fore, _) = arm(true, left_target);
    let legs = std::array::from_fn::<_, 2, _>(|i| super::motion::leg_matrices(i, feet[i], p.drop));
    let head = torso * around(Vec3::Y * 2.30, Quat::from_rotation_x(p.head));
    SEGMENTS.map(|segment| {
        use Segment::*;
        Transform::from_matrix(match segment {
            Pelvis => Mat4::from_translation(-Vec3::Y * p.drop),
            Torso => torso,
            // A worn mask is rigid on the face; a fallen one is placed by the regalia.
            Head | Mask => head,
            Crown => head * super::regalia::crown_tilt(p.crown),
            UpperLeft => torso * left_upper,
            UpperRight => torso * right_upper,
            ForeLeft => torso * left_fore,
            ForeRight => torso * right_fore,
            Blade => torso * blade,
            ThighLeft => legs[0].0,
            ShinLeft => legs[0].1,
            ThighRight => legs[1].0,
            ShinRight => legs[1].1,
            BootLeft => legs[0].2,
            BootRight => legs[1].2,
            Cloak => torso,
        })
    })
}

/// A wrist articulation of the blade about the achieved grip, in radians: `yaw` swings the
/// blade's direction about the vertical, `raise` lifts its tip (negative lowers it).
///
/// Both are weighted by how horizontal the authored blade is, so a blade held upright overhead
/// or planted straight down is unchanged, and a pose that is already clear of terrain uses
/// [`Wrist::AUTHORED`] and is bit-for-bit the authored one. The grip, timing, aim twist and
/// every other joint target stay authored; only where the blade points past the hands changes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in super::super) struct Wrist {
    pub yaw: f32,
    pub raise: f32,
}

impl Wrist {
    pub(in super::super) const AUTHORED: Self = Self {
        yaw: 0.0,
        raise: 0.0,
    };
}

const fn wrist(yaw: f32, raise: f32) -> Wrist {
    Wrist { yaw, raise }
}

/// The variants a planted blow may take beside terrain, in preference order: the authored
/// stroke, then each articulation from the smallest departure to the largest.
pub(super) const WRISTS: [Wrist; 13] = [
    Wrist::AUTHORED,
    wrist(0.0, 0.5),
    wrist(0.0, -0.5),
    wrist(0.5, 0.0),
    wrist(-0.5, 0.0),
    wrist(0.0, 0.9),
    wrist(0.0, -0.9),
    wrist(0.9, 0.0),
    wrist(-0.9, 0.0),
    wrist(0.0, 1.25),
    wrist(0.0, -1.25),
    wrist(1.25, 0.0),
    wrist(-1.25, 0.0),
];

fn articulate(blade: Quat, wrist: Wrist) -> Quat {
    if wrist == Wrist::AUTHORED {
        return blade;
    }
    let along = blade * -Vec3::Y;
    let level = along.xz().length();
    let lift = along.cross(Vec3::Y).normalize_or_zero();
    Quat::from_rotation_y(wrist.yaw * level)
        * Quat::from_axis_angle(lift, wrist.raise * level)
        * blade
}

/// A current, unended window: the only kind this module poses.
pub(super) fn live(one: Option<&PresentedMove>) -> Option<&PresentedMove> {
    one.filter(|one| one.window == Window::Current && one.announced.ended.is_none())
}

/// The planted blows whose blade reaches past the body: the Sentence and the three Tolls.
pub(super) fn planted_blow(one: &PresentedMove) -> bool {
    matches!(
        one.announced.kind,
        EncounterMoveKind::KingsSentence | EncounterMoveKind::ThreeTolls
    )
}

/// The authored controls for this window, with the crown and the locked aim applied.
fn authored(motion: &super::motion::Motion, one: &PresentedMove, yaw: f32) -> Controls {
    let mut p = controls(one);
    p.crown = motion.regalia.crown();
    if let Some(aim) = one.announced.aim {
        let local = Quat::from_rotation_y(-yaw) * Vec3::from_array(aim);
        p.twist += (-local.x).atan2(-local.z).clamp(-0.25, 0.25);
    }
    p
}

/// The pose these controls assemble into, with the blade articulated at the wrist.
pub(super) fn pose(
    motion: &super::motion::Motion,
    mut p: Controls,
    wrist: Wrist,
) -> [Transform; 17] {
    p.blade = articulate(p.blade, wrist);
    let mut pose = assemble(p, super::motion::REST_FEET);
    motion.regalia.dress(&mut pose);
    pose
}

/// Whether both hands still hold the weapon in a pose these controls assembled: the right palm
/// on the grip and, for a two-handed stroke, the left on the second grip. A wrist variant
/// that asks an arm past its rigid reach would let go, which is not a stroke at all.
pub(super) fn held(pose: &[Transform; 17], p: &Controls) -> bool {
    use Segment::*;
    let palm = Vec3::new(0.39, 1.39, -0.02);
    let right = pose[ForeRight as usize].transform_point(palm);
    let grip = pose[Blade as usize].transform_point(palm);
    let left = pose[ForeLeft as usize].transform_point(Vec3::new(-0.39, 1.39, -0.02));
    let second = pose[Blade as usize].transform_point(Vec3::new(0.39, 1.49, -0.02));
    right.distance(grip) < 0.002 && (!p.two_hands || left.distance(second) < 0.003)
}

/// How many samples each phase of a blow is judged at, both ends included.
pub(super) const PHASE_SAMPLES: u32 = 21;

/// The authored controls of every preparation, release and recovery sample of this blow,
/// whichever phase is being presented now: a variant is chosen for the whole blow, so no
/// phase boundary can switch strokes.
pub(super) fn frames(
    motion: &super::motion::Motion,
    one: &PresentedMove,
    yaw: f32,
) -> Vec<Controls> {
    [
        MovePhase::Telegraph,
        MovePhase::Release,
        MovePhase::Recovery,
    ]
    .into_iter()
    .flat_map(|phase| {
        (0..PHASE_SAMPLES).map(move |i| {
            let mut at = one.clone();
            at.announced.phase = phase;
            at.announced.phase_ticks = PHASE_SAMPLES;
            at.remaining_ticks = PHASE_SAMPLES - i;
            authored(motion, &at, yaw)
        })
    })
    .collect()
}

pub(in super::super) fn sample(
    motion: &super::motion::Motion,
    one: Option<&PresentedMove>,
    yaw: f32,
) -> [Transform; 17] {
    let Some(one) = live(one) else {
        return motion.transforms;
    };
    pose(motion, authored(motion, one, yaw), motion.wrist(one))
}
