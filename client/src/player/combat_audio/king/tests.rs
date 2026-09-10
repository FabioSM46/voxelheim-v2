use super::super::{
    CombatAudio,
    guardian::{
        MAX_BOSS_SOURCES, MAX_PER_BOSS,
        tests::{Buffer, fixture},
    },
};
use super::*;
use crate::{
    audio::{Bus, Mixer},
    net::{
        EncounterMoveKind as Move, EncounterTimeline, EncounterTimelineInbox, HazardShape,
        HazardVolume, MobAction as Action, MoveEnd, MovePhase as Phase, Session, Snapshot,
    },
    player::{
        SnapshotBuffer, WorldCamera,
        encounters::{self, EncounterPresentation},
    },
};
use bevy::prelude::*;
use std::{sync::Arc, time::Instant};

/// The server catalogue at its default 20 Hz (`encounter_moves.go`, `encounter_combo.go`).
/// Production reads the announced durations; these only place markers realistically.
pub(in super::super) fn timeline(
    kind: Move,
    phase: Phase,
    combo: Option<(u8, u8)>,
    start: u32,
) -> EncounterTimeline {
    let mut state = encounters::tests::timeline();
    state.boss = MobKind::DraugrKing;
    state.phase = 1;
    let sentence = kind == Move::KingsSentence;
    let one = &mut state.moves[0];
    one.kind = kind;
    one.phase = phase;
    one.phase_started_tick = start;
    one.combo = combo;
    one.phase_ticks = match (sentence, phase) {
        (true, Phase::Telegraph) => 24,
        (true, Phase::Release) => 5,
        (true, _) => 36,
        (false, Phase::Telegraph) => 18,
        (false, Phase::Release) => 4,
        _ if combo.is_some_and(|(step, total)| step < total) => 8,
        _ => 44,
    };
    let cone = !sentence && combo.is_some_and(|(step, _)| step < 3);
    one.hazards = vec![HazardVolume {
        shape: if cone {
            HazardShape::Cone { half_angle: 0.95 }
        } else {
            HazardShape::Line {
                half_width: if sentence { 1.1 } else { 0.65 },
            }
        },
        origin: [0.0, 1.4, 0.0],
        direction: [0.0, 0.0, -1.0],
        radius: if sentence { 5.0 } else { 3.8 },
        height: 3.0,
    }];
    state
}

pub(in super::super) fn snapshot(tick: u32, action: Action) -> Snapshot {
    let mut snapshot = encounters::tests::snapshot(tick);
    snapshot.mobs[0].kind = MobKind::DraugrKing;
    snapshot.mobs[0].action = action;
    snapshot
}

fn king_fixture() -> (App, Arc<Mixer>) {
    let (mut app, mixer) = fixture(8000);
    app.world_mut().resource_mut::<Session>().0.tick_rate = 20;
    (app, mixer)
}

/// Applies one authoritative frame and returns the king cues granted on it.
fn advance(
    app: &mut App,
    tick: u32,
    action: Action,
    timeline: Option<EncounterTimeline>,
) -> Vec<Cue> {
    app.world_mut()
        .resource_mut::<SnapshotBuffer>()
        .accept(snapshot(tick, action), Instant::now());
    if let Some(timeline) = timeline {
        app.world_mut()
            .resource_mut::<EncounterTimelineInbox>()
            .push(timeline);
    }
    app.update();
    let started = &app.world().resource::<CombatAudio>().started;
    started
        .iter()
        .map(|(_, cue, _)| match cue {
            CatalogueCue::King(cue) => *cue,
            other => panic!("a non-king cue {other:?} joined the king's gesture"),
        })
        .collect()
}

fn energy(app: &mut App, mixer: &Mixer, frames: usize) -> [f32; 2] {
    let mut energy = [0.0; 2];
    for _ in 0..frames {
        app.update();
        let mut buffer = Buffer(vec![0.0; 160]);
        mixer.render(&mut buffer);
        for pair in buffer.0.chunks_exact(2) {
            energy[0] += pair[0] * pair[0];
            energy[1] += pair[1] * pair[1];
        }
    }
    energy
}

#[test]
fn every_blade_marker_plays_once_on_its_announced_tick_and_windup_adds_no_bark() {
    use Cue::*;
    let toll = |step| Some((step, 3));
    let cases = [
        (
            Move::KingsSentence,
            None,
            Phase::Telegraph,
            0,
            vec![SentenceRaise],
        ),
        (
            Move::KingsSentence,
            None,
            Phase::Telegraph,
            18,
            vec![SentenceClang],
        ),
        (
            Move::KingsSentence,
            None,
            Phase::Release,
            0,
            vec![SentenceCut],
        ),
        (
            Move::KingsSentence,
            None,
            Phase::Release,
            4,
            vec![BladeBite],
        ),
        (Move::KingsSentence, None, Phase::Recovery, 0, vec![]),
        (
            Move::KingsSentence,
            None,
            Phase::Recovery,
            19,
            vec![BladeFree],
        ),
        (
            Move::ThreeTolls,
            toll(1),
            Phase::Telegraph,
            0,
            vec![TollFirst],
        ),
        (
            Move::ThreeTolls,
            toll(2),
            Phase::Telegraph,
            0,
            vec![TollSecond],
        ),
        (
            Move::ThreeTolls,
            toll(3),
            Phase::Telegraph,
            0,
            vec![TollThird],
        ),
        (
            Move::ThreeTolls,
            toll(1),
            Phase::Release,
            0,
            vec![SweepLeft],
        ),
        (
            Move::ThreeTolls,
            toll(2),
            Phase::Release,
            0,
            vec![SweepRight],
        ),
        (Move::ThreeTolls, toll(3), Phase::Release, 0, vec![Thrust]),
        (Move::ThreeTolls, toll(2), Phase::Recovery, 0, vec![]),
        (
            Move::ThreeTolls,
            toll(3),
            Phase::Recovery,
            0,
            vec![Recovery],
        ),
    ];
    for (kind, combo, phase, elapsed, expected) in cases {
        let (mut app, mixer) = king_fixture();
        let state = timeline(kind, phase, combo, 100);
        let label = format!("{kind:?} {combo:?} {phase:?} +{elapsed}");
        assert_eq!(
            advance(&mut app, 100 + elapsed, Action::Windup, Some(state)),
            expected,
            "{label}"
        );
        assert_eq!(
            advance(&mut app, 100 + elapsed, Action::Windup, None),
            [],
            "held snapshot replayed {label}"
        );
        let heard = energy(&mut app, &mixer, 20).iter().sum::<f32>();
        assert_eq!(heard > 0.05, !expected.is_empty(), "{label}: {heard}");
    }
    let (mut app, mixer) = king_fixture();
    assert_eq!(advance(&mut app, 100, Action::Windup, None), []);
    assert_eq!(
        energy(&mut app, &mixer, 20),
        [0.0; 2],
        "no timeline means no named blade move"
    );
}

#[test]
fn delayed_snapshots_skip_missed_markers_and_keep_future_ones_across_wrap() {
    for (late, expected) in [(2, vec![Cue::SentenceRaise]), (3, vec![])] {
        let (mut app, _) = king_fixture();
        let state = timeline(Move::KingsSentence, Phase::Telegraph, None, 100);
        assert_eq!(
            advance(&mut app, 100 + late, Action::Windup, Some(state)),
            expected,
            "at 20 Hz a marker stays fresh for two ticks"
        );
        assert_eq!(advance(&mut app, 117, Action::Windup, None), []);
        assert_eq!(
            advance(&mut app, 118, Action::Windup, None),
            [Cue::SentenceClang]
        );
    }
    let (mut app, _) = king_fixture();
    let start = u32::MAX - 1;
    let release = timeline(Move::KingsSentence, Phase::Release, None, start);
    assert_eq!(
        advance(&mut app, start, Action::Windup, Some(release)),
        [Cue::SentenceCut]
    );
    assert_eq!(
        advance(&mut app, start.wrapping_add(4), Action::Windup, None),
        [Cue::BladeBite]
    );
    let recovery = timeline(
        Move::KingsSentence,
        Phase::Recovery,
        None,
        start.wrapping_add(5),
    );
    assert_eq!(
        advance(
            &mut app,
            start.wrapping_add(5),
            Action::Recovery,
            Some(recovery)
        ),
        []
    );
    assert_eq!(
        app.world().resource::<CombatAudio>().playing.len(),
        2,
        "cut and bite ring on into their own instance's recovery"
    );
    let mut next = timeline(Move::ThreeTolls, Phase::Telegraph, Some((1, 3)), 7);
    next.moves[0].move_instance_id = 12;
    assert_eq!(
        advance(&mut app, 7, Action::Windup, Some(next)),
        [Cue::TollFirst]
    );
    assert_eq!(
        app.world().resource::<CombatAudio>().playing.len(),
        1,
        "a new instance ends the old tails"
    );
}

#[test]
fn replacement_cancellation_and_death_silence_owned_rings_and_voice_cues_are_once_only() {
    let (mut app, mixer) = king_fixture();
    let first = timeline(Move::ThreeTolls, Phase::Release, Some((1, 3)), 100);
    assert_eq!(
        advance(&mut app, 100, Action::Windup, Some(first)),
        [Cue::SweepLeft]
    );
    let mut second = timeline(Move::ThreeTolls, Phase::Telegraph, Some((2, 3)), 101);
    second.moves[0].move_instance_id = 12;
    assert_eq!(
        advance(&mut app, 101, Action::Windup, Some(second.clone())),
        [Cue::TollSecond]
    );
    assert_eq!(app.world().resource::<CombatAudio>().playing.len(), 1);
    second.moves[0].ended = Some(MoveEnd::Cancelled);
    assert_eq!(advance(&mut app, 102, Action::Idle, Some(second)), []);
    assert_eq!(energy(&mut app, &mixer, 5), [0.0; 2]);

    let (mut app, _) = king_fixture();
    assert_eq!(
        advance(&mut app, 100, Action::Chase, None),
        [],
        "first sight is not a notice"
    );
    assert_eq!(advance(&mut app, 101, Action::Idle, None), []);
    assert_eq!(advance(&mut app, 102, Action::Chase, None), [Cue::Notice]);
    let mut stage = timeline(Move::KingsSentence, Phase::Recovery, None, 102);
    stage.moves.clear();
    stage.phase = 2;
    assert_eq!(
        advance(&mut app, 103, Action::Chase, Some(stage.clone())),
        []
    );
    stage.phase = 3;
    assert_eq!(
        advance(&mut app, 104, Action::Chase, Some(stage.clone())),
        [Cue::MaskFall]
    );
    assert_eq!(
        advance(&mut app, 105, Action::Chase, Some(stage.clone())),
        []
    );
    assert_eq!(
        advance(&mut app, 106, Action::Corpse, None),
        [Cue::Death],
        "the fixture's zero health is never a death signal; the action is"
    );
    assert_eq!(advance(&mut app, 107, Action::Corpse, None), []);

    let (mut app, _) = king_fixture();
    assert_eq!(
        advance(&mut app, 100, Action::Chase, Some(stage)),
        [],
        "a body first seen in the final stage replays no fall"
    );
    let (mut app, _) = king_fixture();
    assert_eq!(advance(&mut app, 100, Action::Corpse, None), []);
}

#[test]
fn blade_cues_pan_to_their_side_fade_with_distance_and_share_the_boss_caps() {
    let heard = |step, distance: f32| {
        let (mut app, mixer) = king_fixture();
        let world = app.world_mut();
        for mut eye in world
            .query_filtered::<&mut Transform, With<WorldCamera>>()
            .iter_mut(world)
        {
            eye.translation.z = distance;
        }
        let state = timeline(Move::ThreeTolls, Phase::Release, Some((step, 3)), 100);
        advance(&mut app, 100, Action::Windup, Some(state));
        energy(&mut app, &mixer, 30)
    };
    let left = heard(1, 3.0);
    let right = heard(2, 3.0);
    assert!(left[0] > left[1] * 1.1, "first toll {left:?}");
    assert!(right[1] > right[0] * 1.1, "second toll {right:?}");
    let far = heard(1, 25.0);
    assert!(far[0] + far[1] < (left[0] + left[1]) * 0.1, "{far:?}");
    assert_eq!(heard(1, 40.0), [0.0; 2], "beyond the combat range");

    let (mut app, mixer) = king_fixture();
    let mut frame = snapshot(100, Action::Windup);
    let king = frame.mobs[0];
    frame.mobs.clear();
    for id in 9..13 {
        let mut mob = king;
        mob.entity_id = id;
        frame.mobs.push(mob);
        let mut state = timeline(Move::KingsSentence, Phase::Release, None, 100);
        state.boss_entity_id = id;
        state.encounter_id = id;
        // A one-tick release places the cut and the bite on the only announced tick.
        state.moves[0].phase_ticks = 1;
        app.world_mut()
            .resource_mut::<EncounterTimelineInbox>()
            .push(state);
    }
    app.world_mut()
        .resource_mut::<SnapshotBuffer>()
        .accept(frame, Instant::now());
    app.update();
    let playing = &app.world().resource::<CombatAudio>().playing;
    assert_eq!(playing.len(), MAX_BOSS_SOURCES);
    assert!((9..13).all(|id| playing.iter().filter(|a| a.id == id).count() <= MAX_PER_BOSS));
    let voices: Vec<_> = (0..8)
        .map(|_| mixer.claim(Bus::Voice).expect("reserved voice slot"))
        .collect();
    assert_eq!(voices.len(), 8);
}

#[test]
fn muting_keeps_the_presentation_and_unmuting_replays_nothing() {
    let mut shown = Vec::new();
    for gain in [0.0, 1.0] {
        let (mut app, mixer) = king_fixture();
        mixer.set_gain(Bus::Sfx, gain);
        let state = timeline(Move::ThreeTolls, Phase::Telegraph, Some((3, 3)), 100);
        assert_eq!(
            advance(&mut app, 100, Action::Windup, Some(state)),
            [Cue::TollThird]
        );
        shown.push(app.world().resource::<EncounterPresentation>().0.clone());
        if gain == 0.0 {
            assert_eq!(energy(&mut app, &mixer, 90), [0.0; 2]);
            mixer.set_gain(Bus::Sfx, 1.0);
            assert_eq!(advance(&mut app, 101, Action::Windup, None), []);
            assert_eq!(energy(&mut app, &mixer, 10), [0.0; 2]);
        }
    }
    assert_eq!(shown[0], shown[1], "sound never changes what is announced");
}

#[test]
fn king_recipes_are_finite_bounded_distinct_and_claim_no_audition() {
    for rate in [8000, 44100, 48000, 192000] {
        let baked: Vec<_> = sounds::CUES
            .iter()
            .map(|cue| {
                let baked = cue.describe().bake(cue.seconds(), rate, 19).unwrap();
                let samples = baked.samples();
                assert_eq!((samples.first(), samples.last()), (Some(&0.0), Some(&0.0)));
                assert!(
                    samples.iter().all(|v| v.is_finite() && v.abs() < 0.99),
                    "clipping {cue:?}"
                );
                assert!(samples.iter().any(|v| v.abs() > 0.05), "inaudible {cue:?}");
                (*cue, baked)
            })
            .collect();
        for (index, (a, left)) in baked.iter().enumerate() {
            for (b, right) in &baked[index + 1..] {
                let length = left.samples().len().min(right.samples().len());
                let difference = left.samples()[..length]
                    .iter()
                    .zip(right.samples())
                    .map(|(x, y)| (x - y).abs())
                    .sum::<f32>()
                    / length as f32;
                assert!(difference > 0.01, "{a:?} and {b:?} collapsed at {rate}");
            }
        }
    }
}
