//! Opt-in GPU review of the spider under the production snapshot consumer and animator, plus
//! its emergence from a wall driven straight through the rig.
use super::*;

#[test]
#[ignore = "requires a render adapter; writes spider review PNGs to the temporary directory"]
fn capture_spider_rig() {
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
        .add(StandardMaterial::from_color(Color::srgb(0.30, 0.31, 0.33)));
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
        mob.kind = MobKind::CaveSpider;
        mob.pos = position.to_array();
        mob.yaw = 0.0;
        mob.health = 12;
        mob.max_health = 12;
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
        let output = std::env::temp_dir().join(format!("spider-1296-{name}.png"));
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
    for (name, position) in [
        ("three-quarter", Vec3::new(1.6, 1.2, -1.8)),
        ("front", Vec3::new(0.0, 0.6, -2.2)),
        ("side", Vec3::new(2.2, 0.5, 0.0)),
        ("above", Vec3::new(0.01, 2.6, 0.0)),
        ("6-blocks", Vec3::new(1.0, 1.7, -6.0)),
    ] {
        *app.world_mut().get_mut::<Transform>(camera).unwrap() =
            Transform::from_translation(position).looking_at(Vec3::new(0.0, 0.3, 0.0), Vec3::Y);
        capture(&mut app, &format!("rest-{name}"));
    }
    *app.world_mut().get_mut::<Transform>(camera).unwrap() =
        Transform::from_xyz(2.2, 0.9, -2.0).looking_at(Vec3::new(0.0, 0.3, -1.2), Vec3::Y);
    for frame in 0..40 {
        deliver(
            &mut app,
            Vec3::new(0.0, 0.0, -frame as f32 * 0.07),
            MobAction::Chase,
        );
        app.update();
        if frame % 4 == 0 {
            capture(&mut app, &format!("run-{frame:03}"));
        }
    }
    let stop = Vec3::new(0.0, 0.0, -2.8);
    *app.world_mut().get_mut::<Transform>(camera).unwrap() =
        Transform::from_translation(stop + Vec3::new(1.5, 0.9, -1.4))
            .looking_at(stop + Vec3::Y * 0.3, Vec3::Y);
    for (action, name) in [
        (MobAction::Windup, "windup"),
        (MobAction::Recovery, "lunge"),
    ] {
        deliver(&mut app, stop, action);
        for _ in 0..30 {
            app.update();
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

    // Emergence, through the rig directly: a spider first seen against a wall to its east.
    deliver(&mut app, Vec3::new(40.0, 0.0, 0.0), MobAction::Idle);
    app.update();
    let wall = app
        .world_mut()
        .resource_mut::<Assets<Mesh>>()
        .add(Cuboid::new(1.0, 1.5, 3.0));
    app.world_mut().spawn((
        Mesh3d(wall),
        MeshMaterial3d(stone),
        Transform::from_xyz(1.5, 0.75, 0.5),
    ));
    let chitin = app.world().resource::<MobVisuals>().cave_spider.clone();
    let parts: Vec<Entity> = chitin
        .spider_parts
        .clone()
        .unwrap()
        .into_iter()
        .map(|(segment, mesh)| {
            let material = if segment == Segment::Eyes {
                chitin.eyes.as_ref().unwrap().material.clone()
            } else {
                chitin.body_material.clone()
            };
            app.world_mut()
                .spawn((Mesh3d(mesh), MeshMaterial3d(material), Transform::IDENTITY))
                .id()
        })
        .collect();
    *app.world_mut().get_mut::<Transform>(camera).unwrap() =
        Transform::from_xyz(-1.2, 1.3, -1.8).looking_at(Vec3::new(0.7, 0.3, 0.5), Vec3::Y);
    let at = Vec3::new(0.5, 0.0, 0.5);
    let yaw = FRAC_PI_2;
    let wall_east = |voxel: IVec3| voxel.x >= 1;
    let mut motion = Motion::new(at, 11, true);
    for frame in 0..48 {
        motion.sample(
            at,
            yaw,
            MobAction::Idle,
            0.0,
            Duration::from_secs_f32(1.0 / 60.0),
            wall_east,
        );
        let root = Transform::from_translation(at).with_rotation(Quat::from_rotation_y(yaw));
        for (entity, local) in parts.iter().zip(motion.transforms) {
            *app.world_mut().get_mut::<Transform>(*entity).unwrap() = root * local;
        }
        if frame % 8 == 0 {
            capture(&mut app, &format!("emerge-{frame:03}"));
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
