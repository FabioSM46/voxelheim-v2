//! The king's grave-iron blade, hollow voice, casts and fallen regalia as bounded
//! synthesis layers. Gestures only: a blade can bite the floor without touching anybody,
//! and an eruption sounds whether or not anybody stood in it. No sound is speech.
use super::super::guardian::sounds::{breath, grit, iron, tone, weight};
use crate::audio::synth::{Envelope, Exciter, Filter, FilterKind, Layer, Sound, Wave};

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
    SpearGather,
    SpearLoose,
    Plant,
    CracksRun,
    BurialErupt,
    EdictCall,
    RuneFirst,
    RuneSecond,
    RuneThird,
    GravesErupt,
    NoteFirst,
    NoteSecond,
    NoteThird,
    RequiemToll,
    ChantBroken,
}

pub(in super::super) const CUES: [Cue; 30] = [
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
    Cue::SpearGather,
    Cue::SpearLoose,
    Cue::Plant,
    Cue::CracksRun,
    Cue::BurialErupt,
    Cue::EdictCall,
    Cue::RuneFirst,
    Cue::RuneSecond,
    Cue::RuneThird,
    Cue::GravesErupt,
    Cue::NoteFirst,
    Cue::NoteSecond,
    Cue::NoteThird,
    Cue::RequiemToll,
    Cue::ChantBroken,
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

/// Rune groups: a struck fundamental with a tritone and an octave, one pitch per pulse.
fn rune(hz: f32) -> Vec<Layer> {
    vec![
        tone(hz, 0.16, 0.004, 0.45),
        tone(hz * 1.414, 0.09, 0.004, 0.35),
        tone(hz * 2.01, 0.06, 0.004, 0.20),
        grit(2800.0, 0.06, 0.002, 0.05, 2.0),
    ]
}

/// An intoned note: a filtered triangle with breath, sustained like a held chant, never a
/// word. Its pitch is the pulse the server announced.
fn note(hz: f32) -> Vec<Layer> {
    vec![
        Layer {
            exciter: Exciter::Oscillator {
                wave: Wave::Triangle,
                hz,
            },
            gain: 0.24,
            envelope: Envelope {
                attack: 0.08,
                decay: 0.62,
                sustain: 0.0,
                release: 0.018,
            },
            filter: Some(Filter {
                kind: FilterKind::Low,
                hz: 900.0,
                q: 0.7,
            }),
        },
        tone(hz * 2.0, 0.05, 0.10, 0.55),
        grit(hz * 4.0, 0.08, 0.06, 0.60, 1.4),
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
            BladeBite | BurialErupt | RuneFirst | RuneSecond | RuneThird => 0.55,
            BladeFree => 0.45,
            TollFirst | TollSecond | TollThird => 0.85,
            Thrust => 0.28,
            Recovery => 0.95,
            MaskFall | Plant | GravesErupt => 0.60,
            Death => 1.20,
            SpearGather => 1.30,
            SpearLoose => 0.40,
            CracksRun => 0.50,
            EdictCall | NoteFirst | NoteSecond | NoteThird => 0.80,
            RequiemToll => 0.75,
            ChantBroken => 0.70,
        }
    }
    /// The signals a player reads before a blow or pulse outrank its whoosh and settling.
    pub fn priority(self) -> u8 {
        use Cue::*;
        match self {
            BladeFree | Recovery => 1,
            Notice | SentenceRaise | CracksRun => 2,
            SentenceCut | SweepLeft | SweepRight | Thrust | Plant | EdictCall => 3,
            _ => 4,
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
            // The crystal forms: staggered long attacks climb in pitch and loudness over
            // the telegraph, the design's rising sound, and stop before the release.
            SpearGather => vec![
                tone(659.0, 0.08, 0.80, 0.45),
                tone(1318.0, 0.06, 1.00, 0.25),
                tone(1760.0, 0.05, 1.10, 0.18),
                tone(2637.0, 0.04, 1.20, 0.09),
                grit(3000.0, 0.10, 1.05, 0.22, 2.0),
            ],
            SpearLoose => vec![
                grit(2600.0, 0.34, 0.010, 0.30, 1.5),
                tone(1975.0, 0.10, 0.003, 0.18),
                grit(700.0, 0.20, 0.020, 0.25, 0.8),
            ],
            Plant => {
                let mut v = weight(55.0, 0.30, 0.45);
                v.extend(iron(0.003, 0.35, 0.09));
                v.push(grit(240.0, 0.25, 0.010, 0.50, 0.6));
                v
            }
            CracksRun => vec![
                grit(1900.0, 0.30, 0.004, 0.06, 1.8),
                grit(900.0, 0.28, 0.050, 0.40, 1.0),
                grit(3100.0, 0.12, 0.002, 0.03, 2.0),
            ],
            BurialErupt => {
                let mut v = weight(38.0, 0.36, 0.45);
                v.push(grit(420.0, 0.30, 0.004, 0.40, 0.6));
                v
            }
            EdictCall => {
                let mut v = breath(48.0, 0.60, 0.08);
                v.push(tone(880.0, 0.06, 0.02, 0.50));
                v
            }
            RuneFirst => rune(392.0),
            RuneSecond => rune(523.0),
            RuneThird => rune(698.0),
            GravesErupt => {
                let mut v = weight(61.0, 0.32, 0.40);
                v.push(grit(1300.0, 0.30, 0.003, 0.20, 1.0));
                v.extend(iron(0.004, 0.20, 0.05));
                v
            }
            // Three descending notes: the chant is readable without a word being sung.
            NoteFirst => note(147.0),
            NoteSecond => note(131.0),
            NoteThird => note(98.0),
            RequiemToll => {
                let mut v = bell(82.0, 0.30);
                v.push(grit(2400.0, 0.12, 0.003, 0.10, 1.4));
                v
            }
            // A choked note and shattering ice: the server broke the channel.
            ChantBroken => {
                let mut v = vec![
                    tone(131.0, 0.15, 0.003, 0.12),
                    grit(2200.0, 0.36, 0.003, 0.35, 1.2),
                ];
                v.extend(iron(0.003, 0.30, 0.10));
                v
            }
        };
        Sound { layers }
    }
}
