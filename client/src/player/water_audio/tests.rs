use super::*;
use crate::{
    audio::{Mixer, Sink},
    net::{ChunkCoord, MountKind, SessionParams},
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

fn key() -> Entity {
    World::new().spawn_empty().id()
}

/// A pool: stone at y = 0 over a nine-by-nine patch, water filling y = 1 and y = 2, and
/// air above it. The listener stands on the bank four blocks to the pool's left.
struct Fixture {
    now: Instant,
    store: ChunkStore,
    eye: Transform,
    mixer: AudioMixer,
    body: Entity,
}

impl Fixture {
    fn new() -> Self {
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
                store.apply_block(BlockCoord { x, y: 0, z }, palette::STONE, 32);
                for y in 1..3 {
                    store.apply_block(BlockCoord { x, y, z }, palette::WATER, 32);
                }
            }
        }
        let mixer = Arc::new(Mixer::new());
        mixer.set_format(48000, 2);
        Self {
            now: Instant::now(),
            store,
            eye: Transform::from_xyz(-4.0, 3.6, 4.5),
            mixer: AudioMixer::from_shared_for_test(mixer),
            body: key(),
        }
    }

    fn frame(&self, millis: u64) -> Frame<'_> {
        self.at(Duration::from_millis(millis))
    }

    /// A frame at an exact offset from the fixture's start, for the frame rates a
    /// millisecond cannot express.
    fn at(&self, elapsed: Duration) -> Frame<'_> {
        Frame {
            now: self.now + elapsed,
            store: &self.store,
            size: 32,
            eye: Some(&self.eye),
        }
    }

    fn drawn(&self, y: f32, mounted: bool) -> Drawn {
        Drawn {
            key: self.body,
            feet: Vec3::new(4.5, y, 4.5),
            mounted,
        }
    }

    /// One frame per `millis`, walking the body down through `heights`, and everything the
    /// mixer rendered while it did.
    fn hear(&self, waters: &mut Waters, millis: u64, heights: &[f32], mounted: bool) -> Vec<f32> {
        let mut result = vec![];
        for (index, height) in heights.iter().enumerate() {
            let bodies = [self.drawn(*height, mounted)];
            waters.advance(&self.mixer, &self.frame(index as u64 * millis), &bodies);
            let mut block = Buffer(vec![0.0; 960]);
            self.mixer.shared_for_test().render(&mut block);
            result.extend(block.0);
        }
        result
    }
}

/// Heights that walk a body from the bank down into the pool at a step of `drop` blocks a
/// frame, then hold it there.
fn descent(drop: f32, holds: usize) -> Vec<f32> {
    let mut heights = vec![4.0, 4.0 - drop];
    while *heights.last().unwrap() > 1.5 {
        heights.push(heights.last().unwrap() - drop);
    }
    heights.extend(std::iter::repeat_n(1.5, holds));
    heights
}

/// Found in review on #1197. The box test is the server's `overlapsFluid`, whose
/// `voxelSpan` is `floor(lo)` to `ceil(hi) - 1` — a half-open band. A face lying exactly on
/// a voxel boundary therefore touches the voxel below it and not the one above, and
/// flooring both ends instead reported water the box only grazed.
#[test]
fn a_face_exactly_on_a_voxel_boundary_touches_the_voxel_below_it_and_not_above() {
    let air = || {
        let mut store = ChunkStore::default();
        store.insert(
            ChunkCoord {
                cx: 0,
                cy: 0,
                cz: 0,
            },
            VoxelChunk::all_air(32),
        );
        store
    };
    // One sheet of water at y = 5 with nothing but air under it: an overhang, or the lip
    // of a fall. A body whose top is exactly 5.0 does not touch it.
    let mut above = air();
    for x in 0..9 {
        for z in 0..9 {
            above.apply_block(BlockCoord { x, y: 5, z }, palette::WATER, 32);
        }
    }
    let top_at_five = Vec3::new(4.5, 5.0 - PLAYER_HEIGHT, 4.5);
    assert_eq!(top_at_five.y + PLAYER_HEIGHT, 5.0, "the top is integral");
    assert!(!in_water(
        &above,
        top_at_five,
        PLAYER_WIDTH,
        PLAYER_HEIGHT,
        32
    ));
    // A hair higher and it does: the box now reaches into voxel 5.
    assert!(in_water(
        &above,
        top_at_five + Vec3::Y * 0.01,
        PLAYER_WIDTH,
        PLAYER_HEIGHT,
        32
    ));
    // The same rule on the horizontal axes, where the mounted box makes it routine: a block
    // wide and centred on 4.5 spans exactly [4.0, 5.0], so it is in voxel 4 and not in
    // voxel 5 — and a body a hundredth further on is in both.
    let mut beside = air();
    for z in 0..9 {
        beside.apply_block(BlockCoord { x: 5, y: 2, z }, palette::WATER, 32);
    }
    let centred = Vec3::new(4.5, 2.0, 4.5);
    assert!(!in_water(
        &beside,
        centred,
        MOUNTED_WIDTH,
        MOUNTED_HEIGHT,
        32
    ));
    assert!(in_water(
        &beside,
        centred + Vec3::X * 0.01,
        MOUNTED_WIDTH,
        MOUNTED_HEIGHT,
        32
    ));
}

/// Found in review on #1197. The downward speed used to be read from one frame's duration
/// and thrown away when that duration fell under `FALL_WINDOW.0`, so on a client running
/// faster than 500 Hz every entry was a [`Force::Step`] — a dive off a cliff sounding like
/// a step off a bank, the acceptance criterion exactly inverted. The reference sample is
/// now held across frames until it is old enough to divide by, so the same fall reads the
/// same force at every frame rate.
#[test]
fn the_same_fall_is_the_same_force_at_every_frame_rate() {
    let f = Fixture::new();
    // 30 blocks a second of fall, sampled at 60, 500, 1000, 4000 and 100,000 frames a
    // second. The last is far past anything real, and that is the point: the force must
    // not depend on the rate at all.
    for micros in [16_666u64, 2_000, 1_000, 250, 10] {
        let mut waters = Waters::default();
        let mut height = 4.0;
        let mut frame = 0u64;
        let mut entered = None;
        while height > 1.4 && entered.is_none() {
            let bodies = [f.drawn(height, false)];
            let at = f.at(Duration::from_micros(frame * micros));
            entered = waters
                .entries(&at, &bodies)
                .into_iter()
                .map(|(_, force)| force)
                .next();
            height -= 30.0 * micros as f32 / 1_000_000.0;
            frame += 1;
        }
        assert_eq!(
            entered,
            Some(Force::Dive),
            "at {micros} µs a frame a 30 blocks-a-second fall read {entered:?}"
        );
    }
    // And the long end of the window still filters: a reference older than FALL_WINDOW.1
    // says nothing about how the body arrived, so the entry is the gentlest one.
    let mut waters = Waters::default();
    waters.entries(&f.frame(0), &[f.drawn(4.0, false)]);
    let stale = waters.entries(&f.frame(2_000), &[f.drawn(1.5, false)]);
    assert_eq!(
        stale.into_iter().map(|(_, force)| force).next(),
        Some(Force::Step)
    );
}

#[test]
fn the_body_box_is_in_water_when_any_voxel_it_touches_is() {
    let f = Fixture::new();
    let feet = |x: f32, y: f32| Vec3::new(x, y, 4.5);
    // Standing on the bottom with the feet in the water: in water, which is what the
    // server's own box test answers and therefore what it is swimming in.
    assert!(in_water(
        &f.store,
        feet(4.5, 1.0),
        PLAYER_WIDTH,
        PLAYER_HEIGHT,
        32
    ));
    // Floating with the head out: still in water, which is why bobbing does not re-fire.
    assert!(in_water(
        &f.store,
        feet(4.5, 2.4),
        PLAYER_WIDTH,
        PLAYER_HEIGHT,
        32
    ));
    // Feet exactly on the surface, body above it: nothing but air.
    assert!(!in_water(
        &f.store,
        feet(4.5, 3.0),
        PLAYER_WIDTH,
        PLAYER_HEIGHT,
        32
    ));
    // A taller box reaches a voxel the walking one does not: the mounted body's feet are
    // above the water and its own box is dry all the same.
    assert!(!in_water(
        &f.store,
        feet(4.5, 3.0),
        MOUNTED_WIDTH,
        MOUNTED_HEIGHT,
        32
    ));
    // Across the pool's edge the wider box still touches the water; a body clear of it
    // does not.
    assert!(in_water(
        &f.store,
        feet(9.2, 2.0),
        MOUNTED_WIDTH,
        MOUNTED_HEIGHT,
        32
    ));
    assert!(!in_water(
        &f.store,
        feet(11.0, 2.0),
        MOUNTED_WIDTH,
        MOUNTED_HEIGHT,
        32
    ));
    // Over world this session does not hold, a body is not reported as swimming.
    assert!(!in_water(
        &f.store,
        feet(400.0, 2.0),
        PLAYER_WIDTH,
        PLAYER_HEIGHT,
        32
    ));
    // A position that is not a number answers no rather than sweeping a span.
    assert!(!in_water(
        &f.store,
        Vec3::new(f32::NAN, 2.0, 4.5),
        PLAYER_WIDTH,
        PLAYER_HEIGHT,
        32
    ));
}

#[test]
fn one_splash_per_crossing_and_a_second_crossing_fires_again() {
    let f = Fixture::new();
    let mut waters = Waters::default();
    // The first frame a body is seen is a baseline: somebody already in the water was
    // never observed to enter it.
    let bodies = [f.drawn(1.5, false)];
    waters.advance(&f.mixer, &f.frame(0), &bodies);
    assert!(waters.playing.is_empty());
    for millis in [10, 20, 30] {
        waters.advance(&f.mixer, &f.frame(millis), &bodies);
    }
    assert!(waters.playing.is_empty(), "a splash per frame in the water");

    // Out of the water, then in again: one entry, once.
    let mut waters = Waters::default();
    let above = [f.drawn(4.0, false)];
    waters.advance(&f.mixer, &f.frame(0), &above);
    waters.advance(&f.mixer, &f.frame(10), &bodies);
    assert_eq!(waters.playing.len(), 1);
    // Bobbing at the surface, head out and back under, fires nothing more.
    for (index, height) in [2.4f32, 2.9, 2.4, 1.2].into_iter().enumerate() {
        let bobbing = [f.drawn(height, false)];
        waters.advance(&f.mixer, &f.frame(20 + index as u64 * 10), &bobbing);
    }
    assert_eq!(waters.playing.len(), 1);
    // Out onto the bank, and back in: a second entry.
    let mut waters = Waters::default();
    waters.advance(&f.mixer, &f.frame(0), &above);
    waters.advance(&f.mixer, &f.frame(10), &bodies);
    waters.advance(&f.mixer, &f.frame(20), &above);
    assert_eq!(waters.playing.len(), 1);
    waters.advance(&f.mixer, &f.frame(30), &bodies);
    assert_eq!(waters.playing.len(), 2);
}

#[test]
fn a_dive_is_louder_than_a_step_in_off_the_bank() {
    let f = Fixture::new();
    // The speed is the drop divided by the frame gap, and both halves matter: a tenth of a
    // block per tenth of a second is a block a second, walking in off the bank, while a
    // whole block per ten milliseconds is a hundred blocks a second, straight off a cliff.
    let step = f.hear(&mut Waters::default(), 100, &descent(0.1, 50), false);
    let dive = f.hear(&mut Waters::default(), 10, &descent(1.0, 50), false);
    assert!(energy(&step) > 0.0, "no splash walking in");
    assert!(
        energy(&dive) > energy(&step) * 1.5,
        "a step measured {}, a dive {}",
        energy(&step),
        energy(&dive)
    );
    // The cue each one chose, read from the cache rather than from the audio.
    let entry = |millis: u64, heights: &[f32]| {
        let mut waters = Waters::default();
        f.hear(&mut waters, millis, heights, false);
        waters.cache.iter().map(|(cue, _)| *cue).collect::<Vec<_>>()
    };
    assert_eq!(
        entry(100, &descent(0.1, 1)),
        [Cue::Splash {
            mounted: false,
            force: Force::Step
        }]
    );
    assert_eq!(
        entry(10, &descent(1.0, 1)),
        [Cue::Splash {
            mounted: false,
            force: Force::Dive
        }]
    );
    // A frame gap too long to read a speed from is the gentlest entry, not a guess.
    let mut waters = Waters::default();
    let bodies = [f.drawn(4.0, false)];
    waters.advance(&f.mixer, &f.frame(0), &bodies);
    let entered = [f.drawn(1.5, false)];
    waters.advance(&f.mixer, &f.frame(2_000), &entered);
    assert_eq!(
        waters.cache.iter().map(|(cue, _)| *cue).collect::<Vec<_>>(),
        [Cue::Splash {
            mounted: false,
            force: Force::Step
        }]
    );
}

#[test]
fn a_mounted_entry_is_a_mounted_cue() {
    let f = Fixture::new();
    let mut waters = Waters::default();
    f.hear(&mut waters, 10, &descent(0.5, 4), true);
    let cues: Vec<_> = waters.cache.iter().map(|(cue, _)| *cue).collect();
    assert_eq!(cues.len(), 1);
    assert!(matches!(cues[0], Cue::Splash { mounted: true, .. }));
}

#[test]
fn another_players_entry_is_panned_on_sfx_and_muted_with_it() {
    let f = Fixture::new();
    let splash = f.hear(&mut Waters::default(), 10, &descent(0.5, 30), false);
    let left = energy(&splash.iter().step_by(2).copied().collect::<Vec<_>>());
    let right = energy(
        &splash
            .iter()
            .skip(1)
            .step_by(2)
            .copied()
            .collect::<Vec<_>>(),
    );
    // The pool is to the listener's right, and yaw zero looks along -Z.
    assert!(right > left * 4.0, "left {left}, right {right}");

    let muted = Fixture::new();
    muted.mixer.shared_for_test().set_gain(Bus::Sfx, 0.0);
    assert_eq!(
        energy(&muted.hear(&mut Waters::default(), 10, &descent(0.5, 30), false)),
        0.0
    );

    // Out of range is never started at all.
    let mut far = Fixture::new();
    far.eye = Transform::from_xyz(4.5, 3.6, 4.5 + RANGE);
    let mut waters = Waters::default();
    far.hear(&mut waters, 10, &descent(0.5, 4), false);
    assert!(waters.playing.is_empty());
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
fn the_system_splashes_for_a_drawn_body_and_forgets_everything_without_a_session() {
    let f = Fixture::new();
    let mut app = App::new();
    app.insert_resource(AudioMixer::from_shared_for_test(f.mixer.shared_for_test()))
        .insert_resource(f.store)
        .insert_resource(session());
    register(&mut app);
    app.world_mut().spawn((WorldCamera, f.eye));
    let body = app
        .world_mut()
        .spawn((Body(2), Transform::from_xyz(4.5, 4.0, 4.5)))
        .id();
    app.update();
    assert!(app.world().resource::<Waters>().playing.is_empty());

    // The body ends up in the pool: one splash, and not another on the frames after it.
    app.world_mut()
        .entity_mut(body)
        .insert(Transform::from_xyz(4.5, 1.5, 4.5));
    app.update();
    assert_eq!(app.world().resource::<Waters>().playing.len(), 1);
    app.update();
    assert_eq!(app.world().resource::<Waters>().playing.len(), 1);
    let mut block = Buffer(vec![0.0; 960]);
    f.mixer.shared_for_test().render(&mut block);
    assert!(energy(&block.0) > 0.0);
    // A horse under the body makes the next entry the mounted one.
    app.world_mut().spawn((
        Horse {
            kind: MountKind::BrownHorse,
        },
        ChildOf(body),
    ));
    app.world_mut()
        .entity_mut(body)
        .insert(Transform::from_xyz(4.5, 4.0, 4.5));
    app.update();
    app.world_mut()
        .entity_mut(body)
        .insert(Transform::from_xyz(4.5, 1.5, 4.5));
    app.update();
    assert!(
        app.world()
            .resource::<Waters>()
            .cache
            .iter()
            .any(|(cue, _)| matches!(cue, Cue::Splash { mounted: true, .. }))
    );

    reset_world(app.world_mut());
    assert!(app.world().resource::<Waters>().playing.is_empty());
    app.world_mut().remove_resource::<Session>();
    app.update();
    let waters = app.world().resource::<Waters>();
    assert!(waters.playing.is_empty() && waters.seen.is_empty());
}
