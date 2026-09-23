//! Opt-in production captures of the first dungeon's zones (#1298); no runtime wiring.
//!
//! The castle capture's harness, pointed at the dungeon: the production `WorldPlugin` and
//! chunk mesher, terrain material, sky, AcesFitted tonemapping and camera field of view, and
//! the dungeon's own light — the wall sconces the server places, lit by `castle_lighting`'s
//! bounded pool from a snapshot the server encoded, and the cave's grade from `cave_light`,
//! which reads the instance's world id. Nothing is brightened for the camera: a dark cave
//! photographs dark.
//!
//! The voxels are the server's gated instance cache, exported by
//! `server/internal/world/dungeon_capture_test.go`; the sconces by
//! `server/internal/game/dungeon_capture_props_test.go`. Camera ids are written in the
//! unrotated drawing's frame (`schematic_instance.go`, `schematic_instance_lower.go`), so a
//! capture needs a seed whose dungeon is unturned; seed 0 is one.
use super::*;
use crate::player::dungeon_capture_fixture::DungeonFixture;

/// Floor 1's standing level and the lower zones', copied from the drawing's constants.
const UPPER: f32 = 51.0;
const SHORE: f32 = 18.0;
const SAND: f32 = 10.0;
const KING: f32 = 1.0;

/// One fixed review camera: its id, the fixture state it is meant for, eye and target in
/// the drawing's frame.
struct View {
    id: &'static str,
    opened: bool,
    eye: [f32; 3],
    target: [f32; 3],
}

const fn view(id: &'static str, opened: bool, eye: [f32; 3], target: [f32; 3]) -> View {
    View {
        id,
        opened,
        eye,
        target,
    }
}

/// Every zone of the route, in route order. Interior views stand at a player's eye height
/// on the zone's floor; the chasm view looks down the opened trapdoor from the arena floor.
const VIEWS: [View; 14] = [
    view(
        "arrival_court",
        false,
        [17.5, UPPER + EYE_HEIGHT, 2.5],
        [17.5, UPPER + 1.2, 9.5],
    ),
    view(
        "draugr_hall",
        false,
        [17.5, UPPER + EYE_HEIGHT, 13.5],
        [17.5, UPPER + 1.5, 31.0],
    ),
    view(
        "vargr_hall",
        false,
        [17.5, UPPER + EYE_HEIGHT, 33.5],
        [17.5, UPPER + 1.5, 51.0],
    ),
    view(
        "rune_hall",
        false,
        [17.5, UPPER + EYE_HEIGHT, 59.5],
        [17.5, UPPER + 2.0, 73.0],
    ),
    view(
        "guardian_arena",
        true,
        [17.5, UPPER + EYE_HEIGHT, 74.5],
        [17.5, UPPER + 1.0, 100.0],
    ),
    view(
        "chasm",
        true,
        [17.5, UPPER + EYE_HEIGHT, 93.2],
        [17.5, 25.0, 97.5],
    ),
    view(
        "pool_shore",
        true,
        [17.5, SHORE + EYE_HEIGHT, 89.0],
        [17.5, SHORE + 1.0, 104.0],
    ),
    view(
        "cave_cavern",
        true,
        [17.5, SHORE + EYE_HEIGHT, 84.5],
        [17.5, SHORE + 1.5, 62.0],
    ),
    view(
        "web_curtain",
        false,
        [17.5, SHORE + EYE_HEIGHT, 62.5],
        [17.5, SHORE + 1.2, 56.0],
    ),
    view(
        "gallery_grille",
        false,
        [20.5, SHORE + EYE_HEIGHT, 53.0],
        [5.5, SHORE + 1.2, 49.0],
    ),
    view(
        "sand_hall",
        true,
        [17.5, SAND + EYE_HEIGHT + 2.0, 36.5],
        [17.5, SAND + 0.5, 10.0],
    ),
    view(
        "sand_twin_lever",
        false,
        [17.5, SAND + EYE_HEIGHT, 20.5],
        [0.5, SAND + 1.5, 20.5],
    ),
    view(
        "king_arena",
        true,
        [29.5, KING + EYE_HEIGHT, 51.0],
        [17.5, KING + 1.5, 67.5],
    ),
    view(
        "return_shortcut",
        true,
        [31.9, KING + EYE_HEIGHT, 80.5],
        [31.9, KING + 8.0, 100.0],
    ),
];

fn dungeon_app(
    fixture: &DungeonFixture,
    eye: Vec3,
    target: Vec3,
    tick: u64,
) -> (App, Handle<Image>, usize, usize) {
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
        spawn: fixture.drawing_point([17.5, UPPER, 3.5]).to_array(),
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
    // An instance, not the open world: the cave's grade reads this id.
    .insert_resource(crate::world::transition::CurrentWorld {
        id: 1,
        seed: fixture.seed,
        exit_arch: None,
        loading: false,
    })
    .insert_resource(InputMode::Playing)
    .init_resource::<CaptureReceipt>()
    .init_resource::<SnapshotBuffer>()
    .init_resource::<Weather>()
    .init_resource::<sky::SkyClock>()
    .add_plugins(WorldPlugin)
    .add_systems(Startup, (sky::spawn_sun, sky::spawn_sky))
    .add_systems(Update, (sky::drive_the_sky, sky::follow_the_eye).chain());
    static_props::register(&mut app);
    castle_lighting::register(&mut app);
    let path = std::env::var("DUNGEON_CAPTURE_SNAPSHOT").expect("server sconce snapshot");
    let bytes = std::fs::read(path).expect("server sconce snapshot");
    let crate::net::CaptureMessage::Snapshot(snapshot) =
        crate::net::decode_for_capture(&bytes).expect("production snapshot decoder")
    else {
        panic!("expected snapshot envelope")
    };
    let sconces = snapshot.static_props.len();
    assert!(sconces > 0, "the dungeon's sconces are missing");
    assert!(
        app.world_mut()
            .resource_mut::<SnapshotBuffer>()
            .accept(snapshot, std::time::Instant::now())
    );
    while app.plugins_state() != bevy::app::PluginsState::Ready {
        std::thread::sleep(Duration::from_millis(10));
    }
    app.finish();
    app.cleanup();
    draw_counts::install(app.sub_app_mut(bevy::render::RenderApp).world_mut());
    app.world_mut()
        .resource_mut::<sky::SkyClock>()
        .freeze_for_capture(tick);
    app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
        Duration::ZERO,
    ));
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
    image.texture_descriptor.usage |=
        TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC | TextureUsages::TEXTURE_BINDING;
    let target_image = app.world_mut().resource_mut::<Assets<Image>>().add(image);
    let fixed = Daylight::FIXED;
    app.world_mut().spawn((
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
        RenderTarget::Image(target_image.clone().into()),
        Transform::from_translation(eye).looking_at(target, Vec3::Y),
    ));
    let chunks = fixture.place_chunks(&mut app.world_mut().resource_mut::<ChunkStore>());
    (app, target_image, chunks, sconces)
}

#[test]
#[ignore = "requires an actual GPU and DUNGEON_CAPTURE_FIXTURE/SNAPSHOT/OUTPUT"]
fn capture_dungeon_production_zone() {
    let _capture = draw_counts::acquire();
    let data =
        std::fs::read(std::env::var("DUNGEON_CAPTURE_FIXTURE").expect("fixture path")).unwrap();
    let fixture = DungeonFixture::parse(
        &data,
        &[&[palette::AIR][..], &palette::PALETTE[..]].concat(),
    )
    .expect("validated server-authored fixture");
    let id = std::env::var("DUNGEON_CAPTURE_VIEW").expect("a fixed camera id");
    let view = VIEWS
        .iter()
        .find(|v| v.id == id)
        .expect("unknown fixed camera id");
    assert_eq!(
        fixture.opened,
        view.opened,
        "this view is reviewed with the doors {}",
        if view.opened { "opened" } else { "as drawn" }
    );
    let output: std::path::PathBuf = std::env::var("DUNGEON_CAPTURE_OUTPUT")
        .expect("output path")
        .into();
    assert!(!output.exists(), "capture destination must be fresh");
    let tick = std::env::var("DUNGEON_CAPTURE_TICK")
        .map(|v| v.parse().expect("fixed tick"))
        .unwrap_or(6000);
    let eye = fixture.drawing_point(view.eye);
    let (mut app, target, chunks, sconces) =
        dungeon_app(&fixture, eye, fixture.drawing_point(view.target), tick);
    let mut drained = 0;
    for frame in 0..3000 {
        app.update();
        std::thread::sleep(Duration::from_millis(10));
        let stats = *app.world().resource::<MeshStats>();
        if frame >= 200
            && stats.chunks_held == chunks
            && stats.meshed_chunks > 0
            && stats.in_flight == 0
            && stats.queued == 0
        {
            drained += 1;
            if drained >= 10 {
                break;
            }
        } else {
            drained = 0;
        }
    }
    assert!(
        drained >= 10,
        "the dungeon never completed production meshing"
    );
    // One second of production time once meshed: the sconces' flames, the light pool's
    // selection and the cave grade's ease all settle on the view.
    for _ in 0..20 {
        update_capture(&mut app, Duration::from_millis(50));
    }
    draw_counts::take();
    capture_image(&mut app, &target, &output, 1280, 720, Duration::ZERO);
    let counts = draw_counts::take();
    assert!(counts[0] > 0, "production scene issued no main mesh draws");
    let world = app.world_mut();
    let lights = world
        .query::<&PointLight>()
        .iter(world)
        .filter(|l| l.intensity > 0.0)
        .count();
    let adapter = app
        .world()
        .resource::<bevy::render::renderer::RenderAdapterInfo>();
    let manifest = format!(
        concat!(
            "view={}\nstate={}\nseed={}\nfacing={}\nworldgen={}\nworld_tick={}\nresolution=1280x720\n",
            "eye_drawing={:?}\ntarget_drawing={:?}\nloaded_chunks={}\nsconces={}\n",
            "active_point_lights={}\nbaseline_main_draws={}\ngpu_name={}\ngpu_driver={}\n",
            "gpu_driver_info={}\ngpu_backend={:?}\ntonemapping=AcesFitted\n"
        ),
        view.id,
        if fixture.opened { "opened" } else { "drawn" },
        fixture.seed,
        fixture.facing,
        fixture.worldgen,
        tick,
        view.eye,
        view.target,
        chunks,
        sconces,
        lights,
        counts[0],
        adapter.name.replace(['\n', '\r'], " "),
        adapter.driver.replace(['\n', '\r'], " "),
        adapter.driver_info.replace(['\n', '\r'], " "),
        adapter.backend,
    );
    std::fs::write(output.with_extension("txt"), manifest).unwrap();
}

#[test]
fn every_dungeon_view_is_a_distinct_standing_eye_inside_the_drawing() {
    let mut ids = std::collections::BTreeSet::new();
    for view in &VIEWS {
        assert!(ids.insert(view.id), "duplicate view {}", view.id);
        for point in [view.eye, view.target] {
            assert!((0.0..35.0).contains(&point[0]) && (0.0..106.0).contains(&point[2]));
            assert!((0.0..60.0).contains(&point[1]));
        }
        assert_ne!(view.eye, view.target);
    }
}
