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
//! ## Two masks, three meshes
//!
//! The sweep fills two masks per plane from one pair of samples and produces the first
//! two of the [`ChunkMesh`]'s three [`SurfaceMesh`]es: the opaque surface and the water
//! surface. The third — cover — is not swept at all, because a flower has no coplanar
//! face to merge with its neighbour; [`build_cover`] walks the voxels once and emits a
//! stem and a head for each. The reason each is its own mesh is on [`ChunkMesh`] itself — blending is order-dependent and
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

/// Where the water at one voxel is going, as the renderer is told it.
///
/// **Quantised on purpose.** This is part of the sweep's mask key, and the greedy
/// merge compares mask entries with `Eq` — which a `f32` cannot answer and which a
/// float derived from a division would answer inconsistently for two cells of one
/// straight river run. One step is 1/[`FLOW_STEPS`] of a unit vector: far finer than
/// a ripple pattern can show, and coarse enough that a straight run still merges into
/// one quad while a bend beside still water does not.
///
/// It is a *rendering hint* and nothing else. The server decides where water goes;
/// this is the client reading the ids it was sent so the surface can be drawn moving
/// the way the server already moves a body through it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WaterFlow {
    /// The horizontal direction, in [`FLOW_STEPS`]ths of a unit vector.
    x: i8,
    z: i8,
    /// Whether this water is falling — a non-source flowing voxel with water
    /// directly above it.
    falling: bool,
}

/// How many steps a unit of [`WaterFlow`] is divided into. `i8`'s positive range, so
/// a full-strength component round-trips exactly.
const FLOW_STEPS: f32 = 127.0;

impl WaterFlow {
    /// Water that is going nowhere: a lake, and every non-water face.
    pub const STILL: Self = Self {
        x: 0,
        z: 0,
        falling: false,
    };

    /// Quantises a horizontal direction. `x` and `z` are expected to be a unit
    /// vector or zero; anything longer is clamped by the quantisation itself.
    fn new(x: f32, z: f32, falling: bool) -> Self {
        let step = |value: f32| (value.clamp(-1.0, 1.0) * FLOW_STEPS).round() as i8;
        Self {
            x: step(x),
            z: step(z),
            falling,
        }
    }

    /// The horizontal flow as the vertex attribute carries it: `(x, z)`, each in
    /// `[-1, 1]`, and `(0, 0)` for still water.
    pub fn vector(self) -> [f32; 2] {
        [
            f32::from(self.x) / FLOW_STEPS,
            f32::from(self.z) / FLOW_STEPS,
        ]
    }

    /// The falling bit as the vertex attribute carries it: `(1, 0)` where the water
    /// is falling, `(0, 0)` everywhere else.
    ///
    /// A `vec2` and not a scalar because the attribute it rides in is
    /// `Mesh::ATTRIBUTE_UV_1`, which the mesh pipeline already knows how to hand a
    /// fragment shader — see `to_bevy_mesh` in `render.rs`. The second component is
    /// spare.
    pub fn falling(self) -> [f32; 2] {
        [if self.falling { 1.0 } else { 0.0 }, 0.0]
    }
}

/// One pass's worth of surface, as the parallel attribute buffers a GPU wants.
///
/// Deliberately not a `bevy::mesh::Mesh`: this type crosses a thread boundary and
/// is compared field by field in tests, and both are easier when it is plain data.
/// `render.rs` turns it into a `Mesh` on the main thread, which is the only place
/// that is allowed to.
///
/// **The flow buffers are all-or-nothing.** They are filled for every vertex of a
/// surface whose faces carry a [`WaterFlow`] and left empty for one whose faces do
/// not, so a surface has either `positions.len()` of each or none at all. That is
/// what lets the opaque half — by far the larger one — pay nothing for an attribute
/// only water reads.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SurfaceMesh {
    /// Vertex positions in chunk-local blocks, four per quad, in winding order.
    pub positions: Vec<[f32; 3]>,
    /// One outward face normal per vertex. Flat by construction — every vertex of
    /// a quad shares the quad's normal, and no vertex is shared between quads.
    pub normals: Vec<[f32; 3]>,
    /// Linear RGBA per vertex, from [`palette::linear_rgba`].
    pub colors: Vec<[f32; 4]>,
    /// The horizontal flow per vertex, from [`WaterFlow::vector`]. Empty on a
    /// surface whose faces carry no flow.
    pub flow: Vec<[f32; 2]>,
    /// The falling bit per vertex, from [`WaterFlow::falling`]. Empty exactly when
    /// [`Self::flow`] is.
    pub falling: Vec<[f32; 2]>,
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
    fn push_quad(
        &mut self,
        corners: [[f32; 3]; 4],
        normal: [f32; 3],
        color: [f32; 4],
        flow: Option<WaterFlow>,
    ) {
        let base = self.positions.len() as u32;

        self.positions.extend_from_slice(&corners);
        self.normals
            .extend(std::iter::repeat_n(normal, VERTICES_PER_QUAD));
        self.colors
            .extend(std::iter::repeat_n(color, VERTICES_PER_QUAD));
        if let Some(flow) = flow {
            self.flow
                .extend(std::iter::repeat_n(flow.vector(), VERTICES_PER_QUAD));
            self.falling
                .extend(std::iter::repeat_n(flow.falling(), VERTICES_PER_QUAD));
        }
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
    /// Every cover voxel's stem and head, from [`build_cover`].
    ///
    /// **The third surface, and it is not produced by the sweep at all.** The sweep
    /// exists to merge coplanar faces of adjacent voxels, and a flower has no coplanar
    /// faces to merge: its geometry is a cross and a small cube inside a voxel that is
    /// otherwise empty. Two flowers side by side are two flowers, so there is nothing a
    /// mask could join and every reason not to pay for a third one per plane.
    ///
    /// It is its own half rather than more quads in [`Self::opaque`] because its
    /// material differs: a stem is a plane, so it is seen from both sides and is drawn
    /// with no back-face culling. That is a pipeline, and a pipeline is an entity.
    pub cover: SurfaceMesh,
}

impl ChunkMesh {
    /// How many merged quads the chunk holds across all three halves.
    pub fn quad_count(&self) -> usize {
        self.opaque.quad_count() + self.water.quad_count() + self.cover.quad_count()
    }

    /// Whether there is anything at all to draw. Every half empty, which is the
    /// all-air chunk and the wholly-buried one.
    pub fn is_empty(&self) -> bool {
        self.opaque.is_empty() && self.water.is_empty() && self.cover.is_empty()
    }
}

/// How wide the two crossing blades of a cover stem are, in blocks.
const COVER_STEM_WIDTH: f32 = 0.3;

/// How tall a stem stands above the bottom of its voxel, in blocks.
const COVER_STEM_HEIGHT: f32 = 0.5;

/// The edge of the cube that sits on the stem, in blocks. It spans
/// `[COVER_STEM_HEIGHT, COVER_STEM_HEIGHT + COVER_HEAD_SIZE]` vertically, so a flower
/// occupies the lower three quarters of its voxel and never crosses into the next one.
const COVER_HEAD_SIZE: f32 = 0.25;

/// How many quads one cover voxel contributes: two stem blades and the head's six
/// faces.
///
/// Test-only for the reason [`palette::PALETTE`] is: production code emits the quads
/// rather than counting them, and a constant nothing reads is a claim nothing checks.
/// Here it is read by the assertions that pin the geometry.
#[cfg(test)]
const QUADS_PER_COVER: usize = 8;

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
    /// The flow the water behind this face carries, and part of the key too: a river
    /// bend beside still water must not merge into one quad that slides both ways.
    /// `None` on every opaque face — the opaque surface carries no flow attribute at
    /// all, which is what keeps [`SurfaceMesh`]'s all-or-nothing invariant true.
    flow: Option<WaterFlow>,
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

    build_cover(&mut mesh.cover, chunk);

    mesh
}

/// Fills the cover half: one stem and one head per [`palette::is_cover`] voxel.
///
/// A whole pass over the chunk rather than a third mask, because there is nothing here
/// for a mask to do — see [`ChunkMesh::cover`]. It reads no neighbour either: a
/// flower's geometry is entirely inside its own voxel, which is why
/// `ChunkStore::apply_block` needs no new remesh rule for one. Breaking a flower on a
/// chunk's edge remeshes that chunk and nothing across the border.
///
/// The iteration order is y, then z, then x — fixed, like the sweep's, so the same
/// voxels produce byte-identical buffers every time.
fn build_cover(mesh: &mut SurfaceMesh, chunk: &VoxelChunk) {
    let size = chunk.size();
    let stem = [
        palette::STEM_LINEAR[0],
        palette::STEM_LINEAR[1],
        palette::STEM_LINEAR[2],
        1.0,
    ];

    for y in 0..size {
        for z in 0..size {
            for x in 0..size {
                let block = chunk.block([x, y, z]);
                if !palette::is_cover(block) {
                    continue;
                }

                let (x, y, z) = (x as f32, y as f32, z as f32);
                // The middle of the voxel's floor: where the stem stands.
                let base = [x + 0.5, y, z + 0.5];
                let half = COVER_STEM_WIDTH / 2.0;
                let top = y + COVER_STEM_HEIGHT;

                // Two blades crossing on the voxel's vertical axis, each a single quad:
                // the material draws both sides, so a second wound the other way would
                // be a coincident copy fighting the depth buffer for nothing.
                push_blade(mesh, base, half, top, 0, stem);
                push_blade(mesh, base, half, top, 2, stem);

                // The head, centred over the crossing. Its colour is the block's, from
                // the one function that owns what an id looks like.
                let head = COVER_HEAD_SIZE / 2.0;
                push_box(
                    mesh,
                    [base[0] - head, top, base[2] - head],
                    [base[0] + head, top + COVER_HEAD_SIZE, base[2] + head],
                    palette::linear_rgba(block),
                );
            }
        }
    }
}

/// One vertical blade of a stem: a quad `2 * half` wide along `span`, rising from
/// `base` to `top`, facing along the other horizontal axis.
///
/// `span` is 0 or 2, and `2 - span` is therefore the axis the blade faces. Wound
/// through the same right-handed `(u, v, axis)` frame [`quad_corners`] uses, so the
/// front is the side the named normal points at and the lighting agrees with it.
fn push_blade(
    mesh: &mut SurfaceMesh,
    base: [f32; 3],
    half: f32,
    top: f32,
    span: usize,
    color: [f32; 4],
) {
    let facing = 2 - span;
    let u = (facing + 1) % 3;

    let mut origin = base;
    origin[span] -= half;

    let mut across = [0.0f32; 3];
    across[span] = 2.0 * half;
    let up = [0.0, top - base[1], 0.0];
    // Whichever of u and v is the vertical axis takes the rise; the other takes the
    // width. That is the whole of what differs between the two blades.
    let (along_u, along_v) = if u == 1 { (up, across) } else { (across, up) };

    let add = |a: [f32; 3], b: [f32; 3]| [a[0] + b[0], a[1] + b[1], a[2] + b[2]];
    let far = add(add(origin, along_u), along_v);
    mesh.push_quad(
        [origin, add(origin, along_u), far, add(origin, along_v)],
        normal(facing, true),
        color,
        None,
    );
}

/// The six faces of an axis-aligned box, wound the way [`quad_corners`] winds a merged
/// quad and for the same reason: (u, v, axis) is right-handed, so walking
/// `origin -> +u -> +u+v -> +v` is counter-clockwise seen from `+axis`.
fn push_box(mesh: &mut SurfaceMesh, min: [f32; 3], max: [f32; 3], color: [f32; 4]) {
    let add = |a: [f32; 3], b: [f32; 3]| [a[0] + b[0], a[1] + b[1], a[2] + b[2]];

    for axis in 0..3 {
        let u = (axis + 1) % 3;
        let v = (axis + 2) % 3;
        let mut along_u = [0.0f32; 3];
        along_u[u] = max[u] - min[u];
        let mut along_v = [0.0f32; 3];
        along_v[v] = max[v] - min[v];

        for positive in [false, true] {
            let mut origin = min;
            origin[axis] = if positive { max[axis] } else { min[axis] };
            let far = add(add(origin, along_u), along_v);
            let corners = if positive {
                [origin, add(origin, along_u), far, add(origin, along_v)]
            } else {
                [origin, add(origin, along_v), far, add(origin, along_u)]
            };
            mesh.push_quad(corners, normal(axis, positive), color, None);
        }
    }
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
                        flow: None,
                    }),
                    (false, true) if above_is_ours => Some(Face {
                        block: positive,
                        positive: false,
                        geometry: FaceGeometry::Full,
                        flow: None,
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
            )
            .map(|face| Face {
                // The face belongs to the water voxel behind it: the one on the
                // negative side when the normal points along +axis, the one on the
                // positive side otherwise. Both are inside *this* chunk — `water_face`
                // emits nothing whose own side is the neighbour's — so the flow is
                // derived from a cell this sweep can address, and only for a face that
                // is actually emitted rather than for every cell of every plane.
                flow: Some(flow_at(chunk, neighbours, {
                    let mut cell = [0usize; 3];
                    cell[axis] = if face.positive { plane - 1 } else { plane };
                    cell[u] = i;
                    cell[v] = j;
                    cell
                })),
                ..face
            });
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
            flow: None,
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
            flow: None,
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
            flow: None,
        }),
        (true, true) if positive_level > negative_level && positive_is_ours => Some(Face {
            block: palette::WATER,
            positive: false,
            geometry: FaceGeometry::WaterSide {
                bottom: negative_level,
                top: positive_level,
            },
            flow: None,
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

/// The four horizontal steps a flowing voxel's gradient is summed over, as
/// `(axis, step)`. `x` then `z`, negative before positive — a fixed order, because
/// the sum has to be the same value every time this chunk is meshed.
const HORIZONTAL_STEPS: [(usize, isize); 4] = [(0, -1), (0, 1), (2, -1), (2, 1)];

/// Where the water at `cell` is going, as the renderer is told it.
///
/// **The client's mirror of the server's `FlowDirection`, and a rendering hint only.**
/// A `WaterCurrent*` id already *is* a direction, so it is taken verbatim. A
/// `WaterFlowN` id carries a level instead, and water runs downhill: the direction is
/// the normalised sum, over the four horizontal neighbours, of
/// `(level − neighbourLevel) × step`, which points away from every shallower
/// neighbour and towards none that is deeper. Plain `Water` is a source and a source
/// is still.
///
/// Two neighbours contribute nothing, for the same reason: they are not somewhere
/// this water can go. A **solid** neighbour is a wall, and a **source** neighbour
/// (plain water or a current, both full height) is the body this trickle came out of.
/// Air is not skipped — it is level 0, the steepest drop there is, which is what makes
/// water pour off a ledge rather than sit on it.
///
/// A neighbour chunk this session has not been sent reads as air, exactly as it does
/// in the sweep above, and for the same reason: over-drawing the motion is the
/// direction to be wrong in while the data is incomplete, and `render.rs` remeshes the
/// chunk when the neighbour arrives.
fn flow_at(chunk: &VoxelChunk, neighbours: &Neighbours, cell: [usize; 3]) -> WaterFlow {
    let block = chunk.block(cell);

    // A current is the server's own answer. It is a source, so it never falls.
    let (current_x, current_z) = palette::current_of(block);
    if current_x != 0 || current_z != 0 {
        return WaterFlow::new(f32::from(current_x), f32::from(current_z), false);
    }

    let level = palette::water_level(block);
    // Level 0 is not water at all; level 8 here is plain `Water`, the source.
    if level == 0 || level == 8 {
        return WaterFlow::STILL;
    }

    let mut sum = [0.0f32; 2];
    for (axis, step) in HORIZONTAL_STEPS {
        let neighbour = stepped_block(chunk, neighbours, cell, axis, step);
        if palette::is_solid(neighbour) {
            continue;
        }
        let neighbour_level = palette::water_level(neighbour);
        if neighbour_level == 8 {
            continue;
        }
        let drop = f32::from(level) - f32::from(neighbour_level);
        sum[axis / 2] += drop * step as f32;
    }

    let length = sum[0].hypot(sum[1]);
    let (x, z) = if length > 0.0 {
        (sum[0] / length, sum[1] / length)
    } else {
        (0.0, 0.0)
    };

    // Falling is the same question `effective_water_level` asks to draw the column at
    // full height: a non-source flowing voxel with water directly above it is a
    // waterfall rather than a puddle.
    let falling = palette::is_water(stepped_block(chunk, neighbours, cell, 1, 1));
    WaterFlow::new(x, z, falling)
}

/// The block one step along `axis` from `cell`, read from the chunk across that face
/// when the step leaves this one. Air when there is no such chunk.
fn stepped_block(
    chunk: &VoxelChunk,
    neighbours: &Neighbours,
    cell: [usize; 3],
    axis: usize,
    step: isize,
) -> BlockId {
    let size = chunk.size();
    let target = cell[axis] as isize + step;
    let mut probe = cell;
    if target >= 0 && (target as usize) < size {
        probe[axis] = target as usize;
        return chunk.block(probe);
    }
    let positive = step > 0;
    let Some(across) = neighbours.across(axis, positive, size) else {
        return palette::AIR;
    };
    probe[axis] = if positive { 0 } else { size - 1 };
    across.block(probe)
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
                face.flow,
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

    // -----------------------------------------------------------------------
    // Flow: what the water shader is told, and what it costs the merge
    // -----------------------------------------------------------------------

    /// The flow of one voxel with a chosen neighbourhood: `around` is
    /// `[-x, +x, -z, +z]` and `above` sits directly on top of it.
    ///
    /// Every neighbour is written explicitly, because the interesting cases are all
    /// about which of them contributes: an unset cell is air, and air is the steepest
    /// drop there is.
    fn flow_of(block: BlockId, around: [BlockId; 4], above: BlockId) -> WaterFlow {
        let mut chunk = air(SIZE);
        let (x, y, z) = (SIZE / 2, SIZE / 2, SIZE / 2);
        chunk.set(x, y, z, block);
        chunk.set(x - 1, y, z, around[0]);
        chunk.set(x + 1, y, z, around[1]);
        chunk.set(x, y, z - 1, around[2]);
        chunk.set(x, y, z + 1, around[3]);
        chunk.set(x, y + 1, z, above);
        super::flow_at(&chunk, &alone(), [x, y, z])
    }

    /// Water on every side of the subject, at the same level, so nothing but the
    /// subject's own id decides its flow.
    const FLAT: [BlockId; 4] = [
        palette::WATER_FLOW4,
        palette::WATER_FLOW4,
        palette::WATER_FLOW4,
        palette::WATER_FLOW4,
    ];

    #[test]
    fn a_current_flows_along_its_own_axis_and_asks_nobody() {
        // The server already answered for these four ids, so the gradient is not
        // consulted: a current surrounded by deeper water still points where it says.
        for (block, wanted) in [
            (palette::WATER_CURRENT_XPOS, [1.0, 0.0]),
            (palette::WATER_CURRENT_XNEG, [-1.0, 0.0]),
            (palette::WATER_CURRENT_ZPOS, [0.0, 1.0]),
            (palette::WATER_CURRENT_ZNEG, [0.0, -1.0]),
        ] {
            let flow = flow_of(block, FLAT, palette::WATER);
            assert_eq!(
                flow.vector(),
                wanted,
                "current {block} points the wrong way"
            );
            assert_eq!(
                flow.falling(),
                [0.0, 0.0],
                "a current is a source, and a source never falls"
            );
        }
    }

    #[test]
    fn plain_water_is_still_however_deep_the_water_beside_it_is() {
        // A lake is a source. It has no gradient to run down, and the shader draws it
        // shimmering in place rather than sliding.
        let flow = flow_of(palette::WATER, [palette::AIR; 4], palette::AIR);
        assert_eq!(flow.vector(), [0.0, 0.0]);
        assert_eq!(flow.falling(), [0.0, 0.0]);
        assert_eq!(flow, WaterFlow::STILL);
    }

    #[test]
    fn a_flowing_voxel_runs_down_its_own_gradient() {
        // One shallower neighbour: straight at it. Air is level 0, which is why water
        // pours off a ledge rather than sitting on it.
        let east = flow_of(
            palette::WATER_FLOW4,
            [
                palette::WATER_FLOW4,
                palette::WATER_FLOW1,
                palette::WATER_FLOW4,
                palette::WATER_FLOW4,
            ],
            palette::AIR,
        );
        assert_eq!(east.vector(), [1.0, 0.0]);

        let off_the_ledge = flow_of(
            palette::WATER_FLOW4,
            [
                palette::WATER_FLOW4,
                palette::WATER_FLOW4,
                palette::AIR,
                palette::WATER_FLOW4,
            ],
            palette::AIR,
        );
        assert_eq!(off_the_ledge.vector(), [0.0, -1.0]);

        // Two equal drops at right angles: the diagonal between them, normalised, and
        // the quantisation is what makes the two components compare equal.
        let corner = flow_of(
            palette::WATER_FLOW4,
            [
                palette::WATER_FLOW4,
                palette::WATER_FLOW2,
                palette::WATER_FLOW4,
                palette::WATER_FLOW2,
            ],
            palette::AIR,
        );
        let [x, z] = corner.vector();
        assert_eq!(x, z, "a symmetric corner must flow at 45 degrees");
        assert!(
            (x.hypot(z) - 1.0).abs() < 2.0 / super::FLOW_STEPS,
            "the flow is not a unit vector: {:?}",
            corner.vector()
        );
    }

    #[test]
    fn a_wall_and_a_source_are_not_places_the_water_can_go() {
        // A solid neighbour is a wall; a source neighbour is the body this trickle came
        // out of. Neither contributes, so a voxel with nothing else around it is still
        // rather than pushed into one of them.
        let penned = flow_of(
            palette::WATER_FLOW4,
            [
                palette::STONE,
                palette::WATER,
                palette::ICE,
                palette::WATER_CURRENT_XPOS,
            ],
            palette::AIR,
        );
        assert_eq!(penned.vector(), [0.0, 0.0]);

        // And a wall on one side does not make the water climb the other: the open side
        // is what it runs down.
        let against_a_wall = flow_of(
            palette::WATER_FLOW4,
            [
                palette::STONE,
                palette::WATER_FLOW1,
                palette::WATER_FLOW4,
                palette::WATER_FLOW4,
            ],
            palette::AIR,
        );
        assert_eq!(against_a_wall.vector(), [1.0, 0.0]);
    }

    #[test]
    fn falling_is_a_non_source_with_water_directly_above_it() {
        // The same question `effective_water_level` asks to draw the column at full
        // height, and the bit the shader streaks downward on.
        assert_eq!(
            flow_of(palette::WATER_FLOW4, FLAT, palette::WATER).falling(),
            [1.0, 0.0]
        );
        assert_eq!(
            flow_of(palette::WATER_FLOW4, FLAT, palette::WATER_FLOW2).falling(),
            [1.0, 0.0]
        );
        assert_eq!(
            flow_of(palette::WATER_FLOW4, FLAT, palette::AIR).falling(),
            [0.0, 0.0]
        );
        assert_eq!(
            flow_of(palette::WATER_FLOW4, FLAT, palette::STONE).falling(),
            [0.0, 0.0]
        );
        // Sources never fall, whatever is on top of them.
        for source in [palette::WATER, palette::WATER_CURRENT_ZNEG] {
            assert_eq!(flow_of(source, FLAT, palette::WATER).falling(), [0.0, 0.0]);
        }
    }

    #[test]
    fn a_chunk_this_session_has_not_been_sent_reads_as_air_here_too() {
        // The sweep's own rule, applied to the gradient: an absent neighbour is air, so
        // the water at the border flows out of the chunk rather than standing still.
        // `render.rs` remeshes when the neighbour arrives, which is what makes that a
        // wait rather than a mistake.
        let mut chunk = air(SIZE);
        chunk.set(0, 1, 1, palette::WATER_FLOW4);
        chunk.set(1, 1, 1, palette::WATER_FLOW4);
        chunk.set(0, 1, 0, palette::WATER_FLOW4);
        chunk.set(0, 1, 2, palette::WATER_FLOW4);
        assert_eq!(
            super::flow_at(&chunk, &alone(), [0, 1, 1]).vector(),
            [-1.0, 0.0],
            "the absent -x neighbour must read as air"
        );

        // And the neighbour that has arrived is read, which is the same cell answering
        // differently once there is something across the border.
        let mut west = air(SIZE);
        west.set(SIZE - 1, 1, 1, palette::WATER);
        assert_eq!(
            super::flow_at(&chunk, &across(0, false, west), [0, 1, 1]).vector(),
            [0.0, 0.0],
            "a source across the border is skipped, and nothing else pushes"
        );
    }

    #[test]
    fn one_flower_is_a_stem_and_a_head_in_the_cover_half_and_nothing_else() {
        // #551's whole geometry, in the one chunk that isolates it. The grass keeps its
        // top face because a flower is not opaque, so the opaque half is a solid chunk's
        // six walls and not five; the water half never hears about cover at all.
        let mut chunk = solid(SIZE, palette::GRASS);
        chunk.set(4, 5, 6, palette::AIR);
        let bare = super::mesh_chunk(&chunk, &alone());
        chunk.set(4, 5, 6, palette::FLOWER_RED);
        let mesh = super::mesh_chunk(&chunk, &alone());

        assert_eq!(
            mesh.opaque, bare.opaque,
            "a flower is see-through, so the opaque sweep must not notice it"
        );
        assert!(mesh.water.is_empty());
        assert_eq!(mesh.cover.quad_count(), QUADS_PER_COVER);
        assert_eq!(
            mesh.quad_count(),
            mesh.opaque.quad_count() + QUADS_PER_COVER
        );
        assert!(!mesh.is_empty());

        // Two blades and six head faces, and the head is a closed box: one quad in each
        // of the six directions.
        let by_normal = quads_by_normal(&mesh.cover);
        assert_eq!(
            by_normal.get(&[1, 0, 0]),
            Some(&2),
            "+x head face and a blade"
        );
        assert_eq!(
            by_normal.get(&[0, 0, 1]),
            Some(&2),
            "+z head face and a blade"
        );
        for direction in [[-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, -1]] {
            assert_eq!(by_normal.get(&direction), Some(&1), "{direction:?}");
        }

        // The declared normal and the winding agree on every quad, which is how a
        // face drawn inside-out is caught without a screen. `cull_mode: None` would
        // hide the mistake in the picture and not in the lighting.
        for quad in 0..mesh.cover.quad_count() {
            let winding = winding_normal(&mesh.cover, quad);
            let declared = mesh.cover.normals[quad * VERTICES_PER_QUAD];
            let length =
                (winding[0] * winding[0] + winding[1] * winding[1] + winding[2] * winding[2])
                    .sqrt();
            let unit = [
                winding[0] / length,
                winding[1] / length,
                winding[2] / length,
            ];
            for axis in 0..3 {
                assert!(
                    (unit[axis] - declared[axis]).abs() < 1e-5,
                    "cover quad {quad} is wound against its normal: {unit:?} vs {declared:?}"
                );
            }
        }

        // Everything stays inside the voxel it grew in, and nothing reaches its lid —
        // which is what makes cover a per-voxel surface with no neighbour to consult.
        assert_eq!(quad_extent(&mesh.cover, 0, 1).0, 5.0);
        for quad in 0..mesh.cover.quad_count() {
            for (axis, (low, high)) in [(0, (4.0, 5.0)), (1, (5.0, 5.75)), (2, (6.0, 7.0))] {
                let (minimum, maximum) = quad_extent(&mesh.cover, quad, axis);
                assert!(
                    minimum >= low && maximum <= high,
                    "cover quad {quad} leaves its voxel on axis {axis}: {minimum}..{maximum}"
                );
            }
        }

        // The cover half carries no flow, which is what keeps `SurfaceMesh`'s
        // all-or-nothing invariant true for it and what keeps `to_bevy_mesh` from
        // inserting a UV attribute nothing reads.
        assert!(mesh.cover.flow.is_empty() && mesh.cover.falling.is_empty());

        // The head is the block's colour and the stem is not, which is the whole of what
        // makes a flower read as a flower rather than as a coloured smear.
        let stem = [
            palette::STEM_LINEAR[0],
            palette::STEM_LINEAR[1],
            palette::STEM_LINEAR[2],
            1.0,
        ];
        let head = palette::linear_rgba(palette::FLOWER_RED);
        assert_ne!(stem, head);
        let mut heads = 0;
        for quad in 0..mesh.cover.quad_count() {
            let colour = mesh.cover.colors[quad * VERTICES_PER_QUAD];
            assert!(colour == stem || colour == head, "quad {quad}: {colour:?}");
            heads += usize::from(colour == head);
        }
        assert_eq!(heads, 6, "the head is the six-faced part");
    }

    #[test]
    fn every_cover_id_grows_its_own_flower_and_a_bare_chunk_grows_none() {
        // Per voxel and not by mask: three flowers in a row do not merge into one, and
        // each takes its own head colour.
        let mut chunk = air(SIZE);
        for (offset, block) in [
            palette::FLOWER_RED,
            palette::FLOWER_YELLOW,
            palette::FLOWER_BLUE,
        ]
        .into_iter()
        .enumerate()
        {
            chunk.set(4 + offset, 5, 6, block);
        }
        let mesh = super::mesh_chunk(&chunk, &alone());

        assert!(
            mesh.opaque.is_empty(),
            "cover is drawn by nothing but itself"
        );
        assert_eq!(mesh.cover.quad_count(), 3 * QUADS_PER_COVER);
        for block in [
            palette::FLOWER_RED,
            palette::FLOWER_YELLOW,
            palette::FLOWER_BLUE,
        ] {
            assert!(mesh.cover.colors.contains(&palette::linear_rgba(block)));
        }

        // And a chunk with no cover in it gets an empty third half rather than an empty
        // buffer of the wrong length.
        let none = super::mesh_chunk(&solid(SIZE, palette::STONE), &alone());
        assert!(none.cover.is_empty());
        assert_eq!(none.cover.quad_count(), 0);
    }

    #[test]
    fn a_flower_on_a_shore_leaves_the_lake_surface_whole() {
        // The one thing cover changes about the water sweep, and it needed no new mask
        // arm: `is_opaque` is false for a flower, so the arm that already reads
        // "see-through" rather than "air" draws the surface against it. A flower that
        // was opaque would punch a hole in the lake beside it.
        let mut chunk = air(SIZE);
        for z in 0..SIZE {
            for x in 0..SIZE {
                chunk.set(x, 0, z, palette::WATER);
            }
        }
        let lake = super::mesh_chunk(&chunk, &alone()).water.quad_count();
        chunk.set(4, 1, 6, palette::FLOWER_BLUE);
        let shore = super::mesh_chunk(&chunk, &alone());

        assert_eq!(
            shore.water.quad_count(),
            lake,
            "the flower above the water changed the surface"
        );
        assert_eq!(shore.cover.quad_count(), QUADS_PER_COVER);
    }

    #[test]
    fn only_the_water_surface_carries_the_flow_attributes() {
        // The invariant on `SurfaceMesh`: all-or-nothing, per surface. The opaque half
        // is by far the larger one and pays nothing for an attribute only water reads.
        let mut chunk = air(SIZE);
        for z in 0..SIZE {
            for x in 0..SIZE {
                chunk.set(x, 0, z, palette::STONE);
                chunk.set(x, 1, z, palette::WATER_CURRENT_XPOS);
            }
        }
        let mesh = super::mesh_chunk(&chunk, &alone());

        assert!(!mesh.opaque.is_empty() && !mesh.water.is_empty());
        assert!(mesh.opaque.flow.is_empty() && mesh.opaque.falling.is_empty());
        assert_eq!(mesh.water.flow.len(), mesh.water.positions.len());
        assert_eq!(mesh.water.falling.len(), mesh.water.positions.len());

        // Flat per quad, exactly like the normal and the colour: four vertices, one
        // value, because no vertex is shared between quads.
        for quad in 0..mesh.water.quad_count() {
            let vertices = &mesh.water.flow[quad * VERTICES_PER_QUAD..][..VERTICES_PER_QUAD];
            assert!(vertices.iter().all(|value| *value == vertices[0]));
        }
        assert!(
            mesh.water.flow.contains(&[1.0, 0.0]),
            "the whole layer is a +x current and no quad says so"
        );
    }

    /// A chunk whose floor is one layer of still water, with `current` written across
    /// the row at `z = 1` over the `x` range given.
    fn water_floor_with_current(current: &[(std::ops::Range<usize>, BlockId)]) -> VoxelChunk {
        let mut chunk = air(SIZE);
        for z in 0..SIZE {
            for x in 0..SIZE {
                chunk.set(x, 0, z, palette::WATER);
            }
        }
        for (range, block) in current {
            for x in range.clone() {
                chunk.set(x, 0, 1, *block);
            }
        }
        chunk
    }

    #[test]
    fn a_straight_run_merges_and_a_bend_beside_still_water_does_not() {
        // The whole cost of putting the flow in the mask key, measured on the surface a
        // player actually sees. Every cell here is full-height water, so nothing but the
        // flow can separate two quads.
        let up = [0.0, 1.0, 0.0];

        let still = super::mesh_chunk(&water_floor_with_current(&[]), &alone()).water;
        assert_eq!(
            quads_facing(&still, up).len(),
            1,
            "a lake surface is one quad"
        );

        // One straight run of current across the whole chunk: the run is a quad of its
        // own, and the still water on either side of it is one quad each.
        let straight = super::mesh_chunk(
            &water_floor_with_current(&[(0..SIZE, palette::WATER_CURRENT_XPOS)]),
            &alone(),
        )
        .water;
        assert_eq!(
            quads_facing(&straight, up).len(),
            3,
            "a straight run must still merge into one quad"
        );

        // The same run, bent halfway along: the two halves push different ways and
        // cannot share a quad.
        let bend = super::mesh_chunk(
            &water_floor_with_current(&[
                (0..SIZE / 2, palette::WATER_CURRENT_XPOS),
                (SIZE / 2..SIZE, palette::WATER_CURRENT_ZPOS),
            ]),
            &alone(),
        )
        .water;
        assert_eq!(
            quads_facing(&bend, up).len(),
            4,
            "a bend must not merge into the run it turns out of"
        );
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
