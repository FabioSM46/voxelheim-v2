//! Short descriptions compiled at the output rate. No attack request reaches this catalogue.
use crate::{
    audio::synth::{Envelope, Exciter, Filter, FilterKind, Layer, Noise, Sound, Wave},
    net::{BlowTarget, MobKind},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Cue {
    DryImpact,
    BeastImpact,
    ClothImpact,
    SoftImpact,
    DraugrNotice,
    DraugrAttack,
    VargrNotice,
    VargrAttack,
}

pub(super) const CUES: [Cue; 8] = [
    Cue::DryImpact,
    Cue::BeastImpact,
    Cue::ClothImpact,
    Cue::SoftImpact,
    Cue::DraugrNotice,
    Cue::DraugrAttack,
    Cue::VargrNotice,
    Cue::VargrAttack,
];

pub(super) fn impact(target: BlowTarget) -> Cue {
    match target {
        BlowTarget::Player => Cue::ClothImpact,
        BlowTarget::Mob(kind) => match kind {
            MobKind::Draugr => Cue::DryImpact,
            MobKind::Vargr => Cue::BeastImpact,
            MobKind::Villager => Cue::ClothImpact,
            MobKind::Deer | MobKind::Horse => Cue::SoftImpact,
        },
    }
}

pub(super) fn voice(kind: MobKind, windup: bool) -> Option<Cue> {
    match (kind, windup) {
        (MobKind::Draugr, false) => Some(Cue::DraugrNotice),
        (MobKind::Draugr, true) => Some(Cue::DraugrAttack),
        (MobKind::Vargr, false) => Some(Cue::VargrNotice),
        (MobKind::Vargr, true) => Some(Cue::VargrAttack),
        // These species have no combat telegraph voice: civilians and mounts do not
        // belong to this hostile voice catalogue. Their physical impacts still play.
        (MobKind::Deer | MobKind::Villager | MobKind::Horse, _) => None,
    }
}

impl Cue {
    pub fn seconds(self) -> f32 {
        match self {
            Self::DryImpact => 0.16,
            Self::BeastImpact => 0.18,
            Self::ClothImpact => 0.12,
            Self::SoftImpact => 0.14,
            Self::DraugrNotice => 0.48,
            Self::DraugrAttack => 0.27,
            Self::VargrNotice => 0.40,
            Self::VargrAttack => 0.18,
        }
    }

    pub fn describe(self) -> Sound {
        // Sine/noise transients make impacts; rough tones are reserved for creature
        // voices. No player grunt is synthesized by the cloth-and-body contact cue.
        let (hz, tone, noise, cutoff, wave) = match self {
            Self::DryImpact => (913.0, 0.18, 0.42, 2700.0, Wave::Sine),
            Self::BeastImpact => (117.0, 0.30, 0.29, 850.0, Wave::Sine),
            Self::ClothImpact => (183.0, 0.12, 0.36, 1550.0, Wave::Sine),
            Self::SoftImpact => (91.0, 0.16, 0.24, 620.0, Wave::Sine),
            Self::DraugrNotice => (73.0, 0.24, 0.30, 1100.0, Wave::Triangle),
            Self::DraugrAttack => (103.0, 0.18, 0.42, 1800.0, Wave::Triangle),
            Self::VargrNotice => (157.0, 0.32, 0.22, 650.0, Wave::Triangle),
            Self::VargrAttack => (281.0, 0.30, 0.37, 2200.0, Wave::Triangle),
        };
        let envelope = Envelope {
            attack: 0.004,
            decay: self.seconds() - 0.012,
            sustain: 0.0,
            release: 0.008,
        };
        Sound {
            layers: vec![
                Layer {
                    exciter: Exciter::Oscillator { wave, hz },
                    gain: tone,
                    envelope,
                    filter: None,
                },
                Layer {
                    exciter: Exciter::Noise(Noise::White),
                    gain: noise,
                    envelope,
                    filter: Some(Filter {
                        kind: FilterKind::Band,
                        hz: cutoff,
                        q: 0.7,
                    }),
                },
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_and_hostile_species_render_distinct_bounded_sounds() {
        for rate in [8000, 44100, 48000, 192000] {
            let samples: Vec<_> = CUES
                .iter()
                .map(|cue| {
                    let baked = cue.describe().bake(cue.seconds(), rate, 3).unwrap();
                    assert_eq!(baked.samples().first(), Some(&0.0));
                    assert_eq!(baked.samples().last(), Some(&0.0));
                    assert!(
                        baked
                            .samples()
                            .iter()
                            .all(|v| v.is_finite() && v.abs() <= 1.0)
                    );
                    assert!(baked.samples().iter().any(|v| v.abs() > 0.05));
                    baked
                })
                .collect();
            for (index, a) in samples.iter().enumerate() {
                for b in &samples[index + 1..] {
                    let length = a.samples().len().min(b.samples().len());
                    let difference = a.samples()[..length]
                        .iter()
                        .zip(b.samples())
                        .map(|(a, b)| (a - b).abs())
                        .sum::<f32>()
                        / length as f32;
                    assert!(difference > 0.01, "two catalogue entries sound alike");
                }
            }
        }
    }

    #[test]
    fn passive_species_have_impacts_but_no_combat_voice() {
        for kind in [MobKind::Deer, MobKind::Villager, MobKind::Horse] {
            assert_eq!(voice(kind, false), None);
            assert_eq!(voice(kind, true), None);
            let cue = impact(BlowTarget::Mob(kind));
            assert!(cue.describe().bake(cue.seconds(), 8000, 1).is_ok());
        }
        assert_ne!(
            impact(BlowTarget::Player),
            impact(BlowTarget::Mob(MobKind::Draugr))
        );
        assert_ne!(
            impact(BlowTarget::Player),
            impact(BlowTarget::Mob(MobKind::Vargr))
        );
    }
}
