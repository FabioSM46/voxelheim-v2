//! The first-person saddle view: a camera child, never a world horse.
//!
//! [`LocalMount`] owns its lifetime, so local intent cannot predict either transition.
//!
//! ## The horse everyone else sees, framed
//!
//! The head wedge, the ears, the neck and the mane are the world horse's own solids, cut by
//! the same lofts from the same constants `horse.rs` uses, so nothing here re-types a size.
//! What is this module's own is the **framing**, and it is a decision: from the saddle the
//! real horse is not in the frame. The eye sits at [`SADDLE_EYE_HEIGHT`], the poll over half
//! a block below it and a block ahead, and the narrowest field of view the settings allow
//! shows twenty degrees below the horizon — the ear tips and nothing else, which
//! `from_the_saddle_the_real_crest_is_below_the_narrowest_frame` measures. So the
//! composition hangs the horse's own head from a crest placed [`CREST_IN_VIEW`] at
//! [`VIEW_SCALE`] — the picture of that head from about three times the distance: ears just
//! under the horizon, the head wedge forward and below it, the crest and the mane running
//! down to the bottom edge of the narrowest frame, and the reins from the fists to the
//! horse's own bit.

use bevy::ecs::system::SystemParam;
use bevy::light::NotShadowCaster;
use bevy::prelude::*;

use super::camera::{ViewMode, WorldCamera};
use super::hands::{mounted_hand_transform, view_field_of_view};
use super::horse::{
    BROW, EAR, EAR_CENTRE, HORSE_HEIGHT, MANE_REST, MANE_ROOT, MANE_STRIP, MUZZLE, NECK_BASE,
    NECK_POLL, REIN_BIT, REIN_WIDTH, lofted_along_y, lofted_along_z,
};
use super::{ApplySnapshots, InputMode, LocalMount};
use crate::net::Session;

#[cfg(test)]
use super::camera::SADDLE_EYE_HEIGHT;

/// The point the composition hangs from, in the horse's own space (feet at y = 0, facing
/// -Z): the mane's root on the crest at the poll, where the head is hung and the neck ends.
const CREST: Vec3 = MANE_ROOT;

/// One block of horse is this much of camera space.
///
/// The picture does not depend on it — scaling about the eye leaves every angle alone — so
/// it is chosen for the near plane: small enough that the nearest part of the neck sits well
/// inside the pocket terrain cannot enter.
const VIEW_SCALE: f32 = 0.2;

/// How far ahead of the eye the crest sits, in camera space.
///
/// Set from the narrowest frame: the crest and the mane run from the poll back and down to
/// the withers, the lowest and nearest thing in the composition, and this is the depth at
/// which they reach the bottom edge at the minimum field of view without crossing it.
/// Nearer, and the frame cuts the neck; further, and it floats above the edge.
const CREST_DEPTH: f32 = 0.62;

/// How far below the horizon the ear tips sit, in camera space.
///
/// Below rather than on it: an ear touching the horizon reads as something on the skyline.
const EARS_BELOW_HORIZON: f32 = 0.02;

/// Where the crest sits in camera space: the height puts the ear tips, [`HORSE_HEIGHT`]
/// above the horse's feet, exactly [`EARS_BELOW_HORIZON`] under the horizon.
const CREST_IN_VIEW: Vec3 = Vec3::new(
    0.0,
    -(HORSE_HEIGHT - CREST.y) * VIEW_SCALE - EARS_BELOW_HORIZON,
    -CREST_DEPTH,
);

#[cfg(test)]
const CAMERA_NEAR: f32 = 0.1;

const COAT_COLOUR: Color = Color::srgb(0.22, 0.14, 0.08);
const MANE_COLOUR: Color = Color::srgb(0.07, 0.055, 0.045);
const REIN_COLOUR: Color = Color::srgb(0.12, 0.075, 0.04);

pub(super) struct SaddleViewPlugin;

impl Plugin for SaddleViewPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LocalMount>()
            .init_resource::<InputMode>()
            .init_resource::<ViewMode>()
            .add_systems(Startup, create_view)
            .add_systems(
                Update,
                (attach_to_camera, ApplyDeferred, sync_view)
                    .chain()
                    .after(ApplySnapshots)
                    .after(crate::settings::ApplyDisplaySettings),
            );
    }
}

#[derive(Component)]
struct SaddleView;

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
enum SaddlePart {
    Head,
    Ear,
    Neck,
    Mane,
    Rein,
}

#[derive(Component)]
struct ReinSide(f32);

/// One solid of the composition: a mesh in its own space, and where that space sits in
/// the camera's.
#[derive(Debug, Clone)]
struct Piece {
    part: SaddlePart,
    mesh: Mesh,
    transform: Transform,
}

/// The one transform every horse piece shares: crest space into camera space.
fn framing() -> Transform {
    Transform::from_translation(CREST_IN_VIEW).with_scale(Vec3::splat(VIEW_SCALE))
}

/// A solid of the world horse, authored in the horse's own space, hung from the crest.
fn horse_piece(part: SaddlePart, mesh: Mesh) -> Piece {
    Piece {
        part,
        mesh: mesh.translated_by(-CREST),
        transform: framing(),
    }
}

fn ear(side: f32) -> Piece {
    horse_piece(
        SaddlePart::Ear,
        Mesh::from(Cuboid::from_size(EAR)).translated_by(EAR_CENTRE * Vec3::new(side, 1.0, 1.0)),
    )
}

/// The mane at rest, lying along the crest exactly as the world horse wears it standing.
fn mane() -> Piece {
    horse_piece(
        SaddlePart::Mane,
        Mesh::from(Cuboid::from_size(MANE_STRIP))
            .translated_by(Vec3::Y * -MANE_STRIP.y / 2.0)
            .transformed_by(
                Transform::from_translation(MANE_ROOT)
                    .with_rotation(Quat::from_rotation_x(MANE_REST)),
            ),
    )
}

/// The rein begins inside the shared bare fist and ends at the horse's own bit.
/// Its unit bar keeps one mesh while the projection changes its length and direction.
fn rein_transform(side: f32, field_of_view: f32) -> Transform {
    let mirror = Vec3::new(side, 1.0, 1.0);
    let start = mounted_hand_transform(side, field_of_view).translation;
    let end = framing().transform_point((REIN_BIT - CREST) * mirror);
    let direction = end - start;
    Transform::from_translation((start + end) / 2.0)
        .with_rotation(Quat::from_rotation_arc(Vec3::Y, direction.normalize()))
        .with_scale(Vec3::new(1.0, direction.length(), 1.0))
}

fn rein(side: f32, field_of_view: f32) -> Piece {
    let width = REIN_WIDTH * VIEW_SCALE;
    Piece {
        part: SaddlePart::Rein,
        mesh: Mesh::from(Cuboid::new(width, 1.0, width)),
        transform: rein_transform(side, field_of_view),
    }
}

fn pieces(field_of_view: f32) -> [Piece; 7] {
    [
        horse_piece(SaddlePart::Head, lofted_along_z(BROW, MUZZLE)),
        ear(-1.0),
        ear(1.0),
        horse_piece(SaddlePart::Neck, lofted_along_y(NECK_BASE, NECK_POLL)),
        mane(),
        rein(-1.0, field_of_view),
        rein(1.0, field_of_view),
    ]
}

fn view_material(colour: Color) -> StandardMaterial {
    StandardMaterial {
        base_color: colour,
        unlit: true,
        fog_enabled: false,
        // Terrain touching the capsule must not slice this camera-space view.
        depth_bias: 1_000.0,
        ..default()
    }
}

fn create_view(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let coat_material = materials.add(view_material(COAT_COLOUR));
    let mane_material = materials.add(view_material(MANE_COLOUR));
    let rein_material = materials.add(view_material(REIN_COLOUR));
    let root = commands
        .spawn((SaddleView, Transform::default(), Visibility::Hidden))
        .id();

    commands.entity(root).with_children(|view| {
        for piece in pieces(view_field_of_view(None)) {
            let material = match piece.part {
                SaddlePart::Head | SaddlePart::Ear | SaddlePart::Neck => coat_material.clone(),
                SaddlePart::Mane => mane_material.clone(),
                SaddlePart::Rein => rein_material.clone(),
            };
            let part = piece.part;
            let side = piece.transform.translation.x.signum();
            let mut entity = view.spawn((
                piece.part,
                Mesh3d(meshes.add(piece.mesh)),
                MeshMaterial3d(material),
                piece.transform,
                Visibility::Inherited,
                NotShadowCaster,
            ));
            if part == SaddlePart::Rein {
                entity.insert(ReinSide(side));
            }
        }
    });
}

fn attach_to_camera(
    mut commands: Commands,
    cameras: Query<Entity, With<WorldCamera>>,
    unattached: Query<Entity, (With<SaddleView>, Without<ChildOf>)>,
) {
    let Some(camera) = cameras.iter().next() else {
        return;
    };
    for view in &unattached {
        commands.entity(view).insert(ChildOf(camera));
    }
}

#[derive(SystemParam)]
struct SaddleSubject<'w> {
    mount: Res<'w, LocalMount>,
    mode: Res<'w, InputMode>,
    view: Res<'w, ViewMode>,
    session: Option<Res<'w, Session>>,
}

fn sync_view(
    subject: SaddleSubject<'_>,
    camera: Query<&Projection, With<WorldCamera>>,
    mut reins: Query<(&ReinSide, &mut Transform)>,
    mut roots: Query<&mut Visibility, With<SaddleView>>,
) {
    let visible = subject.mount.mounted()
        && subject.session.is_some()
        && subject.view.first_person()
        && matches!(*subject.mode, InputMode::Playing | InputMode::Chat);
    for mut visibility in &mut roots {
        *visibility = if visible {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }

    let field_of_view = view_field_of_view(camera.iter().next());
    for (side, mut transform) in &mut reins {
        let next = rein_transform(side.0, field_of_view);
        if *transform != next {
            *transform = next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{MountKind, SessionParams};
    use bevy::asset::AssetPlugin;
    use bevy::mesh::VertexAttributeValues;

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 7,
            spawn: [0.0; 3],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 3,
            inventory_slots: 5,
            hotbar_slots: 4,
            equipment_slots: 1,
            player_token: crate::net::ANY_TOKEN,
            voice_range_blocks: 0.0,
        })
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .insert_resource(session())
            .add_plugins(SaddleViewPlugin);
        app.world_mut()
            .spawn((WorldCamera, Transform::default(), Visibility::Inherited));
        app.update();
        app
    }

    fn root(app: &mut App) -> (Visibility, Entity) {
        let world = app.world_mut();
        let mut query = world.query_filtered::<(&Visibility, &ChildOf), With<SaddleView>>();
        let (visibility, parent) = query.single(world).expect("one saddle view");
        (*visibility, parent.parent())
    }

    /// Every vertex of a piece, in camera space.
    fn vertices(piece: &Piece) -> Vec<Vec3> {
        let Some(VertexAttributeValues::Float32x3(positions)) =
            piece.mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("{:?} has no positions", piece.part);
        };
        positions
            .iter()
            .map(|position| piece.transform.transform_point(Vec3::from_array(*position)))
            .collect()
    }

    fn extent(points: impl IntoIterator<Item = Vec3>) -> (Vec3, Vec3) {
        points
            .into_iter()
            .fold((Vec3::MAX, Vec3::MIN), |(low, high), point| {
                (low.min(point), high.max(point))
            })
    }

    /// The narrowest vertical field of view the settings allow, in degrees.
    fn narrowest_field_of_view() -> f32 {
        let mut settings = crate::settings::Settings::default();
        settings.adjust(crate::settings::Knob::FieldOfView, -1_000);
        settings.field_of_view()
    }

    fn widest_field_of_view() -> f32 {
        let mut settings = crate::settings::Settings::default();
        settings.adjust(crate::settings::Knob::FieldOfView, 1_000);
        settings.field_of_view()
    }

    /// Where a camera-space point lands in a 16:9 frame at `degrees` of vertical field of
    /// view: ±1 is the edge on either axis.
    fn projected(point: Vec3, degrees: f32) -> Vec2 {
        const ASPECT: f32 = 16.0 / 9.0;
        let tangent = (degrees.to_radians() / 2.0).tan();
        let depth = -point.z;
        Vec2::new(
            point.x / (depth * tangent * ASPECT),
            point.y / (depth * tangent),
        )
    }

    #[test]
    fn the_authoritative_mount_owns_one_camera_child_head_neck_and_reins() {
        let mut app = app();
        assert_eq!(root(&mut app).0, Visibility::Hidden);

        app.insert_resource(LocalMount::from_server(Some(MountKind::BrownHorse)));
        app.update();
        let (visibility, parent) = root(&mut app);
        assert_eq!(visibility, Visibility::Visible);
        assert!(app.world().entity(parent).contains::<WorldCamera>());

        let world = app.world_mut();
        let mut parts = world.query::<&SaddlePart>();
        let found: Vec<SaddlePart> = parts.iter(world).copied().collect();
        for (part, count) in [
            (SaddlePart::Head, 1),
            (SaddlePart::Ear, 2),
            (SaddlePart::Neck, 1),
            (SaddlePart::Mane, 1),
            (SaddlePart::Rein, 2),
        ] {
            assert_eq!(found.iter().filter(|found| **found == part).count(), count);
        }

        app.insert_resource(LocalMount::default());
        app.update();
        assert_eq!(root(&mut app).0, Visibility::Hidden);
    }

    #[test]
    fn third_person_never_draws_the_camera_space_reins() {
        let mut app = app();
        app.insert_resource(LocalMount::from_server(Some(MountKind::GreyHorse)));
        *app.world_mut().resource_mut::<ViewMode>() = ViewMode::ThirdPerson;
        app.update();
        assert_eq!(root(&mut app).0, Visibility::Hidden);

        *app.world_mut().resource_mut::<ViewMode>() = ViewMode::FirstPerson;
        app.update();
        assert_eq!(root(&mut app).0, Visibility::Visible);
    }

    #[test]
    fn every_piece_clears_the_near_plane_and_the_whole_allowed_fov_range() {
        let mut settings = crate::settings::Settings::default();
        settings.adjust(crate::settings::Knob::FieldOfView, -1_000);
        loop {
            let degrees = settings.field_of_view();
            for piece in &pieces(degrees.to_radians()) {
                for point in vertices(piece) {
                    assert!(-point.z > CAMERA_NEAR, "{degrees} degrees: {point:?}");
                    let frame = projected(point, degrees);
                    assert!(
                        frame.x.abs() <= 1.0 && frame.y.abs() <= 1.0,
                        "{degrees} degrees: {:?} {point:?} projects to {frame:?}",
                        piece.part
                    );
                }
            }
            settings.adjust(crate::settings::Knob::FieldOfView, 1);
            if settings.field_of_view() == degrees {
                break;
            }
        }
    }

    /// The composition, pinned where a number could drift: head and ears forward of the
    /// fists and under the horizon, crest and mane reaching the bottom of the narrowest frame.
    #[test]
    fn the_head_hangs_forward_and_below_the_horizon_and_the_crest_fills_the_lower_frame() {
        let pieces = pieces(view_field_of_view(None));
        let nearest_fist = -mounted_hand_transform(1.0, view_field_of_view(None))
            .translation
            .z;

        for piece in pieces
            .iter()
            .filter(|piece| matches!(piece.part, SaddlePart::Head | SaddlePart::Ear))
        {
            for point in vertices(piece) {
                assert!(
                    point.y < 0.0,
                    "{:?} rises to the horizon: {point}",
                    piece.part
                );
                assert!(
                    -point.z > nearest_fist,
                    "{:?} sits behind the fists: {point}",
                    piece.part
                );
            }
        }

        let degrees = narrowest_field_of_view();
        let lowest = pieces
            .iter()
            .filter(|piece| matches!(piece.part, SaddlePart::Neck | SaddlePart::Mane))
            .flat_map(vertices)
            .map(|point| projected(point, degrees).y)
            .fold(f32::MAX, f32::min);
        assert!(
            lowest <= -0.9,
            "at {degrees} degrees the crest reaches only {lowest} of the way to the bottom edge"
        );
    }

    /// Each rein starts inside its fist and ends inside the head, in the mouth.
    #[test]
    fn the_reins_run_from_the_fists_to_the_bit_in_the_mouth() {
        for degrees in [
            narrowest_field_of_view(),
            crate::settings::Settings::default().field_of_view(),
            widest_field_of_view(),
        ] {
            let fov = degrees.to_radians();
            let pieces = pieces(fov);
            let (head_low, head_high) = extent(
                pieces
                    .iter()
                    .filter(|piece| piece.part == SaddlePart::Head)
                    .flat_map(vertices),
            );
            let mut reins = 0;
            for piece in pieces.iter().filter(|piece| piece.part == SaddlePart::Rein) {
                let start = piece.transform.transform_point(Vec3::Y * -0.5);
                let end = piece.transform.transform_point(Vec3::Y * 0.5);
                assert!(
                    [-1.0, 1.0].into_iter().any(|side| {
                        let local = mounted_hand_transform(side, fov)
                            .compute_affine()
                            .inverse()
                            .transform_point3(start);
                        local
                            .abs()
                            .cmple(super::super::hands::HAND_SIZE / 2.0)
                            .all()
                    }),
                    "{degrees}: a rein starts outside every fist: {start}"
                );
                assert!(
                    end.cmpge(head_low).all() && end.cmple(head_high).all(),
                    "a rein ends outside the head: {end} not in {head_low}..{head_high}"
                );
                reins += 1;
            }
            assert_eq!(reins, 2);
        }
    }

    #[derive(Resource)]
    struct TestFieldOfView(f32);

    fn change_test_projection(fov: Res<TestFieldOfView>, mut cameras: Query<&mut Projection>) {
        for mut projection in &mut cameras {
            if let Projection::Perspective(perspective) = projection.as_mut() {
                perspective.fov = fov.0;
            }
        }
    }

    #[test]
    fn changing_the_projection_moves_both_reins_without_rebuilding_their_meshes() {
        let mut app = app();
        app.insert_resource(LocalMount::from_server(Some(MountKind::BrownHorse)));
        let camera = root(&mut app).1;
        app.world_mut()
            .entity_mut(camera)
            .insert(Projection::default());
        app.add_systems(
            Update,
            change_test_projection.in_set(crate::settings::ApplyDisplaySettings),
        );
        let original: Vec<Handle<Mesh>> = {
            let world = app.world_mut();
            world
                .query_filtered::<&Mesh3d, With<ReinSide>>()
                .iter(world)
                .map(|mesh| mesh.0.clone())
                .collect()
        };
        for degrees in [narrowest_field_of_view(), widest_field_of_view()] {
            app.insert_resource(TestFieldOfView(degrees.to_radians()));
            app.update();
            let world = app.world_mut();
            let mut query = world.query::<(&ReinSide, &Transform, &Mesh3d)>();
            for (side, transform, mesh) in query.iter(world) {
                assert_eq!(*transform, rein_transform(side.0, degrees.to_radians()));
                assert!(original.contains(&mesh.0));
            }
        }
    }

    /// The head and the neck are the world horse's solids at [`VIEW_SCALE`], the same spans
    /// across, up and along scaled once: nothing here has re-typed a size.
    #[test]
    fn the_head_and_neck_are_the_world_horses_own_solids_at_one_scale() {
        let span = |points: Vec<Vec3>| {
            let (low, high) = extent(points);
            high - low
        };
        let world_positions = |mesh: &Mesh| -> Vec<Vec3> {
            let Some(VertexAttributeValues::Float32x3(positions)) =
                mesh.attribute(Mesh::ATTRIBUTE_POSITION)
            else {
                panic!("a horse solid has no positions");
            };
            positions.iter().copied().map(Vec3::from_array).collect()
        };

        let pieces = pieces(view_field_of_view(None));
        let view_span = |part: SaddlePart| {
            span(
                pieces
                    .iter()
                    .filter(|piece| piece.part == part)
                    .flat_map(vertices)
                    .collect(),
            )
        };
        for (part, world) in [
            (SaddlePart::Head, lofted_along_z(BROW, MUZZLE)),
            (SaddlePart::Neck, lofted_along_y(NECK_BASE, NECK_POLL)),
        ] {
            let wanted = span(world_positions(&world)) * VIEW_SCALE;
            let got = view_span(part);
            assert!(
                got.abs_diff_eq(wanted, 1e-5),
                "{part:?} spans {got}, the world horse's scaled {wanted}"
            );
        }
    }

    /// Why the composition is framed rather than measured: from the saddle eye the real
    /// crest is below the narrowest frame, and the real ear tips are all that is inside it.
    #[test]
    fn from_the_saddle_the_real_crest_is_below_the_narrowest_frame() {
        let degrees = narrowest_field_of_view();
        let eye = Vec3::Y * SADDLE_EYE_HEIGHT;
        let crest = projected(CREST - eye, degrees);
        assert!(
            crest.y < -1.0,
            "the real crest is in the narrowest frame at {crest}, so the view need not be framed"
        );
        let ear_tip = projected(EAR_CENTRE + Vec3::Y * (EAR.y / 2.0) - eye, degrees);
        assert!(
            ear_tip.y > -1.0 && ear_tip.y < 0.0,
            "the real ear tips are at {ear_tip}"
        );
    }
}
