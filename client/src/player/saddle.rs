//! The first-person saddle view: a camera child, never a world horse.
//!
//! [`LocalMount`] owns its lifetime, so local intent cannot predict either transition.

use bevy::ecs::system::SystemParam;
use bevy::light::NotShadowCaster;
use bevy::prelude::*;

use super::camera::{ViewMode, WorldCamera};
use super::hands::HAND_SIZE;
use super::{Appearances, ApplySnapshots, InputMode, LocalMount};
use crate::net::{PLACEHOLDER_APPEARANCE, Session};

const HEAD_SIZE: Vec3 = Vec3::new(0.18, 0.10, 0.24);
const HEAD_CENTRE: Vec3 = Vec3::new(0.0, -0.07, -0.50);
const EAR_SIZE: Vec3 = Vec3::new(0.04, 0.08, 0.04);
const EAR_CENTRE: Vec3 = Vec3::new(0.06, -0.005, -0.42);
const HAND_CENTRE: Vec3 = Vec3::new(0.105, -0.075, -0.30);
const REIN_START: Vec3 = Vec3::new(0.105, -0.063, -0.31);
const REIN_END: Vec3 = Vec3::new(0.055, -0.035, -0.39);
const REIN_WIDTH: f32 = 0.006;
#[cfg(test)]
const CAMERA_NEAR: f32 = 0.1;

const HEAD_COLOUR: Color = Color::srgb(0.22, 0.14, 0.08);
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
    Hand,
    Rein,
}

#[derive(Resource)]
struct SaddleVisuals {
    skin: Handle<StandardMaterial>,
}

#[derive(Debug, Clone, Copy)]
struct Piece {
    part: SaddlePart,
    transform: Transform,
}

fn cuboid(part: SaddlePart, centre: Vec3, size: Vec3) -> Piece {
    Piece {
        part,
        transform: Transform::from_translation(centre).with_scale(size),
    }
}

fn rein(side: f32) -> Piece {
    let start = REIN_START * Vec3::new(side, 1.0, 1.0);
    let end = REIN_END * Vec3::new(side, 1.0, 1.0);
    let direction = end - start;
    Piece {
        part: SaddlePart::Rein,
        transform: Transform::from_translation((start + end) / 2.0)
            .with_rotation(Quat::from_rotation_arc(Vec3::Y, direction.normalize()))
            .with_scale(Vec3::new(REIN_WIDTH, direction.length(), REIN_WIDTH)),
    }
}

fn pieces() -> [Piece; 7] {
    [
        cuboid(SaddlePart::Head, HEAD_CENTRE, HEAD_SIZE),
        cuboid(
            SaddlePart::Ear,
            EAR_CENTRE * Vec3::new(-1.0, 1.0, 1.0),
            EAR_SIZE,
        ),
        cuboid(SaddlePart::Ear, EAR_CENTRE, EAR_SIZE),
        cuboid(
            SaddlePart::Hand,
            HAND_CENTRE * Vec3::new(-1.0, 1.0, 1.0),
            HAND_SIZE,
        ),
        cuboid(SaddlePart::Hand, HAND_CENTRE, HAND_SIZE),
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
    let cube = meshes.add(Cuboid::from_length(1.0));
    let visuals = SaddleVisuals {
        skin: materials.add(view_material(skin_colour(
            PLACEHOLDER_APPEARANCE.skin_color(),
        ))),
    };
    let head_material = materials.add(view_material(HEAD_COLOUR));
    let rein_material = materials.add(view_material(REIN_COLOUR));
    let root = commands
        .spawn((SaddleView, Transform::default(), Visibility::Hidden))
        .id();

    commands.entity(root).with_children(|view| {
        for piece in pieces() {
            let material = match piece.part {
                SaddlePart::Head | SaddlePart::Ear => head_material.clone(),
                SaddlePart::Hand => visuals.skin.clone(),
                SaddlePart::Rein => rein_material.clone(),
            };
            view.spawn((
                piece.part,
                Mesh3d(cube.clone()),
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

    #[test]
    fn the_authoritative_mount_owns_one_camera_child_head_hands_and_reins() {
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
        const ASPECT: f32 = 16.0 / 9.0;
        let mut settings = crate::settings::Settings::default();
        settings.adjust(crate::settings::Knob::FieldOfView, -1_000);
        loop {
            let degrees = settings.field_of_view();
            let tangent = (degrees.to_radians() / 2.0).tan();
            for piece in pieces() {
                for x in [-0.5, 0.5] {
                    for y in [-0.5, 0.5] {
                        for z in [-0.5, 0.5] {
                            let point = piece.transform.transform_point(Vec3::new(x, y, z));
                            let depth = -point.z;
                            assert!(depth > CAMERA_NEAR, "{degrees} degrees: {point:?}");
                            let projected = Vec2::new(
                                point.x / (depth * tangent * ASPECT),
                                point.y / (depth * tangent),
                            );
                            assert!(
                                projected.x.abs() <= 1.0 && projected.y.abs() <= 1.0,
                                "{degrees} degrees: {point:?} projects to {projected:?}"
                            );
                        }
                    }
                }
            }
            settings.adjust(crate::settings::Knob::FieldOfView, 1);
            if settings.field_of_view() == degrees {
                break;
            }
        }
    }
}
