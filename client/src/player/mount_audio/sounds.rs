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

fn tone(hz: f32, gain: f32, decay: f32) -> Layer {
    Layer {
        exciter: Exciter::Oscillator {
            wave: Wave::Sine,
            hz,
        },
        gain,
        envelope: Envelope {
            attack: 0.002,
            decay,
            sustain: 0.0,
            release: 0.015,
        },
        filter: None,
    }
}

pub(super) fn hoof(ground: MaterialClass) -> Sound {
    // The hoof itself: a short low knock and the dull edge of a hard, horny sole.
    let mut layers = vec![
        tone(140.0, 0.30, 0.05),
        noise(0.28, 0.002, 0.045, 900.0, FilterKind::Low),
    ];
    layers.extend(match ground {
        // A shod hoof on rock is the clack everybody knows: short, bright and ringing.
        MaterialClass::Stone | MaterialClass::Glass => vec![
            tone(1450.0, 0.18, 0.05),
            tone(2390.0, 0.10, 0.03),
            noise(0.22, 0.001, 0.02, 3000.0, FilterKind::High),
        ],
        MaterialClass::Earth => vec![
            noise(0.55, 0.003, 0.08, 300.0, FilterKind::Low),
            tone(70.0, 0.25, 0.07),
        ],
        MaterialClass::Sand => vec![noise(0.45, 0.010, 0.10, 2600.0, FilterKind::Band)],
        // A hollow clop over a floor with air under it.
        MaterialClass::Wood => vec![tone(330.0, 0.30, 0.08), tone(610.0, 0.14, 0.05)],
        MaterialClass::Foliage => vec![noise(0.45, 0.008, 0.12, 1800.0, FilterKind::High)],
        // Neither is solid, so the ground probe never answers them. Total over the enum all
        // the same, so a later class cannot silently take the wrong arm.
        MaterialClass::Air | MaterialClass::Water => {
            vec![noise(0.30, 0.010, 0.10, 700.0, FilterKind::Low)]
        }
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
    use crate::world::palette::{self, DIRT, LEAVES, PLANKS, SAND, STONE};

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
