//! The cave spider's voice: legs ticking on stone as it runs, a hiss when it notices you and
//! a wet bite when it lunges — and a cap, so a wave of thirty is a skitter and never a roar.
//!
//! Every cue follows something the player can see: a step the drawn rig just took, or a
//! transition in the action the server sent. None of them says that a bite landed; the blow
//! the server reports is the impact cue, as for every other species.
use super::{Active, Pending, sounds::Cue as CatalogueCue};
use crate::{
    audio::synth::{Curve, Envelope, Exciter, Filter, FilterKind, Gate, Layer, Noise, Sound, Wave},
    net::{BlowTarget, MobKind, Snapshot},
    player::mobs::Mob,
};
use bevy::prelude::*;
use std::collections::HashMap;

/// How many spider cues may sound at once, across every spider, and how many of those may be
/// legs. A horde above the cap is heard as its nearest few: the rest are not mixed at all,
/// which is what keeps thirty skitters from summing into one roar.
pub(super) const MAX_SOURCES: usize = 3;
pub(super) const MAX_SKITTERS: usize = 2;

/// How many spiders running at once turn one spider's ticks into the dense ticking of many.
pub(super) const SWARM: usize = 3;

/// How long after its last step a spider still counts as running, for the swarm count.
const RUNNING: f32 = 0.25;

/// The speed a skitter is at full level: the server's cave-spider run, 5.0 blocks a second.
/// A spider creeping in at a fraction of it ticks at that fraction, down to a floor.
const FULL_SPEED: f32 = 5.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in super::super) enum Cue {
    /// One spider's legs: a few dry ticks, uneven.
    Skitter,
    /// Many spiders' legs through one voice: a denser, faster ticking.
    SkitterSwarm,
    /// Noticed: a breathy, rasping hiss.
    Hiss,
    /// The lunge closing: a click of fangs and a wet smack.
    Bite,
}

pub(in super::super) const CUES: [Cue; 4] = [Cue::Skitter, Cue::SkitterSwarm, Cue::Hiss, Cue::Bite];

fn envelope(attack: f32, decay: f32) -> Envelope {
    Envelope {
        attack,
        decay,
        sustain: 0.0,
        release: 0.01,
    }
}

/// Band-passed white noise: the texture every spider sound is made of.
fn grit(hz: f32, q: f32, gain: f32, attack: f32, decay: f32) -> Layer {
    Layer {
        exciter: Exciter::Noise(Noise::White),
        gain,
        envelope: envelope(attack, decay),
        gate: None,
        filter: Some(Filter {
            kind: FilterKind::Band,
            hz,
            q,
        }),
    }
}

/// A noise layer struck open and shut `from`–`to` times a second: one tick per opening.
fn ticks(layer: Layer, from: f32, to: f32, seconds: f32, duty: f32) -> Layer {
    Layer {
        gate: Some(Gate {
            from,
            to,
            seconds,
            curve: Curve::Linear,
            duty,
        }),
        ..layer
    }
}

impl Cue {
    pub fn seconds(self) -> f32 {
        match self {
            Self::Skitter => 0.16,
            Self::SkitterSwarm => 0.22,
            Self::Hiss => 0.55,
            Self::Bite => 0.24,
        }
    }

    /// Legs yield to voices: a bite or a hiss may take a skitter's place, never the reverse.
    pub fn priority(self) -> u8 {
        match self {
            Self::Skitter | Self::SkitterSwarm => 0,
            Self::Hiss => 2,
            Self::Bite => 3,
        }
    }

    fn legs(self) -> bool {
        matches!(self, Self::Skitter | Self::SkitterSwarm)
    }

    /// Every layer is shaped noise. The two ticking cues are gated at rates that share no
    /// period, so the ticks fall unevenly the way eight feet do; the hiss is a broad band
    /// with a rasping flutter; the bite is a click over a squelch that slows as it closes.
    /// The only tone is a faint body under the bite, and even that is not what is heard.
    pub fn describe(self) -> Sound {
        let layers = match self {
            Self::Skitter => vec![
                ticks(grit(2600.0, 1.4, 0.60, 0.003, 0.15), 34.0, 26.0, 0.16, 0.18),
                ticks(grit(1500.0, 1.1, 0.42, 0.003, 0.15), 23.0, 29.0, 0.16, 0.15),
                ticks(grit(3300.0, 2.0, 0.30, 0.003, 0.14), 41.0, 37.0, 0.16, 0.10),
            ],
            Self::SkitterSwarm => vec![
                ticks(grit(2800.0, 1.4, 0.40, 0.004, 0.21), 47.0, 43.0, 0.22, 0.14),
                ticks(grit(1700.0, 1.2, 0.34, 0.004, 0.21), 39.0, 44.0, 0.22, 0.12),
                ticks(grit(3400.0, 2.0, 0.26, 0.004, 0.20), 53.0, 49.0, 0.22, 0.10),
                ticks(grit(1200.0, 1.0, 0.28, 0.004, 0.21), 31.0, 36.0, 0.22, 0.16),
            ],
            Self::Hiss => vec![
                grit(3100.0, 0.8, 0.55, 0.05, 0.50),
                grit(2200.0, 1.0, 0.32, 0.07, 0.46),
                ticks(grit(3000.0, 1.2, 0.26, 0.05, 0.48), 60.0, 42.0, 0.55, 0.7),
                Layer {
                    exciter: Exciter::Noise(Noise::Brown),
                    gain: 0.16,
                    envelope: envelope(0.08, 0.40),
                    gate: None,
                    filter: Some(Filter {
                        kind: FilterKind::Low,
                        hz: 420.0,
                        q: 0.7,
                    }),
                },
            ],
            Self::Bite => vec![
                grit(3200.0, 2.5, 0.60, 0.001, 0.025),
                ticks(grit(900.0, 1.3, 0.55, 0.004, 0.16), 70.0, 28.0, 0.20, 0.55),
                ticks(grit(1500.0, 1.6, 0.36, 0.006, 0.14), 55.0, 24.0, 0.20, 0.45),
                Layer {
                    exciter: Exciter::Noise(Noise::Brown),
                    gain: 0.40,
                    envelope: envelope(0.003, 0.08),
                    gate: None,
                    filter: Some(Filter {
                        kind: FilterKind::Low,
                        hz: 280.0,
                        q: 0.7,
                    }),
                },
                Layer {
                    exciter: Exciter::Oscillator {
                        wave: Wave::Sine,
                        hz: 190.0,
                    },
                    gain: 0.10,
                    envelope: envelope(0.003, 0.06),
                    gate: None,
                    filter: None,
                },
            ],
        };
        Sound { layers }
    }
}

/// The level a skitter plays at for a spider running at `speed` blocks a second.
pub(super) fn step_gain(speed: f32) -> f32 {
    (speed / FULL_SPEED).clamp(0.3, 1.0)
}

/// The step stamps each spider's rig has already been heard for, and when each last ran.
#[derive(Default)]
pub(super) struct State {
    serials: HashMap<u64, u64>,
    running: HashMap<u64, f32>,
}

impl State {
    /// A skitter for every spider whose rig took a step this frame, loud with its speed and
    /// dense with how many spiders are running. Admission decides which of them are heard.
    pub(super) fn steps(
        &mut self,
        mobs: &Query<&Mob>,
        snapshot: &Snapshot,
        now: f32,
        pending: &mut Vec<Pending>,
    ) {
        self.serials.retain(|id, _| {
            snapshot
                .mobs
                .iter()
                .any(|mob| mob.entity_id == *id && mob.kind == MobKind::CaveSpider)
        });
        self.running
            .retain(|id, last| now - *last <= RUNNING && self.serials.contains_key(id));
        let mut stepped = Vec::new();
        for drawn in mobs {
            let Some((id, serial, contact, speed)) = drawn.spider_audio_step() else {
                continue;
            };
            if self.serials.insert(id, serial) == Some(serial) {
                continue;
            }
            self.running.insert(id, now);
            stepped.push((id, contact, speed));
        }
        let cue = if self.running.len() >= SWARM {
            Cue::SkitterSwarm
        } else {
            Cue::Skitter
        };
        for (id, contact, speed) in stepped {
            pending.push(Pending {
                cue: CatalogueCue::Spider(cue),
                id,
                target: BlowTarget::Mob(MobKind::CaveSpider),
                origin: contact,
                follows: false,
                offset: None,
                owner: None,
                gain: step_gain(speed),
            });
        }
    }
}

/// Whether a spider cue may start, and which playing spider source it replaces if one must
/// go. `Err` refuses it. A spider plays one skitter at a time; the species plays at most
/// [`MAX_SOURCES`], of which at most [`MAX_SKITTERS`] are legs; and a full species gives
/// up its lowest-priority source only to something that outranks it.
pub(super) fn admit(playing: &[Active], cue: Cue, id: u64) -> Result<Option<usize>, ()> {
    let spider = |active: &&Active| matches!(active.cue, CatalogueCue::Spider(_));
    let of = |active: &Active| match active.cue {
        CatalogueCue::Spider(cue) => Some(cue),
        _ => None,
    };
    if cue.legs()
        && playing
            .iter()
            .any(|active| active.id == id && of(active).is_some_and(Cue::legs))
    {
        return Err(());
    }
    let total = playing.iter().filter(spider).count();
    let legs = playing
        .iter()
        .filter(|active| of(active).is_some_and(Cue::legs))
        .count();
    if total < MAX_SOURCES && !(cue.legs() && legs >= MAX_SKITTERS) {
        return Ok(None);
    }
    playing
        .iter()
        .enumerate()
        .filter(|(_, active)| {
            of(active).is_some_and(|other| {
                other.priority() < cue.priority() && (!cue.legs() || other.legs())
            })
        })
        .min_by_key(|(_, active)| active.priority)
        .map(|(index, _)| Some(index))
        .ok_or(())
}

#[cfg(test)]
mod tests;
