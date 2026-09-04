//! Proximity voice, inbound: from the wire to the speakers.
//!
//! **Receiving a frame *is* the audibility decision, already made.** The server owns every
//! position and every party roster and it sent this frame, so nothing here asks whether the
//! speaker is close enough or in the right group. A client that filtered on its own idea of
//! either would be second-guessing the only authority there is —
//! `schemas/player.fbs` states that where `VoiceHeard` is declared, and this module is the
//! consumer it is stated for.
//!
//! ```text
//!   net::VoiceInbox ─▶ Jitter (one per speaker) ─▶ VoiceDecoder ─▶ mixed ─▶ Voice bus
//!                            │                          │
//!                     reorder, 60–200 ms         concealment, or the
//!                     of slack, gaps kept        copy inside the next
//! ```
//!
//! ## What a jitter buffer is for, and what it is not for
//!
//! Frames arrive early, late, out of order, twice, or not at all, and a decoder needs exactly
//! one every 20 ms. So each speaker gets a small buffer that holds a target amount of slack
//! before it starts playing and then hands out one frame per slot: the frame that belongs to
//! that slot if it has arrived, a repair if it has not. The slack is latency the listener
//! pays for continuity, which is why it starts at the smallest useful value and only grows
//! when the network has demonstrably needed it.
//!
//! **It is not a queue that drains.** A buffer that played everything it had as fast as it
//! could would be a buffer with no slack at all a moment later.
//!
//! ## Everything is mixed into one source
//!
//! `audio/mixer.rs` has four source slots for the whole client, and a speaker who claimed one
//! would keep it for the life of the mixer. So the speakers are summed here — mono, at bus
//! gain — and pushed into one source on `Bus::Voice`. Per-speaker attenuation and panning is
//! #854, and it belongs to a source per speaker, which is a change to the mixer rather than a
//! change here.
//!
//! ## Nothing is written down
//!
//! A voice frame is personal data. Nothing in this module logs a payload, a speaker, or a
//! count of either — a count of how often somebody spoke is a fact about a person.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use bevy::prelude::*;

use super::codec::{Missing, VoiceDecoder};
use super::dsp::FRAME_SAMPLES;
use super::mixer::{Bus, SOURCE_CAPACITY, SourceHandle};
use crate::net::{MAX_OPUS_BYTES, VoiceInbox};
use crate::player::SnapshotBuffer;

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
/// **A bound on a length the peer chooses**, like every other one at this boundary: the
/// server relays what a speaker sends, and a speaker that sent faster than real time would
/// otherwise grow this map without limit. Twice the largest target is slack for a burst and
/// nothing like a backlog.
const MAX_HELD_FRAMES: usize = MAX_TARGET_FRAMES * 2;

/// How long a speaker may send nothing before they are released.
///
/// The acceptance criterion's number. It is deliberately much longer than a gap the buffer
/// conceals: what this releases is somebody who has stopped, not somebody mid-sentence.
const RELEASE_AFTER: Duration = Duration::from_millis(500);

/// How much audio is kept queued for the output callback, in frames.
///
/// The source ring is a quarter of a second; this is the depth the pipeline tops it back up
/// to each tick. Four frames is 80 ms — more than two ordinary 60 Hz ticks, so a frame that
/// runs long does not underrun, and far short of the ring, so nothing here is the thing that
/// adds latency.
const QUEUED_FRAMES: usize = 4;

/// One speaker's frames, in order, with the slack a listener hears as continuity.
///
/// Keyed by the speaker's own sequence, which is presentation and never a clock: it is what
/// makes reordering fixable and a gap knowable, and nothing branches on its value.
#[derive(Debug)]
struct Jitter {
    /// Frames waiting, by sequence. A `BTreeMap` because what is asked for is "the lowest",
    /// and because a duplicate arrival must replace rather than accumulate.
    frames: BTreeMap<u32, Vec<u8>>,
    /// The sequence the next slot wants. `None` until the first frame arrives.
    next: Option<u32>,
    /// How many frames of slack this speaker's buffer is holding to.
    target: usize,
    /// Whether the buffer has filled once and started playing.
    playing: bool,
    /// When a frame from this speaker last arrived.
    arrived: Instant,
}

/// What a slot asks the decoder for.
#[derive(Debug, PartialEq, Eq)]
enum Slot {
    /// The frame that belongs to this slot, which arrived.
    Frame(Vec<u8>),
    /// It did not, and the one after it has — so the redundant copy inside that one is the
    /// real frame at a lower bitrate.
    Recover(Vec<u8>),
    /// Neither. The decoder extrapolates from what it played.
    Conceal,
    /// The buffer has not filled yet, or has nothing at all. Silence, and not a repair:
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
    /// **A frame for a slot already played is dropped, and it grows the target.** That is the
    /// whole of the adaptation: arriving late is the only evidence this buffer has that its
    /// slack is too small, and it is direct evidence. Nothing shrinks the target inside one
    /// speaker's turn — a buffer that shrank on every frame that arrived on time would spend
    /// the conversation oscillating.
    fn push(&mut self, sequence: u32, opus: Vec<u8>, now: Instant) {
        self.arrived = now;
        if self.playing
            && let Some(next) = self.next
            && sequence < next
        {
            self.target = (self.target + 1).min(MAX_TARGET_FRAMES);
            return;
        }
        // **A frame that is already held costs nothing to accept, so it evicts nothing.** The
        // review on #923 found the eviction below running for a duplicate: with the buffer
        // full, a second copy of frame 5 dropped frame 19 and then replaced itself, so
        // *receiving* a frame manufactured a gap — the opposite of what a jitter buffer is
        // for. A duplicate is the ordinary shape of a retransmission, not an admission.
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
            // **The lowest held, not the first that arrived.** Which frame turned up first
            // is a fact about the network; where the run starts is a fact about the speaker,
            // and starting from the wrong one plays the whole conversation out of order
            // while every individual frame is intact.
            self.next = self.frames.keys().next().copied();
        }
        let Some(next) = self.next else {
            return Slot::Nothing;
        };
        if self.frames.is_empty() {
            // Nothing at all rather than a gap in a stream: a speaker who has stopped is
            // silence, and concealing it would be inventing audio nobody sent. The run ends
            // here, so the next one starts from whatever is lowest then.
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
        // **Only the frame immediately after this slot can repair it, and the review on #923
        // is where that was got wrong.** Opus's in-band forward error correction puts a
        // lower-bitrate copy of frame *N* inside packet *N + 1* and nowhere else. Offering the
        // lowest frame held — which the first version did — hands the decoder packet 5 to
        // recover slot 3 from, and what comes out is frame 4, played in slot 3 and then again
        // in slot 4: the stream duplicated and a frame out of step, for a gap concealment
        // would have covered honestly.
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
/// **A decoder each, and that is not an optimisation.** An Opus decoder carries the state its
/// concealment extrapolates from, so two speakers sharing one would each be concealed from
/// the other's audio.
#[derive(Debug)]
struct Speaker {
    jitter: Jitter,
    /// Whether a snapshot has ever named this speaker.
    ///
    /// **A speaker the world has not confirmed is released by the silence timer and not by
    /// absence.** Voice and snapshots arrive on their own schedules, so somebody who starts
    /// talking after the newest snapshot and before the next one is genuinely missing from it
    /// — and releasing them there would drop the first part of every voice that comes into
    /// range, in the same frame the buffer was created. Found by review on #923.
    seen: bool,
    /// `None` when libopus would not open one. That costs this speaker their voice and
    /// nothing else — never a panic, and never a retry per frame, which is why the entry is
    /// made either way.
    decoder: Option<VoiceDecoder>,
}

/// Everything the playback half carries between ticks.
#[derive(Resource)]
struct Listening {
    /// One per speaker being heard. Removed by [`release_speakers`] and nothing else.
    ///
    /// **The `Mutex` is a type obligation and not synchronisation**, exactly as `NetLink`'s
    /// is: libopus's decoder holds a raw pointer, so `audiopus` declares it `Send` and not
    /// `Sync`, and a Bevy resource must be `Sync`. The one accessor takes `ResMut` and reaches
    /// the contents with `get_mut`, so no lock is ever taken.
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
            .add_systems(Update, (hear, play, release_speakers).chain());
    }
}

/// Puts every relayed frame into its speaker's buffer.
fn hear(mut inbox: ResMut<VoiceInbox>, mut listening: ResMut<Listening>) {
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
        // Refused rather than trusted: the decoder allocates nothing from this, but the
        // buffer does, and every length a peer chooses gets a bound here. The decode
        // boundary already enforces it — this is the second reader of the same rule.
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
            }
        });
        speaker.jitter.push(frame.sequence, frame.opus, now);
    }
}

/// Decodes what is due and mixes it into the voice source.
fn play(mut listening: ResMut<Listening>) {
    let listening = &mut *listening;
    let Some(source) = listening.source.as_ref() else {
        return;
    };
    let speakers = match listening.speakers.get_mut() {
        Ok(speakers) => speakers,
        Err(poisoned) => poisoned.into_inner(),
    };
    if speakers.is_empty() {
        return;
    }

    // Topped up to a depth rather than drained: a buffer that played everything it had would
    // have no slack a moment later. `SOURCE_CAPACITY - free` is what is still queued.
    let queued = SOURCE_CAPACITY.saturating_sub(source.free());
    let wanted = (QUEUED_FRAMES * FRAME_SAMPLES).saturating_sub(queued) / FRAME_SAMPLES;
    for _ in 0..wanted {
        listening.mixed.fill(0.0);
        let mut anybody = false;
        for speaker in speakers.values_mut() {
            let slot = speaker.jitter.slot();
            let Some(decoder) = speaker.decoder.as_mut() else {
                continue;
            };
            let decoded = match &slot {
                Slot::Nothing => continue,
                Slot::Frame(frame) => decoder.decode(frame, &mut listening.decoded),
                Slot::Recover(next) => {
                    decoder.repair(Missing::FromTheNext, Some(next), &mut listening.decoded)
                }
                Slot::Conceal => decoder.repair(Missing::Conceal, None, &mut listening.decoded),
            };
            // A frame that will not decode is a gap, never a log line: the message would name
            // a speaker, and how often somebody spoke is a fact about a person.
            if decoded.is_err() {
                continue;
            }
            anybody = true;
            for (out, sample) in listening.mixed.iter_mut().zip(listening.decoded.iter()) {
                *out += *sample;
            }
        }
        if !anybody {
            break;
        }
        // Clamped here rather than in the mixer's own sum, so a crowd is quiet distortion on
        // this bus rather than a master stage that has to absorb it.
        for sample in listening.mixed.iter_mut() {
            *sample = sample.clamp(-1.0, 1.0);
        }
        source.push(&listening.mixed);
    }
}

/// Lets go of speakers who have stopped, and of speakers who are no longer there.
///
/// **Two releases, and they are different questions.** A speaker whose frames stop has stopped
/// talking; a speaker whose entity has left the snapshot has gone, and the server has stopped
/// relaying them at the same moment — so waiting out the silence timer for somebody
/// demonstrably absent would hold a decoder and a name for half a second of nothing.
fn release_speakers(mut listening: ResMut<Listening>, snapshots: Option<Res<SnapshotBuffer>>) {
    let now = Instant::now();
    let listening = &mut *listening;
    let speakers = match listening.speakers.get_mut() {
        Ok(speakers) => speakers,
        Err(poisoned) => poisoned.into_inner(),
    };
    speakers.retain(|entity_id, speaker| {
        // **Two things have to be true before absence means anything**, and the review on
        // #923 found only the first of them. A snapshot has to exist at all — "nothing to
        // read" is never "not present", or a late snapshot would silence a conversation. And
        // this speaker has to have *been* in one: voice and snapshots arrive on their own
        // schedules, so somebody who starts talking after the newest snapshot and before the
        // next one is genuinely missing from it, and releasing them there drops the first part
        // of every voice that comes into range in the same frame its buffer was created. Until
        // the world has confirmed them once, the silence timer is what lets them go.
        let present = snapshots
            .as_deref()
            .is_some_and(|snapshots| snapshots.holds_entity(*entity_id));
        speaker.seen |= present;
        let gone = speaker.seen && !present;
        !gone && !speaker.jitter.silent_since(now)
    });
}

/// **No test here opens a device or a socket.** The wire side is `VoiceInbox::push_for_test`,
/// the mixer side is a `Mixer` built in a `Vec`, and the codec is real libopus in this
/// process. What is under test is the buffer's ordering and the two releases.
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

    /// **The buffer holds its slack before it plays anything**, which is the whole of what it
    /// is for: a buffer that played the first frame it received would have no slack a moment
    /// later and would conceal every reordering after it.
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

    /// Frames that arrive out of order are played in order, and one that arrives twice is
    /// played once.
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

    /// **A gap is a gap.** The frame after a missing one is what carries its redundant copy,
    /// so a slot with the next frame present asks for the recovery and a slot with nothing
    /// after it asks for concealment.
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

    /// **A duplicate costs nothing to accept, so it evicts nothing.** With the buffer full, a
    /// second copy of a frame already held used to drop the highest unique one and then
    /// replace itself — so *receiving* a frame manufactured a gap, which is the opposite of
    /// what a jitter buffer is for. Found by review on #923.
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

    /// **The buffer grows when the network makes it, and only then.** A frame that arrives
    /// after its slot has been played is the one piece of direct evidence that the slack was
    /// too small, and nothing else moves the target.
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

    /// A speaker that sends faster than real time cannot grow the buffer without limit — the
    /// bound every length a peer chooses gets at this boundary.
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

    /// **The whole inbound path, through real libopus and a real mixer.** Frames in one end,
    /// audio out of the other, on the `Voice` bus at its gain.
    fn listening_app() -> App {
        let mixer = Arc::new(Mixer::new());
        mixer.set_format(48_000, 1);
        mixer.set_gain(Bus::Voice, 1.0);
        mixer.set_gain(Bus::Master, 1.0);
        let source = mixer.claim(Bus::Voice).expect("a free slot");
        let mut app = App::new();
        app.insert_resource(super::super::AudioMixer(Arc::clone(&mixer)))
            .insert_resource(Listening::new(Some(source)))
            .init_resource::<VoiceInbox>()
            .add_systems(Update, (hear, play, release_speakers).chain());
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
    /// decode boundary enforces the same rule; this is its second reader, because the buffer
    /// allocates from a length the peer chose.
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

    /// **A voice that arrives before the world does is not released.** Voice and snapshots
    /// arrive on their own schedules, so somebody who starts talking after the newest snapshot
    /// and before the next one is genuinely missing from it — and releasing them there drops
    /// the first part of every voice that comes into range, in the same frame its buffer was
    /// created. Absence only means anything once a snapshot has named them at least once.
    /// Found by review on #923.
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

    /// What a listener with no mixer slot gets: silence, and a client that keeps running.
    #[test]
    fn a_listener_with_no_source_is_silent_rather_than_broken() {
        let mut app = App::new();
        app.insert_resource(Listening::new(None))
            .init_resource::<VoiceInbox>()
            .add_systems(Update, (hear, play, release_speakers).chain());
        let opus = frames();
        for (sequence, frame) in opus.iter().enumerate().take(6) {
            say(&mut app, 7, sequence as u32, frame.clone());
        }
        app.update();
    }
}
