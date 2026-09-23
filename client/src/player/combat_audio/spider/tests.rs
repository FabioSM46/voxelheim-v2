//! The spider's voice measured: texture rather than tone, level with speed, density with
//! numbers, and a horde capped at a skitter.
use super::super::{CombatAudio, guardian::tests::fixture};
use super::*;
use crate::{
    audio::{AudioMixer, Mixer, Sink, synth::Glide, synth::Vibrato},
    net::{MobAction, MobState, SessionParams, SnapshotInbox},
    player::{SnapshotBuffer, WorldCamera},
};
use bevy::asset::AssetPlugin;
use bevy::time::TimeUpdateStrategy;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub(in super::super) const RATE: u32 = 8000;

pub(in super::super) fn baked(sound: &Sound, seconds: f32) -> Vec<f32> {
    sound
        .bake(seconds, RATE, 19)
        .expect("a spider sound bakes")
        .samples()
        .to_vec()
}

/// Energy per DFT bin between `low` and `high` hertz, by Goertzel's recurrence — the
/// measurement `water_audio::sounds` reads a splash with.
pub(in super::super) fn band_power(samples: &[f32], low: f32, high: f32) -> Vec<f64> {
    let n = samples.len();
    let bin = |hz: f32| (hz * n as f32 / RATE as f32).round() as usize;
    (bin(low)..bin(high).min(n / 2))
        .map(|k| {
            let coefficient = 2.0 * (std::f64::consts::TAU * k as f64 / n as f64).cos();
            let (mut s1, mut s2) = (0.0f64, 0.0f64);
            for &x in samples {
                let s0 = f64::from(x) + coefficient * s1 - s2;
                s2 = s1;
                s1 = s0;
            }
            s1 * s1 + s2 * s2 - coefficient * s1 * s2
        })
        .collect()
}

/// Spectral flatness from 300 Hz to 3.6 kHz: the geometric mean of the power spectrum over
/// its arithmetic mean. Near one for noise, near zero for a note.
pub(in super::super) fn flatness(samples: &[f32]) -> f64 {
    let power = band_power(samples, 300.0, 3600.0);
    let mean = power.iter().sum::<f64>() / power.len() as f64;
    let log = power.iter().map(|p| (p + mean * 1e-12).ln()).sum::<f64>() / power.len() as f64;
    log.exp() / mean
}

/// The negative control: the same description with every noise layer replaced by a clean
/// sine at its filter's centre and every gate removed — the same layers, envelopes and
/// frequencies as pure tones. It must fail the measurement the real sound passes.
pub(in super::super) fn pure_tones(sound: Sound) -> Sound {
    let layers = sound
        .layers
        .into_iter()
        .map(|layer| {
            let hz = layer.filter.map_or(1000.0, |filter| filter.hz).min(3500.0);
            let exciter = match layer.exciter {
                Exciter::Noise(_) => Exciter::Oscillator {
                    wave: Wave::Sine,
                    hz,
                },
                Exciter::Glide(glide) => Exciter::Glide(Glide {
                    wave: Wave::Sine,
                    vibrato: Vibrato::NONE,
                    ..glide
                }),
                oscillator @ Exciter::Oscillator { .. } => oscillator,
            };
            Layer {
                exciter,
                filter: None,
                gate: None,
                ..layer
            }
        })
        .collect();
    Sound { layers }
}

pub(in super::super) fn energy(samples: &[f32]) -> f64 {
    samples.iter().map(|x| f64::from(*x).powi(2)).sum()
}

/// Separate ticks: how many times the level jumps up out of near-silence.
pub(in super::super) fn onsets(samples: &[f32]) -> usize {
    let window = (RATE / 1000) as usize;
    let levels: Vec<f32> = samples
        .chunks(window)
        .map(|chunk| chunk.iter().fold(0.0f32, |peak, x| peak.max(x.abs())))
        .collect();
    let loud = levels.iter().fold(0.0f32, |peak, x| peak.max(*x));
    levels
        .windows(2)
        .filter(|pair| pair[0] < 0.2 * loud && pair[1] >= 0.2 * loud)
        .count()
}

#[test]
fn every_spider_sound_is_texture_and_a_pure_tone_control_fails_the_measure() {
    for cue in CUES {
        let real = flatness(&baked(&cue.describe(), cue.seconds()));
        let control = flatness(&baked(&pure_tones(cue.describe()), cue.seconds()));
        assert!(real > 0.2, "{cue:?}: flatness {real}");
        assert!(control < 0.05, "{cue:?}: pure tones measured {control}");
    }
    // And the plainest note of all, at a spider's pitch, fails it outright.
    let note = Sound {
        layers: vec![Layer {
            exciter: Exciter::Oscillator {
                wave: Wave::Sine,
                hz: 2600.0,
            },
            gain: 0.6,
            envelope: envelope(0.003, 0.15),
            gate: None,
            filter: None,
        }],
    };
    assert!(flatness(&baked(&note, 0.16)) < 0.05);
}

#[test]
fn spider_recipes_are_bounded_silent_at_both_edges_and_distinct() {
    for rate in [8000, 44100, 48000, 192000] {
        let rendered: Vec<_> = CUES
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
        // Distinct against their own level: the ticking cues are mostly silence between
        // ticks, so an absolute floor would pass two identical quiet recipes.
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
                    "two spider cues collapsed together at {rate} Hz"
                );
            }
        }
    }
}

#[test]
fn legs_tick_rather_than_hum_and_a_swarm_ticks_denser_than_one_spider() {
    let one = baked(&Cue::Skitter.describe(), Cue::Skitter.seconds());
    let many = baked(&Cue::SkitterSwarm.describe(), Cue::SkitterSwarm.seconds());
    let (one_rate, many_rate) = (
        onsets(&one) as f32 / Cue::Skitter.seconds(),
        onsets(&many) as f32 / Cue::SkitterSwarm.seconds(),
    );
    assert!(
        onsets(&one) >= 3,
        "a skitter is separate ticks: {}",
        onsets(&one)
    );
    assert!(
        many_rate > one_rate,
        "{many_rate} ticks a second against {one_rate}"
    );
    // The swarm is denser, not louder: its energy per second is no more than one spider's.
    assert!(
        energy(&many) / f64::from(Cue::SkitterSwarm.seconds())
            <= energy(&one) / f64::from(Cue::Skitter.seconds()) * 1.2
    );
    // Faster is louder, down to a floor that keeps a creeping spider audible.
    assert!(step_gain(1.0) < step_gain(3.0) && step_gain(3.0) < step_gain(5.0));
    assert_eq!(step_gain(0.0), step_gain(0.5));
    assert_eq!(step_gain(5.0), 1.0);
    assert_eq!(step_gain(50.0), 1.0);
}

fn spider(entity_id: u64, pos: [f32; 3], action: MobAction) -> MobState {
    MobState {
        entity_id,
        kind: MobKind::CaveSpider,
        pos,
        vel: [0.0; 3],
        yaw: 0.0,
        health: 20,
        max_health: 20,
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
            CatalogueCue::Spider(cue) => Some(*cue),
            _ => None,
        })
        .collect()
}

#[test]
fn a_spider_hisses_on_notice_and_bites_on_the_lunge_but_not_on_the_windup() {
    let (mut app, _mixer) = fixture(8000);
    let mut tick = 1;
    let mut step = |app: &mut App, action| {
        let mut snapshot = crate::player::encounters::tests::snapshot(tick);
        tick += 1;
        snapshot.mobs = vec![spider(9, [0.0, 0.0, 1.0], action)];
        app.world_mut()
            .resource_mut::<SnapshotBuffer>()
            .accept(snapshot, Instant::now());
        app.update();
        started(app)
    };
    assert!(
        step(&mut app, MobAction::Chase).is_empty(),
        "first seen chasing is no notice"
    );
    assert!(step(&mut app, MobAction::Idle).is_empty());
    assert_eq!(step(&mut app, MobAction::Chase), [Cue::Hiss]);
    assert!(
        step(&mut app, MobAction::Chase).is_empty(),
        "one hiss per notice"
    );
    assert!(
        step(&mut app, MobAction::Windup).is_empty(),
        "the rising legs are the telegraph"
    );
    assert_eq!(step(&mut app, MobAction::Recovery), [Cue::Bite]);
    assert!(step(&mut app, MobAction::Recovery).is_empty());
    assert!(
        step(&mut app, MobAction::Chase).is_empty()
            && step(&mut app, MobAction::Recovery).is_empty(),
        "a recovery nobody watched wind up is not a bite"
    );
}

pub(in super::super) struct Buffer(pub(in super::super) Vec<f32>);
impl Sink for Buffer {
    fn block(&mut self) -> &mut [f32] {
        &mut self.0
    }
}

/// The production rig and the production audio together: spiders run through the real
/// snapshot consumer, their drawn legs stamp steps, and the combat audio hears them.
pub(in super::super) fn horde_app() -> (App, Arc<Mixer>) {
    let mixer = Arc::new(Mixer::new());
    mixer.set_format(RATE, 2);
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, AssetPlugin::default()))
        .init_asset::<Mesh>()
        .init_asset::<StandardMaterial>()
        .insert_resource(crate::net::Session(SessionParams {
            clock: Default::default(),
            entity_id: 7,
            spawn: [0.0; 3],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 8,
            inventory_slots: 37,
            hotbar_slots: 9,
            equipment_slots: 4,
            player_token: crate::net::ANY_TOKEN,
            voice_range_blocks: 0.0,
        }))
        .add_plugins(crate::player::PlayerPlugin)
        .insert_resource(AudioMixer::from_shared_for_test(Arc::clone(&mixer)))
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            10,
        )));
    app.update();
    for mut camera in app
        .world_mut()
        .query_filtered::<&mut Transform, With<WorldCamera>>()
        .iter_mut(app.world_mut())
    {
        *camera = Transform::from_xyz(0.0, 1.5, 0.0);
    }
    (app, mixer)
}

/// `count` spiders running round rings about the listener at the server's 5.0 blocks a
/// second, for two seconds: the most spider sources ever playing at once, the skitters
/// among them, every cue started, and the energy that reached the output.
fn run(count: u64) -> (usize, usize, Vec<Cue>, f32) {
    let (mut app, mixer) = horde_app();
    let (mut peak, mut legs, mut cues, mut heard) = (0, 0, Vec::new(), 0.0f32);
    for frame in 0..200u32 {
        if frame % 5 == 0 {
            let seconds = frame as f32 / 100.0;
            let mobs = (0..count)
                .map(|index| {
                    let ring = 3.0 + (index % 3) as f32;
                    let angle = index as f32 * 0.7 + seconds * 5.0 / ring;
                    spider(
                        100 + index,
                        [ring * angle.cos(), 0.0, ring * angle.sin()],
                        MobAction::Chase,
                    )
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
        let spiders: Vec<_> = state
            .playing
            .iter()
            .filter_map(|active| match active.cue {
                CatalogueCue::Spider(cue) => Some(cue),
                _ => None,
            })
            .collect();
        peak = peak.max(spiders.len());
        legs = legs.max(spiders.iter().filter(|cue| cue.legs()).count());
        cues.extend(started(&app));
        let mut buffer = Buffer(vec![0.0; (RATE / 100 * 2) as usize]);
        mixer.render(&mut buffer);
        heard += buffer.0.iter().map(|x| x * x).sum::<f32>();
    }
    (peak, legs, cues, heard)
}

#[test]
fn a_horde_of_thirty_is_a_capped_skitter_and_never_a_roar() {
    let (one_peak, _, one_cues, one) = run(1);
    assert_eq!(one_peak, 1, "one spider plays one skitter at a time");
    assert!(
        one_cues.iter().filter(|cue| **cue == Cue::Skitter).count() >= 5,
        "a running spider ticks: {one_cues:?}"
    );
    assert!(!one_cues.contains(&Cue::SkitterSwarm));
    assert!(one > 0.0);

    let (peak, legs, cues, horde) = run(30);
    assert!(
        peak <= MAX_SOURCES && legs <= MAX_SKITTERS,
        "{peak} sources, {legs} legs"
    );
    assert!(
        cues.contains(&Cue::SkitterSwarm),
        "thirty spiders tick as a swarm"
    );
    assert!(
        horde <= one * (MAX_SKITTERS as f32 + 0.5),
        "thirty spiders sum to {horde} against one spider's {one}"
    );
}
