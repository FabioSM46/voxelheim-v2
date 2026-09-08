//! Optional art inspection using the production consumer, poses, markers and UI.
use super::*;
use crate::net::{EncounterMoveKind, HazardShape, SessionParams};

#[test]
#[ignore = "requires a render adapter; writes encounter PNGs in the temporary directory"]
fn capture_encounter_presentation() {
    use crate::player::{InputMode, WorldCamera, mobs};
    use bevy::asset::RenderAssetUsages;
    use bevy::camera::RenderTarget;
    use bevy::core_pipeline::tonemapping::Tonemapping;
    use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
    use bevy::render::view::screenshot::{Screenshot, save_to_disk};
    use bevy::window::ExitCondition;
    use std::time::{Duration, Instant};

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
    .add_systems(Startup, mobs::create_visuals)
    .add_systems(
        Update,
        (mobs::apply_snapshots, ApplyDeferred, mobs::animate)
            .chain()
            .in_set(ApplySnapshots),
    )
    .add_systems(Update, mobs::pose_encounters.after(reconcile))
    .add_plugins(crate::ui::encounters::EncounterUiPlugin);
    register(&mut app);
    while app.plugins_state() != bevy::app::PluginsState::Ready {
        std::thread::sleep(Duration::from_millis(10));
    }
    app.finish();
    app.cleanup();
    let mut target_image = Image::new_uninit(
        Extent3d {
            width: 1400,
            height: 900,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    target_image.texture_descriptor.usage |=
        TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC;
    let target = app
        .world_mut()
        .resource_mut::<Assets<Image>>()
        .add(target_image);
    app.world_mut().spawn((
        WorldCamera,
        Camera3d::default(),
        IsDefaultUiCamera,
        Camera {
            clear_color: Color::srgb(0.025, 0.03, 0.045).into(),
            ..default()
        },
        AmbientLight {
            brightness: 700.0,
            ..default()
        },
        Tonemapping::AcesFitted,
        RenderTarget::Image(target.clone().into()),
        Transform::from_xyz(0.0, 14.0, 22.0).looking_at(Vec3::new(0.0, 0.0, -1.0), Vec3::Y),
    ));
    let floor_mesh = app
        .world_mut()
        .resource_mut::<Assets<Mesh>>()
        .add(Cuboid::new(36.0, 0.2, 28.0));
    let floor_material = app
        .world_mut()
        .resource_mut::<Assets<StandardMaterial>>()
        .add(StandardMaterial {
            base_color: Color::srgb(0.18, 0.20, 0.23),
            ..default()
        });
    app.world_mut().spawn((
        Mesh3d(floor_mesh),
        MeshMaterial3d(floor_material),
        Transform::from_xyz(0.0, -0.1, -2.0),
    ));

    for (index, phase) in [
        MovePhase::Telegraph,
        MovePhase::Release,
        MovePhase::Channel,
        MovePhase::Recovery,
        MovePhase::Recovery,
    ]
    .into_iter()
    .enumerate()
    {
        let tick = 110 + index as u32 * 30;
        let mut snap = tests::snapshot(tick);
        let mut guardian = tests::timeline();
        guardian.moves[0].kind = EncounterMoveKind::BiteAndTear;
        guardian.moves[0].combo = Some((2, 2));
        guardian.moves[0].phase = if phase == MovePhase::Channel {
            MovePhase::Telegraph
        } else {
            phase
        };
        guardian.moves[0].phase_started_tick = tick - 10;
        guardian.moves[0].hazards[0] = HazardVolume {
            shape: HazardShape::Cone { half_angle: 0.7 },
            origin: [-5.0, 0.9, 0.0],
            direction: [0.0, 0.0, -1.0],
            radius: 4.0,
            height: 2.2,
        };
        snap.mobs[0].pos = [-5.0, 0.0, 0.0];
        let mut king = guardian.clone();
        king.encounter_id = 6;
        king.boss_entity_id = 10;
        king.boss = MobKind::DraugrKing;
        king.moves[0].kind = if phase == MovePhase::Release {
            EncounterMoveKind::SepulchreSpear
        } else {
            EncounterMoveKind::Burial
        };
        king.moves[0].combo = None;
        king.moves[0].phase = phase;
        king.moves[0].hazards[0] = HazardVolume {
            shape: HazardShape::Ring { inner_radius: 2.0 },
            origin: [5.0, 1.0, 0.0],
            direction: [0.0; 3],
            radius: 4.0,
            height: 2.0,
        };
        if phase == MovePhase::Release {
            king.moves[0].hazards[0] = HazardVolume {
                shape: HazardShape::Line { half_width: 0.9 },
                origin: [5.0, 1.4, 0.0],
                direction: [0.0, 0.0, -1.0],
                radius: 8.0,
                height: 2.6,
            };
        }
        if phase == MovePhase::Channel {
            king.moves[0].pulse = Some((1, 3));
            king.moves[0].interruptible = true;
        }
        let mut king_mob = snap.mobs[0];
        king_mob.entity_id = 10;
        king_mob.kind = MobKind::DraugrKing;
        king_mob.pos = [5.0, 0.0, 0.0];
        snap.mobs.push(king_mob);
        if index == 4 {
            guardian.moves.clear();
            king.moves.clear();
        }
        app.world_mut()
            .resource_mut::<SnapshotBuffer>()
            .accept(snap, Instant::now());
        app.world_mut()
            .resource_mut::<EncounterTimelineInbox>()
            .push(guardian);
        app.world_mut()
            .resource_mut::<EncounterTimelineInbox>()
            .push(king);
        for _ in 0..90 {
            app.update();
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = std::env::temp_dir().join(format!("voxelheim-encounter-{index}.png"));
        app.world_mut()
            .spawn(Screenshot::image(target.clone()))
            .observe(save_to_disk(output.clone()));
        for _ in 0..40 {
            app.update();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(output.exists(), "capture missing");
    }
    let cache = app
        .get_sub_app(bevy::render::RenderApp)
        .unwrap()
        .world()
        .resource::<bevy::render::render_resource::PipelineCache>();
    assert!(cache.pipelines().next().is_some(), "no pipeline compiled");
    for pipeline in cache.pipelines() {
        if let bevy::render::render_resource::CachedPipelineState::Err(error) = &pipeline.state {
            panic!("render pipeline failed: {error}");
        }
    }
}
