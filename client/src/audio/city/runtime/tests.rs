use super::*;
use crate::{
    audio::{Mixer, mixer::Sink},
    net::{BlockCoord, ChunkCoord, Facing, SessionParams, Snapshot},
    world::{VoxelChunk, palette},
};
use bevy::time::TimeUpdateStrategy;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

struct Buffer(Vec<f32>);
impl Sink for Buffer {
    fn block(&mut self) -> &mut [f32] {
        &mut self.0
    }
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

fn structure(id: u64, kind: StructureKind, x: i32) -> StructureState {
    StructureState {
        structure_id: id,
        kind,
        anchor: BlockCoord { x, y: 3, z: 4 },
        facing: Facing::North,
        owner_entity_id: 0,
        lit: true,
    }
}

fn fixture(structures: Vec<StructureState>, wall: Option<u16>) -> (App, Arc<Mixer>) {
    let mixer = Arc::new(Mixer::new());
    mixer.set_format(8000, 2);
    let mut app = App::new();
    app.add_plugins(bevy::time::TimePlugin)
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            10,
        )))
        .insert_resource(AudioMixer(Arc::clone(&mixer)))
        .insert_resource(session())
        .init_resource::<SnapshotBuffer>();
    let mut store = ChunkStore::default();
    if let Some(block) = wall {
        let mut chunk = VoxelChunk::all_air(32);
        for y in 2..7 {
            for z in 2..7 {
                chunk.set(8, y, z, block);
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
    }
    app.insert_resource(store);
    app.world_mut()
        .spawn((WorldCamera, Transform::from_xyz(4.5, 4.5, 4.5)));
    register(&mut app);
    snapshot(&mut app, structures);
    (app, mixer)
}

fn snapshot(app: &mut App, structures: Vec<StructureState>) {
    let mut buffer = app.world_mut().resource_mut::<SnapshotBuffer>();
    let tick = buffer.latest_tick().unwrap_or(0) + 1;
    assert!(buffer.accept(
        Snapshot {
            server_tick: tick,
            structures,
            ..Snapshot::default()
        },
        Instant::now()
    ));
}

fn hear(app: &mut App, mixer: &Mixer, frames: usize) -> [f32; 2] {
    let mut energy = [0.0; 2];
    for _ in 0..frames {
        app.update();
        let mut buffer = Buffer(vec![0.0; mixer.sample_rate() as usize / 100 * 2]);
        mixer.render(&mut buffer);
        for pair in buffer.0.chunks_exact(2) {
            energy[0] += pair[0] * pair[0];
            energy[1] += pair[1] * pair[1];
        }
    }
    energy
}

fn ids(app: &App) -> Vec<u64> {
    let mut ids: Vec<_> = app
        .world()
        .resource::<City>()
        .live
        .iter()
        .map(|emitter| emitter.candidate.id)
        .collect();
    ids.sort_unstable();
    ids
}

#[test]
fn both_emitters_use_ambience_pan_and_actual_material_occlusion() {
    for kind in [StructureKind::Forge, StructureKind::Campfire] {
        let scene = vec![structure(5, kind, 12)];
        let (mut open, mixer) = fixture(scene.clone(), None);
        let air = hear(&mut open, &mixer, 300);
        assert!(
            air[1] > 0.01 && air[0] < air[1] * 0.01,
            "source is not to the right: {air:?}"
        );
        for mut transform in open
            .world_mut()
            .query_filtered::<&mut Transform, With<WorldCamera>>()
            .iter_mut(open.world_mut())
        {
            transform.rotation = Quat::from_rotation_y(std::f32::consts::PI);
        }
        // A forge can pause for five seconds; observe long enough to include its next burst.
        let turned = hear(&mut open, &mixer, 1200);
        assert!(
            turned[0] > 0.01 && turned[1] < turned[0] * 0.01,
            "turned {kind:?}: {turned:?}"
        );
        mixer.set_gain(Bus::Ambience, 0.0);
        assert_eq!(hear(&mut open, &mixer, 100), [0.0; 2]);
        mixer.set_gain(Bus::Ambience, 1.0);
        assert!(hear(&mut open, &mixer, 1200).iter().sum::<f32>() > 0.01);

        let (mut stone, mixer) = fixture(scene.clone(), Some(palette::STONE));
        let muffled = hear(&mut stone, &mixer, 300);
        let (mut wood, mixer) = fixture(scene, Some(palette::PLANKS));
        let timber = hear(&mut wood, &mixer, 300);
        assert!(
            muffled[1] < timber[1] && timber[1] < air[1],
            "material path was lost: stone {muffled:?}, wood {timber:?}, air {air:?}"
        );
    }
}

#[test]
fn distance_reduces_the_actual_signal_and_range_cancels_queued_samples() {
    for kind in [StructureKind::Forge, StructureKind::Campfire] {
        let (mut near, mixer) = fixture(vec![structure(1, kind, 6)], None);
        let close = hear(&mut near, &mixer, 300)[1];
        let (mut far, mixer) = fixture(vec![structure(1, kind, 14)], None);
        let distant = hear(&mut far, &mixer, 300)[1];
        assert!(close > distant && distant > 0.0);
        snapshot(&mut far, vec![structure(1, kind, 40)]);
        assert_eq!(hear(&mut far, &mixer, 1), [0.0; 2]);
        assert!(ids(&far).is_empty());
    }
}

#[test]
fn the_server_douses_a_fire_and_only_a_lit_campfire_can_restart_it() {
    let fire = structure(1, StructureKind::Campfire, 6);
    let (mut app, mixer) = fixture(vec![fire], None);
    assert!(hear(&mut app, &mixer, 100)[1] > 0.0);
    let mut doused = fire;
    doused.lit = false;
    snapshot(&mut app, vec![doused]);
    assert_eq!(hear(&mut app, &mixer, 30), [0.0; 2]);
    assert!(ids(&app).is_empty());
    snapshot(
        &mut app,
        vec![
            structure(2, StructureKind::Tent, 6),
            structure(3, StructureKind::Runestone, 6),
        ],
    );
    assert_eq!(hear(&mut app, &mixer, 30), [0.0; 2]);
    snapshot(&mut app, vec![fire]);
    assert!(hear(&mut app, &mixer, 100)[1] > 0.0);
    snapshot(&mut app, vec![]);
    assert_eq!(hear(&mut app, &mixer, 1), [0.0; 2]);
}

#[test]
fn nearest_two_of_each_kind_win_even_when_the_snapshot_is_reversed() {
    let mut scene = Vec::new();
    for i in 0..10 {
        scene.push(structure(i + 1, StructureKind::Campfire, 5 + i as i32));
        scene.push(structure(i + 21, StructureKind::Forge, 5 + i as i32));
    }
    scene.reverse();
    let (mut app, mixer) = fixture(scene.clone(), None);
    // Fill the protected side and more: the city can only take the four actually free.
    let voices: Vec<_> = (0..MAX_SOURCES - CITY_SOURCES)
        .map(|_| mixer.claim(Bus::Voice).unwrap())
        .collect();
    hear(&mut app, &mixer, 100);
    assert_eq!(ids(&app), vec![1, 2, 21, 22]);
    assert!(voices.iter().all(SourceHandle::live));
    // Walking through the row releases the old pair before trying the new nearest pair.
    for mut transform in app
        .world_mut()
        .query_filtered::<&mut Transform, With<WorldCamera>>()
        .iter_mut(app.world_mut())
    {
        transform.translation.x = 14.5;
    }
    hear(&mut app, &mixer, 100);
    assert_eq!(ids(&app), vec![9, 10, 29, 30]);
    assert!(voices.iter().all(SourceHandle::live));
    // A higher world bus may revoke city ambience, and its owner releases the slot.
    assert!(mixer.claim(Bus::Sfx).is_none());
    hear(&mut app, &mixer, 1);
    assert!(ids(&app).len() < CITY_SOURCES);
    assert!(voices.iter().all(SourceHandle::live));
    let effect = mixer
        .claim(Bus::Sfx)
        .expect("revoked city slot was returned");
    hear(&mut app, &mixer, 100);
    assert_eq!(
        ids(&app),
        vec![9, 10, 30],
        "the reduced allowance must still hear the nearest three"
    );
    assert!(effect.live());
}

#[test]
fn saturated_voice_refuses_city_without_a_backlog_and_later_claims_are_fresh() {
    let (mut app, mixer) = fixture(vec![structure(1, StructureKind::Campfire, 6)], None);
    let voices: Vec<_> = (0..MAX_SOURCES)
        .map(|_| mixer.claim(Bus::Voice).unwrap())
        .collect();
    assert_eq!(hear(&mut app, &mixer, 100), [0.0; 2]);
    assert!(ids(&app).is_empty());
    assert!(voices.iter().all(SourceHandle::live));
    drop(voices);
    assert!(hear(&mut app, &mixer, 100)[1] > 0.0);
    assert_eq!(ids(&app), vec![1]);
}

#[test]
fn device_rate_change_mutes_old_queued_samples_before_update_and_rebuilds() {
    let (mut app, mixer) = fixture(vec![structure(1, StructureKind::Campfire, 6)], None);
    hear(&mut app, &mixer, 100);
    mixer.set_format(44100, 2);
    let mut buffer = Buffer(vec![0.0; 512]);
    mixer.render(&mut buffer);
    assert!(buffer.0.iter().all(|v| *v == 0.0));
    assert!(hear(&mut app, &mixer, 100)[1] > 0.0);
    assert_eq!(app.world().resource::<City>().live[0].stream.rate, 44100);
}

#[test]
fn camera_snapshot_or_session_loss_returns_every_city_source() {
    for missing in 0..3 {
        let (mut app, mixer) = fixture(vec![structure(1, StructureKind::Campfire, 6)], None);
        hear(&mut app, &mixer, 100);
        match missing {
            0 => {
                let camera = app
                    .world_mut()
                    .query_filtered::<Entity, With<WorldCamera>>()
                    .single(app.world())
                    .unwrap();
                app.world_mut().despawn(camera);
            }
            1 => {
                app.world_mut().remove_resource::<SnapshotBuffer>();
            }
            _ => {
                app.world_mut().remove_resource::<Session>();
            }
        }
        assert_eq!(hear(&mut app, &mixer, 1), [0.0; 2]);
        assert!(ids(&app).is_empty());
        let free: Vec<_> = (0..MAX_SOURCES)
            .filter_map(|_| mixer.claim(Bus::Voice))
            .collect();
        assert_eq!(free.len(), MAX_SOURCES);
    }
}

#[test]
fn a_full_ring_does_not_advance_the_random_sound_clock() {
    let scene = vec![structure(1, StructureKind::Campfire, 6)];
    let (mut a, a_mixer) = fixture(scene.clone(), None);
    let (mut b, b_mixer) = fixture(scene, None);
    a.update();
    b.update();
    // Updates without an output callback are ring backpressure, not elapsed audio.
    for _ in 0..80 {
        a.update();
    }
    for _ in 0..2 {
        let mut a_out = Buffer(vec![0.0; SOURCE_CAPACITY * 2]);
        let mut b_out = Buffer(vec![0.0; SOURCE_CAPACITY * 2]);
        a_mixer.render(&mut a_out);
        b_mixer.render(&mut b_out);
        assert_eq!(a_out.0, b_out.0);
        a.update();
        b.update();
    }
}

#[test]
fn disabling_and_reenabling_the_bus_returns_and_reclaims_a_fresh_source() {
    let (mut app, mixer) = fixture(vec![structure(1, StructureKind::Campfire, 6)], None);
    assert!(hear(&mut app, &mixer, 100)[1] > 0.0);
    mixer.set_enabled(Bus::Ambience, false);
    assert_eq!(hear(&mut app, &mixer, 100), [0.0; 2]);
    assert!(ids(&app).is_empty());
    mixer.set_enabled(Bus::Ambience, true);
    assert!(hear(&mut app, &mixer, 100)[1] > 0.0);
    assert_eq!(ids(&app), vec![1]);
}

#[test]
fn the_forge_curve_at_its_twelve_block_range_is_pinned() {
    assert_eq!(
        carry(Kind::Forge),
        Carry {
            range: 12.0,
            gain: 0.6
        }
    );
    assert_eq!(
        carry(Kind::Fire),
        Carry {
            range: 12.0,
            gain: 1.0
        }
    );
    // Flat to two blocks, then inverse distance to exactly zero at the range.
    for (distance, expected) in [
        (0.0, 1.0),
        (2.0, 1.0),
        (3.0, 0.6),
        (4.0, 0.4),
        (6.0, 0.2),
        (8.0, 0.1),
        (10.0, 0.04),
        (12.0, 0.0),
        (28.0, 0.0),
    ] {
        let gain = spatial::attenuation(distance, FORGE_RANGE);
        assert!((gain - expected).abs() < 1e-5, "{distance} blocks: {gain}");
    }
}

#[test]
fn a_forge_at_eight_blocks_is_clearly_quieter_than_at_two_and_silent_at_twelve() {
    // The eye stands at x = 4.5 and a structure at x sounds from x + 0.5, so 6, 12 and 16
    // put the anvil 2, 8 and 12 blocks to the right. One id keeps one strike rhythm.
    let heard = |x| {
        let (mut app, mixer) = fixture(vec![structure(1, StructureKind::Forge, x)], None);
        (hear(&mut app, &mixer, 1200)[1], ids(&app))
    };
    let (two, _) = heard(6);
    let (eight, _) = heard(12);
    let (twelve, live) = heard(16);
    assert!(eight > 0.0 && eight < two * 0.02, "2: {two}, 8: {eight}");
    assert_eq!(twelve, 0.0);
    assert!(live.is_empty());
}

#[test]
fn a_village_forge_across_the_settlement_is_silent_from_its_edge() {
    // Settlement stations belong to nobody and arrive in the same snapshot rows as placed
    // ones. From the edge of a 28-block village, the near hut ring (16 from the centre) is
    // 12 blocks in, the centre 28 and the far hut ring 44.
    let village = |id, x| StructureState {
        owner_entity_id: 0,
        ..structure(id, StructureKind::Forge, x)
    };
    let (mut edge, mixer) = fixture(vec![village(1, 16), village(2, 32), village(3, 48)], None);
    assert_eq!(hear(&mut edge, &mixer, 1200), [0.0; 2]);
    assert!(ids(&edge).is_empty());
    let (mut inside, mixer) = fixture(vec![village(4, 15)], None);
    assert!(hear(&mut inside, &mixer, 1200)[1] > 0.0);
    assert_eq!(ids(&inside), vec![4]);
}

/// Every kind a recipe is made at makes a working sound, read off the recipe mirror so a
/// new station arriving silent fails here; and the ones that are not stations — a tent, a
/// runestone — make none. The campfire is a station and is heard while lit; a doused one
/// is the douse test's.
#[test]
fn every_craft_station_has_a_sound_and_no_other_structure_does() {
    let eye = Vec3::new(4.5, 4.5, 4.5);
    for kind in [
        StructureKind::Tent,
        StructureKind::Forge,
        StructureKind::Campfire,
        StructureKind::Runestone,
        StructureKind::LeatherBench,
        StructureKind::ArmourBench,
        StructureKind::EnchantingTable,
    ] {
        let station = crate::player::recipes_made_at(Some(kind)).next().is_some();
        let heard = Candidate::from_structure(&structure(1, kind, 4), eye);
        assert_eq!(heard.is_some(), station, "{kind:?}");
    }
    for (structure_kind, kind) in [
        (StructureKind::LeatherBench, Kind::Leather),
        (StructureKind::ArmourBench, Kind::Armour),
        (StructureKind::EnchantingTable, Kind::Enchanting),
    ] {
        let candidate = Candidate::from_structure(&structure(1, structure_kind, 4), eye)
            .expect("a bench beside the listener is heard");
        assert_eq!(candidate.kind, kind);
    }
}

#[test]
fn the_benches_carry_no_further_than_a_fire_and_no_louder_than_a_fire() {
    for (kind, range, gain) in [
        (Kind::Leather, 8.0, 0.5),
        (Kind::Armour, 10.0, 0.45),
        (Kind::Enchanting, 6.0, 0.7),
    ] {
        assert_eq!(carry(kind), Carry { range, gain });
        assert!(range <= FIRE_RANGE && gain <= FIRE_GAIN, "{kind:?}");
    }
}

#[test]
fn each_bench_is_heard_beside_it_on_its_side_and_silent_past_its_range() {
    for (kind, range) in [
        (StructureKind::LeatherBench, LEATHER_RANGE),
        (StructureKind::ArmourBench, ARMOUR_RANGE),
        (StructureKind::EnchantingTable, ENCHANTING_RANGE),
    ] {
        // Two blocks to the listener's right; long enough to include a pause and a stroke.
        let (mut app, mixer) = fixture(vec![structure(1, kind, 6)], None);
        let near = hear(&mut app, &mixer, 800);
        assert!(
            near[1] > 0.0 && near[0] < near[1] * 0.01,
            "{kind:?} beside the listener: {near:?}"
        );
        // The eye is at x = 4.5 and a structure at x sounds from x + 0.5.
        snapshot(&mut app, vec![structure(1, kind, 4 + range as i32)]);
        assert_eq!(hear(&mut app, &mixer, 100), [0.0; 2], "{kind:?}");
        assert!(ids(&app).is_empty());
    }
}

#[test]
fn no_city_source_is_admitted_or_placed_outside_the_distance_curve() {
    let origin = Vec3::new(0.5, 4.5, 4.5);
    for (structure_kind, kind) in [
        (StructureKind::Forge, Kind::Forge),
        (StructureKind::Campfire, Kind::Fire),
        (StructureKind::LeatherBench, Kind::Leather),
        (StructureKind::ArmourBench, Kind::Armour),
        (StructureKind::EnchantingTable, Kind::Enchanting),
    ] {
        let carry = carry(kind);
        for step in 0..=160 {
            let eye = origin - Vec3::X * (step as f32 * 0.25);
            let curve = spatial::attenuation(eye.distance(origin), carry.range);
            match Candidate::from_structure(&structure(1, structure_kind, 0), eye) {
                None => assert_eq!(curve, 0.0, "{kind:?} dropped at step {step}"),
                Some(candidate) => {
                    assert!(curve > 0.0, "{kind:?} admitted past its range at {step}");
                    for yaw in [0.0, 1.0, 3.0] {
                        let placement = candidate.place(eye, yaw, 0.0);
                        assert!((placement.gain - curve * carry.gain).abs() < 1e-6);
                        assert!(placement.gain <= curve);
                    }
                }
            }
        }
    }
}
