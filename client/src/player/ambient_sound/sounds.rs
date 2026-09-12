//! Small descriptions rather than assets: every continuous layer advances fresh noise.
use crate::audio::synth::{Envelope, Exciter, Filter, FilterKind, Layer, Noise, Sound, Wave};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Bed {
    Rain,
    DrivingRain,
    Snowfall,
    Sandstorm,
    Blizzard,
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
            Self::Rain => vec![noise(Noise::White, 0.18, FilterKind::High, 1800.0, 0.7)],
            Self::DrivingRain => vec![noise(Noise::White, 0.3, FilterKind::Low, 950.0, 0.7)],
            // A soft granular hush, not the bright white-noise streaks of rainfall.
            Self::Snowfall => vec![noise(Noise::Brown, 0.12, FilterKind::Band, 380.0, 0.7)],
            // Sandy grit over a low wind; the blizzard is colder, narrower and higher.
            Self::Sandstorm => vec![
                noise(Noise::Brown, 0.6, FilterKind::Low, 550.0, 0.7),
                noise(Noise::White, 0.16, FilterKind::Band, 1300.0, 0.8),
            ],
            Self::Blizzard => vec![
                noise(Noise::White, 0.23, FilterKind::Band, 750.0, 2.8),
                noise(Noise::Brown, 0.32, FilterKind::Low, 230.0, 0.7),
            ],
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

/// Off-screen presentation, never a creature. Snow's daytime bird is the eagle
/// already selected by birds::species_for; these descriptions spawn no entities.
#[derive(Clone, Copy, Debug)]
pub(super) enum Call {
    Rattlesnake,
    Crow,
    Eagle,
    Wolf,
    /// Green country at night: an occasional cricket, not a continuous wall of them.
    Cricket,
}

pub(super) const CALLS: [Call; 5] = [
    Call::Rattlesnake,
    Call::Crow,
    Call::Eagle,
    Call::Wolf,
    Call::Cricket,
];

/// Content parameters for the existing Calls lane. Intervals exceed each sound's
/// duration by a wide margin; even dusk leaves the desert mostly silent.
pub(super) struct CallProfile {
    pub interval: [f32; 2],
    pub radius: f32,
    pub height: f32,
    pub seconds: f32,
    pub range: f32,
}

/// Pulses per second within one cricket trill, 26 to 34, from its seed. Steady for the whole
/// call: a trill is recognised by its rate, and a cricket does not drift inside one.
pub(super) fn pulse_rate(seed: u64) -> f32 {
    26.0 + ((seed >> 16) % 81) as f32 / 10.0
}

/// Each of the cricket's five partials, before its 1-4-6-4-1 weight. Their sum peaks at
/// sixteen times this, 0.56 at the source: loud enough to stand out over a silent night at
/// the few blocks the call is placed, where #1145's 0.018 was a faint tick.
const CRICKET_PARTIAL_GAIN: f32 = 0.035;

impl Call {
    pub(super) fn profile(self) -> CallProfile {
        let (interval, radius, height, seconds, range) = match self {
            Self::Rattlesnake => ([12.0, 31.0], 5.0, -1.3, 0.8, 24.0),
            Self::Crow => ([17.0, 43.0], 12.0, 3.0, 0.55, 48.0),
            Self::Eagle => ([9.0, 24.0], 18.0, 35.0, 0.65, 96.0),
            Self::Wolf => ([35.0, 79.0], 26.0, 0.0, 3.8, 96.0),
            // In the grass a few blocks off: one trill of a little under a second, several
            // times a minute, and most of every minute still silence.
            Self::Cricket => ([5.0, 12.0], 4.0, -1.2, 0.9, 24.0),
        };
        CallProfile {
            interval,
            radius,
            height,
            seconds,
            range,
        }
    }

    pub(super) fn description(self, seed: u64) -> Sound {
        let variation = (seed % 101) as f32 / 100.0;
        let tone = |hz, gain, envelope| Layer {
            exciter: Exciter::Oscillator {
                wave: Wave::Sine,
                hz,
            },
            gain,
            envelope,
            filter: None,
        };
        let (attack, decay, sustain, release) = match self {
            Self::Rattlesnake => (0.025, 0.2, 0.7, 0.2),
            Self::Crow => (0.025, 0.28, 0.05, 0.12),
            Self::Eagle => (0.015, 0.4, 0.0, 0.1),
            Self::Wolf => (0.8, 1.8, 0.35, 1.2),
            // A trill held at full level for the whole call: a short rise, then every pulse as
            // loud as the last until the release closes the call over its final few pulses.
            Self::Cricket => (0.03, 0.0, 1.0, 0.12),
        };
        let envelope = Envelope {
            attack,
            decay,
            sustain,
            release,
        };
        let layers = match self {
            // Close partials beat at rattle speed, under a dry band of noise.
            Self::Rattlesnake => {
                let hz = 2300.0 + variation * 250.0;
                vec![
                    tone(hz, 0.08, envelope),
                    tone(hz + 29.0, 0.08, envelope),
                    Layer {
                        envelope,
                        ..noise(Noise::White, 0.24, FilterKind::Band, 2700.0, 2.0)
                    },
                ]
            }
            Self::Crow => {
                let hz = 560.0 + variation * 100.0;
                vec![
                    tone(hz, 0.28, envelope),
                    tone(hz * 2.05, 0.14, envelope),
                    Layer {
                        envelope,
                        ..noise(Noise::White, 0.19, FilterKind::Band, 1200.0, 1.8)
                    },
                ]
            }
            Self::Eagle => {
                let hz = 2100.0 + variation * 300.0;
                vec![
                    tone(hz, 0.36, envelope),
                    tone(
                        hz * 1.35,
                        0.12,
                        Envelope {
                            attack: 0.08,
                            ..envelope
                        },
                    ),
                ]
            }
            // A slowly opening harmonic vowel with a soft breath. Staggered partial
            // envelopes change the colour across the howl without a new synth primitive.
            Self::Wolf => {
                let hz = 310.0 + variation * 55.0;
                vec![
                    tone(hz, 0.44, envelope),
                    tone(
                        hz * 2.0,
                        0.22,
                        Envelope {
                            attack: 1.3,
                            ..envelope
                        },
                    ),
                    tone(
                        hz * 3.01,
                        0.08,
                        Envelope {
                            attack: 1.8,
                            ..envelope
                        },
                    ),
                    Layer {
                        envelope,
                        ..noise(Noise::White, 0.06, FilterKind::Band, 650.0, 1.0)
                    },
                ]
            }
            // Five sines one pulse rate apart, weighted 1-4-6-4-1, sum to 16·cos⁴(π·rate·t)
            // times a carrier: a sharp pulse every 1/rate with true silence between pulses,
            // without an onset primitive. The top partial stays under 8 kHz's 3.6 kHz bound.
            Self::Cricket => {
                let hz = 3000.0 + variation * 200.0;
                let rate = pulse_rate(seed);
                [1.0, 4.0, 6.0, 4.0, 1.0]
                    .into_iter()
                    .enumerate()
                    .map(|(k, weight)| {
                        tone(
                            hz + k as f32 * rate,
                            CRICKET_PARTIAL_GAIN * weight,
                            envelope,
                        )
                    })
                    .collect()
            }
        };
        Sound { layers }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::spatial;
    fn stream(bed: Bed, rate: u32, seed: u64) -> Vec<f32> {
        let mut stream = bed.description().continuous(rate, seed).unwrap();
        let mut output = vec![0.0; rate as usize * 2];
        assert_eq!(stream.render(&mut output), output.len());
        output
    }
    #[test]
    fn every_bed_is_finite_audible_and_fresh_at_supported_rates() {
        for bed in super::super::BEDS {
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
    fn weather_beds_have_distinct_spectra() {
        let bright = |bed| {
            let v = stream(bed, 48000, 11);
            v.windows(2).map(|p| (p[1] - p[0]).powi(2)).sum::<f32>()
                / v.iter().map(|v| v * v).sum::<f32>()
        };
        assert!(bright(Bed::Rain) > bright(Bed::DrivingRain) * 3.0);
        assert!(bright(Bed::Rain) > bright(Bed::Snowfall) * 10.0);
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

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0, |peak, v| v.abs().max(peak))
    }

    /// The onsets of the pulses actually rendered, in seconds, from the samples rather than
    /// from `pulse_rate`: a 2 ms peak envelope must rise past a fifth of the loudest pulse,
    /// and fall back under a twentieth before another pulse is counted.
    fn rendered_pulses(seed: u64, rate: u32) -> Vec<f32> {
        let call = Call::Cricket
            .description(seed)
            .bake(Call::Cricket.profile().seconds, rate, seed)
            .unwrap();
        let envelope: Vec<f32> = call
            .samples()
            .chunks(rate as usize / 500)
            .map(peak)
            .collect();
        let loudest = envelope.iter().copied().fold(0.0, f32::max);
        let (mut onsets, mut armed) = (Vec::new(), true);
        for (window, level) in envelope.into_iter().enumerate() {
            if armed && level > loudest * 0.2 {
                onsets.push(window as f32 / 500.0);
                armed = false;
            } else if level < loudest * 0.05 {
                armed = true;
            }
        }
        onsets
    }

    /// #1161: #1145's call was one to three pulses about a quarter of a second apart — an
    /// occasional tick. A trill is many pulses at one rate, which is what makes it a cricket.
    #[test]
    fn a_cricket_call_is_a_trill_of_many_pulses_at_a_steady_rate() {
        let seconds = Call::Cricket.profile().seconds;
        for seed in 0..60u64 {
            let seed = super::super::controller::scramble(seed);
            let rate = pulse_rate(seed);
            assert!((26.0..=34.0).contains(&rate));
            let period = 1.0 / rate;
            for device in [8000, 48000] {
                let onsets = rendered_pulses(seed, device);
                // The rise and the release take a pulse or so off each end, and no more.
                assert!(
                    onsets.len() as f32 >= (seconds - 0.15) * rate,
                    "seed {seed} at {device}: {} pulses at {rate} a second",
                    onsets.len()
                );
                for pair in onsets.windows(2) {
                    let gap = pair[1] - pair[0];
                    assert!(
                        (gap - period).abs() <= period * 0.25,
                        "seed {seed} at {device}: a {gap} s gap in a trill of {period} s"
                    );
                }
            }
        }
    }

    /// Heard where the lane places it — `radius` out and `height` down, faded by the same
    /// `spatial::attenuation` every placed sound is — before the Ambience bus. #1145's call
    /// peaked near 0.12 here, and only for a few milliseconds of each of three pulses.
    #[test]
    fn a_cricket_call_placed_at_its_radius_is_clearly_heard() {
        let profile = Call::Cricket.profile();
        let gain = spatial::attenuation(profile.radius.hypot(profile.height), profile.range);
        for seed in 0..60u64 {
            let seed = super::super::controller::scramble(seed);
            let call = Call::Cricket
                .description(seed)
                .bake(profile.seconds, 48000, seed)
                .unwrap();
            let heard: Vec<f32> = call.samples().iter().map(|v| v * gain).collect();
            let rms = (heard.iter().map(|v| v * v).sum::<f32>() / heard.len() as f32).sqrt();
            assert!(
                peak(&heard) >= 0.2 && rms >= 0.06,
                "seed {seed}: peak {}, rms {rms}",
                peak(&heard)
            );
        }
    }
}
