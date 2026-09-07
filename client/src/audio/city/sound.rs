//! Sample-timed city sounds. Random events advance with generated samples, never frames.
// Part 2 wires these producers to the city's bounded spatial source pool.
#![allow(dead_code)]

use crate::audio::synth::{
    Baked, Continuous, Envelope, Error, Exciter, Filter, FilterKind, Layer, Noise, Sound, Wave,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Forge,
    Fire,
}

/// One pair of transient buffers per output rate, shared by every audible structure.
pub(super) struct Palette {
    pub rate: u32,
    hammer: Baked,
    pop: Baked,
}

impl Palette {
    pub fn new(rate: u32) -> Result<Self, Error> {
        Ok(Self {
            rate,
            hammer: hammer().bake(0.28, rate, 0x5372)?,
            pop: pop().bake(0.025, rate, 0x9034)?,
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
            },
            cursor: usize::MAX,
            delay,
            gain: 1.0,
            rhythm,
        })
    }
}

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
            Kind::Forge if self.strikes_left == 0 => {
                self.strikes_left = 3 + (self.unit() * 4.0) as u8;
                2.0 + self.unit() * 3.0
            }
            Kind::Forge => {
                self.strikes_left -= 1;
                0.38 + self.unit() * 0.57
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hammer_has_variable_strikes_and_work_pauses_over_a_long_window() {
        let palette = Palette::new(8000).unwrap();
        let mut stream = palette.stream(Kind::Forge, 0).unwrap();
        let mut samples = vec![0.0; 8000 * 120];
        stream.render(&mut samples);
        // Detect real onsets after at least 50ms of silence, not the scheduler's fields.
        let mut silence = 8000;
        let mut strikes = Vec::new();
        for (index, value) in samples.iter().enumerate() {
            if value.abs() > 1e-5 {
                if silence > 400 {
                    strikes.push(index);
                }
                silence = 0;
            } else {
                silence += 1;
            }
        }
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

    #[test]
    fn sample_timing_is_reproducible_and_independent_of_update_blocks() {
        for rate in [8000, 44100, 48000, 192000] {
            let palette = Palette::new(rate).unwrap();
            for kind in [Kind::Forge, Kind::Fire] {
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
                assert_ne!(whole, chunks);
            }
        }
    }
}
