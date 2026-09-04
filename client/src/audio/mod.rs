//! Sound: the mixer every later sound is a source on, and the control surface above it.
//!
//! **Audio is presentation, and the rule is `player/ambience.rs`'s word for word**: nothing
//! here is ever read by input, targeting, placement, or any other code that decides an
//! outcome. A gain is not a fact about the world and a silent client is not a disadvantaged
//! one. The server sends no sound and this module invents no rule from one — what it
//! produces is samples, and samples reach a speaker and nowhere else.
//!
//! ## The second real-time thread
//!
//! `net/mod.rs` describes the first thread boundary this client has; the output callback is
//! the second, and it is stricter. The net thread may block, allocate and log, because
//! nothing is waiting on it to the microsecond. The callback is scheduled by the operating
//! system's audio stack and is not waited for: miss its deadline and the player hears a
//! click, not a dropped frame.
//!
//! ```text
//!   ECS (this file)                        output callback
//!   ───────────────                        ───────────────
//!   SourceHandle::push ── lock-free ring ──▶ Mixer::render
//! ```
//!
//! **What may run in the callback**: atomic loads and stores, and arithmetic over memory
//! that was allocated before the stream opened. Nothing else — no allocation, no lock, no
//! `info!`/`warn!`, no Bevy type, no `Arc` clone, no `String`. Bevy systems write into rings
//! and atomics; the callback reads them. `mixer.rs` is where that is enforced rather than
//! requested, and `the_render_path_allocates_nothing` is what holds it.
//!
//! ## Who owns the device
//!
//! `audio/device.rs`, and nothing else. It puts one `cpal::Stream` on a supervisor thread
//! of its own, reopens it when the device errors or disappears, and treats a machine with
//! no output at all as a log line and a silent client rather than a reason not to run. Its
//! output callback is the only real-time caller [`Mixer::render`] has.
//!
//! `Mixer` is still testable with no device anywhere, through [`mixer::Sink`] — which is
//! how every assertion here and in `mixer.rs` runs, and how the supervisor loop itself is
//! tested without a sound card.
//!
//! ## What this module does not do yet
//!
//! Nothing encodes either. `audiopus` is a dependency of this client from #851 part 1 so the
//! lockfile and the CI package list move once rather than twice, and the codec arrives with
//! proximity voice. There is no capture device, no spatialisation and no bus beyond `Voice`
//! and `Master` — each of those is its own issue, and a bus nothing feeds is a gain nobody
//! can hear moving. See `docs/adr/0001-voice-transport.md`.

mod device;
mod mixer;

use std::f32::consts::TAU;
use std::sync::Arc;

use bevy::prelude::*;

use device::AudioDevice;
pub use mixer::{Bus, Mixer, SOURCE_CAPACITY, SourceHandle};

/// The pitch of the speaker test, in hertz. Concert A: unmistakably a tone rather than a
/// noise, and low enough that a small laptop speaker reproduces it.
pub const TEST_TONE_HZ: f32 = 440.0;

/// How long it plays, in seconds.
pub const TEST_TONE_SECONDS: f32 = 1.0;

/// How loud, before the master gain. Well under full scale, because the point is to hear
/// *whether* the device works, and a test that is louder than the game teaches a player to
/// distrust the volume they just set.
const TEST_TONE_AMPLITUDE: f32 = 0.35;

/// The fade at each end of the test tone, in samples at any sample rate above 10 kHz.
///
/// A sine that starts and stops at a non-zero sample is a step, and a step is a click —
/// which on a speaker test is indistinguishable from a fault in the thing being tested.
const TEST_TONE_FADE: usize = 240;

/// Owns the mixer and keeps it in step with [`AudioControls`].
pub struct AudioPlugin;

impl Plugin for AudioPlugin {
    fn build(&self, app: &mut App) {
        let mixer = Arc::new(Mixer::new());
        let controls = AudioControls::default();
        mixer.set_gain(Bus::Master, controls.master_gain);
        let speaker_test = mixer
            .claim(Bus::Master)
            .map(SpeakerTest::new)
            .expect("a mixer starts with every source free");

        // Last of the three, and after the gain is set: the supervisor opens a device on
        // its own thread the moment this returns, and a stream that starts at the wrong
        // volume is a stream that is briefly audible at the wrong volume.
        let device = AudioDevice::start(Arc::clone(&mixer));

        app.insert_resource(AudioMixer(mixer))
            .insert_resource(device)
            .insert_resource(controls)
            .insert_resource(speaker_test)
            .add_systems(Update, (apply_the_controls, play_the_speaker_test));
    }
}

/// The mixer, as the ECS holds it.
///
/// One owner, and every later sound is a source claimed from it. The device that will render
/// it holds the other end of the same `Arc` — which is why this is an `Arc<Mixer>` rather
/// than a `Mixer`, before there is a second holder to justify it.
#[derive(Resource, Debug)]
pub struct AudioMixer(Arc<Mixer>);

/// Everything a screen may ask of the audio module.
///
/// **The seam between the mechanism and its caller.** The settings tab writes these fields
/// and reads nothing else; this module reads them and owns everything behind them. A knob
/// that reached past this resource into the mixer would be a second owner of the device,
/// which is the thing `docs/adr/0001-voice-transport.md` declines to have.
#[derive(Resource, Debug, Clone, PartialEq)]
pub struct AudioControls {
    /// The master bus gain, `0.0` silent to `1.0` unity. Clamped by the mixer.
    pub master_gain: f32,
    /// Set to play the speaker test once. **Taken back by this module**, so a caller sets
    /// it and never has to clear it.
    pub speaker_test: bool,
}

impl Default for AudioControls {
    fn default() -> Self {
        Self {
            // 80 of 100, which is what the Audio tab's master volume starts at: loud
            // enough to be heard, with room above it for a quiet recording.
            master_gain: 0.8,
            speaker_test: false,
        }
    }
}

/// The speaker test's own source, and how much of the tone is still to be played.
///
/// A [`SourceHandle`] claimed once, at build, and kept for the life of the app: claiming
/// one per press would exhaust [`mixer::MAX_SOURCES`] after four presses, because a slot is
/// claimed for the life of the mixer.
#[derive(Resource, Debug)]
struct SpeakerTest {
    source: SourceHandle,
    /// Samples still to be generated. Zero means nothing is playing.
    remaining: usize,
    /// The tone's length in samples, so the fade at each end can be placed.
    total: usize,
    /// Where the oscillator is, in radians.
    phase: f32,
    /// Reused between frames so a system running at 60 Hz allocates once, not per frame.
    /// (The real-time rule is the callback's; this runs on the main schedule. It is still
    /// not a reason to allocate sixty times a second.)
    scratch: Vec<f32>,
}

impl SpeakerTest {
    fn new(source: SourceHandle) -> Self {
        Self {
            source,
            remaining: 0,
            total: 0,
            phase: 0.0,
            scratch: Vec::with_capacity(SOURCE_CAPACITY),
        }
    }

    /// Starts the tone from the beginning, whatever was playing.
    fn start(&mut self) {
        let rate = self.source.mixer().sample_rate().max(1) as f32;
        self.total = (rate * TEST_TONE_SECONDS) as usize;
        self.remaining = self.total;
        self.phase = 0.0;
    }

    /// Pushes as much of the remaining tone as the ring will take.
    ///
    /// Spread across frames rather than pushed whole, because a second of audio is four
    /// times what a source ring holds — and because that is exactly the shape voice will
    /// have: a producer feeding a bounded ring a frame at a time.
    fn feed(&mut self) {
        if self.remaining == 0 {
            return;
        }
        let rate = self.source.mixer().sample_rate().max(1) as f32;
        let step = TAU * TEST_TONE_HZ / rate;
        let wanted = self.remaining.min(self.source.free());
        self.scratch.clear();
        for _ in 0..wanted {
            let played = self.total - self.remaining + self.scratch.len();
            let left = self.total - played;
            let fade =
                (played.min(left).min(TEST_TONE_FADE) as f32) / (TEST_TONE_FADE.max(1) as f32);
            self.scratch
                .push(TEST_TONE_AMPLITUDE * fade * self.phase.sin());
            self.phase = (self.phase + step) % TAU;
        }
        self.remaining -= self.source.push(&self.scratch);
    }
}

/// Puts the master gain where [`AudioControls`] says.
///
/// Only on a change, so the ordinary frame does nothing at all.
fn apply_the_controls(controls: Res<AudioControls>, audio: Res<AudioMixer>) {
    if !controls.is_changed() {
        return;
    }
    audio.0.set_gain(Bus::Master, controls.master_gain);
}

/// Starts the speaker test when one is asked for, and keeps its ring fed while it plays.
fn play_the_speaker_test(mut controls: ResMut<AudioControls>, mut test: ResMut<SpeakerTest>) {
    // Read through the immutable deref, so an ordinary frame does not mark the resource
    // changed and wake `apply_the_controls` for nothing.
    if controls.speaker_test {
        // Taken back here rather than by the screen that asked: a request nobody has to
        // remember to clear cannot be left set, and a set flag would replay the tone every
        // frame.
        controls.speaker_test = false;
        test.start();
    }
    if test.remaining > 0 {
        test.feed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mixer and a speaker test with no device anywhere near them.
    ///
    /// **No test in this module builds [`AudioPlugin`]**, and that is what keeps the
    /// suite off a sound card: building the plugin is what starts the supervisor thread
    /// that opens one. What is under test is the sample generation and the control
    /// surface, and both are reachable without either.
    fn silent_test(rate: u32) -> (Arc<Mixer>, SpeakerTest) {
        let mixer = Arc::new(Mixer::new());
        mixer.set_format(rate, 1);
        let source = mixer.claim(Bus::Master).expect("a free slot");
        (mixer, SpeakerTest::new(source))
    }

    /// Renders `samples` mono samples out of `mixer`.
    fn rendered(mixer: &Arc<Mixer>, samples: usize) -> Vec<f32> {
        struct VecSink(Vec<f32>);
        impl mixer::Sink for VecSink {
            fn block(&mut self) -> &mut [f32] {
                &mut self.0
            }
        }
        let mut sink = VecSink(vec![0.0; samples]);
        mixer.render(&mut sink);
        sink.0
    }

    #[test]
    fn the_test_tone_is_one_second_of_audio_at_the_stream_rate() {
        let (_, mut test) = silent_test(8_000);
        test.start();
        assert_eq!(test.remaining, 8_000);

        let (_, mut faster) = silent_test(48_000);
        faster.start();
        assert_eq!(faster.remaining, 48_000);
    }

    #[test]
    fn the_tone_is_fed_across_frames_and_finishes() {
        let (mixer, mut test) = silent_test(48_000);
        test.start();
        // A source ring holds a quarter of a second, so one second cannot be pushed in one
        // frame however keen the producer is.
        test.feed();
        assert!(test.remaining > 0, "one frame cannot hold a whole second");

        for _ in 0..200 {
            let _ = rendered(&mixer, SOURCE_CAPACITY);
            test.feed();
        }
        assert_eq!(test.remaining, 0, "it finishes rather than looping");
    }

    #[test]
    fn the_tone_starts_and_ends_at_silence() {
        let (mixer, mut test) = silent_test(48_000);
        mixer.set_gain(Bus::Master, 1.0);
        test.start();
        test.feed();
        let block = rendered(&mixer, 64);
        assert_eq!(block[0], 0.0, "the fade starts from nothing");
        assert!(
            block
                .iter()
                .all(|sample| sample.abs() <= TEST_TONE_AMPLITUDE + 1e-6),
            "the tone never exceeds its own amplitude"
        );
        assert!(
            block[1..].iter().any(|sample| sample.abs() > 0.0),
            "and it is not silent throughout"
        );
    }

    #[test]
    fn a_tone_that_is_not_asked_for_pushes_nothing() {
        let (mixer, mut test) = silent_test(48_000);
        test.feed();
        assert!(
            rendered(&mixer, 32).iter().all(|sample| *sample == 0.0),
            "an idle speaker test is silence"
        );
    }

    #[test]
    fn the_default_master_gain_matches_the_volume_the_audio_tab_starts_at() {
        // 80 of 100. The two numbers are one decision, and this is what says so until the
        // knob exists to read it from.
        assert_eq!(AudioControls::default().master_gain, 0.8);
        assert!(!AudioControls::default().speaker_test);
    }
}
