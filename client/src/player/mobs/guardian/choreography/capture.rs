//! Opt-in GPU review using the actual snapshot consumer and production animator.
use super::*;
use crate::player::encounters;

#[test]
#[ignore = "requires a render adapter; writes guardian review PNGs to the temporary directory"]
fn capture_guardian_choreography() {
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
    let review_fill = app
        .world_mut()
        .spawn((
            DirectionalLight {
                illuminance: 6000.0,
                ..default()
            },
            Transform::from_xyz(-5.0, 2.0, 0.0).looking_at(Vec3::Y, Vec3::Y),
            Visibility::Hidden,
        ))
        .id();
    let tick = std::cell::Cell::new(100);
    let deliver =
        |app: &mut App, kind, combo, phase, elapsed, position: Vec3, clear: bool, corpse: bool| {
            let next = tick.get() + 100;
            tick.set(next);
            let tick = next;
            let mut snapshot = encounters::tests::snapshot(tick);
            snapshot.mobs[0].pos = position.to_array();
            snapshot.mobs[0].action = if corpse {
                MobAction::Corpse
            } else if clear {
                MobAction::Idle
            } else if phase == MovePhase::Recovery {
                MobAction::Recovery
            } else {
                MobAction::Windup
            };
            snapshot.mobs[0].target_entity_id = if corpse { 0 } else { 7 };
            let mut timeline = tests::fixture(kind, combo, phase, tick);
            timeline.moves[0].phase_started_tick =
                tick - elapsed * (timeline.moves[0].phase_ticks - 1) / 60;
            if kind == EncounterMoveKind::PredatorLeap {
                timeline.moves[0].hazards[0].origin[2] = -6.5;
            }
            if clear {
                timeline.moves.clear();
            }
            app.world_mut()
                .resource_mut::<SnapshotBuffer>()
                .accept(snapshot, Instant::now() - Duration::from_millis(100));
            app.world_mut()
                .resource_mut::<crate::net::EncounterTimelineInbox>()
                .push(timeline);
        };
    let capture = |app: &mut App, name: &str| {
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::ZERO));
        let output = std::env::temp_dir().join(format!("guardian-1028-choreography-{name}.png"));
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
    deliver(
        &mut app,
        EncounterMoveKind::BiteAndTear,
        Some((1, 2)),
        MovePhase::Telegraph,
        0,
        Vec3::ZERO,
        false,
        false,
    );
    for _ in 0..100 {
        app.update();
        std::thread::sleep(Duration::from_millis(5));
    }
    for (name, kind, combo) in [
        ("bite-1", EncounterMoveKind::BiteAndTear, Some((1, 2))),
        ("bite-2", EncounterMoveKind::BiteAndTear, Some((2, 2))),
        ("claw-single", EncounterMoveKind::PrisonerClaws, None),
        ("claw-left", EncounterMoveKind::PrisonerClaws, Some((1, 2))),
        ("claw-right", EncounterMoveKind::PrisonerClaws, Some((2, 2))),
        ("charge", EncounterMoveKind::CollarCharge, None),
        ("leap", EncounterMoveKind::PredatorLeap, None),
        ("jaws", EncounterMoveKind::BonebreakerJaws, None),
    ] {
        let radius =
            tests::fixture(kind, combo, MovePhase::Telegraph, 0).moves[0].hazards[0].radius;
        *app.world_mut().get_mut::<Transform>(reference).unwrap() =
            if kind == EncounterMoveKind::PredatorLeap {
                Transform::from_xyz(3.0, 0.9, -6.5)
            } else {
                Transform::from_xyz(radius * 0.3_f32.sin(), 0.9, -radius * 0.3_f32.cos())
            };
        for (phase_name, phase) in [
            ("prep", MovePhase::Telegraph),
            ("strike", MovePhase::Release),
            ("recovery", MovePhase::Recovery),
        ] {
            let samples: &[u32] =
                if kind == EncounterMoveKind::CollarCharge && phase == MovePhase::Telegraph {
                    &[0, 11, 30, 35, 60]
                } else {
                    &[0, 30, 60]
                };
            for &elapsed in samples {
                let position = if kind == EncounterMoveKind::PredatorLeap
                    && phase != MovePhase::Telegraph
                {
                    Vec3::new(
                        0.0,
                        0.0,
                        -6.5 * if phase == MovePhase::Release {
                            let ticks = tests::fixture(kind, combo, phase, 0).moves[0].phase_ticks;
                            let phase_tick = elapsed * (ticks - 1) / 60;
                            (((phase_tick + 1) as f32 * 12.0 / 60.0).min(6.5)) / 6.5
                        } else {
                            1.0
                        },
                    )
                } else {
                    Vec3::ZERO
                };
                deliver(
                    &mut app, kind, combo, phase, elapsed, position, false, false,
                );
                *app.world_mut().get_mut::<Transform>(camera).unwrap() =
                    Transform::from_translation(position + Vec3::new(3.0, 2.2, -4.5))
                        .looking_at(position + Vec3::new(0.0, 1.05, 0.0), Vec3::Y);
                capture(&mut app, &format!("{name}-{phase_name}-{elapsed:02}"));
                if (elapsed == 60 && phase == MovePhase::Telegraph)
                    || (elapsed == 30 && phase == MovePhase::Release)
                {
                    for (view, offset) in [
                        ("front", Vec3::new(0.0, 1.5, -4.5)),
                        ("side", Vec3::new(4.5, 1.6, 0.0)),
                        ("lit-side", Vec3::new(-4.5, 1.6, 0.0)),
                        ("13", Vec3::new(0.0, 1.6, -13.0)),
                        ("25", Vec3::new(0.0, 1.6, -25.0)),
                    ] {
                        for entity in [reference, scale_reference] {
                            app.world_mut()
                                .entity_mut(entity)
                                .insert(if view.contains("side") {
                                    Visibility::Hidden
                                } else {
                                    Visibility::Visible
                                });
                        }
                        app.world_mut()
                            .entity_mut(review_fill)
                            .insert(if view == "lit-side" {
                                Visibility::Visible
                            } else {
                                Visibility::Hidden
                            });
                        *app.world_mut().get_mut::<Transform>(camera).unwrap() =
                            Transform::from_translation(position + offset)
                                .looking_at(position + Vec3::Y * 1.05, Vec3::Y);
                        capture(
                            &mut app,
                            &if phase == MovePhase::Telegraph {
                                format!("{name}-{view}")
                            } else {
                                format!("{name}-strike-{view}")
                            },
                        );
                        for entity in [reference, scale_reference] {
                            app.world_mut()
                                .entity_mut(entity)
                                .insert(Visibility::Visible);
                        }
                        app.world_mut()
                            .entity_mut(review_fill)
                            .insert(Visibility::Hidden);
                    }
                }
            }
        }
    }
    *app.world_mut().get_mut::<Transform>(camera).unwrap() =
        Transform::from_xyz(3.0, 2.2, -4.5).looking_at(Vec3::Y * 0.9, Vec3::Y);
    // Consecutive snapshots at the actual charge speed exercise the production
    // world-space contact history. The lane stays fixed while the root crosses it.
    for entity in [reference, scale_reference] {
        app.world_mut()
            .entity_mut(entity)
            .insert(Visibility::Hidden);
    }
    let charge_start = tick.get() + 1;
    let charge_snapshot = |app: &mut App, server_tick, phase, position: Vec3| {
        tick.set(server_tick);
        let mut snapshot = encounters::tests::snapshot(server_tick);
        snapshot.mobs[0].pos = position.to_array();
        snapshot.mobs[0].action = if phase == MovePhase::Release {
            MobAction::Windup
        } else {
            MobAction::Recovery
        };
        let timeline = tests::fixture(
            EncounterMoveKind::CollarCharge,
            None,
            phase,
            if phase == MovePhase::Release {
                charge_start
            } else {
                server_tick
            },
        );
        app.world_mut()
            .resource_mut::<SnapshotBuffer>()
            .accept(snapshot, Instant::now() - Duration::from_millis(100));
        app.world_mut()
            .resource_mut::<crate::net::EncounterTimelineInbox>()
            .push(timeline);
    };
    for frame in 1..=54 {
        let position = Vec3::new(0.0, 0.0, -11.0 * frame as f32 / 60.0);
        charge_snapshot(
            &mut app,
            charge_start + frame - 1,
            MovePhase::Release,
            position,
        );
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(
            1.0 / 60.0,
        )));
        app.update();
        if frame == 1 || frame % 9 == 0 {
            *app.world_mut().get_mut::<Transform>(camera).unwrap() =
                Transform::from_translation(position + Vec3::new(4.5, 2.0, -1.0))
                    .looking_at(position + Vec3::Y * 0.9, Vec3::Y);
            capture(&mut app, &format!("charge-travel-{frame:02}"));
        }
    }
    charge_snapshot(
        &mut app,
        charge_start + 54,
        MovePhase::Recovery,
        Vec3::new(0.0, 0.0, -9.9),
    );
    capture(&mut app, "charge-stop");
    for entity in [reference, scale_reference] {
        app.world_mut()
            .entity_mut(entity)
            .insert(Visibility::Visible);
    }
    *app.world_mut().get_mut::<Transform>(camera).unwrap() =
        Transform::from_xyz(3.0, 2.2, -4.5).looking_at(Vec3::Y * 0.9, Vec3::Y);
    for entity in [reference, scale_reference] {
        app.world_mut()
            .entity_mut(entity)
            .insert(Visibility::Hidden);
    }
    for (name, kind) in [
        ("phase-one", EncounterMoveKind::CollarCharge),
        ("phase-two", EncounterMoveKind::BonebreakerJaws),
    ] {
        deliver(
            &mut app,
            kind,
            None,
            MovePhase::Telegraph,
            0,
            Vec3::ZERO,
            true,
            false,
        );
        for (view, offset) in [
            ("front", Vec3::new(1.8, 0.9, -3.0)),
            ("side", Vec3::new(-3.0, 1.2, 0.0)),
            ("rear", Vec3::new(-3.0, 1.3, 2.0)),
        ] {
            app.world_mut()
                .entity_mut(review_fill)
                .insert(Visibility::Visible);
            *app.world_mut().get_mut::<Transform>(camera).unwrap() =
                Transform::from_translation(offset).looking_at(Vec3::Y * 0.9, Vec3::Y);
            capture(&mut app, &format!("{name}-{view}"));
        }
    }
    app.world_mut()
        .entity_mut(review_fill)
        .insert(Visibility::Hidden);
    for entity in [reference, scale_reference] {
        app.world_mut()
            .entity_mut(entity)
            .insert(Visibility::Visible);
    }
    *app.world_mut().get_mut::<Transform>(camera).unwrap() =
        Transform::from_xyz(3.0, 2.2, -4.5).looking_at(Vec3::Y * 0.9, Vec3::Y);
    for (name, kind, combo, phase, elapsed, clear, corpse) in [
        (
            "late-second",
            EncounterMoveKind::PrisonerClaws,
            Some((2, 2)),
            MovePhase::Telegraph,
            40,
            false,
            false,
        ),
        (
            "expired",
            EncounterMoveKind::PrisonerClaws,
            Some((2, 2)),
            MovePhase::Telegraph,
            80,
            false,
            false,
        ),
        (
            "cancelled-phase-two",
            EncounterMoveKind::PrisonerClaws,
            Some((2, 2)),
            MovePhase::Telegraph,
            0,
            true,
            false,
        ),
        (
            "replacement",
            EncounterMoveKind::BonebreakerJaws,
            None,
            MovePhase::Telegraph,
            45,
            false,
            false,
        ),
        (
            "death-preparation",
            EncounterMoveKind::BonebreakerJaws,
            None,
            MovePhase::Telegraph,
            45,
            false,
            true,
        ),
    ] {
        deliver(
            &mut app,
            kind,
            combo,
            phase,
            elapsed,
            Vec3::ZERO,
            clear,
            corpse,
        );
        if corpse {
            app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(
                1.0 / 60.0,
            )));
            for _ in 0..45 {
                app.update();
            }
        }
        capture(&mut app, name);
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
