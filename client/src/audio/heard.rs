//! Proximity voice, inbound: from the wire to the speakers.
//!
//! **Receiving a frame *is* the audibility decision, already made.** The server owns every
//! position and every roster and it sent this frame, so nothing here asks whether the speaker
//! is close enough or in the right group; a client that filtered on its own idea of either
//! would be second-guessing the only authority there is. `schemas/player.fbs` states that
//! where `VoiceHeard` is declared, and this module is the consumer it is stated for.
//!
//! ```text
//!   net::VoiceInbox ─▶ Jitter (one per speaker) ─▶ VoiceDecoder ─▶ mixed ─▶ Voice bus
//!                       reorder, 60–200 ms        conceal, or the copy
//!                       of slack, gaps kept       inside the next packet
//! ```
//!
//! ## What a jitter buffer is for, and what it is not for
//!
//! Frames arrive early, late, out of order, twice, or not at all, and a decoder needs exactly
//! one every 20 ms. So each speaker gets a buffer that holds a target amount of slack before it
//! starts and then hands out one frame per slot: the frame belonging to that slot if it has
//! arrived, a repair if it has not. The slack is latency the listener pays for continuity,
//! which is why it starts at the smallest useful value and grows only when the network has
//! demonstrably needed it. **It is not a queue that drains** — one that played everything it
//! had would have no slack a moment later.
//!
//! ## A source per speaker, and one shared source behind them
//!
//! Since #854 a mixer slot can be given back, so [`hear`] takes one for each speaker it
//! starts listening to and the slot goes when the speaker does. [`place_the_speakers`] then
//! sets that source's gain, pan and occlusion every frame from the interpolated snapshot,
//! and [`Mixer::render`] applies them — which is why a voice comes from where its speaker is
//! standing rather than from the middle of the listener's head.
//!
//! **The pool is finite and the fallback is the old behaviour**, deliberately. A speaker who
//! arrives when every slot is taken gets no source of their own; their audio is summed into
//! the one source this module has always held and is heard unpositioned, which is exactly
//! what every speaker sounded like before #854. Being heard from the wrong place is a great
//! deal better than not being heard, and it is the same answer the acceptance criterion asks
//! for when the snapshot has not placed somebody.
//!
//! **The slot is decided once, when the speaker is first heard, and never mid-sentence.**
//! Moving a speaker from the shared sum onto a source of their own would leave up to
//! [`QUEUED_FRAMES`] frames of their audio already summed into the shared ring while the new
//! ring started filling, so they would briefly be heard twice.
//!
//! [`Mixer::render`]: super::mixer::Mixer::render
//!
//! Nothing here is written down. A voice frame is personal data, so nothing logs a payload, a
//! speaker, or a count of either: how often somebody spoke is a fact about a person.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use bevy::prelude::*;

use super::codec::{Missing, VoiceDecoder};
use super::dsp::FRAME_SAMPLES;
use super::listener::{Voices, forget_stale_voices};
use super::mixer::{Bus, SOURCE_CAPACITY, SourceHandle};
use super::spatial::{self, Placement};
use crate::net::{MAX_OPUS_BYTES, Session, VoiceInbox};
use crate::player::{EYE_HEIGHT, SnapshotBuffer, WorldCamera};
use crate::world::ChunkStore;

/// The slack a speaker's buffer starts with, in frames of 20 ms.
///
/// 60 ms: three frames, which absorbs the ordinary reordering of a home connection while
/// staying under what a conversation notices. The buffer grows from here and never starts
/// higher, because latency nobody needed is latency everybody pays.
const TARGET_FRAMES: usize = 3;

/// And the most it may grow to. 200 ms is where a conversation starts talking over itself.
const MAX_TARGET_FRAMES: usize = 10;

/// How many frames a speaker may hold at all.
///
/// **A bound on a length the peer chooses**, like every other at this boundary: a speaker
/// sending faster than real time would otherwise grow this map without limit. Twice the
/// largest target is slack for a burst and nothing like a backlog.
const MAX_HELD_FRAMES: usize = MAX_TARGET_FRAMES * 2;

/// How long a speaker may send nothing before they are released.
///
/// Much longer than a gap the buffer conceals: what this releases is somebody who has
/// stopped, not somebody mid-sentence.
const RELEASE_AFTER: Duration = Duration::from_millis(500);

/// How long a speaker counts as speaking for, once a frame of theirs has been played.
///
/// **Presentation, and the HUD in #852 part 8 is its only reader.** A second is long enough
/// that a name does not flicker between words and short enough that it is gone before the
/// listener wonders why it is still there.
pub const SPEAKING_FOR: Duration = Duration::from_millis(1_000);

/// How much audio is kept queued for the output callback, in frames.
///
/// The source ring is a quarter of a second; this is the depth each tick tops it back up to.
/// Four frames is 80 ms — over two ordinary 60 Hz ticks, so a long frame does not underrun,
/// and far short of the ring, so nothing here is what adds latency.
const QUEUED_FRAMES: usize = 4;

/// One speaker's frames, in order, with the slack a listener hears as continuity.
///
/// Keyed by the speaker's own sequence, which is presentation and never a clock: it makes
/// reordering fixable and a gap knowable, and nothing branches on its value.
#[derive(Debug)]
struct Jitter {
    /// Frames waiting, by sequence. A `BTreeMap` because what is asked for is "the lowest".
    frames: BTreeMap<u32, Vec<u8>>,
    /// The sequence the next slot wants. `None` until the first frame arrives.
    next: Option<u32>,
    /// How many frames of slack this speaker's buffer is holding to.
    target: usize,
    /// Whether the buffer has filled once and started playing.
    playing: bool,
    /// When a frame last arrived.
    arrived: Instant,
}

/// What a slot asks the decoder for.
#[derive(Debug, PartialEq, Eq)]
enum Slot {
    /// The frame that belongs to this slot, which arrived.
    Frame(Vec<u8>),
    /// It did not, and the one immediately after it has — so the redundant copy inside that
    /// one is the real frame at a lower bitrate.
    Recover(Vec<u8>),
    /// Neither. The decoder extrapolates from what it played.
    Conceal,
    /// The buffer has not filled, or has nothing at all. Silence and not a repair:
    /// concealing from a decoder that has decoded nothing is a guess about nothing.
    Nothing,
}

impl Jitter {
    fn new(now: Instant) -> Self {
        Self {
            frames: BTreeMap::new(),
            next: None,
            target: TARGET_FRAMES,
            playing: false,
            arrived: now,
        }
    }

    /// Accepts one relayed frame.
    ///
    /// **A frame for a slot already played is dropped, and it grows the target.** Arriving
    /// late is the only direct evidence this buffer has that its slack is too small, and it is
    /// the whole of the adaptation. Nothing shrinks the target inside a turn: a buffer that
    /// shrank on every on-time frame would spend the conversation oscillating.
    fn push(&mut self, sequence: u32, opus: Vec<u8>, now: Instant) {
        self.arrived = now;
        if self.playing
            && let Some(next) = self.next
            && sequence < next
        {
            self.target = (self.target + 1).min(MAX_TARGET_FRAMES);
            return;
        }
        // **A frame already held costs nothing to accept, so it evicts nothing.** The
        // eviction below used to run for a duplicate too: with the buffer full, a second copy
        // of frame 5 dropped frame 19 and then replaced itself, so *receiving* a frame
        // manufactured a gap — the opposite of what a jitter buffer is for (#923).
        if self.frames.contains_key(&sequence) {
            return;
        }
        // Bounded before the insert, and the *highest* goes: the map is ordered, so the
        // entries furthest from the slot being played are the ones a listener will reach
        // last, and dropping the lowest would throw away the frame about to be needed.
        while self.frames.len() >= MAX_HELD_FRAMES {
            let Some(highest) = self.frames.keys().next_back().copied() else {
                break;
            };
            if highest <= sequence {
                return;
            }
            self.frames.remove(&highest);
        }
        self.frames.insert(sequence, opus);
    }

    /// What the next 20 ms slot should be decoded from.
    fn slot(&mut self) -> Slot {
        if !self.playing {
            if self.frames.len() < self.target {
                return Slot::Nothing;
            }
            self.playing = true;
            // **The lowest held, not the first that arrived.** Which turned up first is a
            // fact about the network; where the run starts is a fact about the speaker, and
            // the wrong one plays the conversation out of order with every frame intact.
            self.next = self.frames.keys().next().copied();
        }
        let Some(next) = self.next else {
            return Slot::Nothing;
        };
        if self.frames.is_empty() {
            // Silence rather than a gap: a speaker who has stopped is silence, and
            // concealing it would be inventing audio nobody sent. The run ends here.
            self.playing = false;
            self.next = None;
            return Slot::Nothing;
        }

        self.next = Some(next.wrapping_add(1));
        // Everything before this slot is a frame that arrived too late to play.
        self.frames.retain(|sequence, _| *sequence >= next);
        if let Some(frame) = self.frames.remove(&next) {
            return Slot::Frame(frame);
        }
        // **Only the frame immediately after this slot can repair it** (#923). Opus's in-band
        // correction puts a copy of frame *N* inside packet *N + 1* and nowhere else, so
        // offering the lowest frame held — as the first version did — hands the decoder packet
        // 5 to recover slot 3 from: frame 4 comes out, played in slot 3 and again in slot 4,
        // for a gap concealment would have covered honestly.
        match self.frames.get(&next.wrapping_add(1)) {
            Some(after) => Slot::Recover(after.clone()),
            None => Slot::Conceal,
        }
    }

    /// Whether this speaker has said nothing for long enough to be let go.
    fn silent_since(&self, now: Instant) -> bool {
        now.duration_since(self.arrived) >= RELEASE_AFTER
    }
}

/// One speaker being listened to: their buffer, their decoder, and when they last spoke.
///
/// **A decoder each, and not as an optimisation.** An Opus decoder carries the state its
/// concealment extrapolates from, so two sharing one would each be concealed from the
/// other's audio.
#[derive(Debug)]
struct Speaker {
    jitter: Jitter,
    /// Whether a snapshot has ever named this speaker. Until one has, absence means nothing
    /// and the silence timer is what releases them — see [`release_speakers`] (#923).
    seen: bool,
    /// `None` when libopus would not open one. That costs this speaker their voice and
    /// nothing else — never a panic, and never a retry per frame, which is why the entry is
    /// made either way.
    decoder: Option<VoiceDecoder>,
    /// This speaker's own mixer slot, when one was free when they were first heard.
    ///
    /// `None` is not a failure: it means they are summed into the shared source and heard
    /// unpositioned. Dropping the handle — which happens when [`release_speakers`] drops the
    /// whole entry — gives the slot back.
    source: Option<SourceHandle>,
    /// When [`place_the_speakers`] last cast this speaker's occlusion rays.
    ///
    /// `None` until the first cast, so a speaker who has just been placed is not heard
    /// through a wall for a tenth of a second before anybody looks.
    rays_at: Option<Instant>,
    /// What those rays answered, held until the next cast. The mixer smooths it; this is the
    /// raw reading and steps.
    occlusion: f32,
}

/// Who has been heard, and when they were last played.
///
/// **Presentation only.** The HUD names the speakers heard in the last second; nothing decides
/// anything from it, which is the rule `client/AGENTS.md` states for everything under `audio/`.
///
/// **Its lifetime is [`SPEAKING_FOR`] and nothing else's**, which the first version got wrong:
/// `release_speakers` forgot an entry the moment it let go of the speaker's decoder, so a name
/// could not outlive [`RELEASE_AFTER`] and the second half of the window this constant
/// describes was unreachable in the assembled client. The two are different questions on
/// different clocks — a decoder is held while frames are *arriving*, a name is shown for a
/// second after one was *played* — and the second is longer on purpose: a name that vanished
/// the instant somebody stopped talking would flicker between sentences and be gone before a
/// listener could look at it. Found by review on #924, and only findable by reading this file
/// against the HUD, because in isolation each half is correct.
#[derive(Resource, Debug, Default)]
pub struct Speaking(Vec<(u64, Instant)>);

impl Speaking {
    /// Every entity heard within [`SPEAKING_FOR`] of `now`, in the order they were first
    /// heard, so a name does not move in a list while somebody is reading it.
    pub fn recent(&self, now: Instant) -> Vec<u64> {
        self.0
            .iter()
            .filter(|(_, at)| now.duration_since(*at) < SPEAKING_FOR)
            .map(|(entity_id, _)| *entity_id)
            .collect()
    }

    fn heard(&mut self, entity_id: u64, at: Instant) {
        match self.0.iter_mut().find(|(held, _)| *held == entity_id) {
            Some((_, when)) => *when = at,
            None => self.0.push((entity_id, at)),
        }
    }

    /// Records a speaker as heard, as `play` does. Test-only, so `ui/voice.rs` can be driven
    /// without a decoder or a mixer.
    #[cfg(test)]
    pub fn heard_for_test(&mut self, entity_id: u64, at: Instant) {
        self.heard(entity_id, at);
    }

    /// Drops every entry older than [`SPEAKING_FOR`].
    ///
    /// **Age is the only thing that removes one.** Not the speaker being released, not their
    /// entity leaving the snapshot: somebody who walked out of range a moment ago was still
    /// heard in the last second, and that is what this list is. Pruning by age is also what
    /// bounds it — an entry lives one second whatever else happens.
    fn forget_stale(&mut self, now: Instant) {
        self.0
            .retain(|(_, at)| now.duration_since(*at) < SPEAKING_FOR);
    }
}

/// Everything the playback half carries between ticks.
#[derive(Resource)]
struct Listening {
    /// One per speaker being heard. Removed by [`release_speakers`] and nothing else.
    ///
    /// **The `Mutex` is a type obligation and not synchronisation**, as `NetLink`'s is:
    /// libopus's decoder holds a raw pointer, so it is `Send` and not `Sync`, and a Bevy
    /// resource must be `Sync`. The one accessor uses `get_mut`, so no lock is ever taken.
    speakers: Mutex<HashMap<u64, Speaker>>,
    /// The one source every speaker is summed into. `None` when the mixer had no slot free,
    /// which is a silent client and not a broken one.
    source: Option<SourceHandle>,
    /// One frame of one speaker, reused.
    decoded: Vec<f32>,
    /// One frame of everybody, reused.
    mixed: Vec<f32>,
}

impl Listening {
    fn new(source: Option<SourceHandle>) -> Self {
        Self {
            speakers: Mutex::new(HashMap::new()),
            source,
            decoded: vec![0.0; FRAME_SAMPLES],
            mixed: vec![0.0; FRAME_SAMPLES],
        }
    }
}

/// Adds the inbound voice pipeline. Built by [`super::AudioPlugin`].
pub(super) struct HeardPlugin;

impl Plugin for HeardPlugin {
    fn build(&self, app: &mut App) {
        let source = app
            .world()
            .get_resource::<super::AudioMixer>()
            .and_then(|mixer| mixer.claim(Bus::Voice));
        if source.is_none() {
            warn!("no mixer source is free for voice; nobody will be heard");
        }
        app.insert_resource(Listening::new(source))
            .init_resource::<Speaking>()
            .init_resource::<Voices>()
            .add_systems(
                Update,
                (
                    hear,
                    // Between the two on purpose: a speaker heard for the first time this
                    // tick is placed before a single sample of theirs is queued, so nothing
                    // of them is ever pushed into the shared source and then repeated from
                    // a source of their own.
                    place_the_speakers,
                    play,
                    release_speakers,
                    forget_stale_voices,
                )
                    .chain(),
            );
    }
}

/// Puts every relayed frame into its speaker's buffer, and gives each new speaker a mixer
/// source of their own if one is free.
///
/// **The slot is taken here, beside the decoder, and for the same reason**: both are per
/// speaker, both are made once, and both go when [`release_speakers`] drops the entry. A
/// slot that could not be had is `None` and costs that speaker their position, not their
/// voice — see the module doc.
fn hear(
    mut inbox: ResMut<VoiceInbox>,
    mut listening: ResMut<Listening>,
    mixer: Option<Res<super::AudioMixer>>,
) {
    let heard = inbox.take();
    if heard.is_empty() {
        return;
    }
    let now = Instant::now();
    let listening = &mut *listening;
    let speakers = match listening.speakers.get_mut() {
        Ok(speakers) => speakers,
        Err(poisoned) => poisoned.into_inner(),
    };
    for frame in heard {
        // Refused rather than trusted: the buffer allocates from this, and every length a
        // peer chooses gets a bound. The decode boundary enforces the same rule.
        if frame.opus.is_empty() || frame.opus.len() > MAX_OPUS_BYTES {
            continue;
        }
        let speaker = speakers.entry(frame.speaker_entity_id).or_insert_with(|| {
            Speaker {
                jitter: Jitter::new(now),
                seen: false,
                // A decoder that will not open costs this speaker their voice and nothing
                // else: the entry is still made, so nothing retries per frame.
                decoder: VoiceDecoder::new().map_err(|err| warn!("{err}")).ok(),
                source: mixer.as_deref().and_then(|mixer| mixer.claim(Bus::Voice)),
                rays_at: None,
                occlusion: 0.0,
            }
        });
        speaker.jitter.push(frame.sequence, frame.opus, now);
    }
}

/// How often each speaker's occlusion rays are cast.
///
/// **10 Hz, and it is a cost decision with a stated bound.** Three rays each up to a few
/// hundred voxels long, per speaker, is far too much to do sixty times a second and far more
/// often than a wall moves. What it costs in responsiveness is bounded by the smoothing it
/// feeds: the mixer takes 50 ms to close a filter and 300 ms to open one, so a reading that
/// is up to 100 ms old is being ramped towards over a window of the same order and there is
/// nothing for the ear to catch. `player/ambience.rs` samples the world on a fixed period
/// for the same reason.
const RAY_PERIOD: Duration = Duration::from_millis(100);

/// Tells each speaker's mixer source where that speaker is standing.
///
/// **Every frame, and from the same interpolated answer the renderer draws them at**: the
/// speaker's eye is their interpolated feet plus [`EYE_HEIGHT`], the listener's eye is the
/// `WorldCamera`'s translation — which `player/camera.rs` documents as the authoritative
/// interpolated position — and the azimuth is measured against that camera's yaw.
///
/// **Occlusion is the one part that is not per frame**, because it is the one part that
/// walks the world. Its cadence is [`RAY_PERIOD`], and the reading is held between casts.
///
/// **A speaker the snapshot does not place is set to [`Placement::UNPOSITIONED`]**, which is
/// unity in both ears and no filter — the acceptance criterion's "unpositioned mono at bus
/// gain", and exactly what this mixer did for every source before #854. That covers a
/// speaker who has just come into range and is not in a snapshot yet, a session with no
/// snapshot at all, and a client whose camera has not been spawned.
///
/// Nothing here decides anything. The server sent every frame this places, and where a voice
/// appears to come from is presentation like every other thing under `audio/`.
fn place_the_speakers(
    mut listening: ResMut<Listening>,
    session: Option<Res<Session>>,
    snapshots: Option<Res<SnapshotBuffer>>,
    store: Option<Res<ChunkStore>>,
    eyes: Query<&Transform, With<WorldCamera>>,
) {
    let listening = &mut *listening;
    let speakers = match listening.speakers.get_mut() {
        Ok(speakers) => speakers,
        Err(poisoned) => poisoned.into_inner(),
    };
    if speakers.is_empty() {
        return;
    }

    let now = Instant::now();
    let listener = session
        .as_deref()
        .zip(eyes.iter().next())
        .map(|(session, eye)| Listener {
            eye: eye.translation,
            // The camera's rotation is `from_rotation_y(yaw) * from_rotation_x(pitch)`, so
            // `YXZ` reads that yaw back exactly. Taking a horizontal forward from the
            // transform instead would be a tiny vector to normalise at a steep pitch.
            yaw: eye.rotation.to_euler(EulerRot::YXZ).0,
            range: session.0.voice_range_blocks,
            chunk_size: usize::from(session.0.chunk_size),
            positions: snapshots
                .as_deref()
                .map(|snapshots| {
                    snapshots.sample(
                        now,
                        Duration::from_secs(1) / u32::from(session.0.tick_rate.max(1)),
                    )
                })
                .unwrap_or_default(),
        });

    for (entity_id, speaker) in speakers.iter_mut() {
        // A speaker the mixer had no slot for has nowhere to be placed, and casting their
        // rays would be work with no reader.
        if speaker.source.is_none() {
            continue;
        }

        let mut placement = None;
        if let Some(listener) = listener.as_ref()
            && let Some((_, at)) = listener.positions.iter().find(|(id, _)| id == entity_id)
        {
            let speaker_eye = at.pos + Vec3::Y * EYE_HEIGHT;
            if speaker
                .rays_at
                .is_none_or(|last| now.duration_since(last) >= RAY_PERIOD)
            {
                speaker.occlusion = store.as_deref().map_or(0.0, |store| {
                    spatial::occlusion(store, listener.chunk_size, listener.eye, speaker_eye)
                });
                speaker.rays_at = Some(now);
            }
            placement = Some(spatial::place(
                listener.eye,
                listener.yaw,
                speaker_eye,
                listener.range,
                speaker.occlusion,
            ));
        }

        // **One place a source is set, and one fallback.** Every way of not knowing where
        // somebody is ends here: no session, no camera, no snapshot yet, or a snapshot that
        // does not name them. Writing the fallback per branch would let one of them be
        // forgotten, and a forgotten one leaves a voice panned to wherever it last was.
        if let Some(source) = speaker.source.as_ref() {
            source.place(placement.unwrap_or(Placement::UNPOSITIONED));
        }
    }
}

/// Everything one frame's placements are measured against.
///
/// Gathered once rather than per speaker, because the camera is read through a query and the
/// snapshot sample allocates: doing either inside the loop would be the same answer computed
/// as many times as somebody is talking.
struct Listener {
    /// The authoritative interpolated eye — `player/camera.rs`'s translation.
    eye: Vec3,
    yaw: f32,
    /// The server's relay range, never a constant here.
    range: f32,
    chunk_size: usize,
    positions: Vec<(u64, crate::player::Interpolated)>,
}

/// Decodes what is due and mixes it into the voice source.
///
/// **The per-speaker gain is multiplied in here, and it is not an audibility decision.** The
/// server sent the frame, which is its answer that this speaker may be heard; what
/// `audio/listener.rs` supplies is a number the listener chose for their own ears. A muted
/// speaker is still decoded, deliberately: an Opus decoder's output depends on everything
/// before it, so skipping the decode would make the first frame after an un-mute an artefact,
/// and the jitter buffer would stop advancing through slots it has to advance through anyway.
fn play(
    mut listening: ResMut<Listening>,
    mut speaking: ResMut<Speaking>,
    mut voices: ResMut<Voices>,
) {
    let listening = &mut *listening;
    let speakers = match listening.speakers.get_mut() {
        Ok(speakers) => speakers,
        Err(poisoned) => poisoned.into_inner(),
    };
    if speakers.is_empty() {
        return;
    }
    let now = Instant::now();

    // Everybody with a source of their own, each paced by their own ring. One speaker's
    // buffer running dry no longer stops another's slot being topped up, which it did while
    // every speaker shared one ring and one frame count.
    for (entity_id, speaker) in speakers.iter_mut() {
        // Read once, before the loop that decodes into this speaker: `speaker` has to be
        // borrowed mutably below, so the handle is reached through it a statement at a time
        // rather than held across the body.
        let Some(wanted) = speaker.source.as_ref().map(frames_wanted) else {
            continue;
        };
        for _ in 0..wanted {
            if !decode_slot(speaker, &mut listening.decoded) {
                break;
            }
            speaking.heard(*entity_id, now);
            voices.heard(*entity_id, now);
            let gain = voices.gain(*entity_id);
            // Clamped for the same reason the sum below is: a listener may push a single
            // speaker to twice unity, and the mixer is no place to absorb it.
            for sample in listening.decoded.iter_mut() {
                *sample = (*sample * gain).clamp(-1.0, 1.0);
            }
            if let Some(source) = speaker.source.as_ref() {
                source.push(&listening.decoded);
            }
        }
    }

    // And everybody the mixer had no slot for, summed into the one source this module holds
    // and heard unpositioned — which is what every speaker sounded like before #854.
    let Some(shared) = listening.source.as_ref() else {
        return;
    };
    // Topped up rather than drained: a buffer that played everything it had would have no
    // slack a moment later.
    for _ in 0..frames_wanted(shared) {
        listening.mixed.fill(0.0);
        let mut anybody = false;
        for (entity_id, speaker) in speakers.iter_mut() {
            if speaker.source.is_some() {
                continue;
            }
            if !decode_slot(speaker, &mut listening.decoded) {
                continue;
            }
            anybody = true;
            speaking.heard(*entity_id, now);
            voices.heard(*entity_id, now);
            // Zero for a muted speaker, so they contribute nothing to the sum while their
            // decoder stays exactly where an un-mute would need it.
            let gain = voices.gain(*entity_id);
            for (out, sample) in listening.mixed.iter_mut().zip(listening.decoded.iter()) {
                *out += *sample * gain;
            }
        }
        if !anybody {
            break;
        }
        // Clamped here rather than in the mixer's sum, so a crowd is quiet distortion on this
        // bus rather than a master stage that has to absorb it.
        for sample in listening.mixed.iter_mut() {
            *sample = sample.clamp(-1.0, 1.0);
        }
        shared.push(&listening.mixed);
    }
}

/// How many more 20 ms frames `source` wants before it is topped back up to
/// [`QUEUED_FRAMES`].
///
/// `SOURCE_CAPACITY - free` is what is still queued.
fn frames_wanted(source: &SourceHandle) -> usize {
    let queued = SOURCE_CAPACITY.saturating_sub(source.free());
    (QUEUED_FRAMES * FRAME_SAMPLES).saturating_sub(queued) / FRAME_SAMPLES
}

/// Advances `speaker`'s buffer by one slot and decodes it into `decoded`.
///
/// Answers whether `decoded` now holds audio. `false` covers all three ways it might not:
/// a buffer with nothing due, a speaker whose decoder would not open, and a frame that would
/// not decode — the last of which is a gap and never a log line, because the message would
/// name a speaker.
///
/// **The slot is taken before the decoder is checked, and the order is not incidental.** A
/// jitter buffer advances through its slots whether or not anybody can decode them; skipping
/// the advance for a speaker with no decoder would leave their buffer holding frames it
/// should have retired.
fn decode_slot(speaker: &mut Speaker, decoded: &mut [f32]) -> bool {
    let slot = speaker.jitter.slot();
    let Some(decoder) = speaker.decoder.as_mut() else {
        return false;
    };
    let outcome = match &slot {
        Slot::Nothing => return false,
        Slot::Frame(frame) => decoder.decode(frame, decoded),
        Slot::Recover(next) => decoder.repair(Missing::FromTheNext, Some(next), decoded),
        Slot::Conceal => decoder.repair(Missing::Conceal, None, decoded),
    };
    outcome.is_ok()
}

/// Lets go of speakers who have stopped, and of speakers who are no longer there.
///
/// **Two releases, and they are different questions.** A speaker whose frames stop has stopped
/// talking; one whose entity has left the snapshot has gone, and the server stopped relaying
/// them at the same moment — so waiting out the silence timer there would hold a decoder for
/// half a second of nothing.
///
/// **It releases decoders, and it does not release names.** [`Speaking`] is pruned by age here
/// because this is the system that runs every tick, not because a released speaker should be
/// forgotten — see that type for why the two lifetimes are separate.
fn release_speakers(
    mut listening: ResMut<Listening>,
    mut speaking: ResMut<Speaking>,
    snapshots: Option<Res<SnapshotBuffer>>,
) {
    let now = Instant::now();
    let listening = &mut *listening;
    let speakers = match listening.speakers.get_mut() {
        Ok(speakers) => speakers,
        Err(poisoned) => poisoned.into_inner(),
    };
    speakers.retain(|entity_id, speaker| {
        // **Two things have to be true before absence means anything** (#923). A snapshot has
        // to exist — "nothing to read" is never "not present". And this speaker has to have
        // *been* in one: voice and snapshots arrive on their own schedules, so somebody who
        // starts talking between two of them is genuinely missing from the newest, and
        // releasing them there drops the first part of every voice that comes into range.
        // Until the world confirms them once, the silence timer is what lets them go.
        let present = snapshots
            .as_deref()
            .is_some_and(|snapshots| snapshots.holds_entity(*entity_id));
        speaker.seen |= present;
        let gone = speaker.seen && !present;
        !gone && !speaker.jitter.silent_since(now)
    });
    speaking.forget_stale(now);
}

/// **No test here opens a device or a socket.** The wire side is `VoiceInbox::push_for_test`,
/// the mixer side is a `Mixer` rendering into a `Vec`, and the codec is real libopus.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::codec::VoiceEncoder;
    use crate::audio::mixer::Mixer;
    use crate::net::{EntityState, Snapshot, VoiceHeard};
    use std::sync::Arc;

    /// Twelve real Opus frames of a tone, so the decoder is fed something it can decode.
    fn frames() -> Vec<Vec<u8>> {
        let mut encoder = VoiceEncoder::new().expect("libopus is linked");
        (0..12)
            .map(|at| {
                let block: Vec<f32> = (0..FRAME_SAMPLES)
                    .map(|index| {
                        let sample = (at * FRAME_SAMPLES + index) as f32;
                        let phase = std::f32::consts::TAU * 300.0 * sample / 48_000.0;
                        0.4 * phase.sin()
                    })
                    .collect();
                encoder.encode(&block).expect("the frame encodes").to_vec()
            })
            .collect()
    }

    /// A buffer with `count` frames in it, in order, and the moment they arrived.
    fn filled(count: usize) -> (Jitter, Instant, Vec<Vec<u8>>) {
        let now = Instant::now();
        let opus = frames();
        let mut jitter = Jitter::new(now);
        for (sequence, frame) in opus.iter().enumerate().take(count) {
            jitter.push(sequence as u32, frame.clone(), now);
        }
        (jitter, now, opus)
    }

    /// **The buffer holds its slack before it plays anything.** One that played the first
    /// frame it received would have no slack a moment later and conceal every reordering.
    #[test]
    fn a_buffer_waits_for_its_target_before_it_plays() {
        let now = Instant::now();
        let opus = frames();
        let mut jitter = Jitter::new(now);
        assert_eq!(jitter.slot(), Slot::Nothing, "an empty buffer played");

        for sequence in 0..TARGET_FRAMES as u32 - 1 {
            jitter.push(sequence, opus[sequence as usize].clone(), now);
            assert_eq!(jitter.slot(), Slot::Nothing, "it played under its target");
        }
        jitter.push(
            TARGET_FRAMES as u32 - 1,
            opus[TARGET_FRAMES - 1].clone(),
            now,
        );
        assert_eq!(jitter.slot(), Slot::Frame(opus[0].clone()));
        assert_eq!(jitter.slot(), Slot::Frame(opus[1].clone()));
    }

    /// Out-of-order frames are played in order, and one that arrives twice is played once.
    #[test]
    fn reordering_is_fixed_and_a_duplicate_is_played_once() {
        let now = Instant::now();
        let opus = frames();
        let mut jitter = Jitter::new(now);
        for sequence in [2u32, 0, 1, 1, 3] {
            jitter.push(sequence, opus[sequence as usize].clone(), now);
        }
        for (expected, frame) in opus.iter().enumerate().take(4) {
            assert_eq!(
                jitter.slot(),
                Slot::Frame(frame.clone()),
                "slot {expected} played out of order"
            );
        }
        assert_eq!(jitter.slot(), Slot::Nothing, "the duplicate played twice");
    }

    /// The frame after a missing one carries its redundant copy, so a slot with that frame
    /// present asks for recovery and one without it asks for concealment.
    #[test]
    fn a_missing_frame_is_recovered_from_the_next_or_concealed() {
        let now = Instant::now();
        let opus = frames();
        let mut jitter = Jitter::new(now);
        // 0, 1, 2 present; 3 missing; 4 present.
        for sequence in [0u32, 1, 2, 4] {
            jitter.push(sequence, opus[sequence as usize].clone(), now);
        }
        for frame in opus.iter().take(3) {
            assert_eq!(jitter.slot(), Slot::Frame(frame.clone()));
        }
        assert_eq!(
            jitter.slot(),
            Slot::Recover(opus[4].clone()),
            "the frame after the gap was not offered for recovery"
        );
        assert_eq!(jitter.slot(), Slot::Frame(opus[4].clone()));

        // And with nothing after the gap at all, the buffer goes quiet rather than
        // concealing indefinitely — a speaker who stopped is silence, not invented audio.
        assert_eq!(jitter.slot(), Slot::Nothing);
    }

    /// Concealment is asked for when the gap is *inside* a run that continues.
    #[test]
    fn a_gap_with_nothing_recoverable_is_concealed() {
        let now = Instant::now();
        let opus = frames();
        let mut jitter = Jitter::new(now);
        for sequence in [0u32, 1, 2] {
            jitter.push(sequence, opus[sequence as usize].clone(), now);
        }
        for frame in opus.iter().take(3) {
            assert_eq!(jitter.slot(), Slot::Frame(frame.clone()));
        }
        // **Slot 3 is missing and only 5 has arrived.** Packet 5 carries a copy of frame 4 and
        // of nothing else, so it cannot repair slot 3 — this is the honest case for
        // concealment, and the first version of this test asserted `Recover(opus[5])` under a
        // comment stating the very fact that makes it wrong. Found by review on #923.
        jitter.push(5, opus[5].clone(), now);
        assert_eq!(
            jitter.slot(),
            Slot::Conceal,
            "a gap was repaired from a packet that does not carry it"
        );
        // Slot 4 is the one packet 5 *can* repair, so the next slot does recover.
        assert_eq!(jitter.slot(), Slot::Recover(opus[5].clone()));
        assert_eq!(jitter.slot(), Slot::Frame(opus[5].clone()));
    }

    /// **A duplicate costs nothing to accept, so it evicts nothing** (#923). With the buffer
    /// full, a second copy of a held frame used to drop the highest unique one and replace
    /// itself — so *receiving* a frame manufactured a gap.
    #[test]
    fn a_duplicate_never_pushes_a_unique_frame_out() {
        let now = Instant::now();
        let opus = frames();
        let mut jitter = Jitter::new(now);
        for sequence in 0..MAX_HELD_FRAMES as u32 {
            jitter.push(sequence, opus[sequence as usize % opus.len()].clone(), now);
        }
        assert_eq!(jitter.frames.len(), MAX_HELD_FRAMES);
        let highest = MAX_HELD_FRAMES as u32 - 1;
        assert!(jitter.frames.contains_key(&highest));

        jitter.push(5, opus[5 % opus.len()].clone(), now);
        assert!(
            jitter.frames.contains_key(&highest),
            "a duplicate pushed the highest unique frame out"
        );
        assert_eq!(jitter.frames.len(), MAX_HELD_FRAMES);
    }

    /// **The buffer grows when the network makes it, and only then.** A frame arriving after
    /// its slot is the one direct evidence the slack was too small; nothing else moves it.
    #[test]
    fn a_late_frame_grows_the_target_and_an_early_one_does_not() {
        let (mut jitter, now, opus) = filled(TARGET_FRAMES);
        assert_eq!(jitter.target, TARGET_FRAMES);
        for _ in 0..TARGET_FRAMES {
            jitter.slot();
        }
        // Frame 0 turns up after its slot has been and gone.
        jitter.push(0, opus[0].clone(), now);
        assert_eq!(jitter.target, TARGET_FRAMES + 1);
        assert!(
            !jitter.frames.contains_key(&0),
            "a late frame was kept to be played"
        );

        // And it stops at the ceiling rather than growing without bound. Pushed with no slot
        // between them: a slot that finds the buffer empty ends the run, and a frame that
        // arrives while nothing is playing is early rather than late.
        for _ in 0..50 {
            jitter.push(0, opus[0].clone(), now);
        }
        assert_eq!(jitter.target, MAX_TARGET_FRAMES);
    }

    /// A speaker sending faster than real time cannot grow the buffer without limit.
    #[test]
    fn a_speaker_cannot_grow_the_buffer_without_limit() {
        let now = Instant::now();
        let opus = frames();
        let mut jitter = Jitter::new(now);
        for sequence in 0..500u32 {
            jitter.push(
                sequence,
                opus[(sequence as usize) % opus.len()].clone(),
                now,
            );
        }
        assert!(
            jitter.frames.len() <= MAX_HELD_FRAMES,
            "the buffer held {} frames",
            jitter.frames.len()
        );
        // What it kept is the frames nearest the slot being played, not the newest.
        assert_eq!(jitter.slot(), Slot::Frame(opus[0].clone()));
    }

    /// The silence release, at the acceptance criterion's 500 ms.
    #[test]
    fn a_speaker_who_stops_is_released_after_five_hundred_milliseconds() {
        let (jitter, now, _) = filled(TARGET_FRAMES);
        assert!(!jitter.silent_since(now));
        assert!(!jitter.silent_since(now + Duration::from_millis(499)));
        assert!(jitter.silent_since(now + RELEASE_AFTER));
    }

    /// The whole inbound path, through real libopus and a real mixer.
    fn listening_app() -> App {
        let mixer = Arc::new(Mixer::new());
        mixer.set_format(48_000, 1);
        mixer.set_gain(Bus::Voice, 1.0);
        mixer.set_gain(Bus::Master, 1.0);
        let source = mixer.claim(Bus::Voice).expect("a free slot");
        let mut app = App::new();
        app.insert_resource(super::super::AudioMixer(Arc::clone(&mixer)))
            .insert_resource(Listening::new(Some(source)))
            .init_resource::<Speaking>()
            .init_resource::<Voices>()
            .init_resource::<VoiceInbox>()
            .add_systems(
                Update,
                // The plugin's own chain, in its order: what these tests exercise is the
                // assembled behaviour, and a test that ran the systems in a different order
                // from the client would be measuring a client nobody ships.
                (
                    hear,
                    place_the_speakers,
                    play,
                    release_speakers,
                    forget_stale_voices,
                )
                    .chain(),
            );
        app
    }

    fn say(app: &mut App, entity_id: u64, sequence: u32, opus: Vec<u8>) {
        app.world_mut()
            .resource_mut::<VoiceInbox>()
            .push_for_test(VoiceHeard {
                speaker_entity_id: entity_id,
                sequence,
                opus,
            });
    }

    /// How loud what has been queued for the output callback is.
    fn heard_level(app: &App) -> f32 {
        let mixer = app.world().resource::<super::super::AudioMixer>();
        struct VecSink(Vec<f32>);
        impl crate::audio::mixer::Sink for VecSink {
            fn block(&mut self) -> &mut [f32] {
                &mut self.0
            }
        }
        let mut sink = VecSink(vec![0.0; FRAME_SAMPLES * QUEUED_FRAMES]);
        mixer.0.render(&mut sink);
        crate::audio::dsp::level_db(&sink.0)
    }

    #[test]
    fn a_relayed_frame_becomes_audio_on_the_voice_bus() {
        let mut app = listening_app();
        let opus = frames();
        for (sequence, frame) in opus.iter().enumerate().take(6) {
            say(&mut app, 7, sequence as u32, frame.clone());
        }
        app.update();

        let level = heard_level(&app);
        assert!(
            level > -30.0,
            "nothing audible reached the voice bus ({level} dB)"
        );
    }

    /// **A muted speaker contributes nothing, and a turned-down one contributes less.**
    ///
    /// Measured off the bus rather than off the gain, because "the gain was read" is a claim
    /// about a call and this is a claim about what a listener hears. The frames still arrive
    /// and are still decoded — the server decided this speaker may be heard and nothing here
    /// overrules that — they are multiplied by zero on the way into the sum.
    #[test]
    fn a_muted_speaker_is_silence_and_a_quietened_one_is_quieter() {
        let opus = frames();

        let mut app = listening_app();
        for (sequence, frame) in opus.iter().enumerate().take(6) {
            say(&mut app, 7, sequence as u32, frame.clone());
        }
        app.world_mut().resource_mut::<Voices>().toggle_mute(7);
        app.update();
        let muted = heard_level(&app);
        assert!(
            muted < -100.0,
            "a muted speaker was audible on the voice bus ({muted} dB)"
        );

        // The same frames, the same speaker, half as loud: audible, and measurably under what
        // the unmuted test above asserts.
        let mut app = listening_app();
        for (sequence, frame) in opus.iter().enumerate().take(6) {
            say(&mut app, 7, sequence as u32, frame.clone());
        }
        app.world_mut()
            .resource_mut::<Voices>()
            .adjust_volume(7, -5);
        app.update();
        let halved = heard_level(&app);
        assert!(
            halved > -100.0,
            "turning a speaker down to half silenced them ({halved} dB)"
        );

        let mut app = listening_app();
        for (sequence, frame) in opus.iter().enumerate().take(6) {
            say(&mut app, 7, sequence as u32, frame.clone());
        }
        app.update();
        let whole = heard_level(&app);
        assert!(
            halved < whole - 3.0,
            "half the volume was not audibly quieter: {halved} dB against {whole} dB"
        );
    }

    /// **A muted speaker is still on the panel's roster**, which is the distinction the
    /// Voices panel rests on: a row a player muted has to stay somewhere they can un-mute it.
    #[test]
    fn a_muted_speaker_is_still_somebody_the_panel_lists() {
        let mut app = listening_app();
        let opus = frames();
        app.world_mut().resource_mut::<Voices>().toggle_mute(7);
        for (sequence, frame) in opus.iter().enumerate().take(6) {
            say(&mut app, 7, sequence as u32, frame.clone());
        }
        app.update();
        assert_eq!(
            app.world().resource::<Voices>().recent(Instant::now()),
            vec![7],
            "a muted speaker fell off the list that offers the un-mute"
        );
    }

    /// Two speakers are summed rather than one of them winning.
    /// Two speakers are summed rather than one of them winning.
    #[test]
    fn two_speakers_are_both_heard() {
        let mut app = listening_app();
        let opus = frames();
        for (sequence, frame) in opus.iter().enumerate().take(6) {
            say(&mut app, 7, sequence as u32, frame.clone());
            say(&mut app, 9, sequence as u32, frame.clone());
        }
        app.update();
        let both = heard_level(&app);

        let mut alone = listening_app();
        for (sequence, frame) in opus.iter().enumerate().take(6) {
            say(&mut alone, 7, sequence as u32, frame.clone());
        }
        alone.update();
        let one = heard_level(&alone);

        assert!(
            both > one + 3.0,
            "two speakers ({both} dB) were no louder than one ({one} dB)"
        );
    }

    /// **A frame no decoder should have been given is refused before it reaches one.** The
    /// decode boundary enforces the same rule; the buffer allocates from that length too.
    #[test]
    fn a_frame_outside_the_contracts_bounds_never_reaches_a_decoder() {
        let mut app = listening_app();
        say(&mut app, 7, 0, Vec::new());
        say(&mut app, 7, 1, vec![0x78; MAX_OPUS_BYTES + 1]);
        app.update();
        assert!(
            heard_level(&app) <= crate::audio::dsp::SILENCE_DB + 1.0,
            "a refused frame was played"
        );
    }

    /// **A voice that arrives before the world does is not released** (#923). Somebody who
    /// starts talking between two snapshots is genuinely missing from the newest, and
    /// releasing them there drops the first part of every voice that comes into range —
    /// in the same frame its buffer was created.
    #[test]
    fn a_speaker_the_world_has_not_confirmed_yet_is_not_released() {
        let mut app = listening_app();
        app.init_resource::<SnapshotBuffer>();
        let opus = frames();
        for (sequence, frame) in opus.iter().enumerate().take(6) {
            say(&mut app, 7, sequence as u32, frame.clone());
        }
        app.update();
        assert!(
            heard_level(&app) > -30.0,
            "the first thing said by a newly nearby speaker was thrown away"
        );

        // A snapshot arrives naming somebody else. **This is the state the finding is about**
        // — a snapshot exists and this speaker is not in it — and it must not release them,
        // because the world has never confirmed them and may simply be behind the voice.
        let named = |entity_id: u64, tick: u32| Snapshot {
            server_tick: tick,
            entities: vec![EntityState {
                entity_id,
                pos: [0.0; 3],
                vel: [0.0; 3],
                yaw: 0.0,
            }],
            ..Snapshot::default()
        };
        app.world_mut()
            .resource_mut::<SnapshotBuffer>()
            .accept(named(9, 1), Instant::now());
        for _ in 0..5 {
            app.update();
        }
        {
            let listening = app.world().resource::<Listening>();
            let speakers = listening.speakers.lock().expect("no test poisons it");
            assert!(
                speakers.contains_key(&7),
                "a speaker no snapshot had ever named was released for being absent"
            );
            assert!(
                !speakers[&7].seen,
                "the world confirmed a speaker it never named"
            );
        }

        // And once the world *has* confirmed them, absence does release them: the rule is
        // "not confirmed yet", not "never released".
        app.world_mut()
            .resource_mut::<SnapshotBuffer>()
            .accept(named(7, 2), Instant::now());
        app.update();
        app.world_mut()
            .resource_mut::<SnapshotBuffer>()
            .accept(named(9, 3), Instant::now());
        app.update();
        let listening = app.world().resource::<Listening>();
        let speakers = listening.speakers.lock().expect("no test poisons it");
        assert!(
            !speakers.contains_key(&7),
            "a speaker the world confirmed and then dropped was kept"
        );
    }

    // ---------------------------------------------------------------------------
    // Spatialisation, through the assembled systems
    // ---------------------------------------------------------------------------

    use crate::net::{ChunkCoord, SessionParams};
    use crate::world::{VoxelChunk, palette};

    const CHUNK: u16 = 32;
    /// A relay range wide enough that the test speakers are inside it and the distance
    /// curve has not taken them to nothing.
    const VOICE_RANGE: f32 = 32.0;

    fn spatial_session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 1,
            spawn: [4.5, 4.0, 4.5],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: CHUNK,
            view_distance: 8,
            inventory_slots: 37,
            hotbar_slots: 9,
            equipment_slots: 4,
            player_token: crate::net::ANY_TOKEN,
            voice_range_blocks: VOICE_RANGE,
        })
    }

    /// The same app as [`listening_app`] with two channels, a world, a camera and a
    /// snapshot — everything [`place_the_speakers`] reads.
    ///
    /// The camera sits at the origin facing Bevy's `-Z`, which is yaw zero, so a speaker at
    /// `+X` is on the listener's right and one at `-X` is on their left.
    fn placed_app(store: ChunkStore) -> App {
        let mixer = Arc::new(Mixer::new());
        mixer.set_format(48_000, 2);
        mixer.set_gain(Bus::Voice, 1.0);
        mixer.set_gain(Bus::Master, 1.0);
        let source = mixer.claim(Bus::Voice).expect("a free slot");
        let mut app = App::new();
        app.insert_resource(super::super::AudioMixer(Arc::clone(&mixer)))
            .insert_resource(Listening::new(Some(source)))
            .insert_resource(spatial_session())
            .insert_resource(store)
            .init_resource::<SnapshotBuffer>()
            .init_resource::<Speaking>()
            .init_resource::<Voices>()
            .init_resource::<VoiceInbox>()
            .add_systems(
                Update,
                (
                    hear,
                    place_the_speakers,
                    play,
                    release_speakers,
                    forget_stale_voices,
                )
                    .chain(),
            );
        app.world_mut().spawn((
            WorldCamera,
            Transform::from_translation(Vec3::new(4.5, 4.5, 4.5)),
        ));
        app
    }

    /// Tells the app where one speaker is standing, in **feet** coordinates.
    fn stands_at(app: &mut App, entity_id: u64, feet: Vec3) {
        app.world_mut().resource_mut::<SnapshotBuffer>().accept(
            Snapshot {
                server_tick: 1,
                entities: vec![EntityState {
                    entity_id,
                    pos: [feet.x, feet.y, feet.z],
                    vel: [0.0; 3],
                    yaw: 0.0,
                }],
                ..Snapshot::default()
            },
            Instant::now(),
        );
    }

    /// The energy in each channel of what has been queued for the output callback.
    fn heard_sides(app: &App) -> (f32, f32) {
        let mixer = app.world().resource::<super::super::AudioMixer>();
        struct VecSink(Vec<f32>);
        impl crate::audio::mixer::Sink for VecSink {
            fn block(&mut self) -> &mut [f32] {
                &mut self.0
            }
        }
        let mut sink = VecSink(vec![0.0; FRAME_SAMPLES * QUEUED_FRAMES * 2]);
        mixer.0.render(&mut sink);
        let left = sink.0.iter().step_by(2).map(|s| s.abs()).sum();
        let right = sink.0.iter().skip(1).step_by(2).map(|s| s.abs()).sum();
        (left, right)
    }

    /// A world holding one all-air chunk at the origin, with the named voxels set.
    fn world_of(blocks: &[(IVec3, crate::world::BlockId)]) -> ChunkStore {
        let mut chunk = VoxelChunk::all_air(usize::from(CHUNK));
        for (voxel, block) in blocks {
            chunk.set(voxel.x as usize, voxel.y as usize, voxel.z as usize, *block);
        }
        let mut store = ChunkStore::default();
        store.insert(
            ChunkCoord {
                cx: 0,
                cy: 0,
                cz: 0,
            },
            chunk,
        );
        store
    }

    fn speak(app: &mut App, entity_id: u64) {
        for (sequence, frame) in frames().into_iter().enumerate().take(6) {
            say(app, entity_id, sequence as u32, frame);
        }
    }

    /// The one that fails with the channels swapped, and the mirror case is why it is not
    /// enough to assert that one side is louder.
    #[test]
    fn a_speaker_is_heard_from_the_side_they_are_standing_on() {
        let sides = |x: f32| {
            let mut app = placed_app(world_of(&[]));
            stands_at(&mut app, 7, Vec3::new(x, 4.0, 4.5));
            speak(&mut app, 7);
            app.update();
            heard_sides(&app)
        };

        let (left, right) = sides(10.5);
        assert!(
            right > left * 4.0,
            "a speaker to the east was heard {left} left and {right} right"
        );

        let (left, right) = sides(-1.5);
        assert!(
            left > right * 4.0,
            "a speaker to the west was heard {left} left and {right} right"
        );
    }

    /// A speaker no snapshot places is heard exactly as every speaker was before #854:
    /// centred and at full gain, which is what "unpositioned mono at bus gain" means.
    #[test]
    fn a_speaker_the_snapshot_has_not_placed_is_heard_unpositioned() {
        let mut app = placed_app(world_of(&[]));
        speak(&mut app, 7);
        app.update();

        let (left, right) = heard_sides(&app);
        assert!(left > 0.0, "an unplaced speaker was not heard at all");
        assert!(
            (left - right).abs() < 1e-3,
            "an unplaced speaker was panned: {left} against {right}"
        );

        // And once the world does place them, the same speaker moves off centre — so the
        // centring above is the absence of a position rather than a placement that failed.
        stands_at(&mut app, 7, Vec3::new(10.5, 4.0, 4.5));
        speak(&mut app, 7);
        app.update();
        let (left, right) = heard_sides(&app);
        assert!(right > left, "{left} against {right}");
    }

    /// **Nothing stays where it last was.** A frame with no camera cannot place anybody, and
    /// a source left at its last placement would keep a voice panned hard to one side for as
    /// long as that lasted. This is the one branch of the fallback that is reachable without
    /// releasing the speaker, so it is the one that holds the single-site `unwrap_or`.
    #[test]
    fn a_frame_that_can_place_nobody_leaves_nobody_where_they_were() {
        let mut app = placed_app(world_of(&[]));
        stands_at(&mut app, 7, Vec3::new(10.5, 4.0, 4.5));
        speak(&mut app, 7);
        app.update();
        let (left, right) = heard_sides(&app);
        assert!(right > left * 4.0, "{left} against {right}");

        // The camera goes; the speaker stays and keeps talking.
        let camera = app
            .world_mut()
            .query_filtered::<Entity, With<WorldCamera>>()
            .iter(app.world())
            .next()
            .expect("the fixture spawned one");
        app.world_mut().entity_mut(camera).despawn();
        speak(&mut app, 7);
        app.update();

        let (left, right) = heard_sides(&app);
        assert!(left > 0.0, "the speaker went silent rather than centring");
        assert!(
            (left - right).abs() < 1e-3,
            "a voice stayed panned with no camera to pan it against: {left} against {right}"
        );
    }

    /// **A voice comes from a speaker's head, not their boots**, which matters exactly where
    /// the two are on opposite sides of something. The wall here sits at head height and
    /// stops nothing at foot height: taking `EYE_HEIGHT` out of the speaker's position makes
    /// the rays run under it and answer open air.
    #[test]
    fn a_speaker_is_heard_from_their_eyes_and_not_their_feet() {
        // The listener's eye is at y 4.5 and the speaker's at 4.0 + EYE_HEIGHT; the ray
        // climbs into the voxel row at y 5 about halfway along, which is where this wall is.
        let wall: Vec<(IVec3, crate::world::BlockId)> = (3..=6)
            .map(|z| (IVec3::new(9, 5, z), palette::STONE))
            .collect();
        let mut app = placed_app(world_of(&wall));
        stands_at(&mut app, 7, Vec3::new(12.5, 4.0, 4.5));
        speak(&mut app, 7);
        app.update();

        let listening = app.world().resource::<Listening>();
        let speakers = listening.speakers.lock().expect("no test poisons it");
        assert!(
            speakers[&7].occlusion > 0.9,
            "a wall at head height was heard as {} of one",
            speakers[&7].occlusion
        );
    }

    /// Occlusion reaching the source at all, and reaching it through the smoothing rather
    /// than instead of it.
    #[test]
    fn a_wall_between_the_two_of_them_muffles_the_voice() {
        let occlusion_after = |wall: bool| {
            let blocks: Vec<(IVec3, crate::world::BlockId)> = if wall {
                (3..=6)
                    .flat_map(|y| (3..=6).map(move |z| (IVec3::new(8, y, z), palette::STONE)))
                    .collect()
            } else {
                Vec::new()
            };
            let mut app = placed_app(world_of(&blocks));
            stands_at(&mut app, 7, Vec3::new(12.5, 4.0, 4.5));
            speak(&mut app, 7);
            app.update();
            // 100 ms of output, which is twice the attack: enough for the ramp to arrive.
            let mixer = app.world().resource::<super::super::AudioMixer>();
            struct VecSink(Vec<f32>);
            impl crate::audio::mixer::Sink for VecSink {
                fn block(&mut self) -> &mut [f32] {
                    &mut self.0
                }
            }
            for _ in 0..10 {
                let mut sink = VecSink(vec![0.0; 480 * 2]);
                mixer.0.render(&mut sink);
            }
            let listening = app.world().resource::<Listening>();
            let speakers = listening.speakers.lock().expect("no test poisons it");
            speakers[&7]
                .source
                .as_ref()
                .expect("the speaker got a slot")
                .reached_occlusion()
        };

        assert!(
            occlusion_after(true) > 0.9,
            "a stone wall was not heard as one"
        );
        assert_eq!(occlusion_after(false), 0.0, "open air occluded something");
    }

    /// **The cadence, asserted by its consequence.** The rays are cast on the tick a speaker
    /// is first placed and then not again for a tenth of a second, so a wall that vanishes
    /// in between is still heard on the next frame. An implementation that cast per frame
    /// would answer zero immediately and fail this.
    #[test]
    fn the_rays_are_not_recast_on_every_frame() {
        let wall: Vec<(IVec3, crate::world::BlockId)> = (3..=6)
            .flat_map(|y| (3..=6).map(move |z| (IVec3::new(8, y, z), palette::STONE)))
            .collect();
        let mut app = placed_app(world_of(&wall));
        stands_at(&mut app, 7, Vec3::new(12.5, 4.0, 4.5));
        speak(&mut app, 7);
        app.update();

        let read_occlusion = |app: &App| {
            let listening = app.world().resource::<Listening>();
            let speakers = listening.speakers.lock().expect("no test poisons it");
            speakers[&7].occlusion
        };
        assert!(read_occlusion(&app) > 0.9, "the wall was never read");

        // The wall is gone, and the next frame is well inside the ray period.
        app.world_mut().insert_resource(world_of(&[]));
        app.update();
        assert!(
            read_occlusion(&app) > 0.9,
            "the rays were recast on the very next frame"
        );
    }

    /// Every slot taken, and a speaker still heard. The fallback is the shared source this
    /// module has always held, which is the same answer an unplaced speaker gets.
    #[test]
    fn a_speaker_the_mixer_had_no_slot_for_is_still_heard() {
        let mut app = placed_app(world_of(&[]));
        let held: Vec<_> = {
            let mixer = app.world().resource::<super::super::AudioMixer>();
            std::iter::from_fn(|| mixer.0.claim(Bus::Voice)).collect()
        };
        assert!(!held.is_empty(), "the fixture left no slots to take");

        stands_at(&mut app, 7, Vec3::new(10.5, 4.0, 4.5));
        speak(&mut app, 7);
        app.update();

        {
            let listening = app.world().resource::<Listening>();
            let speakers = listening.speakers.lock().expect("no test poisons it");
            assert!(
                speakers[&7].source.is_none(),
                "a slot was found where none was free"
            );
        }
        let (left, right) = heard_sides(&app);
        assert!(left > 0.0, "a speaker with no slot was silent");
        assert!(
            (left - right).abs() < 1e-3,
            "a speaker with no slot was somehow panned: {left} against {right}"
        );
    }

    /// The slot goes back when the speaker does, which is the whole reason the pool works
    /// for a session rather than for the first thirteen people to speak.
    #[test]
    fn a_released_speaker_gives_their_slot_back() {
        let mut app = placed_app(world_of(&[]));
        stands_at(&mut app, 7, Vec3::new(10.5, 4.0, 4.5));
        speak(&mut app, 7);
        app.update();

        let free_slots = |app: &App| {
            let mixer = app.world().resource::<super::super::AudioMixer>();
            let taken: Vec<_> = std::iter::from_fn(|| mixer.0.claim(Bus::Voice)).collect();
            taken.len()
        };
        let while_speaking = free_slots(&app);

        // A newer snapshot that no longer names them. The tick has to advance: `accept`
        // refuses one that is older than what it holds, and a refused snapshot would leave
        // the speaker present rather than gone.
        app.world_mut().resource_mut::<SnapshotBuffer>().accept(
            Snapshot {
                server_tick: 2,
                ..Snapshot::default()
            },
            Instant::now(),
        );
        app.update();
        {
            let listening = app.world().resource::<Listening>();
            let speakers = listening.speakers.lock().expect("no test poisons it");
            assert!(!speakers.contains_key(&7), "the speaker was not released");
        }
        // The callback has to run before a released slot can be handed out again.
        let mixer = Arc::clone(&app.world().resource::<super::super::AudioMixer>().0);
        struct VecSink(Vec<f32>);
        impl crate::audio::mixer::Sink for VecSink {
            fn block(&mut self) -> &mut [f32] {
                &mut self.0
            }
        }
        let mut sink = VecSink(vec![0.0; 8]);
        mixer.render(&mut sink);

        assert!(
            free_slots(&app) > while_speaking,
            "a released speaker's slot never came back: {while_speaking} then {}",
            free_slots(&app)
        );
    }

    /// A listener with no mixer slot: silence, and a client that keeps running.
    #[test]
    fn a_listener_with_no_source_is_silent_rather_than_broken() {
        let mut app = App::new();
        app.insert_resource(Listening::new(None))
            .init_resource::<Speaking>()
            .init_resource::<Voices>()
            .init_resource::<VoiceInbox>()
            .add_systems(
                Update,
                (
                    hear,
                    // Between the two on purpose: a speaker heard for the first time this
                    // tick is placed before a single sample of theirs is queued, so nothing
                    // of them is ever pushed into the shared source and then repeated from
                    // a source of their own.
                    place_the_speakers,
                    play,
                    release_speakers,
                    forget_stale_voices,
                )
                    .chain(),
            );
        let opus = frames();
        for (sequence, frame) in opus.iter().enumerate().take(6) {
            say(&mut app, 7, sequence as u32, frame.clone());
        }
        app.update();
    }
    /// **The pair, and the only place this property lives.** A speaker whose decoder has been
    /// released is still a speaker who was heard in the last second.
    ///
    /// This is the shape the whole stack has no other defence against, and it is why the test
    /// runs the real systems rather than `Speaking` on its own: in isolation both halves were
    /// correct — `recent` filtered at one second, `release_speakers` let a decoder go after
    /// half of one — and the assembly was wrong, because releasing the decoder also forgot the
    /// name. Every unit test passed. Found by review on #924, by reading this file against the
    /// HUD.
    ///
    /// The wait is real time because `Jitter::silent_since` reads the clock, and it is the one
    /// place in this suite that sleeps. 600 ms is past `RELEASE_AFTER` and well inside
    /// `SPEAKING_FOR`; the window half is asserted against an explicit instant rather than the
    /// clock, so only the release half depends on how long the sleep actually took.
    #[test]
    fn a_released_speaker_is_still_a_speaker_heard_in_the_last_second() {
        let mut app = listening_app();
        let opus = frames();
        for (sequence, frame) in opus.iter().enumerate().take(6) {
            say(&mut app, 7, sequence as u32, frame.clone());
        }
        app.update();
        let played = Instant::now();
        assert_eq!(
            app.world().resource::<Speaking>().recent(played),
            vec![7],
            "the speaker was never recorded as heard"
        );

        // Nothing more arrives. The decoder is let go; the name is not.
        std::thread::sleep(RELEASE_AFTER + Duration::from_millis(100));
        app.update();

        {
            let listening = app.world().resource::<Listening>();
            let speakers = listening.speakers.lock().expect("no test poisons it");
            assert!(
                !speakers.contains_key(&7),
                "the decoder was held past {RELEASE_AFTER:?} of silence"
            );
        }
        assert_eq!(
            app.world()
                .resource::<Speaking>()
                .recent(played + Duration::from_millis(700)),
            vec![7],
            "releasing the decoder also forgot the name, so the HUD's second half of a \
             second is unreachable"
        );

        // And a second after it was played, it is gone — by age, which is the only thing that
        // removes one.
        app.update();
        assert!(
            app.world()
                .resource::<Speaking>()
                .recent(played + SPEAKING_FOR)
                .is_empty(),
            "a name outlived the window it is documented to have"
        );
    }

    /// A name stops being current a second after it was last heard, so the HUD in part 8 has
    /// something that goes away on its own.
    #[test]
    fn a_speaker_stops_being_recent_after_a_second() {
        let mut speaking = Speaking::default();
        let now = Instant::now();
        speaking.heard(7, now);
        assert_eq!(speaking.recent(now), vec![7]);
        assert_eq!(speaking.recent(now + Duration::from_millis(999)), vec![7]);
        assert!(speaking.recent(now + SPEAKING_FOR).is_empty());

        // And the order is the order they were first heard, so a name does not move in a
        // list while somebody is reading it.
        speaking.heard(9, now);
        speaking.heard(7, now + Duration::from_millis(10));
        assert_eq!(speaking.recent(now + Duration::from_millis(20)), vec![7, 9]);
        // **Age is the only thing that drops an entry, and it drops them one at a time.** At
        // exactly `SPEAKING_FOR` after 9 was heard, 9 goes and 7 — heard ten milliseconds
        // later — stays. Nothing else removes an entry: not the speaker being released, not
        // their entity leaving the snapshot.
        speaking.forget_stale(now + SPEAKING_FOR);
        assert_eq!(speaking.recent(now + Duration::from_millis(20)), vec![7]);
        speaking.forget_stale(now + SPEAKING_FOR + Duration::from_millis(10));
        assert!(speaking.recent(now + Duration::from_millis(20)).is_empty());
    }
}
