//! The scorpion's voice: dry chitin clicks as it walks, the sand shifting as it comes up out
//! of it, and a dry rattle before the sting — capped per species as the spider's is.
//!
//! Every cue follows something the player can see on the drawn rig: a step its legs just took,
//! the emergence it just began, the sting telegraph it just started raising its tail for. The
//! rattle is the one cue that separates the two windups the server sends alike (#1291, PR
//! #1302), and it separates them the way the rig does — by the order it has watched and the
//! telegraph's length — so it sounds for the heavy blow and never for the swipe. None of them
//! says that anything landed; the blow the server reports is the impact cue.
use super::spider::{envelope, grit, ticks};
use super::{Active, Pending, capped, sounds::Cue as CatalogueCue};
use crate::{
    audio::synth::{Curve, Exciter, Filter, FilterKind, Gate, Layer, Noise, Sound},
    net::{BlowTarget, MobKind, Snapshot},
    player::mobs::Mob,
};
use bevy::prelude::*;
use std::collections::HashMap;

/// How many scorpion cues may sound at once, across every scorpion, and how many of those may
/// be legs. The sand hall holds a handful; a rattle must always find room over their feet.
pub(super) const MAX_SOURCES: usize = 3;
pub(super) const MAX_CLICKS: usize = 2;

/// The speed clicks are at full level: the server's scorpion walk, 2.6 blocks a second, the
/// slowest in the game. Slower is quieter, down to a floor.
const FULL_SPEED: f32 = 2.6;

/// The level a step's clicks play at for a scorpion walking at `speed` blocks a second.
pub(super) fn step_gain(speed: f32) -> f32 {
    (speed / FULL_SPEED).clamp(0.3, 1.0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in super::super) enum Cue {
    /// One step of eight chitin-shod feet: a few dry, uneven clicks.
    Click,
    /// Sand pouring off a body coming up through it.
    Sand,
    /// The tail's segments rattling as it rises for the sting.
    Rattle,
}

pub(in super::super) const CUES: [Cue; 3] = [Cue::Click, Cue::Sand, Cue::Rattle];

impl Cue {
    pub fn seconds(self) -> f32 {
        match self {
            Self::Click => 0.14,
            // The server's emergence: the sand runs for as long as the body is rising.
            Self::Sand => 0.8,
            // Most of the sting's 1100 ms telegraph, ending before the strike.
            Self::Rattle => 0.75,
        }
    }

    /// Legs yield to everything: the ambush and the telegraph are what a player needs to hear.
    pub fn priority(self) -> u8 {
        match self {
            Self::Click => 0,
            Self::Sand => 2,
            Self::Rattle => 3,
        }
    }

    fn legs(self) -> bool {
        self == Self::Click
    }

    /// Every layer is shaped noise, and there is no tone anywhere. The clicks are gated at
    /// rates that share no period, so eight feet fall unevenly; the sand is a broad hiss over a
    /// low slide of mass, trickling as its grains gate slower; the rattle is a train of dry
    /// clicks that speeds up as the tail climbs, the way a threat display tightens.
    pub fn describe(self) -> Sound {
        let layers = match self {
            Self::Click => vec![
                ticks(grit(2100.0, 1.6, 0.62, 0.002, 0.13), 19.0, 15.0, 0.14, 0.12),
                ticks(grit(3400.0, 2.2, 0.36, 0.002, 0.12), 23.0, 27.0, 0.14, 0.08),
                ticks(grit(1200.0, 1.2, 0.30, 0.002, 0.13), 13.0, 17.0, 0.14, 0.10),
            ],
            Self::Sand => vec![
                grit(900.0, 0.7, 0.50, 0.06, 0.72),
                ticks(grit(2300.0, 1.0, 0.34, 0.04, 0.74), 58.0, 16.0, 0.8, 0.45),
                grit(1600.0, 0.9, 0.22, 0.10, 0.66),
                Layer {
                    exciter: Exciter::Noise(Noise::Brown),
                    gain: 0.34,
                    envelope: envelope(0.03, 0.60),
                    gate: None,
                    filter: Some(Filter {
                        kind: FilterKind::Low,
                        hz: 260.0,
                        q: 0.7,
                    }),
                },
            ],
            Self::Rattle => vec![
                Layer {
                    gate: Some(Gate {
                        from: 22.0,
                        to: 40.0,
                        seconds: 0.75,
                        curve: Curve::Exponential,
                        duty: 0.28,
                    }),
                    ..grit(2900.0, 1.8, 0.58, 0.03, 0.70)
                },
                Layer {
                    gate: Some(Gate {
                        from: 29.0,
                        to: 47.0,
                        seconds: 0.75,
                        curve: Curve::Exponential,
                        duty: 0.22,
                    }),
                    ..grit(1700.0, 1.4, 0.40, 0.03, 0.70)
                },
                grit(2400.0, 0.9, 0.10, 0.10, 0.60),
            ],
        };
        Sound { layers }
    }
}

/// The stamps each scorpion's rig has already been heard for: steps, rattles, emergences.
#[derive(Default)]
pub(super) struct State {
    heard: HashMap<u64, (u64, u64, u64)>,
}

impl State {
    /// A cue for everything a scorpion's rig did since it was last heard: a click for a step,
    /// the sand for an emergence, the rattle for a sting telegraph. Admission decides which
    /// are mixed.
    pub(super) fn heard(
        &mut self,
        mobs: &Query<&Mob>,
        snapshot: &Snapshot,
        pending: &mut Vec<Pending>,
    ) {
        self.heard.retain(|id, _| {
            snapshot
                .mobs
                .iter()
                .any(|mob| mob.entity_id == *id && mob.kind == MobKind::Scorpion)
        });
        for drawn in mobs {
            let Some((id, stamps)) = drawn.scorpion_stamps() else {
                continue;
            };
            let now = (stamps.steps, stamps.rattles, stamps.emergences);
            let (steps, rattles, emergences) = self.heard.insert(id, now).unwrap_or_default();
            let cue = |cue, origin, follows, gain| Pending {
                cue: CatalogueCue::Scorpion(cue),
                id,
                target: BlowTarget::Mob(MobKind::Scorpion),
                origin,
                follows,
                offset: None,
                owner: None,
                gain,
            };
            if stamps.steps != steps
                && let Some(speed) = stamps.step
            {
                pending.push(cue(Cue::Click, stamps.at, false, step_gain(speed)));
            }
            if stamps.emergences != emergences {
                pending.push(cue(Cue::Sand, stamps.sand, false, 1.0));
            }
            if stamps.rattles != rattles {
                pending.push(cue(Cue::Rattle, stamps.at, true, 1.0));
            }
        }
    }
}

/// Whether a scorpion cue may start, and which playing scorpion source it replaces if one
/// must go — the spider's rule with the scorpion's cap; see [`capped::admit`].
pub(super) fn admit(playing: &[Active], cue: Cue, id: u64) -> Result<Option<usize>, ()> {
    let cap = capped::Cap {
        sources: MAX_SOURCES,
        legs: MAX_CLICKS,
    };
    capped::admit(
        playing,
        cap,
        (cue.priority(), cue.legs()),
        id,
        |active| match active.cue {
            CatalogueCue::Scorpion(cue) => Some((cue.priority(), cue.legs())),
            _ => None,
        },
    )
}

#[cfg(test)]
mod tests;
