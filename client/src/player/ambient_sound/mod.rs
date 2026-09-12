//! Cosmetic sounds from loaded ground, the sky clock and the server's weather.
//! Nothing here is sent or read by gameplay. No climate or biome is inferred.
mod controller;
mod sounds;

use super::{
    Weather,
    ambience::{Ambience, GroundLook},
    birds,
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
use sounds::{Bed, CALLS, Call};

/// The macaw's row in [`birds::BIRDS`], which is appended to and never reordered.
const PARROT: usize = 0;

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
    wildlife: [Calls; 5],
    wildlife_gains: [f32; 5],
    calls: Calls,
    day_gain: f32,
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
    wildlife: [f32; 5],
    day: f32,
}

fn targets(ambience: &Ambience, night: f32, weather: Option<WeatherState>) -> Targets {
    let green = f32::from(u8::from(ambience.ground == GroundLook::Grass));
    let sand = f32::from(u8::from(ambience.ground == GroundLook::Sand));
    let snow_country = f32::from(u8::from(ambience.ground == GroundLook::Snow));
    let night = night.clamp(0.0, 1.0);
    let (rain, snow) = weather.map_or((0.0, 0.0), |weather| {
        let strength = f32::from(weather.intensity) / 255.0;
        match weather.kind {
            WeatherKind::Rain => (strength, 0.0),
            WeatherKind::Snow | WeatherKind::Blizzard => (0.0, strength),
            WeatherKind::Clear | WeatherKind::Sandstorm => (0.0, 0.0),
        }
    });
    // The macaw is heard only where the bird table flies it: wooded grass. An open plain has
    // no species and another country has another one, and neither hosts the call (#1176).
    let parrot = birds::species_for(ambience) == Some(PARROT);
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
        wildlife: [
            sand * (1.0 - night),
            sand * night,
            snow_country * (1.0 - night),
            snow_country * night,
            // The same green-ground night the cricket bed was gated on, now a sparse call.
            green * night,
        ],
        day: (1.0 - night) * f32::from(u8::from(parrot)),
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
    country.day_gain +=
        (target.day - country.day_gain) * (1.0 - (-dt / controller::FADE_SECONDS).exp());
    let gain = country.day_gain;
    let parrot = Call::Parrot.profile();
    country.calls.update(
        mixer,
        CallFrame {
            dt,
            seed,
            interval: parrot.interval,
            radius: parrot.radius,
            height: parrot.height,
            origin: eye_position,
            gain,
        },
        |source| {
            let cover = spatial::occlusion(store, size, eye_position, source).max(cover);
            spatial::place(
                eye_position,
                spatial::listener_yaw(eye.rotation),
                source,
                parrot.range,
                cover,
            )
        },
        |seed, rate| Call::Parrot.bake(seed, rate),
    );
    for (index, call) in CALLS.into_iter().enumerate() {
        country.wildlife_gains[index] += (target.wildlife[index] - country.wildlife_gains[index])
            * (1.0 - (-dt / controller::FADE_SECONDS).exp());
        let gain = country.wildlife_gains[index];
        let profile = call.profile();
        country.wildlife[index].update(
            mixer,
            CallFrame {
                dt,
                // Distinct streams keep simultaneous dusk calls from sharing their bearings.
                seed: seed.wrapping_add(0x9860 + index as u64),
                interval: profile.interval,
                radius: profile.radius,
                height: profile.height,
                origin: eye_position,
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
