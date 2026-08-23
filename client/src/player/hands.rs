//! The first-person held item: a camera child, never a world entity.
//!
//! The selected authoritative stack chooses only a presentation, and this module no
//! longer holds an opinion about what that is: [`super::items`] owns the shape and the
//! colour every item draws in, and the hand reads them exactly as the pack cells and the
//! recipe panel do. What stays here is the view model itself — the meshes, the camera-space
//! placement and the cosmetic swing. None of it is a legality table: it cannot place,
//! consume or reject anything, and an unknown id remains visible through the palette
//! fallback.
//!
//! Mining progress does now enter this module, and only in one direction. The mining
//! loop is *started and stopped* by the authoritative [`super::target::MiningFeedback`]
//! and by nothing else; local time supplies the cadence of one punch and nothing else.
//! There is no timer, no hardness table and no button in that decision, so the hand
//! cannot animate a break the server has not granted and cannot outlast one it has.

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
use super::target::{ApplyMiningFeedback, ApplyTargetInput, BlockTarget, MiningFeedback};
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

/// The mining loop's cadence, and how far one punch carries the view model.
///
/// **All three are cosmetic, and the cadence in particular is not a clock.** How fast the
/// hand punches says nothing about how fast the block is coming apart: a punch takes the
/// same time on dirt as on stone, and the loop simply repeats for as long as
/// [`HandIntent::mining`] — the server's own answer — stays true.
const MINE_PUNCHES_PER_SECOND: f32 = 2.4;
const MINE_PUNCH_RADIANS: f32 = 0.42;

/// How far the fist reaches away from the camera at full extension.
///
/// **Toward the block, so along -Z**, which is deliberately the opposite of
/// [`PLACE_BUMP_DISTANCE`]: a punch reaches for what it is breaking and a placement draws
/// back from what it just set down. Two animations on one axis have to be told apart at a
/// glance, and a shared direction is the first thing that stops being possible once a
/// third one lands here.
const MINE_PUNCH_DISTANCE: f32 = 0.045;

const PLACE_BUMP_TIME: Duration = Duration::from_millis(150);
const PLACE_BUMP_DISTANCE: f32 = 0.025;

/// How long one attack swing plays for, whichever of the three shapes is playing.
///
/// A one-shot, unlike the mining loop above, which repeats while the server reports
/// progress: an attack is an event the server judges once, so its feedback happens once.
///
/// **One duration for all three shapes, and that is a decision rather than a convenience.**
/// A cut that took longer than a thrust would put the drawn shape into the *timing* of the
/// hand, and timing is the one presentation channel a cooldown also lives in. Three arcs
/// that differ in geometry alone cannot be read as three tempos, so nothing a player sees
/// here can be mistaken for the server changing its mind about how often a blade swings.
const ATTACK_SWING_TIME: Duration = Duration::from_millis(220);

/// The overhead cut: how far it carries the blade down and over.
///
/// Unchanged from when this was the only swing there was, so the arc a player already knows
/// is still one of the three and is still the first one drawn.
const OVERHEAD_PITCH_RADIANS: f32 = 0.9;

/// The lateral slash: how far it sweeps across the view, and how far the edge turns over
/// into that sweep.
///
/// Two terms because one of them is what makes it a slash rather than a pan — a blade held
/// upright and moved sideways reads as a wiper blade, and the roll is what puts an edge on
/// the front of the motion.
const LATERAL_YAW_RADIANS: f32 = 1.05;
const LATERAL_ROLL_RADIANS: f32 = 0.75;

/// The thrust: how far it drives along the view, and how far the tip levels out of the rest
/// pose's lean on the way.
///
/// **The reach is the shape and the level-out is a detail**, which is deliberately the
/// opposite balance to [`OVERHEAD_PITCH_RADIANS`] above. The two arcs share the pitch axis,
/// so if they shared its magnitude as well a thrust would read as a smaller chop; what tells
/// them apart is that one is almost all rotation and the other almost all travel.
///
/// Along -Z, the direction [`MINE_PUNCH_DISTANCE`] already established for *toward the thing
/// being hit*, and the opposite of [`PLACE_BUMP_DISTANCE`]'s draw-back.
const THRUST_REACH: f32 = 0.11;
const THRUST_LEVEL_RADIANS: f32 = 0.35;

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
    //
    // The mark is therefore thicker than the blade and centred on it, so **one mark wraps
    // both flat faces** rather than sitting on one of them. That is what `proud` forces
    // rather than something it permits: a mark pushed onto a single face travels half of
    // `proud` to get there, which lands its *other* face exactly on the plane of the
    // blade's — coplanar, differently coloured, overlapping, which is the flicker rule 2 in
    // `client/AGENTS.md` names for the body rig, arriving here by the same door.
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
            // Alternating across the blade's *width*, so the three do not read as one
            // stripe down the middle of it as it turns. Not across its two faces: every
            // mark is on both of those, for the reason above.
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
            // `BlockTargetPlugin` owns this one, and it is here for the same reason.
            .init_resource::<MiningFeedback>()
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
                    // After this frame's authoritative progress has been applied, so the
                    // punch starts and stops on the frame the server's answer changed
                    // rather than the one after it. `ApplyTargetInput` already implies it
                    // today — `player/target.rs` chains the two — but what this module
                    // requires is the progress, not the request that follows it, and an
                    // ordering it depends on should be one it states.
                    .after(ApplyMiningFeedback)
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

/// Which of the three arcs an attack draws.
///
/// **Presentation, and it is worth being exact about how far that goes.** The shape is
/// chosen in this module, from a counter in [`HandAnimation`] that [`swing_pose`] is the
/// only reader of; it reaches no request, no predicate and no other module. `super::combat`
/// routes the left button on the item id and sends the same `AttackRequest` whichever arc is
/// about to play, and the server judges the blow against its own registry — so which picture
/// played cannot change reach, damage, cooldown or what was asked for. It is the rule
/// `client/AGENTS.md` states for the item table, arriving by a different door: drawing an
/// item as a blade no more swings it than holding it as one does, and drawing a thrust
/// reaches no further than drawing a cut.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum SwingShape {
    /// Down and over: the arc this file had when it had one.
    #[default]
    Overhead,
    /// Across the view, with the edge turning over into the sweep.
    Lateral,
    /// Straight along the view, with the tip levelling as it goes.
    Thrust,
}

impl SwingShape {
    /// Every shape, for the sweeps that must cover the whole vocabulary.
    ///
    /// The same hand-written list, for the same reason, as `items::ItemShape::ALL`: no
    /// stable Rust enumerates variants. And as there, the list is not what makes a shape
    /// *drawn* — [`swing_pose`] and [`Self::after`] both match with no wildcard arm, so a
    /// fourth variant fails to build until it has been given an arc and a place in the
    /// rotation. What the list buys is the other half: a sweep that catches an arm filled
    /// in with a copy of its neighbour.
    ///
    /// `#[cfg(test)]` because nothing in the running client enumerates the shapes — the
    /// rotation walks them one at a time and never needs the set. That is where
    /// `ItemShape::ALL` also sat until a runtime reader turned up for it, and the day one
    /// turns up here the attribute comes off rather than the list changing.
    #[cfg(test)]
    const ALL: [Self; 3] = [Self::Overhead, Self::Lateral, Self::Thrust];

    /// The shape that follows this one.
    ///
    /// **A fixed rotation rather than a random pick**, and the acceptance criterion is why:
    /// what a player must stop seeing is the same arc twice in a row, and random repeats.
    /// A cycle also makes *consecutive swings differ* a property one test can hold, rather
    /// than a distribution somebody has to sample.
    ///
    /// Exhaustive with no wildcard, so a fourth shape cannot be added without deciding
    /// where in the rotation it goes — the compiler's half of the guarantee, exactly as
    /// `items::ItemShape` arranges for the two renderers.
    fn after(self) -> Self {
        match self {
            Self::Overhead => Self::Lateral,
            Self::Lateral => Self::Thrust,
            Self::Thrust => Self::Overhead,
        }
    }
}

/// One attack swing in flight: which shape is playing, and how far into it the hand is.
///
/// The pair travels together because neither answers anything on its own — an elapsed time
/// with no shape draws nothing, and a shape with no elapsed time is a swing that is not
/// happening. Keeping them in one `Option` is what makes *no swing* a single state rather
/// than two fields that could disagree about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Swing {
    shape: SwingShape,
    elapsed: Duration,
}

/// How far one attack shape has carried the view model, as an offset from rest.
///
/// Four loose terms rather than a `Transform`, because they are *added* to whatever the
/// mining loop and the placement bump are already doing and two quaternions cannot be added.
/// Every term is zero at both ends of the arc, so a swing that finishes leaves the hand
/// exactly where it found it whichever shape played — which is the property
/// `a_sent_swing_moves_the_view_model_and_then_settles` has held since there was one arc,
/// and now holds three times over.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct SwingPose {
    /// About the camera's X axis. **Negative carries the blade over toward what is being
    /// hit** — the convention [`mine_punch`]'s caller set and the one this file keeps, so a
    /// third and a fourth animation never have to argue about which way *out* is.
    pitch: f32,
    /// About Y: across the view. Positive turns the blade toward -X, which is the far side
    /// of the screen from the hand — [`BASE_TRANSLATION`] puts it on the right — so a slash
    /// crosses the body instead of opening outward off the edge of the view.
    yaw: f32,
    /// About Z: the edge turning over.
    roll: f32,
    /// Along the view, in the same units as [`MINE_PUNCH_DISTANCE`]. **Negative reaches away
    /// from the camera**, toward what is being hit, for the same reason and on the same axis.
    reach: f32,
}

/// Where one shape has carried the hand, a given fraction of the way through its arc.
///
/// One envelope for all three — `sin(fraction * PI)`, out and back, zero at both ends — and
/// three sets of terms to apply it to. The shapes are told apart by *which* degree of freedom
/// each one is mostly made of: the cut is pitch, the slash is yaw, the thrust is reach. That
/// is what `each_shape_leads_with_a_channel_of_its_own` pins, and it is a stronger statement
/// than "the three poses differ", which three near-identical arcs would also satisfy.
fn swing_pose(shape: SwingShape, elapsed: Duration) -> SwingPose {
    let fraction = (elapsed.as_secs_f32() / ATTACK_SWING_TIME.as_secs_f32()).clamp(0.0, 1.0);
    let arc = (fraction * PI).sin();
    match shape {
        SwingShape::Overhead => SwingPose {
            pitch: -arc * OVERHEAD_PITCH_RADIANS,
            ..default()
        },
        SwingShape::Lateral => SwingPose {
            yaw: arc * LATERAL_YAW_RADIANS,
            roll: -arc * LATERAL_ROLL_RADIANS,
            ..default()
        },
        SwingShape::Thrust => SwingPose {
            pitch: -arc * THRUST_LEVEL_RADIANS,
            reach: -arc * THRUST_REACH,
            ..default()
        },
    }
}

#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
struct HandAnimation {
    /// How long the mining loop has been running, and zero the moment it is not.
    ///
    /// **Local time under an authoritative gate, never a measure of the break.** It says
    /// where in a punch the hand is; how far along the block is, is a byte the server
    /// sends and `ui/crosshair.rs` draws. Nothing reads a break out of this field, which
    /// is what stops the animation from becoming a second opinion about one.
    mine_elapsed: Duration,
    bump_elapsed: Option<Duration>,

    /// The attack swing playing right now, if one is. Started by a `SwingSent` message and
    /// by nothing else, so it plays exactly when a request left this client — whether that
    /// request later hits, misses or is refused.
    attack: Option<Swing>,

    /// Which shape the *next* swing will take.
    ///
    /// **The alternation is one field of local presentation state, and it is advanced by a
    /// request leaving rather than by any answer to one.** That is what makes it survive a
    /// swing the server refuses: a refusal is silence on this side — nothing comes back for
    /// a blow that is declined, the same silence a refused block edit produces — so there is
    /// no answer to wait for and none is waited for. Three clicks the server declines draw
    /// three different arcs, because all three requests left.
    ///
    /// It outlives the swing it belongs to on purpose. [`Self::attack`] is `None` between
    /// swings, so a cursor kept inside it would forget which arc had just played and the
    /// next press could repeat it.
    ///
    /// Nothing outside this module can read the field — [`HandAnimation`] is private — and
    /// nothing inside it consults the field for anything but which arc to draw.
    next_swing: SwingShape,
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

/// What the hand is reacting to this frame: one authoritative fact and two local presses.
///
/// A bundle rather than five parameters, for the reason [`HandWardrobe`] is one —
/// [`animate_view_model`] was already at clippy's argument bound, and *what is the hand
/// doing* is one question that should have one place to be asked. It is also where the
/// rule below is written down once, so the next animation this file grows has somewhere
/// to read the answer rather than somewhere to re-decide it.
#[derive(SystemParam)]
struct HandIntent<'w, 's> {
    mode: Res<'w, InputMode>,
    buttons: Option<Res<'w, ButtonInput<MouseButton>>>,
    target: Res<'w, BlockTarget>,
    feedback: Res<'w, MiningFeedback>,
    swings: MessageReader<'w, 's, SwingSent>,
}

impl HandIntent<'_, '_> {
    /// Whether gameplay input counts this frame. A mode transition belongs to the UI for
    /// the whole of it, which is how `target::send_block_edits` reads the same thing.
    fn playing(&self) -> bool {
        *self.mode == InputMode::Playing && !self.mode.is_changed()
    }

    /// **Whether the server says a block is coming apart under this crosshair right now,
    /// and the hand is on screen to be shown doing it.**
    ///
    /// [`MiningFeedback`] is the whole of the *gameplay* answer, and deliberately the whole
    /// of it. It holds a byte the server sent; it is cleared by the zero frame a server-side reset
    /// sends, cleared when the crosshair leaves the voxel that byte describes, and expired
    /// after `PROGRESS_SILENCE_TICKS` of silence. So *the block broke*, *the player looked
    /// away* and *the request was refused and nothing came back* are already one fact by
    /// the time it gets here, and not one of the three is this module's to work out.
    ///
    /// **The button is deliberately not in this predicate.** A held button is a request,
    /// not an outcome: a hand that punched on the press would be animating a break the
    /// server had not granted yet, which is the local clock this file must never grow —
    /// the same mistake as advancing progress locally, wearing a different hat. Reading
    /// the resource instead also keeps the two presentations of one fact in step, because
    /// `ui/crosshair.rs` fills its ring from this very resource: the hand and the ring
    /// start together, hold through the same silence, and stop together.
    ///
    /// **[`Self::playing`] is in it, and it is not a second opinion about mining.** It
    /// answers a different question — does this frame's hand belong to the world at all —
    /// and it is the same UI-state gate [`Self::placing`] takes and
    /// `target::send_block_edits` takes. All it can do is stop the punch being *drawn*
    /// while the pack or the pause menu owns the screen: it advances no progress, times no
    /// break, and decides nothing about whether one happened. Every question about what is
    /// coming apart still has exactly one answer, and it is the byte above.
    ///
    /// It has to be here rather than left to the crosshair, because the byte outlives the
    /// transition. Nothing orders [`super::ApplyInputMode`] before
    /// [`ApplyMiningFeedback`], so on the frame the mode changes the feedback can still be
    /// the one computed while the player was aiming — and the hand would go on punching
    /// behind an open inventory until the next frame's raycast reported nothing targeted.
    /// It is also what keeps the paragraph above true: `ui/crosshair.rs` hides its whole
    /// root on this same mode test, so without the term here the ring and the hand would
    /// stop on different frames — the one thing reading a shared resource was meant to
    /// prevent.
    fn mining(&self) -> bool {
        self.playing() && self.feedback.progress() != 0
    }

    /// A press that asked for a block somewhere there is room to put one.
    fn placing(&self) -> bool {
        self.playing()
            && self
                .buttons
                .as_deref()
                .is_some_and(|buttons| buttons.just_pressed(MouseButton::Right))
            && self.target.0.and_then(|hit| hit.place_target()).is_some()
    }

    /// Whether a swing request left this client this frame.
    fn swing_sent(&mut self) -> bool {
        self.swings.read().next().is_some()
    }
}

fn animate_view_model(
    time: Res<Time>,
    mut intent: HandIntent<'_, '_>,
    mut animation: ResMut<HandAnimation>,
    mut held: Query<&mut Transform, With<HeldItem>>,
) {
    let mut next_animation = *animation;
    // The loop runs exactly while the server's answer says it should, and resets the
    // instant it does not — so a break, a look-away and a refusal all end it, without this
    // module knowing which of the three happened. Opening the pack ends it too, which is
    // the screen changing hands rather than a fourth thing the server said. See
    // [`HandIntent::mining`].
    if intent.mining() {
        next_animation.mine_elapsed += time.delta();
    } else {
        next_animation.mine_elapsed = Duration::ZERO;
    }

    // One swing per message, restarted rather than queued: two clicks inside one
    // animation should look like two swings, and the second server-side request is
    // refused by the cooldown either way.
    //
    // **This is where the shape is chosen, and it is the only place it is.** The cursor
    // advances on the request having left — the same message, on the same frame, that
    // starts the arc — so a swing that is refused, missed or answered by nothing at all
    // still moves the rotation on. Restarting a swing therefore takes the next shape too,
    // which is what makes two clicks inside one animation read as two swings rather than
    // as one arc that stuttered.
    if intent.swing_sent() {
        next_animation.attack = Some(Swing {
            shape: next_animation.next_swing,
            elapsed: Duration::ZERO,
        });
        next_animation.next_swing = next_animation.next_swing.after();
    }
    if let Some(swing) = next_animation.attack.as_mut() {
        swing.elapsed += time.delta();
        if swing.elapsed >= ATTACK_SWING_TIME {
            next_animation.attack = None;
        }
    }
    if intent.placing() {
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
    let punch = mine_punch(animation.mine_elapsed);
    // Whichever arc is in flight, out and back, added to whatever the mining loop is doing.
    // The two never run together in practice — a blade suppresses mining — and summing
    // rather than branching keeps the transform one expression, which is what lets a third
    // and a fourth animation land here without a precedence rule.
    let swing = animation.attack.map_or_else(SwingPose::default, |attack| {
        swing_pose(attack.shape, attack.elapsed)
    });
    let bump = animation.bump_elapsed.map_or(0.0, |elapsed| {
        let fraction = (elapsed.as_secs_f32() / PLACE_BUMP_TIME.as_secs_f32()).clamp(0.0, 1.0);
        (fraction * PI).sin()
    });

    // Three animations on one axis, and the signs are the convention rather than an
    // accident: a placement draws back from the block it just set down, a punch reaches for
    // the one it is breaking, and a thrust reaches the same way a punch does.
    let along_view = bump * PLACE_BUMP_DISTANCE - punch * MINE_PUNCH_DISTANCE + swing.reach;

    Transform {
        translation: BASE_TRANSLATION + Vec3::Z * along_view,
        // The mining punch is negative here for the reason `SwingPose::pitch` is negative
        // for a cut: one convention for *over toward what is being hit*, kept by every
        // animation in this file.
        rotation: Quat::from_rotation_x(-0.18 - punch * MINE_PUNCH_RADIANS + swing.pitch)
            // Identity at rest and for two of the three shapes, so nothing about where the
            // hand sits or how it mines moves for the sake of the slash that needs it.
            * Quat::from_rotation_y(swing.yaw)
            * Quat::from_rotation_z(-0.12 - bump * 0.18 + swing.roll),
        ..default()
    }
}

/// How far through one punch the mining loop is: `0.0` at rest, `1.0` at full extension,
/// back to `0.0` at the end of the cycle, repeating.
///
/// `(1 - cos)/2` rather than a sine, and that is the difference between punching and
/// shaking. A sine is symmetric about rest, so half of every cycle drags the hand back
/// *behind* where it started; this never goes negative, so the loop only ever reaches out
/// and lets the hand return.
///
/// It is a function of local elapsed time and of nothing else. It is only ever consulted
/// while [`HandIntent::mining`] holds, and the caller zeroes its input the moment that
/// stops — so the phase says where in a punch the hand is, never how near the break is.
fn mine_punch(elapsed: Duration) -> f32 {
    let phase = elapsed.as_secs_f32() * MINE_PUNCHES_PER_SECOND * TAU;
    (1.0 - phase.cos()) * 0.5
}

#[cfg(test)]
mod tests {
    use bevy::asset::AssetPlugin;

    use bevy::mesh::VertexAttributeValues;
    use bevy::time::TimeUpdateStrategy;

    use super::super::crafting::ITEM_IRON_SWORD;
    use super::super::target::BlockHit;
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

    /// **Every rust mark wraps the blade; what alternates is where the three sit across it.**
    ///
    /// Pinned because neither half is what a reader guesses from `side`. A mark is thicker
    /// than the blade and centred on it, so it stands proud of *both* flat faces; offsetting
    /// one onto a single face instead would land its other face exactly on the plane of the
    /// blade's — coplanar, differently coloured, overlapping — which is the flicker `proud`
    /// exists to prevent. So this fails in that direction as readily as in the direction of
    /// losing the stagger, which is the point of asserting the geometry rather than the
    /// comment.
    #[test]
    fn every_rust_mark_wraps_the_blade_and_the_three_stagger_across_its_width() {
        let mut app = app();
        app.update();

        let rusted = app.world().resource::<HandVisuals>().rusted_blade.clone();
        let meshes = app.world().resource::<Assets<Mesh>>();
        let mesh = meshes.get(&rusted).expect("the rusted blade mesh");

        let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("the rusted blade must carry Float32x3 positions");
        };
        let Some(VertexAttributeValues::Float32x4(colours)) = mesh.attribute(Mesh::ATTRIBUTE_COLOR)
        else {
            panic!("the rusted blade must carry Float32x4 colours");
        };

        // Quantised for the reason `tints` quantises: these pick vertices out by identity
        // rather than measuring them.
        let rust = RUST_TINT.map(|channel| (channel * 255.0).round() as u8);
        let marks: Vec<[f32; 3]> = positions
            .iter()
            .zip(colours)
            .filter(|(_, colour)| colour.map(|channel| (channel * 255.0).round() as u8) == rust)
            .map(|(position, _)| *position)
            .collect();
        assert!(!marks.is_empty(), "no vertex carries the rust tint");

        // Grouped into marks by the y planes they sit on: a box contributes exactly two,
        // and the three heights do not meet. Asked of each mark rather than of all of them
        // together, because the aggregate span is wide enough to pass while every single
        // mark sits on one face — which is exactly the shape being ruled out.
        let plane = |value: f32| (value * 1e6).round() as i32;
        let mut heights: Vec<i32> = marks.iter().map(|p| plane(p[1])).collect();
        heights.sort_unstable();
        heights.dedup();
        assert!(
            heights.len() >= 2 && heights.len().is_multiple_of(2),
            "the rust sits on {} y planes, which is not a whole number of marks",
            heights.len()
        );

        // Both faces: each mark reaches past the blade's own thickness on each side of it.
        let half = BLADE_SIZE.x / 2.0;
        for (index, bounds) in heights.chunks(2).enumerate() {
            let [bottom, top] = bounds else {
                unreachable!("an even number of planes chunks into pairs")
            };
            let one: Vec<[f32; 3]> = marks
                .iter()
                .copied()
                .filter(|p| plane(p[1]) == *bottom || plane(p[1]) == *top)
                .collect();
            let min_x = one.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min);
            let max_x = one.iter().map(|p| p[0]).fold(f32::NEG_INFINITY, f32::max);
            assert!(
                min_x < -half && max_x > half,
                "mark {index} spans x {min_x}..{max_x} against a blade of ±{half}, so it \
                 sits on one face instead of wrapping both"
            );
        }

        // Staggered: three marks in one column would put every rust vertex on the same two
        // z planes.
        let mut planes: Vec<i32> = marks.iter().map(|p| plane(p[2])).collect();
        planes.sort_unstable();
        planes.dedup();
        assert!(
            planes.len() > 2,
            "the rust sits on {} z planes, so the three marks are one column down the width",
            planes.len()
        );

        // And clear of both edges, which is what keeps a mark reading as a patch rather
        // than as a chipped edge.
        let edge = BLADE_SIZE.z / 2.0;
        let min_z = marks.iter().map(|p| p[2]).fold(f32::INFINITY, f32::min);
        let max_z = marks.iter().map(|p| p[2]).fold(f32::NEG_INFINITY, f32::max);
        assert!(
            min_z > -edge && max_z < edge,
            "the rust spans z {min_z}..{max_z} against edges at ±{edge}, so a mark touches one"
        );
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
            mine_elapsed: Duration::from_millis(50),
            bump_elapsed: None,
            ..Default::default()
        });
        let bumping = animated_transform(&HandAnimation {
            mine_elapsed: Duration::ZERO,
            bump_elapsed: Some(PLACE_BUMP_TIME / 2),
            ..Default::default()
        });

        assert_ne!(swinging.rotation, resting.rotation, "mining did not swing");
        assert_eq!(
            animated_transform(&HandAnimation {
                mine_elapsed: Duration::ZERO,
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

    /// One transform for a swing of the named shape, `fraction` of the way through its arc.
    fn mid_swing(shape: SwingShape, fraction: f32) -> Transform {
        animated_transform(&HandAnimation {
            attack: Some(Swing {
                shape,
                elapsed: ATTACK_SWING_TIME.mul_f32(fraction),
            }),
            ..Default::default()
        })
    }

    /// One swing per message, on the frame the request left — and every shape settles.
    ///
    /// Swept over [`SwingShape::ALL`] rather than over the one arc this used to be: three
    /// shapes are three chances to leave the hand leaning, and the whole reason the pose is
    /// four loose terms added to rest is that each of them returns to zero.
    #[test]
    fn a_sent_swing_moves_the_view_model_and_then_settles() {
        let resting = animated_transform(&HandAnimation::default());

        for shape in SwingShape::ALL {
            let swinging = mid_swing(shape, 0.5);
            assert_ne!(
                resting, swinging,
                "{shape:?} left the view model exactly where it was"
            );

            // The arc is out and back: its ends match rest, so nothing is left leaning.
            // Compared with a tolerance rather than exactly: `sin(PI)` is an ulp away from
            // zero, not zero, so an exact comparison here would be asserting the accuracy
            // of the sine rather than the shape of the arc.
            for (edge, at) in [("started", 0.0), ("finished", 1.0)] {
                let pose = mid_swing(shape, at);
                assert!(
                    pose.rotation.abs_diff_eq(resting.rotation, 1e-5),
                    "{shape:?} {edge} leaning at {:?}",
                    pose.rotation
                );
                assert!(
                    pose.translation.abs_diff_eq(resting.translation, 1e-5),
                    "{shape:?} {edge} reaching at {:?}",
                    pose.translation
                );
            }
        }
    }

    /// **Three shapes, and each leads with a degree of freedom the other two do not.**
    ///
    /// The acceptance criterion asks for an overhead cut, a lateral slash and a thrust —
    /// three *different* things, not one arc scaled three ways. So what is asserted is not
    /// merely that the poses differ, which three near-identical arcs would also satisfy,
    /// but that each shape moves its own named channel furthest: the cut is pitch, the slash
    /// is yaw, the thrust is reach. A fourth shape that copied one of them would land on a
    /// channel already spoken for and this would fail.
    #[test]
    fn each_shape_leads_with_a_channel_of_its_own() {
        let peak: Vec<(SwingShape, SwingPose)> = SwingShape::ALL
            .into_iter()
            .map(|shape| (shape, swing_pose(shape, ATTACK_SWING_TIME / 2)))
            .collect();

        for (shape, name, channel, of) in [
            (
                SwingShape::Overhead,
                "the cut",
                "pitch",
                (|pose: &SwingPose| pose.pitch.abs()) as fn(&SwingPose) -> f32,
            ),
            (SwingShape::Lateral, "the slash", "yaw", |pose| {
                pose.yaw.abs()
            }),
            (SwingShape::Thrust, "the thrust", "reach", |pose| {
                pose.reach.abs()
            }),
        ] {
            let mine = peak
                .iter()
                .find(|(candidate, _)| *candidate == shape)
                .map(|(_, pose)| of(pose))
                .expect("every shape has a peak pose");
            assert!(mine > 0.0, "{name} does not move in {channel} at all");
            for (other, other_pose) in &peak {
                if *other == shape {
                    continue;
                }
                assert!(
                    of(other_pose) < mine,
                    "{name} was supposed to own {channel}, and {other:?} moves it as far"
                );
            }
        }

        // And no two poses are the same pose, which the channel argument implies but which
        // a reader should not have to derive.
        for (index, (shape, pose)) in peak.iter().enumerate() {
            for (other, other_pose) in &peak[index + 1..] {
                assert_ne!(pose, other_pose, "{shape:?} and {other:?} draw one arc");
            }
        }
    }

    /// The rotation visits all three and never repeats one back to back.
    ///
    /// Held over twice the length of the cycle, because a rotation that alternated between
    /// two shapes and dropped the third would satisfy "no two in a row" perfectly.
    #[test]
    fn the_rotation_never_draws_one_shape_twice_running() {
        let mut shape = SwingShape::default();
        let mut drawn = vec![shape];
        for _ in 0..(SwingShape::ALL.len() * 2) {
            shape = shape.after();
            assert_ne!(
                shape,
                *drawn.last().expect("the first shape is already in"),
                "the rotation repeated a shape: {drawn:?}"
            );
            drawn.push(shape);
        }
        for shape in SwingShape::ALL {
            assert!(
                drawn.contains(&shape),
                "{shape:?} is in the vocabulary and never drawn: {drawn:?}"
            );
        }
    }

    /// **A punch, not a wobble.** The hand reaches for the block, comes back, and the
    /// cycle closes on rest so the loop repeats from the same place however long it runs.
    #[test]
    fn the_mining_punch_reaches_for_the_block_and_comes_back() {
        let cycle = Duration::from_secs_f32(1.0 / MINE_PUNCHES_PER_SECOND);
        let resting = animated_transform(&HandAnimation::default());
        let extended = animated_transform(&HandAnimation {
            mine_elapsed: cycle / 2,
            ..Default::default()
        });

        // Away from the camera is -Z, so the fist reaches for what it is breaking.
        assert!(
            extended.translation.z < resting.translation.z,
            "the punch never carried the hand toward the block: {} against {} at rest",
            extended.translation.z,
            resting.translation.z
        );

        // And the other way from a placement, which draws back from the block it just set
        // down. Two animations sharing an axis have to be told apart at a glance.
        let bumping = animated_transform(&HandAnimation {
            bump_elapsed: Some(PLACE_BUMP_TIME / 2),
            ..Default::default()
        });
        assert!(
            bumping.translation.z > resting.translation.z,
            "the placement bump now travels the same way as the mining punch"
        );

        // Nothing is left extended or leaning at the end of one punch. Compared with a
        // tolerance for the reason the attack arc above is: `cos(TAU)` is an ulp from one.
        let closed = animated_transform(&HandAnimation {
            mine_elapsed: cycle,
            ..Default::default()
        });
        assert!(
            closed.translation.abs_diff_eq(resting.translation, 1e-5),
            "the punch left the hand out at {:?}",
            closed.translation
        );
        assert!(
            closed.rotation.abs_diff_eq(resting.rotation, 1e-5),
            "the punch left the hand leaning at {:?}",
            closed.rotation
        );

        // No part of the cycle pulls the hand back *behind* rest. That is the whole
        // difference between a punch and a shake, and it is the property a sine — which is
        // symmetric about rest — would not have had.
        for step in 0u8..=64 {
            let at = animated_transform(&HandAnimation {
                mine_elapsed: cycle.mul_f32(f32::from(step) / 64.0),
                ..Default::default()
            });
            assert!(
                at.translation.z <= resting.translation.z + 1e-6,
                "the punch pulled the hand back behind rest {step}/64 of the way through"
            );
        }
    }

    /// The view model with nothing beside it that writes [`MiningFeedback`].
    ///
    /// The full [`app`] above cannot answer this question: `BlockTargetPlugin` recomputes
    /// the feedback from the inbox and the crosshair every frame, and with no chunks
    /// loaded the raycast answers "nothing targeted" — which is one of the states that
    /// clears it. Here the test plays the server, which is the only way to say *the server
    /// reported this* and still have it be true when `animate_view_model` reads it.
    fn hand_only_app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            // What sibling plugins provide in the game: the aimed voxel from
            // `BlockTargetPlugin`, the swing message from `CombatPlugin`, the mouse from
            // Bevy's input plugin, and the pack from `InventoryPlugin`.
            .init_resource::<BlockTarget>()
            .init_resource::<ButtonInput<MouseButton>>()
            .add_message::<SwingSent>()
            .init_resource::<Inventory>()
            .init_resource::<InputMode>()
            .insert_resource(SelectedSlot(0))
            .add_plugins(HandsPlugin);
        app.update();
        app
    }

    /// **The loop is the server's to start and to stop, and the button's to do neither.**
    ///
    /// The three ways mining ends — the block broke, the player looked away, the request
    /// was refused and nothing came back — are already one fact by the time this module
    /// sees them: `MiningFeedback` reporting nothing. So the test says it the way the code
    /// reads it, and holds the button down throughout to show what is *not* driving this.
    #[test]
    fn the_mining_loop_starts_and_stops_on_the_servers_progress_alone() {
        const STEP: Duration = Duration::from_millis(16);

        let mut app = hand_only_app();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(STEP));

        // A held button and a voxel under the crosshair, and not one word from the server.
        // A hand on a local clock would already be punching here.
        app.world_mut()
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Left);
        *app.world_mut().resource_mut::<BlockTarget>() = BlockTarget(Some(BlockHit {
            block: IVec3::ZERO,
            face: IVec3::Y,
        }));
        app.update();
        app.update();
        assert_eq!(
            app.world().resource::<HandAnimation>().mine_elapsed,
            Duration::ZERO,
            "the hand punched on the press, before the server had granted anything"
        );

        // The server reports progress. Now, and only now, the loop runs.
        *app.world_mut().resource_mut::<MiningFeedback>() = MiningFeedback::for_test(64);
        app.update();
        app.update();
        assert_eq!(
            app.world().resource::<HandAnimation>().mine_elapsed,
            STEP * 2,
            "the server's progress did not start the loop"
        );

        // And the moment the server stops saying so, it resets rather than winding down.
        *app.world_mut().resource_mut::<MiningFeedback>() = MiningFeedback::default();
        app.update();
        assert_eq!(
            app.world().resource::<HandAnimation>().mine_elapsed,
            Duration::ZERO,
            "the hand kept punching after the server stopped reporting progress"
        );

        // The half that makes the two assertions above mean anything.
        assert!(
            app.world()
                .resource::<ButtonInput<MouseButton>>()
                .pressed(MouseButton::Left),
            "the button was released, so this test proved nothing about it"
        );
    }

    /// **The pack opening stops the hand, whatever the last byte from the server said.**
    ///
    /// The gate is [`HandIntent::playing`], and it is UI state rather than a second
    /// opinion about mining: it decides whether this frame's hand belongs to the world,
    /// not whether the block is coming apart. What makes it necessary is that the byte
    /// outlives the transition — nothing orders the input mode before the feedback that
    /// reads it, so the frame the inventory opens on can still be holding the progress
    /// computed while the player was aiming.
    ///
    /// So the test says exactly that: the server's answer is left untouched and the button
    /// is left held down, and both are asserted at the end. If either had changed, the
    /// reset below would be evidence about something other than the mode.
    #[test]
    fn a_mode_that_is_not_playing_stops_the_hand_the_server_is_still_feeding() {
        const STEP: Duration = Duration::from_millis(16);

        for mode in [InputMode::Inventory, InputMode::Menu] {
            let mut app = hand_only_app();
            app.insert_resource(TimeUpdateStrategy::ManualDuration(STEP));

            // A held button, a voxel under the crosshair, and the server reporting that it
            // is coming apart: the loop is running.
            app.world_mut()
                .resource_mut::<ButtonInput<MouseButton>>()
                .press(MouseButton::Left);
            *app.world_mut().resource_mut::<BlockTarget>() = BlockTarget(Some(BlockHit {
                block: IVec3::ZERO,
                face: IVec3::Y,
            }));
            *app.world_mut().resource_mut::<MiningFeedback>() = MiningFeedback::for_test(64);
            app.update();
            app.update();
            assert_eq!(
                app.world().resource::<HandAnimation>().mine_elapsed,
                STEP * 2,
                "{mode:?}: the loop never started, so nothing below is about stopping it"
            );

            // The screen changes hands. The server has said nothing new.
            *app.world_mut().resource_mut::<InputMode>() = mode;
            app.update();
            assert_eq!(
                app.world().resource::<HandAnimation>().mine_elapsed,
                Duration::ZERO,
                "{mode:?}: the hand kept punching while the UI owned the screen"
            );

            // The two halves that make that assertion mean anything.
            assert!(
                app.world()
                    .resource::<ButtonInput<MouseButton>>()
                    .pressed(MouseButton::Left),
                "{mode:?}: the button was released, so this test proved nothing about it"
            );
            assert_ne!(
                app.world().resource::<MiningFeedback>().progress(),
                0,
                "{mode:?}: the server's progress was cleared, so the mode gate proved nothing"
            );
        }
    }

    /// Runs frames until the arc in flight has finished, or gives up and says so.
    ///
    /// Bounded rather than a `while`: a test that hangs when the animation stops ending
    /// tells nobody anything, and the bound is comfortably past the frames one swing takes.
    fn let_the_swing_finish(app: &mut App) {
        for _ in 0..256 {
            if app.world().resource::<HandAnimation>().attack.is_none() {
                return;
            }
            app.update();
        }
        panic!("a swing was still in flight after 256 frames");
    }

    /// **The alternation is driven by the request leaving, and by nothing coming back.**
    ///
    /// There is no session here, no snapshot, no inbound frame of any kind — which is
    /// exactly the state a player is in when the server refuses a swing, because a refused
    /// blow produces no reply at all. Six presses still draw six arcs and the rotation still
    /// visits all three, because what advanced it was the asking.
    ///
    /// The two halves are asserted separately on purpose. *No two in a row* is the
    /// criterion; *all three appear* is what stops a rotation that quietly dropped one from
    /// satisfying it.
    #[test]
    fn every_swing_takes_the_next_shape_with_no_answer_from_any_server() {
        const STEP: Duration = Duration::from_millis(16);

        let mut app = hand_only_app();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(STEP));

        let mut drawn = Vec::new();
        for press in 0..(SwingShape::ALL.len() * 2) {
            app.world_mut().write_message(SwingSent);
            app.update();
            let swing = app
                .world()
                .resource::<HandAnimation>()
                .attack
                .unwrap_or_else(|| panic!("press {press} sent a swing that never played"));
            drawn.push(swing.shape);
            let_the_swing_finish(&mut app);
        }

        for pair in drawn.windows(2) {
            assert_ne!(
                pair[0], pair[1],
                "two swings running drew one arc: {drawn:?}"
            );
        }
        for shape in SwingShape::ALL {
            assert!(drawn.contains(&shape), "{shape:?} never played: {drawn:?}");
        }

        // The half that makes the paragraph above mean anything: nothing ever answered.
        assert!(
            app.world().get_resource::<Session>().is_none(),
            "a session turned up, so this test says nothing about a refused swing"
        );
    }

    /// A second press inside a running arc restarts the swing *and* takes the next shape.
    ///
    /// Two clicks are two swings, and the criterion is about consecutive attacks rather
    /// than about consecutive completed animations — a restart that redrew the same arc
    /// would be the repetition this issue exists to remove, arriving through the one door
    /// the rotation could have been left open at.
    #[test]
    fn a_swing_cut_short_by_the_next_press_still_changes_shape() {
        const STEP: Duration = Duration::from_millis(16);

        let mut app = hand_only_app();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(STEP));

        app.world_mut().write_message(SwingSent);
        app.update();
        let first = app
            .world()
            .resource::<HandAnimation>()
            .attack
            .expect("the first press played nothing");

        // Part way in, and deliberately not to the end.
        app.update();
        app.world_mut().write_message(SwingSent);
        app.update();
        let second = app
            .world()
            .resource::<HandAnimation>()
            .attack
            .expect("the second press played nothing");

        assert_ne!(
            first.shape, second.shape,
            "the interrupted swing was redrawn as the same shape"
        );
        assert_eq!(
            second.elapsed, STEP,
            "the second press continued the first arc instead of restarting it"
        );
    }
}
