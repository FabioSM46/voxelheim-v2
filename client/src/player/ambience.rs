//! A coarse look at the ground around the eye, read only from streamed voxels.
//!
//! [`Ambience`] is presentation: it says whether the blocks the client happens to
//! hold look grassy, snowy or sandy, and whether trees occur among them. It is never
//! sent and must never be read by input, targeting, placement or any other code that
//! decides an outcome. The server sends no biome, and this module does not invent one.

use bevy::prelude::*;

use super::camera::WorldCamera;
use crate::net::{BlockCoord, ChunkCoord, Session};
use crate::world::{BlockId, ChunkStore, palette};

/// How often the loaded ground is sampled.
pub const AMBIENCE_PERIOD_SECONDS: f32 = 1.0;
/// The fixed number of columns in the square lattice.
pub const AMBIENCE_SAMPLES: usize = 64;
/// The distance between neighbouring sample columns, in blocks.
pub const AMBIENCE_SPACING: i32 = 6;
/// The minimum readable columns from which a look may be claimed.
pub const AMBIENCE_MIN_SAMPLES: usize = 16;
/// How long a changed answer must remain unchanged before it is published.
pub const AMBIENCE_SETTLE_SECONDS: f32 = 3.0;

const AMBIENCE_SIDE: usize = 8;
const SCAN_ABOVE_EYE: i32 = 8;
const SCAN_BELOW_EYE: i32 = 24;
const WOODED_DENOMINATOR: usize = 8;

const _: () = assert!(AMBIENCE_SIDE * AMBIENCE_SIDE == AMBIENCE_SAMPLES);
const _: () = assert!(((AMBIENCE_SIDE - 1) as i32 * AMBIENCE_SPACING) % 2 == 0);

/// What the loaded surface around the eye looks like.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum GroundLook {
    /// There is not enough loaded evidence, or the votes do not have one winner.
    #[default]
    Unknown,
    Grass,
    Snow,
    Sand,
}

/// The settled presentation answer exposed to ambient renderers.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Ambience {
    pub ground: GroundLook,
    pub wooded: bool,
}

/// The two facts retained while one column is scanned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroundSample {
    pub top: BlockId,
    pub wooded: bool,
}

/// Reduces column observations to one cosmetic look.
///
/// Only known surface families vote. Wood and foliage are deliberately independent
/// from the top block: a trunk may cover a grassy column while the column still says
/// that the surrounding loaded span is wooded.
pub fn look_of(samples: &[GroundSample]) -> (GroundLook, bool) {
    if samples.len() < AMBIENCE_MIN_SAMPLES {
        return (GroundLook::Unknown, false);
    }
    let mut grass = 0;
    let mut snow = 0;
    let mut sand = 0;
    let mut wooded = 0;

    for sample in samples {
        match sample.top {
            palette::GRASS | palette::DIRT => grass += 1,
            palette::SNOW | palette::ICE => snow += 1,
            palette::SAND | palette::SANDSTONE => sand += 1,
            _ => {}
        }
        wooded += usize::from(sample.wooded);
    }

    let greatest = grass.max(snow).max(sand);
    let winners = usize::from(grass == greatest)
        + usize::from(snow == greatest)
        + usize::from(sand == greatest);
    let ground = match (greatest, winners) {
        (0, _) | (_, 2..) => GroundLook::Unknown,
        (_, 1) if grass == greatest => GroundLook::Grass,
        (_, 1) if snow == greatest => GroundLook::Snow,
        (_, 1) => GroundLook::Sand,
        _ => GroundLook::Unknown,
    };

    let wooded = !samples.is_empty() && wooded * WOODED_DENOMINATOR >= samples.len();
    (ground, wooded)
}

#[derive(Debug, Clone, Copy)]
struct Settler<T> {
    candidate: T,
    held_seconds: f32,
}

impl<T: Copy + Eq> Settler<T> {
    fn apply(&mut self, current: &mut T, observed: T) {
        if observed == *current {
            self.candidate = observed;
            self.held_seconds = 0.0;
            return;
        }
        if observed != self.candidate {
            self.candidate = observed;
            self.held_seconds = 0.0;
            return;
        }

        self.held_seconds += AMBIENCE_PERIOD_SECONDS;
        if self.held_seconds >= AMBIENCE_SETTLE_SECONDS {
            *current = observed;
            self.held_seconds = 0.0;
        }
    }
}

#[derive(Resource, Debug)]
pub(super) struct AmbienceState {
    cadence: Timer,
    ground: Settler<GroundLook>,
    wooded: Settler<bool>,
}

impl Default for AmbienceState {
    fn default() -> Self {
        Self {
            cadence: Timer::from_seconds(AMBIENCE_PERIOD_SECONDS, TimerMode::Repeating),
            ground: Settler {
                candidate: GroundLook::Unknown,
                held_seconds: 0.0,
            },
            wooded: Settler {
                candidate: false,
                held_seconds: 0.0,
            },
        }
    }
}

impl AmbienceState {
    fn is_reset(&self) -> bool {
        self.cadence.elapsed_secs() == 0.0
            && self.ground.candidate == GroundLook::Unknown
            && self.ground.held_seconds == 0.0
            && !self.wooded.candidate
            && self.wooded.held_seconds == 0.0
    }
}

/// Samples a fixed 8 by 8 lattice after the camera has reached this frame's eye.
pub(super) fn sample_the_ground(
    time: Res<Time>,
    session: Option<Res<Session>>,
    store: Option<Res<ChunkStore>>,
    eyes: Query<&Transform, With<WorldCamera>>,
    mut ambience: ResMut<Ambience>,
    mut state: ResMut<AmbienceState>,
) {
    let Some(session) = session else {
        return;
    };
    if !state.cadence.tick(time.delta()).just_finished() {
        return;
    }

    let observations = match (store.as_deref(), eyes.iter().next()) {
        (Some(store), Some(eye)) => {
            samples_at(store, eye.translation, usize::from(session.0.chunk_size))
        }
        _ => Vec::new(),
    };
    if observations.len() < AMBIENCE_MIN_SAMPLES {
        // Insufficient data is not a changed cosmetic opinion to settle: it is the
        // absence of an answer, and must become Unknown on this sample.
        if *ambience != Ambience::default() {
            *ambience = Ambience::default();
        }
        *state = AmbienceState::default();
        return;
    }
    let observed = look_of(&observations);

    state.ground.apply(&mut ambience.ground, observed.0);
    state.wooded.apply(&mut ambience.wooded, observed.1);
}

/// Clears a look as soon as there is no live session to own its loaded chunks.
pub(super) fn forget_ambience_without_a_session(
    session: Option<Res<Session>>,
    mut ambience: ResMut<Ambience>,
    mut state: ResMut<AmbienceState>,
) {
    if session.is_some() {
        return;
    }
    let ambience_changed = *ambience != Ambience::default();
    let state_changed = !state.is_reset();
    if ambience_changed {
        *ambience = Ambience::default();
    }
    if state_changed {
        *state = AmbienceState::default();
    }
}

pub(super) fn samples_at(store: &ChunkStore, eye: Vec3, chunk_size: usize) -> Vec<GroundSample> {
    if !eye.is_finite() || chunk_size == 0 {
        return Vec::new();
    }
    let eye = eye.floor().as_ivec3();
    let mut samples = Vec::with_capacity(AMBIENCE_SAMPLES);
    for z in 0..AMBIENCE_SIDE {
        for x in 0..AMBIENCE_SIDE {
            let Some(x) = eye.x.checked_add(lattice_offset(x)) else {
                continue;
            };
            let Some(z) = eye.z.checked_add(lattice_offset(z)) else {
                continue;
            };
            if let Some(sample) = scan_column(store, x, z, eye.y, chunk_size) {
                samples.push(sample);
            }
        }
    }
    samples
}

const fn lattice_offset(slot: usize) -> i32 {
    slot as i32 * AMBIENCE_SPACING - (AMBIENCE_SIDE as i32 - 1) * AMBIENCE_SPACING / 2
}

fn scan_column(
    store: &ChunkStore,
    x: i32,
    z: i32,
    eye_y: i32,
    chunk_size: usize,
) -> Option<GroundSample> {
    let low = eye_y.checked_sub(SCAN_BELOW_EYE)?;
    let high = eye_y.checked_add(SCAN_ABOVE_EYE)?;
    let size = i32::try_from(chunk_size).ok()?;
    let mut top = palette::AIR;
    let mut wooded = false;

    // `[eye - 24, eye + 8)` is exactly 32 lookups, high to low. Every chunk
    // intersecting the span must be present: otherwise an unseen upper voxel could
    // change the top, or an unseen lower voxel could change `wooded`.
    for y in (low..high).rev() {
        let pos = BlockCoord { x, y, z };
        let coord = ChunkCoord {
            cx: x.div_euclid(size),
            cy: y.div_euclid(size),
            cz: z.div_euclid(size),
        };
        store.get(coord)?;
        let block = store.block_at(pos, chunk_size);
        wooded |= matches!(block, palette::LOG | palette::LEAVES);
        if top == palette::AIR && block != palette::AIR {
            top = block;
        }
    }

    Some(GroundSample { top, wooded })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(top: BlockId) -> GroundSample {
        GroundSample { top, wooded: false }
    }

    #[test]
    fn majority_ties_no_votes_and_wooded_fraction_are_exact() {
        let mut majority = [sample(palette::GRASS); AMBIENCE_MIN_SAMPLES];
        majority[0] = sample(palette::DIRT);
        majority[1] = sample(palette::SAND);
        assert_eq!(look_of(&majority), (GroundLook::Grass, false));
        let mut tie = [sample(palette::GRASS); AMBIENCE_MIN_SAMPLES];
        tie[AMBIENCE_MIN_SAMPLES / 2..].fill(sample(palette::SAND));
        assert_eq!(look_of(&tie), (GroundLook::Unknown, false));
        assert_eq!(
            look_of(&[sample(palette::STONE); AMBIENCE_MIN_SAMPLES]),
            (GroundLook::Unknown, false)
        );

        let mut eighth = [sample(palette::SNOW); AMBIENCE_MIN_SAMPLES];
        eighth[..AMBIENCE_MIN_SAMPLES / WOODED_DENOMINATOR].fill(GroundSample {
            top: palette::SNOW,
            wooded: true,
        });
        assert_eq!(look_of(&eighth), (GroundLook::Snow, true));
        let mut below = [sample(palette::ICE); AMBIENCE_MIN_SAMPLES];
        below[0].wooded = true;
        assert_eq!(look_of(&below), (GroundLook::Snow, false));
        assert_eq!(
            look_of(&[sample(palette::GRASS); AMBIENCE_MIN_SAMPLES - 1]),
            (GroundLook::Unknown, false)
        );
    }

    #[test]
    fn alternating_ground_settles_once_on_the_fourth_sand() {
        let mut current = GroundLook::Unknown;
        let mut memory = Settler {
            candidate: GroundLook::Unknown,
            held_seconds: 0.0,
        };
        let sequence = [
            GroundLook::Grass,
            GroundLook::Sand,
            GroundLook::Grass,
            GroundLook::Sand,
            GroundLook::Sand,
            GroundLook::Sand,
            GroundLook::Sand,
        ];
        let mut changes = Vec::new();
        for (index, observed) in sequence.into_iter().enumerate() {
            let before = current;
            memory.apply(&mut current, observed);
            if current != before {
                changes.push(index);
            }
        }

        assert_eq!(changes, vec![6]);
        assert_eq!(current, GroundLook::Sand);

        let mut wooded = false;
        let mut wooded_memory = Settler {
            candidate: false,
            held_seconds: 0.0,
        };
        for observed in [true, false, true, true, true] {
            wooded_memory.apply(&mut wooded, observed);
        }
        assert!(!wooded, "three true readings only hold the candidate");
        wooded_memory.apply(&mut wooded, true);
        assert!(wooded, "the fourth true reading publishes it");
    }
}
