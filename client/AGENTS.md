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
| `main.rs` | the Bevy app, plugin registration, CLI/env parsing of `--account-service` and its `--account-service-fingerprint`, the development address, `--world`, `--name` and `--identity` | contain game or network logic, or admit a combination of address, service and world that is not one of the three launches on `Start` |
| `player/appearance.rs` | the rig: which box each of the six appearance colours covers, and where each box sits in notches of the collision box | hold a size of its own, or become a second answer for either renderer |
| `net/mod.rs` | `NetPlugin`, `SignInPlugin`, `ServerListPlugin`, the channels, `ConnectionState`/`Session`/`ServerAddress`/`SignInState`/`ServerList` and the world/snapshot/inventory/mining-progress inboxes | touch a socket, or know about rendering |
| `net/frame.rs` | the length-prefixed framing codec | know what a frame means |
| `net/codec.rs` | FlatBuffers encode/decode, contract limits, `ServerWelcome` validation | know about connections |
| `net/handshake.rs` | the handshake state machine and its admission rules | do I/O, or hold a clock |
| `net/session.rs` | one connection's lifetime; the only code that blocks; the per-server identity file, opened only for a server the list named | mention a Bevy type, or open an identity file for an unlisted server |
| `net/signin.rs` | one sign-in attempt: the two POSTs, the browser, and the loopback listener that catches the redirect | mention a Bevy type, hold a PKCE verifier, or put `finish_secret` in a URL |
| `net/servers.rs` | one read of the server list: the ticket it presents, the rows it validates, and the address and fingerprint it keeps out of the ECS | shorten a list it could not read whole, answer a failure with an empty list, or expose an address |
| `net/tls.rs` | the encrypted transport, the certificate check and the two shapes of expectation | write a fingerprint anywhere, or offer a way past a refusal |
| `net/tickets.rs` | the cached ticket — its file, its mode, its expiry, and the base64url the service answers in and reads back | parse a ticket's body, or decide anything from one |
| `net/http.rs` | the smallest HTTP/1.1 the account service needs, its pinned-TLS transport, plus URL and query shapes | grow into a general HTTP client, quote a body in an error, or gain a way to reach a service unencrypted |
| `net/json.rs` | reading the account service's JSON, the one array of flat objects the server list is, and the RFC 3339 timestamps inside it | quote its input in an error, or read anything nested deeper than that one array |
| `world/mod.rs` | `WorldPlugin`, `ChunkStore`, `DecodeQueue`, the RLE expansion and its invariants, applying a `BlockUpdate`, asking for an evicted chunk back, gathering the six chunks a mesh is culled against | mesh, or spawn anything |
| `world/mesher.rs` | greedy meshing, including the cull against the neighbours it is handed | mention a Bevy type, or read a chunk it was not given |
| `world/render.rs` | the meshing tasks, the mesh assets, one entity per chunk | mesh on the main schedule, or own a camera or a light |
| `world/palette.rs` | block id → colour, and which ids are solid | know about meshes or about the wire |
| `player/mod.rs` | input sampling, the send cadence, one body per entity the server sends, the authoritative vitals and the one gate every playing control is read through | decide where anything is, or decide that a player is alive or dead |
| `player/drops.rs` | one small visual per drop in the newest snapshot, plus local spin and bob | infer pickup, merging, expiry or any other reason a drop disappeared |
| `player/mobs.rs` | one body per mob in the newest snapshot, the species boxes mirrored from the server, and the cosmetic lean, hit flash and death fall | read health as death, hold an AI, or advance an action local time did not receive |
| `player/hands.rs` | the camera-space held item, its cosmetic swing/bump, and the mining punch the server's progress starts and stops | decide item legality, mining progress or any gameplay outcome |
| `player/items.rs` | one row per item id: its display name, its held shape, and the block-derived or item-only colour it draws as | hold a capability, a stat, or anything a rule is read from |
| `player/inventory.rs` | the latest complete server-sent slots, the locally selected slot index, and which of the three intents a cell press means | increment, decrement, move or merge a count, move a durability, or decide that a stack may be put down |
| `player/crafting.rs` | the display-only mirror of the server's recipe table, and the craft intent one row originates | decide that a craft succeeds, consume a material, or produce an item |
| `player/interpolate.rs` | the two-snapshot buffer and the interpolation | mention a Bevy world, or extrapolate |
| `player/camera.rs` | the one camera, and what it follows | decide a gameplay outcome |
| `player/sky.rs` | the one directional light, and the curve the sun, the sky colour, the ambient term and the fog are read from | hold a boundary the server sent, let anything read a rule back out of a colour, or own a light that is not the sky's |
| `player/target.rs` | the voxel raycast, target outline, held mining intent and authoritative progress presentation | apply an edit, compute mining progress, or judge an action legal |
| `player/structures.rs` | the tents, forges and campfires the newest snapshot names, the footprint arithmetic mirrored from the server, the fire's own light, and the two requests that ask for one | stand a structure up locally, decide whether a placement is legal, move one, or let the fire's glow state where the server's safe radius ends |
| `player/constants.rs` | the body's dimensions, the look controls and the aiming reach | hold a number the server owns |
| `settings/mod.rs` | what a player may change: the mouse sensitivity, the key bindings and the one rule that refuses a rebinding rather than leaving a control unreachable, the six graphics values and the frame-rate readout — each with its bound, its step and the default it starts from | reach the wire, take a value from something the server sent, or decide any outcome |
| `settings/store.rs` | the settings file — its path under the data directory, its text format, and the temporary-file-and-rename that replaces it | refuse to start over a line it cannot read, hold a bound of its own, or let a test build ask where the data directory is |
| `ui/icon.rs` | the flat picture each `ItemShape` is drawn as in a cell, and the nodes that draw it | key a drawing on an item id, decide a shape of its own, or load an asset |
| `ui/health.rs` | the health bar, the server's respawn-protection flag and the death overlay with its countdown | hold a timer, run a countdown down, or write any resource |
| `ui/status.rs` | the debug text nodes: connection, world counters, player position, inventory — and the frame-rate readout in whichever of the four corners the setting names | reach into another module's internals, grow a health bar, or call the snapshot age a round trip |
| `ui/login.rs` | the login screen: one control, the line under it, and when it is up | start a sign-in, hold a ticket, or offer a way past itself |
| `ui/servers.rs` | the server list screen: a row per server, the retry, the line under them, and when it is up | learn a server's address, open a socket, or draw an empty list for a list it could not read |
| `ui/character.rs` | the character screen: the rows, the creation draft, the stated palettes, the live preview, and the launch that answers it from `--name` | decide whether a name may be worn, invent a colour the contract does not allow, or enter a world before the welcome |
| `ui/settings.rs` | the settings screen behind the pause menu: the rows, the steppers, the two flags and the corner, the rebinding capture and the refusal it prints | hold a bound, a step or a default of its own, narrow the set of keys the model offers, or leave a control with no key |
| `src/gen/` | flatc output | be hand-edited, ever |

**`settings/` is a leaf, and the direction around it is what keeps it one.** `player` and
`ui` both *read* it — the sensitivity and the bindings in `sample_input`, the fog, the render
distance and the brightness in `player/sky.rs`, the readout in `ui/status.rs` — and only `ui`
writes it, from the one screen. It imports nothing from either, and it imports from `net`
exactly one thing: `MAX_VIEW_DISTANCE`, the protocol's ceiling, so the render distance can
never ask for more chunks than any server could stream. **That is a bound, not a value**, and
the distinction is the whole of the rule: `ServerWelcome.view_distance` is never read into a
setting, and `no_setting_is_sourced_from_anything_the_server_sent` is what keeps that true as
the module grows. **The default a setting starts from is
why that is stated here rather than assumed** — `DEFAULT_LOOK_SENSITIVITY` began in
`player/constants.rs`, which made this module import `player` while this file said it did
not, and a documented invariant contradicted by the code beneath it is worse than either
half alone. A bound, a step and a default are one statement about one setting and they live
together. The single exception is a *test*: the pitch limit is `player`'s build invariant
and stayed there, so `the_pitch_limit_holds_at_every_sensitivity_this_screen_offers` reads
it across the module line to hold that no sensitivity this screen offers can reach past it.
The `net` half is the structural half of "nothing the server sent becomes a preference", and
`no_setting_is_sourced_from_anything_the_server_sent` keeps it true as the module grows.

The layout deliberately mirrors the server's packages — `frame.rs` ↔ `internal/transport`,
`codec.rs` ↔ `internal/protocol`, `session.rs` ↔ `internal/session`, `world/` ↔
`internal/world`, `player/` ↔ `internal/game` — so a change to the wire format has an obvious
counterpart on each side. The dependency direction is one-way: `ui`, `world` and `player` depend
on `net`, never the reverse, and nothing outside `net` touches a socket.

**Every edge from `player` to `world` is narrow and read-only, and there are two**:
`player/target.rs` reads `ChunkStore`, because aiming is a question about voxels and the store is
the authority on which of those exist; `player/items.rs` asks `palette` for a terrain swatch when
an item deliberately reuses one. The first-person hand takes its skin colour from the local
player's server-sent `Appearance`, not from a terrain approximation. Neither edge writes world
state, and no edge points back from `world` to `player`. A third, in either direction, is a design
question rather than an import.

**The second of those is the client's one opinion about what an item looks like, and every
renderer reads it rather than owning a second one.** `items::item_linear_rgba` answers which
linear colour an *item* id presents as: a block-like item may reuse a real block swatch, while an
item with no honest block counterpart names an item-only colour in the same row. The block id
space stays entirely the wire's — no client-only reservation can collide with a block the server
appends later. `ui/mod.rs`, `hands.rs` and `drops.rs` all consume the resolved colour. The UI used
to hand an item id straight to `palette::linear_rgba`, which reads it as a **block** id — two
registries that agree only on stone and dirt, so a log drew snow-white in the pack while it drew
as bark in the hand. One table and one resolved answer keep all three surfaces together.

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
  brief silence, then clears it. No timer or hardness table can advance it. Everything else
  reads the `MiningFeedback` resource that holds it and never the inbox — the outline's
  colour, `ui/crosshair.rs`'s ring and `player/hands.rs`'s mining punch — which is what
  makes them one answer that starts, holds through the same silence, and stops together.
  **The punch is the one of the three that could have been written from local input**, and
  deliberately was not: a hand that swung on the button press would be animating a break the
  server had not granted, which is advancing progress locally wearing a different hat.
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
  **The character phase is what makes "before" load-bearing rather than incidental**: the reader
  thread now waits in the middle of the handshake for a person to choose, and the choice reaches
  it as a `NetCommand`, so the writer thread is still started at `Established` and the frame that
  answers the list is still written by the one thread that wrote the hello.
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
  to three neighbours into byte-identical meshes until the review on legacy PR 66 said so.

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
- **One cell press, three possible intents, and choosing between them is routing rather
  than authority.** A picked sharpening stone dropped on a slot that wears out sends a
  `RepairRequest` naming the two slots; a shift-click sends a `DropItemRequest` naming one;
  every other pair sends the `InventoryMoveRequest`
  it always sent. The judgement is read from the durability already beside every stack —
  `max_durability > 0` answers *does this wear out* with no registry and no second copy of
  the server's table — so the only item id in it is the kit's, and that one is presentation
  and routing exactly as `combat::ITEM_RUSTY_SWORD` is. `player/inventory.rs` holds the
  whole decision in one function, `repair_request`, which is the only place a
  `RepairRequest` is built.

  **What it deliberately does not ask is whether the mend would achieve anything.**
  Clicking a stone onto a blade at full durability sends the request and silence answers
  it, because that is the server's decision (legacy PR 110) and this side's copy of the pack is one
  message old. Neither branch moves a count or a bar: an accepted mend appears in the
  durability vectors of the complete `InventoryState` that follows, and a refusal is
  indistinguishable from nothing happening — the same shape a refused block edit already
  has. The kit **stays picked** after a mend, alone among the two branches, so several worn
  items are several clicks; the cursor still holds no stack and no count, so there is
  nothing for a later state to be shadowed by.

  The one gesture this displaces is swapping a stone stack with a durable item by clicking,
  which the server's `moveLocked` used to answer with a swap. It is still reachable from
  the other side — pick the blade, click the stones — because a picked non-kit never mends.

  **The drop is the third, and it is the one that pairs with nothing.** `drop_request` in
  `player/inventory.rs` asks two things and no third: is the index one the contract permits,
  and does the last complete state show something in that cell. It deliberately does *not*
  predict whether the server will accept a slot — that is a gameplay outcome read from a
  pack one message old, and it is the failure direction `combat::BLADES` records, where a
  courtesy that guesses wrong refuses what the server would have granted. A worn blade is
  therefore asked about like anything else; acceptance arrives only through the complete
  inventory and the snapshot's sparse authoritative durability entry. The branch also runs
  ahead of the cursor and leaves it untouched: a picked slot is a source waiting for a
  destination, and a shift-click elsewhere is not that destination.

  **Shift is read against the full-stack button and not the split one** (`ui/inventory.rs`),
  because what the modifier changes is *where the stack goes* rather than *how much of it
  moves*.
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
that size and this side draws a body inside it, so a mismatch is a body that visibly does not fit
the space the server says it fits. They are also the **grid a character is cut on** — see "The rig"
below — so a change to either moves every part of every character with it. **The mob bodies are the same mirror, one file over**:
`DRAUGR_BODY` and `VARGR_BODY` in `player/mobs.rs` copy the `body` field of each row in
`server/internal/game/species.go`, and `the_drawn_body_is_the_box_the_server_collides` asserts
it against the *meshes* rather than against the constants, so a part authored at the wrong
offset fails there rather than looking right in a table and wrong on screen. `WalkSpeed`, `Gravity` and `JumpImpulse` are deliberately **not**
copied: nothing here integrates anything, and a duplicated number with no reader is a
synchronisation hazard that buys nothing. Prediction is the issue that will need them, and the issue
that should bring them across. The relationships between the constants that *are* here — eyes inside
the body, pitch short of vertical — are `const` assertions rather than tests, because a build should
not be able to violate them at all. **The body's own proportions used to be among them and are not
any more**: they were expressible while a body was one capsule inscribed in the collision box, and a
rig of a dozen-odd boxes says things a `const` expression cannot, so `player/appearance.rs` asserts
them as tests instead.

## The rig: a body cut from the same grain as the world

A character is thirteen axis-aligned boxes and a haircut — between one box and four — authored in
`player/appearance.rs` on a grid of **notches**: a twelfth of `PLAYER_WIDTH` across and a
thirty-sixth of `PLAYER_HEIGHT` up, which is the same length on both axes and 0.05 blocks at the
server's numbers. Terrain is cut at one block and a body at a twentieth of one — fine enough for a
fist, coarse enough that nothing reads as smooth. Nothing here is written in metres, because the box
belongs to the server and a character with more hair is not a taller character.

**Two renderers read it, and that is the whole point.** `ui/character.rs` draws the boxes head-on as
`bevy_ui` nodes so a player can see what they are choosing; `player/mod.rs` merges the same boxes
into meshes for the bodies the snapshots drive. Two tables would be two answers to "what does a shirt
colour cover", and the first thing two answers do is disagree.

**Ten meshes for a whole settlement.** Every player is the same geometry and only the colours differ,
so a part is merged into one mesh at startup — five for the parts whose shape is fixed and one per
hair model — and nothing is ever rebuilt. Materials are keyed on the wire's colour itself, so two
players in the same walnut tunic share one `StandardMaterial` and the palettes bound how many there
can be; the map is swept whenever a body leaves, because a server is free to describe a colour nobody
can choose and sixteen million of those is a map rather than a palette.

Four rules hold the numbers together:

1. **Parts interpenetrate; they never merely touch.** A boot swallows a leg, a leg the tunic, the
   neck both. **Nothing checks this one**, and it is written down rather than asserted for the
   reason rule 2 is asserted: what a player would see is rule 2 failing, and a part that merely
   meets its neighbour is one edit away from that rather than already there. Two parts *do*
   legitimately meet along an edge — the trousers and a fist, at the hip — so a test could not tell
   the difference without a list of exceptions nobody would keep.
2. **No two faces of different colours land on the same plane where they overlap.** Coplanar faces
   of different materials fight for the depth buffer and flicker at exactly the distance a body is
   hardest to read. It is a property of the numbers rather than of the renderer, so
   `no_two_colours_share_a_plane` checks it rather than hoping — over a *positive* area, because
   two boxes that share only an edge cover none of it and reporting that as a flicker would be a
   finding somebody had to silence.
3. **Detail sits half a notch proud of what it wraps** — the hair on every face, the eyes on the
   face they look out of. The hat-layer trick every blocky model has used since Minecraft, and what
   lets a cap wrap a head without sharing a plane with it.
4. **The body keeps the box the server collides.** What reaches past it is the arm and the hair,
   and neither is collided, because neither is a gameplay fact: a sleeve by a notch on each side and
   a fist by two, and a topknot three and a half notches above the crown. Twelve notches across
   cannot hold a torso, two legs and two visible arms; Minecraft's own model runs four times further
   past its hitbox than this one does. `the_body_keeps_the_box_the_server_collides` holds the table
   of what may leave it and by how much — **"nothing leaves the box" would be false and "something
   leaves it" would be unfalsifiable**, so what is pinned is which parts and by how far.

**The axes are the model sheet's and not Bevy's, and that is reconciled in exactly one function.**
`z` is measured along the way a character faces, where a body at yaw 0 faces `-Z` here — so the
numbers in the table read against the sheet they were drawn from, and `appearance::placed` is the
one place a sign is applied. `what_faces_the_viewer_is_nearer_than_what_is_behind_it` is what would
catch that negation going missing.

**The eyes are the one part nobody chooses**, and the one colour this side is entitled to decide.
The contract carries five colours; a sixth would be a colour the server stores, so the eyes are a
constant of the model instead. They cost one part and buy the thing a silhouette cannot: a face
reads at four times the distance, and it is what makes the front of a character legible together
with the two notches of toe the boots run past the legs.

## The sky is on the server's clock, and the sun moved here to get to it

`player/sky.rs` owns the one directional light and the curve that four presentation values are
read from: the sun's direction and illuminance, the camera's clear colour, the camera's ambient
term, and the `DistanceFog` on the same camera. Until legacy PR 171 all four were constants — two in
`player/camera.rs` and two in `world/render.rs` — and `Daylight::FIXED` is those same four
numbers, carried over unchanged.

**The sun moved out of `world/render.rs` for the reason the camera did, one issue later.** A
camera that follows a gameplay entity belongs to the module that reads the snapshots; so does a
sun that follows `EntitySnapshot.tick_of_day`. It was in `world/render.rs` for as long as it was
a constant, which is to say for as long as where it lived did not matter. Moving it is also what
keeps the count of `player` → `world` edges at the three enumerated above rather than adding a
fourth: the alternative was a `world` system reading a `player` resource, which is the same edge
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
term and — since legacy PR 172, and only within a dozen blocks of a campfire — by that fire, so away from
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
and every reader goes through it.** A row is a `name`, an `ItemShape` and an `ItemColour`; the
held view model in `hands.rs` is built from the shape and the colour, `ui/mod.rs`'s
`stack_style` draws the colour on a pack or hotbar cell, the recipe panel spells the name, and
a hovered slot reports it. Adding an item to this client means adding a row, and there is
nowhere else to add half of one.

**The held view model is a hand-and-item composition, not an exclusive choice between them.**
`hands.rs` puts the same fist at the origin for an empty hand and every `ItemShape`, then places a
block, material or bundle on its knuckles and a blade or tool through its grip. The two are merged
into one mesh and carry absolute vertex colours — skin from the local player's `Appearance`, item
from this table — under one white material. One stable mesh asset is rebuilt in place only when
the selected item or skin colour changes, so arbitrary server colours cannot grow a cache and all
three swing shapes still move one transform.

**The item colour source is deliberately beside the block palette rather than inside its id
space.** `ItemColour::Block` reuses an existing terrain swatch for a block-like item;
`WornSteel` and `ForgedSteel` are client-only presentation colours for things no block honestly
represents. `item_linear_rgba` resolves both, and unknown item ids still use the block palette's
loud magenta placeholder. Adding a client-only integer to `world/palette.rs` would make that
integer collide with a future wire block unless somebody remembered a reservation forever; this
shape gives the two registries different types instead.

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
table for a name left at the fallback and a block-derived colour no palette entry answers to, and
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
  that is precisely what the iron sword was between legacy PRs 109 and 127, drawn as a blade in the
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
exists — `player/items.rs`, landed in legacy PR 128 — so the fold is available rather than hypothetical:
*the left button swings this* becomes a fourth column and `item_is_a_blade` its accessor, with
no call site changed. It is deliberately **not** done here, because a fourth field on
`ItemDisplay` is a gameplay fact sitting in a table whose own rule is that nothing in a row
ever is one. Routing is presentation-adjacent, not presentation; whether that rule bends for it
is a decision, and decisions of that shape get their own issue rather than riding a merge.

## Two renderers, one shape vocabulary

**`ItemShape` is decided once and drawn twice.** `player/hands.rs` builds geometry per variant for
the held view model; `ui/icon.rs` builds a flat picture per variant for a pack or hotbar cell.
Both read the shape out of the row in `player/items.rs`, so what a player sees in a cell is what
they see in their hand, and neither surface re-decides what an item is. Items share five pictures,
and a new item inherits one by being registered at all — the campfire did exactly that, arriving
as a `Bundle` beside the tent and the forge and needing no new drawing.

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

**A body is the same shape of answer, and `player/appearance.rs` is where it is decided once.**
Five colours cross the wire and six parts wear them — see "The rig" below for the sixth — and the
table says which part takes which field and where each of its boxes sits, in notches of the
collision box rather than in metres. `ui/character.rs` draws those boxes flat as `bevy_ui` nodes for
the preview, exactly as `ui/icon.rs` draws an `ItemShape`; `player/mod.rs` merges the same boxes into
meshes for the bodies in the world. Two tables would be two answers to "what does a shirt colour
cover", and the first thing two answers do is disagree.

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
it on 2026-08-20 (legacy issue 157) over the alternative of leaving the wire in the clear and
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
- **The expectation comes from the server list, and trust on first use is gone.** There is no
  domain name and no issuer, so web PKI has nothing to attest and the default verifier has
  nothing to check — the alternative to checking a fingerprint is not "safer validation", it is
  none. What the client checks is the `certificate_sha256` the account service's list carried for
  that server, read on every launch and never written down.

  This client used to record whatever certificate answered a first connection and compare against
  that afterwards, in a `.pin` file beside the identity. **That file, its reader and its writer
  are removed rather than left as a fallback**, and the removal is the point: two ways to decide
  who a server is means the weaker one decides whenever the stronger is unavailable, and "the
  list could not be read" is exactly the moment an attacker would like the weaker one reached
  for. An unreadable list is a screen with a retry on it and no address at all.
- **A stored identity is never presented to a server nobody stated a certificate for.** The rule
  predates the list and had to survive it, so it is now structural rather than a check.
  `tls::Expectation` has three shapes: `Listed(fingerprint)`, built only by `net/servers.rs` from
  a row of the list; `Supplied(fingerprint)`, built only by `signin::AccountService` from the
  launch (#131) and never reaching a game server; and `Unlisted`, which carries nothing because
  nothing stated anything. `session::run` opens the identity file on `Listed` and on nothing else
  — so **the variant that omits the fingerprint is the variant that omits the credential**, and
  there is no ordering to get right and no flag to forget. `Supplied` is matched there
  explicitly rather than folded into a wildcard, so a fourth variant one day is a compile error
  at the line that decides which credential may be presented. `net/mod.rs` pins it from the wire in both directions: the
  same file and the same token is presented to a listed server and is not presented to an
  unlisted one.
- **`Unlisted` is `--server`, and it is the development path.** An address typed on the command
  line is in no list, so the session is encrypted and unauthenticated, and it presents no
  identity and stores none. It is never what a failed list read falls back to: a session is
  `Unlisted` because the command line named an address, never because a list could not be read.

  **It does present a session ticket, and that is what #154 changed.** `--server` and
  `--account-service` were mutually exclusive until then, on the argument above — nothing stated
  a certificate, so nothing should be handed over. The argument was right about the credential it
  was written for and does not carry to a ticket: a ticket names one world, expires in hours, and
  `Verify` refuses it at any other world, so an unverified address learns one world's session for
  an afternoon at an address the developer typed, where a stored identity would have been that
  player at that server until somebody deleted the file. The two are therefore gated differently
  and deliberately: the identity file is opened only for `Listed`, and `session::Target::ticket`
  is whatever the launch decided regardless of the expectation. **What did not move is the
  certificate** — `Unlisted` still verifies nothing and still says so, in the usage text and on
  the status line.

  The combination is a launch of its own rather than a precedence rule: `--server` with
  `--account-service` also requires `--world`, because `finish` mints a ticket for a named world
  and an address states no world. `net::SignInPlugin::for_world` carries the argument for why
  nothing infers it — a value taken from the far end would let an address in no list choose which
  world's ticket it is handed, which is exactly the choice that has to stay with whoever typed
  the address. Every other combination of the three is a usage error, listed on `Start` in
  `main.rs`.
- **What a fingerprint that does not match means, and what happens.** It is refused inside the
  handshake, before a byte of this protocol is sent, with a message naming the address, both
  fingerprints and **the list** as the source of the expectation — and no bypass flag, no prompt.
  The two things it can mean are "the operator moved the world without `server-key.pem` and never
  re-registered it" and "somebody is standing between you and that server", and nothing on this
  side can tell them apart. There is deliberately nothing here to clear: the remedy is on the
  other side of the list, where whoever runs the server reads the fingerprint out of its own
  `certificate_sha256=…` startup line and registers that. Naming the list rather than a file is
  the one thing this message had to change — a player told "a file says X" goes and edits the
  file, which was the old bypass wearing a remedy's clothes.
- **A row this client cannot verify a server against takes the whole list down.** `net/servers.rs`
  refuses a list holding a `certificate_sha256` that is not a digest, rather than dropping that
  row: a row dropped quietly is a server a player is told does not exist. The account service
  validates the field at registration, so a malformed one means the two sides disagree about the
  shape of the list — which is exactly what a refusal should surface.
- **The signature check is still rustls's.** Pinning replaces *identity* validation, not the
  proof that the peer holds the key it presented; without `verify_tls13_signature` the handshake
  would accept a certificate copied off the wire by anyone who had watched one.
- **Nothing here implements cryptography.** The fingerprint's SHA-256 comes from the crypto
  provider's own cipher suites — rustls exposes each suite's hash — rather than from a fourth
  crate or from a hand-rolled digest.
- **The chain ends at the account service, and the code says so.** `AccountService` in
  `net/signin.rs` carries the paragraph, because a reader should not have to infer the root of
  trust from four modules. One URL is named at launch and everything follows from it: the ticket
  is signed by its key, the list is read from it, and every fingerprint this client verifies a
  server against came out of that list. **What is fixed by construction is that there is exactly
  one of them and nothing can introduce a second** — it is parsed in `main.rs`, inserted into
  `SignInSettings`, and read from there by both the sign-in and the list; no server can add a
  source at runtime and there is no pin file left to be a second opinion.

  **And the hop to it is authenticated, which this paragraph used to have to disclaim** (#131).
  It is `https`, and `AccountService::parse` refuses `http` rather than downgrading silently. The
  certificate is checked against a SHA-256 the launch supplied — `--account-service-fingerprint`
  or `VOXELHEIM_ACCOUNT_SERVICE_FINGERPRINT`, beside the address — through the same verifier a
  game server's certificate goes through, as `tls::Expectation::Supplied`. No root store is
  needed and none is carried: pinning is a digest comparison, which is why this cost no fourth
  crate.

  **The number is supplied, never discovered**, and that is the part not to soften. There is no
  trust on first use, no `--insecure` and no plaintext form: first contact is exactly when a
  substitution happens, so a fingerprint this client learned from the connection would be a
  fingerprint an attacker could choose. It travels the way the address does — out of band, from
  whoever runs the service, once. The refusal names both digests and the flag to correct, and it
  cannot say "the list will fix it" the way a game server's can, because this hop is what the
  list arrives over.

  **`AccountService` cannot be constructed without it in a shipped build.** `parse` is the only
  public constructor and it takes the fingerprint; `plaintext` is `#[cfg(test)]`, the seam
  `http::Transport` documents and `session::Transport` established. So "the sign-in is pinned and
  the list is pinned" is a property of the type rather than of two call sites that each
  remembered.

## The server list, and why an empty one is a claim

`net/servers.rs` reads `GET /v1/servers` on its own thread, once per read, presenting the cached
ticket as `Authorization: Bearer …`. The ticket is read from its file **on that thread** and never
reaches the ECS — the same fence a session's identity file sits behind.

The list answers two questions and the second is the security half: where a server is, read fresh
every launch so a server that moved is followed without anybody being told; and which certificate
to expect there.

**`ServerList` has three variants and no fourth for "empty", deliberately.** `Ready(vec![])` is a
true statement — no server has registered — and it is a different fact from `Unavailable`, which
is "nobody could be asked". Collapsing them would put an empty list in front of a player whose
network is down, and an empty list reads as *no servers exist*. `ui/servers.rs` draws the second
as a line saying the login service could not be reached, with a retry beside it, and draws no rows
at all.

An expired or unusable ticket is a third answer again: it puts the login screen back up rather
than offering a retry that would fail the same way, which is the distinction the account service
split `ticket_expired` out of `unauthorized` to make possible.

**`ui` never learns an address.** `ListedServer` exposes a name, a display name and whether the
service has heard from that server recently; the address and the fingerprint have no public
accessor, its `Debug` redacts the address, and a click writes `ConnectRequest { name }` for the
network boundary to resolve. An address locates somebody's house, which is why the account
service keeps the list behind a credential in the first place, and a screenshot of a panel is not
a place for one.

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

**A sign-in asks for one of two tickets, and which one is the launch's decision.** With no
`--world`, the `finish` body carries `state`, `code` and `finish_secret` and no `world` field at
all — the same "absent rather than empty" encoding `encode_client_hello` uses for a token nobody
holds — and what comes back is an **account** ticket, which names no world. That is what a player
signs in with before they have chosen one, and it is what the server list is read with. With
`--world`, the body names it and what comes back is a **world** ticket, which is the only thing a
game server admits anybody on.

**The two are cached apart**, in `voxelheim/account/<service>` and
`voxelheim/world-ticket/<service>/<world>`. One file for both would put a live credential for the
wrong thing behind a screen that says "signed in" and offers no control — an account ticket at a
game server is `ErrWrongWorld`, a world ticket for another world is the same refusal, and from the
login screen both look like the dead end #154 removed. `tickets::world_ticket_path` says why the
world is a path component rather than a suffix.

On the list path, choosing the world is still the list's job and the row already carries the name;
nothing there asks for a world ticket yet, so a listed session presents the identity file and no
ticket. That is #107.

**Nothing about a ticket reaches the ECS.** `SignInState` is the whole of what leaves `net`, and it
carries a state and a line of text. The ticket lives on the sign-in thread for the length of one
attempt and after that only in the cache — so there is no resource for a `{:?}` to find, and no name
outside `net` anything could start deciding from. That is the fence `PlayerToken` already sits
behind. The server list read is the one thing that presents a ticket, and it does so on its own
thread, reading the cache — exactly as a session reads the identity file.

**Where the listener binds is the account service's decision, not this client's.** The redirect URI
is registered with the provider, so `net/signin.rs` reads it out of the `redirect_uri` inside the
authorize URL and binds *that* — after checking it is loopback and plain HTTP, and refusing
otherwise. A listener on a port of its own choosing would be a listener the browser never reaches —
**including the one the kernel would choose.** A redirect URI naming port 0 is refused rather than
bound: the browser is sent to the literal `redirect_uri`, so an ephemeral port is a port nothing was
told about, and binding one turns a misconfiguration into a wait that runs to the deadline with
nothing to say.

**The tab is told what actually happened.** The listener holds the browser's connection until
`finish` has answered, then renders a page saying the sign-in worked or that it did not. Answering
"it worked" the moment the redirect landed would be wrong exactly when it mattered, and a tab left
saying nothing is how a player concludes the game is broken. The pages are self-contained — no
image, no script, no font, no request to anywhere.

**The `state` is compared twice, and the two comparisons answer different questions.** Before
anything is sent to `finish`, because a `code` may be redeemed once and forwarding a redirect that
belongs to a different attempt would spend somebody else's sign-in. And in the accept loop, because
the redirect port and path are registered configuration — public and identical on every machine — so
any page a player has open can issue a request to them. Ending the wait on the path alone would let
an `<img>` tag abort a sign-in: the listener would be gone before the real redirect arrived. Nothing
is stolen either way; what the second check protects is the attempt itself. Anything reaching that
path without this attempt's `state` — including a query that will not decode — is answered `400` and
the wait carries on.

**Four hand-rolled readers, and the dependency budget is why.** `net/http.rs` and `net/json.rs`
carry an HTTP/1.1 client, a URL splitter, percent-decoding, a JSON reader and an RFC 3339 parser;
`net/tickets.rs` carries a base64url codec. **`net/http.rs` gained TLS in #131 and no crate with
it**: the transport is `net/tls.rs`'s verifier over a `rustls::StreamOwned`, because pinning is a
digest comparison and needs no root store. Every one is narrow on purpose and none is a general
facility — the base64url codec knows one alphabet and one length, and the JSON reader admits
exactly one nested shape, an array of flat objects, because that is what `GET /v1/servers`
answers with. `Depth` in `net/json.rs` is that rule written as a value rather than as care: an
array is a legal value at the outer level and no value at all inside a row, so a reader that
started recursing would stop compiling rather than quietly growing. **No error in any of them quotes its input**, because a
`finish` request carries an authorization code and its response carries a ticket, so a diagnostic
built from those bytes is a diagnostic that can carry one into a log. That is the rule
`signin.go` keeps on the other side.

**The login screen owns the input while it is up.** `choose_input_mode` forces `InputMode::Menu`
and `sync_cursor` releases the pointer, because the game is running behind the overlay and a click
meant for the one control must not also reach the world — and a locked, invisible cursor over a
screen whose whole content is one button is a button nobody can press. `Escape` cannot leave it: a
login screen is deliberately not dismissible, and `show_menu` is the other half, so the pause menu
is not drawn underneath.

## Choosing who goes in

**The handshake has a phase in the middle of it now, and it is the one this client spends waiting
for a person.** `ClientHello` is answered with `ServerCharacterList` — this account's characters on
that world, and how many it may hold — and the world arrives only after a `ServerWelcome` that
answers a choice. `net/handshake.rs` models it as `AwaitingCharacters → Choosing → AwaitingWelcome
→ Established`, so a welcome that overtakes a selection is a protocol error with a name rather than
a spawn nobody asked for, and a second list is one too.

The pieces, and the boundary each stays on:

- `net/handshake.rs` holds the phase and its admission rules. `Handshake::chose` is what moves
  `Choosing → AwaitingWelcome`, called by the session thread in the moment it writes the frame —
  which is what makes "a welcome before a choice" a thing this state machine can *see*.
- `net/session.rs` owns the socket. The choice reaches it as `NetCommand::Choose`, and the
  selection or creation frame is written by the session thread, from the same thread that wrote
  the hello. **The writer thread starts at `Established` and not before**, which is the whole
  reason the command exists: one writer per connection is a rule this handshake would otherwise
  break by waiting for a person in the middle of it.
- `net/mod.rs` carries `CharacterChoice` — the list, the limit, the preselection, whether the
  answer has gone out and the server's last retryable name refusal — and turns one
  `ChooseCharacter` message into one command. `answered` is what makes a double press harmless: a
  second `SelectCharacterRequest` on a welcomed session is a protocol error that closes it.
- `ui/character.rs` draws the rows, the creation draft, the palettes and the live preview, and
  writes `ChooseCharacter`. It never touches a socket, the same way the login screen and the
  server list never do.

**Nothing here decides anything, and the name is the case worth stating.** Whether a name may be
worn is the server's rule; `CHARACTER_NAME_TAKEN` and `CHARACTER_NAME_REFUSED` are its two
different answers, they arrive as a `ServerReject` that closes the connection, and the screen
renders what came back beside the name field. The client reconnects transparently on the same
`RejoinBy` route because the server has already closed the socket; it reads the cached ticket again
instead of keeping credential bytes in the ECS, and the fresh `ServerCharacterList` is what makes
the form writable again. A client that guessed at either would be holding an opinion about a world
it can only see part of. `BAD_REQUEST`, `CHARACTER_LIMIT_REACHED` and ticket refusals do not take
this path: typing another name is not their remedy. The draft stays in place across the one redial.

**The colours are stated rather than picked freely.** `ui/character.rs` holds one table per field
with the reasoning beside it: what a Norse dyer could reach, which is why there is no free colour
picker and no magenta. Every entry is checked at compile time against the `0x00RRGGBB` the contract
carries, so a palette entry the server would refuse is a build failure. `Appearance` itself has
private fields and one constructor — `net/codec.rs` — so the decoder and the screen reach the same
door, and there is no second way to build one that nothing checked.

**Which character was played here is remembered, and it is a convenience rather than a claim.**
`$XDG_DATA_HOME/voxelheim/characters/<address>` holds one id, written after the server welcomes a
session on it, and the screen preselects the row that matches — one file per server, for the reason
the identity file has one: an id is minted per world. Anything unreadable, absent or matching
nothing in the list preselects nothing and costs exactly one keypress, which is why it is not
gated on the certificate expectation the way the identity file is: nothing is decided from it and
it goes back only to the server that issued it. **A creation is deliberately not remembered** —
`ServerWelcome` names an entity and no character, so a client that has just made one cannot know
the id the server minted for it; the next launch lists it and selecting it once is what teaches the
file.

**`--name` answers the phase without a person, and it is the sentence a hello used to carry.**
Before V7 the server read `ClientHello.player_name` and settled a character from it; V7 moved that
decision onto the wire and left the field carrying nothing anybody reads. `ui::PlayAs` is the same
sentence in the new grammar: with a name given, the screen asks to play the listed character
wearing it, or to create one under it when the account holds none, wearing the starting appearance.
It is a request like any other — the server refuses a name it would refuse from the screen — and it
is what lets `scripts/interop-check.sh` reach a world at all, since no unattended check can press a
key. A launch that named nobody waits, which is every player's launch.

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
# Sign in, then pick a server. <sha256> is what voxelheim-auth prints at every start,
# as certificate_sha256; this client checks it instead of a certificate authority.
cargo run --release -- --account-service https://127.0.0.1:7778 \
    --account-service-fingerprint <sha256>
# Development: sign in, one address.
cargo run --release -- --account-service https://127.0.0.1:7778 \
    --account-service-fingerprint <sha256> --server 127.0.0.1:7777 --world midgard
cargo run --release                          # development: 127.0.0.1:7777, and refused
cargo run -- 192.0.2.5:7000                  # the same, at an explicit address
cargo run -- --server norse.example         # bare host gets port 7777
VOXELHEIM_SERVER=192.0.2.5:7000 cargo run    # lower precedence than the CLI
cargo run -- --world midgard                # VOXELHEIM_WORLD is the fallback
cargo run -- --name thora                   # play thora, or create her; VOXELHEIM_NAME is the fallback
cargo run -- --identity /tmp/second         # a second character on one server
cargo run -- --help
```

**The first command is the path a player takes, the second is the development one, and the third
connects and is refused.** `--account-service` is `https` and always travels with
`--account-service-fingerprint`; neither is optional and there is no way to skip the check, since
that connection is where this client's trust begins. With them alone the addresses come from that
service's list, each carrying the certificate to expect at it. With `--server` as well, the address is the
one that was typed and `--world` names the world to ask a ticket for — the session is unverified
and says so, and it presents the ticket because a server admits nobody without one. With an
address and no service there is nothing to sign in against, so the hello names no account and the
server refuses it; that is left reachable because it is the truthful answer to a launch that named
nowhere to sign in, not because it is a mode anybody should use.

A world with nothing to ask one for — no service, or a service with no address — is a usage error
rather than a value silently dropped. The table on `Start` in `main.rs` is the whole rule.

A server has to be listening for any of that to reach one — `go run ./cmd/voxelheimd` from
`server/`, whose own "Running it" section has the flags. The account-service line additionally
needs a `voxelheim-auth` with a Discord application configured, and a `voxelheimd` that has
registered itself with it; `server/cmd/voxelheim-auth` documents both halves.

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

The refusal is the certificate check doing its job, and it names the **server list** as the source
of what it expected. See "The session is encrypted, and that is not a setting" above for why there
is no bypass flag and nothing on this side to clear; the operational half is short.

A server presenting a certificate the list does not carry has been moved or rebuilt without its
`server-key.pem` — a `voxelheimd` running `-world-dir ""` mints a new one every start, which is
what development hits most — or something is standing between the client and it. Nothing here can
tell those apart, so the remedy is deliberately on the other side of the list:

```bash
# on the machine running voxelheimd, out of its startup line
grep -o 'certificate_sha256=[a-f0-9]*' <the server's log>
# then register that server again, so the list carries the new number
curl -sS -X POST http://<account service>/v1/servers \
  -H "Authorization: Bearer <registration key>" \
  -H 'Content-Type: application/json' \
  -d '{"name":"<world>","address":"<host:port>","certificate_sha256":"<the number above>"}'
```

Until the list and the server agree, this client will not connect, and that is the whole design:
there is no file a player can edit to make the refusal go away, because a file a player can edit
is a file an attacker can talk them into editing.

Two refusals that are *not* that one and are easy to mistake for it:

- **"The login service could not be reached."** The account service did not answer, so there is no
  list — not an empty one. The screen offers a retry and nothing else; check that
  `--account-service` names a service that is up.
- **"No server has registered with this account service yet."** The list was read and is genuinely
  empty. Nothing is wrong with this client; a server has to register before it appears.

### Who the client comes back as

The server issues an identity token in `ServerWelcome`; the client stores it and presents it
in the next `ClientHello` to **that** server. One file per server address —
`$XDG_DATA_HOME/voxelheim/identity/<address>`, falling back to `$HOME/.local/share` — because
a token is meaningful only to the server that minted it: presenting server A's token to server
B makes a new character on B, and B's answer must not land in A's file.

Five rules hold that down, and they are all in `net/session.rs`:

- **There is no identity file at all on a connection nobody stated a certificate for.** `run`
  opens one for `tls::Expectation::Listed` and hands the other variant an `IdentityFile` that
  presents nothing and keeps nothing. See "The session is encrypted, and that is not a setting"
  for why this is the shape of the two variants rather than a check before the hello.

- **The token is never logged, printed or shown, at any level.** It is a bearer credential —
  whatever holds one *is* that player — so `PlayerToken` writes its own `Debug` and prints
  `<redacted>`, which makes the redaction a property of the type rather than a habit every call
  site has to remember. There is no `Display` at all. The wire is closed by the encryption above;
  a log file is closed here, and the two are different exposures with different fixes.
- **Nothing is decided from it.** It is read, presented, and stored. The one thing derived from
  it is whether the welcome's token is the one presented, which the status line renders as
  `returning` or `new character`; the server had settled the identity before it answered.

  **A session that kept no file reports neither, and says `signed in` instead.** With nothing
  presented there was no comparison to make, so `new character` would be a claim with nothing
  behind it — and on the development path it is a claim the server contradicts in the same
  handshake, because the ticket names an account and the account is what the server restores a
  character from. `schemas/handshake.fbs` is explicit that the client is not told which of the two
  happened, so `net::Identity` has a third variant for not knowing rather than a default that
  guesses.
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

- **`--server` reaches a server nothing can verify, and it is the development path.** An address
  typed on the command line is in no list, so no fingerprint states what to expect there and the
  session is encrypted but unauthenticated. What keeps that from being a hole is what it may hand
  over: no stored identity, ever — see "The session is encrypted, and that is not a setting" — and
  a session ticket only when the launch signed in for one, which is bounded by naming one world
  and expiring in hours. **That second half is a stated trade rather than a property**, and it is
  #154's: the exposure is one world's session for an afternoon to whoever answered an address the
  developer chose, against a development path that otherwise cannot connect at all. There is no
  way to *state* an expected fingerprint on the command line either: adding one would be a second
  source of truth beside the list, which is the thing #107 removed.
- **This is a client-side check and the account service is a single point of trust.** Everything
  this client verifies traces back to the one `--account-service` URL: whoever controls that
  service, or the registration key that writes to its registry, chooses which certificate the
  client will accept for a given name. That is the trust chain ending somewhere, which it must,
  and it is why the registration endpoint is behind a key and the list is behind a ticket. What
  the connection to that service is *not* is authenticated — see the plain-HTTP gap below.
- **The two stacks are checked meeting, but by hand and not by CI — and the gap that leaves is
  what #154 fell into.** `scripts/interop-check.sh` drives the real client against the real server
  over TLS: the documented development command reaching a world, a hello with no account refused
  in the server's own words, a ticket for one world refused by a server running another, a stored
  identity never presented to a server nothing stated a certificate for, and — since #108 — the
  character phase itself: an empty list answered with a creation on the first launch, and the
  character that made answered with a selection on the second (check 6). Every client it starts
  passes `--name`, because the screen otherwise waits for a person and `timeout` is what would
  answer it. It mints its own
  ticket with a key it generated and gives the server the public half as `-ticket-key`, because a
  real sign-in needs a Discord application and a browser; the account service is the only thing it
  stands in for.

  **It did not catch #154 because it had itself stopped working** — it started `voxelheimd`
  without `-world-name`, which that server now refuses, so the whole script died at the first
  check. A check nobody runs is a check that rots into a check nobody can run, and both halves of
  #154 shipped green underneath it. What it can *not* check end to end is the certificate refusal,
  because the expectation comes from a list rather than from a file the script could write; that
  assertion lives in `net/tls.rs`'s own tests, where the expectation is a value. It is not in CI
  because the client opens a window and needs a display, and because the Go and Rust gates run in
  separate jobs with separate toolchains — no job has both binaries. **Run it after touching
  `internal/transport`, `internal/certs`, `internal/session`, `internal/ticket` or `net/`.**

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
  and legacy issue 6 put that job's setup out of scope. Enabling it therefore takes two changes in one
  PR: add `libwayland-dev` to the `Install Bevy system dependencies` step in
  `.github/workflows/ci.yml`, then add `"wayland"` to the feature list in `Cargo.toml`. Doing only
  the second reddens CI with `Package 'wayland-client' ... not found` — and it will still build
  fine on your machine, which is the trap. It deserves its own issue rather than a drive-by.

- **Joining from the list still presents no ticket, and that is #107.** #154 gave the
  *development* path a world to name, so `--server` with `--account-service` signs in for that
  world and presents what comes back. A row of the list already carries the world name — it is the
  registry's own identifier — but nothing on that path asks for a world ticket yet, so
  `connect_on_request` passes `None` and a listed session identifies a player by the per-server
  token, exactly as before. A game server answers `ErrWrongWorld` to an account ticket, correctly,
  so presenting the cached one there would be worse than presenting nothing.
- **The loopback listener binds a fixed port, so two clients cannot sign in at once.** The port is
  the account service's `redirect_uri`, which the provider requires to match exactly; a second
  client signing in at the same moment finds the port taken and says so. The refusal names it.
- **A redirect URI naming `localhost` binds whichever of `::1` and `127.0.0.1` resolves first.**
  `TcpListener::bind` takes one address, and a browser may connect to the other. Naming the
  literal address in the service's `-discord-redirect-uri` avoids it entirely, which is what its
  own default does.
- **The settings screen is one column, and #247 owes it tabs and a reset per tab.** The
  graphics half of #179 landed here — render distance, field of view, vertical sync, frame
  cap, brightness, where the fog begins, and the frame-rate readout in any of the four
  corners — and it landed as rows on the one column this screen has always been. What #247
  still owes is the `CONTROLS` / `GRAPHICS` strip over them and a reset that puts back **one
  tab's** settings: a reset that wrote `Settings::default()` back would look right on the tab
  it was asked about and would silently clear the other, and restoring bindings has to go
  through `Bindings::from_pairs` because two controls that traded keys cannot be put back one
  rebinding at a time. Not coming with any of it: *cursor capture*, which belongs to the
  camera-control issue this file has named for a while and that still does not exist;
  *audio*; and *shadows, ambient occlusion and texture quality*, which have no shadow map,
  no AO pass and no texture behind them. Nor the pitch limit, which `player/constants.rs`
  explains is an invariant rather than a preference.
- **A module that names a file under the data directory carries #230's guard, and this is
  the second one.** `Environment::read` is `#[cfg(not(test))]` in `settings/store.rs` for
  the same reason it is in `net/session.rs`: a test build that can ask what
  `$XDG_DATA_HOME` is will eventually write into the developer's own, and #230 is what that
  looks like after a while — 1769 files nobody noticed, nothing ever red. Each such module
  falls back through its own `default_environment()`, whose test half names nowhere.
  `scripts/test/client-data-home-isolation.test.sh` holds both, from one list; the next
  module joins that list rather than copying the reasoning.
- **`Escape` is bindable, and the settings screen keeps it only while nothing is waiting.**
  It is `Control::Menu`'s default and `crate::settings` offers it like every other key, so a
  panel that swallowed every press of it would be one that could never put the pause menu
  back where a player found it — the model saying a key is free while the screen quietly
  refuses it is the one disagreement a settings screen must not have. With a capture
  waiting, the press goes to the control that asked for it; with nothing waiting it takes
  the panel down, which is the way out a player who has bound the menu somewhere unfortunate
  still needs. Taking a capture back is a second press of its own row.
- **A character cannot be deleted or renamed, from here or at all.** The contract has no message
  for either — `schemas/handshake.fbs` reserves a list, a selection and a creation — so a roster
  that is full is full, and `--name` naming nobody on one says so and leaves the screen up. The
  server's store is where a deletion would have to start.
- **The preview is the real rig, in the world, turning in front of the one camera.** It is dressed
  out of the same wardrobe a body is — `player::BodyVisualsPlugin`, the same meshes and the same
  material per colour — so it cannot disagree with what a player will see of themselves. It was
  flat `bevy_ui` nodes until #181, with a hand-written painter's order standing in for a depth
  buffer; that went, and `PlacedBox::nearness` and `appearance::slots` went with it. **There is
  still exactly one camera** — `player/camera.rs`'s rule is untouched, and this adds no second
  camera and no render target.
- **The screen is not a window onto the world, and three things make that true together.** The
  camera clears to the screen's own flat `BACKDROP` while it is up and **puts back only what this
  screen put there** — restoring `Daylight::FIXED.sky` unconditionally would have overwritten
  `sky::drive_the_sky` on every frame of every world with a clock, so the day would never have
  turned; the root overlay is transparent, where it used to be a 98% sheet
  that would have hidden the model; and the model is despawned with the screen. Change any one of
  them and the other two stop making sense.
- **What the UI contributes is a hole, not a picture.** `PreviewStage` is a node with no
  background whose computed rect is where the model is placed — which is what keeps the figure and
  the layout agreeing across a resize. It sits *outside* the panel and has to: a `bevy_ui` parent
  draws behind its children, so a transparent node inside an opaque panel shows the panel. The
  rect comes from **`UiGlobalTransform`, not `GlobalTransform`** — `bevy_ui`'s layout writes the
  first and leaves the second at the identity, so reading the wrong one puts every stage at the
  top-left corner of the screen; its translation is the node's *centre* and both it and
  `ComputedNode::size` are physical pixels, which is why no scale-factor term appears. The
  placement goes through the projection rather than `Camera::viewport_to_world`, because that one
  needs a viewport and a headless app has none — the maths would be the one part of the feature no
  test could reach. Anchor on the **vertical**: the field of view Bevy holds fixed across a resize
  is the vertical one. The stage is sized from `appearance::envelope` rather than from the collided
  box, or it would clip the knuckles off everybody and the knot off one of them.
- **No sign-out and no account switching.** Deleting the cached ticket is sign-out; the usage text
  says so and `--account-service` pointed somewhere else is a different file.
- **No generic reconnect, backoff or session resumption.** A dropped connection is reported and
  stays reported, with nothing set to try it a second time. `Rejoining` has exactly two writers,
  both with a complete remedy on the same route: `disconnect_on_request`, after a player asks to
  leave a world, and `drain_session_events`, after `CHARACTER_NAME_TAKEN` or
  `CHARACTER_NAME_REFUSED` answered a creation. The second is request recovery rather than session
  recovery: the server closes by contract, the client dials once, and the new list re-enables the
  form. Every other refusal and every unasked ending sets no flag.

  The flag is dropped *before* the dial that consumes it can fail, so a rejoin that is itself
  refused is a refusal a player can read rather than the first turn of a loop. The list is fetched
  again over a fresh connection rather than reused, because what the server holds may have changed,
  and going back through `RejoinBy::Row` rather than a remembered address is what keeps the
  certificate verified against the same row it was verified against the first time.

  Two consequences worth knowing before editing this. **`NetLink` is now removed when its channel
  closes** — it used to outlive the thread it represents, harmlessly, because every reader takes it
  as an `Option`; its absence is how a rejoin knows the previous session has let go of its socket.
  **A retryable name answer never becomes `Rejected` or `Disconnected` while that happens**:
  `CharacterChoice` keeps the form mounted, the server's `Ended` and the closed event channel both
  preserve it, and the one internal list request is the only request allowed to open a connection
  from `Choosing`. That ordering is what prevents the terminal status screen flashing between the
  answer and the fresh list.
  And **`--name` answers one exchange only**: it is spent once a `Session` has existed, or leaving a
  world would send the player straight back into it. A name refusal keeps `CharacterChoice`
  present across the redial, so the launch's local one-shot guard also stays set and cannot submit
  the same refused name in a loop; the player takes over the form.

  `Reject` crosses the net-thread boundary as a typed value and is flattened for display only after
  this classification. Branch on the code, never on the detail the server wrote for a person.
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
- **Two views, and the local player's body is drawn in both and hidden in one.** First person is
  what the game is played in and the camera is the eye there, so the body is `Visibility::Hidden` —
  a body at the eye fills the screen with the inside of the player's own head. F5 swaps to a
  third-person view whose camera sits `BOOM_LENGTH` behind, and the same body simply becomes
  visible. It used to get no mesh, no children and no `Worn` at all; #172 gave the camera somewhere
  else to be, and one spawn path with a visibility toggle is a much smaller thing than two.
- **Third person is a way of looking, not a way of playing, and that is what makes it small.** No
  crosshair, no outline, and **both** `InputGate::may_aim` and `may_act` closed — they are two
  independent expressions over the same inputs, not one defined in terms of the other, so closing
  only the first would leave a view with no sight in which clicking still mines. Movement keeps
  working; that is the point. Aiming is off rather than re-based on a separate eye point, because
  a second point is a thing that can drift from the camera and a closed gate is not.
- **A death does something different in each view, and it was decided rather than
  inherited.** In **first person** the camera is the eye, so the eye goes over: the pitch
  swings up to `MAX_PITCH` and the eye sinks to `DEATH_EYE_HEIGHT`, and it rests on the sky
  until the server respawns the player. In **third person** the camera does not move at all —
  it is an observer, and what falls is the character, tipped by `collapse_bodies` on the
  same `DeathFall` curve every player rig carries. A camera that fell here would take the body
  out of frame at exactly the moment the player is watching it go down, which is the case that
  view is most worth having for. **The F5 toggle is refused while dead**, in both directions:
  the two views resolve a death into two different things, and flipping mid-death would either
  stand a fallen camera up or drop an upright one on its back. It was also the last playing-mode
  key `SelfVitals::dead` did not already close.
- **None of that decides anything, and the drop is the proof.** Player falls follow the
  server's complete `EntitySnapshot.dead_players` list and mob falls follow `MobAction.Dying`;
  a client that drew neither would be dead for the same three seconds, respawn at the same
  moment, and pick up a draugr's bones at the same moment, because the wait before a kill's loot
  exists is `MobDeathDuration` on the server and nothing here is asked about it. There is
  deliberately **no mirror of that number on this side**: a body's fall is a curve that
  finishes, and what ends a death is the server no longer sending the creature, which despawns
  it through the branch one that walked out of view takes.
- **A draugr topples backwards and a vargr slumps sideways with its legs splaying**, which is
  the one thing about a death that differs by species and the only place `player/mobs.rs`
  matches on the kind to decide a pose. Both pivot at the feet, because every mesh in that
  file is authored with its origin there. The vargr's legs moved out of its body mesh into a
  child of their own so that a collapse can scale them outward — one transform on the group,
  where four legs each turning on their own hip would be a rig, and the price is that they
  thicken by the factor they travel.
- **Holding Shift orbits the camera and never the character.** `LookState::yaw` is what
  `PlayerInput` carries, so the orbit is a separate `Orbit` *offset* — at rest it is zero, which is
  why the camera sits behind a turning character with nothing chasing anything, and why releasing
  the key is an animation back to zero rather than back to a remembered angle. Nothing about the
  view crosses the wire and the server cannot tell which one a client is in.
- **Other players are the rig and one server-driven death pose.** There is no movement or combat
  animation, no name plate, no equipment on the body, no faces beyond the two eye boxes, and no
  texture anywhere — it is coloured geometry. Each of those is its own issue.
- **An entity can be drawn before it has been described, and is never re-spawned when it is.** The
  appearance stream and the snapshot stream are not ordered against each other, so a body whose
  `PlayerAppearance` has not landed wears `codec::PLACEHOLDER_APPEARANCE` — the neutral grey
  `schemas/player.fbs` documents — and `dress_bodies` swaps six handles in place the moment it does.
  Despawning and re-spawning would restart the interpolation and blink the body. The server sends
  the appearance *ahead* of the snapshot that first carries the entity where it can, which makes the
  placeholder rare rather than impossible.
- **Both body caches are the size of a view, not of a session.** An entity that leaves takes its
  cached appearance with it, and `Player.described` on the server drops its own entry at the same
  moment — so a player who walks back into view is described again and neither side had to be told.
  The one case a snapshot cannot answer is an appearance for an entity no snapshot has ever
  mentioned; it is held for `APPEARANCE_GRACE` and then dropped, which is what stops a server that
  describes entities it never shows from growing a map for as long as the connection lasts.
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
- **No texture atlas, no UVs.** `palette.rs` is the whole terrain material system: a colour per
  block id, carried as vertex colours. Item-only swatches live in `player/items.rs` beside the
  rows that name them and resolve to the same linear RGBA shape every renderer consumes. Art
  assets are a later issue.
- **The list is a list and not a browser.** No favourites, no sorting, no player counts and no
  ping column; "online" is the account service saying it heard from that server recently, not a
  probe, and the screen says as much rather than implying reachability it did not measure. The
  list is read when the sign-in completes and when the retry is pressed — there is no automatic
  refresh, so a server that comes up while the panel is open appears on the next press.
- **Interpolation holds the last position for ever when a server goes quiet.** There is no timeout
  that fades an entity out, and none that says "this session is stale": a quiet server is a
  legitimate state, and the read timeout in `session.rs` is a poll interval rather than a session
  timeout. Deadlines belong to the same issue on both sides.
