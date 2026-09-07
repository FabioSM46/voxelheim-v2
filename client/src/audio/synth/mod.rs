//! A sound is a bounded description, compiled at the output device's sample rate.
//! Baked buffers retain that rate: playback must never reinterpret them at another rate.
//! Oscillators are elementary waveforms (saw/square/triangle are not band-limited); authors
//! should prefer sine/noise for bright transients where aliasing would be objectionable.

// Part 2 of #983 supplies the production consumer; later sound descriptions exercise the
// rest of the vocabulary. Public items in this binary crate otherwise count as dead code.
#![allow(dead_code)]

mod primitives;
use primitives::{Biquad, Generator};
// Sound authors in part 2 and the following content issues consume this vocabulary.
#[allow(unused_imports)]
pub use primitives::{Envelope, Exciter, Filter, FilterKind, Noise, Wave};
use std::sync::Arc;

pub const MAX_LAYERS: usize = 16;
pub const MAX_BAKED_SECONDS: f32 = 10.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub exciter: Exciter,
    pub gain: f32,
    pub envelope: Envelope,
    pub filter: Option<Filter>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sound {
    pub layers: Vec<Layer>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    SampleRate,
    Layers,
    Exciter,
    Gain,
    Envelope,
    Filter,
    Duration,
}

fn bounded(value: f32, low: f32, high: f32) -> bool {
    value.is_finite() && value >= low && value <= high
}

impl Sound {
    /// Validation precedes allocation, including the duration-to-sample-count conversion.
    /// Every release must fit the requested duration; an oversized release is rejected,
    /// never silently used as a gain reduction across the entire sound.
    pub fn bake(&self, seconds: f32, rate: u32, seed: u64) -> Result<Baked, Error> {
        self.validate(rate)?;
        if !bounded(seconds, 0.002, MAX_BAKED_SECONDS) {
            return Err(Error::Duration);
        }
        if self
            .layers
            .iter()
            .any(|layer| layer.envelope.release > seconds)
        {
            return Err(Error::Envelope);
        }
        let count = (f64::from(seconds) * f64::from(rate)).round() as usize;
        let mut layers = self.compile(rate, seed);
        let mut samples = Vec::with_capacity(count);
        for index in 0..count {
            let seconds = index as f64 / f64::from(rate);
            let remaining = (count - 1 - index) as f64 / f64::from(rate);
            let sum: f64 = layers
                .iter_mut()
                .map(|layer| {
                    // The final envelope is AFTER filtering: no filter tail can click at the
                    // buffer boundary. Short sounds still have both edges exactly at silence.
                    let release = (remaining / f64::from(layer.envelope.release)).min(1.0);
                    layer.next() * layer.envelope.held(seconds) * release
                })
                .sum();
            samples.push(sum.clamp(-1.0, 1.0) as f32);
        }
        Ok(Baked {
            samples: samples.into(),
            rate,
        })
    }

    pub(super) fn validate(&self, rate: u32) -> Result<(), Error> {
        if !(8_000..=192_000).contains(&rate) {
            return Err(Error::SampleRate);
        }
        if self.layers.is_empty() || self.layers.len() > MAX_LAYERS {
            return Err(Error::Layers);
        }
        for layer in &self.layers {
            if let Exciter::Oscillator { hz, .. } = layer.exciter
                && !bounded(hz, 0.01, rate as f32 * 0.45)
            {
                return Err(Error::Exciter);
            }
            if !bounded(layer.gain, 0.0, 1.0) {
                return Err(Error::Gain);
            }
            let envelope = layer.envelope;
            if !bounded(envelope.attack, 0.001, 60.0)
                || !bounded(envelope.decay, 0.0, 60.0)
                || !bounded(envelope.sustain, 0.0, 1.0)
                || !bounded(envelope.release, 0.001, 60.0)
            {
                return Err(Error::Envelope);
            }
            if let Some(filter) = layer.filter
                && (!bounded(filter.hz, 1.0, rate as f32 * 0.45) || !bounded(filter.q, 0.1, 10.0))
            {
                return Err(Error::Filter);
            }
        }
        Ok(())
    }

    fn compile(&self, rate: u32, seed: u64) -> Vec<CompiledLayer> {
        self.layers
            .iter()
            .enumerate()
            .map(|(index, layer)| CompiledLayer {
                generator: Generator::new(
                    layer.exciter,
                    seed.wrapping_add((index as u64).wrapping_mul(0xd1342543de82ef95)),
                    rate,
                ),
                envelope: layer.envelope,
                filter: layer.filter.map(|filter| Biquad::new(filter, rate)),
                gain: f64::from(layer.gain),
            })
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct Baked {
    samples: Arc<[f32]>,
    rate: u32,
}

impl Baked {
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }
    pub fn sample_rate(&self) -> u32 {
        self.rate
    }
}

#[derive(Debug)]
struct CompiledLayer {
    generator: Generator,
    envelope: Envelope,
    filter: Option<Biquad>,
    gain: f64,
}

impl CompiledLayer {
    fn next(&mut self) -> f64 {
        let input = self.generator.next();
        self.filter
            .as_mut()
            .map_or(input, |filter| filter.next(input))
            * self.gain
    }
}

#[cfg(test)]
mod tests;
