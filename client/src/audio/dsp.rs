//! The four pieces of signal processing a captured voice goes through, and nothing else.
//!
//! **Written by hand, and `docs/adr/0001-voice-transport.md` is why.** The dependency budget
//! in `client/AGENTS.md` is five crates; a resampler, a gate, an automatic gain control and a
//! level meter are two hundred lines of arithmetic between them, and a sixth crate to hold
//! them would cost a discussion the ADR already had. What is deliberately *not* here is echo
//! cancellation — that needs libwebrtc, the ADR declines it by name, and push-to-talk is the
//! mitigation this client ships instead.
//!
//! ```text
//!   cpal input callback            this module                        #852 part 3
//!   ───────────────────            ──────────                         ───────────
//!   interleaved f32 at the  ──▶  Resampler ─▶ NoiseGate ─▶ Agc  ──▶  20 ms Opus frames
//!   device's own rate            (48 kHz mono)   │            │
//!                                                ▼            ▼
//!                                          level_db ──▶ Hold ──▶ transmit?
//! ```
//!
//! **Nothing here runs in an audio callback.** The capture callback's whole job is to copy
//! samples into a ring; everything below is called from the Bevy schedule, where allocating
//! is allowed. It is still written to allocate only when a buffer has to grow — a `Vec` that
//! is reused across calls rather than one per 20 ms — because a producer that allocates sixty
//! times a second is a producer that will eventually be moved onto a worker and be wrong
//! there. `audio/mixer.rs` states the rule that does bind, and this module is on the other
//! side of it.
//!
//! **Nothing here decides anything but its own output.** A level is not a fact about the
//! world, and `client/AGENTS.md`'s rule for `player/ambience.rs` covers this module word for
//! word: no gameplay branch reads a decibel.

// Every item below is consumed by the capture pipeline in #852 part 3, which is the part
// this module exists for. In a binary crate `pub` saves nothing from `dead_code`, and the
// alternative to this attribute is a seam that puts a resampler and the cpal input stream
// that feeds it in one pull request — which is the size this issue is split to avoid. Part 3
// deletes this line; house style is `net/codec.rs`'s outbound encoders, which carry the same
// allowance for the same reason.
#![allow(dead_code)]

use std::time::Duration;

/// What voice is captured, encoded, relayed and played at, in hertz.
///
/// Opus's own preferred rate and the one every other rate here is derived from. A device
/// running at something else is resampled to this before anything looks at it, so exactly one
/// number describes a voice sample anywhere in this client.
pub const VOICE_SAMPLE_RATE: u32 = 48_000;

/// How long one encoded frame is.
///
/// 20 ms is Opus's default and the value the codec is most efficient at: shorter frames spend
/// a larger fraction of each packet on the header, longer ones add latency a conversation can
/// hear.
pub const FRAME: Duration = Duration::from_millis(20);

/// How many samples that is at [`VOICE_SAMPLE_RATE`].
///
/// Derived rather than written, so the pair cannot drift: 48 000 × 20 / 1000.
pub const FRAME_SAMPLES: usize =
    (VOICE_SAMPLE_RATE as usize) * (FRAME.as_millis() as usize) / 1_000;

/// What [`level_db`] answers for a block with nothing in it.
///
/// A floor and not negative infinity, deliberately. `-inf` propagates through every later
/// comparison and formats as `-inf dB` on a HUD; a number well below anything a microphone
/// produces compares the way a reader expects and prints as a number.
pub const SILENCE_DB: f32 = -120.0;

/// Where the noise gate closes, in dBFS.
///
/// A fixed floor and **not** the player's activation threshold: this one is about the room —
/// a fan, a hard drive, the microphone's own hiss — and it is applied to the audio whichever
/// mode voice is in, push to talk included. -55 dBFS is below any speech at a usable
/// recording level and above the noise floor of the microphones this is likely to meet.
pub const NOISE_GATE_DB: f32 = -55.0;

/// How long a gate stays open after the level falls below its threshold.
///
/// The acceptance criterion's number, and it belongs to both users of [`Hold`]: a gate with
/// no tail chops the end of every word, and a transmission with no tail is a sentence cut off
/// between its last two syllables.
pub const HOLD: Duration = Duration::from_millis(300);

/// [`HOLD`] in samples at [`VOICE_SAMPLE_RATE`].
pub const HOLD_SAMPLES: usize = (VOICE_SAMPLE_RATE as usize) * (HOLD.as_millis() as usize) / 1_000;

/// How long the noise gate takes to open or close, in samples.
///
/// **A gate that switched between 0 and 1 between two samples would be a step, and a step is
/// a click** — the same reason the speaker test in `audio/mod.rs` fades its ends. 5 ms is
/// short enough to be inaudible as a fade and long enough to be inaudible as an edge.
const GATE_RAMP_SAMPLES: usize = (VOICE_SAMPLE_RATE as usize) * 5 / 1_000;

/// The root-mean-square level of `samples`, in dBFS, floored at [`SILENCE_DB`].
///
/// RMS and not peak: what the activation threshold is compared against is how loud a block
/// *is*, and one stray sample is not a voice. Full scale is 0 dB, so a full-scale sine reads
/// about -3 dB and everything real reads lower.
///
/// **Non-finite input cannot produce a non-finite answer.** A `NaN` sample would make the sum
/// `NaN`, `NaN.max(SILENCE_DB)` is `NaN` on this platform's ordering, and the comparison
/// against a threshold would then be false forever — a microphone that never opens, with
/// nothing saying why. The guard is the rule `client/AGENTS.md` states about non-finite
/// floats, applied where such a float could first arrive: a device's own buffer.
pub fn level_db(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return SILENCE_DB;
    }
    let sum: f32 = samples
        .iter()
        .map(|sample| {
            if sample.is_finite() {
                sample * sample
            } else {
                0.0
            }
        })
        .sum();
    let mean = sum / samples.len() as f32;
    // Every non-finite sample was replaced by zero above, so this cannot be `NaN` — but it
    // can still be an infinity, from a device buffer full of very large finite numbers, and
    // the finiteness check below is what catches that.
    if mean <= 0.0 {
        return SILENCE_DB;
    }
    // `log10` of the mean square, halved, is the log of the root — one `sqrt` fewer and the
    // same number.
    let db = 10.0 * mean.log10();
    if db.is_finite() {
        db.max(SILENCE_DB)
    } else {
        SILENCE_DB
    }
}

/// Turns whatever a capture device produces into mono at [`VOICE_SAMPLE_RATE`].
///
/// Linear interpolation, and that is a deliberate choice rather than the first thing that
/// came to hand. A polyphase or sinc resampler is audibly better on music and on a
/// down-conversion by a large ratio; this converts speech between rates that are usually
/// within a factor of two of each other, into a codec that will throw away everything above
/// 8 kHz anyway. What it must not do is *drift*: the phase is carried across calls, so a
/// stream resampled a block at a time produces exactly the sample count it would have
/// produced in one pass.
///
/// **The unconsumed tail is kept rather than the phase alone.** A device faster than 48 kHz
/// steps more than one input sample per output sample, so a single carried sample is not
/// enough to interpolate the next one from — and the shape that is wrong only above 48 kHz is
/// the shape that passes every test written on a 44.1 kHz machine.
#[derive(Debug)]
pub struct Resampler {
    /// How many input samples one output sample advances by.
    step: f64,
    /// How many interleaved channels one input frame carries.
    channels: u16,
    /// Input samples not yet consumed, mono. Always keeps at least the one the next output
    /// interpolates from.
    pending: Vec<f32>,
    /// Where the next output sits between `pending[0]` and `pending[1]`, in `[0, 1)`.
    phase: f64,
    /// Scratch for the downmix, reused so a block a frame is not an allocation a frame.
    mono: Vec<f32>,
}

impl Resampler {
    /// A resampler for a device running at `rate` hertz with `channels` interleaved channels.
    ///
    /// A rate or a channel count of zero is taken as one. Neither is a state a working device
    /// reaches; both are states a *reported* configuration can be in, and dividing by either
    /// is not this module's idea of how to find out.
    pub fn new(rate: u32, channels: u16) -> Self {
        let rate = f64::from(rate.max(1));
        Self {
            step: rate / f64::from(VOICE_SAMPLE_RATE),
            channels: channels.max(1),
            pending: Vec::new(),
            phase: 0.0,
            mono: Vec::with_capacity(FRAME_SAMPLES.max(usize::from(channels.max(1)))),
        }
    }

    /// How many channels the downmix averages. Kept beside [`Self::step`] so one constructor
    /// argument cannot be applied and the other forgotten.
    pub const fn channels(&self) -> u16 {
        self.channels
    }

    /// Appends the 48 kHz mono form of one interleaved input block to `out`.
    ///
    /// `out` is the caller's buffer and is never cleared here: a frame builder wants to
    /// accumulate until it has [`FRAME_SAMPLES`], and clearing would make that the caller's
    /// second buffer for no reason.
    pub fn resample(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.downmix(input);
        self.pending.append(&mut self.mono);

        // One output per step while there is a sample on each side of the read position.
        // `pending.len() - 1` and not `pending.len()`, because the interpolation reads the
        // sample *after* the one it sits on.
        while self.pending.len() >= 2 {
            let index = self.phase.floor();
            let at = index as usize;
            if at + 1 >= self.pending.len() {
                break;
            }
            let fraction = (self.phase - index) as f32;
            let left = self.pending[at];
            let right = self.pending[at + 1];
            out.push(left + (right - left) * fraction);
            self.phase += self.step;
        }

        // Everything before the read position is spent. What is left is the tail the next
        // block continues from, and the phase becomes its offset into that tail.
        let consumed = (self.phase.floor() as usize).min(self.pending.len());
        self.pending.drain(..consumed);
        self.phase -= consumed as f64;
    }

    /// Averages one interleaved block into [`Self::mono`].
    fn downmix(&mut self, input: &[f32]) {
        self.mono.clear();
        let channels = usize::from(self.channels.max(1));
        for frame in input.chunks(channels) {
            let sum: f32 = frame
                .iter()
                .map(|sample| if sample.is_finite() { *sample } else { 0.0 })
                .sum();
            self.mono.push(sum / frame.len() as f32);
        }
    }
}

/// A threshold with a tail: open the moment the level reaches it, closed [`HOLD`] after the
/// level last fell below it.
///
/// **One mechanism with two users**, which is why it is a type rather than two copies of a
/// countdown. The noise gate holds it at [`NOISE_GATE_DB`] and applies its answer to the
/// audio; voice activation holds it at whatever the player set and applies its answer to
/// whether anything is sent at all.
#[derive(Debug)]
pub struct Hold {
    threshold_db: f32,
    /// Samples still to run before this closes. Zero is closed.
    left: usize,
}

impl Hold {
    /// A hold that opens at `threshold_db` dBFS, starting closed.
    pub const fn new(threshold_db: f32) -> Self {
        Self {
            threshold_db,
            left: 0,
        }
    }

    /// Moves the threshold, keeping whatever tail is already running.
    ///
    /// A player dragging the knob while speaking should not have the sentence cut: the new
    /// threshold decides the *next* block, and the tail from the last one above the old
    /// threshold still runs out.
    pub const fn set_threshold(&mut self, threshold_db: f32) {
        self.threshold_db = threshold_db;
    }

    /// The threshold this opens at, in dBFS.
    pub const fn threshold_db(&self) -> f32 {
        self.threshold_db
    }

    /// Whether this is open, having seen `samples` samples at `level_db`.
    ///
    /// **`>=` and not `>`.** A threshold a level can reach exactly and not pass is a knob
    /// whose bottom position does nothing, and [`SILENCE_DB`] is the one level that must
    /// stay shut at every threshold this client offers — which it does, because the quietest
    /// the knob reaches is -60 dB.
    pub fn open(&mut self, level_db: f32, samples: usize) -> bool {
        if level_db >= self.threshold_db {
            self.left = HOLD_SAMPLES;
        } else {
            self.left = self.left.saturating_sub(samples);
        }
        self.left > 0
    }

    /// Shuts it now, tail and all. What a mode change or a released key means.
    pub const fn close(&mut self) {
        self.left = 0;
    }

    /// How much tail is still to run, in samples.
    pub const fn tail(&self) -> usize {
        self.left
    }
}

/// The room's noise floor, taken out of the signal.
///
/// A [`Hold`] plus a ramped gain: the hold decides, the ramp is what keeps the decision from
/// being audible as an edge. Below the threshold this produces silence rather than quiet
/// noise, which is what makes an open microphone in an empty room cost the encoder its
/// cheapest possible frame instead of a constant hiss.
#[derive(Debug)]
pub struct NoiseGate {
    hold: Hold,
    /// Where the ramp currently is, `0.0` shut to `1.0` open.
    gain: f32,
}

impl NoiseGate {
    /// A gate at [`NOISE_GATE_DB`], starting shut.
    pub const fn new() -> Self {
        Self {
            hold: Hold::new(NOISE_GATE_DB),
            gain: 0.0,
        }
    }

    /// Gates `samples` in place and answers whether the gate is open.
    pub fn process(&mut self, samples: &mut [f32]) -> bool {
        let open = self.hold.open(level_db(samples), samples.len());
        let target = if open { 1.0 } else { 0.0 };
        // A whole ramp per block would make the ramp's length the block's length. It is a
        // fixed slope instead, so a 5 ms fade is 5 ms whatever the device's buffer size is.
        let slope = 1.0 / GATE_RAMP_SAMPLES.max(1) as f32;
        for sample in samples.iter_mut() {
            self.gain += (target - self.gain).clamp(-slope, slope);
            *sample *= self.gain;
        }
        open
    }

    /// Shuts the gate and its tail, without a ramp. What ending a transmission means.
    pub const fn reset(&mut self) {
        self.hold.close();
        self.gain = 0.0;
    }

    /// Whether the gate is currently letting anything through.
    pub const fn is_open(&self) -> bool {
        self.hold.tail() > 0
    }
}

impl Default for NoiseGate {
    fn default() -> Self {
        Self::new()
    }
}

/// Where the automatic gain control aims, as an RMS level in dBFS.
///
/// Loud enough to use most of the encoder's range, quiet enough that a shout on top of it
/// still has somewhere to go before it clips.
const AGC_TARGET_DB: f32 = -20.0;

/// The quietest block the gain will adapt to.
///
/// Below this there is nothing to normalise, and adapting anyway is how an automatic gain
/// control turns a silent room into a loud one over the course of a minute.
const AGC_FLOOR_DB: f32 = -55.0;

/// The least and the most the gain may be, as a linear factor — about ±18 dB.
const AGC_MIN_GAIN: f32 = 0.125;
const AGC_MAX_GAIN: f32 = 8.0;

/// How much of the way to the wanted gain one block moves.
///
/// **Slow on purpose.** At one block every [`FRAME`] this is a time constant near a second:
/// fast enough that a quiet speaker becomes audible within a sentence, slow enough that the
/// gain does not pump between the syllables of one word.
const AGC_ADAPT: f32 = 0.02;

/// A slow automatic gain control: one gain for the whole stream, moved a little each block.
///
/// **One gain and not a compressor.** What this fixes is a microphone the player has set too
/// quietly or too loudly, which is one number that is wrong for the whole session. Riding the
/// dynamics inside a sentence is a different tool with a different failure mode, and a voice
/// chat that breathes is worse than one that is slightly quiet.
#[derive(Debug)]
pub struct Agc {
    gain: f32,
}

impl Agc {
    /// A gain control at unity, having adapted to nothing.
    pub const fn new() -> Self {
        Self { gain: 1.0 }
    }

    /// The factor currently applied.
    pub const fn gain(&self) -> f32 {
        self.gain
    }

    /// Applies the gain to `samples` in place, adapting it towards [`AGC_TARGET_DB`].
    ///
    /// The gain is interpolated across the block from what it was to what it becomes, so a
    /// step in the gain is never a step in the signal. The result is clamped into full scale:
    /// an encoder handed a sample outside `[-1, 1]` is an encoder handed something no
    /// microphone produced.
    pub fn apply(&mut self, samples: &mut [f32]) {
        if samples.is_empty() {
            return;
        }
        let was = self.gain;
        let level = level_db(samples);
        if level > AGC_FLOOR_DB {
            // The gain that would put *this* block on target, from the level measured before
            // any gain was applied.
            let wanted = db_to_linear(AGC_TARGET_DB - level).clamp(AGC_MIN_GAIN, AGC_MAX_GAIN);
            self.gain += (wanted - self.gain) * AGC_ADAPT;
            self.gain = self.gain.clamp(AGC_MIN_GAIN, AGC_MAX_GAIN);
        }
        let span = samples.len() as f32;
        for (index, sample) in samples.iter_mut().enumerate() {
            let gain = was + (self.gain - was) * (index as f32 / span);
            *sample = (*sample * gain).clamp(-1.0, 1.0);
        }
    }

    /// Back to unity. What a capture stream that was closed and reopened starts from.
    pub const fn reset(&mut self) {
        self.gain = 1.0;
    }
}

impl Default for Agc {
    fn default() -> Self {
        Self::new()
    }
}

/// `db` decibels as a linear factor.
fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `seconds` of a sine at `hz` and `amplitude`, at [`VOICE_SAMPLE_RATE`].
    fn sine(hz: f32, amplitude: f32, samples: usize) -> Vec<f32> {
        (0..samples)
            .map(|index| {
                let phase = std::f32::consts::TAU * hz * index as f32 / VOICE_SAMPLE_RATE as f32;
                amplitude * phase.sin()
            })
            .collect()
    }

    #[test]
    fn a_twenty_millisecond_frame_is_nine_hundred_and_sixty_samples() {
        assert_eq!(FRAME_SAMPLES, 960);
        assert_eq!(HOLD_SAMPLES, 14_400);
        assert_eq!(VOICE_SAMPLE_RATE, 48_000);
    }

    /// The meter's whole contract: full scale is 0 dB, a sine is 3 dB under its own
    /// amplitude, halving the amplitude costs 6 dB, and nothing is ever `-inf` or `NaN`.
    #[test]
    fn the_level_meter_reads_decibels_relative_to_full_scale() {
        assert!(
            (level_db(&[1.0, -1.0, 1.0, -1.0])).abs() < 1e-4,
            "full scale is 0 dB"
        );
        assert!(
            (level_db(&[0.5, -0.5]) + 6.0206).abs() < 1e-3,
            "half is -6 dB"
        );

        let full = level_db(&sine(440.0, 1.0, FRAME_SAMPLES));
        assert!(
            (full + 3.0103).abs() < 0.05,
            "a full-scale sine reads {full} rather than about -3 dB"
        );
        let quieter = level_db(&sine(440.0, 0.25, FRAME_SAMPLES));
        assert!(
            (full - quieter - 12.0412).abs() < 0.05,
            "a quarter of the amplitude is {} dB down rather than 12",
            full - quieter
        );

        assert_eq!(level_db(&[]), SILENCE_DB);
        assert_eq!(level_db(&[0.0; 64]), SILENCE_DB);
    }

    /// **A meter that answers `NaN` is a threshold nothing can ever pass.** `NaN` compares
    /// false against every bound, so a single bad sample from a device would leave voice
    /// activation permanently shut with nothing saying why.
    #[test]
    fn a_block_with_rubbish_in_it_still_reads_as_a_number() {
        for rubbish in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let block = [0.5, rubbish, 0.5, -0.5];
            let read = level_db(&block);
            assert!(read.is_finite(), "{rubbish} read back as {read}");
            assert!(read >= SILENCE_DB);
        }
        assert_eq!(level_db(&[f32::NAN; 32]), SILENCE_DB);
    }

    /// The resampler's arithmetic, at the three ratios a real device produces.
    ///
    /// The count is what matters and it is asserted with a tolerance of one sample, because
    /// where the very first output falls between two inputs is a phase choice rather than a
    /// rate error.
    #[test]
    fn resampling_produces_the_sample_count_the_rate_ratio_asks_for() {
        for (rate, channels) in [(48_000, 1), (44_100, 1), (16_000, 1), (96_000, 2)] {
            let mut resampler = Resampler::new(rate, channels);
            let mut out = Vec::new();
            let blocks = 50;
            let frames = 1_024;
            for _ in 0..blocks {
                let block = vec![0.25; frames * usize::from(channels)];
                resampler.resample(&block, &mut out);
            }
            let expected =
                (blocks * frames) as f64 * f64::from(VOICE_SAMPLE_RATE) / f64::from(rate);
            let produced = out.len() as f64;
            // The tolerance is the interpolation lag, which is one output sample per input
            // sample still held back — `48000 / rate` of them, rounded up — plus one for
            // where the first output happens to fall. Slower devices lag by more outputs
            // because each of their samples is worth more of them; that is a fixed offset
            // and not a drift, which is what the block-splitting test above pins.
            let lag = (f64::from(VOICE_SAMPLE_RATE) / f64::from(rate)).ceil() + 1.0;
            assert!(
                (produced - expected).abs() <= lag,
                "{rate} Hz produced {produced} samples where {expected} were due"
            );
        }
    }

    /// **Direct current in is direct current out**, at every rate. Linear interpolation
    /// between two equal samples is that sample, so a constant that came back changed would
    /// be an indexing error rather than a filtering choice — which is the class of bug a
    /// resampler hides best.
    #[test]
    fn a_constant_survives_every_rate_unchanged() {
        for rate in [8_000, 16_000, 44_100, 48_000, 96_000] {
            let mut resampler = Resampler::new(rate, 1);
            let mut out = Vec::new();
            for _ in 0..10 {
                resampler.resample(&[0.5; 512], &mut out);
            }
            assert!(!out.is_empty(), "{rate} Hz produced nothing at all");
            assert!(
                out.iter().all(|sample| (sample - 0.5).abs() < 1e-6),
                "{rate} Hz changed a constant"
            );
        }
    }

    /// The rate this client hopes for is a copy, not an approximation of one.
    #[test]
    fn a_device_already_at_forty_eight_kilohertz_is_passed_through() {
        let mut resampler = Resampler::new(VOICE_SAMPLE_RATE, 1);
        let input = sine(440.0, 0.5, 4_800);
        let mut out = Vec::new();
        resampler.resample(&input, &mut out);
        assert_eq!(
            out.len(),
            input.len() - 1,
            "one sample of interpolation lag"
        );
        for (produced, source) in out.iter().zip(input.iter()) {
            assert!((produced - source).abs() < 1e-6);
        }
    }

    /// Two channels become one by averaging, which is what makes a stereo microphone with
    /// one live side audible rather than half as loud as it should be relative to a mono one.
    #[test]
    fn interleaved_channels_are_averaged_into_one() {
        let mut resampler = Resampler::new(VOICE_SAMPLE_RATE, 2);
        assert_eq!(resampler.channels(), 2);
        let mut out = Vec::new();
        // Left at 1.0, right at 0.0, so the average is 0.5 throughout.
        let input: Vec<f32> = (0..2_048)
            .map(|index| if index % 2 == 0 { 1.0 } else { 0.0 })
            .collect();
        resampler.resample(&input, &mut out);
        assert!(!out.is_empty());
        assert!(out.iter().all(|sample| (sample - 0.5).abs() < 1e-6));
    }

    /// **Block boundaries are not audible and do not drift.** The same signal split into
    /// awkward blocks resamples to the same samples as the whole thing at once — the property
    /// the carried tail exists for, and the one a phase-only resampler loses above 48 kHz.
    #[test]
    fn a_stream_split_into_blocks_resamples_to_the_same_samples() {
        for rate in [44_100, 96_000] {
            let input = sine(300.0, 0.4, 9_000);
            let mut whole = Vec::new();
            Resampler::new(rate, 1).resample(&input, &mut whole);

            let mut split = Vec::new();
            let mut resampler = Resampler::new(rate, 1);
            let mut at = 0;
            for size in [1, 7, 64, 333, 1_024, 2_048].iter().cycle() {
                if at >= input.len() {
                    break;
                }
                let end = (at + size).min(input.len());
                resampler.resample(&input[at..end], &mut split);
                at = end;
            }
            assert_eq!(whole.len(), split.len(), "{rate} Hz drifted");
            for (one, other) in whole.iter().zip(split.iter()) {
                assert!(
                    (one - other).abs() < 1e-6,
                    "{rate} Hz differs at a block edge"
                );
            }
        }
    }

    /// The acceptance criterion, in one test: open above the threshold, and shut exactly
    /// 300 ms after the level last fell below it.
    #[test]
    fn a_hold_opens_above_its_threshold_and_runs_for_three_hundred_milliseconds() {
        let mut hold = Hold::new(-40.0);
        assert!(!hold.open(-50.0, FRAME_SAMPLES), "a quiet room opened it");
        assert!(hold.open(-30.0, FRAME_SAMPLES), "a voice did not open it");
        assert_eq!(hold.tail(), HOLD_SAMPLES);

        // 300 ms is exactly fifteen frames of 20 ms, and the fifteenth is the one that
        // spends the last of the tail: fourteen frames of silence still transmit, and the
        // frame that takes the countdown to zero is the one that stops.
        for frame in 1..(HOLD_SAMPLES / FRAME_SAMPLES) {
            assert!(
                hold.open(-90.0, FRAME_SAMPLES),
                "the tail ended after frame {frame} of silence"
            );
        }
        assert!(!hold.open(-90.0, FRAME_SAMPLES), "the tail never ended");

        // Exactly at the threshold is open: the knob's own bottom position has to do
        // something.
        assert!(hold.open(-40.0, FRAME_SAMPLES));
        hold.close();
        assert!(!hold.open(-90.0, 1), "close left a tail running");
    }

    /// Moving the threshold decides the next block and does not cut the current sentence.
    #[test]
    fn moving_the_threshold_keeps_the_tail_that_is_already_running() {
        let mut hold = Hold::new(-40.0);
        assert!(hold.open(-20.0, FRAME_SAMPLES));
        hold.set_threshold(-10.0);
        assert_eq!(hold.threshold_db(), -10.0);
        assert!(
            hold.open(-20.0, FRAME_SAMPLES),
            "raising the threshold cut a transmission mid-word"
        );
        assert!(hold.tail() < HOLD_SAMPLES, "the tail was refreshed anyway");
    }

    /// The gate turns a quiet room into silence, and it does so without an edge.
    #[test]
    fn the_noise_gate_silences_a_quiet_room_and_opens_without_a_click() {
        let mut gate = NoiseGate::new();
        let mut room = sine(50.0, 0.0005, FRAME_SAMPLES);
        assert!(!gate.process(&mut room), "room noise opened the gate");
        assert!(
            room.iter().all(|sample| sample.abs() < 1e-9),
            "a shut gate passed something through"
        );

        let mut speech = sine(200.0, 0.3, FRAME_SAMPLES);
        assert!(gate.process(&mut speech), "speech did not open the gate");
        assert!(gate.is_open());
        // The opening ramp: the first sample is near nothing and the block ends at full
        // amplitude. A gate that switched would have the first sample already at 0.3.
        assert!(speech[0].abs() < 0.01, "the gate opened as a step");
        let ramped = &speech[GATE_RAMP_SAMPLES..];
        assert!(
            ramped.iter().any(|sample| sample.abs() > 0.25),
            "the gate never finished opening"
        );
        // And no sample jumps by more than the ramp allows, which is what "no click" means.
        for pair in speech.windows(2) {
            assert!(
                (pair[1] - pair[0]).abs() < 0.05,
                "the gated signal has a step in it"
            );
        }
    }

    /// The gate holds through a pause between words rather than chopping it.
    #[test]
    fn the_noise_gate_holds_through_a_short_pause() {
        let mut gate = NoiseGate::new();
        gate.process(&mut sine(200.0, 0.3, FRAME_SAMPLES));
        // Five frames is 100 ms — a gap between words, well inside the 300 ms tail.
        for _ in 0..5 {
            let mut pause = vec![0.0; FRAME_SAMPLES];
            assert!(gate.process(&mut pause), "a 100 ms pause shut the gate");
        }
        for _ in 0..12 {
            let mut silence = vec![0.0; FRAME_SAMPLES];
            gate.process(&mut silence);
        }
        assert!(!gate.is_open(), "the gate never shut on real silence");

        gate.process(&mut sine(200.0, 0.3, FRAME_SAMPLES));
        gate.reset();
        assert!(!gate.is_open(), "reset left the gate open");
    }

    /// The gain control converges on its target from both directions, and gets there slowly.
    #[test]
    fn the_gain_control_converges_on_its_target_from_either_side() {
        for amplitude in [0.02, 0.9] {
            let mut agc = Agc::new();
            let mut last = Vec::new();
            for _ in 0..400 {
                let mut block = sine(220.0, amplitude, FRAME_SAMPLES);
                agc.apply(&mut block);
                last = block;
            }
            let reached = level_db(&last);
            assert!(
                (reached - AGC_TARGET_DB).abs() < 1.0,
                "an amplitude of {amplitude} settled at {reached} dB rather than {AGC_TARGET_DB}"
            );
        }
    }

    /// **Slow, and that is the property under test rather than an aside.** One block must not
    /// take the gain anywhere near where it is going, or the control is a compressor and the
    /// voice breathes between syllables.
    #[test]
    fn one_block_moves_the_gain_by_a_fraction_of_the_way() {
        let mut agc = Agc::new();
        let mut block = sine(220.0, 0.02, FRAME_SAMPLES);
        agc.apply(&mut block);
        let after_one = agc.gain();
        assert!(
            (1.0..1.2).contains(&after_one),
            "one block moved the gain to {after_one}"
        );

        // Half a second in it is still climbing rather than arrived.
        for _ in 0..24 {
            agc.apply(&mut sine(220.0, 0.02, FRAME_SAMPLES));
        }
        assert!(agc.gain() > after_one, "the gain stopped moving");
        assert!(
            agc.gain() < AGC_MAX_GAIN,
            "half a second reached the ceiling"
        );
    }

    /// Silence must not drive the gain to its ceiling, which is the classic way an automatic
    /// gain control turns an empty room into a loud one.
    #[test]
    fn silence_does_not_wind_the_gain_up() {
        let mut agc = Agc::new();
        for _ in 0..500 {
            agc.apply(&mut vec![0.0; FRAME_SAMPLES]);
        }
        assert_eq!(agc.gain(), 1.0, "silence moved the gain");

        for _ in 0..500 {
            agc.apply(&mut sine(220.0, 0.0005, FRAME_SAMPLES));
        }
        assert_eq!(agc.gain(), 1.0, "noise under the floor moved the gain");
    }

    /// The gain is bounded and the output is inside full scale, whatever it is handed.
    #[test]
    fn the_gain_is_bounded_and_nothing_leaves_full_scale() {
        let mut agc = Agc::new();
        for _ in 0..2_000 {
            agc.apply(&mut sine(220.0, 0.0015, FRAME_SAMPLES));
        }
        assert!(agc.gain() <= AGC_MAX_GAIN);

        let mut loud = vec![0.9; FRAME_SAMPLES];
        agc.apply(&mut loud);
        assert!(
            loud.iter().all(|sample| (-1.0..=1.0).contains(sample)),
            "a sample left full scale"
        );

        agc.reset();
        assert_eq!(agc.gain(), 1.0);
    }

    /// The whole chain in the order the capture pipeline runs it, on a signal that is quiet
    /// speech in a noisy room: the room is gated away, the speech survives, and the level the
    /// activation threshold is compared against is the one measured after both.
    #[test]
    fn the_chain_gates_the_room_and_leaves_the_voice() {
        let mut resampler = Resampler::new(44_100, 1);
        let mut gate = NoiseGate::new();
        let mut agc = Agc::new();
        let mut activation = Hold::new(-40.0);

        let mut room = Vec::new();
        resampler.resample(&sine(60.0, 0.0008, 44_100 / 10), &mut room);
        gate.process(&mut room);
        agc.apply(&mut room);
        assert!(
            !activation.open(level_db(&room), room.len()),
            "a quiet room was transmitted"
        );

        let mut speech = Vec::new();
        resampler.resample(&sine(220.0, 0.15, 44_100 / 10), &mut speech);
        gate.process(&mut speech);
        agc.apply(&mut speech);
        assert!(
            activation.open(level_db(&speech), speech.len()),
            "a voice was not transmitted"
        );
    }
}
