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
    Guardian(super::guardian::sounds::Cue),
    King(super::king::sounds::Cue),
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
            // The bosses take the impact material of the species they share a name with,
            // and that is a fact about what a blade meets rather than a placeholder: a
            // Draugr king is armoured bone and a Vargr guardian is a beast, whatever
            // either turns out to look like. This is the one row of the three this
            // module owns that is *not* deferred to #1019, because an impact plays for
            // a creature nobody has drawn yet — the blow is authoritative and it lands.
            MobKind::Draugr | MobKind::DraugrKing => Cue::DryImpact,
            MobKind::Vargr | MobKind::VargrGuardian => Cue::BeastImpact,
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
        // Both bosses are routed by their explicit encounter phases, never Windup.
        (MobKind::VargrGuardian | MobKind::DraugrKing, _) => None,
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
            Self::Guardian(cue) => cue.seconds(),
            Self::King(cue) => cue.seconds(),
        }
    }

    /// Admission order: boss cues carry their authored priority, generic cues sit at three.
    pub fn priority(self) -> u8 {
        match self {
            Self::Guardian(cue) => cue.priority(),
            Self::King(cue) => cue.priority(),
            _ => 3,
        }
    }

    /// A boss cue that may ring on through later phases of its own move instance. The
    /// king's phases are short at 20 Hz, so each of his gestures owns its instance.
    pub fn tail(self) -> bool {
        matches!(
            self,
            Self::Guardian(super::guardian::sounds::Cue::Landing) | Self::King(_)
        )
    }

    pub fn describe(self) -> Sound {
        match self {
            Self::Guardian(cue) => return cue.describe(),
            Self::King(cue) => return cue.describe(),
            _ => {}
        }
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
            Self::Guardian(_) | Self::King(_) => unreachable!("boss recipes return above"),
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
                    assert!(
                        difference > 0.01,
                        "two catalogue waveforms collapsed to the same recipe"
                    );
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

    /// Bosses speak only through announced phases; the existing impact stays put.
    #[test]
    fn the_bosses_have_no_windup_voice_and_keep_their_impacts() {
        for boss in [MobKind::VargrGuardian, MobKind::DraugrKing] {
            assert_eq!(voice(boss, false), None);
            assert_eq!(voice(boss, true), None);
        }
        assert!(
            CUES.iter()
                .all(|cue| !matches!(cue, Cue::Guardian(_) | Cue::King(_))),
            "boss recipes are baked from their own catalogues"
        );
        assert_eq!(
            impact(BlowTarget::Mob(MobKind::DraugrKing)),
            impact(BlowTarget::Mob(MobKind::Draugr)),
            "a boss meets a blade like the species it shares a name with"
        );
        // And the two bosses are not one material: a beast and a suit of grave-iron.
        assert_ne!(
            impact(BlowTarget::Mob(MobKind::VargrGuardian)),
            impact(BlowTarget::Mob(MobKind::DraugrKing))
        );
    }
}
