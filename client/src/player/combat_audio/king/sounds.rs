//! The king's grave-iron blade, hollow voice and fallen regalia as bounded synthesis
//! layers. Gestures only: a blade can bite the floor without touching anybody.
use super::super::guardian::sounds::{breath, grit, iron, tone, weight};
use crate::audio::synth::{Layer, Sound};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in super::super) enum Cue {
    Notice,
    SentenceRaise,
    SentenceClang,
    SentenceCut,
    BladeBite,
    BladeFree,
    TollFirst,
    TollSecond,
    TollThird,
    SweepLeft,
    SweepRight,
    Thrust,
    Recovery,
    MaskFall,
    Death,
}

pub(in super::super) const CUES: [Cue; 15] = [
    Cue::Notice,
    Cue::SentenceRaise,
    Cue::SentenceClang,
    Cue::SentenceCut,
    Cue::BladeBite,
    Cue::BladeFree,
    Cue::TollFirst,
    Cue::TollSecond,
    Cue::TollThird,
    Cue::SweepLeft,
    Cue::SweepRight,
    Cue::Thrust,
    Cue::Recovery,
    Cue::MaskFall,
    Cue::Death,
];

/// Inharmonic bell partials. The three tolls differ by fundamental, so each blow of the
/// combination keeps its own pitch even when its pose is hidden behind the cloak.
fn bell(hz: f32, gain: f32) -> Vec<Layer> {
    vec![
        tone(hz, gain, 0.003, 0.70),
        tone(hz * 2.76, gain * 0.5, 0.003, 0.34),
        tone(hz * 5.40, gain * 0.25, 0.003, 0.14),
        tone(hz * 0.5, gain * 0.4, 0.012, 0.62),
    ]
}

impl Cue {
    pub fn seconds(self) -> f32 {
        use Cue::*;
        match self {
            Notice => 1.05,
            SentenceRaise => 0.75,
            SentenceClang => 0.65,
            SentenceCut | SweepLeft | SweepRight => 0.30,
            BladeBite => 0.55,
            BladeFree => 0.45,
            TollFirst | TollSecond | TollThird => 0.85,
            Thrust => 0.28,
            Recovery => 0.95,
            MaskFall => 0.60,
            Death => 1.20,
        }
    }
    /// The signals a player reads before a blow outrank its whoosh and its settling.
    pub fn priority(self) -> u8 {
        use Cue::*;
        match self {
            BladeFree | Recovery => 1,
            Notice | SentenceRaise => 2,
            SentenceClang | TollFirst | TollSecond | TollThird | BladeBite | MaskFall | Death => 4,
            _ => 3,
        }
    }
    pub fn describe(self) -> Sound {
        use Cue::*;
        let layers = match self {
            Notice => {
                let mut v = breath(43.0, 0.85, 0.14);
                v.extend(iron(0.06, 0.30, 0.05));
                v
            }
            SentenceRaise => vec![
                grit(1400.0, 0.30, 0.30, 0.38, 1.2),
                grit(520.0, 0.22, 0.22, 0.45, 0.8),
                tone(98.0, 0.14, 0.25, 0.42),
            ],
            // The design's low clang: a heavy inharmonic ring, not a bell.
            SentenceClang => vec![
                tone(131.0, 0.30, 0.003, 0.55),
                tone(211.0, 0.18, 0.003, 0.40),
                tone(467.0, 0.11, 0.003, 0.22),
                tone(739.0, 0.06, 0.003, 0.12),
                grit(1800.0, 0.22, 0.002, 0.05, 0.9),
            ],
            SentenceCut => vec![
                grit(320.0, 0.42, 0.06, 0.20, 0.6),
                grit(950.0, 0.22, 0.09, 0.16, 0.8),
            ],
            BladeBite => {
                let mut v = weight(47.0, 0.34, 0.40);
                v.extend(iron(0.004, 0.30, 0.07));
                v
            }
            BladeFree => {
                let mut v = vec![grit(700.0, 0.34, 0.08, 0.30, 0.9)];
                v.extend(iron(0.10, 0.20, 0.05));
                v
            }
            TollFirst => bell(233.0, 0.26),
            TollSecond => bell(175.0, 0.26),
            TollThird => bell(117.0, 0.28),
            // The two cuts differ in weight as well as pan: the first is drawn low and
            // slow across the body, the second whips back higher and sharper.
            SweepLeft => vec![
                grit(820.0, 0.44, 0.06, 0.22, 0.7),
                grit(300.0, 0.22, 0.08, 0.20, 0.8),
                tone(71.0, 0.16, 0.05, 0.22),
            ],
            SweepRight => vec![
                grit(1600.0, 0.42, 0.02, 0.20, 0.9),
                grit(520.0, 0.18, 0.03, 0.18, 0.8),
            ],
            Thrust => vec![
                grit(1650.0, 0.36, 0.012, 0.16, 1.4),
                tone(92.0, 0.20, 0.006, 0.20),
                grit(600.0, 0.18, 0.02, 0.14, 0.9),
            ],
            Recovery => {
                let mut v = breath(52.0, 0.72, 0.06);
                v.extend(iron(0.03, 0.20, 0.04));
                v
            }
            MaskFall => {
                let mut v = iron(0.003, 0.42, 0.13);
                v.push(tone(1567.0, 0.07, 0.003, 0.30));
                v.extend(weight(163.0, 0.12, 0.10));
                v
            }
            Death => {
                let mut v = breath(37.0, 0.92, 0.03);
                v.extend(weight(41.0, 0.28, 0.62));
                v.extend(iron(0.012, 0.50, 0.05));
                v
            }
        };
        Sound { layers }
    }
}
