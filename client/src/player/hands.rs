//! The first-person held item: a camera child, never a world entity.
//!
//! The selected authoritative stack chooses only a presentation, and this module no
//! longer holds an opinion about what that is: [`super::items`] owns the shape and the
//! colour every item draws in, and the hand reads them exactly as the pack cells and the
//! recipe panel do. What stays here is the view model itself — the meshes, the camera-space
//! placement and the cosmetic swing. None of it is a legality table: it cannot place,
//! consume or reject anything, and an unknown id remains visible through the palette
//! fallback. Mining progress never enters this module either; local time animates the hand,
//! while [`super::target`] alone displays the server's progress byte.

use std::f32::consts::{PI, TAU};
use std::time::Duration;

use bevy::ecs::system::SystemParam;
use bevy::light::NotShadowCaster;
use bevy::prelude::*;

use super::InputMode;
use super::camera::{ViewMode, WorldCamera};
use super::combat::{ITEM_RUSTY_SWORD, SwingSent};
use super::inventory::{ApplyInventory, Inventory, SelectedSlot};
use super::items::{self, ItemShape};
use super::merge_all;
use super::target::{ApplyTargetInput, BlockTarget};
use crate::net::Session;
use crate::world::palette;

/// Close to the near plane and small enough to remain inside the camera's free
/// view-space pocket even when terrain touches the player capsule.
const BASE_TRANSLATION: Vec3 = Vec3::new(0.10, -0.075, -0.18);

/// The whole of the closed fist: the box the palm and the knuckles fit inside.
///
/// Unchanged from when the hand *was* this box, so nothing about where the hand sits or how
/// far it swings moves — #175 replaces what fills it, not what it occupies.
const HAND_SIZE: Vec3 = Vec3::new(0.045, 0.085, 0.045);

/// How far each knuckle stands proud of the palm, as a fraction of the fist's depth.
///
/// Small: a fist read from the inside of a wrist is mostly one mass, and knuckles that
/// carried a third of the depth would be four separate fingers pointing at the camera.
const KNUCKLE_PROUD: f32 = 0.22;

/// How much of the fist's height the knuckle row occupies, measured from the top.
const KNUCKLE_BAND: f32 = 0.30;

/// How much darker a rust mark is than the iron it sits on.
///
/// **A multiplier, not a colour**, and that is what keeps `player/items.rs` the one answer
/// to which palette entry an item presents as. The blade's vertices carry white — identity
/// — everywhere but the marks, so the base that comes through is whatever that table says.
/// Change the sword's palette entry and the rust follows it, because it is a shade *of* it.
///
/// Warm and dark: red kept, green and blue pulled down, which is what turns a pale iron into
/// oxide rather than into grey.
const RUST_TINT: [f32; 4] = [0.72, 0.38, 0.22, 1.0];
const BLOCK_EDGE: f32 = 0.055;
const MATERIAL_RADIUS: f32 = 0.020;
const MATERIAL_LENGTH: f32 = 0.050;
const SWINGS_PER_SECOND: f32 = 2.4;
const SWING_RADIANS: f32 = 0.42;
const PLACE_BUMP_TIME: Duration = Duration::from_millis(150);
const PLACE_BUMP_DISTANCE: f32 = 0.025;

/// How long one attack swing plays for, and how far it carries the view model.
///
/// A one-shot, unlike the mining swing above, which repeats while the button is held: an
/// attack is an event the server judges once, so its feedback happens once.
const ATTACK_SWING_TIME: Duration = Duration::from_millis(220);
const ATTACK_SWING_RADIANS: f32 = 0.9;

/// The blade's shape, in the same camera-space units as the block and material meshes.
const BLADE_SIZE: Vec3 = Vec3::new(0.012, 0.115, 0.030);

/// A carried structure: a bundle, wider than it is tall, so a tent under the arm does not
/// read as another stackable cube.
const BUNDLE_SIZE: Vec3 = Vec3::new(0.075, 0.042, 0.048);

/// An implement's haft: longer and thicker than a blade, because what tells a shovel from
/// a sword at a glance is that one is a handle with weight on the end and the other is
/// mostly edge.
const TOOL_HAFT_SIZE: Vec3 = Vec3::new(0.014, 0.130, 0.014);

/// And its head, across the top of that haft. Wider than the haft in x and z and short in
/// y, which is the T a shovel, a pickaxe and an axe all share — and the whole of what
/// distinguishes the silhouette from [`BLADE_SIZE`]'s single tapering box.
const TOOL_HEAD_SIZE: Vec3 = Vec3::new(0.052, 0.020, 0.026);

/// A haft with a head across the top of it: one mesh, two boxes.
///
/// Merged rather than parented, for the reason the body's parts are merged in
/// `player::part_mesh`: the view model is one entity with one transform that
/// `animate_view_model` drives, and a second entity under it would be a second thing to
/// keep in step with a swing.
///
/// The three implements share it and are told apart by colour — see [`ItemShape::Tool`].
fn tool_mesh() -> Mesh {
    let mut merged = Mesh::from(Cuboid::from_size(TOOL_HAFT_SIZE));
    let head = Mesh::from(Cuboid::from_size(TOOL_HEAD_SIZE)).translated_by(Vec3::new(
        0.0,
        TOOL_HAFT_SIZE.y / 2.0,
        0.0,
    ));
    merge_all(&mut merged, [head], "held tool");
    merged
}

/// A closed fist: a palm with four knuckles standing proud of it.
///
/// **It was a single box**, which is the crudest shape in the game sharing the screen with
/// the other crudest shape — and it is on screen more than anything else, because an empty
/// hand is what a player holds most of the time (#175).
///
/// Five boxes merged into one mesh, for the reason [`tool_mesh`] merges two: the view model
/// is one entity with one transform that `animate_view_model` drives, and a knuckle parented
/// separately would be a second thing to keep in step with a swing.
///
/// It fills exactly [`HAND_SIZE`], so nothing about where the hand sits or how far it swings
/// moves. The knuckles take their depth out of the palm rather than adding to it.
fn fist_mesh() -> Mesh {
    let palm_depth = HAND_SIZE.z * (1.0 - KNUCKLE_PROUD);
    let mut merged = Mesh::from(Cuboid::from_size(Vec3::new(
        HAND_SIZE.x,
        HAND_SIZE.y,
        palm_depth,
    )))
    // Pushed back, so the knuckles below occupy the front of the box rather than growing it.
    .translated_by(Vec3::new(0.0, 0.0, (HAND_SIZE.z - palm_depth) / 2.0));

    // Four knuckles across the top of the palm, front-facing. A gap between them is what
    // makes them read as four rather than as one ridge, so each is a little under a quarter
    // of the width.
    let knuckle = Vec3::new(
        HAND_SIZE.x * 0.20,
        HAND_SIZE.y * KNUCKLE_BAND,
        HAND_SIZE.z * KNUCKLE_PROUD,
    );
    let top = HAND_SIZE.y / 2.0 - knuckle.y / 2.0;
    let front = -(HAND_SIZE.z / 2.0) + knuckle.z / 2.0;
    let knuckles = (0..4).map(|index| {
        // Spread across the palm's width: four centres at 1/8, 3/8, 5/8, 7/8 of it.
        let across = HAND_SIZE.x * ((index as f32 * 2.0 + 1.0) / 8.0 - 0.5);
        Mesh::from(Cuboid::from_size(knuckle)).translated_by(Vec3::new(across, top, front))
    });
    merge_all(&mut merged, knuckles, "fist");
    merged
}

/// The rusty sword's blade: iron with rust on it.
///
/// **Two colours on one mesh and one material**, which is what the cost note in
/// `client/AGENTS.md` asks for — the alternative was a second entity per held item, or a
/// material per item rather than per palette entry.
///
/// The vertices carry `Mesh::ATTRIBUTE_COLOR`, which `StandardMaterial` multiplies into its
/// `base_color`; `world/render.rs` has drawn the whole terrain that way since it existed, so
/// this is the established mechanism rather than a new one. White is identity — the iron
/// that comes through is whatever `player/items.rs` says the sword presents as — and the
/// marks carry [`RUST_TINT`], so they are a shade *of* that base rather than a second
/// opinion about it.
///
/// Three marks rather than a wash, at different heights and not touching the edge: rust
/// takes hold in patches, and a blade evenly discoloured reads as painted.
fn rusted_blade_mesh() -> Mesh {
    let mut merged = plain(Mesh::from(Cuboid::from_size(BLADE_SIZE)));

    // Each mark stands a hair proud of the blade's faces, for the reason the rig's hair does:
    // two surfaces sharing a plane is where a renderer has to choose, and it chooses per
    // frame. A twentieth of the blade's thinnest dimension is enough and is invisible.
    let proud = BLADE_SIZE.x * 0.05;
    let mark = Vec3::new(
        BLADE_SIZE.x + proud,
        BLADE_SIZE.y * 0.13,
        BLADE_SIZE.z * 0.55,
    );
    let marks = [-0.24, 0.02, 0.29]
        .into_iter()
        .enumerate()
        .map(|(index, height)| {
            // Alternating sides, so the blade is not rusted down one face and clean down the
            // other — which is what a single column of marks would look like as it turns.
            let side = if index % 2 == 0 { 1.0 } else { -1.0 };
            rusted(Mesh::from(Cuboid::from_size(mark)).translated_by(Vec3::new(
                0.0,
                BLADE_SIZE.y * height,
                side * BLADE_SIZE.z * 0.10,
            )))
        });
    merge_all(&mut merged, marks, "rusted blade");
    merged
}

/// One mesh with every vertex at identity, so the material's own colour comes through.
///
/// The attribute has to be present on *both* sides of a merge: `Mesh::merge` refuses to join
/// a mesh carrying an attribute to one that does not, and the halves would silently disagree
/// about what white means if it did not.
fn plain(mesh: Mesh) -> Mesh {
    tinted(mesh, [1.0, 1.0, 1.0, 1.0])
}

/// One mesh with every vertex carrying [`RUST_TINT`].
fn rusted(mesh: Mesh) -> Mesh {
    tinted(mesh, RUST_TINT)
}

fn tinted(mesh: Mesh, colour: [f32; 4]) -> Mesh {
    let vertices = mesh.count_vertices();
    mesh.with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, vec![colour; vertices])
}

pub(super) struct HandsPlugin;

impl Plugin for HandsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<HandAnimation>()
            // `PlayerCameraPlugin` owns it in the game. Initialised here too so this module
            // stands up headlessly on its own — the same defence `player/target.rs`,
            // `player/combat.rs`, `player/crafting.rs`, `player/inventory.rs`,
            // `player/structures.rs` and `ui/crosshair.rs` each keep, and it is not
            // optional: a `Res<T>` with no resource takes the app down rather than reading
            // a default.
            .init_resource::<ViewMode>()
            .add_systems(Startup, spawn_view_model)
            .add_systems(
                Update,
                (
                    attach_to_camera,
                    ApplyDeferred,
                    refresh_held_item,
                    animate_view_model,
                )
                    .chain()
                    .after(ApplyInventory)
                    .after(ApplyTargetInput)
                    // After the swing is sent, so the feedback plays on the frame the
                    // request left rather than the one after it.
                    .after(super::combat::ApplyCombatInput),
            );
    }
}

/// The view model's current subject: which item it is drawing, and in what shape.
///
/// `None` in both fields is the empty hand — not an item with a missing entry, which is
/// why [`ItemShape`] has no variant for it and this field is an `Option` instead.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct HeldItem {
    item_id: Option<u16>,
    shape: Option<ItemShape>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Appearance {
    item_id: Option<u16>,
    shape: Option<ItemShape>,
    palette_id: u16,
}

#[derive(Resource, Debug)]
struct HandVisuals {
    hand: Handle<Mesh>,
    /// The rusty sword's own blade, and the only item in this table that has one.
    ///
    /// A shape says what a *kind* of thing looks like; rust is a fact about one blade. The
    /// iron sword is the same [`ItemShape::Blade`] and must not inherit it, which is the
    /// whole reason this is keyed by item rather than by shape.
    rusted_blade: Handle<Mesh>,
    block: Handle<Mesh>,
    material: Handle<Mesh>,
    blade: Handle<Mesh>,
    bundle: Handle<Mesh>,
    tool: Handle<Mesh>,
    materials: Vec<([f32; 4], Handle<StandardMaterial>)>,
}

impl HandVisuals {
    fn mesh(&self, item_id: Option<u16>, shape: Option<ItemShape>) -> Handle<Mesh> {
        // The one item whose look is not simply its shape's. Checked before the shape and
        // not inside it, so [`ItemShape`] stays a vocabulary of kinds and this stays what it
        // is: one exception, named, for one blade.
        if item_id == Some(ITEM_RUSTY_SWORD) {
            return self.rusted_blade.clone();
        }
        match shape {
            None => self.hand.clone(),
            Some(ItemShape::Block) => self.block.clone(),
            Some(ItemShape::Material) => self.material.clone(),
            Some(ItemShape::Blade) => self.blade.clone(),
            Some(ItemShape::Bundle) => self.bundle.clone(),
            Some(ItemShape::Tool) => self.tool.clone(),
        }
    }

    fn material_for(
        &mut self,
        palette_id: u16,
        materials: &mut Assets<StandardMaterial>,
    ) -> Handle<StandardMaterial> {
        let colour = palette::linear_rgba(palette_id);
        if let Some((_, handle)) = self
            .materials
            .iter()
            .find(|(candidate, _)| *candidate == colour)
        {
            return handle.clone();
        }

        let [r, g, b, a] = colour;
        let handle = materials.add(StandardMaterial {
            base_color: Color::linear_rgba(r, g, b, a),
            unlit: true,
            fog_enabled: false,
            // Positive renders closer. Together with the near-plane placement this
            // prevents terrain depth from slicing through the held shape.
            depth_bias: 1_000.0,
            ..default()
        });
        self.materials.push((colour, handle.clone()));
        handle
    }
}

#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
struct HandAnimation {
    swing_elapsed: Duration,
    bump_elapsed: Option<Duration>,

    /// How long the current attack swing has been running, if one is. Started by a
    /// `SwingSent` message and by nothing else, so it plays exactly when a request left
    /// this client — whether that request later hits, misses or is refused.
    attack_elapsed: Option<Duration>,
}

fn spawn_view_model(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mut visuals = HandVisuals {
        hand: meshes.add(fist_mesh()),
        block: meshes.add(Cuboid::from_size(Vec3::splat(BLOCK_EDGE))),
        material: meshes.add(Capsule3d::new(MATERIAL_RADIUS, MATERIAL_LENGTH)),
        blade: meshes.add(Cuboid::from_size(BLADE_SIZE)),
        rusted_blade: meshes.add(rusted_blade_mesh()),
        bundle: meshes.add(Cuboid::from_size(BUNDLE_SIZE)),
        tool: meshes.add(tool_mesh()),
        materials: Vec::new(),
    };
    let appearance = selected_appearance(None);
    let mesh = visuals.mesh(appearance.item_id, appearance.shape);
    let material = visuals.material_for(appearance.palette_id, &mut materials);

    commands.spawn((
        HeldItem {
            item_id: appearance.item_id,
            shape: appearance.shape,
        },
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::from_translation(BASE_TRANSLATION),
        Visibility::Hidden,
        NotShadowCaster,
    ));
    commands.insert_resource(visuals);
}

/// Attaches to the one camera after both startup systems have materialised.
fn attach_to_camera(
    mut commands: Commands,
    cameras: Query<Entity, With<WorldCamera>>,
    unattached: Query<Entity, (With<HeldItem>, Without<ChildOf>)>,
) {
    let Some(camera) = cameras.iter().next() else {
        return;
    };
    for entity in &unattached {
        commands.entity(entity).insert(ChildOf(camera));
    }
}

/// The shared meshes and the assets a material is minted into, as one borrow.
///
/// The two always travel together — `HandVisuals::material_for` needs the assets to mint
/// into, and nothing asks either of them anything on its own — so grouping them is what
/// `player/mod.rs` already does with `Dressing` for the body's wardrobe, and for the same
/// reason. It is also what keeps [`refresh_held_item`] inside clippy's argument count now
/// that it reads the view: an `#[allow]` there would have suppressed the warning rather
/// than answered it, and the two fields genuinely are one thing.
#[derive(SystemParam)]
struct HandWardrobe<'w> {
    visuals: ResMut<'w, HandVisuals>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
}

fn refresh_held_item(
    inventory: Res<Inventory>,
    selected: Res<SelectedSlot>,
    mode: Res<InputMode>,
    view: Res<ViewMode>,
    session: Option<Res<Session>>,
    mut wardrobe: HandWardrobe<'_>,
    mut held: Query<(
        &mut HeldItem,
        &mut Mesh3d,
        &mut MeshMaterial3d<StandardMaterial>,
        &mut Visibility,
    )>,
) {
    let appearance = selected_appearance(inventory.slot(selected.0));
    // **The view term, and it was missing.** This model is a child of the camera, sitting
    // [`BASE_TRANSLATION`] in front of it — a first-person conceit and nothing else. #172
    // moved the camera four blocks back for the third-person view and gave every other such
    // conceit the term that removes it there: `InputGate::may_aim`, `InputGate::may_act`,
    // `ui::crosshair::show_crosshair` and `show_the_local_body`. This one was missed, so the
    // thing a player was holding floated between the camera and their own character (#194).
    //
    // Hidden rather than despawned, which is what the neighbouring test's name has always
    // said: a view toggle that removed the model would rebuild a mesh and a material on a
    // key press, and `animate_view_model` drives a transform on this same entity — so a
    // hidden model is a hidden animation, with nothing further to gate.
    let visible = if *mode == InputMode::Playing && session.is_some() && view.first_person() {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };

    for (mut item, mut mesh, mut material, mut visibility) in &mut held {
        if item.item_id != appearance.item_id || item.shape != appearance.shape {
            item.item_id = appearance.item_id;
            item.shape = appearance.shape;
            mesh.0 = wardrobe.visuals.mesh(appearance.item_id, appearance.shape);
            material.0 = wardrobe
                .visuals
                .material_for(appearance.palette_id, &mut wardrobe.materials);
        }
        if *visibility != visible {
            *visibility = visible;
        }
    }
}

/// The presentation the selected stack asks for, or the empty hand.
///
/// Every fact in it comes from [`super::items`] — the one table the pack cells, the recipe
/// panel and the tooltip read too — so a stack cannot look like one thing in the hand and
/// another in the pack.
fn selected_appearance(stack: Option<crate::net::InventoryStack>) -> Appearance {
    let Some(item_id) = stack
        .filter(|stack| stack.item_id != 0 && stack.count != 0)
        .map(|stack| stack.item_id)
    else {
        return Appearance {
            item_id: None,
            shape: None,
            // The bare hand is not an item and has no row: dirt is the closest the terrain
            // palette comes to skin, and it is read here rather than looked up.
            palette_id: palette::DIRT,
        };
    };

    Appearance {
        item_id: Some(item_id),
        shape: Some(items::item_shape(item_id)),
        palette_id: items::item_palette_id(item_id),
    }
}

fn animate_view_model(
    time: Res<Time>,
    buttons: Option<Res<ButtonInput<MouseButton>>>,
    mode: Res<InputMode>,
    target: Res<BlockTarget>,
    mut swings: MessageReader<SwingSent>,
    mut animation: ResMut<HandAnimation>,
    mut held: Query<&mut Transform, With<HeldItem>>,
) {
    let playing = *mode == InputMode::Playing && !mode.is_changed();
    let mining = playing
        && buttons
            .as_deref()
            .is_some_and(|buttons| buttons.pressed(MouseButton::Left))
        && target.0.is_some();
    let placing = playing
        && buttons
            .as_deref()
            .is_some_and(|buttons| buttons.just_pressed(MouseButton::Right))
        && target.0.and_then(|hit| hit.place_target()).is_some();

    let mut next_animation = *animation;
    if mining {
        next_animation.swing_elapsed += time.delta();
    } else {
        next_animation.swing_elapsed = Duration::ZERO;
    }

    // One swing per message, restarted rather than queued: two clicks inside one
    // animation should look like two swings, and the second server-side request is
    // refused by the cooldown either way.
    if swings.read().next().is_some() {
        next_animation.attack_elapsed = Some(Duration::ZERO);
    }
    if let Some(elapsed) = next_animation.attack_elapsed.as_mut() {
        *elapsed += time.delta();
        if *elapsed >= ATTACK_SWING_TIME {
            next_animation.attack_elapsed = None;
        }
    }
    if placing {
        next_animation.bump_elapsed = Some(Duration::ZERO);
    }
    if let Some(elapsed) = next_animation.bump_elapsed.as_mut() {
        *elapsed += time.delta();
        if *elapsed >= PLACE_BUMP_TIME {
            next_animation.bump_elapsed = None;
        }
    }
    if *animation != next_animation {
        *animation = next_animation;
    }

    let next = animated_transform(&next_animation);
    for mut transform in &mut held {
        if *transform != next {
            *transform = next;
        }
    }
}

fn animated_transform(animation: &HandAnimation) -> Transform {
    let swing_phase = animation.swing_elapsed.as_secs_f32() * SWINGS_PER_SECOND * TAU;
    let mut swing = swing_phase.sin() * SWING_RADIANS;
    // One arc, out and back, added to whatever the mining swing is doing. The two never
    // run together in practice — a blade suppresses mining — and summing rather than
    // branching keeps the transform one expression.
    if let Some(elapsed) = animation.attack_elapsed {
        let fraction = (elapsed.as_secs_f32() / ATTACK_SWING_TIME.as_secs_f32()).clamp(0.0, 1.0);
        swing -= (fraction * PI).sin() * ATTACK_SWING_RADIANS;
    }
    let bump = animation.bump_elapsed.map_or(0.0, |elapsed| {
        let fraction = (elapsed.as_secs_f32() / PLACE_BUMP_TIME.as_secs_f32()).clamp(0.0, 1.0);
        (fraction * PI).sin()
    });

    Transform {
        translation: BASE_TRANSLATION + Vec3::Z * (bump * PLACE_BUMP_DISTANCE),
        rotation: Quat::from_rotation_x(-0.18 + swing) * Quat::from_rotation_z(-0.12 - bump * 0.18),
        ..default()
    }
}

#[cfg(test)]
mod tests {
    use bevy::asset::AssetPlugin;

    use bevy::mesh::VertexAttributeValues;

    use super::super::crafting::ITEM_IRON_SWORD;
    use super::*;
    use crate::net::{InventoryStack, SessionParams};
    use crate::player::items::{ITEM_LOG, ITEM_RAW_COAL, ITEM_RAW_IRON, ITEM_STONE};
    use crate::player::{PlayerPlugin, combat, crafting, structures};

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 1,
            spawn: [0.0; 3],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 3,
            inventory_slots: 4,
            hotbar_slots: 4,
            player_token: crate::net::ANY_TOKEN,
        })
    }

    /// The vertex colours one mesh carries, deduplicated and sorted so a failure reads the
    /// same way twice.
    fn tints(meshes: &Assets<Mesh>, handle: &Handle<Mesh>) -> Vec<[u8; 4]> {
        let mesh = meshes.get(handle).expect("the mesh exists");
        let Some(VertexAttributeValues::Float32x4(colours)) = mesh.attribute(Mesh::ATTRIBUTE_COLOR)
        else {
            return Vec::new();
        };
        // Quantised, because these are compared for identity rather than measured and two
        // f32 that print the same must not sort apart.
        let mut seen: Vec<[u8; 4]> = colours
            .iter()
            .map(|c| c.map(|channel| (channel * 255.0).round() as u8))
            .collect();
        seen.sort_unstable();
        seen.dedup();
        seen
    }

    /// **The rusty sword is iron with rust on it**, not one flat colour.
    ///
    /// Asserted as *two* vertex tints on one mesh, and as the marks being a shade of the
    /// base rather than a colour beside it: white is identity, so the iron that comes
    /// through is whatever `player/items.rs` says the sword presents as. That is what keeps
    /// that table the one answer — change the sword's palette entry and the rust follows it.
    #[test]
    fn the_rusty_sword_carries_iron_and_rust_on_one_mesh() {
        let mut app = app();
        app.update();

        let visuals = app.world().resource::<HandVisuals>();
        let rusted = visuals.rusted_blade.clone();
        let plain = visuals.blade.clone();
        let meshes = app.world().resource::<Assets<Mesh>>();

        let marks = tints(meshes, &rusted);
        assert_eq!(
            marks.len(),
            2,
            "the rusty blade carries {} tints, want iron and rust: {marks:?}",
            marks.len()
        );
        assert!(
            marks.contains(&[255, 255, 255, 255]),
            "no vertex carries identity, so the item's own palette entry never shows through"
        );
        let rust = RUST_TINT.map(|channel| (channel * 255.0).round() as u8);
        assert!(marks.contains(&rust), "no vertex carries the rust tint");

        // And the iron sword is not rusty: it is the same `ItemShape::Blade` and must not
        // inherit one blade's condition. It carries no vertex colours at all — an absent
        // attribute is how a mesh takes its material's colour whole, which is what every
        // other held shape does and what the rusted blade opts out of.
        assert_eq!(
            tints(meshes, &plain),
            Vec::<[u8; 4]>::new(),
            "the plain blade carries vertex colours, so it is no longer simply its material"
        );
        assert_ne!(rusted, plain, "both swords share one mesh");
    }

    /// The rust reaches the screen only for the sword it belongs to.
    ///
    /// Read through the mesh the hand is actually built from, so it is the routing under
    /// test rather than the table: holding the iron sword must not produce the rusted mesh.
    #[test]
    fn only_the_rusty_sword_is_drawn_rusted() {
        let mut app = app();
        app.update();
        let rusted = app.world().resource::<HandVisuals>().rusted_blade.clone();

        for (item_id, want_rusted) in [(ITEM_RUSTY_SWORD, true), (ITEM_IRON_SWORD, false)] {
            *app.world_mut().resource_mut::<Inventory>() =
                Inventory::from_stacks(vec![InventoryStack {
                    item_id,
                    count: 1,
                    ..Default::default()
                }]);
            *app.world_mut().resource_mut::<SelectedSlot>() = SelectedSlot(0);
            app.update();

            let drawn = held(&mut app).0;
            let _ = drawn;
            let world = app.world_mut();
            let mut query = world.query_filtered::<&Mesh3d, With<HeldItem>>();
            let mesh = query.single(world).expect("one held view model").0.clone();
            assert_eq!(
                mesh == rusted,
                want_rusted,
                "item {item_id} drawn with the rusted blade = {}, want {want_rusted}",
                mesh == rusted
            );
        }
    }

    /// **The empty hand is a fist**, which is more than one box and still fits the same one.
    ///
    /// The count is what says it is not the single cuboid it was — a cube is 24 vertices —
    /// and the extent is what says nothing about where the hand sits or how far it swings
    /// moved, which is the half of this that could have broken the swing tests silently.
    #[test]
    fn the_empty_hand_is_a_fist_inside_the_box_the_cuboid_filled() {
        let mut app = app();
        app.update();

        let hand = app.world().resource::<HandVisuals>().hand.clone();
        let meshes = app.world().resource::<Assets<Mesh>>();
        let mesh = meshes.get(&hand).expect("the hand mesh");

        assert!(
            mesh.count_vertices() > 24,
            "the hand is {} vertices, which is one box — a fist is a palm and knuckles",
            mesh.count_vertices()
        );

        let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("the hand must carry Float32x3 positions");
        };
        for (axis, size) in [HAND_SIZE.x, HAND_SIZE.y, HAND_SIZE.z]
            .into_iter()
            .enumerate()
        {
            let min = positions
                .iter()
                .map(|p| p[axis])
                .fold(f32::INFINITY, f32::min);
            let max = positions
                .iter()
                .map(|p| p[axis])
                .fold(f32::NEG_INFINITY, f32::max);
            assert!(
                (max - min - size).abs() < 1e-5,
                "the fist spans {} on axis {axis}, and HAND_SIZE says {size}",
                max - min
            );
        }
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .insert_resource(session())
            .insert_resource(Inventory::from_stacks(vec![
                InventoryStack {
                    item_id: ITEM_STONE,
                    count: 2,
                    ..Default::default()
                },
                InventoryStack {
                    item_id: ITEM_RAW_COAL,
                    count: 1,
                    ..Default::default()
                },
                InventoryStack {
                    item_id: 0,
                    count: 0,
                    ..Default::default()
                },
                InventoryStack {
                    item_id: u16::MAX,
                    count: 1,
                    ..Default::default()
                },
            ]))
            .insert_resource(SelectedSlot(0))
            .add_plugins(PlayerPlugin);
        app.update();
        app
    }

    fn held(app: &mut App) -> (HeldItem, Visibility, Entity) {
        let world = app.world_mut();
        let mut query = world.query::<(&HeldItem, &Visibility, &ChildOf)>();
        let (item, visibility, parent) = query.single(world).expect("one held view model");
        (*item, *visibility, parent.parent())
    }

    #[test]
    fn held_shapes_follow_the_selected_slot_on_that_frame() {
        let mut app = app();
        assert_eq!(held(&mut app).0.shape, Some(ItemShape::Block));

        for (slot, expected) in [
            (1, Some(ItemShape::Material)),
            (2, None),
            (3, Some(ItemShape::Material)),
        ] {
            *app.world_mut().resource_mut::<SelectedSlot>() = SelectedSlot(slot);
            app.update();
            assert_eq!(held(&mut app).0.shape, expected, "slot {slot}");
        }
    }

    #[test]
    fn the_view_model_is_parented_to_the_only_world_camera() {
        let mut app = app();
        let parent = held(&mut app).2;
        assert!(
            app.world().entity(parent).contains::<WorldCamera>(),
            "the held item was left in world space"
        );
        let Projection::Perspective(projection) = app
            .world()
            .get::<Projection>(parent)
            .expect("the world camera has a projection")
        else {
            panic!("the world camera is perspective");
        };
        let largest_depth = HAND_SIZE
            .z
            .max(BLOCK_EDGE)
            .max(MATERIAL_RADIUS * 2.0)
            .max(BUNDLE_SIZE.z);
        assert!(
            -BASE_TRANSLATION.z - largest_depth / 2.0 > projection.near,
            "the held mesh crosses the camera near plane"
        );
    }

    #[test]
    fn unknown_items_use_a_distinct_shape_and_the_palette_fallback() {
        let mut app = app();
        *app.world_mut().resource_mut::<SelectedSlot>() = SelectedSlot(3);
        app.update();

        let world = app.world_mut();
        let mut query = world.query::<(&HeldItem, &MeshMaterial3d<StandardMaterial>)>();
        let (held, handle) = query.single(world).expect("one held item");
        assert_eq!(held.shape, Some(ItemShape::Material));
        let material = world
            .resource::<Assets<StandardMaterial>>()
            .get(&handle.0)
            .expect("the held material");
        let [r, g, b, a] = palette::linear_rgba(u16::MAX);
        assert_eq!(material.base_color, Color::linear_rgba(r, g, b, a));
    }

    #[test]
    fn third_person_hides_the_view_model_without_removing_it() {
        // **The bug this file had**: the model is a child of the camera, and #172 moved the
        // camera four blocks back without giving this system the term that removes a
        // first-person conceit there — so the held item floated between the camera and the
        // character (#194).
        //
        // Asserted on the entity as well as the visibility, because *without removing it* is
        // half the contract: the model is the same one afterwards, so a toggle costs no mesh
        // and no material.
        let mut app = app();
        let (_, visibility, _) = held(&mut app);
        assert_eq!(visibility, Visibility::Visible, "first person draws it");
        let before = held(&mut app).0;

        *app.world_mut().resource_mut::<ViewMode>() = ViewMode::ThirdPerson;
        app.update();
        assert_eq!(held(&mut app).1, Visibility::Hidden);
        assert_eq!(
            held(&mut app).0,
            before,
            "the view toggle rebuilt the model instead of hiding it"
        );

        *app.world_mut().resource_mut::<ViewMode>() = ViewMode::FirstPerson;
        app.update();
        assert_eq!(held(&mut app).1, Visibility::Visible);
    }

    #[test]
    fn inventory_and_menu_hide_the_view_model_without_removing_it() {
        let mut app = app();
        assert_eq!(held(&mut app).1, Visibility::Visible);

        for mode in [InputMode::Inventory, InputMode::Menu] {
            *app.world_mut().resource_mut::<InputMode>() = mode;
            app.update();
            assert_eq!(held(&mut app).1, Visibility::Hidden, "mode {mode:?}");
        }
    }

    #[test]
    fn mining_loops_while_placement_is_one_distinct_bump() {
        let resting = animated_transform(&HandAnimation::default());
        let swinging = animated_transform(&HandAnimation {
            swing_elapsed: Duration::from_millis(50),
            bump_elapsed: None,
            ..Default::default()
        });
        let bumping = animated_transform(&HandAnimation {
            swing_elapsed: Duration::ZERO,
            bump_elapsed: Some(PLACE_BUMP_TIME / 2),
            ..Default::default()
        });

        assert_ne!(swinging.rotation, resting.rotation, "mining did not swing");
        assert_eq!(
            animated_transform(&HandAnimation {
                swing_elapsed: Duration::ZERO,
                bump_elapsed: None,
                ..Default::default()
            }),
            resting,
            "stopping mining did not return to rest"
        );
        assert!(
            bumping.translation.z > resting.translation.z,
            "placement did not make its short forward bump"
        );
        assert_ne!(
            bumping.rotation, swinging.rotation,
            "placement reused the mining pose"
        );
    }
    /// The blade is a shape of its own, so the thing that swings does not look like the
    /// thing that places.
    #[test]
    fn the_rusty_sword_is_held_as_a_blade() {
        let blade = selected_appearance(Some(InventoryStack {
            item_id: combat::ITEM_RUSTY_SWORD,
            count: 1,
            durability: 100,
            max_durability: 100,
        }));
        assert_eq!(blade.shape, Some(ItemShape::Blade));
        assert_eq!(blade.item_id, Some(combat::ITEM_RUSTY_SWORD));

        // A worn-through blade is still a blade in the hand. Whether it *swings* is
        // `super::combat`'s question and the server's answer; this module only draws.
        let worn = selected_appearance(Some(InventoryStack {
            item_id: combat::ITEM_RUSTY_SWORD,
            count: 1,
            durability: 0,
            max_durability: 100,
        }));
        assert_eq!(worn.shape, Some(ItemShape::Blade));

        // And the mapping is cosmetic: it cannot turn another item into a weapon.
        let stone = selected_appearance(Some(InventoryStack {
            item_id: ITEM_STONE,
            count: 1,
            ..Default::default()
        }));
        assert_eq!(stone.shape, Some(ItemShape::Block));
    }

    /// The three items that plant an entity rather than a voxel. The hand is where a
    /// player sees which of them the place press is about to ask for, so a bundle is its
    /// own shape rather than another cube.
    #[test]
    fn a_tent_a_forge_and_a_campfire_are_held_as_bundles() {
        let bundles = [
            structures::ITEM_TENT,
            structures::ITEM_FORGE,
            structures::ITEM_CAMPFIRE,
        ];
        let carried = bundles.map(|item_id| {
            let held = selected_appearance(Some(InventoryStack {
                item_id,
                count: 1,
                ..Default::default()
            }));
            assert_eq!(held.shape, Some(ItemShape::Bundle), "item {item_id}");
            assert_eq!(held.item_id, Some(item_id));
            held
        });

        // Three bundles, three colours: canvas, iron and firewood are what a player is
        // carrying, and two that looked alike would be slots they had to count to tell
        // apart.
        for (first, second) in [(0, 1), (0, 2), (1, 2)] {
            assert_ne!(
                palette::linear_rgba(carried[first].palette_id),
                palette::linear_rgba(carried[second].palette_id),
                "items {} and {} are carried in the same colour",
                bundles[first],
                bundles[second]
            );
        }

        // And an id none of them names is still the placeholder rather than a bundle.
        let unknown = selected_appearance(Some(InventoryStack {
            item_id: u16::MAX,
            count: 1,
            ..Default::default()
        }));
        assert_eq!(unknown.shape, Some(ItemShape::Material));
    }

    /// The forge's two products, once a player has made one.
    ///
    /// The blade is a blade — the shape says *this swings* rather than *this places* — and
    /// it is a different colour from the rusty one, because a pack holding both is two
    /// slots a player has to tell apart. The stone is a consumable and reads as material.
    #[test]
    fn the_iron_blade_and_the_sharpening_stone_have_shapes_of_their_own() {
        let iron = selected_appearance(Some(InventoryStack {
            item_id: crafting::ITEM_IRON_SWORD,
            count: 1,
            durability: 200,
            max_durability: 200,
        }));
        assert_eq!(iron.shape, Some(ItemShape::Blade));
        assert_eq!(iron.item_id, Some(crafting::ITEM_IRON_SWORD));

        let rusty = selected_appearance(Some(InventoryStack {
            item_id: combat::ITEM_RUSTY_SWORD,
            count: 1,
            durability: 100,
            max_durability: 100,
        }));
        assert_ne!(
            palette::linear_rgba(iron.palette_id),
            palette::linear_rgba(rusty.palette_id),
            "the two blades are carried in the same colour"
        );

        let stone = selected_appearance(Some(InventoryStack {
            item_id: crafting::ITEM_SHARPENING_STONE,
            count: 4,
            ..Default::default()
        }));
        assert_eq!(stone.shape, Some(ItemShape::Material));
        assert_eq!(stone.item_id, Some(crafting::ITEM_SHARPENING_STONE));

        // Neither is the placeholder any more: an id this build knows must not draw as a
        // version skew.
        for known in [crafting::ITEM_IRON_SWORD, crafting::ITEM_SHARPENING_STONE] {
            assert_ne!(
                palette::linear_rgba(items::item_palette_id(known)),
                palette::linear_rgba(u16::MAX),
                "item {known} still draws as an unknown id"
            );
        }
    }

    /// The panel and the hand read one opinion, so a stack cannot be two colours at once.
    #[test]
    fn the_swatch_a_panel_draws_is_the_one_the_hand_is_built_from() {
        for item_id in [
            ITEM_STONE,
            ITEM_LOG,
            ITEM_RAW_COAL,
            ITEM_RAW_IRON,
            combat::ITEM_RUSTY_SWORD,
            structures::ITEM_TENT,
            structures::ITEM_FORGE,
            crafting::ITEM_IRON_SWORD,
            crafting::ITEM_SHARPENING_STONE,
        ] {
            assert_eq!(
                items::item_palette_id(item_id),
                selected_appearance(Some(InventoryStack {
                    item_id,
                    count: 1,
                    ..Default::default()
                }))
                .palette_id,
                "item {item_id}"
            );
        }

        // And an id from a newer contract still reaches the palette's loud placeholder
        // rather than a plausible shade this module invented.
        assert_eq!(items::item_palette_id(u16::MAX), u16::MAX);
    }

    /// One swing per message, on the frame the request left.
    #[test]
    fn a_sent_swing_moves_the_view_model_and_then_settles() {
        let resting = animated_transform(&HandAnimation::default());
        let swinging = animated_transform(&HandAnimation {
            attack_elapsed: Some(ATTACK_SWING_TIME / 2),
            ..Default::default()
        });
        assert_ne!(
            resting, swinging,
            "a swing left the view model exactly where it was"
        );

        // The arc is out and back: its ends match rest, so nothing is left leaning.
        let started = animated_transform(&HandAnimation {
            attack_elapsed: Some(Duration::ZERO),
            ..Default::default()
        });
        let finished = animated_transform(&HandAnimation {
            attack_elapsed: Some(ATTACK_SWING_TIME),
            ..Default::default()
        });
        // Compared with a tolerance rather than exactly: `sin(PI)` is an ulp away from
        // zero, not zero, so an exact comparison here would be asserting the accuracy of
        // the sine rather than the shape of the arc.
        assert!(started.rotation.abs_diff_eq(resting.rotation, 1e-5));
        assert!(
            finished.rotation.abs_diff_eq(resting.rotation, 1e-5),
            "the swing left the view model leaning at {:?}",
            finished.rotation
        );
    }
}
