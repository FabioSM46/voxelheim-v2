//! Server outcomes and visible action transitions only. No input, swings or health deltas.
mod guardian;
mod sounds;

use super::{AimCamera, ApplySnapshots, SnapshotBuffer, WorldCamera};
use crate::{
    audio::{
        AudioMixer, Bus, spatial,
        synth::{Baked, Playback, Rendering, Status},
    },
    net::{BlowInbox, BlowTarget, MobAction, MobKind, Session, Snapshot},
    world::ChunkStore,
};
use bevy::prelude::*;
use sounds::{CUES, Cue};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// One close encounter is audible across a small room or clearing, not across the valley.
/// Visibility binding remains mandatory even inside this presentation-only distance.
const COMBAT_RANGE: f32 = 32.0;
/// Generic contacts expire even if the output device stops draining its ring.
/// Guardian cues use their bounded recipe duration plus a half-second scheduling tail.
const MAX_PLAYBACK_AGE: Duration = Duration::from_secs(1);

#[derive(Resource, Default)]
struct CombatAudio {
    #[cfg(test)]
    started: Vec<(u64, Cue, Vec3)>,
    guardian: guardian::State,
    tick: Option<u32>,
    previous: HashMap<u64, (MobKind, MobAction)>,
    palette: Vec<(Cue, Baked)>,
    rate: u32,
    playing: Vec<Active>,
}

struct Active {
    playback: Playback,
    id: u64,
    target: BlowTarget,
    origin: Vec3,
    follows: bool,
    offset: Option<Vec3>,
    owner: Option<guardian::Owner>,
    expires: Duration,
    priority: u8,
}

#[derive(Clone, Copy)]
struct Pending {
    cue: Cue,
    id: u64,
    target: BlowTarget,
    origin: Vec3,
    follows: bool,
    offset: Option<Vec3>,
    owner: Option<guardian::Owner>,
}

pub(super) fn register(app: &mut App) {
    app.init_resource::<CombatAudio>().add_systems(
        Update,
        update
            .after(ApplySnapshots)
            .after(AimCamera)
            .after(super::encounters::reconcile),
    );
}

pub(super) fn reset_world(world: &mut World) {
    crate::world::transition::reset::<CombatAudio>(world);
}

/// No synthetic Idle on first sight: an already-chasing creature cannot announce a
/// transition we never saw. A newly visible Windup does carry a present attack telegraph.
fn transition(
    before: Option<(MobKind, MobAction)>,
    kind: MobKind,
    action: MobAction,
) -> Option<Cue> {
    if before == Some((kind, MobAction::Idle)) && action == MobAction::Chase {
        sounds::voice(kind, false)
    } else if action == MobAction::Windup && before != Some((kind, MobAction::Windup)) {
        sounds::voice(kind, true)
    } else {
        None
    }
}

fn visible_position(snapshot: &Snapshot, id: u64, target: BlowTarget) -> Option<Vec3> {
    match target {
        BlowTarget::Player => snapshot
            .entities
            .iter()
            .find(|entity| entity.entity_id == id)
            .map(|entity| Vec3::from_array(entity.pos)),
        BlowTarget::Mob(kind) => snapshot
            .mobs
            .iter()
            .find(|mob| mob.entity_id == id && mob.kind == kind)
            .map(|mob| Vec3::from_array(mob.pos)),
    }
}

fn mouth(kind: MobKind, feet: Vec3) -> Vec3 {
    feet + Vec3::Y * (super::mobs::body(kind).height * 0.75)
}

#[derive(bevy::ecs::system::SystemParam)]
struct Inputs<'w, 's> {
    time: Res<'w, Time<Real>>,
    session: Option<Res<'w, Session>>,
    snapshots: Option<Res<'w, SnapshotBuffer>>,
    mixer: Option<Res<'w, AudioMixer>>,
    blows: Option<ResMut<'w, BlowInbox>>,
    store: Option<Res<'w, ChunkStore>>,
    timelines: Option<Res<'w, crate::net::EncounterTimelineInbox>>,
    presentation: Option<Res<'w, super::encounters::EncounterPresentation>>,
    mobs: Query<'w, 's, &'static super::mobs::Mob>,
    eyes: Query<'w, 's, &'static Transform, With<WorldCamera>>,
}

fn update(mut state: ResMut<CombatAudio>, mut inputs: Inputs) {
    let state = &mut *state;
    #[cfg(test)]
    state.started.clear();
    let now = Instant::now();
    let snapshot = inputs
        .snapshots
        .as_deref()
        .and_then(SnapshotBuffer::latest_snapshot);
    let contacts = inputs
        .blows
        .as_deref_mut()
        .map_or_else(Vec::new, |inbox| inbox.take(snapshot, now));
    let Some(session) = inputs.session.as_deref() else {
        *state = CombatAudio::default();
        return;
    };
    if inputs
        .session
        .as_ref()
        .is_some_and(|session| session.is_changed())
    {
        *state = CombatAudio::default();
    }
    let Some(snapshot) = snapshot else {
        state.guardian = guardian::State::default();
        state.previous.clear();
        state.tick = None;
        state.playing.clear();
        return;
    };
    let mut pending: Vec<_> = contacts
        .into_iter()
        .map(|blow| Pending {
            cue: sounds::impact(blow.target),
            id: blow.target_entity_id,
            target: blow.target,
            origin: Vec3::from_array(blow.position),
            follows: false,
            offset: None,
            owner: None,
        })
        .collect();
    if state.tick != Some(snapshot.server_tick) {
        for mob in &snapshot.mobs {
            if let Some(cue) = transition(
                state.previous.get(&mob.entity_id).copied(),
                mob.kind,
                mob.action,
            ) {
                pending.push(Pending {
                    cue,
                    id: mob.entity_id,
                    target: BlowTarget::Mob(mob.kind),
                    origin: mouth(mob.kind, Vec3::from_array(mob.pos)),
                    follows: true,
                    offset: None,
                    owner: None,
                });
            }
        }
        state.previous = snapshot
            .mobs
            .iter()
            .map(|mob| (mob.entity_id, (mob.kind, mob.action)))
            .collect();
        state.tick = Some(snapshot.server_tick);
    }
    state.guardian.sample(
        snapshot,
        inputs.timelines.as_deref(),
        inputs.presentation.as_deref(),
        session.0.tick_rate,
        &mut pending,
    );
    state
        .guardian
        .footfalls(&inputs.mobs, snapshot, &mut pending);
    // Events/transitions are consumed even without a mixer or camera. Availability later
    // is never permission to replay earlier blows or an old pursuit transition.
    let Some((mixer, eye)) = inputs.mixer.as_deref().zip(inputs.eyes.iter().next()) else {
        state.playing.clear();
        return;
    };
    let rate = mixer.sample_rate();
    if state.rate != rate {
        state.playing.clear();
        state.palette = CUES
            .iter()
            .copied()
            .chain(guardian::sounds::CUES.into_iter().map(Cue::Guardian))
            .filter_map(|cue| {
                cue.describe()
                    .bake(cue.seconds(), rate, 19)
                    .ok()
                    .map(|baked| (cue, baked))
            })
            .collect();
        state.rate = rate;
    }
    let placement = |origin| {
        let occlusion = inputs.store.as_deref().map_or(0.0, |store| {
            spatial::occlusion(
                store,
                usize::from(session.0.chunk_size),
                eye.translation,
                origin,
            )
        });
        spatial::place(
            eye.translation,
            spatial::listener_yaw(eye.rotation),
            origin,
            COMBAT_RANGE,
            occlusion,
        )
    };
    state.playing.retain_mut(|active| {
        if inputs.time.elapsed() >= active.expires
            || active
                .owner
                .is_some_and(|owner| !state.guardian.valid(active.id, owner))
        {
            return false;
        }
        let Some(position) = visible_position(snapshot, active.id, active.target) else {
            return false;
        };
        if active.follows
            && let BlowTarget::Mob(kind) = active.target
        {
            active.origin = mouth(kind, position);
        }
        if let Some(offset) = active.offset {
            let yaw = snapshot
                .mobs
                .iter()
                .find(|mob| mob.entity_id == active.id)
                .map_or(0.0, |mob| mob.yaw);
            active.origin = position + Quat::from_rotation_y(yaw) * offset;
        }
        let mut placed = placement(active.origin);
        if active.owner.is_some() {
            placed.gain *= guardian::SOURCE_GAIN;
        }
        if placed.gain <= 0.0 {
            return false;
        }
        active.playback.place(placed);
        active.playback.pump() == Status::Playing
    });
    pending.sort_by_key(|pending| {
        std::cmp::Reverse(match pending.cue {
            Cue::Guardian(cue) => cue.priority(),
            _ => 3,
        })
    });
    for cue in pending {
        let mut placed = placement(cue.origin);
        if cue.owner.is_some() {
            placed.gain *= guardian::SOURCE_GAIN;
        }
        if placed.gain <= 0.0 {
            continue;
        }
        let Some((_, baked)) = state.palette.iter().find(|(kind, _)| *kind == cue.cue) else {
            continue;
        };
        if let Cue::Guardian(guardian_cue) = cue.cue {
            let count = state
                .playing
                .iter()
                .filter(|active| active.owner.is_some())
                .count();
            let per_boss = state
                .playing
                .iter()
                .filter(|active| active.owner.is_some() && active.id == cue.id)
                .count();
            if count >= guardian::MAX_GUARDIAN_SOURCES || per_boss >= guardian::MAX_PER_BOSS {
                // Admission can discard our own low-priority texture, never another bus.
                let victim = state
                    .playing
                    .iter()
                    .enumerate()
                    .filter(|(_, active)| {
                        active.owner.is_some()
                            && (per_boss < guardian::MAX_PER_BOSS || active.id == cue.id)
                            && active.priority < guardian_cue.priority()
                    })
                    .min_by_key(|(_, active)| active.priority)
                    .map(|(index, _)| index);
                let Some(victim) = victim else {
                    continue;
                };
                state.playing.remove(victim);
            }
        }
        // One claim per actual cue. No retry after refusal, including a claim which revokes
        // ambience: the existing policy discards that one-shot. Only granted sources enter
        // the vector, so the mixer itself bounds it and protects Voice/Master.
        if let Ok(mut playback) =
            Playback::start(mixer, Bus::Sfx, Rendering::Baked(baked.clone()), placed)
            && playback.pump() == Status::Playing
        {
            #[cfg(test)]
            state.started.push((cue.id, cue.cue, cue.origin));
            state.playing.push(Active {
                playback,
                id: cue.id,
                target: cue.target,
                origin: cue.origin,
                follows: cue.follows,
                offset: cue.offset,
                owner: cue.owner,
                priority: match cue.cue {
                    Cue::Guardian(cue) => cue.priority(),
                    _ => 3,
                },
                expires: inputs.time.elapsed()
                    + if cue.owner.is_some() {
                        Duration::from_secs_f32(cue.cue.seconds() + 0.5)
                    } else {
                        MAX_PLAYBACK_AGE
                    },
            });
        }
    }
}

#[cfg(test)]
mod tests;
