//! Meshing off the main schedule, and the entities that draw the result.
//!
//! ## Nothing meshes on the main schedule
//!
//! A full chunk is hundreds of microseconds of work and a session opens with
//! hundreds of chunks arriving at once. Meshing them where the frame is built would
//! stall it for as long as the burst lasts, so [`start_mesh_jobs`] hands each chunk
//! to `AsyncComputeTaskPool` and returns, and [`apply_finished_meshes`] collects
//! whatever has finished on some **later** frame. A chunk therefore appears a frame
//! or two after its bytes arrive, which is the trade this makes on purpose.
//!
//! `mesh_chunk` is a pure function of a chunk **and the six chunks around it**, which
//! is what lets it move to another thread at all: the task captures an
//! `Arc<VoxelChunk>` and a [`Neighbours`] of the same, and touches no world, no asset
//! and no resource. The neighbours are gathered here, on the main schedule, from the
//! store that is the authority on what exists — the mesher never reaches for them.
//!
//! ## A neighbour that moves under a running task is not waited for
//!
//! A mesh is checked against the chunk it was built from and **not** against the
//! neighbours it was culled against, and that asymmetry is deliberate. The staleness
//! guard below exists because applying revision A's mesh over revision B draws the
//! world one edit behind itself with nothing queued to correct it. A neighbour that
//! changes mid-task has already queued the correction: `ChunkStore` logs a
//! `NeighbourChanged` for this coordinate, so it is in `pending` and will be meshed
//! again from the current voxels.
//!
//! Discarding on a neighbour instead would be worse than useless during the burst it
//! would fire on. A join replaces every chunk's neighbourhood several times over as the
//! volume fills in, so a task whose neighbours must hold still would be thrown away
//! again and again and no terrain would reach the screen until streaming settled. What
//! the client shows for a frame or two instead is a border wall drawn when it did not
//! need to be, or missing when it did — the same "a chunk appears a frame or two after
//! its bytes arrive" this module is built on.
//!
//! ## One task per chunk, at most — and a mesh belongs to the chunk it was built from
//!
//! A chunk the client did not acknowledge is re-sent by the server's next view
//! update, so the same coordinate can go stale while its mesh is still being built.
//! `in_flight` is keyed by coordinate, so a re-sent chunk waits in `pending` rather
//! than starting a second task that would race the first to the same entity.
//!
//! That leaves the other half of the same race: the task that is *already* running was
//! built from voxels the store may have replaced by the time it finishes. "This
//! coordinate still exists" and "this is still that chunk" are different questions, so
//! [`MeshJob`] keeps the `Arc<VoxelChunk>` it handed the task and
//! [`apply_finished_meshes`] compares it against the store's. A mesh that loses that
//! comparison is discarded, and the coordinate is still in `pending` — so the current
//! revision is meshed next.
//!
//! ## Neither the camera nor the sun is here any more
//!
//! The camera was, until movement landed. A camera that follows a gameplay entity belongs
//! to the module that knows where that entity is, so `player/camera.rs` owns it now —
//! including the sky colour, the ambient term and the fog, which are attached to the
//! camera rather than to global resources so they do not depend on plugin order. There is
//! still exactly one camera, and `ui/status.rs` still spawns none of its own.
//!
//! The sun followed it, for the same reason one step later. It was a constant here for as
//! long as it *was* a constant; the moment it became a function of a snapshot's
//! `tick_of_day` it belonged with the rest of the snapshot-driven presentation, and
//! `player/sky.rs` owns it now.
//!
//! What stayed is what this module can answer for on its own: the chunk meshes and the two
//! materials they share.
//!
//! ## Two materials, and one entity per chunk with one child
//!
//! `mesh_chunk` returns a [`ChunkMesh`] holding an opaque surface and a water surface — the
//! reason is on that type — and each half gets a material and a `Mesh3d`. The chunk's entity
//! carries the opaque half and the transform; the water half hangs off it as a child, so it
//! inherits that transform and goes with it under the same `despawn()`.
//!
//! **A chunk with water but no opaque surface still gets the parent entity**, without a mesh
//! on it: the middle of a lake is exactly that shape. A chunk with neither half gets no entity
//! at all, which is the case this module has always skipped and is most of the sky.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bevy::asset::RenderAssetUsages;
use bevy::ecs::system::SystemParam;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on, poll_once};

use super::{ChunkChange, ChunkMesh, ChunkStore, DecodeQueue, SurfaceMesh, VoxelChunk, mesh_chunk};
use crate::net::{ChunkCoord, Session};

/// How many meshing tasks may be started per frame.
///
/// A join streams the whole view distance at once — 4 913 chunks at radius 8 — and
/// spawning a task for every one of them in a single frame would queue more work
/// than the pool can retire before the next burst and allocate every result before
/// any of it is applied. The cap turns that into a steady trickle.
const MAX_JOBS_PER_FRAME: usize = 32;

/// How many finished meshes may become assets per frame.
///
/// Bounded for the other half of the same reason: `Assets<Mesh>::add` and the entity
/// spawn are main-thread work, so a frame that applied two hundred meshes would be
/// exactly the stall that moving the meshing off-thread was meant to avoid.
const MAX_APPLIED_PER_FRAME: usize = 16;

/// Keeps one mesh entity per loaded chunk.
pub struct ChunkRenderPlugin;

impl Plugin for ChunkRenderPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MeshJobs>()
            .init_resource::<MeshStats>()
            .add_systems(Startup, create_materials)
            .add_systems(
                Update,
                (
                    start_mesh_jobs,
                    apply_finished_meshes,
                    refresh_mesh_stats,
                    log_when_meshing_settles,
                )
                    .chain()
                    .after(super::ingest_world_updates),
            );
    }
}

/// The material every opaque chunk face shares.
///
/// One material, and therefore one pipeline for the whole opaque world, because the
/// colour comes from the mesh's vertex colours rather than from the material — see
/// `palette.rs`. The alternative is a material per block type, which means a mesh
/// per block type per chunk.
#[derive(Resource, Debug)]
struct TerrainMaterial(Handle<StandardMaterial>);

/// The material every water face shares.
///
/// The second material and the only one, because the split is by **alpha mode** and
/// not by block: a material is a pipeline, `AlphaMode::Blend` is what a pipeline has
/// to be built for, and every block that is ever drawn through is drawn through the
/// same way. Its base colour is white for the same reason [`TerrainMaterial`]'s is —
/// `palette::linear_rgba` owns what water looks like, alpha included, and a tint here
/// would multiply into itself.
#[derive(Resource, Debug)]
struct WaterMaterial(Handle<StandardMaterial>);

/// The two materials a chunk is drawn with, as one system parameter.
///
/// Two resources and not one, because they answer two different questions and a system
/// that needs only one should say so. Bundled for the reason `player/camera.rs`'s `Aim`
/// is: it names the pair, and it keeps [`apply_finished_meshes`] inside the argument
/// budget the second material took it past. Both are absent only before `Startup`.
#[derive(SystemParam)]
struct ChunkMaterials<'w> {
    opaque: Option<Res<'w, TerrainMaterial>>,
    water: Option<Res<'w, WaterMaterial>>,
}

/// Marks the entities this module spawns for chunks, so a query can find them
/// without also matching the camera and the light.
#[derive(Component)]
struct ChunkMeshEntity;

/// What the debug overlay reports about meshing.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq)]
pub struct MeshStats {
    /// Chunks the session holds voxels for — what the server has streamed.
    ///
    /// Mirrored from [`ChunkStore`] rather than read from it by the overlay, so the
    /// whole line has exactly one change signal to watch and cannot be rebuilt on a
    /// frame where nothing moved.
    pub chunks_held: usize,
    /// Chunks with a live mesh entity. Lower than the store's chunk count while
    /// meshing lags behind streaming, and lower for good once it catches up, because
    /// a chunk of pure air has no mesh at all.
    pub meshed_chunks: usize,
    /// Merged quads across every live chunk mesh.
    pub total_quads: usize,
    /// How long the most recently applied chunk spent inside the mesher. Excludes
    /// the wait in the queue, so it measures the mesher rather than the backlog.
    pub last_mesh: Option<Duration>,
    /// Meshing tasks started and not yet collected.
    pub in_flight: usize,
    /// Chunks waiting for a task slot.
    pub queued: usize,
    /// World updates the server has sent that have not been expanded into voxels yet.
    ///
    /// The one backlog in the pipeline that used to be invisible: before expansion was
    /// capped it never outlived the frame it arrived in, so there was nothing to show.
    /// It is here rather than in a resource of its own so the overlay keeps watching
    /// exactly one change signal.
    pub decode_backlog: usize,
    /// Updates the backlog's bound has refused, for the whole session.
    ///
    /// Beside `decode_backlog` because it is the other half of the same reading: a
    /// backlog that has stopped growing because the burst ended and one that has
    /// stopped growing because it is turning terrain away are the same number without
    /// it. Monotonic — a session total, not a rate — so a value that never moves is
    /// the healthy case.
    pub decode_refused: usize,
    /// Chunks the backlog's bound has evicted from the store, for the whole session.
    ///
    /// The third number of the same reading, and the only one of the three that
    /// describes something the player can see: a refusal turns away what had not
    /// arrived, an eviction takes terrain that had. Also monotonic.
    pub decode_evicted: usize,
}

/// One finished meshing task.
///
/// Carries no coordinate: the task is stored under one in `in_flight`, and a second
/// copy inside the result would be a second thing to keep in step.
struct MeshOutcome {
    mesh: ChunkMesh,
    elapsed: Duration,
}

/// What the renderer knows about one chunk's mesh.
#[derive(Debug)]
struct MeshedChunk {
    entity: Entity,
    quads: usize,
}

/// One meshing task, and the exact voxels it was handed.
///
/// The chunk is kept because finishing a mesh does not make it current. `ChunkStore`
/// replaces a coordinate's voxels wholesale — the server unloads and re-sends, and a
/// `BlockUpdate` swaps in an edited revision — so a task that started before the
/// replacement produces a mesh of terrain that no longer exists.
struct MeshJob {
    task: Task<MeshOutcome>,
    /// Compared by pointer rather than by value, and that is sound rather than
    /// merely cheap: [`ChunkStore::insert`] allocates a fresh `Arc` for every
    /// revision, and this handle keeps the revision it was built from alive for as
    /// long as the comparison can happen — so the allocator cannot recycle its
    /// address for the revision that replaced it, and `Arc::ptr_eq` cannot answer
    /// "the same" about two different chunks.
    ///
    /// Comparing the voxels instead would be 64 KiB of `memcmp` per finished chunk to
    /// answer a question about identity, and it would answer it wrongly: a re-sent
    /// chunk's two revisions are byte-identical, and the mesh of one is still not the
    /// mesh of the other. Since block edits landed the *contents* differ too, so the
    /// guard is no longer only about identity — it is what stops the world being drawn
    /// one edit behind itself.
    source: Arc<VoxelChunk>,
}

/// The meshing pipeline's bookkeeping.
#[derive(Resource, Default)]
struct MeshJobs {
    /// Chunks whose mesh is stale, waiting for a task slot.
    pending: HashSet<ChunkCoord>,
    /// Tasks started and not yet collected.
    in_flight: HashMap<ChunkCoord, MeshJob>,
    /// The entity drawing each chunk, and how many quads it holds.
    meshed: HashMap<ChunkCoord, MeshedChunk>,
}

fn create_materials(mut commands: Commands, mut materials: ResMut<Assets<StandardMaterial>>) {
    commands.insert_resource(TerrainMaterial(materials.add(StandardMaterial {
        // White, and that is load-bearing: the shader multiplies the material's
        // base colour by the vertex colour, so anything else here tints the whole
        // palette.
        base_color: Color::WHITE,
        // Rock, earth and snow, none of which are shiny.
        perceptual_roughness: 0.95,
        ..default()
    })));

    commands.insert_resource(WaterMaterial(materials.add(StandardMaterial {
        // White for the reason above, and it matters more here: the vertex colour carries
        // water's alpha as well as its blue, so a tinted base colour would fade the water
        // by `WATER_ALPHA` twice over.
        base_color: Color::WHITE,
        // The whole of what makes this a second material. `Blend` puts the mesh in the
        // transparent phase, where Bevy sorts entities back to front — which is why the
        // water half has to be its own entity rather than more quads in the opaque mesh.
        alpha_mode: AlphaMode::Blend,
        // The one wet surface in this world; everything else is rock, earth or snow at
        // 0.95. Low enough for a highlight, which is most of what tells a still lake from
        // a hole in the ground.
        perceptual_roughness: 0.15,
        ..default()
    })));
}

/// Turns the store's change log into meshing work and starts as much of it as the
/// per-frame cap allows.
fn start_mesh_jobs(
    mut store: ResMut<ChunkStore>,
    mut jobs: ResMut<MeshJobs>,
    mut commands: Commands,
) {
    // Guarded, because `ResMut` marks a resource changed on every `DerefMut`: taking
    // an empty log every frame would leave `ChunkStore` permanently "changed" and
    // defeat the change detection every consumer of it relies on.
    if store.has_changes() {
        // In order, always. A `Loaded` that follows an `Unloaded` for the same
        // coordinate must win, and the other way round too; a pair of sets could not
        // express that.
        for change in store.take_changes() {
            match change {
                ChunkChange::Loaded(coord) => {
                    jobs.pending.insert(coord);
                }
                // The same queue entry as a load, because the work is the same: mesh
                // this coordinate's current voxels against whatever is around them
                // now. What differs is only why it is stale, and the store has already
                // said that. A coordinate this session does not hold falls out below,
                // where `store.get` answers `None` — which is the ordinary case for the
                // neighbour of an edit at the edge of the streamed volume.
                ChunkChange::NeighbourChanged(coord) => {
                    jobs.pending.insert(coord);
                }
                ChunkChange::Unloaded(coord) => {
                    jobs.pending.remove(&coord);
                    if let Some(meshed) = jobs.meshed.remove(&coord) {
                        commands.entity(meshed.entity).despawn();
                    }
                    // A task already running for this chunk is left to finish, and its
                    // result is discarded on collection because the store no longer
                    // holds the chunk. Cancelling a `Task` means dropping it, which
                    // would need the same membership check anyway.
                }
            }
        }
    }

    if jobs.pending.is_empty() {
        return;
    }

    // Taken out so the loop can borrow the rest of `jobs` mutably. What is not
    // started goes back at the end, which is also how a chunk whose task is still
    // running keeps its place in the queue.
    let pending = std::mem::take(&mut jobs.pending);
    let mut leftover = HashSet::with_capacity(pending.len());
    let pool = AsyncComputeTaskPool::get();
    let mut started = 0usize;

    for coord in pending {
        if started >= MAX_JOBS_PER_FRAME || jobs.in_flight.contains_key(&coord) {
            leftover.insert(coord);
            continue;
        }
        // Unloaded between the log entry and here: there is nothing to mesh, and
        // nothing to keep queued either.
        let Some(chunk) = store.get(coord) else {
            continue;
        };

        // `Arc::clone`, so the 64 KiB of voxels is shared with the task rather than
        // copied into it, and the store stays free to hand the same chunk to a later
        // task. The second clone is the staleness guard's half: the task consumes its
        // handle, and this one is what the result is checked against.
        let source = Arc::clone(chunk);
        let voxels = Arc::clone(chunk);
        // The six chunks the border faces are culled against, resolved here and moved
        // into the task. Six more `Arc` handles, not six more chunks — and gathered on
        // this side of the boundary because the mesher is handed its neighbours and
        // never goes looking for them, which is the whole reason it can run here at all.
        // Whatever is absent stays absent: the mesher reads it as air and over-draws.
        let neighbours = store.neighbours(coord);
        let task = pool.spawn(async move {
            let began = Instant::now();
            let mesh = mesh_chunk(&voxels, &neighbours);
            MeshOutcome {
                mesh,
                elapsed: began.elapsed(),
            }
        });

        jobs.in_flight.insert(coord, MeshJob { task, source });
        started += 1;
    }

    jobs.pending = leftover;
}

/// Collects whatever meshing has finished and puts it on screen.
fn apply_finished_meshes(
    mut jobs: ResMut<MeshJobs>,
    mut stats: ResMut<MeshStats>,
    mut meshes: ResMut<Assets<Mesh>>,
    materials: ChunkMaterials<'_>,
    store: Res<ChunkStore>,
    session: Option<Res<Session>>,
    mut commands: Commands,
) {
    // All three exist from the first frame after startup. A frame without them is a
    // frame before there is a world, and there is nothing to place a chunk relative to.
    let (Some(material), Some(water_material), Some(session)) =
        (materials.opaque, materials.water, session)
    else {
        return;
    };
    let chunk_size = f32::from(session.0.chunk_size);

    // Polled once each, and the value is taken on that one poll: polling a `Task`
    // again after it has completed is not allowed. `poll_once` and never a blocking
    // wait, because this system runs on the frame's schedule and must return whether
    // the mesher is done or not.
    let mut collected = Vec::new();
    for (coord, job) in jobs.in_flight.iter_mut() {
        if collected.len() >= MAX_APPLIED_PER_FRAME {
            break;
        }
        if let Some(outcome) = block_on(poll_once(&mut job.task)) {
            collected.push((*coord, outcome));
        }
    }

    for (coord, outcome) in collected {
        // Taken out here rather than in a pass of its own, because the `Arc` the task
        // was built from has to come out with it — that handle is the whole guard
        // below. `None` cannot happen: these are the keys just iterated. Skipped
        // rather than unwrapped anyway, on the principle the rest of this client
        // follows.
        let Some(job) = jobs.in_flight.remove(&coord) else {
            continue;
        };
        stats.last_mesh = Some(outcome.elapsed);

        // Two ways a finished mesh can be about nothing, and the store is the
        // authority on both. The chunk may have been unloaded while it was being
        // meshed — then this result describes terrain the session cannot see. Or the
        // coordinate may have been *replaced*, in which case the result describes a
        // revision of it that no longer exists: mesh A applied over chunk B draws the
        // world one edit behind itself.
        //
        // Discarding costs nothing that has to be recovered. `ChunkStore::insert`
        // logged the replacement, so the coordinate is still in `pending` and the
        // current revision gets a task of its own on a later frame.
        if !store
            .get(coord)
            .is_some_and(|held| Arc::ptr_eq(held, &job.source))
        {
            continue;
        }

        if let Some(previous) = jobs.meshed.remove(&coord) {
            // A re-sent chunk. The entity is replaced rather than reused, because its
            // mesh handle is the last reference to the old asset — despawning it is
            // also what frees the old buffers.
            commands.entity(previous.entity).despawn();
        }

        // An all-air chunk, and any chunk whose every face is interior, gets no entity
        // at all. Spawning an empty mesh would cost a draw call to render nothing, and
        // the server streams a great deal of sky.
        if outcome.mesh.is_empty() {
            continue;
        }

        let quads = outcome.mesh.quad_count();
        let ChunkMesh {
            opaque,
            water: water_surface,
        } = outcome.mesh;

        // The chunk's own entity: the transform both halves are placed by, and the opaque
        // mesh when there is one. A chunk in the middle of a lake has water and no opaque
        // surface, and it gets the entity without a mesh rather than an empty asset.
        let mut chunk_entity = commands.spawn((
            ChunkMeshEntity,
            Transform::from_translation(chunk_origin(coord, chunk_size)),
            Visibility::default(),
        ));
        if !opaque.is_empty() {
            chunk_entity.insert((
                Mesh3d(meshes.add(to_bevy_mesh(opaque))),
                MeshMaterial3d(material.0.clone()),
            ));
        }
        let entity = chunk_entity.id();

        // A child rather than a sibling, so the `despawn` above takes both — it despawns
        // descendants — and so the water is placed by the chunk's own transform.
        if !water_surface.is_empty() {
            commands.spawn((
                Mesh3d(meshes.add(to_bevy_mesh(water_surface))),
                MeshMaterial3d(water_material.0.clone()),
                Transform::default(),
                ChildOf(entity),
            ));
        }

        jobs.meshed.insert(coord, MeshedChunk { entity, quads });
    }
}

/// Republishes the pipeline's state for the debug overlay.
///
/// Derived from `MeshJobs` rather than maintained incrementally, so the two cannot
/// drift: an incremental total has to be adjusted on load, on replacement, on unload
/// and on the unload-during-meshing race, and one missed path is a counter that
/// lies for the rest of the session. Summing a few thousand `usize` once a frame is
/// cheaper than that class of bug.
///
/// Writes only on a change, because `ResMut` marks the resource changed on every
/// `DerefMut` — and the status line uses change detection to avoid rebuilding its
/// string every frame.
fn refresh_mesh_stats(
    jobs: Res<MeshJobs>,
    store: Res<ChunkStore>,
    queue: Res<DecodeQueue>,
    mut stats: ResMut<MeshStats>,
) {
    let next = MeshStats {
        chunks_held: store.chunk_count(),
        meshed_chunks: jobs.meshed.len(),
        total_quads: jobs.meshed.values().map(|meshed| meshed.quads).sum(),
        in_flight: jobs.in_flight.len(),
        queued: jobs.pending.len(),
        decode_backlog: queue.len(),
        decode_refused: queue.refused(),
        decode_evicted: queue.evicted(),
        last_mesh: stats.last_mesh,
    };

    if *stats != next {
        *stats = next;
    }
}

/// Whether the world pipeline still has work outstanding.
///
/// The decode backlog counts, and it has to: it is the first stage now that expansion
/// is metered, so a world that reported itself settled while chunks were still waiting
/// to be decoded would settle once per burst and mean nothing.
fn is_busy(stats: &MeshStats) -> bool {
    stats.decode_backlog > 0 || stats.in_flight > 0 || stats.queued > 0
}

/// Writes the world's shape to the log each time meshing catches up.
///
/// One line per burst, not per chunk — the mirror of the server's `view updated`
/// debug entry, and for the same reason: a burst is the interesting unit. At `debug`
/// level, so it costs nothing unless someone asks for it.
///
/// It exists because the debug overlay is on the screen, and a screen is exactly
/// what CI, a remote session and an automated end-to-end check do not have. Its
/// only effect is that log line; the counters themselves are [`MeshStats`].
fn log_when_meshing_settles(stats: Res<MeshStats>, mut was_busy: Local<bool>) {
    if is_busy(&stats) {
        *was_busy = true;
        return;
    }
    if !*was_busy {
        return;
    }
    *was_busy = false;

    debug!(
        "world settled: {} chunks held, {} meshed, {} quads, last mesh {:.2} ms",
        stats.chunks_held,
        stats.meshed_chunks,
        stats.total_quads,
        stats
            .last_mesh
            .map_or(f64::NAN, |elapsed| elapsed.as_secs_f64() * 1000.0),
    );
}

/// The world-space position of a chunk's minimum corner.
///
/// `chunk_size` comes from `ServerWelcome`, as `schemas/common.fbs` says it must:
/// the client multiplies the server's number rather than assuming 32. Widened to
/// `i64` first, because a chunk coordinate near `i32::MAX` times 32 overflows an
/// `i32` and would place the chunk on the opposite side of the world.
fn chunk_origin(coord: ChunkCoord, chunk_size: f32) -> Vec3 {
    Vec3::new(
        i64::from(coord.cx) as f32 * chunk_size,
        i64::from(coord.cy) as f32 * chunk_size,
        i64::from(coord.cz) as f32 * chunk_size,
    )
}

/// Wraps one half of the mesher's buffers in the asset the renderer draws.
///
/// The only place a [`SurfaceMesh`] meets a Bevy type, which is what keeps `mesher.rs`
/// free of them. Consumes the buffers rather than cloning: they are hundreds of
/// kilobytes each and the task's result is owned here.
fn to_bevy_mesh(mesh: SurfaceMesh) -> Mesh {
    // `RenderAssetUsages::default()` keeps the vertex data in the main world as well
    // as the render world, which is what lets Bevy compute the mesh's bounding box
    // for frustum culling. `RENDER_WORLD` alone frees the data before that happens.
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, mesh.positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, mesh.normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, mesh.colors)
    .with_inserted_indices(Indices::U32(mesh.indices))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use bevy::asset::AssetPlugin;

    use super::*;
    use crate::net::{BlockCoord, SessionParams, WorldInbox, WorldUpdate};
    use crate::world::{BlockId, MAX_DECODE_BACKLOG, MAX_DECODES_PER_FRAME, Neighbours, palette};

    const SIZE: u16 = 32;
    const VOLUME: u16 = 32768;

    /// How long a test will pump the app waiting for meshing to settle. Generous
    /// because a task pool on a loaded runner is not prompt, and irrelevant to
    /// runtime because every assertion is reached long before it.
    const PATIENCE: Duration = Duration::from_secs(30);

    fn coord(cx: i32, cy: i32, cz: i32) -> ChunkCoord {
        ChunkCoord { cx, cy, cz }
    }

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 1,
            spawn: [0.5, 80.0, 0.5],
            world_seed: 7,
            tick_rate: 20,
            chunk_size: SIZE,
            view_distance: 8,
            inventory_slots: 37,
            hotbar_slots: 9,
            equipment_slots: 4,
            player_token: crate::net::ANY_TOKEN,
        })
    }

    fn solid_chunk(block: BlockId) -> Vec<u16> {
        vec![block, VOLUME]
    }

    /// Quads a solid chunk merges to: one wall per direction.
    const SOLID_QUADS: usize = 6;

    /// Quads [`layered_chunk`] merges to: four horizontal caps plus two rectangles on
    /// each of the four side planes, which cannot merge across the gap between them.
    const LAYERED_QUADS: usize = 12;

    /// Two stone slabs with a gap of air between them, and air above.
    ///
    /// Chosen so its mesh is distinguishable from a solid chunk's by the counter the
    /// overlay already reports — twelve quads against six — which is what turns "whose
    /// mesh was applied" into an assertion instead of a guess.
    fn layered_chunk() -> Vec<u16> {
        // y is the slowest axis of the wire's index order, so a whole band of y-layers
        // is one contiguous run: four bands of eight fill a 32³ chunk exactly.
        const BAND: u16 = 8 * SIZE * SIZE;
        vec![
            palette::STONE,
            BAND,
            palette::AIR,
            BAND,
            palette::STONE,
            BAND,
            palette::AIR,
            BAND,
        ]
    }

    /// The world plugin on a headless app.
    ///
    /// `AssetPlugin` plus the two asset types the renderer touches, and no render
    /// app: `Assets<Mesh>` is a plain resource, so everything short of the GPU upload
    /// is exercised with no display at all. CI has neither a display nor a GPU, and
    /// these tests run there.
    fn headless_world() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .insert_resource(session())
            .add_plugins(crate::world::WorldPlugin);
        app
    }

    fn push(app: &mut App, update: WorldUpdate) {
        app.world_mut().resource_mut::<WorldInbox>().push(update);
    }

    /// Runs frames until `done` holds, or fails the test at [`PATIENCE`].
    fn pump_until(app: &mut App, what: &str, done: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            app.update();
            if done(app) {
                return;
            }
            // The task pool needs a moment to make progress, and a spin would starve
            // it on a single-core runner.
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!(
            "timed out waiting for {what}; stats are {:?}",
            app.world().resource::<MeshStats>()
        );
    }

    fn stats(app: &App) -> MeshStats {
        *app.world().resource::<MeshStats>()
    }

    /// The chunks that currently have a mesh, sorted so a failure reads cleanly.
    fn meshed_coords(app: &App) -> Vec<(i32, i32, i32)> {
        let mut coords: Vec<(i32, i32, i32)> = app
            .world()
            .resource::<MeshJobs>()
            .meshed
            .keys()
            .map(|c| (c.cx, c.cy, c.cz))
            .collect();
        coords.sort_unstable();
        coords
    }

    /// How many entities this module has spawned for chunks.
    fn chunk_entity_count(app: &mut App) -> usize {
        let world = app.world_mut();
        let mut query = world.query::<&ChunkMeshEntity>();
        query.iter(world).count()
    }

    #[test]
    fn the_plugin_spawns_neither_a_camera_nor_a_light_of_its_own() {
        // Both left, and for the same reason a frame apart: a camera that follows a
        // gameplay entity and a sun that follows a snapshot's clock belong to the module
        // that reads the snapshots. Two cameras on one window would need explicit ordering
        // and clear-colour configuration to stop one erasing the other, and two suns would
        // each carry half a day.
        let mut app = headless_world();
        app.update();

        let world = app.world_mut();
        assert_eq!(
            world.query::<&DirectionalLight>().iter(world).count(),
            0,
            "the sun belongs to player/sky.rs"
        );
        assert_eq!(
            world.query::<&Camera3d>().iter(world).count(),
            0,
            "this module must not spawn a camera"
        );
    }

    #[test]
    fn a_chunk_is_meshed_off_the_main_schedule_and_applied_later() {
        // The acceptance criterion that matters most in this module: the frame that
        // ingests a chunk is never the frame that meshes it.
        let mut app = headless_world();
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(0, 2, 0),
                runs: solid_chunk(palette::STONE),
            },
        );

        app.update();
        assert_eq!(
            stats(&app).meshed_chunks,
            0,
            "the ingest frame started a task and applied nothing"
        );
        assert_eq!(stats(&app).in_flight, 1, "and the task is off the schedule");
        assert_eq!(
            app.world().resource::<ChunkStore>().chunk_count(),
            1,
            "the voxels themselves are stored immediately"
        );

        pump_until(&mut app, "the chunk's mesh", |app| {
            stats(app).meshed_chunks == 1
        });

        // A solid chunk merges to six quads, one per wall — the mesher's own tests
        // prove that; this proves the number reaches the overlay.
        let settled = stats(&app);
        assert_eq!(settled.total_quads, 6);
        assert_eq!(settled.in_flight, 0);
        assert_eq!(settled.queued, 0);
        assert!(settled.last_mesh.is_some(), "the duration is recorded");
        assert_eq!(meshed_coords(&app), vec![(0, 2, 0)]);
        assert_eq!(chunk_entity_count(&mut app), 1);
    }

    /// Bottom half stone, top half water: the smallest chunk with both halves of a
    /// [`ChunkMesh`].
    fn lake_chunk() -> Vec<u16> {
        // y is the slowest axis of the wire's index order, so half the chunk is one
        // contiguous run.
        const HALF: u16 = 16 * SIZE * SIZE;
        vec![palette::STONE, HALF, palette::WATER, HALF]
    }

    /// The chunk entities and their children, as `(has its own mesh, child count)`.
    fn chunk_entities(app: &mut App) -> Vec<(bool, usize)> {
        let world = app.world_mut();
        let mut query =
            world.query_filtered::<(Option<&Mesh3d>, Option<&Children>), With<ChunkMeshEntity>>();
        query
            .iter(world)
            .map(|(mesh, children)| (mesh.is_some(), children.map_or(0, |c| c.len())))
            .collect()
    }

    /// Every material handle hanging off a chunk entity's children.
    fn water_child_materials(app: &mut App) -> Vec<Handle<StandardMaterial>> {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&MeshMaterial3d<StandardMaterial>, With<ChildOf>>();
        query
            .iter(world)
            .map(|material| material.0.clone())
            .collect()
    }

    #[test]
    fn water_is_drawn_by_a_child_entity_with_the_blending_material() {
        // The rendering split on one chunk: the chunk entity carries the opaque surface,
        // one child carries the water, and the materials are the two the plugin created.
        let mut app = headless_world();
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(0, 0, 0),
                runs: lake_chunk(),
            },
        );
        pump_until(&mut app, "the chunk's mesh", |app| {
            stats(app).meshed_chunks == 1
        });

        assert_eq!(
            chunk_entities(&mut app),
            vec![(true, 1)],
            "one chunk entity with its own opaque mesh and one water child"
        );

        let water = app.world().resource::<WaterMaterial>().0.clone();
        let terrain = app.world().resource::<TerrainMaterial>().0.clone();
        assert_ne!(water, terrain, "two materials, not one handle twice");
        assert_eq!(water_child_materials(&mut app), vec![water.clone()]);

        // And the material really is the blending one, which is the property that makes
        // the child necessary at all.
        let materials = app.world().resource::<Assets<StandardMaterial>>();
        assert_eq!(
            materials.get(&water).expect("water material").alpha_mode,
            AlphaMode::Blend
        );
        assert_eq!(
            materials
                .get(&terrain)
                .expect("terrain material")
                .alpha_mode,
            AlphaMode::Opaque,
            "the opaque half must stay in the opaque phase"
        );
    }

    #[test]
    fn a_chunk_of_nothing_but_water_gets_a_placer_with_no_mesh_of_its_own() {
        // The middle of a lake: the entity exists because the water has to be placed by
        // something, and carries no `Mesh3d` because an empty one draws nothing.
        let mut app = headless_world();
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(0, 0, 0),
                runs: solid_chunk(palette::WATER),
            },
        );
        pump_until(&mut app, "the chunk's mesh", |app| {
            stats(app).meshed_chunks == 1
        });

        assert_eq!(chunk_entities(&mut app), vec![(false, 1)]);
        assert_eq!(
            stats(&app).total_quads,
            6,
            "one merged wall per direction, all of it water"
        );
    }

    #[test]
    fn unloading_a_chunk_takes_its_water_with_it() {
        // `despawn` takes descendants, which is why the water is a child and not a
        // sibling: nothing in the unload path had to learn about a second entity.
        let mut app = headless_world();
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(0, 0, 0),
                runs: lake_chunk(),
            },
        );
        pump_until(&mut app, "the chunk's mesh", |app| {
            stats(app).meshed_chunks == 1
        });
        assert_eq!(water_child_materials(&mut app).len(), 1);

        push(
            &mut app,
            WorldUpdate::Unload {
                coord: coord(0, 0, 0),
            },
        );
        pump_until(&mut app, "the chunk to go", |app| {
            stats(app).meshed_chunks == 0
        });

        assert_eq!(chunk_entity_count(&mut app), 0);
        assert!(
            water_child_materials(&mut app).is_empty(),
            "the water outlived the chunk it belonged to"
        );
    }

    #[test]
    fn a_chunk_entity_sits_at_its_world_origin() {
        // Chunk coordinates are in chunk units. A mesh placed at the coordinate
        // instead of coordinate × chunk_size would pile the whole world into one
        // 32-block cube.
        let mut app = headless_world();
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(-1, 2, 3),
                runs: solid_chunk(palette::SNOW),
            },
        );
        pump_until(&mut app, "the chunk's mesh", |app| {
            stats(app).meshed_chunks == 1
        });

        let world = app.world_mut();
        let mut query = world.query_filtered::<&Transform, With<ChunkMeshEntity>>();
        let placed = query.iter(world).next().copied().expect("one chunk entity");
        assert_eq!(placed.translation, Vec3::new(-32.0, 64.0, 96.0));
    }

    #[test]
    fn the_mesh_asset_carries_positions_normals_colours_and_indices() {
        let mut app = headless_world();
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(0, 0, 0),
                runs: solid_chunk(palette::GRASS),
            },
        );
        pump_until(&mut app, "the chunk's mesh", |app| {
            stats(app).meshed_chunks == 1
        });

        let world = app.world_mut();
        let mut query = world.query::<&Mesh3d>();
        let handle = query.iter(world).next().cloned().expect("one chunk mesh");
        let meshes = app.world().resource::<Assets<Mesh>>();
        let mesh = meshes.get(&handle.0).expect("the asset exists");

        assert_eq!(mesh.primitive_topology(), PrimitiveTopology::TriangleList);
        assert_eq!(mesh.count_vertices(), 24, "six quads of four vertices");
        // The colour attribute is what makes the palette visible, and the normal
        // attribute is what makes the PBR pipeline light it. A mesh missing either
        // still renders — wrongly — so their presence is asserted rather than assumed.
        for (name, id) in [
            ("position", Mesh::ATTRIBUTE_POSITION.id),
            ("normal", Mesh::ATTRIBUTE_NORMAL.id),
            ("color", Mesh::ATTRIBUTE_COLOR.id),
        ] {
            assert!(
                mesh.attribute(id).is_some(),
                "the {name} attribute is missing"
            );
        }
        assert_eq!(
            mesh.indices().map(|indices| indices.len()),
            Some(36),
            "two triangles per quad"
        );
    }

    #[test]
    fn an_all_air_chunk_gets_no_entity() {
        // Air is a legitimate chunk — the server streams the sky above the terrain —
        // and an empty mesh would cost a draw call to render nothing.
        let mut app = headless_world();
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(0, 9, 0),
                runs: solid_chunk(palette::AIR),
            },
        );
        pump_until(&mut app, "the empty mesh to be collected", |app| {
            let s = stats(app);
            s.last_mesh.is_some() && s.in_flight == 0
        });

        assert_eq!(stats(&app).meshed_chunks, 0);
        assert_eq!(stats(&app).total_quads, 0);
        assert_eq!(chunk_entity_count(&mut app), 0);
        assert_eq!(
            app.world().resource::<ChunkStore>().chunk_count(),
            1,
            "the voxels are still held; only the mesh is absent"
        );
    }

    #[test]
    fn an_unloaded_chunk_loses_its_entity_and_its_quads() {
        let mut app = headless_world();
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(0, 2, 0),
                runs: solid_chunk(palette::STONE),
            },
        );
        pump_until(&mut app, "the chunk's mesh", |app| {
            stats(app).meshed_chunks == 1
        });

        push(
            &mut app,
            WorldUpdate::Unload {
                coord: coord(0, 2, 0),
            },
        );
        pump_until(&mut app, "the entity to go", |app| {
            stats(app).meshed_chunks == 0
        });

        assert_eq!(stats(&app).total_quads, 0, "the quad count follows");
        assert_eq!(chunk_entity_count(&mut app), 0);
        assert_eq!(app.world().resource::<ChunkStore>().chunk_count(), 0);
    }

    #[test]
    fn a_chunk_unloaded_while_it_was_being_meshed_never_appears() {
        // The race the store has to win: a mesh finishes for a chunk the server has
        // already taken away. Drawing it would show terrain this session cannot see.
        let mut app = headless_world();
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(4, 4, 4),
                runs: solid_chunk(palette::DIRT),
            },
        );
        // One frame: the task is started and nothing has been applied yet.
        app.update();
        assert_eq!(stats(&app).in_flight, 1);

        push(
            &mut app,
            WorldUpdate::Unload {
                coord: coord(4, 4, 4),
            },
        );
        pump_until(&mut app, "the task to be collected", |app| {
            stats(app).in_flight == 0
        });

        assert_eq!(stats(&app).meshed_chunks, 0);
        assert_eq!(chunk_entity_count(&mut app), 0);
    }

    #[test]
    fn a_re_sent_chunk_is_never_meshed_twice_at_once() {
        // A chunk the client did not acknowledge is re-sent, so the same coordinate
        // can go stale while its own task is still running. It must end with exactly
        // one entity, and never with two tasks racing to spawn it.
        let mut app = headless_world();
        for _ in 0..3 {
            push(
                &mut app,
                WorldUpdate::Chunk {
                    coord: coord(1, 1, 1),
                    runs: solid_chunk(palette::STONE),
                },
            );
            app.update();
            assert!(
                stats(&app).in_flight <= 1,
                "a coordinate must never have two tasks at once"
            );
        }

        pump_until(&mut app, "meshing to settle", |app| {
            let s = stats(app);
            s.in_flight == 0 && s.queued == 0 && s.meshed_chunks == 1
        });

        assert_eq!(meshed_coords(&app), vec![(1, 1, 1)]);
        assert_eq!(chunk_entity_count(&mut app), 1, "one entity, not three");
        assert_eq!(stats(&app).total_quads, SOLID_QUADS);
    }

    /// The revision a test wants swapped in, and where.
    #[derive(Resource)]
    struct SwapMidFlight {
        coord: ChunkCoord,
        chunk: Option<VoxelChunk>,
    }

    /// Replaces a coordinate's voxels inside the window the staleness guard is about.
    ///
    /// Ordered *after* `start_mesh_jobs` and *before* `apply_finished_meshes`, so the
    /// replacement is guaranteed to land after the task captured the first revision and
    /// before any frame could apply it. The interleaving is forced by the schedule
    /// rather than waited for, which is what makes the test deterministic: a sleep long
    /// enough to "usually" hit the window is a test that fails on a loaded runner and
    /// passes on a fast one.
    fn swap_mid_flight(
        mut swap: ResMut<SwapMidFlight>,
        mut store: ResMut<ChunkStore>,
        jobs: Res<MeshJobs>,
    ) {
        if swap.chunk.is_none() || !jobs.in_flight.contains_key(&swap.coord) {
            return;
        }
        let coord = swap.coord;
        let Some(chunk) = swap.chunk.take() else {
            return;
        };
        store.insert(coord, chunk);
    }

    /// Every quad total the overlay reported, with repeats collapsed.
    #[derive(Resource, Default)]
    struct QuadTrace(Vec<usize>);

    fn trace_quads(stats: Res<MeshStats>, mut trace: ResMut<QuadTrace>) {
        if trace.0.last() != Some(&stats.total_quads) {
            trace.0.push(stats.total_quads);
        }
    }

    #[test]
    fn a_mesh_built_from_a_replaced_chunk_is_never_drawn() {
        // The interleaving, driven directly: a task starts for revision A, the
        // coordinate is replaced by revision B, the task completes. A's mesh must not
        // reach the screen — a mesh belongs to the chunk it was built from, and
        // applying it to a chunk it was not built from draws terrain that no longer
        // exists.
        //
        // It used to be invisible: generation is deterministic, so A and B of a
        // re-sent coordinate were byte-identical and applying the wrong one changed
        // nothing on screen. Block edits ended that — an edited revision genuinely
        // differs from the one before it, and applying A over B draws the hole the
        // player dug as though it were still filled in.
        //
        // The two revisions are told apart by quad count — six for a solid chunk,
        // twelve for two slabs — so "whose mesh was applied" is read off the counter
        // the overlay already reports.
        let target = coord(2, 0, 0);
        let mut app = headless_world();
        app.insert_resource(SwapMidFlight {
            coord: target,
            chunk: Some(
                VoxelChunk::from_runs(&layered_chunk(), SIZE.into()).expect("a valid revision B"),
            ),
        })
        .init_resource::<QuadTrace>()
        .add_systems(
            Update,
            (
                swap_mid_flight
                    .after(start_mesh_jobs)
                    .before(apply_finished_meshes),
                trace_quads.after(refresh_mesh_stats),
            ),
        );

        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: target,
                runs: solid_chunk(palette::STONE),
            },
        );
        pump_until(&mut app, "revision B's mesh", |app| {
            let settled = stats(app);
            settled.total_quads == LAYERED_QUADS && !is_busy(&settled)
        });

        assert_eq!(
            app.world().resource::<QuadTrace>().0,
            vec![0, LAYERED_QUADS],
            "the overlay went from nothing straight to revision B; a {SOLID_QUADS} in \
             that trace is revision A's mesh reaching the screen"
        );
        assert_eq!(
            chunk_entity_count(&mut app),
            1,
            "one entity, and it is the current revision's"
        );
        assert_eq!(
            meshed_coords(&app),
            vec![(target.cx, target.cy, target.cz)],
            "and the discarded result left no bookkeeping behind"
        );
    }

    #[test]
    fn a_refused_payload_is_counted_where_the_overlay_can_see_it() {
        // The bound announces itself, and this is half of how: the refusal count rides
        // in `MeshStats` beside the queue depth, so a backlog holding steady because the
        // burst ended and one holding steady because it is turning terrain away are
        // distinguishable at a glance. The other half is the log line in
        // `ingest_world_updates` — the same division of labour as
        // `log_when_meshing_settles`, whose only effect is a log line and whose numbers
        // are asserted here instead.
        //
        // One coordinate for every payload: what is asserted is the counter, and
        // thousands of distinct chunks would be a `VOLUME`-sized allocation each.
        const FLOOD: usize = MAX_DECODE_BACKLOG + 64;

        let mut app = headless_world();
        assert_eq!(stats(&app).decode_refused, 0, "nothing refused yet");

        for _ in 0..FLOOD {
            push(
                &mut app,
                WorldUpdate::Chunk {
                    coord: coord(0, 0, 0),
                    runs: solid_chunk(palette::STONE),
                },
            );
        }
        app.update();

        assert_eq!(
            stats(&app).decode_refused,
            FLOOD - MAX_DECODE_BACKLOG,
            "the refusals never reached the overlay"
        );
        assert_eq!(
            stats(&app).decode_backlog,
            MAX_DECODE_BACKLOG - MAX_DECODES_PER_FRAME,
            "and the depth beside it is the bounded one"
        );
    }

    #[test]
    fn a_burst_of_chunks_is_spread_over_frames_and_all_of_them_arrive() {
        // What the per-frame caps are for. A join streams the whole view distance at
        // once; the caps turn that into a trickle, and nothing may be lost on the way.
        const CHUNKS: i32 = 50;
        let mut app = headless_world();
        for cx in 0..CHUNKS {
            push(
                &mut app,
                WorldUpdate::Chunk {
                    coord: coord(cx, 0, 0),
                    runs: solid_chunk(palette::STONE),
                },
            );
        }

        app.update();
        let first = stats(&app);
        assert!(
            first.in_flight <= MAX_JOBS_PER_FRAME,
            "started {} tasks in one frame",
            first.in_flight
        );
        // Stated as a conservation law over all three backlogs rather than as
        // `queued > 0`, because which of them holds the burst is now a tuning question:
        // expansion is capped too, so a chunk that has not been decoded yet waits in
        // `decode_backlog` instead of in `queued`. Losing one is not a tuning question.
        //
        // `decode_refused` is a term in it because the backlog is bounded: a chunk that
        // the bound turned away has left the pipeline, and the law has to account for it
        // rather than read the loss as conservation. Fifty chunks is far under
        // `MAX_DECODE_BACKLOG`, so it is zero here — which is the point. If it ever is
        // not, this test says so instead of quietly balancing.
        assert_eq!(
            first.decode_backlog
                + first.queued
                + first.in_flight
                + first.meshed_chunks
                + first.decode_refused,
            CHUNKS as usize,
            "every chunk is waiting, in flight, drawn or refused — none may vanish; {first:?}"
        );
        assert_eq!(
            first.decode_refused, 0,
            "a burst this size must not be refused"
        );

        // Settled, not merely drawn: every chunk that arrives beside one already on
        // screen sends it back for a remesh, so `meshed_chunks` reaches fifty a few
        // frames before the quad count stops moving.
        pump_until(&mut app, "every chunk", |app| {
            stats(app).meshed_chunks == CHUNKS as usize && !is_busy(&stats(app))
        });

        // Fifty solid chunks in a row along x, and the drop cross-chunk culling buys,
        // measured. Each of the 49 seams is a wall that used to be drawn twice — once
        // from each side — and is now drawn from neither, so the two end chunks show
        // five walls and the 48 between them show four: 300 quads become 202.
        let settled = stats(&app);
        const SEAMS: usize = CHUNKS as usize - 1;
        assert_eq!(
            settled.total_quads,
            CHUNKS as usize * SOLID_QUADS - 2 * SEAMS,
            "the shared walls are still being drawn"
        );
        assert_eq!(settled.decode_backlog, 0);
        assert_eq!(settled.queued, 0);
        assert_eq!(settled.in_flight, 0);
        assert_eq!(chunk_entity_count(&mut app), CHUNKS as usize);
    }

    // -----------------------------------------------------------------
    // Incremental remeshing
    // -----------------------------------------------------------------

    /// Which entity is drawing each chunk.
    ///
    /// The observable that makes "only this chunk was remeshed" an assertion.
    /// [`apply_finished_meshes`] despawns a chunk's previous entity and spawns a new one
    /// for the new mesh, so a coordinate's entity id changes exactly when it is remeshed —
    /// including when the new mesh happens to be identical to the old one, which is what a
    /// quad count cannot see.
    fn mesh_entities(app: &App) -> BTreeMap<(i32, i32, i32), Entity> {
        app.world()
            .resource::<MeshJobs>()
            .meshed
            .iter()
            .map(|(coord, meshed)| ((coord.cx, coord.cy, coord.cz), meshed.entity))
            .collect()
    }

    /// Queues one authoritative block change, in world block coordinates.
    fn edit(app: &mut App, pos: [i32; 3], block: BlockId) {
        push(
            app,
            WorldUpdate::Block {
                pos: BlockCoord {
                    x: pos[0],
                    y: pos[1],
                    z: pos[2],
                },
                block_id: block,
            },
        );
    }

    /// Loads `coords` as solid stone chunks and waits until every one of them is drawn.
    fn world_of_stone(coords: &[ChunkCoord]) -> App {
        let mut app = headless_world();
        for coord in coords {
            push(
                &mut app,
                WorldUpdate::Chunk {
                    coord: *coord,
                    runs: solid_chunk(palette::STONE),
                },
            );
        }
        pump_until(&mut app, "every chunk's first mesh", |app| {
            stats(app).meshed_chunks == coords.len() && !is_busy(&stats(app))
        });
        app
    }

    /// Runs frames until the meshing pipeline has settled again.
    fn pump_until_settled(app: &mut App, what: &str) {
        pump_until(app, what, |app| !is_busy(&stats(app)));
    }

    #[test]
    fn digging_a_hole_remeshes_the_chunk_and_adds_the_faces_it_exposed() {
        // The whole point of the issue, counted rather than looked at. A solid chunk merges
        // to six walls; a one-voxel cavity in the middle of it exposes six new faces, one
        // per plane bounding the cavity, and none of them can merge with anything. Twelve.
        //
        // The outer walls are untouched, which is the other half of the assertion: a mesher
        // handed the edited chunk and asked to start again produces the same six merged
        // 32×32 quads it did before.
        let target = coord(0, 0, 0);
        let mut app = world_of_stone(&[target]);
        assert_eq!(stats(&app).total_quads, SOLID_QUADS);
        let before = mesh_entities(&app);

        edit(&mut app, [10, 11, 12], palette::AIR);
        pump_until(&mut app, "the remesh", |app| {
            stats(app).total_quads != SOLID_QUADS && !is_busy(&stats(app))
        });

        assert_eq!(
            stats(&app).total_quads,
            SOLID_QUADS + 6,
            "six walls and the six faces of the cavity"
        );
        assert_ne!(
            mesh_entities(&app),
            before,
            "the chunk kept the mesh it had before the edit"
        );
        assert_eq!(chunk_entity_count(&mut app), 1, "one entity, not two");
    }

    #[test]
    fn an_edit_remeshes_one_chunk_and_leaves_the_rest_of_the_view_alone() {
        // "Only the affected chunk is remeshed — not the whole view." Five chunks are
        // drawn, one voxel changes well inside one of them, and exactly one entity is
        // replaced. A renderer that rebuilt everything would pass every quad-count
        // assertion in this file and stall the frame on every edit.
        let edited = coord(0, 0, 0);
        let others = [
            coord(1, 0, 0),
            coord(0, 1, 0),
            coord(0, 0, 1),
            coord(-1, 0, 0),
        ];
        let mut app = world_of_stone(&[edited, others[0], others[1], others[2], others[3]]);
        let before = mesh_entities(&app);
        // Read rather than computed as `5 × SOLID_QUADS`: all four of the others share a
        // wall with the middle chunk, so eight of the thirty walls are culled before
        // anything is edited. What the predicate needs is that the number *moved*.
        let settled = stats(&app).total_quads;

        edit(&mut app, [10, 11, 12], palette::AIR);
        pump_until(&mut app, "the remesh", |app| {
            stats(app).total_quads != settled && !is_busy(&stats(app))
        });

        let after = mesh_entities(&app);
        assert_ne!(
            after.get(&(edited.cx, edited.cy, edited.cz)),
            before.get(&(edited.cx, edited.cy, edited.cz)),
            "the edited chunk was not remeshed"
        );
        for untouched in others {
            let key = (untouched.cx, untouched.cy, untouched.cz);
            assert_eq!(
                after.get(&key),
                before.get(&key),
                "chunk {key:?} was remeshed for an edit that cannot have changed its mesh"
            );
        }
    }

    #[test]
    fn an_edit_on_a_chunk_border_remeshes_the_neighbour_across_it() {
        // A face on the shared border is culled against the neighbour's voxel, so an edit
        // there makes the neighbour's mesh wrong even though none of its own voxels moved.
        // The invalidation landed one issue before the culling did, deliberately, so that
        // the first thing culling produced was not a wall with a hole in it wherever
        // somebody had dug. This is what the two halves do together.
        //
        // Still asserted on the entity rather than on a quad count alone, because the rule
        // is that the neighbour is **remeshed** — true even where the new mesh happens to
        // match the old one, which a counter cannot see. The count is asserted too now
        // that it moves, and it says which chunk drew the floor of the hole.
        let edited = coord(0, 0, 0);
        let across = coord(1, 0, 0);
        let mut app = world_of_stone(&[edited, across]);
        let before = mesh_entities(&app);
        assert_eq!(
            stats(&app).total_quads,
            2 * SOLID_QUADS - 2,
            "the wall the two share is drawn by neither"
        );

        // Local x = 31, so the voxel sits on the wall the two chunks share.
        edit(&mut app, [31, 5, 5], palette::AIR);
        pump_until_settled(&mut app, "both remeshes");

        // The edited chunk gains the five faces that bound the cavity from inside itself,
        // and the neighbour gains the one face the cavity uncovered — the floor of the
        // hole, which belongs to the neighbour's voxel and is drawn once, by the chunk
        // that owns it.
        assert_eq!(stats(&app).total_quads, 2 * SOLID_QUADS - 2 + 5 + 1);

        let after = mesh_entities(&app);
        assert_ne!(
            after.get(&(0, 0, 0)),
            before.get(&(0, 0, 0)),
            "the edited chunk was not remeshed"
        );
        assert_ne!(
            after.get(&(1, 0, 0)),
            before.get(&(1, 0, 0)),
            "the neighbour across the edited border was not remeshed"
        );
        assert_eq!(chunk_entity_count(&mut app), 2, "still one entity each");
    }

    #[test]
    fn an_edit_away_from_a_border_leaves_the_neighbour_alone() {
        // The control for the rule above, and what stops it from becoming "remesh the
        // neighbourhood": one voxel further in, no other chunk's mesh can depend on it.
        let mut app = world_of_stone(&[coord(0, 0, 0), coord(1, 0, 0)]);
        let before = mesh_entities(&app);
        // Read rather than computed: the wall the two chunks share is culled from both
        // sides, so the pair starts at ten quads and not at twelve.
        let settled = stats(&app).total_quads;

        edit(&mut app, [30, 5, 5], palette::AIR);
        pump_until(&mut app, "the remesh", |app| {
            stats(app).total_quads != settled && !is_busy(&stats(app))
        });

        let after = mesh_entities(&app);
        assert_ne!(after.get(&(0, 0, 0)), before.get(&(0, 0, 0)));
        assert_eq!(
            after.get(&(1, 0, 0)),
            before.get(&(1, 0, 0)),
            "the neighbour was remeshed for an edit that does not touch their shared wall"
        );
    }

    #[test]
    fn a_border_edit_remeshes_a_neighbour_this_session_holds_and_forgets_the_rest() {
        // A border edit at the edge of the streamed volume names a neighbour that was never
        // sent. It must be a no-op rather than an entity for a chunk with no voxels: the
        // coordinate is queued, `store.get` answers `None`, and the queue entry is dropped.
        let mut app = world_of_stone(&[coord(0, 0, 0)]);

        edit(&mut app, [31, 5, 5], palette::AIR);
        pump_until(&mut app, "the remesh", |app| {
            stats(app).total_quads != SOLID_QUADS && !is_busy(&stats(app))
        });

        assert_eq!(
            mesh_entities(&app).len(),
            1,
            "one chunk is held, one is drawn"
        );
        assert_eq!(chunk_entity_count(&mut app), 1);
        assert_eq!(
            stats(&app).queued,
            0,
            "and nothing is left waiting for voxels"
        );
    }

    #[test]
    fn an_edit_is_meshed_off_the_main_schedule_like_a_load() {
        // The same observation as `a_chunk_is_meshed_off_the_main_schedule_and_applied
        // _later`, for the path an edit takes: the frame that applies a `BlockUpdate`
        // starts a task and applies no mesh. Remeshing is the largest piece of work an edit
        // causes, and doing it where the frame is built would stall on every dug block.
        let mut app = world_of_stone(&[coord(0, 0, 0)]);
        let before = mesh_entities(&app);

        edit(&mut app, [10, 11, 12], palette::AIR);
        app.update();

        assert_eq!(
            stats(&app).in_flight,
            1,
            "the edit's remesh is not off the schedule"
        );
        assert_eq!(
            mesh_entities(&app),
            before,
            "the ingest frame put a new mesh on screen"
        );

        pump_until_settled(&mut app, "the remesh");
        assert_ne!(mesh_entities(&app), before, "and a later frame applied it");
    }

    // -----------------------------------------------------------------
    // Culling against the neighbours
    // -----------------------------------------------------------------

    #[test]
    fn a_chunk_that_arrives_takes_its_neighbour_wall_with_it() {
        // The saving, end to end and on the path a join actually takes. One solid chunk
        // draws six walls. A second one arriving beside it makes the wall they share
        // invisible from anywhere outside the pair, so neither draws it any more — and
        // the chunk that was *already on screen* has to be remeshed for that to be true,
        // which is the half a mesher change alone would not have delivered.
        let mut app = world_of_stone(&[coord(0, 0, 0)]);
        assert_eq!(stats(&app).total_quads, SOLID_QUADS);
        let before = mesh_entities(&app);

        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(1, 0, 0),
                runs: solid_chunk(palette::STONE),
            },
        );
        pump_until(&mut app, "both meshes", |app| {
            stats(app).meshed_chunks == 2 && !is_busy(&stats(app))
        });

        assert_eq!(
            stats(&app).total_quads,
            2 * SOLID_QUADS - 2,
            "twelve walls minus the one the two of them share, from both sides"
        );
        assert_ne!(
            mesh_entities(&app).get(&(0, 0, 0)),
            before.get(&(0, 0, 0)),
            "the chunk already on screen was not remeshed when its neighbour arrived"
        );
    }

    #[test]
    fn an_arriving_chunk_remeshes_the_six_that_border_it_and_nothing_else() {
        // The no-cascade rule, from the other direction than the edit tests take it. A
        // chunk arrives into a neighbourhood that already holds its six face neighbours
        // and three chunks touching it only along an edge or a corner. The six are
        // remeshed because the newcomer is now hiding a wall of each; a diagonal shares
        // no face with it, so no quad of a diagonal's can move and remeshing one would be
        // work with a guaranteed-identical result.
        let faces = [
            coord(-1, 0, 0),
            coord(1, 0, 0),
            coord(0, -1, 0),
            coord(0, 1, 0),
            coord(0, 0, -1),
            coord(0, 0, 1),
        ];
        let diagonals = [coord(1, 1, 0), coord(1, 0, 1), coord(1, 1, 1)];
        let mut app = world_of_stone(&[faces.as_slice(), diagonals.as_slice()].concat());
        let before = mesh_entities(&app);
        let before_quads = stats(&app).total_quads;

        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(0, 0, 0),
                runs: solid_chunk(palette::STONE),
            },
        );
        pump_until_settled(&mut app, "the newcomer and its neighbours");

        let after = mesh_entities(&app);
        for face in faces {
            let key = (face.cx, face.cy, face.cz);
            assert_ne!(
                after.get(&key),
                before.get(&key),
                "chunk {key:?} shares a wall with the newcomer and was not remeshed"
            );
        }
        for diagonal in diagonals {
            let key = (diagonal.cx, diagonal.cy, diagonal.cz);
            assert_eq!(
                after.get(&key),
                before.get(&key),
                "chunk {key:?} was remeshed for a chunk it shares no face with"
            );
        }

        // The acceptance criterion in its strongest form: a chunk enclosed on all six
        // sides has no exposed face at all, so it is not merely cheap to draw — it costs
        // no mesh, no asset and no entity. Underground, that is most of the world.
        assert!(
            !after.contains_key(&(0, 0, 0)),
            "a chunk with no exposed face must not cost an entity"
        );
        // The saving, stated as the thing that should be surprising: a chunk **arrived**
        // and the world got cheaper to draw. Its six neighbours each stop drawing the
        // wall they share with it, and it draws none of its own.
        assert_eq!(
            before_quads - stats(&app).total_quads,
            6,
            "one wall per neighbour, and nothing at all from the newcomer"
        );
    }

    #[test]
    fn an_unloaded_chunk_gives_its_neighbour_the_wall_back() {
        // The direction that would be a hole rather than a waste if it were missed. A
        // chunk whose neighbour goes away has to draw the wall it had been culling, or
        // the edge of the streamed volume is see-through — and the server unloads a
        // chunk every time the player walks far enough for one to leave the view.
        let mut app = world_of_stone(&[coord(0, 0, 0), coord(1, 0, 0)]);
        assert_eq!(stats(&app).total_quads, 2 * SOLID_QUADS - 2);
        let before = mesh_entities(&app);

        push(
            &mut app,
            WorldUpdate::Unload {
                coord: coord(1, 0, 0),
            },
        );
        pump_until(&mut app, "the survivor to be remeshed", |app| {
            stats(app).meshed_chunks == 1 && !is_busy(&stats(app))
        });

        assert_eq!(
            stats(&app).total_quads,
            SOLID_QUADS,
            "the wall the unloaded chunk was hiding is not drawn again"
        );
        assert_ne!(
            mesh_entities(&app).get(&(0, 0, 0)),
            before.get(&(0, 0, 0)),
            "the survivor kept a mesh built against a chunk that is gone"
        );
    }

    #[test]
    fn a_malformed_chunk_costs_no_entity_and_no_task() {
        let mut app = headless_world();
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(0, 0, 0),
                runs: vec![palette::STONE, 5],
            },
        );
        app.update();
        app.update();

        assert_eq!(stats(&app).in_flight, 0);
        assert_eq!(stats(&app).meshed_chunks, 0);
        assert_eq!(chunk_entity_count(&mut app), 0);
    }

    /// Records what a consumer with change detection would have seen, one entry per
    /// frame.
    #[derive(Resource, Default)]
    struct ChangeLog {
        stats: Vec<bool>,
        store: Vec<bool>,
    }

    fn log_changes(stats: Res<MeshStats>, store: Res<ChunkStore>, mut log: ResMut<ChangeLog>) {
        log.stats.push(stats.is_changed());
        log.store.push(store.is_changed());
    }

    #[test]
    fn an_idle_frame_marks_neither_the_stats_nor_the_store_changed() {
        // The status line rebuilds its string on a change, so a resource that looks
        // changed every frame would reallocate it every frame for the rest of the
        // session. Both are easy to get wrong in opposite ways: `refresh_mesh_stats`
        // writes `MeshStats` unconditionally unless it compares first, and
        // `start_mesh_jobs` takes `ChunkStore` mutably, which marks it changed on
        // every `DerefMut` whether the log was empty or not.
        //
        // Observed from inside a system rather than with `is_changed()` from outside,
        // because `App::update()` ends each frame with `World::clear_trackers()`; an
        // external check after an update is always false and would pass regardless.
        let mut app = headless_world();
        app.init_resource::<ChangeLog>()
            .add_systems(Update, log_changes.after(refresh_mesh_stats));

        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(0, 0, 0),
                runs: solid_chunk(palette::STONE),
            },
        );
        pump_until(&mut app, "the chunk's mesh", |app| {
            stats(app).meshed_chunks == 1
        });

        // Ignore the frames that legitimately changed something; what follows is only
        // the settled behaviour.
        let settled = stats(&app);
        {
            let mut log = app.world_mut().resource_mut::<ChangeLog>();
            log.stats.clear();
            log.store.clear();
        }

        for _ in 0..6 {
            app.update();
            assert_eq!(
                stats(&app),
                settled,
                "an idle frame must not move a counter"
            );
        }

        let log = app.world().resource::<ChangeLog>();
        assert_eq!(
            log.stats,
            vec![false; 6],
            "MeshStats was rewritten while idle"
        );
        assert_eq!(
            log.store,
            vec![false; 6],
            "ChunkStore was touched while idle"
        );
    }

    #[test]
    fn the_pipeline_is_busy_exactly_while_work_is_outstanding() {
        // What the settle log keys on, and what an end-to-end run reads to decide the
        // world has arrived. A queue that is drained but has tasks running is still
        // busy, and so is the other way round — and so is a world whose chunks have
        // arrived but have not been expanded yet, which is the stage this issue
        // introduced and the one a settle time would otherwise stop measuring.
        assert!(!is_busy(&MeshStats::default()));
        assert!(is_busy(&MeshStats {
            in_flight: 1,
            ..MeshStats::default()
        }));
        assert!(is_busy(&MeshStats {
            queued: 1,
            ..MeshStats::default()
        }));
        assert!(is_busy(&MeshStats {
            decode_backlog: 1,
            ..MeshStats::default()
        }));
        assert!(
            !is_busy(&MeshStats {
                meshed_chunks: 400,
                total_quads: 9_000,
                ..MeshStats::default()
            }),
            "a finished world is not busy"
        );
    }

    #[test]
    fn the_world_origin_multiplies_the_servers_chunk_size() {
        // 32 is the server's current answer, not a constant here. A client that
        // hardcoded it would tile the world with gaps the moment the server changed.
        assert_eq!(
            chunk_origin(coord(2, -3, 0), 16.0),
            Vec3::new(32.0, -48.0, 0.0)
        );
        assert_eq!(
            chunk_origin(coord(2, -3, 0), 32.0),
            Vec3::new(64.0, -96.0, 0.0)
        );
        assert_eq!(chunk_origin(coord(0, 0, 0), 32.0), Vec3::ZERO);
    }

    #[test]
    fn a_far_chunk_coordinate_does_not_overflow_into_the_wrong_hemisphere() {
        // i32::MAX × 32 overflows an i32. Widening first is what keeps a chunk on the
        // side of the world it belongs to.
        let far = chunk_origin(coord(i32::MAX, 0, i32::MIN), 32.0);

        assert!(far.x > 0.0, "x wrapped: {far:?}");
        assert!(far.z < 0.0, "z wrapped: {far:?}");
    }

    #[test]
    fn an_empty_chunk_mesh_becomes_an_asset_with_no_triangles() {
        // `to_bevy_mesh` is never called with an empty mesh in production, because the
        // caller skips those. Asserted anyway: it must be a mesh with no triangles
        // rather than a panic, so a later caller cannot be surprised by it.
        let mesh = to_bevy_mesh(SurfaceMesh::default());

        assert_eq!(mesh.count_vertices(), 0);
        assert_eq!(mesh.indices().map(|indices| indices.len()), Some(0));
    }

    #[test]
    fn every_chunk_mesh_shares_one_material() {
        // One material, and therefore one pipeline, for the whole *opaque* world; the
        // colour comes from vertex colours. A material per chunk would be a pipeline
        // change per draw call. The chunks here hold no water, so the two handles below
        // are the only ones in the world — `water_is_drawn_by_a_child_entity_with_the_
        // blending_material` is where the second material is pinned.
        let mut app = headless_world();
        for cx in 0..2 {
            push(
                &mut app,
                WorldUpdate::Chunk {
                    coord: coord(cx, 0, 0),
                    runs: solid_chunk(palette::STONE),
                },
            );
        }
        pump_until(&mut app, "both chunks", |app| stats(app).meshed_chunks == 2);

        let shared = app.world().resource::<TerrainMaterial>().0.clone();
        let world = app.world_mut();
        let mut query = world.query::<&MeshMaterial3d<StandardMaterial>>();
        let handles: Vec<_> = query
            .iter(world)
            .map(|material| material.0.clone())
            .collect();

        assert_eq!(handles.len(), 2);
        assert!(handles.iter().all(|handle| *handle == shared));
    }

    #[test]
    fn the_mesher_needs_none_of_this() {
        // The purity rule, stated as a test: a chunk meshes with no app, no resources
        // and no display. The day this needs a `World`, the meshing task cannot exist.
        //
        // Culling against the neighbours did not cost that, and the signature is why:
        // the neighbourhood is an argument, so "no neighbours known" is a value this
        // test can pass rather than a store it would have to stand up.
        let chunk =
            VoxelChunk::from_runs(&solid_chunk(palette::STONE), SIZE.into()).expect("valid");

        assert_eq!(mesh_chunk(&chunk, &Neighbours::default()).quad_count(), 6);
    }
}
