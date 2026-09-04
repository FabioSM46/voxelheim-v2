//! Rain, snow and sand in the air: one mesh whose vertices move.
//!
//! ## Why this is a mesh and not a particle system
//!
//! `player/structures.rs` argues against a particle system twice, and this module does not
//! contradict it. A particle system is a general mechanism — emitters, lifetimes, per-particle
//! state, a simulation step and a crate to own all of it — and what falling weather needs is
//! none of that. Every quad here is a pure function of its own index and the elapsed time:
//! there is no state to advance, nothing to be born or to die, and no allocation after the
//! mesh exists. [`PRECIP_QUADS`] positions are recomputed from scratch every frame and
//! written back into the one mesh, which is what Minecraft-style rain is.
//!
//! That has a consequence worth naming rather than discovering: **the volume is anchored to
//! the camera, not to the world**. A quad wraps back into a box that travels with the eye, so
//! a player who sprints does not outrun the rain and a player who stands still sees it fall
//! past them. Nothing here tracks where a drop *was* in the world, because nothing needs to.
//!
//! ## Presentation only
//!
//! Nothing in this module decides anything. The server has already applied the cold, the
//! slowed step and the doused fire, and those arrive as vitals, as position and as
//! `StructureState::lit`. What is left here is geometry the size of the weather the server
//! named. A client that read a rule back out of these quads — reach, speed, visibility to a
//! mob — would be deciding a gameplay outcome from presentation data.
//!
//! ## Water wins
//!
//! An eye inside a voxel of water sees the underwater sky and no precipitation at all,
//! whatever the sky above the surface is doing. That is the same override `player/sky.rs`
//! applies to the fog, read through the same [`sky::submerged_at`] so there is one answer to
//! "is the eye under water" rather than two.
//!
//! ## Roofs are per column
//!
//! Falling weather asks whether a solid voxel stands above each quad. The answer is cached
//! across the fixed 25x25 block footprint and invalidated by every streamed-world change;
//! one frame therefore walks each used column at most once, while an edit, arrival or unload
//! cannot leave an old roof behind. Sand does not use this answer: its source is horizontal
//! and needs a direction rule of its own.

use std::f32::consts::TAU;

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::ecs::system::SystemParam;
use bevy::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;

use super::Weather;
use super::camera::WorldCamera;
use super::sky;
use crate::net::{Session, WeatherKind};
use crate::world::ChunkStore;

/// How many quads the volume holds, drawn or not.
///
/// The mesh is this size for the whole session: intensity chooses how many of them carry
/// area, never how many exist. A mesh that grew and shrank would reallocate its vertex
/// buffer as the weather turned, and reallocation is the one cost a fixed budget removes.
pub(super) const PRECIP_QUADS: usize = 600;

/// The box the quads live in, in blocks, centred on the eye.
///
/// Wider than it is tall on purpose: what a player sees of falling weather is mostly the
/// horizontal spread in front of them, and sixteen blocks of height is already more than a
/// drop falling at [`RAIN_FALL_SPEED`] crosses in a second.
const VOLUME: Vec3 = Vec3::new(24.0, 16.0, 24.0);

/// How many block columns the moving volume can overlap on either horizontal axis.
///
/// A 24-block half-open span crosses at most 25 integer columns when the eye is between
/// block boundaries. The cache is an inline array of that exact footprint: no hash table,
/// no growth and no allocation while weather is being drawn.
const SHELTER_SIDE: usize = VOLUME.x as usize + 1;
const SHELTER_COLUMNS: usize = SHELTER_SIDE * SHELTER_SIDE;

/// A rain streak, in blocks: narrow and long, so it reads as motion rather than as a dot.
const RAIN_SIZE: Vec2 = Vec2::new(0.04, 0.5);
/// A snowflake, in blocks. Square, and larger than a rain streak is wide, because a flake
/// falling at [`SNOW_FALL_SPEED`] is something the eye follows rather than a smear.
const SNOW_SIZE: Vec2 = Vec2::new(0.12, 0.12);
/// A mote of sand, in blocks: between the two, and square like the flake.
const SAND_SIZE: Vec2 = Vec2::new(0.08, 0.08);

/// Calm and driving speeds, in blocks per second. Intensity interpolates between
/// each pair after the low-strength dead zone described by [`driving_strength`].
const RAIN_FALL_SPEED: [f32; 2] = [18.0, 27.0];
const SNOW_FALL_SPEED: [f32; 2] = [1.5, 3.0];
const SAND_DRIFT_SPEED: [f32; 2] = [12.0, 18.0];

/// A fixed unit bearing in world XZ, so a gust never turns with the camera.
/// The 3-4-5 triangle gives an exact normal without runtime normalisation.
const WIND_DIRECTION: Vec3 = Vec3::new(0.6, 0.0, 0.8);
/// How many blocks per second a full gust adds to falling weather's horizontal travel.
const WIND_SPEED: f32 = 6.0;
/// Intensities through this value carry no visible wind lean. The first quarter of
/// the wire scale is calm; above it the lean grows continuously to [`WIND_SPEED`].
const CALM_INTENSITY: u8 = 64;

/// How far a flake wanders to either side of where it would have fallen, in blocks, and
/// how often it completes that wander.
///
/// The drift is what separates snow from slow rain. It is per-quad in phase, out of the
/// same seed the starting position comes from, so no two flakes swing together.
const SNOW_DRIFT_BLOCKS: f32 = 0.5;
const SNOW_DRIFT_HZ: f32 = 0.18;

/// What the blizzard multiplies the flake speed by, and it is the whole of what the
/// blizzard changes here.
///
/// A blizzard is snow that has stopped being weather and started being a hazard, so it
/// draws the volume full whatever intensity says (see [`visible_quads`]) and moves it twice
/// as fast. The sky tint at full and the storm's own countdown are not this module's:
/// the tint is the rest of #466 and the countdown is #470.
const BLIZZARD_SPEED_FACTOR: f32 = 2.0;

/// How opaque one quad is.
///
/// Six tenths, which is what lets six hundred of them stack into weather rather than into a
/// wall: a quad that is nearly opaque hides the terrain the weather is supposed to be
/// falling in front of, and one that is nearly clear cannot be seen against a bright sky.
const PRECIP_ALPHA: f32 = 0.6;

/// Rain, as sRGB with [`PRECIP_ALPHA`]: a pale blue-grey, bright enough to read against wet
/// stone and dark enough not to be mistaken for snow.
const RAIN_COLOUR: Color = Color::srgba(0.62, 0.68, 0.78, PRECIP_ALPHA);
/// Snow, and the blizzard's colour too: near-white with the faintest blue in it.
const SNOW_COLOUR: Color = Color::srgba(0.94, 0.96, 1.0, PRECIP_ALPHA);
/// Sand: the ochre of the desert it was lifted out of.
const SAND_COLOUR: Color = Color::srgba(0.76, 0.62, 0.34, PRECIP_ALPHA);

/// Marks the one entity the whole volume is drawn as.
#[derive(Component)]
pub(super) struct PrecipitationVolume;

/// The one mesh and the three materials the volume is drawn with.
///
/// Three rather than four: a blizzard is snow, and giving it a material of its own would be
/// a second place to change what snow looks like.
#[derive(Resource, Debug)]
pub(super) struct PrecipitationVisuals {
    mesh: Handle<Mesh>,
    rain: Handle<StandardMaterial>,
    snow: Handle<StandardMaterial>,
    sand: Handle<StandardMaterial>,
}

/// The highest solid voxel in one cached world column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColumnShelter {
    Unmeasured,
    Open,
    Roof(i32),
}

/// Bounds covered by [`PrecipitationShelter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ShelterBounds {
    min_x: i32,
    min_y: i32,
    min_z: i32,
    max_y: i32,
    chunk_size: usize,
}

/// Shared, bounded shelter work for the camera-centred precipitation volume.
///
/// A column is walked from the top of the streamed view down only the first time a quad
/// uses it. Later quads and later frames reuse the highest solid voxel. Any store change
/// clears the answers, so a block edit, chunk arrival or unload is visible on the next
/// draw. Moving the volume onto a different block footprint clears them too.
#[derive(Resource, Debug)]
struct PrecipitationShelter {
    bounds: Option<ShelterBounds>,
    columns: [ColumnShelter; SHELTER_COLUMNS],
    #[cfg(test)]
    scans: usize,
}

impl Default for PrecipitationShelter {
    fn default() -> Self {
        Self {
            bounds: None,
            columns: [ColumnShelter::Unmeasured; SHELTER_COLUMNS],
            #[cfg(test)]
            scans: 0,
        }
    }
}

impl PrecipitationShelter {
    fn prepare(&mut self, eye: Vec3, chunk_size: usize, view_distance: u8) {
        let size = i32::try_from(chunk_size).expect("the protocol's chunk size fits i32");
        let eye_y = eye.y.floor() as i32;
        let top_chunk = eye_y
            .div_euclid(size)
            .saturating_add(i32::from(view_distance));
        let bounds = ShelterBounds {
            min_x: (eye.x - VOLUME.x * 0.5).floor() as i32,
            min_y: (eye.y - VOLUME.y * 0.5).floor() as i32,
            min_z: (eye.z - VOLUME.z * 0.5).floor() as i32,
            // The server streams a cube around the player. Its upper chunk face is the
            // farthest roof this client can know about, and makes every walk finite even
            // at the protocol's maximum view distance.
            max_y: top_chunk.saturating_add(1).saturating_mul(size),
            chunk_size,
        };

        if self.bounds != Some(bounds) {
            self.bounds = Some(bounds);
            self.columns.fill(ColumnShelter::Unmeasured);
        }
    }

    fn invalidate(&mut self) {
        self.bounds = None;
        self.columns.fill(ColumnShelter::Unmeasured);
    }

    fn is_sheltered(&mut self, store: &ChunkStore, centre: Vec3) -> bool {
        let Some(bounds) = self.bounds else {
            return false;
        };
        let x = centre.x.floor() as i32;
        let y = centre.y.floor() as i32;
        let z = centre.z.floor() as i32;
        let Some(local_x) = x.checked_sub(bounds.min_x).map(|value| value as usize) else {
            return false;
        };
        let Some(local_z) = z.checked_sub(bounds.min_z).map(|value| value as usize) else {
            return false;
        };
        if local_x >= SHELTER_SIDE || local_z >= SHELTER_SIDE {
            return false;
        }

        let column = &mut self.columns[local_z * SHELTER_SIDE + local_x];
        if *column == ColumnShelter::Unmeasured {
            #[cfg(test)]
            {
                self.scans += 1;
            }
            *column = store
                .highest_solid_y(x, z, bounds.min_y, bounds.max_y, bounds.chunk_size)
                .map_or(ColumnShelter::Open, ColumnShelter::Roof);
        }

        matches!(*column, ColumnShelter::Roof(roof_y) if roof_y >= y)
    }
}

impl PrecipitationVisuals {
    /// Which material draws `kind`.
    ///
    /// [`WeatherKind::Clear`] never reaches here — [`visible_quads`] answers zero for it and
    /// the volume is hidden before a material is chosen — and it answers snow rather than
    /// growing a fourth handle for a case that is not drawn.
    fn material(&self, kind: WeatherKind) -> &Handle<StandardMaterial> {
        match kind {
            WeatherKind::Rain => &self.rain,
            WeatherKind::Sandstorm => &self.sand,
            WeatherKind::Snow | WeatherKind::Blizzard | WeatherKind::Clear => &self.snow,
        }
    }
}

/// Builds the mesh and the materials, and stands the one volume entity up hidden.
///
/// Everything that does not change is written here exactly once: the indices, the normals
/// and the texture coordinates. Only the positions are rewritten per frame, which is what
/// makes the per-frame cost the 7 200 floats [`write_positions`] produces rather than a
/// mesh rebuild.
pub(super) fn create_visuals(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mesh = meshes.add(empty_volume_mesh());
    let visuals = PrecipitationVisuals {
        mesh: mesh.clone(),
        rain: materials.add(precipitation_material(RAIN_COLOUR)),
        snow: materials.add(precipitation_material(SNOW_COLOUR)),
        sand: materials.add(precipitation_material(SAND_COLOUR)),
    };

    commands.spawn((
        PrecipitationVolume,
        Mesh3d(mesh),
        MeshMaterial3d(visuals.snow.clone()),
        // The positions move every frame and the bounding box Bevy computes for an entity
        // is computed **once**, from whatever the mesh held when the entity was spawned --
        // which here is six hundred degenerate quads at the origin. Frustum culling against
        // that box would hide the weather the moment the player walked away from spawn.
        NoFrustumCulling,
        // The transform is the identity for the whole session: `write_positions` puts every
        // quad in world space itself, because the box follows the eye and a quad has to face
        // it, so a parent transform would only be a second thing to keep in step.
        Transform::default(),
        Visibility::Hidden,
    ));
    commands.insert_resource(visuals);
    commands.insert_resource(PrecipitationShelter::default());
}

/// One material per kind: unlit, blended, and drawn from both faces.
///
/// **Unlit** because a raindrop is not a surface with a normal — it has no shaded side, and
/// letting the sun decide how bright it is would make the weather vanish on the dark side of
/// a hill, which is exactly where a player most needs to see that it is raining.
///
/// **`cull_mode: None`** because a camera-facing quad is viewed from whichever face the
/// billboard maths happens to present, the same reason `mobs.rs` gives for the aggro marker.
fn precipitation_material(colour: Color) -> StandardMaterial {
    StandardMaterial {
        base_color: colour,
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        cull_mode: None,
        ..default()
    }
}

/// The mesh as it is created: every quad degenerate, every constant attribute final.
fn empty_volume_mesh() -> Mesh {
    let vertices = PRECIP_QUADS * 4;
    let mut indices = Vec::with_capacity(PRECIP_QUADS * 6);
    for quad in 0..PRECIP_QUADS {
        let base = (quad * 4) as u32;
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    Mesh::new(
        PrimitiveTopology::TriangleList,
        // Extraction must keep both copies: the renderer consumes one while
        // `draw_precipitation` rewrites the main-world vertex data every frame.
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0_f32; 3]; vertices])
    // Never read -- the material is unlit -- and present because a `StandardMaterial` mesh
    // in this crate carries the three attributes `mobs.rs` and `hands.rs` also write.
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 0.0, 1.0]; vertices])
    .with_inserted_attribute(
        Mesh::ATTRIBUTE_UV_0,
        (0..PRECIP_QUADS)
            .flat_map(|_| [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]])
            .collect::<Vec<[f32; 2]>>(),
    )
    .with_inserted_indices(Indices::U32(indices))
}

/// Rewrites the volume from the newest accepted weather and where the eye is now.
///
/// Registered after [`super::camera::AimCamera`] rather than inside the snapshot chain: the
/// box is centred on the eye and its quads face it, so reading the camera a frame early
/// would turn the whole plane edge-on whenever the player span. `player/sky.rs` takes the
/// opposite trade deliberately, and says why -- a sky colour one frame late is invisible.
#[derive(SystemParam)]
pub(super) struct PrecipitationInputs<'w> {
    weather: Res<'w, Weather>,
    session: Option<Res<'w, Session>>,
    store: Option<Res<'w, ChunkStore>>,
    time: Res<'w, Time>,
    visuals: Option<Res<'w, PrecipitationVisuals>>,
    shelter: Option<ResMut<'w, PrecipitationShelter>>,
}

pub(super) fn draw_precipitation(
    read: PrecipitationInputs<'_>,
    mut meshes: ResMut<Assets<Mesh>>,
    eyes: Query<&Transform, With<WorldCamera>>,
    mut volume: Query<
        (&mut Visibility, &mut MeshMaterial3d<StandardMaterial>),
        With<PrecipitationVolume>,
    >,
) {
    let PrecipitationInputs {
        weather,
        session,
        store,
        time,
        visuals,
        shelter,
    } = read;
    let (Some(visuals), Ok((mut visibility, mut material))) = (visuals, volume.single_mut()) else {
        return;
    };
    let Some(mut shelter) = shelter else {
        if *visibility != Visibility::Hidden {
            *visibility = Visibility::Hidden;
        }
        return;
    };
    // Observe changes even while the sky is clear. Bevy advances this system's change
    // tick after every run, so postponing the read until rain returned would miss edits
    // made during dry weather and revive old exposure answers.
    if store.as_ref().is_some_and(|store| store.is_changed()) {
        shelter.invalidate();
    }
    let drawn = drawn_weather(&weather, session.as_deref(), store.as_deref(), &eyes);
    let Some((kind, intensity, eye)) = drawn else {
        // Guarded, because `Mut` marks a component changed on every `DerefMut` and a clear
        // sky is the common case: an unconditional write would re-extract the volume into
        // the render world on every frame of a session it never draws in.
        if *visibility != Visibility::Hidden {
            *visibility = Visibility::Hidden;
        }
        return;
    };

    if *visibility != Visibility::Visible {
        *visibility = Visibility::Visible;
    }
    let wanted = visuals.material(kind);
    if material.0 != *wanted {
        material.0 = wanted.clone();
    }

    let Some(mut mesh) = meshes.get_mut(&visuals.mesh) else {
        return;
    };
    // Taken out, refilled and put back. `insert_attribute` is what marks the vertex data
    // for re-upload, and reusing the vector it replaces is what keeps a per-frame rewrite
    // free of allocation.
    let mut positions = match mesh.remove_attribute(Mesh::ATTRIBUTE_POSITION) {
        Some(VertexAttributeValues::Float32x3(existing)) => existing,
        _ => Vec::new(),
    };
    let column_shelter = match (kind, session.as_deref(), store.as_deref()) {
        (
            WeatherKind::Rain | WeatherKind::Snow | WeatherKind::Blizzard,
            Some(session),
            Some(store),
        ) => {
            shelter.prepare(
                eye.translation,
                usize::from(session.0.chunk_size),
                session.0.view_distance,
            );
            Some((store, &mut *shelter))
        }
        _ => None,
    };
    write_positions(
        &mut positions,
        kind,
        intensity,
        time.elapsed_secs(),
        eye.translation,
        eye.rotation,
        column_shelter,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
}

/// What to draw this frame, or `None` for a frame that draws nothing.
///
/// Four ways to answer nothing, and they are deliberately one answer rather than four
/// branches at the call site: no session, no camera, an eye under water, or a sky with no
/// quads in it (clear weather, an intensity that rounds to none, or a server that keeps no
/// weather at all).
fn drawn_weather<'a>(
    weather: &Weather,
    session: Option<&Session>,
    store: Option<&ChunkStore>,
    eyes: &'a Query<&Transform, With<WorldCamera>>,
) -> Option<(WeatherKind, u8, &'a Transform)> {
    let session = session?;
    let eye = eyes.iter().next()?;
    // Water overrides the sky above it, exactly as it overrides the fog: one answer, read
    // from `player/sky.rs` rather than re-derived here.
    if sky::submerged_at(store, eye.translation, usize::from(session.0.chunk_size)) {
        return None;
    }
    let state = weather.get()?;
    (visible_quads(state.kind, state.intensity) > 0).then_some((state.kind, state.intensity, eye))
}

/// How many of [`PRECIP_QUADS`] carry area at this intensity.
///
/// The contract's rule, applied as written: intensity is a fraction of 255 and the count is
/// that fraction of the budget. Nothing divides the range into named bands, so nothing here
/// switches on it -- half intensity is half the quads, and an intensity of 1 is two of them
/// rather than a threshold that has not been crossed.
///
/// **A blizzard is the one exception and it is the server's own.** It arrives only while a
/// `StormWarning` says `Raging`, and it draws the volume full whatever intensity it carries:
/// a blizzard that came down at a quarter strength would be a storm nobody could see the
/// point of sheltering from.
fn visible_quads(kind: WeatherKind, intensity: u8) -> usize {
    match kind {
        WeatherKind::Clear => 0,
        WeatherKind::Blizzard => PRECIP_QUADS,
        WeatherKind::Rain | WeatherKind::Snow | WeatherKind::Sandstorm => {
            PRECIP_QUADS * usize::from(intensity) / 255
        }
    }
}

/// Writes the whole volume: four world-space corners per quad, [`PRECIP_QUADS`] of them.
///
/// Pure arithmetic over an index and a clock, which is what makes the whole volume testable
/// without a window: seed in, corners out, no state carried between frames and nothing
/// allocated after the first call.
///
/// Quads past [`visible_quads`] are written **degenerate** -- all four corners on the same
/// point -- rather than removed. A triangle with no area is discarded by the rasteriser
/// before it costs a fragment, and keeping the count fixed is what lets the index buffer and
/// the vertex count be written once at startup.
fn write_positions(
    into: &mut Vec<[f32; 3]>,
    kind: WeatherKind,
    intensity: u8,
    elapsed: f32,
    eye: Vec3,
    look: Quat,
    mut shelter: Option<(&ChunkStore, &mut PrecipitationShelter)>,
) {
    into.clear();
    into.reserve(PRECIP_QUADS * 4);

    let visible = visible_quads(kind, intensity);
    let corner = VOLUME * 0.5;
    let (right, up) = billboard_basis(kind, look);
    let size = quad_size(kind);
    let half_right = right * (size.x * 0.5);
    let half_up = up * (size.y * 0.5);

    for quad in 0..PRECIP_QUADS {
        if quad >= visible {
            // The centre of the box: any point does, and this one is inside the volume the
            // visible quads occupy, so a degenerate quad can never stretch the mesh's
            // extents past what the rest of it already covers.
            let hidden = [eye.x, eye.y, eye.z];
            into.extend_from_slice(&[hidden; 4]);
            continue;
        }

        let seed = Vec3::new(
            unit_from(quad as u32 * 3),
            unit_from(quad as u32 * 3 + 1),
            unit_from(quad as u32 * 3 + 2),
        );
        let local = (seed * VOLUME + travelled(kind, intensity, seed, elapsed)).rem_euclid(VOLUME);
        let centre = eye + local - corner;

        let falls_from_sky = matches!(
            kind,
            WeatherKind::Rain | WeatherKind::Snow | WeatherKind::Blizzard
        );
        if falls_from_sky
            && shelter
                .as_mut()
                .is_some_and(|(store, cache)| cache.is_sheltered(store, centre))
        {
            let hidden = centre.to_array();
            into.extend_from_slice(&[hidden; 4]);
            continue;
        }

        into.extend_from_slice(&[
            (centre - half_right - half_up).to_array(),
            (centre + half_right - half_up).to_array(),
            (centre + half_right + half_up).to_array(),
            (centre - half_right + half_up).to_array(),
        ]);
    }
}

/// How far one quad has moved from where its seed put it, after `elapsed` seconds.
///
/// Unbounded on purpose: [`write_positions`] takes the result modulo [`VOLUME`], so a quad
/// that has fallen a mile is a quad that has wrapped through the box a hundred times, and
/// nothing has to remember that it did.
fn travelled(kind: WeatherKind, intensity: u8, seed: Vec3, elapsed: f32) -> Vec3 {
    let driving = driving_strength(intensity);
    match kind {
        // Never reached -- `visible_quads` answers zero and the volume is hidden -- and
        // answered rather than left to a wildcard, so a sixth kind fails to compile here
        // instead of falling through into somebody else's motion.
        WeatherKind::Clear => Vec3::ZERO,
        WeatherKind::Rain => {
            wind_travel(driving, elapsed)
                + Vec3::new(0.0, -scaled_speed(RAIN_FALL_SPEED, driving) * elapsed, 0.0)
        }
        WeatherKind::Snow => {
            // The phase comes out of the seed the starting position did, so a flake's
            // wander is as much its own as where it started -- and it costs no state.
            let phase = TAU * (elapsed * SNOW_DRIFT_HZ + seed.x);
            wind_travel(driving, elapsed)
                + Vec3::new(
                    SNOW_DRIFT_BLOCKS * phase.sin(),
                    -scaled_speed(SNOW_FALL_SPEED, driving) * elapsed,
                    SNOW_DRIFT_BLOCKS * (phase + seed.z * TAU).cos(),
                )
        }
        WeatherKind::Blizzard => {
            let phase = TAU * (elapsed * SNOW_DRIFT_HZ + seed.x);
            Vec3::new(
                SNOW_DRIFT_BLOCKS * phase.sin(),
                -SNOW_FALL_SPEED[0] * BLIZZARD_SPEED_FACTOR * elapsed,
                SNOW_DRIFT_BLOCKS * (phase + seed.z * TAU).cos(),
            )
        }
        // Horizontal, and only horizontal: sand does not fall, it is carried past you.
        WeatherKind::Sandstorm => {
            Vec3::new(scaled_speed(SAND_DRIFT_SPEED, driving) * elapsed, 0.0, 0.0)
                + wind_travel(driving, elapsed)
        }
    }
}

/// The share of the calm-to-driving presentation range this intensity occupies.
/// Low weather remains straight and slow; the remainder is continuous rather than
/// divided into bands, matching the contract's description of intensity.
fn driving_strength(intensity: u8) -> f32 {
    f32::from(intensity.saturating_sub(CALM_INTENSITY)) / f32::from(u8::MAX - CALM_INTENSITY)
}

fn scaled_speed(range: [f32; 2], driving: f32) -> f32 {
    range[0] + (range[1] - range[0]) * driving
}

fn wind_travel(driving: f32, elapsed: f32) -> Vec3 {
    WIND_DIRECTION * (WIND_SPEED * driving * elapsed)
}

/// How wide and how tall one quad is, in blocks.
fn quad_size(kind: WeatherKind) -> Vec2 {
    match kind {
        WeatherKind::Rain => RAIN_SIZE,
        WeatherKind::Sandstorm => SAND_SIZE,
        // Clear draws nothing; it answers the flake's size rather than a zero that would
        // read as a size somebody chose.
        WeatherKind::Snow | WeatherKind::Blizzard | WeatherKind::Clear => SNOW_SIZE,
    }
}

/// The two axes one quad is built on, in world space.
///
/// **Rain is a cylindrical billboard and the other kinds are spherical**, which is one
/// branch and worth it. A streak is long in one direction, and that direction is down: built
/// on the camera's own up axis it would lean over as the player looked up, and a wall of
/// leaning rain reads as a bug rather than as wind. So rain stands on world up and turns only
/// about it. A flake and a mote are square, so there is nothing for a spherical billboard to
/// lean.
///
/// The degenerate case is the player looking straight up or straight down, where the
/// camera's right axis has no horizontal part to normalise. `Dir3::new` refuses that and the
/// fallback is world +X: at that pitch every streak is seen end-on and which way it turns is
/// not something a player can tell.
fn billboard_basis(kind: WeatherKind, look: Quat) -> (Vec3, Vec3) {
    let right = look * Vec3::X;
    if kind == WeatherKind::Rain {
        let flat = Vec3::new(right.x, 0.0, right.z);
        let horizontal = flat.try_normalize().unwrap_or(Vec3::X);
        (horizontal, Vec3::Y)
    } else {
        (right, look * Vec3::Y)
    }
}

/// One deterministic value in `[0, 1)` per index.
///
/// An integer avalanche, not a random number generator: there is no seed to carry, no state
/// to keep in step between frames, and a quad asked for its seed a thousand frames apart
/// gets the same answer both times -- which is the whole reason this module needs no
/// per-particle storage. The multipliers are Pelle Evensen's `lowbias32`, chosen because it
/// scatters consecutive inputs, and consecutive inputs are all this ever gets.
fn unit_from(index: u32) -> f32 {
    let mut hash = index;
    hash ^= hash >> 16;
    hash = hash.wrapping_mul(0x7feb_352d);
    hash ^= hash >> 15;
    hash = hash.wrapping_mul(0x846c_a68b);
    hash ^= hash >> 16;
    // `2^32` as an f32 divisor rather than `u32::MAX`, so the result is a half-open [0, 1)
    // and a quad can never be seeded exactly on the far face of the box.
    hash as f32 / 4_294_967_296.0
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use bevy::asset::AssetPlugin;
    use bevy::time::TimeUpdateStrategy;

    use super::*;
    use crate::net::{
        BlockCoord, ChunkCoord, EntityState, SessionParams, Snapshot, SnapshotInbox, WeatherState,
    };
    use crate::player::PlayerPlugin;
    use crate::world::{VoxelChunk, palette};

    const LOCAL_ID: u64 = 7;
    const SIZE: u16 = 32;
    const SPAWN: [f32; 3] = [0.5, 64.0, 0.5];
    const INTERVAL: Duration = Duration::from_millis(50);

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: LOCAL_ID,
            spawn: SPAWN,
            world_seed: 1,
            tick_rate: 20,
            chunk_size: SIZE,
            view_distance: 8,
            inventory_slots: 37,
            hotbar_slots: 9,
            equipment_slots: 4,
            player_token: crate::net::ANY_TOKEN,
            voice_range_blocks: 0.0,
        })
    }

    /// A store holding one chunk of air around the spawn, with `flooded` deciding whether
    /// the voxel the eye sits in is water.
    fn store(flooded: bool) -> ChunkStore {
        let mut chunk = VoxelChunk::all_air(usize::from(SIZE));
        if flooded {
            for y in 0..usize::from(SIZE) {
                chunk.set(0, y, 0, palette::WATER_FLOW7);
            }
        }
        let mut store = ChunkStore::default();
        store.insert(
            ChunkCoord {
                cx: 0,
                cy: 2,
                cz: 0,
            },
            chunk,
        );
        store
    }

    fn headless_player(flooded: bool) -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .insert_resource(session())
            .insert_resource(store(flooded))
            .insert_resource(TimeUpdateStrategy::ManualDuration(INTERVAL))
            .add_plugins(PlayerPlugin);
        app
    }

    /// Queues a snapshot as the net thread would, always naming the local player so the
    /// camera has a body to follow and therefore an eye to centre the box on.
    fn deliver(app: &mut App, tick: u32, weather: Option<WeatherState>) {
        app.world_mut().resource_mut::<SnapshotInbox>().push(
            Snapshot {
                server_tick: tick,
                entities: vec![EntityState {
                    entity_id: LOCAL_ID,
                    pos: SPAWN,
                    vel: [0.0; 3],
                    yaw: 0.0,
                }],
                weather,
                ..Default::default()
            },
            Instant::now(),
        );
    }

    fn weather_of(kind: WeatherKind, intensity: u8) -> Option<WeatherState> {
        Some(WeatherState { kind, intensity })
    }

    /// What the one volume entity currently is: whether it is drawn, and which material.
    fn volume(app: &mut App) -> (Visibility, Handle<StandardMaterial>) {
        let world = app.world_mut();
        let mut query = world
            .query_filtered::<(&Visibility, &MeshMaterial3d<StandardMaterial>), With<PrecipitationVolume>>();
        let (visibility, material) = query.single(world).expect("one precipitation volume");
        (*visibility, material.0.clone())
    }

    /// The eye the box is centred on, as the camera left it this frame.
    fn eye(app: &mut App) -> Vec3 {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&Transform, With<WorldCamera>>();
        query.single(world).expect("one camera").translation
    }

    fn positions(app: &mut App) -> Vec<[f32; 3]> {
        let world = app.world_mut();
        let handle = world.resource::<PrecipitationVisuals>().mesh.clone();
        let meshes = world.resource::<Assets<Mesh>>();
        let mesh = meshes.get(&handle).expect("the volume's mesh");
        match mesh.attribute(Mesh::ATTRIBUTE_POSITION) {
            Some(VertexAttributeValues::Float32x3(values)) => values.clone(),
            other => panic!("the volume's positions are three floats each, not {other:?}"),
        }
    }

    /// Fills a buffer the way the system does, for the arithmetic tests that need no app.
    fn drawn(kind: WeatherKind, intensity: u8, elapsed: f32) -> Vec<[f32; 3]> {
        let mut into = Vec::new();
        write_positions(
            &mut into,
            kind,
            intensity,
            elapsed,
            Vec3::ZERO,
            Quat::IDENTITY,
            None,
        );
        into
    }

    fn roof_store(block: crate::world::BlockId, covered_x: std::ops::Range<usize>) -> ChunkStore {
        let mut chunk = VoxelChunk::all_air(usize::from(SIZE));
        for x in covered_x {
            for z in 0..usize::from(SIZE) {
                chunk.set(x, 16, z, block);
            }
        }
        let mut store = ChunkStore::default();
        store.insert(
            ChunkCoord {
                cx: 0,
                cy: 0,
                cz: 0,
            },
            chunk,
        );
        store
    }

    fn sheltered_positions(
        kind: WeatherKind,
        eye: Vec3,
        store: &ChunkStore,
        cache: &mut PrecipitationShelter,
    ) -> Vec<[f32; 3]> {
        cache.prepare(eye, usize::from(SIZE), 0);
        let mut into = Vec::new();
        write_positions(
            &mut into,
            kind,
            255,
            0.0,
            eye,
            Quat::IDENTITY,
            Some((store, cache)),
        );
        into
    }

    fn positions_at(kind: WeatherKind, eye: Vec3) -> Vec<[f32; 3]> {
        let mut into = Vec::new();
        write_positions(&mut into, kind, 255, 0.0, eye, Quat::IDENTITY, None);
        into
    }

    fn quad_area(positions: &[[f32; 3]], quad: usize) -> f32 {
        let corners = &positions[quad * 4..quad * 4 + 4];
        (Vec3::from(corners[1]) - Vec3::from(corners[0]))
            .cross(Vec3::from(corners[3]) - Vec3::from(corners[0]))
            .length()
    }

    fn quad_centre(positions: &[[f32; 3]], quad: usize) -> Vec3 {
        positions[quad * 4..quad * 4 + 4]
            .iter()
            .map(|corner| Vec3::from(*corner))
            .sum::<Vec3>()
            * 0.25
    }

    /// The renderer extracts one copy, but the next frame still needs the CPU copy to
    /// rewrite. Headless tests do not run extraction, so pin the usage contract itself.
    #[test]
    fn the_volume_mesh_remains_mutable_after_render_extraction() {
        let usage = empty_volume_mesh().asset_usage;
        assert!(usage.contains(RenderAssetUsages::MAIN_WORLD));
        assert!(usage.contains(RenderAssetUsages::RENDER_WORLD));
    }

    /// The resource is the newest **accepted** snapshot's weather and nothing else: an
    /// older tick describes a sky that has already been drawn.
    #[test]
    fn the_resource_mirrors_the_newest_accepted_snapshot() {
        let mut app = headless_player(false);
        assert_eq!(
            app.world().resource::<Weather>().get(),
            None,
            "before a snapshot the client has been told nothing"
        );

        deliver(&mut app, 10, weather_of(WeatherKind::Rain, 128));
        app.update();
        assert_eq!(
            app.world().resource::<Weather>().get(),
            weather_of(WeatherKind::Rain, 128)
        );

        // Older, and therefore refused by the buffer -- so it cannot move the sky either.
        deliver(&mut app, 9, weather_of(WeatherKind::Sandstorm, 255));
        app.update();
        assert_eq!(
            app.world().resource::<Weather>().get(),
            weather_of(WeatherKind::Rain, 128),
            "a snapshot the buffer refused set the weather"
        );

        deliver(&mut app, 11, weather_of(WeatherKind::Clear, 0));
        app.update();
        assert_eq!(
            app.world().resource::<Weather>().get(),
            weather_of(WeatherKind::Clear, 0)
        );

        // A server that keeps no weather at all is `None`, not a defect -- `net/codec.rs`
        // owns that distinction and refuses the shapes that are defects.
        deliver(&mut app, 12, None);
        app.update();
        assert_eq!(app.world().resource::<Weather>().get(), None);
    }

    /// A sky a session ended under is not the next session's sky.
    #[test]
    fn a_session_that_ends_takes_its_weather_with_it() {
        let mut app = headless_player(false);
        deliver(&mut app, 1, weather_of(WeatherKind::Snow, 200));
        app.update();
        assert!(app.world().resource::<Weather>().get().is_some());

        app.world_mut().remove_resource::<Session>();
        app.update();
        assert_eq!(app.world().resource::<Weather>().get(), None);
    }

    /// The contract's rule, applied as written: a fraction of 255 is that fraction of the
    /// budget, with no bands and no threshold.
    #[test]
    fn the_visible_count_is_the_intensity_fraction_of_the_budget() {
        assert_eq!(visible_quads(WeatherKind::Clear, 0), 0);
        assert_eq!(visible_quads(WeatherKind::Rain, 0), 0);
        // One 255th of six hundred is two, not "not yet raining".
        assert_eq!(visible_quads(WeatherKind::Rain, 1), 2);
        // 128/255 is a hair over half, so the honest answer is 301 rather than the round
        // 300 the issue's test strategy names. The formula is what the contract states.
        assert_eq!(visible_quads(WeatherKind::Rain, 128), 301);
        assert_eq!(visible_quads(WeatherKind::Snow, 255), PRECIP_QUADS);
        assert_eq!(visible_quads(WeatherKind::Sandstorm, 255), PRECIP_QUADS);
        // Monotonic all the way up, and never past the budget.
        let mut previous = 0;
        for intensity in 0..=u8::MAX {
            let count = visible_quads(WeatherKind::Snow, intensity);
            assert!(count >= previous && count <= PRECIP_QUADS, "{intensity}");
            previous = count;
        }
    }

    /// A blizzard is a hazard rather than weather, so it draws the volume full whatever
    /// intensity it happens to carry.
    #[test]
    fn a_blizzard_draws_the_whole_budget_whatever_intensity_says() {
        for intensity in [0, 1, 128, 255] {
            assert_eq!(
                visible_quads(WeatherKind::Blizzard, intensity),
                PRECIP_QUADS,
                "a blizzard at {intensity} drew less than the whole volume"
            );
        }
    }

    /// Every corner of every quad, drawn or degenerate, stays inside the box around the eye
    /// -- at any elapsed time, which is what wrapping is for.
    #[test]
    fn every_quad_stays_inside_the_box_around_the_eye() {
        let bound = VOLUME * 0.5 + Vec3::splat(RAIN_SIZE.y);
        for kind in [
            WeatherKind::Rain,
            WeatherKind::Snow,
            WeatherKind::Sandstorm,
            WeatherKind::Blizzard,
        ] {
            for elapsed in [0.0, 0.5, 17.0, 600.0, 86_400.0] {
                for corner in drawn(kind, 255, elapsed) {
                    let offset = Vec3::from(corner).abs();
                    assert!(
                        offset.cmple(bound).all(),
                        "{kind:?} at {elapsed}s put a corner at {corner:?}, outside {bound:?}"
                    );
                }
            }
        }
    }

    /// The budget is fixed and intensity chooses how many of it carry area, so the mesh
    /// never changes size and the quads past the count draw nothing.
    #[test]
    fn a_quad_past_the_visible_count_has_no_area() {
        let corners = drawn(WeatherKind::Rain, 128, 3.0);
        assert_eq!(corners.len(), PRECIP_QUADS * 4);

        let visible = visible_quads(WeatherKind::Rain, 128);
        for quad in 0..PRECIP_QUADS {
            let corners: Vec<Vec3> = corners[quad * 4..quad * 4 + 4]
                .iter()
                .map(|corner| Vec3::from(*corner))
                .collect();
            let area = (corners[1] - corners[0])
                .cross(corners[3] - corners[0])
                .length();
            if quad < visible {
                assert!(area > 0.0, "visible quad {quad} has no area");
            } else {
                assert_eq!(area, 0.0, "quad {quad} is past the count and still drawn");
            }
        }
    }

    /// Rain falls, snow falls slowly, a blizzard falls at twice the flake speed, and sand
    /// does not fall at all -- it is carried past you.
    #[test]
    fn each_kind_moves_the_way_the_contract_describes_it() {
        let seed = Vec3::new(0.25, 0.5, 0.75);
        let second = 1.0;

        assert_eq!(
            travelled(WeatherKind::Rain, CALM_INTENSITY, seed, second),
            Vec3::new(0.0, -RAIN_FALL_SPEED[0], 0.0)
        );

        let snow = travelled(WeatherKind::Snow, CALM_INTENSITY, seed, second);
        let blizzard = travelled(WeatherKind::Blizzard, 255, seed, second);
        assert_eq!(snow.y, -SNOW_FALL_SPEED[0]);
        assert_eq!(blizzard.y, snow.y * BLIZZARD_SPEED_FACTOR);
        // The lateral wander is what separates snow from slow rain, and it is bounded.
        assert!(snow.x != 0.0 || snow.z != 0.0);
        assert!(snow.x.abs() <= SNOW_DRIFT_BLOCKS && snow.z.abs() <= SNOW_DRIFT_BLOCKS);

        let sand = travelled(WeatherKind::Sandstorm, CALM_INTENSITY, seed, second);
        assert_eq!(sand, Vec3::new(SAND_DRIFT_SPEED[0], 0.0, 0.0));
    }

    /// Ordinary weather moves faster at the top of the server-owned intensity
    /// scale than in calm weather. The blizzard keeps its separate fixed rule.
    #[test]
    fn ordinary_precipitation_speed_scales_with_intensity() {
        let seed = Vec3::new(0.25, 0.5, 0.75);
        for kind in [WeatherKind::Rain, WeatherKind::Snow, WeatherKind::Sandstorm] {
            let calm = travelled(kind, 64, seed, 1.0).length();
            let driving = travelled(kind, 255, seed, 1.0).length();
            assert!(driving > calm, "{kind:?}: {driving} <= {calm}");
        }
    }

    /// Wind is a stable world-space displacement: low intensity has none, high
    /// intensity has some, and no camera rotation is an input to the function.
    #[test]
    fn wind_lean_is_world_space_and_scales_with_intensity() {
        assert_eq!(wind_travel(driving_strength(64), 1.0), Vec3::ZERO);
        let driving = wind_travel(driving_strength(255), 1.0);
        assert_eq!(driving, WIND_DIRECTION * WIND_SPEED);
        assert_eq!(driving.y, 0.0);
        assert!(driving.x != 0.0 && driving.z != 0.0);
    }

    /// A rain streak is long in one direction and that direction is down, whatever the
    /// camera is doing -- a wall of leaning rain reads as a bug rather than as wind.
    #[test]
    fn a_rain_streak_stands_upright_whatever_the_camera_does() {
        for pitch in [0.0, 0.6, -0.6, std::f32::consts::FRAC_PI_2] {
            for yaw in [0.0, 1.0, 3.0] {
                let look = Quat::from_euler(EulerRot::YXZ, yaw, pitch, 0.0);
                let (right, up) = billboard_basis(WeatherKind::Rain, look);
                assert_eq!(up, Vec3::Y, "a streak leaned at pitch {pitch}");
                assert!(
                    right.y.abs() < 1e-6,
                    "a streak's width left the horizontal at pitch {pitch}: {right:?}"
                );
                assert!((right.length() - 1.0).abs() < 1e-5);
            }
        }
        // A flake is square, so there is nothing for a spherical billboard to lean.
        let look = Quat::from_euler(EulerRot::YXZ, 1.0, 0.4, 0.0);
        assert_eq!(
            billboard_basis(WeatherKind::Snow, look),
            (look * Vec3::X, look * Vec3::Y)
        );
    }

    /// The seeds are deterministic, in `[0, 1)`, and scattered rather than clustered --
    /// which is the whole reason a quad needs no state kept for it between frames.
    #[test]
    fn the_seeds_are_deterministic_and_spread_across_the_unit_interval() {
        assert_eq!(unit_from(0), unit_from(0));
        assert_ne!(unit_from(0), unit_from(1));

        let mut buckets = [0_usize; 10];
        for index in 0..(PRECIP_QUADS as u32 * 3) {
            let value = unit_from(index);
            assert!((0.0..1.0).contains(&value), "{index} seeded {value}");
            buckets[(value * 10.0) as usize] += 1;
        }
        // 1 800 seeds over ten buckets is 180 each; a hash that clustered would empty one.
        for (bucket, count) in buckets.iter().enumerate() {
            assert!(*count > 100, "bucket {bucket} holds only {count} of 1800");
        }
    }

    /// Rain is drawn, and the mesh is rewritten in place: the same entity and the same
    /// mesh handle, with different vertices in it.
    #[test]
    fn the_volume_is_rewritten_in_place_rather_than_respawned() {
        let mut app = headless_player(false);
        deliver(&mut app, 1, weather_of(WeatherKind::Rain, 255));
        app.update();

        let (visibility, material) = volume(&mut app);
        assert_eq!(visibility, Visibility::Visible);
        let before = positions(&mut app);
        assert_eq!(before.len(), PRECIP_QUADS * 4);

        let entity = {
            let world = app.world_mut();
            let mut query = world.query_filtered::<Entity, With<PrecipitationVolume>>();
            query.single(world).expect("one volume")
        };

        app.update();
        let after = positions(&mut app);
        assert_ne!(before, after, "a frame passed and nothing fell");
        assert_eq!(after.len(), before.len(), "the vertex count moved");

        let (_, still) = volume(&mut app);
        assert_eq!(still, material, "the material was replaced mid-fall");
        let world = app.world_mut();
        let mut query = world.query_filtered::<Entity, With<PrecipitationVolume>>();
        assert_eq!(
            query.single(world).expect("one volume"),
            entity,
            "the volume was respawned instead of rewritten"
        );
    }

    /// The box travels with the eye, which is what stops a sprinting player outrunning the
    /// rain.
    #[test]
    fn the_box_is_centred_on_the_eye() {
        let mut app = headless_player(false);
        deliver(&mut app, 1, weather_of(WeatherKind::Snow, 255));
        app.update();

        let centre = eye(&mut app);
        let bound = VOLUME * 0.5 + Vec3::splat(SNOW_SIZE.y);
        for corner in positions(&mut app) {
            let offset = (Vec3::from(corner) - centre).abs();
            assert!(
                offset.cmple(bound).all(),
                "a corner at {corner:?} is outside the box around {centre:?}"
            );
        }
    }

    /// Each kind is drawn with its own material, and a clear sky is drawn with none of
    /// them because it is not drawn at all.
    #[test]
    fn the_material_follows_the_kind_and_a_clear_sky_hides_the_volume() {
        let mut app = headless_player(false);
        // One frame first: `create_visuals` runs in `Startup`, so the handles do not exist
        // until the schedule has been through once.
        app.update();
        let handles = {
            let visuals = app.world().resource::<PrecipitationVisuals>();
            [
                (WeatherKind::Rain, visuals.rain.clone()),
                (WeatherKind::Snow, visuals.snow.clone()),
                (WeatherKind::Sandstorm, visuals.sand.clone()),
                // A blizzard is snow, and a second snow material would be a second place
                // to change what snow looks like.
                (WeatherKind::Blizzard, visuals.snow.clone()),
            ]
        };

        let mut tick = 0;
        for (kind, expected) in handles {
            tick += 1;
            deliver(&mut app, tick, weather_of(kind, 255));
            app.update();
            let (visibility, material) = volume(&mut app);
            assert_eq!(visibility, Visibility::Visible, "{kind:?} was not drawn");
            assert_eq!(material, expected, "{kind:?} drew with the wrong material");
        }

        tick += 1;
        deliver(&mut app, tick, weather_of(WeatherKind::Clear, 0));
        app.update();
        assert_eq!(volume(&mut app).0, Visibility::Hidden);

        // And an intensity that rounds to no quads at all is the same nothing.
        tick += 1;
        deliver(&mut app, tick, weather_of(WeatherKind::Rain, 0));
        app.update();
        assert_eq!(volume(&mut app).0, Visibility::Hidden);
    }

    /// Startup owns the shelter resource, but the draw system is total while Bevy is
    /// applying that deferred command or if another presentation system removes it.
    #[test]
    fn a_missing_shelter_resource_hides_the_volume_instead_of_failing_the_system() {
        let mut app = headless_player(false);
        deliver(&mut app, 1, weather_of(WeatherKind::Rain, 255));
        app.update();
        assert_eq!(volume(&mut app).0, Visibility::Visible);

        app.world_mut().remove_resource::<PrecipitationShelter>();
        app.update();

        assert_eq!(volume(&mut app).0, Visibility::Hidden);
    }

    /// Water wins. An eye under the surface sees the underwater sky and no precipitation,
    /// whatever the sky above it is doing -- the same override `player/sky.rs` applies to
    /// the fog, read through the same answer.
    #[test]
    fn water_overrides_the_weather_above_it() {
        let mut app = headless_player(true);
        deliver(&mut app, 1, weather_of(WeatherKind::Blizzard, 255));
        // Twice: `follow_the_player` places the camera from the body the first update
        // spawns, so the eye the volume reads is only under water from the second frame.
        app.update();
        app.update();
        assert_eq!(
            volume(&mut app).0,
            Visibility::Hidden,
            "a submerged player was rained on"
        );

        // The same storm above the surface is drawn.
        let mut dry = headless_player(false);
        deliver(&mut dry, 1, weather_of(WeatherKind::Blizzard, 255));
        dry.update();
        dry.update();
        assert_eq!(volume(&mut dry).0, Visibility::Visible);
    }

    /// Shelter is a per-quad physical question: a roof removes only falling weather
    /// below it, while the same column remains snowy or rainy above the roof line.
    #[test]
    fn a_solid_roof_stops_every_falling_kind_below_it_and_not_above_it() {
        let eye = Vec3::new(15.5, 16.0, 15.5);
        let store = roof_store(palette::STONE, 0..usize::from(SIZE));

        for kind in [WeatherKind::Rain, WeatherKind::Snow, WeatherKind::Blizzard] {
            let open = positions_at(kind, eye);
            let mut cache = PrecipitationShelter::default();
            let sheltered = sheltered_positions(kind, eye, &store, &mut cache);
            let mut below = 0;
            let mut above = 0;

            for quad in 0..PRECIP_QUADS {
                assert!(
                    quad_area(&open, quad) > 0.0,
                    "{kind:?} quad {quad} began hidden"
                );
                if quad_centre(&open, quad).y.floor() as i32 <= 16 {
                    below += 1;
                    assert_eq!(
                        quad_area(&sheltered, quad),
                        0.0,
                        "{kind:?} quad {quad} passed through the roof"
                    );
                } else {
                    above += 1;
                    assert!(
                        quad_area(&sheltered, quad) > 0.0,
                        "{kind:?} quad {quad} above the roof was hidden"
                    );
                }
            }
            assert!(
                below > 0 && above > 0,
                "{kind:?} did not exercise both sides"
            );
        }
    }

    /// A covered eye is not a covered volume. The open half beside it remains visible,
    /// which is what lets a player inside see rain through a doorway or courtyard.
    #[test]
    fn an_open_neighbouring_column_stays_rainy_when_the_camera_is_covered() {
        let eye = Vec3::new(15.5, 16.0, 15.5);
        let store = roof_store(palette::STONE, 0..16);
        let open = positions_at(WeatherKind::Rain, eye);
        let mut cache = PrecipitationShelter::default();
        let sheltered = sheltered_positions(WeatherKind::Rain, eye, &store, &mut cache);
        let mut covered = 0;
        let mut uncovered = 0;

        for quad in 0..PRECIP_QUADS {
            let centre = quad_centre(&open, quad);
            if centre.y.floor() as i32 > 16 {
                continue;
            }
            if (centre.x.floor() as i32) < 16 {
                covered += 1;
                assert_eq!(quad_area(&sheltered, quad), 0.0);
            } else {
                uncovered += 1;
                assert!(quad_area(&sheltered, quad) > 0.0);
            }
        }
        assert!(covered > 0 && uncovered > 0);
    }

    /// The shelter predicate is solidity, not visibility or merely occupying a voxel.
    /// Water, flowers and — since #874 — a bush stop no body and no weather; a solid
    /// pane does both even if a future translucent pass changes whether terrain can be
    /// seen through it.
    #[test]
    fn only_solid_cover_is_a_roof() {
        let eye = Vec3::new(15.5, 16.0, 15.5);
        let below = Vec3::new(15.5, 15.5, 15.5);
        for block in [
            palette::AIR,
            palette::WATER,
            palette::FLOWER_RED,
            palette::BUSH,
        ] {
            let store = roof_store(block, 0..usize::from(SIZE));
            let mut cache = PrecipitationShelter::default();
            cache.prepare(eye, usize::from(SIZE), 0);
            assert!(
                !cache.is_sheltered(&store, below),
                "non-solid block {block} stopped precipitation"
            );
        }
        for block in [palette::STONE, palette::DARK_GLASS] {
            let store = roof_store(block, 0..usize::from(SIZE));
            let mut cache = PrecipitationShelter::default();
            cache.prepare(eye, usize::from(SIZE), 0);
            assert!(
                cache.is_sheltered(&store, below),
                "solid block {block} did not stop precipitation"
            );
        }
    }

    /// Store mutations invalidate both cached open sky and cached roofs. This is the
    /// path used for block edits, chunk replacement/arrival and unload alike.
    #[test]
    fn roof_edits_do_not_leave_stale_column_exposure() {
        let eye = Vec3::new(15.5, 16.0, 15.5);
        let below = Vec3::new(15.5, 15.5, 15.5);
        let mut store = roof_store(palette::AIR, 0..usize::from(SIZE));
        let mut cache = PrecipitationShelter::default();
        cache.prepare(eye, usize::from(SIZE), 0);
        assert!(!cache.is_sheltered(&store, below));

        let roof = BlockCoord {
            x: 15,
            y: 16,
            z: 15,
        };
        store.apply_block(roof, palette::STONE, usize::from(SIZE));
        cache.invalidate();
        cache.prepare(eye, usize::from(SIZE), 0);
        assert!(cache.is_sheltered(&store, below));

        store.apply_block(roof, palette::AIR, usize::from(SIZE));
        cache.invalidate();
        cache.prepare(eye, usize::from(SIZE), 0);
        assert!(!cache.is_sheltered(&store, below));
    }

    /// The ECS path observes `ChunkStore` change ticks itself; callers do not have to
    /// remember to reach into the presentation cache when an authoritative edit lands.
    #[test]
    fn the_draw_system_invalidates_cached_exposure_after_an_edit() {
        let mut app = headless_player(false);
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::ZERO));
        deliver(&mut app, 1, weather_of(WeatherKind::Rain, 255));
        app.update();
        app.update();

        let before = positions(&mut app);
        let quad = (0..PRECIP_QUADS)
            .find(|quad| {
                let centre = quad_centre(&before, *quad);
                let block = centre.floor().as_ivec3();
                (0..i32::from(SIZE)).contains(&block.x)
                    && (64..95).contains(&block.y)
                    && (0..i32::from(SIZE)).contains(&block.z)
            })
            .expect("the volume overlaps the held chunk");
        let centre = quad_centre(&before, quad).floor().as_ivec3();
        let roof = BlockCoord {
            x: centre.x,
            y: centre.y + 1,
            z: centre.z,
        };
        assert!(quad_area(&before, quad) > 0.0);

        app.world_mut().resource_mut::<ChunkStore>().apply_block(
            roof,
            palette::STONE,
            usize::from(SIZE),
        );
        app.update();
        assert_eq!(
            quad_area(&positions(&mut app), quad),
            0.0,
            "the cached open column survived a roof edit"
        );

        app.world_mut().resource_mut::<ChunkStore>().apply_block(
            roof,
            palette::AIR,
            usize::from(SIZE),
        );
        app.update();
        assert!(
            quad_area(&positions(&mut app), quad) > 0.0,
            "the cached roof survived its removal"
        );
    }

    /// Six hundred quads share no more than the fixed 25x25 column budget, and a second
    /// rewrite over the same footprint walks no voxels at all.
    #[test]
    fn shelter_work_is_bounded_and_shared_across_frames() {
        let eye = Vec3::new(15.5, 16.0, 15.5);
        let store = roof_store(palette::AIR, 0..usize::from(SIZE));
        let mut cache = PrecipitationShelter::default();
        let first = sheltered_positions(WeatherKind::Blizzard, eye, &store, &mut cache);
        assert_eq!(first.len(), PRECIP_QUADS * 4);
        assert!(cache.scans > 0 && cache.scans <= SHELTER_COLUMNS);

        let scans = cache.scans;
        let second = sheltered_positions(WeatherKind::Blizzard, eye, &store, &mut cache);
        assert_eq!(second, first);
        assert_eq!(cache.scans, scans, "the same columns were walked twice");

        let sand = sheltered_positions(
            WeatherKind::Sandstorm,
            eye,
            &roof_store(palette::STONE, 0..usize::from(SIZE)),
            &mut cache,
        );
        assert_eq!(sand, positions_at(WeatherKind::Sandstorm, eye));
    }

    /// What one frame of the volume costs, measured rather than asserted.
    ///
    /// `#[ignore]`, so CI never runs it: a wall-clock assertion on a shared runner is a
    /// flake, and this exists to be run by hand when the geometry changes.
    ///
    /// ```text
    /// cargo test --release -p voxelheim-client -- --ignored --nocapture the_rewrite_costs
    /// ```
    #[test]
    #[ignore = "a timing measurement, not an assertion; run it by hand in release"]
    fn the_rewrite_costs() {
        const FRAMES: u32 = 10_000;
        let mut into = Vec::new();
        let store = roof_store(palette::AIR, 0..usize::from(SIZE));
        let mut shelter = PrecipitationShelter::default();
        shelter.prepare(Vec3::ZERO, usize::from(SIZE), 0);
        // One call outside the loop, so the measurement is of the steady state rather than
        // of the one allocation the buffer ever makes or the first column walks.
        write_positions(
            &mut into,
            WeatherKind::Rain,
            255,
            0.0,
            Vec3::ZERO,
            Quat::IDENTITY,
            Some((&store, &mut shelter)),
        );

        let started = Instant::now();
        for frame in 0..FRAMES {
            write_positions(
                &mut into,
                WeatherKind::Rain,
                255,
                frame as f32 / 60.0,
                Vec3::ZERO,
                Quat::IDENTITY,
                Some((&store, &mut shelter)),
            );
            std::hint::black_box(&into);
        }
        let each = started.elapsed().as_secs_f64() * 1_000.0 / f64::from(FRAMES);
        println!("{PRECIP_QUADS} quads rewritten in {each:.4} ms per frame");
    }
}
