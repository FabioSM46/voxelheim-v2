//! Proximity voice, outbound: from the microphone to the socket.
//!
//! This is the part where the pieces below it become a feature. `audio/device.rs` produces
//! samples, `audio/dsp.rs` conditions them, `audio/codec.rs` packs them, `net` carries them —
//! and this file decides, sixty times a second, whether any of that should happen at all.
//!
//! ```text
//!   settings ─▶ VoiceControls ─┬─▶ listen? ─▶ audio/device.rs (the capture stream)
//!   welcome  ─▶               │
//!   Talk key ─────────────────┴─▶ transmit? ─┐
//!                                            ▼
//!   Capture::take ─▶ Resampler ─▶ NoiseGate ─▶ Agc ─▶ 20 ms ─▶ VoiceEncoder ─▶ send_voice
//! ```
//!
//! ## Two questions, and they are not the same question
//!
//! **Whether to hold a microphone open** and **whether to transmit** are decided separately,
//! and conflating them is the design mistake this module exists to avoid in both directions.
//! A client that opened the device only while transmitting would miss the beginning of every
//! sentence, because a capture stream takes tens of milliseconds to start. A client that
//! transmitted whenever the device was open would be an open microphone.
//!
//! So: the stream is open while voice is *usable* — a mode that is not `Off`, on a server that
//! relays voice at all, and in push to talk only once the player has pressed the key at least
//! once. Nothing is sent unless the transmit rule says so, frame by frame.
//!
//! **`voice_range_blocks == 0` closes both.** A server announcing zero relays no voice, so a
//! client that opened a microphone there would be recording a player for nobody, forever.
//! That is `schemas/handshake.fbs`'s "a server that relays no voice at all", taken literally.
//!
//! ## What is deliberately not decided here
//!
//! Who hears this. The frame carries no position and no recipient, the server owns both, and
//! the audience is `Everyone` — a request for the audible set the server already computed.
//! `#853` adds the knob that narrows it. Nothing in this file reads the player's own position,
//! and there is nothing here it could read it from.

use std::sync::Mutex;

use bevy::prelude::*;

use super::codec::VoiceEncoder;
use super::device::AudioCapture;
use super::dsp::{Agc, FRAME_SAMPLES, Hold, NoiseGate, Resampler, level_db};
use crate::net::{Outbound, Session, VoiceAudience, VoiceFrame, encode_voice_frame};
use crate::player::InputMode;
use crate::settings::{Control, Settings, VoiceMode};

/// How many 20 ms frames one Bevy frame may encode.
///
/// The ring holds a quarter of a second, so a client that stalled for longer has samples it
/// can never catch up on and should not try: encoding twelve frames in one tick to send them
/// all at once puts a burst into a queue of eight and drops most of it anyway. Six is two
/// ordinary frames' worth of slack at 60 Hz and a hard stop on the work one tick can do.
const MAX_FRAMES_PER_TICK: usize = 6;

/// What the voice pipeline is being asked for, as one value.
///
/// **The seam between the settings and the mechanism**, and it points one way, exactly as
/// [`super::AudioControls`] does: the settings screen and the welcome write this, and nothing
/// here writes a setting back.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct VoiceControls {
    /// What the microphone is for.
    pub mode: VoiceMode,
    /// The level voice activation starts transmitting at, in dBFS.
    pub activation_db: f32,
    /// How far a voice carries on this server, in blocks, or zero for a server that relays no
    /// voice at all — and for no session, which is the same thing from here.
    pub range_blocks: f32,
}

impl Default for VoiceControls {
    fn default() -> Self {
        let settings = Settings::default();
        Self {
            mode: settings.voice_mode(),
            activation_db: settings.voice_activation_threshold_db(),
            // No session means no server, which relays no voice. The welcome is what makes
            // this non-zero, and it is the only thing that does.
            range_blocks: 0.0,
        }
    }
}

impl VoiceControls {
    /// Whether voice is usable at all: a mode that is not off, on a server that relays it.
    pub fn live(&self) -> bool {
        self.mode != VoiceMode::Off && self.range_blocks > 0.0
    }
}

/// Whether this client is sending voice right now.
///
/// **Presentation, and the HUD in #852 part 7 is its only reader.** Nothing decides anything
/// from it — the rule `client/AGENTS.md` states for everything under `audio/`.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Transmitting(pub bool);

/// Everything the pipeline carries between ticks.
///
/// One resource rather than four, because every piece of it is invalidated by the same event:
/// a capture stream that opened is a new stream, and a resampler's carried tail, a gate's
/// hold, a gain that had adapted and a part-built frame all belong to the one that ended.
#[derive(Resource)]
struct VoicePipeline {
    /// Built for the open stream's format, and thrown away with it. `None` before any stream.
    resampler: Option<Resampler>,
    gate: NoiseGate,
    agc: Agc,
    /// The 300 ms tail voice activation transmits through.
    activation: Hold,
    /// `None` when libopus would not configure. A client that cannot encode is a client
    /// nobody can hear, with a line in the log — never one that will not run.
    ///
    /// **The `Mutex` is a type obligation and not synchronisation**, exactly as `NetLink`'s
    /// and `Outbound`'s are: libopus's encoder holds a raw pointer, so `audiopus` declares it
    /// `Send` and not `Sync`, and a Bevy resource must be `Sync`. The one accessor takes
    /// `ResMut` and reaches the contents with `get_mut`, so no lock is ever taken — which is
    /// what keeps this off the list of things `audio/mixer.rs` forbids, one thread over.
    encoder: Mutex<Option<VoiceEncoder>>,
    /// One device block, reused.
    block: Vec<f32>,
    /// 48 kHz mono samples not yet in a frame, reused.
    pending: Vec<f32>,
    /// Exactly one frame, reused.
    frame: Vec<f32>,
    /// This speaker's own monotonic counter. Wraps rather than clamps: a listener orders
    /// frames by it and hears a gap as a gap, and nothing branches on its value.
    sequence: u32,
    /// Whether push to talk has been pressed since voice became live.
    ///
    /// **What "the stream opens on the first `Talk` press and stays open" means.** A player
    /// who never presses the key never has a microphone opened; one who presses it once keeps
    /// the device, so the second press does not lose the first syllable to a stream starting.
    asked_to_speak: bool,
}

impl Default for VoicePipeline {
    fn default() -> Self {
        Self {
            resampler: None,
            gate: NoiseGate::new(),
            agc: Agc::new(),
            activation: Hold::new(Settings::default().voice_activation_threshold_db()),
            encoder: Mutex::new(
                VoiceEncoder::new()
                    .map_err(|err| warn!("{err}; nobody can hear this player"))
                    .ok(),
            ),
            block: Vec::new(),
            pending: Vec::new(),
            frame: vec![0.0; FRAME_SAMPLES],
            sequence: 0,
            asked_to_speak: false,
        }
    }
}

impl VoicePipeline {
    /// Forgets everything that belonged to a stream that has ended.
    fn start_over(&mut self) {
        self.resampler = None;
        self.pending.clear();
        self.gate.reset();
        self.agc.reset();
        self.activation.close();
    }
}

/// Adds the outbound voice pipeline. Built by [`super::AudioPlugin`], after the mixer and the
/// capture supervisor exist.
pub(super) struct VoicePlugin;

impl Plugin for VoicePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<VoiceControls>()
            .init_resource::<Transmitting>()
            .init_resource::<VoicePipeline>()
            .add_systems(Update, (follow_the_voice_settings, speak).chain());
    }
}

/// Turns what the player chose and what the server announced into what this module acts on.
///
/// **One direction.** [`Settings`] is the player's; [`Session`] is the server's; neither is
/// ever written from here. The range is read from the welcome and nowhere else — there is no
/// setting for it and there must not be, because how far a voice carries is the server's
/// answer and a client that chose its own would be deciding who hears it.
fn follow_the_voice_settings(
    settings: Option<Res<Settings>>,
    session: Option<Res<Session>>,
    mut controls: ResMut<VoiceControls>,
) {
    let settings = settings.as_deref().cloned().unwrap_or_default();
    let wanted = VoiceControls {
        mode: settings.voice_mode(),
        activation_db: settings.voice_activation_threshold_db(),
        // Absent session, absent server, no voice. Not a default worth having an opinion
        // about: it is the state before a welcome arrives and after one ends.
        range_blocks: session.map_or(0.0, |session| session.0.voice_range_blocks),
    };
    // Written only on a change, so an ordinary frame does not mark the resource.
    if *controls != wanted {
        *controls = wanted;
    }
}

/// Opens or closes the microphone, and sends what it hears.
///
/// One system because the two halves read the same three things — the mode, the key and the
/// samples — and splitting them would mean reading the key twice and deciding twice from it.
#[allow(clippy::too_many_arguments)]
fn speak(
    controls: Res<VoiceControls>,
    keys: Option<Res<ButtonInput<KeyCode>>>,
    settings: Option<Res<Settings>>,
    mode: Option<Res<InputMode>>,
    capture: Res<AudioCapture>,
    mut pipeline: ResMut<VoicePipeline>,
    mut outbound: Option<ResMut<Outbound>>,
    mut transmitting: ResMut<Transmitting>,
) {
    let pipeline = &mut *pipeline;

    // **The key is not read while chat owns the keyboard**, which is the whole of the guard
    // and the reason it is here rather than in `player`: typing the letter the control is
    // bound to must not transmit. Every other mode leaves voice live — a player reading their
    // pack or the map is still in the conversation.
    let typing = mode.is_some_and(|mode| *mode == InputMode::Chat);
    let talk = match (&keys, &settings, typing) {
        (Some(keys), Some(settings), false) => keys.pressed(settings.bindings().key(Control::Talk)),
        // No key state and no bindings is a test app, and a client with neither is not one a
        // player is holding. The default binding would be a guess about a setting.
        _ => false,
    };

    if !controls.live() {
        // Off, or a server that relays no voice. Nothing is held and nothing is remembered:
        // turning voice back on starts from a closed microphone and an unpressed key.
        pipeline.asked_to_speak = false;
        pipeline.start_over();
        capture.shared().listen(false);
        set_transmitting(&mut transmitting, false);
        return;
    }

    if talk && controls.mode == VoiceMode::PushToTalk {
        pipeline.asked_to_speak = true;
    }
    // Voice activation needs the device open to have a level to compare; push to talk waits
    // for the first press. See `VoicePipeline::asked_to_speak`.
    let open = controls.mode == VoiceMode::VoiceActivation || pipeline.asked_to_speak;
    capture.shared().listen(open);
    if !open {
        set_transmitting(&mut transmitting, false);
        return;
    }

    pipeline.activation.set_threshold(controls.activation_db);

    pipeline.block.clear();
    let Some(read) = capture.shared().take(&mut pipeline.block) else {
        // No stream, or one that opened while that was reading. Either way the samples this
        // pipeline was carrying belong to a stream that has ended.
        pipeline.start_over();
        set_transmitting(&mut transmitting, false);
        return;
    };
    if read.fresh {
        // A stream this pipeline has not read from: nothing was appended, and everything it
        // was carrying belongs to the one before. `Capture::take` carries the argument.
        pipeline.start_over();
        pipeline.resampler = Some(Resampler::new(read.sample_rate, read.channels));
        set_transmitting(&mut transmitting, false);
        return;
    }
    let Some(resampler) = pipeline.resampler.as_mut() else {
        return;
    };
    resampler.resample(&pipeline.block, &mut pipeline.pending);

    // Reached once, outside the loop: see the field's doc for why this is a `get_mut` and
    // never a lock.
    let encoder = match pipeline.encoder.get_mut() {
        Ok(encoder) => encoder,
        Err(poisoned) => poisoned.into_inner(),
    };

    let mut sending = false;
    for _ in 0..MAX_FRAMES_PER_TICK {
        if pipeline.pending.len() < FRAME_SAMPLES {
            break;
        }
        pipeline
            .frame
            .copy_from_slice(&pipeline.pending[..FRAME_SAMPLES]);
        pipeline.pending.drain(..FRAME_SAMPLES);

        // **The room, then the level, then the decision.** Both modes run the whole chain:
        // a gate and a gain control that only adapted while transmitting would spend the
        // first second of every transmission catching up.
        pipeline.gate.process(&mut pipeline.frame);
        pipeline.agc.apply(&mut pipeline.frame);
        let level = level_db(&pipeline.frame);

        let transmit = match controls.mode {
            VoiceMode::Off => false,
            VoiceMode::PushToTalk => talk,
            VoiceMode::VoiceActivation => pipeline.activation.open(level, FRAME_SAMPLES),
        };
        if !transmit {
            continue;
        }
        sending = true;

        let Some(encoder) = encoder.as_mut() else {
            continue;
        };
        let packet = match encoder.encode(&pipeline.frame) {
            Ok(packet) => packet,
            // Logged and dropped, never fatal: one frame that would not encode is a gap the
            // listener's concealment fills. The message names lengths and never samples.
            Err(err) => {
                warn!("{err}");
                continue;
            }
        };
        let wire = encode_voice_frame(&VoiceFrame {
            sequence: pipeline.sequence,
            // `Everyone` asks for the audible set the server already computed. The knob that
            // narrows it to a party is #853; the wire has carried both since #850.
            audience: VoiceAudience::Everyone,
            opus: packet,
        });
        pipeline.sequence = pipeline.sequence.wrapping_add(1);
        if let Some(outbound) = outbound.as_mut() {
            outbound.send_voice(wire);
        }
    }
    // Everything left over is more than this tick could send. It is dropped rather than
    // carried, for the voice queue's reason: a backlog of speech played out later is worse
    // than a gap, and there is nothing to be done with samples that are already late.
    if pipeline.pending.len() >= FRAME_SAMPLES {
        pipeline.pending.clear();
    }

    // Push to talk says it is transmitting while the key is held even on a tick that produced
    // no whole frame, which is what keeps the indicator from flickering at the frame rate.
    let held = controls.mode == VoiceMode::PushToTalk && talk;
    set_transmitting(&mut transmitting, sending || held);
}

/// Writes the flag only when it changes, so an ordinary frame does not mark the resource.
fn set_transmitting(transmitting: &mut ResMut<Transmitting>, sending: bool) {
    if transmitting.0 != sending {
        transmitting.0 = sending;
    }
}

/// **No test here opens a device or a socket.** The capture side is driven through
/// `AudioCapture::idle`, which spawns no supervisor, and the wire side through
/// `Outbound::to_a_test`, which is a channel a test holds the other end of. What is under test
/// is the two decisions this module makes — whether to hold a microphone, and whether to send.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{Outbound, Sent};
    use crate::settings::Knob;
    use crate::wire::voxelheim::net as fb;
    use std::sync::mpsc::Receiver;

    /// One 20 ms block of a sine at `amplitude`, at 48 kHz.
    fn speech(amplitude: f32, at: usize) -> Vec<f32> {
        (0..FRAME_SAMPLES)
            .map(|index| {
                let sample = (at * FRAME_SAMPLES + index) as f32;
                let phase = std::f32::consts::TAU * 220.0 * sample / 48_000.0;
                amplitude * phase.sin()
            })
            .collect()
    }

    /// An app with the pipeline and everything it reads, on a server that relays voice.
    fn voice_app(settings: Settings) -> (App, Receiver<Vec<u8>>) {
        app_on_a_server(settings, 24.0)
    }

    /// The same, on a server that relays no voice at all — which is also what no session
    /// looks like from here.
    fn voice_app_without_voice(settings: Settings) -> (App, Receiver<Vec<u8>>) {
        app_on_a_server(settings, 0.0)
    }

    /// **`speak` alone, with the controls set as `follow_the_voice_settings` would.** That
    /// system is tested on its own below: the range comes from a `ServerWelcome`, and
    /// `SessionParams` can only be built by the codec from bytes a server sent, so running it
    /// here would overwrite the range with the zero that means "no session" every tick.
    fn app_on_a_server(settings: Settings, range_blocks: f32) -> (App, Receiver<Vec<u8>>) {
        let mut app = App::new();
        let (outbound, sent) = Outbound::to_a_test(64);
        app.insert_resource(VoiceControls {
            mode: settings.voice_mode(),
            activation_db: settings.voice_activation_threshold_db(),
            range_blocks,
        })
        .insert_resource(settings)
        .insert_resource(ButtonInput::<KeyCode>::default())
        .insert_resource(InputMode::Playing)
        .insert_resource(AudioCapture::idle())
        .insert_resource(outbound)
        .init_resource::<Transmitting>()
        .init_resource::<VoicePipeline>()
        .add_systems(Update, speak);
        (app, sent)
    }

    /// The settings a mode needs, with the activation threshold left at its default.
    fn tuned(mode: VoiceMode) -> Settings {
        let mut settings = Settings::default();
        // Stepped rather than assigned: the knob is the only way a player reaches it, and a
        // fixture that assigned the field would be testing a state the screen cannot produce.
        while settings.voice_mode() != mode {
            let before = settings.voice_mode();
            settings.adjust(Knob::VoiceMode, if mode == VoiceMode::Off { -1 } else { 1 });
            assert_ne!(
                settings.voice_mode(),
                before,
                "the knob will not reach {mode:?}"
            );
        }
        settings
    }

    fn tick(app: &mut App) {
        app.update();
    }

    fn microphone_is_open(app: &App) -> bool {
        app.world().resource::<AudioCapture>().shared().wanted()
    }

    fn press(app: &mut App, key: KeyCode) {
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(key);
    }

    fn release(app: &mut App, key: KeyCode) {
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .release(key);
    }

    /// Feeds `blocks` of speech and runs a tick for each, answering every frame that went
    /// out.
    ///
    /// Taken every tick rather than at the end, because the voice queue is eight deep and
    /// drops its oldest: a test that let it fill would be measuring the queue's bound rather
    /// than what the pipeline produced.
    fn speak_for(app: &mut App, blocks: usize, amplitude: f32) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        for at in 0..blocks {
            app.world()
                .resource::<AudioCapture>()
                .fed(&speech(amplitude, at));
            tick(app);
            frames.extend(app.world_mut().resource_mut::<Outbound>().taken_voice());
        }
        frames
    }

    /// **A server that relays no voice never has a microphone opened on it**, whatever the
    /// player set. `schemas/handshake.fbs` calls zero "a server that relays no voice at all",
    /// and this is that taken literally: recording somebody for nobody, forever, is the one
    /// outcome this feature must not have.
    #[test]
    fn a_server_that_relays_no_voice_opens_no_microphone() {
        for mode in [
            VoiceMode::Off,
            VoiceMode::PushToTalk,
            VoiceMode::VoiceActivation,
        ] {
            let (mut app, sent) = voice_app_without_voice(tuned(mode));
            press(&mut app, KeyCode::KeyV);
            for _ in 0..5 {
                app.update();
            }
            assert!(!microphone_is_open(&app), "{mode:?} opened a microphone");
            assert!(!app.world().resource::<Transmitting>().0, "{mode:?}");
            assert!(sent.try_recv().is_err(), "{mode:?} sent something");
        }
    }

    /// Off is a standing instruction and not an absence of one: nothing is captured.
    #[test]
    fn voice_turned_off_holds_no_microphone() {
        let (mut app, sent) = voice_app(tuned(VoiceMode::Off));
        press(&mut app, KeyCode::KeyV);
        for _ in 0..5 {
            tick(&mut app);
        }
        assert!(!microphone_is_open(&app));
        assert!(sent.try_recv().is_err());
    }

    /// **The acceptance criterion for push to talk, in one test.** The stream opens on the
    /// first press and stays open — so the second press does not lose its first syllable to a
    /// device starting — and nothing is sent while the key is up.
    #[test]
    fn push_to_talk_opens_on_the_first_press_and_keeps_the_device() {
        let (mut app, _sent) = voice_app(tuned(VoiceMode::PushToTalk));
        tick(&mut app);
        assert!(
            !microphone_is_open(&app),
            "a microphone was opened before the key was ever pressed"
        );

        press(&mut app, KeyCode::KeyV);
        tick(&mut app);
        assert!(microphone_is_open(&app), "the press opened nothing");

        // The stream this pipeline now reads from.
        app.world().resource::<AudioCapture>().opened(48_000, 1);
        assert!(!speak_for(&mut app, 6, 0.3).is_empty(), "nothing was sent");
        assert!(app.world().resource::<Transmitting>().0);

        release(&mut app, KeyCode::KeyV);
        let after = speak_for(&mut app, 6, 0.3);
        assert!(
            after.is_empty(),
            "a released key sent {} frames",
            after.len()
        );
        assert!(!app.world().resource::<Transmitting>().0);
        assert!(
            microphone_is_open(&app),
            "the device was let go between two things a player said"
        );
    }

    /// Voice activation opens the device when the mode is chosen, because it needs a level to
    /// compare before anybody has decided anything.
    #[test]
    fn voice_activation_opens_the_device_as_soon_as_it_is_chosen() {
        let (mut app, _sent) = voice_app(tuned(VoiceMode::VoiceActivation));
        tick(&mut app);
        assert!(microphone_is_open(&app), "nothing opened the device");
    }

    /// **Voice activation transmits above the threshold and stops after the tail.** The tail
    /// is `dsp::HOLD` — 300 ms, fifteen frames — and what is asserted either side of it is
    /// that speech is sent and that silence stops being sent.
    #[test]
    fn voice_activation_transmits_on_speech_and_stops_after_the_tail() {
        let (mut app, _sent) = voice_app(tuned(VoiceMode::VoiceActivation));
        tick(&mut app);
        app.world().resource::<AudioCapture>().opened(48_000, 1);

        let spoken = speak_for(&mut app, 30, 0.3).len();
        assert!(spoken > 0, "a voice did not open the transmission");
        assert!(app.world().resource::<Transmitting>().0);

        // Silence. The tail runs, and then it stops — checked as "some frames, then none"
        // rather than an exact count, because the pipeline's own buffering decides how many
        // whole frames a tick produces.
        let tail = speak_for(&mut app, 20, 0.0).len();
        assert!(tail < spoken, "silence transmitted as much as speech did");
        let after = speak_for(&mut app, 10, 0.0);
        assert!(after.is_empty(), "the transmission never stopped");
        assert!(!app.world().resource::<Transmitting>().0);
    }

    /// A quiet room is under the threshold and is never transmitted, which is the whole point
    /// of the knob.
    #[test]
    fn a_quiet_room_is_not_transmitted() {
        let (mut app, _sent) = voice_app(tuned(VoiceMode::VoiceActivation));
        tick(&mut app);
        app.world().resource::<AudioCapture>().opened(48_000, 1);
        assert!(
            speak_for(&mut app, 30, 0.0008).is_empty(),
            "room noise was transmitted"
        );
    }

    /// **Typing the letter the control is bound to must not transmit.** Chat owns the
    /// keyboard, and this is the one mode that closes voice's read of the key.
    #[test]
    fn the_talk_key_is_not_read_while_chat_owns_the_keyboard() {
        let (mut app, _sent) = voice_app(tuned(VoiceMode::PushToTalk));
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Chat;
        press(&mut app, KeyCode::KeyV);
        for _ in 0..4 {
            tick(&mut app);
        }
        assert!(!microphone_is_open(&app), "typing opened a microphone");
        assert!(speak_for(&mut app, 4, 0.3).is_empty());

        // And back: the same key, out of chat, does transmit.
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        tick(&mut app);
        app.world().resource::<AudioCapture>().opened(48_000, 1);
        assert!(!speak_for(&mut app, 6, 0.3).is_empty());
    }

    /// The key is the one the player bound, not the letter this client shipped with.
    #[test]
    fn the_rebound_key_is_the_one_that_transmits() {
        let mut settings = tuned(VoiceMode::PushToTalk);
        settings
            .rebind(Control::Talk, KeyCode::KeyB)
            .expect("b is free");
        let (mut app, _sent) = voice_app(settings);
        press(&mut app, KeyCode::KeyV);
        for _ in 0..3 {
            tick(&mut app);
        }
        assert!(!microphone_is_open(&app), "the old key still transmits");

        release(&mut app, KeyCode::KeyV);
        press(&mut app, KeyCode::KeyB);
        tick(&mut app);
        app.world().resource::<AudioCapture>().opened(48_000, 1);
        assert!(!speak_for(&mut app, 6, 0.3).is_empty());
    }

    /// **What goes on the wire**: a `VoiceFrame`, audience `Everyone`, a sequence that
    /// advances by one per frame, and bytes inside the contract's ceiling.
    #[test]
    fn every_frame_is_a_voice_frame_for_everyone_with_the_next_sequence() {
        let (mut app, _sent) = voice_app(tuned(VoiceMode::PushToTalk));
        press(&mut app, KeyCode::KeyV);
        tick(&mut app);
        app.world().resource::<AudioCapture>().opened(48_000, 1);

        let mut sequences = Vec::new();
        for at in 0..10 {
            app.world().resource::<AudioCapture>().fed(&speech(0.3, at));
            tick(&mut app);
            let produced = app.world_mut().resource_mut::<Outbound>().taken_voice();
            for frame in produced {
                let envelope = fb::root_as_envelope(&frame).expect("a frame this client built");
                let voice = envelope
                    .payload_as_voice_frame()
                    .expect("the tag names the payload");
                assert_eq!(voice.audience(), fb::VoiceAudience::Everyone);
                let opus = voice.opus().expect("the vector is written");
                assert!(!opus.is_empty(), "an empty frame was sent");
                assert!(opus.len() <= crate::net::MAX_OPUS_BYTES);
                sequences.push(voice.sequence());
            }
        }
        assert!(sequences.len() >= 3, "{sequences:?}");
        for pair in sequences.windows(2) {
            assert_eq!(pair[1], pair[0] + 1, "the sequence skipped: {sequences:?}");
        }
        assert_eq!(sequences[0], 0, "the first frame of a session is not zero");
    }

    /// **Voice takes its own queue and never the one input waits on**, which is the
    /// acceptance criterion this pipeline is the producer for. Thirty frames of speech leave
    /// every one of `OUTBOUND_QUEUE`'s slots free.
    #[test]
    fn a_flood_of_speech_leaves_the_input_queue_alone() {
        let (mut app, _sent) = voice_app(tuned(VoiceMode::PushToTalk));
        press(&mut app, KeyCode::KeyV);
        tick(&mut app);
        app.world().resource::<AudioCapture>().opened(48_000, 1);
        speak_for(&mut app, 40, 0.3);

        let mut outbound = app.world_mut().resource_mut::<Outbound>();
        // The channel `Outbound::to_a_test` was built with, above.
        for slot in 0..64 {
            assert_eq!(
                outbound.send(vec![1, 2, 3]),
                Sent::Queued,
                "voice had taken input slot {slot}"
            );
        }
    }

    /// A capture stream that reopens is a gap, not a continuation: the pipeline throws away
    /// the resampler, the gate, the gain and the part-built frame rather than splicing the
    /// two streams into one sentence.
    #[test]
    fn a_reopened_stream_starts_the_pipeline_over() {
        let (mut app, _sent) = voice_app(tuned(VoiceMode::PushToTalk));
        press(&mut app, KeyCode::KeyV);
        tick(&mut app);
        app.world().resource::<AudioCapture>().opened(48_000, 1);
        assert!(!speak_for(&mut app, 6, 0.3).is_empty());

        // The device is replaced, at a different rate. The first tick after it appends
        // nothing at all — that is `Capture::take`'s `fresh` — and the pipeline is rebuilt.
        app.world().resource::<AudioCapture>().opened(16_000, 2);
        app.world().resource::<AudioCapture>().fed(&speech(0.3, 0));
        tick(&mut app);
        assert!(
            app.world_mut()
                .resource_mut::<Outbound>()
                .taken_voice()
                .is_empty(),
            "the first tick of a new stream sent something"
        );
        assert!(!app.world().resource::<Transmitting>().0);

        // And it recovers on the new stream rather than staying broken.
        assert!(
            !speak_for(&mut app, 12, 0.3).is_empty(),
            "it never recovered"
        );
    }

    /// **The seam, one way only.** The settings screen and the welcome write what this module
    /// reads, and nothing here writes a setting back.
    #[test]
    fn the_settings_reach_the_controls_and_nothing_is_written_back() {
        let mut settings = tuned(VoiceMode::VoiceActivation);
        settings.adjust(Knob::VoiceActivationThreshold, 3);
        let mut app = App::new();
        app.insert_resource(settings.clone())
            .init_resource::<VoiceControls>()
            .add_systems(Update, follow_the_voice_settings);
        app.update();

        let controls = *app.world().resource::<VoiceControls>();
        assert_eq!(controls.mode, VoiceMode::VoiceActivation);
        assert_eq!(
            controls.activation_db,
            settings.voice_activation_threshold_db()
        );
        // **No session is a server that relays no voice**, which is the same state and the
        // same answer: the range is the welcome's and there is no welcome.
        assert_eq!(controls.range_blocks, 0.0);
        assert!(!controls.live(), "voice was live with no server");
        assert_eq!(
            *app.world().resource::<Settings>(),
            settings,
            "the voice module wrote a setting back"
        );
    }
}
