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
use sounds::CallProfile;
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
    /// Every critter drawn right now, for the same half of the origin rule. Read-only and by
    /// row, so this lane knows *which species is where* without knowing anything else about
    /// one — it holds no opinion about where a critter lives, which is `critters::CRITTERS`'s
    /// to answer and `Habitat::Critter`'s to ask (#1176 is what a second opinion costs).
    critters: Query<'w, 's, (&'static Critter, &'static Transform), Without<WorldCamera>>,
}

/// Where one voice is heard from this frame: its creature's body when the row declares
/// [`Origin::Creature`] and one is drawn, and the row's bearing circle otherwise.
///
/// Named and separate so the rule can be tested rather than only read. A body collapses the
/// circle to nothing — radius and height both zero — because the sound is *at* the animal, not
/// on a ring around the listener; leaving either non-zero would scatter a visible creature's
/// voice away from it. Raised in review on #1221, where the lane's own arms were exercised by
/// nothing and a swapped pair would have passed every test.
///
/// **Both kinds of visible creature answer here.** A flock row names bird rows and a critter
/// row names a critter row, and #1190 and #1191 arrived with one each; keeping two placement
/// paths would have been two chances to forget to zero the circle. The fallback is the bearing
/// rather than silence, which is the origin rule's own third bullet.
fn voice_placement(
    origin: Origin,
    habitat: Habitat,
    flock: &[(usize, Vec3)],
    critters: &[(usize, Vec3)],
    eye_position: Vec3,
    profile: &CallProfile,
) -> (Vec3, f32, f32) {
    // `Origin` decides whether to ask at all; `Habitat::body` decides where, from the list
    // that draws that kind of creature. A `Bearing` row never asks, which is what keeps the
    // macaw's shipped placement exactly where it was.
    let body = match origin {
        Origin::Creature => habitat.body(flock, critters, eye_position),
        Origin::Bearing => None,
    };
    match body {
        Some(at) => (at, 0.0, 0.0),
        None => (eye_position, profile.radius, profile.height),
    }
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
    // The same, for birds. Read from the transform rather than by recomputing `birds::place`,
    // so a perched owl's hoot comes from the branch it is actually drawn on — clamp and perch
    // already applied — rather than from the circuit it would have been flying.
    let flock_drawn: Vec<(usize, Vec3)> = input
        .flock
        .iter()
        .map(|(bird, at)| (bird.species, at.translation))
        .collect();
    for (index, voice) in WILDLIFE.iter().enumerate() {
        country.wildlife_gains[index] += (target.wildlife[index] - country.wildlife_gains[index])
            * (1.0 - (-dt / controller::FADE_SECONDS).exp());
        let gain = country.wildlife_gains[index];
        let call = voice.call;
        let profile = call.profile();
        // **The origin rule, and the whole of where it is applied.** A row that declares
        // `Origin::Creature` is placed at the nearest drawn body of the creature it names, so
        // the sound moves when it moves and is occluded by what stands between; every other row
        // keeps its bearing at the row's radius and height. Which of the two applies is a
        // property of the creature rather than of the frame, and silence is not an option.
        //
        // The radius and height are **zeroed** with the body, not kept: `Calls::update` adds
        // them to the origin to make a bearing, and a hoot eleven blocks from the owl is the
        // very thing placing it at the owl was for.
        let (origin, radius, height) = voice_placement(
            voice.origin,
            voice.habitat,
            &flock_drawn,
            &drawn,
            eye_position,
            &profile,
        );
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
#[cfg(test)]
mod pins;

#[cfg(test)]
mod tests;
