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
use bevy::time::TimeUpdateStrategy;

use super::*;
use crate::net::{EntityState, PlayerAppearance, SessionParams, Snapshot, WorldClock};

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
        inventory_slots: 36,
        hotbar_slots: 9,
        equipment_slots: 3,
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
            level,
        });
}

fn describe_wearing(app: &mut App, entity_id: u64, appearance: Appearance, worn: [u16; 3]) {
    app.world_mut()
        .resource_mut::<AppearanceInbox>()
        .push(PlayerAppearance {
            entity_id,
            appearance,
            name: "Test Character".to_owned(),
            worn_head: worn[0],
            worn_chest: worn[1],
            worn_legs: worn[2],
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
    assert_eq!(
        drawn.transform.translation,
        body_held_item_anchor() - BodyPiece::RightFist.pivot()
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
fn the_body_item_follows_selection_and_an_empty_slot_leaves_no_child() {
    let mut app = headless_player();
    *app.world_mut().resource_mut::<Inventory>() = Inventory::from_stacks(vec![
        crate::net::InventoryStack {
            item_id: items::ITEM_STONE,
            count: 1,
            ..Default::default()
        },
        crate::net::InventoryStack {
            item_id: items::ITEM_RAW_COAL,
            count: 2,
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

    let stone = body_held_item(&mut app);
    assert_eq!(stone.len(), 1);
    assert_eq!(stone[0].item.item_id, items::ITEM_STONE);
    assert_eq!(stone[0].item.shape, ItemShape::Block);
    let entity = stone[0].entity;

    *app.world_mut().resource_mut::<SelectedSlot>() = SelectedSlot(1);
    app.update();
    let coal = body_held_item(&mut app);
    assert_eq!(coal.len(), 1);
    assert_eq!(
        coal[0].entity, entity,
        "a slot change updates the fist-held child in place"
    );
    assert_eq!(coal[0].item.item_id, items::ITEM_RAW_COAL);
    assert_eq!(coal[0].item.shape, ItemShape::Material);

    *app.world_mut().resource_mut::<SelectedSlot>() = SelectedSlot(2);
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

    describe_wearing(&mut app, 99, appearance, [0, 0, 0]);
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

    describe_wearing(&mut app, 99, appearance, [0, 0, 0]);
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
    deliver(
        &mut app,
        1,
        vec![
            state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0),
            state(98, [2.0, 64.0, 0.0], 0.0),
            state(99, [4.0, 64.0, 0.0], 0.0),
        ],
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
    assert_eq!(text, "Lv 1 · Astrid");

    let world = app.world();
    let node = world.entity(plate).get::<Node>().expect("the plate is UI");
    assert_eq!(node.width, Val::Px(NAME_PLATE_WIDTH));
    assert_eq!(node.height, Val::Px(NAME_PLATE_HEIGHT));
    let font = world
        .entity(plate)
        .get::<TextFont>()
        .expect("the plate has a fixed font");
    assert_eq!(font.font_size, FontSize::Px(NAME_PLATE_FONT_SIZE));
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
        Some("Lv 1 · Bjorn".to_owned())
    );

    let (plate, _) = name_plate_of(&mut app, 99).expect("the late description added a plate");
    describe_as_level(&mut app, 99, "Ragnar", an_appearance(HairModel::Cropped), 7);
    app.update();

    assert_eq!(body_of(&mut app, 99), Some(body));
    assert_eq!(
        name_plate_of(&mut app, 99),
        Some((plate, "Lv 7 · Ragnar".to_owned())),
        "a new description rewrites the existing plate without replacing either entity"
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
    assert_eq!(name_plate_text(7, ""), "Lv 7 · ");
    assert_eq!(
        name_plate_text(7, "Sigrid\nJarl"),
        "Lv 7 · Sigrid\u{fffd}Jarl"
    );
    assert_eq!(name_plate_text(7, "石のᚠe\u{301}"), "Lv 7 · 石のᚠe\u{301}");

    let long = "界".repeat(NAME_PLATE_CHARACTERS + 20);
    let shown = name_plate_text(u16::MAX, &long);
    assert_eq!(shown.chars().count(), NAME_PLATE_CHARACTERS);
    assert!(shown.ends_with('…'));
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
fn inventory_and_menu_modes_ignore_movement_and_camera_input() {
    for mode in [InputMode::Inventory, InputMode::Menu] {
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

fn fog(app: &mut App) -> DistanceFog {
    let world = app.world_mut();
    let mut query = world.query_filtered::<&DistanceFog, With<camera::WorldCamera>>();
    let found: Vec<DistanceFog> = query.iter(world).cloned().collect();
    assert_eq!(found.len(), 1, "the one camera carries the one fog");
    found[0].clone()
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
