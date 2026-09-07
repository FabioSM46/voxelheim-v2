//! Excitation describes the implement; the struck material adds its own response.
//! No asset files, pitch-only variants, or second block-id classification.
use crate::{
    audio::synth::{Envelope, Exciter, Filter, FilterKind, Layer, Noise, Sound, Wave},
    net::MiningTool,
    world::palette::MaterialClass,
};

pub(super) const STRIKE_SECONDS: f32 = 0.24;
pub(super) const BREAK_SECONDS: f32 = 0.38;
pub(super) const SWING_SECONDS: f32 = 0.20;

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

pub(super) fn strike(tool: MiningTool, material: MaterialClass) -> Sound {
    // Hand: soft low thud. Shovel: broadband scrape with a hollow plate transient.
    // Pick: long inharmonic metal ring. Axe: short woody knock with a cutting edge.
    let mut layers = match tool {
        MiningTool::Hand => vec![
            tone(95.0, 0.34, 0.065),
            noise(0.30, 0.008, 0.10, 550.0, FilterKind::Low),
        ],
        MiningTool::Shovel => vec![
            noise(0.55, 0.025, 0.15, 1150.0, FilterKind::Band),
            tone(360.0, 0.19, 0.06),
        ],
        MiningTool::Pickaxe => vec![
            tone(1800.0, 0.21, 0.20),
            tone(2741.0, 0.12, 0.14),
            noise(0.24, 0.001, 0.025, 2400.0, FilterKind::High),
        ],
        MiningTool::Axe => vec![
            tone(220.0, 0.30, 0.055),
            noise(0.43, 0.002, 0.09, 1500.0, FilterKind::Low),
        ],
    };
    // These are opinions about canonical classes, not opinions about block ids.
    layers.extend(match material {
        MaterialClass::Stone | MaterialClass::Glass => {
            vec![tone(1237.0, 0.20, 0.17), tone(2131.0, 0.13, 0.11)]
        }
        MaterialClass::Earth => vec![noise(0.65, 0.002, 0.07, 400.0, FilterKind::Low)],
        MaterialClass::Wood => vec![tone(410.0, 0.27, 0.10), tone(735.0, 0.12, 0.06)],
        MaterialClass::Foliage => vec![noise(0.60, 0.025, 0.19, 1900.0, FilterKind::High)],
        MaterialClass::Sand => vec![noise(0.70, 0.045, 0.17, 2800.0, FilterKind::Band)],
        // Neither is normally mineable. A future authoritative observation stays soft.
        MaterialClass::Air | MaterialClass::Water => {
            vec![noise(0.30, 0.025, 0.16, 650.0, FilterKind::Low)]
        }
    });
    Sound { layers }
}

pub(super) fn breaking(tool: MiningTool, material: MaterialClass) -> Sound {
    let mut sound = strike(tool, material);
    // The longer collapse has a rising granular wash and a low body after the attack.
    sound
        .layers
        .push(noise(0.65, 0.09, 0.26, 1400.0, FilterKind::Low));
    sound.layers.push(tone(65.0, 0.23, 0.29));
    sound
}

pub(super) fn swing(weapon: bool) -> Sound {
    Sound {
        layers: if weapon {
            vec![
                noise(0.68, 0.055, 0.125, 1500.0, FilterKind::Band),
                noise(0.20, 0.025, 0.07, 2600.0, FilterKind::High),
            ]
        } else {
            vec![noise(0.60, 0.028, 0.095, 480.0, FilterKind::Low)]
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::palette::{self, DIRT, LOG, SAND, STONE};

    fn samples(sound: Sound, seconds: f32) -> Vec<f32> {
        sound.bake(seconds, 48_000, 7).unwrap().samples().to_vec()
    }
    fn difference(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(a, b)| (a - b).powi(2)).sum::<f32>()
    }
    #[test]
    fn implements_have_different_exciters_envelopes_and_rendered_waveforms() {
        let tools = [MiningTool::Hand, MiningTool::Shovel, MiningTool::Pickaxe];
        for (i, a) in tools.iter().enumerate() {
            for b in tools.iter().skip(i + 1) {
                let a = strike(*a, MaterialClass::Earth);
                let b = strike(*b, MaterialClass::Earth);
                assert_ne!(a.layers[0].envelope, b.layers[0].envelope);
                assert_ne!(a.layers[0].exciter, b.layers[0].exciter);
                assert!(
                    difference(&samples(a, STRIKE_SECONDS), &samples(b, STRIKE_SECONDS)) > 10.0
                );
            }
        }
    }
    #[test]
    fn five_canonical_materials_render_distinctly_for_each_implement() {
        for tool in [
            MiningTool::Hand,
            MiningTool::Shovel,
            MiningTool::Pickaxe,
            MiningTool::Axe,
        ] {
            let blocks = [STONE, DIRT, LOG, palette::LEAVES, SAND];
            let sounds: Vec<_> = blocks
                .into_iter()
                .map(|id| samples(strike(tool, palette::material_class(id)), STRIKE_SECONDS))
                .collect();
            for (i, a) in sounds.iter().enumerate() {
                for b in sounds.iter().skip(i + 1) {
                    assert!(difference(a, b) > 1.0, "{tool:?}");
                }
            }
        }
        assert_eq!(
            crate::audio::spatial::occlusion_weight(palette::material_class(SAND)),
            crate::audio::spatial::occlusion_weight(palette::material_class(DIRT))
        );
    }
    #[test]
    fn collapse_has_energy_after_the_strike_and_swings_are_not_impacts() {
        let strike = samples(
            strike(MiningTool::Hand, MaterialClass::Earth),
            STRIKE_SECONDS,
        );
        let broken = samples(
            breaking(MiningTool::Hand, MaterialClass::Earth),
            BREAK_SECONDS,
        );
        assert!(broken[strike.len()..].iter().map(|x| x * x).sum::<f32>() > 0.1);
        let hand = samples(swing(false), SWING_SECONDS);
        let weapon = samples(swing(true), SWING_SECONDS);
        assert!(difference(&hand, &weapon) > 1.0);
        assert!(difference(&hand, &strike) > 1.0);
    }
    #[test]
    fn catalogue_is_bounded_at_every_supported_device_rate() {
        for rate in [8000, 44100, 48000, 192000] {
            for tool in [
                MiningTool::Hand,
                MiningTool::Shovel,
                MiningTool::Pickaxe,
                MiningTool::Axe,
            ] {
                for material in [
                    MaterialClass::Air,
                    MaterialClass::Stone,
                    MaterialClass::Earth,
                    MaterialClass::Sand,
                    MaterialClass::Wood,
                    MaterialClass::Foliage,
                    MaterialClass::Glass,
                    MaterialClass::Water,
                ] {
                    for (sound, seconds) in [
                        (strike(tool, material), STRIKE_SECONDS),
                        (breaking(tool, material), BREAK_SECONDS),
                        (swing(false), SWING_SECONDS),
                        (swing(true), SWING_SECONDS),
                    ] {
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
    }
}
