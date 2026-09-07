//! An unbounded noise-driven source. Only this source's state is retained, never a loop.

use super::{CompiledLayer, Error, Exciter, Sound};

#[derive(Debug)]
pub struct Continuous {
    layers: Vec<CompiledLayer>,
    rate: u32,
    elapsed: u64,
    release: Option<u64>,
    release_samples: u64,
    finished: bool,
}

impl Sound {
    /// Requires an audible sustained noise layer. A finite collection of constant-frequency
    /// oscillators alone is periodic and is not an ambience bed under this contract.
    pub fn continuous(&self, rate: u32, seed: u64) -> Result<Continuous, Error> {
        self.validate(rate)?;
        if !self.layers.iter().any(|layer| {
            matches!(layer.exciter, Exciter::Noise(_))
                && layer.gain > 0.0
                && layer.envelope.sustain > 0.0
        }) {
            return Err(Error::ContinuousNeedsNoise);
        }
        let release_samples = self
            .layers
            .iter()
            .map(|layer| (f64::from(layer.envelope.release) * f64::from(rate)).ceil() as u64)
            .max()
            .unwrap_or(1)
            .max(1);
        Ok(Continuous {
            layers: self.compile(rate, seed),
            rate,
            elapsed: 0,
            release: None,
            release_samples,
            finished: false,
        })
    }
}

impl Continuous {
    pub fn sample_rate(&self) -> u32 {
        self.rate
    }

    /// Idempotent. Each layer releases from the envelope level it has reached, so stopping
    /// during its attack cannot jump up to the sustain level. The last emitted sample is zero.
    pub fn stop(&mut self) {
        self.release.get_or_insert(self.elapsed);
    }

    /// Returns the initialized prefix. Generation is independent of caller block size,
    /// allocates nothing, and keeps oscillator, noise and filter state across every call.
    pub fn render(&mut self, output: &mut [f32]) -> usize {
        if self.finished {
            return 0;
        }
        let mut written = 0;
        for sample in output {
            let seconds = self.elapsed as f64 / f64::from(self.rate);
            let sum: f64 = self
                .layers
                .iter_mut()
                .map(|layer| {
                    let gain = if let Some(start) = self.release {
                        let released = (self.elapsed - start) as f64 / f64::from(self.rate);
                        layer.envelope.held(start as f64 / f64::from(self.rate))
                            * (1.0 - released / f64::from(layer.envelope.release)).max(0.0)
                    } else {
                        layer.envelope.held(seconds)
                    };
                    layer.next() * gain
                })
                .sum();
            *sample = sum.clamp(-1.0, 1.0) as f32;
            written += 1;
            if self
                .release
                .is_some_and(|start| self.elapsed - start >= self.release_samples)
            {
                // Rounding a fractional release duration up ensures every layer has reached
                // zero, including the longest, before the producer starts draining its ring.
                *sample = 0.0;
                self.finished = true;
                break;
            }
            self.elapsed = self.elapsed.saturating_add(1);
        }
        written
    }
}
