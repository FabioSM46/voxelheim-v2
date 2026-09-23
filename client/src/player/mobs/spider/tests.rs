//! The rig's proportions, its gait, its poses and its emergence, measured rather than
//! described. No window, no display and no GPU.
use super::*;
use crate::net::{MobState, SnapshotInbox};
use bevy::time::TimeUpdateStrategy;

const FRAME: Duration = Duration::from_micros(16_667);

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

fn triangles(mesh: &Mesh) -> usize {
    mesh.indices().map_or(0, |indices| indices.len() / 3)
}

/// The far end of a bone drawn with `transform` from a mesh `length` long along +X.
fn tip(transform: Transform, length: f32) -> Vec3 {
    transform.transform_point(Vec3::X * length)
}

fn never_solid(_: IVec3) -> bool {
    false
}

#[test]
fn the_rest_pose_fills_the_box_the_server_collides_and_stands_on_its_feet() {
    let (w, h) = frame();
    let (min, max) = extent(&posed_meshes(&Pose::rest()));
    let half = w / 2.0;
    assert!(
        min.y > -1e-3 && min.y < 0.01,
        "the tips stand on the ground: {min}"
    );
    assert!(
        max.y <= h + 1e-4 && max.y >= 0.93 * h,
        "knees reach the top: {max}"
    );
    for (axis, low, high) in [("x", min.x, max.x), ("z", min.z, max.z)] {
        assert!(
            low >= -half - 1e-4 && high <= half + 1e-4,
            "{axis}: {low}..{high} leaves the {w}-wide box"
        );
        assert!(
            high - low >= 0.82 * w,
            "{axis}: a spider {} across does not fill its {w}-wide box",
            high - low
        );
    }
    assert!(
        (min.x + max.x).abs() < 1e-4,
        "left and right mirror each other"
    );
}

#[test]
fn a_spider_is_a_small_body_on_long_legs() {
    let (w, h) = frame();
    let meshes = meshes();
    let size = |segment| {
        let (min, max) = extent(&[meshes[index(segment)].1.clone()]);
        max - min
    };
    let (cephalothorax, abdomen) = (size(Segment::Cephalothorax), size(Segment::Abdomen));
    // The abdomen is the larger of the two body parts, and the pair spans well under half
    // the leg span: what reads as a spider rather than as a beetle or a crab.
    assert!(
        abdomen.x * abdomen.y * abdomen.z > cephalothorax.x * cephalothorax.y * cephalothorax.z
    );
    assert!(abdomen.x < 0.5 * w && cephalothorax.x < 0.5 * w);
    for leg in 0..LEGS {
        let (upper, lower) = lengths(leg);
        assert!(
            upper > 0.2 * h && lower > upper,
            "leg {leg}: {upper} then {lower}"
        );
        let across = extent(&[meshes[index(Segment::Upper(leg as u8))].1.clone()]);
        assert!(
            (across.1.x - across.0.x - upper).abs() < 1e-4,
            "leg {leg}'s femur mesh is the length the knee is solved for"
        );
    }
    // Eight eyes of 24 vertices each, two fangs; eight legs of two segments.
    assert_eq!(
        meshes[index(Segment::Eyes)].1.count_vertices(),
        8 * 24,
        "an eight-eyed cluster"
    );
    assert_eq!(meshes[index(Segment::Fangs)].1.count_vertices(), 2 * 24);
    assert_eq!(
        SEGMENTS
            .iter()
            .filter(|segment| matches!(segment, Segment::Upper(_) | Segment::Lower(_)))
            .count(),
        16
    );
    for (position, segment) in SEGMENTS.iter().enumerate() {
        assert_eq!(index(*segment), position);
    }
}

#[test]
fn every_leg_joins_hip_knee_and_tip_without_stretching_in_every_pose() {
    let mut motion = Motion::new(Vec3::ZERO, 3, false);
    let mut poses = vec![pose(&Pose::rest())];
    let actions = [
        MobAction::Chase,
        MobAction::Windup,
        MobAction::Recovery,
        MobAction::Chase,
    ];
    for (step, action) in actions.into_iter().enumerate() {
        for frame in 0..40 {
            let z = -((step * 40 + frame) as f32) * 0.06;
            motion.sample(Vec3::new(0.0, 0.0, z), 0.0, action, 0.0, FRAME, never_solid);
            poses.push(motion.transforms);
        }
    }
    for transforms in poses {
        for leg in 0..LEGS {
            let (upper, lower) = lengths(leg);
            let femur = transforms[index(Segment::Upper(leg as u8))];
            let tibia = transforms[index(Segment::Lower(leg as u8))];
            assert!(
                tip(femur, upper).distance(tibia.translation) < 1e-3,
                "leg {leg}: the knee came apart"
            );
            let body = transforms[index(Segment::Cephalothorax)];
            assert!(
                body.transform_point(hip(leg)).distance(femur.translation) < 1e-3,
                "leg {leg}: the femur left the hip"
            );
            assert!(
                tip(tibia, lower).y > -0.02,
                "leg {leg}: a tip went through the floor"
            );
            assert!(
                (femur.scale - Vec3::ONE).abs().max_element() < 1e-4
                    && (tibia.scale - Vec3::ONE).abs().max_element() < 1e-4,
                "a bone is rotated, never scaled"
            );
        }
    }
}

#[test]
fn the_tips_reach_their_feet_at_rest_and_through_the_gait() {
    for phase in (0..24).map(|step| step as f32 * TAU / 24.0) {
        let feet = std::array::from_fn(|leg| rest_foot(leg) + gait_offset(leg, phase));
        let transforms = pose(&Pose {
            feet,
            ..Pose::rest()
        });
        for (leg, foot) in feet.iter().enumerate() {
            let (_, lower) = lengths(leg);
            let reached = tip(transforms[index(Segment::Lower(leg as u8))], lower);
            assert!(
                reached.distance(*foot) < 2e-3,
                "leg {leg} at phase {phase}: {reached} for {foot}"
            );
        }
    }
}

#[test]
fn an_alternating_tetrapod_keeps_four_feet_down_and_planted() {
    let (w, _) = frame();
    for leg in 0..LEGS {
        let opposite = if leg.is_multiple_of(2) {
            leg + 1
        } else {
            leg - 1
        };
        assert_ne!(group(leg), group(opposite), "a pair never steps together");
        if leg + 2 < LEGS {
            assert_ne!(
                group(leg),
                group(leg + 2),
                "neighbours on one side alternate"
            );
        }
    }
    let per_block = radians_per_block();
    for step in 0..48 {
        let phase = step as f32 * TAU / 48.0;
        let down: Vec<_> = (0..LEGS)
            .filter(|&leg| gait_offset(leg, phase).y == 0.0)
            .collect();
        assert!(
            down.len() >= 4,
            "phase {phase}: only {down:?} on the ground"
        );
        // A planted tip moves backwards under the body exactly as far as the body went
        // forwards, so on the ground it does not slide.
        let travel = 0.01;
        let next = phase + travel * per_block;
        for leg in down {
            let (a, b) = (gait_offset(leg, phase), gait_offset(leg, next));
            if b.y == 0.0 && (phase / PI).floor() == (next / PI).floor() {
                assert!(
                    ((b.z - a.z) - travel).abs() < 1e-4,
                    "leg {leg} slides: {} for {travel}",
                    b.z - a.z
                );
            }
        }
        // And no tip leaves the box, however far into the stride.
        for leg in 0..LEGS {
            let foot = rest_foot(leg) + gait_offset(leg, phase);
            assert!(foot.z.abs() + TIBIA * w <= w / 2.0, "leg {leg}: {foot}");
        }
    }
}

#[test]
fn the_gait_runs_on_distance_travelled_and_nothing_else() {
    let mut motion = Motion::new(Vec3::ZERO, 5, false);
    let mut position = Vec3::ZERO;
    for _ in 0..30 {
        position.z -= 0.05;
        motion.sample(position, 0.0, MobAction::Chase, 0.0, FRAME, never_solid);
    }
    let expected = (1.5 * radians_per_block()).rem_euclid(TAU);
    assert!(
        (motion.phase - expected).abs() < 1e-3,
        "{} after 1.5 blocks, want {expected}",
        motion.phase
    );
    assert!(motion.amplitude > 0.9 && motion.speed > 2.0);

    // Twice as fast is the same phase for the same distance, in half the frames.
    let mut fast = Motion::new(Vec3::ZERO, 5, false);
    let mut position = Vec3::ZERO;
    for _ in 0..15 {
        position.z -= 0.10;
        fast.sample(position, 0.0, MobAction::Chase, 0.0, FRAME, never_solid);
    }
    assert!((fast.phase - expected).abs() < 1e-3);
    assert!(fast.speed > motion.speed * 1.5);

    // Time alone moves nothing, a correction moves nothing, and the stop eases out
    // rather than snapping the legs home.
    let position = Vec3::new(0.0, 0.0, -1.5);
    motion.sample(position, 0.0, MobAction::Chase, 0.0, FRAME, never_solid);
    let stopped = motion.phase;
    let mut previous = motion.transforms;
    for _ in 0..60 {
        motion.sample(position, 0.0, MobAction::Chase, 0.0, FRAME, never_solid);
        for (now, before) in motion.transforms.iter().zip(previous) {
            assert!(now.translation.distance(before.translation) < 0.05);
        }
        previous = motion.transforms;
    }
    assert_eq!(motion.phase, stopped);
    assert!(motion.amplitude < 0.01);
    motion.sample(
        position + Vec3::X * 5.0,
        0.0,
        MobAction::Chase,
        0.0,
        FRAME,
        never_solid,
    );
    assert_eq!(
        motion.phase, stopped,
        "a jump is a correction, not a stride"
    );
}

#[test]
fn the_front_legs_rear_on_windup_and_the_body_lunges_on_recovery() {
    let (_, h) = frame();
    let settle = |action| {
        let mut motion = Motion::new(Vec3::ZERO, 1, false);
        for _ in 0..40 {
            motion.sample(Vec3::ZERO, 0.0, action, 0.0, FRAME, never_solid);
        }
        motion.transforms
    };
    let idle = settle(MobAction::Idle);
    let windup = settle(MobAction::Windup);
    let recovery = settle(MobAction::Recovery);
    let front = |pose: &[Transform; SEGMENT_COUNT], leg: usize| {
        tip(pose[index(Segment::Lower(leg as u8))], lengths(leg).1)
    };
    for leg in [0, 1] {
        assert!(front(&windup, leg).y > front(&idle, leg).y + 0.3 * h);
    }
    for leg in 2..LEGS {
        assert!(
            front(&windup, leg).distance(front(&idle, leg)) < 1e-3,
            "only the front pair rises"
        );
    }
    let body = |pose: &[Transform; SEGMENT_COUNT]| {
        pose[index(Segment::Cephalothorax)].transform_point(Vec3::new(
            0.0,
            0.42 * h,
            -0.13 * frame().0,
        ))
    };
    let fang_tip = |pose: &[Transform; SEGMENT_COUNT]| {
        pose[index(Segment::Fangs)].transform_point(Vec3::new(0.0, 0.23 * h, -0.285 * frame().0))
    };
    assert!(
        body(&recovery).z < body(&idle).z - 0.05,
        "the lunge goes forward"
    );
    assert!(body(&windup).z > body(&idle).z, "the windup draws back");
    assert!(
        fang_tip(&windup).z < fang_tip(&idle).z,
        "the fangs open forward"
    );
}

#[test]
fn a_dead_spider_curls_its_legs_in_and_settles_on_the_ground() {
    let (w, _) = frame();
    let mut motion = Motion::new(Vec3::ZERO, 1, false);
    motion.sample(Vec3::ZERO, 0.0, MobAction::Corpse, 1.0, FRAME, never_solid);
    let meshes: Vec<Mesh> = meshes()
        .into_iter()
        .zip(motion.transforms)
        .map(|((_, mesh), transform)| mesh.transformed_by(transform))
        .collect();
    let (min, max) = extent(&meshes);
    let (rest_min, rest_max) = extent(&posed_meshes(&Pose::rest()));
    assert!(min.y > -0.03 && min.y < 0.06, "lying on the ground: {min}");
    assert!(
        max.x - min.x < 0.75 * (rest_max.x - rest_min.x),
        "legs drawn in"
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
    assert!(
        max.x <= w / 2.0 && max.z <= w / 2.0,
        "curled in, the body is smaller than its box: {max}"
    );
}

#[test]
fn a_standing_spider_twitches_one_leg_at_a_time_and_a_crowd_out_of_step() {
    let mut seen = std::collections::HashSet::new();
    for seed in 0..40 {
        let twitches: Vec<_> = (0..600)
            .filter_map(|frame| twitch(frame as f32 / 60.0, seed))
            .collect();
        assert!(
            !twitches.is_empty(),
            "spider {seed} never twitched in ten seconds"
        );
        assert!(twitches.len() < 200, "spider {seed} twitches constantly");
        assert!(
            twitches
                .iter()
                .all(|(leg, amount)| *leg < LEGS && *amount >= 0.0)
        );
        seen.insert(twitch(0.1, seed).map(|(leg, _)| leg));
    }
    assert!(seen.len() > 3, "every spider twitches the same leg");

    // The twitch is a standing pose: a running spider's legs belong to the gait.
    let mut running = Motion::new(Vec3::ZERO, 2, false);
    let mut position = Vec3::ZERO;
    let mut reference = Motion::new(Vec3::ZERO, 99, false);
    for _ in 0..120 {
        position.z -= 0.08;
        running.sample(position, 0.0, MobAction::Chase, 0.0, FRAME, never_solid);
        reference.sample(position, 0.0, MobAction::Chase, 0.0, FRAME, never_solid);
    }
    for (a, b) in running.transforms.iter().zip(reference.transforms) {
        assert!(a.translation.distance(b.translation) < 0.03);
    }
}

/// A wall filling every voxel at `x >= 1`.
fn wall_east(voxel: IVec3) -> bool {
    voxel.x >= 1
}

#[test]
fn a_burrow_is_found_behind_the_wall_and_nowhere_in_the_open() {
    let back = burrow(Vec3::new(0.5, 0.0, 0.5), wall_east).expect("a wall to the east");
    assert!(back.x > 0.9, "the burrow lies east, into the wall: {back}");
    assert_eq!(burrow(Vec3::new(0.5, 0.0, 0.5), never_solid), None);
    // Closed in evenly on every side is a pit, not a burrow: no side to come out of.
    let pit = |voxel: IVec3| voxel.x != 0 || voxel.z != 0;
    assert_eq!(burrow(Vec3::new(0.5, 0.0, 0.5), pit), None);
}

#[test]
fn a_fresh_spider_crawls_out_of_its_wall_and_a_streamed_one_does_not() {
    let (w, _) = frame();
    let at = Vec3::new(0.5, 0.0, 0.5);
    let centre = |motion: &Motion| motion.transforms[index(Segment::Cephalothorax)].translation;
    // Facing west, away from the wall, so the root frame's +Z is world east.
    let yaw = FRAC_PI_2;
    let mut fresh = Motion::new(at, 4, true);
    fresh.sample(at, yaw, MobAction::Idle, 0.0, FRAME, wall_east);
    let first = centre(&fresh);
    let world = Quat::from_rotation_y(yaw) * first;
    assert!(world.x > 0.5 * w, "starts back inside the burrow: {world}");
    assert!(fresh.emerging());
    let mut previous = world.x;
    let frames = (EMERGE_TIME / FRAME.as_secs_f32()).ceil() as usize;
    for _ in 0..frames {
        fresh.sample(at, yaw, MobAction::Idle, 0.0, FRAME, wall_east);
        let x = (Quat::from_rotation_y(yaw) * centre(&fresh)).x;
        assert!(x <= previous + 1e-5, "it never goes back in");
        previous = x;
    }
    assert!(!fresh.emerging());
    assert!(
        centre(&fresh).length() < 1e-4,
        "out, and standing where the server put it"
    );

    // Not fresh — wounded, dead, or already doing something when first seen — and the wall
    // makes no difference.
    let mut streamed = Motion::new(at, 4, false);
    streamed.sample(at, yaw, MobAction::Idle, 0.0, FRAME, wall_east);
    assert!(!streamed.emerging());
    assert!(centre(&streamed).length() < 1e-4);
}

fn spider(entity_id: u64, x: f32, z: f32, action: MobAction) -> MobState {
    MobState {
        entity_id,
        kind: MobKind::CaveSpider,
        pos: [x, 64.0, z],
        vel: [0.0; 3],
        yaw: 0.0,
        health: 12,
        max_health: 12,
        action,
        target_entity_id: 0,
    }
}

fn app() -> App {
    let mut app = super::super::tests::headless();
    app.insert_resource(TimeUpdateStrategy::ManualDuration(FRAME));
    app
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

/// The spider parts drawn, by owner: segment, mesh, material.
type Drawn = Vec<(Entity, Segment, Handle<Mesh>, Handle<StandardMaterial>)>;

fn drawn(app: &mut App) -> Drawn {
    let world = app.world_mut();
    let mut query = world.query::<(&MobVisual, &Mesh3d, &MeshMaterial3d<StandardMaterial>)>();
    query
        .iter(world)
        .filter_map(|(visual, mesh, material)| match visual.part {
            MobPart::Spider(segment) => {
                Some((visual.owner, segment, mesh.0.clone(), material.0.clone()))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_cave_spider_in_the_snapshot_is_drawn_by_the_rig_and_not_a_box() {
    let mut app = app();
    deliver(&mut app, 1, vec![spider(40, 2.0, 0.0, MobAction::Idle)]);
    app.update();
    app.update();
    let parts = drawn(&mut app);
    assert_eq!(parts.len(), SEGMENT_COUNT);
    let visuals = app.world().resource::<MobVisuals>();
    let eyes = visuals.cave_spider.eyes.as_ref().unwrap().material.clone();
    for (_, segment, _, material) in &parts {
        assert_eq!(
            *material == eyes,
            *segment == Segment::Eyes,
            "only the eyes glow"
        );
    }
    let world = app.world_mut();
    let other = world
        .query::<&MobVisual>()
        .iter(world)
        .filter(|visual| !matches!(visual.part, MobPart::Spider(_)))
        .count();
    assert_eq!(other, 0, "no placeholder box is left under a spider");

    deliver(&mut app, 2, Vec::new());
    app.update();
    app.update();
    assert!(drawn(&mut app).is_empty(), "gone with the snapshot");
}

#[test]
fn a_hit_spider_flashes_every_part_but_its_eyes() {
    let mut app = app();
    deliver(&mut app, 1, vec![spider(40, 2.0, 0.0, MobAction::Chase)]);
    app.update();
    let mut hurt = spider(40, 2.0, 0.0, MobAction::Chase);
    hurt.health = 5;
    deliver(&mut app, 2, vec![hurt]);
    app.update();
    app.update();
    let flash = app.world().resource::<MobVisuals>().flash_material.clone();
    let parts = drawn(&mut app);
    for (_, segment, _, material) in parts {
        assert_eq!(material == flash, segment != Segment::Eyes, "{segment:?}");
    }
}

/// The horde the dungeon's waves send, on a budget: thirty spiders share one set of meshes
/// and two materials, so the renderer batches them rather than paying per spider, and what
/// they draw together stays under what one boss is allowed.
#[test]
fn thirty_spiders_share_their_meshes_and_materials_and_stay_on_budget() {
    let mut app = app();
    let horde = |tick: u32| {
        (0..30)
            .map(|index| {
                let angle = index as f32 * TAU / 30.0 + tick as f32 * 0.02;
                spider(
                    100 + index,
                    6.0 * angle.cos(),
                    6.0 * angle.sin(),
                    MobAction::Chase,
                )
            })
            .collect::<Vec<_>>()
    };
    for tick in 1..=20 {
        deliver(&mut app, tick, horde(tick));
        for _ in 0..3 {
            app.update();
        }
    }
    let parts = drawn(&mut app);
    assert_eq!(parts.len(), 30 * SEGMENT_COUNT);
    let meshes: std::collections::HashSet<_> = parts.iter().map(|part| part.2.id()).collect();
    let materials: std::collections::HashSet<_> = parts.iter().map(|part| part.3.id()).collect();
    assert_eq!(
        meshes.len(),
        SEGMENT_COUNT,
        "one mesh set shared by the whole horde"
    );
    assert_eq!(materials.len(), 2, "chitin and eyes, whatever the count");
    let assets = app.world().resource::<Assets<Mesh>>();
    let per_spider: usize = meshes
        .iter()
        .map(|id| triangles(assets.get(*id).expect("a live mesh")))
        .sum();
    // One boss may draw 12,000 triangles (the arena's authoring budget); a wave of thirty
    // spiders is held to two bosses' worth. What that costs a frame on a real adapter is
    // `measure_a_spider_horde_in_the_shipped_chamber`, beside the bosses' own measurement.
    assert!(
        30 * per_spider <= 24_000,
        "thirty spiders draw {} triangles",
        30 * per_spider
    );
    // And every one of them is being animated: no two sit in the same pose.
    let world = app.world_mut();
    let mut roots = world.query::<(&Mob, &Transform)>();
    let running = roots
        .iter(world)
        .filter(|(mob, _)| {
            mob.spider_motion
                .as_ref()
                .is_some_and(|motion| motion.amplitude > 0.5)
        })
        .count();
    assert_eq!(running, 30, "the whole horde scuttles");
}
