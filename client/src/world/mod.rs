//! The voxel world the server streams, and the meshes that draw it.
//!
//! The client renders exactly the voxels the server sent. It does not generate
//! terrain, does not predict it, and does not keep a chunk the server has said is
//! out of view — `world_seed` reaches the client for diagnostics only. Every voxel
//! on screen arrived over the wire.
//!
//! ## Where each half lives
//!
//! | Module | Owns |
//! | ------ | ---- |
//! | `mod.rs` | the chunk store, the decode backlog, the RLE expansion, the block edits, applying what the net thread said, and asking for a chunk the backlog had to drop |
//! | `mesher.rs` | greedy meshing — a pure function, no Bevy types, no world access |
//! | `render.rs` | the meshing tasks, the mesh assets and one entity per chunk |
//! | `palette.rs` | block id to colour, and which ids are solid, opaque or cover |
//! | `water_material.rs` | what water looks like: the extended material and its embedded shader |
//!
//! The split mirrors the server's `internal/world`: `chunk.go` + `rle.go` are this
//! file, and the run-length invariants live here rather than in `net/codec.rs` for
//! the same reason they live in `world` rather than `protocol` on the server — they
//! are properties of a chunk, and the length they are checked against is
//! `chunk_size`, which the frame does not carry.
//!
//! ## The trust boundary
//!
//! `net/codec.rs` copies the runs out of the frame; nothing here reads a peer's
//! bytes. What is left is arithmetic on untrusted *numbers*, and
//! [`VoxelChunk::from_runs`] refuses every shape the contract forbids before it
//! allocates a chunk-sized anything. An honestly buggy server produces a malformed
//! payload as readily as a hostile one. A `BlockUpdate`'s position is untrusted in
//! the same sense: [`locate`] resolves it with Euclidean arithmetic that cannot
//! escape the chunk it names, and a voxel in a chunk this session does not hold is
//! dropped rather than stored somewhere it might fit.
//!
//! ## Nothing here is predicted
//!
//! The store changes when a `BlockUpdate` arrives and at no other moment. There is
//! no path from a click to a voxel that does not go through the server — see
//! `player/target.rs`, which only ever *asks*.

mod mesher;
pub(crate) mod palette;
mod render;
mod water_material;

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bevy::prelude::*;

pub use mesher::{ChunkMesh, Neighbours, SurfaceMesh, mesh_chunk};
pub use render::MeshStats;

use crate::net::{
    BlockCoord, ChunkCoord, Outbound, Sent, Session, WorldInbox, WorldUpdate,
    encode_chunk_resend_request,
};

/// A voxel type, as it travels on the wire.
///
/// `u16` and not an enum: the ids are the server's, the palette is documented as
/// *append, never renumber*, and a client that could not hold an id it has no name
/// for would break the moment the server grew a block. See [`palette`].
pub type BlockId = u16;

/// Registers the chunk store, the streaming ingest and the renderer.
pub struct WorldPlugin;

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ChunkStore>()
            .init_resource::<DecodeQueue>()
            .init_resource::<DecodeTimeBudget>()
            // `init_resource` rather than `insert_resource`, and `NetPlugin` does
            // the same: whichever plugin is built first creates the inbox and the
            // other finds it, so neither depends on the order in `main.rs`.
            .init_resource::<WorldInbox>()
            .add_systems(Update, ingest_world_updates.after(crate::net::DrainNetwork))
            .add_plugins(render::ChunkRenderPlugin);
    }
}

// ---------------------------------------------------------------------------
// Chunks
// ---------------------------------------------------------------------------

/// One chunk's voxels, dense, in the wire's index order.
///
/// Immutable once built. The renderer hands it to a meshing task behind an `Arc`,
/// which is why nothing here mutates a chunk in place: a 32³ chunk is 64 KiB, and a
/// task that borrowed one would have to be waited for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoxelChunk {
    size: usize,
    blocks: Vec<BlockId>,
}

/// Maps a chunk-local voxel coordinate to its offset in the dense array:
///
/// ```text
/// index = (y * size + z) * size + x
/// ```
///
/// x fastest, then z, then y. This is **wire contract** — `schemas/world.fbs`
/// documents it, `world.Index` on the server encodes in it, and this function is the
/// only place the client spells it out. Changing it is a protocol version bump.
fn index(size: usize, x: usize, y: usize, z: usize) -> usize {
    (y * size + z) * size + x
}

impl VoxelChunk {
    /// Expands run-length pairs into a dense chunk of `size³` voxels.
    ///
    /// Enforces every invariant `schemas/world.fbs` documents for a decoder, and it
    /// is the mirror of `world.Decode` on the server, check for check: even length,
    /// no zero-length run, and lengths summing to exactly the volume. A decoder that
    /// trusted the sum would either allocate whatever the payload asked for or leave
    /// a half-filled chunk to be drawn as terrain, and both are worse than an error.
    pub fn from_runs(runs: &[u16], size: usize) -> Result<Self, RunsError> {
        let volume = size
            .checked_pow(3)
            .ok_or(RunsError::ImpossibleVolume { size })?;

        if runs.is_empty() {
            return Err(RunsError::NoRuns);
        }
        if !runs.len().is_multiple_of(2) {
            return Err(RunsError::OddLength { len: runs.len() });
        }

        // Sized from the *validated* volume, never from anything the payload says,
        // so a hostile run vector cannot ask for an allocation.
        let mut blocks = Vec::with_capacity(volume);
        for (pair, values) in runs.chunks_exact(2).enumerate() {
            let (block, run) = (values[0], usize::from(values[1]));
            if run == 0 {
                return Err(RunsError::ZeroLengthRun { pair });
            }
            if blocks.len() + run > volume {
                return Err(RunsError::TooManyVoxels { volume });
            }
            blocks.resize(blocks.len() + run, block);
        }

        if blocks.len() != volume {
            return Err(RunsError::Incomplete {
                got: blocks.len(),
                want: volume,
            });
        }

        Ok(Self { size, blocks })
    }

    /// The chunk's edge length in blocks — the server's `chunk_size`, never a
    /// constant of the client's own.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Reads one voxel. Takes the coordinate as an array because the mesher builds
    /// it by axis index rather than by name.
    pub fn block(&self, [x, y, z]: [usize; 3]) -> BlockId {
        self.blocks[index(self.size, x, y, z)]
    }

    /// This chunk with one voxel replaced, or `None` when the coordinate is outside
    /// it.
    ///
    /// A **new chunk** rather than a mutation in place, and that is this side of the
    /// staleness guard's bargain. `render.rs` decides whether a finished mesh is
    /// still current with `Arc::ptr_eq`, and the argument for why that is sound is
    /// written in terms of every revision being a fresh allocation. `Arc::make_mut`
    /// would keep the allocation whenever nothing else held a reference — which
    /// happens to be safe, because a mesh in flight *is* such a reference, but it
    /// makes the guard's correctness depend on a reference count a reader has to
    /// re-derive. A chunk is 64 KiB and an edit is one click.
    ///
    /// Bounds are checked per axis rather than on the flat index, because
    /// [`index`] maps an out-of-range `x` onto a perfectly valid offset in the next
    /// row: a voxel would be edited, just not the one that was named.
    pub fn with_block(&self, [x, y, z]: [usize; 3], block: BlockId) -> Option<Self> {
        if x >= self.size || y >= self.size || z >= self.size {
            return None;
        }

        let mut edited = self.clone();
        edited.blocks[index(self.size, x, y, z)] = block;
        Some(edited)
    }

    /// An all-air chunk. Test-only: a real chunk always comes from runs.
    #[cfg(test)]
    pub fn all_air(size: usize) -> Self {
        Self {
            size,
            blocks: vec![palette::AIR; size * size * size],
        }
    }

    /// Writes one voxel. Test-only, for the same reason as [`Self::all_air`].
    #[cfg(test)]
    pub fn set(&mut self, x: usize, y: usize, z: usize, block: BlockId) {
        let size = self.size;
        self.blocks[index(size, x, y, z)] = block;
    }
}

/// Why a run-length payload is not a chunk.
///
/// Split finely enough to name the failing invariant, because "malformed" tells an
/// operator nothing about which side to fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunsError {
    /// No runs at all. Not the same as a chunk of air, which is one run of air.
    NoRuns,
    /// Not a whole number of `(block id, run length)` pairs.
    OddLength { len: usize },
    /// A run of length zero. Legal as arithmetic, forbidden by the contract, and a
    /// cheap way to make a payload that never terminates.
    ZeroLengthRun { pair: usize },
    /// The runs describe more voxels than the chunk holds.
    TooManyVoxels { volume: usize },
    /// The runs stopped short of filling the chunk.
    Incomplete { got: usize, want: usize },
    /// `size³` does not fit in a `usize`.
    ///
    /// Unreachable through the decoder, which caps `chunk_size` at 40 before a
    /// session exists. Named anyway, because an overflowing volume is the one
    /// failure here that a release build would carry out silently.
    ImpossibleVolume { size: usize },
}

impl fmt::Display for RunsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRuns => write!(f, "no runs"),
            Self::ImpossibleVolume { size } => {
                write!(f, "a chunk of edge {size} has no representable volume")
            }
            Self::OddLength { len } => {
                write!(f, "{len} values is not a whole number of (id, run) pairs")
            }
            Self::ZeroLengthRun { pair } => write!(f, "run {pair} has zero length"),
            Self::TooManyVoxels { volume } => {
                write!(f, "runs describe more than {volume} voxels")
            }
            Self::Incomplete { got, want } => {
                write!(f, "runs describe {got} voxels, want {want}")
            }
        }
    }
}

impl std::error::Error for RunsError {}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

/// What happened to a chunk, in the order it happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkChange {
    /// The chunk's voxels are new or have been replaced; its mesh is stale.
    Loaded(ChunkCoord),
    /// The chunk is gone; its mesh must go with it.
    Unloaded(ChunkCoord),
    /// The chunk *next* to this one changed on the border the two share, so this
    /// chunk's mesh is stale even though none of its own voxels moved.
    ///
    /// A face on a shared border is culled against the neighbour's voxel, so a mesh
    /// depends on the border layer of each of its six face neighbours. Four things
    /// move that layer, and all four arrive here: the neighbour was edited on the
    /// border, the neighbour arrived where there had been nothing, the neighbour was
    /// replaced by a revision whose border layer differs, and the neighbour went away.
    /// The first is named by [`border_neighbours`] from the voxel that changed; the
    /// other three by [`ChunkStore::note_neighbours_stale`], which compares the layer.
    ///
    /// A separate variant rather than a second `Loaded`, for two reasons. `Loaded`'s
    /// meaning — "these voxels are new" — would stop being true, and it is what tells
    /// `render.rs` that a re-sent chunk replaced an earlier one. And the border rule
    /// is worth being able to *see*: a test can assert that an edit one voxel inside a
    /// chunk produces no entry of this kind, which is not something a count of
    /// `Loaded`s can express.
    NeighbourChanged(ChunkCoord),
}

/// Every chunk the server currently says this session can see, keyed by chunk
/// coordinate.
///
/// The store also keeps an **ordered** log of what changed since the renderer last
/// looked. Ordered, not a pair of sets: the server unloads a chunk before it
/// re-sends it, and a renderer that learned about the two through separate sets
/// could apply them backwards and either leave a stale mesh behind or drop a live
/// one. The log lives here rather than beside the map so the two cannot diverge —
/// every mutation appends.
#[derive(Resource, Debug, Default)]
pub struct ChunkStore {
    chunks: HashMap<ChunkCoord, Arc<VoxelChunk>>,
    changes: Vec<ChunkChange>,
}

impl ChunkStore {
    /// Stores a chunk, replacing any earlier copy of it, and logs every mesh that
    /// went stale — this chunk's own, and each neighbour that was culling its faces
    /// against a border layer this payload has moved.
    ///
    /// Replacement is normal rather than exceptional: a chunk the client failed to
    /// acknowledge is re-sent by the server's next view update, and the newer copy
    /// is the authoritative one.
    pub fn insert(&mut self, coord: ChunkCoord, chunk: VoxelChunk) {
        let stored = Arc::new(chunk);
        let replaced = self.chunks.insert(coord, Arc::clone(&stored));
        self.changes.push(ChunkChange::Loaded(coord));
        self.note_neighbours_stale(coord, replaced.as_deref(), Some(&stored));
    }

    /// Stores an edited revision of a chunk, logging its own mesh as stale and
    /// nobody else's.
    ///
    /// [`Self::insert`] without the neighbour scan, and the edit path takes it because
    /// it can answer the same question more cheaply and more precisely: one voxel
    /// changed, and [`border_neighbours`] names the chunks that share a face with it
    /// from its coordinate alone. A payload off the wire could have moved any of the
    /// six border layers and has to be compared to find out which.
    fn store_edit(&mut self, coord: ChunkCoord, chunk: VoxelChunk) {
        self.chunks.insert(coord, Arc::new(chunk));
        self.changes.push(ChunkChange::Loaded(coord));
    }

    /// Drops a chunk the server has unloaded, and logs the neighbours that were
    /// hiding behind it.
    ///
    /// The unload is logged even when the chunk was not held. The client can
    /// legitimately be missing one — a malformed payload is dropped with a warning —
    /// and the renderer still has bookkeeping to clear for it. The neighbours are
    /// logged only for what was actually there: a coordinate nothing was stored under
    /// was already meshed against as air.
    pub fn unload(&mut self, coord: ChunkCoord) {
        let dropped = self.chunks.remove(&coord);
        self.changes.push(ChunkChange::Unloaded(coord));
        self.note_neighbours_stale(coord, dropped.as_deref(), None);
    }

    /// The chunks across `coord`'s six faces, plus the four above its horizontal
    /// neighbours needed to resolve falling water.
    ///
    /// Gathered here because the store is the authority on what exists, and handed to
    /// the mesher as a **value**: neighbour data is an input to meshing, never
    /// something the mesher reaches for. That is what keeps `mesh_chunk` runnable on a
    /// task pool and testable on synthetic chunks.
    ///
    /// A coordinate this session does not hold is left absent rather than substituted,
    /// and the mesher reads an absent neighbour as air — so the border face is
    /// over-drawn instead of leaving a hole, and [`Self::insert`] queues the remesh
    /// that removes the extra quads when the chunk finally arrives.
    pub fn neighbours(&self, coord: ChunkCoord) -> Neighbours {
        let gather = |offset| {
            shift(coord, offset).and_then(|neighbour| self.chunks.get(&neighbour).map(Arc::clone))
        };
        Neighbours::with_above_horizontal(
            Neighbours::OFFSETS.map(gather),
            Neighbours::ABOVE_HORIZONTAL_OFFSETS.map(gather),
        )
    }

    /// Logs the mesh of every neighbour that will now draw `coord`'s border layer
    /// differently from the way it has been drawing it.
    ///
    /// The key is opacity and vertical water presence, plus horizontal effective height.
    /// Block identity and inner layers belong only here. A revision
    /// that does not exist compares as all air, which is precisely what the mesher reads
    /// a missing neighbour as. That makes three events one: a chunk arriving (`before` is
    /// `None`), a chunk being replaced, and a chunk going away (`after` is `None`).
    ///
    /// **Comparing rather than assuming is what keeps a join affordable.** Most of what
    /// the server streams is sky, and a chunk of air arriving beside a chunk of air
    /// changes nothing anyone draws. Invalidating unconditionally would remesh up to six
    /// neighbours for every one of the 4 913 chunks a join delivers, almost all of them
    /// for a byte-identical result — and the same again for every re-send, which is
    /// byte-identical by construction while nobody has edited the chunk.
    ///
    /// A neighbour this session does not hold has no mesh to invalidate — `render.rs`
    /// drops the queue entry again at `store.get` — so it is not logged at all.
    fn note_neighbours_stale(
        &mut self,
        coord: ChunkCoord,
        before: Option<&VoxelChunk>,
        after: Option<&VoxelChunk>,
    ) {
        let above = shift(coord, [0, 1, 0]).and_then(|above| self.chunks.get(&above).cloned());
        for (slot, offset) in Neighbours::OFFSETS.into_iter().enumerate() {
            let Some(neighbour) = shift(coord, offset) else {
                continue;
            };
            if !self.chunks.contains_key(&neighbour) {
                continue;
            }
            // The slot order is `-x, +x, -y, +y, -z, +z`, so the axis is the slot halved
            // and the side is its parity. The layer the neighbour looks at is the one on
            // this chunk's face pointing at it.
            if border_layer_differs(before, after, slot / 2, slot % 2 == 1, above.as_deref()) {
                self.changes.push(ChunkChange::NeighbourChanged(neighbour));
            }
        }

        // Bottom water changes the top-water height of the chunk below.
        let Some(below_coord) = shift(coord, [0, -1, 0]) else {
            return;
        };
        let Some(below) = self.chunks.get(&below_coord) else {
            return;
        };
        for (axis, positive, offset) in [
            (0, false, [-1, 0, 0]),
            (0, true, [1, 0, 0]),
            (2, false, [0, 0, -1]),
            (2, true, [0, 0, 1]),
        ] {
            let Some(neighbour) = shift(below_coord, offset) else {
                continue;
            };
            if self.chunks.contains_key(&neighbour)
                && falling_border_differs(below, before, after, axis, positive)
            {
                self.changes.push(ChunkChange::NeighbourChanged(neighbour));
            }
        }
    }

    /// The chunk at `coord`, if the session still holds it.
    pub fn get(&self, coord: ChunkCoord) -> Option<&Arc<VoxelChunk>> {
        self.chunks.get(&coord)
    }

    /// Applies one authoritative block change, and logs every mesh it invalidated.
    ///
    /// The **only** way a voxel in this store ever changes after it arrives, and it is
    /// reachable from exactly one place: a `BlockUpdate` the server sent. A click
    /// produces a request, never a write — see `player/target.rs`.
    ///
    /// The chunk holding the voxel always goes stale. Face neighbours follow the
    /// shared geometry key; a bottom-layer edit may additionally affect the horizontal
    /// neighbours of the chunk below through falling-water height.
    pub fn apply_block(&mut self, pos: BlockCoord, block: BlockId, size: usize) -> BlockApplied {
        let Some((coord, local)) = locate(pos, size) else {
            return BlockApplied::Unlocatable;
        };

        // Not held: dropped, and `schemas/world.fbs` permits exactly that — "a client
        // either buffers it until the chunk lands or drops it and trusts the
        // `ChunkData` to arrive already edited". Dropping is the honest half of that
        // choice here, because the server invalidates the chunk's cached payload on
        // every edit, so the copy this session is eventually sent already carries it.
        // Buffering would mean holding edits for chunks that may never arrive.
        let above = shift(coord, [0, 1, 0]).and_then(|above| self.chunks.get(&above).cloned());
        let Some(held) = self.chunks.get(&coord) else {
            return BlockApplied::Unheld { coord };
        };
        // `None` needs `local` to be outside a chunk `locate` just placed it inside,
        // which takes a chunk stored under a different `chunk_size` than this session
        // has. There is one welcome per session, so there is one size.
        let Some(edited) = held.with_block(local, block) else {
            return BlockApplied::Unlocatable;
        };

        // [`Self::store_edit`] and not [`Self::insert`]: the neighbour scan `insert`
        // runs is the answer to a question this path can answer exactly, from the voxel
        // that moved. Taking both would remesh all six neighbours of every edit made in
        // the middle of a chunk.
        // Read before the store takes the chunk; `above` resolves falling height.
        let before_geometry = border_geometry(Some(held), local, above.as_deref());
        let after_geometry = border_geometry(Some(&edited), local, above.as_deref());
        let falling_neighbours = if local[1] == 0 {
            shift(coord, [0, -1, 0])
                .and_then(|below_coord| {
                    self.chunks.get(&below_coord).map(|below| {
                        let below_size = below.size();
                        if below_size == 0 || local[0] >= below_size || local[2] >= below_size {
                            return Vec::new();
                        }
                        let below_local = [local[0], below_size - 1, local[2]];
                        let moved = border_geometry(Some(below), below_local, Some(held))
                            != border_geometry(Some(below), below_local, Some(&edited));
                        if moved {
                            border_neighbours(below_coord, below_local, below_size)
                                .into_iter()
                                .filter(|neighbour| neighbour.cy == below_coord.cy)
                                .collect()
                        } else {
                            Vec::new()
                        }
                    })
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        self.store_edit(coord, edited);

        // The edited chunk always remeshes: colour is its own, and changes even when
        // solidity does not.
        let mut remeshed = 1;
        for neighbour in border_neighbours(coord, local, size)
            .into_iter()
            .filter(|neighbour| {
                let axis = if neighbour.cx != coord.cx {
                    0
                } else {
                    1 + usize::from(neighbour.cy == coord.cy)
                };
                before_geometry.differs_for_face(after_geometry, axis)
            })
        {
            remeshed += 1;
            self.changes.push(ChunkChange::NeighbourChanged(neighbour));
        }
        remeshed += falling_neighbours.len();
        for neighbour in falling_neighbours {
            self.changes.push(ChunkChange::NeighbourChanged(neighbour));
        }

        BlockApplied::Rewritten { coord, remeshed }
    }

    /// The block at this world block coordinate.
    ///
    /// **A voxel this session does not hold answers [`palette::AIR`]**, and so does a
    /// coordinate that resolves to no chunk at all. That is the honest answer rather
    /// than a cautious one: what a player can aim at, walk into or be submerged by is
    /// what the server has streamed them, and a chunk that has not arrived contains
    /// nothing this client knows about. Aiming into one is a corner case regardless —
    /// the reach is a few blocks and the player stands in the middle of the streamed
    /// volume — and the server refuses an edit inside a chunk it never sent anyway.
    ///
    /// The one place a world coordinate becomes a block id, so every question about
    /// what is at a position — solidity here, submersion in `player/sky.rs` — resolves
    /// the coordinate exactly once and in one way.
    pub fn block_at(&self, pos: BlockCoord, size: usize) -> BlockId {
        let Some((coord, local)) = locate(pos, size) else {
            return palette::AIR;
        };
        let Some(chunk) = self.chunks.get(&coord) else {
            return palette::AIR;
        };
        // Same reasoning as `with_block`'s per-axis check: an out-of-range component
        // would read a valid offset in the wrong row rather than fail.
        if local.iter().any(|axis| *axis >= chunk.size()) {
            return palette::AIR;
        }

        chunk.block(local)
    }

    /// Whether the voxel at this world block coordinate stops a body.
    ///
    /// The question a raycast asks, and it lives here because the store is the
    /// authority on what exists and `palette` is the authority on what is solid.
    /// **Water is not**, since #446: the aiming ray passes through a lake and outlines
    /// the bed under it, which is one predicate away from the behaviour that shipped
    /// before water existed.
    pub fn solid_at(&self, pos: BlockCoord, size: usize) -> bool {
        palette::is_solid(self.block_at(pos, size))
    }

    /// The highest solid voxel in one world column inside `[min_y, max_y)`.
    ///
    /// This is the column-sized counterpart to [`Self::solid_at`]. It walks resident
    /// chunks rather than resolving the same chunk through the hash map once per Y, and
    /// skips absent chunks wholesale. Precipitation uses it when its shelter cache is
    /// cold, where hundreds of columns may be queried together after a streamed-world
    /// change. A chunk this session does not hold still answers air, exactly as
    /// [`Self::block_at`] does.
    pub fn highest_solid_y(
        &self,
        x: i32,
        z: i32,
        min_y: i32,
        max_y: i32,
        size: usize,
    ) -> Option<i32> {
        if min_y >= max_y {
            return None;
        }
        let size_i32 = i32::try_from(size).ok().filter(|size| *size > 0)?;
        let cx = x.div_euclid(size_i32);
        let cz = z.div_euclid(size_i32);
        let local_x = usize::try_from(x.rem_euclid(size_i32)).ok()?;
        let local_z = usize::try_from(z.rem_euclid(size_i32)).ok()?;
        let min_cy = min_y.div_euclid(size_i32);
        let highest_y = max_y - 1;
        let max_cy = highest_y.div_euclid(size_i32);

        for cy in (min_cy..=max_cy).rev() {
            let Some(chunk) = self.chunks.get(&ChunkCoord { cx, cy, cz }) else {
                continue;
            };
            if chunk.size() != size {
                continue;
            }

            let first = if cy == min_cy {
                usize::try_from(min_y.rem_euclid(size_i32)).ok()?
            } else {
                0
            };
            let last = if cy == max_cy {
                usize::try_from(highest_y.rem_euclid(size_i32)).ok()?
            } else {
                size - 1
            };
            let local_y = (first..=last)
                .rev()
                .find(|y| palette::is_solid(chunk.block([local_x, *y, local_z])));
            if let Some(local_y) = local_y {
                return cy
                    .checked_mul(size_i32)
                    .and_then(|origin| origin.checked_add(i32::try_from(local_y).ok()?));
            }
        }
        None
    }

    /// Whether the voxel at this world block coordinate is something the crosshair can
    /// find.
    ///
    /// **The aiming ray's question, and only the aiming ray's.** Solidity answers what
    /// stops a body, and since #446 the ray has read it so a lake is looked *through*.
    /// Cover (#550) needs the opposite of that: a flower stops nothing and is still a
    /// thing a player breaks, so the ray needs a predicate that is true for it while
    /// collision, the camera boom and the healing-target occlusion probe keep reading
    /// [`Self::solid_at`] and keep walking through it.
    ///
    /// Making `is_solid` true for cover instead would have been one edit and four wrong
    /// behaviours: a body stopped by a flower, a boom pushed in by one, a player hidden
    /// behind one, and a mask that culls the grass face under one.
    ///
    /// Presentation only. Naming a voxel is not breaking it — a click still sends a
    /// request and the server still decides.
    pub fn targetable_at(&self, pos: BlockCoord, size: usize) -> bool {
        let block = self.block_at(pos, size);
        palette::is_solid(block) || palette::is_cover(block)
    }

    /// How many chunks the session holds. Shown in the debug overlay.
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Whether anything has changed since the renderer last looked.
    ///
    /// Exists so a caller can avoid [`Self::take_changes`] on an idle frame. That is
    /// not a micro-optimisation: `ResMut` marks a resource changed on every
    /// `DerefMut`, so taking an empty log every frame would make this store *always*
    /// look changed and quietly defeat every consumer's change detection.
    pub fn has_changes(&self) -> bool {
        !self.changes.is_empty()
    }

    /// Takes the change log, leaving it empty.
    ///
    /// The renderer is the only caller, and it must be the only one: a second
    /// consumer would take changes the first never sees.
    pub fn take_changes(&mut self) -> Vec<ChunkChange> {
        std::mem::take(&mut self.changes)
    }
}

/// What applying a `BlockUpdate` did to the store.
///
/// Returned rather than logged in place, because the caller owns the per-frame budget
/// and only the first variant spends it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockApplied {
    /// The chunk holding the voxel was replaced by an edited revision of itself.
    ///
    /// `remeshed` is how many chunks the one voxel made stale: the edited chunk, plus
    /// every neighbour whose shared border it sits on.
    Rewritten { coord: ChunkCoord, remeshed: usize },
    /// This session does not hold the chunk the voxel is in, so there was nothing to
    /// edit. Not an error, and not a hole either: the server invalidates that chunk's
    /// cached payload on every edit, so the copy this session is eventually sent
    /// already has the change in it.
    Unheld { coord: ChunkCoord },
    /// The position cannot be resolved to a chunk at all.
    ///
    /// Needs a `chunk_size` of zero, or one past `i32::MAX` — neither of which a
    /// session can have, because the decoder caps it at 40 before `SessionParams`
    /// exists. Named anyway, so the arithmetic has a total answer rather than a
    /// division by zero.
    Unlocatable,
}

/// Splits a world block coordinate into the chunk that holds it and the voxel's place
/// inside that chunk. `None` when `size` is not a length a chunk can have.
///
/// **Euclidean division, not truncating**, and that is the whole of it: Rust's `/`
/// rounds towards zero, so `-1 / 32` is 0 and a voxel at `x = -1` would be placed in
/// chunk 0 at local x = -1. Every edit west, south or below the origin would land in
/// the wrong chunk — and only on one side of it, so half a test suite would agree.
/// `rem_euclid` against a positive divisor is what makes the local coordinate
/// non-negative and therefore a `usize` at all.
///
/// The mirror of `world.Coord`'s arithmetic on the server, and the client does it a
/// second time only because it keeps its own store; what crosses the wire stays a
/// world coordinate precisely so the two copies cannot disagree about a *request*.
fn locate(pos: BlockCoord, size: usize) -> Option<(ChunkCoord, [usize; 3])> {
    let edge = i32::try_from(size).ok()?;
    if edge < 1 {
        return None;
    }

    let world = [pos.x, pos.y, pos.z];
    let chunk = world.map(|axis| axis.div_euclid(edge));
    // `rem_euclid` against a positive divisor lands in `0..edge`, so the cast is exact
    // and can never be negative.
    let local = world.map(|axis| axis.rem_euclid(edge) as usize);

    Some((
        ChunkCoord {
            cx: chunk[0],
            cy: chunk[1],
            cz: chunk[2],
        },
        local,
    ))
}

/// `coord` moved one chunk along each axis of `offset`.
///
/// `None` at the end of the coordinate range, for the reason [`border_neighbours`]
/// gives: wrapping would name a chunk on the far side of the world and saturating
/// would name this one.
fn shift(coord: ChunkCoord, offset: [i32; 3]) -> Option<ChunkCoord> {
    Some(ChunkCoord {
        cx: coord.cx.checked_add(offset[0])?,
        cy: coord.cy.checked_add(offset[1])?,
        cz: coord.cz.checked_add(offset[2])?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BorderGeometry {
    opaque_masks: [u8; 6],
    water_level: u8,
}

impl BorderGeometry {
    fn differs_for_face(self, other: Self, axis: usize) -> bool {
        self.opaque_masks[axis * 2..axis * 2 + 2] != other.opaque_masks[axis * 2..axis * 2 + 2]
            || (self.water_level != other.water_level
                && (axis != 1 || self.water_level == 0 || other.water_level == 0))
    }
}

fn border_geometry(
    chunk: Option<&VoxelChunk>,
    cell: [usize; 3],
    above_chunk: Option<&VoxelChunk>,
) -> BorderGeometry {
    let Some(chunk) = chunk else {
        return BorderGeometry {
            opaque_masks: [0; 6],
            water_level: 0,
        };
    };
    let block = chunk.block(cell);
    let mut water_level = palette::water_level(block);
    if water_level != 0 {
        let above = if cell[1] + 1 < chunk.size() {
            let mut above = cell;
            above[1] += 1;
            chunk.block(above)
        } else {
            let mut above = cell;
            above[1] = 0;
            above_chunk
                .filter(|above_chunk| above_chunk.size() == chunk.size())
                .map_or(palette::AIR, |above_chunk| above_chunk.block(above))
        };
        if palette::is_water(above) {
            water_level = 8;
        }
    }
    BorderGeometry {
        opaque_masks: std::array::from_fn(|slot| {
            palette::opaque_face_mask(block, slot / 2, slot % 2 == 1)
        }),
        water_level,
    }
}

/// Whether two revisions of one chunk disagree about the geometry on the outer
/// face `(axis, positive)` — the only part the chunk across that face reads.
///
/// `None` is a revision that does not exist: before the chunk arrived, or after it was
/// unloaded. It compares as all air, which is exactly what the mesher reads a neighbour
/// it was not given as, so "arrived", "replaced" and "went away" are one comparison.
///
/// Two revisions that disagree about `size` answer `true`. There is one `chunk_size`
/// per session so they cannot, and a comparison that indexed one with the other's edge
/// would be the wrong way to find that out.
fn border_layer_differs(
    before: Option<&VoxelChunk>,
    after: Option<&VoxelChunk>,
    axis: usize,
    positive: bool,
    above: Option<&VoxelChunk>,
) -> bool {
    let size = match (before, after) {
        (Some(before), Some(after)) if before.size() != after.size() => return true,
        (Some(chunk), _) | (_, Some(chunk)) => chunk.size(),
        (None, None) => return false,
    };

    let u = (axis + 1) % 3;
    let v = (axis + 2) % 3;
    let layer = if positive { size.saturating_sub(1) } else { 0 };
    let geometry = |chunk: Option<&VoxelChunk>, i: usize, j: usize| {
        let mut cell = [0usize; 3];
        cell[axis] = layer;
        cell[u] = i;
        cell[v] = j;
        border_geometry(chunk, cell, above)
    };

    (0..size).any(|j| {
        (0..size).any(|i| geometry(before, i, j).differs_for_face(geometry(after, i, j), axis))
    })
}

fn falling_border_differs(
    below: &VoxelChunk,
    before_above: Option<&VoxelChunk>,
    after_above: Option<&VoxelChunk>,
    axis: usize,
    positive: bool,
) -> bool {
    if before_above
        .into_iter()
        .chain(after_above)
        .any(|above| above.size() != below.size())
    {
        return true;
    }
    let size = below.size();
    if size == 0 {
        return false;
    }
    let edge = if positive { size.saturating_sub(1) } else { 0 };
    (0..size).any(|other| {
        let cell = if axis == 0 {
            [edge, size - 1, other]
        } else {
            [other, size - 1, edge]
        };
        border_geometry(Some(below), cell, before_above)
            != border_geometry(Some(below), cell, after_above)
    })
}

/// The chunks whose meshes an edit at `local` invalidates besides the edited chunk's
/// own.
///
/// **Face-adjacent only.** This helper answers direct shared-face geometry. The one
/// edge-diagonal dependency introduced by falling water is derived separately from
/// the chunk below in [`ChunkStore::apply_block`].
///
/// Both bounds are tested independently rather than as an `else if`, because at
/// `chunk_size` 1 — legal, and pinned by `the_smallest_legal_chunk_size_works` — every
/// voxel is simultaneously at 0 and at `size - 1` on all three axes, so the one voxel
/// borders all six neighbours.
///
/// A chunk at the end of the `i32` range has no neighbour on that side and is skipped;
/// wrapping would name a chunk on the far side of the world, and saturating would name
/// itself.
fn border_neighbours(coord: ChunkCoord, local: [usize; 3], size: usize) -> Vec<ChunkCoord> {
    let base = [coord.cx, coord.cy, coord.cz];
    let mut touched = Vec::new();

    let mut push = |axis: usize, step: i32| {
        let Some(shifted) = base[axis].checked_add(step) else {
            return;
        };
        let mut neighbour = base;
        neighbour[axis] = shifted;
        touched.push(ChunkCoord {
            cx: neighbour[0],
            cy: neighbour[1],
            cz: neighbour[2],
        });
    };

    for (axis, offset) in local.into_iter().enumerate() {
        if offset == 0 {
            push(axis, -1);
        }
        if offset + 1 == size {
            push(axis, 1);
        }
    }

    touched
}

// ---------------------------------------------------------------------------
// Ingest
// ---------------------------------------------------------------------------

/// How many chunk payloads may be expanded into voxels per frame.
///
/// Expansion is main-schedule work — a `size³` write per chunk — and a join delivers
/// the whole view distance in one burst: 729 chunks at view distance 4, 4 913 at 8.
/// Decoding all of them in the frames the queue allows is the same stall that moving
/// *meshing* off the main schedule was meant to avoid, and it was the one half of the
/// pipeline with no cap on it.
///
/// The number matches [`render`]'s per-frame cap on meshing jobs, because a decoded
/// chunk's very next step is a meshing slot: expanding faster than the mesher can
/// start work does not put terrain on screen sooner, it only converts a small
/// run-length payload into 64 KiB of voxels earlier than anything can use it.
///
/// **A block edit spends the same budget**, because it costs the same thing: an edited
/// chunk is a fresh `size³` allocation and a copy into it. One click is one edit and
/// nowhere near the cap, but the server sends a `BlockUpdate` for every edit any
/// player in view makes, and whatever moves blocks on its own later will reuse that
/// same message — so the burst is the case to be bounded for, not the click.
///
/// **It is a ceiling now rather than the rule**, and [`MAX_DECODE_TIME_PER_FRAME`] is
/// why: everything above is an argument about how much *work* is worth doing before
/// the next stage can use it, and none of it is an argument about how long a frame
/// takes. Both bounds are read on the same line, and whichever is reached first ends
/// the frame's expansion.
const MAX_DECODES_PER_FRAME: usize = 32;

/// How much of a frame expanding the backlog may take.
///
/// **Why a count was not enough.** [`MAX_DECODES_PER_FRAME`] bounds items, and the thing
/// it exists to prevent is a frame that visibly stalls. Those are two different
/// quantities joined by a cost per chunk that is not a constant — it moves with the build
/// profile, with the CPU and with what the payload holds — so a count is a bound on the
/// stall only on the machine somebody once measured it on. Expanding and storing one
/// chunk, averaged over the 49 a boundary crossing streams, was **0.026–0.044 ms
/// optimized and 0.26–0.46 ms unoptimized** on one AMD Ryzen 7 3700X: a factor of ten
/// between two builds of the same source, before any second machine is considered. A
/// budget written to bound a frame has to be spent in the frame's own unit.
///
/// **What #629 measured, and where the spike was.** The harness lives in `render.rs` and
/// is re-runnable — `cargo test --release -- --ignored --nocapture measure_`. It streams
/// the 49 chunks one chunk-boundary crossing sends (`DefaultViewDistance` on the server
/// is 3, so a crossing is a 7 × 7 slab) and the 343 a join sends, under each bound in
/// turn, and stamps a clock around each of the three world systems. On the frames the
/// shell lands on, **`ingest_world_updates` owned 88% of the frame**, `start_mesh_jobs`
/// about 5% and `apply_finished_meshes` under 2% — which is why this constant exists and
/// `MAX_APPLIED_PER_FRAME` did not move. With the count as the only bound, the worst
/// frame that system spent was:
///
/// | burst | optimized | unoptimized |
/// | ----- | --------- | ----------- |
/// | one crossing, 49 chunks | 0.27–1.16 ms | 11.7–15.7 ms |
/// | a join, 343 chunks | 1.75–2.42 ms | 15.7–21.2 ms |
///
/// Two frames absorb a whole crossing at 32 a frame, so the unoptimized client spent
/// twelve to sixteen milliseconds in one system, twice, at a spacing of exactly one
/// chunk of travel. That is the periodic hitch the issue reported, and the optimized
/// column is why it was reported by somebody running the game rather than by a profiler.
///
/// **Where 2 ms comes from.** It is an eighth of a 60 Hz frame, and the numbers either
/// side of it are the reason it is not one of them. **1 ms** would bind in the optimized
/// build, where nothing measured needs bounding: a join's first frame was already
/// 1.75–2.42 ms there with the count alone, so a 1 ms slice would spread a join that
/// finishes expanding in 11 frames over roughly 25 to buy back a spike no measurement
/// found. **4 ms** is a quarter of a 60 Hz frame handed to one system on a frame that
/// still has to render, and it would have left the unoptimized crossing above spending
/// 4 ms of 16.67 twice over — a quieter version of the same hitch rather than the end of
/// it. At 2 ms the same runs measured 0.30–1.23 ms and 2.08 ms optimized (join fill
/// unchanged at 11 frames) and 2.2–4.0 ms unoptimized.
///
/// **What it costs, stated rather than buried.** In the unoptimized build a join now
/// finishes expanding after about 80 frames instead of 11. Those are frames the client
/// is running rather than frozen — 11 frames of 16–21 ms each is not a faster join, it
/// is the same join with the client unable to draw it — but terrain does finish arriving
/// about a second later, and if that ever matters more than the smoothness it buys, this
/// is the number to revisit. In the optimized build the join is unchanged, which is the
/// case [`MAX_DECODES_PER_FRAME`] was chosen for.
///
/// **The slice is not a deadline, and the overshoot is deliberate.** It is checked after
/// an update has been applied, so the last one always finishes: the drain is metered,
/// never interrupted. The admission pass that empties the inbox into the queue is outside
/// the clock as well — `net`'s boundary rule leaves it no choice — so a frame that admits
/// a whole join measures about 2.08 ms here rather than 2.00. Both are why this bounds a
/// frame's *shape* rather than promising a maximum.
///
/// **Unloads are inside the slice even though they are outside the count.** The count
/// deliberately skips them, because metering a map removal would let a burst of unloads
/// defer the loads behind it. Wall clock needs no such exemption: an unload that costs
/// nothing spends nothing, and one that somehow costs a millisecond is a millisecond of
/// the frame whatever its name is.
const MAX_DECODE_TIME_PER_FRAME: Duration = Duration::from_millis(2);

/// The slice of a frame [`ingest_world_updates`] may spend on the backlog.
///
/// A resource holding [`MAX_DECODE_TIME_PER_FRAME`], and it is one for the tests rather
/// than for the player: **no screen sets it and nothing on the wire reaches it**. What
/// it buys is that the suite never times the runner. The burst tests below assert an
/// exact number of expansions per frame, which is a statement about the *count* budget;
/// with a wall-clock bound in the same loop those assertions would pass or fail
/// depending on who else was using the machine. So `ingest_app` and `headless_world`
/// install [`Duration::MAX`], where the count is the only bound that can be reached,
/// and the tests that are about this bound install [`Duration::ZERO`], where it is
/// always the one reached. Both ends are exact; the value in between is a tuning
/// number, and a tuning number is not what a test should be pinned to.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DecodeTimeBudget(pub(crate) Duration);

impl Default for DecodeTimeBudget {
    fn default() -> Self {
        Self(MAX_DECODE_TIME_PER_FRAME)
    }
}

/// The view distance whose join the backlog is sized to hold whole.
///
/// The *client's* number, not the session's, and that distinction is the whole point:
/// sizing the bound from `ServerWelcome.view_distance` would hand the party being
/// defended against the job of setting its own ceiling. `schemas/handshake.fbs` caps
/// that field at 16 for the same reason, and 16 is what a server with an interest in
/// the answer would send.
///
/// Eight, because it is the largest volume the metered decoder can absorb inside a few
/// seconds — [`MAX_DECODE_BACKLOG`] does the arithmetic, and the two numbers either
/// side of it are why 8 rather than 3 or 16.
const BACKLOG_VIEW_DISTANCE: usize = 8;

/// How many world updates may be waiting before chunk payloads start being refused.
///
/// **Why there is a bound at all.** The inbox is drained in full every frame and only
/// [`MAX_DECODES_PER_FRAME`] of the backlog is expanded, so arrival sustained above
/// that rate grows the queue for as long as the server keeps writing. Normal play
/// bounds it without help — the server streams the view volume and stops — so reaching
/// this needs a large view distance combined with fast movement, or a server that does
/// not stop. That is the threat model `schemas/handshake.fbs` already records, and its
/// wording applies verbatim: trusting the server on gameplay outcomes is not the same
/// as trusting it on array bounds.
///
/// **Where the number comes from.** The one burst that is legitimately larger than the
/// drain rate is a join: the server streams the whole view volume,
/// `(2 * view_distance + 1)³` chunks, in as few writes as the socket allows. So the
/// bound is one such volume at [`BACKLOG_VIEW_DISTANCE`] — `17³` = 4 913 updates, which
/// at [`MAX_DECODES_PER_FRAME`] is 154 frames, about 2.6 s at 60 Hz. That wait is the
/// latency the player is asked to pay instead of the process. A second whole join
/// arriving before the first has drained is not a burst any more; it is a stream that
/// will not stop, and it is what this refuses.
///
/// The two numbers on either side say why the view distance in it is 8. The server's
/// own default is 3 — `7³` = 343 chunks — so the bound sits fourteen whole joins above
/// the largest burst normal play produces. The protocol's ceiling of 16 would be
/// `33³` = 35 937 updates and 1 123 frames: nineteen seconds of backlog, which is not
/// latency, it is a stall.
///
/// **Those frame counts are now a floor, and the bound is unchanged by that.** The
/// drain stops at [`MAX_DECODES_PER_FRAME`] *or* [`MAX_DECODE_TIME_PER_FRAME`],
/// whichever comes first, so a machine or a build on which 32 expansions do not fit in
/// 2 ms takes more than 154 frames to drain a full backlog. What the bound promises is
/// unaffected — it is a ceiling on memory and on divergence, not a delivery deadline —
/// and the direction is the safe one: the extra frames are the ones that were being
/// stolen from rendering.
const MAX_DECODE_BACKLOG: usize = (2 * BACKLOG_VIEW_DISTANCE + 1).pow(3);

/// World updates that have arrived and have not been applied yet.
///
/// A queue rather than a per-kind buffer, and **ordered across kinds** for the reason
/// [`ChunkStore`]'s change log is: the server unloads a chunk before it re-sends one,
/// so an unload that overtook a queued load would delete a chunk the session can see.
/// Only expansion is metered, so an unload waiting behind a load costs nothing when
/// its turn comes — but it never gets its turn early.
///
/// Holding the *undecoded* payload is deliberate: a run-length chunk is a handful of
/// pairs where the expanded chunk is `size³` voxels, so the backlog is orders of
/// magnitude cheaper to keep here than on the other side of the decoder.
///
/// **Bounded at [`MAX_DECODE_BACKLOG`], with nothing admitted over it.** [`Self::admit`]
/// carries which end gives way, what becomes of each kind that cannot simply be turned
/// away, and why evicting a chunk is a smaller loss than keeping a wrong one.
#[derive(Resource, Debug, Default)]
pub(crate) struct DecodeQueue {
    waiting: VecDeque<WorldUpdate>,
    /// Updates the bound has turned away, for the life of the session.
    ///
    /// Kept on the queue rather than in a resource of its own for the reason
    /// `decode_backlog` lives in [`MeshStats`]: the overlay watches exactly one change
    /// signal, and a second one would be a second thing to keep in step.
    refused: usize,
    /// Chunks the bound has dropped from the store, for the life of the session.
    ///
    /// Separate from `refused` because they are different events with different costs.
    /// A refusal turns away something that had not arrived anywhere yet; an eviction
    /// takes terrain the player could see. Counting them together would hide the second
    /// inside the first, and the second is the one worth noticing.
    evicted: usize,
}

impl DecodeQueue {
    /// How many updates are still waiting. Reported by the debug overlay, which is
    /// the only way a decode backlog is visible at all.
    pub(crate) fn len(&self) -> usize {
        self.waiting.len()
    }

    /// How many updates the bound has turned away this session.
    ///
    /// Reported beside [`Self::len`], because a backlog that has stopped growing and a
    /// backlog that is quietly losing terrain look identical without it. Silent capping
    /// is the failure this repository keeps finding in its own tooling; a cap that
    /// announces itself is the standing answer.
    pub(crate) fn refused(&self) -> usize {
        self.refused
    }

    /// How many chunks the bound has evicted from the store this session.
    ///
    /// Reported beside [`Self::refused`] for the same reason it exists at all: an
    /// eviction is the one consequence of the bound a player can see, and a number
    /// nobody displays is a number nobody checks.
    pub(crate) fn evicted(&self) -> usize {
        self.evicted
    }

    /// Records evictions the caller carried out against the store.
    ///
    /// The count lives here so the overlay reads one resource, but the decision cannot:
    /// only the caller holds [`ChunkStore`], and only it knows which of the coordinates
    /// [`Self::admit`] named were actually resident.
    fn note_evicted(&mut self, chunks: usize) {
        self.evicted += chunks;
    }

    /// Queues everything that arrived this frame, refusing chunk payloads once the
    /// backlog is full. Returns how many this call turned away.
    ///
    /// **The newest end gives way.** The stock argument for dropping the oldest is
    /// freshness — an old entry describes a state a newer one has already superseded —
    /// and it does not apply here, because a chunk payload is not a keyframe. A view
    /// diff sends each coordinate once, so the oldest payload in the queue and the
    /// newest describe *different* parts of the world and neither supersedes the other.
    /// There is no present to get closer to.
    ///
    /// What does apply is the order the server chose. `View.MoveTo` sorts a view update
    /// **nearest first** (`server/internal/session/streaming.go`), so the oldest queued
    /// payload is the ground under the player's feet and the newest is the horizon.
    /// Dropping the oldest would discard the floor and keep the skyline. Refusing at the
    /// tail also means nothing is ever removed from the middle of the queue, so the
    /// ordering guarantee the queue exists for — an unload must never overtake a load —
    /// needs no re-argument under the bound.
    ///
    /// **Nothing is pushed once the bound is reached**, which is what makes it a bound
    /// rather than a preference. The three kinds get there differently:
    ///
    ///   - A `Chunk` is refused. The coordinate stays absent from [`ChunkStore`], a
    ///     state the rest of the client already handles — nothing is drawn there and
    ///     [`ChunkStore::solid_at`] answers false — and the same outcome as the
    ///     malformed-chunk drop that has always been non-fatal.
    ///   - An `Unload` is *applied* rather than queued, by evicting its coordinate. It
    ///     already is an eviction; the server has said the chunk is gone. Jumping the
    ///     queue is safe here and nowhere else, because an eviction takes every queued
    ///     update for that coordinate with it, so no load left behind it can resurrect
    ///     the chunk. Refusing it was never an option: the server drops the coordinate
    ///     from `View.loaded` when it unloads and never mentions it again, so a refused
    ///     unload is this bound's own out-of-memory in slow motion.
    ///   - A `Block` is refused **and the chunk holding it is evicted**.
    ///
    /// **Why an edit evicts rather than simply being dropped.** Not for a faster
    /// recovery: the two are identical there. The server re-sends a chunk only once the
    /// coordinate leaves `View.loaded`, which happens when it leaves the view volume,
    /// and that is the wait either way. What eviction buys is that the divergence stops
    /// accumulating. A chunk kept while N edits are refused is wrong in N places and
    /// nothing records which; an absent chunk is wrong nowhere, and the copy that
    /// eventually arrives is right by construction — see [`BlockApplied::Unheld`], whose
    /// reasoning this reuses: the server invalidates a chunk's cached payload on every
    /// edit, so what it composes next already has the change in it.
    ///
    /// Evicting also *shrinks* the backlog, since it drops whatever was queued for that
    /// coordinate. A flood of edits for held chunks — the one shape that had no refusal
    /// available to stop it — now relieves the pressure it creates.
    ///
    /// **What becomes of an evicted chunk.** It is asked for. `ChunkResendRequest` is the
    /// message that closed this gap: the client names the coordinate, the server checks
    /// that this session still holds it in view, forgets it and diffs at the *current*
    /// centre — so the chunk comes back whole while the player is standing on the hole,
    /// rather than when they next walk out of the view volume and back. The request goes
    /// out where the eviction happens, in [`ingest_world_updates`], because that is the
    /// only place that already knows which coordinate was lost. See [`request_resends`]
    /// for what is asked for and what is deliberately not.
    ///
    /// **The newest end still gives way**, and the length is read before the eviction
    /// pass rather than after: a frame that reaches the bound refuses the rest of its
    /// arrivals even where a later eviction would have made room. Simpler, and it keeps
    /// the bound a ceiling on what one frame can add rather than a race against it.
    fn admit(&mut self, arrived: Vec<WorldUpdate>, size: usize) -> Admission {
        let mut refused = 0;
        // Keyed by coordinate so one frame evicts each at most once, and valued by whether
        // the client may ask for it back. **An unload always wins over a refused edit**,
        // whichever order the two arrive in: the server drops an unloaded coordinate from
        // `View.loaded` and never mentions it again, so a request for one would be refused
        // in silence — see [`Eviction::resendable`].
        let mut evicting: HashMap<ChunkCoord, bool> = HashMap::new();

        for update in arrived {
            if self.waiting.len() < MAX_DECODE_BACKLOG {
                self.waiting.push_back(update);
                continue;
            }
            match update {
                WorldUpdate::Chunk { .. } => refused += 1,
                WorldUpdate::Unload { coord } => {
                    evicting.insert(coord, false);
                }
                WorldUpdate::Block { pos, .. } => {
                    refused += 1;
                    // No chunk to evict when the position resolves to none, and nothing
                    // lost either: the drain would have answered `Unlocatable` and
                    // warned. Unreachable while `chunk_size` is `1..=40`.
                    if let Some((coord, _)) = locate(pos, size) {
                        evicting.entry(coord).or_insert(true);
                    }
                }
            }
        }

        if !evicting.is_empty() {
            // One pass for the whole frame rather than one per coordinate: the walk is
            // over the entire backlog, and the flood this exists for delivers many
            // evictions in the same frame.
            self.waiting
                .retain(|update| owner(update, size).is_none_or(|c| !evicting.contains_key(&c)));
        }

        self.refused += refused;
        Admission {
            refused,
            evicting: evicting
                .into_iter()
                .map(|(coord, resendable)| Eviction { coord, resendable })
                .collect(),
        }
    }
}

/// What one frame's arrivals cost, handed back by [`DecodeQueue::admit`].
///
/// A struct rather than a tuple because the two are read by different code for
/// different reasons: `refused` is a number to report, `evicting` is work the caller
/// has to carry out against a resource the queue cannot touch.
struct Admission {
    /// Updates the bound turned away this frame.
    refused: usize,
    /// Coordinates the bound evicted. Already gone from the queue; still to be dropped
    /// from [`ChunkStore`] by the caller, which is the only holder of it.
    evicting: Vec<Eviction>,
}

/// One coordinate the bound dropped, and whether the client may ask for it back.
struct Eviction {
    coord: ChunkCoord,
    /// Whether a `ChunkResendRequest` for this coordinate is worth sending.
    ///
    /// **True for an eviction the bound chose**, which is every one that came from a
    /// refused `BlockUpdate`. The server only broadcasts an edit to sessions whose view
    /// *holds* the chunk, so an edit arriving is proof that the server still records this
    /// session as having it — which is exactly the state its own resend check requires,
    /// and exactly the hole that would otherwise last until the player walked away and
    /// back.
    ///
    /// **False for an unload**, which is the server saying the chunk is gone. It drops the
    /// coordinate from `View.loaded` when it unloads it, so a request would be refused in
    /// silence: asking would spend a per-session rate limit that the chunks which *can*
    /// come back need, to be told what the client was already told.
    resendable: bool,
}

/// Which chunk an update is about, when it is about one at all.
///
/// The eviction pass needs one answer for all three kinds, and only `Block` has to do
/// arithmetic to give it.
fn owner(update: &WorldUpdate, size: usize) -> Option<ChunkCoord> {
    match *update {
        WorldUpdate::Chunk { coord, .. } | WorldUpdate::Unload { coord } => Some(coord),
        WorldUpdate::Block { pos, .. } => locate(pos, size).map(|(coord, _)| coord),
    }
}

/// Queues everything the net thread said about the world, and applies as much of the
/// backlog as this frame's budget allows.
///
/// Ordered after `net`'s drain, so a chunk that arrived this frame is decoded this
/// frame rather than next one — as long as the frames ahead of it in the queue have
/// been dealt with first.
///
/// The inbox is drained in full even when the budget is spent, so the backlog lives
/// on this side of the net boundary where the overlay can see it. `net`'s rule that a
/// drain never handles one event per frame is intact; what is bounded is the
/// *expansion*, not the handover.
///
/// The backlog itself is bounded too, at [`MAX_DECODE_BACKLOG`], and the refusal is
/// announced twice over: a log line at each edge of the episode, and a counter the
/// overlay carries live beside the queue depth.
pub(crate) fn ingest_world_updates(
    mut inbox: ResMut<WorldInbox>,
    mut queue: ResMut<DecodeQueue>,
    mut store: ResMut<ChunkStore>,
    // How much of this frame the drain below may spend. Read rather than assumed so the
    // suite can pin the two ends of it exactly — see [`DecodeTimeBudget`].
    budget: Res<DecodeTimeBudget>,
    session: Option<Res<Session>>,
    // Where a resend request goes out from, and the reason this system takes a sender at
    // all. An eviction is the only moment the coordinate that was lost is known; a pass
    // that went looking for holes afterwards would have to rediscover it. `Option`,
    // because `Outbound` exists exactly while there is a net thread to send to.
    mut outbound: Option<ResMut<Outbound>>,
    // Whether the bound is currently refusing. Holds the log to one line per episode
    // rather than one per refusal, exactly as `log_when_meshing_settles` does: a server
    // that over-streams refuses on every frame for as long as it lasts, and sixty
    // warnings a second is how the one line that mattered gets scrolled away.
    mut overflowing: Local<bool>,
) {
    let arrived = inbox.take();
    // Nothing to queue and nothing owed. Returned before either resource is touched
    // mutably, because `ResMut` marks a resource changed on every `DerefMut` and the
    // whole overlay hangs off those signals.
    if arrived.is_empty() && queue.waiting.is_empty() {
        return;
    }

    // `chunk_size` is the server's answer and the only length the runs may be
    // checked against. Unreachable when absent — the handshake refuses a world
    // payload before the welcome — but reported rather than unwrapped, because a
    // panic here would take the window down over a protocol error the status line
    // could have shown.
    let Some(session) = session else {
        warn!(
            "dropped {} world updates that arrived with no session",
            arrived.len() + queue.waiting.len()
        );
        // Dropped rather than kept: there is no `chunk_size` to check the runs
        // against, and a backlog nothing will ever be able to decode is a counter
        // that only ever climbs.
        if !queue.waiting.is_empty() {
            queue.waiting.clear();
        }
        // The episode ends with the backlog it was about. Left set, `overflowing` would
        // swallow the opening warning of the next one, and `refused` would stop being
        // the per-session total that its own documentation — and the drained-again log
        // below — both call it.
        //
        // Not reachable today: `Session` is inserted once and never removed, so `admit`
        // cannot have run before this branch fires and the count is already zero. Kept
        // because the two lines beside it are the same cleanup, and because the state
        // becomes reachable the moment reconnection removes the resource — which is
        // exactly when nobody goes back to work out which counters were per-session.
        // Guarded like the clear above it: an unconditional write would mark the
        // resource changed on a frame that changed nothing.
        if queue.refused != 0 {
            queue.refused = 0;
        }
        *overflowing = false;
        return;
    };
    let size = usize::from(session.0.chunk_size);

    if !arrived.is_empty() {
        let admission = queue.admit(arrived, size);

        // The store is the caller's to touch, and only the caller can tell an eviction
        // that took terrain from one that named a coordinate this session never held —
        // a queued load turned away before it ever landed. Only the first is counted,
        // because the count is what the overlay shows and a player can only see the
        // first.
        let mut evicted = 0;
        let mut lost = Vec::new();
        for eviction in admission.evicting {
            if store.get(eviction.coord).is_some() {
                store.unload(eviction.coord);
                evicted += 1;
            }
            // Asked for whether or not the store held it. The eviction pass takes every
            // queued update for the coordinate with it, so a payload that had arrived and
            // not been expanded yet is just as lost as one that had — and the server, which
            // records a chunk as delivered when it *sends* it, believes this session has
            // both. Which of them it will actually resend is the server's answer, not this
            // one's.
            if eviction.resendable {
                lost.push(eviction.coord);
            }
        }
        if evicted > 0 {
            queue.note_evicted(evicted);
        }
        if !lost.is_empty() {
            request_resends(outbound.as_deref_mut(), &lost);
        }

        // The half of "the cap announces itself" a test cannot read back: `warn!` goes
        // to a global subscriber a headless app does not install. The counters beside it
        // are the half that is asserted — see `DecodeQueue::refused`,
        // `DecodeQueue::evicted` and
        // `a_refused_payload_is_counted_where_the_overlay_can_see_it` — which is the
        // division `log_when_meshing_settles` already draws.
        if (admission.refused > 0 || evicted > 0) && !*overflowing {
            *overflowing = true;
            warn!(
                "world-update backlog is full at {MAX_DECODE_BACKLOG}; refusing updates \
                 until it drains, and evicting the chunks whose edits are turned away. \
                 The server is streaming faster than this client can expand; what is \
                 dropped is re-sent when its coordinate re-enters the view volume"
            );
        }
    }

    let mut spent = 0usize;
    // The clock starts here and not at the top of the system, because the admission pass
    // above is not optional: `net`'s boundary rule is that a drain never handles one
    // event per frame, so the inbox is emptied into the queue whatever this frame can
    // afford to expand. Charging that to a slice the loop is stopped by would let a
    // large arrival starve the expansion it just queued — which is the wrong way round,
    // since queuing is the cheap half and expanding is the half being bounded.
    let began = Instant::now();
    // This can now be reached with an empty queue, where once it could not: an
    // eviction drops every update queued for its coordinate, and a flood aimed at one
    // chunk empties the backlog it filled. The `while let` is still the right shape —
    // the first pop simply answers `None` — but the old claim that a pop is always
    // preceded by one that did real work is no longer true, and it was load-bearing
    // for nothing: what keeps an idle frame from touching either resource is the early
    // return above, which fires before anything is taken mutably.
    while let Some(update) = queue.waiting.pop_front() {
        match update {
            WorldUpdate::Chunk { coord, runs } => {
                // Counted whether the payload decodes or not: rejecting a malformed
                // chunk costs the same walk over its runs that accepting one does,
                // and a budget a bad payload could refill is not a budget.
                spent += 1;
                match VoxelChunk::from_runs(&runs, size) {
                    Ok(chunk) => store.insert(coord, chunk),
                    // Dropped, not fatal: the result is a hole in the terrain with the
                    // reason in the log, which is a better outcome than a client that
                    // exits because one frame out of five thousand was wrong. The
                    // server's own decoder closes the connection instead; it can, because
                    // it is holding the socket.
                    Err(err) => warn!(
                        "chunk {},{},{} is malformed and was dropped: {err}",
                        coord.cx, coord.cy, coord.cz
                    ),
                }
            }
            // Free of the budget: an unload is a map removal and a log entry, and
            // metering it would let a burst of unloads defer the loads behind them.
            WorldUpdate::Unload { coord } => store.unload(coord),
            // The authoritative answer to somebody's edit — possibly this player's,
            // possibly not, and the client cannot tell the difference and must not
            // try to. This is the one place a voxel changes after it arrives.
            WorldUpdate::Block { pos, block_id } => {
                match store.apply_block(pos, block_id, size) {
                    // Charged the same as an expansion, and for the same reason: an
                    // edited chunk is a fresh `size³` allocation and a copy into it.
                    BlockApplied::Rewritten { coord, remeshed } => {
                        spent += 1;
                        debug!(
                            "block {},{},{} is now {block_id}: chunk {},{},{} rewritten, \
                             {remeshed} chunk(s) to remesh",
                            pos.x, pos.y, pos.z, coord.cx, coord.cy, coord.cz,
                        );
                    }
                    // Costs no budget, for the same reason an unload does not: a map
                    // lookup is not the work being metered.
                    BlockApplied::Unheld { coord } => debug!(
                        "block {},{},{} is in chunk {},{},{}, which this session does \
                         not hold; dropped",
                        pos.x, pos.y, pos.z, coord.cx, coord.cy, coord.cz,
                    ),
                    // Unreachable: `chunk_size` is `1..=40` before a session exists.
                    BlockApplied::Unlocatable => warn!(
                        "block {},{},{} cannot be placed in a chunk of edge {size}",
                        pos.x, pos.y, pos.z,
                    ),
                }
            }
        }

        // Tested after the update has been applied and never before the pop: a budget
        // enforced by popping and discarding would lose whichever chunk was unlucky.
        //
        // **Both bounds are read here, and that placement is the progress guarantee.**
        // One update is popped and applied before either can stop the loop, so a frame
        // that is already over its slice still moves the queue forward by one — the
        // budget slows streaming down and can never stall it, however slow the machine
        // or however long the *rest* of the frame took. A check at the top of the body
        // would have neither property: with the clock started before the loop it would
        // still let one through, but the moment somebody moved the clock a line earlier
        // it would stop reading the queue entirely.
        //
        // The clock is read once per update. It is a vDSO call of a few tens of
        // nanoseconds against an expansion measured in tens of microseconds, so the
        // meter costs about a thousandth of what it meters.
        if spent >= MAX_DECODES_PER_FRAME || began.elapsed() >= budget.0 {
            break;
        }
    }

    // The other edge of the episode, at `info` because it is the recovery. The total is
    // the number worth keeping; the overlay has been carrying it live throughout.
    if *overflowing && queue.waiting.len() < MAX_DECODE_BACKLOG {
        *overflowing = false;
        info!(
            "world-update backlog is back under {MAX_DECODE_BACKLOG}; {} update(s) \
             refused and {} chunk(s) evicted so far this session",
            queue.refused(),
            queue.evicted()
        );
    }
}

/// Asks the server for every chunk this frame's bound took away.
///
/// **One request per eviction, and never a retry.** The contract has no reply to wait for
/// — `schemas/world.fbs` is explicit that an honoured request is answered by the
/// `ChunkData` that follows and a refused one by silence — so a retry could only be a
/// timer guessing at what silence meant, and a client that guessed wrong would be asking
/// the server for work it had already refused. A request lost to a full outbound queue
/// leaves its chunk exactly where the eviction left it: missing, and re-sent when the
/// coordinate next leaves the view volume and comes back, which is the recovery that
/// existed before this message did.
///
/// **The client is asking for data, not for an outcome.** It does not decide whether it
/// may have the chunk, and it must not treat the request as a promise. The server checks
/// that this session still holds the coordinate in view, refuses anything else in silence,
/// and rate limits what is left — so the honest failure mode of asking too often is that
/// the extra asks do nothing, not that the server does the work.
///
/// **Only the evictions that came from a refused edit are asked for**, which is what
/// [`Eviction::resendable`] records: an unload is the server saying the chunk is gone, and
/// asking for it would spend a budget the recoverable chunks need.
fn request_resends(outbound: Option<&mut Outbound>, coords: &[ChunkCoord]) {
    // No outbound means the session has ended, and a chunk is not something a session
    // that has ended is missing.
    let Some(outbound) = outbound else {
        return;
    };

    let mut dropped = 0;
    for &coord in coords {
        match outbound.send(encode_chunk_resend_request(coord)) {
            Sent::Queued => {}
            Sent::Dropped => dropped += 1,
            // The session has ended and `drain_session_events` has not caught up yet.
            Sent::Closed => return,
        }
    }

    // `debug` rather than `warn`, and counted per frame rather than per request: this is
    // reachable only while the backlog is already full, which is the episode the warning
    // beside the eviction announces. A dropped request costs the chunk nothing it had not
    // already lost.
    if dropped > 0 {
        debug!(
            "the outbound queue was full; {dropped} of {} resend request(s) never left",
            coords.len()
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::Receiver;

    use super::*;
    use crate::net::SessionParams;

    const SIZE: usize = 32;
    const VOLUME: usize = SIZE * SIZE * SIZE;

    fn coord(cx: i32, cy: i32, cz: i32) -> ChunkCoord {
        ChunkCoord { cx, cy, cz }
    }

    /// Run-length encodes a dense voxel array exactly as `world.Encode` does.
    ///
    /// Test-only, and the point of it is to be the *server's* algorithm rather than
    /// the inverse of the decoder: a round trip through a decoder's own mirror image
    /// would pass even if both sides agreed on the wrong format.
    fn encode_runs(blocks: &[BlockId]) -> Vec<u16> {
        // maxRun in server/internal/world/rle.go: the longest run a u16 can express.
        const MAX_RUN: u16 = u16::MAX;

        let mut pairs = Vec::new();
        let mut current = blocks[0];
        let mut run: u16 = 1;
        for block in &blocks[1..] {
            if *block == current && run < MAX_RUN {
                run += 1;
                continue;
            }
            pairs.extend_from_slice(&[current, run]);
            current = *block;
            run = 1;
        }
        pairs.extend_from_slice(&[current, run]);
        pairs
    }

    #[test]
    fn the_index_order_is_the_wire_order() {
        // x fastest, then z, then y — schemas/world.fbs and world.Index. If this
        // drifts, terrain arrives transposed and no other test would say why.
        assert_eq!(index(SIZE, 0, 0, 0), 0);
        assert_eq!(index(SIZE, 1, 0, 0), 1);
        assert_eq!(index(SIZE, 0, 0, 1), SIZE);
        assert_eq!(index(SIZE, 0, 1, 0), SIZE * SIZE);
        assert_eq!(index(SIZE, 31, 31, 31), VOLUME - 1);
    }

    #[test]
    fn an_all_air_chunk_round_trips_through_the_servers_encoder() {
        let blocks = vec![palette::AIR; VOLUME];
        let runs = encode_runs(&blocks);

        assert_eq!(runs, vec![palette::AIR, 32768], "one run spans the chunk");
        let chunk = VoxelChunk::from_runs(&runs, SIZE).expect("that is a valid chunk");
        assert_eq!(chunk.size(), SIZE);
        for y in 0..SIZE {
            assert_eq!(chunk.block([0, y, 0]), palette::AIR);
        }
    }

    #[test]
    fn terrain_shaped_runs_round_trip() {
        // The shape the generator actually produces: stone, then dirt, then a
        // surface block, then air above — a handful of long runs per column.
        let mut blocks = vec![palette::AIR; VOLUME];
        for y in 0..SIZE {
            for z in 0..SIZE {
                for x in 0..SIZE {
                    let surface = 12 + (x + z) % 5;
                    let block = match y {
                        y if y > surface => palette::AIR,
                        y if y == surface => palette::GRASS,
                        y if y + 4 > surface => palette::DIRT,
                        _ => palette::STONE,
                    };
                    blocks[index(SIZE, x, y, z)] = block;
                }
            }
        }

        let chunk = VoxelChunk::from_runs(&encode_runs(&blocks), SIZE).expect("valid");
        for y in 0..SIZE {
            for z in 0..SIZE {
                for x in 0..SIZE {
                    assert_eq!(
                        chunk.block([x, y, z]),
                        blocks[index(SIZE, x, y, z)],
                        "voxel {x},{y},{z}"
                    );
                }
            }
        }
    }

    #[test]
    fn architectural_shape_ids_survive_the_wire_runs_unchanged() {
        let shapes = [
            palette::SLATE_SLAB_BOTTOM,
            palette::SLATE_SLAB_TOP,
            palette::SLATE_STAIR_NORTH_BOTTOM,
            palette::SLATE_STAIR_EAST_BOTTOM,
            palette::SLATE_STAIR_SOUTH_BOTTOM,
            palette::SLATE_STAIR_WEST_BOTTOM,
            palette::SLATE_STAIR_NORTH_TOP,
            palette::SLATE_STAIR_EAST_TOP,
            palette::SLATE_STAIR_SOUTH_TOP,
            palette::SLATE_STAIR_WEST_TOP,
        ];
        let blocks: Vec<_> = shapes.into_iter().cycle().take(VOLUME).collect();
        let chunk = VoxelChunk::from_runs(&encode_runs(&blocks), SIZE).expect("valid shape ids");
        assert_eq!(chunk.blocks, blocks);
    }

    #[test]
    fn the_alternating_worst_case_round_trips() {
        // Two values per voxel: the largest payload the format can produce, and the
        // one that proves the decoder does not assume runs are long.
        let blocks: Vec<BlockId> = (0..VOLUME)
            .map(|i| {
                if i % 2 == 0 {
                    palette::STONE
                } else {
                    palette::AIR
                }
            })
            .collect();
        let runs = encode_runs(&blocks);

        assert_eq!(runs.len(), VOLUME * 2, "every voxel is its own run");
        let chunk = VoxelChunk::from_runs(&runs, SIZE).expect("valid, if large");
        for (i, block) in blocks.iter().enumerate() {
            let (x, z, y) = (i % SIZE, (i / SIZE) % SIZE, i / (SIZE * SIZE));
            assert_eq!(chunk.block([x, y, z]), *block, "voxel {i}");
        }
    }

    #[test]
    fn the_largest_chunk_the_contract_allows_still_fits_one_run() {
        // 40³ = 64 000 ≤ 65 535, which is exactly what `world.fbs` means when it says
        // raising chunk_size past 40 would need the run length widened. The largest
        // legal chunk is still a single run, and the client must accept it.
        const BIG: usize = 40;
        let runs = encode_runs(&vec![palette::STONE; BIG * BIG * BIG]);

        assert_eq!(runs, vec![palette::STONE, 64000]);
        let chunk = VoxelChunk::from_runs(&runs, BIG).expect("valid");
        assert_eq!(chunk.block([39, 39, 39]), palette::STONE);
    }

    #[test]
    fn consecutive_runs_of_the_same_block_are_joined() {
        // Nothing in the contract says a block id appears in only one run, and the
        // server's encoder splits at 0xFFFF the moment a chunk outgrows it. A decoder
        // that treated a repeated id as a mistake would reject a legal payload.
        let split = vec![palette::STONE, 12000, palette::STONE, 20768];
        let chunk = VoxelChunk::from_runs(&split, SIZE).expect("valid");

        assert_eq!(chunk.block([0, 0, 0]), palette::STONE);
        assert_eq!(chunk.block([31, 31, 31]), palette::STONE);
    }

    #[test]
    fn no_runs_is_not_an_empty_chunk() {
        assert_eq!(VoxelChunk::from_runs(&[], SIZE), Err(RunsError::NoRuns));
    }

    #[test]
    fn an_odd_number_of_values_is_refused() {
        assert_eq!(
            VoxelChunk::from_runs(&[1, 32768, 2], SIZE),
            Err(RunsError::OddLength { len: 3 })
        );
    }

    #[test]
    fn a_zero_length_run_is_refused() {
        // Forbidden by the contract, and the shape of a payload that could otherwise
        // carry unbounded pairs without ever filling the chunk.
        assert_eq!(
            VoxelChunk::from_runs(&[1, 100, 2, 0, 3, 32668], SIZE),
            Err(RunsError::ZeroLengthRun { pair: 1 })
        );
    }

    #[test]
    fn runs_that_overflow_the_chunk_are_refused_before_the_allocation_grows() {
        assert_eq!(
            VoxelChunk::from_runs(&[1, 32768, 2, 1], SIZE),
            Err(RunsError::TooManyVoxels { volume: VOLUME })
        );
    }

    #[test]
    fn a_hostile_payload_cannot_size_the_allocation() {
        // 512 runs of 65 535 voxels each asks for 33 million voxels of a 32 768-voxel
        // chunk. It must be refused on the run that crosses the volume, not after the
        // vector has grown to whatever was asked for.
        let runs: Vec<u16> = std::iter::repeat_n([1u16, u16::MAX], 512)
            .flatten()
            .collect();

        assert_eq!(
            VoxelChunk::from_runs(&runs, SIZE),
            Err(RunsError::TooManyVoxels { volume: VOLUME })
        );
    }

    #[test]
    fn runs_that_stop_short_are_refused() {
        // Half a chunk read as terrain would be a floor of air with no explanation.
        assert_eq!(
            VoxelChunk::from_runs(&[1, 16384], SIZE),
            Err(RunsError::Incomplete {
                got: 16384,
                want: VOLUME
            })
        );
    }

    #[test]
    fn the_smallest_legal_chunk_size_works() {
        // chunk_size 1 is legal in the contract, and its volume is one voxel.
        let chunk = VoxelChunk::from_runs(&[palette::SNOW, 1], 1).expect("valid");
        assert_eq!(chunk.block([0, 0, 0]), palette::SNOW);
    }

    #[test]
    fn a_block_id_this_build_does_not_know_is_stored_verbatim() {
        // The client renders what the server sent. An unknown id is a newer contract,
        // not a corrupt chunk, and silently rewriting it to air would delete terrain.
        let chunk = VoxelChunk::from_runs(&[999, 32768], SIZE).expect("valid");
        assert_eq!(chunk.block([5, 5, 5]), 999);
    }

    // -----------------------------------------------------------------
    // The store and the ingest system
    // -----------------------------------------------------------------

    fn air_runs() -> Vec<u16> {
        vec![palette::AIR, 32768]
    }

    fn stone_runs() -> Vec<u16> {
        vec![palette::STONE, 32768]
    }

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 1,
            spawn: [0.5, 80.0, 0.5],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: SIZE as u16,
            view_distance: 8,
            inventory_slots: 37,
            hotbar_slots: 9,
            equipment_slots: 4,
            player_token: crate::net::ANY_TOKEN,
            voice_range_blocks: 0.0,
        })
    }

    /// An app running only the ingest system, with the inbox a test can fill.
    ///
    /// The time budget is [`Duration::MAX`] here, so the *count* is the only bound the
    /// drain can reach and every assertion about how much one frame expands is exact.
    /// Anything else would be timing the runner: see [`DecodeTimeBudget`], and
    /// [`metered_ingest_app`] for the tests that are about the other bound.
    fn ingest_app(with_session: bool) -> App {
        let mut app = metered_app(Duration::MAX);
        if with_session {
            app.insert_resource(session());
        }
        app
    }

    /// An ingest app whose drain may spend exactly `budget` on the backlog.
    fn metered_app(budget: Duration) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<WorldInbox>()
            .init_resource::<DecodeQueue>()
            .init_resource::<ChunkStore>()
            .insert_resource(DecodeTimeBudget(budget))
            .add_systems(Update, ingest_world_updates);
        app
    }

    /// A session-carrying ingest app whose drain may spend exactly `budget`.
    fn metered_ingest_app(budget: Duration) -> App {
        let mut app = metered_app(budget);
        app.insert_resource(session());
        app
    }

    fn push(app: &mut App, update: WorldUpdate) {
        app.world_mut().resource_mut::<WorldInbox>().push(update);
    }

    fn chunk_count(app: &App) -> usize {
        app.world().resource::<ChunkStore>().chunk_count()
    }

    fn backlog(app: &App) -> usize {
        app.world().resource::<DecodeQueue>().len()
    }

    fn refused(app: &App) -> usize {
        app.world().resource::<DecodeQueue>().refused()
    }

    fn evicted(app: &App) -> usize {
        app.world().resource::<DecodeQueue>().evicted()
    }

    #[test]
    fn a_chunk_is_decoded_and_stored_under_its_coordinate() {
        let mut app = ingest_app(true);
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(0, 2, 0),
                runs: stone_runs(),
            },
        );
        app.update();

        let store = app.world().resource::<ChunkStore>();
        assert_eq!(store.chunk_count(), 1);
        let chunk = store.get(coord(0, 2, 0)).expect("stored under its coord");
        assert_eq!(chunk.size(), SIZE);
        assert_eq!(chunk.block([1, 2, 3]), palette::STONE);
        assert!(store.get(coord(0, 0, 0)).is_none());
    }

    #[test]
    fn an_unload_drops_the_chunk() {
        let mut app = ingest_app(true);
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(1, 1, 1),
                runs: stone_runs(),
            },
        );
        app.update();
        push(
            &mut app,
            WorldUpdate::Unload {
                coord: coord(1, 1, 1),
            },
        );
        app.update();

        assert_eq!(app.world().resource::<ChunkStore>().chunk_count(), 0);
    }

    #[test]
    fn the_change_log_keeps_the_servers_order() {
        // Unload then re-send is what the server does when a player leaves a chunk
        // and comes back. Applied backwards, the mesh of a chunk the player can see
        // would be despawned.
        let mut app = ingest_app(true);
        for update in [
            WorldUpdate::Chunk {
                coord: coord(0, 0, 0),
                runs: stone_runs(),
            },
            WorldUpdate::Unload {
                coord: coord(0, 0, 0),
            },
            WorldUpdate::Chunk {
                coord: coord(0, 0, 0),
                runs: air_runs(),
            },
        ] {
            push(&mut app, update);
        }
        app.update();

        let changes = app.world_mut().resource_mut::<ChunkStore>().take_changes();
        assert_eq!(
            changes,
            vec![
                ChunkChange::Loaded(coord(0, 0, 0)),
                ChunkChange::Unloaded(coord(0, 0, 0)),
                ChunkChange::Loaded(coord(0, 0, 0)),
            ]
        );
        // And the last word is the one that stands.
        let store = app.world().resource::<ChunkStore>();
        assert_eq!(
            store.get(coord(0, 0, 0)).expect("re-sent").block([0, 0, 0]),
            palette::AIR
        );
    }

    #[test]
    fn taking_the_changes_empties_the_log() {
        let mut app = ingest_app(true);
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(0, 0, 0),
                runs: stone_runs(),
            },
        );
        app.update();

        let mut store = app.world_mut().resource_mut::<ChunkStore>();
        assert_eq!(store.take_changes().len(), 1);
        assert!(store.take_changes().is_empty(), "the log is not replayed");
    }

    #[test]
    fn a_malformed_chunk_is_dropped_and_the_session_survives() {
        // A hole in the terrain with the reason in the log beats a client that exits
        // because one frame was wrong. The rest of the stream must keep arriving.
        let mut app = ingest_app(true);
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(0, 0, 0),
                runs: vec![1, 5],
            },
        );
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(1, 0, 0),
                runs: stone_runs(),
            },
        );
        app.update();

        let store = app.world().resource::<ChunkStore>();
        assert_eq!(store.chunk_count(), 1, "only the good chunk landed");
        assert!(store.get(coord(0, 0, 0)).is_none());
        assert!(store.get(coord(1, 0, 0)).is_some());
    }

    #[test]
    fn a_re_sent_chunk_replaces_the_earlier_copy() {
        let mut app = ingest_app(true);
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(0, 0, 0),
                runs: stone_runs(),
            },
        );
        app.update();
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(0, 0, 0),
                runs: air_runs(),
            },
        );
        app.update();

        let store = app.world().resource::<ChunkStore>();
        assert_eq!(store.chunk_count(), 1, "one chunk per coordinate, always");
        assert_eq!(
            store
                .get(coord(0, 0, 0))
                .expect("still there")
                .block([0, 0, 0]),
            palette::AIR
        );
    }

    #[test]
    fn updates_without_a_session_are_dropped_rather_than_guessed_at() {
        // Unreachable through the handshake, which refuses terrain before the
        // welcome. If it were reachable, `chunk_size` would be unknown and any guess
        // would be a wrong one.
        let mut app = ingest_app(false);
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(0, 0, 0),
                runs: stone_runs(),
            },
        );
        app.update();

        assert_eq!(app.world().resource::<ChunkStore>().chunk_count(), 0);
        assert_eq!(
            app.world().resource::<WorldInbox>().pending(),
            0,
            "and they are not left to pile up"
        );
    }

    #[test]
    fn an_idle_frame_touches_nothing() {
        let mut app = ingest_app(true);
        app.update();

        assert_eq!(chunk_count(&app), 0);
        assert_eq!(backlog(&app), 0);
        assert!(
            app.world_mut()
                .resource_mut::<ChunkStore>()
                .take_changes()
                .is_empty()
        );
    }

    // -----------------------------------------------------------------
    // Editing a voxel
    // -----------------------------------------------------------------

    fn at(x: i32, y: i32, z: i32) -> BlockCoord {
        BlockCoord { x, y, z }
    }

    /// A store holding one solid chunk of stone at `coord`, with its change log already
    /// taken, so what a test asserts on afterwards is only the edit's own entries.
    fn store_with_stone_at(coord: ChunkCoord) -> ChunkStore {
        let mut store = ChunkStore::default();
        store.insert(
            coord,
            VoxelChunk::from_runs(&stone_runs(), SIZE).expect("valid"),
        );
        store.take_changes();
        store
    }

    #[test]
    fn a_world_block_coordinate_lands_in_the_chunk_that_holds_it() {
        // Euclidean division, and the negative cases are the whole point: Rust's `/`
        // rounds towards zero, so a truncating split would put x = -1 in chunk 0 at local
        // x = -1. Every edit west, south or below the origin would land in the wrong
        // chunk, and only on that side of the world — so half a suite would agree with it.
        for (pos, want) in [
            (at(0, 0, 0), (coord(0, 0, 0), [0, 0, 0])),
            (at(31, 31, 31), (coord(0, 0, 0), [31, 31, 31])),
            (at(32, 0, 0), (coord(1, 0, 0), [0, 0, 0])),
            (at(-1, -1, -1), (coord(-1, -1, -1), [31, 31, 31])),
            (at(-32, 0, 0), (coord(-1, 0, 0), [0, 0, 0])),
            (at(-33, 0, 0), (coord(-2, 0, 0), [31, 0, 0])),
            (at(3, 81, -1), (coord(0, 2, -1), [3, 17, 31])),
        ] {
            assert_eq!(locate(pos, SIZE), Some(want), "{pos:?}");
        }
    }

    #[test]
    fn a_chunk_edge_no_session_can_have_locates_nothing() {
        // Unreachable through the decoder, which caps `chunk_size` at 1..=40 before
        // `SessionParams` exists. Answered rather than divided by: `rem_euclid(0)` panics,
        // and a client that exited over one malformed number would be a worse client.
        assert_eq!(locate(at(0, 0, 0), 0), None);
        assert_eq!(locate(at(0, 0, 0), usize::MAX), None);
    }

    #[test]
    fn an_edit_changes_one_voxel_and_marks_exactly_one_chunk() {
        // The middle of a chunk, so no border is involved. One voxel becomes air, its
        // neighbours do not, and the renderer is told about one chunk.
        let mut store = store_with_stone_at(coord(0, 0, 0));

        assert_eq!(
            store.apply_block(at(10, 11, 12), palette::AIR, SIZE),
            BlockApplied::Rewritten {
                coord: coord(0, 0, 0),
                remeshed: 1
            }
        );

        let chunk = store.get(coord(0, 0, 0)).expect("still held");
        assert_eq!(chunk.block([10, 11, 12]), palette::AIR, "the voxel changed");
        for beside in [[9, 11, 12], [11, 11, 12], [10, 10, 12], [10, 11, 13]] {
            assert_eq!(
                chunk.block(beside),
                palette::STONE,
                "voxel {beside:?} changed too"
            );
        }
        assert_eq!(
            store.take_changes(),
            vec![ChunkChange::Loaded(coord(0, 0, 0))]
        );
    }

    #[test]
    fn placing_a_block_is_the_same_path_as_breaking_one() {
        // One message covers both directions of change — a break is a placement of air —
        // so there is one code path and no second one to keep in step with it.
        let mut store = ChunkStore::default();
        store.insert(
            coord(0, 0, 0),
            VoxelChunk::from_runs(&air_runs(), SIZE).expect("valid"),
        );
        store.take_changes();

        store.apply_block(at(4, 5, 6), palette::SNOW, SIZE);

        assert_eq!(
            store.get(coord(0, 0, 0)).expect("held").block([4, 5, 6]),
            palette::SNOW
        );
    }

    #[test]
    fn an_edit_on_a_chunk_border_marks_the_neighbour_as_well() {
        // The border rule. A face on the shared wall is culled against the neighbour's
        // voxel, so an edit there makes the *neighbour's* mesh wrong even though none of
        // its voxels moved. The control below is what makes this a rule rather than a
        // blanket "remesh everything nearby": one voxel further in, the neighbour is not
        // touched.
        for (pos, neighbour) in [
            (at(0, 5, 5), coord(-1, 0, 0)),
            (at(31, 5, 5), coord(1, 0, 0)),
            (at(5, 0, 5), coord(0, -1, 0)),
            (at(5, 31, 5), coord(0, 1, 0)),
            (at(5, 5, 0), coord(0, 0, -1)),
            (at(5, 5, 31), coord(0, 0, 1)),
        ] {
            let mut store = store_with_stone_at(coord(0, 0, 0));

            assert_eq!(
                store.apply_block(pos, palette::AIR, SIZE),
                BlockApplied::Rewritten {
                    coord: coord(0, 0, 0),
                    remeshed: 2
                },
                "{pos:?}"
            );
            assert_eq!(
                store.take_changes(),
                vec![
                    ChunkChange::Loaded(coord(0, 0, 0)),
                    ChunkChange::NeighbourChanged(neighbour),
                ],
                "{pos:?}"
            );
        }
    }

    #[test]
    fn an_edit_one_voxel_inside_the_border_marks_nobody_else() {
        // The control for the rule above, and the half that keeps it from being "remesh
        // the neighbourhood": these voxels have a solid neighbour on every side *within
        // their own chunk*, so no other chunk's mesh can depend on them.
        for pos in [at(1, 5, 5), at(30, 5, 5), at(5, 1, 5), at(5, 5, 30)] {
            let mut store = store_with_stone_at(coord(0, 0, 0));

            assert_eq!(
                store.apply_block(pos, palette::AIR, SIZE),
                BlockApplied::Rewritten {
                    coord: coord(0, 0, 0),
                    remeshed: 1
                },
                "{pos:?}"
            );
            assert_eq!(
                store.take_changes(),
                vec![ChunkChange::Loaded(coord(0, 0, 0))],
                "{pos:?} pulled a neighbour in with it"
            );
        }
    }

    #[test]
    fn breaking_a_flower_on_a_border_marks_nobody_across_it() {
        // #551 adds no remesh rule, and this is why it needs none: a flower's whole
        // geometry is inside its own voxel, so a neighbour's sweep can no more see one
        // than it can see the air that replaces it. The shared border key is
        // `(is_opaque, water_level)`, and cover and air agree on both.
        for pos in [at(0, 5, 5), at(31, 5, 5), at(5, 5, 31), at(0, 0, 0)] {
            let mut store = store_with_stone_at(coord(0, 0, 0));
            store.apply_block(pos, palette::FLOWER_RED, SIZE);
            let _ = store.take_changes();

            assert_eq!(
                store.apply_block(pos, palette::AIR, SIZE),
                BlockApplied::Rewritten {
                    coord: coord(0, 0, 0),
                    remeshed: 1
                },
                "{pos:?}"
            );
            assert_eq!(
                store.take_changes(),
                vec![ChunkChange::Loaded(coord(0, 0, 0))],
                "{pos:?} pulled a neighbour in with it"
            );
        }
    }

    #[test]
    fn an_edit_that_leaves_a_border_voxel_solid_marks_nobody() {
        // The other control, and the one the review on legacy PR 66 found missing. A neighbour's
        // mesh depends on the occupancy of this chunk's boundary voxels and on nothing
        // else, so one full cube becoming another on a shared wall changes nothing
        // across it — the neighbour would be remeshed into a byte-identical mesh. The
        // edited chunk still remeshes, because colour is its own.
        //
        // The corner is in the list deliberately: it is where the old behaviour was
        // most expensive, marking three chunks for an edit none of them can see.
        for pos in [at(0, 5, 5), at(31, 5, 5), at(5, 5, 31), at(0, 0, 0)] {
            let mut store = store_with_stone_at(coord(0, 0, 0));

            assert_eq!(
                store.apply_block(pos, palette::GRASS, SIZE),
                BlockApplied::Rewritten {
                    coord: coord(0, 0, 0),
                    remeshed: 1
                },
                "{pos:?}"
            );
            assert_eq!(
                store.take_changes(),
                vec![ChunkChange::Loaded(coord(0, 0, 0))],
                "{pos:?} marked a neighbour that cannot see the difference"
            );
        }

        // And the rule is about solidity, not about the edit being a no-op: the same
        // voxels still pull their neighbours in when they stop being solid.
        let mut store = store_with_stone_at(coord(0, 0, 0));
        assert_eq!(
            store.apply_block(at(31, 5, 5), palette::AIR, SIZE),
            BlockApplied::Rewritten {
                coord: coord(0, 0, 0),
                remeshed: 2
            },
            "the guard swallowed an edit that does change the seam"
        );
    }

    #[test]
    fn changing_a_border_shapes_occupied_half_marks_the_neighbour() {
        let pos = at(31, 5, 5);
        let mut store = store_with_stone_at(coord(0, 0, 0));

        for block in [palette::SLATE_SLAB_BOTTOM, palette::SLATE_SLAB_TOP] {
            assert_eq!(
                store.apply_block(pos, block, SIZE),
                BlockApplied::Rewritten {
                    coord: coord(0, 0, 0),
                    remeshed: 2,
                },
                "shape {block} did not invalidate the chunk that reads its partial face"
            );
        }
        assert_eq!(
            store
                .take_changes()
                .into_iter()
                .filter(|change| { *change == ChunkChange::NeighbourChanged(coord(1, 0, 0)) })
                .count(),
            2,
            "full -> bottom and bottom -> top both move the shared half-grid"
        );
    }

    #[test]
    fn a_border_edit_that_adds_or_changes_water_geometry_marks_the_neighbour() {
        let at_border = at(31, 5, 5);
        let mut store = ChunkStore::default();
        store.insert(coord(0, 0, 0), air());
        store.take_changes();

        for (block, remeshed) in [
            (palette::WATER_FLOW7, 2),
            (palette::WATER_FLOW3, 2),
            (palette::WATER, 2),
            (palette::WATER_CURRENT_XPOS, 1),
        ] {
            assert_eq!(
                store.apply_block(at_border, block, SIZE),
                BlockApplied::Rewritten {
                    coord: coord(0, 0, 0),
                    remeshed,
                }
            );
            store.take_changes();
        }
    }

    #[test]
    fn a_level_only_corner_edit_marks_horizontal_but_not_vertical_neighbours() {
        let mut store = ChunkStore::default();
        store.insert(coord(0, 0, 0), air());
        store.take_changes();
        store.apply_block(at(31, 31, 5), palette::WATER_FLOW3, SIZE);
        store.take_changes();

        store.apply_block(at(31, 31, 5), palette::WATER_FLOW4, SIZE);
        assert_eq!(
            store.take_changes(),
            vec![
                ChunkChange::Loaded(coord(0, 0, 0)),
                ChunkChange::NeighbourChanged(coord(1, 0, 0)),
            ]
        );
    }

    #[test]
    fn a_mismatched_below_chunk_does_not_panic_or_mark_a_diagonal() {
        let here = coord(0, 0, 0);
        let below = coord(0, -1, 0);
        let mut below_chunk = VoxelChunk::from_runs(&[palette::AIR, 4096], 16).expect("valid");
        below_chunk.set(15, 15, 5, palette::WATER_FLOW3);
        let mut store = ChunkStore::default();
        store.insert(below, below_chunk);
        store.insert(coord(1, -1, 0), air());
        store.insert(here, air());
        store.take_changes();

        store.apply_block(at(15, 0, 5), palette::WATER_FLOW1, SIZE);
        assert_eq!(
            store.take_changes(),
            vec![
                ChunkChange::Loaded(here),
                ChunkChange::NeighbourChanged(below),
            ]
        );
    }

    #[test]
    fn an_edit_in_a_corner_marks_the_three_chunks_that_share_a_face_with_it() {
        // Three, not seven: this non-water edit changes only shared-face geometry.
        let mut store = store_with_stone_at(coord(0, 0, 0));

        assert_eq!(
            store.apply_block(at(0, 0, 0), palette::AIR, SIZE),
            BlockApplied::Rewritten {
                coord: coord(0, 0, 0),
                remeshed: 4
            }
        );
        assert_eq!(
            store.take_changes(),
            vec![
                ChunkChange::Loaded(coord(0, 0, 0)),
                ChunkChange::NeighbourChanged(coord(-1, 0, 0)),
                ChunkChange::NeighbourChanged(coord(0, -1, 0)),
                ChunkChange::NeighbourChanged(coord(0, 0, -1)),
            ]
        );
    }

    #[test]
    fn the_smallest_legal_chunk_borders_every_neighbour_at_once() {
        // `chunk_size` 1 is legal in the contract, and its one voxel is simultaneously at
        // 0 and at `size - 1` on all three axes. Written as two independent bounds tests
        // rather than an `else if` precisely so this case answers six rather than three.
        let local = [0usize; 3];
        let mut neighbours = border_neighbours(coord(0, 0, 0), local, 1);
        neighbours.sort_unstable_by_key(|c| (c.cx, c.cy, c.cz));

        assert_eq!(
            neighbours,
            vec![
                coord(-1, 0, 0),
                coord(0, -1, 0),
                coord(0, 0, -1),
                coord(0, 0, 1),
                coord(0, 1, 0),
                coord(1, 0, 0),
            ]
        );
    }

    #[test]
    fn a_chunk_at_the_end_of_the_coordinate_range_has_no_neighbour_that_way() {
        // Wrapping would name a chunk on the far side of the world and saturating would
        // name this one, so the neighbour that does not exist is simply left out.
        assert_eq!(
            border_neighbours(coord(i32::MAX, 0, 0), [SIZE - 1, 5, 5], SIZE),
            Vec::new()
        );
        assert_eq!(
            border_neighbours(coord(i32::MIN, 0, 0), [0, 5, 5], SIZE),
            Vec::new()
        );
    }

    // -----------------------------------------------------------------
    // The neighbourhood a chunk is meshed against
    // -----------------------------------------------------------------

    /// A solid stone chunk of the session's size.
    fn stone() -> VoxelChunk {
        VoxelChunk::from_runs(&stone_runs(), SIZE).expect("valid")
    }

    /// An all-air chunk of the session's size.
    fn air() -> VoxelChunk {
        VoxelChunk::from_runs(&air_runs(), SIZE).expect("valid")
    }

    #[test]
    fn the_neighbourhood_is_the_six_chunks_that_share_a_face() {
        // The store gathers the six and the mesher indexes them, and this asserts the two
        // agree — through the mesh rather than by reading the slots back, because a pair
        // of slots swapped is only wrong in what it makes the mesher draw. An assertion
        // that read the slots in the same order that filled them would agree with itself.
        let mut store = ChunkStore::default();
        for at in [
            coord(0, 0, 0),
            coord(-1, 0, 0),
            coord(1, 0, 0),
            coord(0, -1, 0),
            coord(0, 1, 0),
            coord(0, 0, -1),
            coord(0, 0, 1),
            // Shares an edge and no face: it must not reach the mesher at all.
            coord(1, 1, 0),
        ] {
            store.insert(at, stone());
        }

        let middle = store.get(coord(0, 0, 0)).expect("held");
        assert!(
            mesh_chunk(middle, &store.neighbours(coord(0, 0, 0))).is_empty(),
            "a chunk with a solid neighbour across all six faces has nothing to draw"
        );

        // And one of them taken away is exactly one wall back, facing the gap.
        store.unload(coord(0, 1, 0));
        let mesh = mesh_chunk(
            store.get(coord(0, 0, 0)).expect("held"),
            &store.neighbours(coord(0, 0, 0)),
        );
        assert_eq!(mesh.quad_count(), 1);
        assert_eq!(
            mesh.opaque.normals[0],
            [0.0, 1.0, 0.0],
            "the wrong wall came back"
        );
    }

    #[test]
    fn a_chunk_at_the_end_of_the_coordinate_range_has_no_neighbourhood_that_way() {
        // Same reasoning as `border_neighbours`: wrapping would name a chunk on the far
        // side of the world. Absent is the answer, and the mesher already knows what to
        // do with it.
        let mut store = ChunkStore::default();
        store.insert(coord(i32::MAX, 0, 0), stone());

        let mesh = mesh_chunk(
            store.get(coord(i32::MAX, 0, 0)).expect("held"),
            &store.neighbours(coord(i32::MAX, 0, 0)),
        );
        assert_eq!(
            mesh.quad_count(),
            6,
            "all six walls, and no arithmetic panic"
        );
    }

    #[test]
    fn a_chunk_arriving_marks_the_neighbours_it_now_hides() {
        // The other half of the culling: a mesh already on screen goes stale the moment
        // the chunk it was meshed against arrives. Without this the newcomer would be
        // culled correctly and its neighbour would keep drawing the wall between them.
        let mut store = store_with_stone_at(coord(0, 0, 0));

        store.insert(coord(1, 0, 0), stone());

        assert_eq!(
            store.take_changes(),
            vec![
                ChunkChange::Loaded(coord(1, 0, 0)),
                ChunkChange::NeighbourChanged(coord(0, 0, 0)),
            ]
        );
    }

    #[test]
    fn a_chunk_arriving_marks_only_the_chunks_that_share_a_face_with_it() {
        // The no-cascade rule at the store, where the queue entries are created. Six, not
        // twenty-six: a chunk across an edge or a corner shares no face with this one, so
        // no quad of its mesh can depend on it.
        let mut store = ChunkStore::default();
        for at in [
            coord(-1, 0, 0),
            coord(1, 0, 0),
            coord(0, -1, 0),
            coord(0, 1, 0),
            coord(0, 0, -1),
            coord(0, 0, 1),
            coord(1, 1, 0),
            coord(1, 1, 1),
        ] {
            store.insert(at, stone());
        }
        store.take_changes();

        store.insert(coord(0, 0, 0), stone());

        let mut marked: Vec<(i32, i32, i32)> = store
            .take_changes()
            .into_iter()
            .filter_map(|change| match change {
                ChunkChange::NeighbourChanged(c) => Some((c.cx, c.cy, c.cz)),
                _ => None,
            })
            .collect();
        marked.sort_unstable();
        assert_eq!(
            marked,
            vec![
                (-1, 0, 0),
                (0, -1, 0),
                (0, 0, -1),
                (0, 0, 1),
                (0, 1, 0),
                (1, 0, 0),
            ]
        );
    }

    #[test]
    fn a_chunk_of_air_arriving_marks_nobody() {
        // What keeps a join affordable. A missing neighbour is already meshed against as
        // air, so a chunk of air arriving changes nothing anyone draws — and most of what
        // the server streams is sky. Invalidating on arrival alone would remesh up to six
        // neighbours per chunk for 4 913 chunks, almost all for an identical result.
        let mut store = store_with_stone_at(coord(0, 0, 0));

        store.insert(coord(1, 0, 0), air());

        assert_eq!(
            store.take_changes(),
            vec![ChunkChange::Loaded(coord(1, 0, 0))]
        );
    }

    #[test]
    fn a_re_sent_chunk_marks_a_neighbour_only_where_its_border_moved() {
        // A re-send is byte-identical while nobody has edited the chunk, because
        // generation is deterministic — and that case has to stay free, or every view
        // update pays for six remeshes per chunk it repeats. The case that must not be
        // missed is the re-send that genuinely differs, which is what the server composes
        // once somebody has dug into the chunk.
        let mut store = store_with_stone_at(coord(0, 0, 0));
        store.insert(coord(1, 0, 0), stone());
        store.take_changes();

        store.insert(coord(1, 0, 0), stone());
        assert_eq!(
            store.take_changes(),
            vec![ChunkChange::Loaded(coord(1, 0, 0))],
            "an identical re-send remeshed a neighbour for nothing"
        );

        // A hole on the far wall first, which no chunk this session holds is looking at.
        let mut dug_away = stone();
        dug_away.set(SIZE - 1, 5, 5, palette::AIR);
        store.insert(coord(1, 0, 0), dug_away.clone());
        assert_eq!(
            store.take_changes(),
            vec![ChunkChange::Loaded(coord(1, 0, 0))],
            "a border nobody is meshed against pulled a neighbour in with it"
        );

        // Then one in the wall the two chunks share: local x = 0 of the chunk to the east
        // is the layer its western neighbour looks at. Dug from the revision before it, so
        // the far wall is the only other thing that could have moved, and it has not.
        let mut dug = dug_away;
        dug.set(0, 5, 5, palette::AIR);
        store.insert(coord(1, 0, 0), dug);
        assert_eq!(
            store.take_changes(),
            vec![
                ChunkChange::Loaded(coord(1, 0, 0)),
                ChunkChange::NeighbourChanged(coord(0, 0, 0)),
            ]
        );
    }

    #[test]
    fn a_re_sent_water_border_compares_effective_geometry_not_ids() {
        let mut store = store_with_stone_at(coord(0, 0, 0));
        let mut source = air();
        source.set(0, 5, 5, palette::WATER);
        store.insert(coord(1, 0, 0), source);
        store.take_changes();

        let mut lower = air();
        lower.set(0, 5, 5, palette::WATER_FLOW3);
        store.insert(coord(1, 0, 0), lower);
        assert!(
            store
                .take_changes()
                .contains(&ChunkChange::NeighbourChanged(coord(0, 0, 0)))
        );
    }

    #[test]
    fn a_vertical_re_sent_water_border_reads_presence_not_level() {
        let mut flow3 = air();
        flow3.set(5, 0, 5, palette::WATER_FLOW3);
        let mut flow4 = flow3.clone();
        flow4.set(5, 0, 5, palette::WATER_FLOW4);
        let differs = |after| border_layer_differs(Some(&flow3), Some(after), 1, false, None);
        assert!(!differs(&flow4));
        assert!(differs(&air()));
    }

    #[test]
    fn a_mismatched_above_chunk_is_air_for_border_geometry() {
        let mut below = air();
        below.set(SIZE - 1, SIZE - 1, SIZE - 1, palette::WATER_FLOW3);
        let one_block_above = VoxelChunk::from_runs(&[palette::WATER, 1], 1).expect("valid");

        let geometry = border_geometry(
            Some(&below),
            [SIZE - 1, SIZE - 1, SIZE - 1],
            Some(&one_block_above),
        );
        assert_eq!(geometry.water_level, 3);
    }

    #[test]
    fn water_above_a_horizontal_neighbour_invalidates_the_diagonal_mesh() {
        let west = coord(0, 0, 0);
        let east = coord(1, 0, 0);
        let above_east = coord(1, 1, 0);
        let mut store = ChunkStore::default();
        store.insert(west, air());
        let mut east_chunk = air();
        east_chunk.set(0, SIZE - 1, 5, palette::WATER_FLOW3);
        store.insert(east, east_chunk);
        store.take_changes();

        let mut above = air();
        above.set(0, 0, 5, palette::WATER_FLOW1);
        store.insert(above_east, above);
        let changes = store.take_changes();
        assert!(changes.contains(&ChunkChange::NeighbourChanged(east)));
        assert!(changes.contains(&ChunkChange::NeighbourChanged(west)));

        assert_eq!(
            store.apply_block(at(32, 32, 5), palette::AIR, SIZE),
            BlockApplied::Rewritten {
                coord: above_east,
                remeshed: 4,
            }
        );
        assert!(
            store
                .take_changes()
                .contains(&ChunkChange::NeighbourChanged(west))
        );
    }

    #[test]
    fn an_unloaded_chunk_marks_the_neighbours_that_were_hiding_behind_it() {
        // The direction that is a hole rather than a waste if it is missed: the survivor
        // has to draw the wall it had been culling, or the edge of the streamed volume is
        // see-through from outside.
        let mut store = store_with_stone_at(coord(0, 0, 0));
        store.insert(coord(1, 0, 0), stone());
        store.take_changes();

        store.unload(coord(1, 0, 0));

        assert_eq!(
            store.take_changes(),
            vec![
                ChunkChange::Unloaded(coord(1, 0, 0)),
                ChunkChange::NeighbourChanged(coord(0, 0, 0)),
            ]
        );
    }

    #[test]
    fn unloading_a_chunk_of_air_marks_nobody() {
        // The control for the rule above, and the sky again: nothing was culled against a
        // chunk of air, so nothing has to be redrawn when it goes.
        let mut store = store_with_stone_at(coord(0, 0, 0));
        store.insert(coord(1, 0, 0), air());
        store.take_changes();

        store.unload(coord(1, 0, 0));

        assert_eq!(
            store.take_changes(),
            vec![ChunkChange::Unloaded(coord(1, 0, 0))]
        );
    }

    #[test]
    fn unloading_a_chunk_that_was_never_held_marks_nobody() {
        // An unload is logged whether or not the chunk was there — the renderer has
        // bookkeeping to clear either way — but a coordinate nothing was stored under was
        // already being meshed against as air.
        let mut store = store_with_stone_at(coord(0, 0, 0));

        store.unload(coord(1, 0, 0));

        assert_eq!(
            store.take_changes(),
            vec![ChunkChange::Unloaded(coord(1, 0, 0))]
        );
    }

    #[test]
    fn an_edit_marks_the_neighbour_from_the_voxel_and_not_from_the_layer() {
        // Two mechanisms answer the same question, and this is the seam between them. An
        // edit knows the voxel that moved, so `border_neighbours` names the chunks sharing
        // a face with it in O(1); a payload off the wire could have moved any of the six
        // layers, so `insert` compares them. Both are precise, and the edit path must not
        // pick up the payload path's scan — an edit in the middle of a chunk would then
        // remesh all six neighbours for nothing.
        let mut store = store_with_stone_at(coord(0, 0, 0));
        for at in [coord(1, 0, 0), coord(0, 1, 0), coord(0, 0, 1)] {
            store.insert(at, stone());
        }
        store.take_changes();

        store.apply_block(at(10, 11, 12), palette::AIR, SIZE);

        assert_eq!(
            store.take_changes(),
            vec![ChunkChange::Loaded(coord(0, 0, 0))],
            "an edit in the middle of a chunk pulled its neighbours in"
        );
    }

    #[test]
    fn an_edit_replaces_the_chunk_rather_than_mutating_the_one_in_place() {
        // What `render.rs`'s staleness guard rests on. It decides whether a finished mesh
        // is still current with `Arc::ptr_eq`, so a revision that reused its predecessor's
        // allocation would make a mesh built *before* the edit compare equal to the chunk
        // *after* it — and the hole the player dug would be drawn as still filled in.
        let mut store = store_with_stone_at(coord(0, 0, 0));
        let before = Arc::clone(store.get(coord(0, 0, 0)).expect("held"));

        store.apply_block(at(10, 10, 10), palette::AIR, SIZE);
        let after = store.get(coord(0, 0, 0)).expect("still held");

        assert!(
            !Arc::ptr_eq(&before, after),
            "the edit mutated the revision a mesh may already have been built from"
        );
        assert_eq!(
            before.block([10, 10, 10]),
            palette::STONE,
            "and the older revision still describes the world it was meshed from"
        );
        assert_eq!(after.block([10, 10, 10]), palette::AIR);
    }

    #[test]
    fn an_edit_in_a_chunk_this_session_does_not_hold_is_dropped() {
        // Permitted by `schemas/world.fbs`, which lets a client either buffer such an
        // update or drop it and trust the `ChunkData` to arrive already edited. Nothing is
        // logged for the renderer, because nothing became stale.
        let mut store = store_with_stone_at(coord(0, 0, 0));

        assert_eq!(
            store.apply_block(at(1000, 0, 0), palette::AIR, SIZE),
            BlockApplied::Unheld {
                coord: coord(31, 0, 0)
            }
        );
        assert!(store.take_changes().is_empty());
    }

    #[test]
    fn solid_at_answers_for_the_voxels_the_session_holds_and_no_others() {
        // The question the raycast asks. A voxel in a chunk that has not arrived is not
        // solid, and that is the honest answer rather than a cautious one: this client
        // knows nothing about it, and the server refuses an edit inside a chunk it never
        // sent anyway.
        let mut store = store_with_stone_at(coord(0, 0, 0));
        store.apply_block(at(5, 5, 5), palette::AIR, SIZE);

        assert!(store.solid_at(at(6, 5, 5), SIZE), "stone is solid");
        assert!(
            !store.solid_at(at(5, 5, 5), SIZE),
            "the edited voxel is air"
        );
        assert!(
            !store.solid_at(at(-1, 0, 0), SIZE),
            "a chunk this session does not hold contains nothing it knows about"
        );
        assert!(
            !store.solid_at(at(0, 0, 0), 0),
            "and an impossible chunk edge is not a panic"
        );
    }

    #[test]
    fn a_column_query_skips_unloaded_chunks_and_returns_the_highest_resident_solid() {
        let mut lower = air();
        lower.set(4, 3, 7, palette::STONE);
        let mut upper = air();
        upper.set(4, 2, 7, palette::STONE);
        let mut store = ChunkStore::default();
        store.insert(coord(0, 0, 0), lower);
        // cy=1 is deliberately absent. A missing streamed chunk is open sky, not a
        // lookup failure and not a voxel loop through an all-air substitute.
        store.insert(coord(0, 2, 0), upper);

        assert_eq!(store.highest_solid_y(4, 7, 0, 96, SIZE), Some(66));
        assert_eq!(
            store.highest_solid_y(4, 7, 4, 64, SIZE),
            None,
            "the lower roof was outside the requested half-open range"
        );

        store.unload(coord(0, 2, 0));
        assert_eq!(
            store.highest_solid_y(4, 7, 0, 96, SIZE),
            Some(3),
            "an unloaded upper chunk left a stale roof behind"
        );
        assert_eq!(store.highest_solid_y(4, 7, 0, 96, 0), None);
    }

    #[test]
    fn block_at_names_the_id_and_answers_air_for_everything_it_cannot_reach() {
        // The one place a world coordinate becomes a block id. Every "what is here"
        // question in the client resolves through it, so its three refusals are the three
        // that matter: no chunk, no coordinate, and a chunk edge that cannot exist.
        let mut store = store_with_stone_at(coord(0, 0, 0));
        store.apply_block(at(5, 5, 5), palette::WATER, SIZE);

        assert_eq!(store.block_at(at(6, 5, 5), SIZE), palette::STONE);
        assert_eq!(store.block_at(at(5, 5, 5), SIZE), palette::WATER);
        assert_eq!(
            store.block_at(at(-1, 0, 0), SIZE),
            palette::AIR,
            "a chunk this session does not hold contains nothing it knows about"
        );
        assert_eq!(
            store.block_at(at(0, 0, 0), 0),
            palette::AIR,
            "and an impossible chunk edge is not a panic"
        );
    }

    #[test]
    fn a_flower_is_targetable_without_being_solid_and_water_is_neither() {
        // The one predicate #551 adds, and the three answers that separate it from the
        // two it sits beside. A flower is aimed at and walked through; water is walked
        // through and looked through; stone is both.
        let mut store = store_with_stone_at(coord(0, 0, 0));
        store.apply_block(at(5, 5, 5), palette::FLOWER_RED, SIZE);
        store.apply_block(at(5, 6, 5), palette::WATER, SIZE);
        store.apply_block(at(5, 7, 5), palette::AIR, SIZE);

        assert!(!store.solid_at(at(5, 5, 5), SIZE), "a flower stops no body");
        assert!(
            store.targetable_at(at(5, 5, 5), SIZE),
            "and is still a thing the crosshair finds"
        );
        assert!(
            !store.targetable_at(at(5, 6, 5), SIZE),
            "water is looked through, which is what #446 decided"
        );
        assert!(!store.targetable_at(at(5, 7, 5), SIZE), "air is nothing");
        assert!(
            store.solid_at(at(6, 5, 5), SIZE) && store.targetable_at(at(6, 5, 5), SIZE),
            "stone is both"
        );
        assert!(
            !store.targetable_at(at(-1, 0, 0), SIZE),
            "a chunk this session does not hold offers nothing to aim at"
        );
    }

    #[test]
    fn water_is_not_solid_so_the_ray_goes_through_it() {
        // #446 made water a block a body moves through, and this is the client half of
        // it: the voxel is held, it is not air, and it still stops nothing. The lake bed
        // behind it is what the aiming ray reports.
        let mut store = store_with_stone_at(coord(0, 0, 0));
        store.apply_block(at(5, 5, 5), palette::WATER, SIZE);
        store.apply_block(at(5, 6, 5), palette::ICE, SIZE);

        assert_eq!(store.block_at(at(5, 5, 5), SIZE), palette::WATER);
        assert!(!store.solid_at(at(5, 5, 5), SIZE), "water stops nothing");
        assert!(
            store.solid_at(at(5, 6, 5), SIZE),
            "ice is ordinary ground and stops everything"
        );
    }

    // -----------------------------------------------------------------
    // The per-frame expansion budget
    // -----------------------------------------------------------------

    /// How many chunks past a whole number of budgets the burst test sends, so the
    /// last frame is a partial one and cannot be confused with a full budget.
    const REMAINDER: usize = 5;

    #[test]
    fn a_burst_of_chunks_is_expanded_over_frames_rather_than_all_at_once() {
        // The asymmetry this closes. Meshing has had a per-frame cap since it existed;
        // expansion had none, so a join expanded the entire view distance — 729 chunks
        // at view distance 4 — in as few frames as the inbox allowed, and every
        // millisecond of it landed on the frame being built.
        //
        // The property asserted is a bounded amount of *work* per frame, and that is why
        // `ingest_app` hands the drain `Duration::MAX`: since #629 there is a wall-clock
        // bound in the same loop, and an exact expansion count under it would measure the
        // runner rather than the code — passing or failing on who else was using the
        // machine. The reasoning did not change when the second bound arrived; it is the
        // reason the second bound is switched off here and pinned on its own below.
        const BURST: usize = MAX_DECODES_PER_FRAME * 3 + REMAINDER;

        let mut app = ingest_app(true);
        for cx in 0..BURST {
            push(
                &mut app,
                WorldUpdate::Chunk {
                    coord: coord(cx as i32, 0, 0),
                    runs: stone_runs(),
                },
            );
        }

        // How many chunks became voxels on each of the frames it took to drain the
        // burst. Deterministic: no task pool is involved, and the budget is a count.
        let mut expanded_per_frame = Vec::new();
        let mut held = 0;
        for _ in 0..4 {
            app.update();
            let now = chunk_count(&app);
            expanded_per_frame.push(now - held);
            held = now;
        }

        assert_eq!(
            expanded_per_frame,
            vec![
                MAX_DECODES_PER_FRAME,
                MAX_DECODES_PER_FRAME,
                MAX_DECODES_PER_FRAME,
                REMAINDER
            ],
            "one budget per frame until the burst runs out"
        );
        assert_eq!(held, BURST, "and every chunk arrived");
        assert_eq!(backlog(&app), 0, "with nothing left waiting");
    }

    #[test]
    fn the_backlog_is_visible_while_it_lasts() {
        // The counter the overlay reports. Before expansion was capped there was
        // nothing to report — the backlog never outlived the frame it arrived in — so
        // this is the number that makes a regression visible at all.
        const BURST: usize = MAX_DECODES_PER_FRAME + REMAINDER;

        let mut app = ingest_app(true);
        for cx in 0..BURST {
            push(
                &mut app,
                WorldUpdate::Chunk {
                    coord: coord(cx as i32, 0, 0),
                    runs: stone_runs(),
                },
            );
        }
        assert_eq!(backlog(&app), 0, "nothing has been ingested yet");

        app.update();
        assert_eq!(backlog(&app), REMAINDER, "what the budget did not reach");

        app.update();
        assert_eq!(backlog(&app), 0);
    }

    #[test]
    fn a_frame_with_no_time_left_still_expands_one_chunk() {
        // The progress guarantee, at the only value where it can be asserted exactly.
        // `Duration::ZERO` is a budget that is spent before the first update is popped,
        // so this is the worst frame the meter can produce — and it must still be a
        // frame that moves the queue forward, because a budget that can stall streaming
        // is not a budget, it is a deadlock waiting for a slow machine.
        //
        // The placement of the check is what makes it true: both bounds are read at the
        // *bottom* of the loop body, after an update has been popped and applied.
        const BURST: usize = 4;

        let mut app = metered_ingest_app(Duration::ZERO);
        for cx in 0..BURST {
            push(&mut app, payload_at(coord(cx as i32, 0, 0)));
        }

        for expanded in 1..=BURST {
            app.update();
            assert_eq!(
                chunk_count(&app),
                expanded,
                "a frame with no time left expanded neither more nor less than one chunk"
            );
            assert_eq!(backlog(&app), BURST - expanded, "and the rest still waits");
        }

        assert_eq!(refused(&app), 0, "nothing was turned away");
        assert_eq!(evicted(&app), 0, "and nothing was dropped from the store");
    }

    #[test]
    fn an_exhausted_time_budget_still_lets_an_unload_through() {
        // The other kind of update, at the same worst frame. An unload costs no *count*
        // budget deliberately — metering a map removal would let a burst of them defer
        // the loads behind it — and it is inside the *time* slice like everything else.
        // What must not happen either way is that a frame takes nothing off the queue.
        let mut app = metered_ingest_app(Duration::ZERO);
        push(&mut app, payload_at(coord(0, 0, 0)));
        push(
            &mut app,
            WorldUpdate::Unload {
                coord: coord(0, 0, 0),
            },
        );

        app.update();
        assert_eq!(chunk_count(&app), 1, "the chunk landed on the first frame");
        assert_eq!(backlog(&app), 1, "and the unload waited for the second");

        app.update();
        assert_eq!(
            chunk_count(&app),
            0,
            "which is where it took the chunk away"
        );
        assert_eq!(backlog(&app), 0);
    }

    #[test]
    fn a_burst_drained_a_chunk_a_frame_loses_nothing_and_refuses_nothing() {
        // The conservation law under the slowest drain the meter allows. Every chunk of
        // the burst has to arrive, in the server's order, with `decode_refused` and
        // `decode_evicted` still at zero — a burst far under `MAX_DECODE_BACKLOG` is not
        // something the bound may touch however few of them a frame gets through, and a
        // slow frame turning terrain away would be this fix causing the hole it exists
        // to prevent.
        const BURST: usize = MAX_DECODES_PER_FRAME * 2;

        let mut app = metered_ingest_app(Duration::ZERO);
        for cx in 0..BURST {
            push(&mut app, payload_at(coord(cx as i32, 0, 0)));
        }

        // The law is read after each frame rather than before the first: until one has
        // run, the burst is still in the inbox and neither term of it has been reached.
        for frame in 1..=BURST {
            app.update();
            assert_eq!(
                backlog(&app) + chunk_count(&app),
                BURST,
                "every chunk is waiting or held after frame {frame}; none may vanish"
            );
        }

        assert_eq!(chunk_count(&app), BURST, "every chunk arrived");
        assert_eq!(backlog(&app), 0, "with nothing left waiting");
        assert_eq!(refused(&app), 0);
        assert_eq!(evicted(&app), 0);

        let store = app.world().resource::<ChunkStore>();
        for cx in 0..BURST {
            assert!(
                store.get(coord(cx as i32, 0, 0)).is_some(),
                "chunk {cx} of the burst never reached the store"
            );
        }
    }

    #[test]
    fn the_count_is_the_ceiling_and_the_slice_is_the_rule() {
        // The two bounds, and which of them binds. With no time limit the count is
        // reached exactly; with no time at all the count is never reached. Neither end
        // is a tuning number, which is what makes both exact — the shipping value sits
        // between them and is justified where it is declared, not here.
        const BURST: usize = MAX_DECODES_PER_FRAME * 2;

        let mut unmetered = ingest_app(true);
        let mut metered = metered_ingest_app(Duration::ZERO);
        for cx in 0..BURST {
            push(&mut unmetered, payload_at(coord(cx as i32, 0, 0)));
            push(&mut metered, payload_at(coord(cx as i32, 0, 0)));
        }

        unmetered.update();
        metered.update();

        assert_eq!(
            chunk_count(&unmetered),
            MAX_DECODES_PER_FRAME,
            "the count is what stops a frame that has time"
        );
        assert_eq!(
            chunk_count(&metered),
            1,
            "and the slice is what stops one that does not"
        );
    }

    #[test]
    fn the_shipping_budget_is_two_milliseconds() {
        // The number itself, pinned where moving it has to be deliberate — the same job
        // `the_bound_is_one_join_at_view_distance_eight` does for the backlog. What it
        // is measured from is in the constant's own comment, and the measurements are
        // re-derivable: `cargo test --release -- --ignored --nocapture measure_`.
        //
        // The `Default` is what `WorldPlugin` installs — `render.rs` pins that half,
        // where an app carrying the whole plugin stack already exists — so a client
        // built from this tree runs metered rather than with whatever a test left behind.
        assert_eq!(MAX_DECODE_TIME_PER_FRAME, Duration::from_millis(2));
        assert_eq!(DecodeTimeBudget::default().0, MAX_DECODE_TIME_PER_FRAME);
    }

    #[test]
    fn the_backlog_keeps_the_servers_order_across_a_frame_boundary() {
        // Why the backlog is one ordered queue and not a set of coordinates: the server
        // unloads a chunk before it re-sends it, and an unload that overtook the loads
        // queued ahead of it would delete a chunk this session can see. Here the unload
        // and the re-send sit past the first frame's budget, so they are applied a frame
        // later — and still in the order the server chose.
        let mut app = ingest_app(true);
        for cx in 0..MAX_DECODES_PER_FRAME {
            push(
                &mut app,
                WorldUpdate::Chunk {
                    coord: coord(cx as i32, 0, 0),
                    runs: stone_runs(),
                },
            );
        }
        push(
            &mut app,
            WorldUpdate::Unload {
                coord: coord(0, 0, 0),
            },
        );
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(0, 0, 0),
                runs: air_runs(),
            },
        );

        app.update();
        assert_eq!(chunk_count(&app), MAX_DECODES_PER_FRAME);
        assert_eq!(
            backlog(&app),
            2,
            "the unload and the re-send are still waiting"
        );
        assert_eq!(
            app.world()
                .resource::<ChunkStore>()
                .get(coord(0, 0, 0))
                .expect("loaded by the first frame")
                .block([0, 0, 0]),
            palette::STONE,
            "nothing behind the budget may be applied early"
        );

        app.update();
        let store = app.world().resource::<ChunkStore>();
        assert_eq!(
            store.chunk_count(),
            MAX_DECODES_PER_FRAME,
            "unloaded and re-sent, not lost"
        );
        assert_eq!(
            store
                .get(coord(0, 0, 0))
                .expect("re-sent by the second frame")
                .block([0, 0, 0]),
            palette::AIR,
            "the re-send is the last word, as the server sent it"
        );

        // Filtered to the coordinate this test is about rather than sliced off the end
        // of the log: the unload also tells the chunk east of it that the wall the two
        // shared is gone, so the pair is no longer adjacent. What is asserted has not
        // moved — the unload reaches the renderer before the re-send.
        let changes = app.world_mut().resource_mut::<ChunkStore>().take_changes();
        let about_the_re_send: Vec<ChunkChange> = changes
            .into_iter()
            .filter(|change| {
                matches!(
                    change,
                    ChunkChange::Unloaded(c) | ChunkChange::Loaded(c) if *c == coord(0, 0, 0)
                )
            })
            .collect();
        assert_eq!(
            &about_the_re_send[about_the_re_send.len() - 2..],
            &[
                ChunkChange::Unloaded(coord(0, 0, 0)),
                ChunkChange::Loaded(coord(0, 0, 0)),
            ],
            "and the renderer is told about them in that order too"
        );
    }

    #[test]
    fn an_unload_does_not_spend_the_expansion_budget() {
        // An unload is a map removal and a log entry. Metering it would let a burst of
        // unloads — which is what leaving a chunk behind produces, every one of them —
        // defer the loads queued behind them for no reason.
        let mut app = ingest_app(true);
        for cx in 0..100 {
            push(
                &mut app,
                WorldUpdate::Unload {
                    coord: coord(cx, 0, 0),
                },
            );
        }
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(0, 0, 0),
                runs: stone_runs(),
            },
        );
        app.update();

        assert_eq!(backlog(&app), 0, "a hundred unloads cost no budget");
        assert_eq!(
            chunk_count(&app),
            1,
            "and the load behind them still landed"
        );
    }

    // -----------------------------------------------------------------
    // Block updates through the ingest
    // -----------------------------------------------------------------

    /// A block edit as the net thread delivers it.
    fn edit(pos: BlockCoord, block: BlockId) -> WorldUpdate {
        WorldUpdate::Block {
            pos,
            block_id: block,
        }
    }

    #[test]
    fn a_block_update_edits_the_chunk_that_arrived_before_it() {
        let mut app = ingest_app(true);
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(0, 0, 0),
                runs: stone_runs(),
            },
        );
        push(&mut app, edit(at(1, 2, 3), palette::AIR));
        app.update();

        let store = app.world().resource::<ChunkStore>();
        assert_eq!(
            store.get(coord(0, 0, 0)).expect("held").block([1, 2, 3]),
            palette::AIR
        );
    }

    #[test]
    fn a_block_update_that_arrives_before_its_chunk_is_dropped_rather_than_kept() {
        // The queue is ordered, so this is the server describing an edit in a chunk this
        // session has not been sent — not an edit that overtook its own chunk. Buffering it
        // would mean holding edits for chunks that may never arrive; the server invalidates
        // the chunk's cached payload on every edit, so the copy that does arrive already has
        // it.
        let mut app = ingest_app(true);
        push(&mut app, edit(at(1, 2, 3), palette::AIR));
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(0, 0, 0),
                runs: stone_runs(),
            },
        );
        app.update();

        let store = app.world().resource::<ChunkStore>();
        assert_eq!(
            store.get(coord(0, 0, 0)).expect("held").block([1, 2, 3]),
            palette::STONE,
            "the edit was replayed onto a chunk that arrived after it"
        );
        assert_eq!(backlog(&app), 0, "and it is not still waiting");
    }

    #[test]
    fn a_block_update_behind_an_unload_does_not_bring_the_chunk_back() {
        // Why the edit shares the one ordered queue rather than a buffer of its own. The
        // server unloads a chunk and then describes an edit somebody else made in it — a
        // consumer that saw the two through separate buffers could apply them backwards and
        // end up holding a chunk this session cannot see.
        let mut app = ingest_app(true);
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(0, 0, 0),
                runs: stone_runs(),
            },
        );
        push(
            &mut app,
            WorldUpdate::Unload {
                coord: coord(0, 0, 0),
            },
        );
        push(&mut app, edit(at(1, 2, 3), palette::AIR));
        app.update();

        assert_eq!(
            chunk_count(&app),
            0,
            "the edit resurrected an unloaded chunk"
        );
    }

    #[test]
    fn a_burst_of_block_updates_is_spread_over_frames_like_an_expansion() {
        // An edited chunk is a fresh `size³` allocation and a copy into it, which is the
        // same main-schedule work an expansion is — so it is metered the same way. A click
        // is nowhere near the cap; a broadcast of everybody's edits is what this bounds.
        const BURST: usize = MAX_DECODES_PER_FRAME + REMAINDER;

        let mut app = ingest_app(true);
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(0, 0, 0),
                runs: stone_runs(),
            },
        );
        // All inside the one chunk, so every one of them really rewrites it.
        let edited: Vec<BlockCoord> = (0..BURST)
            .map(|i| at((i % SIZE) as i32, (i / SIZE) as i32, 0))
            .collect();
        for pos in &edited {
            push(&mut app, edit(*pos, palette::AIR));
        }

        app.update();
        assert_eq!(
            backlog(&app),
            BURST - (MAX_DECODES_PER_FRAME - 1),
            "the chunk spent one unit of the budget and the edits spent the rest"
        );

        app.update();
        assert_eq!(backlog(&app), 0);

        let store = app.world().resource::<ChunkStore>();
        let chunk = store.get(coord(0, 0, 0)).expect("held");
        for pos in &edited {
            assert_eq!(
                chunk.block([pos.x as usize, pos.y as usize, pos.z as usize]),
                palette::AIR,
                "edit at {pos:?} was lost"
            );
        }
    }

    #[test]
    fn an_edit_for_a_chunk_that_is_not_held_costs_no_budget() {
        // A map lookup is not the work being metered — the same reason an unload is free.
        // A server describing edits in chunks this session cannot see must not be able to
        // defer the chunks queued behind them.
        let mut app = ingest_app(true);
        for i in 0..100 {
            push(&mut app, edit(at(1000 + i, 0, 0), palette::AIR));
        }
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(0, 0, 0),
                runs: stone_runs(),
            },
        );
        app.update();

        assert_eq!(backlog(&app), 0, "a hundred unheld edits cost no budget");
        assert_eq!(
            chunk_count(&app),
            1,
            "and the load behind them still landed"
        );
    }

    /// Records what a consumer with change detection would have seen of the backlog,
    /// one entry per frame.
    #[derive(Resource, Default)]
    struct BacklogChanges(Vec<bool>);

    fn log_backlog_changes(queue: Res<DecodeQueue>, mut log: ResMut<BacklogChanges>) {
        log.0.push(queue.is_changed());
    }

    #[test]
    fn an_idle_frame_does_not_mark_the_backlog_changed() {
        // The same rule that `an_idle_frame_marks_neither_the_stats_nor_the_store_changed`
        // holds render.rs to, applied to the resource this budget added: `ResMut` marks a
        // resource changed on every `DerefMut`, so an ingest that popped or extended
        // unconditionally would leave the backlog permanently "changed" — and every
        // counter on the overlay hangs off that class of signal.
        //
        // Observed from inside a system, because `App::update()` ends each frame with
        // `World::clear_trackers()`; an `is_changed()` check from outside is always false
        // and would pass whatever the ingest did.
        let mut app = ingest_app(true);
        app.init_resource::<BacklogChanges>()
            .add_systems(Update, log_backlog_changes.after(ingest_world_updates));

        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(0, 0, 0),
                runs: stone_runs(),
            },
        );
        app.update();
        assert_eq!(
            app.world().resource::<BacklogChanges>().0,
            vec![true],
            "the frame that queued a chunk did change the backlog"
        );

        app.world_mut().resource_mut::<BacklogChanges>().0.clear();
        for _ in 0..6 {
            app.update();
        }
        assert_eq!(
            app.world().resource::<BacklogChanges>().0,
            vec![false; 6],
            "an idle frame touched the backlog"
        );
    }

    // -----------------------------------------------------------------
    // The bound on the backlog
    // -----------------------------------------------------------------

    /// The largest burst the *contract* permits: a join at the `view_distance` ceiling
    /// in `schemas/handshake.fbs`, `(2 * 16 + 1)³` chunks.
    ///
    /// The flood the overflow test uses, and it comes from the schema rather than from
    /// [`MAX_DECODE_BACKLOG`] deliberately. A test that sizes its input from the bound
    /// it is checking passes for *every* value of that bound, which is the vacuity this
    /// avoids: raise the bound and this input does not move with it.
    const LARGEST_JOIN_THE_CONTRACT_PERMITS: usize = 33 * 33 * 33;

    /// The most frames of decode budget the bound may leave the client waiting.
    ///
    /// The promise in the units the user story asks for — falling behind costs latency,
    /// and this is how much of it: 154 frames of [`MAX_DECODES_PER_FRAME`] is about
    /// 2.6 s at 60 Hz. Written out rather than derived from [`MAX_DECODE_BACKLOG`], for
    /// the same reason as above. It is the assertion that fails when the bound is
    /// raised.
    const MAX_BACKLOG_FRAMES: usize = 154;

    /// How far past the bound the kind-selectivity test pushes.
    ///
    /// Small, and deliberately not a multiple of [`MAX_DECODES_PER_FRAME`], so a
    /// refusal count cannot be mistaken for a budget that happened to match.
    const EXCESS: usize = 7;

    /// How many frames the backlog would take to drain at the current budget.
    fn frames_of_backlog(app: &App) -> usize {
        backlog(app).div_ceil(MAX_DECODES_PER_FRAME)
    }

    /// One chunk payload, always for the same coordinate.
    ///
    /// The coordinate is irrelevant to a queue that holds *updates*, and a distinct one
    /// per payload would be a `SIZE³` allocation each in the store — a memory test
    /// wearing a bound test's name.
    fn one_payload() -> WorldUpdate {
        WorldUpdate::Chunk {
            coord: coord(0, 0, 0),
            runs: stone_runs(),
        }
    }

    /// The same payload, for a coordinate the caller picks.
    ///
    /// Needed only where a test has to tell two chunks apart — an eviction is scoped to
    /// one coordinate, and a flood that is all the same coordinate cannot show that.
    fn payload_at(coord: ChunkCoord) -> WorldUpdate {
        WorldUpdate::Chunk {
            coord,
            runs: stone_runs(),
        }
    }

    #[test]
    fn the_bound_is_one_join_at_view_distance_eight() {
        // The number itself, pinned where moving it has to be deliberate — the same job
        // `net::frame`'s `limits_match_the_server` does for `MAX_FRAME_SIZE`. The derivation
        // is in `MAX_DECODE_BACKLOG`'s own comment; what is asserted here is that the
        // arithmetic still lands where the comment says it does.
        assert_eq!(MAX_DECODE_BACKLOG, 17 * 17 * 17);
        assert_eq!(MAX_DECODE_BACKLOG, 4_913);

        // And what the client is buying with it, in the units the user story uses: a
        // full backlog is 154 frames of decode budget, about 2.6 s at 60 Hz.
        assert_eq!(
            MAX_DECODE_BACKLOG.div_ceil(MAX_DECODES_PER_FRAME),
            MAX_BACKLOG_FRAMES
        );
    }

    #[test]
    fn a_session_that_ends_takes_its_refusal_count_with_it() {
        // `refused` is a per-session total — its own doc says so, and so does the log
        // that reports it once the backlog drains. The branch that ends an episode
        // already clears the queue and the warning latch; the count belongs with them.
        //
        // Constructed rather than reached. `Session` is inserted once and never removed
        // today, so what this pins is the invariant for the reconnection that will
        // remove it, not a state a running client can be in.
        let mut app = ingest_app(true);
        for _ in 0..LARGEST_JOIN_THE_CONTRACT_PERMITS {
            push(&mut app, one_payload());
        }
        app.update();
        assert!(
            refused(&app) > 0,
            "nothing was refused, so there is nothing for a next session to inherit"
        );

        app.world_mut().remove_resource::<Session>();
        push(&mut app, one_payload());
        app.update();

        assert_eq!(
            refused(&app),
            0,
            "the next session opens holding the last one's refusals"
        );
        assert_eq!(backlog(&app), 0, "and a backlog it could no longer decode");
    }

    #[test]
    fn the_backlog_stops_growing_at_the_bound() {
        // The failure this closes: the inbox is drained in full every frame while only a
        // budget of the backlog is expanded, so a server that keeps sending grew the
        // queue for as long as it kept sending. There was no ceiling on it at all.
        //
        // The flood is the biggest join the contract permits, not a multiple of the
        // bound, and what is asserted is the *wait* rather than the count — so raising
        // the bound does not raise the assertion with it. That is the non-vacuity: at
        // view distance 9 the backlog is 214 frames deep and this test fails.
        let mut app = ingest_app(true);
        for _ in 0..LARGEST_JOIN_THE_CONTRACT_PERMITS {
            push(&mut app, one_payload());
        }
        app.update();

        assert!(
            refused(&app) > 0,
            "the whole flood was absorbed, so nothing bounded it"
        );
        assert!(
            frames_of_backlog(&app) <= MAX_BACKLOG_FRAMES,
            "{} frames of backlog, past the {MAX_BACKLOG_FRAMES} the bound promises",
            frames_of_backlog(&app)
        );
        assert_eq!(
            backlog(&app),
            MAX_DECODE_BACKLOG - MAX_DECODES_PER_FRAME,
            "the bound was reached, then one frame's budget came off it"
        );

        // And it holds for as long as arrival outruns the budget. Twice the drain rate
        // per frame is the shape of the failure: before the bound, this loop added
        // `MAX_DECODES_PER_FRAME` to the queue every frame, without end.
        let refused_after_the_join = refused(&app);
        for frame in 0..8 {
            for _ in 0..MAX_DECODES_PER_FRAME * 2 {
                push(&mut app, one_payload());
            }
            app.update();
            assert!(
                frames_of_backlog(&app) <= MAX_BACKLOG_FRAMES,
                "frame {frame}: {} frames of backlog, past the {MAX_BACKLOG_FRAMES} \
                 the bound promises",
                frames_of_backlog(&app)
            );
        }
        assert!(
            refused(&app) > refused_after_the_join,
            "and it kept refusing, and counting, for as long as the flood lasted"
        );
    }

    #[test]
    fn nothing_is_admitted_over_the_bound() {
        // **This assertion was inverted by the review on this PR, and the inversion is
        // the finding.** It used to read `an_unload_and_a_block_are_admitted_over_the
        // _bound`, on the reasoning that refusing either buys the bound with a chunk
        // that never unloads or a voxel that stays wrong. That reasoning was right
        // about refusing and wrong about the conclusion: there is a third answer, and
        // evicting the coordinate is it. What the old test pinned was a queue that
        // could still be grown without limit by a server sending nothing but edits for
        // held chunks — the hole the issue's own title names.
        let mut app = ingest_app(true);
        for _ in 0..MAX_DECODE_BACKLOG {
            push(&mut app, one_payload());
        }
        // Past the bound now, and all in the same frame so the queue is already full
        // when each of the three is offered to it.
        for _ in 0..EXCESS {
            push(&mut app, one_payload());
        }
        push(
            &mut app,
            WorldUpdate::Unload {
                coord: coord(4, 0, 0),
            },
        );
        push(&mut app, edit(at(0, 0, 0), palette::AIR));
        app.update();

        assert!(
            backlog(&app) <= MAX_DECODE_BACKLOG,
            "{} waiting, past the {MAX_DECODE_BACKLOG} the bound promises",
            backlog(&app)
        );
        assert_eq!(
            refused(&app),
            EXCESS + 1,
            "the excess payloads and the edit; an unload is applied, not refused"
        );
    }

    #[test]
    fn an_edit_at_the_bound_evicts_the_chunk_it_could_not_be_applied_to() {
        // The whole argument for evicting rather than dropping: a chunk kept while its
        // edits are refused is wrong in as many places as were refused, and nothing
        // records which. Absent, it is wrong nowhere, and the copy the server composes
        // next already has every edit in it.
        let mut app = ingest_app(true);
        push(&mut app, one_payload());
        app.update();
        assert_eq!(chunk_count(&app), 1, "the chunk under test is held");

        for _ in 0..MAX_DECODE_BACKLOG {
            push(&mut app, payload_at(coord(9, 9, 9)));
        }
        push(&mut app, edit(at(0, 0, 0), palette::AIR));
        app.update();

        assert_eq!(evicted(&app), 1, "the edit's chunk was kept and left wrong");
        // Not `chunk_count`: the same frame drains a budget's worth of the flood, so the
        // store is not empty — it simply no longer holds the coordinate the edit named.
        assert!(
            app.world()
                .resource::<ChunkStore>()
                .get(coord(0, 0, 0))
                .is_none(),
            "the edited chunk is still resident, and now wrong by one voxel"
        );
    }

    #[test]
    fn an_eviction_takes_the_queued_load_for_that_chunk_with_it() {
        // The correctness the eviction rests on, and the one way it could go wrong. A
        // load for the evicted coordinate may already be queued — that is the normal
        // sequence, since the server sends `ChunkData` and then edits what it sent — and
        // a load left behind would land the pre-edit copy after the eviction, turning a
        // chunk that is merely absent into one that is present and wrong.
        let mut app = ingest_app(true);
        push(&mut app, payload_at(coord(1, 0, 0)));
        for _ in 0..MAX_DECODE_BACKLOG {
            push(&mut app, payload_at(coord(9, 9, 9)));
        }
        // Inside chunk (1, 0, 0) at edge 32, so this is an edit to the queued load.
        push(&mut app, edit(at(32, 0, 0), palette::AIR));
        app.update();

        // Drain everything the bound let through, so "absent" cannot mean "not yet".
        for _ in 0..MAX_BACKLOG_FRAMES + 1 {
            app.update();
        }

        assert_eq!(backlog(&app), 0, "the backlog was not fully drained");
        assert!(
            app.world()
                .resource::<ChunkStore>()
                .get(coord(1, 0, 0))
                .is_none(),
            "the queued load survived the eviction and landed its pre-edit copy"
        );
    }

    #[test]
    fn a_flood_of_edits_for_held_chunks_cannot_grow_the_backlog() {
        // The finding this closes, in the shape it was reported: a server that sends
        // nothing but `BlockUpdate`s for chunks the client holds. Every one of them
        // costs decode budget and none of them could be refused, so the queue grew for
        // as long as the server kept writing — the ceiling the issue's title asks for,
        // missing in exactly one direction.
        let mut app = ingest_app(true);
        push(&mut app, one_payload());
        app.update();
        assert_eq!(chunk_count(&app), 1, "the chunk the flood is aimed at");

        for _ in 0..LARGEST_JOIN_THE_CONTRACT_PERMITS {
            push(&mut app, edit(at(0, 0, 0), palette::AIR));
        }
        app.update();

        assert!(
            frames_of_backlog(&app) <= MAX_BACKLOG_FRAMES,
            "{} frames of backlog, past the {MAX_BACKLOG_FRAMES} the bound promises",
            frames_of_backlog(&app)
        );
        assert_eq!(evicted(&app), 1, "the flood's chunk is still resident");
    }

    #[test]
    fn an_edit_for_a_chunk_this_session_never_held_evicts_nothing() {
        // `evicted` is the count of terrain that went away, so it may only move when
        // terrain went away. An edit for a coordinate the store does not hold is turned
        // away like any other update over the bound — the drain would have answered
        // `Unheld` and dropped it — and nothing was lost to count.
        let mut app = ingest_app(true);
        for _ in 0..MAX_DECODE_BACKLOG {
            push(&mut app, payload_at(coord(9, 9, 9)));
        }
        push(&mut app, edit(at(0, 0, 0), palette::AIR));
        app.update();

        assert_eq!(refused(&app), 1, "the edit was not turned away");
        assert_eq!(
            evicted(&app),
            0,
            "an eviction was counted for absent terrain"
        );
    }

    #[test]
    fn nothing_is_refused_below_the_bound() {
        // The bound must be invisible to every burst normal play produces. The largest
        // is a join at the server's default view distance — `(2 * 3 + 1)³` = 343 chunks,
        // fourteen times under this ceiling — so a burst that size has to arrive whole.
        const JOIN_AT_SERVER_DEFAULT: usize = 7 * 7 * 7;

        let mut app = ingest_app(true);
        for _ in 0..JOIN_AT_SERVER_DEFAULT {
            push(&mut app, one_payload());
        }
        app.update();

        assert_eq!(refused(&app), 0, "a legitimate join was refused");
        assert_eq!(
            backlog(&app),
            JOIN_AT_SERVER_DEFAULT - MAX_DECODES_PER_FRAME,
            "and all of it is queued, minus the frame's budget"
        );
    }

    // -----------------------------------------------------------------
    // Asking for an evicted chunk back
    // -----------------------------------------------------------------

    /// An ingest app that can also send, plus the receiving end of its outbound channel.
    fn ingest_app_that_can_send(capacity: usize) -> (App, Receiver<Vec<u8>>) {
        let mut app = ingest_app(true);
        let (outbound, sent) = Outbound::to_a_test(capacity);
        app.insert_resource(outbound);
        (app, sent)
    }

    /// Everything the client has put on the wire, taken.
    ///
    /// Compared against `encode_chunk_resend_request` rather than decoded: `net::codec`'s
    /// own tests hold that encoder to the contract, so comparing bytes asserts the
    /// coordinate *and* that nothing else was sent, in one line.
    fn frames(sent: &Receiver<Vec<u8>>) -> Vec<Vec<u8>> {
        sent.try_iter().collect()
    }

    /// Fills the backlog exactly to the bound, so the next update pushed is over it.
    fn fill_the_backlog(app: &mut App) {
        for _ in 0..MAX_DECODE_BACKLOG {
            push(app, payload_at(coord(9, 9, 9)));
        }
    }

    #[test]
    fn a_chunk_the_bound_evicts_is_asked_for_again() {
        // The repair the eviction policy was missing. The chunk is dropped because the
        // edit for it could not be admitted, and the request is what turns "gone until the
        // player walks out of the view volume and back" into "gone until the server
        // answers" — which for a player standing on the hole is the whole difference.
        let (mut app, sent) = ingest_app_that_can_send(64);

        push(&mut app, one_payload());
        app.update();
        assert_eq!(
            chunk_count(&app),
            1,
            "the chunk to be evicted is not resident"
        );
        assert!(
            frames(&sent).is_empty(),
            "a chunk that arrived normally asked for something"
        );

        fill_the_backlog(&mut app);
        push(&mut app, edit(at(0, 0, 0), palette::AIR));
        app.update();

        assert_eq!(
            evicted(&app),
            1,
            "the edit did not evict the chunk it named"
        );
        assert_eq!(
            frames(&sent),
            vec![encode_chunk_resend_request(coord(0, 0, 0))],
            "the evicted chunk was not asked for, or something else was"
        );
    }

    #[test]
    fn an_unload_over_the_bound_is_not_asked_for() {
        // The one eviction the client must not try to undo. The server drops an unloaded
        // coordinate from `View.loaded` and never mentions it again, so the request would
        // be refused in silence — and would spend a per-session budget that the chunks
        // which *can* come back need.
        let (mut app, sent) = ingest_app_that_can_send(64);

        push(&mut app, one_payload());
        app.update();

        fill_the_backlog(&mut app);
        push(
            &mut app,
            WorldUpdate::Unload {
                coord: coord(0, 0, 0),
            },
        );
        app.update();

        assert_eq!(evicted(&app), 1, "the unload did not evict the chunk");
        assert!(
            frames(&sent).is_empty(),
            "the client asked for a chunk the server had just told it to drop"
        );
    }

    #[test]
    fn an_unload_beats_a_refused_edit_for_the_same_chunk() {
        // Both arrive over the bound in one frame, and they disagree about whether the
        // chunk may be asked for. The unload wins whichever order they came in, because it
        // is the server's own statement that the coordinate is gone.
        for edit_first in [true, false] {
            let (mut app, sent) = ingest_app_that_can_send(64);
            push(&mut app, one_payload());
            app.update();

            fill_the_backlog(&mut app);
            let unload = WorldUpdate::Unload {
                coord: coord(0, 0, 0),
            };
            if edit_first {
                push(&mut app, edit(at(0, 0, 0), palette::AIR));
                push(&mut app, unload);
            } else {
                push(&mut app, unload);
                push(&mut app, edit(at(0, 0, 0), palette::AIR));
            }
            app.update();

            assert!(
                frames(&sent).is_empty(),
                "edit_first = {edit_first}: an unloaded chunk was asked for"
            );
        }
    }

    #[test]
    fn one_request_per_eviction_and_never_a_retry() {
        // The contract has no reply to wait for, so one is the only honest number: a
        // client that re-asked would be guessing at what silence meant, and guessing wrong
        // means asking the server to redo work it already refused. Three edits for one
        // chunk are one eviction, and the frames that follow ask for nothing at all.
        let (mut app, sent) = ingest_app_that_can_send(64);

        push(&mut app, one_payload());
        app.update();
        let _ = frames(&sent);

        fill_the_backlog(&mut app);
        for _ in 0..3 {
            push(&mut app, edit(at(0, 0, 0), palette::AIR));
        }
        app.update();
        assert_eq!(
            frames(&sent).len(),
            1,
            "three edits for one chunk produced more than one request"
        );

        for _ in 0..5 {
            app.update();
        }
        assert!(
            frames(&sent).is_empty(),
            "the client retried a request nobody had answered"
        );
    }

    #[test]
    fn a_full_outbound_queue_costs_the_request_and_nothing_else() {
        // A Bevy system may never block on a socket, so a request that will not fit is
        // dropped. What that costs is the chunk staying where the eviction left it, which
        // is where every evicted chunk was before this message existed.
        let (mut app, sent) = ingest_app_that_can_send(1);

        for cx in 0..3 {
            push(&mut app, payload_at(coord(cx, 0, 0)));
        }
        app.update();
        assert_eq!(
            chunk_count(&app),
            3,
            "the three chunks are not all resident"
        );
        let _ = frames(&sent);

        fill_the_backlog(&mut app);
        for cx in 0..3 {
            push(&mut app, edit(at(cx * SIZE as i32, 0, 0), palette::AIR));
        }
        app.update();

        assert_eq!(evicted(&app), 3, "the bound did not evict all three chunks");
        assert_eq!(
            frames(&sent).len(),
            1,
            "a queue with room for one took more than one request"
        );
    }

    #[test]
    fn an_eviction_with_nowhere_to_send_is_not_an_error() {
        // `Outbound` exists exactly while there is a net thread to send to, so its absence
        // means the session has ended — and a chunk is not something an ended session is
        // missing. The eviction still happens; nothing asks for anything.
        let mut app = ingest_app(true);

        push(&mut app, one_payload());
        app.update();

        fill_the_backlog(&mut app);
        push(&mut app, edit(at(0, 0, 0), palette::AIR));
        app.update();

        assert_eq!(evicted(&app), 1, "the eviction needed a sender to happen");
    }
}
