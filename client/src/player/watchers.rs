//! Eyes at the edge of the dark: a pair of lights watching from the snow, and nothing behind
//! them.
//!
//! ## Deliberately not a creature
//!
//! `birds.rs` and `critters.rs` draw animals: a body with a path, a model and a life. What is
//! here has none of those, on purpose (#1192). A distant wolf is a pair of luminous eyes at the
//! far edge of what the player can see — no body to model, no ground to walk beyond the surface
//! it sits on, nothing to walk toward. It **appears, holds, and goes out**, and it never comes
//! nearer, because a pair of eyes that arrives is a monster and this is a mood.
//!
//! So the rule that makes it a mood is structural rather than tuned:
//!
//! - **Where a pair is, is decided once.** It is placed on the far surface when it lights, and
//!   its translation is never written again. Nothing here has a velocity to get wrong.
//! - **The distance is a floor, not a starting value.** A player may walk toward a pair — that
//!   is the player approaching, not the wolf — and the pair goes out as they do: it starts to
//!   fade at [`WATCH_RETIRE`], and at [`WATCH_FLOOR`] it is removed outright whatever its fade
//!   is doing. No pair is ever drawn inside the floor, at any speed the eye moves.
//!
//! ## The same country and hour as the howl, and nothing more
//!
//! The eyes are consistent with the wolf's howl without being tied to it: they belong to the
//! snow after dark, which is exactly where and when the wildlife table's wolf row is heard, and
//! they know nothing about the moment a howl plays. [`watching`] reads the hour the way that lane
//! does — a server that keeps no clock is day, where the flock's `Period::abroad` would fly it all
//! day — because a wood of eyes on a clockless server with no howl in it would be the two
//! disagreeing. `the_eyes_watch_from_where_and_when_the_howl_is_heard` holds the agreement.
//!
//! ## Cosmetic, and the same rules as everything else that moves without being a thing
//!
//! Nothing here is sent, read by gameplay, hit, targeted or counted. The population is bounded by
//! [`WATCHER_COUNT_MAX`], every seed is mixed from [`WATCH_SEED`] and never from the world seed,
//! and the eyes are the emissive pair `player/eyeshine.rs` shares with the owl and the mouse — a
//! surface that glows, not a light that falls on the snow.

use std::f32::consts::TAU;
use std::ops::RangeInclusive;

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use super::ambience::{Ambience, GroundLook};
use super::camera::WorldCamera;
use super::eyeshine::{BLANK_EYES, Eyeshine, eye_pair_mesh, eyeshine_material};
use super::sky::{self, Period, SkyClock};
use crate::net::{BlockCoord, ChunkCoord, Session};
use crate::world::{ChunkStore, palette};

/// The country the eyes watch from — the wolf's, as the wildlife table hears it.
pub(super) const WATCHED_FROM: GroundLook = GroundLook::Snow;

/// The half of the day they watch in — the wolf's, likewise.
pub(super) const WATCHING: Period = Period::Night;

/// The nearest a pair may ever be drawn to the eye, horizontally, in blocks. A hard floor.
pub(super) const WATCH_FLOOR: f32 = 40.0;

/// How near the eye may come before a pair starts to go out.
///
/// Eight blocks above the floor, so an eye walking toward a pair sees it fade rather than wink
/// out; one moving fast enough to cross the eight blocks inside the fade meets the floor, which
/// removes the pair outright. The floor is the guarantee and this is only the courtesy.
pub(super) const WATCH_RETIRE: f32 = 48.0;

/// Where a pair is placed: this far from the eye, horizontally, and no further.
///
/// Out at the edge of what reads as distance and well inside what a default client draws —
/// `settings`' eight chunks put the fog's start at 128 blocks — so a pair is a light across
/// the snow rather than a light in the fog. A client that draws less has less loaded ground out
/// there, and a column that is not loaded places no pair at all.
pub(super) const WATCH_PLACED: RangeInclusive<f32> = 52.0..=64.0;

/// How far the eye may go from a pair before it is let go, horizontally: a player who has
/// walked away from it is no longer being watched by it.
const WATCH_LEFT_BEHIND: f32 = 96.0;

/// The most pairs that may exist at once, fading ones included.
pub(super) const WATCHER_COUNT_MAX: usize = 3;

/// How long a pair takes to light, and to go out.
const WATCH_FADE_SECONDS: f32 = 1.5;

/// Each slot may light at most one pair in a window this long.
///
/// **The window is what makes the eyes rare.** A pair holds for six to fourteen seconds, and only
/// half of the windows light one at all ([`WATCH_TURNOUT`]), so three slots put a pair or two on
/// the horizon now and then rather than a ring of them always.
const WATCH_WINDOW_SECONDS: f32 = 30.0;
const WATCH_HOLD_SECONDS: RangeInclusive<f32> = 6.0..=14.0;
const WATCH_TURNOUT: f32 = 0.5;

/// How high the eyes sit over the surface under them, in blocks: a wolf's head.
const EYE_HEIGHT: f32 = 0.8;

/// How far above and below the eye's own height a far column is searched for its surface.
///
/// Snow country is mountainous, so the window is deep: forty blocks down and twenty-four up. The
/// cost is paid once per pair, when it lights, and never per frame.
const PROBE_ABOVE: f32 = 24.0;
const PROBE_BELOW: f32 = 40.0;

/// The one constant every watcher seed is mixed from — never the world seed, for the reason
/// `critters::CRITTER_SEED` gives.
const WATCH_SEED: u64 = 0x3E7E_5A7C_4D0F_1192;

/// A wolf's eyes at sixty blocks: pale green-gold, and far larger than life.
///
/// Authored in blocks — a watcher is drawn at a scale of one and has no model for the numbers to
/// be fractions of. Each eye is 0.16 blocks and their centres are 0.26 apart, which at the nearest
/// placement is 0.18° and 0.29°: two points two or three pixels across and three or four apart on
/// a 70° view, the smallest that still reads as a pair rather than as one star.
const WOLF_EYES: Eyeshine = Eyeshine {
    spread: 0.13,
    forward: 0.0,
    rise: 0.0,
    size: 0.16,
    colour: Color::srgb(0.70, 0.80, 0.50),
    glow: LinearRgba::rgb(2.2, 2.9, 1.4),
};

/// Whether the eyes watch where the eye is, right now.
///
/// **A missing clock is day here**, which is the wildlife lane's reading (`night_now(..)
/// .unwrap_or(0.0)`) and deliberately not the flock's (`Period::abroad(None)`, abroad always).
/// The module comment says why: the eyes answer to the howl.
pub(super) fn watching(ambience: &Ambience, night: Option<f32>) -> bool {
    ambience.ground == WATCHED_FROM && WATCHING.abroad(Some(night.unwrap_or(0.0)))
}

// ---------------------------------------------------------------------------
// When a pair is lit
// ---------------------------------------------------------------------------

/// Which window a slot is in, and how far into it, `elapsed` seconds into the session.
///
/// `critters::generation_of`'s arithmetic, staggered by slot so the slots do not all light and
/// go out together.
fn window_of(slot: usize, elapsed: f32) -> (i64, f32) {
    let stagger = WATCH_WINDOW_SECONDS * slot as f32 / WATCHER_COUNT_MAX as f32;
    let since = elapsed + stagger;
    let window = (since / WATCH_WINDOW_SECONDS).floor();
    (window as i64, since - window * WATCH_WINDOW_SECONDS)
}

/// The seed of one slot's pair in one window.
fn watch_seed(slot: usize, window: i64) -> u64 {
    mix(WATCH_SEED, mix(slot as u64, window as u64))
}

/// When a window's pair is lit, as seconds into the window, or `None` for a window that lights
/// no pair at all.
///
/// The span always ends with a whole fade still left in the window, so a pair goes out before
/// its slot's next window can light another.
fn lit_span(seed: u64) -> Option<(f32, f32)> {
    if unit(seed, SALT_TURNOUT) >= WATCH_TURNOUT {
        return None;
    }
    let hold = lerp(
        *WATCH_HOLD_SECONDS.start(),
        *WATCH_HOLD_SECONDS.end(),
        unit(seed, SALT_HOLD),
    );
    let start = unit(seed, SALT_START) * (WATCH_WINDOW_SECONDS - hold - WATCH_FADE_SECONDS);
    Some((start, start + hold))
}

// ---------------------------------------------------------------------------
// Where a pair is
// ---------------------------------------------------------------------------

/// The column one pair is placed over, `WATCH_PLACED` from `eye` on the pair's own bearing.
///
/// Horizontal: the `y` is the eye's, and the surface replaces it.
fn spot(seed: u64, eye: Vec3) -> Vec3 {
    let bearing = unit(seed, SALT_BEARING) * TAU;
    let distance = lerp(
        *WATCH_PLACED.start(),
        *WATCH_PLACED.end(),
        unit(seed, SALT_DISTANCE),
    );
    eye + Vec3::new(bearing.cos(), 0.0, bearing.sin()) * distance
}

/// The top face of the ground a pair may sit on in one far column, or `None` where it may not.
///
/// **`None` is several answers and every one of them is "no pair here":**
///
/// - a chunk the window crosses is not loaded — an absent chunk is not evidence of ground;
/// - the window's highest voxel is already filled, so the surface is above the window and a pair
///   placed at the top of it would be inside a hill;
/// - the first thing under the air is not ground a wolf stands on — water or a bush, which are not
///   solid, or leaves, which **are** (`palette` lets a body walk a canopy) and are excluded by name,
///   because eyes on top of a pine are eyes in the air;
/// - nothing at all is found, which is a chasm.
///
/// Walked downward from the top, so the answer is the highest ground and everything between it and
/// the eyes is air: a pair placed [`EYE_HEIGHT`] over it is never inside terrain by construction.
fn ground_at(store: &ChunkStore, column: Vec3, from: f32, chunk_size: usize) -> Option<f32> {
    let size = i32::try_from(chunk_size).ok().filter(|size| *size > 0)?;
    if !column.is_finite() || !from.is_finite() {
        return None;
    }
    let voxel = |value: f32| value.floor() as i32;
    let (x, z) = (voxel(column.x), voxel(column.z));
    let high = voxel(from + PROBE_ABOVE);
    let low = voxel(from - PROBE_BELOW);
    for y in (low..=high).rev() {
        let coord = ChunkCoord {
            cx: x.div_euclid(size),
            cy: y.div_euclid(size),
            cz: z.div_euclid(size),
        };
        store.get(coord)?;
        let block = store.block_at(BlockCoord { x, y, z }, chunk_size);
        if block == palette::AIR {
            continue;
        }
        let ground =
            palette::is_solid(block) && !matches!(block, palette::LEAVES | palette::BROAD_LEAVES);
        return (y < high && ground).then_some((y + 1) as f32);
    }
    None
}

/// The horizontal distance between two points.
fn across(a: Vec3, b: Vec3) -> f32 {
    Vec3::new(a.x - b.x, 0.0, a.z - b.z).length()
}

// ---------------------------------------------------------------------------
// Seeds
// ---------------------------------------------------------------------------

const SALT_TURNOUT: u64 = 1;
const SALT_HOLD: u64 = 2;
const SALT_START: u64 = 3;
const SALT_BEARING: u64 = 4;
const SALT_DISTANCE: u64 = 5;

/// SplitMix64's finalizer, as `critters::splitmix` and `birds::splitmix` are: an avalanche, so a
/// window asked about a thousand frames apart answers the same.
fn splitmix(mut hash: u64) -> u64 {
    hash = hash.wrapping_add(0x9E37_79B9_7F4A_7C15);
    hash = (hash ^ (hash >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    hash = (hash ^ (hash >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    hash ^ (hash >> 31)
}

fn mix(seed: u64, salt: u64) -> u64 {
    splitmix(seed ^ splitmix(salt))
}

/// One deterministic value in `[0, 1)` per `(seed, salt)`.
fn unit(seed: u64, salt: u64) -> f32 {
    (mix(seed, salt) >> 40) as f32 / 16_777_216.0
}

fn lerp(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

// ---------------------------------------------------------------------------
// The entities
// ---------------------------------------------------------------------------

/// The one eye-pair mesh and one material per slot, built once.
///
/// Per slot for the reason `critters::CritterVisuals` gives: each pair fades on its own.
#[derive(Resource, Debug)]
pub(super) struct WatcherVisuals {
    eyes: Handle<Mesh>,
    pool: [Handle<StandardMaterial>; WATCHER_COUNT_MAX],
}

/// One pair of eyes. The only entity there is: no body, no children.
///
/// It carries no `MobVisuals`, no name plate, no collider, no health and nothing the target
/// raycast or any other system reads.
#[derive(Component, Debug)]
pub(super) struct Watcher {
    /// Which slot lit it, and in which window, so it goes out when the clock leaves that window.
    slot: usize,
    window: i64,
    /// When in the window it goes out.
    ends: f32,
    /// How much of it is drawn: 0 dark, 1 lit.
    pub(super) fade: f32,
    /// What `fade` is moving towards. Zero is on its way out, and nothing moves it back.
    pub(super) wanted: f32,
    pool: usize,
    material: Handle<StandardMaterial>,
}

/// Builds the eye mesh and every material a pair will ever wear.
pub(super) fn create_visuals(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(WatcherVisuals {
        eyes: meshes.add(eye_pair_mesh(WOLF_EYES)),
        pool: std::array::from_fn(|_| materials.add(eyeshine_material(BLANK_EYES, 0.0))),
    });
}

/// The one camera, told apart from the entities these systems also hold mutably.
type EyeOnTheSnow = (With<WorldCamera>, Without<Watcher>);

/// Everything `keep_the_watchers` reads.
#[derive(SystemParam)]
pub(super) struct WatchInputs<'w> {
    ambience: Res<'w, Ambience>,
    session: Option<Res<'w, Session>>,
    store: Option<Res<'w, ChunkStore>>,
    clock: Res<'w, SkyClock>,
    time: Res<'w, Time>,
    visuals: Option<Res<'w, WatcherVisuals>>,
}

/// Decides which pairs should be lit, and lights the missing ones.
///
/// Runs after the camera and the ground sample, as the flock and the critters do. A slot tries
/// its window once — [`ground_at`] is paid when a pair lights and never again — so a window whose
/// spot has no ground fit to sit on simply stays dark.
pub(super) fn keep_the_watchers(
    read: WatchInputs<'_>,
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    eyes: Query<&Transform, EyeOnTheSnow>,
    mut watchers: Query<(&mut Watcher, &Transform)>,
    mut tried: Local<[Option<i64>; WATCHER_COUNT_MAX]>,
) {
    let WatchInputs {
        ambience,
        session,
        store,
        clock,
        time,
        visuals,
    } = read;
    let (Some(visuals), Some(eye)) = (visuals, eyes.iter().next()) else {
        return;
    };
    let eye = eye.translation;
    if !eye.is_finite() {
        return;
    }
    let night = session
        .as_deref()
        .and_then(|session| sky::night_now(&clock, session));
    let lit = watching(&ambience, night);
    let elapsed = time.elapsed_secs();

    let mut taken = [false; WATCHER_COUNT_MAX];
    let mut pool_taken = [false; WATCHER_COUNT_MAX];
    let mut alive = 0usize;
    for (mut watcher, at) in &mut watchers {
        alive += 1;
        pool_taken[watcher.pool] = true;
        if watcher.wanted == 0.0 {
            continue;
        }
        let (window, age) = window_of(watcher.slot, elapsed);
        let distance = across(at.translation, eye);
        if !lit
            || window != watcher.window
            || age >= watcher.ends
            || !(WATCH_RETIRE..=WATCH_LEFT_BEHIND).contains(&distance)
        {
            watcher.wanted = 0.0;
            continue;
        }
        taken[watcher.slot] = true;
    }

    let (true, Some(store), Some(session)) = (lit, store.as_deref(), session.as_deref()) else {
        return;
    };
    let chunk_size = usize::from(session.0.chunk_size);
    for slot in 0..WATCHER_COUNT_MAX {
        if taken[slot] || alive >= WATCHER_COUNT_MAX {
            continue;
        }
        let (window, age) = window_of(slot, elapsed);
        if tried[slot] == Some(window) {
            continue;
        }
        let seed = watch_seed(slot, window);
        let Some((starts, ends)) = lit_span(seed) else {
            tried[slot] = Some(window);
            continue;
        };
        if age < starts {
            continue;
        }
        tried[slot] = Some(window);
        if age >= ends {
            continue;
        }
        let column = spot(seed, eye);
        let Some(surface) = ground_at(store, column, eye.y, chunk_size) else {
            continue;
        };
        let Some(pool) = pool_taken.iter().position(|claimed| !claimed) else {
            break;
        };
        pool_taken[pool] = true;
        alive += 1;
        let material = visuals.pool[pool].clone();
        if let Some(mut written) = materials.get_mut(&material) {
            *written = eyeshine_material(WOLF_EYES, 0.0);
        }
        let at = Vec3::new(column.x, surface + EYE_HEIGHT, column.z);
        commands.spawn((
            Watcher {
                slot,
                window,
                ends,
                fade: 0.0,
                wanted: 1.0,
                pool,
                material: material.clone(),
            },
            Mesh3d(visuals.eyes.clone()),
            MeshMaterial3d(material),
            facing(at, eye),
            Visibility::Visible,
        ));
    }
}

/// A pair at `at`, its faces turned toward `eye` on the horizontal plane.
///
/// The eye-pair mesh looks along `-Z`, and `look_to` points `-Z` along its direction, so the
/// direction is the one *to* the eye. Turning to follow the player is watching, not approaching:
/// the rotation is written and the translation never is.
fn facing(at: Vec3, eye: Vec3) -> Transform {
    let mut transform = Transform::from_translation(at);
    if let Ok(toward) = Dir3::new(Vec3::new(eye.x - at.x, 0.0, eye.z - at.z)) {
        transform.look_to(toward.as_vec3(), Vec3::Y);
    }
    transform
}

/// Everything `run_the_watchers` reads.
#[derive(SystemParam)]
pub(super) struct RunInputs<'w> {
    session: Option<Res<'w, Session>>,
    store: Option<Res<'w, ChunkStore>>,
    time: Res<'w, Time>,
}

/// Fades every pair, turns it toward the eye, and enforces the floor.
///
/// **The floor is enforced here, every frame, and not left to the fade.** A pair the eye has come
/// within [`WATCH_FLOOR`] of is despawned on this frame however far through its fade it is, which
/// is what makes "never drawn within the floor" a fact at any speed rather than a hope about how
/// fast a player can run.
pub(super) fn run_the_watchers(
    read: RunInputs<'_>,
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    eyes: Query<&Transform, EyeOnTheSnow>,
    mut watchers: Query<(Entity, &mut Watcher, &mut Transform, &mut Visibility)>,
) {
    let RunInputs {
        session,
        store,
        time,
    } = read;
    let Some(eye) = eyes.iter().next().map(|eye| eye.translation) else {
        return;
    };
    let step = time.delta_secs() / WATCH_FADE_SECONDS;
    let submerged = session.as_deref().is_some_and(|session| {
        sky::submerged_at(store.as_deref(), eye, usize::from(session.0.chunk_size))
    });

    for (entity, mut watcher, mut transform, mut visibility) in &mut watchers {
        let fade = if watcher.wanted > watcher.fade {
            (watcher.fade + step).min(watcher.wanted)
        } else {
            (watcher.fade - step).max(watcher.wanted)
        };
        if across(transform.translation, eye) < WATCH_FLOOR
            || (watcher.wanted == 0.0 && fade <= 0.0)
        {
            commands.entity(entity).despawn();
            continue;
        }
        if fade != watcher.fade {
            watcher.fade = fade;
            if let Some(mut material) = materials.get_mut(&watcher.material) {
                *material = eyeshine_material(WOLF_EYES, fade);
            }
        }
        // The rotation only: see `facing`.
        let turned = facing(transform.translation, eye).rotation;
        if transform.rotation != turned {
            transform.rotation = turned;
        }
        let should = if submerged {
            Visibility::Hidden
        } else {
            Visibility::Visible
        };
        if *visibility != should {
            *visibility = should;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{BlockId, VoxelChunk};

    const CHUNK: usize = 32;

    /// A store over every chunk a box of `reach` around `centre` touches, holding whatever
    /// `block_at` names — `critters.rs`'s fixture, for a far column.
    fn blocks(centre: Vec3, reach: f32, block_at: impl Fn(IVec3) -> BlockId) -> ChunkStore {
        let span = CHUNK as i32;
        let low = (centre - Vec3::splat(reach)).floor().as_ivec3();
        let high = (centre + Vec3::splat(reach)).floor().as_ivec3();
        let mut store = ChunkStore::default();
        for cx in low.x.div_euclid(span)..=high.x.div_euclid(span) {
            for cy in low.y.div_euclid(span)..=high.y.div_euclid(span) {
                for cz in low.z.div_euclid(span)..=high.z.div_euclid(span) {
                    let mut chunk = VoxelChunk::all_air(CHUNK);
                    for ly in 0..CHUNK {
                        for lz in 0..CHUNK {
                            for lx in 0..CHUNK {
                                let at = IVec3::new(
                                    cx * span + lx as i32,
                                    cy * span + ly as i32,
                                    cz * span + lz as i32,
                                );
                                let block = block_at(at);
                                if block != palette::AIR {
                                    chunk.set(lx, ly, lz, block);
                                }
                            }
                        }
                    }
                    store.insert(ChunkCoord { cx, cy, cz }, chunk);
                }
            }
        }
        store
    }

    #[test]
    fn the_eyes_watch_only_from_the_snow_and_only_after_dark() {
        for ground in [
            GroundLook::Unknown,
            GroundLook::Grass,
            GroundLook::Sand,
            GroundLook::Snow,
        ] {
            for wooded in [false, true] {
                let country = Ambience { ground, wooded };
                let snow = ground == GroundLook::Snow;
                assert_eq!(watching(&country, Some(1.0)), snow, "{ground:?} at night");
                assert!(!watching(&country, Some(0.0)), "{ground:?} by day");
                // A server with no clock hears no howl, so it sees no eyes either.
                assert!(!watching(&country, None), "{ground:?} with no clock");
            }
        }
    }

    #[test]
    fn a_pair_is_placed_well_beyond_the_distance_it_starts_to_go_out_at() {
        // The chain the never-approach rule rests on: the floor, then the distance a pair starts
        // to go out at, then the nearest it is ever placed — each strictly beyond the last, so a
        // pair is never lit already inside its own retirement.
        assert!(WATCH_FLOOR < WATCH_RETIRE && WATCH_RETIRE < *WATCH_PLACED.start());
        assert!(*WATCH_PLACED.end() < WATCH_LEFT_BEHIND);
        let eye = Vec3::new(-300.5, 90.0, 1200.25);
        let mut nearest = f32::INFINITY;
        let mut farthest = 0.0f32;
        let mut bearings = std::collections::HashSet::new();
        for window in -40..40i64 {
            for slot in 0..WATCHER_COUNT_MAX {
                let seed = watch_seed(slot, window);
                let column = spot(seed, eye);
                let distance = across(column, eye);
                assert!(
                    (WATCH_PLACED.start() - 1e-3..=WATCH_PLACED.end() + 1e-3).contains(&distance),
                    "a pair was placed {distance} away"
                );
                assert_eq!(column.y, eye.y, "the spot carries a height of its own");
                nearest = nearest.min(distance);
                farthest = farthest.max(distance);
                let direction = (column - eye).normalize();
                bearings.insert(((direction.x.atan2(direction.z) + 3.2) * 2.0) as i32);
                // Reproducible: the same slot and window place the same spot from the same eye.
                assert_eq!(spot(watch_seed(slot, window), eye), column);
            }
        }
        // And they come from every side, not one.
        assert!(bearings.len() >= 10, "{} bearings", bearings.len());
        assert!(farthest - nearest > 8.0, "every pair at one distance");
    }

    #[test]
    fn a_window_lights_a_pair_now_and_then_and_it_goes_out_with_a_fade_to_spare() {
        let mut lit = 0usize;
        let windows = 400usize;
        for window in 0..windows as i64 {
            let seed = watch_seed(0, window);
            assert_eq!(lit_span(seed), lit_span(watch_seed(0, window)));
            let Some((starts, ends)) = lit_span(seed) else {
                continue;
            };
            lit += 1;
            let hold = ends - starts;
            assert!(
                WATCH_HOLD_SECONDS.contains(&hold),
                "window {window} held for {hold} s"
            );
            assert!(starts >= 0.0);
            assert!(
                ends + WATCH_FADE_SECONDS <= WATCH_WINDOW_SECONDS + 1e-3,
                "window {window} goes out after its window has ended"
            );
        }
        // Now and then: about half the windows, and never all or none of them.
        assert!(
            (windows / 3..=windows * 2 / 3).contains(&lit),
            "{lit} of {windows} windows lit a pair"
        );

        // The windows walk forward with the clock and the slots are staggered.
        for slot in 0..WATCHER_COUNT_MAX {
            let (first, age) = window_of(slot, 0.0);
            assert!((0.0..WATCH_WINDOW_SECONDS).contains(&age));
            let (later, _) = window_of(slot, WATCH_WINDOW_SECONDS * 3.0);
            assert_eq!(later, first + 3);
        }
        assert_ne!(window_of(0, 5.0).1, window_of(1, 5.0).1);
    }

    #[test]
    fn a_pair_sits_on_the_far_surface_and_never_inside_terrain() {
        let column = Vec3::new(8.5, 0.0, 8.5);
        let snow = |top: i32| {
            blocks(Vec3::new(8.0, 60.0, 8.0), 60.0, move |at| {
                if at.y < top {
                    palette::SNOW
                } else {
                    palette::AIR
                }
            })
        };
        // Flat snow whose top voxel is 49: the surface is its top face, 50.
        let flat = snow(50);
        assert_eq!(ground_at(&flat, column, 60.0, CHUNK), Some(50.0));
        // Lower than the eye by most of the window, and higher by most of it, both still found.
        assert_eq!(
            ground_at(&flat, column, 50.0 + PROBE_BELOW - 1.0, CHUNK),
            Some(50.0)
        );
        assert_eq!(
            ground_at(&flat, column, 50.0 - PROBE_ABOVE + 1.0, CHUNK),
            Some(50.0)
        );
        // A window buried in the hill has no surface to offer: the pair would be inside it.
        assert_eq!(
            ground_at(&flat, column, 50.0 - PROBE_ABOVE - 2.0, CHUNK),
            None
        );
        // Ground below the window is a chasm as far as this probe can tell.
        assert_eq!(
            ground_at(&flat, column, 50.0 + PROBE_BELOW + 2.0, CHUNK),
            None
        );

        // Water and leaves are not ground a wolf stands on, and eyes over them would be in a lake
        // or up a tree. Leaves are the case that needs its own exclusion: `palette` calls them
        // solid, so a probe asking only "would this hold a body up" would seat a pair on a pine.
        assert!(palette::is_solid(palette::LEAVES) && palette::is_solid(palette::BROAD_LEAVES));
        for over in [palette::WATER, palette::LEAVES, palette::BROAD_LEAVES] {
            let covered = blocks(Vec3::new(8.0, 60.0, 8.0), 60.0, move |at| match at.y {
                y if y < 50 => palette::SNOW,
                50 => over,
                _ => palette::AIR,
            });
            assert_eq!(
                ground_at(&covered, column, 60.0, CHUNK),
                None,
                "a pair was seated on block {over}"
            );
        }

        // And the answer, placed, is clear: every voxel from the surface up past the eyes is air.
        let surface = ground_at(&flat, column, 60.0, CHUNK).expect("flat snow is ground");
        let eyes = surface + EYE_HEIGHT;
        let voxel = |y: f32| BlockCoord {
            x: column.x.floor() as i32,
            y: y.floor() as i32,
            z: column.z.floor() as i32,
        };
        assert_eq!(flat.block_at(voxel(eyes), CHUNK), palette::AIR);
        assert_eq!(
            flat.block_at(voxel(surface - 0.5), CHUNK),
            palette::SNOW,
            "the eyes are not over the ground they were placed on"
        );

        // Nothing unloaded is read, and nothing unreadable panics.
        assert_eq!(ground_at(&ChunkStore::default(), column, 60.0, CHUNK), None);
        assert_eq!(ground_at(&flat, column, 60.0, 0), None);
        assert_eq!(ground_at(&flat, Vec3::NAN, 60.0, CHUNK), None);
    }

    #[test]
    fn a_pair_is_turned_to_look_at_the_eye_without_moving() {
        let at = Vec3::new(50.0, 40.8, -20.0);
        for eye in [
            Vec3::new(0.0, 60.0, 0.0),
            Vec3::new(100.0, 10.0, -20.0),
            Vec3::new(50.0, 40.0, 30.0),
        ] {
            let transform = facing(at, eye);
            assert_eq!(transform.translation, at, "turning moved the pair");
            let looking = transform.rotation * Vec3::NEG_Z;
            let toward = Vec3::new(eye.x - at.x, 0.0, eye.z - at.z).normalize();
            assert!(
                looking.dot(toward) > 0.999,
                "a pair at {at} looks along {looking}, not at {eye}"
            );
        }
    }
}
