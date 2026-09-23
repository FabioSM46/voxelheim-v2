//! Opt-in GPU review of the scorpion under the production snapshot consumer and animator:
//! its rest pose from every side, the walk, both telegraphs and their strikes, and the death —
//! then, driven straight through the rig, a buried scorpion's mound and its emergence.
use super::*;

#[test]
#[ignore = "requires a render adapter; writes scorpion review PNGs to the temporary directory"]
fn capture_scorpion_rig() {
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
        tick_rate: 60,
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
    .add_systems(Update, (apply_snapshots, ApplyDeferred, animate).chain());
    while app.plugins_state() != bevy::app::PluginsState::Ready {
        std::thread::sleep(Duration::from_millis(10));
    }
    app.finish();
    app.cleanup();
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
            Camera3d::default(),
            Camera {
                clear_color: Color::srgb(0.06, 0.075, 0.10).into(),
                ..default()
            },
            AmbientLight {
                brightness: 900.0,
                ..default()
            },
            Tonemapping::AcesFitted,
            Projection::Perspective(PerspectiveProjection {
                fov: crate::settings::DEFAULT_FIELD_OF_VIEW.to_radians(),
                ..default()
            }),
            RenderTarget::Image(target.clone().into()),
            Transform::from_xyz(1.6, 1.2, -1.8).looking_at(Vec3::new(0.0, 0.3, 0.0), Vec3::Y),
        ))
        .id();
    app.world_mut().spawn((
        DirectionalLight {
            illuminance: 7000.0,
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::from_xyz(-3.0, 6.0, -5.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    let floor = app
        .world_mut()
        .resource_mut::<Assets<Mesh>>()
        .add(Cuboid::new(20.0, 0.1, 20.0));
    let stone = app
        .world_mut()
        .resource_mut::<Assets<StandardMaterial>>()
        .add(StandardMaterial::from_color(Color::srgb(0.78, 0.68, 0.48)));
    app.world_mut().spawn((
        Mesh3d(floor),
        MeshMaterial3d(stone.clone()),
        Transform::from_xyz(0.0, -0.05, 0.0),
    ));
    let mut tick = 1;
    let mut deliver = |app: &mut App, position: Vec3, action: MobAction| {
        let mut snapshot = crate::player::encounters::tests::snapshot(tick);
        tick += 1;
        let mob = &mut snapshot.mobs[0];
        mob.kind = MobKind::Scorpion;
        mob.pos = position.to_array();
        mob.yaw = 0.0;
        mob.health = 72;
        mob.max_health = 72;
        mob.action = action;
        mob.target_entity_id = if action == MobAction::Corpse { 0 } else { 7 };
        app.world_mut()
            .resource_mut::<SnapshotBuffer>()
            .accept(snapshot, Instant::now() - Duration::from_millis(100));
    };
    deliver(&mut app, Vec3::ZERO, MobAction::Idle);
    for _ in 0..60 {
        app.update();
        std::thread::sleep(Duration::from_millis(5));
    }
    let capture = |app: &mut App, name: &str| {
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::ZERO));
        let output = std::env::temp_dir().join(format!("scorpion-1297-{name}.png"));
        // Remove an earlier run's artifact so existence cannot certify a failed capture.
        if output.exists() {
            std::fs::remove_file(&output).unwrap();
        }
        app.world_mut()
            .spawn(Screenshot::image(target.clone()))
            .observe(save_to_disk(output.clone()));
        for _ in 0..15 {
            app.update();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(output.exists(), "missing capture {name}");
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(
            1.0 / 60.0,
        )));
    };
    let aim = |app: &mut App, eye: Vec3, at: Vec3| {
        *app.world_mut().get_mut::<Transform>(camera).unwrap() =
            Transform::from_translation(eye).looking_at(at, Vec3::Y);
    };
    for (name, position) in [
        ("three-quarter", Vec3::new(1.7, 1.1, -1.9)),
        ("front", Vec3::new(0.0, 0.7, -2.4)),
        ("side", Vec3::new(2.4, 0.5, 0.0)),
        ("above", Vec3::new(0.01, 2.8, 0.0)),
        ("6-blocks", Vec3::new(1.0, 1.7, -6.0)),
    ] {
        aim(&mut app, position, Vec3::new(0.0, 0.25, 0.0));
        capture(&mut app, &format!("rest-{name}"));
    }
    aim(
        &mut app,
        Vec3::new(2.3, 0.9, -1.6),
        Vec3::new(0.0, 0.25, -0.8),
    );
    for frame in 0..48 {
        deliver(
            &mut app,
            Vec3::new(0.0, 0.0, -frame as f32 * 2.6 / 60.0),
            MobAction::Chase,
        );
        app.update();
        if frame % 6 == 0 {
            capture(&mut app, &format!("walk-{frame:03}"));
        }
    }
    let stop = Vec3::new(0.0, 0.0, -2.08);
    aim(
        &mut app,
        stop + Vec3::new(2.2, 0.8, -0.3),
        stop + Vec3::Y * 0.3,
    );
    // The rhythm the server keeps: the sting first, then the swipe, each a windup and the
    // recovery that follows it. Timed at the server's own lengths.
    for (action, seconds, name) in [
        (MobAction::Chase, 0.3, "stand"),
        (MobAction::Windup, 1.1, "sting-windup"),
        (MobAction::Recovery, 0.15, "sting-strike"),
        (MobAction::Recovery, 1.25, "sting-recovered"),
        (MobAction::Windup, 0.45, "swipe-windup"),
        (MobAction::Recovery, 0.12, "swipe-strike"),
        (MobAction::Recovery, 0.58, "swipe-recovered"),
    ] {
        deliver(&mut app, stop, action);
        let frames = (seconds * 60.0_f32).round() as usize;
        for frame in 0..frames {
            app.update();
            if action == MobAction::Windup && frame % 12 == 0 {
                capture(&mut app, &format!("{name}-{frame:03}"));
            }
        }
        capture(&mut app, name);
    }
    deliver(&mut app, stop, MobAction::Corpse);
    for frame in 0..50 {
        app.update();
        if frame % 10 == 0 {
            capture(&mut app, &format!("death-{frame:03}"));
        }
    }

    // Under the sand, through the rig directly: a sand floor whose surface is at y = 0, the
    // scorpion one block under it, a player walking in, and the emergence.
    let sand_floor = |voxel: IVec3| voxel.y < 0;
    let rig = app.world().resource::<MobVisuals>().scorpion.clone();
    let root = Vec3::new(40.0, -1.0, 0.0);
    let parts: Vec<Entity> = rig
        .scorpion_parts
        .clone()
        .unwrap()
        .into_iter()
        .map(|(_, mesh)| {
            app.world_mut()
                .spawn((
                    Mesh3d(mesh),
                    MeshMaterial3d(rig.body_material.clone()),
                    Transform::IDENTITY,
                    Visibility::Hidden,
                ))
                .id()
        })
        .collect();
    let floor = app
        .world_mut()
        .resource_mut::<Assets<Mesh>>()
        .add(Cuboid::new(20.0, 0.1, 20.0));
    let sand = app
        .world_mut()
        .resource_mut::<Assets<StandardMaterial>>()
        .add(StandardMaterial::from_color(Color::srgb(0.78, 0.68, 0.48)));
    app.world_mut().spawn((
        Mesh3d(floor),
        MeshMaterial3d(sand),
        Transform::from_xyz(40.0, -0.05, 0.0),
    ));
    aim(
        &mut app,
        Vec3::new(42.0, 1.1, -1.8),
        Vec3::new(40.0, 0.2, 0.0),
    );
    let mut motion = Motion::new(root, 12, MobAction::Idle);
    let show = |app: &mut App, motion: &Motion, at: Vec3| {
        let place = Transform::from_translation(at);
        for (entity, local) in parts.iter().zip(motion.transforms) {
            let shown = local != HIDDEN;
            let mut entity = app.world_mut().entity_mut(*entity);
            *entity.get_mut::<Visibility>().unwrap() = if shown {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            };
            if shown {
                *entity.get_mut::<Transform>().unwrap() = place * local;
            }
        }
    };
    let frame = Duration::from_secs_f32(1.0 / 60.0);
    for step in 0..150 {
        // A player walking in from ten blocks to five and a half.
        let player = root + Vec3::new(0.0, 1.0, -10.0 + step as f32 * 0.03);
        motion.sample(root, MobAction::Idle, 0.0, frame, sand_floor, Some(player));
        show(&mut app, &motion, root);
        if step % 30 == 0 {
            capture(&mut app, &format!("buried-{step:03}"));
        }
    }
    let risen = root + Vec3::Y;
    for step in 0..48 {
        let at = root.lerp(risen, ((step + 1) as f32 / 3.0).min(1.0));
        motion.sample(at, MobAction::Recovery, 0.0, frame, sand_floor, None);
        show(&mut app, &motion, at);
        if step % 6 == 0 {
            capture(&mut app, &format!("emerge-{step:03}"));
        }
    }

    let render = app.get_sub_app(bevy::render::RenderApp).unwrap();
    let cache = render
        .world()
        .resource::<bevy::render::render_resource::PipelineCache>();
    assert!(cache.pipelines().next().is_some());
    for pipeline in cache.pipelines() {
        if let bevy::render::render_resource::CachedPipelineState::Err(error) = &pipeline.state {
            panic!("render pipeline failed: {error}");
        }
    }
}
