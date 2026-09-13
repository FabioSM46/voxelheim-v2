use super::*;
use crate::audio::{Bus, MAX_SOURCES, Mixer, Sink, VOICE_RESERVE};
use crate::net::{ChunkCoord, SessionParams, WorldClock};
use crate::player::ambience::GroundLook;
use crate::player::birds;
use crate::player::sky::{PERIOD_SWITCH, Period};
use crate::world::{VoxelChunk, palette};
use sounds::Call;
use std::sync::Arc;
use wildlife::{Habitat, MOUSE, OWLS, Origin, PARROT, SQUIRREL, VULTURE, Voice, row_of};

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
/// Bare sand: the country the rattlesnake and the condor share by day and nothing holds at
/// night. Treeless, because the desert's two gates — the ground and `BIRDS[VULTURE]` — both
/// answer the same whether or not there are trees.
fn sand() -> Ambience {
    Ambience {
        ground: GroundLook::Sand,
        wooded: false,
    }
}
fn weather(kind: WeatherKind, intensity: u8) -> Option<WeatherState> {
    Some(WeatherState { kind, intensity })
}
/// How loudly one creature is heard, named by the creature rather than by its lane: the
/// tests ask about the cricket, and which row the cricket is in is the table's business.
fn gain_of(target: &Targets, call: Call) -> f32 {
    target.wildlife[row_of(call)]
}

#[test]
fn sky_curve_crossfades_and_ground_alone_selects_green_country() {
    let day = targets(&grass(), 0.0, None);
    let dusk = targets(&grass(), 0.5, None);
    let night = targets(&grass(), 1.0, None);
    assert_eq!(
        (gain_of(&day, Call::Parrot), gain_of(&day, Call::Cricket)),
        (1.0, 0.0)
    );
    assert_eq!(
        (gain_of(&dusk, Call::Parrot), gain_of(&dusk, Call::Cricket)),
        (0.5, 0.5)
    );
    assert_eq!(
        (
            gain_of(&night, Call::Parrot),
            gain_of(&night, Call::Cricket)
        ),
        (0.0, 1.0)
    );
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
        assert_eq!(gain_of(&v, Call::Parrot), 0.0);
        assert_eq!(gain_of(&v, Call::Cricket), 0.0);
        assert_eq!(v.beds, [0.0; 5]);
    }
    let plain = Ambience {
        wooded: false,
        ..grass()
    };
    assert!(birds::BIRDS[PARROT].flies_over(&grass()));
    // No biome, temperature, terrain seed or gameplay facts enter this selector. Trees decide
    // only whether the macaw is there to be heard.
    //
    // That used to be assertable as "the two countries have identical wildlife", because the
    // macaw's gain was a field of its own and the array held only ground-gated lanes. It is
    // now a row like any other, so the same claim is made the only way it still can be: the
    // beds are equal, and the macaw's is the one and only lane the trees move.
    let wood = targets(&grass(), 0.3, weather(WeatherKind::Rain, 120));
    let plain = targets(&plain, 0.3, weather(WeatherKind::Rain, 120));
    assert_eq!(wood.beds, plain.beds);
    let moved: Vec<Call> = WILDLIFE
        .iter()
        .filter(|voice| gain_of(&wood, voice.call) != gain_of(&plain, voice.call))
        .map(|voice| voice.call)
        .collect();
    // Three lanes the trees move, not one: the macaw in the canopy, the squirrel under it
    // (#1190) and the wood's own owl after dark (#1191). All three read the same
    // `Ambience::wooded` through the table that draws them, which is the whole of what trees
    // decide here — no bed moves, and no other country's voice does.
    assert_eq!(moved, vec![Call::Parrot, Call::Squirrel, Call::Owl]);
    // At `night = 0.3` the two day voices carry 0.7 and the night one 0.3, and the plain has
    // none of them at all.
    for call in [Call::Parrot, Call::Squirrel] {
        assert_eq!(
            (gain_of(&wood, call), gain_of(&plain, call)),
            (0.7, 0.0),
            "{call:?}"
        );
    }
    assert_eq!(
        (gain_of(&wood, Call::Owl), gain_of(&plain, Call::Owl)),
        (0.3, 0.0)
    );
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
            // The row named, asked directly. It was `species_for(..) == Some(PARROT)`, a
            // first-match query that #1191 made unanswerable for a second row over one
            // country — see `Habitat::Flock`.
            let macaw = birds::BIRDS[PARROT].flies_over(&country);
            heard += usize::from(macaw);
            for night in [0.0, 0.25, 1.0] {
                let expected = if macaw { 1.0 - night } else { 0.0 };
                for weather in [None, weather(WeatherKind::Rain, 200)] {
                    assert_eq!(
                        gain_of(&targets(&country, night, weather), Call::Parrot),
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
        gain_of(&targets(&plain, 0.0, None), Call::Parrot),
        0.0,
        "an open plain is silent"
    );
    assert_eq!(gain_of(&targets(&grass(), 0.0, None), Call::Parrot), 1.0);
    assert_eq!(
        gain_of(&targets(&grass(), 1.0, None), Call::Parrot),
        0.0,
        "no macaw at night"
    );
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

/// #1176: one call every 0.7 to 2.5 s was close to a continuous bed. A day in the wood now
/// hears a few calls a minute, and most of every minute is silence; an open plain hears
/// nothing at all.
///
/// **Two voices share that wood since #1190**, so the rate this measures is their sum: the
/// macaw calls every 8 to 22 s and the squirrel every 11 to 28, which is about four and about
/// three a minute, less whatever the counter merges when the two overlap. The band is widened
/// to admit it rather than the test narrowed to the macaw, because what this test is actually
/// for is the #1176 property — that a wood is mostly silent — and that is a claim about
/// everything audible in it rather than about one lane.
#[test]
fn a_simulated_day_in_a_wood_hears_a_few_calls_a_minute_and_a_plain_hears_none() {
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
        (2.0..=9.0).contains(&per_minute),
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
/// A session whose server declares a day, so `sky::night_now` answers something other than
/// `None`. The numbers are `player/tests.rs`'s: a 24,000-tick day whose night runs from
/// 14,400 to 21,600.
fn session_with_a_clock() -> Session {
    let mut session = session();
    session.0.clock = WorldClock {
        day_length_ticks: 24_000,
        night_start_ticks: 14_400,
        night_end_ticks: 21_600,
    };
    session
}

/// Ten minutes of the real system over one country at one half of the day: the ambience, the
/// clock, every lane of [`WILDLIFE`] and each profile together. Returns each tick's energy.
///
/// The clock is anchored by hand rather than delivered as a snapshot — this module owns no net
/// thread, and `sky::night_now` reads the anchor either way. Deep night and midday are each
/// thousands of ticks from the nearest twilight, so the real seconds the six thousand updates
/// take (twenty ticks each) cannot walk the hour into the other half.
fn a_simulated_day_at(ambience: Ambience, night: bool) -> Vec<f32> {
    let mixer = mixer();
    let shared = mixer.shared_for_test().clone();
    let mut clock = SkyClock::default();
    clock.anchor(
        if night { 18_000 } else { 7_200 },
        std::time::Instant::now(),
    );
    let mut app = App::new();
    app.insert_resource(mixer)
        .insert_resource(Time::<()>::default())
        .insert_resource(ambience)
        .insert_resource(session_with_a_clock())
        .init_resource::<Weather>()
        .insert_resource(clock)
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
    let macaw = row_of(Call::Parrot);
    assert_eq!(app.world().resource::<Country>().wildlife_gains[macaw], 0.0);
    app.insert_resource(session());
    for _ in 0..20 {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(100));
        app.update();
        energy(&AudioMixer::from_shared_for_test(shared.clone()), 800);
    }
    assert!(app.world().resource::<Country>().wildlife_gains[macaw] > 0.5);
    app.world_mut().despawn(camera);
    app.update();
    assert_eq!(app.world().resource::<Country>().wildlife_gains[macaw], 0.0);
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
    assert_eq!(
        (
            WILDLIFE[row_of(Call::Cricket)].habitat,
            WILDLIFE[row_of(Call::Cricket)].period
        ),
        (Habitat::Ground(GroundLook::Grass), Period::Night),
        "the cricket's row moved"
    );
    for night in [0.0, 0.25, 1.0] {
        for wooded in [false, true] {
            let green = Ambience {
                ground: GroundLook::Grass,
                wooded,
            };
            assert_eq!(gain_of(&targets(&green, night, None), Call::Cricket), night);
            for kind in [WeatherKind::Rain, WeatherKind::Snow, WeatherKind::Blizzard] {
                assert_eq!(
                    gain_of(&targets(&green, night, weather(kind, 255)), Call::Cricket),
                    night
                );
            }
        }
        for ground in [GroundLook::Sand, GroundLook::Snow, GroundLook::Unknown] {
            let country = Ambience {
                ground,
                wooded: true,
            };
            assert_eq!(gain_of(&targets(&country, night, None), Call::Cricket), 0.0);
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
    let unmoved_by_weather = |country: &Ambience, night: f32, wildlife: [f32; VOICES]| {
        for kind in [
            WeatherKind::Rain,
            WeatherKind::Sandstorm,
            WeatherKind::Blizzard,
        ] {
            assert_eq!(
                targets(country, night, weather(kind, 255)).wildlife,
                wildlife
            );
        }
    };
    for wooded in [false, true] {
        // Snow is the country whose two halves still hand one voice over to one voice.
        let snow = Ambience {
            ground: GroundLook::Snow,
            wooded,
        };
        for (night, expected) in [(0.0, [1.0, 0.0]), (0.5, [0.5, 0.5]), (1.0, [0.0, 1.0])] {
            let target = targets(&snow, night, None);
            assert_eq!(
                [gain_of(&target, Call::Eagle), gain_of(&target, Call::Wolf)],
                expected
            );
            // **The two ground voices still hand over, and the total is no longer one.**
            // #1191 put an owl in the northern night as well, so the crossfading pair sums to
            // one and the owl arrives on top of it. The pair is the claim above; this is the
            // whole table, and the owl's own share is what accounts for the difference.
            let owl = gain_of(&target, Call::Owl);
            assert_eq!(owl, night, "the north's owl follows the night curve");
            assert_eq!(target.wildlife.iter().sum::<f32>(), 1.0 + owl);
            assert_eq!(
                target.beds, [0.0; 5],
                "quiet countries have no creature drone"
            );
            unmoved_by_weather(&snow, night, target.wildlife);
        }
        // Sand no longer fits that shape and #1186 is why: its day has **two** voices — the
        // rattlesnake on the ground and the condor in the vulture's air — and its night has
        // one, the mouse (#1192), where the crow used to be. So the total is the claim: two
        // day voices crossing one night voice.
        let sand = Ambience {
            ground: GroundLook::Sand,
            wooded,
        };
        for (night, by_day) in [(0.0, 1.0), (0.5, 0.5), (1.0, 0.0)] {
            let target = targets(&sand, night, None);
            assert_eq!(
                [
                    gain_of(&target, Call::Condor),
                    gain_of(&target, Call::Rattlesnake)
                ],
                [by_day; 2],
                "both of the desert's day voices follow the same half of the day"
            );
            assert_eq!(
                gain_of(&target, Call::Mouse),
                1.0 - by_day,
                "the mouse follows the night curve"
            );
            assert_eq!(
                target.wildlife.iter().sum::<f32>(),
                by_day * 2.0 + (1.0 - by_day),
                "the desert is its two day voices and its one night voice, and nothing else"
            );
            assert_eq!(
                target.beds, [0.0; 5],
                "quiet countries have no creature drone"
            );
            unmoved_by_weather(&sand, night, target.wildlife);
        }
        let mut night = [0.0; VOICES];
        night[row_of(Call::Mouse)] = 1.0;
        assert_eq!(
            targets(&sand, 1.0, None).wildlife,
            night,
            "the desert night is the mouse's alone, not quietly crowed at"
        );
    }
    // Wooded grass at dusk is the cell where the day hands over to the night, and it is now
    // the busiest cell in the table: the macaw and the squirrel going quiet together as the
    // cricket and the owl come up, each at half, and no other country's creature sounding at
    // all. The sum is one per *period* rather than one outright, which is what `Period::share`
    // promises — two day voices and two night voices crossing at dusk is 2.0, where it was 1.0
    // when the wood had one of each.
    let dusk = targets(&grass(), 0.5, None);
    assert_eq!(
        (
            gain_of(&dusk, Call::Parrot),
            gain_of(&dusk, Call::Squirrel),
            gain_of(&dusk, Call::Cricket)
        ),
        (0.5, 0.5, 0.5)
    );
    assert_eq!(gain_of(&dusk, Call::Owl), 0.5);
    assert_eq!(dusk.wildlife.iter().sum::<f32>(), 2.0);
    assert_eq!(
        targets(&Ambience::default(), 0.5, None).wildlife,
        [0.0; VOICES]
    );
    // By day, because snow has two rows since #1191 and only the hour tells them apart.
    assert_eq!(
        birds::species_now(
            &Ambience {
                ground: GroundLook::Snow,
                wooded: false
            },
            Some(0.0)
        ),
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
        for call in WILDLIFE.iter().map(|voice| voice.call) {
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
    for call in WILDLIFE.iter().map(|voice| voice.call) {
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
    let (sand, snow) = (row_of(Call::Rattlesnake), row_of(Call::Eagle));
    assert!(app.world().resource::<Country>().wildlife_gains[sand] > 0.99);
    app.world_mut().resource_mut::<Ambience>().ground = GroundLook::Snow;
    step(&mut app);
    let gains = app.world().resource::<Country>().wildlife_gains;
    assert!(gains[sand] > 0.9 && gains[sand] < 1.0);
    assert!(gains[snow] > 0.0 && gains[snow] < 0.1);
    for _ in 0..120 {
        step(&mut app);
    }
    let gains = app.world().resource::<Country>().wildlife_gains;
    assert!(gains[sand] < 0.01 && gains[snow] > 0.99);
    app.world_mut().despawn(camera);
    app.update();
    assert_eq!(
        app.world().resource::<Country>().wildlife_gains,
        [0.0; VOICES]
    );
    energy(&AudioMixer::from_shared_for_test(shared.clone()), 12000);
    assert_eq!(energy(&AudioMixer::from_shared_for_test(shared), 800), 0.0);
}

/// The table is the whole of the country × hour mapping, so the mapping is what is asserted:
/// every ground look the client can answer, in both halves of the day, against the creature
/// that belongs there and silence everywhere else.
#[test]
fn the_table_answers_every_country_and_half_of_the_day() {
    let expected = |ground, wooded, night: f32| -> Vec<Call> {
        match (ground, wooded, night >= PERIOD_SWITCH) {
            // In the table's order: the condor is a seen-and-heard row and sits above the
            // ground-only ones.
            (GroundLook::Sand, _, false) => vec![Call::Condor, Call::Rattlesnake],
            // #1186 retired the crow and left this cell empty; #1192's mouse fills it, trees
            // or none. The bat that shares the night (#1193) is silent and has no row.
            (GroundLook::Sand, _, true) => vec![Call::Mouse],
            (GroundLook::Snow, _, false) => vec![Call::Eagle],
            // The north at night is the owl and the wolf together: one is seen and heard and
            // claims first, which is the whole of why the table is ordered as it is.
            (GroundLook::Snow, _, true) => vec![Call::Owl, Call::Wolf],
            // Wooded grass is the cell with seen-and-heard species in it, and there are two:
            // the macaw in the canopy by day and the squirrel under it (#1190), with the owl
            // taking over after dark (#1191). In table order, which is claim order.
            (GroundLook::Grass, true, false) => vec![Call::Parrot, Call::Squirrel],
            (GroundLook::Grass, false, false) => vec![],
            (GroundLook::Grass, true, true) => vec![Call::Owl, Call::Cricket],
            (GroundLook::Grass, false, true) => vec![Call::Cricket],
            // Not enough loaded evidence is silence, never a default creature.
            (GroundLook::Unknown, _, _) => vec![],
        }
    };
    for ground in [
        GroundLook::Grass,
        GroundLook::Sand,
        GroundLook::Snow,
        GroundLook::Unknown,
    ] {
        for wooded in [false, true] {
            for night in [0.0, 1.0] {
                let target = targets(&Ambience { ground, wooded }, night, None);
                let sounding: Vec<Call> = WILDLIFE
                    .iter()
                    .zip(target.wildlife)
                    .filter(|(_, gain)| *gain > 0.0)
                    .map(|(voice, _)| voice.call)
                    .collect();
                assert_eq!(
                    sounding,
                    expected(ground, wooded, night),
                    "{ground:?}, wooded {wooded}, night {night}"
                );
            }
        }
    }
}

/// #1186: the condor is the griffon vulture's voice, so its row is gated on the bird table
/// rather than on the ground — the choice the issue left open, made here and asserted here.
///
/// **The two gates coincide today and the test says so rather than hiding it**: `BIRDS[VULTURE]`
/// requires sand and no woodland, which is exactly `Habitat::Ground(Sand)`, so the declaration
/// buys no behaviour now. What it buys is that it cannot come apart later — if that row ever
/// gains a condition, the call follows the bird instead of the sound lane keeping its own
/// opinion about where the species lives, which is what #1176 cost.
#[test]
fn the_condor_is_heard_by_day_only_where_the_bird_table_flies_the_vulture() {
    let row = &birds::BIRDS[VULTURE];
    assert!(
        row.ground == GroundLook::Sand && !row.requires_wooded,
        "VULTURE no longer names the griffon vulture's row"
    );
    assert_eq!(
        WILDLIFE[row_of(Call::Condor)].habitat,
        Habitat::Flock(&[VULTURE]),
        "the condor's voice is gated on the bird table, not on the ground"
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
            // The row named, asked directly. It was `species_for(..) == Some(VULTURE)`, a
            // first-match query #1191 made unanswerable for a second row over one country —
            // see `Habitat::Flock`. The claim is unchanged: this is still exactly "does the
            // bird table fly the vulture here".
            let vulture = birds::BIRDS[VULTURE].flies_over(&country);
            heard += usize::from(vulture);
            // The gates coincide: where the flock flies is where the ground is sand.
            assert_eq!(vulture, ground == GroundLook::Sand);
            for night in [0.0, 0.25, 1.0] {
                let expected = if vulture { 1.0 - night } else { 0.0 };
                for weather in [None, weather(WeatherKind::Sandstorm, 255)] {
                    assert_eq!(
                        gain_of(&targets(&country, night, weather), Call::Condor),
                        expected,
                        "{ground:?}, wooded {wooded}, night {night}"
                    );
                }
            }
        }
    }
    assert_eq!(heard, 2, "sand is the vulture's, trees or none");
    assert_eq!(
        gain_of(&targets(&sand(), 1.0, None), Call::Condor),
        0.0,
        "no condor at night"
    );
}

/// Ten minutes of the real system over a desert at night: the mouse's lane, and nothing else.
/// **A desert night is mostly silent** (#1192), so what is asserted is a few squeaks rather
/// than a stream — every row of the table, the clock and the profile together, with no mouse
/// drawn, so the squeaks come from the fallback bearing. The day is asserted beside it so the
/// shape is shown to be the hour rather than a broken system.
#[test]
fn a_simulated_desert_night_hears_a_few_squeaks_and_is_mostly_silent() {
    let night = a_simulated_day_at(sand(), true);
    assert!(
        night.iter().any(|level| *level > 0.0),
        "the desert night had no mouse in it"
    );
    let heard = night.iter().filter(|level| **level > 0.0).count();
    assert!(
        heard < 300,
        "{heard} of 6000 ticks sounded, which is a stream rather than a desert night"
    );
    let day = a_simulated_day_at(sand(), false);
    assert!(
        day.iter().any(|level| *level > 0.0),
        "the desert day is the rattlesnake's and the condor's"
    );
    let silent = day.iter().filter(|level| **level == 0.0).count();
    assert!(silent > 5000, "{silent} of 6000 ticks silent");
}

/// Two properties of the table itself, each of which a new row can break silently.
///
/// A shared stream salt would make two creatures call together on the same bearing; a voice
/// whose habitat is a flock but whose period is not that flock's would be a call from a
/// species that is not in the air.
#[test]
fn every_voice_has_its_own_stream_and_agrees_with_the_flock_it_belongs_to() {
    for (index, voice) in WILDLIFE.iter().enumerate() {
        assert_eq!(
            WILDLIFE
                .iter()
                .filter(|other| other.stream == voice.stream)
                .count(),
            1,
            "{:?} shares its stream salt",
            voice.call
        );
        assert_eq!(row_of(voice.call), index, "{:?} is in two rows", voice.call);
        if let Habitat::Flock(rows) = voice.habitat {
            // Every row named, because one creature may be two of them — the owl is the wood's
            // and the north's. A voice heard when *any* of its rows is grounded would be a
            // call from a species that is not in the air.
            for row in rows {
                assert_eq!(
                    voice.period,
                    birds::BIRDS[*row].flies,
                    "{:?} is heard when its flock is not flying",
                    voice.call
                );
            }
            assert!(!rows.is_empty(), "{:?} names no flock at all", voice.call);
        }
    }
    assert_eq!(
        WILDLIFE[row_of(Call::Parrot)].habitat,
        Habitat::Flock(&[PARROT]),
        "the macaw's voice is gated on the bird table, not on the ground"
    );
}

/// The half of the day is a property of the row, so a species declared nocturnal is silent by
/// day without anything else in the lane knowing it exists. Written against a row this
/// client does not ship — the owl, the bat and the lynx are later issues — because the point
/// is that the mechanism is already there for them. The call it borrows is a stand-in and
/// nothing rests on which one it is: the row — grass at night, gated on the ground — is what
/// this client does not ship, and the assertions below read `gain` and never a sample.
#[test]
fn a_species_declared_nocturnal_is_not_heard_by_day() {
    let owl = Voice {
        call: Call::Eagle,
        habitat: Habitat::Ground(GroundLook::Grass),
        origin: Origin::Bearing,
        period: Period::Night,
        stream: 0xB00,
    };
    let wood = grass();
    assert_eq!(
        owl.gain(&wood, 0.0),
        0.0,
        "a nocturnal voice sounded by day"
    );
    assert_eq!(owl.gain(&wood, 1.0), 1.0);
    assert_eq!(owl.gain(&wood, 0.5), 0.5, "it crossfades over the twilight");
    // And its day-lit neighbour is the exact complement at every hour, so dusk hands over
    // rather than leaving a gap.
    let day = Voice {
        period: Period::Day,
        ..owl
    };
    for night in [0.0, 0.25, 0.5, 0.75, 1.0] {
        assert_eq!(owl.gain(&wood, night) + day.gain(&wood, night), 1.0);
    }
    // An unclamped curve cannot push a gain outside the lane's range.
    assert_eq!(day.gain(&wood, -3.0), 1.0);
    assert_eq!(day.gain(&wood, 7.0), 0.0);
    // Nothing sounds where the habitat is not, whichever half of the day it is.
    let elsewhere = Ambience {
        ground: GroundLook::Sand,
        wooded: false,
    };
    assert_eq!(owl.gain(&elsewhere, 1.0), 0.0);
}

/// The table's order is the order the lanes claim mixer slots in, and slots run out: `Music`,
/// `Sfx` and `Ambience` hold at most `MAX_SOURCES - VOICE_RESERVE` between them, and
/// `Bus::steal_order` lets a claimant steal only *strictly beneath* itself — `Ambience` is the
/// bottom rank, so an ambience lane can never steal from a peer and a call that finds the
/// world's slots full is dropped rather than queued.
///
/// So a creature the player can see claims before one nobody can, which is both the order
/// that shipped and the one the origin rule argues for: a voice falling silent while its
/// animal is on screen is the worse failure. Wooded grass at dusk is where it is reachable —
/// the macaw and the cricket both sit at half.
#[test]
fn a_creature_that_can_be_seen_claims_its_slot_before_one_that_cannot() {
    // **Both habitats that name a creature the eye can find**, not just the flock. A squirrel
    // is drawn where the player can walk up to it, so a `Critter` row is as much a
    // seen-and-heard row as a `Flock` one — and reading this predicate as "flock" would let a
    // critter row sink below the ground rows while the test went on passing, which is the one
    // outcome the claim order exists to prevent.
    let seen = |voice: &Voice| matches!(voice.habitat, Habitat::Flock(_) | Habitat::Critter(_));
    let last_seen = WILDLIFE.iter().rposition(seen);
    let first_unseen = WILDLIFE.iter().position(|voice| !seen(voice));
    assert!(
        last_seen < first_unseen,
        "a seen-and-heard row sits below a ground-only one, so it now loses a contested slot"
    );
    // Ambience cannot take a slot from ambience, which is why the order decides it at all.
    assert!(Bus::Ambience.steal_order() < Bus::Sfx.steal_order());
    assert_eq!(Bus::Ambience.steal_order(), Some(0));
    // Both of them sound at once in exactly one cell, which is the cell that made it matter.
    let dusk = targets(&grass(), 0.5, None);
    assert_eq!(
        (gain_of(&dusk, Call::Parrot), gain_of(&dusk, Call::Cricket)),
        (0.5, 0.5)
    );
    // And the macaw is still the first lane updated, exactly as it was when it had a lane of
    // its own ahead of the five ground ones: no contested slot changed hands.
    assert_eq!(row_of(Call::Parrot), 0);
    // The squirrel sits under it and above every ground row. Its index is deliberately *not*
    // pinned to a literal — #1186 added the condor between the two while this branch was
    // open, and a literal here would have to be edited by every later seen-and-heard row for
    // no gain. What must hold is the relation: below the macaw, above anything unseen.
    assert!(row_of(Call::Parrot) < row_of(Call::Squirrel));
    assert!(Some(row_of(Call::Squirrel)) < first_unseen);
    // Not vacuous: there is a seen row and an unseen one for the order to be about.
    assert!(WILDLIFE.iter().any(seen) && WILDLIFE.iter().any(|voice| !seen(voice)));
}

/// The origin rule in `wildlife.rs`, as the two answers it gives.
///
/// **A voice whose creature is drawn comes from that creature; one whose creature is not keeps
/// its bearing.** Which of the two applies is a property of the creature rather than of the
/// frame, so this is asserted on the habitat rather than on a rendered frame: `Habitat::body`
/// is the whole of the decision, and the lane does nothing with its answer but place a sound.
#[test]
fn the_lane_places_a_drawn_creature_at_its_body_and_everything_else_on_its_circle() {
    // `body` is tested in isolation below; this covers what the lane *does* with the answer,
    // which nothing exercised before the review of #1221. A swapped pair of arms, or a radius
    // left un-zeroed, would have passed every test in this file.
    let eye = Vec3::new(10.0, 64.0, 10.0);
    // The cricket's circle, because a voice that is placed at its body may legitimately carry
    // no circle at all — and a zero one would make the second half of this test vacuous.
    let profile = Call::Cricket.profile();
    assert!(
        profile.radius > 0.0 && profile.height != 0.0,
        "the circle must be a real one or this test proves nothing"
    );

    // Drawn: at the animal, and the circle collapses to nothing so the sound does not scatter
    // away from the creature the player is looking at.
    let body = eye + Vec3::new(3.0, 0.0, -2.0);
    assert_eq!(
        super::voice_placement(
            Origin::Creature,
            Habitat::Critter(SQUIRREL),
            &[],
            &[(SQUIRREL, body)],
            eye,
            &profile
        ),
        (body, 0.0, 0.0)
    );

    // Not drawn: the row's bearing circle about the listener, at the row's own numbers.
    assert_eq!(
        super::voice_placement(
            Origin::Creature,
            Habitat::Critter(SQUIRREL),
            &[],
            &[],
            eye,
            &profile
        ),
        (eye, profile.radius, profile.height)
    );

    // A voice that belongs to no visible creature keeps the circle even with bodies drawn.
    let cricket = WILDLIFE
        .iter()
        .find(|voice| voice.call == Call::Cricket)
        .expect("the cricket is a row");
    assert_eq!(
        super::voice_placement(
            cricket.origin,
            cricket.habitat,
            &[],
            &[(SQUIRREL, body)],
            eye,
            &profile
        ),
        (eye, profile.radius, profile.height)
    );
}

#[test]
fn a_voice_that_belongs_to_a_visible_creature_is_placed_at_it() {
    let eye = Vec3::new(10.0, 64.0, 10.0);
    let squirrel = Habitat::Critter(SQUIRREL);

    // Nothing drawn: no body, so the lane falls back to the bearing. Silence is never the
    // fallback, which is why this answers `None` rather than suppressing the voice.
    assert_eq!(squirrel.body(&[], &[], eye), None);
    // A critter of another row is not this row's body.
    assert_eq!(squirrel.body(&[], &[(1, Vec3::ZERO)], eye), None);

    // Drawn: the body, and the *nearest* one when several are, because the nearest is the one
    // the player is most likely to be looking at.
    let near = eye + Vec3::new(3.0, 0.0, 0.0);
    let far = eye + Vec3::new(-20.0, 0.0, 12.0);
    assert_eq!(squirrel.body(&[], &[(SQUIRREL, near)], eye), Some(near));
    assert_eq!(
        squirrel.body(&[], &[(SQUIRREL, far), (SQUIRREL, near)], eye),
        Some(near),
        "the far squirrel spoke over the near one"
    );
    // And it is nearest to the *eye* rather than to the origin, which a bare `length()` would
    // get wrong for any player who has walked away from 0, 0. The vantage is chosen so the two
    // orderings disagree: from here `far` is the closer of the two, while measured from the
    // origin it is the further — so a comparison on length alone answers `near` and puts the
    // chatter in the wrong squirrel.
    let vantage = Vec3::new(-12.0, 64.0, 20.0);
    assert!(
        far.length() > near.length(),
        "the two squirrels order the same way from the origin, so this proves nothing"
    );
    assert_eq!(
        squirrel.body(&[], &[(SQUIRREL, far), (SQUIRREL, near)], vantage),
        Some(far)
    );

    // Every other habitat keeps its bearing, the macaw included: it is drawn, but moving a
    // shipped sound from a bearing to a body is a change to where it comes from, and
    // `wildlife.rs` has said since the table was written that it belongs to the issue that
    // needs it. Nothing about the macaw's lane moved here, which is what kept its pins intact.
    for habitat in [
        Habitat::Flock(&[PARROT]),
        Habitat::Ground(GroundLook::Grass),
        Habitat::Ground(GroundLook::Sand),
        Habitat::Ground(GroundLook::Snow),
    ] {
        assert_eq!(
            habitat.body(&[], &[(SQUIRREL, near)], eye),
            None,
            "{habitat:?} took a body it does not own"
        );
    }
}

/// The squirrel is heard exactly where it is drawn, and the two tables are what agree on it.
#[test]
fn the_squirrel_is_heard_exactly_where_the_critter_table_stands_it() {
    for ground in [
        GroundLook::Grass,
        GroundLook::Sand,
        GroundLook::Snow,
        GroundLook::Unknown,
    ] {
        for wooded in [false, true] {
            let ambience = Ambience { ground, wooded };
            let heard = gain_of(&targets(&ambience, 0.0, None), Call::Squirrel) > 0.0;
            let drawn = crate::player::critters::species_for(&ambience) == Some(SQUIRREL);
            assert_eq!(
                heard, drawn,
                "{ground:?}/{wooded}: heard {heard}, drawn {drawn}"
            );
        }
    }
    // And it is a day voice: the same wood after dark has the cricket instead.
    let night = targets(&grass(), 1.0, None);
    assert_eq!(gain_of(&night, Call::Squirrel), 0.0);
    assert!(gain_of(&night, Call::Cricket) > 0.0);
}

/// The mouse is heard exactly where it is drawn, and only after dark: the two tables agree on
/// the country, and the row's period on the hour.
#[test]
fn the_mouse_is_heard_exactly_where_the_critter_table_stands_it_and_only_after_dark() {
    for ground in [
        GroundLook::Grass,
        GroundLook::Sand,
        GroundLook::Snow,
        GroundLook::Unknown,
    ] {
        for wooded in [false, true] {
            let ambience = Ambience { ground, wooded };
            let heard = gain_of(&targets(&ambience, 1.0, None), Call::Mouse) > 0.0;
            let drawn = crate::player::critters::species_for(&ambience) == Some(MOUSE);
            assert_eq!(
                heard, drawn,
                "{ground:?}/{wooded}: heard {heard}, drawn {drawn}"
            );
            assert_eq!(
                gain_of(&targets(&ambience, 0.0, None), Call::Mouse),
                0.0,
                "{ground:?}/{wooded}: a mouse squeaked by day"
            );
        }
    }
    assert_eq!(
        WILDLIFE[row_of(Call::Mouse)].period,
        crate::player::critters::CRITTERS[MOUSE].abroad,
        "the squeak is heard in a different half of the day from the one the mouse is abroad in"
    );
}

/// "The squeak comes from the mouse, and is sparse." Driven through the shipped scheduler for
/// ten minutes with a mouse drawn: a few calls a minute, sparser than the cricket, and **every
/// one of them started at the mouse** — which is the lane's own placement answer fed to the
/// scheduler, not a second copy of it.
#[test]
fn a_simulated_desert_night_hears_a_few_squeaks_each_from_the_mouse() {
    use std::cell::{Cell, RefCell};
    let eye = Vec3::new(10.0, 64.0, 10.0);
    let mouse = eye + Vec3::new(4.0, -1.5, -3.0);
    let profile = Call::Mouse.profile();
    let row = &WILDLIFE[row_of(Call::Mouse)];
    // A squirrel drawn nearer the eye, so "the nearest critter" and "the nearest mouse" differ.
    let drawn = [(SQUIRREL, eye + Vec3::X), (MOUSE, mouse)];
    let (origin, radius, height) =
        super::voice_placement(row.origin, row.habitat, &[], &drawn, eye, &profile);
    assert_eq!((origin, radius, height), (mouse, 0.0, 0.0));

    for seed in [17, 39, 1123] {
        let mixer = mixer();
        let mut calls = Calls::default();
        let placed = Cell::new(Vec3::NAN);
        let starts = RefCell::new(Vec::new());
        let mut silent = 0usize;
        for _ in 0..6000 {
            calls.update(
                &mixer,
                CallFrame {
                    dt: 0.1,
                    seed,
                    interval: profile.interval,
                    radius,
                    height,
                    origin,
                    gain: 1.0,
                },
                |source| {
                    placed.set(source);
                    spatial::place(eye, 0.0, source, profile.range, 0.0)
                },
                |seed, rate| {
                    starts.borrow_mut().push(placed.get());
                    Call::Mouse.bake(seed, rate)
                },
            );
            silent += usize::from(energy(&mixer, 800) == 0.0);
        }
        let starts = starts.into_inner();
        let per_minute = starts.len() as f32 / 10.0;
        assert!(
            (0.8..=3.5).contains(&per_minute),
            "seed {seed}: {per_minute} calls a minute, which is not a mostly silent night"
        );
        assert!(
            starts.iter().all(|at| *at == mouse),
            "seed {seed}: a squeak started somewhere other than the mouse: {starts:?}"
        );
        let crickets = wildlife_sequence(Call::Cricket, seed, 1.0, 1.0).0.len();
        assert!(
            starts.len() * 2 < crickets,
            "{} calls against {crickets} cri-cris",
            starts.len()
        );
        assert!(silent > 5800, "seed {seed}: {silent} of 6000 ticks silent");
    }
}

/// #1191: a night has a few hoots, not a chorus. The sparsest call in the table, driven
/// through the shipped scheduler for ten minutes.
#[test]
fn a_simulated_night_hears_a_few_hoots_rather_than_a_chorus() {
    for seed in [17, 39, 1123] {
        let (starts, levels) = wildlife_sequence(sounds::Call::Owl, seed, 1.0, 1.0);
        // Ten minutes of 0.1 s ticks.
        let calls = starts.len() as f32 / 10.0;
        assert!(
            (0.6..=2.2).contains(&calls),
            "{calls} hoots a minute, which is a chorus rather than a night"
        );
        // And it is genuinely the sparsest voice there is: sparser than the cricket, which is
        // the thing an owl must not sound like.
        let crickets = wildlife_sequence(sounds::Call::Cricket, seed, 1.0, 1.0)
            .0
            .len();
        assert!(
            starts.len() * 3 < crickets,
            "{} hoots against {crickets} cri-cris",
            starts.len()
        );
        // Most of the night is silence, and each hoot is heard across many ticks — two notes
        // and the gap between them run to well over a second.
        let silent = levels.iter().filter(|energy| **energy == 0.0).count();
        assert!(silent > 5500, "{silent} of 6000 ticks silent");
        let heard = 6000 - silent;
        assert!(
            heard >= starts.len() * 8,
            "{heard} ticks heard from {} hoots",
            starts.len()
        );
    }
}

/// The origin rule, as the table declares it.
///
/// An `Origin::Creature` row must name a creature the eye can see — a flock or a critter.
/// Declaring it on a ground row would be a promise to place a voice at a body that does not
/// exist, and the fallback would be the only branch ever taken.
///
/// **Both kinds now qualify**, which the name of this test no longer had to be widened for
/// only because it says "flock": #1190 brought the squirrel as a `Critter` row placed at its
/// body and #1191 the owl as a `Flock` row, and the two arrived independently. The count below
/// is the part worth keeping honest — it is not "one" any more, and a row added without
/// thought about where its voice comes from fails here rather than passing quietly.
#[test]
fn at_the_creature_is_only_declared_by_a_row_that_names_one() {
    let mut placed = 0usize;
    for voice in &WILDLIFE {
        if voice.origin == Origin::Creature {
            assert!(
                matches!(voice.habitat, Habitat::Flock(_) | Habitat::Critter(_)),
                "{:?} is placed at a creature it does not name",
                voice.call
            );
            placed += 1;
        }
    }
    assert_eq!(
        placed, 3,
        "the squirrel, the owl and the mouse are placed at their own bodies today"
    );
    assert_eq!(WILDLIFE[row_of(Call::Mouse)].origin, Origin::Creature);
    assert_eq!(WILDLIFE[row_of(Call::Owl)].origin, Origin::Creature);

    // **The macaw keeps the bearing**, which is the rule's own instruction: moving a shipped
    // sound belongs to the issue that adds a creature needing it, and #1191 added one rather
    // than moving the macaw. Its pins are untouched for the same reason.
    assert_eq!(WILDLIFE[row_of(Call::Parrot)].origin, Origin::Bearing);
    assert!(
        WILDLIFE
            .iter()
            .filter(|voice| matches!(voice.habitat, Habitat::Ground(_)))
            .all(|voice| voice.origin == Origin::Bearing),
        "a voice with no body was promised one"
    );
}

/// The owl's voice is one row over two bird rows, and both of them are its own.
#[test]
fn one_owl_voice_answers_for_both_of_the_bird_tables_owl_rows() {
    let owl = &WILDLIFE[row_of(Call::Owl)];
    assert_eq!(owl.habitat, Habitat::Flock(&OWLS));
    assert_eq!(OWLS.len(), 2, "an owl is the wood's row and the north's");
    // Heard in both countries after dark, and in neither by day.
    for ground in [GroundLook::Grass, GroundLook::Snow] {
        let wooded = ground == GroundLook::Grass;
        let country = Ambience { ground, wooded };
        assert_eq!(owl.gain(&country, 1.0), 1.0, "{ground:?} at night");
        assert_eq!(owl.gain(&country, 0.0), 0.0, "{ground:?} by day");
    }
    // And never over sand, at any hour — the desert has no owl row to name.
    let desert = Ambience {
        ground: GroundLook::Sand,
        wooded: false,
    };
    for night in [0.0, 0.5, 1.0] {
        assert_eq!(owl.gain(&desert, night), 0.0);
    }
    // An open plain has no owl either: the wood's row needs trees to sit in.
    let plain = Ambience {
        ground: GroundLook::Grass,
        wooded: false,
    };
    assert_eq!(owl.gain(&plain, 1.0), 0.0);
}

/// **The night's lanes sum, and the sum is bounded — measured at the worst alignment there
/// is, not at whichever one a ten-minute run happened to produce.**
///
/// Raised in review on #1219: the wood at night now carries a cricket at full gain *and* an
/// owl at full gain, where before every cell summed to one crossfading lane. Each call is only
/// asserted to peak below 0.85 on its own and the pins bake each row in isolation, so nothing
/// bounded the combined output.
///
/// **Driving two lanes for ten minutes is the wrong instrument, and that is worth recording.**
/// It was tried first and reported a peak of 0.7699 for both pairs — identical to four decimal
/// places, which is the tell: that is the owl alone, because with a 30–90 s interval against
/// the cricket's 6–16 s the two calls simply never landed on top of each other in the sample
/// taken. A test that passes because the bad case did not occur proves nothing about the bad
/// case.
///
/// So this slides one call across the other and takes the worst alignment, which is an upper
/// bound on anything the scheduler can ever produce. The owl is given the loudest placement it
/// can have — at its own body, distance zero, no attenuation, which is exactly what
/// `Origin::Creature` made reachable — and its partner the attenuation of its own profile.
///
/// **Measured: an owl over a cricket reaches 0.883 and an owl over a wolf 0.690.** Neither
/// clips, and the number worth keeping is the first one: twelve percent of headroom is not
/// much, so a third night voice in either country, a louder hoot, or a shorter fallback radius
/// for the cricket is the change that brings this down — and it fails here when it does,
/// rather than in somebody's headphones.
#[test]
fn the_loudest_alignment_of_two_night_voices_does_not_clip() {
    let rate = 8000;
    // Every pair that can sound together after dark: the owl is heard in the wood and in the
    // north, and each of those countries has a ground voice at night.
    for partner in [Call::Cricket, Call::Wolf] {
        let partner_profile = partner.profile();
        // Its own placement, which is the loudest that voice is ever heard at.
        let partner_gain = spatial::attenuation(
            partner_profile.radius.hypot(partner_profile.height),
            partner_profile.range,
        );
        let mut worst: f32 = 0.0;
        let mut worst_at = 0usize;
        for seed in (0..6u64).map(controller::scramble) {
            // The owl at its own body: distance zero, so attenuation is exactly one.
            assert_eq!(spatial::attenuation(0.0, Call::Owl.profile().range), 1.0);
            let owl = Call::Owl.bake(seed, rate).unwrap();
            let other = partner.bake(seed, rate).unwrap();
            let owl = owl.samples();
            let other = other.samples();
            // Slide the partner across the whole hoot, a millisecond at a time, and take the
            // loudest sample any alignment produces.
            let step = (rate / 1000).max(1) as usize;
            for offset in (0..owl.len()).step_by(step) {
                for (index, value) in other.iter().enumerate() {
                    let Some(under) = owl.get(offset + index) else {
                        break;
                    };
                    let summed = (under + value * partner_gain).abs();
                    if summed > worst {
                        worst = summed;
                        worst_at = offset;
                    }
                }
            }
        }
        // Not vacuous: some offset really did overlap, so `worst` is a sum of two calls rather
        // than the 0.0 the inner loop leaves when the partner sits entirely past the hoot.
        //
        // It used to also require `worst_at < 1000`, which the review of the assembled head
        // found does not guard that at all: the overlap claim is `worst > 0.0` on its own, and
        // the extra conjunct silently demanded that the loudest alignment fall inside the
        // first 125 ms. Nothing makes that true — the hoot's second note is the longer of the
        // two, and were it ever to become the louder one this would fail as a clipping message
        // about a sound that does not clip.
        assert!(worst > 0.0, "the two calls never overlapped: {worst}");
        // The claim. A rendered sample outside [-1, 1] is a clipped one, and the mixer's
        // buses are at unity here, so this bound is the whole of what reaches the device.
        assert!(
            worst <= 1.0,
            "an owl over a {partner:?} reaches {worst} at offset {worst_at}, which clips"
        );
    }
}
