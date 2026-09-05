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
//! Who hears this. The frame carries no position and no recipient, and the server owns both.
//! `VoiceAudience` is a *request* stamped on every frame — `Everyone` asks for the audible set
//! the server already computed, `Party` asks it to narrow that set — and the answer is the
//! server's either way. Nothing in this file reads the player's own position or the party
//! roster, and there is nothing here it could read either from: whether the player is in a
//! party at all is a question for the HUD, which has the snapshot. A client that stopped
//! transmitting because its own roster looked empty would be deciding an outcome.

use std::sync::Mutex;

use bevy::prelude::*;

use super::codec::VoiceEncoder;
use super::device::{AudioCapture, CaptureFault};
use super::dsp::{Agc, FRAME_SAMPLES, Hold, NoiseGate, Resampler, level_db};
use super::mixer::{Bus, SourceHandle};
use crate::net::VoiceAudience as WireAudience;
use crate::net::{Outbound, Session, VoiceFrame, encode_voice_frame};
use crate::player::InputMode;
use crate::settings::{Control, Settings, VoiceAudience, VoiceMode};

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
    /// Who the player is asking to be heard by. Stamped on every frame; never consulted here
    /// about whether to send one.
    pub audience: VoiceAudience,
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
            audience: settings.voice_audience(),
        }
    }
}

impl VoiceControls {
    /// Whether voice is usable at all: a mode that is not off, on a server that relays it.
    pub fn live(&self) -> bool {
        self.mode != VoiceMode::Off && self.range_blocks > 0.0
    }
}

/// The microphone test: whether the settings screen is holding the input open, and what it
/// last measured.
///
/// **A held state, not a request, and that is the difference from `AudioControls::speaker_test`
/// one module over.** A tone is a press that this module takes back on the frame it starts;
/// a microphone test is a row that is *open*, and the screen owns whether it still is. So this
/// flag is written by `ui/settings.rs` and never cleared here.
///
/// **The level is presentation like every other number under `audio/`.** Nothing decides
/// anything from it — the transmit rule reads its own meter, frame by frame, and would go on
/// working with this resource deleted.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq)]
pub struct MicTest {
    /// Whether the row is open. While it is, the capture device is held whatever the voice
    /// mode says, what it hears is played back through the `Voice` bus, and **nothing is
    /// sent**.
    pub open: bool,
    /// The gated, gain-controlled level of the last frame captured, in dBFS, or `None` when
    /// nothing has been captured since the row was opened.
    pub level_db: Option<f32>,
}

/// What is wrong with the microphone, when something is.
///
/// **This is what makes refusing a named device that is not there safe to do.** The client
/// opens nothing in its place — audio the player never consented to, relayed to people who
/// cannot tell it happened, is the harm that decides it — so what is owed to the player is
/// being *told*, in the place they are already looking when they wonder why nobody answered.
/// `ui/voice.rs` is that place.
///
/// **It carries the cause rather than a bare flag**, because the two the supervisor can tell
/// apart send a player to two different places: a device the host does not list is one to plug
/// in, and one that would not open is one to stop using elsewhere. A single flag paired with a
/// sentence naming *one* of those causes told the other half of the players something false
/// about their own hardware — found by review on #928.
///
/// **Presentation, and the HUD is its only reader**, exactly as [`Transmitting`] is. Nothing
/// decides anything from it.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MicrophoneTrouble(pub Option<CaptureFault>);

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
    /// Makes the next frame that passes the transmit decision fail to reach the encoder.
    ///
    /// **A `cfg(test)` seam, on `Transport::Plaintext`'s precedent.** At 24 kbit/s with a
    /// frame that is always [`FRAME_SAMPLES`] long, `VoiceEncoder::encode` has no reachable
    /// failure — so the branch that drops a frame cannot be exercised from outside, and the
    /// property under test is what the *sequence* does when it is taken. A guard nobody has
    /// watched fire is a guard nobody knows the shape of.
    #[cfg(test)]
    fail_next_encode: Option<()>,
    /// The mixer source the test plays back through, or `None` when the mixer had no slot
    /// free — a test that cannot be heard rather than a client that will not run.
    ///
    /// Claimed once, at build, and kept for the life of the app, for `SpeakerTest`'s reason:
    /// a slot is claimed for the mixer's life, so claiming one per press would exhaust them.
    loopback: Option<SourceHandle>,
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
            loopback: None,
            block: Vec::new(),
            pending: Vec::new(),
            frame: vec![0.0; FRAME_SAMPLES],
            sequence: 0,
            #[cfg(test)]
            fail_next_encode: None,
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
        let loopback = app
            .world()
            .get_resource::<super::AudioMixer>()
            .and_then(|mixer| mixer.claim(Bus::Voice));
        if loopback.is_none() {
            warn!("no mixer source is free for the microphone test");
        }
        app.init_resource::<VoiceControls>()
            .init_resource::<Transmitting>()
            .init_resource::<MicrophoneTrouble>()
            .init_resource::<MicTest>()
            .insert_resource(VoicePipeline {
                loopback,
                ..VoicePipeline::default()
            })
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
        audience: settings.voice_audience(),
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
    mut trouble: ResMut<MicrophoneTrouble>,
    mut test: ResMut<MicTest>,
) {
    let pipeline = &mut *pipeline;
    // **The meter reads only while the row is open.** Cleared here rather than on the paths
    // that shut the device, because those are not the same set: voice activation and a held
    // key both keep a stream open, so a test stopped under either left its last reading on a
    // meter nothing was feeding. Found by review on #943.
    // **The test is a third answer to "hold a microphone open?", and it is the player's own.**
    // A client opens no input device unasked — that is what `VoiceMode::Off` means and what a
    // server relaying no voice means — but a player pressing "Test microphone" has asked, out
    // loud, on this machine. So it holds the device whatever the mode and whatever the server
    // says, and it is the one path that opens one with `Off` chosen.
    let testing = test.open;
    if !testing {
        clear_level(&mut test);
    }

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

    if !controls.live() && !testing {
        // Off, or a server that relays no voice, and no test asking otherwise. Nothing is held
        // and nothing is remembered: turning voice back on starts from a closed microphone and
        // an unpressed key.
        pipeline.asked_to_speak = false;
        pipeline.start_over();
        capture.listen(false);
        set_transmitting(&mut transmitting, false);
        set_trouble(&mut trouble, None);
        clear_level(&mut test);
        return;
    }

    if talk && controls.live() && controls.mode == VoiceMode::PushToTalk {
        pipeline.asked_to_speak = true;
    }
    // Voice activation needs the device open to have a level to compare; push to talk waits
    // for the first press. See `VoicePipeline::asked_to_speak`. **Closing the test restores
    // exactly this**, with no state to put back: the answer is recomputed every tick from the
    // mode and a press the test never touched.
    let open = testing
        || (controls.live()
            && (controls.mode == VoiceMode::VoiceActivation || pipeline.asked_to_speak));
    capture.listen(open);
    // **Only while a device has been asked for.** A microphone nobody wanted cannot be at
    // fault, and the supervisor names a cause only once an open attempt has actually failed —
    // so this cannot flash in the moment between the request and a stream starting.
    set_trouble(
        &mut trouble,
        if open { capture.shared().fault() } else { None },
    );
    if !open {
        set_transmitting(&mut transmitting, false);
        clear_level(&mut test);
        return;
    }

    pipeline.activation.set_threshold(controls.activation_db);

    pipeline.block.clear();
    let Some(read) = capture.shared().take(&mut pipeline.block) else {
        // No stream, or one that opened while that was reading. Either way the samples this
        // pipeline was carrying belong to a stream that has ended.
        pipeline.start_over();
        set_transmitting(&mut transmitting, false);
        clear_level(&mut test);
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

        if testing {
            // **The whole of the test: heard here, and nowhere else.** The player is listening
            // to their own microphone through the same bus every other voice arrives on, so the
            // voice volume they just set is the volume they hear it at.
            //
            // There is no echo canceller in this client — `docs/adr/0001-voice-transport.md`
            // is why push to talk is the default — so a loudspeaker and an open microphone are
            // a feedback path. A test the player opened and can close is where that is
            // acceptable; transmitting from it would not be.
            if let Some(loopback) = pipeline.loopback.as_ref() {
                loopback.push(&pipeline.frame);
            }
            if test.level_db != Some(level) {
                test.level_db = Some(level);
            }
            continue;
        }

        let transmit = match controls.mode {
            VoiceMode::Off => false,
            VoiceMode::PushToTalk => talk,
            VoiceMode::VoiceActivation => pipeline.activation.open(level, FRAME_SAMPLES),
        };
        if !transmit {
            continue;
        }
        sending = true;

        // **The counter advances for every frame that passed the transmit decision, whether
        // or not one goes out.** It is the speaker's count of 20 ms *of speech*, which is what
        // `schemas/player.fbs` says it is and what a listener orders and finds gaps by — not a
        // count of successful encodes, which is a different quantity and one no receiver can
        // use. A frame that is dropped here is 20 ms the speaker said and the listener will
        // not hear, and the listener's decoder has exactly one repair for that; not moving the
        // counter would hide it instead, splicing the two sides of the hole together and
        // playing everything after it 20 ms early. Found by review on #922, where the comment
        // below promised the concealment that this line is what makes possible.
        let sequence = pipeline.sequence;
        pipeline.sequence = pipeline.sequence.wrapping_add(1);

        #[cfg(test)]
        let failing = pipeline.fail_next_encode.take().is_some();
        #[cfg(not(test))]
        let failing = false;

        let Some(encoder) = encoder.as_mut().filter(|_| !failing) else {
            continue;
        };
        let packet = match encoder.encode(&pipeline.frame) {
            Ok(packet) => packet,
            // Logged and dropped, never fatal: one frame that would not encode is a gap, and
            // the sequence above is what makes the listener hear it as one. The message names
            // lengths and never samples.
            Err(err) => {
                warn!("{err}");
                continue;
            }
        };
        let wire = encode_voice_frame(&VoiceFrame {
            sequence,
            // **Every frame carries the knob's current value**, rather than the value the
            // transmission started at. A player who narrows the audience mid-sentence has
            // narrowed it from the next 20 ms on, which is the only reading of that press a
            // listener can be given; carrying a value forward would mean a frame the player
            // no longer intends to be public going out as one.
            audience: wire_audience(controls.audience),
            opus: packet,
        });
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
    // no whole frame, which is what keeps the indicator from flickering at the frame rate. A
    // test never transmits, whatever is held.
    let held = !testing && controls.mode == VoiceMode::PushToTalk && talk;
    set_transmitting(&mut transmitting, !testing && (sending || held));
}

/// Writes the cause only when it changes, so an ordinary frame does not mark the resource.
fn set_trouble(trouble: &mut ResMut<MicrophoneTrouble>, fault: Option<CaptureFault>) {
    if trouble.0 != fault {
        trouble.0 = fault;
    }
}

/// Forgets the last level when nothing is being captured, so a meter cannot sit at whatever it
/// last read while the device is shut.
fn clear_level(test: &mut ResMut<MicTest>) {
    if test.level_db.is_some() {
        test.level_db = None;
    }
}

/// The contract's word for what the player asked for.
///
/// **The one place the two enums meet, and it points one way.** `settings::VoiceAudience` is a
/// preference and `net::VoiceAudience` is a wire tag; `settings/` may not name a contract type,
/// which is the rule that keeps a knob from becoming a message. No wildcard arm, so a third
/// audience has to say what it is on the wire before it builds.
const fn wire_audience(audience: VoiceAudience) -> WireAudience {
    match audience {
        VoiceAudience::Everyone => WireAudience::Everyone,
        VoiceAudience::Party => WireAudience::Party,
    }
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
    use super::super::mixer::Mixer;
    use super::*;
    use crate::net::{Outbound, Sent};
    use crate::settings::Knob;
    use crate::wire::voxelheim::net as fb;
    use std::sync::Arc;
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

    /// The same, with a real mixer behind the loopback source so what the test plays back can
    /// be measured rather than assumed.
    fn voice_app_with_a_mixer(settings: Settings, range_blocks: f32) -> (App, Arc<Mixer>) {
        let mixer = Arc::new(Mixer::new());
        mixer.set_format(48_000, 1);
        mixer.set_gain(Bus::Voice, 1.0);
        mixer.set_gain(Bus::Master, 1.0);
        let loopback = mixer.claim(Bus::Voice).expect("a free slot");
        let (mut app, _sent) = app_on_a_server(settings, range_blocks);
        app.world_mut().resource_mut::<VoicePipeline>().loopback = Some(loopback);
        (app, mixer)
    }

    /// How loud what the loopback has queued for the output callback is.
    fn loopback_level(mixer: &Arc<Mixer>) -> f32 {
        struct VecSink(Vec<f32>);
        impl super::super::mixer::Sink for VecSink {
            fn block(&mut self) -> &mut [f32] {
                &mut self.0
            }
        }
        let mut sink = VecSink(vec![0.0; FRAME_SAMPLES * 4]);
        mixer.render(&mut sink);
        level_db(&sink.0)
    }

    /// Opens or closes the microphone test, as the settings row does.
    fn set_mic_test(app: &mut App, open: bool) {
        app.world_mut().resource_mut::<MicTest>().open = open;
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
            audience: settings.voice_audience(),
        })
        .insert_resource(settings)
        .insert_resource(ButtonInput::<KeyCode>::default())
        .insert_resource(InputMode::Playing)
        .insert_resource(AudioCapture::idle())
        .insert_resource(outbound)
        .init_resource::<Transmitting>()
        .init_resource::<MicrophoneTrouble>()
        .init_resource::<MicTest>()
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

    /// **A microphone at fault reaches the HUD with its cause, and only once one has been
    /// asked for.**
    ///
    /// The second half is what stops the indicator flashing on every first press: `speak` asks
    /// the supervisor whether an attempt has *failed*, not whether a stream is open this
    /// instant, so the moment between `listen(true)` and a stream starting reads normally.
    ///
    /// The cause is carried rather than flattened — a busy device and a missing one are two
    /// different things to tell a player, and the flag that answered both with one sentence is
    /// what #928 found.
    #[test]
    fn a_microphone_at_fault_reaches_the_hud_with_its_cause_and_an_unasked_one_does_not() {
        for fault in [CaptureFault::NotAttached, CaptureFault::WouldNotOpen] {
            let (mut app, _sent) = voice_app(tuned(VoiceMode::PushToTalk));
            app.world().resource::<AudioCapture>().faulted(fault);
            tick(&mut app);
            assert_eq!(
                app.world().resource::<MicrophoneTrouble>().0,
                None,
                "a microphone nobody has asked for was reported at fault"
            );

            press(&mut app, KeyCode::KeyV);
            tick(&mut app);
            assert!(microphone_is_open(&app), "the key did not ask for a device");
            assert_eq!(
                app.world().resource::<MicrophoneTrouble>().0,
                Some(fault),
                "the cause the supervisor found was not the one the HUD was given"
            );
            assert!(
                !app.world().resource::<Transmitting>().0,
                "the HUD was told this player was speaking with no microphone"
            );

            // Voice off closes the device, and nothing is at fault when nothing is wanted.
            app.world_mut().resource_mut::<VoiceControls>().mode = VoiceMode::Off;
            tick(&mut app);
            assert_eq!(
                app.world().resource::<MicrophoneTrouble>().0,
                None,
                "voice off still reported a microphone at fault"
            );
        }
    }

    /// **The test holds the microphone with voice off, plays it back, and sends nothing.**
    ///
    /// Three claims, and each is the whole of a criterion. The device is held because the
    /// player asked for it out loud — the one path that opens an input with `Off` chosen, and
    /// the reason it is not "a microphone nobody asked for". What it hears reaches the `Voice`
    /// bus, so it arrives at the volume the player just set. And **nothing goes out**: the
    /// transmit rule is not consulted at all while the row is open.
    #[test]
    fn the_microphone_test_holds_the_device_plays_it_back_and_sends_nothing() {
        let (mut app, mixer) = voice_app_with_a_mixer(tuned(VoiceMode::Off), 0.0);
        tick(&mut app);
        assert!(
            !microphone_is_open(&app),
            "voice off held a microphone before anybody asked"
        );

        set_mic_test(&mut app, true);
        tick(&mut app);
        assert!(
            microphone_is_open(&app),
            "the test did not open the microphone with voice off"
        );

        app.world().resource::<AudioCapture>().opened(48_000, 1);
        let sent = speak_for(&mut app, 6, 0.3);
        assert!(sent.is_empty(), "the microphone test transmitted");
        assert!(
            !app.world().resource::<Transmitting>().0,
            "the HUD was told this player was speaking during a test"
        );

        let played = loopback_level(&mixer);
        assert!(played > -40.0, "the test played nothing back ({played} dB)");
        let level = app.world().resource::<MicTest>().level_db;
        assert!(
            level.is_some_and(|level| level > -40.0),
            "the meter read {level:?} for speech"
        );

        // **And nothing goes out on the one configuration where the transmit rule would
        // otherwise say yes.** With voice off it says no for its own reasons, so a test that
        // stopped there would pass with the whole loopback branch deleted — measured: removing
        // the `continue` failed nothing until this half existed. Push to talk with the key held
        // on a server that relays voice is the case that distinguishes them.
        let (mut talking, _mixer) = voice_app_with_a_mixer(tuned(VoiceMode::PushToTalk), 24.0);
        set_mic_test(&mut talking, true);
        press(&mut talking, KeyCode::KeyV);
        tick(&mut talking);
        talking.world().resource::<AudioCapture>().opened(48_000, 1);
        assert!(
            speak_for(&mut talking, 6, 0.3).is_empty(),
            "holding the talk key during a microphone test transmitted"
        );
        assert!(
            !talking.world().resource::<Transmitting>().0,
            "the HUD said this player was speaking while they were testing"
        );

        // And the same for voice activation, whose rule answers to the level rather than a key
        // — which the test is deliberately feeding.
        let (mut gated, _mixer) = voice_app_with_a_mixer(tuned(VoiceMode::VoiceActivation), 24.0);
        set_mic_test(&mut gated, true);
        tick(&mut gated);
        gated.world().resource::<AudioCapture>().opened(48_000, 1);
        assert!(
            speak_for(&mut gated, 12, 0.4).is_empty(),
            "voice activation transmitted the microphone test"
        );

        // **Closing the row restores the previous capture state**, and there is no state to
        // put back: with voice off, that state is a closed microphone.
        set_mic_test(&mut app, false);
        tick(&mut app);
        assert!(
            !microphone_is_open(&app),
            "the microphone outlived the test that opened it"
        );
        assert_eq!(
            app.world().resource::<MicTest>().level_db,
            None,
            "the meter kept its last reading with the device shut"
        );

        // **And with the device still open, which is the case the first clear missed.** The
        // level used to be forgotten only on the paths that shut the capture stream — but a
        // test stopped under push to talk with the key held, or under voice activation, leaves
        // a stream open for voice, so the meter kept its last reading while nothing was
        // feeding it. Found by review on #943.
        let (mut holding, _mixer) = voice_app_with_a_mixer(tuned(VoiceMode::PushToTalk), 24.0);
        press(&mut holding, KeyCode::KeyV);
        set_mic_test(&mut holding, true);
        tick(&mut holding);
        holding.world().resource::<AudioCapture>().opened(48_000, 1);
        speak_for(&mut holding, 6, 0.3);
        assert!(
            holding.world().resource::<MicTest>().level_db.is_some(),
            "the meter read nothing during the test"
        );

        set_mic_test(&mut holding, false);
        tick(&mut holding);
        assert!(
            microphone_is_open(&holding),
            "the fixture shut the device, so this is not the case under test"
        );
        assert_eq!(
            holding.world().resource::<MicTest>().level_db,
            None,
            "a stopped test left a stale level on a meter nothing is feeding"
        );
    }

    /// **And closing it restores a microphone that was already open.** Push to talk that has
    /// been pressed once holds the device; a test opened and closed over the top of that must
    /// leave it exactly where it found it, and must not have consumed the press either.
    #[test]
    fn closing_the_test_leaves_push_to_talk_holding_the_device_it_had() {
        let (mut app, _mixer) = voice_app_with_a_mixer(tuned(VoiceMode::PushToTalk), 24.0);
        press(&mut app, KeyCode::KeyV);
        tick(&mut app);
        release(&mut app, KeyCode::KeyV);
        tick(&mut app);
        assert!(
            microphone_is_open(&app),
            "push to talk let go of the device between presses"
        );

        set_mic_test(&mut app, true);
        tick(&mut app);
        set_mic_test(&mut app, false);
        tick(&mut app);
        assert!(
            microphone_is_open(&app),
            "the test closing took push to talk's device with it"
        );

        // And the key still transmits, so nothing about the press was consumed.
        app.world().resource::<AudioCapture>().opened(48_000, 1);
        press(&mut app, KeyCode::KeyV);
        tick(&mut app);
        assert!(!speak_for(&mut app, 6, 0.3).is_empty());
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

    /// **Every frame carries the knob's current value, including the ones mid-sentence.**
    ///
    /// A player who narrows the audience while speaking has narrowed it from the next 20 ms on.
    /// Stamping the value the transmission *started* at would send frames the player no longer
    /// intends to be public — and it is the shape a pipeline naturally takes if the audience is
    /// captured once, which is why the assertion is on frames either side of the press rather
    /// than on a fresh transmission.
    #[test]
    fn the_audience_on_a_frame_is_the_one_the_knob_holds_when_it_is_encoded() {
        let (mut app, _sent) = voice_app(tuned(VoiceMode::PushToTalk));
        press(&mut app, KeyCode::KeyV);
        tick(&mut app);
        app.world().resource::<AudioCapture>().opened(48_000, 1);

        let public = speak_for(&mut app, 6, 0.3);
        assert!(!public.is_empty(), "nothing went out before the press");
        for frame in &public {
            assert_eq!(audience_of(frame), fb::VoiceAudience::Everyone);
        }

        // The press, mid-transmission: the key is still held and the pipeline is still running.
        app.world_mut()
            .resource_mut::<Settings>()
            .adjust(Knob::VoiceAudience, 1);
        // `follow_the_voice_settings` is not in this fixture — see `app_on_a_server` — so the
        // controls are moved the way that system would move them, and by nothing else.
        app.world_mut().resource_mut::<VoiceControls>().audience = VoiceAudience::Party;

        let narrowed = speak_for(&mut app, 6, 0.3);
        assert!(!narrowed.is_empty(), "nothing went out after the press");
        for frame in &narrowed {
            assert_eq!(
                audience_of(frame),
                fb::VoiceAudience::Party,
                "a frame encoded after the press still asked to be public"
            );
        }
    }

    /// The audience tag on one frame this client built.
    fn audience_of(frame: &[u8]) -> fb::VoiceAudience {
        fb::root_as_envelope(frame)
            .expect("a frame this client built")
            .payload_as_voice_frame()
            .expect("the tag names the payload")
            .audience()
    }

    /// **The settings knob reaches the controls, and nothing reaches it back.**
    ///
    /// `follow_the_voice_settings` is the one writer of `VoiceControls::audience`, and the
    /// mapping to the contract's enum is total — a third audience cannot be added without
    /// saying what it is on the wire.
    #[test]
    fn the_audience_setting_reaches_the_controls_and_maps_onto_the_contract() {
        let mut app = App::new();
        app.insert_resource(Settings::default())
            .init_resource::<VoiceControls>()
            .add_systems(Update, follow_the_voice_settings);
        app.update();
        assert_eq!(
            app.world().resource::<VoiceControls>().audience,
            VoiceAudience::Everyone
        );

        app.world_mut()
            .resource_mut::<Settings>()
            .adjust(Knob::VoiceAudience, 1);
        app.update();
        assert_eq!(
            app.world().resource::<VoiceControls>().audience,
            VoiceAudience::Party
        );
        assert_eq!(
            *app.world().resource::<Settings>(),
            {
                let mut expected = Settings::default();
                expected.adjust(Knob::VoiceAudience, 1);
                expected
            },
            "the voice pipeline wrote a setting back"
        );

        assert_eq!(
            wire_audience(VoiceAudience::Everyone),
            WireAudience::Everyone
        );
        assert_eq!(wire_audience(VoiceAudience::Party), WireAudience::Party);
    }

    /// **A frame that is not sent is a gap, and the sequence is what makes the listener hear
    /// it as one.**
    ///
    /// The counter is the speaker's count of 20 ms *of speech*, not of successful encodes. A
    /// frame dropped here is 20 ms the speaker said and the listener will not hear, and the
    /// listener's decoder has exactly one repair for that. Leaving the counter still would
    /// hide the hole instead: the receiver would see a continuous run, conceal nothing, splice
    /// the two sides together, and play everything after it 20 ms early. Found by review on
    /// #922, where the code promised the concealment and the counter denied it.
    #[test]
    fn a_frame_that_does_not_encode_leaves_a_gap_in_the_sequence() {
        let (mut app, _sent) = voice_app(tuned(VoiceMode::PushToTalk));
        press(&mut app, KeyCode::KeyV);
        tick(&mut app);
        app.world().resource::<AudioCapture>().opened(48_000, 1);

        let mut sequences = Vec::new();
        for at in 0..14 {
            if at == 6 {
                app.world_mut()
                    .resource_mut::<VoicePipeline>()
                    .fail_next_encode = Some(());
            }
            app.world().resource::<AudioCapture>().fed(&speech(0.3, at));
            tick(&mut app);
            for frame in app.world_mut().resource_mut::<Outbound>().taken_voice() {
                sequences.push(
                    fb::root_as_envelope(&frame)
                        .expect("a frame this client built")
                        .payload_as_voice_frame()
                        .expect("the tag names the payload")
                        .sequence(),
                );
            }
        }

        assert!(sequences.len() >= 4, "{sequences:?}");
        let skipped: Vec<u32> = sequences
            .windows(2)
            .filter(|pair| pair[1] != pair[0] + 1)
            .map(|pair| pair[0])
            .collect();
        assert_eq!(
            skipped.len(),
            1,
            "one frame was dropped and the sequence shows {} gaps: {sequences:?}",
            skipped.len()
        );
        let at = sequences
            .iter()
            .position(|sequence| *sequence == skipped[0])
            .expect("the gap is in the run");
        assert_eq!(
            sequences[at + 1],
            skipped[0] + 2,
            "the gap is not one frame wide: {sequences:?}"
        );
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
