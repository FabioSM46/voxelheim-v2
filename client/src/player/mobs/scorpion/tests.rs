//! The scorpion's proportions, its gait, its two telegraphs and its death, measured rather
//! than described — and its timings read out of the server's own row. No window, no display
//! and no GPU.
use super::*;
use crate::net::{MobState, SnapshotInbox};
use bevy::time::TimeUpdateStrategy;

const FRAME: Duration = Duration::from_micros(16_667);
const DT: f32 = 1.0 / 60.0;

/// The server's species table, where the scorpion's row lives (#1291, PR #1302).
const SPECIES: &str = include_str!("../../../../../server/internal/game/species.go");

fn extent(meshes: &[Mesh]) -> (Vec3, Vec3) {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for mesh in meshes {
        let positions = mesh
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .and_then(|values| values.as_float3())
            .expect("every part carries positions");
        for vertex in positions {
            min = min.min(Vec3::from_array(*vertex));
            max = max.max(Vec3::from_array(*vertex));
        }
    }
    (min, max)
}

fn posed(transforms: &[Transform; SEGMENT_COUNT]) -> Vec<Mesh> {
    meshes()
        .into_iter()
        .take(RIG)
        .zip(transforms)
        .map(|((_, mesh), transform)| mesh.transformed_by(*transform))
        .collect()
}

/// The far end of a bone drawn with `transform` from a mesh `length` long along +X.
fn tip(transform: Transform, length: f32) -> Vec3 {
    transform.transform_point(Vec3::X * length)
}

/// The stinger's point, read off the drawn stinger rather than off the pose that placed it.
fn sting_point(transforms: &[Transform; SEGMENT_COUNT]) -> Vec3 {
    let (w, _) = frame();
    transforms[index(Segment::Stinger)].transform_point(Vec3::new(
        STINGER_LENGTH * w,
        0.035 * w,
        0.0,
    ))
}

/// The far end of a claw's movable finger, and of its fixed one.
fn fingertips(transforms: &[Transform; SEGMENT_COUNT], arm: u8) -> (Vec3, Vec3) {
    let (w, _) = frame();
    let inside = -side(usize::from(arm));
    (
        transforms[index(Segment::Finger(arm))].transform_point(Vec3::new(0.0, 0.0, -0.08 * w)),
        transforms[index(Segment::Claw(arm))].transform_point(Vec3::new(
            inside * 0.026 * w,
            0.0,
            -0.19 * w,
        )),
    )
}

/// One field of the scorpion's row in `species.go`, in seconds or blocks.
fn server_number(row: &str, key: &str) -> f32 {
    let line = row
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with(&format!("{key}:")))
        .unwrap_or_else(|| panic!("the scorpion's row has no {key}"));
    let value = line[key.len() + 1..].trim().trim_end_matches(',');
    match value.split_once(" * time.Millisecond") {
        Some((millis, _)) => millis.trim().parse::<f32>().unwrap() / 1000.0,
        None => value.parse().unwrap(),
    }
}

fn never_solid(_: IVec3) -> bool {
    false
}

#[test]
fn the_box_is_the_servers() {
    let row = SPECIES
        .split("vnet.MobKindScorpion: {")
        .nth(1)
        .expect("species.go has a scorpion row")
        .split("\n\t},")
        .next()
        .unwrap();
    let envelope = body(MobKind::Scorpion);
    assert!(
        row.contains(&format!(
            "body{{width: {}, height: {}}}",
            envelope.width, envelope.height
        )),
        "the scorpion's box is not the server's"
    );
}

#[test]
fn the_timings_are_the_servers() {
    let row = SPECIES
        .split("vnet.MobKindScorpion: {")
        .nth(1)
        .expect("species.go has a scorpion row")
        .split("\n\t},")
        .next()
        .unwrap();
    let (sting, swipe) = row.split_once("swipe: mobAttack{").expect("a swipe");
    let (swipe, after) = swipe.split_once('}').unwrap();
    assert_eq!(server_number(sting, "windup"), STING_WINDUP);
    assert_eq!(server_number(sting, "recovery"), STING_RECOVERY);
    assert_eq!(server_number(swipe, "windup"), SWIPE_WINDUP);
    assert_eq!(server_number(swipe, "recovery"), SWIPE_RECOVERY);
    assert_eq!(server_number(after, "emergence"), EMERGENCE);
    assert_eq!(server_number(after, "emergeRange"), EMERGE_RANGE);
    // The two telegraphs are far enough apart that the length alone can tell them, and the
    // evidence threshold sits between them.
    const { assert!(SWIPE_WINDUP < STING_EVIDENCE && STING_EVIDENCE < STING_WINDUP) };
    const { assert!(STING_EVIDENCE - SWIPE_WINDUP > 0.2 && STING_WINDUP - STING_EVIDENCE > 0.2) };
    // And the sand starts to stir before the server raises it.
    const { assert!(STIR_RANGE > EMERGE_RANGE + 2.0) };
}

#[test]
fn the_rest_pose_fills_the_box_the_server_collides_and_stands_on_its_legs() {
    let (w, h) = frame();
    let (min, max) = extent(&posed_meshes(&Pose::rest()));
    let half = w / 2.0;
    assert!(
        min.y > -1e-3 && min.y < 0.01,
        "the tips stand on the ground: {min}"
    );
    assert!(
        max.y <= h + 1e-4 && max.y >= 0.85 * h,
        "the tail's arch reaches the top: {max}"
    );
    for (axis, low, high) in [("x", min.x, max.x), ("z", min.z, max.z)] {
        assert!(
            low >= -half - 1e-4 && high <= half + 1e-4,
            "{axis}: {low}..{high} leaves the {w}-wide box"
        );
        // Nose to tail it fills the box; across, a scorpion is narrower than it is long.
        let fill = if axis == "z" { 0.8 } else { 0.65 };
        assert!(
            high - low >= fill * w,
            "{axis}: a scorpion {} across does not fill its {w}-wide box",
            high - low
        );
    }
    assert!(
        (min.x + max.x).abs() < 1e-4,
        "left and right mirror each other"
    );
}

#[test]
fn a_scorpion_is_pincers_eight_legs_and_a_tail_of_five_segments_and_a_sting() {
    let (w, h) = frame();
    let count = |matches: fn(&Segment) -> bool| SEGMENTS.iter().filter(|s| matches(s)).count();
    assert_eq!(count(|s| matches!(s, Segment::Tail(_))), 5);
    assert_eq!(count(|s| matches!(s, Segment::Stinger)), 1);
    assert_eq!(
        count(|s| matches!(s, Segment::Upper(_) | Segment::Lower(_))),
        16
    );
    assert_eq!(count(|s| matches!(s, Segment::Claw(_))), 2);
    assert_eq!(count(|s| matches!(s, Segment::Finger(_))), 2);
    for (position, segment) in SEGMENTS.iter().enumerate() {
        assert_eq!(index(*segment), position);
    }

    let meshes = meshes();
    let size = |segment| {
        let (min, max) = extent(&[meshes[index(segment)].1.clone()]);
        max - min
    };
    // Seven plates, a head shield, its crown, chelicerae, belly, ridge and two eyes.
    assert_eq!(meshes[index(Segment::Carapace)].1.count_vertices(), 14 * 24);
    // Low and long: the body is longer than it is wide and well under half the box's height.
    let back = size(Segment::Carapace);
    assert!(back.z > back.x && back.y < 0.6 * h, "{back}");
    for (ring, expected) in TAIL_LENGTH.into_iter().enumerate() {
        let length = size(Segment::Tail(ring as u8)).x;
        assert!(
            (length - expected * w).abs() < 0.05 * w,
            "tail segment {ring} is {length} long"
        );
    }

    // The tail arches up over the body, and the sting is poised above the back pointing
    // forward — the silhouette a player reads a scorpion by.
    let rest = pose(&Pose::rest());
    let (_, carapace_top) = extent(&posed(&rest)[..1]);
    let point = sting_point(&rest);
    let joints = tail_joints(&TAIL_REST, 0.0);
    assert!(point.y > carapace_top.y, "the sting rides above the back");
    assert!(point.z < joints[TAIL].z, "the sting points forward");
    assert!(joints[2].y > carapace_top.y, "the tail arches");
    for leg in 0..LEGS {
        let (upper, lower) = lengths(leg);
        assert!(
            upper > 0.2 * h && lower > upper,
            "leg {leg}: {upper} then {lower}"
        );
    }
}

/// Every joint of the rig, checked: knees on hips and tips, tail rings end to end, the sting
/// on the last ring, each forearm on its humerus and each finger on its hand.
fn assert_joined(transforms: &[Transform; SEGMENT_COUNT]) {
    let (w, _) = frame();
    let body = transforms[index(Segment::Carapace)];
    for leg in 0..LEGS {
        let (upper, lower) = lengths(leg);
        let femur = transforms[index(Segment::Upper(leg as u8))];
        let tibia = transforms[index(Segment::Lower(leg as u8))];
        assert!(
            tip(femur, upper).distance(tibia.translation) < 1e-3,
            "leg {leg}: the knee came apart"
        );
        assert!(
            body.transform_point(hip(leg)).distance(femur.translation) < 1e-3,
            "leg {leg}: the femur left the hip"
        );
        assert!(
            tip(tibia, lower).y > -0.02,
            "leg {leg}: a tip went through the floor"
        );
    }
    for ring in 0..TAIL {
        let next = if ring + 1 < TAIL {
            Segment::Tail(ring as u8 + 1)
        } else {
            Segment::Stinger
        };
        assert!(
            tip(
                transforms[index(Segment::Tail(ring as u8))],
                TAIL_LENGTH[ring] * w
            )
            .distance(transforms[index(next)].translation)
                < 1e-3,
            "tail ring {ring} came apart"
        );
    }
    for arm in 0..2u8 {
        let humerus = transforms[index(Segment::Humerus(arm))];
        let forearm = transforms[index(Segment::Forearm(arm))];
        let claw = transforms[index(Segment::Claw(arm))];
        let upper = at(ELBOW, usize::from(arm)).distance(shoulder(usize::from(arm)));
        let lower = at(WRIST, usize::from(arm)).distance(at(ELBOW, usize::from(arm)));
        assert!(tip(humerus, upper).distance(forearm.translation) < 1e-3);
        assert!(tip(forearm, lower).distance(claw.translation) < 1e-3);
        assert!(
            claw.transform_point(hinge(usize::from(arm)))
                .distance(transforms[index(Segment::Finger(arm))].translation)
                < 1e-3,
            "claw {arm}: the finger left its hinge"
        );
    }
    for transform in &transforms[..RIG] {
        assert!(
            (transform.scale - Vec3::ONE).abs().max_element() < 1e-4,
            "a part is moved, never scaled"
        );
    }
}

#[test]
fn every_joint_holds_through_walking_both_attacks_and_death() {
    let mut motion = Motion::new(Vec3::ZERO, 3, MobAction::Idle);
    let mut z = 0.0;
    for (action, seconds, down) in [
        (MobAction::Chase, 1.0, 0.0),
        (MobAction::Windup, STING_WINDUP, 0.0),
        (MobAction::Recovery, STING_RECOVERY, 0.0),
        (MobAction::Chase, 0.5, 0.0),
        (MobAction::Windup, SWIPE_WINDUP, 0.0),
        (MobAction::Recovery, SWIPE_RECOVERY, 0.0),
        (MobAction::Corpse, 0.7, 1.0),
    ] {
        for _ in 0..(seconds / DT) as usize {
            if action == MobAction::Chase {
                z -= 2.6 * DT;
            }
            motion.sample(
                Vec3::new(0.0, 0.0, z),
                action,
                down,
                FRAME,
                never_solid,
                None,
            );
            assert_joined(&motion.transforms);
        }
    }
}

#[test]
fn the_tips_reach_their_feet_through_the_gait_and_the_gait_runs_on_distance() {
    for phase in (0..24).map(|step| step as f32 * TAU / 24.0) {
        let feet = std::array::from_fn(|leg| rest_foot(leg) + gait_offset(leg, phase));
        let transforms = pose(&Pose {
            feet,
            ..Pose::rest()
        });
        for (leg, foot) in feet.iter().enumerate() {
            let reached = tip(transforms[index(Segment::Lower(leg as u8))], lengths(leg).1);
            assert!(reached.distance(*foot) < 2e-3, "leg {leg} at {phase}");
        }
    }
    let mut motion = Motion::new(Vec3::ZERO, 5, MobAction::Chase);
    let mut position = Vec3::ZERO;
    for _ in 0..60 {
        position.z -= 2.6 * DT;
        motion.sample(position, MobAction::Chase, 0.0, FRAME, never_solid, None);
    }
    let expected = (2.6 * radians_per_block()).rem_euclid(TAU);
    assert!((motion.phase - expected).abs() < 1e-3, "{}", motion.phase);
    assert!(motion.amplitude > 0.9);
    let stopped = motion.phase;
    for _ in 0..60 {
        motion.sample(position, MobAction::Chase, 0.0, FRAME, never_solid, None);
    }
    assert_eq!(motion.phase, stopped, "time alone moves no leg");
    assert!(motion.amplitude < 0.01);
    motion.sample(
        position + Vec3::X * 5.0,
        MobAction::Chase,
        0.0,
        FRAME,
        never_solid,
        None,
    );
    assert_eq!(
        motion.phase, stopped,
        "a jump is a correction, not a stride"
    );
}

/// Runs `action` for `seconds`, calling `each` with the elapsed time after every frame.
fn run(motion: &mut Motion, action: MobAction, seconds: f32, mut each: impl FnMut(f32, &Motion)) {
    for frame in 1..=(seconds / DT).round() as usize {
        motion.sample(Vec3::ZERO, action, 0.0, FRAME, never_solid, None);
        each(frame as f32 * DT, motion);
    }
}

/// When a quantity first gets 90% of the way from `from` to `to`.
fn ninety(trace: &[(f32, f32)], from: f32, to: f32) -> f32 {
    trace
        .iter()
        .find(|(_, value)| (value - from) / (to - from) >= 0.9)
        .map(|(time, _)| *time)
        .expect("never got there")
}

/// **The sting reads.** The tail climbs and draws back across the server's whole 1100 ms
/// telegraph — still rising in its last tenth — and then drives forward in a small fraction of
/// that. A player who watches the tail has most of a second, and the strike itself is not a
/// thing anybody reacts to.
#[test]
fn the_sting_winds_up_over_the_servers_whole_telegraph_and_strikes_in_a_fraction_of_it() {
    let mut motion = Motion::new(Vec3::ZERO, 1, MobAction::Idle);
    run(&mut motion, MobAction::Idle, 0.5, |_, _| {});
    let rest = sting_point(&motion.transforms);

    let mut windup = Vec::new();
    run(
        &mut motion,
        MobAction::Windup,
        STING_WINDUP,
        |time, motion| {
            assert_eq!(motion.attack, Some(Attack::Sting), "the sting always opens");
            windup.push((time, sting_point(&motion.transforms)));
        },
    );
    let cocked = windup.last().unwrap().1;
    assert!(
        cocked.y > rest.y + 0.03,
        "the tail climbs: {rest} -> {cocked}"
    );
    assert!(
        cocked.z > rest.z + 0.03,
        "and draws back: {rest} -> {cocked}"
    );
    let climb: Vec<_> = windup.iter().map(|(t, p)| (*t, p.y)).collect();
    let cocked_at = ninety(&climb, rest.y, cocked.y);
    assert!(
        cocked_at >= 0.7 * STING_WINDUP,
        "the tail is up after {cocked_at}s of a {STING_WINDUP}s telegraph"
    );
    let late = windup
        .iter()
        .find(|(time, _)| *time >= 0.9 * STING_WINDUP)
        .unwrap()
        .1;
    assert!(late.y < cocked.y - 0.002, "still rising in the last tenth");

    let mut strike = Vec::new();
    run(
        &mut motion,
        MobAction::Recovery,
        STING_RECOVERY,
        |time, motion| {
            assert_eq!(motion.attack, Some(Attack::Sting));
            strike.push((time, sting_point(&motion.transforms)));
        },
    );
    let (_, deepest) = strike
        .iter()
        .copied()
        .min_by(|a, b| a.1.z.total_cmp(&b.1.z))
        .unwrap();
    assert!(
        deepest.z < rest.z - 0.1,
        "the sting drives forward: {deepest}"
    );
    assert!(deepest.y < rest.y, "and down: {deepest}");
    let drive: Vec<_> = strike.iter().map(|(t, p)| (*t, p.z)).collect();
    let struck_at = ninety(&drive, cocked.z, deepest.z);
    assert!(struck_at <= 0.25, "the strike took {struck_at}s");
    assert!(
        cocked_at >= 3.0 * struck_at,
        "a {cocked_at}s wind-up against a {struck_at}s strike is not clearly longer"
    );
    // And by the end of the recovery the tail is back where it rests.
    assert!(strike.last().unwrap().1.distance(rest) < 0.05);
}

#[test]
fn the_swipe_opens_the_pincers_wide_and_sweeps_them_shut_leaving_the_tail_alone() {
    let mut motion = Motion::new(Vec3::ZERO, 1, MobAction::Idle);
    run(&mut motion, MobAction::Idle, 0.3, |_, _| {});
    let rest = motion.transforms;
    let gap = |transforms: &[Transform; SEGMENT_COUNT], arm| {
        let (moving, fixed) = fingertips(transforms, arm);
        moving.distance(fixed)
    };
    // Put the sting behind it, so the rhythm has turned to the swipe.
    run(&mut motion, MobAction::Windup, STING_WINDUP, |_, _| {});
    run(&mut motion, MobAction::Recovery, STING_RECOVERY, |_, _| {});
    run(&mut motion, MobAction::Chase, 0.3, |_, _| {});
    run(&mut motion, MobAction::Windup, SWIPE_WINDUP, |_, motion| {
        assert_eq!(motion.attack, Some(Attack::Swipe));
    });
    let open = motion.transforms;
    for arm in 0..2u8 {
        // Which way the claw points, outward positive.
        let heading = |t: &[Transform; SEGMENT_COUNT]| {
            let ahead = t[index(Segment::Claw(arm))].rotation * Vec3::NEG_Z;
            side(usize::from(arm)) * ahead.x.atan2(-ahead.z)
        };
        assert!(gap(&open, arm) > gap(&rest, arm) * 1.6, "claw {arm} opens");
        assert!(
            heading(&open) > heading(&rest) + 0.4,
            "claw {arm} swings out"
        );
    }
    assert!(
        sting_point(&open).distance(sting_point(&rest)) < 0.03,
        "the tail is not part of the swipe"
    );
    let mut innermost = [f32::INFINITY; 2];
    let mut shut = [f32::INFINITY; 2];
    run(&mut motion, MobAction::Recovery, 0.2, |_, motion| {
        for arm in 0..2u8 {
            let x = motion.transforms[index(Segment::Claw(arm))]
                .translation
                .x
                .abs();
            innermost[usize::from(arm)] = innermost[usize::from(arm)].min(x);
            shut[usize::from(arm)] = shut[usize::from(arm)].min(gap(&motion.transforms, arm));
        }
    });
    for arm in 0..2 {
        let rest_x = rest[index(Segment::Claw(arm as u8))].translation.x.abs();
        assert!(innermost[arm] < rest_x - 0.03, "claw {arm} sweeps across");
        assert!(
            shut[arm] <= gap(&rest, arm as u8) + 1e-3,
            "claw {arm} snaps shut"
        );
    }
}

#[test]
fn the_rhythm_is_read_from_the_order_and_corrected_by_the_telegraphs_length() {
    assert_eq!(classify(Attack::Swipe, 0.9, true), Attack::Sting);
    assert_eq!(classify(Attack::Sting, 0.45, true), Attack::Swipe);
    assert_eq!(
        classify(Attack::Sting, 0.3, false),
        Attack::Sting,
        "too little seen"
    );
    assert_eq!(classify(Attack::Swipe, 0.3, false), Attack::Swipe);

    let attack_after = |steps: &[(MobAction, f32)]| {
        let mut motion = Motion::new(Vec3::ZERO, 1, MobAction::Chase);
        for (action, seconds) in steps {
            run(&mut motion, *action, *seconds, |_, _| {});
        }
        motion.attack
    };
    use MobAction::{Chase, Recovery, Windup};
    let sting = [(Windup, STING_WINDUP), (Recovery, STING_RECOVERY)];
    let swipe = [(Windup, SWIPE_WINDUP), (Recovery, SWIPE_RECOVERY)];
    assert_eq!(attack_after(&[(Windup, 0.1)]), Some(Attack::Sting));
    assert_eq!(
        attack_after(&[sting.as_slice(), &[(Chase, 0.2), (Windup, 0.1)]].concat()),
        Some(Attack::Swipe),
        "a swipe follows a sting"
    );
    assert_eq!(
        attack_after(&[sting.as_slice(), &swipe, &[(Windup, 0.1)]].concat()),
        Some(Attack::Sting),
        "and a sting follows the swipe"
    );
    assert_eq!(
        attack_after(&[&sting[..1], &[(Chase, 0.2), (Windup, 0.1)]].concat()),
        Some(Attack::Sting),
        "an abandoned windup turns nothing over"
    );
    // The order said swipe; the telegraph ran past a swipe's length, so it is the sting.
    let mut motion = Motion::new(Vec3::ZERO, 1, MobAction::Chase);
    for (action, seconds) in sting.iter().chain(&[(Chase, 0.2)]) {
        run(&mut motion, *action, *seconds, |_, _| {});
    }
    run(&mut motion, Windup, STING_WINDUP, |time, motion| {
        // The action is counted from the first frame that drew it, so the threshold is met
        // one frame after the wall clock passes it.
        if (time - STING_EVIDENCE).abs() <= DT {
            return;
        }
        let expected = if time < STING_EVIDENCE {
            Attack::Swipe
        } else {
            Attack::Sting
        };
        assert_eq!(motion.attack, Some(expected), "at {time}");
    });
    run(&mut motion, Recovery, 0.2, |_, _| {});
    run(&mut motion, Windup, 0.1, |_, motion| {
        assert_eq!(
            motion.attack,
            Some(Attack::Swipe),
            "corrected: a swipe comes next"
        );
    });
    // A first sight part of the way into a windup takes the opening sting.
    let streamed = Motion::new(Vec3::ZERO, 1, Windup);
    assert_eq!(streamed.attack, Some(Attack::Sting));
}

#[test]
fn a_dead_scorpion_curls_its_legs_lays_its_tail_down_and_settles_on_the_sand() {
    let (w, _) = frame();
    let mut motion = Motion::new(Vec3::ZERO, 1, MobAction::Idle);
    run(&mut motion, MobAction::Idle, 0.2, |_, _| {});
    let (rest_min, rest_max) = extent(&posed(&motion.transforms));
    let rest_sting = sting_point(&motion.transforms);
    for _ in 0..60 {
        motion.sample(Vec3::ZERO, MobAction::Corpse, 1.0, FRAME, never_solid, None);
    }
    let (min, max) = extent(&posed(&motion.transforms));
    assert!(min.y > -0.04 && min.y < 0.06, "lying on the sand: {min}");
    assert!(
        max.y < 0.8 * rest_max.y,
        "the tail sags: {max} against {rest_max}"
    );
    assert!(
        sting_point(&motion.transforms).y < rest_sting.y - 0.06,
        "the sting is down"
    );
    // The corpse stays inside the box the server collides, as the living scorpion does.
    for (axis, low, high) in [("x", min.x, max.x), ("z", min.z, max.z)] {
        assert!(
            low >= -w / 2.0 - 1e-4 && high <= w / 2.0 + 1e-4,
            "{axis}: the corpse spans {low}..{high}, outside the {w}-wide box"
        );
    }
    assert!(
        max.x - min.x < rest_max.x - rest_min.x,
        "the legs are drawn in"
    );
    for leg in 0..LEGS {
        let reached = tip(
            motion.transforms[index(Segment::Lower(leg as u8))],
            lengths(leg).1,
        );
        assert!(
            reached.xz().length() < rest_foot(leg).xz().length(),
            "leg {leg} still reaches out"
        );
    }
}

/// Sand filling every voxel below `y = 0`: a sand floor whose surface is at height zero.
fn sand_floor(voxel: IVec3) -> bool {
    voxel.y < 0
}

/// A buried scorpion's feet: one block under the floor's surface, as the server sends it.
const BURIED: Vec3 = Vec3::new(0.5, -1.0, 0.5);

#[test]
fn buried_is_idle_inside_the_sand_and_nothing_else() {
    for (action, feet, expected) in [
        (MobAction::Idle, BURIED, true),
        (MobAction::Idle, Vec3::new(0.5, 0.0, 0.5), false),
        (MobAction::Recovery, BURIED, false),
        (MobAction::Chase, BURIED, false),
    ] {
        assert_eq!(
            buried(MobKind::Scorpion, action, feet, sand_floor),
            expected,
            "{action:?} at {feet}"
        );
    }
    assert_eq!(surface_above(BURIED, sand_floor), 0.0);
    assert_eq!(surface_above(Vec3::new(0.5, -2.0, 0.5), sand_floor), 0.0);
}

#[test]
fn the_sand_stirs_as_a_player_comes_and_is_all_stirred_where_the_server_raises_it() {
    let (w, _) = frame();
    let risen = Vec3::new(0.5, 0.0, 0.5);
    let at = |distance: f32| stir(risen, risen + Vec3::X * (distance + w / 2.0));
    assert_eq!(at(STIR_RANGE + 0.5), 0.0);
    assert_eq!(at(EMERGE_RANGE), 1.0);
    let mut previous = 0.0;
    for step in 0..=20 {
        let distance = STIR_RANGE - step as f32 * (STIR_RANGE - EMERGE_RANGE) / 20.0;
        let now = at(distance);
        assert!(now >= previous, "stirs less at {distance} than further out");
        previous = now;
    }
}

/// What is drawn of the sand this frame: the mound's transform, and the grains in the air.
fn sand(motion: &Motion) -> (Option<Transform>, Vec<Vec3>) {
    let mound = motion.transforms[index(Segment::Mound)];
    let grains = (0..GRAINS)
        .map(|grain| motion.transforms[index(Segment::Grain(grain as u8))])
        .filter(|grain| *grain != HIDDEN)
        .map(|grain| grain.translation)
        .collect();
    ((mound != HIDDEN).then_some(mound), grains)
}

#[test]
fn a_buried_scorpion_is_a_mound_on_the_surface_that_shifts_only_when_a_player_is_near() {
    let (_, h) = frame();
    let mut motion = Motion::new(BURIED, 9, MobAction::Idle);
    let far = Some(BURIED + Vec3::new(STIR_RANGE + 3.0, 1.0, 0.0));
    let near = Some(BURIED + Vec3::new(EMERGE_RANGE + 1.2, 1.0, 0.0));
    for _ in 0..120 {
        motion.sample(BURIED, MobAction::Idle, 0.0, FRAME, sand_floor, far);
        let (mound, grains) = sand(&motion);
        let mound = mound.expect("a buried scorpion is drawn as its mound");
        assert!(
            (mound.translation.y - 1.0).abs() < 1e-5,
            "the mound sits on the surface, a block over the feet"
        );
        assert_eq!(mound.scale, Vec3::ONE, "nothing near: the sand lies still");
        assert!(grains.is_empty());
        assert!(motion.buried.is_some() && !motion.emerging());
        // The body is inside the sand: nothing of it reaches the surface.
        let (_, top) = extent(&posed(&motion.transforms)[..1]);
        assert!(top.y < 1.0, "the carapace pokes out of the sand: {top}");
    }
    let (mut shivered, mut hopped) = (false, 0);
    for _ in 0..240 {
        motion.sample(BURIED, MobAction::Idle, 0.0, FRAME, sand_floor, near);
        let (mound, grains) = sand(&motion);
        let mound = mound.unwrap();
        shivered |= mound.scale.y > 1.05;
        assert!(
            mound.scale.y < 1.0 + 0.30 + 1e-4,
            "a subtle mound stays subtle"
        );
        for grain in &grains {
            assert!(grain.y >= 1.0 && grain.y < 1.0 + 0.6 * h + 0.3, "{grain}");
        }
        hopped += grains.len();
    }
    assert!(shivered, "a player close by stirs the mound");
    assert!(hopped > 0, "and grains of sand hop off it");
}

#[test]
fn the_scorpion_bursts_up_through_the_sand_over_the_servers_emergence_throwing_grains() {
    let (_, h) = frame();
    let mut motion = Motion::new(BURIED, 9, MobAction::Idle);
    for _ in 0..10 {
        motion.sample(BURIED, MobAction::Idle, 0.0, FRAME, sand_floor, None);
    }
    // The server lifts it onto the surface in `Recovery`; the root arrives over one
    // snapshot's interpolation while the body takes the whole emergence.
    let risen = BURIED + Vec3::Y;
    let centre = |motion: &Motion, root: Vec3| {
        root.y + motion.transforms[index(Segment::Carapace)].translation.y
    };
    let (mut lowest, mut previous) = (f32::INFINITY, f32::NEG_INFINITY);
    let (mut thrown, mut highest_grain) = (0, f32::NEG_INFINITY);
    let mut mound_gone_at = None;
    let frames = (EMERGENCE / DT).round() as usize;
    for frame in 0..frames {
        let root = BURIED.lerp(risen, ((frame + 1) as f32 / 3.0).min(1.0));
        motion.sample(root, MobAction::Recovery, 0.0, FRAME, sand_floor, None);
        assert!(motion.emerging() || frame + 1 == frames, "frame {frame}");
        let y = centre(&motion, root);
        lowest = lowest.min(y);
        assert!(
            y >= previous - 1e-4,
            "it never sinks back: {y} after {previous}"
        );
        previous = y;
        let (mound, grains) = sand(&motion);
        if mound.is_none() && mound_gone_at.is_none() {
            mound_gone_at = Some(frame as f32 * DT);
        }
        thrown += grains.len();
        for grain in grains {
            highest_grain = highest_grain.max(root.y + grain.y);
        }
    }
    assert!(lowest < -0.6 * h, "it starts inside the sand: {lowest}");
    assert!(
        centre(&motion, risen).abs() < 1e-4,
        "and ends standing on it"
    );
    assert!(!motion.emerging() && motion.buried.is_none());
    assert!(
        mound_gone_at.is_some_and(|at| at < 0.6 * EMERGENCE),
        "the mound slumps as it comes up: {mound_gone_at:?}"
    );
    assert!(thrown > GRAINS * 10, "grains are thrown: {thrown}");
    assert!(
        highest_grain > 0.3,
        "and thrown clear of the surface: {highest_grain}"
    );
    // Once up it is an ordinary scorpion: no sand is drawn.
    motion.sample(risen, MobAction::Recovery, 0.0, FRAME, sand_floor, None);
    let (mound, grains) = sand(&motion);
    assert!(mound.is_none() && grains.is_empty());
    for grain in 0..GRAINS {
        assert!(thrown_at_rest(grain), "grain {grain} still in the air");
    }

    // A scorpion first seen already up — streamed in mid-fight — never plays it.
    let mut streamed = Motion::new(risen, 9, MobAction::Recovery);
    streamed.sample(risen, MobAction::Recovery, 0.0, FRAME, sand_floor, None);
    assert!(!streamed.emerging() && streamed.buried.is_none());
}

/// Every grain of a burst has come down by the end of its flight.
fn thrown_at_rest(grain: usize) -> bool {
    thrown(9, grain, GRAIN_TIME).is_none()
}

fn scorpion(entity_id: u64, x: f32, action: MobAction) -> MobState {
    MobState {
        entity_id,
        kind: MobKind::Scorpion,
        pos: [x, 64.0, 0.0],
        vel: [0.0; 3],
        yaw: 0.0,
        health: 72,
        max_health: 72,
        action,
        target_entity_id: 0,
    }
}

fn deliver(app: &mut App, tick: u32, mobs: Vec<MobState>) {
    app.world_mut().resource_mut::<SnapshotInbox>().push(
        crate::net::Snapshot {
            server_tick: tick,
            mobs,
            ..Default::default()
        },
        Instant::now(),
    );
}

type Drawn = Vec<(Segment, Handle<StandardMaterial>)>;

fn drawn(app: &mut App) -> Drawn {
    let world = app.world_mut();
    let mut query = world.query::<(&MobVisual, &MeshMaterial3d<StandardMaterial>)>();
    query
        .iter(world)
        .filter_map(|(visual, material)| match visual.part {
            MobPart::Scorpion(segment) => Some((segment, material.0.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn a_scorpion_in_the_snapshot_is_drawn_by_the_rig_and_flashes_whole_when_hit() {
    let mut app = super::super::tests::headless();
    app.insert_resource(TimeUpdateStrategy::ManualDuration(FRAME));
    deliver(&mut app, 1, vec![scorpion(60, 2.0, MobAction::Chase)]);
    app.update();
    app.update();
    let parts = drawn(&mut app);
    assert_eq!(parts.len(), SEGMENT_COUNT);
    let world = app.world_mut();
    let other = world
        .query::<&MobVisual>()
        .iter(world)
        .filter(|visual| !matches!(visual.part, MobPart::Scorpion(_)))
        .count();
    assert_eq!(other, 0, "no placeholder box is left under a scorpion");

    let mut hurt = scorpion(60, 2.0, MobAction::Chase);
    hurt.health = 40;
    deliver(&mut app, 2, vec![hurt]);
    app.update();
    app.update();
    let flash = app.world().resource::<MobVisuals>().flash_material.clone();
    for (segment, material) in drawn(&mut app) {
        assert_eq!(material, flash, "{segment:?} did not flash");
    }

    deliver(&mut app, 3, Vec::new());
    app.update();
    app.update();
    assert!(drawn(&mut app).is_empty(), "gone with the snapshot");
}

#[test]
fn a_buried_scorpion_in_the_snapshot_shows_only_its_mound() {
    use crate::world::{ChunkStore, VoxelChunk, palette};
    let mut app = super::super::tests::headless();
    app.insert_resource(TimeUpdateStrategy::ManualDuration(FRAME));
    // A sand floor whose surface is at y = 64, and the scorpion one block under it.
    let mut chunk = VoxelChunk::all_air(32);
    for x in 0..32 {
        for z in 0..32 {
            chunk.set(x, 0, z, palette::SAND);
        }
    }
    let mut store = ChunkStore::default();
    store.insert(
        crate::net::ChunkCoord {
            cx: 0,
            cy: 2,
            cz: 0,
        },
        chunk,
    );
    app.insert_resource(store);
    let mut lying = scorpion(61, 4.0, MobAction::Idle);
    lying.pos = [4.5, 64.0, 4.5];
    deliver(&mut app, 1, vec![lying]);
    for _ in 0..3 {
        app.update();
    }
    let world = app.world_mut();
    let mut query = world.query::<(&MobVisual, &Visibility)>();
    let shown: Vec<Segment> = query
        .iter(world)
        .filter_map(|(visual, visibility)| match visual.part {
            MobPart::Scorpion(segment) if *visibility != Visibility::Hidden => Some(segment),
            _ => None,
        })
        .collect();
    assert!(shown.contains(&Segment::Mound), "the mound is drawn");
    assert!(
        !shown
            .iter()
            .any(|segment| matches!(segment, Segment::Grain(_))),
        "no grain is in the air with nobody near"
    );
    assert_eq!(
        shown.len(),
        RIG + 1,
        "the rig lies in the sand under its mound"
    );
}

/// The sand hall on a budget: a dozen scorpions share one set of meshes and one material, so
/// the renderer batches them rather than paying per scorpion, and together they draw no more
/// than one boss's authoring budget — sand included.
#[test]
fn a_hall_of_scorpions_shares_its_meshes_and_stays_on_budget() {
    let mut app = super::super::tests::headless();
    app.insert_resource(TimeUpdateStrategy::ManualDuration(FRAME));
    let hall = |tick: u32| {
        (0..12)
            .map(|index| {
                let mut one = scorpion(60 + index, index as f32 * 2.0, MobAction::Chase);
                one.pos[2] = tick as f32 * 0.05;
                one
            })
            .collect::<Vec<_>>()
    };
    for tick in 1..=10 {
        deliver(&mut app, tick, hall(tick));
        for _ in 0..3 {
            app.update();
        }
    }
    let world = app.world_mut();
    let mut query = world.query::<(&MobVisual, &Mesh3d, &MeshMaterial3d<StandardMaterial>)>();
    let parts: Vec<_> = query
        .iter(world)
        .filter(|(visual, _, _)| matches!(visual.part, MobPart::Scorpion(_)))
        .map(|(_, mesh, material)| (mesh.0.id(), material.0.id()))
        .collect();
    assert_eq!(parts.len(), 12 * SEGMENT_COUNT);
    let meshes: std::collections::HashSet<_> = parts.iter().map(|part| part.0).collect();
    let materials: std::collections::HashSet<_> = parts.iter().map(|part| part.1).collect();
    assert_eq!(
        meshes.len(),
        SEGMENT_COUNT - GRAINS + 1,
        "one mesh set, one grain mesh"
    );
    assert_eq!(
        materials.len(),
        1,
        "one chitin-and-sand material, whatever the count"
    );
    let assets = app.world().resource::<Assets<Mesh>>();
    let triangles = |id| {
        assets
            .get(id)
            .and_then(|mesh: &Mesh| mesh.indices())
            .map_or(0, |indices| indices.len() / 3)
    };
    let per_scorpion: usize = (0..SEGMENT_COUNT)
        .map(|part| {
            let segment = SEGMENTS[part];
            let visuals = app.world().resource::<MobVisuals>();
            let parts = visuals.scorpion.scorpion_parts.as_ref().unwrap();
            triangles(parts[index(segment)].1.id())
        })
        .sum();
    assert!(
        12 * per_scorpion <= 12_000,
        "twelve scorpions draw {} triangles",
        12 * per_scorpion
    );
}
