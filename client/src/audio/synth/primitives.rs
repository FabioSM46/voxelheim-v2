//! Stateful arithmetic shared by baked and streaming renderers. No device or Bevy types.

use std::f64::consts::TAU;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Wave {
    Sine,
    Saw,
    Square,
    Triangle,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Noise {
    White,
    /// White noise integrated through a leaky pole at 40 Hz (a bounded brown spectrum).
    Brown,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Exciter {
    Oscillator {
        wave: Wave,
        hz: f32,
    },
    Noise(Noise),
    /// An oscillator whose frequency moves while it sounds.
    Glide(Glide),
}

/// How a [`Glide`] travels between its two frequencies.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Curve {
    /// Equal hertz in equal time.
    Linear,
    /// Equal musical interval in equal time: the midpoint is the geometric mean, which is
    /// how a voice falls.
    Exponential,
}

/// Sinusoidal frequency modulation. `depth` is a fraction of the frequency the glide has
/// reached, so a flutter stays the same interval wide as the pitch falls under it, and it
/// opens linearly from nothing over `onset` seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vibrato {
    pub hz: f32,
    pub depth: f32,
    pub onset: f32,
}

impl Vibrato {
    pub const NONE: Self = Self {
        hz: 0.0,
        depth: 0.0,
        onset: 0.0,
    };
}

/// A waveform from `from` to `to` hertz over `seconds`, holding `to` afterwards, with a
/// vibrato around wherever it is. The phase is accumulated from the instantaneous
/// frequency, so the waveform never jumps however fast the frequency moves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glide {
    pub wave: Wave,
    pub from: f32,
    pub to: f32,
    pub seconds: f32,
    pub curve: Curve,
    pub vibrato: Vibrato,
}

impl Glide {
    /// The instantaneous frequency `seconds` after the sound starts.
    pub(super) fn hz_at(self, seconds: f64) -> f64 {
        let (from, to) = (f64::from(self.from), f64::from(self.to));
        let progress = (seconds / f64::from(self.seconds)).min(1.0);
        let centre = match self.curve {
            Curve::Linear => from + (to - from) * progress,
            Curve::Exponential => from * (to / from).powf(progress),
        };
        let opened = if self.vibrato.onset > 0.0 {
            (seconds / f64::from(self.vibrato.onset)).min(1.0)
        } else {
            1.0
        };
        let swing = (TAU * f64::from(self.vibrato.hz) * seconds).sin();
        centre * (1.0 + f64::from(self.vibrato.depth) * opened * swing)
    }

    /// The highest frequency the glide can reach, vibrato included.
    pub(super) fn peak(self) -> f32 {
        self.from.max(self.to) * (1.0 + self.vibrato.depth)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FilterKind {
    Low,
    High,
    Band,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Filter {
    pub kind: FilterKind,
    pub hz: f32,
    pub q: f32,
}

/// Attack/decay/sustain followed by an explicit release. Times are seconds, never samples.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Envelope {
    pub attack: f32,
    pub decay: f32,
    pub sustain: f32,
    pub release: f32,
}

impl Envelope {
    pub(super) fn held(self, seconds: f64) -> f64 {
        if seconds < f64::from(self.attack) {
            seconds / f64::from(self.attack)
        } else if self.decay > 0.0 && seconds < f64::from(self.attack + self.decay) {
            let progress = (seconds - f64::from(self.attack)) / f64::from(self.decay);
            1.0 + (f64::from(self.sustain) - 1.0) * progress
        } else {
            f64::from(self.sustain)
        }
    }
}

fn shape(wave: Wave, phase: f64) -> f64 {
    match wave {
        Wave::Sine => (TAU * phase).sin(),
        Wave::Saw => 2.0 * phase - 1.0,
        Wave::Square => {
            if phase < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
        Wave::Triangle => 1.0 - 4.0 * (phase - 0.5).abs(),
    }
}

#[derive(Debug)]
pub(super) struct Generator {
    exciter: Exciter,
    phase: f64,
    step: f64,
    random: u64,
    brown: f64,
    pole: f64,
    rate: f64,
    elapsed: u64,
}

impl Generator {
    pub(super) fn new(exciter: Exciter, seed: u64, rate: u32) -> Self {
        let step = match exciter {
            Exciter::Oscillator { hz, .. } => f64::from(hz) / f64::from(rate),
            Exciter::Noise(_) | Exciter::Glide(_) => 0.0,
        };
        Self {
            exciter,
            phase: 0.0,
            step,
            random: seed,
            brown: 0.0,
            pole: 1.0 - (-TAU * 40.0 / f64::from(rate)).exp(),
            rate: f64::from(rate),
            elapsed: 0,
        }
    }

    pub(super) fn next(&mut self) -> f64 {
        match self.exciter {
            Exciter::Oscillator { wave, .. } => {
                let phase = self.phase;
                self.phase = (phase + self.step).fract();
                shape(wave, phase)
            }
            Exciter::Glide(glide) => {
                let phase = self.phase;
                let hz = glide.hz_at(self.elapsed as f64 / self.rate);
                self.elapsed = self.elapsed.saturating_add(1);
                self.phase = (phase + hz / self.rate).fract();
                shape(glide.wave, phase)
            }
            Exciter::Noise(kind) => {
                // SplitMix64: an explicitly specified integer stream, including seed zero.
                // Its state advances forever; no render-buffer boundary resets it.
                self.random = self.random.wrapping_add(0x9e3779b97f4a7c15);
                let mut value = self.random;
                value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
                value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
                value ^= value >> 31;
                let white = (value >> 11) as f64 * (2.0 / ((1u64 << 53) as f64)) - 1.0;
                match kind {
                    Noise::White => white,
                    Noise::Brown => {
                        self.brown += self.pole * (white - self.brown);
                        self.brown
                    }
                }
            }
        }
    }
}

/// Normalized biquad, transposed direct form II. Coefficients follow the W3C Audio EQ
/// Cookbook, https://www.w3.org/TR/audio-eq-cookbook/ (band-pass uses unity peak gain).
#[derive(Debug)]
pub(super) struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    pub(super) fn new(filter: Filter, rate: u32) -> Self {
        let omega = TAU * f64::from(filter.hz) / f64::from(rate);
        let cos = omega.cos();
        let alpha = omega.sin() / (2.0 * f64::from(filter.q));
        let b = match filter.kind {
            FilterKind::Low => [(1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0],
            FilterKind::High => [(1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0],
            FilterKind::Band => [alpha, 0.0, -alpha],
        };
        Self {
            b: b.map(|v| v / (1.0 + alpha)),
            a: [-2.0 * cos / (1.0 + alpha), (1.0 - alpha) / (1.0 + alpha)],
            z: [0.0; 2],
        }
    }

    pub(super) fn next(&mut self, input: f64) -> f64 {
        let output = self.b[0] * input + self.z[0];
        self.z[0] = self.b[1] * input - self.a[0] * output + self.z[1];
        self.z[1] = self.b[2] * input - self.a[1] * output;
        output
    }
}
