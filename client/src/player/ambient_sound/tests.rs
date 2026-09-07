use super::*;
use crate::audio::{Bus, MAX_SOURCES, Mixer, Sink, VOICE_RESERVE};
use crate::net::{ChunkCoord, SessionParams};
use crate::world::{VoxelChunk, palette};
use std::sync::Arc;

struct Buffer(Vec<f32>);
impl Sink for Buffer {
    fn block(&mut self) -> &mut [f32] {
        &mut self.0
    }
}
fn mixer() -> AudioMixer {
    let mixer = Arc::new(Mixer::new());
    mixer.set_format(8000, 1);
    AudioMixer::from_shared_for_test(mixer)
}
fn energy(mixer: &AudioMixer, count: usize) -> f32 {
    let mut sink = Buffer(vec![0.0; count]);
    mixer.shared_for_test().render(&mut sink);
    sink.0.iter().map(|s| s * s).sum()
}
fn grass() -> Ambience {
    Ambience {
        ground: GroundLook::Grass,
        wooded: true,
    }
}
fn weather(kind: WeatherKind, intensity: u8) -> Option<WeatherState> {
    Some(WeatherState { kind, intensity })
}

#[test]
fn sky_curve_crossfades_and_ground_alone_selects_green_country() {
    let day = targets(&grass(), 0.0, None);
    let dusk = targets(&grass(), 0.5, None);
    let night = targets(&grass(), 1.0, None);
    assert_eq!((day.day, day.beds[0]), (1.0, 0.0));
    assert_eq!((dusk.day, dusk.beds[0]), (0.5, 0.5));
    assert_eq!((night.day, night.beds[0]), (0.0, 1.0));
    for look in [GroundLook::Snow, GroundLook::Sand, GroundLook::Unknown] {
        let v = targets(
            &Ambience {
                ground: look,
                wooded: true,
            },
            0.5,
            None,
        );
        assert_eq!(v.day, 0.0);
        assert_eq!(v.beds, [0.0; 4]);
    }
    let plain = Ambience {
        wooded: false,
        ..grass()
    };
    assert_eq!(targets(&plain, 0.0, None).day, 1.0);
    assert_eq!(birds::species_for(&grass()), Some(0));
    // No biome, temperature, terrain seed or gameplay facts enter this selector.
    let a = targets(&grass(), 0.3, weather(WeatherKind::Rain, 120));
    let b = targets(&plain, 0.3, weather(WeatherKind::Rain, 120));
    assert_eq!(a, b);
}

#[test]
fn rain_grows_louder_and_thicker_and_snow_has_its_own_lane() {
    let light = targets(&grass(), 0.0, weather(WeatherKind::Rain, 50));
    let heavy = targets(&grass(), 0.0, weather(WeatherKind::Rain, 220));
    assert!(heavy.beds[1] > light.beds[1]);
    assert!(heavy.beds[2] / heavy.beds[1] > light.beds[2] / light.beds[1]);
    let snow = targets(&grass(), 0.0, weather(WeatherKind::Snow, 220));
    assert_eq!(snow.beds[1..3], [0.0; 2]);
    assert!(snow.beds[3] > 0.0);
    assert_eq!(
        targets(&grass(), 0.0, weather(WeatherKind::Rain, 0)).beds,
        [0.0; 4]
    );
}

fn play_bed(voice: &mut BedVoice, mixer: &AudioMixer, target: f32, cover: f32) {
    voice.update(
        mixer,
        BedFrame {
            dt: 0.1,
            gain: target,
            seed: 7,
            placement: Placement {
                occlusion: cover,
                ..Placement::UNPOSITIONED
            },
        },
        || Bed::Rain.description(),
    );
}
#[test]
fn fade_is_gradual_and_eventually_returns_the_source() {
    let mixer = mixer();
    let mut voice = BedVoice::default();
    play_bed(&mut voice, &mixer, 1.0, 0.0);
    assert_eq!(voice.gain, 0.0);
    play_bed(&mut voice, &mixer, 1.0, 0.0);
    assert!(voice.gain > 0.0 && voice.gain < 0.1);
    for _ in 0..100 {
        play_bed(&mut voice, &mixer, 1.0, 0.0);
        energy(&mixer, 800);
    }
    assert!(voice.gain > 0.99);
    play_bed(&mut voice, &mixer, 0.0, 0.0);
    assert!(voice.gain > 0.9);
    for _ in 0..200 {
        play_bed(&mut voice, &mixer, 0.0, 0.0);
        energy(&mixer, 800);
    }
    assert_eq!(voice.gain, 0.0);
    assert_eq!(energy(&mixer, 800), 0.0);
}

#[test]
fn roof_of_stone_attenuates_without_cutting_the_bed() {
    let mut store = ChunkStore::default();
    let mut chunk = VoxelChunk::all_air(32);
    for x in 0..32 {
        for z in 0..32 {
            chunk.set(x, 10, z, palette::STONE);
        }
    }
    store.insert(
        ChunkCoord {
            cx: 0,
            cy: 0,
            cz: 0,
        },
        chunk,
    );
    let eye = Vec3::new(12.5, 5.5, 12.5);
    let cover = spatial::occlusion(&store, 32, eye, eye + Vec3::Y * 32.0);
    assert!(cover > 0.0);
    let hear = |cover| {
        let mixer = mixer();
        let mut voice = BedVoice::default();
        let mut total = 0.0;
        for _ in 0..80 {
            play_bed(&mut voice, &mixer, 1.0, cover);
            total += energy(&mixer, 800);
        }
        total
    };
    let open = hear(0.0);
    let roof = hear(cover);
    assert!(roof > 0.0 && roof < open * 0.5);
}

#[test]
fn ambience_bus_gain_and_voice_duck_reach_actual_bed_samples() {
    let hear = |gain, duck| {
        let mixer = mixer();
        mixer.shared_for_test().set_gain(Bus::Ambience, gain);
        mixer.shared_for_test().set_duck(duck);
        let mut voice = BedVoice::default();
        for _ in 0..50 {
            play_bed(&mut voice, &mixer, 1.0, 0.0);
            energy(&mixer, 800);
        }
        play_bed(&mut voice, &mixer, 1.0, 0.0);
        energy(&mixer, 800)
    };
    let open = hear(1.0, 1.0);
    assert!(open > 0.0);
    assert_eq!(hear(0.0, 1.0), 0.0);
    assert!(hear(1.0, 0.2) < open * 0.1);
}

#[test]
fn stolen_bed_yields_then_recovers_and_device_change_rebuilds() {
    let mixer = mixer();
    let mut voice = BedVoice::default();
    play_bed(&mut voice, &mixer, 1.0, 0.0);
    let others: Vec<_> = (0..MAX_SOURCES - VOICE_RESERVE - 1)
        .map(|_| mixer.shared_for_test().claim(Bus::Sfx).unwrap())
        .collect();
    let voices: Vec<_> = (0..VOICE_RESERVE)
        .map(|_| mixer.shared_for_test().claim(Bus::Voice).unwrap())
        .collect();
    assert!(mixer.shared_for_test().claim(Bus::Voice).is_none());
    play_bed(&mut voice, &mixer, 1.0, 0.0);
    assert_eq!(voice.gain, 0.0);
    assert!(others.iter().chain(voices.iter()).all(|v| v.live()));
    drop(others);
    drop(voices);
    energy(&mixer, 800);
    for _ in 0..40 {
        play_bed(&mut voice, &mixer, 1.0, 0.0);
        energy(&mixer, 800);
    }
    assert!(energy(&mixer, 800) > 0.0);
    mixer.shared_for_test().set_format(16000, 1);
    play_bed(&mut voice, &mixer, 1.0, 0.0);
    assert_eq!(voice.gain, 0.0);
    for _ in 0..40 {
        energy(&mixer, 1600);
        play_bed(&mut voice, &mixer, 1.0, 0.0);
    }
    assert!(energy(&mixer, 1600) > 0.0);
}

fn session() -> Session {
    Session(SessionParams {
        clock: Default::default(),
        entity_id: 1,
        spawn: [0.0; 3],
        world_seed: 7,
        tick_rate: 20,
        chunk_size: 32,
        view_distance: 8,
        inventory_slots: 37,
        hotbar_slots: 9,
        equipment_slots: 4,
        player_token: crate::net::ANY_TOKEN,
        voice_range_blocks: 32.0,
    })
}
#[test]
fn session_and_camera_lifetime_bound_all_country_sources() {
    let mixer = mixer();
    let shared = mixer.shared_for_test().clone();
    let mut app = App::new();
    app.insert_resource(mixer)
        .insert_resource(Time::<()>::default())
        .insert_resource(grass())
        .init_resource::<Weather>()
        .init_resource::<SkyClock>()
        .init_resource::<ChunkStore>();
    register(&mut app);
    let camera = app
        .world_mut()
        .spawn((WorldCamera, Transform::default()))
        .id();
    app.update();
    assert_eq!(app.world().resource::<Country>().day_gain, 0.0);
    app.insert_resource(session());
    for _ in 0..20 {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(100));
        app.update();
        energy(&AudioMixer::from_shared_for_test(shared.clone()), 800);
    }
    assert!(app.world().resource::<Country>().day_gain > 0.5);
    app.world_mut().despawn(camera);
    app.update();
    assert_eq!(app.world().resource::<Country>().day_gain, 0.0);
    app.world_mut().remove_resource::<Session>();
    app.update();
    energy(&AudioMixer::from_shared_for_test(shared.clone()), 12000);
    assert_eq!(energy(&AudioMixer::from_shared_for_test(shared), 800), 0.0);
}

#[test]
fn cricket_trills_have_pulses_and_a_slow_phrase_contour() {
    let levels: Vec<f32> = (0..1000)
        .map(|i| cricket_trill(f64::from(i) / 100.0))
        .collect();
    assert!(levels.iter().all(|v| *v >= 0.12 && *v <= 1.0));
    assert!(levels.iter().copied().fold(0.0, f32::max) > 0.95);
    let peaks = levels
        .windows(3)
        .filter(|w| w[1] > w[0] && w[1] > w[2])
        .count();
    assert!(
        (55..=59).contains(&peaks),
        "insect pulses must not become a steady noise"
    );
    assert!((cricket_trill(1024.0) - cricket_trill(1024.0001)).abs() < 0.01);
    assert_ne!(cricket_trill(0.125), cricket_trill(10.125));
}

#[test]
fn prolonged_source_pressure_does_not_spend_the_recovery_fade() {
    let mixer = mixer();
    let mut voice = BedVoice::default();
    let held: Vec<_> = (0..MAX_SOURCES)
        .map(|_| mixer.shared_for_test().claim(Bus::Voice).unwrap())
        .collect();
    for _ in 0..60 {
        play_bed(&mut voice, &mixer, 1.0, 0.0);
        assert_eq!(voice.gain, 0.0, "an inaudible bed must not spend its fade");
        assert_eq!(energy(&mixer, 800), 0.0);
    }
    drop(held);
    energy(&mixer, 800); // The callback returns the dropped slots before admission.
    for _ in 0..20 {
        play_bed(&mut voice, &mixer, 1.0, 0.0);
        energy(&mixer, 800);
        if voice.gain > 0.0 {
            break;
        }
    }
    assert!(voice.gain > 0.0 && voice.gain < 0.1);
    let mut early = 0.0;
    for _ in 0..10 {
        play_bed(&mut voice, &mixer, 1.0, 0.0);
        early += energy(&mixer, 800);
    }
    assert!(voice.gain < 0.5);
    for _ in 0..90 {
        play_bed(&mut voice, &mixer, 1.0, 0.0);
        energy(&mixer, 800);
    }
    let mut settled = 0.0;
    for _ in 0..10 {
        play_bed(&mut voice, &mixer, 1.0, 0.0);
        settled += energy(&mixer, 800);
    }
    assert!(early > 0.0 && early < settled * 0.3);
    assert!(voice.gain > 0.99);
}
