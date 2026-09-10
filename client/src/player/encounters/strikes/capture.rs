//! Opt-in GPU review of strike reach through the production snapshot consumer, animator,
//! boundary cues, strike layer and encounter readings. Each sample is captured twice on the
//! same frame state: with the strike layer hidden, as the model alone presented the blow
//! before #1037, and shown.
use super::*;
use crate::net::{EncounterTimelineInbox, MobAction, Session, SessionParams};
use crate::player::encounters::tests;
use crate::player::{ApplySnapshots, InputMode, SnapshotBuffer, WorldCamera, mobs};
use std::time::{Duration, Instant};

/// The server catalogue at 20 Hz after #1037 part 2: release ticks, stage and region.
fn announced(kind: EncounterMoveKind, combo: Option<(u8, u8)>) -> (MobKind, u8, u32, HazardVolume) {
    use EncounterMoveKind::*;
    let bearing = match (kind, combo) {
        (PrisonerClaws | ThreeTolls, Some((1, _))) => -0.45_f32,
        (PrisonerClaws | ThreeTolls, Some((2, _))) => 0.45,
        _ => 0.0,
    };
    // The server turns the locked aim (0, 0, -1) by the blow's bearing.
    let direction = [bearing.sin(), 0.0, -bearing.cos()];
    let (boss, stage, ticks, shape, radius) = match (kind, combo) {
        (BiteAndTear, _) => (
            MobKind::VargrGuardian,
            1,
            4,
            HazardShape::Cone { half_angle: 0.70 },
            3.0,
        ),
        (PrisonerClaws, None) => (
            MobKind::VargrGuardian,
            1,
            6,
            HazardShape::Cone { half_angle: 1.05 },
            3.4,
        ),
        (PrisonerClaws, _) => (
            MobKind::VargrGuardian,
            2,
            6,
            HazardShape::Cone { half_angle: 1.05 },
            3.4,
        ),
        (BonebreakerJaws, _) => (
            MobKind::VargrGuardian,
            2,
            6,
            HazardShape::Cone { half_angle: 0.38 },
            3.6,
        ),
        (KingsSentence, _) => (
            MobKind::DraugrKing,
            1,
            5,
            HazardShape::Line { half_width: 1.1 },
            3.3,
        ),
        (ThreeTolls, Some((3, _))) => (
            MobKind::DraugrKing,
            1,
            4,
            HazardShape::Line { half_width: 0.65 },
            3.3,
        ),
        _ => (
            MobKind::DraugrKing,
            1,
            4,
            HazardShape::Cone { half_angle: 0.95 },
            3.3,
        ),
    };
    let body = super::super::super::mobs::body(boss);
    let height = if boss == MobKind::VargrGuardian {
        2.2
    } else {
        3.0
    };
    (
        boss,
        stage,
        ticks,
        HazardVolume {
            shape,
            origin: [0.0, body.height / 2.0, 0.0],
            direction,
            radius,
            height,
        },
    )
}

#[test]
#[ignore = "requires a render adapter; writes strike reach PNGs to the temporary directory"]
fn capture_strike_reach() {
    use EncounterMoveKind::*;
    use bevy::asset::RenderAssetUsages;
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
    .add_systems(Startup, (mobs::create_visuals, mobs::setup_regalia))
    .add_systems(
        Update,
        (mobs::apply_snapshots, ApplyDeferred, mobs::animate)
            .chain()
            .in_set(ApplySnapshots),
    )
    .add_systems(
        Update,
        (
            mobs::pose_encounters.after(reconcile),
            mobs::present_regalia.after(mobs::pose_encounters),
        ),
    )
    .add_plugins(crate::ui::encounters::EncounterUiPlugin);
    super::super::register(&mut app);
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
            WorldCamera,
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
            Transform::default(),
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
    // A player-sized scale reference standing just inside the announced far boundary.
    let marker = app
        .world_mut()
        .resource_mut::<Assets<Mesh>>()
        .add(Cuboid::new(0.6, 1.8, 0.6));
    let reference = app
        .world_mut()
        .spawn((Mesh3d(marker), MeshMaterial3d(stone), Transform::default()))
        .id();
    for _ in 0..90 {
        app.update();
        std::thread::sleep(Duration::from_millis(10));
    }

    let mut tick = 1000u32;
    let shot = |app: &mut App, name: &str, from: Vec3, at: Vec3, strikes: bool| {
        *app.world_mut().get_mut::<Transform>(camera).unwrap() =
            Transform::from_translation(from).looking_at(at, Vec3::Y);
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::ZERO));
        // Settle first, so the strike this move announces has been spawned, then choose
        // whether the frame shows it.
        for _ in 0..8 {
            app.update();
            std::thread::sleep(Duration::from_millis(5));
        }
        let world = app.world_mut();
        let effects: Vec<Entity> = world
            .query_filtered::<Entity, With<StrikeEffect>>()
            .iter(world)
            .collect();
        assert!(!effects.is_empty(), "{name}: no strike is being presented");
        for entity in effects {
            world.entity_mut(entity).insert(if strikes {
                Visibility::Visible
            } else {
                Visibility::Hidden
            });
        }
        let output = std::env::temp_dir().join(format!("strikes-1037-{name}.png"));
        let _ = std::fs::remove_file(&output);
        for _ in 0..2 {
            app.update();
            std::thread::sleep(Duration::from_millis(5));
        }
        app.world_mut()
            .spawn(Screenshot::image(target.clone()))
            .observe(save_to_disk(output.clone()));
        // Pipelines compile asynchronously: wait for the file, not a frame count.
        for _ in 0..400 {
            if output.exists() {
                break;
            }
            app.update();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(output.exists(), "missing capture {name}");
    };

    for (name, kind, combo) in [
        ("bite", BiteAndTear, Some((1, 2))),
        ("claw-single", PrisonerClaws, None),
        ("claw-left", PrisonerClaws, Some((1, 2))),
        ("jaws", BonebreakerJaws, None),
        ("sentence", KingsSentence, None),
        ("toll-left", ThreeTolls, Some((1, 3))),
        ("toll-right", ThreeTolls, Some((2, 3))),
        ("toll-thrust", ThreeTolls, Some((3, 3))),
    ] {
        let (boss, stage, ticks, volume) = announced(kind, combo);
        let direction = Vec3::from_array(volume.direction);
        *app.world_mut().get_mut::<Transform>(reference).unwrap() =
            Transform::from_translation(direction * (volume.radius - 0.3) + Vec3::Y * 0.9);
        for percent in [0u32, 50, 100] {
            tick += 100;
            let mut snapshot = tests::snapshot(tick);
            snapshot.mobs[0].kind = boss;
            snapshot.mobs[0].action = MobAction::Windup;
            let mut timeline = tests::timeline();
            (timeline.boss, timeline.phase) = (boss, stage);
            let one = &mut timeline.moves[0];
            one.move_instance_id = u64::from(tick);
            (one.kind, one.combo, one.phase, one.phase_ticks) =
                (kind, combo, MovePhase::Release, ticks);
            one.phase_started_tick = tick - percent * (ticks - 1) / 100;
            one.hazards = vec![volume];
            app.world_mut()
                .resource_mut::<SnapshotBuffer>()
                .accept(snapshot, Instant::now() - Duration::from_millis(100));
            app.world_mut()
                .resource_mut::<EncounterTimelineInbox>()
                .push(timeline);
            let side = direction.cross(Vec3::Y).normalize();
            let near = side * 4.2 - direction * 1.2 + Vec3::Y * 3.4;
            let focus = direction * volume.radius * 0.45;
            for strikes in [false, true] {
                let state = if strikes { "after" } else { "before" };
                shot(
                    &mut app,
                    &format!("{name}-{percent:03}-{state}"),
                    near,
                    focus,
                    strikes,
                );
            }
            if percent == 100 {
                for distance in [13.0, 25.0] {
                    let from = -direction.with_y(0.0) * 0.0 + side * distance * 0.5
                        - direction * distance * 0.866
                        + Vec3::Y * 1.7;
                    shot(
                        &mut app,
                        &format!("{name}-{distance:.0}-blocks"),
                        from,
                        Vec3::Y * 0.9,
                        true,
                    );
                }
            }
        }
    }
    let render = app.get_sub_app(bevy::render::RenderApp).unwrap();
    let cache = render
        .world()
        .resource::<bevy::render::render_resource::PipelineCache>();
    for pipeline in cache.pipelines() {
        if let bevy::render::render_resource::CachedPipelineState::Err(error) = &pipeline.state {
            panic!("render pipeline failed: {error}");
        }
    }
}
