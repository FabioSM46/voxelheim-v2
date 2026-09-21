//! Shared opt-in production castle scene; no runtime player wiring.
use super::*;
use crate::net::{ChunkCoord, SessionParams};
use crate::world::{ChunkStore, MeshStats, VoxelChunk, WorldPlugin, palette};
use bevy::asset::RenderAssetUsages;
use bevy::camera::RenderTarget;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::window::ExitCondition;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;
#[path = "castle_capture_draw_counts.rs"]
mod draw_counts;
#[path = "castle_capture_fixture.rs"]
mod fixture;
use fixture::CastleFixture;

struct CaptureConfig {
    output: PathBuf,
    tick: u64,
    view: String,
    eye: [f32; 3],
    target: [f32; 3],
    width: u32,
    height: u32,
}

impl CastleFixture {
    fn place_chunks(&self, store: &mut ChunkStore) -> usize {
        let mut chunks = BTreeMap::new();
        let [width, height, depth] = self.size;
        for y in 0..height {
            for z in 0..depth {
                for x in 0..width {
                    let block = self.blocks[(y * depth + z) * width + x];
                    let world = [
                        self.origin[0] + x as i64,
                        self.origin[1] + y as i64,
                        self.origin[2] + z as i64,
                    ];
                    let coord = (
                        world[0].div_euclid(32) as i32,
                        world[1].div_euclid(32) as i32,
                        world[2].div_euclid(32) as i32,
                    );
                    chunks
                        .entry(coord)
                        .or_insert_with(|| VoxelChunk::all_air(32))
                        .set(
                            world[0].rem_euclid(32) as usize,
                            world[1].rem_euclid(32) as usize,
                            world[2].rem_euclid(32) as usize,
                            block,
                        );
                }
            }
        }
        let count = chunks.len();
        for ((cx, cy, cz), chunk) in chunks {
            store.insert(ChunkCoord { cx, cy, cz }, chunk);
        }
        count
    }
    fn canonical_point(&self, point: [f32; 3]) -> Vec3 {
        let [x, y, z] = point;
        let (x, z) = match (self.actual_facing + self.review_turn) % 4 {
            0 => (x, z),
            1 => (63.0 - z, x),
            2 => (63.0 - x, 63.0 - z),
            3 => (z, 63.0 - x),
            _ => unreachable!(),
        };
        Vec3::new(
            self.building_origin[0] as f32 + x,
            self.building_origin[1] as f32 + y,
            self.building_origin[2] as f32 + z,
        )
    }
}

fn castle_app(
    fixture: &CastleFixture,
    config: &CaptureConfig,
) -> (App, Entity, Handle<Image>, usize) {
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
        clock: crate::net::WorldClock {
            day_length_ticks: 24000,
            night_start_ticks: 14400,
            night_end_ticks: 21600,
        },
        entity_id: 7,
        spawn: fixture.canonical_point([31.5, 0.0, 62.5]).to_array(),
        world_seed: fixture.seed,
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
    .init_resource::<CaptureReceipt>()
    .init_resource::<SnapshotBuffer>()
    .init_resource::<Weather>()
    .init_resource::<sky::SkyClock>()
    .add_plugins(WorldPlugin)
    .add_systems(Startup, (sky::spawn_sun, sky::spawn_sky))
    .add_systems(Update, (sky::drive_the_sky, sky::follow_the_eye).chain());
    static_props::register(&mut app);
    if std::env::var("CASTLE_CAPTURE_LIGHTING").as_deref() != Ok("off") {
        castle_lighting::register(&mut app);
    }
    if let Ok(path) = std::env::var("CASTLE_CAPTURE_SNAPSHOT") {
        let bytes = std::fs::read(path).expect("server snapshot export");
        let crate::net::CaptureMessage::Snapshot(mut snapshot) =
            crate::net::decode_for_capture(&bytes).expect("production snapshot decoder")
        else {
            panic!("expected snapshot envelope")
        };
        if std::env::var("CASTLE_CAPTURE_CANDLES").as_deref() == Ok("off") {
            snapshot.static_props.retain(|p| {
                !matches!(
                    p.kind,
                    crate::net::StaticPropKind::WallSconce
                        | crate::net::StaticPropKind::FloorCandelabrum
                        | crate::net::StaticPropKind::TableCandelabrum
                )
            });
        }
        if std::env::var("CASTLE_CAPTURE_FURNITURE").as_deref() == Ok("off") {
            snapshot.static_props.clear();
        }
        assert!(
            app.world_mut()
                .resource_mut::<SnapshotBuffer>()
                .accept(snapshot, std::time::Instant::now())
        );
    }
    while app.plugins_state() != bevy::app::PluginsState::Ready {
        std::thread::sleep(Duration::from_millis(10));
    }
    app.finish();
    app.cleanup();
    draw_counts::install(app.sub_app_mut(bevy::render::RenderApp).world_mut());
    app.world_mut()
        .resource_mut::<sky::SkyClock>()
        .freeze_for_capture(config.tick);
    app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
        Duration::ZERO,
    ));
    let mut image = Image::new_uninit(
        Extent3d {
            width: config.width,
            height: config.height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_descriptor.usage |=
        TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC | TextureUsages::TEXTURE_BINDING;
    let target = app.world_mut().resource_mut::<Assets<Image>>().add(image);
    let fixed = Daylight::FIXED;
    let camera = app
        .world_mut()
        .spawn((
            WorldCamera,
            bevy::camera::ShadowLodOrigin,
            bevy::camera::Exposure::default(),
            Camera3d::default(),
            Camera {
                clear_color: fixed.sky.into(),
                ..default()
            },
            AmbientLight {
                brightness: fixed.ambient_brightness,
                ..default()
            },
            Tonemapping::AcesFitted,
            Projection::Perspective(PerspectiveProjection {
                fov: crate::settings::DEFAULT_FIELD_OF_VIEW.to_radians(),
                ..default()
            }),
            RenderTarget::Image(target.clone().into()),
            Transform::from_translation(fixture.canonical_point(config.eye))
                .looking_at(fixture.canonical_point(config.target), Vec3::Y),
        ))
        .id();
    let chunks = fixture.place_chunks(&mut app.world_mut().resource_mut::<ChunkStore>());
    for (x, floors) in [(12.5, 4), (48.5, 5)] {
        for floor in 0..floors {
            let eye = fixture.canonical_point([x, floor as f32 * 7.0 + EYE_HEIGHT, 21.5]);
            assert!(
                precipitation::castle_capture_sheltered(app.world().resource::<ChunkStore>(), eye),
                "barred room lost precipitation shelter"
            );
        }
    }
    (app, camera, target, chunks)
}

#[test]
#[ignore = "requires an actual GPU and CASTLE_CAPTURE_FIXTURE/OUTPUT"]
fn capture_castle_production_scene() {
    let _capture = draw_counts::acquire();
    let data =
        std::fs::read(std::env::var("CASTLE_CAPTURE_FIXTURE").expect("fixture path")).unwrap();
    let mut fixture = CastleFixture::parse(
        &data,
        &[&[palette::AIR][..], &palette::PALETTE[..]].concat(),
    )
    .expect("validated server-authored fixture");
    if std::env::var("CASTLE_CAPTURE_WINDOWS").as_deref() == Ok("blocked") {
        for block in &mut fixture.blocks {
            if matches!(*block, palette::IRON_GRILLE_X | palette::IRON_GRILLE_Z) {
                *block = palette::STONE;
            }
        }
    }
    let view = std::env::var("CASTLE_CAPTURE_VIEW").unwrap_or_else(|_| "exterior_gate".into());
    let (eye, target) = match view.as_str() {
        "exterior_gate" => ([31.5, 35.0, 130.0], [31.5, 26.0, 28.0]),
        "gate_entry" => ([31.5, 2.3, 76.0], [31.5, 2.3, 60.0]),
        "west_stair" => ([8.5, EYE_HEIGHT, 37.5], [8.5, 7.0, 29.5]),
        "east_stair" => ([53.5, EYE_HEIGHT, 37.5], [53.5, 7.0, 29.5]),
        "bridge" => ([31.5, 21.0 + EYE_HEIGHT, 24.5], [46.5, 23.0, 24.5]),
        "nw_lookout" => ([8.5, 35.0 + EYE_HEIGHT, 14.5], [10.5, 36.0, 18.5]),
        "sw_lookout" => ([20.5, 29.0 + EYE_HEIGHT, 34.5], [18.0, 29.0, 30.5]),
        "ne_lookout" => ([50.5, 41.0 + EYE_HEIGHT, 14.5], [48.0, 41.0, 10.5]),
        "se_lookout" => ([42.5, 35.0 + EYE_HEIGHT, 34.5], [40.0, 35.0, 30.5]),
        "corridor" => ([16.5, EYE_HEIGHT, 28.5], [16.5, 1.5, 10.0]),
        "landing" => ([11.5, 7.0 + EYE_HEIGHT, 28.5], [11.5, 11.0, 34.5]),
        "window_patch" => ([12.5, 21.0 + EYE_HEIGHT, 24.5], [6.5, 22.0, 21.5]),
        "window_east" => ([50.5, 21.0 + EYE_HEIGHT, 24.5], [56.5, 22.0, 21.5]),
        "tower_study" => ([12.5, 35.0 + EYE_HEIGHT, 14.5], [10.5, 36.0, 12.5]),
        "banquet" => ([54.5, EYE_HEIGHT, 29.5], [44.0, 1.2, 20.5]),
        "study" => ([18.5, 7.0 + EYE_HEIGHT, 24.5], [22.5, 8.2, 14.5]),
        "throne" => ([46.5, 7.0 + EYE_HEIGHT, 21.5], [40.0, 9.0, 21.5]),
        _ => panic!("unknown fixed camera id"),
    };
    let trace = std::env::var("CASTLE_CAPTURE_TRACE")
        .ok()
        .map(|path| read_trace(&std::fs::read_to_string(path).expect("authoritative trace")));
    let mut config = CaptureConfig {
        output: std::env::var("CASTLE_CAPTURE_OUTPUT")
            .expect("output path")
            .into(),
        eye,
        target,
        view,
        tick: std::env::var("CASTLE_CAPTURE_TICK")
            .map(|v| v.parse().expect("fixed tick"))
            .unwrap_or(6000),
        width: 1280,
        height: 720,
    };
    if let Some(trace) = &trace {
        config.eye = [trace[0][0], trace[0][1] + EYE_HEIGHT, trace[0][2]];
        config.target = [config.eye[0], config.eye[1], config.eye[2] - 1.0];
        config.view = "walk_trace".into();
    }
    assert!(!config.output.exists(), "capture destination must be fresh");
    let (mut app, camera, target, chunks) = castle_app(&fixture, &config);
    let mut ready = false;
    let mut drained_frames = 0;
    for frame in 0..2000 {
        app.update();
        if frame == 0 && std::env::var("CASTLE_CAPTURE_LIGHTING").as_deref() == Ok("off") {
            let world = app.world_mut();
            for mut light in world.query::<&mut DirectionalLight>().iter_mut(world) {
                light.shadow_maps_enabled = false;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
        let stats = *app.world().resource::<MeshStats>();
        if frame >= 200
            && stats.chunks_held == chunks
            && stats.meshed_chunks > 0
            && stats.in_flight == 0
            && stats.queued == 0
        {
            drained_frames += 1;
            if drained_frames >= 10 {
                ready = true;
                break;
            }
        } else {
            drained_frames = 0;
        }
    }
    assert!(ready, "castle never completed production meshing");
    // GPU/mesher readiness uses zero elapsed time. Once ready, give production
    // timers exactly one second of idle warmup, independent of machine speed.
    for _ in 0..20 {
        update_capture(&mut app, Duration::from_millis(50));
    }
    draw_counts::take();
    app.update();
    app.sub_app(bevy::render::RenderApp)
        .world()
        .resource::<bevy::render::renderer::RenderDevice>()
        .poll(bevy::render::render_resource::PollType::wait_indefinitely())
        .unwrap();
    let counts = draw_counts::take();
    assert!(counts[0] > 0, "production scene issued no main mesh draws");
    let mut manifest = capture_manifest(&app, &fixture, &config, counts);
    let limits = app
        .world()
        .resource::<bevy::render::renderer::RenderDevice>()
        .limits();
    manifest.push_str(&format!("max_texture_array_layers={}\npoint_shadow_map_size={}\ndirectional_shadow_map_size={}\nexposure_ev100={}\ntonemapping=AcesFitted\ncamera_eye_local={:?}\ncamera_target_local={:?}\n", limits.max_texture_array_layers, app.world().resource::<bevy::light::PointLightShadowMap>().size, app.world().resource::<bevy::light::DirectionalLightShadowMap>().size, bevy::camera::Exposure::default().ev100, config.eye, config.target));
    manifest.push_str(&capture_costs(&mut app));
    manifest.push_str(&format!(
        "lighting={}\nwindows={}\n",
        std::env::var("CASTLE_CAPTURE_LIGHTING").unwrap_or_else(|_| "on".into()),
        std::env::var("CASTLE_CAPTURE_WINDOWS").unwrap_or_else(|_| "open".into())
    ));
    if let Ok(path) = std::env::var("CASTLE_CAPTURE_SNAPSHOT") {
        let name = std::path::Path::new(&path)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        manifest.push_str(&format!(
            "snapshot_file={name}\nfurniture={}\n",
            if std::env::var("CASTLE_CAPTURE_FURNITURE").as_deref() == Ok("off") {
                "off"
            } else {
                "on"
            }
        ));
    }
    if let Ok(path) = std::env::var("CASTLE_CAPTURE_TRACE") {
        let name = std::path::Path::new(&path)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        manifest.push_str(&format!("trace_file={name}\n"));
    }

    if let Some(trace) = trace {
        let directory = config.output.with_extension("frames");
        assert!(!directory.exists(), "frame directory must be fresh");
        std::fs::create_dir_all(&directory).unwrap();
        let mut direction = Vec3::NEG_Z;
        let mut previous_tick = 0;
        for (frame, index) in trace_sample_indices(trace.len()).into_iter().enumerate() {
            let eye = fixture.canonical_point(trace[index]) + Vec3::Y * EYE_HEIGHT;
            let next = fixture.canonical_point(trace[(index + 2).min(trace.len() - 1)]);
            let delta = Vec3::new(next.x - eye.x, 0.0, next.z - eye.z);
            if delta.length_squared() > 0.0001 {
                direction = delta.normalize();
            }
            *app.world_mut()
                .entity_mut(camera)
                .get_mut::<Transform>()
                .unwrap() = Transform::from_translation(eye).looking_to(direction, Vec3::Y);
            capture_image(
                &mut app,
                &target,
                &directory.join(format!("frame-{frame:05}.png")),
                config.width,
                config.height,
                Duration::from_millis(((index - previous_tick) * 50) as u64),
            );
            previous_tick = index;
        }
    } else {
        capture_image(
            &mut app,
            &target,
            &config.output,
            config.width,
            config.height,
            Duration::ZERO,
        );
    }
    manifest.push_str(&format!(
        "capture_elapsed_seconds={}\n",
        app.world().resource::<Time>().elapsed_secs_f64()
    ));
    std::fs::write(config.output.with_extension("txt"), manifest).unwrap();
}

#[derive(Resource, Default)]
struct CaptureReceipt(u64);
fn capture_received(
    _: On<bevy::render::view::screenshot::ScreenshotCaptured>,
    mut receipt: ResMut<CaptureReceipt>,
) {
    receipt.0 += 1;
}
fn capture_image(
    app: &mut App,
    target: &Handle<Image>,
    path: &std::path::Path,
    width: u32,
    height: u32,
    elapsed: Duration,
) {
    assert!(!path.exists(), "screenshot destination must be fresh");
    let before = app.world().resource::<CaptureReceipt>().0;
    // The observer outlives this borrowed path. Bind the owned path before
    // creating Bevy's callback so its closure has no borrowed lifetime.
    let screenshot_path = path.to_path_buf();
    app.world_mut()
        .spawn(bevy::render::view::screenshot::Screenshot::image(
            target.clone(),
        ))
        .observe(bevy::render::view::screenshot::save_to_disk(
            screenshot_path,
        ))
        .observe(capture_received);
    for attempt in 0..200 {
        // Advance simulation time exactly once for this sampled pose; waiting
        // for asynchronous screenshot completion must not advance it again.
        update_capture(
            app,
            if attempt == 0 {
                elapsed
            } else {
                Duration::ZERO
            },
        );
        app.sub_app(bevy::render::RenderApp)
            .world()
            .resource::<bevy::render::renderer::RenderDevice>()
            .poll(bevy::render::render_resource::PollType::wait_indefinitely())
            .unwrap();
        if app.world().resource::<CaptureReceipt>().0 > before && path.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        app.world().resource::<CaptureReceipt>().0 > before,
        "screenshot receipt was not observed"
    );
    let data = std::fs::read(path).expect("completed screenshot file");
    let image = Image::from_buffer(
        &data,
        bevy::image::ImageType::Extension("png"),
        bevy::image::CompressedImageFormats::NONE,
        true,
        bevy::image::ImageSampler::default(),
        RenderAssetUsages::MAIN_WORLD,
    )
    .expect("decodable screenshot");
    assert_eq!((image.width(), image.height()), (width, height));
}

fn read_trace(text: &str) -> Vec<[f32; 3]> {
    let mut rows = text.lines();
    assert_eq!(
        rows.next(),
        Some("tick\tlocal_feet_x\tlocal_feet_y\tlocal_feet_z")
    );
    let mut trace = Vec::new();
    for (index, line) in rows.enumerate() {
        let values: Vec<_> = line.split('\t').collect();
        assert_eq!(values.len(), 4);
        assert_eq!(
            values[0].parse::<usize>().unwrap(),
            index,
            "trace ticks must be contiguous"
        );
        let p = [
            values[1].parse::<f32>().unwrap(),
            values[2].parse::<f32>().unwrap(),
            values[3].parse::<f32>().unwrap(),
        ];
        assert!(p.iter().all(|n| n.is_finite()));
        trace.push(p);
    }
    assert!(!trace.is_empty());
    trace
}

fn trace_sample_indices(length: usize) -> Vec<usize> {
    assert!(length > 0);
    let mut indices: Vec<_> = (0..length).step_by(2).collect();
    if indices.last() != Some(&(length - 1)) {
        indices.push(length - 1);
    }
    indices
}
#[test]
fn trace_sampling_preserves_the_actual_terminal_pose() {
    assert_eq!(trace_sample_indices(1), vec![0]);
    assert_eq!(trace_sample_indices(4), vec![0, 2, 3]);
    assert_eq!(trace_sample_indices(5), vec![0, 2, 4]);
}
#[test]
fn trace_parser_requires_contiguous_finite_authoritative_ticks() {
    let header = "tick\tlocal_feet_x\tlocal_feet_y\tlocal_feet_z\n";
    assert_eq!(
        read_trace(&format!("{header}0\t1\t2\t3\n1\t2\t3\t4")),
        vec![[1., 2., 3.], [2., 3., 4.]]
    );
    for bad in ["0\tNaN\t2\t3", "1\t1\t2\t3", "0\t1\t2", ""] {
        assert!(std::panic::catch_unwind(|| read_trace(&format!("{header}{bad}"))).is_err());
    }
}

fn update_capture(app: &mut App, elapsed: Duration) {
    app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(elapsed));
    app.update();
    app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
        Duration::ZERO,
    ));
}
#[test]
fn loaded_air_chunks_remain_available_to_production_rays() {
    let fixture = CastleFixture {
        worldgen: 36,
        seed: 1,
        actual_facing: 0,
        review_turn: 0,
        scene_mode: 1,
        building_origin: [0, 0, 0],
        origin: [-32, 0, 0],
        size: [64, 32, 32],
        blocks: vec![palette::AIR; 64 * 32 * 32],
    };
    let mut store = ChunkStore::default();
    assert_eq!(fixture.place_chunks(&mut store), 2);
    for cx in [-1, 0] {
        let chunk = store
            .get(ChunkCoord { cx, cy: 0, cz: 0 })
            .expect("loaded air chunk");
        assert_eq!(chunk.block([31, 31, 31]), palette::AIR);
    }
    assert!(
        store
            .get(ChunkCoord {
                cx: 1,
                cy: 0,
                cz: 0
            })
            .is_none()
    );
}
#[test]
fn capture_elapsed_time_advances_once_per_sample_not_per_readback() {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    update_capture(&mut app, Duration::ZERO);
    for _ in 0..20 {
        update_capture(&mut app, Duration::from_millis(50));
    }
    assert_eq!(
        app.world().resource::<Time>().elapsed(),
        Duration::from_secs(1)
    );
    update_capture(&mut app, Duration::from_millis(100));
    for _ in 0..5 {
        update_capture(&mut app, Duration::ZERO);
    }
    assert_eq!(
        app.world().resource::<Time>().elapsed(),
        Duration::from_millis(1100)
    );
    update_capture(&mut app, Duration::from_millis(50));
    assert_eq!(
        app.world().resource::<Time>().elapsed(),
        Duration::from_millis(1150)
    );
}

fn capture_manifest(
    app: &App,
    fixture: &CastleFixture,
    config: &CaptureConfig,
    counts: [u64; 3],
) -> String {
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .output()
            .expect("git provenance");
        assert!(output.status.success(), "git provenance failed");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    let revision = git(&["rev-parse", "HEAD"]);
    assert!(revision.len() == 40 && revision.bytes().all(|b| b.is_ascii_hexdigit()));
    let dirty = !git(&["status", "--porcelain", "--untracked-files=normal"]).is_empty();
    let adapter = app
        .world()
        .resource::<bevy::render::renderer::RenderAdapterInfo>();
    format!(
        concat!(
            "source_commit={}\nsource_dirty={}\nscene_mode={}\nworldgen={}\nseed={}\n",
            "actual_facing={}\nreview_turn={}\nview={}\nworld_tick={}\nresolution={}x{}\n",
            "baseline_main_draws={}\nbaseline_shadow_draws={}\nbaseline_prepass_draws={}\n",
            "loaded_chunks={}\ngpu_name={}\ngpu_driver={}\ngpu_driver_info={}\ngpu_backend={:?}\n",
            "idle_warmup_seconds=1\ntrace_tick_hz=20\ntrace_stride=2\nterminal_pose=included\n"
        ),
        revision,
        dirty,
        fixture.scene_mode,
        fixture.worldgen,
        fixture.seed,
        fixture.actual_facing,
        fixture.review_turn,
        config.view,
        config.tick,
        config.width,
        config.height,
        counts[0],
        counts[1],
        counts[2],
        app.world().resource::<ChunkStore>().chunk_count(),
        adapter.name.replace(['\n', '\r'], " "),
        adapter.driver.replace(['\n', '\r'], " "),
        adapter.driver_info.replace(['\n', '\r'], " "),
        adapter.backend
    )
}

// Synchronized CPU+GPU frame latency after meshing and pipeline warmup; not GPU-only timing.
fn capture_costs(app: &mut App) -> String {
    let mut millis = Vec::with_capacity(120);
    for _ in 0..120 {
        let started = std::time::Instant::now();
        app.update();
        app.sub_app(bevy::render::RenderApp)
            .world()
            .resource::<bevy::render::renderer::RenderDevice>()
            .poll(bevy::render::render_resource::PollType::wait_indefinitely())
            .unwrap();
        millis.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    millis.sort_by(f64::total_cmp);
    let world = app.world_mut();
    let (entities, props, candles) = capture_scene_counts(world);
    let lights = world
        .query::<&PointLight>()
        .iter(world)
        .filter(|l| l.intensity > 0.0)
        .count();
    let shadowed = world
        .query::<&PointLight>()
        .iter(world)
        .filter(|l| l.intensity > 0.0 && l.shadow_maps_enabled)
        .count();
    format!(
        "frame_samples=120\nframe_metric=synchronized_cpu_gpu_ms\nframe_p50_ms={:.3}\nframe_p95_ms={:.3}\nentities={}\nstatic_prop_roots={}\nactive_point_lights={}\nshadowed_point_lights={}\ncandles={}\ncandle_fixture_roots={}\n",
        millis[59],
        millis[113],
        entities,
        props,
        lights,
        shadowed,
        if candles == 0 { "off" } else { "on" },
        candles
    )
}

fn capture_scene_counts(world: &mut World) -> (u32, usize, usize) {
    // Bevy 0.19.1 forwards entity_count to count_spawned, not allocator len.
    let entities = world.entity_count();
    let (props, candles) = world
        .query::<&static_props::StaticPropRoot>()
        .iter(world)
        .fold((0, 0), |(props, candles), root| {
            use crate::net::StaticPropKind::{FloorCandelabrum, TableCandelabrum, WallSconce};
            (
                props + 1,
                candles
                    + usize::from(matches!(
                        root.0.kind,
                        WallSconce | FloorCandelabrum | TableCandelabrum
                    )),
            )
        });
    (entities, props, candles)
}

#[test]
fn capture_counts_live_entities_and_retained_candles_not_allocator_slots() {
    use crate::net::{BlockCoord, Facing, StaticPropKind, StaticPropState};
    let mut world = World::new();
    // Bevy 0.19 stores resources as live entities, including its bootstrap filter.
    let baseline = world.entity_count();
    let root = |prop_id, kind| {
        static_props::StaticPropRoot(StaticPropState {
            prop_id,
            kind,
            origin: BlockCoord { x: 0, y: 0, z: 0 },
            facing: Facing::North,
            variant: 0,
        })
    };
    let furniture = world.spawn(root(1, StaticPropKind::Bookcase)).id();
    let candle = world.spawn(root(2, StaticPropKind::WallSconce)).id();
    let vacant = world.spawn_empty().id();
    world.despawn(vacant);
    assert!(world.entities().len() > world.entity_count());
    assert_eq!(capture_scene_counts(&mut world), (baseline + 2, 2, 1));
    world.despawn(candle);
    assert_eq!(capture_scene_counts(&mut world), (baseline + 1, 1, 0));
    world.despawn(furniture);
    assert_eq!(capture_scene_counts(&mut world), (baseline, 0, 0));
}
