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
    for transform in transforms {
        assert!(
            (transform.scale - Vec3::ONE).abs().max_element() < 1e-4,
            "a part is moved, never scaled"
        );
    }
}

#[test]
fn every_joint_holds_through_walking_the_server_s_actions_and_death() {
    let mut motion = Motion::new(Vec3::ZERO, 3);
    let mut z = 0.0;
    for (action, seconds, down) in [
        (MobAction::Chase, 1.0, 0.0),
        (MobAction::Windup, 1.1, 0.0),
        (MobAction::Recovery, 1.4, 0.0),
        (MobAction::Chase, 0.5, 0.0),
        (MobAction::Windup, 0.45, 0.0),
        (MobAction::Recovery, 0.7, 0.0),
        (MobAction::Corpse, 0.7, 1.0),
    ] {
        for _ in 0..(seconds / DT) as usize {
            if action == MobAction::Chase {
                z -= 2.6 * DT;
            }
            motion.sample(Vec3::new(0.0, 0.0, z), action, down, FRAME);
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
    let mut motion = Motion::new(Vec3::ZERO, 5);
    let mut position = Vec3::ZERO;
    for _ in 0..60 {
        position.z -= 2.6 * DT;
        motion.sample(position, MobAction::Chase, 0.0, FRAME);
    }
    let expected = (2.6 * radians_per_block()).rem_euclid(TAU);
    assert!((motion.phase - expected).abs() < 1e-3, "{}", motion.phase);
    assert!(motion.amplitude > 0.9);
    let stopped = motion.phase;
    for _ in 0..60 {
        motion.sample(position, MobAction::Chase, 0.0, FRAME);
    }
    assert_eq!(motion.phase, stopped, "time alone moves no leg");
    assert!(motion.amplitude < 0.01);
    motion.sample(position + Vec3::X * 5.0, MobAction::Chase, 0.0, FRAME);
    assert_eq!(
        motion.phase, stopped,
        "a jump is a correction, not a stride"
    );
}

/// Runs `action` for `seconds`, calling `each` with the elapsed time after every frame.
fn run(motion: &mut Motion, action: MobAction, seconds: f32, mut each: impl FnMut(f32, &Motion)) {
    for frame in 1..=(seconds / DT).round() as usize {
        motion.sample(Vec3::ZERO, action, 0.0, FRAME);
        each(frame as f32 * DT, motion);
    }
}

#[test]
fn a_dead_scorpion_curls_its_legs_lays_its_tail_down_and_settles_on_the_sand() {
    let (w, _) = frame();
    let mut motion = Motion::new(Vec3::ZERO, 1);
    run(&mut motion, MobAction::Idle, 0.2, |_, _| {});
    let (rest_min, rest_max) = extent(&posed(&motion.transforms));
    let rest_sting = sting_point(&motion.transforms);
    for _ in 0..60 {
        motion.sample(Vec3::ZERO, MobAction::Corpse, 1.0, FRAME);
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
