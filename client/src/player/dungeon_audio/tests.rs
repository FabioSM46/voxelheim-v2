//! The dungeon's moving parts measured: texture rather than tone, one sound for each thing
//! that moved, and nothing for anything that did not.
use super::*;
use crate::audio::{
    Mixer,
    synth::{Envelope, Exciter, Glide, Layer, Sound, Vibrato, Wave},
};
use crate::net::SessionParams;
use bevy::asset::AssetPlugin;
use bevy::time::TimeUpdateStrategy;
use sounds::CUES;
use std::sync::Arc;

const RATE: u32 = 8000;

fn baked(sound: &Sound, seconds: f32) -> Vec<f32> {
    sound
        .bake(seconds, RATE, 1295)
        .expect("a dungeon sound bakes")
        .samples()
        .to_vec()
}

/// Energy per DFT bin between `low` and `high` hertz, by Goertzel's recurrence — the
/// measurement the spider's and the water's sounds are read with.
fn band_power(samples: &[f32], low: f32, high: f32) -> Vec<f64> {
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

/// Spectral flatness from 150 Hz to 3.6 kHz: the geometric mean of the power spectrum over
/// its arithmetic mean. Near one for noise, near zero for a note. Lower than the spider's
/// 300 Hz floor, because a door and a dropped grille live down there.
fn flatness(samples: &[f32]) -> f64 {
    let power = band_power(samples, 150.0, 3600.0);
    let mean = power.iter().sum::<f64>() / power.len() as f64;
    let log = power.iter().map(|p| (p + mean * 1e-12).ln()).sum::<f64>() / power.len() as f64;
    log.exp() / mean
}

/// The negative control: every noise layer replaced by a clean sine at its filter's centre
/// and every gate removed — the same layers, envelopes and frequencies as pure tones.
fn pure_tones(sound: Sound) -> Sound {
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

#[test]
fn every_dungeon_sound_is_texture_and_a_pure_tone_control_fails_the_measure() {
    for cue in CUES {
        let real = flatness(&baked(&cue.describe(), cue.seconds()));
        let control = flatness(&baked(&pure_tones(cue.describe()), cue.seconds()));
        assert!(real > 0.2, "{cue:?}: flatness {real}");
        assert!(control < 0.05, "{cue:?}: pure tones measured {control}");
    }
    // And the plainest note of all, a hum at a rune's ring, fails it outright.
    let hum = Sound {
        layers: vec![Layer {
            exciter: Exciter::Oscillator {
                wave: Wave::Sine,
                hz: 1330.0,
            },
            gain: 0.6,
            envelope: Envelope {
                attack: 0.1,
                decay: 1.0,
                sustain: 0.0,
                release: 0.01,
            },
            gate: None,
            filter: None,
        }],
    };
    assert!(flatness(&baked(&hum, 1.2)) < 0.05);
}

#[test]
fn no_dungeon_sound_is_an_oscillator_at_all() {
    for cue in CUES {
        for layer in cue.describe().layers {
            assert!(
                matches!(layer.exciter, Exciter::Noise(_)),
                "{cue:?} carries a tone"
            );
        }
    }
}

#[test]
fn dungeon_recipes_are_bounded_silent_at_both_edges_and_distinct() {
    for rate in [8000, 44100, 48000, 192000] {
        let rendered: Vec<_> = CUES
            .iter()
            .map(|cue| {
                let baked = cue.describe().bake(cue.seconds(), rate, 1295).unwrap();
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
                    "two dungeon cues collapsed together at {rate} Hz"
                );
            }
        }
    }
}

#[test]
fn a_rune_waking_rings_and_one_going_dark_does_not() {
    // The ring is the rune's resonance: energy concentrated round its two bands, which the
    // dousing scrape has no share of.
    let share = |cue: Cue| {
        let samples = baked(&cue.describe(), cue.seconds());
        let ring: f64 = band_power(&samples, 1250.0, 1410.0).iter().sum();
        let all: f64 = band_power(&samples, 150.0, 3600.0).iter().sum();
        ring / all
    };
    assert!(
        share(Cue::RuneIgnite) > 2.0 * share(Cue::RuneDouse),
        "{} against {}",
        share(Cue::RuneIgnite),
        share(Cue::RuneDouse)
    );
}

fn at(x: i32, y: i32, z: i32) -> BlockCoord {
    BlockCoord { x, y, z }
}

fn change(pos: BlockCoord, before: BlockId, after: BlockId) -> BlockReplaced {
    BlockReplaced { pos, before, after }
}

#[test]
fn each_moving_part_is_heard_for_what_it_is_and_from_where_it_is() {
    let heard = hear(
        &[
            change(at(1, 2, 3), palette::LEVER_OFF, palette::LEVER_ON),
            change(at(4, 2, 3), palette::LEVER_ON, palette::LEVER_OFF),
            change(at(5, 2, 3), palette::RUNE_STONE, palette::RUNE_STONE_LIT),
            change(at(6, 2, 3), palette::RUNE_STONE_LIT, palette::RUNE_STONE),
            change(at(7, 2, 3), palette::COBWEB, palette::AIR),
        ],
        true,
    );
    assert_eq!(
        heard,
        vec![
            (Cue::Lever, Vec3::new(1.5, 2.5, 3.5)),
            (Cue::Lever, Vec3::new(4.5, 2.5, 3.5)),
            (Cue::RuneIgnite, Vec3::new(5.5, 2.5, 3.5)),
            (Cue::RuneDouse, Vec3::new(6.5, 2.5, 3.5)),
            (Cue::WebTear, Vec3::new(7.5, 2.5, 3.5)),
        ]
    );
}

#[test]
fn a_grille_or_a_door_is_heard_once_from_the_middle_of_its_cells() {
    let grille: Vec<BlockReplaced> = (0..3)
        .flat_map(|x| (0..3).map(move |y| (x, y)))
        .map(|(x, y)| change(at(x, y, 9), palette::IRON_GRILLE_X, palette::AIR))
        .collect();
    assert_eq!(
        hear(&grille, true),
        vec![(Cue::GrilleUp, Vec3::new(1.5, 1.5, 9.5))]
    );
    let shut: Vec<BlockReplaced> = grille
        .iter()
        .map(|c| change(c.pos, palette::AIR, palette::IRON_GRILLE_Z))
        .collect();
    assert_eq!(hear(&shut, true)[0].0, Cue::GrilleDown);

    let door: Vec<BlockReplaced> = (0..9)
        .map(|y| change(at(20, y, 0), palette::BLACK_BRICK, palette::AIR))
        .collect();
    assert_eq!(
        hear(&door, true),
        vec![(Cue::Door, Vec3::new(20.5, 4.5, 0.5))]
    );
    let closing: Vec<BlockReplaced> = door
        .iter()
        .map(|c| change(c.pos, palette::AIR, palette::BLACK_BRICK))
        .collect();
    assert_eq!(hear(&closing, true).len(), 1);
}

#[test]
fn a_player_building_and_the_open_world_are_never_a_door() {
    // One block placed and one broken in a dungeon: somebody building, heard by the mining
    // sounds and not here.
    assert!(
        hear(
            &[
                change(at(0, 0, 0), palette::AIR, palette::STONE),
                change(at(3, 0, 0), palette::STONE, palette::AIR),
            ],
            true
        )
        .is_empty()
    );
    // A whole wall knocked through in the open world is a crew at work, not a door.
    let wall: Vec<BlockReplaced> = (0..9)
        .map(|y| change(at(20, y, 0), palette::STONE, palette::AIR))
        .collect();
    assert!(hear(&wall, false).is_empty());
    // Nothing that is not a moving part makes a sound: grass grown, water spread, a lever
    // that was built rather than thrown.
    assert!(
        hear(
            &[
                change(at(0, 0, 0), palette::DIRT, palette::GRASS),
                change(at(1, 0, 0), palette::AIR, palette::WATER),
                change(at(2, 0, 0), palette::AIR, palette::LEVER_OFF),
            ],
            true
        )
        .is_empty()
    );
}

fn app() -> App {
    let mixer = Arc::new(Mixer::new());
    mixer.set_format(RATE, 2);
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, AssetPlugin::default()))
        .init_asset::<Mesh>()
        .init_asset::<StandardMaterial>()
        .insert_resource(Session(SessionParams {
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
        .init_resource::<ChunkStore>()
        .add_plugins(crate::player::PlayerPlugin)
        .insert_resource(AudioMixer::from_shared_for_test(mixer))
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
    app
}

fn playing(app: &App) -> usize {
    app.world().resource::<DungeonSounds>().playing.len()
}

#[test]
fn a_lever_the_server_threw_is_heard_and_one_it_refused_is_not() {
    let mut app = app();
    // A refused pull changes no voxel, so the world reports nothing and nothing plays.
    app.update();
    assert_eq!(playing(&app), 0);

    app.world_mut()
        .write_message(change(at(2, 1, 2), palette::LEVER_OFF, palette::LEVER_ON));
    app.update();
    assert_eq!(playing(&app), 1, "the thrown lever is heard");

    // Far past the range, a lever is not mixed at all.
    app.world_mut()
        .write_message(change(at(200, 1, 2), palette::LEVER_ON, palette::LEVER_OFF));
    app.update();
    assert_eq!(playing(&app), 1);

    // And a burst is capped rather than summed into one roar.
    for x in 0..20 {
        app.world_mut()
            .write_message(change(at(x, 1, 4), palette::COBWEB, palette::AIR));
    }
    app.update();
    assert_eq!(playing(&app), MAX_PLAYING);
}
