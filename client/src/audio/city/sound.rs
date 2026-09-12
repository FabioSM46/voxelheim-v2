//! Sample-timed city sounds. Random events advance with generated samples, never frames.

use crate::audio::synth::{
    Baked, Continuous, Envelope, Error, Exciter, Filter, FilterKind, Layer, Noise, Sound, Wave,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Forge,
    Fire,
    /// A knife drawn over hide at the leather bench.
    Leather,
    /// A light planishing tap at the armour bench — brighter, shorter and quieter than the
    /// forge's hammer, so the two stations are told apart by ear.
    Armour,
    /// A low swell of hum at the enchanting table.
    Enchanting,
}

impl Kind {
    /// Every kind, for the sweeps. Hand-written because no stable Rust enumerates variants;
    /// what keeps a kind sounded is the wildcard-free matches below, not this list.
    #[cfg(test)]
    pub(super) const ALL: [Self; 5] = [
        Self::Forge,
        Self::Fire,
        Self::Leather,
        Self::Armour,
        Self::Enchanting,
    ];

    /// A dense index, for per-kind counters.
    pub(super) const COUNT: usize = 5;

    pub(super) fn index(self) -> usize {
        match self {
            Self::Forge => 0,
            Self::Fire => 1,
            Self::Leather => 2,
            Self::Armour => 3,
            Self::Enchanting => 4,
        }
    }
}

/// One set of transient buffers per output rate, shared by every audible structure.
pub(super) struct Palette {
    pub rate: u32,
    hammer: Baked,
    pop: Baked,
    scrape: Baked,
    tap: Baked,
    hum: Baked,
}

impl Palette {
    pub fn new(rate: u32) -> Result<Self, Error> {
        Ok(Self {
            rate,
            hammer: hammer().bake(0.28, rate, 0x5372)?,
            pop: pop().bake(0.025, rate, 0x9034)?,
            scrape: scrape().bake(SCRAPE_SECONDS, rate, 0x6c75)?,
            tap: tap().bake(TAP_SECONDS, rate, 0x7a11)?,
            hum: hum().bake(HUM_SECONDS, rate, 0x4d0e)?,
        })
    }

    pub fn stream(&self, kind: Kind, seed: u64) -> Result<Stream, Error> {
        let mut rhythm = Rhythm::new(seed);
        // Separate stations never all strike when they enter earshot together.
        let delay = (rhythm.unit() * f64::from(self.rate)) as usize;
        Ok(Stream {
            rate: self.rate,
            kind,
            bed: if kind == Kind::Fire {
                Some(fire().continuous(self.rate, seed)?)
            } else {
                None
            },
            transient: match kind {
                Kind::Forge => self.hammer.clone(),
                Kind::Fire => self.pop.clone(),
                Kind::Leather => self.scrape.clone(),
                Kind::Armour => self.tap.clone(),
                Kind::Enchanting => self.hum.clone(),
            },
            cursor: usize::MAX,
            delay,
            gain: 1.0,
            rhythm,
        })
    }
}

/// How long each bench's baked sound lasts. Every gap its rhythm schedules is longer, so
/// no stroke is ever cut off by the next one.
const SCRAPE_SECONDS: f32 = 0.26;
const TAP_SECONDS: f32 = 0.14;
const HUM_SECONDS: f32 = 2.4;

fn envelope(attack: f32, decay: f32, sustain: f32, release: f32) -> Envelope {
    Envelope {
        attack,
        decay,
        sustain,
        release,
    }
}

fn hammer() -> Sound {
    // Inharmonic steel partials over a short broadband impact; all decay to silence.
    let mut layers: Vec<_> = [(713.0, 0.23), (1193.0, 0.16), (2137.0, 0.09)]
        .into_iter()
        .map(|(hz, gain)| Layer {
            exciter: Exciter::Oscillator {
                wave: Wave::Sine,
                hz,
            },
            gain,
            envelope: envelope(0.001, 0.26, 0.0, 0.015),
            filter: None,
        })
        .collect();
    layers.push(Layer {
        exciter: Exciter::Noise(Noise::White),
        gain: 0.24,
        envelope: envelope(0.001, 0.025, 0.0, 0.015),
        filter: None,
    });
    Sound { layers }
}

fn pop() -> Sound {
    Sound {
        layers: vec![Layer {
            exciter: Exciter::Noise(Noise::White),
            gain: 0.36,
            envelope: envelope(0.001, 0.014, 0.0, 0.004),
            filter: Some(Filter {
                kind: FilterKind::High,
                hz: 850.0,
                q: 0.7,
            }),
        }],
    }
}

fn fire() -> Sound {
    // Low turbulent combustion under individual sharp pops. The bed is generated forever,
    // not a baked buffer repeated beneath a randomized volume control.
    Sound {
        layers: vec![Layer {
            exciter: Exciter::Noise(Noise::Brown),
            gain: 0.26,
            envelope: envelope(0.1, 0.0, 1.0, 0.1),
            filter: Some(Filter {
                kind: FilterKind::Low,
                hz: 1800.0,
                q: 0.7,
            }),
        }],
    }
}

fn scrape() -> Sound {
    // Band-limited noise with a soft attack: a blade drawn over hide, not struck on it. No
    // tonal layer at all, which is what keeps it from reading as metal.
    let band = |hz, q, gain, attack, decay| Layer {
        exciter: Exciter::Noise(Noise::White),
        gain,
        envelope: envelope(attack, decay, 0.0, 0.02),
        filter: Some(Filter {
            kind: FilterKind::Band,
            hz,
            q,
        }),
    };
    Sound {
        layers: vec![
            band(2400.0, 0.9, 0.55, 0.03, 0.20),
            band(900.0, 0.7, 0.30, 0.05, 0.16),
        ],
    }
}

fn tap() -> Sound {
    // Higher, sparser partials than the hammer and a fraction of its level and length: a
    // small hammer on thin plate.
    // Every partial stays under 0.45 of the lowest rate the synthesiser accepts, 8 kHz.
    let mut layers: Vec<_> = [(1480.0, 0.10), (2630.0, 0.06), (3310.0, 0.03)]
        .into_iter()
        .map(|(hz, gain)| Layer {
            exciter: Exciter::Oscillator {
                wave: Wave::Sine,
                hz,
            },
            gain,
            envelope: envelope(0.001, 0.12, 0.0, 0.01),
            filter: None,
        })
        .collect();
    layers.push(Layer {
        exciter: Exciter::Noise(Noise::White),
        gain: 0.10,
        envelope: envelope(0.001, 0.008, 0.0, 0.004),
        filter: Some(Filter {
            kind: FilterKind::High,
            hz: 2000.0,
            q: 0.7,
        }),
    });
    Sound { layers }
}

fn hum() -> Sound {
    // Two low sines a fifth apart over a dark noise floor, swelling in and out. Baked once
    // like every transient; what varies from swell to swell is the rhythm's gain and gap.
    let sustained = envelope(0.6, 0.0, 1.0, 0.8);
    let sine = |hz, gain| Layer {
        exciter: Exciter::Oscillator {
            wave: Wave::Sine,
            hz,
        },
        gain,
        envelope: sustained,
        filter: None,
    };
    Sound {
        layers: vec![
            sine(98.0, 0.20),
            sine(147.0, 0.08),
            Layer {
                exciter: Exciter::Noise(Noise::Brown),
                gain: 0.15,
                envelope: sustained,
                filter: Some(Filter {
                    kind: FilterKind::Low,
                    hz: 240.0,
                    q: 0.7,
                }),
            },
        ],
    }
}

pub(super) struct Stream {
    pub rate: u32,
    kind: Kind,
    bed: Option<Continuous>,
    transient: Baked,
    cursor: usize,
    delay: usize,
    gain: f32,
    rhythm: Rhythm,
}

impl Stream {
    /// Called only by the ECS producer. The caller retains samples until its ring accepts
    /// them; no allocation occurs here and chunk boundaries cannot alter the sound.
    pub fn render(&mut self, output: &mut [f32]) {
        if let Some(bed) = &mut self.bed {
            bed.render(output);
        } else {
            output.fill(0.0);
        }
        for sample in output {
            if self.delay == 0 {
                self.cursor = 0;
                self.gain = (0.55 + self.rhythm.unit() * 0.45) as f32;
                self.delay = (self.rhythm.interval(self.kind) * f64::from(self.rate)) as usize;
            }
            self.delay -= 1;
            if let Some(value) = self.transient.samples().get(self.cursor) {
                *sample += value * self.gain;
                self.cursor += 1;
            }
        }
    }
}

struct Rhythm {
    state: u64,
    strikes_left: u8,
}

impl Rhythm {
    fn new(seed: u64) -> Self {
        Self {
            state: seed,
            strikes_left: 3,
        }
    }

    // SplitMix64: zero is a valid seed; independent of the synthesis noise's generator.
    fn unit(&mut self) -> f64 {
        self.state = self.state.wrapping_add(0x9e3779b97f4a7c15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
        value ^= value >> 31;
        (value >> 11) as f64 / (1u64 << 53) as f64
    }

    fn interval(&mut self, kind: Kind) -> f64 {
        match kind {
            Kind::Fire => 0.035 + self.unit() * 0.32,
            Kind::Forge => self.burst((3, 4.0), (2.0, 3.0), (0.38, 0.57)),
            // One to three strokes, then a long pause while the hide is turned.
            Kind::Leather => self.burst((1, 3.0), (3.0, 4.0), (0.55, 0.50)),
            Kind::Armour => self.burst((2, 3.0), (3.0, 3.0), (0.50, 0.40)),
            // Never shorter than a swell plus a breath of silence.
            Kind::Enchanting => f64::from(HUM_SECONDS) + 0.6 + self.unit() * 4.0,
        }
    }

    /// Work in bursts: a group of strokes `between` apart, then a `pause`. Each pair is a
    /// minimum and a random spread over it; the group size is a minimum and a spread too.
    fn burst(&mut self, strikes: (u8, f64), pause: (f64, f64), between: (f64, f64)) -> f64 {
        if self.strikes_left == 0 {
            self.strikes_left = strikes.0 + (self.unit() * strikes.1) as u8;
            pause.0 + self.unit() * pause.1
        } else {
            self.strikes_left -= 1;
            between.0 + self.unit() * between.1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Onsets after at least `quiet` samples of silence, as sample indices.
    fn onsets(samples: &[f32], quiet: usize) -> Vec<usize> {
        let mut silence = usize::MAX / 2;
        let mut found = Vec::new();
        for (index, value) in samples.iter().enumerate() {
            if value.abs() > 1e-5 {
                if silence > quiet {
                    found.push(index);
                }
                silence = 0;
            } else {
                silence += 1;
            }
        }
        found
    }

    fn peak(samples: &[f32]) -> f32 {
        samples
            .iter()
            .fold(0.0, |peak, value| peak.max(value.abs()))
    }

    /// Sign changes per second of audible signal: a rough measure of how high a sound sits.
    fn crossings_per_second(samples: &[f32], rate: u32) -> f32 {
        let audible: Vec<_> = samples.iter().filter(|v| v.abs() > 1e-4).collect();
        let crossings = audible
            .windows(2)
            .filter(|w| (*w[0] < 0.0) != (*w[1] < 0.0))
            .count();
        crossings as f32 / (audible.len() as f32 / rate as f32)
    }

    #[test]
    fn hammer_has_variable_strikes_and_work_pauses_over_a_long_window() {
        let palette = Palette::new(8000).unwrap();
        let mut stream = palette.stream(Kind::Forge, 0).unwrap();
        let mut samples = vec![0.0; 8000 * 120];
        stream.render(&mut samples);
        // Detect real onsets after at least 50ms of silence, not the scheduler's fields.
        let strikes = onsets(&samples, 400);
        assert!(strikes.len() > 60);
        let gaps: Vec<_> = strikes
            .windows(2)
            .map(|w| (w[1] - w[0]) as f32 / 8000.0)
            .collect();
        assert!(gaps.iter().any(|gap| *gap > 2.0));
        assert!(gaps.iter().any(|gap| *gap < 0.7));
        assert!(
            gaps.windows(2)
                .filter(|w| (w[0] - w[1]).abs() > 0.02)
                .count()
                > 40
        );
    }

    #[test]
    fn fire_keeps_changing_for_a_minute_and_has_sharp_crackles() {
        let palette = Palette::new(8000).unwrap();
        let mut stream = palette.stream(Kind::Fire, 17).unwrap();
        let mut samples = vec![0.0; 8000 * 60];
        stream.render(&mut samples);
        for lag in [800, 8000, 8000 * 5, 8000 * 10, 8000 * 20] {
            let equal = samples[8000..8000 + 8000]
                .iter()
                .zip(&samples[8000 + lag..])
                .filter(|(a, b)| a == b)
                .count();
            assert!(equal < 10, "repeated fire at lag {lag}");
        }
        assert!(samples.iter().filter(|v| v.abs() > 0.15).count() > 100);
        assert!(samples.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
    }

    /// The benches are sparse: short strokes in small groups, with long pauses between, and
    /// silent most of the time.
    #[test]
    fn the_leather_and_armour_benches_work_in_sparse_short_strokes() {
        for kind in [Kind::Leather, Kind::Armour] {
            let palette = Palette::new(8000).unwrap();
            let mut stream = palette.stream(kind, 3).unwrap();
            let mut samples = vec![0.0; 8000 * 120];
            stream.render(&mut samples);
            let strokes = onsets(&samples, 400);
            assert!(strokes.len() > 20, "{kind:?}: {} strokes", strokes.len());
            let gaps: Vec<_> = strokes
                .windows(2)
                .map(|w| (w[1] - w[0]) as f32 / 8000.0)
                .collect();
            assert!(gaps.iter().any(|gap| *gap > 3.0), "{kind:?} never pauses");
            assert!(gaps.iter().any(|gap| *gap < 1.0), "{kind:?} never groups");
            assert!(
                gaps.iter().all(|gap| *gap > 0.3),
                "{kind:?} strokes overlap"
            );
            let sounding = samples.iter().filter(|v| v.abs() > 1e-5).count();
            assert!(
                sounding < samples.len() / 4,
                "{kind:?} sounds {sounding} of {} samples",
                samples.len()
            );
        }
    }

    #[test]
    fn the_armour_tap_is_lighter_and_shorter_than_the_forge_hammer() {
        let palette = Palette::new(48000).unwrap();
        assert!(peak(palette.tap.samples()) < peak(palette.hammer.samples()) * 0.6);
        assert!(palette.tap.samples().len() < palette.hammer.samples().len());
        // And brighter, so it is not simply a quiet forge.
        assert!(
            crossings_per_second(palette.tap.samples(), 48000)
                > crossings_per_second(palette.hammer.samples(), 48000)
        );
    }

    #[test]
    fn the_enchanting_hum_is_low_and_comes_in_separate_swells() {
        let palette = Palette::new(8000).unwrap();
        let hum = crossings_per_second(palette.hum.samples(), 8000);
        let scrape = crossings_per_second(palette.scrape.samples(), 8000);
        assert!(
            hum < 600.0 && hum * 3.0 < scrape,
            "hum {hum}, scrape {scrape}"
        );

        let mut stream = palette.stream(Kind::Enchanting, 11).unwrap();
        let mut samples = vec![0.0; 8000 * 90];
        stream.render(&mut samples);
        let swells = onsets(&samples, 2000);
        assert!(swells.len() >= 8, "{} swells", swells.len());
        for pair in swells.windows(2) {
            let gap = (pair[1] - pair[0]) as f32 / 8000.0;
            assert!(gap >= HUM_SECONDS + 0.5, "swells {gap}s apart overlap");
        }
    }

    #[test]
    fn sample_timing_is_reproducible_and_independent_of_update_blocks() {
        for rate in [8000, 44100, 48000, 192000] {
            let palette = Palette::new(rate).unwrap();
            for kind in Kind::ALL {
                let mut a = palette.stream(kind, 59).unwrap();
                let mut b = palette.stream(kind, 59).unwrap();
                let mut whole = vec![0.0; rate as usize * 2];
                let mut chunks = whole.clone();
                a.render(&mut whole);
                for chunk in chunks.chunks_mut(137) {
                    b.render(chunk);
                }
                assert_eq!(whole, chunks);
                let mut other = palette.stream(kind, 60).unwrap();
                other.render(&mut chunks);
                assert_ne!(whole, chunks, "{kind:?} at {rate}");
            }
        }
    }

    #[test]
    fn every_kind_has_a_distinct_index_below_the_count() {
        let mut indices: Vec<_> = Kind::ALL.iter().map(|kind| kind.index()).collect();
        indices.sort_unstable();
        indices.dedup();
        assert_eq!(indices, (0..Kind::COUNT).collect::<Vec<_>>());
    }
}
