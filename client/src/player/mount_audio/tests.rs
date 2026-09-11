use super::*;
use crate::{
    audio::{Mixer, Sink},
    net::{ChunkCoord, EntityState, MountKind, MountState, SessionParams},
    player::interpolate::{WALK_PHASE_PERIOD_BLOCKS, WALK_STRIDE_BLOCKS},
    world::VoxelChunk,
};
use std::sync::Arc;

struct Buffer(Vec<f32>);
impl Sink for Buffer {
    fn block(&mut self) -> &mut [f32] {
        &mut self.0
    }
}
fn energy(samples: &[f32]) -> f32 {
    samples.iter().map(|v| v * v).sum()
}

/// The distance phase after `blocks` of travel, as the interpolator counts it.
fn travelled(blocks: f32) -> WalkPose {
    WalkPose {
        phase: (TAU * blocks / WALK_STRIDE_BLOCKS)
            .rem_euclid(TAU * WALK_PHASE_PERIOD_BLOCKS / WALK_STRIDE_BLOCKS),
        moving: true,
    }
}

/// Strikes heard walking `blocks` from `start`, one frame per hundredth of a block.
fn strikes(
    mounts: &mut Mounts,
    key: Entity,
    gait: &'static Gait,
    start: f32,
    blocks: f32,
) -> usize {
    let frames = (blocks * 100.0).round() as usize;
    (1..=frames)
        .filter(|frame| {
            mounts.stride(Drawn {
                key,
                gait,
                walk: travelled(start + *frame as f32 / 100.0),
            })
        })
        .count()
}

fn key() -> Entity {
    World::new().spawn_empty().id()
}

#[test]
fn a_walk_strikes_four_times_a_stride_and_a_canter_three() {
    for (gait, stride, beats) in [(horse::gait(false), 1.7, 4), (horse::gait(true), 3.4, 3)] {
        let key = key();
        let mut mounts = Mounts::default();
        // Start just past a beat so the stride holds each beat exactly once.
        mounts.stride(Drawn {
            key,
            gait,
            walk: travelled(0.005),
        });
        assert_eq!(strikes(&mut mounts, key, gait, 0.005, stride), beats);
        assert_eq!(
            strikes(&mut mounts, key, gait, 0.005 + stride, stride * 3.0),
            beats * 3
        );
    }
}

#[test]
fn the_phase_wrap_neither_drops_nor_repeats_a_beat() {
    for (gait, stride, beats) in [(horse::gait(false), 1.7, 4), (horse::gait(true), 3.4, 3)] {
        let key = key();
        let mut mounts = Mounts::default();
        let start = WALK_PHASE_PERIOD_BLOCKS - stride / 2.0 + 0.005;
        mounts.stride(Drawn {
            key,
            gait,
            walk: travelled(start),
        });
        assert_eq!(strikes(&mut mounts, key, gait, start, stride), beats);
    }
}

#[test]
fn standing_noise_and_a_delayed_frame_strike_no_more_than_the_legs_do() {
    let key = key();
    let mut mounts = Mounts::default();
    let standing = Drawn {
        key,
        gait: horse::gait(true),
        walk: WalkPose::default(),
    };
    for _ in 0..50 {
        assert!(!mounts.stride(standing));
    }
    assert!(mounts.cycles.is_empty());
    // Starting to move lands nothing: the hooves were already down.
    let moving = |blocks| Drawn {
        key,
        gait: horse::gait(true),
        walk: travelled(blocks),
    };
    assert!(!mounts.stride(moving(0.3)));
    // A small step backwards is noise, and does not re-land the beat it passed.
    assert!(mounts.stride(moving(0.9)));
    assert!(!mounts.stride(moving(0.88)));
    assert!(!mounts.stride(moving(0.9)));
    // A frame delayed by a stride and a half is caught up by no more than one strike a
    // beat, never replayed as every hoof it covered.
    assert!(footfalls(horse::gait(true), 0.1, 0.1 + 1.5 * TAU) <= 3);
    assert!(footfalls(horse::gait(false), 0.1, 0.1 + 1.5 * TAU) <= 4);
}

fn snapshot(tick: u32, mounted: &[u64], present: &[u64]) -> Snapshot {
    Snapshot {
        server_tick: tick,
        entities: present
            .iter()
            .map(|&entity_id| EntityState {
                entity_id,
                pos: [0.0; 3],
                vel: [0.0; 3],
                yaw: 0.0,
                health: 100,
                max_health: 100,
            })
            .collect(),
        mounts: mounted
            .iter()
            .map(|&entity_id| MountState {
                entity_id,
                mount: MountKind::BrownHorse,
            })
            .collect(),
        ..Default::default()
    }
}

#[test]
fn a_whinny_is_one_per_mount_transition_and_never_a_replay() {
    let mut mounts = Mounts::default();
    // The first snapshot is a baseline: a player already riding was never seen to mount.
    assert!(
        mounts
            .transitions(Some(&snapshot(1, &[2], &[1, 2])))
            .is_empty()
    );
    assert!(
        mounts
            .transitions(Some(&snapshot(2, &[2], &[1, 2])))
            .is_empty()
    );
    assert_eq!(
        mounts.transitions(Some(&snapshot(3, &[1, 2], &[1, 2]))),
        [1]
    );
    // The same tick read on a later frame, and a later tick that repeats the state.
    assert!(
        mounts
            .transitions(Some(&snapshot(3, &[1, 2], &[1, 2])))
            .is_empty()
    );
    assert!(
        mounts
            .transitions(Some(&snapshot(4, &[1, 2], &[1, 2])))
            .is_empty()
    );
    // Dismount, then mount again: a second transition.
    assert!(
        mounts
            .transitions(Some(&snapshot(5, &[2], &[1, 2])))
            .is_empty()
    );
    assert_eq!(
        mounts.transitions(Some(&snapshot(6, &[1, 2], &[1, 2]))),
        [1]
    );
    // Somebody who comes into view already mounted was not seen to mount.
    assert!(
        mounts
            .transitions(Some(&snapshot(7, &[1, 2, 3], &[1, 2, 3])))
            .is_empty()
    );
    // A cleared buffer — a reconnect or a world replacement — starts a new baseline.
    assert!(mounts.transitions(None).is_empty());
    assert!(
        mounts
            .transitions(Some(&snapshot(1, &[1, 2, 3], &[1, 2, 3])))
            .is_empty()
    );
    mounts = Mounts::default();
    assert!(
        mounts
            .transitions(Some(&snapshot(9, &[1], &[1])))
            .is_empty()
    );
}

struct Fixture {
    now: Instant,
    feet: HashMap<Entity, Vec3>,
    store: ChunkStore,
    eye: Transform,
    mixer: AudioMixer,
    horse: Entity,
}

impl Fixture {
    /// A horse four blocks to the listener's right on a floor of `floor` at y = 0.
    fn new(floor: crate::world::BlockId) -> Self {
        let mut store = ChunkStore::default();
        store.insert(
            ChunkCoord {
                cx: 0,
                cy: 0,
                cz: 0,
            },
            VoxelChunk::all_air(32),
        );
        for x in 0..9 {
            for z in 0..9 {
                store.apply_block(BlockCoord { x, y: 0, z }, floor, 32);
            }
        }
        let mixer = Arc::new(Mixer::new());
        mixer.set_format(48000, 2);
        let horse = key();
        Self {
            now: Instant::now(),
            feet: HashMap::from([(horse, Vec3::new(4.5, 1.0, 4.5))]),
            store,
            eye: Transform::from_xyz(0.5, 2.6, 4.5),
            mixer: AudioMixer::from_shared_for_test(mixer),
            horse,
        }
    }
    fn frame(&self, millis: u64) -> Frame<'_> {
        Frame {
            now: self.now + Duration::from_millis(millis),
            feet: &self.feet,
            store: &self.store,
            size: 32,
            eye: Some(&self.eye),
        }
    }
    /// Renders a canter covering `blocks` over ten-millisecond frames, or standing when
    /// `blocks` is zero, with `mounted` riders whinnying on the first frame.
    fn hear(&self, mounts: &mut Mounts, blocks: f32, mounted: &[Entity]) -> Vec<f32> {
        let mut result = vec![];
        for i in 0..60u16 {
            let walk = if blocks == 0.0 {
                WalkPose::default()
            } else {
                travelled(0.05 + blocks * f32::from(i) / 60.0)
            };
            let horses = [Drawn {
                key: self.horse,
                gait: horse::gait(true),
                walk,
            }];
            let whinny = if i == 0 { mounted } else { &[] };
            mounts.advance(&self.mixer, &self.frame(u64::from(i) * 10), &horses, whinny);
            let mut block = Buffer(vec![0.0; 960]);
            self.mixer.shared_for_test().render(&mut block);
            result.extend(block.0);
        }
        result
    }
}

#[test]
fn a_moving_horse_on_the_ground_is_heard_and_a_standing_or_airborne_one_is_not() {
    let f = Fixture::new(palette::STONE);
    assert!(energy(&f.hear(&mut Mounts::default(), 3.4, &[])) > 0.1);
    assert_eq!(energy(&f.hear(&mut Mounts::default(), 0.0, &[])), 0.0);
    let mut airborne = Fixture::new(palette::STONE);
    airborne
        .feet
        .insert(airborne.horse, Vec3::new(4.5, 3.0, 4.5));
    let mut mounts = Mounts::default();
    assert_eq!(energy(&airborne.hear(&mut mounts, 3.4, &[])), 0.0);
    assert!(mounts.playing.is_empty());
}

#[test]
fn the_strike_changes_with_the_ground_under_the_horse() {
    let stone = Fixture::new(palette::STONE).hear(&mut Mounts::default(), 3.4, &[]);
    let dirt = Fixture::new(palette::DIRT).hear(&mut Mounts::default(), 3.4, &[]);
    let difference: Vec<_> = stone.iter().zip(&dirt).map(|(a, b)| a - b).collect();
    assert!(energy(&difference) > 0.1);
    let f = Fixture::new(palette::PLANKS);
    assert_eq!(
        ground(&f.store, Vec3::new(4.5, 1.0, 4.5), 32),
        Some(MaterialClass::Wood)
    );
    // Across an edge the footprint still finds the floor; past it there is nothing.
    assert_eq!(
        ground(&f.store, Vec3::new(9.3, 1.0, 4.5), 32),
        Some(MaterialClass::Wood)
    );
    assert_eq!(ground(&f.store, Vec3::new(11.0, 1.0, 4.5), 32), None);
}

#[test]
fn another_players_horse_is_panned_on_sfx_and_muted_with_it() {
    let f = Fixture::new(palette::DIRT);
    let whinny = f.hear(&mut Mounts::default(), 0.0, &[f.horse]);
    let left = energy(&whinny.iter().step_by(2).copied().collect::<Vec<_>>());
    let right = energy(
        &whinny
            .iter()
            .skip(1)
            .step_by(2)
            .copied()
            .collect::<Vec<_>>(),
    );
    assert!(right > left * 4.0, "left {left}, right {right}");
    let muted = Fixture::new(palette::DIRT);
    muted.mixer.shared_for_test().set_gain(Bus::Sfx, 0.0);
    assert_eq!(
        energy(&muted.hear(&mut Mounts::default(), 3.4, &[muted.horse])),
        0.0
    );
    // Out of range is not started at all.
    let mut far = Fixture::new(palette::DIRT);
    far.eye = Transform::from_xyz(0.5, 2.6, 4.5 + RANGE);
    let mut mounts = Mounts::default();
    far.hear(&mut mounts, 3.4, &[far.horse]);
    assert!(mounts.playing.is_empty());
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
fn the_system_whinnies_once_for_a_drawn_rider_and_forgets_everything_without_a_session() {
    let f = Fixture::new(palette::DIRT);
    let mut app = App::new();
    let mut snapshots = SnapshotBuffer::default();
    snapshots.accept(snapshot(1, &[], &[1, 2]), f.now);
    app.insert_resource(AudioMixer::from_shared_for_test(f.mixer.shared_for_test()))
        .insert_resource(snapshots)
        .insert_resource(f.store)
        .insert_resource(session());
    register(&mut app);
    app.world_mut().spawn((WorldCamera, f.eye));
    let rider = app
        .world_mut()
        .spawn((
            Body(2),
            Transform::from_xyz(4.5, 1.0, 4.5),
            WalkPose::default(),
        ))
        .id();
    app.update();
    assert!(app.world().resource::<Mounts>().playing.is_empty());

    app.world_mut()
        .resource_mut::<SnapshotBuffer>()
        .accept(snapshot(2, &[2], &[1, 2]), f.now);
    app.world_mut().spawn((
        Horse {
            kind: MountKind::BrownHorse,
        },
        ChildOf(rider),
    ));
    app.update();
    assert_eq!(app.world().resource::<Mounts>().playing.len(), 1);
    app.update();
    app.world_mut()
        .resource_mut::<SnapshotBuffer>()
        .accept(snapshot(3, &[2], &[1, 2]), f.now);
    app.update();
    assert_eq!(app.world().resource::<Mounts>().playing.len(), 1);
    let mut block = Buffer(vec![0.0; 960]);
    f.mixer.shared_for_test().render(&mut block);
    assert!(energy(&block.0) > 0.0);

    reset_world(app.world_mut());
    assert!(app.world().resource::<Mounts>().playing.is_empty());
    app.world_mut().remove_resource::<Session>();
    app.update();
    let mounts = app.world().resource::<Mounts>();
    assert!(mounts.playing.is_empty() && mounts.tick.is_none());
}
