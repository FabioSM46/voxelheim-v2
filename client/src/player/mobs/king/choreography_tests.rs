use super::*;
use crate::net::{EncounterMoveKind, EncounterTimeline, HazardShape, HazardVolume, MovePhase};
use crate::player::encounters::{MoveKey, PresentedMove, Window};

pub(super) const MOVES: [(EncounterMoveKind, Option<(u8, u8)>); 8] = [
    (EncounterMoveKind::KingsSentence, None),
    (EncounterMoveKind::ThreeTolls, Some((1, 3))),
    (EncounterMoveKind::ThreeTolls, Some((2, 3))),
    (EncounterMoveKind::ThreeTolls, Some((3, 3))),
    (EncounterMoveKind::Burial, None),
    (EncounterMoveKind::EdictOfTheGraves, None),
    (EncounterMoveKind::SepulchreSpear, None),
    (EncounterMoveKind::RequiemOfTheBuried, None),
];

pub(super) fn fixture(
    kind: EncounterMoveKind,
    combo: Option<(u8, u8)>,
    phase: MovePhase,
    tick: u32,
) -> EncounterTimeline {
    use EncounterMoveKind::*;
    let mut state = crate::player::encounters::tests::timeline();
    state.boss = MobKind::DraugrKing;
    state.phase = 3;
    let one = &mut state.moves[0];
    one.kind = kind;
    one.combo = combo;
    one.phase = phase;
    one.phase_started_tick = tick;
    // Catalogue durations at 20 Hz, including the shorter inter-toll recovery.
    one.phase_ticks = match (kind, phase) {
        (KingsSentence, MovePhase::Telegraph) => 24,
        (KingsSentence, MovePhase::Release) => 5,
        (ThreeTolls, MovePhase::Telegraph) => 18,
        (ThreeTolls, MovePhase::Release) => 4,
        (KingsSentence | EdictOfTheGraves, MovePhase::Recovery) => 36,
        (ThreeTolls, MovePhase::Recovery) => {
            if combo == Some((3, 3)) {
                44
            } else {
                8
            }
        }
        (SepulchreSpear, MovePhase::Telegraph) => 28,
        (SepulchreSpear, MovePhase::Release) => 16,
        (SepulchreSpear, MovePhase::Recovery) => 32,
        (EdictOfTheGraves, MovePhase::Channel) => 16,
        (RequiemOfTheBuried, MovePhase::Channel) => 18,
        (_, MovePhase::Channel) => 14,
        (_, MovePhase::Release) => 4,
        (_, MovePhase::Recovery) => 40,
        _ => 30,
    };
    one.pulse = matches!(kind, Burial | EdictOfTheGraves | RequiemOfTheBuried)
        .then_some((0, if kind == Burial { 4 } else { 3 }));
    one.interruptible = phase == MovePhase::Channel && kind == RequiemOfTheBuried;
    one.hazards = vec![HazardVolume {
        shape: match kind {
            KingsSentence => HazardShape::Line { half_width: 1.1 },
            ThreeTolls if combo == Some((3, 3)) => HazardShape::Line { half_width: 0.65 },
            ThreeTolls => HazardShape::Cone { half_angle: 0.95 },
            Burial => HazardShape::Ring { inner_radius: 0.0 },
            SepulchreSpear => HazardShape::Line { half_width: 0.9 },
            _ => HazardShape::Disc,
        },
        origin: [0.0, 1.4, 0.0],
        direction: [0.0, 0.0, -1.0],
        radius: match kind {
            KingsSentence => 5.0,
            ThreeTolls => 3.8,
            SepulchreSpear => 17.6,
            Burial => 2.0,
            EdictOfTheGraves => 3.0,
            _ => 3.2,
        },
        height: 3.0,
    }];
    if kind == ThreeTolls && combo != Some((3, 3)) {
        let bearing: f32 = if combo == Some((1, 3)) { -0.45 } else { 0.45 };
        one.hazards[0].direction = [bearing.sin(), 0.0, -bearing.cos()];
    }
    if matches!(kind, EdictOfTheGraves | RequiemOfTheBuried) {
        let ring = if kind == EdictOfTheGraves { 7.0 } else { 6.0 };
        let mut opposite = one.hazards[0];
        one.hazards[0].origin[0] = ring;
        opposite.origin[0] = -ring;
        one.hazards.push(opposite);
    }
    one.hazards.iter_mut().for_each(|hazard| {
        hazard.height = match kind {
            Burial => 2.0,
            EdictOfTheGraves | RequiemOfTheBuried => 2.4,
            SepulchreSpear => 2.6,
            _ => 3.0,
        };
    });
    one.aim = Some(one.hazards[0].direction);
    state
}

pub(super) fn presented(
    kind: EncounterMoveKind,
    combo: Option<(u8, u8)>,
    phase: MovePhase,
    progress: f32,
) -> PresentedMove {
    let state = fixture(kind, combo, phase, 100);
    let announced = state.moves[0].clone();
    let ticks = announced.phase_ticks;
    PresentedMove {
        key: MoveKey {
            encounter: state.encounter_id,
            boss: state.boss_entity_id,
            instance: announced.move_instance_id,
        },
        boss_kind: state.boss,
        stage: state.phase,
        announced,
        window: Window::Current,
        progress,
        remaining_ticks: ticks - ((ticks - 1) as f32 * progress).round() as u32,
    }
}

fn poses(one: &PresentedMove) -> [Transform; 15] {
    let motion = motion::Motion::new(Vec3::ZERO, 0.0, MobAction::Windup);
    choreography::sample(&motion, Some(one), 0.0)
}

pub(super) fn vertices(segment: Segment, transform: Transform) -> Vec<Vec3> {
    use bevy::mesh::VertexAttributeValues;
    let mesh = geometry(segment);
    let Some(VertexAttributeValues::Float32x3(points)) = mesh.attribute(Mesh::ATTRIBUTE_POSITION)
    else {
        panic!("positions");
    };
    points
        .iter()
        .map(|&point| transform.transform_point(Vec3::from_array(point)))
        .collect()
}

#[test]
fn sentence_has_a_held_high_blade_a_real_downstroke_and_a_planted_opening() {
    let prepare = poses(&presented(
        EncounterMoveKind::KingsSentence,
        None,
        MovePhase::Telegraph,
        1.0,
    ));
    let first = poses(&presented(
        EncounterMoveKind::KingsSentence,
        None,
        MovePhase::Release,
        0.0,
    ));
    let last = poses(&presented(
        EncounterMoveKind::KingsSentence,
        None,
        MovePhase::Release,
        1.0,
    ));
    let recovery = poses(&presented(
        EncounterMoveKind::KingsSentence,
        None,
        MovePhase::Recovery,
        0.0,
    ));
    let tip = Vec3::new(0.42, 0.25, -0.115);
    assert!(prepare[Segment::Blade as usize].transform_point(tip).y > 3.2);
    assert!(
        prepare[Segment::Blade as usize]
            .to_matrix()
            .abs_diff_eq(first[Segment::Blade as usize].to_matrix(), 0.001)
    );
    let low = last[Segment::Blade as usize].transform_point(tip);
    assert!(low.y.abs() < 0.03, "blade did not plant: {low}");
    assert!(
        last[Segment::Blade as usize]
            .to_matrix()
            .abs_diff_eq(recovery[Segment::Blade as usize].to_matrix(), 0.001)
    );
}

#[test]
fn the_three_tolls_are_opposite_horizontal_cuts_then_a_forward_thrust() {
    let tip = Vec3::new(0.42, 0.25, -0.115);
    let stroke = |step, t| {
        poses(&presented(
            EncounterMoveKind::ThreeTolls,
            Some((step, 3)),
            MovePhase::Release,
            t,
        ))[Segment::Blade as usize]
            .transform_point(tip)
    };
    let (left_start, left_end) = (stroke(1, 0.0), stroke(1, 1.0));
    let (right_start, right_end) = (stroke(2, 0.0), stroke(2, 1.0));
    assert!(left_start.x < left_end.x - 1.0);
    assert!(right_start.x > right_end.x + 1.0);
    let (thrust_start, thrust_end) = (stroke(3, 0.0), stroke(3, 1.0));
    assert!((thrust_end.x - thrust_start.x).abs() < 0.10);
    assert!(thrust_end.z < thrust_start.z - 0.25);
    // A late step three has no dependence on seeing steps one and two.
    let late = presented(
        EncounterMoveKind::ThreeTolls,
        Some((3, 3)),
        MovePhase::Release,
        1.0,
    );
    assert_eq!(
        poses(&late)[Segment::Blade as usize].transform_point(tip),
        thrust_end
    );
}

#[test]
fn each_spell_has_a_distinct_preparation_and_authoritative_pulse_gesture() {
    let mut prior = Vec::new();
    for kind in [
        EncounterMoveKind::Burial,
        EncounterMoveKind::EdictOfTheGraves,
        EncounterMoveKind::SepulchreSpear,
        EncounterMoveKind::RequiemOfTheBuried,
    ] {
        let pose = poses(&presented(kind, None, MovePhase::Telegraph, 1.0));
        let hand = pose[Segment::ForeLeft as usize].transform_point(Vec3::new(-0.39, 1.39, -0.02));
        assert!(
            prior.iter().all(|other: &Vec3| other.distance(hand) > 0.20),
            "cast preparations share the same hand: {kind:?} {hand}"
        );
        prior.push(hand);
    }
    let mut pulse = presented(
        EncounterMoveKind::RequiemOfTheBuried,
        None,
        MovePhase::Channel,
        0.5,
    );
    let before = poses(&pulse);
    pulse.remaining_ticks = 1;
    let contact = poses(&pulse);
    assert_ne!(
        before[Segment::ForeLeft as usize],
        contact[Segment::ForeLeft as usize]
    );
}

#[test]
fn every_authored_pose_keeps_rigid_limbs_the_weapon_hand_and_grounded_soles() {
    for (kind, combo) in MOVES {
        for phase in [
            MovePhase::Telegraph,
            MovePhase::Release,
            MovePhase::Channel,
            MovePhase::Recovery,
        ] {
            for sample in 0..21 {
                let one = presented(kind, combo, phase, sample as f32 / 20.0);
                let pose = poses(&one);
                for transform in pose {
                    assert!(transform.translation.is_finite() && transform.rotation.is_finite());
                    assert!(
                        transform.scale.abs_diff_eq(Vec3::ONE, 0.001),
                        "scaled a rigid segment"
                    );
                }
                let right =
                    pose[Segment::ForeRight as usize].transform_point(Vec3::new(0.39, 1.39, -0.02));
                let grip =
                    pose[Segment::Blade as usize].transform_point(Vec3::new(0.39, 1.39, -0.02));
                assert!(
                    right.distance(grip) < 0.002,
                    "detached blade {kind:?}/{phase:?}: {right} vs {grip}"
                );
                if choreography::controls(&one).two_hands {
                    let left = pose[Segment::ForeLeft as usize]
                        .transform_point(Vec3::new(-0.39, 1.39, -0.02));
                    let second_grip =
                        pose[Segment::Blade as usize].transform_point(Vec3::new(0.39, 1.49, -0.02));
                    assert!(
                        left.distance(second_grip) < 0.003,
                        "left hand missed second grip {kind:?}/{phase:?}: {}",
                        left.distance(second_grip)
                    );
                }
                for (thigh, shin, boot, x) in [
                    (
                        Segment::ThighLeft,
                        Segment::ShinLeft,
                        Segment::BootLeft,
                        -0.17,
                    ),
                    (
                        Segment::ThighRight,
                        Segment::ShinRight,
                        Segment::BootRight,
                        0.17,
                    ),
                ] {
                    let knee = Vec3::new(x, 0.55, 0.0);
                    let ankle = Vec3::new(x, 0.17, 0.0);
                    assert!(
                        pose[thigh as usize]
                            .transform_point(knee)
                            .distance(pose[shin as usize].transform_point(knee))
                            < 0.002,
                        "detached knee {kind:?}/{phase:?}"
                    );
                    assert!(
                        pose[shin as usize]
                            .transform_point(ankle)
                            .distance(pose[boot as usize].transform_point(ankle))
                            < 0.002,
                        "detached ankle {kind:?}/{phase:?}"
                    );
                    assert!(
                        (pose[boot as usize].rotation * Vec3::Y).abs_diff_eq(Vec3::Y, 0.001),
                        "tilted planted sole {kind:?}/{phase:?}"
                    );
                }
                for (upper, fore, x) in [
                    (Segment::UpperLeft, Segment::ForeLeft, -0.39),
                    (Segment::UpperRight, Segment::ForeRight, 0.39),
                ] {
                    let joint = Vec3::new(x, 1.80, 0.0);
                    assert!(
                        pose[upper as usize]
                            .transform_point(joint)
                            .distance(pose[fore as usize].transform_point(joint))
                            < 0.002
                    );
                }
                for segment in [Segment::BootLeft, Segment::BootRight] {
                    let floor = vertices(segment, pose[segment as usize])
                        .iter()
                        .map(|p| p.y)
                        .fold(f32::INFINITY, f32::min);
                    assert!(
                        floor.abs() < 0.003,
                        "{kind:?}/{phase:?} {segment:?} floor{floor}"
                    );
                }
            }
        }
    }
}

#[test]
fn stale_cancelled_or_replaced_windows_cannot_replay_an_attack() {
    let mut motion = motion::Motion::new(Vec3::ZERO, 0.0, MobAction::Windup);
    motion.sample(
        Vec3::ZERO,
        0.0,
        MobAction::Windup,
        Duration::ZERO,
        0.0,
        Duration::ZERO,
        true,
    );
    let mut one = presented(
        EncounterMoveKind::ThreeTolls,
        Some((2, 3)),
        MovePhase::Release,
        0.5,
    );
    assert_ne!(
        choreography::sample(&motion, Some(&one), 0.0),
        motion.transforms
    );
    for window in [Window::Upcoming, Window::AwaitingUpdate] {
        one.window = window;
        assert_eq!(
            choreography::sample(&motion, Some(&one), 0.0),
            motion.transforms
        );
    }
    one.window = Window::Current;
    one.announced.ended = Some(crate::net::MoveEnd::Cancelled);
    assert_eq!(
        choreography::sample(&motion, Some(&one), 0.0),
        motion.transforms
    );
    assert_eq!(choreography::sample(&motion, None, 0.0), motion.transforms);
}

#[test]
fn entrance_is_only_cosmetic_idle_appearance_and_never_masks_a_late_fight() {
    let sample = |action, encounter| {
        let mut motion = motion::Motion::new(Vec3::ZERO, 0.0, action);
        motion.sample(
            Vec3::ZERO,
            0.0,
            action,
            Duration::ZERO,
            0.0,
            Duration::ZERO,
            encounter,
        );
        motion
    };
    let idle = sample(MobAction::Idle, false);
    let late = sample(MobAction::Idle, true);
    let active = sample(MobAction::Windup, false);
    assert!(idle.transforms[Segment::Pelvis as usize].translation.y < -0.4);
    assert_eq!(late.transforms[Segment::Pelvis as usize].translation.y, 0.0);
    assert_eq!(
        active.transforms[Segment::Pelvis as usize].translation.y,
        0.0
    );
    let mut interrupted = idle;
    interrupted.sample(
        Vec3::ZERO,
        0.0,
        MobAction::Windup,
        Duration::ZERO,
        0.0,
        Duration::ZERO,
        true,
    );
    interrupted.sample(
        Vec3::ZERO,
        0.0,
        MobAction::Idle,
        Duration::ZERO,
        0.0,
        Duration::ZERO,
        false,
    );
    assert_eq!(
        interrupted.transforms[Segment::Pelvis as usize]
            .translation
            .y,
        0.0
    );
}

#[test]
fn death_articulates_then_stops_without_moving_the_authoritative_root() {
    let mut motion = motion::Motion::new(Vec3::ZERO, 0.0, MobAction::Corpse);
    for sample in 1..21 {
        let down = sample as f32 / 20.0;
        motion.sample(
            Vec3::ZERO,
            0.0,
            MobAction::Corpse,
            Duration::ZERO,
            down,
            Duration::ZERO,
            false,
        );
        let floor = SEGMENTS
            .into_iter()
            .flat_map(|segment| vertices(segment, motion.transforms[segment as usize]))
            .map(|p| p.y)
            .fold(f32::INFINITY, f32::min);
        assert!((-0.001..0.04).contains(&floor), "corpse floor{floor}");
    }
    let chest_floor = vertices(Segment::Torso, motion.transforms[Segment::Torso as usize])
        .iter()
        .map(|point| point.y)
        .fold(f32::INFINITY, f32::min);
    assert!(
        chest_floor < 0.02,
        "corpse is held above the floor: {chest_floor}"
    );
    let settled = motion.transforms;
    motion.sample(
        Vec3::ZERO,
        0.0,
        MobAction::Corpse,
        Duration::from_secs(30),
        1.0,
        Duration::from_secs(30),
        false,
    );
    assert_eq!(settled, motion.transforms);
}
