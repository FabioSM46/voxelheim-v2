# client/ — Rust + Bevy Client

Rendering, input and prediction. Read the root `AGENTS.md` first; it is authoritative for the
pipeline and for the rule that shapes every decision below.

## The client renders; the server decides

Movement validation, combat outcomes, loot rolls, placement legality, durability, respawn — none
of it happens here. The client transports intent to the server and draws the answers that come
back. A gameplay rule implemented client-side is a bug even when it appears to work, because it
is a cheat vector by construction.

Client-side prediction is welcome for feel, and when it lands it will be structured so the
server's answer always wins: a prediction is a guess that gets corrected, never a decision that
gets kept. **Today the client predicts nothing, deliberately** — see "The player" below.

The contract makes this structural rather than aspirational: there is no message in which the
client states its own position. `PlayerInput` carries intent, `EntitySnapshot` carries the
answer.

**The same rule governs changing the world, and there it is stricter.** A click sends a
`BlockEditRequest` and nothing else; the voxel changes when the server's `BlockUpdate` arrives,
through `ChunkStore::apply_block`, which is the only writer of a voxel there is. So **a refused
edit looks exactly like nothing happening, and that is correct** — `schemas/world.fbs` is
explicit that a refusal produces no reply of any kind, not even an error payload.

Prediction is not merely absent here, it is harder than for movement and deserves the same
separate treatment. A predicted position is corrected by the next snapshot, which always
arrives; a predicted edit would have to be un-predicted on a *silence*, so it would need a
deadline to decide the guess had been wrong. That is a design, not a detail. The client that
never guesses needs none of it, and `player/target.rs` is written so there is no code path from
a mouse button to a voxel at all.

**Trusting the server on gameplay is not trusting it on array bounds.** Every number the server
sends that the client divides by, allocates from, or indexes with is validated at the decode
boundary — see `net/codec.rs`. An honestly buggy server reaches a division by zero exactly as
easily as a hostile one.

## Crate layout

One package, and a workspace with one member so that later crates (world streaming, assets)
join it rather than starting workspaces of their own. `cargo <gate> --workspace` therefore
keeps meaning "everything the client is".

| Module | Owns | Must not |
| ------ | ---- | -------- |
| `main.rs` | the Bevy app, plugin registration, CLI/env parsing of the address, `--name` and `--identity` | contain game or network logic |
| `net/mod.rs` | `NetPlugin`, `SignInPlugin`, the channels, `ConnectionState`/`Session`/`ServerAddress`/`SignInState` and the world/snapshot/inventory/mining-progress inboxes | touch a socket, or know about rendering |
| `net/frame.rs` | the length-prefixed framing codec | know what a frame means |
| `net/codec.rs` | FlatBuffers encode/decode, contract limits, `ServerWelcome` validation | know about connections |
| `net/handshake.rs` | the handshake state machine and its admission rules | do I/O, or hold a clock |
| `net/session.rs` | one connection's lifetime; the only code that blocks; the per-server identity file, read before the hello and written after the welcome | mention a Bevy type |
| `net/signin.rs` | one sign-in attempt: the two POSTs, the browser, and the loopback listener that catches the redirect | mention a Bevy type, hold a PKCE verifier, or put `finish_secret` in a URL |
| `net/tickets.rs` | the cached ticket — its file, its mode, its expiry, and the base64url the service answers in | parse a ticket's body, or decide anything from one |
| `net/http.rs` | the smallest HTTP/1.1 the account service needs, plus URL and query shapes | grow into a general HTTP client, or quote a body in an error |
| `net/json.rs` | reading the account service's JSON and the RFC 3339 timestamps inside it | quote its input in an error, or read a nested value |
| `world/mod.rs` | `WorldPlugin`, `ChunkStore`, `DecodeQueue`, the RLE expansion and its invariants, applying a `BlockUpdate`, asking for an evicted chunk back, gathering the six chunks a mesh is culled against | mesh, or spawn anything |
| `world/mesher.rs` | greedy meshing, including the cull against the neighbours it is handed | mention a Bevy type, or read a chunk it was not given |
| `world/render.rs` | the meshing tasks, the mesh assets, one entity per chunk | mesh on the main schedule, or own a camera or a light |
| `world/palette.rs` | block id → colour, and which ids are solid | know about meshes or about the wire |
| `player/mod.rs` | input sampling, the send cadence, one body per entity the server sends, the authoritative vitals and the one gate every playing control is read through | decide where anything is, or decide that a player is alive or dead |
| `player/drops.rs` | one small visual per drop in the newest snapshot, plus local spin and bob | infer pickup, merging, expiry or any other reason a drop disappeared |
| `player/mobs.rs` | one body per mob in the newest snapshot, the species boxes mirrored from the server, and the cosmetic lean and hit flash | read health as death, hold an AI, or advance an action local time did not receive |
| `player/hands.rs` | the camera-space held item and its cosmetic swing/bump | decide item legality, mining progress or any gameplay outcome |
| `player/items.rs` | one row per item id: its display name, its held shape, the palette entry it draws as | hold a capability, a stat, or anything a rule is read from |
| `player/inventory.rs` | the latest complete server-sent slots, the locally selected slot index, and which of the two intents a cell click means | increment, decrement, move or merge a count, or move a durability |
| `player/crafting.rs` | the display-only mirror of the server's recipe table, and the craft intent one row originates | decide that a craft succeeds, consume a material, or produce an item |
| `player/interpolate.rs` | the two-snapshot buffer and the interpolation | mention a Bevy world, or extrapolate |
| `player/camera.rs` | the one camera, and what it follows | decide a gameplay outcome |
| `player/sky.rs` | the one directional light, and the curve the sun, the sky colour, the ambient term and the fog are read from | hold a boundary the server sent, let anything read a rule back out of a colour, or own a light that is not the sky's |
| `player/target.rs` | the voxel raycast, target outline, held mining intent and authoritative progress presentation | apply an edit, compute mining progress, or judge an action legal |
| `player/structures.rs` | the tents, forges and campfires the newest snapshot names, the footprint arithmetic mirrored from the server, the fire's own light, and the two requests that ask for one | stand a structure up locally, decide whether a placement is legal, move one, or let the fire's glow state where the server's safe radius ends |
| `player/constants.rs` | the body's dimensions, the look controls and the aiming reach | hold a number the server owns |
| `ui/icon.rs` | the flat picture each `ItemShape` is drawn as in a cell, and the nodes that draw it | key a drawing on an item id, decide a shape of its own, or load an asset |
| `ui/health.rs` | the health bar, the server's respawn-protection flag and the death overlay with its countdown | hold a timer, run a countdown down, or write any resource |
| `ui/status.rs` | the debug text nodes: connection, world counters, player position, inventory | reach into another module's internals, or grow a health bar |
| `ui/login.rs` | the login screen: one control, the line under it, and when it is up | start a sign-in, hold a ticket, or offer a way past itself |
| `src/gen/` | flatc output | be hand-edited, ever |

The layout deliberately mirrors the server's packages — `frame.rs` ↔ `internal/transport`,
`codec.rs` ↔ `internal/protocol`, `session.rs` ↔ `internal/session`, `world/` ↔
`internal/world`, `player/` ↔ `internal/game` — so a change to the wire format has an obvious
counterpart on each side. The dependency direction is one-way: `ui`, `world` and `player` depend
on `net`, never the reverse, and nothing outside `net` touches a socket.

**Every edge from `player` to `world` is narrow and read-only, and there are four**:
`player/target.rs` reads `ChunkStore`, because aiming is a question about voxels and the store is
the authority on which of those exist; `player/drops.rs` asks `palette` for the colour of an item
the server named; `player/items.rs` names the palette entry each item presents as; and
`player/hands.rs` asks `palette` for the linear colour that entry stands for, plus the one it
draws an empty hand in. None writes world state, and no edge points back from `world` to `player`.
A fifth, in either direction, is a design question rather than an import.

**The third of those is the client's one opinion about what an item looks like, and `ui` reads it
rather than owning a second one.** `items::item_palette_id` answers "which palette entry does this
*item* id present as", and `ui/mod.rs`'s `stack_style` calls it for the pack and hotbar cells. It
used to hand the item id straight to `palette::linear_rgba`, which reads it as a **block** id —
two registries that agree only on stone and dirt, so a log drew snow-white in the pack while it
drew as bark in the hand. One table, every reader, no second edge to `world`. The fourth is the
mechanical half of the same answer and holds no opinion at all: `hands.rs` turns a palette entry
into the material it draws with, exactly as `ui` turns one into a swatch colour.

**`player/target.rs` keeps its tests inline, and it has to.** A submodule of `target.rs` lives in
a directory called `target/`, and `.gitignore` ignores `target/` at any depth because that is
Cargo's build directory. So `src/player/target/tests.rs` compiles perfectly, passes locally, and
is **silently never committed** — CI would then run a suite with no aiming tests in it and report
green. `#[cfg(test)] mod tests { … }` in the same file avoids the directory entirely, which is
also what `net/codec.rs`, `world/mod.rs` and `world/render.rs` do. Only `player/mod.rs` uses a
separate `tests.rs`, and it can because `player/` is not an ignored name.

## The net-thread boundary

A Bevy system must never block, and a socket read blocks by definition. So the socket lives on a
dedicated `std::thread` that owns it exclusively, and the two sides exchange **values** over
`std::sync::mpsc` rather than sharing access:

```
  ECS (net/mod.rs)                        net thread (net/session.rs)
  ────────────────                        ───────────────────────────
  Receiver<SessionEvent>   ◀── mpsc ───   Sender<SessionEvent>
  Sender<NetCommand>       ─── mpsc ──▶   Receiver<NetCommand>
  SyncSender<Vec<u8>>      ─── mpsc ──▶   Receiver<Vec<u8>>  (writer thread)
```

Rules that hold on this boundary:

- **`drain_session_events` uses `try_recv` in a loop and returns.** There is no code path on
  which a Bevy system waits for a network. It drains rather than handling one event per frame,
  so a burst never queues up behind the frame rate.
- **Only decoded values cross.** The thread sends `SessionEvent`, never a frame, a socket or a
  FlatBuffers accessor — an accessor borrows the frame it came from, and frames are transient.
- **An `InventoryState` crosses as one complete value.** The ECS queues it in
  `InventoryInbox`; `player/inventory.rs` keeps only the newest complete state in a frame
  and replaces its resource wholesale. There is no delta path on either side of the
  thread boundary.
- **A `MineProgress` crosses as one complete server answer.** The ECS queues it in
  `MineProgressInbox`; `player/target.rs` displays the exact byte, holds it unchanged during
  brief silence, then clears it. No timer or hardness table can advance it.
- **No Bevy type appears below `net/mod.rs`.** That is what makes `frame`, `codec`, `handshake`
  and `session` testable as plain Rust, with no app to build and no display to open.
- **The thread is stopped by dropping the ECS end of the channels**, not by an atomic flag: the
  `Drop` impl on `Channels` sends `Disconnect` and the channel closing says the same thing to a
  thread that looks between the two. The read timeout in `session.rs` is what bounds how long it
  takes to notice; it is a poll interval, not a session timeout.
- **The `Mutex` around the channels is a type obligation, not synchronisation.** A Bevy resource
  must be `Sync` and both `mpsc` endpoints are `Send`-only. The one accessor takes `ResMut` and
  reaches the contents with `get_mut`, so no lock is ever taken.
- **One reader and one writer per connection, exactly as on the server.** The outbound channel is
  drained by a *second* thread holding its own `try_clone` of the socket, not by a Bevy system: a
  system that writes to a socket is a frame that can stall on a network. The handshake is written
  from the reader thread before the writer thread is started, so there are never two writers at
  once — which is the only arrangement `transport.Conn` promises to survive on the far side.
- **The outbound channel is bounded and lossy, and the other two are not.** It is the only channel
  the ECS *produces* into, and a producer that cannot block has to be able to drop. What waits
  there is input, and an input frame describes the controls *now*: by the time a deep queue
  drained, every frame in it would be describing a tick that had passed. Same reasoning as the
  server dropping a snapshot for a session whose queue is full.
- **`Outbound` exists exactly while there is a thread to send to.** `drain_session_events` removes
  the resource on a terminal event, which closes the channel, which is how the writer thread learns
  to stop and lets go of its socket handle. A sender is therefore an `Option<ResMut<Outbound>>`,
  and its absence means "there is nowhere to send".

## The world: streamed, meshed, drawn

Chunks arrive as `ChunkData`, are expanded into dense voxels, are turned into a mesh off the
main schedule, and become one entity each. These rules hold that pipeline together:

- **`world/mesher.rs` is pure, and its signature says so.** A chunk and the six chunks
  around it in, vertex and index buffers out; no Bevy type, no resource, no `World`. That is
  what lets it run on `AsyncComputeTaskPool`, and what lets its tests assert exact quad
  counts with no app and no GPU. The neighbours are what made that a live question rather
  than a settled one: they are an **input**, gathered by `ChunkStore::neighbours` on the main
  schedule and moved into the task as six more `Arc` handles. A mesher that fetched them
  instead would need the store, and the meshing task could not exist.
- **Nothing meshes on the main schedule.** `start_mesh_jobs` spawns a task and returns;
  `apply_finished_meshes` collects it on a **later** frame with `poll_once`, never a blocking
  wait. A chunk therefore appears a frame or two after its bytes arrive. Both systems are
  capped per frame, because a join streams the whole view distance at once.
- **Nothing expands unboundedly on the main schedule either, and that is the same rule.**
  `VoxelChunk::from_runs` cannot move off the frame — the ordering between a load and the
  unload behind it is the invariant the whole store is built on — so it is metered instead:
  `MAX_DECODES_PER_FRAME` chunks per frame, out of the ordered `DecodeQueue`. The number
  matches `MAX_JOBS_PER_FRAME` because a decoded chunk's next step is a meshing slot, so
  expanding faster only converts a few run-length pairs into 64 KiB of voxels earlier than
  anything can use them. **Unloads cost no budget**: a map removal is not the work being
  bounded, and metering it would let a burst of them defer the loads queued behind.
- **The queue that metering created has a ceiling, and the ceiling is derived.**
  `MAX_DECODE_BACKLOG` is one whole join — `(2 · 8 + 1)³` = 4 913 updates — at a view
  distance the *client* chooses, never `ServerWelcome.view_distance`, because sizing the
  bound from the server's number hands the party being defended against the job of setting
  its own ceiling. A full backlog is 154 frames of decode budget, about 2.6 s at 60 Hz, and
  that wait is the latency the bound trades the process for. The server's default of 3 is
  343 chunks, fourteen joins under it; the protocol ceiling of 16 would be 35 937 updates
  and nineteen seconds, which is a stall rather than a latency. Bounds are justified where
  they are declared here — `view_distance <= 16` in `schemas/handshake.fbs` is the pattern.
- **The newest end gives way, and nothing is admitted over the bound.** Dropping the oldest
  would be the freshness argument, and there is no freshness here: a chunk payload is not a
  keyframe, a view diff sends each coordinate once, and the oldest and newest entries
  describe different parts of the world rather than the same part at different times. What
  is real is the server's ordering — `View.MoveTo` sorts a view update **nearest first** —
  so the oldest queued payload is the ground under the player's feet and the newest is the
  horizon. **What becomes of each kind at the bound is the no-permanent-hole constraint,
  not a preference.** A `Chunk` is refused, and leaves the client in the state a malformed
  payload already does. An `Unload` is *applied* by evicting its coordinate, because the
  server drops an unloaded coordinate from `View.loaded` and never mentions it again, so
  refusing one is this bound's own out-of-memory in slow motion. A `BlockUpdate` is refused
  **and the chunk holding it is evicted** — the answer that took a review round to find,
  after the first version admitted both over the bound and left the queue growable by a
  server sending nothing but edits for held chunks.
- **Evicting a chunk is a smaller loss than keeping a wrong one, and not a faster recovery.**
  The wait is identical either way: the server re-sends a chunk only once its coordinate
  leaves `View.loaded`, which happens when it leaves the view volume. What eviction buys is
  that the divergence stops accumulating — a chunk kept while N edits are refused is wrong
  in N places and nothing records which, where an absent one is wrong nowhere and the copy
  composed next already carries every edit (`BlockApplied::Unheld` states the server-side
  half). It also *shrinks* the backlog, since an eviction drops every update queued for that
  coordinate — which is what closes the growable direction rather than merely naming it.
  `DecodeQueue::admit` carries the whole argument.
- **An evicted chunk is asked for, and the ask goes out where the eviction happens.**
  `ChunkResendRequest` closed the recovery gap this bound used to leave. The client names
  the coordinate; the server owns the repair — `View.Forget` plus a diff at the **current**
  centre — and sends the composed chunk back while the player is still standing on the
  hole, instead of when they next leave the view volume and come back.
  `ingest_world_updates` is where the request goes out, because the eviction is the only
  moment the lost coordinate is known and a reconciliation pass would have to rediscover it.

  **One request per eviction, never a retry, and not for every eviction.** There is no
  reply to wait for — an honoured request is answered by the `ChunkData` that follows, a
  refused one by silence — so a retry could only be a timer guessing at what the silence
  meant. A request lost to a full outbound queue leaves its chunk exactly where the
  eviction left it, which is where every evicted chunk was before this message existed. And
  an eviction that came from an **unload** is not asked for at all: the server drops that
  coordinate from `View.loaded` when it unloads it, so the request would be refused, and it
  would spend a per-session rate limit that the recoverable chunks need.
  `Eviction::resendable` is where that distinction lives, and `request_resends` carries the
  rest of the argument.
- **One task per coordinate, at most.** A chunk the client did not acknowledge is re-sent by
  the server's next view update, so the same coordinate can go stale while its own task runs.
  It waits in `pending` rather than starting a second task that would race the first to the
  same entity.
- **A mesh belongs to the chunk it was built from, and finishing does not make it current.**
  `MeshJob` keeps the `Arc<VoxelChunk>` the task was handed and `apply_finished_meshes`
  compares it against the store's with `Arc::ptr_eq`; a mesh that loses is discarded, and the
  coordinate is still in `pending`, so the current revision is meshed next. "This coordinate
  still exists" is a *different question* from "this is still that chunk", and only the second
  one is the whole answer. Pointer equality is sound rather than merely cheap here: `insert`
  allocates a fresh `Arc` per revision, and the captured handle keeps the old allocation alive
  for exactly as long as the comparison can happen, so no recycled address can make two
  revisions look like one. **The guard used to be invisible** — generation is deterministic, so
  the two revisions of a *re-sent* chunk are byte-identical and applying the wrong one changed
  nothing on screen. Block edits ended that: an edited revision genuinely differs, and applying
  its predecessor draws the hole a player dug as though it were still filled in. It was here
  first on purpose, and `VoxelChunk::with_block` is what keeps it sound — an edit **clones** the
  chunk rather than mutating it, so "a fresh allocation per revision" stays literally true
  instead of depending on a reference count a reader would have to re-derive.

- **A `BlockUpdate` is the only thing that ever changes a voxel, and it costs the same as an
  expansion.** `ChunkStore::apply_block` resolves the world block coordinate with *Euclidean*
  division (`div_euclid`, never `/` — truncation puts every voxel west, south or below the
  origin in the wrong chunk, and only on one side of the world, so half a suite would agree
  with it), replaces the chunk with an edited clone, and logs what went stale. It spends one
  unit of `MAX_DECODES_PER_FRAME`, because a `size³` allocation and copy is the same
  main-schedule work an expansion is. An edit for a chunk this session does not hold is
  **dropped** — the contract permits it, and the server invalidates that chunk's cached payload
  on every edit, so the copy that eventually arrives already carries the change. Dropping costs
  no budget, for the same reason an unload does not.

- **A border face is culled against the neighbour across it**, so two chunks stacked together
  no longer each draw the wall they share. On solid terrain that was the dominant cost: a 3³
  block of solid chunks drops from 162 quads to 54, an 8³ block from 3 072 to 384, and the
  chunks with a solid neighbour on all six faces cost no mesh, no asset and no entity at all
  — 216 of those 512. A line of 50 drops 300 quads to 202.

- **A missing neighbour is a state, not an error.** A chunk whose neighbour has not arrived
  meshes anyway, with the border face *emitted* rather than culled, because a neighbour the
  mesher was not given reads as air. Over-drawing is the direction to be wrong in while the
  data is incomplete: the extra quad is coincident with one the neighbour will draw and
  invisible from outside the pair, where culling against a chunk nobody has seen is a hole a
  player *can* see — permanently, at the edge of the streamed volume, where the neighbour
  never arrives. It is not permanent in the other direction either: the arrival queues the
  remesh that takes the extra quads away.

- **A chunk draws only its own faces.** At a border plane the solid side of a face can be the
  neighbour's, and the neighbour emits that face from its own sweep, at the same world
  position. Emitting it from both sides would put two coincident copies of one quad in the
  world to fight over the depth buffer — the same artifact as the unculled wall, arriving by
  the other door. It is why digging through a chunk wall shows exactly one floor, drawn by
  the chunk that owns the voxel it is cut into.

- **Four things move a chunk's mesh without moving its own voxels**, and all four reach
  `render.rs` as `ChunkChange::NeighbourChanged`: the neighbour was edited on the shared
  border, the neighbour arrived, the neighbour was replaced by a revision whose border layer
  differs, and the neighbour went away. Two mechanisms name them, because they answer the
  same question at different costs. An **edit** knows the voxel that moved, so
  `border_neighbours` names the chunks sharing a *face* with it from its coordinate alone —
  up to three for a corner voxel, six at `chunk_size` 1, and never a diagonal, which shares
  no face and so cannot depend on it — and it names them **only when solidity moved**, because
  the criterion below is the whole rule and stone becoming grass on a shared wall changes
  nothing across it. The edited chunk still remeshes: colour is its own. A **payload off the
  wire** could have moved any of the six border layers, so `ChunkStore::note_neighbours_stale`
  compares them: a neighbour's mesh depends on exactly which of this chunk's boundary voxels
  are solid and on nothing else, and a revision that does not exist compares as all air —
  which is what the mesher reads a missing neighbour as, so "arrived", "replaced" and "went
  away" are one comparison. **Both paths apply the same criterion**, and that is not a
  coincidence to be maintained by hand: the edit path was written without it and remeshed up
  to three neighbours into byte-identical meshes until the review on #66 said so.

  **That comparison is what keeps a join affordable, and it is not an optimisation to trade
  away.** Most of what the server streams is sky, and a chunk of air arriving beside a chunk
  of air changes nothing anyone draws. Invalidating on arrival alone would remesh up to six
  neighbours for each of the 4 913 chunks a join delivers, almost all for a byte-identical
  result — and again on every re-send, which is byte-identical by construction until somebody
  edits the chunk. What survives the comparison is real work: an arrival that genuinely hides
  something does cost its neighbours a remesh, and that is the price of the quads it saves.

  **The remesh does not cascade.** A neighbour arriving remeshes the chunks that border it,
  not the view. Remeshing is `render.rs`'s business and writes nothing back to the store, so
  no invalidation can produce another one. `start_mesh_jobs` treats `Loaded` and
  `NeighbourChanged` identically — the work is the same — and the distinction exists so the
  rule can be asserted rather than inferred from a count.

- **A mesh is checked against the chunk it was built from and *not* against the neighbours it
  was culled against.** The staleness guard above exists because applying revision A's mesh
  over revision B leaves the world one edit behind with nothing queued to correct it; a
  neighbour that moves mid-task has already queued the correction, because the store logged a
  `NeighbourChanged` for this coordinate. Discarding on a neighbour instead would fire
  throughout the one burst where it would matter — a join replaces every chunk's
  neighbourhood several times over — and no terrain would reach the screen until streaming
  settled. What a frame or two shows instead is a border wall drawn when it need not be, or
  missing when it should not be, which is the latency this module is already built on.

- **`ChunkStore` is the authority on what exists.** It keeps an *ordered* change log, not a
  pair of sets: the server unloads a chunk before it re-sends one, and a consumer that learned
  about the two through separate sets could apply them backwards. `DecodeQueue` is ordered for
  the same reason and across the same kinds — an unload that overtook the loads queued ahead of
  it would delete a chunk the session can see.

**Every stage's backlog is on the overlay**, in the order work moves through them: `decode`
has arrived and is not voxels yet, `queued` is voxels waiting for a meshing slot, `meshing` is
off-thread right now. `decode` carries the session's refusal and eviction counts in brackets
beside it, because a backlog holding steady at its bound and one that has finished draining
are the same depth without it — **the cap announces itself**, on the overlay and in a log line
at each edge of the episode. The two counts are separate because they are different losses:
a refusal turns away what had not arrived, an eviction takes terrain the player could see, and
adding them together would hide the second inside the first. All four come from `MeshStats`, so the status line has exactly one
change signal to watch — and none of them may be written on an idle frame, because `ResMut`
marks a resource changed on every `DerefMut`. Four tests exist only to hold that line, and none
of them covers another's resource: `an_idle_frame_marks_neither_the_stats_nor_the_store_changed`
in `world/render.rs`, `an_idle_frame_does_not_mark_the_backlog_changed` in `world/mod.rs`,
`an_idle_frame_does_not_touch_the_stats` in `player/tests.rs`, and
`an_idle_frame_does_not_touch_the_target` in `player/target/tests.rs`. The last is the most
exposed of the four: the aiming raycast runs every frame whether the player moved or not, and its
result feeds a `Transform`, so an unconditional write there would repropagate a transform for the
rest of the session. Each of them observes the change flag from **inside a system**, because
`App::update()` ends every frame with `World::clear_trackers()` — an `is_changed()` check from
outside is always false and would pass whatever the code did.

The **RLE invariants live in `world/mod.rs`**, not in `net/codec.rs`, and that split mirrors
the server: `world.Decode` owns them there too, because the length they are checked against is
`chunk_size` and the frame does not carry it. `codec.rs` owns the envelope and copies the runs
out; `VoxelChunk::from_runs` enforces even length, no zero-length run, and a sum of exactly
`chunk_size³`, sizing its allocation from the validated volume and never from the payload.

The **voxel index order is wire contract**: `index = (y * size + z) * size + x`, x fastest then
z then y, spelled out once in `world::index` and documented in `schemas/world.fbs`. Changing it
is a protocol version bump.

**A malformed chunk is dropped with a warning, not fatal.** The server closes the connection on
one of these; it can, because it is holding the socket. Here the result is a hole in the terrain
with the coordinate and the reason in the log, which beats a client that exits over one bad
frame out of five thousand.

**Colour comes from vertex colours, and there is exactly one material.** `palette.rs` maps a
block id to a linear RGBA; the PBR shader multiplies the material's `base_color` by it, which
is why that colour is white and must stay white. One material means one pipeline for the whole
world. An id this build has no colour for renders magenta rather than a plausible grey — a
server one contract ahead should be obvious, not invisible.

**`world/render.rs` owns the one camera, and it is a `Camera3d`.** Two cameras targeting one
window need explicit ordering and clear-colour configuration to keep one from erasing the
other, and `bevy_ui` renders in the 3D graph as readily as the 2D one — so the status text
draws through this camera and `ui/status.rs` spawns none of its own. It is created at startup so
the status line is visible before a session exists, and moved to `ServerWelcome.spawn` once the
server says where that is.

## The player: intent out, snapshots in

The client samples the controls, sends what the player is *trying* to do at the rate
`ServerWelcome` announced, and draws the answers. Five rules hold that half together:

- **Nothing here decides anything.** No gravity, no collision, no speed clamp that matters. Walking
  into a wall stops the player because the *server* stopped them, and the client finds out when the
  next snapshot says so. The movement axes are sent **un-normalised** on purpose: scaling the
  diagonal is the server's clamp to apply, and a client that did it here would be doing the
  server's job without being any faster for it.
- **Inventory slots are snapshots too.** The client has no `add`, `consume`, move, split or
  merge operation: `Inventory` is exactly the last complete `InventoryState` the server
  sent. A click never touches it. `SelectedSlot` is local input — number keys 1 through 9
  choose a slot index from the server-announced hotbar — and a place request carries only
  that index. The server resolves the item in its authoritative inventory, and its next
  complete state is the only thing that changes the displayed contents.
- **Item drops are snapshot entities, not pickup candidates.** `player/drops.rs` uses the
  newest `drops` vector as the complete existence set and interpolates positions through the
  same two-snapshot buffer as player bodies. Proximity and clicks are not inputs. Spin and bob
  live on a visual child driven by local time, while the parent transform stays exactly at the
  authoritative interpolated position. Inventory and menu modes hide that parent without
  despawning it, so opening UI cannot be mistaken for a pickup.
- **Structures are snapshot entities that never move, and that is why they are not
  interpolated.** `StructureState` carries an anchor cell and a `Facing` — no position and
  no velocity — so `SnapshotBuffer::structures` hands the newest snapshot's list over with
  no `now` and no interval to sample at. There is no call a caller could make that would
  blend a building, which is what keeps one off the entity-motion path by construction
  rather than by discipline. The newest snapshot is the complete existence set, exactly as
  it is for mobs: taken back by its owner, collapsed under a broken block and simply out of
  view are one fact on the wire, and this client does not distinguish them.

  **The footprint arithmetic is mirrored from the server and must stay in step with it.**
  `TENT_FOOTPRINT`, `FORGE_FOOTPRINT`, `CAMPFIRE_FOOTPRINT`, the three headrooms and
  `rotate_offset` in `player/structures.rs` are copies of `tentFootprint`,
  `forgeFootprint`, `campfireFootprint`, `tentHeadroom`, `forgeHeadroom`,
  `campfireHeadroom` and `rotateOffset` in `server/internal/game/structure.go`. The server
  validates the footprint and this side draws it, so a mismatch is a tent that visibly does
  not cover the ground the server says it covers. **The anchor is the ground cell** on both
  sides, and the structure stands in the air above it. The compass is the movement basis —
  North is -Z, East is +X, South is +Z, West is -X — so a yaw of 0 is North, and
  `quantize_facing` resolves the camera's angle once, on the side that has the camera,
  because the contract carries four members rather than a float.

  **One press asks for at most one thing**, and the predicates that keep it that way are
  single functions read from both sides: `combat::blade_in_hand` routes the break press
  between mining and a swing, `HeldItem::structure` routes the place press between a block
  edit and a placement, and `StructureTarget` — which can only ever hold a structure *this
  session owns* — takes the break press away from both mining and the swing. A refused
  placement or removal is silence and nothing appears locally, the same rule a block edit
  already follows.
- **The recipe list is a mirror, and a mirror decides nothing.** `player/crafting.rs`
  carries a display-only copy of `recipeTable` in `server/internal/game/craft.go`, for the
  reason `schemas/player.fbs` gives: the wire carries a `RecipeID` and nothing else — no
  ingredients, no product, no station — so there is no claim here for the server to
  disbelieve, and a drift between the two copies can show a wrong label but can never
  create an item. Graying out a row whose materials are short is a courtesy read from
  `Inventory::count`, exactly as `combat::blade_in_hand` is a courtesy, and the same
  predicate is read by the panel that draws the row and by the sender that declines to ask.

  **Proximity is deliberately not mirrored, and the asymmetry is the whole of the rule.**
  Whether a forge stands within `ForgeCraftRadius` is something the server can see and this
  client can only guess at — the structures a snapshot names are the ones in *view*, not the
  ones that exist — so a forge recipe stays clickable from anywhere and says what it needs
  instead of pretending to know. A courtesy that guessed here would produce the one failure
  a courtesy must never produce: a row refusing a craft the server would have granted. The
  craft itself changes nothing locally; the complete `InventoryState` that follows is what
  moves a count, and a refusal is silence.
- **One cell click, two possible intents, and choosing between them is routing rather
  than authority.** A picked sharpening stone dropped on a slot that wears out sends a
  `RepairRequest` naming the two slots; every other pair sends the `InventoryMoveRequest`
  it always sent. The judgement is read from the durability already beside every stack —
  `max_durability > 0` answers *does this wear out* with no registry and no second copy of
  the server's table — so the only item id in it is the kit's, and that one is presentation
  and routing exactly as `combat::ITEM_RUSTY_SWORD` is. `player/inventory.rs` holds the
  whole decision in one function, `repair_request`, which is the only place a
  `RepairRequest` is built.

  **What it deliberately does not ask is whether the mend would achieve anything.**
  Clicking a stone onto a blade at full durability sends the request and silence answers
  it, because that is the server's decision (#110) and this side's copy of the pack is one
  message old. Neither branch moves a count or a bar: an accepted mend appears in the
  durability vectors of the complete `InventoryState` that follows, and a refusal is
  indistinguishable from nothing happening — the same shape a refused block edit already
  has. The kit **stays picked** after a mend, alone among the two branches, so several worn
  items are several clicks; the cursor still holds no stack and no count, so there is
  nothing for a later state to be shadowed by.

  The one gesture this displaces is swapping a stone stack with a durable item by clicking,
  which the server's `moveLocked` used to answer with a swap. It is still reachable from
  the other side — pick the blade, click the stones — because a picked non-kit never mends.
- **Vitals are snapshots too, and the death countdown is not a timer.** `SelfVitals` is
  exactly the `self_vitals` of the newest snapshot the buffer *accepted* — replaced whole,
  never merged, never incremented, and never advanced by a frame. `respawn_ticks` is
  converted through `ServerWelcome.tick_rate` **for display only**, so when snapshots stop
  the number on screen holds, which is the same rule the interpolation follows for a
  position. A `Timer` here would be the client naming the frame the player comes back, and
  that frame is the server's to name. `None` is the honest encoding of *the server has not
  said yet*; it deliberately does not read as dead, because a client that had heard nothing
  would otherwise lock a living player out of their own controls.
- **Dead input suppression is usability, never authority.** `InputGate` bundles `InputMode`
  and `SelfVitals` so that *may this frame's input act on the world?* has one answer and one
  place to change it: `may_aim` for the continuous questions (the raycast, the outline) and
  `may_act` for the edge-triggered ones (a request leaving this client). Movement axes are
  zeroed rather than the input stream being stopped — `PlayerInput` still has to carry the
  yaw, and going quiet would itself be a decision — and the camera keeps turning, because
  `schemas/player.fbs` names the camera a client concern. The server refuses a dead player's
  request whatever the gate answers; what the gate buys is a client that does not fire
  intent into a refusal. It is read **after** `ApplySnapshots` everywhere, for the reason
  `sample_input` already runs after the UI system that sets the mode: a gate read a frame
  late is a gate that leaks a frame of input.
- **There is no client-side prediction, and that is a decision rather than a gap.** On a local
  server the input latency is imperceptible, and prediction with reconciliation is a design of its
  own — a guess that has to be corrected by an authoritative answer, with all the rollback
  machinery that implies — rather than something to smuggle into a skeleton. It deserves its own
  issue. Until then the client never corrects, rewinds, or overrides a position, and runs no
  collision.
- **The client draws one snapshot interval in the past.** That is what makes the interpolation an
  interpolation: at the instant a snapshot arrives the weight between the two buffered snapshots is
  0, and it reaches 1 exactly when the next one is due. The contract's *"if snapshots stop
  arriving the last known position holds rather than extrapolating"* then falls out of a clamp
  instead of being a special case. The cost is one tick — 50 ms at the server's default rate — and
  it is bought knowingly. Snapshots are timestamped **on the net thread**, where they were decoded:
  the interpolation divides by the gap between two arrivals, and a frame's worth of scheduling
  jitter in that number is a frame's worth of jitter in every position on screen.
- **The camera's position comes from the server and its direction comes from here.** Its
  translation is the authoritative position, interpolated, an eye height above the feet. Its
  rotation is the local look state, applied the frame the pointer moves. That is not an exception
  to the rule — `schemas/player.fbs` says in as many words that "the camera is a client concern",
  and the yaw a snapshot echoes back came from here in the first place. Waiting a tick for that
  echo would put a network round trip on the act of looking around.
- **The aiming ray is an exact grid traversal, and its reach bounds a request rather than an
  edit.** `target::raycast` steps from voxel boundary to voxel boundary (Amanatides & Woo), so it
  visits every voxel the ray passes through, in order, and no others. Marching along the ray at
  fixed intervals is the tempting alternative and it is wrong twice over, in ways that both read
  to a player as "the game ignored my click": a step longer than the thinnest geometry walks
  through a wall, and at a grazing angle the sample that first lands inside a voxel is often not
  the voxel the ray entered first — so the outline sits one block off, and the block that breaks
  is not the one that was lit up. A hit therefore carries the **face** it came in through, which
  a point sample cannot know at all and which is the difference between building on top of a wall
  and building beside it. `MAX_REACH` decides which voxel gets an outline; the server checks its
  own reach against the position *it* computed and refuses anything beyond it in silence, so this
  number must never exceed the server's — a client that reached further would offer outlines on
  blocks that will not break. **The two sides do not measure the same segment**, and matching
  magnitudes hide that: this side measures eye to ray-entry, the server measures body centre to
  voxel centre, and both differences push this side's answer down. Reconciling what is measured
  needs the server half merged and is its own issue; see the note on the constant.

**`player/camera.rs` owns the one camera, and it is a `Camera3d`.** It moved there from
`world/render.rs` when movement landed, because a camera that follows a gameplay entity belongs to
the module that knows where that entity is; `world/render.rs` kept the chunk meshes and their
material. There is still exactly one camera, and that is still a rule: two cameras targeting one
window need explicit ordering and clear-colour configuration to keep one from erasing the other, and
`bevy_ui` renders in the 3D graph as readily as the 2D one — so the status text draws through this
camera and `ui/status.rs` spawns none of its own. `PlayerPlugin` is therefore built **before**
`StatusUiPlugin` in `main.rs`.

**The targeted block is outlined with twelve bars, not a wireframe and not a tinted cube.** A
line-list mesh would need the material's face culling switched off — wgpu rejects a cull mode on
a non-triangle topology — and would be one pixel wide at any distance; a filled overlay would
tint the block and so change the colour `palette.rs` uses to say what the block *is*. Twelve
merged `Cuboid`s go through the same triangle pipeline the terrain already uses, and the block
they mark stays exactly the colour it was. The material is `unlit`, because an outline that faded
on the shaded side of a hill would be least legible where the terrain is hardest to read.

**Two constants are copied from the server and must stay in sync with it**: `PLAYER_WIDTH` and
`PLAYER_HEIGHT` mirror `game.PlayerWidth` and `game.PlayerHeight`. The server collides a box of
that size and this side draws a capsule of it, so a mismatch is a body that visibly does not fit
the space the server says it fits. **The mob bodies are the same mirror, one file over**:
`DRAUGR_BODY` and `VARGR_BODY` in `player/mobs.rs` copy the `body` field of each row in
`server/internal/game/species.go`, and `the_drawn_body_is_the_box_the_server_collides` asserts
it against the *meshes* rather than against the constants, so a part authored at the wrong
offset fails there rather than looking right in a table and wrong on screen. `WalkSpeed`, `Gravity` and `JumpImpulse` are deliberately **not**
copied: nothing here integrates anything, and a duplicated number with no reader is a
synchronisation hazard that buys nothing. Prediction is the issue that will need them, and the issue
that should bring them across. The relationships between the constants that *are* here — eyes inside
the body, capsule inside the collision box, pitch short of vertical — are `const` assertions rather
than tests, because a build should not be able to violate them at all.

## The sky is on the server's clock, and the sun moved here to get to it

`player/sky.rs` owns the one directional light and the curve that four presentation values are
read from: the sun's direction and illuminance, the camera's clear colour, the camera's ambient
term, and the `DistanceFog` on the same camera. Until #171 all four were constants — two in
`player/camera.rs` and two in `world/render.rs` — and `Daylight::FIXED` is those same four
numbers, carried over unchanged.

**The sun moved out of `world/render.rs` for the reason the camera did, one issue later.** A
camera that follows a gameplay entity belongs to the module that reads the snapshots; so does a
sun that follows `EntitySnapshot.tick_of_day`. It was in `world/render.rs` for as long as it was
a constant, which is to say for as long as where it lived did not matter. Moving it is also what
keeps the count of `player` → `world` edges at the four enumerated above rather than adding a
fifth: the alternative was a `world` system reading a `player` resource, which is the same edge
pointing the other way.

**The boundaries are the server's and there is exactly one copy of them.** `ServerWelcome`
carries `day_length_ticks`, `night_start_ticks` and `night_end_ticks`; `net/codec.rs` validates
them together into one `WorldClock` on `Session`, and `sky.rs` reads that every frame rather than
keeping a copy. The only number the client contributes is `RAMP_SECONDS` — how *wide* dusk and
dawn are drawn, which the wire does not carry and which is not a boundary. The reason is the one
the schema gives: the night you see must be the night the server is simulating, because that is
the night its spawn rules use.

**The ramps sit in the daylight, not inside the night.** `[night_start_ticks, night_end_ticks)`
is fully dark, all of it; dusk is the minute *before* `night_start_ticks` and dawn the minute
*after* `night_end_ticks`. That is a gameplay-facing choice rather than an aesthetic one — the
server begins spawning at `night_start_ticks`, so the light has to have gone by then and not
after.

**`SkyClock` is an anchor, not a clock.** It holds the `tick_of_day` of the newest **accepted**
snapshot and the instant the net thread decoded it, and `ingest_snapshots` sets it on exactly the
gate `SelfVitals` rides — `SnapshotBuffer::accept`. So a reordered or duplicate frame cannot run
the sun backwards, for the same reason and through the same test that stops it walking a player's
health backwards. Between anchors the time of day is advanced at the server's `tick_rate`, which
is what keeps a sixty-frame second from stepping twenty times.

**The ambient floor is a playability constant, and what it is *for* is not what it looks like.**
With shadow maps off and no per-voxel light, a face the sun does not reach is lit by the ambient
term and — since #172, and only within a dozen blocks of a campfire — by that fire, so away from
one the ambient term is still the whole of it and `NIGHT_AMBIENT_BRIGHTNESS` decides whether the
far side of a boulder is a dark shape or an invisible one. It does **not** decide how dark night feels: the measured
day-to-night change on the ground a player stands on is sRGB 168 down to 52, and almost all of that
is the sun falling by a factor of five. The floor sits at 480 against a daytime 600 for that reason
— it is set where shaded faces stop being separable from each other, not where the night starts
feeling like night. Its docstring carries the arithmetic (Bevy's ambient term, `Exposure::BLENDER`,
`AcesFitted`) and is honest about what was and was not looked at. **Tune that constant, not the
curve.**

**A server that keeps no clock renders the fixed sky and says so once.** `day_length_ticks == 0`
is a legal announcement, not a missing field, and it is what every server in this repository sends
today. On that path `drive_the_sky` writes neither the sun nor the camera at all — which is both
the required behaviour and the change-detection guard, since writing an unchanged value every
frame would mark two components changed for the rest of the session. The fog is the exception and
is set either way: how far the world is streamed is not a time of day.

## One display registry per item

**`player/items.rs` holds every display fact this client has about an item, one row per id,
and every reader goes through it.** A row is a `name`, an `ItemShape` and a `palette_id`; the
held view model in `hands.rs` is built from the shape and the colour, `ui/mod.rs`'s
`stack_style` draws the colour on a pack or hotbar cell, the recipe panel spells the name, and
a hovered slot reports it. Adding an item to this client means adding a row, and there is
nowhere else to add half of one.

**The split it replaces is worth remembering, because the gap it left was invisible by
construction.** `hands::item_presentation` used to own the shape and the colour and
`crafting::item_label` the name — and `item_label` was written for the recipe panel, so it
named exactly what a recipe mentions. Dirt, snow and the rusty sword had no name at all. The
test over the names could not see that, because it swept the *recipes*: an item nobody crafts
was outside the only thing checking. A tooltip is the first reader that asks about every item
a player can hold, which is why the gap surfaced when it did.

**Two mechanisms keep a row complete, and they cover different halves.** `ItemDisplay` has
three mandatory fields, so a row missing a fact does not compile — that is the shape column
entirely, since `ItemShape` has no unknown variant. What the compiler cannot see is a field
filled in with a placeholder, so `every_known_item_has_a_name_a_shape_and_a_colour` sweeps the
table for a name left at the fallback and a `palette_id` no colour answers to, and
`the_sweep_rejects_a_row_that_is_missing_a_fact` runs that predicate over fixtures so the
sweep's teeth are asserted rather than inferred from rows that already pass. The ids are also
required to be the contiguous `1..N` block an append-only server registry issues, which is what
catches a duplicate and a hole.

**The one direction the table cannot check about itself** is an id declared elsewhere and never
registered. `the_registry_names_every_item_id_this_client_declares` names each one and asserts
the two lists are the same length, so neither can drift without the other failing.

**Where an item id is declared is a rule, not a habit.** An id a module *acts* on is declared by
that module — the blade in `combat.rs`, the three bundles in `structures.rs`, the forge's two
products in `crafting.rs`, and `inventory.rs` reads the kit's from `crafting` rather than
copying the number. Ids nothing acts on, which is every plain block and material, are declared
in `items.rs` because drawing them is all anyone does — and that now includes the three a
hunt puts in the pack: bones, a vargr pelt and a leather patch are drawn and named on this
side and routed nowhere, because what a patch mends is the server's registry. The registry
names all fifteen from wherever they live: one declaration read from several places cannot
drift the way two declarations of the same number can.

**Nothing in a row is ever a gameplay fact, and a fourth field must not make one.** What an item
can do is the server's registry; a client-side copy of it is a cheat vector however carefully it
is written, and drawing an item as a `Blade` deliberately does not make the left button swing it
— `combat.rs` routes on the ids it knows. A later fact that is genuinely presentation (a drawn
icon, a rarity tint) is a fourth field and needs no restructuring to add.

**A tooltip decides nothing either.** `ui/inventory.rs` reads `Interaction` on the cells that
already carry it, writes no message, and touches no resource; hovering therefore cannot become a
request the way a click can. There is one tooltip entity, moved rather than respawned, so
"moving between two slots replaces rather than accumulates" is structural. It is anchored away
from whichever window edge the pointer is nearer — `left`/`top` in the near half, `right`/`bottom`
in the far one — rather than clamped, because a node's width is decided by layout a frame later
and a clamp would have to guess it. Pinning the far edge makes the box grow back into the window,
so it cannot be clipped and nobody has to measure a word.

## What the left button means, and the one table that answers it

`combat::BLADES` is the list of item ids this client routes the left button to a swing for,
and `combat::item_is_a_blade` is the only reader of it. It replaced an `item_id ==
ITEM_RUSTY_SWORD` comparison — one weapon's name spelled inside the routing — for the reason
`armedWithSwordLocked` stopped comparing ids on the server and started reading `meleeDamage`
out of the item registry: **a third blade should be an entry, not an edit to the predicate.**

Three things about it are worth keeping straight.

- **It decides nothing, and its two failure directions are not symmetric.** The server
  re-reads its own registry for every swing, so an id wrongly listed here costs a request
  that is refused — nothing granted, nothing lost. An id wrongly *omitted* costs a weapon:
  that is precisely what the iron sword was between #109 and #127, drawn as a blade in the
  hand, worth 40 damage on the server, and never once asked for, because this client would
  not send the frame. A table that fails open toward asking is the honest shape.
- **`blade_in_hand` is the stack question, `item_is_a_blade` the item question**, and they
  are deliberately separate functions. The stack also has to be there and not worn through,
  where **worn through means zero durability under a non-zero maximum** — the same pair the
  server reads, and never the current value alone. `max_durability > 0` is already this
  client's answer to *does this wear out* (`inventory::repair_request` asks it that way), and
  a weapon registered with no maximum would arrive as `(0, 0)` like every resource does; the
  narrower test would call it broken on arrival and refuse a swing the server would grant.
- **The two opinions about a blade are pinned to each other by a test, not by discipline.**
  `items::ItemShape::Blade` decides which items *draw* as a blade and `combat::BLADES`
  decides which ones *swing*; they lived apart long enough to disagree once.
  `every_item_the_hand_draws_as_a_blade_also_swings` in `player/combat.rs` sweeps a range of
  item ids, reads the mesh the hand is actually built from — written when the shape was private to
  `hands`, and kept because going through the built model checks one thing more than
  reading the table would — and fails when a drawn blade does not route as one. It sweeps a range rather
  than a list on purpose: a hand-written list of today's ids would be a third copy of the
  item table, and the entry it lost would be the new one.

This is one list, not a second registry. The per-item table it was written to fold into now
exists — `player/items.rs`, landed in #128 — so the fold is available rather than hypothetical:
*the left button swings this* becomes a fourth column and `item_is_a_blade` its accessor, with
no call site changed. It is deliberately **not** done here, because a fourth field on
`ItemDisplay` is a gameplay fact sitting in a table whose own rule is that nothing in a row
ever is one. Routing is presentation-adjacent, not presentation; whether that rule bends for it
is a decision, and decisions of that shape get their own issue rather than riding a merge.

## Two renderers, one shape vocabulary

**`ItemShape` is decided once and drawn twice.** `player/hands.rs` builds a mesh per variant for
the held view model; `ui/icon.rs` builds a flat picture per variant for a pack or hotbar cell.
Both read the shape out of the row in `player/items.rs`, so what a player sees in a cell is what
they see in their hand, and neither surface re-decides what an item is. Twelve items share four
pictures, and a thirteenth inherits one by being registered at all — the campfire did exactly
that, arriving as a `Bundle` beside the tent and the forge and needing no new drawing.

**Keying the drawing on the shape rather than on item ids is the whole design.** The alternative
that keeps the two surfaces honest by construction is rendering the held meshes to a texture — and
it costs a camera, a render layer, an image handle per item, framing and lighting decisions, and it
moves the result out of reach of the headless tests this module is deliberately built for. One
table read twice buys the same guarantee with none of that, and the risk it carries — a second
vocabulary drifting from the first — is exactly the risk that keying on `ItemShape` removes.

**What stops a shape going undrawn is the compiler, not a test.** Both renderers match on
`ItemShape` with no wildcard arm, so a fifth variant fails to build until it has been given a mesh
*and* a drawing; there is no branch for it to fall through into a square. `ItemShape::ALL` is a
hand-written list, because no stable Rust enumerates variants — what it buys is the other half, the
one the name sweep established: `every_shape_has_a_drawing_of_its_own` catches an arm answered with
nothing, or with a copy of another shape's picture, and `the_sweep_rejects_a_shape_that_is_not_drawn`
runs that predicate over fixtures so the teeth are asserted rather than inferred. If the list ever
falls behind the enum it costs a sweep some coverage and nothing else.

**A picture is a handful of `bevy_ui` nodes**: rectangles positioned in percentages of the cell,
some rounded, some rotated, each shaded from the item's own colour. No image, no atlas, no asset
pipeline, no dependency — and the result is components a test can read rather than a texture
somebody has to look at. Shading is a **mix** toward white or black rather than a multiply, because
a multiply cannot separate a dark item from itself: three faces of a log at `0.10 / 0.08 / 0.06`
linear read as one flat silhouette, while a mix lifts the lit face away from the base at every
brightness.

**`ui/mod.rs::stack_style` is the one contract both grids go through**, and it now answers with a
plate, a picture and a count instead of a colour and a count. A filled cell is a dark plate — the
colour lives in the picture, because a coloured square behind a coloured icon is the flat fill this
replaced. Empty cells keep exactly the treatment they had. `refresh_cell_contents` writes the
inside of a cell for both grids, so the pack and the hotbar cannot become two answers for one slot;
what stays per-grid is the border, which is the only thing they genuinely disagree about.

**Two traps live in a cell's children, and both are silent.**

- **A node with no `FocusPolicy` blocks.** An icon or a count laid over its own cell takes the
  pointer, the cell falls to `Interaction::None`, and clicking a *full* slot stops working while an
  empty one still does — because only a full one has anything covering it. Every child of a cell
  carries `FocusPolicy::Pass`, the same reason the tooltip does, and
  `a_drawn_cell_still_answers_the_pointer` is what says so.
- **The count is a child now, not the cell.** It used to be a bare `Text` on the cell entity, and a
  node that is itself a text block is no place to hang a picture. It sits bottom-right on a plate
  of its own that appears with the number and grows with it, so three digits stay readable over
  whatever the icon drew underneath. A test that still read `Text` off the cell would pass while
  the screen showed nothing, which is why `drawn_cell` in `ui/mod.rs` walks the children.

**A picture decides nothing.** Drawing an item as a blade no more swings it than holding it as one
does — `combat.rs` routes on the ids it knows, and what an item can do is the server's registry. A
wrong icon draws the wrong picture and has no other effect available to it.

## Conventions that are not obvious from the code

- **`net/codec.rs` is the only place untrusted bytes are read.** It copies every field it needs
  into plain Rust values before returning, so no accessor over a peer's bytes escapes it. Unlike
  the Go runtime, the Rust FlatBuffers runtime ships a verifier — so use `root_as_envelope` and
  **never** `root_as_envelope_unchecked`. `root_as_envelope` does not check the file identifier,
  which is why the tag is tested separately rather than instead.
- **`ServerWelcome` is validated before it becomes a value.** `SessionParams` can only be
  constructed inside `codec.rs`, and construction is gated on every invariant
  `schemas/handshake.fbs` documents. There is no reachable state in which the rest of the client
  holds a `tick_rate` of zero to divide by.
- **Non-finite floats are rejected, never clamped.** `NaN` compares false against every bound, so
  a clamp passes it through untouched and it then propagates through every transform downstream.
- **Check sizes before allocating.** `frame::MAX_FRAME_SIZE` is enforced on the length prefix,
  from the four header bytes alone, before the payload is waited for. The ordering is the
  security property — same rule, same reason as `transport.MaxFrameSize` on the server.
- **`ConnectionState::Rejected` means "no session, and here is why"**; `Disconnected` means "a
  session ended". A `ServerReject`, an unreachable address and a peer that turns out not to speak
  the protocol are all rejections, because in all three the player is looking at a status line
  and needs a reason. After a session exists, the same failures are disconnections and the detail
  belongs in the log.
- **A protocol failure is never a panic.** The net thread reports and returns; the app keeps
  running with the reason on screen. `codec::decode` is total over arbitrary bytes, and the tests
  hold it to that over every truncation and every single-byte corruption of a valid frame.
- **`bevy::log` only** (`info!`/`warn!`/`error!`, re-exported through `bevy::prelude`). No
  `println!` outside the CLI's `--help` and usage output, no `dbg!`.
- **Identities come from the server.** `entity_id` arrives in `ServerWelcome`; nothing here
  invents or edits one.

## Toolchain and dependencies

The toolchain is pinned exactly in `rust-toolchain.toml`, in the same spirit as `.flatc-version`
and the `go` directive in `server/go.mod`. CI pins the matching
`dtolnay/rust-toolchain@<version>` release by full SHA, so moving to a new compiler means bumping
the channel and every workflow action pin together. `Cargo.lock` is committed and every gate
runs `--locked`.

**Three dependencies: `bevy`, `flatbuffers` and `rustls`.** All three are GDD-level
architecture. A fourth needs a discussion before a commit — in particular there is still no
async runtime and no networking framework here, by design: `std::net` plus `std::sync::mpsc` on
one thread is the whole netcode substrate, and it is enough.

That budget is also why signing in brought no crate with it: opening a browser is `xdg-open`
through `std::process::Command`, the loopback listener is `std::net`, and the HTTP, JSON,
base64url and RFC 3339 readers are the narrow hand-rolled ones listed above. The one thing it
genuinely costs is **`https` to the account service**, which is refused rather than downgraded —
see "Known gaps".

**`rustls` is the third, and the discussion the rule asks for is on the record.** Fabio decided
it on 2026-08-20 (issue #157) over the alternative of leaving the wire in the clear and
documenting a WireGuard or VPN deployment. The reasoning is the part worth keeping: a tunnel
protects only when every operator configures one correctly and every player joins it, and it
silently protects nothing when either does not happen — whereas encryption in the transport
protects by default and fails as a refused connection rather than as an unnoticed exposure. The
crate is taken `default-features = false` with `ring` and `std` and nothing else; the reasoning
for the provider, and for leaving `tls12` off, is in `Cargo.toml` beside the entry. The server's
half of this cost nothing: it uses `crypto/tls` from Go's standard library and `server/go.mod`
still has one dependency.

`bevy` is taken with `default-features = false` and an explicit feature list, because the default
`2d + 3d + ui + audio` set drags in glTF, animation, image codecs and rodio — none of which the
client uses, and audio additionally wants ALSA headers at build time. The list in `Cargo.toml` is
Bevy's own curated `ui_api` + `ui_bevy_render` bundles plus **`bevy_pbr`**, which chunk rendering
needs for `StandardMaterial`, `MeshMaterial3d` and `DirectionalLight`. Adding a Bevy feature is
not adding a dependency; adding a crate is.

Two things about that feature choice are deliberate and easy to undo by accident:

- **`bevy_pbr`, not the `3d_bevy_render` bundle.** `Camera3d`, `Mesh3d` and the mesh/material
  plumbing were already present — `ui_bevy_render` brings `bevy_core_pipeline`, and `common_api`
  brings `bevy_mesh` and `bevy_material` — so `bevy_pbr` alone is what was missing. The curated
  3D bundle would also enable glTF, animation, anti-aliasing and post-processing, none of which
  anything renders.
- **No `tonemapping_luts`, and the camera pays for it explicitly.** `Camera3d`'s registered
  default tonemapper is `TonyMcMapface`, which reads a KTX2 lookup texture that only that feature
  ships; without it Bevy logs an error per pipeline and renders through a placeholder. The
  feature would pull `ktx2` and `bevy_image/zstd` into the graph for one texture, so
  `world/render.rs` asks for `Tonemapping::AcesFitted` instead — computed in the shader, no asset
  at all. If that line is ever deleted, the client silently regresses to the error path.

**A Bevy feature can still add a *system* dependency, and that budget is set by CI.** The
`client` job installs `libasound2-dev`, `libudev-dev` and `pkg-config` and nothing else — read
that list from `.github/workflows/ci.yml` before enabling a feature, and treat a successful local
build as no evidence at all: a developer desktop carries development headers a runner does not,
so the two hosts disagree exactly where it hurts. Two checks that do settle it:

```bash
# Does anything in the graph bind a native library? (a manifest `links` key is the marker)
cargo metadata --format-version 1 --filter-platform x86_64-unknown-linux-gnu \
  | python3 -c 'import json,sys; print([(p["name"], p["links"]) for p in json.load(sys.stdin)["packages"] if p.get("links")])'

# Would it build on a host with no discoverable system libraries whatsoever?
env -u PKG_CONFIG_PATH PKG_CONFIG_LIBDIR=/nonexistent CARGO_TARGET_DIR=/tmp/blind \
  cargo build --workspace --locked
```

Both pass on the current feature set, `bevy_pbr` included: no package in the 315-package graph
declares `links`, and the whole client builds from scratch with pkg-config blind — so it needs
none of the three packages at build time. The only link directives any build script emits are
`dl` (glibc) and blake3's own bundled assembly. Re-run both after every feature change; a green
ordinary build proves nothing about the runner.

`flatbuffers` is version-matched (`=`) to the flatc release in `.flatc-version`. The generator and
the runtime move together, and a mismatch is a decode bug that only shows up on the wire.

## The session is encrypted, and that is not a setting

Everything below lives in `net/tls.rs`, beside the session thread that is the only code here
that owns a socket.

- **There is no plaintext path, on either side.** An identity token is a bearer credential:
  whatever can read one off the wire can come back as that player. A switch that turned the
  encryption off would make that exposure a choice somebody makes once and never revisits, and a
  plaintext session looks correct from both ends — so nobody would notice. The server has no
  flag either. This is also why the rule "never present a stored token over an unencrypted
  connection" needs no code: there is no unencrypted connection to present one over. The one
  seam is `session::Transport::Plaintext`, which exists under `cfg(test)` and nowhere else, so
  the guarantee is enforced by the compiler in every build a player runs.
- **Trust on first use, pinned per server address.** There is no domain name and no issuer, so
  web PKI has nothing to attest and the default verifier has nothing to check — the alternative
  to pinning is not "safer validation", it is none. The client records the SHA-256 of the
  certificate the first time it connects to an address and refuses any other one after that.
- **A stored identity is never presented to a server that was never pinned.** The vulnerable
  connection is the first one, and the tempting thing to say is that it carries nothing worth
  taking — a client with no identity file presents an empty token and is minted a new character.
  That is only true of a client that has never played there. **Every player carried over from
  the plaintext transport has an identity file and no pin**, so their first connection after the
  upgrade would have accepted any certificate and then handed it the identity: the weak moment
  and the valuable moment overlapped for exactly the people with the most to lose. That
  connection is refused instead, with a message naming the pin file and the two ways out — write
  the fingerprint the operator reads off their server log into it, or delete the identity beside
  it and join as a new character, which pins safely because a new character has nothing to
  present.
- **A token is only ever kept beside a pin.** A first connection whose fingerprint could not be
  written down does not store the identity it was granted either. Otherwise the next connection
  would hold a token with no pin — and be refused by the rule above, locking the player out of a
  character the previous session had just made.
- **What a changed fingerprint means, and what happens.** It is refused, with a message naming
  the address, the file and both fingerprints — and no bypass flag, no prompt. The two things it
  can mean are "the operator moved the world without `server-key.pem`" and "somebody is standing
  between you and that server", and nothing on this side can tell them apart. Clearing it is
  deleting the pin file by hand, which is a deliberate act taken after asking the operator for
  the fingerprint their server logs at startup.
- **A pin that cannot be read is not a pin.** An unreadable or malformed pin file is an error,
  never "no pin": reading it as absent would silently re-pin whatever answered the next
  connection, which is exactly the substitution the file exists to catch.
- **The pin is a second fact about the same server, in a second file.** Its path is the identity
  file's own with `.pin` after it, so `--identity` moves both — including to somewhere writable
  when the default data directory is not — and the atomic write is `session.rs`'s, reused rather
  than copied. Two characters against one server keep two pins of one fingerprint: redundant,
  harmless, and each verified on its own. A separate file rather than a field inside the identity file, because that
  file is exactly 32 raw bytes: giving it a format would make every file already on disk the
  wrong length, which the reader correctly treats as "not a token" and answers by starting a new
  character. Adding encryption is not a reason to take everybody's character away.
- **The signature check is still rustls's.** Pinning replaces *identity* validation, not the
  proof that the peer holds the key it presented; without `verify_tls13_signature` the handshake
  would accept a certificate copied off the wire by anyone who had watched one.
- **Nothing here implements cryptography.** The fingerprint's SHA-256 comes from the crypto
  provider's own cipher suites — rustls exposes each suite's hash — rather than from a fourth
  crate or from a hand-rolled digest.

## Signing in, once

`net::SignInPlugin` is built **only when `--account-service` names one**, and that is the
conservative half of the feature rather than a limitation: an account service is something an
operator runs and this client cannot invent one, so with no service there is no login screen, no
`SignInState`, and no behaviour change at all. It mirrors `newSignIn` on the other side, which
answers 503 rather than refusing to start.

**The three values this client handles are `state`, `finish_secret` and the ticket.** `state` is
public and travels through the browser. `finish_secret` is private, lives in memory for the length
of one attempt, and never enters a URL, a file or a log — it exists because the provider's redirect
carries `code` and `state` in the *same* URL, so a secret that went through the browser would
protect nothing. The ticket is private and is cached at mode `0600`.

**The PKCE verifier is not one of them**, and that is the correction #122 made: the account service
mints it and the account service redeems the code, because PKCE requires the redeemer to hold the
verifier. It never exists on this machine and nothing here talks to Discord's token endpoint. There
is no client secret in the binary either.

**A sign-in asks for an *account* ticket — one that names no world.** The `finish` body carries
`state`, `code` and `finish_secret` and deliberately no `world` field at all, which is the same
"absent rather than empty" encoding `encode_client_hello` uses for a token nobody holds. A ticket
that names no world is what a player signs in with before they have chosen one, and it is what the
server list reads; a *world* ticket is what joining needs, and choosing the world is the server
list's job.

**Nothing about a ticket reaches the ECS.** `SignInState` is the whole of what leaves `net`, and it
carries a state and a line of text. The ticket lives on the sign-in thread for the length of one
attempt and after that only in the cache — so there is no resource for a `{:?}` to find, and no name
outside `net` anything could start deciding from. That is the fence `PlayerToken` already sits
behind. The server list reads the cache, exactly as a session reads the identity file.

**Where the listener binds is the account service's decision, not this client's.** The redirect URI
is registered with the provider, so `net/signin.rs` reads it out of the `redirect_uri` inside the
authorize URL and binds *that* — after checking it is loopback and plain HTTP, and refusing
otherwise. A listener on a port of its own choosing would be a listener the browser never reaches. A
redirect URI naming port 0 does get an ephemeral port, which is the one case where choosing one
means anything.

**The tab is told what actually happened.** The listener holds the browser's connection until
`finish` has answered, then renders a page saying the sign-in worked or that it did not. Answering
"it worked" the moment the redirect landed would be wrong exactly when it mattered, and a tab left
saying nothing is how a player concludes the game is broken. The pages are self-contained — no
image, no script, no font, no request to anywhere.

**The `state` is compared before anything is sent to `finish`.** A `code` may be redeemed once, so
forwarding a redirect that belongs to a different attempt would spend somebody else's sign-in.

**Four hand-rolled readers, and the dependency budget is why.** `net/http.rs` and `net/json.rs`
carry an HTTP/1.1 client, a URL splitter, percent-decoding, a flat-object JSON reader and an RFC
3339 parser; `net/tickets.rs` carries a base64url decoder. Every one is narrow on purpose and none
is a general facility — the JSON reader refuses nesting rather than skipping it, and the base64url
decoder knows one alphabet and one length. **No error in any of them quotes its input**, because a
`finish` request carries an authorization code and its response carries a ticket, so a diagnostic
built from those bytes is a diagnostic that can carry one into a log. That is the rule
`signin.go` keeps on the other side.

**The login screen owns the input while it is up.** `choose_input_mode` forces `InputMode::Menu`
and `sync_cursor` releases the pointer, because the game is running behind the overlay and a click
meant for the one control must not also reach the world — and a locked, invisible cursor over a
screen whose whole content is one button is a button nobody can press. `Escape` cannot leave it: a
login screen is deliberately not dismissible, and `show_menu` is the other half, so the pause menu
is not drawn underneath.

## Generated bindings

Committed, never hand-edited, regenerated with the flatc release pinned in `.flatc-version` at
the repo root. From the repository root:

```bash
rm -rf client/src/gen
flatc --rust --rust-module-root-file -o client/src/gen -I schemas schemas/*.fbs
flatc --rust --rust-module-root-file -o client/src/gen -I schemas schemas/envelope.fbs
(cd client && cargo fmt --all)
```

Four details in that recipe are load-bearing:

- **`--rust-module-root-file`**: without it, flatc emits one file per schema whose cross-schema
  imports read `use crate::<other>_generated::*` — a glob that imports the namespace *module*
  `voxelheim` and not the types inside `voxelheim::net`, so every cross-schema reference fails to
  resolve and the output does not compile at all. The module-root layout uses `use super::*`
  instead, which is relative and therefore works wherever the tree is mounted.
- **Two invocations, and `envelope.fbs` second**: flatc writes `mod.rs` once per input file
  rather than accumulating it, so the last input wins. The first pass generates every type file;
  the second regenerates the module root from the schema that transitively includes all the
  others, which is the only way `mod.rs` ends up listing the whole contract.
- **`rm -rf` first**: nothing prunes a file whose type was deleted from the contract, and a stale
  `*_generated.rs` still compiles.
- **`cargo fmt --all`**: flatc's output is not rustfmt-clean, and CI's `cargo fmt --all --check`
  covers `src/gen/` because the tree is reachable from the crate root as a real module. Formatting
  is part of generation here, not an edit to generated code — it is deterministic and idempotent,
  so regenerating and reformatting yields no diff. The rule against hand-editing `src/gen/` is
  intact: never fix a binding, always regenerate it.

The tree is mounted in `main.rs` as `#[path = "gen/mod.rs"] mod wire;`. The Rust module is named
`wire` rather than `gen` because **`gen` is a reserved keyword in edition 2024** and `mod gen;`
does not parse; the *directory* keeps the repository-wide `gen/` name that the review bot's
exclusion and AGENTS.md's no-hand-editing rule both key on.

That declaration carries the narrowest `#[allow(...)]` set that silences flatc's output under
`clippy -D warnings`: `unused_imports`, `dead_code`, `clippy::extra_unused_lifetimes`,
`clippy::derivable_impls`. Each is there because flatc emits the pattern that triggers it, and
the reason is recorded next to the attribute. If a contract change makes clippy complain about
something new, add the one lint with a comment — never widen the set, and never disable a lint
workspace-wide.

A `schemas/**` change rebuilds both consumers — CI runs the `schemas`, `server` and `client` jobs
for any contract diff — so regenerate here and in the server in the same PR.

## Running it

```bash
cargo run --release                          # 127.0.0.1:7777
cargo run -- 192.0.2.5:7000                  # explicit address
cargo run -- --server norse.example         # bare host gets port 7777
VOXELHEIM_SERVER=192.0.2.5:7000 cargo run    # lower precedence than the CLI
cargo run -- --name thora                   # display name; VOXELHEIM_NAME is the fallback
cargo run -- --identity /tmp/second         # a second character on one server
cargo run -- --account-service http://127.0.0.1:7780   # sign in with Discord
cargo run -- --help
```

A server has to be listening for any of that to reach one — `go run ./cmd/voxelheimd` from
`server/`, whose own "Running it" section has the flags.

**`--release` is in the first line for a reason.** A debug build of a Bevy renderer is slow
enough that the frame rate reads as a bug in the mesher, and the gates below are the only place
a debug build is the right one.

### Watching what it is doing

Two channels, and neither needs a flag to exist.

`RUST_LOG` selects what reaches the terminal — Bevy's `LogPlugin` reads it, so
`RUST_LOG=info,voxelheim_client=debug` quiets the engine and keeps this crate. The session
thread's own diagnostics arrive here: the welcome it accepted, the clock the server declared,
and any refusal, which is logged rather than panicked over for the reason the status line exists.

The three debug lines in the corner of the window are the other, and they are always drawn:
the connection (and the refusal text, if the server sent one), the streamed world — chunks held,
quads merged, last mesh duration — and where the **server** says the player is, which is the one
number that says movement is working end to end. All three are pure functions of resources, which
is why `ui/status.rs` can test what a player would read without opening a window.

### When it refuses to connect

The refusal is the certificate pin doing its job, and it names the file it is talking about. See
"The session is encrypted, and that is not a setting" above for why there is no bypass flag; the
operational half is short:

- **A fingerprint that changed** — the server was pointed at a new world directory, or is running
  `-world-dir ""` and mints a new certificate every start. Development hits the second one most.
- **An identity with no pin** — a character made against a server from before pinning. The stored
  token is not presented to an unverified certificate, so this is refused on the *first*
  connection rather than a later one.

Whoever runs the server reads the fingerprint out of its startup line
(`certificate_sha256=…`) and writes that one line into the pin file the refusal names:

```bash
printf '%s\n' "<certificate_sha256 from the server log>" \
  > "${XDG_DATA_HOME:-$HOME/.local/share}/voxelheim/identity/127.0.0.1_7777.pin"
```

Writing it in is the remedy for both cases, and the only one that keeps the character. Deleting a
file instead is narrower than it looks, because the two are checked independently —
`ConnectError::Unverified` comes from the guard before the handshake, `ConnectError::Substituted`
from the verifier during it:

- Deleting the *identity* file answers the second case and only the second: with nothing to
  present, the first connection pins safely. It does nothing about a fingerprint that changed —
  `read_pin` still returns the old one and `PinnedServer` still refuses it.
- Deleting the *pin* is what addresses a changed fingerprint, and it re-pins whatever answers
  next, so it is right only when you already know why the fingerprint changed. With an identity
  file still beside it that is the second case again; delete both to join as a new character.

### Who the client comes back as

The server issues an identity token in `ServerWelcome`; the client stores it and presents it
in the next `ClientHello` to **that** server. One file per server address —
`$XDG_DATA_HOME/voxelheim/identity/<address>`, falling back to `$HOME/.local/share` — because
a token is meaningful only to the server that minted it: presenting server A's token to server
B makes a new character on B, and B's answer must not land in A's file.

Four rules hold that down, and they are all in `net/session.rs`:

- **The token is never logged, printed or shown, at any level.** It is a bearer credential —
  whatever holds one *is* that player — so `PlayerToken` writes its own `Debug` and prints
  `<redacted>`, which makes the redaction a property of the type rather than a habit every call
  site has to remember. There is no `Display` at all. The wire is closed by the encryption above;
  a log file is closed here, and the two are different exposures with different fixes.
- **Nothing is decided from it.** It is read, presented, and stored. The one thing derived from
  it is whether the welcome's token is the one presented, which the status line renders as
  `returning` or `new character`; the server had settled the identity before it answered.
- **A missing or unreadable file is a first connection, never a failure** — but only one of
  those two is then replaced, and the difference is whether the bytes were *read*. A
  wrong-length file has been seen not to be a token, so the welcome's token overwrites it. A
  file that would not open at all might still hold a good identity — a permission left behind
  by one `sudo` run, a transient I/O error — so it is left exactly as it is, and the player who
  fixes the permission gets their character back.
- **Written atomically and only when it differs** — temp file in the destination directory,
  flush, rename, mode `0600` on Unix; the same shape as the server's `writeAtomic`, for the same
  reason. A returning session therefore writes nothing at all.

`net/session.rs` has no logger of its own — no Bevy below `net/mod.rs` — so a warning crosses the
thread boundary as a `SessionEvent::Warning` value and `net/mod.rs` logs it. That is what lets a
test read one back instead of hoping a global logger was installed.

CI's `client` job installs `libasound2-dev`, `libudev-dev` and `pkg-config`; the feature set needs
none of them at build time (see "Toolchain and dependencies"). What it does need is a **runtime**
X11 library, because the `x11` feature reaches libX11 through `x11-dl`, which dlopens it when a
window is created. That is why CI is unaffected: the gates build and run headless tests, and no
test opens a window.

On a Wayland session the window goes through XWayland. That is deliberate — see "Known gaps".

## Gates

Run from `client/`, and all four before opening a PR:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --locked
cargo test --workspace --locked
```

CI runs exactly these. `cargo fmt --all` and the clippy messages are how you fix the first two;
formatting is the gate most often skipped and the one that most often reddens CI.

The tests need no display, no GPU and open no window, and that is a rule rather than a
coincidence — a gate CI cannot run is not a gate. Three patterns keep it true:

- `net/mod.rs` builds the app with `MinimalPlugins` plus `NetPlugin` and drives it against an
  in-process stub server over a real loopback socket.
- `world/render.rs` adds `MinimalPlugins` + `AssetPlugin` and `init_asset::<Mesh>()` /
  `init_asset::<StandardMaterial>()`. `Assets<T>` is an ordinary resource, so the whole pipeline
  short of the GPU upload — tasks, mesh assets, entities, despawns, counters — runs with no render
  app at all.
- `world/mesher.rs` and `world/palette.rs` need none of that: they are plain Rust.

Anything that meshes should be assertable as an exact quad count. If a new test needs a screen to
tell whether the mesher is right, the property being tested has not been found yet — `winding` is
the worked example: it is checked as a cross product against the normal attribute, because
inside-out terrain looks exactly like no terrain. Cross-chunk culling is the second: "the seam is
invisible" is a screenshot, and "a chunk enclosed by solid neighbours emits nothing, and the floor
of a hole dug through a wall is drawn once, by the chunk that owns it" is a test.

**A *remesh* is asserted as a change of entity identity, not of quad count.** `apply_finished_meshes`
despawns a chunk's previous entity and spawns a new one, so a coordinate's `Entity` changes exactly
when it is remeshed — including when the new mesh is byte-identical to the old one, which is what a
quad count cannot see. Since border culling landed, a remesh usually *does* move the count, and the
tests assert both where they can: the count says what was drawn, the entity says what was rebuilt,
and "only the affected chunk was remeshed" needs the second. `mesh_entities` in `world/render.rs`'s
tests is the helper.

## Known gaps, deliberately

Recorded here so the next reader does not mistake them for oversights:

- **A first connection is trusted, and there is no out-of-band way to check it.** Trust on
  first use is what pinning is, and its one weak moment is the connection that establishes the
  pin. Nothing here lets a player type in a fingerprint an operator gave them beforehand, so a
  first connection through a hostile network pins whatever answered it. What limits the damage
  is that a first connection carries no token to steal — see the section above — and what would
  close it is a way to state the expected fingerprint before connecting. That is its own issue.
- **The two stacks are checked meeting, but by hand and not by CI.** `scripts/interop-check.sh`
  drives the real client against the real server: a first connection established and pinned, a
  reconnect returning as the same character, a substituted certificate refused before anything
  is sent, and a stored identity never presented to a server that was never pinned. It is not in CI because the client opens a window and needs a display, and
  because the Go and Rust gates run in separate jobs with separate toolchains — no job has both
  binaries. **Run it after touching `internal/transport`, `internal/certs` or `net/tls.rs`.**

  It is worth saying what that script caught the first time it ran, because it is the argument
  for keeping it: every unit test on both sides passed while the client discarded whatever one
  `read_tls` call did not take out of a socket read. A socket read and a TLS record are
  different units — a busy server routinely fills one read with several records — so the stream
  desynchronised the moment the world started streaming, and the session died with "cannot
  decrypt peer's message" a few milliseconds after it was established. Neither side could see
  it alone: the Go tests drove Go's own client, and the Rust tests never ran a handshake.
- **No native Wayland: the client is X11-only, and on a Wayland desktop it runs through
  XWayland.** Bevy's `wayland` feature pulls `wayland-sys`, whose build script needs
  `wayland-client.pc` from **`libwayland-dev`** — a package CI's `client` job does not install,
  and issue #6 put that job's setup out of scope. Enabling it therefore takes two changes in one
  PR: add `libwayland-dev` to the `Install Bevy system dependencies` step in
  `.github/workflows/ci.yml`, then add `"wayland"` to the feature list in `Cargo.toml`. Doing only
  the second reddens CI with `Package 'wayland-client' ... not found` — and it will still build
  fine on your machine, which is the trap. It deserves its own issue rather than a drive-by.

- **The account service is reached over plain HTTP, and `https` is refused rather than
  downgraded.** Verifying a web PKI certificate needs a root store, and `rustls` is taken here
  without one — `webpki-roots` or `rustls-native-certs` would be a fourth crate. So
  `AccountService::parse` turns an `https://` URL away with a message saying why, because a client
  that silently spoke plaintext to a URL that said `https` would be the worst of the three
  outcomes. `voxelheim-auth` serves plain HTTP today (`srv.Serve(ln)`, no TLS), so this matches
  what exists; what it means operationally is that the account service belongs on a loopback
  address or behind a private network until the crate discussion happens. It deserves its own
  issue rather than a drive-by.
- **A sign-in caches an account ticket and nothing presents one yet.** `ClientHello.session_ticket`
  is still `None` in `net/session.rs`: a ticket that names no world cannot join a world — the game
  server would answer `ErrWrongWorld` — and the screen that turns an account ticket into a world
  ticket is the server list. That is #107, and it reads the cache.
- **The loopback listener binds a fixed port, so two clients cannot sign in at once.** The port is
  the account service's `redirect_uri`, which the provider requires to match exactly; a second
  client signing in at the same moment finds the port taken and says so. The refusal names it.
- **A redirect URI naming `localhost` binds whichever of `::1` and `127.0.0.1` resolves first.**
  `TcpListener::bind` takes one address, and a browser may connect to the other. Naming the
  literal address in the service's `-discord-redirect-uri` avoids it entirely, which is what its
  own default does.
- **No sign-out and no account switching.** Deleting the cached ticket is sign-out; the usage text
  says so and `--account-service` pointed somewhere else is a different file.
- **No reconnect, backoff or session resumption.** A refused or dropped connection is reported
  and stays reported. `Reject` carries the reject code's name for display; a reconnect policy is
  the thing that would want to branch on the numeric code, and it can widen that struct.
- **`MAX_DECODE_BACKLOG` bounds chunk payloads and nothing else, so a flood of `BlockUpdate`s
  for chunks this session *holds* still grows the queue.** Each costs a decode budget unit and
  none may be refused, because nothing re-sends one. It is a narrower door than the one that was
  closed — it needs a server editing chunks the player is standing in, and an edit is four
  integers where a payload is a heap allocation — and the fix is a different mechanism: an edit
  *is* superseded by a later edit to the same voxel, so coalescing by coordinate loses nothing,
  where a refusal would. Asking the server to slow down is the other candidate and it is a
  protocol change. Stated rather than traded away, in `DecodeQueue::admit` as well as here.
- **The pointer is not captured, so turning stops when it leaves the window.** Mouse motion drives
  the look state while the pointer is over the window and nothing recentres it. Cursor capture is
  fiddly and platform-specific — `CursorGrabMode::Locked` is unsupported on X11, and `Confined`
  stops generating motion at the window edge — so it belongs with the camera-control issue rather
  than as a drive-by here.
- **First person, and the local player has no body.** The camera sits at its eyes, so a capsule
  there would fill the screen with the inside of the player's own head. A third-person or orbit
  camera is what would want one, and that is a camera issue.
- **Other players are coloured capsules and nothing else.** No animation, no name plate, no
  distinction between a player and anything else the server might send. Art assets and a character
  rig are later issues; the colour is keyed on `entity_id` so two players are at least told apart.
- **No cross-chunk lighting, ambient occlusion, shadows, LOD or frustum-driven requests.** One
  directional light with shadow maps off, plus a per-camera ambient term and a per-camera distance
  fog — all three on the server's clock — and one `PointLight` per campfire in view, which is on
  nobody's clock and casts no shadow either. **The fire's light is the one light in this client
  that is not the sky's**, and its reach is a presentation number that deliberately falls short of
  `game.CampfireSafeRadius`: the ground a fire keeps clear is checked in `spawn.go` and is not
  something a renderer may draw a boundary around. None of the four casts a shadow. Each of the rest is its
  own issue. Ambient occlusion is the one that moved closer: it needs a chunk's neighbours, and
  border culling put them in the mesher's hands. What to *do* with them is still a rendering
  decision rather than a plumbing one, and greedy merging works against per-vertex occlusion —
  which is exactly the design that issue owes.
- **No texture atlas, no UVs.** `palette.rs` is the whole material system: a colour per block id,
  carried as vertex colours. Art assets are a later issue.
- **One connection per process, opened when the plugin is built.** There is no lobby, no server
  browser and no way to change the address without restarting.
- **Interpolation holds the last position for ever when a server goes quiet.** There is no timeout
  that fades an entity out, and none that says "this session is stale": a quiet server is a
  legitimate state, and the read timeout in `session.rs` is a poll interval rather than a session
  timeout. Deadlines belong to the same issue on both sides.
