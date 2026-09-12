//! Sculpted armour: which silhouette a worn item is drawn from, and how one is built.
//!
//! **A style is a row fact, and this module is its dispatch.** An armour item names the
//! style it is sculpted in through the registry's `armour_style` column in
//! [`super::items`], and [`ArmourStyle::parts`] is the one wildcard-free `match` that turns a
//! style and a segment into geometry. Nothing here compares an item id: a second set is a
//! variant, an arm and a module beside [`rusty`], and an armour item whose row names no
//! style is drawn as the plain overlay cuboid it always was.
//!
//! **Every style is cut inside the cell the cuboid filled.** [`placed_armour`] decides the
//! box one segment occupies, and every vertex a style emits stays inside it, so the pivots,
//! the walk cycle and the body envelope are the ones the cuboid had. What a style changes is
//! how that box is *spent*: plates, ridges and bands at the surface, and a darker core
//! showing through the gaps between them.
//!
//! # The primitive
//!
//! One kind of solid, [`Part`]: an octagonal section lofted through a list of [`Ring`]s. A
//! ring is a height, an inset from the part's outline and a corner chamfer, so the same
//! primitive is a plain box (two rings, no inset, no chamfer), a chamfered plate, a flared
//! cuff or a domed crown. Each part is emitted flat-shaded with its own normals, texture
//! coordinates and a per-vertex [`Tone`], and the parts of one segment are merged into the one
//! mesh the body shares.
//!
//! **Authored in the model sheet's notches and axes** — `+z` is forwards, as it is in
//! [`super::appearance`] — and converted to Bevy's space in exactly one place,
//! [`sheet_to_body`], so the tables in a style module read against the rig they wrap.
//!
//! # The surface
//!
//! A style's parts wear the item's livery through real texture coordinates: `along` runs up
//! the segment's cell and `across` over whichever side of it a face looks out of, both inside
//! the livery's own band. A segment for an item with no livery points every vertex at the
//! neutral texel instead. The recesses are darker by a vertex colour, which multiplies the
//! material's colour and its livery alike.

mod leather;
mod rusty;

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use super::appearance::{ArmourPiece, ArmourSegment, NOTCH_XZ, NOTCH_Y, PlacedBox, placed_armour};
use super::inventory::{EQUIPMENT_ROUTES, equipment_offset};
use super::items::{ITEMS, Livery, armour_styles, item_armour_style, item_livery};
use super::{livery, merge_all};

/// The sculpted set one armour item is drawn as.
///
/// **A vocabulary of sets, not of items**: the three pieces of one set share a variant, and
/// which segment each covers is the equipment slot's answer, never this enum's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ArmourStyle {
    /// Old plate, worn: a knight's helm, a cuirass with pauldrons, segmented vambraces and
    /// greaves with knee cops. See [`rusty`].
    Rusty,
    /// Worked hide: a stitched cap with a brim, a laced and strapped jerkin, strapped bracers
    /// and leggings with a padded knee. See [`leather`].
    Leather,
}

impl ArmourStyle {
    /// Every style, for the sweeps. Hand-written for the reason `ItemShape::ALL` is.
    #[cfg(test)]
    pub(crate) const ALL: [Self; 2] = [Self::Rusty, Self::Leather];

    /// The parts one segment of this style is cut from.
    ///
    /// **Wildcard-free**, so a new style does not compile until every segment has geometry.
    fn parts(self, segment: ArmourSegment) -> &'static [Part] {
        match self {
            Self::Rusty => rusty::parts(segment),
            Self::Leather => leather::parts(segment),
        }
    }

    /// Where one segment of this style deliberately shows the body under it.
    ///
    /// **A declaration to the containment test, and nothing a mesh reads.** Every other point
    /// of a covered body piece has to lie inside some part; a point inside an opening does not,
    /// and `no_part_closes_a_declared_opening` holds the style to leaving it open. A closed helm
    /// declares none; a cap with a brim declares the face.
    #[cfg(test)]
    fn openings(self, segment: ArmourSegment) -> &'static [Opening] {
        match self {
            Self::Rusty => &[],
            Self::Leather => leather::openings(segment),
        }
    }

    /// Whether this style's helm closes over the hair.
    ///
    /// **The hair is taller than the helmet's cell**, and the cell is not this module's to
    /// grow: every hair model's cap reaches half a notch above the top of the head cell's
    /// second wrapping tier, and the topknot three and a half more. A closed helm drawn inside
    /// its cell over visible hair would wear a hair-coloured lid, so a style that closes says
    /// so and the body hides its hair while the helm is worn.
    pub(super) const fn hides_hair(self) -> bool {
        match self {
            Self::Rusty => rusty::HIDES_HAIR,
            Self::Leather => leather::HIDES_HAIR,
        }
    }
}

/// A box of a segment's cell, in the sheet's notches, where a style leaves the body showing.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Opening {
    pub(super) x: (f32, f32),
    pub(super) y: (f32, f32),
    pub(super) z: (f32, f32),
}

#[cfg(test)]
impl Opening {
    /// Whether a point in the sheet's notches is inside this opening.
    pub(super) fn holds(self, point: Vec3) -> bool {
        const SLACK: f32 = 1e-3;
        let inside =
            |value: f32, (low, high): (f32, f32)| value >= low - SLACK && value <= high + SLACK;
        inside(point.x, self.x) && inside(point.y, self.y) && inside(point.z, self.z)
    }
}

/// Everything about an item that decides which sculpted mesh it wears.
///
/// **The livery belongs in it**, because it is written into the texture coordinates: two
/// items in one style and two different metals are two meshes, and two items sharing both
/// halves share one — the same widening `drops::MeshKey` made for a pitted blade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct ArmourLook {
    pub(super) style: ArmourStyle,
    pub(super) livery: Option<Livery>,
}

/// The sculpted look one item id is drawn with, or `None` for the plain overlay cuboid.
///
/// `None` for an unknown id and for every armour row that names no style, which is the
/// fallback the acceptance criterion asks for rather than a guess about a newer server's item.
pub(super) fn look(item_id: u16) -> Option<ArmourLook> {
    item_armour_style(item_id).map(|style| ArmourLook {
        style,
        livery: item_livery(item_id),
    })
}

/// Every distinct look an item in this build is drawn with, for the shared mesh cache.
///
/// Derived from the registry rather than the cross product of styles and liveries, so the
/// cache holds exactly what can be worn.
pub(super) fn looks() -> Vec<ArmourLook> {
    armour_styles()
        .into_iter()
        .map(|(style, livery)| ArmourLook { style, livery })
        .collect()
}

/// The piece of the rig one armour item covers, read off the equipment slot it routes to.
///
/// **Not a second table**: `inventory::EQUIPMENT_ROUTES` already answers which slot an item
/// fits, and `inventory::equipment_offset` — defined beside that table, so the two cannot drift
/// apart — names which of its routes each piece is. The off-hand is no piece.
pub(super) fn piece_of(item_id: u16) -> Option<ArmourPiece> {
    ArmourPiece::ALL.into_iter().find(|piece| {
        EQUIPMENT_ROUTES
            .get(equipment_offset(*piece))
            .is_some_and(|accepted| accepted.contains(&item_id))
    })
}

/// One sculpted armour item as an object of its own: the look it is worn in and the piece it
/// covers. `None` for anything the body would draw as the plain overlay.
pub(super) fn sculpted_piece(item_id: u16) -> Option<(ArmourLook, ArmourPiece)> {
    Some((look(item_id)?, piece_of(item_id)?))
}

/// What a cell draws a sculpted armour item as: its set and its piece.
///
/// The livery is not in it, because a cell's picture does not change with the metal.
pub(crate) fn sculpted_icon(item_id: u16) -> Option<(ArmourStyle, ArmourPiece)> {
    sculpted_piece(item_id).map(|(look, piece)| (look.style, piece))
}

/// Every sculpted piece an item in this build is, for the drop's shared mesh cache.
pub(super) fn sculpted_pieces() -> Vec<(ArmourLook, ArmourPiece)> {
    let mut found: Vec<(ArmourLook, ArmourPiece)> = Vec::new();
    for row in ITEMS {
        if let Some(piece) = sculpted_piece(row.item_id)
            && !found.contains(&piece)
        {
            found.push(piece);
        }
    }
    found
}

/// A sculpted piece taken off the rig: the segments it is worn as, merged in their resting
/// places, centred on their own origin and scaled so the longest side is `longest` blocks.
///
/// **The same meshes the body wears, not a second drawing of them**, which is what keeps a
/// dropped helm and a worn one from becoming two objects that drift apart. A cuirass is its
/// torso and both vambraces; greaves are both legs.
pub(super) fn piece_mesh(look: ArmourLook, piece: ArmourPiece, longest: f32) -> Mesh {
    let mut segments = ArmourSegment::ALL
        .into_iter()
        .filter(|segment| segment.piece() == piece)
        .map(|segment| {
            segment_mesh(Some(look), segment).translated_by(segment.body_piece().pivot())
        });
    // Unreachable: every piece is covered by at least one segment.
    let Some(mut merged) = segments.next() else {
        return Mesh::from(Cuboid::from_length(longest));
    };
    merge_all(&mut merged, segments, "dropped sculpted armour");

    let Some(bevy::mesh::VertexAttributeValues::Float32x3(positions)) =
        merged.attribute(Mesh::ATTRIBUTE_POSITION)
    else {
        return merged;
    };
    let (low, high) = positions
        .iter()
        .fold((Vec3::MAX, Vec3::MIN), |(low, high), position| {
            let position = Vec3::from_array(*position);
            (low.min(position), high.max(position))
        });
    let size = (high - low).max_element().max(f32::EPSILON);
    merged
        .translated_by(-(low + high) / 2.0)
        .scaled_by(Vec3::splat(longest / size))
}

/// How dark a recess is drawn, as a multiplier of the material's colour.
///
/// **Dark enough to read as a gap at the distance a body is hardest to read**, and not black:
/// the core of a plate is the same metal in shadow, so it keeps the livery's rust under it.
const RECESS_SHADE: f32 = 0.55;

/// How dark a strap, a lace or a line of stitching is drawn over the hide it is laid on.
///
/// **Darker leather, not a shadow**: lighter than a recess, because a strap stands proud of the
/// hide and catches the same light, and dark enough to read as a second leather at the
/// distance a body is hardest to read.
const STRAP_SHADE: f32 = 0.62;

/// Which surface one part is: the plate or hide a player sees, the core showing through a gap,
/// or darker leather laid over the hide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tone {
    /// The outer surface: a plate of the rusty set, the hide of the leather set.
    Plate,
    /// The core of a plated segment, in shadow between its plates.
    Recess,
    /// A strap, lace, tie or seam of the leather set, standing proud of the hide.
    Strap,
}

impl Tone {
    const fn shade(self) -> f32 {
        match self {
            Self::Plate => 1.0,
            Self::Recess => RECESS_SHADE,
            Self::Strap => STRAP_SHADE,
        }
    }
}

/// One horizontal section of a lofted part, in notches.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Ring {
    /// Height above the feet.
    pub(super) y: f32,
    /// How far this section stands in from the part's outline on every side.
    pub(super) inset: f32,
    /// How much each vertical corner of this section is cut, measured along both sides.
    pub(super) chamfer: f32,
}

/// One ring, in the order a style's table reads: height, inset, chamfer.
pub(super) const fn ring(y: f32, inset: f32, chamfer: f32) -> Ring {
    Ring { y, inset, chamfer }
}

/// The rings one part is lofted through, lowest first.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Profile {
    /// A plain box between two heights.
    Block { y: (f32, f32) },
    /// A section lofted through every ring in order.
    Loft(&'static [Ring]),
}

/// One solid of a sculpted segment, in the model sheet's notches and axes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Part {
    /// Low and high bound to the character's right.
    pub(super) x: (f32, f32),
    /// Low and high bound along the way they face. **Positive is forwards.**
    pub(super) z: (f32, f32),
    pub(super) profile: Profile,
    pub(super) tone: Tone,
}

impl Part {
    /// A plain box.
    pub(super) const fn block(x: (f32, f32), y: (f32, f32), z: (f32, f32), tone: Tone) -> Self {
        Self {
            x,
            z,
            profile: Profile::Block { y },
            tone,
        }
    }

    /// A section lofted through `rings`, lowest first.
    pub(super) const fn loft(
        x: (f32, f32),
        z: (f32, f32),
        rings: &'static [Ring],
        tone: Tone,
    ) -> Self {
        Self {
            x,
            z,
            profile: Profile::Loft(rings),
            tone,
        }
    }

    /// The same part on the other side of the body.
    ///
    /// The rig is mirrored across `x = 0` — the right sleeve, trouser and fist are the left
    /// ones negated — so a style authors one side and mirrors it rather than keeping two
    /// tables that could disagree.
    pub(super) const fn mirrored(self) -> Self {
        Self {
            x: (-self.x.1, -self.x.0),
            ..self
        }
    }

    fn rings(self) -> Vec<Ring> {
        match self.profile {
            Profile::Block { y } => vec![
                Ring {
                    y: y.0,
                    inset: 0.0,
                    chamfer: 0.0,
                },
                Ring {
                    y: y.1,
                    inset: 0.0,
                    chamfer: 0.0,
                },
            ],
            Profile::Loft(rings) => rings.to_vec(),
        }
    }
}

/// Every part of one table, mirrored across the body.
pub(super) const fn mirrored<const N: usize>(parts: [Part; N]) -> [Part; N] {
    let mut out = parts;
    let mut index = 0;
    while index < N {
        out[index] = parts[index].mirrored();
        index += 1;
    }
    out
}

/// A point in the model sheet's notches, as Bevy's feet-relative blocks.
///
/// **The one place this module applies the sheet's sign**, for the reason
/// `appearance::placed` is the one place the rig does: the sheet measures forwards as `+z`
/// and a body faces `-Z`.
fn sheet_to_body(x: f32, y: f32, z: f32) -> Vec3 {
    Vec3::new(x * NOTCH_XZ, y * NOTCH_Y, -z * NOTCH_XZ)
}

/// One worn segment's mesh, authored around the pivot of the body piece underneath it.
///
/// `None` is the plain overlay: the inflated cell as one cuboid, exactly what every armour
/// item drew before a style existed and what an item without one still draws.
pub(super) fn segment_mesh(look: Option<ArmourLook>, segment: ArmourSegment) -> Mesh {
    let cell = placed_armour(segment.piece(), segment.cell());
    let pivot = segment.body_piece().pivot();
    let cuboid = || Mesh::from(Cuboid::from_size(cell.size)).translated_by(cell.centre - pivot);
    let Some(look) = look else {
        return cuboid();
    };

    let mut parts = look
        .style
        .parts(segment)
        .iter()
        .map(|part| part_mesh(*part, cell, pivot, look.livery));
    // Unreachable: every style's table is non-empty, and
    // `every_sculpted_segment_stays_inside_its_inflated_cell` builds all of them. The cuboid
    // is the cosmetic direction to fail in.
    let Some(mut merged) = parts.next() else {
        error!("{:?} {segment:?} is cut from no parts at all", look.style);
        return cuboid();
    };
    merge_all(&mut merged, parts, "sculpted armour");
    merged
}

/// One part as a flat-shaded solid, relative to `pivot`.
fn part_mesh(part: Part, cell: PlacedBox, pivot: Vec3, livery: Option<Livery>) -> Mesh {
    let rings = part.rings();
    let centre = ((part.x.0 + part.x.1) / 2.0, (part.z.0 + part.z.1) / 2.0);
    let outline = |ring: &Ring| -> [Vec3; 8] {
        let (x0, x1) = (part.x.0 + ring.inset, part.x.1 - ring.inset);
        let (z0, z1) = (part.z.0 + ring.inset, part.z.1 - ring.inset);
        let cut = ring.chamfer;
        [
            (x0 + cut, z0),
            (x1 - cut, z0),
            (x1, z0 + cut),
            (x1, z1 - cut),
            (x1 - cut, z1),
            (x0 + cut, z1),
            (x0, z1 - cut),
            (x0, z0 + cut),
        ]
        .map(|(x, z)| sheet_to_body(x, ring.y, z))
    };
    let sections: Vec<[Vec3; 8]> = rings.iter().map(outline).collect();

    // Every triangle with the way it has to face. The winding is settled against that normal
    // on the way out, so no arithmetic here has to reason about the sheet's flipped axis.
    let mut triangles: Vec<([Vec3; 3], Vec3)> = Vec::new();
    for pair in sections.windows(2) {
        let (low, high) = (pair[0], pair[1]);
        for corner in 0..8 {
            let next = (corner + 1) % 8;
            let quad = [low[corner], low[next], high[next], high[corner]];
            let normal = (quad[1] - quad[0]).cross(quad[2] - quad[0])
                + (quad[2] - quad[0]).cross(quad[3] - quad[0]);
            let Some(mut normal) = normal.try_normalize() else {
                continue;
            };
            let middle = (quad[0] + quad[1] + quad[2] + quad[3]) / 4.0;
            let axis = sheet_to_body(centre.0, 0.0, centre.1);
            let outward = Vec3::new(middle.x - axis.x, 0.0, middle.z - axis.z);
            if normal.dot(outward) < 0.0 {
                normal = -normal;
            }
            triangles.push(([quad[0], quad[1], quad[2]], normal));
            triangles.push(([quad[0], quad[2], quad[3]], normal));
        }
    }
    for (section, normal) in [(sections.first(), Vec3::NEG_Y), (sections.last(), Vec3::Y)] {
        let Some(section) = section else {
            continue;
        };
        let middle = section.iter().copied().sum::<Vec3>() / 8.0;
        for corner in 0..8 {
            triangles.push(([middle, section[corner], section[(corner + 1) % 8]], normal));
        }
    }

    let shade = part.tone.shade();
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    for (corners, normal) in triangles {
        let facing = (corners[1] - corners[0]).cross(corners[2] - corners[0]);
        // A chamfer of zero repeats a corner, which leaves triangles with no area; they
        // would draw nothing and only cost vertices.
        if facing.length_squared() < 1e-14 {
            continue;
        }
        let ordered = if facing.dot(normal) < 0.0 {
            [corners[0], corners[2], corners[1]]
        } else {
            corners
        };
        for point in ordered {
            positions.push((point - pivot).to_array());
            normals.push(normal.to_array());
            uvs.push(surface_uv(livery, point, normal, cell));
        }
    }
    let count = positions.len();
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_attribute(
        Mesh::ATTRIBUTE_COLOR,
        vec![[shade, shade, shade, 1.0]; count],
    )
    .with_inserted_indices(Indices::U32((0..count as u32).collect()))
}

/// The texture coordinate one vertex of a sculpted segment carries.
///
/// `along` runs up the segment's cell and `across` over the side of the cell the face looks
/// out of — the depth for a face turned sideways, the width otherwise — so the field reads
/// continuously over each face and a face never samples across the whole image the way a
/// seam of a wrapped coordinate would. Both land inside the livery's own band through
/// [`livery::blade_uv`], which is what keeps a plate from reading another metal's rows.
fn surface_uv(livery: Option<Livery>, point: Vec3, normal: Vec3, cell: PlacedBox) -> [f32; 2] {
    let Some(livery) = livery else {
        return livery::neutral_uv();
    };
    let low = cell.centre - cell.size / 2.0;
    let along = ((point.y - low.y) / cell.size.y).clamp(0.0, 1.0);
    let across = if normal.x.abs() > normal.z.abs() {
        (point.z - low.z) / cell.size.z
    } else {
        (point.x - low.x) / cell.size.x
    };
    livery::blade_uv(livery, across.clamp(0.0, 1.0), along)
}

#[cfg(test)]
mod tests {
    use bevy::mesh::VertexAttributeValues;

    use super::super::appearance::{ArmourPiece, BodyPart, BodyPiece, boxes, piece_boxes, placed};
    use super::super::items::{ITEMS, ItemShape};
    use super::super::livery::band_holds;
    use super::*;
    use crate::net::HairModel;

    /// Every style with every livery it could be worn in, plus none.
    fn every_look() -> Vec<ArmourLook> {
        let mut all = Vec::new();
        for style in ArmourStyle::ALL {
            for livery in Livery::ALL.map(Some).into_iter().chain([None]) {
                all.push(ArmourLook { style, livery });
            }
        }
        all
    }

    fn positions(mesh: &Mesh) -> Vec<Vec3> {
        let Some(VertexAttributeValues::Float32x3(values)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("a segment carries positions");
        };
        values
            .iter()
            .map(|value| Vec3::from_array(*value))
            .collect()
    }

    /// The extents of one placed box, as (low, high) per axis.
    fn spans(box_: PlacedBox) -> [(f32, f32); 3] {
        let low = box_.centre - box_.size / 2.0;
        let high = box_.centre + box_.size / 2.0;
        [(low.x, high.x), (low.y, high.y), (low.z, high.z)]
    }

    /// One part's bounding box, in Bevy's feet-relative blocks.
    fn part_spans(part: Part) -> [(f32, f32); 3] {
        let rings = part.rings();
        let (y0, y1) = (rings[0].y, rings[rings.len() - 1].y);
        let low = sheet_to_body(part.x.0, y0, part.z.1);
        let high = sheet_to_body(part.x.1, y1, part.z.0);
        [(low.x, high.x), (low.y, high.y), (low.z, high.z)]
    }

    fn overlaps(a: (f32, f32), b: (f32, f32)) -> bool {
        a.0 < b.1 && b.0 < a.1
    }

    /// Whether a point in the sheet's notches is inside one part's solid.
    fn contains(part: Part, point: Vec3) -> bool {
        const SLACK: f32 = 1e-3;
        let rings = part.rings();
        let (first, last) = (rings[0], rings[rings.len() - 1]);
        if point.y < first.y - SLACK || point.y > last.y + SLACK {
            return false;
        }
        let y = point.y.clamp(first.y, last.y);
        let ring = rings
            .windows(2)
            .find(|pair| (pair[0].y..=pair[1].y).contains(&y))
            .map_or(first, |pair| {
                let t = (y - pair[0].y) / (pair[1].y - pair[0].y).max(f32::EPSILON);
                Ring {
                    y,
                    inset: pair[0].inset + (pair[1].inset - pair[0].inset) * t,
                    chamfer: pair[0].chamfer + (pair[1].chamfer - pair[0].chamfer) * t,
                }
            });
        let (x0, x1) = (part.x.0 + ring.inset, part.x.1 - ring.inset);
        let (z0, z1) = (part.z.0 + ring.inset, part.z.1 - ring.inset);
        let inside =
            |value: f32, low: f32, high: f32| value >= low - SLACK && value <= high + SLACK;
        if !inside(point.x, x0, x1) || !inside(point.z, z0, z1) {
            return false;
        }
        let across = (point.x - x0).min(x1 - point.x);
        let deep = (point.z - z0).min(z1 - point.z);
        across + deep >= ring.chamfer - SLACK
    }

    /// Samples of a box, in the sheet's notches: its corners, edge midpoints and face centres.
    fn samples(box_: PlacedBox) -> Vec<Vec3> {
        let low = box_.centre - box_.size / 2.0;
        let high = box_.centre + box_.size / 2.0;
        let mut points = Vec::new();
        for x in [low.x, box_.centre.x, high.x] {
            for y in [low.y, box_.centre.y, high.y] {
                for z in [low.z, box_.centre.z, high.z] {
                    points.push(Vec3::new(x / NOTCH_XZ, y / NOTCH_Y, -z / NOTCH_XZ));
                }
            }
        }
        points
    }

    /// The body pieces an armour segment is drawn over.
    fn covered(segment: ArmourSegment) -> Vec<BodyPiece> {
        match segment {
            ArmourSegment::Helmet => vec![BodyPiece::HeadAndNeck, BodyPiece::Eyes],
            other => vec![other.body_piece()],
        }
    }

    #[test]
    fn every_ring_list_climbs_and_stays_inside_its_outline() {
        for style in ArmourStyle::ALL {
            for segment in ArmourSegment::ALL {
                for part in style.parts(segment) {
                    let rings = part.rings();
                    assert!(rings.len() >= 2, "{style:?} {segment:?} has a flat part");
                    assert!(
                        rings.windows(2).all(|pair| pair[0].y < pair[1].y),
                        "{style:?} {segment:?} has rings out of order: {part:?}"
                    );
                    let narrowest = (part.x.1 - part.x.0).min(part.z.1 - part.z.0);
                    for ring in rings {
                        assert!(
                            ring.inset >= 0.0
                                && ring.chamfer >= 0.0
                                && 2.0 * (ring.inset + ring.chamfer) < narrowest + f32::EPSILON,
                            "{style:?} {segment:?} has a ring that turns inside out: {part:?}"
                        );
                    }
                }
            }
        }
    }

    /// **Every vertex stays inside the cell the cuboid filled**, which is what keeps the
    /// pivot, the walk cycle and the body envelope unchanged.
    #[test]
    fn every_sculpted_segment_stays_inside_its_inflated_cell() {
        for look in every_look() {
            for segment in ArmourSegment::ALL {
                let cell = spans(placed_armour(segment.piece(), segment.cell()));
                let pivot = segment.body_piece().pivot();
                let mesh = segment_mesh(Some(look), segment);
                let points = positions(&mesh);
                assert!(!points.is_empty(), "{look:?} {segment:?} drew nothing");
                for point in points {
                    let point = point + pivot;
                    for (axis, (low, high)) in cell.iter().enumerate() {
                        assert!(
                            point[axis] >= low - 1e-5 && point[axis] <= high + 1e-5,
                            "{look:?} {segment:?} leaves its cell on axis {axis} at {point}"
                        );
                    }
                }
            }
        }
    }

    /// **The body under a segment is inside it**, so nothing of the chest, a limb or the head
    /// shows through the plate at rest — and because the segment turns about the same pivot
    /// as the piece it wraps, not in the walk cycle either.
    ///
    /// Read against the parts rather than the triangles: every corner, edge midpoint and face
    /// centre of each covered box, clipped to the height the cell spans, lies in some solid —
    /// or in an opening the style declares, which `no_part_closes_a_declared_opening` holds it
    /// to leaving open.
    #[test]
    fn the_body_under_a_sculpted_segment_stays_inside_it() {
        for style in ArmourStyle::ALL {
            for segment in ArmourSegment::ALL {
                let cell = spans(placed_armour(segment.piece(), segment.cell()));
                for piece in covered(segment) {
                    for part_box in piece_boxes(piece, HairModel::Shaved) {
                        let mut box_ = placed(piece.part(), *part_box);
                        let [_, (low, high), _] = spans(box_);
                        let (low, high) = (low.max(cell[1].0), high.min(cell[1].1));
                        if low >= high {
                            continue;
                        }
                        box_.size.y = high - low;
                        box_.centre.y = (low + high) / 2.0;
                        for point in samples(box_) {
                            assert!(
                                style
                                    .parts(segment)
                                    .iter()
                                    .any(|part| contains(*part, point))
                                    || style
                                        .openings(segment)
                                        .iter()
                                        .any(|opening| opening.holds(point)),
                                "{style:?} {segment:?} leaves {piece:?} showing at {point}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// **Rule 2 of the rig, for sculpted plates**: no face of a part lands on the plane of a
    /// body face of another colour where the two overlap.
    ///
    /// Read off each part's bounding box, which can report a plane a chamfer has cut away and
    /// cannot miss one — the direction for a check like this to be weak in. Hair under a helm
    /// that hides it is not drawn and is skipped.
    #[test]
    fn no_sculpted_face_shares_a_plane_with_the_body() {
        for style in ArmourStyle::ALL {
            for model in HairModel::ALL {
                for segment in ArmourSegment::ALL {
                    for part in style.parts(segment) {
                        let overlay = part_spans(*part);
                        for body_part in BodyPart::IN_DRAWING_ORDER {
                            if body_part == BodyPart::Hair
                                && segment == ArmourSegment::Helmet
                                && style.hides_hair()
                            {
                                continue;
                            }
                            for cell in boxes(body_part, model) {
                                let under = spans(placed(body_part, *cell));
                                for axis in 0..3 {
                                    let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
                                    if !overlaps(overlay[u], under[u])
                                        || !overlaps(overlay[v], under[v])
                                    {
                                        continue;
                                    }
                                    for side in [overlay[axis].0, overlay[axis].1] {
                                        for face in [under[axis].0, under[axis].1] {
                                            assert!(
                                                (side - face).abs() > 1e-5,
                                                "{style:?} {segment:?} part {part:?} shares \
                                                 plane {side} on axis {axis} with {body_part:?} \
                                                 wearing {model:?}"
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Two parts of one segment in different tones never face the same way on one plane
    /// where they overlap — a plate and the recess beside it would flicker between two
    /// shades. Facing opposite ways on a shared plane is two solids meeting, and is allowed.
    #[test]
    fn no_two_tones_share_a_plane_inside_one_segment() {
        for style in ArmourStyle::ALL {
            for segment in ArmourSegment::ALL {
                let parts = style.parts(segment);
                for (index, one) in parts.iter().enumerate() {
                    for two in &parts[index + 1..] {
                        if one.tone == two.tone {
                            continue;
                        }
                        let (a, b) = (part_spans(*one), part_spans(*two));
                        for axis in 0..3 {
                            let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
                            if !overlaps(a[u], b[u]) || !overlaps(a[v], b[v]) {
                                continue;
                            }
                            assert!(
                                (a[axis].0 - b[axis].0).abs() > 1e-5
                                    && (a[axis].1 - b[axis].1).abs() > 1e-5,
                                "{style:?} {segment:?}: {one:?} and {two:?} share a face on \
                                 axis {axis}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// **Every face of every part points out of that part**, so back-face culling keeps the
    /// outside of every plate rather than drawing a segment inside out.
    ///
    /// Read per part against the part's own interior: a lofted part is convex, so each outward
    /// normal leans away from its centre. Checking the winding against the normal alone would
    /// prove nothing, because `part_mesh` derives the winding *from* that normal — an outward
    /// test flipped in `part_mesh` would turn every plate inside out and still agree with it.
    #[test]
    fn every_sculpted_face_points_out_of_its_part() {
        for look in every_look() {
            for segment in ArmourSegment::ALL {
                let cell = placed_armour(segment.piece(), segment.cell());
                let pivot = segment.body_piece().pivot();
                for part in look.style.parts(segment) {
                    let rings = part.rings();
                    let centre = sheet_to_body(
                        (part.x.0 + part.x.1) / 2.0,
                        (rings[0].y + rings[rings.len() - 1].y) / 2.0,
                        (part.z.0 + part.z.1) / 2.0,
                    ) - pivot;
                    let mesh = part_mesh(*part, cell, pivot, look.livery);
                    let points = positions(&mesh);
                    let Some(VertexAttributeValues::Float32x3(normals)) =
                        mesh.attribute(Mesh::ATTRIBUTE_NORMAL)
                    else {
                        panic!("a part carries normals");
                    };
                    assert!(
                        !points.is_empty(),
                        "{look:?} {segment:?} drew an empty part"
                    );
                    for (index, triangle) in points.chunks(3).enumerate() {
                        let normal = Vec3::from_array(normals[index * 3]);
                        let middle = (triangle[0] + triangle[1] + triangle[2]) / 3.0;
                        assert!(
                            normal.dot(middle - centre) > 0.0,
                            "{look:?} {segment:?} part {part:?} triangle {index} faces into \
                             its part"
                        );
                        let facing = (triangle[1] - triangle[0]).cross(triangle[2] - triangle[0]);
                        assert!(
                            facing.dot(normal) > 0.0,
                            "{look:?} {segment:?} triangle {index} is wound against its normal"
                        );
                    }
                }
            }
        }
    }

    /// Every sculpted segment shows its outer surface and something darker on it: the rusty
    /// set a plate and a recess, the leather set hide and a strap.
    #[test]
    fn every_segment_has_a_surface_and_a_darker_tone() {
        for darker in [Tone::Recess, Tone::Strap] {
            assert!(darker.shade() < Tone::Plate.shade(), "{darker:?}");
            assert!(
                darker.shade() > 0.0,
                "{darker:?} is a surface in shadow, not a hole"
            );
        }
        assert!(
            Tone::Recess.shade() < Tone::Strap.shade(),
            "a strap stands proud of the hide and catches more light than a recess"
        );
        for style in ArmourStyle::ALL {
            for segment in ArmourSegment::ALL {
                let parts = style.parts(segment);
                assert!(
                    parts.iter().any(|part| part.tone == Tone::Plate),
                    "{style:?} {segment:?} has no outer surface"
                );
                assert!(
                    parts.iter().any(|part| part.tone != Tone::Plate),
                    "{style:?} {segment:?} is one flat tone"
                );
            }
        }
        assert!(
            ArmourSegment::ALL.into_iter().all(|segment| {
                rusty::parts(segment)
                    .iter()
                    .all(|part| part.tone != Tone::Strap)
            }),
            "the rusty set is plate, and plate has no straps"
        );
    }

    /// **A declared opening is open**: no part of the segment reaches into it, and it lies in
    /// the segment's cell. Without this an opening would be a way to excuse any gap at all.
    #[test]
    fn no_part_closes_a_declared_opening() {
        let mut declared = 0;
        for style in ArmourStyle::ALL {
            for segment in ArmourSegment::ALL {
                let cell = spans(placed_armour(segment.piece(), segment.cell()));
                for opening in style.openings(segment) {
                    declared += 1;
                    let low = sheet_to_body(opening.x.0, opening.y.0, opening.z.1);
                    let high = sheet_to_body(opening.x.1, opening.y.1, opening.z.0);
                    for (axis, (cell_low, cell_high)) in cell.iter().enumerate() {
                        assert!(
                            low[axis] >= cell_low - 1e-5 && high[axis] <= cell_high + 1e-5,
                            "{style:?} {segment:?} declares {opening:?} outside its cell"
                        );
                    }
                    for part in style.parts(segment) {
                        let rings = part.rings();
                        let (y0, y1) = (rings[0].y, rings[rings.len() - 1].y);
                        assert!(
                            !(overlaps(part.x, opening.x)
                                && overlaps((y0, y1), opening.y)
                                && overlaps(part.z, opening.z)),
                            "{style:?} {segment:?} part {part:?} reaches into {opening:?}"
                        );
                    }
                }
            }
        }
        assert!(
            declared > 0,
            "no style declares an opening, so this checks nothing"
        );
    }

    /// A liveried segment samples only its own livery's band; an unliveried one only the
    /// neutral texel.
    #[test]
    fn a_sculpted_segment_samples_only_the_livery_it_wears() {
        for look in every_look() {
            for segment in ArmourSegment::ALL {
                let mesh = segment_mesh(Some(look), segment);
                let Some(VertexAttributeValues::Float32x2(uvs)) =
                    mesh.attribute(Mesh::ATTRIBUTE_UV_0)
                else {
                    panic!("a segment carries texture coordinates");
                };
                for uv in uvs {
                    match look.livery {
                        Some(livery) => assert!(
                            band_holds(livery, *uv),
                            "{look:?} {segment:?} reads outside its band at {uv:?}"
                        ),
                        None => assert_eq!(*uv, livery::neutral_uv()),
                    }
                }
            }
        }
    }

    /// **Every armour item with a style resolves a sculpted mesh, and every one without falls
    /// back to the cuboid** — swept over the registry, so a new armour row is covered by
    /// existing.
    #[test]
    fn every_armour_item_resolves_a_mesh_and_unstyled_ones_keep_the_cuboid() {
        let armour_rows: Vec<_> = ITEMS
            .iter()
            .filter(|row| row.shape == ItemShape::Armour)
            .collect();
        assert!(armour_rows.iter().any(|row| look(row.item_id).is_some()));
        for row in armour_rows {
            for segment in ArmourSegment::ALL {
                let cuboid = positions(&segment_mesh(None, segment));
                let drawn = positions(&segment_mesh(look(row.item_id), segment));
                match look(row.item_id) {
                    Some(found) => {
                        assert!(
                            looks().contains(&found),
                            "item {} is not in the shared cache",
                            row.item_id
                        );
                        assert_ne!(drawn, cuboid, "item {} is still a cuboid", row.item_id);
                    }
                    None => assert_eq!(drawn, cuboid, "item {} lost its cuboid", row.item_id),
                }
            }
        }
        assert_eq!(look(4242), None, "an unknown id wears no style");
    }

    /// **Every armour item covers the piece its equipment slot names**, asserted item by item
    /// against the real routing table — so a route inserted or reordered in
    /// `EQUIPMENT_ROUTES` fails here instead of silently handing a helm the chest's meshes.
    #[test]
    fn every_armour_item_covers_the_piece_its_equipment_slot_names() {
        use super::super::crafting::{
            ITEM_LEATHER_CAP, ITEM_LEATHER_JERKIN, ITEM_LEATHER_LEGGINGS, ITEM_RUSTY_CUIRASS,
            ITEM_RUSTY_GREAVES, ITEM_RUSTY_HELM, ITEM_WOODEN_SHIELD,
        };

        for (item_id, piece) in [
            (ITEM_LEATHER_CAP, ArmourPiece::Head),
            (ITEM_LEATHER_JERKIN, ArmourPiece::Chest),
            (ITEM_LEATHER_LEGGINGS, ArmourPiece::Legs),
            (ITEM_RUSTY_HELM, ArmourPiece::Head),
            (ITEM_RUSTY_CUIRASS, ArmourPiece::Chest),
            (ITEM_RUSTY_GREAVES, ArmourPiece::Legs),
        ] {
            assert_eq!(piece_of(item_id), Some(piece), "item {item_id}");
        }
        assert_eq!(
            piece_of(ITEM_WOODEN_SHIELD),
            None,
            "the off-hand is no piece of the rig"
        );

        // And no armour row routes to two pieces, which a lookup by `find` would hide.
        for row in ITEMS.iter().filter(|row| row.shape == ItemShape::Armour) {
            let pieces = ArmourPiece::ALL
                .into_iter()
                .filter(|piece| EQUIPMENT_ROUTES[equipment_offset(*piece)].contains(&row.item_id))
                .count();
            assert_eq!(pieces, 1, "item {} routes to {pieces} pieces", row.item_id);
        }
    }

    #[test]
    fn the_mirrored_segments_are_mirrors() {
        for style in ArmourStyle::ALL {
            for (left, right) in [
                (ArmourSegment::LeftSleeve, ArmourSegment::RightSleeve),
                (ArmourSegment::LeftGreave, ArmourSegment::RightGreave),
            ] {
                let mirrored: Vec<Part> = style
                    .parts(left)
                    .iter()
                    .map(|part| part.mirrored())
                    .collect();
                assert_eq!(mirrored, style.parts(right));
            }
            // The torso and the helm are their own mirror images.
            for segment in [ArmourSegment::Torso, ArmourSegment::Helmet] {
                let parts = style.parts(segment);
                for part in parts {
                    assert!(
                        parts.contains(&part.mirrored()),
                        "{style:?} {segment:?} is lopsided at {part:?}"
                    );
                }
            }
        }
    }

    /// **The reason a closed helm hides the hair** is a fact about the rig, pinned so the
    /// switch cannot outlive it: every hair model reaches above the helmet's cell.
    #[test]
    fn every_hair_model_reaches_above_the_helmets_cell() {
        let cell = spans(placed_armour(
            ArmourPiece::Head,
            ArmourSegment::Helmet.cell(),
        ));
        for model in HairModel::ALL {
            let top = boxes(BodyPart::Hair, model)
                .iter()
                .map(|box_| spans(placed(BodyPart::Hair, *box_))[1].1)
                .fold(f32::MIN, f32::max);
            assert!(top > cell[1].1, "{model:?} fits under the helm after all");
        }
    }
}
