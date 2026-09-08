//! Opt-in GPU review using the actual snapshot consumer and production animator.
use super::*;
use crate::net::{EncounterMoveKind, MovePhase};
use crate::player::encounters;

#[test]
#[ignore = "requires a render adapter; writes king review PNGs to the temporary directory"]
fn capture_king_choreography() {
    record_blade_reach();
    use crate::net::SessionParams;
    use bevy::camera::RenderTarget;
    use bevy::core_pipeline::tonemapping::Tonemapping;
    use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
    use bevy::render::view::screenshot::{Screenshot, save_to_disk};
    use bevy::time::TimeUpdateStrategy;
    use bevy::window::ExitCondition;
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .build()
            .disable::<bevy::winit::WinitPlugin>()
            .disable::<bevy::render::pipelined_rendering::PipelinedRenderingPlugin>()
            .set(WindowPlugin {
                primary_window: None,
                exit_condition: ExitCondition::DontExit,
                ..default()
            }),
    )
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
    }))
    .insert_resource(InputMode::Playing)
    .init_resource::<SnapshotBuffer>()
    .add_systems(Startup, create_visuals)
    .add_systems(
        Update,
        (apply_snapshots, ApplyDeferred, animate)
            .chain()
            .in_set(crate::player::ApplySnapshots),
    )
    .add_systems(Update, pose_encounters.after(encounters::reconcile))
    .add_plugins(crate::ui::encounters::EncounterUiPlugin);
    encounters::register(&mut app);
    while app.plugins_state() != bevy::app::PluginsState::Ready {
        std::thread::sleep(Duration::from_millis(10));
    }
    app.finish();
    app.cleanup();
    app.world_mut()
        .resource_mut::<Time<Virtual>>()
        .set_max_delta(Duration::from_secs(5));
    let mut image = Image::new_uninit(
        Extent3d {
            width: 1280,
            height: 720,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_descriptor.usage |= TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC;
    let target = app.world_mut().resource_mut::<Assets<Image>>().add(image);
    let camera = app
        .world_mut()
        .spawn((
            crate::player::WorldCamera,
            IsDefaultUiCamera,
            Camera3d::default(),
            Camera {
                clear_color: Color::srgb(0.06, 0.075, 0.10).into(),
                ..default()
            },
            AmbientLight {
                brightness: 700.0,
                ..default()
            },
            Tonemapping::AcesFitted,
            Projection::Perspective(PerspectiveProjection {
                fov: crate::settings::DEFAULT_FIELD_OF_VIEW.to_radians(),
                ..default()
            }),
            RenderTarget::Image(target.clone().into()),
            Transform::from_xyz(3.0, 2.5, -4.0).looking_at(Vec3::new(0.0, 0.9, 0.0), Vec3::Y),
        ))
        .id();
    app.world_mut().spawn((
        DirectionalLight {
            illuminance: 6000.0,
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::from_xyz(-3.0, 6.0, -5.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    let floor = app
        .world_mut()
        .resource_mut::<Assets<Mesh>>()
        .add(Cuboid::new(80.0, 0.1, 80.0));
    let stone = app
        .world_mut()
        .resource_mut::<Assets<StandardMaterial>>()
        .add(StandardMaterial::from_color(Color::srgb(0.24, 0.26, 0.29)));
    app.world_mut().spawn((
        Mesh3d(floor),
        MeshMaterial3d(stone.clone()),
        Transform::from_xyz(0.0, -0.05, 0.0),
    ));
    let marker = app
        .world_mut()
        .resource_mut::<Assets<Mesh>>()
        .add(Cuboid::new(0.6, 1.8, 0.6));
    let scale_reference = app
        .world_mut()
        .spawn((
            Mesh3d(marker),
            MeshMaterial3d(stone.clone()),
            Transform::from_xyz(-2.2, 0.9, 0.6),
        ))
        .id();
    let boundary = app
        .world_mut()
        .resource_mut::<Assets<Mesh>>()
        .add(Cuboid::new(0.6, 1.8, 0.6));
    let reference = app
        .world_mut()
        .spawn((
            Mesh3d(boundary),
            MeshMaterial3d(stone),
            Transform::from_xyz(0.0, 0.9, -3.0),
        ))
        .id();
    let _review_fill = app
        .world_mut()
        .spawn((
            DirectionalLight {
                illuminance: 6000.0,
                ..default()
            },
            Transform::from_xyz(-5.0, 2.0, 0.0).looking_at(Vec3::Y, Vec3::Y),
            Visibility::Visible,
        ))
        .id();
    let tick = std::cell::Cell::new(100);
    let deliver = |app: &mut App,
                   kind,
                   combo,
                   phase,
                   percent: u32,
                   position: Vec3,
                   action: MobAction,
                   active: bool| {
        let next = tick.get() + 100;
        tick.set(next);
        let mut snapshot = encounters::tests::snapshot(next);
        snapshot.mobs[0].kind = MobKind::DraugrKing;
        snapshot.mobs[0].pos = position.to_array();
        snapshot.mobs[0].action = action;
        snapshot.mobs[0].target_entity_id = if action == MobAction::Corpse { 0 } else { 7 };
        let mut timeline = choreography_tests::fixture(kind, combo, phase, next);
        timeline.moves[0].move_instance_id = u64::from(next);
        timeline.moves[0].phase_started_tick =
            next - percent * (timeline.moves[0].phase_ticks - 1) / 100;
        app.world_mut()
            .resource_mut::<SnapshotBuffer>()
            .accept(snapshot, Instant::now() - Duration::from_millis(100));
        if active {
            app.world_mut()
                .resource_mut::<crate::net::EncounterTimelineInbox>()
                .push(timeline);
        } else {
            app.insert_resource(crate::net::EncounterTimelineInbox::default());
        }
    };
    let capture = |app: &mut App, name: &str| {
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::ZERO));
        let output = std::env::temp_dir().join(format!("king-1034-{name}.png"));
        if output.exists() {
            std::fs::remove_file(&output).unwrap();
        }
        for _ in 0..8 {
            app.update();
            std::thread::sleep(Duration::from_millis(5));
        }
        app.world_mut()
            .spawn(Screenshot::image(target.clone()))
            .observe(save_to_disk(output.clone()));
        for _ in 0..15 {
            app.update();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(output.exists(), "missing capture {name}");
    };
    // First idle appearance only: it may stand from the blade-supported crouch.
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::ZERO));
    deliver(
        &mut app,
        EncounterMoveKind::KingsSentence,
        None,
        MovePhase::Telegraph,
        0,
        Vec3::ZERO,
        MobAction::Idle,
        false,
    );
    for _ in 0..60 {
        app.update();
        std::thread::sleep(Duration::from_millis(5));
    }
    *app.world_mut().get_mut::<Transform>(camera).unwrap() =
        Transform::from_xyz(3.5, 2.4, -5.5).looking_at(Vec3::new(0.0, 1.55, 0.0), Vec3::Y);
    for (name, duration) in [
        ("entrance-start", Duration::ZERO),
        ("entrance-mid", Duration::from_millis(650)),
        ("entrance-end", Duration::from_millis(750)),
    ] {
        app.insert_resource(TimeUpdateStrategy::ManualDuration(duration));
        app.update();
        capture(&mut app, name);
    }
    for (name, kind, combo) in [
        ("sentence", EncounterMoveKind::KingsSentence, None),
        ("toll-left", EncounterMoveKind::ThreeTolls, Some((1, 3))),
        ("toll-right", EncounterMoveKind::ThreeTolls, Some((2, 3))),
        ("toll-thrust", EncounterMoveKind::ThreeTolls, Some((3, 3))),
        ("burial", EncounterMoveKind::Burial, None),
        ("edict", EncounterMoveKind::EdictOfTheGraves, None),
        ("spear", EncounterMoveKind::SepulchreSpear, None),
        ("requiem", EncounterMoveKind::RequiemOfTheBuried, None),
    ] {
        let radius = choreography_tests::fixture(kind, combo, MovePhase::Telegraph, 0).moves[0]
            .hazards[0]
            .radius;
        *app.world_mut().get_mut::<Transform>(reference).unwrap() =
            Transform::from_xyz(0.0, 0.9, -(radius - 0.3));
        for (phase_name, phase) in [
            ("prep", MovePhase::Telegraph),
            ("release", MovePhase::Release),
            ("recovery", MovePhase::Recovery),
            ("pulse", MovePhase::Channel),
        ] {
            let ritual = matches!(
                kind,
                EncounterMoveKind::Burial
                    | EncounterMoveKind::EdictOfTheGraves
                    | EncounterMoveKind::RequiemOfTheBuried
            );
            if phase == MovePhase::Channel && !ritual {
                continue;
            }
            for percent in [0, 50, 100] {
                deliver(
                    &mut app,
                    kind,
                    combo,
                    phase,
                    percent,
                    Vec3::ZERO,
                    if phase == MovePhase::Recovery {
                        MobAction::Recovery
                    } else {
                        MobAction::Windup
                    },
                    true,
                );
                *app.world_mut().get_mut::<Transform>(camera).unwrap() =
                    Transform::from_xyz(3.5, 2.4, -5.5)
                        .looking_at(Vec3::new(0.0, 2.05, -0.2), Vec3::Y);
                capture(&mut app, &format!("{name}-{phase_name}-{percent}"));
                if percent == 100 && phase == MovePhase::Telegraph {
                    for (view, from) in [
                        ("front", Vec3::new(0.0, 1.7, -5.5)),
                        ("side", Vec3::new(5.5, 1.7, 0.0)),
                        ("rear", Vec3::new(0.0, 2.0, 5.5)),
                        ("13", Vec3::new(0.0, 1.7, -13.0)),
                        ("25", Vec3::new(0.0, 1.7, -25.0)),
                    ] {
                        app.world_mut()
                            .entity_mut(reference)
                            .insert(Visibility::Hidden);
                        *app.world_mut().get_mut::<Transform>(camera).unwrap() =
                            Transform::from_translation(from)
                                .looking_at(Vec3::new(0.0, 2.05, 0.0), Vec3::Y);
                        capture(&mut app, &format!("{name}-{view}"));
                        app.world_mut()
                            .entity_mut(reference)
                            .insert(Visibility::Visible);
                    }
                }
                if phase == MovePhase::Release && percent == 50 {
                    *app.world_mut().get_mut::<Transform>(camera).unwrap() =
                        Transform::from_xyz(6.5, 5.0, -1.0)
                            .looking_at(Vec3::new(0.0, 0.6, -2.0), Vec3::Y);
                    capture(&mut app, &format!("{name}-reach-overlay"));
                }
            }
        }
    }
    // A newly visible boss already in the third blow must not play its entrance
    // or reconstruct either preceding cut. Remove the old body through snapshots.
    let mut absent = encounters::tests::snapshot(tick.get() + 1);
    absent.mobs.clear();
    app.world_mut()
        .resource_mut::<SnapshotBuffer>()
        .accept(absent, Instant::now());
    app.update();
    deliver(
        &mut app,
        EncounterMoveKind::ThreeTolls,
        Some((3, 3)),
        MovePhase::Release,
        100,
        Vec3::ZERO,
        MobAction::Windup,
        true,
    );
    *app.world_mut().get_mut::<Transform>(camera).unwrap() =
        Transform::from_xyz(3.5, 2.4, -5.5).looking_at(Vec3::new(0.0, 2.05, 0.0), Vec3::Y);
    capture(&mut app, "late-third-toll");
    deliver(
        &mut app,
        EncounterMoveKind::RequiemOfTheBuried,
        None,
        MovePhase::Channel,
        100,
        Vec3::ZERO,
        MobAction::Windup,
        true,
    );
    capture(&mut app, "replacement-requiem");
    deliver(
        &mut app,
        EncounterMoveKind::KingsSentence,
        None,
        MovePhase::Telegraph,
        90,
        Vec3::ZERO,
        MobAction::Windup,
        true,
    );
    let mut turned = encounters::tests::snapshot(tick.get() + 1);
    turned.mobs[0].kind = MobKind::DraugrKing;
    turned.mobs[0].yaw = 0.45;
    app.world_mut()
        .resource_mut::<SnapshotBuffer>()
        .accept(turned, Instant::now() - Duration::from_millis(100));
    capture(&mut app, "replacement-sentence-turned");
    deliver(
        &mut app,
        EncounterMoveKind::KingsSentence,
        None,
        MovePhase::Telegraph,
        100,
        Vec3::ZERO,
        MobAction::Idle,
        false,
    );
    capture(&mut app, "cancelled-attack");
    app.world_mut()
        .entity_mut(reference)
        .insert(Visibility::Hidden);
    app.world_mut()
        .entity_mut(scale_reference)
        .insert(Visibility::Hidden);
    for frame in 0..60 {
        let position = Vec3::new(0.0, 0.0, -frame as f32 * 0.025);
        deliver(
            &mut app,
            EncounterMoveKind::KingsSentence,
            None,
            MovePhase::Telegraph,
            0,
            position,
            MobAction::Chase,
            false,
        );
        // Drive real displacement and turns through snapshot reconciliation.
        if frame >= 30 {
            let mut snapshot = encounters::tests::snapshot(tick.get() + 1);
            snapshot.mobs[0].kind = MobKind::DraugrKing;
            snapshot.mobs[0].pos = position.to_array();
            snapshot.mobs[0].yaw = (frame - 30) as f32 * 0.02;
            snapshot.mobs[0].action = MobAction::Chase;
            app.world_mut()
                .resource_mut::<SnapshotBuffer>()
                .accept(snapshot, Instant::now() - Duration::from_millis(100));
        }
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(
            1.0 / 60.0,
        )));
        app.update();
        if [10, 20, 40, 50].contains(&frame) {
            *app.world_mut().get_mut::<Transform>(camera).unwrap() =
                Transform::from_translation(position + Vec3::new(3.5, 2.3, -5.0))
                    .looking_at(position + Vec3::Y * 1.4, Vec3::Y);
            capture(&mut app, &format!("gait-{frame}"));
        }
    }
    deliver(
        &mut app,
        EncounterMoveKind::KingsSentence,
        None,
        MovePhase::Recovery,
        0,
        Vec3::ZERO,
        MobAction::Corpse,
        false,
    );
    *app.world_mut().get_mut::<Transform>(camera).unwrap() =
        Transform::from_xyz(3.5, 2.6, -5.5).looking_at(Vec3::new(0.0, 0.8, 0.0), Vec3::Y);
    for (name, duration) in [
        ("death-start", Duration::ZERO),
        ("death-mid", Duration::from_millis(250)),
        ("death-end", Duration::from_secs(1)),
    ] {
        app.insert_resource(TimeUpdateStrategy::ManualDuration(duration));
        app.update();
        capture(&mut app, name);
    }
}

// Record the actual immutable blade mesh at every authoritative release tick.
// This is a measurement artifact, not an assertion that the server lane is fair.
fn record_blade_reach() {
    use std::fmt::Write;
    let mut csv = String::from("move,step,tick,forward_max,radial_max,tip_x,tip_y,tip_z\n");
    for (kind, combo) in choreography_tests::MOVES.into_iter().take(4) {
        let count =
            choreography_tests::fixture(kind, combo, MovePhase::Release, 100).moves[0].phase_ticks;
        let motion = motion::Motion::new(Vec3::ZERO, 0.0, MobAction::Windup);
        for tick in 0..count {
            let one = choreography_tests::presented(
                kind,
                combo,
                MovePhase::Release,
                tick as f32 / (count - 1) as f32,
            );
            let matrix = choreography::sample(&motion, Some(&one), 0.0)[Segment::Blade as usize];
            let vertices = choreography_tests::vertices(Segment::Blade, matrix);
            let direction = Vec3::from_array(one.announced.hazards[0].direction);
            let forward = vertices
                .iter()
                .map(|point| point.dot(direction))
                .fold(f32::NEG_INFINITY, f32::max);
            let radial = vertices
                .iter()
                .map(|point| point.xz().length())
                .fold(0.0, f32::max);
            let tip = matrix.transform_point(Vec3::new(0.42, 0.25, -0.115));
            writeln!(
                csv,
                "{kind:?},{},{tick},{forward:.4},{radial:.4},{:.4},{:.4},{:.4}",
                combo.map_or(0, |(step, _)| step),
                tip.x,
                tip.y,
                tip.z
            )
            .unwrap();
        }
    }
    std::fs::write(std::env::temp_dir().join("king-1034-blade-reach.csv"), csv).unwrap();
}
