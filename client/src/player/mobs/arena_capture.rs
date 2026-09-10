//! Opt-in GPU review of both first-dungeon bosses in motion inside the shipped chamber.
//!
//! The chamber is read at capture time from the server's own drawing,
//! `server/internal/world/schematic_instance.go`, through the schematic legend into the
//! client's chunk store, and drawn by the production chunk mesher and terrain material. It is
//! unrotated, and lit for review by an ambient term and an unshadowed directional light: the
//! drawing has a roof, and the dungeon's own lighting is not what this reviews.
//!
//! Every frame also measures clipping on the CPU: each vertex of the boss's visible meshes
//! against the chamber's solid voxels and the floor top, written beside the PNGs.
use std::path::Path;

use super::*;
use crate::net::{
    ChunkCoord, EncounterMoveKind, EncounterTimelineInbox, HazardShape, HazardVolume, MovePhase,
    SessionParams,
};
use crate::player::encounters::{self, tests as fixture};
use crate::world::{ChunkStore, MeshStats, VoxelChunk, WorldPlugin, palette};
use EncounterMoveKind::*;

const CHUNK: usize = 32;
const FLOOR_TOP: f32 = 1.0;
/// How far inside a voxel a vertex must be to count as clipping it.
const INSET: f32 = 0.02;

/// The chamber's voxels, `[x, y, z]` in the drawing's own axes: a layer is a `y`, a row a `z`,
/// a column an `x`.
struct Chamber {
    size: [usize; 3],
    blocks: Vec<crate::world::BlockId>,
}

impl Chamber {
    fn read() -> Self {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../server/internal/world/schematic_instance.go");
        let source = std::fs::read_to_string(&path).expect("the shipped chamber drawing");
        let mut layers: Vec<Vec<String>> = Vec::new();
        let mut rows: Option<Vec<String>> = None;
        for line in source.lines().map(str::trim) {
            if line.starts_with("[]string{ // y=") {
                rows = Some(Vec::new());
            } else if let Some(current) = rows.as_mut() {
                if let Some(row) = line.strip_prefix('"').and_then(|l| l.strip_suffix("\",")) {
                    current.push(row.to_owned());
                } else if line.starts_with('}') {
                    layers.push(rows.take().unwrap());
                }
            }
        }
        let (height, depth, width) = (layers.len(), layers[0].len(), layers[0][0].len());
        assert_eq!(height, 10, "the chamber drawing has ten layers");
        let mut blocks = vec![palette::AIR; width * height * depth];
        for (y, layer) in layers.iter().enumerate() {
            for (z, row) in layer.iter().enumerate() {
                for (x, rune) in row.chars().enumerate() {
                    // The subset of the server's schematicLegend this drawing uses; the
                    // block-palette parity test pins those names to the client's ids.
                    blocks[(y * depth + z) * width + x] = match rune {
                        '_' => palette::AIR,
                        'b' => palette::BASALT,
                        'K' => palette::BLACK_BRICK,
                        'k' => palette::BLACK_BRICK_WORN,
                        'U' => palette::RUNE_STONE,
                        'v' => palette::PORTAL_VEIL,
                        'O' => palette::PORTAL_HEART,
                        other => panic!("rune {other:?} is outside this capture's legend"),
                    };
                }
            }
        }
        Self {
            size: [width, height, depth],
            blocks,
        }
    }

    fn block(&self, x: usize, y: usize, z: usize) -> crate::world::BlockId {
        let [width, height, depth] = self.size;
        if x >= width || y >= height || z >= depth {
            return palette::AIR;
        }
        self.blocks[(y * depth + z) * width + x]
    }

    /// Whether a point lies strictly inside a solid voxel, at least [`INSET`] from its faces.
    fn clips(&self, p: Vec3) -> bool {
        if p.cmplt(Vec3::ZERO).any() {
            return false;
        }
        let voxel = p.floor();
        let within = p - voxel;
        within.cmpge(Vec3::splat(INSET)).all()
            && within.cmple(Vec3::splat(1.0 - INSET)).all()
            && palette::is_solid(self.block(voxel.x as usize, voxel.y as usize, voxel.z as usize))
    }

    /// Stores every chunk the drawing spans and answers how many hold any voxel: a chunk of
    /// pure air is stored and never meshed.
    fn insert_into(&self, store: &mut ChunkStore) -> usize {
        let [width, height, depth] = self.size;
        let mut solid = 0;
        for cx in 0..width.div_ceil(CHUNK) {
            for cz in 0..depth.div_ceil(CHUNK) {
                let mut chunk = VoxelChunk::all_air(CHUNK);
                let mut any = false;
                for y in 0..height {
                    for z in 0..CHUNK {
                        for x in 0..CHUNK {
                            let block = self.block(cx * CHUNK + x, y, cz * CHUNK + z);
                            if block != palette::AIR {
                                chunk.set(x, y, z, block);
                                any = true;
                            }
                        }
                    }
                }
                solid += usize::from(any);
                store.insert(
                    ChunkCoord {
                        cx: cx as i32,
                        cy: 0,
                        cz: cz as i32,
                    },
                    chunk,
                );
            }
        }
        solid
    }
}

/// One boss as the snapshot stream names it.
#[derive(Clone, Copy)]
struct Boss {
    kind: MobKind,
    entity: u64,
}

enum Announce {
    /// No timeline at all: an idle body before any pull.
    Nothing,
    /// A live encounter announcing nothing.
    Empty,
    /// One move: kind, combination step, phase, pulse and how far through the phase.
    Move(EncounterMoveKind, Option<(u8, u8)>, MovePhase, u8, u32),
}

/// The server catalogue at 20 Hz after #1037 part 2: phase ticks and each region, placed
/// from the boss's root and facing the way the server places them.
fn region(
    boss: MobKind,
    kind: EncounterMoveKind,
    combo: Option<(u8, u8)>,
    phase: MovePhase,
    pulse: u8,
    pos: Vec3,
    forward: Vec3,
) -> (u32, Vec<HazardVolume>, Option<(u8, u8)>) {
    use MovePhase::*;
    let bearing = match (kind, combo) {
        (PrisonerClaws | ThreeTolls, Some((1, _))) => -0.45_f32,
        (PrisonerClaws | ThreeTolls, Some((2, _))) => 0.45,
        _ => 0.0,
    };
    let aim = Vec3::new(
        forward.x * bearing.cos() - forward.z * bearing.sin(),
        0.0,
        forward.x * bearing.sin() + forward.z * bearing.cos(),
    );
    let last = combo.is_none_or(|(step, total)| step == total);
    let ticks = match (kind, phase) {
        (BiteAndTear, Telegraph) => 18,
        (BiteAndTear, Release) | (ThreeTolls, Release) => 4,
        (PrisonerClaws, Telegraph) | (PredatorLeap, Telegraph) => 20,
        (PrisonerClaws | BonebreakerJaws, Release) => 6,
        (CollarCharge, Telegraph) | (KingsSentence, Telegraph) => 24,
        (CollarCharge, Release) | (ThreeTolls, Telegraph) => 18,
        (PredatorLeap, Release) => 12,
        (SepulchreSpear, Telegraph) => 28,
        (BonebreakerJaws, Telegraph) | (_, Telegraph) => 30,
        (KingsSentence, Release) => 5,
        (SepulchreSpear, Release) => 16,
        (Burial, Channel) => 20,
        (_, Channel) => 26,
        (_, Recovery) if !last => 8,
        (PrisonerClaws, Recovery) if combo.is_none() => 28,
        (BiteAndTear | PrisonerClaws, Recovery) => 36,
        (ThreeTolls, Recovery) => 44,
        (CollarCharge | BonebreakerJaws, Recovery) => 50,
        (PredatorLeap | SepulchreSpear, Recovery) => 32,
        (KingsSentence | EdictOfTheGraves, Recovery) => 36,
        _ => 40,
    };
    let centre = pos + Vec3::Y * body(boss).height / 2.0;
    let volume = |shape, origin: Vec3, direction: Vec3, radius, height| HazardVolume {
        shape,
        origin: origin.to_array(),
        direction: direction.to_array(),
        radius,
        height,
    };
    let sectors = |ring: f32, radius: f32| {
        (0..2u8)
            .map(|sector| {
                let slot = (u32::from(pulse) * 2 + u32::from(sector) * 3) % 6;
                let angle = std::f32::consts::TAU * slot as f32 / 6.0;
                let at = pos + Vec3::new(angle.cos() * ring, 1.2, angle.sin() * ring);
                volume(HazardShape::Disc, at, Vec3::ZERO, radius, 2.4)
            })
            .collect::<Vec<_>>()
    };
    let hazards = if phase == Recovery {
        Vec::new()
    } else {
        match kind {
            BiteAndTear => vec![volume(
                HazardShape::Cone { half_angle: 0.70 },
                centre,
                aim,
                3.0,
                2.2,
            )],
            PrisonerClaws => vec![volume(
                HazardShape::Cone { half_angle: 1.05 },
                centre,
                aim,
                3.4,
                2.2,
            )],
            BonebreakerJaws => vec![volume(
                HazardShape::Cone { half_angle: 0.38 },
                centre,
                aim,
                3.6,
                2.2,
            )],
            CollarCharge => vec![volume(
                HazardShape::Line { half_width: 1.4 },
                centre,
                aim,
                9.9,
                2.4,
            )],
            PredatorLeap => vec![volume(
                HazardShape::Disc,
                pos + aim * 6.0 + Vec3::Y * 1.5,
                Vec3::ZERO,
                3.0,
                3.0,
            )],
            KingsSentence => vec![volume(
                HazardShape::Line { half_width: 1.1 },
                centre,
                aim,
                3.3,
                3.0,
            )],
            ThreeTolls if combo == Some((3, 3)) => {
                vec![volume(
                    HazardShape::Line { half_width: 0.65 },
                    centre,
                    aim,
                    3.3,
                    3.0,
                )]
            }
            ThreeTolls => vec![volume(
                HazardShape::Cone { half_angle: 0.95 },
                centre,
                aim,
                3.3,
                3.0,
            )],
            SepulchreSpear => vec![volume(
                HazardShape::Line { half_width: 0.9 },
                centre,
                aim,
                17.6,
                2.6,
            )],
            Burial => {
                let inner = f32::from(pulse) * 2.0;
                vec![volume(
                    HazardShape::Ring {
                        inner_radius: inner,
                    },
                    pos + Vec3::Y,
                    Vec3::ZERO,
                    inner + 2.0,
                    2.0,
                )]
            }
            EdictOfTheGraves => sectors(7.0, 3.0),
            _ => sectors(6.0, 3.2),
        }
    };
    let total = if kind == Burial { 4 } else { 3 };
    (ticks, hazards, (phase == Channel).then_some((pulse, total)))
}

#[allow(clippy::too_many_arguments)]
fn announce(
    app: &mut App,
    tick: u32,
    boss: Boss,
    pos: Vec3,
    yaw: f32,
    action: MobAction,
    stage: u8,
    what: Announce,
) {
    announce_from(app, tick, boss, pos, pos, yaw, action, stage, what);
}

/// [`announce`] for a move whose region the server fixed where it was announced — a charge's
/// lane from its run start, a leap's landing disc — while the snapshot carries the body on.
#[allow(clippy::too_many_arguments)]
fn announce_from(
    app: &mut App,
    tick: u32,
    boss: Boss,
    pos: Vec3,
    origin: Vec3,
    yaw: f32,
    action: MobAction,
    stage: u8,
    what: Announce,
) {
    let mut snapshot = fixture::snapshot(tick);
    let mob = &mut snapshot.mobs[0];
    (mob.entity_id, mob.kind, mob.pos, mob.yaw, mob.action) =
        (boss.entity, boss.kind, pos.to_array(), yaw, action);
    app.world_mut()
        .resource_mut::<SnapshotBuffer>()
        .accept(snapshot, Instant::now() - Duration::from_millis(100));
    let mut timeline = fixture::timeline();
    (timeline.boss_entity_id, timeline.boss, timeline.phase) = (boss.entity, boss.kind, stage);
    match what {
        Announce::Nothing => return,
        Announce::Empty => timeline.moves.clear(),
        Announce::Move(kind, combo, phase, pulse, percent) => {
            let forward = Vec3::new(-yaw.sin(), 0.0, -yaw.cos());
            let (ticks, hazards, pulses) =
                region(boss.kind, kind, combo, phase, pulse, origin, forward);
            let one = &mut timeline.moves[0];
            one.move_instance_id = u64::from(tick);
            (
                one.kind,
                one.combo,
                one.phase,
                one.phase_ticks,
                one.pulse,
                one.hazards,
            ) = (kind, combo, phase, ticks, pulses, hazards);
            one.phase_started_tick = tick - percent * (ticks - 1) / 100;
            one.aim = Some(forward.to_array());
            one.target_entity_id = None;
            one.interruptible = kind == RequiemOfTheBuried && phase == MovePhase::Channel;
        }
    }
    app.world_mut()
        .resource_mut::<EncounterTimelineInbox>()
        .push(timeline);
}

/// Vertices of the boss's visible meshes, how many lie strictly inside solid chamber
/// voxels, and the deepest any lies below the floor top.
fn clipping(app: &mut App, boss: Boss, chamber: &Chamber) -> (usize, usize, f32) {
    let world = app.world_mut();
    let Some(owner) = world
        .query::<(Entity, &Mob)>()
        .iter(world)
        .find(|(_, mob)| mob.entity_id == boss.entity)
        .map(|(entity, _)| entity)
    else {
        return (0, 0, 0.0);
    };
    let parts: Vec<(Handle<Mesh>, GlobalTransform)> = world
        .query::<(&MobVisual, &Mesh3d, &GlobalTransform, &InheritedVisibility)>()
        .iter(world)
        .filter(|(visual, _, _, visible)| visual.owner == owner && visible.get())
        .map(|(_, mesh, transform, _)| (mesh.0.clone(), *transform))
        .collect();
    let meshes = world.resource::<Assets<Mesh>>();
    let (mut total, mut inside, mut below) = (0, 0, 0.0_f32);
    for (handle, transform) in parts {
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(points)) = meshes
            .get(&handle)
            .and_then(|mesh| mesh.attribute(Mesh::ATTRIBUTE_POSITION))
        else {
            continue;
        };
        for &point in points {
            let p = transform.transform_point(Vec3::from_array(point));
            total += 1;
            below = below.max(FLOOR_TOP - p.y);
            if p.y > FLOOR_TOP + INSET && chamber.clips(p) {
                inside += 1;
            }
        }
    }
    (total, inside, below.max(0.0))
}

struct Review {
    camera: Entity,
    target: Handle<Image>,
    rows: Vec<String>,
}

impl Review {
    /// Settles the frame, measures the boss, and waits for the PNG on disk.
    fn shot(
        &mut self,
        app: &mut App,
        chamber: &Chamber,
        boss: Boss,
        name: &str,
        from: Vec3,
        at: Vec3,
    ) {
        *app.world_mut().get_mut::<Transform>(self.camera).unwrap() =
            Transform::from_translation(from).looking_at(at, Vec3::Y);
        app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            Duration::ZERO,
        ));
        for _ in 0..8 {
            app.update();
            std::thread::sleep(Duration::from_millis(5));
        }
        let (total, inside, below) = clipping(app, boss, chamber);
        assert!(total > 0, "{name}: the boss draws no vertices");
        let species = if boss.kind == MobKind::DraugrKing {
            "king"
        } else {
            "guardian"
        };
        self.rows
            .push(format!("{species},{name},{total},{inside},{below:.4}"));
        let output = std::env::temp_dir().join(format!("arena-1037-{species}-{name}.png"));
        let _ = std::fs::remove_file(&output);
        app.world_mut()
            .spawn(bevy::render::view::screenshot::Screenshot::image(
                self.target.clone(),
            ))
            .observe(bevy::render::view::screenshot::save_to_disk(output.clone()));
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

fn advance(app: &mut App, millis: u64, frames: usize) {
    app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
        Duration::from_millis(millis),
    ));
    for _ in 0..frames {
        app.update();
    }
}

fn yaw_toward(direction: Vec3) -> f32 {
    (-direction.x).atan2(-direction.z)
}

#[test]
#[ignore = "requires a render adapter and the server source; writes arena PNGs and a clipping CSV to the temporary directory"]
fn capture_bosses_in_the_shipped_chamber() {
    use MovePhase::{Channel, Recovery, Release, Telegraph};
    use bevy::camera::RenderTarget;
    use bevy::core_pipeline::tonemapping::Tonemapping;
    use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
    use bevy::window::ExitCondition;

    let chamber = Chamber::read();
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
        spawn: [16.5, 1.0, 5.5],
        world_seed: 1,
        tick_rate: 20,
        chunk_size: CHUNK as u16,
        view_distance: 8,
        inventory_slots: 37,
        hotbar_slots: 9,
        equipment_slots: 4,
        player_token: crate::net::ANY_TOKEN,
        voice_range_blocks: 0.0,
    }))
    .insert_resource(InputMode::Playing)
    .init_resource::<SnapshotBuffer>()
    .add_plugins(WorldPlugin)
    .add_systems(Startup, (create_visuals, setup_regalia))
    .add_systems(
        Update,
        (apply_snapshots, ApplyDeferred, animate)
            .chain()
            .in_set(crate::player::ApplySnapshots),
    )
    .add_systems(
        Update,
        (
            pose_encounters.after(encounters::reconcile),
            present_regalia.after(pose_encounters),
        ),
    )
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
            WorldCamera,
            IsDefaultUiCamera,
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
            Transform::default(),
        ))
        .id();
    // Unshadowed: the drawing's roof would otherwise leave the whole chamber in shadow.
    app.world_mut().spawn((
        DirectionalLight {
            illuminance: 5000.0,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_xyz(-3.0, 6.0, -5.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    let meshable = chamber.insert_into(&mut app.world_mut().resource_mut::<ChunkStore>());
    let held = chamber.size[0].div_ceil(CHUNK) * chamber.size[2].div_ceil(CHUNK);
    // Meshing runs on tasks: wait until every chunk holding voxels has a mesh.
    for _ in 0..2000 {
        app.update();
        let stats = *app.world().resource::<MeshStats>();
        if stats.chunks_held == held
            && stats.meshed_chunks == meshable
            && stats.in_flight == 0
            && stats.queued == 0
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let stats = *app.world().resource::<MeshStats>();
    assert_eq!(
        (stats.chunks_held, stats.meshed_chunks),
        (held, meshable),
        "the chamber never finished meshing: {stats:?}"
    );

    let mut review = Review {
        camera,
        target,
        rows: vec!["boss,scene,vertices,inside_solid,below_floor".to_owned()],
    };
    let mut tick = 1000u32;
    let mut next = || {
        tick += 100;
        tick
    };
    // Eye height for the distance views: a standing player's eyes over the floor top.
    let eye = Vec3::Y * 2.7;
    let phases = [
        ("prep", Telegraph, 100),
        ("release-50", Release, 50),
        ("release-100", Release, 100),
        ("recovery-50", Recovery, 50),
    ];

    // ----- The Vargr guardian, courtyard -----
    let guardian = Boss {
        kind: MobKind::VargrGuardian,
        entity: 21,
    };
    let home = Vec3::new(16.5, FLOOR_TOP, 14.5);
    announce(
        &mut app,
        next(),
        guardian,
        home,
        std::f32::consts::PI,
        MobAction::Idle,
        1,
        Announce::Nothing,
    );
    // A first frame can be written before the terrain and mob pipelines have compiled; the
    // warm-up frame is taken and discarded so no recorded frame is that one.
    review.shot(
        &mut app,
        &chamber,
        guardian,
        "warm-up",
        home + Vec3::new(3.5, 2.6, 4.5),
        home + Vec3::Y * 0.9,
    );
    review.rows.pop();
    let _ = std::fs::remove_file(std::env::temp_dir().join("arena-1037-guardian-warm-up.png"));
    review.shot(
        &mut app,
        &chamber,
        guardian,
        "idle-home",
        home + Vec3::new(3.5, 2.6, 4.5),
        home + Vec3::Y * 0.9,
    );
    review.shot(
        &mut app,
        &chamber,
        guardian,
        "idle-13",
        Vec3::new(16.5, 0.0, 27.5) + eye,
        home + Vec3::Y * 0.9,
    );
    review.shot(
        &mut app,
        &chamber,
        guardian,
        "idle-25",
        Vec3::new(16.5, 0.0, 39.5) + eye,
        home + Vec3::Y * 0.9,
    );

    // Gait and a turn, walked toward the monolith at (8, 22) at the guardian's speed.
    let stand = Vec3::new(10.35, FLOOR_TOP, 22.5);
    let steps = 45;
    for frame in 1..=steps + 15 {
        let t = (frame as f32 / steps as f32).min(1.0);
        let pos = home.lerp(stand, t);
        let yaw = if frame <= steps {
            yaw_toward(stand - home)
        } else {
            yaw_toward(stand - home)
                .lerp(std::f32::consts::FRAC_PI_2, (frame - steps) as f32 / 15.0)
        };
        let tick = next();
        announce(
            &mut app,
            tick,
            guardian,
            pos,
            yaw,
            MobAction::Chase,
            1,
            Announce::Empty,
        );
        advance(&mut app, 50, 1);
        if [15, 30, 45, 60].contains(&frame) {
            review.shot(
                &mut app,
                &chamber,
                guardian,
                &format!("gait-{frame}"),
                pos + Vec3::new(3.0, 2.4, 3.5),
                pos + Vec3::Y * 0.9,
            );
        }
    }
    // Planted blows with the monolith 0.55 blocks from the body's front edge.
    let facing = std::f32::consts::FRAC_PI_2;
    for (name, kind, combo, stage) in [
        ("bite", BiteAndTear, Some((1, 2)), 1),
        ("claw", PrisonerClaws, None, 1),
        ("claw-left", PrisonerClaws, Some((1, 2)), 2),
        ("claw-right", PrisonerClaws, Some((2, 2)), 2),
        ("jaws", BonebreakerJaws, None, 2),
    ] {
        for (phase_name, phase, percent) in phases {
            let action = if phase == Recovery {
                MobAction::Recovery
            } else {
                MobAction::Windup
            };
            announce(
                &mut app,
                next(),
                guardian,
                stand,
                facing,
                action,
                stage,
                Announce::Move(kind, combo, phase, 0, percent),
            );
            review.shot(
                &mut app,
                &chamber,
                guardian,
                &format!("{name}-{phase_name}"),
                stand + Vec3::new(2.4, 2.8, 4.2),
                stand + Vec3::new(-1.0, 0.6, 0.0),
            );
        }
    }
    // The charge run into the east wall, and its impact stop.
    let start = Vec3::new(20.5, FLOOR_TOP, 14.5);
    let east = -std::f32::consts::FRAC_PI_2;
    announce(
        &mut app,
        next(),
        guardian,
        start,
        east,
        MobAction::Windup,
        1,
        Announce::Move(CollarCharge, None, Telegraph, 0, 100),
    );
    review.shot(
        &mut app,
        &chamber,
        guardian,
        "charge-prep",
        start + Vec3::new(1.0, 2.8, 5.0),
        start + Vec3::new(3.0, 0.6, 0.0),
    );
    let wall_stop = 30.0 - body(MobKind::VargrGuardian).width / 2.0;
    let release_start = next();
    for step in 1..=18u32 {
        let x = (start.x + 0.55 * step as f32).min(wall_stop);
        let pos = Vec3::new(x, FLOOR_TOP, start.z);
        let tick = release_start + step;
        announce_from(
            &mut app,
            tick,
            guardian,
            pos,
            start,
            east,
            MobAction::Windup,
            1,
            Announce::Move(CollarCharge, None, Release, 0, (step - 1) * 100 / 17),
        );
        advance(&mut app, 50, 1);
        if [6, 12].contains(&step) {
            review.shot(
                &mut app,
                &chamber,
                guardian,
                &format!("charge-run-{step}"),
                pos + Vec3::new(-1.0, 2.4, 4.5),
                pos + Vec3::Y * 0.9,
            );
        }
    }
    let stopped = Vec3::new(wall_stop, FLOOR_TOP, start.z);
    announce(
        &mut app,
        next(),
        guardian,
        stopped,
        east,
        MobAction::Recovery,
        1,
        Announce::Move(CollarCharge, None, Recovery, 0, 20),
    );
    review.shot(
        &mut app,
        &chamber,
        guardian,
        "charge-impact",
        stopped + Vec3::new(-2.5, 2.4, 4.0),
        stopped + Vec3::new(0.3, 0.9, 0.0),
    );
    // The leap across open floor.
    // East over clear floor: the arrival portal's row lies north of the anchor, and a leap
    // the server's collision would stop is not one to review.
    let landing = home + Vec3::new(6.0, 0.0, 0.0);
    announce(
        &mut app,
        next(),
        guardian,
        home,
        east,
        MobAction::Windup,
        1,
        Announce::Move(PredatorLeap, None, Telegraph, 0, 100),
    );
    review.shot(
        &mut app,
        &chamber,
        guardian,
        "leap-prep",
        home + Vec3::new(1.0, 2.4, 4.5),
        home + Vec3::new(3.0, 0.9, 0.0),
    );
    for (name, percent, pos) in [
        ("leap-air", 50, home.lerp(landing, 0.5)),
        ("leap-landing", 100, landing),
    ] {
        announce_from(
            &mut app,
            next(),
            guardian,
            pos,
            home,
            east,
            MobAction::Windup,
            1,
            Announce::Move(PredatorLeap, None, Release, 0, percent),
        );
        review.shot(
            &mut app,
            &chamber,
            guardian,
            name,
            pos + Vec3::new(0.5, 2.2, 4.5),
            pos + Vec3::Y * 0.9,
        );
    }
    // Stage two posture and readability at play distance.
    announce(
        &mut app,
        next(),
        guardian,
        home,
        std::f32::consts::PI,
        MobAction::Windup,
        2,
        Announce::Move(BonebreakerJaws, None, Telegraph, 0, 60),
    );
    review.shot(
        &mut app,
        &chamber,
        guardian,
        "jaws-prep-13",
        Vec3::new(16.5, 0.0, 27.5) + eye,
        home + Vec3::Y * 0.9,
    );
    review.shot(
        &mut app,
        &chamber,
        guardian,
        "jaws-prep-25",
        Vec3::new(16.5, 0.0, 39.5) + eye,
        home + Vec3::Y * 0.9,
    );
    // Death beside the monolith.
    announce(
        &mut app,
        next(),
        guardian,
        stand,
        facing,
        MobAction::Corpse,
        2,
        Announce::Empty,
    );
    for (name, millis) in [("death-start", 0), ("death-mid", 250), ("death-end", 1000)] {
        advance(&mut app, millis, 1);
        review.shot(
            &mut app,
            &chamber,
            guardian,
            name,
            stand + Vec3::new(2.4, 2.6, 3.8),
            stand + Vec3::new(-0.6, 0.4, 0.0),
        );
    }

    // ----- The Draugr king, hall -----
    let king = Boss {
        kind: MobKind::DraugrKing,
        entity: 22,
    };
    let throne = Vec3::new(16.5, FLOOR_TOP, 52.5);
    let south = 0.0; // facing -Z, toward the gallery
    announce(
        &mut app,
        next(),
        king,
        throne,
        south,
        MobAction::Idle,
        1,
        Announce::Nothing,
    );
    for (name, millis) in [
        ("entrance-start", 0),
        ("entrance-mid", 650),
        ("entrance-end", 750),
    ] {
        advance(&mut app, millis, 1);
        review.shot(
            &mut app,
            &chamber,
            king,
            name,
            throne + Vec3::new(2.5, 2.6, -4.5),
            throne + Vec3::Y * 1.4,
        );
    }
    review.shot(
        &mut app,
        &chamber,
        king,
        "idle-13",
        Vec3::new(16.5, 0.0, 39.5) + eye,
        throne + Vec3::Y * 1.4,
    );
    review.shot(
        &mut app,
        &chamber,
        king,
        "idle-25",
        Vec3::new(16.5, 0.0, 27.5) + eye,
        throne + Vec3::Y * 1.4,
    );
    // Gait and a turn toward the monolith at (24, 60).
    let post = Vec3::new(22.9, FLOOR_TOP, 60.5);
    for frame in 1..=steps + 15 {
        let t = (frame as f32 / steps as f32).min(1.0);
        let pos = throne.lerp(post, t);
        let yaw = if frame <= steps {
            yaw_toward(post - throne)
        } else {
            yaw_toward(post - throne)
                .lerp(-std::f32::consts::FRAC_PI_2, (frame - steps) as f32 / 15.0)
        };
        announce(
            &mut app,
            next(),
            king,
            pos,
            yaw,
            MobAction::Chase,
            1,
            Announce::Empty,
        );
        advance(&mut app, 50, 1);
        if [15, 30, 45, 60].contains(&frame) {
            review.shot(
                &mut app,
                &chamber,
                king,
                &format!("gait-{frame}"),
                pos + Vec3::new(-3.0, 2.6, -3.5),
                pos + Vec3::Y * 1.4,
            );
        }
    }
    // Blade blows with the monolith 0.6 blocks from the body's front edge.
    let toward_monolith = -std::f32::consts::FRAC_PI_2;
    for (name, kind, combo) in [
        ("sentence", KingsSentence, None),
        ("toll-left", ThreeTolls, Some((1, 3))),
        ("toll-right", ThreeTolls, Some((2, 3))),
        ("toll-thrust", ThreeTolls, Some((3, 3))),
    ] {
        for (phase_name, phase, percent) in phases {
            let action = if phase == Recovery {
                MobAction::Recovery
            } else {
                MobAction::Windup
            };
            announce(
                &mut app,
                next(),
                king,
                post,
                toward_monolith,
                action,
                1,
                Announce::Move(kind, combo, phase, 0, percent),
            );
            review.shot(
                &mut app,
                &chamber,
                king,
                &format!("{name}-{phase_name}"),
                post + Vec3::new(-2.6, 3.0, -4.4),
                post + Vec3::new(1.0, 1.0, 0.0),
            );
        }
    }
    // Casts and channels at the throne.
    // Under the roof, whose underside is at y = 9: a camera above it sees the roof.
    let above = throne + Vec3::new(0.0, 6.8, -8.5);
    for (name, kind, phase, pulse, percent, stage, from) in [
        (
            "spear-prep",
            SepulchreSpear,
            Telegraph,
            0,
            100,
            1,
            throne + Vec3::new(2.5, 2.6, -4.0),
        ),
        (
            "spear-release",
            SepulchreSpear,
            Release,
            0,
            50,
            1,
            throne + Vec3::new(6.0, 6.0, -6.0),
        ),
        (
            "burial-prep",
            Burial,
            Telegraph,
            0,
            100,
            2,
            throne + Vec3::new(2.5, 2.6, -4.0),
        ),
        ("burial-pulse", Burial, Channel, 1, 100, 2, above),
        ("edict-pulse", EdictOfTheGraves, Channel, 0, 50, 2, above),
        (
            "requiem-pulse",
            RequiemOfTheBuried,
            Channel,
            1,
            50,
            3,
            above,
        ),
    ] {
        announce(
            &mut app,
            next(),
            king,
            throne,
            south,
            MobAction::Windup,
            stage,
            Announce::Move(kind, None, phase, pulse, percent),
        );
        review.shot(&mut app, &chamber, king, name, from, throne + Vec3::Y * 1.2);
    }
    // The final-stage transition, first seen after stage two, then play distances.
    announce(
        &mut app,
        next(),
        king,
        throne,
        south,
        MobAction::Recovery,
        2,
        Announce::Empty,
    );
    advance(&mut app, 50, 4);
    announce(
        &mut app,
        next(),
        king,
        throne,
        south,
        MobAction::Windup,
        3,
        Announce::Move(KingsSentence, None, Telegraph, 0, 10),
    );
    advance(&mut app, 16, 16);
    review.shot(
        &mut app,
        &chamber,
        king,
        "mask-falling",
        throne + Vec3::new(1.4, 2.2, -3.6),
        throne + Vec3::new(0.0, 1.3, -0.2),
    );
    advance(&mut app, 50, 30);
    review.shot(
        &mut app,
        &chamber,
        king,
        "final-front",
        throne + Vec3::new(1.0, 2.5, -3.3),
        throne + Vec3::Y * 1.6,
    );
    review.shot(
        &mut app,
        &chamber,
        king,
        "final-13",
        Vec3::new(16.5, 0.0, 39.5) + eye,
        throne + Vec3::Y * 1.4,
    );
    review.shot(
        &mut app,
        &chamber,
        king,
        "final-25",
        Vec3::new(16.5, 0.0, 27.5) + eye,
        throne + Vec3::Y * 1.4,
    );
    // Death beside the monolith.
    announce(
        &mut app,
        next(),
        king,
        post,
        toward_monolith,
        MobAction::Corpse,
        3,
        Announce::Empty,
    );
    for (name, millis) in [("death-start", 0), ("death-mid", 250), ("death-end", 1000)] {
        advance(&mut app, millis, 1);
        review.shot(
            &mut app,
            &chamber,
            king,
            name,
            post + Vec3::new(-2.6, 2.6, -3.8),
            post + Vec3::new(0.6, 0.5, 0.0),
        );
    }

    std::fs::write(
        std::env::temp_dir().join("arena-1037-clipping.csv"),
        review.rows.join("\n") + "\n",
    )
    .unwrap();
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
