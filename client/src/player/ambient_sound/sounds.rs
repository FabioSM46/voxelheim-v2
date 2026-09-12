//! Small descriptions rather than assets: every continuous layer advances fresh noise.
use crate::audio::synth::{
    self, Baked, Curve, Envelope, Exciter, Filter, FilterKind, Glide, Layer, Noise, Sound, Vibrato,
    Wave,
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

/// A voice, and nothing but a voice. These descriptions spawn no entities: snow's daytime
/// bird is the eagle already selected by birds::species_for, and the macaw's body comes from
/// `birds.rs`.
///
/// **Where each of these is heard, and when, is not here** — it is one row of
/// [`super::wildlife::WILDLIFE`], which is also where the rule about where a voice
/// originates is written down. This enum is the sound; that table is the creature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Call {
    Rattlesnake,
    Crow,
    Eagle,
    Wolf,
    /// Green country at night: an occasional cricket, not a continuous wall of them.
    Cricket,
    /// The macaw's squawk, heard by day only where `birds::species_for` answers the macaw.
    Parrot,
}

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

/// How many squawks one macaw call carries: one or two, from its seed — a "raak", or a
/// "raak... raak".
pub(super) fn squawks(seed: u64) -> usize {
    1 + ((seed >> 8) % 2) as usize
}

/// How long one squawk sounds.
pub(super) const SQUAWK_SECONDS: f32 = 0.3;

/// Where each squawk of a call starts: one every 0.42 to 0.52 s, from its seed, so a second
/// squawk follows the first after at least 120 ms of silence.
pub(super) fn squawk_onsets(seed: u64) -> Vec<f32> {
    let period = 0.42 + ((seed >> 16) % 101) as f32 / 1000.0;
    (0..squawks(seed))
        .map(|index| index as f32 * period)
        .collect()
}

/// How far the squawk's pitch arches above its falling line, as the vibrato's depth: one half
/// cycle of the vibrato spans the squawk, so the pitch rises into the call and falls out of it.
const SQUAWK_ARCH: f32 = 0.35;

/// Where the squawk's pitch line ends, as a fraction of where it starts.
const SQUAWK_FALL: f32 = 0.72;

/// How far above the voice its rough twin sits: at 620 to 780 Hz, 4% beats at 25 to 31 Hz,
/// the rattle of a harsh throat rather than a second note.
const SQUAWK_DETUNE: f32 = 1.04;

/// One squawk of a macaw: harsh, raspy and broadband, never a note.
///
/// - **A pitch contour.** Every voiced layer rides one glide from the seed's pitch down to
///   [`SQUAWK_FALL`] of it, lifted by a single half-cycle of vibrato that peaks mid-squawk:
///   the pitch rises about 15% into the call and then falls about 40% below that peak.
/// - **Roughness.** A sawtooth's dense harmonics through two formant bands, the nasal
///   1.5 kHz and the bright 2.8 kHz, and beside each a second sawtooth [`SQUAWK_DETUNE`]
///   higher: the two beat at tens of hertz, amplitude modulation a throat makes.
/// - **Breath.** White noise through the same two formants and a hiss above them, so the
///   spectrum between the harmonics is filled rather than empty.
///
/// Every band, and every frequency a glide reaches, stays under 3.6 kHz: 0.45 of the lowest
/// supported rate.
fn squawk(variation: f32, envelope: Envelope) -> Vec<Layer> {
    let hz = 620.0 + variation * 160.0;
    let voice = |detune: f32, gain, formant, q| Layer {
        exciter: Exciter::Glide(Glide {
            wave: Wave::Saw,
            from: hz * detune,
            to: hz * detune * SQUAWK_FALL,
            seconds: SQUAWK_SECONDS,
            curve: Curve::Exponential,
            vibrato: Vibrato {
                hz: 0.5 / SQUAWK_SECONDS,
                depth: SQUAWK_ARCH,
                onset: 0.0,
            },
        }),
        gain,
        envelope,
        filter: Some(Filter {
            kind: FilterKind::Band,
            hz: formant,
            q,
        }),
    };
    let breath = |gain, kind, formant, q| Layer {
        envelope,
        ..noise(Noise::White, gain, kind, formant, q)
    };
    vec![
        voice(1.0, 0.26, 1500.0, 1.0),
        voice(SQUAWK_DETUNE, 0.19, 1500.0, 1.0),
        voice(1.0, 0.17, 2800.0, 1.4),
        voice(SQUAWK_DETUNE, 0.12, 2800.0, 1.4),
        breath(0.15, FilterKind::Band, 1500.0, 1.5),
        breath(0.15, FilterKind::Band, 2800.0, 2.0),
        breath(0.06, FilterKind::Band, 3400.0, 1.2),
    ]
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
            // In a tree seven blocks off and five up, where the day lane always placed it, and
            // a few calls a minute rather than one every second (#1176). Two squawks at the
            // slowest spacing end at 0.52 + 0.3 = 0.82 s, inside the baked 0.85 s.
            Self::Parrot => ([8.0, 22.0], 7.0, 5.0, 0.85, 32.0),
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
    /// baked once for its profile's length — except the cricket and the macaw, whose
    /// descriptions are one syllable, struck at each of [`syllable_onsets`] or
    /// [`squawk_onsets`] with silence between.
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
            Self::Parrot => self.description(seed).bake_at(
                &squawk_onsets(seed),
                SQUAWK_SECONDS,
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
            // One squawk: a hard but unclicked onset, a harsh held middle, a quick close.
            Self::Parrot => (0.012, 0.08, 0.7, 0.06),
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
            Self::Parrot => squawk(variation, envelope),
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
            let call = Call::Parrot.bake(7, rate).unwrap();
            assert_eq!(call.samples()[0], 0.0);
            assert_eq!(*call.samples().last().unwrap(), 0.0);
            assert!(call.samples().iter().any(|v| v.abs() > 0.01));
            assert_ne!(
                call.samples(),
                Call::Parrot.bake(11, rate).unwrap().samples()
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

    /// The call #1176 replaced, verbatim: two sine partials at `hz` and `hz * 1.7` over a
    /// band of noise. The owner's recording matched it on every axis.
    fn old_parrot(seed: u64) -> Sound {
        let hz = 1150.0 + (seed % 401) as f32;
        let envelope = Envelope {
            attack: 0.015,
            decay: 0.11,
            sustain: 0.0,
            release: 0.04,
        };
        Sound {
            layers: vec![
                Layer {
                    exciter: Exciter::Oscillator {
                        wave: Wave::Sine,
                        hz,
                    },
                    gain: 0.16,
                    envelope,
                    filter: None,
                },
                Layer {
                    exciter: Exciter::Oscillator {
                        wave: Wave::Sine,
                        hz: hz * 1.7,
                    },
                    gain: 0.07,
                    envelope,
                    filter: None,
                },
                Layer {
                    envelope: Envelope {
                        attack: 0.01,
                        decay: 0.14,
                        sustain: 0.0,
                        release: 0.04,
                    },
                    ..noise(Noise::White, 0.11, FilterKind::Band, 1600.0, 1.5)
                },
            ],
        }
    }

    /// The new squawk with its voice made clean: every voiced layer the same glide as a sine,
    /// and no breath. The contour survives; the rasp and the air do not.
    fn clean_squawk(seed: u64) -> Sound {
        Sound {
            layers: Call::Parrot
                .description(seed)
                .layers
                .into_iter()
                .filter_map(|layer| match layer.exciter {
                    Exciter::Glide(glide) => Some(Layer {
                        exciter: Exciter::Glide(Glide {
                            wave: Wave::Sine,
                            ..glide
                        }),
                        filter: None,
                        ..layer
                    }),
                    _ => None,
                })
                .collect(),
        }
    }

    /// Energy per DFT bin from `low` to `high` hertz, one bin per `1 / duration` hertz, by
    /// Goertzel's recurrence as in [`tonal_share`].
    fn band_power(samples: &[f32], rate: u32, low: f32, high: f32) -> Vec<f64> {
        let n = samples.len();
        let bin = |hz: f32| (hz * n as f32 / rate as f32).round() as usize;
        (bin(low)..bin(high).min(n / 2))
            .map(|k| {
                let coefficient = 2.0 * (std::f64::consts::TAU * k as f64 / n as f64).cos();
                let (mut s1, mut s2) = (0.0f64, 0.0f64);
                for &x in samples {
                    let s0 = f64::from(x) + coefficient * s1 - s2;
                    s2 = s1;
                    s1 = s0;
                }
                s1 * s1 + s2 * s2 - coefficient * s1 * s2
            })
            .collect()
    }

    /// Spectral flatness from 400 Hz to 3.6 kHz: the geometric mean of the power spectrum over
    /// its arithmetic mean. One for a perfectly flat spectrum, about 0.56 for one periodogram of
    /// white noise, and near zero for clean partials, whose energy is on a few bins and whose
    /// other bins are empty. A voice made rough and breathy fills the bins between harmonics.
    fn flatness(samples: &[f32], rate: u32) -> f64 {
        let power = band_power(samples, rate, 400.0, 3600.0);
        let floor = power.iter().sum::<f64>() / power.len() as f64 * 1e-12;
        let log = power.iter().map(|p| (p + floor).ln()).sum::<f64>() / power.len() as f64;
        log.exp() / (power.iter().sum::<f64>() / power.len() as f64)
    }

    /// The squawk's voiced layers alone, one squawk baked at 8 kHz: the pitch without the
    /// breath, so a tracker reads the voice rather than the noise around it.
    fn voiced_squawk(seed: u64) -> Vec<f32> {
        Sound {
            layers: Call::Parrot
                .description(seed)
                .layers
                .into_iter()
                .filter(|layer| matches!(layer.exciter, Exciter::Glide(_)))
                .collect(),
        }
        .bake(SQUAWK_SECONDS, 8000, seed)
        .unwrap()
        .samples()
        .to_vec()
    }

    /// The dominant frequency between `low` and `high` hertz every ten milliseconds, from a
    /// Hann-windowed thirty-millisecond window, read on a 5 Hz grid. Windows under a twentieth
    /// of the loudest one's energy are skipped: the onset and the release have no pitch worth
    /// reading.
    fn dominant_track(samples: &[f32], rate: u32, low: f32, high: f32) -> Vec<(f32, f32)> {
        let window = rate as usize * 3 / 100;
        let hop = rate as usize / 100;
        let hann: Vec<f32> = (0..window)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / window as f32).cos())
            .collect();
        let frames: Vec<(usize, Vec<f32>)> = (0..samples.len() - window)
            .step_by(hop)
            .map(|start| {
                let frame = samples[start..start + window]
                    .iter()
                    .zip(&hann)
                    .map(|(x, w)| x * w)
                    .collect();
                (start, frame)
            })
            .collect();
        let energy = |frame: &[f32]| frame.iter().map(|x| x * x).sum::<f32>();
        let loudest = frames.iter().map(|(_, f)| energy(f)).fold(0.0, f32::max);
        frames
            .into_iter()
            .filter(|(_, frame)| energy(frame) >= loudest * 0.05)
            .map(|(start, frame)| {
                let power = |hz: f32| {
                    let coefficient =
                        2.0 * (std::f64::consts::TAU * f64::from(hz / rate as f32)).cos();
                    let (mut s1, mut s2) = (0.0f64, 0.0f64);
                    for &x in &frame {
                        let s0 = f64::from(x) + coefficient * s1 - s2;
                        s2 = s1;
                        s1 = s0;
                    }
                    s1 * s1 + s2 * s2 - coefficient * s1 * s2
                };
                let hz = (0..)
                    .map(|step| low + step as f32 * 5.0)
                    .take_while(|hz| *hz <= high)
                    .max_by(|a, b| power(*a).total_cmp(&power(*b)))
                    .unwrap_or(low);
                ((start + window / 2) as f32 / rate as f32, hz)
            })
            .collect()
    }

    /// The pitch a squawk's glide starts from, read back from its description.
    fn squawk_pitch(seed: u64) -> f32 {
        match Call::Parrot.description(seed).layers[0].exciter {
            Exciter::Glide(glide) => glide.from,
            other => panic!("the squawk's first layer is not voiced: {other:?}"),
        }
    }

    /// #1176: the owner's rule is that a sound is realistic, never a note, and the day call was
    /// two sine partials. A squawk spreads its energy across the whole band a macaw fills, where
    /// a note keeps it on a few frequencies with nothing between them. The negative controls are
    /// the point: the call it replaced, a strict sine pair, and this very squawk voiced as clean
    /// sines along the same contour all fail the measurement the squawk passes.
    #[test]
    fn a_squawk_is_harsh_and_broadband_and_not_a_note() {
        let profile = Call::Parrot.profile();
        for seed in (0..20u64).map(scramble) {
            let call = Call::Parrot.bake(seed, 8000).unwrap();
            let flat = flatness(call.samples(), 8000);
            let tonal = tonal_share(call.samples(), 8000);
            assert!(
                flat > 0.15 && tonal < 0.4,
                "seed {seed}: flatness {flat}, {tonal} of the energy on one frequency"
            );
            let old = old_parrot(seed).bake(0.3, 8000, seed).unwrap();
            let pair = Sound {
                layers: old_parrot(seed).layers[..2].to_vec(),
            }
            .bake(0.3, 8000, seed)
            .unwrap();
            let clean = clean_squawk(seed)
                .bake_at(
                    &squawk_onsets(seed),
                    SQUAWK_SECONDS,
                    profile.seconds,
                    8000,
                    seed,
                )
                .unwrap();
            for (name, note) in [
                ("the old call", old),
                ("a sine pair", pair),
                ("a clean squawk", clean),
            ] {
                let flat = flatness(note.samples(), 8000);
                assert!(flat < 0.05, "seed {seed}: {name} measured flatness {flat}");
            }
        }
    }

    /// A squawk's pitch moves: it rises into the call and falls out of it, and is never steady.
    /// Read on the voiced layers alone, between 0.6 and 1.3 times the pitch the glide starts
    /// from — a band the fundamental stays inside for the whole squawk and its second harmonic
    /// never enters.
    #[test]
    fn a_squawk_rises_into_the_call_and_falls_out_of_it() {
        for seed in (0..12u64).map(scramble) {
            let hz = squawk_pitch(seed);
            let track = dominant_track(&voiced_squawk(seed), 8000, hz * 0.6, hz * 1.3);
            assert!(
                track.len() >= 20,
                "seed {seed}: {} voiced windows",
                track.len()
            );
            let (time, top) = track
                .iter()
                .copied()
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap();
            let (first, last) = (track[0].1, track[track.len() - 1].1);
            assert!(
                top >= first * 1.08,
                "seed {seed}: rises from {first} to only {top} Hz"
            );
            assert!(
                top >= last * 1.25,
                "seed {seed}: falls from {top} to only {last} Hz"
            );
            assert!(
                (0.05..=0.2).contains(&time),
                "seed {seed}: peaks at {time} s"
            );
        }
    }

    /// A call is one squawk or two, from its seed, with real silence between two.
    #[test]
    fn a_macaw_call_is_one_or_two_squawks_with_silence_between() {
        let mut seen = [false; 2];
        for seed in (0..40u64).map(scramble) {
            let expected = squawks(seed);
            seen[expected - 1] = true;
            for rate in [8000, 48000] {
                let call = Call::Parrot.bake(seed, rate).unwrap();
                let found = rendered_syllables(call.samples(), rate);
                assert_eq!(found.len(), expected, "seed {seed} at {rate}: {found:?}");
                for (first, last) in &found {
                    let seconds = (last - first) as f32 / rate as f32;
                    assert!(
                        seconds > 0.2 && seconds <= SQUAWK_SECONDS,
                        "seed {seed} at {rate}: a {seconds} s squawk"
                    );
                }
                for pair in found.windows(2) {
                    let silence = (pair[1].0 - pair[0].1) as f32 / rate as f32;
                    assert!(
                        silence >= 0.1,
                        "seed {seed} at {rate}: {silence} s between squawks"
                    );
                }
            }
        }
        assert_eq!(seen, [true; 2], "both squawk counts occur");
    }

    /// Heard where the day lane places it — seven blocks out and five up, faded by the same
    /// `spatial::attenuation` every placed sound is. A macaw carries: the squawk peaks near 0.6
    /// before placement at 48 kHz and near 0.44 at 8 kHz, where the call it replaced peaked near
    /// 0.23 and was heard at about 0.04. It never clips, and starts and ends at silence, at
    /// every device rate.
    #[test]
    fn a_macaw_call_carries_to_its_perch_without_clipping_at_any_rate() {
        let profile = Call::Parrot.profile();
        let gain = spatial::attenuation(profile.radius.hypot(profile.height), profile.range);
        for seed in (0..20u64).map(scramble) {
            for rate in [8000, 44100, 48000, 96000, 192000] {
                let call = Call::Parrot.bake(seed, rate).unwrap();
                let samples = call.samples();
                assert!(samples.iter().all(|v| v.is_finite()));
                assert!(
                    peak(samples) < 0.85,
                    "seed {seed} at {rate}: peaks at {}",
                    peak(samples)
                );
                assert!(
                    peak(samples) * gain >= 0.06,
                    "seed {seed} at {rate}: heard at {}",
                    peak(samples) * gain
                );
                assert_eq!(samples.first(), Some(&0.0));
                assert_eq!(samples.last(), Some(&0.0));
            }
        }
    }
}
