//! Cosmetic contacts follow snapshot displacement. A planted boot stays in world
//! space until the other support takes over; teleports replant instead of stretching.
use super::*;

pub(super) const REST_FEET: [Vec3; 2] = [Vec3::new(-0.17, 0.0, 0.0), Vec3::new(0.17, 0.0, 0.0)];

#[derive(Debug, Clone, Copy)]
struct Foot {
    contact: Vec3,
    from: Vec3,
    to: Vec3,
    swing: f32,
}

#[derive(Debug)]
pub(in super::super) struct Motion {
    last: Vec3,
    yaw: f32,
    feet: [Foot; 2],
    next: usize,
    entrance: Option<Duration>,
    /// Mask, crown and core state. Survives a replant: a teleport is not a new fight.
    pub(in super::super) regalia: super::regalia::Regalia,
    pub(in super::super) transforms: [Transform; 17],
}

impl Motion {
    pub(in super::super) fn new(position: Vec3, yaw: f32, action: MobAction) -> Self {
        let rotation = Quat::from_rotation_y(yaw);
        Self {
            last: position,
            yaw,
            feet: REST_FEET.map(|foot| {
                let contact = position + rotation * foot;
                Foot {
                    contact,
                    from: contact,
                    to: contact,
                    swing: 1.0,
                }
            }),
            next: 0,
            entrance: (action == MobAction::Idle).then_some(Duration::ZERO),
            regalia: default(),
            transforms: [Transform::IDENTITY; 17],
        }
    }

    #[allow(clippy::too_many_arguments)] // Snapshot body, motion and lifecycle facts are independent inputs.
    pub(in super::super) fn sample(
        &mut self,
        position: Vec3,
        yaw: f32,
        action: MobAction,
        elapsed: Duration,
        down: f32,
        delta: Duration,
        in_encounter: bool,
    ) {
        let displacement = position - self.last;
        let turn = (yaw - self.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
            - std::f32::consts::PI;
        if displacement.length() > 1.5 || turn.abs() > 1.0 {
            // A discontinuous replacement cannot drag feet across the room or replay entry.
            let regalia = std::mem::take(&mut self.regalia);
            *self = Self::new(position, yaw, action);
            self.regalia = regalia;
            self.entrance = None;
        }
        if action != MobAction::Idle || in_encounter || down > 0.0 {
            self.entrance = None;
        }
        let rotation = Quat::from_rotation_y(yaw);
        let dt = delta.as_secs_f32().min(0.05);
        let moving = displacement.xz().length() > 0.0001 || turn.abs() > 0.0001;
        let gait =
            matches!(action, MobAction::Idle | MobAction::Chase | MobAction::Flee) && down == 0.0;
        if gait && self.feet.iter().all(|foot| foot.swing >= 1.0) {
            let i = self.next;
            let desired = position + rotation * REST_FEET[i];
            if self.feet[i].contact.distance(desired) > if moving { 0.13 } else { 0.035 } {
                let foot = &mut self.feet[i];
                foot.from = foot.contact;
                foot.to = desired
                    + Vec3::new(displacement.x, 0.0, displacement.z).clamp_length_max(0.08) * 2.0;
                foot.to.y = position.y;
                foot.swing = 0.0;
                self.next ^= 1;
            }
        }
        let speed =
            (displacement.xz().length() + turn.abs() * 0.25) / delta.as_secs_f32().max(0.001);
        let swing_time = (0.22 / (1.0 + speed)).clamp(0.035, 0.22);
        let mut feet = REST_FEET;
        for (i, foot) in self.feet.iter_mut().enumerate() {
            if !gait {
                foot.contact = position + rotation * REST_FEET[i];
                foot.swing = 1.0;
            } else if foot.swing < 1.0 {
                foot.swing = (foot.swing + dt / swing_time).min(1.0);
                foot.contact = foot
                    .from
                    .lerp(foot.to, super::choreography::smooth(foot.swing))
                    + Vec3::Y * (std::f32::consts::PI * foot.swing).sin() * 0.11;
            }
            feet[i] = rotation.inverse() * (foot.contact - position);
            if feet[i].distance(REST_FEET[i]) > 0.38 {
                feet[i] = REST_FEET[i];
                foot.contact = position + rotation * feet[i];
                foot.swing = 1.0;
            }
        }
        let drift = feet
            .iter()
            .zip(REST_FEET)
            .map(|(foot, rest)| foot.distance(rest))
            .fold(0.0, f32::max);
        let mut p = super::choreography::Controls {
            drop: drift.min(0.30) * 0.55,
            pitch: if down == 0.0 {
                (elapsed.as_secs_f32() * 1.8).sin() * 0.008
            } else {
                0.0
            },
            crown: self.regalia.crown(),
            ..default()
        };
        if let Some(entry) = self.entrance.as_mut() {
            *entry += delta;
            let progress = entry.as_secs_f32() / 1.3;
            if progress < 1.0 {
                p = super::choreography::Controls {
                    crown: self.regalia.crown(),
                    ..super::choreography::entrance(progress)
                };
            } else {
                self.entrance = None;
            }
        }
        self.transforms = super::choreography::assemble(p, feet);
        if down > 0.0 {
            // Fold at the hips and knees before settling on the floor. All fifteen
            // parts share the final grounding translation; no independent floating limbs.
            p = super::choreography::Controls {
                drop: 0.12 * (std::f32::consts::PI * down).sin(),
                grip: Vec3::new(0.42, 1.39, -0.115),
                head: 0.25 * down,
                crown: self.regalia.crown(),
                ..default()
            };
            let mut pose = super::choreography::assemble(p, REST_FEET);
            let fall = around(
                Vec3::Y * 1.05,
                Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2 * down),
            );
            for transform in &mut pose {
                *transform = Transform::from_matrix(fall * transform.to_matrix());
            }
            let minimum = SEGMENTS
                .iter()
                .zip(&pose)
                .flat_map(|(&segment, transform)| {
                    ground_points(segment)
                        .iter()
                        .map(|&point| transform.transform_point(point).y)
                })
                .fold(f32::INFINITY, f32::min);
            for transform in &mut pose {
                transform.translation.y -= minimum;
            }
            self.transforms = pose;
        }
        self.regalia.update(
            &mut self.transforms,
            position,
            yaw,
            delta.as_secs_f32().min(0.1),
        );
        self.last = position;
        self.yaw = yaw;
    }
}

pub(super) fn leg_matrices(index: usize, target: Vec3, drop: f32) -> (Mat4, Mat4, Mat4) {
    let foot = REST_FEET[index];
    let boot = Mat4::from_translation(target - foot);
    if target.abs_diff_eq(foot, 0.000001) && drop == 0.0 {
        return (Mat4::IDENTITY, Mat4::IDENTITY, boot);
    }
    let rest_hip = foot + Vec3::Y * 1.05;
    let hip = rest_hip - Vec3::Y * drop;
    let ankle_rest = foot + Vec3::Y * 0.17;
    let ankle = target + Vec3::Y * 0.17;
    let delta = ankle - hip;
    let distance = delta.length().clamp(0.12001, 0.87999);
    let direction = delta.normalize_or_zero();
    let along = (0.50_f32.powi(2) - 0.38_f32.powi(2) + distance.powi(2)) / (2.0 * distance);
    let bend = (-Vec3::Z + direction * direction.z).normalize_or_zero();
    let knee = hip + direction * along + bend * (0.50_f32.powi(2) - along.powi(2)).max(0.0).sqrt();
    let lower = Mat4::from_translation(ankle - ankle_rest)
        * around(
            ankle_rest,
            Quat::from_rotation_arc(Vec3::Y, (knee - ankle).normalize_or_zero()),
        );
    let upper = Mat4::from_translation(-Vec3::Y * drop)
        * around(
            rest_hip,
            Quat::from_rotation_arc(-Vec3::Y, (knee - hip).normalize_or_zero()),
        );
    // Ankle articulation leaves the whole sole parallel to its contact plane.
    // It does not tilt the boot merely to lift one corner above the floor.
    (upper, lower, boot)
}

// Read actual authored vertices once. A box around a slanted corpse includes
// empty corners and would visibly float the real mesh above the floor.
fn ground_points(segment: Segment) -> &'static [Vec3] {
    use bevy::mesh::VertexAttributeValues;
    static POINTS: std::sync::LazyLock<[Vec<Vec3>; 17]> = std::sync::LazyLock::new(|| {
        SEGMENTS.map(|segment| {
            let mesh = geometry(segment);
            let Some(VertexAttributeValues::Float32x3(positions)) =
                mesh.attribute(Mesh::ATTRIBUTE_POSITION)
            else {
                unreachable!("king mesh positions");
            };
            positions.iter().copied().map(Vec3::from_array).collect()
        })
    });
    &POINTS[segment as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walking_and_turning_exchange_support_without_sliding_the_planted_boot() {
        let mut motion = Motion::new(Vec3::ZERO, 0.0, MobAction::Chase);
        let mut planted_frames = 0;
        let mut steps = 0;
        for frame in 1..180 {
            let old = motion.feet;
            let position = Vec3::new(0.0, 0.0, -(frame as f32) * 0.022);
            let yaw = frame as f32 * 0.012;
            motion.sample(
                position,
                yaw,
                MobAction::Chase,
                Duration::from_millis(frame * 16),
                0.0,
                Duration::from_secs_f32(1.0 / 60.0),
                false,
            );
            for (before, after) in old.iter().zip(motion.feet) {
                if before.swing == 1.0 && after.swing == 1.0 {
                    assert!(
                        before.contact.distance(after.contact) < 0.0001,
                        "planted contact slipped"
                    );
                    planted_frames += 1;
                }
                if before.swing < 1.0 && after.swing == 1.0 {
                    steps += 1;
                }
            }
            let root = Mat4::from_rotation_translation(Quat::from_rotation_y(yaw), position);
            for segment in [Segment::BootLeft, Segment::BootRight] {
                let transform = root * motion.transforms[segment as usize].to_matrix();
                let floor = ground_points(segment)
                    .iter()
                    .map(|&point| transform.transform_point3(point).y)
                    .fold(f32::INFINITY, f32::min);
                assert!(floor >= -0.005, "boot crossed floor: {floor}");
            }
        }
        assert!(planted_frames > 100 && steps > 10);
    }

    #[test]
    fn teleport_replants_and_does_not_restart_the_entrance() {
        let mut motion = Motion::new(Vec3::ZERO, 0.0, MobAction::Idle);
        let position = Vec3::new(50.0, 0.0, 20.0);
        motion.sample(
            position,
            2.0,
            MobAction::Idle,
            Duration::ZERO,
            0.0,
            Duration::ZERO,
            false,
        );
        assert!(motion.entrance.is_none());
        for (index, foot) in motion.feet.iter().enumerate() {
            assert!(
                foot.contact
                    .distance(position + Quat::from_rotation_y(2.0) * REST_FEET[index])
                    < 0.0001
            );
        }
    }
}
