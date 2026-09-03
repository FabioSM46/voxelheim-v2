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
through `ChunkStore::apply_block`, which is the only writer of a voxel there is. A refused edit
normally looks exactly like nothing happening. `Warded` is the one exception: the server sends
`ActionRefused{EditBlock, Warded}` so the status line can explain the silence without changing
the world locally.

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
| `world/mod.rs` | `WorldPlugin`, `ChunkStore`, `DecodeQueue`, the RLE expansion and its invariants, applying a `BlockUpdate`, asking for an evicted chunk back, gathering the chunks a mesh depends on, and the two questions about a voxel — `solid_at` for what stops a body, `targetable_at` for what the crosshair finds | mesh, or spawn anything |
| `world/mesher.rs` | greedy meshing, including the cull against the neighbours it is handed, the per-vertex `Occlusion` the opaque surface's corners carry, the per-vertex `WaterFlow` the water surface carries, and the third half — the per-voxel plant `build_cover` grows for every `is_cover` block, a flower for each flower id, a leafy bramble for a bush and a leafless thorn bramble for desert scrub | mention a Bevy type, or read a chunk it was not given |
| `world/render.rs` | the meshing tasks, the mesh assets, the three materials, one entity per chunk with the water and cover halves as its children | mesh on the main schedule, or own a camera or a light |
| `world/palette.rs` | block id → colour and alpha, which ids stop a body (`is_solid`), which hide what is behind them (`is_opaque`), and which are cover — there to be seen and broken, solid to nothing, and the set the mesher grows a shape for rather than sweeping as a cube (`is_cover`) | know about meshes or about the wire |
| `world/water_material.rs` | what water looks like: the `ExtendedMaterial` over `StandardMaterial`, its embedded WGSL and the one `time` uniform | decide anything, or reproduce what the base material already answers for |
| `player/mod.rs` | input sampling, the send cadence, one body per entity the server sends, the authoritative vitals and the one gate every playing control is read through | decide where anything is, or decide that a player is alive or dead |
| `player/ambience.rs` | the cosmetic ground look sampled from the loaded voxels around the eye | be read by anything that decides an outcome, be sent, or be derived from anything the server said about climate or weather |
| `player/birds.rs` | the ambient birds — the species table, the flight paths and the flap — chosen by `Ambience` | be sent, be hit, be targeted, be counted by anything, or be chosen by anything the server said |
| `player/drops.rs` | one small visual per drop in the newest snapshot, plus local spin and bob | infer pickup, merging, expiry or any other reason a drop disappeared |
| `player/projectiles.rs` | one visual per projectile in the newest snapshot, oriented from its newest velocity | integrate velocity, test a hit, or keep a body the server omitted |
| `player/mobs.rs` | one body per mob in the newest snapshot, the species boxes mirrored from the server, and the cosmetic lean, hit flash and death fall | read health as death, hold an AI, or advance an action local time did not receive |
| `player/hands.rs` | the camera-space held item, its origin-anchored render-layer camera, its cosmetic swing/bump, and the mining punch the server's progress starts and stops | decide item legality, mining progress or any gameplay outcome |
| `player/saddle.rs` | the camera-space saddle view shown by the newest authoritative local mount projection: the world horse's own head, ears, neck and mane at one scale, framed for the narrowest field of view, and the fists with the reins to the bit | draw a world horse, re-type a size the horse has, predict a mount transition, or decide any mounted action legality |
| `player/horse.rs` | the world-space horse — under its rider's body when ridden, on a root of its own in the paddock — cut to real proportions from `shapes::hexahedron`, its gait, its tack, and the humanoid rig seated astride it, all drawn inside the mounted body the server collides (`MOUNTED_WIDTH` × `MOUNTED_HEIGHT`, mirrored in `constants.rs`) | decide that anyone is mounted, move a horse, or fit anything to the player's walking box |
| `player/shapes.rs` | the eight-corner solid: one closed six-faced `Mesh` from eight free corners, flat-shaded, UVs on every face, carrying exactly the attributes a `Cuboid` mesh carries so `merge` joins the two | share a vertex between faces, average a normal across a crease, or grow a seventh face, a cylinder or a sphere |
| `player/items.rs` | one row per item id: its display name, its held shape, and the block-derived or item-only colour it draws as | hold a capability, a stat, or anything a rule is read from |
| `player/inventory.rs` | the latest complete server-sent slots, the locally selected slot index, and which of the four intents a cell press means | increment, decrement, move or merge a count, move a durability, or decide that a stack may be put down or consumed |
| `player/crafting.rs` | the display-only mirror of the server's recipe table, and the craft intent one row originates | decide that a craft succeeds, consume a material, or produce an item |
| `player/interpolate.rs` | the two-snapshot buffer and the interpolation | mention a Bevy world, or extrapolate |
| `player/camera.rs` | the world camera, and what it follows | decide a gameplay outcome |
| `player/sky.rs` | the one directional light, the curve the sun, the sky colour, the ambient term and the fog are read from, and the sky and fog a submerged eye sees instead | hold a boundary the server sent, let anything read a rule back out of a colour, own a light that is not the sky's, or decide what being in water *does* |
| `player/wards.rs` | the newest complete server-sent ward columns and the three translucent boundary meshes drawn from them | derive a ward, authorise an action, feed targeting or movement, draw a dome, or let presentation become gameplay state |
| `player/target.rs` | the voxel raycast, target outline, held mining intent and authoritative progress presentation | apply an edit, compute mining progress, or judge an action legal |
| `player/structures.rs` | the tents, forges and campfires the newest snapshot names, the footprint arithmetic mirrored from the server, the fire's own light, and the two requests that ask for one | stand a structure up locally, decide whether a placement is legal, move one, or let the fire's glow state where the server's safe radius ends |
| `player/constants.rs` | the body's dimensions, the look controls and the aiming reach | hold a number the server owns |
| `settings/mod.rs` | what a player may change: the mouse sensitivity, the key bindings and the one rule that refuses a rebinding rather than leaving a control unreachable, the eight graphics values — including window mode and the attached monitor — and the frame-rate readout; each setting keeps its bound, step and default here, plus the tab that scopes its reset | reach the wire, take a value from something the server sent, decide any outcome, or let one tab's reset reach another tab's fields |
| `settings/store.rs` | the settings file — its path under the data directory, its text format, and the temporary-file-and-rename that replaces it | refuse to start over a line it cannot read, hold a bound of its own, or let a test build ask where the data directory is |
| `ui/icon.rs` | the flat picture each `ItemShape` is drawn as in a cell, and the nodes that draw it | key a drawing on an item id, decide a shape of its own, or load an asset |
| `ui/health.rs` | the health bar, the server's respawn-protection flag and the death overlay with its countdown | hold a timer, run a countdown down, or write any resource |
| `ui/hunger.rs` | the hunger bar and its wall-clock low-reserve reminder | change hunger, decide whether food may be eaten, or turn its presentation timer into simulation time |
| `ui/chat.rs` | the local chat draft, the last eight accepted lines, their wall-clock fade and routing of the five slash commands into typed party requests | parse received text, trust a display name as identity, decide that a message or party action succeeds, or keep persistent history |
| `ui/storm.rs` | the last server-sent storm warning, its receive instant, and the one routing that publishes each milestone sentence once through the tagged chat channel | infer a storm from weather, advance a phase locally, or grow a second notification surface |
| `ui/party.rs` | four permanent rows mirroring the newest accepted party snapshot, with names from the appearance cache, and the two marks a row draws — the leader's crown and the hunted mark — as nodes | infer membership, health, leadership, invitation state or any party outcome from local intent, or give a drawn mark a colour of its own |
| `ui/status.rs` | the debug text nodes: connection, world counters, player position, inventory — the frame-rate readout in whichever of the four corners the setting names, and routing server refusals and trade endings into tagged chat | reach into another module's internals, grow a health bar, call the snapshot age a round trip, or grow a second notification surface |
| `ui/login.rs` | the login screen: one control, the line under it, and when it is up | start a sign-in, hold a ticket, or offer a way past itself |
| `ui/servers.rs` | the server list screen: a row per server, the retry, the line under them, the reconnect that goes back to the server the last session was on, and when each is up | learn a server's address, open a socket, dial without a press, or draw an empty list for a list it could not read |
| `ui/character.rs` | the character screen: the rows, the creation draft, the stated palettes, the live preview, and the launch that answers it from `--name` | decide whether a name may be worn, invent a colour the contract does not allow, or enter a world before the welcome |
| `ui/settings.rs` | the settings screen behind the pause menu: the two tabs, the fixed-height area under them, the rows, the steppers, the rebinding capture, the refusal it prints and one reset per tab | hold a bound, a step or a default of its own, decide which tab a setting is on, narrow the set of keys the model offers, or leave a control with no key |
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

**Every edge from `player` to `world` is narrow and read-only, and there are six**:
`player/target.rs` reads `ChunkStore`, because aiming is a question about voxels and the store is
the authority on which of those exist; `player/camera.rs` reads it for the same question one step
further on, so the third-person boom stops at a wall instead of going through it;
`player/sky.rs` reads it for exactly one voxel — the one the eye is inside — because water is the
one block that changes what the sky looks like; `player/wards.rs` reads that same answer to hide
its presentation under water; `player/ambience.rs` reads a coarse lattice of loaded columns to
describe their cosmetic ground look; and `player/items.rs` asks `palette` for a terrain
swatch when an item deliberately reuses one. The first-person hand takes its skin colour from the
local player's server-sent `Appearance`, not from a terrain approximation. **No edge writes world
state, and no edge points back from `world` to `player`.** A seventh, in either direction, is a
design question rather than an import. The five that read the store all resolve their voxel
through `ChunkStore::block_at`, the one place a world coordinate becomes a block id.

**The last of those is the client's one opinion about what an item looks like, and every
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
- **A `StormWarning` crosses with the instant it was decoded.** The ECS queues the pair in
  `StormInbox`; `ui/storm.rs` keeps the newest pair in `Storm` and the compass subtracts wall
  time from the exact seconds the server stated. It never changes the stored phase or seconds,
  never infers one from `Snapshot.weather`, and clears the pair when the session ends. The
  receive instant is presentation's anchor, not a second world clock.
- **A `WardsNearby` crosses as one complete value.** The ECS queues it in `WardsInbox`;
  `player/wards.rs` keeps only the newest complete set in a frame and replaces `Wards` wholesale.
  An empty set clears it, and ending the session clears it again. The boundary renderer reads
  those columns and never derives a ward from terrain, structures or the seed.
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

- **`world/mesher.rs` is pure, and its signature says so.** A chunk, its six face neighbours
  and the four chunks above its horizontal neighbours in; vertex and index buffers out. That is
  what lets it run on `AsyncComputeTaskPool`, and what lets its tests assert exact quad
  counts with no app and no GPU. The neighbours are what made that a live question rather
  than a settled one: they are an **input**, gathered by `ChunkStore::neighbours` on the main
  schedule and moved into the task as `Arc` handles. A mesher that fetched them
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
- **Since #629 the count is the ceiling and a slice of the frame is the rule.** A count
  bounds items; what a stall is made of is time, and the two are joined by a cost per
  chunk that moves by a factor of ten between an optimized build and an unoptimized one
  on the same machine. So `ingest_world_updates` also stops at `MAX_DECODE_TIME_PER_FRAME`
  — 2 ms — whichever bound it reaches first. **Both are read at the bottom of the loop
  body**, after an update has been applied, which is the whole of the progress guarantee:
  a frame that is already over its slice still takes one update off the queue, so the
  budget can slow streaming down and can never stall it. Unloads sit inside the slice
  even though they sit outside the count — wall clock needs no exemption for work that
  costs nothing. The number, what it was measured from and what the values either side of
  it cost are at its declaration; the measurement is re-runnable with
  `cargo test --release -- --ignored --nocapture measure_`, and `DecodeTimeBudget` is why
  the suite's own burst assertions still count rather than time.

- **And that budget is still the whole of the join frame — #651 went looking for what else
  was and found nothing here.** #642 fixed the walking hitch and left a finding it did not act
  on: with the decode spike metered away, the worst join frame was owned by none of the three
  world systems, and the remainder was "the command flush, `refresh_mesh_stats` and Bevy's own
  scheduling". That reading was taken in a build with `opt-level = 0` for this crate *and*
  every dependency. Under the profile #650 shipped it is gone. `measure_what_owns_the_join_frame`
  stamps a fourth system — `refresh_mesh_stats`, the one candidate that was ours — and prices
  the residue against an **idle baseline** on the settled world, which is the control that turns
  "everything else" from a subtraction into a claim. The worst join frame is **1.7–2.4 ms
  dev and 1.4–2.4 ms release**, a factor of about 1.0–1.1 rather than #642's ten, and in five
  runs of six per profile it is the frame `ingest_world_updates` spends its 2 ms slice on
  (79–90% of it). `refresh_mesh_stats` costs **0.002–0.005 ms** there and a median of
  0.0024–0.0038 ms across a whole drain, over 146 meshed chunks — a fiftieth of a percent of a
  60 Hz frame, so the derived counter stays derived. Everything outside the four systems is
  0.09–0.19 ms, and a settled idle frame doing no join work is 0.07–0.16 ms in total: **the
  join adds nothing to the residue that can be told apart from the app existing.** The two
  frames in twelve that were *not* the expansion frame are the useful ones — 1.0–2.0 ms of
  residue while the four stamped systems cost under a tenth of a millisecond and 230 meshing
  tasks were in flight — which is the executor and the operating system, not a system here.
  **So #651 changed no production code, deliberately**, and that was the outcome its
  acceptance criteria named rather than a shortfall.
- **The queue that metering created has a ceiling, and the ceiling is derived.**
  `MAX_DECODE_BACKLOG` is one whole join — `(2 · 8 + 1)³` = 4 913 updates — at a view
  distance the *client* chooses, never `ServerWelcome.view_distance`, because sizing the
  bound from the server's number hands the party being defended against the job of setting
  its own ceiling. A full backlog is at least 154 frames of decode budget, about 2.6 s at
  60 Hz — a floor since #629, because the time slice can end a frame before the count
  does — and that wait is the latency the bound trades the process for. The server's default of 3 is
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
  no face and ordinarily cannot depend on it — and it names them **only when geometry moved**;
  falling water can additionally invalidate horizontal neighbours below.
  The edited chunk still remeshes: colour is its own. A **payload off the
  wire** could have moved any of the six border layers, so `ChunkStore::note_neighbours_stale`
  compares them: vertical faces read opacity and water presence, horizontal faces also read
  level, and missing revisions compare as air.

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

**Colour comes from vertex colours.** `palette.rs` maps a block id to a linear RGBA; the PBR
shader multiplies the material's `base_color` by it, which is why every material's colour is
white and must stay white. On the opaque half the vertex
colour carries one thing more: the mesher multiplies its RGB by that vertex's `Occlusion`, so
the four corners of one quad can differ and an edge is visible without a second attribute, a
second material or a shader. Alpha is never touched by it. An id this build has no colour for
renders magenta rather than a plausible grey — a server one contract ahead should be obvious,
not invisible.

**The second material is water, and the split is by alpha mode rather than by block.** Blending
is order-dependent where opacity is not, and Bevy sorts transparent meshes back to front **per
entity** — so one mesh carrying both kinds would have one sort key and would draw the water
either in front of the lake bed it is seen through or behind the far bank. `mesh_chunk` therefore
returns **three** `SurfaceMesh`es: the chunk's entity carries the opaque one, and the water half and
the cover half hang off it as children, which is also why unloading needed no new code, since
`despawn` takes descendants. A chunk in the middle of a lake gets the parent with no mesh of its
own, because an empty opaque mesh is a draw call that renders nothing.

**The water flow attribute mirrors which sources the server lets feed a voxel.** A plain `Water`
source contributes its full level from every side; a `WaterCurrent*` contributes only when it
points from its own voxel into the flowing voxel being meshed. `palette::water_feeds_toward` is the
client mirror of the server's predicate, and `mesher::flow_at` applies it before summing the level
gradient. The attribute remains a rendering hint — the server decides the block and the swimmer's
motion — but showing a current from a source the automaton rejects would make a lateral water wall
look physically real after the authoritative geometry was corrected.

**The third is cover, and it is split by pipeline rather than by alpha.** A stem, a petal and a
leaf are single planes, so the cover material is `cull_mode: None` and `double_sided` — and a
material is a pipeline, so it is an entity. It is otherwise `AlphaMode::Opaque`, which keeps it out
of the back-to-front sort water needs.

**Nothing in the cover half is swept, and that is what it is for.** `build_cover` walks the voxels
once and grows a shape inside each one `palette::is_cover` answers for: a stem, two leaves, a
five-petal corolla and an eye for each of the three flower ids, and a dry arching cane with short
thorns and no bloom for desert scrub. **The meadow bush left that frame in
#835**: three forked shoots of unequal length leave three separate feet in sectors of unequal width,
their wood tapers from foot to tip, and ten leaves follow the golden angle round them — each a midrib
of three quads that opens, recurves and closes to a point rather than a card. **The winter bramble
followed in #837** and shares that `push_shoot` through a `ShootProfile` rather than a copy of it:
the same branching drawn at crushed proportions — lower and wider than the meadow bush — with a thin
near-white ribbon of snow along the upper side of every segment standing above the drift, and three
bunches of dark berries hanging from dial-chosen twig joints on short pedicels. The desert scrub
(#836) is the last caller of the old four-cane frame and the issue that retires it. Every vertex stays inside its own voxel, which lets
`ChunkStore::apply_block` need no remesh rule for a plant on a chunk border — a neighbour's sweep can
no more see one than it can see the air that replaces it. There is nothing for a mask to merge here,
and for either bramble that is the point: it used to be an ordinary opaque cube, so a cluster of
them merged into one flat slab. Small per-voxel variations are drawn from a hash of the
*chunk-local* coordinate, so a row of plants is a row of different plants and the buffers are still
byte-identical on every remesh.

**The winter bramble never had a fill guarantee, and since #874 no plant here does.**
`world.WinterBramble` has been `Cover` since #790, so no body was ever stopped by that cube: the
species has no collision span to make visible and therefore nothing it has to span, which is what
makes it correct for it to be crushed, lopsided and short of every wall. A test pinning a six-wall
span for it would be pinning a guarantee the server does not make. `BUSH_INSET` still applies for
the reason the paragraph below leaves it — two neighbouring plants must not put coincident quads on
the plane they share — and `WINTER_BRAMBLE_HEIGHT` is the crushed ceiling the shape stays under,
held by a `const` assertion against the profile rather than by a measurement nobody repeats. The
snow is a rule and not a decoration: a cap is drawn only on a segment with both ends above
`WINTER_SNOW_CAP_HEIGHT`, and it faces upward, which
`every_winter_snow_cap_faces_upward_and_only_above_the_snow_line` asserts as `n.y > 0.5` — snow sits
on top of a twig, never under it. The berry **total** is fixed and only its distribution across the
three bunches turns with the seed, which is what keeps the quad count an exact number rather than a
maximum.

**The two brambles used to be where `is_opaque` and `is_solid` parted company, and #874 is the change
that ends it.** Until then, `world.Bush` and `world.DesertShrub` were `Solid` on the server, so a
body was stopped by the whole cube and each drawn bramble had to span it — reaching to within
`BUSH_INSET` of every wall was what kept the collision box from being an invisible wall. #874 makes
both ids `world.Cover`, the **server** change with three enforced consequences this file used to say
was not being made: a body now passes through either one, a placement displaces it, and a plant may
overwrite it, exactly as a flower or the winter bramble already worked. `is_opaque` was already false
for both — drawn with gaps, so the ground underneath kept the top face the old cube culled — and now
`is_solid` agrees: neither predicate needed to change, because both already read `is_cover`. What
changed is `BUSH_INSET`'s reason: it no longer keeps a collision box from being invisible, because
there is no collision box. It survives for the reason that was always the second one — two
neighbouring bushes or scrubs must not put coincident quads on the plane they share — and the shapes
themselves are unchanged, because #874 draws nothing new.

**The quad budget is the thing to watch when any shape changes.** Cover is per voxel and
unmerged, so every quad a plant gains is paid once per plant in the world: a flower is 11 quads, a
bush is 66, and a winter bramble is 70. On a generated meadow chunk with 12 flowers and 9 bushes
the cover half is 726 quads where the two shapes before #634 cost 96, while the opaque half fell by
24 — the bush's cube left the sweep and the ground under it gained the faces that cube was culling.
**The bush was 42 before #835 and 66 after it**, in three steps: three shoots for four canes took it
to 34, their forks to 46, and leaves that are pointed surfaces rather than one quad each to 66. The
taper cost nothing, because a trapezoid has the rectangle's four corners. #835 caps the shape at 96
and says to lower the leaf count before raising that.
**The winter bramble was 36 before #837 and 70 after it**, in two steps: the shared branching shoot
took it to 42 — 30 quads of forked tapering wood where four constant-width canes cost 24, plus six
beads — and the dressing took it to 70, 12 of snow and 28 of fruit where the six beads cost 12.
#837 caps the shape at 72. The snow is the twelve to defend
last: it is what ties the plant to the white it stands on, which is the whole of why it reads at
distance. The one-in-256 tundra fixture carries four winter brambles and therefore 280 cover quads.
`a_flowered_chunk_costs_the_quads_it_is_recorded_as_costing` is where the number is written down.
The desert bramble is 46 quads. The dense desert fixture grows exactly 25 per chunk, so its cover
half is 1,150 quads; in the same generated-terrain fixture the old cube proxy's opaque half was
7,645 quads while bare and shaped scrub were both 7,637. The new shape therefore adds 1,150 cover
quads, removes a net 8 opaque quads after returning the sand tops the cubes hid, and adds 1,142
quads in all. `a_desert_chunk_costs_the_scrub_quads_and_returns_the_sand_faces` pins the cover cost
and exact restoration of the bare opaque buffer.

**The queue still does not distinguish the shapes, and #788 re-measured that instead of assuming
the larger bramble was free.** Meshing runs
on `AsyncComputeTaskPool`, so a mesh that takes longer is throughput and not a hitch; the readings
that would show a cost anyone pays are `MeshStats::queued`, `MeshStats::in_flight` and how long a
chunk waits between arriving and having a mesh entity. `measure_a_planted_join` and
`measure_a_planted_walk` stream one terrain three ways — no plants, bushes as the opaque cube they
were before #634, and the world as it ships. In the #788 optimized rerun over a 343-chunk join,
`Bare` / `CubeBushes` / `Planted` respectively recorded peak `queued` ranges of 144..146 /
138..141 / 134..138, peak `in_flight` of 310..323 / 315..319 / 315..317, and last mesh entities at
229.2..321.2 / 250.7..335.5 / 244.1..322.9 ms. Over four walking crossings the same three modes
recorded peak `queued` of 4..17 / 4..20 / 4..20, peak `in_flight` of 46..73 / 48..73 / 48..76, and
last meshes at 47.4..71.5 / 37.0..56.1 / 35.4..61.0 ms. Those overlapping spreads provide no
evidence that the bramble moved the streaming queue.
**The queue depths are set by `MAX_JOBS_PER_FRAME` and the rate chunks arrive at, not by how long a
mesh takes**, which is why they do not move: the same finding #642 made about
`MAX_APPLIED_PER_FRAME`, one stage earlier. `measure_what_a_planted_chunk_costs_to_mesh` put the
then-new 510-quad cover buffer in context: the planted terrain chunk measured 4.218..5.197 ms (median
4.354 ms), overlapping the bare terrain's 4.030..4.914 ms (median 4.314 ms). Those readings are #788's
and describe the 42-quad bush; #835 re-ran the two streaming harnesses over the 66-quad one and
reports them on its pull requests.

**#789 repeated the same controls for dense desert scrub.** In the optimized 343-chunk join,
`DesertBare` / `CubeScrub` / `DesertPlanted` recorded peak `queued` ranges of 155..169 / 161..171 /
151..158, peak `in_flight` of 326..337 / 328..335 / 318..328, and last mesh entities at
1,254.7..1,330.4 / 1,132.5..1,544.7 / 565.4..687.3 ms. Over four walking crossings they recorded
peak `queued` of 4..8 / 4..11 / 4..8, peak `in_flight` of 48..77 / 49..77 / 49..77, and last meshes
at 173.5..259.3 / 165.3..278.8 / 164.6..274.3 ms. The queue and in-flight spreads overlap both
controls; these runs provide no evidence that the 46-quad shape regressed streaming. The per-chunk
measurement put the same result on one worker: `DesertPlanted` took 7.287..8.335 ms (median 7.338
ms), overlapping `DesertBare` at 7.254..7.525 ms (median 7.305 ms).

**#790 measured the tundra fixture too.** Three winter joins recorded queued 155..163, in-flight
327..330 and worst-frame 2.487..2.778 ms; four walks recorded 4..20, 49..76 and 1.603..2.461 ms.
These are one optimized run's fixture readings, not bounds; exact cost and conservation are tests.

**A shaped plant costs the sweep nothing at all**, and since #652 that is asserted rather than
counted: the opaque and water buffers over ground carrying twelve flowers and nine bushes are
*byte-identical* to the buffers over the same ground carrying none, because `is_opaque` is false for
both shapes and a mask arm that reads "see-through" cannot tell them from the air they replace. The
cube-bush row is the one where that is false, and it has to be — an opaque cube is swept, and it
culls the grass face under it.

**`palette.rs` answers two questions and they are not the same question.** `is_solid` is "does
this stop a body and can it be aimed at" — `solid_at`, the raycast, the camera boom. `is_opaque`
is "does this hide what is behind it" — the mesher, and only the mesher. Water answers no to both.
**The bush and the desert scrub were the first ids to separate them from each other**, between #634
and #874: drawn as foliage with gaps in it while the server still stopped a body with the whole
cube. #874 puts both ids back in step with everything else — `is_cover` is what both predicates
read, so nothing separates them today — but the two functions stay two functions, for the reason
they always were: glass is still expected to be the id that reopens the gap, and the day it does,
only one of these two has to learn about it.

**`world/render.rs` owns no camera.** The world camera lives in `player/camera.rs`; the isolated
view-model camera lives in `player/hands.rs`. Both are created at startup, so the status line and
the held composition have their render paths before a session exists.

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
  complete state is the only thing that changes the displayed contents. The welcome also
  announces a non-empty trailing equipment subset; the pack grid draws only the slots between
  the hotbar and that subset. The inventory screen draws the trailing subset as a labelled
  head/chest/legs/off-hand column beside the pack and routes every press through the same slot-index
  message as an ordinary cell. `EQUIPMENT_ROUTES` may tint a mismatched destination as a courtesy;
  because no off-hand item exists yet, a non-off-hand drop onto that fourth cell sends nothing.
  The four worn ids in a described appearance select optional head, chest and legs overlays on
  world bodies while preserving the off-hand id without adding geometry. That renderer
  reads only the server's latest description; it never infers equipment from the local pack.
- **Item drops are snapshot entities, not pickup candidates.** `player/drops.rs` uses the
  newest `drops` vector as the complete existence set and interpolates positions through the
  same two-snapshot buffer as player bodies. Proximity and clicks are not inputs. Spin and bob
  live on a visual child driven by local time, while the parent transform stays exactly at the
  authoritative interpolated position. Inventory and menu modes hide that parent without
  despawning it, so opening UI cannot be mistaken for a pickup.
- **Projectiles are snapshot drawings, never a client simulation.** `player/projectiles.rs`
  samples each position on the same one-tick-delayed segment as a drop and uses the newest
  velocity only to point an arrow or lay out an orb trail. It integrates no gravity, tests no
  collision and extrapolates nowhere: when the newest complete snapshot omits an id, that body
  is gone immediately. This stops an arrow the server has resolved as a hit from continuing
  past the target on a local guess.
- **Structures are snapshot entities that never move, and that is why they are not
  interpolated.** `StructureState` carries an anchor cell and a `Facing` — no position and
  no velocity — so `SnapshotBuffer::structures` hands the newest snapshot's list over with
  no `now` and no interval to sample at. There is no call a caller could make that would
  blend a building, which is what keeps one off the entity-motion path by construction
  rather than by discipline. The newest snapshot is the complete existence set, exactly as
  it is for mobs: taken back by its owner, collapsed under a broken block and simply out of
  view are one fact on the wire, and this client does not distinguish them.

  **The footprint arithmetic is mirrored from the server and must stay in step with it.**
  `TENT_FOOTPRINT`, `FORGE_FOOTPRINT`, `CAMPFIRE_FOOTPRINT`, `RUNESTONE_FOOTPRINT`, their
  four headrooms and `rotate_offset` in `player/structures.rs` mirror the tables and
  `rotateOffset` in `server/internal/game/structure.go`. The server validates the footprint
  and this side draws it, so a mismatch is a structure that visibly does not cover the ground
  the server says it covers. **The anchor is the ground cell** on both
  sides, and the structure stands in the air above it. The compass is the movement basis —
  North is -Z, East is +X, South is +Z, West is -X — so a yaw of 0 is North, and
  `quantize_facing` resolves the camera's angle once, on the side that has the camera,
  because the contract carries four members rather than a float.

  **One press asks for at most one thing**, and the predicates that keep it that way are
  single functions read from both sides: `combat::blade_in_hand` routes the break press
  between mining and a swing, `HeldItem::structure` routes the place press between a block
  edit and a placement, and `StructureTarget` — which can only ever hold a structure *this
  session owns* — takes the break press away from both mining and the swing. A refused
  placement changes only the status line through `ActionRefused`; a refused removal stays
  silent. `Warded` is additionally the one edit or mine refusal the server answers.
- **The recipe list is a mirror, and a mirror decides nothing.** `player/crafting.rs`
  carries a display-only copy of `recipeTable` in `server/internal/game/craft.go`, for the
  reason `schemas/player.fbs` gives: the wire carries a `RecipeID` and nothing else — no
  ingredients, no product, no station — so there is no claim here for the server to
  disbelieve, and a drift between the two copies can show a wrong label but can never
  create an item. Graying out a row whose materials are short is a courtesy read from
  `Inventory::count`, exactly as `combat::blade_in_hand` is a courtesy, and the same
  predicate is read by the panel that draws the row and by the sender that declines to ask.

  **Proximity is deliberately not mirrored, and the asymmetry is the whole of the rule.**
  Whether a forge or campfire stands within its crafting radius is something the server can see and this
  client can only guess at — the structures a snapshot names are the ones in *view*, not the
  ones that exist — so a station recipe stays clickable from anywhere and says what it needs
  instead of pretending to know. A courtesy that guessed here would produce the one failure
  a courtesy must never produce: a row refusing a craft the server would have granted. The
  craft itself changes nothing locally; the complete `InventoryState` that follows is what
  moves a count, and a refusal is silence.

  **The crafting tab is a bounded viewport, not the height of its rows.** `ALL` preserves
  the complete mirror; `SURVIVAL`, `TOOLS` and `ARMOUR` are local presentation shelves over
  those same rows, carried by `RecipeCategory` and never sent. The wheel moves one
  `ScrollPosition` and the rail mirrors it, so adding a recipe cannot grow the tab below the
  fixed inventory frame. Filtering uses `Display::None`, resets the viewport to the top and
  neither creates nor removes a mirrored recipe.
- **One cell press, four possible intents, and choosing between them is routing rather
  than authority.** A consume press on a known consumable sends a `ConsumeRequest` naming one slot;
  a picked sharpening stone dropped on a slot that wears out sends a `RepairRequest` naming
  the two slots; a shift-click sends a `DropItemRequest` naming one; every other pair sends
  the `InventoryMoveRequest`
  it always sent.

  **Consume has three spellings and two branches, and what keeps a second branch from being
  a second request is the input mode.** `Control::Consume` is the rebindable one, read
  through the same `Bindings` every other control is and defaulting to `C`; middle-click is
  the fixed shortcut it grew out of, kept so a player who learned it does not have to
  unlearn it. Both of those are the pack's, and `inventory_clicks` answers them from one
  branch, which is what makes pressing the key and the button in the same frame over the
  same cell one press rather than two — and two `ConsumeRequest` frames for one intent is
  exactly what a branch each would have produced. The third spelling is that same key with
  the pack **closed**: `consume_selected` sends one request naming `SelectedSlot`, so food
  on the hotbar is a keypress in play rather than a trip into the backpack (#626).

  That third one is a second branch, and it is safe for a different reason than the first
  pair's — not one branch, but two that can never run in the same frame. `inventory_clicks`
  returns early unless the mode is `InputMode::Inventory`, and `consume_selected` is closed
  by `InputGate::may_act`, which is `Playing` and nothing else, so the one key still has
  exactly one reader in any frame. What the two branches share is the half that would
  otherwise have drifted: both end at `consume_request`, still the only place a
  `ConsumeRequest` is built and the only place `CONSUMABLES` is read. Only the slot differs — the
  cell under the pointer, or the hotbar index `SelectedSlot` already carries for a place
  request, out of the leading slots of the same authoritative pack.

  **Interact means two things, and which one it means is the input mode.** Out of the loot
  window `Control::Interact` asks to *open* the nearest accessible corpse; inside it, the
  same press asks to *empty* the one on screen — one `LootTakeAllRequest` carrying the
  corpse id and the revision currently shown, and naming no entry and no count, because
  which stacks fit is the server's answer and not this side's. So a routine kill is two
  presses and no clicking, and clicking an entry still takes exactly that entry.
  `send_loot_intents` reads the key **once, above the mode branch**, for the reason the
  consume press is read once: `just_pressed` is cleared per frame rather than per reader,
  so a second read in the other arm would be a second press to whichever arm ran first.

  The three answers a take-all can come back as are all the ones the window already knew.
  A bare container is a `LootClosed` and the window goes while the corpse remains in the
  world without its accessible highlight; a partial one is a `LootState` of the
  remainder, which the newest-revision guard accepts precisely because the server spent a
  revision on the entries that did move, beside a `TakeLoot`/`InventoryFull` refusal that
  the status line already had a sentence for. Nothing here removes an entry, closes the
  window or predicts a fit on its own.

  **A held key is one press, and that is measured rather than asserted.** The key is
  edge-triggered for the same reason the buttons are, but what makes a repeat harmless is
  `bevy_input`'s `press()` — it arms `just_pressed` only when the key was not already in
  `pressed`, and the per-frame `clear()` never touches `pressed`. `keyboard_input_system`
  does *not* filter `KeyboardInput { repeat: true }`, so a review on PR #403 read the code
  and concluded a held key would drain a food stack frame by frame. It does not, but the
  guarantee belongs to a dependency rather than to this tree, so a Bevy upgrade could take
  it away silently. Four tests hold a key across frames through the real input pipeline and
  pin it: `holding_the_consume_key_reports_one_press_and_a_later_press_reports_again` in
  `ui/inventory.rs`, `holding_the_consume_key_in_play_asks_once_and_a_later_press_asks_again`
  in `player/inventory.rs` — the hotbar's own spelling of the same key, which needs its own
  because it is a second branch rather than a second reader of the first — and
  `holding_interact_asks_to_open_a_corpse_once_and_a_later_press_asks_again`
  and `holding_interact_inside_the_window_asks_to_take_everything_once` in `player/loot.rs` —
  one per thing the interact key now means. All four also press again after a release,
  because a test that only proved "one" would pass just as well with the key dead.

  **Consumable routing follows the kit pattern without copying the server's capability table.**
  `CONSUMABLES` names the ids whose consume press is worth sending, and `consume_request` is the
  only place a `ConsumeRequest` is built. It deliberately does not ask how much hunger an
  item restores, whether a mount is already learned or whether the server will still consider
  the stack consumable: all are authoritative answers. A mistaken extra id grants nothing;
  an omitted id makes a supported consumable unreachable, so the list fails open toward asking.

  Repair's separate judgement is read from the durability already beside every stack —
  `max_durability > 0` answers *does this wear out* with no registry and no second copy of
  the server's table — so the only item ids in that branch are the kits', and those are
  presentation and routing exactly as `combat::ITEM_RUSTY_SWORD` is. `repair_request` is
  the only place a `RepairRequest` is built.

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

  **Drop and consume are the two actions that pair with nothing.** `drop_request` in
  `player/inventory.rs` asks two things and no third: is the index one the contract permits,
  and does the last complete state show something in that cell. It deliberately does *not*
  predict whether the server will accept a slot — that is a gameplay outcome read from a
  pack one message old, and it is the failure direction `combat::LEFT_BUTTON_USES` records, where a
  courtesy that guesses wrong refuses what the server would have granted. A worn blade is
  therefore asked about like anything else; acceptance arrives only through the complete
  inventory and the snapshot's sparse authoritative durability entry. The branch also runs
  ahead of the cursor and leaves it untouched: a picked slot is a source waiting for a
  destination, and neither a shift-click nor a consume press elsewhere is that destination.

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
  place to change it: `may_move` for the horizontal axes, `may_aim` for the continuous world
  questions (the raycast, the outline) and `may_act` for the edge-triggered ones (a request
  leaving this client). **Inventory is the deliberate split:** W/A/S/D keep producing
  horizontal intent while the pack is open, but jump, camera look, aiming and world actions
  remain closed; chat, loot and the pause menu still close movement too. Death zeroes the
  axes rather than stopping the input stream — `PlayerInput` still has to carry the yaw, and
  going quiet would itself be a decision — and the camera keeps turning, because
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
  translation is the authoritative position, interpolated, an eye height above the feet. That
  cosmetic eye height eases between walking and saddle height — and the third-person boom
  eases between `BOOM_LENGTH` and `MOUNTED_BOOM_LENGTH` on the same clock, from the same
  authoritative answer — but the saddle view itself, the ordinary held item and the crosshair
  switch on the exact frame the complete server snapshot adds or removes this player's mount. Its
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

**`player/camera.rs` owns the world `Camera3d`.** It moved there from
`world/render.rs` when movement landed, because a camera that follows a gameplay entity belongs to
the module that knows where that entity is; `world/render.rs` kept the chunk meshes and their
material. `player/hands.rs` owns the deliberate second camera: an origin-anchored, no-clear pass
that sees only the view-model render layer and draws after the world. Keeping its transforms small
prevents the hand's sub-block offsets from being lost when a distant f32 world position is added
and subtracted again. The world camera is marked as the UI default, so `bevy_ui` and the status text
still draw through it rather than through the overlay. `PlayerPlugin` is therefore built **before**
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

**Twenty-two meshes for a whole settlement.** Every player is the same geometry and only the colours
and server-described equipment differ, so each independently moving piece is merged into one mesh
at startup — eleven fixed body pieces, one per hair model and six armour segments grouped under
three of the four equipment slots; off-hand carries no geometry — and nothing is ever rebuilt.
Splitting the arms and legs creates pivots, not
per-player geometry: every body still shares those handles. Armour is up to six optional overlay
children; sleeves and greaves reuse the matching limb pivots, and changing worn ids adds, removes or
re-materialises them without replacing the body. The helmet and cuirass occupy a second
half-notch wrapping tier where hair can cross them, while greaves occupy the first tier over the
trousers, so different materials never share a plane. Materials are keyed on colour plus finish:
the ordinary rig and leather stay rough, while iron is smoother and slightly metallic. The map is
swept against the appearances and worn ids still in view, because a server may describe colours this
client cannot choose and a cache filled by the wire must remain a cache rather than a history.

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

   **The rule is the body rig's and the first-person hand broke it, which is how we learned it
   was only ever checked on one of them.** `player/hands.rs` builds one merged mesh carrying
   colour per vertex rather than a table of boxes, so the check there reads *faces*:
   `no_two_colours_share_a_plane_in_the_hand`. It cost a player seeing the sword drawn through
   the arm — the pommel's side faces and the wrist's were the same two planes to the bit
   (#415). Two things about it are worth carrying to the next rig that needs one. It is
   **colour-aware**, because parts of the same colour are flush on purpose here and a
   colour-blind version fires on them by design. And it reads **axis-aligned faces only**: the
   blade is lofted and its bevels are not, so it can miss a plane and cannot invent one, which
   is the direction to be weak in.
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

`player/sky.rs` owns the one directional light and the curve that five presentation values are
read from: the sun's direction and illuminance, the camera's clear colour, the camera's ambient
term, and the `DistanceFog` on the same camera — plus, since #518, the **horizon**, which is the
colour that fog fades into.

**The rim is a second colour, and the fog fades into it rather than into the zenith.**
`Daylight::horizon` equals `Daylight::sky` at midday and at midnight and blends towards
`DUSK_HORIZON` by a bell that peaks half way through each ramp, so dusk and dawn get the same warm
band by construction — the bell is a function of `night_fraction` and of nothing else, and that
fraction is already its own mirror across the two boundaries the server named. The far edge of the
streamed cube sits on the horizon, so terrain that dissolved into the zenith would dissolve into
the wrong half of the sky the moment the rim had a colour of its own.

**One dome carries the gradient, and it is geometry rather than a shader.** An inverted 325-vertex
sphere at `SKY_BODY_DISTANCE`, unlit, `fog_enabled: false`, with the colour in a `COLOR` attribute
that is rewritten only when the `(sky, horizon)` pair moves — the same "never on an idle frame"
discipline the four component writes already keep, one layer down, because this write is a buffer
upload. It is marked `SkyBody`, which means exactly two things: `follow_the_eye` puts it on the
camera's *translation* after `AimCamera` (a colour one frame late is invisible, a horizon one frame
late slides), and `drive_the_sky` hides it while the eye is under water. It is spawned hidden, so
`ui/character.rs`'s flat backdrop still owns the creation screen.

**There are two suns, and that is the point rather than an accident.** `sun_position` drives the
`DirectionalLight`, whose altitude never drops below `HORIZON_ALTITUDE_DEGREES` because a light
under the horizon shines *upward* — a rendering constraint, not a fact about the sky. So the disc
gets a second curve, `apparent_sun_altitude`, which crosses zero at both of the server's boundaries
and reaches `-MIDDAY_ALTITUDE_DEGREES` in the middle of the night. The two share **one** azimuth,
out of the one `sun_phase`, so the disc sets in the west the light still comes from. Never widen the
apparent curve into the light: `the_light_always_shines_downwards` is what stands there.

**Four things hang on the sky and one `SkyBodyKind` says which is which.** The dome, the sun's disc,
the moon at the antisolar direction, and every star as one mesh — a value rather than four markers,
because every rule here is a `match` on exactly it. A disc is drawn while its altitude is above
`-SKY_BODY_RADIUS_DEGREES`, and is a **fan, not a quad**: nothing here is textured, so a quad's
silhouette is the quad — a square sun whose corners reach `sqrt(2)` times that radius, so its
angular size depends on which way across it is measured. The star field is always drawn, faded by
its material's alpha — which *is* the night fraction — seeded from the constant `STAR_SEED` and
never from `world_seed`, its quads square-on to the field's own centre so the whole field is one
draw that only ever turns. **A world with no clock has no apparent sun**, and that `None` is the
whole of "the fixed sky draws no bodies". Until legacy PR 171 all four were constants — two in
`player/camera.rs` and two in `world/render.rs` — and `Daylight::FIXED` is those same four
numbers, carried over unchanged.

**The sun moved out of `world/render.rs` for the reason the camera did, one issue later.** A
camera that follows a gameplay entity belongs to the module that reads the snapshots; so does a
sun that follows `EntitySnapshot.tick_of_day`. It was in `world/render.rs` for as long as it was
a constant, which is to say for as long as where it lived did not matter. Moving it is also what
kept it from becoming a `world` system reading a `player` resource, which is the same edge
pointing the wrong way.

**Water is the one thing here that is a function of where the player is, and it overrides the
clock rather than blending with it.** When the camera's eye is inside a voxel of `WATER` the
clear colour and the fog colour become `UNDERWATER_SKY` over `UNDERWATER_VISIBILITY` blocks,
whatever hour it is and whether or not the server keeps a clock. **Only those two values move**:
the sun's direction, its illuminance and the ambient term stay where the day left them, so
terrain under water is lit as terrain and *tinted* rather than re-lit.

It decides nothing. The server owns what being in water *means* — `world.Fluid` drives the swim
physics and this client predicts none of it — and both sides read the same id through
`ChunkStore::block_at`, so they agree by construction. The eye comes from the camera's
`Transform`, which `AimCamera` writes *after* the set this system runs in, so crossing the
surface changes the sky on the next frame; the alternative is ordering the sky behind the camera
and the camera behind its snapshot, for one frame of a colour nobody sees arrive late.

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

**The end-to-end path a new item takes — server registry first, this table second — is
`docs/ADDING_AN_ITEM.md`.** What follows is why this half is shaped the way it is.

**`player/items.rs` holds every display fact this client has about an item, one row per id,
and every reader goes through it.** A row is a `name`, an `ItemShape` and an `ItemColour`; the
held view model in `hands.rs` is built from the shape and the colour, `ui/mod.rs`'s
`stack_style` draws the colour on a pack or hotbar cell, the recipe panel spells the name, and
a hovered slot reports it. Adding an item to this client means adding a row, and there is
nowhere else to add half of one.

**The held view model is a hand-and-item composition, not an exclusive choice between them.**
`hands.rs` puts the same fist at the origin for an empty hand and every `ItemShape`, then places a
block, material or bundle on top of it and a blade or tool through its grip. That fist is **one
cube** since #396: the view model's material is `unlit`, so relief on a box 24 millimetres across
is invisible by construction, and three iterations of modelled digits were deleted for costing
geometry and buying nothing.

**That material still has no light, and since #434 the composition carries its own.** `unlit` means
`StandardMaterial` ignores vertex normals outright, so every face of every held mesh rendered one
colour — and two later changes were authored against a light that was not there.
`BLADE_RIDGE_FRACTION`'s doc claimed six faces per span meant "the light catches a different pair
as the hand turns"; #426's pitting displaces vertices in `x` alone and deliberately preserves the
outline, so under a flat colour a pit changed nothing anybody could see. In the hand the rusty
sword showed its livery's dark smudges and never the shape beneath them, while the *same meshes*
showed their facets on the ground, because `drops.rs` mints a lit material. One asset read as two
different objects depending on which surface drew it.

`hands::shaded` folds `dot(normal, SHADE_LIGHT)` into the vertex colours of the whole first-person
composition — fist, wrist, arm and held item — at build time. It **multiplies**, so it composes
with the item's colour, a grip's wood and a bundle's straps; a fully lit face is identity, so
nothing is ever brighter than what `player/items.rs` says, and `SHADE_FLOOR` keeps the far side a
shade of the steel rather than a silhouette.

**Two costs, both deliberate.** The light is in model space, so it turns with the sword instead of
staying fixed in the world — under `unlit` there is no correct answer available, and a baked light
is what a hand-painted low-poly asset does. And the pass is applied where the arrangement is
*composed*, not where the geometry is *built*, which is what keeps it off the drop: baking a second
light into a mesh a lit material draws would add to the real one.
`the_dropped_sword_is_not_shaded_twice` is what holds that seam, because "it is applied somewhere
else" is a claim about a call site and call sites move.

Whether a shaded fist is now worth re-modelling is a live question again and its own issue — #396's
reasoning was about a light that did not exist, and one does now. What reads as a hand is the silhouette — a block at the end of a
narrower wrist. The two are merged
into one mesh and carry absolute vertex colours — skin from the local player's `Appearance`, item
from this table — under one white material. One stable mesh asset is rebuilt in place only when
the selected item or skin colour changes, so arbitrary server colours cannot grow a cache and all
three swing shapes still move one transform.

**A sword's grip is turned wood, and the wood is reached by division rather than written down.**
The gladius' three furniture pieces were boxes; the grip is now a cylinder of `GRIP_SIDES`
inscribed in the box it replaced — same height, radius `GRIP_SIZE.x / 2` — so the three
`const _: () = assert!` blocks between `GRIP_SIZE` and `HAND_SIZE` are unchanged, because a
cylinder that stays inside the old extents satisfies every component-wise comparison they make.
A vertex colour **multiplies** the item colour, so a mesh can only reach what is darker than its
item in every channel; `wood_over` divides `palette::LOG` by the blade's own steel at build time,
which lands the rusty sword's grip and the iron sword's on exactly the same wood. A single
hard-coded multiplier would give them two different woods, silently, because `ForgedSteel` is
brighter than `WornSteel`. A blade too dark to reach `LOG` in any channel gets steel and a log
line rather than a colour nobody chose — unreached today, and swept.

**The world draws a sword in two pieces, because its grip is not steel.** `hands.rs` reaches its
wood by dividing `palette::LOG` out of *that* blade's own colour, which the world cannot do:
`DropVisuals` caches **one mesh per shape and livery**, shared by both blades and coloured by a
per-item material, so a tint divided out of one steel and baked into that mesh is right for one
sword and quietly wrong for the other. #419 therefore shipped the turned grip without the wood and
pinned the gap.

#435 closed it without touching a cache key. `sword_mesh_with` answers the sword **without** its
grip, `sword_grip_mesh` answers the grip alone, and the world draws the second as a child with an
absolute `palette::LOG` material shared by every blade — the arrangement a bundle's straps have
always used. An absolute colour needs no division, so a third blade in a third steel costs
nothing. `DropVisuals::second_piece_for` is the one place that pairing lives, read by the ground
drop *and* by the body's fist, so the two cannot disagree about what a grip is made of.

**The first-person hand is unchanged**: still one mesh, one material, and still the division. Two
surfaces, two arrangements, one shape.

**Two independent readings guard a new solid, and signed volume is the general one.** A ring
walked the wrong way round builds a part inside out; back-face culling then discards its front
faces and keeps its back ones, so the result is a solid that renders transparent — what
`BladeSection::perimeter` calls "a sword that vanishes when you look at it".
`every_solid_in_the_sword_is_wound_outward` sums the divergence theorem over every triangle and
requires a positive volume, and asserts the reversed winding reads negative so the test is not
proving the mesh with the mesh. It covers the cylinder and every solid added after it.

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
in `items.rs` because drawing them is all anyone does — including bones, a vargr pelt and
raw meat. Products this side routes live with their recipes: a leather patch is a repair
kit and cooked meat is food. The stablemaster tokens are the narrow exception: their canonical
mount presentation was established in `items.rs`, and `inventory.rs` imports those same constants
for consume-intent routing instead of moving or copying their identity. The registry names every
id from wherever it lives: one declaration read from several places cannot drift the way two
declarations of the same number can.

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

`combat::LEFT_BUTTON_USES` is the list of item ids this client routes the left button to an
attack for: both blades and the bow. It replaced an `item_id ==
ITEM_RUSTY_SWORD` comparison — one weapon's name spelled inside the routing — for the reason
`armedWithSwordLocked` became `armedForAttackLocked` on the server and started reading either
`meleeDamage` or `launches` out of the item registry: **a new weapon should be an entry, not an
edit to the predicate.**

Three things about it are worth keeping straight.

- **It decides nothing, and its two failure directions are not symmetric.** The server
  re-reads its own registry for every swing, so an id wrongly listed here costs a request
  that is refused — nothing granted, nothing lost. An id wrongly *omitted* costs a weapon:
  that is precisely what the iron sword was between legacy PRs 109 and 127, drawn as a blade in the
  hand, worth 40 damage on the server, and never once asked for, because this client would
  not send the frame. A table that fails open toward asking is the honest shape.
- **`attack_item_in_hand` is the stack question; `item_is_a_blade` remains the narrower
  presentation test for sword shapes.** The stack also has to be there and not worn through,
  where **worn through means zero durability under a non-zero maximum** — the same pair the
  server reads, and never the current value alone. `max_durability > 0` is already this
  client's answer to *does this wear out* (`inventory::repair_request` asks it that way), and
  a weapon registered with no maximum would arrive as `(0, 0)` like every resource does; the
  narrower test would call it broken on arrival and refuse a swing the server would grant.
- **The hand-drawn attack shapes are pinned to routing by tests, not by discipline.**
  `items::ItemShape::Blade` decides which items *draw* as a blade and `combat::BLADE_SHAPES`
  records that narrower shape set; `LEFT_BUTTON_USES` additionally names the hand-drawn bow.
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

**A bundle is one rolled shape recipe at two scales.** `rolled_bundle_parts` supplies the
first-person hand and the world drop with a horizontal roll and two raised straps inside each
surface's existing bound. The roll keeps the item-table colour; the straps use the one shared
brown. World drops keep those as two visual children so one material can stay item-coloured and
the other brown, while the first-person white material reads both as vertex colours.

**The rusty sword's oxide was fourteen small boxes merged into the blade, and it is a livery now.**
The colour was always right — a multiplier, so `player/items.rs` stayed the one answer to what the
steel is — and the silhouette never was: a box has six faces, three visible at once, and a hard
edge against the steel at every angle, so at any distance fourteen of them read as *damage*, which
is the exact thing the constant's own comment said they were made small to avoid. A per-vertex
patina would have fixed that and kept the other half of the defect. **The rust lived in one
renderer only** — `hands::item_mesh` reached it through `if item_id == ITEM_RUSTY_SWORD`, while
`drops::drop_mesh` served the ground drop *and* the third-person fist from plain `sword_mesh`, and
`ui/icon.rs` drew flat rectangles — and a patina has to be regenerated in every mesh at every
scale, with no vertices in the cell to tint at all. An asset is consumed from several places
without anybody copying it, so agreement between those surfaces becomes handle identity. Pointing
the other three at it is its own issue.

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
Five colours cross the wire and six colour parts wear them — see "The rig" below for the sixth —
while twelve independently placeable pieces let each arm and leg rotate around its joint. The table
says which part takes which field and where each of its boxes sits, in notches of the collision box
rather than in metres. `ui/character.rs` and `player/mod.rs` consume those same pieces and shared
meshes. Two tables would be two answers to "what does a shirt colour cover", and the first thing two
answers do is disagree.

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

  **"A session exists" is not the same question as `Handshake::established`, and `peer_closed` is
  where that cost something.** The character phase is a screen, not a status line: the server has
  answered the hello with this account's characters, so a clean close there — which is what
  `-character-timeout` expiring *is*, closed deliberately and without a reply — is a session
  ending. Asking `established()` made it a refusal reading *"closed the connection before
  answering the handshake"*, a sentence untrue of the one case that reaches it most often (#627).
  `peer_closed` therefore matches `Handshake::phase` exhaustively: `Established` and `Choosing`
  end, `AwaitingCharacters` and `AwaitingWelcome` refuse. A fifth phase must not compile until
  somebody decides which it is.
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

**Chat owns text, not the world.** `InputMode::Chat` keeps the cursor captured and leaves mobs,
the authoritative vital bars and the selected held-item presentation visible, while the same
`InputGate` that closes inventory and menu input closes aiming, actions, movement and camera
sampling. The inventory, crafting screen, hotbar, crosshair, settings and pause menu remain hidden;
`T` is a normal stored `Control::Chat` binding, while Enter and Escape belong to the text field and
are intentionally not rebindable. The opening frame drains its `KeyboardInput`, so the key that
opened the line cannot become its first character.

**Only locally typed text is command syntax.** `/invite`, `/accept`, `/decline`, `/leave` and
`/kick` become typed `PartyRequest`s; every other non-empty line becomes a `ChatRequest`.
Ordinary chat is trimmed, while an otherwise-unrecognised line whose first character is `/`
reaches that encoder byte-for-byte so the authoritative server is the only parser. A `ChatMessage` or `PartyInvite`
received from the server is display text only, bounded and stripped of layout controls before Bevy
sees it, never parsed or used as identity. Both share one `ChatInbox`, which preserves their
relative wire order for its first consumer. The log holds eight lines, fades them after twelve
seconds of `Time<Real>`, and shows all
eight fully while chat is open; there is no persistence, scrollback, timestamp or channel state.

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
  list — not an empty one. The screen offers a retry, and a reconnect if this client has already
  been on a server; check that `--account-service` names a service that is up.
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
  app at all. `MaterialPlugin` is safe there too: it creates the render-app systems only
  `if let Some(render_app)`, so on a headless app it registers the asset and nothing else.
- `world/water_material.rs` adds `init_asset::<Shader>()` and `init_asset_loader::<ShaderLoader>()`,
  the two registrations `RenderPlugin` would otherwise make, and loads the embedded WGSL through
  the real `AssetServer`. That proves the path, the registration and the WGSL preprocessor.
  **It does not prove the shader compiles**, and the one test that does is `#[ignore]`d because
  it needs a render device — see "Known gaps".
- `world/mesher.rs` and `world/palette.rs` need none of that: they are plain Rust.

**A test that needs a concrete port must hold it, never learn it and let go.** The only way to
find a free port is to bind one, and the obvious helper — bind `127.0.0.1:0`, read the number,
drop the listener, return the number — hands out a port that belongs to nobody from the drop
until the code under test binds it. `cargo test` runs one binary on many threads and this crate
asks the kernel for ephemeral ports all over it (`net/mod.rs`, `net/tls.rs`, and the sign-in
tests' own fake account service), so a sibling's `bind` can be given the number that was just
released. The sign-in tests are where it was paid for: the loser's `bind` fails, the worker
returns, its `Sender` drops, and the test's `recv_timeout` reports **`Disconnected`** — a
failure that reads as "the client never opened a browser" and reproduces on nothing. It turned
`develop` red on a server-only merge (#542, #557).

So `net/signin.rs`'s `reserved_loopback_port` returns the listener alongside the port and the
test hands *that* to `signin::Loopback::Prebound`: the port passes from the test to the attempt
without an instant of being free. `Loopback` is a `cfg(test)` seam of the same shape as
`Browser` — one production variant, which binds the redirect's port exactly as before — and the
`Prebound` arm asserts the handed-in listener is on the redirect's port, so the seam cannot
become a way to test a port the browser was never sent to. Serialising those tests behind a
mutex would not have been enough: the competing binds are not all sign-in tests.

**The rule has exactly one exception, and it lives in the same crate — so read this before you
read it as a contradiction.** `net/mod.rs` keeps two `closed_port()` helpers, one in
`sign_in_tests` and one in `server_list_tests`, that still bind, read the number and drop the
listener. They want the opposite thing: a port *nothing is listening on*, so `start` fails fast
and deterministically. Holding it is not merely unnecessary there, it would destroy the property
under test, so "hold it" has nothing to say about them. Their residual race is real and it is a
different kind: a sibling handed the released number makes the connection *succeed* where the
test expects it refused — a loud failure on the assertion, not the `Disconnected` ghost above
that reads as a different bug entirely. It has never been observed and is left alone rather than
fixed blind (#557). **Any other bind-read-drop helper is the bug, not a third exception.**

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

- **CI compiles no shader, and the water shader is the first one that would notice.** Four
  things about `world/flowing_water.wgsl` are checked on every run — the path resolves, the file
  is embedded at it, the WGSL loader parses it, and the imports it declares are the `bevy_pbr`
  modules it is composed from — and none of them is compilation. Composition (`naga_oil`
  splicing those modules in, naga validating the result) happens in `ShaderCache`, which is only
  ever reached from a `RenderDevice`; the `client` job installs `libasound2-dev libudev-dev
  pkg-config` and nothing else, so wgpu opens no adapter and there is no software one either.
  **A shader that does not compile therefore turns nothing red**: Bevy logs the error and the
  water draws with the base material's fragment shader, which looks like water that has stopped
  moving rather than like a failure.

  `the_shader_compiles_through_the_real_pipeline` in `world/water_material.rs` is the test that
  does answer it — `DefaultPlugins` without a window, a camera rendering into an `Image`, one
  water quad, and every `PipelineCache` entry read back for an `Err` — and it is `#[ignore]`d
  for exactly the reason above. Run it by hand on a machine with a GPU after touching the WGSL;
  it takes ten seconds. It was verified against a deliberately broken shader before it was
  trusted, which is the only thing that makes a passing run mean anything.

  #598 raised this rather than adding `naga` as a direct dependency to parse around it: a second
  parser agreeing is not the pipeline agreeing. Closing it needs a render device in CI —
  lavapipe on the runner, or a runner that has one — which is a CI change and not a client one.
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
- **The settings screen is not every setting, and what it leaves out each has a reason.**
  #179's second half landed in #247 — the original six graphics values, the four-corner frame-rate
  readout, the two tabs and one reset per tab — so what remains outside is deliberate:
  *cursor capture*, which belongs to the camera-control issue this file has named for a
  while and that still does not exist; *audio*, of which there is none; and *shadows,
  ambient occlusion and texture quality*, which have no shadow map, no AO pass and no
  texture behind them. Nor the pitch limit, which `player/constants.rs` explains is an
  invariant rather than a preference.
- **A reset is scoped by a tab, and the obvious implementation is the bug.** `Settings::reset`
  names one tab's fields; writing `Settings::default()` back would look right on the tab
  being reset and would silently clear the other — a button labelled "reset graphics"
  throwing away every key binding a player had set. The two directions are asserted
  separately, in the model and again through the screen, because a happy-path test passes
  either way. A Controls reset goes through `Bindings::from_pairs`, the same
  whole-assignment validation the file is read with: two controls that traded keys cannot be
  restored one rebinding at a time.
- **The settings screen's content area is a fixed height, and `ui/inventory.rs`'s is not.**
  Both draw a strip above their halves; that one sizes the panel to whatever the visible
  half needs, so the strip moves when a player switches tabs, which is what #251 is about.
  This one gives the area below the strip `CONTENT_HEIGHT` whichever tab is up, and
  `no_tab_needs_more_rows_than_the_area_it_is_drawn_in` fails when a row is added past what
  that height was sized for — rather than the panel quietly growing and taking the strip
  with it.
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
- **The preview is the real rig, in the world, turning in front of the world camera.** It is dressed
  out of the same wardrobe a body is — `player::BodyVisualsPlugin`, the same meshes and the same
  material per colour — so it cannot disagree with what a player will see of themselves. It was
  flat `bevy_ui` nodes until #181, with a hand-written painter's order standing in for a depth
  buffer; that went, and `PlacedBox::nearness` and `appearance::slots` went with it. **There is
  still exactly one world camera** — the hands' second camera sees only its private layer, so the
  preview adds no camera and no render target.
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
- **No *automatic* reconnect, backoff or session resumption — and one button.** A dropped
  connection is reported and stays reported, with nothing set to try it a second time. What #627
  added is not a policy but an affordance: `ui/servers.rs` draws `RECONNECT` while a session is
  over and there is a `ServerAddress` to go back to, and a press writes one `ReconnectRequest`
  that `net::reconnect_on_request` turns into exactly one dial on the `RejoinBy` route the session
  was opened by. **The press is the whole of the trigger.** There is no timer, no backoff and no
  countdown behind it, it inserts no `Rejoining`, and a client that redialled on its own would be
  hammering a server that had just closed it. A `Row` route is answered by writing the very
  `ConnectRequest` a click on that row writes, and an `Address` route shares
  `dial_recorded_address` with the rejoin, so there is one dial path rather than a second one.

  `Rejoining` has exactly two writers,
  both with a complete remedy on the same route: `disconnect_on_request`, after a player asks to
  leave a world, and `drain_session_events`, after `CHARACTER_NAME_TAKEN` or
  `CHARACTER_NAME_REFUSED` answered a creation. The second is request recovery rather than session
  recovery: the server closes by contract, the client dials once, and the new list re-enables the
  form. Every other refusal and every unasked ending sets no flag.

  A deliberate disconnect is also not locally complete. It sends the empty `LeaveRequest` and
  retains the session socket while setting its bounded `Outbound` sender aside: an accepted
  cancellation restores that sender on this same connection, so there is no second writer to invent. `InputMode::Menu` closes
  gameplay and releases the pointer while `ConnectionState::Leaving` is up; the pause panel is
  hidden, and `send_player_input` therefore carries only inert input until the answer. Esc sends
  the empty `LeaveCancelRequest` once and changes only the display to pending. A refused
  `LeaveCancelResult` refreshes the local presentation deadline from the server's milliseconds and
  leaves the gate closed; only `accepted=true` restores `Connected`, drops `Rejoining` and lets the
  ordinary changed-state path restore play. A close that arrives first still removes the session
  and permits the one rejoin, so the countdown wins the race exactly where the server decided it.

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
- **The pointer has three states, and focus loss is always the released one.** A focused live
  session uses `Locked` and hides the pointer for play and chat; panels keep it visible under
  `Confined`; login, server-list, disconnect and every unfocused window use `None`. A
  `WindowFocused` transition writes that answer even when `CursorOptions` already contains it,
  because the compositor can drop a native constraint without changing Bevy's component. Bevy
  falls `Locked` back to `Confined` on X11, where true locking is unsupported; that platform
  distinction is why headless policy tests are evidence about ECS state and not about native
  multi-monitor behaviour.
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
  server's complete `EntitySnapshot.dead_players` list and mob falls follow the snapshot's
  `MobAction`; a client that drew neither would be dead for the same three seconds, respawn at
  the same moment, and pick up a draugr's bones at the same moment. Since #441 that last one is
  *immediate*: a killing blow produces the lootable corpse on the tick it lands, so the snapshot
  that first says `Corpse` is the snapshot the fall starts on and the snapshot F already works
  on. `nearest_accessible_corpse` reads `mobs` and `accessible_loot_corpses` and nothing else —
  no `Mob` component, no `falling`, no `FALL_TIME` — which is what makes "press F while the body
  is still going over" a property rather than a coincidence.
- **There is no mirror of any server death number on this side, and there is no longer one to
  mirror.** `FALL_TIME` is 700 ms of presentation with nothing waiting behind it. It used to be
  argued against `MobDeathDuration`, the two and a half seconds the server spent between the blow
  and the corpse; that constant is gone. A body's fall is a curve that finishes, and what ends a
  death is the server no longer sending the creature, which despawns it through the branch one
  that walked out of view takes. `MobAction::Dying` is still in the contract and this server never
  sends it — the client still handles it, because a wire enumeration is not narrowed by one
  server's choice not to use a value.
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
- **Other players are the rig, a distance-driven walk, one server-driven death pose and one
  server-owned name plate.** The plate is screen-space UI projected from the body's head anchor:
  its pixel size stays readable with distance, it follows the same transform as the body, and it is
  removed with that body. The local player gets no plate in either view, including third person.
  Display text is bounded and controls are replaced before layout, while empty and arbitrary
  Unicode names remain valid. The local body mirrors the selected authoritative pack item into its
  hand in third person; other players still carry no held-item fact on the wire, so drawing one for
  them would be a guess. There is no combat animation, no other equipment on the body, no faces
  beyond the two eye boxes, and no texture anywhere — it is coloured geometry. Each of those is
  its own issue.
- **An entity can be drawn before it has been described, and is never re-spawned when it is.** The
  appearance stream and the snapshot stream are not ordered against each other, so a body whose
  `PlayerAppearance` has not landed wears `codec::PLACEHOLDER_APPEARANCE` — the neutral grey
  `schemas/player.fbs` documents — and `dress_bodies` swaps the existing piece handles in place the
  moment it does.
  Despawning and re-spawning would restart the interpolation and blink the body. The server sends
  the appearance *ahead* of the snapshot that first carries the entity where it can, which makes the
  placeholder rare rather than impossible.
- **The body cache is the size of a view; the appearance cache is the view plus the party.** An
  entity that leaves both takes its cached appearance and server-owned name with it, and
  `Player.described` on the server drops its own entry at the same moment — so a player who walks
  back into view is described again and neither side had to be told. Party membership is the one
  server-sent reason an out-of-view description remains live: `ui/party.rs` still has a row to
  name. The one case neither complete set can answer is a `PlayerAppearance` description for an
  entity no snapshot has ever mentioned; it is held for `APPEARANCE_GRACE` and then dropped, which
  stops a server that describes entities it never shows or groups with from growing a map for as
  long as the connection lasts.
- **No cross-chunk lighting, shadows, LOD or frustum-driven requests.** One directional light
  with shadow maps off, plus a per-camera ambient term and a per-camera distance
  fog — all three on the server's clock — and one `PointLight` per campfire in view, which is on
  nobody's clock and casts no shadow either. **The fire's light is the one light in this client
  that is not the sky's**, and its reach is a presentation number that deliberately falls short of
  `game.CampfireSafeRadius`: the ground a fire keeps clear is checked in `spawn.go` and is not
  something a renderer may draw a boundary around. None of the four casts a shadow. Each of the rest is its
  own issue. **Ambient occlusion is no longer one of them** — #628 landed it, in the mesher, as
  `Occlusion`: each opaque quad's four corners count the opaque voxels touching them on the
  outward side and darken that vertex's colour. It needed a chunk's neighbours and border culling
  had already put them in the mesher's hands, so it reads those and nothing else — a corner whose
  sample lies in a **diagonal** chunk reads as air, because `Neighbours` carries six and the
  mesher asks for nothing it was not handed.

  What it cost is the thing to know before touching the sweep. Greedy merging does work against
  per-vertex occlusion, exactly as this file said it would: the four corner levels are part of the
  `Face` mask key, so two faces lit differently cannot merge, and a ridged 32³ terrain chunk went
  from 4 642 quads to 7 637. That is the price of the seam and it is paid once — a later change
  that multiplies it *again* has to say so in a diff, which is what
  `the_quad_count_of_a_chunk_of_terrain_is_recorded` is for.
- **No font asset either, and that is a constraint on what the UI may write.** Bevy's
  `default_font` is the whole font stack: `FiraMono-subset.ttf`, embedded in `bevy_text`, whose
  `cmap` holds exactly 95 glyphs — every printable ASCII codepoint and nothing else. **A codepoint
  it does not have is laid out with zero advance**, not drawn as a box and not logged, so a string
  containing one is silently shorter on the screen than it is in the source. Six characters —
  `°` `·` `—` `…` `♛` `⚔` — reached twenty-one sites across eleven modules that way, from the
  compass onward, and every test agreed with them because every test compared the formatted string
  against the same literal (#481).

  So **every string this client composes is ASCII**, and
  `ui::ascii_guard::every_string_the_client_composes_is_ascii` is what keeps it that way: it walks
  the crate's own source and fails on a non-ASCII character in any string or character literal the
  production build compiles. The scan is the whole crate rather than a list of the modules that
  draw, because text reaches the screen from further away than `ui/` — a name plate's level is
  composed in `player/`, the field of view in `settings/`, and the line under the login control is
  a `tls::ConnectError` message written in `net/` — and a list kept in step with a directory by
  hand falls behind it. Generated code and `tests.rs` are outside it; a test may name a hostile
  string in a script this font cannot draw, and several do.

  A pictograph with no ASCII spelling becomes **geometry**, in the style `ui/icon.rs` established:
  `ui/party.rs` draws the crown and the crossed swords as a handful of `bevy_ui` rectangles in the
  row's own colour, which is what the characters got from `TextColor` for free. Shipping a real
  font is the other answer and a larger change — a file, its licence, an asset load path the
  headless tests must tolerate, and a decision about which font — and it is the one to reach for
  if the UI ever wants typographic characters generally.
- **No texture atlas, and no art assets — but there is one generated image, and the meshes that
  sample it carry real UVs.** `palette.rs` is still the whole *terrain* material system: a colour
  per block id, carried as vertex colours, on **opaque** chunk meshes with no texture coordinates
  at all.

  **The water half is the exception, and it samples nothing.** Since #598 the water surface carries
  `ATTRIBUTE_UV_0` and `ATTRIBUTE_UV_1`, and neither is a texture coordinate: UV_0 is the
  horizontal flow the server's block ids imply, UV_1's `x` is whether the water is falling, and
  `world/flowing_water.wgsl` slides a procedural two-octave ripple along them. They ride in the
  UV slots because `MeshPipeline` already forwards those two to the fragment stage under
  `VERTEX_UVS_A` / `VERTEX_UVS_B` — a custom attribute would need a vertex shader and a layout
  specialization of ours, for the same four floats. The opaque half still carries neither, which
  is `SurfaceMesh`'s all-or-nothing rule: the buffers are filled for every vertex of a surface
  whose faces carry a flow and empty for one whose faces do not.

  **Three waters, and since #655 they read as three.** #598 gave all of them one amplitude —
  `RIPPLE_DEPTH = 0.08`, argued as a ceiling so that moving water was still the same colour it
  had been. On a translucent blue surface at play distance that was not visible at all, and a
  river could not be told from a lake. The falling branch was worse off still: it had never once
  run, because the server wrote only water *sources* until #653 gave the flow automaton a way to
  make a fall and #654 wrote falls into the terrain.
  Each state now differs from the others in three ways, not one — how deep the ripple cuts, how
  far its crest is pulled toward white, and the *shape* of the pattern. The third is the one that
  carries it: still water is isotropic swell, a current is stretched along the way it runs, and a
  fall is stretched down the wall it falls on. Foam is the half that is not brightness, which is
  the difference between water moving and water under a stronger lamp; still water has none, and
  that absence is one of the three differences rather than an omission.
  The numbers live in the WGSL and nowhere else. `water_material.rs`'s tests read them back out
  of `SOURCE` rather than restating them, and what they pin is the *ordering* — still quieter
  than a current, a current quieter than a fall — because that is what makes three things
  readable as three, and it is what a retune must not invert by accident.
  **Since #673 the two colour halves of the pattern take two lighting paths.** Ripple depth still
  modulates `base_color` before `apply_pbr_lighting`, so it changes the diffuse water and never the
  specular highlight the standard material computes. Foam enters `StandardMaterial.emissive`
  instead, which PBR adds after direct and ambient lighting: at noon it is small beside the sun,
  and at night it remains the scattering cue that says a current or fall is moving. It emits no
  light into the world. The depth and foam constants did not move — their ordering and daylight
  look were already the right ones — only foam's path through lighting did.
  **The same wave now has a third, bounded shading output: its analytic gradient perturbs
  `PbrInput.N`, so the low-roughness highlight moves instead of sitting on a perfectly flat
  sheet.** The gradient is projected onto the actual face before it is applied, which lets one
  expression serve a horizontal river and either orientation of a falling wall. The strength is
  deliberately small, and `world_normal` is not changed: geometry and shadows still describe the
  flat surface at the level the server sent.
  Greedy meshing merges quads across blocks, so a texture there is a different problem with
  different costs — seams across merged quads, an atlas, a per-chunk material decision — and none
  of it is on the table. Item-only swatches live in `player/items.rs` beside the rows that name
  them and resolve to the same linear RGBA shape every renderer consumes.

  What changed is **items**. `player/livery.rs` generates one `Image` at startup from a fixed seed
  and hands out one handle; an item's row names the livery it wears, or `None`, which is what
  almost every row says. `MeshBuild` now emits real coordinates — `u` around the blade's six-corner
  perimeter, `v` along it — and `livery::field` is the one function both the texels **and** the
  blade's own vertices are read from: a liveried blade is lofted through 31 rings instead of 3 and
  each vertex is displaced inward where the field is strongest. Corrosion eats metal rather than
  sitting on top of it, so a livery that only tinted would be paint.

  The displacement is in `x` alone, which is what keeps the outline the outline: the two corners
  of the hexagonal section that sit on the blade's edges have no `x` to lose, so a pit can only
  ever eat through a flat. That makes "no displaced vertex leaves the blade's envelope" a property
  of the arithmetic rather than something to check afterwards, and `no_pit_leaves_the_blades_envelope`
  measures it against `blade_surface`'s closed form anyway.

  **Row 0 of that image is pure white and everything else points at it.** The first-person hand is
  one mesh and one material — fist, wrist, arm and held item — so a material carrying an image
  means every cuboid in the composition has a coordinate that now matters; Bevy's primitives
  generate coordinates spanning the whole image, which would wrap the rusty sword's oxide around
  the player's knuckles. `hands::neutral` points all of it at that white texel, identity for a
  multiplier, so an un-liveried item draws exactly what it drew before the image existed. That is
  what lets one material serve a liveried blade and an un-liveried one in the same draw, and
  `every_held_arrangement_samples_only_the_livery_it_owns` is the sweep that holds it.

  No file asset, no image codec, no new dependency and no new Bevy feature — the image is
  generated, and it is committed to nothing.

  **`App::init_asset` is not idempotent, and believing it was took the client down on startup.**
  `livery::register` opened with `app.init_asset::<Image>().init_resource::<Liveries>()` and a doc
  comment saying both calls were no-ops if the asset already existed. `init_resource` is;
  `init_asset` ends in an unconditional `self.insert_resource(assets)` on a freshly defaulted
  store, so calling it after `ImagePlugin` **throws away every image the renderer has loaded**.
  `FallbackImage` is among them, its D3 entry is what the mesh view bind group binds when there is
  no irradiance volume, and the game died with `Texture binding 18 expects dimension = D3, but
  given a view with dimension = D2` before it drew a frame.

  **Nothing in this suite could have seen it, and that is the part to remember.** Every test here
  is headless: no render app, no `FallbackImage`, no bind group to validate — and each test
  *builds* `Assets<Image>` itself, so the reset lands on an empty store and changes nothing. All
  four gates were green, the review was clean, and it was found by running the game. The
  headless half is now pinned by `registering_twice_keeps_the_images_already_loaded`, which
  asserts that a **foreign** image survives a second registration — the livery's own would be
  re-created by `FromWorld` either way, and asserting on it would have passed against the bug.

  The general rule this is the second instance of: **a claim about somebody else's API is a claim
  about the world, and the ones that cost most are the ones a green suite cannot contradict.**
  Read the function before writing "idempotent" in a doc comment.

  **Four surfaces draw one item, and since #418 they cannot disagree.** The rust used to be
  reached by `if item_id == ITEM_RUSTY_SWORD` inside `hands::item_mesh` and nowhere else, so the
  hand had oxide while the ground drop, the third-person fist and the inventory cell drew clean
  steel — a divergence that had always existed and that nothing measured. Agreement is *handle
  identity* now: `all_four_surfaces_that_draw_a_sword_sample_one_image` reads the handle off the
  running view model's material, off `DropVisuals::material_for`, and off a real `ImageNode`
  component, and requires all three to be the one the `Liveries` resource minted.

  **`DropVisuals` is keyed on `(ItemShape, Option<Livery>)`, both halves of it.** The material
  half needed the livery because a livery arrives as `base_color_texture`; the **mesh** half
  needed it too, because a livery decides geometry — the rusty blade is pitted and the iron one
  is not, so the two stopped being one mesh. That is a widening of the existing key rather than
  an item-id exception smuggled into a shape-keyed cache: two items sharing a shape *and* a
  livery still share one mesh, which is asserted, because sharing is what the cache is for. The
  liveried entries are built from the pairs `items::liveried_shapes` reports rather than from the
  cross product, so the cache holds exactly what can be drawn.

  **A livery belongs to a material, not to an item**, which is what keeps the second one a row
  rather than a generator. Roughly thirty item ids and about six materials among them: a haft, a
  bow stave, a shield plate and a sceptre shaft are the same wood; a helm, a cuirass, greaves and
  the iron sword are the same forged iron. `Livery` names materials, every row in the item table
  names one or names `None`, and `livery.rs` contains no item id in any match arm.

  **The column is explicit per row and is never derived from `ItemColour`**, which is the finding
  that decided the shape of it. `ItemColour::Block(palette::LOG)` is worn by the log, the campfire,
  the wooden shield, the bow and the sceptre — and also by the **axe**, whose swatch is the ground
  it works rather than what it is made of, and by the **leather patch**, which is bark-coloured
  worked hide. Two of those seven are not wood, so a livery inferred from the colour would grain
  them both.

  **A livery has to earn its place, and the default answer is no.** Which materials have one, and
  why the rest do not:

  | Material | Livery | Why |
  | --- | --- | --- |
  | worn steel | oxide | The starter blade is meant to look old, and a flat tint said "grey sword". It displaces as well as tints, because corrosion eats metal. |
  | forged steel | forge marks | Colour only: an unground flat over the ridge, hammer banding, grinding streaks, a sparse scale. It darkens toward **blue-grey** where the rust goes warm, which is what tells the two blades apart at a distance. |
  | wood | grain | Lines along the piece, wandering slowly across it, sharpened to narrow dark bands. The strongest case in the set: a bare cube carried in the hand is the flattest thing in the game. Colour only — grain is what a tree grew, not what took its surface away. Worn by the log, the campfire, the wooden shield, the bow and the sceptre; **not** by the axe or the leather patch, which borrow the `LOG` swatch for reasons that are not their material. |
  | worked hide | none | Three pieces share one `Armour` silhouette, and a warm dark brown already reads as hide. Grain would be detail neither the mesh nor the cell's plate-and-shoulders picture has anywhere else. |
  | bone, meat, arrow | none | One `Material` stub each. A texture nobody will look at. |
  | stone, earth, snow, ore | none | Block-like items take the terrain swatch they represent, whole. **Terrain is not in this**: `world/palette.rs` plus vertex colours is that material system, greedy meshing merges quads across blocks, and a texture there is a different problem with different costs. An item that represents a block may take a livery in the hand and in the cell; the world does not change. |

  **The image holds one band per livery**, `FIELD_ROWS` tall, under the neutral row. One image is
  the point: the count of images is the count of materials the renderer has to bind, and a second
  would need its reasoning written down. A mesh points its vertices at its own material's band, a
  cell hands `bevy_ui` that band as a `rect`, and `livery::band_holds` is what lets a test say a
  blade never reads the rows another metal was written into.

  **The drop's mesh cache is keyed on whether the *shape's* mesh is built against a livery, and
  the wrong answer to that was caught in review.** `MeshKey` was `(ItemShape, Option<Livery>)`,
  which mints a byte-identical duplicate whenever a livery changes only colour — and giving the
  campfire a wood livery split the bundle roll the forge and the tent share: three structures, one
  silhouette, suddenly two meshes for no geometric reason.

  The first fix was to drop a livery whose `pit_depth` is zero, on the reasoning that a livery
  which displaces nothing leaves the mesh alone. **That is false.** `blade_loft` writes the
  livery's own *band* into the texture coordinates whether it displaces or not, so a blade wearing
  forged steel and a blade wearing none have identical positions and different coordinates.
  Collapsing them would have dropped the forge marks off every dropped iron sword — silently, with
  no error and no red test, a blade that merely looks a little plain.

  What is true is narrower and belongs to the shape: **only the blade's mesh is built against a
  livery at all**. `drops::mesh_varies_with_livery` is wildcard-free, so a shape whose geometry
  starts reading a livery has to say so, and `mesh_key` keeps the livery for exactly those. That
  still fixes the campfire, because `create_visuals` builds a bundle from `rolled_bundle_parts`
  and never looks at the livery — and it strengthens what #418 asked for rather than weakening it:
  two items sharing a shape and a livery still share one mesh, and now so do two whose shape
  ignores the livery entirely.

  **The rule is checked against the builder, not against itself.**
  `the_mesh_cache_separates_exactly_the_meshes_that_differ` builds both meshes for every shape and
  every livery and requires the key to separate them exactly when they differ; a rule that is right
  by accident and a rule that is right fail it differently. `a_dropped_blade_reads_its_own_bands`
  pins the consequence a player would have seen.

  **Subdivision follows displacement, not the presence of a livery.** `livery::pit_depth` answers
  zero for forged steel, so its blade is the two-span six-face loft an un-liveried one is — which
  is what keeps the iron sword `sword_mesh` in both states, to the vertex, while it wears a
  surface. Forge marks are the record of work done to metal that is still whole; corrosion is not.

  **The cell is the surface that could never have joined a geometric answer.** It has no vertices
  to tint — a picture there is `bevy_ui` rectangles — but `ImageNode` carries a handle, a `color`
  that multiplies it and a `rect` selecting a region, which is the three things a livery needs.
  One `IconPart` flag says which rectangles sample it, so a blade's edge wears the rust while its
  guard and grip do not, and the drawing stays keyed on `ItemShape` exactly as it was. The `rect`
  is `livery::field_rect`, which takes the neutral row off: a mesh points its vertices past that
  row and never sees it, while an `ImageNode` with no rectangle would draw the whole image and
  put a white line across every blade.
- **The list is a list and not a browser.** No favourites, no sorting, no player counts and no
  ping column; "online" is the account service saying it heard from that server recently, not a
  probe, and the screen says as much rather than implying reachability it did not measure. The
  list is read when the sign-in completes and when the retry is pressed — there is no automatic
  refresh, so a server that comes up while the panel is open appears on the next press.
- **Interpolation holds the last position for ever when a server goes quiet.** There is no timeout
  that fades an entity out, and none that says "this session is stale": a quiet server is a
  legitimate state, and the read timeout in `session.rs` is a poll interval rather than a session
  timeout. Deadlines belong to the same issue on both sides.
