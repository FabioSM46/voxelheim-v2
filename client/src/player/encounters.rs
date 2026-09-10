//! Presentation of complete boss timelines on the newest received simulation tick.
//!
//! No local clock advances a phase. A delayed announcement is sampled where the
//! snapshot stream is now, never replayed from its beginning. If that tick is past
//! its declared window, retain an explicit waiting state without a damage cue; only
//! another authoritative timeline can name the next phase or end the instance.

use bevy::prelude::*;

#[cfg(test)]
mod capture;
mod cues;
pub(super) mod spells;
mod strikes;

pub(crate) fn is_spell(kind: crate::net::EncounterMoveKind) -> bool {
    use crate::net::EncounterMoveKind::*;
    matches!(
        kind,
        Burial | EdictOfTheGraves | SepulchreSpear | RequiemOfTheBuried
    )
}

use super::{ApplySnapshots, SnapshotBuffer};
use crate::net::{
    EncounterMove, EncounterTimeline, EncounterTimelineInbox, HazardVolume, MobAction, MobKind,
    MovePhase, Session, Snapshot,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Window {
    Upcoming,
    Current,
    AwaitingUpdate,
}

/// An identity names an instance, not a move kind: consecutive bites never share it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct MoveKey {
    pub encounter: u64,
    pub boss: u64,
    pub instance: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PresentedMove {
    pub key: MoveKey,
    pub boss_kind: MobKind,
    pub stage: u8,
    pub announced: EncounterMove,
    pub window: Window,
    pub progress: f32,
    pub remaining_ticks: u32,
}

impl PresentedMove {
    pub fn label_height(&self) -> f32 {
        super::mobs::body(self.boss_kind).height + 0.25
    }

    /// Geometry is copied from the announcement, never reconstructed from its name
    /// or aimed at the current player position. Recovery and expired windows draw none.
    pub fn hazards(&self) -> &[HazardVolume] {
        if self.window == Window::AwaitingUpdate || self.announced.phase == MovePhase::Recovery {
            &[]
        } else {
            &self.announced.hazards
        }
    }

    pub fn damaging(&self) -> bool {
        // The server announces each channel pulse for its interval, then resolves
        // contact on the last tick only. Release damages throughout its window.
        self.window == Window::Current
            && (self.announced.phase == MovePhase::Release
                || (self.announced.phase == MovePhase::Channel && self.remaining_ticks == 1))
    }
}

/// Bounded by the inbox's four encounters and the decoder's eight moves each.
#[derive(Resource, Debug, Default, PartialEq)]
pub(crate) struct EncounterPresentation(pub Vec<PresentedMove>);

pub(super) fn register(app: &mut App) {
    cues::register(app);
    spells::register(app);
    strikes::register(app);
    app.init_resource::<EncounterTimelineInbox>()
        .init_resource::<EncounterPresentation>()
        .add_systems(Update, reconcile.after(ApplySnapshots));
}

pub(super) fn reconcile(
    session: Option<Res<Session>>,
    inbox: Res<EncounterTimelineInbox>,
    snapshots: Res<SnapshotBuffer>,
    mut presentation: ResMut<EncounterPresentation>,
) {
    if session
        .as_ref()
        .is_some_and(|session| !session.is_changed())
        && !inbox.is_changed()
        && !snapshots.is_changed()
    {
        return;
    }
    let next = if session.is_some() {
        project(inbox.live(), snapshots.latest_snapshot())
    } else {
        Vec::new()
    };
    if presentation.0 != next {
        presentation.0 = next;
    }
}

fn project(timelines: &[EncounterTimeline], snapshot: Option<&Snapshot>) -> Vec<PresentedMove> {
    let Some(snapshot) = snapshot else {
        return Vec::new();
    };
    let mut result = Vec::new();
    for timeline in timelines {
        if !snapshot.mobs.iter().any(|mob| {
            mob.entity_id == timeline.boss_entity_id
                && mob.kind == timeline.boss
                && !matches!(mob.action, MobAction::Dying | MobAction::Corpse)
        }) {
            continue;
        }
        for announced in &timeline.moves {
            if announced.ended.is_some() {
                continue;
            }
            let (window, progress) = sample_window(snapshot.server_tick, announced);
            result.push(PresentedMove {
                key: MoveKey {
                    encounter: timeline.encounter_id,
                    boss: timeline.boss_entity_id,
                    instance: announced.move_instance_id,
                },
                boss_kind: timeline.boss,
                stage: timeline.phase,
                announced: announced.clone(),
                window,
                progress,
                remaining_ticks: announced.phase_ticks.saturating_sub(
                    snapshot
                        .server_tick
                        .wrapping_sub(announced.phase_started_tick),
                ),
            });
        }
    }
    result
}

fn sample_window(tick: u32, announced: &EncounterMove) -> (Window, f32) {
    let elapsed = tick.wrapping_sub(announced.phase_started_tick);
    // The same serial-number ordering as SnapshotBuffer, including u32 wrap.
    // A timeline can arrive just before the corresponding snapshot.
    if elapsed >= 1 << 31 {
        (Window::Upcoming, 0.0)
    } else if elapsed >= announced.phase_ticks {
        (Window::AwaitingUpdate, 1.0)
    } else {
        (
            Window::Current,
            elapsed as f32 / announced.phase_ticks as f32,
        )
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::net::{EncounterMoveKind, HazardShape, MobState, MoveEnd};

    pub(crate) fn timeline() -> EncounterTimeline {
        EncounterTimeline {
            encounter_id: 5,
            boss_entity_id: 9,
            boss: MobKind::VargrGuardian,
            phase: 2,
            moves: vec![EncounterMove {
                move_instance_id: 11,
                kind: EncounterMoveKind::CollarCharge,
                phase: MovePhase::Telegraph,
                phase_started_tick: 100,
                phase_ticks: 20,
                target_entity_id: None,
                aim: Some([0.0, 0.0, -1.0]),
                hazards: vec![HazardVolume {
                    shape: HazardShape::Line { half_width: 1.0 },
                    origin: [3.0, 2.0, 1.0],
                    direction: [0.0, 0.0, -1.0],
                    radius: 8.0,
                    height: 2.0,
                }],
                combo: None,
                pulse: None,
                interruptible: false,
                ended: None,
            }],
        }
    }

    pub(crate) fn snapshot(tick: u32) -> Snapshot {
        Snapshot {
            server_tick: tick,
            mobs: vec![MobState {
                entity_id: 9,
                kind: MobKind::VargrGuardian,
                pos: [0.0; 3],
                vel: [0.0; 3],
                yaw: 0.0,
                health: 0, // Health is deliberately not a death signal.
                max_health: 100,
                action: MobAction::Windup,
                target_entity_id: 7,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn late_combo_step_survives_replacement_without_replaying_prior_blows() {
        let mut next = timeline();
        next.moves[0].kind = EncounterMoveKind::ThreeTolls;
        next.moves[0].combo = Some((3, 3));
        let snap = snapshot(110);
        let shown = project(&[next.clone()], Some(&snap));
        assert_eq!(shown[0].announced.combo, Some((3, 3)));
        next.moves[0].phase = MovePhase::Recovery;
        assert_eq!(
            project(&[next.clone()], Some(&snap))[0].announced.combo,
            Some((3, 3))
        );
        next.moves[0].move_instance_id += 1;
        next.moves[0].kind = EncounterMoveKind::BiteAndTear;
        next.moves[0].combo = Some((1, 2));
        let replaced = project(&[next.clone()], Some(&snap));
        assert_ne!(shown[0].key, replaced[0].key);
        assert_eq!(replaced[0].announced.combo, Some((1, 2)));
        next.moves[0].ended = Some(MoveEnd::Cancelled);
        assert!(project(&[next], Some(&snap)).is_empty());
        assert!(project(&[], Some(&snap)).is_empty());
    }

    #[test]
    fn late_telegraph_resumes_at_received_tick_and_keeps_locked_geometry() {
        let state = timeline();
        let presented = project(std::slice::from_ref(&state), Some(&snapshot(115)));
        assert_eq!(presented.len(), 1);
        assert_eq!(presented[0].progress, 0.75);
        assert_eq!(presented[0].hazards(), state.moves[0].hazards);
        assert!(!presented[0].damaging());
        assert_eq!(presented[0].stage, 2);
    }

    #[test]
    fn expired_release_never_replays_damage_or_invents_recovery() {
        let mut state = timeline();
        state.moves[0].phase = MovePhase::Release;
        for tick in [120, 121, 2000] {
            let shown = project(std::slice::from_ref(&state), Some(&snapshot(tick)));
            assert_eq!(shown[0].window, Window::AwaitingUpdate);
            assert_eq!(shown[0].announced.phase, MovePhase::Release);
            assert!(shown[0].hazards().is_empty());
            assert!(!shown[0].damaging());
        }
        let shown = project(&[state], Some(&snapshot(119)));
        assert!(shown[0].damaging());
        assert_eq!(shown[0].hazards().len(), 1);
    }

    #[test]
    fn future_phase_and_tick_wrap_use_the_snapshot_serial_order() {
        let mut announced = timeline().moves.remove(0);
        assert_eq!(sample_window(99, &announced), (Window::Upcoming, 0.0));
        announced.phase_started_tick = u32::MAX - 9;
        assert_eq!(sample_window(0, &announced), (Window::Current, 0.5));
        assert_eq!(sample_window(10, &announced), (Window::AwaitingUpdate, 1.0));
    }

    #[test]
    fn cancellation_empty_replacement_and_visibility_remove_every_cue() {
        for end in [
            MoveEnd::Cancelled,
            MoveEnd::Completed,
            MoveEnd::Interrupted,
            MoveEnd::Unknown,
        ] {
            let mut state = timeline();
            state.moves[0].ended = Some(end);
            assert!(project(&[state], Some(&snapshot(110))).is_empty());
        }
        let mut state = timeline();
        state.moves.clear();
        assert!(project(&[state], Some(&snapshot(110))).is_empty());
        assert!(project(&[timeline()], None).is_empty());
        for action in [MobAction::Corpse, MobAction::Dying] {
            let mut snap = snapshot(110);
            snap.mobs[0].action = action;
            assert!(project(&[timeline()], Some(&snap)).is_empty());
        }
        let mut snap = snapshot(110);
        snap.mobs.clear();
        assert!(project(&[timeline()], Some(&snap)).is_empty());
        snap = snapshot(110);
        snap.mobs[0].kind = MobKind::DraugrKing;
        assert!(project(&[timeline()], Some(&snap)).is_empty());
    }

    #[test]
    fn newest_complete_state_replaces_instances_and_channel_pulses() {
        let mut inbox = EncounterTimelineInbox::default();
        inbox.push(timeline());
        let old = project(inbox.live(), Some(&snapshot(110)));
        let mut next = timeline();
        next.moves[0].move_instance_id += 1;
        next.moves[0].phase = MovePhase::Channel;
        next.moves[0].pulse = Some((2, 3));
        next.moves[0].interruptible = true;
        inbox.push(next);
        let new = project(inbox.live(), Some(&snapshot(115)));
        assert_eq!(new.len(), 1);
        assert_ne!(old[0].key, new[0].key);
        assert_eq!(new[0].announced.pulse, Some((2, 3)));
        assert!(!new[0].damaging());
        assert!(project(inbox.live(), Some(&snapshot(119)))[0].damaging());
        assert!(new[0].announced.interruptible);
        assert_eq!(new, project(inbox.live(), Some(&snapshot(115))));
    }

    #[test]
    fn reconciliation_clears_on_disconnect_and_rejects_old_snapshots() {
        use crate::net::SessionParams;
        use std::time::Instant;
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .init_resource::<SnapshotBuffer>()
            .insert_resource(Session(SessionParams {
                clock: Default::default(),
                entity_id: 7,
                spawn: [0.0; 3],
                world_seed: 1,
                tick_rate: 20,
                chunk_size: 32,
                view_distance: 8,
                inventory_slots: 37,
                hotbar_slots: 9,
                equipment_slots: 4,
                player_token: crate::net::ANY_TOKEN,
                voice_range_blocks: 0.0,
            }));
        register(&mut app);
        app.world_mut()
            .resource_mut::<EncounterTimelineInbox>()
            .push(timeline());
        assert!(
            app.world_mut()
                .resource_mut::<SnapshotBuffer>()
                .accept(snapshot(115), Instant::now())
        );
        app.update();
        let shown = app.world().resource::<EncounterPresentation>().0.clone();
        assert_eq!(shown[0].progress, 0.75);
        assert!(
            !app.world_mut()
                .resource_mut::<SnapshotBuffer>()
                .accept(snapshot(101), Instant::now())
        );
        app.update();
        assert_eq!(app.world().resource::<EncounterPresentation>().0, shown);
        app.world_mut().remove_resource::<Session>();
        app.update();
        assert!(app.world().resource::<EncounterPresentation>().0.is_empty());
    }
}
