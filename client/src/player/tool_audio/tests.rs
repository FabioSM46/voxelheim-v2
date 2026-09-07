use super::*;
use crate::{
    audio::{MAX_SOURCES, Mixer, Sink},
    net::{EntityState, SessionParams, Snapshot},
    world::VoxelChunk,
};
use std::sync::Arc;
struct Buffer(Vec<f32>);
impl Sink for Buffer {
    fn block(&mut self) -> &mut [f32] {
        &mut self.0
    }
}
fn event(id: u64, phase: MiningPhase) -> MiningActivity {
    MiningActivity {
        tick: 7,
        actor_entity_id: 2,
        activity_id: id,
        pos: BlockCoord { x: 4, y: 0, z: 4 },
        block_id: palette::STONE,
        tool: MiningTool::Pickaxe,
        phase,
    }
}
fn snapshot(tick: u32) -> Snapshot {
    Snapshot {
        server_tick: tick,
        entities: vec![
            EntityState {
                entity_id: 1,
                pos: [0.0, 0.0, 4.0],
                vel: [0.0; 3],
                yaw: 0.0,
            },
            EntityState {
                entity_id: 2,
                pos: [4.0, 0.0, 4.0],
                vel: [0.0; 3],
                yaw: 0.0,
            },
        ],
        ..Default::default()
    }
}
struct Fixture {
    now: Instant,
    snapshots: SnapshotBuffer,
    positions: HashMap<u64, Vec3>,
    store: ChunkStore,
    eye: Transform,
    mixer: AudioMixer,
}
impl Fixture {
    fn new() -> Self {
        let now = Instant::now();
        let mut snapshots = SnapshotBuffer::default();
        snapshots.accept(snapshot(7), now);
        let mut store = ChunkStore::default();
        store.insert(
            ChunkCoord {
                cx: 0,
                cy: 0,
                cz: 0,
            },
            VoxelChunk::all_air(32),
        );
        let mixer = Arc::new(Mixer::new());
        mixer.set_format(48000, 2);
        Self {
            now,
            snapshots,
            positions: HashMap::from([
                (1, Vec3::new(0.0, EYE_HEIGHT, 4.0)),
                (2, Vec3::new(4.0, EYE_HEIGHT, 4.0)),
            ]),
            store,
            eye: Transform::from_xyz(0.0, EYE_HEIGHT, 4.0),
            mixer: AudioMixer::from_shared_for_test(mixer),
        }
    }
    fn frame(&self, millis: u64) -> Frame<'_> {
        Frame {
            now: self.now + Duration::from_millis(millis),
            snapshots: &self.snapshots,
            positions: &self.positions,
            store: &self.store,
            size: 32,
            local: 1,
            local_mining: true,
            eye: Some(&self.eye),
        }
    }
    fn hear(&self, tools: &mut Tools, start: u64, blocks: u64) -> Vec<f32> {
        let mut result = vec![];
        for i in 0..blocks {
            tools.advance(&self.mixer, &self.frame(start + i * 10));
            let mut block = Buffer(vec![0.0; 960]);
            self.mixer.shared_for_test().render(&mut block);
            result.extend(block.0);
        }
        result
    }
}
fn energy(samples: &[f32]) -> f32 {
    samples.iter().map(|v| v * v).sum()
}
#[test]
fn repeated_renewals_use_punch_cadence_not_network_cadence_and_expire_without_stop() {
    let f = Fixture::new();
    let mut tools = Tools::default();
    assert!(!tools.observe(event(1, MiningPhase::Active), f.now, f.now, Some(7), true));
    assert!(energy(&f.hear(&mut tools, 0, 30)) > 0.1);
    let next = tools.attempts[&2].next_strike;
    for tick in 8..12 {
        let mut e = event(1, MiningPhase::Active);
        e.tick = tick;
        assert!(!tools.observe(
            e,
            f.now + Duration::from_millis(100),
            f.now + Duration::from_millis(100),
            Some(tick),
            true
        ));
        assert_eq!(tools.attempts[&2].next_strike, next);
    }
    assert!(energy(&f.hear(&mut tools, 420, 15)) > 0.1);
    tools.advance(&f.mixer, &f.frame(600));
    assert!(!tools.attempts[&2].active);
    assert!(tools.playing.is_empty());
    assert_eq!(tools.attempts[&2].event.activity_id, 1);
}
#[test]
fn exact_tick_lease_and_visibility_fail_closed() {
    let f = Fixture::new();
    for (tick, visible, age) in [
        (Some(8), true, 0),
        (None, true, 0),
        (Some(7), false, 0),
        (Some(7), true, 500),
        (Some(7), true, 3000),
    ] {
        let mut tools = Tools::default();
        assert!(!tools.observe(
            event(1, MiningPhase::Active),
            f.now,
            f.now + Duration::from_millis(age),
            tick,
            visible
        ));
        assert!(tools.attempts.is_empty());
    }
}
#[test]
fn completion_is_once_and_highwater_blocks_old_attempts_after_expiry() {
    let f = Fixture::new();
    let mut tools = Tools::default();
    assert!(tools.observe(
        event(2, MiningPhase::Completed),
        f.now,
        f.now,
        Some(7),
        true
    ));
    for e in [
        event(2, MiningPhase::Completed),
        event(2, MiningPhase::Active),
        event(1, MiningPhase::Completed),
        event(1, MiningPhase::Active),
    ] {
        assert!(!tools.observe(e, f.now, f.now, Some(7), true));
    }
    assert!(tools.attempts[&2].completed);
    assert!(!tools.observe(event(3, MiningPhase::Active), f.now, f.now, Some(7), true));
    tools.advance(&f.mixer, &f.frame(500));
    assert_eq!(tools.attempts[&2].event.activity_id, 3);
    assert!(!tools.observe(
        event(2, MiningPhase::Completed),
        f.now,
        f.now,
        Some(7),
        true
    ));
    // Same tick duplicates cannot renew a lease even if dequeued a second time later.
    assert!(!tools.observe(
        event(3, MiningPhase::Active),
        f.now + Duration::from_millis(500),
        f.now + Duration::from_millis(500),
        Some(7),
        true
    ));
    assert!(!tools.attempts[&2].active);
}
#[test]
fn old_tick_or_changed_identity_cannot_replace_a_newer_observation() {
    let f = Fixture::new();
    let mut tools = Tools::default();
    let mut newer = event(1, MiningPhase::Active);
    newer.tick = 8;
    tools.observe(newer, f.now, f.now, Some(8), true);
    assert!(!tools.observe(
        event(1, MiningPhase::Completed),
        f.now,
        f.now,
        Some(7),
        true
    ));
    let mut changed = newer;
    changed.tick = 9;
    changed.tool = MiningTool::Hand;
    assert!(!tools.observe(changed, f.now, f.now, Some(9), true));
    assert_eq!(tools.attempts[&2].event, newer);
    newer.tick = u32::MAX;
    tools.attempts.clear();
    tools.observe(newer, f.now, f.now, Some(u32::MAX), true);
    newer.tick = 0;
    tools.observe(newer, f.now, f.now, Some(0), true);
    assert_eq!(tools.attempts[&2].event.tick, 0);
}
#[test]
fn death_disappearance_unloaded_target_and_local_stop_cancel_playback() {
    for reason in 0..4 {
        let mut f = Fixture::new();
        let mut tools = Tools::default();
        tools.observe(event(1, MiningPhase::Active), f.now, f.now, Some(7), true);
        f.hear(&mut tools, 0, 3);
        assert!(!tools.playing.is_empty());
        let mut snap = snapshot(8);
        match reason {
            0 => snap.dead_players.push(2),
            1 => snap.entities.retain(|entity| entity.entity_id != 2),
            2 => f.store = ChunkStore::default(),
            _ => {}
        }
        f.snapshots.accept(snap, f.now + Duration::from_millis(40));
        let mut frame = f.frame(40);
        if reason == 3 {
            frame.local = 2;
            frame.local_mining = false;
        }
        tools.advance(&f.mixer, &frame);
        assert!(tools.playing.is_empty(), "reason {reason}");
        if reason < 2 {
            assert!(tools.attempts.is_empty());
        }
    }
}
#[test]
fn another_players_strike_is_panned_muffled_and_on_sfx() {
    let f = Fixture::new();
    let render = |f: &Fixture| {
        let mut tools = Tools::default();
        tools.observe(event(1, MiningPhase::Active), f.now, f.now, Some(7), true);
        f.hear(&mut tools, 0, 30)
    };
    let open = render(&f);
    let left = energy(&open.iter().step_by(2).copied().collect::<Vec<_>>());
    let right = energy(&open.iter().skip(1).step_by(2).copied().collect::<Vec<_>>());
    assert!(right > left * 4.0, "left {left}, right {right}");
    let mut wall = Fixture::new();
    for y in 0..5 {
        for z in 0..9 {
            wall.store
                .apply_block(BlockCoord { x: 2, y, z }, palette::STONE, 32);
        }
    }
    assert!(energy(&render(&wall)) < energy(&open) * 0.65);
    let muted = Fixture::new();
    muted.mixer.shared_for_test().set_gain(Bus::Sfx, 0.0);
    assert_eq!(energy(&render(&muted)), 0.0);
}
#[test]
fn pressure_drops_one_shots_and_device_change_discards_old_rate_samples() {
    let f = Fixture::new();
    let held: Vec<_> = (0..MAX_SOURCES)
        .filter_map(|_| f.mixer.shared_for_test().claim(Bus::Voice))
        .collect();
    let mut tools = Tools::default();
    tools.start(
        &f.mixer,
        Cue::Break(MiningTool::Hand, MaterialClass::Earth),
        2,
        None,
        &f.frame(0),
    );
    assert!(tools.playing.is_empty());
    drop(held);
    assert_eq!(energy(&f.hear(&mut tools, 10, 10)), 0.0);
    tools.start(&f.mixer, Cue::Swing(true), 2, None, &f.frame(110));
    assert!(energy(&f.hear(&mut tools, 110, 3)) > 0.0);
    f.mixer.shared_for_test().set_format(44100, 2);
    tools.advance(&f.mixer, &f.frame(150));
    assert!(tools.playing.is_empty());
    tools.start(&f.mixer, Cue::Swing(true), 2, None, &f.frame(160));
    assert_eq!(tools.cache[0].1.sample_rate(), 44100);
    assert!(energy(&f.hear(&mut tools, 160, 5)) > 0.0);
}
fn session() -> Session {
    Session(SessionParams {
        clock: Default::default(),
        entity_id: 1,
        spawn: [0.0; 3],
        world_seed: 1,
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
fn real_swing_message_plays_without_any_hit_and_world_or_disconnect_returns_sources() {
    let f = Fixture::new();
    let mut app = App::new();
    app.insert_resource(AudioMixer::from_shared_for_test(f.mixer.shared_for_test()))
        .insert_resource(f.snapshots)
        .insert_resource(f.store)
        .insert_resource(session())
        .init_resource::<MiningFeedback>()
        .init_resource::<LocalMount>()
        .insert_resource(InputMode::Playing);
    register(&mut app);
    app.world_mut().spawn((WorldCamera, f.eye));
    app.world_mut()
        .spawn((Body(1), Transform::from_xyz(0.0, 0.0, 4.0)));
    app.update();
    app.world_mut().write_message(SwingSent {
        item_id: ITEM_RUSTY_SWORD,
    });
    app.update();
    assert_eq!(app.world().resource::<Tools>().playing.len(), 1);
    app.update();
    let mut block = Buffer(vec![0.0; 960]);
    f.mixer.shared_for_test().render(&mut block);
    assert!(energy(&block.0) > 0.0);
    reset_world(app.world_mut());
    assert!(app.world().resource::<Tools>().playing.is_empty());
    app.world_mut().write_message(SwingSent {
        item_id: super::super::crafting::ITEM_BOW,
    });
    app.update();
    assert!(app.world().resource::<Tools>().playing.is_empty());
    app.world_mut().write_message(SwingSent {
        item_id: ITEM_IRON_SWORD,
    });
    app.update();
    app.world_mut().remove_resource::<Session>();
    app.update();
    assert!(app.world().resource::<Tools>().playing.is_empty());
    // Dropped producers become silent immediately; the callback owns ring recycling.
    f.mixer.shared_for_test().render(&mut block);
    assert_eq!(energy(&block.0), 0.0);
    let held: Vec<_> = (0..MAX_SOURCES)
        .filter_map(|_| f.mixer.shared_for_test().claim(Bus::Voice))
        .collect();
    assert_eq!(held.len(), MAX_SOURCES);
}

#[test]
fn completion_renders_its_tail_when_the_next_attempt_starts_in_the_same_batch_or_soon_after() {
    for (next_at, target_loaded) in [(0, true), (100, true), (0, false)] {
        let render = |with_completion: bool| {
            let mut f = Fixture::new();
            if target_loaded {
                f.store.insert(
                    ChunkCoord {
                        cx: 1,
                        cy: 0,
                        cz: 0,
                    },
                    VoxelChunk::all_air(32),
                );
            }
            let mut tools = Tools::default();
            if with_completion {
                assert!(tools.observe(
                    event(1, MiningPhase::Completed),
                    f.now,
                    f.now,
                    Some(7),
                    true
                ));
                tools.start(
                    &f.mixer,
                    Cue::Break(MiningTool::Pickaxe, MaterialClass::Stone),
                    2,
                    Some(1),
                    &f.frame(0),
                );
            }
            let mut samples = f.hear(&mut tools, 0, next_at / 10);
            let mut next = event(2, MiningPhase::Active);
            // Its target is in another chunk: the old completion must retain its own
            // loaded target rather than borrowing this newer attempt's coordinate.
            next.pos.x = 33;
            let now = f.now + Duration::from_millis(next_at);
            tools.observe(next, now, now, Some(7), true);
            samples.extend(f.hear(&mut tools, next_at, (400 - next_at) / 10));
            assert!(tools.playing.iter().all(|voice| !voice.completion));
            samples
        };
        let with_break = render(true);
        let without_break = render(false);
        // A strike has ended by 240ms. The collapse still has audible energy later,
        // even if a next Active immediately replaced the attempt that authorized it.
        let tail = 26 * 960;
        assert!(energy(&with_break[tail..]) > 0.01, "next at {next_at}ms");
        let difference: Vec<_> = with_break[tail..]
            .iter()
            .zip(&without_break[tail..])
            .map(|(a, b)| a - b)
            .collect();
        assert!(
            energy(&difference) > 0.01,
            "completion disappeared at {next_at}ms, target loaded {target_loaded}"
        );
    }
}
