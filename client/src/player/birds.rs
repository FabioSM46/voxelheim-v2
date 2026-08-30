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
//! `player/tests.rs` pins the negative half of that: a bird carries no component from
//! `mobs.rs`, `hands.rs`, `drops.rs` or `structures.rs`, a snapshot with no mobs leaves the
//! flock alone, and a mining intent aimed straight along a bird produces the same bytes it
//! would with the sky empty. `target.rs` raycasts voxels and the bodies a snapshot named, so
//! there is no path from a bird into it at all.
//!
//! ## When a flock comes and goes
//!
//! A bird arrives and leaves over [`BIRD_FADE_SECONDS`] rather than between two frames, and
//! a replacement for one the anchor left behind is seeded on the far side of the move, so
//! nothing appears in the view the player is walking into. Two conditions stop the flock
//! outright, both read from `player/sky.rs` so that "it is night" and "the eye is under
//! water" have one answer in this client rather than two: the birds roost once
//! [`NIGHT_ROOST`] of the night has arrived, and they are hidden — not faded — while the eye
//! is submerged.
//!
//! ## How high a bird is, and the one thing that overrides it
//!
//! An altitude band is measured from the *anchor*, and the anchor is a quantised copy of the
//! eye — so a band says how far above the **player** a bird flies and nothing at all about
//! the ground under the bird. Stand in a valley beside a ridge and the arithmetic puts a
//! parrot inside the ridge. So `fly_the_flock` holds every bird to [`BIRD_CLEARANCE`] over
//! whatever is beneath it, eased in at [`CLEARANCE_LIFT_SPEED`] and bounded by
//! [`BIRD_RANGE`], as a named step over [`place`]'s answer rather than as a fifth argument
//! to it. It is a minimum height and nothing more: no collision, no avoidance, no
//! pathfinding, and no landing.
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
use bevy::ecs::system::SystemParam;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use super::ambience::{Ambience, GroundLook};
use super::camera::WorldCamera;
use super::sky::{self, SkyClock};
use crate::net::{BlockCoord, ChunkCoord, Session};
use crate::world::{ChunkStore, palette};

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

/// The most birds that may exist at once, fading ones included.
pub(super) const BIRD_COUNT_MAX: usize = 6;

/// How long a bird takes to fade in, and to fade out before it is despawned.
pub(super) const BIRD_FADE_SECONDS: f32 = 1.5;

/// The one constant every bird seed is mixed from.
///
/// **Never `world_seed`, and never an entity id.** Two players in the same desert see
/// different vultures on purpose: a bird nobody shares is a bird nobody can arrange to meet,
/// which is the cheapest available proof that none of this is state.
const BIRD_SEED: u64 = 0xB1BD_5EED_A17E_0F73;

/// How far a wing swings either side of level, in radians.
const FLAP_AMPLITUDE_RADIANS: f32 = 0.55;

/// The share of the night at which the flock roosts.
const NIGHT_ROOST: f32 = 0.5;

/// How many re-seeds are tried before a replacement is accepted wherever it fell.
///
/// Each try is one hash and about half land on the far side, so the expected cost is under
/// two and the fallback is reached about once in four thousand replacements. It is a real
/// branch rather than an unreachable one, and
/// `a_replacement_is_seeded_on_the_far_side_of_the_move` asserts the contract including it.
const FAR_SIDE_TRIES: u64 = 12;

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

/// How much clear air a bird keeps under it, in blocks.
///
/// A bird's altitude is measured from its anchor, and the anchor is the centre of the
/// [`BIRD_ANCHOR_CELL`] the *eye* is in — so "four blocks up" is four blocks above the
/// **player**, and says nothing at all about the ridge the bird is crossing. Stand in a
/// valley beside a hill and a parrot's band puts it inside the hill. This is the floor that
/// answer is held to, and holding it is the whole of the clamp: there is no avoidance and no
/// pathfinding here, only a height a bird may not be drawn below.
///
/// **Five blocks, argued from the three things it has to be at once.** It is five wingspans
/// of daylight under the widest row in [`BIRDS`] and fourteen under the narrowest, which is
/// the gap that reads as *flying over* a slope rather than as brushing it. It is a block
/// over the parrot's four-block band floor, so on broken ground the clamp is what decides a
/// low bird's height rather than half-deciding it with the band. And it is small enough that
/// a flock crossing a wood is still among the treetops rather than above the weather — the
/// surface a bird clears includes the canopy, so a larger number would push every parrot off
/// the trees its row exists to fly over.
const BIRD_CLEARANCE: f32 = 5.0;

/// How fast the clearance may lift a bird, in blocks per second.
///
/// The lift is eased rather than applied, for the reason the server's `approach` gives in
/// `internal/game/player.go`: a value that snaps to its target reads as a wall of velocity,
/// and a bird that jumps upward the instant it crosses a cliff edge is exactly that.
///
/// Four blocks a second is under the slowest row's own `max_speed` of 7.5, so the correction
/// never outruns the flight it is correcting, and it covers the whole of [`BIRD_CLEARANCE`]
/// in a little over a second — inside the [`BIRD_FADE_SECONDS`] a new bird arrives over, so
/// a bird seeded inside a hill is clear of it by the time it is fully drawn.
const CLEARANCE_LIFT_SPEED: f32 = 4.0;

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
    /// The colours one bird of this row wears.
    ///
    /// **Test-only**, and for the same reason [`BirdSpecies::max_speed`] below is: nothing
    /// draws a bird from a `Color` any more — [`BirdVisuals`] holds one material per plumage
    /// and a spawning bird clones the handle at [`BirdSpecies::plumage_of`] — so this exists
    /// to let `the_flock_size_and_the_plumage_stay_inside_their_rows` fail on a pair that is
    /// not in the row's table.
    #[cfg(test)]
    pub(super) fn colours(&self, seed: u64) -> (Color, Color) {
        self.plumage_at(self.plumage_of(seed))
    }

    /// Which plumage one bird of this row wears, as an index rather than a pair of colours.
    ///
    /// An index because the materials are built once and held in that order: a spawning bird
    /// needs the *slot*, so nothing has to look a `Color` up in a table to draw a bird.
    fn plumage_of(&self, seed: u64) -> usize {
        mix(seed, SALT_PLUMAGE) as usize % self.plumages()
    }

    /// How many plumages this row can wear — its own pair plus its variants.
    fn plumages(&self) -> usize {
        self.plumage.len() + 1
    }

    /// The `(body, wing)` pair at one plumage index: zero is the row's own pair and the rest
    /// come from [`BirdSpecies::plumage`], so a row with no variants has one answer.
    fn plumage_at(&self, choice: usize) -> (Color, Color) {
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
// How far the ground pushes a bird up
// ---------------------------------------------------------------------------
//
// [`place`] answers where a bird *would* be, from four arguments and nothing else, and
// nothing below changes that. The clamp is a second, named step over its answer, applied in
// `fly_the_flock` where the terrain and the previous frame's lift both already are — so the
// path stays a pure function of `(species, seed, elapsed, anchor)` and stays testable
// without a window, and the whole of what the ground does to a bird is one number.

/// One float floored to the voxel index containing it.
///
/// `floor`, never a bare cast, for the reason `player/target.rs`'s raycast gives: `-0.5 as
/// i32` truncates to 0 and the voxel containing -0.5 is -1. Half the world is on that side
/// of the origin. glam's cast saturates, so an absurd height gives an absurd index rather
/// than a wrapped one.
fn voxel_of(value: f32) -> i32 {
    Vec3::splat(value).floor().as_ivec3().x
}

/// What the probe found under a bird: three answers, not two.
///
/// It answered `Option<f32>` until the review on #640, and `None` meant two things that were
/// deliberately not told apart on the grounds that both wanted the same lift. They do not.
/// An empty window is a **measurement** — the surface is further down than the window
/// reaches, the clearance is already met, and letting the lift ease back to nothing is the
/// whole of how a bird comes down off a hill it has crossed. An unloaded chunk is the
/// **absence** of a measurement, and there the last frame that could see the ground is
/// better evidence than zero: decaying the lift walks a bird down into terrain that may
/// well be there, which is "an absent chunk is not evidence of a mountain" read backwards.
///
/// Collapsing the two is not a hypothetical mistake. `map_or(lift, ..)` — holding on both —
/// leaves a bird that has cleared a hill stranded at its old lift forever, and the suite
/// this type was added to did not catch that either: it is
/// `a_bird_holds_its_lift_only_where_the_ground_went_unread` that now separates them.
#[derive(Debug, Clone, Copy, PartialEq)]
enum GroundUnder {
    /// The top face of the first thing found under the bird.
    Surface(f32),
    /// Nothing in the window, and every chunk it crosses was there to be read. The surface
    /// is below the window, so the clearance is met and the lift may ease away.
    Clear,
    /// A chunk the window crosses is not loaded, so there is no answer at all.
    Unknown,
}

/// The top face of whatever is under a bird, looked for within one clearance of `drawn_y`.
///
/// **An absent chunk is not evidence of a mountain** — the same conservative direction
/// `Terrain.Fluid` takes, and the mesher's neighbour rule, and the server's step-up probe —
/// and it is not evidence of a plain either, which is why it answers
/// [`GroundUnder::Unknown`] rather than [`GroundUnder::Clear`]. The store is *read* here and
/// never asked to fetch: [`ChunkStore::get`] answering `None` ends the probe.
///
/// **Not air, rather than [`ChunkStore::solid_at`].** The question is what a bird would be
/// seen to fly into, and a lake's surface and a leaf canopy are both that, while solidity —
/// what stops a *body* — deliberately excludes water and cover. `block_at` alone cannot
/// answer it either: a coordinate in a chunk this session does not hold and one in a chunk
/// full of air are both `AIR`, which is why the presence check is separate.
///
/// **The window is `[drawn_y - BIRD_CLEARANCE - 1, drawn_y]`** — seven voxels, one probe per
/// bird per frame, forty-two at [`BIRD_COUNT_MAX`]. Two things about it are load-bearing.
/// The probe hangs off where the bird is **drawn** rather than off [`place`]'s answer, so a
/// bird buried deep in a hill walks its way out over a few frames instead of needing a
/// window as tall as the world. And the extra block *below* the clearance is what makes it
/// converge: at rest a bird sits exactly [`BIRD_CLEARANCE`] over the surface, so a window
/// one clearance deep would hold nothing but air, answer "no constraint", drop the bird back
/// into the hill and lift it again forever.
fn surface_under(store: &ChunkStore, column: Vec3, drawn_y: f32, chunk_size: usize) -> GroundUnder {
    // An argument nothing can be measured from is an absence, not an empty window.
    if !column.is_finite() || !drawn_y.is_finite() || chunk_size == 0 {
        return GroundUnder::Unknown;
    }
    let Ok(size) = i32::try_from(chunk_size) else {
        return GroundUnder::Unknown;
    };
    let x = voxel_of(column.x);
    let z = voxel_of(column.z);
    let high = voxel_of(drawn_y);
    let low = voxel_of(drawn_y - BIRD_CLEARANCE - 1.0);

    for y in (low..=high).rev() {
        let coord = ChunkCoord {
            cx: x.div_euclid(size),
            cy: y.div_euclid(size),
            cz: z.div_euclid(size),
        };
        // Downwards, and a gap ends the probe rather than being read through: a voxel this
        // session does not hold could be higher than anything found under it.
        if store.get(coord).is_none() {
            return GroundUnder::Unknown;
        }
        if store.block_at(BlockCoord { x, y, z }, chunk_size) != palette::AIR {
            // The voxel spans `[y, y + 1)`, so its top face is what a bird flies over.
            return GroundUnder::Surface((y + 1) as f32);
        }
    }
    GroundUnder::Clear
}

/// Moves `current` toward `target` by at most `step`, without overshooting.
///
/// The client's mirror of the server's `approach` in `internal/game/player.go`, and here for
/// the reason given there: a signed max/min pair rather than an exponential ease, because
/// there is no time constant to tune and "eases toward a target" is exactly what it says.
/// Once the target moves slower than `step`, this sits on it exactly rather than trailing
/// it — which is what lets a clamped bird hold the clearance to the bit while its pattern
/// keeps rising and falling underneath.
fn approach(current: f32, target: f32, step: f32) -> f32 {
    if current > target {
        (current - step).max(target)
    } else {
        (current + step).min(target)
    }
}

/// This frame's lift for one bird: the whole of the clamp, as one step over [`place`].
///
/// **The three answers [`GroundUnder`] gives are three different targets**, and only two of
/// them move a bird. A [`GroundUnder::Surface`] asks for the clearance over it. A
/// [`GroundUnder::Clear`] window asks for nothing, and the lift eases away — that is how a
/// bird comes down again once the hill it climbed is behind it. [`GroundUnder::Unknown`]
/// asks for **this frame's lift back**: nothing was measured, so nothing moves, and the
/// bird holds the height the last frame that could see the ground put it at. `ground` is
/// `None` for a frame with no session or no store, and that is the same absence — at spawn
/// the lift is zero, so holding it is the old behaviour to the bit.
///
/// **The lift is never negative.** The clearance is a floor under a bird and never a ceiling
/// over one, so a pattern already flying high enough is left alone to the last bit and flat
/// ground under the whole box changes nothing.
///
/// **And it is bounded by the box the anchor draws.** A bird lifted past [`BIRD_RANGE`] is
/// one `keep_the_flock` retires as left behind by its anchor, so where a hill is high enough
/// for the clearance and the box to disagree the box wins and the bird flies through the
/// hill. That is the cheaper of the two mistakes: `a_bird_never_leaves_its_box` is the
/// invariant every other part of this module is built on, and a bird that far under a ridge
/// is one nobody is looking at. The bound is applied twice — to the target and to the eased
/// result — because a ceiling that falls faster than [`CLEARANCE_LIFT_SPEED`] would
/// otherwise leave yesterday's lift outside today's box.
///
/// **That second bound cannot snap a bird downward, and it is worth saying why rather than
/// leaving it to be re-derived.** The review on #640 read `.min(ceiling)` as able to
/// teleport a bird when the anchor steps down a cell — but [`Bird::anchor`] is written once
/// at spawn and never again, so a living bird's ceiling has no term the eye can move. The
/// only thing that lowers it is the pattern's own climb, and that is slower than the ease
/// step by a factor of three: `a_falling_ceiling_lowers_a_bird_no_faster_than_it_raises_one`
/// measures both, over ground where the ceiling is the binding bound on nine frames in ten.
/// A crossing eye *retires* the birds it leaves behind and spawns replacements at zero lift;
/// it never re-aims a live one.
fn next_lift(
    ground: Option<(&ChunkStore, usize)>,
    unclamped: Vec3,
    anchor: Vec3,
    lift: f32,
    dt: f32,
) -> f32 {
    let ceiling = (anchor.y + BIRD_RANGE - unclamped.y).max(0.0);
    let under = ground.map_or(GroundUnder::Unknown, |(store, chunk_size)| {
        surface_under(store, unclamped, unclamped.y + lift, chunk_size)
    });
    let wanted = match under {
        GroundUnder::Surface(surface) => surface + BIRD_CLEARANCE - unclamped.y,
        GroundUnder::Clear => 0.0,
        // Held, not decayed. The box still bounds it below, so a lift kept across an
        // unloaded chunk cannot outlive a ceiling that has closed under it.
        GroundUnder::Unknown => lift,
    }
    .clamp(0.0, ceiling);
    approach(lift, wanted, CLEARANCE_LIFT_SPEED * dt).min(ceiling)
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

/// The seed of one bird: its flock's, its slot in that flock, and a re-seed salt.
fn bird_seed(flock: u64, index: usize, salt: u64) -> u64 {
    mix(flock, (index as u64).wrapping_add(salt.wrapping_mul(64)))
}

/// A seed whose home lies on the far side of a move, so nothing pops in ahead of the player.
///
/// The bias is the anchor's own displacement. After [`FAR_SIDE_TRIES`] it accepts the first
/// seed rather than looping: a bird that appears behind the player's shoulder is worth less
/// than a frame spent hunting for one.
fn seed_on_the_far_side(
    flock: u64,
    index: usize,
    species: &BirdSpecies,
    anchor: Vec3,
    bias: Vec3,
) -> u64 {
    let first = bird_seed(flock, index, 0);
    let Some(direction) = Vec3::new(bias.x, 0.0, bias.z).try_normalize() else {
        return first;
    };
    for salt in 0..FAR_SIDE_TRIES {
        let seed = bird_seed(flock, index, salt);
        let home = home_of(species, seed, anchor) - anchor;
        if Vec3::new(home.x, 0.0, home.z).dot(direction) > 0.0 {
            return seed;
        }
    }
    first
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

/// The two meshes every bird in the session is drawn from, and one material pair per slot.
///
/// The materials are built once, here, rather than at every spawn. The set is fixed and
/// tiny — one body and one wing per bird the sky can hold — while a flock is stood up and
/// retired every time the eye crosses an anchor cell, so `materials.add` at spawn time
/// minted a fresh `StandardMaterial` for a colour that already had one on every crossing.
///
/// **Keyed by slot rather than by plumage, and the fade is why.** A handle shared by a
/// whole plumage cannot carry a per-bird alpha: two parrots in one pair, one arriving and
/// one leaving, would fade as one bird. `keep_the_flock`'s second guard holds the sky to
/// [`BIRD_COUNT_MAX`], so a pool that size never runs dry and nothing is minted after
/// startup — which is what the per-plumage table bought.
#[derive(Resource, Debug)]
pub(super) struct BirdVisuals {
    body: Handle<Mesh>,
    wing: Handle<Mesh>,
    /// One `(body, wing)` pair per bird the sky can hold, claimed at spawn.
    pool: [(Handle<StandardMaterial>, Handle<StandardMaterial>); BIRD_COUNT_MAX],
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
    /// How much of the bird is drawn: 0 invisible, 1 whole.
    pub(super) fade: f32,
    /// What `fade` is moving towards. Zero means this bird is on its way out, and nothing
    /// ever moves it back, so a look that flickers cannot make a bird flicker with it.
    pub(super) wanted: f32,
    /// How far the clearance has lifted this bird above where [`place`] put it, in blocks.
    ///
    /// The only per-bird state the flight has, and it is deliberately the *offset* rather
    /// than a position: [`place`] remains the whole of where a bird would be, and this is
    /// how far the ground has pushed it off that. Zero is the untouched case and stays
    /// exactly zero, so a flock over ground it already clears is drawn where it was before
    /// the clamp existed.
    lift: f32,
    /// Which pair of [`BirdVisuals::pool`] this bird draws from. Distinct from `index`: a
    /// stray and a new bird can hold the same *flock* slot, and must not share an alpha.
    pool: usize,
    body_material: Handle<StandardMaterial>,
    wing_material: Handle<StandardMaterial>,
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

/// Builds the two meshes and every material any bird will ever wear.
pub(super) fn create_visuals(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(BirdVisuals {
        body: meshes.add(quad(Vec2::new(-0.15, -0.5), Vec2::new(0.15, 0.5))),
        // Authored from the hinge outwards, so rotating the child about its own origin is
        // the flap and nothing has to offset it.
        wing: meshes.add(quad(Vec2::new(0.0, -0.25), Vec2::new(0.5, 0.25))),
        // Colourless and invisible until a bird claims the pair and writes its plumage in.
        pool: std::array::from_fn(|_| {
            (
                materials.add(plumage_material(Color::WHITE, 0.0)),
                materials.add(plumage_material(Color::WHITE, 0.0)),
            )
        }),
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

/// Lit, blended and drawn from both faces.
///
/// **Lit** is the choice worth naming: `player/sky.rs`'s bodies are unlit because they are
/// the light source, and a bird is not — it is an object in the world, so night darkens it
/// and the fog takes it at distance exactly as they take a mob.
///
/// **`cull_mode: None`** because a flat quad is seen from below as often as from above, the
/// same reason `mobs.rs` gives for the aggro marker. **`AlphaMode::Blend` and an explicit
/// alpha** because the fade is written here, which is also why the pair a bird draws from
/// is its own rather than its plumage's — see [`BirdVisuals`].
fn plumage_material(colour: Color, alpha: f32) -> StandardMaterial {
    StandardMaterial {
        base_color: colour.with_alpha(alpha),
        alpha_mode: AlphaMode::Blend,
        cull_mode: None,
        ..default()
    }
}

/// Everything `keep_the_flock` reads and nothing it writes.
#[derive(SystemParam)]
pub(super) struct FlockInputs<'w> {
    ambience: Res<'w, Ambience>,
    session: Option<Res<'w, Session>>,
    clock: Res<'w, SkyClock>,
    time: Res<'w, Time>,
    visuals: Option<Res<'w, BirdVisuals>>,
}

/// Decides which birds should exist, and stands the missing ones up.
///
/// Runs after `camera::AimCamera` and after `ambience::sample_the_ground`, so the anchor is
/// this frame's eye and the look is this frame's answer. It writes nothing outside its own
/// entities.
pub(super) fn keep_the_flock(
    read: FlockInputs<'_>,
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    eyes: Query<&Transform, With<WorldCamera>>,
    mut flock: Query<(Entity, &mut Bird)>,
) {
    let FlockInputs {
        ambience,
        session,
        clock,
        time,
        visuals,
    } = read;
    let (Some(visuals), Some(eye)) = (visuals, eyes.iter().next()) else {
        return;
    };
    if !eye.translation.is_finite() {
        return;
    }

    let cell = cell_of(eye.translation);
    let anchor = anchor_of(cell);
    let elapsed = time.elapsed_secs();

    // Roosted at night, and only when the server keeps a clock: `night_now` answers `None`
    // for a world with no time of day, which flies them all day rather than never.
    let roosting = session
        .as_deref()
        .and_then(|session| sky::night_now(&clock, session))
        .is_some_and(|night| night >= NIGHT_ROOST);
    let wanted = if roosting {
        None
    } else {
        species_for(&ambience)
    };
    let flock_seed = cell_seed(cell);
    // Read before the retirement pass, because how many this cell wants is what decides how
    // many of the previous cell's birds may stay.
    let wanted_size = wanted.map_or(0, |index| flock_size(&BIRDS[index], flock_seed));

    // Retire everything that is the wrong species for this look, or that the anchor has
    // left behind. A bird outside its box is one the eye has walked away from: its path is
    // drawn around an anchor half a world back, so keeping it would be keeping a bird
    // nobody can see. Retiring is one-way, so a look that flickers cannot oscillate a flock.
    //
    // **Two counts, because a fade makes "how many birds are there" two questions.**
    // `staying` is every bird not on its way out, bounded by `wanted_size`, so the flying
    // population is one row's and never two anchors' summed. `alive` is every entity, the
    // fading ones included, bounded by `BIRD_COUNT_MAX` — which is also what guarantees the
    // material pool has a free pair for a bird about to spawn.
    let mut bias = Vec3::ZERO;
    let mut taken = [false; BIRD_COUNT_MAX];
    let mut pool_taken = [false; BIRD_COUNT_MAX];
    let mut alive = 0usize;
    let mut staying = 0usize;
    // A bird of the right row still inside the box whose anchor is a *previous* cell's.
    // It holds no slot in `taken` — its `index` numbers another anchor's flock — so it is
    // counted against `wanted_size` below instead of being invisible to it. It used to be
    // invisible: a one-cell walk left the old flock in the air, the spawn loop read every
    // slot as free, and a second flock went up beside the first.
    let mut strays = [None; BIRD_COUNT_MAX];
    let mut stray_count = 0usize;
    for (entity, mut bird) in &mut flock {
        alive += 1;
        pool_taken[bird.pool] = true;
        // Already leaving: it holds a pool pair and counts against the sky, but nothing
        // here may bring it back.
        if bird.wanted == 0.0 {
            continue;
        }
        let position = place(&BIRDS[bird.species], bird.seed, elapsed, bird.anchor);
        let outside = (position - anchor).abs().max_element() > BIRD_RANGE;
        if wanted != Some(bird.species) || outside {
            bird.wanted = 0.0;
            // Only a bird the *anchor* left behind says which way the player went; one
            // retired because the ground changed under them says nothing about direction.
            if outside {
                bias += anchor - bird.anchor;
            }
            continue;
        }
        if bird.anchor == anchor && bird.index < BIRD_COUNT_MAX {
            // This cell's own flock. `index < wanted_size` holds by construction: the
            // anchor determines the cell, and the cell determines `wanted_size`.
            taken[bird.index] = true;
            staying += 1;
        } else if stray_count < BIRD_COUNT_MAX {
            strays[stray_count] = Some(entity);
            stray_count += 1;
        }
    }

    // A stray flies on only while this cell's flock has room for it, and starts fading the
    // moment it does not — it fades rather than vanishing, and the count is `wanted_size`.
    for entity in strays.into_iter().flatten() {
        if staying < wanted_size {
            staying += 1;
        } else if let Ok((_, mut bird)) = flock.get_mut(entity) {
            bird.wanted = 0.0;
        }
    }

    let Some(index) = wanted else {
        return;
    };
    let species = &BIRDS[index];

    for (slot, held) in taken.iter().enumerate().take(wanted_size) {
        // The flock is the cap, and `flock_size` is already clamped to BIRD_COUNT_MAX. The
        // second guard is the whole sky rather than one flock: a bird still fading out holds
        // a material pair, so a free pair exists only while `alive` is under the maximum.
        if staying >= wanted_size || alive >= BIRD_COUNT_MAX {
            break;
        }
        if *held {
            continue;
        }
        let Some(pool) = pool_taken.iter().position(|claimed| !claimed) else {
            break;
        };
        pool_taken[pool] = true;
        staying += 1;
        alive += 1;
        let seed = seed_on_the_far_side(flock_seed, slot, species, anchor, bias);
        // Claimed, not minted: `create_visuals` built every pair, and this writes the
        // plumage the seed chose into the two handles the slot owns.
        let (body_colour, wing_colour) = species.plumage_at(species.plumage_of(seed));
        let (body_material, wing_material) = visuals.pool[pool].clone();
        if let Some(mut material) = materials.get_mut(&body_material) {
            *material = plumage_material(body_colour, 0.0);
        }
        if let Some(mut material) = materials.get_mut(&wing_material) {
            *material = plumage_material(wing_colour, 0.0);
        }
        let bird = commands
            .spawn((
                Bird {
                    species: index,
                    seed,
                    index: slot,
                    anchor,
                    fade: 0.0,
                    wanted: 1.0,
                    // Where the pattern put it. `fly_the_flock` eases the ground's answer
                    // in on the frames that follow, inside the fade it arrives over.
                    lift: 0.0,
                    pool,
                    body_material: body_material.clone(),
                    wing_material: wing_material.clone(),
                },
                Mesh3d(visuals.body.clone()),
                MeshMaterial3d(body_material),
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

/// The one camera, told apart from the entities this system also holds mutably.
///
/// Bevy cannot prove a `WorldCamera` is neither a bird nor a wing, and refuses the system
/// rather than risk aliasing the `Transform` — the same reason `player/sky.rs` filters its
/// eye query `Without<Sun>`. A named type because the filter is otherwise long enough for
/// clippy to call the query complex, and a name is better than an allow.
type EyeOfTheFlock = (With<WorldCamera>, Without<Bird>, Without<BirdWing>);

/// Everything `fly_the_flock` reads.
#[derive(SystemParam)]
pub(super) struct FlightInputs<'w> {
    session: Option<Res<'w, Session>>,
    store: Option<Res<'w, ChunkStore>>,
    time: Res<'w, Time>,
}

/// Moves every bird, beats its wings, and fades the ones on their way out.
///
/// Three transforms per bird per frame and one colour write when the alpha has actually
/// moved: at [`BIRD_COUNT_MAX`] that is eighteen transforms, which is why a flock costs less
/// than one mob's snapshot application. Measured on a headless client at a full flock of
/// five, the pair of bird systems sits inside the frame-to-frame noise of the whole `Update`
/// schedule — see the pull request.
///
/// The wings are a second query rather than a child lookup because the parent's `Bird` is
/// already held here: `BirdWing` carries its own copy of the row's beat, so neither loop has
/// to reach into the other's entity.
///
/// It is also where the ground clamp lives, because this is where the terrain and the
/// previous frame's lift both already are — see [`next_lift`]. That adds one bounded column
/// probe per bird per frame to the budget above, next to the one [`sky::submerged_at`]
/// already takes for the eye.
pub(super) fn fly_the_flock(
    read: FlightInputs<'_>,
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    eyes: Query<&Transform, EyeOfTheFlock>,
    mut flock: Query<(Entity, &mut Bird, &mut Transform, &mut Visibility)>,
    mut wings: Query<(&BirdWing, &mut Transform), Without<Bird>>,
) {
    let FlightInputs {
        session,
        store,
        time,
    } = read;
    let elapsed = time.elapsed_secs();
    let step = time.delta_secs() / BIRD_FADE_SECONDS;

    // Under water the sky above the surface is not what the eye sees, so the birds are
    // hidden outright rather than faded: the same override `player/sky.rs` applies to the
    // fog, read through the same answer so there are not two of them. Hidden and not
    // retired, because surfacing must not cost a second and a half of empty sky.
    let submerged = match (session.as_deref(), eyes.iter().next()) {
        (Some(session), Some(eye)) => sky::submerged_at(
            store.as_deref(),
            eye.translation,
            usize::from(session.0.chunk_size),
        ),
        _ => false,
    };
    // The terrain the clearance is measured against, if there is any to read. A frame with
    // no session or no store answers the same way an unloaded chunk does: no clamp.
    let ground = match (store.as_deref(), session.as_deref()) {
        (Some(store), Some(session)) => Some((store, usize::from(session.0.chunk_size))),
        _ => None,
    };

    for (entity, mut bird, mut transform, mut visibility) in &mut flock {
        let fade = if bird.wanted > bird.fade {
            (bird.fade + step).min(bird.wanted)
        } else {
            (bird.fade - step).max(bird.wanted)
        };
        if bird.wanted == 0.0 && fade <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }
        if fade != bird.fade {
            bird.fade = fade;
            for handle in [bird.body_material.clone(), bird.wing_material.clone()] {
                if let Some(mut material) = materials.get_mut(&handle) {
                    material.base_color = material.base_color.with_alpha(fade);
                }
            }
        }

        let species = &BIRDS[bird.species];
        let position = place(species, bird.seed, elapsed, bird.anchor);
        // The clamp: a named step over `place`'s answer, never a fifth argument to it. It
        // moves the bird up and never sideways, so everything below still reads the
        // pattern's own point.
        let lift = next_lift(ground, position, bird.anchor, bird.lift, time.delta_secs());
        // Guarded for the reason the visibility write below is: `Mut` marks a component
        // changed on every `DerefMut`, and over ground a flock already clears this is zero
        // every frame forever.
        if lift != bird.lift {
            bird.lift = lift;
        }
        transform.translation = position + Vec3::Y * lift;
        // Which way it faces is the direction it is going, sampled from the same pure
        // function rather than differenced against last frame — so a bird nothing drew for
        // a hundred frames comes back facing correctly on the first one. Both samples are
        // unclamped, so the heading stays the pattern's and a lift never tips a bird's nose
        // up: the clearance changes where a bird is, not where it is going.
        let ahead = place(species, bird.seed, elapsed + HEADING_STEP, bird.anchor) - position;
        if let Ok(heading) = Dir3::new(ahead) {
            transform.look_to(heading.as_vec3(), Vec3::Y);
        }

        let should = if submerged {
            Visibility::Hidden
        } else {
            Visibility::Visible
        };
        // Guarded: `Mut` marks a component changed on every `DerefMut`, and re-extracting a
        // visibility that has not moved is a cost for nothing.
        if *visibility != should {
            *visibility = should;
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
    use crate::world::{BlockId, VoxelChunk};

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

                let seed = bird_seed(flock, 0, 0);
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
    fn a_replacement_is_seeded_on_the_far_side_of_the_move() {
        // The whole point of the re-seed: a bird that appears must appear behind the
        // player's shoulder blade, never in the middle of the view they are walking into.
        let anchor = Vec3::new(64.0, 96.0, 64.0);
        let flock = cell_seed(IVec3::new(2, 3, 2));
        for species in &BIRDS {
            for (bias, axis) in [
                (Vec3::X, Vec3::X),
                (Vec3::NEG_X, Vec3::NEG_X),
                (Vec3::Z, Vec3::Z),
                (Vec3::new(-3.0, 7.0, -3.0), Vec3::new(-1.0, 0.0, -1.0)),
            ] {
                for slot in 0..BIRD_COUNT_MAX {
                    let seed = seed_on_the_far_side(flock, slot, species, anchor, bias * 32.0);
                    let far = |seed| {
                        let home = home_of(species, seed, anchor) - anchor;
                        Vec3::new(home.x, 0.0, home.z).dot(axis.normalize()) > 0.0
                    };
                    if far(seed) {
                        continue;
                    }
                    // The documented fallback, asserted rather than tolerated: it may only
                    // be reached when every salt in range was on the near side, and it may
                    // only ever answer the first seed.
                    assert_eq!(
                        seed,
                        bird_seed(flock, slot, 0),
                        "{:?} fell back to a seed that is not the first",
                        species.pattern
                    );
                    assert!(
                        !(0..FAR_SIDE_TRIES).any(|salt| far(bird_seed(flock, slot, salt))),
                        "{:?} fell back past a seed that was on the far side",
                        species.pattern
                    );
                }
            }
        }
        // No move, no bias, and the first seed is taken as it comes.
        assert_eq!(
            seed_on_the_far_side(flock, 0, &BIRDS[0], anchor, Vec3::ZERO),
            bird_seed(flock, 0, 0)
        );
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

    // -----------------------------------------------------------------------
    // The ground under a bird
    // -----------------------------------------------------------------------

    /// The chunk edge every clamp fixture below is built at, and asks about.
    const CHUNK: usize = 32;

    /// How long a walk is given to settle before its height is read.
    ///
    /// A bird seeded inside a hill climbs out of it a clearance at a time, because the
    /// probe hangs off where it is *drawn*. Ten seconds is forty blocks of climb at
    /// [`CLEARANCE_LIFT_SPEED`], which is most of a box.
    const SETTLED: usize = 10 * 60;

    /// A store over every chunk a box of `reach` around `centre` touches, holding `block`
    /// wherever `solid` says so and air everywhere else.
    ///
    /// Synthetic on purpose: the clamp's whole input is "what is under this column", so a
    /// terrain a test can state in one closure is the only fixture it needs.
    fn terrain(
        centre: Vec3,
        reach: f32,
        block: BlockId,
        solid: impl Fn(IVec3) -> bool,
    ) -> ChunkStore {
        let span = CHUNK as i32;
        let low = (centre - Vec3::splat(reach)).floor().as_ivec3();
        let high = (centre + Vec3::splat(reach)).floor().as_ivec3();
        let mut store = ChunkStore::default();
        for cx in low.x.div_euclid(span)..=high.x.div_euclid(span) {
            for cy in low.y.div_euclid(span)..=high.y.div_euclid(span) {
                for cz in low.z.div_euclid(span)..=high.z.div_euclid(span) {
                    let mut chunk = VoxelChunk::all_air(CHUNK);
                    for ly in 0..CHUNK {
                        for lz in 0..CHUNK {
                            for lx in 0..CHUNK {
                                let at = IVec3::new(
                                    cx * span + lx as i32,
                                    cy * span + ly as i32,
                                    cz * span + lz as i32,
                                );
                                if solid(at) {
                                    chunk.set(lx, ly, lz, block);
                                }
                            }
                        }
                    }
                    store.insert(ChunkCoord { cx, cy, cz }, chunk);
                }
            }
        }
        store
    }

    /// Walks one bird for `samples` frames and answers `(drawn point, lift)` for each.
    ///
    /// It drives [`next_lift`] rather than restating what `fly_the_flock` does with it: a
    /// test that re-implemented the clamp would pass whatever the client actually drew.
    fn flown(
        ground: Option<(&ChunkStore, usize)>,
        species: &BirdSpecies,
        seed: u64,
        anchor: Vec3,
        samples: usize,
    ) -> Vec<(Vec3, f32)> {
        let mut lift = 0.0;
        let mut path = Vec::with_capacity(samples + 1);
        for sample in 0..=samples {
            let unclamped = place(species, seed, sample as f32 * DT, anchor);
            lift = next_lift(ground, unclamped, anchor, lift, DT);
            path.push((unclamped + Vec3::Y * lift, lift));
        }
        path
    }

    #[test]
    fn the_surface_a_bird_clears_is_the_top_face_of_what_is_under_it() {
        // Solid below 40, so the highest voxel is 39. It spans `[39, 40)`, and 40 is the
        // height a bird flies over.
        let store = terrain(Vec3::new(8.0, 40.0, 8.0), 40.0, palette::STONE, |at| {
            at.y < 40
        });
        let column = Vec3::new(8.5, 0.0, 8.5);
        assert_eq!(
            surface_under(&store, column, 44.0, CHUNK),
            GroundUnder::Surface(40.0)
        );

        // At rest a bird sits exactly the clearance up, and the window has to still see the
        // block that put it there. This is the one probed block *below* `BIRD_CLEARANCE`
        // earning its place: without it the answer here is `None`, the lift falls to zero,
        // the bird drops back into the hill, and it lifts again forever.
        assert_eq!(
            surface_under(&store, column, 40.0 + BIRD_CLEARANCE, CHUNK),
            GroundUnder::Surface(40.0)
        );

        // Higher than that and the ground has nothing to say about where the bird flies.
        // `Clear`, not `Unknown`: every chunk the window crosses was read, and finding
        // nothing in it is an answer rather than the lack of one.
        assert_eq!(
            surface_under(&store, column, 48.0, CHUNK),
            GroundUnder::Clear
        );

        // And it floors rather than truncating, on the side of the origin where the two
        // differ — the trap `player/target.rs`'s raycast names, over half the world.
        let below = terrain(Vec3::new(-8.0, -8.0, -8.0), 24.0, palette::STONE, |at| {
            at.y < -8
        });
        assert_eq!(
            surface_under(&below, Vec3::new(-0.5, 0.0, -0.5), -4.0, CHUNK),
            GroundUnder::Surface(-8.0)
        );
    }

    #[test]
    fn a_bird_clears_a_lake_surface_and_a_canopy_rather_than_what_is_under_them() {
        // Not `ChunkStore::solid_at`: solidity answers what stops a *body*, and since #446
        // and #550 it deliberately excludes water and cover. The question here is what a
        // bird would be seen to fly into, and a lake's surface and a leaf canopy are both
        // that — a clearance measured to a lake bed would draw a parrot under water.
        for block in [
            palette::STONE,
            palette::WATER,
            palette::WATER_FLOW3,
            palette::LEAVES,
            palette::FLOWER_RED,
        ] {
            let store = terrain(Vec3::new(8.0, 40.0, 8.0), 40.0, block, |at| at.y == 39);
            assert_eq!(
                surface_under(&store, Vec3::new(8.5, 0.0, 8.5), 44.0, CHUNK),
                GroundUnder::Surface(40.0),
                "a bird was flown through block {block}"
            );
        }
    }

    #[test]
    fn terrain_nobody_has_streamed_is_not_evidence_of_a_mountain() {
        // Absence is not evidence — the direction `Terrain.Fluid`, the mesher's neighbour
        // rule and the server's step-up probe all take. A bird over a chunk that has not
        // arrived is left exactly where the pattern put it.
        let nothing = ChunkStore::default();
        let column = Vec3::new(8.5, 0.0, 8.5);
        assert_eq!(
            surface_under(&nothing, column, 44.0, CHUNK),
            GroundUnder::Unknown
        );

        let unclamped = Vec3::new(8.5, 44.0, 8.5);
        let anchor = Vec3::new(8.5, 40.0, 8.5);
        assert_eq!(
            next_lift(Some((&nothing, CHUNK)), unclamped, anchor, 0.0, DT),
            0.0
        );
        // And a frame with no store or no session at all takes the same direction.
        assert_eq!(next_lift(None, unclamped, anchor, 0.0, DT), 0.0);

        // A gap is not read *through*, either. One chunk holds a hilltop at 40 and the
        // chunk above it never arrived: a probe that crossed the hole would answer with
        // the highest thing it happens to hold rather than with the highest thing there is.
        let mut chunk = VoxelChunk::all_air(8);
        for y in 0..8 {
            for z in 0..8 {
                for x in 0..8 {
                    chunk.set(x, y, z, palette::STONE);
                }
            }
        }
        let mut gapped = ChunkStore::default();
        gapped.insert(
            ChunkCoord {
                cx: 0,
                cy: 4,
                cz: 0,
            },
            chunk,
        );
        let column = Vec3::new(4.5, 0.0, 4.5);
        // `[32, 40)` is the one chunk there is, and inside it the answer is the honest one.
        assert_eq!(
            surface_under(&gapped, column, 39.0, 8),
            GroundUnder::Surface(40.0)
        );
        // Six blocks higher the window opens into the missing chunk, and the hilltop two
        // blocks under it is no longer an answer anybody may give.
        assert_eq!(
            surface_under(&gapped, column, 44.0, 8),
            GroundUnder::Unknown
        );
    }

    #[test]
    fn ground_a_flock_already_clears_moves_no_bird() {
        // The clearance is a floor under a bird and never a ceiling over one. With a level
        // surface under the whole box, every bird is exactly where it was before the clamp
        // existed — to the bit, and with no lift to write.
        let anchor = Vec3::new(16.0, 80.0, 16.0);
        let store = terrain(anchor, BIRD_RANGE + 8.0, palette::STONE, |at| at.y < 16);
        // Not vacuous: the floor of the box is there to be found.
        assert_eq!(
            surface_under(&store, anchor, 20.0, CHUNK),
            GroundUnder::Surface(16.0)
        );

        for species in &BIRDS {
            for seed in 0..4u64 {
                let seed = mix(seed, 0xF1A7);
                for (sample, (drawn, lift)) in
                    flown(Some((&store, CHUNK)), species, seed, anchor, SAMPLES)
                        .into_iter()
                        .enumerate()
                {
                    assert_eq!(
                        lift, 0.0,
                        "{:?} was lifted over ground it already cleared",
                        species.pattern
                    );
                    assert_eq!(drawn, place(species, seed, sample as f32 * DT, anchor));
                }
            }
        }
    }

    #[test]
    fn a_bird_inside_a_hill_is_drawn_exactly_the_clearance_over_it() {
        let anchor = Vec3::new(16.0, 80.0, 16.0);
        for species in &BIRDS {
            // A hill reaching the floor of this row's own band, so every row is genuinely
            // buried for part of its pattern rather than only the low one.
            let top = anchor.y + *species.altitude.start();
            let store = terrain(anchor, BIRD_RANGE + 8.0, palette::STONE, |at| {
                (at.y as f32) < top
            });
            let mut lowest = f32::INFINITY;
            for seed in 0..4u64 {
                let seed = mix(seed, 0xC11F);
                for (sample, (drawn, _)) in
                    flown(Some((&store, CHUNK)), species, seed, anchor, SAMPLES)
                        .into_iter()
                        .enumerate()
                        .skip(SETTLED)
                {
                    assert!(
                        drawn.y >= top + BIRD_CLEARANCE - 1e-3,
                        "{:?} was drawn at {} over a hilltop at {top}",
                        species.pattern,
                        drawn.y
                    );
                    // Up, and only up: the clamp is one axis and the pattern owns the
                    // other two.
                    let unclamped = place(species, seed, sample as f32 * DT, anchor);
                    assert_eq!((drawn.x, drawn.z), (unclamped.x, unclamped.z));
                    assert!(drawn.y >= unclamped.y);
                    lowest = lowest.min(drawn.y);
                }
            }
            // Exactly the clearance, and reached: a bound nothing ever touches would be
            // satisfied by a bird parked in the stratosphere.
            assert!(
                (lowest - (top + BIRD_CLEARANCE)).abs() <= 1e-3,
                "{:?} settled at {lowest}, not at {}",
                species.pattern,
                top + BIRD_CLEARANCE
            );
        }
    }

    #[test]
    fn a_step_in_the_ground_lifts_a_bird_without_teleporting_it() {
        // A cliff through the middle of the box: low ground on one side, a hundred-block
        // wall on the other. Crossing it a bird climbs, and the whole reason the lift is
        // approached rather than assigned is that it must not jump.
        let anchor = Vec3::new(16.0, 80.0, 16.0);
        let store = terrain(anchor, BIRD_RANGE + 8.0, palette::STONE, |at| {
            at.y < if at.x < 16 { 8 } else { 108 }
        });
        let mut climbed = 0usize;
        for species in &BIRDS {
            for seed in 0..4u64 {
                let seed = mix(seed, 0xC11F);
                for pair in flown(Some((&store, CHUNK)), species, seed, anchor, SAMPLES).windows(2)
                {
                    let ((was, before), (now, after)) = (pair[0], pair[1]);
                    // The row's own bound plus the lift's, which is the whole of what the
                    // clamp may add — `a_bird_moves_no_faster_than_its_row_allows` pins the
                    // first term on the unclamped path and this pins the sum on the drawn
                    // one.
                    let moved = now.distance(was);
                    assert!(
                        moved <= (species.max_speed + CLEARANCE_LIFT_SPEED) * DT + 1e-4,
                        "{:?} moved {moved} in {DT}s at a cliff edge",
                        species.pattern
                    );
                    assert!(
                        (after - before).abs() <= CLEARANCE_LIFT_SPEED * DT + 1e-4,
                        "{:?} snapped its lift from {before} to {after}",
                        species.pattern
                    );
                    climbed += usize::from(after > before);
                }
            }
        }
        assert!(
            climbed > 0,
            "no bird ever met the cliff, so this test would pass vacuously"
        );
    }

    #[test]
    fn a_clamped_bird_never_leaves_its_box() {
        // `a_bird_never_leaves_its_box`, with the clamp on. Solid rock everywhere means the
        // clearance asks for more lift than `BIRD_RANGE` allows, and the box wins — the
        // documented trade, and the invariant `keep_the_flock`'s retirement rests on.
        let anchor = Vec3::new(16.0, 80.0, 16.0);
        let store = terrain(anchor, BIRD_RANGE + 8.0, palette::STONE, |_| true);
        let mut ceilinged = 0usize;
        for species in &BIRDS {
            for seed in 0..4u64 {
                let seed = mix(seed, 0xB0C5);
                for (drawn, _) in flown(Some((&store, CHUNK)), species, seed, anchor, SAMPLES) {
                    let from = drawn - anchor;
                    assert!(
                        from.abs().max_element() <= BIRD_RANGE + 1e-3,
                        "{:?} reached {from} from its anchor",
                        species.pattern
                    );
                    ceilinged += usize::from(from.y >= BIRD_RANGE - 1e-3);
                }
            }
        }
        assert!(
            ceilinged > 0,
            "nothing ever reached the ceiling, so the box was never the binding bound"
        );
    }

    #[test]
    fn a_bird_holds_its_lift_only_where_the_ground_went_unread() {
        // The review on #640 read `map_or(0.0, ..)` as decaying a real lift to nothing the
        // moment the chunk under a bird stopped being readable, and it was right: a bird
        // eased downward into terrain the last frame that could see it had measured. What
        // the suggested `map_or(lift, ..)` would also do is hold the lift where the window
        // is *empty*, and that is the case a bird descends through — so the two reasons
        // `surface_under` had for answering "no surface" are separated instead, and this
        // pins both directions of the separation.
        let anchor = Vec3::new(16.0, 80.0, 16.0);
        let unclamped = Vec3::new(16.5, 80.0, 16.5);
        let step = CLEARANCE_LIFT_SPEED * DT;
        let held = 20.0;

        // Read, and empty: the surface is far below the window, the clearance is met, and
        // the lift eases off. Hold it here and a bird that has crossed a hill never comes
        // down again — measured before this test existed, at exactly 20.0 after ten frames.
        let loaded = terrain(anchor, BIRD_RANGE + 8.0, palette::STONE, |at| at.y < 16);
        assert_eq!(
            surface_under(&loaded, unclamped, unclamped.y + held, CHUNK),
            GroundUnder::Clear
        );
        assert_eq!(
            next_lift(Some((&loaded, CHUNK)), unclamped, anchor, held, DT),
            held - step
        );

        // Unread: nothing was measured this frame, so nothing moves. The bird stays where
        // the last frame that could see the ground put it.
        let nothing = ChunkStore::default();
        assert_eq!(
            surface_under(&nothing, unclamped, unclamped.y + held, CHUNK),
            GroundUnder::Unknown
        );
        assert_eq!(
            next_lift(Some((&nothing, CHUNK)), unclamped, anchor, held, DT),
            held
        );
        // A frame with no session and no store at all is the same absence.
        assert_eq!(next_lift(None, unclamped, anchor, held, DT), held);

        // Holding is still bounded by the box, which is what keeps `GroundUnder::Unknown`
        // from outliving a ceiling that closed under it while nobody could read the ground.
        let ceiling = anchor.y + BIRD_RANGE - unclamped.y;
        assert_eq!(
            next_lift(None, unclamped, anchor, ceiling + 10.0, DT),
            ceiling
        );

        // And an unread frame changes nothing at spawn, where the lift is zero — the
        // behaviour `terrain_nobody_has_streamed_is_not_evidence_of_a_mountain` pins.
        assert_eq!(next_lift(None, unclamped, anchor, 0.0, DT), 0.0);
    }

    #[test]
    fn a_falling_ceiling_lowers_a_bird_no_faster_than_it_raises_one() {
        // The other half of `a_step_in_the_ground_lifts_a_bird_without_teleporting_it`,
        // which only ever climbs a cliff. The review on #640 asked whether the
        // `.min(ceiling)` after `approach` can move a lift *down* faster than the ease
        // step. Solid rock everywhere is where it could: the clearance asks for more than
        // the box allows, so the ceiling is the binding bound nearly every frame, and it
        // falls whenever the pattern climbs.
        let anchor = Vec3::new(16.0, 80.0, 16.0);
        let store = terrain(anchor, BIRD_RANGE + 8.0, palette::STONE, |_| true);
        let mut fell_at_the_ceiling = 0usize;
        for species in &BIRDS {
            for seed in 0..4u64 {
                let seed = mix(seed, 0xB0C5);
                for pair in flown(Some((&store, CHUNK)), species, seed, anchor, SAMPLES).windows(2)
                {
                    let ((was, before), (now, after)) = (pair[0], pair[1]);
                    assert!(
                        (after - before).abs() <= CLEARANCE_LIFT_SPEED * DT + 1e-4,
                        "{:?} snapped its lift from {before} to {after} under a falling ceiling",
                        species.pattern
                    );
                    // What a player actually sees. The drawn point is the pattern's own,
                    // plus a lift the ceiling may cut — and cutting a value can only ever
                    // move it *toward* the previous frame's, never past it.
                    let moved = now.distance(was);
                    assert!(
                        moved <= (species.max_speed + CLEARANCE_LIFT_SPEED) * DT + 1e-4,
                        "{:?} moved {moved} in {DT}s with the box as its bound",
                        species.pattern
                    );
                    if after < before && now.y - anchor.y >= BIRD_RANGE - 1e-3 {
                        fell_at_the_ceiling += 1;
                    }
                }
            }
        }
        assert!(
            fell_at_the_ceiling > 0,
            "no lift ever fell while the box was the binding bound, so this proves nothing"
        );
    }

    #[test]
    fn a_lift_approaches_its_target_and_then_sits_on_it() {
        // The server's `approach`, mirrored: no overshoot in either direction, and exact
        // once the target is within one step — which is what lets a clamped bird hold the
        // clearance to the bit while its pattern rises and falls underneath.
        assert_eq!(approach(0.0, 1.0, 0.25), 0.25);
        assert_eq!(approach(0.9, 1.0, 0.25), 1.0);
        assert_eq!(approach(2.0, 1.0, 0.25), 1.75);
        assert_eq!(approach(1.1, 1.0, 0.25), 1.0);
        assert_eq!(approach(1.0, 1.0, 0.25), 1.0);
    }
}
