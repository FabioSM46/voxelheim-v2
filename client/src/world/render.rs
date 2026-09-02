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
//! What stayed is what this module can answer for on its own: the chunk meshes and the three
//! materials they share.
//!
//! ## Three materials, and one entity per chunk with two children
//!
//! `mesh_chunk` returns a [`ChunkMesh`] holding an opaque surface, a water surface and a
//! cover surface — the reason is on that type — and each half gets a material and a
//! `Mesh3d`. The chunk's entity carries the opaque half and the transform; the other two
//! hang off it as children, so they inherit that transform and go with it under the same
//! `despawn()`, which despawns descendants.
//!
//! **A chunk with water or cover but no opaque surface still gets the parent entity**,
//! without a mesh on it: the middle of a lake is exactly that shape, and so is a chunk of
//! meadow air above the ground. A chunk with no half at all gets no entity, which is the
//! case this module has always skipped and is most of the sky.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bevy::asset::RenderAssetUsages;
use bevy::ecs::system::SystemParam;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on, poll_once};

use super::water_material::{FlowingWater, FlowingWaterPlugin};
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
///
/// **Still a count, and #629 is where that was measured rather than assumed.** That
/// issue named this number and `MAX_DECODES_PER_FRAME` as its two suspects and asked
/// for the spike to be attributed to a system before either moved. The harness at the
/// foot of this file did the attribution: on the frames a chunk shell lands on, this
/// system spent **0.02 to 0.13 ms** — under 2% of the spike frame — where
/// `ingest_world_updates` spent 88% of it. Over a whole optimized drain its median
/// frame was 0.037 ms, its 99th percentile 0.094 ms and its worst 0.28 ms. So the
/// expansion budget became a duration (`MAX_DECODE_TIME_PER_FRAME` in `world/mod.rs`)
/// and this one did not move at all, because nothing measured says it is a cost.
///
/// **What that measurement cannot see, said here rather than left in a pull request.**
/// The harness runs with no render app, so `Assets<Mesh>::add` is a resource insert and
/// the GPU buffer upload it schedules never happens. The number above is the *main
/// schedule's* share, which is a floor on what applying a mesh costs a real frame. If a
/// capture on a machine with a display ever shows the upload dominating, this is the
/// constant to revisit, and the change would be the one `world/mod.rs` already made:
/// keep 16 as the ceiling and let a slice of the frame be the rule.
const MAX_APPLIED_PER_FRAME: usize = 16;

/// Keeps one mesh entity per loaded chunk.
pub struct ChunkRenderPlugin;

impl Plugin for ChunkRenderPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MeshJobs>()
            .init_resource::<MeshStats>()
            .add_plugins(FlowingWaterPlugin)
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
///
/// A [`FlowingWater`] rather than a bare `StandardMaterial` since #598: the base half
/// is the same material this always was, and the extension beside it slides a ripple
/// along the flow the mesher wrote into the vertex attributes. What the base half
/// answers for — colour, alpha, roughness, the blending phase — did not move.
#[derive(Resource, Debug)]
struct WaterMaterial(Handle<FlowingWater>);

/// The material every cover face shares.
///
/// The third material, and the split is by **pipeline** again rather than by block: a
/// stem is a plane, so it is seen from behind as often as from in front, and a
/// `cull_mode` is baked into the pipeline a material builds. Opaque and lit like the
/// terrain — a flower in shade should read as being in shade — so this is
/// [`TerrainMaterial`] with the back faces kept and their normals flipped, and its base
/// colour is white for the reason the other two are.
#[derive(Resource, Debug)]
struct CoverMaterial(Handle<StandardMaterial>);

/// The three materials a chunk is drawn with, as one system parameter.
///
/// Three resources and not one, because they answer three different questions and a
/// system that needs only one should say so. Bundled for the reason
/// `player/camera.rs`'s `Aim` is: it names the set, and it keeps
/// [`apply_finished_meshes`] inside the argument budget the second material took it
/// past. All are absent only before `Startup`.
#[derive(SystemParam)]
struct ChunkMaterials<'w> {
    opaque: Option<Res<'w, TerrainMaterial>>,
    water: Option<Res<'w, WaterMaterial>>,
    cover: Option<Res<'w, CoverMaterial>>,
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

fn create_materials(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut water: ResMut<Assets<FlowingWater>>,
) {
    commands.insert_resource(TerrainMaterial(materials.add(StandardMaterial {
        // White, and that is load-bearing: the shader multiplies the material's
        // base colour by the vertex colour, so anything else here tints the whole
        // palette.
        base_color: Color::WHITE,
        // Rock, earth and snow, none of which are shiny.
        perceptual_roughness: 0.95,
        ..default()
    })));

    commands.insert_resource(WaterMaterial(water.add(FlowingWater {
        base: StandardMaterial {
            // White for the reason above, and it matters more here: the vertex colour
            // carries water's alpha as well as its blue, so a tinted base colour would
            // fade the water by `WATER_ALPHA` twice over.
            base_color: Color::WHITE,
            // The whole of what makes this a second material. `Blend` puts the mesh in
            // the transparent phase, where Bevy sorts entities back to front — which is
            // why the water half has to be its own entity rather than more quads in the
            // opaque mesh.
            alpha_mode: AlphaMode::Blend,
            // The one wet surface in this world; everything else is rock, earth or snow
            // at 0.95. Low enough for a highlight, which is most of what tells a still
            // lake from a hole in the ground.
            perceptual_roughness: 0.15,
            ..default()
        },
        // Zero, and a system replaces it every frame from `Res<Time>`.
        extension: default(),
    })));

    commands.insert_resource(CoverMaterial(materials.add(StandardMaterial {
        // White for the reason the other two are: `palette` owns the colours, and the
        // vertex colours carry them.
        base_color: Color::WHITE,
        // A stem's two blades are planes with no inside, so the half of each that faces
        // away from the camera has to be drawn rather than culled. `double_sided` is the
        // other half of that: without it the back face would be shaded by the front's
        // normal and one side of every flower would be lit from the wrong direction.
        cull_mode: None,
        double_sided: true,
        // Vegetation, not rock and not water.
        perceptual_roughness: 0.8,
        // No alpha, deliberately: the geometry is the flower's shape, so there is
        // nothing to cut out, and staying in the opaque phase keeps cover out of the
        // sort water needs.
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
    // All four exist from the first frame after startup. A frame without them is a
    // frame before there is a world, and there is nothing to place a chunk relative to.
    let (Some(material), Some(water_material), Some(cover_material), Some(session)) =
        (materials.opaque, materials.water, materials.cover, session)
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
            cover: cover_surface,
        } = outcome.mesh;

        // The chunk's own entity: the transform every half is placed by, and the opaque
        // mesh when there is one. A chunk in the middle of a lake has water and no opaque
        // surface, and it gets the entity without a mesh rather than an empty asset. So
        // does a chunk holding nothing but flowers, for the same reason.
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

        // Children rather than siblings, so the `despawn` above takes all three — it
        // despawns descendants — and so each half is placed by the chunk's own transform.
        //
        // Two blocks and not one loop over the pair: since #598 the water handle is a
        // `FlowingWater` and the cover handle a `StandardMaterial`, so there is no one
        // type an array of the two could have. The shape they share is `MeshMaterial3d`,
        // and that is a component rather than a value these can be quantified over.
        if !water_surface.is_empty() {
            commands.spawn((
                Mesh3d(meshes.add(to_bevy_mesh(water_surface))),
                MeshMaterial3d(water_material.0.clone()),
                Transform::default(),
                ChildOf(entity),
            ));
        }
        if !cover_surface.is_empty() {
            commands.spawn((
                Mesh3d(meshes.add(to_bevy_mesh(cover_surface))),
                MeshMaterial3d(cover_material.0.clone()),
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
///
/// **The sum was named as a suspect, and #651 priced it.** #642 left a finding it did not
/// act on: with the decode spike metered away, the worst join frame was owned by none of
/// the three world systems, and what was left was "the command flush, `refresh_mesh_stats`
/// and Bevy's own scheduling". This system is the only one of those three that lives in
/// this repository, and it is the one whose cost grows with the *world* rather than with
/// the burst — it walks every meshed chunk once a frame, for as long as the session lasts.
/// So it is now stamped by the harness at the foot of this file rather than argued about.
/// Over a 343-chunk join settling into 146 meshed chunks it costs a **median of
/// 0.0024–0.0038 ms** a frame, 0.002–0.005 ms on the worst frame of the join, and
/// 0.010–0.022 ms on the worst frame it ever had. That is about a fiftieth of one percent
/// of a 60 Hz frame — and it is the same figure in both build profiles, because summing a
/// few hundred `usize` is not work an optimizer has much to do with.
///
/// **So the paragraph above stands as written, and the counter stays derived.** The trade
/// it describes — one sum a frame against a class of drift bug that survives any missed
/// update path — was made without a number, and the number favours it by three orders of
/// magnitude. What would reopen it is a view distance putting a hundred times more chunks
/// in the store, and `MAX_DECODE_BACKLOG`'s own arithmetic binds well before that.
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
    let mut built = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, mesh.positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, mesh.normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, mesh.colors);

    // The flow attributes, on the surfaces that carry them — which is the water half
    // and nothing else, so the opaque and cover halves' vertices stay four floats lighter. The
    // two ride in the pipeline's own texture-coordinate slots rather than in a custom
    // attribute: `MeshPipeline` already forwards UV_0 and UV_1 to the fragment stage
    // under `VERTEX_UVS_A` / `VERTEX_UVS_B`, so this needs no vertex shader and no
    // layout specialization of ours. See `WaterFlow` in `mesher.rs` for what is in
    // them, and `world/flowing_water.wgsl` for what reads them.
    if !mesh.flow.is_empty() {
        built = built
            .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, mesh.flow)
            .with_inserted_attribute(Mesh::ATTRIBUTE_UV_1, mesh.falling);
    }

    built.with_inserted_indices(Indices::U32(mesh.indices))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use bevy::asset::AssetPlugin;

    use super::*;
    use crate::net::{BlockCoord, SessionParams, WorldInbox, WorldUpdate};
    use crate::world::{
        BlockId, DecodeTimeBudget, MAX_DECODE_BACKLOG, MAX_DECODE_TIME_PER_FRAME,
        MAX_DECODES_PER_FRAME, Neighbours, mesher, palette,
    };

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
    ///
    /// The expansion time budget is [`Duration::MAX`], so the count is the only bound
    /// the drain can reach and the burst assertions below stay exact rather than
    /// measuring the runner — `DecodeTimeBudget` in `world/mod.rs` carries the argument.
    /// The measurements at the end of this file install the real default instead.
    fn headless_world() -> App {
        world_with_budget(DecodeTimeBudget(Duration::MAX))
    }

    /// [`headless_world`] with the expansion budget named rather than disabled.
    ///
    /// `insert_resource` after the plugin, so the value here wins over the one
    /// `WorldPlugin` installs. What the plugin installs is pinned by
    /// [`the_plugin_ships_the_metered_drain`].
    fn world_with_budget(budget: DecodeTimeBudget) -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .insert_resource(session())
            .add_plugins(crate::world::WorldPlugin)
            .insert_resource(budget);
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
    fn the_plugin_ships_the_metered_drain() {
        // The other half of `the_shipping_budget_is_two_milliseconds` in `world/mod.rs`,
        // and it lives here because this is the file with an app that carries the whole
        // plugin stack. Every other test in this module overrides the budget so its
        // per-frame counts stay exact; this one is what says the client does not.
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .insert_resource(session())
            .add_plugins(crate::world::WorldPlugin);

        assert_eq!(
            *app.world().resource::<DecodeTimeBudget>(),
            DecodeTimeBudget::default(),
            "a client built from this tree expands on a slice of the frame"
        );
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

    /// Every `StandardMaterial` handle hanging off a chunk entity's children — which is
    /// the cover half and nothing else, since water's handle is a `FlowingWater`.
    fn cover_child_materials(app: &mut App) -> Vec<Handle<StandardMaterial>> {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&MeshMaterial3d<StandardMaterial>, With<ChildOf>>();
        query
            .iter(world)
            .map(|material| material.0.clone())
            .collect()
    }

    /// A chunk of grass with one flower standing on it, as the wire's run order sends it.
    fn meadow_chunk() -> Vec<u16> {
        // y is the slowest axis, so one layer of grass is a contiguous run and the
        // flower is a single voxel in the layer above it.
        const LAYER: u16 = SIZE * SIZE;
        vec![
            palette::GRASS,
            LAYER,
            palette::FLOWER_RED,
            1,
            palette::AIR,
            VOLUME - LAYER - 1,
        ]
    }

    /// Every material handle hanging off a chunk entity's children.
    fn water_child_materials(app: &mut App) -> Vec<Handle<FlowingWater>> {
        let world = app.world_mut();
        let mut query = world.query_filtered::<&MeshMaterial3d<FlowingWater>, With<ChildOf>>();
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
        assert_eq!(water_child_materials(&mut app), vec![water.clone()]);

        // And the material really is the blending one, which is the property that makes
        // the child necessary at all. It lives on the extended material's **base** half
        // since #598, together with everything else that decides what water looks like:
        // the extension animates the brightness and answers for nothing here.
        let base = app
            .world()
            .resource::<Assets<FlowingWater>>()
            .get(&water)
            .expect("water material")
            .base
            .clone();
        assert_eq!(base.alpha_mode, AlphaMode::Blend);
        assert_eq!(base.base_color, Color::WHITE);
        assert_eq!(base.perceptual_roughness, 0.15);
        assert_eq!(
            app.world()
                .resource::<Assets<StandardMaterial>>()
                .get(&terrain)
                .expect("terrain material")
                .alpha_mode,
            AlphaMode::Opaque,
            "the opaque half must stay in the opaque phase"
        );
    }

    #[test]
    fn cover_is_drawn_by_a_child_entity_with_the_double_sided_material() {
        // The third half's rendering split. The chunk entity carries the grass, one
        // child carries the flower, and the material it gets is the one that keeps both
        // sides of a stem — opaque and lit, because a flower in shade is in shade.
        let mut app = headless_world();
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(0, 0, 0),
                runs: meadow_chunk(),
            },
        );
        pump_until(&mut app, "the chunk's mesh", |app| {
            stats(app).meshed_chunks == 1
        });

        assert_eq!(
            chunk_entities(&mut app),
            vec![(true, 1)],
            "one chunk entity with the opaque mesh and one cover child"
        );

        let cover = app.world().resource::<CoverMaterial>().0.clone();
        let terrain = app.world().resource::<TerrainMaterial>().0.clone();
        assert_ne!(cover, terrain, "three materials, not one handle twice");
        assert_eq!(cover_child_materials(&mut app), vec![cover.clone()]);
        assert!(
            water_child_materials(&mut app).is_empty(),
            "a meadow has no water, so no `FlowingWater` child"
        );

        let materials = app.world().resource::<Assets<StandardMaterial>>();
        let material = materials.get(&cover).expect("cover material");
        assert_eq!(material.cull_mode, None, "a stem is seen from both sides");
        assert!(material.double_sided, "and lit correctly on both");
        assert!(
            !material.unlit,
            "cover is lit like the terrain it stands in"
        );
        assert_eq!(
            material.alpha_mode,
            AlphaMode::Opaque,
            "cover has no alpha, so it stays out of the sort water needs"
        );
    }

    #[test]
    fn a_chunk_of_nothing_but_flowers_gets_a_placer_with_no_mesh_of_its_own() {
        // The water precedent, and the reason `ChunkMesh::is_empty` had to learn about
        // the third half: a chunk with cover and nothing else is not an empty chunk, and
        // the entity exists because the cover has to be placed by something.
        let mut app = headless_world();
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(0, 0, 0),
                runs: solid_chunk(palette::FLOWER_BLUE),
            },
        );
        pump_until(&mut app, "the chunk's mesh", |app| {
            stats(app).meshed_chunks == 1
        });

        assert_eq!(chunk_entities(&mut app), vec![(false, 1)]);
        let cover = app.world().resource::<CoverMaterial>().0.clone();
        assert_eq!(cover_child_materials(&mut app), vec![cover]);
    }

    #[test]
    fn breaking_a_flower_empties_the_cover_half_and_despawns_only_its_mesh() {
        // A `BlockUpdate` to air on the one flower. The chunk keeps its entity because
        // the grass under it is still there; what goes is the cover child.
        let mut app = headless_world();
        push(
            &mut app,
            WorldUpdate::Chunk {
                coord: coord(0, 0, 0),
                runs: meadow_chunk(),
            },
        );
        pump_until(&mut app, "the chunk's mesh", |app| {
            stats(app).meshed_chunks == 1
        });
        let before = stats(&app).total_quads;

        push(
            &mut app,
            WorldUpdate::Block {
                pos: BlockCoord { x: 0, y: 1, z: 0 },
                block_id: palette::AIR,
            },
        );
        pump_until(&mut app, "the remesh", |app| {
            stats(app).total_quads != before
        });

        assert_eq!(
            chunk_entities(&mut app),
            vec![(true, 0)],
            "the flower's child is gone and the grass's entity is not"
        );
        assert_eq!(stats(&app).meshed_chunks, 1);
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
    fn only_the_water_mesh_asset_carries_the_flow_attributes() {
        // The other half of `mesher.rs`'s all-or-nothing invariant, on the assets the
        // renderer actually builds: the water child gets UV_0 and UV_1 — the flow
        // vector and the falling bit the shader reads — and the opaque mesh gets
        // neither, so an opaque vertex is four floats lighter than a water one.
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

        let world = app.world_mut();
        let mut parents = world.query_filtered::<&Mesh3d, With<ChunkMeshEntity>>();
        let opaque = parents
            .iter(world)
            .next()
            .cloned()
            .expect("the opaque mesh");
        let mut children = world.query_filtered::<&Mesh3d, With<ChildOf>>();
        let water = children
            .iter(world)
            .next()
            .cloned()
            .expect("the water mesh");

        let meshes = app.world().resource::<Assets<Mesh>>();
        let opaque = meshes.get(&opaque.0).expect("the opaque asset");
        let water = meshes.get(&water.0).expect("the water asset");

        for (name, id) in [
            ("flow", Mesh::ATTRIBUTE_UV_0.id),
            ("falling", Mesh::ATTRIBUTE_UV_1.id),
        ] {
            assert!(
                water.attribute(id).is_some(),
                "the water mesh is missing its {name} attribute"
            );
            assert!(
                opaque.attribute(id).is_none(),
                "the opaque mesh carries a {name} attribute it never reads"
            );
        }
        assert_eq!(
            water.attribute(Mesh::ATTRIBUTE_UV_0.id).map(|a| a.len()),
            Some(water.count_vertices()),
            "one flow vector per water vertex"
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

    // -----------------------------------------------------------------
    // The measurement harness (#629)
    // -----------------------------------------------------------------
    //
    // Every measurement in this file is `#[ignore]`d, so `cargo test` never times
    // anything and CI never goes red because a runner was busy, and so the numbers in the
    // constants below stay re-derivable rather than becoming folklore. **There is
    // deliberately no count of them here**: this comment said "two" from #629 until #651,
    // by which point #652 had added three more — a hand-kept tally of a set that grows is
    // wrong from the next change onward, and the `measure_` prefix in the command below is
    // the only enumeration that cannot fall behind.
    //
    //     cargo test --release -- --ignored --nocapture measure_
    //
    // **What it cannot see, stated where it will be read.** There is no display and no
    // render app, so `Assets<Mesh>::add` is a resource insert and the GPU buffer upload
    // that follows it never happens. Everything the *main schedule* does is measured;
    // the render world's share of applying a mesh is not, and no number here may be
    // quoted as though it were.
    //
    // **What is measured is printed; what is conserved is asserted.** A frame time is a
    // reading and cannot be a bound on a shared runner — but "every chunk arrived,
    // nothing was refused or evicted, and the bound did not change the world" is exact,
    // so `drain_burst` and `same_world` assert it rather than leaving a regression to be
    // spotted in a printed line. Being `#[ignore]`d, those assertions bind whoever runs
    // the measurement and not CI; for every run the same conservation is pinned by
    // `a_burst_drained_a_chunk_a_frame_loses_nothing_and_refuses_nothing`, which is not.

    /// Run-length encodes a dense voxel array the way the server's `world.Encode` does.
    fn encode_runs(blocks: &[BlockId]) -> Vec<u16> {
        let mut pairs = Vec::new();
        let mut current = blocks[0];
        let mut run: u16 = 1;
        for block in &blocks[1..] {
            if *block == current && run < u16::MAX {
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

    /// The wire's index order: x fastest, then z, then y.
    fn at(size: usize, x: usize, y: usize, z: usize) -> usize {
        (y * size + z) * size + x
    }

    /// What a measurement's surface chunks grow on top of their grass, and the whole of
    /// how #652 attributes the cover half's cost.
    ///
    /// Three plantings rather than two, because "with plants" and "without plants" cannot
    /// separate *shape* from *presence*: a bush was an ordinary opaque cube before #634,
    /// so the world before that change had the same plants standing in the same voxels
    /// and paid the **sweep** for them instead of the cover pass. [`Planting::CubeBushes`]
    /// is that world, and it is the only honest "before" this harness can build without
    /// resurrecting deleted geometry.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Planting {
        /// No plants at all — the control the cover half is attributed against, and what
        /// every measurement written before #652 streamed.
        Bare,
        /// Flowers as they are, bushes as the opaque cube they were before #634: solid,
        /// swept, merged with their neighbours, and culling the grass face beneath.
        ///
        /// **Not a faithful pre-#647 flower**, which was eight quads to today's eleven —
        /// that shape is gone and this harness does not rebuild it. So the distance from
        /// here to [`Planting::Planted`] is #634's bush and nothing else, and the three
        /// quads a flower gained sit inside *both* of those numbers.
        CubeBushes,
        /// The world as it ships: a bush is an arching bramble with leaves along its
        /// canes and flowers at their tips; a flower is a stem, a pair of leaves and a
        /// corolla.
        Planted,
    }

    /// How many plants of each kind every surface chunk carries.
    ///
    /// **Taken from the generated meadow chunk #634 and #647 reported against** — twelve
    /// flowers and nine bushes — so this fixture costs what that chunk costs while
    /// staying a fixture. It *is* a fixture: the world generator is the server's, and a
    /// client measurement that needed a live session could not live in `cargo test`. Any
    /// report quoting a number from here has to say which of the two it is.
    ///
    /// **Exact per chunk, not an average**, and that is the whole reason [`plant_at`]
    /// permutes an index instead of thresholding a hash. A hash gets the density right
    /// across the world and wrong on any one chunk: the first draft of this fixture put
    /// 23 flowers and 5 bushes in the chunk the per-chunk measurement reads, which
    /// under-weights the bush by nearly half — the one shape the whole issue is about.
    const FLOWERS_PER_CHUNK: usize = 12;
    const BUSHES_PER_CHUNK: usize = 9;

    /// How many columns a chunk's surface has. A power of two, which is what lets the
    /// permutation in [`plant_at`] be a permutation.
    const COLUMNS: usize = (SIZE as usize) * (SIZE as usize);

    /// The plant standing on this chunk-local column of the chunk at `(cx, cz)`, if any.
    ///
    /// The column's index is run through a **permutation** of `0..COLUMNS` — xor by a
    /// per-chunk word, multiply by an odd number, add another, all modulo a power of two
    /// — and the first [`FLOWERS_PER_CHUNK`] slots grow a flower while the next
    /// [`BUSHES_PER_CHUNK`] grow a bush. Since a permutation hits every slot exactly
    /// once, every chunk grows exactly that many of each, in places that move from chunk
    /// to chunk. Derived from nothing but coordinates, so a chunk carries the same plants
    /// wherever the walk below reaches it and two runs mesh identical geometry.
    fn plant_at(cx: i32, cz: i32, x: usize, z: usize, planting: Planting) -> Option<BlockId> {
        if planting == Planting::Bare {
            return None;
        }
        let mut hash = (i64::from(cx) as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ (i64::from(cz) as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        hash ^= hash >> 30;
        hash = hash.wrapping_mul(0x94D0_49BB_1331_11EB);
        hash ^= hash >> 31;

        let mask = COLUMNS - 1;
        let index = z * usize::from(SIZE) + x;
        let slot = (index ^ (hash as usize & mask))
            .wrapping_mul(599)
            .wrapping_add((hash >> 32) as usize)
            & mask;

        match slot {
            slot if slot < FLOWERS_PER_CHUNK => Some(match slot % 3 {
                0 => palette::FLOWER_RED,
                1 => palette::FLOWER_YELLOW,
                _ => palette::FLOWER_BLUE,
            }),
            slot if slot < FLOWERS_PER_CHUNK + BUSHES_PER_CHUNK => Some(match planting {
                Planting::Planted => palette::BUSH,
                // The pre-#634 bush: opaque, so the sweep draws it and the grass under
                // it loses the top face a shaped bush leaves in place.
                _ => palette::LEAVES,
            }),
            _ => None,
        }
    }

    /// How many plants of each kind one surface chunk actually grew — counted, never
    /// assumed, so the paragraph above [`FLOWERS_PER_CHUNK`] is a claim something reads.
    fn plant_census(cx: i32, cz: i32, planting: Planting) -> (usize, usize) {
        let size = usize::from(SIZE);
        let (mut flowers, mut bushes) = (0, 0);
        for z in 0..size {
            for x in 0..size {
                match plant_at(cx, cz, x, z, planting) {
                    Some(palette::BUSH | palette::LEAVES) => bushes += 1,
                    Some(_) => flowers += 1,
                    None => {}
                }
            }
        }
        (flowers, bushes)
    }

    /// The fixture's plants standing one block off the floor of an otherwise empty
    /// chunk: the cover pass with no sweep at all in the number.
    fn cover_only_runs(cx: i32, cz: i32) -> Vec<u16> {
        let size = usize::from(SIZE);
        let mut blocks = vec![palette::AIR; size * size * size];
        for z in 0..size {
            for x in 0..size {
                if let Some(plant) = plant_at(cx, cz, x, z, Planting::Planted) {
                    blocks[at(size, x, 1, z)] = plant;
                }
            }
        }
        encode_runs(&blocks)
    }

    /// A chunk shaped like ground a player walks over: a ridged stone surface under a
    /// grass skin, with air above it and whatever `planting` grows on it.
    ///
    /// Deliberately not flat and deliberately not solid. A solid chunk is two `u16` on
    /// the wire and six quads on screen, and measuring against one would understate
    /// every cost here by an order of magnitude.
    fn terrain_runs(cx: i32, cz: i32, planting: Planting) -> Vec<u16> {
        let size = usize::from(SIZE);
        let mut blocks = vec![palette::AIR; size * size * size];
        for z in 0..size {
            for x in 0..size {
                let wx = i64::from(cx) * size as i64 + x as i64;
                let wz = i64::from(cz) * size as i64 + z as i64;
                let height = 12
                    + (wx * 7 + wz * 13).rem_euclid(9) as usize
                    + (wx * wz).rem_euclid(3) as usize;
                for y in 0..height {
                    blocks[at(size, x, y, z)] = palette::STONE;
                }
                blocks[at(size, x, height - 1, z)] = palette::GRASS;
                if let Some(plant) = plant_at(cx, cz, x, z, planting) {
                    blocks[at(size, x, height, z)] = plant;
                }
            }
        }
        encode_runs(&blocks)
    }

    /// One column of the streamed volume: bedrock below, one surface chunk, sky above.
    fn column(cx: i32, cz: i32, radius: i32, planting: Planting) -> Vec<(ChunkCoord, Vec<u16>)> {
        (-radius..=radius)
            .map(|cy| {
                let runs = match cy {
                    ..=-1 => solid_chunk(palette::STONE),
                    0 => terrain_runs(cx, cz, planting),
                    _ => solid_chunk(palette::AIR),
                };
                (coord(cx, cy, cz), runs)
            })
            .collect()
    }

    /// The chunks one chunk-boundary crossing streams at the server's default view
    /// distance of 3 — a 7 x 7 slab, 49 chunks, in `View.MoveTo`'s order.
    ///
    /// `cx` is *which* crossing it is: the walk below steps it, so every slab is terrain
    /// the session has not seen and the store grows the way it does while walking.
    fn boundary_slab(cx: i32, planting: Planting) -> Vec<(ChunkCoord, Vec<u16>)> {
        (-3..=3)
            .flat_map(|cz| column(cx, cz, 3, planting))
            .collect()
    }

    /// The chunks a join streams at the same view distance: the whole 7 x 7 x 7 volume.
    fn join_volume(planting: Planting) -> Vec<(ChunkCoord, Vec<u16>)> {
        (-3..=3)
            .flat_map(|cx| (-3..=3).flat_map(move |cz| column(cx, cz, 3, planting)))
            .collect()
    }

    /// Where a frame went, stamped around the four world systems.
    ///
    /// Four since #651, not three: [`refresh_mesh_stats`] was named in #642's parting
    /// finding as one of the candidates for the remainder, and a candidate that is never
    /// stamped can only ever be argued about. It sums over every meshed chunk once a
    /// frame, so it is the one system here whose cost grows with the *world* rather than
    /// with the burst — which is exactly the shape that hides inside a "remainder".
    #[derive(Resource, Default)]
    struct Phases {
        mark: Option<Instant>,
        ingest: Duration,
        jobs: Duration,
        apply: Duration,
        stats: Duration,
    }

    fn phase_begin(mut phases: ResMut<Phases>) {
        phases.mark = Some(Instant::now());
    }

    fn phase_ingest_end(mut phases: ResMut<Phases>) {
        phases.ingest = phases.mark.take().unwrap_or_else(Instant::now).elapsed();
        phases.mark = Some(Instant::now());
    }

    fn phase_jobs_end(mut phases: ResMut<Phases>) {
        phases.jobs = phases.mark.take().unwrap_or_else(Instant::now).elapsed();
        phases.mark = Some(Instant::now());
    }

    fn phase_apply_end(mut phases: ResMut<Phases>) {
        phases.apply = phases.mark.take().unwrap_or_else(Instant::now).elapsed();
        phases.mark = Some(Instant::now());
    }

    fn phase_stats_end(mut phases: ResMut<Phases>) {
        phases.stats = phases.mark.take().unwrap_or_else(Instant::now).elapsed();
    }

    /// [`world_with_budget`] with a stopwatch around each of the four world systems.
    ///
    /// **The `before` on `phase_apply_end` is load-bearing, and #651 added it.** That
    /// stamp used to say only `.after(apply_finished_meshes)`, which left it unordered
    /// against [`refresh_mesh_stats`] — the two share no conflicting parameter, so the
    /// multi-threaded executor was free to run them at the same time and the `apply`
    /// reading then contained however much of the stats sum happened to have run by
    /// then. That is tolerable while the stats sum is not a number anybody reads. It is
    /// not tolerable once it is the number under examination.
    fn instrumented_world(budget: DecodeTimeBudget) -> App {
        let mut app = world_with_budget(budget);
        app.init_resource::<Phases>().add_systems(
            Update,
            (
                phase_begin.before(crate::world::ingest_world_updates),
                phase_ingest_end
                    .after(crate::world::ingest_world_updates)
                    .before(start_mesh_jobs),
                phase_jobs_end
                    .after(start_mesh_jobs)
                    .before(apply_finished_meshes),
                phase_apply_end
                    .after(apply_finished_meshes)
                    .before(refresh_mesh_stats),
                phase_stats_end
                    .after(refresh_mesh_stats)
                    .before(log_when_meshing_settles),
            ),
        );
        app
    }

    /// One frame's cost, split into the four stamped systems and everything else.
    ///
    /// **"Everything else" is a residue, not a system**, and naming it is the whole
    /// point: it is what `app.update()` cost minus what the four stamped systems cost,
    /// so it carries the command flush at every sync point,
    /// [`log_when_meshing_settles`], `MinimalPlugins`' time and frame-count systems, the
    /// asset plugin's event pumps, and the executor's own per-system overhead. A real
    /// client adds render extraction to that list, and this harness cannot see it — the
    /// note at the head of this section applies here word for word.
    #[derive(Debug, Clone, Copy, Default)]
    struct FrameSplit {
        total: Duration,
        ingest: Duration,
        jobs: Duration,
        apply: Duration,
        stats: Duration,
    }

    impl FrameSplit {
        /// What the four stamped systems cost between them.
        fn measured(&self) -> Duration {
            self.ingest + self.jobs + self.apply + self.stats
        }

        /// `saturating_sub`, because the four spans are stamped from inside the schedule
        /// and the total from outside it. On a frame where every stamped system is
        /// sub-microsecond the two clocks can disagree by less than their own
        /// resolution, and `Duration` subtraction panics rather than reporting anything.
        fn remainder(&self) -> Duration {
            self.total.saturating_sub(self.measured())
        }

        /// Each part's share of the frame as a percentage, in schedule order, remainder
        /// last.
        fn shares(&self) -> [f64; 5] {
            let total = self.total.as_secs_f64().max(f64::MIN_POSITIVE);
            let pct = |part: Duration| part.as_secs_f64() / total * 100.0;
            [
                pct(self.ingest),
                pct(self.jobs),
                pct(self.apply),
                pct(self.stats),
                pct(self.remainder()),
            ]
        }
    }

    /// One line of attribution, the way #642 reported the frame it was about.
    fn print_split(what: &str, split: &FrameSplit) {
        let [ingest, jobs, apply, stats, rest] = split.shares();
        println!(
            "  {what}: {:.3} ms = ingest {:.3} ({ingest:.0}%) | jobs {:.3} ({jobs:.0}%) | \
             apply {:.3} ({apply:.0}%) | stats {:.3} ({stats:.0}%) | everything else \
             {:.3} ({rest:.0}%)",
            ms(split.total),
            ms(split.ingest),
            ms(split.jobs),
            ms(split.apply),
            ms(split.stats),
            ms(split.remainder()),
        );
    }

    /// The two bounds each burst is drained under: the count on its own, which is what
    /// `MAX_DECODES_PER_FRAME` was before #629, and the shipping pair.
    const COMPARED: [(&str, DecodeTimeBudget); 2] = [
        ("count only", DecodeTimeBudget(Duration::MAX)),
        ("metered", DecodeTimeBudget(MAX_DECODE_TIME_PER_FRAME)),
    ];

    fn ms(elapsed: Duration) -> f64 {
        elapsed.as_secs_f64() * 1000.0
    }

    /// What one drain is worth reporting, beyond what [`MeshStats`] already carries.
    ///
    /// **The queue depths cannot be read at the end, and that is why they are here.** A
    /// settled world has `queued` and `in_flight` at zero by definition, so the depth
    /// that means anything is the widest one any frame of the drain saw — which is the
    /// reading #652 asked for, and the only one that can answer whether the queue backs
    /// up. The two mesh times are the other half of that question: how long the burst
    /// took to put its first mesh entity and its last one in the world.
    #[derive(Debug, Clone, Copy)]
    struct Drain {
        stats: MeshStats,
        frames: usize,
        /// The frame the last payload became voxels on. This is the half of "how long
        /// until the world is there" that the expansion budget can move; the frames
        /// after it belong to the mesher and the task pool.
        expanded_on: Option<usize>,
        peak_queued: usize,
        peak_in_flight: usize,
        /// Wall clock from this burst being pushed to its first new mesh entity, and to
        /// its last. `None` when the burst added no mesh at all.
        first_mesh: Option<Duration>,
        last_mesh: Option<Duration>,
        spent: Duration,
        worst: Duration,
        worst_ingest: Duration,
        /// How the worst frame of the drain divided up (#651). `worst` is this split's
        /// `total`; the split is what says who spent it.
        worst_split: FrameSplit,
        /// The median and the worst of [`refresh_mesh_stats`] over every frame of the
        /// drain. The median is what the system costs a frame that is doing nothing
        /// else; the worst is the most it ever cost while the world was growing under
        /// it.
        stats_median: Duration,
        stats_worst: Duration,
    }

    /// Pumps `app` until nothing is outstanding, printing what every frame cost.
    ///
    /// `held` is how many chunks the store must hold once the burst has expanded, and
    /// the mesh times are measured against the meshed count *on entry* rather than
    /// against zero — the walk below drains several bursts through one app, and a
    /// cumulative counter would report the first crossing's meshes for every later one.
    fn drain(app: &mut App, held: usize) -> Drain {
        let pushed = Instant::now();
        let deadline = pushed + PATIENCE;
        let already_meshed = stats(app).meshed_chunks;
        let (mut frame, mut worst, mut spent) = (0usize, Duration::ZERO, Duration::ZERO);
        let mut worst_ingest = Duration::ZERO;
        let (mut peak_queued, mut peak_in_flight) = (0usize, 0usize);
        let (mut first_mesh, mut last_mesh) = (None, None);
        let mut meshed = already_meshed;
        let mut expanded_on = None;
        let mut worst_split = FrameSplit::default();
        let mut stats_times = Vec::new();
        loop {
            let began = Instant::now();
            app.update();
            let total = began.elapsed();
            let phases = app.world().resource::<Phases>();
            let split = FrameSplit {
                total,
                ingest: phases.ingest,
                jobs: phases.jobs,
                apply: phases.apply,
                stats: phases.stats,
            };
            let (ingest, jobs, apply) = (split.ingest, split.jobs, split.apply);
            let seen = stats(app);
            frame += 1;
            spent += total;
            if total > worst_split.total {
                worst_split = split;
            }
            stats_times.push(split.stats);
            worst = worst.max(total);
            worst_ingest = worst_ingest.max(ingest);
            peak_queued = peak_queued.max(seen.queued);
            peak_in_flight = peak_in_flight.max(seen.in_flight);
            if seen.meshed_chunks > meshed {
                meshed = seen.meshed_chunks;
                let elapsed = pushed.elapsed();
                first_mesh.get_or_insert(elapsed);
                last_mesh = Some(elapsed);
            }
            if expanded_on.is_none() && seen.decode_backlog == 0 && seen.chunks_held == held {
                expanded_on = Some(frame);
            }
            println!(
                "  frame {frame:>3}: total {:>8.3} | ingest {:>7.3} | jobs {:>6.3} | \
                 apply {:>6.3} | stats {:>6.3} | rest {:>7.3} | backlog {:>4} queued \
                 {:>3} in flight {:>3} meshed {:>3}",
                ms(total),
                ms(ingest),
                ms(jobs),
                ms(apply),
                ms(split.stats),
                ms(split.remainder()),
                seen.decode_backlog,
                seen.queued,
                seen.in_flight,
                seen.meshed_chunks,
            );
            if !is_busy(&seen) && frame > 2 {
                break;
            }
            assert!(Instant::now() < deadline, "timed out: {seen:?}");
            // The task pool needs a moment, and a spin would starve it on one core.
            // Outside the stopwatch: only `app.update()` is being timed.
            std::thread::sleep(Duration::from_millis(1));
        }

        stats_times.sort_unstable();
        Drain {
            stats: stats(app),
            frames: frame,
            expanded_on,
            peak_queued,
            peak_in_flight,
            first_mesh,
            last_mesh,
            spent,
            worst,
            worst_ingest,
            worst_split,
            stats_median: stats_times[stats_times.len() / 2],
            stats_worst: *stats_times.last().expect("a drain runs at least one frame"),
        }
    }

    /// Streams `burst` in one frame under `budget`, reports what every frame cost, and
    /// returns what the drain left behind.
    fn drain_burst(
        what: &str,
        budget: DecodeTimeBudget,
        burst: Vec<(ChunkCoord, Vec<u16>)>,
    ) -> Drain {
        let mut app = instrumented_world(budget);
        app.update();
        drain_burst_in(&mut app, what, 0, burst)
    }

    /// [`drain_burst`] on an app the caller keeps, and the whole of what #651 needed
    /// that [`drain_burst`] could not give it.
    ///
    /// The measurement there has to pump the *settled* world after the burst has drained
    /// — that is where [`refresh_mesh_stats`] is at its most expensive and every other
    /// world system at its cheapest — and `drain_burst` drops its app on the way out.
    /// `held_before` is how many chunks the store already held, so the conservation
    /// assertion below stays exact on an app that has drained a burst already.
    fn drain_burst_in(
        app: &mut App,
        what: &str,
        held_before: usize,
        burst: Vec<(ChunkCoord, Vec<u16>)>,
    ) -> Drain {
        let chunks = burst.len();
        let held = held_before + chunks;

        for (coord, runs) in burst {
            push(app, WorldUpdate::Chunk { coord, runs });
        }

        let drained = drain(app, held);
        let seen = drained.stats;
        println!(
            "{what}: {chunks} chunks, all expanded by frame {:?}, settled after {} \
             frames, {:.1} ms of main schedule in all\n  worst frame {:.3} ms (worst \
             ingest {:.3} ms); {} held, {} meshed, {} quads, refused {}, evicted {}\n  \
             peak queued {}, peak in flight {}, first mesh {:.1} ms, last mesh {:.1} ms",
            drained.expanded_on,
            drained.frames,
            ms(drained.spent),
            ms(drained.worst),
            ms(drained.worst_ingest),
            seen.chunks_held,
            seen.meshed_chunks,
            seen.total_quads,
            seen.decode_refused,
            seen.decode_evicted,
            drained.peak_queued,
            drained.peak_in_flight,
            ms(drained.first_mesh.unwrap_or_default()),
            ms(drained.last_mesh.unwrap_or_default()),
        );
        print_split("worst frame", &drained.worst_split);

        // The acceptance criteria, checked rather than printed.
        assert_eq!(
            seen.chunks_held, held,
            "{what}: {chunks} chunks streamed onto {held_before} already held, {} held",
            seen.chunks_held
        );
        assert_eq!(seen.decode_refused, 0, "{what}: updates were refused");
        assert_eq!(seen.decode_evicted, 0, "{what}: chunks were evicted");
        assert!(seen.total_quads > 0, "{what}: the burst meshed nothing");
        drained
    }

    /// The bound may change how long a burst takes to drain; it may not change what the
    /// burst leaves behind. Asserted across the runs in [`COMPARED`] rather than left to
    /// whoever reads the printed lines.
    fn same_world(outcomes: &[Drain]) {
        // `windows(2)` over one outcome compares nothing and passes anyway.
        assert_eq!(outcomes.len(), COMPARED.len(), "a bound went unmeasured");
        for pair in outcomes.windows(2) {
            assert_eq!(
                pair[0].stats.chunks_held, pair[1].stats.chunks_held,
                "chunks held differ"
            );
            assert_eq!(
                pair[0].stats.meshed_chunks, pair[1].stats.meshed_chunks,
                "meshed chunks differ"
            );
            assert_eq!(
                pair[0].stats.total_quads, pair[1].stats.total_quads,
                "quad totals differ"
            );
        }
    }

    #[test]
    #[ignore = "a measurement, not an assertion — cargo test --release -- --ignored --nocapture"]
    fn measure_what_one_frames_streaming_budget_costs() {
        // Half one: what the two halves of an expansion actually cost, on a bare store
        // with no ECS around it. `from_runs` is what the budget's own documentation
        // reasons about; `ChunkStore::insert` is the neighbour-staleness scan that
        // follows it, and the point of separating them is that only one of the two was
        // ever in the argument for the number.
        // `Planting::Bare`, which is what this terrain was before #652 gave it plants:
        // the number this measurement recorded for #629 is about the expansion budget,
        // and re-basing it on a different world would silently retire that reading.
        let slab = boundary_slab(0, Planting::Bare);
        println!("a boundary crossing is {} chunks", slab.len());
        println!(
            "  a surface chunk is {} runs on the wire; a solid or empty one is 1",
            terrain_runs(0, 0, Planting::Bare).len() / 2
        );

        // Warm the allocator so the first chunk is not measured cold.
        for (_, runs) in &slab {
            let _ = VoxelChunk::from_runs(runs, SIZE.into()).expect("valid");
        }

        let began = Instant::now();
        let decoded: Vec<_> = slab
            .iter()
            .map(|(coord, runs)| {
                (
                    *coord,
                    VoxelChunk::from_runs(runs, SIZE.into()).expect("valid"),
                )
            })
            .collect();
        let expanding = began.elapsed();

        let mut store = ChunkStore::default();
        let began = Instant::now();
        for (coord, chunk) in decoded {
            store.insert(coord, chunk);
        }
        let storing = began.elapsed();

        println!(
            "  VoxelChunk::from_runs x{}: {:.3} ms ({:.4} ms each)",
            slab.len(),
            ms(expanding),
            ms(expanding) / slab.len() as f64
        );
        println!(
            "  ChunkStore::insert   x{}: {:.3} ms ({:.4} ms each)",
            slab.len(),
            ms(storing),
            ms(storing) / slab.len() as f64
        );

        // Half two: the same crossing through the real pipeline, frame by frame, under
        // each bound in turn. Both runs are in one process on one build, which is the
        // whole point — a before and an after taken from two `cargo test` invocations
        // minutes apart compare the machine's mood as much as the code.
        let mut outcomes = Vec::new();
        for (label, budget) in COMPARED {
            outcomes.push(drain_burst(
                &format!("one boundary crossing, {label}"),
                budget,
                boundary_slab(0, Planting::Bare),
            ));
        }
        same_world(&outcomes);
    }

    #[test]
    #[ignore = "a measurement, not an assertion — cargo test --release -- --ignored --nocapture"]
    fn measure_the_join_burst() {
        // The case the per-frame budgets were set for, and the one the fix must not
        // make materially slower: the whole 7 x 7 x 7 view volume in one burst.
        let mut outcomes = Vec::new();
        for (label, budget) in COMPARED {
            outcomes.push(drain_burst(
                &format!("a join, {label}"),
                budget,
                join_volume(Planting::Bare),
            ));
        }
        same_world(&outcomes);
    }

    // -----------------------------------------------------------------
    // What the cover half costs the pipeline (#652)
    // -----------------------------------------------------------------
    //
    // #634 doubled a chunk's mesh time and said so, and #652 asks the narrower question
    // that number does not answer: meshing runs on `AsyncComputeTaskPool`, so a mesh
    // that takes twice as long is throughput and not a hitch, and what a player can feel
    // is the **queue** — whether `queued` and `in_flight` back up while joining or
    // walking, and how long a chunk waits between arriving and having a mesh entity.
    //
    //     cargo test --release -- --ignored --nocapture measure_
    //
    // Everything the harness at the top of this section cannot see applies here
    // unchanged: no display, no render app, so the GPU upload that follows
    // `Assets<Mesh>::add` is not in any number below. What it *can* see is the whole of
    // what the main schedule does, which is where a hitch would have to live.
    //
    // The three [`Planting`]s are the before, the after and the control, and the terrain
    // is a **fixture** rather than generated: densities from the meadow chunk #634 and
    // #647 measured, geometry from this file.

    /// Every planting, in the order a report reads them.
    const PLANTINGS: [Planting; 3] = [Planting::Bare, Planting::CubeBushes, Planting::Planted];

    /// How many crossings one walk makes. Four, because the reading wanted is a range
    /// across crossings and one crossing is a number.
    const CROSSINGS: i32 = 4;

    /// How many times each join is repeated, for the same reason. A join is also the
    /// burst most exposed to whatever else the machine is doing while it runs.
    const JOINS: usize = 3;

    /// How many times one chunk is meshed for a per-chunk reading.
    const MESH_REPEATS: usize = 64;

    /// Meshes `chunk` [`MESH_REPEATS`] times and answers with the mesh and the fastest,
    /// median and slowest of the runs.
    ///
    /// The mesh each repeat produced is dropped **after** the stopwatch stops: freeing
    /// three vectors is not meshing, and on the planted rows it is not a small share of
    /// what would otherwise be counted.
    fn time_meshing(chunk: &VoxelChunk) -> (ChunkMesh, [Duration; 3]) {
        // Warm the allocator, so the first repeat is not measured cold.
        drop(mesh_chunk(chunk, &Neighbours::default()));

        let mut times = Vec::with_capacity(MESH_REPEATS);
        for _ in 0..MESH_REPEATS {
            let began = Instant::now();
            let built = mesh_chunk(chunk, &Neighbours::default());
            times.push(began.elapsed());
            drop(built);
        }
        times.sort_unstable();
        let spread = [times[0], times[MESH_REPEATS / 2], times[MESH_REPEATS - 1]];
        (mesh_chunk(chunk, &Neighbours::default()), spread)
    }

    /// One row of the per-chunk report: what it cost and what it drew.
    fn print_meshing(what: &str, mesh: &ChunkMesh, [fast, median, slow]: [Duration; 3]) {
        println!(
            "  {what}: {:.3}..{:.3} ms (median {:.3}) | quads: opaque {} water {} cover {} \
             — {} in all",
            ms(fast),
            ms(slow),
            ms(median),
            mesh.opaque.quad_count(),
            mesh.water.quad_count(),
            mesh.cover.quad_count(),
            mesh.quad_count(),
        );
    }

    /// The ranges #652 asked to be reported as ranges.
    fn report_ranges(what: &str, drains: &[Drain]) {
        let span = |pick: fn(&Drain) -> f64| {
            drains
                .iter()
                .map(pick)
                .fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)))
        };
        let (queued_lo, queued_hi) = span(|d| d.peak_queued as f64);
        let (flight_lo, flight_hi) = span(|d| d.peak_in_flight as f64);
        let (first_lo, first_hi) = span(|d| ms(d.first_mesh.unwrap_or_default()));
        let (last_lo, last_hi) = span(|d| ms(d.last_mesh.unwrap_or_default()));
        let (frames_lo, frames_hi) = span(|d| d.frames as f64);
        let (worst_lo, worst_hi) = span(|d| ms(d.worst));
        println!(
            "{what} over {} drains:\n  peak queued {queued_lo:.0}..{queued_hi:.0}, peak in \
             flight {flight_lo:.0}..{flight_hi:.0}\n  first mesh {first_lo:.1}..{first_hi:.1} \
             ms, last mesh {last_lo:.1}..{last_hi:.1} ms\n  settled in \
             {frames_lo:.0}..{frames_hi:.0} frames, worst frame {worst_lo:.3}..{worst_hi:.3} ms",
            drains.len(),
        );
    }

    #[test]
    #[ignore = "a measurement, not an assertion — cargo test --release -- --ignored --nocapture"]
    fn measure_what_a_planted_chunk_costs_to_mesh() {
        // The per-chunk number #634 and #647 reported, re-derivable: how long one surface
        // chunk spends inside `mesh_chunk`, and what its three halves cost in quads.
        //
        // Meshed against [`Neighbours::default`] — no neighbours known — so a border face
        // is emitted rather than culled. That over-draws every row by the same six walls,
        // and the cover half reads no neighbour at all, so the difference between the
        // rows is the plant and nothing else.
        //
        // **This terrain is far more broken than generated ground**, deliberately: #629
        // chose a surface whose height moves under almost every column, which is why its
        // opaque half runs to thousands of quads where a meadow chunk's runs to hundreds.
        // The cover half's *absolute* cost is the same either way — a plant is a plant —
        // but its **share** of a chunk is understated here, so read the last two rows for
        // what the cover pass costs and not the ratio between the first three.
        let (flowers, bushes) = plant_census(0, 0, Planting::Planted);
        assert_eq!(
            (flowers, bushes),
            (FLOWERS_PER_CHUNK, BUSHES_PER_CHUNK),
            "the fixture does not grow the density it says it grows"
        );
        println!(
            "one 32³ surface chunk, meshed {MESH_REPEATS} times; every chunk grows \
             {flowers} flowers and {bushes} bushes, the generated meadow chunk #634 \
             measured"
        );

        let mut meshes = Vec::new();
        for planting in PLANTINGS {
            let chunk =
                VoxelChunk::from_runs(&terrain_runs(0, 0, planting), SIZE.into()).expect("valid");
            let (mesh, spread) = time_meshing(&chunk);
            print_meshing(&format!("{planting:?}"), &mesh, spread);

            // What the fixture must cost, read from the mesher's own constants rather
            // than written down again here — a second literal is the thing those
            // constants exist to stop.
            let expected = match planting {
                Planting::Bare => 0,
                Planting::CubeBushes => flowers * mesher::QUADS_PER_COVER,
                Planting::Planted => {
                    flowers * mesher::QUADS_PER_COVER + bushes * mesher::QUADS_PER_BUSH
                }
            };
            assert_eq!(
                mesh.cover.quad_count(),
                expected,
                "{planting:?}: the fixture's cover half is not what its plants cost"
            );
            meshes.push(mesh);
        }

        // #647's evidence, in the strongest form this fixture can give it. A shaped plant
        // fills nothing and hides nothing, so the sweep over ground that carries plants
        // produces the *same bytes* as the sweep over ground that carries none — not a
        // matching quad count, the buffers themselves. The cube bush is the row where
        // that is false, and it is false by construction: it is opaque, so it is swept
        // and it culls the grass face under it.
        assert_eq!(
            meshes[0].opaque, meshes[2].opaque,
            "a shaped plant moved the opaque half"
        );
        assert_eq!(
            meshes[0].water, meshes[2].water,
            "a shaped plant moved the water half"
        );
        assert_ne!(
            meshes[0].opaque, meshes[1].opaque,
            "a cube bush left the sweep alone"
        );

        // And the cover pass on its own, which is the number the rows above cannot
        // resolve: the same plants in an otherwise empty chunk, against an empty chunk
        // with no plants at all. The difference is `build_cover` and nothing else.
        let empty = VoxelChunk::from_runs(&solid_chunk(palette::AIR), SIZE.into()).expect("valid");
        let (mesh, spread) = time_meshing(&empty);
        print_meshing("air, no plants", &mesh, spread);

        let meadow = VoxelChunk::from_runs(&cover_only_runs(0, 0), SIZE.into()).expect("valid");
        let (mesh, spread) = time_meshing(&meadow);
        print_meshing("air, plants only", &mesh, spread);
        assert_eq!(
            mesh.opaque.quad_count(),
            0,
            "the cover-only chunk swept something"
        );
    }

    #[test]
    #[ignore = "a measurement, not an assertion — cargo test --release -- --ignored --nocapture"]
    fn measure_a_planted_join() {
        // A join: the whole 7 x 7 x 7 view volume in one burst, under the shipping
        // budget, [`JOINS`] times per planting. Every run is in one process on one
        // build — a before and an after taken from two `cargo test` invocations minutes
        // apart compare the machine's mood as much as the code.
        for planting in PLANTINGS {
            let drains: Vec<Drain> = (0..JOINS)
                .map(|run| {
                    drain_burst(
                        &format!("a join, {planting:?}, run {run}"),
                        DecodeTimeBudget(MAX_DECODE_TIME_PER_FRAME),
                        join_volume(planting),
                    )
                })
                .collect();
            report_ranges(&format!("a join, {planting:?}"), &drains);
        }
    }

    #[test]
    #[ignore = "a measurement, not an assertion — cargo test --release -- --ignored --nocapture"]
    fn measure_a_planted_walk() {
        // A walk: [`CROSSINGS`] chunk-boundary crossings through one app, each streaming
        // the slab the server sends for one step, each drained before the next.
        //
        // **Nothing is unloaded behind the player**, which is the one way this is not a
        // walk: the store keeps growing, so the neighbour-staleness scan and the stats
        // sum both cost more each crossing than they would in a session. That is the
        // conservative direction — it can only make the later crossings look worse.
        for planting in PLANTINGS {
            let mut app = instrumented_world(DecodeTimeBudget(MAX_DECODE_TIME_PER_FRAME));
            app.update();

            let mut held = 0;
            let mut drains = Vec::new();
            for cx in 0..CROSSINGS {
                let slab = boundary_slab(cx, planting);
                held += slab.len();
                for (coord, runs) in slab {
                    push(&mut app, WorldUpdate::Chunk { coord, runs });
                }
                let drained = drain(&mut app, held);
                println!(
                    "  crossing {cx}, {planting:?}: settled in {} frames, peak queued {}, \
                     peak in flight {}, first mesh {:.1} ms, last mesh {:.1} ms, worst \
                     frame {:.3} ms, {} held, {} meshed, {} quads",
                    drained.frames,
                    drained.peak_queued,
                    drained.peak_in_flight,
                    ms(drained.first_mesh.unwrap_or_default()),
                    ms(drained.last_mesh.unwrap_or_default()),
                    ms(drained.worst),
                    drained.stats.chunks_held,
                    drained.stats.meshed_chunks,
                    drained.stats.total_quads,
                );
                assert_eq!(drained.stats.chunks_held, held, "a crossing lost a chunk");
                assert_eq!(drained.stats.decode_refused, 0, "updates were refused");
                assert_eq!(drained.stats.decode_evicted, 0, "chunks were evicted");
                drains.push(drained);
            }
            report_ranges(&format!("a walk, {planting:?}"), &drains);
        }
    }

    // -----------------------------------------------------------------
    // What owns the join frame now (#651)
    // -----------------------------------------------------------------
    //
    // #642 fixed the walking hitch and left a finding it did not act on: with the decode
    // spike metered away, the worst *unoptimized* join frame was no longer owned by any
    // of the three world systems, and what was left was named as "the command flush,
    // `refresh_mesh_stats`, and Bevy's own scheduling". That was three candidates and a
    // shrug, taken in a build where #642 had already measured chunk expansion costing ten
    // times what it costs optimized — so the first question is whether the remainder
    // survived #650 giving this crate `opt-level = 1` and its dependencies `3`.
    //
    //     cargo test -- --ignored --nocapture measure_what_owns_the_join_frame
    //     cargo test --release -- --ignored --nocapture measure_what_owns_the_join_frame
    //
    // **Two methods, because "everything else" is not an attribution.** The first is
    // subtraction: [`Phases`] gained a fourth stamp so `refresh_mesh_stats` is measured
    // rather than suspected, and [`FrameSplit::remainder`] is what the frame cost minus
    // the four. The second is the control that turns that residue into a claim — an
    // **idle baseline** on the settled world, [`IDLE_FRAMES`] frames with the burst fully
    // drained, every chunk meshed and nothing outstanding. Those frames run the same
    // schedule, the same command flush and the same executor, and do no join work at all.
    // A remainder that matches the idle frame is the floor the app pays for existing; a
    // remainder materially above it is join work hiding in an unstamped system, and would
    // be the thing to go and find.
    //
    // Everything the harness at the top of this section cannot see applies unchanged, and
    // it bites hardest here: there is no render app, so nothing below contains render
    // extraction, and the residue this measures is the *main schedule's* floor rather
    // than a frame's. A conclusion drawn from it is a conclusion about this repository's
    // systems, which is precisely the question the issue asks.

    // **What it answered, on one 16-core Linux desktop, three joins per planting per
    // profile.** A wall clock on one machine is a reading and not a bound; what is claimed
    // is the shape, not the milliseconds.
    //
    // | | dev (`opt-level` 1 / deps 3) | release |
    // | --- | --- | --- |
    // | worst join frame | 1.715..2.401 ms | 1.395..2.370 ms |
    // | `ingest_world_updates` on it | 1.363..2.078 ms, 79..90% | 0.268..2.064 ms, 19..87% |
    // | `start_mesh_jobs` | 0.025..0.139 ms | 0.042..0.101 ms |
    // | `apply_finished_meshes` | 0.049..0.079 ms | 0.020..0.100 ms |
    // | `refresh_mesh_stats` | 0.003..0.005 ms | 0.002..0.005 ms |
    // | everything else | 0.094..0.191 ms | 0.101..0.161 ms |
    // | a settled idle frame, in all | 0.102..0.160 ms | 0.074..0.161 ms |
    // | `refresh_mesh_stats`, median over a whole drain | 0.0029..0.0038 ms | 0.0024..0.0036 ms |
    //
    // **The remainder #642 was chasing is not there, and the reason is #650.** That
    // finding was taken in a build with `opt-level = 0` for this crate *and* every
    // dependency, where #642 had already measured chunk expansion costing ten times what
    // it costs optimized. Under the profile #650 shipped the two columns above have
    // stopped being different games: the worst join frame is the same 1.4..2.4 ms band in
    // both, because in both it is the frame `ingest_world_updates` spends its 2 ms slice
    // on. The dev-to-release factor on this frame is about **1.0..1.1**, not ten.
    //
    // **And the residue is the floor, measured against the control rather than inferred.**
    // Everything outside the four stamped systems is 0.094..0.191 ms on the worst join
    // frame — and a *settled idle frame*, doing no join work whatsoever, is 0.074..0.161 ms
    // in total, of which 0.051..0.115 ms is itself residue. The join adds nothing to it
    // that can be told apart from the app existing. There is no unstamped system here with
    // a material share, so there is nothing here to reduce.
    //
    // **Two frames in twelve were not the expansion frame at all, and they are the most
    // useful rows in the run.** Once per profile the worst frame of a drain landed
    // mid-mesh: 2.034 ms in the dev run with 1.951 ms of residue, and 1.395 ms in the
    // release run with 1.063 ms. On both, the four stamped systems together cost under a
    // tenth of a millisecond — `ingest` 0.006 ms on the dev one — while 230-odd meshing
    // tasks were in flight on `AsyncComputeTaskPool`. **Nothing in this repository ran on
    // those frames.** A main thread waiting on sixteen cores that are all meshing is the
    // executor and the operating system, exactly the answer #642 guessed at, and it is not
    // addressable by a change to any system named above.

    /// How many idle frames a baseline is taken over.
    ///
    /// Four seconds of 60 Hz, which is enough that the median is a median rather than a
    /// sample and enough that a stray scheduler hiccup lands in the worst without moving
    /// it.
    const IDLE_FRAMES: usize = 240;

    /// Pumps `app` with nothing outstanding and answers the median and the worst of those
    /// frames, ranked by total.
    ///
    /// Two warm-up frames first: the frame immediately after a burst drains still carries
    /// the last of its change detection, and it is not an idle frame however idle the
    /// queues say the world is.
    fn idle_baseline(app: &mut App, frames: usize) -> (FrameSplit, FrameSplit) {
        app.update();
        app.update();

        let mut splits: Vec<FrameSplit> = (0..frames)
            .map(|_| {
                let began = Instant::now();
                app.update();
                let total = began.elapsed();
                let phases = app.world().resource::<Phases>();
                FrameSplit {
                    total,
                    ingest: phases.ingest,
                    jobs: phases.jobs,
                    apply: phases.apply,
                    stats: phases.stats,
                }
            })
            .collect();
        splits.sort_unstable_by_key(|split| split.total);
        (splits[frames / 2], splits[frames - 1])
    }

    /// The attribution #651 asked for, as ranges over repeated runs.
    fn report_attribution(what: &str, drains: &[Drain], idle: &[FrameSplit]) {
        let span = |pick: fn(&Drain) -> f64| {
            drains
                .iter()
                .map(pick)
                .fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)))
        };
        let idle_span = |pick: fn(&FrameSplit) -> f64| {
            idle.iter()
                .map(pick)
                .fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)))
        };
        let (total_lo, total_hi) = span(|d| ms(d.worst_split.total));
        let (ingest_lo, ingest_hi) = span(|d| ms(d.worst_split.ingest));
        let (jobs_lo, jobs_hi) = span(|d| ms(d.worst_split.jobs));
        let (apply_lo, apply_hi) = span(|d| ms(d.worst_split.apply));
        let (stats_lo, stats_hi) = span(|d| ms(d.worst_split.stats));
        let (rest_lo, rest_hi) = span(|d| ms(d.worst_split.remainder()));
        let (share_lo, share_hi) = span(|d| d.worst_split.shares()[4]);
        let (median_lo, median_hi) = span(|d| ms(d.stats_median));
        let (worst_lo, worst_hi) = span(|d| ms(d.stats_worst));
        let (idle_lo, idle_hi) = idle_span(|s| ms(s.total));
        let (idle_stats_lo, idle_stats_hi) = idle_span(|s| ms(s.stats));
        println!(
            "{what} — the worst join frame over {} runs:\n  \
             total {total_lo:.3}..{total_hi:.3} ms\n  \
             ingest_world_updates {ingest_lo:.3}..{ingest_hi:.3} | start_mesh_jobs \
             {jobs_lo:.3}..{jobs_hi:.3} | apply_finished_meshes \
             {apply_lo:.3}..{apply_hi:.3} | refresh_mesh_stats \
             {stats_lo:.3}..{stats_hi:.3}\n  \
             everything else {rest_lo:.3}..{rest_hi:.3} ms — \
             {share_lo:.0}..{share_hi:.0}% of the frame\n  \
             refresh_mesh_stats across the whole drain: median \
             {median_lo:.4}..{median_hi:.4} ms, worst {worst_lo:.4}..{worst_hi:.4} ms\n  \
             a settled idle frame: {idle_lo:.3}..{idle_hi:.3} ms in all, of which \
             refresh_mesh_stats is {idle_stats_lo:.4}..{idle_stats_hi:.4} ms",
            drains.len(),
        );
    }

    #[test]
    #[ignore = "a measurement, not an assertion — cargo test --release -- --ignored --nocapture"]
    fn measure_what_owns_the_join_frame() {
        // The join #642 measured and the join that ships. `Bare` is the world every
        // number in #629 and #642 was taken on, so it is the only row comparable with
        // them; `Planted` is what a player actually joins into since #634 and #647, and
        // it is the row that says whether the answer survives the plants.
        for planting in [Planting::Bare, Planting::Planted] {
            let mut drains = Vec::new();
            let mut idles = Vec::new();
            for run in 0..JOINS {
                let mut app = instrumented_world(DecodeTimeBudget(MAX_DECODE_TIME_PER_FRAME));
                app.update();
                let drained = drain_burst_in(
                    &mut app,
                    &format!("a join, {planting:?}, run {run}"),
                    0,
                    join_volume(planting),
                );

                // The control: the same app, the same schedule, the same command flush,
                // with the join already in the world and no work outstanding.
                let settled = drained.stats;
                let (median, worst) = idle_baseline(&mut app, IDLE_FRAMES);
                println!(
                    "  idle on the settled world — {} chunks held, {} meshed, {} quads",
                    settled.chunks_held, settled.meshed_chunks, settled.total_quads,
                );
                print_split("median idle frame", &median);
                print_split("worst idle frame ", &worst);

                // Idling is not a world event, and the counters say so rather than the
                // paragraph above saying it. Conservation across the baseline, held to
                // the same standard the burst is: nothing arrived, nothing left, nothing
                // was refused or evicted while the stopwatch ran.
                assert_eq!(
                    stats(&app),
                    settled,
                    "{planting:?}: idling changed the world"
                );

                drains.push(drained);
                idles.push(median);
            }
            report_attribution(&format!("a join, {planting:?}"), &drains, &idles);
        }
    }
}
