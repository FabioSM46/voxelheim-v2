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
//! ## Generated world sounds
//!
//! `synth` owns descriptions, deterministic baked buffers and non-looping continuous
//! generators. `synth::Playback` feeds them through the same rings and allocation policy as
//! voice, outside the output callback. `arrival` supplies the first real SFX: a short chime
//! placed at the announced spawn point when a welcome establishes a session. The remaining
//! sound catalogues arrive as descriptions in the following issues.
//!
//! Five buses share one source pool. The controls below still own their volumes, music
//! enable, ducking and mono fold; every synthesized sound passes through those same controls.

mod arrival;
mod city;
mod codec;
mod device;
mod dsp;
mod heard;
mod listener;
mod mixer;
pub(crate) mod spatial;
pub mod synth;
mod voice;

use std::f32::consts::TAU;
use std::sync::Arc;
use std::time::Instant;

use bevy::prelude::*;

use crate::settings::{AudioDevices, DeviceChoice, Settings};
use device::{AudioCapture, AudioDevice};
// `SPEAKING_FOR` is the window `ui/voice.rs` reads through `Speaking::recent`, named here so
// that module's test can pin it rather than restating the number.
#[allow(unused_imports)]
pub use heard::{SPEAKING_FOR, Speaking};
// `HEARD_FOR` and `MAX_VOICE` are the Voices panel's window and its ceiling, named here so
// `ui/settings.rs`'s tests can pin them rather than restating the numbers — the reason
// `SPEAKING_FOR` above carries the same allowance.
pub use device::CaptureFault;
#[allow(unused_imports)]
pub use listener::{HEARD_FOR, MAX_VOICE, Voices};
pub use mixer::{Bus, Mixer, SOURCE_CAPACITY, SourceHandle};
#[cfg(test)]
pub(crate) use mixer::{MAX_SOURCES, Sink, VOICE_RESERVE};
pub use voice::{MicTest, MicrophoneTrouble, Transmitting, VoiceControls};

/// The pitch of the speaker test, in hertz. Concert A: unmistakably a tone rather than a
/// noise, and low enough that a small laptop speaker reproduces it.
pub const TEST_TONE_HZ: f32 = 440.0;

/// The pitch the test tone plays at for `bus`, in hertz.
///
/// **A different note per bus, so that two presses can be told apart.** A player setting four
/// volumes against each other hears them one after another and has to know which one just
/// moved; an identical beep on every row makes that a memory test. The five sit inside two
/// octaves either side of [`TEST_TONE_HZ`], ordered the way the buses themselves are — lowest
/// for the bed underneath everything, highest for the thing that has just happened to
/// somebody — and every one of them is well inside the band a small laptop speaker
/// reproduces.
///
/// No wildcard arm: a sixth bus has to say what it sounds like before this compiles.
const fn test_tone_hz(bus: Bus) -> f32 {
    match bus {
        // A3, lowest, because a bed is.
        Bus::Ambience => 220.0,
        // D4, a fifth under the reference: music sits under the world.
        Bus::Music => 293.66,
        // A4, the reference, and the note the speaker test has always played. It is
        // the device test as much as a level, so it keeps the pitch a player already knows.
        Bus::Master => TEST_TONE_HZ,
        // C5, up where speech carries.
        Bus::Voice => 523.25,
        // A5, an octave over the reference, where a one-shot lives.
        Bus::Sfx => 880.0,
    }
}

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

/// How many frames a tone test waits for the mixer slot it has just given back.
///
/// **Two, and it is a bound rather than a preference.** Releasing a slot and claiming it
/// again cannot happen on one frame — the output callback is what clears a released slot, so
/// one frame of patience is what makes this control work at all. Past that, a refusal is the
/// allocation policy talking rather than the callback being late: the pool is full of voice,
/// or the bus is off. Waiting through that would fire a tone minutes afterwards, at whatever
/// moment a conversation happened to end, so the press is dropped instead — which is the
/// answer [`mixer::MAX_SOURCES`]'s policy gives every other sound that cannot claim.
const TONE_TEST_PATIENCE: u8 = 2;

/// Owns the mixer and keeps it in step with [`AudioControls`].
pub struct AudioPlugin;

impl Plugin for AudioPlugin {
    fn build(&self, app: &mut App) {
        let mixer = Arc::new(Mixer::new());
        let controls = AudioControls::default();
        apply_to(&mixer, &controls);
        let tone_test = ToneTest::new(
            Some(
                mixer
                    .claim(Bus::Master)
                    .expect("a mixer starts with every source free"),
            ),
            Bus::Master,
        );

        // Last of the three, and after the gain is set: the supervisor opens a device on
        // its own thread the moment this returns, and a stream that starts at the wrong
        // volume is a stream that is briefly audible at the wrong volume.
        let device = AudioDevice::start(Arc::clone(&mixer));
        // **Started, and holding no microphone.** The capture supervisor opens a device only
        // once something calls `Capture::listen(true)`, which nothing does until #852 part 6
        // — so a client built today runs this thread and never touches an input device, which
        // is exactly what a player who has not asked for voice expects.
        let capture = AudioCapture::start();

        arrival::register(app);
        city::register(app);
        app.insert_resource(AudioMixer(mixer))
            .insert_resource(device)
            .insert_resource(capture)
            .insert_resource(controls)
            .insert_resource(tone_test)
            .init_resource::<LastListing>()
            .add_plugins((voice::VoicePlugin, heard::HeardPlugin))
            .add_systems(
                Update,
                (
                    follow_the_settings,
                    offer_the_devices,
                    apply_the_controls,
                    // Not in the `is_changed` group above it: what this reads is who is
                    // speaking, which moves without any setting moving.
                    duck_under_speech,
                    play_the_tone_test,
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
    /// Descriptions outside the audio module compile for the device's current clock.
    pub(crate) fn sample_rate(&self) -> u32 {
        self.0.sample_rate()
    }

    #[cfg(test)]
    pub(crate) fn from_shared_for_test(mixer: Arc<Mixer>) -> Self {
        Self(mixer)
    }

    #[cfg(test)]
    pub(crate) fn shared_for_test(&self) -> Arc<Mixer> {
        Arc::clone(&self.0)
    }

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
    /// The music bus gain, `0.0` silent to `1.0` unity. Clamped by the mixer.
    ///
    /// Applied to the sources on [`Bus::Music`], then ducked, then by the master — the bus
    /// arithmetic `audio/mixer.rs` states.
    pub music_gain: f32,
    /// The effects bus gain, `0.0` silent to `1.0` unity. Clamped by the mixer.
    pub sfx_gain: f32,
    /// The ambience bus gain, `0.0` silent to `1.0` unity. Clamped by the mixer.
    pub ambience_gain: f32,
    /// Whether the music bus may hold a source at all.
    ///
    /// **Not the same statement as a music gain of zero**, and the difference is the whole
    /// reason it exists: off means nothing is generated, nothing is pushed and no mixer slot
    /// is spent, where zero is a generator still running into a ring that is multiplied by
    /// nothing. [`Mixer::set_enabled`] is what enforces it.
    pub music_on: bool,
    /// What the ducked buses are multiplied by while somebody is speaking nearby, `1.0` for
    /// no ducking at all.
    ///
    /// **A gain and not a depth**, so that the identity is a value a reader can see rather
    /// than a subtraction they have to perform. `Settings::duck_gain` is the one conversion
    /// from the percentage a player reads.
    pub duck_gain: f32,
    /// Whether the stereo image is folded to one.
    pub mono: bool,
    /// Set to play the test tone once, on this bus. **Taken back by this module**, so a
    /// caller sets it and never has to clear it.
    ///
    /// A `Bus` rather than the bare flag it was until #982: the row that asks is now one of
    /// several, and which bus is being proved is the only thing that differs between them.
    pub tone_test: Option<Bus>,
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
            music_gain: Settings::default().music_gain(),
            sfx_gain: Settings::default().sfx_gain(),
            ambience_gain: Settings::default().ambience_gain(),
            music_on: Settings::default().music_on(),
            duck_gain: Settings::default().duck_gain(),
            mono: Settings::default().mono_audio(),
            output_device: None,
            input_device: None,
            tone_test: None,
        }
    }
}

/// The tone test's own source, which bus it is currently on, and how much of the tone is
/// still to be played.
///
/// **One [`SourceHandle`] for every bus, moved rather than multiplied.** Holding one per bus
/// would spend five of [`mixer::MAX_SOURCES`] on a control a player touches twice a year, and
/// claiming a fresh one per press would exhaust the pool — a slot is not handed back until
/// its handle drops. So the handle is dropped and re-claimed when, and only when, a press
/// names a different bus from the one it is already on.
///
/// **A re-claim can fail for one block and that is not an error**, which is why [`Self::bus`]
/// stays pending rather than being taken: a released slot is [`mixer::SOURCE_CAPACITY`]'s
/// worth of somebody else's memory until the output callback has cleared it, so the claim
/// lands on the next frame instead. On a client with no working output device it never lands,
/// and nothing is lost: there is no tone to hear there either way.
#[derive(Resource, Debug)]
struct ToneTest {
    /// The slot, while this test holds one.
    source: Option<SourceHandle>,
    /// The bus [`Self::source`] was claimed on, once it has been.
    bus: Option<Bus>,
    /// The bus a press asked for and this test has not reached yet.
    wanted: Option<Bus>,
    /// How many frames [`Self::wanted`] has gone unanswered. See [`TONE_TEST_PATIENCE`].
    waited: u8,
    /// Samples still to be generated. Zero means nothing is playing.
    remaining: usize,
    /// The tone's length in samples, so the fade at each end can be placed.
    total: usize,
    /// The pitch being played, in hertz — [`test_tone_hz`] of whichever bus is under test.
    hz: f32,
    /// Where the oscillator is, in radians.
    phase: f32,
    /// Reused between frames so a system running at 60 Hz allocates once, not per frame.
    /// (The real-time rule is the callback's; this runs on the main schedule. It is still
    /// not a reason to allocate sixty times a second.)
    scratch: Vec<f32>,
}

impl ToneTest {
    fn new(source: Option<SourceHandle>, bus: Bus) -> Self {
        Self {
            bus: source.as_ref().map(|_| bus),
            source,
            wanted: None,
            waited: 0,
            remaining: 0,
            total: 0,
            hz: test_tone_hz(bus),
            phase: 0.0,
            scratch: Vec::with_capacity(SOURCE_CAPACITY),
        }
    }

    /// Records that a tone has been asked for on `bus`.
    ///
    /// Nothing is claimed here: [`Self::settle`] does that on the frame it can, so a press
    /// that arrives while the slot is still being cleared is honoured a frame later rather
    /// than dropped.
    fn ask(&mut self, bus: Bus) {
        self.wanted = Some(bus);
        self.waited = 0;
    }

    /// Moves the source onto whichever bus was asked for, and starts the tone once it has.
    ///
    /// Answers whether there is anything to feed.
    fn settle(&mut self, mixer: &AudioMixer) -> bool {
        // **A slot can be taken away, so a handle held across frames has to be re-checked
        // before it is reused.** Switching the music bus off is how a player does it: that
        // revokes every source on the bus, this one included, and a revoked handle pushes
        // into nothing. Letting go here is what returns the slot to the pool — a revoked slot
        // stays its owner's until the owner drops it — and it is what stops a half-played
        // tone being fed forever, since `feed` can never drain `remaining` through a handle
        // that accepts no samples.
        //
        // Found by review on #998. The fast path below reused `self.source` on the strength
        // of `self.bus` alone, so music off, music on, and a second press left the button
        // silently doing nothing and the slot spent for the life of the client.
        if self.source.as_ref().is_some_and(|source| !source.live()) {
            self.source = None;
            self.bus = None;
            self.remaining = 0;
        }
        let Some(bus) = self.wanted else {
            return self.remaining > 0;
        };
        if self.bus != Some(bus) {
            // Dropped first, because the slot is what is being asked for. A handle that has
            // been stolen from is already inert, so this is the same statement either way.
            self.source = None;
            self.bus = None;
            let Some(source) = mixer.claim(bus) else {
                // Not an error on the first frame: the slot this just released is on its way
                // back through the callback and cannot be claimed until it arrives. Past
                // `TONE_TEST_PATIENCE` frames it is the allocation policy refusing, and the
                // press is dropped rather than fired at some unrelated later moment.
                self.waited = self.waited.saturating_add(1);
                if self.waited >= TONE_TEST_PATIENCE {
                    self.wanted = None;
                    self.waited = 0;
                }
                return self.remaining > 0;
            };
            self.source = Some(source);
            self.bus = Some(bus);
        }
        self.wanted = None;
        self.waited = 0;
        self.start(test_tone_hz(bus));
        true
    }

    /// Starts the tone from the beginning at `hz`, whatever was playing.
    fn start(&mut self, hz: f32) {
        let rate = self
            .source
            .as_ref()
            .map_or(mixer::DEFAULT_SAMPLE_RATE, |source| {
                source.mixer().sample_rate()
            })
            .max(1) as f32;
        self.total = (rate * TEST_TONE_SECONDS) as usize;
        self.remaining = self.total;
        self.hz = hz;
        self.phase = 0.0;
    }

    /// Pushes as much of the remaining tone as the ring will take.
    ///
    /// Spread across frames rather than pushed whole, because a second of audio is four
    /// times what a source ring holds — and because that is exactly the shape voice will
    /// have: a producer feeding a bounded ring a frame at a time.
    fn feed(&mut self) {
        let Some(source) = self.source.as_ref() else {
            return;
        };
        if self.remaining == 0 {
            return;
        }
        let rate = source.mixer().sample_rate().max(1) as f32;
        let step = TAU * self.hz / rate;
        let wanted = self.remaining.min(source.free());
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
        self.remaining -= source.push(&self.scratch);
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
    let music = settings.music_gain();
    let sfx = settings.sfx_gain();
    let ambience = settings.ambience_gain();
    let music_on = settings.music_on();
    let duck = settings.duck_gain();
    let mono = settings.mono_audio();
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
    if controls.music_gain != music {
        controls.music_gain = music;
    }
    if controls.sfx_gain != sfx {
        controls.sfx_gain = sfx;
    }
    if controls.ambience_gain != ambience {
        controls.ambience_gain = ambience;
    }
    if controls.music_on != music_on {
        controls.music_on = music_on;
    }
    if controls.duck_gain != duck {
        controls.duck_gain = duck;
    }
    if controls.mono != mono {
        controls.mono = mono;
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

/// Puts every gain, the music switch, the fold and the chosen device where [`AudioControls`]
/// says.
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
    apply_to(&audio.0, &controls);
    device.use_output(controls.output_device.clone());
    capture.use_input(controls.input_device.clone());
}

/// Everything [`AudioControls`] says about the mixer, applied to one.
///
/// **Shared with [`AudioPlugin::build`] rather than written twice**, for the reason the
/// plugin already gives about the master: the supervisor opens a device the moment the
/// plugin returns, and a bus that started at a gain the settings file disagrees with is a
/// stream briefly audible at the wrong volume. Two copies of that list is how that
/// disagreement arrives.
///
/// The duck is deliberately not here: it is not a setting, it is who is speaking, and
/// [`duck_under_speech`] owns it every frame.
fn apply_to(mixer: &Mixer, controls: &AudioControls) {
    mixer.set_gain(Bus::Master, controls.master_gain);
    mixer.set_gain(Bus::Voice, controls.voice_gain);
    mixer.set_gain(Bus::Music, controls.music_gain);
    mixer.set_gain(Bus::Sfx, controls.sfx_gain);
    mixer.set_gain(Bus::Ambience, controls.ambience_gain);
    mixer.set_enabled(Bus::Music, controls.music_on);
    mixer.set_mono(controls.mono);
}

/// Takes the ducked buses down while somebody is being heard nearby, and lets them back up
/// when nobody is.
///
/// **The trigger is [`Speaking`] and there is no level detector here.** That resource is what
/// `audio/heard.rs` publishes when it *plays* a frame of somebody's voice, so this ducks for
/// audio the listener is actually hearing rather than for audio somebody sent — and it
/// inherits [`SPEAKING_FOR`]'s one-second tail, which is what stops the bed pumping between
/// two words of one sentence. A second detector reading levels off the voice bus would answer
/// a slightly different question slightly later and would be a second thing to keep true.
///
/// This writes a *target*. The ramp lives in the render path, where it advances with the
/// audio rather than with the frame rate — see `audio/mixer.rs`.
fn duck_under_speech(
    controls: Res<AudioControls>,
    speaking: Res<Speaking>,
    audio: Res<AudioMixer>,
) {
    let target = if speaking.anyone(Instant::now()) {
        controls.duck_gain
    } else {
        1.0
    };
    audio.0.set_duck(target);
}

/// Starts the tone test when one is asked for, and keeps its ring fed while it plays.
fn play_the_tone_test(
    mut controls: ResMut<AudioControls>,
    audio: Res<AudioMixer>,
    mut test: ResMut<ToneTest>,
) {
    // Read through the immutable deref, so an ordinary frame does not mark the resource
    // changed and wake `apply_the_controls` for nothing.
    if let Some(bus) = controls.tone_test {
        // Taken back here rather than by the screen that asked: a request nobody has to
        // remember to clear cannot be left set, and a set request would replay the tone every
        // frame. The *slot* the tone needs may not arrive this frame; `ToneTest` holds that
        // half, so the screen's half is finished either way.
        controls.tone_test = None;
        test.ask(bus);
    }
    if test.settle(&audio) {
        test.feed();
    }
}

pub(crate) fn reset_world(world: &mut World) {
    heard::reset_world(world);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{Choices, Knob, MonitorChoices, Tab};
    use mixer::{MAX_SOURCES, VOICE_RESERVE};

    /// A mixer and a tone test with no device anywhere near them.
    ///
    /// **No test in this module builds [`AudioPlugin`]**, and that is what keeps the
    /// suite off a sound card: building the plugin is what starts the supervisor thread
    /// that opens one. What is under test is the sample generation and the control
    /// surface, and both are reachable without either.
    fn silent_test(rate: u32) -> (Arc<Mixer>, ToneTest) {
        silent_test_on(rate, Bus::Master)
    }

    /// The same, on whichever bus a test is about.
    fn silent_test_on(rate: u32, bus: Bus) -> (Arc<Mixer>, ToneTest) {
        let mixer = Arc::new(Mixer::new());
        mixer.set_format(rate, 1);
        let source = mixer.claim(bus).expect("a free slot");
        (mixer, ToneTest::new(Some(source), bus))
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
        test.start(TEST_TONE_HZ);
        assert_eq!(test.remaining, 8_000);

        let (_, mut faster) = silent_test(48_000);
        faster.start(TEST_TONE_HZ);
        assert_eq!(faster.remaining, 48_000);
    }

    #[test]
    fn the_tone_is_fed_across_frames_and_finishes() {
        let (mixer, mut test) = silent_test(48_000);
        test.start(TEST_TONE_HZ);
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
        test.start(TEST_TONE_HZ);
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
            "an idle tone test is silence"
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
        assert!(AudioControls::default().tone_test.is_none());
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

    /// A mixer with one source on `bus` and every control applied to it, and no device
    /// anywhere near either.
    fn bus_under_test(bus: Bus, controls: &AudioControls) -> (Arc<Mixer>, SourceHandle) {
        let mixer = Arc::new(Mixer::new());
        mixer.set_format(48_000, 1);
        apply_to(&mixer, controls);
        let source = mixer.claim(bus).expect("a free slot");
        (mixer, source)
    }

    /// What a steady source on `mixer` is heard at once every ramp has arrived.
    ///
    /// 160 ms of 10 ms blocks, comfortably past the duck's 80 ms attack and its 400 ms
    /// release, so what this measures is the level rather than where a ramp had got to.
    fn settled_level(mixer: &Arc<Mixer>, source: &SourceHandle) -> f32 {
        for _ in 0..64 {
            source.push(&vec![1.0; 480]);
            let _ = rendered(mixer, 480);
        }
        source.push(&[1.0]);
        rendered(mixer, 1)[0]
    }

    /// **Every new volume reaches its own bus and no other.** The arithmetic is the mixer's
    /// and is asserted there; what this holds is the wiring between a setting a player moved
    /// and the bus it is supposed to move.
    #[test]
    fn each_new_volume_reaches_the_bus_it_names_and_leaves_the_others_alone() {
        for (knob, bus) in [
            (Knob::MusicVolume, Bus::Music),
            (Knob::SfxVolume, Bus::Sfx),
            (Knob::AmbienceVolume, Bus::Ambience),
        ] {
            let mut quiet = Settings::default();
            // Down to silence, which is a value this knob really reaches.
            quiet.adjust(knob, -100);
            let mut app = App::new();
            let mixer = Arc::new(Mixer::new());
            mixer.set_format(48_000, 1);
            let under_test = mixer.claim(bus).expect("a free slot");
            let control = mixer.claim(Bus::Master).expect("a second free slot");
            app.insert_resource(quiet)
                .insert_resource(AudioControls::default())
                .insert_resource(AudioMixer(Arc::clone(&mixer)))
                .insert_resource(AudioDevice::idle())
                .insert_resource(AudioCapture::idle())
                .add_systems(Update, (follow_the_settings, apply_the_controls).chain());
            app.update();

            under_test.push(&[1.0]);
            control.push(&[0.0]);
            let heard = rendered(&mixer, 1)[0];
            assert!(
                heard.abs() < 1e-6,
                "{knob:?} at zero left {bus:?} audible at {heard}"
            );

            // The master source is untouched, at the master's own default of 0.8 — so the
            // knob moved one bus rather than the output.
            under_test.push(&[0.0]);
            control.push(&[1.0]);
            let untouched = rendered(&mixer, 1)[0];
            assert!(
                (untouched - 0.8).abs() < 1e-6,
                "{knob:?} moved something that is not {bus:?}: {untouched}"
            );
        }
    }

    /// **The beds duck while somebody is being heard, and come back when nobody is.**
    ///
    /// Driven from [`Speaking`] — the resource `audio/heard.rs` writes when it *plays* a
    /// frame of somebody's voice — rather than from a level detector of this module's own,
    /// which is the acceptance criterion. Both directions, because a duck that never came
    /// back is the half a listener actually notices.
    #[test]
    fn the_beds_duck_while_somebody_is_heard_and_come_back_when_nobody_is() {
        let controls = AudioControls::default();
        let (mixer, music) = bus_under_test(Bus::Music, &controls);
        let mut app = App::new();
        app.insert_resource(controls)
            .insert_resource(AudioMixer(Arc::clone(&mixer)))
            .init_resource::<Speaking>()
            .add_systems(Update, duck_under_speech);

        // Nobody speaking: the music bus at its own gain under the master, and the duck is
        // the identity rather than something close to it.
        app.update();
        let quiet_room = settled_level(&mixer, &music);
        let unducked = Settings::default().music_gain() * Settings::default().master_gain();
        assert!(
            (quiet_room - unducked).abs() < 1e-5,
            "an empty room ducked the music to {quiet_room} rather than {unducked}"
        );

        // Somebody is heard. The window is `SPEAKING_FOR`, so a speaker noted now is inside
        // it for the whole of this measurement.
        app.world_mut()
            .resource_mut::<Speaking>()
            .heard(7, Instant::now());
        app.update();
        let talking = settled_level(&mixer, &music);
        let ducked = unducked * Settings::default().duck_gain();
        assert!(
            (talking - ducked).abs() < 1e-5,
            "with somebody speaking the music was heard at {talking} rather than {ducked}"
        );
        assert!(talking < quiet_room, "the duck went the wrong way");

        // And back: the speaking state is what is taken away, so nothing but who is talking
        // has changed between the two measurements.
        *app.world_mut().resource_mut::<Speaking>() = Speaking::default();
        app.update();
        let after = settled_level(&mixer, &music);
        assert!(
            (after - quiet_room).abs() < 1e-5,
            "the music came back to {after} rather than {quiet_room}"
        );
    }

    /// **A player who has turned ducking off hears nothing move.** The negative control the
    /// test above needs: an assertion that a duck happened is worth little from a client that
    /// ducks whatever the setting says.
    #[test]
    fn ducking_turned_all_the_way_down_leaves_the_music_where_it_was() {
        let mut settings = Settings::default();
        settings.adjust(Knob::VoiceDucking, -100);
        assert_eq!(settings.voice_ducking(), 0);
        let controls = AudioControls {
            duck_gain: settings.duck_gain(),
            ..AudioControls::default()
        };
        let (mixer, music) = bus_under_test(Bus::Music, &controls);

        let mut app = App::new();
        app.insert_resource(controls)
            .insert_resource(AudioMixer(Arc::clone(&mixer)))
            .init_resource::<Speaking>()
            .add_systems(Update, duck_under_speech);
        app.world_mut()
            .resource_mut::<Speaking>()
            .heard(7, Instant::now());
        app.update();

        let heard = settled_level(&mixer, &music);
        let unducked = Settings::default().music_gain() * Settings::default().master_gain();
        assert!(
            (heard - unducked).abs() < 1e-5,
            "ducking was off and the music still moved, to {heard} from {unducked}"
        );
    }

    /// **Music off means no source at all, through the assembled control surface.**
    ///
    /// The mixer's own test holds the mechanism; this holds that the switch a player presses
    /// reaches it — and that the music *volume* at zero is deliberately not the same thing,
    /// which is the whole reason the tab draws both.
    #[test]
    fn turning_music_off_leaves_a_generator_nothing_to_claim() {
        let mut app = App::new();
        let mixer = Arc::new(Mixer::new());
        mixer.set_format(48_000, 1);
        app.insert_resource(Settings::default())
            .insert_resource(AudioControls::default())
            .insert_resource(AudioMixer(Arc::clone(&mixer)))
            .insert_resource(AudioDevice::idle())
            .insert_resource(AudioCapture::idle())
            .add_systems(Update, (follow_the_settings, apply_the_controls).chain());
        app.update();
        let playing = mixer.claim(Bus::Music).expect("music starts on");

        let mut off = Settings::default();
        off.toggle_music();
        assert!(!off.music_on());
        *app.world_mut().resource_mut::<Settings>() = off;
        app.update();

        assert!(
            !playing.live(),
            "the switch left a music source holding a slot"
        );
        assert!(
            mixer.claim(Bus::Music).is_none(),
            "a generator was handed a slot with music switched off"
        );

        // The negative control, and the distinction the two controls exist for: the volume
        // at zero leaves the slot exactly where it was.
        let mut silent = Settings::default();
        silent.adjust(Knob::MusicVolume, -100);
        assert_eq!(silent.music_volume(), 0);
        assert!(silent.music_on());
        *app.world_mut().resource_mut::<Settings>() = silent;
        app.update();
        let still_there = mixer
            .claim(Bus::Music)
            .expect("a gain of zero spends no slot");
        assert!(still_there.live());
    }

    /// The mono fold crosses the seam. What it does to the image is the mixer's assertion.
    #[test]
    fn the_mono_setting_reaches_the_mixer_and_folds_both_ears_together() {
        let mut folded = Settings::default();
        folded.toggle_mono_audio();
        assert!(folded.mono_audio());

        let mixer = Arc::new(Mixer::new());
        mixer.set_format(48_000, 2);
        let source = mixer.claim(Bus::Master).expect("a free slot");
        source.place(crate::audio::spatial::Placement {
            gain: 1.0,
            pan: crate::audio::spatial::pan_gains(std::f32::consts::FRAC_PI_2),
            occlusion: 0.0,
            high_cue: 1.0,
        });

        let mut app = App::new();
        app.insert_resource(folded)
            .insert_resource(AudioControls::default())
            .insert_resource(AudioMixer(Arc::clone(&mixer)))
            .insert_resource(AudioDevice::idle())
            .insert_resource(AudioCapture::idle())
            .add_systems(Update, (follow_the_settings, apply_the_controls).chain());
        app.update();

        source.push(&[1.0; 2]);
        let out = rendered(&mixer, 4);
        assert!(
            (out[0] - out[1]).abs() < 1e-6,
            "a hard-panned source was not folded: {out:?}"
        );
        assert!(out[0] > 0.0, "the fold silenced the source");
    }

    /// **A press names a bus and the source moves onto it**, so the tone a player hears is
    /// scaled by the gain in the row they pressed rather than by whichever one it was on
    /// last.
    #[test]
    fn a_tone_test_moves_its_one_source_onto_the_bus_the_screen_named() {
        let mixer = Arc::new(Mixer::new());
        mixer.set_format(48_000, 1);
        let mut app = App::new();
        app.insert_resource(AudioControls::default())
            .insert_resource(AudioMixer(Arc::clone(&mixer)))
            .insert_resource(ToneTest::new(
                Some(mixer.claim(Bus::Master).expect("a free slot")),
                Bus::Master,
            ))
            .add_systems(Update, play_the_tone_test);

        for bus in [Bus::Sfx, Bus::Ambience, Bus::Music, Bus::Voice, Bus::Master] {
            app.world_mut().resource_mut::<AudioControls>().tone_test = Some(bus);
            app.update();
            assert_eq!(
                app.world().resource::<AudioControls>().tone_test,
                None,
                "the request was not taken back on the frame it was acted on"
            );
            let test = app.world().resource::<ToneTest>();
            assert_eq!(test.bus, Some(bus), "the tone stayed on the wrong bus");
            assert!(test.remaining > 0, "no tone was started for {bus:?}");
            assert!(
                (test.hz - test_tone_hz(bus)).abs() < f32::EPSILON,
                "{bus:?} played at {} rather than its own note",
                test.hz
            );
            // One block, so the slot this is about to give up is clear before the next press
            // asks for it — the output callback's job, and the reason `ToneTest` is patient.
            let _ = rendered(&mixer, 64);
        }

        // Exactly one slot for the whole set, which is what "moved rather than multiplied"
        // means: five presses have not spent five of sixteen.
        let free = std::iter::from_fn(|| mixer.claim(Bus::Voice)).count();
        assert_eq!(
            free,
            MAX_SOURCES - 1,
            "the tone test is holding more than one slot"
        );
    }

    /// A press the allocation policy refuses is dropped rather than fired later.
    ///
    /// **The failure this rules out is a tone arriving minutes afterwards**, at whatever
    /// moment a conversation happened to end — which is what an unbounded retry would do.
    #[test]
    fn a_tone_test_the_policy_refuses_is_given_up_on_rather_than_queued() {
        let mixer = Arc::new(Mixer::new());
        mixer.set_format(48_000, 1);
        let mut app = App::new();
        app.insert_resource(AudioControls::default())
            .insert_resource(AudioMixer(Arc::clone(&mixer)))
            .insert_resource(ToneTest::new(
                Some(mixer.claim(Bus::Master).expect("a free slot")),
                Bus::Master,
            ))
            .add_systems(Update, play_the_tone_test);
        // **The world buses hold their whole share, so no world claim can be granted however
        // many frames it waits.** Filling the pool with voice instead would not do it since
        // the review on #996: the budget bounds what the world *holds*, not how many slots
        // happen to be free, so a pool full of voice still leaves the world its own eight —
        // and this test asserted the opposite by accident until the fix propagated here.
        let beds: Vec<SourceHandle> = (0..MAX_SOURCES - VOICE_RESERVE)
            .map(|_| mixer.claim(Bus::Ambience).expect("a free slot"))
            .collect();
        let voices: Vec<SourceHandle> = std::iter::from_fn(|| mixer.claim(Bus::Voice)).collect();
        assert_eq!(
            beds.len() + voices.len(),
            MAX_SOURCES - 1,
            "the pool is full"
        );
        assert!(
            mixer.claim(Bus::Music).is_none(),
            "the world budget is spent, so this is a refusal the policy will keep making"
        );

        app.world_mut().resource_mut::<AudioControls>().tone_test = Some(Bus::Music);
        for _ in 0..TONE_TEST_PATIENCE {
            app.update();
            let _ = rendered(&mixer, 64);
        }
        let test = app.world().resource::<ToneTest>();
        assert_eq!(test.wanted, None, "the press is still queued");
        assert_eq!(test.remaining, 0, "a tone was played on a slot nobody had");
        assert!(
            voices.iter().all(|held| held.live()),
            "the tone test took a slot off somebody being heard"
        );
        drop(beds);
        drop(voices);
    }

    /// **Music TEST, music off, music on, music TEST — and the second press is heard.**
    ///
    /// The sequence the review on #998 asked for. Switching the bus off revokes every source
    /// on it, the tone test's included, and the tone test holds its handle across frames: on
    /// the strength of `self.bus` alone the second press took the fast path and fed a handle
    /// that accepts no samples, so the button did nothing and the slot was spent for the life
    /// of the client. `settle` now lets go of a handle that is no longer live before it looks
    /// at anything else.
    ///
    /// Asserted through what comes out of the mixer rather than through the resource's own
    /// fields: "a tone was started" is a claim about a counter, and this is a claim about
    /// whether a player hears the button they pressed.
    #[test]
    fn a_music_test_is_heard_again_after_music_is_switched_off_and_back_on() {
        let mixer = Arc::new(Mixer::new());
        mixer.set_format(48_000, 1);
        let mut app = App::new();
        app.insert_resource(AudioControls::default())
            .insert_resource(AudioMixer(Arc::clone(&mixer)))
            .insert_resource(ToneTest::new(
                Some(mixer.claim(Bus::Master).expect("a free slot")),
                Bus::Master,
            ))
            .add_systems(Update, play_the_tone_test);

        // What one press sounds like, so the second press has something to be compared with.
        let press_and_listen = |app: &mut App| {
            app.world_mut().resource_mut::<AudioControls>().tone_test = Some(Bus::Music);
            let mut loudest = 0.0f32;
            for _ in 0..8 {
                app.update();
                for sample in rendered(&mixer, 480) {
                    loudest = loudest.max(sample.abs());
                }
            }
            loudest
        };

        let first = press_and_listen(&mut app);
        assert!(first > 0.0, "the first press was not audible at all");

        // The player switches music off and back on. `apply_to` is what does this in an
        // assembled client; calling the mixer directly keeps the test to one moving part.
        mixer.set_enabled(Bus::Music, false);
        let _ = rendered(&mixer, 480);
        mixer.set_enabled(Bus::Music, true);
        let _ = rendered(&mixer, 480);

        let second = press_and_listen(&mut app);
        assert!(
            second > 0.0,
            "the button went silent after music was switched off and on: {second}"
        );
        assert!(
            (second - first).abs() < 1e-6,
            "the second press was heard at {second} rather than the first's {first}"
        );

        // And the slot was returned rather than spent: the pool is whole but for the one the
        // tone test is holding.
        let free = std::iter::from_fn(|| mixer.claim(Bus::Voice)).count();
        assert_eq!(
            free,
            MAX_SOURCES - 1,
            "the revoked slot was never given back: {free} of {MAX_SOURCES} left"
        );
    }

    /// The first-launch levels, pinned as the numbers a player actually hears rather than as
    /// an equality between two spellings of one expression — the reason
    /// `the_default_master_gain_matches_the_volume_the_audio_tab_starts_at` gives.
    #[test]
    fn the_new_buses_start_at_the_levels_the_audio_tab_starts_at() {
        let controls = AudioControls::default();
        for (name, gain, want) in [
            ("music", controls.music_gain, 0.6),
            ("effects", controls.sfx_gain, 1.0),
            ("ambience", controls.ambience_gain, 0.7),
            // 60 of 100 down, so the beds are multiplied by the remaining 40.
            ("the duck", controls.duck_gain, 0.4),
        ] {
            assert!(
                (gain - want).abs() < 1e-6,
                "first launch plays {name} at {gain} rather than {want}"
            );
        }
        assert!(controls.music_on, "music is off on first launch");
        assert!(!controls.mono, "the stereo image is folded on first launch");
        // The ordering is the statement the three defaults are chosen for: effects over
        // ambience over music.
        assert!(controls.sfx_gain > controls.ambience_gain);
        assert!(controls.ambience_gain > controls.music_gain);
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
        app.world_mut().resource_mut::<AudioControls>().tone_test = Some(Bus::Master);
        app.update();

        let controls = app.world().resource::<AudioControls>();
        assert_eq!(controls.master_gain, quieter.master_gain());
        assert!(
            (controls.master_gain - 0.6).abs() < f32::EPSILON,
            "four presses off 80 is 60 of 100"
        );
        assert_eq!(
            controls.tone_test,
            Some(Bus::Master),
            "the tone request was cleared"
        );
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
