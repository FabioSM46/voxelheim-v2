//! Bounded, disposable SFX. Mining observations authorize presentation; no sound
//! originates a request, advances progress, infers a break, or consumes a landed blow.
pub(super) mod sounds;

use super::{
    Body, EYE_HEIGHT, InputMode, LocalMount, SnapshotBuffer, WorldCamera,
    combat::{ITEM_RUSTY_SWORD, SwingSent},
    crafting::ITEM_IRON_SWORD,
    target::MiningFeedback,
};
use crate::{
    audio::{
        AudioMixer, Bus, spatial,
        synth::{Baked, Playback, Rendering, Status},
    },
    net::{
        BlockCoord, ChunkCoord, MiningActivity, MiningActivityInbox, MiningPhase, MiningTool,
        Session,
    },
    world::{
        ChunkStore,
        palette::{self, MaterialClass},
    },
};
use bevy::prelude::*;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

const RANGE: f32 = 32.0;
const MAX_PLAYING: usize = 8;
const OCCLUSION_PERIOD: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cue {
    Strike(MiningTool, MaterialClass),
    Break(MiningTool, MaterialClass),
    Swing(bool),
}

struct Attempt {
    event: MiningActivity,
    decoded: Instant,
    active: bool,
    completed: bool,
    next_strike: Instant,
}
struct Voice {
    actor: u64,
    activity: Option<u64>,
    completion: bool,
    target: Option<BlockCoord>,
    playback: Playback,
    occlusion: f32,
    next_occlusion: Instant,
}
#[derive(Resource, Default)]
struct Tools {
    attempts: HashMap<u64, Attempt>,
    playing: Vec<Voice>,
    cache: Vec<(Cue, Baked)>,
    rate: u32,
}

pub(super) fn register(app: &mut App) {
    app.init_resource::<Tools>()
        .init_resource::<MiningActivityInbox>()
        .add_message::<SwingSent>()
        .add_systems(
            Update,
            update
                .after(super::ApplySnapshots)
                .after(super::ApplyInputMode)
                .after(super::camera::AimCamera)
                .after(super::target::ApplyMiningFeedback)
                .after(super::combat::ApplyCombatInput),
        );
}
pub(super) fn reset_world(world: &mut World) {
    crate::world::transition::reset::<Tools>(world);
}

fn loaded(store: &ChunkStore, pos: BlockCoord, size: usize) -> bool {
    let Ok(size) = i32::try_from(size) else {
        return false;
    };
    if size == 0 {
        return false;
    }
    store
        .get(ChunkCoord {
            cx: pos.x.div_euclid(size),
            cy: pos.y.div_euclid(size),
            cz: pos.z.div_euclid(size),
        })
        .is_some()
}

impl Tools {
    // High-water ids survive lease expiry and completion. Only actor disappearance or
    // world/session replacement clears them, as the wire contract specifies.
    fn observe(
        &mut self,
        event: MiningActivity,
        decoded: Instant,
        now: Instant,
        tick: Option<u32>,
        visible: bool,
    ) -> bool {
        if tick != Some(event.tick)
            || !visible
            || now.saturating_duration_since(decoded) >= MiningActivityInbox::LEASE
        {
            return false;
        }
        if let Some(old) = self.attempts.get(&event.actor_entity_id) {
            if event.activity_id < old.event.activity_id {
                return false;
            }
            if event.activity_id == old.event.activity_id {
                if old.completed
                    || event.pos != old.event.pos
                    || event.tool != old.event.tool
                    || event.block_id != old.event.block_id
                {
                    return false;
                }
                let delta = event.tick.wrapping_sub(old.event.tick);
                if delta >= (1 << 31) || (delta == 0 && event.phase == MiningPhase::Active) {
                    return false;
                }
            }
        }
        let newer = self
            .attempts
            .get(&event.actor_entity_id)
            .is_none_or(|old| old.event.activity_id != event.activity_id);
        if newer {
            // A new attempt ends the old repeating strike, not the already-authorized
            // collapse. Completed -> Active can arrive in the same inbox batch.
            self.playing.retain(|voice| {
                voice.actor != event.actor_entity_id || voice.activity.is_none() || voice.completion
            });
        }
        let next_strike = self
            .attempts
            .get(&event.actor_entity_id)
            .filter(|old| !newer && old.active)
            .map_or(now, |old| old.next_strike);
        let completed = event.phase == MiningPhase::Completed;
        self.attempts.insert(
            event.actor_entity_id,
            Attempt {
                event,
                decoded,
                active: !completed,
                completed,
                next_strike,
            },
        );
        if completed {
            self.playing.retain(|voice| {
                voice.actor != event.actor_entity_id || voice.activity != Some(event.activity_id)
            });
        }
        completed
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
            Cue::Strike(tool, material) => {
                let mut sound = sounds::strike(tool, material);
                // The existing bare-hand action is its mining punch, not an air attack.
                // Its soft whoosh shares one source with each punch's contact transient.
                if tool == MiningTool::Hand {
                    sound.layers.extend(sounds::swing(false).layers);
                }
                (sound, sounds::STRIKE_SECONDS)
            }
            Cue::Break(tool, material) => (sounds::breaking(tool, material), sounds::BREAK_SECONDS),
            Cue::Swing(weapon) => (sounds::swing(weapon), sounds::SWING_SECONDS),
        };
        let baked = sound.bake(seconds, rate, 984).ok()?;
        self.cache.push((cue, baked.clone()));
        Some(baked)
    }

    fn start(
        &mut self,
        mixer: &AudioMixer,
        cue: Cue,
        actor: u64,
        activity: Option<u64>,
        frame: &Frame<'_>,
    ) {
        // No deferred sound queue: source pressure loses a strike or completion, never
        // plays it later over another action. A future active renewal can strike again.
        if self.playing.len() >= MAX_PLAYING {
            return;
        }
        let Some(origin) = frame.positions.get(&actor).copied() else {
            return;
        };
        let Some(eye) = frame.eye else {
            return;
        };
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
                actor,
                activity,
                completion: matches!(cue, Cue::Break(..)),
                target: activity.and_then(|id| {
                    self.attempts
                        .get(&actor)
                        .filter(|attempt| attempt.event.activity_id == id)
                        .map(|attempt| attempt.event.pos)
                }),
                playback,
                occlusion,
                next_occlusion: frame.now + OCCLUSION_PERIOD,
            });
        }
    }

    fn advance(&mut self, mixer: &AudioMixer, frame: &Frame<'_>) {
        self.attempts.retain(|actor, _| frame.visible(*actor));
        for attempt in self.attempts.values_mut() {
            if frame.now.saturating_duration_since(attempt.decoded) >= MiningActivityInbox::LEASE
                || !loaded(frame.store, attempt.event.pos, frame.size)
                || (attempt.event.actor_entity_id == frame.local && !frame.local_mining)
            {
                attempt.active = false;
            }
        }
        self.playing.retain_mut(|voice| {
            if !frame.visible(voice.actor) || frame.eye.is_none() {
                return false;
            }
            // A completion owns its original target and bounded baked tail even after
            // the actor starts elsewhere. Visibility/world/device cancellation still applies.
            if voice
                .target
                .is_some_and(|pos| !loaded(frame.store, pos, frame.size))
            {
                return false;
            }
            if let Some(id) = voice.activity
                && !voice.completion
            {
                let Some(attempt) = self.attempts.get(&voice.actor) else {
                    return false;
                };
                if attempt.event.activity_id != id || !attempt.active {
                    return false;
                }
            }
            let Some(origin) = frame.positions.get(&voice.actor).copied() else {
                return false;
            };
            let eye = frame.eye.expect("checked above");
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
        let mut due = Vec::new();
        for attempt in self.attempts.values_mut() {
            if attempt.active && frame.now >= attempt.next_strike {
                // Share the visual punch's cosmetic cadence. Never catch up a delayed frame.
                attempt.next_strike = frame.now
                    + Duration::from_secs_f32(1.0 / super::hands::MINE_PUNCHES_PER_SECOND);
                due.push(attempt.event);
            }
        }
        due.sort_by_key(|event| event.actor_entity_id);
        for event in due {
            self.start(
                mixer,
                Cue::Strike(event.tool, palette::material_class(event.block_id)),
                event.actor_entity_id,
                Some(event.activity_id),
                frame,
            );
        }
    }
}

struct Frame<'a> {
    now: Instant,
    snapshots: &'a SnapshotBuffer,
    positions: &'a HashMap<u64, Vec3>,
    store: &'a ChunkStore,
    size: usize,
    local: u64,
    local_mining: bool,
    eye: Option<&'a Transform>,
}
impl Frame<'_> {
    fn visible(&self, actor: u64) -> bool {
        self.snapshots.holds_entity(actor) && !self.snapshots.player_is_dead(actor)
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "one presentation system reads network, world, view and audio resources"
)]
fn update(
    mut tools: ResMut<Tools>,
    mut inbox: ResMut<MiningActivityInbox>,
    mut swings: MessageReader<SwingSent>,
    session: Option<Res<Session>>,
    mixer: Option<Res<AudioMixer>>,
    snapshots: Res<SnapshotBuffer>,
    store: Option<Res<ChunkStore>>,
    eyes: Query<&Transform, With<WorldCamera>>,
    bodies: Query<(&Body, &Transform)>,
    feedback: Res<MiningFeedback>,
    mode: Res<InputMode>,
    mount: Res<LocalMount>,
) {
    let now = Instant::now();
    let observations = inbox.take(now);
    let swing_items: Vec<_> = swings.read().map(|event| event.item_id).collect();
    let Some(session) = session else {
        *tools = Tools::default();
        return;
    };
    if session.is_changed() {
        *tools = Tools::default();
    }
    // PlayerPlugin also runs without WorldPlugin/AudioPlugin in headless consumers.
    let (Some(mixer), Some(store)) = (mixer, store) else {
        *tools = Tools::default();
        return;
    };
    let positions = bodies
        .iter()
        .map(|(body, pose)| (body.0, pose.translation + Vec3::Y * EYE_HEIGHT))
        .collect();
    let frame = Frame {
        now,
        snapshots: &snapshots,
        positions: &positions,
        store: &store,
        size: usize::from(session.0.chunk_size),
        local: session.0.entity_id,
        local_mining: feedback.progress() != 0
            && *mode == InputMode::Playing
            && !mode.is_changed()
            && !mount.mounted(),
        eye: eyes.iter().next(),
    };
    for (event, decoded) in observations {
        let visible = frame.visible(event.actor_entity_id) && loaded(&store, event.pos, frame.size);
        if tools.observe(event, decoded, now, snapshots.latest_tick(), visible) {
            tools.start(
                &mixer,
                Cue::Break(event.tool, palette::material_class(event.block_id)),
                event.actor_entity_id,
                Some(event.activity_id),
                &frame,
            );
        }
    }
    tools.advance(&mixer, &frame);
    // The transport's actual swing signal includes ranged actions. Only a melee blade
    // sweeps through air here; bow release, casting and landed impacts are not this cue.
    for item in swing_items {
        if [ITEM_RUSTY_SWORD, ITEM_IRON_SWORD].contains(&item)
            && !mount.mounted()
            && frame.visible(frame.local)
        {
            tools.start(&mixer, Cue::Swing(true), frame.local, None, &frame);
        }
    }
}

#[cfg(test)]
mod pins;

#[cfg(test)]
mod tests;
