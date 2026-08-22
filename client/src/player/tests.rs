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
use bevy::input::InputPlugin;
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
fn describe(app: &mut App, entity_id: u64, appearance: Appearance) {
    app.world_mut()
        .resource_mut::<AppearanceInbox>()
        .push(PlayerAppearance {
            entity_id,
            appearance,
        });
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

/// Every drawn part of one body, sorted by part so a failure reads the same way twice.
fn parts_of(
    app: &mut App,
    entity_id: u64,
) -> Vec<(BodyPart, Handle<Mesh>, Handle<StandardMaterial>)> {
    let world = app.world_mut();
    let mut owners = world.query::<(&Body, &Children)>();
    let children: Vec<Entity> = owners
        .iter(world)
        .find(|(body, _)| body.0 == entity_id)
        .map(|(_, children)| children.iter().collect())
        .unwrap_or_default();

    let mut parts = world.query::<(&BodyVisual, &Mesh3d, &MeshMaterial3d<StandardMaterial>)>();
    let mut found: Vec<(BodyPart, Handle<Mesh>, Handle<StandardMaterial>)> = children
        .into_iter()
        .filter_map(|child| parts.get(world, child).ok())
        .map(|(visual, mesh, material)| (visual.0, mesh.0.clone(), material.0.clone()))
        .collect();
    found.sort_by_key(|(part, _, _)| format!("{part:?}"));
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
fn the_local_player_has_no_body_and_another_player_has_one() {
    // The camera sits at the local player's eyes, so a body there would fill the screen
    // with the inside of its own head. Another player is exactly what a body is for.
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

    assert_eq!(drawn, vec![(LOCAL_ID, false), (99, true)]);
}

#[test]
fn a_body_is_drawn_from_parts_that_each_take_their_own_colour() {
    // The acceptance criterion, part by part: head and hands the skin, torso the shirt,
    // legs the trousers, feet the shoes, hair its own — and the eyes a colour nobody
    // picked. Six parts, six materials, and no part wearing another's field.
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
    let mut parts: Vec<BodyPart> = drawn.iter().map(|(part, _, _)| *part).collect();
    parts.sort_by_key(|part| format!("{part:?}"));
    let mut expected = BodyPart::IN_DRAWING_ORDER.to_vec();
    expected.sort_by_key(|part| format!("{part:?}"));
    assert_eq!(parts, expected, "every part of the rig is drawn");

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
    let shirt = |drawn: &[(BodyPart, Handle<Mesh>, Handle<StandardMaterial>)]| {
        drawn
            .iter()
            .find(|(part, _, _)| *part == BodyPart::Shirt)
            .map(|(_, _, material)| material.clone())
            .expect("a body has a shirt")
    };
    assert_ne!(shirt(&one), shirt(&two), "two shirts, two materials");
    assert_eq!(
        one.iter().find(|(part, _, _)| *part == BodyPart::Skin),
        two.iter().find(|(part, _, _)| *part == BodyPart::Skin),
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
            .find(|(drawn, _, _)| *drawn == part)
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
            .find(|(part, _, _)| *part == BodyPart::Hair)
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
    let mut parts = world.query_filtered::<&Transform, With<BodyVisual>>();
    for child in children {
        assert_eq!(
            *parts.get(world, child).expect("every child is a part"),
            Transform::default(),
            "a part carries no offset: the mesh is authored at the feet"
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
    // frame between the two, by at most one body's worth of parts. Measured here it peaks
    // at 13 where the cache justifies 12, and comes straight back down. What would fail is
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
        let justified = (cached + 1) * BodyPart::IN_DRAWING_ORDER.len();
        // One body is re-dressed per frame here, so one part-set is the whole slack.
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
        palette <= (cached + 1) * BodyPart::IN_DRAWING_ORDER.len(),
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
        life_state,
        respawn_ticks,
        invulnerable: false,
    }
}

/// Queues a snapshot carrying vitals the test chose, as the net thread would.
fn deliver_vitals(app: &mut App, tick: u32, carried: PlayerVitals, at: Instant) {
    app.world_mut().resource_mut::<SnapshotInbox>().push(
        Snapshot {
            server_tick: tick,
            entities: vec![state(LOCAL_ID, [0.0, 64.0, 0.0], 0.0)],
            self_vitals: carried,
            ..Default::default()
        },
        at,
    );
}

fn held(app: &App) -> Option<PlayerVitals> {
    app.world().resource::<SelfVitals>().get()
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
    // it until #167 lands. A welcome whose `day_length_ticks` is zero is a legal
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
