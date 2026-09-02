//! The eight-corner solid: one primitive for every part of a body that is not a box.
//!
//! A `Cuboid` has six faces and eight corners in fixed places. This is the same thing with
//! the corners set free — a barrel that swells in the middle, a neck that tapers toward the
//! head, a muzzle that is a wedge, a hoof that is wider at the ground — one call each, and
//! each merging with the cuboids beside it because it carries exactly the attributes a
//! `Cuboid` mesh carries. The horse in [`super::horse`] is what it is for.
//!
//! **Flat normals, and nothing else.** Every face gets one outward normal computed from its
//! own corners and no vertex is shared between faces, so a tapered solid reads as faceted
//! like the `Cuboid` next to it rather than as a soft blob among hard boxes. That is the
//! decision the sword blade in [`super::hands`] takes, and it is load-bearing for the same
//! reason: a normal averaged across a crease puts a gradient exactly where the highlight
//! should break.
//!
//! **Six faces and eight corners are the whole kit.** No cylinders, no spheres, no general
//! polyhedron: what a body cut from boxes needs is boxes that are allowed to lean and
//! taper, and every rounder shape would bring smooth shading with it.

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

/// The six faces, each as four corner indices wound counter-clockwise seen from outside.
///
/// Every face reads bottom-left, bottom-right, top-right, top-left from where a viewer
/// outside the solid would stand, which is the order [`FACE_UVS`] is written in. The
/// bottom four corners were given counter-clockwise from *above*, so the bottom face alone
/// runs through them backwards.
const FACES: [[usize; 4]; 6] = [
    [0, 1, 5, 4], // front, +Z
    [1, 2, 6, 5], // right, +X
    [2, 3, 7, 6], // back, -Z
    [3, 0, 4, 7], // left, -X
    [4, 5, 6, 7], // top, +Y
    [3, 2, 1, 0], // bottom, -Y
];

/// One face's texture coordinates in the corner order above: the whole image, once.
const FACE_UVS: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];

/// A closed six-faced solid from eight arbitrary corners, flat-shaded, with UVs on every
/// face.
///
/// The corners are the bottom four counter-clockwise seen from above, then the top four in
/// the same order — so that at `±half` on every axis this is exactly a `Cuboid`:
///
/// ```text
///          7-------------6
///         /|            /|        +Y up
///        / |           / |        +X right
///       4-------------5  |        +Z toward the viewer
///       |  |          |  |
///       |  3----------|--2
///       | /           | /
///       |/            |/
///       0-------------1
///
///   0 (-x, -y, +z)   1 (+x, -y, +z)   2 (+x, -y, -z)   3 (-x, -y, -z)
///   4 (-x, +y, +z)   5 (+x, +y, +z)   6 (+x, +y, -z)   7 (-x, +y, -z)
/// ```
///
/// Each face is two triangles wound counter-clockwise seen from outside — the winding
/// `Cuboid` uses and the one the renderer culls against — and no vertex is shared between
/// faces. The normal comes from the face's diagonals rather than from one triangle's two
/// edges: a face lofted between two ends of different sizes is not exactly planar, and the
/// diagonals answer the normal both of its triangles are nearest to instead of the first
/// one's. A face with no area at all — two coincident corners collapse a face to a
/// triangle, which still has one; it takes a second pair to flatten one to a line — gets a
/// zero normal rather than a NaN, and draws nothing, which is what a zero-area face should
/// draw.
///
/// UVs run 0..1 across every face, so a repeating texture such as the coat image in
/// [`super::horse`] shades a face the same whatever its size in the world.
///
/// The result is a plain `Mesh` carrying exactly `POSITION`, `NORMAL`, `UV_0` and indices —
/// the set a `Cuboid` mesh carries — so `Mesh::merge` with one joins the buffers instead of
/// silently skipping an attribute and leaving them different lengths (the warning at
/// `finish` in [`super::hands`]), and `translated_by` and `transformed_by` apply as they do
/// to a box.
pub(super) fn hexahedron(corners: [Vec3; 8]) -> Mesh {
    let mut positions = Vec::with_capacity(FACES.len() * 4);
    let mut normals = Vec::with_capacity(FACES.len() * 4);
    let mut uvs = Vec::with_capacity(FACES.len() * 4);
    let mut indices = Vec::with_capacity(FACES.len() * 6);

    for face in FACES {
        let [a, b, c, d] = face.map(|corner| corners[corner]);
        let normal = (c - a).cross(d - b).normalize_or_zero().to_array();
        let first = positions.len() as u32;
        positions.extend([a, b, c, d].map(|corner| corner.to_array()));
        normals.extend([normal; 4]);
        uvs.extend(FACE_UVS);
        indices.extend([first, first + 1, first + 2, first + 2, first + 3, first]);
    }

    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_indices(Indices::U32(indices))
}

#[cfg(test)]
mod tests {
    use bevy::mesh::VertexAttributeValues;

    use super::*;

    const BOX: Vec3 = Vec3::new(0.6, 0.9, 1.4);

    /// The eight corners of a box of `size` centred at the origin, in the documented order.
    fn box_corners(size: Vec3) -> [Vec3; 8] {
        let h = size / 2.0;
        [
            Vec3::new(-h.x, -h.y, h.z),
            Vec3::new(h.x, -h.y, h.z),
            Vec3::new(h.x, -h.y, -h.z),
            Vec3::new(-h.x, -h.y, -h.z),
            Vec3::new(-h.x, h.y, h.z),
            Vec3::new(h.x, h.y, h.z),
            Vec3::new(h.x, h.y, -h.z),
            Vec3::new(-h.x, h.y, -h.z),
        ]
    }

    /// A neck: a box whose top is narrower than its bottom on both axes and leans forward,
    /// so no face is parallel to another and none is axis-aligned but the bottom.
    fn neck_corners() -> [Vec3; 8] {
        let mut corners = box_corners(BOX);
        for corner in &mut corners[4..] {
            corner.x *= 0.6;
            corner.z = corner.z * 0.5 + 0.4;
        }
        corners
    }

    /// A muzzle: the box with its top-right front edge pinched to a point.
    fn wedge_corners() -> [Vec3; 8] {
        let mut corners = box_corners(BOX);
        corners[6] = corners[5];
        corners
    }

    fn positions(mesh: &Mesh) -> Vec<Vec3> {
        let Some(VertexAttributeValues::Float32x3(values)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("mesh has no positions");
        };
        values.iter().copied().map(Vec3::from_array).collect()
    }

    fn normals(mesh: &Mesh) -> Vec<Vec3> {
        let Some(VertexAttributeValues::Float32x3(values)) = mesh.attribute(Mesh::ATTRIBUTE_NORMAL)
        else {
            panic!("mesh has no normals");
        };
        values.iter().copied().map(Vec3::from_array).collect()
    }

    fn uvs(mesh: &Mesh) -> Vec<[f32; 2]> {
        let Some(VertexAttributeValues::Float32x2(values)) = mesh.attribute(Mesh::ATTRIBUTE_UV_0)
        else {
            panic!("mesh has no uvs");
        };
        values.clone()
    }

    fn indices(mesh: &Mesh) -> Vec<usize> {
        mesh.indices()
            .expect("mesh has no indices")
            .iter()
            .collect()
    }

    /// Whether `a` and `b` hold the same points counted with multiplicity, order aside.
    fn same_set(a: &[Vec3], b: &[Vec3]) -> bool {
        let mut rest = b.to_vec();
        for point in a {
            let Some(at) = rest
                .iter()
                .position(|other| point.abs_diff_eq(*other, 1e-6))
            else {
                return false;
            };
            rest.swap_remove(at);
        }
        rest.is_empty()
    }

    #[test]
    fn the_corners_of_a_cuboid_rebuild_the_cuboid() {
        let ours = hexahedron(box_corners(BOX));
        let theirs = Mesh::from(Cuboid::from_size(BOX));

        assert_eq!(ours.count_vertices(), theirs.count_vertices());
        assert!(
            same_set(&positions(&ours), &positions(&theirs)),
            "positions differ from the Cuboid's: {:?}",
            positions(&ours)
        );
        assert!(
            same_set(&normals(&ours), &normals(&theirs)),
            "normals differ from the Cuboid's: {:?}",
            normals(&ours)
        );
        assert_eq!(indices(&ours).len(), indices(&theirs).len());
    }

    #[test]
    fn every_face_is_two_triangles_over_its_own_four_vertices() {
        let mesh = hexahedron(neck_corners());
        let indices = indices(&mesh);

        assert_eq!(
            mesh.count_vertices(),
            24,
            "four vertices per face, unshared"
        );
        assert_eq!(indices.len(), 36, "two triangles per face");
        for (face, triangles) in indices.chunks(6).enumerate() {
            let own = face * 4..face * 4 + 4;
            assert!(
                triangles.iter().all(|index| own.contains(index)),
                "face {face} reaches outside its own vertices: {triangles:?}"
            );
            let (first, second) = (&triangles[..3], &triangles[3..]);
            assert_ne!(first, second, "face {face} draws the same triangle twice");
        }
        let normals = normals(&mesh);
        for (face, shared) in normals.chunks(4).enumerate() {
            assert!(
                shared.iter().all(|normal| *normal == shared[0]),
                "face {face} does not share one flat normal: {shared:?}"
            );
        }
    }

    #[test]
    fn every_face_normal_of_a_convex_solid_points_away_from_the_centroid() {
        for corners in [box_corners(BOX), neck_corners(), wedge_corners()] {
            let mesh = hexahedron(corners);
            let centroid = corners.iter().sum::<Vec3>() / corners.len() as f32;
            let positions = positions(&mesh);
            let normals = normals(&mesh);
            for (face, (points, normal)) in positions.chunks(4).zip(normals.chunks(4)).enumerate() {
                let centre = points.iter().sum::<Vec3>() / points.len() as f32;
                let outward = (centre - centroid).dot(normal[0]);
                assert!(
                    outward > 1e-4,
                    "face {face} of {corners:?} points inward: normal {:?}, outward {outward}",
                    normal[0]
                );
            }
        }
    }

    #[test]
    fn a_wedge_still_has_finite_unit_normals() {
        let mesh = hexahedron(wedge_corners());
        for (index, normal) in normals(&mesh).iter().enumerate() {
            assert!(
                normal.is_finite(),
                "normal {index} is not finite: {normal:?}"
            );
            assert!(
                (normal.length() - 1.0).abs() < 1e-5,
                "normal {index} is not unit length: {normal:?}"
            );
        }
        for (index, position) in positions(&mesh).iter().enumerate() {
            assert!(
                position.is_finite(),
                "position {index} is not finite: {position:?}"
            );
        }
    }

    #[test]
    fn every_face_carries_the_whole_image() {
        let mesh = hexahedron(neck_corners());
        let uvs = uvs(&mesh);
        assert_eq!(uvs.len(), mesh.count_vertices());
        for (face, corners) in uvs.chunks(4).enumerate() {
            for uv in corners {
                assert!(
                    uv.iter().all(|coordinate| (0.0..=1.0).contains(coordinate)),
                    "face {face} has a coordinate outside the image: {uv:?}"
                );
            }
            let mut sorted = corners.to_vec();
            sorted.sort_by(|a, b| a.partial_cmp(b).expect("finite uvs"));
            assert_eq!(
                sorted,
                [[0.0, 0.0], [0.0, 1.0], [1.0, 0.0], [1.0, 1.0]],
                "face {face} does not span the image"
            );
        }
    }

    #[test]
    fn it_carries_what_a_cuboid_carries_and_merges_with_one_either_way() {
        let cuboid = Mesh::from(Cuboid::from_size(BOX));
        let ours = hexahedron(neck_corners());

        let attributes: Vec<_> = ours
            .attributes()
            .map(|(attribute, _)| attribute.id)
            .collect();
        assert_eq!(attributes.len(), 3, "{attributes:?}");
        for expected in [
            Mesh::ATTRIBUTE_POSITION,
            Mesh::ATTRIBUTE_NORMAL,
            Mesh::ATTRIBUTE_UV_0,
        ] {
            assert!(
                attributes.contains(&expected.id),
                "missing {}",
                expected.name
            );
        }
        assert!(matches!(ours.indices(), Some(Indices::U32(_))));

        for (mut into, part) in [(cuboid.clone(), ours.clone()), (ours, cuboid)] {
            into.merge(&part).expect("a hexahedron and a cuboid merge");
            assert_eq!(into.count_vertices(), 48);
            for (attribute, values) in into.attributes() {
                assert_eq!(values.len(), 48, "{} was skipped by merge", attribute.name);
            }
            let indices = indices(&into);
            assert_eq!(indices.len(), 72);
            assert!(indices.iter().all(|index| *index < 48));
        }
    }

    #[test]
    fn it_is_a_plain_mesh_that_moves_like_a_cuboid() {
        let offset = Vec3::new(0.3, 1.1, -0.7);
        let ours = hexahedron(box_corners(BOX)).translated_by(offset);
        let theirs = Mesh::from(Cuboid::from_size(BOX)).translated_by(offset);
        assert!(same_set(&positions(&ours), &positions(&theirs)));
    }
}
