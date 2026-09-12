//! Cosmetic sounds from loaded ground, the sky clock and the server's weather.
//! Nothing here is sent or read by gameplay. No climate or biome is inferred.
mod controller;
mod sounds;
mod wildlife;

use super::{
    Weather,
    ambience::Ambience,
    camera::{AimCamera, WorldCamera},
    critters::Critter,
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
use wildlife::WILDLIFE;

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
    /// Every critter drawn right now, for the half of the origin rule that places a voice at
    /// the creature it belongs to. Read-only and by row, so this lane knows *which species is
    /// where* without knowing anything else about one — it holds no opinion about where a
    /// critter lives, which is `critters::CRITTERS`'s to answer and `Habitat::Critter`'s to
    /// ask (#1176 is what a second opinion costs).
    critters: Query<'w, 's, (&'static Critter, &'static Transform), Without<WorldCamera>>,
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
    // Where each species is drawn, gathered once rather than per lane. Empty when nothing is
    // on the ground, which is the fallback branch the origin rule names: a voice with no body
    // keeps its bearing, because silence is never the fallback.
    let drawn: Vec<(usize, Vec3)> = input
        .critters
        .iter()
        .map(|(critter, at)| (critter.species, at.translation))
        .collect();
    for (index, voice) in WILDLIFE.iter().enumerate() {
        country.wildlife_gains[index] += (target.wildlife[index] - country.wildlife_gains[index])
            * (1.0 - (-dt / controller::FADE_SECONDS).exp());
        let gain = country.wildlife_gains[index];
        let call = voice.call;
        let profile = call.profile();
        // **The origin rule, and the whole of where it is applied.** A voice whose creature is
        // drawn is placed *at that creature*: the origin is its body and the bearing circle
        // collapses to nothing, so the sound moves when it moves and is occluded by what
        // stands between. A voice with no body keeps the row's bearing at the row's radius and
        // height. Which of the two applies is a property of the creature rather than of the
        // frame — `Habitat::body` answers for the habitat and never looks at the clock — and
        // silence is not one of the options.
        let (origin, radius, height) = match voice.habitat.body(&drawn, eye_position) {
            Some(body) => (body, 0.0, 0.0),
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

#[cfg(test)]
mod pins;

#[cfg(test)]
mod tests;
