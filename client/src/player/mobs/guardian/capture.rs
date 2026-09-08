//! Opt-in GPU review using the actual snapshot consumer and production animator.
use super::*;

#[test]
#[ignore = "requires a render adapter; writes guardian review PNGs to the temporary directory"]
fn capture_guardian_articulation() {
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
        .add(Cuboid::new(20.0, 0.1, 20.0));
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
    app.world_mut().spawn((
        Mesh3d(marker),
        MeshMaterial3d(stone.clone()),
        Transform::from_xyz(-1.6, 0.9, 0.0),
    ));
    let wall = app
        .world_mut()
        .resource_mut::<Assets<Mesh>>()
        .add(Cuboid::new(5.0, 2.0, 0.2));
    let wall_entity = app
        .world_mut()
        .spawn((
            Mesh3d(wall),
            MeshMaterial3d(stone),
            Transform::from_xyz(0.0, 1.0, 1.3),
        ))
        .id();
    let mut tick = 1;
    let mut deliver = |app: &mut App, position: [f32; 3], yaw: f32, action: MobAction| {
        let mut snapshot = crate::player::encounters::tests::snapshot(tick);
        tick += 1;
        snapshot.mobs[0].pos = position;
        snapshot.mobs[0].yaw = yaw;
        snapshot.mobs[0].action = action;
        snapshot.mobs[0].target_entity_id = if action == MobAction::Corpse { 0 } else { 7 };
        app.world_mut()
            .resource_mut::<SnapshotBuffer>()
            .accept(snapshot, Instant::now() - Duration::from_millis(100));
    };
    deliver(&mut app, [0.0; 3], 0.0, MobAction::Idle);
    for _ in 0..100 {
        app.update();
        std::thread::sleep(Duration::from_millis(5));
    }
    let capture = |app: &mut App, name: &str| {
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::ZERO));
        let output = std::env::temp_dir().join(format!("guardian-1028-{name}.png"));
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
    capture(&mut app, "rest");
    for (name, position) in [
        ("front", Vec3::new(0.0, 1.4, -4.0)),
        ("side", Vec3::new(4.0, 1.4, 0.0)),
        ("rear", Vec3::new(0.0, 1.8, 4.0)),
        ("13-blocks", Vec3::new(0.0, 1.6, -13.0)),
        ("25-blocks", Vec3::new(0.0, 1.6, -25.0)),
    ] {
        app.world_mut()
            .entity_mut(wall_entity)
            .insert(if name == "rear" {
                Visibility::Hidden
            } else {
                Visibility::Visible
            });
        *app.world_mut().get_mut::<Transform>(camera).unwrap() =
            Transform::from_translation(position).looking_at(Vec3::new(0.0, 0.9, 0.0), Vec3::Y);
        capture(&mut app, name);
    }
    *app.world_mut().get_mut::<Transform>(camera).unwrap() =
        Transform::from_xyz(3.0, 2.0, -4.5).looking_at(Vec3::new(0.0, 0.8, -0.8), Vec3::Y);
    for frame in 0..120 {
        deliver(
            &mut app,
            [0.0, 0.0, -frame as f32 * 0.012],
            if frame < 60 {
                0.0
            } else {
                (frame - 60) as f32 * 0.008
            },
            MobAction::Chase,
        );
        app.update();
        if frame % 10 == 0 {
            capture(&mut app, &format!("walk-{frame:03}"));
        }
    }
    deliver(&mut app, [0.0, 0.0, -1.428], 0.472, MobAction::Corpse);
    for frame in 0..50 {
        app.update();
        if frame % 5 == 0 {
            capture(&mut app, &format!("death-{frame:03}"));
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
