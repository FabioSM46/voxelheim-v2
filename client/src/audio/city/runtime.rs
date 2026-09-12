//! Nearest-first ownership. This reads structure state; no world rule reads this module.

use super::sound::{Kind, Palette, Stream};
use crate::{
    audio::{
        AudioMixer, Bus, SourceHandle,
        mixer::{MAX_SOURCES, SOURCE_CAPACITY, VOICE_RESERVE},
        spatial,
    },
    net::{Session, StructureKind, StructureState},
    player::{AimCamera, ApplySnapshots, SnapshotBuffer, WorldCamera},
    world::ChunkStore,
};
use bevy::prelude::*;

/// A hammer is heard at the smithy, not across the settlement; a fire belongs to the camp
/// gathered around it. A village is 28 blocks in radius, so at twelve a forge on its far
/// side is silent from its edge. These presentation ranges are not the gameplay safe radius.
const FORGE_RANGE: f32 = 12.0;
const FIRE_RANGE: f32 = 12.0;
/// The hammer is a bright transient over silence; below unity it stays in the yard it is in.
const FORGE_GAIN: f32 = 0.6;
const FIRE_GAIN: f32 = 1.0;
/// The benches are quiet work, heard by whoever is standing at them rather than across the
/// yard: every one carries less far than a fire, and the assertion keeps it that way.
const LEATHER_RANGE: f32 = 8.0;
const ARMOUR_RANGE: f32 = 10.0;
const ENCHANTING_RANGE: f32 = 6.0;
const LEATHER_GAIN: f32 = 0.5;
const ARMOUR_GAIN: f32 = 0.45;
const ENCHANTING_GAIN: f32 = 0.7;
const _: () = assert!(
    LEATHER_RANGE <= FIRE_RANGE && ARMOUR_RANGE <= FIRE_RANGE && ENCHANTING_RANGE <= FIRE_RANGE
);
/// Half the world's eight slots leaves room for wilderness beds and effects. At most two
/// of each kind prevents a row of forges from erasing the fires. The mixer may grant fewer,
/// always protects its Voice/Master reserve, and may revoke this lower-priority ambience.
const CITY_SOURCES: usize = (MAX_SOURCES - VOICE_RESERVE) / 2;
const PER_KIND: usize = 2;
/// Refused beds retry the current nearest selection four times a second. No backlog of
/// strikes is kept; a successful later claim starts a fresh stream at the current rate.
const RETRY_SECONDS: f32 = 0.25;
/// Three per-material rays per live emitter, ten times a second like the voice path.
const RAY_SECONDS: f32 = 0.1;

#[derive(Clone, Copy)]
struct Candidate {
    id: u64,
    kind: Kind,
    origin: Vec3,
    distance: f32,
}

impl Candidate {
    fn from_structure(structure: &StructureState, eye: Vec3) -> Option<Self> {
        let kind = match structure.kind {
            StructureKind::Forge => Kind::Forge,
            StructureKind::Campfire if structure.lit => Kind::Fire,
            StructureKind::LeatherBench => Kind::Leather,
            StructureKind::ArmourBench => Kind::Armour,
            StructureKind::EnchantingTable => Kind::Enchanting,
            StructureKind::Campfire | StructureKind::Tent | StructureKind::Runestone => {
                return None;
            }
        };
        let anchor = structure.anchor;
        // Anchor is the ground voxel. The source is half a block above its top, centred
        // horizontally on the anchor cell — the anvil, the fire, the cuirass form or the
        // lectern — rather than inside the floor that supports it.
        let origin = Vec3::new(
            anchor.x as f32 + 0.5,
            anchor.y as f32 + 1.5,
            anchor.z as f32 + 0.5,
        );
        let distance = eye.distance(origin);
        (distance.is_finite() && distance < carry(kind).range).then_some(Self {
            id: structure.structure_id,
            kind,
            origin,
            distance,
        })
    }

    /// The one placement every city source is given: the distance curve at its kind's
    /// range, then its kind's gain. Nothing scales a source up after this.
    fn place(&self, eye: Vec3, yaw: f32, occlusion: f32) -> spatial::Placement {
        let carry = carry(self.kind);
        let mut placement = spatial::place(eye, yaw, self.origin, carry.range, occlusion);
        placement.gain *= carry.gain;
        placement
    }
}

/// How far one kind carries and how loud it is at the source.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Carry {
    range: f32,
    gain: f32,
}

/// One row per audible kind.
fn carry(kind: Kind) -> Carry {
    match kind {
        Kind::Forge => Carry {
            range: FORGE_RANGE,
            gain: FORGE_GAIN,
        },
        Kind::Fire => Carry {
            range: FIRE_RANGE,
            gain: FIRE_GAIN,
        },
        Kind::Leather => Carry {
            range: LEATHER_RANGE,
            gain: LEATHER_GAIN,
        },
        Kind::Armour => Carry {
            range: ARMOUR_RANGE,
            gain: ARMOUR_GAIN,
        },
        Kind::Enchanting => Carry {
            range: ENCHANTING_RANGE,
            gain: ENCHANTING_GAIN,
        },
    }
}

/// Keep only a bounded shortlist while scanning the snapshot. Ties use stable server ids,
/// so snapshot ordering cannot reshuffle stationary emitters from frame to frame.
fn nearest(structures: &[StructureState], eye: Vec3) -> Vec<Candidate> {
    let mut selected: Vec<Candidate> = Vec::with_capacity(CITY_SOURCES + 1);
    for structure in structures {
        let Some(candidate) = Candidate::from_structure(structure, eye) else {
            continue;
        };
        let index = selected.partition_point(|other| {
            other
                .distance
                .total_cmp(&candidate.distance)
                .then(other.id.cmp(&candidate.id))
                .is_lt()
        });
        selected.insert(index, candidate);
        let mut counts = [0; Kind::COUNT];
        selected.retain(|candidate| {
            let count = &mut counts[candidate.kind.index()];
            *count += 1;
            *count <= PER_KIND
        });
        selected.truncate(CITY_SOURCES);
    }
    selected
}

#[derive(Resource, Default)]
struct City {
    palette: Option<Palette>,
    live: Vec<Emitter>,
    retry: f32,
    generation: u64,
}

struct Emitter {
    candidate: Candidate,
    source: SourceHandle,
    stream: Stream,
    pending: [f32; 512],
    start: usize,
    end: usize,
    occlusion: f32,
    since_rays: f32,
}

impl Emitter {
    fn pump(&mut self) -> bool {
        if !self.source.live() || self.source.mixer().sample_rate() != self.stream.rate {
            return false;
        }
        let mut budget = self.source.free().min(SOURCE_CAPACITY);
        while budget > 0 {
            if self.start == self.end {
                self.start = 0;
                self.end = budget.min(self.pending.len());
                self.stream.render(&mut self.pending[..self.end]);
            }
            let end = self.end.min(self.start + budget);
            let written = self.source.push(&self.pending[self.start..end]);
            self.start += written;
            budget -= written;
            if written == 0 {
                break;
            }
        }
        true
    }
}

pub(super) fn register(app: &mut App) {
    app.init_resource::<City>().add_systems(
        Update,
        play_city
            .after(crate::audio::apply_the_controls)
            .after(ApplySnapshots)
            .after(AimCamera),
    );
}

#[allow(clippy::too_many_arguments)]
fn play_city(
    mut city: ResMut<City>,
    mixer: Res<AudioMixer>,
    time: Res<Time>,
    session: Option<Res<Session>>,
    snapshots: Option<Res<SnapshotBuffer>>,
    store: Option<Res<ChunkStore>>,
    eyes: Query<&Transform, With<WorldCamera>>,
) {
    let city = &mut *city;
    let Some((session, snapshots, eye)) = session
        .as_ref()
        .zip(snapshots.as_ref())
        .zip(eyes.iter().next())
        .map(|((session, snapshots), eye)| (session, snapshots, eye))
    else {
        city.live.clear();
        city.retry = 0.0;
        return;
    };
    if session.is_changed() {
        city.live.clear();
        city.retry = 0.0;
    }
    let rate = mixer.0.sample_rate();
    if city
        .palette
        .as_ref()
        .is_none_or(|palette| palette.rate != rate)
    {
        city.live.clear();
        city.palette = Palette::new(rate).ok();
        city.retry = 0.0;
    }
    let wanted = nearest(snapshots.structures(), eye.translation);
    // Immediate cancellation discards queued samples too: a removed/doused/out-of-range
    // emitter cannot play a quarter-second of old ring contents after the server said stop.
    city.live.retain(|emitter| {
        emitter.source.live()
            && wanted.iter().any(|candidate| {
                candidate.id == emitter.candidate.id
                    && candidate.kind == emitter.candidate.kind
                    && candidate.origin == emitter.candidate.origin
            })
    });
    city.retry -= time.delta_secs();
    if city.retry <= 0.0 {
        city.retry = RETRY_SECONDS;
        if let Some(palette) = &city.palette {
            for (rank, candidate) in wanted.iter().enumerate() {
                if city
                    .live
                    .iter()
                    .any(|emitter| emitter.candidate.id == candidate.id)
                {
                    continue;
                }
                // One claim per candidate, nearest first. On refusal stop the pass: a farther
                // station must not leapfrog a nearer one while the callback frees dirty slots.
                let Some(source) = mixer.claim(Bus::Ambience) else {
                    // Another bus can revoke the nearest slot while leaving a farther
                    // city source alive. Give up our farthest lower-ranked owner so the
                    // next pass can restore nearest-first even with a smaller allowance.
                    let victim = city
                        .live
                        .iter()
                        .enumerate()
                        .filter_map(|(index, emitter)| {
                            wanted
                                .iter()
                                .position(|entry| entry.id == emitter.candidate.id)
                                .filter(|held_rank| *held_rank > rank)
                                .map(|held_rank| (held_rank, index))
                        })
                        .max();
                    if let Some((_, index)) = victim {
                        city.live.remove(index);
                    }
                    break;
                };
                source.require_rate(rate);
                city.generation = city.generation.wrapping_add(1);
                let seed = session.0.world_seed as u64
                    ^ candidate.id.rotate_left(23)
                    ^ city.generation.wrapping_mul(0x517cc1b727220a95);
                let Ok(stream) = palette.stream(candidate.kind, seed) else {
                    break;
                };
                city.live.push(Emitter {
                    candidate: *candidate,
                    source,
                    stream,
                    pending: [0.0; 512],
                    start: 0,
                    end: 0,
                    occlusion: 0.0,
                    since_rays: RAY_SECONDS,
                });
            }
        }
    }
    let yaw = spatial::listener_yaw(eye.rotation);
    city.live.retain_mut(|emitter| {
        emitter.since_rays += time.delta_secs();
        if emitter.since_rays >= RAY_SECONDS {
            emitter.since_rays = 0.0;
            emitter.occlusion = store.as_deref().map_or(0.0, |store| {
                spatial::occlusion(
                    store,
                    usize::from(session.0.chunk_size),
                    eye.translation,
                    emitter.candidate.origin,
                )
            });
        }
        emitter.source.place(
            emitter
                .candidate
                .place(eye.translation, yaw, emitter.occlusion),
        );
        emitter.pump()
    });
}

#[cfg(test)]
mod tests;
