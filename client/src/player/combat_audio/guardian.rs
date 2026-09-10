//! Once-only boss gestures sampled from the server timeline, plus the guardian's
//! explicitly cosmetic gait contacts. Neither path infers a successful hit. The
//! observer is shared by every boss [`Voice`]; a species adds only its catalogue.
pub(super) mod sounds;
use super::{Pending, king, sounds::Cue as CatalogueCue};
use crate::{
    net::{
        BlowTarget, EncounterMoveKind, EncounterTimelineInbox, MobAction, MobKind, MobState,
        MovePhase, Snapshot,
    },
    player::{
        encounters::{EncounterPresentation, MoveKey, PresentedMove, Window},
        mobs::Mob,
    },
};
use bevy::prelude::*;
use sounds::Cue;
use std::collections::HashMap;

// Four correlated boss gestures must coexist with protected voice references.
// This is authored source headroom, not a mixer or user-setting change. The caps are
// shared by every boss voice, not granted again to each species.
pub(super) const SOURCE_GAIN: f32 = 0.5;
pub(super) const MAX_PER_BOSS: usize = 3;
pub(super) const MAX_BOSS_SOURCES: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PhaseKey {
    key: MoveKey,
    kind: EncounterMoveKind,
    phase: MovePhase,
    started: u32,
    combo: Option<(u8, u8)>,
}
impl From<&PresentedMove> for PhaseKey {
    fn from(one: &PresentedMove) -> Self {
        Self {
            key: one.key,
            kind: one.announced.kind,
            phase: one.announced.phase,
            started: one.announced.phase_started_tick,
            combo: one.announced.combo,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Owner {
    Phase(PhaseKey),
    /// Rings on through later phases of the same announced instance, never past it.
    Tail(PhaseKey),
    Alive,
    Death,
    Gait,
}
#[derive(Default)]
struct Observed {
    action: Option<MobAction>,
    stage: Option<(u64, u8)>,
    phase: Option<PhaseKey>,
    consumed: u8,
    current: Option<PhaseKey>,
    foot_serial: Option<u64>,
}
#[derive(Default)]
pub(super) struct State(HashMap<u64, Observed>);

/// One boss species' authored routing. Observation, freshness, ownership and caps
/// stay here, so a second boss cannot drift from these replay rules.
pub(super) struct Voice {
    pub(super) kind: MobKind,
    pub(super) supported: fn(EncounterMoveKind) -> bool,
    pub(super) markers: fn(&PresentedMove) -> Vec<(u32, CatalogueCue, f32)>,
    pub(super) notice: CatalogueCue,
    pub(super) death: CatalogueCue,
    /// Observed crossing into this stage ordinal, inside one encounter, plays the cue once.
    pub(super) stage: (u8, CatalogueCue),
    /// Where a cue sits in the boss frame; `side` is -1 for left and +1 for right.
    pub(super) offset: fn(CatalogueCue, f32) -> Vec3,
}

static GUARDIAN: Voice = Voice {
    kind: MobKind::VargrGuardian,
    supported,
    markers: guardian_markers,
    notice: CatalogueCue::Guardian(Cue::Notice),
    death: CatalogueCue::Guardian(Cue::Death),
    stage: (2, CatalogueCue::Guardian(Cue::StrapTear)),
    offset: guardian_offset,
};

fn voice(kind: MobKind) -> Option<&'static Voice> {
    [&GUARDIAN, &king::VOICE]
        .into_iter()
        .find(|voice| voice.kind == kind)
}

fn dead(action: MobAction) -> bool {
    matches!(action, MobAction::Dying | MobAction::Corpse)
}
fn supported(kind: EncounterMoveKind) -> bool {
    matches!(
        kind,
        EncounterMoveKind::BiteAndTear
            | EncounterMoveKind::PrisonerClaws
            | EncounterMoveKind::CollarCharge
            | EncounterMoveKind::PredatorLeap
            | EncounterMoveKind::BonebreakerJaws
    )
}
fn current<'a>(
    presentation: Option<&'a EncounterPresentation>,
    boss: u64,
    voice: &Voice,
) -> Option<&'a PresentedMove> {
    presentation?
        .0
        .iter()
        .filter(|one| {
            one.key.boss == boss
                && one.boss_kind == voice.kind
                && one.window == Window::Current
                && one.announced.ended.is_none()
                && (voice.supported)(one.announced.kind)
        })
        .max_by_key(|one| (one.damaging(), one.key.instance))
}

/// Floor, not ceil: below 10 Hz only the marker's own tick is fresh. A tick at 1 Hz
/// is not permission to replay a transient one second late. Serial ordering wraps.
fn fresh(tick: u32, marker: u32, rate: u8) -> bool {
    tick.wrapping_sub(marker) <= u32::from(rate) / 10
}

fn enqueue(
    pending: &mut Vec<Pending>,
    mob: &MobState,
    voice: &Voice,
    cue: CatalogueCue,
    owner: Owner,
    side: f32,
) {
    let offset = (voice.offset)(cue, side);
    let rotated = Quat::from_rotation_y(mob.yaw) * offset;
    pending.push(Pending {
        cue,
        id: mob.entity_id,
        target: BlowTarget::Mob(mob.kind),
        origin: Vec3::from_array(mob.pos) + rotated,
        follows: false,
        offset: Some(offset),
        owner: Some(owner),
    });
}

fn guardian_offset(cue: CatalogueCue, side: f32) -> Vec3 {
    if matches!(cue, CatalogueCue::Guardian(cue) if cue.at_feet()) {
        Vec3::new(side * 0.58, 0.05, -0.34)
    } else {
        Vec3::new(0.0, 1.25, -0.32)
    }
}

fn guardian_markers(one: &PresentedMove) -> Vec<(u32, CatalogueCue, f32)> {
    markers(one)
        .into_iter()
        .map(|(tick, cue, side)| (tick, CatalogueCue::Guardian(cue), side))
        .collect()
}

/// Small marker schedules use normalized authoritative intervals. There is no local
/// timer, queued future playback, fixed phase duration or observed-instance counter.
fn markers(one: &PresentedMove) -> Vec<(u32, Cue, f32)> {
    use EncounterMoveKind::*;
    use MovePhase::*;
    let second = one.announced.combo.is_some_and(|(step, _)| step == 2);
    let last = one.announced.phase_ticks.saturating_sub(1);
    match (one.announced.kind, one.announced.phase) {
        (BiteAndTear, Telegraph) => vec![(
            0,
            if second {
                Cue::BiteLoadSecond
            } else {
                Cue::BiteLoad
            },
            0.0,
        )],
        (BiteAndTear, Release) => {
            vec![(0, if second { Cue::BiteTear } else { Cue::BiteSnap }, 0.0)]
        }
        (PrisonerClaws, Telegraph) => vec![(
            0,
            Cue::ClawLift,
            if second || one.announced.combo.is_none() {
                1.0
            } else {
                -1.0
            },
        )],
        (PrisonerClaws, Release) => vec![(
            0,
            if second || one.announced.combo.is_none() {
                Cue::ClawRight
            } else {
                Cue::ClawLeft
            },
            if second || one.announced.combo.is_none() {
                1.0
            } else {
                -1.0
            },
        )],
        (PrisonerClaws, Recovery) => vec![(
            0,
            Cue::PawSettle,
            if second || one.announced.combo.is_none() {
                1.0
            } else {
                -1.0
            },
        )],
        // The visual scrapes peak at normalized .18 and .58. Chain tension follows
        // both scrapes, before launch; these are gesture markers, never damage ticks.
        (CollarCharge, Telegraph) => vec![
            (last * 18 / 100, Cue::Scrape, -1.0),
            (last * 58 / 100, Cue::Scrape, 1.0),
            (last * 82 / 100, Cue::ChainTaut, 0.0),
        ],
        (CollarCharge, Release) => vec![(0, Cue::ChainRun, 0.0)],
        (CollarCharge, Recovery) => vec![(0, Cue::ChargeStop, 0.0)],
        (PredatorLeap, Telegraph) => vec![(0, Cue::LeapLoad, 0.0)],
        (PredatorLeap, Release) => vec![(0, Cue::LeapLaunch, 0.0), (last, Cue::Landing, 0.0)],
        (BonebreakerJaws, Telegraph) => vec![(0, Cue::JawsLoad, 0.0)],
        (BonebreakerJaws, Release) => vec![(0, Cue::JawsClose, 0.0)],
        (_, Recovery) => vec![(0, Cue::Recovery, 0.0)],
        _ => Vec::new(),
    }
}

impl State {
    pub(super) fn sample(
        &mut self,
        snapshot: &Snapshot,
        inbox: Option<&EncounterTimelineInbox>,
        presentation: Option<&EncounterPresentation>,
        rate: u8,
        pending: &mut Vec<Pending>,
    ) {
        self.0.retain(|id, _| {
            snapshot
                .mobs
                .iter()
                .any(|mob| mob.entity_id == *id && voice(mob.kind).is_some())
        });
        for mob in &snapshot.mobs {
            let Some(voice) = voice(mob.kind) else {
                continue;
            };
            let seen = self.0.entry(mob.entity_id).or_default();
            if seen.action.is_some_and(|action| !dead(action)) && dead(mob.action) {
                enqueue(pending, mob, voice, voice.death, Owner::Death, 0.0);
            } else if seen.action == Some(MobAction::Idle) && mob.action == MobAction::Chase {
                enqueue(pending, mob, voice, voice.notice, Owner::Alive, 0.0);
            }
            seen.action = Some(mob.action);
            let stage = inbox
                .and_then(|inbox| {
                    inbox.live().iter().find(|timeline| {
                        timeline.boss_entity_id == mob.entity_id && timeline.boss == mob.kind
                    })
                })
                .map(|timeline| (timeline.encounter_id, timeline.phase));
            let (to, cue) = voice.stage;
            if !dead(mob.action)
                && seen
                    .stage
                    .zip(stage)
                    .is_some_and(|(old, new)| old.0 == new.0 && old.1 < to && new.1 >= to)
            {
                enqueue(pending, mob, voice, cue, Owner::Alive, 0.0);
            }
            // A frame without a timeline is not a new stage; only an announcement is.
            if stage.is_some() {
                seen.stage = stage;
            }
            let one = (!dead(mob.action))
                .then(|| current(presentation, mob.entity_id, voice))
                .flatten();
            if one.is_none() && seen.current.is_some() {
                seen.consumed = u8::MAX;
            }
            seen.current = one.map(PhaseKey::from);
            let Some(one) = one else {
                continue;
            };
            let key = PhaseKey::from(one);
            if seen.phase != Some(key) {
                seen.phase = Some(key);
                seen.consumed = 0;
            }
            for (index, (offset, cue, side)) in (voice.markers)(one).into_iter().enumerate() {
                let tick = one.announced.phase_started_tick.wrapping_add(offset);
                if snapshot.server_tick.wrapping_sub(tick) >= 1 << 31
                    || seen.consumed & (1 << index) != 0
                {
                    continue;
                }
                // Even an inaudible, late or refused marker is consumed permanently.
                seen.consumed |= 1 << index;
                if fresh(snapshot.server_tick, tick, rate) {
                    enqueue(
                        pending,
                        mob,
                        voice,
                        cue,
                        if cue.tail() {
                            Owner::Tail(key)
                        } else {
                            Owner::Phase(key)
                        },
                        side,
                    );
                }
            }
        }
    }
    pub(super) fn footfalls(
        &mut self,
        mobs: &Query<&Mob>,
        snapshot: &Snapshot,
        pending: &mut Vec<Pending>,
    ) {
        for drawn in mobs {
            let Some((id, serial, contact)) = drawn.guardian_audio_contact() else {
                continue;
            };
            let Some(seen) = self.0.get_mut(&id) else {
                continue;
            };
            let before = seen.foot_serial.replace(serial);
            if before == Some(serial) {
                continue;
            }
            let Some(mob) = snapshot
                .mobs
                .iter()
                .find(|mob| mob.entity_id == id && mob.kind == MobKind::VargrGuardian)
            else {
                continue;
            };
            if dead(mob.action) {
                continue;
            }
            let gait = matches!(mob.action, MobAction::Idle | MobAction::Chase)
                || seen.current.is_some_and(|phase| {
                    phase.kind == EncounterMoveKind::CollarCharge
                        && phase.phase == MovePhase::Release
                });
            if gait {
                pending.push(Pending {
                    cue: CatalogueCue::Guardian(Cue::Footfall),
                    id,
                    target: BlowTarget::Mob(MobKind::VargrGuardian),
                    origin: contact,
                    follows: false,
                    offset: None,
                    owner: Some(
                        seen.current
                            .filter(|phase| {
                                phase.kind == EncounterMoveKind::CollarCharge
                                    && phase.phase == MovePhase::Release
                            })
                            .map_or(Owner::Gait, Owner::Phase),
                    ),
                });
            }
        }
    }
    pub(super) fn valid(&self, id: u64, owner: Owner) -> bool {
        let Some(seen) = self.0.get(&id) else {
            return false;
        };
        match owner {
            Owner::Phase(key) => seen.current == Some(key),
            // Phases of one instance only move forward, so identity is the whole test.
            Owner::Tail(key) => seen
                .current
                .is_some_and(|now| now.key == key.key && now.kind == key.kind),
            Owner::Death => seen.action.is_some_and(dead),
            Owner::Alive => seen.action.is_some_and(|action| !dead(action)),
            Owner::Gait => {
                seen.action
                    .is_some_and(|action| matches!(action, MobAction::Idle | MobAction::Chase))
                    || seen.current.is_some_and(|phase| {
                        phase.kind == EncounterMoveKind::CollarCharge
                            && phase.phase == MovePhase::Release
                    })
            }
        }
    }
}

#[cfg(test)]
pub(super) mod capture;
#[cfg(test)]
pub(super) mod tests;
