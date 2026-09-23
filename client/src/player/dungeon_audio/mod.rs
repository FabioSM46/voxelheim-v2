//! The dungeon's moving parts are heard: a lever's clunk, a rune stone waking, a grille
//! rattling up or down, a stone door grinding and a web torn apart.
//!
//! **Every sound follows a `BlockUpdate` the store has already applied, and nothing else.**
//! The world reports each voxel it changed as a [`BlockReplaced`] — what it was and what the
//! server made it — and this module reads those reports and makes a noise for the ones that
//! are a mechanism moving. Nothing here originates a request, predicts a lever or remembers
//! a puzzle: a lever pulled and refused makes no sound, because nothing moved; a lever
//! thrown back by the server's timer makes one, because something did.
//!
//! **What moved is read from the pair of ids, never from where they are.**
//!
//! - `LeverOff` ↔ `LeverOn` is a lever thrown.
//! - `RuneStone` → `RuneStoneLit` is a rune waking; the reverse is one going dark.
//! - `Cobweb` → air is a web torn.
//! - An iron grille ↔ air is a grille moving. A grille is only ever drawn as a door, so
//!   every one of its cells that moves in one frame is one grille, heard once from the
//!   middle of them.
//! - A solid block ↔ air in an instance is a stone door only when at least [`DOOR_CELLS`] of
//!   them move the same way in one frame. A door is nine cells or more and opens in one
//!   update; a player placing or breaking a block moves one. Below the threshold it is
//!   somebody building, and the mining sounds already answer for that.
//!
//! Placed through `spatial::place` on [`Bus::Sfx`] like every other effect, occluded by
//! whatever stands between the listener and the cell, and capped at [`MAX_PLAYING`] at once.
mod sounds;

use super::WorldCamera;
use crate::{
    audio::{
        AudioMixer, Bus, spatial,
        synth::{Baked, Playback, Rendering, Status},
    },
    net::{BlockCoord, Session},
    world::{BlockId, BlockReplaced, ChunkStore, palette, transition::CurrentWorld},
};
use bevy::prelude::*;
use sounds::Cue;
use std::time::{Duration, Instant};

/// How far away a moving part is still heard. A door grinding open is heard down a hall.
const RANGE: f32 = 36.0;
/// At most this many dungeon sounds at once; under pressure a sound is lost rather than
/// played late.
const MAX_PLAYING: usize = 6;
const OCCLUSION_PERIOD: Duration = Duration::from_millis(100);
/// How many solid cells turning to air (or back) in one frame make a door rather than a
/// player's edit. The smallest door in the first dungeon is nine cells.
const DOOR_CELLS: usize = 4;
/// How far towards the listener the occlusion ray stops short of a cell's centre, so the
/// solid lever or stone that is making the sound does not muffle itself.
const SOURCE_CLEARANCE: f32 = 0.75;

struct Voice {
    origin: Vec3,
    playback: Playback,
    occlusion: f32,
    next_occlusion: Instant,
}

#[derive(Resource, Default)]
struct DungeonSounds {
    playing: Vec<Voice>,
    cache: Vec<(Cue, Baked)>,
    rate: u32,
}

pub(super) fn register(app: &mut App) {
    app.init_resource::<DungeonSounds>()
        .add_message::<BlockReplaced>()
        .add_systems(
            Update,
            update
                .after(crate::world::ingest_world_updates)
                .after(super::camera::AimCamera),
        );
}

pub(super) fn reset_world(world: &mut World) {
    crate::world::transition::reset::<DungeonSounds>(world);
}

/// The centre of the voxel at `pos`.
fn centre(pos: BlockCoord) -> Vec3 {
    Vec3::new(pos.x as f32, pos.y as f32, pos.z as f32) + Vec3::splat(0.5)
}

/// Whether `block` is something a door is built of: solid, and none of the dungeon's own
/// moving parts, which are heard for what they are.
fn door_stone(block: BlockId) -> bool {
    palette::is_solid(block)
        && !palette::is_grille(block)
        && !matches!(
            block,
            palette::LEVER_OFF | palette::LEVER_ON | palette::RUNE_STONE | palette::RUNE_STONE_LIT
        )
}

/// Every sound one frame's changes make, and where each is heard from.
///
/// `instance` says whether the world is a dungeon: a door is only ever heard there, so a
/// crew raising a wall in the open world is never taken for one.
fn hear(changes: &[BlockReplaced], instance: bool) -> Vec<(Cue, Vec3)> {
    let mut heard = Vec::new();
    // Grilles up, grilles down, doors open, doors shut: cells gathered and heard once each.
    let mut groups: [Vec<Vec3>; 4] = Default::default();
    for change in changes {
        let at = centre(change.pos);
        let (before, after) = (change.before, change.after);
        let cue = match (before, after) {
            (palette::LEVER_OFF, palette::LEVER_ON) | (palette::LEVER_ON, palette::LEVER_OFF) => {
                Some(Cue::Lever)
            }
            (palette::RUNE_STONE, palette::RUNE_STONE_LIT) => Some(Cue::RuneIgnite),
            (palette::RUNE_STONE_LIT, palette::RUNE_STONE) => Some(Cue::RuneDouse),
            (palette::COBWEB, palette::AIR) => Some(Cue::WebTear),
            _ => None,
        };
        if let Some(cue) = cue {
            heard.push((cue, at));
            continue;
        }
        let group = if palette::is_grille(before) && after == palette::AIR {
            Some(0)
        } else if before == palette::AIR && palette::is_grille(after) {
            Some(1)
        } else if instance && door_stone(before) && after == palette::AIR {
            Some(2)
        } else if instance && before == palette::AIR && door_stone(after) {
            Some(3)
        } else {
            None
        };
        if let Some(group) = group {
            groups[group].push(at);
        }
    }
    for (index, cells) in groups.iter().enumerate() {
        let (cue, least) = match index {
            0 => (Cue::GrilleUp, 1),
            1 => (Cue::GrilleDown, 1),
            _ => (Cue::Door, DOOR_CELLS),
        };
        if cells.len() >= least {
            let middle = cells.iter().copied().sum::<Vec3>() / cells.len() as f32;
            heard.push((cue, middle));
        }
    }
    heard
}

impl DungeonSounds {
    fn bake(&mut self, cue: Cue, rate: u32) -> Option<Baked> {
        if self.rate != rate {
            self.rate = rate;
            self.cache.clear();
            self.playing.clear();
        }
        if let Some((_, baked)) = self.cache.iter().find(|(key, _)| *key == cue) {
            return Some(baked.clone());
        }
        let baked = cue.describe().bake(cue.seconds(), rate, 1295).ok()?;
        self.cache.push((cue, baked.clone()));
        Some(baked)
    }
}

/// The point the occlusion ray is cast to: short of the source, on the listener's side.
fn heard_from(origin: Vec3, eye: Vec3) -> Vec3 {
    let towards = (eye - origin).normalize_or_zero();
    origin + towards * SOURCE_CLEARANCE
}

fn update(
    mut sounds: ResMut<DungeonSounds>,
    mut changes: MessageReader<BlockReplaced>,
    session: Option<Res<Session>>,
    mixer: Option<Res<AudioMixer>>,
    store: Option<Res<ChunkStore>>,
    current: Option<Res<CurrentWorld>>,
    eyes: Query<&Transform, With<WorldCamera>>,
) {
    let changes: Vec<BlockReplaced> = changes.read().copied().collect();
    let (Some(session), Some(mixer), Some(store)) = (session, mixer, store) else {
        *sounds = DungeonSounds::default();
        return;
    };
    let Some(eye) = eyes.iter().next() else {
        return;
    };
    let size = usize::from(session.0.chunk_size);
    let now = Instant::now();
    let yaw = spatial::listener_yaw(eye.rotation);
    sounds.playing.retain_mut(|voice| {
        if now >= voice.next_occlusion {
            voice.occlusion = spatial::occlusion(
                &store,
                size,
                eye.translation,
                heard_from(voice.origin, eye.translation),
            );
            voice.next_occlusion = now + OCCLUSION_PERIOD;
        }
        voice.playback.place(spatial::place(
            eye.translation,
            yaw,
            voice.origin,
            RANGE,
            voice.occlusion,
        ));
        voice.playback.pump() == Status::Playing
    });

    let instance = current.as_deref().is_some_and(|world| world.id != 0);
    for (cue, origin) in hear(&changes, instance) {
        if sounds.playing.len() >= MAX_PLAYING || origin.distance(eye.translation) >= RANGE {
            continue;
        }
        let Some(baked) = sounds.bake(cue, mixer.sample_rate()) else {
            continue;
        };
        let occlusion = spatial::occlusion(
            &store,
            size,
            eye.translation,
            heard_from(origin, eye.translation),
        );
        if let Ok(playback) = Playback::start(
            &mixer,
            Bus::Sfx,
            Rendering::Baked(baked),
            spatial::place(eye.translation, yaw, origin, RANGE, occlusion),
        ) {
            sounds.playing.push(Voice {
                origin,
                playback,
                occlusion,
                next_occlusion: now + OCCLUSION_PERIOD,
            });
        }
    }
}

#[cfg(test)]
mod tests;
