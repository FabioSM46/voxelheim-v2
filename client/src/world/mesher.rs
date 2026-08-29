//! Greedy meshing: chunk voxels in, vertex and index buffers out.
//!
//! A pure function with no Bevy type anywhere in its signature. That is not
//! tidiness — it is what lets [`mesh_chunk`] run on `AsyncComputeTaskPool` without
//! touching the world, and what lets the tests below assert exact quad counts on
//! synthetic chunks with no app, no window and no GPU.
//!
//! ## The algorithm
//!
//! The standard three-axis sweep. For each axis `d`, walk the `size + 1` planes
//! perpendicular to it. Each plane separates two voxels; a face exists there
//! exactly when one side hides what is behind it and the other does not, and it
//! points away from the hiding side. That is written into a 2D mask over the plane's other two axes, and
//! the mask is then consumed by growing maximal rectangles of identical entries —
//! width first along `u`, then height along `v` while every cell of the row
//! matches. One rectangle becomes one quad, whatever its size.
//!
//! Interior faces vanish for free: a plane with opaque voxels on both sides
//! contributes nothing to the mask.
//!
//! ## Two masks, two meshes
//!
//! The sweep fills two masks per plane from one pair of samples and produces a
//! [`ChunkMesh`] holding two [`SurfaceMesh`]es: the opaque surface and the water
//! surface. The reason is on [`ChunkMesh`] itself — blending is order-dependent and
//! Bevy sorts per entity, so the two have to be separate draws. The face rules are
//! on [`build_masks`]; the greedy merge below is shared and knows about neither.
//!
//! ## The chunk border is culled against the neighbour it is handed
//!
//! A face on the outer surface of a chunk is culled against the voxel across it in
//! the neighbouring chunk, exactly as an interior face is culled against the voxel
//! beside it. The neighbours are an **input** — [`Neighbours`], gathered by the
//! caller — and never something this function goes looking for. That is what keeps
//! it pure: it runs on a task pool because it reads nothing but its arguments, and
//! it is testable on synthetic chunks because a neighbourhood is a value.
//!
//! Two rules make that safe while the world is still arriving.
//!
//! **A neighbour that has not arrived reads as air**, so the border face is emitted
//! rather than culled. Over-drawing is the direction to be wrong in while the data
//! is incomplete: the extra quad is coincident with one the neighbour will draw and
//! is invisible from outside the pair, where culling against a chunk nobody has seen
//! is a hole a player *can* see. It is not permanent either — `render.rs` remeshes a
//! chunk when a neighbour arrives, which is what turns the over-draw into a wait.
//!
//! **A chunk draws only its own faces.** A face belongs to the voxel it sits
//! on, and at the two border planes that voxel can be the neighbour's — in which case
//! the neighbour emits it from its own sweep, at the same world position. Emitting it
//! here as well would put two coincident copies of one quad in the world and let them
//! fight over the depth buffer, which is this issue's artifact arriving by the other
//! door.

use std::sync::Arc;

use super::{BlockId, VoxelChunk, palette};

/// How many vertices one quad contributes. Four, never shared: two quads meeting at
/// an edge disagree about both the normal and the colour there.
const VERTICES_PER_QUAD: usize = 4;

/// One pass's worth of surface, as the parallel attribute buffers a GPU wants.
///
/// Deliberately not a `bevy::mesh::Mesh`: this type crosses a thread boundary and
/// is compared field by field in tests, and both are easier when it is plain data.
/// `render.rs` turns it into a `Mesh` on the main thread, which is the only place
/// that is allowed to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SurfaceMesh {
    /// Vertex positions in chunk-local blocks, four per quad, in winding order.
    pub positions: Vec<[f32; 3]>,
    /// One outward face normal per vertex. Flat by construction — every vertex of
    /// a quad shares the quad's normal, and no vertex is shared between quads.
    pub normals: Vec<[f32; 3]>,
    /// Linear RGBA per vertex, from [`palette::linear_rgba`].
    pub colors: Vec<[f32; 4]>,
    /// Two triangles per quad, wound counter-clockwise seen from outside.
    pub indices: Vec<u32>,
}

impl SurfaceMesh {
    /// How many merged quads the mesh holds.
    pub fn quad_count(&self) -> usize {
        self.positions.len() / VERTICES_PER_QUAD
    }

    /// Whether there is anything to draw. An all-air chunk, and a chunk whose every
    /// face is interior, both land here.
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    /// Adds one quad: four vertices in winding order plus the two triangles over
    /// them.
    ///
    /// Vertices are never shared between quads. Sharing would need a per-position
    /// index and would fight the flat normals and per-block colours anyway — two
    /// quads meeting at an edge disagree about both.
    fn push_quad(&mut self, corners: [[f32; 3]; 4], normal: [f32; 3], color: [f32; 4]) {
        let base = self.positions.len() as u32;

        self.positions.extend_from_slice(&corners);
        self.normals
            .extend(std::iter::repeat_n(normal, VERTICES_PER_QUAD));
        self.colors
            .extend(std::iter::repeat_n(color, VERTICES_PER_QUAD));
        self.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

/// One chunk's surface, split by how the GPU has to draw it.
///
/// **Two meshes and not one, because blending is ordered and opacity is not.** An opaque
/// quad writes depth and can be drawn in any order; a translucent one is composited over
/// what is already behind it, which Bevy arranges by sorting transparent entities back to
/// front. One mesh carrying both kinds would be one entity with one sort key, so its water
/// would draw either before the lake bed it is seen through or after the far bank it sits
/// in front of. Splitting here is what lets `render.rs` give each half a material, and it
/// costs one extra mask per plane rather than a second traversal.
///
/// The split is by *material*, not by block, so a chunk is at most two draw calls however
/// many block ids it holds.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChunkMesh {
    /// Every face of every block that hides what is behind it.
    pub opaque: SurfaceMesh,
    /// Water against transparent non-water voxels, plus the exposed skirt between
    /// unequal water levels. Equal water and water against solid blocks stay hidden.
    pub water: SurfaceMesh,
}

impl ChunkMesh {
    /// How many merged quads the chunk holds across both halves.
    pub fn quad_count(&self) -> usize {
        self.opaque.quad_count() + self.water.quad_count()
    }

    /// Whether there is anything at all to draw. Both halves empty, which is the
    /// all-air chunk and the wholly-buried one.
    pub fn is_empty(&self) -> bool {
        self.opaque.is_empty() && self.water.is_empty()
    }
}

/// The chunks across a chunk's six faces, in the order the sweep reads them.
///
/// An **input**, never a lookup. [`mesh_chunk`] is handed whatever the caller could
/// find and asks for nothing more, which is what keeps it a pure function with no
/// access to a store — and what lets the tests below build a neighbourhood out of
/// synthetic chunks and assert exact quad counts against it.
///
/// Held as `Arc` rather than as a borrow because a meshing task has to own what it
/// reads, and rather than as a copy of the six border layers because a handle costs
/// nothing and `size²` voxels do.
#[derive(Debug, Clone, Default)]
pub struct Neighbours {
    across: [Option<Arc<VoxelChunk>>; 6],
    above_horizontal: [Option<Arc<VoxelChunk>>; 4],
}

impl Neighbours {
    /// The chunk-coordinate offset of each slot, in slot order: `-x, +x, -y, +y, -z,
    /// +z`.
    ///
    /// Public because the caller has to gather the six chunks in the order this type
    /// stores them, and a second spelling of that order somewhere else is a seam to
    /// get wrong. `ChunkStore::neighbours` maps over exactly this array, and
    /// `each_solid_neighbour_culls_exactly_the_wall_they_share` pins the slot the
    /// sweep reads to the offset named here.
    pub const OFFSETS: [[i32; 3]; 6] = [
        [-1, 0, 0],
        [1, 0, 0],
        [0, -1, 0],
        [0, 1, 0],
        [0, 0, -1],
        [0, 0, 1],
    ];

    pub const ABOVE_HORIZONTAL_OFFSETS: [[i32; 3]; 4] =
        [[-1, 1, 0], [1, 1, 0], [0, 1, -1], [0, 1, 1]];

    /// A neighbourhood from the six chunks at [`Self::OFFSETS`], in that order.
    /// `None` for a coordinate the caller does not hold.
    pub fn new(across: [Option<Arc<VoxelChunk>>; 6]) -> Self {
        Self {
            across,
            above_horizontal: Default::default(),
        }
    }

    pub fn with_above_horizontal(
        across: [Option<Arc<VoxelChunk>>; 6],
        above_horizontal: [Option<Arc<VoxelChunk>>; 4],
    ) -> Self {
        let mut neighbours = Self::new(across);
        neighbours.above_horizontal = above_horizontal;
        neighbours
    }

    /// The chunk across the face perpendicular to `axis`, on the positive or the
    /// negative side of this chunk.
    ///
    /// A neighbour that disagrees about `size` is treated as absent. There is one
    /// `chunk_size` per session so it cannot happen — and the mesher indexes a
    /// neighbour with *this* chunk's coordinates, so the alternative to a comparison
    /// per plane is reading past the end of somebody else's array. Unreachable states
    /// get the conservative answer here, the same way a malformed chunk does.
    fn across(&self, axis: usize, positive: bool, size: usize) -> Option<&VoxelChunk> {
        self.across[axis * 2 + usize::from(positive)]
            .as_deref()
            .filter(|neighbour| neighbour.size() == size)
    }

    fn above_horizontal(&self, axis: usize, positive: bool, size: usize) -> Option<&VoxelChunk> {
        let slot = match axis {
            0 => usize::from(positive),
            2 => 2 + usize::from(positive),
            _ => return None,
        };
        self.above_horizontal[slot]
            .as_deref()
            .filter(|chunk| chunk.size() == size)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FaceGeometry {
    Full,
    WaterTop { level: u8 },
    WaterBottom { level: u8 },
    WaterSide { bottom: u8, top: u8 },
}

/// One entry of the sweep's mask: a face of `block`, pointing along the sweep axis
/// or against it.
///
/// `Eq` is what the merge compares, so two faces merge only when they are the same
/// block *and* point the same way. Without the direction in the key, the two sides
/// of a one-voxel-thick wall would merge into a single quad facing one way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Face {
    block: BlockId,
    /// True when the normal points along +axis (the solid voxel is on the negative
    /// side of the plane).
    positive: bool,
    geometry: FaceGeometry,
}

/// Meshes one chunk against the neighbours it was given.
///
/// Deterministic: the same voxels and the same neighbours produce byte-identical
/// buffers, every time. The sweep order is fixed, nothing is hashed, and no float is
/// derived from anything but an integer coordinate — which is what makes the output
/// comparable in a test and cacheable later.
///
/// A default [`Neighbours`] meshes the chunk in isolation, emitting every border
/// face. That is a legitimate answer rather than a degraded one: it is what a chunk at
/// the edge of the streamed volume actually needs, and what every chunk gets for the
/// frame or two before its neighbours arrive.
pub fn mesh_chunk(chunk: &VoxelChunk, neighbours: &Neighbours) -> ChunkMesh {
    let mut mesh = ChunkMesh::default();
    let size = chunk.size();
    if size == 0 {
        return mesh;
    }

    // Reused across every plane of every axis, cleared rather than reallocated:
    // a 32³ chunk sweeps 99 planes, and 99 allocations of 4 KiB is pure waste.
    // Two of them now, filled from one pass over the plane — the second mask is
    // what the split costs, and it is cheaper than sweeping the chunk twice.
    let mut opaque_mask = vec![None; size * size];
    let mut water_mask = vec![None; size * size];

    for axis in 0..3 {
        // The two axes that span the plane. This cyclic choice makes (u, v, axis) a
        // right-handed triple — û × v̂ = axis — which is what makes the winding
        // below correct for all three axes with no special cases.
        let u = (axis + 1) % 3;
        let v = (axis + 2) % 3;

        for plane in 0..=size {
            build_masks(
                chunk,
                neighbours,
                axis,
                u,
                v,
                plane,
                &mut opaque_mask,
                &mut water_mask,
            );
            merge_mask(&mut mesh.opaque, size, axis, u, v, plane, &mut opaque_mask);
            merge_mask(&mut mesh.water, size, axis, u, v, plane, &mut water_mask);
        }
    }

    mesh
}

/// Fills both masks with the faces on the plane at `axis = plane`.
///
/// The plane sits between the voxels at `plane - 1` and `plane` along `axis`. At the
/// first and the last plane one of those two voxels is outside the chunk, and it is
/// read from the neighbour across that face — or counted as air when there is no
/// neighbour to read, which is what leaves a border face exposed rather than culling
/// it against data nobody has.
///
/// Only a face whose **own side is inside this chunk** is written. At a border plane
/// that side can be the neighbour's, and the face is then the neighbour's to draw
/// from its own sweep at the same world position; emitting it here too would put two
/// coincident copies of one quad in the world.
///
/// **The two masks ask different questions of the same pair of voxels**, which is the whole
/// of the split:
///
/// - `opaque_mask` gets a face wherever exactly one side is opaque, so an opaque block
///   draws against air *and* against water — that is what makes a lake bed visible.
#[expect(
    clippy::too_many_arguments,
    reason = "the sweep's coordinate frame plus the two masks it fills; the frame is \
              already the shape `merge_mask` and `quad_corners` take"
)]
fn build_masks(
    chunk: &VoxelChunk,
    neighbours: &Neighbours,
    axis: usize,
    u: usize,
    v: usize,
    plane: usize,
    opaque_mask: &mut [Option<Face>],
    water_mask: &mut [Option<Face>],
) {
    let size = chunk.size();

    // Which chunk each side of the plane is read from, and at which layer of it.
    // Resolved once per plane rather than per cell, and `None` means air — an absent
    // neighbour, and nothing else.
    let below = match plane.checked_sub(1) {
        Some(layer) => Some((chunk, layer)),
        None => neighbours
            .across(axis, false, size)
            .map(|neighbour| (neighbour, size - 1)),
    };
    let above = if plane < size {
        Some((chunk, plane))
    } else {
        neighbours
            .across(axis, true, size)
            .map(|neighbour| (neighbour, 0))
    };

    // Whether the voxel on each side is one of ours. Only our own faces are ours to
    // draw; see this function's doc comment.
    let below_is_ours = plane > 0;
    let above_is_ours = plane < size;

    for j in 0..size {
        for i in 0..size {
            let negative = sample(below, axis, u, v, i, j);
            let positive = sample(above, axis, u, v, i, j);

            opaque_mask[j * size + i] =
                match (palette::is_opaque(negative), palette::is_opaque(positive)) {
                    // Opaque below, see-through above: the face belongs to the opaque
                    // voxel and points along +axis. "See-through" is air or water,
                    // which is what keeps the lake bed's top.
                    (true, false) if below_is_ours => Some(Face {
                        block: negative,
                        positive: true,
                        geometry: FaceGeometry::Full,
                    }),
                    (false, true) if above_is_ours => Some(Face {
                        block: positive,
                        positive: false,
                        geometry: FaceGeometry::Full,
                    }),
                    // See-through on both sides: nothing. Opaque on both sides: an
                    // interior face, which is what greedy meshing exists never to emit.
                    // What is left is a face whose opaque side is across the border, and
                    // the chunk that owns it draws it.
                    _ => None,
                };

            let negative_level = effective_water_level(below, chunk, neighbours, axis, u, v, i, j);
            let positive_level = effective_water_level(above, chunk, neighbours, axis, u, v, i, j);

            water_mask[j * size + i] = water_face(
                axis,
                negative,
                negative_level,
                below_is_ours,
                positive,
                positive_level,
                above_is_ours,
            );
        }
    }
}

fn water_face(
    axis: usize,
    negative: BlockId,
    negative_level: u8,
    negative_is_ours: bool,
    positive: BlockId,
    positive_level: u8,
    positive_is_ours: bool,
) -> Option<Face> {
    let negative_water = palette::is_water(negative);
    let positive_water = palette::is_water(positive);

    match (negative_water, positive_water) {
        (true, false) if !palette::is_opaque(positive) && negative_is_ours => Some(Face {
            block: palette::WATER,
            positive: true,
            geometry: if axis == 1 {
                FaceGeometry::WaterTop {
                    level: negative_level,
                }
            } else {
                FaceGeometry::WaterSide {
                    bottom: 0,
                    top: negative_level,
                }
            },
        }),
        (false, true) if !palette::is_opaque(negative) && positive_is_ours => Some(Face {
            block: palette::WATER,
            positive: false,
            geometry: if axis == 1 {
                FaceGeometry::WaterBottom {
                    level: positive_level,
                }
            } else {
                FaceGeometry::WaterSide {
                    bottom: 0,
                    top: positive_level,
                }
            },
        }),
        (true, true) if axis == 1 => None,
        (true, true) if negative_level > positive_level && negative_is_ours => Some(Face {
            block: palette::WATER,
            positive: true,
            geometry: FaceGeometry::WaterSide {
                bottom: positive_level,
                top: negative_level,
            },
        }),
        (true, true) if positive_level > negative_level && positive_is_ours => Some(Face {
            block: palette::WATER,
            positive: false,
            geometry: FaceGeometry::WaterSide {
                bottom: negative_level,
                top: positive_level,
            },
        }),
        _ => None,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the sweep coordinate frame is the same one `sample` takes"
)]
fn effective_water_level(
    source: Option<(&VoxelChunk, usize)>,
    chunk: &VoxelChunk,
    neighbours: &Neighbours,
    axis: usize,
    u: usize,
    v: usize,
    i: usize,
    j: usize,
) -> u8 {
    let Some((source_chunk, layer)) = source else {
        return 0;
    };

    let mut cell = [0usize; 3];
    cell[axis] = layer;
    cell[u] = i;
    cell[v] = j;
    let block = source_chunk.block(cell);
    let encoded = palette::water_level(block);
    if encoded == 0 {
        return 0;
    }

    let above = if cell[1] + 1 < source_chunk.size() {
        cell[1] += 1;
        source_chunk.block(cell)
    } else if std::ptr::eq(source_chunk, chunk) {
        cell[1] = 0;
        neighbours
            .across(1, true, chunk.size())
            .map_or(palette::AIR, |above| above.block(cell))
    } else if neighbours
        .across(1, false, chunk.size())
        .is_some_and(|below| std::ptr::eq(source_chunk, below))
    {
        cell[1] = 0;
        chunk.block(cell)
    } else {
        let positive = neighbours
            .across(axis, true, chunk.size())
            .is_some_and(|across| std::ptr::eq(source_chunk, across));
        cell[1] = 0;
        neighbours
            .above_horizontal(axis, positive, chunk.size())
            .map_or(palette::AIR, |above| above.block(cell))
    };

    if palette::is_water(above) { 8 } else { encoded }
}

/// One voxel of a plane's negative or positive side.
///
/// `None` is the side that has no chunk behind it — a neighbour this session has not
/// been sent — and it reads as air, so the face in front of it is emitted.
fn sample(
    source: Option<(&VoxelChunk, usize)>,
    axis: usize,
    u: usize,
    v: usize,
    i: usize,
    j: usize,
) -> BlockId {
    let Some((chunk, layer)) = source else {
        return palette::AIR;
    };

    let mut cell = [0usize; 3];
    cell[axis] = layer;
    cell[u] = i;
    cell[v] = j;
    chunk.block(cell)
}

/// Consumes `mask`, emitting one quad per maximal rectangle of identical faces.
///
/// Leaves the mask empty, which is what lets the caller reuse the buffer for the
/// next plane without clearing it separately.
fn merge_mask(
    mesh: &mut SurfaceMesh,
    size: usize,
    axis: usize,
    u: usize,
    v: usize,
    plane: usize,
    mask: &mut [Option<Face>],
) {
    for j in 0..size {
        let mut i = 0;
        while i < size {
            let Some(face) = mask[j * size + i] else {
                i += 1;
                continue;
            };

            let partial_vertical = matches!(
                face.geometry,
                FaceGeometry::WaterSide { bottom, top } if bottom != 0 || top != 8
            );
            let u_is_restricted = partial_vertical && u == 1;
            let v_is_restricted = partial_vertical && v == 1;

            // Grow along u while the row agrees.
            let mut width = 1;
            while !u_is_restricted && i + width < size && mask[j * size + i + width] == Some(face) {
                width += 1;
            }

            // Then along v, but only by whole rows: a rectangle is the shape a quad
            // can be, so a row that matches for part of the width does not count.
            let mut height = 1;
            while !v_is_restricted && j + height < size {
                let row = (j + height) * size;
                if (i..i + width).any(|x| mask[row + x] != Some(face)) {
                    break;
                }
                height += 1;
            }

            for row in j..j + height {
                mask[row * size + i..row * size + i + width].fill(None);
            }

            mesh.push_quad(
                quad_corners(axis, u, v, plane, i, j, width, height, face),
                normal(axis, face.positive),
                palette::linear_rgba(face.block),
            );

            i += width;
        }
    }
}

/// The four corners of one merged quad, in winding order.
///
/// Chunk-local block units: voxel `n` along an axis occupies `[n, n + 1]`, so the
/// plane between voxels `plane - 1` and `plane` sits at exactly `plane`.
///
/// The winding is the load-bearing part. With (u, v, axis) right-handed, walking
/// `origin → +u → +u+v → +v` is counter-clockwise seen from `+axis`, which is what
/// Bevy's default face culling wants for a face whose normal is `+axis`. A face
/// pointing the other way walks v first, reversing it. Get this backwards and the
/// terrain renders inside-out: every surface invisible and every cavity solid.
#[expect(
    clippy::too_many_arguments,
    reason = "the sweep's full coordinate frame; bundling it into a struct would \
              only move the same nine values behind a name that adds nothing"
)]
fn quad_corners(
    axis: usize,
    u: usize,
    v: usize,
    plane: usize,
    i: usize,
    j: usize,
    width: usize,
    height: usize,
    face: Face,
) -> [[f32; 3]; 4] {
    let mut origin = [0.0f32; 3];
    origin[axis] = plane as f32;
    origin[u] = i as f32;
    origin[v] = j as f32;

    let mut along_u = [0.0f32; 3];
    along_u[u] = width as f32;

    let mut along_v = [0.0f32; 3];
    along_v[v] = height as f32;

    match face.geometry {
        FaceGeometry::Full | FaceGeometry::WaterBottom { .. } => {}
        FaceGeometry::WaterTop { level } => {
            debug_assert_eq!(axis, 1);
            origin[axis] = plane.saturating_sub(1) as f32 + f32::from(level) / 8.0;
        }
        FaceGeometry::WaterSide { bottom, top } => {
            debug_assert_ne!(axis, 1);
            let vertical_origin = if u == 1 { i as f32 } else { j as f32 };
            origin[1] = vertical_origin + f32::from(bottom) / 8.0;
            if u == 1 {
                along_u[1] = if bottom == 0 && top == 8 {
                    width as f32
                } else {
                    f32::from(top - bottom) / 8.0
                };
            } else {
                along_v[1] = if bottom == 0 && top == 8 {
                    height as f32
                } else {
                    f32::from(top - bottom) / 8.0
                };
            }
        }
    }

    let add = |a: [f32; 3], b: [f32; 3]| [a[0] + b[0], a[1] + b[1], a[2] + b[2]];
    let far = add(add(origin, along_u), along_v);

    if face.positive {
        [origin, add(origin, along_u), far, add(origin, along_v)]
    } else {
        [origin, add(origin, along_v), far, add(origin, along_u)]
    }
}

/// The unit normal of a face on `axis`.
fn normal(axis: usize, positive: bool) -> [f32; 3] {
    let mut normal = [0.0f32; 3];
    normal[axis] = if positive { 1.0 } else { -1.0 };
    normal
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    use super::*;

    /// Two triangles per quad.
    const INDICES_PER_QUAD: usize = 6;

    /// The chunk edge the server actually sends. Every count below is written for
    /// this size, so a test that hardcodes 32 is agreeing with the server on
    /// purpose.
    const SIZE: usize = 32;

    fn air(size: usize) -> VoxelChunk {
        VoxelChunk::all_air(size)
    }

    /// The opaque half of a chunk's surface, under the name the sweep's own tests have
    /// always called for.
    ///
    /// **It deliberately shadows [`super::mesh_chunk`]**, which is what a glob import lets
    /// an explicit item do. Every assertion that uses it is about the sweep — winding,
    /// merging, border culling, determinism — and none about the opaque/water split, so
    /// restating each as `.opaque` would be thirty-five edits that say nothing. The tests
    /// that *are* about the split spell `super::mesh_chunk` and name both halves.
    fn mesh_chunk(chunk: &VoxelChunk, neighbours: &Neighbours) -> SurfaceMesh {
        super::mesh_chunk(chunk, neighbours).opaque
    }

    /// A chunk of one block, in the middle so no border face is involved.
    fn single_block(size: usize, block: BlockId) -> VoxelChunk {
        let mut chunk = air(size);
        chunk.set(size / 2, size / 2, size / 2, block);
        chunk
    }

    /// Nothing known across any face — a chunk meshed on its own.
    ///
    /// Every border face is then emitted, which is exactly what this mesher did before
    /// it could see a neighbour at all: the conservative answer, and the one a chunk at
    /// the edge of the streamed volume genuinely wants.
    fn alone() -> Neighbours {
        Neighbours::default()
    }

    /// A neighbourhood with `chunk` across one face and nothing across the other five.
    fn across(axis: usize, positive: bool, chunk: VoxelChunk) -> Neighbours {
        let mut slots: [Option<Arc<VoxelChunk>>; 6] = Default::default();
        slots[axis * 2 + usize::from(positive)] = Some(Arc::new(chunk));
        Neighbours::new(slots)
    }

    fn across_with_above(
        axis: usize,
        positive: bool,
        chunk: VoxelChunk,
        above: VoxelChunk,
    ) -> Neighbours {
        let mut sides: [Option<Arc<VoxelChunk>>; 6] = Default::default();
        sides[axis * 2 + usize::from(positive)] = Some(Arc::new(chunk));
        let mut diagonals: [Option<Arc<VoxelChunk>>; 4] = Default::default();
        let slot = if axis == 0 {
            usize::from(positive)
        } else {
            2 + usize::from(positive)
        };
        diagonals[slot] = Some(Arc::new(above));
        Neighbours::with_above_horizontal(sides, diagonals)
    }

    /// The same chunk across all six faces.
    fn surrounded_by(chunk: &VoxelChunk) -> Neighbours {
        let shared = Arc::new(chunk.clone());
        Neighbours::new(std::array::from_fn(|_| Some(Arc::clone(&shared))))
    }

    fn solid(size: usize, block: BlockId) -> VoxelChunk {
        let mut chunk = air(size);
        for y in 0..size {
            for z in 0..size {
                for x in 0..size {
                    chunk.set(x, y, z, block);
                }
            }
        }
        chunk
    }

    /// How many quads face each direction, keyed by the normal as integers so the
    /// map is ordered and the failure message readable.
    fn quads_by_normal(mesh: &SurfaceMesh) -> BTreeMap<[i32; 3], usize> {
        let mut counts = BTreeMap::new();
        for quad in 0..mesh.quad_count() {
            let n = mesh.normals[quad * VERTICES_PER_QUAD];
            let key = [n[0] as i32, n[1] as i32, n[2] as i32];
            *counts.entry(key).or_insert(0) += 1;
        }
        counts
    }

    /// The area of a quad in blocks, from the two edges leaving its first corner.
    /// The cross product's magnitude is exactly that area.
    fn quad_area(mesh: &SurfaceMesh, quad: usize) -> f32 {
        let n = winding_normal(mesh, quad);
        (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt()
    }

    fn quad_extent(mesh: &SurfaceMesh, quad: usize, axis: usize) -> (f32, f32) {
        let corners = &mesh.positions[quad * VERTICES_PER_QUAD..][..VERTICES_PER_QUAD];
        corners.iter().fold(
            (f32::INFINITY, f32::NEG_INFINITY),
            |(minimum, maximum), corner| (minimum.min(corner[axis]), maximum.max(corner[axis])),
        )
    }

    fn quads_facing(mesh: &SurfaceMesh, wanted: [f32; 3]) -> Vec<usize> {
        (0..mesh.quad_count())
            .filter(|quad| mesh.normals[quad * VERTICES_PER_QUAD] == wanted)
            .collect()
    }

    /// The six axis-aligned directions, as the keys [`quads_by_normal`] produces.
    fn six_directions() -> [[i32; 3]; 6] {
        [
            [1, 0, 0],
            [-1, 0, 0],
            [0, 1, 0],
            [0, -1, 0],
            [0, 0, 1],
            [0, 0, -1],
        ]
    }

    /// The geometric normal of a quad, from its winding: (c1 - c0) × (c3 - c0).
    ///
    /// This is what the GPU derives the facing from, so comparing it against the
    /// normal attribute is how a winding mistake is caught without a screen.
    fn winding_normal(mesh: &SurfaceMesh, quad: usize) -> [f32; 3] {
        let c = |k: usize| mesh.positions[quad * VERTICES_PER_QUAD + k];
        let (c0, c1, c3) = (c(0), c(1), c(3));
        let a = [c1[0] - c0[0], c1[1] - c0[1], c1[2] - c0[2]];
        let b = [c3[0] - c0[0], c3[1] - c0[1], c3[2] - c0[2]];
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }

    #[test]
    fn an_all_air_chunk_has_nothing_to_draw() {
        let mesh = mesh_chunk(&air(SIZE), &alone());

        assert_eq!(mesh.quad_count(), 0);
        assert!(mesh.is_empty());
        assert!(mesh.positions.is_empty() && mesh.indices.is_empty());
    }

    #[test]
    fn a_zero_sized_chunk_is_not_a_panic() {
        // Unreachable through the decoder, which refuses chunk_size 0 at the
        // handshake. Asserted anyway: the mesher is the one place a size of zero
        // would become an empty-range subtraction rather than an error — and the
        // border layer of a neighbour is `size - 1`, so the neighbourhood is passed
        // in too rather than left as the case nobody tried.
        assert_eq!(mesh_chunk(&air(0), &alone()).quad_count(), 0);
        assert_eq!(mesh_chunk(&air(0), &surrounded_by(&air(0))).quad_count(), 0);
    }

    #[test]
    fn one_solid_block_has_exactly_six_faces() {
        let mesh = mesh_chunk(&single_block(SIZE, palette::STONE), &alone());

        assert_eq!(mesh.quad_count(), 6);
        assert_eq!(mesh.positions.len(), 24);
        assert_eq!(mesh.indices.len(), 36);

        let by_normal = quads_by_normal(&mesh);
        for direction in six_directions() {
            assert_eq!(
                by_normal.get(&direction),
                Some(&1),
                "one face per direction; got {by_normal:?}"
            );
        }
    }

    #[test]
    fn a_fully_solid_chunk_merges_to_one_quad_per_direction() {
        // The strongest greedy-merge assertion there is. 32³ solid voxels have
        // 196 608 faces before merging and 6 after: every interior face is culled,
        // and each of the six outer walls merges into a single 32×32 quad. A mesher
        // that merged along one axis only would answer 192 here.
        let mesh = mesh_chunk(&solid(SIZE, palette::STONE), &alone());

        assert_eq!(mesh.quad_count(), 6, "one merged wall per direction");

        let by_normal = quads_by_normal(&mesh);
        for direction in six_directions() {
            assert_eq!(by_normal.get(&direction), Some(&1), "{by_normal:?}");
        }

        // And each of them really is the whole wall, not a 1×1 corner that happens
        // to face the right way.
        for quad in 0..mesh.quad_count() {
            let area = quad_area(&mesh, quad);
            assert_eq!(
                area,
                (SIZE * SIZE) as f32,
                "quad {quad} covers {area} blocks, want {}",
                SIZE * SIZE
            );
        }
    }

    #[test]
    fn a_one_voxel_thick_plane_merges_to_one_quad_per_visible_direction() {
        // A full slab at y = 0. Six directions are visible: up, down, and the four
        // sides, which are on the chunk border and therefore exposed. The two large
        // faces merge to 32×32 and the four edges to 1×32.
        let mut chunk = air(SIZE);
        for z in 0..SIZE {
            for x in 0..SIZE {
                chunk.set(x, 0, z, palette::GRASS);
            }
        }
        let mesh = mesh_chunk(&chunk, &alone());

        assert_eq!(mesh.quad_count(), 6);
        let by_normal = quads_by_normal(&mesh);
        for direction in six_directions() {
            assert_eq!(
                by_normal.get(&direction),
                Some(&1),
                "one quad per direction; got {by_normal:?}"
            );
        }

        // The flat faces are the whole slab; the rims are one block deep.
        let areas: Vec<f32> = (0..mesh.quad_count())
            .map(|q| quad_area(&mesh, q))
            .collect();
        let large = areas.iter().filter(|a| **a == (SIZE * SIZE) as f32).count();
        let rims = areas.iter().filter(|a| **a == SIZE as f32).count();
        assert_eq!((large, rims), (2, 4), "areas were {areas:?}");
    }

    #[test]
    fn faces_pointing_opposite_ways_never_merge() {
        // Two blocks of the same type touching only along an edge. Their +x and -x
        // faces land coplanar and adjacent in the same mask, and so do their +y and
        // -y faces — so a merge keyed on the block id alone would fuse each pair into
        // one quad that faces the wrong way over half its area, and the terrain would
        // have holes exactly where two heights meet. Twelve faces, none merged.
        let mut chunk = air(SIZE);
        chunk.set(4, 4, 4, palette::STONE);
        chunk.set(5, 5, 4, palette::STONE);
        let mesh = mesh_chunk(&chunk, &alone());

        assert_eq!(mesh.quad_count(), 12);
        let by_normal = quads_by_normal(&mesh);
        for direction in six_directions() {
            assert_eq!(
                by_normal.get(&direction),
                Some(&2),
                "one face per block per direction; got {by_normal:?}"
            );
        }
        for quad in 0..mesh.quad_count() {
            assert_eq!(
                quad_area(&mesh, quad),
                1.0,
                "quad {quad} merged with a neighbour"
            );
        }
    }

    #[test]
    fn two_stacked_blocks_share_no_face() {
        // The interior-culling assertion at the smallest scale that has an interior:
        // 12 faces exist, 2 are shared, and the 4 side pairs merge into 4 quads of
        // area 2. A mesher that culled nothing would answer 12; one that culled but
        // did not merge would answer 10.
        let mut chunk = air(SIZE);
        chunk.set(4, 4, 4, palette::STONE);
        chunk.set(4, 5, 4, palette::STONE);
        let mesh = mesh_chunk(&chunk, &alone());

        assert_eq!(mesh.quad_count(), 6);
        let areas: Vec<f32> = (0..mesh.quad_count())
            .map(|q| quad_area(&mesh, q))
            .collect();
        assert_eq!(
            (
                areas.iter().filter(|a| **a == 1.0).count(),
                areas.iter().filter(|a| **a == 2.0).count()
            ),
            (2, 4),
            "two 1×1 caps and four 1×2 sides; areas were {areas:?}"
        );
    }

    #[test]
    fn different_blocks_never_merge() {
        // Two ids that would merge on geometry alone must not, or the colour of one
        // would be drawn over the other. A 2×1×1 pair of different blocks has 10
        // exposed faces, and the four sides cannot merge across the seam.
        let mut chunk = air(SIZE);
        chunk.set(4, 4, 4, palette::STONE);
        chunk.set(5, 4, 4, palette::GRASS);
        let mesh = mesh_chunk(&chunk, &alone());

        assert_eq!(mesh.quad_count(), 10, "no quad spans the two block types");

        let colours: Vec<[f32; 4]> = (0..mesh.quad_count())
            .map(|q| mesh.colors[q * VERTICES_PER_QUAD])
            .collect();
        assert_eq!(
            colours
                .iter()
                .filter(|c| **c == palette::linear_rgba(palette::STONE))
                .count(),
            5
        );
        assert_eq!(
            colours
                .iter()
                .filter(|c| **c == palette::linear_rgba(palette::GRASS))
                .count(),
            5
        );
    }

    #[test]
    fn a_border_face_is_emitted_when_the_neighbour_has_not_arrived() {
        // This assertion used to pin a documented limitation — the mesher could not see
        // a neighbour, so it emitted every border face. It now pins the rule that
        // replaced it, on the same numbers: with nothing known across a face, the face
        // is emitted. A single block in the corner still has all six, three of them on
        // the chunk border.
        //
        // Over-drawing is the direction to be wrong in. Culling these three would be a
        // hole for as long as the neighbour took to arrive, and a permanent one at the
        // edge of the streamed volume, where the neighbour never arrives at all.
        let mut chunk = air(SIZE);
        chunk.set(0, 0, 0, palette::STONE);

        assert_eq!(mesh_chunk(&chunk, &alone()).quad_count(), 6);
    }

    #[test]
    fn a_chunk_enclosed_by_solid_neighbours_has_nothing_to_draw() {
        // The measure the issue asks for, at its extreme: a solid chunk on its own
        // merges to six walls, and the same chunk buried in solid rock has no exposed
        // face at all. `render.rs` spawns no entity for an empty mesh, so underground —
        // where most of a streamed volume is — a chunk costs a draw call only if
        // something has been dug into it.
        let chunk = solid(SIZE, palette::STONE);

        let alone = mesh_chunk(&chunk, &alone());
        let buried = mesh_chunk(&chunk, &surrounded_by(&chunk));

        assert_eq!(alone.quad_count(), 6, "six walls with nothing beside it");
        assert_eq!(
            buried.quad_count(),
            0,
            "and none of them once it is enclosed"
        );
        assert!(buried.is_empty());
    }

    #[test]
    fn each_solid_neighbour_culls_exactly_the_wall_they_share() {
        // What pins `Neighbours::OFFSETS` to the slot the sweep reads: fill exactly one
        // slot and the wall that disappears must be the one facing that offset. A
        // transposed pair of slots would still cull six walls out of six and pass every
        // count in this file, while drawing the world with its seams inside out.
        let chunk = solid(SIZE, palette::STONE);

        for (slot, offset) in Neighbours::OFFSETS.into_iter().enumerate() {
            let neighbour = across(slot / 2, slot % 2 == 1, solid(SIZE, palette::STONE));
            let mesh = mesh_chunk(&chunk, &neighbour);

            assert_eq!(
                mesh.quad_count(),
                5,
                "slot {slot} culled more than one wall"
            );
            let by_normal = quads_by_normal(&mesh);
            assert_eq!(
                by_normal.get(&offset),
                None,
                "the wall facing {offset:?} is still drawn; got {by_normal:?}"
            );
            for other in six_directions().into_iter().filter(|d| *d != offset) {
                assert_eq!(by_normal.get(&other), Some(&1), "{by_normal:?}");
            }
        }
    }

    #[test]
    fn an_air_neighbour_culls_nothing() {
        // The control that keeps the rule about *solidity* rather than about presence.
        // The server streams a great deal of sky, and a chunk of it next door hides
        // nothing: a wall culled against air is a hole.
        let chunk = solid(SIZE, palette::STONE);

        assert_eq!(
            mesh_chunk(&chunk, &surrounded_by(&air(SIZE))).quad_count(),
            6
        );
    }

    #[test]
    fn an_arriving_neighbour_takes_the_wall_they_share_away() {
        // The measurable form of "culling happened", on the shape the pipeline actually
        // produces: the same chunk meshed before its neighbour arrived and again after.
        // Six quads become five, and the one that went is the shared wall.
        let chunk = solid(SIZE, palette::STONE);

        let before = mesh_chunk(&chunk, &alone());
        let after = mesh_chunk(&chunk, &across(0, true, solid(SIZE, palette::STONE)));

        assert_eq!((before.quad_count(), after.quad_count()), (6, 5));
        assert_eq!(quads_by_normal(&after).get(&[1, 0, 0]), None);
    }

    #[test]
    fn a_hole_dug_at_a_border_is_drawn_from_both_sides_and_counted_once() {
        // Digging into a chunk wall, which is the case that made this issue visible.
        // `dug` is solid but for one voxel of air on the wall it shares with `wall`.
        //
        // Each chunk emits the faces of its own solid voxels and no others:
        //
        //   - `dug` gains the five faces that bound the cavity from inside its own
        //     chunk — four sides and the back — and none of them merge with anything.
        //     Its shared wall stays culled: it was never the cavity's floor.
        //   - `wall` gains exactly one quad, the face of its own voxel that the cavity
        //     uncovered. That is the floor of the hole, and it is drawn once.
        //
        // A mesher that let `dug` draw the floor as well would put two coincident quads
        // there, which is the same artifact as the uncalled border wall and just as
        // invisible until something moves.
        let mut dug = solid(SIZE, palette::STONE);
        dug.set(SIZE - 1, 5, 5, palette::AIR);
        let wall = solid(SIZE, palette::STONE);

        let dug_mesh = mesh_chunk(&dug, &across(0, true, wall.clone()));
        let wall_mesh = mesh_chunk(&wall, &across(0, false, dug.clone()));

        // One wall on -x, four on ±y/±z — all against no neighbour — plus the five
        // faces of the cavity.
        assert_eq!(dug_mesh.quad_count(), 10);
        // Nothing at all on the plane the two chunks share. Only an x-facing quad can lie
        // in it, and every one of `dug`'s is at a lower x — the four sides of the cavity
        // face y and z, and its back faces +x from x = 31. (A y- or z-facing wall has
        // corners at x = 32 because it spans the chunk; that is its width, not its plane.)
        let on_the_shared_plane = (0..dug_mesh.quad_count())
            .filter(|quad| {
                let corner = dug_mesh.positions[quad * VERTICES_PER_QUAD];
                let normal = dug_mesh.normals[quad * VERTICES_PER_QUAD];
                normal[0] != 0.0 && corner[0] == SIZE as f32
            })
            .count();
        assert_eq!(
            on_the_shared_plane, 0,
            "the chunk drew something on the wall its neighbour owns"
        );

        // Five walls against no neighbour, plus the floor of the hole.
        assert_eq!(wall_mesh.quad_count(), 6);
        let floor: Vec<usize> = (0..wall_mesh.quad_count())
            .filter(|quad| wall_mesh.normals[quad * VERTICES_PER_QUAD] == [-1.0, 0.0, 0.0])
            .collect();
        assert_eq!(floor.len(), 1, "the floor of the hole is drawn once");
        assert_eq!(quad_area(&wall_mesh, floor[0]), 1.0, "and it is one block");
        assert!(
            wall_mesh.positions[floor[0] * VERTICES_PER_QUAD][0] == 0.0,
            "the floor sits on the shared plane"
        );
    }

    #[test]
    fn a_neighbour_that_disagrees_about_the_chunk_size_is_treated_as_absent() {
        // There is one `chunk_size` per session, so this cannot happen. If it ever did,
        // the alternative to treating the neighbour as absent is indexing its array with
        // this chunk's coordinates — so the unreachable state gets the conservative
        // answer rather than an out-of-bounds read.
        let chunk = solid(SIZE, palette::STONE);

        assert_eq!(
            mesh_chunk(&chunk, &surrounded_by(&solid(SIZE - 1, palette::STONE))).quad_count(),
            6
        );
        assert_eq!(
            mesh_chunk(&chunk, &surrounded_by(&solid(SIZE + 1, palette::STONE))).quad_count(),
            6
        );
    }

    #[test]
    fn every_quad_is_wound_to_face_outward() {
        // Winding is the one property of a mesh that a headless test can check and a
        // human staring at a screen cannot: inside-out terrain looks like no terrain
        // at all. The cross product of the quad's first two edges must point the
        // same way as the normal attribute it carries.
        let mut chunk = air(SIZE);
        chunk.set(1, 1, 1, palette::STONE);
        chunk.set(1, 2, 1, palette::DIRT);
        chunk.set(2, 1, 1, palette::GRASS);
        for z in 0..SIZE {
            for x in 0..SIZE {
                chunk.set(x, 8, z, palette::SNOW);
            }
        }
        let mesh = mesh_chunk(&chunk, &alone());
        assert!(mesh.quad_count() > 0);

        for quad in 0..mesh.quad_count() {
            let geometric = winding_normal(&mesh, quad);
            let declared = mesh.normals[quad * VERTICES_PER_QUAD];
            let length =
                (geometric[0].powi(2) + geometric[1].powi(2) + geometric[2].powi(2)).sqrt();
            assert!(length > 0.0, "quad {quad} is degenerate");

            let unit = [
                geometric[0] / length,
                geometric[1] / length,
                geometric[2] / length,
            ];
            assert_eq!(
                unit, declared,
                "quad {quad} is wound against its own normal, so it would be culled"
            );
        }
    }

    #[test]
    fn every_normal_is_a_unit_axis_and_every_vertex_of_a_quad_agrees() {
        let mesh = mesh_chunk(&solid(SIZE, palette::SNOW), &alone());

        for quad in 0..mesh.quad_count() {
            let first = mesh.normals[quad * VERTICES_PER_QUAD];
            assert_eq!(
                first.iter().map(|c| c.abs()).sum::<f32>(),
                1.0,
                "normal {first:?} is not an axis"
            );
            for vertex in 1..VERTICES_PER_QUAD {
                assert_eq!(mesh.normals[quad * VERTICES_PER_QUAD + vertex], first);
            }
        }
    }

    #[test]
    fn the_buffers_stay_in_step() {
        let mut chunk = air(SIZE);
        for y in 0..SIZE / 2 {
            for z in 0..SIZE {
                for x in 0..SIZE {
                    chunk.set(
                        x,
                        y,
                        z,
                        if y % 3 == 0 {
                            palette::DIRT
                        } else {
                            palette::STONE
                        },
                    );
                }
            }
        }
        let mesh = mesh_chunk(&chunk, &alone());

        assert_eq!(mesh.positions.len(), mesh.quad_count() * VERTICES_PER_QUAD);
        assert_eq!(mesh.normals.len(), mesh.positions.len());
        assert_eq!(mesh.colors.len(), mesh.positions.len());
        assert_eq!(mesh.indices.len(), mesh.quad_count() * INDICES_PER_QUAD);
        assert!(
            mesh.indices
                .iter()
                .all(|i| (*i as usize) < mesh.positions.len()),
            "an index points past the vertex buffer"
        );
    }

    #[test]
    fn every_position_is_inside_the_chunk() {
        // A quad at 33 would be drawn inside the neighbouring chunk. The bound is
        // inclusive because a face on the far border sits exactly on `size`.
        let mesh = mesh_chunk(&solid(SIZE, palette::STONE), &alone());

        for position in &mesh.positions {
            for axis in position {
                assert!(
                    (0.0..=SIZE as f32).contains(axis),
                    "vertex {position:?} escapes the chunk"
                );
            }
        }
    }

    #[test]
    fn meshing_is_deterministic() {
        // What makes the output cacheable and this whole test file meaningful: the
        // mesher must not depend on hash order, allocation addresses or anything
        // else that differs between two runs of the same input.
        let mut chunk = air(SIZE);
        for y in 0..SIZE {
            for z in 0..SIZE {
                for x in 0..SIZE {
                    // A shape with structure on all three axes, so a difference in
                    // sweep order would show up.
                    let block = match (x * 7 + y * 13 + z * 3) % 5 {
                        0 => palette::AIR,
                        1 => palette::STONE,
                        2 => palette::DIRT,
                        3 => palette::GRASS,
                        _ => palette::SNOW,
                    };
                    chunk.set(x, y, z, block);
                }
            }
        }

        let first = mesh_chunk(&chunk, &alone());
        let second = mesh_chunk(&chunk, &alone());

        assert!(first.quad_count() > 1000, "the fixture must be non-trivial");
        assert_eq!(
            first, second,
            "the same chunk must mesh to the same buffers"
        );

        // And over the other half of the input, which is voxels too.
        let neighbourhood = surrounded_by(&chunk);
        assert_eq!(
            mesh_chunk(&chunk, &neighbourhood),
            mesh_chunk(&chunk, &neighbourhood),
            "the same chunk and the same neighbours must mesh to the same buffers"
        );
    }

    #[test]
    fn a_chunk_smaller_than_the_servers_still_meshes() {
        // chunk_size is the server's answer, not a constant here: the contract
        // permits 1..=40, and the mesher is indexed by the size it is handed.
        for size in [1usize, 2, 3, 40] {
            let mesh = mesh_chunk(&solid(size, palette::STONE), &alone());
            assert_eq!(mesh.quad_count(), 6, "a solid {size}³ chunk has six walls");
            for quad in 0..6 {
                assert_eq!(quad_area(&mesh, quad), (size * size) as f32);
            }
        }
    }

    #[test]
    fn a_checkerboard_is_the_worst_case_and_still_finishes() {
        // The pathological input: every solid voxel is surrounded by air, so nothing
        // merges and the mask work is entirely wasted. 16 384 solid voxels × 6 faces
        // = 98 304 quads, which is the ceiling on what one chunk can ever cost.
        //
        // The time bound is a tripwire for an accidentally quadratic merge, not a
        // performance target: it is deliberately loose enough to survive a debug
        // build on a loaded CI runner, where this runs unoptimised. The real budget
        // lives in the debug overlay, measured on real terrain.
        const CEILING: Duration = Duration::from_secs(20);

        let mut chunk = air(SIZE);
        for y in 0..SIZE {
            for z in 0..SIZE {
                for x in 0..SIZE {
                    if (x + y + z) % 2 == 0 {
                        chunk.set(x, y, z, palette::STONE);
                    }
                }
            }
        }

        let started = Instant::now();
        let mesh = mesh_chunk(&chunk, &alone());
        let elapsed = started.elapsed();

        assert_eq!(
            mesh.quad_count(),
            SIZE * SIZE * SIZE / 2 * 6,
            "every face of every solid voxel is exposed and none of them merge"
        );
        assert!(
            elapsed < CEILING,
            "the worst case took {elapsed:?}, over the {CEILING:?} tripwire"
        );
    }

    // -----------------------------------------------------------------------
    // Water: the second mesh, and the faces that do and do not reach it
    // -----------------------------------------------------------------------

    /// A column of `blocks` standing on the chunk's floor at its centre, bottom
    /// first. Everything above them is air.
    fn column(size: usize, blocks: &[BlockId]) -> VoxelChunk {
        let mut chunk = air(size);
        for (y, block) in blocks.iter().enumerate() {
            chunk.set(size / 2, y, size / 2, *block);
        }
        chunk
    }

    #[test]
    fn water_over_stone_draws_the_lake_bed_and_one_surface() {
        // Stone at y = 0 with water directly on top of it.
        let chunk = column(SIZE, &[palette::STONE, palette::WATER_FLOW3]);
        let mesh = super::mesh_chunk(&chunk, &alone());

        // The stone keeps all six faces: five meet air and the sixth meets water, which
        // is the whole point.
        assert_eq!(mesh.opaque.quad_count(), 6, "the stone keeps every face");
        let by_normal = quads_by_normal(&mesh.opaque);
        for direction in six_directions() {
            assert_eq!(
                by_normal.get(&direction),
                Some(&1),
                "the stone's {direction:?} face, the top one included — it is under \
                 the water, and it is what the water is looked through at"
            );
        }

        // One voxel of water with air on five sides and stone below: five faces.
        assert_eq!(mesh.water.quad_count(), 5);
        assert_eq!(
            quads_by_normal(&mesh.water).get(&[0, -1, 0]),
            None,
            "the face water shares with the stone under it is nobody's to draw twice"
        );
        assert_eq!(
            quads_by_normal(&mesh.water).get(&[0, 1, 0]),
            Some(&1),
            "the surface, which is the face the player looks down through"
        );
        let top = quads_facing(&mesh.water, [0.0, 1.0, 0.0]);
        assert_eq!(quad_extent(&mesh.water, top[0], 1), (1.375, 1.375));
        let side = quads_facing(&mesh.water, [1.0, 0.0, 0.0]);
        assert_eq!(quad_extent(&mesh.water, side[0], 1), (1.0, 1.375));
    }

    #[test]
    fn unequal_water_levels_draw_only_the_higher_cells_exposed_skirt() {
        let mut chunk = air(SIZE);
        let y = SIZE / 2;
        let z = SIZE / 2;
        let lower_x = SIZE / 2 - 1;
        let higher_x = SIZE / 2;
        chunk.set(lower_x, y, z, palette::WATER_FLOW3);
        chunk.set(higher_x, y, z, palette::WATER_FLOW4);

        let mesh = super::mesh_chunk(&chunk, &alone()).water;
        assert_eq!(quads_facing(&mesh, [0.0, 1.0, 0.0]).len(), 2);

        let shared_plane = higher_x as f32;
        let skirts: Vec<usize> = quads_facing(&mesh, [-1.0, 0.0, 0.0])
            .into_iter()
            .filter(|quad| quad_extent(&mesh, *quad, 0) == (shared_plane, shared_plane))
            .collect();
        assert_eq!(skirts.len(), 1);
        assert_eq!(
            quad_extent(&mesh, skirts[0], 1),
            (y as f32 + 0.375, y as f32 + 0.5)
        );
    }

    #[test]
    fn water_below_water_is_a_full_falling_column() {
        let mut chunk = air(SIZE);
        let x = SIZE / 2;
        let z = SIZE / 2;
        let lower_y = SIZE / 2 - 1;
        chunk.set(x, lower_y, z, palette::WATER_FLOW3);
        chunk.set(x, lower_y + 1, z, palette::WATER_FLOW4);

        let mesh = super::mesh_chunk(&chunk, &alone()).water;
        let lower_sides: Vec<usize> = quads_facing(&mesh, [-1.0, 0.0, 0.0])
            .into_iter()
            .filter(|quad| {
                quad_extent(&mesh, *quad, 0) == (x as f32, x as f32)
                    && quad_extent(&mesh, *quad, 1).0 == lower_y as f32
            })
            .collect();
        assert_eq!(lower_sides.len(), 1);
        assert_eq!(
            quad_extent(&mesh, lower_sides[0], 1),
            (lower_y as f32, lower_y as f32 + 1.0)
        );
    }

    #[test]
    fn water_beside_water_has_no_surface_between_it() {
        // Two voxels of water in one column. The face between them is what the merge
        // exists never to emit, and the four sides merge across both.
        let chunk = column(SIZE, &[palette::WATER, palette::WATER]);
        let mesh = super::mesh_chunk(&chunk, &alone());

        assert!(
            mesh.opaque.is_empty(),
            "there is nothing opaque in the chunk"
        );
        assert_eq!(
            quads_by_normal(&mesh.water),
            BTreeMap::from([
                ([0, 1, 0], 1),
                ([0, -1, 0], 1),
                ([1, 0, 0], 1),
                ([-1, 0, 0], 1),
                ([0, 0, 1], 1),
                ([0, 0, -1], 1),
            ]),
            "one surface, one floor, and four sides merged over both voxels"
        );
    }

    #[test]
    fn water_against_a_solid_block_draws_no_water_face() {
        // Water with stone on every side: the stone draws the faces they share and the
        // water draws nothing. A water quad there would be a sheet buried in the rock.
        let mut chunk = solid(SIZE, palette::STONE);
        chunk.set(SIZE / 2, SIZE / 2, SIZE / 2, palette::WATER);
        let mesh = super::mesh_chunk(&chunk, &surrounded_by(&solid(SIZE, palette::STONE)));

        assert!(
            mesh.water.is_empty(),
            "water sealed in stone has no surface"
        );
        assert_eq!(
            mesh.opaque.quad_count(),
            6,
            "the six stone faces around the pocket, and nothing else in a solid chunk"
        );
    }

    #[test]
    fn a_water_column_at_the_border_draws_its_side_once() {
        // Water filling the -x layer, with an air neighbour across that face. The side
        // face is emitted exactly once, by the chunk the water is in.
        let mut chunk = air(SIZE);
        for y in 0..SIZE {
            for z in 0..SIZE {
                chunk.set(0, y, z, palette::WATER);
            }
        }
        let mesh = super::mesh_chunk(&chunk, &across(0, false, air(SIZE)));

        assert_eq!(
            quads_by_normal(&mesh.water).get(&[-1, 0, 0]),
            Some(&1),
            "one merged quad over the whole border layer, drawn by its own chunk"
        );

        // And the neighbour, meshed from its own side, draws none of it: the water is
        // not its water.
        let neighbour = super::mesh_chunk(&air(SIZE), &across(0, true, chunk));
        assert!(
            neighbour.water.is_empty(),
            "the air chunk across the border draws nobody else's surface"
        );
    }

    #[test]
    fn water_across_a_border_is_culled_against_the_neighbour_that_holds_it() {
        // The rule the opaque sweep already follows, on the other mesh.
        let water = solid(SIZE, palette::WATER);
        let mesh = super::mesh_chunk(&water, &surrounded_by(&water));

        assert!(mesh.is_empty(), "a chunk inside a lake draws nothing");

        // Alone, every border face is emitted — the conservative answer this mesher
        // gives whenever a neighbour has not arrived.
        assert_eq!(mesh_chunk(&water, &alone()).quad_count(), 0);
        assert_eq!(super::mesh_chunk(&water, &alone()).water.quad_count(), 6);
    }

    #[test]
    fn falling_water_across_a_horizontal_and_vertical_border_has_one_owner() {
        let y = SIZE - 1;
        let z = SIZE / 2;
        let mut west = air(SIZE);
        west.set(SIZE - 1, y, z, palette::WATER_FLOW7);
        let mut east = air(SIZE);
        east.set(0, y, z, palette::WATER_FLOW4);
        let mut above_east = air(SIZE);
        above_east.set(0, 0, z, palette::WATER_FLOW1);

        let west_mesh = super::mesh_chunk(
            &west,
            &across_with_above(0, true, east.clone(), above_east.clone()),
        )
        .water;
        assert!(
            quads_facing(&west_mesh, [1.0, 0.0, 0.0])
                .into_iter()
                .all(|quad| quad_extent(&west_mesh, quad, 0) != (SIZE as f32, SIZE as f32)),
            "the lower west cell does not own the shared skirt"
        );

        let mut east_neighbours = across(0, false, west);
        east_neighbours.across[3] = Some(Arc::new(above_east));
        let east_mesh = super::mesh_chunk(&east, &east_neighbours).water;
        let skirts: Vec<_> = quads_facing(&east_mesh, [-1.0, 0.0, 0.0])
            .into_iter()
            .filter(|quad| quad_extent(&east_mesh, *quad, 0) == (0.0, 0.0))
            .collect();
        assert_eq!(skirts.len(), 1);
        assert_eq!(
            quad_extent(&east_mesh, skirts[0], 1),
            (y as f32 + 0.875, SIZE as f32)
        );
    }

    #[test]
    fn mixed_flowing_water_keeps_the_mesher_performance_tripwire() {
        const CEILING: Duration = Duration::from_secs(20);

        let mut chunk = air(SIZE);
        for y in 0..SIZE {
            for z in 0..SIZE {
                for x in 0..SIZE {
                    if (x + y + z) % 2 == 0 {
                        let level = ((x * 7 + y * 3 + z * 5) % 7) as BlockId;
                        chunk.set(x, y, z, palette::WATER_FLOW1 + level);
                    }
                }
            }
        }

        let started = Instant::now();
        let mesh = super::mesh_chunk(&chunk, &alone());
        let elapsed = started.elapsed();

        assert_eq!(
            mesh.water.quad_count(),
            SIZE * SIZE * SIZE / 2 * 6,
            "every isolated flowing voxel exposes all six faces"
        );
        assert!(
            elapsed < CEILING,
            "mixed flowing water took {elapsed:?}, over the {CEILING:?} tripwire"
        );
    }

    #[test]
    fn ice_is_meshed_as_ordinary_ground() {
        // The other half of #446's pair took none of water's behaviour with it: ice is
        // opaque, so it meshes exactly like stone and never reaches the water half.
        let ice = super::mesh_chunk(&single_block(SIZE, palette::ICE), &alone());
        let stone = super::mesh_chunk(&single_block(SIZE, palette::STONE), &alone());

        assert_eq!(ice.opaque.quad_count(), stone.opaque.quad_count());
        assert_eq!(ice.opaque.positions, stone.opaque.positions);
        assert!(ice.water.is_empty());
    }

    #[test]
    fn the_two_halves_never_share_a_quad() {
        // A slab of stone under a slab of water under air, and the invariant that keeps
        // the two draw calls off each other: no quad appears in both meshes.
        let mut chunk = air(SIZE);
        for z in 0..SIZE {
            for x in 0..SIZE {
                chunk.set(x, 0, z, palette::STONE);
                chunk.set(x, 1, z, palette::WATER);
            }
        }
        let mesh = super::mesh_chunk(&chunk, &alone());

        assert!(!mesh.opaque.is_empty() && !mesh.water.is_empty());
        for quad in 0..mesh.opaque.quad_count() {
            let corners = &mesh.opaque.positions[quad * VERTICES_PER_QUAD..][..VERTICES_PER_QUAD];
            for other in 0..mesh.water.quad_count() {
                let against =
                    &mesh.water.positions[other * VERTICES_PER_QUAD..][..VERTICES_PER_QUAD];
                assert_ne!(corners, against, "a quad is in both meshes");
            }
        }
    }
}
