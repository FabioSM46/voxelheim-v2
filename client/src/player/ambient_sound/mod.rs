//! Cosmetic sounds from loaded ground, the sky clock and the server's weather.
//! Nothing here is sent or read by gameplay. No climate or biome is inferred.
mod controller;
mod sounds;
mod wildlife;

use super::{
    Weather,
    ambience::Ambience,
    birds::Bird,
    camera::{AimCamera, WorldCamera},
    sky::{self, SkyClock},
};
use crate::{
    audio::{
        AudioMixer,
        spatial::{self, Placement},
    },
    net::{Session, WeatherKind, WeatherState},
    world::ChunkStore,
};
use bevy::prelude::*;
use controller::{BedFrame, BedVoice, CallFrame, Calls};
use sounds::Bed;
use wildlife::{Habitat, Origin, WILDLIFE};

/// How many wildlife lanes there are: one per row of [`WILDLIFE`] and never a number of its
/// own, so a new species brings its lane, its gain and its target with it.
const VOICES: usize = WILDLIFE.len();

const BEDS: [Bed; 5] = [
    Bed::Rain,
    Bed::DrivingRain,
    Bed::Snowfall,
    Bed::Sandstorm,
    Bed::Blizzard,
];

#[derive(Resource, Default)]
struct Country {
    beds: [BedVoice; 5],
    wildlife: [Calls; VOICES],
    wildlife_gains: [f32; VOICES],
}

pub(super) fn register(app: &mut App) {
    app.init_resource::<Country>().add_systems(
        Update,
        update
            .after(AimCamera)
            .after(super::ambience::sample_the_ground)
            .after(super::ApplySnapshots),
    );
}

#[derive(Debug, PartialEq)]
struct Targets {
    beds: [f32; 5],
    /// One gain per row of [`WILDLIFE`], in the table's order. Nothing here knows which row
    /// is which creature — that is the point of the table.
    wildlife: [f32; VOICES],
}

fn targets(ambience: &Ambience, night: f32, weather: Option<WeatherState>) -> Targets {
    let (rain, snow) = weather.map_or((0.0, 0.0), |weather| {
        let strength = f32::from(weather.intensity) / 255.0;
        match weather.kind {
            WeatherKind::Rain => (strength, 0.0),
            WeatherKind::Snow | WeatherKind::Blizzard => (0.0, strength),
            WeatherKind::Clear | WeatherKind::Sandstorm => (0.0, 0.0),
        }
    });
    let (sand_wind, ice_wind) = weather.map_or((0.0, 0.0), |weather| {
        let strength = f32::from(weather.intensity) / 255.0;
        match weather.kind {
            WeatherKind::Sandstorm => (strength, 0.0),
            WeatherKind::Blizzard => (0.0, strength),
            _ => (0.0, 0.0),
        }
    });
    Targets {
        beds: [rain, rain * rain, snow, sand_wind, ice_wind],
        // Country × hour comes out of the table, not out of an expression per lane: each
        // row answers for its own habitat and its own half of the day, so the weather above
        // decides a bed and never a creature.
        wildlife: WILDLIFE.map(|voice| voice.gain(ambience, night)),
    }
}

#[derive(bevy::ecs::system::SystemParam)]
struct Inputs<'w, 's> {
    time: Res<'w, Time>,
    mixer: Option<Res<'w, AudioMixer>>,
    session: Option<Res<'w, Session>>,
    ambience: Res<'w, Ambience>,
    weather: Res<'w, Weather>,
    clock: Res<'w, SkyClock>,
    store: Option<Res<'w, ChunkStore>>,
    eyes: Query<'w, 's, &'static Transform, With<WorldCamera>>,
    /// Every bird drawn right now, so a voice belonging to one can be placed at it.
    ///
    /// **Read-only and filtered `Without<WorldCamera>`**: Bevy cannot prove a camera is not a
    /// bird, and this system already holds the camera's `Transform`. The same reason
    /// `birds.rs`'s own `EyeOfTheFlock` exists.
    flock: Query<'w, 's, (&'static Bird, &'static Transform), Without<WorldCamera>>,
}

fn update(input: Inputs, mut country: ResMut<Country>) {
    let (Some(mixer), Some(session), Some(eye), Some(store)) = (
        input.mixer.as_deref(),
        input.session.as_ref(),
        input.eyes.iter().next(),
        input.store.as_deref(),
    ) else {
        *country = Country::default();
        return;
    };
    if session.is_changed() {
        *country = Country::default();
    }
    let dt = input.time.delta_secs().min(0.25);
    let target = targets(
        &input.ambience,
        sky::night_now(&input.clock, session).unwrap_or(0.0),
        input.weather.get(),
    );
    let eye_position = eye.translation;
    let size = usize::from(session.0.chunk_size);
    // The sky-facing three-ray probe reuses the voice material weights. It catches
    // a roof/cave ceiling without needing a biome, shelter flag or server change.
    let cover = spatial::occlusion(store, size, eye_position, eye_position + Vec3::Y * 32.0);
    let placement = Placement {
        occlusion: cover,
        ..Placement::UNPOSITIONED
    };
    let seed = session.0.world_seed as u64;
    for (index, (voice, bed)) in country.beds.iter_mut().zip(BEDS).enumerate() {
        voice.update(
            mixer,
            BedFrame {
                dt,
                gain: target.beds[index],
                placement,
                seed: seed.wrapping_add(index as u64),
            },
            || bed.description(),
        );
    }
    // One lane per row of the table, the macaw's included: it had a lane of its own until a
    // habitat could be something other than the ground, and folding it in changed no seed.
    for (index, voice) in WILDLIFE.iter().enumerate() {
        country.wildlife_gains[index] += (target.wildlife[index] - country.wildlife_gains[index])
            * (1.0 - (-dt / controller::FADE_SECONDS).exp());
        let gain = country.wildlife_gains[index];
        let call = voice.call;
        let profile = call.profile();
        // Where this voice comes from — the rule `wildlife.rs` writes down, applied. A row
        // that declares `Origin::Creature` is placed at the nearest drawn body of the flock it
        // names; with no body to place it at, it falls back to the bearing, because a voice
        // with no body is still ambience and silence is not the fallback.
        //
        // The radius and height are **zeroed** with the body, not kept: `Calls::update` adds
        // them to the origin to make a bearing, and a hoot eleven blocks from the owl is the
        // very thing placing it at the owl was for.
        let body = match (voice.origin, voice.habitat) {
            (Origin::Creature, Habitat::Flock(rows)) => {
                nearest_body(&input.flock, rows, eye_position)
            }
            _ => None,
        };
        let (origin, radius, height) = match body {
            Some(at) => (at, 0.0, 0.0),
            None => (eye_position, profile.radius, profile.height),
        };
        country.wildlife[index].update(
            mixer,
            CallFrame {
                dt,
                // Distinct streams keep simultaneous dusk calls from sharing their bearings.
                // The salt is the row's, so the table may be appended to or reordered
                // without moving a call that ships today.
                seed: seed.wrapping_add(voice.stream),
                interval: profile.interval,
                radius,
                height,
                origin,
                gain,
            },
            |source| {
                let cover = spatial::occlusion(store, size, eye_position, source).max(cover);
                spatial::place(
                    eye_position,
                    spatial::listener_yaw(eye.rotation),
                    source,
                    profile.range,
                    cover,
                )
            },
            |seed, rate| call.bake(seed, rate),
        );
    }
}

/// The drawn body of `rows` nearest the eye, if any is drawn at all.
///
/// **Nearest rather than first**, because a flock is several birds and the one a player is
/// looking at is the one whose voice has to come from the right place. With two owls in the
/// wood the far one's hoot arriving from the near one is a smaller error than a hoot on a
/// bearing, but it is still an error, and picking the nearest costs one comparison a bird.
///
/// It reads the transform rather than recomputing `birds::place`, so a perched owl's hoot
/// comes from the branch it is actually drawn on — the clamp and the perch both already
/// applied — rather than from the circuit it would have been flying.
fn nearest_body(
    flock: &Query<(&Bird, &Transform), Without<WorldCamera>>,
    rows: &[usize],
    eye: Vec3,
) -> Option<Vec3> {
    flock
        .iter()
        .filter(|(bird, _)| rows.contains(&bird.species))
        .map(|(_, at)| at.translation)
        .filter(|at| at.is_finite())
        .min_by(|a, b| a.distance_squared(eye).total_cmp(&b.distance_squared(eye)))
}

#[cfg(test)]
mod pins;

#[cfg(test)]
mod tests;
