//! The Draugr king's voice, blade, casts and final-stage cues through the shared boss
//! observer in [`super::guardian`]. Only the announced move, phase, combo position, pulse
//! and ticks select a cue: no local clock, animation event or displayed health.
pub(super) mod sounds;

use super::{guardian::Voice, sounds::Cue as CatalogueCue};
use crate::{
    net::{EncounterMoveKind, MobKind, MovePhase},
    player::encounters::PresentedMove,
};
use bevy::prelude::Vec3;
use sounds::Cue;

pub(super) static VOICE: Voice = Voice {
    kind: MobKind::DraugrKing,
    supported,
    markers,
    notice: CatalogueCue::King(Cue::Notice),
    death: CatalogueCue::King(Cue::Death),
    // The mask falls on the first observed final-stage timeline, as the regalia do.
    stage: (3, CatalogueCue::King(Cue::MaskFall)),
    interrupted: Some(CatalogueCue::King(Cue::ChantBroken)),
    offset,
};

fn supported(kind: EncounterMoveKind) -> bool {
    use EncounterMoveKind::*;
    matches!(
        kind,
        KingsSentence
            | ThreeTolls
            | SepulchreSpear
            | Burial
            | EdictOfTheGraves
            | RequiemOfTheBuried
    )
}

/// Rest-mesh anchors: the funeral mask, where it lands, the planted blade every ritual is
/// anchored on, the raised free hand, and the blade in hand shifted towards its region.
/// Ritual cues stay on the king; the floor geometry, not the sound, says where to stand.
fn offset(cue: CatalogueCue, side: f32) -> Vec3 {
    use Cue::*;
    match cue {
        CatalogueCue::King(
            Notice | Recovery | Death | EdictCall | NoteFirst | NoteSecond | NoteThird
            | ChantBroken,
        ) => Vec3::new(0.10, 2.40, -0.20),
        CatalogueCue::King(MaskFall) => Vec3::new(-0.30, 0.05, -0.40),
        CatalogueCue::King(
            BladeBite | BladeFree | Plant | CracksRun | BurialErupt | GravesErupt | RequiemToll,
        ) => Vec3::new(0.08, 0.05, -1.30),
        CatalogueCue::King(SpearGather | SpearLoose | RuneFirst | RuneSecond | RuneThird) => {
            Vec3::new(-0.45, 2.10, -0.40)
        }
        _ => Vec3::new(0.08 + side * 0.90, 1.85, -0.55),
    }
}

/// Normalised authoritative intervals, like the guardian's. The Sentence clang sits in
/// the held pause and the blade pulls free where the choreography starts to lift it. A
/// channel pulse is shown on its first tick and contacts on its last, as the server does.
fn markers(one: &PresentedMove) -> Vec<(u32, CatalogueCue, f32)> {
    use EncounterMoveKind::*;
    use MovePhase::*;
    let last = one.announced.phase_ticks.saturating_sub(1);
    // Copied whole from the server, never counted from earlier instances.
    let (step, total) = one.announced.combo.unwrap_or((1, 1));
    let pulse = usize::from(one.announced.pulse.map_or(0, |(index, _)| index).min(2));
    // The first toll's region lies left of the aim, the second right, the thrust ahead.
    let side = match (one.announced.kind, step) {
        (ThreeTolls, 1) => -1.0,
        (ThreeTolls, 2) => 1.0,
        _ => 0.0,
    };
    let cues = match (one.announced.kind, one.announced.phase) {
        (KingsSentence, Telegraph) => vec![
            (0, Cue::SentenceRaise),
            (last * 80 / 100, Cue::SentenceClang),
        ],
        (KingsSentence, Release) => vec![(0, Cue::SentenceCut), (last, Cue::BladeBite)],
        (KingsSentence, Recovery) => vec![(last * 55 / 100, Cue::BladeFree)],
        (ThreeTolls, Telegraph) => vec![(
            0,
            match step {
                1 => Cue::TollFirst,
                2 => Cue::TollSecond,
                _ => Cue::TollThird,
            },
        )],
        (ThreeTolls, Release) => vec![(
            0,
            match step {
                1 => Cue::SweepLeft,
                2 => Cue::SweepRight,
                _ => Cue::Thrust,
            },
        )],
        (ThreeTolls, Recovery) if step >= total => vec![(0, Cue::Recovery)],
        (SepulchreSpear, Telegraph) => vec![(0, Cue::SpearGather)],
        (SepulchreSpear, Release) => vec![(0, Cue::SpearLoose)],
        (Burial | RequiemOfTheBuried, Telegraph) => vec![(0, Cue::Plant)],
        (EdictOfTheGraves, Telegraph) => vec![(0, Cue::EdictCall)],
        (Burial, Channel) => vec![(0, Cue::CracksRun), (last, Cue::BurialErupt)],
        (EdictOfTheGraves, Channel) => vec![
            (0, [Cue::RuneFirst, Cue::RuneSecond, Cue::RuneThird][pulse]),
            (last, Cue::GravesErupt),
        ],
        (RequiemOfTheBuried, Channel) => vec![
            (0, [Cue::NoteFirst, Cue::NoteSecond, Cue::NoteThird][pulse]),
            (last, Cue::RequiemToll),
        ],
        (SepulchreSpear | Burial | EdictOfTheGraves | RequiemOfTheBuried, Recovery) => {
            vec![(0, Cue::Recovery)]
        }
        _ => Vec::new(),
    };
    cues.into_iter()
        .map(|(tick, cue)| (tick, CatalogueCue::King(cue), side))
        .collect()
}

#[cfg(test)]
mod capture;
#[cfg(test)]
pub(super) mod tests;
