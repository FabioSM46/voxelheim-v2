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
//! composition hangs the horse's own head from the crest at one [`VIEW_SCALE`]. The
//! narrowest view keeps the ears below the horizon; opening to the default view brings
//! the head nearer and raises it, allowing the ear tips above the horizon. This makes the
//! default head twice its old projected height without clipping it at the narrowest FOV.
//! The neck continues below the world withers so even the widest frame has no sky under it.

use bevy::ecs::system::SystemParam;
use bevy::light::NotShadowCaster;
use bevy::prelude::*;

use super::camera::{ViewMode, WorldCamera};
use super::hands::{mounted_hand_transform, view_field_of_view};
use super::horse::{
    BROW, EYE_COLOUR, HAIR_COLOUR, HORSE_HEIGHT, HorseCoats, LEATHER_COLOUR, MANE_REST, MANE_ROOT,
    MANE_STRIP, MUZZLE, NECK_BASE, NECK_POLL, REIN_BIT, REIN_WIDTH, coat_colour, horse_ear_mesh,
    horse_eye_mesh, lofted_along_y, lofted_along_z,
};
use super::{ApplySnapshots, InputMode, LocalMount};
use crate::net::Session;

#[cfg(test)]
use super::camera::SADDLE_EYE_HEIGHT;

/// The point the composition hangs from, in the horse's own space (feet at y = 0, facing
/// -Z): the mane's root on the crest at the poll, where the head is hung and the neck ends.
const CREST: Vec3 = MANE_ROOT;

/// One block of horse is this much of camera space, at every field of view.
const VIEW_SCALE: f32 = 0.30;

/// Default-view crest placement. Bringing the head nearer doubles its projected height;
/// raising it leaves room for the muzzle below, with the ears just above the horizon.
const CREST_DEPTH: f32 = 0.395;
const CREST_Y: f32 = -0.048;

/// The narrow frame needs a more distant, lower composition to keep the whole head and
/// ears below the horizon. Only translation changes: every solid keeps the same scale.
const NARROW_CREST_DEPTH: f32 = 0.57;
const EARS_BELOW_HORIZON: f32 = 0.02;

#[cfg(test)]
const CAMERA_NEAR: f32 = 0.1;

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

/// The single coat asset shared by the head, ears and both neck pieces.
#[derive(Component)]
struct SaddleCoat(Handle<StandardMaterial>);

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
enum SaddlePart {
    Head,
    Ear,
    Eye,
    Neck,
    NeckExtension,
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
fn framing(field_of_view: f32) -> Transform {
    let narrow = crate::settings::MIN_FIELD_OF_VIEW.to_radians();
    let default_fov = crate::settings::DEFAULT_FIELD_OF_VIEW.to_radians();
    let opening = ((field_of_view - narrow) / (default_fov - narrow)).clamp(0.0, 1.0);
    let low = -(HORSE_HEIGHT - CREST.y) * VIEW_SCALE - EARS_BELOW_HORIZON;
    let narrow_crest = Vec3::new(0.0, low, -NARROW_CREST_DEPTH);
    let default_crest = Vec3::new(0.0, CREST_Y, -CREST_DEPTH);
    Transform::from_translation(narrow_crest.lerp(default_crest, opening))
        .with_scale(Vec3::splat(VIEW_SCALE))
}

/// A solid of the world horse, authored in the horse's own space, hung from the crest.
fn horse_piece(part: SaddlePart, mesh: Mesh, field_of_view: f32) -> Piece {
    Piece {
        part,
        mesh: mesh.translated_by(-CREST),
        transform: framing(field_of_view),
    }
}

fn ear(side: f32, field_of_view: f32) -> Piece {
    horse_piece(SaddlePart::Ear, horse_ear_mesh(side), field_of_view)
}

/// The mane at rest, lying along the crest exactly as the world horse wears it standing.
fn mane(field_of_view: f32) -> Piece {
    horse_piece(
        SaddlePart::Mane,
        Mesh::from(Cuboid::from_size(MANE_STRIP))
            .translated_by(Vec3::Y * -MANE_STRIP.y / 2.0)
            .transformed_by(
                Transform::from_translation(MANE_ROOT)
                    .with_rotation(Quat::from_rotation_x(MANE_REST)),
            ),
        field_of_view,
    )
}

/// The rein begins inside the shared bare fist and ends at the horse's own bit.
/// Its unit bar keeps one mesh while the projection changes its length and direction.
fn rein_transform(side: f32, field_of_view: f32) -> Transform {
    let mirror = Vec3::new(side, 1.0, 1.0);
    let start = mounted_hand_transform(side, field_of_view).translation;
    let end = framing(field_of_view).transform_point((REIN_BIT - CREST) * mirror);
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

fn pieces(field_of_view: f32) -> [Piece; 9] {
    [
        horse_piece(
            SaddlePart::Head,
            lofted_along_z(BROW, MUZZLE),
            field_of_view,
        ),
        ear(-1.0, field_of_view),
        ear(1.0, field_of_view),
        horse_piece(SaddlePart::Eye, horse_eye_mesh(), field_of_view),
        horse_piece(
            SaddlePart::Neck,
            lofted_along_y(NECK_BASE, NECK_POLL),
            field_of_view,
        ),
        // The original neck is unchanged. Continue its own base footprint down by one
        // mane length, only in this view, to cover the bottom edge at the widest FOV.
        horse_piece(
            SaddlePart::NeckExtension,
            lofted_along_y(NECK_BASE.lowered(MANE_STRIP.y), NECK_BASE),
            field_of_view,
        ),
        mane(field_of_view),
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
    coats: Res<HorseCoats>,
) {
    // Hidden until sync_view has resolved the authoritative mount colour.
    let coat_material = materials.add(StandardMaterial {
        base_color_texture: Some(coats.image.clone()),
        ..view_material(Color::WHITE)
    });
    let mane_material = materials.add(view_material(HAIR_COLOUR));
    let eye_material = materials.add(StandardMaterial {
        cull_mode: None,
        ..view_material(EYE_COLOUR)
    });
    let rein_material = materials.add(view_material(LEATHER_COLOUR));
    let root = commands
        .spawn((
            SaddleView,
            SaddleCoat(coat_material.clone()),
            Transform::default(),
            Visibility::Hidden,
        ))
        .id();

    commands.entity(root).with_children(|view| {
        for piece in pieces(view_field_of_view(None)) {
            let material = match piece.part {
                SaddlePart::Head
                | SaddlePart::Ear
                | SaddlePart::Neck
                | SaddlePart::NeckExtension => coat_material.clone(),
                SaddlePart::Eye => eye_material.clone(),
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
    mut horse: Query<&mut Transform, (With<SaddlePart>, Without<ReinSide>)>,
    mut roots: Query<(&mut Visibility, &SaddleCoat), With<SaddleView>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let visible = subject.mount.mounted()
        && subject.session.is_some()
        && subject.view.first_person()
        && matches!(*subject.mode, InputMode::Playing | InputMode::Chat);
    for (mut visibility, coat) in &mut roots {
        if let Some(kind) = subject.mount.kind()
            && let Some(mut material) = materials.get_mut(&coat.0)
        {
            let colour = coat_colour(kind);
            if material.base_color != colour {
                material.base_color = colour;
            }
        }
        *visibility = if visible {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }

    let field_of_view = view_field_of_view(camera.iter().next());
    let next_frame = framing(field_of_view);
    for mut transform in &mut horse {
        if *transform != next_frame {
            *transform = next_frame;
        }
    }
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
            .insert_resource(session());
        super::super::horse::register(&mut app);
        app.add_plugins(SaddleViewPlugin);
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
            (SaddlePart::Eye, 1),
            (SaddlePart::NeckExtension, 1),
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

    fn allowed_fields_of_view() -> impl Iterator<Item = f32> {
        // Include intermediate degrees as well as settings' five-degree steps: translation
        // blends continuously between the narrow and default compositions.
        let low = narrowest_field_of_view() as u32;
        let high = widest_field_of_view() as u32;
        (low..=high).map(|degrees| degrees as f32)
    }

    #[test]
    fn every_piece_clears_the_near_plane_at_every_field_of_view() {
        for degrees in allowed_fields_of_view() {
            for piece in &pieces(degrees.to_radians()) {
                for point in vertices(piece) {
                    assert!(
                        -point.z > CAMERA_NEAR,
                        "{degrees}: {:?} {point:?}",
                        piece.part
                    );
                }
            }
        }
    }

    #[test]
    fn the_head_ears_and_eyes_stay_inside_every_frame() {
        for degrees in allowed_fields_of_view() {
            for piece in pieces(degrees.to_radians()).iter().filter(|piece| {
                matches!(
                    piece.part,
                    SaddlePart::Head | SaddlePart::Ear | SaddlePart::Eye
                )
            }) {
                for point in vertices(piece) {
                    let frame = projected(point, degrees);
                    assert!(
                        frame.x.abs() <= 1.0 && frame.y.abs() <= 1.0,
                        "{degrees}: {:?} projects to {frame:?}",
                        piece.part
                    );
                    if degrees == narrowest_field_of_view() {
                        assert!(
                            point.y < 0.0,
                            "the narrow frame puts {:?} above the horizon",
                            piece.part
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_neck_crosses_the_bottom_edge_at_every_field_of_view() {
        for degrees in allowed_fields_of_view() {
            let lowest = pieces(degrees.to_radians())
                .iter()
                .filter(|piece| matches!(piece.part, SaddlePart::Neck | SaddlePart::NeckExtension))
                .flat_map(vertices)
                .map(|point| projected(point, degrees).y)
                .fold(f32::MAX, f32::min);
            assert!(lowest <= -1.0, "{degrees}: neck ends at {lowest}");
        }
    }

    #[test]
    fn the_default_head_has_at_least_twice_its_old_projected_height() {
        // Measured once from the old BROW/MUZZLE at scale 0.2, crest depth 0.62,
        // ear offset 0.02 and default 45-degree FOV: max(y/z) - min(y/z), / tan(22.5°).
        const OLD_PROJECTED_HEIGHT: f32 = 0.407_078_92;
        let degrees = crate::settings::Settings::default().field_of_view();
        let (low, high) = pieces(degrees.to_radians())
            .iter()
            .filter(|piece| piece.part == SaddlePart::Head)
            .flat_map(vertices)
            .map(|point| projected(point, degrees).y)
            .fold((f32::MAX, f32::MIN), |(low, high), y| {
                (low.min(y), high.max(y))
            });
        assert!(
            high - low >= 2.0 * OLD_PROJECTED_HEIGHT,
            "new height {}, old {OLD_PROJECTED_HEIGHT}",
            high - low
        );
    }

    /// Reserve room for the existing canter's three-degree crest nod (#878), without
    /// adding animation here. The continuation must clear the near plane too.
    #[test]
    fn the_framing_leaves_room_for_a_three_degree_crest_nod() {
        for degrees in allowed_fields_of_view() {
            for phase in 0..8 {
                let nod = 3.0_f32.to_radians() * (phase as f32 * std::f32::consts::TAU / 8.0).sin();
                for mut piece in pieces(degrees.to_radians()) {
                    if piece.part == SaddlePart::Rein {
                        continue;
                    }
                    piece.transform.rotation = Quat::from_rotation_x(nod);
                    for point in vertices(&piece) {
                        assert!(
                            -point.z > CAMERA_NEAR,
                            "{degrees}/{phase}: {:?} near plane {point:?}",
                            piece.part
                        );
                        if matches!(
                            piece.part,
                            SaddlePart::Head | SaddlePart::Ear | SaddlePart::Eye
                        ) {
                            let frame = projected(point, degrees);
                            assert!(
                                frame.x.abs() <= 1.0 && frame.y.abs() <= 1.0,
                                "{degrees}/{phase}: {:?} nods outside {frame:?}",
                                piece.part
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_head_stays_ahead_of_the_fists() {
        for degrees in allowed_fields_of_view() {
            let fov = degrees.to_radians();
            let fist_depth = -mounted_hand_transform(1.0, fov).translation.z;
            for piece in pieces(fov)
                .iter()
                .filter(|piece| matches!(piece.part, SaddlePart::Head | SaddlePart::Ear))
            {
                assert!(vertices(piece).iter().all(|point| -point.z > fist_depth));
            }
        }
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
        for degrees in [
            narrowest_field_of_view(),
            crate::settings::DEFAULT_FIELD_OF_VIEW,
            widest_field_of_view(),
        ] {
            app.insert_resource(TestFieldOfView(degrees.to_radians()));
            app.update();
            let world = app.world_mut();
            let mut query = world.query::<(&ReinSide, &Transform, &Mesh3d)>();
            for (side, transform, mesh) in query.iter(world) {
                assert_eq!(*transform, rein_transform(side.0, degrees.to_radians()));
                assert!(original.contains(&mesh.0));
            }
            let mut horse =
                world.query_filtered::<&Transform, (With<SaddlePart>, Without<ReinSide>)>();
            assert!(
                horse
                    .iter(world)
                    .all(|transform| *transform == framing(degrees.to_radians()))
            );
        }
    }

    #[test]
    fn the_eyes_are_the_world_horses_own_unlit_colour() {
        let mut app = app();
        let world = app.world_mut();
        let mut query = world.query::<(&SaddlePart, &MeshMaterial3d<StandardMaterial>)>();
        let eye = query
            .iter(world)
            .find(|(part, _)| **part == SaddlePart::Eye)
            .unwrap()
            .1
            .0
            .clone();
        let material = world
            .resource::<Assets<StandardMaterial>>()
            .get(&eye)
            .unwrap();
        assert_eq!(material.base_color, EYE_COLOUR);
        assert!(material.unlit);
        assert_eq!(material.cull_mode, None);
    }

    #[test]
    fn the_authoritative_kind_reskins_every_coat_piece_in_place() {
        let mut app = app();
        let image = app.world().resource::<HorseCoats>().image.clone();
        let coat = {
            let world = app.world_mut();
            world
                .query::<&SaddleCoat>()
                .single(world)
                .unwrap()
                .0
                .clone()
        };
        let original_assets = app.world().resource::<Assets<StandardMaterial>>().len();
        for kind in [
            MountKind::GreyHorse,
            MountKind::BlackHorse,
            MountKind::BrownHorse,
        ] {
            app.insert_resource(LocalMount::from_server(Some(kind)));
            app.update();
            assert_eq!(root(&mut app).0, Visibility::Visible);
            let world = app.world_mut();
            let mut parts = world.query::<(&SaddlePart, &MeshMaterial3d<StandardMaterial>)>();
            let coats: Vec<_> = parts
                .iter(world)
                .filter(|(part, _)| {
                    matches!(
                        part,
                        SaddlePart::Head
                            | SaddlePart::Ear
                            | SaddlePart::Neck
                            | SaddlePart::NeckExtension
                    )
                })
                .collect();
            assert_eq!(coats.len(), 5);
            assert!(coats.iter().all(|(_, material)| material.0 == coat));
            let materials = world.resource::<Assets<StandardMaterial>>();
            assert_eq!(materials.len(), original_assets);
            let material = materials.get(&coat).unwrap();
            assert_eq!(material.base_color, coat_colour(kind));
            assert_eq!(material.base_color_texture.as_ref(), Some(&image));
            assert!(material.unlit);
            assert!(!material.fog_enabled);
            assert_eq!(material.depth_bias, 1_000.0);
        }
        app.insert_resource(LocalMount::default());
        app.update();
        assert_eq!(root(&mut app).0, Visibility::Hidden);
        let material = app
            .world()
            .resource::<Assets<StandardMaterial>>()
            .get(&coat)
            .unwrap();
        assert_eq!(material.base_color, coat_colour(MountKind::BrownHorse));
        assert_eq!(material.base_color_texture.as_ref(), Some(&image));
    }

    #[test]
    fn hair_leather_and_eyes_share_the_world_horse_palette() {
        let mut app = app();
        let world = app.world_mut();
        let mut parts = world.query::<(&SaddlePart, &MeshMaterial3d<StandardMaterial>)>();
        let materials = world.resource::<Assets<StandardMaterial>>();
        for (part, material) in parts.iter(world) {
            let colour = match part {
                SaddlePart::Mane => HAIR_COLOUR,
                SaddlePart::Rein => LEATHER_COLOUR,
                SaddlePart::Eye => EYE_COLOUR,
                _ => continue,
            };
            let material = materials.get(&material.0).unwrap();
            assert_eq!(material.base_color, colour);
            assert!(material.base_color_texture.is_none());
            assert!(material.unlit);
            assert!(!material.fog_enabled);
            assert_eq!(material.depth_bias, 1_000.0);
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
            (SaddlePart::Eye, horse_eye_mesh()),
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
        let ear_tip = projected(
            Vec3::new(
                super::super::horse::EAR_CENTRE.x,
                HORSE_HEIGHT,
                super::super::horse::EAR_CENTRE.z,
            ) - eye,
            degrees,
        );
        assert!(
            ear_tip.y > -1.0 && ear_tip.y < 0.0,
            "the real ear tips are at {ear_tip}"
        );
    }
}
