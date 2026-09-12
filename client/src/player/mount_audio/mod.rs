//! Hoofbeats and a whinny: bounded, disposable SFX read from what is already drawn.
//!
//! A hoof strikes where the rig shows it land. The clock is the horse's own
//! [`Gait::cycle`] — the distance phase `horse.rs` poses the legs from — and a beat is
//! heard on the frame the cycle passes one of [`Gait::beats`], each distinct instant once:
//! four at a walk, three at a canter, whose diagonal pair lands together. Standing is no
//! cycle and so no beat, and a horse with nothing solid under its feet is airborne and
//! strikes nothing. Which gait a horse uses is not chosen here: [`horse::gait`] is the one
//! selection, and `horse::animate_gait` poses the legs from the same answer.
//!
//! The whinny is a transition in the authoritative mount projection, observed between two
//! snapshot ticks: a player the previous tick held unmounted whom this one holds mounted.
//! A tick read twice is one observation, a player who comes into view already riding was
//! never seen to mount, and the first snapshot after a session or world replacement is a
//! baseline rather than a transition — so a reconnect replays nothing.
//!
//! Nothing here decides anything. No sound originates a request, and nothing is read back.
mod sounds;

use super::{
    Body, SnapshotBuffer, WalkPose, WorldCamera,
    constants::MOUNTED_WIDTH,
    horse::{self, Gait, Horse, PaddockHorse},
};
use crate::{
    audio::{
        AudioMixer, Bus, spatial,
        synth::{Baked, Playback, Rendering, Status},
    },
    net::{BlockCoord, Session, Snapshot},
    world::{
        ChunkStore,
        palette::{self, MaterialClass},
    },
};
use bevy::prelude::*;
use std::{
    collections::HashMap,
    f32::consts::{PI, TAU},
    time::{Duration, Instant},
};

const RANGE: f32 = 32.0;
const MAX_PLAYING: usize = 6;
const OCCLUSION_PERIOD: Duration = Duration::from_millis(100);
/// How far under the feet the ground is looked for. Enough to forgive an interpolated
/// position a fraction of a block off the surface; not enough to find the ground under a
/// jump.
const GROUND_PROBE: f32 = 0.25;
/// Where a strike is heard from above the feet: the hooves.
const HOOF_HEIGHT: f32 = 0.1;
/// Where a whinny is heard from above the feet: the head.
const HEAD_HEIGHT: f32 = 1.8;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cue {
    Hoof(MaterialClass),
    Whinny,
}

struct Voice {
    /// The rider's body or the paddock horse the sound follows.
    key: Entity,
    height: f32,
    playback: Playback,
    occlusion: f32,
    next_occlusion: Instant,
}

#[derive(Resource, Default)]
struct Mounts {
    /// Each moving horse's gait cycle as of the last frame it was heard.
    cycles: HashMap<Entity, f32>,
    /// The last snapshot tick read for mount transitions, and whether each player it held
    /// was mounted in it.
    tick: Option<u32>,
    riders: HashMap<u64, bool>,
    playing: Vec<Voice>,
    cache: Vec<(Cue, Baked)>,
    rate: u32,
}

pub(super) fn register(app: &mut App) {
    app.init_resource::<Mounts>().add_systems(
        Update,
        update
            .after(super::ApplySnapshots)
            .after(super::camera::AimCamera),
    );
}

pub(super) fn reset_world(world: &mut World) {
    crate::world::transition::reset::<Mounts>(world);
}

/// How many of `gait`'s distinct footfalls land after cycle `from` and up to `to`.
///
/// The phase wraps at a whole number of cycles, so a wrap reads as the short step it
/// really is. A step backwards smaller than half a cycle is interpolation noise and lands
/// nothing, and a delayed frame is never caught up: however far it went, each beat is
/// counted at most once.
fn footfalls(gait: &Gait, from: f32, to: f32) -> usize {
    let step = to - from;
    if step.abs() < f32::EPSILON || (-PI..0.0).contains(&step) {
        return 0;
    }
    let travelled = step.rem_euclid(TAU);
    let beats = gait.beats();
    beats
        .iter()
        .enumerate()
        .filter(|(index, beat)| !beats[..*index].contains(beat))
        .filter(|(_, beat)| (from - **beat).rem_euclid(TAU) + travelled >= TAU)
        .count()
}

/// What the ground under a horse is, or `None` when nothing solid is there to strike.
///
/// The centre first, then the corners of the mounted footprint, so a horse standing across
/// an edge still finds the block it is standing on.
fn ground(store: &ChunkStore, feet: Vec3, size: usize) -> Option<MaterialClass> {
    let half = MOUNTED_WIDTH * 0.45;
    let y = (feet.y - GROUND_PROBE).floor() as i32;
    [
        (0.0, 0.0),
        (-half, -half),
        (half, -half),
        (-half, half),
        (half, half),
    ]
    .into_iter()
    .find_map(|(dx, dz)| {
        let block = store.block_at(
            BlockCoord {
                x: (feet.x + dx).floor() as i32,
                y,
                z: (feet.z + dz).floor() as i32,
            },
            size,
        );
        palette::is_solid(block).then(|| palette::material_class(block))
    })
}

/// One horse as this frame draws it.
#[derive(Clone, Copy)]
struct Drawn {
    key: Entity,
    gait: &'static Gait,
    walk: WalkPose,
}

struct Frame<'a> {
    now: Instant,
    /// Every drawn horse's feet, keyed by the entity its sounds follow.
    feet: &'a HashMap<Entity, Vec3>,
    store: &'a ChunkStore,
    size: usize,
    eye: Option<&'a Transform>,
}

impl Mounts {
    /// Advances one horse's cycle and answers whether a hoof lands this frame.
    fn stride(&mut self, horse: Drawn) -> bool {
        let Some(cycle) = horse.gait.cycle(horse.walk) else {
            self.cycles.remove(&horse.key);
            return false;
        };
        let Some(before) = self.cycles.insert(horse.key, cycle) else {
            // The first moving frame starts from hooves already on the ground.
            return false;
        };
        let landed = footfalls(horse.gait, before, cycle);
        if landed == 0 && (-PI..0.0).contains(&(cycle - before)) {
            // Noise is not progress: keep the furthest point reached.
            self.cycles.insert(horse.key, before);
        }
        landed > 0
    }

    /// The players who mounted between the last tick read and this one.
    fn transitions(&mut self, snapshot: Option<&Snapshot>) -> Vec<u64> {
        let Some(snapshot) = snapshot else {
            self.tick = None;
            self.riders.clear();
            return Vec::new();
        };
        if self.tick == Some(snapshot.server_tick) {
            return Vec::new();
        }
        let mut mounted = Vec::new();
        let riders = snapshot
            .entities
            .iter()
            .map(|entity| {
                let id = entity.entity_id;
                let riding = snapshot.mounts.iter().any(|mount| mount.entity_id == id);
                if riding && self.riders.get(&id) == Some(&false) {
                    mounted.push(id);
                }
                (id, riding)
            })
            .collect();
        self.riders = riders;
        self.tick = Some(snapshot.server_tick);
        mounted
    }

    fn bake(&mut self, cue: Cue, rate: u32) -> Option<Baked> {
        if self.rate != rate {
            self.rate = rate;
            self.cache.clear();
            self.playing.clear();
        }
        if let Some((_, baked)) = self.cache.iter().find(|(key, _)| *key == cue) {
            return Some(baked.clone());
        }
        let (sound, seconds) = match cue {
            Cue::Hoof(ground) => (sounds::hoof(ground), sounds::HOOF_SECONDS),
            Cue::Whinny => (sounds::whinny(), sounds::WHINNY_SECONDS),
        };
        let baked = sound.bake(seconds, rate, 1122).ok()?;
        self.cache.push((cue, baked.clone()));
        Some(baked)
    }

    fn start(&mut self, mixer: &AudioMixer, cue: Cue, key: Entity, frame: &Frame<'_>) {
        // No deferred queue: under pressure a strike is lost, never played late over the
        // next one.
        if self.playing.len() >= MAX_PLAYING {
            return;
        }
        let (Some(feet), Some(eye)) = (frame.feet.get(&key).copied(), frame.eye) else {
            return;
        };
        let height = match cue {
            Cue::Hoof(_) => HOOF_HEIGHT,
            Cue::Whinny => HEAD_HEIGHT,
        };
        let origin = feet + Vec3::Y * height;
        if origin.distance(eye.translation) >= RANGE {
            return;
        }
        let Some(baked) = self.bake(cue, mixer.sample_rate()) else {
            return;
        };
        let occlusion = spatial::occlusion(frame.store, frame.size, eye.translation, origin);
        if let Ok(playback) = Playback::start(
            mixer,
            Bus::Sfx,
            Rendering::Baked(baked),
            spatial::place(
                eye.translation,
                spatial::listener_yaw(eye.rotation),
                origin,
                RANGE,
                occlusion,
            ),
        ) {
            self.playing.push(Voice {
                key,
                height,
                playback,
                occlusion,
                next_occlusion: frame.now + OCCLUSION_PERIOD,
            });
        }
    }

    fn advance(
        &mut self,
        mixer: &AudioMixer,
        frame: &Frame<'_>,
        horses: &[Drawn],
        mounted: &[Entity],
    ) {
        self.cycles.retain(|key, _| frame.feet.contains_key(key));
        self.playing.retain_mut(|voice| {
            let (Some(feet), Some(eye)) = (frame.feet.get(&voice.key).copied(), frame.eye) else {
                return false;
            };
            let origin = feet + Vec3::Y * voice.height;
            if frame.now >= voice.next_occlusion {
                voice.occlusion =
                    spatial::occlusion(frame.store, frame.size, eye.translation, origin);
                voice.next_occlusion = frame.now + OCCLUSION_PERIOD;
            }
            voice.playback.place(spatial::place(
                eye.translation,
                spatial::listener_yaw(eye.rotation),
                origin,
                RANGE,
                voice.occlusion,
            ));
            voice.playback.pump() == Status::Playing
        });
        for &key in mounted {
            self.start(mixer, Cue::Whinny, key, frame);
        }
        for &horse in horses {
            if !self.stride(horse) {
                continue;
            }
            let Some(feet) = frame.feet.get(&horse.key).copied() else {
                continue;
            };
            if let Some(ground) = ground(frame.store, feet, frame.size) {
                self.start(mixer, Cue::Hoof(ground), horse.key, frame);
            }
        }
    }
}

type RiddenHorses<'w, 's> = Query<'w, 's, &'static ChildOf, (With<Horse>, Without<PaddockHorse>)>;
type PaddockHorses<'w, 's> =
    Query<'w, 's, (Entity, &'static Transform, &'static WalkPose), With<PaddockHorse>>;

#[allow(
    clippy::too_many_arguments,
    reason = "one presentation system reads network, world, view and audio resources"
)]
fn update(
    mut mounts: ResMut<Mounts>,
    session: Option<Res<Session>>,
    mixer: Option<Res<AudioMixer>>,
    snapshots: Res<SnapshotBuffer>,
    store: Option<Res<ChunkStore>>,
    eyes: Query<&Transform, With<WorldCamera>>,
    bodies: Query<(&Body, &Transform, &WalkPose)>,
    ridden: RiddenHorses<'_, '_>,
    paddock: PaddockHorses<'_, '_>,
) {
    let Some(session) = session else {
        *mounts = Mounts::default();
        return;
    };
    if session.is_changed() {
        *mounts = Mounts::default();
    }
    // PlayerPlugin also runs without WorldPlugin/AudioPlugin in headless consumers.
    let (Some(mixer), Some(store)) = (mixer, store) else {
        *mounts = Mounts::default();
        return;
    };
    let mut feet = HashMap::new();
    let mut horses = Vec::new();
    let mut riders = HashMap::new();
    for parent in &ridden {
        let rider = parent.parent();
        let Ok((body, pose, walk)) = bodies.get(rider) else {
            continue;
        };
        feet.insert(rider, pose.translation);
        riders.insert(body.0, rider);
        horses.push(Drawn {
            key: rider,
            gait: horse::gait(true),
            walk: *walk,
        });
    }
    for (entity, pose, walk) in &paddock {
        feet.insert(entity, pose.translation);
        horses.push(Drawn {
            key: entity,
            gait: horse::gait(false),
            walk: *walk,
        });
    }
    // A player seen to mount whose horse is not drawn yet has nothing to be heard from.
    let mounted: Vec<_> = mounts
        .transitions(snapshots.latest_snapshot())
        .into_iter()
        .filter_map(|id| riders.get(&id).copied())
        .collect();
    let frame = Frame {
        now: Instant::now(),
        feet: &feet,
        store: &store,
        size: usize::from(session.0.chunk_size),
        eye: eyes.iter().next(),
    };
    mounts.advance(&mixer, &frame, &horses, &mounted);
}

#[cfg(test)]
mod pins;

#[cfg(test)]
mod tests;
