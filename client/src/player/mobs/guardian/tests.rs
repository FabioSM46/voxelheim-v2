use super::*;

#[test]
fn guardian_has_seventeen_bounded_meshes_and_preserves_rest_pose() {
    let all = meshes();
    assert_eq!(all.len(), 17);
    let mut triangles = 0;
    for (segment, mesh) in all {
        triangles += mesh.indices().unwrap().len() / 3;
        let bevy::mesh::VertexAttributeValues::Float32x3(points) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap()
        else {
            panic!("positions")
        };
        let bound = bounds(segment);
        let lo = bound.iter().copied().reduce(Vec3::min).unwrap();
        let hi = bound.iter().copied().reduce(Vec3::max).unwrap();
        for point in points {
            let p = Vec3::from_array(*point);
            assert!(
                p.cmpge(lo - Vec3::splat(0.00001)).all()
                    && p.cmple(hi + Vec3::splat(0.00001)).all(),
                "{segment:?} {p}"
            );
            assert!(p.x.abs() <= 0.8 && p.z.abs() <= 0.8 && p.y >= 0.0 && p.y <= 1.8);
        }
    }
    assert!(triangles < 12000);
    let rest = pose(
        std::array::from_fn(rest_foot),
        MobAction::Idle,
        Duration::ZERO,
        0.0,
    );
    assert!(
        rest.iter()
            .all(|t| t.to_matrix().abs_diff_eq(Mat4::IDENTITY, 0.00001))
    );
}

fn vertices(mesh: &Mesh) -> &[[f32; 3]] {
    let bevy::mesh::VertexAttributeValues::Float32x3(points) =
        mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap()
    else {
        panic!("positions")
    };
    points
}

#[test]
fn guardian_segmentation_preserves_the_accepted_exterior_and_colours() {
    // The old three-mesh rig is retained only as this historical art fixture.
    // Compare exterior rays, not mesh counts: an internal joint face is harmless,
    // but widening a shoulder, losing a fang or recolouring the face is not.
    fn boxes_in(meshes: Vec<Mesh>) -> Vec<(Vec3, Vec3, [f32; 4])> {
        meshes
            .into_iter()
            .flat_map(|mesh| {
                let bevy::mesh::VertexAttributeValues::Float32x4(colours) =
                    mesh.attribute(Mesh::ATTRIBUTE_COLOR).unwrap()
                else {
                    panic!("colours")
                };
                vertices(&mesh)
                    .chunks_exact(24)
                    .enumerate()
                    .map(|(index, points)| {
                        let lo = points
                            .iter()
                            .copied()
                            .map(Vec3::from_array)
                            .reduce(Vec3::min)
                            .unwrap();
                        let hi = points
                            .iter()
                            .copied()
                            .map(Vec3::from_array)
                            .reduce(Vec3::max)
                            .unwrap();
                        (lo, hi, colours[index * 24])
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }
    let before = boxes_in(vec![
        bosses::guardian_body(),
        bosses::guardian_head(),
        bosses::guardian_legs(),
    ]);
    let after = boxes_in(meshes().into_iter().map(|(_, m)| m).collect());
    for axis in 0..3 {
        for positive in [false, true] {
            for a in 0..180 {
                for b in 0..180 {
                    let mut ray = Vec3::ZERO;
                    ray[(axis + 1) % 3] = -0.81 + a as f32 * 0.0157;
                    ray[(axis + 2) % 3] = -0.81 + b as f32 * 0.0157;
                    let cast = |boxes: &[(Vec3, Vec3, [f32; 4])]| {
                        boxes
                            .iter()
                            .filter(|(lo, hi, _)| {
                                (0..3)
                                    .filter(|&i| i != axis)
                                    .all(|i| ray[i] > lo[i] + 0.000001 && ray[i] < hi[i] - 0.000001)
                            })
                            .map(|(lo, hi, c)| (if positive { hi[axis] } else { -lo[axis] }, *c))
                            .max_by(|a, b| a.0.total_cmp(&b.0))
                    };
                    match (cast(&before), cast(&after)) {
                        (None, None) => (),
                        (Some((a, ac)), Some((b, bc))) => {
                            assert!((a - b).abs() < 0.00001, "silhouette axis {axis}");
                            assert_eq!(ac, bc, "palette axis {axis}");
                        }
                        _ => panic!("silhouette ray missed on axis {axis}"),
                    }
                }
            }
        }
    }
}

#[test]
fn guardian_support_contacts_stay_fixed_while_the_body_walks_and_turns() {
    let mut motion = Motion::new(Vec3::ZERO, 0.0);
    let mut position = Vec3::ZERO;
    let mut yaw = 0.0;
    let mut exchanges = 0;
    for frame in 0..360 {
        let previous = motion.feet;
        position.z -= if frame < 120 { 0.012 } else { 4.3 / 60.0 };
        if frame > 120 {
            yaw += 0.003;
        }
        motion.sample(
            position,
            yaw,
            MobAction::Chase,
            Duration::from_secs_f32(frame as f32 / 60.0),
            0.0,
            Duration::from_secs_f32(1.0 / 60.0),
        );
        let swinging = motion.feet.iter().filter(|f| f.swing < 1.0).count();
        assert!(swinging <= 2, "no simultaneous four-foot glide");
        for (old, new) in previous.iter().zip(motion.feet) {
            if old.swing == 1.0 && new.swing == 1.0 {
                assert!(
                    old.contact.abs_diff_eq(new.contact, 0.00001),
                    "planted support moved"
                );
            }
            if old.swing == 1.0 && new.swing < 1.0 {
                exchanges += 1;
            }
        }
        let matrices = motion.transforms;
        for leg in 0..4 {
            let upper = matrices[5 + leg * 2];
            let lower = matrices[6 + leg * 2];
            let knee = rest_foot(leg) + Vec3::Y * 0.57;
            assert!(
                upper
                    .transform_point(knee)
                    .distance(lower.transform_point(knee))
                    < 0.003,
                "detached knee {leg}"
            );
            let contact =
                position + Quat::from_rotation_y(yaw) * lower.transform_point(rest_foot(leg));
            assert!(
                contact
                    .xz()
                    .abs_diff_eq(motion.feet[leg].contact.xz(), 0.00001),
                "rendered support slid"
            );
        }
        for (index, segment) in SEGMENTS.iter().enumerate() {
            if limb(*segment).is_some_and(|(_, lower)| lower) {
                let mesh = boxes(&geometry(*segment));
                let lowest = vertices(&mesh)
                    .iter()
                    .map(|p| matrices[index].transform_point(Vec3::from_array(*p)).y)
                    .fold(f32::INFINITY, f32::min);
                assert!(
                    lowest >= -0.00001,
                    "{segment:?} sole penetrates floor: {lowest}"
                );
            }
        }
    }
    assert!(exchanges > 12, "the body moved without taking steps");
    for _ in 0..120 {
        motion.sample(
            position,
            yaw,
            MobAction::Idle,
            Duration::ZERO,
            0.0,
            Duration::from_secs_f32(1.0 / 60.0),
        );
    }
    assert!(motion.feet.iter().all(|f| f.swing == 1.0));
}

#[test]
fn guardian_correction_and_combat_discard_stale_support_without_stretching() {
    let mut motion = Motion::new(Vec3::ZERO, 0.0);
    motion.sample(
        Vec3::new(0.0, 0.0, -0.2),
        0.0,
        MobAction::Chase,
        Duration::ZERO,
        0.0,
        Duration::from_millis(16),
    );
    for (pos, yaw, action) in [
        (Vec3::new(20.0, 4.0, 0.0), 1.6, MobAction::Chase),
        (Vec3::new(20.0, 4.0, -0.1), 1.6, MobAction::Windup),
    ] {
        motion.sample(
            pos,
            yaw,
            action,
            Duration::ZERO,
            0.0,
            Duration::from_millis(16),
        );
        for (i, foot) in motion.feet.iter().enumerate() {
            assert!(
                foot.contact
                    .abs_diff_eq(pos + Quat::from_rotation_y(yaw) * rest_foot(i), 0.00001)
            );
        }
    }
}

#[test]
fn guardian_death_articulates_limbs_keeps_the_floor_and_freezes_secondary_motion() {
    for step in 1..=20 {
        let down = step as f32 / 20.0;
        let transforms = pose(
            std::array::from_fn(rest_foot),
            MobAction::Corpse,
            Duration::from_secs(1),
            down,
        );
        let later = pose(
            std::array::from_fn(rest_foot),
            MobAction::Corpse,
            Duration::from_secs(10),
            down,
        );
        assert_eq!(transforms, later, "corpse secondary motion continued");
        let mut floor = f32::INFINITY;
        for ((_, mesh), transform) in meshes().into_iter().zip(transforms) {
            for p in vertices(&mesh) {
                floor = floor.min(transform.transform_point(Vec3::from_array(*p)).y);
            }
        }
        assert!(
            (0.0..0.08).contains(&(floor + 0.00001)),
            "corpse floor {floor}"
        );
        assert_ne!(
            transforms[5].rotation, transforms[6].rotation,
            "upper/lower leg never articulate"
        );
        assert!(
            transforms
                .iter()
                .all(|t| t.scale.abs_diff_eq(Vec3::ONE, 0.00001)),
            "rig was stretched"
        );
    }
}
