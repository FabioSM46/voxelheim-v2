//! Snapshot-driven horses under the existing humanoid body rig.
//!
//! A ridden horse exists because the newest complete mount projection names its rider. It
//! is a child of that rider's stable [`Body`] entity, so it inherits the authoritative
//! feet position, yaw and visibility; dismounting removes only this child tree and never
//! replaces the person. A capital paddock horse arrives as a `MobKind::Horse` row and
//! owns a world-space root instead. Both routes spawn the same mesh children. The rider
//! remains the ordinary twelve-piece humanoid rig, lifted onto the saddle.
//!
//! The mesh is procedural — tapered solids from [`super::shapes`] where a body is not a
//! box, cuboids where it is — cut to real proportions inside the mounted body the server
//! collides. The gait is a transform on four leg children. Its only clock is
//! [`WalkPose::phase`], which interpolation advances from horizontal distance: faster
//! authoritative travel cycles the same rig faster, while elapsed time over no distance
//! cannot move a hoof.

use std::collections::HashMap;
use std::f32::consts::{FRAC_PI_2, PI};
use std::time::{Duration, Instant};

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use super::appearance::{BodyPiece, Limb};
use super::interpolate::SnapshotBuffer;
use super::shapes::hexahedron;
use super::{Body, InputMode, WalkPose, merge_all};
use crate::net::{MobKind, MountKind, Session};

// The horse is drawn to real proportions — one block is one metre — inside the mounted
// body the server collides: `MOUNTED_WIDTH` square and `MOUNTED_HEIGHT` tall, mirrored in
// `super::constants`. Width and height fit; length does not, because that footprint is
// square and set from the horse's width rather than its length (the server's own reasoning
// at `MountedWidth`). Feet at y = 0, yaw 0 facing -Z, so the nose is the most negative z.

/// One end of a solid lofted along z: a rectangle in the x/y plane at one depth.
#[derive(Debug, Clone, Copy)]
struct Slice {
    z: f32,
    half_x: f32,
    y: (f32, f32),
}

impl Slice {
    const fn new(z: f32, half_x: f32, y: (f32, f32)) -> Self {
        Self { z, half_x, y }
    }
}

/// One end of a solid lofted along y: a rectangle in the x/z plane at one height.
#[derive(Debug, Clone, Copy)]
struct Deck {
    y: f32,
    half_x: f32,
    z: (f32, f32),
}

impl Deck {
    const fn new(y: f32, half_x: f32, z: (f32, f32)) -> Self {
        Self { y, half_x, z }
    }
}

// The barrel, breast to rump: a chest that is deepest at the girth, a loin that shallows
// behind it, and a croup that falls away to the tail.
const BREAST: Slice = Slice::new(-0.80, 0.26, (0.92, 1.48));
const GIRTH: Slice = Slice::new(-0.10, 0.33, (0.85, 1.55));
const LOIN: Slice = Slice::new(0.50, 0.31, (1.00, 1.52));
const RUMP: Slice = Slice::new(0.78, 0.24, (1.10, 1.37));

// The neck rises from the withers to the poll, narrowing as it goes; the head hangs from
// the poll forward and down to a muzzle narrower and shallower than the brow.
const NECK_BASE: Deck = Deck::new(1.44, 0.16, (-0.78, -0.40));
const NECK_POLL: Deck = Deck::new(1.94, 0.11, (-1.12, -0.96));
const BROW: Slice = Slice::new(-0.96, 0.15, (1.66, 2.08));
const MUZZLE: Slice = Slice::new(-1.52, 0.09, (1.42, 1.64));
const EAR: Vec3 = Vec3::new(0.06, 0.20, 0.05);
const EAR_CENTRE: Vec3 = Vec3::new(0.09, 2.10, -1.02);
const EYE: Vec2 = Vec2::new(0.05, 0.05);
const EYE_CENTRE: Vec3 = Vec3::new(0.135, 1.84, -1.14);

/// How tall the drawn horse is: its ear tips. `mobs.rs` reads it as the presentation
/// envelope of a paddock horse.
pub(super) const HORSE_HEIGHT: f32 = EAR_CENTRE.y + EAR.y / 2.0;

// Each leg is one segment from its pivot — the shoulder for a foreleg, the hip for a hind
// leg, both inside the barrel — down to a hoof that is wider at the ground. The swing is
// sized so a hoof sweeps at most `HOOF_SWEEP` along the ground whatever the leg's length.
const LEG_PIVOT_X: f32 = 0.20;
const LEG_PIVOT_Y: f32 = 1.12;
const FRONT_PIVOT_Z: f32 = -0.45;
const REAR_PIVOT_Z: f32 = 0.55;
const HOOF_HEIGHT: f32 = 0.10;
const HORSE_LEG: Vec3 = Vec3::new(0.12, LEG_PIVOT_Y - HOOF_HEIGHT, 0.14);
const HOOF_TOP: Deck = Deck::new(
    HOOF_HEIGHT - LEG_PIVOT_Y,
    HORSE_LEG.x / 2.0,
    (-HORSE_LEG.z / 2.0, HORSE_LEG.z / 2.0),
);
const HOOF_SOLE: Deck = Deck::new(-LEG_PIVOT_Y, 0.08, (-0.10, 0.08));
const HOOF_SWEEP: f32 = 0.30;
const HORSE_LEG_SWING: f32 = HOOF_SWEEP / (2.0 * LEG_PIVOT_Y);

// The mane lies along the crest from the poll to the withers and the tail hangs from the
// croup: each a strip authored downwards from its root and turned to rest along its line.
const MANE_ROOT: Vec3 = Vec3::new(0.0, 1.96, -0.94);
const MANE_STRIP: Vec3 = Vec3::new(0.055, 0.76, 0.05);
const MANE_REST: f32 = -0.84;
const MANE_SWING: f32 = 0.035;
const TAIL_ROOT: Vec3 = Vec3::new(0.0, 1.38, 0.72);
const TAIL_STRIP: Vec3 = Vec3::new(0.075, 0.74, 0.05);
const TAIL_REST: f32 = -0.12;
const TAIL_SWING: f32 = 0.10;

// The saddle seat sits on the back behind the withers with a flap down each side; the
// reins run from the corners of the mouth along the neck to the rider's fists.
const SADDLE: Vec3 = Vec3::new(0.44, 0.07, 0.36);
const SADDLE_CENTRE: Vec3 = Vec3::new(0.0, 1.575, 0.02);
const SADDLE_FLAP: Vec3 = Vec3::new(0.035, 0.30, 0.26);
const SADDLE_FLAP_CENTRE: Vec3 = Vec3::new(0.345, 1.42, 0.02);
const REIN_BIT: Vec3 = Vec3::new(0.095, 1.53, -1.40);
const REIN_HAND: Vec3 = Vec3::new(0.27, 1.82, -0.28);
const REIN_WIDTH: f32 = 0.018;

// What the numbers above have to be to each other, checked when the crate compiles rather
// than when a test runs: the barrel's slices run breast to rump and shallow behind the
// girth; the neck rises from inside the chest and narrows; the head hangs from the poll
// forward to a muzzle narrower and shallower than the brow; a hoof narrows upward.
const _: () = assert!(BREAST.z < GIRTH.z && GIRTH.z < LOIN.z && LOIN.z < RUMP.z);
const _: () = assert!(GIRTH.y.1 - GIRTH.y.0 > LOIN.y.1 - LOIN.y.0);
const _: () = assert!(LOIN.y.1 - LOIN.y.0 > RUMP.y.1 - RUMP.y.0);
const _: () = assert!(NECK_BASE.y > GIRTH.y.0 && NECK_BASE.y < GIRTH.y.1);
const _: () = assert!(NECK_BASE.z.0 > BREAST.z && NECK_BASE.z.1 < GIRTH.z);
const _: () = assert!(NECK_POLL.half_x < NECK_BASE.half_x);
const _: () = assert!(BROW.y.1 > NECK_POLL.y && BROW.y.0 < NECK_POLL.y && BROW.z >= NECK_POLL.z.1);
const _: () = assert!(MUZZLE.z < BROW.z && MUZZLE.half_x < BROW.half_x);
const _: () = assert!(MUZZLE.y.1 - MUZZLE.y.0 < BROW.y.1 - BROW.y.0);
const _: () = assert!(HOOF_TOP.y > HOOF_SOLE.y && HOOF_TOP.half_x < HOOF_SOLE.half_x);
const _: () = assert!(HOOF_TOP.z.1 - HOOF_TOP.z.0 < HOOF_SOLE.z.1 - HOOF_SOLE.z.0);

const COAT_EDGE: u32 = 32;
const COAT_SEED: u32 = 0x0715_C0A7;

/// Raising the existing humanoid by this amount puts its hip pivot on the saddle seat.
/// The legs then fold forward and splay outward from that same hip, so the rider sits
/// astride the barrel rather than through it, and no second humanoid rig is introduced.
/// `camera.rs` reads it for the mounted eye height.
pub(super) const RIDER_LIFT: f32 = 0.90;
const RIDER_LEG_ANGLE: f32 = 1.05;
const RIDER_LEG_SPLAY: f32 = 0.34;
const RIDER_ARM_ANGLE: f32 = 0.75;

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

/// The root of one horse. Ridden roots are parented to a body; paddock roots stand in
/// world space, but both own the exact same mesh children and gait transforms.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Horse {
    pub(super) kind: MountKind,
}

/// A world-space paddock horse keyed by the opaque identity in MobState.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PaddockHorse(u64);

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

    /// Where the leg turns: the shoulder for a foreleg, the hip for a hind leg.
    const fn hip(self) -> Vec3 {
        match self {
            Self::LeftFront => Vec3::new(-LEG_PIVOT_X, LEG_PIVOT_Y, FRONT_PIVOT_Z),
            Self::RightFront => Vec3::new(LEG_PIVOT_X, LEG_PIVOT_Y, FRONT_PIVOT_Z),
            Self::LeftRear => Vec3::new(-LEG_PIVOT_X, LEG_PIVOT_Y, REAR_PIVOT_Z),
            Self::RightRear => Vec3::new(LEG_PIVOT_X, LEG_PIVOT_Y, REAR_PIVOT_Z),
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

/// A solid lofted along z between two slices, the rear one (+z) first.
fn lofted_along_z(rear: Slice, front: Slice) -> Mesh {
    hexahedron([
        Vec3::new(-rear.half_x, rear.y.0, rear.z),
        Vec3::new(rear.half_x, rear.y.0, rear.z),
        Vec3::new(front.half_x, front.y.0, front.z),
        Vec3::new(-front.half_x, front.y.0, front.z),
        Vec3::new(-rear.half_x, rear.y.1, rear.z),
        Vec3::new(rear.half_x, rear.y.1, rear.z),
        Vec3::new(front.half_x, front.y.1, front.z),
        Vec3::new(-front.half_x, front.y.1, front.z),
    ])
}

/// A solid lofted along y between two decks, the lower one first.
fn lofted_along_y(bottom: Deck, top: Deck) -> Mesh {
    hexahedron([
        Vec3::new(-bottom.half_x, bottom.y, bottom.z.1),
        Vec3::new(bottom.half_x, bottom.y, bottom.z.1),
        Vec3::new(bottom.half_x, bottom.y, bottom.z.0),
        Vec3::new(-bottom.half_x, bottom.y, bottom.z.0),
        Vec3::new(-top.half_x, top.y, top.z.1),
        Vec3::new(top.half_x, top.y, top.z.1),
        Vec3::new(top.half_x, top.y, top.z.0),
        Vec3::new(-top.half_x, top.y, top.z.0),
    ])
}

/// Chest, loin and croup alone, so a back and a belly can be measured with no neck in
/// the way.
fn horse_barrel_mesh() -> Mesh {
    let mut barrel = lofted_along_z(GIRTH, BREAST);
    merge_all(
        &mut barrel,
        [lofted_along_z(LOIN, GIRTH), lofted_along_z(RUMP, LOIN)],
        "horse barrel",
    );
    barrel
}

fn horse_body_mesh() -> Mesh {
    let mut body = horse_barrel_mesh();
    merge_all(
        &mut body,
        [lofted_along_y(NECK_BASE, NECK_POLL)],
        "horse body",
    );
    body
}

fn horse_head_mesh() -> Mesh {
    let mut head = lofted_along_z(BROW, MUZZLE);
    let ears = [-1.0, 1.0].map(|side| {
        Mesh::from(Cuboid::from_size(EAR)).translated_by(EAR_CENTRE * Vec3::new(side, 1.0, 1.0))
    });
    merge_all(&mut head, ears, "horse head");
    head
}

/// One leg authored downwards from its pivot, shared by all four leg children.
fn horse_leg_mesh() -> Mesh {
    let mut leg =
        Mesh::from(Cuboid::from_size(HORSE_LEG)).translated_by(Vec3::Y * -HORSE_LEG.y / 2.0);
    merge_all(&mut leg, [lofted_along_y(HOOF_SOLE, HOOF_TOP)], "horse leg");
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
    let flaps = [-1.0, 1.0].map(|side| {
        Mesh::from(Cuboid::from_size(SADDLE_FLAP))
            .translated_by(SADDLE_FLAP_CENTRE * Vec3::new(side, 1.0, 1.0))
    });
    let reins = [-1.0, 1.0].map(|side| {
        let mirror = Vec3::new(side, 1.0, 1.0);
        bar_between(REIN_BIT * mirror, REIN_HAND * mirror, REIN_WIDTH)
    });
    merge_all(&mut saddle, flaps.into_iter().chain(reins), "horse tack");
    saddle
}

/// Two eyes on the sides of the head, where a horse's are: each a rectangle turned to
/// face its own side and set a hair proud of the tapering cheek.
fn horse_eye_mesh() -> Mesh {
    let [mut left, right] = [-1.0_f32, 1.0].map(|side| {
        Mesh::from(Rectangle::new(EYE.x, EYE.y)).transformed_by(
            Transform::from_translation(EYE_CENTRE * Vec3::new(side, 1.0, 1.0))
                .with_rotation(Quat::from_rotation_y(side * FRAC_PI_2)),
        )
    });
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
                spawn_horse_parts(horse, visuals, &material);
            });
    });
}

fn spawn_horse_parts(
    horse: &mut ChildSpawnerCommands<'_>,
    visuals: &HorseVisuals,
    material: &Handle<StandardMaterial>,
) {
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
}

// The server leaves entity ids opaque to gameplay. Its three stable anchors receive
// low-bit presentation seeds 0, 1 and 2 exactly once; this total cosmetic mapping turns
// that seed into one of the existing coat materials and is never sent back.
const fn paddock_coat(entity_id: u64) -> MountKind {
    match entity_id & 0b11 {
        0 => MountKind::BlackHorse,
        1 => MountKind::BrownHorse,
        _ => MountKind::GreyHorse,
    }
}

/// Reconciles world-space horses with Horse rows in the complete mob projection.
pub(super) fn sync_paddock_horses(
    buffer: Res<SnapshotBuffer>,
    session: Option<Res<Session>>,
    mode: Res<InputMode>,
    visuals: Res<HorseVisuals>,
    mut existing: Query<(
        Entity,
        &PaddockHorse,
        &mut Transform,
        &mut WalkPose,
        &mut Visibility,
    )>,
    mut commands: Commands,
) {
    let Some(session) = session else {
        return;
    };
    let interval = Duration::from_secs(1) / u32::from(session.0.tick_rate);
    let drawn: HashMap<_, _> = buffer
        .sample_mobs(Instant::now(), interval)
        .into_iter()
        .filter(|(_, state)| state.kind == MobKind::Horse)
        .collect();
    let visibility = super::mobs::mob_visibility(*mode);
    let mut placed = HashMap::with_capacity(drawn.len());

    for (entity, horse, mut transform, mut walk, mut shown) in &mut existing {
        let Some(state) = drawn.get(&horse.0) else {
            commands.entity(entity).despawn();
            continue;
        };
        transform.translation = state.pos;
        transform.rotation = Quat::from_rotation_y(state.yaw);
        *walk = WalkPose {
            phase: state.walk_phase,
            moving: state.walking,
        };
        *shown = visibility;
        placed.insert(horse.0, ());
    }

    for (entity_id, state) in drawn {
        if placed.contains_key(&entity_id) {
            continue;
        }
        let kind = paddock_coat(entity_id);
        let material = visuals.material(kind);
        commands
            .spawn((
                Horse { kind },
                PaddockHorse(entity_id),
                WalkPose {
                    phase: state.walk_phase,
                    moving: state.walking,
                },
                Transform::from_translation(state.pos)
                    .with_rotation(Quat::from_rotation_y(state.yaw)),
                visibility,
            ))
            .with_children(|horse| spawn_horse_parts(horse, &visuals, &material));
    }
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
type RiddenHorseQuery<'w, 's> =
    Query<'w, 's, (&'static ChildOf, &'static Children), (With<Horse>, Without<PaddockHorse>)>;
type PaddockHorseQuery<'w, 's> =
    Query<'w, 's, (&'static WalkPose, &'static Children), With<PaddockHorse>>;

/// Poses four independently transformed legs from the owning body's distance sample.
pub(super) fn animate_gait(
    bodies: Query<&WalkPose>,
    horses: RiddenHorseQuery<'_, '_>,
    paddock_horses: PaddockHorseQuery<'_, '_>,
    mut moving_parts: MovingHorsePartQuery<'_, '_>,
) {
    for (parent, children) in &horses {
        let Ok(walk) = bodies.get(parent.parent()) else {
            continue;
        };
        pose_horse(children, *walk, &mut moving_parts);
    }
    for (walk, children) in &paddock_horses {
        pose_horse(children, *walk, &mut moving_parts);
    }
}

fn pose_horse(
    children: &Children,
    walk: WalkPose,
    moving_parts: &mut MovingHorsePartQuery<'_, '_>,
) {
    for child in children {
        let Ok((leg, hair, mut transform)) = moving_parts.get_mut(*child) else {
            continue;
        };
        let next = match (leg, hair) {
            (Some(leg), None) => gait_transform(leg.0, walk),
            (None, Some(hair)) => hair_transform(*hair, walk),
            _ => continue,
        };
        if *transform != next {
            *transform = next;
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
    let (root, rest, swing) = match hair {
        HorseHair::Mane => (MANE_ROOT, MANE_REST, MANE_SWING),
        HorseHair::Tail => (TAIL_ROOT, TAIL_REST, TAIL_SWING),
    };
    Transform::from_translation(root).with_rotation(Quat::from_rotation_x(rest + phase * swing))
}

/// The existing humanoid piece in its seated pose: every pivot lifted onto the saddle,
/// the arms forward to the reins, and each leg folded forward after a second rotation —
/// about the forward axis, at the same hip — that splays it outward over the barrel.
pub(super) fn rider_piece_transform(piece: BodyPiece, blocking: bool) -> Transform {
    let (angle, splay) = match piece.limb() {
        Some(Limb::LeftArm) if blocking => (-1.05, 0.0),
        Some(Limb::LeftArm | Limb::RightArm) => (RIDER_ARM_ANGLE, 0.0),
        Some(Limb::LeftLeg) => (RIDER_LEG_ANGLE, -RIDER_LEG_SPLAY),
        Some(Limb::RightLeg) => (RIDER_LEG_ANGLE, RIDER_LEG_SPLAY),
        None => (0.0, 0.0),
    };
    Transform::from_translation(piece.pivot() + Vec3::Y * RIDER_LIFT)
        .with_rotation(Quat::from_rotation_x(angle) * Quat::from_rotation_z(splay))
}

#[cfg(test)]
mod tests {
    use bevy::asset::AssetPlugin;
    use bevy::mesh::VertexAttributeValues;

    use super::super::constants::{MOUNTED_HEIGHT, MOUNTED_WIDTH, PLAYER_HEIGHT};
    use super::super::{ANY_HAIR, piece_mesh};
    use super::*;
    use crate::net::HairModel;

    fn positions(meshes: &[Mesh]) -> Vec<Vec3> {
        meshes
            .iter()
            .flat_map(|mesh| {
                let Some(VertexAttributeValues::Float32x3(positions)) =
                    mesh.attribute(Mesh::ATTRIBUTE_POSITION)
                else {
                    panic!("horse mesh has no positions");
                };
                positions.iter().copied().map(Vec3::from_array)
            })
            .collect()
    }

    fn extent(meshes: &[Mesh]) -> (Vec3, Vec3) {
        positions(meshes)
            .into_iter()
            .fold((Vec3::MAX, Vec3::MIN), |(low, high), point| {
                (low.min(point), high.max(point))
            })
    }

    fn walking(phase: f32) -> WalkPose {
        WalkPose {
            phase,
            moving: true,
        }
    }

    /// Standing, and the two extremes of the stride.
    fn gaits() -> [WalkPose; 3] {
        [
            WalkPose::default(),
            walking(PI / 2.0),
            walking(3.0 * PI / 2.0),
        ]
    }

    /// Every part of the horse posed at `pose`, with no rider on it.
    fn horse_meshes(pose: WalkPose) -> Vec<Mesh> {
        let mut meshes = vec![horse_body_mesh(), horse_head_mesh()];
        meshes
            .extend(Leg::ALL.map(|leg| horse_leg_mesh().transformed_by(gait_transform(leg, pose))));
        meshes.extend([
            horse_mane_mesh().transformed_by(hair_transform(HorseHair::Mane, pose)),
            horse_tail_mesh().transformed_by(hair_transform(HorseHair::Tail, pose)),
            horse_tack_mesh(),
            horse_eye_mesh(),
        ]);
        meshes
    }

    /// One piece of the humanoid rig in the saddle.
    fn rider_mesh(piece: BodyPiece, model: HairModel, blocking: bool) -> Mesh {
        piece_mesh(piece, model).transformed_by(rider_piece_transform(piece, blocking))
    }

    #[test]
    fn the_standing_horse_has_the_proportions_of_a_horse() {
        let (belly, withers) = extent(&[horse_barrel_mesh()]);
        assert!(
            (1.50..=1.60).contains(&withers.y),
            "back top at {}",
            withers.y
        );
        assert!(belly.y >= 0.80, "belly bottom at {}", belly.y);
        let width = withers.x - belly.x;
        assert!((0.62..=0.70).contains(&width), "barrel {width} wide");
        assert!(
            (withers.x + belly.x).abs() < 1e-6,
            "barrel off the centre line"
        );

        let (nose, ears) = extent(&[horse_head_mesh()]);
        assert!((2.15..=2.25).contains(&ears.y), "ear tips at {}", ears.y);
        assert!(
            (ears.y - HORSE_HEIGHT).abs() < 1e-6,
            "HORSE_HEIGHT is not the ear tips"
        );
        assert!(
            nose.z <= -1.10,
            "nose at z {}: the head is above the chest",
            nose.z
        );
        let length = withers.z - nose.z;
        assert!((2.20..=2.40).contains(&length), "nose to rump {length}");

        let (_, whole) = extent(&horse_meshes(WalkPose::default()));
        assert!(
            whole.y <= HORSE_HEIGHT + 1e-6,
            "something stands above the ears: {}",
            whole.y
        );
    }

    /// What the `const` asserts beside the constants cannot state: a slope, and a
    /// measurement on the mesh itself.
    #[test]
    fn the_neck_rises_at_a_lean_and_the_head_narrows_to_the_muzzle() {
        let base_z = (NECK_BASE.z.0 + NECK_BASE.z.1) / 2.0;
        let poll_z = (NECK_POLL.z.0 + NECK_POLL.z.1) / 2.0;
        let rise = (NECK_POLL.y - NECK_BASE.y)
            .atan2(base_z - poll_z)
            .to_degrees();
        assert!((45.0..=60.0).contains(&rise), "the neck rises at {rise}°");

        let muzzle = positions(&[horse_head_mesh()])
            .into_iter()
            .filter(|point| point.z <= MUZZLE.z + 1e-4)
            .map(|point| point.x.abs())
            .fold(0.0_f32, f32::max);
        assert!(
            muzzle < BROW.half_x,
            "the muzzle is as wide as the brow: {muzzle}"
        );
    }

    /// The server collides a mounted body `MOUNTED_WIDTH` square and `MOUNTED_HEIGHT`
    /// tall; horse, tack and rider are drawn inside it across and up at every gait phase.
    /// **Not along.** That footprint is square and set from the horse's *width* — the
    /// server's own reasoning at `MountedWidth` — so a horse over two blocks long
    /// overhangs the square at the nose and the tail by construction, and the test says
    /// so rather than pretending otherwise.
    #[test]
    fn horse_tack_and_rider_fit_the_mounted_body_across_and_up_at_every_gait_phase() {
        let half = MOUNTED_WIDTH / 2.0;
        for pose in gaits() {
            let mut meshes = horse_meshes(pose);
            for blocking in [false, true] {
                meshes.extend(BodyPiece::FIXED.map(|piece| rider_mesh(piece, ANY_HAIR, blocking)));
            }
            let (low, high) = extent(&meshes);
            assert!(
                low.x >= -half && high.x <= half,
                "at {pose:?} x = {}..{}",
                low.x,
                high.x
            );
            assert!(
                low.y >= -1e-2 && high.y <= MOUNTED_HEIGHT,
                "at {pose:?} y = {}..{}",
                low.y,
                high.y
            );
            assert!(
                high.z - low.z > MOUNTED_WIDTH,
                "at {pose:?} the horse fits the square lengthways too, so the box was set \
                 from the length rather than the width"
            );
        }

        // Hair is the one part that may leave the box, and that is the rig's decision
        // rather than the saddle's: `appearance::envelope` names the topknot as leaving
        // the walking box upwards. The saddle adds no overshoot of its own — whatever a
        // model pokes above this box, it already pokes above the walking box by at least
        // as much.
        for model in HairModel::ALL {
            let (_, on_foot) = extent(&[piece_mesh(BodyPiece::Hair, model)]);
            let (_, mounted) = extent(&[rider_mesh(BodyPiece::Hair, model, false)]);
            let allowed = (on_foot.y - PLAYER_HEIGHT).max(0.0);
            assert!(
                mounted.y - MOUNTED_HEIGHT <= allowed + 1e-6,
                "{model:?} leaves the mounted box by {} and the walking box by only {allowed}",
                mounted.y - MOUNTED_HEIGHT
            );
        }
    }

    #[test]
    fn hooves_rest_on_the_feet_plane_standing() {
        for leg in Leg::ALL {
            let standing =
                horse_leg_mesh().transformed_by(gait_transform(leg, WalkPose::default()));
            let (low, _) = extent(&[standing]);
            assert!(low.y.abs() < 1e-3, "{leg:?} stands at y {}", low.y);
        }
    }

    #[test]
    fn legs_pivot_at_the_shoulder_and_the_hip_and_a_hoof_sweeps_under_a_third_of_a_block() {
        let (belly, withers) = extent(&[horse_barrel_mesh()]);
        let sole = Vec3::new(0.0, HOOF_SOLE.y, (HOOF_SOLE.z.0 + HOOF_SOLE.z.1) / 2.0);
        for leg in Leg::ALL {
            let pivot = leg.hip();
            let wanted_z = match leg {
                Leg::LeftFront | Leg::RightFront => -0.45,
                Leg::LeftRear | Leg::RightRear => 0.55,
            };
            assert!(
                (pivot.x.abs() - 0.20).abs() < 1e-6 && (pivot.z - wanted_z).abs() < 1e-6,
                "{leg:?} pivots at {pivot}"
            );
            assert!(
                pivot.y > belly.y && pivot.y < withers.y,
                "{leg:?} pivots outside the barrel at y {}",
                pivot.y
            );
            let forward = gait_transform(leg, walking(PI / 2.0)).transform_point(sole);
            let back = gait_transform(leg, walking(3.0 * PI / 2.0)).transform_point(sole);
            let sweep = (forward.z - back.z).abs();
            assert!(sweep > 0.2 && sweep <= HOOF_SWEEP, "{leg:?} sweeps {sweep}");
        }
    }

    #[test]
    fn the_rider_sits_on_the_saddle_astride_the_barrel_with_the_reins_in_hand() {
        let hip = BodyPiece::LeftTrouser.pivot().y + RIDER_LIFT;
        let seat = SADDLE_CENTRE.y + SADDLE.y / 2.0;
        assert!(
            (hip - seat).abs() < 0.02,
            "hip at {hip}, saddle seat at {seat}"
        );

        // Each leg is splayed outward as well as folded forward, so the foot hangs beside
        // the barrel rather than through it. The foot is taken at the centre of its sole:
        // the rig's shoe is 0.25 wide, and no rotation about the hip fits all of a sole
        // that wide between the barrel's side at 0.33 and the box's at 0.50 — its inner
        // edge sits against the horse, as a rider's heel does.
        let (belly, _) = extent(&[horse_barrel_mesh()]);
        for (shoe, side) in [(BodyPiece::LeftShoe, -1.0), (BodyPiece::RightShoe, 1.0)] {
            let mesh = piece_mesh(shoe, ANY_HAIR);
            let (low, _) = extent(std::slice::from_ref(&mesh));
            let sole: Vec<Vec3> = positions(std::slice::from_ref(&mesh))
                .into_iter()
                .filter(|point| (point.y - low.y).abs() < 1e-6)
                .collect();
            let sole = sole.iter().sum::<Vec3>() / sole.len() as f32;
            let seated = rider_piece_transform(shoe, false);
            let foot = seated.transform_point(sole);
            assert!(
                foot.x * side >= GIRTH.half_x,
                "the {shoe:?} hangs through the barrel at x {}",
                foot.x
            );
            assert!(
                foot.y > belly.y,
                "the {shoe:?} hangs below the belly at y {}",
                foot.y
            );
            let (fold, _, splay) = seated.rotation.to_euler(EulerRot::XYZ);
            assert!(
                (fold - RIDER_LEG_ANGLE).abs() < 1e-5,
                "{shoe:?} folded by {fold}"
            );
            assert!(
                (splay - side * RIDER_LEG_SPLAY).abs() < 1e-5,
                "{shoe:?} splayed by {splay}"
            );
        }

        // The arms reach the reins: each rein ends inside the fist on its side, and starts
        // at the mouth.
        for (fist, side) in [(BodyPiece::LeftFist, -1.0), (BodyPiece::RightFist, 1.0)] {
            let hand = REIN_HAND * Vec3::new(side, 1.0, 1.0);
            let held = rider_piece_transform(fist, false)
                .compute_affine()
                .inverse()
                .transform_point3(hand);
            let (low, high) = extent(&[piece_mesh(fist, ANY_HAIR)]);
            assert!(
                held.cmpge(low).all() && held.cmple(high).all(),
                "the {fist:?} does not hold its rein: {held} outside {low}..{high}"
            );
        }
        let (low, high) = extent(&[lofted_along_z(BROW, MUZZLE)]);
        assert!(
            REIN_BIT.cmpge(low).all() && REIN_BIT.cmple(high).all(),
            "the bit is not in the mouth"
        );
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
        let stride = walking(PI / 2.0);
        for (hair, rest, want) in [
            (HorseHair::Mane, MANE_REST, MANE_SWING),
            (HorseHair::Tail, TAIL_REST, TAIL_SWING),
        ] {
            let pitch = hair_transform(hair, stride)
                .rotation
                .to_euler(EulerRot::XYZ)
                .0;
            assert!((pitch - rest - want).abs() < 1e-5);
            assert!(
                hair_transform(hair, WalkPose::default())
                    .rotation
                    .abs_diff_eq(Quat::from_rotation_x(rest), 1e-6)
            );
        }
        assert!((angle(Leg::LeftFront, stride) - HORSE_LEG_SWING).abs() < 1e-5);
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
    fn paddock_identity_seeds_choose_one_of_each_existing_coat() {
        assert_eq!(paddock_coat(0), MountKind::BlackHorse);
        assert_eq!(paddock_coat(1), MountKind::BrownHorse);
        assert_eq!(paddock_coat(2), MountKind::GreyHorse);
        // Total and cosmetic: a malformed fourth seed still gets a coat rather than
        // changing snapshot acceptance or inventing a gameplay state.
        assert_eq!(paddock_coat(3), MountKind::GreyHorse);
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
