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

use bevy::asset::RenderAssetUsages;
use bevy::ecs::system::SystemParam;
use bevy::light::NotShadowCaster;
use bevy::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;

use super::camera::{ViewMode, WorldCamera};
use super::combat::{ITEM_RUSTY_SWORD, SwingSent};
use super::inventory::{ApplyInventory, Inventory, SelectedSlot};
use super::items::{self, ItemShape};
use super::merge_all;
use super::target::{ApplyMiningFeedback, ApplyTargetInput, BlockTarget, MiningFeedback};
use super::{HeldItemSurface, InputMode, held_item_surface, stack_item_id};
use crate::net::{PLACEHOLDER_APPEARANCE, Session};
#[cfg(test)]
use crate::world::palette;

/// Close to the near plane and small enough to remain inside the camera's free
/// view-space pocket even when terrain touches the player capsule.
const BASE_TRANSLATION: Vec3 = Vec3::new(0.10, -0.075, -0.18);

/// The whole of the closed fist: the box the palm and the knuckles fit inside.
///
/// Unchanged from when the hand *was* this box, so nothing about where the hand sits or how
/// far it swings moves — #175 replaces what fills it, not what it occupies.
const HAND_SIZE: Vec3 = Vec3::new(0.045, 0.085, 0.045);

/// How far a carried object sinks into the top of the fist holding it.
///
/// A gap would leave the item floating and no overlap would put two faces on the same
/// plane. Six millimetres is enough to hide the join without swallowing the object's
/// silhouette; [`the_item_stays_recognisable_outside_the_fist`] holds the other side.
const HOLD_OVERLAP: f32 = 0.006;

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
/// to which colour an item presents as. The blade's vertices carry white — identity
/// — everywhere but the marks, so the base that comes through is whatever that table says.
/// Change the sword's item colour and the rust follows it, because it is a shade *of* it.
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

/// The whole sword, pommel to tip, in the same camera-space units as the block and
/// material meshes.
///
/// **The budget, and every part below is spent out of it**: it is exactly what the single
/// box occupied before #204, so nothing about where the hand sits or how far it swings
/// moves — the same constraint #175's fist met against [`HAND_SIZE`]. Grow one part and
/// another gives the length back, which is what
/// [`the_sword_spends_exactly_the_length_the_box_did`] holds.
const SWORD_LENGTH: f32 = 0.115;

/// How much of that length is blade, once the pommel, the grip and the guard have taken
/// theirs.
const BLADE_LENGTH: f32 = 0.075;

/// The blade across the flats, at the guard. It narrows from here — see
/// [`POINT_WIDTH_FRACTION`].
const BLADE_WIDTH: f32 = 0.030;

/// The blade through the ridge, which is the thickest it ever is: the section is knife-thin
/// at both edges and full thickness only along the central flat.
const BLADE_THICKNESS: f32 = 0.012;

/// How much of the blade's half-width the central flat occupies, the rest being bevel.
///
/// **This is what makes the section a hexagon rather than a rectangle**, and it is the whole
/// of why the blade reads as bevelled: six side faces per span instead of four, so the light
/// catches a different pair as the hand turns.
const BLADE_RIDGE_FRACTION: f32 = 0.34;

/// How wide the blade is where the point begins, as a fraction of its width at the guard.
///
/// Under one, so the blade is waisted rather than parallel — the taper a gladius has before
/// the point starts at all.
const POINT_WIDTH_FRACTION: f32 = 0.76;

/// How much of the blade's length is the taper to the tip.
const POINT_LENGTH: f32 = 0.020;

/// What is left of the section at the very tip, as a fraction of the section at the
/// shoulder.
///
/// **Small rather than zero, and that is a renderer's constraint rather than a shape
/// decision.** A section that collapses to one vertex turns six quads into six zero-area
/// slivers, and a zero-area triangle has no normal to compute — so the tip converges to a
/// hexagon a tenth the size instead, which is a tenth of two and a half millimetres of
/// camera space and reads as a point.
const POINT_TIP_FRACTION: f32 = 0.10;

/// The cross guard: thicker than the blade so it stands out from it in the hand, thin in
/// length, and wide enough across to read as a guard rather than a collar.
const GUARD_SIZE: Vec3 = Vec3::new(0.019, 0.006, 0.044);

/// The grip the hand closes on: leather, narrower than everything around it.
const GRIP_SIZE: Vec3 = Vec3::new(0.014, 0.024, 0.014);

/// The pommel: brass, wider than the grip, which is what stops the sword ending in a stub.
const POMMEL_SIZE: Vec3 = Vec3::new(0.018, 0.010, 0.017);

/// How far the blade's root is buried in the guard.
///
/// Half the guard, so the blade's own end cap sits *inside* the guard's volume rather than
/// flush with its top face. Flush would be two coplanar quads facing the same way, which is
/// the flicker rule 2 in `client/AGENTS.md` names for the body rig — and the reason a rust
/// mark stands proud of the blade rather than sitting on it.
const BLADE_TANG: f32 = GUARD_SIZE.y / 2.0;

/// How many rust marks the rusty blade carries.
///
/// **Several small ones rather than three large ones**, which is the difference between
/// oxide and damage: rust takes hold in freckles across a blade, and three patches at fixed
/// heights read as somebody having hit it with something.
const RUST_MARKS: u32 = 14;

/// The longest side of one mark, before [`scatter`] varies it down.
const RUST_MARK_SIZE: f32 = 0.010;

/// How much of each end of the blade stays clear of rust.
///
/// The whole mark, not its centre: a mark's own length is taken out of the range before it
/// is placed, so nothing overhangs the tip or disappears into the guard.
const RUST_MARK_MARGIN: f32 = 0.05;

/// How far a mark stands proud of the blade's surface, as a fraction of
/// [`BLADE_THICKNESS`].
///
/// The same twentieth #175 used, and for the same reason: two surfaces sharing a plane is
/// where a renderer has to choose, and it chooses per frame.
const RUST_MARK_PROUD: f32 = 0.05;

/// How deep a mark is bedded into the blade, as a fraction of the surface's own offset from
/// the mid-plane at that point.
///
/// **Both bounds are load-bearing and neither is a taste.** A mark is an axis-aligned box on
/// a surface that tilts away from it across the bevel, so the surface under one end of the
/// mark sits lower than under its middle; bedding it shallower than that drop would leave
/// the far end floating off the blade. Under one, so the mark can never reach through to the
/// other face and appear on both. The arithmetic that makes the first bound hold is in
/// [`rusted_blade_mesh`], and [`every_rust_mark_stays_on_the_blade_it_freckles`] measures it.
const RUST_MARK_SINK: f32 = 0.6;

/// The seed the marks are scattered from.
///
/// **Deterministic, so the same sword looks the same every run** — a blade whose freckles
/// moved between sessions would be the one thing about it a player could not learn.
const RUST_SEED: u32 = 0x5EED_0204;

/// A carried structure: a bundle, wider than it is tall, so a tent under the arm does not
/// read as another stackable cube.
const BUNDLE_SIZE: Vec3 = Vec3::new(0.075, 0.042, 0.048);

/// An implement's haft: longer and thicker than a blade, because what tells a shovel from
/// a sword at a glance is that one is a handle with weight on the end and the other is
/// mostly edge.
const TOOL_HAFT_SIZE: Vec3 = Vec3::new(0.014, 0.130, 0.014);

/// And its head, across the top of that haft. Wider than the haft in x and z and short in
/// y, which is the T a shovel, a pickaxe and an axe all share — and the whole of what
/// distinguishes the silhouette from [`sword_mesh`]'s guard, grip and tapering blade.
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

/// One cross-section of the blade: where it sits along the sword, how far it reaches to
/// either edge, and how thick it is through the central ridge.
///
/// **The blade is lofted from three of these**, which is what "bevelled" means in a form the
/// renderer can hold: knife-thin at both edges and full thickness only along a central flat.
#[derive(Debug, Clone, Copy)]
struct BladeSection {
    y: f32,
    half_width: f32,
    half_thickness: f32,
}

impl BladeSection {
    /// The six corners of the section, in order around its perimeter.
    ///
    /// **The order is load-bearing rather than a convention.** [`MeshBuild::quad`] takes the
    /// outward normal from the corners it is handed, so walking a section the other way
    /// round turns the whole blade inside out — visible only as a sword that vanishes when
    /// you look at it, which is the failure that costs the most to diagnose.
    fn perimeter(self) -> [Vec3; 6] {
        let Self {
            y,
            half_width: w,
            half_thickness: t,
        } = self;
        let ridge = w * BLADE_RIDGE_FRACTION;
        [
            Vec3::new(0.0, y, w),
            Vec3::new(t, y, ridge),
            Vec3::new(t, y, -ridge),
            Vec3::new(0.0, y, -w),
            Vec3::new(-t, y, -ridge),
            Vec3::new(-t, y, ridge),
        ]
    }
}

/// The buffers one hand-authored mesh is accumulated into.
///
/// **Hand-authored positions rather than merged primitives, and only for the blade.** The
/// guard, the grip and the pommel are boxes and stay boxes; a bevelled section that tapers
/// to a point is not something `Cuboid`, `Cone` or `ConicalFrustum` can express — a cone is
/// round and a frustum is round, and what this needs is a hexagon that narrows in width
/// faster than in thickness. `world/render.rs` builds the entire terrain this way, so the
/// mechanism is the established one rather than a new one.
#[derive(Debug, Default)]
struct MeshBuild {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
}

impl MeshBuild {
    /// One flat-shaded quad, wound around its perimeter.
    ///
    /// Flat rather than smooth, deliberately: six faces per span that each catch the light
    /// separately is the whole reason the section is a hexagon, and averaging the normals at
    /// the ridge would put a soft gradient exactly where the highlight should break.
    fn quad(&mut self, corners: [Vec3; 4]) {
        let [a, b, c, d] = corners;
        // From the diagonals rather than from one triangle's two edges: a quad lofted
        // between sections of different widths is not exactly planar, and the diagonals
        // give the normal both of its triangles are nearest to instead of the first one's.
        let normal = (c - a).cross(d - b).normalize_or_zero();
        let first = self.push(corners.into_iter().zip(UNIT_UVS), normal);
        self.indices
            .extend([first, first + 1, first + 3, first + 1, first + 2, first + 3]);
    }

    /// One flat-shaded polygon, as a fan from its first corner.
    ///
    /// The corners must already be wound so that `normal` is the outward one; the caller
    /// reverses them for the end that faces the other way.
    fn fan(&mut self, corners: [Vec3; 6], normal: Vec3) {
        // The cap is never seen — the root is buried in the guard and the tip is a tenth of
        // a section — so its texture coordinates carry no information and say so.
        let first = self.push(corners.into_iter().zip([[0.0, 0.0]; 6]), normal);
        for corner in 1..corners.len() as u32 - 1 {
            self.indices
                .extend([first, first + corner, first + corner + 1]);
        }
    }

    /// Appends vertices sharing one normal, and answers the index the first of them landed
    /// at.
    fn push(&mut self, corners: impl Iterator<Item = (Vec3, [f32; 2])>, normal: Vec3) -> u32 {
        let first = self.positions.len() as u32;
        for (corner, uv) in corners {
            self.positions.push(corner.to_array());
            self.normals.push(normal.to_array());
            self.uvs.push(uv);
        }
        first
    }

    /// The three attributes and the indices, as the asset the renderer draws.
    ///
    /// **All three attributes, and that is not decoration.** `Mesh::merge` walks the
    /// attributes of the mesh being merged *into* and silently skips any the other side
    /// lacks, which leaves the buffers different lengths rather than raising — so a blade
    /// missing `ATTRIBUTE_UV_0` would merge with a `Cuboid` guard and corrupt it quietly.
    fn finish(self) -> Mesh {
        Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs)
        .with_inserted_indices(Indices::U32(self.indices))
    }
}

/// One texture coordinate per corner of a quad, in the order [`MeshBuild::quad`] walks them.
///
/// Nothing samples them — this client has no texture and `client/AGENTS.md` says the palette
/// is the whole material system — but the attribute has to be *present*, because a merge
/// drops any attribute one side is missing and leaves the buffers unequal lengths.
const UNIT_UVS: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];

/// Where the blade starts: the top of the guard, in the sword's own space.
///
/// The sword is centred on its own origin, exactly as every `Cuboid` in this file is, so
/// that swapping the held mesh moves nothing about where the hand sits.
fn blade_base() -> f32 {
    -SWORD_LENGTH / 2.0 + POMMEL_SIZE.y + GRIP_SIZE.y + GUARD_SIZE.y
}

/// The three sections the blade is lofted from: at the guard, at the shoulder where the
/// point begins, and at the tip.
fn blade_sections() -> [BladeSection; 3] {
    let base = blade_base();
    let half_width = BLADE_WIDTH / 2.0;
    let half_thickness = BLADE_THICKNESS / 2.0;
    [
        // Sunk into the guard by [`BLADE_TANG`], so the blade's own end cap is inside the
        // guard's volume rather than flush with its top face.
        BladeSection {
            y: base - BLADE_TANG,
            half_width,
            half_thickness,
        },
        // The shoulder. The blade has narrowed to [`POINT_WIDTH_FRACTION`] by here and is
        // still full thickness: a gladius is waisted long before it is pointed.
        BladeSection {
            y: base + BLADE_LENGTH - POINT_LENGTH,
            half_width: half_width * POINT_WIDTH_FRACTION,
            half_thickness,
        },
        // The tip, where both give way together.
        BladeSection {
            y: base + BLADE_LENGTH,
            half_width: half_width * POINT_WIDTH_FRACTION * POINT_TIP_FRACTION,
            half_thickness: half_thickness * POINT_TIP_FRACTION,
        },
    ]
}

/// The section the blade has at a given height, interpolated along the loft.
///
/// Read by [`rusted_blade_mesh`] so a mark sits on the surface the blade actually has there
/// rather than on the one it has at the guard.
fn blade_at(y: f32) -> BladeSection {
    let [root, shoulder, tip] = blade_sections();
    let (lower, upper) = if y <= shoulder.y {
        (root, shoulder)
    } else {
        (shoulder, tip)
    };
    let along = ((y - lower.y) / (upper.y - lower.y)).clamp(0.0, 1.0);
    let between = |from: f32, to: f32| from + (to - from) * along;
    BladeSection {
        y,
        half_width: between(lower.half_width, upper.half_width),
        half_thickness: between(lower.half_thickness, upper.half_thickness),
    }
}

/// How far the blade's surface stands off its mid-plane, `z` across a given section.
///
/// Flat at [`BLADE_THICKNESS`] over the ridge, then falling away linearly to nothing at the
/// edge — the bevel, read as a number.
fn blade_surface(section: BladeSection, z: f32) -> f32 {
    let ridge = section.half_width * BLADE_RIDGE_FRACTION;
    let across = z.abs();
    if across <= ridge {
        section.half_thickness
    } else {
        section.half_thickness * (section.half_width - across) / (section.half_width - ridge)
    }
}

/// A gladius: a bevelled blade that tapers to a point, a cross guard, a grip and a pommel,
/// merged into one mesh at whatever length the caller draws it.
///
/// **One mesh, for the reason [`tool_mesh`] and [`fist_mesh`] are one each**: the view model
/// is a single entity with a single transform that `animate_view_model` drives, and a guard
/// parented separately would be a second thing to keep in step with a swing.
///
/// **The length is a parameter because two renderers draw this weapon and they must draw the
/// same one.** `player/drops.rs` calls it too, at drop scale. That is deliberately *not* the
/// shared-mesh arrangement its `drop_mesh` note rules out — each surface still mints its own
/// asset, at its own size, with its own materials — it is the shape being one answer instead
/// of two that somebody has to keep in step, which is exactly the relationship
/// `player/items.rs` already has with its readers.
pub(super) fn sword_mesh(length: f32) -> Mesh {
    let base = blade_base();
    let sections = blade_sections();

    let mut build = MeshBuild::default();
    for pair in sections.windows(2) {
        let [lower, upper] = pair else {
            unreachable!("windows(2) yields pairs")
        };
        let low = lower.perimeter();
        let high = upper.perimeter();
        for corner in 0..low.len() {
            let next = (corner + 1) % low.len();
            build.quad([low[corner], low[next], high[next], high[corner]]);
        }
    }
    // The two ends. The root's winding is reversed because its face looks the other way,
    // and a cap wound like the tip's would be culled from outside and visible from within.
    let mut root = sections[0].perimeter();
    root.reverse();
    build.fan(root, Vec3::NEG_Y);
    build.fan(sections[2].perimeter(), Vec3::Y);
    let mut sword = build.finish();

    // The furniture, in boxes, down from the base. Each sits directly under the last: two
    // solid boxes meeting on a plane present that plane's two quads back to back, and a
    // back-facing quad is culled — which is why *these* joins need no overlap and the
    // blade's root, whose cap would face the same way as the guard's, does.
    let guard = Mesh::from(Cuboid::from_size(GUARD_SIZE))
        .translated_by(Vec3::Y * (base - GUARD_SIZE.y / 2.0));
    let grip = Mesh::from(Cuboid::from_size(GRIP_SIZE))
        .translated_by(Vec3::Y * (base - GUARD_SIZE.y - GRIP_SIZE.y / 2.0));
    let pommel = Mesh::from(Cuboid::from_size(POMMEL_SIZE))
        .translated_by(Vec3::Y * (base - GUARD_SIZE.y - GRIP_SIZE.y - POMMEL_SIZE.y / 2.0));
    merge_all(&mut sword, [guard, grip, pommel], "sword");

    // Uniform, so the normals computed above stay unit vectors — `Mesh::scale_by` leaves
    // them alone for exactly that case and rebuilds them for every other.
    sword.scaled_by(Vec3::splat(length / SWORD_LENGTH))
}

/// A deterministic value in `0.0..1.0` for one rust mark and one of its dimensions.
///
/// **A seeded hash rather than a crate and rather than a table of hand-placed numbers.**
/// Fourteen scattered boxes are not worth a fourth dependency (`client/AGENTS.md` is
/// explicit about the budget), and an integer hash is reproducible on every platform, which
/// is what [`RUST_SEED`]'s promise of the same sword every run actually requires.
fn scatter(mark: u32, channel: u32) -> f32 {
    let mut bits = mark
        .wrapping_mul(0x9E37_79B1)
        .wrapping_add(channel.wrapping_mul(0x85EB_CA6B))
        ^ RUST_SEED;
    bits ^= bits >> 16;
    bits = bits.wrapping_mul(0x7FEB_352D);
    bits ^= bits >> 15;
    bits = bits.wrapping_mul(0x846C_A68B);
    bits ^= bits >> 16;
    // The top 24 bits over their own range: every value of that width is exactly
    // representable in an f32, so the division is the only rounding anywhere in here.
    (bits >> 8) as f32 / 16_777_216.0
}

/// The rusty sword: [`sword_mesh`] with oxide on the blade.
///
/// **Two colours on one mesh and one material**, which is what the cost note in
/// `client/AGENTS.md` asks for — the alternative was a second entity per held item, or a
/// material per item rather than one cached handle per resolved colour.
///
/// The vertices carry `Mesh::ATTRIBUTE_COLOR`, which `StandardMaterial` multiplies into its
/// `base_color`; `world/render.rs` has drawn the whole terrain that way since it existed, so
/// this is the established mechanism rather than a new one. White is identity — the iron
/// that comes through is whatever `player/items.rs` says the sword presents as — and the
/// marks carry [`RUST_TINT`], so they are a shade *of* that base rather than a second
/// opinion about it.
///
/// **Fourteen small marks scattered from a seed, where there used to be three large ones at
/// hand-picked heights.** Three patches a seventh of the blade tall read as damage; oxide is
/// freckles. Each is bedded into the surface it sits on rather than laid over it, which is
/// what lets a mark straddle the ridge and the bevel without either floating clear of the
/// blade or reaching through to the far face.
fn rusted_blade_mesh() -> Mesh {
    let mut merged = plain(sword_mesh(SWORD_LENGTH));
    let base = blade_base();
    let proud = BLADE_THICKNESS * RUST_MARK_PROUD;

    let marks = (0..RUST_MARKS).map(|mark| {
        // The longest side, half to all of `RUST_MARK_SIZE`. The whole mark is kept out of
        // the margin at each end rather than merely its centre, so nothing overhangs the
        // tip or disappears into the guard however large it came out.
        let length = RUST_MARK_SIZE * (0.5 + 0.5 * scatter(mark, 0));
        let lowest = base + BLADE_LENGTH * RUST_MARK_MARGIN + length / 2.0;
        let highest = base + BLADE_LENGTH * (1.0 - RUST_MARK_MARGIN) - length / 2.0;
        // **One mark per stratum of the blade, jittered inside its own** — rather than
        // fourteen independent draws over the whole length. Fourteen samples of a hash
        // clump: the first cut of this left the top third and the bottom tenth bare and put
        // nine marks in the middle, which reads as a band rather than as weathering.
        // Stratifying makes *spread over the blade* a property of the placement instead of a
        // hope about the seed, and the jitter is what keeps it from being a row.
        let stratum = (mark as f32 + scatter(mark, 1)) / RUST_MARKS as f32;
        let y = lowest + (highest - lowest) * stratum;

        // **Two bounds, and they are what keep a mark from overhanging the edge it sits
        // beside.** The mark spans at most a quarter of the local half-width to each side of
        // its centre, and its centre stays inside half of it — so the blade's surface can
        // fall away *across* the bevel under the mark by at most `0.38 × half_thickness`.
        //
        // They are not what makes the bedding below sufficient, which is what this comment
        // used to claim: the fall-off across the bevel is only one of the two directions the
        // surface drops in, and the bedding answers both. See `footing`.
        let section = blade_at(y);
        let width = (length * 0.5).min(section.half_width * 0.5);
        let room = (section.half_width * 0.5 - width / 2.0).max(0.0);
        let z = room * (scatter(mark, 2) * 2.0 - 1.0);

        // Alternating faces, so a blade turning in the hand shows freckles on whichever one
        // it presents rather than a stripe down one side of it.
        let face = if mark % 2 == 0 { 1.0 } else { -1.0 };
        let surface = blade_surface(section, z);
        // **Bedded from the shallowest surface under the whole mark, rather than from the one
        // under its centre.** The blade thins along its length as well as across the bevel,
        // and on the point it does so fast enough to outrun `RUST_MARK_SINK`: measured on the
        // fourteenth mark, bedded to 0.00122 from the section at its own centre while the
        // surface under its upper, outer corner is 0.00088 — so that corner floated 0.00034
        // clear of the blade it is meant to be sunk into, and a fleck of rust hung off the
        // point with daylight behind it.
        //
        // **Which corner answers is never in doubt**, which is what makes one sample enough:
        // the surface falls as `y` rises and as `|z|` grows, so the highest and farthest
        // corner is the shallowest of the four. On the flat this changes nothing — the
        // section at the mark's top and the section at its centre are the same numbers there,
        // and `RUST_MARK_SINK` still decides — so the deeper bedding is spent only where the
        // taper actually takes the surface away.
        let footing = blade_surface(blade_at(y + length / 2.0), z.abs() + width / 2.0)
            .min(surface * (1.0 - RUST_MARK_SINK));
        let sink = surface - footing;
        rusted(
            Mesh::from(Cuboid::from_size(Vec3::new(sink + proud, length, width)))
                .translated_by(Vec3::new(face * (surface + (proud - sink) / 2.0), y, z)),
        )
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

/// One wire colour as the linear vertex value Bevy's PBR shader consumes.
///
/// Character colours are `0x00RRGGBB` in sRGB, while item colours have already been
/// resolved to linear values by `player/items.rs`. Keeping the conversion at this boundary
/// gives both sources exactly one interpretation.
fn linear_rgb(colour: u32) -> [f32; 4] {
    let linear = Color::srgb_u8(
        ((colour >> 16) & 0xFF) as u8,
        ((colour >> 8) & 0xFF) as u8,
        (colour & 0xFF) as u8,
    )
    .to_linear();
    [linear.red, linear.green, linear.blue, linear.alpha]
}

/// Applies an item's resolved colour to a mesh, preserving any relative vertex tint it
/// already carries.
///
/// Most item meshes have no colour attribute and receive the resolved colour whole. The
/// rusty blade carries white and [`RUST_TINT`]; multiplying those by the item colour keeps
/// `player/items.rs` the one answer to what the steel is while retaining the oxide as a
/// shade of it.
fn coloured(mut mesh: Mesh, base: [f32; 4]) -> Mesh {
    let colours = match mesh.attribute(Mesh::ATTRIBUTE_COLOR) {
        Some(VertexAttributeValues::Float32x4(tints)) => tints
            .iter()
            .map(|tint| std::array::from_fn(|channel| tint[channel] * base[channel]))
            .collect(),
        // Every mesh in this module either has no colour or a Float32x4 one. Replacing an
        // unexpected representation is the cosmetic, non-fatal direction to fail in.
        _ => vec![base; mesh.count_vertices()],
    };
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colours);
    mesh
}

/// The geometry one held item contributes before it is arranged against the fist.
///
/// Exhaustive over [`ItemShape`], so a new shape does not compile until the hand can hold
/// it. The rusty sword remains the one item-level exception: rust belongs to that blade,
/// not to every item sharing its shape.
fn item_mesh(item_id: u16, shape: ItemShape) -> Mesh {
    if item_id == ITEM_RUSTY_SWORD {
        return rusted_blade_mesh();
    }
    match shape {
        ItemShape::Block => Mesh::from(Cuboid::from_size(Vec3::splat(BLOCK_EDGE))),
        ItemShape::Material => Mesh::from(Capsule3d::new(MATERIAL_RADIUS, MATERIAL_LENGTH)),
        ItemShape::Blade => sword_mesh(SWORD_LENGTH),
        ItemShape::Bundle => Mesh::from(Cuboid::from_size(BUNDLE_SIZE)),
        ItemShape::Tool => tool_mesh(),
    }
}

/// Where an item sits relative to the fist at the origin.
///
/// Blocks, materials and bundles rest on the knuckles. A sword is lifted until the centre
/// of its grip crosses the palm, and a tool until the palm closes around the lower haft.
/// These are translations of the approved geometry, not new shapes.
fn item_translation(shape: ItemShape) -> Vec3 {
    let hand_top = HAND_SIZE.y / 2.0;
    let y = match shape {
        ItemShape::Block => hand_top + BLOCK_EDGE / 2.0 - HOLD_OVERLAP,
        ItemShape::Material => hand_top + MATERIAL_LENGTH / 2.0 + MATERIAL_RADIUS - HOLD_OVERLAP,
        ItemShape::Blade => {
            let grip_centre = blade_base() - GUARD_SIZE.y - GRIP_SIZE.y / 2.0;
            -grip_centre
        }
        ItemShape::Bundle => hand_top + BUNDLE_SIZE.y / 2.0 - HOLD_OVERLAP,
        // The head stays above the hand and most of the haft remains visible below it.
        ItemShape::Tool => HAND_SIZE.y * 0.35,
    };
    Vec3::Y * y
}

/// The complete first-person arrangement: the player's fist and, when selected, the item
/// it holds, merged into one coloured mesh.
///
/// The fist is always first in the buffers. Besides making the mesh deterministic, that
/// gives the tests a structural way to assert that every shape still contains the exact
/// hand #175 approved instead of merely containing skin-coloured vertices somewhere.
fn held_mesh(skin_colour: u32, appearance: HeldAppearance) -> Mesh {
    let mut held = tinted(fist_mesh(), linear_rgb(skin_colour));
    let (Some(item_id), Some(shape), Some(item_colour)) =
        (appearance.item_id, appearance.shape, appearance.item_colour)
    else {
        return held;
    };

    let item =
        coloured(item_mesh(item_id, shape), item_colour).translated_by(item_translation(shape));
    merge_all(&mut held, [item], "hand and held item");
    held
}

pub(super) struct HandsPlugin;

impl Plugin for HandsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<HandAnimation>()
            // `PlayerPlugin` owns the appearance cache in the game. Initialised here too
            // because the focused animation tests build this plugin on its own.
            .init_resource::<super::Appearances>()
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
                    // After this frame's appearance message has been cached, so the fist
                    // takes the local player's skin colour on the same frame as their body.
                    .after(super::ApplySnapshots)
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
pub(super) struct HeldItem {
    item_id: Option<u16>,
    shape: Option<ItemShape>,
    /// The player's own skin colour, so a late appearance message rebuilds the hand even
    /// when the selected slot did not move.
    skin_colour: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct HeldAppearance {
    item_id: Option<u16>,
    shape: Option<ItemShape>,
    item_colour: Option<[f32; 4]>,
}

#[derive(Resource, Debug)]
struct HandVisuals {
    /// The one mesh asset the entity draws. Its contents change only when the selected
    /// item or the local player's skin colour changes; the handle and entity stay put.
    mesh: Handle<Mesh>,
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
    let appearance = selected_appearance(None);
    let skin_colour = PLACEHOLDER_APPEARANCE.skin_color();
    let mesh = meshes.add(held_mesh(skin_colour, appearance));
    let material = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        unlit: true,
        fog_enabled: false,
        // Positive renders closer. Together with the near-plane placement this prevents
        // terrain depth from slicing through the held arrangement.
        depth_bias: 1_000.0,
        ..default()
    });
    let visuals = HandVisuals { mesh: mesh.clone() };

    commands.spawn((
        HeldItem {
            item_id: appearance.item_id,
            shape: appearance.shape,
            skin_colour,
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

/// The stable view-model handle and the asset whose contents it names, as one borrow.
///
/// Rebuilding the one asset in place avoids both a mesh cache keyed by arbitrary server
/// colours and a second entity. The render-world handle therefore stays stable through a
/// slot change while the hand and item remain one draw.
#[derive(SystemParam)]
struct HandAssets<'w> {
    visuals: Res<'w, HandVisuals>,
    meshes: ResMut<'w, Assets<Mesh>>,
}

/// The two facts that choose what the view model draws: the selected authoritative stack
/// and the local player's authoritative appearance.
///
/// They arrive on different streams and change independently, so keeping the lookup in one
/// parameter is what prevents a slot refresh from forgetting skin or an appearance refresh
/// from forgetting the item.
#[derive(SystemParam)]
struct HandSubject<'w> {
    inventory: Res<'w, Inventory>,
    selected: Res<'w, SelectedSlot>,
    session: Option<Res<'w, Session>>,
    appearances: Res<'w, super::Appearances>,
}

impl HandSubject<'_> {
    fn read(&self) -> (HeldAppearance, u32) {
        let appearance = selected_appearance(self.inventory.slot(self.selected.0));
        let skin_colour = self
            .session
            .as_deref()
            .and_then(|session| self.appearances.0.get(&session.0.entity_id))
            .map_or(PLACEHOLDER_APPEARANCE.skin_color(), |described| {
                described.appearance.skin_color()
            });
        (appearance, skin_colour)
    }
}

fn refresh_held_item(
    subject: HandSubject<'_>,
    mode: Res<InputMode>,
    view: Res<ViewMode>,
    mut assets: HandAssets<'_>,
    mut held: Query<(&mut HeldItem, &Mesh3d, &mut Visibility)>,
) {
    let (appearance, skin_colour) = subject.read();
    let view_mesh = assets.visuals.mesh.clone();
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
    let visible = if held_item_surface(*mode, *view, subject.session.is_some())
        == HeldItemSurface::ViewModel
    {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };

    for (mut item, mesh, mut visibility) in &mut held {
        if item.item_id != appearance.item_id
            || item.shape != appearance.shape
            || item.skin_colour != skin_colour
        {
            item.item_id = appearance.item_id;
            item.shape = appearance.shape;
            item.skin_colour = skin_colour;
            if mesh.0 != view_mesh {
                error!("the held entity no longer names the view-model mesh");
            } else if let Some(mut mesh) = assets.meshes.get_mut(&view_mesh) {
                *mesh = held_mesh(skin_colour, appearance);
            } else {
                error!("the held view-model mesh asset is missing");
            }
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
fn selected_appearance(stack: Option<crate::net::InventoryStack>) -> HeldAppearance {
    let Some(item_id) = stack_item_id(stack) else {
        return HeldAppearance {
            item_id: None,
            shape: None,
            item_colour: None,
        };
    };

    HeldAppearance {
        item_id: Some(item_id),
        shape: Some(items::item_shape(item_id)),
        item_colour: Some(items::item_linear_rgba(item_id)),
    }
}

/// The shape the first-person composition builds for one non-empty stack.
///
/// Test-only: `combat.rs` uses it to pin the presentation route to the blade-routing table.
/// The running client goes through [`selected_appearance`] with the real selected stack.
#[cfg(test)]
pub(super) fn drawn_item_shape(item_id: u16) -> ItemShape {
    selected_appearance(Some(crate::net::InventoryStack {
        item_id,
        count: 1,
        ..Default::default()
    }))
    .shape
    .expect("a non-empty stack has an item shape")
}

/// What the hand is reacting to this frame: one authoritative fact and two local presses.
///
/// A bundle rather than five parameters, for the reason [`HandAssets`] is one —
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
    use crate::net::{
        Appearance as PlayerLook, AppearanceInbox, InventoryStack, PlayerAppearance, SessionParams,
    };
    use crate::player::items::{ITEM_LOG, ITEM_RAW_COAL, ITEM_RAW_IRON, ITEM_STONE};
    use crate::player::{PlayerPlugin, combat, crafting, structures};

    /// Deliberately unlike every item swatch, so skin vertices can be identified in a
    /// composite without mistaking part of the item for the hand.
    const TEST_SKIN: u32 = 0x00E3_C4A0;

    fn shape_examples() -> [(ItemShape, u16); ItemShape::ALL.len()] {
        [
            (ItemShape::Block, ITEM_STONE),
            (ItemShape::Material, ITEM_RAW_COAL),
            (ItemShape::Blade, ITEM_IRON_SWORD),
            (ItemShape::Bundle, structures::ITEM_TENT),
            (ItemShape::Tool, crafting::ITEM_SHOVEL),
        ]
    }

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
    fn tints(mesh: &Mesh) -> Vec<[u8; 4]> {
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
    /// that table the one answer — change the sword's colour and the rust follows it.
    #[test]
    fn the_rusty_sword_carries_iron_and_rust_on_one_mesh() {
        let rusted = rusted_blade_mesh();
        let plain = sword_mesh(SWORD_LENGTH);

        let marks = tints(&rusted);
        assert_eq!(
            marks.len(),
            2,
            "the rusty blade carries {} tints, want iron and rust: {marks:?}",
            marks.len()
        );
        assert!(
            marks.contains(&[255, 255, 255, 255]),
            "no vertex carries identity, so the item's own colour never shows through"
        );
        let rust = RUST_TINT.map(|channel| (channel * 255.0).round() as u8);
        assert!(marks.contains(&rust), "no vertex carries the rust tint");

        // And the iron sword is not rusty: it is the same `ItemShape::Blade` and must not
        // inherit one blade's condition. It carries no vertex colours at all — an absent
        // attribute is how a mesh takes its material's colour whole, which is what every
        // other held shape does and what the rusted blade opts out of.
        assert_eq!(
            tints(&plain),
            Vec::<[u8; 4]>::new(),
            "the plain blade carries vertex colours, so it is no longer simply its material"
        );
        assert!(
            rusted.count_vertices() > plain.count_vertices(),
            "the rusty sword has no mark geometry of its own"
        );
    }

    /// Every vertex position one mesh carries.
    fn positions(mesh: &Mesh) -> Vec<[f32; 3]> {
        let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("the mesh must carry Float32x3 positions");
        };
        positions.clone()
    }

    /// The lowest and highest value a set of vertices reaches on one axis.
    fn extent(positions: &[[f32; 3]], axis: usize) -> (f32, f32) {
        positions
            .iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(low, high), p| {
                (low.min(p[axis]), high.max(p[axis]))
            })
    }

    /// **The sword spends exactly the length the box did**, and is still centred on its own
    /// origin.
    ///
    /// This is the half most likely to break the swing tests without anybody noticing, so it
    /// is asserted twice over: once against the parts, so growing one has to take the length
    /// from another, and once against the mesh, so an arithmetic slip in the stacking cannot
    /// pass by agreeing with itself.
    #[test]
    fn the_sword_spends_exactly_the_length_the_box_did() {
        let parts = POMMEL_SIZE.y + GRIP_SIZE.y + GUARD_SIZE.y + BLADE_LENGTH;
        assert!(
            (parts - SWORD_LENGTH).abs() < 1e-6,
            "the pommel, grip, guard and blade come to {parts} against a budget of \
             {SWORD_LENGTH}"
        );

        let sword = positions(&sword_mesh(SWORD_LENGTH));

        let (low, high) = extent(&sword, 1);
        assert!(
            (high - low - SWORD_LENGTH).abs() < 1e-5,
            "the sword spans {} on y, and SWORD_LENGTH says {SWORD_LENGTH}",
            high - low
        );
        assert!(
            (high + low).abs() < 1e-5,
            "the sword is not centred on its own origin: it spans {low}..{high}, so swapping \
             the held mesh would move where the hand sits"
        );
    }

    /// **A gladius rather than a bar**: a blade that narrows and thins to a point, bevelled
    /// from a central ridge, with a cross guard, a grip and a pommel under it.
    ///
    /// Every clause is a property rather than a vertex list. *Tapers* is the cross-section at
    /// the tip being smaller than at the guard on both axes, which is what "has a point"
    /// means in a form a test can read and which a box fails by construction. *Bevelled* is
    /// the section reaching its full thickness somewhere other than at its widest point,
    /// which a rectangular section fails in both directions.
    #[test]
    fn the_held_sword_is_a_gladius_and_not_one_box() {
        let sword = positions(&sword_mesh(SWORD_LENGTH));

        let one_box = Mesh::from(Cuboid::from_size(Vec3::ONE)).count_vertices();
        assert!(
            sword.len() > one_box,
            "the sword is {} vertices, which is one box",
            sword.len()
        );

        // The vertices sitting on one horizontal plane, which is how a section is read out of
        // a merged mesh: the loft puts blade vertices at exactly three heights and the
        // furniture's boxes at four more, and no two of the seven coincide.
        let on = |y: f32| -> Vec<[f32; 3]> {
            let found: Vec<[f32; 3]> = sword
                .iter()
                .copied()
                .filter(|p| (p[1] - y).abs() < 1e-6)
                .collect();
            assert!(!found.is_empty(), "no vertex sits at y {y}");
            found
        };
        let across = |section: &[[f32; 3]]| section.iter().map(|p| p[2].abs()).fold(0.0, f32::max);
        let through = |section: &[[f32; 3]]| section.iter().map(|p| p[0].abs()).fold(0.0, f32::max);

        let [root, shoulder, tip] = blade_sections().map(|section| on(section.y));

        // It tapers, and twice over: waisted from the guard to the shoulder, then converging
        // in both axes at once over the point.
        assert!(
            across(&tip) < across(&shoulder) && across(&shoulder) < across(&root),
            "the blade does not narrow: {} at the guard, {} at the shoulder, {} at the tip",
            across(&root),
            across(&shoulder),
            across(&tip)
        );
        assert!(
            through(&tip) < through(&root),
            "the blade is {} thick at the tip against {} at the guard, so it ends in a chisel",
            through(&tip),
            through(&root)
        );

        // It is bevelled: thickest along a central ridge and knife-thin at both edges, so the
        // vertex reaching furthest *across* is not the one reaching furthest *through*.
        let widest = root.iter().copied().fold([0.0f32; 3], |best, p| {
            if p[2].abs() > best[2].abs() { p } else { best }
        });
        assert!(
            widest[0].abs() < through(&root) * 0.5,
            "the blade is {} thick at its widest point against {} at the ridge: the section is \
             a rectangle rather than a bevel",
            widest[0].abs(),
            through(&root)
        );
        let ridge: Vec<[f32; 3]> = root
            .iter()
            .copied()
            .filter(|p| (p[0].abs() - through(&root)).abs() < 1e-6)
            .collect();
        assert!(
            across(&ridge) < across(&root),
            "the ridge is as wide as the blade, so there is no bevel to run from"
        );

        // A cross guard wider than the blade, a grip narrower than it, and a pommel wider
        // than the grip. Each part meets its neighbour on a shared plane, so a joint carries
        // two widths — the part above it and the part below — and reading both is what tells
        // three stacked parts from one box of the right height.
        let base = blade_base();
        let widths = |y: f32| -> (f32, f32) {
            let plane = on(y);
            (
                plane
                    .iter()
                    .map(|p| p[2].abs())
                    .fold(f32::INFINITY, f32::min),
                across(&plane),
            )
        };
        let (_, guard) = widths(base);
        assert!(
            guard > across(&root),
            "the part on top of the grip reaches {guard} across against the blade's {}, so it \
             is not a cross guard",
            across(&root)
        );
        let (grip, _) = widths(base - GUARD_SIZE.y);
        assert!(
            grip < across(&root),
            "the part under the guard is {grip} across against a blade of {}, so there is no \
             grip for a hand to close on",
            across(&root)
        );
        let (heel, pommel) = widths(base - GUARD_SIZE.y - GRIP_SIZE.y);
        assert!(
            pommel > heel,
            "the grip runs into the bottom of the sword at {heel} with nothing wider under it, \
             so there is no pommel"
        );
    }

    /// **One mesh and one material for every hand-and-item arrangement.**
    ///
    /// The cost rule the body rig set and #175 kept, and the one a sword assembled from a
    /// extra entities would break quietly: they could look right and animate wrong, because
    /// `animate_view_model` drives one transform and a guard parented separately would be a
    /// second thing to keep in step with a swing.
    #[test]
    fn every_held_shape_is_one_mesh_one_material_and_one_transform() {
        let mut app = app();
        let view_mesh = app.world().resource::<HandVisuals>().mesh.clone();

        for (shape, item_id) in shape_examples() {
            *app.world_mut().resource_mut::<Inventory>() =
                Inventory::from_stacks(vec![InventoryStack {
                    item_id,
                    count: 1,
                    ..Default::default()
                }]);
            *app.world_mut().resource_mut::<SelectedSlot>() = SelectedSlot(0);
            app.update();

            let world = app.world_mut();
            let mut view = world.query_filtered::<
                (Entity, &HeldItem, &Mesh3d),
                With<MeshMaterial3d<StandardMaterial>>,
            >();
            let drawn: Vec<(Entity, HeldItem, Handle<Mesh>)> = view
                .iter(world)
                .map(|(entity, held, mesh)| (entity, *held, mesh.0.clone()))
                .collect();
            assert_eq!(
                drawn.len(),
                1,
                "{shape:?} is {} entities carrying a mesh and a material",
                drawn.len()
            );
            assert_eq!(drawn[0].1.shape, Some(shape));
            assert_eq!(
                drawn[0].2, view_mesh,
                "{shape:?} replaced the stable view-model mesh handle"
            );

            let mut children = world.query::<(&ChildOf, Entity)>();
            let under = children
                .iter(world)
                .filter(|(parent, _)| parent.parent() == drawn[0].0)
                .count();
            assert_eq!(
                under, 0,
                "{shape:?} has {under} child entities, so part of it is not on the transform \
                 `animate_view_model` drives"
            );
        }
    }

    /// **The rust is many small marks bedded into the blade.**
    ///
    /// It was three patches, each an eighth of the sword tall and more than half the blade
    /// wide, at hand-picked heights — which reads as damage rather than as oxide. What this
    /// pins is the shape of the replacement: [`RUST_MARKS`] of them, none longer than
    /// [`RUST_MARK_SIZE`], spread over the blade rather than banded across a third of it, and
    /// each *bedded into* the face it sits on rather than laid over it.
    ///
    /// That last clause is the one that needs measuring, because it is the only part not
    /// obvious from reading the constants. A mark is an axis-aligned box on a surface that
    /// tilts away across the bevel, so there are two ways to get it wrong and they fail in
    /// opposite directions: bedded too shallow and the far end lifts off the blade, bedded
    /// too deep and it comes through on the other face. Both are checked against the surface
    /// the blade actually has under each mark.
    #[test]
    fn every_rust_mark_is_bedded_into_the_blade_it_freckles() {
        // What three marks used to be, so "smaller" is measured against something rather than
        // asserted about nothing: 13% of the sword's length by 55% of the blade's width.
        const WAS_LONG: f32 = 0.115 * 0.13;
        const WAS_WIDE: f32 = 0.030 * 0.55;
        const {
            assert!(
                RUST_MARKS > 3 && RUST_MARK_SIZE < WAS_LONG && RUST_MARK_SIZE < WAS_WIDE,
                "the rust is not more numerous and smaller than the three patches it replaced"
            );
        }

        let mesh = rusted_blade_mesh();

        let Some(VertexAttributeValues::Float32x3(all)) = mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("the rusted blade must carry Float32x3 positions");
        };
        let Some(VertexAttributeValues::Float32x4(colours)) = mesh.attribute(Mesh::ATTRIBUTE_COLOR)
        else {
            panic!("the rusted blade must carry Float32x4 colours");
        };
        // Quantised for the reason `tints` quantises: this picks vertices out by identity
        // rather than measuring them.
        let rust = RUST_TINT.map(|channel| (channel * 255.0).round() as u8);
        let marked: Vec<[f32; 3]> = all
            .iter()
            .zip(colours)
            .filter(|(_, colour)| colour.map(|channel| (channel * 255.0).round() as u8) == rust)
            .map(|(position, _)| *position)
            .collect();

        // One mark is one box and `merge` appends, so the tinted vertices arrive in whole
        // marks, in the order they were built.
        let per_mark = Mesh::from(Cuboid::from_size(Vec3::ONE)).count_vertices();
        assert_eq!(
            marked.len(),
            RUST_MARKS as usize * per_mark,
            "the rust is {} vertices, which is not {RUST_MARKS} boxes of {per_mark}",
            marked.len()
        );

        let proud = BLADE_THICKNESS * RUST_MARK_PROUD;
        let base = blade_base();
        let mut faces = [false; 2];
        let mut centres: Vec<f32> = Vec::new();
        for (index, one) in marked.chunks(per_mark).enumerate() {
            let (low_x, high_x) = extent(one, 0);
            let (low_y, high_y) = extent(one, 1);
            let (low_z, high_z) = extent(one, 2);

            let longest = [high_x - low_x, high_y - low_y, high_z - low_z]
                .into_iter()
                .fold(0.0, f32::max);
            assert!(
                (RUST_MARK_SIZE * 0.5 - 1e-6..=RUST_MARK_SIZE + 1e-6).contains(&longest),
                "mark {index} is {longest} on its longest side, outside half to all of \
                 {RUST_MARK_SIZE}"
            );

            // Inside the blade lengthwise and off the last few per cent at each end: a mark
            // overhanging the tip blunts it, one inside the guard is invisible.
            assert!(
                low_y > base + BLADE_LENGTH * RUST_MARK_MARGIN - 1e-6
                    && high_y < base + BLADE_LENGTH * (1.0 - RUST_MARK_MARGIN) + 1e-6,
                "mark {index} spans y {low_y}..{high_y}, outside the blade's rustable length"
            );

            // On one face rather than wrapped across both: that is what alternating faces
            // means, and a mark straddling the mid-plane would satisfy every other clause.
            assert!(
                low_x * high_x > 0.0,
                "mark {index} spans x {low_x}..{high_x}, so it wraps the blade rather than \
                 sitting on one face of it"
            );
            faces[usize::from(high_x > 0.0)] = true;

            let section = blade_at((low_y + high_y) / 2.0);
            let centre = (low_z + high_z) / 2.0;
            let surface = blade_surface(section, centre);
            let outer = low_x.abs().max(high_x.abs());
            let inner = low_x.abs().min(high_x.abs());
            assert!(
                (outer - (surface + proud)).abs() < 1e-6,
                "mark {index} reaches {outer} from the mid-plane where the blade's surface is \
                 at {surface}, so it is not bedded into the face it sits on"
            );

            // **The shallowest corner, not the middle.** The surface falls in two directions
            // under a mark — across the bevel as `|z|` grows, and along the blade as it
            // tapers toward the point — so the corner that decides whether the mark is bedded
            // is the highest and the farthest, and the section under *that* is the one to ask.
            // Measuring the middle instead is what let the fourteenth mark float 0.00034 clear
            // of the point while this test passed: at its own centre the blade is 0.00242 deep
            // and it was bedded to 0.00122, which looks bedded until you look 0.0038 higher up,
            // where the blade has thinned to 0.00088. Both sections are checked; they are the
            // same number for every mark on the flat, and differ only where the taper is real.
            let far = centre.abs() + (high_z - low_z) / 2.0;
            let top = blade_at(high_y);
            for (where_, at) in [("its centre", section), ("its upper edge", top)] {
                assert!(
                    far < at.half_width,
                    "mark {index} reaches {far} across a blade half {} wide at {where_}, so it \
                     overhangs an edge",
                    at.half_width
                );
                assert!(
                    inner <= blade_surface(at, far) + 1e-9,
                    "mark {index} is bedded to {inner} where the blade's surface under its far \
                     edge at {where_} is {}, so it floats clear of the blade",
                    blade_surface(at, far)
                );
            }
            assert!(
                inner > 0.0,
                "mark {index} reaches through the mid-plane, so it shows on the far face too"
            );

            centres.push((low_y + high_y) / 2.0);
        }

        assert_eq!(faces, [true; 2], "every mark is on one face of the blade");

        let (lowest, highest) = (
            centres.iter().copied().fold(f32::INFINITY, f32::min),
            centres.iter().copied().fold(f32::NEG_INFINITY, f32::max),
        );
        assert!(
            highest - lowest > BLADE_LENGTH * 0.6,
            "the marks span {} of a {BLADE_LENGTH} blade, so they are a band rather than \
             weathering",
            highest - lowest
        );
        let mut heights: Vec<i32> = centres.iter().map(|y| (y * 1e6) as i32).collect();
        heights.sort_unstable();
        heights.dedup();
        assert_eq!(
            heights.len(),
            RUST_MARKS as usize,
            "two marks share a height, so the scatter is not scattering"
        );
    }

    /// **The sword is not inside out**, which is the one failure in here that costs the most
    /// to diagnose.
    ///
    /// A lofted section walked the wrong way round produces a mesh that is geometrically
    /// perfect and invisible: back-face culling removes every triangle you can see and keeps
    /// every triangle you cannot, so the sword disappears when you look at it and reappears
    /// from inside. Nothing else in this file's tests would notice — the extents, the taper,
    /// the bevel and the rust are all statements about positions.
    ///
    /// Two independent readings, because they fail apart. The winding check says the stored
    /// normal agrees with the order the triangle's own corners are in; the radial check says
    /// that order is the outward one rather than a consistent inward one, which is exactly
    /// what reversing a section produces.
    #[test]
    fn every_face_of_the_sword_looks_outward() {
        let mesh = sword_mesh(SWORD_LENGTH);

        let Some(VertexAttributeValues::Float32x3(positions)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("the sword must carry Float32x3 positions");
        };
        let Some(VertexAttributeValues::Float32x3(normals)) =
            mesh.attribute(Mesh::ATTRIBUTE_NORMAL)
        else {
            panic!("the sword must carry Float32x3 normals");
        };
        let indices: Vec<usize> = mesh
            .indices()
            .expect("the sword is indexed")
            .iter()
            .collect();

        let point = |index: usize| Vec3::from_array(positions[index]);
        for corner in indices.chunks(3) {
            let [a, b, c] = corner else {
                panic!("the sword's indices are not whole triangles")
            };
            let wound = (point(*b) - point(*a)).cross(point(*c) - point(*a));
            let stored = Vec3::from_array(normals[*a]);
            assert!(
                wound.dot(stored) > 0.0,
                "the triangle at {a} is wound against the normal it carries, so it draws from \
                 the wrong side"
            );

            // Away from the sword's own axis, for every face that has an opinion about it.
            // The two end caps do not — they look along the axis — and they are the ones
            // this term skips rather than the ones it fails on.
            let middle = (point(*a) + point(*b) + point(*c)) / 3.0;
            let radial = Vec3::new(middle.x, 0.0, middle.z);
            if radial.length() > 1e-4 && stored.xz().length() > 1e-4 {
                assert!(
                    radial.dot(stored) > 0.0,
                    "the triangle at {a} faces in toward the sword's axis, so the mesh is \
                     inside out"
                );
            }
        }
    }

    /// **The same sword every run**, which is what a seeded generator buys over a random one.
    ///
    /// A blade whose freckles moved between sessions would be the one thing about it a player
    /// could not learn, and the failure is invisible inside any single run.
    #[test]
    fn the_rusty_sword_is_scattered_the_same_way_every_time() {
        let read = |mesh: &Mesh| {
            let Some(VertexAttributeValues::Float32x3(positions)) =
                mesh.attribute(Mesh::ATTRIBUTE_POSITION)
            else {
                panic!("the rusted blade must carry Float32x3 positions");
            };
            positions.clone()
        };
        assert_eq!(
            read(&rusted_blade_mesh()),
            read(&rusted_blade_mesh()),
            "two builds of one sword put the rust in different places"
        );
    }

    /// **The sword still fits the motion that already exists**, in all three of its arcs.
    ///
    /// #174 replaced the one swing with three, and a shape that ends in a point is exactly the
    /// kind of change that reads well in a cut and slices through the camera in a thrust. So
    /// this walks the *real vertices* — not a bounding box, whose corners no vertex of this
    /// shape occupies — through every arc frame by frame and asks the one question a near
    /// plane asks.
    ///
    /// The placement bump is swept alongside, because it is the only animation that carries
    /// the model *toward* the camera and it can coincide with a swing: a right click and a
    /// left click inside the same 220 ms both play. That combination is the tightest pose the
    /// view model ever reaches, and it is worth recording that the single box this replaced
    /// did **not** clear the near plane there while the sword does — the pommel's corner sits
    /// closer to the axis of the swing than the box's did.
    #[test]
    fn every_held_arrangement_clears_the_near_plane_through_every_swing() {
        let mut app = app();
        app.update();
        let parent = held(&mut app).2;
        let Projection::Perspective(projection) = app
            .world()
            .get::<Projection>(parent)
            .expect("the world camera has a projection")
        else {
            panic!("the world camera is perspective");
        };
        let near = projection.near;

        let appearances = shape_examples()
            .into_iter()
            .map(|(_, item_id)| {
                selected_appearance(Some(InventoryStack {
                    item_id,
                    count: 1,
                    ..Default::default()
                }))
            })
            .chain([selected_appearance(None)]);

        for appearance in appearances {
            let corners = positions(&held_mesh(TEST_SKIN, appearance));
            let mut arcs: Vec<Option<SwingShape>> = SwingShape::ALL.map(Some).to_vec();
            arcs.push(None);
            for shape in arcs {
                for step in 0..=32u8 {
                    for bump in 0..=16u8 {
                        let animation = HandAnimation {
                            attack: shape.map(|shape| Swing {
                                shape,
                                elapsed: ATTACK_SWING_TIME.mul_f32(f32::from(step) / 32.0),
                            }),
                            bump_elapsed: Some(PLACE_BUMP_TIME.mul_f32(f32::from(bump) / 16.0)),
                            ..Default::default()
                        };
                        let transform = animated_transform(&animation);
                        for corner in &corners {
                            let point = transform.transform_point(Vec3::from_array(*corner));
                            assert!(
                                -point.z > near,
                                "{:?} in {shape:?} at {step}/32 with the bump at {bump}/16 \
                                 carries {corner:?} to z {} against a near plane at {near}",
                                appearance.shape,
                                point.z
                            );
                        }
                    }
                }
            }
        }
    }

    /// The rust reaches the screen only for the sword it belongs to.
    ///
    /// Read through the mesh the hand is actually built from, so it is the routing under
    /// test rather than the table: holding the iron sword must not produce the rusted mesh.
    #[test]
    fn only_the_rusty_sword_is_drawn_rusted() {
        for (item_id, want_rusted) in [(ITEM_RUSTY_SWORD, true), (ITEM_IRON_SWORD, false)] {
            let appearance = selected_appearance(Some(InventoryStack {
                item_id,
                count: 1,
                ..Default::default()
            }));
            let mesh = held_mesh(TEST_SKIN, appearance);
            let item_colour = appearance.item_colour.expect("an item has a colour");
            let rust = std::array::from_fn(|channel| item_colour[channel] * RUST_TINT[channel]);
            let rust = rust.map(|channel| (channel * 255.0).round() as u8);
            assert_eq!(
                tints(&mesh).contains(&rust),
                want_rusted,
                "item {item_id} carries a rust tint = {}, want {want_rusted}",
                tints(&mesh).contains(&rust)
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
        let mesh = held_mesh(TEST_SKIN, selected_appearance(None));

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

    /// The cause of #240 was an exclusive mesh match: five item shapes replaced the fist.
    /// The exact fist now starts every composite, including the empty one, so this sweep
    /// fails if any future arrangement takes that shortcut again.
    #[test]
    fn the_same_fist_is_present_whatever_the_hand_holds() {
        let fist = positions(&fist_mesh());
        let appearances = shape_examples()
            .into_iter()
            .map(|(shape, item_id)| {
                let appearance = selected_appearance(Some(InventoryStack {
                    item_id,
                    count: 1,
                    ..Default::default()
                }));
                assert_eq!(appearance.shape, Some(shape));
                appearance
            })
            .chain([selected_appearance(None)]);

        for appearance in appearances {
            let composite = positions(&held_mesh(TEST_SKIN, appearance));
            assert_eq!(
                &composite[..fist.len()],
                fist,
                "{:?} replaced or moved the fist instead of composing with it",
                appearance.shape
            );
        }
    }

    /// Holding is overlap, not concealment: at least a quarter of every item's vertices
    /// remain outside the fist's box after the arrangement is applied. A sword spends many
    /// vertices on its grip, guard and pommel inside the palm; its blade is the third that
    /// remains outside, which is the silhouette the arrangement must preserve.
    #[test]
    fn the_item_stays_recognisable_outside_the_fist() {
        let half = HAND_SIZE / 2.0;
        for (shape, item_id) in shape_examples() {
            let item = item_mesh(item_id, shape).translated_by(item_translation(shape));
            let item_positions = positions(&item);
            let outside = item_positions
                .iter()
                .filter(|position| {
                    position[0].abs() > half.x + 1e-6
                        || position[1].abs() > half.y + 1e-6
                        || position[2].abs() > half.z + 1e-6
                })
                .count();
            assert!(
                outside * 4 >= item_positions.len(),
                "only {outside}/{} vertices of {shape:?} remain outside the fist",
                item_positions.len()
            );
        }
    }

    /// Skin comes from the local player's authoritative appearance, item colour from the
    /// display table, and white material identity lets both coexist in one draw.
    #[test]
    fn the_hand_and_item_keep_their_two_authoritative_colours_on_one_material() {
        let mut app = app();
        let look = PlayerLook::new(
            TEST_SKIN,
            0x0011_2233,
            0x0044_5566,
            0x0077_8899,
            crate::net::HairModel::Shaved,
            0x000F_0E0D,
        )
        .expect("the test appearance is legal");
        app.world_mut()
            .resource_mut::<AppearanceInbox>()
            .push(PlayerAppearance {
                entity_id: session().0.entity_id,
                appearance: look,
            });
        app.update();

        let world = app.world_mut();
        let mut query = world.query::<(&HeldItem, &Mesh3d, &MeshMaterial3d<StandardMaterial>)>();
        let (held, mesh, material) = query.single(world).expect("one held arrangement");
        assert_eq!(held.skin_colour, TEST_SKIN);

        let meshes = world.resource::<Assets<Mesh>>();
        let colours = tints(meshes.get(&mesh.0).expect("the held mesh"));
        let skin = linear_rgb(TEST_SKIN).map(|channel| (channel * 255.0).round() as u8);
        let stone =
            items::item_linear_rgba(ITEM_STONE).map(|channel| (channel * 255.0).round() as u8);
        assert!(colours.contains(&skin), "the mesh has no local skin colour");
        assert!(
            colours.contains(&stone),
            "the mesh has no item-table colour"
        );

        let materials = world.resource::<Assets<StandardMaterial>>();
        assert_eq!(
            materials
                .get(&material.0)
                .expect("the held material")
                .base_color,
            Color::WHITE,
            "the material tinted both vertex colours a second time"
        );
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
            .max(BUNDLE_SIZE.z)
            // The sword's widest point is its cross guard, not its blade — the one held
            // shape whose depth is not the constant naming it.
            .max(GUARD_SIZE.z);
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
        let mut query = world.query::<(&HeldItem, &Mesh3d, &MeshMaterial3d<StandardMaterial>)>();
        let (held, mesh, material) = query.single(world).expect("one held item");
        assert_eq!(held.shape, Some(ItemShape::Material));
        assert_eq!(
            world
                .resource::<Assets<StandardMaterial>>()
                .get(&material.0)
                .expect("the held material")
                .base_color,
            Color::WHITE
        );
        let colours = tints(
            world
                .resource::<Assets<Mesh>>()
                .get(&mesh.0)
                .expect("the held mesh"),
        );
        let fallback =
            palette::linear_rgba(u16::MAX).map(|channel| (channel * 255.0).round() as u8);
        assert!(
            colours.contains(&fallback),
            "the item vertices do not carry the palette fallback"
        );
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
                carried[first].item_colour, carried[second].item_colour,
                "items {} and {} are carried in the same colour",
                bundles[first], bundles[second]
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
            iron.item_colour, rusty.item_colour,
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
                items::item_linear_rgba(known),
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
                items::item_linear_rgba(item_id),
                selected_appearance(Some(InventoryStack {
                    item_id,
                    count: 1,
                    ..Default::default()
                }))
                .item_colour
                .expect("an item has a colour"),
                "item {item_id}"
            );
        }

        // And an id from a newer contract still reaches the palette's loud placeholder
        // rather than a plausible shade this module invented.
        assert_eq!(
            items::item_linear_rgba(u16::MAX),
            palette::linear_rgba(u16::MAX)
        );
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
