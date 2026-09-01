//! The sky, the sun, the ambient light and the fog, on the server's clock.
//!
//! ## Why this lives in `player/` and not in `world/`
//!
//! The sun used to be a constant in `world/render.rs`, spawned beside the terrain
//! meshes because a constant has to be spawned somewhere and that is where the
//! environment was. It is not a constant any more: it is a function of
//! `EntitySnapshot.tick_of_day`, and a snapshot arrives in exactly one place. Everything
//! in this module's parent is the same shape — bodies, mobs, drops, structures, vitals
//! and the camera are all *what the newest accepted snapshot said, drawn* — while
//! `world/` is the other pipeline entirely, fed by `WorldInbox` and ending in meshes.
//!
//! That is the argument `player/camera.rs` already made in the other direction when
//! movement landed: the camera moved *out* of `world/render.rs` the moment its position
//! stopped being a constant and started being a snapshot. The sun follows it for the same
//! reason, and moving it keeps the number of edges between `player` and `world` at the
//! four `client/AGENTS.md` enumerates rather than adding a fifth.
//!
//! ## Presentation only
//!
//! Nothing here decides a gameplay outcome. The server owns what the dark *means* — which
//! creatures spawn, and where — and this module owns only what it looks like. In
//! particular the light a fire casts on screen is not the radius in which the server
//! refuses to spawn anything, and no rule may ever be read back out of a colour computed
//! here.
//!
//! ## The boundaries are the server's, and there is no second copy of them
//!
//! `ServerWelcome` carries `day_length_ticks`, `night_start_ticks` and `night_end_ticks`,
//! validated together in `net/codec.rs` and held as one [`WorldClock`] on
//! [`Session`]. Dusk and dawn are ramps built around **those two boundaries**, read from
//! the session every frame; the only number this module contributes is how *long* a ramp
//! lasts ([`RAMP_SECONDS`]), which is not a boundary and which the wire does not carry.
//!
//! The reason is the one `schemas/handshake.fbs` gives: the night you see has to be the
//! night the server is simulating, because that is the night its spawn rules use. A
//! client that recomputed dusk from constants of its own would drift from the simulation
//! the moment either side's constant moved, and it would drift silently.
//!
//! ## A server with no clock
//!
//! `day_length_ticks == 0` is a legal, expected announcement — it is what every server in
//! this repository sends today, and it is the pre-V6 world in which time does not pass.
//! That world renders at exactly the four values it rendered at before this module
//! existed ([`Daylight::FIXED`]), and the client says so once in the log rather than
//! falling back in silence.

use std::f32::consts::{PI, TAU};
use std::time::Instant;

use bevy::asset::RenderAssetUsages;
use bevy::ecs::system::SystemParam;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use super::Weather;
use super::camera::WorldCamera;
use crate::net::{BlockCoord, Session, WeatherKind, WeatherState, WorldClock};
use crate::settings::Settings;
use crate::world::{ChunkStore, palette};

/// How long dusk and dawn take, in seconds of real time.
///
/// Converted to ticks through `ServerWelcome.tick_rate`, so a server that simulates at a
/// different rate still gets a minute-long sunset rather than a minute's worth of its own
/// ticks. It is deliberately **not** a boundary: `night_start_ticks` and
/// `night_end_ticks` are the server's to name and this is only how wide the transition
/// around them is drawn.
///
/// A minute is long enough that a player crossing open ground notices the light going and
/// has time to decide about it, and short enough that the middle of the night is most of
/// the night.
const RAMP_SECONDS: f32 = 60.0;

/// Where the sun stands at midday, in degrees above the horizon.
///
/// Sixty rather than overhead: this is a Norse winter, and a sun that never climbs far is
/// what makes the shadowless terrain read as relief instead of as a flat plan. It is also
/// where the old fixed constant already pointed — `Vec3::new(-0.4, -1.0, -0.25)` normalises
/// to about 65° above the horizon — so midday looks like the sky this client has always
/// had.
const MIDDAY_ALTITUDE_DEGREES: f32 = 60.0;

/// Where the light stands at dawn, at dusk, and for the whole night.
///
/// Low, **never negative**, and deliberately not lower than this. A directional light below
/// the horizon shines *upward*, which lights the undersides of terrain and the inward faces
/// of a dug shaft — an effect no sky produces and every player reads as a bug. But a light
/// that merely grazes the horizon is barely better at night, because with no shadow maps
/// the only thing separating a wall from the ground in front of it is which way each face
/// is turned, and a grazing light leaves the ground on the ambient term alone. Twenty
/// degrees is where the ground still catches enough of the night light to be walked on.
///
/// One value for all three moments, which is what makes the arc continuous with no second
/// blend: the daylight curve is already pinned here at both boundaries, so the sun neither
/// jumps nor climbs as it sets.
const HORIZON_ALTITUDE_DEGREES: f32 = 20.0;

/// The sky at midday, as sRGB — a dark Fimbulvetr overcast, so the terrain reads against
/// it. Unchanged from the constant `player/camera.rs` carried before this module existed.
const DAY_SKY: [f32; 3] = [0.055, 0.070, 0.094];

/// The sky at the middle of the night, as sRGB.
///
/// About a third of the day's value and a little bluer, which is as dark as the sky may go
/// while the horizon is still a horizon: the fog fades distant terrain into this colour, so
/// a black sky would turn the far edge of the streamed cube into a void rather than into
/// distance.
pub(crate) const NIGHT_SKY: [f32; 3] = [0.018, 0.024, 0.040];

/// The one directional light's illuminance at midday, in lux. Full daylight is ~100 000;
/// this is a winter overcast, and it is the constant `world/render.rs` carried before.
const DAY_ILLUMINANCE: f32 = 9_000.0;

/// The same light at the middle of the night, in lux.
///
/// A fifth of the day, which is what carries almost all of the change between the two: the
/// ambient floor below barely moves by comparison, so the sun collapsing is what night
/// *is*. Enough of it survives to keep the light directional — a face turned towards it
/// reads about half again as bright as the ground and the faces turned away are darker than
/// either — so the night has a shape rather than being uniformly grey.
const NIGHT_ILLUMINANCE: f32 = 1_800.0;

/// The ambient term at midday. Unchanged from `player/camera.rs`'s old constant: it lights
/// the downward faces enough to stay readable without flattening the relief.
const DAY_AMBIENT_BRIGHTNESS: f32 = 600.0;

/// **The ambient floor** — the ambient term at the middle of the night, and the number
/// that decides whether night is playable.
///
/// A face turned away from the night light is lit by **this and nothing else** — there are
/// no shadow maps and no per-voxel light, so the ambient term is the whole of what fills a
/// surface the sun does not reach. This is therefore the number that decides whether the far
/// side of a boulder is a dark shape or an invisible one.
///
/// How it was chosen. Bevy's ambient term is
/// `EnvBRDFApprox(base_colour, F_AB(1.0, NdotV)) * ambient_colour * brightness`, the camera
/// applies `Exposure::BLENDER` (`ev100 = 9.7`, a factor of 1.002e-3), and `AcesFitted`
/// tonemaps the result. For the stone in `world/palette.rs` that gives, in sRGB code values
/// out of 255:
///
/// | | ground | face towards the light | face away |
/// | --- | --- | --- | --- |
/// | midday (9 000 lx, ambient 600) | 168 | 137 | **39** |
/// | midnight (1 800 lx, ambient 480) | 52 | 81 | **31** |
///
/// **The surprise, and the reason this constant is 480 rather than something dramatic**: the
/// ambient term barely moves the picture. Dropping it to 400 takes the away-facing column
/// from 31 to 26 and to 300 takes it to 19, while the day-to-night change a player actually
/// sees — 168 down to 52 on the ground they are standing on — is the sun collapsing by a
/// factor of five. So the floor is set where the *shaded* faces stop being separable from
/// each other rather than where the night starts feeling dark, because darkness was never
/// this number's job. Below about 350 a wall facing away from the moon and the ground in
/// front of it converge, which is the shape of "unplayable" here: not a black screen, a
/// screen with no edges in it.
///
/// **Chosen against computed pixels, not against a running game.** The table above was
/// evaluated off Bevy's own shader arithmetic and rendered as swatches of every entry in
/// `world/palette.rs` under this curve; the author looked at those. The manual check the
/// issue asks for — one full cycle against a local server, on a calibrated monitor — needs a
/// server that declares a clock and is still owed. Tune this constant, not the curve.
const NIGHT_AMBIENT_BRIGHTNESS: f32 = 480.0;

/// Where the fog begins, as a fraction of the distance at which it is total.
///
/// Half: terrain is clear for the near half of what the server streams and dissolves
/// across the far half. Earlier and the world feels like a room; later and the fade is too
/// abrupt to hide the edge it exists to hide.
const FOG_START_FRACTION: f32 = 0.5;

/// The weather colours the server's kind moves the day/night sky towards, as sRGB.
///
/// These are presentation colours, not climate data. The sand tint is the issue's stated
/// ochre exactly; the other three are chosen around the existing winter sky: rain closes
/// it down to charcoal grey, snow fills it with pale overcast and a blizzard takes that
/// all the way to near-white.
const RAIN_TINT: [f32; 3] = [0.032, 0.036, 0.042];
const SNOW_TINT: [f32; 3] = [0.72, 0.75, 0.79];
const SAND_TINT: [f32; 3] = [0.45, 0.33, 0.14];
const BLIZZARD_TINT: [f32; 3] = [0.92, 0.95, 0.98];

/// The sky and the fog seen from inside water, as sRGB.
///
/// **Not a time of day, and it overrides one.** Under a lake the sun is above the surface and
/// what reaches the eye is whatever the water lets through, which does not care what hour it
/// is. Bright enough to be readable — going under must not read as going blind — and far
/// enough from every sky above it that the surface is a boundary the player can feel from
/// underneath.
const UNDERWATER_SKY: [f32; 3] = [0.05, 0.22, 0.35];

/// How far a submerged eye sees, in blocks.
///
/// Ten, and short on purpose: it makes water a place with its own scale rather than the same
/// view with a filter on it, and it is what stands between the player and the fact that this
/// client draws no caustics, no surface from below and no light shafts. The render-distance
/// setting is deliberately not consulted — that is a choice about how much *world* to draw,
/// and how far you see through water is a property of the water.
const UNDERWATER_VISIBILITY: f32 = 10.0;

/// Where the underwater fog begins, in blocks: the same half of the way out that
/// [`FOG_START_FRACTION`] puts it above the surface, so the fade has the same shape at a
/// tenth of the scale.
const UNDERWATER_START: f32 = UNDERWATER_VISIBILITY * FOG_START_FRACTION;

/// The colour the horizon takes at the middle of dusk and of dawn, as sRGB.
///
/// The band a low sun leaves along the rim while the zenith is already going out. Blended
/// **linearly**, component by component, exactly as [`weather_tint`] blends towards a
/// weather's colour; every colour constant here is sRGB and this one is no exception.
const DUSK_HORIZON: [f32; 3] = [0.55, 0.22, 0.08];

/// How far from the eye everything drawn on the sky sits, in blocks.
///
/// Past the far edge of the streamed cube at every render distance the settings offer, so
/// the dome encloses the world rather than standing in it. The fog is total well inside
/// this, which is why every material here carries `fog_enabled: false`.
///
/// A **radius**, not a position: the dome is centred on the eye and follows it, so the
/// player can never walk to the horizon or out from under the sky.
const SKY_BODY_DISTANCE: f32 = 400.0;

/// How far away the dome is drawn, and it must be **further than every body on it**.
///
/// **The dome used to be built at [`SKY_BODY_DISTANCE`] exactly, and that is why the sky
/// was empty.** The sun, the moon and the star field are billboards on a sphere of that
/// radius; a dome on the same radius is coincident with all three, so which one survives
/// the depth test is undefined — and in practice the dome won every time. The bodies were
/// spawned, made visible, placed and given the right alpha, and none of them was ever
/// drawn. Nothing reported it because nothing was wrong: at the level of the ECS the sky
/// was correct, which is what made it take a rendered capture to find.
///
/// A quarter further out, rather than a hair: the gap has to survive depth precision at
/// four hundred blocks, and the star field is drawn in the transparent pass, where it is
/// depth-tested against the dome without writing depth of its own. A margin measured in
/// float epsilons would be a bug waiting for a different GPU.
///
/// It costs nothing else. The dome is unlit, unfogged and drawn at whatever distance it is
/// given, and the camera's far plane is well beyond both.
const DOME_DISTANCE: f32 = SKY_BODY_DISTANCE * 1.25;

/// The dome must enclose the bodies, or none of them is drawn. See [`DOME_DISTANCE`].
const _: () = assert!(DOME_DISTANCE > SKY_BODY_DISTANCE);

/// How many rings the dome is divided into from zenith to nadir, and how many segments
/// around: 13 x 25 = 325 vertices.
///
/// The colour is a function of height alone, so the rings buy how smoothly the warm band
/// gives way to the zenith and the segments buy nothing but a rounder silhouette.
const DOME_RINGS: usize = 12;
const DOME_SEGMENTS: usize = 24;

/// How tightly the warm rim hugs the horizon.
///
/// The dome's colour is `lerp(horizon, sky, t.powf(HORIZON_FALLOFF))`, `t` being the
/// vertex's height above the rim as a unit fraction — so this decides whether dusk is a
/// band along the edge of the world or half the sky turning orange. Below one, so the
/// zenith wins quickly: a tenth of the way up is already nearly half way to it. At exactly
/// one the gradient is linear in height and the sunset fills the upper hemisphere, which is
/// not what a sunset looks like from inside one.
const HORIZON_FALLOFF: f32 = 0.35;

/// The angular radius of the sun and the moon, in degrees.
///
/// Three degrees across, against the half a degree the real sun subtends: at true size the
/// disc is four blocks wide at [`SKY_BODY_DISTANCE`] and reads as a speck. Also the threshold
/// a body disappears at, which is the moment its upper limb goes under.
const SKY_BODY_RADIUS_DEGREES: f32 = 1.5;

/// How many triangles a disc is fanned out of. Thirty-two, at which its rim departs from a
/// true circle by `1 - cos(PI / 32)`: half a percent of [`SKY_BODY_RADIUS_DEGREES`].
const DISC_SEGMENTS: usize = 32;

/// The sun's disc, as sRGB: a warm white, unlit, and unchanged by the hour. **It is not the
/// light** — a disc that dimmed as it set would be a second day-night curve over the one
/// [`DAY_ILLUMINANCE`] and [`NIGHT_ILLUMINANCE`] already draw.
const SUN_COLOUR: [f32; 3] = [1.0, 0.94, 0.82];

/// The moon's disc, as sRGB: paler and cooler than the sun, and always full.
const MOON_COLOUR: [f32; 3] = [0.78, 0.82, 0.90];

/// How many stars should be visible above any world horizon, within the small variation a
/// deterministic uniform draw produces.
const VISIBLE_STAR_COUNT: usize = 600;

/// How many stars the complete celestial shell holds. A sphere has twice the area of the
/// hemisphere the player can see, so doubling the old hemisphere budget preserves its apparent
/// density instead of either halving it or putting twice as many stars overhead. They still live
/// in one mesh, under one material, in one draw.
const STAR_COUNT: usize = VISIBLE_STAR_COUNT * 2;

/// The seed the star positions are drawn from. **A constant, and deliberately not
/// `world_seed`** — every world looks up at the same sky.
const STAR_SEED: u32 = 0x5EED_5747;

/// The three sizes a star is drawn at, in blocks at [`SKY_BODY_DISTANCE`], and how the field
/// is divided between them: mostly small, so it reads as depth and not as equal dots.
const STAR_SIZES: [f32; 3] = [1.4, 2.4, 4.0];
const STAR_SIZE_SHARES: [f32; 2] = [0.72, 0.94];

/// A star's colour, as sRGB. The alpha is not here: it is the night fraction.
const STAR_COLOUR: [f32; 3] = [0.90, 0.93, 1.0];

/// Where in the day the star field is in its unrotated orientation. Keeping that orientation at
/// full alpha preserves the field's original midnight pose while its complete shell turns once a
/// day: three quarters round, which [`sun_phase`] makes the middle of the night.
const MIDNIGHT_PHASE: f32 = 0.75;

/// Marks the one directional light this module owns.
#[derive(Component)]
pub struct Sun;

/// Marks an entity drawn **on** the sky rather than in the world.
///
/// Two rules, and they are the whole of what the marker means: it follows the eye's
/// *translation* and never its rotation, and it is hidden while the eye is under water,
/// because down there the sky is the water. [`follow_the_eye`] owns the first because the
/// eye is only final after `AimCamera`; [`drive_the_sky`] owns the second because "is the
/// eye under water" already has exactly one owner and must not grow a second.
#[derive(Component)]
pub struct SkyBody;

/// Which of the four things drawn on the sky an entity is — a value rather than four marker
/// components, because every rule here is a `match` on exactly this.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkyBodyKind {
    /// The gradient the other three are seen against.
    Dome,
    /// The *apparent* sun, which sets — not the light, which does not.
    Sun,
    /// The moon's disc, at the antisolar direction and always full.
    Moon,
    /// Every star, as one mesh.
    Stars,
}

/// Where one sky entity sits relative to the eye, as [`drive_the_sky`] last computed it and
/// [`follow_the_eye`] consumes it — the split [`SkyBody`] describes.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub(super) enum SkyPlacement {
    /// Centred on the eye and turned by this: the dome (never) and the field (once a day).
    Around(Quat),
    /// A billboard [`SKY_BODY_DISTANCE`] away in this unit direction, facing back at the eye.
    Facing(Vec3),
}

/// The one mesh the dome is drawn as, kept so its colour attribute can be rewritten.
///
/// A resource rather than a lookup through the entity, for the reason `precipitation.rs`
/// keeps its own: the handle is what `Assets<Mesh>` is indexed by.
#[derive(Resource, Debug)]
pub(super) struct SkyVisuals {
    dome: Handle<Mesh>,
    /// The star field's one material: its base colour's **alpha** is the night fraction, so
    /// the whole field fades through a single write.
    stars: Handle<StandardMaterial>,
}

/// Where the world's day is right now, as the newest **accepted** snapshot left it.
///
/// Two numbers and no boundaries: the boundaries live on [`Session`] and are read from
/// there, so there is exactly one copy of them in the client. This is only the anchor the
/// time of day is advanced from.
///
/// **Nothing but an accepted snapshot moves it.** `SnapshotBuffer::accept` is the one
/// wrap-aware test of whether a snapshot is newer than the newest held, and
/// `ingest_snapshots` sets this only when that test passes — so a duplicate or a
/// reordered frame cannot run the sky backwards, for the same reason and by the same
/// gate that stops it walking a player's health backwards.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq)]
pub struct SkyClock(Option<Anchor>);

/// One `(tick_of_day, arrival)` pair: the time of day the server named, and the instant the
/// net thread decoded the frame that named it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Anchor {
    tick_of_day: u32,
    at: Instant,
}

impl SkyClock {
    /// Records the time of day an accepted snapshot carried.
    pub fn anchor(&mut self, tick_of_day: u32, at: Instant) {
        self.0 = Some(Anchor { tick_of_day, at });
    }

    /// Where the day is at `now`, in ticks, or `None` before the first snapshot.
    ///
    /// **The advance is what keeps the sky smooth**, and it is the same trick
    /// `interpolate.rs` uses for a position: snapshots arrive twenty times a second and
    /// frames are drawn sixty, so a sky driven by the anchor alone would hold still for
    /// three frames and then jump, twenty times a second, in a scene whose whole subject is
    /// a gradual change. The tick rate is the
    /// server's own number, and the elapsed time is measured from the instant the net
    /// thread decoded the frame, so the advance is a reading of the server's clock rather
    /// than a clock of the client's own.
    ///
    /// It is monotonic between anchors. A new anchor can nudge the sample back by whatever
    /// the network's jitter was worth in ticks — the gap between the tick the server stamped
    /// a snapshot with and the instant this thread decoded it. That is well under a tick on
    /// a healthy connection and a handful of ticks after a hiccup; the bound is the jitter,
    /// not a number this code can promise. Out of a 24 000-tick day even the bad case moves
    /// the sun by a fraction of a degree.
    ///
    /// **A numerically smaller anchor is not a step backwards.** The advance above is taken
    /// modulo the day, so a client whose own extrapolation has already crossed midnight
    /// reads 12 where the next snapshot says 12, and the new anchor is continuous with it —
    /// `the_clock_re_anchors_across_midnight_without_jumping` is what pins that. It is also
    /// why refusing an anchor lower than the current reading would be a bug rather than a
    /// guard: once a day, the lower value is the correct one.
    pub(crate) fn ticks_at(
        &self,
        now: Instant,
        tick_rate: u8,
        day_length_ticks: u32,
    ) -> Option<f32> {
        let anchor = self.0?;
        let elapsed = now.saturating_duration_since(anchor.at).as_secs_f32();
        let advanced = anchor.tick_of_day as f32 + elapsed * f32::from(tick_rate);
        Some(advanced.rem_euclid(day_length_ticks as f32))
    }
}

/// Everything about the light that is a function of the time of day.
///
/// A plain value with no Bevy world in it, so the whole curve is testable as arithmetic:
/// boundaries and a tick in, four values out. The fog is here only as a colour — its
/// *distances* come from how far the server streams, which is not a time of day.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Daylight {
    /// The direction the light shines *in* — away from the sun, towards the ground.
    pub sun_direction: Vec3,
    /// The light's illuminance, in lux.
    pub sun_illuminance: f32,
    /// The camera's clear colour, and the colour the dome carries at its zenith.
    pub sky: Color,
    /// The colour the rim of the sky takes, and the colour distance fades into.
    ///
    /// **The fog fades terrain into this and not into [`Self::sky`]**, which is the whole
    /// point of a second colour: the far edge of the streamed cube sits on the horizon, so
    /// terrain dissolving into the zenith would dissolve into the wrong half of the sky the
    /// moment dusk gave the rim a colour of its own. Equal to [`Self::sky`] at midday and
    /// at midnight, and warm in between — see [`horizon_colour`].
    pub horizon: Color,
    /// The per-camera ambient term.
    pub ambient_brightness: f32,
}

impl Daylight {
    /// The sky a server that keeps no clock renders, which is exactly the sky this client
    /// rendered before there was a clock to read.
    ///
    /// The four constants that used to live in `player/camera.rs` and `world/render.rs`,
    /// carried here unchanged so that "a world with no time of day looks precisely as it
    /// always did" is one value rather than four scattered ones.
    pub const FIXED: Self = Self {
        sun_direction: Vec3::new(-0.4, -1.0, -0.25),
        sun_illuminance: DAY_ILLUMINANCE,
        sky: Color::srgb(DAY_SKY[0], DAY_SKY[1], DAY_SKY[2]),
        // The same colour, written out rather than computed: a world with no clock has no
        // dusk to be in the middle of, so its rim is its zenith and its dome is one flat
        // colour — which is exactly the sky it rendered before this module existed.
        // `horizon_colour` would answer this too, and a `const` cannot call it.
        horizon: Color::srgb(DAY_SKY[0], DAY_SKY[1], DAY_SKY[2]),
        ambient_brightness: DAY_AMBIENT_BRIGHTNESS,
    };

    /// The light at `tick_of_day`, for a world whose day the server described.
    ///
    /// Returns [`Self::FIXED`] unchanged when the clock was never declared. Every other
    /// function below is reached only through this guard, which is what lets them divide by
    /// `day_length_ticks` without asking again.
    pub fn at(clock: &WorldClock, tick_of_day: f32, tick_rate: u8) -> Self {
        if !clock.declared() {
            return Self::FIXED;
        }

        let night = night_fraction(clock, tick_of_day, RAMP_SECONDS * f32::from(tick_rate));
        let sky = Color::srgb(
            lerp(DAY_SKY[0], NIGHT_SKY[0], night),
            lerp(DAY_SKY[1], NIGHT_SKY[1], night),
            lerp(DAY_SKY[2], NIGHT_SKY[2], night),
        );

        Self {
            sun_direction: -sun_position(clock, tick_of_day),
            sun_illuminance: lerp(DAY_ILLUMINANCE, NIGHT_ILLUMINANCE, night),
            sky,
            horizon: horizon_colour(sky, night),
            ambient_brightness: lerp(DAY_AMBIENT_BRIGHTNESS, NIGHT_AMBIENT_BRIGHTNESS, night),
        }
    }
}

/// How much of [`DUSK_HORIZON`] the rim carries at this night fraction.
///
/// `4n(1 - n)`: zero where the night fraction is 0 and 1, one where it is a half, and — the
/// expression being unchanged by swapping `n` for `1 - n` — the same value at equal
/// distances either side of the peak.
///
/// **A parabola rather than the obvious half-sine, and for exactness rather than shape.**
/// The two differ by under five hundredths anywhere, but `(PI * 1.0).sin()` is `-8.7e-8`
/// and not zero, so a sine would leave the midnight rim a hair off the midnight sky — far
/// enough that `horizon == sky` would have to be a tolerance instead of an equality.
///
/// **The symmetry is inherited, not restated.** The bell is a function of
/// [`night_fraction`] and of nothing else, and that fraction is already its own mirror
/// across the two boundaries the server named — which is what
/// `dusk_and_dawn_are_mirror_images_of_each_other` pins. So a change to `RAMP_SECONDS` or
/// to either boundary moves both sides together by construction.
///
/// Clamped because the fraction is a colour input, and the clamp is what makes "in range"
/// a property of this function rather than an assumption about its caller.
fn dusk_bell(night: f32) -> f32 {
    let night = night.clamp(0.0, 1.0);
    4.0 * night * (1.0 - night)
}

/// The rim's colour: the sky, blended towards [`DUSK_HORIZON`] by [`dusk_bell`].
///
/// At the peak the rim **is** `DUSK_HORIZON` rather than a fraction of the way to it —
/// [`HORIZON_FALLOFF`] is what keeps that from being a wall of orange, and a peak that
/// stopped short would put a second tuning number beside the colour it scales.
fn horizon_colour(sky: Color, night: f32) -> Color {
    let amount = dusk_bell(night);
    let from = Srgba::from(sky);
    Color::srgb(
        lerp(from.red, DUSK_HORIZON[0], amount),
        lerp(from.green, DUSK_HORIZON[1], amount),
        lerp(from.blue, DUSK_HORIZON[2], amount),
    )
}

/// How much of the night has arrived at `tick_of_day`: 0 in full day, 1 in full night.
///
/// **The night is exactly the interval the server named**, `[night_start_ticks,
/// night_end_ticks)`, and the ramps sit in the daylight on either side of it rather than
/// inside it. That is deliberate and it is a gameplay-facing choice rather than an
/// aesthetic one: the server starts spawning at `night_start_ticks`, so the player has to
/// have watched the light go *before* that tick, not after it. Dusk therefore ends where
/// night begins, and dawn begins where night ends.
///
/// `ramp_ticks` is clamped to half the daylight so a server that declares a short day still
/// gets a continuous curve — the two ramps meet at midday instead of overlapping into a
/// discontinuity.
fn night_fraction(clock: &WorldClock, tick_of_day: f32, ramp_ticks: f32) -> f32 {
    let day_length = clock.day_length_ticks as f32;
    // Guaranteed by the decoder: 0 < night_start < night_end <= day_length. So the night
    // is neither empty nor the whole day, and it never wraps past tick zero — only the
    // daylight does.
    let night_length = (clock.night_end_ticks - clock.night_start_ticks) as f32;
    let daylight = day_length - night_length;

    let into_night = (tick_of_day - clock.night_start_ticks as f32).rem_euclid(day_length);
    if into_night < night_length {
        return 1.0;
    }

    let ramp = ramp_ticks.min(daylight * 0.5).max(f32::MIN_POSITIVE);
    let since_dawn = into_night - night_length;
    let until_dusk = day_length - into_night;
    1.0 - (since_dawn / ramp).min(until_dusk / ramp).min(1.0)
}

/// The unit vector from the world *towards* the sun at `tick_of_day`.
///
/// One revolution per day, and continuous across every boundary including tick zero. The
/// compass is the movement basis `player/structures.rs` already mirrors from the server —
/// North is -Z, East is +X, South is +Z, West is -X — so the sun rises in the east, crosses
/// the south at midday, sets in the west, and completes the circuit through the north while
/// it is down.
///
/// Its altitude is pinned to [`HORIZON_ALTITUDE_DEGREES`] at both boundaries and held there
/// for the whole night, which is what makes the arc continuous without a second blend: the
/// daylight arc reaches exactly that value at `night_end` and at `night_start`, so there is
/// no step for the night to hide.
fn sun_position(clock: &WorldClock, tick_of_day: f32) -> Vec3 {
    let day_length = clock.day_length_ticks as f32;
    let night_length = (clock.night_end_ticks - clock.night_start_ticks) as f32;
    let daylight = day_length - night_length;

    let since_dawn = (tick_of_day - clock.night_end_ticks as f32).rem_euclid(day_length);
    let azimuth = TAU * sun_phase(clock, tick_of_day);

    let day_progress = (since_dawn / daylight).min(1.0);
    let altitude = (HORIZON_ALTITUDE_DEGREES
        + (MIDDAY_ALTITUDE_DEGREES - HORIZON_ALTITUDE_DEGREES) * (PI * day_progress).sin())
    .to_radians();

    let (altitude_sin, altitude_cos) = altitude.sin_cos();
    let (azimuth_sin, azimuth_cos) = azimuth.sin_cos();
    Vec3::new(
        azimuth_cos * altitude_cos,
        altitude_sin,
        azimuth_sin * altitude_cos,
    )
}

/// How far round its one revolution the sun is at `tick_of_day`, in turns.
///
/// Zero at `night_end_ticks` and a half at `night_start_ticks`: half a revolution across the
/// daylight and half across the night, so the two meet at the boundaries and the whole is
/// continuous across every one of them, tick zero included.
///
/// **Extracted from [`sun_position`] rather than copied out of it**, so one azimuth is one
/// expression and the disc and the light cannot drift apart.
pub(crate) fn sun_phase(clock: &WorldClock, tick_of_day: f32) -> f32 {
    let day_length = clock.day_length_ticks as f32;
    let night_length = (clock.night_end_ticks - clock.night_start_ticks) as f32;
    let daylight = day_length - night_length;

    let since_dawn = (tick_of_day - clock.night_end_ticks as f32).rem_euclid(day_length);
    if since_dawn <= daylight {
        0.5 * since_dawn / daylight
    } else {
        0.5 + 0.5 * (since_dawn - daylight) / night_length
    }
}

/// Where the sun's **disc** stands at `tick_of_day`, in degrees above the horizon.
///
/// **A second curve**, because the light's own altitude never drops below
/// [`HORIZON_ALTITUDE_DEGREES`]. The *disc* is free to set: one sine over the whole
/// revolution, zero at both boundaries, continuous across tick zero because [`sun_phase`] is.
pub(super) fn apparent_sun_altitude(clock: &WorldClock, tick_of_day: f32) -> f32 {
    MIDDAY_ALTITUDE_DEGREES * (TAU * sun_phase(clock, tick_of_day)).sin()
}

/// The unit vector from the eye towards the sun's disc: the light's azimuth, its own altitude.
fn apparent_sun_direction(clock: &WorldClock, tick_of_day: f32) -> Vec3 {
    let phase = sun_phase(clock, tick_of_day);
    let (azimuth_sin, azimuth_cos) = (TAU * phase).sin_cos();
    let (altitude_sin, altitude_cos) = apparent_sun_altitude(clock, tick_of_day)
        .to_radians()
        .sin_cos();
    Vec3::new(
        azimuth_cos * altitude_cos,
        altitude_sin,
        azimuth_sin * altitude_cos,
    )
}

/// Everything the bodies on the sky are placed and faded by, at one tick of one day —
/// computed only where the server declared a clock, which is why a world without one draws no
/// disc, no moon and no stars.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ApparentSky {
    /// Towards the sun's disc. The moon is at its negation.
    sun: Vec3,
    /// The night fraction, which is the star field's alpha.
    night: f32,
    /// How far the star field has turned about the east-west axis.
    turn: Quat,
}

impl ApparentSky {
    fn at(clock: &WorldClock, tick_of_day: f32, tick_rate: u8) -> Self {
        Self {
            sun: apparent_sun_direction(clock, tick_of_day),
            night: night_fraction(clock, tick_of_day, RAMP_SECONDS * f32::from(tick_rate)),
            // About +X, the east-west axis, offset so the field keeps its original orientation
            // at the one hour it is at full alpha.
            turn: Quat::from_rotation_x(TAU * (sun_phase(clock, tick_of_day) - MIDNIGHT_PHASE)),
        }
    }

    /// Whether a body at `altitude` degrees has any part of itself above the horizon.
    fn above_the_horizon(altitude: f32) -> bool {
        altitude > -SKY_BODY_RADIUS_DEGREES
    }
}

/// How high a unit direction stands above the horizon, in degrees.
fn altitude_of(direction: Vec3) -> f32 {
    direction.y.clamp(-1.0, 1.0).asin().to_degrees()
}

fn lerp(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

/// Spawns the one directional light.
///
/// Shadow maps stay **off**, exactly as they were when this light lived in
/// `world/render.rs`: they belong to a lighting issue and this one puts them out of scope.
/// Without them the face normals plus the ambient term are what give the terrain its shape,
/// which is the reason [`NIGHT_AMBIENT_BRIGHTNESS`] above is a playability constant and not
/// a taste one.
pub(super) fn spawn_sun(mut commands: Commands) {
    let fixed = Daylight::FIXED;
    commands.spawn((
        Sun,
        DirectionalLight {
            illuminance: fixed.sun_illuminance,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::default().looking_to(fixed.sun_direction, Vec3::Y),
    ));
}

/// Builds the dome and stands it up hidden, centred on wherever the eye happens to be.
///
/// **Hidden, and that is not a detail.** `ui/character.rs` paints its own flat backdrop
/// while the creation screen is up by writing the camera's clear colour, and a dome in
/// front of it would be the sky overwriting a screen that deliberately has none. Nothing
/// here shows the dome; [`drive_the_sky`] does, and it returns before it can on every frame
/// there is no [`Session`] — exactly the span that screen owns.
///
/// The mesh is built once and never rebuilt: only its colour attribute moves, which is why
/// the handle is kept in [`SkyVisuals`].
pub(super) fn spawn_sky(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let fixed = Daylight::FIXED;
    let dome = meshes.add(dome_mesh(fixed.sky, fixed.horizon));
    let material = materials.add(StandardMaterial {
        // White is load-bearing, for the reason `world/render.rs` gives about the terrain
        // material: the shader multiplies the base colour by the vertex colour.
        base_color: Color::WHITE,
        // The sky is not a surface the sun falls on. Lit, the dome would be a second and
        // wrong day-night curve drawn over the one this module computes.
        unlit: true,
        // At `SKY_BODY_DISTANCE` the fog is total, so without this the dome would be
        // painted entirely in the fog's own colour and the gradient never seen.
        fog_enabled: false,
        // Seen from the inside, which is the one face a sky is ever seen from. `None`
        // rather than `Front` so a camera that somehow leaves the dome still sees a sky
        // rather than a hole.
        cull_mode: None,
        ..default()
    });

    commands.spawn((
        SkyBody,
        SkyBodyKind::Dome,
        // Never turned: a dome on the camera's rotation is a horizon you cannot look away from.
        SkyPlacement::Around(Quat::IDENTITY),
        Mesh3d(dome.clone()),
        MeshMaterial3d(material),
        Transform::default(),
        Visibility::Hidden,
    ));

    let disc = meshes.add(disc_mesh());
    for (kind, colour) in [
        (SkyBodyKind::Sun, SUN_COLOUR),
        (SkyBodyKind::Moon, MOON_COLOUR),
    ] {
        commands.spawn((
            SkyBody,
            kind,
            SkyPlacement::Facing(Vec3::Y),
            Mesh3d(disc.clone()),
            MeshMaterial3d(
                materials.add(sky_material(Color::srgb(colour[0], colour[1], colour[2]))),
            ),
            Transform::default(),
            Visibility::Hidden,
        ));
    }

    let stars = materials.add(StandardMaterial {
        base_color: Color::srgba(STAR_COLOUR[0], STAR_COLOUR[1], STAR_COLOUR[2], 0.0),
        alpha_mode: AlphaMode::Blend,
        unlit: true,
        fog_enabled: false,
        cull_mode: None,
        ..default()
    });
    commands.spawn((
        SkyBody,
        SkyBodyKind::Stars,
        SkyPlacement::Around(Quat::IDENTITY),
        Mesh3d(meshes.add(star_mesh())),
        MeshMaterial3d(stars.clone()),
        Transform::default(),
        Visibility::Hidden,
    ));

    commands.insert_resource(SkyVisuals { dome, stars });
}

/// The material every opaque body on the sky is drawn with.
///
/// Unlit and unfogged for the reasons the dome's material gives, and `cull_mode: None`
/// because a billboard is seen from whichever face the maths presents.
fn sky_material(colour: Color) -> StandardMaterial {
    StandardMaterial {
        base_color: colour,
        unlit: true,
        fog_enabled: false,
        cull_mode: None,
        ..default()
    }
}

/// Half the width of a disc at [`SKY_BODY_DISTANCE`], in blocks.
fn disc_half_width() -> f32 {
    SKY_BODY_DISTANCE * SKY_BODY_RADIUS_DEGREES.to_radians().tan()
}

/// One disc in the XY plane at the angular size of the sun and the moon, built at its final
/// size rather than scaled by the transform, so [`follow_the_eye`] never writes a scale.
///
/// **A fan, not a quad**: nothing here is textured, so a quad's silhouette is the quad — a
/// square sun whose corners stand at `sqrt(2) * SKY_BODY_RADIUS_DEGREES`, 2.12°, and whose
/// angular size therefore depends on which way across it is measured.
fn disc_mesh() -> Mesh {
    let half = disc_half_width();
    let mut positions = vec![[0.0, 0.0, 0.0]];
    let mut uvs = vec![[0.5, 0.5]];
    let mut indices = Vec::with_capacity(DISC_SEGMENTS * 3);
    for segment in 0..DISC_SEGMENTS {
        let (sin, cos) = (TAU * segment as f32 / DISC_SEGMENTS as f32).sin_cos();
        positions.push([half * cos, half * sin, 0.0]);
        uvs.push([0.5 + 0.5 * cos, 0.5 - 0.5 * sin]);
        let rim = 1 + segment as u32;
        indices.extend_from_slice(&[0, rim, 1 + (rim % DISC_SEGMENTS as u32)]);
    }
    sky_mesh(positions, uvs, indices, Vec3::NEG_Z)
}

/// Every star as one mesh, in the field's own space: quads on a sphere of
/// [`SKY_BODY_DISTANCE`], each one already square-on to the centre. **The billboard is baked
/// in, and that is what makes this one draw** — the eye is always at this mesh's origin, so a
/// quad in the tangent plane at its own position faces it at every rotation.
fn star_mesh() -> Mesh {
    let mut quads = Vec::with_capacity(STAR_COUNT);
    for star in 0..STAR_COUNT as u32 {
        // Uniform in height is uniform density on the sphere — Archimedes' hat-box.
        let height = 2.0 * hash_unit(star * 3) - 1.0;
        let azimuth = TAU * hash_unit(star * 3 + 1);
        let radius = (1.0 - height * height).sqrt();
        let (azimuth_sin, azimuth_cos) = azimuth.sin_cos();
        let towards = Vec3::new(radius * azimuth_cos, height, radius * azimuth_sin);

        let half = star_size(hash_unit(star * 3 + 2)) * 0.5;
        // Degenerate only at exactly either pole, where the fallback is as good a tangent basis.
        let right = towards.cross(Vec3::Y).try_normalize().unwrap_or(Vec3::X);
        quads.push((
            towards * SKY_BODY_DISTANCE,
            right * half,
            right.cross(towards) * half,
        ));
    }
    quad_mesh(&quads, Vec3::NEG_Z)
}

/// Which of [`STAR_SIZES`] this draw lands in, in blocks.
fn star_size(draw: f32) -> f32 {
    if draw < STAR_SIZE_SHARES[0] {
        STAR_SIZES[0]
    } else if draw < STAR_SIZE_SHARES[1] {
        STAR_SIZES[1]
    } else {
        STAR_SIZES[2]
    }
}

/// A mesh of quads, each given as its centre and its two half-axes.
fn quad_mesh(quads: &[(Vec3, Vec3, Vec3)], normal: Vec3) -> Mesh {
    let mut positions = Vec::with_capacity(quads.len() * 4);
    let mut indices = Vec::with_capacity(quads.len() * 6);
    for (centre, right, up) in quads {
        let first = positions.len() as u32;
        positions.extend_from_slice(&[
            (*centre - *right - *up).to_array(),
            (*centre + *right - *up).to_array(),
            (*centre + *right + *up).to_array(),
            (*centre - *right + *up).to_array(),
        ]);
        indices.extend_from_slice(&[first, first + 1, first + 2, first, first + 2, first + 3]);
    }
    let uvs = (0..quads.len())
        .flat_map(|_| [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]])
        .collect();
    sky_mesh(positions, uvs, indices, normal)
}

/// Positions, texture coordinates and indices as the one mesh a sky body is drawn from.
fn sky_mesh(positions: Vec<[f32; 3]>, uvs: Vec<[f32; 2]>, indices: Vec<u32>, normal: Vec3) -> Mesh {
    let vertices = positions.len();
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    // Never read — every material here is unlit — and present because a `StandardMaterial`
    // mesh in this crate carries the three attributes `mobs.rs` writes.
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![normal.to_array(); vertices])
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_indices(Indices::U32(indices))
}

/// One deterministic value in `[0, 1)` per index, out of [`STAR_SEED`].
///
/// Pelle Evensen's `lowbias32` avalanche, which `precipitation.rs` uses for the same job and
/// reason: consecutive inputs are all this ever gets. **Not shared with that copy**, which
/// would couple two presentation modules both ways.
fn hash_unit(index: u32) -> f32 {
    let mut hash = index ^ STAR_SEED;
    hash ^= hash >> 16;
    hash = hash.wrapping_mul(0x7feb_352d);
    hash ^= hash >> 15;
    hash = hash.wrapping_mul(0x846c_a68b);
    hash ^= hash >> 16;
    // `2^32` rather than `u32::MAX`, so the result is a half-open [0, 1).
    hash as f32 / 4_294_967_296.0
}

/// The unit height and unit radius of every ring, from the zenith down to the nadir.
///
/// **One iterator, two consumers, and that is the point.** [`dome_mesh`] writes the
/// positions from it and [`dome_colours`] the colours, so the two vectors share an order by
/// construction rather than by two loops that happen to agree — a colour attribute out of
/// step with its positions is a gradient subtly wrong everywhere and obviously wrong
/// nowhere.
fn dome_rings() -> impl Iterator<Item = (f32, f32)> {
    (0..=DOME_RINGS).map(|ring| {
        let polar = PI * ring as f32 / DOME_RINGS as f32;
        (polar.cos(), polar.sin())
    })
}

/// How many vertices the dome carries.
const fn dome_vertex_count() -> usize {
    (DOME_RINGS + 1) * (DOME_SEGMENTS + 1)
}

/// The dome as it is built: a sphere of [`SKY_BODY_DISTANCE`] turned outside in.
///
/// Wound and normalled **inward**, because the only camera that will ever see it is inside
/// it. The normals are never read — the material is unlit — and are written because every
/// `StandardMaterial` mesh in this crate carries them; the winding is what would matter if
/// the material stopped being `cull_mode: None`. The seam is a duplicated column of
/// vertices rather than a wrapped index: thirteen vertices, for a colour attribute that can
/// be written as one flat run per ring.
fn dome_mesh(sky: Color, horizon: Color) -> Mesh {
    let mut positions = Vec::with_capacity(dome_vertex_count());
    let mut normals = Vec::with_capacity(dome_vertex_count());

    for (height, radius) in dome_rings() {
        for segment in 0..=DOME_SEGMENTS {
            let azimuth = TAU * segment as f32 / DOME_SEGMENTS as f32;
            let (azimuth_sin, azimuth_cos) = azimuth.sin_cos();
            let unit = Vec3::new(radius * azimuth_cos, height, radius * azimuth_sin);
            positions.push((unit * DOME_DISTANCE).to_array());
            normals.push((-unit).to_array());
        }
    }

    let stride = DOME_SEGMENTS + 1;
    let mut indices = Vec::with_capacity(DOME_RINGS * DOME_SEGMENTS * 6);
    for ring in 0..DOME_RINGS {
        for segment in 0..DOME_SEGMENTS {
            let top = (ring * stride + segment) as u32;
            let bottom = top + stride as u32;
            indices.extend_from_slice(&[top, top + 1, bottom, top + 1, bottom + 1, bottom]);
        }
    }

    Mesh::new(
        PrimitiveTopology::TriangleList,
        // Both copies stay: the renderer consumes one while `drive_the_sky` rewrites the
        // main-world colour attribute whenever the hour moves.
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, dome_colours(sky, horizon))
    .with_inserted_indices(Indices::U32(indices))
}

/// The gradient, as one colour per vertex: `sky` at the zenith and `horizon` at the rim.
///
/// Everything at or below the rim is the horizon colour flat. The lower half is only ever
/// seen where terrain has not been streamed yet, and more horizon is what a player expects
/// to find at the bottom of the sky; a zenith colour there would read as a second sky under
/// their feet.
fn dome_colours(sky: Color, horizon: Color) -> Vec<[f32; 4]> {
    let sky = Srgba::from(sky);
    let horizon = Srgba::from(horizon);
    let mut colours = Vec::with_capacity(dome_vertex_count());
    for (height, _) in dome_rings() {
        let towards_the_zenith = height.max(0.0).powf(HORIZON_FALLOFF);
        let colour = [
            lerp(horizon.red, sky.red, towards_the_zenith),
            lerp(horizon.green, sky.green, towards_the_zenith),
            lerp(horizon.blue, sky.blue, towards_the_zenith),
            1.0,
        ];
        colours.extend(std::iter::repeat_n(colour, DOME_SEGMENTS + 1));
    }
    colours
}

/// Puts everything drawn on the sky back around the eye.
///
/// **After [`super::camera::AimCamera`], and it is the one system here that has to be.**
/// `drive_the_sky` deliberately reads a camera position one frame old and says why — a sky
/// *colour* one frame late is invisible. A sky *position* one frame late is not: the dome is
/// centred on the eye, so a frame's worth of sprinting shows as the whole horizon sliding.
///
/// The translation only, for the reason [`SkyBody`] gives.
pub(super) fn follow_the_eye(
    eyes: Query<&Transform, (With<WorldCamera>, Without<SkyBody>)>,
    mut bodies: Query<(&mut Transform, &SkyPlacement), With<SkyBody>>,
) {
    let Some(at) = eyes.iter().next().map(|eye| eye.translation) else {
        return;
    };
    for (mut transform, placement) in &mut bodies {
        let wanted = match *placement {
            SkyPlacement::Around(turn) => Transform {
                translation: at,
                rotation: turn,
                scale: Vec3::ONE,
            },
            // `looking_at` leaves the plane square-on whichever face it presents, which is
            // why every disc material is `cull_mode: None`.
            SkyPlacement::Facing(direction) => {
                Transform::from_translation(at + direction * SKY_BODY_DISTANCE)
                    .looking_at(at, Vec3::Y)
            }
        };
        // Guarded, because `Mut` marks a component changed on every `DerefMut` and a player
        // standing still under a clockless sky is the common case.
        if *transform != wanted {
            *transform = wanted;
        }
    }
}

/// Everything the sky is computed from, and nothing it writes.
///
/// One `SystemParam` rather than four resources threaded through a signature, for the reason
/// [`super::camera::Aim`] is one: it names "the state the light is a function of" and it keeps
/// [`drive_the_sky`] inside the argument budget the store took it past.
#[derive(SystemParam)]
pub(super) struct SkyInputs<'w> {
    session: Option<Res<'w, Session>>,
    clock: Res<'w, SkyClock>,
    weather: Res<'w, Weather>,
    /// Read for exactly one question — is the eye inside a voxel of water — and read
    /// only. This is the fourth edge from `player` to `world`, and `client/AGENTS.md`
    /// enumerates it beside the other three.
    store: Option<Res<'w, ChunkStore>>,
    settings: Option<Res<'w, Settings>>,
}

/// Everything the sky is *drawn* on, as one parameter: the mesh whose colour the hour
/// rewrites, and the entities the water hides.
///
/// Grouped for the reason [`SkyInputs`] is, and for one more — [`drive_the_sky`] is at the
/// argument budget `clippy::too_many_arguments` allows.
#[derive(SystemParam)]
pub(super) struct SkyGeometry<'w, 's> {
    visuals: Option<Res<'w, SkyVisuals>>,
    meshes: ResMut<'w, Assets<Mesh>>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
    bodies: Query<
        'w,
        's,
        (
            &'static SkyBodyKind,
            &'static mut Visibility,
            &'static mut SkyPlacement,
        ),
        With<SkyBody>,
    >,
}

/// The previous-frame facts needed only to avoid redundant writes.
#[derive(Default)]
pub(super) struct SkyMemory {
    announced: bool,
    submerged: bool,
    weather: Option<WeatherState>,
    /// The night fraction the star material's alpha was last written from.
    stars: Option<f32>,
    /// The `(sky, horizon)` pair the dome's colour attribute was last written from.
    ///
    /// The dome's write is a **buffer upload** rather than a component assignment, so this
    /// guard is the one that matters most: a server with no clock would otherwise
    /// re-extract 325 vertices into the render world on every frame of the session.
    dome: Option<(Color, Color)>,
}

/// Puts the sun, the sky, the ambient term and the fog where the server's clock says they
/// are.
///
/// Registered by `PlayerPlugin` inside the chain that begins with `ingest_snapshots`, for
/// the reason `mobs.rs` gives at its own registration: a system ordered against the
/// `ApplySnapshots` *set* is ordered against its members rather than after them, and could
/// run before the snapshot it reads has landed.
///
/// **It writes nothing while the clock is undeclared**, which is both the required
/// behaviour and the change-detection guard: the four values are already at
/// [`Daylight::FIXED`] where `spawn_sun` and `spawn_camera` left them, so touching them
/// every frame would only mark two components changed for the rest of the session. The fog
/// is the exception and is set either way, because how far the world is streamed is not a
/// time of day.
///
/// **Water is the second exception, and it overrides the clock rather than blending with it.**
/// When the camera's eye is inside a voxel of water the sky and the fog become
/// [`UNDERWATER_SKY`] over [`UNDERWATER_VISIBILITY`] blocks, whatever hour it is and whether
/// or not the server keeps a clock. Only those two: the sun's direction, its illuminance and
/// the ambient term stay where the day left them, so terrain under water is lit as terrain
/// and tinted rather than re-illuminated.
pub(super) fn drive_the_sky(
    read: SkyInputs<'_>,
    mut geometry: SkyGeometry<'_, '_>,
    mut sun: Query<(&mut DirectionalLight, &mut Transform), With<Sun>>,
    mut cameras: Query<
        (
            Entity,
            &mut Camera,
            &mut AmbientLight,
            Option<&mut DistanceFog>,
        ),
        With<WorldCamera>,
    >,
    // Read separately from the query above, and `Without<Sun>` because the sun's `Transform`
    // is taken mutably here: Bevy cannot prove the two filters name different entities and
    // refuses the system rather than risk aliasing them.
    eyes: Query<&Transform, (With<WorldCamera>, Without<Sun>)>,
    mut commands: Commands,
    mut memory: Local<SkyMemory>,
) {
    let SkyInputs {
        session,
        clock,
        weather,
        store,
        settings,
    } = read;
    let Some(session) = session else {
        return;
    };
    let params = session.0;
    let declared = params.clock.declared();

    if !memory.announced {
        memory.announced = true;
        if declared {
            info!(
                "the server keeps a clock: a day is {} ticks, night runs {}..{}",
                params.clock.day_length_ticks,
                params.clock.night_start_ticks,
                params.clock.night_end_ticks
            );
        } else {
            info!(
                "this server declares no day length, so it has no time of day: the sky, \
                 the sun and the ambient light stay at their fixed values"
            );
        }
    }

    // `declared` is checked before the clock is read and not after, so `day_length_ticks` is
    // never handed to the advance as a zero to take a remainder against — which answers
    // `NaN`, and a `NaN` that reached a colour would propagate through every value
    // downstream. Same rule as `net/codec.rs`: reject the shape, never repair the number.
    //
    // `None` covers both refusals — no clock, and the frames before the first snapshot named
    // a time of day. Neither has an hour, so neither draws a sun, a moon or a star.
    let sampled = declared
        .then(|| {
            clock.ticks_at(
                Instant::now(),
                params.tick_rate,
                params.clock.day_length_ticks,
            )
        })
        .flatten();
    let light = match sampled {
        Some(tick_of_day) => Daylight::at(&params.clock, tick_of_day, params.tick_rate),
        None => Daylight::FIXED,
    };
    let apparent =
        sampled.map(|tick_of_day| ApparentSky::at(&params.clock, tick_of_day, params.tick_rate));

    if declared {
        for (mut directional, mut transform) in &mut sun {
            directional.illuminance = light.sun_illuminance;
            *transform = Transform::default().looking_to(light.sun_direction, Vec3::Y);
        }
    }

    // How far the fog reaches is a distance rather than an hour, so only its colour follows
    // the clock.
    //
    // **Two numbers meet here and only one of them is a setting.** The player's render
    // distance is the client's own choice — `crate::settings` says why, and never reads
    // `ServerWelcome.view_distance` into it — and what the server streams is a *ceiling* on
    // it, applied here at the moment of drawing rather than copied into the setting: fog that
    // reached past the last chunk the server sent would put an edge of nothing where the
    // horizon should be.
    let (chosen_distance, fog_start, brightness_scale) = match settings.as_deref() {
        Some(settings) => (
            settings.render_distance(),
            settings.fog_start(),
            settings.brightness(),
        ),
        None => (params.view_distance, FOG_START_FRACTION, 1.0),
    };
    let base_span = fog_span(
        chosen_distance,
        params.view_distance,
        params.chunk_size,
        fog_start,
    );
    let current_weather = weather.get();
    let (start, end) = weather_fog_span(base_span, current_weather);
    let weather_sky = weather_tint(light.sky, current_weather);
    // The same tint over both, so a blizzard closes the whole sky down rather than leaving
    // an orange band under a white one. The two colours differ by the hour, never by the
    // weather.
    let weather_horizon = weather_tint(light.horizon, current_weather);
    let ambient_brightness = light.ambient_brightness * brightness_scale;

    // **The one thing in this module that is a function of where the player is.** `AimCamera`
    // moves the camera after the set this system runs in, so the position read here is the one
    // the previous frame drew from — which is why crossing the surface changes the sky on the
    // next frame rather than this one.
    let submerged = eyes.iter().next().is_some_and(|eye| {
        submerged_at(
            store.as_deref(),
            eye.translation,
            usize::from(params.chunk_size),
        )
    });
    let (sky, horizon, start, end) = if submerged {
        // One colour for both under water: there is no rim down here, and the fog reaching
        // ten blocks is what the eye reads as the edge of what water lets through.
        (
            submerged_sky(),
            submerged_sky(),
            UNDERWATER_START,
            UNDERWATER_VISIBILITY,
        )
    } else {
        (weather_sky, weather_horizon, start, end)
    };
    let weather_changed = memory.weather != current_weather;

    paint_the_sky(
        &mut geometry,
        &mut memory,
        submerged,
        apparent,
        weather_sky,
        weather_horizon,
    );

    for (entity, mut camera, mut ambient, fog) in &mut cameras {
        // Written on the frame the player goes under and on the frame they come back
        // up, whatever the clock says — a server with no time of day still has to stop
        // showing the underwater sky once the eye leaves the water. `*was_submerged` is
        // what carries the second of those: `ClearColorConfig` has no `PartialEq` to
        // compare against, so the restoring write is triggered by the transition rather
        // than by a difference.
        if declared || submerged || memory.submerged || weather_changed {
            camera.clear_color = ClearColorConfig::Custom(sky);
        }
        // Outside the `declared` gate and guarded instead, because the brightness setting
        // moves on a server with no clock too — and the guard is what keeps an undeclared sky
        // from marking the component changed on every frame, which is the property the gate
        // was there for.
        if ambient.brightness != ambient_brightness {
            ambient.brightness = ambient_brightness;
        }
        match fog {
            // Read through `Deref` and written only on a difference. `Mut` marks a
            // component changed on every `DerefMut`, and on the undeclared path nothing here
            // ever moves — an unconditional write would re-extract the fog into the render
            // world on every frame of a session whose sky is a constant.
            Some(mut fog) => {
                if weather_changed
                    || fog.color != horizon
                    || !fades_between(&fog.falloff, start, end)
                {
                    fog.color = horizon;
                    fog.falloff = FogFalloff::Linear { start, end };
                }
            }
            None => {
                commands.entity(entity).insert(DistanceFog {
                    color: horizon,
                    falloff: FogFalloff::Linear { start, end },
                    ..default()
                });
            }
        }
    }

    memory.submerged = submerged;
    memory.weather = current_weather;
}

/// Shows or hides everything on the sky, and repaints the dome when its gradient has moved.
///
/// Called from [`drive_the_sky`] and only from there, so the eye it hides for is the same
/// eye the fog was computed against.
///
/// **The dome is painted from the sky above the water, never from the water itself.** It is
/// hidden while the eye is under, so a repaint there is an upload nobody sees — and the pair
/// the guard remembers would then flap between the water and the hour on every crossing of
/// the surface, which is two redundant uploads apiece rather than none.
///
/// `apparent` is `None` for a world with no hour, and that one value carries the whole of
/// "the fixed sky draws no bodies": the dome is still shown; the other three are not.
fn paint_the_sky(
    geometry: &mut SkyGeometry<'_, '_>,
    memory: &mut SkyMemory,
    submerged: bool,
    apparent: Option<ApparentSky>,
    sky: Color,
    horizon: Color,
) {
    for (kind, mut visibility, mut placement) in &mut geometry.bodies {
        let (wanted, wanted_placement) = match (kind, apparent) {
            (SkyBodyKind::Dome, _) => (!submerged, Some(SkyPlacement::Around(Quat::IDENTITY))),
            (_, None) => (false, None),
            (SkyBodyKind::Sun, Some(sky)) => (
                !submerged && ApparentSky::above_the_horizon(altitude_of(sky.sun)),
                Some(SkyPlacement::Facing(sky.sun)),
            ),
            // The antisolar point, which is where a full moon is by definition.
            (SkyBodyKind::Moon, Some(sky)) => (
                !submerged && ApparentSky::above_the_horizon(altitude_of(-sky.sun)),
                Some(SkyPlacement::Facing(-sky.sun)),
            ),
            // Shown at every hour and faded by its alpha, so midday costs no write.
            (SkyBodyKind::Stars, Some(sky)) => (!submerged, Some(SkyPlacement::Around(sky.turn))),
        };

        let wanted = if wanted {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *visibility != wanted {
            *visibility = wanted;
        }
        if let Some(wanted) = wanted_placement
            && *placement != wanted
        {
            *placement = wanted;
        }
    }

    let night = apparent.map_or(0.0, |sky| sky.night);
    if memory.stars != Some(night)
        && let Some(visuals) = geometry.visuals.as_deref()
        && let Some(mut material) = geometry.materials.get_mut(&visuals.stars)
    {
        material.base_color = material.base_color.with_alpha(night);
        memory.stars = Some(night);
    }

    if submerged || memory.dome == Some((sky, horizon)) {
        return;
    }
    let Some(visuals) = geometry.visuals.as_deref() else {
        return;
    };
    let Some(mut mesh) = geometry.meshes.get_mut(&visuals.dome) else {
        return;
    };
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, dome_colours(sky, horizon));
    memory.dome = Some((sky, horizon));
}

/// Whether the voxel holding `eye` is water.
///
/// Presentation, and only presentation: the server decides what being in water *does* —
/// `world.Fluid` drives the swim physics and this client predicts none of it — and this
/// decides what it looks like. The two read the same block id and agree by construction
/// rather than by a second copy of the rule.
///
/// A store this session has no chunk for answers air, which is [`ChunkStore::block_at`]'s
/// own rule: a lake nobody has been sent is not one the player is inside.
///
/// **Read by `player/precipitation.rs` as well**, because water overrides the precipitation
/// volume for the same reason it overrides the fog, and two copies of "is the eye under
/// water" would be two answers the moment either moved.
pub(super) fn submerged_at(store: Option<&ChunkStore>, eye: Vec3, chunk_size: usize) -> bool {
    let Some(store) = store else {
        return false;
    };
    if !eye.is_finite() {
        return false;
    }
    // `floor`, never a cast, for the reason `player/target.rs`'s raycast gives: `-0.5 as
    // i32` truncates to 0 and the voxel containing -0.5 is -1. Half the world is on that
    // side of the origin.
    let voxel = eye.floor().as_ivec3();
    let pos = BlockCoord {
        x: voxel.x,
        y: voxel.y,
        z: voxel.z,
    };
    palette::is_water(store.block_at(pos, chunk_size))
}

/// How much of the night has arrived right now, or `None` for a world with no night.
///
/// **Read by `player/birds.rs` and by nothing else.** The flock roosts on the same curve the
/// sky is tinted from rather than on a second reading of the clock, so "it is night" has one
/// answer in this client — the same reason [`submerged_at`] is shared with
/// `player/precipitation.rs`.
///
/// `None` is two cases and they are deliberately one answer: a server that declares no day
/// length has no night to roost through, and a session whose first snapshot has not landed
/// has not been told where the day is. Both fly the birds, which is what a world with no time
/// of day looked like before there was a clock to read.
pub(super) fn night_now(clock: &SkyClock, session: &Session) -> Option<f32> {
    let params = session.0;
    if !params.clock.declared() {
        return None;
    }
    let tick_of_day = clock.ticks_at(
        Instant::now(),
        params.tick_rate,
        params.clock.day_length_ticks,
    )?;
    Some(night_fraction(
        &params.clock,
        tick_of_day,
        RAMP_SECONDS * f32::from(params.tick_rate),
    ))
}

/// The colour a submerged camera clears to and fades into.
fn submerged_sky() -> Color {
    Color::srgb(UNDERWATER_SKY[0], UNDERWATER_SKY[1], UNDERWATER_SKY[2])
}

/// Where the fog begins and where it is total, in blocks.
///
/// **The nearer of the two distances wins, and only one of them is a setting.** `chosen` is
/// the client's own render distance — `crate::settings` owns it, persists it, and never takes
/// it from a server — while `streamed` is `ServerWelcome.view_distance`, how far the server
/// actually sends chunks. Fog that reached past the last chunk that arrived would draw an
/// edge of nothing where the horizon belongs, so the server's number is a ceiling applied
/// here, at the moment of drawing. It is never copied into the setting: turn the slider down
/// on a generous server and the horizon comes in; turn it up on a stingy one and nothing
/// moves, because there is nothing further out to show.
///
/// `max(1.0)` keeps `start` strictly below `end` for a server that streams a single chunk,
/// which would otherwise divide by zero in the shader.
fn fog_span(chosen: u8, streamed: u8, chunk_size: u16, fog_start: f32) -> (f32, f32) {
    let end = (f32::from(chosen.min(streamed)) * f32::from(chunk_size)).max(1.0);
    (end * fog_start, end)
}

/// Pulls both ends of the horizon towards the eye by the weather's visibility scale.
///
/// A clear sky and absent weather are the identity. Blizzard uses the snow scale at full
/// strength whatever intensity the field happens to carry, matching the full volume and
/// full tint it draws elsewhere.
fn weather_fog_span(span: (f32, f32), weather: Option<WeatherState>) -> (f32, f32) {
    let scale = weather.map_or(1.0, |weather| {
        let intensity = weather_intensity(weather);
        match weather.kind {
            WeatherKind::Clear => 1.0,
            WeatherKind::Rain => 1.0 - 0.4 * intensity,
            WeatherKind::Snow | WeatherKind::Sandstorm | WeatherKind::Blizzard => {
                1.0 - 0.75 * intensity
            }
        }
    });
    (span.0 * scale, span.1 * scale)
}

/// Blends the day/night sky underneath towards this weather's tint.
fn weather_tint(sky: Color, weather: Option<WeatherState>) -> Color {
    let Some(weather) = weather else {
        return sky;
    };
    let tint = match weather.kind {
        WeatherKind::Clear => return sky,
        WeatherKind::Rain => RAIN_TINT,
        WeatherKind::Snow => SNOW_TINT,
        WeatherKind::Sandstorm => SAND_TINT,
        WeatherKind::Blizzard => BLIZZARD_TINT,
    };
    let amount = weather_intensity(weather);
    let from = Srgba::from(sky);
    Color::srgb(
        lerp(from.red, tint[0], amount),
        lerp(from.green, tint[1], amount),
        lerp(from.blue, tint[2], amount),
    )
}

/// Intensity as the unit fraction the presentation paths consume.
///
/// Blizzard is full by definition on this side: the server's storm override names the
/// kind, and the volume, fog and tint all have to present the same severity.
fn weather_intensity(weather: WeatherState) -> f32 {
    if weather.kind == WeatherKind::Blizzard {
        1.0
    } else {
        f32::from(weather.intensity) / f32::from(u8::MAX)
    }
}

/// Whether a falloff is already the linear fade this module would write.
///
/// `FogFalloff` carries no `PartialEq`, and the alternative to asking is writing the
/// component every frame.
fn fades_between(falloff: &FogFalloff, start: f32, end: f32) -> bool {
    matches!(
        falloff,
        FogFalloff::Linear { start: from, end: to } if *from == start && *to == end
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// A world whose day is 24 000 ticks with a night from 14 400 to 21 600 — the shape
    /// `net/handshake.rs`'s own fixtures use.
    fn clock() -> WorldClock {
        WorldClock {
            day_length_ticks: 24_000,
            night_start_ticks: 14_400,
            night_end_ticks: 21_600,
        }
    }

    const TICK_RATE: u8 = 20;
    /// What [`RAMP_SECONDS`] is worth at [`TICK_RATE`].
    const RAMP: f32 = RAMP_SECONDS * TICK_RATE as f32;

    fn night_at(tick: f32) -> f32 {
        night_fraction(&clock(), tick, RAMP)
    }

    /// The client's render distance can bring the horizon in, and the server's can hold it
    /// back — and the setting is never the server's number wearing a different name.
    #[test]
    fn the_fog_stops_at_the_nearer_of_the_two_distances() {
        const CHUNK: u16 = 32;

        // A player who wants less than the server sends gets less.
        assert_eq!(fog_span(3, 8, CHUNK, 0.5), (48.0, 96.0));
        // A player who wants more than the server sends gets what arrived, because there is
        // nothing beyond it to draw.
        assert_eq!(fog_span(16, 3, CHUNK, 0.5), (48.0, 96.0));
        // And the fog control moves where the fade begins without moving the horizon.
        assert_eq!(fog_span(4, 4, CHUNK, 0.25), (32.0, 128.0));
        assert_eq!(fog_span(4, 4, CHUNK, 0.9), (115.2, 128.0));
        // One chunk still leaves `start` strictly below `end`.
        let (start, end) = fog_span(1, 1, 1, 0.5);
        assert!(start < end, "{start} is not below {end}");
    }

    #[test]
    fn weather_scales_both_ends_of_the_fog_span() {
        let span = (64.0, 128.0);
        assert_eq!(weather_fog_span(span, None), span);
        assert_eq!(
            weather_fog_span(
                span,
                Some(WeatherState {
                    kind: WeatherKind::Clear,
                    intensity: 0,
                })
            ),
            span
        );

        let rain = weather_fog_span(
            span,
            Some(WeatherState {
                kind: WeatherKind::Rain,
                intensity: 128,
            }),
        );
        let rain_scale = 1.0 - 0.4 * (128.0 / 255.0);
        assert!((rain.0 - span.0 * rain_scale).abs() < 1e-5);
        assert!((rain.1 - span.1 * rain_scale).abs() < 1e-5);

        for kind in [
            WeatherKind::Snow,
            WeatherKind::Sandstorm,
            WeatherKind::Blizzard,
        ] {
            assert_eq!(
                weather_fog_span(
                    span,
                    Some(WeatherState {
                        kind,
                        // A blizzard ignores this on purpose; the other two use full.
                        intensity: if kind == WeatherKind::Blizzard {
                            1
                        } else {
                            255
                        },
                    })
                ),
                (16.0, 32.0),
                "{kind:?} did not close the horizon to one quarter"
            );
        }
    }

    #[test]
    fn weather_tint_blends_from_the_day_night_sky_to_the_kinds_colour() {
        let same_colour = |left: Color, right: Color| {
            let (left, right) = (Srgba::from(left), Srgba::from(right));
            (left.red - right.red).abs() < 1e-6
                && (left.green - right.green).abs() < 1e-6
                && (left.blue - right.blue).abs() < 1e-6
        };
        let underneath = Color::srgb(0.11, 0.22, 0.33);
        for (kind, tint) in [
            (WeatherKind::Rain, RAIN_TINT),
            (WeatherKind::Snow, SNOW_TINT),
            (WeatherKind::Sandstorm, SAND_TINT),
        ] {
            assert_eq!(
                weather_tint(underneath, Some(WeatherState { kind, intensity: 0 })),
                underneath,
                "zero-strength {kind:?} moved the underlying sky"
            );
            assert!(
                same_colour(
                    weather_tint(
                        underneath,
                        Some(WeatherState {
                            kind,
                            intensity: 255,
                        })
                    ),
                    Color::srgb(tint[0], tint[1], tint[2])
                ),
                "full-strength {kind:?} did not reach its tint"
            );
        }

        assert!(
            same_colour(
                weather_tint(
                    underneath,
                    Some(WeatherState {
                        kind: WeatherKind::Blizzard,
                        intensity: 1,
                    })
                ),
                Color::srgb(BLIZZARD_TINT[0], BLIZZARD_TINT[1], BLIZZARD_TINT[2])
            ),
            "a blizzard is full tint whatever its intensity byte says"
        );
        assert_eq!(
            weather_tint(
                underneath,
                Some(WeatherState {
                    kind: WeatherKind::Clear,
                    intensity: 0,
                })
            ),
            underneath
        );
    }

    /// The acceptance criterion that matters most today, because it is the only path any
    /// server in this repository exercises: no clock means the four values this module
    /// exists to move do not move.
    #[test]
    fn a_world_with_no_clock_renders_exactly_the_fixed_sky() {
        let none = WorldClock::default();
        assert!(!none.declared());
        for tick in [0.0, 1.0, 12_345.0, 1e9] {
            assert_eq!(
                Daylight::at(&none, tick, TICK_RATE),
                Daylight::FIXED,
                "tick {tick} moved a sky that has no clock"
            );
        }
    }

    /// And the fixed sky is the sky this client had before the clock existed, to the exact
    /// bit — the four constants that used to live in `player/camera.rs` and
    /// `world/render.rs`.
    #[test]
    fn the_fixed_sky_is_the_one_this_client_always_had() {
        assert_eq!(Daylight::FIXED.sun_direction, Vec3::new(-0.4, -1.0, -0.25));
        assert_eq!(Daylight::FIXED.sun_illuminance, 9_000.0);
        assert_eq!(Daylight::FIXED.sky, Color::srgb(0.055, 0.070, 0.094));
        assert_eq!(Daylight::FIXED.ambient_brightness, 600.0);
        // The fifth value is new and is the same colour as the second: a world with no
        // clock has no dusk, so its rim is its zenith and its dome is flat.
        assert_eq!(Daylight::FIXED.horizon, Daylight::FIXED.sky);
    }

    // -----------------------------------------------------------------------
    // The warm horizon, and the dome that carries it
    // -----------------------------------------------------------------------

    /// The bell is zero at both ends of the night fraction and peaks in the middle of it,
    /// which is the whole of what makes the rim warm only during the ramps.
    #[test]
    fn the_dusk_bell_is_zero_at_both_ends_and_peaks_in_the_middle() {
        // Exactly zero at both ends, not nearly: `horizon == sky` at midday and midnight is
        // an equality the acceptance criterion states, and a bell that only nearly vanishes
        // would make it a tolerance.
        assert_eq!(dusk_bell(0.0), 0.0);
        assert_eq!(dusk_bell(1.0), 0.0);
        assert_eq!(dusk_bell(0.5), 1.0);
        // And outside the range it is pinned rather than folded back down.
        assert_eq!(dusk_bell(-0.5), 0.0);
        assert_eq!(dusk_bell(1.5), 0.0);
    }

    /// The same curve on both sides of the peak, which is what lets the rim inherit
    /// `dusk_and_dawn_are_mirror_images_of_each_other` instead of restating it.
    #[test]
    fn the_dusk_bell_is_mirror_symmetric_about_its_peak() {
        for step in 0..=10 {
            let night = step as f32 / 10.0;
            let (here, mirrored) = (dusk_bell(night), dusk_bell(1.0 - night));
            assert!(
                (here - mirrored).abs() < 1e-6,
                "night {night}: {here} against {mirrored}"
            );
        }
    }

    /// The horizon is the sky at both ends of the day and warmer in between — measured as
    /// the red-to-blue ratio, which is what "warmer" means as a number.
    #[test]
    fn the_horizon_is_the_sky_at_midday_and_midnight_and_warm_between_them() {
        let clock = clock();
        // 21 600 -> 14 400 the long way round: midday is 4 800, midnight 18 000.
        for tick in [4_800.0, 18_000.0] {
            let light = Daylight::at(&clock, tick, TICK_RATE);
            assert_eq!(
                light.horizon, light.sky,
                "tick {tick} gave the rim a colour of its own"
            );
        }

        // Half a ramp before night begins is where the night fraction is exactly a half.
        let dusk = Daylight::at(
            &clock,
            clock.night_start_ticks as f32 - RAMP * 0.5,
            TICK_RATE,
        );
        let midday = Daylight::at(&clock, 4_800.0, TICK_RATE);
        assert!(
            warmth(dusk.horizon) > warmth(midday.horizon),
            "the dusk rim is not warmer than the midday one"
        );
        // And at the peak it is the constant itself rather than a fraction of the way to it.
        let peak = Srgba::from(dusk.horizon);
        for (got, want) in [
            (peak.red, DUSK_HORIZON[0]),
            (peak.green, DUSK_HORIZON[1]),
            (peak.blue, DUSK_HORIZON[2]),
        ] {
            assert!((got - want).abs() < 1e-5, "{got} is not {want}");
        }
    }

    /// How warm a colour is, as the ratio the acceptance criterion names.
    fn warmth(colour: Color) -> f32 {
        let colour = Srgba::from(colour);
        colour.red / colour.blue.max(f32::MIN_POSITIVE)
    }

    /// Dusk and dawn produce the same rim, tick for mirrored tick.
    #[test]
    fn the_rim_at_dusk_is_the_rim_at_dawn() {
        let clock = clock();
        for step in 0..=10 {
            let into_ramp = RAMP * step as f32 / 10.0;
            let dusk = Daylight::at(
                &clock,
                clock.night_start_ticks as f32 - into_ramp,
                TICK_RATE,
            );
            let dawn = Daylight::at(&clock, clock.night_end_ticks as f32 + into_ramp, TICK_RATE);
            let (dusk, dawn) = (Srgba::from(dusk.horizon), Srgba::from(dawn.horizon));
            for (left, right) in [
                (dusk.red, dawn.red),
                (dusk.green, dawn.green),
                (dusk.blue, dawn.blue),
            ] {
                assert!(
                    (left - right).abs() < 1e-5,
                    "{into_ramp} ticks from the boundary: {left} against {right}"
                );
            }
        }
    }

    /// The dome runs from the sky at the zenith to the horizon at the rim, everything under
    /// the rim is the rim's colour, and nothing in between doubles back.
    #[test]
    fn the_dome_is_the_sky_at_the_top_and_the_horizon_at_the_rim() {
        let sky = Color::srgb(0.05, 0.07, 0.09);
        let horizon = Color::srgb(0.55, 0.22, 0.08);
        let colours = dome_colours(sky, horizon);
        assert_eq!(colours.len(), dome_vertex_count());

        // Red is the channel the two colours are furthest apart in, so the blend factor is
        // readable straight off it: 0 at the rim and 1 at the zenith.
        let ring = |index: usize| colours[index * (DOME_SEGMENTS + 1)];
        let towards_the_sky = |colour: [f32; 4]| {
            (colour[0] - Srgba::from(horizon).red)
                / (Srgba::from(sky).red - Srgba::from(horizon).red)
        };
        assert!(
            towards_the_sky(ring(0)).abs() > 1.0 - 1e-5,
            "the zenith is not the sky"
        );
        // `DOME_RINGS` is even, so the equator is a ring rather than a gap between two.
        let equator = DOME_RINGS / 2;
        for ring_index in equator..=DOME_RINGS {
            assert!(
                towards_the_sky(ring(ring_index)).abs() < 1e-5,
                "ring {ring_index} is at or below the rim and is not the rim's colour"
            );
        }
        for ring_index in (0..equator).rev() {
            assert!(
                towards_the_sky(ring(ring_index)) >= towards_the_sky(ring(ring_index + 1)) - 1e-6,
                "ring {ring_index} doubled back towards the rim"
            );
        }
        // And every vertex in a ring shares that ring's colour.
        for segment in 0..=DOME_SEGMENTS {
            assert_eq!(colours[segment], colours[0]);
        }
    }

    // -----------------------------------------------------------------------
    // The apparent sun, and the bodies that follow it
    // -----------------------------------------------------------------------

    /// The disc crosses the horizon where the server's night begins and ends, stands at
    /// midday's altitude at midday and at its negation in the middle of the night.
    #[test]
    fn the_apparent_sun_crosses_the_horizon_at_both_boundaries() {
        let clock = clock();
        for boundary in [clock.night_start_ticks, clock.night_end_ticks] {
            let altitude = apparent_sun_altitude(&clock, boundary as f32);
            assert!(
                altitude.abs() < 1e-3,
                "the disc stood at {altitude} degrees at tick {boundary}"
            );
        }
        // The daylight runs 21 600 -> 14 400 the long way round, which is 16 800 ticks, so
        // the sun stands highest 8 400 ticks after dawn: tick 6 000. (Several older tests
        // here sample "noon" at 4 800, which is fully daylight but is not the peak.) The
        // middle of the night is 18 000.
        assert!((apparent_sun_altitude(&clock, 6_000.0) - MIDDAY_ALTITUDE_DEGREES).abs() < 1e-3);
        assert!((apparent_sun_altitude(&clock, 18_000.0) + MIDDAY_ALTITUDE_DEGREES).abs() < 1e-3);
    }

    /// The same curve read in both directions about midday and about midnight, which is what
    /// makes the disc set the way it rose.
    #[test]
    fn the_apparent_sun_is_symmetric_about_midday_and_about_midnight() {
        let clock = clock();
        let daylight =
            (clock.day_length_ticks - clock.night_end_ticks + clock.night_start_ticks) as f32;
        let night = (clock.night_end_ticks - clock.night_start_ticks) as f32;
        for step in 0..=10 {
            let from_midday = daylight * 0.5 * step as f32 / 10.0;
            let (before, after) = (
                apparent_sun_altitude(&clock, 6_000.0 - from_midday),
                apparent_sun_altitude(&clock, 6_000.0 + from_midday),
            );
            assert!(
                (before - after).abs() < 1e-3,
                "{from_midday} ticks from midday: {before} against {after}"
            );

            let from_midnight = night * 0.5 * step as f32 / 10.0;
            let (before, after) = (
                apparent_sun_altitude(&clock, 18_000.0 - from_midnight),
                apparent_sun_altitude(&clock, 18_000.0 + from_midnight),
            );
            assert!(
                (before - after).abs() < 1e-3,
                "{from_midnight} ticks from midnight: {before} against {after}"
            );
        }
    }

    /// Nothing steps, tick zero included — and the light it shares an azimuth with is still
    /// pinned above the horizon at every one of them.
    #[test]
    fn the_apparent_sun_is_continuous_and_never_moves_the_light() {
        let clock = clock();
        let mut previous = apparent_sun_altitude(&clock, 0.0);
        for tick in 1..=clock.day_length_ticks {
            let now = apparent_sun_altitude(&clock, (tick % clock.day_length_ticks) as f32);
            // One revolution of `MIDDAY_ALTITUDE_DEGREES` a day, so a tick is a hundredth
            // of a degree at this day length; a tenth is generous.
            assert!(
                (now - previous).abs() < 0.1,
                "the disc jumped at tick {tick}: {previous} -> {now}"
            );
            previous = now;

            // The disc goes under the horizon and the light does not: the criterion the
            // whole second curve exists for.
            let direction = Daylight::at(&clock, tick as f32, TICK_RATE).sun_direction;
            assert!(direction.y <= -HORIZON_ALTITUDE_DEGREES.to_radians().sin() + 1e-4);
        }
        assert!(
            (0..clock.day_length_ticks)
                .any(|tick| apparent_sun_altitude(&clock, tick as f32) < -1.0),
            "the disc never set"
        );
    }

    /// The disc takes the light's azimuth and its own altitude, and it is drawn until its
    /// upper limb goes under the horizon.
    #[test]
    fn the_disc_sets_in_the_west_while_the_light_keeps_lighting() {
        let clock = clock();
        // A little after dusk: the disc is under the horizon, and both are due west.
        let tick = clock.night_start_ticks as f32 + 200.0;
        let disc = apparent_sun_direction(&clock, tick);
        let light = sun_position(&clock, tick);
        assert!(disc.y < 0.0, "the disc had not set: {disc:?}");
        assert!(light.y > 0.0, "the light went under the horizon: {light:?}");
        assert!(
            (disc.x.atan2(disc.z) - light.x.atan2(light.z)).abs() < 1e-4,
            "the disc and the light disagreed about which way west is"
        );
        assert!((disc.length() - 1.0).abs() < 1e-4);

        assert!(ApparentSky::above_the_horizon(0.0));
        assert!(!ApparentSky::above_the_horizon(-SKY_BODY_RADIUS_DEGREES));
        // And the placement reads an altitude back out of a direction the way it went in.
        for tick in (0..24_000).step_by(250) {
            let direction = apparent_sun_direction(&clock, tick as f32);
            assert!(
                (altitude_of(direction) - apparent_sun_altitude(&clock, tick as f32)).abs() < 1e-3
            );
        }
    }

    /// The sun and the moon are round: every rim vertex stands at exactly the radius
    /// [`SKY_BODY_RADIUS_DEGREES`] names, which a quad's corners overshoot by `sqrt(2)`.
    #[test]
    fn the_sun_is_a_disc_and_not_a_square() {
        let mesh = disc_mesh();
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("the disc's positions are three floats each");
        };
        assert_eq!(positions.len(), DISC_SEGMENTS + 1);
        let half = disc_half_width();
        for (index, vertex) in positions.iter().enumerate() {
            let radius = Vec3::from_array(*vertex).length();
            let wanted = if index == 0 { 0.0 } else { half };
            assert!(
                (radius - wanted).abs() < half * 1e-4,
                "vertex {index} stood {radius} from the centre, not {wanted}"
            );
        }
    }

    fn star_directions(mesh: &Mesh) -> Vec<Vec3> {
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("the field's positions are three floats each");
        };
        positions
            .chunks_exact(4)
            .map(|quad| {
                (quad
                    .iter()
                    .map(|corner| Vec3::from_array(*corner))
                    .sum::<Vec3>()
                    / 4.0)
                    .normalize()
            })
            .collect()
    }

    /// Every star is on the complete celestial shell, at [`SKY_BODY_DISTANCE`], and square-on
    /// to an eye at the centre — which is what lets the field be one draw that only turns.
    #[test]
    fn the_star_field_is_one_shell_of_camera_facing_quads() {
        let mesh = star_mesh();
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("the field's positions are three floats each");
        };
        assert_eq!(positions.len(), STAR_COUNT * 4);

        let largest = STAR_SIZES[2];
        let mut hemispheres = [0_usize; 2];
        for quad in positions.chunks_exact(4) {
            let corners: Vec<Vec3> = quad
                .iter()
                .map(|corner| Vec3::from_array(*corner))
                .collect();
            let centre: Vec3 = corners.iter().sum::<Vec3>() / 4.0;
            hemispheres[usize::from(centre.y >= 0.0)] += 1;
            assert!(
                (centre.length() - SKY_BODY_DISTANCE).abs() < largest,
                "a star sat {} from the eye",
                centre.length()
            );
            // Perpendicular to the line from the field's centre, so it faces an eye
            // standing there whatever the field has been turned by.
            let normal = (corners[1] - corners[0]).cross(corners[3] - corners[0]);
            assert!(
                normal.normalize().dot(centre.normalize()).abs() > 1.0 - 1e-3,
                "a star was not square-on to the eye"
            );
        }
        assert!(
            hemispheres.iter().all(|count| *count > 0),
            "the shell left one local hemisphere empty: {hemispheres:?}"
        );
    }

    /// Turning the field cannot expose the empty half-dome the old local hemisphere carried.
    /// The sectors are equal-area in height; the angular probes additionally reject a broad gap
    /// that happens to cross their boundaries instead of leaving one whole sector empty.
    #[test]
    fn every_visible_sector_stays_populated_as_the_field_turns() {
        const AZIMUTH_SECTORS: usize = 12;
        const HEIGHT_SECTORS: usize = 2;
        const MAX_PROBE_GAP_DEGREES: f32 = 20.0;

        let directions = star_directions(&star_mesh());
        for (pose, turn) in [
            ("midnight", Quat::IDENTITY),
            ("quarter turn", Quat::from_rotation_x(PI * 0.5)),
            ("half turn", Quat::from_rotation_x(PI)),
        ] {
            let visible: Vec<Vec3> = directions
                .iter()
                .map(|direction| turn * *direction)
                .filter(|direction| direction.y >= 0.0)
                .collect();
            let density_error = visible.len().abs_diff(VISIBLE_STAR_COUNT);
            assert!(
                density_error <= VISIBLE_STAR_COUNT / 10,
                "{pose} put {} stars above the horizon, outside the {} +/- 10% budget",
                visible.len(),
                VISIBLE_STAR_COUNT
            );

            let mut sectors = [0_usize; AZIMUTH_SECTORS * HEIGHT_SECTORS];
            for direction in &visible {
                let azimuth = ((direction.z.atan2(direction.x) + PI) / TAU * AZIMUTH_SECTORS as f32)
                    .floor() as usize;
                let height = (direction.y * HEIGHT_SECTORS as f32).floor() as usize;
                sectors[height.min(HEIGHT_SECTORS - 1) * AZIMUTH_SECTORS
                    + azimuth.min(AZIMUTH_SECTORS - 1)] += 1;
            }
            assert!(
                sectors.iter().all(|count| *count > 0),
                "{pose} left a visible sector empty: {sectors:?}"
            );

            let mut widest_gap = 0.0_f32;
            for height_step in 0..5 {
                let height = (height_step as f32 + 0.5) / 5.0;
                let radius = (1.0 - height * height).sqrt();
                for azimuth_step in 0..24 {
                    let azimuth = TAU * azimuth_step as f32 / 24.0;
                    let (sin, cos) = azimuth.sin_cos();
                    let probe = Vec3::new(radius * cos, height, radius * sin);
                    let nearest = visible
                        .iter()
                        .map(|direction| probe.dot(*direction).clamp(-1.0, 1.0).acos())
                        .fold(f32::INFINITY, f32::min);
                    widest_gap = widest_gap.max(nearest);
                }
            }
            assert!(
                widest_gap <= MAX_PROBE_GAP_DEGREES.to_radians(),
                "{pose} left a {:.1}-degree gap in the visible field",
                widest_gap.to_degrees()
            );
        }
    }

    /// The field is the same field every session, and it is not the world's.
    #[test]
    fn the_star_field_is_the_same_in_every_world() {
        let first = star_mesh();
        let again = star_mesh();
        assert_eq!(
            first.attribute(Mesh::ATTRIBUTE_POSITION),
            again.attribute(Mesh::ATTRIBUTE_POSITION)
        );
        assert_eq!(first.indices(), again.indices());
        // Three size classes, all of them used, and the smallest is the commonest.
        let mut counts = [0_usize; 3];
        for star in 0..STAR_COUNT as u32 {
            let size = star_size(hash_unit(star * 3 + 2));
            let class = STAR_SIZES
                .iter()
                .position(|candidate| *candidate == size)
                .expect("a star is one of the three sizes");
            counts[class] += 1;
        }
        assert!(counts.iter().all(|count| *count > 0), "{counts:?}");
        assert!(counts[0] > counts[1] && counts[1] > counts[2], "{counts:?}");
    }

    /// The whole field is overhead at the one hour it is fully visible.
    #[test]
    fn the_star_field_turns_once_a_day_and_is_overhead_at_midnight() {
        let clock = clock();
        let midnight = ApparentSky::at(&clock, 18_000.0, TICK_RATE);
        assert_eq!(midnight.night, 1.0);
        assert!(
            midnight.turn.angle_between(Quat::IDENTITY) < 1e-3,
            "the field was turned away from the eye at midnight"
        );
        let midday = ApparentSky::at(&clock, 6_000.0, TICK_RATE);
        assert_eq!(midday.night, 0.0);
        assert!(
            midday.turn.angle_between(Quat::IDENTITY) > 1.0,
            "the field had not turned by midday"
        );
        // And the moon is opposite the sun at every hour.
        for tick in (0..24_000).step_by(500) {
            let sky = ApparentSky::at(&clock, tick as f32, TICK_RATE);
            assert!((altitude_of(sky.sun) + altitude_of(-sky.sun)).abs() < 1e-3);
        }
    }

    /// A clockless world paints one flat colour, which is the sky it always had.
    #[test]
    fn a_dome_with_no_clock_is_one_flat_colour() {
        let fixed = Daylight::FIXED;
        let colours = dome_colours(fixed.sky, fixed.horizon);
        let sky = Srgba::from(fixed.sky);
        for colour in &colours {
            assert!(
                (colour[0] - sky.red).abs() < 1e-6
                    && (colour[1] - sky.green).abs() < 1e-6
                    && (colour[2] - sky.blue).abs() < 1e-6,
                "a clockless dome carried {colour:?}"
            );
        }
    }

    /// The mesh is as long as it is wide: one colour and one normal per position, and every
    /// position exactly [`SKY_BODY_DISTANCE`] from the eye it is centred on.
    ///
    /// A colour attribute out of step with its positions is the one way a gradient built
    /// from two loops can be silently wrong.
    #[test]
    fn the_dome_mesh_is_one_shell_with_a_colour_for_every_vertex() {
        let fixed = Daylight::FIXED;
        let mesh = dome_mesh(fixed.sky, fixed.horizon);
        for attribute in [Mesh::ATTRIBUTE_COLOR, Mesh::ATTRIBUTE_NORMAL] {
            assert_eq!(
                mesh.attribute(attribute)
                    .expect("the dome carries every attribute")
                    .len(),
                dome_vertex_count()
            );
        }
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("the dome's positions are three floats each");
        };
        assert_eq!(positions.len(), dome_vertex_count());
        for position in positions {
            let radius = Vec3::from_array(*position).length();
            // [`DOME_DISTANCE`] and not [`SKY_BODY_DISTANCE`], which is the whole of
            // the fix: this assertion used to pin the dome to the *same* radius as the
            // sun, the moon and the stars, so it was passing on a sky in which none of
            // the three could be drawn.
            assert!(
                (radius - DOME_DISTANCE).abs() < 1e-2,
                "a vertex sits {radius} from the eye"
            );
        }
    }

    /// Night is the interval the server named, all of it, with nothing of the ramps inside
    /// it — because the server starts spawning at `night_start_ticks` and the player has to
    /// have seen it coming by then.
    #[test]
    fn the_whole_of_the_servers_night_is_fully_night() {
        let clock = clock();
        for tick in [
            clock.night_start_ticks as f32,
            clock.night_start_ticks as f32 + 1.0,
            18_000.0,
            clock.night_end_ticks as f32 - 1.0,
        ] {
            assert_eq!(
                night_at(tick),
                1.0,
                "tick {tick} is inside the server's night"
            );
        }
    }

    /// And the middle of the day is fully day.
    #[test]
    fn the_middle_of_the_day_is_fully_day() {
        // 21 600 -> 14 400 the long way round is the daylight; its midpoint is 4 800.
        assert_eq!(night_at(4_800.0), 0.0);
        assert_eq!(night_at(0.0), 0.0);
    }

    /// The two ramps are the same length and the same shape, measured from the boundaries
    /// the server sent rather than from anything here.
    #[test]
    fn dusk_and_dawn_are_mirror_images_of_each_other() {
        let clock = clock();
        for step in 0..=10 {
            let into_ramp = RAMP * step as f32 / 10.0;
            let dusk = night_at(clock.night_start_ticks as f32 - into_ramp);
            let dawn = night_at(clock.night_end_ticks as f32 + into_ramp);
            assert!(
                (dusk - dawn).abs() < 1e-5,
                "{into_ramp} ticks from the boundary: dusk {dusk}, dawn {dawn}"
            );
            let expected = 1.0 - into_ramp / RAMP;
            assert!(
                (dusk - expected).abs() < 1e-5,
                "{into_ramp} ticks before night: {dusk}, want {expected}"
            );
        }
    }

    /// Nothing steps. Sampled every tick through a whole day, no value moves further in one
    /// tick than the ramp's own slope allows — which is what rules out a discontinuity at
    /// either boundary, at tick zero, and at the far end of each ramp.
    #[test]
    fn the_whole_curve_is_continuous_across_a_whole_day() {
        let clock = clock();
        let mut previous = Daylight::at(&clock, 0.0, TICK_RATE);
        for tick in 1..clock.day_length_ticks {
            let now = Daylight::at(&clock, tick as f32, TICK_RATE);

            // One tick is 1/RAMP of a ramp, so no ramped value may move by more than that
            // fraction of its whole range — with a little slack for the sun's arc, which is
            // not ramped and moves by a full revolution per day.
            let step = 1.0 / RAMP;
            assert!(
                (now.sun_illuminance - previous.sun_illuminance).abs()
                    <= (DAY_ILLUMINANCE - NIGHT_ILLUMINANCE) * step + 1e-3,
                "illuminance jumped at tick {tick}"
            );
            assert!(
                (now.ambient_brightness - previous.ambient_brightness).abs()
                    <= (DAY_AMBIENT_BRIGHTNESS - NIGHT_AMBIENT_BRIGHTNESS) * step + 1e-3,
                "ambient jumped at tick {tick}"
            );
            assert!(
                now.sun_direction.distance(previous.sun_direction) < 0.02,
                "the sun jumped at tick {tick}: {:?} -> {:?}",
                previous.sun_direction,
                now.sun_direction
            );
            previous = now;
        }

        // And the day closes on itself: the last tick is one step from the first.
        let first = Daylight::at(&clock, 0.0, TICK_RATE);
        let last = Daylight::at(&clock, (clock.day_length_ticks - 1) as f32, TICK_RATE);
        assert!(first.sun_direction.distance(last.sun_direction) < 0.02);
    }

    /// Midnight is darker than noon, as a number rather than by eye.
    #[test]
    fn midnight_is_darker_than_noon() {
        let clock = clock();
        let noon = Daylight::at(&clock, 4_800.0, TICK_RATE);
        let midnight = Daylight::at(&clock, 18_000.0, TICK_RATE);

        assert!(midnight.sun_illuminance < noon.sun_illuminance / 4.0);
        assert!(midnight.ambient_brightness < noon.ambient_brightness);
        let (noon_sky, midnight_sky) = (Srgba::from(noon.sky), Srgba::from(midnight.sky));
        assert!(midnight_sky.red < noon_sky.red);
        assert!(midnight_sky.green < noon_sky.green);
        assert!(midnight_sky.blue < noon_sky.blue);
    }

    /// The floor is a floor: no tick of any day lights the world less than it.
    #[test]
    fn the_ambient_never_falls_below_the_floor() {
        let clock = clock();
        for tick in 0..clock.day_length_ticks {
            let light = Daylight::at(&clock, tick as f32, TICK_RATE);
            assert!(
                light.ambient_brightness >= NIGHT_AMBIENT_BRIGHTNESS,
                "tick {tick} lit the world with {}",
                light.ambient_brightness
            );
            assert!(light.ambient_brightness <= DAY_AMBIENT_BRIGHTNESS);
            assert!(light.sun_illuminance >= NIGHT_ILLUMINANCE);
        }
    }

    /// The light never comes from below. A directional light under the horizon lights the
    /// undersides of terrain, which is the one artifact this arc exists to avoid.
    #[test]
    fn the_light_always_shines_downwards() {
        let clock = clock();
        for tick in 0..clock.day_length_ticks {
            let direction = Daylight::at(&clock, tick as f32, TICK_RATE).sun_direction;
            assert!(
                direction.y <= -HORIZON_ALTITUDE_DEGREES.to_radians().sin() + 1e-4,
                "tick {tick} put the sun at {direction:?}"
            );
            assert!((direction.length() - 1.0).abs() < 1e-4);
        }
    }

    /// The sun rises in the east and sets in the west, on the compass
    /// `player/structures.rs` mirrors from the server.
    #[test]
    fn the_sun_crosses_the_south_between_east_and_west() {
        let clock = clock();
        let dawn = sun_position(&clock, clock.night_end_ticks as f32);
        let dusk = sun_position(&clock, clock.night_start_ticks as f32);
        // 21 600 -> 14 400 the long way round: midday is 4 800.
        let midday = sun_position(&clock, 4_800.0);

        assert!(dawn.x > 0.9, "dawn should be due east, got {dawn:?}");
        assert!(dusk.x < -0.9, "dusk should be due west, got {dusk:?}");
        assert!(
            midday.z > 0.0,
            "midday should be to the south, got {midday:?}"
        );
        assert!(midday.y > dawn.y, "midday should be higher than dawn");
    }

    /// A night that ends on the last tick of the day is legal — `night_end_ticks` is
    /// compared with `<=` against the day length — so the daylight it leaves wraps past
    /// tick zero. Nothing in the curve may notice.
    #[test]
    fn a_day_that_wraps_past_tick_zero_is_still_continuous() {
        let clock = WorldClock {
            day_length_ticks: 24_000,
            night_start_ticks: 20_000,
            night_end_ticks: 24_000,
        };
        // Daylight is 0..20 000 with the night at the far end, so tick zero is the moment
        // night ends and the day rolls over.
        let mut previous = Daylight::at(&clock, 0.0, TICK_RATE);
        for tick in 1..clock.day_length_ticks {
            let now = Daylight::at(&clock, tick as f32, TICK_RATE);
            assert!(
                now.sun_direction.distance(previous.sun_direction) < 0.02,
                "the sun jumped at tick {tick}"
            );
            previous = now;
        }
        assert_eq!(night_fraction(&clock, 22_000.0, RAMP), 1.0);
        assert_eq!(night_fraction(&clock, 10_000.0, RAMP), 0.0);
    }

    /// A day so short that the two ramps would overlap still produces a curve rather than a
    /// step, because the ramp is clamped to half the daylight.
    #[test]
    fn a_day_shorter_than_two_ramps_still_has_a_curve() {
        let clock = WorldClock {
            day_length_ticks: 2_000,
            night_start_ticks: 100,
            night_end_ticks: 1_900,
        };
        // 200 ticks of daylight against a 1 200-tick ramp.
        let mut previous = night_fraction(&clock, 0.0, RAMP);
        for tick in 1..clock.day_length_ticks {
            let now = night_fraction(&clock, tick as f32, RAMP);
            assert!(
                (now - previous).abs() <= 0.02,
                "night fraction jumped at tick {tick}: {previous} -> {now}"
            );
            assert!((0.0..=1.0).contains(&now));
            previous = now;
        }
    }

    /// The clock is empty until a snapshot names a time of day.
    #[test]
    fn a_clock_with_no_snapshot_yet_has_no_answer() {
        let sky = SkyClock::default();
        assert_eq!(sky.ticks_at(Instant::now(), 20, 24_000), None);
    }

    /// Between snapshots the clock advances at the server's tick rate, not in steps.
    #[test]
    fn the_clock_advances_smoothly_between_snapshots() {
        let mut sky = SkyClock::default();
        let at = Instant::now();
        sky.anchor(1_000, at);

        assert_eq!(sky.ticks_at(at, 20, 24_000), Some(1_000.0));
        let quarter = sky
            .ticks_at(at + Duration::from_millis(25), 20, 24_000)
            .expect("anchored");
        assert!(
            (quarter - 1_000.5).abs() < 1e-3,
            "half a tick after the anchor the clock read {quarter}"
        );
        let second = sky
            .ticks_at(at + Duration::from_secs(1), 20, 24_000)
            .expect("anchored");
        assert!((second - 1_020.0).abs() < 1e-3, "one second on, {second}");
    }

    /// Re-anchoring just after midnight is continuous, not a jump backwards.
    ///
    /// The case that looks alarming and is not. By the time the server's first
    /// post-midnight snapshot arrives, this client's own extrapolation has already crossed
    /// the boundary, so the low `tick_of_day` it carries is the value the clock had reached
    /// anyway. Pinned because the obvious "guard" — refusing an anchor below the current
    /// reading — would break exactly this, once every day.
    #[test]
    fn the_clock_re_anchors_across_midnight_without_jumping() {
        let mut sky = SkyClock::default();
        let at = Instant::now();
        sky.anchor(23_990, at);

        // 1.1s on, the extrapolation has wrapped to 12 of its own accord.
        let arrival = at + Duration::from_millis(1_100);
        let before = sky.ticks_at(arrival, 20, 24_000).expect("anchored");
        assert!(
            (before - 12.0).abs() < 1e-3,
            "extrapolated past midnight to {before}"
        );

        // The snapshot the server stamped at that instant carries a low tick of day, and
        // it is **honoured**. Twenty rather than the twelve the extrapolation reached, so
        // that the assertion can tell "the anchor was taken" from "the old one happened to
        // extrapolate here anyway" — the two are indistinguishable at an equal value, which
        // is what makes the obvious version of this test pass against a broken clock.
        sky.anchor(20, arrival);
        let after = sky.ticks_at(arrival, 20, 24_000).expect("anchored");
        assert!(
            (after - 20.0).abs() < 1e-3,
            "the post-midnight anchor was ignored: the clock reads {after}, want 20"
        );
    }

    /// And it wraps at the day length rather than running off the end of it.
    #[test]
    fn the_clock_wraps_at_the_end_of_the_day() {
        let mut sky = SkyClock::default();
        let at = Instant::now();
        sky.anchor(23_990, at);
        let wrapped = sky
            .ticks_at(at + Duration::from_secs(1), 20, 24_000)
            .expect("anchored");
        assert!(
            (wrapped - 10.0).abs() < 1e-3,
            "a second past 23 990 read {wrapped}"
        );
    }

    // -----------------------------------------------------------------------
    // Under water
    // -----------------------------------------------------------------------

    /// A store holding one water-family voxel in the chunk at the origin.
    fn water_at(local: [usize; 3], size: usize, block: crate::world::BlockId) -> ChunkStore {
        let mut chunk = crate::world::VoxelChunk::all_air(size);
        chunk.set(local[0], local[1], local[2], block);
        let mut store = ChunkStore::default();
        store.insert(
            crate::net::ChunkCoord {
                cx: 0,
                cy: 0,
                cz: 0,
            },
            chunk,
        );
        store
    }

    #[test]
    fn an_eye_inside_a_voxel_of_water_is_submerged_and_one_above_it_is_not() {
        let store = water_at([2, 3, 4], 32, palette::WATER_FLOW7);

        // The voxel spans [2, 3) x [3, 4) x [4, 5), and every corner of it counts.
        for eye in [
            Vec3::new(2.0, 3.0, 4.0),
            Vec3::new(2.5, 3.5, 4.5),
            Vec3::new(2.99, 3.99, 4.99),
        ] {
            assert!(
                submerged_at(Some(&store), eye, 32),
                "{eye:?} is in the water"
            );
        }
        // One block up is the air above the surface, which is the boundary this whole
        // colour exists to make visible.
        assert!(!submerged_at(Some(&store), Vec3::new(2.5, 4.0, 4.5), 32));
        assert!(!submerged_at(Some(&store), Vec3::new(2.5, 2.99, 4.5), 32));
    }

    #[test]
    fn a_negative_eye_position_floors_rather_than_truncating() {
        // The trap `player/target.rs`'s raycast names: `-0.5 as i32` is 0 and the voxel
        // containing -0.5 is -1. Half the world is on that side of the origin.
        let mut chunk = crate::world::VoxelChunk::all_air(32);
        chunk.set(31, 31, 31, palette::WATER);
        let mut store = ChunkStore::default();
        store.insert(
            crate::net::ChunkCoord {
                cx: -1,
                cy: -1,
                cz: -1,
            },
            chunk,
        );

        assert!(submerged_at(Some(&store), Vec3::new(-0.5, -0.5, -0.5), 32));
        assert!(!submerged_at(Some(&store), Vec3::new(0.5, 0.5, 0.5), 32));
    }

    #[test]
    fn a_world_nobody_has_streamed_is_never_water() {
        // Both shapes of "nothing to be inside": no store at all, which is every frame
        // before the handshake, and no chunk there, which is the edge of the volume.
        assert!(!submerged_at(None, Vec3::new(2.5, 3.5, 4.5), 32));
        assert!(!submerged_at(
            Some(&water_at([2, 3, 4], 32, palette::WATER)),
            Vec3::new(900.0, 900.0, 900.0),
            32
        ));
    }

    #[test]
    fn an_eye_that_is_not_a_number_is_not_under_water() {
        // A `NaN` reaches `floor` as a `NaN` and the cast as a zero, which would report the
        // voxel at the origin. Refused before the cast, as the raycast does.
        let store = water_at([0, 0, 0], 32, palette::WATER);
        for eye in [
            Vec3::new(f32::NAN, 0.5, 0.5),
            Vec3::new(0.5, f32::INFINITY, 0.5),
            Vec3::splat(f32::NEG_INFINITY),
        ] {
            assert!(!submerged_at(Some(&store), eye, 32));
        }
    }

    #[test]
    fn the_underwater_fog_is_ten_blocks_and_the_sky_is_the_colour_it_fades_into() {
        // The two numbers, pinned where they are written rather than where they are used.
        assert_eq!(UNDERWATER_VISIBILITY, 10.0);
        const { assert!(UNDERWATER_START > 0.0 && UNDERWATER_START < UNDERWATER_VISIBILITY) };
        assert_eq!(
            submerged_sky(),
            Color::srgb(UNDERWATER_SKY[0], UNDERWATER_SKY[1], UNDERWATER_SKY[2])
        );
        // And nothing the clock can produce, at any hour.
        let clock = clock();
        for tick in (0..24_000).step_by(500) {
            let sky = Daylight::at(&clock, tick as f32, 20).sky;
            assert_ne!(
                sky,
                submerged_sky(),
                "the sky reaches the water colour at tick {tick}"
            );
        }
    }
}

#[cfg(test)]
mod dome_encloses_the_sky {
    use super::*;
    use bevy::render::mesh::VertexAttributeValues;

    /// The furthest any vertex of a mesh sits from its origin.
    fn furthest(mesh: &Mesh) -> f32 {
        let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("the mesh carries no float positions");
        };
        positions
            .iter()
            .map(|p| Vec3::from_array(*p).length())
            .fold(0.0_f32, f32::max)
    }

    /// **The dome must enclose every body drawn on it, and this reads the geometry rather
    /// than the constants.**
    ///
    /// The constants were right and the sky was still empty: the dome was built at
    /// [`SKY_BODY_DISTANCE`] — the same radius as the sun, the moon and the stars — so all
    /// three were coincident with it and lost the depth test. Asserting `DOME_DISTANCE >
    /// SKY_BODY_DISTANCE` would not have caught that, because the dome did not use
    /// `DOME_DISTANCE`; only the built vertices know where the dome actually is.
    #[test]
    fn every_body_is_inside_the_dome() {
        let dome = furthest(&dome_mesh(Color::WHITE, Color::BLACK));
        let stars = furthest(&star_mesh());
        let disc = furthest(&disc_mesh());

        assert!(
            stars < dome,
            "the star field reaches {stars} and the dome only {dome}; the stars are behind it"
        );
        assert!(
            disc < dome,
            "a sun or moon disc reaches {disc} and the dome only {dome}; it is behind it"
        );
        // Not merely inside: far enough inside that depth precision at this range cannot
        // put them back on the same plane. See [`DOME_DISTANCE`].
        assert!(
            dome - stars > SKY_BODY_DISTANCE * 0.1,
            "the dome clears the stars by only {}, which is inside the margin the depth \
             test needs",
            dome - stars
        );
    }
}
