# server/ — Authoritative Go Backend

The simulation lives here. Read the root `AGENTS.md` first; it is authoritative for the pipeline
and for the rule that shapes every decision below.

## The server decides; the client renders

Movement validation, combat outcomes, loot rolls, placement legality, durability, respawn — all of
it is computed here. A gameplay rule that exists only in the client is a bug even when it appears
to work, because it is a cheat vector by construction.

The contract makes that structural rather than aspirational: there is no message in which a client
states its own position. `PlayerInput` carries intent, the server simulates it, and
`EntitySnapshot` carries the answer. That means there is no client claim to validate, and so no
validation to get wrong.

## Package layout

| Package | Owns | Must not |
| ------- | ---- | -------- |
| `cmd/voxelheimd` | flags, logger, listener wiring, signal handling, shutdown | contain game logic |
| `internal/transport` | framing, TCP, TLS, the `Transport`/`Conn` interfaces | know what a frame means |
| `internal/protocol` | FlatBuffers encode/decode, contract limits | know about connections |
| `internal/session` | one connection's lifetime, handshake admission, entity ids, the one-session-per-identity claim | decide gameplay outcomes |
| `internal/game` | the fixed-rate loop, every player, movement, collision, inventory, snapshots | read or write a socket |
| `internal/world` | chunks, terrain generation, the RLE codec, the chunk cache, the world directory | know that sessions exist |
| `internal/identity` | what a player token is, what a player id is, and the one-way hash between them | import anything of ours |
| `internal/certs` | the server's own TLS certificate: generated once, kept under the world directory | implement any cryptography |
| `internal/persist` | the player store under `<world-dir>/players/`, the camp in `<world-dir>/structures.bin`, and the world's time of day in `<world-dir>/clock.bin` | be imported by `game` |
| `gen/` | flatc output | be hand-edited, ever |

The dependency direction is one-way: `game` and `session` depend on `protocol`, `transport` and
`world`, never the reverse. `transport` imports nothing from this module at all — which is what lets
it be replaced. `world` imports nothing of ours either: terrain does not need to know who is
watching it.

`session` also depends on `game`, and that direction is fixed too: a session hands the simulation
intent and is handed frames back through a callback it supplied, so `game` never imports `session`
and never learns that a session exists beyond "there is somewhere to put a snapshot".

`identity` is a leaf: it imports nothing from this module at all, which is what lets `game`,
`session` and `persist` all name a player without any of them importing each other. `persist` sits
between `session` and `world` — it imports both `identity` and `world`, reusing the delta store's
record framing rather than copying it — and **neither of `game` and `persist` imports the other**.
That is why a stored life is declared twice: `game.Life` and the life fields of `persist.Record`
carry the same four values, and `session` is the one place that maps between them. The duplication
is four field names; what it buys is that the store never decides what a life may say and the
simulation never decides how one is written down. Both use `protocol.InventoryStack` for the slots,
so the 36-slot shape has exactly one declaration.

`session` and `game` both name a few enums from `gen/` — `Payload`, `EditAction` — and that is
deliberate rather than a leak. `protocol` owns *reading and writing* FlatBuffers; the wire's
enumerations are the vocabulary of the contract, and declaring a parallel copy of one so that a
package can avoid the import would create two truths to keep in step for no benefit.

## Conventions that are not obvious from the code

- **`protocol.Decode` is the only place untrusted bytes are read**, and it copies every field it
  needs into plain Go values before returning. That is why it can recover from a panic and report
  an error: the Go FlatBuffers runtime has no buffer verifier, so a malformed offset surfaces as an
  out-of-range index. Returning a live accessor would move that panic somewhere unguarded — so
  don't. Add a field to the copied struct instead.
- **Check sizes before allocating.** `transport.MaxFrameSize` is enforced on the length prefix,
  before the payload is read. The ordering is the security property.
- **Identities come from `session.Registry`, never from the wire.** An id a client can choose is an
  id a client can claim from someone else.
- **A player token is a credential; a player id is a hash. Never confuse which one you are
  holding.** The token is 32 bytes of `crypto/rand` the server mints and one client keeps, and
  whatever holds it *is* that player — so it is never logged, never displayed, never written to
  disk, and never used as a key. The player id is its SHA-256: it names the same identity, it is
  what the store keys on and what a log line carries, and it gives nothing away. `identity.Token`
  carries both a `String` and a `LogValue` that redact it, and the second is not redundant — slog's
  JSON handler would otherwise marshal the array as 32 numbers, which a Stringer never sees.
  `TestTheTokenNeverReachesTheLog` captures a whole handshake through both handlers and looks for
  the token in hex, base64 and raw. **A token comparison, if one is ever needed, is
  `identity.Token.Equal` and therefore `crypto/subtle`** — nothing on the resolution path needs one,
  because an identity is found by hashing what was presented and looking *that* up.
- **The identity resolves on the session goroutine, between the decode and the handshake, and never
  under `sim.mu`.** Resolution reads the player store, and a tick that waits on a file is a tick
  every connected player misses. `session.Handshake` stays a pure function of its inputs — it is
  handed the resolved token and is table-tested on the rules that remain — and
  `session.Identities.Resolve` is where the store lookup, the mint and the exclusivity claim live,
  tested separately. The four-way rule, in order: a token of any length but 0 or 32 is
  `BAD_REQUEST`; an empty one mints; a 32-byte one whose record the store holds *and can read*
  resumes; a 32-byte one it does not know mints a **new** identity with a **new** token, because the
  server never adopts a client-chosen value as a key. A record it holds and cannot read is the same
  answer as one it does not have, once the file has been set aside — see the corrupt-record rule
  under "Known gaps".

  **What `Handshake` is handed is the whole resolved identity, not just the token**, because the
  welcome's `spawn` is the position the player is actually placed at and only the resolved record
  knows it. That is the reason the record loads during resolution: `Handshake` is pure and cannot go
  and find one.
- **The identity claim is released last in `Serve`'s teardown**, after `sim.Leave` and after the
  record write: `sim.Leave` → record write → release. Either other order is a reconnect served
  wrongly — refused for a session that has already gone, or handed a record that is still being
  written. It runs on every path out of `Serve`, an expired read deadline included, which is what
  makes an idle session hand its identity back instead of holding it until a restart.
- **One reader, one writer per connection.** `transport.Conn` promises to survive that and nothing
  more. The writer goroutine keeps draining its queue even after a write fails, because a producer
  blocked on a dead writer is a deadlock.
- **A write failure is classified, not promoted.** `Serve`'s deferred block asks
  `transport.IsDisconnect` about the writer's failure exactly as the read loop asks about its own:
  the peer going away is one event whichever goroutine noticed it first, so it ends the session
  with `nil`, and only an unrecognised error becomes the returned one. Promoted unconditionally, it
  meant a player quitting while a spawn chunk was in flight ended with `session: write: …` and a
  WARN — a warning fired by the most routine thing a player does, and a warning like that stops
  being read. The corollary belongs to the tests: `fakeConn` answers a closed connection with
  `net.ErrClosed`, because `IsDisconnect` is a claim about what real transports return. Teaching it
  `io.ErrClosedPipe`, which no socket produces, would have widened a production predicate to cover
  `io.Pipe` for the sole benefit of a double — and would have left the note under `IsClosed` about
  the two questions untrue.
- **A silent connection is closed, on a read deadline the server arms.** Two flags, both
  validated at startup by `session.Timeouts.Validate` — which is the one place the rule lives,
  asked by `options.validate` rather than restated there. `-handshake-timeout` (default `5s`)
  bounds the first read: a connection that has said nothing has not yet claimed to be a client,
  so it is closed **without a reply** — `ServerReject` answers a message, and there is none.
  `-idle-timeout` (default `20s`) bounds every read after the handshake and is re-armed before
  each one, which is the same thing as after every frame and is one call site rather than two.
  Seconds are safe because **`PlayerInput` is the heartbeat**: the client sends one every tick,
  standing still and dead included, so a healthy client is never silent for longer than one tick
  interval and 20s is hundreds of missed frames. Which is also why there is no ping message —
  adding one would put a second heartbeat on a wire that already has one. A handshake window
  longer than the idle window is refused: it would hold only clients that had already proved they
  were talking to the stricter number.
- **An expired deadline ends the session with `nil`, and `transport.IsDisconnect` says so.** The
  bullet above this one, arrived at from the other direction: a session that goes quiet has ended,
  and returning an error would make `acceptLoop` log "session ended with an error" for the most
  ordinary way a dead connection is noticed. `IsDisconnect` answers `true` for
  `os.ErrDeadlineExceeded` because nothing but this process arms a deadline, so its expiry is this
  side deciding the connection is over. `transport.IsTimeout` is the narrower half of the same
  question — not a third question — and only `Serve` needs it, to choose between logging
  `session idle` and `handshake timed out`. Both at Info; neither is a fault. Teardown is the
  ordinary one, so an idle player leaves the simulation and releases their identity.
- **The tick loop uses a fixed timestep**: the next deadline advances by exactly one interval,
  never from `Now()`. Deriving it from the clock would turn every overrun into permanent drift.
  After a long stall the loop abandons the missed ticks rather than running them back to back.
- **Time comes from `game.Clock`.** Tests inject a fake clock and simulate minutes instantly; code
  that calls `time.Now()` directly is untestable and does not belong in the simulation.
- **The shutdown order in `server.shutdown` is load-bearing.** Close the listener, wait for the
  accept loop, *then* close the registered connections, then wait for the sessions. Swapping the
  middle two lets a connection accepted during shutdown escape the registry snapshot: its session
  blocks in `ReadFrame`, nothing closes it, and the final wait never returns.
  `TestShutdownClosesAConnectionAcceptedDuringShutdown` fails by timing out if the order changes.
- **The accept loop ends only on shutdown.** A transient `Accept` error — file-descriptor
  exhaustion, a peer that vanished between SYN and accept — is logged and retried with backoff. A
  server that has stopped accepting while its tick loop keeps running looks healthy from the
  outside, which is the worst way to fail.
- **Terrain generation is integer-only, in Q16.16 fixed point.** Not a stylistic choice: Go's spec
  permits an implementation to fuse floating-point operations, so the same float expression may
  round differently after a compiler upgrade or on another architecture. The GDD's weekly storm has
  to regenerate a chunk to the bytes it had months ago, and `testdata/chunk_golden.bin` is the test
  that says so. Do not introduce a float into `internal/world`'s generation path.
- **The tick loop uses `Cache.Peek`, never `Cache.Get`.** Get generates on a miss; a tick that waits
  on terrain is a tick every connected player misses. Peek answers "is it here yet" and nothing else.
- **A chunk is recorded as delivered only after the send succeeds** (`View.MarkLoaded`). Marking it
  when it is scheduled is simpler and leaves a client permanently missing terrain the server
  believes it has, because a failed send would still count.
- **The chunk stream lags the snapshot stream, and a test may not assume otherwise.** A tick
  publishes the player's chunk to a newest-wins doorbell (`game.chunkFeed`) and the session's
  streaming goroutine does the reading, encoding and sending — deliberately, because generating
  terrain on the tick goroutine would cost every connected player a tick. So a snapshot showing a
  player over a border is **not** evidence that the chunk beyond it has been sent, and asserting
  both in one breath is a race rather than a test: that is the whole of issue #55, worth 1 failure
  in 40 there and 5 in 1,200 when it was re-measured under `-race` at GOMAXPROCS 1 and 2 on a
  loaded machine. Wait for the chunk — and park the player first. Newest-wins means a
  walk that runs on while the assertion waits can carry the player out the far side of the chunk,
  and the chunk the test asked about is then never sent at all, correctly.
- **`select` with two ready cases picks at random, so check cancellation before the select.** Every
  place that races a context against something else — the cache semaphore, the session's outbound
  queue, the clock's sleep — checks `ctx.Err()` first. Without it, an already-cancelled context is
  honoured about half the time, which surfaces later as a flaky test rather than as the bug it is.
- **Anything positional is derived from the height field, never stated as a constant.** The spawn
  point was `y = 80` while the terrain ranges 44..84: it buried the player for about one seed in 500
  and floated them 26 blocks above the ground for the default one. `world.SpawnAt` asks `HeightAt`
  instead. The general form of the mistake is pinning a number that a procedural system decides.
- **Validate flags before narrowing them.** `-tick-rate 1000` must fail at startup, not become a
  silent 255 Hz server, and the error must quote what the operator typed. Clamp-then-validate reads
  as safe and is not.
- **`log/slog` only.** No `fmt.Println`, no `print`. Session logs carry `entity_id` and
  `remote_addr`; a message worth reading twice deserves a field, not string formatting.

## The session is encrypted, and that is not a setting

- **There is no plaintext listener and no flag to ask for one.** An identity token is a bearer
  credential: whatever can read one off the wire can come back as that player. A switch that
  turned the encryption off would make that exposure a configuration mistake somebody makes once
  and never notices, because a plaintext session looks correct from both ends. The only setting
  nobody can get wrong is the one that does not exist.
  `TestTheServerSpeaksNoPlaintext` pins it as a property of the binary rather than as a sentence
  here: a peer speaking the framing straight at the bound port gets nothing.
- **`transport.ListenTLS` is the second implementation the package doc anticipated, and it cost
  the framing a rename and nothing else.** A `*tls.Conn` is a `net.Conn`, so it goes through the
  same `framedConn` an unencrypted socket did — which is what the two interfaces in
  `transport.go` were declared for. `ListenTCP` stays as a tested library type; the server does
  not call it.
- **`MinVersion` is stated rather than inherited**, and it is 1.3. crypto/tls's default floor has
  moved between releases and will again, and a server whose security properties are decided by a
  changelog nobody read is not a decision. Both ends of this wire are ours, so there is no
  browser and no legacy client to accommodate: 1.2 buys nothing and costs a downgrade path.
  Nothing else is configured — cipher suites and the key exchange are crypto/tls's to choose, and
  under 1.3 they are not negotiable anyway.
- **The handshake happens on the first read, not in `Accept`.** That is `tls.NewListener`'s
  behaviour and it is the one to want: handshaking in `Accept` would put an unauthenticated
  peer's stalling in the accept loop, where one slow client delays every other connection. On the
  first read it lands on the session goroutine, inside the handshake deadline `Serve` already
  arms — so a peer that opens a socket and says nothing is closed by the same timeout that
  already covered a peer who sent no `ClientHello`.
- **A read deadline still fires through the TLS layer**, and it is pinned rather than assumed. A
  TLS record boundary is not a frame boundary and the deadline is measured on the socket
  underneath; without the test, #150's handshake and idle timeouts could have stopped bounding
  anything while every log line looked healthy. crypto/tls's answer to an expired deadline —
  the connection is finished — is exactly what `Conn.SetReadDeadline` already required.
- **The certificate is generated once and kept**, under `-world-dir` as `server-cert.pem` and
  `server-key.pem`, the key at `0600`. Keeping it is what makes the client's pin mean something:
  a server that regenerated would hand every returning client a fingerprint it did not pin, and a
  client doing its job would refuse to reconnect. Half a pair is refused rather than repaired,
  and an unreadable one is an error rather than a fresh start — regenerating over either would
  silently change the fingerprint every client pinned.
- **An ephemeral world keeps no key**, so it presents a new certificate every start and every
  returning client is refused by its own pin. That is the honest consequence of `-world-dir ""`
  rather than a defect; a startup warning says so.
- **The fingerprint is logged at Info on every start.** It is the number a player compares
  against a refusal, and an operator who cannot produce it on demand cannot answer the one
  question a refused client asks. It gives nothing away — it is a hash of a certificate the
  server hands to everyone who connects. The private key is never logged, and a test looks for
  it in the captured output.
- **Nothing here implements cryptography.** Key generation, signing and the certificate encoding
  are `crypto/ecdsa`, `crypto/x509` and `crypto/tls`, which is also why `server/go.mod` still has
  one dependency.
- **`scripts/interop-check.sh` is the only thing that sees both halves at once**, and it is not
  in CI — the client opens a window, and the Go and Rust gates run in separate jobs with
  separate toolchains. Run it by hand after touching this package or `internal/certs`. The first
  time it ran it found a client bug that every unit test on both sides had passed over; the
  story is in `client/AGENTS.md` under "Known gaps", and the moral is that a transport tested
  only against its own language's client is a transport tested against half of itself.

## Movement, and the invariants it added

- **`Sim.Step` holds one lock for a whole tick, and that is load-bearing.** It is what makes
  `Sim.Leave` a *guarantee*: Leave takes the same lock, so once it returns no tick can be
  part-way through delivering a frame to that session and no later tick will start one. Session
  teardown is built on that — `sim.Leave(player)` runs **before** `close(out)`, because a send on
  a closed channel is a panic in a goroutine and takes the process with it.
  `TestSnapshotsStopBeforeTheOutboundQueueIsClosed` crashes the test binary if the two are
  swapped, and it can only see the swap because `trySend` deliberately has *no* "is this session
  ending?" check to mask it.
- **Nothing under that lock may block.** Terrain is read with `Peek`, the chunk feed is a
  non-blocking doorbell, and a snapshot for a session whose queue is full is *dropped*. Dropping is
  right rather than merely convenient: a snapshot describes one tick and is worthless by the time a
  full queue drains, so waiting for room would stall every other player's tick to deliver something
  already stale. A chunk keeps the blocking path, because a chunk is not replaced by a later one.
- **A chunk that is not resident is solid.** The tick loop may not generate terrain, so an absent
  chunk has only two possible answers and "air" drops the player out of a world that is merely
  still loading. A player standing over one freezes with zero velocity — so when the chunk arrives
  they do not fall in with three seconds of accumulated speed.
- **Non-finite input is refused before any physics runs, and refused rather than clamped.** NaN
  compares false against every bound, so `if v > 1 { v = 1 }` passes it straight into the
  integrator and the position stays NaN for the rest of the session. `schemas/player.fbs` states
  it as a decoder invariant; it is enforced in `game.Player.Submit` rather than in
  `protocol.Decode`, because `protocol` owns the envelope and what a NaN axis *means* is a
  decision. Refused input is dropped and logged at debug, never fatal: the frame was well formed
  and the stream is still trustworthy.
- **The speed clamp is on the movement vector's magnitude, not on each axis.** Clamping components
  to ±1 still admits `(1, 1)`, a vector of length √2 — a diagonal 41% faster than any straight
  line, reachable by an honest client by accident. Computed with `math.Hypot` in float64, because a
  forged 1e38 squares to +Inf in float32 and the scale factor would then be zero, freezing the
  player instead of clamping them.
- **The physics timestep is derived from the tick rate**, so `-tick-rate 40` is a server that
  simulates the same world at twice the resolution rather than twice the speed. Pinned by a test
  that walks a player for one simulated second at two rates.
- **Collision resolves one axis at a time, in sub-steps shorter than a block.** Both halves are
  what make it correct without a physics engine: resolving three axes together needs a swept test
  to find the first contact, and a step longer than a block can pass clean through a wall between
  two overlap tests. At terminal velocity a tick is three blocks, so the sub-stepping is not
  theoretical — the tunnelling test sweeps twenty sub-block starting heights, because a single drop
  either happens to sample inside the floor or happens not to.
- **Intent persists across ticks, but not for ever.** `PlayerInput` describes the state of the
  controls, not an event, so one late frame must not stop a player mid-stride. After half a second
  of silence "still held" stops being a fair reading and the movement axes are zeroed — otherwise a
  client that closed its send loop would walk to the horizon. The yaw is kept: facing is not a
  control that decays.
- **There is no position to validate, and that is the point.** Nothing in `PlayerInput` says where
  a client is, so there is no claim to check and no rejection path to get wrong. If you find
  yourself writing code that decides whether a client-reported position is plausible, the contract
  has changed underneath you — go and read `schemas/player.fbs` first.

## Editing the world, and the rules that make it safe

- **A chunk is generated base plus deltas, and the generator never learns about the deltas.**
  `world.Generate` stays a pure function of (seed, coord); every edit is recorded in
  `world.Deltas` and composed on top when a chunk is built. The layering is not tidiness: the
  GDD's Fimbulvetr storm has to restore an unprotected chunk to its *original* procedural
  state, which is discarding a map while the edits live in one, and impossible the moment
  anything bakes one into the terrain function. The delta layer also outlives every cache
  entry on purpose — a chunk evicted and regenerated has to come back with its edits.
- **A composed chunk is immutable once it is published; an edit swaps the pointer.** That is
  the locking rule, and it is what lets `CacheTerrain.Solid` read a voxel on the tick
  goroutine with no lock and no atomic while edits arrive on session goroutines. `Cache.Apply`
  clones, patches, re-encodes and publishes; 64 KiB per edit buys a plain slice index on every
  one of the thousands of terrain reads a tick performs.
- **A consumer that remembers a chunk pointer must watch `Cache.Revision`.** A published
  chunk stays readable for ever and stops being the world the moment somebody digs. The
  collision memo compares the revision on every lookup, because it otherwise refreshed only
  when the *coordinate* changed — and a player standing in the chunk they are digging never
  changes coordinate. The revision is bumped *after* the patched chunk is published, so the
  new number can never be read beside the old chunk.
- **`Cache.composeMu` is never held across `Generate`, and no edit holds the simulation's
  lock across a chunk.** Generation is the millisecond-scale part and runs outside every lock;
  composition — apply the deltas, encode, publish — runs under `composeMu`, which is what
  makes both possible orders of "an edit is recorded" and "a chunk is published" correct.
  `Player.Edit` takes `sim.mu` only to read the authoritative position and to test the player
  boxes, never across `Apply`, because `Apply` can wait on a chunk being generated. Mining
  completion crosses the same seam: `Step` hands an opaque completion to the session worker,
  which is the only place allowed to call the blocking editor.
- **Legality travels *into* the write.** `Cache.Apply` takes an `allow` predicate evaluated
  against the block that is there, inside the critical section that replaces it. Reading the
  voxel first and writing afterwards would let two players edit the same voxel, both be told
  they succeeded, and broadcast two answers while the delta layer keeps one. `allow` must be
  pure and must not block: every chunk being composed anywhere in the server waits behind
  that lock.
- **Reach is measured from the centre of the player's collision box** (`game.EditReach`) and
  is checked **before any world read**. `pos` is unbounded on the wire, so that ordering is
  what stops a request naming a voxel on the far side of the world from making the server
  generate the chunk around it. From the body rather than the eyes, because eye height is the
  client's to choose — `EYE_HEIGHT` in `client/src/player/constants.rs` says so — and a reach
  measured from a camera would be one the server could only evaluate by mirroring a constant
  it does not own.
- **An ordinary refused placement or mining intent is silence plus a debug line**, exactly as a
  refused `PlayerInput` is. There is no rejection message in the contract and no acknowledgement
  of any kind: the client learns it did not apply by not seeing it apply. The retired
  `EditAction.Break` is deliberately different: carrying that value in `BlockEditRequest` is a
  protocol error and ends the connection, because `MineRequest` is now the only breaking intent.
- **Mining messages refresh intent; only `Step` pays hardness.** Progress is guarded beside the
  player's other simulation state, advances at most once per authoritative tick, and expires on
  the same half-second idle boundary as movement input. Client cancellation and target changes
  clear it silently. A block change or movement beyond `EditReach` sends the miner one zero frame;
  ordinary positive progress also goes only to that session. A non-resident target holds paid
  progress without generating, and completion sends no progress frame — its `BlockUpdate` is the
  answer. Active targets are reverse-indexed by voxel, so an edit invalidates only miners of that
  voxel rather than scanning every session under `Sim.mu`. A server reset is durable state: if the
  non-blocking outbound queue is full it stays pending and is retried before any later progress,
  then cleared only after exactly one zero enters the queue.
- **Inventory edits are serialised per player, separately from the simulation.** Each player
  has a mutex separate from `Sim.mu`; `Cache.ApplyGuarded` generates the target chunk first,
  then lets `Player.Edit` acquire that mutex across the stack recheck, world write and count
  change. That serialises a placement with inventory moves without holding either the
  simulation or inventory lock across chunk generation. A place with an empty or non-placeable
  item is refused through the ordinary edit path and produces no `BlockUpdate`. **A break holds
  no inventory lock at all** since its yield became a drop — see the drops section below, where
  that and the tick's `TryLock` are one decision.
- **Items and blocks are different id spaces.** The server-only registry in `game/items.go`
  owns each item's placed block (or none) and its per-item stack limit, currently 64. The
  drop table independently decides what each mined block yields, and what it names is spawned
  as an entity rather than inserted: a completion against 36 full slots removes the voxel and
  leaves the yield lying where the block was.
- **Inventory state is sent whole, once on join and after each real count change.** The
  session never sends a delta and never drops one on a full outbound queue: unlike a tick
  snapshot, no later frame is guaranteed to supersede it. A pickup is decided on the tick and
  therefore uses the tick's non-blocking seam, which is why it keeps a durable flag and retries
  until one is accepted rather than dropping the frame. Protocol V2 always sends 36 real,
  stable slot-indexed pairs; `(0, 0)` is empty and the first nine are the hotbar. Insertions
  fill partial same-item stacks before the lowest empty slot; moves split, merge or swap
  under the same per-player lock. `BlockEditRequest.slot` spends exactly the slot the client
  named for a placement after the server revalidates it.
- **"Every session holding this chunk" lives in `session.Registry`, not in the tick loop.**
  It is a different question from the snapshot fan-out: the tick knows where each player is
  and can derive the cube it is streaming, but only the streamer knows which chunks have
  actually reached the client, and `View.loaded` is the only record of that. `View` is
  therefore mutex-guarded — an edit resolved on one session's goroutine asks about another
  session's view while its streamer is diffing it.
- **A lost chunk is repaired by forgetting it and asking for a diff, and there is exactly one
  way to do that.** `Streamer.repair` is `View.Forget` plus a wake, and its two callers are a
  client's `ChunkResendRequest` and `sendChunk` giving up on a re-send. Forgetting was always
  enough *arithmetically* — the next diff sends whatever the view is missing, and `View.MoveTo`
  deliberately has no shortcut for an unchanged centre precisely so that works. What was
  missing is that diffs happen on **chunk crossings**: `followPlayer` blocks in
  `Player.NextChunk`, which returns when the simulation puts the player in a chunk they were
  not in and at no other time, so for a player standing still "the next diff" was not a time at
  all. `Player.WakeStreaming` rings the doorbell the tick loop rings, which is what makes the
  repair immediate rather than eventual. Do not give `MoveTo` that early return, and do not add
  a second wake channel: the doorbell collapses, and a repair records what it has to say in the
  view *before* ringing, so one diff serves any number of them.
- **The client may ask for a chunk, and that is not the client deciding anything.** It asks for
  data it has already lost, never for an outcome. `Streamer.Resend` refuses a coordinate outside
  the session's view volume, one this session was never sent, and one that arrives faster than
  the bucket allows — silently, like every other refusal, because the contract has no rejection
  message and an honoured request is answered by the `ChunkData` that follows it.
- **It is also the only rate limit in this repository, and both its numbers are derived.** The
  burst is one view volume, `(2r+1)³`: the most a client can ever honestly need, and work the
  server already agreed to do for it once at join. The refill is
  `TerminalFallSpeed / ChunkSize` chunks a second — the fastest a player can cross chunk
  boundaries, and therefore the fastest the world can legitimately move under a session. It
  bounds *chunk work*, not messages: a request refused before the bucket is consulted costs a
  mutex and a map lookup, and bounding that is the socket-level backpressure policy the gaps
  below still ask for.
- **`Registry.Unsubscribe` is the broadcast's `Sim.Leave`.** It takes the lock
  `BroadcastChunk` holds *while it sends*, so once it returns nothing can still be sending to
  that session — and `Serve` calls it **before** `close(out)`, because a send on a closed
  channel is a panic in a goroutine and takes the process with it. Both halves are
  load-bearing: the send must stay inside the registry's lock, and the unsubscribe must stay
  ahead of the close. `TestBroadcastsRunSafelyWhileSessionsArriveAndLeave` fails on either.

## Item drops, and the entity shape the rest of the world will reuse

A drop is the first thing in this simulation that is not a player. It lives in `Sim.drops`,
is stepped by the tick, is streamed by the same visibility rule, and has a lifetime — which
is exactly the shape a mob needs, so it is built as "an entity the simulation owns" rather
than as a special case of the inventory.

- **A break no longer touches the inventory.** `breakMined` writes Air and spawns a drop
  carrying whatever the drop table names; the pack changes only when somebody walks over it.
  A block whose yield is `ItemNone` — Leaves explicitly, an unlisted block implicitly —
  spawns nothing and costs no identity. The consequence worth stating: a full pack no longer
  loses a yield, it leaves it on the ground.
- **The collision box is a parameter, not a constant.** `moveAndCollide` takes a `body`, and
  `playerBody` carries the numbers that used to be spelled inside `playerBox`. Dividing a
  float64 by two is exact, so the player's arithmetic is unchanged — and the existing
  movement tests are what say so. A drop is a `DropSize` cube and falls with the player's
  integrator, which is why it inherits sub-stepping, the skin, and the rule below for free.
- **Every entity's position is the bottom of its box**, in the simulation. A drop's *wire*
  position is the centre of its box, translated in exactly one place (`itemDrop.wirePos`),
  because the client draws a cube centred on what it is sent — `DROP_EDGE` in
  `client/src/player/drops.rs`. Spawning centres the box in the broken voxel, so the position
  a client receives is the centre of the block it just watched disappear.
- **A drop over a chunk that is not resident holds where it is**, with no accumulated fall
  speed, by the same rule that keeps a player from dropping out of a world that is merely
  still loading: an absent chunk is solid, `moveAndCollide` refuses a move that starts inside
  one, and a blocked axis zeroes the velocity.
- **Pickup is proximity and nothing else.** No key, no aim, no request: the client sends
  nothing and learns the outcome by the id leaving the `drops` vector. The radius is measured
  between the two boxes and is Euclidean for the reason `EditReach` is. A drop cannot be
  collected for `dropPickupDelayTicks` after it appears, which is what makes it something a
  player sees before they have it rather than an inventory insert wearing a delay.
- **The tick takes the inventory lock only if it is free.** `Player.collect` uses `TryLock`,
  so a pickup can happen under `Sim.mu` without ever waiting on a session goroutine that is
  holding that lock across a chunk composition. It makes the pair deadlock-free by
  construction rather than by lock ordering, and a contended tick simply leaves the drop on
  the ground for another fifty milliseconds. **This is why `breakMined` was able to stop
  taking the inventory lock at all** — the two changes are one decision.
- **The inventory state a pickup produces is durable, like a mining reset.** It can only use
  the tick's non-blocking seam, and unlike a snapshot no later frame is guaranteed to
  supersede it, so `inventoryDirty` survives a full queue and the retry re-reads the live
  slots instead of resending a stale encoding.
- **Merging keeps the older drop and the older drop's age.** The tick's list is ordered by
  identity and identities only increase, so the survivor is always the earlier one — which is
  what stops a mining spree from renewing a pile's lifetime for ever. Merging runs before
  collecting, so a pile arrives as one insertion.
- **Identities come from the counter that names players** (`session.Registry.NextID`, injected
  into `NewSim`). One counter is what makes "an id names one thing" a fact rather than a
  coincidence; a nil source is refused rather than replaced with a local counter.
- **Pickup is O(players × drops) and merging is O(drops²)**, knowingly, and the same
  judgement `Sim.Step` already records for snapshot visibility: a spatial index is worth
  building when the quadratic term matters and not one issue before.
- **Drops are not persisted, deliberately.** A restart loses whatever is lying on the ground.
  `world.Deltas` records changes to the *world*, and a drop is a moment in a simulation
  rather than a change to the world — persisting it would also mean deciding what a drop's
  five-minute lifetime means across a server that was off for a day.

## Structures, and the entity that does not move

A tent, a forge and a campfire are the third entity kind, and the first one the tick does
not step.
They live in `Sim.structures`, are streamed by the same visibility rule as drops and mobs,
and their identities come from the same counter — but nothing about a structure changes
with time, so `Step` reads them and never advances them.

- **A structure is an entity, not a voxel**, and that is what keeps it out of `world`.
  Chunk data, the RLE palette and the delta layer are untouched by a camp going up: a tent
  is not nine blocks the world has to remember, it is one thing the simulation owns. The
  Fimbulvetr storm's "discard the deltas" therefore stays a decision about terrain that a
  shelter cannot complicate.
- **The anchor is a *ground* cell, and the footprint names what has to be solid.** A tent
  rests on 3×3 cells at one height with two cells of air above each; a forge on the anchor
  (the anvil) plus the cell one step along its facing (the hearth), with one cell of air
  above each. Both halves are one question — a cell that must be solid and a column that
  must be clear — and `footprintOf` answers them together so a later kind cannot answer one
  and forget the other.
- **Facing is a request, validated like the anchor is.** The client quantizes its camera
  yaw to four compass members and the server checks the result. The compass is the movement
  integrator's: North is -Z, East is +X. `rotateOffset` is integer arithmetic, because a
  rotation in float would put a footprint cell on the wrong side of a boundary it is sitting
  exactly on. The tent's nine cells are symmetric, so rotating them is a no-op — and it is
  still computed rather than special-cased.
- **Validating the ground and inserting the structure are one critical section.** The
  collapse rule can only see structures the registry holds, so doing those in two sections
  would leave a window in which a break passes between them and is never noticed. Nothing in
  that section blocks: the terrain read is non-generating, and the inventory is taken with
  `TryLock` — the same discipline the tick uses, which is what keeps the pair deadlock-free
  by construction rather than by lock ordering. **Do not take the inventory lock with `Lock`
  under `sim.mu`, and do not take `sim.mu` under the inventory lock**; either direction turns
  the tick's `TryLock`s into the only thing standing between this and a deadlock.
- **A footprint at the edge of loaded terrain is refused, not waited for.** The read goes
  through `Terrain`, which never generates, so a cell in a chunk the server has not composed
  answers neither solid nor clear. The alternative is a session goroutine holding `sim.mu`
  across a chunk generation, which is a tick every connected player misses. A player within
  `EditReach` of the anchor has had that chunk for a long time, so this is a boundary case.
- **One tent to a player; forges and campfires are unlimited.** A tent is where its owner
  comes back to, and two answers to that is a choice nobody made. What throttles a forge is
  eight stone and two coal, and a fire four logs and a coal — a cost rather than a rule. **A
  camp may have several fires on purpose**: the safe ground is a property of each fire rather
  than a per-owner allowance, and extending the tent's rule to cover them would refuse a
  second fire nobody had a reason to refuse.
- **A campfire is one cell, one cell of headroom, and one radius.** It rests on the anchor
  alone, which makes facing a no-op the way the tent's symmetric nine do — and it is still
  rotated rather than special-cased. Everything else about it comes from the code that
  already stands a tent up: `knownStructureKind` and `structureItem` gained one arm each,
  `footprintOf` one case, and ownership, removal, collapse and the dropped item are the
  paths the other two kinds already take. **What a fire does is keep spawns off the ground
  around it**, through `Sim.nearACampfireLocked` and `CampfireSafeRadius` — both declared by
  the spawn director and read here, never redeclared (see the spawn section). It has no
  fuel, does not burn down, cannot be put out, cooks nothing, lights nothing in the mesher
  and hurts nobody who stands in it; the entity and the radius are the whole feature.
- **Ownership decides removal and respawn, and nothing else.** Any player may walk into any
  tent, and the crafting issue reads this registry for a nearby forge without consulting the
  owner at all.
- **The owner is an `identity.PlayerID`, and the wire carries an entity id.** An entity id
  names one session; a camp outlives every session its owner will ever open, so keyed by the
  entity id a tent stopped being its owner's the moment they reconnected — they came back
  with a new number, respawned at the world spawn, and could not take down their own tent.
  The registry therefore keys ownership by identity and `structureStatesLocked` resolves it
  to the owner's **current** entity id once per snapshot, or to `0` while they have no live
  session (`schemas/player.fbs`, V5). Zero means *offline*, not *unowned*: no entity is ever
  numbered 0, so an offline owner matches nobody. **The identity itself never goes on the
  wire** — it is what a player record is keyed by, and sending one would hand every client a
  key to every camp. The resolution is a map hit per structure through `Sim.byIdentity`,
  maintained in `Join` and `Leave` beside `Sim.players`; the alternative is
  O(structures × players) inside the lock `Step` holds for a whole tick.
- **Removal and collapse put the item on the ground**, at the anchor, through the same
  `spawnDrop` a mined block uses. A full pack is a reason to leave something lying there,
  never a reason to destroy it.
- **Breaking a supporting ground block brings the structure down.** The hook is in
  `breakMined`, which is the one transition to Air, and the rule is exactly "a cell the
  footprint rests on stopped being solid" — no floating anvils, no tents over a pit. The
  registry change is one critical section and the drops that follow take the lock again,
  because `spawnDrop` takes it itself.
- **The respawn point is resolved from the live registry every time.** A cached position
  would still name a tent that collapsed or was picked up an hour ago, and the player would
  come back standing in the air where one used to be. With no tent, the join spawn — which
  is what `respawnLocked` always did.
- **A structure is placed in the chunk of the ground it rests on**, which is the chunk
  *below* the player standing on it whenever the anchor is the last block of one. It only
  matters at a view distance of zero or one; every real deployment streams further than that.
- **A camp is persisted; a drop and a mob still are not.** Kind, anchor, facing and owner go
  to `<world-dir>/structures.bin` through `persist.StructureStore`, on the delta store's
  discipline (magic, version, CRC, temp-and-rename, unknown versions refused) and rewritten
  whole rather than appended to. **No structure id on disk**: ids are re-minted through the
  injected `mintEntityID` on load, so "one id names one thing" survives a restart without the
  counter itself having to be serialised. Loaded once before the listener is served, so a
  returning player finds their camp in their *first* snapshot. The dirty-flag-and-flush shape
  is the chunk cache's — placement, removal and collapse set the flag, the autosave and the
  shutdown clear it through `Sim.TakeDirtyStructures`, and a failed write re-marks it — so a
  world nobody is building in costs no I/O at all. See the two gaps below.
- **A new kind does not move `persist.StructuresVersion`.** A record is fixed-width and the
  kind is one byte of it, so a campfire is a new *value* rather than a new shape and a file
  holding one is the file this format always described. An older build refuses it through
  `RestoreStructures` — a kind with no footprint is a camp it cannot place — which is a
  judgement about content, and `persist` deliberately makes none. Bump the version for the
  layout and for nothing else; `TestACampIsTheSizeTheFormatSaysItIs` reads the entry size and
  the version as one pair for that reason.

## Crafting, and how a transaction is made out of an array

Everything here lives in `internal/game/craft.go`, beside the registry it reads.

- **The recipe table is not sent to clients**, for the reason `itemRegistry` is not. A
  client mirrors a display-only copy so it can gray out a row nobody can afford; a drift
  between the two copies can show a wrong label but can never create an item. The wire
  carries a `RecipeID` and nothing else — no ingredient list, no product, no station — so
  there is no claim here for the server to disbelieve.
- **A craft is all-or-nothing because it runs on a copy.** `slotTable.craft` consumes every
  ingredient from a scratch table and inserts the product into the same scratch table, and
  replaces the real slots only once every step has succeeded. That is what makes "materials
  *and* room for the output verified before anything is consumed" true without a
  would-this-fit predicate sitting beside the insertion rule — **the check is the
  insertion**, run somewhere it can be thrown away. A second copy of that rule is a copy
  that can disagree, and the disagreement is an item that stops existing.
- **The order inside the copy is the reason it is worth its cost.** The ingredients come out
  first, so a pack whose every slot is full still crafts when the recipe empties one. A room
  check performed before the spend refuses that, and refusing it is a bug rather than a rule
  anybody chose.
- **`slotTable` exists so that copy is legal.** `inventory` carries a mutex and `go vet`'s
  copylocks check refuses to see one copied; an array of slots copies for free. The rules
  about how items enter and leave a pack therefore live on the array, and `inventory` is the
  lock around it. `slotTable.consume` deliberately does **not** unwind a partial spend — its
  only caller throws the whole table away — and the comment on it is where a second caller is
  supposed to notice.
- **Per-item melee damage lives in the registry, and the swing's numbers do not.** Reach, arc
  and cadence describe the *swing* and stayed in `constants.go`; damage and wear describe the
  *blade* and sit beside `itemRegistry` in `items.go`, which is the rule
  `RustySwordMaxDurability` established. `armedWithSwordLocked` returns what the slot is worth
  rather than comparing against an item id, so **a third weapon is a registry entry rather
  than an edit to combat**. A zero damage — every resource, every structure, the empty slot —
  is the same refusal the id comparison used to make.
- **A worn-through blade is `durability == 0` under a *non-zero maximum*.** Testing the
  current value alone would make a weapon that does not wear out permanently unusable the
  moment somebody registered one, because a wearless item carries `(0, 0)` like every
  resource does.
- **Forge proximity is a scan of the structure registry, never of voxels.** A handful of
  entries at craft frequency, on the same explicit trade the drops and the mobs record.
  Ownership is deliberately not consulted: a forge is a place, not a possession, and the owner
  field exists for removal and respawn.
- **One critical section**, for the reason placement has one: liveness, the station scan and
  the slot arithmetic are one decision, and splitting them leaves a window in which the player
  walks away from the forge between the check and the spend. Nothing in it blocks — the
  registry scan is arithmetic and the inventory is taken with `TryLock`.

## Repairing a blade, and why it needs nowhere to stand

Everything here lives in `internal/game/repair.go`. It is short on purpose: the difficult
parts of it were already decided by the registry and by the slot invariants above.

- **A repair has no station, and that absence is the feature.** GDD §4 makes mending a
  field action — a stone comes out of the pack wherever the player is standing, which is
  what turns a death or a long fight into a supply cost rather than an expiry date on the
  weapon. The forge is only where the stones are *made*, and that half is `recipeTable`'s.
  Adding a proximity test here would be a second answer to "where can this be done".
- **What counts as a repair kit is a registry field, not a list of item ids.** The lesson
  is the one `meleeDamage` recorded: a swing that named a stack of stone used to be
  refused by comparing against `ItemRustySword`, and is now refused by the stone having no
  damage to do. `repairRestore` is the same shape, fail-closed the same way — an item that
  says nothing about repair cannot be spent as a kit. The restore amount sits in `items.go`
  with the damages, because it describes the *stone*.

  **This paragraph used to end "so the leather patch this game does not have yet is an
  entry beside `itemRegistry` rather than an edit to the repair path", and that is exactly
  what the patch cost.** `ItemLeatherPatch` is one row with a `repairRestore` of 40 and one
  line in `recipeTable`; `repair.go` was not opened. A design note that predicts the shape
  of a change is worth more when the change arrives and matches it than when it is written,
  so the prediction is recorded here rather than quietly deleted. The second kit is also
  what turned `TestTheSharpeningStoneIsTheOnlyRepairKit` into
  `TestTheStoneAndThePatchAreTheOnlyRepairKits` — the sweep exists so that a *third* one is
  a decision somebody makes in that list rather than an accident.
- **The two kits differ in where they come from, not in a multiplier.** A stone restores 50
  and is made at a forge out of stone and coal; a patch restores 40 and is made anywhere out
  of two vargr pelts. GDD §4 asks for a pair of field kits, and a patch worth a quarter of a
  stone would not be a second answer — it would be a worse one nobody carries. What a player
  chooses between is a walk home and a hunt.
- **It is a flat amount, never a fraction of the target's maximum.** A fraction would cost
  the same number of stones to keep either blade alive, so the reward for forging the
  better one would silently include a cheaper upkeep, and the number would stop being
  readable from the item that carries it.
- **`restoredBy` widens to uint32 before it adds**, which is `wornByDeath`'s rule read from
  the other direction: durability is a uint16, and a slot near the ceiling of its type plus
  a restore is a sum that does not fit in one. Wrapping there hands a worn blade a tiny
  durability instead of a full one — an overflow that reads as a repair having *almost*
  worked, which is the worst shape for an authoritative number to fail in.
- **A blade at zero under a non-zero maximum is the target the feature exists for.** The
  eligibility test asks `durable()` — the maximum — never the current value alone, for the
  reason `armedWithSwordLocked` does: a wearless item carries `(0, 0)` like every resource,
  and reading that pair as "worn through" would make it repairable and a resource
  un-repairable, both wrong.
- **No scratch copy, unlike `slotTable.craft`, and the difference is worth naming.** A
  craft has to know whether its product would fit before it spends anything, and the only
  honest answer to that is the insertion actually running — so it runs somewhere it can be
  thrown away. A repair touches two known slots and adds nothing to the pack, so every
  condition is answerable before the first write. The one fallible step is therefore
  ordered first: the kit is consumed, and only then is the target's wear raised.
- **Out-of-range slots and `kit_slot == target_slot` are refused here, not by the decoder**,
  and `schemas/player.fbs` says so. The asymmetry with `InventoryMoveRequest` — which *is*
  bounded in `protocol.Decode` — is real: a move names slots that package indexes with, so
  it must bound them before anything reads an array, while a repair names slots the
  simulation looks up against the player's own pack. Refusing them at the framing layer
  would close a connection whose bytes are perfectly readable.
- **One critical section**, for the reason `Craft` has one: liveness and the slot
  arithmetic are one decision, and splitting them leaves a window in which a player killed
  between the two still spends a stone.

## The world's clock, and the one predicate that reads it

Everything about it lives in `internal/game/clock.go` and the three constants beside
`DefaultTickRate` in `loop.go`; the file it is written to is
`internal/persist/clock.go`, and `cmd/voxelheimd/main.go` is where the two meet.

- **The tick is the clock.** `Sim.tickOfDay` advances by exactly one at the top of
  `Sim.Step`, under the lock the whole tick is under, and nothing anywhere reads
  `time.Now()` to decide what time of day it is. A server that asked the wall clock
  would run its day at the speed of the machine, and a stall that made the loop abandon
  missed ticks would skip that much daylight while the players saw nothing move.
  Because the loop ticks whether or not anybody is connected, an unattended server's
  night arrives on time.
- **The clock moves before anything that could read it.** It is the first statement in
  `Step`, so every consumer within one tick — the `tick_of_day` in each snapshot, and
  whatever asks `IsNight` — sees one value, and there is no ordering question about
  which half of a tick ran before the day moved.
- **`game.IsNight` is the only place the boundary is decided.** Nothing else compares a
  clock against `NightStartTicks` or `NightEndTicks`, and nothing else may: a second
  comparison is a second answer to "is it dark", and the two disagree the first time
  somebody moves a boundary by one tick. It is half-open, `[start, end)`, so the two
  boundaries partition the day with no tick counted twice. **The spawn director is its
  one production caller**, and it asks twice a tick for two different rules — whether a
  creature may arrive, and whether one that is hunting nobody may stay. Both are that
  one comparison; neither is a second one.
- **The day length is the one duration not derived from the tick rate**, so
  `-tick-rate 40` really is a ten-minute day. Everything else stated as a duration —
  `DeathDuration`, the draugr's timings, the drop lifetime — is converted per server,
  because three seconds of death has to be three seconds everywhere. A day is stated in
  *ticks* because ticks are what crosses the wire: `ServerWelcome` announces the number
  and `EntitySnapshot.tick_of_day` is measured against it, so both sides count the same
  integers with no rounding rule to agree on. Derived instead, the boundaries would be
  per-server values and `IsNight` could not answer without being handed a simulation.
  The constant says so, at length, where somebody reaching for `-tick-rate` will read it.
- **The three numbers are announced from the constants, never from `session.Config`.**
  Every field of that struct is something an operator sets; these are the design, and
  putting them there would invent a knob and then have to validate it. `Handshake` reads
  `game.DayLengthTicks` the same way it reads `protocol.InventorySlots`.
- **A stored tick at or beyond `DayLengthTicks` is refused, never wrapped.**
  `% DayLengthTicks` would turn a byte-mangled four billion into an ordinary
  mid-afternoon and destroy the only evidence anything was wrong. The refusal lives in
  `game.Sim.RestoreClock` and not in `persist`, for the reason `game.Life.Validate`
  exists: that package judges what a *file* can be wrong about — magic, version,
  checksum, size — and what the bytes are allowed to *mean* is decided where the
  constants are. `persist` does not import `game` and must not start.
- **`clock.bin` is sixteen fixed bytes** — magic, `ClockVersion`, `tick_of_day`, CRC —
  on the delta store's discipline through the helpers `internal/world` exports, with its
  own version number for the reason the structures file has one. **It is the one file
  under the world directory whose size the format fixes**, so the read's size check is an
  equality rather than a ceiling and a truncated file and an over-long one are both
  refused before a byte is loaded. `world.StoreVersion` is deliberately untouched:
  bumping it invalidates every stored chunk delta in every existing world.
- **What is not in it: the absolute tick, and the day length.** The absolute tick only
  increases, and storing it would have a restarted world claim an uptime it never had.
  The day length is a constant of this build, not a property of the world — a copy on
  disk could only ever disagree with `game.DayLengthTicks`, and the range check above is
  what a build whose day shortened uses instead.
- **No dirty flag, and the absence is the point.** The camp has one because a world
  nobody is building in should cost no I/O; there is no such thing as a world in which
  time is not passing, so a flag would be set on every pass. The clock is written
  unconditionally on the same `-save` interval and once more at shutdown, after
  `workers.Wait()` — the tick loop is a worker, so that is the first moment the day has
  genuinely stopped and the flush is the last word on where it stopped. **A failed write
  is not re-marked either**, and needs no equivalent of `MarkStructuresDirty`: the next
  pass reads the live clock, which is newer than the one that failed, so nothing is lost
  by dropping a failure on the floor.
- **An unreadable clock is logged and survived, and the file is kept** — `restoreClock`
  is `restoreStructures`' discipline over one number. It does not stay kept for long,
  because with no dirty flag the next autosave rewrites it; it survives the start that
  could not use it, which is the window an operator has to look at it in. That is the
  trade a value which changes every tick forces, and it is why a corrupt player record
  is *quarantined* under a timestamped name instead: a clock costs a player their place
  in the day, and a record costs them a life.
- **An ephemeral world keeps a clock in memory and writes nothing.** A nil
  `*persist.ClockStore` is a no-op at every call site rather than a branch at each one,
  the shape a nil `*Store`, a nil `*StructureStore` and a nil `world.Store` already
  have. Its night still arrives on time; it just does not remember which part of the day
  it was in.

## The spawn director, and what stopped being true when it landed

Everything is in `internal/game/spawn.go`, beside the state machine it populates
(`mob.go`) and reading the constants in `constants.go`. It is the first and only
production caller of `game.IsNight`.

- **Nothing exists because the server started.** There used to be one draugr, placed at
  boot from a seed-derived anchor and replaced at that same anchor ten seconds after
  anyone killed it. `world.MobAnchorAt`, `MobAnchorOffset`, `Sim.SpawnDraugr`, the
  `mobAnchor` / `haveMobAnchor` / `mobRespawnTicks` fields and `DraugrRespawnDelay` are
  all gone rather than disabled — left in place they are dead code somebody wires back
  up, and "the world has one draugr" would be true again in a world that had stopped
  being built for it. `world` has gone back to not knowing what walks on its terrain; do
  not add a "where should a mob go" helper there.
- **There is no exported way to create a mob.** `spawnMobLocked` is the one path in, and
  its one production caller is the director. A mob exists because the dark put it near a
  player, never because a caller outside the simulation asked for one. The tests that
  need a creature in a particular place reach for the locked helper, which is what keeps
  that fact from being weakened for their convenience. **It refuses a kind `mobRegistry`
  does not hold**, and that refusal is what makes `mob.species()` total for everything
  already in the world.
- **The director runs inside `Sim.Step`, after `advanceMobsLocked`.** Same reason the
  swings run after the movement: what it decides is decided against the positions this
  tick produced and the target each mob chose in it. It reports whether the population
  changed, and the tick re-reads the sorted mob list when it did, so the snapshot is the
  world as the tick left it — one created here is in it and one removed here is not.
- **Only the spawn is once a second; both removals are every tick, and the split is
  deliberate.** The spawn is the expensive question (it reads a column of terrain) and
  the one whose rate is a gameplay decision. "A *nocturnal* creature with nobody to hunt
  does not survive the sun" is a rule about a clock that moves every tick, and asked once a second
  it would leave one standing in the daylight for up to a second, on a different tick for
  every mob. "Outside every cube for five seconds" is a *counter*, and a counter advanced
  on one tick in twenty is not measuring seconds.
- **Two caps, and the second is not redundant.** At most `MobsPerPlayer` inside one
  player's streamed cube — read through the same `withinView` the snapshot's visibility
  filter uses, because a cap measured on a different volume is a number about nothing —
  and at most `MobsPerPlayerWorldwide × connected players` in the world. The ceiling is
  above the per-player cap on purpose, so the per-player number stays the one that binds:
  what the ceiling catches is the population the per-player count *cannot see*, which is
  every mob that has left every cube and is still inside its despawn grace, and the
  moment after a disconnect when the world holds more than the remaining players justify.
  With nobody connected the ceiling is zero.
- **The legality test is the collision's `Terrain` seam, so a player's own digging
  counts.** The column is scanned *downwards* from the top of that player's streamed
  cube, and the first solid voxel it meets is the surface — which makes "there is ground
  here" and "there is sky over it" one answer to one question, and is why nothing spawns
  in a cave. This game has no light propagation, so dark underground is a question
  nothing here can answer; night plus an open sky is the rule the server actually checks.
- **An absent chunk answers solid, so the residency check is what makes the scan safe.**
  A column of terrain the cache has not composed would otherwise read as perfectly good
  ground under a perfectly clear sky. The surface voxel is re-read with `Terrain.Block`,
  which reports residency, and a non-resident one is a refusal.
- **The headroom is asked for by block rather than inferred from the scan, and today that
  question cannot come back no.** `Solid` is `!resident || block != Air`, so "not solid"
  is exactly "resident air", and a scan that stopped at the first solid voxel has already
  left air in every cell above it. **That equivalence belongs to the palette, not to the
  rule**: it holds only while nothing in this world is passable, and the first fluid a
  body can wade into ends it — the scan would walk straight down through the water and
  hand back the lake bed with the lake still on top of it. The criterion is two blocks of
  air, so the director asks for two blocks of air, in the shape `footprintFitsLocked`
  already asks it for a structure's footprint. `TestNothingSpawnsInsideAFluid` scripts
  such a block, so the check is pinned rather than dormant: delete it and a test goes red
  today, not on the day somebody adds water.
- **One draw and one legality test per player per pass, and a refusal is not retried
  inside the pass.** That is what makes a pass a constant amount of work whatever the
  terrain looks like: a player standing in the middle of a lake would otherwise cost an
  unbounded search on the tick goroutine, under the lock every other player's tick is
  waiting on. The draw is a square that contains the ring, so about three in eight land
  in the corners and spawn nothing — a second of waiting, in a night six minutes long.
- **`CampfireSafeRadius` is declared here and read by the campfire issue, not the other
  way round.** The predicate is a function over placed structures of that kind and is
  correct when there are none, which is why the director never waited on the fire being
  buildable. It reuses `stationWithinLocked` — the forge asks it to find somewhere to
  work and the director asks it to find somewhere to keep away from, and a second
  implementation of "is one of these within r of here" is a second answer that can
  disagree.
- **The PRNG is owned by `Sim`, seeded from the world seed, and advanced only inside the
  locked tick.** `NewSim` takes the seed for that and for nothing else — the simulation
  still generates no terrain and the seam it reads chunks through has no seed on it. A
  package-level `rand` is shared with every other goroutine in the process and a
  generator advanced outside the tick depends on *when* it was asked; owned here, the
  same world given the same ticks produces the same creatures in the same places, which
  is what lets `spawn_test.go` assert positions exactly instead of statistically. The
  draw is integer arithmetic for the reason terrain generation is: a float expression may
  round differently on another architecture, and a position asserted exactly must not
  depend on which machine ran the test.
- **`math/rand/v2` is standard library**, so none of this added a dependency; `go.mod`
  still has one.
- **Still nothing about a mob is persisted.** A restart loses whatever was hunting, and
  the director puts creatures back where the players actually are — which is a better
  answer than a file could give.

## Two species, one state machine — and the registry that is the difference

`internal/game/species.go` holds `mobRegistry`, one row per `vnet.MobKind`. `mob.go` is
what a creature *does*, `spawn.go` is what puts one in the world, and both read their
numbers from that table rather than from constants of their own.

- **It is the move `itemRegistry` made, and it was made for a bug that was already in the
  code.** `swingTargetLocked` built `draugrBody.boxAt(m.pos)` for every mob it
  considered. One species, one box, and the arithmetic was right — for exactly as long as
  there was one species. A vargr is wider and much shorter, so that line would have given
  it a draugr's reach in both directions while appearing to measure it. The box moved into
  the row **with** the second species rather than after it, which is why `draugrBody` no
  longer exists as a name.
- **A vargr is a draugr's brain with different numbers in front of it.** Health, speed,
  aggro range, attack range, damage, windup, recovery, body and `nocturnal` — that is the
  whole of a species. Two state machines would be two things to reason about and two
  places for the same bug; one state machine parameterised by a table is one of each. **Do
  not add a `switch` on `MobKind` anywhere.** If something differs by species it is a
  column; if a column would hold the same value for every row it is not a species
  difference.
- **Every field is a real number, and that is the opposite of `itemRegistry`.** There a
  zero `maxDurability` is the documented way to say "this does not wear out"; here nothing
  reads that way, because a creature with no health is already dead and one with no body
  occupies nothing. `TestEverySpeciesIsFullyDescribed` sweeps every row for a zero.
  `nocturnal` is the single exception — its `false` is a species that hunts by day. An
  empty `loot` table is **not** a second exception: the sweep refuses one, because a
  creature nobody gains anything by killing is a decision rather than a default.
- **`nocturnal` is a property of the creature, and both ends of the sentence read the same
  field.** `spawnableSpecies` decides who may *arrive* at this hour and
  `removeSpentMobsLocked` decides who the dawn *takes*; a species allowed in by one and
  removed by the other would either wander through the whole day or be deleted on the tick
  it arrived. The director never asks "is it night, therefore a draugr" — it asks the
  registry which rows this hour allows and draws one with the same generator it draws a
  position with, so the sequence advances by the same amount whatever the hour.
- **"Nothing spawns in daylight" stopped being true, and it was never the rule it looked
  like.** It was a statement about the draugr wearing a clock's clothes. What is true is
  that a nocturnal species arrives only in the dark.
- **The two timings are per species and still converted per server.** `Sim.mobTimings` is
  `mobTimingsFor(tickRate)`, built once at construction beside every other duration
  `NewSim` turns into ticks. The vargr's 400 ms telegraph is the shortest in the game, and
  `ticksFor` never rounds one to zero.
- **The registry is not sent to clients**, exactly as `itemRegistry` is not. A snapshot
  carries the kind, the position, the health and each creature's *own* maximum — that last
  one used to be a single constant, which would have drawn a full-health vargr at 35 of 60.
- **The wire is ahead of the client here, deliberately.** `MobKind.Vargr` has existed since
  Protocol V6 and `client/src/net/codec.rs` still answers `None` for it, so a client refuses
  a snapshot carrying one. That is the fail-closed rule working as designed, and drawing a
  vargr is the separate issue that owns it — the server half is finished first because the
  contract was reserved first.

## What the dead leave behind, and the lock the drop had to get past

`internal/game/loot.go` owns the roll and the spawn; the table itself is a `loot` column of
`mobRegistry`, because what a creature is worth killing belongs beside what it costs to
kill. A draugr leaves 1..2 bones, a vargr leaves exactly one pelt, and two pelts are a
`RecipeIDLeatherPatch` away from a field repair.

- **A kill, and only a kill.** `damageMobLocked` is the single caller of `rollLootLocked`,
  which is the whole of the rule: the director's two removals — dawn, and "outside every
  streamed cube for five seconds" — `delete` from `Sim.mobs` without going near it, so a mob
  that despawns leaves nothing. Loot is the reward for the kill, and a world that paid it out
  for having existed would be a world where waiting is a strategy.
- **The lock is the whole design, and it is why `Step` is two functions.** A mob dies inside
  the tick, under `Sim.mu`; `spawnDrop` takes `Sim.mu` itself, because its other callers are
  session goroutines. Spawning at the point of death therefore deadlocks the server on the
  first kill anybody makes. So the death **collects** and the tick **spawns**:
  `resolveSwingLocked` and `damageMobLocked` return `[]lootDrop`, `stepWorld` gathers them
  and returns them under the lock, and `Step` — which holds nothing — is one line:
  `s.spawnLoot(s.stepWorld(tick))`. That is `collapseStructuresAt` / `dropCollapsed` from
  `edit.go`, written for a caller that is already inside the critical section.
- **The consequence is one tick, and it is worth knowing rather than discovering.** Loot
  spawns after the killing tick has already encoded its snapshots, so a kill on tick N is a
  drop on tick N+1 — the same tick a mined block's yield waits, and pinned by
  `TestAKillInsideTheTickNeitherDeadlocksNorMissesTheNextSnapshot`. A re-entrant lock or a
  `spawnDropLocked` twin would buy that tick back and cost the one-way lock discipline the
  rest of this file rests on.
- **`Sim.loot` is its own generator, not a share of `Sim.spawns`.** Both are seeded from the
  world seed on different PCG streams (`mobLootStream`, "voxellot"). One generator for both
  would make a kill shift every later spawn position in the world — "where does the dark put
  the next creature" would depend on what the player had killed — and `spawn_test.go` pins
  exact positions on the assumption that it does not. This is `mobSpawnStream`'s own argument
  read from the other side: the constant is what makes a stream *this* system's.
- **Determinism is a requirement, not a preference.** Counts come from `Sim.loot`, guarded by
  `mu` and advanced only inside the locked tick — no package-level `rand`, no wall clock — so
  the same world leaves the same items on the same ground and a test asserts an exact drop
  rather than a distribution. `IntN` over an inclusive `min..max`, integer arithmetic
  throughout for the reason the ring draw is.
- **A loot drop is an ordinary drop, with no special case anywhere.** It goes through
  `spawnDrop`, so it merges with what is already lying there, ages out after `DropLifetime`,
  is collected by walking over it, and is refused if the item is unregistered or the count is
  zero. `lootDrop` carries a voxel rather than a `*mob` because by the time it is spawned the
  creature has already left `Sim.mobs`.
- **Nothing consumes a bone yet, and `TestNothingConsumesABoneYet` is what makes that a
  claim.** It is the reagent GDD §7's engraving table will want. A resource with no sink is a
  resource; the alternative was a creature that leaves nothing, which the sweep now refuses.

## Persisting the world, and what is deliberately not persisted

Everything here lives in `internal/world/store.go`; the flag, the seed check and the final
flush are wired in `cmd/voxelheimd/main.go`.

- **Only the deltas are written; the generated base never is.** The base is a pure function
  of the seed, so storing it would be spending disk on something the process can always
  recompute — and it would erase the distinction the GDD's Fimbulvetr storm is built on.
  Restoring an unprotected chunk to its original state has to stay "throw the deltas away",
  which it can only be while what is on disk *is* the deltas. The size follows from the
  same fact: a chunk nobody has touched costs zero bytes, and one edit costs 34.
- **One file per chunk, and a region format is not an optimisation until something is
  measured.** A save touches exactly the chunk that changed, a load reads exactly the chunk
  being composed, and the atomic rename below needs no reasoning about neighbours sharing a
  file. Packing many chunks into one file — Minecraft's `.mca` — trades all of that for
  fewer inodes, and nothing here has yet measured that the inodes cost anything. The
  interesting number is how many *edited* chunks a played-in world accumulates, and this
  format is what will produce it.
- **The recorded seed is a precondition, not a hint.** A world directory opened under a
  different seed is a refusal to start. Loading it would not fail — a delta names a voxel by
  its index inside a chunk, and every index resolves against any terrain — it would quietly
  serve one landscape wearing another world's digging. The check runs *before the listener
  is bound*, for the same reason flag validation does.
- **So is the recorded worldgen version, and the seed alone does not cover it.** "The base is
  a pure function of the seed" is true of a *fixed* generator; change `world.Generate` and the
  same seed yields a different landscape, so the stored indices still all resolve and the
  failure is the one above, arriving by the other road. `StoreVersion` cannot stand in for it
  — that guards the file's layout, and worldgen can change without a byte of the layout
  moving. `world.bin` therefore records `WorldgenVersion` beside the seed, and a mismatch is
  the same refusal. **Bump it whenever you reach for `-update-golden`**: the golden chunk test
  failing is the moment the generator changed, and it is the only reminder there is — this is
  a number a human maintains, not a hash of the function. Found by the review on #65.
- **Writes are atomic: temporary file, flush, rename, in that order.** The temporary file is
  created in the destination directory, because rename is only atomic within one filesystem.
  Writing in place would leave a truncated file that parses perfectly as a *shorter* edit
  list, which is to say as a shelter with some of its walls back. A crash between the two
  leaves an inert temporary file that no reader opens and the next `OpenStore` sweeps.
- **Every file carries a magic number, a format version and a trailing CRC, and a bad one is
  an error rather than a fallback to terrain.** "Read it as terrain" is the single answer
  that silently discards what a player built and then invites them to dig the replacement.
  The version is not there for migration — there is one version — but so that a later build
  can *refuse* an old file instead of reading the bytes it recognises and guessing at the
  rest. Bump `StoreVersion` for any layout change, including one that only appends.
- **A save takes neither `composeMu` nor `mu`.** That is what "saving does not block the tick
  loop or a session's read path" means concretely, and
  `TestASaveTakesNeitherTheCompositionNorTheEntryLock` holds both locks and watches a flush
  finish anyway. The only lock a save shares with an edit is the delta layer's own, held for
  the length of a map copy. `Cache.Apply` does no I/O at all: it marks a coordinate and
  returns.
- **The dirty set is cleared before the snapshot is taken, never after.** An edit landing in
  between then re-marks the chunk, so the worst case is writing the same bytes twice. The
  other order has a window in which an edit is in neither the file being written nor the set
  of chunks still to write, and that edit is simply gone at the next restart —
  `TestEditsAndSavesRunConcurrently` fails within a handful of runs when the two are swapped.
- **Stored edits are loaded once, on the way into a chunk's first composition**, inside the
  generation semaphore and outside every lock. There is no separate "hydrated" set: the delta
  layer already holding edits for a coordinate *is* that record, because disk is only ever
  written from memory and `Deltas` never forgets an edit, so memory is never behind disk.
  That check is a fast path; `Deltas.Restore` refusing to overwrite is the guarantee, and it
  is what makes an orphaned generation of an evicted chunk finishing late harmless.
- **The final flush is inside `server.shutdown`'s ordering, at the end of it.** The autosave
  loop is a worker, so `workers.Wait()` is the first moment at which no session can still be
  recording an edit and nothing else is writing to the directory — which makes that one flush
  the last word on what the world holds. Earlier races an edit into oblivion; later, from
  `main`, runs after the process has told itself it stopped.
- **`-world-dir ""` is an ephemeral world**, chosen explicitly and logged as a warning. It is
  how the tests in this repository run, and the reason every persistence path is a no-op
  against a nil store rather than a branch at each call site.


## Generated bindings

Committed, never hand-edited, regenerated with the flatc release pinned in `.flatc-version` at the
repo root. From the repository root:

```bash
flatc --go --go-module-name github.com/FabioSM46/voxelheim-v2/server/gen -o server/gen -I schemas schemas/*.fbs
gofmt -w server/gen
```

Two details in that recipe are load-bearing:

- **`--go-module-name`**: without it the generated files import each other as `Voxelheim/Net`,
  which is not a resolvable module path and does not compile.
- **`gofmt -w server/gen`**: flatc's output is not gofmt-clean, and CI's `gofmt -l .` covers the
  whole workspace. Formatting is part of generation here, not an edit to generated code — it is
  deterministic and idempotent, so regenerating and reformatting yields no diff. The rule against
  hand-editing `gen/` is intact: never fix a binding, always regenerate it.

A `schemas/**` change rebuilds both consumers — CI runs the `schemas`, `server` and `client` jobs
for any contract diff — so regenerate here and in the client in the same PR.

## Running it

```bash
go run ./cmd/voxelheimd                       # 127.0.0.1:7777, world kept in ./world
go run ./cmd/voxelheimd -listen 0.0.0.0:7777  # reachable from another machine
go run ./cmd/voxelheimd -listen 127.0.0.1:0   # a free port, printed in the listening line
go run ./cmd/voxelheimd -seed 42              # a different world; the same seed is the same world
go run ./cmd/voxelheimd -log-level debug -log-format json
go run ./cmd/voxelheimd -h                    # every flag, with the default it actually holds
```

`-h` is the list, deliberately: the defaults are constants in `internal/game`, `internal/world`
and `internal/session`, and a table here restating them would be a copy that drifts. What the
flags decide is the part worth writing down.

| Flag | Decides |
| ---- | ------- |
| `-listen` | the address to bind. A `:0` port binds a free one and the startup line names it |
| `-seed` | the terrain. It is regenerated from the seed, never read from disk |
| `-world-dir` | where edits, player records, the clock and the TLS key are kept. Empty runs an ephemeral world |
| `-tick-rate` | authoritative simulation ticks per second (1..255) |
| `-view-distance` | the chunk streaming radius, in chunks (0..16) |
| `-handshake-timeout` | how long a new connection may say nothing before it is closed |
| `-idle-timeout` | how long an admitted session may say nothing. Must be at least the handshake timeout |
| `-log-level` | `debug`, `info`, `warn` or `error` |
| `-log-format` | `text` or `json` |

Every one of them is validated before it is narrowed — see `validate` in `cmd/voxelheimd/main.go`
and the reasoning above it, which is the pattern any second command copies.

### What the world directory holds, and the one thing that surprises people

It is the default (`world`, resolved against the working directory, so `cd server && go run …`
writes `server/world/` — git-ignored) and it holds four things: the chunk deltas players made, the
player records, the clock, and **the server's TLS key**. The terrain itself is not in there; it is
a function of `-seed`.

That last item is why `-world-dir ""` costs more than the edits it discards. A server with
nowhere to keep a key mints a new certificate on every start, and the client pins a fingerprint
per address and refuses any other one — so an ephemeral server is a client that stops connecting
after the first restart, which reads as a networking bug and is not one. The server says so in a
startup warning, and the client's refusal names the pin file and both fingerprints. Whoever runs
the server can read the real one out of the startup line:

```
level=INFO msg="listening with an encrypted session" certificate_sha256=…
```

Writing that value into the pin file the client names is the supported way through it; see
"Running it" in `client/AGENTS.md`. In development, keeping the default world directory is the
way it never comes up.

## Gates

Run from `server/`, and all five before opening a PR:

```bash
test -z "$(gofmt -l .)"
go vet ./...
golangci-lint run          # config in server/.golangci.yml
go build ./...
go test ./...
```

CI runs exactly these, with golangci-lint pinned to the version in `.github/workflows/ci.yml`.
`go test -race ./...` is not in the gate but is worth running whenever you touch a goroutine.

The Go toolchain is pinned by the `go` directive in `go.mod`: CI reads it through
`go-version-file`, and a local toolchain older than the directive downloads the right one
automatically (`GOTOOLCHAIN=auto`).

## Known gaps, deliberately

Recorded here so the next reader does not mistake them for oversights:

- **Below about 10 Hz nothing can jump onto a one-block step.** `JumpImpulse` under a fixed
  timestep loses height as the step coarsens: the apex is 1.429 blocks at 255 Hz, 1.230 at 20,
  1.020 at 10, 0.938 at 8 and 0.680 at 5. It is the integrator rather than any one caller, so it
  applies to a player pressing jump exactly as it does to a draugr climbing a step — which is why
  `TestADraugrClimbsAStepAtEveryRateThePhysicsAllows` starts its sweep at 10 Hz and says so.
  Raising `JumpImpulse` to cover 5 Hz would change how every jump feels at 20; the honest fix is a
  sub-stepped integrator, and that is its own issue.
- **A life survives a disconnect; the session around it deliberately does not.** What is written is
  position, yaw, health and all 36 slots with their durability — `game.Life`, captured by
  `Player.Record` and stored by `persist`. What is **not** written is everything that only means
  something inside one connection: the death countdown, the respawn protection window, mining
  progress, a pending swing, the three client-tick ordering guards, and the drops and mobs in the
  world. A returning player therefore arrives with their pack and their health, standing where they
  logged out, settling by falling exactly as a new join does (`onGround` is false either way) — and
  with none of the timers a previous session was part-way through.

  **A record always describes a living player**, which is what makes quitting mid-death neither an
  escape nor a double charge. A player who is dead when their record is taken is written as
  `respawnLocked` would have left them: alive, at `PlayerMaxHealth`, at `respawnPositionLocked` —
  their tent if one stands, the join spawn otherwise — with the −20% durability penalty charged if
  the tick had not managed it yet. `chargeDeathPenaltyLocked` is the one-shot both paths go through,
  so whichever gets there first, it is spent once. `game.Life.Validate` refuses a health of zero for
  the same reason: it is not a corpse to restore, it is a record this server did not write.

  **Three write paths, and no fourth.** `Serve`'s teardown (every way out, an expired read deadline
  included), the autosave beside the world's on the same `-save` interval
  (`world.DefaultSaveInterval`, 5s), and the shutdown — which needs no flush of its own, because
  `shutdown` waits for every session and each one writes its own record on the way out. The autosave
  exists for the death nobody gets to tear down cleanly: a `kill -9` costs at most one interval.
  Capture and write are separate everywhere, the `takeDirty`/`Flush` discipline: `Player.Record`
  takes `sim.mu` and then the inventory lock, copies, and returns; every byte reaches the disk with
  no lock held. **No disk I/O happens under `sim.mu` or on the tick goroutine.**

  **The teardown's record is the last word, and that is enforced rather than hoped for.** An
  autosave captures every connected player and then writes them one at a time, so a session can end
  inside one pass — leaving the pass holding a life older than the one that session's teardown
  wrote. `Identities.RememberAll` therefore skips an identity with no live claim or whose teardown
  has already written, and both paths take one write lock, so a teardown lands entirely before a
  pass or entirely after it. Without that, a player who died and quit inside a pass could come back
  from before the death with the penalty unpaid.

  **A corrupt record is refused whole, kept, and the player joins as new.** Non-finite position or
  yaw, an unknown item id, a slot violating an `InventoryState` invariant, a bad checksum, a version
  this build does not speak — all the same answer: `Identities.recall` logs it at error, `Quarantine`
  renames the file to `<id>.bin.corrupt.<nanos>`, and resolution mints a **new** identity, so nothing
  the session goes on to write can land on the record nobody could read. The timestamped suffix is
  not decoration: a fixed `.corrupt` would destroy the previous one. A record that is *unreachable*
  rather than corrupt — a permission, a failing disk — still refuses the connection, because a retry
  may succeed and reading it as "no record" would throw away a good life on a transient fault.

  **`persist.StoreVersion` is 2 and there is no migration.** A v1 record held a name and a timestamp,
  which is not enough to reconstruct a life, and nothing has shipped. `CheckHeader` refuses it like
  any other unknown version. Note that it takes the caller's version rather than a package constant,
  so the player record and the chunk record version independently.

  **What identifies a player.** An identity is a 32-byte token the server mints from `crypto/rand`
  and announces in `ServerWelcome`; the client presents it in its next `ClientHello`, and the server
  recognises it by looking up its SHA-256. The token is the credential and the hash is the name:
  `<world-dir>/players/<player-id-hex>.bin` holds a display name, a last-seen time and the life under
  the hash, so a leaked players directory is a list of hashes rather than a list of credentials.

  **One live session per identity**, refused with `RejectReason.ALREADY_CONNECTED`; the older
  session is never kicked, and `-idle-timeout` is what keeps a dead one from holding an identity for
  long. **What a token is not**: an account, a password, or anything rotatable or revocable. It is a
  bearer credential, so whatever can read one *is* that player. What protects it is the transport
  and nothing in this directory: the session is encrypted with no way to ask for otherwise, and
  the client refuses a server whose certificate is not the one it pinned. Before that landed,
  anyone who could watch a handshake could copy a token and come back as that player;
  `schemas/handshake.fbs` states both configurations rather than either alone.

  **In an ephemeral world (`-world-dir ""`) tokens are still minted and identities are still
  exclusive**, and nothing is written — no record, no life. No token is ever recognised, so every
  connection is a first one and a reconnect is a new player at the spawn even within the same
  process. The client cannot tell that apart from a server that has never seen it, and the contract
  already requires it to store whatever token arrives. The flag's own help text says so.
- **No backpressure policy beyond a bounded queue.** A client that stops reading eventually blocks
  its own writer; nothing yet disconnects it.
- **Two goroutines may deliver an `InventoryState` out of order.** Every session-side sender —
  `Player.Edit`, `Player.MoveInventory` and their callers in `session.go` — captures the state
  *under* the inventory lock and encodes and enqueues it after releasing, so a pickup resolving on
  the tick inside that gap delivers the newer state first and the client's last word about its own
  pack is the older one. Nothing resends it, because both senders clear their own reason to.
  The window is microseconds and the state is absolute rather than a delta, so the next real change
  repairs it — but "the next real change" can be a long time in a pack nobody is touching.
  `offerInventoryLocked` holds the lock across its own non-blocking deliver, which closes the
  opposite direction and is free; closing this one needs either a single sender (let the tick
  deliver every inventory state, with the durable retry it already has) or a version the receiver
  can compare, and both are a change to the delivery contract rather than to this code. **It
  predates item drops** — the same reorder was reachable between a placement on the read loop and a
  mined break's insertion on the mining worker. Found by the review on #89.
- **The chunk cache is bounded by count, not by memory.** 1024 chunks is roughly 70 MiB with their
  encoded payloads; a smaller machine or a larger `ChunkSize` would want a byte budget instead.
- **Players do not collide with each other.** Only with terrain. Entity-versus-entity collision
  needs a broadphase and a decision about who yields, and neither belongs in a walking skeleton.
- **Snapshot fan-out is O(sessions × entities), and every snapshot is complete.** No spatial index,
  no delta compression, no interest management beyond the view-distance cube. All three are worth
  building when the quadratic term matters and not one issue before.
- **A camp can outlive the block it stands on, for one crash.** The chunk deltas and
  `structures.bin` are two flushes, and nothing orders them against each other, so a `kill -9`
  landing between them can bring the server back with a structure over ground somebody had
  already dug away. It stays there until the next edit of one of its footprint cells, which
  collapses it through the ordinary rule — so the world heals, it just does not start healed.
  The fix nobody should reach for is validating support at load: it would generate every chunk
  under every camp before the first session is accepted, which is a startup that scales with how
  much the world has been built in. A shared transaction across the two stores is the honest fix
  and is its own issue.
- **A restored camp may overlap, because a placed one may.** `PlaceStructure` validates a
  footprint against *terrain* and not against the other structures standing in it, so a forge
  inside its owner's tent is legal today. `Sim.RestoreStructures` therefore accepts overlap too,
  and the symmetry is the point: load-time validation is exactly as strict as placement and no
  stricter, because a rule that refuses what this server writes turns a camp somebody built into
  an unloadable file at the next restart. If structures ever stop being allowed to overlap, the
  rule goes in `PlaceStructure` **first** — `TestARestoredCampMayOverlapExactlyAsAPlacedOneMay`
  fails when that happens, and is the place to record the new answer.
- **An owner who never comes back leaves their camp standing.** Nothing reclaims a structure
  whose owner has stopped playing: the identity keeps naming them, the tent keeps being theirs,
  and no sweep runs. That is the intended shape rather than a gap to close early — the GDD's
  Fimbulvetr storm is the reclaim, and it is its own issue.
- **No fall damage, no health, no death.** A player who falls a hundred blocks lands and walks
  away. `TerminalFallSpeed` exists to bound the per-tick step, not to model anything.
- **No anti-cheat beyond the speed clamp and the discard rule.** A client can send input as fast as
  it likes; the server only ever applies the newest one per tick, so the ceiling is the tick rate.
  Rate limiting the *socket* is a backpressure issue, not a movement one. `ChunkResendRequest` is
  bounded because it is the one message whose *work* is unbounded per frame — a chunk composed, and
  possibly generated — not because a general limiter arrived.
- **A delta that restores the generated value is still a delta.** Filling a hole back in
  costs two stored edits rather than none, because `Deltas` never forgets and detecting the
  restoration would mean generating the chunk to compare against — the millisecond-scale work
  the edit path is built to stay out of. A world dug up and refilled therefore keeps growing.
- **The chunk directory is flat and never compacted.** One file per edited chunk, no packing,
  no pruning. Bounded by how many chunks players have actually touched, which is the number
  worth measuring before anyone reaches for a region format.
- **The file's bytes are flushed before the rename; the directory entry is not.** A process
  crash cannot leave a truncated chunk. A power loss can still lose the rename itself, and
  buying that back means an fsync on the directory per save.
- **Edits are not rate limited either.** A client may ask for one per frame, and each accepted one
  costs a chunk clone and a re-encode. The cheapest place to bound that is the same socket-level
  limit the line above asks for.
- **A `BlockUpdate` dropped by a full outbound queue costs that session the whole chunk.**
  `BroadcastChunk` forgets it in the view so the next diff re-sends it composed — which is the
  recovery a failed chunk send already relies on, and better than leaving the client permanently
  wrong about a voxel. But the diff only runs when the player crosses a chunk border, so a
  *stationary* player whose queue was full at that moment stays stale until they move. **The wake
  that would fix that now exists and is deliberately not used here.** Waking the streamer from a
  broadcast turns one dropped 30-byte frame into a re-sent chunk, and under sustained load into a
  re-send storm — the broadcast is the one caller with no bound on how often it fires, because it
  fires on somebody *else's* edits. The client cannot ask for this one either: a dropped
  `BlockUpdate` is an update it never saw, so it does not know it is stale. The real answer is
  still the backpressure policy two bullets up.
- **A chunk edited while it is in flight is re-sent, up to twice.** Between reading a chunk and
  `MarkLoaded`, `View.Holds` is false for that session, so `BroadcastChunk` skips it: an edit
  accepted in that window reaches everybody else and not the session being streamed to. `sendChunk`
  closes it by comparing the chunk pointer after marking — an edit publishes a clone rather than
  mutating, so an unchanged pointer is proof per chunk that nothing was missed, where `Revision`
  counts edits anywhere in the world and would re-send the stream because someone dug a kilometre
  away. Two things it deliberately does not do. It does not loop for ever: past `maxChunkResends`
  it falls back to the repair — `Forget` plus a wake, so a *stationary* player is served rather than
  left waiting — and it asks **once**. A follow-up pass that loses the same race forgets the chunk
  and stops asking, because a chunk being edited faster than one session can be sent it is not a
  race a fourth pass wins, and asking anyway is the re-send storm the bullet above describes. And
  it does not order a re-send against a `BlockUpdate` broadcast concurrently — from the second pass
  on the chunk is marked held, so the update is delivered anyway and arriving twice is harmless,
  but an update for an edit made between the re-read and the re-send can still reach the client
  ahead of the chunk that predates it. Found by review on PR #54; the window is one step wider than
  the finding said, because it opens at the read rather than at the send.
- **A block cannot be placed inside a player, but the check is not atomic with the write.** A tick
  landing in the microseconds between them can still move somebody into the voxel. The consequence
  is bounded rather than absent: `moveAndCollide` refuses to move a player who is already inside a
  solid rather than teleporting them out, so they stand still until it is broken again. Closing the
  window means holding `sim.mu` across a chunk generation, which costs every connected player a
  tick.
- **Mining consults the owning session's delivered view before accepting a target.** `View.Holds`
  is the authoritative record for that fact; distance alone is insufficient at view distance 0.
  Placement retains the older behaviour in which the editor's own view is not consulted: a
  neighbouring edit may be applied while its `BlockUpdate` reaches only sessions that hold the
  chunk.
