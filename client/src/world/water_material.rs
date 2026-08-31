//! The material the water surface is drawn with, and the one uniform that animates it.
//!
//! ## Why an extension rather than a material
//!
//! Water was a plain `StandardMaterial` until this module existed, and everything that
//! made it look like water is still that material's: the base colour, `AlphaMode::Blend`
//! and a low `perceptual_roughness` so a lake catches a highlight. None of that is
//! reproduced here. [`FlowingWaterExtension`] is a `MaterialExtension`, so Bevy composes
//! the standard PBR fragment shader with one of ours and the base half keeps answering
//! for lighting, fog, tonemapping and alpha exactly as it did — the extension changes
//! the colour's brightness on its way in, and nothing else.
//!
//! ## Nothing is decided here
//!
//! The flow this shader slides along is a **rendering hint** derived from block ids the
//! server sent. `mesher.rs` writes it into the vertex attributes; this module hands the
//! shader a clock. No gameplay outcome depends on either, and a client that drew the
//! ripples backwards would still swim in exactly the water the server says is there.
//!
//! ## The shader is embedded, not loaded
//!
//! `embedded_asset!` compiles `flowing_water.wgsl` into the binary and registers it under
//! the `embedded://` asset source, so there is no file beside the executable, no asset
//! directory and nothing to ship. This client has no asset pipeline at all — see
//! "No texture atlas, and no art assets" in `client/AGENTS.md` — and the shader does not
//! become the first thing to need one.

use bevy::asset::embedded_asset;
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;

/// The water material: the established `StandardMaterial` with the ripple on top.
///
/// A type alias because the pair is spelled in four places — the resource, the asset
/// store, the `MeshMaterial3d` on the water child, and the plugin — and the generic
/// written out four times is four chances to write a different one.
pub type FlowingWater = ExtendedMaterial<StandardMaterial, FlowingWaterExtension>;

/// Where the embedded fragment shader lives, as the asset server addresses it.
///
/// `embedded_asset!` derives this path from the crate name and this file's location;
/// it is written out because the shader is loaded by path and a test has to be able to
/// name the same one.
pub const SHADER_PATH: &str = "embedded://voxelheim_client/world/flowing_water.wgsl";

/// The extension's one uniform: how long the session has been running.
///
/// Binding 100 because the base `StandardMaterial` owns 0-99 of the material bind
/// group; the shader declares the same number.
#[derive(Asset, AsBindGroup, Reflect, Debug, Clone, Default)]
pub struct FlowingWaterExtension {
    /// Seconds since startup, wrapped by [`advance_flow_time`].
    #[uniform(100)]
    pub time: f32,
}

impl MaterialExtension for FlowingWaterExtension {
    fn fragment_shader() -> ShaderRef {
        SHADER_PATH.into()
    }
}

/// Registers the shader, the material and the clock that drives it.
///
/// Separate from `ChunkRenderPlugin` so the whole of "what water looks like" is one
/// module: the renderer asks for a handle and places an entity, and never learns what
/// the material is made of.
pub struct FlowingWaterPlugin;

impl Plugin for FlowingWaterPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "flowing_water.wgsl");
        app.add_plugins(MaterialPlugin::<FlowingWater>::default())
            .add_systems(Update, advance_flow_time);
    }
}

/// Hands the shader the current time, once per frame.
///
/// **Wrapped rather than raw.** `elapsed_secs` grows without bound and is an `f32`: after
/// a few hours its steps are coarser than a frame, and the ripple would visibly stutter
/// and then stop. `elapsed_secs_wrapped` returns the same clock modulo `Time`'s wrap
/// period, which is exactly the fix — the pattern is a sine of the value, so a wrap is
/// invisible as long as the period is long compared to a frame.
///
/// Every material in the store is written, and there is one. Writing through `iter_mut`
/// is what marks the asset changed, which is what re-uploads the uniform.
fn advance_flow_time(time: Res<Time>, mut materials: ResMut<Assets<FlowingWater>>) {
    let elapsed = time.elapsed_secs_wrapped();
    for (_, material) in materials.iter_mut() {
        material.extension.time = elapsed;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bevy::asset::AssetPlugin;
    use bevy::shader::{Shader, ShaderImport, ShaderLoader};

    use super::*;

    /// The shader source as it is compiled into the binary. The same bytes
    /// `embedded_asset!` registers, read here so the assertions below are about the
    /// file the client actually ships.
    const SOURCE: &str = include_str!("flowing_water.wgsl");

    /// How long a test will pump the app waiting for the embedded shader to load.
    /// Generous because the asset pipeline runs on a task pool, and irrelevant to
    /// runtime because it is reached in a frame or two.
    const PATIENCE: Duration = Duration::from_secs(30);

    /// The plugin on a headless app, with the two registrations `RenderPlugin` would
    /// otherwise make. `Shader` is a plain asset — the loader parses WGSL and resolves
    /// imports with no device anywhere — so everything short of pipeline compilation
    /// is exercised with no display and no GPU. CI has neither.
    fn headless() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Shader>()
            .init_asset_loader::<ShaderLoader>()
            .add_plugins(FlowingWaterPlugin);
        app
    }

    fn pump_until(app: &mut App, what: &str, done: impl Fn(&App) -> bool) {
        let deadline = std::time::Instant::now() + PATIENCE;
        while std::time::Instant::now() < deadline {
            app.update();
            if done(app) {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("timed out waiting for {what}");
    }

    #[test]
    fn the_embedded_shader_loads_at_the_path_the_material_names() {
        // Three claims at once, and each fails the same silent way on its own: the
        // extension asks for a path, `embedded_asset!` registers the file at exactly
        // that path, and the WGSL loader accepts what it finds there. A miss anywhere
        // means Bevy falls back to the base material's fragment shader, which looks
        // like water that has stopped moving rather than like an error.
        let ShaderRef::Path(named) = FlowingWaterExtension::fragment_shader() else {
            panic!("the extension must name a shader path");
        };
        assert_eq!(named.to_string(), SHADER_PATH);

        let mut app = headless();
        let handle: Handle<Shader> = app.world().resource::<AssetServer>().load(SHADER_PATH);
        pump_until(&mut app, "the embedded shader", |app| {
            app.world()
                .resource::<Assets<Shader>>()
                .get(&handle)
                .is_some()
        });

        let shaders = app.world().resource::<Assets<Shader>>();
        let shader = shaders.get(&handle).expect("the embedded shader");
        // The preprocessor read the file: these are the bevy_pbr modules the fragment
        // function is composed from, and losing one is how the shader stops compiling
        // in a build that has a GPU.
        let imports: Vec<String> = shader
            .imports
            .iter()
            .map(|import| match import {
                ShaderImport::Custom(name) => name.clone(),
                ShaderImport::AssetPath(path) => path.clone(),
            })
            .collect();
        for wanted in [
            "bevy_pbr::forward_io",
            "bevy_pbr::pbr_fragment",
            "bevy_pbr::pbr_functions",
            "bevy_pbr::pbr_types",
        ] {
            assert!(
                imports.iter().any(|import| import.starts_with(wanted)),
                "the shader no longer imports {wanted}; it imports {imports:?}"
            );
        }
    }

    #[test]
    fn the_shader_declares_the_uniform_the_extension_binds() {
        // `AsBindGroup` puts the uniform at 100 and the WGSL has to agree; nothing but
        // this pins the two numbers together, because a mismatch is a pipeline error at
        // draw time and there is no GPU in this suite to raise one.
        assert!(
            SOURCE.contains("@binding(100) var<uniform> flowing_water: FlowingWater;"),
            "the shader does not bind the extension's uniform at 100"
        );
        assert!(
            SOURCE.contains("@group(#{MATERIAL_BIND_GROUP})"),
            "the uniform must sit in the material bind group the pipeline substitutes"
        );
    }

    #[test]
    fn the_shader_reads_both_flow_attributes_the_mesher_writes() {
        // The seam with `mesher.rs`: UV_0 carries the horizontal flow and UV_1 the
        // falling bit, and each is guarded by the shader def the mesh layout produces.
        for guard in ["#ifdef VERTEX_UVS_A", "#ifdef VERTEX_UVS_B"] {
            assert!(SOURCE.contains(guard), "the shader must guard on {guard}");
        }
        assert!(SOURCE.contains("flow = in.uv;"));
        assert!(SOURCE.contains("falling = in.uv_b.x;"));
    }

    /// Reads a `const <name>: f32 = <value>;` out of the shader source.
    ///
    /// The shader is the one declaration of these numbers and this reads it rather than
    /// restating it — a second copy in Rust would be a second answer, and the whole
    /// reason `SOURCE` is included here is that there is only ever one.
    fn shader_const(name: &str) -> f32 {
        let needle = format!("const {name}: f32 = ");
        let start = SOURCE
            .find(&needle)
            .unwrap_or_else(|| panic!("the shader declares no {name}"))
            + needle.len();
        let rest = &SOURCE[start..];
        let end = rest.find(';').expect("unterminated const");
        rest[..end]
            .trim()
            .parse()
            .unwrap_or_else(|error| panic!("{name} is not a float: {error}"))
    }

    /// **This test used to pin `RIPPLE_DEPTH` at 0.08, and 0.08 was the defect.**
    ///
    /// #598 set one amplitude for all three waters and argued it as a ceiling: visible
    /// motion, and still the same colour as before it moved. On a translucent blue
    /// surface at play distance it was not visible at all, and a river was
    /// indistinguishable from a lake — which is what #655 reported. So the ceiling is
    /// still a ceiling; what changed is that there are three numbers under it and they
    /// must be ordered.
    ///
    /// **The order is the claim, not the values.** Still water is the quiet one, a
    /// current is louder, a fall is loudest — that ordering is what makes the three
    /// readable as three things, and it is what a later retune must not accidentally
    /// invert. The bound above it keeps a retune from answering "make it visible" with
    /// a surface that is no longer water.
    ///
    /// **The bound is a bound on the rendered push only because `ripple` is normalised**,
    /// and that is the conditional this test does not itself carry: the shader computes
    /// `1.0 + depth * ripple(point)`, so a depth bounds the brightness shift exactly when
    /// |ripple| <= 1. It does — the divisor on `ripple`'s last line proves it — and
    /// [`the_ripple_is_normalised_and_the_depths_depend_on_it`] is what keeps that true,
    /// which is why the two tests are worth reading together.
    #[test]
    fn the_three_waters_are_ordered_and_bounded() {
        let still = shader_const("STILL_DEPTH");
        let running = shader_const("RUN_DEPTH");
        let falling = shader_const("FALL_DEPTH");
        assert!(
            still < running && running < falling,
            "the three ripple depths must rise from still to falling, got {still} / {running} / {falling}"
        );
        assert!(
            falling <= 0.5,
            "the loudest water may not push its own colour by more than half, got {falling}"
        );
        assert!(
            still <= 0.08,
            "still water must stay at or under the 0.08 #598 argued for, got {still}"
        );
    }

    /// The one thing every depth constant above silently depends on: `ripple` returns a
    /// value in [-1, 1], so a depth *is* the brightness shift rather than merely scaling
    /// an unknown.
    ///
    /// The proof is arithmetic and lives in the shader: a product of two sines is at most
    /// one, a sine is at most one, so the weighted sum is at most the sum of the weights —
    /// and the last line divides by exactly that. **What this test guards is the divisor.**
    /// Adding a third octave without adding its weight there makes `ripple` return more
    /// than one, and `FALL_DEPTH` alone would then drive brightness negative. That is a
    /// silent break: the surface would still render, just wrongly, on the one code path
    /// CI has no device to exercise.
    ///
    /// Read out of the source rather than restated, like every other number here. The
    /// shader is not clamped on purpose — a clamp would keep a broken `ripple` looking
    /// plausible, which is the opposite of what this repository wants from a broken
    /// invariant.
    #[test]
    fn the_ripple_is_normalised_and_the_depths_depend_on_it() {
        let weight = shader_const("OCTAVE_WEIGHT");
        assert!(
            (0.0..1.0).contains(&weight),
            "the second octave must weigh less than the first, got {weight}"
        );
        // Every octave in the numerator must appear in the divisor. Two octaves today:
        // the unit-weight coarse term and OCTAVE_WEIGHT's fine one.
        assert!(
            SOURCE.contains("(coarse + fine * OCTAVE_WEIGHT) / (1.0 + OCTAVE_WEIGHT)"),
            "ripple must divide by the sum of its octave weights, or it is no longer \
             bounded by one and every depth constant above it stops being a bound"
        );
        // And exactly two octaves: a third term would need its weight in that divisor,
        // and this is what notices one arriving without it.
        assert_eq!(
            SOURCE.matches("let coarse").count() + SOURCE.matches("let fine").count(),
            2,
            "ripple gained or lost an octave; its divisor and the depth constants both \
             have to move with it"
        );
    }

    /// The half that is not brightness, which the issue asks for by name.
    ///
    /// A crest pulled toward white is foam; a crest that is only brighter is the same
    /// surface under a stronger lamp. Still water has none of it, and that absence is
    /// itself one of the three differences — a lake does not foam.
    #[test]
    fn only_moving_water_foams_and_a_fall_foams_most() {
        let running = shader_const("RUN_FOAM");
        let falling = shader_const("FALL_FOAM");
        assert!(
            running > 0.0 && falling > running,
            "foam must rise from a current to a fall, got {running} / {falling}"
        );
        assert!(
            falling < 1.0,
            "foam may not replace the water's colour outright, got {falling}"
        );
        assert!(
            !SOURCE.contains("STILL_FOAM"),
            "still water must have no foam constant at all; its absence is the rule"
        );
        assert!(
            SOURCE.contains("foam = 0.0;"),
            "the still branch must set foam to zero explicitly"
        );
    }

    /// The shape of the pattern, which is the difference that actually carries the three
    /// apart: swell, streaks along a current, and columns down a fall.
    ///
    /// Both stretches are bounded above for a reason worth keeping: past about five the
    /// pattern stops varying along the direction it travels, and a band that does not
    /// vary cannot be seen to move — so an over-stretched streak is a *less* legible
    /// current, not a more legible one.
    #[test]
    fn moving_water_is_stretched_along_the_way_it_moves() {
        for name in ["RUN_STRETCH", "FALL_STRETCH"] {
            let stretch = shader_const(name);
            assert!(
                stretch > 1.0 && stretch <= 5.0,
                "{name} must stretch the pattern without flattening it, got {stretch}"
            );
        }
    }

    #[test]
    fn the_time_uniform_advances_with_the_clock() {
        let mut app = headless();
        let handle = app
            .world_mut()
            .resource_mut::<Assets<FlowingWater>>()
            .add(FlowingWater {
                base: StandardMaterial::default(),
                extension: FlowingWaterExtension::default(),
            });

        let at = |app: &App| {
            app.world()
                .resource::<Assets<FlowingWater>>()
                .get(&handle)
                .expect("the material")
                .extension
                .time
        };

        app.update();
        let first = at(&app);
        // Several frames with a real pause between them: the first frame's delta can be
        // zero on a fast machine, and a clock that never moved would pass on one update.
        for _ in 0..8 {
            std::thread::sleep(Duration::from_millis(2));
            app.update();
        }
        let last = at(&app);
        assert!(
            last > first,
            "the shader's clock did not move: {first} then {last}"
        );
    }

    /// What nothing else here can check: that the shader **compiles**.
    ///
    /// Everything above tests the shader as bytes and as a path. Composition — naga_oil
    /// splicing the `bevy_pbr` modules in and naga validating the result — happens in
    /// `ShaderCache`, which is only ever reached from a `RenderDevice`. So this one
    /// builds the real thing: `DefaultPlugins` without a window, a camera rendering into
    /// an `Image`, one water quad carrying both flow attributes, and then every entry of
    /// the render app's `PipelineCache` read back for an `Err`.
    ///
    /// **`#[ignore]`, because CI has no render device.** The `client` job installs
    /// `libasound2-dev libudev-dev pkg-config` and nothing else, so there is no adapter
    /// for wgpu to open and no software one either; the render sub-app is never created
    /// and this test could only ever report the absence of a GPU. It is kept rather than
    /// deleted because it is the only thing in this repository that can answer the
    /// question, it runs in ten seconds on a workstation, and it was verified to
    /// **fail** on a deliberately broken shader before it was trusted to pass on a
    /// working one. Run it by hand after touching the WGSL:
    ///
    /// ```text
    /// cargo test -p voxelheim-client -- --ignored --nocapture --test-threads=1 \
    ///     the_shader_compiles_through_the_real_pipeline
    /// ```
    ///
    /// Two plugins are disabled deliberately. `WinitPlugin` would want a display;
    /// `PipelinedRenderingPlugin` moves the render sub-app out of the `App` between
    /// frames, which is what makes `get_sub_app` answer `None` on a machine that does
    /// have a GPU.
    #[test]
    #[ignore = "needs a render device; CI has no GPU and no software adapter"]
    fn the_shader_compiles_through_the_real_pipeline() {
        use bevy::render::RenderApp;
        use bevy::render::render_resource::{CachedPipelineState, PipelineCache};
        use bevy::window::{ExitCondition, WindowPlugin};

        let mut app = App::new();
        app.add_plugins(
            DefaultPlugins
                .build()
                .disable::<bevy::winit::WinitPlugin>()
                .disable::<bevy::render::pipelined_rendering::PipelinedRenderingPlugin>()
                .set(WindowPlugin {
                    primary_window: None,
                    exit_condition: ExitCondition::DontExit,
                    close_when_requested: false,
                    ..default()
                }),
        )
        .add_plugins(FlowingWaterPlugin)
        .add_systems(Startup, one_water_quad_in_front_of_a_camera);

        // `App::update` on its own skips `finish`/`cleanup`, and the render device is
        // created between them: without this the first frame runs `bevy_pbr` systems
        // that ask for a `RenderDevice` nobody has inserted yet.
        while app.plugins_state() != bevy::app::PluginsState::Ready {
            std::thread::sleep(Duration::from_millis(10));
        }
        app.finish();
        app.cleanup();

        for _ in 0..240 {
            app.update();
            std::thread::sleep(Duration::from_millis(10));
        }

        let render_app = app
            .get_sub_app(RenderApp)
            .expect("no render sub-app: wgpu opened no adapter on this machine");
        let cache = render_app.world().resource::<PipelineCache>();
        let mut specialized = 0;
        for pipeline in cache.pipelines() {
            specialized += 1;
            if let CachedPipelineState::Err(error) = &pipeline.state {
                panic!("a pipeline failed to compile: {error}");
            }
        }
        assert!(
            specialized > 0,
            "nothing was specialized, so nothing was compiled and this proved nothing"
        );
    }

    /// The smallest scene that makes Bevy specialize the water material: a lit quad with
    /// both flow attributes, seen by a camera that renders into an image rather than a
    /// window.
    fn one_water_quad_in_front_of_a_camera(
        mut commands: Commands,
        mut meshes: ResMut<Assets<Mesh>>,
        mut images: ResMut<Assets<bevy::image::Image>>,
        mut water: ResMut<Assets<FlowingWater>>,
    ) {
        use bevy::asset::RenderAssetUsages;
        use bevy::camera::RenderTarget;
        use bevy::core_pipeline::tonemapping::Tonemapping;
        use bevy::image::Image;
        use bevy::mesh::{Indices, PrimitiveTopology};
        use bevy::render::render_resource::{
            Extent3d, TextureDimension, TextureFormat, TextureUsages,
        };

        let mut image = Image::new_fill(
            Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            &[0, 0, 0, 255],
            TextureFormat::Bgra8UnormSrgb,
            RenderAssetUsages::default(),
        );
        image.texture_descriptor.usage = TextureUsages::COPY_SRC
            | TextureUsages::RENDER_ATTACHMENT
            | TextureUsages::TEXTURE_BINDING;
        let target = images.add(image);

        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_POSITION,
            vec![
                [-1.0, 0.0, -1.0],
                [1.0, 0.0, -1.0],
                [1.0, 0.0, 1.0],
                [-1.0, 0.0, 1.0f32],
            ],
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0f32]; 4]);
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[0.01, 0.07, 0.26, 0.62f32]; 4]);
        // A full-strength +x flow, and falling: the two branches the fragment function
        // takes, so a mistake in either is compiled rather than skipped.
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[1.0, 0.0f32]; 4]);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, vec![[1.0, 0.0f32]; 4]);
        mesh.insert_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3]));

        commands.spawn((
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(water.add(FlowingWater {
                base: StandardMaterial {
                    base_color: Color::WHITE,
                    alpha_mode: AlphaMode::Blend,
                    perceptual_roughness: 0.15,
                    ..default()
                },
                extension: default(),
            })),
            Transform::default(),
        ));
        commands.spawn((
            DirectionalLight::default(),
            Transform::from_xyz(1.0, 4.0, 1.0).looking_at(Vec3::ZERO, Vec3::Y),
        ));
        // `AcesFitted` for the reason `player/camera.rs` uses it: the default tonemapper
        // wants a KTX2 LUT this client deliberately does not ship.
        commands.spawn((
            Camera3d::default(),
            Tonemapping::AcesFitted,
            RenderTarget::Image(target.into()),
            Transform::from_xyz(0.0, 3.0, 4.0).looking_at(Vec3::ZERO, Vec3::Y),
        ));
    }
}
