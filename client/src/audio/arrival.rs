//! One proof of synthesis: a short entry chime at the server's announced spawn point.
//! The welcome authorizes entry; this module merely gives it a sound, once per session.

use super::{
    AudioMixer, Bus, spatial,
    synth::{Baked, Envelope, Exciter, Layer, Playback, Rendering, Sound, Status, Wave},
};
use crate::{
    net::Session,
    player::{EYE_HEIGHT, WorldCamera},
};
use bevy::prelude::*;

#[derive(Resource, Default)]
struct Arrival {
    seen: Option<u64>,
    cache: Option<Baked>,
    playing: Option<Playback>,
    origin: Vec3,
}

pub(super) fn register(app: &mut App) {
    app.init_resource::<Arrival>()
        .add_systems(Update, play_arrival);
}

fn description() -> Sound {
    // Two decaying partials: a soft, small bell. This is the only sound catalogue entry in
    // #983; later issues add their own descriptions and use the same playback machinery.
    Sound {
        layers: [(660.0, 0.22), (990.0, 0.09)]
            .into_iter()
            .map(|(hz, gain)| Layer {
                exciter: Exciter::Oscillator {
                    wave: Wave::Sine,
                    hz,
                },
                gain,
                envelope: Envelope {
                    attack: 0.005,
                    decay: 0.28,
                    sustain: 0.0,
                    release: 0.02,
                },
                filter: None,
            })
            .collect(),
    }
}

fn play_arrival(
    mut arrival: ResMut<Arrival>,
    mixer: Res<AudioMixer>,
    session: Option<Res<Session>>,
    eyes: Query<&Transform, With<WorldCamera>>,
) {
    let Some(session) = session else {
        arrival.seen = None;
        arrival.playing = None;
        return;
    };
    // Resource replacement may reuse an entity id; a new welcome still deserves one chime.
    if session.is_changed() {
        arrival.seen = None;
        arrival.playing = None;
    }
    let Some(eye) = eyes.iter().next() else {
        return;
    };
    if arrival.seen != Some(session.0.entity_id) {
        arrival.seen = Some(session.0.entity_id);
        arrival.origin = Vec3::from_array(session.0.spawn) + Vec3::Y * EYE_HEIGHT;
        let rate = mixer.0.sample_rate();
        if arrival
            .cache
            .as_ref()
            .is_none_or(|baked| baked.sample_rate() != rate)
        {
            arrival.cache = description().bake(0.32, rate, 0).ok();
        }
        arrival.playing = arrival.cache.as_ref().and_then(|baked| {
            Playback::start(
                &mixer,
                Bus::Sfx,
                Rendering::Baked(baked.clone()),
                spatial::place(
                    eye.translation,
                    spatial::listener_yaw(eye.rotation),
                    arrival.origin,
                    16.0,
                    0.0,
                ),
            )
            .ok()
        });
    }
    let origin = arrival.origin;
    if let Some(playing) = arrival.playing.as_mut() {
        playing.place(spatial::place(
            eye.translation,
            spatial::listener_yaw(eye.rotation),
            origin,
            16.0,
            0.0,
        ));
        if playing.pump() != Status::Playing {
            arrival.playing = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{Mixer, mixer::Sink};
    use crate::net::SessionParams;
    use std::sync::Arc;
    struct Buffer(Vec<f32>);
    impl Sink for Buffer {
        fn block(&mut self) -> &mut [f32] {
            &mut self.0
        }
    }
    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 1,
            spawn: [0.0; 3],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 8,
            inventory_slots: 37,
            hotbar_slots: 9,
            equipment_slots: 4,
            player_token: crate::net::ANY_TOKEN,
            voice_range_blocks: 32.0,
        })
    }
    fn fixture() -> (App, Arc<Mixer>) {
        let mixer = Arc::new(Mixer::new());
        mixer.set_format(48_000, 2);
        let mut app = App::new();
        app.insert_resource(AudioMixer(Arc::clone(&mixer)));
        register(&mut app);
        app.world_mut()
            .spawn((WorldCamera, Transform::from_xyz(0.0, EYE_HEIGHT, 0.0)));
        (app, mixer)
    }
    fn hear(app: &mut App, mixer: &Mixer) -> f32 {
        let mut energy = 0.0;
        for _ in 0..40 {
            app.update();
            let mut sink = Buffer(vec![0.0; 960]);
            mixer.render(&mut sink);
            energy += sink.0.iter().map(|v| v * v).sum::<f32>();
        }
        energy
    }
    #[test]
    fn a_welcome_plays_one_chime_and_disconnect_then_reconnect_plays_another() {
        let (mut app, mixer) = fixture();
        assert_eq!(hear(&mut app, &mixer), 0.0);
        app.insert_resource(session());
        assert!(hear(&mut app, &mixer) > 1.0);
        assert_eq!(hear(&mut app, &mixer), 0.0);
        app.world_mut().remove_resource::<Session>();
        app.update();
        app.insert_resource(session());
        assert!(hear(&mut app, &mixer) > 1.0);
    }
    #[test]
    fn the_real_chime_respects_sfx_mute_and_rebakes_for_a_different_device() {
        let (mut app, mixer) = fixture();
        mixer.set_gain(Bus::Sfx, 0.0);
        app.insert_resource(session());
        assert_eq!(hear(&mut app, &mixer), 0.0);
        app.world_mut().remove_resource::<Session>();
        app.update();
        mixer.set_format(44_100, 2);
        mixer.set_gain(Bus::Sfx, 1.0);
        app.insert_resource(session());
        assert!(hear(&mut app, &mixer) > 1.0);
        assert_eq!(
            app.world()
                .resource::<Arrival>()
                .cache
                .as_ref()
                .unwrap()
                .sample_rate(),
            44_100
        );
    }
}
