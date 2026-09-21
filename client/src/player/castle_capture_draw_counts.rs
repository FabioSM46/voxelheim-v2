//! Capture-only measurement of actual mesh draw API submissions, after batching.
//!
//! Bevy 0.19.1 DrawMesh issues exactly one draw or multi-draw call on Success;
//! every early-return path is Skip. Delegate it unchanged and count only that result.
//! Multi-draw is one API submission; GPU-expanded indirect draws and fullscreen
//! postprocessing are deliberately not represented by these counters.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

static CAPTURE_LOCK: Mutex<()> = Mutex::new(());

/// Hold through the complete capture app lifetime; parallel ignored tests must not mix counts.
pub(super) fn acquire() -> MutexGuard<'static, ()> {
    CAPTURE_LOCK.lock().expect("capture lock")
}

use bevy::ecs::{query::ROQueryItem, system::SystemParamItem};
use bevy::pbr::*;
use bevy::prelude::World;
use bevy::render::render_phase::{
    DrawFunctions, PhaseItem, RenderCommand, RenderCommandResult, RenderCommandState,
    SetItemPipeline, TrackedRenderPass,
};

static MAIN: AtomicU64 = AtomicU64::new(0);
static SHADOW: AtomicU64 = AtomicU64::new(0);
static PREPASS: AtomicU64 = AtomicU64::new(0);

struct CountedMesh<const PASS: u8>;
impl<P: PhaseItem, const PASS: u8> RenderCommand<P> for CountedMesh<PASS> {
    type Param = <DrawMesh as RenderCommand<P>>::Param;
    type ViewQuery = <DrawMesh as RenderCommand<P>>::ViewQuery;
    type ItemQuery = <DrawMesh as RenderCommand<P>>::ItemQuery;

    fn render<'w>(
        item: &P,
        view: ROQueryItem<Self::ViewQuery>,
        entity: Option<ROQueryItem<Self::ItemQuery>>,
        param: SystemParamItem<'w, '_, Self::Param>,
        pass: &mut TrackedRenderPass<'w>,
    ) -> RenderCommandResult {
        let result = <DrawMesh as RenderCommand<P>>::render(item, view, entity, param, pass);
        if matches!(result, RenderCommandResult::Success) {
            match PASS {
                0 => &MAIN,
                1 => &SHADOW,
                _ => &PREPASS,
            }
            .fetch_add(1, Ordering::Relaxed);
        }
        result
    }
}

type CountedMaterial = (
    SetItemPipeline,
    SetMeshViewBindGroup<0>,
    SetMeshViewBindingArrayBindGroup<1>,
    SetMeshBindGroup<2>,
    SetMaterialBindGroup<3>,
    CountedMesh<0>,
);
type CountedPrepass<const PASS: u8> = (
    SetItemPipeline,
    SetPrepassViewBindGroup<0>,
    SetPrepassViewEmptyBindGroup<1>,
    SetMeshBindGroup<2>,
    SetMaterialBindGroup<3>,
    CountedMesh<PASS>,
);
type CountedDepthOnly<const PASS: u8> = (
    SetItemPipeline,
    SetPrepassViewBindGroup<0>,
    SetPrepassViewEmptyBindGroup<1>,
    SetMeshBindGroup<2>,
    SetPrepassEmptyMaterialBindGroup<3>,
    CountedMesh<PASS>,
);

fn replace<P, Original, Counted>(world: &mut World)
where
    P: PhaseItem,
    Original: 'static,
    Counted: RenderCommand<P> + Send + Sync + 'static,
    Counted::Param: bevy::ecs::system::ReadOnlySystemParam,
{
    let draw = RenderCommandState::<P, Counted>::new(world);
    world
        .resource::<DrawFunctions<P>>()
        .write()
        .add_with::<Original, _>(draw);
}

/// Call after plugin initialization, before the first material extraction/queued frame.
/// Capture apps are serialized; these counters are reset only after GPU completion.
pub(super) fn install(world: &mut World) {
    use bevy::core_pipeline::core_3d::{AlphaMask3d, Opaque3d, Transparent3d};
    use bevy::core_pipeline::prepass::{AlphaMask3dPrepass, Opaque3dPrepass};
    replace::<Opaque3d, DrawMaterial, CountedMaterial>(world);
    replace::<AlphaMask3d, DrawMaterial, CountedMaterial>(world);
    replace::<Transparent3d, DrawMaterial, CountedMaterial>(world);
    replace::<Transmissive3d, DrawMaterial, CountedMaterial>(world);
    replace::<Shadow, DrawPrepass, CountedPrepass<1>>(world);
    replace::<Shadow, DrawDepthOnlyPrepass, CountedDepthOnly<1>>(world);
    replace::<Opaque3dPrepass, DrawPrepass, CountedPrepass<2>>(world);
    replace::<Opaque3dPrepass, DrawDepthOnlyPrepass, CountedDepthOnly<2>>(world);
    replace::<AlphaMask3dPrepass, DrawPrepass, CountedPrepass<2>>(world);
}

pub(super) fn take() -> [u64; 3] {
    [
        MAIN.swap(0, Ordering::Relaxed),
        SHADOW.swap(0, Ordering::Relaxed),
        PREPASS.swap(0, Ordering::Relaxed),
    ]
}

#[cfg(test)]
mod tests {
    use bevy::camera::RenderTarget;
    use bevy::prelude::*;
    use bevy::render::render_resource::{
        Extent3d, PollType, TextureDimension, TextureFormat, TextureUsages,
    };

    #[test]
    #[ignore = "requires a render adapter; calibrates real mesh draw API submission counts"]
    fn calibrate_draw_submission_counts() {
        let _capture = super::acquire();
        let mut app = App::new();
        app.add_plugins(
            DefaultPlugins
                .build()
                .disable::<bevy::winit::WinitPlugin>()
                .disable::<bevy::render::pipelined_rendering::PipelinedRenderingPlugin>()
                .set(WindowPlugin {
                    primary_window: None,
                    exit_condition: bevy::window::ExitCondition::DontExit,
                    ..default()
                }),
        );
        app.finish();
        app.cleanup();
        let mut target = Image::new_fill(
            Extent3d {
                width: 256,
                height: 256,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            &[0, 0, 0, 255],
            TextureFormat::Rgba8UnormSrgb,
            default(),
        );
        target.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING
            | TextureUsages::COPY_SRC
            | TextureUsages::RENDER_ATTACHMENT;
        let target = app.world_mut().resource_mut::<Assets<Image>>().add(target);
        app.world_mut().spawn((
            Camera3d::default(),
            bevy::core_pipeline::tonemapping::Tonemapping::Reinhard,
            Camera { ..default() },
            RenderTarget::Image(target.into()),
            Transform::from_xyz(3.0, 3.0, 6.0).looking_at(Vec3::ZERO, Vec3::Y),
        ));
        let mesh = app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(Cuboid::new(1.0, 1.0, 1.0));
        let material = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(Color::WHITE);
        let cube = app
            .world_mut()
            .spawn((
                Mesh3d(mesh.clone()),
                MeshMaterial3d(material.clone()),
                Transform::default(),
            ))
            .id();
        let sun = app
            .world_mut()
            .spawn((
                DirectionalLight {
                    shadow_maps_enabled: false,
                    ..default()
                },
                bevy::light::CascadeShadowConfigBuilder {
                    num_cascades: 1,
                    maximum_distance: 30.0,
                    ..default()
                }
                .build(),
                Transform::from_xyz(3., 5., 3.).looking_at(Vec3::ZERO, Vec3::Y),
            ))
            .id();
        let render = app.get_sub_app_mut(bevy::render::RenderApp).unwrap();
        super::install(render.world_mut());
        let device = render
            .world()
            .resource::<bevy::render::renderer::RenderDevice>()
            .clone();
        let sample = |app: &mut App, label: &str| {
            for _ in 0..200 {
                app.update();
                device.poll(PollType::wait_indefinitely()).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            super::take();
            app.update();
            device.poll(PollType::wait_indefinitely()).unwrap();
            let count = super::take();
            println!("{label}: {count:?}");
            count
        };
        let base = sample(&mut app, "one cube shadows off");
        assert_eq!(base, [1, 0, 0]);
        app.world_mut()
            .entity_mut(sun)
            .get_mut::<DirectionalLight>()
            .unwrap()
            .shadow_maps_enabled = true;
        let shadow = sample(&mut app, "one cube shadows on");
        assert_eq!(shadow[0], 1);
        assert!(shadow[1] > 0);
        app.world_mut().spawn((
            Mesh3d(mesh),
            MeshMaterial3d(material),
            Transform::from_xyz(-1.5, 0., 0.),
        ));
        let batch = sample(&mut app, "two shared cubes shadows on");
        assert!((1..=2).contains(&batch[0]));
        app.world_mut().entity_mut(cube).insert(Visibility::Hidden);
        let hidden = sample(&mut app, "one of two cubes hidden");
        assert_eq!(hidden[0], 1);
    }
}
