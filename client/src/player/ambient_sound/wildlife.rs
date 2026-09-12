//! Which creature is heard where, and when — as a table rather than as an expression.
//!
//! One row per voice. A row says what the call is, what makes it present, which half of the
//! day it belongs to, and which stream of bearings and intervals is its own. Nothing about a
//! species lives outside [`WILDLIFE`] and the description `sounds.rs` holds for its call, so
//! **adding a species is a row**: the update loop, the gain bookkeeping and `targets()` are
//! all written against the table's length and never against a particular creature.
//!
//! Nothing here is a biome, a climate or a gameplay fact. A voice is chosen from the look of
//! the loaded ground and the sky clock, exactly as before — see `ambience.rs` for why that
//! distinction is load-bearing.
//!
//! # Where a creature's voice comes from
//!
//! **The rule, because six later species each need the same answer and must not each invent
//! one:**
//!
//! - **A voice that belongs to a creature the eye can see is placed at that creature.** If
//!   the squirrel chattering is a squirrel the player is looking at, the sound comes from
//!   its body, moves when it moves, and is occluded by what stands between. A voice arriving
//!   from a bearing while its owner is visibly elsewhere reads as a bug in the world, not as
//!   ambience.
//! - **A voice that belongs to nothing visible keeps the bearing-on-a-circle placement**
//!   [`super::controller::Calls`] gives it: a random bearing at the row's `radius` and
//!   `height`, re-chosen for each call, anchored for that call's short life. A rattlesnake
//!   nobody sees is somewhere over there, and that is the whole truth about it.
//! - **Which of the two applies is a property of the creature, not of the frame.** A species
//!   that is sometimes drawn and sometimes not — the macaw is the one that exists today — is
//!   placed at a body when one is there to place it at, and falls back to the bearing when
//!   the flock is out of range or has not spawned. Silence is not the fallback: a voice with
//!   no body is still ambience.
//!
//! Today every row is on the second half of that rule, the macaw included: its call has come
//! from a bearing since it was written, unrelated to where any macaw is
//! (`controller.rs`'s `Calls::update`). Moving it to the first half is a change to where a
//! shipped sound comes from and belongs to the issue that adds a creature which needs it, not
//! to the refactor that wrote the rule down.

use super::sounds::Call;
use crate::player::ambience::{Ambience, GroundLook};
use crate::player::birds;
use crate::player::sky::Period;

/// What makes a voice present where the eye is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Habitat {
    /// The country's ground look alone, wooded or not. A creature with no body needs no
    /// more than this: the rattlesnake is heard over sand because the ground is sand.
    Ground(GroundLook),
    /// Exactly where [`birds::species_for`] answers one row of [`birds::BIRDS`].
    ///
    /// **For a creature that is seen as well as heard**, whose gate is the table that draws
    /// it rather than the ground alone — the macaw needs trees, and the bird table is where
    /// that is already written down. #1176 is what happens when the sound lane keeps its own
    /// opinion about where a visible species lives: the day call played on every sunny plain.
    Flock(usize),
}

impl Habitat {
    /// Whether this habitat is where the eye is.
    fn present(self, ambience: &Ambience) -> bool {
        match self {
            Self::Ground(ground) => ambience.ground == ground,
            Self::Flock(species) => birds::species_for(ambience) == Some(species),
        }
    }
}

/// One row of [`WILDLIFE`]: one voice, and everything about where and when it is heard.
#[derive(Debug)]
pub(super) struct Voice {
    /// The call itself. Its interval, radius, height, length and range come from
    /// [`Call::profile`]; this table decides nothing about how a voice sounds.
    pub(super) call: Call,
    /// Where it is heard.
    pub(super) habitat: Habitat,
    /// Which half of the day it is heard in. Crossfaded over the twilight by
    /// [`Period::share`], so a country's day voice and its night voice cross rather than
    /// leaving a silent gap.
    pub(super) period: Period,
    /// The salt that makes this row's bearings and intervals its own stream.
    ///
    /// **Data rather than the row's position**, for two reasons. It lets a row be appended,
    /// moved or split without moving a single call that ships today — which is what let the
    /// macaw's bespoke lane fold into this table bit-for-bit, since its lane was seeded with
    /// the world seed unsalted and says so here with a `0`. And it makes "two creatures
    /// calling at dusk must not share a bearing" a property of the row rather than an index
    /// arithmetic in the update loop.
    ///
    /// It is what makes the *seeds* position-independent, and nothing more: the table's order
    /// is still load-bearing for slot arbitration, which is the paragraph on [`WILDLIFE`].
    ///
    /// A new row takes a salt no other row uses;
    /// `every_voice_has_its_own_stream_and_agrees_with_the_flock_it_belongs_to` is what holds
    /// that.
    pub(super) stream: u64,
}

/// Every ambient voice there is, by country and by half of the day.
///
/// |  | day | night |
/// |---|---|---|
/// | sand | rattlesnake | crow |
/// | snow | eagle | wolf |
/// | grass | macaw, where the bird table flies it | cricket |
///
/// `GroundLook::Unknown` is deliberately absent and so is the open plain's day: "not enough
/// loaded evidence" is silence, exactly as it is an empty sky in [`birds::BIRDS`], rather
/// than a default creature.
///
/// ## The order is the claim order, and it is load-bearing
///
/// The update loop walks this table in order and each lane claims its own mixer slot, so the
/// order decides who wins a contested one. That is not hypothetical arbitration: `Music`,
/// `Sfx` and `Ambience` hold at most `MAX_SOURCES - VOICE_RESERVE` (8 of 16) between them,
/// and `Bus::steal_order` ranks `Ambience` lowest with stealing "strictly beneath" the
/// claimant — so an ambience lane can never steal from a peer. When the world's slots are
/// full the call that asks first is heard and the next one is **dropped, not queued**
/// (`audio/mixer.rs`).
///
/// So the rows are ordered **seen-and-heard first**: every [`Habitat::Flock`] row precedes
/// every [`Habitat::Ground`] row, and
/// `a_creature_that_can_be_seen_claims_its_slot_before_one_that_cannot` holds it. A voice
/// falling silent while the player is watching the animal that owns it is a worse failure
/// than an off-screen call going unheard — which is the same reasoning as the origin rule at
/// the top of this file. It is also the order that shipped, so no contested slot changes
/// hands: the macaw's lane was updated before the five ground lanes and still is.
///
/// A new species that is drawn as well as heard belongs with the macaw, above the ground-only
/// rows. `pins.rs` renders in this order too; seeds do not depend on it ([`Voice::stream`]).
pub(super) const WILDLIFE: [Voice; 6] = [
    // The macaw, heard by day exactly where the bird table flies it: wooded grass. An open
    // plain has no species and another country has another one, and neither hosts the call
    // (#1176). It had a lane of its own until the table could hold a habitat that is not the
    // ground's; the `0` salt is that lane's seed, kept so not one squawk moved.
    Voice {
        call: Call::Parrot,
        habitat: Habitat::Flock(PARROT),
        period: Period::Day,
        stream: 0,
    },
    Voice {
        call: Call::Rattlesnake,
        habitat: Habitat::Ground(GroundLook::Sand),
        period: Period::Day,
        stream: 0x9860,
    },
    Voice {
        call: Call::Crow,
        habitat: Habitat::Ground(GroundLook::Sand),
        period: Period::Night,
        stream: 0x9861,
    },
    Voice {
        call: Call::Eagle,
        habitat: Habitat::Ground(GroundLook::Snow),
        period: Period::Day,
        stream: 0x9862,
    },
    Voice {
        call: Call::Wolf,
        habitat: Habitat::Ground(GroundLook::Snow),
        period: Period::Night,
        stream: 0x9863,
    },
    // The same green-ground night the cricket bed was gated on, now a sparse call.
    Voice {
        call: Call::Cricket,
        habitat: Habitat::Ground(GroundLook::Grass),
        period: Period::Night,
        stream: 0x9864,
    },
];

/// The macaw's row in [`birds::BIRDS`], which is appended to and never reordered.
pub(super) const PARROT: usize = 0;

impl Voice {
    /// How loudly this voice belongs where the eye is, right now: its habitat's yes or no
    /// times its period's share of the day.
    ///
    /// The whole of the country × hour mapping that used to be an inline expression per lane.
    pub(super) fn gain(&self, ambience: &Ambience, night: f32) -> f32 {
        f32::from(u8::from(self.habitat.present(ambience))) * self.period.share(night)
    }
}

/// Which row carries one call, for a caller that names a species rather than a lane.
///
/// Test-only on purpose: nothing that runs looks a voice up by its creature — the update
/// loop walks the table — so this exists to let the tests ask "where is the cricket heard"
/// without pinning the cricket to a position in the table.
#[cfg(test)]
pub(super) fn row_of(call: Call) -> usize {
    WILDLIFE
        .iter()
        .position(|voice| voice.call == call)
        .expect("every call has a row in WILDLIFE")
}
