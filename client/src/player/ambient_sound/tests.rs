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
    assert_eq!((day.day, day.wildlife[4]), (1.0, 0.0));
    assert_eq!((dusk.day, dusk.wildlife[4]), (0.5, 0.5));
    assert_eq!((night.day, night.wildlife[4]), (0.0, 1.0));
    assert_eq!(
        night.beds, [0.0; 5],
        "crickets are no longer a continuous bed"
    );
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
        assert_eq!(v.wildlife[4], 0.0);
        assert_eq!(v.beds, [0.0; 5]);
    }
    let plain = Ambience {
        wooded: false,
        ..grass()
    };
    assert_eq!(birds::species_for(&grass()), Some(PARROT));
    // No biome, temperature, terrain seed or gameplay facts enter this selector. Trees decide
    // only whether the macaw is there to be heard.
    let a = targets(&grass(), 0.3, weather(WeatherKind::Rain, 120));
    let b = targets(&plain, 0.3, weather(WeatherKind::Rain, 120));
    assert_eq!((a.beds, a.wildlife), (b.beds, b.wildlife));
}

/// #1176: the day call played on every sunny grass tile, because a treeless plain has no
/// species and `is_none_or` counted that as parrot country. It is heard now exactly where the
/// bird table flies the macaw, and every other answer — no species, or another one — is
/// silence.
#[test]
fn the_macaw_is_heard_by_day_only_where_the_bird_table_flies_it() {
    let row = &birds::BIRDS[PARROT];
    assert!(
        row.ground == GroundLook::Grass && row.requires_wooded,
        "PARROT no longer names the macaw's row"
    );
    let mut heard = 0;
    for ground in [
        GroundLook::Grass,
        GroundLook::Sand,
        GroundLook::Snow,
        GroundLook::Unknown,
    ] {
        for wooded in [false, true] {
            let country = Ambience { ground, wooded };
            let macaw = birds::species_for(&country) == Some(PARROT);
            heard += usize::from(macaw);
            for night in [0.0, 0.25, 1.0] {
                let expected = if macaw { 1.0 - night } else { 0.0 };
                for weather in [None, weather(WeatherKind::Rain, 200)] {
                    assert_eq!(
                        targets(&country, night, weather).day,
                        expected,
                        "wooded {wooded}, night {night}"
                    );
                }
            }
        }
    }
    assert_eq!(heard, 1, "only wooded grass is the macaw's");
    let plain = Ambience {
        wooded: false,
        ..grass()
    };
    assert_eq!(
        targets(&plain, 0.0, None).day,
        0.0,
        "an open plain is silent"
    );
    assert_eq!(targets(&grass(), 0.0, None).day, 1.0);
    assert_eq!(targets(&grass(), 1.0, None).day, 0.0, "no macaw at night");
}

/// Ten minutes of the real system at 0.1 s a tick, on the world's own seed: the ambience,
/// the day lane and its profile together, and nothing but the macaw sounding by day. Returns
/// each tick's energy.
fn a_simulated_day(ambience: Ambience) -> Vec<f32> {
    let mixer = mixer();
    let shared = mixer.shared_for_test().clone();
    let mut app = App::new();
    app.insert_resource(mixer)
        .insert_resource(Time::<()>::default())
        .insert_resource(ambience)
        .insert_resource(session())
        .init_resource::<Weather>()
        .init_resource::<SkyClock>()
        .init_resource::<ChunkStore>();
    register(&mut app);
    app.world_mut().spawn((WorldCamera, Transform::default()));
    (0..6000)
        .map(|_| {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_millis(100));
            app.update();
            energy(&AudioMixer::from_shared_for_test(shared.clone()), 800)
        })
        .collect()
}

/// #1176: one call every 0.7 to 2.5 s was close to a continuous bed. A day in the macaw's
/// wood now hears a few calls a minute, and most of every minute is silence; an open plain
/// hears nothing at all.
#[test]
fn a_simulated_day_in_a_wood_hears_a_few_squawks_a_minute_and_a_plain_hears_none() {
    let levels = a_simulated_day(grass());
    // A call is at most 0.85 s and the next starts at least 8 s later, so a sound after two
    // silent seconds is a new call.
    let mut calls = 0;
    let mut quiet = usize::MAX;
    for level in &levels {
        if *level == 0.0 {
            quiet = quiet.saturating_add(1);
        } else {
            calls += usize::from(quiet >= 20);
            quiet = 0;
        }
    }
    let per_minute = calls as f32 / 10.0;
    assert!(
        (2.0..=6.0).contains(&per_minute),
        "{per_minute} calls a minute"
    );
    let silent = levels.iter().filter(|level| **level == 0.0).count();
    assert!(silent > 5400, "{silent} of 6000 ticks silent");
    let plain = a_simulated_day(Ambience {
        wooded: false,
        ..grass()
    });
    assert!(plain.iter().all(|level| *level == 0.0), "a plain squawked");
}

#[test]
fn rain_grows_louder_and_thicker_and_snow_has_its_own_lane() {
    let light = targets(&grass(), 0.0, weather(WeatherKind::Rain, 50));
    let heavy = targets(&grass(), 0.0, weather(WeatherKind::Rain, 220));
    assert!(heavy.beds[0] > light.beds[0]);
    assert!(heavy.beds[1] / heavy.beds[0] > light.beds[1] / light.beds[0]);
    let snow = targets(&grass(), 0.0, weather(WeatherKind::Snow, 220));
    assert_eq!(snow.beds[0..2], [0.0; 2]);
    assert!(snow.beds[2] > 0.0);
    assert_eq!(
        targets(&grass(), 0.0, weather(WeatherKind::Rain, 0)).beds,
        [0.0; 5]
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
fn crickets_are_a_call_gated_on_the_green_night_the_bed_used() {
    let profile = sounds::Call::Cricket.profile();
    assert_eq!(
        (
            profile.interval,
            profile.radius,
            profile.height,
            profile.seconds,
            profile.range
        ),
        ([6.0, 16.0], 4.0, -1.2, 0.45, 24.0)
    );
    assert!(profile.seconds < profile.interval[0]);
    assert!(matches!(CALLS[4], sounds::Call::Cricket));
    for night in [0.0, 0.25, 1.0] {
        for wooded in [false, true] {
            let green = Ambience {
                ground: GroundLook::Grass,
                wooded,
            };
            assert_eq!(targets(&green, night, None).wildlife[4], night);
            for kind in [WeatherKind::Rain, WeatherKind::Snow, WeatherKind::Blizzard] {
                assert_eq!(
                    targets(&green, night, weather(kind, 255)).wildlife[4],
                    night
                );
            }
        }
        for ground in [GroundLook::Sand, GroundLook::Snow, GroundLook::Unknown] {
            let country = Ambience {
                ground,
                wooded: true,
            };
            assert_eq!(targets(&country, night, None).wildlife[4], 0.0);
        }
    }
}

#[test]
fn a_simulated_night_hears_a_few_cri_cris_a_minute_rather_than_a_wall() {
    for seed in [17, 39, 1123] {
        let (starts, levels) = wildlife_sequence(sounds::Call::Cricket, seed, 1.0, 1.0);
        // wildlife_sequence spans ten minutes of 0.1 s ticks.
        let calls = starts.len() as f32 / 10.0;
        assert!((3.0..=8.0).contains(&calls), "{calls} calls a minute");
        let syllables = starts
            .iter()
            .map(|(_, seed, _)| sounds::syllables(*seed))
            .sum::<usize>() as f32
            / 10.0;
        assert!(
            syllables >= calls * 2.0 && syllables <= calls * 3.0,
            "{syllables} syllables a minute from {calls} calls"
        );
        // Most of every minute is silence, and every call is still heard across more than
        // one tick: two or three syllables spread over a quarter to half a second.
        let silent = levels.iter().filter(|energy| **energy == 0.0).count();
        assert!(silent > 5400, "{silent} of 6000 ticks silent");
        let heard = 6000 - silent;
        assert!(
            heard >= starts.len() * 2,
            "{heard} ticks heard from {} calls",
            starts.len()
        );
    }
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

#[test]
fn countries_and_twilight_select_their_own_calls_without_weather_deciding_ground() {
    for wooded in [false, true] {
        for (ground, first) in [(GroundLook::Sand, 0), (GroundLook::Snow, 2)] {
            let country = Ambience { ground, wooded };
            for (night, expected) in [(0.0, [1.0, 0.0]), (0.5, [0.5, 0.5]), (1.0, [0.0, 1.0])] {
                let target = targets(&country, night, None);
                assert_eq!(&target.wildlife[first..first + 2], &expected);
                assert_eq!(target.wildlife.iter().sum::<f32>(), 1.0);
                assert_eq!(
                    target.beds, [0.0; 5],
                    "quiet countries have no creature drone"
                );
                for kind in [
                    WeatherKind::Rain,
                    WeatherKind::Sandstorm,
                    WeatherKind::Blizzard,
                ] {
                    assert_eq!(
                        targets(&country, night, weather(kind, 255)).wildlife,
                        target.wildlife
                    );
                }
            }
        }
    }
    assert_eq!(
        targets(&grass(), 0.5, None).wildlife,
        [0.0, 0.0, 0.0, 0.0, 0.5]
    );
    assert_eq!(targets(&Ambience::default(), 0.5, None).wildlife, [0.0; 5]);
    assert_eq!(
        birds::species_for(&Ambience {
            ground: GroundLook::Snow,
            wooded: false
        }),
        Some(2)
    );
}

#[test]
fn storm_winds_scale_independently_and_blizzard_keeps_the_existing_snowfall() {
    for (kind, wind) in [(WeatherKind::Sandstorm, 3), (WeatherKind::Blizzard, 4)] {
        let zero = targets(&grass(), 0.0, weather(kind, 0));
        let light = targets(&grass(), 0.0, weather(kind, 64));
        let heavy = targets(&grass(), 0.0, weather(kind, 255));
        assert_eq!(zero.beds[wind], 0.0);
        assert!(light.beds[wind] > 0.0 && light.beds[wind] < heavy.beds[wind]);
        assert_eq!(heavy.beds[wind], 1.0);
        assert_eq!(heavy.beds[7 - wind], 0.0);
        assert_eq!(
            heavy.beds[2],
            f32::from(u8::from(kind == WeatherKind::Blizzard))
        );
        // The weather is already authoritative; wind does not wait for a ground vote.
        assert_eq!(
            targets(&Ambience::default(), 0.0, weather(kind, 255)).beds,
            heavy.beds
        );
    }
}

#[test]
fn new_descriptions_are_audible_distinct_seeded_and_have_silent_edges() {
    for rate in [8000, 48000, 192000] {
        let mut signatures = Vec::new();
        for call in CALLS.into_iter().chain([sounds::Call::Parrot]) {
            let render = |seed| call.bake(seed, rate).unwrap();
            let first = render(7);
            let samples = first.samples();
            assert!(samples.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
            assert!(samples.iter().any(|v| v.abs() > 0.02));
            assert_eq!(samples[0], 0.0);
            assert_eq!(*samples.last().unwrap(), 0.0);
            assert_eq!(samples, render(7).samples());
            assert_ne!(samples, render(29).samples());
            // Half a second, or the whole call when it is shorter — a cricket's is 0.45 s.
            let signature = samples[..(rate as usize / 2).min(samples.len())].to_vec();
            assert!(signatures.iter().all(|other| other != &signature));
            signatures.push(signature);
        }
        let wind = |bed: Bed| {
            let mut stream = bed.description().continuous(rate, 11).unwrap();
            let mut samples = vec![0.0; rate as usize];
            stream.render(&mut samples);
            samples
        };
        let sand = wind(Bed::Sandstorm);
        let snow = wind(Bed::Blizzard);
        assert_ne!(sand, snow);
        let roughness = |samples: &[f32]| {
            samples
                .windows(2)
                .map(|v| (v[1] - v[0]).powi(2))
                .sum::<f32>()
                / samples.iter().map(|v| v * v).sum::<f32>()
        };
        assert!(
            roughness(&sand) > roughness(&snow) * 1.2,
            "sandy grit differs from the blizzard's lower whistle"
        );
    }
}

// Exercise the existing scheduler with the shipped profiles over ten minutes. Record
// actual starts/positions from its callbacks, not a second copy of its random arithmetic.
fn wildlife_sequence(
    call: sounds::Call,
    seed: u64,
    bus: f32,
    duck: f32,
) -> (Vec<(usize, u64, Vec3)>, Vec<f32>) {
    use std::cell::Cell;
    let mixer = mixer();
    mixer.shared_for_test().set_gain(Bus::Ambience, bus);
    mixer.shared_for_test().set_duck(duck);
    let mut calls = Calls::default();
    let profile = call.profile();
    let mut starts = Vec::new();
    let mut levels = Vec::new();
    let position = Cell::new(Vec3::ZERO);
    let origin = Vec3::new(17.0, 80.0, -23.0);
    for tick in 0..6000 {
        calls.update(
            &mixer,
            CallFrame {
                dt: 0.1,
                seed,
                interval: profile.interval,
                radius: profile.radius,
                height: profile.height,
                origin,
                gain: 1.0,
            },
            |source| {
                position.set(source);
                spatial::place(origin, 0.0, source, profile.range, 0.0)
            },
            |seed, rate| {
                starts.push((tick, seed, position.get()));
                call.bake(seed, rate)
            },
        );
        levels.push(energy(&mixer, 800));
    }
    for (_, _, position) in &starts {
        assert!(((position - origin).xz().length() - profile.radius).abs() < 0.001);
        assert!((position.y - origin.y - profile.height).abs() < 0.001);
    }
    (starts, levels)
}

#[test]
fn shipped_calls_are_sparse_irregular_reproducible_and_world_placed() {
    for call in CALLS.into_iter().chain([sounds::Call::Parrot]) {
        let first = wildlife_sequence(call, 17, 1.0, 1.0);
        assert_eq!(first, wildlife_sequence(call, 17, 1.0, 1.0));
        assert_ne!(first.0, wildlife_sequence(call, 39, 1.0, 1.0).0);
        assert!(first.0.len() >= 7, "enough events to observe irregularity");
        let intervals: Vec<_> = first.0.windows(2).map(|v| v[1].0 - v[0].0).collect();
        assert!(intervals.iter().any(|v| *v != intervals[0]));
        assert!(first.0.windows(2).any(|v| v[0].2 != v[1].2));
        let profile = call.profile();
        assert!(intervals.iter().all(|v| {
            let seconds = *v as f32 * 0.1;
            seconds >= profile.interval[0] && seconds <= profile.interval[1] + 0.2
        }));
        assert!(first.1.iter().any(|energy| *energy > 0.00001));
        assert!(
            first.1.iter().filter(|energy| **energy == 0.0).count() > 5100,
            "at least 85% of each country's lane is silence"
        );
    }
}

#[test]
fn wildlife_uses_ambience_gain_and_ducking() {
    let audible = wildlife_sequence(sounds::Call::Wolf, 17, 1.0, 1.0)
        .1
        .iter()
        .sum::<f32>();
    assert!(audible > 0.0);
    assert_eq!(
        wildlife_sequence(sounds::Call::Wolf, 17, 0.0, 1.0)
            .1
            .iter()
            .sum::<f32>(),
        0.0
    );
    let ducked = wildlife_sequence(sounds::Call::Wolf, 17, 1.0, 0.2)
        .1
        .iter()
        .sum::<f32>();
    assert!(ducked > 0.0 && ducked < audible * 0.1);
}

#[test]
fn crossing_countries_fades_outgoing_calls_while_incoming_calls_rise() {
    let mixer = mixer();
    let shared = mixer.shared_for_test().clone();
    let mut app = App::new();
    app.insert_resource(mixer)
        .insert_resource(Time::<()>::default())
        .insert_resource(Ambience {
            ground: GroundLook::Sand,
            wooded: false,
        })
        .insert_resource(session())
        .init_resource::<Weather>()
        .init_resource::<SkyClock>()
        .init_resource::<ChunkStore>();
    register(&mut app);
    let camera = app
        .world_mut()
        .spawn((WorldCamera, Transform::default()))
        .id();
    let step = |app: &mut App| {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(100));
        app.update();
        energy(&AudioMixer::from_shared_for_test(shared.clone()), 800);
    };
    for _ in 0..120 {
        step(&mut app);
    }
    assert!(app.world().resource::<Country>().wildlife_gains[0] > 0.99);
    app.world_mut().resource_mut::<Ambience>().ground = GroundLook::Snow;
    step(&mut app);
    let gains = app.world().resource::<Country>().wildlife_gains;
    assert!(gains[0] > 0.9 && gains[0] < 1.0);
    assert!(gains[2] > 0.0 && gains[2] < 0.1);
    for _ in 0..120 {
        step(&mut app);
    }
    let gains = app.world().resource::<Country>().wildlife_gains;
    assert!(gains[0] < 0.01 && gains[2] > 0.99);
    app.world_mut().despawn(camera);
    app.update();
    assert_eq!(app.world().resource::<Country>().wildlife_gains, [0.0; 5]);
    energy(&AudioMixer::from_shared_for_test(shared.clone()), 12000);
    assert_eq!(energy(&AudioMixer::from_shared_for_test(shared), 800), 0.0);
}
