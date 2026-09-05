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
//! collides. The gait is a transform on each of eight leg segments — an upper leg swinging
//! at the shoulder or the hip and, under it, a cannon folding at the knee or the hock —
//! and on the mane and the tail. Its only clock is [`WalkPose::phase`], which
//! interpolation advances from horizontal distance and which a [`Gait`] rescales to a
//! stride of its own: faster authoritative travel cycles the same rig faster, while
//! elapsed time over no distance cannot move a hoof.
//!
//! Which gait a horse uses is which kind of horse it is — a ridden one canters, a paddock
//! one walks — and no speed is read, mirrored or inferred to decide it.

use std::collections::HashMap;
use std::f32::consts::{FRAC_PI_2, PI};
use std::time::{Duration, Instant};

use bevy::asset::RenderAssetUsages;
use bevy::camera::primitives::MeshAabb;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use super::appearance::{BodyPiece, Limb};
use super::interpolate::{SnapshotBuffer, WALK_STRIDE_BLOCKS};
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
pub(super) struct Slice {
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
pub(super) struct Deck {
    y: f32,
    half_x: f32,
    z: (f32, f32),
}

impl Deck {
    const fn new(y: f32, half_x: f32, z: (f32, f32)) -> Self {
        Self { y, half_x, z }
    }

    /// Continue a view's neck below its withers without re-authoring the footprint.
    pub(super) fn lowered(self, distance: f32) -> Self {
        Self {
            y: self.y - distance,
            ..self
        }
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
pub(super) const NECK_BASE: Deck = Deck::new(1.44, 0.16, (-0.78, -0.40));
pub(super) const NECK_POLL: Deck = Deck::new(1.94, 0.11, (-1.12, -0.96));
pub(super) const BROW: Slice = Slice::new(-0.96, 0.15, (1.66, 2.08));
pub(super) const MUZZLE: Slice = Slice::new(-1.52, 0.09, (1.42, 1.64));
pub(super) const EAR_CENTRE: Vec3 = Vec3::new(0.09, 2.10, -1.02);
const EAR_BASE: Deck = Deck::new(2.00, 0.03, (-1.045, -0.995));
const EAR_TIP: Deck = Deck::new(2.20, 0.003, (-1.0225, -1.0175));
const EYE: Vec2 = Vec2::new(0.05, 0.05);
const EYE_CENTRE: Vec3 = Vec3::new(0.135, 1.84, -1.14);

// The barrel turns about the girth and the neck about its own base, each where it
// stands: every mesh stays authored in horse space and a part at rest is the identity.
const BARREL_PIVOT: Vec3 = Vec3::new(0.0, (GIRTH.y.0 + GIRTH.y.1) / 2.0, GIRTH.z);
const NECK_PIVOT: Vec3 = Vec3::new(0.0, NECK_BASE.y, (NECK_BASE.z.0 + NECK_BASE.z.1) / 2.0);
const DEGREE: f32 = PI / 180.0;

/// How tall the drawn horse is: its ear tips. `mobs.rs` reads it as the presentation
/// envelope of a paddock horse.
pub(super) const HORSE_HEIGHT: f32 = EAR_TIP.y;

// Each leg is two segments. The upper runs from its pivot — the shoulder for a foreleg,
// the hip for a hind leg, both inside the barrel — down to the knee or the hock, cut at
// one height for all four; the lower is the cannon and a hoof wider at the ground, a
// child of the upper that pivots at that joint. The joint therefore follows the shoulder
// for free, and nothing solves for where it is. Both segments are authored downwards from
// their own pivot, so a rotation about the origin is a rotation about the joint.
const LEG_PIVOT_X: f32 = 0.20;
const LEG_PIVOT_Y: f32 = 1.12;
const FRONT_PIVOT_Z: f32 = -0.45;
const REAR_PIVOT_Z: f32 = 0.55;
const KNEE_Y: f32 = 0.50;
const HOOF_HEIGHT: f32 = 0.10;
const UPPER_LEG: Vec3 = Vec3::new(0.12, LEG_PIVOT_Y - KNEE_Y, 0.14);
const LOWER_LEG: Vec3 = Vec3::new(0.12, KNEE_Y - HOOF_HEIGHT, 0.14);
/// The knee in its upper segment's frame: straight below the pivot, where the segments meet.
const KNEE: Vec3 = Vec3::new(0.0, -UPPER_LEG.y, 0.0);
const HOOF_TOP: Deck = Deck::new(
    HOOF_HEIGHT - KNEE_Y,
    LOWER_LEG.x / 2.0,
    (-LOWER_LEG.z / 2.0, LOWER_LEG.z / 2.0),
);
const HOOF_SOLE: Deck = Deck::new(-KNEE_Y, 0.08, (-0.10, 0.08));

// The mane lies along the crest from the poll to the withers and the tail hangs from the
// croup: each a strip authored downwards from its root and turned to rest along its line.
pub(super) const MANE_ROOT: Vec3 = Vec3::new(0.0, 1.96, -0.94);
pub(super) const MANE_STRIP: Vec3 = Vec3::new(0.055, 0.76, 0.05);
pub(super) const MANE_REST: f32 = -0.84;
const MANE_SWING: f32 = 0.035;
const TAIL_ROOT: Vec3 = Vec3::new(0.0, 1.38, 0.72);
const TAIL_STRIP: Vec3 = Vec3::new(0.075, 0.74, 0.05);
const TAIL_REST: f32 = -0.12;
const TAIL_SWING: f32 = 0.10;

// The saddle seat sits on the back behind the withers with a flap down each side; the
// reins run from the corners of the mouth along the neck to the rider's fists, and are
// re-aimed every frame because the mouth moves with the neck and the fists do not.
const SADDLE: Vec3 = Vec3::new(0.44, 0.07, 0.36);
const SADDLE_CENTRE: Vec3 = Vec3::new(0.0, 1.575, 0.02);
const SADDLE_FLAP: Vec3 = Vec3::new(0.035, 0.30, 0.26);
const SADDLE_FLAP_CENTRE: Vec3 = Vec3::new(0.345, 1.42, 0.02);
pub(super) const REIN_BIT: Vec3 = Vec3::new(0.095, 1.53, -1.40);
const REIN_HAND: Vec3 = Vec3::new(0.27, 1.82, -0.28);
pub(super) const REIN_WIDTH: f32 = 0.018;

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
const _: () = assert!(EAR_TIP.y > EAR_BASE.y);
const _: () = assert!(MUZZLE.y.1 - MUZZLE.y.0 < BROW.y.1 - BROW.y.0);
const _: () = assert!(HOOF_TOP.y > HOOF_SOLE.y && HOOF_TOP.half_x < HOOF_SOLE.half_x);
const _: () = assert!(HOOF_TOP.z.1 - HOOF_TOP.z.0 < HOOF_SOLE.z.1 - HOOF_SOLE.z.0);
const _: () = assert!(KNEE_Y > HOOF_HEIGHT && KNEE_Y < LEG_PIVOT_Y);

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
pub(super) const HAIR_COLOUR: Color = Color::srgb(0.055, 0.045, 0.038);
pub(super) const LEATHER_COLOUR: Color = Color::srgb(0.25, 0.105, 0.045);
pub(super) const EYE_COLOUR: Color = Color::srgb(0.82, 0.61, 0.18);

#[derive(Resource, Clone)]
pub(super) struct HorseCoats {
    pub(super) image: Handle<Image>,
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
    barrel: Handle<Mesh>,
    neck: Handle<Mesh>,
    head: Handle<Mesh>,
    rein: Handle<Mesh>,
    upper_leg: Handle<Mesh>,
    lower_leg: Handle<Mesh>,
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

/// The exact coat colour shared by the world rig and every presentation of its token.
///
/// Keeping the match beside the named rig constants makes a coat retune one edit. Item icons,
/// held tokens and drops resolve this same value through the item display registry.
pub(super) const fn coat_colour(kind: MountKind) -> Color {
    match kind {
        MountKind::BlackHorse => BLACK_COAT,
        MountKind::BrownHorse => BROWN_COAT,
        MountKind::GreyHorse => GREY_COAT,
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

/// One transform the gait writes, and which part of the horse it moves.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HorseJoint {
    Leg(Leg, Segment),
    Hair(HorseHair),
    /// Chest, loin and croup, bobbing or rocking about the girth; carries the neck, the
    /// tack and the tail, while the legs hang from the root so the hooves stay down.
    Barrel,
    /// The neck, nodding or leaning about its base; carries the head, eyes and mane.
    Neck,
    /// One rein, aimed from the bit where the neck has put it to the fist on its side.
    Rein(Side),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Side {
    Left,
    Right,
}

impl Side {
    const BOTH: [Self; 2] = [Self::Left, Self::Right];

    const fn mirror(self) -> Vec3 {
        match self {
            Self::Left => Vec3::new(-1.0, 1.0, 1.0),
            Self::Right => Vec3::ONE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Leg {
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

    /// This leg's place in [`Self::ALL`], and in every table written in that order.
    const fn index(self) -> usize {
        match self {
            Self::LeftFront => 0,
            Self::RightFront => 1,
            Self::LeftRear => 2,
            Self::RightRear => 3,
        }
    }

    /// Where the leg turns: the shoulder for a foreleg, the hip for a hind leg.
    const fn pivot(self) -> Vec3 {
        match self {
            Self::LeftFront => Vec3::new(-LEG_PIVOT_X, LEG_PIVOT_Y, FRONT_PIVOT_Z),
            Self::RightFront => Vec3::new(LEG_PIVOT_X, LEG_PIVOT_Y, FRONT_PIVOT_Z),
            Self::LeftRear => Vec3::new(-LEG_PIVOT_X, LEG_PIVOT_Y, REAR_PIVOT_Z),
            Self::RightRear => Vec3::new(LEG_PIVOT_X, LEG_PIVOT_Y, REAR_PIVOT_Z),
        }
    }
}

/// The two pieces of one leg.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Segment {
    /// Shoulder or hip down to the knee or the hock; a child of the horse.
    Upper,
    /// Cannon and hoof; a child of the upper segment, turning at the joint.
    Lower,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HorseHair {
    Mane,
    Tail,
}

/// One way of moving: how far the horse travels in a cycle of its legs, when in that
/// cycle each hoof lands, and how far the joints go.
///
/// A gait reads the distance phase and rescales it, and that is all it reads: nothing here
/// is a speed, and no gait is chosen from one. The server holds a ridden horse at
/// `MountSpeed` and the paddock lap at a walk, so which gait a horse uses follows from
/// which kind of horse it is, and the phase alone says where in the cycle it stands.
struct Gait {
    /// Blocks per cycle. A drawn stride length, documented the way
    /// `WALK_RADIANS_PER_BLOCK` is, and no more a speed than that one: the server still
    /// owns how quickly the blocks are covered.
    stride: f32,
    /// Where in the cycle each leg lands, in [`Leg::ALL`] order.
    beats: [f32; 4],
    /// How far an upper segment swings either way from vertical.
    swing: f32,
    /// How far a lower segment folds back at the top of its swing.
    fold: f32,
    /// How many times a cycle the body moves: twice at a walk, once for each fore hoof,
    /// and once at a canter, with the leading fore.
    sways: f32,
    /// How far the girth rises and falls, and how far the barrel pitches about it.
    bob: f32,
    rock: f32,
    /// The neck's pitch forward of rest while moving — negative lowers the poll — and
    /// how far it nods either way of that.
    lean: f32,
    nod: f32,
}

/// The walk: a lateral four-beat sequence — left rear, left front, right rear, right
/// front — each a quarter of a cycle behind the last, one cycle per 1.7 blocks.
const WALK: Gait = Gait {
    stride: 1.7,
    beats: [FRAC_PI_2, 3.0 * FRAC_PI_2, 0.0, PI],
    swing: 0.32,
    fold: 0.60,
    sways: 2.0,
    bob: 0.02,
    rock: 0.0,
    lean: 0.0,
    nod: 4.0 * DEGREE,
};

/// The canter: three beats and a suspension, one cycle per 3.4 blocks. On the right
/// lead the trailing hind — the left — lands first, the diagonal pair a quarter later,
/// the leading fore — the right — a quarter after that, and in the fourth quarter
/// nothing lands. The neck stretches forward and the barrel rocks once a stride.
const CANTER: Gait = Gait {
    stride: 3.4,
    beats: [FRAC_PI_2, PI, 0.0, FRAC_PI_2],
    swing: 0.45,
    fold: 1.0,
    sways: 1.0,
    bob: 0.0,
    rock: 3.0 * DEGREE,
    lean: -10.0 * DEGREE,
    nod: 3.0 * DEGREE,
};

impl Gait {
    /// Where in this gait's cycle the horse is, or `None` standing.
    fn cycle(&self, walk: WalkPose) -> Option<f32> {
        walk.moving
            .then(|| walk.phase * (WALK_STRIDE_BLOCKS / self.stride))
    }

    /// Where in its sway the body is, `sways` times a cycle, or `None` standing.
    fn sway(&self, walk: WalkPose) -> Option<f32> {
        self.cycle(walk).map(|cycle| (cycle * self.sways).sin())
    }

    /// One leg's two angles: the upper segment's swing, positive forward, and the lower
    /// segment's fold, positive folded. Both zero standing.
    ///
    /// A leg's own cycle starts at its footfall. The hoof is furthest forward there and
    /// travels back through the stance to lift half a cycle later, so the swing is the
    /// cosine; the cannon is straight for the whole of the stance and folds through the
    /// swing, most at its middle, so the fold is the negative half of the sine — squared,
    /// so it arrives at the ground and leaves it at rest.
    fn leg(&self, leg: Leg, walk: WalkPose) -> (f32, f32) {
        let Some(cycle) = self.cycle(walk) else {
            return (0.0, 0.0);
        };
        let own = cycle - self.beats[leg.index()];
        let lift = (-own.sin()).max(0.0);
        (own.cos() * self.swing, lift * lift * self.fold)
    }
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
        barrel: meshes.add(horse_barrel_mesh()),
        neck: meshes.add(horse_neck_mesh()),
        head: meshes.add(horse_head_mesh()),
        rein: meshes.add(horse_rein_mesh()),
        upper_leg: meshes.add(horse_upper_leg_mesh()),
        lower_leg: meshes.add(horse_lower_leg_mesh()),
        mane: meshes.add(horse_mane_mesh()),
        tail: meshes.add(horse_tail_mesh()),
        tack: meshes.add(horse_tack_mesh()),
        eyes: meshes.add(horse_eye_mesh()),
        black: materials.add(coat_material(
            coat_colour(MountKind::BlackHorse),
            &coats.image,
        )),
        brown: materials.add(coat_material(
            coat_colour(MountKind::BrownHorse),
            &coats.image,
        )),
        grey: materials.add(coat_material(
            coat_colour(MountKind::GreyHorse),
            &coats.image,
        )),
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
pub(super) fn lofted_along_z(rear: Slice, front: Slice) -> Mesh {
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
pub(super) fn lofted_along_y(bottom: Deck, top: Deck) -> Mesh {
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

/// The neck alone, from the withers to the poll.
fn horse_neck_mesh() -> Mesh {
    lofted_along_y(NECK_BASE, NECK_POLL)
}

/// One pointed ear in horse space, shared by the world horse and the saddle view.
pub(super) fn horse_ear_mesh(side: f32) -> Mesh {
    lofted_along_y(EAR_BASE, EAR_TIP).translated_by(Vec3::X * EAR_CENTRE.x * side)
}

fn horse_head_mesh() -> Mesh {
    let mut head = lofted_along_z(BROW, MUZZLE);
    let ears = [-1.0, 1.0].map(horse_ear_mesh);
    merge_all(&mut head, ears, "horse head");
    head
}

/// The world horse's own head, ears, poll and neck connection, normalised for item renderers.
///
/// A slight three-quarter turn exposes both the long face and its narrow brow when the held-item
/// camera or a world drop looks along Z. `edge` is the longest final extent; both item renderers
/// choose their own existing scale without re-authoring the silhouette.
pub(super) fn horse_head_item_mesh(edge: f32) -> Mesh {
    const CENTRE: Vec3 = Vec3::new(0.0, 1.82, -0.96);
    let mut head = horse_neck_mesh();
    merge_all(&mut head, [horse_head_mesh()], "horse-head item silhouette");

    let head = head
        .translated_by(-CENTRE)
        .rotated_by(Quat::from_rotation_y(-0.55));
    let bounds = head
        .compute_aabb()
        .expect("the horse-head lofts have positions");
    let longest_extent = 2.0 * bounds.half_extents.max_element();
    head.scaled_by(Vec3::splat(edge / longest_extent))
}

/// Shoulder or hip to the knee, authored downwards from the pivot; shared by all four.
fn horse_upper_leg_mesh() -> Mesh {
    Mesh::from(Cuboid::from_size(UPPER_LEG)).translated_by(Vec3::Y * -UPPER_LEG.y / 2.0)
}

/// Knee to the ground — the cannon and the hoof — authored downwards from the knee;
/// shared by all four.
fn horse_lower_leg_mesh() -> Mesh {
    let mut leg =
        Mesh::from(Cuboid::from_size(LOWER_LEG)).translated_by(Vec3::Y * -LOWER_LEG.y / 2.0);
    merge_all(
        &mut leg,
        [lofted_along_y(HOOF_SOLE, HOOF_TOP)],
        "horse lower leg",
    );
    leg
}

fn horse_mane_mesh() -> Mesh {
    Mesh::from(Cuboid::from_size(MANE_STRIP)).translated_by(Vec3::Y * -MANE_STRIP.y / 2.0)
}

fn horse_tail_mesh() -> Mesh {
    Mesh::from(Cuboid::from_size(TAIL_STRIP)).translated_by(Vec3::Y * -TAIL_STRIP.y / 2.0)
}

/// The transform that lays a unit bar along z from `start` to `end`.
fn bar_between(start: Vec3, end: Vec3) -> Transform {
    let axis = end - start;
    Transform {
        translation: (start + end) / 2.0,
        rotation: Quat::from_rotation_arc(Vec3::Z, axis.normalize()),
        scale: Vec3::new(1.0, 1.0, axis.length()),
    }
}

/// The saddle and its flaps: the reins are a joint of their own.
fn horse_tack_mesh() -> Mesh {
    let mut saddle = Mesh::from(Cuboid::from_size(SADDLE)).translated_by(SADDLE_CENTRE);
    let flaps = [-1.0, 1.0].map(|side| {
        Mesh::from(Cuboid::from_size(SADDLE_FLAP))
            .translated_by(SADDLE_FLAP_CENTRE * Vec3::new(side, 1.0, 1.0))
    });
    merge_all(&mut saddle, flaps, "horse tack");
    saddle
}

/// One rein: a unit bar along z that [`rein_transform`] aims and stretches from the bit
/// to the fist through [`bar_between`].
fn horse_rein_mesh() -> Mesh {
    Mesh::from(Cuboid::new(REIN_WIDTH, REIN_WIDTH, 1.0))
}

/// Two eyes on the sides of the head, where a horse's are: each a rectangle turned to
/// face its own side and set a hair proud of the tapering cheek.
pub(super) fn horse_eye_mesh() -> Mesh {
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
    // The barrel carries the neck, the tack and the tail; the neck carries the head, the
    // eyes and the mane; the legs and the reins hang from the root, where the ground and
    // the fists are.
    horse
        .spawn(jointed(HorseJoint::Barrel, &visuals.barrel, material))
        .with_children(|barrel| {
            barrel
                .spawn(jointed(HorseJoint::Neck, &visuals.neck, material))
                .with_children(|neck| {
                    neck.spawn(fixed(&visuals.head, material));
                    neck.spawn(fixed(&visuals.eyes, &visuals.eye));
                    let mane = HorseJoint::Hair(HorseHair::Mane);
                    neck.spawn(jointed(mane, &visuals.mane, &visuals.hair));
                });
            let tail = HorseJoint::Hair(HorseHair::Tail);
            barrel.spawn(jointed(tail, &visuals.tail, &visuals.hair));
            barrel.spawn(fixed(&visuals.tack, &visuals.leather));
        });
    for side in Side::BOTH {
        horse.spawn(jointed(
            HorseJoint::Rein(side),
            &visuals.rein,
            &visuals.leather,
        ));
    }
    for leg in Leg::ALL {
        let upper = HorseJoint::Leg(leg, Segment::Upper);
        let lower = HorseJoint::Leg(leg, Segment::Lower);
        horse
            .spawn((
                upper,
                Mesh3d(visuals.upper_leg.clone()),
                MeshMaterial3d(material.clone()),
                rest_transform(upper),
            ))
            .with_children(|upper| {
                upper.spawn((
                    lower,
                    Mesh3d(visuals.lower_leg.clone()),
                    MeshMaterial3d(material.clone()),
                    rest_transform(lower),
                ));
            });
    }
}

/// A part that never moves, drawn where its mesh was authored.
fn fixed(mesh: &Handle<Mesh>, material: &Handle<StandardMaterial>) -> impl Bundle {
    (
        HorsePart,
        Mesh3d(mesh.clone()),
        MeshMaterial3d(material.clone()),
        Transform::default(),
    )
}

/// A part the gait moves, spawned at rest.
fn jointed(
    joint: HorseJoint,
    mesh: &Handle<Mesh>,
    material: &Handle<StandardMaterial>,
) -> impl Bundle {
    (
        HorsePart,
        joint,
        Mesh3d(mesh.clone()),
        MeshMaterial3d(material.clone()),
        rest_transform(joint),
    )
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

pub(super) type MovingHorsePartQuery<'w, 's> =
    Query<'w, 's, (&'static HorseJoint, &'static mut Transform)>;
type RiddenHorseQuery<'w, 's> =
    Query<'w, 's, (Entity, &'static ChildOf), (With<Horse>, Without<PaddockHorse>)>;
type PaddockHorseQuery<'w, 's> = Query<'w, 's, (Entity, &'static WalkPose), With<PaddockHorse>>;

/// Poses every joint of every horse from its owning body's or its own row's distance
/// sample.
///
/// A ridden horse canters whenever it moves and a paddock horse walks, because the
/// server holds the one at `MountSpeed` and the other on a lap at a walk: the selection
/// is which route spawned the horse. No speed is mirrored here to compare against and
/// none is inferred from the phase, which would be the same mirror one step removed.
pub(super) fn animate_gait(
    bodies: Query<&WalkPose>,
    horses: RiddenHorseQuery<'_, '_>,
    paddock_horses: PaddockHorseQuery<'_, '_>,
    children: Query<&Children>,
    mut joints: MovingHorsePartQuery<'_, '_>,
) {
    for (horse, parent) in &horses {
        let Ok(walk) = bodies.get(parent.parent()) else {
            continue;
        };
        pose_horse(horse, *walk, &CANTER, &children, &mut joints);
    }
    for (horse, walk) in &paddock_horses {
        pose_horse(horse, *walk, &WALK, &children, &mut joints);
    }
}

/// Writes every joint under one horse root, and only the ones that change: a lower leg
/// is a grandchild, so this walks descendants rather than children.
fn pose_horse(
    horse: Entity,
    walk: WalkPose,
    gait: &Gait,
    children: &Query<&Children>,
    joints: &mut MovingHorsePartQuery<'_, '_>,
) {
    for part in children.iter_descendants(horse) {
        let Ok((joint, mut transform)) = joints.get_mut(part) else {
            continue;
        };
        let next = joint_transform(*joint, gait, walk);
        if *transform != next {
            *transform = next;
        }
    }
}

fn joint_transform(joint: HorseJoint, gait: &Gait, walk: WalkPose) -> Transform {
    match joint {
        HorseJoint::Leg(leg, segment) => leg_transform(leg, segment, gait, walk),
        HorseJoint::Hair(hair) => hair_transform(hair, gait, walk),
        HorseJoint::Barrel => barrel_transform(gait, walk),
        HorseJoint::Neck => neck_transform(gait, walk),
        HorseJoint::Rein(side) => rein_transform(side, gait, walk),
    }
}

/// A part authored in horse space, turned about `pivot` where it stands and raised by
/// `rise`: its rest is the identity, and so is every fixed part's under it.
fn turned_in_place(pivot: Vec3, rotation: Quat, rise: f32) -> Transform {
    Transform {
        translation: pivot - rotation * pivot + Vec3::Y * rise,
        rotation,
        scale: Vec3::ONE,
    }
}

/// The barrel bobs and rocks about the girth, `sways` times a cycle.
fn barrel_transform(gait: &Gait, walk: WalkPose) -> Transform {
    let Some(sway) = gait.sway(walk) else {
        return Transform::default();
    };
    turned_in_place(
        BARREL_PIVOT,
        Quat::from_rotation_x(sway * gait.rock),
        sway * gait.bob,
    )
}

/// The neck leans forward while moving and nods about its base, in the barrel's frame.
fn neck_transform(gait: &Gait, walk: WalkPose) -> Transform {
    let Some(sway) = gait.sway(walk) else {
        return Transform::default();
    };
    turned_in_place(
        NECK_PIVOT,
        Quat::from_rotation_x(gait.lean + sway * gait.nod),
        0.0,
    )
}

/// One rein from the bit, wherever the barrel and the neck have carried it, to the fist
/// on its side, which sits on the rider and moves with neither.
fn rein_transform(side: Side, gait: &Gait, walk: WalkPose) -> Transform {
    let bit = (barrel_transform(gait, walk) * neck_transform(gait, walk))
        .transform_point(REIN_BIT * side.mirror());
    bar_between(bit, REIN_HAND * side.mirror())
}

/// A joint standing still. The gait is immaterial there — every one puts every joint at
/// rest — so the walk stands in for all of them.
fn rest_transform(joint: HorseJoint) -> Transform {
    joint_transform(joint, &WALK, WalkPose::default())
}

/// The upper segment turns about the shoulder or the hip, forward for a positive angle;
/// the lower turns about the knee in the upper's frame, and folds backward — the hoof
/// trails the joint through the swing, as a knee and a hock both bend.
fn leg_transform(leg: Leg, segment: Segment, gait: &Gait, walk: WalkPose) -> Transform {
    let (swing, fold) = gait.leg(leg, walk);
    let (translation, angle) = match segment {
        Segment::Upper => (leg.pivot(), swing),
        Segment::Lower => (KNEE, -fold),
    };
    Transform::from_translation(translation).with_rotation(Quat::from_rotation_x(angle))
}

fn hair_transform(hair: HorseHair, gait: &Gait, walk: WalkPose) -> Transform {
    let swing = gait.cycle(walk).map_or(0.0, f32::sin);
    let (root, rest, reach) = match hair {
        HorseHair::Mane => (MANE_ROOT, MANE_REST, MANE_SWING),
        HorseHair::Tail => (TAIL_ROOT, TAIL_REST, TAIL_SWING),
    };
    Transform::from_translation(root).with_rotation(Quat::from_rotation_x(rest + swing * reach))
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
    use std::f32::consts::TAU;

    use bevy::asset::AssetPlugin;
    use bevy::mesh::VertexAttributeValues;

    use super::super::constants::{MOUNTED_HEIGHT, MOUNTED_WIDTH, PLAYER_HEIGHT};
    use super::super::interpolate::WALK_PHASE_PERIOD_BLOCKS;
    use super::super::{ANY_HAIR, piece_mesh};
    use super::*;
    use crate::net::HairModel;

    /// How many phases a cycle is sampled at where a bound has to hold everywhere.
    const STEPS: usize = 96;

    /// Both gaits, named, for every bound that has to hold whatever the horse is doing.
    const GAITS: [(&str, &Gait); 2] = [("walk", &WALK), ("canter", &CANTER)];

    /// The top of the neck, standing.
    const POLL: Vec3 = Vec3::new(0.0, NECK_POLL.y, (NECK_POLL.z.0 + NECK_POLL.z.1) / 2.0);

    /// The centre of a sole, in the lower segment's frame.
    const SOLE_CENTRE: Vec3 = Vec3::new(0.0, HOOF_SOLE.y, (HOOF_SOLE.z.0 + HOOF_SOLE.z.1) / 2.0);

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

    /// The distance phase after `blocks` of travel, as the interpolator counts it.
    fn travelled(blocks: f32) -> WalkPose {
        walking(TAU * blocks / WALK_STRIDE_BLOCKS)
    }

    /// `fraction` of the way through one cycle of `gait`.
    fn through(gait: &Gait, fraction: f32) -> WalkPose {
        travelled(gait.stride * fraction)
    }

    /// Standing, then eight phases through each gait.
    fn poses() -> Vec<(&'static Gait, WalkPose)> {
        std::iter::once((&WALK, WalkPose::default()))
            .chain(GAITS.into_iter().flat_map(|(_, gait)| {
                (0..8).map(move |eighth| (gait, through(gait, eighth as f32 / 8.0)))
            }))
            .collect()
    }

    /// Both segments of one leg in horse space: the lower's transform composes with
    /// the upper's, exactly as the hierarchy composes them.
    fn leg_transforms(leg: Leg, gait: &Gait, pose: WalkPose) -> (Transform, Transform) {
        let upper = leg_transform(leg, Segment::Upper, gait, pose);
        let lower = upper * leg_transform(leg, Segment::Lower, gait, pose);
        (upper, lower)
    }

    fn leg_meshes(leg: Leg, gait: &Gait, pose: WalkPose) -> [Mesh; 2] {
        let (upper, lower) = leg_transforms(leg, gait, pose);
        [
            horse_upper_leg_mesh().transformed_by(upper),
            horse_lower_leg_mesh().transformed_by(lower),
        ]
    }

    /// The centre of one sole, in horse space.
    fn sole(leg: Leg, gait: &Gait, pose: WalkPose) -> Vec3 {
        let (_, lower) = leg_transforms(leg, gait, pose);
        lower.transform_point(SOLE_CENTRE)
    }

    /// The upper segment's angle, positive forward, and the lower's, negative folded back.
    fn angles(leg: Leg, gait: &Gait, pose: WalkPose) -> (f32, f32) {
        let pitch = |segment| {
            leg_transform(leg, segment, gait, pose)
                .rotation
                .to_euler(EulerRot::XYZ)
                .0
        };
        (pitch(Segment::Upper), pitch(Segment::Lower))
    }

    fn same_pose(a: Transform, b: Transform) -> bool {
        a.translation.abs_diff_eq(b.translation, 1e-4)
            && a.rotation.abs_diff_eq(b.rotation, 1e-4)
            && a.scale.abs_diff_eq(b.scale, 1e-4)
    }

    fn all_joints() -> Vec<HorseJoint> {
        Leg::ALL
            .into_iter()
            .flat_map(|leg| {
                [Segment::Upper, Segment::Lower].map(|segment| HorseJoint::Leg(leg, segment))
            })
            .chain([
                HorseJoint::Hair(HorseHair::Mane),
                HorseJoint::Hair(HorseHair::Tail),
                HorseJoint::Barrel,
                HorseJoint::Neck,
            ])
            .chain(Side::BOTH.map(HorseJoint::Rein))
            .collect()
    }

    /// The neck's pitch at `pose`: forward of rest for negative.
    fn neck_pitch(gait: &Gait, pose: WalkPose) -> f32 {
        neck_transform(gait, pose)
            .rotation
            .to_euler(EulerRot::XYZ)
            .0
    }

    /// The head in horse space at `pose`, under the barrel and the neck.
    fn head_mesh(gait: &Gait, pose: WalkPose) -> Mesh {
        horse_head_mesh().transformed_by(barrel_transform(gait, pose) * neck_transform(gait, pose))
    }

    /// Every part of the horse posed at `pose`, with no rider on it: the hierarchy
    /// composed exactly as the spawn nests it.
    fn horse_meshes(gait: &Gait, pose: WalkPose) -> Vec<Mesh> {
        let barrel = barrel_transform(gait, pose);
        let neck = barrel * neck_transform(gait, pose);
        let mut meshes = vec![
            horse_barrel_mesh().transformed_by(barrel),
            horse_neck_mesh().transformed_by(neck),
            horse_head_mesh().transformed_by(neck),
            horse_eye_mesh().transformed_by(neck),
            horse_mane_mesh().transformed_by(neck * hair_transform(HorseHair::Mane, gait, pose)),
            horse_tail_mesh().transformed_by(barrel * hair_transform(HorseHair::Tail, gait, pose)),
            horse_tack_mesh().transformed_by(barrel),
        ];
        meshes.extend(
            Side::BOTH
                .map(|side| horse_rein_mesh().transformed_by(rein_transform(side, gait, pose))),
        );
        meshes.extend(
            Leg::ALL
                .into_iter()
                .flat_map(|leg| leg_meshes(leg, gait, pose)),
        );
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

        let (_, whole) = extent(&horse_meshes(&WALK, WalkPose::default()));
        assert!(
            whole.y <= HORSE_HEIGHT + 1e-6,
            "something stands above the ears: {}",
            whole.y
        );
    }

    #[test]
    fn the_shared_ear_tapers_to_a_tenth_and_defines_the_horse_height() {
        for side in [-1.0, 1.0] {
            let points = positions(&[horse_ear_mesh(side)]);
            let footprint = |y: f32| {
                let (low, high) = points
                    .iter()
                    .filter(|point| (point.y - y).abs() < 1e-6)
                    .fold((Vec3::MAX, Vec3::MIN), |(low, high), point| {
                        (low.min(*point), high.max(*point))
                    });
                assert!(low.is_finite() && high.is_finite());
                high - low
            };
            let base = footprint(EAR_BASE.y);
            let tip = footprint(EAR_TIP.y);
            assert!(tip.x <= base.x * 0.1 + 1e-6);
            assert!(tip.z <= base.z * 0.1 + 1e-6);
            assert_eq!(EAR_TIP.y, HORSE_HEIGHT);
        }
    }

    #[test]
    fn the_pointed_head_token_fits_its_requested_edge() {
        for edge in [0.12, 0.5, 1.0] {
            let (low, high) = extent(&[horse_head_item_mesh(edge)]);
            assert!(((high - low).max_element() - edge).abs() < 1e-5);
        }
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
        for (gait, pose) in poses() {
            let mut meshes = horse_meshes(gait, pose);
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

    /// Standing, every joint is exactly at rest: identity rotation, rest translation,
    /// and the hooves on the feet plane — whatever the phase and whichever gait.
    #[test]
    fn standing_every_joint_is_exactly_at_rest_and_the_hooves_are_on_the_ground() {
        for (_, gait) in GAITS {
            every_joint_rests(gait);
        }
    }

    fn every_joint_rests(gait: &Gait) {
        for standing in [
            WalkPose::default(),
            WalkPose {
                phase: 2.3,
                moving: false,
            },
        ] {
            for leg in Leg::ALL {
                let upper = leg_transform(leg, Segment::Upper, gait, standing);
                let lower = leg_transform(leg, Segment::Lower, gait, standing);
                assert_eq!(upper, Transform::from_translation(leg.pivot()), "{leg:?}");
                assert_eq!(lower, Transform::from_translation(KNEE), "{leg:?}");
                assert_eq!(upper, rest_transform(HorseJoint::Leg(leg, Segment::Upper)));
                assert_eq!(lower, rest_transform(HorseJoint::Leg(leg, Segment::Lower)));
                let (low, _) = extent(&leg_meshes(leg, gait, standing));
                assert!(low.y.abs() < 1e-3, "{leg:?} stands at y {}", low.y);
            }
            for (hair, rest) in [(HorseHair::Mane, MANE_REST), (HorseHair::Tail, TAIL_REST)] {
                let at_rest = hair_transform(hair, gait, standing);
                assert_eq!(at_rest, rest_transform(HorseJoint::Hair(hair)));
                assert_eq!(at_rest.rotation, Quat::from_rotation_x(rest));
            }
            // The barrel and the neck turn in place, so their rest is the identity —
            // the canter's lean included, which is a pose of moving, not of standing.
            assert_eq!(barrel_transform(gait, standing), Transform::IDENTITY);
            assert_eq!(neck_transform(gait, standing), Transform::IDENTITY);
            // A rein at rest is the bar that used to be merged into the tack.
            for side in Side::BOTH {
                let rein = rein_transform(side, gait, standing);
                assert_eq!(
                    rein,
                    bar_between(REIN_BIT * side.mirror(), REIN_HAND * side.mirror())
                );
                assert_eq!(rein, rest_transform(HorseJoint::Rein(side)));
            }
        }
    }

    #[test]
    fn legs_pivot_at_the_shoulder_and_the_hip_and_fold_at_one_knee_height() {
        let (belly, withers) = extent(&[horse_barrel_mesh()]);
        for leg in Leg::ALL {
            let pivot = leg.pivot();
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

            // The knee is where the lower segment's frame sits: straight under the pivot,
            // at the cut, and the upper segment reaches exactly down to it.
            let (_, lower) = leg_transforms(leg, &WALK, WalkPose::default());
            let knee = lower.translation;
            assert!(
                (knee - Vec3::new(pivot.x, KNEE_Y, pivot.z)).length() < 1e-6,
                "{leg:?} bends at {knee}"
            );
            let (upper_low, _) = extent(&[horse_upper_leg_mesh().transformed_by(leg_transform(
                leg,
                Segment::Upper,
                &WALK,
                WalkPose::default(),
            ))]);
            assert!((upper_low.y - KNEE_Y).abs() < 1e-6);

            // Between its footfall and its lift the sole sweeps most of a block along the
            // ground at a walk — a real stride's worth for a 1.7-block cycle, and more than
            // twice what one stiff segment used to be allowed — and about a block at a
            // canter.
            for ((name, gait), band) in GAITS.into_iter().zip([0.6..=0.8, 0.9..=1.1]) {
                let beat = gait.beats[leg.index()] / TAU;
                let footfall = sole(leg, gait, through(gait, beat));
                let lift = sole(leg, gait, through(gait, beat + 0.5));
                let sweep = lift.z - footfall.z;
                assert!(
                    band.contains(&sweep),
                    "{leg:?} sweeps {sweep} at a {}",
                    name
                );
            }
        }
    }

    /// The walk is lateral and four-beat: left rear, left front, right rear, right
    /// front, each a quarter of a cycle behind the last. At each quarter exactly one
    /// hoof lands — its upper segment is at full forward swing — the leg that landed
    /// two quarters earlier is lifting, and the two between are vertical, one planted
    /// with its cannon straight and one mid-swing with its cannon folded.
    #[test]
    fn the_walk_is_four_beats_a_quarter_of_a_cycle_apart() {
        use Leg::*;
        let landing = [LeftRear, LeftFront, RightRear, RightFront];
        for quarter in 0..4 {
            let pose = through(&WALK, quarter as f32 / 4.0);
            let lands = landing[quarter];
            let lifts = landing[(quarter + 2) % 4];
            let folds = landing[(quarter + 1) % 4];
            for leg in Leg::ALL {
                let (swing, fold) = angles(leg, &WALK, pose);
                let want = if leg == lands {
                    WALK.swing
                } else if leg == lifts {
                    -WALK.swing
                } else {
                    0.0
                };
                assert!(
                    (swing - want).abs() < 1e-5,
                    "quarter {quarter}: {leg:?} swings {swing}, want {want}"
                );
                let want = if leg == folds { -WALK.fold } else { 0.0 };
                assert!(
                    (fold - want).abs() < 1e-5,
                    "quarter {quarter}: {leg:?} folds {fold}, want {want}"
                );
            }
        }
    }

    /// The cannon is straight for the whole of the stance and folds only through the
    /// swing, backward, so the hoof trails the knee — and it never leaves the ground
    /// while it is meant to be on it. At either gait.
    #[test]
    fn the_cannon_folds_through_the_swing_and_is_straight_through_the_stance() {
        for (name, gait) in GAITS {
            cannon_folds_through_the_swing(name, gait);
        }
    }

    fn cannon_folds_through_the_swing(name: &str, gait: &Gait) {
        for leg in Leg::ALL {
            let beat = gait.beats[leg.index()];
            for step in 1..STEPS {
                let own = TAU * step as f32 / STEPS as f32;
                let pose = through(gait, (beat + own) / TAU);
                let (swing, fold) = angles(leg, gait, pose);
                if own < PI {
                    assert!(
                        fold.abs() < 1e-6,
                        "{leg:?} folds {fold} at {own} into its stance at a {name}"
                    );
                } else if own > PI {
                    assert!(
                        fold < 0.0,
                        "{leg:?} is straight at {own} into its swing at a {name}"
                    );
                    // Folded back: in the upper segment's frame the sole sits behind
                    // where a straight cannon would put it.
                    let trailing =
                        leg_transform(leg, Segment::Lower, gait, pose).transform_point(SOLE_CENTRE);
                    let straight = Transform::from_translation(KNEE).transform_point(SOLE_CENTRE);
                    assert!(
                        trailing.z > straight.z,
                        "{leg:?} folds forward at {own}: sole at {trailing}"
                    );
                }
                // Halfway through each half the upper is vertical.
                if (own - FRAC_PI_2).abs() < 1e-4 || (own - 3.0 * FRAC_PI_2).abs() < 1e-4 {
                    assert!(swing.abs() < 1e-5, "{leg:?} leans {swing} at {own}");
                }
            }
            let planted = sole(leg, gait, through(gait, (beat + FRAC_PI_2) / TAU));
            assert!(planted.y.abs() < 1e-4, "{leg:?} planted at y {}", planted.y);
            let (_, fold) = angles(leg, gait, through(gait, (beat + 3.0 * FRAC_PI_2) / TAU));
            assert!(
                (fold + gait.fold).abs() < 1e-5,
                "{leg:?} folds {fold} mid-swing at a {name}"
            );
        }
    }

    /// The sole is flat and the stance leg is stiff, so as it leans the sole's leading
    /// or trailing edge digs in by `0.10 · sin θ − 1.12 · (1 − cos θ)`, which peaks at
    /// four and a half millimetres near five degrees of lean. That is the floor of this
    /// bound: a hoof through the ground would be centimetres.
    #[test]
    fn no_hoof_rises_above_a_third_of_a_block_or_sinks_under_the_ground() {
        for (name, gait) in GAITS {
            hooves_stay_between_the_ground_and_a_third_of_a_block(name, gait);
        }
    }

    fn hooves_stay_between_the_ground_and_a_third_of_a_block(name: &str, gait: &Gait) {
        for leg in Leg::ALL {
            let mut highest = 0.0_f32;
            for step in 0..STEPS {
                let pose = through(gait, step as f32 / STEPS as f32);
                let (low, _) = extent(&leg_meshes(leg, gait, pose));
                assert!(
                    low.y > -0.005,
                    "{leg:?} sinks to {} at {pose:?} at a {name}",
                    low.y
                );
                let hoof = sole(leg, gait, pose).y;
                assert!(
                    hoof <= 0.35,
                    "{leg:?} rises to {hoof} at {pose:?} at a {name}"
                );
                highest = highest.max(hoof);
            }
            assert!(highest > 0.05, "{leg:?} never lifts at a {name}: {highest}");
        }
    }

    /// A stride is a drawn length: one brings every joint back to where it was, half of
    /// one does not, and the period the distance phase wraps at — a whole number of
    /// either gait's strides — brings it back too, so the wrap moves nothing.
    #[test]
    fn a_cycle_is_one_stride_and_the_phase_wrap_is_a_whole_number_of_them() {
        assert!((CANTER.stride - 2.0 * WALK.stride).abs() < 1e-6);
        for (name, gait) in GAITS {
            one_stride_and_the_wrap_move_nothing(name, gait);
        }
    }

    fn one_stride_and_the_wrap_move_nothing(name: &str, gait: &Gait) {
        let cycles = WALK_PHASE_PERIOD_BLOCKS / gait.stride;
        assert!(
            (cycles - cycles.round()).abs() < 1e-4,
            "{} blocks is {cycles} {name} strides",
            WALK_PHASE_PERIOD_BLOCKS
        );
        for joint in all_joints() {
            let start = joint_transform(joint, gait, travelled(0.0));
            let one_stride = joint_transform(joint, gait, travelled(gait.stride));
            let wrapped = joint_transform(joint, gait, travelled(WALK_PHASE_PERIOD_BLOCKS));
            assert!(
                same_pose(start, one_stride),
                "{joint:?} after one {name} stride"
            );
            assert!(
                same_pose(start, wrapped),
                "{joint:?} at the wrap at a {name}"
            );
        }
        let (swing, _) = angles(Leg::LeftRear, gait, travelled(0.0));
        let (half, _) = angles(Leg::LeftRear, gait, travelled(gait.stride / 2.0));
        assert!((swing + gait.swing).abs() > 1e-3 && (swing - half).abs() > 1e-3);
    }

    /// The canter is three beats and a suspension. Right lead: at the first quarter the
    /// trailing hind lands, at the second the diagonal pair, at the third the leading
    /// fore, and at the fourth nothing lands — every upper segment pinned at each
    /// quarter, in units of the swing, in `Leg::ALL` order: a 1 is a hoof landing.
    #[test]
    fn the_canter_is_three_beats_and_a_suspension() {
        let table: [[f32; 4]; 4] = [
            [0.0, -1.0, 1.0, 0.0],
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 1.0, -1.0, 0.0],
            [-1.0, 0.0, 0.0, -1.0],
        ];
        for (quarter, row) in table.iter().enumerate() {
            let pose = through(&CANTER, quarter as f32 / 4.0);
            for leg in Leg::ALL {
                let (swing, _) = angles(leg, &CANTER, pose);
                let want = row[leg.index()] * CANTER.swing;
                assert!(
                    (swing - want).abs() < 1e-5,
                    "quarter {quarter}: {leg:?} swings {swing}, want {want}"
                );
            }
        }
    }

    /// At a walk the neck nods ±4° and never leans and the girth rises and falls at most
    /// 0.04 with no pitch, both twice a cycle and up together. At a canter the neck holds
    /// forward of rest at every phase, nodding ±3° about that lean, so the poll reaches
    /// ahead and the head is carried lower — the nose itself hangs almost level with the
    /// neck's base, so the lean takes it down rather than out — while the girth stays put
    /// and the barrel rocks ±3° about it, one end up as the other goes down.
    #[test]
    fn the_neck_and_the_barrel_nod_and_bob_at_a_walk_and_stretch_and_rock_at_a_canter() {
        let standing = extent(&[head_mesh(&WALK, WalkPose::default())]).1.y;
        assert!((standing - HORSE_HEIGHT).abs() < 1e-6);
        let girth = |gait: &Gait, pose| {
            barrel_transform(gait, pose).transform_point(BARREL_PIVOT) - BARREL_PIVOT
        };

        let (mut nod, mut rise, mut fall) = (0.0_f32, 0.0_f32, 0.0_f32);
        for step in 0..STEPS {
            let pose = through(&WALK, step as f32 / STEPS as f32);
            let pitch = neck_pitch(&WALK, pose);
            assert!(
                pitch.abs() <= 4.0 * DEGREE + 1e-6,
                "the walk nods {}°",
                pitch / DEGREE
            );
            nod = nod.max(pitch.abs());
            let barrel = barrel_transform(&WALK, pose);
            assert!(
                barrel.rotation.abs_diff_eq(Quat::IDENTITY, 1e-6),
                "the walk pitches"
            );
            let moved = girth(&WALK, pose);
            assert!(
                moved.x.abs() < 1e-6 && moved.z.abs() < 1e-6 && moved.y.abs() <= 0.04 + 1e-6,
                "the girth moved {moved}"
            );
            rise = rise.max(moved.y);
            fall = fall.min(moved.y);
        }
        assert!(
            (nod - 4.0 * DEGREE).abs() < 1e-3,
            "the walk nods only {}°",
            nod / DEGREE
        );
        assert!(
            (rise - WALK.bob).abs() < 1e-3 && (fall + WALK.bob).abs() < 1e-3,
            "the walk bobs {fall}..{rise}"
        );
        assert!(
            neck_pitch(&WALK, through(&WALK, 0.0)).abs() < 1e-6,
            "the walk leans"
        );
        // Twice a cycle and together: up at the first eighth, down at the third.
        for (eighth, sign) in [(1, 1.0), (3, -1.0)] {
            let pose = through(&WALK, eighth as f32 / 8.0);
            let pitch = neck_pitch(&WALK, pose);
            assert!(
                (pitch - sign * 4.0 * DEGREE).abs() < 1e-4,
                "eighth {eighth}: {pitch}"
            );
            let moved = girth(&WALK, pose).y;
            assert!(
                (moved - sign * WALK.bob).abs() < 1e-4,
                "eighth {eighth}: {moved}"
            );
        }
        let (_, nodded_up) = extent(&[head_mesh(&WALK, through(&WALK, 1.0 / 8.0))]);
        assert!(
            nodded_up.y > standing + 0.02,
            "a nod up left the ears at {}",
            nodded_up.y
        );

        // The lean outreaches the nod, so the neck is forward of rest at every phase.
        let held = (CANTER.lean - CANTER.nod - 1e-6)..=(CANTER.lean + CANTER.nod + 1e-6);
        assert!(*held.end() < 0.0);
        let mut rock = 0.0_f32;
        for step in 0..STEPS {
            let pose = through(&CANTER, step as f32 / STEPS as f32);
            let pitch = neck_pitch(&CANTER, pose);
            assert!(
                held.contains(&pitch),
                "the canter's neck is at {}°",
                pitch / DEGREE
            );
            let (_, ears) = extent(&[head_mesh(&CANTER, pose)]);
            assert!(ears.y < standing, "the ears came back up at {pose:?}");
            // The stretch is the neck's own, measured in the barrel's frame: the
            // barrel's nose-up rock lifts the whole front at the phase the neck nods up.
            let poll = neck_transform(&CANTER, pose).transform_point(POLL);
            assert!(
                poll.z < POLL.z - 0.04 && poll.y < POLL.y - 0.04,
                "the poll did not stretch at {pose:?}: {poll}"
            );
            assert!(
                girth(&CANTER, pose).length() < 1e-5,
                "the girth moved at a canter"
            );
            let tilt = barrel_transform(&CANTER, pose)
                .rotation
                .to_euler(EulerRot::XYZ)
                .0;
            assert!(
                tilt.abs() <= 3.0 * DEGREE + 1e-6,
                "the canter rocks {}°",
                tilt / DEGREE
            );
            rock = rock.max(tilt.abs());
        }
        assert!(
            (rock - 3.0 * DEGREE).abs() < 1e-3,
            "the canter rocks only {}°",
            rock / DEGREE
        );
        // A rock about the girth lifts one end of the barrel as it lowers the other.
        let quarter = barrel_transform(&CANTER, through(&CANTER, 0.25));
        let end = |z| quarter.transform_point(Vec3::new(0.0, BARREL_PIVOT.y, z)).y - BARREL_PIVOT.y;
        assert!(end(BREAST.z) * end(RUMP.z) < 0.0 && end(RUMP.z).abs() > 0.03);
    }

    /// A rein runs from the bit to the fist at every pose of either gait: the bit is
    /// wherever the barrel and the neck have carried it, the fist rides on the body.
    #[test]
    fn the_reins_follow_the_bit_and_stay_in_the_fists() {
        for (gait, pose) in poses() {
            let mouth = barrel_transform(gait, pose) * neck_transform(gait, pose);
            for side in Side::BOTH {
                let rein = rein_transform(side, gait, pose);
                let bit_end = rein.transform_point(Vec3::Z * -0.5);
                let hand_end = rein.transform_point(Vec3::Z * 0.5);
                let bit = mouth.transform_point(REIN_BIT * side.mirror());
                assert!(
                    (bit_end - bit).length() < 1e-4,
                    "{side:?} rein starts at {bit_end}, bit at {bit}"
                );
                assert!(
                    (hand_end - REIN_HAND * side.mirror()).length() < 1e-4,
                    "{side:?} rein ends at {hand_end}"
                );
            }
        }
    }

    /// One horse's joints after `animate_gait` has run once over what `spawn` put in
    /// the world.
    fn posed_by_the_system(
        spawn: impl FnOnce(&mut World, &HorseVisuals),
    ) -> Vec<(HorseJoint, Transform)> {
        use bevy::ecs::system::RunSystemOnce;
        let mut app = visual_app();
        let world = app.world_mut();
        world.resource_scope(|world, visuals: Mut<HorseVisuals>| spawn(world, &visuals));
        world.flush();
        world
            .run_system_once(animate_gait)
            .expect("animate_gait runs");
        let mut joints = world.query::<(&HorseJoint, &Transform)>();
        joints
            .iter(world)
            .map(|(joint, transform)| (*joint, *transform))
            .collect()
    }

    /// The same phase on the two routes is two gaits: the horse under a rider canters,
    /// the paddock horse walks, and no speed was consulted — the system reads only which
    /// query the root came from.
    #[test]
    fn a_ridden_horse_canters_and_a_paddock_horse_walks_from_the_same_phase() {
        let pose = travelled(0.6);
        let kind = MountKind::BlackHorse;
        let ridden = posed_by_the_system(|world, visuals| {
            let rider = world
                .spawn((Body(1), pose, Transform::default(), Visibility::default()))
                .id();
            let mut commands = world.commands();
            spawn_horse(&mut commands, visuals, rider, kind);
        });
        let paddock = posed_by_the_system(|world, visuals| {
            let material = visuals.material(kind);
            world
                .commands()
                .spawn((
                    Horse { kind },
                    PaddockHorse(0),
                    pose,
                    Transform::default(),
                    Visibility::default(),
                ))
                .with_children(|horse| spawn_horse_parts(horse, visuals, &material));
        });
        assert_eq!(
            (ridden.len(), paddock.len()),
            (all_joints().len(), all_joints().len())
        );
        for (joint, transform) in &ridden {
            assert_eq!(
                *transform,
                joint_transform(*joint, &CANTER, pose),
                "{joint:?} under a rider"
            );
        }
        for (joint, transform) in &paddock {
            assert_eq!(
                *transform,
                joint_transform(*joint, &WALK, pose),
                "{joint:?} in the paddock"
            );
        }
        let lead = HorseJoint::Leg(Leg::LeftRear, Segment::Upper);
        assert_ne!(
            joint_transform(lead, &CANTER, pose),
            joint_transform(lead, &WALK, pose)
        );
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

    #[test]
    fn mane_and_tail_read_the_gait_cycle_without_changing_it() {
        let quarter = through(&WALK, 0.25);
        for (hair, rest, want) in [
            (HorseHair::Mane, MANE_REST, MANE_SWING),
            (HorseHair::Tail, TAIL_REST, TAIL_SWING),
        ] {
            let pitch = hair_transform(hair, &WALK, quarter)
                .rotation
                .to_euler(EulerRot::XYZ)
                .0;
            assert!((pitch - rest - want).abs() < 1e-5);
        }
        let (swing, _) = angles(Leg::LeftFront, &WALK, quarter);
        assert!((swing - WALK.swing).abs() < 1e-5);
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
            [
                coat_colour(MountKind::BlackHorse),
                coat_colour(MountKind::BrownHorse),
                coat_colour(MountKind::GreyHorse),
            ]
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
        assert_eq!(horse_tack_mesh().count_vertices(), cuboid * 3);
        assert_eq!(horse_rein_mesh().count_vertices(), cuboid);
        assert_ne!(HAIR_COLOUR, LEATHER_COLOUR);
        for mesh in [
            horse_barrel_mesh(),
            horse_neck_mesh(),
            horse_head_mesh(),
            horse_upper_leg_mesh(),
            horse_lower_leg_mesh(),
        ] {
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
