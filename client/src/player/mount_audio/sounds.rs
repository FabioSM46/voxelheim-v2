//! A hoof is one implement striking many grounds, so the strike is `tool_audio`'s shape:
//! a keratin knock shared by every footfall, and the ground's own response on top of it.
//! No asset files, and the classes are the palette's rather than a second opinion about
//! block ids.
use crate::{
    audio::synth::{Envelope, Exciter, Filter, FilterKind, Layer, Noise, Sound, Wave},
    world::palette::MaterialClass,
};

pub(super) const HOOF_SECONDS: f32 = 0.16;
pub(super) const WHINNY_SECONDS: f32 = 1.25;

fn noise(gain: f32, attack: f32, decay: f32, hz: f32, kind: FilterKind) -> Layer {
    Layer {
        exciter: Exciter::Noise(Noise::White),
        gain,
        envelope: Envelope {
            attack,
            decay,
            sustain: 0.0,
            release: 0.015,
        },
        filter: Some(Filter { kind, hz, q: 0.7 }),
    }
}

/// A sine with the strike's soft onset: four milliseconds, never the two of a hammer.
fn tone(hz: f32, gain: f32, decay: f32) -> Layer {
    Layer {
        exciter: Exciter::Oscillator {
            wave: Wave::Sine,
            hz,
        },
        gain,
        envelope: Envelope {
            attack: 0.004,
            decay,
            sustain: 0.0,
            release: 0.015,
        },
        filter: None,
    }
}

/// White noise through a band of its own width. [`noise`]'s fixed `q` of 0.7 is so wide that
/// its skirts carry a clop's energy far above and below the middle it is centred on.
fn band(gain: f32, attack: f32, decay: f32, hz: f32, q: f32) -> Layer {
    Layer {
        filter: Some(Filter {
            kind: FilterKind::Band,
            hz,
            q,
        }),
        ..noise(gain, attack, decay, hz, FilterKind::Band)
    }
}

pub(super) fn hoof(ground: MaterialClass) -> Sound {
    // How long the hoof's own clop rings: rock gives it nothing to sink into.
    let ring = match ground {
        MaterialClass::Stone | MaterialClass::Glass => 0.03,
        _ => 0.05,
    };
    // The hoof itself: a soft, dull clop. Two narrow bands of noise in the middle of the
    // spectrum, opened over four or five milliseconds so the onset is short without being a
    // click, over a body far too quiet and too high to thump.
    let mut layers = vec![
        band(0.55, 0.004, ring, 1000.0, 1.6),
        band(0.30, 0.005, ring * 0.8, 620.0, 1.2),
        tone(280.0, 0.05, ring * 0.7),
    ];
    layers.extend(match ground {
        // A shod hoof on rock: brighter and shorter than any other ground, with a small ring.
        MaterialClass::Stone | MaterialClass::Glass => vec![
            tone(1450.0, 0.10, 0.03),
            tone(2390.0, 0.06, 0.02),
            noise(0.16, 0.003, 0.015, 3000.0, FilterKind::High),
        ],
        // Earth and grass take the clop in: a low, short band that muffles it.
        MaterialClass::Earth => vec![band(0.35, 0.005, 0.035, 420.0, 1.0)],
        // A soft hiss of grains moving under the hoof.
        MaterialClass::Sand => vec![noise(0.35, 0.010, 0.07, 2600.0, FilterKind::Band)],
        // A hollow clop over a floor with air under it.
        MaterialClass::Wood => vec![tone(330.0, 0.16, 0.06), tone(610.0, 0.08, 0.04)],
        MaterialClass::Foliage => vec![noise(0.30, 0.008, 0.09, 1800.0, FilterKind::High)],
        // Neither is solid, so the ground probe never answers them. Total over the enum all
        // the same, so a later class cannot silently take the wrong arm.
        MaterialClass::Air | MaterialClass::Water => vec![band(0.20, 0.010, 0.07, 700.0, 0.7)],
    });
    Sound { layers }
}

/// One step of the call: a nasal, buzzing voice. Two saws a percent apart beat at about
/// ten hertz, which is the flutter a whinny has, and a band at the nose shapes the buzz.
fn call(hz: f32, onset: f32, decay: f32) -> [Layer; 2] {
    let voice = |hz| Layer {
        exciter: Exciter::Oscillator {
            wave: Wave::Saw,
            hz,
        },
        gain: 0.22,
        envelope: Envelope {
            attack: onset,
            decay,
            sustain: 0.0,
            release: 0.03,
        },
        filter: Some(Filter {
            kind: FilterKind::Band,
            hz: 1300.0,
            q: 0.9,
        }),
    };
    [voice(hz), voice(hz * 1.012)]
}

pub(super) fn whinny() -> Sound {
    // The synthesiser has no glide, so the fall of the call is four steps whose envelopes
    // peak one after another — each attack is where that step is loudest — from a high
    // squeal down to the throaty end, over a breath that carries the whole of it.
    let mut layers = Vec::with_capacity(10);
    for (hz, onset, decay) in [
        (1180.0, 0.06, 0.22),
        (1010.0, 0.20, 0.26),
        (840.0, 0.36, 0.30),
        (640.0, 0.55, 0.35),
    ] {
        layers.extend(call(hz, onset, decay));
    }
    layers.push(noise(0.18, 0.04, 0.90, 2200.0, FilterKind::Band));
    layers.push(Layer {
        exciter: Exciter::Noise(Noise::Brown),
        gain: 0.35,
        envelope: Envelope {
            attack: 0.85,
            decay: 0.25,
            sustain: 0.0,
            release: 0.03,
        },
        filter: Some(Filter {
            kind: FilterKind::Low,
            hz: 400.0,
            q: 0.7,
        }),
    });
    Sound { layers }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::MiningTool;
    use crate::player::tool_audio::sounds::{STRIKE_SECONDS, strike};
    use crate::world::palette::{self, DIRT, LEAVES, PLANKS, SAND, STONE};

    const GROUNDS: [MaterialClass; 8] = [
        MaterialClass::Air,
        MaterialClass::Stone,
        MaterialClass::Earth,
        MaterialClass::Sand,
        MaterialClass::Wood,
        MaterialClass::Foliage,
        MaterialClass::Glass,
        MaterialClass::Water,
    ];
    const RATE: u32 = 48_000;

    /// Energy per DFT bin from 0 Hz to Nyquist, one bin per `1 / duration` hertz. Goertzel's
    /// recurrence per bin rather than an FFT: no crate, and the length need not be a power of
    /// two. Every baked sound starts and ends at exact silence, so the rectangular window
    /// leaks nothing a Hann window would have saved.
    fn spectrum(samples: &[f32]) -> Vec<(f32, f64)> {
        let n = samples.len();
        (0..n / 2)
            .map(|bin| {
                let omega = std::f64::consts::TAU * bin as f64 / n as f64;
                let coefficient = 2.0 * omega.cos();
                let (mut s1, mut s2) = (0.0f64, 0.0f64);
                for &x in samples {
                    let s0 = f64::from(x) + coefficient * s1 - s2;
                    s2 = s1;
                    s1 = s0;
                }
                let power = s1 * s1 + s2 * s2 - coefficient * s1 * s2;
                (bin as f32 * RATE as f32 / n as f32, power)
            })
            .collect()
    }
    /// The spectrum's centre of mass, in hertz: where a strike's energy sits on average.
    fn centroid(spectrum: &[(f32, f64)]) -> f32 {
        let total: f64 = spectrum.iter().map(|(_, power)| power).sum();
        (spectrum
            .iter()
            .map(|(hz, power)| f64::from(*hz) * power)
            .sum::<f64>()
            / total) as f32
    }
    /// The fraction of a strike's energy below `hz`.
    fn share_below(spectrum: &[(f32, f64)], hz: f32) -> f32 {
        let total: f64 = spectrum.iter().map(|(_, power)| power).sum();
        (spectrum
            .iter()
            .filter(|(bin, _)| *bin < hz)
            .map(|(_, power)| power)
            .sum::<f64>()
            / total) as f32
    }
    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0, |peak, x| x.abs().max(peak))
    }

    /// When a strike's energy arrives on average, in seconds from its start.
    fn mean_time(samples: &[f32]) -> f32 {
        samples
            .iter()
            .enumerate()
            .map(|(index, x)| index as f32 * x * x)
            .sum::<f32>()
            / energy(samples)
            / RATE as f32
    }

    /// #1161: every footfall was a bass-drum thump. A 140 Hz knock and noise low-passed at
    /// 900 Hz on every ground, and on earth 300 Hz noise and a 70 Hz sine on top: #1143's
    /// strike measured a 111 Hz centroid on earth with 97% of its energy under 200 Hz, and
    /// 35-93% on every other ground. Now the darkest ground, wood, sits at 666 Hz and no
    /// ground puts more than 0.4% under 200 Hz.
    #[test]
    fn every_ground_strikes_a_soft_clop_in_the_middle_of_the_spectrum_rather_than_a_drum() {
        for ground in GROUNDS {
            let sound = hoof(ground);
            for layer in &sound.layers {
                // No hard onset anywhere in the strike, and no sine low enough to thump.
                assert!(layer.envelope.attack >= 0.003, "{ground:?}: {layer:?}");
                if let Exciter::Oscillator { hz, .. } = layer.exciter {
                    assert!(hz >= 200.0, "{ground:?}: a {hz} Hz knock");
                }
            }
            let spectrum = spectrum(&samples(sound, HOOF_SECONDS));
            let centroid = centroid(&spectrum);
            let low = share_below(&spectrum, 200.0);
            assert!(centroid > 500.0, "{ground:?}: centroid at {centroid} Hz");
            assert!(low < 0.02, "{ground:?}: {low} of the energy under 200 Hz");
        }
    }

    /// Softer than an implement striking the same ground — the quietest of the four, so the
    /// hoof is never the loudest thing a player at work hears — and still heard: the energy
    /// above 200 Hz, where an ear and a laptop speaker both are, measures 6.4 on earth and
    /// more on every other ground.
    #[test]
    fn a_hoof_is_heard_but_strikes_softer_than_any_tool_on_the_same_ground() {
        for ground in GROUNDS {
            let hoof = samples(hoof(ground), HOOF_SECONDS);
            let softest = [
                MiningTool::Hand,
                MiningTool::Shovel,
                MiningTool::Pickaxe,
                MiningTool::Axe,
            ]
            .map(|tool| peak(&samples(strike(tool, ground), STRIKE_SECONDS)))
            .into_iter()
            .fold(f32::INFINITY, f32::min);
            assert!(
                peak(&hoof) < softest * 0.8,
                "{ground:?}: a hoof peaks at {}, the softest tool at {softest}",
                peak(&hoof)
            );
            let audible = energy(&hoof) * (1.0 - share_below(&spectrum(&hoof), 200.0));
            assert!(audible > 4.0, "{ground:?}: {audible} above 200 Hz");
        }
    }

    #[test]
    fn rock_rings_brighter_and_shorter_than_earth() {
        let stone = samples(hoof(MaterialClass::Stone), HOOF_SECONDS);
        let earth = samples(hoof(MaterialClass::Earth), HOOF_SECONDS);
        assert!(centroid(&spectrum(&stone)) > centroid(&spectrum(&earth)) * 2.0);
        assert!(
            mean_time(&stone) < mean_time(&earth),
            "stone {} s, earth {} s",
            mean_time(&stone),
            mean_time(&earth)
        );
    }

    fn samples(sound: Sound, seconds: f32) -> Vec<f32> {
        sound.bake(seconds, 48_000, 7).unwrap().samples().to_vec()
    }
    fn difference(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(a, b)| (a - b).powi(2)).sum::<f32>()
    }
    fn energy(samples: &[f32]) -> f32 {
        samples.iter().map(|x| x * x).sum()
    }

    #[test]
    fn five_canonical_grounds_strike_distinctly() {
        let strikes: Vec<_> = [STONE, DIRT, PLANKS, LEAVES, SAND]
            .into_iter()
            .map(|id| samples(hoof(palette::material_class(id)), HOOF_SECONDS))
            .collect();
        for (i, a) in strikes.iter().enumerate() {
            for b in strikes.iter().skip(i + 1) {
                assert!(difference(a, b) > 1.0);
            }
        }
    }

    #[test]
    fn a_whinny_outlasts_a_strike_and_does_not_open_like_one() {
        let whinny = samples(whinny(), WHINNY_SECONDS);
        let strike = samples(hoof(MaterialClass::Earth), HOOF_SECONDS);
        // Past the end of any strike, the call is still sounding.
        assert!(energy(&whinny[strike.len() * 3..]) > 1.0);
        assert!(difference(&whinny[..strike.len()], &strike) > 1.0);
    }

    #[test]
    fn catalogue_is_bounded_at_every_supported_device_rate() {
        for rate in [8000, 44100, 48000, 192000] {
            let mut sounds = vec![(whinny(), WHINNY_SECONDS)];
            for ground in [
                MaterialClass::Air,
                MaterialClass::Stone,
                MaterialClass::Earth,
                MaterialClass::Sand,
                MaterialClass::Wood,
                MaterialClass::Foliage,
                MaterialClass::Glass,
                MaterialClass::Water,
            ] {
                sounds.push((hoof(ground), HOOF_SECONDS));
            }
            for (sound, seconds) in sounds {
                let baked = sound.bake(seconds, rate, 17).unwrap();
                assert!(
                    baked
                        .samples()
                        .iter()
                        .all(|x| x.is_finite() && x.abs() <= 1.0)
                );
                assert_eq!(baked.samples().first(), Some(&0.0));
                assert_eq!(baked.samples().last(), Some(&0.0));
            }
        }
    }
}
