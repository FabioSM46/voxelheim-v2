//! Bounded reusable lanes and sparse seeded calls. Content selects gains; these own voices.
use crate::audio::{
    AudioMixer, Bus,
    spatial::Placement,
    synth::{Playback, Rendering, Sound, Status},
};
use bevy::prelude::*;

pub(super) struct BedFrame {
    pub dt: f32,
    pub gain: f32,
    pub placement: Placement,
    pub seed: u64,
}

pub(super) struct CallFrame {
    pub dt: f32,
    pub seed: u64,
    pub interval: [f32; 2],
    pub radius: f32,
    pub height: f32,
    pub seconds: f32,
    pub origin: Vec3,
    pub gain: f32,
}

pub(super) const FADE_SECONDS: f32 = 2.0;
const RETRY_SECONDS: f32 = 1.0;
const SILENT: f32 = 0.0001;

#[derive(Default)]
pub(super) struct BedVoice {
    playing: Option<Playback>,
    pub(super) gain: f32,
    retry: f32,
}

impl BedVoice {
    /// The description is constructed only when a voice is admitted or its device changes.
    /// A revoked bed backs off; it cannot displace a source of the same or higher priority.
    pub(super) fn update(
        &mut self,
        mixer: &AudioMixer,
        frame: BedFrame,
        describe: impl FnOnce() -> Sound,
    ) {
        let BedFrame {
            dt,
            gain: target,
            mut placement,
            seed,
        } = frame;
        self.gain += (target - self.gain) * (1.0 - (-dt / FADE_SECONDS).exp());
        self.retry = (self.retry - dt).max(0.0);
        if target == 0.0 && self.gain < SILENT {
            self.playing = None;
            self.gain = 0.0;
            return;
        }
        placement.gain *= self.gain;
        if self.playing.is_none() && target > 0.0 && self.retry == 0.0 {
            self.retry = RETRY_SECONDS;
            self.playing = describe()
                .continuous(mixer.sample_rate(), seed)
                .ok()
                .and_then(|sound| {
                    Playback::start(
                        mixer,
                        Bus::Ambience,
                        Rendering::Continuous(sound),
                        placement,
                    )
                    .ok()
                });
        }
        if let Some(playing) = &mut self.playing {
            playing.place(placement);
            if playing.pump() != Status::Playing {
                self.playing = None;
                self.retry = RETRY_SECONDS;
                self.gain = 0.0;
            }
        }
    }
}

/// SplitMix-style deterministic scramble; presentation seed, never a biome or game state.
pub(super) fn scramble(mut seed: u64) -> u64 {
    seed = (seed ^ (seed >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    seed = (seed ^ (seed >> 27)).wrapping_mul(0x94d049bb133111eb);
    seed ^ (seed >> 31)
}

#[derive(Default)]
pub(super) struct Calls {
    remaining: f32,
    sequence: u64,
    playing: Option<(Playback, Vec3)>,
}

impl Calls {
    /// At most one call per frame, no catch-up burst after a stall. The position stays
    /// anchored for its short lifetime. Rate/device changes drop the old one-shot.
    pub(super) fn update(
        &mut self,
        mixer: &AudioMixer,
        frame: CallFrame,
        place: impl Fn(Vec3) -> Placement,
        describe: impl FnOnce(u64) -> Sound,
    ) {
        let CallFrame {
            dt,
            seed,
            interval,
            radius,
            height,
            seconds,
            origin,
            gain,
        } = frame;
        self.remaining = (self.remaining - dt).max(0.0);
        if gain <= 0.0001 {
            self.remaining = interval[0];
        } else if self.remaining == 0.0 && self.playing.is_none() {
            self.sequence = self.sequence.wrapping_add(1);
            let seed = scramble(seed.wrapping_add(self.sequence));
            let unit = (seed >> 40) as f32 / (1u32 << 24) as f32;
            self.remaining = interval[0] + unit * (interval[1] - interval[0]);
            let angle = unit * std::f32::consts::TAU;
            let source = origin + Vec3::new(angle.cos() * radius, height, angle.sin() * radius);
            let mut placement = place(source);
            placement.gain *= gain;
            self.playing = describe(seed)
                .bake(seconds, mixer.sample_rate(), seed)
                .ok()
                .and_then(|sound| {
                    Playback::start(mixer, Bus::Ambience, Rendering::Baked(sound), placement).ok()
                })
                .map(|voice| (voice, source));
        }
        if let Some((playing, source)) = &mut self.playing {
            let mut placement = place(*source);
            placement.gain *= gain;
            playing.place(placement);
            if playing.pump() != Status::Playing {
                self.playing = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::sounds;
    use super::*;
    use crate::audio::{Mixer, Sink};
    use std::sync::Arc;
    struct Buffer(Vec<f32>);
    impl Sink for Buffer {
        fn block(&mut self) -> &mut [f32] {
            &mut self.0
        }
    }

    fn calls(seed: u64, gain: f32) -> Vec<f32> {
        let shared = Arc::new(Mixer::new());
        shared.set_format(8000, 1);
        let mixer = AudioMixer::from_shared_for_test(shared.clone());
        let mut calls = Calls::default();
        let mut output = Vec::new();
        for _ in 0..100 {
            calls.update(
                &mixer,
                CallFrame {
                    dt: 0.1,
                    seed,
                    interval: [0.7, 2.5],
                    radius: 7.0,
                    height: 5.0,
                    seconds: 0.3,
                    origin: Vec3::ZERO,
                    gain,
                },
                |_| Placement::UNPOSITIONED,
                sounds::parrot,
            );
            let mut buffer = Buffer(vec![0.0; 800]);
            shared.render(&mut buffer);
            output.extend(buffer.0);
        }
        output
    }
    #[test]
    fn sparse_calls_are_reproducible_varied_and_gated() {
        let first = calls(11, 1.0);
        assert_eq!(first, calls(11, 1.0));
        assert_ne!(first, calls(12, 1.0));
        assert!(first.iter().any(|s| s.abs() > 0.01));
        let silent_blocks = first
            .chunks(800)
            .filter(|block| block.iter().all(|s| *s == 0.0))
            .count();
        assert!(silent_blocks > 50, "calls leave more silence than sound");
        assert!(calls(11, 0.0).iter().all(|s| *s == 0.0));
    }
}
