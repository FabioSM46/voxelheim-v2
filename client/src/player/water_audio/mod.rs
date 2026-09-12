//! The water is heard: a splash the moment a body breaks the surface, and a stroke for
//! every stride's worth of water it pushes while it swims.
//!
//! **This is presentation, and the probe is what keeps it honest.** Swimming is decided
//! entirely on the server — `overlapsFluid` against the body's box in
//! `server/internal/game/player.go`, read by the swim speed, the rise and the currents.
//! Nothing here predicts any of that. Each frame this module asks the same question of the
//! voxels the client already holds, about the body the interpolator has already placed, and
//! makes a noise when the answer changes from no to yes. No sound originates a request and
//! nothing reads any of this back: not input, not targeting, not placement.
//!
//! **Why a probe of its own, when [`super::sky::submerged_at`] exists.** That one asks
//! whether the *eye* is under water, which is the wrong question twice over. A splash
//! happens when the feet break the surface, seconds before the head does on a long fall;
//! and a swimmer at the surface with their head out is still swimming. So the question
//! here is the server's: does the body's box touch any voxel of the water family
//! ([`palette::is_water`])? A voxel span over the box, exactly as `overlapsFluid` sweeps
//! one, and an absent chunk answers air and therefore not water — the same direction the
//! server's `Terrain.Fluid` fails in.
//!
//! **One splash per crossing.** A body's first frame is a baseline rather than a
//! transition, so somebody who comes into view already swimming was never seen to enter,
//! and a reconnect or a world replacement replays nothing. Bobbing at the surface does not
//! re-fire: the box test does not stop being true when the head comes out, because the feet
//! are still in the water. Leaving the water entirely and coming back is a second entry.
//!
//! **Strokes are clocked by distance, not by time**, so their rate follows the swimmer's
//! speed and stops dead when the swimmer does — the same reasoning `mount_audio`'s
//! `footfalls` uses for a hoof, in the shape a travelled distance takes. See
//! [`STROKE_BLOCKS`] for the stride and for why only the horizontal counts.
//!
//! **A swimming horse does not strike anything**, so `mount_audio` asks this module's probe
//! before it lands a hoof: the surface a swimming horse's legs are moving through is water,
//! not ground, and [`in_water`] is the one definition of that both modules read.
//!
//! **Who is heard.** Every drawn player body — the listener's own, and every other
//! player's, mounted or on foot — placed through `spatial::place` on [`Bus::Sfx`] like the
//! hoof beside it, so a remote entry is panned and faded by distance and a muted bus is
//! silent. A paddock horse is not heard: it is scenery nobody is riding, and its entry
//! would be a sound with no event behind it.
mod sounds;

use super::{
    Body, WorldCamera,
    constants::{MOUNTED_HEIGHT, MOUNTED_WIDTH, PLAYER_HEIGHT, PLAYER_WIDTH},
    horse::{Horse, PaddockHorse},
};
use crate::{
    audio::{
        AudioMixer, Bus, spatial,
        synth::{Baked, Playback, Rendering, Status},
    },
    net::{BlockCoord, Session},
    world::{ChunkStore, palette},
};
use bevy::prelude::*;
use sounds::Force;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// How far away an entry is still heard. Further than a hoof's 32 blocks: a body hitting
/// water is one of the loudest things that happens outdoors, and a dive off a cliff is
/// heard from the top of it.
const RANGE: f32 = 40.0;
/// At most this many water voices at once. Under pressure a sound is lost rather than
/// played late over the next one — the hoof's rule, for the same reason. Six, because a
/// swimmer strokes about twice a second and a stroke lasts up to seven tenths of one: a
/// couple of swimmers in earshot need room for their strokes to overlap and for an entry
/// on top of them.
const MAX_PLAYING: usize = 6;
const OCCLUSION_PERIOD: Duration = Duration::from_millis(100);
/// Where a water sound is heard from, above the feet: at the surface the body went
/// through, which is where the water is thrown from and where a stroke breaks it.
const SURFACE_HEIGHT: f32 = 0.2;
/// The frame gaps a downward speed may be read from, in seconds. Shorter than the first is
/// a division that magnifies interpolation noise into a dive; longer than the second and
/// the body could have done anything in between. Outside the window the entry is the
/// gentlest one there is rather than a guess.
const SPEED_WINDOW: (f32, f32) = (0.002, 0.25);
/// How far a body swims between strokes, in blocks.
///
/// **A distance rather than a period, which is what makes the rate follow the speed.** The
/// server caps a swimmer at `SwimSpeed` — 2.58 blocks a second, and a mounted swimmer at
/// the same, since the mount's speed is capped to it too — so a swimmer on foot strokes
/// about twice a second and a horse about once: the slower, bigger stroke the acceptance
/// criterion asks for, falling out of the stride rather than out of a second timer.
///
/// This is the [`super::mount_audio::footfalls`] mechanism in the shape distance-travelled
/// takes rather than a wrapped phase, and it keeps that mechanism's two properties. **At
/// most one stroke a frame**, so a frame delayed over three strides' worth of water is one
/// stroke rather than three. And **the remainder is carried**, so the rate over a long swim
/// is exactly one stroke a stride rather than losing a fraction of a frame to truncation
/// every time — but carried no further than [`STROKE_CARRY`] of a stride, which is what
/// stops that same delayed frame from leaving enough credit behind to burst on the frames
/// after it.
const STROKE_BLOCKS: f32 = 1.3;
const MOUNTED_STROKE_BLOCKS: f32 = 2.6;
/// The most of a stride's overshoot that may be carried past a stroke. Half: a frame that
/// covered far more than a stride leaves at most half a stride of credit, so the next
/// stroke still needs real swimming under it.
const STROKE_CARRY: f32 = 0.5;
/// Movement under this much in one frame is interpolation noise rather than swimming, and
/// accumulates nothing. Without it a body held still by a jittering interpolation would
/// eventually stroke.
const STILL_BLOCKS: f32 = 0.0005;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cue {
    /// An entry, by a body that is mounted or not, at one of three forces.
    Splash { mounted: bool, force: Force },
    /// One stroke of a body swimming.
    Stroke { mounted: bool },
}

struct Voice {
    /// The body the sound follows, so a splash keeps being placed where the swimmer is.
    key: Entity,
    playback: Playback,
    occlusion: f32,
    next_occlusion: Instant,
}

/// One drawn body as the last frame that looked saw it.
#[derive(Clone, Copy)]
struct Seen {
    in_water: bool,
    feet: Vec3,
    at: Instant,
    /// How far this body has swum since its last stroke, in blocks. Zero out of water.
    swum: f32,
}

#[derive(Resource, Default)]
struct Waters {
    /// Every drawn body's last observed state, keyed by the entity its sounds follow.
    seen: HashMap<Entity, Seen>,
    playing: Vec<Voice>,
    cache: Vec<(Cue, Baked)>,
    rate: u32,
}

pub(super) fn register(app: &mut App) {
    app.init_resource::<Waters>().add_systems(
        Update,
        update
            .after(super::ApplySnapshots)
            .after(super::camera::AimCamera),
    );
}

pub(super) fn reset_world(world: &mut World) {
    crate::world::transition::reset::<Waters>(world);
}

/// Whether the box a body of `width` by `height` occupies with its feet at `feet` touches
/// any water.
///
/// The client's mirror of the server's `overlapsFluid`: the voxel span the box covers on
/// each axis, and any one of them being water is the answer. A chunk this session does not
/// hold reads as air through [`ChunkStore::block_at`] and therefore as not water, which is
/// the same direction the server's `Terrain.Fluid` answers an absent chunk in — a body over
/// unloaded world is not reported as swimming.
pub(super) fn in_water(
    store: &ChunkStore,
    feet: Vec3,
    width: f32,
    height: f32,
    size: usize,
) -> bool {
    let half = width * 0.5;
    let span = |low: f32, high: f32| {
        let (low, high) = (low.floor(), high.floor());
        if !low.is_finite() || !high.is_finite() {
            return None;
        }
        Some((low as i32)..=(high as i32))
    };
    let (Some(xs), Some(ys), Some(zs)) = (
        span(feet.x - half, feet.x + half),
        span(feet.y, feet.y + height),
        span(feet.z - half, feet.z + half),
    ) else {
        return false;
    };
    for y in ys {
        for z in zs.clone() {
            for x in xs.clone() {
                if palette::is_water(store.block_at(BlockCoord { x, y, z }, size)) {
                    return true;
                }
            }
        }
    }
    false
}

/// One drawn body as this frame has it.
#[derive(Clone, Copy)]
struct Drawn {
    key: Entity,
    feet: Vec3,
    mounted: bool,
}

impl Drawn {
    /// The box the server collides for this body: the mounted one is horse and rider
    /// together, which is both wider and more than a block taller.
    fn size(self) -> (f32, f32) {
        if self.mounted {
            (MOUNTED_WIDTH, MOUNTED_HEIGHT)
        } else {
            (PLAYER_WIDTH, PLAYER_HEIGHT)
        }
    }
}

struct Frame<'a> {
    now: Instant,
    store: &'a ChunkStore,
    size: usize,
    eye: Option<&'a Transform>,
}

impl Waters {
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
            Cue::Splash { mounted, force } => (
                sounds::splash(mounted, force),
                if mounted {
                    sounds::MOUNTED_SPLASH_SECONDS
                } else {
                    sounds::SPLASH_SECONDS
                },
            ),
            Cue::Stroke { mounted } => (
                sounds::stroke(mounted),
                if mounted {
                    sounds::MOUNTED_STROKE_SECONDS
                } else {
                    sounds::STROKE_SECONDS
                },
            ),
        };
        let baked = sound.bake(seconds, rate, 1188).ok()?;
        self.cache.push((cue, baked.clone()));
        Some(baked)
    }

    fn start(&mut self, mixer: &AudioMixer, cue: Cue, body: Drawn, frame: &Frame<'_>) {
        if self.playing.len() >= MAX_PLAYING {
            return;
        }
        let Some(eye) = frame.eye else {
            return;
        };
        let origin = body.feet + Vec3::Y * SURFACE_HEIGHT;
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
                key: body.key,
                playback,
                occlusion,
                next_occlusion: frame.now + OCCLUSION_PERIOD,
            });
        }
    }

    /// What each drawn body did between the last frame that looked and this one: an entry
    /// where it crossed into water, a stroke where it has swum a stride's worth since its
    /// last one, and nothing at all otherwise.
    ///
    /// A body seen for the first time is recorded and never sounded: it was not observed to
    /// cross anything or to swim anywhere. At most one cue per body per frame, and an entry
    /// wins — the frame a body arrives is the splash, and its first stroke comes a stride
    /// later.
    fn observe(&mut self, frame: &Frame<'_>, bodies: &[Drawn]) -> Vec<(Drawn, Cue)> {
        let mut cues = Vec::new();
        for &body in bodies {
            let (width, height) = body.size();
            let wet = in_water(frame.store, body.feet, width, height, frame.size);
            let before = self.seen.get(&body.key).copied();
            let mut swum = 0.0;
            match before {
                Some(before) if wet && !before.in_water => {
                    let gap = frame.now.saturating_duration_since(before.at).as_secs_f32();
                    let down = if (SPEED_WINDOW.0..=SPEED_WINDOW.1).contains(&gap) {
                        (before.feet.y - body.feet.y) / gap
                    } else {
                        0.0
                    };
                    cues.push((
                        body,
                        Cue::Splash {
                            mounted: body.mounted,
                            force: Force::of(down),
                        },
                    ));
                }
                Some(before) if wet => {
                    // **Horizontal only, and that is the whole of why a still swimmer is
                    // silent.** The server sinks a body that does nothing at
                    // `SwimSinkSpeed`, one block a second, so counting the vertical would
                    // stroke for a swimmer who is not swimming. A body carried sideways by
                    // a current *is* heard: this side cannot tell that from a swimmer's own
                    // intent, and either way a body is moving through water.
                    let moved = (body.feet - before.feet).xz().length();
                    swum = before.swum + if moved >= STILL_BLOCKS { moved } else { 0.0 };
                    let stride = if body.mounted {
                        MOUNTED_STROKE_BLOCKS
                    } else {
                        STROKE_BLOCKS
                    };
                    if swum >= stride {
                        swum = (swum - stride).min(stride * STROKE_CARRY);
                        cues.push((
                            body,
                            Cue::Stroke {
                                mounted: body.mounted,
                            },
                        ));
                    }
                }
                _ => {}
            }
            self.seen.insert(
                body.key,
                Seen {
                    in_water: wet,
                    feet: body.feet,
                    at: frame.now,
                    swum,
                },
            );
        }
        self.seen
            .retain(|key, _| bodies.iter().any(|body| body.key == *key));
        cues
    }

    fn advance(&mut self, mixer: &AudioMixer, frame: &Frame<'_>, bodies: &[Drawn]) {
        let placed: HashMap<Entity, Vec3> =
            bodies.iter().map(|body| (body.key, body.feet)).collect();
        self.playing.retain_mut(|voice| {
            let (Some(feet), Some(eye)) = (placed.get(&voice.key).copied(), frame.eye) else {
                return false;
            };
            let origin = feet + Vec3::Y * SURFACE_HEIGHT;
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
        for (body, cue) in self.observe(frame, bodies) {
            self.start(mixer, cue, body, frame);
        }
    }
}

type RiddenHorses<'w, 's> = Query<'w, 's, &'static ChildOf, (With<Horse>, Without<PaddockHorse>)>;

fn update(
    mut waters: ResMut<Waters>,
    session: Option<Res<Session>>,
    mixer: Option<Res<AudioMixer>>,
    store: Option<Res<ChunkStore>>,
    eyes: Query<&Transform, With<WorldCamera>>,
    bodies: Query<(Entity, &Transform), With<Body>>,
    ridden: RiddenHorses<'_, '_>,
) {
    let Some(session) = session else {
        *waters = Waters::default();
        return;
    };
    if session.is_changed() {
        *waters = Waters::default();
    }
    // PlayerPlugin also runs without WorldPlugin/AudioPlugin in headless consumers.
    let (Some(mixer), Some(store)) = (mixer, store) else {
        *waters = Waters::default();
        return;
    };
    let mounted: Vec<Entity> = ridden.iter().map(ChildOf::parent).collect();
    let drawn: Vec<Drawn> = bodies
        .iter()
        .map(|(key, pose)| Drawn {
            key,
            feet: pose.translation,
            mounted: mounted.contains(&key),
        })
        .collect();
    let frame = Frame {
        now: Instant::now(),
        store: &store,
        size: usize::from(session.0.chunk_size),
        eye: eyes.iter().next(),
    };
    waters.advance(&mixer, &frame, &drawn);
}

#[cfg(test)]
mod pins;

#[cfg(test)]
mod tests;
