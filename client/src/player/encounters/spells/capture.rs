//! Opt-in GPU review of spell shapes and final-stage regalia through the production
//! snapshot consumer, animator, boundary cues, spell layer and encounter readings.
use super::*;
use crate::net::{EncounterTimelineInbox, MobAction, MoveEnd, SessionParams};
use crate::player::encounters::tests;
use crate::player::{ApplySnapshots, InputMode, SnapshotBuffer, WorldCamera, mobs};
use EncounterMoveKind::*;
use std::time::{Duration, Instant};

/// Catalogue durations at 20 Hz and the server's placement rules for each pulse.
fn announced(
    kind: EncounterMoveKind,
    phase: MovePhase,
    pulse: u8,
) -> (u32, Vec<HazardVolume>, Option<(u8, u8)>) {
    let ticks = match (kind, phase) {
        (SepulchreSpear, MovePhase::Telegraph) => 28,
        (SepulchreSpear, MovePhase::Release) => 16,
        (KingsSentence, MovePhase::Telegraph) => 24,
        (Burial, MovePhase::Channel) => 14,
        (EdictOfTheGraves, MovePhase::Channel) => 16,
        (RequiemOfTheBuried, MovePhase::Channel) => 18,
        (_, MovePhase::Recovery) => 32,
        _ => 30,
    };
    let sectors = |ring: f32, radius: f32| {
        (0..2)
            .map(|sector| {
                let bearing = (f32::from(pulse) * 2.0 + sector as f32 * 3.0) * TAU / 6.0;
                HazardVolume {
                    shape: HazardShape::Disc,
                    origin: [bearing.cos() * ring, 1.2, bearing.sin() * ring],
                    direction: [0.0; 3],
                    radius,
                    height: 2.4,
                }
            })
            .collect()
    };
    let lane = |half_width, radius, height| {
        vec![HazardVolume {
            shape: HazardShape::Line { half_width },
            origin: [0.0, 1.4, 0.0],
            direction: [0.0, 0.0, -1.0],
            radius,
            height,
        }]
    };
    let hazards = match kind {
        SepulchreSpear => lane(0.9, 17.6, 2.6),
        KingsSentence => lane(1.1, 5.0, 3.0),
        Burial => {
            let inner = f32::from(pulse) * 2.0;
            vec![HazardVolume {
                shape: HazardShape::Ring {
                    inner_radius: inner,
                },
                origin: [0.0, 1.0, 0.0],
                direction: [0.0; 3],
                radius: inner + 2.0,
                height: 2.0,
            }]
        }
        EdictOfTheGraves => sectors(7.0, 3.0),
        _ => sectors(6.0, 3.2),
    };
    let total = if kind == Burial { 4 } else { 3 };
    (
        ticks,
        hazards,
        (phase == MovePhase::Channel).then_some((pulse, total)),
    )
}

type View = ([f32; 3], [f32; 3]);

/// One review action: an announcement, cosmetic time, a disappearance or a frame.
enum Step {
    Move(
        u8,
        EncounterMoveKind,
        MovePhase,
        u8,
        u32,
        MobAction,
        Option<MoveEnd>,
    ),
    Wait(u64, usize),
    Vanish,
    Shot(&'static str, View),
}

#[test]
#[ignore = "requires a render adapter; writes spell and regalia PNGs to the temporary directory"]
fn capture_spells_and_regalia() {
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
    // A player-sized scale reference, not a collision simulation.
    let marker = app
        .world_mut()
        .resource_mut::<Assets<Mesh>>()
        .add(Cuboid::new(0.6, 1.8, 0.6));
    app.world_mut().spawn((
        Mesh3d(marker),
        MeshMaterial3d(stone),
        Transform::from_xyz(-2.2, 0.9, 0.6),
    ));

    for _ in 0..90 {
        app.update();
        std::thread::sleep(Duration::from_millis(10));
    }
    use MobAction::{Corpse, Windup};
    use MovePhase::{Channel, Release, Telegraph};
    use Step::{Move, Shot, Vanish, Wait};
    const NEAR: View = ([-1.7, 2.5, -2.9], [-0.45, 2.25, -0.35]);
    const LANE: View = ([9.0, 11.0, 5.0], [0.0, 0.5, -8.0]);
    const ABOVE: View = ([0.0, 17.0, 11.0], [0.0, 0.0, -1.0]);
    const FACE: View = ([1.0, 2.5, -3.3], [0.0, 1.6, 0.0]);
    const HOLD: MovePhase = MovePhase::Recovery;
    const OPEN: MobAction = MobAction::Recovery;
    let script = [
        // Stage one: the crystal forms in the raised hand, then crosses its locked lane.
        Move(1, SepulchreSpear, Telegraph, 0, 10, Windup, None),
        Shot("spear-telegraph-10", NEAR),
        Move(1, SepulchreSpear, Telegraph, 0, 60, Windup, None),
        Shot("spear-telegraph-60", NEAR),
        Move(1, SepulchreSpear, Telegraph, 0, 100, Windup, None),
        Shot("spear-telegraph-100", NEAR),
        Move(1, SepulchreSpear, Release, 0, 0, Windup, None),
        Shot("spear-release-0", LANE),
        Move(1, SepulchreSpear, Release, 0, 40, Windup, None),
        Shot("spear-release-40", LANE),
        Move(1, SepulchreSpear, Release, 0, 100, Windup, None),
        Shot("spear-release-100", LANE),
        // Stage two rituals from above: one announced pulse at a time.
        Move(2, Burial, Telegraph, 0, 60, Windup, None),
        Shot("burial-telegraph-60", ABOVE),
        Move(2, Burial, Channel, 0, 50, Windup, None),
        Shot("burial-pulse1-50", ABOVE),
        Move(2, Burial, Channel, 1, 50, Windup, None),
        Shot("burial-pulse2-50", ABOVE),
        Move(2, Burial, Channel, 2, 50, Windup, None),
        Shot("burial-pulse3-50", ABOVE),
        Move(2, Burial, Channel, 3, 50, Windup, None),
        Shot("burial-pulse4-50", ABOVE),
        Move(2, Burial, Channel, 1, 100, Windup, None),
        Shot("burial-pulse2-contact", ABOVE),
        Move(2, EdictOfTheGraves, Telegraph, 0, 60, Windup, None),
        Shot("edict-telegraph-60", ABOVE),
        Move(2, EdictOfTheGraves, Channel, 0, 50, Windup, None),
        Shot("edict-pulse1-50", ABOVE),
        Move(2, EdictOfTheGraves, Channel, 1, 50, Windup, None),
        Shot("edict-pulse2-50", ABOVE),
        Move(2, EdictOfTheGraves, Channel, 2, 50, Windup, None),
        Shot("edict-pulse3-50", ABOVE),
        Move(2, EdictOfTheGraves, Channel, 1, 100, Windup, None),
        Shot("edict-pulse2-contact", ABOVE),
        Move(
            2,
            EdictOfTheGraves,
            Channel,
            1,
            50,
            Windup,
            Some(MoveEnd::Cancelled),
        ),
        Shot("edict-cancelled", ABOVE),
        // The transition is the first final-stage announcement for a body seen before it.
        Move(2, SepulchreSpear, HOLD, 0, 50, OPEN, None),
        Wait(50, 4),
        Shot("regalia-stage2-front", ([1.0, 2.5, -3.3], [0.0, 1.9, 0.0])),
        Move(3, RequiemOfTheBuried, Telegraph, 0, 10, Windup, None),
        Wait(16, 16),
        Shot("regalia-mask-falling", ([1.4, 2.2, -3.6], [0.0, 1.3, -0.2])),
        Wait(50, 30),
        Move(3, KingsSentence, HOLD, 0, 50, OPEN, None),
        Wait(50, 2),
        Shot("regalia-mask-floor", ([0.4, 1.5, -2.3], [-0.3, 0.05, -0.4])),
        Shot("regalia-final-front", FACE),
        Shot("regalia-final-side", ([3.4, 2.4, 0.0], [0.0, 1.6, 0.0])),
        Shot("regalia-final-rear", ([0.0, 2.6, 3.6], [0.0, 1.8, 0.0])),
        Shot("regalia-final-13", ([0.0, 1.7, -13.0], [0.0, 1.8, 0.0])),
        Shot("regalia-final-25", ([0.0, 1.7, -25.0], [0.0, 1.8, 0.0])),
        Move(3, RequiemOfTheBuried, Channel, 0, 50, Windup, None),
        Shot("requiem-pulse1-50", ABOVE),
        Move(3, RequiemOfTheBuried, Channel, 1, 50, Windup, None),
        Shot("requiem-pulse2-50", ABOVE),
        Move(3, RequiemOfTheBuried, Channel, 2, 50, Windup, None),
        Shot("requiem-pulse3-50", ABOVE),
        Move(3, RequiemOfTheBuried, Channel, 2, 100, Windup, None),
        Shot("requiem-pulse3-contact", ABOVE),
        Shot("regalia-core-flare", ([0.9, 2.3, -2.6], [0.0, 1.8, 0.0])),
        // Late visibility: a new body already in the final stage replays no fall.
        Vanish,
        Move(3, KingsSentence, Telegraph, 0, 40, Windup, None),
        Wait(50, 3),
        Shot("regalia-late-final", FACE),
        // Death after a witnessed fall: the mask stays where it landed, the core goes out.
        Vanish,
        Move(2, KingsSentence, Telegraph, 0, 40, Windup, None),
        Wait(50, 3),
        Move(3, KingsSentence, Telegraph, 0, 60, Windup, None),
        Wait(50, 30),
        Move(3, KingsSentence, HOLD, 0, 50, Corpse, None),
        Wait(50, 30),
        Shot("regalia-corpse", ([3.0, 2.6, -3.4], [0.0, 0.4, 0.0])),
    ];
    let mut tick = 1000u32;
    for step in script {
        match step {
            Move(stage, kind, phase, pulse, percent, action, ended) => {
                tick += 100;
                let mut snapshot = tests::snapshot(tick);
                snapshot.mobs[0].kind = MobKind::DraugrKing;
                snapshot.mobs[0].action = action;
                let (ticks, hazards, pulse) = announced(kind, phase, pulse);
                let mut timeline = tests::timeline();
                (timeline.boss, timeline.phase) = (MobKind::DraugrKing, stage);
                let one = &mut timeline.moves[0];
                one.move_instance_id = u64::from(tick);
                (one.kind, one.phase, one.phase_ticks, one.pulse) = (kind, phase, ticks, pulse);
                one.phase_started_tick = tick - percent * (ticks - 1) / 100;
                one.hazards = if ended.is_some() { Vec::new() } else { hazards };
                one.interruptible = kind == RequiemOfTheBuried && phase == Channel;
                one.ended = ended;
                app.world_mut()
                    .resource_mut::<SnapshotBuffer>()
                    .accept(snapshot, Instant::now() - Duration::from_millis(100));
                app.world_mut()
                    .resource_mut::<EncounterTimelineInbox>()
                    .push(timeline);
            }
            Wait(millis, frames) => {
                app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
                    millis,
                )));
                for _ in 0..frames {
                    app.update();
                }
            }
            Vanish => {
                tick += 1;
                let mut absent = tests::snapshot(tick);
                absent.mobs.clear();
                app.world_mut()
                    .resource_mut::<SnapshotBuffer>()
                    .accept(absent, Instant::now());
                app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
                    50,
                )));
                for _ in 0..3 {
                    app.update();
                }
            }
            Shot(name, (from, at)) => {
                *app.world_mut().get_mut::<Transform>(camera).unwrap() =
                    Transform::from_translation(Vec3::from_array(from))
                        .looking_at(Vec3::from_array(at), Vec3::Y);
                app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::ZERO));
                let output = std::env::temp_dir().join(format!("spells-1034-{name}.png"));
                let _ = std::fs::remove_file(&output);
                for _ in 0..8 {
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
            }
        }
    }
}
