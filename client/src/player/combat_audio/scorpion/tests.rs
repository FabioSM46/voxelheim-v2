//! The scorpion's voice measured: texture rather than tone, a rattle that quickens toward the
//! strike, sand lower than a step — and, through the production rig and audio together, each
//! cue on the frame the drawn scorpion earns it and the sting's rattle never on the swipe.
use super::super::CombatAudio;
use super::super::spider::tests::{
    Buffer, RATE, baked, band_power, flatness, horde_app, onsets, pure_tones,
};
use super::*;
use crate::{
    audio::synth::{Envelope, Wave},
    net::{ChunkCoord, MobAction, MobState, SnapshotInbox},
    player::WorldCamera,
    world::{ChunkStore, VoxelChunk, palette},
};
use std::time::Instant;

#[test]
fn every_scorpion_sound_is_texture_and_a_pure_tone_control_fails_the_measure() {
    for cue in CUES {
        let real = flatness(&baked(&cue.describe(), cue.seconds()));
        let control = flatness(&baked(&pure_tones(cue.describe()), cue.seconds()));
        assert!(real > 0.2, "{cue:?}: flatness {real}");
        assert!(control < 0.05, "{cue:?}: pure tones measured {control}");
        assert!(
            cue.describe()
                .layers
                .iter()
                .all(|layer| matches!(layer.exciter, Exciter::Noise(_))),
            "{cue:?} carries a tone"
        );
    }
    // And a plain note at a rattle's pitch fails it outright.
    let note = Sound {
        layers: vec![Layer {
            exciter: Exciter::Oscillator {
                wave: Wave::Sine,
                hz: 2900.0,
            },
            gain: 0.6,
            envelope: Envelope {
                attack: 0.03,
                decay: 0.7,
                sustain: 0.0,
                release: 0.01,
            },
            gate: None,
            filter: None,
        }],
    };
    assert!(flatness(&baked(&note, 0.75)) < 0.05);
}

#[test]
fn scorpion_recipes_are_bounded_silent_at_both_edges_and_distinct() {
    for rate in [8000, 44100, 48000, 192000] {
        let rendered: Vec<Vec<f32>> = CUES
            .iter()
            .map(|cue| {
                let baked = cue.describe().bake(cue.seconds(), rate, 19).unwrap();
                let samples = baked.samples();
                assert_eq!(samples.first(), Some(&0.0));
                assert_eq!(samples.last(), Some(&0.0));
                assert!(samples.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
                assert!(
                    samples.iter().any(|v| v.abs() > 0.05),
                    "{cue:?} is inaudible"
                );
                samples.to_vec()
            })
            .collect();
        let level =
            |samples: &[f32]| samples.iter().map(|v| v.abs()).sum::<f32>() / samples.len() as f32;
        for (index, a) in rendered.iter().enumerate() {
            for b in &rendered[index + 1..] {
                let length = a.len().min(b.len());
                let difference = a[..length]
                    .iter()
                    .zip(b)
                    .map(|(a, b)| (a - b).abs())
                    .sum::<f32>()
                    / length as f32;
                assert!(
                    difference > 0.5 * (level(a) + level(b)) / 2.0,
                    "two scorpion cues collapsed together at {rate} Hz"
                );
            }
        }
    }
}

#[test]
fn the_rattle_is_a_train_of_dry_clicks_that_quickens_toward_the_strike() {
    let rattle = baked(&Cue::Rattle.describe(), Cue::Rattle.seconds());
    let half = rattle.len() / 2;
    let (early, late) = (onsets(&rattle[..half]), onsets(&rattle[half..]));
    assert!(
        early + late >= 12,
        "a rattle is many clicks: {}",
        early + late
    );
    assert!(late > early, "it quickens: {early} clicks then {late}");
    // It is over before the sting lands: most of the 1100 ms telegraph, never the strike.
    assert!(Cue::Rattle.seconds() < 1.1);
    let step = baked(&Cue::Click.describe(), Cue::Click.seconds());
    assert!(
        onsets(&step) >= 2,
        "a step is separate clicks: {}",
        onsets(&step)
    );
    assert!(step_gain(1.0) < step_gain(2.0) && step_gain(2.0) < step_gain(2.6));
    assert_eq!(step_gain(0.0), step_gain(0.3));
    assert_eq!(step_gain(10.0), 1.0);
}

/// The power-weighted mean frequency, in hertz.
fn centroid(samples: &[f32]) -> f64 {
    let power = band_power(samples, 60.0, 3900.0);
    let width = f64::from(RATE) / samples.len() as f64;
    let low = (60.0 / width).round();
    let total: f64 = power.iter().sum();
    power
        .iter()
        .enumerate()
        .map(|(k, p)| (low + k as f64) * width * p)
        .sum::<f64>()
        / total
}

#[test]
fn the_sand_is_a_low_long_slide_under_a_steps_dry_click() {
    let sand = baked(&Cue::Sand.describe(), Cue::Sand.seconds());
    let step = baked(&Cue::Click.describe(), Cue::Click.seconds());
    let rattle = baked(&Cue::Rattle.describe(), Cue::Rattle.seconds());
    assert!(
        centroid(&sand) < 0.75 * centroid(&step),
        "sand at {} Hz against a click at {} Hz",
        centroid(&sand),
        centroid(&step)
    );
    assert!(centroid(&sand) < centroid(&rattle));
    assert!(Cue::Sand.seconds() > 4.0 * Cue::Click.seconds());
}

fn scorpion(pos: [f32; 3], action: MobAction) -> MobState {
    MobState {
        entity_id: 30,
        kind: MobKind::Scorpion,
        pos,
        vel: [0.0; 3],
        yaw: 0.0,
        health: 72,
        max_health: 72,
        action,
        target_entity_id: 7,
    }
}

fn started(app: &App) -> Vec<Cue> {
    app.world()
        .resource::<CombatAudio>()
        .started
        .iter()
        .filter_map(|(_, cue, _)| match cue {
            CatalogueCue::Scorpion(cue) => Some(*cue),
            _ => None,
        })
        .collect()
}

/// The production rig and audio over a sand floor whose surface is at y = 65, the listener a
/// few blocks off.
fn sand_hall() -> (App, std::sync::Arc<crate::audio::Mixer>) {
    let (mut app, mixer) = horde_app();
    let mut chunk = VoxelChunk::all_air(32);
    for x in 0..32 {
        for z in 0..32 {
            chunk.set(x, 0, z, palette::SAND);
        }
    }
    let mut store = ChunkStore::default();
    store.insert(
        ChunkCoord {
            cx: 0,
            cy: 2,
            cz: 0,
        },
        chunk,
    );
    app.insert_resource(store);
    for mut camera in app
        .world_mut()
        .query_filtered::<&mut Transform, With<WorldCamera>>()
        .iter_mut(app.world_mut())
    {
        *camera = Transform::from_xyz(8.0, 66.6, 4.5);
    }
    (app, mixer)
}

/// Holds a frame to 10 ms of real time. The renderer interpolates the drawn scorpion at the
/// wall clock (`Instant::now()`) and its steps follow the drawn root, so an unpaced loop would
/// count steps for however much ground the host's speed let it cover — the reasoning
/// `spider::tests::run` carries, applied here too.
fn pace(started_at: Instant) {
    if let Some(rest) = std::time::Duration::from_millis(10).checked_sub(started_at.elapsed()) {
        std::thread::sleep(rest);
    }
}

/// Runs `frames` 10 ms frames with a snapshot every fifth, the scorpion placed by `at`, and
/// returns every scorpion cue started.
fn play(
    (app, mixer): (&mut App, &crate::audio::Mixer),
    tick: &mut u32,
    frames: u32,
    action: MobAction,
    at: impl Fn(u32) -> [f32; 3],
) -> Vec<Cue> {
    let mut cues = Vec::new();
    for frame in 0..frames {
        let started_at = Instant::now();
        if frame % 5 == 0 {
            *tick += 1;
            app.world_mut().resource_mut::<SnapshotInbox>().push(
                crate::net::Snapshot {
                    server_tick: *tick,
                    mobs: vec![scorpion(at(frame), action)],
                    ..Default::default()
                },
                Instant::now(),
            );
        }
        app.update();
        cues.extend(started(app));
        // Drain the output as a device would, so a finished cue leaves the mix.
        mixer.render(&mut Buffer(vec![0.0; (RATE / 100 * 2) as usize]));
        pace(started_at);
    }
    cues
}

#[test]
fn the_sand_hisses_as_it_rises_its_feet_click_and_only_the_sting_rattles() {
    let (mut app, mixer) = sand_hall();
    let mut tick = 0;
    let buried = |_| [4.5, 64.0, 4.5];
    let risen = |_| [4.5, 65.0, 4.5];
    let count = |cues: &[Cue], cue| cues.iter().filter(|heard| **heard == cue).count();

    let lying = play((&mut app, &mixer), &mut tick, 60, MobAction::Idle, buried);
    assert!(lying.is_empty(), "a buried scorpion is silent: {lying:?}");
    let rising = play(
        (&mut app, &mixer),
        &mut tick,
        80,
        MobAction::Recovery,
        risen,
    );
    assert_eq!(rising, [Cue::Sand], "one slide of sand as it comes up");

    let walking = play(
        (&mut app, &mixer),
        &mut tick,
        120,
        MobAction::Chase,
        |frame| [4.5 - frame as f32 * 0.026, 65.0, 4.5],
    );
    assert!(
        count(&walking, Cue::Click) >= 3,
        "its feet click: {walking:?}"
    );
    assert_eq!(count(&walking, Cue::Rattle) + count(&walking, Cue::Sand), 0);
    let stand = |_| [1.38, 65.0, 4.5];

    // The rhythm the server keeps: the sting first, then the swipe, then the sting again.
    let sting = play((&mut app, &mixer), &mut tick, 110, MobAction::Windup, stand);
    assert_eq!(sting, [Cue::Rattle], "the sting's telegraph rattles");
    play(
        (&mut app, &mixer),
        &mut tick,
        140,
        MobAction::Recovery,
        stand,
    );
    play((&mut app, &mixer), &mut tick, 20, MobAction::Chase, stand);
    let swipe = play((&mut app, &mixer), &mut tick, 45, MobAction::Windup, stand);
    assert!(swipe.is_empty(), "the swipe has no rattle: {swipe:?}");
    play(
        (&mut app, &mixer),
        &mut tick,
        70,
        MobAction::Recovery,
        stand,
    );
    let again = play((&mut app, &mixer), &mut tick, 110, MobAction::Windup, stand);
    assert_eq!(again, [Cue::Rattle], "and the next sting does");
}

#[test]
fn a_crowd_of_scorpions_is_heard_as_its_nearest_few() {
    let (mut app, mixer) = sand_hall();
    let (mut peak, mut legs, mut heard) = (0, 0, 0.0f32);
    for frame in 0..200u32 {
        let started_at = Instant::now();
        if frame % 5 == 0 {
            let mobs = (0..8u64)
                .map(|index| {
                    let mut one = scorpion(
                        [
                            2.0 + index as f32 * 2.0 + frame as f32 * 0.026,
                            65.0,
                            2.0 + (index % 2) as f32 * 3.0,
                        ],
                        MobAction::Chase,
                    );
                    one.entity_id = 100 + index;
                    one
                })
                .collect();
            app.world_mut().resource_mut::<SnapshotInbox>().push(
                crate::net::Snapshot {
                    server_tick: frame / 5 + 1,
                    mobs,
                    ..Default::default()
                },
                Instant::now(),
            );
        }
        app.update();
        let state = app.world().resource::<CombatAudio>();
        let playing: Vec<_> = state
            .playing
            .iter()
            .filter_map(|active| match active.cue {
                CatalogueCue::Scorpion(cue) => Some(cue),
                _ => None,
            })
            .collect();
        peak = peak.max(playing.len());
        legs = legs.max(playing.iter().filter(|cue| cue.legs()).count());
        let mut buffer = Buffer(vec![0.0; (RATE / 100 * 2) as usize]);
        mixer.render(&mut buffer);
        heard += buffer.0.iter().map(|x| x * x).sum::<f32>();
        pace(started_at);
    }
    assert!(
        peak <= MAX_SOURCES && legs <= MAX_CLICKS,
        "{peak} sources, {legs} legs"
    );
    assert!(legs > 0 && heard > 0.0, "a walking crowd is heard");
}
