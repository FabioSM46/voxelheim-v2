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

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use super::camera::WorldCamera;
use crate::net::{BlockCoord, Session, WorldClock};
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

/// Marks the one directional light this module owns.
#[derive(Component)]
pub struct Sun;

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
    fn ticks_at(&self, now: Instant, tick_rate: u8, day_length_ticks: u32) -> Option<f32> {
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
    /// The camera's clear colour, and the colour distance fades into.
    pub sky: Color,
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

        Self {
            sun_direction: -sun_position(clock, tick_of_day),
            sun_illuminance: lerp(DAY_ILLUMINANCE, NIGHT_ILLUMINANCE, night),
            sky: Color::srgb(
                lerp(DAY_SKY[0], NIGHT_SKY[0], night),
                lerp(DAY_SKY[1], NIGHT_SKY[1], night),
                lerp(DAY_SKY[2], NIGHT_SKY[2], night),
            ),
            ambient_brightness: lerp(DAY_AMBIENT_BRIGHTNESS, NIGHT_AMBIENT_BRIGHTNESS, night),
        }
    }
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

    // Half a revolution across the daylight and half across the night, so the two pieces
    // meet at the boundaries and the whole is one continuous sweep.
    let phase = if since_dawn <= daylight {
        0.5 * since_dawn / daylight
    } else {
        0.5 + 0.5 * (since_dawn - daylight) / night_length
    };
    let azimuth = TAU * phase;

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

/// Everything the sky is computed from, and nothing it writes.
///
/// One `SystemParam` rather than four resources threaded through a signature, for the reason
/// [`super::camera::Aim`] is one: it names "the state the light is a function of" and it keeps
/// [`drive_the_sky`] inside the argument budget the store took it past.
#[derive(SystemParam)]
pub(super) struct SkyInputs<'w> {
    session: Option<Res<'w, Session>>,
    clock: Res<'w, SkyClock>,
    /// Read for exactly one question — is the eye inside a voxel of water — and read
    /// only. This is the fourth edge from `player` to `world`, and `client/AGENTS.md`
    /// enumerates it beside the other three.
    store: Option<Res<'w, ChunkStore>>,
    settings: Option<Res<'w, Settings>>,
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
    mut announced: Local<bool>,
    mut was_submerged: Local<bool>,
) {
    let SkyInputs {
        session,
        clock,
        store,
        settings,
    } = read;
    let Some(session) = session else {
        return;
    };
    let params = session.0;
    let declared = params.clock.declared();

    if !*announced {
        *announced = true;
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
    let light = if declared {
        match clock.ticks_at(
            Instant::now(),
            params.tick_rate,
            params.clock.day_length_ticks,
        ) {
            Some(tick_of_day) => Daylight::at(&params.clock, tick_of_day, params.tick_rate),
            // No snapshot has named a time of day yet. Unreachable a tick after the welcome.
            None => Daylight::FIXED,
        }
    } else {
        Daylight::FIXED
    };

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
    let (start, end) = fog_span(
        chosen_distance,
        params.view_distance,
        params.chunk_size,
        fog_start,
    );
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
    let (sky, start, end) = if submerged {
        (submerged_sky(), UNDERWATER_START, UNDERWATER_VISIBILITY)
    } else {
        (light.sky, start, end)
    };

    for (entity, mut camera, mut ambient, fog) in &mut cameras {
        // Written on the frame the player goes under and on the frame they come back
        // up, whatever the clock says — a server with no time of day still has to stop
        // showing the underwater sky once the eye leaves the water. `*was_submerged` is
        // what carries the second of those: `ClearColorConfig` has no `PartialEq` to
        // compare against, so the restoring write is triggered by the transition rather
        // than by a difference.
        if declared || submerged || *was_submerged {
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
                if fog.color != sky || !fades_between(&fog.falloff, start, end) {
                    fog.color = sky;
                    fog.falloff = FogFalloff::Linear { start, end };
                }
            }
            None => {
                commands.entity(entity).insert(DistanceFog {
                    color: sky,
                    falloff: FogFalloff::Linear { start, end },
                    ..default()
                });
            }
        }
    }

    *was_submerged = submerged;
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
fn submerged_at(store: Option<&ChunkStore>, eye: Vec3, chunk_size: usize) -> bool {
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
    store.block_at(pos, chunk_size) == palette::WATER
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

    /// A store holding one voxel of water in the chunk at the origin.
    fn water_at(local: [usize; 3], size: usize) -> ChunkStore {
        let mut chunk = crate::world::VoxelChunk::all_air(size);
        chunk.set(local[0], local[1], local[2], palette::WATER);
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
        let store = water_at([2, 3, 4], 32);

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
            Some(&water_at([2, 3, 4], 32)),
            Vec3::new(900.0, 900.0, 900.0),
            32
        ));
    }

    #[test]
    fn an_eye_that_is_not_a_number_is_not_under_water() {
        // A `NaN` reaches `floor` as a `NaN` and the cast as a zero, which would report the
        // voxel at the origin. Refused before the cast, as the raycast does.
        let store = water_at([0, 0, 0], 32);
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
