//! Server-window choreography of rigid, authored pieces. This module never writes
//! a root, advances a phase, chooses a target, or decides whether contact damages.
use super::*;
use crate::net::{EncounterMoveKind, MovePhase};
use crate::player::encounters::{PresentedMove, Window};

/// Read the current inbox against the newest snapshot, even before reconciliation
/// runs later in this frame. Windup alone does not distinguish preparation/travel.
pub(in super::super) fn travelling(
    inbox: &crate::net::EncounterTimelineInbox,
    snapshots: &SnapshotBuffer,
    boss: u64,
) -> bool {
    snapshots.latest_snapshot().is_some_and(|snapshot| {
        inbox
            .live()
            .iter()
            .filter(|state| state.boss_entity_id == boss)
            .flat_map(|state| &state.moves)
            .any(|one| {
                one.kind == EncounterMoveKind::CollarCharge
                    && one.phase == MovePhase::Release
                    && one.ended.is_none()
                    && snapshot.server_tick.wrapping_sub(one.phase_started_tick) < one.phase_ticks
            })
    })
}

#[derive(Clone, Copy)]
struct Pose {
    drop: f32,
    pitch: f32,
    roll: f32,
    twist: f32,
    neck: f32,
    head: f32,
    jaw: f32,
    lift: f32,
    feet: [Vec3; 4],
}
impl Default for Pose {
    fn default() -> Self {
        Self {
            drop: 0.10,
            pitch: 0.0,
            roll: 0.0,
            twist: 0.0,
            neck: 0.0,
            head: 0.0,
            jaw: 0.0,
            lift: 0.0,
            feet: std::array::from_fn(rest_foot),
        }
    }
}
fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Sample the *announced* interval inclusively so the last leap release tick has
/// landed. A one-tick phase starts at its endpoint. Local render time is irrelevant.
fn progress(one: &PresentedMove) -> f32 {
    let ticks = one.announced.phase_ticks;
    if ticks <= 1 {
        1.0
    } else {
        ticks.saturating_sub(one.remaining_ticks) as f32 / (ticks - 1) as f32
    }
    .clamp(0.0, 1.0)
}

pub(in super::super) fn sample(
    motion: &Motion,
    one: Option<&PresentedMove>,
    stage: u8,
    yaw: f32,
) -> [Transform; 19] {
    let one = one.filter(|one| one.window == Window::Current && one.announced.ended.is_none());
    let Some(one) = one else {
        // Empty/expired/replaced timelines have no retained attack state. Idle
        // phase two still opens the fastening, including first sight after transition.
        return if stage >= 2 {
            let rotation = Quat::from_rotation_y(-yaw);
            let p = Pose {
                drop: 0.17,
                pitch: 0.04,
                head: -0.06,
                jaw: 0.12,
                feet: motion
                    .feet
                    .map(|foot| rotation * (foot.contact - motion.last)),
                ..Default::default()
            };
            stage_pose(assemble(p), stage)
        } else {
            motion.transforms
        };
    };
    let mut pose = controls(one);
    // The locked aim is relative to the interpolated snapshot facing, not the
    // player. Bound the mesh correction while the snapshot catches up; no root yaw.
    let aim = Vec3::from_array(one.announced.aim.unwrap_or([0.0; 3]));
    if aim.xz().length_squared() > 0.001 {
        let local = Quat::from_rotation_y(-yaw) * aim;
        pose.twist += (-local.x).atan2(-local.z).clamp(-0.12, 0.12);
    }
    if one.announced.kind == EncounterMoveKind::CollarCharge
        && one.announced.phase == MovePhase::Release
    {
        let rotation = Quat::from_rotation_y(-yaw);
        pose.feet = motion
            .feet
            .map(|foot| rotation * (foot.contact - motion.last));
        // Sufficient lowered-hip reach for the fast authoritative displacement.
        pose.drop = 0.24;
    }
    stage_pose(assemble(pose), stage)
}

fn controls(one: &PresentedMove) -> Pose {
    use EncounterMoveKind::*;
    use MovePhase::*;
    let t = progress(one);
    let load = smooth(t);
    let mut p = Pose::default();
    match one.announced.kind {
        BiteAndTear => {
            let second = one.announced.combo.is_some_and(|(step, _)| step == 2);
            let side = if second { -1.0 } else { 1.0 };
            match one.announced.phase {
                Telegraph => {
                    p.drop = 0.13 + 0.06 * load;
                    p.pitch = -0.04 - 0.05 * load;
                    p.twist = side * (0.025 + 0.06 * load);
                    p.head = -0.14;
                    p.jaw = 0.28 + 0.65 * load;
                }
                Release => {
                    p.pitch = 0.12;
                    p.neck = 0.045;
                    p.head = 0.08;
                    p.jaw = 0.10 * (1.0 - load);
                    p.twist = side * if second { 0.10 - 0.20 * load } else { -0.045 };
                }
                Recovery => {
                    p.drop = 0.19;
                    p.pitch = 0.12;
                    p.head = -0.18;
                    p.twist = -side * if second { 0.13 } else { 0.07 };
                    p.roll = side * 0.04;
                    p.jaw = 0.08;
                }
                Channel => {}
            }
        }
        PrisonerClaws => {
            // The server's explicit 1/2 and 2/2 select anatomical left then right.
            // A lone phase-one scratch uses the right paw, without instance parity.
            let left = one.announced.combo.is_some_and(|(step, _)| step == 1);
            let leg = if left { 0 } else { 1 };
            let side = if left { -1.0 } else { 1.0 };
            p.drop = 0.20;
            p.roll = -side * 0.07;
            p.head = -0.08;
            match one.announced.phase {
                Telegraph => {
                    p.twist = -side * 0.08;
                    p.feet[leg] += Vec3::new(side * 0.07, 0.13 + 0.13 * load, -0.06);
                    p.jaw = 0.18;
                }
                Release => {
                    p.twist = side * (0.06 - 0.12 * load);
                    // Rigid paw sweeps forward and inwards; pale claw underside is
                    // exposed by the lower-bone rotation, not new finger geometry.
                    p.feet[leg] +=
                        Vec3::new(-side * 0.25 * load, 0.22 * (1.0 - load) + 0.06, -0.30);
                    p.pitch = 0.055;
                }
                Recovery => {
                    p.twist = side * 0.10;
                    p.pitch = 0.07;
                    p.head = -0.16;
                }
                Channel => {}
            }
        }
        CollarCharge => match one.announced.phase {
            Telegraph => {
                p.drop = 0.19;
                p.pitch = -0.03;
                p.head = 0.12 * load;
                p.jaw = 0.16;
                // Exactly two complete scrapes, each returning to its support.
                for (leg, start) in [(1, 0.0), (0, 0.40)] {
                    let u = ((t - start) / 0.36).clamp(0.0, 1.0);
                    p.feet[leg] += Vec3::new(
                        0.0,
                        (u * std::f32::consts::PI).sin() * 0.17,
                        -(u * std::f32::consts::TAU).sin() * 0.16,
                    );
                }
                p.neck = 0.04 * load;
            }
            Release => {
                p.drop = 0.24;
                p.pitch = 0.06;
                p.head = 0.12;
            }
            Recovery => {
                // No impact-cause event exists. This heavy planted stop holds the
                // whole announced recovery, including a monolith-extended window.
                p.drop = 0.25;
                p.pitch = 0.14;
                p.head = -0.14;
                p.jaw = 0.20;
            }
            Channel => {}
        },
        PredatorLeap => match one.announced.phase {
            Telegraph => {
                p.drop = 0.15 + 0.16 * load;
                p.pitch = -0.04;
                p.head = 0.17;
                p.jaw = 0.13;
            }
            Release => {
                // The server currently supplies horizontal travel. Only the child
                // mesh rises, within a bounded arc; landing is the final release tick.
                let arc = (std::f32::consts::PI * t).sin().max(0.0);
                p.lift = 0.62 * arc;
                p.drop = 0.28;
                p.pitch = -0.08 + 0.21 * load;
                p.head = 0.13 - 0.28 * load;
                for foot in &mut p.feet {
                    foot.y = 0.24 * arc;
                }
            }
            Recovery => {
                p.drop = 0.28;
                p.pitch = 0.13;
                p.head = -0.15;
                p.jaw = 0.13;
            }
            Channel => {}
        },
        BonebreakerJaws => match one.announced.phase {
            Telegraph => {
                p.drop = 0.12;
                p.pitch = -0.10 * load;
                p.head = 0.15 + 0.16 * load;
                p.jaw = 0.52 + 0.70 * load;
            }
            Release => {
                p.drop = 0.18;
                p.pitch = 0.15;
                p.head = -0.08;
                p.jaw = 0.04;
            }
            Recovery => {
                p.drop = 0.23;
                p.pitch = 0.14;
                p.head = -0.20;
                p.jaw = 0.06;
            }
            Channel => {}
        },
        _ => {}
    }
    p
}

/// Every matrix transforms the original authored frame. Hip and knee endpoints
/// agree exactly across the two rigid bones; no scale or mesh deformation is used.
fn assemble(p: Pose) -> [Transform; 19] {
    let pelvis = Mat4::from_translation(-Vec3::Y * p.drop);
    let torso = pelvis
        * around(
            Vec3::new(0.0, 0.93, 0.20),
            Quat::from_euler(EulerRot::YXZ, p.twist, p.pitch, p.roll),
        );
    let neck = torso * around(Vec3::new(0.0, 1.15, -0.28), Quat::from_rotation_x(p.neck));
    let head = neck * around(Vec3::new(0.0, 0.97, -0.40), Quat::from_rotation_x(p.head));
    let legs: [_; 4] = std::array::from_fn(|i| {
        let hip =
            (if i < 2 { torso } else { pelvis }).transform_point3(rest_foot(i) + Vec3::Y * 0.90);
        solve_leg(i, hip, p.feet[i])
    });
    SEGMENTS.map(|segment| {
        use Segment::*;
        let local = match segment {
            Pelvis | Tail => pelvis,
            Thorax => torso,
            Neck | CollarLeft | CollarRight | ChainLeft | ChainMiddle | ChainRight => neck,
            Head => head,
            Jaw => head * around(Vec3::new(0.0, 0.73, -0.43), Quat::from_rotation_x(-p.jaw)),
            _ => {
                let (leg, lower) = limb(segment).expect("limb");
                if lower { legs[leg].1 } else { legs[leg].0 }
            }
        };
        Transform::from_matrix(Mat4::from_translation(Vec3::Y * p.lift) * local)
    })
}

fn solve_leg(leg: usize, hip: Vec3, target: Vec3) -> (Mat4, Mat4) {
    let foot = rest_foot(leg);
    let rest_hip = foot + Vec3::Y * 0.90;
    let knee_rest = foot + Vec3::Y * 0.57;
    let mut lift = 0.0;
    let mut lower = Mat4::IDENTITY;
    for _ in 0..16 {
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
    let knee = lower.transform_point3(knee_rest);
    let upper = Mat4::from_translation(hip - rest_hip)
        * around(
            rest_hip,
            Quat::from_rotation_arc(-Vec3::Y, (knee - hip).normalize_or_zero()),
        );
    (upper, lower)
}

fn stage_pose(mut pose: [Transform; 19], stage: u8) -> [Transform; 19] {
    if stage >= 2 {
        for (segment, side) in [(Segment::CollarLeft, -1.0), (Segment::CollarRight, 1.0)] {
            let index = segment as usize;
            pose[index] = Transform::from_matrix(
                pose[index].to_matrix()
                    * around(
                        Vec3::new(side * 0.375, 1.0, 0.02),
                        Quat::from_rotation_y(-side * 0.48),
                    ),
            );
        }
        // The accepted chain geometry remains attached to the opened halves.
        // The middle fragment belongs to the left half of the broken fastening.
        pose[Segment::ChainLeft as usize] = pose[Segment::CollarLeft as usize];
        pose[Segment::ChainMiddle as usize] = pose[Segment::CollarLeft as usize];
        pose[Segment::ChainRight as usize] = pose[Segment::CollarRight as usize];
    }
    pose
}

pub(in super::super) fn corpse_pose(base: [Transform; 19], stage: u8) -> [Transform; 19] {
    let mut pose = stage_pose(base, stage);
    let lowest = SEGMENTS
        .iter()
        .enumerate()
        .flat_map(|(i, &segment)| bounds(segment).map(|p| pose[i].transform_point(p).y))
        .fold(0.0, f32::min);
    // Opening the fastening changes the collapsed silhouette. Reground its full
    // bounds locally so neither collar nor chain is driven through the floor.
    for part in &mut pose {
        part.translation.y -= lowest;
    }
    pose
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod capture;
