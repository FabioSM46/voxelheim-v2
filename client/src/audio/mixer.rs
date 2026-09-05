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
//! `Voice` has a gain of its own, applied to the sources on it; `Master` is the output
//! stage, so its gain applies to the sum of everything after the per-bus gains. A source
//! claimed directly onto `Master` — the speaker test is the first — is therefore scaled
//! once, by the master gain, and never twice.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicUsize, Ordering};

use super::spatial::{self, BANDS, HIGH_CROSSOVER_HZ, LOW_CROSSOVER_HZ, PanGains, Placement};

/// How many sources may feed the mixer at once.
///
/// A fixed count, because the alternative is a `Vec` the callback would walk while a Bevy
/// system reallocated it — and because every per-block scratch array in [`Mixer::render`] is
/// sized from it on the stack, which is what keeps the render path allocation-free now that
/// there is per-source state to prepare.
///
/// **Sixteen since #854, and the three that are spoken for are what the number is set
/// from.** The speaker test holds one for the client's life, the local voice monitor holds
/// another, and `audio/heard.rs` holds a third for every speaker it could not give a slot of
/// its own — so thirteen people can be heard from where they are standing, and a fourteenth
/// is heard unpositioned rather than not at all. Raising it costs
/// [`SOURCE_CAPACITY`] × 4 bytes per slot and nothing else.
pub const MAX_SOURCES: usize = 16;

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
/// Two, and deliberately only two. An SFX bus and a music bus arrive with the features
/// that need them; a bus nothing feeds is a gain nobody can hear moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bus {
    /// Proximity voice. Scaled by its own gain, then by the master.
    Voice,
    /// The output stage. A source claimed here is scaled by the master gain alone.
    Master,
}

impl Bus {
    /// Every bus, in the order [`Mixer`] stores their gains.
    pub const ALL: [Self; 2] = [Self::Voice, Self::Master];

    /// This bus's index into the gain array.
    const fn index(self) -> usize {
        match self {
            Self::Voice => 0,
            Self::Master => 1,
        }
    }

    /// The bus `index` names, or [`Self::Master`] for anything else.
    ///
    /// Total rather than fallible because the callback reads it: the only writer is
    /// [`Mixer::claim`], which writes [`Self::index`], so an out-of-range value is
    /// unreachable — and the callback is no place to answer an unreachable state with a
    /// panic.
    const fn from_index(index: u8) -> Self {
        match index {
            0 => Self::Voice,
            _ => Self::Master,
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
/// | [`Self::Dirty`] | the output callback, after clearing the slot | `Free` |
///
/// A slot is claimable only while `Free`, and `Free` is written only by the thread that has
/// just finished clearing it. There is no state a second reader can catch half-written,
/// because there is nothing to read twice.
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
    /// A [`Bus::index`]. Written by [`Mixer::claim`], read by the callback.
    bus: AtomicU8,
    /// A [`SlotState`]. The one thing that decides who may touch the rest.
    state: AtomicU8,
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
            bus: AtomicU8::new(Bus::Master.index() as u8),
            // A slot nobody has used needs no clearing, which is what makes the first
            // `MAX_SOURCES` claims of a fresh mixer succeed with no callback anywhere.
            state: AtomicU8::new(SlotState::Free as u8),
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
    fn recycle(&self) {
        self.ring.skip();
        self.low_state.store(0.0f32.to_bits(), Ordering::Relaxed);
        self.mid_state.store(0.0f32.to_bits(), Ordering::Relaxed);
        self.occlusion.store(0.0f32.to_bits(), Ordering::Relaxed);
        self.place(Placement::UNPOSITIONED);
        let _ = self.state.compare_exchange(
            SlotState::Dirty as u8,
            SlotState::Free as u8,
            Ordering::Release,
            Ordering::Relaxed,
        );
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

    /// Takes one of the [`MAX_SOURCES`] slots, or `None` when they are all taken.
    ///
    /// The handle is the only way to write samples, and there is exactly one per slot —
    /// which is what makes each ring's producer single, as its ordering assumes.
    pub fn claim(self: &Arc<Self>, bus: Bus) -> Option<SourceHandle> {
        for (index, source) in self.sources.iter().enumerate() {
            // **One compare-exchange, and it is the whole decision.** Only a `Free` slot can
            // be won, and a slot is `Free` only after the callback has cleared it — a
            // released one still holding the previous owner's audio is `Dirty` and this
            // fails on it, as it does on one somebody else is using. The acquire pairs with
            // `recycle`'s release, so the winner sees the cleared ring and the reset
            // placement rather than whatever was there before.
            if source
                .state
                .compare_exchange(
                    SlotState::Free as u8,
                    SlotState::Live as u8,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                )
                .is_err()
            {
                continue;
            }
            // After the slot is won, and safely: the callback may render it for one block
            // before this store lands, and renders `0.0` whichever bus it reads — `recycle`
            // emptied the ring and pushing needs the handle this has not returned yet, which
            // the type system enforces rather than a comment.
            source.bus.store(bus.index() as u8, Ordering::Relaxed);
            return Some(SourceHandle {
                mixer: Arc::clone(self),
                index,
            });
        }
        None
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
        match self.sources[index].state.load(Ordering::Acquire) {
            0 => SlotState::Free,
            1 => SlotState::Live,
            _ => SlotState::Dirty,
        }
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
    /// **A mono device is not panned at all.** One channel cannot carry a stereo image, and
    /// the two honest answers are to average the pair — which makes a hard-panned voice 3 dB
    /// quieter than a centred one for no reason a listener could act on — or to skip the pan.
    /// This skips it: the distance gain and the occlusion filter still apply, because those
    /// are audible on one loudspeaker, and the direction simply is not.
    pub fn render(&self, sink: &mut impl Sink) {
        let channels = self.channels.load(Ordering::Relaxed).max(1) as usize;
        let sample_rate = self.sample_rate.load(Ordering::Relaxed).max(1);
        let voice = f32::from_bits(self.gains[Bus::Voice.index()].load(Ordering::Relaxed));
        let master = f32::from_bits(self.gains[Bus::Master.index()].load(Ordering::Relaxed));
        let low_crossover = f32::from_bits(self.crossover_low.load(Ordering::Relaxed));
        let high_crossover = f32::from_bits(self.crossover_high.load(Ordering::Relaxed));

        let block = sink.block();
        let frames = block.len() / channels;
        let elapsed = frames as f32 / sample_rate as f32;

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
            let state = source.state.load(Ordering::Acquire);
            if state == SlotState::Dirty as u8 {
                source.recycle();
                continue;
            }
            if state != SlotState::Live as u8 {
                continue;
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

            // The master gain is the output stage below, so a source on the master bus is
            // scaled once rather than squared.
            let bus = match Bus::from_index(source.bus.load(Ordering::Relaxed)) {
                Bus::Master => 1.0,
                Bus::Voice => voice,
            };
            // On a mono device the pan is skipped and the distance gain is not: one is
            // inaudible on one loudspeaker and the other plainly is not.
            let (pan_left, pan_right) = if channels == 1 {
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

/// The producer end of one claimed source.
///
/// Held by a Bevy system. Cloning it is deliberately impossible: two producers on one ring
/// is the assumption [`Ring`] is built on, and there is no way to get a second handle to a
/// slot [`Mixer::claim`] has already given away.
///
/// **Dropping it gives the slot back**, which is what lets `audio/heard.rs` keep one source
/// per speaker out of a fixed pool. Before #854 a claim was for the mixer's life, because
/// the three things that claimed one lived as long as the client; a speaker does not.
#[derive(Debug)]
pub struct SourceHandle {
    mixer: Arc<Mixer>,
    index: usize,
}

impl SourceHandle {
    /// Appends as much of `samples` as fits, and answers how much that was.
    pub fn push(&self, samples: &[f32]) -> usize {
        self.mixer.sources[self.index].ring.push(samples)
    }

    /// How many more samples [`Self::push`] would accept right now.
    pub fn free(&self) -> usize {
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
    /// A plain store rather than an exchange: `Live` is this handle's state to leave, there
    /// is exactly one handle per slot, and nothing else writes `Dirty`. The release is what
    /// makes the last samples this owner pushed visible to the callback that discards them.
    fn drop(&mut self) {
        self.mixer.sources[self.index]
            .state
            .store(SlotState::Dirty as u8, Ordering::Release);
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
