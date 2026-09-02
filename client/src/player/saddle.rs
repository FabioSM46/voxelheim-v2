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
use super::hands::HAND_SIZE;
use super::horse::{
    BROW, EAR, EAR_CENTRE, HORSE_HEIGHT, MANE_REST, MANE_ROOT, MANE_STRIP, MUZZLE, NECK_BASE,
    NECK_POLL, REIN_BIT, REIN_WIDTH, lofted_along_y, lofted_along_z,
};
use super::{Appearances, ApplySnapshots, InputMode, LocalMount};
use crate::net::{PLACEHOLDER_APPEARANCE, Session};

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

/// Where each fist sits, in camera space. The reins leave from its top.
const HAND_CENTRE: Vec3 = Vec3::new(0.105, -0.075, -0.30);

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
            .init_resource::<Appearances>()
            .add_systems(Startup, create_view)
            .add_systems(
                Update,
                (attach_to_camera, ApplyDeferred, sync_view)
                    .chain()
                    .after(ApplySnapshots),
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
    Hand,
    Rein,
}

#[derive(Resource)]
struct SaddleVisuals {
    skin: Handle<StandardMaterial>,
}

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

fn hand(side: f32) -> Piece {
    Piece {
        part: SaddlePart::Hand,
        mesh: Mesh::from(Cuboid::from_size(HAND_SIZE)),
        transform: Transform::from_translation(HAND_CENTRE * Vec3::new(side, 1.0, 1.0)),
    }
}

/// From the top of the fist to the horse's own bit under the head's framing, so the reins
/// end in the mouth because the head is where it is.
fn rein(side: f32) -> Piece {
    let mirror = Vec3::new(side, 1.0, 1.0);
    let start = (HAND_CENTRE + Vec3::Y * (HAND_SIZE.y / 2.0)) * mirror;
    let end = framing().transform_point((REIN_BIT - CREST) * mirror);
    let direction = end - start;
    let width = REIN_WIDTH * VIEW_SCALE;
    Piece {
        part: SaddlePart::Rein,
        mesh: Mesh::from(Cuboid::new(width, direction.length(), width)),
        transform: Transform::from_translation((start + end) / 2.0)
            .with_rotation(Quat::from_rotation_arc(Vec3::Y, direction.normalize())),
    }
}

fn pieces() -> [Piece; 9] {
    [
        horse_piece(SaddlePart::Head, lofted_along_z(BROW, MUZZLE)),
        ear(-1.0),
        ear(1.0),
        horse_piece(SaddlePart::Neck, lofted_along_y(NECK_BASE, NECK_POLL)),
        mane(),
        hand(-1.0),
        hand(1.0),
        rein(-1.0),
        rein(1.0),
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
    let visuals = SaddleVisuals {
        skin: materials.add(view_material(skin_colour(
            PLACEHOLDER_APPEARANCE.skin_color(),
        ))),
    };
    let coat_material = materials.add(view_material(COAT_COLOUR));
    let mane_material = materials.add(view_material(MANE_COLOUR));
    let rein_material = materials.add(view_material(REIN_COLOUR));
    let root = commands
        .spawn((SaddleView, Transform::default(), Visibility::Hidden))
        .id();

    commands.entity(root).with_children(|view| {
        for piece in pieces() {
            let material = match piece.part {
                SaddlePart::Head | SaddlePart::Ear | SaddlePart::Neck => coat_material.clone(),
                SaddlePart::Mane => mane_material.clone(),
                SaddlePart::Hand => visuals.skin.clone(),
                SaddlePart::Rein => rein_material.clone(),
            };
            view.spawn((
                piece.part,
                Mesh3d(meshes.add(piece.mesh)),
                MeshMaterial3d(material),
                piece.transform,
                Visibility::Inherited,
                NotShadowCaster,
            ));
        }
    });
    commands.insert_resource(visuals);
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

fn skin_colour(colour: u32) -> Color {
    Color::srgb_u8(
        ((colour >> 16) & 0xFF) as u8,
        ((colour >> 8) & 0xFF) as u8,
        (colour & 0xFF) as u8,
    )
}

#[derive(SystemParam)]
struct SaddleSubject<'w> {
    mount: Res<'w, LocalMount>,
    mode: Res<'w, InputMode>,
    view: Res<'w, ViewMode>,
    session: Option<Res<'w, Session>>,
    appearances: Res<'w, Appearances>,
}

#[derive(SystemParam)]
struct SaddleAssets<'w> {
    visuals: Res<'w, SaddleVisuals>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
}

fn sync_view(
    subject: SaddleSubject<'_>,
    mut assets: SaddleAssets<'_>,
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

    let skin = subject
        .session
        .as_deref()
        .and_then(|session| subject.appearances.0.get(&session.0.entity_id))
        .map_or(PLACEHOLDER_APPEARANCE.skin_color(), |described| {
            described.appearance.skin_color()
        });
    if let Some(mut material) = assets.materials.get_mut(&assets.visuals.skin) {
        let wanted = skin_colour(skin);
        if material.base_color != wanted {
            material.base_color = wanted;
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
    fn the_authoritative_mount_owns_one_camera_child_head_neck_hands_and_reins() {
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
            (SaddlePart::Hand, 2),
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
            for piece in &pieces() {
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
        let pieces = pieces();
        let nearest_fist = pieces
            .iter()
            .filter(|piece| piece.part == SaddlePart::Hand)
            .flat_map(vertices)
            .map(|point| -point.z)
            .fold(f32::MAX, f32::min);

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
        let pieces = pieces();
        let (head_low, head_high) = extent(
            pieces
                .iter()
                .filter(|piece| piece.part == SaddlePart::Head)
                .flat_map(vertices),
        );
        let fists: Vec<(Vec3, Vec3)> = pieces
            .iter()
            .filter(|piece| piece.part == SaddlePart::Hand)
            .map(|piece| extent(vertices(piece)))
            .collect();

        let mut reins = 0;
        for piece in pieces.iter().filter(|piece| piece.part == SaddlePart::Rein) {
            // A rein is a bar along its own Y, so its ends are the centres of the two end
            // faces: half its own length either way from its origin.
            let Some(VertexAttributeValues::Float32x3(local)) =
                piece.mesh.attribute(Mesh::ATTRIBUTE_POSITION)
            else {
                panic!("a rein has no positions");
            };
            let (low, high) = extent(local.iter().copied().map(Vec3::from_array));
            let half = (high.y - low.y) / 2.0;
            let start = piece.transform.transform_point(Vec3::Y * -half);
            let end = piece.transform.transform_point(Vec3::Y * half);
            assert!(
                fists.iter().any(|(low, high)| {
                    start.cmpge(*low - 1e-4).all() && start.cmple(*high + 1e-4).all()
                }),
                "a rein starts outside every fist: {start}"
            );
            assert!(
                end.cmpge(head_low).all() && end.cmple(head_high).all(),
                "a rein ends outside the head: {end} not in {head_low}..{head_high}"
            );
            reins += 1;
        }
        assert_eq!(reins, 2);
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

        let pieces = pieces();
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
