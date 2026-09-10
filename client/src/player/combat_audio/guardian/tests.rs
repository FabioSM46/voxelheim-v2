use super::super::{CombatAudio, register};
use super::*;
use crate::net::{EncounterMoveKind as Move, MobAction as Action, MovePhase as Phase};
use crate::{
    audio::{AudioMixer, Bus, Mixer, Sink},
    net::{BlowInbox, BlowKind, BlowLanded, EncounterTimeline, MoveEnd, Session, SessionParams},
    player::{SnapshotBuffer, WorldCamera, encounters},
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub(in super::super) struct Buffer(pub Vec<f32>);
impl Sink for Buffer {
    fn block(&mut self) -> &mut [f32] {
        &mut self.0
    }
}

pub(in super::super) fn fixture(rate: u32) -> (App, Arc<Mixer>) {
    let mixer = Arc::new(Mixer::new());
    mixer.set_format(rate, 2);
    let mut app = App::new();
    app.add_plugins(bevy::time::TimePlugin)
        .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            Duration::from_millis(10),
        ))
        .insert_resource(AudioMixer::from_shared_for_test(Arc::clone(&mixer)))
        .insert_resource(Session(SessionParams {
            clock: Default::default(),
            entity_id: 7,
            spawn: [0.0; 3],
            world_seed: 1,
            tick_rate: 60,
            chunk_size: 32,
            view_distance: 8,
            inventory_slots: 37,
            hotbar_slots: 9,
            equipment_slots: 4,
            player_token: crate::net::ANY_TOKEN,
            voice_range_blocks: 32.0,
        }))
        .init_resource::<SnapshotBuffer>()
        .init_resource::<BlowInbox>()
        .init_resource::<EncounterTimelineInbox>()
        .init_resource::<Assets<Mesh>>()
        .init_resource::<Assets<StandardMaterial>>();
    encounters::register(&mut app);
    app.world_mut()
        .spawn((WorldCamera, Transform::from_xyz(0.0, 1.25, 3.0)));
    register(&mut app);
    (app, mixer)
}

pub(super) fn timeline(
    kind: EncounterMoveKind,
    phase: MovePhase,
    combo: Option<(u8, u8)>,
    start: u32,
) -> EncounterTimeline {
    let mut state = encounters::tests::timeline();
    state.phase = if combo.is_some() || kind == Move::BonebreakerJaws {
        2
    } else {
        1
    };
    let one = &mut state.moves[0];
    one.kind = kind;
    one.phase = phase;
    one.phase_started_tick = start;
    one.combo = combo;
    // Server catalogue at 60 Hz. Tests also replace these with other announced
    // durations/rates to check that production routing owns no copy of them.
    one.phase_ticks = match (kind, phase) {
        (Move::BiteAndTear, Phase::Telegraph) => 54,
        (Move::BiteAndTear, Phase::Release) => 12,
        (Move::PrisonerClaws, Phase::Telegraph) => 60,
        (Move::PrisonerClaws, Phase::Release) => 18,
        (Move::CollarCharge, Phase::Telegraph) => 72,
        (Move::CollarCharge, Phase::Release) => 54,
        (Move::PredatorLeap, Phase::Telegraph) => 60,
        (Move::PredatorLeap, Phase::Release) => 36,
        (Move::PredatorLeap, Phase::Recovery) => 96,
        (Move::BonebreakerJaws, Phase::Telegraph) => 90,
        (Move::BonebreakerJaws, Phase::Release) => 18,
        (Move::BonebreakerJaws, Phase::Recovery) => 150,
        (_, Phase::Recovery) if combo == Some((1, 2)) => 24,
        (Move::PrisonerClaws, Phase::Recovery) if combo.is_none() => 84,
        _ => 108,
    };
    one.hazards = vec![crate::net::HazardVolume {
        shape: match kind {
            Move::CollarCharge => crate::net::HazardShape::Line { half_width: 1.4 },
            Move::PredatorLeap => crate::net::HazardShape::Disc,
            Move::PrisonerClaws => crate::net::HazardShape::Cone { half_angle: 1.05 },
            Move::BonebreakerJaws => crate::net::HazardShape::Cone { half_angle: 0.38 },
            _ => crate::net::HazardShape::Cone { half_angle: 0.70 },
        },
        origin: [0.0, if kind == Move::PredatorLeap { 1.5 } else { 0.9 }, 0.0],
        direction: [0.0, 0.0, -1.0],
        radius: match kind {
            Move::CollarCharge => 9.9,
            Move::PrisonerClaws => 3.4,
            Move::BonebreakerJaws => 3.6,
            _ => 3.0,
        },
        height: match kind {
            Move::PredatorLeap => 3.0,
            Move::CollarCharge => 2.4,
            _ => 2.2,
        },
    }];
    state
}

pub(super) fn advance(
    app: &mut App,
    tick: u32,
    action: MobAction,
    timeline: Option<EncounterTimeline>,
) {
    let mut snap = encounters::tests::snapshot(tick);
    snap.mobs[0].action = action;
    app.world_mut()
        .resource_mut::<SnapshotBuffer>()
        .accept(snap, Instant::now());
    if let Some(timeline) = timeline {
        app.world_mut()
            .resource_mut::<EncounterTimelineInbox>()
            .push(timeline);
    }
    app.update();
}
fn count(app: &App) -> usize {
    app.world().resource::<CombatAudio>().playing.len()
}
fn output(app: &mut App, mixer: &Mixer, frames: usize) -> f32 {
    let mut energy = 0.0;
    for _ in 0..frames {
        app.update();
        let mut buffer = Buffer(vec![0.0; (mixer.sample_rate() / 100 * 2) as usize]);
        mixer.render(&mut buffer);
        energy += buffer.0.iter().map(|value| value * value).sum::<f32>();
    }
    energy
}

#[test]
fn each_gesture_renders_once_and_generic_windup_cannot_double_it() {
    use Move::*;
    let cases = [
        (BiteAndTear, Some((1, 2))),
        (BiteAndTear, Some((2, 2))),
        (PrisonerClaws, None),
        (PrisonerClaws, Some((1, 2))),
        (PrisonerClaws, Some((2, 2))),
        (CollarCharge, None),
        (PredatorLeap, None),
        (BonebreakerJaws, None),
    ];
    for (kind, combo) in cases {
        for phase in [Phase::Telegraph, Phase::Release, Phase::Recovery] {
            let (mut app, mixer) = fixture(8000);
            let state = timeline(kind, phase, combo, 100);
            let tick = if kind == CollarCharge && phase == Phase::Telegraph {
                112
            } else {
                100
            };
            advance(&mut app, tick, Action::Windup, Some(state));
            assert_eq!(
                count(&app),
                1,
                "{kind:?} {phase:?}: one gesture, no generic bark"
            );
            assert!(output(&mut app, &mixer, 110) > 0.05);
            assert_eq!(count(&app), 0);
            assert_eq!(
                output(&mut app, &mixer, 20),
                0.0,
                "held snapshot replayed {kind:?} {phase:?}"
            );
        }
    }
    let (mut app, mixer) = fixture(8000);
    advance(&mut app, 100, Action::Windup, None);
    assert_eq!(
        output(&mut app, &mixer, 20),
        0.0,
        "no timeline means no named attack"
    );
}

#[test]
fn recipes_are_finite_bounded_and_rate_specific_with_no_claim_of_audition() {
    for rate in [8000, 44100, 48000, 192000] {
        for cue in sounds::CUES {
            let baked = cue.describe().bake(cue.seconds(), rate, 19).unwrap();
            assert_eq!(baked.sample_rate(), rate);
            assert_eq!(baked.samples().first(), Some(&0.0));
            assert_eq!(baked.samples().last(), Some(&0.0));
            assert!(
                baked
                    .samples()
                    .iter()
                    .all(|v| v.is_finite() && v.abs() < 0.99),
                "clipping {cue:?}"
            );
            assert!(
                baked.samples().iter().any(|v| v.abs() > 0.05),
                "inaudible recipe {cue:?}"
            );
        }
    }
}

#[test]
fn leap_landing_is_only_the_final_release_tick_and_tail_survives_recovery() {
    let (mut app, mixer) = fixture(8000);
    let state = timeline(Move::PredatorLeap, Phase::Release, None, 100);
    advance(&mut app, 120, Action::Windup, Some(state));
    assert_eq!(
        count(&app),
        0,
        "late entry does not replay launch or anticipate landing"
    );
    advance(&mut app, 134, Action::Windup, None);
    assert_eq!(count(&app), 0);
    advance(&mut app, 135, Action::Windup, None);
    assert_eq!(count(&app), 1);
    let recovery = timeline(Move::PredatorLeap, Phase::Recovery, None, 136);
    advance(&mut app, 136, Action::Recovery, Some(recovery));
    assert_eq!(
        count(&app),
        2,
        "landing rings into recovery alongside breath"
    );
    assert!(output(&mut app, &mixer, 20) > 0.05);
}

#[test]
fn missed_markers_never_catch_up_but_future_markers_still_play() {
    let (mut app, mixer) = fixture(8000);
    let state = timeline(Move::CollarCharge, Phase::Telegraph, None, 100);
    advance(&mut app, 124, Action::Windup, Some(state));
    assert_eq!(count(&app), 0, "first scrape too old");
    advance(&mut app, 141, Action::Windup, None);
    assert_eq!(count(&app), 1, "second scrape belongs to this tick");
    output(&mut app, &mixer, 40);
    advance(&mut app, 158, Action::Windup, None);
    assert_eq!(count(&app), 1, "taut chain follows both scrapes");
    output(&mut app, &mixer, 50);
    advance(&mut app, 172, Action::Windup, None);
    assert_eq!(count(&app), 0, "expired phase is silent");
    for rate in [1, 2, 9, 10, 20, 60] {
        assert!(fresh(100, 100, rate));
        assert!(!fresh(100 + u32::from(rate) / 10 + 1, 100, rate));
    }
    assert!(fresh(0, u32::MAX, 60));
    assert!(!fresh(u32::MAX, 0, 60));
}

#[test]
fn late_combo_uses_authoritative_second_side_and_never_counts_instances() {
    let (mut app, _) = fixture(8000);
    let state = timeline(Move::PrisonerClaws, Phase::Release, Some((2, 2)), 100);
    advance(&mut app, 100, Action::Windup, Some(state));
    let active = &app.world().resource::<CombatAudio>().playing[0];
    assert!(
        active.origin.x > 0.5,
        "second claw is the right paw even on first sight"
    );
    let mut first = timeline(Move::PrisonerClaws, Phase::Release, Some((1, 2)), 101);
    first.moves[0].move_instance_id = 44;
    advance(&mut app, 101, Action::Windup, Some(first));
    assert_eq!(count(&app), 1, "replacement cancels old gesture");
    assert!(app.world().resource::<CombatAudio>().playing[0].origin.x < -0.5);
}

#[test]
fn cancellation_replacement_empty_expiry_and_despawn_discard_owned_rings() {
    for end in [
        Some(MoveEnd::Cancelled),
        Some(MoveEnd::Interrupted),
        Some(MoveEnd::Completed),
        None,
    ] {
        let (mut app, mixer) = fixture(8000);
        let state = timeline(Move::BonebreakerJaws, Phase::Telegraph, None, 100);
        advance(&mut app, 100, Action::Windup, Some(state.clone()));
        assert_eq!(count(&app), 1);
        let mut cancelled = state.clone();
        if let Some(end) = end {
            cancelled.moves[0].ended = Some(end);
        } else {
            cancelled.moves.clear();
        }
        advance(&mut app, 101, Action::Idle, Some(cancelled));
        assert_eq!(count(&app), 0);
        assert_eq!(output(&mut app, &mixer, 5), 0.0);
        // Even a stale reappearance of the same phase cannot resurrect its onset.
        advance(&mut app, 102, Action::Windup, Some(state));
        assert_eq!(count(&app), 0);
    }
    let (mut app, mixer) = fixture(8000);
    let state = timeline(Move::BiteAndTear, Phase::Release, Some((1, 2)), 100);
    advance(&mut app, 100, Action::Windup, Some(state));
    advance(&mut app, 112, Action::Windup, None);
    assert_eq!(output(&mut app, &mixer, 3), 0.0);
    app.world_mut().resource_mut::<SnapshotBuffer>().accept(
        Snapshot {
            server_tick: 113,
            ..default()
        },
        Instant::now(),
    );
    app.update();
    assert!(app.world().resource::<CombatAudio>().guardian.0.is_empty());
    app.world_mut().remove_resource::<Session>();
    app.update();
    assert_eq!(count(&app), 0);
}

#[test]
fn observed_notice_phase_change_and_direct_corpse_transition_are_once_only() {
    let (mut app, mixer) = fixture(8000);
    advance(&mut app, 100, Action::Chase, None);
    assert_eq!(count(&app), 0);
    advance(&mut app, 101, Action::Idle, None);
    advance(&mut app, 102, Action::Chase, None);
    assert_eq!(count(&app), 1);
    assert!(output(&mut app, &mixer, 100) > 0.05);
    let mut state = timeline(Move::BiteAndTear, Phase::Telegraph, None, 100);
    state.moves.clear();
    state.phase = 1;
    advance(&mut app, 103, Action::Idle, Some(state.clone()));
    state.phase = 2;
    advance(&mut app, 104, Action::Idle, Some(state));
    assert_eq!(
        count(&app),
        1,
        "tear comes from observed ordinal, not a move or health"
    );
    output(&mut app, &mixer, 90);
    advance(&mut app, 105, Action::Corpse, None);
    assert_eq!(count(&app), 1, "server sends live directly to corpse");
    assert!(output(&mut app, &mixer, 110) > 0.05);
    advance(&mut app, 106, Action::Corpse, None);
    assert_eq!(count(&app), 0);
    let (mut app, _) = fixture(8000);
    advance(&mut app, 100, Action::Corpse, None);
    assert_eq!(
        count(&app),
        0,
        "already-dead body does not replay its death"
    );
}

#[test]
fn unavailable_output_and_muting_consume_markers_without_later_replay() {
    for unavailable in [0, 1, 2] {
        let (mut app, mixer) = fixture(8000);
        if unavailable == 0 {
            app.world_mut().remove_resource::<AudioMixer>();
        }
        if unavailable == 1 {
            let world = app.world_mut();
            let entities: Vec<_> = world
                .query_filtered::<Entity, With<WorldCamera>>()
                .iter(world)
                .collect();
            for entity in entities {
                world.despawn(entity);
            }
        }
        if unavailable == 2 {
            mixer.set_gain(Bus::Sfx, 0.0);
        }
        advance(
            &mut app,
            100,
            Action::Windup,
            Some(timeline(
                Move::BiteAndTear,
                Phase::Release,
                Some((1, 2)),
                100,
            )),
        );
        output(&mut app, &mixer, 100);
        app.world_mut()
            .insert_resource(AudioMixer::from_shared_for_test(Arc::clone(&mixer)));
        if unavailable == 1 {
            app.world_mut()
                .spawn((WorldCamera, Transform::from_xyz(0.0, 1.25, 3.0)));
        }
        mixer.set_gain(Bus::Sfx, 1.0);
        assert_eq!(output(&mut app, &mixer, 20), 0.0);
    }
}

#[test]
fn only_blow_landed_can_play_a_confirmed_target_contact() {
    let (mut app, mixer) = fixture(8000);
    advance(&mut app, 100, Action::Windup, None);
    // Health is zero in the fixture already, deliberately proving no death inference.
    assert_eq!(count(&app), 0);
    app.world_mut().resource_mut::<BlowInbox>().push_for_test(
        BlowLanded {
            tick: 100,
            attacker_entity_id: 7,
            target_entity_id: 9,
            position: [0.0; 3],
            kind: BlowKind::Melee,
            target: BlowTarget::Mob(MobKind::VargrGuardian),
        },
        Instant::now(),
    );
    app.update();
    assert_eq!(count(&app), 1);
    assert!(
        app.world().resource::<CombatAudio>().playing[0]
            .owner
            .is_none()
    );
    assert!(output(&mut app, &mixer, 50) > 0.05);
    assert_eq!(output(&mut app, &mixer, 20), 0.0);
}

#[test]
fn party_gestures_are_capped_and_leave_all_eight_reserved_voice_slots() {
    let (mut app, mixer) = fixture(8000);
    let mut snap = encounters::tests::snapshot(100);
    let original = snap.mobs[0];
    snap.mobs.clear();
    for id in 9..13 {
        let mut mob = original;
        mob.entity_id = id;
        snap.mobs.push(mob);
        let mut state = timeline(Move::PredatorLeap, Phase::Release, None, 100);
        state.boss_entity_id = id;
        state.encounter_id = id;
        // A one-tick release places both physical markers on the only announced tick.
        state.moves[0].phase_ticks = 1;
        app.world_mut()
            .resource_mut::<EncounterTimelineInbox>()
            .push(state);
    }
    app.world_mut()
        .resource_mut::<SnapshotBuffer>()
        .accept(snap, Instant::now());
    app.update();
    assert_eq!(count(&app), MAX_BOSS_SOURCES);
    for id in 9..13 {
        assert!(
            app.world()
                .resource::<CombatAudio>()
                .playing
                .iter()
                .filter(|a| a.id == id)
                .count()
                <= MAX_PER_BOSS
        );
    }
    let voices: Vec<_> = (0..8)
        .map(|_| mixer.claim(Bus::Voice).expect("reserved voice slot"))
        .collect();
    for voice in &voices {
        assert_eq!(voice.push(&[0.02; 160]), 160);
    }
    let mut buffer = Buffer(vec![0.0; 160]);
    mixer.render(&mut buffer);
    assert!(buffer.0.iter().any(|sample| sample.abs() > 0.02));
}

#[test]
fn a_refused_gesture_is_not_queued_for_when_the_party_frees_a_slot() {
    let (mut app, mixer) = fixture(8000);
    let held: Vec<_> = (0..8).map(|_| mixer.claim(Bus::Sfx).unwrap()).collect();
    advance(
        &mut app,
        100,
        Action::Windup,
        Some(timeline(Move::BonebreakerJaws, Phase::Telegraph, None, 100)),
    );
    assert_eq!(count(&app), 0);
    drop(held);
    assert_eq!(output(&mut app, &mixer, 20), 0.0);
    advance(&mut app, 101, Action::Windup, None);
    assert_eq!(count(&app), 0);
}

#[test]
fn output_rate_change_cancels_old_ring_and_bakes_only_for_new_markers() {
    let (mut app, mixer) = fixture(8000);
    advance(
        &mut app,
        100,
        Action::Windup,
        Some(timeline(Move::BonebreakerJaws, Phase::Telegraph, None, 100)),
    );
    assert_eq!(count(&app), 1);
    mixer.set_format(48000, 2);
    assert_eq!(output(&mut app, &mixer, 2), 0.0);
    let state = app.world().resource::<CombatAudio>();
    assert_eq!(state.rate, 48000);
    assert!(
        state
            .palette
            .iter()
            .all(|(_, baked)| baked.sample_rate() == 48000)
    );
    advance(
        &mut app,
        101,
        Action::Windup,
        Some(timeline(Move::BonebreakerJaws, Phase::Release, None, 101)),
    );
    assert!(output(&mut app, &mixer, 10) > 0.05);
}

#[test]
fn duration_changes_and_tick_wrap_do_not_invent_fixed_charge_or_combo_timers() {
    for ticks in [1, 3, 17, 180] {
        let (mut app, _) = fixture(8000);
        let start = u32::MAX - 2;
        let mut state = timeline(Move::CollarCharge, Phase::Recovery, None, start);
        state.moves[0].phase_ticks = ticks;
        advance(&mut app, start, Action::Recovery, Some(state));
        assert_eq!(
            count(&app),
            1,
            "same physical stop for every announced duration"
        );
        advance(&mut app, start.wrapping_add(ticks), Action::Recovery, None);
        assert_eq!(count(&app), 0);
    }
}

#[test]
fn muting_leaves_the_same_announced_hazard_meshes_and_authoritative_state() {
    for kind in [
        Move::BiteAndTear,
        Move::PrisonerClaws,
        Move::CollarCharge,
        Move::PredatorLeap,
        Move::BonebreakerJaws,
    ] {
        for phase in [Phase::Telegraph, Phase::Release, Phase::Recovery] {
            let mut observations = Vec::new();
            for gain in [0.0, 1.0] {
                let (mut app, mixer) = fixture(8000);
                mixer.set_gain(Bus::Sfx, gain);
                advance(
                    &mut app,
                    100,
                    Action::Windup,
                    Some(timeline(kind, phase, Some((2, 2)), 100)),
                );
                let volumes = app.world().resource::<EncounterPresentation>().0[0]
                    .hazards()
                    .to_vec();
                let mut query = app.world_mut().query::<&Mesh3d>();
                let geometry = query
                    .iter(app.world())
                    .map(|mesh| {
                        let asset = app.world().resource::<Assets<Mesh>>().get(&mesh.0).unwrap();
                        let Some(bevy::mesh::VertexAttributeValues::Float32x3(points)) =
                            asset.attribute(Mesh::ATTRIBUTE_POSITION)
                        else {
                            panic!("cue positions");
                        };
                        points.clone()
                    })
                    .collect::<Vec<_>>();
                if phase != Phase::Recovery {
                    assert!(!geometry.is_empty());
                }
                if gain == 0.0 {
                    assert_eq!(output(&mut app, &mixer, 10), 0.0);
                }
                observations.push((volumes, geometry));
            }
            assert_eq!(
                observations[0], observations[1],
                "muting changed {kind:?} {phase:?}"
            );
        }
    }
}

/// The production rig systems supply the actual Mob and guardian Motion sampler.
/// This is shared with the offline walking/charge export, not a replacement gait.
pub(in super::super) fn rig_fixture(rate: u32) -> (App, Arc<Mixer>) {
    let (mut app, mixer) = fixture(rate);
    app.insert_resource(crate::player::InputMode::Playing)
        .add_systems(Startup, crate::player::mobs::create_visuals)
        .add_systems(
            Update,
            (
                crate::player::mobs::apply_snapshots,
                ApplyDeferred,
                crate::player::mobs::animate,
            )
                .chain()
                .in_set(crate::player::ApplySnapshots),
        )
        .add_systems(
            Update,
            crate::player::mobs::pose_encounters.after(encounters::reconcile),
        )
        .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            Duration::from_secs_f64(1.0 / 60.0),
        ));
    (app, mixer)
}

#[test]
fn production_gait_contacts_reach_audio_without_a_second_gait_or_attack_hit_detector() {
    let (mut app, mixer) = rig_fixture(8000);
    let mut footsteps = 0;
    for frame in 0..120 {
        let mut snapshot = encounters::tests::snapshot(100 + frame);
        snapshot.mobs[0].action = Action::Chase;
        snapshot.mobs[0].pos = [0.0, 0.0, -(frame as f32) / 60.0];
        app.world_mut()
            .resource_mut::<SnapshotBuffer>()
            .accept(snapshot, Instant::now() - Duration::from_millis(100));
        app.update();
        footsteps += app
            .world()
            .resource::<CombatAudio>()
            .started
            .iter()
            .filter(|(_, cue, _)| *cue == CatalogueCue::Guardian(Cue::Footfall))
            .count();
        let mut buffer = Buffer(vec![0.0; 266]);
        mixer.render(&mut buffer);
    }
    assert!(
        footsteps > 3 && footsteps < 60,
        "actual support events: {footsteps}"
    );
    let mut snapshot = encounters::tests::snapshot(220);
    snapshot.mobs[0].pos = [0.0, 8.0, -2.0]; // Reconciliation, not a stomp.
    snapshot.mobs[0].action = Action::Windup;
    app.world_mut()
        .resource_mut::<SnapshotBuffer>()
        .accept(snapshot, Instant::now() - Duration::from_millis(100));
    app.update();
    assert!(app.world().resource::<CombatAudio>().started.is_empty());
}
