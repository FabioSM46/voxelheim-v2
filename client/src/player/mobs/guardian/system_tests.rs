//! Observable production-system checks: the rig under the real inbox consumer.
use super::*;
use crate::net::{EncounterMoveKind, EncounterTimelineInbox, SnapshotInbox};
use crate::player::encounters::tests::{snapshot, timeline};
use bevy::asset::AssetPlugin;
use bevy::time::TimeUpdateStrategy;

fn app() -> App {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, AssetPlugin::default()))
        .init_asset::<Mesh>()
        .init_asset::<StandardMaterial>()
        .insert_resource(super::super::tests::session())
        .add_plugins(crate::player::PlayerPlugin)
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            16,
        )));
    app
}
fn deliver(app: &mut App, tick: u32, action: MobAction) {
    let mut snap = snapshot(tick);
    snap.mobs[0].action = action;
    snap.mobs[0].pos = [2.0, 4.0, 1.0];
    snap.mobs[0].yaw = 0.2;
    app.world_mut()
        .resource_mut::<SnapshotInbox>()
        .push(snap, Instant::now() - Duration::from_millis(100));
}
fn pose(app: &mut App) -> [Transform; 19] {
    let mut result = [Transform::IDENTITY; 19];
    let mut query = app.world_mut().query::<(&MobVisual, &Transform)>();
    let mut found = 0;
    for (part, transform) in query.iter(app.world()) {
        if let MobPart::Guardian(segment) = part.part {
            result[segment as usize] = *transform;
            found += 1;
        }
    }
    assert_eq!(found, 19);
    result
}
fn assert_root(app: &mut App) {
    let mut query = app.world_mut().query::<(&Mob, &Transform)>();
    let (mob, root) = query.single(app.world()).unwrap();
    assert_eq!(mob.entity_id, 9);
    assert_eq!(root.translation, Vec3::new(2.0, 4.0, 1.0));
    assert!(
        root.rotation
            .abs_diff_eq(Quat::from_rotation_y(0.2), 0.00001)
    );
    assert_eq!(root.scale, Vec3::ONE);
}

#[test]
fn authoritative_replacement_expiry_death_and_despawn_control_the_actual_rig() {
    let mut app = app();
    let mut state = timeline();
    state.moves[0].kind = EncounterMoveKind::PrisonerClaws;
    state.moves[0].combo = Some((2, 2));
    state.moves[0].phase_started_tick = 100;
    state.moves[0].phase_ticks = 61;
    app.world_mut()
        .resource_mut::<EncounterTimelineInbox>()
        .push(state.clone());
    deliver(&mut app, 140, MobAction::Windup); // First sight is already the second scratch.
    app.update();
    assert_root(&mut app);
    let late = pose(&mut app);
    assert!(
        late[Segment::LowerFrontRight as usize]
            .transform_point(rest_foot(1))
            .y
            > late[Segment::LowerFrontLeft as usize]
                .transform_point(rest_foot(0))
                .y
                + 0.1
    );
    for _ in 0..120 {
        app.update();
    }
    assert_eq!(late, pose(&mut app), "render time advanced an attack");
    deliver(&mut app, 170, MobAction::Windup); // No next phase announced.
    app.update();
    let expired = pose(&mut app);
    assert_ne!(expired, late);
    state.moves.clear();
    app.world_mut()
        .resource_mut::<EncounterTimelineInbox>()
        .push(state.clone());
    app.update();
    assert_eq!(
        expired,
        pose(&mut app),
        "empty timeline retained the scratch"
    );
    let mut replaced = timeline();
    replaced.moves[0].kind = EncounterMoveKind::BonebreakerJaws;
    replaced.moves[0].move_instance_id = 99;
    replaced.moves[0].phase_started_tick = 170;
    replaced.moves[0].phase_ticks = 61;
    app.world_mut()
        .resource_mut::<EncounterTimelineInbox>()
        .push(replaced);
    deliver(&mut app, 215, MobAction::Windup);
    app.update();
    assert_ne!(expired, pose(&mut app));
    assert_root(&mut app);
    // Death interrupts the mesh pose even while the last prep was still current.
    deliver(&mut app, 216, MobAction::Corpse);
    app.update();
    for _ in 0..60 {
        app.update();
    }
    let corpse = pose(&mut app);
    assert_root(&mut app);
    for _ in 0..30 {
        app.update();
    }
    assert_eq!(corpse, pose(&mut app));
    let mut query = app.world_mut().query::<&Mob>();
    let motion = query
        .single(app.world())
        .unwrap()
        .guardian_motion
        .as_ref()
        .unwrap();
    assert_eq!(
        motion.stage, 2,
        "corpse fastening forgot the announced stage"
    );
    let mut snap = snapshot(217);
    snap.mobs.clear();
    app.world_mut()
        .resource_mut::<SnapshotInbox>()
        .push(snap, Instant::now());
    app.update();
    assert_eq!(app.world_mut().query::<&Mob>().iter(app.world()).count(), 0);
    assert_eq!(
        app.world_mut()
            .query::<&MobVisual>()
            .iter(app.world())
            .count(),
        0
    );
}
