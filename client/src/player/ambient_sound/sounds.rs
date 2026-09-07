//! Small descriptions rather than assets: every continuous layer advances fresh noise.
use crate::audio::synth::{Envelope, Exciter, Filter, FilterKind, Layer, Noise, Sound, Wave};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Bed {
    Crickets,
    Rain,
    DrivingRain,
    Snowfall,
}

fn noise(noise: Noise, gain: f32, kind: FilterKind, hz: f32, q: f32) -> Layer {
    Layer {
        exciter: Exciter::Noise(noise),
        gain,
        envelope: Envelope {
            attack: 0.5,
            decay: 0.0,
            sustain: 1.0,
            release: 0.8,
        },
        filter: Some(Filter { kind, hz, q }),
    }
}

impl Bed {
    pub(super) fn description(self) -> Sound {
        // All bands remain below the lowest supported sample rate's Nyquist margin.
        // Rain gains a second, lower wash as intensity grows: more than louder drizzle.
        let layers = match self {
            Self::Crickets => vec![noise(Noise::White, 0.24, FilterKind::Band, 3200.0, 8.0)],
            Self::Rain => vec![noise(Noise::White, 0.18, FilterKind::High, 1800.0, 0.7)],
            Self::DrivingRain => vec![noise(Noise::White, 0.3, FilterKind::Low, 950.0, 0.7)],
            // A soft granular hush, not the bright white-noise streaks of rainfall.
            Self::Snowfall => vec![noise(Noise::Brown, 0.12, FilterKind::Band, 380.0, 0.7)],
        };
        Sound { layers }
    }
}

/// The macaw already drawn over wooded grass: a short rough pair of partials,
/// varied by the caller's seed. Open grass can hear the same species off-screen.
pub(super) fn parrot(seed: u64) -> Sound {
    let hz = 1150.0 + (seed % 401) as f32;
    Sound {
        layers: [(hz, 0.16), (hz * 1.7, 0.07)]
            .into_iter()
            .map(|(hz, gain)| Layer {
                exciter: Exciter::Oscillator {
                    wave: Wave::Sine,
                    hz,
                },
                gain,
                envelope: Envelope {
                    attack: 0.015,
                    decay: 0.11,
                    sustain: 0.0,
                    release: 0.04,
                },
                filter: None,
            })
            .chain(std::iter::once(Layer {
                envelope: Envelope {
                    attack: 0.01,
                    decay: 0.14,
                    sustain: 0.0,
                    release: 0.04,
                },
                ..noise(Noise::White, 0.11, FilterKind::Band, 1600.0, 1.5)
            }))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn stream(bed: Bed, rate: u32, seed: u64) -> Vec<f32> {
        let mut stream = bed.description().continuous(rate, seed).unwrap();
        let mut output = vec![0.0; rate as usize * 2];
        assert_eq!(stream.render(&mut output), output.len());
        output
    }
    #[test]
    fn every_bed_is_finite_audible_and_fresh_at_supported_rates() {
        for bed in [Bed::Crickets, Bed::Rain, Bed::DrivingRain, Bed::Snowfall] {
            for rate in [8000, 44100, 48000, 192000] {
                let samples = stream(bed, rate, 17);
                assert!(samples.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
                let second = &samples[rate as usize..];
                assert!(second.iter().any(|v| v.abs() > 0.001));
                assert_ne!(&second[..second.len() / 2], &second[second.len() / 2..]);
            }
        }
    }
    #[test]
    fn weather_and_insects_have_distinct_spectra() {
        let bright = |bed| {
            let v = stream(bed, 48000, 11);
            v.windows(2).map(|p| (p[1] - p[0]).powi(2)).sum::<f32>()
                / v.iter().map(|v| v * v).sum::<f32>()
        };
        assert!(bright(Bed::Rain) > bright(Bed::DrivingRain) * 3.0);
        assert!(bright(Bed::Rain) > bright(Bed::Snowfall) * 10.0);
        assert_ne!(stream(Bed::Crickets, 8000, 2), stream(Bed::Rain, 8000, 2));
    }
    #[test]
    fn descriptions_are_seeded_and_calls_have_silent_edges() {
        assert_eq!(stream(Bed::Rain, 8000, 2), stream(Bed::Rain, 8000, 2));
        assert_ne!(stream(Bed::Rain, 8000, 2), stream(Bed::Rain, 8000, 3));
        for rate in [8000, 48000, 192000] {
            let call = parrot(7).bake(0.3, rate, 7).unwrap();
            assert_eq!(call.samples()[0], 0.0);
            assert_eq!(*call.samples().last().unwrap(), 0.0);
            assert!(call.samples().iter().any(|v| v.abs() > 0.01));
            assert_ne!(
                call.samples(),
                parrot(11).bake(0.3, rate, 11).unwrap().samples()
            );
        }
    }
}
