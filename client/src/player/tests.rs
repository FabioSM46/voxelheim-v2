//! Tests for the player module.
//!
//! No display, no GPU, no window, and that is a rule rather than a coincidence — a gate CI
//! cannot run is not a gate. `MinimalPlugins` plus `AssetPlugin` gives the whole pipeline
//! short of the GPU upload: `Assets<T>` is an ordinary resource, so meshes, materials,
//! entities and transforms all exist with no render app at all.
//!
//! `InputPlugin` is added only where the keyboard is the thing under test, which is also
//! what proves [`sample_input`] tolerates its absence.

use bevy::asset::AssetPlugin;
use bevy::input::ButtonState;
use bevy::input::InputPlugin;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::input::mouse::MouseMotion;
use bevy::mesh::VertexAttributeValues;
use bevy::time::TimeUpdateStrategy;

use super::*;
use crate::net::{
    EntityState, PlayerAppearance, SessionParams, Snapshot, WeatherKind, WeatherState, WorldClock,
};

const TICK_RATE: u8 = 20;
const INTERVAL: Duration = Duration::from_millis(50);

/// This session's own entity, as `ServerWelcome` names it.
const LOCAL_ID: u64 = 7;

fn session() -> Session {
    Session(SessionParams {
        clock: Default::default(),
        entity_id: LOCAL_ID,
        spawn: [0.5, 64.0, 0.5],
        world_seed: 1,
        tick_rate: TICK_RATE,
        chunk_size: 32,
        view_distance: 8,
        inventory_slots: 37,
        hotbar_slots: 9,
        equipment_slots: 4,
        player_token: crate::net::ANY_TOKEN,
    })
}

fn state(entity_id: u64, pos: [f32; 3], yaw: f32) -> EntityState {
    EntityState {
        entity_id,
        pos,
        vel: [0.0, 0.0, 0.0],
        yaw,
    }
}

/// The player module on a headless app, with a session already established.
fn headless_player() -> App {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, AssetPlugin::default()))
        .init_asset::<Mesh>()
        .init_asset::<StandardMaterial>()
        .insert_resource(session())
        .add_plugins(PlayerPlugin);
    app
}

/// Queues a snapshot as the net thread would.
fn deliver(app: &mut App, tick: u32, entities: Vec<EntityState>, at: Instant) {
    app.world_mut().resource_mut::<SnapshotInbox>().push(
        Snapshot {
            server_tick: tick,
            entities,
            drops: vec![],
            ..Default::default()
        },
        at,
    );
}

#[test]
fn weather_mirrors_only_the_newest_accepted_snapshot() {
    let mut app = headless_player();
    let deliver_weather = |app: &mut App, tick, kind, intensity| {
        app.world_mut().resource_mut::<SnapshotInbox>().push(
            Snapshot {
                server_tick: tick,
                weather: Some(WeatherState { kind, intensity }),
                ..Default::default()
            },
            Instant::now(),
        );
    };

    deliver_weather(&mut app, 10, WeatherKind::Rain, 128);
    app.update();
    assert_eq!(
        app.world().resource::<Weather>().get(),
        Some(WeatherState {
            kind: WeatherKind::Rain,
            intensity: 128,
        })
    );

    deliver_weather(&mut app, 9, WeatherKind::Sandstorm, 255);
    app.update();
    assert_eq!(
        app.world().resource::<Weather>().get(),
        Some(WeatherState {
            kind: WeatherKind::Rain,
            intensity: 128,
        }),
        "a snapshot the buffer refused changed the weather"
    );
}

#[test]
fn an_ended_session_takes_its_weather_with_it() {
    let mut app = headless_player();
    app.world_mut().resource_mut::<SnapshotInbox>().push(
        Snapshot {
            server_tick: 1,
            weather: Some(WeatherState {
                kind: WeatherKind::Snow,
                intensity: 200,
            }),
            ..Default::default()
        },
        Instant::now(),
    );
    app.update();
    assert!(app.world().resource::<Weather>().get().is_some());

    app.world_mut().remove_resource::<Session>();
    app.update();
    assert_eq!(app.world().resource::<Weather>().get(), None);
}

fn deliver_party(
    app: &mut App,
    tick: u32,
    entities: Vec<EntityState>,
    leader_entity_id: u64,
    members: Vec<crate::net::PartyMemberState>,
    member_names: &[&str],
    at: Instant,
) {
    let mut roster: Vec<crate::net::PartyRosterMember> = members
        .iter()
        .zip(member_names)
        .map(|(member, name)| crate::net::PartyRosterMember {
            character_id: member.entity_id,
            entity_id: member.entity_id,
            name: (*name).to_owned(),
            online: true,
        })
        .collect();
    roster.push(crate::net::PartyRosterMember {
        character_id: LOCAL_ID,
        entity_id: LOCAL_ID,
        name: "This session".to_owned(),
        online: true,
    });
    roster.sort_by_key(|member| member.entity_id != leader_entity_id);
    app.world_mut().resource_mut::<SnapshotInbox>().push(
        Snapshot {
            server_tick: tick,
            entities,
            drops: vec![],
            party_leader_entity_id: leader_entity_id,
            party_members: members,
            party_roster: roster,
            ..Default::default()
        },
        at,
    );
}

fn party_member(
    entity_id: u64,
    health: u16,
    max_health: u16,
    alive: bool,
) -> crate::net::PartyMemberState {
    crate::net::PartyMemberState {
        entity_id,
        pos: [0.0, 64.0, 0.0],
        health,
        max_health,
        alive,
    }
}

/// The five colours a character wears, none of them [`appearance::EYE_COLOUR`], so a body
/// built from them has one material per part and a test can tell them apart.
const A_SKIN: u32 = 0x00C6_8642;
const A_SHIRT: u32 = 0x008C_3B2B;
const A_TROUSERS: u32 = 0x003B_3226;
const A_SHOES: u32 = 0x002A_211B;
const A_HAIR: u32 = 0x006B_4423;

fn an_appearance(model: HairModel) -> Appearance {
    Appearance::new(A_SKIN, A_SHIRT, A_TROUSERS, A_SHOES, model, A_HAIR)
        .expect("every colour is inside the contract's range")
}

/// Queues an appearance as the net thread would.
fn describe_as(app: &mut App, entity_id: u64, name: &str, appearance: Appearance) {
    describe_as_level(app, entity_id, name, appearance, 1);
}

fn describe_as_level(
    app: &mut App,
    entity_id: u64,
    name: &str,
    appearance: Appearance,
    level: u16,
) {
    app.world_mut()
        .resource_mut::<AppearanceInbox>()
        .push(PlayerAppearance {
            entity_id,
            appearance,
            name: name.to_owned(),
            worn_head: 0,
            worn_chest: 0,
            worn_legs: 0,
            worn_offhand: 0,
            level,
        });
}

fn describe_wearing(app: &mut App, entity_id: u64, appearance: Appearance, worn: [u16; 4]) {
    app.world_mut()
        .resource_mut::<AppearanceInbox>()
        .push(PlayerAppearance {
            entity_id,
            appearance,
            name: "Test Character".to_owned(),
            worn_head: worn[0],
            worn_chest: worn[1],
            worn_legs: worn[2],
            worn_offhand: worn[3],
            level: 1,
        });
}

fn describe(app: &mut App, entity_id: u64, appearance: Appearance) {
    describe_as(app, entity_id, "Test Character", appearance);
}

/// The entity drawing one of the server's, if there is one.
fn body_of(app: &mut App, entity_id: u64) -> Option<Entity> {
    let world = app.world_mut();
    let mut query = world.query::<(Entity, &Body)>();
    query
        .iter(world)
        .find(|(_, body)| body.0 == entity_id)
        .map(|(entity, _)| entity)
}

fn name_plate_of(app: &mut App, entity_id: u64) -> Option<(Entity, String)> {
    let world = app.world_mut();
    let mut query = world.query::<(Entity, &NamePlate, &Text)>();
    query
        .iter(world)
        .find(|(_, plate, _)| plate.0 == entity_id)
        .map(|(entity, _, text)| (entity, text.0.clone()))
}

/// Every drawn part of one body, sorted by part so a failure reads the same way twice.
fn parts_of(
    app: &mut App,
    entity_id: u64,
) -> Vec<(BodyPiece, Handle<Mesh>, Handle<StandardMaterial>)> {
    let world = app.world_mut();
    let mut owners = world.query::<(&Body, &Children)>();
    let children: Vec<Entity> = owners
        .iter(world)
        .find(|(body, _)| body.0 == entity_id)
        .map(|(_, children)| children.iter().collect())
        .unwrap_or_default();

    let mut parts = world.query::<(&BodyVisual, &Mesh3d, &MeshMaterial3d<StandardMaterial>)>();
    let mut found: Vec<(BodyPiece, Handle<Mesh>, Handle<StandardMaterial>)> = children
        .into_iter()
        .filter_map(|child| parts.get(world, child).ok())
        .map(|(visual, mesh, material)| (visual.0, mesh.0.clone(), material.0.clone()))
        .collect();
    found.sort_by_key(|(part, _, _)| format!("{part:?}"));
    found
}

fn armour_of(
    app: &mut App,
    entity_id: u64,
) -> Vec<(ArmourSegment, Handle<Mesh>, Handle<StandardMaterial>)> {
    let world = app.world_mut();
    let mut owners = world.query::<(&Body, &Children)>();
    let children: Vec<Entity> = owners
        .iter(world)
        .find(|(body, _)| body.0 == entity_id)
        .map(|(_, children)| children.iter().collect())
        .unwrap_or_default();

    let mut overlays = world.query::<(&ArmourVisual, &Mesh3d, &MeshMaterial3d<StandardMaterial>)>();
    let mut found: Vec<_> = children
        .into_iter()
        .filter_map(|child| overlays.get(world, child).ok())
        .map(|(visual, mesh, material)| (visual.0, mesh.0.clone(), material.0.clone()))
        .collect();
    found.sort_by_key(|(piece, _, _)| format!("{piece:?}"));
    found
}

fn stats(app: &App) -> PlayerStats {
    *app.world().resource::<PlayerStats>()
}

/// Every body the module has spawned, as (entity id, translation), sorted so a failure
/// reads cleanly.
fn bodies(app: &mut App) -> Vec<(u64, Vec3)> {
    let world = app.world_mut();
    let mut query = world.query::<(&Body, &Transform)>();
    let mut found: Vec<(u64, Vec3)> = query
        .iter(world)
        .map(|(body, transform)| (body.0, transform.translation))
        .collect();
    found.sort_by_key(|(id, _)| *id);
    found
}

/// Where one body's local up axis points after its snapshot pose is composed.
fn body_up_axis(app: &mut App, entity_id: u64) -> Vec3 {
    let world = app.world_mut();
    let mut query = world.query::<(&Body, &Transform)>();
    query
        .iter(world)
        .find(|(body, _)| body.0 == entity_id)
        .map(|(_, transform)| transform.rotation * Vec3::Y)
        .unwrap_or_else(|| panic!("entity {entity_id} has no body"))
}

fn piece_transform(app: &mut App, entity_id: u64, piece: BodyPiece) -> Transform {
    let world = app.world_mut();
    let mut owners = world.query::<(&Body, &Children)>();
    let children: Vec<Entity> = owners
        .iter(world)
        .find(|(body, _)| body.0 == entity_id)
        .map(|(_, children)| children.iter().collect())
        .unwrap_or_else(|| panic!("entity {entity_id} has no body"));
    let mut pieces = world.query::<(&BodyVisual, &Transform)>();
    children
        .into_iter()
        .filter_map(|child| pieces.get(world, child).ok())
        .find(|(visual, _)| visual.0 == piece)
        .map(|(_, transform)| *transform)
        .unwrap_or_else(|| panic!("entity {entity_id} has no {piece:?}"))
}

fn armour_transform(app: &mut App, entity_id: u64, segment: ArmourSegment) -> Transform {
    let world = app.world_mut();
    let mut owners = world.query::<(&Body, &Children)>();
    let children: Vec<Entity> = owners
        .iter(world)
        .find(|(body, _)| body.0 == entity_id)
        .map(|(_, children)| children.iter().collect())
        .unwrap_or_else(|| panic!("entity {entity_id} has no body"));
    let mut segments = world.query::<(&ArmourVisual, &Transform)>();
    children
        .into_iter()
        .filter_map(|child| segments.get(world, child).ok())
        .find(|(visual, _)| visual.0 == segment)
        .map(|(_, transform)| *transform)
        .unwrap_or_else(|| panic!("entity {entity_id} has no {segment:?} armour segment"))
}

fn shield_transform(app: &mut App, entity_id: u64) -> Transform {
    let world = app.world_mut();
    let mut owners = world.query::<(&Body, &Children)>();
    let children: Vec<Entity> = owners
        .iter(world)
        .find(|(body, _)| body.0 == entity_id)
        .map(|(_, children)| children.iter().collect())
        .unwrap_or_else(|| panic!("entity {entity_id} has no body"));
    let mut shields = world.query::<(&ShieldVisual, &Transform)>();
    children
        .into_iter()
        .find_map(|child| shields.get(world, child).ok())
        .map(|(_, transform)| *transform)
        .unwrap_or_else(|| panic!("entity {entity_id} has no shield"))
}

fn child_count(app: &mut App, entity_id: u64) -> usize {
    let world = app.world_mut();
    let mut owners = world.query::<(&Body, &Children)>();
    owners
        .iter(world)
        .find(|(body, _)| body.0 == entity_id)
        .map_or(0, |(_, children)| children.len())
}

/// Advances past the complete death curve without hitting virtual time's delta clamp.
fn finish_body_fall(app: &mut App) {
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
        100,
    )));
    for _ in 0..12 {
        app.update();
    }
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(1)));
}

fn camera_transform(app: &mut App) -> Transform {
    let world = app.world_mut();
    let mut query = world.query_filtered::<&Transform, With<camera::WorldCamera>>();
    let found: Vec<Transform> = query.iter(world).copied().collect();
    assert_eq!(found.len(), 1, "exactly one camera owns the window");
    found[0]
}

// ---------------------------------------------------------------------------
// Bodies
// ---------------------------------------------------------------------------

#[test]
fn the_plugin_registers_its_resources_and_one_camera_without_a_display() {
    let mut app = headless_player();
    app.update();

    let world = app.world();
    assert!(world.contains_resource::<LookState>());
    assert!(world.contains_resource::<MoveIntent>());
    assert_eq!(*world.resource::<InputMode>(), InputMode::Playing);
    assert!(world.contains_resource::<SnapshotBuffer>());
    assert!(world.contains_resource::<PlayerStats>());
    assert!(world.contains_resource::<PlayerVisuals>());

    let world = app.world_mut();
    assert_eq!(
        world
            .query_filtered::<Entity, With<camera::WorldCamera>>()
            .iter(world)
            .count(),
        1
    );
}

#[test]
fn a_snapshot_places_the_local_player_exactly_where_the_server_says() {
    // The heart of it: the transform is the authoritative position, not a rounding of it
    // and not a guess near it. One snapshot means there is no segment to interpolate along,
    // so the value is exact and can be asserted as such.
    let mut app = headless_player();
    deliver(
        &mut app,
        1,
        vec![state(LOCAL_ID, [1.5, 64.0, -2.5], 0.0)],
        Instant::now(),
    );
    app.update();

    assert_eq!(
        bodies(&mut app),
        vec![(LOCAL_ID, Vec3::new(1.5, 64.0, -2.5))]
    );
    assert_eq!(
        stats(&app).position,
        Some(Vec3::new(1.5, 64.0, -2.5)),
        "the overlay reports the server's answer"
    );
    assert_eq!(stats(&app).server_tick, Some(1));
}

#[test]
fn the_local_player_is_drawn_like_everybody_else_and_hidden_while_it_is_the_eye() {
    // **This assertion is inverted from what it was, on purpose** — see #172. The local
    // player used to get no mesh and no children at all, because the camera sat at its
    // eyes and a body there fills the screen with the inside of its own head. It is now
    // built from the same rig as everybody else and simply hidden while the camera is
    // still the eye, which is what lets the third-person view have something to look at
    // without a second spawn path to keep in step.
    let mut app = headless_player();
    deliver(
        &mut app,
        1,
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(99, [4.0, 64.0, 0.0], 0.0),
        ],
        Instant::now(),
    );
    app.update();

    let world = app.world_mut();
    let mut query = world.query::<(&Body, Option<&Children>)>();
    let mut drawn: Vec<(u64, bool)> = query
        .iter(world)
        .map(|(body, children)| (body.0, children.is_some_and(|drawn| !drawn.is_empty())))
        .collect();
    drawn.sort_by_key(|(id, _)| *id);
    assert_eq!(
        drawn,
        vec![(LOCAL_ID, true), (99, true)],
        "both bodies are drawn from parts"
    );

    assert_eq!(
        local_visibility(&mut app),
        Visibility::Hidden,
        "the client starts in first person, where the body is inside the camera"
    );
}

#[test]
fn every_body_root_carries_visibility_for_its_rendered_children() {
    let mut app = headless_player();
    deliver(
        &mut app,
        1,
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(99, [4.0, 64.0, 0.0], 0.0),
        ],
        Instant::now(),
    );
    app.update();

    let world = app.world_mut();
    let mut roots = world.query::<(&Body, &Visibility, &Children)>();
    let mut visibility: Vec<_> = roots
        .iter(world)
        .map(|(body, visibility, children)| {
            assert!(
                !children.is_empty(),
                "body {} has no rendered children",
                body.0
            );
            (body.0, *visibility)
        })
        .collect();
    visibility.sort_by_key(|(id, _)| *id);
    assert_eq!(
        visibility,
        vec![(LOCAL_ID, Visibility::Hidden), (99, Visibility::Inherited),],
        "a rendered child under a root without visibility triggers Bevy B0004"
    );
}

/// What the local body's own `Visibility` says. Its own, not the computed one: there is no
/// render app here to propagate inheritance, and the value this client writes is the thing
/// under test.
fn local_visibility(app: &mut App) -> Visibility {
    let world = app.world_mut();
    let mut query = world.query_filtered::<&Visibility, With<LocalPlayer>>();
    let found: Vec<Visibility> = query.iter(world).copied().collect();
    assert_eq!(found.len(), 1, "exactly one body is this session's own");
    found[0]
}

#[derive(Debug, Clone)]
struct DrawnBodyItem {
    entity: Entity,
    item: BodyHeldItem,
    parent: Entity,
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
    transform: Transform,
    visibility: Visibility,
}

/// The one item parented to the local body's right fist, if its selected slot is full.
fn body_held_item(app: &mut App) -> Vec<DrawnBodyItem> {
    let world = app.world_mut();
    let entities: Vec<Entity> = world
        .query_filtered::<Entity, With<BodyHeldItem>>()
        .iter(world)
        .collect();
    let world = app.world();
    entities
        .into_iter()
        .map(|entity| DrawnBodyItem {
            entity,
            item: *world.get::<BodyHeldItem>(entity).expect("the held item"),
            parent: world
                .get::<ChildOf>(entity)
                .expect("the body-held item has a parent")
                .parent(),
            mesh: world
                .get::<Mesh3d>(entity)
                .expect("the held mesh")
                .0
                .clone(),
            material: world
                .get::<MeshMaterial3d<StandardMaterial>>(entity)
                .expect("the held material")
                .0
                .clone(),
            transform: *world.get::<Transform>(entity).expect("the held transform"),
            visibility: *world
                .get::<Visibility>(entity)
                .expect("the held visibility"),
        })
        .collect()
}

fn view_model_visibility(app: &mut App) -> Visibility {
    let world = app.world_mut();
    let mut query = world.query_filtered::<&Visibility, With<hands::HeldItem>>();
    *query.single(world).expect("one first-person view model")
}

fn transformed_mesh_bounds(mesh: &Mesh, transform: Transform, parent: Vec3) -> (Vec3, Vec3) {
    let Some(VertexAttributeValues::Float32x3(positions)) =
        mesh.attribute(Mesh::ATTRIBUTE_POSITION)
    else {
        panic!("the body-held mesh must carry Float32x3 positions");
    };
    positions
        .iter()
        .map(|position| transform.transform_point(Vec3::from_array(*position)) + parent)
        .fold((Vec3::MAX, Vec3::MIN), |(low, high), position| {
            (low.min(position), high.max(position))
        })
}

/// One axis-aligned bound per triangle of a transformed mesh.
///
/// A single bound around the whole sword is no longer a sound proxy for it. The grip and
/// the pommel are closed *inside* the fist while the guard and the blade are forward of it,
/// and those two halves are separated from the rig on different axes — so one box drawn
/// around both reports an overlap that neither half has. A bound per triangle keeps the
/// halves apart, and it is sound in general: a triangle whose bound is disjoint from a box
/// is disjoint from that box. It is exact here as well, because the attachment turns the
/// model sheet by a quarter turn and nothing else, which maps every axis-aligned face of it
/// to another axis-aligned face.
fn transformed_triangle_bounds(
    mesh: &Mesh,
    transform: Transform,
    parent: Vec3,
) -> Vec<(Vec3, Vec3)> {
    let Some(VertexAttributeValues::Float32x3(positions)) =
        mesh.attribute(Mesh::ATTRIBUTE_POSITION)
    else {
        panic!("the body-held mesh must carry Float32x3 positions");
    };
    let points: Vec<Vec3> = positions
        .iter()
        .map(|position| transform.transform_point(Vec3::from_array(*position)) + parent)
        .collect();
    let indices: Vec<usize> = mesh
        .indices()
        .expect("the body-held mesh must be indexed")
        .iter()
        .collect();
    indices
        .chunks_exact(3)
        .map(|triangle| {
            triangle
                .iter()
                .fold((Vec3::MAX, Vec3::MIN), |(low, high), index| {
                    (low.min(points[*index]), high.max(points[*index]))
                })
        })
        .collect()
}

fn body_piece_bounds(pieces: &[BodyPiece]) -> (Vec3, Vec3) {
    pieces
        .iter()
        .flat_map(|piece| {
            piece_boxes(*piece, ANY_HAIR)
                .iter()
                .map(|cell| placed_box(piece.part(), *cell))
        })
        .fold((Vec3::MAX, Vec3::MIN), |(low, high), placed| {
            (
                low.min(placed.centre - placed.size / 2.0),
                high.max(placed.centre + placed.size / 2.0),
            )
        })
}

/// Fraction of a real model-sheet segment that falls outside one rectangular body
/// silhouette. `horizontal_axis` is X for the front view and Z for the side view; Y is
/// vertical in both. Sampling the whole segment prevents one protruding endpoint from
/// standing in for a readable carried item.
fn projected_outside_fraction(
    segment: [Vec3; 2],
    silhouette: (Vec3, Vec3),
    horizontal_axis: usize,
) -> f32 {
    const SAMPLES: usize = 201;
    let [start, end] = segment;
    let (low, high) = silhouette;
    let outside = (0..SAMPLES)
        .filter(|sample| {
            let fraction = *sample as f32 / (SAMPLES - 1) as f32;
            let point = start.lerp(end, fraction);
            point[horizontal_axis] < low[horizontal_axis]
                || point[horizontal_axis] > high[horizontal_axis]
                || point.y < low.y
                || point.y > high.y
        })
        .count();
    outside as f32 / SAMPLES as f32
}

#[test]
fn the_local_body_holds_the_authoritative_selected_item_at_world_scale() {
    let mut app = headless_player();
    *app.world_mut().resource_mut::<Inventory>() =
        Inventory::from_stacks(vec![crate::net::InventoryStack {
            item_id: combat::ITEM_RUSTY_SWORD,
            count: 1,
            ..Default::default()
        }]);
    *app.world_mut().resource_mut::<ViewMode>() = ViewMode::ThirdPerson;
    deliver(
        &mut app,
        1,
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(99, [4.0, 64.0, 0.0], 0.0),
        ],
        Instant::now(),
    );
    app.update();

    let drawn = body_held_item(&mut app);
    assert_eq!(
        drawn.len(),
        1,
        "only the local body knows its selected slot"
    );
    let drawn = &drawn[0];
    assert_eq!(drawn.item.item_id, combat::ITEM_RUSTY_SWORD);
    assert_eq!(drawn.item.shape, ItemShape::Blade);
    let parent_visual = app
        .world()
        .get::<BodyVisual>(drawn.parent)
        .expect("the item is attached to a body piece");
    assert_eq!(parent_visual.0, BodyPiece::RightFist);
    let parent = BodyPiece::RightFist.pivot();
    let grip = drawn.transform.transform_point(drops::blade_grip_centre()) + parent;
    let (fist_low, fist_high) = body_piece_bounds(&[BodyPiece::RightFist]);
    let inside_fist = |point: Vec3| {
        (0..3).all(|axis| point[axis] >= fist_low[axis] && point[axis] <= fist_high[axis])
    };
    assert!(
        inside_fist(grip),
        "the carried sword's grip {grip:?} left the fist {fist_low:?}..{fist_high:?}"
    );
    // Both axis mappings of the one rotation, pinned separately, because the pose they
    // produce would look plausible with either of them wrong.
    //
    // The sword's own +Y is its length, tip end, and **forward is -Z**: `placed_in_layer`
    // says so where the model sheet is read — the sheet measures forwards as +z and a body
    // faces -Z, one negation in one place — and the body entity's own rotation is
    // `Quat::from_rotation_y(state.yaw)`, so the rig's local -Z is the direction the
    // character faces at every yaw. The opposite sign is a sword coming out of its back.
    let forward = drawn.transform.rotation * Vec3::Y;
    assert!(
        forward.distance(Vec3::NEG_Z) < 1e-6,
        "the sword's length axis {forward:?} does not point where the character faces"
    );
    // The sword's own +Z spans the cross guard, which is also the blade's width axis. Up,
    // so the guard stands upright and the blade's flat is vertical — an ordinary forward
    // grip, and a crossbar rather than a point from a camera behind the character.
    let guard_axis = drawn.transform.rotation * Vec3::Z;
    assert!(
        guard_axis.distance(Vec3::Y) < 1e-6,
        "the cross guard's span {guard_axis:?} is not upright"
    );
    assert_eq!(drawn.visibility, Visibility::Inherited);

    let drop_mesh = app
        .world()
        .resource::<drops::DropVisuals>()
        .mesh_for(combat::ITEM_RUSTY_SWORD);
    assert_eq!(
        drawn.mesh, drop_mesh,
        "the body item is exactly the drop-scale world asset, not the camera-space view model"
    );

    // **And the same material, which is what puts the third-person fist on the same livery
    // as the ground.** The two surfaces are one call — `BodyHeldAssets::presentation` reaches
    // `DropVisuals::material_for` — and "it follows automatically" is a claim, not a
    // measurement, so #418 measures it. The texture is asserted present because a shared
    // material carrying no image would satisfy the equality above and draw clean steel.
    let drop_material = {
        let world = app.world_mut();
        let mut materials = world
            .remove_resource::<Assets<StandardMaterial>>()
            .expect("the material store");
        let handle = world
            .resource_mut::<drops::DropVisuals>()
            .material_for(combat::ITEM_RUSTY_SWORD, &mut materials);
        let livery = materials
            .get(&handle)
            .expect("the drop's material")
            .base_color_texture
            .clone();
        world.insert_resource(materials);
        (handle, livery)
    };
    assert_eq!(
        drawn.material, drop_material.0,
        "the body item mints its own material, so the fist and the ground can disagree"
    );
    assert!(
        drop_material.1.is_some(),
        "the rusty sword's world material carries no livery image, so the body and the \
         ground both draw clean steel"
    );
    let mesh = app
        .world()
        .resource::<Assets<Mesh>>()
        .get(&drawn.mesh)
        .expect("the body-held blade mesh");
    let (blade_low, blade_high) = transformed_mesh_bounds(mesh, drawn.transform, parent);
    let triangles = transformed_triangle_bounds(mesh, drawn.transform, parent);
    let (arm_low, arm_high) = body_piece_bounds(&[BodyPiece::RightSleeve, BodyPiece::RightFist]);

    let transform_segment =
        |segment: [Vec3; 2]| segment.map(|point| drawn.transform.transform_point(point) + parent);
    let blade = transform_segment(drops::blade_span());
    let guard = transform_segment(drops::blade_guard_span());

    // The guard's rearward face is seated on the fist's forward face — forward is -Z, so
    // that is the fist's *lowest* z — and everything of the sword behind that plane is grip
    // and pommel and has to be closed inside the fist. Seating it by the grip instead would
    // bury the guard: the fist is 0.20 blocks through and the grip only 0.082. The plane
    // itself is shared by the guard's rearward face, whose ends reach outside the fist
    // vertically by design, so the filter below is strict.
    let guard_seat = drawn.transform.transform_point(drops::blade_guard_base()) + parent;
    assert!(
        guard_seat.z <= fist_low.z + 1e-6,
        "the cross guard's rearward face {guard_seat:?} is behind the fist's forward face {:?}",
        fist_low.z
    );
    let Some(VertexAttributeValues::Float32x3(positions)) =
        mesh.attribute(Mesh::ATTRIBUTE_POSITION)
    else {
        panic!("the body-held mesh must carry Float32x3 positions");
    };
    let held: Vec<Vec3> = positions
        .iter()
        .map(|position| drawn.transform.transform_point(Vec3::from_array(*position)) + parent)
        .filter(|point| point.z > fist_low.z + 1e-4)
        .collect();
    assert!(
        !held.is_empty(),
        "no part of the sword is held inside the fist at all"
    );
    assert!(
        held.iter().copied().all(inside_fist),
        "the grip and pommel are not closed inside the fist {fist_low:?}..{fist_high:?}: \
         {:?}..{:?}",
        held.iter().copied().fold(Vec3::MAX, Vec3::min),
        held.iter().copied().fold(Vec3::MIN, Vec3::max)
    );
    // The rearmost point of the whole sword is the pommel end, because the sword points
    // along -Z. Named separately from the sweep above so the far end of what the fist is
    // meant to close on is a bound in its own right rather than one member of a filtered
    // set.
    let pommel_end = held.iter().copied().fold(Vec3::NEG_INFINITY, |far, point| {
        if point.z > far.z { point } else { far }
    });
    assert!(
        inside_fist(pommel_end),
        "the pommel end {pommel_end:?} left the fist {fist_low:?}..{fist_high:?}"
    );
    assert!(
        blade.iter().all(|point| point.z < fist_low.z),
        "the blade {blade:?} does not reach clear in front of the fist's forward face {:?}",
        fist_low.z
    );
    // And clear in front of the *body*, not merely of the hand that holds it: the envelope
    // is the whole rig including hair, and the tip is beyond its forward face.
    let envelope = body_envelope();
    let body_front = envelope.centre.z - envelope.size.z / 2.0;
    assert!(
        blade[1].z < body_front,
        "the blade tip {:?} does not reach in front of the body's forward face {body_front}",
        blade[1]
    );

    // Upright, which is the whole of what makes the guard read as a crossbar: it spends its
    // entire length on Y and has no horizontal extent at all.
    let guard_span = guard[1] - guard[0];
    assert!(
        (guard_span.y.abs() - guard_span.length()).abs() < 1e-6,
        "the cross guard {guard_span:?} is not upright"
    );
    // What `BLADE_HANG_OUTSET` used to buy, and why the forward pose needs no offset at all.
    // A yawed, hanging guard swept about 0.076 blocks either side of the hang axis and would
    // have buried its inboard tip in the tunic hem; an upright one is only `GUARD_SIZE.x`
    // thick across the body, so the sword seated on the fist's centre clears that hem with
    // nothing pushing it outboard.
    let (_, torso_high) = body_piece_bounds(&[BodyPiece::Torso]);
    assert!(
        blade_low.x > torso_high.x,
        "the sword's inboard face {:?} reaches inside the tunic hem {:?}",
        blade_low.x,
        torso_high.x
    );

    // Every rig box but the right fist, which holds the grip and pommel on purpose. Two
    // boxes are disjoint exactly when one axis separates them, which is what the three
    // axis-aligned projections answer between them — asked of each triangle of the sword
    // rather than of one box around all of them, for the reason
    // [`transformed_triangle_bounds`] gives.
    for piece in [
        BodyPiece::RightTrouser,
        BodyPiece::RightShoe,
        BodyPiece::LeftTrouser,
        BodyPiece::LeftShoe,
        BodyPiece::Torso,
        BodyPiece::RightSleeve,
        BodyPiece::LeftSleeve,
        BodyPiece::LeftFist,
        BodyPiece::HeadAndNeck,
    ] {
        let (low, high) = body_piece_bounds(&[piece]);
        let hit = triangles.iter().find(|(triangle_low, triangle_high)| {
            (0..3).all(|axis| triangle_high[axis] > low[axis] && triangle_low[axis] < high[axis])
        });
        assert!(
            hit.is_none(),
            "the carried sword {blade_low:?}..{blade_high:?} intersects {piece:?} \
             {low:?}..{high:?} with the triangle {hit:?}"
        );
    }
    // The side view is the one this pose is readable from, and there the whole blade
    // projects clear of the arm: every point of it is forward of the fist's forward face,
    // which is the arm silhouette's forward face too.
    //
    // **There is deliberately no equivalent claim for the front view, and its absence is a
    // measurement rather than an omission.** A sword pointing along -Z is end-on to a camera
    // looking along Z, so from *directly* behind the character the fist covers it and any
    // projected-visibility threshold there would be asserting something false. What makes
    // the pose read from behind is the upright guard pinned above; what makes it read at all
    // is that the third-person camera sits `BOOM_LENGTH` back and an eye height up rather
    // than on the facing axis.
    let visible_blade = projected_outside_fraction(blade, (arm_low, arm_high), 2);
    assert!(
        visible_blade >= 0.99,
        "only {:.1}% of the blade projects outside the arm in the side view",
        visible_blade * 100.0
    );
    let expected = item_linear_rgba(combat::ITEM_RUSTY_SWORD);
    let actual = app
        .world()
        .resource::<Assets<StandardMaterial>>()
        .get(&drawn.material)
        .expect("the body-held material")
        .base_color;
    assert_eq!(
        actual,
        Color::linear_rgba(expected[0], expected[1], expected[2], expected[3])
    );
}

#[test]
fn only_blades_receive_the_body_attachment_rotation() {
    let anchor = body_held_item_anchor() - BodyPiece::RightFist.pivot();
    for shape in ItemShape::ALL {
        let transform = body_held_item_transform(shape);
        if shape == ItemShape::Blade {
            assert_ne!(transform.rotation, Quat::IDENTITY);
            assert_ne!(transform.translation, anchor);
        } else {
            assert_eq!(transform.rotation, Quat::IDENTITY, "{shape:?} rotated");
            assert_eq!(transform.translation, anchor, "{shape:?} moved");
        }
    }
}

#[test]
fn the_body_item_changes_blade_block_blade_in_place_then_clears() {
    let mut app = headless_player();
    *app.world_mut().resource_mut::<Inventory>() = Inventory::from_stacks(vec![
        crate::net::InventoryStack {
            item_id: combat::ITEM_RUSTY_SWORD,
            count: 1,
            ..Default::default()
        },
        crate::net::InventoryStack {
            item_id: items::ITEM_STONE,
            count: 1,
            ..Default::default()
        },
        crate::net::InventoryStack {
            item_id: crafting::ITEM_IRON_SWORD,
            count: 1,
            ..Default::default()
        },
        crate::net::InventoryStack::default(),
    ]);
    *app.world_mut().resource_mut::<ViewMode>() = ViewMode::ThirdPerson;
    deliver(
        &mut app,
        1,
        vec![state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0)],
        Instant::now(),
    );
    app.update();

    let rusty = body_held_item(&mut app);
    assert_eq!(rusty.len(), 1);
    assert_eq!(rusty[0].item.item_id, combat::ITEM_RUSTY_SWORD);
    assert_eq!(rusty[0].item.shape, ItemShape::Blade);
    assert_eq!(
        rusty[0].transform,
        body_held_item_transform(ItemShape::Blade)
    );
    let entity = rusty[0].entity;

    *app.world_mut().resource_mut::<SelectedSlot>() = SelectedSlot(1);
    app.update();
    let stone = body_held_item(&mut app);
    assert_eq!(stone.len(), 1);
    assert_eq!(
        stone[0].entity, entity,
        "a slot change updates the fist-held child in place"
    );
    assert_eq!(stone[0].item.item_id, items::ITEM_STONE);
    assert_eq!(stone[0].item.shape, ItemShape::Block);
    assert_eq!(
        stone[0].transform,
        body_held_item_transform(ItemShape::Block)
    );

    *app.world_mut().resource_mut::<SelectedSlot>() = SelectedSlot(2);
    app.update();
    let iron = body_held_item(&mut app);
    assert_eq!(iron.len(), 1);
    assert_eq!(iron[0].entity, entity);
    assert_eq!(iron[0].item.item_id, crafting::ITEM_IRON_SWORD);
    assert_eq!(iron[0].item.shape, ItemShape::Blade);
    assert_eq!(
        iron[0].transform,
        body_held_item_transform(ItemShape::Blade)
    );

    *app.world_mut().resource_mut::<SelectedSlot>() = SelectedSlot(3);
    app.update();
    assert!(
        body_held_item(&mut app).is_empty(),
        "an empty authoritative stack leaves the body hand empty"
    );
}

#[test]
fn exactly_one_held_item_renderer_owns_each_playing_view() {
    let mut app = headless_player();
    *app.world_mut().resource_mut::<Inventory>() =
        Inventory::from_stacks(vec![crate::net::InventoryStack {
            item_id: items::ITEM_STONE,
            count: 1,
            ..Default::default()
        }]);
    deliver(
        &mut app,
        1,
        vec![state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0)],
        Instant::now(),
    );
    app.update();

    assert_eq!(view_model_visibility(&mut app), Visibility::Visible);
    assert_eq!(body_held_item(&mut app)[0].visibility, Visibility::Hidden);

    *app.world_mut().resource_mut::<ViewMode>() = ViewMode::ThirdPerson;
    app.update();
    assert_eq!(view_model_visibility(&mut app), Visibility::Hidden);
    assert_eq!(
        body_held_item(&mut app)[0].visibility,
        Visibility::Inherited
    );

    *app.world_mut().resource_mut::<InputMode>() = InputMode::Chat;
    app.update();
    assert_eq!(view_model_visibility(&mut app), Visibility::Hidden);
    assert_eq!(
        body_held_item(&mut app)[0].visibility,
        Visibility::Inherited,
        "chat keeps the held-item presentation in the live world"
    );

    *app.world_mut().resource_mut::<InputMode>() = InputMode::Menu;
    app.update();
    assert_eq!(view_model_visibility(&mut app), Visibility::Hidden);
    assert_eq!(body_held_item(&mut app)[0].visibility, Visibility::Hidden);
}

/// A session, two players and an appearance for each, one frame in.
fn a_world_with_a_body(app: &mut App) {
    describe(app, LOCAL_ID, an_appearance(HairModel::Braided));
    deliver(
        app,
        1,
        vec![state(LOCAL_ID, [1.5, 64.0, -2.5], 0.0)],
        Instant::now(),
    );
    app.update();
}

/// Drags the pointer through the event `InputPlugin` accumulates, and one frame.
///
/// [`drag`] pokes `AccumulatedMouseMotion` directly, which only works in an app with no
/// `InputPlugin` — the plugin recomputes that resource in `PreUpdate` from the messages
/// below, so a poked value is gone before `sample_input` runs. Every test that needs both
/// a keyboard and a pointer has to come this way.
fn drag_with_input(app: &mut App, delta: Vec2) {
    app.world_mut().write_message(MouseMotion { delta });
    app.update();
}

/// Presses the view toggle for one frame.
///
/// Written as a `KeyboardInput` message rather than poked into `ButtonInput`, because
/// `InputPlugin` clears `just_pressed` at the start of every frame — a resource written
/// before `update()` arrives at `Update` already cleared, and the toggle is an edge. The
/// same reason `combat.rs` and `structures.rs` drive their presses this way.
fn press_toggle(app: &mut App) {
    app.world_mut().write_message(KeyboardInput {
        key_code: KeyCode::F5,
        logical_key: Key::F5,
        state: ButtonState::Pressed,
        text: None,
        repeat: false,
        window: Entity::PLACEHOLDER,
    });
    app.update();
    app.world_mut().write_message(KeyboardInput {
        key_code: KeyCode::F5,
        logical_key: Key::F5,
        state: ButtonState::Released,
        text: None,
        repeat: false,
        window: Entity::PLACEHOLDER,
    });
    app.update();
}

#[test]
fn the_toggle_shows_the_body_and_puts_the_camera_behind_it_without_respawning_anything() {
    let mut app = headless_player();
    app.add_plugins(InputPlugin);
    a_world_with_a_body(&mut app);

    let feet = Vec3::new(1.5, 64.0, -2.5);
    let eye = feet + Vec3::Y * constants::EYE_HEIGHT;
    let body = body_of(&mut app, LOCAL_ID).expect("the local player is drawn");
    let worn_before = *app
        .world()
        .get::<Worn>(body)
        .expect("the local body is dressed like every other");
    assert_eq!(camera_transform(&mut app).translation, eye, "first person");

    press_toggle(&mut app);

    assert_eq!(*app.world().resource::<ViewMode>(), ViewMode::ThirdPerson);
    assert_eq!(local_visibility(&mut app), Visibility::Inherited);
    assert_eq!(
        body_of(&mut app, LOCAL_ID),
        Some(body),
        "the toggle respawned the body instead of revealing the one that was there"
    );
    assert_eq!(
        *app.world().get::<Worn>(body).expect("still dressed"),
        worn_before,
        "the toggle undressed the body"
    );

    // Behind the eye by the whole boom, because nothing has been streamed to stop it —
    // and still looking the same way, which is what "behind" means here.
    let placed = camera_transform(&mut app);
    let back = -(placed.rotation * Vec3::NEG_Z);
    assert!(
        (placed.translation - (eye + back * constants::BOOM_LENGTH)).length() < 1e-4,
        "third person put the camera at {}",
        placed.translation
    );
    assert!(
        (placed.rotation.angle_between(Quat::IDENTITY)).abs() < 1e-4,
        "the toggle changed where the player was looking"
    );

    // And back again.
    press_toggle(&mut app);
    assert_eq!(*app.world().resource::<ViewMode>(), ViewMode::FirstPerson);
    assert_eq!(local_visibility(&mut app), Visibility::Hidden);
    assert_eq!(camera_transform(&mut app).translation, eye);
}

#[test]
fn the_local_bodys_transform_is_the_feet_position_the_snapshot_carries() {
    // The assertion #169 established for remote bodies, now that the local one is drawn
    // the same way: the parent stands on the feet and the meshes are authored from there,
    // so nothing carries an offset that could drift.
    let mut app = headless_player();
    a_world_with_a_body(&mut app);

    assert_eq!(
        bodies(&mut app),
        vec![(LOCAL_ID, Vec3::new(1.5, 64.0, -2.5))]
    );
}

#[test]
fn the_local_body_goes_when_the_snapshots_stop_naming_it() {
    // Now that it is drawn like everybody else it has to be forgotten like everybody else:
    // the world is the authority on which bodies exist, and `apply_snapshots` despawns any
    // that this tick did not name — with no exception for the local one.
    let mut app = headless_player();
    a_world_with_a_body(&mut app);
    assert!(body_of(&mut app, LOCAL_ID).is_some());

    deliver(&mut app, 2, vec![], Instant::now() + INTERVAL);
    app.update();
    app.update();

    assert_eq!(
        body_of(&mut app, LOCAL_ID),
        None,
        "the local body outlived the snapshots that named it"
    );
}

#[test]
fn holding_the_orbit_moves_the_camera_and_not_the_character() {
    // **The criterion that keeps the mode a way of looking.** `LookState::yaw` is what
    // `PlayerInput` carries, so a mouse that moved it while the orbit key was held would
    // spin the character on the server for a player who only wanted to see their own back.
    // Asserted on the look state rather than on the camera: a build that turned both would
    // pass a test that only watched the camera move.
    let mut app = headless_player();
    app.add_plugins(InputPlugin);
    a_world_with_a_body(&mut app);
    press_toggle(&mut app);

    let facing_before = *app.world().resource::<LookState>();
    let camera_before = camera_transform(&mut app).rotation;

    app.world_mut()
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::ShiftLeft);
    drag_with_input(&mut app, Vec2::new(120.0, 0.0));

    assert_eq!(
        *app.world().resource::<LookState>(),
        facing_before,
        "the orbit turned the character"
    );
    assert!(
        app.world().resource::<Orbit>().swung(),
        "the orbit did not move"
    );
    assert!(
        camera_transform(&mut app)
            .rotation
            .angle_between(camera_before)
            > 0.1,
        "the camera did not move either"
    );
}

#[test]
fn the_camera_returns_behind_the_character_and_arrives() {
    // Animated, not snapped: the first frame after release is neither where it was nor at
    // rest. And it *arrives* — a decay alone approaches zero for ever, which would leave
    // the camera fractionally off to one side for the rest of the session.
    let mut app = headless_player();
    app.add_plugins(InputPlugin);
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
        16,
    )));
    a_world_with_a_body(&mut app);
    press_toggle(&mut app);

    app.world_mut()
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::ShiftLeft);
    drag_with_input(&mut app, Vec2::new(300.0, 0.0));
    let swung = *app.world().resource::<Orbit>();
    assert!(swung.swung(), "nothing to return from");

    app.world_mut()
        .resource_mut::<ButtonInput<KeyCode>>()
        .release(KeyCode::ShiftLeft);
    app.update();

    let midway = *app.world().resource::<Orbit>();
    assert!(
        midway.swung() && midway.yaw.abs() < swung.yaw.abs(),
        "the return snapped or did not start: {swung:?} -> {midway:?}"
    );

    // One second of 16 ms frames is far more than the decay needs, and the point is that
    // it reaches rest rather than how fast.
    for _ in 0..64 {
        app.update();
    }
    assert_eq!(
        *app.world().resource::<Orbit>(),
        Orbit::default(),
        "the camera never arrived behind the character"
    );
}

#[test]
fn the_orbit_settles_when_the_view_is_left_or_the_pointer_is_taken_away() {
    // Two ways out of the orbit that are not releasing the key, both of which used to be
    // able to leave the camera swung: toggling back to first person, and a mode change
    // that stops `sample_input` reading the keyboard at all.
    for leave in [
        |app: &mut App| press_toggle(app),
        |app: &mut App| {
            *app.world_mut().resource_mut::<InputMode>() = InputMode::Menu;
            app.update();
            app.update();
        },
    ] {
        let mut app = headless_player();
        app.add_plugins(InputPlugin);
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            16,
        )));
        a_world_with_a_body(&mut app);
        press_toggle(&mut app);

        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::ShiftLeft);
        drag_with_input(&mut app, Vec2::new(300.0, 0.0));
        assert!(app.world().resource::<Orbit>().swung());

        leave(&mut app);
        for _ in 0..64 {
            app.update();
        }
        assert_eq!(*app.world().resource::<Orbit>(), Orbit::default());
    }
}

#[test]
fn third_person_originates_no_aim_and_no_request() {
    // **Both gates, and the reason they are asserted separately.** `may_act` is not
    // defined in terms of `may_aim` — it is a second expression over the same inputs — so
    // closing only the first would give a view with no crosshair and no outline in which
    // clicking still mines. Asserted through the gate rather than through the outline,
    // because the outline is presentation and the request is what the server would act on.
    let mut app = headless_player();
    app.add_plugins(InputPlugin);
    a_world_with_a_body(&mut app);

    assert_eq!(
        gates(&mut app),
        (true, true),
        "first person neither aims nor acts"
    );

    press_toggle(&mut app);
    assert_eq!(
        gates(&mut app),
        (false, false),
        "third person left a gate open — and a `may_aim` that closed alone would be the \
         worst of both: no crosshair, no outline, and clicking still mines"
    );

    press_toggle(&mut app);
    assert_eq!(gates(&mut app), (true, true), "the view came back closed");
}

/// `(may_aim, may_act)`, read the way every consumer reads them.
fn gates(app: &mut App) -> (bool, bool) {
    fn read(gate: InputGate<'_>, mut answer: ResMut<GateAnswer>) {
        *answer = GateAnswer(gate.may_aim(), gate.may_act());
    }
    app.init_resource::<GateAnswer>();
    let id = app.world_mut().register_system(read);
    // Twice, and the second answer is the one read. `may_act` carries `InputMode`'s change
    // flag, and a system that has never run sees *every* resource as changed — so a
    // one-shot's first answer is always `may_act == false`, for a reason that has nothing
    // to do with the view. The second run has a real `last_run` to compare against.
    app.world_mut().run_system(id).expect("the gate reads");
    app.world_mut().run_system(id).expect("the gate reads");
    let answer = *app.world().resource::<GateAnswer>();
    (answer.0, answer.1)
}

#[derive(Resource, Default, Clone, Copy)]
struct GateAnswer(bool, bool);

#[test]
fn there_is_still_exactly_one_camera_in_both_views() {
    // The rule in `player/camera.rs`, which this issue could have broken in the obvious
    // way. `camera_transform` asserts the count, so calling it in both views is the test.
    let mut app = headless_player();
    app.add_plugins(InputPlugin);
    a_world_with_a_body(&mut app);
    let _ = camera_transform(&mut app);
    press_toggle(&mut app);
    let _ = camera_transform(&mut app);
    press_toggle(&mut app);
    let _ = camera_transform(&mut app);
}

#[test]
fn a_body_is_drawn_from_pieces_that_each_take_their_part_colour() {
    // The acceptance criterion, part by part: head and hands the skin, torso the shirt,
    // legs the trousers, feet the shoes, hair its own — and the eyes a colour nobody
    // picked. Twelve independently moving pieces, six materials, and no piece wearing
    // another part's field. A bare body has no optional server-described overlays.
    let mut app = headless_player();
    let worn = an_appearance(HairModel::Braided);
    describe(&mut app, 99, worn);
    deliver(
        &mut app,
        1,
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(99, [4.0, 64.0, 0.0], 0.0),
        ],
        Instant::now(),
    );
    app.update();

    let drawn = parts_of(&mut app, 99);
    assert!(armour_of(&mut app, 99).is_empty());
    let mut pieces: Vec<BodyPiece> = drawn.iter().map(|(piece, _, _)| *piece).collect();
    pieces.sort_by_key(|piece| format!("{piece:?}"));
    let mut expected = BodyPiece::ALL.to_vec();
    expected.sort_by_key(|piece| format!("{piece:?}"));
    assert_eq!(pieces, expected, "every piece of the rig is drawn");

    let materials: HashSet<Handle<StandardMaterial>> = drawn
        .iter()
        .map(|(_, _, material)| material.clone())
        .collect();
    assert_eq!(
        materials.len(),
        BodyPart::IN_DRAWING_ORDER.len(),
        "six colours that differ are six materials that differ"
    );

    let meshes: HashSet<Handle<Mesh>> = drawn.iter().map(|(_, mesh, _)| mesh.clone()).collect();
    assert_eq!(meshes.len(), drawn.len(), "no two parts share geometry");
}

#[test]
fn full_iron_is_six_moving_segments_in_three_slots_that_strip_in_place() {
    let mut app = headless_player();
    let appearance = an_appearance(HairModel::Braided);
    let full_iron = [
        crafting::ITEM_IRON_HELM,
        crafting::ITEM_IRON_CUIRASS,
        crafting::ITEM_IRON_GREAVES,
        0,
    ];
    describe_wearing(&mut app, LOCAL_ID, appearance, full_iron);
    describe_wearing(&mut app, 99, appearance, full_iron);
    deliver(
        &mut app,
        1,
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(99, [4.0, 64.0, 0.0], 0.0),
        ],
        Instant::now(),
    );
    app.update();

    let body = body_of(&mut app, 99).expect("the described body is drawn");
    let remote = armour_of(&mut app, 99);
    assert_eq!(remote.len(), ArmourSegment::ALL.len());
    assert_eq!(
        remote
            .iter()
            .map(|(segment, _, _)| segment.piece())
            .collect::<HashSet<_>>(),
        ArmourPiece::ALL.into_iter().collect(),
    );
    assert_eq!(
        armour_of(&mut app, LOCAL_ID),
        remote,
        "the hidden local body and a remote body use the same overlays"
    );
    for (_, _, handle) in &remote {
        let material = app
            .world()
            .resource::<Assets<StandardMaterial>>()
            .get(handle)
            .expect("the overlay material exists");
        assert_eq!(material.perceptual_roughness, BodyFinish::Iron.roughness());
        assert_eq!(material.metallic, BodyFinish::Iron.metallic());
    }
    assert_eq!(
        child_count(&mut app, 99),
        BodyPiece::ALL.len() + ArmourSegment::ALL.len(),
    );

    describe_wearing(&mut app, 99, appearance, [0, 0, 0, 0]);
    app.update();

    assert_eq!(body_of(&mut app, 99), Some(body), "the body was respawned");
    assert!(armour_of(&mut app, 99).is_empty());
    assert_eq!(parts_of(&mut app, 99).len(), BodyPiece::ALL.len());
    assert_eq!(child_count(&mut app, 99), BodyPiece::ALL.len());

    describe_wearing(&mut app, 99, appearance, full_iron);
    app.update();
    assert_eq!(armour_of(&mut app, 99).len(), ArmourSegment::ALL.len());
    assert_eq!(
        child_count(&mut app, 99),
        BodyPiece::ALL.len() + ArmourSegment::ALL.len(),
        "re-dressing appended stale overlays to Children",
    );

    describe_wearing(&mut app, 99, appearance, [0, 0, 0, 0]);
    app.update();
    assert_eq!(child_count(&mut app, 99), BodyPiece::ALL.len());
}

#[test]
fn leather_keeps_the_existing_rough_non_metallic_finish() {
    let mut app = headless_player();
    describe_wearing(
        &mut app,
        99,
        an_appearance(HairModel::Cropped),
        [
            crafting::ITEM_LEATHER_CAP,
            crafting::ITEM_LEATHER_JERKIN,
            crafting::ITEM_LEATHER_LEGGINGS,
            0,
        ],
    );
    deliver(
        &mut app,
        1,
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(99, [4.0, 64.0, 0.0], 0.0),
        ],
        Instant::now(),
    );
    app.update();

    for (_, _, handle) in armour_of(&mut app, 99) {
        let material = app
            .world()
            .resource::<Assets<StandardMaterial>>()
            .get(&handle)
            .expect("the overlay material exists");
        assert_eq!(material.perceptual_roughness, 0.9);
        assert_eq!(material.metallic, 0.0);
    }
}

#[test]
fn actual_snapshot_motion_swings_local_and_remote_limbs_by_one_path() {
    let mut app = headless_player();
    let start = Instant::now() - INTERVAL;
    let appearance = an_appearance(HairModel::Braided);
    let full_iron = [
        crafting::ITEM_IRON_HELM,
        crafting::ITEM_IRON_CUIRASS,
        crafting::ITEM_IRON_GREAVES,
        0,
    ];
    describe_wearing(&mut app, LOCAL_ID, appearance, full_iron);
    describe_wearing(&mut app, 99, appearance, full_iron);
    deliver(
        &mut app,
        1,
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(99, [4.0, 64.0, 0.0], 0.0),
        ],
        start,
    );
    app.update();

    deliver(
        &mut app,
        2,
        vec![
            state(LOCAL_ID, [0.3, 64.0, 0.0], 0.0),
            state(99, [4.3, 64.0, 0.0], 0.0),
        ],
        Instant::now() - INTERVAL / 2,
    );
    app.update();

    let local = piece_transform(&mut app, LOCAL_ID, BodyPiece::LeftTrouser);
    let remote = piece_transform(&mut app, 99, BodyPiece::LeftTrouser);
    assert_ne!(
        local.rotation,
        Quat::IDENTITY,
        "the local leg did not swing"
    );
    assert!(
        local.rotation.abs_diff_eq(remote.rotation, 1e-4),
        "identical authoritative travel produced different local and remote strides"
    );

    let right_leg = piece_transform(&mut app, 99, BodyPiece::RightTrouser);
    let left_arm = piece_transform(&mut app, 99, BodyPiece::LeftSleeve);
    let right_arm = piece_transform(&mut app, 99, BodyPiece::RightSleeve);
    assert!(
        local.rotation.abs_diff_eq(right_arm.rotation, 1e-4),
        "the opposite arm did not counter-swing with the leg"
    );
    assert!(
        right_leg.rotation.abs_diff_eq(left_arm.rotation, 1e-4),
        "the other arm and leg are not paired"
    );
    assert_ne!(
        local.rotation, right_leg.rotation,
        "both legs swung together"
    );

    for segment in [
        ArmourSegment::LeftSleeve,
        ArmourSegment::RightSleeve,
        ArmourSegment::LeftGreave,
        ArmourSegment::RightGreave,
    ] {
        assert_eq!(
            armour_transform(&mut app, LOCAL_ID, segment),
            piece_transform(&mut app, LOCAL_ID, segment.body_piece()),
            "local {segment:?} did not follow its animated body pivot",
        );
        assert_eq!(
            armour_transform(&mut app, 99, segment),
            piece_transform(&mut app, 99, segment.body_piece()),
            "remote {segment:?} did not follow its animated body pivot",
        );
    }
}

#[test]
fn a_remote_shield_and_left_arm_follow_authoritative_blocking_players() {
    let mut app = headless_player();
    describe_wearing(
        &mut app,
        99,
        an_appearance(HairModel::Cropped),
        [0, 0, 0, crafting::ITEM_WOODEN_SHIELD],
    );
    let now = Instant::now();
    app.world_mut().resource_mut::<SnapshotInbox>().push(
        Snapshot {
            server_tick: 1,
            entities: vec![
                state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
                state(99, [4.0, 64.0, 0.0], 0.0),
            ],
            blocking_players: vec![99],
            ..Default::default()
        },
        now,
    );
    app.update();

    assert_eq!(shield_transform(&mut app, 99), shield_pose(true));
    assert_eq!(
        piece_transform(&mut app, 99, BodyPiece::LeftSleeve).rotation,
        Quat::from_rotation_x(-1.05),
        "the blocking body did not raise its left arm"
    );

    deliver(
        &mut app,
        2,
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(99, [4.0, 64.0, 0.0], 0.0),
        ],
        now + INTERVAL,
    );
    app.update();
    assert_eq!(shield_transform(&mut app, 99), shield_pose(false));
    assert_eq!(
        piece_transform(&mut app, 99, BodyPiece::LeftSleeve),
        resting_piece_transform(BodyPiece::LeftSleeve)
    );
}

#[test]
fn a_body_that_covers_no_horizontal_ground_keeps_its_limbs_at_rest() {
    let mut app = headless_player();
    let start = Instant::now() - INTERVAL;
    deliver(
        &mut app,
        1,
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(99, [4.0, 64.0, 0.0], 0.0),
        ],
        start,
    );
    app.update();
    deliver(
        &mut app,
        2,
        vec![
            state(LOCAL_ID, [0.0, 70.0, 0.0], 0.0),
            state(99, [4.0, 64.0, 0.0], 0.0),
        ],
        Instant::now() - INTERVAL / 2,
    );
    app.update();

    for id in [LOCAL_ID, 99] {
        for piece in [
            BodyPiece::LeftTrouser,
            BodyPiece::RightTrouser,
            BodyPiece::LeftSleeve,
            BodyPiece::RightSleeve,
        ] {
            assert_eq!(
                piece_transform(&mut app, id, piece),
                resting_piece_transform(piece),
                "body {id}'s {piece:?} animated without horizontal travel"
            );
        }
    }
}

#[test]
fn death_clears_a_mid_stride_pose_before_the_body_falls() {
    let mut app = headless_player();
    let start = Instant::now() - INTERVAL;
    deliver(
        &mut app,
        1,
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(99, [4.0, 64.0, 0.0], 0.0),
        ],
        start,
    );
    app.update();
    deliver(
        &mut app,
        2,
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(99, [4.3, 64.0, 0.0], 0.0),
        ],
        Instant::now() - INTERVAL / 2,
    );
    app.update();
    assert_ne!(
        piece_transform(&mut app, 99, BodyPiece::LeftTrouser).rotation,
        Quat::IDENTITY,
        "the fixture never reached mid-stride"
    );

    app.world_mut().resource_mut::<SnapshotInbox>().push(
        Snapshot {
            server_tick: 3,
            entities: vec![
                state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
                state(99, [4.6, 64.0, 0.0], 0.0),
            ],
            dead_players: vec![99],
            ..Default::default()
        },
        Instant::now() - INTERVAL / 2,
    );
    app.update();

    for piece in [
        BodyPiece::LeftTrouser,
        BodyPiece::RightTrouser,
        BodyPiece::LeftSleeve,
        BodyPiece::RightSleeve,
    ] {
        assert_eq!(
            piece_transform(&mut app, 99, piece),
            resting_piece_transform(piece),
            "{piece:?} kept walking while the body fell"
        );
    }
    finish_body_fall(&mut app);
    assert!(body_up_axis(&mut app, 99).z > 0.99);
}

#[test]
fn every_body_shares_the_geometry_and_two_in_the_same_clothes_share_the_material() {
    // The cost criterion. The meshes are built once at startup and never again, so two
    // players are two sets of handles to one set of meshes; the materials are keyed on the
    // colour itself, so twenty players in view are not a hundred materials.
    let mut app = headless_player();
    let worn = an_appearance(HairModel::Cropped);
    describe(&mut app, 1, worn);
    describe(&mut app, 2, worn);
    deliver(
        &mut app,
        1,
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(1, [2.0, 64.0, 0.0], 0.0),
            state(2, [4.0, 64.0, 0.0], 0.0),
        ],
        Instant::now(),
    );
    app.update();

    assert_eq!(
        parts_of(&mut app, 1),
        parts_of(&mut app, 2),
        "the same character twice is the same handles twice"
    );

    // And a different shirt is a different material, or the cache would be answering the
    // wrong question — one material for everybody rather than one per colour.
    let other = Appearance::new(
        A_SKIN,
        0x0000_7F3F,
        A_TROUSERS,
        A_SHOES,
        HairModel::Cropped,
        A_HAIR,
    )
    .expect("green is a colour");
    describe(&mut app, 2, other);
    app.update();

    let one = parts_of(&mut app, 1);
    let two = parts_of(&mut app, 2);
    let shirt = |drawn: &[(BodyPiece, Handle<Mesh>, Handle<StandardMaterial>)]| {
        drawn
            .iter()
            .find(|(piece, _, _)| piece.part() == BodyPart::Shirt)
            .map(|(_, _, material)| material.clone())
            .expect("a body has a shirt")
    };
    assert_ne!(shirt(&one), shirt(&two), "two shirts, two materials");
    assert_eq!(
        one.iter()
            .find(|(piece, _, _)| piece.part() == BodyPart::Skin),
        two.iter()
            .find(|(piece, _, _)| piece.part() == BodyPart::Skin),
        "and the skin they still share is still one material"
    );
}

#[test]
fn an_appearance_that_arrives_late_dresses_the_body_that_is_already_there() {
    // **The criterion that says never popped out and re-spawned.** The two streams are not
    // ordered against each other, so a player is sometimes visible before the message
    // describing them lands. The body is drawn in the documented placeholder and updated in
    // place — same entity, same children, same transform, different colours.
    let mut app = headless_player();
    let start = Instant::now();
    deliver(
        &mut app,
        1,
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(99, [4.0, 64.0, 0.0], 0.0),
        ],
        start,
    );
    app.update();

    let entity = body_of(&mut app, 99).expect("the entity is drawn before it is described");
    let children: Vec<Entity> = {
        let world = app.world_mut();
        let mut query = world.query::<&Children>();
        query
            .get(world, entity)
            .expect("a drawn body has parts")
            .iter()
            .collect()
    };
    let grey = parts_of(&mut app, 99);
    let placeholder = |part: BodyPart| {
        grey.iter()
            .find(|(drawn, _, _)| drawn.part() == part)
            .map(|(_, _, material)| material.clone())
            .expect("every part is drawn")
    };
    assert_eq!(
        placeholder(BodyPart::Skin),
        placeholder(BodyPart::Shirt),
        "the placeholder is one grey for every worn part"
    );

    describe(&mut app, 99, an_appearance(HairModel::Topknot));
    deliver(
        &mut app,
        2,
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(99, [4.0, 64.0, 0.0], 0.0),
        ],
        start + INTERVAL,
    );
    app.update();

    assert_eq!(
        body_of(&mut app, 99),
        Some(entity),
        "the body was dressed, not replaced"
    );
    let world = app.world_mut();
    let mut query = world.query::<&Children>();
    let after: Vec<Entity> = query
        .get(world, entity)
        .expect("a dressed body still has parts")
        .iter()
        .collect();
    assert_eq!(after, children, "and neither were its parts");

    let dressed = parts_of(&mut app, 99);
    assert_ne!(
        dressed, grey,
        "the appearance that arrived is the one drawn"
    );
}

#[test]
fn only_a_described_remote_body_gets_a_fixed_size_name_plate() {
    let mut app = headless_player();
    describe_as(
        &mut app,
        LOCAL_ID,
        "This session",
        an_appearance(HairModel::Braided),
    );
    describe_as(&mut app, 99, "Astrid", an_appearance(HairModel::Topknot));
    deliver_party(
        &mut app,
        1,
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(98, [2.0, 64.0, 0.0], 0.0),
            state(99, [4.0, 64.0, 0.0], 0.0),
        ],
        99,
        vec![party_member(99, 70, 100, true)],
        &["Astrid"],
        Instant::now(),
    );
    app.update();

    assert!(
        name_plate_of(&mut app, LOCAL_ID).is_none(),
        "the local name is not drawn over the player's own view in either camera mode"
    );
    assert!(
        name_plate_of(&mut app, 98).is_none(),
        "a body is not labelled with a name the server has not described"
    );
    let (plate, text) = name_plate_of(&mut app, 99).expect("the described remote has a plate");
    assert_eq!(text, "Lv 1 | Astrid");

    let world = app.world();
    let node = world.entity(plate).get::<Node>().expect("the plate is UI");
    assert_eq!(node.width, Val::Px(NAME_PLATE_WIDTH));
    assert_eq!(node.height, Val::Px(NAME_PLATE_HEIGHT));
    let font = world
        .entity(plate)
        .get::<TextFont>()
        .expect("the plate has a fixed font");
    assert_eq!(font.font_size, FontSize::Px(NAME_PLATE_FONT_SIZE));
    assert_eq!(
        world.entity(plate).get::<TextColor>().unwrap().0,
        PARTY_PLATE_COLOUR,
        "the authoritative party set tints the plate"
    );
    deliver(
        &mut app,
        2,
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(99, [4.0, 64.0, 0.0], 0.0),
        ],
        Instant::now() + INTERVAL,
    );
    app.update();
    let (plate, _) = name_plate_of(&mut app, 99).unwrap();
    assert_eq!(
        app.world().entity(plate).get::<TextColor>().unwrap().0,
        DEFAULT_PLATE_COLOUR,
        "leaving restores the ordinary plate colour on the next reconcile"
    );
}

#[test]
fn party_uses_only_accepted_snapshots_and_clears_without_a_session() {
    let mut app = headless_player();
    let start = Instant::now();
    describe_as(&mut app, 99, "Eivor", an_appearance(HairModel::Cropped));
    deliver_party(
        &mut app,
        2,
        vec![state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0)],
        LOCAL_ID,
        vec![party_member(99, 75, 100, true)],
        &["Eivor"],
        start,
    );
    app.update();
    assert_eq!(app.world().resource::<Party>().members[0].entity_id, 99);
    assert_eq!(
        app.world_mut().resource_mut::<PartyLogInbox>().take(),
        ["Eivor joined the party", "You are now the party leader"]
    );

    deliver_party(&mut app, 1, vec![], 0, vec![], &[], start + INTERVAL);
    app.update();
    assert_eq!(app.world().resource::<Party>().members[0].entity_id, 99);
    assert!(
        app.world_mut()
            .resource_mut::<PartyLogInbox>()
            .take()
            .is_empty()
    );

    app.world_mut().remove_resource::<Session>();
    app.update();
    assert_eq!(*app.world().resource::<Party>(), Party::default());

    app.insert_resource(session());
    deliver_party(
        &mut app,
        1,
        vec![state(LOCAL_ID, [1.0, 64.0, 0.0], 0.0)],
        99,
        vec![party_member(99, 50, 100, true)],
        &["Eivor"],
        start + INTERVAL * 2,
    );
    app.update();
    assert_eq!(
        app.world().resource::<Party>().roster[0].name,
        "Eivor",
        "the reconnect inherited the previous session's tick ordering"
    );
}

#[test]
fn a_description_that_arrives_late_adds_a_plate_without_replacing_the_body() {
    let mut app = headless_player();
    deliver(
        &mut app,
        1,
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(99, [4.0, 64.0, 0.0], 0.0),
        ],
        Instant::now(),
    );
    app.update();

    let body = body_of(&mut app, 99).expect("the undescribed body is drawn");
    assert!(name_plate_of(&mut app, 99).is_none());

    describe_as(&mut app, 99, "Bjorn", an_appearance(HairModel::Cropped));
    app.update();

    assert_eq!(body_of(&mut app, 99), Some(body));
    assert_eq!(
        name_plate_of(&mut app, 99).map(|(_, text)| text),
        Some("Lv 1 | Bjorn".to_owned())
    );

    let (plate, _) = name_plate_of(&mut app, 99).expect("the late description added a plate");
    describe_as_level(&mut app, 99, "Ragnar", an_appearance(HairModel::Cropped), 7);
    app.update();

    assert_eq!(body_of(&mut app, 99), Some(body));
    assert_eq!(
        name_plate_of(&mut app, 99),
        Some((plate, "Lv 7 | Ragnar".to_owned())),
        "a new description rewrites the existing plate without replacing either entity"
    );
}

// ---------------------------------------------------------------------------
// The people a settlement put there
// ---------------------------------------------------------------------------

/// A resident exactly as the server sends one, in the snapshot's `MobState` vector.
///
/// `Villager`, `Idle`, full health and no target are constants in `resident.state()`
/// (`server/internal/game/resident.go`), not choices this helper makes.
fn resident_state(entity_id: u64, pos: [f32; 3], yaw: f32) -> crate::net::MobState {
    crate::net::MobState {
        entity_id,
        kind: crate::net::MobKind::Villager,
        pos,
        vel: [0.0; 3],
        yaw,
        health: 100,
        max_health: 100,
        action: crate::net::MobAction::Idle,
        target_entity_id: 0,
    }
}

/// A snapshot carrying players and residents together, which is how one really arrives.
fn deliver_with_residents(
    app: &mut App,
    tick: u32,
    entities: Vec<EntityState>,
    residents: Vec<crate::net::MobState>,
    at: Instant,
) {
    app.world_mut().resource_mut::<SnapshotInbox>().push(
        Snapshot {
            server_tick: tick,
            entities,
            mobs: residents,
            ..Default::default()
        },
        at,
    );
}

/// What the rig says one body is wearing.
fn worn_appearance(app: &mut App, entity_id: u64) -> Option<Appearance> {
    let world = app.world_mut();
    let mut query = world.query::<(&Body, &Worn)>();
    query
        .iter(world)
        .find(|(body, _)| body.0 == entity_id)
        .map(|(_, worn)| worn.appearance)
}

/// Queues a resident description as the net thread would.
fn describe_resident(
    app: &mut App,
    entity_id: u64,
    name: &str,
    role: ResidentRole,
    appearance: Appearance,
) {
    app.world_mut()
        .resource_mut::<crate::net::ResidentInbox>()
        .push(crate::net::ResidentAppearance {
            entity_id,
            name: name.to_owned(),
            role,
            appearance,
        });
}

/// A villager in the snapshot is a person, with a face and a trade over their head.
///
/// The acceptance criterion end to end: the resident arrives in the `MobState` vector, is
/// drawn on the humanoid rig rather than as a creature, wears the appearance the server
/// chose, and carries a plate reading `Name | Role`.
///
/// **The separator is an ASCII pipe, and #458's own text is wrong about it.** `·` is not
/// among the ninety-five glyphs of the embedded font, so it lays out with zero advance and
/// the two fields run together with nothing between them. `|` is what this client already
/// separates fields with (`X 12 | Z -4`), chosen over a hyphen because a hyphen collides
/// with a negative coordinate.
#[test]
fn a_villager_is_drawn_as_a_person_with_a_name_and_a_trade() {
    let mut app = headless_player();
    let looks = an_appearance(HairModel::Braided);
    describe_resident(&mut app, 400, "Bjorn", ResidentRole::Smith, looks);
    deliver_with_residents(
        &mut app,
        1,
        vec![state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0)],
        vec![resident_state(400, [4.0, 64.0, 0.0], 0.0)],
        Instant::now(),
    );
    app.update();

    assert!(
        body_of(&mut app, 400).is_some(),
        "a resident was not drawn on the rig every other person is drawn on"
    );
    assert_eq!(
        name_plate_of(&mut app, 400).map(|(_, text)| text),
        Some("Bjorn | Smith".to_owned())
    );
    assert_eq!(
        worn_appearance(&mut app, 400),
        Some(looks),
        "a resident is not wearing the appearance the server chose for them"
    );
}

/// Every role reaches the plate as its own word, beside the same name.
///
/// The sweep the single case above cannot be: a plate reading `Bjorn | Smith` whichever
/// role arrived would satisfy that test exactly.
#[test]
fn a_residents_plate_names_the_trade_the_settlement_gave_them() {
    for (index, role) in EVERY_ROLE.into_iter().enumerate() {
        let mut app = headless_player();
        let entity_id = 400 + index as u64;
        describe_resident(&mut app, entity_id, "Bjorn", role, PLACEHOLDER_APPEARANCE);
        deliver_with_residents(
            &mut app,
            1,
            vec![state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0)],
            vec![resident_state(entity_id, [4.0, 64.0, 0.0], 0.0)],
            Instant::now(),
        );
        app.update();

        assert_eq!(
            name_plate_of(&mut app, entity_id).map(|(_, text)| text),
            Some(format!("Bjorn | {}", role_label(role))),
            "{role:?} did not reach the plate as its own word"
        );
    }
}

/// A resident is drawn before their description arrives, and the plate follows.
///
/// The two streams are not ordered against each other — a remote player's body follows the
/// same rule — so the body is drawn in the placeholder grey and dressed in place when the
/// description lands. Nothing pops out and respawns.
#[test]
fn a_resident_stands_in_the_placeholder_until_their_description_arrives() {
    let mut app = headless_player();
    let start = Instant::now();
    deliver_with_residents(
        &mut app,
        1,
        vec![state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0)],
        vec![resident_state(400, [4.0, 64.0, 0.0], 0.0)],
        start,
    );
    app.update();

    let body = body_of(&mut app, 400).expect("an undescribed resident is still drawn");
    assert_eq!(worn_appearance(&mut app, 400), Some(PLACEHOLDER_APPEARANCE));
    assert!(
        name_plate_of(&mut app, 400).is_none(),
        "a plate was drawn before there was a name to put on it"
    );

    let looks = an_appearance(HairModel::Topknot);
    describe_resident(&mut app, 400, "Sigrun", ResidentRole::Cook, looks);
    deliver_with_residents(
        &mut app,
        2,
        vec![state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0)],
        vec![resident_state(400, [4.0, 64.0, 0.0], 0.0)],
        start + INTERVAL,
    );
    app.update();

    assert_eq!(
        body_of(&mut app, 400),
        Some(body),
        "the late description replaced the body instead of dressing it"
    );
    assert_eq!(worn_appearance(&mut app, 400), Some(looks));
    assert_eq!(
        name_plate_of(&mut app, 400).map(|(_, text)| text),
        Some("Sigrun | Cook".to_owned())
    );
}

/// A resident never goes over, not even on the frame this session's own body does.
///
/// The criterion says never a fall pose, and what makes it true is structural: `dead_players`
/// is a list of *players*, and a resident's id is derived with bit 62 set where a player's
/// is minted from a counter, so it can never appear there. Asserted on the same frame the
/// local body is falling, because a test where nobody is dead would pass against a build
/// with no such rule at all.
#[test]
fn a_resident_never_falls_over() {
    let mut app = headless_player();
    let start = Instant::now();
    app.world_mut().resource_mut::<SnapshotInbox>().push(
        Snapshot {
            server_tick: 1,
            entities: vec![state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0)],
            mobs: vec![resident_state(400, [4.0, 64.0, 0.0], 0.0)],
            dead_players: vec![LOCAL_ID],
            ..Default::default()
        },
        start,
    );
    app.update();
    app.world_mut()
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            400,
        )));
    app.update();

    let (local_fall, resident_fall) = {
        let world = app.world_mut();
        let mut query = world.query::<(&Body, &camera::DeathFall)>();
        let found: Vec<(u64, f32)> = query
            .iter(world)
            .map(|(body, fall)| (body.0, fall.fallen()))
            .collect();
        let of = |entity_id: u64| {
            found
                .iter()
                .find(|(drawn, _)| *drawn == entity_id)
                .map(|(_, fallen)| *fallen)
        };
        (of(LOCAL_ID), of(400))
    };
    assert!(
        local_fall.is_some_and(|fallen| fallen > 0.0),
        "the dead player is not falling, so this test would pass against anything"
    );
    assert_eq!(
        resident_fall,
        Some(0.0),
        "a resident went over while somebody else died"
    );

    let upright = {
        let world = app.world_mut();
        let mut query = world.query::<(&Body, &Transform)>();
        query
            .iter(world)
            .find(|(body, _)| body.0 == 400)
            .map(|(_, transform)| transform.rotation)
    };
    assert!(
        upright.is_some_and(|rotation| rotation.abs_diff_eq(Quat::IDENTITY, 1e-5)),
        "a resident standing at yaw zero is not upright: {upright:?}"
    );
}

/// A resident who leaves the view takes their body and their plate with them.
///
/// The newest snapshot is the existence set for a resident exactly as it is for everybody
/// else, and this client does not guess why one stopped being sent. Part 4a pinned the
/// body half; the plate is a UI root rather than a body child, so it has to be removed
/// explicitly and is the half a shared despawn path would not cover.
#[test]
fn a_resident_who_leaves_the_view_takes_their_plate_with_them() {
    let mut app = headless_player();
    let start = Instant::now();
    describe_resident(
        &mut app,
        400,
        "Ivar",
        ResidentRole::Guard,
        PLACEHOLDER_APPEARANCE,
    );
    deliver_with_residents(
        &mut app,
        1,
        vec![state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0)],
        vec![resident_state(400, [4.0, 64.0, 0.0], 0.0)],
        start,
    );
    app.update();
    assert!(name_plate_of(&mut app, 400).is_some());

    deliver_with_residents(
        &mut app,
        2,
        vec![state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0)],
        vec![],
        start + INTERVAL,
    );
    app.update();

    assert!(body_of(&mut app, 400).is_none());
    assert!(
        name_plate_of(&mut app, 400).is_none(),
        "a screen-space root cannot outlive the body it labels"
    );
}

#[test]
fn a_player_who_leaves_takes_their_name_plate_with_them() {
    let mut app = headless_player();
    let start = Instant::now();
    describe_as(&mut app, 99, "Freya", an_appearance(HairModel::Loose));
    deliver(
        &mut app,
        1,
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(99, [4.0, 64.0, 0.0], 0.0),
        ],
        start,
    );
    app.update();
    assert!(name_plate_of(&mut app, 99).is_some());

    deliver(
        &mut app,
        2,
        vec![state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0)],
        start + INTERVAL,
    );
    app.update();

    assert!(body_of(&mut app, 99).is_none());
    assert!(
        name_plate_of(&mut app, 99).is_none(),
        "a screen-space root cannot outlive the body it labels"
    );
}

#[test]
fn hostile_and_unicode_names_remain_bounded_valid_single_line_text() {
    assert_eq!(name_plate_text(PlateLabel::Level(7), ""), "Lv 7 | ");
    assert_eq!(
        name_plate_text(PlateLabel::Level(7), "Sigrid\nJarl"),
        "Lv 7 | Sigrid?Jarl"
    );
    assert_eq!(
        name_plate_text(PlateLabel::Level(7), "石のᚠe\u{301}"),
        "Lv 7 | 石のᚠe\u{301}"
    );

    let long = "界".repeat(NAME_PLATE_CHARACTERS + 20);
    let shown = name_plate_text(PlateLabel::Level(u16::MAX), &long);
    assert_eq!(shown.chars().count(), NAME_PLATE_CHARACTERS);
    assert!(shown.ends_with("..."), "{shown}");

    // The mark is taken out of the bound, so the bound is what holds — for every level a
    // `u16` can carry, not only for the short prefixes a test would think to write.
    for level in [0, 9, 10, 99, 100, 9_999, 10_000, u16::MAX] {
        let shown = name_plate_text(PlateLabel::Level(level), &long);
        assert!(
            shown.chars().count() <= NAME_PLATE_CHARACTERS,
            "level {level} drew {} characters onto a {NAME_PLATE_CHARACTERS}-character plate: {shown}",
            shown.chars().count()
        );
    }

    // And the same bound with the fixed half on the other side of the name. A resident's
    // name is server-chosen and short, but nothing in this function knows that, and the
    // half that gives way must still be the name rather than the role.
    for role in EVERY_ROLE {
        let shown = name_plate_text(PlateLabel::Role(role), &long);
        assert!(
            shown.chars().count() <= NAME_PLATE_CHARACTERS,
            "{role:?} drew {} characters onto a {NAME_PLATE_CHARACTERS}-character plate: {shown}",
            shown.chars().count()
        );
        assert!(
            shown.ends_with(&format!(" | {}", role_label(role))),
            "the role was truncated off the plate instead of the name: {shown}"
        );
    }
}

/// Every role the contract names, so the sweeps below run over all of them.
///
/// Written out rather than derived, for the reason every list like it here is: one derived
/// from the same `match` it checks would agree with every hole in that `match`. Its length
/// is pinned to the contract's own count below, because a fixed-size array of enum values
/// does not stop compiling when the enum grows — the mistake `EVERY_REASON` cost twice.
const EVERY_ROLE: [ResidentRole; 6] = [
    ResidentRole::Villager,
    ResidentRole::Smith,
    ResidentRole::Carpenter,
    ResidentRole::Cook,
    ResidentRole::Trader,
    ResidentRole::Guard,
];

/// The list is complete, and every role reads as its own ASCII word.
///
/// `Unknown` is the contract's zero rather than a role — `ResidentRole::from_wire` answers
/// `None` for it and the session ends — so no plate is ever drawn from it.
///
/// ASCII is what `client/src/ui/mod.rs` fails the build over, asserted again here because
/// that scan reads *source* and would not catch a word composed at runtime. Distinctness is
/// the point of drawing the role at all: a plate reading the same over the smith and the
/// guard would say nothing.
#[test]
fn every_role_is_in_the_sweep_as_its_own_ascii_word() {
    assert_eq!(
        EVERY_ROLE.len(),
        crate::wire::voxelheim::net::ResidentRole::ENUM_VALUES.len() - 1,
        "a role the contract names is missing from EVERY_ROLE, so every sweep over it is \
         reporting on a subset while reading as if it swept them all"
    );
    for (seen, role) in EVERY_ROLE.iter().enumerate() {
        let word = role_label(*role);
        assert!(word.is_ascii() && !word.is_empty(), "{role:?} -> {word:?}");
        assert!(
            !EVERY_ROLE[..seen]
                .iter()
                .any(|other| *other == *role || role_label(*other) == word),
            "{role:?} is a duplicate of an earlier role or of its word"
        );
    }
}

#[test]
fn a_name_plate_anchor_follows_the_body_transform() {
    let at_origin = name_plate_anchor(&Transform::IDENTITY);
    let moved = Transform::from_translation(Vec3::new(3.0, 8.0, -5.0));
    assert!(
        name_plate_anchor(&moved).abs_diff_eq(at_origin + moved.translation, 1e-6),
        "the label anchor did not follow the body"
    );
}

/// What `position_name_plates` decided about one plate, before the projection has any say.
fn plate_sight_of(app: &mut App, entity_id: u64) -> Option<PlateSight> {
    let world = app.world_mut();
    let mut query = world.query::<(&NamePlate, &PlateSight)>();
    query
        .iter(world)
        .find(|(plate, _)| plate.0 == entity_id)
        .map(|(_, sight)| *sight)
}

/// A stone wall filling the plane z = 3 across the chunk the session spawns in.
///
/// Solid for the whole height of the chunk, so nothing about the test rests on where the
/// eye and the anchor sit relative to each other on the vertical.
fn a_wall_at_z3() -> crate::world::ChunkStore {
    let mut chunk = crate::world::VoxelChunk::all_air(32);
    for x in 0..32 {
        for y in 0..32 {
            chunk.set(x, y, 3, crate::world::palette::STONE);
        }
    }
    let mut store = crate::world::ChunkStore::default();
    store.insert(
        crate::net::ChunkCoord {
            cx: 0,
            cy: 2,
            cz: 0,
        },
        chunk,
    );
    store
}

/// A session, a described remote and enough frames for the hysteresis to settle.
///
/// `remote_z` is where the other body stands; the local body stays at the spawn, which is
/// where the first-person camera therefore is.
fn plates_after_settling(store: Option<crate::world::ChunkStore>, remote_z: f32) -> App {
    let mut app = headless_player();
    if let Some(store) = store {
        app.insert_resource(store);
    }
    describe_as(&mut app, 99, "Astrid", an_appearance(HairModel::Topknot));
    let mut at = Instant::now();
    for tick in 1..=(u32::from(NAME_PLATE_SIGHT_DWELL) + 3) {
        deliver(
            &mut app,
            tick,
            vec![
                state(LOCAL_ID, [0.5, 64.0, 0.5], 0.0),
                state(99, [0.5, 64.0, remote_z], 0.0),
            ],
            at,
        );
        app.update();
        at += INTERVAL;
    }
    app
}

#[test]
fn solid_terrain_between_the_camera_and_a_head_hides_the_name() {
    // The control is the half that matters. Without it this test passes for a plate that
    // was never going to be drawn — the same two points with the wall taken away have to
    // come back clear, or the geometry proved nothing.
    let eye = Vec3::new(0.5, 65.5, 0.5);
    let anchor = Vec3::new(0.5, 65.9, 8.5);

    assert!(
        !name_plate_line_is_clear(eye, anchor, |voxel| voxel.z == 4),
        "a name was readable through a wall"
    );
    assert!(
        name_plate_line_is_clear(eye, anchor, |_| false),
        "the control failed: these two points do not see each other in an empty world"
    );
}

#[test]
fn the_block_the_anchor_reaches_into_is_not_what_hides_the_name() {
    // Two people under one roof. The anchor is a hand's width above the head, so it is
    // routinely inside the ceiling block; a ray that counted the voxel it ends in would hide
    // the plate of somebody standing plainly in front of the camera, indoors.
    let eye = Vec3::new(0.5, 65.5, 0.5);
    let anchor = Vec3::new(0.5, 66.2, 4.5);
    let ceiling = anchor.floor().as_ivec3();

    assert!(
        name_plate_line_is_clear(eye, anchor, |voxel| voxel == ceiling),
        "the anchor's own voxel hid the plate it belongs to"
    );
    assert!(
        !name_plate_line_is_clear(eye, anchor, |voxel| voxel == ceiling || voxel.z == 2),
        "excluding the anchor's voxel also excused a wall in front of it"
    );
}

#[test]
fn the_two_name_plate_rules_are_independent() {
    // One test, four corners, because the criterion is that neither rule can stand in for
    // the other: near and occluded is hidden, far and clear is hidden, and only near and
    // clear is drawn.
    let eye = Vec3::new(0.5, 65.5, 0.5);
    let near = eye + Vec3::Z * 6.0;
    let far = eye + Vec3::Z * (NAME_PLATE_DISTANCE + 6.0);
    let clear = |_: IVec3| false;
    let wall = |voxel: IVec3| voxel.z == 4;

    assert!(
        name_plate_is_in_sight(eye, near, false, clear).1,
        "a body six blocks away in the open has no plate"
    );
    assert!(
        !name_plate_is_in_sight(eye, near, false, wall).1,
        "the occlusion rule did not fire on a body well inside the distance limit"
    );
    assert!(
        !name_plate_is_in_sight(eye, far, true, clear).1,
        "the distance rule did not fire on a completely clear line"
    );
    assert!(
        !name_plate_is_in_sight(eye, far, true, wall).1,
        "neither rule fired when both should have"
    );

    // And the distance answer that comes back is the distance rule's alone: a wall between
    // the two endpoints hides the plate without ever reporting the body as far away, which
    // is the property the next test spends a walk on.
    assert!(
        name_plate_is_in_sight(eye, near, false, wall).0,
        "an occluded body six blocks away was reported as out of distance"
    );
}

#[test]
fn clearing_a_wall_brings_a_name_plate_back_inside_the_hysteresis_band() {
    // The regression that reading `shown` for the threshold produced. A plate is drawn at 31
    // blocks, a wall goes up, the plate settles out — and then the wall comes down with the
    // body never having moved. Judged against the drawn state the plate would now be asked
    // for the hidden threshold of 30, and would stay off with a clear line of sight and the
    // distance rule satisfied; judged against the distance rule's own history it comes back.
    let eye = Vec3::ZERO;
    let inside = Vec3::Z * (NAME_PLATE_DISTANCE - NAME_PLATE_DISTANCE_MARGIN - 1.0);
    let anchor = Vec3::Z * (NAME_PLATE_DISTANCE - 1.0);
    assert!(
        anchor.length() > NAME_PLATE_DISTANCE - NAME_PLATE_DISTANCE_MARGIN,
        "the body has to sit inside the band for this test to be about anything"
    );

    let clear = |_: IVec3| false;
    let wall = |voxel: IVec3| voxel.z == 4;
    let mut sight = PlateSight::default();

    // Walk inside the tighter threshold before moving out into the band, so `near` is earned
    // rather than asserted. A plate that starts hidden must not appear from inside the band;
    // that is the hysteresis this regression test is meant to preserve.
    let settle =
        |sight: &mut PlateSight, anchor: Vec3, solid: &dyn Fn(IVec3) -> bool, frames: usize| {
            for _ in 0..frames {
                let (near, wanted) = name_plate_is_in_sight(eye, anchor, sight.near, solid);
                *sight = settle_plate_sight(PlateSight { near, ..*sight }, wanted);
            }
        };

    settle(
        &mut sight,
        inside,
        &clear,
        NAME_PLATE_SIGHT_DWELL as usize + 1,
    );
    assert!(sight.shown, "the plate never appeared on a clear line");

    settle(
        &mut sight,
        anchor,
        &wall,
        NAME_PLATE_SIGHT_DWELL as usize + 1,
    );
    assert!(!sight.shown, "the wall did not hide the plate");
    assert!(
        sight.near,
        "the occlusion rule moved the distance rule's own answer"
    );

    settle(
        &mut sight,
        anchor,
        &clear,
        NAME_PLATE_SIGHT_DWELL as usize + 1,
    );
    assert!(
        sight.shown,
        "the plate stayed hidden after the wall came down, {} blocks away with a clear line",
        anchor.length()
    );
}

#[test]
fn the_distance_limit_stays_inside_the_smallest_world_this_client_draws() {
    // The criterion the constant exists for: a plate must never be the thing that tells a
    // player an entity is there. Two chunks is the floor `crate::settings` puts under the
    // render distance and a chunk is 32 blocks, so 64 blocks is the nearest the fog can ever
    // be brought in — and the limit has to sit comfortably inside that, not merely under it.
    // `const`, so this one is not a test that has to be run: both are compile-time
    // invariants of the two numbers, and a build that breaks either never links.
    const {
        assert!(
            NAME_PLATE_DISTANCE <= 64.0 / 2.0,
            "the name plate limit reaches into the fog on the tightest render distance"
        );
        assert!(
            NAME_PLATE_DISTANCE_MARGIN > 0.0 && NAME_PLATE_DISTANCE_MARGIN < NAME_PLATE_DISTANCE,
            "the hysteresis band is not a band"
        );
    }
    assert_eq!(name_plate_reach(true), NAME_PLATE_DISTANCE);
    assert_eq!(
        name_plate_reach(false),
        NAME_PLATE_DISTANCE - NAME_PLATE_DISTANCE_MARGIN,
        "a hidden plate has to come further in than the limit it went out at"
    );
}

#[test]
fn a_name_plate_behind_a_fence_post_does_not_strobe() {
    // The occlusion boundary has no width to widen, so its stability is temporal: an answer
    // that alternates every frame — which is what a slow pan across a fence post produces --
    // must reach the screen as no change at all.
    let mut sight = PlateSight {
        shown: true,
        dwell: 0,
        near: true,
    };
    for frame in 0..40 {
        sight = settle_plate_sight(sight, frame % 2 == 0);
        assert!(
            sight.shown,
            "the plate went out on an alternating answer at frame {frame}"
        );
    }

    // And a change that genuinely holds still lands, on the frame the dwell names and not
    // before it. A filter that never let anything through would pass the assertion above.
    let mut sight = PlateSight {
        shown: true,
        dwell: 0,
        near: true,
    };
    for frame in 1..NAME_PLATE_SIGHT_DWELL {
        sight = settle_plate_sight(sight, false);
        assert!(sight.shown, "the plate gave up after {frame} frames");
    }
    sight = settle_plate_sight(sight, false);
    assert!(!sight.shown, "a settled answer never landed");
    assert_eq!(sight.dwell, 0, "the counter was not reset by the flip");
}

#[test]
fn a_name_plate_sitting_on_the_distance_limit_settles_once_and_stays() {
    // The other boundary and the other mechanism. A body standing exactly on the limit
    // crosses it every frame on the noise in its interpolated position alone; the band is
    // what turns that into one transition instead of one per frame.
    let eye = Vec3::ZERO;
    let mut sight = PlateSight {
        shown: true,
        dwell: 0,
        near: true,
    };
    let mut history = Vec::new();
    for frame in 0..60 {
        let jitter = if frame % 2 == 0 { -0.2 } else { 0.2 };
        let anchor = Vec3::Z * (NAME_PLATE_DISTANCE + jitter);
        let (near, wanted) = name_plate_is_in_sight(eye, anchor, sight.near, |_| false);
        sight = settle_plate_sight(PlateSight { near, ..sight }, wanted);
        history.push(sight.shown);
    }

    let flips = history.windows(2).filter(|pair| pair[0] != pair[1]).count();
    assert_eq!(
        flips, 1,
        "the plate changed {flips} times instead of settling once"
    );
    assert!(
        !history.last().copied().unwrap_or(true),
        "the plate never settled on the hidden side of the band"
    );

    // The other half, without which a filter that never let anything through would pass:
    // a body that actually walks out of range does lose its name, and does not get it back
    // inside the band.
    for step in 0..20u32 {
        let anchor = Vec3::Z * (NAME_PLATE_DISTANCE + 4.0);
        let (near, wanted) = name_plate_is_in_sight(eye, anchor, sight.near, |_| false);
        sight = settle_plate_sight(PlateSight { near, ..sight }, wanted);
        assert!(
            step < u32::from(NAME_PLATE_SIGHT_DWELL) || !sight.shown,
            "a body four blocks past the limit kept its name at step {step}"
        );
    }
    let inside_the_band = Vec3::Z * (NAME_PLATE_DISTANCE - NAME_PLATE_DISTANCE_MARGIN / 2.0);
    for _ in 0..20 {
        let (near, wanted) = name_plate_is_in_sight(eye, inside_the_band, sight.near, |_| false);
        sight = settle_plate_sight(PlateSight { near, ..sight }, wanted);
    }
    assert!(
        !sight.shown,
        "a hidden plate came back before it was all the way inside the band"
    );
}

#[test]
fn a_wall_hides_a_name_plate_and_an_empty_world_does_not() {
    // End to end through the real system, camera included, and again with the control: the
    // same two bodies with nothing between them have to keep the name.
    let mut behind_a_wall = plates_after_settling(Some(a_wall_at_z3()), 8.5);
    assert!(
        name_plate_of(&mut behind_a_wall, 99).is_some(),
        "the plate has to exist for its visibility to mean anything"
    );
    assert_eq!(
        plate_sight_of(&mut behind_a_wall, 99).map(|sight| sight.shown),
        Some(false),
        "a name was readable through a wall the server had streamed"
    );

    let mut in_the_open = plates_after_settling(None, 8.5);
    assert_eq!(
        plate_sight_of(&mut in_the_open, 99).map(|sight| sight.shown),
        Some(true),
        "the control failed: the same body is not visible with nothing in the way"
    );
}

#[test]
fn a_name_plate_past_the_limit_is_hidden_on_a_completely_clear_line() {
    // The second rule on its own: no store at all, so nothing anywhere is solid, and the
    // only thing that can hide this plate is how far away it is.
    let mut far = plates_after_settling(None, 0.5 + NAME_PLATE_DISTANCE + 8.0);
    assert!(
        name_plate_of(&mut far, 99).is_some(),
        "the plate has to exist for its visibility to mean anything"
    );
    assert_eq!(
        plate_sight_of(&mut far, 99).map(|sight| sight.shown),
        Some(false),
        "a name was legible across open ground the player cannot make anybody out over"
    );
}

#[test]
fn the_hair_a_player_chose_is_the_mesh_their_body_wears() {
    // The one part whose *shape* is chosen rather than only its colour, so it is the one
    // part where dressing a body swaps a mesh handle.
    let mut app = headless_player();
    describe(&mut app, 99, an_appearance(HairModel::Shaved));
    deliver(
        &mut app,
        1,
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(99, [4.0, 64.0, 0.0], 0.0),
        ],
        Instant::now(),
    );
    app.update();

    let hair = |app: &mut App| {
        parts_of(app, 99)
            .into_iter()
            .find(|(piece, _, _)| piece.part() == BodyPart::Hair)
            .map(|(_, mesh, _)| mesh)
            .expect("a body has hair, even shaved")
    };
    let shaved = hair(&mut app);

    describe(&mut app, 99, an_appearance(HairModel::Loose));
    app.update();

    assert_ne!(
        shaved,
        hair(&mut app),
        "a different model is a different mesh"
    );
}

#[test]
fn a_body_stands_on_the_feet_position_the_snapshot_carries() {
    // **The property the capsule had and the rig keeps.** Every part is authored from the
    // ground up, so the parent transform is the server's position exactly and the children
    // carry no offset of their own — which is what lets the camera add an eye height to the
    // same number.
    let mut app = headless_player();
    let feet = Vec3::new(4.0, 64.0, -2.0);
    describe(&mut app, 99, an_appearance(HairModel::Braided));
    deliver(
        &mut app,
        1,
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(99, feet.to_array(), 0.0),
        ],
        Instant::now(),
    );
    app.update();

    let entity = body_of(&mut app, 99).expect("the other player is drawn");
    let world = app.world_mut();
    let mut owners = world.query::<(&Transform, &Children)>();
    let (transform, children) = owners.get(world, entity).expect("a drawn body has both");
    assert_eq!(transform.translation, feet);

    let children: Vec<Entity> = children.iter().collect();
    let mut parts = world.query::<(&BodyVisual, &Transform)>();
    for child in children {
        let (visual, transform) = parts.get(world, child).expect("every child is a piece");
        assert_eq!(
            *transform,
            resting_piece_transform(visual.0),
            "a piece is resting at its authored pivot"
        );
    }
}

#[test]
fn an_entity_that_leaves_for_good_takes_its_appearance_with_it() {
    // The cache is the size of what is in view. The server drops its own record for the
    // same entity at the same moment and describes it again if it comes back, so the two
    // sides agree without either being told.
    let mut app = headless_player();
    let start = Instant::now();
    describe(&mut app, 99, an_appearance(HairModel::Braided));
    deliver(
        &mut app,
        1,
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(99, [4.0, 64.0, 0.0], 0.0),
        ],
        start,
    );
    app.update();
    assert!(app.world().resource::<Appearances>().0.contains_key(&99));

    deliver(
        &mut app,
        2,
        vec![state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0)],
        start + INTERVAL,
    );
    app.update();
    assert!(
        !app.world().resource::<Appearances>().0.contains_key(&99),
        "the entity left, and its appearance left with it"
    );

    // And it is refilled, because the server describes an entity that comes back.
    describe(&mut app, 99, an_appearance(HairModel::Loose));
    deliver(
        &mut app,
        3,
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(99, [4.0, 64.0, 0.0], 0.0),
        ],
        start + INTERVAL * 2,
    );
    app.update();
    assert!(app.world().resource::<Appearances>().0.contains_key(&99));
    assert!(body_of(&mut app, 99).is_some());
}

#[test]
fn describing_an_entity_again_does_not_restart_its_grace() {
    // **The bound belongs to this client, not to whoever is sending.** `APPEARANCE_GRACE`
    // is a grace on how long an appearance with nothing to draw it on is kept, measured
    // from when the entity was *first* described. Restarting that clock on every message
    // would hand the sender the bound: an entity that never appears in a snapshot, named
    // again inside every window, would live for as long as the connection did — and a map
    // of them would grow with it, which is the growth the grace exists to stop.
    //
    // The clock cannot be advanced from a test without spending real seconds, so what is
    // asserted is the thing the expiry reads: that the timestamp did not move, and that the
    // description did.
    let mut app = headless_player();
    let first = an_appearance(HairModel::Braided);
    describe(&mut app, 99, first);
    app.update();

    let at = {
        let cached = app.world().resource::<Appearances>();
        let described = cached.0.get(&99).expect("the appearance was cached");
        assert!(!described.drawn, "no snapshot has named this entity");
        described.at
    };

    let second = an_appearance(HairModel::Loose);
    describe(&mut app, 99, second);
    app.update();

    let cached = app.world().resource::<Appearances>();
    let described = cached.0.get(&99).expect("the appearance is still cached");
    assert_eq!(
        described.at, at,
        "a second description restarted the grace, so a sender can hold an entry for ever"
    );
    assert_eq!(
        described.appearance, second,
        "the newest description is the one kept; only the clock is not restarted"
    );
}

#[test]
fn a_body_changing_its_clothes_does_not_grow_the_palette_for_ever() {
    // **A size comparison misses this one entirely.** The palette used to be swept only
    // when the appearance cache changed length, and a body that changes what it is wearing
    // without leaving keeps the cache exactly as long as it was — so every colour it
    // stopped wearing stayed for the rest of the session.
    //
    // What is asserted is a ceiling rather than a number, and the ceiling carries one
    // frame of slack on purpose. The sweep is a trigger, and triggers have hysteresis: it
    // runs inside `apply_snapshots`, and `dress_bodies` adds the colours for the change it
    // has just been told about *after* that — so the map is over its bound for exactly the
    // frame between the two, by at most one body's worth of base parts. What would fail is
    // growth: unswept, forty changes of shirt leave forty-odd colours behind, and the
    // ceiling below does not move with the number of changes.
    let mut app = headless_player();
    let start = Instant::now();
    describe(&mut app, 99, an_appearance(HairModel::Braided));
    deliver(
        &mut app,
        1,
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(99, [4.0, 64.0, 0.0], 0.0),
        ],
        start,
    );
    app.update();

    for shirt in 0..40u32 {
        let worn = Appearance::new(
            A_SKIN,
            0x0001_0000 * shirt + 0x0000_4020,
            A_TROUSERS,
            A_SHOES,
            HairModel::Braided,
            A_HAIR,
        )
        .expect("every colour is inside the contract's range");
        describe(&mut app, 99, worn);
        deliver(
            &mut app,
            2 + shirt,
            vec![
                state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
                state(99, [4.0, 64.0, 0.0], 0.0),
            ],
            start + INTERVAL * (1 + shirt),
        );
        app.update();

        let cached = app.world().resource::<Appearances>().0.len();
        let palette = app.world().resource::<BodyMaterials>().0.len();
        let per_description = BodyPart::IN_DRAWING_ORDER.len() + ArmourPiece::ALL.len();
        let justified = (cached + 1) * per_description;
        // One body is re-dressed per frame here, and only its base appearance changes.
        let ceiling = justified + BodyPart::IN_DRAWING_ORDER.len();
        assert!(
            palette <= ceiling,
            "after {} changes of shirt the palette holds {palette} colours, where {cached} \
             cached appearances justify {justified} and one frame of slack allows {ceiling}",
            shirt + 1,
        );
    }

    // And it settles: a frame in which nobody changes anything sweeps the slack away, so
    // the map ends where the cache says it should rather than a body's worth above it.
    deliver(
        &mut app,
        100,
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(99, [4.0, 64.0, 0.0], 0.0),
        ],
        start + INTERVAL * 60,
    );
    app.update();

    let cached = app.world().resource::<Appearances>().0.len();
    let palette = app.world().resource::<BodyMaterials>().0.len();
    assert!(
        palette <= (cached + 1) * (BodyPart::IN_DRAWING_ORDER.len() + ArmourPiece::ALL.len()),
        "the palette settled at {palette} colours for {cached} cached appearances"
    );
}

#[test]
fn the_end_of_a_session_forgets_every_body_it_drew() {
    // The mirror of the vitals: what the people in a session that has ended looked like is
    // not a fact about the next session, and a reconnect is described from scratch.
    let mut app = headless_player();
    describe(&mut app, 99, an_appearance(HairModel::Braided));
    deliver(
        &mut app,
        1,
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(99, [4.0, 64.0, 0.0], 0.0),
        ],
        Instant::now(),
    );
    app.update();
    assert!(!app.world().resource::<Appearances>().0.is_empty());
    assert!(!app.world().resource::<BodyMaterials>().0.is_empty());

    app.world_mut().remove_resource::<Session>();
    app.update();

    assert!(app.world().resource::<Appearances>().0.is_empty());
    assert!(app.world().resource::<BodyMaterials>().0.is_empty());
}

#[test]
fn an_entity_that_leaves_the_snapshot_loses_its_body() {
    // The latest snapshot is the whole truth about what this session can see. A body kept
    // because an older snapshot mentioned it would be a ghost standing where it last was.
    let mut app = headless_player();
    let start = Instant::now();
    describe_wearing(
        &mut app,
        99,
        an_appearance(HairModel::Braided),
        [
            crafting::ITEM_LEATHER_CAP,
            crafting::ITEM_LEATHER_JERKIN,
            crafting::ITEM_LEATHER_LEGGINGS,
            0,
        ],
    );
    deliver(
        &mut app,
        1,
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(99, [4.0, 64.0, 0.0], 0.0),
        ],
        start,
    );
    app.update();
    assert_eq!(bodies(&mut app).len(), 2);
    let overlay_entities: Vec<Entity> = {
        let world = app.world_mut();
        let mut query = world.query_filtered::<Entity, With<ArmourVisual>>();
        query.iter(world).collect()
    };
    assert_eq!(overlay_entities.len(), ArmourSegment::ALL.len());

    deliver(
        &mut app,
        2,
        vec![state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0)],
        start + INTERVAL,
    );
    app.update();

    let remaining = bodies(&mut app);
    assert_eq!(remaining.len(), 1, "the entity that left is still drawn");
    assert_eq!(remaining[0].0, LOCAL_ID);
    assert_eq!(stats(&app).entities, 1);
    for overlay in overlay_entities {
        assert!(
            app.world().get::<ArmourVisual>(overlay).is_none(),
            "an overlay outlived the body it dressed"
        );
    }
}

#[test]
fn a_synthetic_off_hand_id_reaches_the_rig_without_adding_geometry() {
    let mut app = headless_player();
    describe_wearing(
        &mut app,
        99,
        an_appearance(HairModel::Braided),
        [0, 0, 0, u16::MAX],
    );
    deliver(
        &mut app,
        1,
        vec![state(99, [4.0, 64.0, 0.0], 0.0)],
        Instant::now(),
    );
    app.update();

    let body = body_of(&mut app, 99).expect("the described body is drawn");
    assert_eq!(
        app.world()
            .get::<Worn>(body)
            .expect("the rig is dressed")
            .off_hand,
        u16::MAX
    );
    assert!(armour_of(&mut app, 99).is_empty());
    assert_eq!(child_count(&mut app, 99), BodyPiece::ALL.len());
}

#[test]
fn a_body_is_interpolated_between_two_snapshots_rather_than_snapped_to_the_newest() {
    // The client renders one interval in the past, so at the instant a snapshot arrives the
    // body is still at the *previous* position and walks to the new one over the tick that
    // follows. A client that snapped would put it at 8.0 immediately and then sit still.
    let mut app = headless_player();
    let start = Instant::now();

    deliver(
        &mut app,
        1,
        vec![state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0)],
        start,
    );
    app.update();
    deliver(
        &mut app,
        2,
        vec![state(LOCAL_ID, [8.0, 64.0, 0.0], 0.0)],
        // Dated in the future, so the sample this frame takes lands at the start of the
        // segment however long the frame itself took.
        Instant::now() + INTERVAL,
    );
    app.update();

    let x = bodies(&mut app)[0].1.x;
    assert!(
        (0.0..8.0).contains(&x),
        "the body was drawn at x = {x}, which is not between the two snapshots"
    );
    assert!(
        x < 4.0,
        "the body was drawn at x = {x}, more than halfway along a segment it has just \
         started: the client is not rendering behind the newest snapshot"
    );
}

#[test]
fn snapshots_that_arrive_with_no_session_are_held_rather_than_guessed_at() {
    // Unreachable through the handshake, which refuses a snapshot before the welcome. If it
    // were reachable, `tick_rate` would be unknown and there would be no interval to
    // interpolate over — and `entity_id` would be unknown, so nothing could be identified
    // as this player.
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, AssetPlugin::default()))
        .init_asset::<Mesh>()
        .init_asset::<StandardMaterial>()
        .add_plugins(PlayerPlugin);

    deliver(
        &mut app,
        1,
        vec![state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0)],
        Instant::now(),
    );
    app.update();

    assert!(bodies(&mut app).is_empty());
    assert_eq!(stats(&app).position, None);
}

#[test]
fn the_overlay_reports_the_speed_the_server_sent() {
    // The server's own number, not a difference of two interpolated positions: two frames
    // inside one tick would difference to zero, and the overlay would read "standing still"
    // for most of a walk.
    let mut app = headless_player();
    app.world_mut().resource_mut::<SnapshotInbox>().push(
        Snapshot {
            server_tick: 1,
            entities: vec![EntityState {
                entity_id: LOCAL_ID,
                pos: [0.0, 64.0, 0.0],
                vel: [3.0, 0.0, 4.0],
                yaw: 0.0,
            }],
            drops: vec![],
            ..Default::default()
        },
        Instant::now(),
    );
    app.update();

    let speed = stats(&app).speed.expect("a snapshot named this player");
    assert!((speed - 5.0).abs() < 1e-4, "speed = {speed}, want 5");
}

// ---------------------------------------------------------------------------
// Vitals
// ---------------------------------------------------------------------------

/// The vitals a server could send, in one line.
fn vitals(health: u16, life_state: LifeState, respawn_ticks: u32) -> PlayerVitals {
    PlayerVitals {
        health,
        max_health: 100,
        hunger: 100,
        max_hunger: 100,
        level: 1,
        experience: 0,
        experience_to_next: 50,
        life_state,
        respawn_ticks,
        invulnerable: false,
        blocking: false,
    }
}

/// Queues a snapshot carrying vitals the test chose, as the net thread would.
fn deliver_vitals(app: &mut App, tick: u32, carried: PlayerVitals, at: Instant) {
    let dead_players = (carried.life_state == LifeState::Dead)
        .then_some(LOCAL_ID)
        .into_iter()
        .collect();
    app.world_mut().resource_mut::<SnapshotInbox>().push(
        Snapshot {
            server_tick: tick,
            entities: vec![state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0)],
            self_vitals: carried,
            dead_players,
            ..Default::default()
        },
        at,
    );
}

fn held(app: &App) -> Option<PlayerVitals> {
    app.world().resource::<SelfVitals>().get()
}

/// This session's own body goes over backwards while the server says the player is dead,
/// and stands back up on the respawn with nothing having animated it back.
///
/// **The half of a death only third person can see**, and the reason `camera.rs` leaves its
/// camera alone in that view. The direction is asserted in world space rather than against
/// `DEATH_BODY_PITCH`, for the reason a draugr's is: the sign is two conventions multiplied
/// together — the rig faces -Z and a positive rotation about +X carries its top towards +Z —
/// and a claim about where the body's own up axis ended up survives somebody re-authoring
/// either of them.
#[test]
fn the_local_body_goes_over_backwards() {
    let mut app = headless_player();
    let start = Instant::now();
    // Yaw 0, so the body's local frame is the world's and "behind" is +Z.
    deliver_vitals(&mut app, 1, vitals(100, LifeState::Alive, 0), start);
    app.update();
    assert!(
        body_up_axis(&mut app, LOCAL_ID).dot(Vec3::Y) > 0.999,
        "a living body is already leaning"
    );

    deliver_vitals(
        &mut app,
        2,
        vitals(0, LifeState::Dead, 60),
        start + INTERVAL,
    );
    app.update();

    // The whole fall, in steps under `Time<Virtual>`'s 250 ms `max_delta`: one long step
    // is silently clamped to that, so a fall driven in one arrives part way over and every
    // assertion below would really be about the clamp.
    finish_body_fall(&mut app);

    let fallen = body_up_axis(&mut app, LOCAL_ID);
    assert!(
        fallen.z > 0.99,
        "a dead player's body ended up with its head at {fallen}, want it behind at +Z"
    );

    // And the respawn puts it upright with nothing here having to animate it: the pose is
    // composed onto the transform `apply_snapshots` rewrites every frame, so forgetting the
    // fall is the whole of standing back up.
    deliver_vitals(
        &mut app,
        3,
        vitals(100, LifeState::Alive, 0),
        start + INTERVAL * 2,
    );
    app.update();
    assert!(
        body_up_axis(&mut app, LOCAL_ID).dot(Vec3::Y) > 0.999,
        "the body was still on its back after the respawn"
    );
}

/// The same authoritative death list drives the viewer and everybody beside them, and the
/// first snapshot that clears it stands both bodies back up.
#[test]
fn every_client_sees_every_dead_player_fall_and_respawn() {
    let mut app = headless_player();
    let start = Instant::now();
    let entities = || {
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(99, [4.0, 64.0, 0.0], 0.0),
        ]
    };

    deliver(&mut app, 1, entities(), start);
    app.update();

    app.world_mut().resource_mut::<SnapshotInbox>().push(
        Snapshot {
            server_tick: 2,
            entities: entities(),
            self_vitals: vitals(0, LifeState::Dead, 60),
            dead_players: vec![LOCAL_ID, 99],
            ..Default::default()
        },
        start + INTERVAL,
    );
    app.update();
    finish_body_fall(&mut app);

    let local = body_up_axis(&mut app, LOCAL_ID);
    let remote = body_up_axis(&mut app, 99);
    assert!(
        local.z > 0.99,
        "the local body did not finish its fall: {local}"
    );
    assert!(
        remote.z > 0.99,
        "the remote body did not finish its fall: {remote}"
    );
    assert!(
        local.abs_diff_eq(remote, 1e-5),
        "one server state produced different local and remote poses: {local} and {remote}"
    );

    app.world_mut().resource_mut::<SnapshotInbox>().push(
        Snapshot {
            server_tick: 3,
            entities: entities(),
            self_vitals: vitals(100, LifeState::Alive, 0),
            ..Default::default()
        },
        start + INTERVAL * 2,
    );
    app.update();
    for id in [LOCAL_ID, 99] {
        assert!(
            body_up_axis(&mut app, id).dot(Vec3::Y) > 0.999,
            "body {id} stayed down after the server respawned it"
        );
    }
}

/// A client entering view after the event sees the state, not a replay from standing.
#[test]
fn a_body_first_seen_dead_is_already_on_the_ground() {
    let mut app = headless_player();
    app.world_mut().resource_mut::<SnapshotInbox>().push(
        Snapshot {
            server_tick: 1,
            entities: vec![
                state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
                state(99, [4.0, 64.0, 0.0], 0.0),
            ],
            dead_players: vec![99],
            ..Default::default()
        },
        Instant::now(),
    );
    app.update();

    assert!(
        body_up_axis(&mut app, 99).z > 0.99,
        "an already-dead body replayed its fall from standing"
    );
    assert!(
        body_up_axis(&mut app, LOCAL_ID).dot(Vec3::Y) > 0.999,
        "the living viewer inherited the remote body's pose"
    );
}

/// Presentation never promotes a health value into a life-state decision.
#[test]
fn zero_health_without_the_authoritative_dead_list_does_not_tip_a_body() {
    let mut app = headless_player();
    app.world_mut().resource_mut::<SnapshotInbox>().push(
        Snapshot {
            server_tick: 1,
            entities: vec![
                state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
                state(99, [4.0, 64.0, 0.0], 0.0),
            ],
            self_vitals: vitals(0, LifeState::Alive, 0),
            ..Default::default()
        },
        Instant::now(),
    );
    app.update();
    finish_body_fall(&mut app);

    for id in [LOCAL_ID, 99] {
        assert!(
            body_up_axis(&mut app, id).dot(Vec3::Y) > 0.999,
            "body {id} fell even though dead_players did not name it"
        );
    }
}

#[test]
fn every_accepted_snapshot_replaces_the_vitals_whole() {
    // Replaced, never merged and never incremented. The resource is exactly what the
    // newest accepted snapshot said, down to the respawn count.
    let mut app = headless_player();
    let start = Instant::now();
    assert_eq!(
        held(&app),
        None,
        "nothing is claimed before the server speaks"
    );

    deliver_vitals(&mut app, 1, vitals(100, LifeState::Alive, 0), start);
    app.update();
    assert_eq!(held(&app), Some(vitals(100, LifeState::Alive, 0)));

    deliver_vitals(
        &mut app,
        2,
        vitals(41, LifeState::Alive, 0),
        start + INTERVAL,
    );
    app.update();
    assert_eq!(held(&app), Some(vitals(41, LifeState::Alive, 0)));

    deliver_vitals(
        &mut app,
        3,
        vitals(0, LifeState::Dead, 60),
        start + INTERVAL * 2,
    );
    app.update();
    assert_eq!(held(&app), Some(vitals(0, LifeState::Dead, 60)));

    // Two in one frame: the newer one is the answer, and the older is not merged into it.
    deliver_vitals(
        &mut app,
        4,
        vitals(0, LifeState::Dead, 40),
        start + INTERVAL * 3,
    );
    deliver_vitals(
        &mut app,
        5,
        vitals(0, LifeState::Dead, 20),
        start + INTERVAL * 4,
    );
    app.update();
    assert_eq!(held(&app), Some(vitals(0, LifeState::Dead, 20)));
}

#[test]
fn hunger_is_carried_by_self_vitals_without_local_change() {
    let mut app = headless_player();
    let mut carried = vitals(73, LifeState::Alive, 0);
    carried.hunger = 24;

    deliver_vitals(&mut app, 1, carried, Instant::now());
    app.update();
    assert_eq!(held(&app), Some(carried));

    // Local frames neither drain nor restore it. Only another accepted snapshot may
    // replace the complete value.
    for _ in 0..4 {
        app.update();
        assert_eq!(held(&app).map(|vitals| vitals.hunger), Some(24));
    }

    let mut newer = carried;
    newer.hunger = 81;
    deliver_vitals(&mut app, 2, newer, Instant::now() + INTERVAL);
    app.update();
    assert_eq!(held(&app), Some(newer));
}

#[test]
fn a_snapshot_that_is_not_newer_does_not_move_the_vitals() {
    // Server ticks are monotonic per session, so a tick that is not newer is a duplicate —
    // and its vitals describe a moment already drawn. Accepting them would walk health
    // backwards, which is the one direction a client must never move it on its own.
    let mut app = headless_player();
    let start = Instant::now();

    deliver_vitals(&mut app, 7, vitals(55, LifeState::Alive, 0), start);
    app.update();

    deliver_vitals(
        &mut app,
        6,
        vitals(100, LifeState::Alive, 0),
        start + INTERVAL,
    );
    deliver_vitals(
        &mut app,
        7,
        vitals(12, LifeState::Alive, 0),
        start + INTERVAL,
    );
    app.update();

    assert_eq!(held(&app), Some(vitals(55, LifeState::Alive, 0)));
}

#[test]
fn silence_holds_the_servers_vitals_rather_than_running_them_down() {
    // The countdown the death overlay draws is this number. Nothing here advances it: local
    // time passes and the last authoritative answer stands, exactly as an entity's last
    // position holds when snapshots stop.
    let mut app = headless_player();
    deliver_vitals(&mut app, 1, vitals(0, LifeState::Dead, 60), Instant::now());
    app.update();

    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(1)));
    for _ in 0..10 {
        app.update();
        assert_eq!(held(&app), Some(vitals(0, LifeState::Dead, 60)));
    }
}

fn log_vitals_changes(vitals: Res<SelfVitals>, mut log: ResMut<ChangeLog>) {
    log.0.push(vitals.is_changed());
}

#[test]
fn an_idle_frame_does_not_touch_the_vitals() {
    // The health bar and the death countdown both rebuild their strings on a change, so a
    // resource that looked changed every frame would reallocate them every frame for the
    // rest of the session. Observed from inside a system, because `App::update()` ends each
    // frame with `World::clear_trackers()`.
    let mut app = headless_player();
    app.init_resource::<ChangeLog>()
        .add_systems(Update, log_vitals_changes.after(ApplySnapshots));

    deliver_vitals(&mut app, 1, vitals(70, LifeState::Alive, 0), Instant::now());
    app.update();
    app.world_mut().resource_mut::<ChangeLog>().0.clear();

    for _ in 0..4 {
        app.update();
    }
    assert_eq!(
        app.world().resource::<ChangeLog>().0,
        vec![false; 4],
        "SelfVitals was rewritten on a frame with no snapshot in it"
    );

    // And a snapshot repeating the same answer is not news either.
    app.world_mut().resource_mut::<ChangeLog>().0.clear();
    deliver_vitals(&mut app, 2, vitals(70, LifeState::Alive, 0), Instant::now());
    app.update();
    assert_eq!(app.world().resource::<ChangeLog>().0, vec![false]);
}

#[test]
fn the_end_of_a_session_forgets_the_servers_vitals() {
    // The combat HUD hides on the condition every other permanent panel hides on, and this
    // is the resource half of it: health from a session that has ended is not health.
    let mut app = headless_player();
    deliver_vitals(&mut app, 1, vitals(33, LifeState::Alive, 0), Instant::now());
    app.update();
    assert!(held(&app).is_some());

    app.world_mut().remove_resource::<Session>();
    app.update();
    assert_eq!(held(&app), None);
}

#[test]
fn a_dead_player_sends_no_movement_and_can_still_look_around() {
    // Suppression is usability and bandwidth, not authority — the server refuses a dead
    // player's movement whatever this client sends. What it must not do is stop the player
    // looking: `schemas/player.fbs` names the camera a client concern, and a corpse that
    // cannot turn its head is a bug rather than a rule.
    //
    // The controls are inserted rather than driven through `InputPlugin`, for the reason
    // [`drag`] gives: that plugin's own systems clear both resources in `PreUpdate`, before
    // `sample_input` would see anything a test had written.
    let mut app = headless_player();
    // One frame first: `InputMode` is inserted when the plugin is built, so the frame that
    // *adds* it reads as a mode transition and belongs to the UI — see `sample_input`.
    app.update();

    let mut keys = ButtonInput::default();
    keys.press(KeyCode::KeyW);
    keys.press(KeyCode::KeyD);
    keys.press(KeyCode::Space);
    app.insert_resource(keys);
    app.insert_resource(AccumulatedMouseMotion {
        delta: Vec2::new(60.0, 0.0),
    });

    deliver_vitals(&mut app, 1, vitals(0, LifeState::Dead, 60), Instant::now());
    app.update();

    assert_eq!(
        *app.world().resource::<MoveIntent>(),
        MoveIntent::default(),
        "a dead player's controls reached the wire"
    );
    assert_ne!(
        app.world().resource::<LookState>().yaw,
        0.0,
        "looking around is not a gameplay outcome and is not suppressed"
    );

    // And the server bringing them back restores the controls on the frame it says so.
    deliver_vitals(
        &mut app,
        2,
        vitals(100, LifeState::Alive, 0),
        Instant::now() + INTERVAL,
    );
    app.update();
    assert_eq!(
        *app.world().resource::<MoveIntent>(),
        MoveIntent {
            x: 1.0,
            z: 1.0,
            jump: true,
        }
    );
}

/// Records what a consumer with change detection would have seen, one entry per frame.
#[derive(Resource, Default)]
struct ChangeLog(Vec<bool>);

fn log_stats_changes(stats: Res<PlayerStats>, mut log: ResMut<ChangeLog>) {
    log.0.push(stats.is_changed());
}

#[test]
fn an_idle_frame_does_not_touch_the_stats() {
    // The status line rebuilds its string on a change, so a resource that looks changed
    // every frame would reallocate it every frame for the rest of the session.
    //
    // Observed from inside a system rather than with `is_changed()` from outside, because
    // `App::update()` ends each frame with `World::clear_trackers()`; an external check
    // after an update is always false and would pass regardless.
    let mut app = headless_player();
    app.init_resource::<ChangeLog>()
        .add_systems(Update, log_stats_changes.after(refresh_player_stats));

    deliver(
        &mut app,
        1,
        vec![state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0)],
        Instant::now(),
    );
    app.update();
    app.world_mut().resource_mut::<ChangeLog>().0.clear();

    for _ in 0..4 {
        app.update();
    }

    assert_eq!(
        app.world().resource::<ChangeLog>().0,
        vec![false; 4],
        "PlayerStats was rewritten on a frame where nothing moved"
    );
}

// ---------------------------------------------------------------------------
// The camera
// ---------------------------------------------------------------------------

#[test]
fn the_camera_starts_at_the_servers_spawn_point() {
    // Before any snapshot: the welcome is the only thing that has said where the player is,
    // and one tick of terrain beats one tick of black.
    let mut app = headless_player();
    app.update();

    let placed = camera_transform(&mut app).translation;
    assert_eq!(placed, Vec3::new(0.5, 64.0 + constants::EYE_HEIGHT, 0.5));
}

#[test]
fn the_camera_follows_the_authoritative_position_at_eye_height() {
    let mut app = headless_player();
    deliver(
        &mut app,
        1,
        vec![state(LOCAL_ID, [10.0, 70.0, -3.0], 0.0)],
        Instant::now(),
    );
    app.update();

    let placed = camera_transform(&mut app).translation;
    assert_eq!(
        placed,
        Vec3::new(10.0, 70.0 + constants::EYE_HEIGHT, -3.0),
        "the camera is not at the eyes of the body the server placed"
    );
}

#[test]
fn the_camera_is_turned_by_the_local_look_state_and_not_by_the_snapshot() {
    // Where the camera points is a client concern — `schemas/player.fbs` says so — and it
    // has to be immediate: waiting for the server to echo the yaw back would put a network
    // round trip on the act of looking around. The snapshot's yaw turns the *body*, and the
    // body is not drawn for the local player.
    let mut app = headless_player();
    deliver(
        &mut app,
        1,
        // A yaw the server is echoing from some earlier tick, deliberately different.
        vec![state(LOCAL_ID, [0.0, 64.0, 0.0], 3.0)],
        Instant::now(),
    );
    *app.world_mut().resource_mut::<LookState>() = LookState {
        yaw: 1.0,
        pitch: -0.25,
    };
    app.update();

    let rotation = camera_transform(&mut app).rotation;
    let want = Quat::from_rotation_y(1.0) * Quat::from_rotation_x(-0.25);
    assert!(
        rotation.abs_diff_eq(want, 1e-5),
        "camera rotation is {rotation:?}, want the look state's {want:?}"
    );
}

#[test]
fn yaw_zero_looks_along_negative_z() {
    // The basis both sides spell out. A mismatch here sends players sideways and reads as a
    // physics bug rather than a convention one — see the comment on `Player.step` in
    // server/internal/game/player.go.
    let mut app = headless_player();
    deliver(
        &mut app,
        1,
        vec![state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0)],
        Instant::now(),
    );
    app.update();

    let forward = camera_transform(&mut app).rotation * Vec3::NEG_Z;
    assert!(
        forward.abs_diff_eq(Vec3::NEG_Z, 1e-5),
        "yaw 0 looks along {forward:?}"
    );

    // And a quarter turn to the right looks along +X, which is the server's `right` vector.
    app.world_mut().resource_mut::<LookState>().yaw = -std::f32::consts::FRAC_PI_2;
    app.update();

    let right = camera_transform(&mut app).rotation * Vec3::NEG_Z;
    assert!(
        right.abs_diff_eq(Vec3::X, 1e-5),
        "a right turn looks along {right:?}"
    );
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

#[test]
fn the_send_cadence_averages_the_servers_tick_rate() {
    // Tick-driven, not frame-driven: a 240 Hz machine must not send twelve times the input
    // a 20 Hz one does. 60 frames of 16.67 ms is one second, and one second at 20 Hz is
    // twenty inputs.
    let mut cadence = InputCadence::default();
    let frame = Duration::from_secs_f64(1.0 / 60.0);

    let sent = (0..60).filter(|_| cadence.due(frame, INTERVAL)).count();
    assert_eq!(
        sent, 20,
        "a second of 60 fps produced {sent} inputs, want 20"
    );
}

#[test]
fn an_awkward_frame_time_still_averages_the_tick_rate() {
    // 40 ms frames against a 50 ms interval. Resetting the credit instead of accumulating it
    // would send on every other frame — 12.5 Hz where the server asked for 20.
    let mut cadence = InputCadence::default();
    let frame = Duration::from_millis(40);

    let sent = (0..25).filter(|_| cadence.due(frame, INTERVAL)).count();
    assert_eq!(
        sent, 20,
        "a second of 25 fps produced {sent} inputs, want 20"
    );
}

#[test]
fn a_long_stall_produces_an_input_rather_than_a_burst_of_them() {
    // A window drag or a shader compile. Every frame in such a burst would describe the same
    // controls, and only the newest is worth anything — the same reasoning as the server's
    // tick loop abandoning its missed ticks.
    let mut cadence = InputCadence::default();

    assert!(cadence.due(Duration::from_secs(5), INTERVAL));

    let after = (0..3)
        .filter(|_| cadence.due(Duration::from_millis(1), INTERVAL))
        .count();
    assert!(
        after <= 1,
        "the stall was paid off over the following frames: {after} extra inputs"
    );
}

#[test]
fn a_frame_shorter_than_a_tick_sends_nothing() {
    let mut cadence = InputCadence::default();
    assert!(!cadence.due(Duration::from_millis(1), INTERVAL));
    assert!(!cadence.due(Duration::from_millis(1), INTERVAL));
}

#[test]
fn nothing_is_sent_without_a_session() {
    // No session means the server has not said what rate to send at. A client that guessed
    // would be inventing a number the contract says it is told.
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, AssetPlugin::default()))
        .init_asset::<Mesh>()
        .init_asset::<StandardMaterial>()
        .add_plugins(PlayerPlugin);

    for _ in 0..10 {
        app.update();
        std::thread::sleep(Duration::from_millis(10));
    }

    let cadence = app.world().resource::<InputCadence>();
    assert_eq!(cadence.client_tick, 0);
    assert_eq!(cadence.sent, 0);
}

#[test]
fn the_keyboard_becomes_intent_and_nothing_else() {
    // Held rather than pressed, because PlayerInput describes the state of the controls each
    // tick. And un-normalised: scaling the diagonal is the *server's* clamp to apply, and a
    // client that did it here would be doing the server's job for it — see acceptIntent.
    let mut app = headless_player();
    app.add_plugins(InputPlugin);
    app.update();

    {
        let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        keys.press(KeyCode::KeyW);
        keys.press(KeyCode::KeyD);
        keys.press(KeyCode::Space);
    }
    app.update();

    assert_eq!(
        *app.world().resource::<MoveIntent>(),
        MoveIntent {
            x: 1.0,
            z: 1.0,
            jump: true
        }
    );

    {
        let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        keys.press(KeyCode::KeyA);
        keys.press(KeyCode::KeyS);
    }
    app.update();

    assert_eq!(
        *app.world().resource::<MoveIntent>(),
        MoveIntent {
            x: 0.0,
            z: 0.0,
            jump: true
        },
        "opposite keys must cancel rather than fight"
    );
}

#[test]
fn the_module_runs_without_an_input_plugin_at_all() {
    // Every test above this one relies on it, and so does CI: `Res<T>` on a missing resource
    // panics, and a client that exits because it has no keyboard is worse than one that
    // stands still.
    let mut app = headless_player();
    for _ in 0..3 {
        app.update();
    }

    assert_eq!(*app.world().resource::<MoveIntent>(), MoveIntent::default());
}

#[test]
fn a_yaw_is_wrapped_into_a_range_a_lerp_can_use() {
    use std::f32::consts::{PI, TAU};

    for (given, want) in [
        (0.0, 0.0),
        (PI - 0.1, PI - 0.1),
        (-PI + 0.1, -PI + 0.1),
        (PI + 0.1, -PI + 0.1),
        (TAU, 0.0),
        (TAU * 3.0 + 1.0, 1.0),
        (-TAU * 3.0 - 1.0, -1.0),
    ] {
        let got = wrap_angle(given);
        assert!(
            (got - want).abs() < 1e-4,
            "wrap_angle({given}) = {got}, want {want}"
        );
        assert!(
            (-PI..=PI).contains(&got),
            "wrap_angle({given}) = {got} is outside (-PI, PI]"
        );
    }
}

/// Drags the pointer, and one frame.
///
/// `AccumulatedMouseMotion` is inserted rather than driven through `InputPlugin`, because
/// that plugin's own system zeroes the resource in `PreUpdate` — before `sample_input` would
/// see anything a test had written.
fn drag(app: &mut App, delta: Vec2) -> LookState {
    app.insert_resource(AccumulatedMouseMotion { delta });
    app.update();
    *app.world().resource::<LookState>()
}

#[test]
fn the_pointer_turns_the_view_the_way_the_pointer_moved() {
    // Right turns right. Looking along -Z, turning towards +X is a negative rotation about
    // +Y, so a rightward drag has to *lower* the yaw — and getting this backwards is exactly
    // the kind of thing that is obvious in a window and invisible in a suite without one.
    let mut app = headless_player();
    app.update();

    let look = drag(&mut app, Vec2::new(100.0, 0.0));
    let forward = Quat::from_rotation_y(look.yaw) * Vec3::NEG_Z;
    assert!(
        forward.x > 0.0,
        "a rightward drag turned the view towards {forward:?}"
    );

    // And a downward drag lowers the pitch, because screen y grows downward.
    let mut app = headless_player();
    app.update();

    let look = drag(&mut app, Vec2::new(0.0, 100.0));
    assert!(
        look.pitch < 0.0,
        "a downward drag gave pitch {}",
        look.pitch
    );
}

#[test]
fn inventory_keeps_each_horizontal_direction_but_not_jump_or_camera_input() {
    for (key, x, z) in [
        (KeyCode::KeyW, 0.0, 1.0),
        (KeyCode::KeyS, 0.0, -1.0),
        (KeyCode::KeyA, -1.0, 0.0),
        (KeyCode::KeyD, 1.0, 0.0),
    ] {
        let mut app = headless_player();
        app.add_plugins(InputPlugin);
        app.update();
        let before = *app.world().resource::<LookState>();

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Inventory;
        {
            let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
            keys.press(key);
            keys.press(KeyCode::Space);
        }
        app.world_mut().write_message(MouseMotion {
            delta: Vec2::new(80.0, -40.0),
        });
        app.update();

        assert_eq!(*app.world().resource::<LookState>(), before, "key {key:?}");
        assert_eq!(
            *app.world().resource::<MoveIntent>(),
            MoveIntent { x, z, jump: false },
            "key {key:?} did not remain horizontal-only while the inventory was open"
        );
    }
}

#[test]
fn modes_that_own_the_keyboard_or_pause_ignore_movement_and_camera_input() {
    for mode in [InputMode::Chat, InputMode::Loot, InputMode::Menu] {
        let mut app = headless_player();
        app.add_plugins(InputPlugin);
        app.update();
        let before = *app.world().resource::<LookState>();

        *app.world_mut().resource_mut::<InputMode>() = mode;
        {
            let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
            keys.press(KeyCode::KeyW);
            keys.press(KeyCode::KeyD);
            keys.press(KeyCode::Space);
        }
        app.world_mut().write_message(MouseMotion {
            delta: Vec2::new(80.0, -40.0),
        });
        app.update();

        assert_eq!(
            *app.world().resource::<LookState>(),
            before,
            "mode {mode:?}"
        );
        assert_eq!(
            *app.world().resource::<MoveIntent>(),
            MoveIntent::default(),
            "mode {mode:?} leaked movement"
        );
    }
}

#[test]
fn the_pitch_cannot_pass_straight_up_or_straight_down() {
    // At exactly ±π/2 every yaw looks the same and the image flips as the pitch crosses it.
    let mut app = headless_player();
    app.update();

    for (drag_y, want) in [(-1e6, MAX_PITCH), (1e6, -MAX_PITCH)] {
        let look = drag(&mut app, Vec2::new(0.0, drag_y));
        assert_eq!(
            look.pitch, want,
            "a drag of {drag_y} gave pitch {}",
            look.pitch
        );
    }
}

#[test]
fn a_pointer_that_did_not_move_leaves_the_view_alone() {
    // `ResMut` marks a resource changed on every `DerefMut`, so an unconditional write would
    // make the look state look changed every frame — and the camera reads it.
    let mut app = headless_player();
    app.update();

    let turned = drag(&mut app, Vec2::new(50.0, 0.0));
    let still = drag(&mut app, Vec2::ZERO);
    assert_eq!(turned, still);
}

#[test]
fn a_yaw_that_has_turned_many_times_stays_in_range() {
    // A player who spins for a while. The server wraps the yaw it echoes, so a client whose
    // own copy had drifted a thousand turns away would disagree with every snapshot about
    // which way it faces — and a lerp between the two would be nonsense.
    use std::f32::consts::PI;

    let mut app = headless_player();
    app.update();

    for _ in 0..40 {
        let look = drag(&mut app, Vec2::new(-1_000.0, 0.0));
        assert!(
            (-PI..=PI).contains(&look.yaw),
            "yaw drifted to {}",
            look.yaw
        );
    }
}

#[test]
fn the_interval_is_the_servers_number() {
    // A client that hardcoded 20 Hz would send at the wrong rate against every other server.
    assert_eq!(tick_interval(20), Duration::from_millis(50));
    assert_eq!(tick_interval(1), Duration::from_secs(1));
    assert_eq!(tick_interval(u8::MAX), Duration::from_secs(1) / 255);
}

// ---------------------------------------------------------------------------
// The sky
// ---------------------------------------------------------------------------

/// A welcome that describes a day: 24 000 ticks with a night from 14 400 to 21 600, the
/// shape `net/handshake.rs`'s own fixtures use.
fn session_with_a_clock() -> Session {
    let mut session = session();
    session.0.clock = WorldClock {
        day_length_ticks: 24_000,
        night_start_ticks: 14_400,
        night_end_ticks: 21_600,
    };
    session
}

fn headless_player_with_a_clock() -> App {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, AssetPlugin::default()))
        .init_asset::<Mesh>()
        .init_asset::<StandardMaterial>()
        .insert_resource(session_with_a_clock())
        .add_plugins(PlayerPlugin);
    app
}

/// Queues a snapshot that names where the day is, as the net thread would.
fn deliver_at_tick_of_day(app: &mut App, tick: u32, tick_of_day: u32, at: Instant) {
    app.world_mut().resource_mut::<SnapshotInbox>().push(
        Snapshot {
            server_tick: tick,
            tick_of_day,
            ..Default::default()
        },
        at,
    );
}

/// The sun's illuminance and the direction it shines in.
fn sun(app: &mut App) -> (f32, Vec3) {
    let world = app.world_mut();
    let mut query = world.query_filtered::<(&DirectionalLight, &Transform), With<sky::Sun>>();
    let found: Vec<(f32, Vec3)> = query
        .iter(world)
        .map(|(light, transform)| (light.illuminance, transform.forward().as_vec3()))
        .collect();
    assert_eq!(found.len(), 1, "exactly one sun lights the world");
    found[0]
}

/// The camera's clear colour and ambient term.
fn sky_and_ambient(app: &mut App) -> (Color, f32) {
    let world = app.world_mut();
    let mut query = world.query_filtered::<(&Camera, &AmbientLight), With<camera::WorldCamera>>();
    let found: Vec<(Color, f32)> = query
        .iter(world)
        .map(|(camera, ambient)| {
            let ClearColorConfig::Custom(sky) = camera.clear_color else {
                panic!("the camera must clear to an explicit sky colour");
            };
            (sky, ambient.brightness)
        })
        .collect();
    assert_eq!(found.len(), 1, "exactly one camera owns the window");
    found[0]
}

/// The dome's transform, its visibility, and its vertices as `(height, colour)` pairs.
///
/// Read through the entity rather than through `SkyVisuals`, so the test asks the question
/// the renderer does — which mesh is this entity drawing.
fn dome(app: &mut App) -> (Transform, Visibility, Vec<(f32, [f32; 4])>) {
    let world = app.world_mut();
    let mut query =
        world.query_filtered::<(&Mesh3d, &Transform, &Visibility), With<sky::SkyDome>>();
    let found: Vec<(Handle<Mesh>, Transform, Visibility)> = query
        .iter(world)
        .map(|(mesh, transform, visibility)| (mesh.0.clone(), *transform, *visibility))
        .collect();
    assert_eq!(found.len(), 1, "exactly one dome carries the sky");
    let (handle, transform, visibility) = found[0].clone();

    let meshes = world.resource::<Assets<Mesh>>();
    let mesh = meshes.get(&handle).expect("the dome's mesh exists");
    let Some(VertexAttributeValues::Float32x3(positions)) =
        mesh.attribute(Mesh::ATTRIBUTE_POSITION)
    else {
        panic!("the dome's positions are three floats each");
    };
    let Some(VertexAttributeValues::Float32x4(colours)) = mesh.attribute(Mesh::ATTRIBUTE_COLOR)
    else {
        panic!("the dome's colours are four floats each");
    };
    assert_eq!(positions.len(), colours.len());
    let vertices = positions
        .iter()
        .zip(colours)
        .map(|(position, colour)| (position[1], *colour))
        .collect();
    (transform, visibility, vertices)
}

/// The colour of the dome vertex nearest the rim, and of the one at the zenith.
fn rim_and_zenith(app: &mut App) -> ([f32; 4], [f32; 4]) {
    let (_, _, vertices) = dome(app);
    let rim = vertices
        .iter()
        .min_by(|left, right| left.0.abs().total_cmp(&right.0.abs()))
        .expect("the dome has vertices")
        .1;
    let zenith = vertices
        .iter()
        .max_by(|left, right| left.0.total_cmp(&right.0))
        .expect("the dome has vertices")
        .1;
    (rim, zenith)
}

/// How warm a vertex colour is: the acceptance criterion's red-to-blue ratio.
fn warmth(colour: [f32; 4]) -> f32 {
    colour[0] / colour[2].max(f32::MIN_POSITIVE)
}

fn fog(app: &mut App) -> DistanceFog {
    let world = app.world_mut();
    let mut query = world.query_filtered::<&DistanceFog, With<camera::WorldCamera>>();
    let found: Vec<DistanceFog> = query.iter(world).cloned().collect();
    assert_eq!(found.len(), 1, "the one camera carries the one fog");
    found[0].clone()
}

/// Puts the one camera's eye at `at`. Nothing overwrites it in these tests: with no local
/// body in the snapshot, `follow_the_player` returns before it places anything.
fn put_the_eye_at(app: &mut App, at: Vec3) {
    let world = app.world_mut();
    let mut query = world.query_filtered::<&mut Transform, With<camera::WorldCamera>>();
    let mut placed = 0;
    for mut transform in query.iter_mut(world) {
        transform.translation = at;
        placed += 1;
    }
    assert_eq!(placed, 1, "exactly one camera owns the window");
}

/// A store holding one voxel of water at world block (2, 3, 4), and air everywhere else.
fn a_puddle() -> crate::world::ChunkStore {
    let mut chunk = crate::world::VoxelChunk::all_air(32);
    chunk.set(2, 3, 4, crate::world::palette::WATER);
    let mut store = crate::world::ChunkStore::default();
    store.insert(
        crate::net::ChunkCoord {
            cx: 0,
            cy: 0,
            cz: 0,
        },
        chunk,
    );
    store
}

const AMBIENCE_EYE: Vec3 = Vec3::new(32.5, 64.5, 32.5);
const AMBIENCE_GROUND_Y: i32 = 48;

/// A fully loaded sampling span whose 64 lattice columns take their surface from
/// `surface`. Trees stand two blocks over the surface in every Nth column.
fn an_ambience_landscape(
    mut surface: impl FnMut(usize) -> crate::world::BlockId,
    tree_every: Option<usize>,
) -> crate::world::ChunkStore {
    use std::collections::HashMap;

    const SIZE: usize = 32;
    let mut chunks = HashMap::new();
    for cx in 0..=1 {
        for cy in 1..=2 {
            for cz in 0..=1 {
                chunks.insert(
                    crate::net::ChunkCoord { cx, cy, cz },
                    crate::world::VoxelChunk::all_air(SIZE),
                );
            }
        }
    }

    let centre = AMBIENCE_EYE.floor().as_ivec3();
    for index in 0..ambience::AMBIENCE_SAMPLES {
        let lattice_x = index % 8;
        let lattice_z = index / 8;
        let offset = |slot: usize| {
            slot as i32 * ambience::AMBIENCE_SPACING - 7 * ambience::AMBIENCE_SPACING / 2
        };
        let x = centre.x + offset(lattice_x);
        let z = centre.z + offset(lattice_z);
        set_ambience_block(&mut chunks, x, AMBIENCE_GROUND_Y, z, surface(index));
        if tree_every.is_some_and(|stride| index % stride == 0) {
            set_ambience_block(
                &mut chunks,
                x,
                AMBIENCE_GROUND_Y + 2,
                z,
                crate::world::palette::LOG,
            );
        }
    }

    let mut store = crate::world::ChunkStore::default();
    for (coord, chunk) in chunks {
        store.insert(coord, chunk);
    }
    store
}

fn set_ambience_block(
    chunks: &mut std::collections::HashMap<crate::net::ChunkCoord, crate::world::VoxelChunk>,
    x: i32,
    y: i32,
    z: i32,
    block: crate::world::BlockId,
) {
    const SIZE: i32 = 32;
    let coord = crate::net::ChunkCoord {
        cx: x.div_euclid(SIZE),
        cy: y.div_euclid(SIZE),
        cz: z.div_euclid(SIZE),
    };
    chunks
        .get_mut(&coord)
        .expect("the complete sampling span is loaded")
        .set(
            x.rem_euclid(SIZE) as usize,
            y.rem_euclid(SIZE) as usize,
            z.rem_euclid(SIZE) as usize,
            block,
        );
}

fn settled_ambience(store: crate::world::ChunkStore) -> Ambience {
    let direct = ambience::samples_at(&store, AMBIENCE_EYE, 32);
    assert!(direct.len() <= ambience::AMBIENCE_SAMPLES);
    let mut app = headless_player();
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(1)))
        .insert_resource(store);
    // Startup spawns and initially places the camera. The following four readings
    // establish a changed candidate and then hold it for the three-second dwell.
    // Bevy caps virtual time at 250 ms per frame, so four frames make each reading.
    app.update();
    put_the_eye_at(&mut app, AMBIENCE_EYE);
    for _ in 0..16 {
        app.update();
    }
    *app.world().resource::<Ambience>()
}

#[test]
fn sand_columns_look_like_sand() {
    assert_eq!(
        settled_ambience(an_ambience_landscape(|_| crate::world::palette::SAND, None)),
        Ambience {
            ground: GroundLook::Sand,
            wooded: false,
        }
    );
}

#[test]
fn snow_columns_look_like_snow() {
    assert_eq!(
        settled_ambience(an_ambience_landscape(|_| crate::world::palette::SNOW, None)),
        Ambience {
            ground: GroundLook::Snow,
            wooded: false,
        }
    );
}

#[test]
fn grass_with_a_tree_in_every_six_columns_looks_wooded() {
    assert_eq!(
        settled_ambience(an_ambience_landscape(
            |_| crate::world::palette::GRASS,
            Some(6)
        )),
        Ambience {
            ground: GroundLook::Grass,
            wooded: true,
        }
    );
}

#[test]
fn a_checkerboard_of_sand_and_grass_has_no_ground_winner() {
    assert_eq!(
        settled_ambience(an_ambience_landscape(
            |index| if index % 2 == 0 {
                crate::world::palette::SAND
            } else {
                crate::world::palette::GRASS
            },
            None,
        )),
        Ambience::default()
    );
}

#[test]
fn an_empty_store_has_no_ground_look() {
    assert_eq!(
        settled_ambience(crate::world::ChunkStore::default()),
        Ambience::default()
    );
}

#[test]
fn water_on_every_column_top_has_no_ground_look() {
    assert_eq!(
        settled_ambience(an_ambience_landscape(
            |_| crate::world::palette::WATER,
            None
        )),
        Ambience::default()
    );
}

#[test]
fn losing_the_session_forgets_the_ground_look() {
    let mut app = headless_player();
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(1)))
        .insert_resource(an_ambience_landscape(|_| crate::world::palette::SAND, None));
    app.update();
    put_the_eye_at(&mut app, AMBIENCE_EYE);
    for _ in 0..16 {
        app.update();
    }
    assert_eq!(app.world().resource::<Ambience>().ground, GroundLook::Sand);

    app.world_mut().remove_resource::<Session>();
    app.update();
    assert_eq!(*app.world().resource::<Ambience>(), Ambience::default());
}

/// Two colours as the eye would see them, to within a hair.
fn same_colour(left: Color, right: Color) -> bool {
    let (a, b) = (Srgba::from(left), Srgba::from(right));
    [(a.red, b.red), (a.green, b.green), (a.blue, b.blue)]
        .iter()
        .all(|(x, y)| (x - y).abs() < 1e-3)
}

#[test]
fn the_sky_goes_blue_green_when_the_eye_is_under_water() {
    // The one thing in the sky that is a function of where the player is, against a
    // day-clock server so the comparison is with a sky that is genuinely computed.
    let mut app = headless_player_with_a_clock();
    app.insert_resource(a_puddle());
    deliver_at_tick_of_day(&mut app, 1, 4_800, Instant::now());
    app.update();

    let above = sky_and_ambient(&mut app).0;
    let above_fog = fog(&mut app);
    assert!(
        !same_colour(above, Color::srgb(0.05, 0.22, 0.35)),
        "the day sky must not already be the underwater one"
    );

    // Into the water. The voxel at (2, 3, 4) spans [2, 3) x [3, 4) x [4, 5).
    put_the_eye_at(&mut app, Vec3::new(2.5, 3.5, 4.5));
    app.update();

    let under = sky_and_ambient(&mut app).0;
    let under_fog = fog(&mut app);
    assert!(
        same_colour(under, Color::srgb(0.05, 0.22, 0.35)),
        "under water the sky is the water, not the hour: got {under:?}"
    );
    assert!(same_colour(under_fog.color, under), "the fog fades into it");
    let FogFalloff::Linear { start, end } = under_fog.falloff else {
        panic!("the fog stays a linear fade under water");
    };
    assert_eq!(
        end, 10.0,
        "ten blocks of visibility, not the render distance"
    );
    assert!(start < end && start > 0.0);

    let FogFalloff::Linear { end: above_end, .. } = above_fog.falloff else {
        panic!("the fog above the surface is a linear fade too");
    };
    assert!(
        above_end > end,
        "the world above water is visible further than ten blocks"
    );

    // And back out, which is what the `Local` flag exists for: nothing compares
    // `ClearColorConfig`, so the restoring write is triggered by the transition.
    put_the_eye_at(&mut app, Vec3::new(2.5, 9.5, 4.5));
    app.update();

    let after = sky_and_ambient(&mut app).0;
    assert!(
        !same_colour(after, Color::srgb(0.05, 0.22, 0.35)),
        "leaving the water restores the day-clock sky"
    );
    assert!(same_colour(after, above), "and restores that exact sky");
    assert!(same_colour(fog(&mut app).color, after));
}

#[test]
fn a_server_with_no_clock_still_shows_the_water_it_streams() {
    // Not a time of day, so it overrides the fixed sky too — and a server with no clock
    // is every server in this repository today.
    let mut app = headless_player();
    app.insert_resource(a_puddle());
    app.update();

    assert!(same_colour(
        sky_and_ambient(&mut app).0,
        sky::Daylight::FIXED.sky
    ));

    put_the_eye_at(&mut app, Vec3::new(2.5, 3.5, 4.5));
    app.update();
    assert!(same_colour(
        sky_and_ambient(&mut app).0,
        Color::srgb(0.05, 0.22, 0.35)
    ));

    put_the_eye_at(&mut app, Vec3::new(2.5, 9.5, 4.5));
    app.update();
    assert!(
        same_colour(sky_and_ambient(&mut app).0, sky::Daylight::FIXED.sky),
        "and the clockless sky comes back exactly as it was"
    );
}

#[test]
fn the_sky_never_asks_a_store_it_has_not_been_given() {
    // `PlayerPlugin` does not add `WorldPlugin`, so every other test here runs with no
    // store at all. That is the pre-handshake frame, and it must do nothing.
    let mut app = headless_player_with_a_clock();
    deliver_at_tick_of_day(&mut app, 1, 4_800, Instant::now());
    // The camera is spawned on `Startup`, which is this first update.
    app.update();
    put_the_eye_at(&mut app, Vec3::new(2.5, 3.5, 4.5));
    app.update();

    assert!(
        !same_colour(sky_and_ambient(&mut app).0, Color::srgb(0.05, 0.22, 0.35)),
        "with no streamed world there is no water to be inside"
    );
}

#[test]
fn a_server_with_no_clock_leaves_the_sky_exactly_where_it_was() {
    // The path every server in this repository takes today, and the one that keeps taking
    // it until legacy PR 167 lands. A welcome whose `day_length_ticks` is zero is a legal
    // announcement rather than a missing field, and the four values it leaves alone are
    // the four this client rendered before there was a clock at all.
    let mut app = headless_player();
    app.update();

    let fixed = sky::Daylight::FIXED;
    for (tick, tick_of_day) in [(1_u32, 0_u32), (2, 12_345), (3, u32::MAX)] {
        deliver_at_tick_of_day(&mut app, tick, tick_of_day, Instant::now());
        app.update();

        let (illuminance, direction) = sun(&mut app);
        assert_eq!(illuminance, fixed.sun_illuminance);
        assert!(
            direction.distance(fixed.sun_direction.normalize()) < 1e-5,
            "tick of day {tick_of_day} moved a sun that has no clock: {direction:?}"
        );

        let (colour, ambient) = sky_and_ambient(&mut app);
        assert_eq!(colour, fixed.sky);
        assert_eq!(ambient, fixed.ambient_brightness);
    }
}

#[test]
fn a_declared_clock_moves_the_sun_the_sky_and_the_ambient() {
    // The other half of the same criterion: when the server does describe a day, all five
    // values are functions of it rather than constants.
    let mut app = headless_player_with_a_clock();
    app.update();

    deliver_at_tick_of_day(&mut app, 1, 4_800, Instant::now());
    app.update();
    let (noon_illuminance, noon_direction) = sun(&mut app);
    let (noon_sky, noon_ambient) = sky_and_ambient(&mut app);

    deliver_at_tick_of_day(&mut app, 2, 18_000, Instant::now());
    app.update();
    let (midnight_illuminance, midnight_direction) = sun(&mut app);
    let (midnight_sky, midnight_ambient) = sky_and_ambient(&mut app);

    assert!(
        midnight_illuminance < noon_illuminance,
        "midnight lit the world at {midnight_illuminance} against noon's {noon_illuminance}"
    );
    assert!(midnight_ambient < noon_ambient);
    assert!(Srgba::from(midnight_sky).blue < Srgba::from(noon_sky).blue);
    assert!(
        midnight_direction.distance(noon_direction) > 0.5,
        "the sun stood still between noon and midnight"
    );
}

#[test]
fn a_snapshot_older_than_the_newest_does_not_move_the_clock() {
    // The same gate that stops a reordered frame walking health backwards. Without it a
    // late snapshot would run the sun back across the sky, which is the one thing an
    // interpolated clock must never do.
    let mut app = headless_player_with_a_clock();
    app.update();

    deliver_at_tick_of_day(&mut app, 10, 12_000, Instant::now());
    app.update();
    let after_the_newest = *app.world().resource::<sky::SkyClock>();

    deliver_at_tick_of_day(&mut app, 5, 100, Instant::now());
    app.update();

    assert_eq!(
        *app.world().resource::<sky::SkyClock>(),
        after_the_newest,
        "a snapshot from an earlier tick anchored the sky"
    );
}

#[test]
fn the_fog_follows_the_sky_and_fades_at_the_edge_of_what_the_server_streams() {
    // The far face of the streamed cube is a cut in the world, and the fog is what turns it
    // into distance. Its reach is the server's `view_distance x chunk_size` rather than a
    // number here, and its colour is the sky's, so terrain dissolves into the sky rather
    // than into a haze of some other shade.
    let mut app = headless_player_with_a_clock();
    app.update();
    deliver_at_tick_of_day(&mut app, 1, 4_800, Instant::now());
    app.update();

    let fog = fog(&mut app);
    let (colour, _) = sky_and_ambient(&mut app);
    assert_eq!(fog.color, colour);

    let FogFalloff::Linear { start, end } = fog.falloff else {
        panic!("the fog fades linearly across the streamed volume");
    };
    // The session's own numbers: eight chunks of thirty-two blocks each.
    assert_eq!(end, 8.0 * 32.0);
    assert!(start > 0.0 && start < end);
}

fn log_clock_changes(clock: Res<sky::SkyClock>, mut log: ResMut<ChangeLog>) {
    log.0.push(clock.is_changed());
}

#[test]
fn an_idle_frame_does_not_touch_the_clock() {
    // `ResMut` marks a resource changed on every `DerefMut`, and `ingest_snapshots` runs on
    // every frame whether or not the inbox holds anything. Observed from inside a system,
    // because `App::update()` clears the trackers at the end of every frame.
    let mut app = headless_player_with_a_clock();
    app.init_resource::<ChangeLog>()
        .add_systems(Update, log_clock_changes.after(ingest_snapshots));

    deliver_at_tick_of_day(&mut app, 1, 4_800, Instant::now());
    app.update();
    app.world_mut().resource_mut::<ChangeLog>().0.clear();

    for _ in 0..4 {
        app.update();
    }

    assert_eq!(
        app.world().resource::<ChangeLog>().0,
        vec![false; 4],
        "the clock was rewritten on a frame no snapshot arrived on"
    );
}

#[test]
fn the_sky_advances_between_snapshots_rather_than_waiting_for_one() {
    // Snapshots arrive twenty times a second and frames are drawn sixty, so a sky anchored
    // to arrivals alone would step three times per frame's worth of change. It reads the
    // server's tick rate and advances between them.
    let mut app = headless_player_with_a_clock();
    app.update();

    // Anchored a whole daylight period behind noon, then read after enough wall-clock time
    // for the advance to have carried it forward.
    let anchored = Instant::now() - Duration::from_secs(120);
    deliver_at_tick_of_day(&mut app, 1, 21_700, anchored);
    app.update();

    let (illuminance, _) = sun(&mut app);
    let full_day = sky::Daylight::at(&session_with_a_clock().0.clock, 4_800.0, TICK_RATE);
    assert!(
        (illuminance - full_day.sun_illuminance).abs() < 1e-3,
        "two minutes past dawn the sun should be at its full daylight value, not {illuminance}"
    );
}

/// One query per component rather than one tuple of four: the tuple trips
/// `clippy::type_complexity`, and four read-only queries over the same entity are free.
fn log_environment_changes(
    suns: Query<Ref<DirectionalLight>, With<sky::Sun>>,
    cameras: Query<Ref<Camera>, With<camera::WorldCamera>>,
    ambients: Query<Ref<AmbientLight>, With<camera::WorldCamera>>,
    fogs: Query<Ref<DistanceFog>, With<camera::WorldCamera>>,
    mut log: ResMut<ChangeLog>,
) {
    log.0.push(
        suns.iter().any(|light| light.is_changed())
            || cameras.iter().any(|camera| camera.is_changed())
            || ambients.iter().any(|ambient| ambient.is_changed())
            || fogs.iter().any(|fog| fog.is_changed()),
    );
}

#[test]
fn a_clockless_server_leaves_the_environment_alone_after_the_first_frame() {
    // The other half of "renders exactly today's fixed sky": not merely the same values,
    // but not written at all. `Mut` marks a component changed on every `DerefMut`, so a
    // system that assigned the same four constants every frame would re-extract the sun and
    // the camera into the render world for the whole of a session whose sky never moves.
    //
    // Observed from inside a system, because `App::update()` clears the trackers at the end
    // of every frame.
    let mut app = headless_player();
    app.init_resource::<ChangeLog>()
        .add_systems(Update, log_environment_changes.after(ApplySnapshots));

    // Three frames to spawn the camera, insert the fog and let the insert settle. The
    // assertion is what keeps this test from passing vacuously on a fog that never arrived:
    // an absent component is an empty query, and an empty query reports no change.
    for _ in 0..3 {
        app.update();
    }
    let _ = fog(&mut app);
    app.world_mut().resource_mut::<ChangeLog>().0.clear();

    // Snapshots keep arriving; the sky still must not move, because there is no clock.
    for tick in 1..=4 {
        deliver_at_tick_of_day(&mut app, tick, tick * 1_000, Instant::now());
        app.update();
    }

    assert_eq!(
        app.world().resource::<ChangeLog>().0,
        vec![false; 4],
        "a server with no clock rewrote the sun, the sky, the ambient or the fog"
    );
}

#[test]
fn weather_updates_a_clockless_servers_sky_and_fog_together() {
    // Weather is allowed to move even when time of day is not. The sky and the fog have
    // separate component guards, so this pins the change that must open both of them.
    let mut app = headless_player();
    for _ in 0..3 {
        app.update();
    }
    let before = fog(&mut app);

    app.world_mut().resource_mut::<SnapshotInbox>().push(
        Snapshot {
            server_tick: 1,
            weather: Some(WeatherState {
                kind: WeatherKind::Rain,
                intensity: u8::MAX,
            }),
            ..Default::default()
        },
        Instant::now(),
    );
    app.update();

    let after = fog(&mut app);
    let (sky, _) = sky_and_ambient(&mut app);
    assert_eq!(after.color, sky, "the tinted sky and fog colour disagreed");
    let FogFalloff::Linear {
        start: before_start,
        end: before_end,
    } = before.falloff
    else {
        panic!("the baseline fog did not fade linearly");
    };
    let FogFalloff::Linear {
        start: after_start,
        end: after_end,
    } = after.falloff
    else {
        panic!("the weather fog did not fade linearly");
    };
    assert!((after_start - before_start * 0.6).abs() < 1e-4);
    assert!((after_end - before_end * 0.6).abs() < 1e-4);
}

// ---------------------------------------------------------------------------
// The birds
// ---------------------------------------------------------------------------

/// The look these tests want held, whatever the ground sampler makes of an empty store.
///
/// `ambience::sample_the_ground` publishes `Unknown` once a second when there are no chunks
/// to read, which is correct and would silently empty the sky under every test below. This
/// resource plus [`hold_the_ambience`] make the look an input rather than a race.
#[derive(Resource, Debug, Clone, Copy)]
struct HeldLook(Ambience);

fn hold_the_ambience(held: Res<HeldLook>, mut ambience: ResMut<Ambience>) {
    if *ambience != held.0 {
        *ambience = held.0;
    }
}

/// A headless client whose ground look is whatever the test says it is.
fn birdwatching(ground: GroundLook, wooded: bool) -> App {
    let mut app = headless_player();
    app.insert_resource(HeldLook(Ambience { ground, wooded }))
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            100,
        )))
        .add_systems(
            Update,
            hold_the_ambience
                .before(birds::keep_the_flock)
                .after(ambience::sample_the_ground),
        );
    app
}

fn look_at(app: &mut App, ground: GroundLook, wooded: bool) {
    app.world_mut().resource_mut::<HeldLook>().0 = Ambience { ground, wooded };
}

/// The row each bird alive belongs to.
fn flock(app: &mut App) -> Vec<usize> {
    let world = app.world_mut();
    let mut query = world.query::<&birds::Bird>();
    query.iter(world).map(|bird| bird.species).collect()
}

fn bird_entities(app: &mut App) -> Vec<Entity> {
    let world = app.world_mut();
    let mut query = world.query_filtered::<Entity, With<birds::Bird>>();
    query.iter(world).collect()
}

/// Runs `frames` frames of 100 ms each.
fn watch(app: &mut App, frames: usize) {
    for _ in 0..frames {
        app.update();
    }
}

#[test]
fn each_look_gets_its_own_flock_and_a_bare_plain_gets_none() {
    for (ground, wooded, species) in [
        (GroundLook::Sand, false, Some(1)),
        (GroundLook::Snow, false, Some(2)),
        (GroundLook::Grass, true, Some(0)),
        (GroundLook::Grass, false, None),
        (GroundLook::Unknown, false, None),
    ] {
        let mut app = birdwatching(ground, wooded);
        watch(&mut app, 3);

        let birds = flock(&mut app);
        match species {
            None => assert!(
                birds.is_empty(),
                "{ground:?}/{wooded} put {} birds in the sky",
                birds.len()
            ),
            Some(index) => {
                let row = &birds::BIRDS[index];
                assert!(
                    row.flock.contains(&(birds.len() as u8)),
                    "{ground:?}/{wooded} flew {} of row {index}",
                    birds.len()
                );
                assert!(birds.iter().all(|row| *row == index));
            }
        }
        assert!(birds.len() <= birds::BIRD_COUNT_MAX);
    }
}

#[test]
fn a_crossing_replaces_the_flock_rather_than_mixing_two() {
    // The look is the whole of the existence set: a vulture over grass is a vulture the
    // ground stopped explaining, so it goes. *How* it goes — retired on the frame, or faded
    // out of the sky — is the second half of this issue; that it goes is this one.
    let mut app = birdwatching(GroundLook::Sand, false);
    watch(&mut app, 3);
    assert!(flock(&mut app).iter().all(|row| *row == 1));

    look_at(&mut app, GroundLook::Grass, true);
    app.update();
    let parrots = flock(&mut app);
    assert!(
        !parrots.is_empty() && parrots.iter().all(|row| *row == 0),
        "the crossing left the sky as {parrots:?}"
    );
    assert!(parrots.len() <= birds::BIRD_COUNT_MAX);

    look_at(&mut app, GroundLook::Grass, false);
    app.update();
    assert!(
        flock(&mut app).is_empty(),
        "felling the wood left the parrots in the air"
    );
}

#[test]
fn walking_a_long_way_re_seeds_the_flock_around_the_new_anchor() {
    // The anchor is what makes a bird's path a pure function of the clock, and it holds
    // still for a cell's width. Cross enough of them and every bird is behind the eye, so
    // the whole flock is replaced by one seeded where the player now is.
    let mut app = birdwatching(GroundLook::Snow, false);
    watch(&mut app, 3);
    let before = bird_entities(&mut app);
    assert!(!before.is_empty());

    put_the_eye_at(&mut app, Vec3::new(4096.0, 64.0, -4096.0));
    app.update();
    let after = bird_entities(&mut app);
    assert!(
        after.iter().all(|entity| !before.contains(entity)),
        "a bird outlived the anchor it was drawn around"
    );
    let anchored = bird_positions(&mut app);
    assert!(!anchored.is_empty(), "the new cell got no flock");
    for position in anchored {
        assert!(
            (position - Vec3::new(4096.0, 64.0, -4096.0))
                .abs()
                .max_element()
                <= birds::BIRD_RANGE + birds::BIRD_ANCHOR_CELL,
            "a re-seeded bird landed at {position}"
        );
    }
}

fn bird_positions(app: &mut App) -> Vec<Vec3> {
    let world = app.world_mut();
    let mut query = world.query_filtered::<&Transform, With<birds::Bird>>();
    query
        .iter(world)
        .map(|transform| transform.translation)
        .collect()
}

/// The dome sits on the eye, and it never takes the eye's rotation.
#[test]
fn the_sky_dome_is_centred_on_the_eye_and_does_not_turn_with_it() {
    // Every vertex is `SKY_BODY_DISTANCE` from the dome's own origin — the pure test in
    // `sky.rs` pins that — so putting the origin on the camera is what makes the sky the
    // same distance away wherever the player walks.
    let mut app = headless_player_with_a_clock();
    deliver_at_tick_of_day(&mut app, 1, 4_800, Instant::now());
    app.update();

    let eye = Vec3::new(120.0, 70.0, -45.0);
    put_the_eye_at(&mut app, eye);
    app.update();

    let (transform, visibility, vertices) = dome(&mut app);
    assert_eq!(
        transform.translation, eye,
        "the dome did not follow the eye"
    );
    assert_eq!(
        transform.rotation,
        Quat::IDENTITY,
        "the dome turned with the camera"
    );
    assert_eq!(visibility, Visibility::Visible);
    // 13 rings of 25 segments: the "few hundred vertices" the gradient is carried on.
    assert_eq!(vertices.len(), 325);
    for (height, _) in &vertices {
        assert!(
            height.abs() <= 400.0 + 1e-2,
            "a vertex stood {height} above the eye"
        );
    }
}

/// The fog fades terrain into the rim, and the camera still clears to the zenith.
#[test]
fn the_fog_fades_into_the_horizon_while_the_camera_clears_to_the_sky() {
    // At the middle of dusk the two colours are as far apart as they ever get, and the far
    // edge of the streamed cube is on the rim. Half a ramp before night begins:
    // `RAMP_SECONDS` is 60 at 20 ticks a second, so 600 ticks before 14 400 is where the
    // night fraction is exactly a half.
    let mut app = headless_player_with_a_clock();
    deliver_at_tick_of_day(&mut app, 1, 13_800, Instant::now());
    app.update();

    let (sky, _) = sky_and_ambient(&mut app);
    let fog = fog(&mut app);
    assert_ne!(
        fog.color, sky,
        "at the middle of dusk the rim and the zenith must differ"
    );
    let (sky, horizon) = (Srgba::from(sky), Srgba::from(fog.color));
    assert!(
        horizon.red / horizon.blue > sky.red / sky.blue,
        "the fog is not warmer than the sky it hangs under"
    );
}

/// The dome's rim is warm at dusk and is the zenith's own colour at midday.
#[test]
fn the_dome_rim_is_warmer_at_dusk_than_at_midday() {
    let mut app = headless_player_with_a_clock();
    deliver_at_tick_of_day(&mut app, 1, 4_800, Instant::now());
    app.update();
    let (midday_rim, midday_zenith) = rim_and_zenith(&mut app);
    for channel in 0..3 {
        assert!(
            (midday_rim[channel] - midday_zenith[channel]).abs() < 1e-5,
            "midday gave the rim a colour of its own in channel {channel}"
        );
    }

    deliver_at_tick_of_day(&mut app, 2, 13_800, Instant::now());
    app.update();
    let (dusk_rim, dusk_zenith) = rim_and_zenith(&mut app);
    assert!(
        warmth(dusk_rim) > warmth(midday_rim),
        "the dusk rim is not warmer than the midday one: {dusk_rim:?}"
    );
    assert!(
        warmth(dusk_rim) > warmth(dusk_zenith),
        "the dusk rim is not warmer than the sky above it"
    );
}

/// Under water the sky is the water, and none of it is drawn.
#[test]
fn the_dome_is_hidden_while_the_eye_is_under_water() {
    let mut app = headless_player_with_a_clock();
    app.insert_resource(a_puddle());
    deliver_at_tick_of_day(&mut app, 1, 4_800, Instant::now());
    app.update();
    assert_eq!(dome(&mut app).1, Visibility::Visible);

    // The voxel at (2, 3, 4) spans [2, 3) x [3, 4) x [4, 5).
    put_the_eye_at(&mut app, Vec3::new(2.5, 3.5, 4.5));
    app.update();
    assert_eq!(
        dome(&mut app).1,
        Visibility::Hidden,
        "the sky was still drawn from under the water"
    );

    put_the_eye_at(&mut app, Vec3::new(2.5, 9.5, 4.5));
    app.update();
    assert_eq!(
        dome(&mut app).1,
        Visibility::Visible,
        "coming back up did not put the sky back"
    );
}

/// A server with no clock paints one flat colour — the sky this client always had.
#[test]
fn a_clockless_server_paints_the_dome_at_the_fixed_sky() {
    let mut app = headless_player();
    app.update();

    let fixed = Srgba::from(sky::Daylight::FIXED.sky);
    let (_, visibility, vertices) = dome(&mut app);
    assert_eq!(visibility, Visibility::Visible);
    for (height, colour) in vertices {
        assert!(
            (colour[0] - fixed.red).abs() < 1e-5
                && (colour[1] - fixed.green).abs() < 1e-5
                && (colour[2] - fixed.blue).abs() < 1e-5,
            "the vertex {height} above the eye carried {colour:?}"
        );
    }
}

#[derive(Resource, Default)]
struct DomeEdits(usize);

fn count_dome_edits(
    mut edited: MessageReader<AssetEvent<Mesh>>,
    domes: Query<&Mesh3d, With<sky::SkyDome>>,
    mut edits: ResMut<DomeEdits>,
) {
    let Some(dome) = domes.iter().next() else {
        return;
    };
    for event in edited.read() {
        if matches!(event, AssetEvent::Modified { id } if *id == dome.0.id()) {
            edits.0 += 1;
        }
    }
}

/// The dome's vertex colours are a buffer upload, and an idle frame does not spend one.
#[test]
fn an_idle_frame_does_not_repaint_the_dome() {
    // `an_idle_frame_does_not_touch_the_clock` one layer out: `Assets::get_mut` marks the
    // asset modified whether or not the bytes moved, so a dome repainted unconditionally
    // would re-extract 325 vertices every frame of a session whose sky is a constant.
    let mut app = headless_player();
    app.init_resource::<DomeEdits>()
        .add_systems(Update, count_dome_edits);

    // Four frames to spawn the dome, paint it once and let the event through.
    for _ in 0..4 {
        app.update();
    }
    assert!(
        app.world().resource::<DomeEdits>().0 >= 1,
        "the dome was never painted, so this test would pass vacuously"
    );
    app.world_mut().resource_mut::<DomeEdits>().0 = 0;

    // Snapshots keep arriving; with no clock the gradient cannot move.
    for tick in 1..=4 {
        deliver_at_tick_of_day(&mut app, tick, tick * 1_000, Instant::now());
        app.update();
    }
    assert_eq!(
        app.world().resource::<DomeEdits>().0,
        0,
        "the dome was repainted on a frame its colours had not moved"
    );
}
