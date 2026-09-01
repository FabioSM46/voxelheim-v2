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

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

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

const MANE_ROOT: Vec3 = Vec3::new(0.0, 1.48, -0.015);
const MANE_STRIP: Vec3 = Vec3::new(0.055, 0.44, 0.045);
const TAIL_ROOT: Vec3 = Vec3::new(0.0, 1.02, 0.23);
const TAIL_STRIP: Vec3 = Vec3::new(0.075, 0.46, 0.045);
const MANE_SWING: f32 = 0.035;
const TAIL_SWING: f32 = 0.10;

const SADDLE: Vec3 = Vec3::new(0.50, 0.08, 0.26);
const SADDLE_CENTRE: Vec3 = Vec3::new(0.0, 1.15, 0.045);
const SADDLE_FLAP: Vec3 = Vec3::new(0.035, 0.25, 0.18);
const EYE: Vec2 = Vec2::new(0.045, 0.045);

const COAT_EDGE: u32 = 32;
const COAT_SEED: u32 = 0x0715_C0A7;

/// Raising the existing humanoid by this amount puts its hip on the top of the horse's
/// back. The legs then fold from that same hip; the rider is seated rather than standing
/// on the torso, and no second humanoid rig is introduced.
pub(super) const RIDER_LIFT: f32 = 0.42;
const RIDER_LEG_ANGLE: f32 = 1.05;
const RIDER_ARM_ANGLE: f32 = 0.68;

const BLACK_COAT: Color = Color::srgb(0.075, 0.065, 0.055);
const BROWN_COAT: Color = Color::srgb(0.30, 0.16, 0.075);
const GREY_COAT: Color = Color::srgb(0.43, 0.45, 0.44);
const HAIR_COLOUR: Color = Color::srgb(0.055, 0.045, 0.038);
const LEATHER_COLOUR: Color = Color::srgb(0.25, 0.105, 0.045);
const EYE_COLOUR: Color = Color::srgb(0.82, 0.61, 0.18);

#[derive(Resource, Clone)]
pub(super) struct HorseCoats {
    image: Handle<Image>,
}

impl FromWorld for HorseCoats {
    fn from_world(world: &mut World) -> Self {
        Self {
            image: world
                .resource_mut::<Assets<Image>>()
                .add(generated_coat_image()),
        }
    }
}

#[derive(Resource)]
pub(super) struct HorseVisuals {
    body: Handle<Mesh>,
    head: Handle<Mesh>,
    leg: Handle<Mesh>,
    mane: Handle<Mesh>,
    tail: Handle<Mesh>,
    tack: Handle<Mesh>,
    eyes: Handle<Mesh>,
    black: Handle<StandardMaterial>,
    brown: Handle<StandardMaterial>,
    grey: Handle<StandardMaterial>,
    hair: Handle<StandardMaterial>,
    leather: Handle<StandardMaterial>,
    eye: Handle<StandardMaterial>,
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

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HorseHair {
    Mane,
    Tail,
}

fn coat_scatter(mark: u32, channel: u32) -> f32 {
    let mut bits = mark
        .wrapping_mul(0x9E37_79B1)
        .wrapping_add(channel.wrapping_mul(0x85EB_CA6B))
        ^ COAT_SEED;
    bits ^= bits >> 16;
    bits = bits.wrapping_mul(0x7FEB_352D);
    bits ^= bits >> 15;
    bits = bits.wrapping_mul(0x846C_A68B);
    bits ^= bits >> 16;
    (bits >> 8) as f32 / 16_777_216.0
}

fn coat_field(u: f32, v: f32) -> f32 {
    let mut dapple = 0.0_f32;
    for mark in 0..12 {
        let centre_u = coat_scatter(mark, 0);
        let centre_v = coat_scatter(mark, 1);
        let radius = 0.08 + coat_scatter(mark, 2) * 0.08;
        let around = (u - centre_u).abs();
        let dx = around.min(1.0 - around);
        let dy = v - centre_v;
        let distance = (dx * dx + dy * dy).sqrt() / radius;
        dapple = dapple.max((1.0 - distance * distance).max(0.0));
    }

    let muscle = (v * PI * 4.0).sin().abs();
    (0.76 + 0.16 * dapple + 0.08 * muscle).clamp(0.0, 1.0)
}

fn generated_coat_image() -> Image {
    let mut data = Vec::with_capacity((COAT_EDGE * COAT_EDGE * 4) as usize);
    for row in 0..COAT_EDGE {
        for column in 0..COAT_EDGE {
            let value = coat_field(
                (column as f32 + 0.5) / COAT_EDGE as f32,
                (row as f32 + 0.5) / COAT_EDGE as f32,
            );
            let texel = (value * 255.0).round() as u8;
            data.extend_from_slice(&[texel, texel, texel, u8::MAX]);
        }
    }

    let mut image = Image::new(
        Extent3d {
            width: COAT_EDGE,
            height: COAT_EDGE,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::default(),
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        ..default()
    });
    image
}

pub(super) fn register(app: &mut App) {
    if !app.world().contains_resource::<Assets<Image>>() {
        app.init_asset::<Image>();
    }
    app.init_resource::<HorseCoats>();
}

fn coat_material(colour: Color, image: &Handle<Image>) -> StandardMaterial {
    StandardMaterial {
        base_color: colour,
        base_color_texture: Some(image.clone()),
        perceptual_roughness: 0.92,
        ..default()
    }
}

pub(super) fn create_visuals(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    coats: Res<HorseCoats>,
) {
    commands.insert_resource(HorseVisuals {
        body: meshes.add(horse_body_mesh()),
        head: meshes.add(horse_head_mesh()),
        leg: meshes.add(horse_leg_mesh()),
        mane: meshes.add(horse_mane_mesh()),
        tail: meshes.add(horse_tail_mesh()),
        tack: meshes.add(horse_tack_mesh()),
        eyes: meshes.add(horse_eye_mesh()),
        black: materials.add(coat_material(BLACK_COAT, &coats.image)),
        brown: materials.add(coat_material(BROWN_COAT, &coats.image)),
        grey: materials.add(coat_material(GREY_COAT, &coats.image)),
        hair: materials.add(StandardMaterial::from_color(HAIR_COLOUR)),
        leather: materials.add(StandardMaterial::from_color(LEATHER_COLOUR)),
        eye: materials.add(StandardMaterial {
            base_color: EYE_COLOUR,
            unlit: true,
            cull_mode: None,
            depth_bias: 1.0,
            ..default()
        }),
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

fn horse_mane_mesh() -> Mesh {
    Mesh::from(Cuboid::from_size(MANE_STRIP)).translated_by(Vec3::Y * -MANE_STRIP.y / 2.0)
}

fn horse_tail_mesh() -> Mesh {
    Mesh::from(Cuboid::from_size(TAIL_STRIP)).translated_by(Vec3::Y * -TAIL_STRIP.y / 2.0)
}

fn bar_between(start: Vec3, end: Vec3, width: f32) -> Mesh {
    let axis = end - start;
    Mesh::from(Cuboid::new(width, width, axis.length())).transformed_by(
        Transform::from_translation((start + end) / 2.0)
            .with_rotation(Quat::from_rotation_arc(Vec3::Z, axis.normalize())),
    )
}

fn horse_tack_mesh() -> Mesh {
    let mut saddle = Mesh::from(Cuboid::from_size(SADDLE)).translated_by(SADDLE_CENTRE);
    let flap_x = SADDLE.x / 2.0 - SADDLE_FLAP.x / 2.0;
    let flaps = [-1.0, 1.0].map(|side| {
        Mesh::from(Cuboid::from_size(SADDLE_FLAP)).translated_by(Vec3::new(
            side * flap_x,
            1.045,
            SADDLE_CENTRE.z,
        ))
    });
    let reins = [-1.0, 1.0].map(|side| {
        bar_between(
            Vec3::new(side * 0.11, 1.35, -0.29),
            Vec3::new(side * 0.20, 1.18, 0.13),
            0.018,
        )
    });
    merge_all(&mut saddle, flaps.into_iter().chain(reins), "horse tack");
    saddle
}

fn horse_eye_mesh() -> Mesh {
    let mut left = Mesh::from(Rectangle::new(EYE.x, EYE.y)).translated_by(Vec3::new(
        -0.08,
        HORSE_HEAD_CENTRE.y + 0.02,
        HORSE_HEAD_CENTRE.z - HORSE_HEAD.z / 2.0,
    ));
    let right = Mesh::from(Rectangle::new(EYE.x, EYE.y)).translated_by(Vec3::new(
        0.08,
        HORSE_HEAD_CENTRE.y + 0.02,
        HORSE_HEAD_CENTRE.z - HORSE_HEAD.z / 2.0,
    ));
    merge_all(&mut left, [right], "horse eyes");
    left
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
                horse.spawn((
                    HorsePart,
                    HorseHair::Mane,
                    Mesh3d(visuals.mane.clone()),
                    MeshMaterial3d(visuals.hair.clone()),
                    hair_transform(HorseHair::Mane, WalkPose::default()),
                ));
                horse.spawn((
                    HorsePart,
                    HorseHair::Tail,
                    Mesh3d(visuals.tail.clone()),
                    MeshMaterial3d(visuals.hair.clone()),
                    hair_transform(HorseHair::Tail, WalkPose::default()),
                ));
                horse.spawn((
                    HorsePart,
                    Mesh3d(visuals.tack.clone()),
                    MeshMaterial3d(visuals.leather.clone()),
                    Transform::default(),
                ));
                horse.spawn((
                    HorsePart,
                    Mesh3d(visuals.eyes.clone()),
                    MeshMaterial3d(visuals.eye.clone()),
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

pub(super) type MovingHorsePartQuery<'w, 's> = Query<
    'w,
    's,
    (
        Option<&'static HorseLeg>,
        Option<&'static HorseHair>,
        &'static mut Transform,
    ),
    Or<(With<HorseLeg>, With<HorseHair>)>,
>;

/// Poses four independently transformed legs from the rider's distance sample.
pub(super) fn animate_gait(
    bodies: Query<&WalkPose>,
    horses: Query<(&ChildOf, &Children), With<Horse>>,
    mut moving_parts: MovingHorsePartQuery<'_, '_>,
) {
    for (parent, children) in &horses {
        let Ok(walk) = bodies.get(parent.parent()) else {
            continue;
        };
        for child in children {
            let Ok((leg, hair, mut transform)) = moving_parts.get_mut(*child) else {
                continue;
            };
            let next = match (leg, hair) {
                (Some(leg), None) => gait_transform(leg.0, *walk),
                (None, Some(hair)) => hair_transform(*hair, *walk),
                _ => continue,
            };
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

fn hair_transform(hair: HorseHair, walk: WalkPose) -> Transform {
    let phase = if walk.moving { walk.phase.sin() } else { 0.0 };
    let (root, swing) = match hair {
        HorseHair::Mane => (MANE_ROOT, MANE_SWING),
        HorseHair::Tail => (TAIL_ROOT, TAIL_SWING),
    };
    Transform::from_translation(root).with_rotation(Quat::from_rotation_x(phase * swing))
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
    use bevy::asset::AssetPlugin;
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
            meshes.extend([
                horse_mane_mesh().transformed_by(hair_transform(HorseHair::Mane, pose)),
                horse_tail_mesh().transformed_by(hair_transform(HorseHair::Tail, pose)),
                horse_tack_mesh(),
                horse_eye_mesh(),
            ]);
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
    fn mane_and_tail_read_the_legs_distance_phase_without_changing_it() {
        let walking = WalkPose {
            phase: PI / 2.0,
            moving: true,
        };
        for (hair, want) in [(HorseHair::Mane, MANE_SWING), (HorseHair::Tail, TAIL_SWING)] {
            let pitch = hair_transform(hair, walking)
                .rotation
                .to_euler(EulerRot::XYZ)
                .0;
            assert!((pitch - want).abs() < 1e-5);
            assert_eq!(
                hair_transform(hair, WalkPose::default()).rotation,
                Quat::IDENTITY
            );
        }
        assert!((angle(Leg::LeftFront, walking) - HORSE_LEG_SWING).abs() < 1e-5);
    }

    fn visual_app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>();
        register(&mut app);
        app.add_systems(Startup, create_visuals);
        app.update();
        app
    }

    #[test]
    fn one_generated_image_dresses_all_three_coats() {
        let app = visual_app();
        let world = app.world();
        let coats = world.resource::<HorseCoats>();
        let visuals = world.resource::<HorseVisuals>();
        let materials = world.resource::<Assets<StandardMaterial>>();

        let coat_materials = [&visuals.black, &visuals.brown, &visuals.grey]
            .map(|handle| materials.get(handle).expect("horse coat material"));
        for material in coat_materials {
            assert_eq!(material.base_color_texture.as_ref(), Some(&coats.image));
        }
        assert_eq!(
            coat_materials.map(|material| material.base_color),
            [BLACK_COAT, BROWN_COAT, GREY_COAT]
        );
        assert_eq!(
            materials.get(&visuals.hair).unwrap().base_color,
            HAIR_COLOUR
        );
        assert_eq!(
            materials.get(&visuals.leather).unwrap().base_color,
            LEATHER_COLOUR
        );
        let cuboid = Mesh::from(Cuboid::from_size(Vec3::ONE)).count_vertices();
        assert_eq!(horse_head_mesh().count_vertices(), cuboid * 3);
        assert_eq!(horse_eye_mesh().count_vertices(), 8);
        assert_eq!(horse_tack_mesh().count_vertices(), cuboid * 5);
        assert_ne!(HAIR_COLOUR, LEATHER_COLOUR);
        for mesh in [horse_body_mesh(), horse_head_mesh(), horse_leg_mesh()] {
            let Some(VertexAttributeValues::Float32x2(uvs)) = mesh.attribute(Mesh::ATTRIBUTE_UV_0)
            else {
                panic!("a coat mesh has no UVs");
            };
            assert!(uvs.iter().any(|uv| *uv != uvs[0]));
        }
    }

    #[test]
    fn the_seeded_coat_is_stable_and_has_depth() {
        let first = generated_coat_image();
        assert_eq!(first.data, generated_coat_image().data);

        let data = first.data.expect("generated coat carries texels");
        let values: std::collections::HashSet<u8> =
            data.chunks_exact(4).map(|rgba| rgba[0]).collect();
        assert!(values.len() > 12);
    }

    #[test]
    fn registering_the_coat_twice_preserves_foreign_images_and_its_one_handle() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Image>();
        let foreign = app
            .world_mut()
            .resource_mut::<Assets<Image>>()
            .add(Image::default());

        register(&mut app);
        let coat = app.world().resource::<HorseCoats>().image.clone();
        register(&mut app);

        let images = app.world().resource::<Assets<Image>>();
        assert!(images.get(&foreign).is_some());
        assert!(images.get(&coat).is_some());
        assert_eq!(app.world().resource::<HorseCoats>().image, coat);
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
