//! The first dungeon's own blocks, as geometry: the cobweb, the two lever states and the
//! glowing rune on a lit stone.
//!
//! A child of the mesher rather than more of it, because all three are drawn one voxel at
//! a time the way cover is and share nothing with the sweep but [`SurfaceMesh`]. Each
//! function here is called from a pass the mesher already makes over the chunk, so none
//! of them costs a traversal of its own.
//!
//! **Every vertex stays inside its own voxel, and the rune is the one exception by a
//! hair.** A web and a lever are wholly inside the cell they stand in, which is what lets
//! `ChunkStore::apply_block` remesh only the chunk that owns the edit. A rune is lifted
//! [`RUNE_LIFT`] off the face it is carved in so it never fights that face for depth; it
//! is emitted only while the face it rides on is exposed, which is exactly the condition
//! under which the sweep emits the face itself, so it needs no remesh rule the face does
//! not already have.

use super::{Neighbours, SurfaceMesh, face_normal, opaque, stepped_block};
use crate::world::{BlockId, VoxelChunk, palette};

/// How wide one silk thread is drawn, in blocks. A real thread is far finer; this is
/// the narrowest that still survives a mip level at the distance a web is first seen.
const WEB_THREAD_WIDTH: f32 = 0.014;
/// The spokes radiating from a web's hub.
const WEB_SPOKES: usize = 8;
/// The rings of capture silk wound round the spokes, as fractions of the web's reach.
const WEB_RINGS: [f32; 3] = [0.3, 0.56, 0.84];
/// How far a web reaches from its hub along the plane's horizontal and vertical axes.
/// Short of the voxel's half diagonal so a thread's width stays inside the cell too.
const WEB_REACH: [f32; 2] = [0.64, 0.45];
/// How far a web's hub may sit off the middle of the voxel, vertically.
const WEB_HUB_WANDER: f32 = 0.04;
/// Quads one web plane is made of: a quad per spoke and one per ring segment.
#[cfg(test)]
pub(super) const QUADS_PER_WEB_PLANE: usize = WEB_SPOKES + WEB_RINGS.len() * WEB_SPOKES;
/// A web is two crossed planes, the way a flower's stem is.
#[cfg(test)]
pub(super) const QUADS_PER_WEB: usize = 2 * QUADS_PER_WEB_PLANE;

/// A lever's plinth: a block of dressed stone, as half extents about its centre.
const LEVER_PLINTH_HALF: [f32; 3] = [0.3, 0.14, 0.22];
/// The handle: an iron bar this far across, pivoting on the plinth's top.
const LEVER_HANDLE_HALF_WIDTH: f32 = 0.04;
const LEVER_HANDLE_LENGTH: f32 = 0.52;
/// How far a thrown handle leans from upright, in radians — about thirty-four degrees,
/// towards −z when the lever is off and towards +z when it is on. The two states are the
/// same parts thrown the opposite way, which is how a lever reads at a glance.
const LEVER_THROW: f32 = 0.6;
/// The bronze grip at the handle's tip, as a half extent.
const LEVER_GRIP_HALF: f32 = 0.065;
/// Quads one lever is made of: three boxes.
#[cfg(test)]
pub(super) const QUADS_PER_LEVER: usize = 3 * 6;

/// How far a rune is lifted off the face it is carved in, so the two never share a depth.
pub(super) const RUNE_LIFT: f32 = 0.004;
/// How wide a rune's strokes are drawn.
const RUNE_STROKE_WIDTH: f32 = 0.07;
/// The runes a lit stone may carry, each as strokes `[s0, t0, s1, t1]` in a face's own
/// coordinates: `s` across the face as seen from outside, `t` up it.
const RUNES: [&[[f32; 4]]; 3] = [
    // ᛉ algiz: a stave with two arms raised from its middle.
    &[
        [0.5, 0.18, 0.5, 0.82],
        [0.5, 0.5, 0.28, 0.76],
        [0.5, 0.5, 0.72, 0.76],
    ],
    // ᛏ tiwaz: a stave under an arrowhead.
    &[
        [0.5, 0.18, 0.5, 0.82],
        [0.5, 0.82, 0.29, 0.6],
        [0.5, 0.82, 0.71, 0.6],
    ],
    // ᚠ fehu: a stave with two arms rising to the right.
    &[
        [0.38, 0.18, 0.38, 0.84],
        [0.38, 0.52, 0.68, 0.72],
        [0.38, 0.7, 0.66, 0.88],
    ],
];

/// One cobweb, spun in the voxel whose minimum corner is `floor`.
///
/// Two vertical planes crossing on the voxel's diagonals, as a flower's stem does, each
/// carrying [`WEB_SPOKES`] spokes from a hub and [`WEB_RINGS`] rings of capture silk
/// wound between them. **What makes it read as a web and not a sheet is what it leaves
/// out**: the cover half is opaque, and the space between the threads is where the
/// cave behind shows through, which is the translucency a web has.
///
/// The hub wanders a little and the spokes are turned by a fraction of their spacing,
/// both from `seed`, so a curtain of webs across a passage is not one web drawn five times.
pub(super) fn push_cobweb(mesh: &mut SurfaceMesh, floor: [f32; 3], seed: u32) {
    let color = palette::linear_rgba(palette::COBWEB);
    let hub_rise = 0.5 + (super::dial(seed, 0) - 0.5) * 2.0 * WEB_HUB_WANDER;
    let turn = super::dial(seed, 1) * std::f32::consts::TAU / WEB_SPOKES as f32;
    for (plane, across) in [[1.0f32, 1.0], [1.0, -1.0]].into_iter().enumerate() {
        let norm = std::f32::consts::FRAC_1_SQRT_2;
        let along = [across[0] * norm, 0.0, across[1] * norm];
        // The second plane is turned the other way too, so the two do not share spokes.
        let turn = if plane == 0 { turn } else { -turn };
        let point = |s: f32, t: f32| {
            [
                floor[0] + 0.5 + along[0] * s,
                floor[1] + hub_rise + t,
                floor[2] + 0.5 + along[2] * s,
            ]
        };
        let spoke = |k: usize, reach: f32| {
            let angle = turn + k as f32 * std::f32::consts::TAU / WEB_SPOKES as f32;
            (
                angle.cos() * WEB_REACH[0] * reach,
                angle.sin() * WEB_REACH[1] * reach,
            )
        };
        for k in 0..WEB_SPOKES {
            let (s, t) = spoke(k, 1.0);
            push_thread(mesh, along, point(0.0, 0.0), point(s, t), color);
        }
        for ring in WEB_RINGS {
            for k in 0..WEB_SPOKES {
                let (s0, t0) = spoke(k, ring);
                let (s1, t1) = spoke((k + 1) % WEB_SPOKES, ring);
                push_thread(mesh, along, point(s0, t0), point(s1, t1), color);
            }
        }
    }
}

/// One thread from `from` to `to`, lying in the web plane whose horizontal axis is
/// `along`. Its width is taken perpendicular to the thread inside that plane.
fn push_thread(
    mesh: &mut SurfaceMesh,
    along: [f32; 3],
    from: [f32; 3],
    to: [f32; 3],
    color: [f32; 4],
) {
    let direction = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    // The thread's (s, t) direction in its plane, and the perpendicular to it there.
    let ds = direction[0] * along[0] + direction[2] * along[2];
    let dt = direction[1];
    let length = (ds * ds + dt * dt).sqrt().max(f32::EPSILON);
    let half = WEB_THREAD_WIDTH / 2.0;
    let (ps, pt) = (-dt / length * half, ds / length * half);
    let side = [along[0] * ps, pt, along[2] * ps];
    let corners = [
        [from[0] - side[0], from[1] - side[1], from[2] - side[2]],
        [to[0] - side[0], to[1] - side[1], to[2] - side[2]],
        [to[0] + side[0], to[1] + side[1], to[2] + side[2]],
        [from[0] + side[0], from[1] + side[1], from[2] + side[2]],
    ];
    mesh.push_quad(corners, face_normal(corners), color, None);
}

/// One lever standing in `cell`: a stone plinth, an iron handle pivoting on its top and
/// a bronze grip at the handle's tip, the handle thrown towards −z when `block` is
/// [`palette::LEVER_OFF`] and towards +z when it is [`palette::LEVER_ON`].
///
/// Drawn into the opaque half and wound for back-face culling like the terrain: every
/// piece is a closed box. **The server's body sees a full cube here**, and so does the
/// aiming ray; this is only what the eye is shown of it.
pub(super) fn push_lever(mesh: &mut SurfaceMesh, cell: [usize; 3], block: BlockId) {
    let origin = cell.map(|coordinate| coordinate as f32);
    let at = |x: f32, y: f32, z: f32| [origin[0] + x, origin[1] + y, origin[2] + z];

    let plinth = opaque(palette::LEVER_PLINTH_LINEAR);
    let plinth_centre = at(0.5, LEVER_PLINTH_HALF[1], 0.5);
    push_box(
        mesh,
        plinth_centre,
        [
            [LEVER_PLINTH_HALF[0], 0.0, 0.0],
            [0.0, LEVER_PLINTH_HALF[1], 0.0],
            [0.0, 0.0, LEVER_PLINTH_HALF[2]],
        ],
        plinth,
    );

    let lean = if block == palette::LEVER_ON {
        1.0
    } else {
        -1.0
    };
    let up = [0.0, LEVER_THROW.cos(), lean * LEVER_THROW.sin()];
    let across = [1.0, 0.0, 0.0];
    // across × up, which keeps the box's three axes right-handed.
    let depth = [0.0, -up[2], up[1]];
    let pivot = at(0.5, 2.0 * LEVER_PLINTH_HALF[1], 0.5);
    let scaled = |v: [f32; 3], by: f32| v.map(|c| c * by);
    let offset = |base: [f32; 3], v: [f32; 3], by: f32| {
        [
            base[0] + v[0] * by,
            base[1] + v[1] * by,
            base[2] + v[2] * by,
        ]
    };
    push_box(
        mesh,
        offset(pivot, up, LEVER_HANDLE_LENGTH / 2.0),
        [
            scaled(across, LEVER_HANDLE_HALF_WIDTH),
            scaled(up, LEVER_HANDLE_LENGTH / 2.0),
            scaled(depth, LEVER_HANDLE_HALF_WIDTH),
        ],
        palette::linear_rgba(block),
    );
    push_box(
        mesh,
        offset(pivot, up, LEVER_HANDLE_LENGTH),
        [
            scaled(across, LEVER_GRIP_HALF),
            scaled(up, LEVER_GRIP_HALF),
            scaled(depth, LEVER_GRIP_HALF),
        ],
        opaque(palette::LEVER_GRIP_LINEAR),
    );
}

/// A closed box about `centre` whose three half-extent vectors `axes` are right-handed.
///
/// Each face is wound so the normal [`face_normal`] derives from its corners points out
/// of the box — the property the opaque half's back-face culling needs.
fn push_box(mesh: &mut SurfaceMesh, centre: [f32; 3], axes: [[f32; 3]; 3], color: [f32; 4]) {
    for k in 0..3 {
        let (u, v) = (axes[(k + 1) % 3], axes[(k + 2) % 3]);
        for sign in [1.0f32, -1.0] {
            let face = [
                centre[0] + axes[k][0] * sign,
                centre[1] + axes[k][1] * sign,
                centre[2] + axes[k][2] * sign,
            ];
            let corner = |a: f32, b: f32| {
                [
                    face[0] + u[0] * a + v[0] * b,
                    face[1] + u[1] * a + v[1] * b,
                    face[2] + u[2] * a + v[2] * b,
                ]
            };
            // u × v points along +axes[k] for a right-handed frame, so the far face is
            // walked the other way round.
            let corners = if sign > 0.0 {
                [
                    corner(-1.0, -1.0),
                    corner(1.0, -1.0),
                    corner(1.0, 1.0),
                    corner(-1.0, 1.0),
                ]
            } else {
                [
                    corner(-1.0, -1.0),
                    corner(-1.0, 1.0),
                    corner(1.0, 1.0),
                    corner(1.0, -1.0),
                ]
            };
            mesh.push_quad(corners, face_normal(corners), color, None);
        }
    }
}

/// The glowing rune carved into every exposed side of the lit stone in `cell`, pushed
/// into the glow half.
///
/// A side is exposed when the voxel across it is not swept as an opaque cube: the same
/// test the sweep makes before it draws that side, so a rune is never carved into a face
/// nobody can see. The top and the bottom are left plain — an inscription is read from
/// the side. Which of [`RUNES`] a stone carries is drawn from its cell, and a stone
/// carries the same rune on every side.
pub(super) fn push_rune(
    glow: &mut SurfaceMesh,
    chunk: &VoxelChunk,
    neighbours: &Neighbours,
    cell: [usize; 3],
) {
    let color = opaque(palette::RUNE_GLOW_LINEAR);
    let rune = RUNES[super::plant_seed(cell[0], cell[1], cell[2]) as usize % RUNES.len()];
    let origin = cell.map(|coordinate| coordinate as f32);
    for axis in [0usize, 2] {
        for positive in [false, true] {
            let step = if positive { 1 } else { -1 };
            if palette::is_greedy_opaque(stepped_block(chunk, neighbours, cell, axis, step)) {
                continue;
            }
            let sign = if positive { 1.0 } else { -1.0 };
            let mut outward = [0.0f32; 3];
            outward[axis] = sign;
            // The direction a reader facing this side calls "right": the view direction
            // (−outward) crossed with up.
            let right = [outward[2], 0.0, -outward[0]];
            let mut base = [origin[0] + 0.5, origin[1], origin[2] + 0.5];
            base[axis] += sign * (0.5 + RUNE_LIFT);
            let point = |s: f32, t: f32| {
                [
                    base[0] + right[0] * (s - 0.5),
                    base[1] + t,
                    base[2] + right[2] * (s - 0.5),
                ]
            };
            for stroke in rune {
                let (ds, dt) = (stroke[2] - stroke[0], stroke[3] - stroke[1]);
                let length = (ds * ds + dt * dt).sqrt();
                let half = RUNE_STROKE_WIDTH / 2.0;
                let (ps, pt) = (-dt / length * half, ds / length * half);
                let corners = [
                    point(stroke[0] - ps, stroke[1] - pt),
                    point(stroke[2] - ps, stroke[3] - pt),
                    point(stroke[2] + ps, stroke[3] + pt),
                    point(stroke[0] + ps, stroke[1] + pt),
                ];
                let derived = face_normal(corners);
                let corners = if derived[axis] * sign > 0.0 {
                    corners
                } else {
                    [corners[0], corners[3], corners[2], corners[1]]
                };
                glow.push_quad(corners, outward, color, None);
            }
        }
    }
}

/// Quads the rune on one exposed side is made of.
#[cfg(test)]
pub(super) fn quads_per_rune_side(cell: [usize; 3]) -> usize {
    RUNES[super::plant_seed(cell[0], cell[1], cell[2]) as usize % RUNES.len()].len()
}
