//! A sound is a bounded description, compiled at the output device's sample rate.
//! Baked buffers retain that rate: playback must never reinterpret them at another rate.
//! Oscillators are elementary waveforms (saw/square/triangle are not band-limited); authors
//! should prefer sine/noise for bright transients where aliasing would be objectionable.
//! A glide is the one exciter whose frequency moves: a fall or rise between two pitches with
//! an optional vibrato, its phase accumulated sample by sample so the waveform never steps.
//! A gate is the one field that shapes a layer many times a second rather than once: a layer
//! struck open and shut, for a voice that is a sequence of impacts rather than a held sound.

// The arrival chime consumes baked sine layers; #984–#987 and #999 consume the other
// primitives and continuous/playback APIs. Public items in this binary crate otherwise
// count as dead code before those callers land.
#![allow(dead_code)]

mod continuous;
mod playback;
mod primitives;
pub use continuous::Continuous;
#[allow(unused_imports)]
pub use playback::{Playback, Rendering, StartError, Status};
use primitives::{Biquad, Gating, Generator};
// Following content issues consume the rest of this synthesis vocabulary.
#[allow(unused_imports)]
pub use primitives::{
    Curve, Envelope, Exciter, Filter, FilterKind, Gate, Glide, Noise, Vibrato, Wave,
};
use std::sync::Arc;

pub const MAX_LAYERS: usize = 16;
pub const MAX_BAKED_SECONDS: f32 = 10.0;

/// How many times a second a [`Gate`] may open, as a fraction of the sample rate: a twentieth,
/// so the shortest opening a gate can describe still holds a sample of its own at the lowest
/// supported rate.
const MAX_GATE_FRACTION: f32 = 0.05;

#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub exciter: Exciter,
    pub gain: f32,
    pub envelope: Envelope,
    pub filter: Option<Filter>,
    /// Struck open and shut rather than sounded continuously. Applied after the filter, so a
    /// resonant tail cannot smear the silence between two openings into a dip.
    pub gate: Option<Gate>,
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
    Gate,
    Duration,
    ContinuousNeedsNoise,
    GatedBed,
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

    /// This sound struck again at each of `onsets` seconds, summed into one `seconds`-long
    /// buffer: a call of separate syllables with true silence between them. Every layer's
    /// envelope starts at its sound's own zero, so a later syllable cannot be a layer of one
    /// description.
    ///
    /// Each strike is baked for `length` seconds exactly as [`Sound::bake`] would, and draws its
    /// own noise — the seed plus the strike's index — so a noise syllable is not one grain
    /// replayed. Every strike must end inside the buffer, which keeps both edges of the call at
    /// exact silence; one that would not, a negative onset, or no strike at all is refused. At
    /// most [`MAX_LAYERS`] strikes, the same bound one description's layers have.
    pub fn bake_at(
        &self,
        onsets: &[f32],
        length: f32,
        seconds: f32,
        rate: u32,
        seed: u64,
    ) -> Result<Baked, Error> {
        if onsets.is_empty() || onsets.len() > MAX_LAYERS {
            return Err(Error::Layers);
        }
        if !bounded(seconds, 0.002, MAX_BAKED_SECONDS)
            || !bounded(length, 0.002, seconds)
            || onsets
                .iter()
                .any(|onset| !bounded(*onset, 0.0, seconds - length))
        {
            return Err(Error::Duration);
        }
        let strikes = (0..onsets.len())
            .map(|index| self.bake(length, rate, seed.wrapping_add(index as u64)))
            .collect::<Result<Vec<_>, _>>()?;
        let count = (f64::from(seconds) * f64::from(rate)).round() as usize;
        let mut samples = vec![0.0f32; count];
        for (onset, strike) in onsets.iter().zip(&strikes) {
            let start = ((f64::from(*onset) * f64::from(rate)).round() as usize).min(count);
            for (sample, value) in samples[start..].iter_mut().zip(strike.samples()) {
                *sample = (*sample + value).clamp(-1.0, 1.0);
            }
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
            // Every frequency a glide passes through, vibrato included, obeys the
            // oscillator's own bound; a depth under one half keeps the phase moving forwards.
            if let Exciter::Glide(glide) = layer.exciter {
                let limit = rate as f32 * 0.45;
                let vibrato = glide.vibrato;
                if !(bounded(glide.from, 0.01, limit)
                    && bounded(glide.to, 0.01, limit)
                    && bounded(glide.seconds, 0.001, 60.0)
                    && bounded(vibrato.hz, 0.0, 40.0)
                    && bounded(vibrato.depth, 0.0, 0.5)
                    && bounded(vibrato.onset, 0.0, 60.0)
                    && glide.peak() <= limit)
                {
                    return Err(Error::Exciter);
                }
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
            // Both ends of a gate's rate obey the same bound, and the duty stays strictly
            // under one: a gate that never closes is a gain, and this field is not one.
            if let Some(gate) = layer.gate {
                let limit = rate as f32 * MAX_GATE_FRACTION;
                if !(bounded(gate.from, 0.1, limit)
                    && bounded(gate.to, 0.1, limit)
                    && bounded(gate.seconds, 0.001, 60.0)
                    && bounded(gate.duty, 0.05, 0.9))
                {
                    return Err(Error::Gate);
                }
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
                gate: layer.gate.map(|gate| Gating::new(gate, rate)),
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
    gate: Option<Gating>,
    gain: f64,
}

impl CompiledLayer {
    fn next(&mut self) -> f64 {
        let input = self.generator.next();
        let filtered = self
            .filter
            .as_mut()
            .map_or(input, |filter| filter.next(input));
        // The gate advances every sample whether or not the layer has one, because its phase
        // is a property of elapsed time and not of how loud the layer happens to be.
        let gated = self
            .gate
            .as_mut()
            .map_or(filtered, |gate| filtered * gate.next());
        gated * self.gain
    }
}

#[cfg(test)]
pub(crate) mod pin;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod stream_tests;
