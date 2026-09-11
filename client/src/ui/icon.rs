//! What one item looks like drawn flat, inside a pack or hotbar cell.
//!
//! **One shape vocabulary, two renderers.** `player::hands` builds a mesh per
//! [`ItemShape`] for the hand; this module builds a picture per [`ItemShape`] for a cell.
//! Both read the shape from the one row in `player::items`, so a sword is a sword on both
//! surfaces and neither can re-decide what an item is. That is the whole of why the
//! drawings below are keyed on the shape rather than on item ids: eleven items share four
//! pictures, and a twelfth item inherits one by having a shape at all.
//!
//! **Procedural, like everything else this client draws.** A picture is a handful of
//! `bevy_ui` nodes — coloured rectangles, some rounded, some rotated — positioned in
//! percentages of the cell. No image, no sprite sheet, no atlas, and therefore no asset
//! pipeline and no dependency. It also keeps the result readable by a headless test: an
//! icon here is a set of components, not a texture somebody has to look at.
//!
//! **A picture decides nothing.** The shape is presentation, exactly as its row says: what
//! an item does is the server's registry, and a cell drawn as a blade no more swings than
//! a hand drawn as one does.
//!
//! **The map's marks are drawn here too**, from the same [`IconPart`] vocabulary and through
//! the same [`part_bundle`], keyed on [`MarkerKind`] instead of on [`ItemShape`]. They are
//! not items and share no table with them; what they share is the renderer, which is the
//! whole reason a second picture in this client costs a table and not a mechanism.

use bevy::color::LinearRgba;
use bevy::math::Rot2;
use bevy::prelude::*;
use bevy::ui::FocusPolicy;

use crate::net::MarkerKind;
use crate::player::{ArmourPiece, ArmourStyle, ItemShape, Liveries, Livery, field_rect};

/// The picture one cell is drawing: a shape, in the item's colour.
///
/// Both fields come from the same registry row — [`crate::player::item_shape`] and
/// [`crate::player::item_linear_rgba`] — so a cell cannot disagree with the hand about
/// either half.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StackIcon {
    pub(crate) shape: ItemShape,
    pub(crate) colour: Color,
    /// The generated surface this item's material wears, when it wears one.
    ///
    /// **The cell is the surface that could never have joined a geometric answer**, and this
    /// field is why it can join an asset. It has no vertices to tint — a picture here is
    /// `bevy_ui` rectangles — but `ImageNode` carries a `Handle<Image>`, a `color` that
    /// multiplies it and a `rect` selecting a region, which is exactly the three things a
    /// livery needs. The drawing stays keyed on [`ItemShape`]; this says which of the
    /// rectangles in it sample an image.
    pub(crate) livery: Option<Livery>,
    /// The sculpted set and piece an armour item is drawn as, when it is one
    /// (`player::sculpted_icon`).
    ///
    /// **The one refinement of a drawing keyed on the shape.** A helm, a cuirass and greaves
    /// of one sculpted set are three silhouettes on a body and on the ground, and one plate in
    /// the pack would say otherwise; an armour item with no set keeps [`ARMOUR`].
    pub(crate) armour: Option<(ArmourStyle, ArmourPiece)>,
}

/// One rectangle of a picture.
///
/// Everything is a percentage of the cell rather than a pixel count, so [`super::CELL_SIZE`]
/// stays the only number that decides how big a cell is and an icon follows it for free.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct IconPart {
    /// The part's box, as percentages of the cell: distance from its left and top edges,
    /// then width and height.
    left: f32,
    top: f32,
    width: f32,
    height: f32,
    /// Corner rounding, as a percentage of the part's own shorter side. `50` is a circle.
    radius: f32,
    /// How this part is shaded relative to the item's colour — see [`shaded`].
    shade: f32,
    /// Clockwise rotation about the part's own centre, in radians.
    rotation: f32,
    /// Uses iron instead of the item's base colour.
    iron: bool,
    /// Uses the sceptre focus green instead of the item's base colour.
    green: bool,
    /// Draws the item's livery over this rectangle, when the item wears one.
    ///
    /// **A property of the rectangle, not of the shape.** A blade's guard and grip are not
    /// steel that rusts in the same way its edge is, and the cell says so with one flag
    /// rather than with a second picture.
    livery: bool,
}

impl IconPart {
    /// A square-cornered, unrotated part in the item's own colour. Every drawing below is
    /// written as a deviation from this, so a part says only what makes it different.
    const PLAIN: Self = Self {
        left: 0.0,
        top: 0.0,
        width: 0.0,
        height: 0.0,
        radius: 0.0,
        shade: 0.0,
        rotation: 0.0,
        iron: false,
        green: false,
        livery: false,
    };
}

/// A quarter turn, which is the angle every part of the blade is drawn at.
const QUARTER_TURN: f32 = std::f32::consts::FRAC_PI_4;

/// A cube, in three faces: lit on top, the item's own colour in front, turned away at the
/// side. The same reading a voxel gets in the world, which is what makes a carried block
/// look like the thing it places.
const BLOCK: [IconPart; 3] = [
    IconPart {
        left: 16.0,
        top: 18.0,
        width: 68.0,
        height: 18.0,
        shade: 0.30,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 16.0,
        top: 34.0,
        width: 44.0,
        height: 46.0,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 58.0,
        top: 34.0,
        width: 26.0,
        height: 46.0,
        shade: -0.42,
        ..IconPart::PLAIN
    },
];

/// A small heap of raw material: three nuggets, the top one catching the light. Round, so
/// it never reads as a cube at a glance.
const MATERIAL: [IconPart; 3] = [
    IconPart {
        left: 24.0,
        top: 40.0,
        width: 30.0,
        height: 30.0,
        radius: 50.0,
        shade: -0.30,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 48.0,
        top: 44.0,
        width: 28.0,
        height: 28.0,
        radius: 50.0,
        shade: -0.12,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 34.0,
        top: 24.0,
        width: 30.0,
        height: 30.0,
        radius: 50.0,
        shade: 0.22,
        ..IconPart::PLAIN
    },
];

/// A struck coin: one disc, with a smaller darker one inset for the device stamped into it.
///
/// Two circles and nothing else, which is what a coin can be at this size — and what keeps
/// it from reading as the three-nugget heap [`MATERIAL`] draws. The inset is *darker* rather
/// than lighter so it reads as struck into the face rather than sitting on it; the rim is
/// lifted for the same reason.
///
/// **It is drawn at two sizes from one table.** The pack cell hangs it in a
/// [`super::CELL_SIZE`] square and the inventory window's silver readout hangs it in an
/// 18-pixel one, and because every part is a percentage of its host neither of them names a
/// pixel — see [`IconPart`], which is why a second size costs a node and not a drawing.
const COIN: [IconPart; 2] = [
    IconPart {
        left: 20.0,
        top: 20.0,
        width: 60.0,
        height: 60.0,
        radius: 50.0,
        shade: 0.20,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 36.0,
        top: 36.0,
        width: 28.0,
        height: 28.0,
        radius: 50.0,
        shade: -0.32,
        ..IconPart::PLAIN
    },
];

/// A horse head at a slight three-quarter angle: neck connection, poll, long face, muzzle
/// and two ears.
///
/// Six overlapping rounded rectangles keep the silhouette readable in the vendor's 34-pixel
/// host as well as the larger inventory cell. The item's exact world-coat colour comes through
/// the registry; the shades here only separate the planes of that one colour.
const HORSE_HEAD: [IconPart; 6] = [
    // The neck leaves the poll down and to the right, behind the face.
    IconPart {
        left: 43.0,
        top: 53.0,
        width: 30.0,
        height: 34.0,
        radius: 22.0,
        shade: -0.34,
        rotation: -0.20,
        ..IconPart::PLAIN
    },
    // Poll and cheek, broad enough to hold both ears.
    IconPart {
        left: 28.0,
        top: 24.0,
        width: 46.0,
        height: 38.0,
        radius: 30.0,
        shade: 0.16,
        rotation: 0.12,
        ..IconPart::PLAIN
    },
    // The long face narrows the picture towards the muzzle.
    IconPart {
        left: 29.0,
        top: 40.0,
        width: 35.0,
        height: 38.0,
        radius: 28.0,
        rotation: 0.25,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 24.0,
        top: 65.0,
        width: 30.0,
        height: 19.0,
        radius: 36.0,
        shade: -0.20,
        rotation: 0.25,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 32.0,
        top: 10.0,
        width: 13.0,
        height: 26.0,
        radius: 45.0,
        shade: -0.12,
        rotation: -0.14,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 55.0,
        top: 8.0,
        width: 12.0,
        height: 28.0,
        radius: 45.0,
        shade: 0.30,
        rotation: 0.19,
        ..IconPart::PLAIN
    },
];

/// A sword: a bright bar on the cell's diagonal, a cross guard across it, a grip below.
///
/// The guard is perpendicular *by construction* rather than by a second angle — it is a
/// bar drawn across the cell where the blade is drawn along it, and the same quarter turn
/// then takes them to right angles. One number to change if the tilt ever moves.
///
/// **Checked against the sword `player::hands` builds and deliberately left alone** (#204).
/// That issue gave the two 3D renderers a bevelled blade tapering to a point, a cross guard,
/// a grip and a pommel, where they had had a single box; this drawing already had three of
/// those four and was the renderer the other two were catching up to, so the criterion it
/// answers is that the three stop disagreeing rather than that a third one changes.
///
/// The two the flat picture does not carry are the two a cell has no room for. It is
/// [`super::CELL_SIZE`] pixels square and the blade part is 12% of that — six pixels across
/// — so a taper would be one pixel a side and a pommel would be two by two, which is a
/// smudge on the end of the grip rather than a pommel. The structural agreement is pinned by
/// `the_flat_sword_draws_the_parts_the_held_one_does`.
const BLADE: [IconPart; 3] = [
    IconPart {
        left: 44.0,
        top: 12.0,
        width: 12.0,
        height: 60.0,
        shade: 0.34,
        rotation: QUARTER_TURN,
        // The edge, and the only part of the picture a livery reaches — see
        // [`IconPart::livery`].
        livery: true,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 20.0,
        top: 52.0,
        width: 32.0,
        height: 8.0,
        shade: -0.25,
        rotation: QUARTER_TURN,
        ..IconPart::PLAIN
    },
    // Every field is spelled out here, so there is nothing left for `PLAIN` to fill in.
    IconPart {
        left: 24.0,
        top: 54.0,
        width: 11.0,
        height: 16.0,
        radius: 30.0,
        shade: -0.55,
        rotation: QUARTER_TURN,
        iron: false,
        green: false,
        livery: false,
    },
];

/// A bundle: wider than it is tall and tied with a cord, so a carried structure never
/// reads as another stackable cube. Same argument as the held view model's, drawn flat.
const BUNDLE: [IconPart; 3] = [
    IconPart {
        left: 14.0,
        top: 30.0,
        width: 72.0,
        height: 14.0,
        radius: 18.0,
        shade: 0.26,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 14.0,
        top: 42.0,
        width: 72.0,
        height: 34.0,
        radius: 14.0,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 44.0,
        top: 28.0,
        width: 12.0,
        height: 50.0,
        shade: -0.48,
        ..IconPart::PLAIN
    },
];

/// An implement: a haft up the cell with a head across the top of it.
///
/// The T is what tells it from [`BLADE`] at a glance — a blade is one tapering box, and
/// this is a handle with weight on the end. Only the axe is drawn as it since #1121; the
/// pickaxe and the shovel are [`PICKAXE`] and [`SHOVEL`].
///
/// Drawn head-last so it sits over the haft, which is the order these arrays mean.
const TOOL: [IconPart; 3] = [
    // The haft, up the middle and slightly right of centre so the head has somewhere to
    // overhang.
    IconPart {
        left: 44.0,
        top: 26.0,
        width: 12.0,
        height: 54.0,
        radius: 6.0,
        shade: -0.42,
        ..IconPart::PLAIN
    },
    // The head, across the top.
    IconPart {
        left: 20.0,
        top: 20.0,
        width: 60.0,
        height: 20.0,
        radius: 6.0,
        shade: 0.22,
        ..IconPart::PLAIN
    },
    // And the lit edge along the head's top, which is what stops it reading as a flat bar.
    IconPart {
        left: 20.0,
        top: 20.0,
        width: 60.0,
        height: 7.0,
        radius: 6.0,
        shade: 0.52,
        ..IconPart::PLAIN
    },
];

/// A pickaxe: a wooden haft up the cell under a curved, two-pointed iron head.
///
/// The head is an iron eye over the haft's end and two arms leaving it, each turned so its
/// outer end falls — the curve the held and dropped meshes lift and drop in two segments. The
/// haft is the row's colour, which is the log's wood; the three head parts are `iron`, the
/// same flag the shield's boss uses. Drawn haft first, head over it.
const PICKAXE: [IconPart; 4] = [
    IconPart {
        left: 45.0,
        top: 30.0,
        width: 10.0,
        height: 56.0,
        radius: 6.0,
        shade: -0.30,
        ..IconPart::PLAIN
    },
    // The left arm, turned counter-clockwise so its outer end drops.
    IconPart {
        left: 10.0,
        top: 22.0,
        width: 42.0,
        height: 11.0,
        radius: 45.0,
        rotation: -0.32,
        shade: 0.10,
        iron: true,
        ..IconPart::PLAIN
    },
    // And the right one, mirrored.
    IconPart {
        left: 48.0,
        top: 22.0,
        width: 42.0,
        height: 11.0,
        radius: 45.0,
        rotation: 0.32,
        shade: -0.08,
        iron: true,
        ..IconPart::PLAIN
    },
    // The eye closed over the haft's end, lit, on top of both arms.
    IconPart {
        left: 40.0,
        top: 18.0,
        width: 20.0,
        height: 18.0,
        radius: 12.0,
        shade: 0.40,
        iron: true,
        ..IconPart::PLAIN
    },
];

/// A shovel: a flat iron blade over a wooden haft that ends in a small D-grip.
///
/// Blade up, as the hand holds it and as [`TOOL`] and [`PICKAXE`] put their heads: a cell and
/// the hand agree about which end the work is at. The blade is the broadest, roundest-ended
/// part of any implement drawing, which is what reads as a spade at a cell's size; the grip is
/// a crossbar across the haft's foot. Drawn haft first, head over it.
const SHOVEL: [IconPart; 4] = [
    IconPart {
        left: 45.0,
        top: 36.0,
        width: 10.0,
        height: 48.0,
        radius: 6.0,
        shade: -0.30,
        ..IconPart::PLAIN
    },
    // The D-grip's crossbar across the foot of the haft.
    IconPart {
        left: 34.0,
        top: 82.0,
        width: 32.0,
        height: 9.0,
        radius: 40.0,
        shade: -0.12,
        ..IconPart::PLAIN
    },
    // The blade: broad, flat and rounded at its working end.
    IconPart {
        left: 29.0,
        top: 8.0,
        width: 42.0,
        height: 32.0,
        radius: 32.0,
        shade: 0.20,
        iron: true,
        ..IconPart::PLAIN
    },
    // The socket where the blade closes over the haft, in shadow under it.
    IconPart {
        left: 40.0,
        top: 36.0,
        width: 20.0,
        height: 8.0,
        radius: 20.0,
        shade: -0.25,
        iron: true,
        ..IconPart::PLAIN
    },
];

/// Armour laid flat: a broad chest plate with two shoulders and a narrowed waist.
///
/// The picture of every armour item that is not a sculpted piece — the leather set today —
/// told apart by the item registry's colour. A sculpted piece draws [`armour_parts`] instead.
const ARMOUR: [IconPart; 3] = [
    IconPart {
        left: 27.0,
        top: 24.0,
        width: 46.0,
        height: 58.0,
        radius: 12.0,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 14.0,
        top: 20.0,
        width: 72.0,
        height: 20.0,
        radius: 28.0,
        shade: 0.28,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 34.0,
        top: 64.0,
        width: 32.0,
        height: 18.0,
        radius: 10.0,
        shade: -0.34,
        ..IconPart::PLAIN
    },
];

/// The rusty helm, face on: a domed shell under a low crest, the brow band across it, two dark
/// eye slits split by the nasal ridge and a barred grille over the mouth — the silhouette the
/// worn helm is cut in.
const RUSTY_HELM: [IconPart; 8] = [
    IconPart {
        left: 45.0,
        top: 8.0,
        width: 10.0,
        height: 20.0,
        radius: 40.0,
        shade: 0.25,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 24.0,
        top: 14.0,
        width: 52.0,
        height: 66.0,
        radius: 38.0,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 20.0,
        top: 38.0,
        width: 60.0,
        height: 9.0,
        radius: 20.0,
        shade: 0.30,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 31.0,
        top: 51.0,
        width: 15.0,
        height: 5.0,
        shade: -0.75,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 54.0,
        top: 51.0,
        width: 15.0,
        height: 5.0,
        shade: -0.75,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 47.0,
        top: 34.0,
        width: 6.0,
        height: 30.0,
        shade: 0.35,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 34.0,
        top: 64.0,
        width: 32.0,
        height: 10.0,
        radius: 10.0,
        shade: -0.60,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 34.0,
        top: 68.0,
        width: 32.0,
        height: 2.5,
        shade: 0.10,
        ..IconPart::PLAIN
    },
];

/// The rusty cuirass laid flat: a breastplate with its centre ridge, domed pauldrons at both
/// shoulders, the gorget at the neck and a fauld lame under a dark seam at the waist.
const RUSTY_CUIRASS: [IconPart; 7] = [
    IconPart {
        left: 27.0,
        top: 24.0,
        width: 46.0,
        height: 52.0,
        radius: 14.0,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 12.0,
        top: 20.0,
        width: 26.0,
        height: 20.0,
        radius: 45.0,
        shade: 0.25,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 62.0,
        top: 20.0,
        width: 26.0,
        height: 20.0,
        radius: 45.0,
        shade: 0.25,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 38.0,
        top: 14.0,
        width: 24.0,
        height: 11.0,
        radius: 30.0,
        shade: 0.15,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 48.0,
        top: 30.0,
        width: 4.0,
        height: 42.0,
        shade: 0.40,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 29.0,
        top: 74.0,
        width: 42.0,
        height: 3.0,
        shade: -0.60,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 26.0,
        top: 77.0,
        width: 48.0,
        height: 10.0,
        radius: 18.0,
        shade: -0.15,
        ..IconPart::PLAIN
    },
];

/// The rusty greaves side by side: two shin plates with a ridge down each, knee cops standing
/// proud of them and a flare at each ankle.
const RUSTY_GREAVES: [IconPart; 8] = [
    IconPart {
        left: 22.0,
        top: 22.0,
        width: 20.0,
        height: 58.0,
        radius: 16.0,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 58.0,
        top: 22.0,
        width: 20.0,
        height: 58.0,
        radius: 16.0,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 30.0,
        top: 54.0,
        width: 4.0,
        height: 22.0,
        shade: 0.40,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 66.0,
        top: 54.0,
        width: 4.0,
        height: 22.0,
        shade: 0.40,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 19.0,
        top: 36.0,
        width: 26.0,
        height: 16.0,
        radius: 45.0,
        shade: 0.30,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 55.0,
        top: 36.0,
        width: 26.0,
        height: 16.0,
        radius: 45.0,
        shade: 0.30,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 18.0,
        top: 78.0,
        width: 28.0,
        height: 8.0,
        radius: 30.0,
        shade: -0.20,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 54.0,
        top: 78.0,
        width: 28.0,
        height: 8.0,
        radius: 30.0,
        shade: -0.20,
        ..IconPart::PLAIN
    },
];

/// Wooden board, rim and iron boss.
const SHIELD: [IconPart; 3] = [
    IconPart {
        left: 24.0,
        top: 16.0,
        width: 52.0,
        height: 64.0,
        radius: 24.0,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 30.0,
        top: 66.0,
        width: 40.0,
        height: 17.0,
        radius: 50.0,
        shade: -0.22,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 40.0,
        top: 35.0,
        width: 20.0,
        height: 20.0,
        radius: 50.0,
        shade: 0.72,
        iron: true,
        ..IconPart::PLAIN
    },
];

/// A bowed stave in two limbs and its taut string.
const BOW: [IconPart; 3] = [
    IconPart {
        left: 28.0,
        top: 12.0,
        width: 10.0,
        height: 42.0,
        radius: 35.0,
        rotation: -0.24,
        shade: 0.18,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 28.0,
        top: 46.0,
        width: 10.0,
        height: 42.0,
        radius: 35.0,
        rotation: 0.24,
        shade: -0.18,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 58.0,
        top: 13.0,
        width: 4.0,
        height: 74.0,
        radius: 20.0,
        shade: 0.65,
        ..IconPart::PLAIN
    },
];

/// A wooden shaft with a small green focus at its tip.
const SCEPTRE: [IconPart; 2] = [
    IconPart {
        left: 45.0,
        top: 24.0,
        width: 10.0,
        height: 62.0,
        radius: 20.0,
        rotation: QUARTER_TURN,
        shade: -0.18,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 58.0,
        top: 12.0,
        width: 24.0,
        height: 24.0,
        radius: 50.0,
        green: true,
        shade: 0.18,
        ..IconPart::PLAIN
    },
];

/// The rectangles one shape is drawn from, in the order they are stacked.
///
/// **Exhaustive, with no wildcard arm.** A fifth [`ItemShape`] does not compile until it
/// has been drawn, which is the same guarantee `player::items` gets from `ItemDisplay`'s
/// three mandatory fields: there is no branch for a new shape to fall through into a
/// square. The sweep in the tests below covers what the compiler cannot see — an arm
/// answered with an empty drawing, or with a copy of another shape's.
/// Whether one shape's picture has a rectangle that samples a livery.
///
/// **Not every shape does, and that is a drawing decision rather than an omission.** A blade
/// has an edge, and an edge is what a livery is about; a cell drawn as armour is a handful of
/// plates, and rust on them at a cell's size would be detail nobody can read. So a rusty helm
/// names `WornSteel` honestly and its cell shows the rust through the registry colour alone,
/// while the worn and dropped sculpted meshes carry the livery itself.
///
/// Test-only: [`part_bundle`] already answers it per rectangle, and this is the same fact one
/// level up, for the sweep that has to know which items can reach a livery in a cell at all.
#[cfg(test)]
pub(crate) fn draws_a_livery(shape: ItemShape) -> bool {
    parts(shape).iter().any(|part| part.livery)
}

/// The rectangles one cell's icon is drawn from: the sculpted piece's picture for an armour
/// item that is one, and its shape's picture for everything else.
pub(crate) fn drawing(icon: StackIcon) -> &'static [IconPart] {
    match icon.armour {
        Some((style, piece)) if icon.shape == ItemShape::Armour => armour_parts(style, piece),
        _ => parts(icon.shape),
    }
}

/// The rectangles one sculpted armour piece is drawn from.
///
/// **Wildcard-free over both halves**, so a new set does not compile until each of its three
/// pieces has a picture — the same guarantee [`parts`] gives a new shape.
pub(crate) fn armour_parts(style: ArmourStyle, piece: ArmourPiece) -> &'static [IconPart] {
    match (style, piece) {
        (ArmourStyle::Rusty, ArmourPiece::Head) => &RUSTY_HELM,
        (ArmourStyle::Rusty, ArmourPiece::Chest) => &RUSTY_CUIRASS,
        (ArmourStyle::Rusty, ArmourPiece::Legs) => &RUSTY_GREAVES,
    }
}

pub(crate) fn parts(shape: ItemShape) -> &'static [IconPart] {
    match shape {
        ItemShape::Block => &BLOCK,
        ItemShape::Material => &MATERIAL,
        ItemShape::Blade => &BLADE,
        ItemShape::Bundle => &BUNDLE,
        ItemShape::Tool => &TOOL,
        ItemShape::Pickaxe => &PICKAXE,
        ItemShape::Shovel => &SHOVEL,
        ItemShape::Armour => &ARMOUR,
        ItemShape::Shield => &SHIELD,
        ItemShape::Bow => &BOW,
        ItemShape::Sceptre => &SCEPTRE,
        ItemShape::Coin => &COIN,
        ItemShape::HorseHead => &HORSE_HEAD,
    }
}

/// One part's colour: the item's, mixed toward white or black by `shade`.
///
/// **A mix rather than a multiply**, because a multiply cannot separate a dark item from
/// itself. A log is `0.10` linear; scaled to three faces it is `0.10 / 0.08 / 0.06`, which
/// reads as one flat silhouette. Mixing toward white lifts the lit face away from the base
/// at every brightness, and mixing toward black does the same at the other end — so the
/// cube reads for snow and for coal with one pair of numbers.
fn shaded(base: LinearRgba, shade: f32) -> Color {
    let shade = shade.clamp(-1.0, 1.0);
    let target = if shade >= 0.0 { 1.0 } else { 0.0 };
    let towards = shade.abs();
    let mix = |channel: f32| channel + (target - channel) * towards;
    Color::linear_rgba(mix(base.red), mix(base.green), mix(base.blue), base.alpha)
}

/// The node every cell hangs its picture under: one per cell, filling it, drawing nothing
/// of its own.
///
/// It exists so that redrawing a cell is *replacing this node's children* rather than
/// picking the old rectangles out of the cell by hand. Accumulation is therefore not a
/// rule anybody has to remember, in the same way the single tooltip entity is not.
///
/// `FocusPolicy::Pass` is load-bearing and the omission is silent: a node with no policy
/// **blocks**, so an icon laid over its own cell would take the pointer, the cell would
/// fall to `Interaction::None`, and clicking a full slot would stop working while an empty
/// one still did. Every part below carries it for the same reason.
pub(crate) fn host_bundle() -> impl Bundle {
    (
        IconHost,
        DrawnIcon(None),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px(0.0),
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        FocusPolicy::Pass,
    )
}

/// Draws one immutable icon while a freshly rebuilt UI row is being spawned.
pub(crate) fn spawn(
    host: &mut ChildSpawnerCommands<'_>,
    icon: StackIcon,
    liveries: Option<&Liveries>,
) {
    let base = icon.colour.to_linear();
    let image = livery_image(icon, liveries);
    for part in drawing(icon) {
        let mut rect = host.spawn(part_bundle(part, base));
        if let Some(node) = livery_node(part, base, image.as_ref(), icon.livery) {
            rect.insert(node);
        }
    }
}

/// The image one cell samples, or `None` when the item wears no livery.
///
/// `Option<&Liveries>` because the resource is the player plugin's and the UI stands up
/// headlessly without it. An absent resource draws the flat rectangles this module always
/// drew, which is the honest fallback: a cell that cannot reach the image is a cell with no
/// livery to draw, not a cell that should refuse to draw.
fn livery_image(icon: StackIcon, liveries: Option<&Liveries>) -> Option<Handle<Image>> {
    icon.livery?;
    Some(liveries?.material_image())
}

/// The icon host inside one cell.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IconHost;

/// What the host is currently drawing, so a refresh can tell "no change" from "redraw".
///
/// The refresh path runs every frame; without this the cell would despawn and respawn its
/// rectangles sixty times a second for a stack nobody touched.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub(crate) struct DrawnIcon(Option<StackIcon>);

/// Draws `next` under `host`, replacing whatever was there, and does nothing at all when
/// that is already what is drawn.
///
/// `None` clears the host, which is how an emptied slot loses its picture — the same call,
/// not a second path.
pub(crate) fn redraw(
    commands: &mut Commands<'_, '_>,
    host: Entity,
    mut drawn: Mut<'_, DrawnIcon>,
    next: Option<StackIcon>,
    liveries: Option<&Liveries>,
) {
    // Read through the shared reference and write only on a real change, so a stack
    // nobody touched neither respawns its rectangles every frame nor reports itself as
    // having changed.
    if drawn.0 == next {
        return;
    }
    drawn.0 = next;

    let mut host = commands.entity(host);
    host.despawn_related::<Children>();
    let Some(icon) = next else {
        return;
    };
    let base = icon.colour.to_linear();
    let image = livery_image(icon, liveries);
    host.with_children(|host| {
        for part in drawing(icon) {
            let mut rect = host.spawn(part_bundle(part, base));
            if let Some(node) = livery_node(part, base, image.as_ref(), icon.livery) {
                rect.insert(node);
            }
        }
    });
}

/// One rectangle of a picture, as the nodes that draw it.
fn part_bundle(part: &IconPart, base: LinearRgba) -> impl Bundle {
    let base = if part.iron {
        LinearRgba::new(0.30, 0.35, 0.42, 1.0)
    } else if part.green {
        LinearRgba::new(0.16, 0.82, 0.28, 1.0)
    } else {
        base
    };
    (
        IconRect,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(part.left),
            top: Val::Percent(part.top),
            width: Val::Percent(part.width),
            height: Val::Percent(part.height),
            border_radius: BorderRadius::all(Val::Percent(part.radius)),
            ..default()
        },
        BackgroundColor(shaded(base, part.shade)),
        UiTransform::from_rotation(Rot2::radians(part.rotation)),
        FocusPolicy::Pass,
    )
}

/// The image node one rectangle draws its livery through, when it draws one.
///
/// **The livery multiplies the rectangle's own colour**, exactly as it multiplies an item's
/// colour on a mesh: `ImageNode::color` is a tint over the sampled texel, so
/// `player/items.rs` stays the one answer to what the steel is here too. `rect` selects this
/// material's own band — see [`field_rect`] — because an `ImageNode` with no rectangle draws
/// the whole image, the neutral row and every other material's band included.
fn livery_node(
    part: &IconPart,
    base: LinearRgba,
    image: Option<&Handle<Image>>,
    livery: Option<Livery>,
) -> Option<ImageNode> {
    if !part.livery {
        return None;
    }
    Some(ImageNode {
        image: image?.clone(),
        color: shaded(base, part.shade),
        rect: Some(field_rect(livery?)),
        ..default()
    })
}

/// A pick: a diagonal haft under a curved head. What a resource is worth going back for.
const PICK: [IconPart; 2] = [
    IconPart {
        left: 46.0,
        top: 16.0,
        width: 8.0,
        height: 68.0,
        radius: 20.0,
        rotation: 0.70,
        shade: -0.35,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 14.0,
        top: 22.0,
        width: 72.0,
        height: 12.0,
        radius: 30.0,
        rotation: -0.35,
        shade: 0.30,
        ..IconPart::PLAIN
    },
];

/// An arch: rock with a mouth cut out of it. The dark part is the cave.
const ARCH: [IconPart; 2] = [
    IconPart {
        left: 20.0,
        top: 30.0,
        width: 60.0,
        height: 55.0,
        radius: 45.0,
        shade: 0.25,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 32.0,
        top: 44.0,
        width: 36.0,
        height: 41.0,
        radius: 45.0,
        shade: -0.75,
        ..IconPart::PLAIN
    },
];

/// A fang: a brow with two tapering teeth under it.
const FANG: [IconPart; 3] = [
    IconPart {
        left: 24.0,
        top: 20.0,
        width: 52.0,
        height: 14.0,
        radius: 8.0,
        shade: -0.30,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 30.0,
        top: 30.0,
        width: 12.0,
        height: 46.0,
        radius: 40.0,
        rotation: 0.18,
        shade: 0.35,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 58.0,
        top: 30.0,
        width: 12.0,
        height: 46.0,
        radius: 40.0,
        rotation: -0.18,
        shade: 0.35,
        ..IconPart::PLAIN
    },
];

/// A crown: a band with three points, the middle one tallest and brightest.
const CROWN: [IconPart; 4] = [
    IconPart {
        left: 20.0,
        top: 58.0,
        width: 60.0,
        height: 20.0,
        radius: 6.0,
        shade: -0.15,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 20.0,
        top: 26.0,
        width: 14.0,
        height: 36.0,
        radius: 6.0,
        shade: 0.25,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 43.0,
        top: 18.0,
        width: 14.0,
        height: 44.0,
        radius: 6.0,
        shade: 0.45,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 66.0,
        top: 26.0,
        width: 14.0,
        height: 36.0,
        radius: 6.0,
        shade: 0.25,
        ..IconPart::PLAIN
    },
];

/// A tent: two leaning panels on a strip of trodden ground.
const TENT: [IconPart; 3] = [
    IconPart {
        left: 24.0,
        top: 26.0,
        width: 20.0,
        height: 58.0,
        radius: 4.0,
        rotation: 0.30,
        shade: 0.28,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 56.0,
        top: 26.0,
        width: 20.0,
        height: 58.0,
        radius: 4.0,
        rotation: -0.30,
        shade: -0.25,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 16.0,
        top: 76.0,
        width: 68.0,
        height: 9.0,
        radius: 4.0,
        shade: -0.45,
        ..IconPart::PLAIN
    },
];

/// A roof over a wall: the smallest thing that reads as somewhere people live.
const ROOF: [IconPart; 3] = [
    IconPart {
        left: 20.0,
        top: 26.0,
        width: 34.0,
        height: 12.0,
        radius: 4.0,
        rotation: 0.55,
        shade: 0.35,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 46.0,
        top: 26.0,
        width: 34.0,
        height: 12.0,
        radius: 4.0,
        rotation: -0.55,
        shade: 0.10,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 28.0,
        top: 50.0,
        width: 44.0,
        height: 34.0,
        radius: 3.0,
        shade: -0.28,
        ..IconPart::PLAIN
    },
];

/// A flag: a pole and a banner. The mark that is only its note.
const FLAG: [IconPart; 2] = [
    IconPart {
        left: 30.0,
        top: 14.0,
        width: 8.0,
        height: 72.0,
        radius: 20.0,
        shade: -0.40,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 38.0,
        top: 20.0,
        width: 40.0,
        height: 28.0,
        radius: 3.0,
        shade: 0.30,
        ..IconPart::PLAIN
    },
];

/// The rectangles one kind of mark is drawn from, in the order they are stacked.
///
/// **Exhaustive, with no wildcard arm**, for the reason [`parts`] is: an eighth
/// [`MarkerKind`] does not compile until somebody has drawn it, rather than falling through
/// into whichever picture the wildcard happened to name.
fn marker_parts(kind: MarkerKind) -> &'static [IconPart] {
    match kind {
        MarkerKind::Resource => &PICK,
        MarkerKind::Cave => &ARCH,
        MarkerKind::Monster => &FANG,
        MarkerKind::Boss => &CROWN,
        MarkerKind::Camp => &TENT,
        MarkerKind::Village => &ROOF,
        MarkerKind::Note => &FLAG,
    }
}

/// What one kind of mark is drawn in.
///
/// **A constant per kind, not a colour anybody chooses.** Seven silhouettes at twenty-odd
/// pixels are not seven things a player can tell apart at a glance, and the colour is what
/// makes the row of kind buttons readable as well. It decides nothing: the kind is the
/// server's, and this is the paint over it.
fn marker_colour(kind: MarkerKind) -> Color {
    match kind {
        MarkerKind::Resource => Color::srgb(0.85, 0.62, 0.25),
        MarkerKind::Cave => Color::srgb(0.55, 0.58, 0.66),
        MarkerKind::Monster => Color::srgb(0.80, 0.28, 0.26),
        MarkerKind::Boss => Color::srgb(0.95, 0.78, 0.30),
        MarkerKind::Camp => Color::srgb(0.72, 0.55, 0.34),
        MarkerKind::Village => Color::srgb(0.42, 0.68, 0.40),
        MarkerKind::Note => Color::srgb(0.62, 0.74, 0.92),
    }
}

/// Draws one mark's picture under `host`.
///
/// The same rectangles a cell gets and the same [`part_bundle`], so a mark on the map and a
/// stack in the pack are drawn by one renderer. No livery: a livery is a material's surface
/// and a mark is not made of anything.
pub(crate) fn spawn_marker(host: &mut ChildSpawnerCommands<'_>, kind: MarkerKind) {
    let base = marker_colour(kind).to_linear();
    for part in marker_parts(kind) {
        host.spawn(part_bundle(part, base));
    }
}

/// One rectangle of a drawn icon. Carried so a test can count what a cell drew without
/// walking the hierarchy looking for anything with a background.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IconRect;

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether one shape's answer is a drawing rather than a placeholder standing in for
    /// one.
    ///
    /// The predicate the sweep applies, extracted so `the_sweep_rejects_a_shape_that_is_not_drawn`
    /// can assert the failure mode on fixtures instead of the sweep only ever running over
    /// answers that already pass. Three ways to not be a drawing: nothing at all, a part
    /// with no area, and a part that has wandered outside the cell it is drawn in.
    fn is_a_drawing(parts: &[IconPart]) -> bool {
        !parts.is_empty()
            && parts.iter().all(|part| {
                part.width > 0.0
                    && part.height > 0.0
                    && part.left >= 0.0
                    && part.top >= 0.0
                    && part.left + part.width <= 100.0
                    && part.top + part.height <= 100.0
            })
    }

    /// The counterpart to the registry's name sweep: every shape in the vocabulary is
    /// drawn, and no two are drawn the same.
    ///
    /// The second half is what "a shape with none silently falls back to a square" would
    /// look like once the compiler has forced an arm to exist: an arm that answers with
    /// somebody else's picture.
    #[test]
    fn every_shape_has_a_drawing_of_its_own() {
        for shape in ItemShape::ALL {
            assert!(
                is_a_drawing(parts(shape)),
                "{shape:?} has no drawing: {:?}",
                parts(shape)
            );
        }

        for (index, shape) in ItemShape::ALL.iter().enumerate() {
            for other in &ItemShape::ALL[index + 1..] {
                assert_ne!(
                    parts(*shape),
                    parts(*other),
                    "{shape:?} and {other:?} draw the same picture"
                );
            }
        }
    }

    /// **Every sculpted armour piece has a drawing of its own** (#1130): no two pieces share a
    /// picture, and none borrows a shape's.
    #[test]
    fn every_sculpted_armour_piece_has_a_drawing_of_its_own() {
        let mut seen: Vec<&[IconPart]> = ItemShape::ALL.iter().map(|shape| parts(*shape)).collect();
        for style in ArmourStyle::ALL {
            for piece in ArmourPiece::ALL {
                let drawn = armour_parts(style, piece);
                assert!(
                    is_a_drawing(drawn),
                    "{style:?} {piece:?} has no drawing: {drawn:?}"
                );
                assert!(
                    !seen.contains(&drawn),
                    "{style:?} {piece:?} draws a picture something else already draws"
                );
                seen.push(drawn);
            }
        }
    }

    /// A cell draws the sculpted piece for exactly the items that are one, and every other
    /// item its shape's picture.
    #[test]
    fn a_cell_draws_the_sculpted_piece_an_item_is_and_otherwise_its_shape() {
        use crate::player::{item_shape, known_item_ids, sculpted_icon};

        let mut sculpted = 0;
        for item_id in known_item_ids() {
            let icon = StackIcon {
                shape: item_shape(item_id),
                colour: Color::WHITE,
                livery: None,
                armour: sculpted_icon(item_id),
            };
            match icon.armour {
                Some((style, piece)) => {
                    sculpted += 1;
                    assert_eq!(icon.shape, ItemShape::Armour, "item {item_id}");
                    assert_eq!(drawing(icon), armour_parts(style, piece), "item {item_id}");
                }
                None => assert_eq!(drawing(icon), parts(icon.shape), "item {item_id}"),
            }
        }
        assert_eq!(
            sculpted, 3,
            "the rusty helm, cuirass and greaves are the sculpted pieces"
        );
    }

    #[test]
    fn the_horse_head_keeps_its_profile_at_vendor_and_inventory_sizes() {
        let [neck, poll, face, muzzle, left_ear, right_ear] =
            <[IconPart; 6]>::try_from(parts(ItemShape::HorseHead))
                .expect("the horse head has a neck, poll, face, muzzle and two ears");

        assert!(left_ear.top < poll.top && right_ear.top < poll.top);
        assert!(face.height > muzzle.height && poll.width > muzzle.width);
        assert!(neck.top < muzzle.top + muzzle.height && neck.top + neck.height > face.top);
        assert!(
            parts(ItemShape::HorseHead)
                .iter()
                .all(|part| !part.iron && !part.green && !part.livery),
            "every horse-head plane must keep the registry's exact coat colour"
        );

        for pixels in [34.0_f32, 50.0] {
            for (name, part) in [("left ear", left_ear), ("right ear", right_ear)] {
                let narrowest = part.width.min(part.height) * pixels / 100.0;
                assert!(
                    narrowest >= 4.0,
                    "the {name} is only {narrowest}px across in a {pixels}px icon"
                );
            }
        }
    }

    /// The same sweep for the map's marks: every kind is drawn, no two the same, and no two
    /// wear the same colour either.
    ///
    /// The colour half has no counterpart above because an item's colour comes from the
    /// registry, where the sweep already lives. A mark's comes from this file, so this is
    /// the only place that can notice two kinds painted alike -- which at this size is the
    /// same failure as two kinds drawn alike.
    #[test]
    fn every_kind_of_mark_has_a_drawing_and_a_colour_of_its_own() {
        for kind in MarkerKind::ALL {
            assert!(
                is_a_drawing(marker_parts(kind)),
                "{kind:?} has no drawing: {:?}",
                marker_parts(kind)
            );
        }

        for (index, kind) in MarkerKind::ALL.iter().enumerate() {
            for other in &MarkerKind::ALL[index + 1..] {
                assert_ne!(
                    marker_parts(*kind),
                    marker_parts(*other),
                    "{kind:?} and {other:?} draw the same picture"
                );
                assert_ne!(
                    marker_colour(*kind).to_linear(),
                    marker_colour(*other).to_linear(),
                    "{kind:?} and {other:?} are painted the same"
                );
            }
        }
    }

    /// **The flat sword and the 3D one draw the same weapon** (#204).
    ///
    /// `player::hands` and `player::drops` build a gladius; this cell draws one. Nothing here
    /// can reach across to those constants — `player::hands` is private to `player` and
    /// `ui::icon` is private to `ui`, which is the module boundary working — so what this
    /// pins is the structure a reader can check against them by eye: three parts, one long
    /// one crossed by a shorter one, with the grip on the far side of the cross from the tip.
    ///
    /// It is the assertion that fails if somebody "simplifies" this drawing back to a bar,
    /// which is the direction the three renderers drifted apart in last time.
    #[test]
    fn the_flat_sword_draws_the_parts_the_held_one_does() {
        let [blade, guard, grip] = <[IconPart; 3]>::try_from(parts(ItemShape::Blade))
            .expect("the sword is drawn as a blade, a guard and a grip");

        // One angle for all three: the guard is perpendicular to the blade because both are
        // turned by the same quarter turn, not because a second number says so.
        for (name, part) in [("blade", blade), ("guard", guard), ("grip", grip)] {
            assert!(
                (part.rotation - QUARTER_TURN).abs() < f32::EPSILON,
                "the sword's {name} is turned by {} rather than the one quarter turn the \
                 drawing shares",
                part.rotation
            );
        }

        assert!(
            blade.height > guard.height * 4.0 && blade.height > grip.height * 2.0,
            "the sword's blade is {} long against a guard of {} and a grip of {}, so it does \
             not read as mostly edge",
            blade.height,
            guard.height,
            grip.height
        );
        assert!(
            guard.width > blade.width * 2.0,
            "the sword's guard is {} across against a blade of {}, so it is a collar rather \
             than a cross guard",
            guard.width,
            blade.width
        );
        assert!(
            grip.width < guard.width && grip.height < blade.height,
            "the sword's grip is not the smallest part of it: {}x{} against a guard {} across \
             and a blade {} long",
            grip.width,
            grip.height,
            guard.width,
            blade.height
        );

        // Down the cell is toward the hilt, and the parts arrive in that order: point, then
        // guard, then grip. A grip drawn on the tip side of the guard would be a sword held
        // by its blade, and every assertion above would still pass.
        let along = |part: IconPart| part.top + part.height / 2.0;
        assert!(
            along(blade) < along(guard) && along(guard) < along(grip),
            "the sword's parts run blade {}, guard {}, grip {} down the cell, so the hand is \
             not at the hilt end of it",
            along(blade),
            along(guard),
            along(grip)
        );
    }

    /// **The flat pickaxe and shovel draw what the held ones are made of** (#1121): a haft in
    /// the item's own colour, which the registry says is wood, under a head drawn entirely in
    /// iron — and each head says which implement it is.
    ///
    /// Like the sword's test above, nothing here can reach the mesh constants across the
    /// module boundary, so what it pins is the structure a reader can check by eye: the
    /// pick's head is wider than anything else in its drawing and sits over the haft's top;
    /// the shovel's blade is its broadest part, above the haft, with the grip below it.
    #[test]
    fn the_flat_pickaxe_and_shovel_are_wooden_hafts_under_iron_heads() {
        let centre_y = |part: IconPart| part.top + part.height / 2.0;

        let [haft, left_arm, right_arm, eye] = <[IconPart; 4]>::try_from(parts(ItemShape::Pickaxe))
            .expect("the pickaxe is drawn as a haft, two arms and an eye");
        assert!(
            !haft.iron,
            "the pickaxe's haft is drawn in iron rather than wood"
        );
        for (name, part) in [
            ("left arm", left_arm),
            ("right arm", right_arm),
            ("eye", eye),
        ] {
            assert!(part.iron, "the pickaxe's {name} is not iron");
            assert!(
                centre_y(part) < haft.top + haft.height / 4.0,
                "the pickaxe's {name} is not across the top of the haft"
            );
        }
        // Two points, falling away on either side: the arms turn in opposite directions and
        // together span more of the cell than the haft is long.
        assert!(left_arm.rotation < 0.0 && right_arm.rotation > 0.0);
        let head_span = right_arm.left + right_arm.width - left_arm.left;
        assert!(
            head_span > haft.height,
            "the pickaxe's head spans {head_span}, no wider than its {} haft",
            haft.height
        );

        let [haft, grip, blade, socket] = <[IconPart; 4]>::try_from(parts(ItemShape::Shovel))
            .expect("the shovel is drawn as a haft, a grip, a blade and a socket");
        assert!(
            !haft.iron && !grip.iron,
            "the shovel's wood is drawn in iron"
        );
        assert!(blade.iron && socket.iron, "the shovel's head is not iron");
        assert!(
            blade.width > grip.width && blade.width > socket.width && blade.width > haft.width,
            "the shovel's blade is not its broadest part"
        );
        assert!(
            centre_y(blade) < centre_y(haft) && centre_y(haft) < centre_y(grip),
            "the shovel does not run blade, haft, grip down the cell"
        );
        assert!(
            blade.rotation == 0.0,
            "the shovel's blade is turned, so it no longer reads as flat"
        );
    }

    /// The sweep's teeth, on fixtures rather than on the drawings that already pass.
    #[test]
    fn the_sweep_rejects_a_shape_that_is_not_drawn() {
        assert!(!is_a_drawing(&[]), "an empty drawing passed the sweep");
        assert!(
            !is_a_drawing(&[IconPart {
                width: 40.0,
                ..IconPart::PLAIN
            }]),
            "a part with no height passed the sweep"
        );
        assert!(
            !is_a_drawing(&[IconPart {
                left: 80.0,
                top: 10.0,
                width: 40.0,
                height: 40.0,
                ..IconPart::PLAIN
            }]),
            "a part hanging off the side of the cell passed the sweep"
        );
        assert!(
            is_a_drawing(&[IconPart {
                left: 10.0,
                top: 10.0,
                width: 80.0,
                height: 80.0,
                ..IconPart::PLAIN
            }]),
            "an honest one-rectangle drawing failed the sweep"
        );
    }

    /// The property a multiply does not have: three faces stay three faces at both ends of
    /// the brightness range.
    #[test]
    fn shading_separates_a_dark_item_from_itself_and_a_bright_one_too() {
        for base in [
            LinearRgba::new(0.030, 0.034, 0.042, 1.0),
            LinearRgba::new(0.888, 0.913, 0.930, 1.0),
        ] {
            let lit = shaded(base, 0.30).to_linear();
            let plain = shaded(base, 0.0).to_linear();
            let turned = shaded(base, -0.42).to_linear();
            assert!(
                lit.red - plain.red > 0.02,
                "the lit face did not separate from the base: {lit:?} vs {plain:?}"
            );
            assert!(
                plain.red - turned.red > 0.005,
                "the turned face did not separate from the base: {plain:?} vs {turned:?}"
            );
            // The item's own colour is what the unshaded part draws, which is what keeps
            // the swatch a cell shows the swatch the registry named.
            assert_eq!(plain, base);
        }
    }

    /// Alpha is the item's, never the shade's: a mix that touched it would fade an icon in
    /// and out with its own lighting.
    #[test]
    fn shading_leaves_the_alpha_channel_alone() {
        let base = LinearRgba::new(0.2, 0.3, 0.4, 0.5);
        for shade in [-1.0, -0.42, 0.0, 0.3, 1.0] {
            assert!(
                (shaded(base, shade).to_linear().alpha - 0.5).abs() < f32::EPSILON,
                "shade {shade} moved the alpha channel"
            );
        }
    }
}
