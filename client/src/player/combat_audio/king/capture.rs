//! Offline king review exports through the guardian's recorder: real synthesis, the
//! snapshot-applied rig, encounter reconciliation and the production mixer at 20 Hz.
//! Producing and measuring a WAV is not a claim that anybody listened to it.
use super::super::guardian::capture::{Recording, catalogue, review_directory, stone_wall};
use super::tests::{snapshot, timeline};
use super::*;
use crate::{
    net::{
        EncounterMoveKind as Move, EncounterTimeline, EncounterTimelineInbox, MobAction as Action,
        MoveEnd, MovePhase as Phase, Session,
    },
    player::SnapshotBuffer,
};
use std::{
    path::Path,
    time::{Duration, Instant},
};

const TOLLS: [Option<(u8, u8)>; 3] = [Some((1, 3)), Some((2, 3)), Some((3, 3))];

fn king_recording(distance: f32, mute: bool) -> Recording {
    let mut recording = Recording::new(distance, mute);
    recording
        .app
        .world_mut()
        .resource_mut::<Session>()
        .0
        .tick_rate = 20;
    recording
}

/// One authoritative 20 Hz tick, mixed as three 60 Hz frames of the same snapshot.
fn frame(
    recording: &mut Recording,
    tick: u32,
    action: Action,
    timeline: Option<EncounterTimeline>,
) {
    let world = recording.app.world_mut();
    world.resource_mut::<SnapshotBuffer>().accept(
        snapshot(tick, action),
        Instant::now() - Duration::from_millis(100),
    );
    if let Some(timeline) = timeline {
        world
            .resource_mut::<EncounterTimelineInbox>()
            .push(timeline);
    }
    for _ in 0..3 {
        recording.mix(tick);
    }
}

/// Every phase of each listed blow at catalogue durations, then an empty timeline.
fn blows(recording: &mut Recording, tick: &mut u32, kind: Move, combos: &[Option<(u8, u8)>]) {
    for (index, combo) in combos.iter().enumerate() {
        for phase in [Phase::Telegraph, Phase::Release, Phase::Recovery] {
            let mut state = timeline(kind, phase, *combo, *tick);
            state.moves[0].move_instance_id = 21 + index as u64;
            let action = if phase == Phase::Recovery {
                Action::Recovery
            } else {
                Action::Windup
            };
            for elapsed in 0..state.moves[0].phase_ticks {
                frame(
                    recording,
                    *tick,
                    action,
                    (elapsed == 0).then(|| state.clone()),
                );
                *tick += 1;
            }
        }
    }
    let mut empty = timeline(kind, Phase::Recovery, None, *tick);
    empty.moves.clear();
    for elapsed in 0..20 {
        frame(
            recording,
            *tick,
            Action::Idle,
            (elapsed == 0).then(|| empty.clone()),
        );
        *tick += 1;
    }
}

fn physical(directory: &Path, name: &str, kind: Move, distance: f32, mute: bool, wall: bool) {
    let mut recording = king_recording(distance, mute);
    if wall {
        stone_wall(recording.app.world_mut());
    }
    let mut tick = 100;
    frame(&mut recording, tick, Action::Idle, None);
    tick += 1;
    let combos: &[_] = if kind == Move::ThreeTolls {
        &TOLLS
    } else {
        &[None]
    };
    blows(&mut recording, &mut tick, kind, combos);
    recording.save(directory, name);
}

#[test]
#[ignore = "writes an offline production-mixer WAV for manual listening; no audio device"]
fn export_king_audio_catalogue() {
    let cues = sounds::CUES.map(|cue| (format!("{cue:?}"), cue.seconds(), cue.describe()));
    catalogue(&review_directory(), "king-catalogue", &cues);
}

#[test]
#[ignore = "writes actual ECS/mixer sequence WAVs and timestamps for manual listening"]
fn export_king_audio_sequences() {
    let directory = review_directory();
    let sentence = Move::KingsSentence;
    let tolls = Move::ThreeTolls;
    physical(&directory, "sentence", sentence, 3.0, false, false);
    physical(
        &directory,
        "sentence-stone-wall",
        sentence,
        3.0,
        false,
        true,
    );
    physical(&directory, "three-tolls", tolls, 3.0, false, false);
    physical(&directory, "tolls-13-blocks", tolls, 13.0, false, false);
    physical(&directory, "tolls-25-blocks", tolls, 25.0, false, false);
    physical(&directory, "tolls-muted", tolls, 3.0, true, false);

    // An observed notice, the stage 2 → 3 mask fall, then the corpse.
    let mut recording = king_recording(3.0, false);
    let mut stage = timeline(sentence, Phase::Recovery, None, 100);
    stage.moves.clear();
    for tick in 100..220 {
        let action = match tick {
            100 => Action::Idle,
            200.. => Action::Corpse,
            _ => Action::Chase,
        };
        let announced = match tick {
            105 => Some(2),
            140 => Some(3),
            _ => None,
        }
        .map(|phase| {
            let mut state = stage.clone();
            state.phase = phase;
            state
        });
        frame(&mut recording, tick, action, announced);
    }
    recording.save(&directory, "notice-mask-death");

    // A Sentence cancelled before its clang; a second toll first seen too late for its
    // bell but in time for its sweep; a new Sentence replacing it during recovery.
    let mut recording = king_recording(3.0, false);
    let toll = |phase, start| {
        let mut state = timeline(tolls, phase, Some((2, 3)), start);
        state.moves[0].move_instance_id = 12;
        state
    };
    for tick in 100..200 {
        let state = match tick {
            100 => Some(timeline(sentence, Phase::Telegraph, None, 100)),
            110 => {
                let mut state = timeline(sentence, Phase::Telegraph, None, 100);
                state.moves[0].ended = Some(MoveEnd::Cancelled);
                Some(state)
            }
            130 => Some(toll(Phase::Telegraph, 122)),
            140 => Some(toll(Phase::Release, 140)),
            144 => Some(toll(Phase::Recovery, 144)),
            146 => {
                let mut state = timeline(sentence, Phase::Telegraph, None, 146);
                state.moves[0].move_instance_id = 13;
                Some(state)
            }
            _ => None,
        };
        frame(&mut recording, tick, Action::Windup, state);
    }
    recording.save(&directory, "cancel-late-replaced");
}
