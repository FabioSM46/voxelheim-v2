//! The king's articulated funeral armour. Joint transforms are cosmetic: the
//! snapshot still owns the root, action, health, death and disappearance.
use super::*;
use bosses::boxes;

const IRON: Color = Color::srgb(0.22, 0.27, 0.30);
const EDGE: Color = Color::srgb(0.39, 0.44, 0.45);
const RUST: Color = Color::srgb(0.42, 0.29, 0.23);
const BONE: Color = Color::srgb(0.72, 0.70, 0.62);
const ICE: Color = Color::srgb(0.50, 0.72, 0.77);
const CLOTH: Color = Color::srgb(0.19, 0.28, 0.34);

/// Mesh segments, not encounter identities or gameplay limbs. Ornaments are
/// merged into the segment they follow; no rivet or scale gets an entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Segment {
    Pelvis,
    Torso,
    Head,
    UpperLeft,
    UpperRight,
    ForeLeft,
    ForeRight,
    ThighLeft,
    ThighRight,
    ShinLeft,
    ShinRight,
    Blade,
    CloakLeft,
    CloakMiddle,
    CloakRight,
}

pub(super) const SEGMENTS: [Segment; 15] = [
    Segment::Pelvis,
    Segment::Torso,
    Segment::Head,
    Segment::UpperLeft,
    Segment::UpperRight,
    Segment::ForeLeft,
    Segment::ForeRight,
    Segment::ThighLeft,
    Segment::ThighRight,
    Segment::ShinLeft,
    Segment::ShinRight,
    Segment::Blade,
    Segment::CloakLeft,
    Segment::CloakMiddle,
    Segment::CloakRight,
];

type Part = (Vec3, Vec3, Color);
fn part(size: [f32; 3], centre: [f32; 3], colour: Color) -> Part {
    (Vec3::from_array(size), Vec3::from_array(centre), colour)
}

fn geometry(segment: Segment) -> Mesh {
    use Segment::*;
    let mut parts = Vec::new();
    match segment {
        Pelvis => {
            parts.push(part([0.54, 0.33, 0.42], [0.0, 1.10, 0.0], IRON));
            parts.push(part([0.58, 0.10, 0.46], [0.0, 1.23, 0.0], RUST));
            for x in [-0.21, -0.07, 0.09, 0.23] {
                parts.push(part([0.035, 0.21, 0.035], [x, 1.07, -0.245], EDGE));
                parts.push(part([0.070, 0.085, 0.045], [x, 0.95, -0.25], BONE));
            }
        }
        Torso => {
            // Two chest halves leave a true narrow fissure; the core is recessed,
            // not a bright stripe covering otherwise solid armour.
            for x in [-0.1725, 0.1725] {
                parts.push(part([0.275, 1.05, 0.47], [x, 1.79, 0.0], IRON));
                for y in [1.41, 1.62, 1.83, 2.04] {
                    parts.push(part([0.28, 0.08, 0.035], [x, y, -0.249], RUST));
                    for dx in [-0.09, 0.09] {
                        parts.push(part([0.025, 0.025, 0.016], [x + dx, y, -0.275], EDGE));
                    }
                }
            }
            parts.push(part([0.06, 0.60, 0.12], [0.0, 1.82, -0.13], ICE));
            parts.push(part([0.15, 0.16, 0.21], [0.0, 2.30, 0.0], BONE));
            // Cords stay on the outer chest halves, leaving the casting core legible.
            for x in [-0.25, 0.25] {
                parts.push(part([0.035, 0.58, 0.025], [x, 1.88, -0.288], CLOTH));
            }
        }
        Head => {
            parts.push(part([0.34, 0.33, 0.33], [0.0, 2.45, 0.0], BONE));
            parts.push(part([0.37, 0.09, 0.38], [0.0, 2.60, 0.0], IRON));
            parts.push(part([0.19, 0.30, 0.04], [0.10, 2.44, -0.19], IRON));
            parts.push(part([0.13, 0.075, 0.02], [-0.09, 2.49, -0.174], CLOTH));
            parts.push(part([0.04, 0.025, 0.015], [-0.09, 2.49, -0.187], ICE));
            for x in [-0.14, -0.08, -0.02] {
                parts.push(part([0.027, 0.065, 0.025], [x, 2.31, -0.18], BONE));
            }
            parts.push(part([0.43, 0.055, 0.43], [0.0, 2.665, 0.0], RUST));
            for (x, h) in [(-0.17, 0.08), (-0.06, 0.13), (0.06, 0.09), (0.17, 0.05)] {
                for z in [-0.17, 0.17] {
                    parts.push(part([0.045, h, 0.045], [x, 2.67 + h / 2.0, z], IRON));
                }
            }
        }
        UpperLeft | UpperRight => {
            let x = if segment == UpperLeft { -0.39 } else { 0.39 };
            parts.push(part([0.17, 0.49, 0.22], [x, 2.025, 0.0], IRON));
            // Pauldron layers are rigid on the upper arm, so they share its mesh.
            for (y, depth) in [(2.24, 0.38), (2.14, 0.33), (2.04, 0.28)] {
                parts.push(part([0.21, 0.085, depth], [x, y, 0.0], EDGE));
                parts.push(part([0.16, 0.02, depth + 0.02], [x, y + 0.043, 0.0], RUST));
            }
        }
        ForeLeft | ForeRight => {
            let x = if segment == ForeLeft { -0.39 } else { 0.39 };
            parts.push(part([0.17, 0.40, 0.21], [x, 1.62, 0.0], IRON));
            parts.push(part([0.19, 0.075, 0.24], [x, 1.78, 0.0], EDGE));
            parts.push(part([0.17, 0.12, 0.18], [x, 1.39, -0.02], BONE));
            for dx in [-0.054, 0.0, 0.054] {
                parts.push(part([0.032, 0.095, 0.07], [x + dx, 1.32, -0.07], IRON));
            }
        }
        ThighLeft | ThighRight => {
            let x = if segment == ThighLeft { -0.17 } else { 0.17 };
            parts.push(part([0.24, 0.53, 0.30], [x, 0.80, 0.0], IRON));
            for y in [0.70, 0.88, 1.02] {
                parts.push(part([0.26, 0.065, 0.34], [x, y, 0.0], RUST));
            }
        }
        ShinLeft | ShinRight => {
            let x = if segment == ShinLeft { -0.17 } else { 0.17 };
            parts.push(part([0.25, 0.48, 0.31], [x, 0.32, 0.0], IRON));
            parts.push(part([0.28, 0.17, 0.43], [x, 0.085, -0.035], IRON));
            parts.push(part([0.23, 0.08, 0.035], [x, 0.51, -0.177], EDGE));
            parts.push(part([0.04, 0.27, 0.025], [x, 0.29, -0.171], RUST));
        }
        Blade => {
            // Entire blade stays outside the chest's X extent through the sword
            // poses. Grip overlaps the right palm. The silhouette reads as a
            // long funeral blade, with a broad fuller and a broken square tip.
            parts.push(part([0.055, 0.29, 0.07], [0.42, 1.39, -0.115], CLOTH));
            parts.push(part([0.16, 0.05, 0.12], [0.42, 1.235, -0.115], RUST));
            parts.push(part([0.12, 0.88, 0.045], [0.42, 0.78, -0.115], EDGE));
            parts.push(part([0.065, 0.79, 0.012], [0.42, 0.81, -0.141], IRON));
            parts.push(part([0.08, 0.10, 0.045], [0.42, 0.30, -0.115], EDGE));
            for y in [0.55, 0.75, 0.95] {
                parts.push(part([0.035, 0.028, 0.01], [0.42, y, -0.150], ICE));
            }
        }
        CloakLeft | CloakMiddle | CloakRight => {
            let (x, h) = match segment {
                CloakLeft => (-0.25, 1.58),
                CloakMiddle => (0.0, 1.73),
                _ => (0.25, 1.49),
            };
            // The strip wraps over the armour at its hinge instead of floating behind it.
            parts.push(part([0.22, 0.10, 0.19], [x, 2.09, 0.295], CLOTH));
            parts.push(part([0.22, h, 0.065], [x, 2.10 - h / 2.0, 0.36], CLOTH));
            for dx in [-0.075, 0.065] {
                parts.push(part(
                    [0.035, h - 0.12, 0.025],
                    [x + dx, 2.05 - h / 2.0, 0.402],
                    ICE,
                ));
            }
            parts.push(part(
                [0.10, 0.13, 0.08],
                [x - 0.035, 2.10 - h + 0.025, 0.365],
                ICE,
            ));
        }
    }
    boxes(&parts)
}

pub(super) fn meshes() -> Vec<(Segment, Mesh)> {
    SEGMENTS
        .into_iter()
        .map(|segment| (segment, geometry(segment)))
        .collect()
}

pub(super) fn visuals(
    meshes_out: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) -> SpeciesVisuals {
    let material = materials.add(StandardMaterial {
        perceptual_roughness: 0.72,
        metallic: 0.20,
        ..StandardMaterial::from_color(Color::WHITE)
    });
    let parts: Vec<_> = meshes()
        .into_iter()
        .map(|(segment, mesh)| (segment, meshes_out.add(mesh)))
        .collect();
    SpeciesVisuals {
        body: parts[0].1.clone(),
        head: parts[2].1.clone(),
        legs: None,
        arms: None,
        eyes: None,
        body_material: material.clone(),
        head_material: material,
        king_parts: Some(parts),
    }
}

fn around(pivot: Vec3, rotation: Quat) -> Mat4 {
    Mat4::from_translation(pivot) * Mat4::from_quat(rotation) * Mat4::from_translation(-pivot)
}

#[derive(Clone, Copy)]
struct Pose {
    breath: f32,
    head: f32,
    left: f32,
    right: f32,
    elbow: f32,
    stride: f32,
    cloak: f32,
}

fn pose(action: MobAction, elapsed: Duration, arm: Quat) -> Pose {
    let t = elapsed.as_secs_f32();
    let living = !matches!(action, MobAction::Dying | MobAction::Corpse);
    let walking = matches!(action, MobAction::Chase | MobAction::Flee);
    let angle = arm.to_euler(EulerRot::XYZ).0;
    Pose {
        breath: if living {
            (t * 1.8).sin() * 0.012 + angle * 0.03
        } else {
            0.0
        },
        head: if action == MobAction::Idle {
            (t * 0.7).sin() * 0.025
        } else {
            0.0
        },
        left: if living { angle * 0.72 } else { 0.0 },
        right: if living { angle } else { 0.0 },
        elbow: if living { angle * 0.18 } else { 0.0 },
        stride: if walking { (t * 5.0).sin() * 0.28 } else { 0.0 },
        cloak: if living { (t * 2.0).sin() * 0.025 } else { 0.0 },
    }
}

fn joint(segment: Segment, p: Pose) -> Transform {
    use Segment::*;
    let torso = around(Vec3::Y * 1.25, Quat::from_rotation_x(p.breath));
    let shoulder = |left: bool| {
        torso
            * around(
                Vec3::new(if left { -0.39 } else { 0.39 }, 2.25, 0.0),
                Quat::from_rotation_x(if left { p.left } else { p.right }),
            )
    };
    let elbow = |left: bool| {
        shoulder(left)
            * around(
                Vec3::new(if left { -0.39 } else { 0.39 }, 1.80, 0.0),
                Quat::from_rotation_x(p.elbow),
            )
    };
    let leg = |left: bool| {
        let angle = if left { p.stride } else { -p.stride };
        Mat4::from_translation(Vec3::Y * (angle.abs() * 0.40))
            * around(
                Vec3::new(if left { -0.17 } else { 0.17 }, 1.05, 0.0),
                Quat::from_rotation_x(angle),
            )
    };
    let matrix = match segment {
        Pelvis => Mat4::IDENTITY,
        Torso => torso,
        Head => torso * around(Vec3::Y * 2.30, Quat::from_rotation_y(p.head)),
        UpperLeft => shoulder(true),
        UpperRight => shoulder(false),
        ForeLeft => elbow(true),
        ForeRight | Blade => elbow(false),
        ThighLeft => leg(true),
        ThighRight => leg(false),
        ShinLeft => {
            leg(true)
                * around(
                    Vec3::new(-0.17, 0.55, 0.0),
                    Quat::from_rotation_x(p.stride.max(0.0) * 0.4),
                )
        }
        ShinRight => {
            leg(false)
                * around(
                    Vec3::new(0.17, 0.55, 0.0),
                    Quat::from_rotation_x((-p.stride).max(0.0) * 0.4),
                )
        }
        CloakLeft | CloakMiddle | CloakRight => {
            let x = match segment {
                CloakLeft => -0.25,
                CloakMiddle => 0.0,
                _ => 0.25,
            };
            torso
                * around(
                    Vec3::new(x, 2.10, 0.36),
                    Quat::from_rotation_x(
                        p.cloak
                            * match segment {
                                CloakMiddle => 0.65,
                                CloakRight => -0.8,
                                _ => 1.0,
                            },
                    ),
                )
        }
    };
    Transform::from_matrix(matrix)
}

pub(super) fn transform(
    segment: Segment,
    action: MobAction,
    elapsed: Duration,
    arm: Quat,
) -> Transform {
    joint(segment, pose(action, elapsed, arm))
}

#[cfg(test)]
pub(super) fn posed_meshes(action: MobAction, elapsed: Duration) -> Vec<Mesh> {
    let arm = draugr_arm_swing(action, 0.0, elapsed);
    meshes()
        .into_iter()
        .map(|(segment, mesh)| mesh.transformed_by(transform(segment, action, elapsed, arm)))
        .collect()
}

/// Use the same articulated joints as the body renderer, sampled directly from the
/// announced phase. A cast raises the free arm, a channel plants the blade and a
/// physical preparation raises the weapon arm. No timer restarts on late arrival.
pub(super) fn encounter_transform(
    segment: Segment,
    one: Option<&crate::player::encounters::PresentedMove>,
) -> Transform {
    use crate::net::MovePhase;
    let mut p = Pose {
        breath: 0.0,
        head: 0.0,
        left: 0.0,
        right: 0.0,
        elbow: 0.0,
        stride: 0.0,
        cloak: 0.0,
    };
    if let Some(one) = one {
        let strength = match one.announced.phase {
            MovePhase::Telegraph => 0.4 + 0.6 * one.progress,
            MovePhase::Release => -0.2,
            MovePhase::Channel => 0.9,
            MovePhase::Recovery => -0.3 * (1.0 - one.progress),
        };
        if crate::player::encounters::is_spell(one.announced.kind) {
            p.left = strength * 1.35;
            p.right = if one.announced.phase == MovePhase::Channel {
                0.25
            } else {
                0.0
            };
            p.elbow = strength * 0.4;
        } else {
            p.right = strength * DRAUGR_ARM_RAISED;
            p.left = p.right * 0.72;
        }
        p.head = strength * -0.12;
    }
    joint(segment, p)
}

#[cfg(test)]
pub(super) fn cast_meshes() -> Vec<Mesh> {
    let p = Pose {
        breath: 0.0,
        head: -0.12,
        left: 1.35,
        right: 0.0,
        elbow: 0.4,
        stride: 0.0,
        cloak: 0.0,
    };
    meshes()
        .into_iter()
        .map(|(segment, mesh)| mesh.transformed_by(joint(segment, p)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::mesh::VertexAttributeValues;

    fn positions(mesh: &Mesh) -> &[[f32; 3]] {
        let VertexAttributeValues::Float32x3(values) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap()
        else {
            panic!("positions");
        };
        values
    }

    #[test]
    fn king_joints_articulate_and_the_blade_follows_the_weapon_hand() {
        let elapsed = Duration::from_millis(300);
        let arm = draugr_arm_swing(MobAction::Windup, 0.0, elapsed);
        let p = pose(MobAction::Windup, elapsed, arm);
        assert_ne!(joint(Segment::UpperRight, p), joint(Segment::ForeRight, p));
        assert_eq!(joint(Segment::Blade, p), joint(Segment::ForeRight, p));
        let walking = pose(MobAction::Chase, elapsed, Quat::IDENTITY);
        assert_ne!(
            joint(Segment::ThighLeft, walking),
            joint(Segment::ShinLeft, walking)
        );
        assert_ne!(
            joint(Segment::ThighLeft, walking),
            joint(Segment::ThighRight, walking)
        );
        let casting = Pose {
            left: 1.35,
            elbow: 0.4,
            right: 0.0,
            ..p
        };
        let hand = Vec3::new(-0.39, 1.39, -0.02);
        let raised = joint(Segment::ForeLeft, casting).transform_point(hand);
        assert!(
            raised.y > 2.0 && raised.z < -0.5,
            "cast hand is not raised in front: {raised}"
        );
        assert_eq!(
            joint(Segment::Blade, casting),
            joint(Segment::ForeRight, casting)
        );
    }

    #[test]
    fn king_geometry_has_bounded_segments_and_preserves_the_rest_box() {
        let rig = meshes();
        assert!(rig.len() <= 17);
        let mut triangles = 0;
        let envelope = body(MobKind::DraugrKing);
        for (i, (segment, mesh)) in rig.iter().enumerate() {
            assert!(!rig[..i].iter().any(|(other, _)| other == segment));
            triangles += mesh.indices().unwrap().len() / 3;
            for &[x, y, z] in positions(mesh) {
                assert!(
                    x.abs() <= envelope.width / 2.0 + 1e-5
                        && z.abs() <= envelope.width / 2.0 + 1e-5
                        && y >= -1e-5
                        && y <= envelope.height + 1e-5,
                    "{segment:?}: {x}, {y}, {z}"
                );
            }
        }
        assert!(triangles <= 12_000);
        assert!(triangles > 500, "the authored ornament disappeared");
    }

    #[test]
    fn king_blade_stays_clear_of_crown_and_chest_through_sword_and_cast_poses() {
        let rig = meshes();
        for action in [
            MobAction::Idle,
            MobAction::Chase,
            MobAction::Windup,
            MobAction::Recovery,
            MobAction::Corpse,
        ] {
            for frame in 0..61 {
                let elapsed = Duration::from_secs_f32(frame as f32 / 60.0);
                let p = pose(action, elapsed, draugr_arm_swing(action, 0.0, elapsed));
                // X is a separating axis even when both shoulder and elbow rotate.
                // The blade's left edge must stay beyond every crown/chest vertex.
                let edge = |segment: Segment, minimum: bool, p: Pose| {
                    let mesh = &rig.iter().find(|(s, _)| *s == segment).unwrap().1;
                    let transform = joint(segment, p);
                    positions(mesh)
                        .iter()
                        .map(|&v| transform.transform_point(Vec3::from_array(v)).x)
                        .reduce(|a, b| if minimum { a.min(b) } else { a.max(b) })
                        .unwrap()
                };
                assert!(edge(Segment::Blade, true, p) > edge(Segment::Head, false, p));
                // Crossguard may meet the arm but must not enter the torso.
                assert!(edge(Segment::Blade, true, p) > edge(Segment::Torso, false, p));
                // Preview-only free-hand cast key pose; no cast state is inferred
                // from MobAction or sent locally. The future contract owns that.
                let cast = Pose {
                    left: 1.35,
                    elbow: 0.4,
                    right: 0.0,
                    ..p
                };
                assert!(edge(Segment::Blade, true, cast) > edge(Segment::Head, false, cast));
            }
        }
    }

    #[test]
    fn king_feet_and_cloak_stay_above_the_floor_and_death_stops_secondary_motion() {
        for frame in 0..121 {
            let elapsed = Duration::from_secs_f32(frame as f32 / 60.0);
            for (segment, mesh) in meshes() {
                let transform = transform(segment, MobAction::Chase, elapsed, Quat::IDENTITY);
                for &v in positions(&mesh) {
                    assert!(
                        transform.transform_point(Vec3::from_array(v)).y >= -1e-5,
                        "{segment:?} went below its local floor"
                    );
                }
                assert_eq!(
                    super::transform(segment, MobAction::Corpse, elapsed, Quat::IDENTITY),
                    super::transform(segment, MobAction::Corpse, Duration::ZERO, Quat::IDENTITY)
                );
            }
        }
    }
}
