//! Critters on the ground: a colour that moves, and nothing else.
//!
//! ## Why a gameplay rule is not being smuggled in here
//!
//! The rule this file inherits is `birds.rs`'s, word for word, and it is the reason both
//! modules are allowed to exist on a client at all: a *gameplay* rule may not live only here,
//! and a creature that cannot be hit, targeted, eaten, counted or seen by the server is not a
//! gameplay rule, it is a colour that moves. Nothing about a critter is sent, nothing reads a
//! rule back out of one, and the species is chosen from [`Ambience`] alone — never from the
//! snapshot's weather, never from anything the server said. The day somebody wants to shoot a
//! squirrel, the squirrel becomes a `MobKind` on the server and this module's row for it is
//! deleted, in that order.
//!
//! `player/tests.rs` pins the negative half of that for a critter exactly as it does for a
//! bird: no component from `mobs.rs`, `hands.rs`, `drops.rs` or `structures.rs`, a snapshot
//! with no mobs leaves the ground alone, and a mining intent aimed straight along a squirrel
//! produces the same bytes it would with the wood empty.
//!
//! ## What this shares with the flock, and what is genuinely new
//!
//! Shared, and deliberately a second small copy rather than a generalisation over both: the
//! eye quantised to an anchor so a position needs no stored state, a fade in and out, a
//! replacement seeded on the far side of a move, a population bound that counts the fading,
//! a species table gated on [`Ambience`], and a model lofted in code because this client
//! ships no art asset. Those are four hundred lines of *argument* that transfer and about
//! sixty lines of code that does; a trait over "ambient cosmetic creature" would have to
//! abstract the one thing the two do not share, which is where a creature is.
//!
//! **Three things are new, and they are the whole of this module's interest.**
//!
//! - **The ground is the path rather than an obstacle.** A bird is *lifted* off the terrain
//!   by [`birds`]'s clearance; a critter is *placed on* it. That is the same column probe
//!   with the opposite sign and a far tighter tolerance — [`surface_under`] and
//!   [`next_stand`] — and it means [`place`] cannot answer a critter's height at all. It
//!   answers where a critter is on the **horizontal plane**, and the terrain answers the
//!   rest. Every box invariant here is therefore horizontal, which is the one structural
//!   difference from `birds.rs` a reader has to hold on to.
//! - **A critter has a life, and the clock is what ends it.** A bird's pattern is a loop:
//!   it flies forever and is only ever retired by the anchor moving. A squirrel that climbs
//!   a trunk and disappears into the leaves has a beginning and an end by definition, and
//!   [`generation_of`] is how that is expressed without storing a birth time: the life
//!   window is a *function of the session clock*, so the critter alive in one slot at one
//!   moment is decided by arithmetic rather than remembered.
//! - **The climb needs a trunk, and a trunk is a fact about the world.** [`trunk_near`]
//!   looks for one when a critter is stood up, and that answer is written into the component
//!   once and never again — the same category of spawn-time constant as [`Critter::anchor`].
//!   A critter that finds none simply fades where it is, and that is a real branch rather
//!   than an unreachable one: `a_critter_with_no_trunk_forages_and_fades_where_it_is`
//!   asserts it.
//!
//! ## Built so that the second species is a row
//!
//! This module is a foundation before it is a squirrel, and the shape that makes it one is
//! the same shape `birds.rs` has: everything about a kind of critter is one row of
//! [`CRITTERS`], the systems are written against the table's length, and the motion families
//! are an enum ([`Gait`]) rather than a branch per species. A mouse is a row with a smaller
//! size and a shorter life; a creature that watches rather than forages is a second [`Gait`]
//! and a row that names it. Nothing below reads `CRITTERS[0]` by its index outside the
//! table's own tests.
//!
//! ## Two entities and no asset
//!
//! A body lofted through nine cross-sections, and a tail as a child — one mesh each, so a
//! critter is two draws and a full wood is eight. The tail is a child for the reason a bird's
//! wing is: it turns about its own root, which is cheaper to write and to read than
//! recomputing its vertices, and `a_critter_is_two_draws_however_detailed_it_is` is what keeps
//! a richer model from becoming a richer scene.
//!
//! ## How this issue was landed, since the parts are still visible in its history
//!
//! #1190 measured about 170,000 characters whole against a 90,000 review cap, so it arrived in
//! four pull requests, each answering one question and each a boundary the code already draws:
//! where a critter is (the pure path and the seeds), what it stands on (the terrain probe and
//! the trunk search), **what draws it** — this part, which is also what removes the module's
//! temporary dead-code allowance by giving every item above a caller — and what it sounds like.

use std::f32::consts::{FRAC_1_SQRT_2, TAU};
use std::ops::RangeInclusive;

use bevy::asset::RenderAssetUsages;
use bevy::ecs::system::SystemParam;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use super::ambience::{Ambience, GroundLook};
use super::camera::WorldCamera;
use super::sky::{self, Period, SkyClock};
use crate::net::{BlockCoord, ChunkCoord, Session};
use crate::world::{ChunkStore, palette};

/// How coarsely the eye is quantised before it anchors the critters in a cell, in blocks.
///
/// The same thirty-two blocks `birds::BIRD_ANCHOR_CELL` uses, and for the same reason: the
/// anchor is what makes a critter's path a pure function of the clock, because it holds still
/// while the player walks a cell's width. It is deliberately not shared as one constant — the
/// quantisation that suits a flock at forty blocks up is a coincidence rather than a claim,
/// and the day a critter wants a finer cell the two must be free to differ.
pub(super) const CRITTER_ANCHOR_CELL: f32 = 32.0;

/// How far from its anchor a critter may be **on the horizontal axes**, in blocks.
///
/// Horizontal and nothing else, which is the difference from `birds::BIRD_RANGE` worth
/// reading twice: a bird's box bounds all three axes because a bird's height is measured from
/// the anchor, and a critter's height is measured from the *terrain*. A wood on a mountainside
/// puts a squirrel thirty blocks below the eye's cell centre while it is still four blocks
/// away, and a vertical bound would retire it for standing on the ground it is supposed to
/// stand on.
///
/// Forty rather than the flock's sixty-four, because a critter is worth drawing only where it
/// can be seen as a shape: [`HOME_SPREAD`] plus the whole of a forage's travel and jitter is
/// twenty-seven, and `a_critter_never_leaves_its_horizontal_box` pins the sum.
pub(super) const CRITTER_RANGE: f32 = 40.0;

/// The most critters that may exist at once, fading ones included.
pub(super) const CRITTER_COUNT_MAX: usize = 4;

/// How long a critter takes to fade in, and to fade out before it is despawned.
///
/// **A second and a quarter, where a bird gets three, and the difference is an angle rather
/// than a preference.** `birds::BIRD_FADE_SECONDS` is three seconds because a bird subtending
/// about a degree is a bright dot whatever its alpha is doing, so a shorter fade reads as a
/// twinkle. A critter is metres away and subtends several degrees: it is unmistakably a shape
/// from the first frame, and three seconds of a translucent squirrel sitting in the grass
/// would read as a ghost rather than as an arrival.
///
/// It is also what the end of a climb is measured back from — see [`CritterSpecies::life`] —
/// so a squirrel is at the top of its trunk while this is running out, which is what makes
/// the disappearance a disappearance *into the leaves*.
pub(super) const CRITTER_FADE_SECONDS: f32 = 1.25;

/// The one constant every critter seed is mixed from.
///
/// **Never `world_seed`, and never an entity id**, for the reason `birds::BIRD_SEED` gives:
/// two players in the same wood see different squirrels on purpose, because a critter nobody
/// shares is a critter nobody can arrange to meet — the cheapest available proof that none of
/// this is state.
const CRITTER_SEED: u64 = 0x5C01_7715_C0DE_1A75;

/// How many re-seeds are tried before a replacement is accepted wherever it fell.
///
/// The flock's [`birds`] reasoning applies unchanged: each try is one hash, about half land on
/// the far side, and the fallback is a real branch that
/// `a_replacement_is_seeded_on_the_far_side_of_the_move` asserts.
const FAR_SIDE_TRIES: u64 = 12;

/// How far a critter's home sits from the anchor, in blocks, on the horizontal axes.
///
/// Plus the widest forage — [`FORAGE_TRAVEL`] times a life's forage stretch, plus
/// [`SCURRY_SPREAD`] — this is under [`CRITTER_RANGE`].
const HOME_SPREAD: f32 = 16.0;

/// The corner of a scurry leg's waypoint box, in blocks, and how long one leg lasts.
///
/// Horizontal only: a critter's height is the ground's business, so a waypoint that moved it
/// vertically would be a second opinion about the terrain.
const SCURRY_SPREAD: f32 = 1.5;
const SCURRY_LEG_SECONDS: RangeInclusive<f32> = 1.8..=3.0;

/// How much of a scurry leg is spent moving, the rest of it spent still.
///
/// **The whole of what makes it a scurry rather than a walk.** A squirrel crossing open
/// ground dashes and then freezes — it is the freeze that reads as a squirrel, and a leg
/// interpolated smoothly from end to end for its whole duration reads as a clockwork mouse.
/// Six tenths moving, four tenths held: at the shortest leg that is a 1.08-second dash and
/// three quarters of a second sitting still.
///
/// **The peak speed is one and a half times the average, and forgetting that is how the first
/// version of this file failed its own test.** [`smooth`] is a smoothstep, whose derivative
/// peaks at 1.5 in the middle of the interval — so a dash covering `d` blocks in `t` seconds
/// touches `1.5 * d / t`, not `d / t`. The longest dash is the diagonal of the waypoint box,
/// `2 * SCURRY_SPREAD * sqrt(2)` = 4.24 blocks, over `0.6 * 1.8` = 1.08 seconds: 5.89 blocks a
/// second at the peak, plus [`FORAGE_TRAVEL`] where the two happen to align. That is what
/// [`CritterSpecies::max_speed`] is 6.5 for, and
/// `a_critter_moves_no_faster_than_its_row_allows` is what measures it rather than trusting
/// this paragraph.
const SCURRY_DASH_SHARE: f32 = 0.6;

/// How fast a foraging critter's centre travels, in blocks per second, and over how long.
///
/// Without it a squirrel jitters about one point forever, which reads as a tethered animal.
/// Half a block a second is a slow browse: over the forage stretch of a twenty-second life it
/// covers about seven blocks, which is a squirrel working its way across a clearing.
const FORAGE_TRAVEL: f32 = 0.5;

/// How far ahead a critter is sampled to find which way it is facing, in seconds.
///
/// Shorter than the flock's, because a scurry changes direction far more sharply than a glide
/// and a heading sampled a twentieth of a second out would point at where the dash *ends*
/// rather than along it.
const HEADING_STEP: f32 = 0.02;

/// How far down a critter's column is probed for the ground it stands on, in blocks.
///
/// Measured from the anchor's own height, so it is a window around the eye rather than around
/// the critter: twenty-four blocks below and eight above covers a wooded hillside without
/// letting a critter stand on a cave floor under the player's feet. It is the one number here
/// that bounds the probe's cost — thirty-two lookups per critter per frame, a hundred and
/// twenty-eight at [`CRITTER_COUNT_MAX`], beside the eight the flock already takes.
const STAND_PROBE_BELOW: f32 = 24.0;
const STAND_PROBE_ABOVE: f32 = 8.0;

/// How fast a critter's feet may follow a change in the ground under them, in blocks/second.
///
/// Eased rather than assigned, for the reason the server's `approach` gives in
/// `internal/game/player.go` and `birds::CLEARANCE_LIFT_SPEED` repeats: a value that snaps to
/// its target reads as a wall of velocity, and a squirrel that jumps a block the instant it
/// crosses a voxel edge is exactly that.
///
/// **Twenty-four blocks a second, which is three times the fastest dash, and the factor is
/// the point.** While the ground under a scurrying critter changes more slowly than this, the
/// ease reaches it and sits on it exactly — so "a critter stands on the surface" is an
/// equality on rolling ground rather than a tolerance, and
/// `a_critter_stands_exactly_on_the_ground_once_it_has_settled` asserts it as one. A vertical
/// step of a single voxel is crossed in forty milliseconds, which is a hop rather than a
/// teleport.
const STAND_STEP_SPEED: f32 = 24.0;

/// How much of a critter's climb is spent reaching the trunk before any of it is spent rising.
const CLIMB_APPROACH_SHARE: f32 = 0.35;

/// How far a critter may be from its trunk when the climb begins, in blocks.
///
/// **The approach stretch has to cover it without outrunning the gait, and that is what sets
/// the number.** [`CLIMB_APPROACH_SHARE`] of the squirrel's climb stretch is 1.96 seconds, and
/// the approach is smoothed — so its peak speed is `1.5 * TRUNK_REACH / 1.96`, the same factor
/// of one and a half [`SCURRY_DASH_SHARE`] explains. Seven blocks is 5.36 a second, under the
/// 6.5 the row allows; twelve, which this was first written as, is 9.2 and breaks the bound.
///
/// **It is measured from where the forage *ends*, not from where it begins**, and that is not
/// a detail either. `keep_the_critters` probes for the trunk at
/// `place(.., forage_seconds, ..)`, because a critter browses several blocks across the ground
/// during its life: a trunk within seven blocks of the *start* can be fourteen from the finish,
/// and the approach would then have to sprint to reach it.
const TRUNK_REACH: f32 = 7.0;

/// How high a critter climbs before it is gone, in blocks above the surface.
///
/// The canopy `ambience.rs` reads to call a column wooded starts a few blocks up and runs to
/// about a dozen, so nine blocks puts a squirrel among the leaves rather than above them —
/// which is what the fade has to coincide with for the disappearance to read as one.
const CLIMB_RISE: f32 = 9.0;

/// How a critter moves over the ground.
///
/// An enum rather than a branch per species, so the second and third critters are a row that
/// names a gait rather than a copy of this module. Only one is written, because an
/// unconstructed variant would be a claim about content nobody has authored — the same
/// reasoning `sky::Period` gives for having no `Always`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Gait {
    /// Short dashes between waypoints with a held stillness between them, over a centre that
    /// browses slowly across the ground.
    Scurry,
}

/// One row of [`CRITTERS`]: everything about a kind of critter there is.
#[derive(Debug)]
pub(super) struct CritterSpecies {
    /// The [`GroundLook`] this row lives on. No row lives on [`GroundLook::Unknown`].
    pub(super) ground: GroundLook,
    /// Whether the look also has to be wooded.
    ///
    /// **The squirrel shares the macaw's gate deliberately**, and it is the same field on the
    /// same answer rather than a second opinion: `birds::BirdSpecies::requires_wooded` is
    /// what makes an open plain parrotless, and a wood in the plains is exactly where a
    /// squirrel belongs. The two tables agree because both read `Ambience::wooded`, and
    /// `a_squirrel_lives_where_the_macaw_flies` pins the agreement rather than a comment
    /// claiming it.
    pub(super) requires_wooded: bool,
    /// The half of the day this row is abroad, as [`Period`] — the same enum the bird table
    /// and the wildlife table declare, so a nocturnal critter and its call read one
    /// definition of "after dark".
    pub(super) abroad: Period,
    /// How many of them are about at once in one anchor cell.
    pub(super) count: RangeInclusive<u8>,
    /// The body length, nose to rump, in blocks. The whole critter is drawn at this scale.
    ///
    /// Literally the body length: the model is authored exactly one long from nose to rump,
    /// with the tail reaching further back, so this is a scale and a length at once and
    /// nothing divides it back out.
    /// `the_model_is_authored_at_a_body_length_of_exactly_one` keeps that true.
    pub(super) size: f32,
    /// How long one critter of this row is about, in seconds, from fading in to gone.
    ///
    /// **The life is what a bird does not have**, and [`generation_of`] is why it needs no
    /// birth time: the window is a function of the session clock, so the critter alive in a
    /// slot right now is arithmetic rather than memory.
    pub(super) life: f32,
    /// What share of a life is spent foraging before the climb begins.
    ///
    /// The rest is the climb, and the last [`CRITTER_FADE_SECONDS`] of *that* are the fade —
    /// so the row has to leave the climb long enough to reach the leaves before it starts
    /// disappearing. `a_squirrel_is_high_in_its_trunk_by_the_time_it_starts_to_fade` is what
    /// holds the three numbers to each other.
    pub(super) forage_share: f32,
    /// Whether this row ends its life by climbing, or simply by fading where it stands.
    ///
    /// A squirrel climbs. A mouse will not: it goes into a hole, which from the outside is
    /// this row with `climbs: false` and nothing else changed.
    pub(super) climbs: bool,
    /// How often the tail completes one flick, in hertz.
    pub(super) flick_hz: f32,
    /// The row's own body and tail colours, and the first pair [`CritterSpecies::coat_at`]
    /// chooses from.
    pub(super) body: Color,
    pub(super) tail: Color,
    /// The *other* pairs a critter of this row may wear, chosen by its own seed. Empty for a
    /// species with one coat, and no pair is written twice.
    pub(super) coats: &'static [(Color, Color)],
    /// How it moves.
    pub(super) gait: Gait,
    /// The fastest this row's gait can move it, in blocks per second.
    ///
    /// A bound rather than a speed, and **test-only** for the reason
    /// `birds::BirdSpecies::max_speed` is: nothing reads it to move a critter — [`place`] and
    /// the ground are the whole of where one is — and
    /// `a_critter_moves_no_faster_than_its_row_allows` is its only consumer. It is here so
    /// the continuity of the path is a number a test can fail on rather than a claim in a
    /// comment.
    #[cfg(test)]
    pub(super) max_speed: f32,
}

impl CritterSpecies {
    /// Which coat one critter of this row wears, as an index rather than a pair of colours.
    ///
    /// An index because the materials are built once and held in that order: a spawning
    /// critter needs the *slot*, so nothing has to look a `Color` up in a table to draw one.
    fn coat_of(&self, seed: u64) -> usize {
        mix(seed, SALT_COAT) as usize % (self.coats.len() + 1)
    }

    /// The `(body, tail)` pair at one coat index: zero is the row's own pair and the rest come
    /// from [`CritterSpecies::coats`], so a row with no variants has one answer.
    fn coat_at(&self, choice: usize) -> (Color, Color) {
        match choice.checked_sub(1) {
            None => (self.body, self.tail),
            Some(variant) => self.coats[variant],
        }
    }

    /// How long this row's forage stretch is, in seconds.
    fn forage_seconds(&self) -> f32 {
        self.life * self.forage_share
    }

    /// How long this row's climb stretch is, in seconds — the rest of a life.
    fn climb_seconds(&self) -> f32 {
        self.life - self.forage_seconds()
    }

    /// Whether this row is the one [`Ambience`] is asking for.
    fn matches(&self, ambience: &Ambience) -> bool {
        self.ground == ambience.ground && (!self.requires_wooded || ambience.wooded)
    }
}

/// Every kind of critter there is. Appended to, never reordered: [`Critter::species`] is an
/// index into this table and a critter alive across a reorder would change species on the
/// ground.
///
/// ## How the size was chosen: an angle again, and a different range of them
///
/// `birds::BIRDS` argues its wingspans from the angle a bird subtends at the *top of its own
/// altitude band*, because the top of the band is the only distance a bird is ever seen from.
/// A critter has no band: the player walks up to it, so the distance runs from arm's length
/// to the far side of the box, and the angle with it.
///
/// | distance | 0.55-block squirrel subtends |
/// |---|---|
/// | 3 blocks | 10.5° |
/// | 8 blocks | 3.9° |
/// | 20 blocks | 1.6° |
/// | 27 blocks (the forage's reach) | 1.2° |
///
/// Which is read the other way round from the flock's table: the constraint is not that it
/// must be *large* enough at the top of a band, it is that it must not be **absurd** at the
/// bottom of the range while still reading as a shape in the middle of it. Eight blocks is
/// where a squirrel is interesting and 3.9° there is squarely inside the two-and-a-half to
/// four-and-a-half degrees the flock's rows land in. At arm's length it is a squirrel rather
/// than a dog, and at the far edge of the box it is a dot — which is correct, and is what the
/// fade in and out exists to keep from popping.
///
/// Zero point five five blocks is also a little over life size: a red squirrel's body is about
/// 0.22 m. The flock's eagle is stretched much further than that for the same reason, and it
/// is named here rather than left to be re-derived.
pub(super) const CRITTERS: [CritterSpecies; 1] = [
    // The squirrel: wooded green country by day, on the same gate the macaw uses. It forages
    // across the ground, climbs a trunk, and is gone into the leaves.
    CritterSpecies {
        ground: GroundLook::Grass,
        requires_wooded: true,
        abroad: Period::Day,
        count: 1..=3,
        size: 0.55,
        life: 20.0,
        forage_share: 0.72,
        climbs: true,
        // A flick or two a second: the idle twitch of a tail held over the back, not a wag.
        flick_hz: 1.4,
        body: Color::srgb(0.55, 0.29, 0.13),
        tail: Color::srgb(0.62, 0.36, 0.18),
        coats: &[
            // The grey squirrel, and a darker red one.
            (Color::srgb(0.46, 0.46, 0.48), Color::srgb(0.56, 0.56, 0.58)),
            (Color::srgb(0.42, 0.19, 0.09), Color::srgb(0.50, 0.26, 0.12)),
        ],
        gait: Gait::Scurry,
        // The longest dash is the waypoint box's diagonal, 4.24 blocks, over
        // `SCURRY_DASH_SHARE` of the shortest leg, 1.08 s — times the 1.5 a smoothstep peaks
        // at, which is 5.89 — plus `FORAGE_TRAVEL` where the two align. The climb's approach
        // is slower by construction; see `TRUNK_REACH`, which is derived from this bound
        // rather than the other way round.
        #[cfg(test)]
        max_speed: 6.5,
    },
];

/// Which row [`Ambience`] is asking for, if any.
///
/// [`GroundLook::Unknown`] and grass without trees both answer `None`: "not enough loaded
/// evidence" and "an open plain" come out as empty ground rather than as a default critter —
/// the same direction `birds::species_for` takes, and the same direction the wildlife table
/// takes for a voice.
pub(super) fn species_for(ambience: &Ambience) -> Option<usize> {
    CRITTERS.iter().position(|row| row.matches(ambience))
}

// ---------------------------------------------------------------------------
// Where a critter is, on the plane
// ---------------------------------------------------------------------------

/// The anchor cell the eye is in.
fn cell_of(eye: Vec3) -> IVec3 {
    (eye / CRITTER_ANCHOR_CELL).floor().as_ivec3()
}

/// The point a cell's critters are anchored to: its centre.
fn anchor_of(cell: IVec3) -> Vec3 {
    (cell.as_vec3() + Vec3::splat(0.5)) * CRITTER_ANCHOR_CELL
}

/// The point a critter's forage is drawn around. Horizontal: the `y` is the anchor's, and is
/// a placeholder the ground replaces.
fn home_of(seed: u64, anchor: Vec3) -> Vec3 {
    anchor
        + Vec3::new(
            centred(seed, SALT_HOME_X) * HOME_SPREAD,
            0.0,
            centred(seed, SALT_HOME_Z) * HOME_SPREAD,
        )
}

/// Which life window a slot is in, and how far into it, `elapsed` seconds into the session.
///
/// **This is what a bird does not need and a critter cannot do without.** A flock's pattern
/// is a loop, so a bird has no end and needs no beginning; a squirrel that climbs a trunk and
/// vanishes has both. Storing a birth time would work and is what this deliberately avoids:
/// the window is `floor((elapsed + stagger) / life)`, so the generation and the age are
/// *arithmetic on the session clock* and a critter re-derives its own position after a
/// thousand frames nobody drew it in.
///
/// The stagger is the slot's share of a life, so the critters in one cell do not all vanish
/// and re-appear on the same frame — which is what a shared window would do, and which reads
/// as a scene being swapped rather than as animals coming and going.
fn generation_of(species: &CritterSpecies, slot: usize, elapsed: f32) -> (i64, f32) {
    let stagger = species.life * slot as f32 / CRITTER_COUNT_MAX as f32;
    let since = elapsed + stagger;
    // `as i64` saturates rather than wrapping to nonsense on a clock nobody will run that
    // long anyway, which is the reasoning `birds::offset` gives for the same cast.
    let generation = (since / species.life).floor();
    (generation as i64, since - generation * species.life)
}

/// Where one critter is on the horizontal plane, `age` seconds into its life.
///
/// Pure: the same five arguments give the same point forever, so there is no per-critter
/// position state to advance, nothing to keep in step between frames, and the whole of the
/// path is testable without a window.
///
/// **Four of the five are fixed when the critter is stood up** — the row, the seed, the
/// anchor and the trunk — and `age` is the clock. That is the flock's claim with one more
/// constant in it, and the extra constant is the trunk, because where a tree is cannot be
/// derived from a hash: [`trunk_near`] reads it out of the world once. A critter whose trunk
/// is `None` forages for its whole life and never rises, which is the documented fallback.
///
/// **`trunk` must be within [`TRUNK_REACH`] of where this critter's forage ends**, which is
/// what [`trunk_near`] answers within and what `keep_the_critters` searches from. It is a
/// precondition rather than a clamp because the approach's speed is derived from it: a trunk
/// twice as far is an approach that walks twice as fast, and
/// `a_critter_moves_no_faster_than_its_row_allows` would rather fail than have this function
/// quietly cover for a caller that broke the chain.
///
/// **The `y` it answers is the anchor's, and means nothing.** A critter's height is the
/// ground's answer plus its climb, which [`next_stand`] and [`climb_rise`] own; everything
/// here is `x` and `z`. Continuity is the property that matters and it is pinned —
/// `a_critter_moves_no_faster_than_its_row_allows` walks every row's whole life at sixty
/// samples a second and fails on a step longer than `max_speed * dt`.
pub(super) fn place(
    species: &CritterSpecies,
    seed: u64,
    age: f32,
    anchor: Vec3,
    trunk: Option<Vec3>,
) -> Vec3 {
    let forage = forage_at(species, seed, age.min(species.forage_seconds()), anchor);
    let Some(trunk) = trunk.filter(|_| species.climbs) else {
        // No tree, or a row that does not climb: the forage is the whole of the life, and its
        // last position is held while the fade finishes.
        return forage;
    };
    let climbing = (age - species.forage_seconds()).max(0.0);
    if climbing == 0.0 {
        return forage;
    }
    // The approach: from where the forage ended to the foot of the trunk, over the first
    // share of the climb. Past that it is the trunk's own column, which is what makes the
    // rise vertical.
    let approach = (climbing / (species.climb_seconds() * CLIMB_APPROACH_SHARE)).min(1.0);
    let foot = Vec3::new(trunk.x, forage.y, trunk.z);
    forage.lerp(foot, smooth(approach))
}

/// Where a foraging critter is, `age` seconds in: a browsing centre with a scurry over it.
fn forage_at(species: &CritterSpecies, seed: u64, age: f32, anchor: Vec3) -> Vec3 {
    let home = home_of(seed, anchor);
    // The browse: one direction per critter, held for the whole forage, so a squirrel works
    // its way across a clearing rather than jittering about a tether.
    let bearing = unit(seed, SALT_BEARING) * TAU;
    let travelled = FORAGE_TRAVEL * age;
    let centre = home + Vec3::new(bearing.cos(), 0.0, bearing.sin()) * travelled;
    centre + scurry(species, seed, age)
}

/// How far one critter is from its browsing centre, `age` seconds in.
fn scurry(species: &CritterSpecies, seed: u64, age: f32) -> Vec3 {
    match species.gait {
        Gait::Scurry => {
            let leg = lerp(
                *SCURRY_LEG_SECONDS.start(),
                *SCURRY_LEG_SECONDS.end(),
                unit(seed, SALT_LEG),
            );
            let progress = age / leg;
            let index = progress.floor();
            // The waypoint index is the leg number, so consecutive legs share an end point
            // and the path is continuous across every boundary — `birds::offset`'s dart, on
            // the plane and with a dash rather than a glide between the two.
            let from = waypoint(seed, index as i64);
            let to = waypoint(seed, index as i64 + 1);
            let along = ((progress - index) / SCURRY_DASH_SHARE).min(1.0);
            from.lerp(to, smooth(along))
        }
    }
}

/// The `index`th waypoint of a scurrying critter, relative to its browsing centre.
fn waypoint(seed: u64, index: i64) -> Vec3 {
    let leg = mix(seed, index as u64 ^ SALT_WAYPOINT);
    Vec3::new(
        centred(leg, 0) * SCURRY_SPREAD,
        0.0,
        centred(leg, 1) * SCURRY_SPREAD,
    )
}

/// How far up its trunk a critter has climbed, `age` seconds into its life.
///
/// Zero for the whole forage and for the approach, then a rise to [`CLIMB_RISE`]. Smoothed at
/// both ends so a squirrel does not start and stop climbing between two frames — the one place
/// in this module where an ease is about how a thing *looks* rather than about not teleporting.
///
/// **The rise finishes exactly where the fade begins, and that is the whole design of the
/// ending.** A squirrel reaches the leaves and *then* disappears into them, over the last
/// [`CRITTER_FADE_SECONDS`] of its life. A rise that ran to the end of the life instead — which
/// is how this was first written — leaves it still climbing while it is already half
/// transparent, and that reads as dissolving in mid-air rather than as going into a tree.
/// `a_squirrel_is_whole_and_high_in_its_trunk_when_it_starts_to_fade` asserts it as an
/// *equality* rather than as a threshold, precisely because the two moments are defined to be
/// the same moment.
fn climb_rise(species: &CritterSpecies, age: f32, trunk: Option<Vec3>) -> f32 {
    if !species.climbs || trunk.is_none() {
        return 0.0;
    }
    let starts = species.forage_seconds() + species.climb_seconds() * CLIMB_APPROACH_SHARE;
    let ends = species.life - CRITTER_FADE_SECONDS;
    if age <= starts || ends <= starts {
        return 0.0;
    }
    CLIMB_RISE * smooth((age - starts) / (ends - starts))
}

/// A smoothstep: zero slope at both ends, so nothing starts or stops between two frames.
fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

// ---------------------------------------------------------------------------
// The ground a critter stands on
// ---------------------------------------------------------------------------
//
// [`place`] answers where a critter is on the plane, from five arguments and nothing else,
// and nothing below changes that. The ground is a second, named step over its answer, applied
// in `run_the_critters` where the terrain and the previous frame's height both already are —
// so the path stays a pure function and stays testable without a window, and the whole of
// what the ground does to a critter is one number.
//
// This is `birds::surface_under` and `birds::next_lift` with the opposite sign: the flock is
// *lifted off* the surface by a clearance and a critter is *placed on* it. The three-answer
// shape is the same, and the reasoning for it is the one `birds::GroundUnder` gives at length.

/// One float floored to the voxel index containing it.
///
/// `floor`, never a bare cast, for the reason `player/target.rs`'s raycast gives: `-0.5 as
/// i32` truncates to 0 and the voxel containing -0.5 is -1. Half the world is on that side of
/// the origin.
fn voxel_of(value: f32) -> i32 {
    Vec3::splat(value).floor().as_ivec3().x
}

/// What the probe found in a critter's column: three answers, not two.
///
/// The separation is `birds::GroundUnder`'s and is load-bearing for the same reason, with one
/// difference in what the middle answer *means*. For a bird, an empty window is a measurement
/// that the clearance is already met. For a critter it is a measurement that there is **no
/// ground here at all** within a window centred on the eye's own height — a chasm, or a column
/// the player is flying over — and a ground creature with no ground under it has nowhere to
/// be, so it is retired rather than held.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Ground {
    /// The top face of the first thing found in the window.
    Surface(f32),
    /// Nothing in the window, and every chunk it crosses was there to be read.
    Empty,
    /// A chunk the window crosses is not loaded, so there is no answer at all.
    Unknown,
}

/// The top face of the ground in one column, looked for in a window around `from`.
///
/// **An absent chunk is not evidence of a floor** — the conservative direction `Terrain.Fluid`
/// takes, and the mesher's neighbour rule, and the server's step-up probe — and it is not
/// evidence of a chasm either, which is why it answers [`Ground::Unknown`] rather than
/// [`Ground::Empty`]. The store is *read* here and never asked to fetch: [`ChunkStore::get`]
/// answering `None` ends the probe.
///
/// **`solid_at` rather than "not air", which is the opposite of the choice the flock makes,
/// and the reason is the same question asked about a different body.** A bird's clearance asks
/// what it would be *seen to fly into*, so a lake's surface and a leaf canopy both count. A
/// critter's ground asks what would *hold it up*, which is exactly what solidity means here —
/// and since #446 and #550 it deliberately excludes water and cover. A squirrel standing on a
/// lake surface or halfway up a leaf is the failure this choice avoids, and
/// `a_critter_does_not_stand_on_water_or_on_leaves` pins both.
///
/// **The window is `[from - STAND_PROBE_BELOW, from + STAND_PROBE_ABOVE]`**, thirty-three
/// voxels, walked downward from the top so the answer is the highest ground rather than the
/// first one found. It hangs off the **anchor's** height rather than off the critter's own,
/// deliberately: a critter's height is what is being computed, so using it would be circular,
/// and the anchor is the eye's cell centre — which is to say the window is "the ground near
/// the player", which is the only ground worth standing a cosmetic squirrel on.
fn surface_under(store: &ChunkStore, column: Vec3, from: f32, chunk_size: usize) -> Ground {
    // An argument nothing can be measured from is an absence, not an empty window.
    if !column.is_finite() || !from.is_finite() || chunk_size == 0 {
        return Ground::Unknown;
    }
    let Ok(size) = i32::try_from(chunk_size) else {
        return Ground::Unknown;
    };
    let x = voxel_of(column.x);
    let z = voxel_of(column.z);
    let high = voxel_of(from + STAND_PROBE_ABOVE);
    let low = voxel_of(from - STAND_PROBE_BELOW);

    for y in (low..=high).rev() {
        let coord = ChunkCoord {
            cx: x.div_euclid(size),
            cy: y.div_euclid(size),
            cz: z.div_euclid(size),
        };
        // Downwards, and a gap ends the probe rather than being read through: a voxel this
        // session does not hold could be higher than anything found under it, and standing a
        // critter on the floor of a hole it cannot see the lid of is worse than not drawing
        // it.
        if store.get(coord).is_none() {
            return Ground::Unknown;
        }
        if store.solid_at(BlockCoord { x, y, z }, chunk_size) {
            // The voxel spans `[y, y + 1)`, so its top face is what a critter stands on.
            return Ground::Surface((y + 1) as f32);
        }
    }
    Ground::Empty
}

/// Moves `current` toward `target` by at most `step`, without overshooting.
///
/// The client's mirror of the server's `approach` in `internal/game/player.go`, and here for
/// the reason given there and in `birds::approach`: a signed max/min pair rather than an
/// exponential ease, because there is no time constant to tune. Once the target moves slower
/// than `step` this sits on it exactly rather than trailing it — which is what lets a settled
/// critter stand on the ground to the bit while it scurries over it.
fn approach(current: f32, target: f32, step: f32) -> f32 {
    if current > target {
        (current - step).max(target)
    } else {
        (current + step).min(target)
    }
}

/// This frame's answer for one critter's feet: the height it stands at, or `None` if it has
/// nowhere to stand.
///
/// **The three answers [`Ground`] gives are three different outcomes, and only one of them is
/// a target.** A [`Ground::Surface`] is eased toward at [`STAND_STEP_SPEED`].
/// [`Ground::Unknown`] asks for **this frame's height back**: nothing was measured, so nothing
/// moves, and the critter holds where the last frame that could read the ground put it — the
/// direction `birds::next_lift` takes, and for the same reason. [`Ground::Empty`] answers
/// `None`, which is not a height at all: the caller retires the critter, because a ground
/// creature over a chasm has no correct position and fading it out is the only honest answer.
///
/// `ground` is `None` for a frame with no session or no store, and that is the same absence as
/// an unloaded chunk: the height is held.
fn next_stand(
    ground: Option<(&ChunkStore, usize)>,
    column: Vec3,
    from: f32,
    stand: f32,
    dt: f32,
) -> Option<f32> {
    let under = ground.map_or(Ground::Unknown, |(store, chunk_size)| {
        surface_under(store, column, from, chunk_size)
    });
    match under {
        Ground::Surface(surface) => Some(approach(stand, surface, STAND_STEP_SPEED * dt)),
        Ground::Unknown => Some(stand),
        Ground::Empty => None,
    }
}

/// The foot of a trunk a critter may climb, within [`TRUNK_REACH`] of `from`.
///
/// **A ring search rather than a scan, because the cost has to be bounded and it is paid
/// once.** This runs when a critter is stood up and never again — the answer goes into
/// [`Critter::trunk`] — so it may read more columns than a per-frame probe could afford, and
/// it reads them in rings outward so the trunk it finds is a near one rather than the first in
/// a raster.
///
/// `palette::LOG` and nothing else: it is the same block `ambience.rs` reads to decide a
/// column is wooded, which is what makes "a squirrel appears where the wood is" and "a
/// squirrel finds a trunk" the same fact rather than two that can disagree.
///
/// **`None` is a real answer and the caller must handle it**: a wooded look is a vote over
/// sixty-four columns, so a critter can perfectly well be stood up a dozen blocks from the
/// nearest actual trunk. It then forages for its whole life and fades where it is.
fn trunk_near(store: &ChunkStore, from: Vec3, surface: f32, chunk_size: usize) -> Option<Vec3> {
    // `> 0` and not merely convertible: `div_euclid(0)` panics, and a zero chunk size reaches
    // here from a session whose welcome has not landed. `surface_under` fails closed on the
    // same argument for the same reason.
    let size = i32::try_from(chunk_size).ok().filter(|size| *size > 0)?;
    if !from.is_finite() || !surface.is_finite() {
        return None;
    }
    let reach = TRUNK_REACH as i32;
    let base = IVec3::new(voxel_of(from.x), voxel_of(surface), voxel_of(from.z));
    // Rings outward from the critter's own column, so the nearest trunk wins.
    for ring in 0..=reach {
        for dx in -ring..=ring {
            for dz in -ring..=ring {
                if dx.abs().max(dz.abs()) != ring {
                    continue;
                }
                let column = IVec3::new(base.x + dx, base.y, base.z + dz);
                // **The rings are square and the reach is a circle**, so a corner of the
                // outermost ring is rejected: Chebyshev 7 is Euclidean 9.9, and `place`'s
                // approach derives its speed from this distance, so letting a corner through
                // would break a bound two functions away. Walking rings and rejecting corners
                // is cheaper than ordering a disc, and the ring order still means the nearest
                // trunk wins.
                let away = Vec3::new(
                    column.x as f32 + 0.5 - from.x,
                    0.0,
                    column.z as f32 + 0.5 - from.z,
                );
                if away.length() > TRUNK_REACH {
                    continue;
                }
                // A trunk is a log standing in the two voxels above the surface: one log flat
                // on the ground is a fallen branch, and a squirrel does not climb it.
                if (1..=2).all(|up| {
                    let coord = ChunkCoord {
                        cx: column.x.div_euclid(size),
                        cy: (column.y + up).div_euclid(size),
                        cz: column.z.div_euclid(size),
                    };
                    store.get(coord).is_some()
                        && store.block_at(
                            BlockCoord {
                                x: column.x,
                                y: column.y + up,
                                z: column.z,
                            },
                            chunk_size,
                        ) == palette::LOG
                }) {
                    // The centre of the column, so the climb is up the middle of the trunk.
                    return Some(Vec3::new(
                        column.x as f32 + 0.5,
                        surface,
                        column.z as f32 + 0.5,
                    ));
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Seeds
// ---------------------------------------------------------------------------

const SALT_HOME_X: u64 = 1;
const SALT_HOME_Z: u64 = 2;
const SALT_BEARING: u64 = 3;
const SALT_LEG: u64 = 4;
const SALT_COAT: u64 = 5;
const SALT_COUNT: u64 = 6;
const SALT_WAYPOINT: u64 = 0x9E37_79B9_7F4A_7C15;

/// SplitMix64's finalizer: an avalanche, not a generator.
///
/// The same reasoning `birds::splitmix` and `player/precipitation.rs` give. There is no state
/// to carry and no stream to keep in step, so a critter asked where it lives a thousand frames
/// apart is told the same thing both times.
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

/// The seed of the critters in `cell`. Mixed from [`CRITTER_SEED`] and nothing else.
fn cell_seed(cell: IVec3) -> u64 {
    let packed = mix(
        i64::from(cell.x) as u64,
        mix(i64::from(cell.y) as u64, i64::from(cell.z) as u64),
    );
    mix(CRITTER_SEED, packed)
}

/// The seed of one critter: its cell's, its slot, which life window it is, and a re-seed salt.
///
/// The generation is in the seed, which is the whole of how a life ends: the critter in a slot
/// after the window rolls over is a *different* critter with a different home, a different
/// browse and a different coat, and nothing had to remember that the last one existed.
fn critter_seed(cell: u64, slot: usize, generation: i64, salt: u64) -> u64 {
    mix(
        cell,
        mix(
            (slot as u64).wrapping_add(salt.wrapping_mul(64)),
            generation as u64,
        ),
    )
}

/// A seed whose home lies on the far side of a move, so nothing appears in front of the player.
///
/// The bias is the anchor's own displacement. After [`FAR_SIDE_TRIES`] it accepts the first
/// seed rather than looping: a critter that appears behind the player's shoulder is worth less
/// than a frame spent hunting for one. `birds::seed_on_the_far_side`, with a home that has no
/// altitude in it.
fn seed_on_the_far_side(cell: u64, slot: usize, generation: i64, anchor: Vec3, bias: Vec3) -> u64 {
    let first = critter_seed(cell, slot, generation, 0);
    let Some(direction) = Vec3::new(bias.x, 0.0, bias.z).try_normalize() else {
        return first;
    };
    for salt in 0..FAR_SIDE_TRIES {
        let seed = critter_seed(cell, slot, generation, salt);
        let home = home_of(seed, anchor) - anchor;
        if Vec3::new(home.x, 0.0, home.z).dot(direction) > 0.0 {
            return seed;
        }
    }
    first
}

/// How many critters of this row are about in `cell`.
fn group_size(species: &CritterSpecies, cell: u64) -> usize {
    let low = usize::from(*species.count.start());
    let span = usize::from(*species.count.end()) - low + 1;
    (low + mix(cell, SALT_COUNT) as usize % span).min(CRITTER_COUNT_MAX)
}

// ---------------------------------------------------------------------------
// The entities
// ---------------------------------------------------------------------------

/// The two meshes every critter in the session is drawn from, and one material pair per slot.
///
/// The materials are built once, here, rather than at every spawn — the reasoning
/// `birds::BirdVisuals` gives, and it applies harder to a critter: a life is twenty seconds,
/// so a wood stands critters up and retires them continuously rather than only when the eye
/// crosses a cell, and `materials.add` at spawn time would mint a fresh `StandardMaterial`
/// several times a minute forever.
///
/// **Keyed by slot rather than by coat, and the fade is why.** A handle shared by a whole coat
/// cannot carry a per-critter alpha: two grey squirrels, one arriving and one leaving, would
/// fade as one. `keep_the_critters`'s second guard holds the ground to
/// [`CRITTER_COUNT_MAX`], so a pool that size never runs dry and nothing is minted after
/// startup.
#[derive(Resource, Debug)]
pub(super) struct CritterVisuals {
    body: Handle<Mesh>,
    tail: Handle<Mesh>,
    /// One `(body, tail)` pair per critter the ground can hold, claimed at spawn.
    pool: [(Handle<StandardMaterial>, Handle<StandardMaterial>); CRITTER_COUNT_MAX],
}

/// One critter. The root, and the only thing anything outside this module may see.
///
/// It deliberately carries **no** `MobVisuals`, no name plate, no collider, no health and
/// nothing the target raycast or any other system reads.
#[derive(Component, Debug)]
pub(super) struct Critter {
    /// The row of [`CRITTERS`] this critter is, as an index. Never re-read from [`Ambience`]:
    /// a critter whose species changed is one that should have been replaced.
    pub(super) species: usize,
    seed: u64,
    /// Which slot of its cell this critter holds, so a replacement takes the empty one.
    index: usize,
    /// Which life window it is, so a critter is retired when the clock leaves its window
    /// rather than when anything stored says so.
    generation: i64,
    /// The point [`place`] draws its forage around, fixed for this critter's whole life.
    pub(super) anchor: Vec3,
    /// The foot of the trunk this critter climbs at the end of its life, if it found one.
    ///
    /// Written once when the critter is stood up and never again — the same category of
    /// spawn-time constant as `anchor`, and the reason [`place`] takes five arguments rather
    /// than four. `None` is the documented fallback: it forages for its whole life.
    trunk: Option<Vec3>,
    /// How much of the critter is drawn: 0 invisible, 1 whole.
    pub(super) fade: f32,
    /// What `fade` is moving towards. Zero means this critter is on its way out, and nothing
    /// ever moves it back, so a look that flickers cannot make a critter flicker with it.
    pub(super) wanted: f32,
    /// The height its feet are drawn at: the ground's answer, eased.
    ///
    /// The only per-critter state its position has, and it is deliberately the *ground* rather
    /// than the whole position: [`place`] remains the whole of where a critter is on the
    /// plane, [`climb_rise`] is the whole of how far above this it has climbed, and this is
    /// what the terrain says about the column it is in.
    stand: f32,
    /// Which pair of [`CritterVisuals::pool`] this critter draws from. Distinct from `index`:
    /// a stray and a new critter can hold the same *slot*, and must not share an alpha.
    pool: usize,
    body_material: Handle<StandardMaterial>,
    tail_material: Handle<StandardMaterial>,
}

/// One tail, as a child of the critter it belongs to.
#[derive(Component, Debug)]
pub(super) struct CritterTail {
    /// Its own copy of the row's flick, so the tail needs nothing from its parent and the two
    /// queries can be taken in one system without aliasing a `Transform`.
    flick_hz: f32,
}

/// Builds the two meshes and every material any critter will ever wear.
pub(super) fn create_visuals(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(CritterVisuals {
        body: meshes.add(body_mesh()),
        // Authored from its root outwards, so rotating the child about its own origin is the
        // flick and nothing has to offset it.
        tail: meshes.add(tail_mesh()),
        // Colourless and invisible until a critter claims the pair and writes its coat in.
        pool: std::array::from_fn(|_| {
            (
                materials.add(coat_material(Color::WHITE, 0.0)),
                materials.add(coat_material(Color::WHITE, 0.0)),
            )
        }),
    });
}

/// The buffers one hand-authored critter mesh is accumulated into.
///
/// The same three-attribute accumulator `player/hands.rs` uses for its blade and `birds.rs`
/// for its body, and deliberately a third small copy rather than either made public: each
/// carries the assumptions of its own model, and `hands::MeshBuild::fan` writes
/// `livery::neutral_uv` into every corner, which is a statement about the first-person hand's
/// atlas that a critter — whose material carries no image at all — has no part in.
#[derive(Debug, Default)]
struct MeshBuild {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
}

impl MeshBuild {
    /// One flat-shaded quad, wound around its perimeter.
    ///
    /// Flat rather than smooth, for the reason `hands::MeshBuild::quad` gives: the facets are
    /// the shape, and averaging normals along a ring would soften exactly where the light
    /// should break.
    fn quad(&mut self, corners: [Vec3; 4], uvs: [[f32; 2]; 4]) {
        let [a, b, c, d] = corners;
        // From the diagonals rather than from one triangle's two edges: a quad lofted between
        // two sections of different widths is not exactly planar.
        let normal = (c - a).cross(d - b).normalize_or_zero();
        let first = self.push(corners.into_iter().zip(uvs), normal);
        self.indices
            .extend([first, first + 1, first + 3, first + 1, first + 2, first + 3]);
    }

    /// One flat-shaded polygon, as a fan from its first corner.
    ///
    /// The corners must already be wound so that `normal` is the outward one; [`loft`]
    /// reverses them for the end that faces the other way.
    fn fan(&mut self, corners: &[Vec3], normal: Vec3) {
        let first = self.push(corners.iter().map(|corner| (*corner, [0.5, 0.5])), normal);
        for corner in 1..corners.len() as u32 - 1 {
            self.indices
                .extend([first, first + corner, first + corner + 1]);
        }
    }

    /// Appends vertices sharing one normal, and answers the index the first of them landed at.
    fn push(&mut self, corners: impl Iterator<Item = (Vec3, [f32; 2])>, normal: Vec3) -> u32 {
        let first = self.positions.len() as u32;
        for (corner, uv) in corners {
            self.positions.push(corner.to_array());
            self.normals.push(normal.to_array());
            self.uvs.push(uv);
        }
        first
    }

    /// The three attributes and the indices, as the asset the renderer draws.
    fn finish(self) -> Mesh {
        Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs)
        .with_inserted_indices(Indices::U32(self.indices))
    }
}

/// Lofts one closed shell through `rings` and caps both ends.
///
/// Every ring must carry the same number of corners, wound the same way, and the rings must
/// run along `forward`. The first cap is the first ring **reversed** — it faces the other way,
/// exactly as `hands::blade_loft`'s root cap and `birds::loft` do — and the last is the last
/// ring as authored.
fn loft(build: &mut MeshBuild, rings: &[Vec<Vec3>], forward: Vec3) {
    let spans = rings.len() - 1;
    for (span, pair) in rings.windows(2).enumerate() {
        let [lower, upper] = pair else {
            unreachable!("windows(2) yields pairs")
        };
        let sides = lower.len();
        let along = |step: usize| step as f32 / spans as f32;
        for corner in 0..sides {
            let next = (corner + 1) % sides;
            let around = |step: usize| step as f32 / sides as f32;
            build.quad(
                [lower[corner], lower[next], upper[next], upper[corner]],
                [
                    [around(corner), along(span)],
                    [around(corner + 1), along(span)],
                    [around(corner + 1), along(span + 1)],
                    [around(corner), along(span + 1)],
                ],
            );
        }
    }
    let mut first: Vec<Vec3> = rings[0].clone();
    first.reverse();
    build.fan(&first, -forward);
    build.fan(&rings[spans], forward);
}

/// One cross-section of a critter's body: where it sits along the critter, how far it reaches
/// to either side, how tall it is, and how far its centre is off the ground.
///
/// The fourth field is what `birds::BodySection` has no need of: a bird is authored about its
/// own centre line because nothing it does is relative to a floor, while a critter's model has
/// to **stand on `y = 0`** so that placing it on a measured surface puts its feet there. So a
/// section carries a `lift` — how high the middle of the body is at that station — and the
/// shape's lowest point is what `the_model_stands_on_its_own_origin` checks.
#[derive(Debug, Clone, Copy)]
struct BodySection {
    z: f32,
    half_width: f32,
    half_height: f32,
    lift: f32,
}

impl BodySection {
    /// The eight corners of the section, in order around its perimeter.
    ///
    /// **The order is load-bearing rather than a convention**, and this is the warning
    /// `hands::BladeSection::perimeter` and `birds::BodySection::perimeter` both carry:
    /// [`MeshBuild::quad`] takes the outward normal from the corners it is handed, so a ring
    /// walked the other way round is a critter lit entirely from the inside. `cull_mode: None`
    /// means the shape does not even vanish to say so — it merely looks wrong, at ten blocks,
    /// where nobody will diagnose it. Counter-clockwise seen from `+Z`, lofted toward `+Z`,
    /// and `every_face_of_a_critter_is_wound_outward` is what checks that rather than a pair
    /// of eyes.
    fn perimeter(self) -> Vec<Vec3> {
        let Self {
            z,
            half_width: w,
            half_height: h,
            lift,
        } = self;
        let (dw, dh) = (w * FRAC_1_SQRT_2, h * FRAC_1_SQRT_2);
        vec![
            Vec3::new(0.0, lift + h, z),
            Vec3::new(-dw, lift + dh, z),
            Vec3::new(-w, lift, z),
            Vec3::new(-dw, lift - dh, z),
            Vec3::new(0.0, lift - h, z),
            Vec3::new(dw, lift - dh, z),
            Vec3::new(w, lift, z),
            Vec3::new(dw, lift + dh, z),
        ]
    }
}

/// The sections the body is lofted through, from the tip of the nose to the rump.
///
/// **`-Z` is forward, and `y = 0` is the ground.** `run_the_critters` aims a critter with
/// `Transform::look_to`, which points `-Z` along the heading, so the nose is the most negative
/// `z` in this table; and the model stands on its own origin, so the lowest corner any section
/// reaches is exactly zero.
///
/// The whole model is authored at a body length of exactly one, nose to rump, so
/// [`CritterSpecies::size`] is literally that length. The shape is a squirrel's crouch: a low
/// forequarter, a waist, and haunches taller than the shoulders, which is what makes it read
/// as an animal gathered to spring rather than as a sausage.
///
/// **Two sections carry a `lift` exactly equal to their `half_height`, and that is deliberate
/// rather than a coincidence of rounding.** Those are the belly, and a section whose centre
/// sits its own half-height off the floor is one whose lowest corner is at exactly zero —
/// which is what puts the animal *on* the ground when it is placed on a measured surface.
/// `the_model_stands_on_its_own_origin` asserts that minimum as an equality, so an edit that
/// lifts the belly by a thousandth fails rather than making every squirrel hover.
fn body_sections() -> [BodySection; 9] {
    [
        // The nose: a point, and the reason a critter has a front at all from above.
        BodySection {
            z: -0.500,
            half_width: 0.012,
            half_height: 0.010,
            lift: 0.105,
        },
        BodySection {
            z: -0.455,
            half_width: 0.035,
            half_height: 0.032,
            lift: 0.110,
        },
        // The head, wider and taller than the muzzle, and the waist of a neck behind it —
        // the waist is what makes it a head rather than the front of the body.
        BodySection {
            z: -0.380,
            half_width: 0.082,
            half_height: 0.086,
            lift: 0.135,
        },
        BodySection {
            z: -0.285,
            half_width: 0.068,
            half_height: 0.070,
            lift: 0.120,
        },
        // The shoulders, low: a squirrel's forequarter is close to the ground.
        BodySection {
            z: -0.175,
            half_width: 0.100,
            half_height: 0.098,
            lift: 0.098,
        },
        BodySection {
            z: -0.030,
            half_width: 0.108,
            half_height: 0.106,
            lift: 0.106,
        },
        // The haunches: the tallest and widest part, and the reason the silhouette rises
        // toward the back.
        BodySection {
            z: 0.140,
            half_width: 0.125,
            half_height: 0.132,
            lift: 0.140,
        },
        BodySection {
            z: 0.330,
            half_width: 0.105,
            half_height: 0.115,
            lift: 0.150,
        },
        // The rump, where the tail is rooted.
        BodySection {
            z: 0.500,
            half_width: 0.055,
            half_height: 0.060,
            lift: 0.155,
        },
    ]
}

/// The sections the tail is lofted through, from its root at the rump out to its tip.
///
/// **Authored from its own root outwards along `+Z`**, so rotating the child entity about its
/// origin is the flick and nothing has to offset it — the property `birds::wing_sections`
/// relies on for the same reason.
///
/// It is a *bushy* tail: the width more than doubles away from the root before it tapers, and
/// it is as tall as it is wide rather than flat, because a squirrel's tail is the half of the
/// silhouette that identifies it. `a_tail_is_bushy_rather_than_a_rod` is what holds that
/// against a future edit that quietly tidies it into a cylinder.
fn tail_sections() -> [BodySection; 5] {
    [
        BodySection {
            z: 0.000,
            half_width: 0.040,
            half_height: 0.044,
            lift: 0.0,
        },
        BodySection {
            z: 0.130,
            half_width: 0.078,
            half_height: 0.086,
            lift: 0.0,
        },
        BodySection {
            z: 0.290,
            half_width: 0.098,
            half_height: 0.112,
            lift: 0.0,
        },
        BodySection {
            z: 0.440,
            half_width: 0.080,
            half_height: 0.094,
            lift: 0.0,
        },
        BodySection {
            z: 0.545,
            half_width: 0.026,
            half_height: 0.032,
            lift: 0.0,
        },
    ]
}

/// Where the tail is rooted on the body, in the model's own units.
///
/// The last body section's station, lifted to the middle of the rump: a tail hinged at the
/// ground would sweep through the terrain, and one hinged at the top of the rump would float.
const TAIL_ROOT: Vec3 = Vec3::new(0.0, 0.155, 0.440);

/// How far the tail swings from its resting arch, in radians, and where that rest is.
///
/// **The rest is not level, and that is the whole of the pose.** A squirrel at ease holds its
/// tail up over its back in an S; a tail sticking straight out behind is a rat. So the child
/// is rotated most of a right angle up as its neutral, and the flick is a modest swing about
/// that — `a_tail_is_held_over_the_back_and_flicks_about_that_rest` pins both halves.
const TAIL_REST_RADIANS: f32 = -1.15;
const TAIL_FLICK_RADIANS: f32 = 0.22;

// The two halves of that pose, checked by the compiler rather than by a test, which is what
// `ambience.rs` does with the arithmetic relating its lattice constants. A claim about two
// literals sitting beside each other is one a test can only restate: clippy says as much —
// `assertions_on_constants` fires on exactly this — and a compile-time assertion is the form
// that both satisfies it and fails at the declaration a reader is editing.
const _: () = assert!(
    TAIL_REST_RADIANS < -0.8,
    "the tail is not held over the back"
);
const _: () = assert!(
    TAIL_FLICK_RADIANS < -TAIL_REST_RADIANS / 2.0,
    "the flick is a sweep rather than a twitch"
);

/// The body, as one mesh and therefore one draw.
fn body_mesh() -> Mesh {
    let mut build = MeshBuild::default();
    let rings: Vec<Vec<Vec3>> = body_sections()
        .iter()
        .map(|section| section.perimeter())
        .collect();
    loft(&mut build, &rings, Vec3::Z);
    build.finish()
}

/// The tail, lofted from its root out to its tip.
fn tail_mesh() -> Mesh {
    let mut build = MeshBuild::default();
    let rings: Vec<Vec<Vec3>> = tail_sections()
        .iter()
        .map(|section| section.perimeter())
        .collect();
    loft(&mut build, &rings, Vec3::Z);
    build.finish()
}

/// The rotation one tail has, `elapsed` seconds into the session.
///
/// About `X`, which is the axis that lifts it over the back: the tail is authored along `+Z`
/// and a negative turn about `X` takes `+Z` toward `+Y`. A negative scale would also arch it
/// and would invert the winding — which `cull_mode: None` hides rather than fixes, which is
/// exactly why it is not used.
fn tail_turn(tail: &CritterTail, elapsed: f32) -> Quat {
    let swing = (elapsed * tail.flick_hz * TAU).sin() * TAIL_FLICK_RADIANS;
    Quat::from_rotation_x(TAIL_REST_RADIANS + swing)
}

/// Lit, blended and drawn from both faces.
///
/// **Lit** for the reason `birds::plumage_material` gives: `player/sky.rs`'s bodies are unlit
/// because they are the light source, and a critter is not — it is an object in the world, so
/// night darkens it and the fog takes it at distance exactly as they take a mob.
///
/// **`cull_mode: None`** so that the winding mistake [`BodySection::perimeter`] warns about is
/// a shading mistake rather than a hole; the winding is held by a test instead.
/// **`AlphaMode::Blend` and an explicit alpha** because the fade is written here, which is
/// also why the pair a critter draws from is its own rather than its coat's — see
/// [`CritterVisuals`].
fn coat_material(colour: Color, alpha: f32) -> StandardMaterial {
    StandardMaterial {
        base_color: colour.with_alpha(alpha),
        alpha_mode: AlphaMode::Blend,
        cull_mode: None,
        ..default()
    }
}

/// Everything `keep_the_critters` reads and nothing it writes.
#[derive(SystemParam)]
pub(super) struct GroundInputs<'w> {
    ambience: Res<'w, Ambience>,
    session: Option<Res<'w, Session>>,
    store: Option<Res<'w, ChunkStore>>,
    clock: Res<'w, SkyClock>,
    time: Res<'w, Time>,
    visuals: Option<Res<'w, CritterVisuals>>,
}

/// Decides which critters should exist, and stands the missing ones up.
///
/// Runs after `camera::AimCamera` and after `ambience::sample_the_ground`, so the anchor is
/// this frame's eye and the look is this frame's answer. It writes nothing outside its own
/// entities.
///
/// **It is `birds::keep_the_flock` with one extra retirement and one extra spawn-time read.**
/// The retirement is the life: a critter whose generation is no longer the clock's is on its
/// way out, which is what makes a population that comes and goes rather than one that is only
/// ever replaced by walking. The read is the trunk, and it is why this system needs the store
/// at all — `keep_the_flock` does not.
pub(super) fn keep_the_critters(
    read: GroundInputs<'_>,
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    eyes: Query<&Transform, With<WorldCamera>>,
    mut ground: Query<(Entity, &mut Critter)>,
) {
    let GroundInputs {
        ambience,
        session,
        store,
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

    // Indoors of its own half of the day, and only when the server keeps a clock:
    // `night_now` answers `None` for a world with no time of day, which puts every row about
    // all day rather than never — see `Period::abroad`.
    let night = session
        .as_deref()
        .and_then(|session| sky::night_now(&clock, session));
    let wanted = species_for(&ambience).filter(|index| CRITTERS[*index].abroad.abroad(night));
    let cell_seed = cell_seed(cell);
    // Read before the retirement pass, because how many this cell wants is what decides how
    // many of the previous cell's critters may stay.
    let wanted_size = wanted.map_or(0, |index| group_size(&CRITTERS[index], cell_seed));

    // Retire everything that is the wrong species for this look, that the anchor has left
    // behind, or whose life is over. Retiring is one-way, so a look that flickers cannot
    // oscillate a population.
    //
    // **Two counts, because a fade makes "how many critters are there" two questions.**
    // `staying` is every critter not on its way out, bounded by `wanted_size`, so the living
    // population is one row's and never two anchors' summed. `alive` is every entity, the
    // fading ones included, bounded by `CRITTER_COUNT_MAX` — which is also what guarantees
    // the material pool has a free pair for a critter about to spawn.
    let mut bias = Vec3::ZERO;
    let mut taken = [false; CRITTER_COUNT_MAX];
    let mut pool_taken = [false; CRITTER_COUNT_MAX];
    let mut alive = 0usize;
    let mut staying = 0usize;
    // A critter of the right row still inside the box whose anchor is a *previous* cell's. It
    // holds no slot in `taken` — its `index` numbers another anchor's group — so it is counted
    // against `wanted_size` below instead of being invisible to it, which is the defect
    // `birds::keep_the_flock` names: a one-cell walk otherwise leaves the old group on the
    // ground and a second group is stood up beside it.
    let mut strays = [None; CRITTER_COUNT_MAX];
    let mut stray_count = 0usize;
    for (entity, mut critter) in &mut ground {
        alive += 1;
        pool_taken[critter.pool] = true;
        // Already leaving: it holds a pool pair and counts against the population, but
        // nothing here may bring it back.
        if critter.wanted == 0.0 {
            continue;
        }
        let row = &CRITTERS[critter.species];
        let (generation, age) = generation_of(row, critter.index, elapsed);
        let position = place(row, critter.seed, age, critter.anchor, critter.trunk);
        let from = position - anchor;
        let outside = Vec3::new(from.x, 0.0, from.z).abs().max_element() > CRITTER_RANGE;
        if wanted != Some(critter.species) || outside || generation != critter.generation {
            critter.wanted = 0.0;
            // Only a critter the *anchor* left behind says which way the player went; one
            // retired because the ground changed under them, or because its life ran out,
            // says nothing about direction.
            if outside {
                bias += anchor - critter.anchor;
            }
            continue;
        }
        if critter.anchor == anchor && critter.index < CRITTER_COUNT_MAX {
            // This cell's own group. `index < wanted_size` holds by construction: the anchor
            // determines the cell, and the cell determines `wanted_size`.
            taken[critter.index] = true;
            staying += 1;
        } else if stray_count < CRITTER_COUNT_MAX {
            strays[stray_count] = Some(entity);
            stray_count += 1;
        }
    }

    // A stray stays only while this cell's group has room for it, and starts fading the
    // moment it does not — it fades rather than vanishing, and the count is `wanted_size`.
    for entity in strays.into_iter().flatten() {
        if staying < wanted_size {
            staying += 1;
        } else if let Ok((_, mut critter)) = ground.get_mut(entity) {
            critter.wanted = 0.0;
        }
    }

    let Some(index) = wanted else {
        return;
    };
    let species = &CRITTERS[index];
    // Nothing to stand on and nothing to climb: a critter is not stood up at all rather than
    // stood up in the air. This is the one place the two systems differ in what an absent
    // store means — `run_the_critters` *holds* a critter whose ground it cannot read, because
    // it already has a height, and here there is no height to hold. It is temporary either
    // way: the next frame with a store stands the critter up.
    let (Some(store), Some(session)) = (store.as_deref(), session.as_deref()) else {
        return;
    };
    let chunk_size = usize::from(session.0.chunk_size);

    for (slot, held) in taken.iter().enumerate().take(wanted_size) {
        // The group is the cap, and `group_size` is already clamped to CRITTER_COUNT_MAX. The
        // second guard is the whole population rather than one group: a critter still fading
        // out holds a material pair, so a free pair exists only while `alive` is under the
        // maximum.
        if staying >= wanted_size || alive >= CRITTER_COUNT_MAX {
            break;
        }
        if *held {
            continue;
        }
        let Some(pool) = pool_taken.iter().position(|claimed| !claimed) else {
            break;
        };
        let (generation, age) = generation_of(species, slot, elapsed);
        let seed = seed_on_the_far_side(cell_seed, slot, generation, anchor, bias);
        // The ground under where this critter's forage begins, which is what both the trunk
        // search and the first frame's height are measured from. No ground, no critter: the
        // alternative is one standing in the air until the probe succeeds.
        let home = place(species, seed, age, anchor, None);
        let Ground::Surface(surface) = surface_under(store, home, anchor.y, chunk_size) else {
            continue;
        };
        // Read once, here, and never again: `Critter::trunk` is a spawn-time constant.
        //
        // **Searched where the forage ends rather than where it begins.** A critter browses
        // several blocks across the ground over its life, so a trunk within `TRUNK_REACH` of
        // its first position can be twice that from its last — and the approach would then
        // have to outrun the gait to reach it. The forage's end is knowable here because the
        // path is a pure function: `place` at `forage_seconds` is where this critter will be
        // when it stops foraging, and no frame has to run for that to be true.
        let ends_at = place(species, seed, species.forage_seconds(), anchor, None);
        let trunk = species
            .climbs
            .then(|| trunk_near(store, ends_at, surface, chunk_size))
            .flatten();
        pool_taken[pool] = true;
        staying += 1;
        alive += 1;
        // Claimed, not minted: `create_visuals` built every pair, and this writes the coat the
        // seed chose into the two handles the slot owns.
        let (body_colour, tail_colour) = species.coat_at(species.coat_of(seed));
        let (body_material, tail_material) = visuals.pool[pool].clone();
        if let Some(mut material) = materials.get_mut(&body_material) {
            *material = coat_material(body_colour, 0.0);
        }
        if let Some(mut material) = materials.get_mut(&tail_material) {
            *material = coat_material(tail_colour, 0.0);
        }
        let at = place(species, seed, age, anchor, trunk);
        let critter = commands
            .spawn((
                Critter {
                    species: index,
                    seed,
                    index: slot,
                    generation,
                    anchor,
                    trunk,
                    fade: 0.0,
                    wanted: 1.0,
                    stand: surface,
                    pool,
                    body_material: body_material.clone(),
                    tail_material: tail_material.clone(),
                },
                Mesh3d(visuals.body.clone()),
                MeshMaterial3d(body_material),
                Transform::from_translation(Vec3::new(at.x, surface, at.z))
                    .with_scale(Vec3::splat(species.size)),
                Visibility::Visible,
            ))
            .id();
        commands.entity(critter).with_children(|parent| {
            parent.spawn((
                CritterTail {
                    flick_hz: species.flick_hz,
                },
                Mesh3d(visuals.tail.clone()),
                MeshMaterial3d(tail_material),
                Transform::from_translation(TAIL_ROOT),
            ));
        });
    }
}

/// The one camera, told apart from the entities this system also holds mutably.
///
/// Bevy cannot prove a `WorldCamera` is neither a critter nor a tail, and refuses the system
/// rather than risk aliasing the `Transform` — the same reason `birds::EyeOfTheFlock` and
/// `player/sky.rs`'s `Without<Sun>` filter exist. A named type because the filter is otherwise
/// long enough for clippy to call the query complex, and a name is better than an allow.
type EyeOnTheGround = (With<WorldCamera>, Without<Critter>, Without<CritterTail>);

/// Everything `run_the_critters` reads.
#[derive(SystemParam)]
pub(super) struct RunInputs<'w> {
    session: Option<Res<'w, Session>>,
    store: Option<Res<'w, ChunkStore>>,
    time: Res<'w, Time>,
}

/// Moves every critter, stands it on the ground, flicks its tail, and fades the ones on their
/// way out.
///
/// Two transforms per critter per frame and one colour write when the alpha has actually
/// moved: at [`CRITTER_COUNT_MAX`] that is eight transforms, beside the eighteen a full flock
/// costs. The tail is a second query rather than a child lookup because the parent's `Critter`
/// is already held here — `CritterTail` carries its own copy of the row's flick, so neither
/// loop has to reach into the other's entity.
///
/// **The height is the ground's and the climb's, in that order**, which is where this differs
/// from `birds::fly_the_flock`: the flock takes `place`'s `y` and lifts it clear of the
/// terrain, and here the terrain *is* the `y` and the climb is what goes on top. A critter the
/// terrain cannot place — [`Ground::Empty`], a chasm — is retired rather than drawn somewhere
/// arbitrary.
///
/// Critters are hidden, not faded, while the eye is submerged: the same override
/// `player/sky.rs` applies to the fog and `birds.rs` to the flock, read through the same
/// answer so there are not two of them.
pub(super) fn run_the_critters(
    read: RunInputs<'_>,
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    eyes: Query<&Transform, EyeOnTheGround>,
    mut ground: Query<(Entity, &mut Critter, &mut Transform, &mut Visibility)>,
    mut tails: Query<(&CritterTail, &mut Transform), Without<Critter>>,
) {
    let RunInputs {
        session,
        store,
        time,
    } = read;
    let elapsed = time.elapsed_secs();
    let dt = time.delta_secs();
    let step = dt / CRITTER_FADE_SECONDS;

    let submerged = match (session.as_deref(), eyes.iter().next()) {
        (Some(session), Some(eye)) => sky::submerged_at(
            store.as_deref(),
            eye.translation,
            usize::from(session.0.chunk_size),
        ),
        _ => false,
    };
    // The terrain a critter stands on, if there is any to read. A frame with no session or no
    // store answers the same way an unloaded chunk does: the height is held.
    let terrain = match (store.as_deref(), session.as_deref()) {
        (Some(store), Some(session)) => Some((store, usize::from(session.0.chunk_size))),
        _ => None,
    };

    for (entity, mut critter, mut transform, mut visibility) in &mut ground {
        let fade = if critter.wanted > critter.fade {
            (critter.fade + step).min(critter.wanted)
        } else {
            (critter.fade - step).max(critter.wanted)
        };
        if critter.wanted == 0.0 && fade <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }
        if fade != critter.fade {
            critter.fade = fade;
            for handle in [critter.body_material.clone(), critter.tail_material.clone()] {
                if let Some(mut material) = materials.get_mut(&handle) {
                    material.base_color = material.base_color.with_alpha(fade);
                }
            }
        }

        let species = &CRITTERS[critter.species];
        let (_, age) = generation_of(species, critter.index, elapsed);
        // The last fade of a life belongs to the end of the climb, so a squirrel is high in
        // its trunk while it is disappearing rather than dissolving on the ground. It is set
        // here rather than in `keep_the_critters` because this is the system that owns the
        // fade, and it is one-way: nothing moves `wanted` back up.
        if critter.wanted > 0.0 && age >= species.life - CRITTER_FADE_SECONDS {
            critter.wanted = 0.0;
        }

        let position = place(species, critter.seed, age, critter.anchor, critter.trunk);
        // The ground: a named step over `place`'s answer, never a sixth argument to it.
        let Some(stand) = next_stand(terrain, position, critter.anchor.y, critter.stand, dt) else {
            // Nowhere to stand. Retired rather than drawn: this frame keeps the height it
            // had, and the fade takes it from here.
            critter.wanted = 0.0;
            continue;
        };
        // Guarded for the reason the visibility write below is: `Mut` marks a component
        // changed on every `DerefMut`, and over level ground this is the same number every
        // frame forever.
        if stand != critter.stand {
            critter.stand = stand;
        }
        let rise = climb_rise(species, age, critter.trunk);
        transform.translation = Vec3::new(position.x, stand + rise, position.z);
        // Which way it faces is the direction it is going, sampled from the same pure
        // function rather than differenced against last frame — so a critter nothing drew for
        // a hundred frames comes back facing correctly on the first one. Horizontal only: a
        // climbing squirrel keeps its body along the trunk's column rather than pitching, and
        // a heading that took the ground's slope in would tip it into the hill.
        let ahead = place(
            species,
            critter.seed,
            age + HEADING_STEP,
            critter.anchor,
            critter.trunk,
        ) - position;
        if let Ok(heading) = Dir3::new(Vec3::new(ahead.x, 0.0, ahead.z)) {
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

    for (tail, mut transform) in &mut tails {
        transform.rotation = tail_turn(tail, elapsed);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::world::{BlockId, VoxelChunk};
    use bevy::mesh::MeshVertexAttributeId;

    const DT: f32 = 1.0 / 60.0;
    /// The fastest a critter may be drawn climbing, in blocks per second.
    ///
    /// A separate bound from [`CritterSpecies::max_speed`] because it bounds a separate
    /// number: the gait's bound is on [`place`], which is horizontal, and the rise is a named
    /// step over it that `place` never sees. Nine blocks over 2.39 seconds is 3.77 on average
    /// and 5.65 at a smoothstep's peak, so six is the bound with the same one-and-a-half
    /// factor [`SCURRY_DASH_SHARE`] explains folded in.
    const CLIMB_SPEED_MAX: f32 = 6.0;
    /// The chunk edge every fixture below is built at, and asks about.
    const CHUNK: usize = 32;
    /// How long a critter is given to settle onto the ground before its height is read.
    ///
    /// A critter is stood up exactly on the surface under its first position, so the only
    /// thing it has to settle is the change in ground as it scurries — a tenth of a second is
    /// six frames of [`STAND_STEP_SPEED`], which is a block and a half.
    const SETTLED: usize = 6;

    /// How many frames one whole life is, at sixty a second.
    fn life_frames(species: &CritterSpecies) -> usize {
        (species.life / DT).ceil() as usize
    }

    /// A store over every chunk a box of `reach` around `centre` touches, holding whatever
    /// `block_at` names at each voxel and air wherever it names [`palette::AIR`].
    ///
    /// Synthetic on purpose: the ground step's whole input is "what is in this column", so a
    /// terrain a test can state in one closure is the only fixture it needs. It is
    /// `birds.rs`'s clamp fixture with the block moved *into* the closure rather than beside
    /// it, which is what the trunk search needs: a wood is a floor **and** a log, and two
    /// kinds of block in one store cannot be stated by a predicate over one.
    fn blocks(centre: Vec3, reach: f32, block_at: impl Fn(IVec3) -> BlockId) -> ChunkStore {
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
                                let block = block_at(at);
                                if block != palette::AIR {
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

    /// The common case of [`blocks`]: one kind of block wherever `solid` says so.
    fn terrain(
        centre: Vec3,
        reach: f32,
        block: BlockId,
        solid: impl Fn(IVec3) -> bool,
    ) -> ChunkStore {
        blocks(
            centre,
            reach,
            |at| {
                if solid(at) { block } else { palette::AIR }
            },
        )
    }

    /// One critter's drawn path over `frames` frames: `(drawn point, stand, rise)` each frame,
    /// or `None` for the frame the ground gave it nowhere to be.
    ///
    /// It drives [`next_stand`] and [`climb_rise`] rather than restating what
    /// `run_the_critters` does with them: a test that re-implemented the ground step would
    /// pass whatever the client actually drew.
    fn walked(
        ground: Option<(&ChunkStore, usize)>,
        species: &CritterSpecies,
        seed: u64,
        anchor: Vec3,
        trunk: Option<Vec3>,
        frames: usize,
    ) -> Vec<Option<(Vec3, f32, f32)>> {
        let mut stand = match ground {
            Some((store, size)) => {
                match surface_under(
                    store,
                    place(species, seed, 0.0, anchor, trunk),
                    anchor.y,
                    size,
                ) {
                    Ground::Surface(surface) => surface,
                    _ => anchor.y,
                }
            }
            None => anchor.y,
        };
        let mut path = Vec::with_capacity(frames + 1);
        for frame in 0..=frames {
            let age = frame as f32 * DT;
            let at = place(species, seed, age, anchor, trunk);
            match next_stand(ground, at, anchor.y, stand, DT) {
                Some(next) => {
                    stand = next;
                    let rise = climb_rise(species, age, trunk);
                    path.push(Some((Vec3::new(at.x, stand + rise, at.z), stand, rise)));
                }
                None => path.push(None),
            }
        }
        path
    }

    #[test]
    fn one_row_per_look_and_no_row_answers_an_unknown_one() {
        assert_eq!(species_for(&Ambience::default()), None);
        assert_eq!(
            species_for(&Ambience {
                ground: GroundLook::Grass,
                wooded: false,
            }),
            None,
            "an open plain has no squirrels"
        );
        assert_eq!(
            species_for(&Ambience {
                ground: GroundLook::Grass,
                wooded: true,
            }),
            Some(0)
        );
        for ground in [GroundLook::Sand, GroundLook::Snow] {
            for wooded in [false, true] {
                assert_eq!(
                    species_for(&Ambience { ground, wooded }),
                    None,
                    "{ground:?}/{wooded} got a critter it has no row for"
                );
            }
        }
        assert!(
            CRITTERS.iter().all(|row| row.ground != GroundLook::Unknown),
            "no row may live on an answer that means there is no answer"
        );
    }

    /// The gate is one fact read by two tables, and this is what makes that a fact rather than
    /// a comment: the squirrel is about exactly where the macaw flies.
    #[test]
    fn a_squirrel_lives_where_the_macaw_flies() {
        for ground in [
            GroundLook::Unknown,
            GroundLook::Grass,
            GroundLook::Sand,
            GroundLook::Snow,
        ] {
            for wooded in [false, true] {
                let ambience = Ambience { ground, wooded };
                let squirrels = species_for(&ambience).is_some();
                let macaws = super::super::birds::species_for(&ambience) == Some(0);
                assert_eq!(
                    squirrels, macaws,
                    "{ground:?}/{wooded}: squirrels {squirrels}, macaws {macaws}"
                );
            }
        }
    }

    #[test]
    fn a_critter_moves_no_faster_than_its_row_allows() {
        // The whole reason the path is a pure function: a critter may not teleport, and the
        // only way to know it does not is to walk it. A whole life covers the forage, the
        // seam into the climb, the approach and the rise.
        //
        // **The trunks are placed relative to where each forage ends, within
        // [`TRUNK_REACH`], because that is the precondition the caller actually upholds.**
        // `keep_the_critters` probes for a trunk at `place(.., forage_seconds, ..)` and
        // `trunk_near` answers nothing beyond `TRUNK_REACH` of the point it is handed — so a
        // trunk further away than that is a configuration this client cannot produce. Written
        // with trunks pinned to the anchor instead, this test failed at 6.62 blocks a second
        // against a 6.5 bound, measuring an approach no critter will ever walk;
        // `a_trunk_is_a_standing_log_near_the_critter_and_nothing_else` is what holds up the
        // other half of the pair, by asserting the reach the probe answers within.
        let anchor = Vec3::new(96.0, 80.0, -32.0);
        for species in &CRITTERS {
            for offset in [
                None,
                Some(Vec3::new(TRUNK_REACH, 0.0, 0.0)),
                Some(Vec3::new(-0.6 * TRUNK_REACH, 0.0, 0.7 * TRUNK_REACH)),
                // A trunk right where the forage ends, which is the degenerate approach: zero
                // distance over a real duration.
                Some(Vec3::ZERO),
            ] {
                for seed in 0..16u64 {
                    let seed = mix(seed, 0xFACE);
                    let ends_at = place(species, seed, species.forage_seconds(), anchor, None);
                    let trunk = offset.map(|offset| ends_at + offset);
                    if let Some(trunk) = trunk {
                        let reach = (trunk - ends_at).length();
                        assert!(
                            reach <= TRUNK_REACH + 1e-3,
                            "the fixture put a trunk {reach} away, which `trunk_near` cannot"
                        );
                    }
                    let mut previous = place(species, seed, 0.0, anchor, trunk);
                    for frame in 1..=life_frames(species) {
                        let now = place(species, seed, frame as f32 * DT, anchor, trunk);
                        let moved = now.distance(previous);
                        assert!(
                            moved <= species.max_speed * DT,
                            "{:?} moved {moved} in {DT}s, over its {} bound",
                            species.gait,
                            species.max_speed
                        );
                        previous = now;
                    }
                }
            }
        }
    }

    #[test]
    fn a_critter_never_leaves_its_horizontal_box() {
        // With the anchor still, the home spread and the forage's travel keep every critter
        // inside `CRITTER_RANGE` by construction — so the only thing that ever puts one
        // outside is the anchor moving, which is the case `keep_the_critters` handles.
        //
        // **Horizontal, and the vertical is deliberately not asserted here**: a critter's
        // height is the terrain's answer, and a wood on a hillside is not a bug.
        let anchor = Vec3::new(-512.0, 64.0, 512.0);
        let mut reached = 0.0f32;
        for species in &CRITTERS {
            for trunk in [None, Some(anchor + Vec3::new(TRUNK_REACH, 0.0, 0.0))] {
                for seed in 0..32u64 {
                    let seed = mix(seed, 0xB0A7);
                    for frame in 0..=life_frames(species) {
                        let from = place(species, seed, frame as f32 * DT, anchor, trunk) - anchor;
                        let out = Vec3::new(from.x, 0.0, from.z).abs().max_element();
                        assert!(
                            out <= CRITTER_RANGE,
                            "{:?} reached {out} from its anchor",
                            species.gait
                        );
                        reached = reached.max(out);
                    }
                }
            }
        }
        // And the box is not absurdly larger than what is reached, or it would bound nothing
        // and the retirement it backs would never fire.
        assert!(
            reached > CRITTER_RANGE / 2.0,
            "nothing came within half the box: {reached} of {CRITTER_RANGE}"
        );
    }

    #[test]
    fn a_life_is_a_window_of_the_clock_and_the_slots_are_staggered() {
        // The whole of how a critter ends without a birth time: the generation and the age
        // are arithmetic on the session clock. Two properties matter — the age walks forward
        // and wraps to zero exactly when the generation increments, and two slots never wrap
        // on the same frame.
        let species = &CRITTERS[0];
        for slot in 0..CRITTER_COUNT_MAX {
            let mut previous = generation_of(species, slot, 0.0);
            let mut wraps = 0usize;
            for frame in 1..=life_frames(species) * 3 {
                let now = generation_of(species, slot, frame as f32 * DT);
                assert!(
                    (0.0..species.life).contains(&now.1),
                    "slot {slot} aged {} of a {} life",
                    now.1,
                    species.life
                );
                if now.0 == previous.0 {
                    assert!(now.1 > previous.1, "slot {slot} aged backwards");
                } else {
                    assert_eq!(now.0, previous.0 + 1, "slot {slot} skipped a generation");
                    assert!(now.1 < previous.1, "slot {slot} rolled over without ageing");
                    wraps += 1;
                }
                previous = now;
            }
            assert!(wraps >= 2, "slot {slot} lived {wraps} lives in three");
        }

        // Staggered: no two slots share a window boundary, so a cell's critters do not all
        // vanish on one frame.
        let boundaries: HashSet<i64> = (0..CRITTER_COUNT_MAX)
            .map(|slot| {
                let mut at = 0;
                for frame in 1..=life_frames(species) {
                    let elapsed = frame as f32 * DT;
                    if generation_of(species, slot, elapsed).0
                        != generation_of(species, slot, elapsed - DT).0
                    {
                        at = frame as i64;
                        break;
                    }
                }
                at
            })
            .collect();
        assert_eq!(
            boundaries.len(),
            CRITTER_COUNT_MAX,
            "two slots roll over on the same frame: {boundaries:?}"
        );
    }

    #[test]
    fn a_squirrel_is_whole_and_high_in_its_trunk_when_it_starts_to_fade() {
        // The three numbers a disappearance *into the leaves* rests on: the forage share, the
        // climb's approach share and the fade. Get them out of step and a squirrel either
        // dissolves on the ground or is still climbing when it is already half transparent.
        //
        // **An equality, because the rise and the fade are defined to meet.** This was first
        // written as `>= 0.75 * CLIMB_RISE` against a rise that ran to the end of the life,
        // and it measured 6.54 of 9 — a squirrel a third of the way short of the leaves when
        // it began to vanish. The fix was to the rise rather than to the threshold, and the
        // threshold became an equality because a tolerance here would hide the same defect
        // again.
        for species in CRITTERS.iter().filter(|row| row.climbs) {
            let trunk = Some(Vec3::ZERO);
            let at_fade = climb_rise(species, species.life - CRITTER_FADE_SECONDS, trunk);
            assert_eq!(
                at_fade, CLIMB_RISE,
                "{:?} was {at_fade} of {CLIMB_RISE} up when it began to fade",
                species.gait
            );
            // And the climb is a climb rather than a jump: the rate the player sees, bounded
            // the way the gait's own is. `place` does not carry the rise, so
            // `a_critter_moves_no_faster_than_its_row_allows` cannot see this and something
            // has to.
            let mut fastest = 0.0f32;
            for frame in 0..life_frames(species) {
                let was = climb_rise(species, frame as f32 * DT, trunk);
                let now = climb_rise(species, (frame + 1) as f32 * DT, trunk);
                fastest = fastest.max((now - was).abs() / DT);
            }
            assert!(
                fastest <= CLIMB_SPEED_MAX,
                "{:?} climbed at {fastest} blocks a second",
                species.gait
            );
            assert!(fastest > 0.0, "{:?} never climbed at all", species.gait);
            // And it is on the ground for the whole forage, and for the approach after it.
            assert_eq!(climb_rise(species, 0.0, trunk), 0.0);
            assert_eq!(
                climb_rise(species, species.forage_seconds(), trunk),
                0.0,
                "{:?} left the ground before its forage was over",
                species.gait
            );
            assert_eq!(
                climb_rise(
                    species,
                    species.forage_seconds() + species.climb_seconds() * CLIMB_APPROACH_SHARE,
                    trunk
                ),
                0.0,
                "{:?} began to rise before it reached its trunk",
                species.gait
            );
            // A row with no trunk never rises at all, which is the fallback branch.
            assert_eq!(climb_rise(species, species.life * 0.99, None), 0.0);
        }
    }

    #[test]
    fn a_critter_with_no_trunk_forages_and_fades_where_it_is() {
        // The documented fallback, asserted rather than hoped for: a wooded look is a vote
        // over sixty-four columns, so a critter can perfectly well be stood up out of reach
        // of any trunk. It must forage for its whole life and never rise — never panic, never
        // aim at a trunk that is not there, and never leave the ground.
        let anchor = Vec3::new(16.0, 80.0, 16.0);
        let store = terrain(anchor, CRITTER_RANGE + 8.0, palette::GRASS, |at| at.y < 64);
        for species in &CRITTERS {
            for seed in 0..8u64 {
                let seed = mix(seed, 0x7A11);
                let path = walked(
                    Some((&store, CHUNK)),
                    species,
                    seed,
                    anchor,
                    None,
                    life_frames(species),
                );
                for (frame, step) in path.iter().enumerate() {
                    let (drawn, stand, rise) = step.expect("level ground places every critter");
                    assert_eq!(rise, 0.0, "frame {frame} rose with no trunk to climb");
                    assert_eq!(drawn.y, stand, "frame {frame} left the ground");
                }
                // And the position it holds while it fades is the forage's own last one,
                // rather than a jump to a trunk column it never had.
                let last = place(species, seed, species.life, anchor, None);
                let held = place(species, seed, species.life * 2.0, anchor, None);
                assert_eq!(last, held, "a trunkless critter moved after its forage");
            }
        }
    }

    // -----------------------------------------------------------------------
    // The ground a critter stands on
    // -----------------------------------------------------------------------

    #[test]
    fn the_surface_a_critter_stands_on_is_the_top_face_of_what_holds_it_up() {
        // Solid below 40, so the highest voxel is 39. It spans `[39, 40)`, and 40 is where a
        // critter's feet are.
        let store = terrain(Vec3::new(8.0, 40.0, 8.0), 40.0, palette::GRASS, |at| {
            at.y < 40
        });
        let column = Vec3::new(8.5, 0.0, 8.5);
        assert_eq!(
            surface_under(&store, column, 40.0, CHUNK),
            Ground::Surface(40.0)
        );
        // **The window's reach is asserted at both its edges**, because a window is only a
        // bound if something is outside it. The highest solid voxel is 39, so a probe whose
        // floor is exactly 39 still finds it and one a single block higher does not — which
        // is the difference between "the ground near the player" and "any ground at all".
        assert_eq!(
            surface_under(&store, column, 39.0 + STAND_PROBE_BELOW, CHUNK),
            Ground::Surface(40.0)
        );
        assert_eq!(
            surface_under(&store, column, 40.0 + STAND_PROBE_BELOW, CHUNK),
            Ground::Empty,
            "a window whose floor is above the ground is not a measurement of the ground"
        );
        // And a window *buried* in the hill answers the top of the window rather than the top
        // of the hill, which is the honest answer and worth pinning rather than leaving to be
        // discovered: the probe is bounded, so the surface it reports is the highest one it
        // was allowed to look at. A critter there is standing inside a hill, and it is the
        // eye's own anchor that keeps that from happening — the window is centred on the
        // player, who is not usually inside the ground.
        assert_eq!(
            surface_under(&store, column, 40.0 - STAND_PROBE_ABOVE - 2.0, CHUNK),
            Ground::Surface(39.0)
        );

        // And it floors rather than truncating, on the side of the origin where the two
        // differ — the trap `player/target.rs`'s raycast names, over half the world.
        let below = terrain(Vec3::new(-8.0, -8.0, -8.0), 24.0, palette::GRASS, |at| {
            at.y < -8
        });
        assert_eq!(
            surface_under(&below, Vec3::new(-0.5, 0.0, -0.5), -8.0, CHUNK),
            Ground::Surface(-8.0)
        );
    }

    #[test]
    fn a_critter_stands_on_what_would_hold_a_body_up_and_not_on_water() {
        // `solid_at` and not "not air", which is the opposite of the choice the flock makes
        // and the same question asked about a different body: a bird's clearance asks what it
        // would be *seen to fly into*, so a lake surface and a leaf canopy both count, while a
        // critter's ground asks what would *hold it up*. A squirrel standing on the surface of
        // a lake is the failure this pins.
        //
        // **Leaves are on the other side of that line and deliberately so.** `palette` calls
        // them solid — they stop a body, which is why a player can walk a canopy — so a
        // squirrel may stand on them, and a squirrel in a canopy is exactly where a squirrel
        // belongs. This test asserted the opposite when it was written, from the assumption
        // that "not the ground" and "not solid" were the same set; `is_solid` is the authority
        // and it disagreed.
        let column = Vec3::new(8.5, 0.0, 8.5);
        let lake = |block| {
            terrain(Vec3::new(8.0, 40.0, 8.0), 40.0, block, |at| {
                (20..40).contains(&at.y)
            })
        };
        for block in [palette::WATER, palette::WATER_FLOW3] {
            assert_eq!(
                surface_under(&lake(block), column, 40.0, CHUNK),
                Ground::Empty,
                "a critter was stood on block {block}"
            );
        }
        // Every block that stops a body does hold a critter up, so the comparison above is
        // about water rather than about the fixture.
        for block in [
            palette::STONE,
            palette::GRASS,
            palette::LOG,
            palette::LEAVES,
        ] {
            assert!(
                palette::is_solid(block),
                "block {block} is not solid, so this row proves nothing"
            );
            assert_eq!(
                surface_under(&lake(block), column, 40.0, CHUNK),
                Ground::Surface(40.0),
                "a critter fell through block {block}"
            );
        }
    }

    #[test]
    fn terrain_nobody_has_streamed_is_not_evidence_of_a_floor_or_of_a_chasm() {
        // Absence is not evidence — the direction `Terrain.Fluid`, the mesher's neighbour
        // rule and the server's step-up probe all take. An unread column is `Unknown`, which
        // holds a critter's height; an empty *read* column is `Empty`, which retires it. The
        // two must not be collapsed: holding on an empty column strands a critter over a
        // chasm forever, and retiring on an unread one kills every critter the moment a chunk
        // is evicted.
        let nothing = ChunkStore::default();
        let column = Vec3::new(8.5, 0.0, 8.5);
        assert_eq!(
            surface_under(&nothing, column, 40.0, CHUNK),
            Ground::Unknown
        );
        assert_eq!(
            next_stand(Some((&nothing, CHUNK)), column, 40.0, 37.0, DT),
            Some(37.0)
        );
        // A frame with no store or no session at all takes the same direction.
        assert_eq!(next_stand(None, column, 40.0, 37.0, DT), Some(37.0));

        // A gap is not read *through*, either. One chunk holds a floor and the chunk above it
        // never arrived: a probe that crossed the hole would answer with the highest thing it
        // happens to hold rather than with the highest thing there is.
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
        // A window that stays inside the one chunk there is gets the honest answer: from 31
        // it tops out at 39, which is the highest voxel that chunk holds.
        assert_eq!(
            surface_under(&gapped, column, 31.0, 8),
            Ground::Surface(40.0)
        );
        // Opened into the missing chunk above, the floor under it is no longer an answer
        // anybody may give.
        assert_eq!(surface_under(&gapped, column, 38.0, 8), Ground::Unknown);

        // And an empty read column is not a height at all.
        let void = terrain(Vec3::new(8.0, 40.0, 8.0), 40.0, palette::GRASS, |_| false);
        assert_eq!(
            surface_under(&void, Vec3::new(8.5, 0.0, 8.5), 40.0, CHUNK),
            Ground::Empty
        );
        assert_eq!(
            next_stand(
                Some((&void, CHUNK)),
                Vec3::new(8.5, 0.0, 8.5),
                40.0,
                37.0,
                DT
            ),
            None
        );
    }

    #[test]
    fn a_critter_stands_on_flat_ground_exactly_and_on_broken_ground_within_a_voxel() {
        // **Two terrains, because the honest claim is two claims.** On flat ground a settled
        // critter is on the surface *exactly*: the ease reaches its target and sits on it,
        // which is the property `approach` is chosen for. On broken ground it cannot be
        // exact every frame and should not pretend to be — a voxel world's surface changes
        // in whole-block steps, so crossing one takes `1 / (STAND_STEP_SPEED * DT)` frames,
        // and during those frames the critter is walking up the step rather than teleporting
        // to the top of it. What is asserted there is that it is never more than that one
        // voxel out, and that it is exactly on the surface for the large majority of frames —
        // which is what separates a working ease from one that permanently trails the ground.
        //
        // Asserting equality on the ramp is what this test did when it was written, and the
        // ramp failed it at frame 80 by four tenths of a block: the ease was mid-step, which
        // is correct behaviour that a wrong assertion called a bug.
        let anchor = Vec3::new(16.0, 80.0, 16.0);
        let flat = terrain(anchor, CRITTER_RANGE + 8.0, palette::GRASS, |at| at.y < 64);
        let ramp = terrain(anchor, CRITTER_RANGE + 8.0, palette::GRASS, |at| {
            at.y < 64 + at.x.div_euclid(4)
        });
        let mut heights = HashSet::new();
        let mut exact = 0usize;
        let mut total = 0usize;
        for species in &CRITTERS {
            for seed in 0..8u64 {
                let seed = mix(seed, 0x5177);
                for (name, store) in [("flat", &flat), ("ramp", &ramp)] {
                    // Trunkless, so the whole walk is on the ground and every frame is a
                    // claim about the surface.
                    for (frame, step) in walked(
                        Some((store, CHUNK)),
                        species,
                        seed,
                        anchor,
                        None,
                        (species.forage_seconds() / DT) as usize,
                    )
                    .into_iter()
                    .enumerate()
                    .skip(SETTLED)
                    {
                        let (drawn, stand, _) = step.expect("solid ground places every critter");
                        let surface = match surface_under(store, drawn, anchor.y, CHUNK) {
                            Ground::Surface(surface) => surface,
                            other => panic!("{name} frame {frame}: the ground answered {other:?}"),
                        };
                        assert_eq!(drawn.y, stand, "{name} frame {frame} left the ground");
                        if name == "flat" {
                            assert_eq!(
                                stand, surface,
                                "{name} frame {frame}: stood at {stand} over {surface}"
                            );
                            continue;
                        }
                        assert!(
                            (stand - surface).abs() <= 1.0,
                            "{name} frame {frame}: stood at {stand} over {surface}"
                        );
                        exact += usize::from(stand == surface);
                        total += 1;
                        heights.insert(surface as i32);
                    }
                }
            }
        }
        assert!(
            heights.len() > 1,
            "the ramp never changed height under a critter, so this proves nothing"
        );
        assert!(
            exact * 10 >= total * 8,
            "only {exact} of {total} ramp frames were exactly on the ground"
        );
    }

    #[test]
    fn a_step_in_the_ground_never_teleports_a_critter() {
        // A cliff through the middle of the box. Crossing it a critter climbs the step, and
        // the whole reason the height is approached rather than assigned is that it must not
        // jump.
        let anchor = Vec3::new(16.0, 80.0, 16.0);
        let store = terrain(anchor, CRITTER_RANGE + 8.0, palette::GRASS, |at| {
            at.y < if at.x < 16 { 66 } else { 72 }
        });
        let mut stepped = 0usize;
        for species in &CRITTERS {
            for seed in 0..8u64 {
                let seed = mix(seed, 0xC11F);
                let path = walked(
                    Some((&store, CHUNK)),
                    species,
                    seed,
                    anchor,
                    None,
                    (species.forage_seconds() / DT) as usize,
                );
                for pair in path.windows(2) {
                    let (Some((was, before, _)), Some((now, after, _))) = (pair[0], pair[1]) else {
                        continue;
                    };
                    assert!(
                        (after - before).abs() <= STAND_STEP_SPEED * DT + 1e-4,
                        "{:?} snapped its footing from {before} to {after}",
                        species.gait
                    );
                    // What a player actually sees: the gait's own bound plus the footing's.
                    let moved = now.distance(was);
                    assert!(
                        moved <= (species.max_speed + STAND_STEP_SPEED) * DT + 1e-4,
                        "{:?} moved {moved} in {DT}s at a cliff edge",
                        species.gait
                    );
                    stepped += usize::from(after != before);
                }
            }
        }
        assert!(
            stepped > 0,
            "no critter ever met the step, so this test would pass vacuously"
        );
    }

    #[test]
    fn a_footing_approaches_its_target_and_then_sits_on_it() {
        // The server's `approach`, mirrored: no overshoot in either direction, and exact once
        // the target is within one step — which is what lets a settled critter stand on the
        // ground to the bit while it scurries over it.
        assert_eq!(approach(0.0, 1.0, 0.25), 0.25);
        assert_eq!(approach(0.9, 1.0, 0.25), 1.0);
        assert_eq!(approach(2.0, 1.0, 0.25), 1.75);
        assert_eq!(approach(1.1, 1.0, 0.25), 1.0);
        assert_eq!(approach(1.0, 1.0, 0.25), 1.0);
    }

    #[test]
    fn a_trunk_is_a_standing_log_near_the_critter_and_nothing_else() {
        let surface = 64.0;
        let anchor = Vec3::new(16.0, 80.0, 16.0);
        let reach = CRITTER_RANGE + 8.0;
        // A wood: a grass floor with one trunk standing in it a few blocks off, and a second
        // trunk far outside the search.
        let trunk_at = IVec3::new(21, 64, 18);
        let far_at = IVec3::new(21 + TRUNK_REACH as i32 + 6, 64, 18);
        let wood = |log: IVec3, height: std::ops::Range<i32>| {
            move |at: IVec3| {
                if at.x == log.x && at.z == log.z && height.contains(&(at.y - log.y)) {
                    palette::LOG
                } else if at.y < 64 {
                    palette::GRASS
                } else {
                    palette::AIR
                }
            }
        };
        let bare = blocks(anchor, reach, wood(trunk_at, 0..0));
        let standing = blocks(anchor, reach, wood(trunk_at, 1..6));
        let distant = blocks(anchor, reach, wood(far_at, 1..6));
        // A single log lying *on* the ground is a fallen branch rather than a trunk.
        let fallen = blocks(anchor, reach, wood(trunk_at, 1..2));

        let near = Vec3::new(18.5, 0.0, 17.5);
        let found = trunk_near(&standing, near, surface, CHUNK).expect("the trunk is in reach");
        assert_eq!(
            (found.x, found.z),
            (trunk_at.x as f32 + 0.5, trunk_at.z as f32 + 0.5),
            "the climb does not go up the middle of the trunk"
        );
        assert_eq!(found.y, surface, "the trunk's foot is not on the ground");
        // **The half of the contract `place` relies on**: the approach's speed is derived
        // from this reach, so a probe that answered further would break a bound two functions
        // away. Measured horizontally, because the foot is on the ground and the critter is
        // too.
        let reach = Vec3::new(found.x - near.x, 0.0, found.z - near.z).length();
        assert!(
            reach <= TRUNK_REACH,
            "the probe answered a trunk {reach} away, over its {TRUNK_REACH} reach"
        );
        // The nearest wins: a second trunk further out does not change the answer.
        let crowded = blocks(anchor, reach, |at| {
            match (wood(trunk_at, 1..6)(at), wood(far_at, 1..6)(at)) {
                (palette::LOG, _) | (_, palette::LOG) => palette::LOG,
                (block, _) => block,
            }
        });
        assert_eq!(trunk_near(&crowded, near, surface, CHUNK), Some(found));

        // Every shape of "no trunk" is the fallback branch, and each is reached.
        for (name, store) in [
            ("a wood with no trunk in it", &bare),
            ("a trunk out of reach", &distant),
            ("a log lying on the ground", &fallen),
        ] {
            assert_eq!(
                trunk_near(store, near, surface, CHUNK),
                None,
                "{name} answered a trunk"
            );
        }
        // And an unreadable store answers the same way rather than panicking.
        assert_eq!(
            trunk_near(&ChunkStore::default(), near, surface, CHUNK),
            None
        );
        assert_eq!(trunk_near(&standing, near, surface, 0), None);
        assert_eq!(trunk_near(&standing, Vec3::NAN, surface, CHUNK), None);
    }

    #[test]
    fn a_climbing_critter_rises_up_its_trunk_and_holds_its_column() {
        // The climb: the approach brings it to the trunk's column on the ground, and the rise
        // takes it up that column without moving sideways.
        let anchor = Vec3::new(16.0, 80.0, 16.0);
        let store = terrain(anchor, CRITTER_RANGE + 8.0, palette::GRASS, |at| at.y < 64);
        for species in CRITTERS.iter().filter(|row| row.climbs) {
            for seed in 0..8u64 {
                let seed = mix(seed, 0xC11B);
                let trunk = Vec3::new(anchor.x + 6.5, 64.0, anchor.z - 4.5);
                let path = walked(
                    Some((&store, CHUNK)),
                    species,
                    seed,
                    anchor,
                    Some(trunk),
                    life_frames(species),
                );
                let last = path
                    .last()
                    .and_then(|step| *step)
                    .expect("level ground places every critter");
                let (drawn, stand, rise) = last;
                assert_eq!(rise, CLIMB_RISE, "a climb ended {rise} of {CLIMB_RISE} up");
                assert_eq!(drawn.y, stand + rise, "the rise is not above the ground");
                assert!(
                    (drawn.x - trunk.x).abs() < 1e-3 && (drawn.z - trunk.z).abs() < 1e-3,
                    "a climb ended at {drawn}, not up the trunk at {trunk}"
                );
                // And it is still on the ground when the rise begins, so the approach is
                // walked rather than flown.
                let at_rise = place(
                    species,
                    seed,
                    species.forage_seconds() + species.climb_seconds() * CLIMB_APPROACH_SHARE,
                    anchor,
                    Some(trunk),
                );
                assert!(
                    (at_rise.x - trunk.x).abs() < 1e-3 && (at_rise.z - trunk.z).abs() < 1e-3,
                    "the rise began at {at_rise}, off the trunk at {trunk}"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Seeds
    // -----------------------------------------------------------------------

    #[test]
    fn the_group_size_and_the_coat_stay_inside_their_rows() {
        for (index, species) in CRITTERS.iter().enumerate() {
            let mut seen_sizes = [false; CRITTER_COUNT_MAX + 1];
            for cell in -400..400 {
                let seed = cell_seed(IVec3::new(cell, 4, cell * 3));
                let size = group_size(species, seed);
                assert!(
                    species.count.contains(&(size as u8)) && size <= CRITTER_COUNT_MAX,
                    "row {index} answered a group of {size}"
                );
                seen_sizes[size] = true;

                let critter = critter_seed(seed, 0, 0, 0);
                let pair = species.coat_at(species.coat_of(critter));
                let allowed = std::iter::once((species.body, species.tail))
                    .chain(species.coats.iter().copied());
                assert!(
                    allowed.into_iter().any(|known| known == pair),
                    "row {index} wore a colour that is not in its table"
                );
            }
            let range = usize::from(*species.count.start())..=usize::from(*species.count.end());
            for size in range {
                assert!(seen_sizes[size], "row {index} never answered {size}");
            }
        }
    }

    #[test]
    fn a_replacement_is_seeded_on_the_far_side_of_the_move() {
        // The whole point of the re-seed: a critter that appears must appear behind the
        // player's shoulder, never in the middle of the view they are walking into.
        let anchor = Vec3::new(64.0, 96.0, 64.0);
        let cell = cell_seed(IVec3::new(2, 3, 2));
        for generation in [0i64, 7, -3] {
            for (bias, axis) in [
                (Vec3::X, Vec3::X),
                (Vec3::NEG_X, Vec3::NEG_X),
                (Vec3::Z, Vec3::Z),
                (Vec3::new(-3.0, 7.0, -3.0), Vec3::new(-1.0, 0.0, -1.0)),
            ] {
                for slot in 0..CRITTER_COUNT_MAX {
                    let seed = seed_on_the_far_side(cell, slot, generation, anchor, bias * 32.0);
                    let far = |seed| {
                        let home = home_of(seed, anchor) - anchor;
                        Vec3::new(home.x, 0.0, home.z).dot(axis.normalize()) > 0.0
                    };
                    if far(seed) {
                        continue;
                    }
                    // The documented fallback, asserted rather than tolerated: it may only be
                    // reached when every salt in range was on the near side, and it may only
                    // ever answer the first seed.
                    assert_eq!(
                        seed,
                        critter_seed(cell, slot, generation, 0),
                        "fell back to a seed that is not the first"
                    );
                    assert!(
                        !(0..FAR_SIDE_TRIES)
                            .any(|salt| far(critter_seed(cell, slot, generation, salt))),
                        "fell back past a seed that was on the far side"
                    );
                }
            }
        }
        // No move, no bias, and the first seed is taken as it comes.
        assert_eq!(
            seed_on_the_far_side(cell, 0, 0, anchor, Vec3::ZERO),
            critter_seed(cell, 0, 0, 0)
        );
    }

    #[test]
    fn the_anchor_is_the_centre_of_the_cell_the_eye_is_in() {
        // Half a cell from the eye at worst, on every axis, including the negative side of the
        // origin where a truncating cast would have put the cell one too high.
        for eye in [
            Vec3::ZERO,
            Vec3::new(31.9, 0.1, -0.1),
            Vec3::new(-0.5, -33.0, -64.0),
            Vec3::new(1024.0, 96.0, -1024.0),
        ] {
            let anchor = anchor_of(cell_of(eye));
            assert!(
                (anchor - eye).abs().max_element() <= CRITTER_ANCHOR_CELL,
                "an eye at {eye} anchored at {anchor}"
            );
        }
        assert_eq!(cell_of(Vec3::new(-1.0, 0.0, 0.0)).x, -1);
        assert_eq!(cell_of(Vec3::new(32.0, 0.0, 0.0)).x, 1);
        assert_eq!(
            anchor_of(IVec3::ZERO),
            Vec3::splat(CRITTER_ANCHOR_CELL / 2.0),
            "the anchor is the cell's centre, not its corner"
        );
    }

    #[test]
    fn a_seed_is_mixed_from_the_constant_and_the_cell_and_the_generation() {
        // A neighbouring cell must not be a neighbouring seed: the groups would rhyme, and a
        // player walking a straight line would watch the same squirrels re-appear.
        let mut seen = Vec::new();
        for x in -8..8 {
            for z in -8..8 {
                seen.push(cell_seed(IVec3::new(x, 2, z)));
            }
        }
        let mut sorted = seen.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), seen.len(), "two cells share a seed");

        // And the constant is load-bearing: change it and every wood changes.
        assert_ne!(
            cell_seed(IVec3::ZERO),
            mix(CRITTER_SEED.wrapping_add(1), mix(0, mix(0, 0))),
        );

        // The generation is in the seed, which is the whole of how a life ends: the next
        // critter in a slot is a different animal rather than the same one restarted.
        let cell = cell_seed(IVec3::new(1, 2, 3));
        let generations: HashSet<u64> = (0..32i64)
            .map(|generation| critter_seed(cell, 0, generation, 0))
            .collect();
        assert_eq!(generations.len(), 32, "two lives shared a seed");
    }
    // -----------------------------------------------------------------------
    // The model
    // -----------------------------------------------------------------------

    /// One mesh's positions, its normals, and its triangles as index triples.
    fn geometry(mesh: &Mesh) -> (Vec<Vec3>, Vec<Vec3>, Vec<[usize; 3]>) {
        let read = |id: MeshVertexAttributeId| {
            mesh.attribute(id)
                .and_then(|values| values.as_float3())
                .expect("a critter mesh carries positions and normals")
                .iter()
                .map(|value| Vec3::from_array(*value))
                .collect::<Vec<_>>()
        };
        let Some(Indices::U32(indices)) = mesh.indices() else {
            panic!("a critter mesh is a U32 triangle list")
        };
        let triangles = indices
            .chunks_exact(3)
            .map(|triple| [triple[0] as usize, triple[1] as usize, triple[2] as usize])
            .collect();
        (
            read(Mesh::ATTRIBUTE_POSITION.id),
            read(Mesh::ATTRIBUTE_NORMAL.id),
            triangles,
        )
    }

    /// Every position in a mesh.
    fn points(mesh: &Mesh) -> Vec<Vec3> {
        geometry(mesh).0
    }

    #[test]
    fn every_face_of_a_critter_is_wound_outward() {
        // The failure `hands::BladeSection::perimeter` warns about and `birds.rs` made a
        // machine's problem, made one here too. A ring walked the wrong way round is a shell
        // lit entirely from the inside, and because the coat material draws both faces it does
        // not vanish to announce itself.
        //
        // Three properties settle it without anybody looking: every triangle's stored normal
        // agrees in direction with its own winding, the area vectors cancel (true of a closed
        // surface and of nothing else, so no cap was forgotten), and the volume that winding
        // encloses is positive — which is the whole of what "outward" means, and the one of
        // the three a mesh built inside out fails.
        for (name, mesh) in [("body", body_mesh()), ("tail", tail_mesh())] {
            let (positions, normals, triangles) = geometry(&mesh);
            let mut area = Vec3::ZERO;
            let mut volume = 0.0f32;
            for corners in triangles {
                let [a, b, c] = corners.map(|corner| positions[corner]);
                let cross = (b - a).cross(c - a);
                assert!(cross.length() > 1e-9, "{name} has a degenerate face at {a}");
                let wound = cross.normalize();
                for corner in corners {
                    let stored = normals[corner];
                    // **The sign is the property; the margin is a sanity bound.** A quad
                    // lofted between two sections of different shape is not planar, and
                    // `MeshBuild::quad` deliberately hands both its triangles the *diagonal*
                    // normal rather than either one's own — so the two can never agree
                    // exactly. Anything under 0.8 is a section table that has folded a quad
                    // over rather than tapered it.
                    assert!(
                        stored.dot(wound) > 0.8,
                        "{name}: a face stores {stored} where its winding gives {wound}"
                    );
                }
                area += cross;
                volume += a.cross(b).dot(c);
            }
            assert!(
                area.length() < 1e-4,
                "{name} is not a closed shell: its area vectors sum to {area}"
            );
            assert!(
                volume > 0.0,
                "{name} encloses {}, so its rings are wound inside out",
                volume / 6.0
            );
            // **The negative control, which `every_solid_in_the_sword_is_wound_outward`
            // established and `birds.rs`'s copy of this test does not have.** A signed volume
            // computed from the mesh and then asserted positive is proving the mesh with the
            // mesh: the same code would pass if `volume` were an absolute value, or if the
            // winding convention were the other one. Reversing every triangle must read
            // negative, and if it does not then this test is measuring nothing.
            let reversed: f32 = geometry(&mesh)
                .2
                .into_iter()
                .map(|corners| {
                    let [a, b, c] = corners.map(|corner| positions[corner]);
                    a.cross(c).dot(b)
                })
                .sum();
            assert!(
                reversed < 0.0,
                "{name} read {reversed} wound inside out, so the sign proves nothing"
            );
        }
    }

    #[test]
    fn the_model_is_authored_at_a_body_length_of_exactly_one() {
        // `CritterSpecies::size` is documented as the body length *and* used as the scale, so
        // the model has to be one long nose to rump, or every angle argued on `CRITTERS` is
        // wrong by a factor nobody wrote down.
        let body = points(&body_mesh());
        let nose = body.iter().fold(f32::INFINITY, |near, at| near.min(at.z));
        let rump = body.iter().fold(f32::NEG_INFINITY, |far, at| far.max(at.z));
        assert_eq!(rump - nose, 1.0, "the body is not one long nose to rump");
        // `-Z` is forward, so the nose has to be the far end of that.
        assert!(nose < -0.4 && rump > 0.4);
        // And it is an animal rather than a plank: narrow across, and taller at the haunches
        // than at the shoulders, which is the whole of the crouch.
        assert!(
            body.iter().all(|at| at.x.abs() <= 0.2),
            "the body is wider than it is a third long"
        );
        let tallest = |range: std::ops::RangeInclusive<f32>| {
            body.iter()
                .filter(|at| range.contains(&at.z))
                .fold(f32::NEG_INFINITY, |high, at| high.max(at.y))
        };
        assert!(
            tallest(0.1..=0.35) > tallest(-0.2..=-0.03),
            "the haunches are not above the shoulders, so it is not crouched"
        );
    }

    #[test]
    fn the_model_stands_on_its_own_origin() {
        // The one thing about this model that `birds.rs`'s does not have to be true of: a
        // critter is placed *on* a measured surface, so its feet are at `y = 0` in its own
        // units. Author it a hair above and every squirrel hovers; a hair below and every
        // squirrel is buried, at every scale, and nothing would say which.
        let body = points(&body_mesh());
        let lowest = body.iter().fold(f32::INFINITY, |low, at| low.min(at.y));
        assert_eq!(lowest, 0.0, "the body does not rest on y = 0");
        let highest = body
            .iter()
            .fold(f32::NEG_INFINITY, |high, at| high.max(at.y));
        assert!(
            (0.2..0.45).contains(&highest),
            "a critter {highest} tall for a body one long is not a squirrel"
        );
    }

    #[test]
    fn a_tail_is_bushy_rather_than_a_rod() {
        // The half of the silhouette that says "squirrel". A tail that is quietly tidied into
        // a cylinder is a rat's, and nothing about a cylinder would fail any other test here.
        let sections = tail_sections();
        let root = sections[0].half_width;
        let widest = sections
            .iter()
            .fold(0.0f32, |wide, section| wide.max(section.half_width));
        assert!(
            widest >= root * 2.0,
            "a tail {widest} at its widest and {root} at its root is a rod"
        );
        assert!(
            sections.last().expect("a tail has sections").half_width < widest,
            "a tail that never tapers is a club"
        );
        // As tall as it is wide, rather than flat: a bird's tail is a flat spread and a
        // squirrel's is a plume.
        for section in sections {
            assert!(
                section.half_height >= section.half_width,
                "a tail section {} wide and {} tall is flat",
                section.half_width,
                section.half_height
            );
        }
        // And it is rooted on the body rather than floating behind it.
        let body = points(&body_mesh());
        let rump = body.iter().fold(f32::NEG_INFINITY, |far, at| far.max(at.z));
        assert!(
            TAIL_ROOT.z < rump && TAIL_ROOT.z > rump - 0.2,
            "the tail is rooted at {} on a body ending at {rump}",
            TAIL_ROOT.z
        );
    }

    #[test]
    fn a_tail_is_held_over_the_back_and_flicks_about_that_rest() {
        // The pose, which is most of what reads as a squirrel: the tail is up over the back
        // and twitches, rather than trailing behind and wagging.
        let tail = CritterTail { flick_hz: 1.4 };
        let tip = Vec3::new(0.0, 0.0, tail_sections()[4].z);
        let mut highest = f32::NEG_INFINITY;
        let mut lowest = f32::INFINITY;
        for step in 0..=64u32 {
            let at = tail_turn(&tail, step as f32 / 8.0) * tip;
            assert!(
                at.y > tip.z * 0.5,
                "the tail fell to {at} instead of staying over the back"
            );
            highest = highest.max(at.y);
            lowest = lowest.min(at.y);
        }
        assert!(
            highest - lowest > 0.02,
            "nothing ever flicked: {lowest} to {highest}"
        );
        // That the rest is an arch rather than level, and the flick a twitch rather than a
        // sweep, is asserted at the two constants themselves where the compiler checks it —
        // see the `const _` pair beside them. What is measured *here* is the thing those two
        // numbers are for: where the tip actually ends up once the rotation is applied.
    }

    #[test]
    fn the_drawn_critter_stays_inside_its_horizontal_box() {
        // `a_critter_never_leaves_its_horizontal_box` is about where `place` puts a critter's
        // **origin**, and `place` reads no part of `CritterSpecies::size` — so it would pass
        // with a squirrel the size of a hill. This is the half the size moves: the tail tip,
        // not the origin.
        let mut reach = Vec3::ZERO;
        for point in points(&body_mesh()) {
            reach = reach.max(point.abs());
        }
        // The tail wherever the flick takes it, rooted where it is rooted.
        let tail = points(&tail_mesh());
        for step in 0..=32u32 {
            let turn = tail_turn(&CritterTail { flick_hz: 1.0 }, step as f32 / 4.0);
            for point in &tail {
                reach = reach.max((TAIL_ROOT + turn * *point).abs());
            }
        }
        let anchor = Vec3::new(-512.0, 64.0, 512.0);
        for species in &CRITTERS {
            // A whole turned critter is at most its longest axis from its origin, whichever
            // way `look_to` has it facing.
            let half = reach.max_element() * species.size;
            for seed in 0..16u64 {
                let seed = mix(seed, 0xB0A7);
                for frame in 0..=life_frames(species) {
                    let at = place(species, seed, frame as f32 * DT, anchor, None) - anchor;
                    let drawn = Vec3::new(at.x, 0.0, at.z).abs() + Vec3::splat(half);
                    assert!(
                        drawn.max_element() <= CRITTER_RANGE,
                        "{:?} drew out to {drawn} from its anchor",
                        species.gait
                    );
                }
            }
        }
    }
}
