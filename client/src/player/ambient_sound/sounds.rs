//! Small descriptions rather than assets: every continuous layer advances fresh noise.
use crate::audio::synth::{
    self, Baked, Envelope, Exciter, Filter, FilterKind, Layer, Noise, Sound, Wave,
};

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

/// How many syllables one cricket call carries: two or three, from its seed — a "cri-cri".
pub(super) fn syllables(seed: u64) -> usize {
    2 + ((seed >> 8) % 2) as usize
}

/// How long one cricket syllable sounds.
pub(super) const SYLLABLE_SECONDS: f32 = 0.06;

/// Where each syllable of a call starts: one every 0.15 to 0.19 s, from its seed, so every
/// syllable is followed by at least 90 ms of silence before the next one.
pub(super) fn syllable_onsets(seed: u64) -> Vec<f32> {
    let period = 0.15 + ((seed >> 16) % 41) as f32 / 1000.0;
    (0..syllables(seed))
        .map(|index| index as f32 * period)
        .collect()
}

impl Call {
    pub(super) fn profile(self) -> CallProfile {
        let (interval, radius, height, seconds, range) = match self {
            Self::Rattlesnake => ([12.0, 31.0], 5.0, -1.3, 0.8, 24.0),
            Self::Crow => ([17.0, 43.0], 12.0, 3.0, 0.55, 48.0),
            Self::Eagle => ([9.0, 24.0], 18.0, 35.0, 0.65, 96.0),
            Self::Wolf => ([35.0, 79.0], 26.0, 0.0, 3.8, 96.0),
            // In the grass a few blocks off. The longest call, three syllables at the slowest
            // spacing, ends at 2 * 0.19 + 0.06 = 0.44 s, inside the baked 0.45 s.
            Self::Cricket => ([6.0, 16.0], 4.0, -1.2, 0.45, 24.0),
        };
        CallProfile {
            interval,
            radius,
            height,
            seconds,
            range,
        }
    }

    /// One call rendered at the device's rate, from its seed. Every call is its description
    /// baked once for its profile's length — except the cricket, whose description is one
    /// syllable, struck at each of [`syllable_onsets`] with silence between.
    pub(super) fn bake(self, seed: u64, rate: u32) -> Result<Baked, synth::Error> {
        let seconds = self.profile().seconds;
        match self {
            Self::Cricket => self.description(seed).bake_at(
                &syllable_onsets(seed),
                SYLLABLE_SECONDS,
                seconds,
                rate,
                seed,
            ),
            _ => self.description(seed).bake(seconds, rate, seed),
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
            // One syllable: a quick scrape that settles and is cut off before the next.
            Self::Cricket => (0.005, 0.025, 0.85, 0.02),
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
            // The cricket voice from before #1145, which was right in timbre and wrong only in
            // never stopping: white noise through a narrow band (q 8) near 3.2 kHz, a scraped
            // shimmer rather than #1145's pure whistle. Four such bands side by side, not one:
            // a syllable is sixty milliseconds rather than a bed, and uncorrelated bands add
            // level as well as width. The top band stays under 8 kHz's 3.6 kHz bound.
            Self::Cricket => {
                let hz = 3000.0 + variation * 100.0;
                [0.0, 150.0, 300.0, 450.0]
                    .into_iter()
                    .map(|offset| Layer {
                        envelope,
                        ..noise(Noise::White, 1.0, FilterKind::Band, hz + offset, 8.0)
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

    fn scramble(seed: u64) -> u64 {
        super::super::controller::scramble(seed)
    }

    /// The syllables actually rendered, as `(first, last)` sounding sample: runs of sound
    /// separated by at least 20 ms of exact silence. Band-passed noise crosses zero, but never
    /// holds it for twenty milliseconds.
    fn rendered_syllables(samples: &[f32], rate: u32) -> Vec<(usize, usize)> {
        let gap = rate as usize / 50;
        let mut found: Vec<(usize, usize)> = Vec::new();
        for (index, _) in samples.iter().enumerate().filter(|(_, v)| **v != 0.0) {
            match found.last_mut() {
                Some((_, last)) if index - *last <= gap => *last = index,
                _ => found.push((index, index)),
            }
        }
        found
    }

    /// #1161: an occasional "cri-cri". A call is two or three short syllables with real
    /// silence between them — not #1145's one to three faint pulses, and not a trill.
    #[test]
    fn a_cricket_call_is_two_or_three_syllables_with_silence_between() {
        let mut seen = [false; 2];
        for seed in (0..60u64).map(scramble) {
            let expected = syllables(seed);
            seen[expected - 2] = true;
            for rate in [8000, 48000] {
                let call = Call::Cricket.bake(seed, rate).unwrap();
                let found = rendered_syllables(call.samples(), rate);
                assert_eq!(found.len(), expected, "seed {seed} at {rate}: {found:?}");
                for (first, last) in &found {
                    let seconds = (last - first) as f32 / rate as f32;
                    assert!(
                        seconds > 0.03 && seconds <= SYLLABLE_SECONDS,
                        "seed {seed} at {rate}: a {seconds} s syllable"
                    );
                }
                for pair in found.windows(2) {
                    let silence = (pair[1].0 - pair[0].1) as f32 / rate as f32;
                    assert!(
                        silence >= 0.08,
                        "seed {seed} at {rate}: {silence} s between syllables"
                    );
                }
            }
        }
        assert_eq!(seen, [true; 2], "both syllable counts occur");
    }

    /// The share of a sound's energy within 60 Hz of its loudest frequency between 1 and 4 kHz.
    /// Goertzel's recurrence per DFT bin, so no crate and any length. A pure tone keeps nearly
    /// all of its energy on one frequency however it is enveloped; a scraped band spreads it.
    fn tonal_share(samples: &[f32], rate: u32) -> f32 {
        let n = samples.len();
        let bin = |hz: f32| (hz * n as f32 / rate as f32).round() as usize;
        let power: Vec<(usize, f64)> = (bin(1000.0)..bin(4000.0).min(n / 2))
            .map(|k| {
                let coefficient = 2.0 * (std::f64::consts::TAU * k as f64 / n as f64).cos();
                let (mut s1, mut s2) = (0.0f64, 0.0f64);
                for &x in samples {
                    let s0 = f64::from(x) + coefficient * s1 - s2;
                    s2 = s1;
                    s1 = s0;
                }
                (k, s1 * s1 + s2 * s2 - coefficient * s1 * s2)
            })
            .collect();
        let loudest = power
            .iter()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map_or(0, |(k, _)| *k);
        let near = power
            .iter()
            .filter(|(k, _)| k.abs_diff(loudest) <= bin(60.0))
            .map(|(_, p)| p)
            .sum::<f64>();
        // Parseval: the positive half of the spectrum holds n / 2 times the sample energy.
        let energy: f64 = samples.iter().map(|x| f64::from(*x).powi(2)).sum();
        (near / (energy * n as f64 / 2.0)) as f32
    }

    /// The owner's report on #1161: the cricket had become "just a whistle", a pure tone.
    /// The voice from before #1145 was a band of noise, and that is what a call is again. The
    /// negative control is the point: the same syllables voiced as a sine — the whistle — fail
    /// the same measurement, so the floor separates the two rather than passing everything.
    #[test]
    fn a_cricket_chirp_is_a_scraped_band_and_not_a_whistle() {
        let profile = Call::Cricket.profile();
        for seed in (0..20u64).map(scramble) {
            let call = Call::Cricket.bake(seed, 8000).unwrap();
            let share = tonal_share(call.samples(), 8000);
            assert!(
                share < 0.4,
                "seed {seed}: {share} of the energy on one frequency"
            );
        }
        let seed = scramble(7);
        let whistle = Sound {
            layers: vec![Layer {
                exciter: Exciter::Oscillator {
                    wave: Wave::Sine,
                    hz: 3200.0,
                },
                gain: 0.5,
                envelope: Call::Cricket.description(seed).layers[0].envelope,
                filter: None,
            }],
        }
        .bake_at(
            &syllable_onsets(seed),
            SYLLABLE_SECONDS,
            profile.seconds,
            8000,
            seed,
        )
        .unwrap();
        let share = tonal_share(whistle.samples(), 8000);
        assert!(share > 0.8, "a whistle measured {share}");
    }

    /// Heard where the lane places it — `radius` out and `height` down, faded by the same
    /// `spatial::attenuation` every placed sound is — before the Ambience bus, at the 48 kHz a
    /// device usually runs. #1145's call peaked near 0.12 here. The RMS is over the syllables,
    /// because the silence between them is the design rather than a lack of level.
    #[test]
    fn a_cricket_call_placed_at_its_radius_is_clearly_heard() {
        let profile = Call::Cricket.profile();
        let gain = spatial::attenuation(profile.radius.hypot(profile.height), profile.range);
        for seed in (0..60u64).map(scramble) {
            let call = Call::Cricket.bake(seed, 48000).unwrap();
            let heard: Vec<f32> = call.samples().iter().map(|v| v * gain).collect();
            let voiced: Vec<f32> = rendered_syllables(&heard, 48000)
                .into_iter()
                .flat_map(|(first, last)| heard[first..=last].to_vec())
                .collect();
            let rms = (voiced.iter().map(|v| v * v).sum::<f32>() / voiced.len() as f32).sqrt();
            assert!(
                peak(&heard) >= 0.2 && rms >= 0.05,
                "seed {seed}: peak {}, rms {rms}",
                peak(&heard)
            );
        }
    }
}
