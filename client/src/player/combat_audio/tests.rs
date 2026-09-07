use super::*;
use crate::{
    audio::{MAX_SOURCES, Mixer, Sink},
    net::{BlowKind, BlowLanded, ChunkCoord, EntityState, MobState, SessionParams},
    world::{VoxelChunk, palette},
};
use std::sync::Arc;

struct Buffer(Vec<f32>);
impl Sink for Buffer {
    fn block(&mut self) -> &mut [f32] {
        &mut self.0
    }
}

fn fixture() -> (App, Arc<Mixer>) {
    let mixer = Arc::new(Mixer::new());
    mixer.set_format(8000, 2);
    let mut app = App::new();
    app.add_plugins(bevy::time::TimePlugin)
        .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            Duration::from_millis(10),
        ))
        .insert_resource(AudioMixer::from_shared_for_test(Arc::clone(&mixer)))
        .insert_resource(Session(SessionParams {
            clock: Default::default(),
            entity_id: 1,
            spawn: [4.0, 0.0, 4.0],
            world_seed: 1,
            tick_rate: 20,
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
        .init_resource::<ChunkStore>()
        .add_message::<super::super::combat::SwingSent>();
    app.world_mut()
        .spawn((WorldCamera, Transform::from_xyz(4.0, 1.5, 4.0)));
    register(&mut app);
    (app, mixer)
}

fn mob(kind: MobKind, action: MobAction) -> MobState {
    MobState {
        entity_id: 7,
        kind,
        pos: [10.0, 0.0, 4.0],
        vel: [0.0; 3],
        yaw: 0.0,
        health: 10,
        max_health: 10,
        action,
        target_entity_id: 1,
    }
}

fn snapshot(app: &mut App, mobs: Vec<MobState>) -> u32 {
    let mut snapshots = app.world_mut().resource_mut::<SnapshotBuffer>();
    let tick = snapshots.latest_tick().unwrap_or(0).wrapping_add(1);
    snapshots.accept(
        Snapshot {
            server_tick: tick,
            mobs,
            entities: vec![EntityState {
                entity_id: 1,
                pos: [4.0, 0.0, 4.0],
                vel: [0.0; 3],
                yaw: 0.0,
            }],
            ..Snapshot::default()
        },
        Instant::now(),
    );
    tick
}

fn blow(app: &mut App, target: BlowTarget, count: usize) {
    let snapshots = app.world().resource::<SnapshotBuffer>();
    let snapshot = snapshots.latest_snapshot().unwrap();
    let (id, position) = match target {
        BlowTarget::Player => (1, snapshot.entities[0].pos),
        BlowTarget::Mob(_) => (7, snapshot.mobs[0].pos),
    };
    let blow = BlowLanded {
        tick: snapshot.server_tick,
        attacker_entity_id: 0,
        target_entity_id: id,
        position,
        kind: BlowKind::Melee,
        target,
    };
    for _ in 0..count {
        app.world_mut()
            .resource_mut::<BlowInbox>()
            .push_for_test(blow, Instant::now());
    }
}

fn hear(app: &mut App, mixer: &Mixer, frames: usize) -> [f32; 2] {
    let mut energy = [0.0; 2];
    for _ in 0..frames {
        app.update();
        let mut buffer = Buffer(vec![0.0; 160]);
        mixer.render(&mut buffer);
        for pair in buffer.0.chunks_exact(2) {
            for side in 0..2 {
                energy[side] += pair[side] * pair[side];
            }
        }
    }
    energy
}

#[test]
fn only_the_landed_event_can_make_an_impact_not_a_swing_or_health_loss() {
    let (mut app, mixer) = fixture();
    snapshot(&mut app, vec![mob(MobKind::Draugr, MobAction::Idle)]);
    app.world_mut()
        .write_message(super::super::combat::SwingSent { item_id: 5 });
    assert_eq!(hear(&mut app, &mixer, 100), [0.0; 2]);
    let mut hurt = mob(MobKind::Draugr, MobAction::Idle);
    hurt.health = 1;
    snapshot(&mut app, vec![hurt]);
    assert_eq!(hear(&mut app, &mixer, 100), [0.0; 2]);
    blow(&mut app, BlowTarget::Mob(MobKind::Draugr), 1);
    assert!(hear(&mut app, &mixer, 100)[1] > 0.1);
    assert_eq!(hear(&mut app, &mixer, 100), [0.0; 2]);
}

#[test]
fn repeated_contacts_in_one_tick_claim_separate_sources_and_respect_sfx_gain() {
    for target in [
        BlowTarget::Player,
        BlowTarget::Mob(MobKind::Draugr),
        BlowTarget::Mob(MobKind::Vargr),
    ] {
        let (mut app, mixer) = fixture();
        let species = match target {
            BlowTarget::Mob(kind) => kind,
            _ => MobKind::Draugr,
        };
        snapshot(&mut app, vec![mob(species, MobAction::Idle)]);
        blow(&mut app, target, 2);
        app.update();
        assert_eq!(app.world().resource::<CombatAudio>().playing.len(), 2);
        mixer.set_gain(Bus::Sfx, 0.0);
        assert_eq!(hear(&mut app, &mixer, 100), [0.0; 2]);
        mixer.set_gain(Bus::Sfx, 1.0);
        blow(&mut app, target, 1);
        assert!(hear(&mut app, &mixer, 100).iter().sum::<f32>() > 0.1);
    }
}

#[test]
fn aggro_repeats_only_after_idle_and_windup_is_one_telegraph_per_transition() {
    for kind in [MobKind::Draugr, MobKind::Vargr] {
        let (mut app, mixer) = fixture();
        snapshot(&mut app, vec![mob(kind, MobAction::Chase)]);
        assert_eq!(
            hear(&mut app, &mixer, 100),
            [0.0; 2],
            "newly seen chasing is not an observed notice"
        );
        for _ in 0..2 {
            snapshot(&mut app, vec![mob(kind, MobAction::Idle)]);
            assert_eq!(hear(&mut app, &mixer, 100), [0.0; 2]);
            snapshot(&mut app, vec![mob(kind, MobAction::Chase)]);
            assert!(hear(&mut app, &mixer, 100)[1] > 0.1);
            snapshot(&mut app, vec![mob(kind, MobAction::Chase)]);
            assert_eq!(hear(&mut app, &mixer, 100), [0.0; 2]);
        }
        snapshot(&mut app, vec![mob(kind, MobAction::Windup)]);
        assert!(hear(&mut app, &mixer, 100)[1] > 0.1);
        snapshot(&mut app, vec![mob(kind, MobAction::Windup)]);
        assert_eq!(hear(&mut app, &mixer, 100), [0.0; 2]);
        snapshot(&mut app, vec![mob(kind, MobAction::Recovery)]);
        assert_eq!(hear(&mut app, &mixer, 100), [0.0; 2]);
        snapshot(&mut app, vec![mob(kind, MobAction::Windup)]);
        assert!(hear(&mut app, &mixer, 100)[1] > 0.1);
    }
}

/// The bosses are in this list for a different reason from the three beside them, and
/// the difference is worth one sentence: a deer, a villager and a horse have no hostile
/// voice and never will, while a boss has one this build has not been given. Both are
/// silent through every action state today, and both are audible when hit.
#[test]
fn passive_species_do_not_gain_hostile_voices_from_action_states() {
    for kind in [
        MobKind::Deer,
        MobKind::Villager,
        MobKind::Horse,
        MobKind::VargrGuardian,
        MobKind::DraugrKing,
    ] {
        let (mut app, mixer) = fixture();
        for action in [MobAction::Idle, MobAction::Chase, MobAction::Windup] {
            snapshot(&mut app, vec![mob(kind, action)]);
            assert_eq!(hear(&mut app, &mixer, 100), [0.0; 2]);
        }
        blow(&mut app, BlowTarget::Mob(kind), 1);
        assert!(hear(&mut app, &mixer, 100).iter().sum::<f32>() > 0.0);
    }
}

#[test]
fn snapshot_advance_hidden_target_and_malformed_pairing_never_reveal_a_blow() {
    let (mut app, mixer) = fixture();
    snapshot(&mut app, vec![mob(MobKind::Draugr, MobAction::Idle)]);
    blow(&mut app, BlowTarget::Mob(MobKind::Draugr), 1);
    snapshot(&mut app, vec![mob(MobKind::Draugr, MobAction::Idle)]);
    assert_eq!(hear(&mut app, &mixer, 100), [0.0; 2]);
    blow(&mut app, BlowTarget::Mob(MobKind::Vargr), 1);
    assert_eq!(hear(&mut app, &mixer, 100), [0.0; 2]);
    blow(&mut app, BlowTarget::Mob(MobKind::Draugr), 1);
    app.update();
    assert_eq!(app.world().resource::<CombatAudio>().playing.len(), 1);
    snapshot(&mut app, vec![]);
    assert_eq!(
        hear(&mut app, &mixer, 1),
        [0.0; 2],
        "queued samples of a hidden target survived"
    );
}

#[test]
fn actual_impacts_pan_at_the_event_position_and_are_muffled_by_the_world() {
    let render = |wall, yaw| {
        let (mut app, mixer) = fixture();
        if wall {
            let mut chunk = VoxelChunk::all_air(32);
            for y in 0..4 {
                for z in 2..7 {
                    chunk.set(7, y, z, palette::STONE);
                }
            }
            app.world_mut().resource_mut::<ChunkStore>().insert(
                ChunkCoord {
                    cx: 0,
                    cy: 0,
                    cz: 0,
                },
                chunk,
            );
        }
        for mut transform in app
            .world_mut()
            .query_filtered::<&mut Transform, With<WorldCamera>>()
            .iter_mut(app.world_mut())
        {
            transform.rotation = Quat::from_rotation_y(yaw);
        }
        snapshot(&mut app, vec![mob(MobKind::Draugr, MobAction::Idle)]);
        blow(&mut app, BlowTarget::Mob(MobKind::Draugr), 1);
        hear(&mut app, &mixer, 100)
    };
    let air = render(false, 0.0);
    let wall = render(true, 0.0);
    let turned = render(false, std::f32::consts::PI);
    assert!(air[1] > 0.1 && air[0] < air[1] * 0.01);
    assert!(turned[0] > 0.1 && turned[1] < turned[0] * 0.01);
    assert!(wall[1] < air[1] * 0.7);
}

#[test]
fn source_refusal_discards_events_without_stealing_voice_or_replaying_later() {
    let (mut app, mixer) = fixture();
    let voices: Vec<_> = (0..MAX_SOURCES)
        .map(|_| mixer.claim(Bus::Voice).unwrap())
        .collect();
    snapshot(&mut app, vec![mob(MobKind::Draugr, MobAction::Idle)]);
    blow(&mut app, BlowTarget::Mob(MobKind::Draugr), 12);
    assert_eq!(hear(&mut app, &mixer, 100), [0.0; 2]);
    assert!(voices.iter().all(|voice| voice.live()));
    assert!(app.world().resource::<CombatAudio>().playing.is_empty());
    drop(voices);
    assert_eq!(hear(&mut app, &mixer, 100), [0.0; 2]);
    blow(&mut app, BlowTarget::Mob(MobKind::Draugr), 1);
    assert!(hear(&mut app, &mixer, 100)[1] > 0.1);
}

#[test]
fn world_session_camera_and_device_changes_do_not_replay_old_combat() {
    for reason in 0..4 {
        let (mut app, mixer) = fixture();
        snapshot(&mut app, vec![mob(MobKind::Vargr, MobAction::Windup)]);
        app.update();
        assert_eq!(app.world().resource::<CombatAudio>().playing.len(), 1);
        match reason {
            0 => {
                super::super::reset_world(app.world_mut());
            }
            1 => {
                app.world_mut().remove_resource::<Session>();
            }
            2 => {
                let camera = app
                    .world_mut()
                    .query_filtered::<Entity, With<WorldCamera>>()
                    .single(app.world())
                    .unwrap();
                app.world_mut().despawn(camera);
            }
            _ => {
                mixer.set_format(44100, 2);
            }
        }
        assert_eq!(hear(&mut app, &mixer, 1), [0.0; 2]);
        assert!(app.world().resource::<CombatAudio>().playing.is_empty());
    }
}

#[test]
fn an_undrained_sound_expires_instead_of_waiting_for_a_device_forever() {
    let (mut app, mixer) = fixture();
    snapshot(&mut app, vec![mob(MobKind::Draugr, MobAction::Windup)]);
    app.update();
    app.world_mut().resource_mut::<CombatAudio>().playing[0].expires = Duration::ZERO;
    assert_eq!(hear(&mut app, &mixer, 1), [0.0; 2]);
    assert!(app.world().resource::<CombatAudio>().playing.is_empty());
}
