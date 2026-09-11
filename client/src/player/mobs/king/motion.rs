//! Cosmetic contacts follow snapshot displacement. A planted boot stays in world
//! space until the other support takes over; teleports replant instead of stretching.
use super::choreography::Wrist;
use super::*;
use crate::player::encounters::PresentedMove;

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
    /// Which way the corpse folds: 1 forward, the authored fall, or -1 backward. Chosen once
    /// when a fall begins, from terrain, and cleared by a replant.
    fall: Option<f32>,
    /// The wrist articulation chosen for the planted blow being presented, from terrain.
    blade: Option<Blade>,
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
            fall: None,
            blade: None,
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
            self.transforms = self.fold(down, self.fall.unwrap_or(1.0));
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

impl Motion {
    /// The corpse pose `down` of the way through a fall, forward (`sign` 1) or backward.
    ///
    /// Fold at the hips and knees before settling on the floor. All fifteen parts share the
    /// final grounding translation; no independent floating limbs.
    fn fold(&self, down: f32, sign: f32) -> [Transform; 17] {
        let p = super::choreography::Controls {
            drop: 0.12 * (std::f32::consts::PI * down).sin(),
            grip: Vec3::new(0.42, 1.39, -0.115),
            head: 0.25 * down,
            crown: self.regalia.crown(),
            ..default()
        };
        let mut pose = super::choreography::assemble(p, REST_FEET);
        let fall = around(
            Vec3::Y * 1.05,
            Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2 * down * sign),
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
        pose
    }

    /// Chooses once which way this corpse folds: forward, the authored fall, unless the
    /// fully folded body would lie inside solid terrain and the backward fold would not.
    ///
    /// Measured in the shipped chamber (#1037): a king felled facing a monolith 1.1 blocks
    /// away folded 108 vertices into it. Cosmetic only — the snapshot root and yaw are not
    /// touched, and the pose is the same fold mirrored, inside the same envelope.
    pub(in super::super) fn choose_fall(
        &mut self,
        position: Vec3,
        yaw: f32,
        solid: impl Fn(IVec3) -> bool,
    ) {
        if self.fall.is_some() {
            return;
        }
        let root = Mat4::from_rotation_translation(Quat::from_rotation_y(yaw), position);
        let clips = |sign: f32| {
            let pose = self.fold(1.0, sign);
            SEGMENTS.iter().zip(&pose).any(|(&segment, transform)| {
                let matrix = root * transform.to_matrix();
                ground_points(segment)
                    .iter()
                    .any(|&point| buried(matrix.transform_point3(point), position.y, &solid))
            })
        };
        self.fall = Some(if clips(1.0) && !clips(-1.0) {
            -1.0
        } else {
            1.0
        });
    }

    /// The wrist articulation to present `one` with, if it is the blow a choice was made for.
    pub(in super::super) fn wrist(&self, one: &PresentedMove) -> Wrist {
        self.blade
            .filter(|blade| blade.instance == one.key.instance && blade.step == step(one))
            .map_or(Wrist::AUTHORED, |blade| blade.wrist)
    }

    /// Chooses once per planted blow how its blade is articulated: the authored stroke unless
    /// some model vertex would lie inside solid terrain in a preparation, release or recovery
    /// sample, then the first variant in [`super::choreography::WRISTS`] with none, or failing
    /// that the one with the fewest.
    ///
    /// Measured in the shipped chamber (#1037): a king striking 1.1 blocks from a monolith put
    /// 24–96 blade vertices inside it. Presentation only — the snapshot root, yaw, locked aim,
    /// timing and announced regions are not touched, and the strike layer drawing the announced
    /// reach does not read this choice. A new blow, or a root that moves, chooses again.
    pub(in super::super) fn choose_blade(
        &mut self,
        one: Option<&PresentedMove>,
        position: Vec3,
        yaw: f32,
        solid: impl Fn(IVec3) -> bool,
    ) {
        let Some(one) =
            super::choreography::live(one).filter(|one| super::choreography::planted_blow(one))
        else {
            return;
        };
        if self.blade.is_some_and(|blade| {
            blade.instance == one.key.instance
                && blade.step == step(one)
                && blade.position.distance(position) < 0.25
                && (blade.yaw - yaw).abs() < 0.1
        }) {
            return;
        }
        let near = Near::read(position, &solid);
        let wrist = if near.any {
            self.clearest(one, position, yaw, &near)
        } else {
            Wrist::AUTHORED
        };
        self.blade = Some(Blade {
            instance: one.key.instance,
            step: step(one),
            position,
            yaw,
            wrist,
        });
    }

    fn clearest(&self, one: &PresentedMove, position: Vec3, yaw: f32, near: &Near) -> Wrist {
        use Segment::*;
        // The segments a wrist articulation moves. Every other segment is the same in every
        // variant, so it is counted once, from the authored stroke.
        const WIELDING: [Segment; 5] = [Blade, UpperLeft, UpperRight, ForeLeft, ForeRight];
        let root = Mat4::from_rotation_translation(Quat::from_rotation_y(yaw), position);
        let frames = super::choreography::frames(self, one, yaw);
        let solid = |voxel: IVec3| near.at(voxel);
        let count = |wrist: Wrist, wielding: bool, limit: usize| {
            let mut clipped = 0;
            for &p in &frames {
                let pose = super::choreography::pose(self, p, wrist);
                if wielding && wrist != Wrist::AUTHORED && !super::choreography::held(&pose, &p) {
                    return usize::MAX;
                }
                for (&segment, transform) in SEGMENTS.iter().zip(&pose) {
                    if WIELDING.contains(&segment) != wielding {
                        continue;
                    }
                    let matrix = root * transform.to_matrix();
                    clipped += ground_points(segment)
                        .iter()
                        .filter(|&&point| {
                            buried(matrix.transform_point3(point), position.y, &solid)
                        })
                        .count();
                    if clipped > limit {
                        return clipped;
                    }
                }
            }
            clipped
        };
        let fixed = count(Wrist::AUTHORED, false, usize::MAX);
        let (mut fewest, mut chosen) = (usize::MAX, Wrist::AUTHORED);
        for wrist in super::choreography::WRISTS {
            // Only a strictly smaller count replaces the earlier, smaller departure.
            let limit = fewest.saturating_sub(fixed).saturating_sub(1);
            let total = fixed.saturating_add(count(wrist, true, limit));
            if total < fewest {
                (fewest, chosen) = (total, wrist);
            }
            if fewest == 0 {
                break;
            }
        }
        chosen
    }
}

/// The blow a wrist choice was made for, and the stance it was made in.
#[derive(Debug, Clone, Copy)]
struct Blade {
    instance: u64,
    step: u8,
    position: Vec3,
    yaw: f32,
    wrist: Wrist,
}

fn step(one: &PresentedMove) -> u8 {
    one.announced.combo.map_or(0, |(step, _)| step)
}

/// Whether a model point lies strictly inside a solid voxel above the floor the body stands on,
/// at least 0.02 blocks from every face: the measure the chamber capture reports (#1037).
pub(super) fn buried(point: Vec3, floor: f32, solid: &impl Fn(IVec3) -> bool) -> bool {
    let within = point - point.floor();
    point.y > floor + 0.02
        && within.cmpge(Vec3::splat(0.02)).all()
        && within.cmple(Vec3::splat(0.98)).all()
        && solid(point.floor().as_ivec3())
}

/// The voxels a standing king's blade can reach, read once per choice so a blow's many samples
/// never query the chunk store vertex by vertex. Nothing outside it is reachable: the blade tip
/// is at most 2.6 blocks from the root across, and 3.8 above the floor.
struct Near {
    origin: IVec3,
    cells: Vec<bool>,
    any: bool,
}

impl Near {
    const REACH: i32 = 4;
    const HEIGHT: i32 = 6;
    const SIDE: i32 = 2 * Self::REACH + 1;

    /// Starts at the layer above the floor the body stands on, which [`buried`] never counts,
    /// so a king on open floor reads no solid voxel and keeps the authored stroke at once.
    fn read(position: Vec3, solid: &impl Fn(IVec3) -> bool) -> Self {
        let base = (position + Vec3::Y * 0.02).floor().as_ivec3();
        let origin = base - IVec3::new(Self::REACH, 0, Self::REACH);
        let mut cells = Vec::with_capacity((Self::SIDE * Self::SIDE * Self::HEIGHT) as usize);
        for y in 0..Self::HEIGHT {
            for z in 0..Self::SIDE {
                for x in 0..Self::SIDE {
                    cells.push(solid(origin + IVec3::new(x, y, z)));
                }
            }
        }
        let any = cells.contains(&true);
        Self { origin, cells, any }
    }

    fn at(&self, voxel: IVec3) -> bool {
        let local = voxel - self.origin;
        let side = Self::SIDE;
        (0..side).contains(&local.x)
            && (0..side).contains(&local.z)
            && (0..Self::HEIGHT).contains(&local.y)
            && self.cells[((local.y * side + local.z) * side + local.x) as usize]
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
pub(super) fn ground_points(segment: Segment) -> &'static [Vec3] {
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
    fn a_corpse_folds_away_from_terrain_it_would_otherwise_lie_in() {
        // Root half a block inside cell z = 0, so an integer boundary separates the forward
        // fold's reach from the backward fold's front edge.
        let root = Vec3::new(0.5, 0.0, 0.5);
        let reach = |sign: f32| {
            let motion = Motion::new(root, 0.0, MobAction::Corpse);
            SEGMENTS
                .iter()
                .zip(&motion.fold(1.0, sign))
                .flat_map(|(&segment, transform)| {
                    ground_points(segment)
                        .iter()
                        .map(|&point| transform.transform_point(point).z + root.z)
                })
                .fold(f32::INFINITY, f32::min)
        };
        assert!(
            reach(1.0) < -1.05 && reach(-1.0) > -0.95,
            "fixture boundary"
        );
        let wall_ahead = |voxel: IVec3| voxel.z <= -2 && voxel.y >= 0;

        let mut felled = Motion::new(root, 0.0, MobAction::Corpse);
        felled.choose_fall(root, 0.0, wall_ahead);
        assert_eq!(felled.fall, Some(-1.0), "a wall ahead mirrors the fall");
        felled.choose_fall(root, 0.0, |_| false);
        assert_eq!(felled.fall, Some(-1.0), "the choice is made once");
        felled.sample(
            root,
            0.0,
            MobAction::Corpse,
            Duration::ZERO,
            1.0,
            Duration::from_millis(16),
            false,
        );
        let lowest = SEGMENTS
            .iter()
            .zip(&felled.transforms)
            .flat_map(|(&segment, transform)| {
                ground_points(segment)
                    .iter()
                    .map(|&point| transform.transform_point(point).y)
            })
            .fold(f32::INFINITY, f32::min);
        assert!(
            lowest.abs() < 0.005,
            "the mirrored corpse rests on the floor: {lowest}"
        );

        for (terrain, expected) in [
            (
                Box::new(|_: IVec3| false) as Box<dyn Fn(IVec3) -> bool>,
                1.0,
            ),
            (
                Box::new(|voxel: IVec3| (voxel.z <= -2 || voxel.z >= 1) && voxel.y >= 0),
                1.0,
            ),
        ] {
            let mut motion = Motion::new(root, 0.0, MobAction::Corpse);
            motion.choose_fall(root, 0.0, terrain);
            assert_eq!(
                motion.fall,
                Some(expected),
                "open or boxed-in ground keeps the authored fall"
            );
        }
        let mut replanted = felled;
        replanted.sample(
            root + Vec3::X * 40.0,
            0.0,
            MobAction::Corpse,
            Duration::ZERO,
            1.0,
            Duration::from_millis(16),
            false,
        );
        assert_eq!(replanted.fall, None, "a replant chooses again");
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
