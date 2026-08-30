//! Birds in the air: a colour that moves, and nothing else.
//!
//! ## Why a gameplay rule is not being smuggled in here
//!
//! The rule the whole project is built on (`AGENTS.md`, and the head of `client/AGENTS.md`)
//! is that a *gameplay* rule may not live only on the client. A bird that cannot be hit,
//! targeted, eaten, counted or seen by the server is not a gameplay rule; it is a colour
//! that moves. Nothing about a bird is sent, nothing reads a rule back out of one, and the
//! species is chosen from [`Ambience`] alone — never from the snapshot's weather, never from
//! anything the server said. The day somebody wants to shoot a vulture, the bird becomes a
//! `MobKind` on the server and this module's row for it is deleted, in that order.
//!
//! **This is the first half of #549, and the seam is which modules a bird touches.** Every
//! line below reads `player/ambience.rs` and `player/camera.rs` and nothing else: a bird on
//! its own. The second half is a bird *beside* the rest of the client — the roost on
//! `player/sky.rs`'s night, the hide under its water, the pins that assert a bird carries no
//! component from `mobs.rs`, `hands.rs`, `drops.rs` or `structures.rs`, and the one that
//! aims a mining intent straight along a bird and compares the bytes with an empty sky. It
//! is additive to every line below and it changes none of them.
//!
//! ## Two ideas already in this crate, with a species table in front
//!
//! `player/sky.rs` draws hand-built quads that follow the eye and are hash-seeded from a
//! constant; `player/precipitation.rs` keeps one client-only volume around the camera whose
//! contents are a pure function of a seed and the elapsed time. A bird is those two with
//! [`Ambience`] choosing which row of [`BIRDS`] flies.
//!
//! Three entities and no asset: a body quad with two wing quads as children, and the flap is
//! the children's own rotation about their hinge. `player/precipitation.rs` rewrites six
//! hundred quads because they are *one* draw; six birds are six draws either way, so
//! rotating a child is cheaper to write and to read than recomputing vertices.

use std::f32::consts::{PI, TAU};
use std::ops::RangeInclusive;

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use super::ambience::{Ambience, GroundLook};
use super::camera::WorldCamera;

/// How coarsely the eye is quantised before it anchors a flock, in blocks.
///
/// The anchor is what makes a bird's path a pure function of time: it holds still while the
/// player walks a cell's width, so nothing has to remember where a bird was.
pub(super) const BIRD_ANCHOR_CELL: f32 = 32.0;

/// How far from its anchor a bird may be, in blocks, on every axis.
///
/// Every altitude band and every pattern radius is chosen to stay inside this while the
/// anchor holds still — `a_bird_never_leaves_its_box` pins it — so the only thing that ever
/// puts a bird outside it is the anchor moving.
pub(super) const BIRD_RANGE: f32 = 64.0;

/// The most birds that may exist at once, over the whole sky rather than per flock.
pub(super) const BIRD_COUNT_MAX: usize = 6;

/// The one constant every bird seed is mixed from.
///
/// **Never `world_seed`, and never an entity id.** Two players in the same desert see
/// different vultures on purpose: a bird nobody shares is a bird nobody can arrange to meet,
/// which is the cheapest available proof that none of this is state.
const BIRD_SEED: u64 = 0xB1BD_5EED_A17E_0F73;

/// How far a wing swings either side of level, in radians.
const FLAP_AMPLITUDE_RADIANS: f32 = 0.55;

/// How far a bird's home sits from the anchor, in blocks, on the horizontal axes.
///
/// Plus the widest pattern radius (an eagle's 40) this is 60, inside [`BIRD_RANGE`].
const HOME_SPREAD: f32 = 20.0;

/// The corner of a dart leg's waypoint box, in blocks, and how long one leg lasts.
///
/// The longest leg is `2 * |DART_SPREAD|` over the shortest time, which is what `Parrot`'s
/// `max_speed` is.
const DART_SPREAD: Vec3 = Vec3::new(5.0, 1.5, 5.0);
const DART_LEG_SECONDS: RangeInclusive<f32> = 2.0..=4.0;

/// How far a circling bird rises and falls, in blocks, and over how many turns.
const SPIRAL_RISE: f32 = 3.0;
const SPIRAL_RISE_TURNS: f32 = 3.0;

/// How far a circling bird's centre wanders, in blocks, and how long one wander takes.
///
/// The "drifting" half of a vulture's spiral: without it a vulture turns about a point in
/// the air forever, which reads as a carousel rather than as a bird.
const CIRCLE_DRIFT: f32 = 6.0;
const CIRCLE_DRIFT_SECONDS: f32 = 40.0;

/// How far an arcing bird rises and falls across one sweep, in blocks.
const ARC_RISE: f32 = 3.0;

/// How far ahead a bird is sampled to find which way it is facing, in seconds.
const HEADING_STEP: f32 = 0.05;

/// How a bird moves through the air.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Flight {
    /// Short straight legs between waypoints re-chosen every [`DART_LEG_SECONDS`].
    Dart,
    /// A slow drifting spiral.
    Circle,
    /// Long slow sweeps: a figure whose two lobes are one continuous pass.
    Arc,
}

/// One row of [`BIRDS`]: everything about a kind of bird there is.
#[derive(Debug)]
pub(super) struct BirdSpecies {
    /// The [`GroundLook`] this row flies over. One row per look, and no row flies over
    /// [`GroundLook::Unknown`].
    pub(super) ground: GroundLook,
    /// Whether the look also has to be wooded. Parrots need trees, so an open plain has no
    /// parrots and a wood in the plains does.
    pub(super) requires_wooded: bool,
    /// How many of them fly together.
    pub(super) flock: RangeInclusive<u8>,
    /// How far above the anchor they fly, in blocks.
    pub(super) altitude: RangeInclusive<f32>,
    /// The wingspan, in blocks. The whole bird is drawn at this scale.
    pub(super) size: f32,
    /// How often a wing completes one beat, in hertz. Under one is a glide.
    pub(super) flap_hz: f32,
    /// The row's own body and wing colours, and the first pair
    /// [`BirdSpecies::colours`] chooses from.
    pub(super) body: Color,
    pub(super) wing: Color,
    /// The *other* pairs a bird of this row may wear, chosen by its own seed. Empty for a
    /// species with one plumage; a parrot has three, so this holds the two that are not
    /// already `body`/`wing` and no pair is written twice.
    pub(super) plumage: &'static [(Color, Color)],
    /// How it flies.
    pub(super) pattern: Flight,
    /// The fastest this row's pattern can move it, in blocks per second.
    ///
    /// A bound rather than a speed, and **test-only** for exactly that reason: nothing reads
    /// it to move a bird — [`place`] is the whole of where a bird is — and
    /// `a_bird_moves_no_faster_than_its_row_allows` is its only consumer. It is here so the
    /// continuity of [`place`] is a number a test can fail on rather than a claim in a
    /// comment; `combat.rs`'s `BLADE_SHAPES` is the same shape one module over.
    #[cfg(test)]
    pub(super) max_speed: f32,
}

impl BirdSpecies {
    /// The colours one bird of this row wears: index zero is the row's own pair and the
    /// rest come from [`BirdSpecies::plumage`], so a row with no variants has one answer.
    pub(super) fn colours(&self, seed: u64) -> (Color, Color) {
        let choice = mix(seed, SALT_PLUMAGE) as usize % (self.plumage.len() + 1);
        match choice.checked_sub(1) {
            None => (self.body, self.wing),
            Some(variant) => self.plumage[variant],
        }
    }

    /// Whether this row is the one [`Ambience`] is asking for.
    fn matches(&self, ambience: &Ambience) -> bool {
        self.ground == ambience.ground && (!self.requires_wooded || ambience.wooded)
    }
}

/// Every kind of bird there is. One row per [`GroundLook`] that has one.
///
/// Appended to, never reordered: [`Bird::species`] is an index into this table and a bird
/// alive across a reorder would change species in the air.
pub(super) const BIRDS: [BirdSpecies; 3] = [
    // The parrot: small, bright, fast and low, and the only row that needs trees.
    BirdSpecies {
        ground: GroundLook::Grass,
        requires_wooded: true,
        flock: 3..=5,
        altitude: 4.0..=12.0,
        size: 0.35,
        flap_hz: 5.0,
        body: Color::srgb(0.85, 0.16, 0.14),
        wing: Color::srgb(0.16, 0.66, 0.24),
        plumage: &[
            (Color::srgb(0.14, 0.32, 0.86), Color::srgb(0.94, 0.82, 0.16)),
            (Color::srgb(0.16, 0.66, 0.24), Color::srgb(0.20, 0.56, 0.86)),
        ],
        pattern: Flight::Dart,
        // The longest leg is |2 * DART_SPREAD| = 14.5 blocks over the shortest leg time.
        #[cfg(test)]
        max_speed: 7.5,
    },
    // The vulture: high over the sand, turning, and barely beating a wing.
    BirdSpecies {
        ground: GroundLook::Sand,
        requires_wooded: false,
        flock: 2..=4,
        altitude: 25.0..=45.0,
        size: 0.9,
        flap_hz: 0.6,
        body: Color::srgb(0.24, 0.17, 0.12),
        wing: Color::srgb(0.13, 0.09, 0.07),
        plumage: &[],
        pattern: Flight::Circle,
        // The tightest turn at the widest radius, plus the drift and the rise.
        #[cfg(test)]
        max_speed: 7.5,
    },
    // The eagle: higher still, alone or in a pair, and never in a hurry.
    BirdSpecies {
        ground: GroundLook::Snow,
        requires_wooded: false,
        flock: 1..=2,
        altitude: 35.0..=60.0,
        size: 1.0,
        flap_hz: 0.6,
        body: Color::srgb(0.31, 0.21, 0.13),
        wing: Color::srgb(0.72, 0.68, 0.60),
        plumage: &[],
        pattern: Flight::Arc,
        // Both lobes of the sweep reach their fastest together at the crossing.
        #[cfg(test)]
        max_speed: 10.0,
    },
];

/// Which row [`Ambience`] is asking for, if any.
///
/// [`GroundLook::Unknown`] and grass without trees both answer `None`: "not enough loaded
/// evidence" and "an open plain" come out as an empty sky rather than as a default bird.
pub(super) fn species_for(ambience: &Ambience) -> Option<usize> {
    BIRDS.iter().position(|row| row.matches(ambience))
}

// ---------------------------------------------------------------------------
// Where a bird is
// ---------------------------------------------------------------------------

/// The anchor cell the eye is in.
fn cell_of(eye: Vec3) -> IVec3 {
    (eye / BIRD_ANCHOR_CELL).floor().as_ivec3()
}

/// The point a cell's flock is anchored to: its centre.
fn anchor_of(cell: IVec3) -> Vec3 {
    (cell.as_vec3() + Vec3::splat(0.5)) * BIRD_ANCHOR_CELL
}

/// The point a bird's pattern is drawn around, and the whole of its altitude band.
fn home_of(species: &BirdSpecies, seed: u64, anchor: Vec3) -> Vec3 {
    anchor
        + Vec3::new(
            centred(seed, SALT_HOME_X) * HOME_SPREAD,
            lerp(
                *species.altitude.start(),
                *species.altitude.end(),
                unit(seed, SALT_ALTITUDE),
            ),
            centred(seed, SALT_HOME_Z) * HOME_SPREAD,
        )
}

/// Where one bird is, `elapsed` seconds into the session.
///
/// Pure: the same four arguments give the same point forever, so there is no per-bird state
/// to advance, nothing to keep in step between frames, and the whole of the flight is
/// testable without a window. Continuity is the property that matters and it is pinned —
/// `a_bird_moves_no_faster_than_its_row_allows` walks 120 seconds at 60 samples a second and
/// fails on a step longer than `max_speed * dt`.
pub(super) fn place(species: &BirdSpecies, seed: u64, elapsed: f32, anchor: Vec3) -> Vec3 {
    home_of(species, seed, anchor) + offset(species, seed, elapsed)
}

/// How far one bird is from its home, `elapsed` seconds in.
fn offset(species: &BirdSpecies, seed: u64, elapsed: f32) -> Vec3 {
    match species.pattern {
        Flight::Dart => {
            let leg = lerp(
                *DART_LEG_SECONDS.start(),
                *DART_LEG_SECONDS.end(),
                unit(seed, SALT_PERIOD),
            );
            let progress = elapsed / leg;
            let index = progress.floor();
            // The waypoint index is the leg number, so consecutive legs share an end point
            // and the path is continuous across every boundary. `as i64` saturates rather
            // than wrapping to nonsense on a clock nobody will run that long anyway.
            let from = waypoint(seed, index as i64);
            let to = waypoint(seed, index as i64 + 1);
            from.lerp(to, progress - index)
        }
        Flight::Circle => {
            let radius = lerp(10.0, 18.0, unit(seed, SALT_RADIUS));
            let period = lerp(20.0, 30.0, unit(seed, SALT_PERIOD));
            let angle = TAU * (elapsed / period + unit(seed, SALT_PHASE));
            let drift = TAU * (elapsed / CIRCLE_DRIFT_SECONDS + unit(seed, SALT_DRIFT));
            Vec3::new(
                radius * angle.cos() + CIRCLE_DRIFT * drift.sin(),
                SPIRAL_RISE * (angle / SPIRAL_RISE_TURNS).sin(),
                radius * angle.sin() + CIRCLE_DRIFT * drift.cos(),
            )
        }
        Flight::Arc => {
            let radius = lerp(30.0, 40.0, unit(seed, SALT_RADIUS));
            let period = lerp(40.0, 60.0, unit(seed, SALT_PERIOD));
            let angle = TAU * (elapsed / period + unit(seed, SALT_PHASE));
            // A lemniscate rather than a circle: one revolution is two long sweeps that
            // cross, which is what an eagle riding a ridge looks like from underneath.
            Vec3::new(
                radius * angle.sin(),
                ARC_RISE * (2.0 * angle).sin(),
                radius * angle.sin() * angle.cos(),
            )
        }
    }
}

/// The `index`th waypoint of a darting bird, relative to its home.
fn waypoint(seed: u64, index: i64) -> Vec3 {
    let leg = mix(seed, index as u64 ^ SALT_WAYPOINT);
    Vec3::new(
        centred(leg, 0) * DART_SPREAD.x,
        centred(leg, 1) * DART_SPREAD.y,
        centred(leg, 2) * DART_SPREAD.z,
    )
}

// ---------------------------------------------------------------------------
// Seeds
// ---------------------------------------------------------------------------

const SALT_HOME_X: u64 = 1;
const SALT_ALTITUDE: u64 = 2;
const SALT_HOME_Z: u64 = 3;
const SALT_RADIUS: u64 = 4;
const SALT_PERIOD: u64 = 5;
const SALT_PHASE: u64 = 6;
const SALT_DRIFT: u64 = 7;
const SALT_PLUMAGE: u64 = 8;
const SALT_FLOCK: u64 = 9;
const SALT_WAYPOINT: u64 = 0x9E37_79B9_7F4A_7C15;

/// SplitMix64's finalizer: an avalanche, not a generator.
///
/// The same reasoning `player/precipitation.rs` gives for `lowbias32`, one word wider. There
/// is no state to carry and no stream to keep in step, so a bird asked where it lives a
/// thousand frames apart is told the same thing both times.
fn splitmix(mut hash: u64) -> u64 {
    hash = hash.wrapping_add(0x9E37_79B9_7F4A_7C15);
    hash = (hash ^ (hash >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    hash = (hash ^ (hash >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    hash ^ (hash >> 31)
}

fn mix(seed: u64, salt: u64) -> u64 {
    splitmix(seed ^ splitmix(salt))
}

/// One deterministic value in `[0, 1)` per `(seed, salt)`.
///
/// Twenty-four bits over `2^24`, so the result is half-open and an f32 holds it exactly.
fn unit(seed: u64, salt: u64) -> f32 {
    (mix(seed, salt) >> 40) as f32 / 16_777_216.0
}

/// One deterministic value in `[-1, 1)` per `(seed, salt)`.
fn centred(seed: u64, salt: u64) -> f32 {
    unit(seed, salt).mul_add(2.0, -1.0)
}

fn lerp(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

/// The seed of the flock in `cell`. Mixed from [`BIRD_SEED`] and nothing else.
fn cell_seed(cell: IVec3) -> u64 {
    let packed = mix(
        i64::from(cell.x) as u64,
        mix(i64::from(cell.y) as u64, i64::from(cell.z) as u64),
    );
    mix(BIRD_SEED, packed)
}

/// The seed of one bird: its flock's, and its slot in that flock.
fn bird_seed(flock: u64, index: usize) -> u64 {
    mix(flock, index as u64)
}

/// How many birds of this row fly over `cell`.
fn flock_size(species: &BirdSpecies, flock: u64) -> usize {
    let low = usize::from(*species.flock.start());
    let span = usize::from(*species.flock.end()) - low + 1;
    (low + mix(flock, SALT_FLOCK) as usize % span).min(BIRD_COUNT_MAX)
}

// ---------------------------------------------------------------------------
// The entities
// ---------------------------------------------------------------------------

/// The two meshes every bird in the session is drawn from.
#[derive(Resource, Debug)]
pub(super) struct BirdVisuals {
    body: Handle<Mesh>,
    wing: Handle<Mesh>,
}

/// One bird. The root, and the only thing anything outside this module may see.
///
/// It deliberately carries **no** `MobVisuals`, no name plate, no collider, no health and
/// nothing the target raycast or any other system reads.
#[derive(Component, Debug)]
pub(super) struct Bird {
    /// The row of [`BIRDS`] this bird is, as an index. Never re-read from [`Ambience`]: a
    /// bird whose species changed is a bird that should have been replaced.
    pub(super) species: usize,
    seed: u64,
    /// Which slot of its flock this bird holds, so a replacement takes the empty one.
    index: usize,
    /// The point [`place`] draws its path around, fixed for this bird's whole life.
    pub(super) anchor: Vec3,
}

/// One wing, as a child of the bird it belongs to.
#[derive(Component, Debug)]
pub(super) struct BirdWing {
    /// Whether this is the mirrored wing.
    left: bool,
    /// Its own copy of the row's beat, so the flap needs nothing from its parent and the
    /// two queries can be taken in one system without aliasing a `Transform`.
    flap_hz: f32,
}

/// Builds the two meshes. No material here: alpha is per bird, so materials are too.
pub(super) fn create_visuals(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>) {
    commands.insert_resource(BirdVisuals {
        body: meshes.add(quad(Vec2::new(-0.15, -0.5), Vec2::new(0.15, 0.5))),
        // Authored from the hinge outwards, so rotating the child about its own origin is
        // the flap and nothing has to offset it.
        wing: meshes.add(quad(Vec2::new(0.0, -0.25), Vec2::new(0.5, 0.25))),
    });
}

/// A flat quad in the XZ plane, facing up.
///
/// `cull_mode: None` on the material is what makes one quad enough for a bird seen from
/// below as well as from above.
fn quad(low: Vec2, high: Vec2) -> Mesh {
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(
        Mesh::ATTRIBUTE_POSITION,
        vec![
            [low.x, 0.0, low.y],
            [high.x, 0.0, low.y],
            [high.x, 0.0, high.y],
            [low.x, 0.0, high.y],
        ],
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; 4])
    .with_inserted_attribute(
        Mesh::ATTRIBUTE_UV_0,
        vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
    )
    .with_inserted_indices(Indices::U32(vec![0, 2, 1, 0, 3, 2]))
}

/// Lit, and drawn from both faces.
///
/// **Lit** is the choice worth naming: `player/sky.rs`'s bodies are unlit because they are
/// the light source, and a bird is not — it is an object in the world, so night darkens it
/// and the fog takes it at distance exactly as they take a mob.
///
/// **`cull_mode: None`** because a flat quad is seen from below as often as from above, the
/// same reason `mobs.rs` gives for the aggro marker. One material per bird rather than one
/// per row, because a parrot's plumage is its own.
fn plumage_material(colour: Color) -> StandardMaterial {
    StandardMaterial {
        base_color: colour,
        cull_mode: None,
        ..default()
    }
}

/// Decides which birds should exist, and stands the missing ones up.
///
/// Runs after `camera::AimCamera` and after `ambience::sample_the_ground`, so the anchor is
/// this frame's eye and the look is this frame's answer. It writes nothing outside its own
/// entities.
pub(super) fn keep_the_flock(
    ambience: Res<Ambience>,
    time: Res<Time>,
    visuals: Option<Res<BirdVisuals>>,
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    eyes: Query<&Transform, With<WorldCamera>>,
    flock: Query<(Entity, &Bird)>,
) {
    let (Some(visuals), Some(eye)) = (visuals, eyes.iter().next()) else {
        return;
    };
    if !eye.translation.is_finite() {
        return;
    }

    let cell = cell_of(eye.translation);
    let anchor = anchor_of(cell);
    let elapsed = time.elapsed_secs();
    let wanted = species_for(&ambience);

    // Retire everything that is the wrong species for this look, or that the anchor has
    // left behind. A bird outside its box is one the eye has walked away from: its path is
    // drawn around an anchor half a world back, so keeping it would be keeping a bird
    // nobody can see.
    let mut taken = [false; BIRD_COUNT_MAX];
    let mut alive = 0usize;
    for (entity, bird) in &flock {
        let position = place(&BIRDS[bird.species], bird.seed, elapsed, bird.anchor);
        let outside = (position - anchor).abs().max_element() > BIRD_RANGE;
        if wanted != Some(bird.species) || outside {
            commands.entity(entity).despawn();
            continue;
        }
        alive += 1;
        if bird.anchor == anchor && bird.index < BIRD_COUNT_MAX {
            taken[bird.index] = true;
        }
    }

    let Some(index) = wanted else {
        return;
    };
    let species = &BIRDS[index];
    let flock_seed = cell_seed(cell);
    let wanted_size = flock_size(species, flock_seed);

    for (slot, held) in taken.iter().enumerate().take(wanted_size) {
        // The cap is over the whole sky rather than over one flock, so it still holds on
        // the frame a crossing has retired one row and is standing the next one up.
        if alive >= BIRD_COUNT_MAX {
            break;
        }
        if *held {
            continue;
        }
        alive += 1;
        let seed = bird_seed(flock_seed, slot);
        let (body, wing) = species.colours(seed);
        let wing_material = materials.add(plumage_material(wing));
        let bird = commands
            .spawn((
                Bird {
                    species: index,
                    seed,
                    index: slot,
                    anchor,
                },
                Mesh3d(visuals.body.clone()),
                MeshMaterial3d(materials.add(plumage_material(body))),
                Transform::from_translation(place(species, seed, elapsed, anchor))
                    .with_scale(Vec3::splat(species.size)),
                Visibility::Visible,
            ))
            .id();
        commands.entity(bird).with_children(|parent| {
            for left in [false, true] {
                parent.spawn((
                    BirdWing {
                        left,
                        flap_hz: species.flap_hz,
                    },
                    Mesh3d(visuals.wing.clone()),
                    MeshMaterial3d(wing_material.clone()),
                    Transform::default(),
                ));
            }
        });
    }
}

/// Moves every bird and beats its wings.
///
/// Three transforms per bird per frame and nothing else: at [`BIRD_COUNT_MAX`] that is
/// eighteen writes, which is why a flock costs less than one mob's snapshot application.
/// Measured on a headless client at a full flock of five, the pair of bird systems sits
/// inside the frame-to-frame noise of the whole `Update` schedule — see the pull request.
///
/// The wings are a second query rather than a child lookup because the parent's `Bird` is
/// already held here: `BirdWing` carries its own copy of the row's beat, so neither loop
/// has to reach into the other's entity and Bevy needs no `Without` between them beyond the
/// one that separates the two `Transform` accesses.
pub(super) fn fly_the_flock(
    time: Res<Time>,
    mut flock: Query<(&Bird, &mut Transform)>,
    mut wings: Query<(&BirdWing, &mut Transform), Without<Bird>>,
) {
    let elapsed = time.elapsed_secs();

    for (bird, mut transform) in &mut flock {
        let species = &BIRDS[bird.species];
        let position = place(species, bird.seed, elapsed, bird.anchor);
        transform.translation = position;
        // Which way it faces is the direction it is going, sampled from the same pure
        // function rather than differenced against last frame — so a bird nothing drew for
        // a hundred frames comes back facing correctly on the first one.
        let ahead = place(species, bird.seed, elapsed + HEADING_STEP, bird.anchor) - position;
        if let Ok(heading) = Dir3::new(ahead) {
            transform.look_to(heading.as_vec3(), Vec3::Y);
        }
    }

    for (wing, mut transform) in &mut wings {
        let angle = (elapsed * wing.flap_hz * TAU).sin() * FLAP_AMPLITUDE_RADIANS;
        // The mirrored wing is a half turn about Y first, which takes its arm to -X and
        // makes the same local rotation lift it by the same amount. A negative scale would
        // do it too and would invert the winding, which `cull_mode: None` hides rather than
        // fixes.
        let mirror = if wing.left { PI } else { 0.0 };
        transform.rotation = Quat::from_rotation_y(mirror) * Quat::from_rotation_z(angle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLES: usize = 120 * 60;
    const DT: f32 = 1.0 / 60.0;

    #[test]
    fn one_row_per_look_and_no_row_answers_an_unknown_one() {
        assert_eq!(species_for(&Ambience::default()), None);
        assert_eq!(
            species_for(&Ambience {
                ground: GroundLook::Grass,
                wooded: false,
            }),
            None,
            "an open plain has no parrots"
        );
        assert_eq!(
            species_for(&Ambience {
                ground: GroundLook::Grass,
                wooded: true,
            }),
            Some(0)
        );
        assert_eq!(
            species_for(&Ambience {
                ground: GroundLook::Sand,
                wooded: true,
            }),
            Some(1),
            "trees do not change what flies over sand"
        );
        assert_eq!(
            species_for(&Ambience {
                ground: GroundLook::Snow,
                wooded: false,
            }),
            Some(2)
        );
        assert!(
            BIRDS.iter().all(|row| row.ground != GroundLook::Unknown),
            "no row may fly over an answer that means there is no answer"
        );
    }

    #[test]
    fn a_bird_moves_no_faster_than_its_row_allows() {
        // The whole reason the path is a pure function: a bird may not teleport, and the
        // only way to know it does not is to walk it. Two minutes at sixty samples a second
        // covers several of every period in the table.
        let anchor = Vec3::new(96.0, 80.0, -32.0);
        for species in &BIRDS {
            for seed in 0..16u64 {
                let seed = mix(seed, 0xFACE);
                let mut previous = place(species, seed, 0.0, anchor);
                for sample in 1..=SAMPLES {
                    let now = place(species, seed, sample as f32 * DT, anchor);
                    let moved = now.distance(previous);
                    assert!(
                        moved <= species.max_speed * DT,
                        "{:?} moved {moved} in {DT}s, over its {} bound",
                        species.pattern,
                        species.max_speed
                    );
                    previous = now;
                }
            }
        }
    }

    #[test]
    fn a_bird_never_leaves_its_box() {
        // With the anchor still, the altitude bands and the pattern radii keep every bird
        // inside `BIRD_RANGE` by construction — so the only thing that ever puts one
        // outside is the anchor moving, which is the case `keep_the_flock` handles.
        let anchor = Vec3::new(-512.0, 64.0, 512.0);
        for species in &BIRDS {
            for seed in 0..16u64 {
                let seed = mix(seed, 0xB0A7);
                for sample in 0..=SAMPLES {
                    let from = place(species, seed, sample as f32 * DT, anchor) - anchor;
                    assert!(
                        from.abs().max_element() <= BIRD_RANGE,
                        "{:?} reached {from} from its anchor",
                        species.pattern
                    );
                    let altitude = from.y;
                    assert!(
                        altitude >= *species.altitude.start() - SPIRAL_RISE.max(ARC_RISE)
                            && altitude
                                <= *species.altitude.end() + SPIRAL_RISE.max(ARC_RISE) + 0.001,
                        "{:?} flew at {altitude}, outside its band",
                        species.pattern
                    );
                }
            }
        }
    }

    #[test]
    fn the_flock_size_and_the_plumage_stay_inside_their_rows() {
        for (index, species) in BIRDS.iter().enumerate() {
            let mut seen_sizes = [false; BIRD_COUNT_MAX + 1];
            for cell in -400..400 {
                let flock = cell_seed(IVec3::new(cell, 4, cell * 3));
                let size = flock_size(species, flock);
                assert!(
                    species.flock.contains(&(size as u8)) && size <= BIRD_COUNT_MAX,
                    "row {index} answered a flock of {size}"
                );
                seen_sizes[size] = true;

                let seed = bird_seed(flock, 0);
                let pair = species.colours(seed);
                let allowed = std::iter::once((species.body, species.wing))
                    .chain(species.plumage.iter().copied());
                assert!(
                    allowed.into_iter().any(|known| known == pair),
                    "row {index} wore a colour that is not in its table"
                );
            }
            let range = usize::from(*species.flock.start())..=usize::from(*species.flock.end());
            for size in range {
                assert!(seen_sizes[size], "row {index} never answered {size}");
            }
        }
    }

    #[test]
    fn the_anchor_is_the_centre_of_the_cell_the_eye_is_in() {
        // Half a cell from the eye at worst, on every axis, including the negative side of
        // the origin where a truncating cast would have put the cell one too high.
        for eye in [
            Vec3::ZERO,
            Vec3::new(31.9, 0.1, -0.1),
            Vec3::new(-0.5, -33.0, -64.0),
            Vec3::new(1024.0, 96.0, -1024.0),
        ] {
            let anchor = anchor_of(cell_of(eye));
            assert!(
                (anchor - eye).abs().max_element() <= BIRD_ANCHOR_CELL,
                "an eye at {eye} anchored at {anchor}"
            );
        }
        assert_eq!(cell_of(Vec3::new(-1.0, 0.0, 0.0)).x, -1);
        assert_eq!(cell_of(Vec3::new(32.0, 0.0, 0.0)).x, 1);
        assert_eq!(
            anchor_of(IVec3::ZERO),
            Vec3::splat(BIRD_ANCHOR_CELL / 2.0),
            "the anchor is the cell's centre, not its corner"
        );
    }

    #[test]
    fn a_seed_is_mixed_from_the_constant_and_the_cell_and_nothing_else() {
        // A neighbouring cell must not be a neighbouring seed: the flocks would rhyme, and
        // a player walking a straight line would watch the same birds re-appear.
        let mut seen = Vec::new();
        for x in -8..8 {
            for z in -8..8 {
                seen.push(cell_seed(IVec3::new(x, 2, z)));
            }
        }
        let mut sorted = seen.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), seen.len(), "two cells share a flock seed");

        // And the constant is load-bearing: change it and every flock changes.
        assert_ne!(
            cell_seed(IVec3::ZERO),
            mix(BIRD_SEED.wrapping_add(1), mix(0, mix(0, 0))),
        );
    }
}
