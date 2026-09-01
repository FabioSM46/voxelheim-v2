//! Snapshot-driven horses under the existing humanoid body rig.
//!
//! A horse exists only because the newest complete mount projection names its rider. It
//! is a child of that rider's stable [`Body`] entity, so it inherits the authoritative
//! feet position, yaw and visibility; dismounting removes only this child tree and never
//! replaces the person. The rider remains the ordinary twelve-piece humanoid rig.
//!
//! The mesh is procedural cuboids and the gait is a transform on four leg children. Its
//! only clock is [`WalkPose::phase`], which interpolation advances from horizontal
//! distance: faster authoritative travel cycles the same rig faster, while elapsed time
//! over no distance cannot move a hoof.

use std::collections::HashMap;
use std::f32::consts::PI;

use bevy::prelude::*;

use super::appearance::{BodyPiece, Limb};
use super::interpolate::SnapshotBuffer;
use super::{Body, WalkPose, merge_all};
use crate::net::MountKind;

/// The horse stays inside the same 0.6-square footprint the authoritative player body
/// collides. It may be taller: the rider is visibly seated above a mount, while gameplay
/// continues to collide the server's unchanged player box.
const HORSE_TORSO: Vec3 = Vec3::new(0.42, 0.40, 0.56);
const HORSE_TORSO_CENTRE: Vec3 = Vec3::new(0.0, 0.92, 0.0);
const HORSE_NECK: Vec3 = Vec3::new(0.20, 0.50, 0.16);
const HORSE_NECK_CENTRE: Vec3 = Vec3::new(0.0, 1.17, -0.12);
const HORSE_HEAD: Vec3 = Vec3::new(0.30, 0.24, 0.20);
const HORSE_HEAD_CENTRE: Vec3 = Vec3::new(0.0, 1.38, -0.20);
const HORSE_EAR: Vec3 = Vec3::new(0.07, 0.16, 0.07);
const HORSE_LEG: Vec3 = Vec3::new(0.11, 0.72, 0.11);
const HORSE_HOOF: Vec3 = Vec3::new(0.13, 0.12, 0.16);
const HORSE_HIP_X: f32 = 0.21;
const HORSE_HIP_Z: f32 = 0.11;
const HORSE_LEG_SWING: f32 = 0.13;

/// Raising the existing humanoid by this amount puts its hip on the top of the horse's
/// back. The legs then fold from that same hip; the rider is seated rather than standing
/// on the torso, and no second humanoid rig is introduced.
pub(super) const RIDER_LIFT: f32 = 0.42;
const RIDER_LEG_ANGLE: f32 = 1.05;
const RIDER_ARM_ANGLE: f32 = 0.68;

const BLACK_COAT: Color = Color::srgb(0.075, 0.065, 0.055);
const BROWN_COAT: Color = Color::srgb(0.30, 0.16, 0.075);
const GREY_COAT: Color = Color::srgb(0.43, 0.45, 0.44);

#[derive(Resource)]
pub(super) struct HorseVisuals {
    body: Handle<Mesh>,
    head: Handle<Mesh>,
    leg: Handle<Mesh>,
    black: Handle<StandardMaterial>,
    brown: Handle<StandardMaterial>,
    grey: Handle<StandardMaterial>,
}

impl HorseVisuals {
    fn material(&self, kind: MountKind) -> Handle<StandardMaterial> {
        match kind {
            MountKind::BlackHorse => self.black.clone(),
            MountKind::BrownHorse => self.brown.clone(),
            MountKind::GreyHorse => self.grey.clone(),
        }
    }
}

/// The root of one horse, parented directly to the existing rider body.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Horse {
    pub(super) kind: MountKind,
}

#[derive(Component)]
struct HorsePart;

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HorseLeg(Leg);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Leg {
    LeftFront,
    RightFront,
    LeftRear,
    RightRear,
}

impl Leg {
    const ALL: [Self; 4] = [
        Self::LeftFront,
        Self::RightFront,
        Self::LeftRear,
        Self::RightRear,
    ];

    const fn hip(self) -> Vec3 {
        match self {
            Self::LeftFront => Vec3::new(-HORSE_HIP_X, HORSE_LEG.y, -HORSE_HIP_Z),
            Self::RightFront => Vec3::new(HORSE_HIP_X, HORSE_LEG.y, -HORSE_HIP_Z),
            Self::LeftRear => Vec3::new(-HORSE_HIP_X, HORSE_LEG.y, HORSE_HIP_Z),
            Self::RightRear => Vec3::new(HORSE_HIP_X, HORSE_LEG.y, HORSE_HIP_Z),
        }
    }

    /// Diagonal pairs share a phase; the other pair is half a cycle away.
    const fn phase_offset(self) -> f32 {
        match self {
            Self::LeftFront | Self::RightRear => 0.0,
            Self::RightFront | Self::LeftRear => PI,
        }
    }
}

pub(super) fn create_visuals(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(HorseVisuals {
        body: meshes.add(horse_body_mesh()),
        head: meshes.add(horse_head_mesh()),
        leg: meshes.add(horse_leg_mesh()),
        black: materials.add(StandardMaterial::from_color(BLACK_COAT)),
        brown: materials.add(StandardMaterial::from_color(BROWN_COAT)),
        grey: materials.add(StandardMaterial::from_color(GREY_COAT)),
    });
}

fn horse_body_mesh() -> Mesh {
    let mut torso = Mesh::from(Cuboid::from_size(HORSE_TORSO)).translated_by(HORSE_TORSO_CENTRE);
    let neck = Mesh::from(Cuboid::from_size(HORSE_NECK)).translated_by(HORSE_NECK_CENTRE);
    merge_all(&mut torso, [neck], "horse body");
    torso
}

fn horse_head_mesh() -> Mesh {
    let mut head = Mesh::from(Cuboid::from_size(HORSE_HEAD)).translated_by(HORSE_HEAD_CENTRE);
    let ears = [-1.0, 1.0].map(|side| {
        Mesh::from(Cuboid::from_size(HORSE_EAR)).translated_by(Vec3::new(
            side * 0.09,
            HORSE_HEAD_CENTRE.y + HORSE_HEAD.y / 2.0 + HORSE_EAR.y / 2.0,
            -0.20,
        ))
    });
    merge_all(&mut head, ears, "horse head");
    head
}

/// One leg authored downwards from its hip, shared by all four leg children.
fn horse_leg_mesh() -> Mesh {
    let mut leg = Mesh::from(Cuboid::from_size(HORSE_LEG)).translated_by(Vec3::new(
        0.0,
        -HORSE_LEG.y / 2.0,
        0.0,
    ));
    let hoof = Mesh::from(Cuboid::from_size(HORSE_HOOF)).translated_by(Vec3::new(
        0.0,
        -HORSE_LEG.y + HORSE_HOOF.y / 2.0,
        -0.015,
    ));
    merge_all(&mut leg, [hoof], "horse leg");
    leg
}

/// Reconciles horse child trees with the newest sparse complete mount projection.
///
/// A changed kind replaces the horse presentation, not the rider. In ordinary play a
/// mount kind is stable until dismount; handling the complete projection anyway keeps a
/// malformed transition from leaving the wrong coat on screen.
pub(super) fn sync_horses(
    buffer: Res<SnapshotBuffer>,
    visuals: Res<HorseVisuals>,
    bodies: Query<(Entity, &Body)>,
    horses: Query<(Entity, &Horse, &ChildOf)>,
    mut commands: Commands,
) {
    let mut current: HashMap<Entity, (Entity, MountKind)> = horses
        .iter()
        .map(|(entity, horse, parent)| (parent.parent(), (entity, horse.kind)))
        .collect();

    for (body_entity, body) in &bodies {
        let wanted = buffer.mount_of(body.0);
        match (current.remove(&body_entity), wanted) {
            (Some((_, kind)), Some(wanted)) if kind == wanted => {}
            (Some((horse, _)), None) => commands.entity(horse).despawn(),
            (Some((horse, _)), Some(wanted)) => {
                commands.entity(horse).despawn();
                spawn_horse(&mut commands, &visuals, body_entity, wanted);
            }
            (None, Some(wanted)) => spawn_horse(&mut commands, &visuals, body_entity, wanted),
            (None, None) => {}
        }
    }

    // The projection is complete over bodies, so a horse whose parent is no longer a
    // body is stale too. Ordinary body despawns take their descendants with them; this
    // closes the distinct case where the parent survives but stops being a body.
    for (_, (horse, _)) in current {
        commands.entity(horse).despawn();
    }
}

fn spawn_horse(commands: &mut Commands, visuals: &HorseVisuals, rider: Entity, kind: MountKind) {
    let material = visuals.material(kind);
    commands.entity(rider).with_children(|body| {
        body.spawn((Horse { kind }, Transform::default(), Visibility::Inherited))
            .with_children(|horse| {
                horse.spawn((
                    HorsePart,
                    Mesh3d(visuals.body.clone()),
                    MeshMaterial3d(material.clone()),
                    Transform::default(),
                ));
                horse.spawn((
                    HorsePart,
                    Mesh3d(visuals.head.clone()),
                    MeshMaterial3d(material.clone()),
                    Transform::default(),
                ));
                for leg in Leg::ALL {
                    horse.spawn((
                        HorseLeg(leg),
                        Mesh3d(visuals.leg.clone()),
                        MeshMaterial3d(material.clone()),
                        gait_transform(leg, WalkPose::default()),
                    ));
                }
            });
    });
}

/// Poses four independently transformed legs from the rider's distance sample.
pub(super) fn animate_gait(
    bodies: Query<&WalkPose>,
    horses: Query<(&ChildOf, &Children), With<Horse>>,
    mut legs: Query<(&HorseLeg, &mut Transform)>,
) {
    for (parent, children) in &horses {
        let Ok(walk) = bodies.get(parent.parent()) else {
            continue;
        };
        for child in children {
            let Ok((leg, mut transform)) = legs.get_mut(*child) else {
                continue;
            };
            let next = gait_transform(leg.0, *walk);
            if *transform != next {
                *transform = next;
            }
        }
    }
}

fn gait_transform(leg: Leg, walk: WalkPose) -> Transform {
    let angle = if walk.moving {
        (walk.phase + leg.phase_offset()).sin() * HORSE_LEG_SWING
    } else {
        0.0
    };
    Transform::from_translation(leg.hip()).with_rotation(Quat::from_rotation_x(angle))
}

/// The existing humanoid piece in its seated pose.
pub(super) fn rider_piece_transform(piece: BodyPiece, blocking: bool) -> Transform {
    let angle = match piece.limb() {
        Some(Limb::LeftArm) if blocking => -1.05,
        Some(Limb::LeftArm | Limb::RightArm) => RIDER_ARM_ANGLE,
        Some(Limb::LeftLeg | Limb::RightLeg) => RIDER_LEG_ANGLE,
        None => 0.0,
    };
    Transform::from_translation(piece.pivot() + Vec3::Y * RIDER_LIFT)
        .with_rotation(Quat::from_rotation_x(angle))
}

#[cfg(test)]
mod tests {
    use bevy::mesh::VertexAttributeValues;

    use super::super::constants::PLAYER_WIDTH;
    use super::*;

    fn extent(meshes: &[Mesh]) -> (Vec3, Vec3) {
        let mut low = Vec3::MAX;
        let mut high = Vec3::MIN;
        for mesh in meshes {
            let Some(VertexAttributeValues::Float32x3(positions)) =
                mesh.attribute(Mesh::ATTRIBUTE_POSITION)
            else {
                panic!("horse mesh has no positions");
            };
            for position in positions {
                let point = Vec3::from_array(*position);
                low = low.min(point);
                high = high.max(point);
            }
        }
        (low, high)
    }

    #[test]
    fn the_horse_stays_inside_the_authoritative_player_footprint() {
        let half = PLAYER_WIDTH / 2.0;
        for pose in [
            WalkPose::default(),
            WalkPose {
                phase: PI / 2.0,
                moving: true,
            },
            WalkPose {
                phase: 3.0 * PI / 2.0,
                moving: true,
            },
        ] {
            let mut meshes = vec![horse_body_mesh(), horse_head_mesh()];
            meshes.extend(
                Leg::ALL.map(|leg| horse_leg_mesh().transformed_by(gait_transform(leg, pose))),
            );
            let (low, high) = extent(&meshes);
            assert!(
                low.x >= -half && high.x <= half,
                "horse at {pose:?} has x = {low:?}..{high:?}"
            );
            assert!(
                low.z >= -half && high.z <= half,
                "horse at {pose:?} has z = {low:?}..{high:?}"
            );
            if !pose.moving {
                assert!(
                    low.y.abs() < 1e-5,
                    "standing horse does not reach the rider's feet plane"
                );
            }
        }
    }

    fn angle(leg: Leg, pose: WalkPose) -> f32 {
        gait_transform(leg, pose).rotation.to_euler(EulerRot::XYZ).0
    }

    #[test]
    fn four_legs_share_one_distance_phase_with_diagonal_offsets() {
        let walking = WalkPose {
            phase: PI / 2.0,
            moving: true,
        };
        let left_front = angle(Leg::LeftFront, walking);
        let right_front = angle(Leg::RightFront, walking);
        assert!((left_front - HORSE_LEG_SWING).abs() < 1e-5);
        assert!((right_front + HORSE_LEG_SWING).abs() < 1e-5);
        assert_eq!(left_front, angle(Leg::RightRear, walking));
        assert_eq!(right_front, angle(Leg::LeftRear, walking));

        let standing = WalkPose {
            phase: PI / 2.0,
            moving: false,
        };
        for leg in Leg::ALL {
            assert_eq!(angle(leg, standing), 0.0);
        }
    }

    #[test]
    fn the_rider_is_seated_from_the_same_humanoid_pivots() {
        let torso = rider_piece_transform(BodyPiece::Torso, false);
        assert_eq!(torso.translation, Vec3::Y * RIDER_LIFT);
        let leg = rider_piece_transform(BodyPiece::LeftTrouser, false);
        assert_eq!(
            leg.translation,
            BodyPiece::LeftTrouser.pivot() + Vec3::Y * RIDER_LIFT
        );
        assert!((leg.rotation.to_euler(EulerRot::XYZ).0 - RIDER_LEG_ANGLE).abs() < 1e-5);
    }
}
