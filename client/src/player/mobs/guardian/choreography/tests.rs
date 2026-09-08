use super::*;
use crate::net::{EncounterTimeline, HazardShape, HazardVolume};
use crate::player::encounters::{MoveKey, tests::timeline};

const MOVES: [(EncounterMoveKind, Option<(u8, u8)>); 8] = [
    (EncounterMoveKind::BiteAndTear, Some((1, 2))),
    (EncounterMoveKind::BiteAndTear, Some((2, 2))),
    (EncounterMoveKind::PrisonerClaws, None),
    (EncounterMoveKind::PrisonerClaws, Some((1, 2))),
    (EncounterMoveKind::PrisonerClaws, Some((2, 2))),
    (EncounterMoveKind::CollarCharge, None),
    (EncounterMoveKind::PredatorLeap, None),
    (EncounterMoveKind::BonebreakerJaws, None),
];

pub(in super::super) fn fixture(
    kind: EncounterMoveKind,
    combo: Option<(u8, u8)>,
    phase: MovePhase,
    tick: u32,
) -> EncounterTimeline {
    let mut state = timeline();
    state.phase = if combo.is_some() || kind == EncounterMoveKind::BonebreakerJaws {
        2
    } else {
        1
    };
    let one = &mut state.moves[0];
    one.kind = kind;
    one.combo = combo;
    one.phase = phase;
    one.phase_started_tick = tick;
    // Server catalogue at 60 Hz; dense pose tests sample these finite windows.
    one.phase_ticks = match (kind, phase) {
        (EncounterMoveKind::BiteAndTear, MovePhase::Telegraph) => 54,
        (EncounterMoveKind::BiteAndTear, MovePhase::Release) => 12,
        (EncounterMoveKind::PrisonerClaws, MovePhase::Telegraph) => 60,
        (EncounterMoveKind::PrisonerClaws, MovePhase::Release) => 18,
        (EncounterMoveKind::CollarCharge, MovePhase::Telegraph) => 72,
        (EncounterMoveKind::CollarCharge, MovePhase::Release) => 54,
        (EncounterMoveKind::PredatorLeap, MovePhase::Telegraph) => 60,
        (EncounterMoveKind::PredatorLeap, MovePhase::Release) => 36,
        (EncounterMoveKind::PredatorLeap, MovePhase::Recovery) => 96,
        (EncounterMoveKind::BonebreakerJaws, MovePhase::Telegraph) => 90,
        (EncounterMoveKind::BonebreakerJaws, MovePhase::Release) => 18,
        (EncounterMoveKind::BonebreakerJaws, MovePhase::Recovery) => 150,
        (_, MovePhase::Recovery) if combo == Some((1, 2)) => 24,
        (EncounterMoveKind::PrisonerClaws, MovePhase::Recovery) if combo.is_none() => 84,
        _ => 108,
    };
    one.hazards = vec![HazardVolume {
        shape: match kind {
            EncounterMoveKind::CollarCharge => HazardShape::Line { half_width: 1.4 },
            EncounterMoveKind::PredatorLeap => HazardShape::Disc,
            EncounterMoveKind::PrisonerClaws => HazardShape::Cone { half_angle: 1.05 },
            EncounterMoveKind::BonebreakerJaws => HazardShape::Cone { half_angle: 0.38 },
            _ => HazardShape::Cone { half_angle: 0.70 },
        },
        origin: [
            0.0,
            if kind == EncounterMoveKind::PredatorLeap {
                1.5
            } else {
                0.9
            },
            0.0,
        ],
        direction: [0.0, 0.0, -1.0],
        radius: match kind {
            EncounterMoveKind::CollarCharge => 9.9,
            EncounterMoveKind::PrisonerClaws => 3.4,
            EncounterMoveKind::BonebreakerJaws => 3.6,
            _ => 3.0,
        },
        height: match kind {
            EncounterMoveKind::PredatorLeap => 3.0,
            EncounterMoveKind::CollarCharge => 2.4,
            _ => 2.2,
        },
    }];
    // The server faces the locked aim at the beginning of each blow. This fixture
    // uses that root-local frame; target bearing is not an extra local yaw offset.
    state
}
fn presented(
    kind: EncounterMoveKind,
    combo: Option<(u8, u8)>,
    phase: MovePhase,
    elapsed: u32,
) -> PresentedMove {
    let state = fixture(kind, combo, phase, 100);
    let ticks = state.moves[0].phase_ticks;
    let elapsed = elapsed * (ticks - 1) / 60;
    PresentedMove {
        key: MoveKey {
            encounter: 5,
            boss: 9,
            instance: 11,
        },
        boss_kind: state.boss,
        stage: state.phase,
        announced: state.moves[0].clone(),
        window: Window::Current,
        progress: elapsed as f32 / ticks as f32,
        remaining_ticks: ticks - elapsed,
    }
}
fn points(segment: Segment, transform: Transform) -> Vec<Vec3> {
    let mesh = boxes(&geometry(segment));
    let bevy::mesh::VertexAttributeValues::Float32x3(vertices) =
        mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap()
    else {
        panic!("positions");
    };
    vertices
        .iter()
        .map(|p| transform.transform_point(Vec3::from_array(*p)))
        .collect()
}

#[test]
fn every_extreme_has_joined_rigid_limbs_and_planted_supports() {
    let motion = Motion::new(Vec3::ZERO, 0.0);
    for (kind, combo) in MOVES {
        for phase in [
            MovePhase::Telegraph,
            MovePhase::Release,
            MovePhase::Recovery,
        ] {
            for elapsed in 0..=60 {
                let one = presented(kind, combo, phase, elapsed);
                let controls = controls(&one);
                let pose = sample(&motion, Some(&one), 2, 0.0);
                for leg in 0..4 {
                    let upper = pose[5 + 2 * leg];
                    let lower = pose[6 + 2 * leg];
                    let knee = rest_foot(leg) + Vec3::Y * 0.57;
                    let gap = upper
                        .transform_point(knee)
                        .distance(lower.transform_point(knee));
                    assert!(
                        gap < 0.003,
                        "{kind:?} {combo:?} {phase:?} tick {elapsed} leg {leg} knee gap {gap}"
                    );
                    let hip = rest_foot(leg) + Vec3::Y * 0.90;
                    let parent = pose[if leg < 2 {
                        Segment::Thorax
                    } else {
                        Segment::Pelvis
                    } as usize];
                    assert!(
                        upper
                            .transform_point(hip)
                            .distance(parent.transform_point(hip))
                            < 0.00001
                    );
                    let sole = points(SEGMENTS[6 + 2 * leg], lower)
                        .into_iter()
                        .map(|p| p.y)
                        .fold(f32::INFINITY, f32::min);
                    assert!(
                        sole >= -0.00002,
                        "{kind:?} {phase:?} tick {elapsed} sole {sole}"
                    );
                    if controls.feet[leg].y == 0.0 && controls.lift == 0.0 {
                        assert!(sole < 0.001, "unplanted support {kind:?} {phase:?} {sole}");
                        assert!(
                            lower
                                .transform_point(rest_foot(leg))
                                .xz()
                                .abs_diff_eq(controls.feet[leg].xz(), 0.00001)
                        );
                    }
                }
                assert!(pose.iter().all(|t| t.scale.abs_diff_eq(Vec3::ONE, 0.00001)));
                for (i, segment) in SEGMENTS.iter().enumerate() {
                    for point in points(*segment, pose[i]) {
                        assert!(
                            point.y >= -0.00002 && point.y < 3.0,
                            "{kind:?} {phase:?} {segment:?} y {}",
                            point.y
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn distinct_steps_scrapes_and_heavy_recoveries_are_observable() {
    let motion = Motion::new(Vec3::ZERO, 0.0);
    let mut preparations = Vec::new();
    for (kind, combo) in MOVES {
        let pose = sample(
            &motion,
            Some(&presented(kind, combo, MovePhase::Telegraph, 55)),
            2,
            0.0,
        );
        if combo != Some((2, 2)) || kind != EncounterMoveKind::PrisonerClaws {
            preparations.push(pose);
        }
        let release = sample(
            &motion,
            Some(&presented(kind, combo, MovePhase::Release, 20)),
            2,
            0.0,
        );
        assert_ne!(pose, release);
        let early = sample(
            &motion,
            Some(&presented(kind, combo, MovePhase::Recovery, 0)),
            2,
            0.0,
        );
        let late = sample(
            &motion,
            Some(&presented(kind, combo, MovePhase::Recovery, 60)),
            2,
            0.0,
        );
        assert_eq!(
            early, late,
            "an opening disappears before the announced recovery ends"
        );
    }
    for (i, pose) in preparations.iter().enumerate() {
        assert!(!preparations[..i].contains(pose));
    }
    for (tick, expected_leg) in [(11, 1), (35, 0)] {
        let one = presented(
            EncounterMoveKind::CollarCharge,
            None,
            MovePhase::Telegraph,
            tick,
        );
        let pose = sample(&motion, Some(&one), 1, 0.0);
        let minimum = |leg: usize| {
            points(SEGMENTS[6 + leg * 2], pose[6 + leg * 2])
                .iter()
                .map(|p| p.y)
                .fold(f32::INFINITY, f32::min)
        };
        assert!(minimum(expected_leg) > 0.10);
        assert!(minimum(1 - expected_leg) < 0.001);
    }
    for (step, raised) in [(1, 0), (2, 1)] {
        let one = presented(
            EncounterMoveKind::PrisonerClaws,
            Some((step, 2)),
            MovePhase::Telegraph,
            60,
        );
        let pose = sample(&motion, Some(&one), 2, 0.0);
        assert!(pose[6 + raised * 2].transform_point(rest_foot(raised)).y > 0.25);
        assert!(
            points(SEGMENTS[6 + (1 - raised) * 2], pose[6 + (1 - raised) * 2])
                .iter()
                .any(|p| p.y.abs() < 0.001)
        );
    }
}

#[test]
fn missing_cancelled_and_late_state_never_queues_or_replays_an_attack() {
    let mut motion = Motion::new(Vec3::ZERO, 0.0);
    motion.sample(
        Vec3::ZERO,
        0.0,
        MobAction::Windup,
        Duration::ZERO,
        0.0,
        Duration::from_millis(16),
    );
    let mut one = presented(
        EncounterMoveKind::PrisonerClaws,
        Some((2, 2)),
        MovePhase::Release,
        30,
    );
    let direct_second = sample(&motion, Some(&one), 2, 0.0);
    one.key.instance = 99;
    assert_eq!(direct_second, sample(&motion, Some(&one), 2, 0.0));
    for window in [Window::Upcoming, Window::AwaitingUpdate] {
        one.window = window;
        assert_eq!(
            sample(&motion, Some(&one), 2, 0.0),
            sample(&motion, None, 2, 0.0)
        );
    }
    one.window = Window::Current;
    one.announced.ended = Some(crate::net::MoveEnd::Cancelled);
    assert_eq!(
        sample(&motion, Some(&one), 2, 0.0),
        sample(&motion, None, 2, 0.0)
    );
    one.announced.ended = None;
    one.announced.phase_ticks = 1;
    one.remaining_ticks = 1;
    assert_eq!(progress(&one), 1.0);
    let leap = presented(
        EncounterMoveKind::PredatorLeap,
        None,
        MovePhase::Release,
        60,
    );
    let landing = sample(&motion, Some(&leap), 1, 0.0);
    for leg in 0..4 {
        assert!(
            points(SEGMENTS[6 + leg * 2], landing[6 + leg * 2])
                .iter()
                .any(|p| p.y.abs() < 0.001)
        );
    }
}

#[test]
fn actual_striking_vertices_fit_hazards_and_report_unexplained_reach() {
    // Accepted dimensions are not stretched to fill provisional damage geometry.
    // Record the remaining radius for #1037 instead of passing a containment-only check.
    let motion = Motion::new(Vec3::ZERO, 0.0);
    let mut report = String::from(
        "move,combo,damage_radius,max_visible_radius,radial_shortfall,max_angle_deviation\n",
    );
    for (kind, combo) in MOVES {
        if kind == EncounterMoveKind::CollarCharge {
            continue;
        }
        let mut maximum: f32 = 0.0;
        let mut angle_max: f32 = 0.0;
        for elapsed in 0..=60 {
            if kind == EncounterMoveKind::PredatorLeap && elapsed != 60 {
                continue;
            }
            let one = presented(kind, combo, MovePhase::Release, elapsed);
            let pose = sample(&motion, Some(&one), 2, 0.0);
            let volume = &one.announced.hazards[0];
            let segments: Vec<_> = match kind {
                EncounterMoveKind::PrisonerClaws => {
                    vec![if combo.is_some_and(|(step, _)| step == 1) {
                        Segment::LowerFrontLeft
                    } else {
                        Segment::LowerFrontRight
                    }]
                }
                EncounterMoveKind::PredatorLeap => vec![
                    Segment::LowerFrontLeft,
                    Segment::LowerFrontRight,
                    Segment::LowerRearLeft,
                    Segment::LowerRearRight,
                ],
                _ => vec![Segment::Head],
            };
            for segment in segments {
                let mesh = boxes(&geometry(segment));
                let bevy::mesh::VertexAttributeValues::Float32x3(vertices) =
                    mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap()
                else {
                    panic!("positions")
                };
                let bevy::mesh::VertexAttributeValues::Float32x4(colours) =
                    mesh.attribute(Mesh::ATTRIBUTE_COLOR).unwrap()
                else {
                    panic!("colours")
                };
                for (point, colour) in vertices.iter().zip(colours) {
                    if *colour != BONE.to_linear().to_f32_array() {
                        continue;
                    }
                    let p = pose[segment as usize].transform_point(Vec3::from_array(*point));
                    let radius = p.xz().length();
                    maximum = maximum.max(radius);
                    let direction = Vec3::from_array(volume.direction).xz().normalize_or_zero();
                    let angle = p
                        .xz()
                        .normalize_or_zero()
                        .dot(direction)
                        .clamp(-1.0, 1.0)
                        .acos();
                    angle_max = angle_max.max(angle);
                    assert!(radius <= volume.radius, "striking reach outside {kind:?}");
                    assert!((p.y - volume.origin[1]).abs() <= volume.height * 0.5 + 0.0001);
                    match volume.shape {
                        HazardShape::Cone { half_angle } => assert!(
                            angle <= half_angle,
                            "{kind:?} {combo:?}: angle {angle} > {half_angle}"
                        ),
                        HazardShape::Line { half_width } => {
                            assert!(p.x.abs() <= half_width && p.z <= 0.0)
                        }
                        HazardShape::Disc => (),
                        _ => panic!("physical fixture"),
                    }
                }
            }
        }
        let radius = fixture(kind, combo, MovePhase::Release, 0).moves[0].hazards[0].radius;
        assert!(maximum > 0.3, "no striking vertices measured");
        report.push_str(&format!(
            "{kind:?},\"{combo:?}\",{radius:.3},{maximum:.3},{:.3},{angle_max:.3}\n",
            radius - maximum
        ));
    }
    let charge = presented(
        EncounterMoveKind::CollarCharge,
        None,
        MovePhase::Release,
        60,
    );
    let pose = sample(&motion, Some(&charge), 2, 0.0);
    let vertices: Vec<_> = SEGMENTS
        .iter()
        .enumerate()
        .flat_map(|(i, segment)| points(*segment, pose[i]))
        .collect();
    let half_width = vertices.iter().map(|p| p.x.abs()).fold(0.0, f32::max);
    let overhang = vertices.iter().map(|p| -p.z).fold(0.0, f32::max);
    assert!(half_width < 1.4);
    // The lane caps root travel, not the accepted body's nose. Measure the
    // overhang honestly: translating the root to 9.9 puts these vertices beyond it.
    assert!(overhang > 0.7 && overhang < 1.0);
    std::fs::write(std::env::temp_dir().join("guardian-1028-charge.csv"), format!(
        "announced_root_travel,announced_half_width,visible_half_width,lateral_shortfall,lane_end_body_overhang\n9.900,1.400,{half_width:.3},{:.3},{overhang:.3}\n", 1.4 - half_width)).unwrap();
    std::fs::write(std::env::temp_dir().join("guardian-1028-reach.csv"), report).unwrap();
}

#[test]
fn charge_at_server_speed_keeps_supports_and_stops_for_the_whole_announced_window() {
    let mut motion = Motion::new(Vec3::ZERO, 0.0);
    motion.fast_travel = true;
    for frame in 1..55 {
        let previous = motion.feet;
        let position = Vec3::new(0.0, 0.0, -11.0 * frame as f32 / 60.0);
        motion.sample(
            position,
            0.0,
            MobAction::Windup,
            Duration::ZERO,
            0.0,
            Duration::from_secs_f32(1.0 / 60.0),
        );
        let one = presented(
            EncounterMoveKind::CollarCharge,
            None,
            MovePhase::Release,
            frame,
        );
        let pose = sample(&motion, Some(&one), 2, 0.0);
        for leg in 0..4 {
            if previous[leg].swing == 1.0 && motion.feet[leg].swing == 1.0 {
                assert!(
                    previous[leg]
                        .contact
                        .abs_diff_eq(motion.feet[leg].contact, 0.00001),
                    "charge support slid at frame {frame} leg {leg}"
                );
            }
            let knee = rest_foot(leg) + Vec3::Y * 0.57;
            let gap = pose[5 + 2 * leg]
                .transform_point(knee)
                .distance(pose[6 + 2 * leg].transform_point(knee));
            assert!(gap < 0.003, "charge knee gap {gap}");
        }
    }
    let mut recovery = presented(
        EncounterMoveKind::CollarCharge,
        None,
        MovePhase::Recovery,
        0,
    );
    let a = sample(&motion, Some(&recovery), 2, 0.0);
    recovery.announced.phase_ticks = 157; // Arbitrary authoritative extension, not a copied impact duration.
    recovery.remaining_ticks = 1;
    assert_eq!(a, sample(&motion, Some(&recovery), 2, 0.0));
}

#[test]
fn stage_two_opens_outward_and_keeps_chain_anchors_joined_including_on_the_floor() {
    let rest = [Transform::IDENTITY; 19];
    assert_eq!(stage_pose(rest, 1), rest);
    let opened = stage_pose(rest, 2);
    let centre_front = Vec3::new(0.0, 1.0, -0.54);
    assert!(
        opened[Segment::CollarLeft as usize]
            .transform_point(centre_front)
            .x
            < -0.25
    );
    assert!(
        opened[Segment::CollarRight as usize]
            .transform_point(centre_front)
            .x
            > 0.25
    );
    for (chain, collar) in [
        (Segment::ChainLeft, Segment::CollarLeft),
        (Segment::ChainMiddle, Segment::CollarLeft),
        (Segment::ChainRight, Segment::CollarRight),
    ] {
        assert_eq!(opened[chain as usize], opened[collar as usize]);
    }
    for step in 1..=20 {
        let base = super::super::pose(
            std::array::from_fn(rest_foot),
            MobAction::Corpse,
            Duration::ZERO,
            step as f32 / 20.0,
        );
        let corpse = corpse_pose(base, 2);
        let lowest = SEGMENTS
            .iter()
            .enumerate()
            .flat_map(|(i, &segment)| points(segment, corpse[i]))
            .map(|p| p.y)
            .fold(f32::INFINITY, f32::min);
        assert!(
            (-0.00001..0.08).contains(&lowest),
            "opened corpse floor {lowest}"
        );
    }
}
