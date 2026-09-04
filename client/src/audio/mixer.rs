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

/// How many sources may feed the mixer at once.
///
/// A fixed count, because the alternative is a `Vec` the callback would walk while a Bevy
/// system reallocated it. Four is what the audio iteration needs — the speaker test, the
/// local voice monitor, and two remote speakers — and raising it costs one array slot.
pub const MAX_SOURCES: usize = 4;

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
}

/// One slot in the mixer: a ring and the bus it is mixed on.
#[derive(Debug)]
struct Source {
    ring: Ring,
    /// A [`Bus::index`]. Written once by [`Mixer::claim`], read by the callback.
    bus: AtomicU8,
}

/// The whole of what the output callback touches.
///
/// Held behind an `Arc` by both sides: the callback renders from it, Bevy systems set
/// gains and push samples into it. Every field is an atomic or a slice of atomics, which
/// is what makes that sharing lock-free rather than merely undocumented.
#[derive(Debug)]
pub struct Mixer {
    sources: Box<[Source]>,
    /// How many of [`Self::sources`] have been handed out. Only [`Self::claim`] writes it.
    claimed: AtomicUsize,
    /// One gain per [`Bus`], as `f32` bits, indexed by [`Bus::index`].
    gains: [AtomicU32; Bus::ALL.len()],
    /// What the open stream is running at, or [`DEFAULT_SAMPLE_RATE`] before one is.
    sample_rate: AtomicU32,
    /// How many samples one frame of the open stream holds.
    channels: AtomicU32,
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
        Self {
            sources: (0..MAX_SOURCES)
                .map(|_| Source {
                    ring: Ring::new(SOURCE_CAPACITY),
                    bus: AtomicU8::new(Bus::Master.index() as u8),
                })
                .collect(),
            claimed: AtomicUsize::new(0),
            gains: Bus::ALL.map(|_| AtomicU32::new(1.0f32.to_bits())),
            sample_rate: AtomicU32::new(DEFAULT_SAMPLE_RATE),
            channels: AtomicU32::new(2),
        }
    }

    /// Takes one of the [`MAX_SOURCES`] slots, or `None` when they are all taken.
    ///
    /// The handle is the only way to write samples, and there is exactly one per slot —
    /// which is what makes each ring's producer single, as its ordering assumes.
    pub fn claim(self: &Arc<Self>, bus: Bus) -> Option<SourceHandle> {
        let index = self.claimed.fetch_add(1, Ordering::Relaxed);
        if index >= self.sources.len() {
            // Put it back, so a refused claim cannot exhaust the counter.
            self.claimed.store(self.sources.len(), Ordering::Relaxed);
            return None;
        }
        self.sources[index]
            .bus
            .store(bus.index() as u8, Ordering::Relaxed);
        Some(SourceHandle {
            mixer: Arc::clone(self),
            index,
        })
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
        self.sample_rate
            .store(sample_rate.max(1), Ordering::Relaxed);
        self.channels
            .store(u32::from(channels).max(1), Ordering::Relaxed);
    }

    /// The sample rate the open stream is running at.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate.load(Ordering::Relaxed)
    }

    /// Fills `sink` with everything the sources have to say.
    ///
    /// **This is the function that runs on the output thread.** It allocates nothing, takes
    /// no lock, logs nothing and mentions no Bevy type; every read below is an atomic load
    /// over memory that was allocated when the mixer was built.
    pub fn render(&self, sink: &mut impl Sink) {
        let channels = self.channels.load(Ordering::Relaxed).max(1) as usize;
        let voice = f32::from_bits(self.gains[Bus::Voice.index()].load(Ordering::Relaxed));
        let master = f32::from_bits(self.gains[Bus::Master.index()].load(Ordering::Relaxed));
        for frame in sink.block().chunks_mut(channels) {
            let mut sum = 0.0;
            for source in &self.sources {
                // An underrun is silence, never the previous sample and never whatever
                // happened to be in the buffer.
                let sample = source.ring.pop().unwrap_or(0.0);
                sum += match Bus::from_index(source.bus.load(Ordering::Relaxed)) {
                    // The master gain is the output stage below, so a source on the
                    // master bus is scaled once rather than squared.
                    Bus::Master => sample,
                    Bus::Voice => sample * voice,
                };
            }
            // Clamped, so a sum of loud sources is quiet distortion rather than whatever
            // the device does with a sample outside its range.
            let out = (sum * master).clamp(-1.0, 1.0);
            // Mono in, every channel out. Panning is #854's, and it belongs to a source
            // rather than to the bus arithmetic.
            for sample in frame {
                *sample = out;
            }
        }
    }
}

/// The producer end of one claimed source.
///
/// Held by a Bevy system. Cloning it is deliberately impossible: two producers on one ring
/// is the assumption [`Ring`] is built on, and there is no way to get a second handle to a
/// slot [`Mixer::claim`] has already given away.
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
