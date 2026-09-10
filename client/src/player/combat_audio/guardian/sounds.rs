//! Physical materials and effort, authored as bounded synthesis layers. The catalogue
//! contains gestures, not damage outcomes: a jaw can close without hitting anyone.
use crate::audio::synth::{Envelope, Exciter, Filter, FilterKind, Layer, Noise, Sound, Wave};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in super::super) enum Cue {
    Notice,
    BiteLoad,
    BiteLoadSecond,
    BiteSnap,
    BiteTear,
    ClawLift,
    ClawLeft,
    ClawRight,
    PawSettle,
    Scrape,
    ChainTaut,
    ChainRun,
    ChargeStop,
    LeapLoad,
    LeapLaunch,
    Landing,
    JawsLoad,
    JawsClose,
    Recovery,
    StrapTear,
    Death,
    Footfall,
}

pub(in super::super) const CUES: [Cue; 22] = [
    Cue::Notice,
    Cue::BiteLoad,
    Cue::BiteLoadSecond,
    Cue::BiteSnap,
    Cue::BiteTear,
    Cue::ClawLift,
    Cue::ClawLeft,
    Cue::ClawRight,
    Cue::PawSettle,
    Cue::Scrape,
    Cue::ChainTaut,
    Cue::ChainRun,
    Cue::ChargeStop,
    Cue::LeapLoad,
    Cue::LeapLaunch,
    Cue::Landing,
    Cue::JawsLoad,
    Cue::JawsClose,
    Cue::Recovery,
    Cue::StrapTear,
    Cue::Death,
    Cue::Footfall,
];

fn envelope(attack: f32, decay: f32) -> Envelope {
    Envelope {
        attack,
        decay,
        sustain: 0.0,
        release: 0.018,
    }
}
pub(in super::super) fn tone(hz: f32, gain: f32, attack: f32, decay: f32) -> Layer {
    Layer {
        exciter: Exciter::Oscillator {
            wave: Wave::Sine,
            hz,
        },
        gain,
        envelope: envelope(attack, decay),
        filter: None,
    }
}
pub(in super::super) fn grit(hz: f32, gain: f32, attack: f32, decay: f32, q: f32) -> Layer {
    Layer {
        exciter: Exciter::Noise(Noise::White),
        gain,
        envelope: envelope(attack, decay),
        filter: Some(Filter {
            kind: FilterKind::Band,
            hz,
            q,
        }),
    }
}
pub(in super::super) fn breath(chest: f32, seconds: f32, attack: f32) -> Vec<Layer> {
    vec![
        tone(chest, 0.19, attack, seconds),
        tone(chest * 1.07, 0.11, attack, seconds * 0.8),
        grit(390.0, 0.30, attack, seconds, 0.7),
        grit(930.0, 0.10, attack * 1.3, seconds * 0.7, 1.2),
    ]
}
pub(in super::super) fn iron(attack: f32, decay: f32, gain: f32) -> Vec<Layer> {
    // Inharmonic ringing rather than a musical chord; all frequencies also fit 8 kHz.
    vec![
        tone(713.0, gain, attack, decay),
        tone(1193.0, gain * 0.62, attack, decay * 0.7),
        tone(1877.0, gain * 0.35, attack, decay * 0.4),
        grit(2300.0, gain, attack, 0.07, 0.8),
    ]
}
pub(in super::super) fn weight(hz: f32, gain: f32, decay: f32) -> Vec<Layer> {
    vec![
        tone(hz, gain, 0.004, decay),
        tone(hz * 1.61, gain * 0.35, 0.003, decay * 0.4),
        grit(650.0, 0.28, 0.002, decay * 0.7, 0.7),
        grit(2400.0, 0.14, 0.002, 0.035, 0.8),
    ]
}

impl Cue {
    pub fn seconds(self) -> f32 {
        use Cue::*;
        match self {
            Notice => 0.85,
            BiteLoad => 0.43,
            BiteLoadSecond => 0.48,
            BiteSnap => 0.16,
            BiteTear => 0.19,
            ClawLift => 0.31,
            ClawLeft | ClawRight => 0.24,
            PawSettle => 0.22,
            Scrape => 0.24,
            ChainTaut => 0.32,
            ChainRun => 0.35,
            ChargeStop => 0.65,
            LeapLoad => 0.55,
            LeapLaunch => 0.28,
            Landing => 0.50,
            JawsLoad => 0.95,
            JawsClose => 0.23,
            Recovery => 0.72,
            StrapTear => 0.58,
            Death => 0.95,
            Footfall => 0.20,
        }
    }
    /// Low-priority surface texture yields first; effort and attack cues remain bounded
    /// too, without ever stealing a voice-chat source or changing mixer policy.
    pub fn priority(self) -> u8 {
        use Cue::*;
        match self {
            Footfall | ChainRun => 0,
            PawSettle | Recovery => 1,
            Notice | ClawLift | Scrape | ChainTaut => 2,
            Death | Landing | ChargeStop | StrapTear => 4,
            _ => 3,
        }
    }
    pub fn at_feet(self) -> bool {
        matches!(
            self,
            Self::ClawLift
                | Self::ClawLeft
                | Self::ClawRight
                | Self::PawSettle
                | Self::Scrape
                | Self::Footfall
                | Self::Landing
                | Self::ChargeStop
                | Self::LeapLaunch
        )
    }
    pub fn describe(self) -> Sound {
        use Cue::*;
        let layers = match self {
            Notice => {
                let mut v = breath(57.0, 0.70, 0.09);
                v.extend(iron(0.03, 0.16, 0.05));
                v
            }
            BiteLoad => breath(91.0, 0.34, 0.07),
            BiteLoadSecond => breath(73.0, 0.36, 0.12),
            BiteSnap => {
                let mut v = weight(153.0, 0.30, 0.10);
                v.push(grit(1600.0, 0.22, 0.002, 0.07, 1.6));
                v
            }
            BiteTear => vec![
                grit(1300.0, 0.43, 0.003, 0.16, 0.8),
                tone(117.0, 0.21, 0.004, 0.12),
            ],
            ClawLift => vec![
                grit(520.0, 0.28, 0.08, 0.18, 0.7),
                tone(87.0, 0.12, 0.05, 0.16),
            ],
            ClawLeft | ClawRight => vec![
                grit(
                    if self == ClawLeft { 1750.0 } else { 1900.0 },
                    0.48,
                    0.022,
                    0.18,
                    0.8,
                ),
                grit(620.0, 0.18, 0.055, 0.13, 0.9),
            ],
            PawSettle => weight(103.0, 0.20, 0.16),
            Scrape => vec![
                grit(870.0, 0.42, 0.025, 0.17, 1.1),
                grit(2450.0, 0.24, 0.014, 0.18, 1.0),
            ],
            ChainTaut => iron(0.075, 0.20, 0.14),
            ChainRun => iron(0.003, 0.28, 0.12),
            ChargeStop => {
                let mut v = weight(53.0, 0.32, 0.30);
                v.extend(iron(0.015, 0.28, 0.08));
                v.push(grit(370.0, 0.24, 0.08, 0.46, 0.7));
                v
            }
            LeapLoad => breath(68.0, 0.38, 0.13),
            LeapLaunch => vec![
                grit(490.0, 0.30, 0.006, 0.18, 0.7),
                grit(1900.0, 0.28, 0.04, 0.17, 0.8),
            ],
            Landing => {
                let mut v = weight(43.0, 0.37, 0.40);
                v.push(grit(270.0, 0.24, 0.015, 0.38, 0.65));
                v
            }
            JawsLoad => {
                let mut v = breath(49.0, 0.64, 0.23);
                v.push(grit(1500.0, 0.17, 0.29, 0.42, 1.0));
                v
            }
            JawsClose => {
                let mut v = weight(89.0, 0.38, 0.16);
                v.push(grit(2100.0, 0.31, 0.002, 0.09, 1.3));
                v
            }
            Recovery => breath(61.0, 0.56, 0.045),
            StrapTear => {
                let mut v = iron(0.012, 0.39, 0.13);
                v.push(grit(1200.0, 0.43, 0.008, 0.25, 0.8));
                v
            }
            Death => {
                let mut v = breath(41.0, 0.72, 0.025);
                v.push(grit(280.0, 0.25, 0.26, 0.55, 0.7));
                v
            }
            Footfall => {
                let mut v = weight(79.0, 0.20, 0.15);
                v.extend(iron(0.003, 0.09, 0.025));
                v
            }
        };
        Sound { layers }
    }
}
