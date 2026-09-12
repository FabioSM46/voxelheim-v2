//! A splash is not a note, and it is not a drum either: it is a body's worth of water
//! thrown into the air and falling back. Broadband from the start, brightest in the first
//! few milliseconds, and settling into a low gurgle with bubbles rising out of it.
//!
//! **Every frequency here stays at or under 3.6 kHz.** That is `rate * 0.45` at the lowest
//! device rate the synthesiser accepts (`audio::synth::Sound::validate`), so a description
//! that reaches higher would refuse to bake on an 8 kHz device and the sound would simply
//! not exist there. A splash's brightness therefore has to be made under that ceiling
//! rather than above it.
//!
//! No asset files, and nothing here reads a block id: `mod.rs` asks the palette whether a
//! voxel is water and this file is only told how hard the body arrived.
use crate::audio::synth::{
    Curve, Envelope, Exciter, Filter, FilterKind, Glide, Layer, Noise, Sound, Vibrato, Wave,
};

/// How long an entry splash sounds for. The mounted entry is longer because the water it
/// throws takes longer to come back down.
pub(super) const SPLASH_SECONDS: f32 = 0.9;
pub(super) const MOUNTED_SPLASH_SECONDS: f32 = 1.2;

/// How hard a body broke the surface.
///
/// **Three steps rather than a continuous size, and the quantisation is deliberate.** A
/// baked description is cached by its cue, so a splash whose description varied with the
/// exact speed would bake a new buffer on every entry. Three steps span the range the
/// acceptance criterion asks for — a step in off a bank is not a dive off a cliff — and
/// the mapping from speed to step is [`Force::of`], which is what a test reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Force {
    /// Walking in, or sinking back after bobbing at the surface.
    Step,
    /// A drop of a block or two: off a bank, off a boat, down a shelf.
    Fall,
    /// A long fall. The loudest entry there is.
    Dive,
}

/// Downward speeds, in blocks per second, at or above which an entry is a [`Force::Fall`]
/// and a [`Force::Dive`]. Below the first, an entry is a [`Force::Step`] however slowly it
/// happened — walking in off a bank still displaces a body's worth of water.
const FALL_SPEED: f32 = 3.5;
const DIVE_SPEED: f32 = 9.0;

impl Force {
    /// Which step a body travelling `down` blocks per second entered at. A body moving
    /// upward or sideways into water enters at [`Force::Step`]: `down` is negative there,
    /// and neither comparison holds.
    pub(super) fn of(down: f32) -> Self {
        if down >= DIVE_SPEED {
            Self::Dive
        } else if down >= FALL_SPEED {
            Self::Fall
        } else {
            Self::Step
        }
    }

    /// How much water this entry throws, as a multiplier on the splash's gains. Never
    /// above one: a layer's gain is bounded by the synthesiser, and the whole of the
    /// loudest splash is written at the gains a dive uses.
    fn weight(self) -> f32 {
        match self {
            Self::Step => 0.40,
            Self::Fall => 0.70,
            Self::Dive => 1.0,
        }
    }
}

/// Noise through one filter, with an explicit envelope. Every splash layer is this or a
/// bubble: there is no clean partial anywhere in a splash.
fn noise(kind: Noise, gain: f32, attack: f32, decay: f32, filter: Filter) -> Layer {
    Layer {
        exciter: Exciter::Noise(kind),
        gain,
        envelope: Envelope {
            attack,
            decay,
            sustain: 0.0,
            release: 0.02,
        },
        filter: Some(filter),
    }
}

fn band(hz: f32, q: f32) -> Filter {
    Filter {
        kind: FilterKind::Band,
        hz,
        q,
    }
}

fn high(hz: f32) -> Filter {
    Filter {
        kind: FilterKind::High,
        hz,
        q: 0.7,
    }
}

fn low(hz: f32) -> Filter {
    Filter {
        kind: FilterKind::Low,
        hz,
        q: 0.7,
    }
}

/// One bubble rising out of the gurgle: a pitch that climbs as the bubble shrinks toward
/// the surface, through a band narrow enough to be a plink and made rough by a saw's
/// harmonics walking across it. A sine here would be a note, which is the one thing a
/// water sound must not be.
fn bubble(from: f32, gain: f32, onset: f32, seconds: f32) -> Layer {
    Layer {
        exciter: Exciter::Glide(Glide {
            wave: Wave::Saw,
            from,
            to: from * 1.6,
            seconds,
            curve: Curve::Exponential,
            vibrato: Vibrato {
                hz: 23.0,
                depth: 0.08,
                onset: 0.0,
            },
        }),
        gain,
        envelope: Envelope {
            attack: onset.max(0.004),
            decay: seconds,
            sustain: 0.0,
            release: 0.02,
        },
        filter: Some(band(from * 1.3, 2.4)),
    }
}

/// The splash a body makes breaking the surface.
///
/// The shape is the same on foot and mounted, and the two differ in the three ways a
/// heavier body differs: the bright crown of spray is quieter and the low displacement
/// louder, everything takes longer, and the bubbles are bigger — which means lower, since
/// a bigger bubble rings lower.
pub(super) fn splash(mounted: bool, force: Force) -> Sound {
    let weight = force.weight();
    // How much of the entry is the bright crown of spray. A dive throws water high and
    // fast; a step in off a bank mostly just displaces it.
    let crown = match force {
        Force::Step => 0.45,
        Force::Fall => 0.75,
        Force::Dive => 1.0,
    };
    // A heavier body carries its energy lower and holds it longer.
    let (spray, slump, stretch) = if mounted {
        (0.55, 1.0, 1.35)
    } else {
        (1.0, 0.6, 1.0)
    };
    let mut layers = vec![
        // The crown: the first few milliseconds, everything above 2 kHz at once. Opened
        // over three milliseconds so it is a fast onset and not a click.
        noise(
            Noise::White,
            (0.32 * weight * crown * spray).min(1.0),
            0.003,
            0.05 * stretch,
            high(2000.0),
        ),
        // The body of the splash, falling out of the crown into the middle of the
        // spectrum over a fifth of a second.
        noise(
            Noise::White,
            (0.34 * weight * spray.max(0.7)).min(1.0),
            0.004,
            0.18 * stretch,
            band(1500.0, 0.8),
        ),
        noise(
            Noise::White,
            (0.40 * weight).min(1.0),
            0.008,
            0.34 * stretch,
            band(780.0, 0.7),
        ),
        // The gurgle the splash settles into: low, broad and by far the longest part of
        // it. Low-passed rather than banded, because a band's skirts would carry the
        // gurgle brighter than the crown that preceded it.
        noise(
            Noise::White,
            (0.34 * weight * (0.7 + slump)).min(1.0),
            0.03 * stretch,
            0.55 * stretch,
            low(420.0),
        ),
        // The displacement under it all: the mass of water the body shoved aside.
        noise(
            Noise::Brown,
            (0.55 * weight * (0.5 + slump)).min(1.0),
            0.02 * stretch,
            0.45 * stretch,
            low(240.0),
        ),
    ];
    // Bubbles rising out of the gurgle, each starting later than the one before it, and
    // each smaller — so the last one to leave is the highest. A heavier entry takes more
    // water down with it and so lets more bubbles back up.
    let biggest = if mounted { 300.0 } else { 430.0 };
    let count = match force {
        Force::Step => 2,
        Force::Fall => 3,
        Force::Dive => 4,
    };
    for index in 0..count {
        let step = index as f32;
        layers.push(bubble(
            biggest * (1.0 + 0.34 * step),
            (0.13 * weight).min(1.0),
            0.05 + 0.09 * step * stretch,
            0.16 * stretch,
        ));
    }
    Sound { layers }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 8000;
    const FORCES: [Force; 3] = [Force::Step, Force::Fall, Force::Dive];
    /// The highest frequency any description in this file may name. See the module note.
    const CEILING_HZ: f32 = 3600.0;

    fn seconds(mounted: bool) -> f32 {
        if mounted {
            MOUNTED_SPLASH_SECONDS
        } else {
            SPLASH_SECONDS
        }
    }

    fn samples(sound: &Sound, mounted: bool, rate: u32) -> Vec<f32> {
        sound
            .bake(seconds(mounted), rate, 1188)
            .expect("a splash bakes")
            .samples()
            .to_vec()
    }

    fn energy(samples: &[f32]) -> f64 {
        samples.iter().map(|x| f64::from(*x).powi(2)).sum()
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0, |peak, x| x.abs().max(peak))
    }

    /// Energy per DFT bin between `low` and `high` hertz, every `stride`th bin, by
    /// Goertzel's recurrence — the same measurement `ambient_sound::sounds` reads a voice
    /// with, strided because a splash is long and the bins are far narrower than any band
    /// a share is read over.
    fn band_power(samples: &[f32], rate: u32, low: f32, high: f32, stride: usize) -> Vec<f64> {
        let n = samples.len();
        let bin = |hz: f32| (hz * n as f32 / rate as f32).round() as usize;
        (bin(low)..bin(high).min(n / 2))
            .step_by(stride)
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

    /// Spectral flatness from 400 Hz to 3.6 kHz: the geometric mean of the power spectrum
    /// over its arithmetic mean. Near one for a flat spectrum, near zero for clean
    /// partials, whose energy sits on a few bins with nothing between them.
    fn flatness(samples: &[f32], rate: u32) -> f64 {
        let power = band_power(samples, rate, 400.0, 3600.0, 4);
        let mean = power.iter().sum::<f64>() / power.len() as f64;
        let log = power.iter().map(|p| (p + mean * 1e-12).ln()).sum::<f64>() / power.len() as f64;
        log.exp() / mean
    }

    /// The fraction of a splash's energy below `hz`.
    fn share_below(samples: &[f32], rate: u32, hz: f32) -> f64 {
        let whole = band_power(samples, rate, 1.0, 3600.0, 4);
        let under = band_power(samples, rate, 1.0, hz, 4);
        under.iter().sum::<f64>() / whole.iter().sum::<f64>()
    }

    /// When a splash's energy arrives on average, in seconds from its start.
    fn mean_time(samples: &[f32], rate: u32) -> f64 {
        samples
            .iter()
            .enumerate()
            .map(|(index, x)| index as f64 * f64::from(*x).powi(2))
            .sum::<f64>()
            / energy(samples)
            / f64::from(rate)
    }

    /// The negative control: this very splash with every noise layer replaced by a clean
    /// sine at its filter's centre, and every bubble's saw replaced by a sine. Same
    /// layers, same envelopes, same frequencies — and no texture anywhere. It must fail
    /// the measurement the splash passes, or the measurement is not reading texture.
    fn clean_partials(mounted: bool, force: Force) -> Sound {
        let layers = splash(mounted, force)
            .layers
            .into_iter()
            .map(|layer| {
                let hz = layer.filter.map_or(1000.0, |filter| filter.hz).min(3500.0);
                match layer.exciter {
                    Exciter::Noise(_) => Layer {
                        exciter: Exciter::Oscillator {
                            wave: Wave::Sine,
                            hz,
                        },
                        filter: None,
                        ..layer
                    },
                    Exciter::Glide(glide) => Layer {
                        exciter: Exciter::Glide(Glide {
                            wave: Wave::Sine,
                            vibrato: Vibrato::NONE,
                            ..glide
                        }),
                        filter: None,
                        ..layer
                    },
                    Exciter::Oscillator { .. } => layer,
                }
            })
            .collect();
        Sound { layers }
    }

    /// A splash is water, never a note: its energy is spread across the whole band rather
    /// than sitting on a handful of frequencies. The negative control is the point — the
    /// same description voiced as clean partials measures an order of magnitude flatter
    /// and fails the floor this passes.
    #[test]
    fn every_splash_is_broadband_and_a_clean_partial_control_fails_the_measurement() {
        for mounted in [false, true] {
            for force in FORCES {
                let splash = samples(&splash(mounted, force), mounted, RATE);
                let flat = flatness(&splash, RATE);
                assert!(flat > 0.15, "mounted {mounted}, {force:?}: flatness {flat}");
                let control = samples(&clean_partials(mounted, force), mounted, RATE);
                let clean = flatness(&control, RATE);
                assert!(
                    clean < 0.05,
                    "mounted {mounted}, {force:?}: clean partials measured flatness {clean}"
                );
            }
        }
    }

    /// The acceptance criterion in the issue's own words: a step in off a bank is not a
    /// dive off a cliff. Read as energy, which is what a listener hears as size, and as
    /// the peak, which is what they hear as suddenness.
    #[test]
    fn a_splash_grows_with_the_speed_the_body_arrived_at() {
        for mounted in [false, true] {
            let rendered: Vec<_> = FORCES
                .into_iter()
                .map(|force| samples(&splash(mounted, force), mounted, RATE))
                .collect();
            for pair in rendered.windows(2) {
                assert!(
                    energy(&pair[1]) > energy(&pair[0]) * 1.5,
                    "mounted {mounted}: {} then {}",
                    energy(&pair[0]),
                    energy(&pair[1])
                );
                assert!(peak(&pair[1]) > peak(&pair[0]));
            }
        }
    }

    /// The speed-to-step mapping, including the direction a body moving upward into water
    /// takes: the gentlest entry there is, never the loudest.
    #[test]
    fn the_entry_step_follows_the_downward_speed_and_nothing_else() {
        assert_eq!(Force::of(-12.0), Force::Step);
        assert_eq!(Force::of(0.0), Force::Step);
        assert_eq!(Force::of(FALL_SPEED - 0.01), Force::Step);
        assert_eq!(Force::of(FALL_SPEED), Force::Fall);
        assert_eq!(Force::of(DIVE_SPEED - 0.01), Force::Fall);
        assert_eq!(Force::of(DIVE_SPEED), Force::Dive);
        assert_eq!(Force::of(120.0), Force::Dive);
        // A speed that is not a number is the gentlest entry, not the loudest: neither
        // comparison holds against a NaN.
        assert_eq!(Force::of(f32::NAN), Force::Step);
    }

    /// A mounted entry is heard as the heavier one: lower, longer, and measurably a
    /// different sound rather than the same one turned up.
    #[test]
    fn a_mounted_entry_is_heavier_and_longer_than_one_on_foot() {
        for force in FORCES {
            let foot = samples(&splash(false, force), false, RATE);
            let mount = samples(&splash(true, force), true, RATE);
            assert!(
                share_below(&mount, RATE, 500.0) > share_below(&foot, RATE, 500.0) * 1.2,
                "{force:?}: mounted {} under 500 Hz, on foot {}",
                share_below(&mount, RATE, 500.0),
                share_below(&foot, RATE, 500.0)
            );
            assert!(
                mean_time(&mount, RATE) > mean_time(&foot, RATE) * 1.15,
                "{force:?}: mounted at {} s, on foot at {} s",
                mean_time(&mount, RATE),
                mean_time(&foot, RATE)
            );
            // Not the same buffer at another gain: the shortest stretch of either, scaled
            // to the same peak, still differs.
            let span = foot.len().min(mount.len());
            let scale = peak(&foot) / peak(&mount);
            let difference: f64 = foot[..span]
                .iter()
                .zip(&mount[..span])
                .map(|(a, b)| f64::from(a - b * scale).powi(2))
                .sum();
            assert!(difference > energy(&foot[..span]) * 0.25, "{force:?}");
        }
    }

    /// A splash opens bright and settles low: the crown of spray first, the gurgle after.
    #[test]
    fn a_splash_opens_with_spray_and_settles_into_a_gurgle() {
        for mounted in [false, true] {
            let splash = samples(&splash(mounted, Force::Dive), mounted, RATE);
            // The spray's own band — above 1.5 kHz — at the open against a window around
            // the middle, where the gurgle is still sounding. Not the very tail: every
            // layer has decayed to silence there, and a share of no energy is not a
            // number. Read as the bright share rather than the low one because a mounted
            // entry is already low at the open: it measures 0.34 of its energy above
            // 1.5 kHz in the first fiftieth and 0.02 in the middle, where one on foot
            // goes from 0.65 to 0.04.
            let span = splash.len() / 18;
            let middle = splash.len() / 2;
            let bright = |window: &[f32]| 1.0 - share_below(window, RATE, 1500.0);
            let open = bright(&splash[..span]);
            let close = bright(&splash[middle..middle + span * 2]);
            assert!(
                open > close * 3.0,
                "mounted {mounted}: {open} bright at the open, {close} at the close"
            );
        }
    }

    /// Every frequency a description names is at or under 3.6 kHz, so every one of them
    /// bakes at the lowest device rate the synthesiser accepts. The assertion on the
    /// descriptions is what states the rule; the bake at 8 kHz is what would fail if a
    /// later edit broke it in a way the read-back missed.
    #[test]
    fn every_frequency_is_under_the_eight_kilohertz_ceiling() {
        for mounted in [false, true] {
            for force in FORCES {
                let sound = splash(mounted, force);
                for layer in &sound.layers {
                    match layer.exciter {
                        Exciter::Oscillator { hz, .. } => {
                            assert!(hz <= CEILING_HZ, "{mounted} {force:?}: a {hz} Hz partial");
                        }
                        Exciter::Glide(glide) => {
                            // The highest frequency the glide reaches, vibrato included:
                            // the synthesiser's own `Glide::peak`, which is private to
                            // `audio::synth`, restated here rather than exported for a
                            // test.
                            let peak = glide.from.max(glide.to) * (1.0 + glide.vibrato.depth);
                            assert!(
                                peak <= CEILING_HZ,
                                "{mounted} {force:?}: a glide peaking at {peak} Hz"
                            );
                        }
                        Exciter::Noise(_) => {}
                    }
                    if let Some(filter) = layer.filter {
                        assert!(
                            filter.hz <= CEILING_HZ,
                            "{mounted} {force:?}: a filter at {} Hz",
                            filter.hz
                        );
                    }
                }
                assert!(sound.bake(seconds(mounted), 8000, 1188).is_ok());
            }
        }
    }

    /// Bounded, unclipped and silent at both edges at every device rate a player can open,
    /// and loud enough to be heard once the distance has taken its share.
    #[test]
    fn the_catalogue_is_bounded_at_every_supported_device_rate() {
        for mounted in [false, true] {
            for force in FORCES {
                for rate in [8000, 44100, 48000, 96000, 192000] {
                    let sound = splash(mounted, force);
                    let baked = sound
                        .bake(seconds(mounted), rate, 17)
                        .expect("a splash bakes at every rate");
                    let rendered = baked.samples();
                    assert!(rendered.iter().all(|x| x.is_finite() && x.abs() <= 1.0));
                    assert!(
                        peak(rendered) < 0.85,
                        "mounted {mounted}, {force:?} at {rate}: peaks at {}",
                        peak(rendered)
                    );
                    assert!(
                        peak(rendered) > 0.05,
                        "mounted {mounted}, {force:?} at {rate}: peaks at only {}",
                        peak(rendered)
                    );
                    assert_eq!(rendered.first(), Some(&0.0));
                    assert_eq!(rendered.last(), Some(&0.0));
                }
            }
        }
    }
}
