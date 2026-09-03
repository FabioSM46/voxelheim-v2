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
//! surface. The third — cover — is not swept at all, because a plant has no coplanar
//! face to merge with its neighbour; [`build_cover`] walks the voxels once and grows a
//! flower or a bramble in each voxel [`palette::is_cover`] answers for. The reason each is
//! its own mesh is on [`ChunkMesh`] itself — blending is order-dependent and
//! Bevy sorts per entity, so the two have to be separate draws. The face rules are
//! on [`build_masks`]; the greedy merge below is shared and knows about neither.
//!
//! ## Corners are darkened, and it is the merge key that pays for it
//!
//! Lighting is a function of the face normal, and every face pointing the same way is
//! therefore exactly the same colour whatever is in front of it. A staircase of one
//! block type seen head-on is one flat rectangle. [`Occlusion`] is the answer: each of
//! a quad's four corners counts the opaque voxels that touch it on the side the face
//! points at, and darkens that vertex's colour.
//!
//! It rides in the vertex colour that was already there — [`SurfaceMesh::colors`], which
//! `render.rs` uploads as `Mesh::ATTRIBUTE_COLOR` — so it costs no attribute, no
//! material and no shader. Four vertices per quad, never shared, is what makes that
//! possible at all.
//!
//! **And it is part of the mask key**, so two faces whose corners are lit differently do
//! not merge. That raises the quad count, deliberately: the merge is exactly what
//! erased the seam this is drawn to show.
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

/// How much of the sky each of a quad's four corners is cut off from, as the count of
/// opaque voxels that touch it on the side the face points at.
///
/// **Per-vertex ambient occlusion, and the reason it can exist at all is
/// [`VERTICES_PER_QUAD`]**: four vertices per quad, never shared, so the four corners of
/// one quad can carry four different colours without any other quad disagreeing.
///
/// Three voxels touch a corner on the outward side — the two that share an edge with the
/// face and the one diagonally across from it — so a level runs 0 (nothing beside it) to
/// 3 (a corner buried in rock). The two edge-adjacent voxels being solid is level 3 on
/// its own: the diagonal is behind them and cannot be seen whether or not it is there,
/// which is the standard rule and the one that keeps an inside corner from reading
/// lighter than the wall beside it.
///
/// **It is part of the sweep's mask key**, for exactly the reason [`WaterFlow`] is: the
/// greedy merge compares mask entries with `Eq`, and two faces whose corners are lit
/// differently must not be fused into one quad that has to pick one of the two answers.
/// That costs quads — it is why a step in a staircase stops merging into the step beside
/// it — and the cost is the feature, since the merge is what erased the seam.
///
/// Quantised to a level rather than held as a float for the same reason `WaterFlow` is
/// quantised: a mask key has to answer `Eq`, and a float derived from a division cannot.
///
/// Corners are stored in the plane's own `(u, v)` frame — `(0,0)`, `(1,0)`, `(1,1)`,
/// `(0,1)` — which is the winding order [`quad_corners`] emits for a face pointing along
/// `+axis`. A face pointing the other way walks the same four corners in the other
/// direction; [`Self::shade`] is the one place that knows it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Occlusion([u8; VERTICES_PER_QUAD]);

/// The brightness of a vertex at each occlusion level.
///
/// One occluding voxel takes a fifth of the light and the next takes a fifth of what is
/// left, so the curve is `0.8ⁿ` written out rather than computed — a `powi` per vertex
/// for four values that never change. The multiplication happens in **linear** space,
/// which is where [`palette::linear_rgba`] already works, so a step here is a smaller
/// perceptual step than the same number applied to an sRGB colour would be.
const OCCLUSION_SHADE: [f32; 4] = [1.0, 0.8, 0.64, 0.512];

/// Which way to step, in the plane's `(u, v)` frame, to reach the voxels that touch each
/// corner. Same order as [`Occlusion`]'s corners.
const CORNER_STEPS: [(isize, isize); VERTICES_PER_QUAD] = [(-1, -1), (1, -1), (1, 1), (-1, 1)];

impl Occlusion {
    /// A face with nothing beside it: every corner fully lit.
    ///
    /// What every water and cover quad carries — the occlusion is the opaque sweep's and
    /// nothing else's — and what an opaque face standing alone in the air computes.
    pub const NONE: Self = Self([0; VERTICES_PER_QUAD]);

    /// `color` darkened per corner, in the winding order [`quad_corners`] emits for a
    /// face whose normal points along `+axis` when `positive`.
    ///
    /// Alpha is carried through untouched: occlusion is a shading term, and darkening a
    /// surface must not also make it see-through.
    fn shade(self, color: [f32; 4], positive: bool) -> [[f32; 4]; VERTICES_PER_QUAD] {
        // `quad_corners` walks origin → +u → +u+v → +v for a positive face and
        // origin → +v → +u+v → +u for a negative one, which is this array reversed
        // after its first entry.
        let winding: [usize; VERTICES_PER_QUAD] =
            if positive { [0, 1, 2, 3] } else { [0, 3, 2, 1] };
        winding.map(|corner| {
            let shade = OCCLUSION_SHADE[usize::from(self.0[corner])];
            [
                color[0] * shade,
                color[1] * shade,
                color[2] * shade,
                color[3],
            ]
        })
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
    /// Linear RGBA per vertex, from [`palette::linear_rgba`], with its RGB darkened by
    /// the vertex's own [`Occlusion`] on the opaque half. Four vertices of one quad can
    /// therefore differ, which is the whole of how an edge becomes visible without a
    /// second attribute, a second material or a shader.
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
        self.push_shaded_quad(corners, normal, [color; VERTICES_PER_QUAD], flow);
    }

    /// [`Self::push_quad`] with a colour per corner rather than one for the quad.
    ///
    /// The opaque sweep is the only caller that needs it: its four corners carry four
    /// [`Occlusion`] levels. Water and cover push one colour four times through
    /// [`Self::push_quad`], which is what leaves them untouched by this.
    fn push_shaded_quad(
        &mut self,
        corners: [[f32; 3]; 4],
        normal: [f32; 3],
        colors: [[f32; 4]; VERTICES_PER_QUAD],
        flow: Option<WaterFlow>,
    ) {
        let base = self.positions.len() as u32;

        self.positions.extend_from_slice(&corners);
        self.normals
            .extend(std::iter::repeat_n(normal, VERTICES_PER_QUAD));
        self.colors.extend_from_slice(&colors);
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
    /// Every plant, grown one voxel at a time by [`build_cover`]: a flower's stem, leaves
    /// and corolla, and a bush's foliage, twigs and flower specks.
    ///
    /// **The third surface, and it is not produced by the sweep at all.** The sweep
    /// exists to merge coplanar faces of adjacent voxels, and a plant has none to merge:
    /// a flower is a cross of blades under a corolla and a bush is overlapping clumps
    /// with crossed detail ribbons, both inside a voxel nothing else is drawn in. Two of
    /// either side by side are two plants — which is the point for the bush, since being
    /// merged into its neighbour is exactly what made a row of them one flat slab — so
    /// there is nothing a mask could join and every reason not to pay for a third one per
    /// plane.
    ///
    /// It is its own half rather than more quads in [`Self::opaque`] because its
    /// material differs: a stem, a petal and a leaf are all single planes, so they are
    /// seen from both sides and are drawn with no back-face culling. That is a pipeline,
    /// and a pipeline is an entity.
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

/// How wide the two crossing blades of a flower's stem are, in blocks.
///
/// Narrower than the 0.3 a bare head stood on, because the stem is no longer most of
/// what a flower is: beside a petal 0.17 across it read as a post rather than a stalk.
const COVER_STEM_WIDTH: f32 = 0.16;

/// How tall a stem stands above the bottom of its voxel, in blocks. The corolla sits on
/// top of it, so this is also the height of a petal's inner edge.
const COVER_STEM_HEIGHT: f32 = 0.5;

/// How many petals a corolla is built from. Five, and odd on purpose: an even count
/// pairs every petal with one directly opposite and reads as a cross from above.
const COVER_PETALS: usize = 5;

/// How far a petal reaches from the stem's axis, in blocks.
///
/// The inner edge is **on** the axis rather than short of it, which is what closes the
/// middle of the corolla: five rectangles whose inner edges all cross the centre overlap
/// there and leave no hole, while at the outer radius they are 29° wide and clearly five
/// separate petals.
const COVER_PETAL_OUTER: f32 = 0.33;

/// Half a petal's width across, in blocks.
const COVER_PETAL_HALF_WIDTH: f32 = 0.085;

/// How far a petal's outer edge lifts above its inner one, in blocks. The corolla is a
/// shallow bowl and not a disc, so it is still a band of colour seen edge-on from the
/// side, which a flat one would not be.
const COVER_PETAL_RISE: f32 = 0.11;

/// The eye at the middle of the corolla: two crossing blades this wide, rising this far
/// above the petals' inner edge, in blocks.
///
/// Vertical rather than a horizontal disc because a disc would sit within a hundredth of
/// a block of the petal surface it caps and the two would fight the depth buffer over the
/// middle of every flower in the world. Crossing blades share no plane with a petal.
const COVER_EYE_WIDTH: f32 = 0.09;
const COVER_EYE_HEIGHT: f32 = 0.085;

/// The pair of leaves partway up the stem: their height above the voxel floor, how far
/// they reach from the axis, half their width and how far the tip lifts — all in blocks.
const COVER_LEAF_HEIGHT: f32 = 0.21;
const COVER_LEAF_OUTER: f32 = 0.3;
const COVER_LEAF_HALF_WIDTH: f32 = 0.055;
const COVER_LEAF_RISE: f32 = 0.09;

/// How far a bush's foliage keeps back from the walls of its voxel, in blocks.
///
/// Bushes grow in clusters of up to three (`visitBush` on the server), and a face
/// exactly on the plane two of them share would be coincident with the neighbour's —
/// two quads fighting the depth buffer over one surface. Two percent is far too small
/// to read as a gap and far too large to z-fight, which is why a bush's drawn extent
/// is 96% of its voxel on every axis rather than 100%.
///
/// **It no longer has anything to do with filling the collision box.** Until #874
/// `world.Bush` was `world.Solid`, so the drawn shape had to reach every wall or a
/// player would walk into a cube nothing showed them. Now that `Bush` is `Cover`,
/// nothing is stopped by the voxel at all — the inset survives for the z-fighting
/// reason alone, and `stays_inside_the_voxel` is the one guarantee left to assert
/// about it, not a claim that the shape reaches every wall.
const BUSH_INSET: f32 = 0.02;

/// The umbrella frame the two undressed brambles still stand on: four canes at equal
/// angles from one hub, each bent through three woody segments of one constant width.
///
/// **It is being retired one biome at a time, and this is the middle of that.** The meadow
/// bush left it in #835 for [`push_shoot`]; the desert scrub (#836) and the winter bramble
/// (#837) dress it until they follow, and retiring it here would have redrawn both, which
/// #835 puts out of scope by name. `BRAMBLE_` rather than `BUSH_` because the meadow bush
/// is exactly what no longer reads them.
const BRAMBLE_CANES: usize = 4;
const BRAMBLE_CANE_SEGMENTS: usize = 3;
const BRAMBLE_CANE_REACH: f32 = 0.38;
const BRAMBLE_CANE_REACH_JITTER: f32 = 0.06;
const BRAMBLE_CANE_HALF_WIDTH: f32 = 0.014;
const BRAMBLE_CANE_ARCH: f32 = 0.78;
const BRAMBLE_CANE_ARCH_JITTER: f32 = 0.1;
const BRAMBLE_CANE_TIP: f32 = 0.46;
const BRAMBLE_CANE_TIP_JITTER: f32 = 0.12;

/// The meadow bush's shoot skeleton, in blocks. Three numbers answer the three ways the
/// old frame gave itself away:
///
/// - [`BUSH_ROOT_SPREAD`] displaces every foot from the voxel's axis along its own yaw,
///   so the shoots leave three distinct feet on the floor instead of one hub.
/// - [`BUSH_SHOOT_SECTOR_MIN`] and `_MAX` partition the turn into sectors of *unequal*
///   width — normalised so they still close — and the shoot is jittered inside its own.
///   Deliberately not a free yaw per shoot: free dials let all three clump into one
///   quadrant, and the fill guarantee would then be carried by nothing.
/// - [`BUSH_SHOOT_GUARD`] is what each sector keeps clear at both edges, and it makes the
///   feet's spacing a property rather than a hope: two feet at radius `r` and angle `t`
///   apart are `2 r sin(t / 2)` apart, so guarding both edges of two adjacent sectors puts
///   any two over [`BUSH_ROOT_SPREAD`] apart on the floor.
///
/// **`_MAX` is bounded by the guard and not chosen freely**, and getting that wrong is how
/// the guarantee fails silently. The narrowest sector the normalisation can produce is one
/// shoot at `_MIN` with *both* others at `_MAX` — `TAU * MIN / (MIN + 2 * MAX)` — not two
/// at `_MIN`, which is the larger number and the one that is easy to write down instead.
/// At `_MAX = 1.45` that narrowest sector is 1.00 rad against a guard pair of 1.16, so
/// `free` went negative and the shoot was placed inside the clearance the doc claimed for
/// it. `a_sector_is_always_wider_than_the_guard_pair_it_has_to_hold` is the assertion that
/// now holds this, because the 512-bush sweep did not: those seeds never drew the extreme.
const BUSH_SHOOTS: usize = 3;
const BUSH_SHOOT_SEGMENTS: usize = 3;
const BUSH_FORK_SEGMENTS: usize = 2;
const BUSH_ROOT_SPREAD: f32 = 0.12;
const BUSH_SHOOT_GUARD: f32 = 0.58;
const BUSH_SHOOT_SECTOR_MIN: f32 = 0.55;
const BUSH_SHOOT_SECTOR_MAX: f32 = 1.20;

/// Apical dominance, as two ranges driven by one dial per shoot.
///
/// The leading shoot draws that dial from [`BUSH_LEADER_VIGOUR`] up to one and every other
/// from zero up to [`BUSH_FOLLOWER_VIGOUR`]; a shoot's reach and arch are both that number
/// read off their ranges. **The gap between the bands is what makes "the leader is at
/// least 1.3x the shortest" arithmetic rather than luck** — 1.36 at the worst. The tip
/// falls back to a fraction of the arch, so a shoot is an arch and not a mast, and that
/// fraction's jitter is narrow on purpose: a wide one could lengthen a follower's
/// descending segment past a leader's and take the ratio with it.
const BUSH_LEADER_VIGOUR: f32 = 0.85;
const BUSH_FOLLOWER_VIGOUR: f32 = 0.35;
const BUSH_SHOOT_REACH: f32 = 0.14;
const BUSH_SHOOT_REACH_SPAN: f32 = 0.17;
const BUSH_SHOOT_ARCH: f32 = 0.42;
const BUSH_SHOOT_ARCH_SPAN: f32 = 0.38;
const BUSH_SHOOT_TIP_FALL: f32 = 0.50;
const BUSH_SHOOT_TIP_FALL_JITTER: f32 = 0.04;
const BUSH_SHOOT_BEND: f32 = 0.08;

/// The half-width the wood is drawn at, at each of a shoot's four joints and a fork's
/// three. A table rather than an interpolation, so "monotonically non-increasing from
/// foot to tip" is visible here instead of emergent. The fork's are fractions of the
/// parent's half-width **at the shoulder it leaves**, which is what makes a child thinner
/// at its fork than its parent is there by construction.
const BUSH_SHOOT_HALF_WIDTHS: [f32; BUSH_SHOOT_SEGMENTS + 1] = [0.020, 0.014, 0.009, 0.005];
const BUSH_FORK_HALF_SCALES: [f32; BUSH_FORK_SEGMENTS + 1] = [0.60, 0.42, 0.25];
const BUSH_FORK_REACH: f32 = 0.09;
const BUSH_FORK_RISE: f32 = 0.10;
const BUSH_FORK_YAW_SPREAD: f32 = 1.2;

/// The meadow bush's leaves, in blocks.
///
/// Four low ones at the foot are the dense tangle a bramble genuinely grows there, and they
/// carry five of the six fill extrema: their petioles sit on the floor inset and their
/// **widest station** — not their tip — lands on each of the four horizontal insets, which
/// is the difference between a leaf touching the wall and a needle touching it.
///
/// Two more dress every shoot, by phyllotaxis rather than by index parity: each is
/// [`GOLDEN_ANGLE`] further round its shoot than the last and smaller than it, which is
/// what stops a pair from reading as a fixed pair of wings. The crest leaf stands up
/// ([`BUSH_CREST_LEAF_PITCH`]), and on the **leading** shoot it is sized from the ceiling
/// it has to reach rather than from the table — so the sixth extremum is a widest station
/// too. [`BUSH_SHOOT_LEAF_LENGTH`] is above the longest that derivation can ask for, which
/// is what keeps the pair's sizes non-increasing on the leader as well.
const BUSH_BASE_LEAVES: usize = 4;
const BUSH_LEAVES_PER_SHOOT: usize = 2;
const BUSH_BASE_LEAF_PITCH: f32 = 0.22;
const BUSH_BASE_LEAF_HALF_WIDTH: f32 = 0.075;
const BUSH_SHOOT_LEAF_LENGTH: f32 = 0.24;
const BUSH_SHOOT_LEAF_HALF_WIDTH: f32 = 0.05;
const BUSH_SHOOT_LEAF_PITCH: f32 = 0.35;
const BUSH_CREST_LEAF_PITCH: f32 = std::f32::consts::FRAC_PI_2;
const BUSH_LEAF_SHRINK: f32 = 0.72;

/// The angle successive leaves on one shoot are apart around it. `137.5` degrees.
///
/// The one number here that is not a tuning dial: it is the divergence real shoots put
/// between successive leaves, and the reason a spiral of them never puts one leaf directly
/// above another however many there are.
const GOLDEN_ANGLE: f32 = 2.399_963_2;

/// One leaf's midrib, as three profiles read at [`LEAF_SEGMENTS`] + 1 stations.
///
/// **A leaf is not a rectangle and this is where that is decided.** The width profile runs
/// narrow at the petiole, widest at about a third, and closes to a point, so the last
/// station is zero and the outermost quad is a triangle. The rib's two profiles make the
/// blade a surface rather than a plate: `ALONG` reaches its furthest at the widest station
/// and then *recurves*, while `RISE` lifts and then droops, so the three quads carry three
/// different normals and the curled half presents its underside to the light. That is the
/// whole point of the lit double-sided cover material: one leaf gets several tones of one
/// green for free.
const LEAF_SEGMENTS: usize = 3;
const LEAF_STATIONS: usize = LEAF_SEGMENTS + 1;
const LEAF_WIDTH_PROFILE: [f32; LEAF_STATIONS] = [0.30, 1.00, 0.62, 0.00];
const LEAF_RIB_ALONG: [f32; LEAF_STATIONS] = [0.00, 1.00, 0.97, 0.82];
const LEAF_RIB_RISE: [f32; LEAF_STATIONS] = [0.00, 0.35, 0.30, 0.05];

/// Every dial index a meadow bush draws, allocated in one place.
///
/// **Two features sharing an index move together**, which is the correlated-jitter bug
/// that makes a plant look stamped rather than grown. The old frame reserved five per cane
/// with an arithmetic `1 + index * 5`; these are named and deliberately sparse, so a later
/// dressing takes a new one rather than borrowing a neighbour's.
const BUSH_DIAL_TURN: u32 = 0;
const BUSH_DIAL_LEADER: u32 = 1;
const BUSH_DIAL_SECTOR: u32 = 4;
const BUSH_DIAL_SHOOT_YAW: u32 = 8;
const BUSH_DIAL_SHOOT: u32 = 16;
const BUSH_DIALS_PER_SHOOT: u32 = 4;

/// The desert bramble's dressing on the shared cane skeleton. Two short thorns leave
/// every arch, while two low crossed canes reach the four horizontal walls. A crossed
/// thorn at the highest crest reaches the ceiling; together with the low canes it makes
/// the full-voxel collision visible without putting foliage or a filler box in the sand.
const DESERT_THORNS_PER_CANE: usize = 2;
const DESERT_THORN_REACH: f32 = 0.09;
const DESERT_THORN_RISE: f32 = 0.035;
const DESERT_BASE_CANES: usize = 2;
const DESERT_CROWN_BLADES: usize = 2;

/// Three flower specks sit at three cane tips. Their crossed blades are less than half
/// the flower eye's width and height, so they remain punctuation rather than miniature
/// flowers.
const BUSH_SPECKS: usize = 3;
const BUSH_SPECK_HALF_WIDTH: f32 = 0.018;
const BUSH_SPECK_HEIGHT: f32 = 0.04;

/// One crossed-blade berry per tip, plus two at crests.
const WINTER_BRAMBLE_BERRIES: usize = BRAMBLE_CANES + 2;
const WINTER_BERRY_HALF_WIDTH: f32 = 0.025;
const WINTER_BERRY_HEIGHT: f32 = 0.05;

/// How many quads one flower contributes: two stem blades, two leaves,
/// [`COVER_PETALS`] petals and the eye's two blades.
///
/// Test-only for the reason [`palette::PALETTE`] is: production code emits the quads
/// rather than counting them, and a constant nothing reads is a claim nothing checks.
/// Here it is read by the assertions that pin the geometry.
///
/// `pub(super)` rather than private since #652: `render.rs`'s measurement harness
/// grows plants into its terrain fixture and states what that terrain's cover half
/// must cost, and a second literal `11` written down over there would be exactly the
/// hand-copied number this constant exists to stop. Still `#[cfg(test)]` — nothing
/// outside a test may read it, because nothing outside a test counts quads.
#[cfg(test)]
pub(super) const QUADS_PER_COVER: usize = 4 + COVER_PETALS + 2;

/// How many quads one meadow bush contributes: one blade per leaf for the four at the
/// foot and the two on every shoot, two ribbons for each of a shoot's segments and each of
/// its fork's, and two crossed blades per flower speck.
///
/// **66**, where the umbrella frame cost 42, three shoots alone cost 34 and their forks
/// took it to 46. The twenty this half adds are the leaves: ten of them at
/// [`LEAF_SEGMENTS`] quads each rather than one, because a leaf is now a pointed surface
/// and not a card. That is the whole of #835's cost, against a ceiling of 96 — cover is per
/// voxel and unmerged, so every quad is paid once per plant in the world, which is why the
/// issue says to lower the leaf count before raising the ceiling.
#[cfg(test)]
pub(super) const QUADS_PER_BUSH: usize = BUSH_BASE_LEAVES * LEAF_SEGMENTS
    + BUSH_SHOOTS
        * ((BUSH_SHOOT_SEGMENTS + BUSH_FORK_SEGMENTS) * 2 + BUSH_LEAVES_PER_SHOOT * LEAF_SEGMENTS)
    + BUSH_SPECKS * 2;

/// How many quads one desert bramble contributes: the shared four arches, two crossed
/// low canes, two short thorns per arch and the crossed crown thorn.
#[cfg(test)]
pub(super) const QUADS_PER_DESERT_BRAMBLE: usize = BRAMBLE_CANES * BRAMBLE_CANE_SEGMENTS * 2
    + DESERT_BASE_CANES * 2
    + BRAMBLE_CANES * DESERT_THORNS_PER_CANE * 2
    + DESERT_CROWN_BLADES;

/// Four bare canes plus two crossed blades per berry.
#[cfg(test)]
pub(super) const QUADS_PER_WINTER_BRAMBLE: usize =
    BRAMBLE_CANES * BRAMBLE_CANE_SEGMENTS * 2 + WINTER_BRAMBLE_BERRIES * 2;

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
    /// How dark each of this face's four corners is, and part of the key for the same
    /// reason `flow` is: two faces lit differently at their corners must not merge into
    /// one quad that could only carry one of the two answers. [`Occlusion::NONE`] on
    /// every water face — occlusion is the opaque half's, and water is drawn through
    /// rather than lit at its edges.
    occlusion: Occlusion,
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

    build_architecture(&mut mesh.opaque, chunk, neighbours);
    build_cover(&mut mesh.cover, chunk);

    mesh
}

/// Adds slabs and stairs to the opaque surface on their exact half-block grid.
///
/// Ordinary cubes remain in the full-block greedy sweep above. Shapes are sparse,
/// cannot merge through a neighbouring voxel without changing identity, and need
/// partial culling, so this pass pays a 2x2x2 sweep only for a shaped voxel. A full
/// cube beside one contributes only the uncovered quadrants of their shared face;
/// every other cube face is still owned by the ordinary sweep.
fn build_architecture(mesh: &mut SurfaceMesh, chunk: &VoxelChunk, neighbours: &Neighbours) {
    let size = chunk.size();
    for y in 0..size {
        for z in 0..size {
            for x in 0..size {
                let cell = [x, y, z];
                let block = chunk.block(cell);
                if palette::is_architectural_shape(block) {
                    push_architectural_shape(mesh, chunk, neighbours, cell, block);
                    for axis in 0..3 {
                        for positive in [false, true] {
                            let step = if positive { 1 } else { -1 };
                            let coordinate = cell[axis] as isize + step;
                            if coordinate < 0 || coordinate >= size as isize {
                                continue;
                            }
                            let mut cube_cell = cell;
                            cube_cell[axis] = coordinate as usize;
                            let cube = chunk.block(cube_cell);
                            if palette::is_greedy_opaque(cube) {
                                push_cube_shape_interface(
                                    mesh, chunk, neighbours, cube_cell, cube, axis, !positive,
                                    block,
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    // Shapes in another chunk are not visited above. Only the six border layers can
    // meet one, so inspect those instead of making every ordinary cube pay six
    // neighbour lookups throughout the volume.
    for axis in 0..3 {
        let u = (axis + 1) % 3;
        let v = (axis + 2) % 3;
        for positive in [false, true] {
            let Some(across) = neighbours.across(axis, positive, size) else {
                continue;
            };
            for j in 0..size {
                for i in 0..size {
                    let mut cell = [0usize; 3];
                    cell[axis] = if positive { size - 1 } else { 0 };
                    cell[u] = i;
                    cell[v] = j;
                    let cube = chunk.block(cell);
                    if !palette::is_greedy_opaque(cube) {
                        continue;
                    }
                    let mut neighbour_cell = cell;
                    neighbour_cell[axis] = if positive { 0 } else { size - 1 };
                    let shape = across.block(neighbour_cell);
                    if palette::is_architectural_shape(shape) {
                        push_cube_shape_interface(
                            mesh, chunk, neighbours, cell, cube, axis, positive, shape,
                        );
                    }
                }
            }
        }
    }
}

fn push_architectural_shape(
    mesh: &mut SurfaceMesh,
    chunk: &VoxelChunk,
    neighbours: &Neighbours,
    cell: [usize; 3],
    block: BlockId,
) {
    let material = palette::shape_of(block).material;
    let base = cell.map(|coordinate| coordinate * 2);
    let mut mask = [None; 4];

    for axis in 0..3 {
        let u = (axis + 1) % 3;
        let v = (axis + 2) % 3;
        for plane in 0usize..=2 {
            for j in 0..2 {
                for i in 0..2 {
                    let mut negative = [0u8; 3];
                    negative[axis] = plane.saturating_sub(1) as u8;
                    negative[u] = i as u8;
                    negative[v] = j as u8;
                    let mut positive = negative;
                    positive[axis] = plane.min(1) as u8;

                    let negative_occupied = if plane > 0 {
                        palette::occupies_half(block, negative)
                    } else {
                        let mut probe = base.map(|coordinate| coordinate as isize);
                        probe[axis] -= 1;
                        probe[u] += i as isize;
                        probe[v] += j as isize;
                        occupied_half_at(chunk, neighbours, probe)
                    };
                    let positive_occupied = if plane < 2 {
                        palette::occupies_half(block, positive)
                    } else {
                        let mut probe = base.map(|coordinate| coordinate as isize);
                        probe[axis] += 2;
                        probe[u] += i as isize;
                        probe[v] += j as isize;
                        occupied_half_at(chunk, neighbours, probe)
                    };

                    let face = match (negative_occupied, positive_occupied) {
                        (true, false) if plane > 0 => Some(half_face(
                            chunk,
                            neighbours,
                            material,
                            axis,
                            u,
                            v,
                            true,
                            base[axis] + plane,
                            base[u] + i,
                            base[v] + j,
                        )),
                        (false, true) if plane < 2 => Some(half_face(
                            chunk,
                            neighbours,
                            material,
                            axis,
                            u,
                            v,
                            false,
                            base[axis] + plane,
                            base[u] + i,
                            base[v] + j,
                        )),
                        _ => None,
                    };
                    mask[j * 2 + i] = face;
                }
            }
            merge_half_mask(
                mesh,
                axis,
                u,
                v,
                base[axis] + plane,
                base[u],
                base[v],
                &mut mask,
            );
        }
    }
}

/// Emits the part of a full cube's face a neighbouring slab or stair does not cover.
#[expect(
    clippy::too_many_arguments,
    reason = "one partial interface names both blocks and the face between them"
)]
fn push_cube_shape_interface(
    mesh: &mut SurfaceMesh,
    chunk: &VoxelChunk,
    neighbours: &Neighbours,
    cell: [usize; 3],
    block: BlockId,
    axis: usize,
    positive: bool,
    neighbour: BlockId,
) {
    let base = cell.map(|coordinate| coordinate * 2);
    let u = (axis + 1) % 3;
    let v = (axis + 2) % 3;
    let mut mask = [None; 4];
    for j in 0..2 {
        for i in 0..2 {
            let mut half = [0u8; 3];
            half[axis] = u8::from(!positive);
            half[u] = i as u8;
            half[v] = j as u8;
            if !palette::occupies_half(neighbour, half) {
                mask[j * 2 + i] = Some(half_face(
                    chunk,
                    neighbours,
                    block,
                    axis,
                    u,
                    v,
                    positive,
                    base[axis] + usize::from(positive) * 2,
                    base[u] + i,
                    base[v] + j,
                ));
            }
        }
    }
    merge_half_mask(
        mesh,
        axis,
        u,
        v,
        base[axis] + usize::from(positive) * 2,
        base[u],
        base[v],
        &mut mask,
    );
}

#[expect(
    clippy::too_many_arguments,
    reason = "a half-grid face needs the sweep frame and its absolute coordinates"
)]
fn half_face(
    chunk: &VoxelChunk,
    neighbours: &Neighbours,
    block: BlockId,
    axis: usize,
    u: usize,
    v: usize,
    positive: bool,
    plane: usize,
    i: usize,
    j: usize,
) -> Face {
    let outward = if positive {
        plane as isize
    } else {
        plane as isize - 1
    };
    Face {
        block,
        positive,
        geometry: FaceGeometry::Full,
        flow: None,
        occlusion: half_occlusion_at(chunk, neighbours, axis, u, v, outward, i, j),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the half-grid AO probe carries the same coordinate frame as its face"
)]
fn half_occlusion_at(
    chunk: &VoxelChunk,
    neighbours: &Neighbours,
    axis: usize,
    u: usize,
    v: usize,
    outward: isize,
    i: usize,
    j: usize,
) -> Occlusion {
    let (i, j) = (i as isize, j as isize);
    let occupied = |i, j| {
        let mut probe = [0isize; 3];
        probe[axis] = outward;
        probe[u] = i;
        probe[v] = j;
        occupied_half_at(chunk, neighbours, probe)
    };
    let mut levels = [0u8; VERTICES_PER_QUAD];
    for (corner, (du, dv)) in CORNER_STEPS.into_iter().enumerate() {
        let side_u = occupied(i + du, j);
        let side_v = occupied(i, j + dv);
        levels[corner] = if side_u && side_v {
            3
        } else {
            u8::from(side_u) + u8::from(side_v) + u8::from(occupied(i + du, j + dv))
        };
    }
    Occlusion(levels)
}

/// Whether one half-cell in chunk-local half coordinates is opaque.
fn occupied_half_at(chunk: &VoxelChunk, neighbours: &Neighbours, half: [isize; 3]) -> bool {
    let size = chunk.size() as isize;
    let mut voxel = [0isize; 3];
    let mut local_half = [0u8; 3];
    for axis in 0..3 {
        voxel[axis] = half[axis].div_euclid(2);
        local_half[axis] = half[axis].rem_euclid(2) as u8;
    }

    let mut leaves = None;
    for (axis, coordinate) in voxel.iter().enumerate() {
        if *coordinate < 0 || *coordinate >= size {
            if leaves.is_some() {
                return false;
            }
            leaves = Some(axis);
        }
    }
    let block = match leaves {
        None => chunk.block(voxel.map(|coordinate| coordinate as usize)),
        Some(axis) => {
            let positive = voxel[axis] >= size;
            let Some(across) = neighbours.across(axis, positive, chunk.size()) else {
                return false;
            };
            voxel[axis] = if positive { 0 } else { size - 1 };
            across.block(voxel.map(|coordinate| coordinate as usize))
        }
    };
    palette::is_opaque(block) && palette::occupies_half(block, local_half)
}

#[expect(
    clippy::too_many_arguments,
    reason = "the half-grid version of the greedy sweep carries the same coordinate frame"
)]
fn merge_half_mask(
    mesh: &mut SurfaceMesh,
    axis: usize,
    u: usize,
    v: usize,
    plane: usize,
    base_u: usize,
    base_v: usize,
    mask: &mut [Option<Face>; 4],
) {
    for j in 0..2 {
        let mut i = 0;
        while i < 2 {
            let Some(face) = mask[j * 2 + i] else {
                i += 1;
                continue;
            };
            let mut width = 1;
            while i + width < 2 && mask[j * 2 + i + width] == Some(face) {
                width += 1;
            }
            let mut height = 1;
            while j + height < 2 && (i..i + width).all(|x| mask[(j + height) * 2 + x] == Some(face))
            {
                height += 1;
            }
            for row in j..j + height {
                mask[row * 2 + i..row * 2 + i + width].fill(None);
            }

            let corners = half_quad_corners(
                axis,
                u,
                v,
                plane,
                base_u + i,
                base_v + j,
                width,
                height,
                face.positive,
            );
            mesh.push_shaded_quad(
                corners,
                normal(axis, face.positive),
                face.occlusion
                    .shade(palette::linear_rgba(face.block), face.positive),
                None,
            );
            i += width;
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the half-grid quad carries the sweep's complete coordinate frame"
)]
fn half_quad_corners(
    axis: usize,
    u: usize,
    v: usize,
    plane: usize,
    i: usize,
    j: usize,
    width: usize,
    height: usize,
    positive: bool,
) -> [[f32; 3]; 4] {
    let mut origin = [0.0; 3];
    origin[axis] = plane as f32 * 0.5;
    origin[u] = i as f32 * 0.5;
    origin[v] = j as f32 * 0.5;
    let mut along_u = [0.0; 3];
    along_u[u] = width as f32 * 0.5;
    let mut along_v = [0.0; 3];
    along_v[v] = height as f32 * 0.5;
    let add = |left: [f32; 3], right: [f32; 3]| {
        [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
    };
    let far = add(add(origin, along_u), along_v);
    if positive {
        [origin, add(origin, along_u), far, add(origin, along_v)]
    } else {
        [origin, add(origin, along_v), far, add(origin, along_u)]
    }
}

/// Fills the cover half: one plant per [`palette::is_cover`] voxel — a flower for each
/// of the three flower ids, a leafy bramble for a bush, a bare thorn bramble for
/// desert scrub or a berry-dressed winter bramble.
///
/// A whole pass over the chunk rather than a third mask, because there is nothing here
/// for a mask to do — see [`ChunkMesh::cover`]. It reads no neighbour either: a plant's
/// geometry is entirely inside its own voxel, which is why `ChunkStore::apply_block`
/// needs no new remesh rule for one. Breaking a flower on a chunk's edge remeshes that
/// chunk and nothing across the border.
///
/// The iteration order is y, then z, then x — fixed, like the sweep's, so the same
/// voxels produce byte-identical buffers every time.
fn build_cover(mesh: &mut SurfaceMesh, chunk: &VoxelChunk) {
    let size = chunk.size();

    for y in 0..size {
        for z in 0..size {
            for x in 0..size {
                let block = chunk.block([x, y, z]);
                // The gate is [`palette::is_cover`] and not the six ids read apart,
                // because that predicate is a statement *about this loop*: it is what
                // makes [`palette::is_opaque`] false, so the sweep emits nothing at all
                // for a cover voxel. An id that answers it and reaches no branch here
                // is not drawn as a cube — it is not drawn at all. Asking it once is
                // what keeps that from happening silently, for the reason
                // `player::inventory`'s `KITS` is a table and not a comparison: the
                // failure that costs something is the omission, not the extra entry.
                if !palette::is_cover(block) {
                    continue;
                }

                let floor = [x as f32, y as f32, z as f32];
                let seed = plant_seed(x, y, z);
                if block == palette::BUSH {
                    push_bush(mesh, floor, seed);
                } else if block == palette::DESERT_SHRUB {
                    push_desert_bramble(mesh, floor, seed);
                } else if block == palette::WINTER_BRAMBLE {
                    push_winter_bramble(mesh, floor, seed);
                } else {
                    push_flower(mesh, floor, seed, palette::linear_rgba(block));
                }
            }
        }
    }
}

/// A deterministic word for the plant standing in this chunk-local voxel, from which
/// [`dial`] draws the small variations that keep two neighbours from being stamped
/// copies of each other.
///
/// **Derived from nothing but integer coordinates**, which is the rule the whole mesher
/// is written to: the same voxels must produce byte-identical buffers every time, or a
/// remesh triggered by something else would move geometry that did not change. It is
/// chunk-*local* on purpose — `build_cover` is handed no chunk coordinate and asking for
/// one would give the cover half an input the sweep does not have, to break a repeat
/// nobody can see at a spacing of 32 blocks.
fn plant_seed(x: usize, y: usize, z: usize) -> u32 {
    let mut hash = (x as u32).wrapping_mul(0x9E37_79B1)
        ^ (y as u32).wrapping_mul(0x85EB_CA6B)
        ^ (z as u32).wrapping_mul(0xC2B2_AE35);
    hash ^= hash >> 15;
    hash = hash.wrapping_mul(0x2545_F491);
    hash ^ (hash >> 13)
}

/// One dial in `[0, 1)` from a plant's seed; `index` picks a different one from the same
/// word, so a plant's several offsets do not move together.
fn dial(seed: u32, index: u32) -> f32 {
    let mut hash = seed.wrapping_add(index.wrapping_mul(0x9E37_79B1));
    hash ^= hash >> 16;
    hash = hash.wrapping_mul(0x7FEB_352D);
    hash ^= hash >> 15;
    f32::from(((hash >> 8) & 0xFFFF) as u16) / 65536.0
}

/// One flower, standing on the floor of the voxel whose minimum corner is `floor`.
///
/// Four parts and eleven quads: two crossing stem blades, two leaves partway up, a
/// corolla of [`COVER_PETALS`] petals, and the two blades of the eye they meet at. The
/// whole plant is yawed by a fraction of a petal's spacing drawn from `seed`, which is
/// what keeps three flowers in a row from being one flower drawn three times.
///
/// **Every vertex stays inside the voxel**, and that is a property rather than a
/// coincidence: it is what lets `ChunkStore::apply_block` need no remesh rule for a
/// flower on a border. The tallest thing here reaches
/// `COVER_STEM_HEIGHT + COVER_PETAL_RISE + COVER_EYE_HEIGHT`, and the widest reaches
/// `hypot(COVER_PETAL_OUTER, COVER_PETAL_HALF_WIDTH)` from the middle;
/// `every_flower_vertex_stays_inside_its_own_voxel` is what holds the arithmetic to it.
fn push_flower(mesh: &mut SurfaceMesh, floor: [f32; 3], seed: u32, petal: [f32; 4]) {
    let stem_color = opaque(palette::STEM_LINEAR);
    let leaf_color = opaque(palette::LEAF_LINEAR);
    let eye_color = opaque(palette::FLOWER_CENTRE_LINEAR);

    // The middle of the voxel's floor: where the stem stands.
    let base = [floor[0] + 0.5, floor[1], floor[2] + 0.5];
    let top = floor[1] + COVER_STEM_HEIGHT;

    // Two blades crossing on the voxel's vertical axis, each a single quad: the material
    // draws both sides, so a second wound the other way would be a coincident copy
    // fighting the depth buffer for nothing.
    let half = COVER_STEM_WIDTH / 2.0;
    push_blade(mesh, base, half, top, 0, stem_color);
    push_blade(mesh, base, half, top, 2, stem_color);

    let spacing = std::f32::consts::TAU / COVER_PETALS as f32;
    let yaw = dial(seed, 0) * spacing;

    // The leaves, opposite each other so the plant does not lean. They are the same
    // primitive as a petal — a blade radiating from the stem and lifting at the tip —
    // which is the only reason two more parts cost no more code than one.
    let leaf_base = [base[0], floor[1] + COVER_LEAF_HEIGHT, base[2]];
    for side in 0..2 {
        push_radial_blade(
            mesh,
            leaf_base,
            yaw + side as f32 * std::f32::consts::PI,
            COVER_LEAF_OUTER,
            COVER_LEAF_HALF_WIDTH,
            COVER_LEAF_RISE,
            leaf_color,
        );
    }

    // The corolla. Every petal starts on the axis, so the five of them overlap at the
    // middle and the flower has no hole in it seen from above.
    let corolla = [base[0], top, base[2]];
    for petal_index in 0..COVER_PETALS {
        push_radial_blade(
            mesh,
            corolla,
            yaw + petal_index as f32 * spacing,
            COVER_PETAL_OUTER,
            COVER_PETAL_HALF_WIDTH,
            COVER_PETAL_RISE,
            petal,
        );
    }

    // The eye, in the one place the petals all meet.
    let eye = COVER_EYE_WIDTH / 2.0;
    push_blade(mesh, corolla, eye, top + COVER_EYE_HEIGHT, 0, eye_color);
    push_blade(mesh, corolla, eye, top + COVER_EYE_HEIGHT, 2, eye_color);
}

#[derive(Clone, Copy)]
struct Cane {
    shoulder: [f32; 3],
    crest: [f32; 3],
    tip: [f32; 3],
    yaw: f32,
}

/// One arching cane in the shared bramble skeleton.
///
/// Three [`push_twig`] segments leave the base, climb through a shoulder to a crest,
/// then descend to the tip. Yaw, reach, arch height, sideways bend and tip height all
/// come from separate [`dial`]s on the plant's word. The meadow bush dresses the
/// returned joints with leaves and flowers; the desert and tundra bushes in #789 and
/// #790 reuse this skeleton and dress it for their own biomes.
fn push_cane(
    mesh: &mut SurfaceMesh,
    floor: [f32; 3],
    seed: u32,
    index: usize,
    color: [f32; 4],
) -> Cane {
    let spacing = std::f32::consts::TAU / BRAMBLE_CANES as f32;
    let dial_index = 1 + index as u32 * 5;
    let yaw =
        dial(seed, 0) * spacing + index as f32 * spacing + (dial(seed, dial_index) - 0.5) * 0.36;
    let reach = BRAMBLE_CANE_REACH + dial(seed, dial_index + 1) * BRAMBLE_CANE_REACH_JITTER;
    let arch = BRAMBLE_CANE_ARCH + dial(seed, dial_index + 2) * BRAMBLE_CANE_ARCH_JITTER;
    let tip_height = BRAMBLE_CANE_TIP + dial(seed, dial_index + 3) * BRAMBLE_CANE_TIP_JITTER;
    let bend = (dial(seed, dial_index + 4) - 0.5) * 0.1;
    let (sin, cos) = yaw.sin_cos();
    let along = [cos, 0.0, sin];
    let across = [-sin, 0.0, cos];
    let at = |distance: f32, sideways: f32, height: f32| {
        [
            floor[0] + 0.5 + along[0] * distance + across[0] * sideways,
            floor[1] + height,
            floor[2] + 0.5 + along[2] * distance + across[2] * sideways,
        ]
    };

    let base = at(-0.06, 0.0, BUSH_INSET + BRAMBLE_CANE_HALF_WIDTH);
    let shoulder = at(reach * 0.32, bend * 0.5, arch * 0.72);
    let crest = at(reach * 0.68, bend, arch);
    let tip = at(reach, bend * 0.5, tip_height);
    let joints = [base, shoulder, crest, tip];
    for segment in 0..BRAMBLE_CANE_SEGMENTS {
        push_twig(
            mesh,
            joints[segment],
            joints[segment + 1],
            BRAMBLE_CANE_HALF_WIDTH,
            color,
        );
    }

    Cane {
        shoulder,
        crest,
        tip,
        yaw,
    }
}

/// The joints one shoot's dressing needs, the way [`Cane`] answers for the old frame.
#[derive(Clone, Copy)]
struct Shoot {
    shoulder: [f32; 3],
    crest: [f32; 3],
    tip: [f32; 3],
    yaw: f32,
    leader: bool,
}

/// Which of a bush's shoots leads. One dial, read in one place, so the shoot that draws
/// its vigour from the top of the range is the same one the ceiling leaf is grown on.
fn bush_leader(seed: u32) -> usize {
    (dial(seed, BUSH_DIAL_LEADER) * BUSH_SHOOTS as f32) as usize % BUSH_SHOOTS
}

/// Where each shoot's foot leaves the floor, as a yaw per shoot.
///
/// The turn is partitioned into sectors of unequal width — a dial each, normalised so they
/// still close — and every shoot is jittered inside its own, clear of
/// [`BUSH_SHOOT_GUARD`] at both edges. Two shoots are therefore never `TAU / n` apart
/// except by coincidence, and never closer than twice the guard, which is what puts their
/// feet more than [`BUSH_ROOT_SPREAD`] apart on the floor.
fn bush_shoot_yaws(seed: u32) -> [f32; BUSH_SHOOTS] {
    let widths: [f32; BUSH_SHOOTS] = std::array::from_fn(|shoot| {
        BUSH_SHOOT_SECTOR_MIN
            + dial(seed, BUSH_DIAL_SECTOR + shoot as u32)
                * (BUSH_SHOOT_SECTOR_MAX - BUSH_SHOOT_SECTOR_MIN)
    });
    let total: f32 = widths.iter().sum();

    let mut edge = dial(seed, BUSH_DIAL_TURN) * std::f32::consts::TAU;
    let mut yaws = [0.0; BUSH_SHOOTS];
    for (shoot, yaw) in yaws.iter_mut().enumerate() {
        let sector = widths[shoot] / total * std::f32::consts::TAU;
        // The narrowest sector the normalisation can produce is wider than twice the
        // guard, so this is always somewhere to move rather than a negative span.
        let free = sector - 2.0 * BUSH_SHOOT_GUARD;
        *yaw = edge + BUSH_SHOOT_GUARD + free * dial(seed, BUSH_DIAL_SHOOT_YAW + shoot as u32);
        edge += sector;
    }
    yaws
}

/// One shoot of the meadow bush's skeleton.
///
/// [`BUSH_SHOOT_SEGMENTS`] segments climb from a foot displaced along `yaw` through a
/// shoulder to a crest and back down to a tip. The returned joints are what [`push_bush`]
/// dresses, exactly as [`Cane`] is for the frame the other two brambles still stand on.
///
/// Each segment is drawn narrower than the last, and one child twig leaves the shoulder at
/// a dial-driven deviation — shorter than its parent and thinner than the parent is at that
/// joint. Two levels is the whole of it: this is a knee-high plant inside one voxel.
fn push_shoot(
    mesh: &mut SurfaceMesh,
    floor: [f32; 3],
    seed: u32,
    index: usize,
    yaw: f32,
    leader: bool,
    color: [f32; 4],
) -> Shoot {
    let dials = BUSH_DIAL_SHOOT + index as u32 * BUSH_DIALS_PER_SHOOT;
    // The two bands never meet, which is what makes apical dominance arithmetic.
    let vigour = if leader {
        BUSH_LEADER_VIGOUR + (1.0 - BUSH_LEADER_VIGOUR) * dial(seed, dials)
    } else {
        BUSH_FOLLOWER_VIGOUR * dial(seed, dials)
    };
    let reach = BUSH_SHOOT_REACH + vigour * BUSH_SHOOT_REACH_SPAN;
    let arch = BUSH_SHOOT_ARCH + vigour * BUSH_SHOOT_ARCH_SPAN;
    let fall = BUSH_SHOOT_TIP_FALL + dial(seed, dials + 1) * BUSH_SHOOT_TIP_FALL_JITTER;
    let bend = (dial(seed, dials + 2) - 0.5) * BUSH_SHOOT_BEND;

    let (sin, cos) = yaw.sin_cos();
    let along = [cos, 0.0, sin];
    let across = [-sin, 0.0, cos];
    // Radius from the voxel's vertical axis, not from a hub: every joint of this shoot
    // starts BUSH_ROOT_SPREAD out along its own yaw.
    let at = |radius: f32, sideways: f32, height: f32| {
        [
            floor[0] + 0.5 + along[0] * radius + across[0] * sideways,
            floor[1] + height,
            floor[2] + 0.5 + along[2] * radius + across[2] * sideways,
        ]
    };

    // The foot sits its own half-width above the floor inset, so the ribbon around the
    // first segment cannot dip below the extent the low leaves are what carries.
    let foot = at(
        BUSH_ROOT_SPREAD,
        0.0,
        BUSH_INSET + BUSH_SHOOT_HALF_WIDTHS[0],
    );
    let shoulder = at(BUSH_ROOT_SPREAD + reach * 0.32, bend * 0.5, arch * 0.72);
    let crest = at(BUSH_ROOT_SPREAD + reach * 0.68, bend, arch);
    let tip = at(BUSH_ROOT_SPREAD + reach, bend * 0.5, arch * fall);
    let joints = [foot, shoulder, crest, tip];
    for segment in 0..BUSH_SHOOT_SEGMENTS {
        push_tapered_twig(
            mesh,
            joints[segment],
            joints[segment + 1],
            BUSH_SHOOT_HALF_WIDTHS[segment],
            BUSH_SHOOT_HALF_WIDTHS[segment + 1],
            color,
        );
    }

    // The fork leaves the shoulder, which is the joint that keeps a child inside the
    // voxel whatever the leader's reach turned out to be.
    let fork_yaw = yaw + (dial(seed, dials + 3) - 0.5) * BUSH_FORK_YAW_SPREAD;
    let (fork_sin, fork_cos) = fork_yaw.sin_cos();
    let fork_at = |step: f32| {
        [
            shoulder[0] + fork_cos * BUSH_FORK_REACH * step,
            shoulder[1] + BUSH_FORK_RISE * step,
            shoulder[2] + fork_sin * BUSH_FORK_REACH * step,
        ]
    };
    let shoulder_half = BUSH_SHOOT_HALF_WIDTHS[1];
    let fork_joints = [shoulder, fork_at(0.55), fork_at(1.0)];
    for segment in 0..BUSH_FORK_SEGMENTS {
        push_tapered_twig(
            mesh,
            fork_joints[segment],
            fork_joints[segment + 1],
            shoulder_half * BUSH_FORK_HALF_SCALES[segment],
            shoulder_half * BUSH_FORK_HALF_SCALES[segment + 1],
            color,
        );
    }

    Shoot {
        shoulder,
        crest,
        tip,
        yaw,
        leader,
    }
}

/// One meadow bush, filling the voxel whose minimum corner is `floor`.
///
/// Three forked woody shoots of unequal length make the silhouette, ten pointed leaves
/// dress them, and three shoot tips flower. Nothing here is `n` of anything at `TAU / n`
/// any more: the shoots leave three separate feet in three sectors of unequal width, one of
/// them leads, and the leaves on each follow the golden angle rather than an index parity.
///
/// **Reaching every wall used to be a requirement and is now a leftover.** Before #874
/// `world.Bush` was `Solid`, so the shape had to fill the cube a body was stopped by:
/// the four low leaves reached the floor and the four horizontal insets, and the
/// leading shoot's crest leaf was sized from the ceiling rather than from the table.
/// `Bush` is `Cover` now, nothing is stopped by this voxel, and the shape is unchanged
/// because this issue draws nothing new — `stays_inside_the_voxel` is the one thing
/// still asserted about it.
fn push_bush(mesh: &mut SurfaceMesh, floor: [f32; 3], seed: u32) {
    let foliage = palette::linear_rgba(palette::BUSH);
    let leaf = opaque(palette::BUSH_CROWN_LINEAR);
    let wood = palette::linear_rgba(palette::LOG);

    let yaws = bush_shoot_yaws(seed);
    let leader = bush_leader(seed);
    let shoots: [Shoot; BUSH_SHOOTS] = std::array::from_fn(|shoot| {
        push_shoot(mesh, floor, seed, shoot, yaws[shoot], shoot == leader, wood)
    });

    // The low tangle. Cardinal yaws exactly, because these four are the fill guarantee on
    // the four horizontal walls and a jittered yaw would land the extremum short of it by
    // its own cosine. The length is derived rather than tuned: it is whatever puts the
    // widest station of the rib on the inset at this pitch.
    let petiole = [floor[0] + 0.5, floor[1] + BUSH_INSET, floor[2] + 0.5];
    let base_length = (0.5 - BUSH_INSET) / leaf_reach_factor(BUSH_BASE_LEAF_PITCH);
    for direction in 0..BUSH_BASE_LEAVES {
        push_leaf(
            mesh,
            petiole,
            direction as f32 * std::f32::consts::FRAC_PI_2,
            BUSH_BASE_LEAF_PITCH,
            base_length,
            BUSH_BASE_LEAF_HALF_WIDTH,
            foliage,
        );
    }

    // Two leaves per shoot, each a golden angle further round it than the last and smaller
    // than it. The crest one stands up; on the leader it is grown to the ceiling.
    let ceiling = floor[1] + 1.0 - BUSH_INSET;
    for shoot in &shoots {
        for index in 0..BUSH_LEAVES_PER_SHOOT {
            let (attach, pitch, length) = if index == 0 {
                (
                    shoot.shoulder,
                    BUSH_SHOOT_LEAF_PITCH,
                    BUSH_SHOOT_LEAF_LENGTH,
                )
            } else {
                let length = if shoot.leader {
                    (ceiling - shoot.crest[1]) / leaf_lift_factor(BUSH_CREST_LEAF_PITCH)
                } else {
                    BUSH_SHOOT_LEAF_LENGTH * BUSH_LEAF_SHRINK
                };
                (shoot.crest, BUSH_CREST_LEAF_PITCH, length)
            };
            push_leaf(
                mesh,
                attach,
                shoot.yaw + (index + 1) as f32 * GOLDEN_ANGLE,
                pitch,
                length,
                BUSH_SHOOT_LEAF_HALF_WIDTH * BUSH_LEAF_SHRINK.powi(index as i32),
                leaf,
            );
        }
    }

    // Every tip flowers. Which colour leads still turns with the seed, and the rotation
    // is what keeps a bush showing three different ones rather than three of one.
    let flowers = [
        palette::FLOWER_RED,
        palette::FLOWER_YELLOW,
        palette::FLOWER_BLUE,
    ];
    for (speck, shoot) in shoots.iter().take(BUSH_SPECKS).enumerate() {
        let colour = palette::linear_rgba(flowers[(seed as usize + speck) % flowers.len()]);
        push_blade(
            mesh,
            shoot.tip,
            BUSH_SPECK_HALF_WIDTH,
            shoot.tip[1] + BUSH_SPECK_HEIGHT,
            0,
            colour,
        );
        push_blade(
            mesh,
            shoot.tip,
            BUSH_SPECK_HALF_WIDTH,
            shoot.tip[1] + BUSH_SPECK_HEIGHT,
            2,
            colour,
        );
    }
}

/// One desert bramble, filling the voxel whose minimum corner is `floor`.
///
/// Four [`push_cane`] arches carry the silhouette and two short thorns grow from each.
/// Two crossed canes at the foot make the dry tangle broad without leaves; their ends
/// reach the four horizontal walls, and the crossed thorn rising from the highest crest
/// reaches the ceiling. Every part is the scrub's existing sun-bleached khaki — there
/// are no flowers, berries, leaves or saturated accents in this biome.
///
/// **Reaching every wall used to be a requirement and is now a leftover.** Before #874
/// `world.DesertShrub` was `Solid`, so the sparse geometry had to show the whole cube a
/// body was stopped by. `DesertShrub` is `Cover` now, nothing is stopped by this voxel,
/// and the shape is unchanged because this issue draws nothing new. The six extrema
/// still land at [`BUSH_INSET`] rather than on the voxel planes — that keeps
/// neighbouring scrubs from sharing a plane, which is the reason `BUSH_INSET` survives.
fn push_desert_bramble(mesh: &mut SurfaceMesh, floor: [f32; 3], seed: u32) {
    let wood = palette::linear_rgba(palette::DESERT_SHRUB);
    let canes: [Cane; BRAMBLE_CANES] =
        std::array::from_fn(|cane| push_cane(mesh, floor, seed, cane, wood));

    // Two low canes cross through the roots. Their ribbons touch the floor inset and
    // their centre lines touch the four horizontal insets: the full drawn span is
    // carried by wood, not by a leaf or a hidden box.
    let base_axes: [usize; DESERT_BASE_CANES] = [0, 2];
    for axis in base_axes {
        let mut start = [
            floor[0] + 0.5,
            floor[1] + BUSH_INSET + BRAMBLE_CANE_HALF_WIDTH,
            floor[2] + 0.5,
        ];
        let mut end = start;
        start[axis] = floor[axis] + BUSH_INSET;
        end[axis] = floor[axis] + 1.0 - BUSH_INSET;
        push_twig(mesh, start, end, BRAMBLE_CANE_HALF_WIDTH, wood);
    }

    // Alternating sides make the thorns read as a tangle rather than a comb. Every
    // endpoint is derived from the cane's deterministic yaw and joints, so no world or
    // chunk coordinate has entered the shape.
    for (index, cane) in canes.iter().enumerate() {
        let thorn_joints: [[f32; 3]; DESERT_THORNS_PER_CANE] = [cane.shoulder, cane.crest];
        for (thorn, joint) in thorn_joints.into_iter().enumerate() {
            let side = if (index + thorn) % 2 == 0 { 1.0 } else { -1.0 };
            let angle = cane.yaw + side * std::f32::consts::FRAC_PI_2;
            let (sin, cos) = angle.sin_cos();
            let end = [
                joint[0] + cos * DESERT_THORN_REACH,
                joint[1] + DESERT_THORN_RISE,
                joint[2] + sin * DESERT_THORN_REACH,
            ];
            push_twig(mesh, joint, end, BRAMBLE_CANE_HALF_WIDTH, wood);
        }
    }

    // The highest arch owns the one vertical thorn that reaches the voxel's ceiling.
    // Crossed blades are the established tiny-detail primitive; here they use the same
    // dry wood and meet the crest instead of pretending to be a flower.
    let crown = canes
        .iter()
        .max_by(|left, right| left.crest[1].total_cmp(&right.crest[1]))
        .expect("a bramble has canes")
        .crest;
    let top = floor[1] + 1.0 - BUSH_INSET;
    let crown_axes: [usize; DESERT_CROWN_BLADES] = [0, 2];
    for axis in crown_axes {
        push_blade(mesh, crown, BRAMBLE_CANE_HALF_WIDTH, top, axis, wood);
    }
}

/// Bare shared canes with dark berries; `world.Cover`, so it has no fill guarantee.
fn push_winter_bramble(mesh: &mut SurfaceMesh, floor: [f32; 3], seed: u32) {
    let wood = palette::linear_rgba(palette::LOG);
    let berry = palette::linear_rgba(palette::WINTER_BRAMBLE);
    let canes: [Cane; BRAMBLE_CANES] =
        std::array::from_fn(|cane| push_cane(mesh, floor, seed, cane, wood));

    for cane in &canes {
        push_berry(mesh, cane.tip, berry);
    }
    for cane in canes.iter().take(WINTER_BRAMBLE_BERRIES - BRAMBLE_CANES) {
        push_berry(mesh, cane.crest, berry);
    }
}

/// One small round-ish berry made from two crossed, double-sided blades.
fn push_berry(mesh: &mut SurfaceMesh, centre: [f32; 3], color: [f32; 4]) {
    let base = [centre[0], centre[1] - WINTER_BERRY_HEIGHT * 0.5, centre[2]];
    let top = centre[1] + WINTER_BERRY_HEIGHT * 0.5;
    push_blade(mesh, base, WINTER_BERRY_HALF_WIDTH, top, 0, color);
    push_blade(mesh, base, WINTER_BERRY_HALF_WIDTH, top, 2, color);
}

/// A palette tone as the opaque RGBA a vertex carries. The cover half has no alpha of its
/// own — `render.rs` gives it an opaque material — so this is the only shape the
/// conversion ever takes, and the three tones the mesher names directly are the only
/// callers: [`palette::linear_rgba`] already answers with an alpha.
fn opaque(tone: [f32; 3]) -> [f32; 4] {
    [tone[0], tone[1], tone[2], 1.0]
}

/// A petal or a leaf: a quad radiating from `centre` at `angle`, `2 * half_width` across,
/// reaching `outer` from the axis and lifting `rise` at its outer edge.
///
/// **The inner edge is on the axis rather than short of it**, so the blades of one corolla
/// overlap at the middle and close it. Two of them at different angles are two planes
/// through a common point: they cross along a line and share no area, which is what keeps
/// the overlap from being a depth-buffer fight.
///
/// The corner order is `inner(-w), inner(+w), outer(+w), outer(-w)` and it is
/// **load-bearing** — `hands::BladeSection::perimeter` says why, and it has been paid for
/// twice this iteration: walking a section the other way round turns the surface inside
/// out, which is visible only as a shape that vanishes when you look at it. Here the
/// normal is *derived* from those corners rather than declared beside them, so the two
/// cannot disagree however the order is later edited.
fn push_radial_blade(
    mesh: &mut SurfaceMesh,
    centre: [f32; 3],
    angle: f32,
    outer: f32,
    half_width: f32,
    rise: f32,
    color: [f32; 4],
) {
    let (sin, cos) = angle.sin_cos();
    let at = |radius: f32, across: f32, lift: f32| {
        [
            centre[0] + cos * radius - sin * across,
            centre[1] + lift,
            centre[2] + sin * radius + cos * across,
        ]
    };
    let corners = [
        at(0.0, -half_width, 0.0),
        at(0.0, half_width, 0.0),
        at(outer, half_width, rise),
        at(outer, -half_width, rise),
    ];
    mesh.push_quad(corners, face_normal(corners), color, None);
}

/// The unit normal a quad's corners imply, from the two edges leaving the first one —
/// the same cross product the winding assertions in the tests below take.
fn face_normal(corners: [[f32; 3]; VERTICES_PER_QUAD]) -> [f32; 3] {
    let edge = |from: [f32; 3], to: [f32; 3]| [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let (a, b) = (edge(corners[0], corners[1]), edge(corners[0], corners[3]));
    let cross = [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ];
    let length = (cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2]).sqrt();
    [cross[0] / length, cross[1] / length, cross[2] / length]
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

/// Two crossed ribbons around one sloping segment, `half_at_start` across where it leaves
/// and `half_at_end` where it arrives. The cover material is double-sided, so each ribbon
/// is one quad and the pair reads as a twig from every horizontal view.
///
/// **The taper is what separates a branch from a wire, and it costs no quads**: a
/// trapezoid has the rectangle's four corners. [`push_twig`] is this with equal ends, so
/// there is one implementation of the crossed ribbon rather than two that can drift.
fn push_tapered_twig(
    mesh: &mut SurfaceMesh,
    start: [f32; 3],
    end: [f32; 3],
    half_at_start: f32,
    half_at_end: f32,
    color: [f32; 4],
) {
    let direction = [end[0] - start[0], end[1] - start[1], end[2] - start[2]];
    let horizontal_length = direction[0].hypot(direction[2]);
    // Unit rather than pre-scaled, since the two ends are no longer the same width.
    let side = [
        -direction[2] / horizontal_length,
        0.0,
        direction[0] / horizontal_length,
    ];
    let length =
        (direction[0] * direction[0] + direction[1] * direction[1] + direction[2] * direction[2])
            .sqrt();
    let across = [
        (direction[1] * side[2] - direction[2] * side[1]) / length,
        (direction[2] * side[0] - direction[0] * side[2]) / length,
        (direction[0] * side[1] - direction[1] * side[0]) / length,
    ];
    let offset = |unit: [f32; 3], point: [f32; 3], half: f32, sign: f32| {
        [
            point[0] + unit[0] * half * sign,
            point[1] + unit[1] * half * sign,
            point[2] + unit[2] * half * sign,
        ]
    };
    for unit in [side, across] {
        let corners = [
            offset(unit, start, half_at_start, -1.0),
            offset(unit, start, half_at_start, 1.0),
            offset(unit, end, half_at_end, 1.0),
            offset(unit, end, half_at_end, -1.0),
        ];
        mesh.push_quad(corners, face_normal(corners), color, None);
    }
}

/// Two crossed ribbons of one constant width around one sloping segment.
fn push_twig(
    mesh: &mut SurfaceMesh,
    start: [f32; 3],
    end: [f32; 3],
    half_width: f32,
    color: [f32; 4],
) {
    push_tapered_twig(mesh, start, end, half_width, half_width, color);
}

/// The furthest a leaf of unit length reaches from its petiole, at `pitch`.
///
/// The maximum over the rib's stations and not the last one, because the profile recurves:
/// the outermost station is the **widest** one rather than the tip. A caller that has to
/// land a leaf's outer edge exactly on a wall divides by this, which is why no leaf length
/// in this file is a tuned literal.
fn leaf_reach_factor(pitch: f32) -> f32 {
    let (sin, cos) = pitch.sin_cos();
    LEAF_RIB_ALONG
        .into_iter()
        .zip(LEAF_RIB_RISE)
        .map(|(along, rise)| along * cos - rise * sin)
        .fold(f32::NEG_INFINITY, f32::max)
}

/// The highest a leaf of unit length lifts above its petiole, at `pitch`, for the same
/// reason and read by the one leaf that is grown to the voxel's ceiling.
fn leaf_lift_factor(pitch: f32) -> f32 {
    let (sin, cos) = pitch.sin_cos();
    LEAF_RIB_ALONG
        .into_iter()
        .zip(LEAF_RIB_RISE)
        .map(|(along, rise)| along * sin + rise * cos)
        .fold(f32::NEG_INFINITY, f32::max)
}

/// One leaf: a midrib of [`LEAF_SEGMENTS`] quads leaving `attach` along `yaw`, tilted out
/// of the horizontal by `pitch`, with a width that opens and then closes to a point.
///
/// **This is the primitive that replaces a rectangle with a leaf.** [`push_radial_blade`]
/// takes one half-width and uses it at both ends, so what it draws is a card however it is
/// aimed; here the width is a profile and the rib is two more, so the blade widens,
/// recurves, droops and closes. The three quads therefore carry three different normals,
/// and on the lit double-sided cover material that is what turns a cluster of leaves into a
/// volume instead of one flat tone.
///
/// The width runs horizontally across the rib at every pitch, deliberately: it keeps a
/// leaf's height a function of the rib alone, which is what lets [`leaf_lift_factor`]
/// answer exactly for the one leaf that has to touch the ceiling.
///
/// The corner order is `near(-w), near(+w), far(+w), far(-w)`, the order
/// [`push_radial_blade`] documents as load-bearing, and the normal is derived from the
/// corners rather than declared beside them so the two cannot disagree. The last station is
/// zero wide, so the outermost quad is a triangle with two coincident corners — that is the
/// point, and [`face_normal`] reads corners 0, 1 and 3, none of which are the pair.
fn push_leaf(
    mesh: &mut SurfaceMesh,
    attach: [f32; 3],
    yaw: f32,
    pitch: f32,
    length: f32,
    half_width: f32,
    color: [f32; 4],
) {
    let (sin_yaw, cos_yaw) = yaw.sin_cos();
    let (sin_pitch, cos_pitch) = pitch.sin_cos();
    let across = [-sin_yaw, 0.0, cos_yaw];
    let station = |index: usize| {
        let along = LEAF_RIB_ALONG[index] * length;
        let rise = LEAF_RIB_RISE[index] * length;
        let out = along * cos_pitch - rise * sin_pitch;
        (
            [
                attach[0] + cos_yaw * out,
                attach[1] + along * sin_pitch + rise * cos_pitch,
                attach[2] + sin_yaw * out,
            ],
            half_width * LEAF_WIDTH_PROFILE[index],
        )
    };
    let edge = |point: [f32; 3], half: f32, sign: f32| {
        [
            point[0] + across[0] * half * sign,
            point[1],
            point[2] + across[2] * half * sign,
        ]
    };

    for segment in 0..LEAF_SEGMENTS {
        let (near, near_half) = station(segment);
        let (far, far_half) = station(segment + 1);
        let corners = [
            edge(near, near_half, -1.0),
            edge(near, near_half, 1.0),
            edge(far, far_half, 1.0),
            edge(far, far_half, -1.0),
        ];
        mesh.push_quad(corners, face_normal(corners), color, None);
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

            opaque_mask[j * size + i] = match (
                palette::is_greedy_opaque(negative),
                palette::is_greedy_opaque(positive),
            ) {
                // Opaque below, see-through above: the face belongs to the opaque
                // voxel and points along +axis. "See-through" is air or water,
                // which is what keeps the lake bed's top.
                (true, false) if below_is_ours && !palette::is_architectural_shape(positive) => {
                    Some(Face {
                        block: negative,
                        positive: true,
                        geometry: FaceGeometry::Full,
                        flow: None,
                        // The face points along +axis, so the side it is lit from is the
                        // layer at `plane`: the see-through one it was emitted against.
                        occlusion: occlusion_at(
                            chunk,
                            neighbours,
                            axis,
                            u,
                            v,
                            true,
                            plane as isize,
                            i,
                            j,
                        ),
                    })
                }
                (false, true) if above_is_ours && !palette::is_architectural_shape(negative) => {
                    Some(Face {
                        block: positive,
                        positive: false,
                        geometry: FaceGeometry::Full,
                        flow: None,
                        occlusion: occlusion_at(
                            chunk,
                            neighbours,
                            axis,
                            u,
                            v,
                            false,
                            plane as isize - 1,
                            i,
                            j,
                        ),
                    })
                }
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
            occlusion: Occlusion::NONE,
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
            occlusion: Occlusion::NONE,
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
            occlusion: Occlusion::NONE,
            positive: true,
            geometry: FaceGeometry::WaterSide {
                bottom: positive_level,
                top: negative_level,
            },
            flow: None,
        }),
        (true, true) if positive_level > negative_level && positive_is_ours => Some(Face {
            block: palette::WATER,
            occlusion: Occlusion::NONE,
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
/// A solid neighbour contributes nothing because it is a wall. A source contributes
/// its full level only when it feeds this voxel: plain water feeds every side, while a
/// current feeds only the side it points toward. This is the server automaton's rule,
/// mirrored here so the shader never shows a lateral spill the simulation rejects.
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
        let toward_here = match axis {
            0 => (-(step as i8), 0),
            2 => (0, -(step as i8)),
            _ => unreachable!("horizontal flow uses only x and z"),
        };
        if palette::is_water(neighbour)
            && !palette::water_feeds_toward(neighbour, toward_here.0, toward_here.1)
        {
            continue;
        }
        let neighbour_level = palette::water_level(neighbour);
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

/// The [`Occlusion`] of the face on mask cell `(i, j)`, sampled on the layer it points
/// at.
///
/// `outward` is that layer's index along `axis`, and it is an `isize` because it is
/// legitimately `-1` or `size`: a face on a chunk's border points out of the chunk, and
/// what is beyond it is the neighbour's — or air, when no neighbour was handed in.
///
/// Three samples per corner and twelve per face, all of them on one layer, and none of
/// them taken for a mask cell that has no face. That keeps the cost proportional to the
/// **surface** rather than to the volume, which is why it can live in the sweep at all.
#[expect(
    clippy::too_many_arguments,
    reason = "the sweep's coordinate frame, the same one `sample` and `merge_mask` take"
)]
fn occlusion_at(
    chunk: &VoxelChunk,
    neighbours: &Neighbours,
    axis: usize,
    u: usize,
    v: usize,
    positive: bool,
    outward: isize,
    i: usize,
    j: usize,
) -> Occlusion {
    let (i, j) = (i as isize, j as isize);
    let mut levels = [0u8; VERTICES_PER_QUAD];
    for (corner, (du, dv)) in CORNER_STEPS.into_iter().enumerate() {
        let axis_half = u8::from(!positive);
        let current_u_half = u8::from(du > 0);
        let current_v_half = u8::from(dv > 0);
        let outside_u_half = u8::from(du < 0);
        let outside_v_half = u8::from(dv < 0);
        let mut side_u_half = [0u8; 3];
        side_u_half[axis] = axis_half;
        side_u_half[u] = outside_u_half;
        side_u_half[v] = current_v_half;
        let mut side_v_half = [0u8; 3];
        side_v_half[axis] = axis_half;
        side_v_half[u] = current_u_half;
        side_v_half[v] = outside_v_half;
        let mut diagonal_half = [0u8; 3];
        diagonal_half[axis] = axis_half;
        diagonal_half[u] = outside_u_half;
        diagonal_half[v] = outside_v_half;

        let side_u = is_opaque_at(
            chunk,
            neighbours,
            axis,
            u,
            v,
            outward,
            i + du,
            j,
            side_u_half,
        );
        let side_v = is_opaque_at(
            chunk,
            neighbours,
            axis,
            u,
            v,
            outward,
            i,
            j + dv,
            side_v_half,
        );
        levels[corner] = if side_u && side_v {
            // Both edges walled in. The diagonal is behind them either way, so it is
            // not sampled and cannot lighten the corner — the rule that keeps an
            // inside corner from reading brighter than the two walls that form it.
            3
        } else {
            u8::from(side_u)
                + u8::from(side_v)
                + u8::from(is_opaque_at(
                    chunk,
                    neighbours,
                    axis,
                    u,
                    v,
                    outward,
                    i + du,
                    j + dv,
                    diagonal_half,
                ))
        };
    }
    Occlusion(levels)
}

/// Whether the voxel at `(outward, i, j)` in the plane's frame hides light, reading
/// across a chunk border when one of the three coordinates leaves this chunk.
///
/// **Two rules, and both are the ones the sweep already follows.** A coordinate that
/// leaves the chunk on **one** axis is read from the neighbour across that face, and an
/// absent neighbour reads as air — the face is lit rather than darkened, which is the
/// same direction of being wrong as emitting a border face rather than culling it, and
/// it is undone by the same remesh when the neighbour arrives. A coordinate that leaves
/// on **two** axes at once belongs to a diagonal chunk, which [`Neighbours`] does not
/// carry and [`mesh_chunk`] is not handed; it reads as air for the same reason. That
/// second case is the only place occlusion is deliberately approximate, and it reaches
/// one corner sample of the faces that run along a chunk's twelve edges — never the
/// faces themselves, which are culled and lit against the six neighbours in full.
#[expect(
    clippy::too_many_arguments,
    reason = "the sweep's coordinate frame, as an `isize` triple that may leave the chunk"
)]
fn is_opaque_at(
    chunk: &VoxelChunk,
    neighbours: &Neighbours,
    axis: usize,
    u: usize,
    v: usize,
    outward: isize,
    i: isize,
    j: isize,
    half: [u8; 3],
) -> bool {
    let size = chunk.size();
    let mut cell = [0isize; 3];
    cell[axis] = outward;
    cell[u] = i;
    cell[v] = j;

    let mut leaves = None;
    for (a, coordinate) in cell.iter().enumerate() {
        if *coordinate < 0 || *coordinate >= size as isize {
            if leaves.is_some() {
                return false;
            }
            leaves = Some(a);
        }
    }

    let block = match leaves {
        None => chunk.block(cell.map(|coordinate| coordinate as usize)),
        Some(a) => {
            let positive = cell[a] >= size as isize;
            let Some(across) = neighbours.across(a, positive, size) else {
                return false;
            };
            cell[a] = if positive { 0 } else { size as isize - 1 };
            across.block(cell.map(|coordinate| coordinate as usize))
        }
    };

    palette::is_opaque(block) && palette::occupies_half(block, half)
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

            // Every cell of this rectangle carried the same `Face`, occlusion
            // included — that is what the mask key buys — so the merged quad's four
            // corners are the four any one of its cells had.
            mesh.push_shaded_quad(
                quad_corners(axis, u, v, plane, i, j, width, height, face),
                normal(axis, face.positive),
                face.occlusion
                    .shade(palette::linear_rgba(face.block), face.positive),
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
    use std::collections::{BTreeMap, BTreeSet};
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
    fn isolated_slabs_and_stairs_have_a_recorded_quad_census() {
        for slab in [palette::SLATE_SLAB_BOTTOM, palette::SLATE_SLAB_TOP] {
            let mesh = mesh_chunk(&single_block(SIZE, slab), &alone());
            assert_eq!(mesh.quad_count(), 6, "slab {slab}");
            assert!(
                mesh.colors
                    .iter()
                    .all(|color| color[3] == palette::linear_rgba(palette::SLATE_TILE)[3])
            );
        }

        for stair in palette::SLATE_STAIR_NORTH_BOTTOM..=palette::SLATE_STAIR_WEST_TOP {
            let mesh = mesh_chunk(&single_block(SIZE, stair), &alone());
            assert_eq!(mesh.quad_count(), 12, "stair {stair}");
            assert_eq!(quads_by_normal(&mesh).len(), 6, "stair {stair}");
        }
    }

    #[test]
    fn a_cube_beside_a_slab_keeps_only_the_uncovered_half_of_the_shared_face() {
        let mut chunk = air(SIZE);
        chunk.set(4, 4, 4, palette::STONE);
        chunk.set(5, 4, 4, palette::SLATE_SLAB_BOTTOM);
        let mesh = mesh_chunk(&chunk, &alone());

        let shared: Vec<usize> = quads_facing(&mesh, [1.0, 0.0, 0.0])
            .into_iter()
            .filter(|quad| quad_extent(&mesh, *quad, 0) == (5.0, 5.0))
            .collect();
        assert_eq!(shared.len(), 2, "AO splits the two half-cells: {shared:?}");
        assert!(
            shared
                .iter()
                .all(|quad| quad_extent(&mesh, *quad, 1) == (4.5, 5.0))
        );
        assert_eq!(
            shared
                .iter()
                .map(|quad| quad_area(&mesh, *quad))
                .sum::<f32>(),
            0.5
        );
    }

    #[test]
    fn partial_culling_reads_a_shaped_neighbour_across_a_chunk_border() {
        let mut chunk = air(SIZE);
        chunk.set(SIZE - 1, 4, 4, palette::STONE);
        let mut neighbour = air(SIZE);
        neighbour.set(0, 4, 4, palette::SLATE_SLAB_BOTTOM);
        let mesh = mesh_chunk(&chunk, &across(0, true, neighbour));

        let border: Vec<usize> = quads_facing(&mesh, [1.0, 0.0, 0.0])
            .into_iter()
            .filter(|quad| quad_extent(&mesh, *quad, 0) == (SIZE as f32, SIZE as f32))
            .collect();
        assert_eq!(border.len(), 2);
        assert!(
            border
                .iter()
                .all(|quad| quad_extent(&mesh, *quad, 1) == (4.5, 5.0))
        );
        assert_eq!(
            border
                .iter()
                .map(|quad| quad_area(&mesh, *quad))
                .sum::<f32>(),
            0.5
        );
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
    // Ambient occlusion: the corner shading, and what the mask key costs (#628)
    // -----------------------------------------------------------------------

    /// The occlusion level of each of a quad's four vertices, read back out of the
    /// colours the sweep wrote.
    ///
    /// A finished mesh cannot be asked what its occlusion was — the factor is multiplied
    /// into the colour and the level is gone — so this inverts it against the block's own
    /// palette entry. Indexed in the buffers' own order, which is the **winding** order
    /// and therefore not [`Occlusion`]'s `(u, v)` frame; every assertion below is about a
    /// set of four levels or about which vertex sits where, so the two never have to be
    /// reconciled and neither is quietly assumed to be the other.
    fn shade_levels(mesh: &SurfaceMesh, quad: usize, block: BlockId) -> [u8; VERTICES_PER_QUAD] {
        let base = palette::linear_rgba(block);
        // The brightest channel, so a colour with no red is still readable.
        let channel = (0..3)
            .max_by(|a, b| base[*a].total_cmp(&base[*b]))
            .expect("a colour has three channels");
        assert!(
            base[channel] > 0.0,
            "a black block carries no readable shade"
        );

        std::array::from_fn(|vertex| {
            let color = mesh.colors[quad * VERTICES_PER_QUAD + vertex];
            assert_eq!(color[3], base[3], "occlusion changed a vertex's alpha");
            let shade = color[channel] / base[channel];
            OCCLUSION_SHADE
                .iter()
                .position(|level| (level - shade).abs() < 1e-4)
                .unwrap_or_else(|| panic!("quad {quad} vertex {vertex} is shaded {shade}"))
                as u8
        })
    }

    /// A floor one block deep filling the chunk's y = 0 layer.
    fn floor(size: usize, block: BlockId) -> VoxelChunk {
        let mut chunk = air(size);
        for z in 0..size {
            for x in 0..size {
                chunk.set(x, 0, z, block);
            }
        }
        chunk
    }

    #[test]
    fn a_block_alone_in_the_air_is_occluded_nowhere() {
        // The control: with nothing beside it, every vertex keeps the palette's colour
        // exactly. Level 0 multiplies by 1.0, which changes no bit, so this is `==` and
        // not an epsilon — a shade curve that did not start at 1.0 would show up here.
        let mesh = mesh_chunk(&single_block(SIZE, palette::STONE), &alone());

        assert_eq!(mesh.quad_count(), 6);
        for color in &mesh.colors {
            assert_eq!(*color, palette::linear_rgba(palette::STONE));
        }
    }

    #[test]
    fn a_flat_wall_with_nothing_near_it_is_unshaded_and_still_merges() {
        // The acceptance criterion about a large flat surface: no occlusion and no
        // banding. The floor still merges to one quad per direction, which is the part a
        // mask key gone wrong would break — a key that varied across a flat surface
        // would answer 1024 here rather than 6.
        let mesh = mesh_chunk(&floor(SIZE, palette::STONE), &alone());

        assert_eq!(mesh.quad_count(), 6, "the slab still merges per direction");
        for color in &mesh.colors {
            assert_eq!(*color, palette::linear_rgba(palette::STONE));
        }
    }

    #[test]
    fn a_voxel_on_the_outward_side_darkens_exactly_the_two_corners_it_touches() {
        // What "the three voxels touching that corner on the outward side" means, at the
        // smallest size that has an answer. The block at (4, 4, 4) keeps its top face;
        // the block diagonally above it along x sits in that face's outward layer,
        // edge-on to two of its four corners and to neither of the other two.
        //
        // A block merely *beside* it, at the same height, would darken nothing: it is not
        // in the layer the face points at. That is the algorithm rather than an omission,
        // and it is why the fixture is a step and not a pair.
        let mut chunk = air(SIZE);
        chunk.set(4, 4, 4, palette::STONE);
        chunk.set(5, 5, 4, palette::STONE);
        let mesh = mesh_chunk(&chunk, &alone());

        let lower = quads_facing(&mesh, [0.0, 1.0, 0.0])
            .into_iter()
            .find(|quad| quad_extent(&mesh, *quad, 1) == (5.0, 5.0))
            .expect("the lower block's top face");

        let levels = shade_levels(&mesh, lower, palette::STONE);
        for (vertex, level) in levels.into_iter().enumerate() {
            let x = mesh.positions[lower * VERTICES_PER_QUAD + vertex][0];
            // One occluder, edge-on, and no diagonal behind it: level 1 on the two
            // corners at the far x, nothing on the two at the near one.
            assert_eq!(level, u8::from(x == 5.0), "vertex {vertex} at x = {x}");
        }
    }

    #[test]
    fn a_top_slab_does_not_cast_the_full_cubes_ambient_occlusion() {
        let mut cube = air(SIZE);
        cube.set(4, 4, 4, palette::STONE);
        cube.set(5, 5, 4, palette::STONE);
        let mut slab = air(SIZE);
        slab.set(4, 4, 4, palette::STONE);
        slab.set(5, 5, 4, palette::SLATE_SLAB_TOP);

        let top = |mesh: &SurfaceMesh| {
            quads_facing(mesh, [0.0, 1.0, 0.0])
                .into_iter()
                .find(|quad| quad_extent(mesh, *quad, 1) == (5.0, 5.0))
                .expect("the lower block's top face")
        };
        let cube = mesh_chunk(&cube, &alone());
        let slab = mesh_chunk(&slab, &alone());
        let cube_levels = shade_levels(&cube, top(&cube), palette::STONE);
        let slab_levels = shade_levels(&slab, top(&slab), palette::STONE);

        assert!(cube_levels.into_iter().any(|level| level > 0));
        assert_eq!(
            slab_levels, [0; VERTICES_PER_QUAD],
            "the empty lower half of a top slab must not occlude the corner below it"
        );
    }

    #[test]
    fn a_staircase_gains_a_dark_seam_along_the_base_of_each_riser() {
        // The reported bug, as an assertion. A staircase of one block type: column x is
        // solid from y = 0 to y = x, spanning every z. Every riser faces -x, so they
        // share one normal and one block colour and were one flat rectangle with no seam
        // anywhere until the camera moved.
        //
        // Each riser is now darker at its foot than at its head: the step below is
        // edge-on to its two bottom corners, and nothing at all is near the top two.
        const STEPS: usize = 6;
        let mut chunk = air(SIZE);
        for x in 0..STEPS {
            for z in 0..SIZE {
                for y in 0..=x {
                    chunk.set(x, y, z, palette::STONE);
                }
            }
        }
        let mesh = mesh_chunk(&chunk, &alone());

        // Every riser but the first, which stands on the chunk floor with no step below.
        let risers: Vec<usize> = quads_facing(&mesh, [-1.0, 0.0, 0.0])
            .into_iter()
            .filter(|quad| quad_extent(&mesh, *quad, 1).0 > 0.0)
            .collect();
        assert!(!risers.is_empty(), "the fixture has no risers to read");

        let mut fully_seamed = 0;
        for riser in risers {
            let (foot_y, _) = quad_extent(&mesh, riser, 1);
            let levels = shade_levels(&mesh, riser, palette::STONE);
            let (mut foot, mut head) = (Vec::new(), Vec::new());
            for (vertex, level) in levels.into_iter().enumerate() {
                let y = mesh.positions[riser * VERTICES_PER_QUAD + vertex][1];
                if y == foot_y {
                    foot.push(level)
                } else {
                    head.push(level)
                }
            }

            assert_eq!((foot.len(), head.len()), (2, 2), "{levels:?}");
            assert_eq!(
                head,
                vec![0, 0],
                "the head of a riser is darkened: {levels:?}"
            );
            assert!(
                foot.iter().all(|level| *level > 0),
                "the base of a riser is not darkened: {levels:?}"
            );
            if foot == vec![2, 2] {
                fully_seamed += 1;
            }
        }

        // The riser is cut into three along z: the run down the middle sees the step
        // below on both diagonals, and the two cells on the chunk's z borders see one
        // of theirs across a chunk this mesher was handed none of. That split is the
        // mask key doing its job, and the middle run is the seam a player reads.
        assert_eq!(
            fully_seamed,
            STEPS - 1,
            "one fully occluded run per riser above the first"
        );
    }

    #[test]
    fn the_occlusion_of_two_faces_is_what_decides_whether_they_merge() {
        // Straight at the key rather than through a fixture. `Face` is compared with
        // `Eq` during the merge, so two faces alike in every other field and unlike at
        // one corner must not compare equal — that comparison is the whole mechanism,
        // and it is the same discipline `WaterFlow` is in the key for.
        let face = Face {
            block: palette::STONE,
            positive: true,
            geometry: FaceGeometry::Full,
            flow: None,
            occlusion: Occlusion::NONE,
        };

        assert_eq!(face, Face { ..face });
        assert_ne!(
            face,
            Face {
                occlusion: Occlusion([0, 0, 0, 1]),
                ..face
            },
            "a face darkened at one corner merged with one that is not"
        );
    }

    #[test]
    fn a_floor_stops_merging_where_something_stands_on_it() {
        // The mask-key criterion as a before and after on one fixture, and the quad cost
        // the issue asks to be reported. A bare floor is one quad; put a single block on
        // it and the cells around that block are lit differently from the rest, so the
        // floor cannot be one quad any more.
        let bare = floor(SIZE, palette::STONE);
        let mut occupied = bare.clone();
        occupied.set(SIZE / 2, 1, SIZE / 2, palette::STONE);

        let bare = mesh_chunk(&bare, &alone());
        let occupied = mesh_chunk(&occupied, &alone());

        assert_eq!(
            quads_facing(&bare, [0.0, 1.0, 0.0]).len(),
            1,
            "an unoccluded floor is one merged quad"
        );
        let split = quads_facing(&occupied, [0.0, 1.0, 0.0]).len();
        assert!(
            split > 1,
            "the floor merged across cells lit differently: {split} quads"
        );
    }

    #[test]
    fn a_border_face_reads_the_neighbour_it_was_handed_and_air_when_it_was_handed_none() {
        // The border rule, both ways round. A block on the chunk's -x wall has a top
        // face whose outward layer runs across that border. Handed no neighbour it reads
        // as air and the face is unoccluded — the same over-draw convention the sweep
        // already uses for the face itself, and undone by the same remesh. Handed a
        // solid one it is occluded, so the darkening crosses the border rather than
        // stopping at it.
        let mut chunk = air(SIZE);
        chunk.set(0, 4, 4, palette::STONE);

        let unknown = mesh_chunk(&chunk, &alone());
        let top = quads_facing(&unknown, [0.0, 1.0, 0.0]);
        assert_eq!(top.len(), 1);
        assert_eq!(
            shade_levels(&unknown, top[0], palette::STONE),
            [0; VERTICES_PER_QUAD],
            "an absent neighbour occluded something"
        );

        let known = mesh_chunk(&chunk, &across(0, false, solid(SIZE, palette::STONE)));
        let top = quads_facing(&known, [0.0, 1.0, 0.0]);
        assert_eq!(top.len(), 1);
        let levels = shade_levels(&known, top[0], palette::STONE);
        for (vertex, level) in levels.into_iter().enumerate() {
            let x = known.positions[top[0] * VERTICES_PER_QUAD + vertex][0];
            // The neighbour is solid throughout, so the corners on the border see an
            // edge-on voxel and the diagonal behind it: level 2 there, nothing opposite.
            assert_eq!(level, if x == 0.0 { 2 } else { 0 }, "vertex at x = {x}");
        }
    }

    #[test]
    fn two_loaded_chunks_show_no_seam_where_their_floors_meet() {
        // The border acceptance criterion on the shape that would show a failure. The
        // floor continues into the chunk next door, occlusion is sampled across the
        // border, so the border cells see the same flat floor the interior ones do and
        // the whole top still merges into one unshaded quad. A mesher that read air
        // across the border would draw a dark stripe one block wide along every chunk
        // edge — a seam of exactly the kind this issue exists to remove.
        let floor = floor(SIZE, palette::STONE);
        let mesh = mesh_chunk(&floor, &surrounded_by(&floor));

        let top = quads_facing(&mesh, [0.0, 1.0, 0.0]);
        assert_eq!(top.len(), 1, "the floor's top split at the border");
        assert_eq!(quad_area(&mesh, top[0]), (SIZE * SIZE) as f32);
        assert_eq!(
            shade_levels(&mesh, top[0], palette::STONE),
            [0; VERTICES_PER_QUAD],
            "the border cells were darkened by a chunk that is right there"
        );
    }

    #[test]
    fn occlusion_never_reaches_the_water_or_the_cover_half() {
        // The out-of-scope rule, asserted rather than trusted. Occlusion is the opaque
        // sweep's; a water or cover quad carries one colour repeated four times, which is
        // exactly what `Occlusion::NONE` and `push_quad` between them guarantee. Read as
        // "the four vertices of every quad agree", which is the property that breaks the
        // moment either half starts asking for a corner value.
        let mut chunk = air(SIZE);
        for z in 0..SIZE {
            for x in 0..SIZE {
                chunk.set(x, 0, z, palette::STONE);
                chunk.set(x, 1, z, palette::WATER);
            }
        }
        chunk.set(4, 1, 4, palette::FLOWER_RED);
        // A ledge over the water, so the water and the cover both have something solid
        // beside them that the opaque half is being darkened by.
        chunk.set(6, 2, 6, palette::STONE);

        let mesh = super::mesh_chunk(&chunk, &alone());
        assert!(
            !mesh.water.is_empty() && !mesh.cover.is_empty(),
            "the fixture is inert"
        );

        for (half, surface) in [("water", &mesh.water), ("cover", &mesh.cover)] {
            for quad in 0..surface.quad_count() {
                let corners = &surface.colors[quad * VERTICES_PER_QUAD..][..VERTICES_PER_QUAD];
                assert!(
                    corners.iter().all(|color| *color == corners[0]),
                    "the {half} half's quad {quad} carries four colours: {corners:?}"
                );
            }
        }
    }

    /// A chunk shaped like ground a player walks over: a ridged stone body under a grass
    /// skin, with air above it. The same shape the streaming measurement in `render.rs`
    /// streams, so the counts here and the totals there are about the same ground.
    fn ridged_terrain(size: usize) -> VoxelChunk {
        let mut chunk = air(size);
        for z in 0..size {
            for x in 0..size {
                let (x64, z64) = (x as i64, z as i64);
                let height = 12
                    + (x64 * 7 + z64 * 13).rem_euclid(9) as usize
                    + (x64 * z64).rem_euclid(3) as usize;
                for y in 0..height {
                    chunk.set(x, y, z, palette::STONE);
                }
                chunk.set(x, height - 1, z, palette::GRASS);
            }
        }
        chunk
    }

    #[test]
    fn the_quad_count_of_a_chunk_of_terrain_is_recorded() {
        // The regression the issue asks for, and the number its description reports.
        // Occlusion in the mask key raises the quad count — that is the price of the
        // seam, paid once — and this is where a later change that multiplies it *again*
        // has to say so in a diff rather than in a frame time nobody is watching.
        //
        // Meshed alone, so the number is a property of these voxels and not of whichever
        // neighbourhood a test happened to build.
        const BEFORE_OCCLUSION: usize = 4642;
        const WITH_OCCLUSION: usize = 7637;

        let mesh = mesh_chunk(&ridged_terrain(SIZE), &alone());

        assert_eq!(
            mesh.quad_count(),
            WITH_OCCLUSION,
            "the corner key was worth {BEFORE_OCCLUSION} quads before #628"
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
    fn a_wall_is_not_somewhere_water_can_go() {
        // A wall on one side does not make the water climb the other: the open side
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
    fn a_source_contributes_only_where_it_feeds() {
        let plain = flow_of(
            palette::WATER_FLOW4,
            [
                palette::WATER_FLOW4,
                palette::WATER,
                palette::WATER_FLOW4,
                palette::WATER_FLOW4,
            ],
            palette::AIR,
        );
        assert_eq!(plain.vector(), [-1.0, 0.0], "lake water feeds every side");

        let feeding_current = flow_of(
            palette::WATER_FLOW4,
            [
                palette::WATER_FLOW4,
                palette::WATER_CURRENT_XNEG,
                palette::WATER_FLOW4,
                palette::WATER_FLOW4,
            ],
            palette::AIR,
        );
        assert_eq!(feeding_current.vector(), [-1.0, 0.0]);

        let passing_current = flow_of(
            palette::WATER_FLOW4,
            [
                palette::WATER_FLOW4,
                palette::WATER_CURRENT_ZPOS,
                palette::WATER_FLOW4,
                palette::WATER_FLOW4,
            ],
            palette::AIR,
        );
        assert_eq!(
            passing_current.vector(),
            [0.0, 0.0],
            "a southbound source east of the voxel does not feed it"
        );
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
            [1.0, 0.0],
            "a plain source across the border feeds east, into this voxel and onward"
        );
    }

    /// Which colour every quad of a surface carries, counted. The census the geometry
    /// tests below read: a flower is four named tones and a bush is two, and how many
    /// quads each one covers is the whole of what "a stem, leaves and a corolla" means
    /// to a test with no screen.
    fn quads_by_colour(mesh: &SurfaceMesh) -> BTreeMap<[u32; 4], usize> {
        let mut counts = BTreeMap::new();
        for quad in 0..mesh.quad_count() {
            let colour = mesh.colors[quad * VERTICES_PER_QUAD];
            *counts.entry(colour.map(f32::to_bits)).or_insert(0) += 1;
        }
        counts
    }

    fn tone(rgb: [f32; 3]) -> [u32; 4] {
        [rgb[0], rgb[1], rgb[2], 1.0].map(f32::to_bits)
    }

    /// Every quad of `mesh` is wound the way its declared normal says it is.
    ///
    /// The check that catches a face drawn inside-out without a screen, and the cover
    /// material's `cull_mode: None` is why it has to be made here: a reversed quad is
    /// still drawn, so the mistake would show up in the lighting and not in the picture.
    fn winding_agrees_with_every_normal(mesh: &SurfaceMesh) {
        for quad in 0..mesh.quad_count() {
            let winding = winding_normal(mesh, quad);
            let declared = mesh.normals[quad * VERTICES_PER_QUAD];
            let length =
                (winding[0] * winding[0] + winding[1] * winding[1] + winding[2] * winding[2])
                    .sqrt();
            assert!(length > 1e-6, "quad {quad} is degenerate");
            for axis in 0..3 {
                assert!(
                    (winding[axis] / length - declared[axis]).abs() < 1e-5,
                    "quad {quad} is wound against its normal: {winding:?} vs {declared:?}"
                );
            }
        }
    }

    /// Every quad of `mesh` lies inside the voxel with minimum corner `corner`.
    fn stays_inside_the_voxel(mesh: &SurfaceMesh, corner: [f32; 3]) {
        for quad in 0..mesh.quad_count() {
            for (axis, floor) in corner.into_iter().enumerate() {
                let (minimum, maximum) = quad_extent(mesh, quad, axis);
                assert!(
                    minimum >= floor - 1e-5 && maximum <= floor + 1.0 + 1e-5,
                    "quad {quad} leaves its voxel on axis {axis}: {minimum}..{maximum}"
                );
            }
        }
    }

    #[test]
    fn one_flower_is_a_stem_leaves_and_a_corolla_in_the_cover_half_and_nothing_else() {
        // #634's whole geometry, in the one chunk that isolates it. The grass keeps its
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

        // Four parts, four colours, and the count of each is what says which part is
        // which: two stem blades, two leaves, five petals, two eye blades.
        assert_eq!(
            quads_by_colour(&mesh.cover),
            BTreeMap::from([
                (tone(palette::STEM_LINEAR), 2),
                (tone(palette::LEAF_LINEAR), 2),
                (tone(palette::FLOWER_CENTRE_LINEAR), 2),
                (
                    palette::linear_rgba(palette::FLOWER_RED).map(f32::to_bits),
                    COVER_PETALS
                ),
            ]),
            "a stem, a pair of leaves, a corolla of separate petals and an eye"
        );

        // The petals and the leaves are the parts that are neither vertical nor
        // axis-aligned, and every one of them faces upward. A corolla wound the other way
        // round is a bowl seen from underneath — lit from below, dark from above — which
        // is the failure `push_radial_blade`'s corner order exists to prevent and the one
        // a screenshot shows and a quad count does not.
        let petal = palette::linear_rgba(palette::FLOWER_RED);
        let leaf = opaque(palette::LEAF_LINEAR);
        let mut upward = 0;
        for quad in 0..mesh.cover.quad_count() {
            let normal = mesh.cover.normals[quad * VERTICES_PER_QUAD];
            let colour = mesh.cover.colors[quad * VERTICES_PER_QUAD];
            let radial = colour == petal || colour == leaf;
            assert_eq!(
                radial,
                normal[1] > 0.5,
                "quad {quad} faces {normal:?} with colour {colour:?}"
            );
            upward += usize::from(radial);
        }
        assert_eq!(upward, COVER_PETALS + 2, "the corolla and the two leaves");

        winding_agrees_with_every_normal(&mesh.cover);
        stays_inside_the_voxel(&mesh.cover, [4.0, 5.0, 6.0]);
        // And it stands on the floor of that voxel rather than hovering over it.
        assert_eq!(quad_extent(&mesh.cover, 0, 1).0, 5.0);

        // The cover half carries no flow, which is what keeps `SurfaceMesh`'s
        // all-or-nothing invariant true for it and what keeps `to_bevy_mesh` from
        // inserting a UV attribute nothing reads.
        assert!(mesh.cover.flow.is_empty() && mesh.cover.falling.is_empty());
    }

    /// One bush in the fixture voxel, fixed at (4, 5, 6) so a test can recompute the
    /// plant's own dials from [`plant_seed`] and name the parts it asserts about.
    fn one_bush() -> SurfaceMesh {
        let mut chunk = solid(SIZE, palette::GRASS);
        chunk.set(4, 5, 6, palette::BUSH);
        super::mesh_chunk(&chunk, &alone()).cover
    }

    const BUSH_FIXTURE: [usize; 3] = [4, 5, 6];
    const BUSH_FIXTURE_FLOOR: [f32; 3] = [4.0, 5.0, 6.0];
    fn fixture_seed() -> u32 {
        plant_seed(BUSH_FIXTURE[0], BUSH_FIXTURE[1], BUSH_FIXTURE[2])
    }

    /// The quads of one part, in the order [`push_bush`] emits them.
    fn quads_coloured(mesh: &SurfaceMesh, colour: [f32; 4]) -> Vec<usize> {
        (0..mesh.quad_count())
            .filter(|quad| mesh.colors[quad * VERTICES_PER_QUAD] == colour)
            .collect()
    }

    /// Half the width a ribbon is drawn at, at one of its two ends. The near pair is
    /// corners 0 and 1 and the far pair is 2 and 3, for every primitive in the cover half.
    fn drawn_half_width(mesh: &SurfaceMesh, quad: usize, far: bool) -> f32 {
        let corners = &mesh.positions[quad * VERTICES_PER_QUAD..][..VERTICES_PER_QUAD];
        let (left, right) = if far {
            (corners[3], corners[2])
        } else {
            (corners[0], corners[1])
        };
        ((right[0] - left[0]).powi(2) + (right[1] - left[1]).powi(2) + (right[2] - left[2]).powi(2))
            .sqrt()
            / 2.0
    }

    /// The middle of a quad's near or far edge: the centre line of a ribbon or a midrib.
    fn centre_line(mesh: &SurfaceMesh, quad: usize, far: bool) -> [f32; 3] {
        let corners = &mesh.positions[quad * VERTICES_PER_QUAD..][..VERTICES_PER_QUAD];
        let pair = if far {
            [corners[2], corners[3]]
        } else {
            [corners[0], corners[1]]
        };
        [
            (pair[0][0] + pair[1][0]) * 0.5,
            (pair[0][1] + pair[1][1]) * 0.5,
            (pair[0][2] + pair[1][2]) * 0.5,
        ]
    }

    fn distance(from: [f32; 3], to: [f32; 3]) -> f32 {
        ((to[0] - from[0]).powi(2) + (to[1] - from[1]).powi(2) + (to[2] - from[2]).powi(2)).sqrt()
    }

    /// The yaw a leaf was pushed at, read back from the direction of its petiole edge.
    ///
    /// `push_leaf` lays every station's width along `(-sin yaw, 0, cos yaw)`, so corner 1
    /// minus corner 0 is that vector scaled — which makes the phyllotaxis assertion a
    /// measurement of the drawn geometry rather than a repetition of the source.
    fn leaf_yaw(mesh: &SurfaceMesh, quad: usize) -> f32 {
        let corners = &mesh.positions[quad * VERTICES_PER_QUAD..][..VERTICES_PER_QUAD];
        let across = [corners[1][0] - corners[0][0], corners[1][2] - corners[0][2]];
        (-across[0]).atan2(across[1])
    }

    /// The shortest angle between two yaws, in `[0, PI]`.
    fn angle_between(first: f32, second: f32) -> f32 {
        let mut difference = (first - second).abs() % std::f32::consts::TAU;
        if difference > std::f32::consts::PI {
            difference = std::f32::consts::TAU - difference;
        }
        difference
    }

    /// The wood of one shoot: its [`BUSH_SHOOT_SEGMENTS`] segments then its fork's, two
    /// ribbons each, in the order `push_shoot` pushes them.
    fn shoot_wood(mesh: &SurfaceMesh, shoot: usize) -> Vec<usize> {
        let quads_per_shoot = (BUSH_SHOOT_SEGMENTS + BUSH_FORK_SEGMENTS) * 2;
        quads_coloured(mesh, palette::linear_rgba(palette::LOG))[shoot * quads_per_shoot..]
            [..quads_per_shoot]
            .to_vec()
    }

    /// The centre line of one shoot, foot to tip, read out of its `side` ribbons.
    fn shoot_joints(mesh: &SurfaceMesh, shoot: usize) -> Vec<[f32; 3]> {
        let wood = shoot_wood(mesh, shoot);
        let mut joints = vec![centre_line(mesh, wood[0], false)];
        for segment in 0..BUSH_SHOOT_SEGMENTS {
            joints.push(centre_line(mesh, wood[segment * 2], true));
        }
        joints
    }

    fn polyline_length(joints: &[[f32; 3]]) -> f32 {
        joints
            .windows(2)
            .map(|pair| distance(pair[0], pair[1]))
            .sum()
    }

    #[test]
    fn a_meadow_bush_is_three_forked_shoots_ten_leaves_and_three_specks() {
        // A bush is drawn per voxel rather than swept, which means the opaque sweep must
        // stop drawing its cube — and the grass under it must start drawing the top face
        // that cube used to cull. **This comment used to open by saying `world.Bush` is
        // `Solid` and that the drawing therefore had to span the cube collision uses.**
        // #874 made it `Cover`, so a body passes through it and that constraint is gone
        // along with the assertion that enforced it; what is left is the rendering half,
        // which never depended on solidity.
        let mut chunk = solid(SIZE, palette::GRASS);
        chunk.set(4, 5, 6, palette::AIR);
        let hole = super::mesh_chunk(&chunk, &alone());
        chunk.set(4, 5, 6, palette::BUSH);
        let mesh = super::mesh_chunk(&chunk, &alone());

        assert_eq!(
            mesh.opaque, hole.opaque,
            "a bush is drawn by itself, so the sweep sees the same hole as before"
        );
        assert!(mesh.water.is_empty());
        assert_eq!(mesh.cover.quad_count(), QUADS_PER_BUSH);
        // What `client/AGENTS.md`'s quad budget is written down as, and #835's ceiling.
        assert_eq!(QUADS_PER_BUSH, 66);
        const { assert!(QUADS_PER_BUSH <= 96) };

        assert_eq!(
            quads_by_colour(&mesh.cover),
            BTreeMap::from([
                (
                    palette::linear_rgba(palette::BUSH).map(f32::to_bits),
                    BUSH_BASE_LEAVES * LEAF_SEGMENTS
                ),
                (
                    tone(palette::BUSH_CROWN_LINEAR),
                    BUSH_SHOOTS * BUSH_LEAVES_PER_SHOOT * LEAF_SEGMENTS
                ),
                (
                    palette::linear_rgba(palette::LOG).map(f32::to_bits),
                    BUSH_SHOOTS * (BUSH_SHOOT_SEGMENTS + BUSH_FORK_SEGMENTS) * 2
                ),
                (
                    palette::linear_rgba(palette::FLOWER_RED).map(f32::to_bits),
                    2
                ),
                (
                    palette::linear_rgba(palette::FLOWER_YELLOW).map(f32::to_bits),
                    2
                ),
                (
                    palette::linear_rgba(palette::FLOWER_BLUE).map(f32::to_bits),
                    2
                ),
            ]),
            "a low leaf tangle, forked tapering shoots, two leaves on each and three \
             tip flowers"
        );

        let repeated = super::mesh_chunk(&chunk, &alone());
        assert_eq!(mesh.cover, repeated.cover, "a remesh reshuffled the bush");

        winding_agrees_with_every_normal(&mesh.cover);
        stays_inside_the_voxel(&mesh.cover, BUSH_FIXTURE_FLOOR);

        // The cover half carries no flow, which is what keeps `SurfaceMesh`'s
        // all-or-nothing invariant true for it.
        assert!(mesh.cover.flow.is_empty() && mesh.cover.falling.is_empty());
    }

    #[test]
    fn a_sector_is_always_wider_than_the_guard_pair_it_has_to_hold() {
        // **The claim the sweep below cannot make.** A sweep answers for the seeds it
        // draws; this answers for every seed there is, because the narrowest sector the
        // normalisation can produce is a closed form: one shoot at the minimum with every
        // other at the maximum. Written as it is rather than as a number, so tuning either
        // bound is what re-evaluates it.
        let narrowest = std::f32::consts::TAU * BUSH_SHOOT_SECTOR_MIN
            / (BUSH_SHOOT_SECTOR_MIN + (BUSH_SHOOTS - 1) as f32 * BUSH_SHOOT_SECTOR_MAX);
        assert!(
            narrowest >= 2.0 * BUSH_SHOOT_GUARD,
            "the narrowest sector is {narrowest} rad and has to hold two guards of \
             {BUSH_SHOOT_GUARD}: `free` goes negative and a shoot is placed inside the \
             clearance its feet spacing is derived from"
        );

        // And the guard is what it is because of this: two feet at BUSH_ROOT_SPREAD from
        // the axis, that angle apart, are this far apart on the floor.
        let closest = 2.0 * BUSH_ROOT_SPREAD * BUSH_SHOOT_GUARD.sin();
        assert!(
            closest >= BUSH_ROOT_SPREAD,
            "two feet a guard pair apart are only {closest} apart, inside \
             BUSH_ROOT_SPREAD"
        );
    }

    #[test]
    fn every_meadow_bush_leaves_three_separate_feet_and_one_leading_shoot() {
        // The umbrella frame's arithmetic was `i * TAU / n` from one hub with under a
        // fifth of a radian of wobble, which cannot break a fourfold symmetry and gave
        // every cane the same length besides. **The sweep is every plant in the first eight
        // voxels of each axis**: `plant_seed` is a pure function of them, so this is a
        // fixed set of 512 bushes and not a sample.
        let third = std::f32::consts::TAU / BUSH_SHOOTS as f32;
        let (mut narrowest, mut widest) = (f32::INFINITY, f32::NEG_INFINITY);
        for x in 0..8 {
            for y in 0..8 {
                for z in 0..8 {
                    let seed = plant_seed(x, y, z);
                    let yaws = bush_shoot_yaws(seed);
                    let foot = |yaw: f32| {
                        [
                            BUSH_ROOT_SPREAD * yaw.cos(),
                            0.0,
                            BUSH_ROOT_SPREAD * yaw.sin(),
                        ]
                    };
                    for first in 0..BUSH_SHOOTS {
                        for second in first + 1..BUSH_SHOOTS {
                            let separation = angle_between(yaws[first], yaws[second]);
                            assert!(
                                separation >= 2.0 * BUSH_SHOOT_GUARD - 1e-4,
                                "({x},{y},{z}) shoots {first} and {second} are \
                                 {separation} apart, closer than the guard allows"
                            );
                            narrowest = narrowest.min(separation);
                            widest = widest.max(separation);
                            let gap = distance(foot(yaws[first]), foot(yaws[second]));
                            assert!(
                                gap >= BUSH_ROOT_SPREAD,
                                "({x},{y},{z}) feet {first} and {second} are {gap} apart, \
                                 inside BUSH_ROOT_SPREAD"
                            );
                        }
                    }

                    // Apical dominance, and it is a property of the two vigour bands
                    // rather than of a lucky seed.
                    let mut chunk = air(SIZE);
                    chunk.set(x, y, z, palette::BUSH);
                    let mesh = super::mesh_chunk(&chunk, &alone()).cover;
                    let lengths: Vec<f32> = (0..BUSH_SHOOTS)
                        .map(|shoot| polyline_length(&shoot_joints(&mesh, shoot)))
                        .collect();
                    let longest = lengths.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                    let shortest = lengths.iter().copied().fold(f32::INFINITY, f32::min);
                    assert!(
                        longest >= shortest * 1.3,
                        "({x},{y},{z}) has no leading shoot: {lengths:?}"
                    );
                    let leader = bush_leader(seed);
                    assert!(
                        (lengths[leader] - longest).abs() < 1e-5,
                        "({x},{y},{z}) the shoot drawn from the leader's vigour band is \
                         not the longest: {lengths:?}, leader {leader}"
                    );
                }
            }
        }

        // **The even partition is a property of the population, and that is the level it
        // is asserted at.** One bush whose two shoots happen to sit a third of the turn
        // apart is a coincidence and not the frame this replaces — (1, 0, 1) is such a
        // bush, and a per-pair "never exactly a third" assertion fails on it while
        // describing nothing. What the frame could not do is vary.
        assert!(
            narrowest < third - 0.3 && widest > third + 0.3,
            "shoot separations only ran {narrowest}..{widest} around {third}: the turn is \
             still being partitioned evenly with a wobble on top"
        );

        // And on the one bush every other test here reads, no two shoots are a third of
        // the turn apart at all.
        let fixture = bush_shoot_yaws(fixture_seed());
        for first in 0..BUSH_SHOOTS {
            for second in first + 1..BUSH_SHOOTS {
                assert!(
                    (angle_between(fixture[first], fixture[second]) - third).abs() > 1e-3,
                    "the fixture's shoots {first} and {second} are exactly a third of the \
                     turn apart"
                );
            }
        }
    }

    #[test]
    fn every_meadow_shoot_tapers_from_foot_to_tip_and_forks_a_shorter_thinner_child() {
        // Branch order two, and wood that is wood rather than wire. Both are read back out
        // of the drawn quads: a constant width from root to tip would show up here as a
        // flat profile, and a fork drawn at the parent's width as an equal one.
        let mesh = one_bush();
        for shoot in 0..BUSH_SHOOTS {
            let wood = shoot_wood(&mesh, shoot);
            let mut widths = vec![drawn_half_width(&mesh, wood[0], false)];
            for segment in 0..BUSH_SHOOT_SEGMENTS {
                widths.push(drawn_half_width(&mesh, wood[segment * 2], true));
            }
            for pair in widths.windows(2) {
                assert!(
                    pair[1] <= pair[0] + 1e-6,
                    "shoot {shoot} widens along its stroke: {widths:?}"
                );
            }
            assert!(
                *widths.last().expect("a shoot has stations") < widths[0] - 1e-6,
                "shoot {shoot} is drawn at one width from root to tip: {widths:?}"
            );

            // The fork leaves the shoulder, which is station 1 of the parent.
            let fork = &wood[BUSH_SHOOT_SEGMENTS * 2..];
            let at_fork = drawn_half_width(&mesh, fork[0], false);
            assert!(
                at_fork < widths[1] - 1e-6,
                "shoot {shoot}'s child is not thinner at its fork: {at_fork} vs {}",
                widths[1]
            );
            let mut fork_joints = vec![centre_line(&mesh, fork[0], false)];
            for segment in 0..BUSH_FORK_SEGMENTS {
                fork_joints.push(centre_line(&mesh, fork[segment * 2], true));
            }
            let shoulder = shoot_joints(&mesh, shoot)[1];
            for (child, parent) in fork_joints[0].iter().zip(shoulder.iter()) {
                assert!(
                    (child - parent).abs() < 1e-5,
                    "shoot {shoot}'s child does not leave its shoulder"
                );
            }
            let child = polyline_length(&fork_joints);
            let parent = polyline_length(&shoot_joints(&mesh, shoot));
            assert!(
                child < parent - 1e-6,
                "shoot {shoot}'s child is not shorter: {child} vs {parent}"
            );
        }
    }

    #[test]
    fn a_leaf_widens_to_a_point_and_carries_more_than_one_normal() {
        // What replaces `push_radial_blade` for foliage. Its four corners are
        // `inner(-w), inner(+w), outer(+w), outer(-w)` — the same half-width at both ends
        // for every caller — so a leaf drawn with it is a rectangle whatever numbers it is
        // passed. This is the assertion that a leaf is no longer one.
        let mut mesh = SurfaceMesh::default();
        push_leaf(
            &mut mesh,
            [0.5, 0.0, 0.5],
            0.7,
            BUSH_SHOOT_LEAF_PITCH,
            0.30,
            0.06,
            opaque(palette::BUSH_CROWN_LINEAR),
        );

        const { assert!(LEAF_SEGMENTS >= 3) };
        assert_eq!(mesh.quad_count(), LEAF_SEGMENTS);

        let mut widths = vec![drawn_half_width(&mesh, 0, false)];
        for quad in 0..LEAF_SEGMENTS {
            widths.push(drawn_half_width(&mesh, quad, true));
        }
        assert!(
            widths[0] < widths[1] - 1e-6,
            "a leaf must open from its petiole: {widths:?}"
        );
        for pair in widths[1..].windows(2) {
            assert!(
                pair[1] < pair[0] - 1e-6,
                "a leaf must close after its widest station: {widths:?}"
            );
        }
        assert_eq!(
            *widths.last().expect("a leaf has stations"),
            0.0,
            "a leaf must end in a point, not an edge"
        );

        let normals: BTreeSet<[u32; 3]> = (0..mesh.quad_count())
            .map(|quad| mesh.normals[quad * VERTICES_PER_QUAD].map(f32::to_bits))
            .collect();
        assert!(
            normals.len() > 1,
            "every quad of the leaf shares one normal, so the midrib is flat"
        );
        winding_agrees_with_every_normal(&mesh);
    }

    #[test]
    fn a_meadow_bush_tilts_its_leaves_out_of_the_horizontal_fan() {
        // The lever this half of #835 is really buying: the cover material is lit and
        // double-sided, so a leaf whose quads face different ways gets different tones of
        // one green for free. Every leaf normal used to point nearly straight up, which is
        // why the plant shaded as one flat tone whatever its silhouette was.
        let mesh = one_bush();
        let leaves: Vec<usize> = quads_coloured(&mesh, palette::linear_rgba(palette::BUSH))
            .into_iter()
            .chain(quads_coloured(&mesh, opaque(palette::BUSH_CROWN_LINEAR)))
            .collect();
        let tilted: BTreeSet<[u32; 3]> = leaves
            .iter()
            .map(|quad| mesh.normals[quad * VERTICES_PER_QUAD])
            .filter(|normal| normal[1].abs() < 0.8)
            .map(|normal| normal.map(f32::to_bits))
            .collect();
        assert!(
            tilted.len() >= 3,
            "only {} distinct steeply-tilted leaf normals; the foliage is still a \
             horizontal fan",
            tilted.len()
        );
    }

    #[test]
    fn leaves_follow_the_golden_angle_round_a_shoot_and_shrink_toward_its_tip() {
        // Phyllotaxis rather than index parity, which is what the pair of leaves per cane
        // was: a fixed `+-0.72 * FRAC_PI_2` from the cane's own yaw, so every cane wore the
        // same two wings.
        let mesh = one_bush();
        let shoot_leaves = quads_coloured(&mesh, opaque(palette::BUSH_CROWN_LINEAR));
        for shoot in 0..BUSH_SHOOTS {
            let mut sizes = Vec::new();
            let mut yaws = Vec::new();
            for leaf in 0..BUSH_LEAVES_PER_SHOOT {
                let first = shoot_leaves[(shoot * BUSH_LEAVES_PER_SHOOT + leaf) * LEAF_SEGMENTS];
                sizes.push(drawn_half_width(&mesh, first, true));
                yaws.push(leaf_yaw(&mesh, first));
            }
            for pair in sizes.windows(2) {
                assert!(
                    pair[1] <= pair[0] + 1e-6,
                    "shoot {shoot}'s leaves grow toward its tip: {sizes:?}"
                );
            }
            for pair in yaws.windows(2) {
                let turn = angle_between(pair[0], pair[1]);
                let golden = angle_between(GOLDEN_ANGLE, 0.0);
                assert!(
                    (turn - golden).abs() < 1e-4,
                    "shoot {shoot}'s successive leaves are {turn} apart, not the golden \
                     angle {golden}"
                );
            }
        }
    }

    #[test]
    fn a_desert_bramble_has_arches_thorns_and_no_bloom() {
        // `world.DesertShrub` has the bush's shape family and none of its meadow
        // dressing: the old cube leaves the sweep, the sand regains its top face, and
        // the drawn geometry is bare wood with no leaf, flower or saturated accent.
        let mut chunk = solid(SIZE, palette::SAND);
        chunk.set(4, 5, 6, palette::AIR);
        let hole = super::mesh_chunk(&chunk, &alone());
        chunk.set(4, 5, 6, palette::DESERT_SHRUB);
        let mesh = super::mesh_chunk(&chunk, &alone());

        assert_eq!(
            mesh.opaque, hole.opaque,
            "desert scrub is drawn by itself, so the sweep sees the same hole"
        );
        assert!(mesh.water.is_empty());
        assert_eq!(mesh.cover.quad_count(), QUADS_PER_DESERT_BRAMBLE);
        assert_eq!(
            quads_by_colour(&mesh.cover),
            BTreeMap::from([(
                palette::linear_rgba(palette::DESERT_SHRUB).map(f32::to_bits),
                QUADS_PER_DESERT_BRAMBLE,
            )]),
            "bare sun-bleached wood, with no meadow leaf, flower or saturated accent"
        );

        let repeated = super::mesh_chunk(&chunk, &alone());
        assert_eq!(
            mesh.cover, repeated.cover,
            "a remesh reshuffled the desert bramble"
        );

        // The shared primitive emits two ribbons for each of three segments, and the
        // first four groups are the bramble's arches. Reading their centre lines back
        // proves each one climbs through a shoulder and crest before descending.
        let endpoint = |quad: usize, end: bool| {
            let vertices = &mesh.cover.positions[quad * VERTICES_PER_QUAD..][..VERTICES_PER_QUAD];
            let pair = if end {
                [vertices[2], vertices[3]]
            } else {
                [vertices[0], vertices[1]]
            };
            [
                (pair[0][0] + pair[1][0]) * 0.5,
                (pair[0][1] + pair[1][1]) * 0.5,
                (pair[0][2] + pair[1][2]) * 0.5,
            ]
        };
        for cane in 0..BRAMBLE_CANES {
            let first = cane * BRAMBLE_CANE_SEGMENTS * 2;
            let base = endpoint(first, false);
            let shoulder = endpoint(first, true);
            let crest = endpoint(first + 2, true);
            let tip = endpoint(first + 4, true);
            assert!(
                base[1] < shoulder[1] && shoulder[1] < crest[1] && tip[1] < crest[1],
                "cane {cane} is not an arch: {base:?} -> {shoulder:?} -> {crest:?} -> {tip:?}"
            );
        }

        // Beyond those arches, every short branch is still woody geometry: two basal
        // canes, two thorns per arch and the crossed crown thorn.
        let dressing = QUADS_PER_DESERT_BRAMBLE - BRAMBLE_CANES * BRAMBLE_CANE_SEGMENTS * 2;
        assert_eq!(
            dressing,
            DESERT_BASE_CANES * 2
                + BRAMBLE_CANES * DESERT_THORNS_PER_CANE * 2
                + DESERT_CROWN_BLADES
        );

        winding_agrees_with_every_normal(&mesh.cover);
        stays_inside_the_voxel(&mesh.cover, [4.0, 5.0, 6.0]);
    }

    #[test]
    fn every_desert_bramble_vertex_stays_inside_its_own_voxel() {
        // `dial` is chunk-local, so sweep enough coordinates to exercise its several
        // yaw, bend, height and thorn combinations instead of checking one lucky seed.
        for y in 0..4 {
            for z in 0..4 {
                for x in 0..4 {
                    let floor = [x as f32, y as f32, z as f32];
                    let mut mesh = SurfaceMesh::default();
                    push_desert_bramble(&mut mesh, floor, plant_seed(x, y, z));
                    stays_inside_the_voxel(&mesh, floor);
                }
            }
        }
    }

    #[test]
    fn a_winter_bramble_has_bare_canes_and_small_berries() {
        // Unlike the solid meadow bush, `world.WinterBramble` is Cover on the server:
        // the snow beneath it stays visible and a body walks through the sparse canes.
        let mut chunk = air(SIZE);
        chunk.set(4, 4, 6, palette::SNOW);
        let bare = super::mesh_chunk(&chunk, &alone());
        chunk.set(4, 5, 6, palette::WINTER_BRAMBLE);
        let mesh = super::mesh_chunk(&chunk, &alone());

        assert_eq!(
            mesh.opaque, bare.opaque,
            "the shaped bramble changed the snow sweep instead of growing over it"
        );
        assert!(mesh.water.is_empty());
        assert_eq!(mesh.cover.quad_count(), QUADS_PER_WINTER_BRAMBLE);
        assert_eq!(
            quads_by_colour(&mesh.cover),
            BTreeMap::from([
                (
                    palette::linear_rgba(palette::LOG).map(f32::to_bits),
                    BRAMBLE_CANES * BRAMBLE_CANE_SEGMENTS * 2,
                ),
                (
                    palette::linear_rgba(palette::WINTER_BRAMBLE).map(f32::to_bits),
                    WINTER_BRAMBLE_BERRIES * 2,
                ),
            ]),
            "bare wood and berries are the whole winter shape"
        );

        const {
            assert!(WINTER_BERRY_HEIGHT <= 0.05 && WINTER_BERRY_HALF_WIDTH * 2.0 <= 0.05);
        }

        winding_agrees_with_every_normal(&mesh.cover);
        stays_inside_the_voxel(&mesh.cover, [4.0, 5.0, 6.0]);
    }

    #[test]
    fn two_bushes_side_by_side_are_two_bushes() {
        // What #634 is for. A bush used to be an ordinary opaque cube, so the greedy
        // sweep merged a cluster of them into one slab with one flat top; `visitBush` on
        // the server grows clusters of up to three, so that was the common case rather
        // than the corner one. Now each is its own bramble, and the per-voxel jitter
        // puts their cane arches at different heights and angles.
        let mut chunk = air(SIZE);
        chunk.set(4, 5, 6, palette::BUSH);
        chunk.set(5, 5, 6, palette::BUSH);
        let mesh = super::mesh_chunk(&chunk, &alone());

        assert!(
            mesh.opaque.is_empty(),
            "a bush is drawn by nothing but itself"
        );
        assert_eq!(mesh.cover.quad_count(), 2 * QUADS_PER_BUSH);

        // Neither bush leaves its own voxel, and the two are not the same shape shifted
        // by one block: the cane crests alone put a visible step between them.
        let (first, second) = (0..mesh.cover.quad_count()).fold(
            (Vec::new(), Vec::new()),
            |(mut first, mut second), quad| {
                let (low, _) = quad_extent(&mesh.cover, quad, 0);
                if low < 5.0 {
                    first.push(quad)
                } else {
                    second.push(quad)
                }
                (first, second)
            },
        );
        assert_eq!(first.len(), QUADS_PER_BUSH);
        assert_eq!(second.len(), QUADS_PER_BUSH);

        let wood = palette::linear_rgba(palette::LOG);
        let highest_cane = |quads: &[usize]| {
            quads
                .iter()
                .filter(|quad| mesh.cover.colors[**quad * VERTICES_PER_QUAD] == wood)
                .map(|quad| quad_extent(&mesh.cover, *quad, 1).1)
                .fold(f32::NEG_INFINITY, f32::max)
        };
        assert_ne!(
            highest_cane(&first),
            highest_cane(&second),
            "two neighbours grew the same arch"
        );

        // And the two never share a plane, which is what `BUSH_INSET` buys: coincident
        // quads on the boundary would fight the depth buffer along the whole seam.
        let first_high = first
            .iter()
            .map(|quad| quad_extent(&mesh.cover, *quad, 0).1)
            .fold(f32::NEG_INFINITY, f32::max);
        let second_low = second
            .iter()
            .map(|quad| quad_extent(&mesh.cover, *quad, 0).0)
            .fold(f32::INFINITY, f32::min);
        assert!(
            first_high < second_low,
            "{first_high} and {second_low} meet on one plane"
        );
    }

    #[test]
    fn a_flowered_chunk_costs_the_quads_it_is_recorded_as_costing() {
        // The regression the issue asked for: cover is per voxel and unmerged, so every
        // quad a plant gains is paid once per plant in the world. A change that
        // multiplies either shape has to come here and say so.
        let mut chunk = air(SIZE);
        let (mut flowers, mut bushes) = (0, 0);
        for z in (0..SIZE).step_by(4) {
            for x in (0..SIZE).step_by(4) {
                chunk.set(x, 5, z, palette::FLOWER_YELLOW);
                flowers += 1;
                if x % 8 == 0 {
                    chunk.set(x, 5, z + 1, palette::BUSH);
                    bushes += 1;
                }
            }
        }
        let mesh = super::mesh_chunk(&chunk, &alone());

        assert_eq!((flowers, bushes), (64, 32));
        assert_eq!(
            mesh.cover.quad_count(),
            flowers * QUADS_PER_COVER + bushes * QUADS_PER_BUSH
        );
        assert_eq!(mesh.cover.quad_count(), 2816);
    }

    #[test]
    fn a_desert_chunk_costs_the_scrub_quads_and_returns_the_sand_faces() {
        // One scrub in forty columns is about twenty-five in a 32 x 32 surface. Keep
        // that dense case exact here: the cover half pays every unmerged bramble, while
        // taking the old cubes out of the sweep returns the sand tops they hid.
        let mut bare = air(SIZE);
        for z in 0..SIZE {
            for x in 0..SIZE {
                bare.set(x, 4, z, palette::SAND);
            }
        }
        let mut cubes = bare.clone();
        let mut planted = bare.clone();
        let mut shrubs = 0;
        for z in (2..SIZE).step_by(6) {
            for x in (2..SIZE).step_by(6) {
                // THATCH is an ordinary opaque cube and stands in for the old sweep
                // path; DESERT_SHRUB itself cannot take that path after this change.
                cubes.set(x, 5, z, palette::THATCH);
                planted.set(x, 5, z, palette::DESERT_SHRUB);
                shrubs += 1;
            }
        }

        let bare_mesh = super::mesh_chunk(&bare, &alone());
        let cube_mesh = super::mesh_chunk(&cubes, &alone());
        let planted_mesh = super::mesh_chunk(&planted, &alone());
        assert_eq!(shrubs, 25);
        assert_eq!(
            planted_mesh.cover.quad_count(),
            shrubs * QUADS_PER_DESERT_BRAMBLE
        );
        assert_eq!(planted_mesh.cover.quad_count(), 1150);
        assert_eq!(
            planted_mesh.opaque, bare_mesh.opaque,
            "the shaped scrub must return every sand face and leave no cube in the sweep"
        );
        assert!(
            cube_mesh.opaque.quad_count() > planted_mesh.opaque.quad_count(),
            "the old opaque scrub proxy should cost more sweep quads: cube {} vs shaped {}",
            cube_mesh.opaque.quad_count(),
            planted_mesh.opaque.quad_count()
        );
    }

    #[test]
    fn a_winter_chunk_costs_the_quads_it_is_recorded_as_costing() {
        // The measured one-in-256 worldgen density is four candidates per 32x32
        // surface chunk before conifer priority. Keep the per-voxel cost explicit:
        // cover is never merged, so each bramble pays the whole shape.
        let mut chunk = air(SIZE);
        let mut brambles = 0;
        for z in [8, 24] {
            for x in [8, 24] {
                chunk.set(x, 5, z, palette::WINTER_BRAMBLE);
                brambles += 1;
            }
        }
        let mesh = super::mesh_chunk(&chunk, &alone());

        assert_eq!(brambles, 4);
        assert_eq!(mesh.cover.quad_count(), brambles * QUADS_PER_WINTER_BRAMBLE);
        assert_eq!(mesh.cover.quad_count(), 144);
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
        // Three flowers in a row are three flowers, and no two of them are the same
        // shape: the yaw each takes from its own voxel is what stops a meadow reading as
        // one flower stamped across it.
        let yaws: Vec<[f32; 3]> = (0..3)
            .map(|plant| mesh.cover.positions[plant * QUADS_PER_COVER * VERTICES_PER_QUAD + 8])
            .collect();
        assert_ne!(yaws[0][2], yaws[1][2]);
        assert_ne!(yaws[1][2], yaws[2][2]);
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
    fn every_cover_id_grows_geometry_and_no_other_id_does() {
        // The lockstep `palette::is_cover` claims and `palette::is_opaque` leans on.
        // A cover voxel is see-through to the sweep, so it emits no mask face at all;
        // if `build_cover` skipped it too the block would be *invisible*, not a cube.
        // Driven off the palette rather than off a list typed here, so a seventh cover
        // id is caught by this test instead of by somebody noticing a hole in a hill.
        const EDGE: usize = 4;
        for block in palette::PALETTE {
            let mut chunk = air(EDGE);
            chunk.set(2, 2, 2, block);
            let mesh = super::mesh_chunk(&chunk, &alone());
            if palette::is_cover(block) {
                assert!(
                    mesh.cover.quad_count() > 0,
                    "cover block {block} grows no geometry anywhere"
                );
                assert!(
                    mesh.opaque.is_empty() && mesh.water.is_empty(),
                    "cover block {block} is drawn by nothing but the cover half"
                );
            } else {
                assert_eq!(
                    mesh.cover.quad_count(),
                    0,
                    "block {block} is swept as a cube and grows no plant"
                );
            }
        }
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
