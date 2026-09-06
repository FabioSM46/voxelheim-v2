//! The mixer: what the output callback runs, and the only thing it is allowed to run.
//!
//! **Everything in this file is written to the rule in the module doc above it**: the
//! callback allocates nothing, locks nothing, logs nothing and touches no Bevy type. That
//! is not a style preference. The callback is a real-time thread the operating system will
//! not wait for — a `malloc` that happens to take a lock somebody else holds, or a `Mutex`
//! a Bevy system is sitting on for one frame, is an underrun a player hears as a click.
//!
//! Three decisions carry it, and each removes one of those temptations rather than
//! documenting it away:
//!
//! - **The sample buffers are `[AtomicU32]`, not a locked or `unsafe`-shared slice.** A
//!   single-producer single-consumer ring wants interior mutability shared across two
//!   threads, and there are exactly three ways to get it: a lock (forbidden here), an
//!   `UnsafeCell` (this client contains no hand-written `unsafe` — see `client/AGENTS.md`),
//!   or a slice of atomics. The third is the one that is both safe and lock-free, and an
//!   `f32` fits an `AtomicU32` exactly through [`f32::to_bits`]. The allocation happens
//!   once, in [`Ring::new`], on whichever thread built the mixer.
//! - **The gains are atomics too.** A Bevy system stores; the callback loads. There is no
//!   frame on which the two can wait for each other.
//! - **A source that has nothing to say renders as silence.** [`Ring::pop`] answers `None`
//!   on an underrun and [`Mixer::render`] reads that as `0.0`, so a starved source is
//!   quiet rather than repeating whatever was last in the buffer.
//!
//! **The bus arithmetic, stated once.** A source is claimed onto a bus and stays there.
//! `Voice`, `Music`, `Sfx` and `Ambience` each have a gain of their own, applied to the
//! sources on them; `Master` is the output stage, so its gain applies to the sum of
//! everything after the per-bus gains. A source claimed directly onto `Master` — the tone
//! test is the first — is therefore scaled once, by the master gain, and never twice.
//!
//! ```text
//!   out = master * ( voice     * voice_sources
//!                  + duck * music * music_sources
//!                  + sfx       * sfx_sources
//!                  + duck * ambience * ambience_sources
//!                  +             master_sources )
//! ```
//!
//! **Three of those buses arrived empty, on purpose.** `Music`, `Sfx` and `Ambience` are
//! claimed by nothing in this client yet: #982 built the buses, their gains and the policy
//! below so that the sounds arriving after it find a slot and a knob already there. A bus
//! nothing feeds renders nothing and costs nothing — [`Mixer::render`]'s per-slot work is
//! skipped for every slot that is not live, which is all of theirs.
//!
//! **`duck` is one multiplier, on two buses, and it is exactly `1.0` when nobody is
//! speaking.** A Bevy system writes a *target* through [`Mixer::set_duck`] and the render
//! path walks towards it by exactly one block's worth of time — the shape the occlusion ramp
//! already has, for its reason: a gain that moved with the frame rate of whatever was looking
//! at the world would step audibly on a long frame. At a target of `1.0` the multiply is the
//! identity, so a player who has turned ducking off hears music this stage has not touched
//! rather than music it has approximately restored.
//!
//! **The source budget is a policy and it is written down at [`MAX_SOURCES`].** Sixteen
//! slots, one pool, shared by voice and by everything else — so "who gets the last one" has
//! an answer rather than being a race between whoever asks first.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use super::spatial::{self, BANDS, HIGH_CROSSOVER_HZ, LOW_CROSSOVER_HZ, PanGains, Placement};

/// How many sources may feed the mixer at once.
///
/// A fixed count, because the alternative is a `Vec` the callback would walk while a Bevy
/// system reallocated it — and because every per-block scratch array in [`Mixer::render`] is
/// sized from it on the stack, which is what keeps the render path allocation-free now that
/// there is per-source state to prepare.
///
/// **Sixteen since #854, and the three that are spoken for are what the number is set
/// from.** The tone test holds one for the client's life, the local voice monitor holds
/// another, and `audio/heard.rs` holds a third for every speaker it could not give a slot of
/// its own — so thirteen people can be heard from where they are standing, and a fourteenth
/// is heard unpositioned rather than not at all. Raising it costs
/// [`SOURCE_CAPACITY`] × 4 bytes per slot and nothing else.
///
/// ## The allocation policy, decided at #982 while nothing was feeding the new buses
///
/// One pool of sixteen, and a bed of ambience must never be the reason somebody's friend
/// cannot be heard. Three rules, each enforced by [`Mixer::claim`] rather than asked of its
/// callers:
///
/// 1. **Which bus may take the last slot: `Voice` and `Master`, and nothing else.**
///    `Music`, `Sfx` and `Ambience` may hold at most `MAX_SOURCES - `[`VOICE_RESERVE`]
///    between them, so voice can always obtain [`VOICE_RESERVE`] slots however busy the world
///    is. `Voice` is exempt because it is what the reserve is for.
///
///    **That is a count of what the world *holds*, not of what happens to be free, and the
///    review on #996 is why.** The first version counted free slots and then won one, which
///    is two steps: two claims that both read one more than the reserve as free both
///    succeeded, and voice was left short of the reserve that exists to stop exactly that. A
///    per-slot compare-exchange cannot enforce a pool-wide rule, so the budget is one atomic
///    location — [`Mixer::world_sources`] — moved by a single `fetch_update`.
///    [`Bus::may_take_the_last_slot`] is the predicate, and `Master` is exempt with it: the
///    two claims ever made on it are the tone test and the loopback monitor, each taken once
///    at startup, before anything else exists to be starved.
/// 2. **What is stolen when none is free: ambience first, then music, then effects — and
///    only ever by a bus above the victim.** [`Bus::steal_order`] is that ranking and
///    [`Mixer::claim`] walks it. Stealing is the mixer's own act, not a caller's: a
///    `SourceHandle` whose slot was taken goes inert the instant it is, which is what
///    [`Source::slot`]'s generation is for.
/// 3. **What may never be stolen: `Voice` and `Master`.** [`Bus::steal_order`] answers
///    `None` for both, so no arrangement of the other three can reach a slot somebody is
///    being heard through. That is the acceptance criterion this policy exists for.
///
/// **The sound that triggers a steal does not get the slot, and is dropped rather than
/// queued.** The one-shot that asked first is simply not heard; that is the cheaper half of
/// the trade, because a one-shot nobody hears costs one sound and a queue of them costs a
/// sound arriving after the thing that caused it.
///
/// **What a steal does is [`SlotState::Revoked`], and the review on #996 is why.** Taking a
/// slot away used to mark it `Dirty`, which made it reusable as soon as the callback had
/// cleared it — while the previous owner still held a [`SourceHandle`] it might be part-way
/// through pushing into. `live()` is one load and `push` is another, so a producer could pass
/// the check and then write into the ring of whoever had since been given the slot: two
/// producers on a ring whose whole memory ordering assumes one. A revoked slot is instead
/// silent immediately and **still its owner's**, and only that owner's `Drop` moves it on. So
/// a revocation is heard at once and the slot comes back a frame later rather than a block
/// later, from a producer that noticed. The cost is stated at [`SlotState::Revoked`]: a
/// producer that never runs again never returns its slot.
pub const MAX_SOURCES: usize = 16;

/// How many of the [`MAX_SOURCES`] slots are held back from the world's own sounds.
///
/// **Eight, which is half, and it is set from what the other half has to hold.** The buses a
/// world feeds can plausibly want an ambience bed, weather, a fire, a forge and a handful of
/// one-shots — seven — so eight is that list with a slot to spare, and it leaves eight for
/// voice: seven speakers heard from where they are standing plus the shared source behind
/// them, on a client whose tone test and loopback monitor have taken their two.
///
/// Lower and a busy scene starves a conversation, which is the failure this exists to
/// prevent; higher and the world falls silent while a slot nobody is speaking through is kept
/// warm. The number is a policy rather than a measurement, and it is the one this file states
/// so that the sounds arriving later are written against it.
pub const VOICE_RESERVE: usize = 8;

/// How long the duck takes to reach full depth, in seconds.
///
/// **Fast, because it is answering somebody who has already started talking.** 80 ms is under
/// a syllable, so the first word is heard over a bed that is already getting out of the way,
/// and it is long enough not to be a step: a bed cut instantly is a click.
pub const DUCK_ATTACK_SECONDS: f32 = 0.080;

/// And how long it takes to come back, in seconds.
///
/// **Five times the attack, which is the ordinary asymmetry of every duck.** Music that
/// jumped back between two words would pump audibly, and 400 ms is comfortably longer than
/// the gap inside a sentence while being shorter than the second `SPEAKING_FOR` already holds
/// a speaker for — so the release starts from a state that has already outlasted the pause.
///
/// [`SPEAKING_FOR`]: super::SPEAKING_FOR
pub const DUCK_RELEASE_SECONDS: f32 = 0.400;

const _: () = {
    // The reserve is a real one in both directions: it leaves the world's own sounds strictly
    // fewer slots than the pool has, and it leaves voice strictly more than none. A reserve of
    // zero would silently be no policy at all, and one of `MAX_SOURCES` would mean no sound
    // outside voice could ever be heard — both of which build, and neither of which is a state
    // any test would be looking for.
    assert!(
        VOICE_RESERVE > 0,
        "the reserve holds nothing back for voice"
    );
    assert!(
        VOICE_RESERVE < MAX_SOURCES,
        "the reserve leaves the world's sounds no slots at all"
    );
    // And the duck comes back more slowly than it goes, which is the asymmetry that stops a
    // bed pumping between two words rather than a pair of numbers that happen to differ.
    assert!(DUCK_ATTACK_SECONDS > 0.0);
    assert!(DUCK_RELEASE_SECONDS > DUCK_ATTACK_SECONDS);
};

/// How many mono samples one source may hold before its producer is refused.
///
/// A quarter of a second at 48 kHz. Long enough that a Bevy system running at 60 Hz can
/// stay ahead of the callback with several frames of slack, short enough that a source
/// which stops being fed goes quiet promptly rather than playing a stale quarter-second.
pub const SOURCE_CAPACITY: usize = 12_000;

/// The sample rate assumed before a device has said what its own is.
pub const DEFAULT_SAMPLE_RATE: u32 = 48_000;

/// Where a source is mixed, and therefore which gain reaches it.
///
/// **Five, and the three that are not `Voice` or `Master` were appended rather than
/// inserted.** `Voice` keeps index 0 and `Master` index 1 because those are the two the rest
/// of this client already claims onto, and moving them would have been a rewrite of every
/// call site to buy nothing. The order of [`Self::ALL`] is therefore the order the gains are
/// stored in and not the order the settings screen lists them, which is the screen's business
/// and not this file's.
///
/// **Every `match` on this enum is wildcard-free except [`Self::from_index`]**, whose one
/// wildcard is argued at its own doc comment. A sixth bus has to say what its gain does, what
/// it may steal and whether it ducks before this file compiles — which is the whole reason
/// those three are predicates on the enum rather than lists somewhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bus {
    /// Proximity voice. Scaled by its own gain, then by the master.
    Voice,
    /// The output stage. A source claimed here is scaled by the master gain alone.
    Master,
    /// Generated music. Scaled by its own gain and by the duck, then by the master.
    ///
    /// **Empty in this client**, and deliberately: the generator is a later arc, and this
    /// exists so that when it lands it finds its slot, its gain and its on/off already built
    /// and already persisted.
    Music,
    /// The world's one-shots — a hit, a footstep, a door. Scaled by its own gain, then by the
    /// master, and never ducked: an effect is the thing a listener is being told about, and a
    /// duck would hide the sound that a speaker is probably talking *about*.
    Sfx,
    /// The bed under the world — weather, a fire, a forge. Scaled by its own gain and by the
    /// duck, then by the master.
    Ambience,
}

impl Bus {
    /// Every bus, in the order [`Mixer`] stores their gains.
    ///
    /// **A hand-written list, because no stable Rust enumerates variants** — so it does not
    /// enforce itself and a sixth member added to the enum will not appear here on its own.
    /// What holds it is `every_bus_is_named_in_all_exactly_once` below, which walks this list
    /// against the index each member reports.
    pub const ALL: [Self; 5] = [
        Self::Voice,
        Self::Master,
        Self::Music,
        Self::Sfx,
        Self::Ambience,
    ];

    /// This bus's index into the gain array.
    const fn index(self) -> usize {
        match self {
            Self::Voice => 0,
            Self::Master => 1,
            Self::Music => 2,
            Self::Sfx => 3,
            Self::Ambience => 4,
        }
    }

    /// The bus `index` names, or [`Self::Master`] for anything else.
    ///
    /// Total rather than fallible because the callback reads it: the only writer is
    /// [`Mixer::claim`], which writes [`Self::index`], so an out-of-range value is
    /// unreachable — and the callback is no place to answer an unreachable state with a
    /// panic. `Master` is the fallback because a source heard once at the wrong gain is a
    /// better answer than one that is silently dropped.
    const fn from_index(index: u8) -> Self {
        match index {
            0 => Self::Voice,
            2 => Self::Music,
            3 => Self::Sfx,
            4 => Self::Ambience,
            _ => Self::Master,
        }
    }

    /// Whether this bus's gain is taken down while somebody is speaking nearby.
    ///
    /// **Music and ambience, and nothing else.** Both are beds: they are there to be under
    /// something, so taking them down is the whole of what makes a voice audible over them.
    /// Effects are not, for the reason at [`Self::Sfx`], and neither `Voice` nor `Master`
    /// could be without ducking the voice that caused the duck.
    pub const fn ducks(self) -> bool {
        match self {
            Self::Music | Self::Ambience => true,
            Self::Voice | Self::Master | Self::Sfx => false,
        }
    }

    /// Whether a claim on this bus may take a slot that [`VOICE_RESERVE`] holds back.
    ///
    /// See the policy at [`MAX_SOURCES`]: `Voice` may because the reserve is for it, and
    /// `Master` may because its two claims are made once at startup.
    pub const fn may_take_the_last_slot(self) -> bool {
        match self {
            Self::Voice | Self::Master => true,
            Self::Music | Self::Sfx | Self::Ambience => false,
        }
    }

    /// Where this bus stands in the order a live source is taken away, lowest first, or
    /// `None` for a bus that is never stolen from.
    ///
    /// **`Voice` and `Master` answer `None`, and that is the acceptance criterion.** No
    /// arrangement of the other three can reach a slot somebody is being heard through,
    /// because [`Mixer::claim`] only ever considers a victim this function ranks.
    ///
    /// Ambience is taken first because it is the sound a listener is least likely to be able
    /// to name; music second; an effect last, since by the time one is playing it is telling
    /// somebody what just happened to them.
    pub const fn steal_order(self) -> Option<u8> {
        match self {
            Self::Ambience => Some(0),
            Self::Music => Some(1),
            Self::Sfx => Some(2),
            Self::Voice | Self::Master => None,
        }
    }
}

/// Somewhere rendered audio goes.
///
/// The one seam that makes this module testable: the output callback hands the mixer the
/// device's buffer, and a test hands it a `Vec<f32>`. Nothing in [`Mixer::render`] knows
/// which it got, so every assertion below runs with no device open — which is the whole of
/// how the acceptance criterion "no test opens a real device" is met.
///
/// `device.rs` is the implementation that matters: it wraps the buffer `cpal` hands the
/// output callback, so the device's own memory *is* the sink and the render path copies
/// nothing on its way out.
pub trait Sink {
    /// The interleaved block to fill, `channels` samples per frame.
    fn block(&mut self) -> &mut [f32];
}

/// A fixed-capacity single-producer single-consumer ring of mono samples.
///
/// One producer and one consumer, and which thread is which depends on the direction. On the
/// way out a Bevy system produces through [`SourceHandle`] and the output callback consumes
/// through [`Mixer::render`]; on the way in `audio/device.rs`'s capture callback produces and
/// a Bevy system consumes. The indices only ever increase; the modulo is taken at access, so
/// "empty" and "full" are told apart by the difference rather than by a spare slot.
///
/// **`pub(super)` because there is one ring in this module and not two.** The capture side
/// needs the same lock-free, allocation-free structure for the same reason — a callback the
/// operating system will not wait for — and a second copy of it would be a second place for
/// the memory ordering to be subtly different.
#[derive(Debug)]
pub(super) struct Ring {
    /// `f32` bits. See the module doc for why this is a slice of atomics.
    data: Box<[AtomicU32]>,
    /// What the consumer has taken. Written by the consumer only.
    read: AtomicUsize,
    /// What the producer has written. Written by the producer only.
    written: AtomicUsize,
}

impl Ring {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            data: (0..capacity.max(1)).map(|_| AtomicU32::new(0)).collect(),
            read: AtomicUsize::new(0),
            written: AtomicUsize::new(0),
        }
    }

    /// How many samples are waiting.
    pub(super) fn len(&self) -> usize {
        self.written
            .load(Ordering::Acquire)
            .wrapping_sub(self.read.load(Ordering::Acquire))
    }

    /// How many more samples [`Self::push`] would accept.
    pub(super) fn free(&self) -> usize {
        self.data.len() - self.len().min(self.data.len())
    }

    /// Appends as much of `samples` as fits, and answers how much that was.
    ///
    /// **Refuses rather than overwrites.** A full ring means the consumer is behind; the
    /// producer dropping the newest samples costs the tail of a sound, where overwriting
    /// the oldest would tear the middle of one that is already being played.
    pub(super) fn push(&self, samples: &[f32]) -> usize {
        let mut written = self.written.load(Ordering::Relaxed);
        let taken = samples.len().min(self.free());
        for sample in &samples[..taken] {
            self.data[written % self.data.len()].store(sample.to_bits(), Ordering::Relaxed);
            written = written.wrapping_add(1);
        }
        // Release, so the samples above are visible to the consumer before the index that
        // admits they exist.
        self.written.store(written, Ordering::Release);
        taken
    }

    /// Takes the next sample, or `None` when there is none.
    pub(super) fn pop(&self) -> Option<f32> {
        let read = self.read.load(Ordering::Relaxed);
        if read == self.written.load(Ordering::Acquire) {
            return None;
        }
        let bits = self.data[read % self.data.len()].load(Ordering::Relaxed);
        self.read.store(read.wrapping_add(1), Ordering::Release);
        Some(f32::from_bits(bits))
    }

    /// Moves everything waiting onto the end of `out`, and answers how much that was.
    ///
    /// The consumer's counterpart to [`Self::push`], and the shape the capture side wants: a
    /// Bevy system draining a callback's output every frame asks "everything you have" rather
    /// than one sample at a time. `out` is the caller's buffer and is never cleared here, for
    /// [`Resampler::resample`]'s reason — the caller is accumulating towards a frame.
    ///
    /// [`Resampler::resample`]: super::dsp::Resampler::resample
    pub(super) fn drain_into(&self, out: &mut Vec<f32>) -> usize {
        // Read once: the producer may add more while this runs, and taking those too would
        // make the amount drained unbounded by anything the caller can see.
        let waiting = self.len().min(self.data.len());
        out.reserve(waiting);
        let mut taken = 0;
        while taken < waiting {
            match self.pop() {
                Some(sample) => {
                    out.push(sample);
                    taken += 1;
                }
                None => break,
            }
        }
        taken
    }

    /// Throws away everything waiting.
    ///
    /// The consumer's, like [`Self::pop`] and [`Self::drain_into`] — it advances the read
    /// index and nothing else, so it keeps the single-consumer assumption this ring's memory
    /// ordering rests on. `audio/device.rs` uses it for the samples either side of a capture
    /// stream reopening, which are two devices' audio as far as anything can tell.
    pub(super) fn skip(&self) {
        let written = self.written.load(Ordering::Acquire);
        self.read.store(written, Ordering::Release);
    }
}

/// Where one slot is in its life, as **one** value.
///
/// **Two booleans were the bug, and one value is the fix** (found by review on #948). A slot
/// that was free and cleared read as `taken == false, flushed == true`, and the callback
/// tested those two flags with two separate loads: a claim landing between them left the
/// callback clearing the ring and the filter state of a slot that had just been handed to
/// somebody. Two loads are not one decision, and no ordering on either of them makes them one
/// — the second load can always read a value from before the claim it is meant to notice.
///
/// So there is one location, and **each state has exactly one party permitted to leave it**:
///
/// | State | Who may move it | To |
/// | --- | --- | --- |
/// | [`Self::Free`] | [`Mixer::claim`], by compare-exchange | `Live` |
/// | [`Self::Live`] | the one [`SourceHandle`]'s `Drop` | `Dirty` |
/// | [`Self::Live`] | a claim above it revoking it, or its bus being switched off | `Revoked` |
/// | [`Self::Revoked`] | the one [`SourceHandle`]'s `Drop` | `Dirty` |
/// | [`Self::Dirty`] | the output callback, after clearing the slot | `Free` |
///
/// A slot is claimable only while `Free`, and `Free` is written only by the thread that has
/// just finished clearing it. There is no state a second reader can catch half-written,
/// because there is nothing to read twice.
///
/// **#982 added a generation, and the review on #996 showed why that was not enough.** The
/// generation makes `Drop` safe: a handle whose slot has moved on fails its compare-exchange
/// instead of marking `Dirty` a slot somebody else was given. What it does *not* do is make an
/// already-entered `push` inert. A producer that reads `live()` as true and is then preempted
/// while its slot is revoked, recycled and handed to a new owner goes on to write into that
/// owner's ring — the same check-then-act shape #948 was about, moved to the producer side,
/// and a second writer of the very index [`Ring`]'s ordering assumes has one.
///
/// **[`Self::Revoked`] fixes it structurally rather than by widening the check.** Taking a slot
/// away no longer makes it available; it makes it *unrenderable*, and only the owner's own
/// `Drop` moves it onward. So while a handle exists its slot is in `Live` or `Revoked` and can
/// never belong to anybody else, and there is no interleaving in which two producers hold one
/// ring. `live()` stops being a safety check and becomes what it always read as: the way a
/// producer is told to let go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum SlotState {
    /// Nobody owns it and there is nothing in it.
    Free = 0,
    /// A [`SourceHandle`] exists. The callback renders it.
    Live = 1,
    /// The handle is gone and the callback has not cleared it yet.
    ///
    /// **Why a released slot is not immediately re-claimable.** What needs resetting is the
    /// consumer's memory — the read index and the filter states — and a producer resetting it
    /// would be a second writer of what [`Ring`]'s ordering assumes has one. A client with no
    /// working output device therefore never recycles a slot, which costs nothing: nothing is
    /// audible there either way.
    Dirty = 2,
    /// The slot has been taken away, and its handle has not noticed yet.
    ///
    /// **Silent immediately, reusable only once the owner lets go.** The callback renders a
    /// revoked slot as nothing, so a bed that loses its slot to a voice stops being heard on
    /// the next block — which is what "stolen" has to mean. The slot itself stays the old
    /// owner's until its `Drop`, which is what makes the theft safe: no second owner can exist
    /// for the ring that owner may still be writing into.
    ///
    /// **The cost is stated rather than hidden.** A producer that never runs again holds its
    /// slot forever, so a revocation reclaims a slot on the order of one frame rather than one
    /// audio block, and only from a producer that is still running. Every producer in this
    /// client pushes each frame and can therefore see `live()` go false; the contract that
    /// comes with a [`SourceHandle`] is to drop it when it does.
    Revoked = 3,
}

/// How many low bits of a slot word [`SlotState`] occupies.
const STATE_BITS: u32 = 2;

/// The mask those bits make.
const STATE_MASK: u32 = (1 << STATE_BITS) - 1;

/// How many bits above the state hold the [`Bus`], and where they start.
const BUS_SHIFT: u32 = STATE_BITS;
const BUS_BITS: u32 = 3;
const BUS_MASK: u32 = (1 << BUS_BITS) - 1;

/// Where the generation starts, and the mask that keeps it inside its own field.
const GENERATION_SHIFT: u32 = BUS_SHIFT + BUS_BITS;
const GENERATION_MASK: u32 = u32::MAX >> GENERATION_SHIFT;

const _: () = {
    // Three bits is exactly enough for `Bus::ALL`, and one bus more needs a fourth. Stated
    // here because the failure is silent: a sixth bus would alias a fifth's bits and the
    // callback would render it on the wrong gain.
    assert!(
        Bus::ALL.len() <= (BUS_MASK as usize) + 1,
        "a bus does not fit the slot word's bus field"
    );
    assert!(
        SlotState::Revoked as u32 <= STATE_MASK,
        "a state does not fit"
    );
};

/// One slot word: the generation above, then the bus, then the state.
///
/// **The bus lives in the word since the review on #996, and that is a fix rather than a
/// tidy-up.** It used to be an `AtomicU8` beside it, written *after* the compare-exchange that
/// won the slot — so a revocation walking the slots could read a bus from the previous owner
/// and take a slot away from whoever had just been given it. Reading a stale `Ambience` off a
/// slot that had just become `Voice` would have broken the one rule this whole policy exists
/// for. One word means the state, the bus and the generation are published by the single
/// compare-exchange that wins the slot, and read by the single load that inspects it.
const fn pack(generation: u32, bus: Bus, state: SlotState) -> u32 {
    ((generation & GENERATION_MASK) << GENERATION_SHIFT)
        | ((bus.index() as u32) << BUS_SHIFT)
        | state as u32
}

/// The state a slot word holds.
///
/// Total, for [`Bus::from_index`]'s reason: the only writer is [`pack`] and the callback is no
/// place to answer an unreachable state with a panic.
const fn state_of(word: u32) -> SlotState {
    match word & STATE_MASK {
        0 => SlotState::Free,
        1 => SlotState::Live,
        2 => SlotState::Dirty,
        _ => SlotState::Revoked,
    }
}

/// The bus a slot word names.
const fn bus_of(word: u32) -> Bus {
    Bus::from_index(((word >> BUS_SHIFT) & BUS_MASK) as u8)
}

/// The generation a slot word holds.
///
/// Twenty-seven bits, and it wraps. A generation is only ever compared for equality against one
/// a [`SourceHandle`] captured, so what it has to be is *different from the last few*, not
/// unique for the life of the process — and a slot would have to be claimed and released a
/// hundred million times inside the life of one handle for a wrap to make a stale handle look
/// live.
const fn generation_of(word: u32) -> u32 {
    word >> GENERATION_SHIFT
}

/// The generation after `generation`, kept inside its field.
const fn next_generation(generation: u32) -> u32 {
    generation.wrapping_add(1) & GENERATION_MASK
}

/// One slot in the mixer: a ring, the bus it is mixed on, and where it is heard from.
///
/// **Every field is an atomic, and which thread writes which is the whole contract.** A Bevy
/// system writes [`Self::bus`], the gain, the two pan gains, [`Self::target_occlusion`] and
/// [`Self::high_cue`]; the callback writes [`Self::occlusion`] and the two filter states and
/// reads everything else. [`Self::state`] is what says which of them is entitled to.
#[derive(Debug)]
struct Source {
    ring: Ring,
    /// A [`SlotState`], the [`Bus`] and the generation, packed by [`pack`]. The one thing that
    /// decides who may touch the rest — which owner it decides for, and, since the review on
    /// #996, which bus it decides about.
    slot: AtomicU32,
    /// Distance attenuation, as `f32` bits. Kept apart from the pan rather than folded into
    /// it, because a mono device applies this and skips the pan — and recovering one from
    /// the other would be arithmetic standing in for a field.
    gain: AtomicU32,
    /// The constant-power pan pair, as `f32` bits.
    pan_left: AtomicU32,
    pan_right: AtomicU32,
    /// Where the occlusion filter is being asked to go, in `0..1`. Written by a Bevy system.
    target_occlusion: AtomicU32,
    /// Where it has actually got to. Written by the callback alone, once per block.
    occlusion: AtomicU32,
    /// The front/back cue's multiplier on the high band.
    high_cue: AtomicU32,
    /// The two one-pole crossover states. The callback's, and nobody else's.
    low_state: AtomicU32,
    mid_state: AtomicU32,
}

impl Source {
    /// A free slot with nothing in it and nothing in the way.
    fn new() -> Self {
        Self {
            ring: Ring::new(SOURCE_CAPACITY),
            // A slot nobody has used needs no clearing, which is what makes the first
            // `MAX_SOURCES` claims of a fresh mixer succeed with no callback anywhere.
            slot: AtomicU32::new(pack(0, Bus::Master, SlotState::Free)),
            gain: AtomicU32::new(1.0f32.to_bits()),
            pan_left: AtomicU32::new(1.0f32.to_bits()),
            pan_right: AtomicU32::new(1.0f32.to_bits()),
            target_occlusion: AtomicU32::new(0.0f32.to_bits()),
            occlusion: AtomicU32::new(0.0f32.to_bits()),
            high_cue: AtomicU32::new(1.0f32.to_bits()),
            low_state: AtomicU32::new(0.0f32.to_bits()),
            mid_state: AtomicU32::new(0.0f32.to_bits()),
        }
    }

    /// Sets everything one placement says. Called from a Bevy system; never from a callback.
    fn place(&self, placement: Placement) {
        let gain = if placement.gain.is_finite() {
            placement.gain.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let finite = |value: f32, fallback: f32| if value.is_finite() { value } else { fallback };
        self.gain.store(gain.to_bits(), Ordering::Relaxed);
        self.pan_left
            .store(finite(placement.pan.left, 0.0).to_bits(), Ordering::Relaxed);
        self.pan_right.store(
            finite(placement.pan.right, 0.0).to_bits(),
            Ordering::Relaxed,
        );
        self.target_occlusion.store(
            finite(placement.occlusion, 0.0).clamp(0.0, 1.0).to_bits(),
            Ordering::Relaxed,
        );
        self.high_cue.store(
            finite(placement.high_cue, 1.0).clamp(0.0, 1.0).to_bits(),
            Ordering::Relaxed,
        );
    }

    /// Throws away everything the previous owner left behind and frees the slot.
    ///
    /// **The callback runs this, only on a slot it has just read as [`SlotState::Dirty`], and
    /// nothing else may leave that state** — `claim` moves only `Free`, and `Drop` needs a
    /// handle that no longer exists. So the whole reset happens here, consumer-side ring
    /// index and producer-side placement alike, with nobody else entitled to the slot: one
    /// place a slot is cleared rather than two halves that have to agree.
    ///
    /// The exchange rather than a store makes the *safety* independent of that argument —
    /// were something ever able to leave `Dirty`, this fails and the slot stays dirty rather
    /// than being handed out unflushed — and its release pairs with `claim`'s acquire.
    ///
    /// `word` is what the caller read when it decided this slot was dirty, so the exchange is
    /// against that exact generation: a slot whose word has moved since is not this thread's
    /// to free.
    ///
    /// Answers whether it actually freed the slot, which is what [`Mixer::render`] needs in
    /// order to give the world-bus budget back exactly once per slot it was spent on.
    fn recycle(&self, word: u32) -> bool {
        self.ring.skip();
        self.low_state.store(0.0f32.to_bits(), Ordering::Relaxed);
        self.mid_state.store(0.0f32.to_bits(), Ordering::Relaxed);
        self.occlusion.store(0.0f32.to_bits(), Ordering::Relaxed);
        self.place(Placement::UNPOSITIONED);
        self.slot
            .compare_exchange(
                word,
                pack(generation_of(word), bus_of(word), SlotState::Free),
                Ordering::Release,
                Ordering::Relaxed,
            )
            .is_ok()
    }
}

/// The whole of what the output callback touches.
///
/// Held behind an `Arc` by both sides: the callback renders from it, Bevy systems set
/// gains and push samples into it. Every field is an atomic or a slice of atomics, which
/// is what makes that sharing lock-free rather than merely undocumented.
#[derive(Debug)]
pub struct Mixer {
    sources: Box<[Source]>,
    /// One gain per [`Bus`], as `f32` bits, indexed by [`Bus::index`].
    gains: [AtomicU32; Bus::ALL.len()],
    /// Whether each [`Bus`] may hold a source at all, indexed by [`Bus::index`].
    ///
    /// **Not a gain of zero, and that is the whole point of it.** A bus that is off refuses a
    /// claim and releases what it is holding, so nothing is generated, nothing is pushed and
    /// no slot is spent — where a bus at zero gain is a producer still running, still filling
    /// a ring and still occupying one of [`MAX_SOURCES`] to be multiplied by nothing.
    /// `Music` is the only bus a player can turn off today; the mechanism is per-bus because
    /// "off" has to be enforced where slots are handed out, which is a property of the slot.
    enabled: [AtomicBool; Bus::ALL.len()],
    /// How many slots the buses a world feeds are holding between them.
    ///
    /// **One counter, because the reserve is one pool-wide fact and a per-slot
    /// compare-exchange cannot enforce a pool-wide anything.** The review on #996 found the
    /// original: it counted free slots and then won a slot, and two claims that both read
    /// `VOICE_RESERVE + 1` free both succeeded, leaving voice a slot short of the reserve that
    /// exists to stop exactly that. This is decremented by [`Mixer::render`] when a slot is
    /// recycled, so a claim spends budget for as long as it holds a slot and not one moment
    /// longer.
    world_sources: AtomicUsize,
    /// Where the duck is being asked to go, as `f32` bits. Written by a Bevy system; `1.0` is
    /// no duck at all.
    duck_target: AtomicU32,
    /// Where it has actually got to. Written by the callback alone, once per block — the
    /// occlusion ramp's arrangement, for its reason.
    duck: AtomicU32,
    /// Whether the stereo image is folded.
    ///
    /// **A player's choice, folded through the pan the render path already skips on a mono
    /// device**, rather than a second panning path — see [`Mixer::render`].
    mono: AtomicBool,
    /// What the open stream is running at, or [`DEFAULT_SAMPLE_RATE`] before one is.
    sample_rate: AtomicU32,
    /// How many samples one frame of the open stream holds.
    channels: AtomicU32,
    /// The two one-pole coefficients the occlusion filter splits a source at, as `f32` bits.
    ///
    /// **Derived once per stream in [`Self::set_format`], not per block.** They depend on
    /// the device's rate and on two fixed frequencies and on nothing else, so recomputing
    /// them in the callback would be an `exp` per source per block answering a question
    /// whose inputs change only when a device is opened.
    crossover_low: AtomicU32,
    crossover_high: AtomicU32,
}

impl Default for Mixer {
    fn default() -> Self {
        Self::new()
    }
}

impl Mixer {
    /// A mixer with every bus at unity gain and no source claimed.
    ///
    /// This is where the buffers are allocated, and the only place any allocation in this
    /// file happens.
    pub fn new() -> Self {
        let mixer = Self {
            sources: (0..MAX_SOURCES).map(|_| Source::new()).collect(),
            gains: Bus::ALL.map(|_| AtomicU32::new(1.0f32.to_bits())),
            enabled: Bus::ALL.map(|_| AtomicBool::new(true)),
            world_sources: AtomicUsize::new(0),
            // `1.0` on both, so a mixer nobody has spoken to ducks nothing at all rather
            // than ramping up from silence on the first block it renders.
            duck_target: AtomicU32::new(1.0f32.to_bits()),
            duck: AtomicU32::new(1.0f32.to_bits()),
            mono: AtomicBool::new(false),
            sample_rate: AtomicU32::new(DEFAULT_SAMPLE_RATE),
            channels: AtomicU32::new(2),
            crossover_low: AtomicU32::new(0.0f32.to_bits()),
            crossover_high: AtomicU32::new(0.0f32.to_bits()),
        };
        mixer.set_crossovers(DEFAULT_SAMPLE_RATE);
        mixer
    }

    /// Recomputes the band split for a stream running at `sample_rate`.
    fn set_crossovers(&self, sample_rate: u32) {
        self.crossover_low.store(
            spatial::one_pole_coefficient(LOW_CROSSOVER_HZ, sample_rate).to_bits(),
            Ordering::Relaxed,
        );
        self.crossover_high.store(
            spatial::one_pole_coefficient(HIGH_CROSSOVER_HZ, sample_rate).to_bits(),
            Ordering::Relaxed,
        );
    }

    /// Takes one of the [`MAX_SOURCES`] slots, or `None` when the policy at [`MAX_SOURCES`]
    /// says this bus may not have one.
    ///
    /// The handle is the only way to write samples, and there is exactly one live per slot —
    /// which is what makes each ring's producer single, as its ordering assumes.
    ///
    /// **This is where the whole allocation policy lives, and it is not a question the caller
    /// is asked.** Three refusals, in order: a bus that is off has no sources; a bus outside
    /// [`Bus::may_take_the_last_slot`] may not spend the [`VOICE_RESERVE`]; and a claim with
    /// nowhere to go takes a slot away from the lowest bus [`Bus::steal_order`] ranks beneath
    /// this one — and then still answers `None`, because that slot comes back through the
    /// callback like every other released one. The caller drops the sound it wanted to play
    /// rather than queueing it; the slot is there for whoever asks next.
    pub fn claim(self: &Arc<Self>, bus: Bus) -> Option<SourceHandle> {
        if !self.enabled[bus.index()].load(Ordering::SeqCst) {
            return None;
        }
        if let Some(handle) = self.take_a_free_slot(bus) {
            return Some(handle);
        }
        // Re-read before revoking anything. `take_a_free_slot` also answers `None` when the
        // bus was switched off while this claim was landing, and a bus that is off must not
        // take a slot away from one that is on — a refusal that costs somebody else their
        // ambience bed for a sound that will never be generated is worse than the refusal.
        if !self.enabled[bus.index()].load(Ordering::SeqCst) {
            return None;
        }
        self.steal_for(bus);
        None
    }

    /// Wins a `Free` slot for `bus`, honouring the reserve, or answers `None`.
    fn take_a_free_slot(self: &Arc<Self>, bus: Bus) -> Option<SourceHandle> {
        // **The budget is taken before the slot, and it is one atomic step.** The review on
        // #996 found the original doing this the other way round — counting free slots and
        // then winning one — so two claims that both read `VOICE_RESERVE + 1` free both
        // succeeded and voice was left a slot short of the reserve that exists to stop
        // precisely that. A per-slot compare-exchange cannot enforce a pool-wide rule; only a
        // pool-wide location can.
        let budgeted = !bus.may_take_the_last_slot();
        if budgeted && !self.take_world_budget() {
            return None;
        }
        for (index, source) in self.sources.iter().enumerate() {
            // **One compare-exchange, and it is the whole decision.** Only a `Free` slot can
            // be won, and a slot is `Free` only after the callback has cleared it — a
            // released one still holding the previous owner's audio is `Dirty` and this
            // fails on it, as it does on one somebody else is using or one that has been
            // revoked and not yet let go. The acquire pairs with `recycle`'s release, so the
            // winner sees the cleared ring and the reset placement rather than whatever was
            // there before.
            //
            // The generation and the bus move with the claim, in the same word, so there is
            // no window in which this slot is live and reads as somebody else's bus.
            let word = source.slot.load(Ordering::Acquire);
            if state_of(word) != SlotState::Free {
                continue;
            }
            let generation = next_generation(generation_of(word));
            if source
                .slot
                .compare_exchange(
                    word,
                    pack(generation, bus, SlotState::Live),
                    Ordering::SeqCst,
                    Ordering::Relaxed,
                )
                .is_err()
            {
                continue;
            }
            // **The bus is re-read after the slot is won, and that is what makes "off means no
            // source" true rather than nearly true.** `set_enabled(false)` stores the flag and
            // *then* sweeps for live sources; this wins the slot and *then* reads the flag.
            // Both operations are `SeqCst`, so the four have one total order and the two
            // interleavings are the only ones there are: if this load reads `true` the store
            // is later than it, so the sweep that follows the store sees this slot already
            // live and revokes it; if it reads `false`, this gives the slot back here. There
            // is no order in which a live source survives on a bus that is off.
            if !self.enabled[bus.index()].load(Ordering::SeqCst) {
                // Straight to `Dirty`: this slot has no handle to drop it, and its ring is
                // the consumer's to clear like any other released one. The budget goes back
                // with the recycle rather than here, so it is given back exactly once.
                let _ = source.slot.compare_exchange(
                    pack(generation, bus, SlotState::Live),
                    pack(generation, bus, SlotState::Dirty),
                    Ordering::SeqCst,
                    Ordering::Relaxed,
                );
                return None;
            }
            return Some(SourceHandle {
                mixer: Arc::clone(self),
                index,
                generation,
            });
        }
        // Budget taken and no slot to spend it on. Give it straight back: it is only ever a
        // claim on a slot somebody holds, and this claim holds none.
        if budgeted {
            self.give_world_budget_back();
        }
        None
    }

    /// Takes one unit of the world buses' share of the pool, or answers `false`.
    ///
    /// One `fetch_update`, so the read and the increment are one step and the ceiling cannot
    /// be crossed by two claims that both looked before either wrote.
    fn take_world_budget(&self) -> bool {
        self.world_sources
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |held| {
                (held < MAX_SOURCES - VOICE_RESERVE).then_some(held + 1)
            })
            .is_ok()
    }

    /// Gives one unit back.
    fn give_world_budget_back(&self) {
        // Saturating rather than wrapping: an underflow here would be a bookkeeping bug
        // handing the world buses the whole pool, which is the one outcome the reserve exists
        // to prevent, and a saturating floor keeps that failure at "no worse than now".
        let _ = self
            .world_sources
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |held| {
                Some(held.saturating_sub(1))
            });
    }

    /// How many slots are free right now. Test-only: the run-time budget is
    /// [`Self::world_sources`], which is a count of what is *held* rather than of what is left.
    #[cfg(test)]
    fn free_slots(&self) -> usize {
        self.sources
            .iter()
            .filter(|source| state_of(source.slot.load(Ordering::Acquire)) == SlotState::Free)
            .count()
    }

    /// Takes a slot away from the lowest-ranked bus beneath `bus`, and says whether it did.
    ///
    /// **Strictly beneath**, so ambience never steals from ambience and a bus [`Bus::steal_order`]
    /// answers `None` for is never a victim and never a thief. Ties go to the lowest slot
    /// index, which is arbitrary and is stated so that it is not mistaken for a rule.
    fn steal_for(&self, bus: Bus) -> bool {
        let Some(claimant) = bus.steal_order() else {
            // A bus that is never stolen from is also the one nothing is beneath: `Voice` and
            // `Master` outrank everything, so they steal from the deepest victim there is.
            return self.steal_beneath(u8::MAX);
        };
        self.steal_beneath(claimant)
    }

    /// Revokes the live source with the lowest steal order strictly below `claimant`.
    fn steal_beneath(&self, claimant: u8) -> bool {
        let mut victim: Option<(u8, usize, u32)> = None;
        for (index, source) in self.sources.iter().enumerate() {
            // **One load, and it answers both halves of the question.** The state and the bus
            // are one word since the review on #996, so there is no interleaving in which
            // this reads `Live` from a slot that has just been claimed and `Ambience` from
            // the owner before it — which would have taken a slot away from a voice.
            let word = source.slot.load(Ordering::Acquire);
            if state_of(word) != SlotState::Live {
                continue;
            }
            let Some(order) = bus_of(word).steal_order() else {
                continue;
            };
            if order >= claimant {
                continue;
            }
            if victim.is_none_or(|(lowest, _, _)| order < lowest) {
                victim = Some((order, index, word));
            }
        }
        let Some((_, index, word)) = victim else {
            return false;
        };
        self.revoke(index, word)
    }

    /// Moves one live slot to [`SlotState::Revoked`], and says whether it did.
    ///
    /// **Not to `Dirty`, and the difference is the whole of what the review on #996 bought.**
    /// A revoked slot is silent from the next block and is still its owner's: only that
    /// owner's `Drop` moves it on, so no second owner can ever exist for a ring the first may
    /// still be writing into. The generation is left alone, because the handle is not being
    /// invalidated — it is being told to let go, and `SourceHandle::live` is how it hears that.
    fn revoke(&self, index: usize, word: u32) -> bool {
        self.sources[index]
            .slot
            .compare_exchange(
                word,
                pack(generation_of(word), bus_of(word), SlotState::Revoked),
                Ordering::SeqCst,
                Ordering::Relaxed,
            )
            .is_ok()
    }

    /// Sets one bus's gain. `0.0` is silent, `1.0` is unity; anything else is clamped in.
    pub fn set_gain(&self, bus: Bus, gain: f32) {
        let gain = if gain.is_finite() {
            gain.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.gains[bus.index()].store(gain.to_bits(), Ordering::Relaxed);
    }

    /// Turns one bus on or off.
    ///
    /// **Off means no source at all**, which is why this revokes what the bus is holding
    /// rather than only refusing the next claim: a generator already running would otherwise
    /// go on filling a ring nobody would ever hear, and go on holding a slot the policy at
    /// [`MAX_SOURCES`] has other uses for. Turning a bus back on restores nothing — whatever
    /// was playing is gone, and the next thing to ask gets a slot.
    ///
    /// **The store comes before the sweep, and [`Mixer::claim`] reads the flag after winning
    /// its slot.** That ordering is the whole of why a claim racing this cannot leave a live
    /// source on a bus that is off; the argument is written out at the re-read in
    /// `take_a_free_slot`, and both sides are `SeqCst` so that there is a total order for it
    /// to be an argument about.
    // #982 part 1 establishes the mechanism; part 3 is where `AudioControls::music_on` calls
    // it, so between the two this has only its tests below.
    #[allow(dead_code)]
    pub fn set_enabled(&self, bus: Bus, on: bool) {
        self.enabled[bus.index()].store(on, Ordering::SeqCst);
        if on {
            return;
        }
        for (index, source) in self.sources.iter().enumerate() {
            let word = source.slot.load(Ordering::SeqCst);
            if state_of(word) != SlotState::Live || bus_of(word) != bus {
                continue;
            }
            self.revoke(index, word);
        }
    }

    /// Asks the ducked buses to walk to `gain`. `1.0` is no duck at all.
    ///
    /// A target and not a value: [`Mixer::render`] advances towards it by one block's worth
    /// of time, so a Bevy system may call this every frame and it costs one atomic store.
    // See `set_enabled` above for why this carries the allowance: `duck_under_speech` is
    // part 3's.
    #[allow(dead_code)]
    pub fn set_duck(&self, gain: f32) {
        let gain = if gain.is_finite() {
            gain.clamp(0.0, 1.0)
        } else {
            1.0
        };
        self.duck_target.store(gain.to_bits(), Ordering::Relaxed);
    }

    /// Folds the stereo image, or stops folding it.
    // See `set_enabled` above for why this carries the allowance: the setting that calls it
    // is part 2's and the wiring is part 3's.
    #[allow(dead_code)]
    pub fn set_mono(&self, mono: bool) {
        self.mono.store(mono, Ordering::Relaxed);
    }

    /// Records the format the open stream negotiated. `device.rs` is the one caller, and
    /// it calls this once per stream it opens.
    pub fn set_format(&self, sample_rate: u32, channels: u16) {
        let sample_rate = sample_rate.max(1);
        self.sample_rate.store(sample_rate, Ordering::Relaxed);
        self.channels
            .store(u32::from(channels).max(1), Ordering::Relaxed);
        self.set_crossovers(sample_rate);
    }

    /// What one slot is doing. Test-only: nothing at run time asks, because the answer is
    /// only ever acted on by the compare-exchange that reads it.
    #[cfg(test)]
    fn slot_state(&self, index: usize) -> SlotState {
        state_of(self.sources[index].slot.load(Ordering::Acquire))
    }

    /// Which bus one live slot is on. Test-only, for the assertions about which slot a steal
    /// chose — the run-time answer is only ever read by the callback that acts on it.
    #[cfg(test)]
    fn slot_bus(&self, index: usize) -> Bus {
        bus_of(self.sources[index].slot.load(Ordering::Acquire))
    }

    /// Where the duck has actually got to, for the tests that watch it ramp.
    #[cfg(test)]
    fn reached_duck(&self) -> f32 {
        f32::from_bits(self.duck.load(Ordering::Relaxed))
    }

    /// The sample rate the open stream is running at.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate.load(Ordering::Relaxed)
    }

    /// Fills `sink` with everything the sources have to say.
    ///
    /// **This is the function that runs on the output thread.** It allocates nothing, takes
    /// no lock, logs nothing and mentions no Bevy type; every read below is an atomic load
    /// over memory that was allocated when the mixer was built, and every scratch array is
    /// a fixed-size stack array indexed by slot.
    ///
    /// ## Three passes, and the first two happen once per block rather than per sample
    ///
    /// 1. **Recycle and prepare.** Any slot whose handle has been dropped is flushed here,
    ///    because everything that needs clearing is memory this thread owns. Then each live
    ///    slot's smoothed occlusion is advanced by exactly this block's worth of time and the
    ///    band gains it produces are computed — once, and held constant across the block. A
    ///    block is a few milliseconds against a 50 ms attack, so per-block is fine-grained
    ///    enough for the ear and is what "the smoothing runs per audio block" means.
    /// 2. **Mix.** Per sample per live slot: a two-crossover three-band split, the band gains
    ///    from pass 1, then the source's own two gains into a left and a right sum.
    /// 3. **Write out.** Left to the even channels, right to the odd ones.
    ///
    /// **A source with nothing in its way is reconstructed exactly.** At zero occlusion every
    /// band gain is `1.0`, and `low + (mid − low) + (x − mid)` is `x` whatever the crossovers
    /// are — so the filter is transparent rather than approximately transparent, and a
    /// speaker in the open sounds like one this stage never touched.
    ///
    /// **A mono device is not panned at all, and neither is a player who asked for mono.**
    /// One channel cannot carry a stereo image, and the two honest answers are to average the
    /// pair — which makes a hard-panned voice 3 dB quieter than a centred one for no reason a
    /// listener could act on — or to skip the pan. This skips it: the distance gain and the
    /// occlusion filter still apply, because those are audible on one loudspeaker, and the
    /// direction simply is not.
    ///
    /// **[`Mixer::set_mono`] folds the image through that same branch and not a second one.**
    /// A player on a stereo card who wants one image — a headphone in one ear, a hearing
    /// difference, a speaker on the wrong side of the room — asks the same question a mono
    /// device asks, so it gets the same answer and the same [`PanGains::UNPOSITIONED`] pair
    /// rather than a downmix written twice. Both channels then carry the identical sum, which
    /// is what "folded" means on a device that still has two of them.
    pub fn render(&self, sink: &mut impl Sink) {
        let channels = self.channels.load(Ordering::Relaxed).max(1) as usize;
        let sample_rate = self.sample_rate.load(Ordering::Relaxed).max(1);
        let bus_gains = Bus::ALL.map(|bus| {
            let stored = f32::from_bits(self.gains[bus.index()].load(Ordering::Relaxed));
            // The master is the output stage below rather than a per-source multiplier, so a
            // source on it is scaled once and never squared.
            if matches!(bus, Bus::Master) {
                1.0
            } else {
                stored
            }
        });
        let master = f32::from_bits(self.gains[Bus::Master.index()].load(Ordering::Relaxed));
        let mono = channels == 1 || self.mono.load(Ordering::Relaxed);
        let low_crossover = f32::from_bits(self.crossover_low.load(Ordering::Relaxed));
        let high_crossover = f32::from_bits(self.crossover_high.load(Ordering::Relaxed));

        let block = sink.block();
        let frames = block.len() / channels;
        let elapsed = frames as f32 / sample_rate as f32;

        // The duck advances once per block, by exactly this block's worth of time, and its
        // state lives here because it has to move with the audio rather than with whatever
        // frame rate the world is being looked at. Held constant across the block for the
        // reason pass 1 gives about the occlusion ramp.
        let duck = advance_duck(
            f32::from_bits(self.duck.load(Ordering::Relaxed)),
            f32::from_bits(self.duck_target.load(Ordering::Relaxed)),
            elapsed,
        );
        self.duck.store(duck.to_bits(), Ordering::Relaxed);

        // Pass 1. Fixed-size and on the stack: `MAX_SOURCES` is a constant precisely so this
        // preparation costs no allocation.
        let mut live = [false; MAX_SOURCES];
        let mut bands = [[0.0f32; BANDS]; MAX_SOURCES];
        let mut left = [0.0f32; MAX_SOURCES];
        let mut right = [0.0f32; MAX_SOURCES];
        let mut low_state = [0.0f32; MAX_SOURCES];
        let mut mid_state = [0.0f32; MAX_SOURCES];
        for (index, source) in self.sources.iter().enumerate() {
            // **One load, one decision.** Clearing a `Dirty` slot is this thread's job
            // because nothing else may leave that state; live and free are not this thread's
            // to touch.
            let word = source.slot.load(Ordering::Acquire);
            match state_of(word) {
                SlotState::Dirty => {
                    // Clearing a `Dirty` slot is this thread's job because nothing else may
                    // leave that state — and freeing it is where a world bus's budget goes
                    // back, exactly once, on the one transition every released slot makes.
                    if source.recycle(word) && !bus_of(word).may_take_the_last_slot() {
                        self.give_world_budget_back();
                    }
                    continue;
                }
                // A revoked slot is silent from this block on, and is still its owner's: only
                // that owner's `Drop` moves it onward, which is what stops a second owner
                // existing for a ring the first may still be writing into.
                SlotState::Free | SlotState::Revoked => continue,
                SlotState::Live => {}
            }
            live[index] = true;

            let smoothed = spatial::advance(
                f32::from_bits(source.occlusion.load(Ordering::Relaxed)),
                f32::from_bits(source.target_occlusion.load(Ordering::Relaxed)),
                elapsed,
            );
            source
                .occlusion
                .store(smoothed.to_bits(), Ordering::Relaxed);
            let mut gains = spatial::band_gains(smoothed);
            // The front/back cue rides on the high band alone. It is a second multiplier
            // rather than a second filter, which is the whole of why it costs nothing here.
            gains[BANDS - 1] *= f32::from_bits(source.high_cue.load(Ordering::Relaxed));
            bands[index] = gains;

            // The bus gain, and the duck on the two buses that take one. A bus nobody is
            // ducking multiplies by exactly `1.0`, so music with ducking turned off is the
            // samples that were pushed rather than an approximate restoration of them.
            let bus = bus_of(word);
            let bus = if bus.ducks() {
                bus_gains[bus.index()] * duck
            } else {
                bus_gains[bus.index()]
            };
            // On a mono device, or for a player who asked for one image, the pan is skipped
            // and the distance gain is not: one is inaudible on one loudspeaker and the
            // other plainly is not.
            let (pan_left, pan_right) = if mono {
                (PanGains::UNPOSITIONED.left, PanGains::UNPOSITIONED.right)
            } else {
                (
                    f32::from_bits(source.pan_left.load(Ordering::Relaxed)),
                    f32::from_bits(source.pan_right.load(Ordering::Relaxed)),
                )
            };
            let gain = f32::from_bits(source.gain.load(Ordering::Relaxed)) * bus;
            left[index] = pan_left * gain;
            right[index] = pan_right * gain;
            low_state[index] = f32::from_bits(source.low_state.load(Ordering::Relaxed));
            mid_state[index] = f32::from_bits(source.mid_state.load(Ordering::Relaxed));
        }

        // Pass 2.
        for frame in block.chunks_mut(channels) {
            let mut sum_left = 0.0;
            let mut sum_right = 0.0;
            for index in 0..self.sources.len() {
                if !live[index] {
                    continue;
                }
                // An underrun is silence, never the previous sample and never whatever
                // happened to be in the buffer.
                let sample = self.sources[index].ring.pop().unwrap_or(0.0);
                low_state[index] += low_crossover * (sample - low_state[index]);
                mid_state[index] += high_crossover * (sample - mid_state[index]);
                let low = low_state[index];
                let mid = mid_state[index] - low;
                let high = sample - mid_state[index];
                let filtered =
                    low * bands[index][0] + mid * bands[index][1] + high * bands[index][2];
                sum_left += filtered * left[index];
                sum_right += filtered * right[index];
            }
            // Clamped, so a sum of loud sources is quiet distortion rather than whatever
            // the device does with a sample outside its range.
            let out_left = (sum_left * master).clamp(-1.0, 1.0);
            let out_right = (sum_right * master).clamp(-1.0, 1.0);
            // Pass 3. Left to the even channels, right to the odd ones — which is the
            // ordinary interleaving for one and two channels and a plain, stated answer for
            // more, since this client has no surround layout to place anything into.
            for (channel, sample) in frame.iter_mut().enumerate() {
                *sample = if channel % 2 == 0 {
                    out_left
                } else {
                    out_right
                };
            }
        }

        // The filter states belong to this thread, and this is where they go back.
        for (index, source) in self.sources.iter().enumerate() {
            if live[index] {
                source
                    .low_state
                    .store(low_state[index].to_bits(), Ordering::Relaxed);
                source
                    .mid_state
                    .store(mid_state[index].to_bits(), Ordering::Relaxed);
            }
        }
    }
}

/// How far a duck may travel in `elapsed_seconds`, applied to `current` towards `target`.
///
/// [`spatial::advance`]'s arithmetic, deliberately not [`spatial::advance`] itself: that one
/// carries the occlusion filter's attack and release, which are a claim about a wall rather
/// than about a bed getting out of somebody's way, and folding two rates into one function
/// would make each harder to move than it is now.
///
/// A duck going *down* is an attack, so the comparison is the mirror of the occlusion ramp's:
/// there, a rising occlusion is a listener walking behind something.
fn advance_duck(current: f32, target: f32, elapsed_seconds: f32) -> f32 {
    let current = if current.is_finite() {
        current.clamp(0.0, 1.0)
    } else {
        1.0
    };
    if !elapsed_seconds.is_finite() || elapsed_seconds <= 0.0 {
        return current;
    }
    let target = if target.is_finite() {
        target.clamp(0.0, 1.0)
    } else {
        return current;
    };
    let full_travel = if target < current {
        DUCK_ATTACK_SECONDS
    } else {
        DUCK_RELEASE_SECONDS
    };
    let step = elapsed_seconds / full_travel;
    current + (target - current).clamp(-step, step)
}

/// The producer end of one claimed source.
///
/// Held by a Bevy system. Cloning it is deliberately impossible: two producers on one ring
/// is the assumption [`Ring`] is built on, and there is no way to get a second handle to a
/// slot [`Mixer::claim`] has already given away.
///
/// **Dropping it gives the slot back**, which is what lets `audio/heard.rs` keep one source
/// per speaker out of a fixed pool. Before #854 a claim was for the mixer's life, because
/// the three things that claimed one lived as long as the client; a speaker does not.
///
/// **Since #982 the slot can also be taken away**, by [`Mixer::claim`] enforcing the policy at
/// [`MAX_SOURCES`] or by [`Mixer::set_enabled`] turning a bus off. A handle whose slot has
/// gone is *inert*, not dangling: [`Self::generation`] no longer matches the one the slot
/// holds, so every method below answers as though the ring were full and `Drop` does nothing.
/// A caller that never checks therefore pushes into a void rather than into somebody else's
/// audio, which is the failure this shape exists to make impossible.
#[derive(Debug)]
pub struct SourceHandle {
    mixer: Arc<Mixer>,
    index: usize,
    /// The generation this handle won. See [`Source::slot`].
    generation: u32,
}

impl SourceHandle {
    /// Whether this handle still owns its slot.
    ///
    /// **One load, and the answer is only ever true of the whole word**: the generation and
    /// the state live in one location precisely so that "is it mine" cannot be answered from
    /// two reads that disagree.
    pub fn live(&self) -> bool {
        let word = self.mixer.sources[self.index].slot.load(Ordering::Acquire);
        generation_of(word) == self.generation && state_of(word) == SlotState::Live
    }

    /// Whether this handle's slot has been taken away.
    ///
    /// **The one thing a producer owes the pool.** A revoked slot is silent already; what it
    /// is waiting for is its owner to drop the handle, which is the only move that returns it.
    /// A producer that goes on holding one is not unsafe — nobody else can have the slot while
    /// it does, which is the point — it is simply keeping a slot the policy has given away.
    #[cfg(test)]
    fn revoked(&self) -> bool {
        let word = self.mixer.sources[self.index].slot.load(Ordering::Acquire);
        generation_of(word) == self.generation && state_of(word) == SlotState::Revoked
    }

    /// Appends as much of `samples` as fits, and answers how much that was.
    ///
    /// A handle whose slot was taken accepts nothing, which is the same answer a full ring
    /// gives and wants no separate branch from its caller.
    pub fn push(&self, samples: &[f32]) -> usize {
        if !self.live() {
            return 0;
        }
        self.mixer.sources[self.index].ring.push(samples)
    }

    /// How many more samples [`Self::push`] would accept right now.
    pub fn free(&self) -> usize {
        if !self.live() {
            return 0;
        }
        self.mixer.sources[self.index].ring.free()
    }

    /// The mixer this source feeds, for the sample rate a generator needs.
    pub fn mixer(&self) -> &Arc<Mixer> {
        &self.mixer
    }

    /// Says where this source is heard from.
    ///
    /// Every field is stored and none is acted on here: the occlusion is a *target* the
    /// callback ramps towards, and the gains apply from the next block the device asks for.
    /// A Bevy system may call this every frame and it costs four atomic stores.
    pub fn place(&self, placement: Placement) {
        if !self.live() {
            return;
        }
        self.mixer.sources[self.index].place(placement);
    }

    /// The smoothed occlusion the render path has actually reached, for tests.
    ///
    /// Test-only, and it reads the *consumer's* value rather than the target — which is the
    /// only thing that distinguishes a filter that is ramping from one that has been told to.
    #[cfg(test)]
    pub fn reached_occlusion(&self) -> f32 {
        f32::from_bits(
            self.mixer.sources[self.index]
                .occlusion
                .load(Ordering::Relaxed),
        )
    }
}

impl Drop for SourceHandle {
    /// Hands the slot back, and asks the callback to clear it.
    ///
    /// **One store, and it does not touch the ring.** The read index and the filter states
    /// are the consumer's memory, and a producer resetting them would be a second writer of
    /// exactly what [`Ring`]'s ordering assumes has one. So this moves the slot to
    /// [`SlotState::Dirty`] — silent to the callback, refused by [`Mixer::claim`] — and the
    /// callback does the clearing and the freeing together.
    ///
    /// **A read-modify-write rather than a store, and two reviews are why.** `Live` used to be
    /// this handle's state to leave and nobody else's; #982 added a second party who may leave
    /// it, so a plain store could land after the slot had been cleared and handed on, killing
    /// a source this handle has nothing to do with. The review on #996 added a fourth state,
    /// so there are now two states a live handle may be dropped from — `Live` if nothing has
    /// happened to it, `Revoked` if its slot was taken — and exactly one party, this `Drop`,
    /// may leave either.
    ///
    /// The update is conditional on the generation, so a slot whose generation has moved is
    /// left alone; the release is what makes the last samples this owner pushed visible to the
    /// callback that discards them.
    fn drop(&mut self) {
        let generation = self.generation;
        let _ = self.mixer.sources[self.index].slot.fetch_update(
            Ordering::Release,
            Ordering::Relaxed,
            |word| {
                if generation_of(word) != generation {
                    return None;
                }
                match state_of(word) {
                    SlotState::Live | SlotState::Revoked => {
                        Some(pack(generation, bus_of(word), SlotState::Dirty))
                    }
                    SlotState::Free | SlotState::Dirty => None,
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    /// Counts this thread's allocations, so the render path can be asserted to make none.
    ///
    /// **Per thread, and with no destructor.** A global counter would be inflated by
    /// whatever the rest of the suite is doing on its own threads — `cargo test` runs them
    /// in parallel — and would make this assertion flake rather than fail. `const`-initialised
    /// so that reaching the counter cannot itself allocate, and read through `try_with` so
    /// that an allocation during thread teardown cannot panic inside the allocator.
    struct Counting;

    thread_local! {
        static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    }

    fn note_allocation() {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get().wrapping_add(1)));
    }

    fn allocations() -> usize {
        ALLOCATIONS.with(Cell::get)
    }

    // SAFETY-free by construction: every method delegates to `System` and the only extra
    // work is a `Cell` increment on a thread-local with no destructor.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            note_allocation();
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            note_allocation();
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    #[global_allocator]
    static COUNTING: Counting = Counting;

    /// A sink over a fixed buffer, which is the whole of what a test needs a device for.
    struct VecSink(Vec<f32>);

    impl Sink for VecSink {
        fn block(&mut self) -> &mut [f32] {
            &mut self.0
        }
    }

    fn mono_mixer() -> Arc<Mixer> {
        let mixer = Arc::new(Mixer::new());
        mixer.set_format(DEFAULT_SAMPLE_RATE, 1);
        mixer
    }

    /// The same, with the two channels a pan needs somewhere to go.
    fn stereo_mixer() -> Arc<Mixer> {
        let mixer = Arc::new(Mixer::new());
        mixer.set_format(DEFAULT_SAMPLE_RATE, 2);
        mixer
    }

    /// A placement with one field moved off its identity, so a test says what it is testing.
    fn placed(gain: f32, pan: PanGains, occlusion: f32, high_cue: f32) -> Placement {
        Placement {
            gain,
            pan,
            occlusion,
            high_cue,
        }
    }

    fn tone(hz: f32, samples: usize) -> Vec<f32> {
        (0..samples)
            .map(|n| {
                (n as f32 * std::f32::consts::TAU * hz / DEFAULT_SAMPLE_RATE as f32).sin() * 0.5
            })
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    /// **The property that makes the three-band stage safe to put in front of everything.**
    /// At zero occlusion every band gain is `1.0`, and `low + (mid − low) + (x − mid)` is `x`
    /// whatever the crossovers are — so a speaker in the open is not "close enough to
    /// transparent", it is the samples that were pushed in. A filter implemented as three
    /// independent band-pass sections would be neither.
    #[test]
    fn a_source_with_nothing_in_its_way_is_reconstructed_exactly() {
        let mixer = mono_mixer();
        let source = mixer.claim(Bus::Master).expect("a free slot");
        let input = tone(440.0, 512);
        source.push(&input);

        let mut sink = VecSink(vec![0.0; input.len()]);
        mixer.render(&mut sink);

        for (out, want) in sink.0.iter().zip(&input) {
            assert!((out - want).abs() < 1e-5, "{out} against {want}");
        }
    }

    /// The assertion a swapped channel pair fails, and the reason the mirror case is here
    /// too: asserting only "right is louder" would pass an implementation that put
    /// everything in the right channel whatever the pan said.
    #[test]
    fn a_panned_source_puts_its_energy_where_the_pan_says() {
        let energy = |azimuth: f32| {
            let mixer = stereo_mixer();
            let source = mixer.claim(Bus::Master).expect("a free slot");
            source.place(placed(1.0, spatial::pan_gains(azimuth), 0.0, 1.0));
            source.push(&[1.0; 8]);
            let mut sink = VecSink(vec![0.0; 16]);
            mixer.render(&mut sink);
            let left: f32 = sink.0.iter().step_by(2).map(|s| s.abs()).sum();
            let right: f32 = sink.0.iter().skip(1).step_by(2).map(|s| s.abs()).sum();
            (left, right)
        };

        let (left, right) = energy(std::f32::consts::FRAC_PI_2);
        assert!(right > 7.9, "hard right put {right} in the right channel");
        assert!(left < 0.1, "hard right put {left} in the left channel");

        let (left, right) = energy(-std::f32::consts::FRAC_PI_2);
        assert!(left > 7.9, "hard left put {left} in the left channel");
        assert!(right < 0.1, "hard left put {right} in the right channel");

        let (left, right) = energy(0.0);
        assert!((left - right).abs() < 1e-4, "dead ahead is not centred");
    }

    /// **The assertion an occlusion filter that is really a volume control fails.** Both
    /// tones lose something; the high one has to lose a great deal more.
    #[test]
    fn an_occluded_voice_loses_its_top_before_its_bottom() {
        let surviving = |hz: f32| {
            let measure = |occlusion: f32| {
                let mixer = mono_mixer();
                let source = mixer.claim(Bus::Master).expect("a free slot");
                source.place(placed(1.0, PanGains::UNPOSITIONED, occlusion, 1.0));
                let input = tone(hz, 4_800);
                source.push(&input);
                let mut sink = VecSink(vec![0.0; input.len()]);
                mixer.render(&mut sink);
                // The second half only: the crossovers start from zero and the first
                // milliseconds are the filter settling rather than the answer.
                rms(&sink.0[2_400..])
            };
            measure(1.0) / measure(0.0)
        };

        let low = surviving(120.0);
        let high = surviving(8_000.0);
        assert!(low < 1.0, "a wall took nothing off a low tone: {low}");
        assert!(
            high < low * 0.5,
            "a wall took {high} off the top and {low} off the bottom — that is a volume control"
        );
    }

    /// A step implementation reaches the target on its first block, which is what this
    /// arithmetic rules out: 10 ms of a 50 ms attack is a fifth of the way, exactly.
    #[test]
    fn the_filter_ramps_towards_an_occlusion_rather_than_stepping_to_it() {
        let mixer = mono_mixer();
        let source = mixer.claim(Bus::Master).expect("a free slot");
        source.place(placed(1.0, PanGains::UNPOSITIONED, 1.0, 1.0));

        // 480 frames at 48 kHz is 10 ms.
        let mut sink = VecSink(vec![0.0; 480]);
        mixer.render(&mut sink);
        let reached = source.reached_occlusion();
        assert!(
            (reached - 0.2).abs() < 1e-4,
            "one 10 ms block of a 50 ms attack reached {reached}"
        );

        for _ in 0..4 {
            mixer.render(&mut sink);
        }
        assert!(
            (source.reached_occlusion() - 1.0).abs() < 1e-5,
            "five blocks did not arrive: {}",
            source.reached_occlusion()
        );
    }

    /// **The transition table, walked.** Each state has exactly one party permitted to leave
    /// it, and this is what that means in practice: a live slot refuses a claim and is not
    /// cleared by the callback; a dirty one refuses a claim until the callback has cleared
    /// it; a free one is claimable and comes back with nothing of its last owner in it.
    ///
    /// This is the property the two booleans could not have — not a faster version of the
    /// same check. There is one location, so there is no pair of loads for a claim to land
    /// between (#948).
    #[test]
    fn a_slot_moves_free_to_live_to_dirty_to_free_and_nowhere_else() {
        let mixer = mono_mixer();
        assert_eq!(mixer.slot_state(0), SlotState::Free);

        let mut held: Vec<SourceHandle> = (0..MAX_SOURCES)
            .map(|_| mixer.claim(Bus::Master).expect("a free slot"))
            .collect();
        for index in 0..MAX_SOURCES {
            assert_eq!(mixer.slot_state(index), SlotState::Live, "slot {index}");
        }
        assert!(
            mixer.claim(Bus::Master).is_none(),
            "a live slot was claimed"
        );

        let last = MAX_SOURCES - 1;
        let released = held.pop().expect("one of sixteen");
        released.push(&[0.5; 64]);
        drop(released);
        assert_eq!(mixer.slot_state(last), SlotState::Dirty);
        assert!(
            mixer.claim(Bus::Master).is_none(),
            "a dirty slot was claimed"
        );

        // Only the callback leaves `Dirty`, and it leaves it clear.
        let mut sink = VecSink(vec![0.0; 4]);
        mixer.render(&mut sink);
        assert_eq!(sink.0, vec![0.0; 4], "a released slot was still audible");
        assert_eq!(mixer.slot_state(last), SlotState::Free);

        let again = mixer.claim(Bus::Master).expect("the slot back");
        assert_eq!(mixer.slot_state(last), SlotState::Live);
        let mut sink = VecSink(vec![0.0; 8]);
        mixer.render(&mut sink);
        assert_eq!(
            sink.0,
            vec![0.0; 8],
            "a new owner inherited the last one's audio"
        );
        drop(again);
        drop(held);
    }

    /// **The single-threaded shadow of the race #948's review found**, and the closest a test
    /// can honestly get to it: the callback deciding, slot by slot, whose a slot is, while
    /// fifteen of them are being recycled in the same pass. The race itself needed a claim to
    /// land between two atomic loads and cannot be reproduced deterministically; what is
    /// testable is that the decision is per slot and never reaches a live one.
    #[test]
    fn recycling_one_slot_does_not_touch_a_live_one() {
        let mixer = mono_mixer();
        let live = mixer.claim(Bus::Master).expect("a free slot");
        let doomed: Vec<SourceHandle> = (0..MAX_SOURCES - 1)
            .map(|_| mixer.claim(Bus::Master).expect("a free slot"))
            .collect();
        live.push(&[0.5; 64]);
        drop(doomed);

        let mut sink = VecSink(vec![0.0; 8]);
        mixer.render(&mut sink);
        assert_eq!(
            sink.0,
            vec![0.5; 8],
            "a live slot lost its audio to a neighbour being recycled"
        );
    }

    #[test]
    fn a_slot_the_callback_has_not_cleared_is_not_handed_out() {
        let mixer = mono_mixer();
        let mut held: Vec<SourceHandle> = (0..MAX_SOURCES)
            .map(|_| mixer.claim(Bus::Master).expect("a free slot"))
            .collect();
        assert!(mixer.claim(Bus::Master).is_none(), "a seventeenth slot");

        held.pop();
        assert!(
            mixer.claim(Bus::Master).is_none(),
            "a released slot was handed out before the callback cleared it"
        );

        let mut sink = VecSink(vec![0.0; 4]);
        mixer.render(&mut sink);
        assert!(
            mixer.claim(Bus::Master).is_some(),
            "a cleared slot was never handed back out"
        );
    }

    /// Two properties in one, because they are the same defect seen from either end: what a
    /// released slot plays, and what a re-claimed one inherits.
    #[test]
    fn a_reused_slot_carries_nothing_of_its_previous_owner() {
        let mixer = mono_mixer();
        let first = mixer.claim(Bus::Master).expect("a free slot");
        first.push(&[1.0; 64]);
        drop(first);

        let mut sink = VecSink(vec![0.0; 8]);
        mixer.render(&mut sink);
        assert_eq!(sink.0, vec![0.0; 8], "a released slot was still audible");

        let second = mixer.claim(Bus::Master).expect("the slot back");
        let mut sink = VecSink(vec![0.0; 64]);
        mixer.render(&mut sink);
        assert_eq!(
            sink.0,
            vec![0.0; 64],
            "a new owner inherited the last one's audio"
        );

        second.push(&[0.5; 4]);
        let mut sink = VecSink(vec![0.0; 4]);
        mixer.render(&mut sink);
        assert_eq!(sink.0, vec![0.5; 4], "the new owner could not be heard");
    }

    /// One loudspeaker cannot carry a direction, so the pan is skipped and the distance gain
    /// is not. Half of both — the arithmetic of a naive downmix — is the wrong answer this
    /// pins out.
    #[test]
    fn a_mono_device_hears_the_distance_but_not_the_direction() {
        let mixer = mono_mixer();
        let source = mixer.claim(Bus::Master).expect("a free slot");
        source.place(placed(
            0.5,
            spatial::pan_gains(std::f32::consts::FRAC_PI_2),
            0.0,
            1.0,
        ));
        source.push(&[1.0; 4]);

        let mut sink = VecSink(vec![0.0; 4]);
        mixer.render(&mut sink);
        for sample in &sink.0 {
            assert!((sample - 0.5).abs() < 1e-5, "{sample}");
        }
    }

    /// The front/back cue rides on the high band and on nothing else, so a source behind the
    /// listener keeps its body and loses its edge.
    #[test]
    fn the_front_back_cue_touches_the_top_band_alone() {
        let surviving = |hz: f32, cue: f32| {
            let mixer = mono_mixer();
            let source = mixer.claim(Bus::Master).expect("a free slot");
            source.place(placed(1.0, PanGains::UNPOSITIONED, 0.0, cue));
            let input = tone(hz, 4_800);
            source.push(&input);
            let mut sink = VecSink(vec![0.0; input.len()]);
            mixer.render(&mut sink);
            rms(&sink.0[2_400..])
        };

        let low_ahead = surviving(120.0, 1.0);
        let low_behind = surviving(120.0, 0.5);
        assert!(
            (low_ahead - low_behind).abs() / low_ahead < 0.05,
            "the cue took {low_ahead} down to {low_behind} at 120 Hz"
        );

        let high_ahead = surviving(8_000.0, 1.0);
        let high_behind = surviving(8_000.0, 0.5);
        assert!(
            high_behind < high_ahead * 0.7,
            "the cue took nothing off the top: {high_ahead} to {high_behind}"
        );
    }

    /// A slot comes back from `recycle` unpositioned as well as empty. The placement is the
    /// producer's memory and the ring is the consumer's, and both are reset in the one place
    /// where nobody else is entitled to the slot — so a new speaker never opens their mouth
    /// panned to wherever the last one was standing.
    #[test]
    fn a_reused_slot_comes_back_unpositioned() {
        let mixer = stereo_mixer();
        let first = mixer.claim(Bus::Master).expect("a free slot");
        first.place(placed(
            1.0,
            spatial::pan_gains(std::f32::consts::FRAC_PI_2),
            0.0,
            1.0,
        ));
        drop(first);

        let mut sink = VecSink(vec![0.0; 4]);
        mixer.render(&mut sink);

        let second = mixer.claim(Bus::Master).expect("the slot back");
        second.push(&[1.0; 4]);
        let mut sink = VecSink(vec![0.0; 8]);
        mixer.render(&mut sink);
        assert_eq!(
            sink.0,
            vec![1.0; 8],
            "a new owner inherited the last one's pan"
        );
    }

    #[test]
    fn a_source_is_unpositioned_until_it_is_placed() {
        let mixer = stereo_mixer();
        let source = mixer.claim(Bus::Master).expect("a free slot");
        source.push(&[1.0; 4]);
        let mut sink = VecSink(vec![0.0; 8]);
        mixer.render(&mut sink);
        assert_eq!(
            sink.0,
            vec![1.0; 8],
            "a source nobody placed was not heard at unity in both ears"
        );
    }

    #[test]
    fn a_source_on_the_master_bus_is_scaled_once() {
        let mixer = mono_mixer();
        let source = mixer.claim(Bus::Master).expect("a free slot");
        mixer.set_gain(Bus::Master, 0.5);
        source.push(&[1.0, 1.0]);

        let mut sink = VecSink(vec![0.0; 2]);
        mixer.render(&mut sink);

        assert_eq!(sink.0, vec![0.5, 0.5], "master applies exactly once");
    }

    #[test]
    fn the_master_gain_applies_after_the_bus_gain() {
        let mixer = mono_mixer();
        let source = mixer.claim(Bus::Voice).expect("a free slot");
        mixer.set_gain(Bus::Voice, 0.5);
        mixer.set_gain(Bus::Master, 0.5);
        source.push(&[1.0]);

        let mut sink = VecSink(vec![0.0; 1]);
        mixer.render(&mut sink);

        assert_eq!(sink.0, vec![0.25], "0.5 on the bus, then 0.5 on the master");
    }

    #[test]
    fn two_sources_sum() {
        let mixer = mono_mixer();
        let one = mixer.claim(Bus::Master).expect("a free slot");
        let two = mixer.claim(Bus::Master).expect("a second free slot");
        one.push(&[0.25, 0.25]);
        two.push(&[0.5, 0.5]);

        let mut sink = VecSink(vec![0.0; 2]);
        mixer.render(&mut sink);

        assert_eq!(sink.0, vec![0.75, 0.75]);
    }

    #[test]
    fn an_underrun_is_silence_and_not_the_last_sample() {
        let mixer = mono_mixer();
        let source = mixer.claim(Bus::Master).expect("a free slot");
        source.push(&[1.0]);

        let mut sink = VecSink(vec![-7.0; 4]);
        mixer.render(&mut sink);

        assert_eq!(
            sink.0,
            vec![1.0, 0.0, 0.0, 0.0],
            "the one sample, then silence — not a repeat and not the buffer's old contents"
        );
    }

    #[test]
    fn a_full_ring_refuses_rather_than_overwriting() {
        let mixer = mono_mixer();
        let source = mixer.claim(Bus::Master).expect("a free slot");
        let accepted = source.push(&vec![0.5; SOURCE_CAPACITY + 100]);

        assert_eq!(accepted, SOURCE_CAPACITY);
        assert_eq!(source.free(), 0);
        assert_eq!(source.push(&[0.25]), 0, "a full ring takes nothing");

        let mut sink = VecSink(vec![0.0; 1]);
        mixer.render(&mut sink);
        assert_eq!(sink.0, vec![0.5], "the oldest sample survived the refusal");
    }

    #[test]
    fn a_ring_wraps_past_its_capacity() {
        let mixer = mono_mixer();
        let source = mixer.claim(Bus::Master).expect("a free slot");
        let mut sink = VecSink(vec![0.0; SOURCE_CAPACITY]);
        for round in 0..3 {
            let value = 0.1 * (round + 1) as f32;
            assert_eq!(source.push(&vec![value; SOURCE_CAPACITY]), SOURCE_CAPACITY);
            mixer.render(&mut sink);
            assert!(
                sink.0.iter().all(|sample| (sample - value).abs() < 1e-6),
                "round {round} read back what it wrote"
            );
        }
    }

    #[test]
    fn only_max_sources_may_be_claimed() {
        let mixer = mono_mixer();
        let claimed: Vec<_> = (0..MAX_SOURCES)
            .map(|_| mixer.claim(Bus::Voice).expect("a free slot"))
            .collect();
        assert!(mixer.claim(Bus::Voice).is_none(), "the fifth is refused");
        // A refusal must not have spent a slot that was never there: refusing twice is
        // still a refusal, and dropping the handles frees nothing (a slot is claimed for
        // the life of the mixer).
        assert!(mixer.claim(Bus::Voice).is_none());
        assert_eq!(claimed.len(), MAX_SOURCES);
    }

    /// Asserted through what a gain *does* rather than through a getter, which is the
    /// only place the clamp could still be wrong after being applied.
    #[test]
    fn a_gain_outside_its_range_is_clamped_and_a_nan_is_silence() {
        for (set, heard) in [(4.0, 1.0), (-1.0, 0.0), (f32::NAN, 0.0)] {
            let mixer = mono_mixer();
            let source = mixer.claim(Bus::Master).expect("a free slot");
            mixer.set_gain(Bus::Master, set);
            source.push(&[1.0]);

            let mut sink = VecSink(vec![0.0; 1]);
            mixer.render(&mut sink);

            assert_eq!(sink.0, vec![heard], "a gain of {set} is heard as {heard}");
        }
    }

    #[test]
    fn a_loud_sum_is_clamped_into_range() {
        let mixer = mono_mixer();
        let one = mixer.claim(Bus::Master).expect("a free slot");
        let two = mixer.claim(Bus::Master).expect("a second free slot");
        one.push(&[1.0]);
        two.push(&[1.0]);

        let mut sink = VecSink(vec![0.0; 1]);
        mixer.render(&mut sink);

        assert_eq!(sink.0, vec![1.0]);
    }

    #[test]
    fn one_mono_sample_reaches_every_channel_of_a_frame() {
        let mixer = Arc::new(Mixer::new());
        mixer.set_format(DEFAULT_SAMPLE_RATE, 2);
        let source = mixer.claim(Bus::Master).expect("a free slot");
        source.push(&[0.5, 0.25]);

        let mut sink = VecSink(vec![0.0; 4]);
        mixer.render(&mut sink);

        assert_eq!(sink.0, vec![0.5, 0.5, 0.25, 0.25]);
    }

    #[test]
    fn the_render_path_allocates_nothing() {
        let mixer = mono_mixer();
        let source = mixer.claim(Bus::Master).expect("a free slot");
        let mut sink = VecSink(vec![0.0; 512]);
        // Warm every path once, so nothing counted below is a first-call initialisation.
        source.push(&vec![0.25; 512]);
        mixer.render(&mut sink);

        let samples = vec![0.25; 512];
        let before = allocations();
        for _ in 0..64 {
            source.push(&samples);
            mixer.set_gain(Bus::Master, 0.5);
            mixer.render(&mut sink);
        }
        assert_eq!(
            allocations() - before,
            0,
            "pushing, setting a gain and rendering must not allocate"
        );
    }

    /// **The hand-written list against the thing it claims to enumerate.** `Bus::ALL` cannot
    /// enforce itself — no stable Rust enumerates variants, and an array of enum values is
    /// exactly what `client/src/ui/status.rs` was seven members short of behind a doc comment
    /// claiming the compiler would notice. What can be checked is that every index in
    /// `0..ALL.len()` is claimed by exactly one member and that `from_index` agrees, which is
    /// what a member missing from this list would break: its index would be somebody else's.
    #[test]
    fn every_bus_is_named_in_all_exactly_once_and_from_index_agrees() {
        let mut seen = [0usize; Bus::ALL.len()];
        for bus in Bus::ALL {
            let index = bus.index();
            assert!(index < Bus::ALL.len(), "{bus:?} indexes outside the array");
            seen[index] += 1;
            assert_eq!(
                Bus::from_index(index as u8),
                bus,
                "{bus:?} does not come back from its own index"
            );
        }
        assert_eq!(seen, [1; Bus::ALL.len()], "an index is shared or unclaimed");
        // The one wildcard in the file: anything out of range is the master, deliberately.
        assert_eq!(Bus::from_index(Bus::ALL.len() as u8), Bus::Master);
        assert_eq!(Bus::from_index(u8::MAX), Bus::Master);
    }

    /// **The policy as three properties rather than three lists.** Voice must not lose a slot
    /// to an ambience bed, which is what the middle assertion is: no arrangement of the buses
    /// a world feeds can reach a slot somebody is being heard through.
    #[test]
    fn the_allocation_policy_says_what_it_claims_to_say() {
        for bus in Bus::ALL {
            assert_eq!(
                bus.may_take_the_last_slot(),
                bus.steal_order().is_none(),
                "{bus:?} is protected from one half of the policy and not the other"
            );
        }
        assert_eq!(Bus::Voice.steal_order(), None, "voice can be stolen from");
        assert_eq!(Bus::Master.steal_order(), None);
        // Ambience first, then music, then effects — and each strictly, so the ordering is
        // asserted rather than the membership.
        let ambience = Bus::Ambience.steal_order().expect("ambience is stealable");
        let music = Bus::Music.steal_order().expect("music is stealable");
        let sfx = Bus::Sfx.steal_order().expect("effects are stealable");
        assert!(ambience < music && music < sfx, "{ambience} {music} {sfx}");
    }

    /// Each new gain moves its own bus and leaves the others exactly where they were.
    ///
    /// Rendered rather than read back off the mixer, for the reason `audio/mod.rs`'s voice
    /// test gives: "the gain was stored" is a claim about a field, and this is a claim about
    /// what a player hears.
    #[test]
    fn each_bus_gain_scales_its_own_sources_and_then_the_master() {
        for bus in Bus::ALL {
            let mixer = mono_mixer();
            let under_test = mixer.claim(bus).expect("a free slot");
            let control = mixer
                .claim(if matches!(bus, Bus::Sfx) {
                    Bus::Ambience
                } else {
                    Bus::Sfx
                })
                .expect("a second free slot");
            mixer.set_gain(Bus::Master, 0.5);
            mixer.set_gain(bus, 0.5);

            under_test.push(&[1.0]);
            control.push(&[0.0]);
            let heard = rendered(&mixer, 1)[0];
            // The master applies once on top of the bus gain — except on the master itself,
            // where there is no second multiply to make.
            let want = if matches!(bus, Bus::Master) {
                0.5
            } else {
                0.25
            };
            assert!(
                (heard - want).abs() < 1e-6,
                "{bus:?} at half under a half master was heard at {heard} rather than {want}"
            );

            // And the control source, which is on a bus nobody touched, is still at unity
            // under the master alone.
            under_test.push(&[0.0]);
            control.push(&[1.0]);
            let untouched = rendered(&mixer, 1)[0];
            assert!(
                (untouched - 0.5).abs() < 1e-6,
                "turning {bus:?} down moved a source that is not on it: {untouched}"
            );
        }
    }

    /// Renders `samples` mono samples and answers what came out, for the duck tests.
    fn rendered(mixer: &Arc<Mixer>, samples: usize) -> Vec<f32> {
        let mut sink = VecSink(vec![0.0; samples]);
        mixer.render(&mut sink);
        sink.0
    }

    /// **The duck reaches the two buses it is for and neither of the three it is not.**
    #[test]
    fn the_duck_takes_down_music_and_ambience_and_nothing_else() {
        for bus in Bus::ALL {
            let mixer = mono_mixer();
            let source = mixer.claim(bus).expect("a free slot");
            mixer.set_duck(0.25);
            // 160 ms of 10 ms blocks, comfortably past the 80 ms attack, so what is measured
            // is the depth the duck reaches rather than where its ramp had got to.
            for _ in 0..16 {
                source.push(&[1.0; 480]);
                let _ = rendered(&mixer, 480);
            }
            source.push(&[1.0]);
            let heard = rendered(&mixer, 1)[0];
            let want = if bus.ducks() { 0.25 } else { 1.0 };
            assert!(
                (heard - want).abs() < 1e-5,
                "{bus:?} ducks() is {} but it was heard at {heard} rather than {want}",
                bus.ducks()
            );
        }
    }

    /// **A duck of `1.0` is the identity, not an approximation of one.**
    ///
    /// The same property `a_source_with_nothing_in_its_way_is_reconstructed_exactly` holds
    /// for the occlusion filter, and for the same reason: a stage that is in front of every
    /// source has to be provably transparent when it is doing nothing, or every player who
    /// has turned ducking off is paying for a feature they declined.
    #[test]
    fn music_with_ducking_turned_off_is_the_samples_that_were_pushed() {
        let mixer = mono_mixer();
        let source = mixer.claim(Bus::Music).expect("a free slot");
        mixer.set_duck(1.0);
        let input = tone(440.0, 512);
        source.push(&input);

        let out = rendered(&mixer, input.len());
        for (heard, want) in out.iter().zip(&input) {
            assert!((heard - want).abs() < 1e-6, "{heard} against {want}");
        }
    }

    /// A step implementation arrives on its first block, which is what this arithmetic rules
    /// out — and the release is asserted as well as the attack, because a duck that never
    /// comes back is the failure a listener actually notices.
    #[test]
    fn the_duck_ramps_in_both_directions_and_comes_back_more_slowly_than_it_went() {
        let mixer = mono_mixer();
        let _source = mixer.claim(Bus::Music).expect("a free slot");
        // 480 frames at 48 kHz is 10 ms, an eighth of the attack.
        let block = 480;

        mixer.set_duck(0.4);
        let _ = rendered(&mixer, block);
        let after_one = mixer.reached_duck();
        assert!(
            (after_one - 0.875).abs() < 1e-4,
            "one 10 ms block of an 80 ms attack reached {after_one}"
        );
        assert!(after_one < 1.0, "the duck did not move at all");
        for _ in 0..8 {
            let _ = rendered(&mixer, block);
        }
        assert!(
            (mixer.reached_duck() - 0.4).abs() < 1e-5,
            "the attack did not arrive: {}",
            mixer.reached_duck()
        );

        // And back. One block of release covers a fifth of what one block of attack did,
        // which is `DUCK_RELEASE_SECONDS / DUCK_ATTACK_SECONDS` and is the asymmetry that
        // stops a bed pumping between two words.
        mixer.set_duck(1.0);
        let _ = rendered(&mixer, block);
        let up = mixer.reached_duck() - 0.4;
        let down = 1.0 - after_one;
        assert!(up > 0.0, "the duck did not come back at all");
        assert!(
            (down / up - DUCK_RELEASE_SECONDS / DUCK_ATTACK_SECONDS).abs() < 1e-3,
            "the release is {up} to the attack's {down}"
        );
    }

    /// **The world's buses cannot spend the reserve, and voice can.**
    #[test]
    fn the_world_buses_stop_at_the_reserve_and_voice_may_take_what_is_left() {
        let mixer = mono_mixer();
        let held: Vec<SourceHandle> = std::iter::from_fn(|| mixer.claim(Bus::Ambience)).collect();
        assert_eq!(
            held.len(),
            MAX_SOURCES - VOICE_RESERVE,
            "ambience took {} slots of {MAX_SOURCES}",
            held.len()
        );
        assert!(
            mixer.claim(Bus::Music).is_none(),
            "another world bus spent the reserve ambience could not"
        );
        assert!(mixer.claim(Bus::Sfx).is_none());

        // The whole point of the reserve: what it held back is there for voice.
        let voices: Vec<SourceHandle> = std::iter::from_fn(|| mixer.claim(Bus::Voice)).collect();
        assert_eq!(
            voices.len(),
            VOICE_RESERVE,
            "voice found {} of the {VOICE_RESERVE} slots held for it",
            voices.len()
        );
        drop(held);
        drop(voices);
    }

    /// **Fill the pool, and assert voice is never the slot a steal takes.**
    ///
    /// The acceptance criterion, directly: sixteen slots, half of them people being heard,
    /// and a bus below them asking for one. Nothing is taken, every speaker keeps their slot,
    /// and the claim is refused rather than blocking.
    #[test]
    fn a_world_bus_that_cannot_claim_never_takes_a_slot_off_voice() {
        let mixer = mono_mixer();
        let voices: Vec<SourceHandle> = std::iter::from_fn(|| mixer.claim(Bus::Voice)).collect();
        assert_eq!(voices.len(), MAX_SOURCES, "voice may fill the pool");

        for bus in [Bus::Ambience, Bus::Music, Bus::Sfx] {
            assert!(
                mixer.claim(bus).is_none(),
                "{bus:?} was handed a slot out of a pool that is entirely voice"
            );
        }
        assert!(
            voices.iter().all(|held| held.live()),
            "a world bus took a slot away from somebody being heard"
        );
        for index in 0..MAX_SOURCES {
            assert_eq!(
                mixer.slot_bus(index),
                Bus::Voice,
                "slot {index} changed bus"
            );
            assert_eq!(mixer.slot_state(index), SlotState::Live);
        }

        // And the refusal is an answer rather than a wait: it can be asked again and it says
        // the same thing, having spent nothing.
        assert!(mixer.claim(Bus::Sfx).is_none());
        assert!(voices.iter().all(|held| held.live()));
        drop(voices);
    }

    /// **Ambience is what a voice takes when there is nothing free**, and the stolen owner
    /// finds out by going inert rather than by writing into somebody else's audio.
    #[test]
    fn a_voice_claim_takes_the_ambience_bed_and_leaves_the_stolen_handle_inert() {
        let mixer = mono_mixer();
        // Exact counts rather than filling until refused, and that is a property worth
        // naming: **the refusal is what performs the revocation**, so a loop that claims
        // until it is told no has already taken a bed by the time it stops. Every caller in
        // this client claims once for one sound, which is the shape the policy is written for.
        let beds: Vec<SourceHandle> = (0..MAX_SOURCES - VOICE_RESERVE)
            .map(|_| mixer.claim(Bus::Ambience).expect("a free slot"))
            .collect();
        let voices: Vec<SourceHandle> = (0..VOICE_RESERVE)
            .map(|_| mixer.claim(Bus::Voice).expect("a free slot"))
            .collect();
        assert_eq!(beds.len() + voices.len(), MAX_SOURCES, "the pool is full");
        assert_eq!(mixer.free_slots(), 0);

        // The claim that finds nothing free: refused, and the slot it took is not this
        // caller's. A one-shot that cannot claim is dropped rather than queued.
        assert!(mixer.claim(Bus::Voice).is_none());
        let taken: Vec<&SourceHandle> = beds.iter().filter(|bed| bed.revoked()).collect();
        assert_eq!(
            taken.len(),
            1,
            "{} beds were taken for one claim",
            taken.len()
        );
        assert!(
            voices.iter().all(|held| held.live()),
            "the revocation reached a voice slot"
        );

        // The revoked owner writes into nothing at all, and, the property the review on #996
        // asked for, **nobody else can be given the slot while that owner still holds it**,
        // however many blocks the callback runs. So there is no interleaving in which a stale
        // push reaches a new owner's ring.
        let ghost = taken[0];
        assert!(!ghost.live());
        assert_eq!(ghost.push(&[1.0; 8]), 0, "a revoked handle still pushes");
        assert_eq!(ghost.free(), 0);
        for _ in 0..8 {
            let _ = rendered(&mixer, 64);
            assert!(
                mixer.claim(Bus::Voice).is_none(),
                "a slot was handed out from under a handle that still holds it"
            );
        }

        // It comes back when, and only when, the owner lets go, which is the whole of what a
        // producer owes the pool.
        drop(beds);
        let _ = rendered(&mixer, 8);
        let replacement = mixer.claim(Bus::Voice).expect("the freed slot");
        assert!(replacement.live());
        // And it comes back empty, like every other recycled slot.
        assert_eq!(rendered(&mixer, 8), vec![0.0; 8]);
        drop(voices);
        drop(replacement);
    }

    /// **A revoked slot is silent from the next block**, which is what "stolen" has to mean
    /// even though the slot itself is not reusable yet. The two halves are separable, and
    /// this is the one a listener hears.
    #[test]
    fn a_revoked_source_goes_quiet_immediately_even_though_its_slot_has_not_come_back() {
        let mixer = mono_mixer();
        let bed = mixer.claim(Bus::Ambience).expect("a free slot");
        bed.push(&[0.5; 64]);
        assert_eq!(rendered(&mixer, 4), vec![0.5; 4], "the bed is audible");

        assert!(mixer.revoke(0, mixer.sources[0].slot.load(Ordering::Acquire)));
        assert!(bed.revoked());
        assert_eq!(
            rendered(&mixer, 8),
            vec![0.0; 8],
            "a revoked bed was still heard"
        );
        assert_eq!(
            mixer.slot_state(0),
            SlotState::Revoked,
            "and it is not free"
        );
        drop(bed);
    }

    /// **Music off is no source at all, not a source at zero.** Both halves: a claim is
    /// refused while it is off, and one already granted is taken back.
    #[test]
    fn a_bus_that_is_off_holds_no_source_and_hands_out_none() {
        let mixer = mono_mixer();
        let playing = mixer.claim(Bus::Music).expect("a free slot");
        playing.push(&[1.0; 8]);
        assert!(playing.live());

        mixer.set_enabled(Bus::Music, false);
        assert!(!playing.live(), "the music source survived music being off");
        assert!(
            mixer.claim(Bus::Music).is_none(),
            "a slot was handed out on a bus that is off"
        );
        // Nothing of it is heard either, which is the half a gain of zero would also give —
        // and the half above is the one it would not.
        let _ = rendered(&mixer, 8);
        assert_eq!(rendered(&mixer, 8), vec![0.0; 8]);
        assert_eq!(
            playing.push(&[1.0; 8]),
            0,
            "a revoked generator still pushes"
        );

        // The slot comes back when its owner lets go, not before: the contract every
        // revocation carries.
        drop(playing);
        let _ = rendered(&mixer, 8);
        assert_eq!(mixer.slot_state(0), SlotState::Free, "the slot came back");

        // The negative control: with the bus back on, the same call is answered.
        mixer.set_enabled(Bus::Music, true);
        let again = mixer.claim(Bus::Music).expect("music is on again");
        again.push(&[0.5; 4]);
        assert_eq!(rendered(&mixer, 4), vec![0.5; 4]);
    }

    /// **The reserve holds against concurrent claims, which is what the review on #996 found
    /// it did not.** Sixteen threads all asking for a world bus at once: the pool-wide ceiling
    /// is one atomic location, so however they interleave, the number granted cannot exceed
    /// what the world buses are allowed to hold.
    ///
    /// **This assertion cannot fail spuriously, and that is deliberate.** It is an upper bound
    /// that the fixed implementation can never cross, so a flaky machine makes it slower and
    /// never redder. What it cannot promise is to catch the old defect on every run — a race
    /// has to be lost for that — which is why the property below it is asserted too: whatever
    /// the threads did, `VOICE_RESERVE` slots are still there for voice afterwards.
    #[test]
    fn a_crowd_of_concurrent_world_claims_cannot_cross_the_reserve() {
        let mixer = mono_mixer();
        let granted: Vec<SourceHandle> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..MAX_SOURCES)
                .map(|n| {
                    let mixer = Arc::clone(&mixer);
                    scope.spawn(move || {
                        let bus = match n % 3 {
                            0 => Bus::Ambience,
                            1 => Bus::Music,
                            _ => Bus::Sfx,
                        };
                        mixer.claim(bus)
                    })
                })
                .collect();
            handles
                .into_iter()
                .filter_map(|handle| handle.join().expect("no claim panics"))
                .collect()
        });

        assert!(
            granted.len() <= MAX_SOURCES - VOICE_RESERVE,
            "{} world sources were granted out of a budget of {}",
            granted.len(),
            MAX_SOURCES - VOICE_RESERVE
        );
        // The property the whole reserve exists for, asserted rather than inferred from the
        // count: voice can still be heard by as many people as it was promised.
        let voices: Vec<SourceHandle> = (0..VOICE_RESERVE)
            .map(|_| mixer.claim(Bus::Voice).expect("the reserve is still there"))
            .collect();
        assert_eq!(voices.len(), VOICE_RESERVE);
        drop(granted);
        drop(voices);
    }

    /// **The reserve bounds what the world buses *hold*, not what happens to be free — and
    /// that difference is what makes this test discriminate.**
    ///
    /// Counting free slots and counting held ones agree from an empty pool, so the obvious
    /// test passes against either. They disagree the moment voice is holding most of the
    /// pool: with ten voices out, six slots are free, and a rule that refuses below
    /// `VOICE_RESERVE` free would refuse the world entirely — silencing the whole world to
    /// protect a reserve voice has already exceeded. The pool-wide counter allows it, because
    /// the promise is "voice may always obtain `VOICE_RESERVE`", and voice has ten.
    ///
    /// This is the deterministic half of the review finding on #996. The threaded test above
    /// bounds the outcome under real concurrency; this one fails outright against the
    /// free-slot rule that finding was about.
    #[test]
    fn the_reserve_counts_what_the_world_holds_rather_than_what_is_free() {
        let mixer = mono_mixer();
        let voices: Vec<SourceHandle> = (0..10)
            .map(|_| mixer.claim(Bus::Voice).expect("voice may fill the pool"))
            .collect();
        assert_eq!(mixer.free_slots(), MAX_SOURCES - 10);
        assert!(
            mixer.free_slots() < VOICE_RESERVE,
            "the case only discriminates while fewer than the reserve are free"
        );

        let bed = mixer.claim(Bus::Ambience);
        assert!(
            bed.is_some(),
            "the world was refused a slot to protect a reserve voice already exceeds"
        );

        // And the ceiling still holds from here: the world may reach its share and no more.
        let rest: Vec<SourceHandle> = std::iter::from_fn(|| mixer.claim(Bus::Music))
            .take(MAX_SOURCES)
            .collect();
        assert_eq!(
            rest.len() + 1,
            MAX_SOURCES - 10,
            "the world took {} of the six slots that were free",
            rest.len() + 1
        );
        drop(voices);
        drop(bed);
        drop(rest);
    }

    /// **The budget is given back exactly once per slot, on the one transition every released
    /// slot makes.** Spend the whole world budget, let it go, and spend it again — a leak
    /// shows up as the second round coming back short, and a double refund as a third round
    /// that should not exist.
    #[test]
    fn the_world_budget_comes_back_with_the_slot_and_only_once() {
        let mixer = mono_mixer();
        for round in 0..3 {
            let held: Vec<SourceHandle> = std::iter::from_fn(|| mixer.claim(Bus::Ambience))
                .take(MAX_SOURCES)
                .collect();
            assert_eq!(
                held.len(),
                MAX_SOURCES - VOICE_RESERVE,
                "round {round} was granted {} slots",
                held.len()
            );
            drop(held);
            // The callback is what frees a released slot, and freeing is what returns the
            // budget. Several blocks, because a released slot needs one and the loop above
            // released many.
            for _ in 0..4 {
                let _ = rendered(&mixer, 8);
            }
            assert_eq!(
                mixer.free_slots(),
                MAX_SOURCES,
                "round {round} leaked a slot"
            );
        }
    }

    /// **The half of the claim that answers the review finding, tested on its own.**
    ///
    /// `claim` refuses a disabled bus at its first line, so no single-threaded call can reach
    /// the interleaving the review on #996 named — the one where a claim passes that check,
    /// the bus is switched off and swept, and the claim's slot lands afterwards. What *is*
    /// reachable directly is `take_a_free_slot`, which carries the second half of the pair:
    /// it wins the slot and only keeps it if the bus is still on. Calling it against a bus
    /// that is already off puts it in exactly the state that interleaving leaves it in, and
    /// it must come back empty-handed having given the slot and the budget back.
    ///
    /// **This is the discriminating test.** The threaded one below asserts the invariant under
    /// real concurrency and is worth having, but it passes against an implementation with no
    /// re-check at all — the window is too narrow to lose reliably — so it is not evidence on
    /// its own, and is not presented as any.
    #[test]
    fn a_slot_won_on_a_bus_that_is_off_is_given_straight_back() {
        let mixer = mono_mixer();
        mixer.set_enabled(Bus::Music, false);

        assert!(
            mixer.take_a_free_slot(Bus::Music).is_none(),
            "a slot was kept on a bus that is switched off"
        );

        // Neither the slot nor the budget leaked: the slot goes back through the callback
        // like any other released one, and the budget goes with it.
        let _ = rendered(&mixer, 8);
        assert_eq!(mixer.free_slots(), MAX_SOURCES, "the slot leaked");
        mixer.set_enabled(Bus::Music, true);
        let world: Vec<SourceHandle> = std::iter::from_fn(|| mixer.claim(Bus::Music))
            .take(MAX_SOURCES)
            .collect();
        assert_eq!(
            world.len(),
            MAX_SOURCES - VOICE_RESERVE,
            "the budget leaked: {} of {} available",
            world.len(),
            MAX_SOURCES - VOICE_RESERVE
        );
        drop(world);
    }

    /// The same invariant under two threads actually racing. See the note above: this is a
    /// belt-and-braces check on the assembled pair, not the evidence for either half.
    #[test]
    fn a_bus_switched_off_under_a_claim_is_left_holding_no_live_source() {
        for _ in 0..64 {
            let mixer = mono_mixer();
            let claimed = std::thread::scope(|scope| {
                let claimer = {
                    let mixer = Arc::clone(&mixer);
                    scope.spawn(move || {
                        let mut held = Vec::new();
                        for _ in 0..MAX_SOURCES {
                            if let Some(source) = mixer.claim(Bus::Music) {
                                held.push(source);
                            }
                        }
                        held
                    })
                };
                let switcher = {
                    let mixer = Arc::clone(&mixer);
                    scope.spawn(move || mixer.set_enabled(Bus::Music, false))
                };
                switcher.join().expect("the switch does not panic");
                claimer.join().expect("no claim panics")
            });

            // Whatever the interleaving, music is off and nothing is live on it. A handle may
            // still exist — a revoked slot is its owner's until it drops — but no slot is in
            // a state the callback would render on a bus that is switched off.
            assert!(
                claimed.iter().all(|held| !held.live()),
                "a live music source survived music being switched off"
            );
            for index in 0..MAX_SOURCES {
                assert!(
                    mixer.slot_state(index) != SlotState::Live
                        || mixer.slot_bus(index) != Bus::Music,
                    "slot {index} is live on a bus that is off"
                );
            }
        }
    }

    /// **A producer that keeps pushing through a revoked handle cannot reach anybody else.**
    ///
    /// The finding on `SourceHandle::live` was that the check and the write are two steps, so
    /// a producer could pass the check and then write into a ring that had since been handed
    /// on. The fix is structural rather than a wider check: the slot cannot be handed on at
    /// all while this handle exists. This is that property under a thread that never stops
    /// pushing, with the pool exhausted and claims running against it throughout.
    #[test]
    fn a_stale_producer_cannot_write_into_a_slot_somebody_else_was_given() {
        let mixer = mono_mixer();
        let bed = mixer.claim(Bus::Ambience).expect("a free slot");
        let bed_index = 0;
        let _rest: Vec<SourceHandle> = (0..MAX_SOURCES - 1)
            .map(|_| mixer.claim(Bus::Voice).expect("a free slot"))
            .collect();

        // Revoke the bed, then keep pushing through it while claims and the callback run.
        assert!(mixer.claim(Bus::Voice).is_none(), "the pool is full");
        assert!(bed.revoked(), "the bed was not the one taken");

        std::thread::scope(|scope| {
            let mixer = Arc::clone(&mixer);
            let bed = &bed;
            scope.spawn(move || {
                for _ in 0..512 {
                    // Every one of these is refused, and even were it not, the slot is still
                    // this handle's — which is the property under test.
                    let _ = bed.push(&[1.0; 16]);
                    let _ = mixer.claim(Bus::Voice);
                    let mut sink = VecSink(vec![0.0; 32]);
                    mixer.render(&mut sink);
                }
            });
        });

        // The slot never left this handle, so nothing else was ever given it.
        assert!(
            bed.revoked(),
            "the revoked slot changed hands under its owner"
        );
        assert_eq!(mixer.slot_bus(bed_index), Bus::Ambience);
        drop(bed);
    }

    /// **The mono fold uses the pan the render path already skips, so both ears carry the
    /// same sum on a device that still has two of them.**
    #[test]
    fn the_mono_fold_puts_one_image_in_both_ears_of_a_stereo_device() {
        let ears = |mono: bool| {
            let mixer = stereo_mixer();
            mixer.set_mono(mono);
            let source = mixer.claim(Bus::Master).expect("a free slot");
            source.place(placed(
                1.0,
                spatial::pan_gains(std::f32::consts::FRAC_PI_2),
                0.0,
                1.0,
            ));
            source.push(&[1.0; 4]);
            let mut sink = VecSink(vec![0.0; 8]);
            mixer.render(&mut sink);
            let left: f32 = sink.0.iter().step_by(2).sum();
            let right: f32 = sink.0.iter().skip(1).step_by(2).sum();
            (left, right)
        };

        // The negative control first: without the fold, a hard-right source is in one ear
        // only, so the assertion below is about the fold rather than about the pan.
        let (left, right) = ears(false);
        assert!(left < 0.1 && right > 3.9, "unfolded: {left} and {right}");

        let (left, right) = ears(true);
        assert!(
            (left - right).abs() < 1e-5,
            "folded, the ears differ: {left} and {right}"
        );
        assert!(right > 3.9, "the fold took the source's level with it");
    }

    #[test]
    fn the_counting_allocator_can_actually_see_an_allocation() {
        // The negative control the test above needs: an assertion of "zero allocations"
        // is worthless from an instrument that reports zero for everything.
        let before = allocations();
        let counted = std::hint::black_box(vec![0u8; 1024]);
        assert!(!counted.is_empty());
        assert!(allocations() > before, "the allocator counts");
    }
}
