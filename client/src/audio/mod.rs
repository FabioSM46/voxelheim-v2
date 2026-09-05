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

mod codec;
mod device;
mod dsp;
mod heard;
mod mixer;
mod voice;

use std::f32::consts::TAU;
use std::sync::Arc;

use bevy::prelude::*;

use crate::settings::{AudioDevices, DeviceChoice, Settings};
use device::{AudioCapture, AudioDevice};
// `SPEAKING_FOR` is the window `ui/voice.rs` reads through `Speaking::recent`, named here so
// that module's test can pin it rather than restating the number.
#[allow(unused_imports)]
pub use heard::{SPEAKING_FOR, Speaking};
pub use mixer::{Bus, Mixer, SOURCE_CAPACITY, SourceHandle};
pub use voice::{Transmitting, VoiceControls};

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
        mixer.set_gain(Bus::Voice, controls.voice_gain);
        let speaker_test = mixer
            .claim(Bus::Master)
            .map(SpeakerTest::new)
            .expect("a mixer starts with every source free");

        // Last of the three, and after the gain is set: the supervisor opens a device on
        // its own thread the moment this returns, and a stream that starts at the wrong
        // volume is a stream that is briefly audible at the wrong volume.
        let device = AudioDevice::start(Arc::clone(&mixer));
        // **Started, and holding no microphone.** The capture supervisor opens a device only
        // once something calls `Capture::listen(true)`, which nothing does until #852 part 6
        // — so a client built today runs this thread and never touches an input device, which
        // is exactly what a player who has not asked for voice expects.
        let capture = AudioCapture::start();

        app.insert_resource(AudioMixer(mixer))
            .insert_resource(device)
            .insert_resource(capture)
            .insert_resource(controls)
            .insert_resource(speaker_test)
            .init_resource::<LastListing>()
            .add_plugins((voice::VoicePlugin, heard::HeardPlugin))
            .add_systems(
                Update,
                (
                    follow_the_settings,
                    offer_the_devices,
                    apply_the_controls,
                    play_the_speaker_test,
                )
                    .chain(),
            );
    }
}

/// The mixer, as the ECS holds it.
///
/// One owner, and every later sound is a source claimed from it. The device that will render
/// it holds the other end of the same `Arc` — which is why this is an `Arc<Mixer>` rather
/// than a `Mixer`, before there is a second holder to justify it.
#[derive(Resource, Debug)]
pub struct AudioMixer(Arc<Mixer>);

impl AudioMixer {
    /// Takes one of the mixer's source slots for `bus`, or `None` when they are all taken.
    ///
    /// The one way anything outside this file reaches the mixer, so "how many sources are
    /// there" stays a question with one answer.
    fn claim(&self, bus: Bus) -> Option<SourceHandle> {
        self.0.claim(bus)
    }
}

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
    /// The voice bus gain, `0.0` silent to `1.0` unity. Clamped by the mixer.
    ///
    /// **Applied to the sources on [`Bus::Voice`] and then by the master**, which is the bus
    /// arithmetic `audio/mixer.rs` states: a listener who turns the game down turns voice
    /// down with it, and this is what they turn down *relative* to the game.
    pub voice_gain: f32,
    /// Which output device to open, under the name its host gives it, or `None` for "follow
    /// whatever the system calls its default".
    ///
    /// **A name and not a `settings::DeviceChoice`**: that enum is a *choice*, with a variant
    /// meaning "follow the system", and this is the instruction it resolves to.
    /// `audio/device.rs` matches it against the names the host answered with and nothing
    /// else.
    pub output_device: Option<String>,
    /// Which input device to open when one is opened at all, under the name its host gives
    /// it, or `None` for "follow whatever the system calls its default".
    ///
    /// The same shape as [`Self::output_device`] and resolved the same way. What differs is
    /// entirely on the other side of the seam: a named loudspeaker that is not attached
    /// leaves the client silent and retrying, and a named microphone that is not attached is
    /// substituted by the host's default with a log line. `audio/device.rs` carries that
    /// argument beside the code that makes the choice.
    pub input_device: Option<String>,
    /// Set to play the speaker test once. **Taken back by this module**, so a caller sets
    /// it and never has to clear it.
    pub speaker_test: bool,
}

impl Default for AudioControls {
    fn default() -> Self {
        Self {
            // Read from the knob, not restated beside it. The plugin sets the gain
            // before the first frame, so a default that disagreed with the settings
            // file's would be a stream briefly audible at the wrong volume — and two
            // copies of one number is how that disagreement arrives. `Settings` is
            // already this module's dependency: `follow_the_settings` below reads the
            // same accessor every frame.
            master_gain: Settings::default().master_gain(),
            voice_gain: Settings::default().voice_gain(),
            output_device: None,
            input_device: None,
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

/// Turns the Audio tab's settings into what this module acts on.
///
/// **One direction, and only ever this one.** [`Settings`] is what a player chose and what
/// the file holds; [`AudioControls`] is what the mixer is told. Nothing here writes a
/// setting back, so no failure inside this module can quietly rewrite a choice a player
/// made — the rule `settings/mod.rs` states about values that reach it from elsewhere.
///
/// `SettingsPlugin` is added before `AudioPlugin` in `main.rs`, which is what makes
/// [`Settings`] present here.
fn follow_the_settings(settings: Res<Settings>, mut controls: ResMut<AudioControls>) {
    if !settings.is_changed() {
        return;
    }
    let gain = settings.master_gain();
    let voice = settings.voice_gain();
    let device = match settings.output_device() {
        DeviceChoice::SystemDefault => None,
        DeviceChoice::Named(name) => Some(name.clone()),
    };
    let microphone = match settings.input_device() {
        DeviceChoice::SystemDefault => None,
        DeviceChoice::Named(name) => Some(name.clone()),
    };
    // Written only on a real change, so a settings change that moved nothing this module
    // reads does not mark the resource and wake `apply_the_controls` for a gain that is
    // already set. The speaker test is deliberately untouched: it is a press, not a
    // setting, and the screen that asks for one writes it straight onto this resource.
    if controls.master_gain != gain {
        controls.master_gain = gain;
    }
    if controls.voice_gain != voice {
        controls.voice_gain = voice;
    }
    if controls.output_device != device {
        controls.output_device = device;
    }
    if controls.input_device != microphone {
        controls.input_device = microphone;
    }
}

/// Each supervisor's device-list version number as this module last saw it — compared rather
/// than the list, which the supervisor would clone under a lock every frame.
///
/// **One per side**: the supervisors enumerate on their own schedules, so a shared counter
/// would make each side's enumeration republish the other's list.
#[derive(Resource, Debug, Default)]
struct LastListing {
    outputs: u64,
    inputs: u64,
}

/// Hands the settings knobs the device names the supervisors last enumerated — the other
/// direction across the same seam, and the only one: [`AudioDevices`] is a bound the machine
/// owns, so the module that talks to the machine fills it.
///
/// **Both sides in one system**, because the answer goes into one resource: written
/// separately, a frame would mark `AudioDevices` changed twice for nothing.
fn offer_the_devices(
    device: Res<AudioDevice>,
    capture: Res<AudioCapture>,
    mut last: ResMut<LastListing>,
    mut offered: ResMut<AudioDevices>,
) {
    let outputs = device.listings();
    if outputs != last.outputs {
        last.outputs = outputs;
        offered.offer_outputs(device.output_devices());
    }
    let inputs = capture.listings();
    if inputs != last.inputs {
        last.inputs = inputs;
        offered.offer_inputs(capture.input_devices());
    }
}

/// Puts the master gain and the chosen device where [`AudioControls`] says.
///
/// Only on a change, so the ordinary frame does nothing at all.
fn apply_the_controls(
    controls: Res<AudioControls>,
    audio: Res<AudioMixer>,
    device: Res<AudioDevice>,
    capture: Res<AudioCapture>,
) {
    if !controls.is_changed() {
        return;
    }
    audio.0.set_gain(Bus::Master, controls.master_gain);
    audio.0.set_gain(Bus::Voice, controls.voice_gain);
    device.use_output(controls.output_device.clone());
    capture.use_input(controls.input_device.clone());
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
    use crate::settings::{Choices, Knob, MonitorChoices, Tab};

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
        // The value, not the equality. `AudioControls::default()` now *is*
        // `Settings::default().master_gain()`, so asserting those two against each other
        // would compare a thing with itself and could never fail — a test that passes
        // whatever anybody does to either side. What can still break is the number a
        // player actually hears on first launch, so that is what is pinned: the Audio
        // tab starts at 80 of 100 and the gain is linear, so this is 0.8. Move either
        // the knob's default or the conversion and this fails.
        assert!(
            (AudioControls::default().master_gain - 0.8).abs() < f32::EPSILON,
            "first launch plays at {} rather than 0.8",
            AudioControls::default().master_gain
        );
        assert!(!AudioControls::default().speaker_test);
        // And the voice bus starts at unity, for the reason `settings/mod.rs` gives beside
        // `DEFAULT_VOICE_VOLUME`: a voice has already lost what a room, a codec and a jitter
        // buffer take from it, so there is nothing left to reserve headroom for.
        assert!(
            (AudioControls::default().voice_gain - 1.0).abs() < f32::EPSILON,
            "voice starts at {} rather than unity",
            AudioControls::default().voice_gain
        );
    }

    /// **The voice knob is a bus gain, and the bus arithmetic is what makes it worth having.**
    ///
    /// `out = master * (voice * voice_sources + master_sources)`, so turning voice down moves
    /// a source on `Bus::Voice` and leaves one on `Bus::Master` — the speaker test — exactly
    /// where it was. Rendered rather than read back off the mixer, because "the gain was
    /// stored" is a claim about a field and this is a claim about what a player hears.
    #[test]
    fn the_voice_volume_moves_the_voice_bus_and_leaves_the_rest_alone() {
        let mixer = Arc::new(Mixer::new());
        mixer.set_format(48_000, 1);
        let on_voice = mixer.claim(Bus::Voice).expect("a free slot");
        let on_master = mixer.claim(Bus::Master).expect("a free slot");

        let mut app = App::new();
        app.insert_resource(Settings::default())
            .insert_resource(AudioControls::default())
            .insert_resource(AudioMixer(Arc::clone(&mixer)))
            .insert_resource(AudioDevice::idle())
            .insert_resource(AudioCapture::idle())
            .add_systems(Update, (follow_the_settings, apply_the_controls).chain());
        app.update();

        // Master at its default 0.8, voice at unity: the voice source arrives at 0.8 and so
        // does the master one.
        on_voice.push(&[1.0]);
        on_master.push(&[0.0]);
        assert!((rendered(&mixer, 1)[0] - 0.8).abs() < 1e-6);

        let mut quieter = Settings::default();
        quieter.adjust(Knob::VoiceVolume, -10);
        assert_eq!(quieter.voice_volume(), 50);
        *app.world_mut().resource_mut::<Settings>() = quieter;
        app.update();

        on_voice.push(&[1.0]);
        on_master.push(&[0.0]);
        assert!(
            (rendered(&mixer, 1)[0] - 0.4).abs() < 1e-6,
            "half the voice volume did not halve what a speaker is heard at"
        );

        on_voice.push(&[0.0]);
        on_master.push(&[1.0]);
        assert!(
            (rendered(&mixer, 1)[0] - 0.8).abs() < 1e-6,
            "turning voice down moved a source that is not on the voice bus"
        );
    }

    /// **The seam, in one direction only.** The tab writes a setting, this module reads it,
    /// and nothing travels back — and a tone somebody asked for is not swallowed by a
    /// settings change that lands on the same frame.
    #[test]
    fn the_volume_setting_reaches_the_controls_and_nothing_is_written_back() {
        let mut app = App::new();
        app.insert_resource(Settings::default())
            .insert_resource(AudioControls::default())
            .add_systems(Update, follow_the_settings);
        app.update();

        let mut quieter = Settings::default();
        quieter.adjust(Knob::MasterVolume, -4);
        *app.world_mut().resource_mut::<Settings>() = quieter.clone();
        app.world_mut().resource_mut::<AudioControls>().speaker_test = true;
        app.update();

        let controls = app.world().resource::<AudioControls>();
        assert_eq!(controls.master_gain, quieter.master_gain());
        assert!(
            (controls.master_gain - 0.6).abs() < f32::EPSILON,
            "four presses off 80 is 60 of 100"
        );
        assert!(controls.speaker_test, "the tone request was cleared");
        assert_eq!(
            *app.world().resource::<Settings>(),
            quieter,
            "the audio module wrote a setting back"
        );
        assert_eq!(
            controls.output_device, None,
            "an untouched setting asks for no device in particular"
        );
    }

    /// The systems that carry the two device seams, with no device anywhere: both
    /// supervisors are built through `idle()`, the constructors that spawn no thread — the
    /// ones that do are what would open a stream.
    fn device_app() -> App {
        let mixer = Arc::new(Mixer::new());
        let mut app = App::new();
        app.insert_resource(Settings::default())
            .insert_resource(AudioControls::default())
            .insert_resource(AudioMixer(mixer))
            .insert_resource(AudioDevice::idle())
            .insert_resource(AudioCapture::idle())
            .insert_resource(AudioDevices::default())
            .init_resource::<LastListing>()
            .add_systems(
                Update,
                (follow_the_settings, offer_the_devices, apply_the_controls).chain(),
            );
        app
    }

    /// **The microphone knob's bound is filled by the assembled client, not by a fixture.**
    ///
    /// The knob is stepped through the `AudioDevices` **this module's own system wrote**,
    /// which is the whole of the assertion: a bound handed to `adjust_with_choices` by hand
    /// would pass with nothing between the capture supervisor and the settings screen at all,
    /// and the knob would sit on "system default" in a real client with nothing failing. The
    /// review on #929 found exactly that gap, and this is what would have failed.
    #[test]
    fn the_microphone_knob_can_step_onto_a_device_the_capture_supervisor_enumerated() {
        let mut app = device_app();
        app.update();
        assert_eq!(
            app.world().resource::<AudioDevices>().inputs(),
            AudioDevices::default().inputs(),
            "a capture supervisor that has enumerated nothing offers nothing"
        );

        app.world().resource::<AudioCapture>().enumerated(vec![
            "Built-in microphone".to_owned(),
            "USB headset mic".to_owned(),
        ]);
        app.update();

        let offered = app.world().resource::<AudioDevices>().clone();
        let monitors = MonitorChoices::default();
        app.world_mut()
            .resource_mut::<Settings>()
            .adjust_with_choices(
                Knob::InputDevice,
                1,
                Choices {
                    monitors: &monitors,
                    devices: &offered,
                },
            );
        assert_eq!(
            app.world().resource::<Settings>().input_device(),
            &crate::settings::DeviceChoice::Named("Built-in microphone".to_owned()),
            "the knob could not step off the system default in an assembled client"
        );

        // And the two sides do not overwrite one another: each supervisor's listing counter
        // is compared on its own, so the loudspeaker enumerating does not republish an empty
        // microphone list over the one above.
        app.world()
            .resource::<AudioDevice>()
            .enumerated(vec!["Built-in speakers".to_owned()]);
        app.update();
        let offered = app.world().resource::<AudioDevices>();
        assert_eq!(
            offered,
            &AudioDevices::named(
                &["Built-in speakers"],
                &["Built-in microphone", "USB headset mic"]
            ),
            "one side's enumeration cleared the other's"
        );
    }

    /// **The microphone knob reaches the capture supervisor**, which is the other half of the
    /// seam part 2 only asserted one way. The choice is stepped through the `AudioDevices`
    /// `offer_the_devices` wrote, so nothing here hands the model a list by hand.
    #[test]
    fn the_chosen_microphone_reaches_the_capture_supervisor() {
        let mut app = device_app();
        app.world().resource::<AudioCapture>().enumerated(vec![
            "Built-in microphone".to_owned(),
            "USB headset mic".to_owned(),
        ]);
        app.update();
        assert_eq!(
            app.world().resource::<AudioCapture>().wanted_input(),
            None,
            "an untouched setting asks for no microphone in particular"
        );

        let offered = app.world().resource::<AudioDevices>().clone();
        let monitors = MonitorChoices::default();
        app.world_mut()
            .resource_mut::<Settings>()
            .adjust_with_choices(
                Knob::InputDevice,
                2,
                Choices {
                    monitors: &monitors,
                    devices: &offered,
                },
            );
        app.update();
        assert_eq!(
            app.world().resource::<AudioControls>().input_device,
            Some("USB headset mic".to_owned())
        );
        assert_eq!(
            app.world().resource::<AudioCapture>().wanted_input(),
            Some("USB headset mic".to_owned()),
            "the capture supervisor was never told which microphone to open"
        );

        // Back to the system default, which is an instruction and not an absence of one: the
        // supervisor has to hear it, or it keeps opening the headset.
        app.world_mut().resource_mut::<Settings>().reset(Tab::Audio);
        app.update();
        assert_eq!(
            app.world().resource::<AudioCapture>().wanted_input(),
            None,
            "resetting the tab left the supervisor holding the old microphone"
        );
    }

    /// **The device seam, both ways, with no device anywhere.** The supervisor's list
    /// reaches the knob's bound; the knob's choice reaches the supervisor.
    #[test]
    fn the_chosen_device_reaches_the_supervisor_and_its_list_reaches_the_knob() {
        let mut app = device_app();
        app.update();
        assert_eq!(
            app.world().resource::<AudioDevices>(),
            &AudioDevices::default(),
            "a supervisor that has enumerated nothing offers nothing"
        );

        // What the supervisor does once it has looked at the machine.
        app.world().resource::<AudioDevice>().enumerated(vec![
            "Built-in speakers".to_owned(),
            "USB headset".to_owned(),
        ]);
        app.update();
        let offered = app.world().resource::<AudioDevices>().clone();
        assert_eq!(
            offered,
            AudioDevices::named(&["Built-in speakers", "USB headset"], &[]),
            "the knob's bound never heard about the devices"
        );

        // And back the other way: the knob picks one, and the supervisor is asked for it.
        let monitors = MonitorChoices::default();
        app.world_mut()
            .resource_mut::<Settings>()
            .adjust_with_choices(
                Knob::OutputDevice,
                2,
                Choices {
                    monitors: &monitors,
                    devices: &offered,
                },
            );
        app.update();
        assert_eq!(
            app.world().resource::<AudioControls>().output_device,
            Some("USB headset".to_owned())
        );
        assert_eq!(
            app.world().resource::<AudioDevice>().wanted(),
            Some("USB headset".to_owned()),
            "the supervisor was never told which device to open"
        );

        // Back to the system default, which is an instruction too and not an absence of
        // one: the supervisor has to hear it or it keeps holding the headset.
        app.world_mut().resource_mut::<Settings>().reset(Tab::Audio);
        app.update();
        assert_eq!(app.world().resource::<AudioDevice>().wanted(), None);
    }
}
