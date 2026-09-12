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
//!   once and never again — the same category of spawn-time constant as `Critter::anchor` (part two).
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
//! ## What is here, and what arrives with the parts after it
//!
//! **This is part one of four, and each part answers one question.** This one answers *where a
//! critter is*: the species table, the path as a pure function of a seed and the clock, the
//! life window, and the seeds. It reads nothing — there is no `ChunkStore` import in this file
//! and no `use` of `palette` — so every claim below is settled by arithmetic, and every
//! function is exercised by this module's own tests rather than by anything that runs.
//!
//! - **Part two: what a critter stands on.** `surface_under`, `next_stand` and `trunk_near` —
//!   the terrain probe and the trunk search, and with them this module's first read of the
//!   world.
//! - **Part three: what draws it.** `CritterVisuals`, the `Critter` and `CritterTail`
//!   components, the body and tail lofted in code, and the `keep_the_critters` /
//!   `run_the_critters` pair that `player/mod.rs` registers.
//! - **Part four: what it sounds like.** The squirrel's chatter, its row in the wildlife table,
//!   and the rule that places a visible creature's voice at its body.
//!
//! Until the systems land, the items here have no caller in a shipped build and carry
//! `#[allow(dead_code)]` for exactly that reason, in the form this repository already uses for
//! a contract that ships before its consumer — see `net/codec.rs`, whose outbound intent
//! builders carry the same allowance with the same kind of comment. **Part three removes it**,
//! and nothing else may: an item still unused once the systems exist is one nobody needed.
//!
//! The seams are boundaries the code already draws rather than character counts — but the
//! counts are why there are four of them. Whole, this issue measured about 170,000 characters
//! against a 90,000 review cap, and a truncated review is one that comes back having read
//! neither half.

// **Removed by part two, and only by part two.** Nothing in this half has a caller in a
// shipped build yet: the systems that would call it are what part two adds. It is scoped to
// this module and to this one lint, which is the form `client/AGENTS.md` asks for when a lint
// has to be silenced at all — never a wider set and never workspace-wide — and it is the same
// bargain `net/codec.rs` strikes for an outbound contract that ships before its consumer.
//
// It is a blanket rather than forty-odd attributes because *every* item here is in that
// position, and forty copies of one comment would say less than this one does. The risk it
// carries is the honest one: while it is here, genuinely dead code would not be reported
// either. That is bounded by its lifetime — part two deletes this line and the compiler then
// has an opinion about every item below.
#![allow(dead_code)]

use std::f32::consts::TAU;
use std::ops::RangeInclusive;

use bevy::prelude::*;

use super::ambience::{Ambience, GroundLook};
use super::sky::Period;
use crate::net::{BlockCoord, ChunkCoord};
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

/// Every kind of critter there is. Appended to, never reordered: `Critter::species` (part two) is an
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
/// `Critter::trunk` (part two) — so it may read more columns than a per-frame probe could afford, and
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::world::{BlockId, VoxelChunk};

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
}
