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
| `cmd/voxelheimd` | flags, logger, listener wiring, the one read of the account service's public key, signal handling, shutdown | contain game logic |
| `cmd/voxelheim-auth` | the account service: its own flags, HTTP route table, port and directory | know anything about the game — it imports no package the simulation uses |
| `internal/transport` | framing, TCP, TLS, the `Transport`/`Conn` interfaces | know what a frame means |
| `internal/protocol` | FlatBuffers encode/decode, contract limits | know about connections |
| `internal/session` | one connection's lifetime, handshake admission, ticket verification, the character phase, entity ids, the one-session-per-account claim | decide gameplay outcomes |

| `internal/game` | the fixed-rate loop, every player, movement, collision, inventory, snapshots | read or write a socket |
| `internal/world` | chunks, terrain generation, the RLE codec, the chunk cache, the world directory | know that sessions exist |
| `internal/identity` | what an account is, what a player id is, and the one-way hash between them | import anything of ours, or mint anything |
| `internal/certs` | the server's own TLS certificate: generated once, kept under the world directory | implement any cryptography |
| `internal/persist` | the character store under `<world-dir>/players/` and the name index over it, the camp in `<world-dir>/structures.bin`, and the world's time of day in `<world-dir>/clock.bin` | be imported by `game` |
| `internal/auth` | the account store under `<auth-dir>/accounts/`: who a person is, never how they prove it | be imported by anything but `cmd/voxelheim-auth` |
| `internal/discord` | the Discord sign-in: OAuth 2.0 Authorization Code with PKCE, and the sign-ins in flight | import anything of ours, or keep anything the provider hands it |
| `internal/ticket` | the Ed25519 signing pair beside the accounts, what a session ticket says, and the offline check a game server makes with the public half | import anything of ours but `internal/world`, or offer any way to read the private key |
| `internal/registry` | the registered game servers under `<auth-dir>/servers/`: name, address, certificate fingerprint, and when each was last heard from | be imported by anything but `cmd/voxelheim-auth`, or import anything of ours but `internal/world` and `internal/ticket` |
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
carry the same five values, and `session` is the one place that maps between them. The duplication
is five field names; what it buys is that the store never decides what a life may say and the
simulation never decides how one is written down. Both use `protocol.InventoryStack` for the slots,
so the 40-slot shape has exactly one declaration.

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
- **A session ticket is a credential; an account is a name; a player id is a hash of it. Never
  confuse which one you are holding.** The ticket is 96 bytes the account service signed and one
  client presents, and whatever holds it can make the claim it carries — so it is never logged and
  never displayed. The account is the sixteen bytes inside it: not a credential, and redacted
  anyway, because a log naming accounts is a record of who plays here and when they were online.
  The player id is `SHA-256(playerIDDomain ‖ account)`: it names the same player, it is what the
  store keys on and what a log line carries, and it gives nothing away. `identity.Account` carries
  `String`, `GoString`, `LogValue` and `MarshalJSON` that all redact, and none of the four is
  redundant — slog's JSON handler would marshal the array as 16 numbers, which a Stringer never
  sees, and `%#v` prints `0x1f, 0x3e, …`, which neither of those sees.
  `TestTheCredentialNeverReachesTheLog` captures a whole handshake through both handlers and looks
  for the ticket, the account **and the signature alone** in hex, base64 and raw;
  `TestARefusedTicketNeverReachesTheLogEither` does the same for the path somebody actually
  investigates.

  **This replaced a model, and the model is worth knowing because its shape survived.** Through V6
  a player id was the SHA-256 of a *token this server minted* — 32 bytes of `crypto/rand` handed
  to one client and presented back. `internal/identity` no longer mints anything and holds no
  credential at all. What survived is the distance between the two values, because the reason for
  it never depended on where the first one came from: a log line and a file name should name a
  player without naming the person.
- **A handshake is three exchanges and the middle one is a person deciding.** `ClientHello` is
  answered with `ServerCharacterList`; a `SelectCharacterRequest` or a `CreateCharacterRequest`
  is answered with `ServerWelcome`; `ServerReject` is legal in place of any of them and closes
  the connection. `session.phase` is what decides which messages are legal and which deadline
  the next read is armed with, and `schemas/handshake.fbs` holds the reason the choice comes
  before the welcome: **`ServerWelcome.spawn` belongs to a character**, so a welcome sent before
  there is one would carry a position the client must not trust, and every other field in that
  message is authoritative the moment it arrives.
- **The account is admitted on the session goroutine, between the decode and the list, and never
  under `sim.mu`.** Admission reads the player store, and a tick that waits on a file is a tick
  every connected player misses. `session.Welcome` stays a pure function of its inputs — it is
  handed the settled character and is tested on the fields it announces — and
  `session.Identities` is where verification, the store lookup and the exclusivity claim live,
  tested separately. The rule, in order: a `session_ticket` of any length but 96 is
  `BAD_REQUEST`, **absent and empty included**, decided before a signature is checked; the ticket
  is then verified — signature, then world, then expiry, all arithmetic; the account it names
  becomes a player id and *only then* is anything looked up; the account is claimed, and one
  already playing is `ALREADY_CONNECTED`; and only then is the store read for that account's
  characters. A record the store holds and cannot read is the same answer as one it does not
  have, once the file has been set aside — see the corrupt-record rule under "Known gaps".
- **The claim is taken at the hello and not at the choice**, which is what makes a phase a person
  sits inside safe to have at all: two connections for one account would otherwise both browse,
  both select, and one of them find out afterwards. The cost is a window in which an account is
  live and has no character, and every reader of the claim fails closed over it —
  `Identities.stillPlaying` answers false, so an autosave that reached one would write nothing.
- **`ClientHello.player_name` decides nothing at all.** It is untrusted display text and it is no
  longer read: what a player is called here is the name their character was created under, which
  is the one that is unique on this world. **`player_token` is read past entirely, its length
  included**; `schemas/handshake.fbs` retires the field at V7 and a rule that survived would be a
  V6 rule refusing a V7 client over a field neither of them uses.
  `TestTheRetiredTokenFieldIsIgnored` presents every length the old rule refused.
- **Two refusals in the character phase are deliberately one answer.** A selection naming a
  character this world has never minted and one naming another account's are both `BAD_REQUEST`
  carrying the same sentence, because a client that could tell them apart could enumerate this
  world's characters by asking for ids it does not have — `game.Player.RemoveStructure`'s rule
  with a character in place of a camp. A refused *name* is the opposite case and says which of
  the three it was: the player picked it, and `CHARACTER_NAME_TAKEN`, `CHARACTER_NAME_REFUSED`
  and `CHARACTER_LIMIT_REACHED` are three different things to do about it.

- **The protocol version is settled before a ticket is verified**, and `session.unspeakable` is the
  one implementation of it. Admission happens between the decode and the answer, so while the
  version check lived where the welcome was built a client speaking an older protocol — which
  presents no ticket, because the ticket is what V7 added — was refused *for the ticket* and
  never told about the version: the one refusal it could act on, replaced by one it cannot. It is
  also the cheaper question, and it comes before an Ed25519 verification on bytes chosen by a
  connection nobody has authenticated.

- **The claim is released last in `Serve`'s teardown**, after `sim.Leave` and after the
  record write: `sim.Leave` → record write → release. Either other order is a reconnect served
  wrongly — refused for a session that has already gone, or handed a record that is still being
  written. It runs on every path out of `Serve`, an expired read deadline included, which is what
  makes an idle session hand its place back instead of holding it until a restart. **The ordering
  survived the move to the account unchanged**, and it had to: what the claim is keyed by changed,
  and every reason the order is what it is was about *when* the key is released rather than what it
  names. It survived the character phase for the same reason: the claim is taken a message earlier
  than it used to be, and the record write is still the last thing that happens before it is given
  back. A session that never chose a character writes nothing and releases anyway.

- **Leaving is an irrevocable ten-second server lifecycle, not a socket state.** A polite
  `LeaveRequest`, EOF, a dead writer and the post-welcome idle deadline all call
  `Player.BeginLeaving` and start the same `DefaultLeaveLinger`; idle and leave are sequential,
  so the idle timeout cannot remove the body early. During the linger the player stays in
  `Sim`: snapshots, gravity, damage and world interaction continue, while movement, mining and
  every other player action are cleared or refused. There is no cancel and no resumption. The
  ordinary damage and death path still runs: a respawn reached inside the linger remains inert
  because `leaving` survives it, while `Sim.Leave` removes the player from the only tick loop that
  can advance an unfinished respawn countdown — there is no timer that can resurrect it later. The
  account claim remains held, so a reconnect receives `ALREADY_CONNECTED` until `sim.Leave`, the
  final post-linger record write and claim release complete. Server shutdown may skip the wait
  because the world itself is ending, but it still performs that persistence ordering.

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
- **A silent connection is closed, on a read deadline the server arms.** Three flags, all
  validated at startup by `session.Timeouts.Validate` — which is the one place the rule lives,
  asked by `options.validate` rather than restated there. `-handshake-timeout` (default `5s`)
  bounds the first read: a connection that has said nothing has not yet claimed to be a client,
  so it is closed **without a reply** — `ServerReject` answers a message, and there is none.
  `-idle-timeout` (default `20s`) bounds every read after the welcome and is re-armed before
  each one, which is the same thing as after every frame and is one call site rather than two.
  Seconds are safe because **`PlayerInput` is the heartbeat**: the client sends one every tick,
  standing still and dead included, so a healthy client is never silent for longer than one tick
  interval and 20s is hundreds of missed frames. Which is also why there is no ping message —
  adding one would put a second heartbeat on a wire that already has one. A handshake window
  longer than the idle window is refused: it would hold only clients that had already proved they
  were talking to the stricter number.

  **`-character-timeout` (default `2m`) is the third, and it is the odd one because the phase it
  bounds is the only one a person is inside.** Between the character list and the choice there is
  somebody reading names, picking colours and typing, and neither of the other two numbers
  describes that: held to the handshake window a player is disconnected for reading their own
  list, and held to the idle window they are disconnected for choosing carefully — a character
  screen sends nothing at all and is not idle. What bounds it at all is that the account's single
  live session is already claimed while it waits, so a connection parked there is one the same
  person cannot reconnect past. It must be at least the handshake window, by the argument above
  read from the other side — a peer that has presented a ticket this server accepted must not be
  held to a stricter number than one that has presented nothing — and it is *expected* to exceed
  the idle window, so there is deliberately no rule tying those two together. A timed-out
  character phase closes without a reply for the reason a timed-out handshake does: the client
  was answered with a list and then said nothing, so there is no message for a `ServerReject` to
  be the answer to.

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
  both in one breath is a race rather than a test: that is the whole of legacy issue 55, worth 1 failure
  in 40 there and 5 in 1,200 when it was re-measured under `-race` at GOMAXPROCS 1 and 2 on a
  loaded machine. Wait for the chunk — and park the player first. Newest-wins means a
  walk that runs on while the assertion waits can carry the player out the far side of the chunk,
  and the chunk the test asked about is then never sent at all, correctly.
- **`select` with two ready cases picks at random, so check cancellation before the select.** Every
  place that races a context against something else — the cache semaphore, the session's outbound
  queue, the clock's sleep — checks `ctx.Err()` first. Without it, an already-cancelled context is
  honoured about half the time, which surfaces later as a flaky test rather than as the bug it is.
- **Anything positional is derived from the world, never stated as a constant.** The spawn
  point was `y = 80` while the terrain ranges 44..84: it buried the player for about one seed in 500
  and floated them 26 blocks above the ground for the default one. It then asked `HeightAt` at the
  origin column, and since #519 it asks the lattice: `world.SpawnAt` is `world.CapitalAt(seed)`'s
  centre pushed `capitalSpawnOffset` along +Z, on the capital's plateau. The second form of the
  mistake is deriving a number from a *drawing* and writing it down as a literal — the castle grew
  from 15 across to 21 in #555, so that offset is computed from `largestHalfFootprint`.
- **Validate flags before narrowing them.** `-tick-rate 1000` must fail at startup, not become a
  silent 255 Hz server, and the error must quote what the operator typed. Clamp-then-validate reads
  as safe and is not.
- **`log/slog` only.** No `fmt.Println`, no `print`. Session logs carry `entity_id` and
  `remote_addr`; a message worth reading twice deserves a field, not string formatting.

## The session is encrypted, and that is not a setting

- **There is no plaintext listener and no flag to ask for one.** A session ticket is a bearer
  credential: whatever can read one off the wire can present it and be that player, because a
  signature proves who issued a ticket and not who is holding it. A switch that
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
  underneath; without the test, legacy PR 150's handshake and idle timeouts could have stopped bounding
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
- **The end-to-end path a new item takes is `docs/ADDING_AN_ITEM.md`**, which starts here
  because an item that is not in this registry does not exist. It names the rule behind each
  step rather than restating it.
- **Items and blocks are different id spaces.** The server-only registry in `game/items.go`
  owns each item's placed block (or none) and its per-item stack limit, currently 64. The
  drop table independently decides what each mined block yields, and what it names is spawned
  as an entity rather than inserted: a completion against a full pack removes the voxel and
  leaves the yield lying where the block was.
- **Inventory state is sent whole, once on join and after each real count change.** The
  session never sends a delta and never drops one on a full outbound queue: unlike a tick
  snapshot, no later frame is guaranteed to supersede it. A pickup is decided on the tick and
  therefore uses the tick's non-blocking seam, which is why it keeps a durable flag and retries
  until one is accepted rather than dropping the frame. The current protocol sends 40 real,
  stable slot-indexed pairs; `(0, 0)` is empty, the first nine are the hotbar, slots 9–35 are
  the pack and slots 36–39 are head, chest, legs and off-hand equipment. Automatic insertions
  fill partial
  same-item stacks before the lowest empty pack slot and never enter equipment; moves split,
  merge or swap under the same per-player lock, and only an explicit compatible move may enter an
  equipment slot. `BlockEditRequest.slot` spends exactly the slot the client named for a placement
  after the server revalidates it.
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
- **The chunk resend bucket is the first rate limit in this repository, and both its numbers are derived.** The
  burst is one view volume, `(2r+1)³`: the most a client can ever honestly need, and work the
  server already agreed to do for it once at join. The refill is
  `TerminalFallSpeed / ChunkSize` chunks a second — the fastest a player can cross chunk
  boundaries, and therefore the fastest the world can legitimately move under a session. It
  bounds *chunk work*, not messages: a request refused before the bucket is consulted costs a
  mutex and a map lookup, and bounding that is the socket-level backpressure policy the gaps
  below still ask for.
- **World chat is the second rate limit, and the first on message text rather than work.** Five
  accepted lines may arrive together and one line of credit returns each second. The bucket is
  keyed by `identity.PlayerID` in `Sim`, not stored on `Player`: a `Player` is recreated for every
  session, so putting it there would let a reconnect manufacture a fresh burst. That is a
  deliberate deviation from the issue's `player.go` field pointer in favour of its behavioural
  requirement that reconnecting does not refill. Invalid text and a body that cannot act spend
  nothing; an accepted line spends one token even when every outbound queue drops it.
- **The map tile bucket is the third, and the first that bounds work a client can ask for
  *twice over*: the request is throttled, and the answer is drawn rather than looked up.** The
  burst is 32 tiles — more than one opening of the map costs at any scale, since a tile is 64×64
  pixels however coarse — and the refill is 8 a second, which is comfortably above a client
  panning across a continent. Both numbers are generous where `resendRefillPerSecond` could not
  be, and the reason is what the work touches rather than how much of it there is: a resend can
  regenerate a chunk under the semaphore every session shares, while a tile is 4096 evaluations
  of a pure function on the session's own goroutine, so a client spending its whole bucket
  delays nobody's terrain but its own. An empty bucket drops the request in silence, which is
  the resend precedent: the contract's one refusal here is `TileMisaligned`, and that names a
  malformed request rather than an impatient one.
- **A map tile is arithmetic, and the mask is applied before the arithmetic rather than after
  it.** `world.SurfaceAt` reads the same `columnAt` the generator does, so a pixel and the chunk
  under it cannot disagree; nothing on the tile path opens a chunk, the cache or the delta
  store, which is why there is no server-side tile cache and why a dug-out hill still draws as a
  hill. A pixel inside a chunk column this character has not been streamed is left at zero in
  both arrays *and its terrain is never computed* — the unexplored is not withheld from the
  frame, it is never a value in the process. A client is not where a secret is kept, and the
  cheapest way to be sure of that is to have nothing to withhold.
- **A party is live simulation state, never a client claim.** `Sim.parties` owns ordered
  membership and leadership, while `byName` exists only to resolve Invite and Kick against the
  stored character name. Invitations expire on the authoritative tick and disappear with the
  session; parties are neither persisted nor resumed on reconnect. The per-viewer snapshot lists
  every *other* member even outside ordinary view, so its leader is either the recipient or one of
  those entries. Appearance uses the same consent boundary to deliver names and levels out of view,
  and membership teardown forgets that description edge so a later party must describe it again.
  The first player to damage a mob taps its registry experience: later attackers may help or land
  the killing blow, but they cannot transfer that award to themselves. Death and disconnection do
  not erase the tap; only discarding/resetting that mob does. If the owner is online and belongs to
  a party when the mob dies, the award is shared with living members within `PartyShareRadius` of
  the creature: equal integer shares, with the remainder kept by the tap owner even when dead. An
  offline owner receives the full award, keyed by account and character rather than by a session
  pointer and queued as an absolute lifetime total. Disconnect/reconnect cycles refresh that total,
  and the player autosave (plus the final shutdown flush) acknowledges it only after a durable write,
  so retrying cannot award it twice and an immediate reconnect sees it before persistence runs.
  Mining and crafting stay personal rewards,
  and every online recipient still goes through `Sim.awardExperienceLocked` so a shared level-up
  invalidates appearances exactly like a solo one.
- **`Registry.Unsubscribe` is the broadcast's `Sim.Leave`.** It takes the lock
  `BroadcastChunk` holds *while it sends*, so once it returns nothing can still be sending to
  that session — and `Serve` calls it **before** `close(out)`, because a send on a closed
  channel is a panic in a goroutine and takes the process with it. Both halves are
  load-bearing: the send must stay inside the registry's lock, and the unsubscribe must stay
  ahead of the close. `TestBroadcastsRunSafelyWhileSessionsArriveAndLeave` fails on either.
- **The session ceiling is admission, not identity.** `-max-players` accepts 100..1000 and
  `Registry.Add` checks and inserts under the same lock, so two accepts cannot both take the last
  slot. A connection past it receives `ServerReject.SERVER_FULL` and is closed; it is never given
  an entity id and never reaches the ticket or character phases. Every connection already in the
  registry counts, including one still silent at the hello and one choosing a character, because
  both consume a socket and a session goroutine.

  **The terrain budget is related and deliberately not equivalent.**
  `-terrain-memory-mib` charges 96 KiB per resident chunk and `world.CacheCapacityFor` keeps no
  more separated working sets than the player ceiling allows, but a budget need not hold every
  admitted player separated at once. Their union exceeding the cache is ordinary LRU degradation;
  one session's own view plus headroom exceeding it is the collapse #666 measured and is refused
  at startup. The default 193 MiB preserves #666's 2056-entry residency.

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
  collecting, so a wearless pile arrives as one insertion. A durable drop never merges, even
  with an identical item at identical wear: one durability pair describes one object, and a
  merge would have to discard one object's condition.
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

### A player can ask for one, and that is the fourth reason a drop exists

`Player.DropItem` in `drop.go` answers a `DropItemRequest`: one slot index on the wire, and
the whole of it decided here. It is the only caller of `spawnPlayerStackDrop` that answers a
*request* rather than something that happened to the world; the wearless world callers use
`spawnDrop`, and both forms reach the same core entity path.

- **The wire carries a slot and nothing else, and both absences are the safety.** No count,
  because a client that could name one would be stating what leaves its own pack; no
  position or direction, because either would let it decide where an item lands. The stack
  is whatever the slot holds, and its landing displacement comes from the movement basis's
  forward vector computed from `p.yaw` and the server's `dropPlacementDistance`.
- **Two phases split by the lock, which is `RemoveStructure`'s shape.** `releaseSlot` takes
  `Sim.mu`, checks liveness, takes the inventory with `TryLock`, reads the slot, empties it
  and returns the state; `DropItem` then spawns outside that lock, because
  `spawnPlayerStackDrop` takes it. Nothing is lost in the gap: the spawn core refuses an
  empty, unregistered or invalid stack, and `releaseSlot` has already read a validated
  inventory slot before it emptied anything.
- **A worn slot reaches the ground as the exact authoritative object.** The fixed
  `ItemDropState` remains unchanged; V11 appends sparse `drop_durabilities` entries keyed by
  entity id, so block yields, loot rolls and structure bundles stay wearless and pay no
  per-drop wire cost. The shared spawn core carries the inventory stack through the entity, and
  pickup inserts that exact durable stack into one empty slot instead of rebuilding it with
  `stackOf`. A blade let go of at 12 durability therefore comes back at 12, never at the
  registry maximum. Durable drops never merge because one wear pair describes one object.
- **A player-authored drop is collision-placed once, before it appears.** Its horizontal
  displacement goes through the existing axis-by-axis `moveAndCollide`, so a wall stops it on
  this side and may shorten or slide the placement along an unblocked axis. In open terrain it
  begins outside the player's pickup radius on every configured tick rate, while the unchanged
  `dropPickupDelayTicks` remains the only answer to when any drop may be collected. World-produced
  drops enter the same creation core with no displacement and stay exactly where the world put
  them. Because nothing carries horizontal velocity into `mergeDropsLocked`, a mixed player/world
  merge cannot cancel a throw or drag a world pile after either has appeared.
- **`RefusedAction.DropItem` exists on the wire and nothing sends it**, exactly like
  `MineBlock`, `EditBlock`, `Craft` and `Repair`. It has a member where a removal deliberately
  does not, and the contrast is the rule: every question a refused drop could answer is about
  the asking player's own pack, which they already hold a complete `InventoryState` of.

## Projectiles, and the transient entity that moves fastest

Arrows and energy orbs live in `Sim.projectiles`, take identities from the same source
as players, drops and mobs, and are advanced after swings but before mobs. They are an
authoritative answer rather than a client claim: the server owns their origin, velocity,
gravity, collision, lifetime, target and effect, while the client receives only the
complete visible set in `EntitySnapshot.projectiles`.

- **One flight path serves both weapons.** A kind selects only the differences: arrows
  accelerate under gravity, damage mobs and stick in terrain; energy orbs keep a constant
  velocity, damage mobs, heal other living players and disappear on terrain. The bow and
  sceptre only choose a registry row and call `spawnProjectileLocked`.
- **The sweep is shorter than half a block.** Each tick is divided so no projectile move
  exceeds `ProjectileMaxStep`; every travelled segment is slab-tested against living body
  boxes. The nearest crossing wins, and terrain shortens the tested segment through
  `moveAndCollide`, so neither a one-block wall nor a vargr can be tunnelled through.
- **The owner is never a target.** Spawning advances the projectile's small body outside
  the shooter's body before its first step. Arrows ignore every player; orbs may heal a
  living player other than their owner. A retained stable owner reference lasts only as
  long as the projectile, allowing a shot already in flight to establish the ordinary mob
  tap after its session leaves; threat still belongs only to a live session.
- **A non-resident chunk is a hold, never air.** No gravity is accumulated and no position
  changes until the chunk is resident. Lifetimes continue to count authoritative ticks,
  as drop lifetimes do.
- **A stuck arrow is still a projectile.** It carries zero velocity in snapshots for three
  seconds and then leaves the complete vector. An orb leaves on contact, and either kind
  leaves silently when its flight lifetime expires.
- **Nothing is persisted.** A restart loses every arrow and orb, including arrows resting
  in terrain. A projectile is a moment in the live simulation, not a change to the world;
  keeping one would also require deciding how flight and a three-second stuck lifetime age
  while the server is off.

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
  fuel, does not burn down and cannot be put out. It cooks raw meat immediately through
  the ordinary crafting transaction, lights nothing in the mesher and hurts nobody who
  stands in it; fuel, burn time and lit state remain absent.
- **Ownership decides removal and respawn, and nothing else.** Any player may walk into any
  tent, and the crafting issue reads this registry for a nearby forge without consulting the
  owner at all.
- **The owner is an `identity.PlayerID`, and the wire carries an entity id.** An entity id
  names one session; a camp outlives every session its owner will ever open, so keyed by the
  entity id a tent stopped being its owner's the moment they reconnected — they came back
  with a new number, respawned at the join spawn, and could not take down their own tent.
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
  come back standing in the air where one used to be. With no tent the answer is no longer
  the join spawn directly — see "Waking up with no tent" below — but the registry is still
  the first question asked and a standing tent still outranks everything.
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

### The village forge belongs to nobody

`internal/game/station.go`, and it needed almost no new rule: a **world-owned station** is an
ordinary structure whose owner is the zero `identity.PlayerID`, which `Join` refuses — so
removal already refuses one, the tent lookup already fails to match one, and crafting never
consulted the owner. Nothing on the wire moved: `owner_entity_id` is the 0 V5 already
reserved, and a client compares it against its own entity id, which is never 0.

**What that widened is the *meaning* of 0, and `schemas/player.fbs` has not caught up.** It
says "`0` while the owner is offline" and, in as many words, "so `0` does not mean unowned" —
which was true until a settlement's forge and fire had no owner at all. Amending it is a
`schemas/**` change: flatc copies those comments into `server/gen/` and into the client's
`*_generated.rs`, so it costs a regeneration, all three CI jobs and the two client comments
that restate the same sentence. It is owed, it is a comment rather than a decoder rule, and
every decoder is already correct — see #456's pull request.

- **Nothing about one is written down.** Its position is a settlement anchor and its id is
  `world.HashLattice(seed + offset, x, z)` with the high bit set — both pure functions of the
  seed, so a restart re-derives the same forge with the same id. The bit keeps a derived id
  out of `session.Registry.NextID`'s range, which matters because a client that meets two
  entities sharing an id closes the connection; `persist.StructureStore.Save` drops an
  ownerless record, so `structures.bin` still holds only what players did.
- **They are created by being looked at** — `Streamer.ReportEntering`, once per chunk that
  newly enters a view — and the derived id is what makes that idempotent.
- **Nothing brings one down**, the collapse in `breakMined` included, and the reason is
  duplication: the seed puts it back the next time somebody looks, so a collapse that dropped
  a forge item would hand out one crafted station per break. Digging under a village forge
  leaves it standing on nothing.

### Two wards, and why one belongs to nobody

A runestone wards its 3x3 column square for the player who raised it; that owner may still dig,
build and remove structures there. A settlement wards every column touched by its plateau disc
for the zero `identity.PlayerID`. Join can never assign that identity to a player, so the ordinary
owner exemption matches nobody and the village belongs to the world rather than to whoever plants
a runestone beside it. `wardOf` checks and caches the settlement answer first, which also makes a
settlement win every overlap; storm regeneration must therefore keep on its returned boolean,
never on whether the returned owner is non-zero.

**The client is shown that answer; it never derives it.** `WardsNearby` is a complete replacement
inside the streamer's horizontal radius, sorted `(CZ, CX)`, and an empty vector clears the last
claim the client drew. A session sends one after its initial `MoveTo` has materialised the
settlement structures and before releasing its first snapshot, again after a column crossing, and
again when `Sim.WardsRevision` says the runestone map was rebuilt. Each snapshot crosses a
one-entry, newest-wins session handoff beside the authoritative column it was built for; the worker
holds it until `MoveTo` has completed for that same centre, so asynchronous streaming cannot release
new-position state under an old ward list. The comparison and ordered send stay off the tick
goroutine; the tick remains non-blocking and is still the heartbeat that makes a stationary player
learn a stone was raised or removed.

### The residents — the third entity class

`internal/game/resident.go`. A **resident** is a person a settlement drawing put in a slot:
`Sim.residents`, keyed by identity, derived from the seed and written down nowhere. It shares
station.go's whole provenance — an anchor for a position, `world.HashLattice(seed + offset, x, z)`
for an id, `Streamer.ReportEntering` for a birthday — and `materialiseSettlementsLocked` now
asks both questions of one anchor, because one pass over `world.SettlementsNear` per chunk is
the cost either of them would pay alone.

- **A resident is not in `Sim.mobs`, and that is the entire safety argument.**
  `swingTargetLocked` scans `mobs`; a projectile scans `mobs`; the director counts, spawns and
  despawns `mobs`; `makeCorpseLocked` takes a `*mob`. Residents are invulnerable, unlootable,
  un-aggroable and never despawned **by construction rather than by a branch in each of those**
  — and a branch in each of them is what a fifth reader would forget to add. Do not "fix" a
  future feature by teaching combat about residents; keep them out of the collection.
- **`MobKind.Villager` has no `mobRegistry` row, deliberately.** That table is health, damage,
  reach, telegraph timings, aggro radius, rank, nocturnality and a loot roll — every number in
  it is about hunting or being hunted, so a resident's row would be zeroes and a lie.
  `species_test.go` exempts the one member by name and still fails for any other.
- **A resident stands *in* the slot; a station rests on the block *under* it.** A
  `world.PlacedAnchor` names a building's floor, which is air — so a structure takes the voxel
  below and a person's feet sit at the bottom of that cell. It is the one place the two
  materialisers differ, and it is why they file a resident under the slot's own chunk.
- **The wire has no third vector and needs none.** A resident is appended to the `MobState`
  stream by `mobSnapshotsLocked` alongside the mobs and the corpses, always `Villager` / `Idle`
  / full health / no target; the role travels once in `ResidentAppearance`, on the same
  once-per-view bookkeeping (`Player.described`) a `PlayerAppearance` uses. **The sweep of that
  map therefore runs after the resident pass**, not after the players — stamped later than the
  sweep, an entry would be swept every tick and the description re-sent every tick.
- **The one behaviour is a yaw.** `advanceResidentsLocked` turns whoever has a live player
  inside `ResidentNoticeRadius` toward the nearest of them at `ResidentTurnRate`, and turns
  everyone else back toward their anchor's bearing at the same rate. Nothing walks, paths,
  schedules or speaks. The pass iterates the map rather than a sorted slice because it reads
  only players and writes only its own field — there is no order for a result to depend on.
- **Every `NpcInteractRequest` is refused `ActionRefused{Interact, NotAVendor}`**, a vendor
  role included. An unknown id, an id that is not a resident, one out of `EditReach` and one
  who keeps no stall all produce the same frame, so a client learns nothing by probing. This is
  the fail-closed default rather than a stand-in: #459 is what teaches the server what a vendor
  role opens, and `vendorRole` is the one place the trades are named so that issue changes an
  outcome rather than rediscovering a list.
- **The router's case is what closed a live edge, and it is worth knowing which one.**
  `NpcInteractRequest` has decoded at the protocol boundary since V25; while the router had no
  case for it, such a frame fell through the default and closed the session as **malformed** —
  a V25 client hung up on by a server that understood every byte it sent. A refusal is an
  answer and a disconnect is not, which is why `npc_test.go` sends two requests rather than
  one: the second can only be answered by a session that survived the first.

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
- **Station proximity is a scan of the structure registry, never of voxels.** Forges and
  campfires each have an explicit five-block crafting radius; an unknown station kind has
  no radius and fails closed rather than inheriting one. A handful of
  entries at craft frequency, on the same explicit trade the drops and the mobs record.
  Ownership is deliberately not consulted: a forge is a place, not a possession, and the owner
  field exists for removal and respawn. Cooking therefore works at another player's fire
  on exactly the same terms forging works at another player's forge.
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

## What a death costs, and the one question that decides it

`applyDeathPenaltyLocked` in `internal/game/inventory.go` is the whole death penalty. Blades,
tools and armour do not wear from ordinary use; bows are the deliberate exception and spend
one point per launch in the attack path.

- **It reaches what the player has *on them*, and `carriedOnPerson` is the one answer to
  which slots those are.** The pack behind them is untouched, so a spare blade stowed away
  outlives the death that spent the one in hand. The answer is the leading hotbar plus the
  trailing equipment slots; a slot's own index is the whole of it and the server needs nothing
  from the client to compute it.
- **A function rather than the two range comparisons inline in the loop**, for the reason
  `meleeDamage` is a registry field rather than a list of item ids in the combat path. Worn
  equipment joined this rule by widening that one answer; a second comparison elsewhere would
  be a second answer that can disagree, and the disagreement is a death that costs two different
  things.
- **"What is on the player" was never the selected slot, and an earlier draft of #199 read
  it that way.** There is no selection in `internal/game` and none on the wire: a slot
  reaches this server only inside a request that names one. That draft concluded
  `PlayerInput` would have to carry it. It does not — the range is a server constant the
  server already holds and already sends in the welcome, so the change was a condition
  inside a loop rather than a field on a message.
- **The arithmetic did not move.** `wornByDeath` is still `floor(current * 4/5)`, still
  integer for the reason `deathDurabilityKept` records, still one pass under one lock so no
  snapshot can show half a player penalised. What changed is which slots the loop reaches.
- **An empty hotbar is a normal death**, not a special case: the penalty finds nothing on
  the player to spend and reports no change, which is the same answer a pack of worn-out
  blades already gave. `chargeDeathPenaltyLocked` still marks it charged either way — see
  the one-shot's own note, where "it ran" and "something changed" are deliberately
  different questions.

### Who is told about a death

`PlayerVitals` is per-recipient by contract, which is why nothing could tell a client that
the player beside it had been killed — the information was not unused, it was never sent.
Protocol V10 adds `EntitySnapshot.dead_players`, and `Sim.Step` fills it from `Player.alive()`
**in the same pass that fills the entity vector**: the contract says every id there names a
player in the same snapshot's entities, and a second walk over the player list would be a
second visibility decision to keep in step with the first.

- **The viewer's own id goes in it like everybody else's.** A session is inside its own view,
  so the body it watches go down and the bodies beside it are stated the same way. Its vitals
  still carry the health and the countdown, which genuinely are the recipient's alone.
- **It is a state, not an event.** A session that connects after a death is told by its first
  snapshot, with nothing replayed and nothing to replay.
- **It costs nothing on the ticks nobody is dead**, because the encoder writes no field at
  all for an empty vector and a vtable is trimmed of its trailing empty slots.
  `TestWhatADeathCostsOnTheWire` asserts that as *byte* equality against the frame with no
  such field, and measures what the rejected table-per-player shape would have cost instead.
- **What draws any of this is a separate change.** This is the wire and the server half; the
  client still tips only the viewer's own body, and `client/AGENTS.md` still records that gap
  until the half that closes it lands.

## Waking up with no tent, and the wall the offset does not clear

`respawnPositionLocked` in `internal/game/vitals.go` resolves three tiers in order, and #460
inserted the middle one: **the player's tent, else the nearest settlement to where they fell,
else the join spawn — which since #519 is the capital's gate square.**

**The third tier stopped being a consolation prize and nothing in it changed to make that
true.** It reads `Player.spawn`, which is `session.Config.Spawn`, which is `world.SpawnAt` —
the world's origin column until #519 and the capital's gate square since.

- **`world.NearestSettlement` is a pure lattice query, which is the only reason this can run
  on the tick.** It is a handful of hashes over the seed and the death column — no chunk is
  read, nothing is generated, and a settlement that has never been visited answers as readily
  as one somebody lives in. It searches three lattice cells of blocks out from the column and
  answers false rather than spiralling when nothing that far out holds anything, which is the
  case the third tier exists for.
- **`Sim.worldSeed` is retained for it**, and that is a small widening of what the seed was
  for: it used to feed the spawn director's and the loot table's generators and nothing else.
  It still generates nothing. The voxels the lattice names are read through the terrain seam
  like every other voxel in this package.
- **The body is put down at the settlement's plateau plus `world.SpawnClearance`**, which is
  the join spawn's own rule rather than a second one — a settlement flattens its ground, so
  the plateau *is* the surface and there is no height field to sample — and then pushed
  `respawnSettlementOffset` (3) blocks out along the bearing they died on. The bearing is what
  the offset is for: two deaths from two directions land on two columns instead of stacking on
  one voxel, and pointing outward faces the player at the walk they are about to make.
- **The chosen column is then verified through the same non-generating read a tent placement
  uses** — `footprintFitsLocked`'s two questions asked over a body: the plateau under the feet
  resident and not air, every voxel the body occupies resident and air. A tick may not wait for
  a chunk, and a body that starts inside a solid cannot be moved by `moveAndCollide` at all. On
  either refusal the tier falls through to the join spawn and says so at `Debug`.
- **A capital blocks three of its four cardinal bearings, and that is known rather than
  discovered.** The keep is the one drawing that is not hollow three blocks from its middle:
  its inner tower's wall stands exactly there on ±x and −z, and the fourth cardinal is the
  tower's doorway. So a player who dies due east of a capital wakes at the join spawn — which
  since #519 is that same capital's gate square, so it costs them nothing at all. Villages put
  a hollow hall or smithy at their centre and are clear on every
  bearing, and they are what this tier exists for. Widening the offset until it cleared the
  keep would move every respawn in the world to repair the one case where falling through is
  cheapest. `TestTheKeepStandsWhereThisRespawnRuleSaysItDoes` reads the drawings, so the
  paragraph cannot quietly stop being true.

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
  putting them there would invent a knob and then have to validate it. `session.Welcome` reads
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

## The Fimbulvetr, and why a real week is not a tick count

- **The persisted deadline is the state; the phase is derived.** Ten-second polls derive
  the three warnings, the five-minute blizzard and healing from `next_storm_unix`. A
  restart resumes Raging, while a fully missed storm first gives one minute of warning.
  `-storm-period` defaults to 168 hours, zero disables it, and `-next-storm` is a one-run
  RFC3339 override. Deadline changes are saved immediately.
- **Warnings are transitions; joining is a state read.** Broadcasts happen at ten
  minutes, one minute, ten seconds, Raging and Passed; late joiners receive the live
  Approaching or Raging phase after `ServerWelcome`.
- **Only listing runs outside the tick.** `Cache.RegenerationChunks` unions and sorts
  stored, in-memory and resident chunks on the worker. The tick then generates and
  durably deletes at most `RegenerateChunksPerTick`; Passed and the next deadline wait
  for the queue and changed camp to become durable, so a crash cannot skip healing.
- **A ward keeps a column, not an object.** `wardOf` preserves every vertical chunk in
  its column. Healing removes unwarded player state; derived settlements remain pristine.

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
- **The headroom is asked for by block rather than inferred from the scan, and since
  worldgen 5 that question comes back no.** `Solid` used to be `!resident || block != Air`,
  so "not solid" was exactly "resident air" and a scan that stopped at the first solid
  voxel had already left air in every cell above it. **That equivalence belonged to the
  palette, not to the rule**, and water ended it: the classification now lives in
  `world.Solid`, and the scan walks straight down through a lake and hands back the bed
  with the lake still on top of it. The criterion is two blocks of air, so the director
  asks for two blocks of air, in the shape `footprintFitsLocked` already asks it for a
  structure's footprint. `TestNothingSpawnsInsideAFluid` was written against a synthetic
  block before there was a real one; it now scripts `world.Water`.
- **And the floor is asked about by name, because ice is the case the headroom rule cannot
  reach.** A lid of ice over a lake is *solid*, so the downward scan stops on it, the two
  cells above it are honest air, and every other check the director makes says yes.
  `standableFloor` is the one thing that refuses it — a blacklist rather than a whitelist,
  deliberately, because an unclassified block is ordinary ground and refusing to spawn on
  it would silently empty a region every time the palette grew.
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
- **How long a body takes to go down is deliberately not a column.** `MobDeathDuration` is
  in `constants.go` because it holds for every species alike, which is this section's own
  test for what a row is: a column that would carry the same value in every row is not a
  species difference. What *does* differ is the pose, and a pose is drawn rather than
  simulated — so no number describing one exists on this side at all.
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

## The hostile ledger, and why armour earns attention at the blow

Every hostile `mob` owns a transient threat ledger keyed by player entity id. It is guarded by
`Sim.mu`, is never persisted, and never crosses the wire; `MobState.target_entity_id` is the one
projection clients receive. Passive species stay on `stepPassive` and never allocate a ledger.

- **Damage, healing and blocking have one seam each.** `creditDamageThreatLocked` adds the actual
  mob health removed multiplied by `1 + worn.threat / ThreatScale`;
  `creditHealThreatLocked` gives a healer half the health actually restored on every mob hunting
  the healed player; `creditBlockThreatLocked` adds `ShieldTauntThreat` for the blocker whose
  shield absorbed the mob's blow. The future projectile, sceptre and shield paths reuse those
  seams rather than restating ledger arithmetic.
- **Worn armour multiplies generated threat; it does not extend awareness.** A creature still
  considers only living, unprotected players inside its species' `aggroRange`. Once somebody in
  that set has positive threat, the highest ledger value is the primary choice. The old
  `distance / weight` comparison remains only the zero-ledger fallback, so an untouched creature
  can still acquire its first target without inventing threat.
- **A valid target is tenacious, not permanent.** Another candidate must be strictly above the
  current entry multiplied by `ThreatSwitchRatio` (1.1) to take the hunt; equality never switches.
  Death, protection, disconnection and leaving `aggroRange` invalidate the current target at once,
  independently of the ratio. `startBossEncounterLocked` sees the first player actually committed
  to, so the frozen boss roster does not move with later threat.
- **Memory spends simulation time.** Outside Chase, Windup and Recovery, every entry loses
  `ThreatDecayPerSecond` after a complete second of authoritative ticks. Ten consecutive seconds
  without a target (`ThreatForgetSeconds`) clear the ledger whole. Player death and `Sim.Leave`
  remove that entity from every ledger immediately; mob death clears its own ledger.

## What the dead leave behind, and the lock the drop had to get past

`internal/game/loot.go` owns the roll and the spawn; the table itself is a `loot` column of
`mobRegistry`, because what a creature is worth killing belongs beside what it costs to
kill. A draugr leaves 1..2 bones, a vargr leaves exactly one pelt, and two pelts are a
`RecipeIDLeatherPatch` away from a field repair.

- **A kill, and only a kill.** `makeCorpseLocked` is the single caller of `rollLootLocked`,
  and only a creature that was *killed* ever reaches it: the director's two removals — dawn,
  and "outside every streamed cube for five seconds" — `delete` from `Sim.mobs` without going
  near it, so a mob that despawns leaves nothing. Loot is the reward for the kill, and a world
  that paid it out for having existed would be a world where waiting is a strategy.
- **The blow is the whole of the death, and that is #441.** `damageMobLocked` takes the
  creature out of `Sim.mobs`, calls `makeCorpseLocked` and rolls the container, all inside the
  call that empties its health. The rest of the tick — `advanceMobsLocked`, the director, the
  snapshot projection, `offerLootLocked` — therefore already sees a corpse, so the first
  snapshot that draws a body draws it as `MobAction.Corpse` and lists it in that recipient's
  `accessible_loot_corpses` if they own it and are in reach. Pressing F while the body is
  still visibly going over opens the loot window, because the fall is the client's and the
  server has nothing to wait for.
- **There was a two-and-a-half-second wait here and it is gone. What was given up is worth
  naming.** From #176 to #441 a kill put the creature into `vnet.MobActionDying` with a
  countdown of `mobDeathTicks`, left it in `Sim.mobs` and in every snapshot for
  `MobDeathDuration`, and `advanceMobsLocked` reaped it and rolled the loot when the countdown
  ran out. The argument for it was sound and its premise was not: when an item begins to exist
  is a gameplay outcome, so a client that draws no animation must not get its loot sooner —
  true, and irrelevant, because the wait was never deciding *whether* the drop was earned.
  What it decided was how long a player who had already earned it had to stand there. Two
  things went with it. **A body killed on a ledge no longer slides off before the loot is
  placed**: the corpse is at the position the blow landed on, which is one tick's movement
  from where it would have come to rest. And **the server never emits `MobAction.Dying`**;
  the value stays in the contract, because a wire enumeration is not narrowed because one
  server stopped sending one of its members, and `TestNoSnapshotEverCarriesDying` is what
  says the server has.
- **Three guards went with it, and none of them was deleted for being wrong.** A dying
  creature had to be skipped by `swingTargetLocked`, or a corpse would absorb every swing
  aimed past it — being immune is not the same as not being chosen. `removeSpentMobsLocked`
  had to skip it outright, because the dawn rule matches a nocturnal creature that hunts
  nobody, which is exactly what a dying draugr was on every tick of its death. Both are now
  structural: a killed creature is not in `Sim.mobs`, so neither loop can reach one. What
  remains is the `health == 0` check in each, kept as an invariant rather than as a live case,
  and the `target` still cleared at the blow — the projectile pass captures its mob slice
  *before* the first arrow moves, so a creature the first arrow killed is still in that slice
  when the second one looks, and `firstProjectileTargetLocked`'s zero-health guard is the one
  that genuinely fires.
- **The roll happens at the blow, and the position it uses is the blow's.** `Sim.loot` is
  advanced only inside the locked tick either way, so determinism is untouched — and the roll
  is still exactly once per corpse, at creation, which is what makes opening a container a
  projection rather than a draw.
- **The lock is the whole design, and it is why `Step` is two functions.** The reap runs
  inside the tick, under `Sim.mu`; `spawnDrop` takes `Sim.mu` itself, because its other
  callers are session goroutines. Spawning there therefore deadlocks the server on the first
  kill anybody makes. So the tick **collects** and `Step` **spawns**: `advanceMobsLocked`
  returns `[]lootDrop` beside its surviving mobs, `stepWorld` carries them out under the
  lock, and `Step` — which holds nothing — is one line: `s.spawnLoot(s.stepWorld(tick))`.
  That is `collapseStructuresAt` / `dropCollapsed` from `edit.go`, written for a caller that
  is already inside the critical section.
- **The consequence is one tick, and it is worth knowing rather than discovering.** Loot
  spawns after the *reaping* tick has already encoded its snapshots, so a body that goes on
  tick N is a drop on tick N+1 — the same tick a mined block's yield waits, and pinned by
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
- **Emptying a corpse is one walk, one revision and two answers.** `TakeLoot` moves one
  named entry and is all-or-nothing; `TakeAllLoot` shares its preconditions through
  `openContainerLocked` — reachable, owned, open on this session, at the revision the
  request names — and differs only in the loop. It walks the entries in `entryID` order
  and **skips** what does not fit rather than stopping at it, so a bone behind a blade the
  pack has no empty slot for still comes home. One `revision++` for the whole walk if
  anything moved, none if nothing did. A bare container is `removeCorpseLocked` and the
  existing `LootClosed`; a remainder is `lootDirty` and a `TakeLoot`/`InventoryFull`
  refusal, which is the one place in this file where a *partial success* is reported as
  both things it is — the entries that moved are committed, and the refusal is what says
  the rest did not. The whole walk is inside the one `inventory.mu.TryLock` window, which
  is what makes "what fits" a question about a pack no other request is halfway through
  changing. On a boss corpse the walk is over `containerFor`'s answer and can therefore
  only ever be the requester's own container.
- **Take-all is its own client-ordering stream**, beside open's and take's, for the reason
  those two are separate: pressing F is not clicking an entry, and a tick number spent on
  one must not silence the other in the frame they share.
- **Arrows are the only bone sink, and `TestOnlyArrowsConsumeBones` makes that a claim.**
  One bone and one log make four arrows; no other recipe, durability or combat rule consumes
  bone. The alternative was a creature that left a resource with no present use.

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
  a number a human maintains, not a hash of the function. Found by the review on legacy PR 65.
- **Writes are atomic: temporary file, flush, rename, in that order.** The temporary file is
  created in the destination directory, because rename is only atomic within one filesystem.
  Writing in place would leave a truncated file that parses perfectly as a *shorter* edit
  list, which is to say as a shelter with some of its walls back. A crash between the two
  leaves an inert temporary file that no reader opens and the next `OpenStore` sweeps.
- **The sweep removes temporaries this code can prove it wrote, and nothing else.**
  `world.SweepTemporaries` takes the destination names the caller writes and removes only
  `<destination>.tmp<digits>` — the shape `os.CreateTemp` gives a `WriteAtomic` temporary —
  matching each destination with `filepath.Match`. It used to glob `*.tmp*` over whatever
  directory it was handed, which was tolerable while the only caller was the world store and
  stopped being tolerable when `internal/ticket` called it on `-auth-dir`: a path an operator
  typed, out of which the account service then deleted files nothing here had written (#137).
  **Naming nothing sweeps nothing**, because the safe direction to fail in is removing too
  little; a destination of `*` would restore the bug exactly. The division that follows is
  worth keeping: a directory the operator named (`-world-dir`, `-auth-dir`) is swept by the
  literal names of the files this code puts in it, and a directory this code creates
  (`chunks/`, `players/`, `accounts/`, `servers/`) may name the shape of its own records
  instead. It does **not** decline to sweep the operator's directory — the temporary a crash
  leaves beside `signing-key.bin` is a second copy of an Ed25519 seed, and leaving that on
  disk is worse than tidying it.
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
- **The one exception is the character index, and #103 is why.** A nil `persist.Store` still keeps
  nothing, but `session.NewIdentities` substitutes `persist.NewMemoryStore` for one: an index with
  no directory under it. A name being unique within a world and an account holding at most so many
  characters are rules about the *world*, not about the disk, and an ephemeral world owes its
  players both. What it costs them is still only the life.

## Characters, and why the account is not the key

- **Choosing one is on the wire, and creating one is the only way a character comes into
  existence.** `Store.Create` is the single path in, and the `ResolveOrCreate` beside it — which
  answered "which character does this account play" and made one from the hello's display name
  when the account had none — is gone rather than left for somebody to wire back up. It was the
  stand-in for a choice that had no message to arrive in; a second, name-driven way to create is
  a way past the appearance a creation is required to carry.
- **What a character looks like is written down with its name, and read from there for ever
  after.** `persist.Character` carries an `Appearance` in the index, because a character list is
  a map hit and an index holding only names would have to read every one of an account's records
  to draw a screen; `persist.Record` carries it on disk, beside the id and the owner rather than
  beside the life, because it is chosen once and never written again. `Store.Save` fills all four
  from the index and ignores what a caller put in them — a save that could restate an appearance
  would be a way to come back from a session wearing somebody else's face.
- **`persist.StoreVersion` is 4 because of it, and there is no migration.** A v3 record cannot say
  what its character looks like, and the only value this build could invent is the placeholder the
  contract reserves for an appearance that has not *arrived*, which is a different claim entirely.
  A directory of them is set aside whole on the first start under this format, as
  `players.pre-v4.<timestamp>/` — the name says which format the build that moved it speaks,
  because an operator who finds two of these needs to know which is which.
- **The store writes an appearance down and judges none of it**, which is its rule about
  descriptions rather than an oversight: it judges what a *key* may be, and
  `schemas/common.fbs` puts the appearance's invariants on whatever accepts the
  `CreateCharacterRequest` they arrived in. That gate is `session.Identities.Create`, and it
  refuses **before anything is stored** — a character persisted with a hair model no member names
  is one every client is afterwards required to refuse, and the person who cannot get in is not
  the person who sent it. The startup scan is the one place that judges a stored one, and for a
  reason that is about the index rather than about the value: every character in it is a row in a
  `ServerCharacterList`, so one unusable record would cost that account the whole list.
- **A record is keyed by a server-minted `persist.CharacterID` and the account is a field in it.**

  Keyed by the account there is one character per world, which is the thing #103 removed; keyed by
  the character, "every character this account owns here" is a filter over an index rather than a
  lookup. The id is the wire's `ulong` (`CharacterSummary.character_id`) rather than a digest,
  because a client is shown it and hands it back; it is never zero, which the contract reserves.
- **The name index is built once, at startup, from the records themselves.** There is no second
  file to keep in step with the first: `OpenStore` reads the directory once and keeps id, owner
  and name in memory, so a join is a map hit. A record the index cannot use is *set aside* rather
  than skipped — skipped, its id stays free for a later mint to write over.
- **Name uniqueness is one critical section, and that is the acceptance criterion rather than an
  implementation detail.** `Store.Create` checks the name, checks the allowance, mints the id,
  writes the record and updates the index under one lock — the shape `Sim.PlaceStructure` uses.
  Split into a check and a later insert, two creations racing for one name both see it free and
  both take it. **A `-race` run does not catch that**, because there is no data race to catch:
  `TestTwoCreationsRacingForOneNameLeaveOneWinner` asserts the outcome instead.
- **The three refusals map one to one onto reject reasons** — `ErrNameTaken`,
  `ErrNameRefused` and `ErrCharacterLimit` become `CHARACTER_NAME_TAKEN`,
  `CHARACTER_NAME_REFUSED` and `CHARACTER_LIMIT_REACHED` in `session.refuseCharacter`, by a
  switch and never by parsing prose. A refused *name* says which of the two it was, unlike a
  refused ticket: the player picked it, and the contract says the client may tell them apart.
- **`players/` from a build before characters is set aside whole on the first start, never
  migrated.** `players.pre-accounts.<timestamp>/`, the doctrine `Store.Quarantine` keeps one level
  up, timestamp included so a second set-aside cannot destroy the first. A v2 record names a player
  by the hash of their account and cannot say which character it was, so there is nothing to
  convert — and the code to guess would outlive by years the single event it serves. A directory
  written by a *newer* build is the one case that refuses to start instead.


## Where a character has been, and what "explored" is allowed to mean

- **Explored means streamed, and that is the only definition this server can enforce.** A column
  enters the ledger when `View.MarkLoaded` records a chunk in it as *delivered* — not when it is
  scheduled, which is the same distinction streaming already draws for its own reason: a chunk that
  never arrived is terrain the client does not have, and calling it explored would draw a map of
  places nobody saw. "Looked at" and "walked through" are facts about a camera and a pair of feet,
  both of which live on the client, and a server that asked would be taking a claim from the one
  party that benefits from lying about it. It also costs one map insert on a path that was already
  doing one.
- **The unit is the chunk column, and a column has no height.** A character who has been somewhere
  has been there at every y, so the whole vertical stack of a view cube adds one entry rather than
  seven. `world.Column` is that type — `{CX, CZ}`, chunk units, deliberately not a `Coord` with a
  field every caller has to remember to zero — and `schemas/world.fbs` names `MapColumn`'s fields
  the same way.
- **It is a second file per character, for the reason `structures.bin` was a second file.**
  `persist.Record`'s layout is fixed-width-then-one-variable-length-name with an exact size check
  at the read, which is what makes a truncated record refusable; it has no extensible area, and
  giving it one to hold a list that reaches sixty-five thousand entries would change the shape of
  the thing every player's life is stored in. So `exploration/<character-id-hex>.bin` carries its
  own `persist.ExplorationVersion`, and a change to one format never bumps the other.
- **Two caps with one name, and they are not the same number.** `persist.MaxExploredColumns` is
  65,536 — the ledger's own bound, 512 KiB per character, enforced at the reveal *and* at the write
  so this build can never write a file it would refuse to read. `protocol.MaxExploredColumns` is
  4096 — the most columns one `MapExplored` frame may carry. Paging exists because of the gap.
  A character at the cap keeps playing; what stops is the map growing, and the session says so once
  at `Warn` rather than once per chunk crossing.
- **An unreadable ledger is kept and never written over**, which is `Store.Quarantine`'s doctrine
  one file along: the bytes are the only evidence of what went wrong, and the session that could
  not read them is about to write to that exact path. Both go through the same `setAside`, so the
  timestamp that keeps a second quarantine from destroying the first is written down once.
- **The set lives on the session, not in `game.Life`.** `game` never imports `persist`, and the
  tick loop has no business learning about map state; the code that knows a chunk reached a client
  is in `session`. `session.Exploration` is that set, it carries its own mutex because the
  streaming goroutine reveals into it while the session goroutine reads it, and `MarkLoaded`
  deliberately calls out to it **with the view's lock released** — two correct types locked in two
  orders is how a deadlock gets built.
- **It is saved exactly where a record is saved, and nowhere else.** `Identities.write` writes both,
  so the teardown and the autosave are the whole schedule and there is no second decision to get
  wrong about when a file may be touched. An unchanged ledger costs no write, which is what keeps
  the autosave cheap for the many connected players standing still, and the dirty flag is cleared
  *before* the write for `world.Cache.takeDirty`'s reason — the worst case is writing the same bytes
  twice, where the other order loses a column entirely.
- **An unreadable ledger never refuses the connection, and that asymmetry with a record is the
  point.** An unreadable *life* is refused when it cannot be set aside, because the session that
  followed would write its own life over the only evidence. An unreadable *map* costs the fog and
  nothing else — no items, no position, no progress — so the character plays with a blank one. The
  evidence is protected the other way: the file is quarantined if it can be, and if it cannot be
  moved, or could not be *reached* in the first place, the ledger is **sealed** and this session
  writes nothing at all. Sealing has to be said out loud, unlike `restoreStructures` leaving a camp
  file alone, because a ledger *is* rewritten by the session that could not read it.
- **The ordering on the wire is the ordering of the facts.** The whole stored ledger goes out in
  pages immediately after `ServerWelcome` and before the streamer exists; every later view diff
  that revealed columns sends one batch. A client's ledger is the union of every `MapExplored` it
  has received, there is no revision number and no end marker, and an empty page is a message the
  contract forbids — a client reading one as "you have explored nothing" would erase its own map.

## The marks a character puts on the map, and where the world ends

- **A mark is not gameplay, and that is why it lives where it does.** Nothing in `game` reads one,
  no outcome depends on one, and a character with sixty-four of them plays exactly like a character
  with none. So marks live in `persist` and `session` only, which is where the exploration ledger
  landed and for the same reason: state with a file and a wire message and no simulation half.
- **A third file per character, for the reason `exploration/` was a second one.** `persist.Record`
  has no extensible area, and a list of sixty-four entries each carrying a hundred and twenty bytes
  of somebody's own text is not what to give it one for. `markers/<character-id-hex>.bin` carries
  its own `persist.MarkersVersion`, and a change to one format never bumps the others:

  ```
  magic[4] version:u32 next_id:u64 count:u32
  count × (marker_id:u64, x:i32, z:i32, kind:u8, note_len:u8, note[120])
  crc32:u32
  ```

- **The note is zero-padded to its maximum with an explicit length, so the decoder's only variable
  quantity is still the count.** That is the discipline the player record's layout insists on,
  applied to the one field that would otherwise reintroduce a second one — and it is what makes a
  truncated file fail the exact-size check rather than read as a shorter map.
- **`next_id` is in the header rather than derived from the entries, and that is the whole of "an
  id is never reused".** Derived as `max(id)+1` it falls back the moment the highest-numbered mark
  is removed, and the next placement mints an id the client has already been told means something
  else. Stored, a removal costs nothing and the counter only ever goes up.
- **`persist.MaxMarkers` and `persist.MaxMarkerNote` are literals pinned to the wire's, never
  aliases of it.** They must equal `protocol.MaxMarkers` and `protocol.MarkerNoteMaxBytes` — a
  stored mark goes on the wire unchanged, so a file that could hold more than a `MarkerList` may
  carry is one this server could read and then not send — and the entry width above is a *function*
  of the note cap, so an alias would let a contract change reshape every file on disk with nothing
  saying so and `MarkersVersion` unbumped. Two compile-time guards in `markers.go` pin the pair in
  both directions; widening one is then a build failure at the line where somebody has to decide.
- **This store judges more than the ledger store does, and the reason is where the value goes
  next.** A column is two int32s and any pair is a place this world could have streamed. A mark is
  put straight on the wire, and `schemas/player.fbs` states what a `MarkerList` may carry: a
  non-zero id unique within the list, a known kind, a note of at most 120 valid UTF-8 bytes. A file
  that cannot produce that is refused as corrupt, because the alternative is a server answering
  with an illegal frame. `protocol.MarkerKindOK` is exported for exactly that check — a second
  switch would be a second answer to keep in step.
- **An unreadable marker file is kept and never written over**, through the same `setAside` a
  player record and an exploration ledger both use, timestamp included.
- **Every mutation is answered with the whole list, and a refusal with none of it.**
  `MarkerList` replaces the client's copy wholesale, which is `InventoryState`'s argument: sixty-four
  marks are small, a delta needs a revision and an ordering to be applied safely, and a client that
  missed one delta holds a map that is quietly wrong. Replacing is the only operation with no
  history to get right — so a refused placement or removal sends nothing at all, because the
  client's copy did not change.
- **The list follows `ServerWelcome` once, empty included**, and that is the one place this differs
  from the ledger beside it. An empty `MarkerList` is a statement — a character who removed their
  last mark on another machine must see it gone here — where an empty `MapExplored` is the absence
  of one, which is why the contract forbids that and not this.
- **`session.Markers` owns the live list and `Identities.write` saves it**, beside the record and
  the ledger and on the same two occasions: the teardown and the autosave, both off the tick
  goroutine, both under the write lock. An unchanged map costs no write, and the dirty flag is
  cleared *before* the write for the reason the ledger's is. Three files, and a failure of one never
  skips the others.
- **An unreadable marker file never refuses the connection**, and if it can be neither read nor
  moved aside the map is **sealed**: the character plays unmarked and this session writes nothing,
  so the ordinary autosave cannot destroy the evidence. The ledger's rule exactly, one file along.
- **Two of the three placement checks are second opinions, deliberately.** `protocol.Decode` already
  refuses a note over 120 bytes, a note that is not valid UTF-8 and an unknown `kind` — at the
  decode boundary, by closing the session, which `schemas/player.fbs` names as the stricter of the
  two answers it allows. So `NoteTooLong` is declared and never sent, and `Markers.Place` checks
  both anyway: it is the authority on what may enter the file, and an authority that trusts its
  caller is not one.
- **A mark outside the world is refused in silence**, because the contract has no member for it:
  `TileMisaligned`, `TooManyMarkers`, `NoteTooLong` and `MarkerUnknown` are the four the map has,
  and none of them says "that is not a place". A `RefusalReason` of `Unknown` is what tells the
  session to send no frame at all, which is the vocabulary every other silent admission refusal in
  this server already uses.
- **A mark's note is never logged.** It is text a player typed, an operator has no use for it, and
  a log line carrying it would put it in a file the privacy boundary says nothing a player wrote
  belongs in. Its *length* decides the outcome, so its length is what the debug line reports.

### `world.BlockLimit` — the world has an edge, and now it has a name

- **The number is 2²⁴ and it did not move.** It was `game.worldLimit` and is now
  `world.BlockLimit`, which `game.worldLimit` is defined as: one number, named twice for two
  audiences. Beyond it a `float32` cannot address individual blocks and the `int64` voxel
  arithmetic stops being meaningful.
- **It was promoted because `MarkerPlaceRequest` is the first message in which a client chooses a
  coordinate outright.** Everything before it — a block edit's target, a structure's anchor, a
  chunk resend, a mined voxel — starts from terrain *this server streamed*, so "inside the world"
  was a property of the input rather than a question about it. A mark's `x` and `z` are a bare pair
  of ints nothing produced, and `schemas/player.fbs` says the server refuses one outside its own
  extent without naming a number, because the number is the server's.
- **`internal/world` is where it belongs**, not `internal/game`: the world is what has an edge, and
  `world` is the leaf that `game`, `session` and `persist` all already import. `session` cannot read
  `game.worldLimit` and must not grow a second copy of it. What stays local to `game` is how the
  number is *used* — that package applies it to the vertical axis too, which is a property of the
  box being moved rather than of the world.

## What a player looks like, and the "once" that is not once per session

`PlayerAppearance` is the only message this server sends per *entity* rather than per tick or per
event, and everything about it follows from that: it is sent when a player enters a session's view
and not again while they stay there.

- **It is a message and not a field of `EntityState`, and the struct's size is a test.** A
  snapshot's entity list is a flat inlined array — the most frequently sent payload in the game,
  once per visible entity per tick — and five colours and a hair model never change for the life of
  a character. Carrying them there would pay for them at the tick rate, for ever, to send a value
  identical in every frame. `TestEntityStateIsStillFortyBytesOnTheWire` reads the encoded width
  and is what catches somebody quietly adding a fifth field.

- **The viewer remembers, not the subject.** `Player.described` maps an entity id to the tick it
  was last visible on, and it lives on the *viewer* because the question is "has this session been
  told". It is built by `Join`, so a reconnect starts empty and everything in view is described
  again — which is not a rule of its own but the absence of one.
- **What bounds it is the pruning, and the pruning is also what makes a return work.** Entries not
  refreshed on the tick they were checked are dropped, so the map is the size of a view rather than
  of a session's history — and an entity that left and came back is described again, which is
  exactly what the client needs: a snapshot is the complete existence set, so an entity that
  stopped appearing in one was despawned and its appearance went with it.
- **It is recorded as sent only once the frame is in the queue**, which is `View.MarkLoaded`'s rule
  and the same failure it exists to avoid. Unlike a snapshot there is no later frame to supersede a
  dropped one, and unlike an inventory state it needs no durable flag to say so: an unrecorded
  entity is described again on the next tick, for as long as it stays in view.
- **One encode per player per tick at most.** The frame is built the first time a viewer turns out
  not to have been told and handed to every viewer after that — `Registry.BroadcastChunk`'s "one
  encode for every recipient" — which is what makes asking the question inside the per-viewer loop
  cost nothing on the ticks where nobody's view changed, which is almost all of them.
- **The appearance a player wears comes from the store and from nothing a client said.**
  `Sim.Join` takes it beside the life, and validates it there for the reason it validates the
  life: this is the boundary a stored value crosses into the simulation, and from here it goes out
  on a wire where a client is required to refuse anything the contract forbids.

## The account service, and why it is a second command


`cmd/voxelheim-auth` keeps who the people playing here are. It ships from this module and
shares nothing else with the game server: not a port, not a directory, not a package the
simulation uses.

- **It listens over TLS and there is no plaintext form of that either** (#131). The certificate
  is `internal/certs`' — self-signed, kept under `-auth-dir` beside the accounts and the signing
  key, generated on first start and read back after — and its SHA-256 is logged at every start as
  `certificate_sha256`, spelled exactly as `cmd/voxelheimd` spells its own so an operator reads
  one attribute name out of two logs. Both callers are given that number out of band and refuse
  anything else: `-account-service-fingerprint` on a game server,
  `--account-service-fingerprint` on a client. **This hop is the root of the whole chain** — a
  game server's fingerprint reaches a client inside `/v1/servers`, which is worth nothing unless
  the connection that carried the list was the right one — so it is the one identity that can
  inherit trust from nothing above it, and the one number an operator has to hand out.
  `certs.Ephemeral` is deliberately not used here: a service that keeps accounts by definition
  cannot present a new fingerprint on every restart.
- **A second command rather than a second workspace.** A top-level `auth/` would need its own
  CI job, its own `AGENTS.md`, a rule in `scripts/changed-areas.sh`, an entry in `ci-gate`'s
  selector audit and a row in `scripts/test/gate-tables.test.sh`. That is pipeline construction
  for no benefit at this scale, and it would fork the store discipline `internal/world` and
  `internal/persist` already share.
- **`cmd/voxelheimd` must not import `internal/auth`, and nothing else may either.** They are
  separate trust domains that happen to ship together, and the moment the simulation can open
  the accounts directory, "the account service holds the accounts" stops being true. It is a
  test rather than a sentence: `internal/auth/imports_test.go` parses every Go file under
  `server/` and asserts that the only importer is `cmd/voxelheim-auth` — stated the strong way
  round, so the transitive question never arises, because a package that never holds this one
  cannot pass it on. Its other half asserts the reverse direction: `internal/auth` imports
  `internal/world` and nothing else of ours, for the five record helpers and for nothing more.
  Both fail closed — a walk that found no files is a failure, not an empty set that passes.
- **No credential is kept, so there is none to leak.** An account is an internal id, the provider
  identity it was created from, a display name and a created-at time. Whatever a provider hands
  over to show that somebody is who they say they are is checked by the flow that receives it
  and then dropped. A leaked accounts directory is an embarrassment rather than a way in, and
  that is a property of the format instead of a rule somebody has to keep remembering.
- **The store judges its keys, and nothing but its keys.** `internal/persist` still judges no
  *life*, because `internal/game` owns what a life may say — what it judges is what a key may be,
  and #103 made a character's name one of those: unique within the world, so it is decided in the
  store rather than described by it. There is no such layer above
  `internal/auth`, so the line is drawn elsewhere: between keys and description. A provider
  identity and an account id are refused if they are not ones this build would write — on the way
  in *and* on the way out, because a format whose two halves disagree about what an account is
  has two definitions. A display name and a timestamp describe rather than find, and are written
  down as given.
- **An unreadable record is never an absent one**, and here that rule has teeth. Reported as "no
  such account", a damaged file mints a *second* account for a person who already has one: they
  sign in successfully, find none of their characters, and the new account's first write lands on
  the record nobody could read. `Store.Ensure` therefore stops on the error rather than minting,
  and the damaged file is left exactly where it is.
- **There is no ephemeral mode, which is the deliberate difference from every store under the
  world directory.** A nil `world.Store`, `persist.Store` or `persist.ClockStore` is a world an
  operator chose not to keep, and losing an evening's digging is a trade somebody can knowingly
  take. An account nobody kept is a person who cannot get back in, so there is no nil `auth.Store`
  at all: `-auth-dir ""` is a refusal at startup rather than a store that quietly writes nowhere.
- **The file name is a hash of the provider identity, never the identity.** A provider subject is
  a third party's text and may hold a slash, a NUL, a `..` or four hundred characters of anything;
  a digest is fixed-length hex, and the directory listing is not a roster of provider ids either.
  The identity is written *into* the record as well, so a file copied onto another identity's path
  is caught rather than answered — `internal/world` writes a chunk's coordinate into its file for
  the same reason, and `internal/persist` now does too. It could not while a player record was
  named for the hash of a token: only the credential decided which file was opened, so there was
  nothing to compare a name against. A character id is a number this server minted, so #103 put it
  in the record beside the file name and the startup scan checks the two agree. The NUL between the two halves of the hashed
  input is load-bearing: without it `("disc", "ordX")` and `("discord", "X")` share a file, which
  is two people sharing an account.
- **The HTTP surface is a table, and the table is the whole surface.** `routes()` is the one place
  a pattern is registered, and the method is part of the pattern — Go's own `ServeMux` has
  understood `GET /healthz` since 1.22, so a wrong method is a 405 from the mux rather than the
  first four lines of every handler. `TestTheRouteTableIsTheWholeSurface` drives the mux and
  checks that what answers is exactly what is listed, `/debug/pprof/` included, which is what
  would answer if this had drifted onto `http.DefaultServeMux`.
- **`/healthz` reports liveness, not readiness, and touches no disk.** A health endpoint that
  stats the accounts directory on every probe turns a monitoring interval into disk load and lets
  a slow filesystem report a healthy service as dead. What could go wrong with the storage is
  asked once, at startup, where it refuses to bind instead of answering probes — which is why
  "this process is up" is the whole of what this can honestly claim.
- **Flags are validated before they are narrowed**, the rule `-tick-rate` keeps one binary over.
  The listen port is the case that shows it: a port is a `uint16` by the time anything binds it,
  so `-listen 127.0.0.1:99999` fails at startup quoting the number the operator typed, rather
  than becoming a silent 33465. A named port is refused too — `:htpt` is a typo far more often
  than it is a service name, and a machine whose `/etc/services` differs is a machine where the
  same flag binds somewhere else.
- **Every configuration is checked before anything is created, and `run` is two passes because
  of it.** The registration key and the Discord sign-in are read and refused first; only then are
  the account store, the ticket signing key and the registry opened. It ran the other way round
  until #136, so a start about to be refused for a typo'd `-discord-client-id` had already minted
  an Ed25519 pair into the directory the typo named — and there is no revocation, so that pair
  stayed valid for as long as the file existed, in a directory nobody would remember to delete.
  **The directory is not created either**: `auth.OpenStore`, `ticket.LoadOrCreate` and
  `registry.OpenStore` all `MkdirAll`, and a start that cannot succeed has no business leaving a
  tree behind. `TestARefusedStartCreatesNothing` pins both halves for every shape of unusable
  Discord configuration, and `TestAnAcceptedStartMintsTheSigningKey` pins that an accepted start
  still mints — the point is that the two outcomes differ, and before the hoist they did not.
  What the hoist may **not** do is move the storage below the listener: the store is opened before
  the port is bound, `TestTheStoreIsOpenedBeforeThePortIsBound` says so, and putting the
  configuration above the storage is what makes the storage genuinely the last thing that can
  refuse a start rather than merely the first thing that runs. The cost is that `signIn` is built
  in two steps — the flow when the configuration is checked, the account store when there is one —
  which is why those two statements sit within a few lines of each other in `run`.

## Signing in with Discord, and the secret that does not exist

`internal/discord` runs OAuth 2.0 Authorization Code with PKCE as a **public client**, and
`cmd/voxelheim-auth/signin.go` is the one file where the identity it produces meets the store
that records it. Two routes, `POST /v1/signin/discord/start` and
`POST /v1/signin/discord/finish`.

- **There is no client secret, and there is nowhere for one to come from.** `discord.Config`
  has no field for it and no call sends one; PKCE is what stands in for it. The verifier is
  minted here and never leaves the process — only its SHA-256 goes to the authorize endpoint —
  so a code intercepted on the way back cannot be redeemed by whoever intercepted it. A secret
  shipped to players is not a secret, which is the whole reason the flow is shaped this way.
- **The redirect lands on the player's machine.** The client opens the authorize URL in a
  browser and catches the redirect on a loopback listener of its own, which is why this service
  needs no public callback URL and why the flow works behind a home router. `-discord-redirect-uri`
  names that address; nothing here ever binds it.
- **The state is consumed before the provider is called**, which is the "a code may be redeemed
  once" rule and this service's rather than Discord's. Taking it afterwards would leave a window
  in which two requests carrying one state both found the verifier, and would leave a state
  usable for a replay after a provider call that failed halfway. The honest cost is that a
  transient failure means starting again — a refusal that says so, rather than a sign-in that
  half-succeeded.
- **Unknown, expired and already-redeemed are one answer.** An error that distinguished them
  would tell whoever is guessing which guesses are getting warmer. Nothing compares a state byte
  by byte either: it is a map key and the lookup is the whole of the check.
- **Nothing from the provider is logged, and most of it is never even decoded.** `refresh_token`,
  `expires_in`, `scope` and `email` have no field in the two response structs, so there is no
  value for anything to leak — the scope asked for is `identify`, and the struct agrees with it.
  What *is* held is `discord.Secret`, which redacts through fmt, through log/slog **and** through
  encoding/json — the three `identity.Account` also carries, plus `%#v`: a `Secret` is a string, so
  a struct holding one is something `encoding/json` would otherwise write out verbatim. A
  provider's response body is a third party's text, so a refusal names the HTTP status and
  nothing from the body — and the JSON decode error on the request body is deliberately not
  logged either, because that body carries an authorization code.
  `TestNothingFromTheProviderReachesTheLog` drives a whole sign-in plus every refusal through
  both handlers and looks for each value in hex, base64, base64url and raw — the Discord user id
  and the display name included, which are personal data rather than credentials.
- **A refusal, never a half-succeeded sign-in.** Four sentinels — `ErrNoSuchSignIn`,
  `ErrRejected`, `ErrProviderUnavailable`, `ErrTooManyPending` — and none of them returns an
  identity, so the account store is only ever reached after somebody has actually proved who they
  are. The provider is asked first and `auth.Store.Ensure` second, and every refusal test asserts
  the accounts directory is still empty rather than trusting that it is. The 4xx/5xx split at the
  token endpoint is the difference between "this sign-in is not valid" and "ask again later";
  429 is the exception, because being rate-limited says nothing about the code. Every failure at
  the *identity* endpoint is the provider's, a 401 included: the token it is refusing is one it
  issued seconds ago.
- **The endpoints are struct fields, not constants**, which is what lets every test point the
  flow at an `httptest.Server`. A test that reached the real Discord would be a test of somebody
  else's uptime. The fakes compute the S256 challenge from RFC 7636's formula spelled out at the
  call site rather than by calling `challengeFor` — a fake that borrowed the code under test
  would agree with a wrong one — and `TestTheChallengeIsTheRFC7636Transformation` pins that
  function to appendix B's vector.
- **`-discord-client-id` empty is "not configured", not an error.** A Discord application is
  something an operator registers and this service cannot invent, so refusing to start without
  one would mean the account service could not run at all — including in every test that is
  about the store, the port or the health probe. The routes exist either way and answer 503
  `sign_in_not_configured`, which is a service that says what is missing rather than one that is
  silently absent. A client id *with* an unusable redirect URI is a real misconfiguration and
  refuses **before anything is created** — ahead of the account store rather than behind it, in
  the configuration pass described above, and so long before the port is bound.
- **The pending table is capped and swept.** The start endpoint is unauthenticated by
  construction — a sign-in is how somebody becomes known, so there is nobody to authenticate yet
  — which makes an uncapped table a way to spend this service's memory for the price of an HTTP
  request. The sweep runs on insert rather than on a timer, because that is the only place the
  table grows, and the cap is checked after it so a table full of expired entries is not a
  refusal.
- **The account's display name is the one recorded at creation.** `auth.Store.Ensure` returns a
  found account exactly as stored and hands the write-through decision to this flow; this flow
  declines it for now. Comparing a fresh name against a stored one needs `auth.truncateName`,
  which is unexported, and comparing untruncated would write on every sign-in for any name past
  the 64-byte cap. Refreshing it is a change to `internal/auth`'s surface and belongs to the
  issue that wants it.
- **The access token is dropped, and no attempt is made to revoke it.** This service asks Discord
  who somebody is once and has no further use for the answer. Revocation would be a fourth call
  to a provider that has already answered, on a token that expires on its own.

## A ticket the game server can check on its own

`internal/ticket` holds the key pair and mints; `cmd/voxelheim-auth/tickets.go` publishes the
public half at `GET /v1/ticket-key`; a finished sign-in hands the ticket back in
`session_ticket`. `internal/ticket` is the one package here the **game server** is expected to
import, which is what shapes almost every rule below.

- **The game server verifies a signature instead of asking permission.** The alternative — a call
  to the account service on every join — makes a small service a hard dependency of play, and its
  failure mode is that nobody can play a game running on hardware that is up. A server reads the
  public key once at startup, keeps it, and from then on admitting a player is arithmetic.
- **The cost is stated rather than mitigated: there is no revocation.** A ticket cannot be
  withdrawn before it expires, so a stolen one dies only by expiring, and `ticket.Lifetime` —
  eight hours — is the whole of the answer. A grace period for an unreachable verifier would be
  strictly worse than having none: it is a rule an attacker triggers by blocking the service.
- **The bytes are the ones `ClientHello.session_ticket` carries**, and the body's layout is a
  consequence of that number rather than a choice beside it: 96 = a 32-byte body and a 64-byte
  detached signature, as `schemas/handshake.fbs` states. Inside the 32: 16 bytes of account id,
  12 of world id, 4 of expiry. The world field is 12 rather than 8 because it is a truncated
  digest and it is what defends against a ticket being replayed at another world — the attacker
  there is the operator of the world the ticket was issued *for*, who picks their own world's
  name, so the work is a second preimage; 96 bits is out of reach and 64 is not comfortably so.
  The expiry is four bytes of Unix seconds and therefore stops working on 2106-02-07, which is
  written down beside the constant; `Mint` refuses an expiry it cannot represent rather than
  wrapping it. `internal/protocol` states the same 96, and `internal/ticket/imports_test.go`
  parses that file to pin the pair, because the two packages must not import each other.
- **There is no version field in the body**, and that is an argument rather than an omission: a
  ticket is only ever presented in a `ClientHello`, which carries `protocol_version` beside it,
  and the contract already says changing the length, the scheme or the split is a version bump.
- **A signature says what it is a signature of, and the tag is in the digest rather than in the
  body** (#138). What a mint signs is `SHA-256(ticketBodyDomain ‖ body)`, not the body — so the
  key's guarantee is "the account service signed this *ticket*" instead of the weaker "signed
  these 32 bytes", and a second 32-byte object this pair ever signs cannot be presented at a
  handshake and decoded as an account, a world and an expiry. `worldIDDomain` is the same idea one
  layer down, applied to a digest; this is the worse of the two failures, because a digest
  collision needs luck and a second signed object needs only somebody adding a feature. The domain
  goes in front of the hash because the body has no room to spare: a tag inside the 32 would have
  to come out of the world id (96 bits, and 64 is not comfortably out of reach) or the expiry
  (already argued down to four bytes), whereas a prefix to a hash costs nothing and leaves
  `ClientHello.session_ticket` at 96. **The cost is that every ticket minted before it stops
  verifying**, and there is no revocation to soften that: deploying this is one sign-in for
  everybody connected. It is refused as `ErrNotATicket` rather than `ErrBadSignature`, so what an
  operator reads is the transition rather than a key mismatch that is not there — the same carve-out
  `ErrVerifierWorld` got from `ErrWrongWorld`. That branch recognises the undomained shape only; an
  object under a *sibling* domain is `ErrBadSignature`, which is honest, because nothing here knows
  that domain.
- **`internal/ticket` is a leaf, and it has to be.** The game server imports it in order to
  verify, so anything reachable from it is reachable from the simulation — an import of
  `internal/auth` would put the accounts directory back inside the trust domain it was split out
  of, and `internal/auth/imports_test.go` would not see it coming, because the importer would be
  `internal/ticket` rather than `cmd/voxelheimd`. Its own `imports_test.go` holds that end:
  `internal/world` for the five record helpers, nothing else of ours. It is also why
  `ticket.AccountID` is this package's own sixteen bytes — `signin.go` converts `auth.AccountID`
  into it, and that one line stops compiling if either width moves.
- **Verification touches nothing, and that is asserted as a claim about imports.** Every file but
  `key.go` is held to an allow-list, and not one entry on it can open a file or a socket; `now` is
  a parameter, so there is not even a clock. A behavioural test can show that one call did no I/O.
  This shows that none can.
- **Half a pair is refused rather than repaired, and an unreadable one is an error rather than a
  fresh start** — `internal/certs`'s rules, with more at stake. It is refused even in the
  direction that *could* be repaired, because deriving the missing public half would mean this
  service deciding on its own that the survivor is the correct file, and one rule that always
  says the same thing beats two that depend on which file went missing. Regenerating over a
  damaged pair invalidates every ticket in flight and every copy a game server has stored: a
  fleet refusing every player at once, on the strength of a permission problem. The two records
  are the same size, so they carry different magics — without that a seed would read back as a
  public key and nothing later in the load would notice.
- **The private key is never logged; the public key always is.** `ticket.SigningKey` is a struct
  with an unexported field, which is stronger than the named types redacted elsewhere here: there
  is no conversion out of it, no accessor and no `Reveal`, because there is no legitimate caller
  for the bytes. It redacts through fmt, through `%#v`, through log/slog and through
  encoding/json — four routes, and `%#v` is the one a Stringer never sees. `ticket.Pair` renders
  as its public key, so the deliberate disclosure is the default.
  `TestNothingOfTheSigningKeyReachesTheLog` drives a whole mint plus every refusal through both
  handlers, and **reads the seed off disk and proves it rebuilds the published key before looking
  for it** — a secrecy test searching for the wrong bytes passes while proving nothing.
- **A type composed of redacting types is not itself redacted, and `ticket.Pair` had to learn it
  twice** (#126). `fmt` reaches a `Stringer` or a `GoStringer` only through a value it could hand
  to an interface, and `reflect.Value.CanInterface` is false for an **unexported field** — so the
  reflection walker steps straight past every method `SigningKey` declares and prints the ed25519
  key inside it. `Pair` therefore declares its own `GoString`; the outer type has to say so.
  And **every rendering method on a redacting type takes a value receiver**, because a method set
  on `*T` leaves a `T` value implementing neither `fmt.Stringer` nor `slog.LogValuer`, which a
  caller reaches by nothing more exotic than a dereference. `discord.Secret` was already declared
  that way, `identity.Account` is declared that way for the same reason, and `Pair` was the one
  that was not — its own doc comment claimed the opposite.
- **The redaction test searches the form a leak actually takes.** `renderings()` covers raw, hex,
  base64, base64url, space-joined decimal **and `%#v`'s `0x9c, 0x1f, …`** — the last of which was
  missing, so the one guard that should have caught the `%#v` leak was green while the key sat in
  its output. Nothing in either secrecy test quotes what it found: a failure means the rendering
  holds the signing key, and this repository's CI log is public.
- **A clock at or before the epoch is a refusal at the mint.** `encodeBody` bounds the *expiry*,
  which is `now` plus `Lifetime`, so it only ever fired a whole lifetime before 1970 — and a host
  that had never set its clock got a 200 and a ticket that expired before it was issued, with no
  retry, because `Redeem` spends the sign-in before the mint. The bound on `now` lives in
  `Pair.mint`, where the question is about the machine rather than about the format.
- **`Verify` bounds a ticket's remaining life, not only its expiry.** A body carrying `0xFFFFFFFF`
  is a legal record, so a ticket signed with the real key verified with seventy-six years left.
  `Mint` cannot produce one today, which makes this defence in depth — and the game server is the
  party that must not have to trust the account service beyond its signature. The bound carries
  `verifierClockSkew` and the expiry does not: being strict about *freshness* refuses real tickets
  on a fleet whose clocks differ by seconds, and being lax about expiry is a stolen credential
  nobody can revoke.
- **A verifier that names no world answers `ErrVerifierWorld`, not `ErrWrongWorld`.** They are two
  different things to tell an operator, and the misconfiguration is the one that hides: every
  player refused, every line saying the ticket names another world, nothing saying that this
  server names none. `ErrPublicKeySize` is the same class of question and always had its own
  sentinel.
- **The key directory's mode is made true rather than asked for; the key file's is refused.**
  `os.MkdirAll(dir, 0o700)` does nothing to a directory that already exists, and rename(2) is
  governed by permission on the directory — so 0600 on the seed does not stop anybody who can
  write there swapping in a pair of their own. `LoadOrCreate` sets the directory to **exactly**
  `0700` on every start — "carries no bit outside 0700" is the security question and only half of
  the one that matters, since 0600 and 0500 pass it while leaving a directory this service cannot
  traverse or write. It does **not** tighten a signing key found at 0644: a directory that is too open
  is a risk that can still be closed, and a key file that is too open is a disclosure that has
  already happened, so that one is `ErrKeyPermissions` and a message an operator has to read.
- **`LoadOrCreate` serialises, and only within one process.** Two concurrent callers both saw an
  empty directory, both generated and both wrote, and the four renames interleave into a pair the
  next start refuses — 76 damaged directories in 200 rounds of four callers. A package mutex fixes
  that exactly as far as `auth.Store` and `registry.Store` fix their own: **one `-auth-dir` per
  process is a property of the deployment**, and none of the three enforces it.
- **A failed second write removes the first half.** This is the one place in the package where
  deleting a private key is right, and it is right because of what this function knows and no
  later start can: the key it just made has never been published, never signed anything, and is
  worth nothing. Left behind it is half a pair that refuses every start, with the one recovery a
  first start has no backup for — which is why that refusal now names both recoveries and says
  which situation each belongs to.
- **A ticket is minted at the end of a sign-in, because there is nowhere else it could be.** A
  separate endpoint would need the caller to prove who they are, and the only thing that ever
  proved that is the authorization code the finish request spends. A credential that outlived the
  sign-in so a ticket could be asked for later is exactly the refresh token this design does not
  have. So `finish` names the world up front, and a player joining a second world signs in again.
- **The world is checked before the provider is called.** A code may be redeemed once: refusing
  after the redemption would spend somebody's sign-in, and mint them an account, for a mistake
  this service could see in the request body without asking anybody anything. The name is
  constrained rather than normalised — lowercase letters, digits and hyphens, `internal/auth`'s
  rule for a provider name and for its reason — and it is never echoed into a log, because it
  arrives in an unauthenticated body.
- **Key rotation is a known gap, written down rather than built.** One operator, one pair, and
  every game server holding a copy by hand; rotating would need a way to publish two keys and a
  window in which both verify. Deleting the pair is the whole of the ceremony today, and it costs
  every ticket in flight.

### The doorman, and why it needs no telephone

`cmd/voxelheimd` reads the public key once at startup and `session.Identities.Admit` verifies a
signature; nothing on the admission path touches the network again.
 This is the game server's half
of the section above, and every rule in it follows from that one sentence.

- **A start with no key is a refusal to start.** Exactly one of `-account-service` (read the key
  from `GET /v1/ticket-key`) or `-ticket-key` (the key in hex, copied by hand) is required, and
  `-world-name` is required beside them. The alternative is a flag or a fallback that admits
  players unverified, which is the second way in this design exists to remove — and it is the
  failure nobody notices, because a server with no doorman looks exactly like a server that is
  working. `session.NewIdentities` refuses a nil verifier for the same reason one layer down: there
  is no way to build a claim set that admits people without checking them, so the rule cannot be
  undone by forgetting an argument.
- **Two key sources, mutually exclusive rather than ordered** — `internal/registry`'s rule for its
  own pair, and for its reason: a precedence rule is something an operator has to remember, and one
  who has set both has already made a mistake worth being told about. `-ticket-key` exists because
  the fetch cannot tell that it reached the right service (below), and pasting the key is the only
  way to avoid that today.
- **The key is decoded to bytes, so its case does not matter** — the deliberate opposite of
  `internal/registry`'s certificate fingerprint, which is refused rather than folded. The
  difference is what happens to the string: a fingerprint is *compared as text*, so two spellings
  are two values that eventually fail to match; this one is compared as bytes.
- **`ticket.Verify` and never `VerifyAnyWorld`.** The world comparison is what stops the operator of
  one world collecting its players' tickets and presenting them at another as those players, and
  what turns an account ticket away at the door. `internal/ticket/callers_test.go` holds that
  boundary by name and named this issue while doing it.
- **Five refusals, distinguishable in the log and identical on the wire.** Absent, the wrong length,
  signed by another key, expired, issued for another world: every one is `BAD_REQUEST` carrying the
  same sentence, and `session.Refused.Cause` carries the sentinel that says which. That split is
  `game.Player.RemoveStructure`'s rule with a credential in place of a camp — a client that could
  tell "expired" from "signed by another key" from "wrong world" could ask this server questions
  about tickets nobody presented, on a connection nobody has authenticated.
  `TestEveryRefusedTicketLeavesTheSameFrame` compares the frames byte for byte;
  `TestATicketThisServerWillNotAdmitIsRefused` names each sentinel. **A ticket signed the old way
  is `ErrNotATicket` rather than `ErrBadSignature`** and the log says so, because that is a
  deployment an operator can recognise rather than a key mismatch that is not there (#138).
- **Two of `Verify`'s answers are not refusals here.** `ErrPublicKeySize` and `ErrVerifierWorld` say
  this server is misconfigured, and `BAD_REQUEST` would blame a client for something it did not do.
  They end the session with no reply and reach a log, the same split `session.Refused` already
  draws. `session.NewVerifier` makes both unreachable by refusing such a configuration at startup —
  which is #126's lesson taken one layer out: the misconfiguration that hides is the one whose
  symptom is the sentence that means the check is working.
- **The exclusivity claim is the account's**, so the same person cannot hold two live sessions on
  one world — two machines, two sign-ins, two different tickets, one session. The ordering that
  makes it correct is unchanged and is stated above: `sim.Leave` → record write → release.
- **The startup line names the world, the world id and the public key, at Info, on every start.**
  All three are public, and they are what makes a fleet-wide key or world mismatch legible in one
  line instead of as one refusal per player. Nothing else about a ticket is ever logged.
- **`ServerWelcome.player_token` is filled with zeroes.** The field is retired and the contract
  still requires it present and exactly 32 bytes, so the honest value is the right shape carrying
  nothing. No V6 client is ever on the far end of a welcome — the version check refuses them first
  — and a V7 server reads past the field on the way in, so nothing can be resumed with it.
- **The fetch is pinned, and that is what closed the substitution** (#131). The endpoint is
  deliberately unauthenticated, so the exposure was never confidentiality but *substitution*:
  whoever could answer for that address handed this server their own public key, and this server
  then admitted the tickets they minted and refused every real one — for as long as the key is
  kept, which is for ever. `-account-service` is now `https` only and **requires
  `-account-service-fingerprint` beside it**; `accountServiceClient` is the one client this
  server reaches that service with, and it compares the SHA-256 of the leaf against that flag.
  `InsecureSkipVerify` is set there and is a *replacement* rather than a bypass — what it turns
  off is a chain to a root store and a hostname match, neither of which can say anything about a
  self-signed certificate at an address an operator typed, while `VerifyPeerCertificate` runs
  regardless. A missing or malformed fingerprint is a refusal at startup, because a flag that
  could be omitted is the hole reachable by omission. `-ticket-key` remains the way to avoid the
  fetch entirely, and is now a choice about whether to depend on that service at startup rather
  than a way around an unauthenticated hop.
- **The bound before the work, at the one call this server makes**: the response is read through an
  `io.LimitReader` and refused if it exceeds `maxTicketKeyResponseBytes` before any JSON is parsed,
  and the published `algorithm` is compared rather than assumed. It is `ticket.Decode`'s ordering
  and `registry.ParseKey`'s, one protocol up.

## The list that ends the trust chain

`internal/registry` holds the registered game servers and `cmd/voxelheim-auth/servers.go` is
the two endpoints in front of it: `POST /v1/servers` to register, `GET /v1/servers` to read
the list. It is where trust on first use stops being how a player decides who a server is.

- **The chain, stated once.** The client knows the account service by construction — it is
  compiled in. The account service knows the game servers because an operator registered
  them. Therefore a client can verify a game server it has never met, against a fingerprint
  it was told rather than one it guessed. The fingerprint exchanged by hand that #83 left as
  the only manual step disappears here.
- **The fingerprint is `certs.Fingerprint`'s number and nothing else computes it.** A
  registration carries it as text, `internal/registry` checks that it is a well-formed
  SHA-256 digest in lowercase hex, and the list serves it back verbatim. A second way of
  arriving at the number is a second number to disagree with the first, and the one an
  operator reads out of `voxelheimd`'s `certificate_sha256=…` startup line has to be the one
  a client compares. Uppercase is refused rather than folded, for the reason a provider name
  and a world name are: one value with two spellings is one value that eventually gets
  compared before it reaches the folding.
- **Two credentials, and neither is the other.** Registration presents an operator-configured
  key; the list presents a session ticket. Anybody able to register could put their own
  address in the list under a name players trust — and that is a *better* attack than the one
  this list replaces, because the client would believe the answer. Anybody able to read it
  without a ticket would have a public directory of people's home addresses.
- **These are the only two routes in this service that answer 401**, and `signin.go` says in
  as many words that none of its own does. That is not a contradiction: it declines the status
  because it has no authentication scheme to name, and inventing one to justify a header would
  be inventing one. Here `Bearer` is what the request actually carries, so
  `WWW-Authenticate: Bearer` is true and the 401 says what a 400 could not.
- **The registration key is read from a file or from the environment, never from a flag**, and
  `registry.Key` keeps only its SHA-256 — so "the key is never logged" is a property of the
  type rather than a rule every call site remembers. A flag would be visible in `ps` to every
  user on the machine. The two sources are mutually exclusive rather than ordered, because a
  precedence rule is something an operator has to remember and one who has set both has
  already made a mistake worth being told about. A key under `registry.MinKeyBytes` is refused
  at startup; there is no rate limit and none is coming, so the key's length is the whole of
  the bound on guessing.
- **A credential is bounded before it is worked on, at both routes.** These are the two places
  in this service reachable by somebody nobody has authenticated yet — a credential has to be
  read to be refused — and an `Authorization` header is as long as whoever sent it chose, a
  megabyte by `net/http`'s default. So the length is settled before the work:
  `registry.MaxKeyBytes` bounds `registry.Key.Matches` before it copies and hashes, and
  `ticket.EncodedSize` bounds `ticket.Decode` before it decodes. It is `transport.MaxFrameSize`
  enforced on the length prefix, one protocol up — **the ordering is the security property** —
  and neither check refuses anything the work would have accepted. `registry.ParseKey` refuses
  a key over `MaxKeyBytes` for the same reason `Matches` does, and the two halves are one
  decision: a bound on only the presentation is an operator whose long key silently stops
  working, an authentication failure with nothing in any log, which is exactly what trimming
  the key's whitespace exists to prevent.
- **A re-registration replaces the record, which is the point rather than a convenience.** The
  address the list serves is the one the server last announced, so a home connection that gets
  a new address overnight is invisible to players. Nothing in a record survives a
  registration, because every field of it came from the announcement.
- **A server that stops announcing is shown offline, never dropped** — `registry.OfflineAfter`,
  five minutes, exported because the announcing side must read it rather than pick a second
  number. Dropping it would make a server that is briefly unreachable look like one nobody
  ever registered, and an empty list is what a player concludes the whole game is broken from.
- **The address is the one value in this service that never reaches a log.** It locates
  somebody's house, which is the reason the list is behind a credential at all, and a value
  that must not be published must not be in a log line either. Every other field is logged and
  quoted back on purpose: registration is authenticated, so the text is the operator's own,
  and naming the field that was wrong is the difference between a mistake they can fix and one
  they have to guess at. That is the deliberate opposite of the sign-in routes' rule, and the
  reason is that their caller is unauthenticated and this one is not.
- **A damaged record fails the whole list and is repaired by the next announcement.** Skipping
  it would make a server silently vanish — the player sees a shorter list and nobody is told
  anything — and unlike `auth.Store`, this store *can* heal, because the announcer holds every
  field and is restating all of them. So `List` reports and `Register` overwrites, and the two
  opposite answers are the same rule applied where each caller stands.
- **The server name is the world name.** One string does both jobs, which is what closes the
  chain: the client reads a name out of the list and hands that name to
  `POST /v1/signin/discord/finish`. `registry.Server.Validate` asks `ticket.WorldIDFor` rather
  than restating its rule, so a name this store accepts is always one a ticket can be minted
  for — a registry that accepted a name the ticket service would not is a server a player can
  see and cannot join.
- Deliberately not here: withdrawing a server (deleting the file is the ceremony), any
  moderation, player counts, and any probe of a registered address. Nothing in this service
  dials anybody; `Online` reports only that a server said something lately.

### An account ticket names no world

`ticket.Mint` refuses a zero `WorldID` and `finish` required a `world`, which closed the chain
in a circle: a player needs a ticket to read the list, needs to name a world to be minted one,
and the list is what tells them the worlds exist. **A zero `WorldID` is now the account
ticket** — "this ticket names no world".

- `POST /v1/signin/discord/finish` accepts a body whose `world` is absent or empty and mints
  an account ticket. A *named* world that this service will not issue for is still
  `world_not_named`; empty means "no world", and `" "` is an attempt at one that failed.
- **The zero id is safe to spend on this because `WorldIDFor` never produces it**, so the two
  kinds cannot be confused. `TestAWorldIDIsTheNameAndNothingElse` pins that rather than
  leaving it an assumption.
- **`ticket.Verify` is unchanged and still refuses a ticket whose world is not the one asked
  for, so a game server rejects an account ticket** — for every world it could be configured
  with, including the misconfiguration of having none.
  `TestAnAccountTicketNamesNoWorldAndIsRefusedByEveryGameServer` is that property, and it is
  the one to break a change on.
- **Minting one is a different method, not a sentinel argument.** `Mint` still refuses a zero
  world; `Pair.MintAccountTicket` is how one is asked for on purpose. The zero value is what a
  caller gets from a forgotten field, and the difference between "I meant no world" and "I
  forgot the world" cannot be recovered from the argument — so it is carried by which function
  was called, where it cannot be lost.
- `ticket.VerifyAnyWorld` is the account service's own check and **a game server must never
  call it**. Its one caller is the list endpoint, which needs to know only which account is
  asking. It accepts either kind, so somebody already holding a world ticket does not have to
  sign in again to read the list.

### Telling the list where home is, and the one thing announcing may never do

`cmd/voxelheimd/announce.go` is the other end of `POST /v1/servers`: the game server saying
where it is, repeatedly, so that a home connection which changes address overnight stops being
invisible to players. It is outbound only — nothing in the account service dials a game server,
which is what keeps one behind a router with a single forwarded port.

- **A failed announce is logged and survived, never fatal, and that is the criterion the rest
  of this design rests on.** Admitting a player is a signature check precisely so that the
  account service being down costs nobody a game; an announcer that could refuse a start or end
  a process would rebuild that hard dependency in one line. So *every* way this goes wrong ends
  in a server that is still serving: nothing configured, half configured, an address no player
  could dial, a service that is unreachable, one that refuses, one that stalls, one that answers
  nonsense. `TestAFailedAnnounceIsLoggedAndSurvived` drives the last four through a real
  handshake and asserts the player is welcomed anyway, and
  `TestABrokenAnnounceConfigurationDoesNotRefuseTheStart` asserts the first two through `run`.
  **This is the deliberate opposite of `openVerifier`**, which is fatal on the same service
  being down — and the asymmetry is the whole point: one call decides who may play and happens
  once, the other decides who can *find* the server and happens for ever.
- **Announcing is off unless `-account-service`, `-announce-address` and a registration key are
  all given, and "off" is one clean startup line rather than a complaint per interval.** Nothing
  configured is Info — a LAN game, a test, an operator who has registered nothing — and a half
  or unusable configuration is Warn, because somebody meant to be in the list and will not be.
  Neither is ever repeated: nothing about it can change while the process runs.
- **A failure at runtime *is* repeated, but not loudly.** The first failure and every change of
  reason warns; an identical repeat is a debug line; the return to success is one Info. A
  service that has been down since lunchtime is one thing that is wrong, not three hundred, and
  a warning that fires on a timer stops being read. Every failed announce is still logged, which
  is what the acceptance criterion asks for.
- **The registration key comes from the environment or a file and never from a flag.**
  `loadRegistrationKey`'s rule at the other end of the same secret — one variable,
  `VOXELHEIM_REGISTRATION_KEY`, deliberately shared, because it is one value and two names for
  it is one more thing to get wrong. It is not a player credential and it is a credential:
  whoever holds it can put an address in the list under a name players trust, which is a better
  attack than the one the list replaces. `registry.Key` keeps only a digest because it only ever
  *compares*; this end has to *present*, so `main.registrationKey` holds the bytes and redacts
  through all four routes instead.
- **The outer type redacts too, and that is not belt and braces.** `fmt` reaches a Stringer only
  through a value it could hand to an interface and `reflect.Value.CanInterface` is false for an
  **unexported field**, so `%+v` on an `announcer` printed the whole key straight past every
  method `registrationKey` declares. It is `ticket.Pair`'s lesson for the third time, it was
  found by the secrecy test rather than by reasoning, and the fix is four value-receiver methods
  on `announcer` itself. **A struct that holds a redacting type is not a redacting type.**
- **The fingerprint is the one `certs.Fingerprint` produced, handed down from `listen`.** That
  function now returns it as well as logging it, because #150 made a client take its expectation
  from the list rather than from a pinned file: the number in the startup line, the number in the
  list and the number a client demands are one string, or the server is one nobody can join.
  Nothing in this file computes a digest.
- **The announced address is separate from `-listen` and cannot be derived from it.** A server
  bound to `0.0.0.0` is reachable at an address only its operator knows. `0.0.0.0:7777` and
  `[::]:7777` are refused *here* and nowhere else — they are well-formed `host:port`, so
  `internal/registry` would write them down and serve a row every client dials and none reaches,
  and the listening side is the only side that knows the difference between "bind everywhere"
  and "come to this address".
- **The address is the one value this server keeps out of its own log**, which is
  `internal/registry`'s rule for the same string: it locates somebody's house, which is why the
  list is behind a credential at all. The operator typed it and `-h` documents it, so nothing is
  lost.
- **Nothing from a response body is written down except a refusal code out of the closed set
  `cmd/voxelheim-auth` answers with**, and the acknowledged name is deliberately not among them
  — the world name in the success line is this server's own. Whatever answered that address is
  not known to be the account service, so its free text is a stranger writing in this log. The
  transport error is unwrapped from its `*url.Error` for the same reason one layer down: that
  wrapper renders as the URL it was given, which is the one string an operator may have written
  a password into.
- **The interval is read rather than copied.** `registry.OfflineAfter` is documented as the
  number the announcing side must be under, and `internal/registry/imports_test.go` forbids this
  process from importing that package at all — so the account service publishes it as
  `offline_after_seconds` in every acknowledgement and `announcer.settle` believes it, within
  limits. The rule in one line: never slower than four announcements inside the published window,
  never faster than the configured interval, and a floor under the derived value so that a
  service answering nonsense cannot turn this into a hot loop. It only ever narrows, which is
  what lets a test shorten the interval and keep it.
- Deliberately not here: a display name (the account service defaults it to the name), any
  discovery of this server's own public address, anything inbound, and any status beyond having
  been heard from.

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
# -world-name and one of the two key flags are required: a server that cannot verify a
# session ticket cannot admit anybody, and refuses to start rather than opening its door.
go run ./cmd/voxelheimd -world-name midgard \
  -account-service https://127.0.0.1:7778 -account-service-fingerprint <64 hex characters>
go run ./cmd/voxelheimd -world-name midgard -ticket-key <64 hex characters>

go run ./cmd/voxelheimd -world-name midgard -ticket-key <key> -listen 0.0.0.0:7777  # reachable from another machine
go run ./cmd/voxelheimd -world-name midgard -ticket-key <key> -listen 127.0.0.1:0   # a free port, printed in the listening line
go run ./cmd/voxelheimd -world-name midgard -ticket-key <key> -seed 42              # a different world; the same seed is the same world
go run ./cmd/voxelheimd -world-name midgard -ticket-key <key> -log-level debug -log-format json

# In the list players choose from. The key is never a flag; announcing is off without all three,
# and a failed announce is logged and survived rather than being a reason not to start.
VOXELHEIM_REGISTRATION_KEY=<key> go run ./cmd/voxelheimd -world-name midgard \
  -account-service https://127.0.0.1:7778 -account-service-fingerprint <sha256> \
  -listen 0.0.0.0:7777 -announce-address <host>:7777
go run ./cmd/voxelheimd -h                                                          # every flag, with the default it actually holds
```

The account service prints the key it publishes at startup, and `GET /v1/ticket-key` serves the
same 64 characters — so `-ticket-key` is one copy of one string, and `-account-service` is that
copy made by the machine. **It prints two numbers, and they are not interchangeable**: the
`public_key` is what a ticket's signature is checked against, and the `certificate_sha256` is
what the connection that fetches it is checked against. The second is the one
`-account-service-fingerprint` takes.

`-h` is the list, deliberately: the defaults are constants in `internal/game`, `internal/world`
and `internal/session`, and a table here restating them would be a copy that drifts. What the
flags decide is the part worth writing down.

| Flag | Decides |
| ---- | ------- |
| `-listen` | the address to bind. A `:0` port binds a free one and the startup line names it |
| `-seed` | the terrain. It is regenerated from the seed, never read from disk |
| `-world-dir` | where edits, player records, the clock and the TLS key are kept. Empty runs an ephemeral world |
| `-world-name` | which world this is. A ticket names one world and is useless at any other, so this is what every ticket presented here must name. **Required** |
| `-account-service` | the `https` base URL to read the signing key from, once, at startup. Mutually exclusive with `-ticket-key`; exactly one is **required** |
| `-account-service-fingerprint` | the SHA-256 of the certificate that service presents, as it logs it. **Required with `-account-service`** and refused without it; there is no way to skip the check |
| `-ticket-key` | that key in hex, when it is copied by hand instead of fetched |
| `-announce-address` | the `host:port` players dial, announced to `-account-service`. **Separate from `-listen`**; announcing is off without it |
| `-registration-key-file` | a file holding the registration key. The key is never a flag; `VOXELHEIM_REGISTRATION_KEY` is the other source, and never both |
| `-tick-rate` | authoritative simulation ticks per second (1..255) |
| `-view-distance` | the chunk streaming radius, in chunks (0..16) |
| `-max-players` | the maximum concurrent sessions (100..1000). A connection past it receives `SERVER_FULL` |
| `-terrain-memory-mib` | the memory budget for resident terrain, charged at 96 KiB per chunk. A budget below one complete working set is refused at startup |
| `-handshake-timeout` | how long a new connection may say nothing before it is closed |
| `-character-timeout` | how long an admitted account may take to choose a character. The one window a person is inside; must be at least the handshake timeout |
| `-idle-timeout` | how long a welcomed session may say nothing. Must be at least the handshake timeout |

| `-log-level` | `debug`, `info`, `warn` or `error` |
| `-log-format` | `text` or `json` |

Every one of them is validated before it is narrowed — see `validate` in `cmd/voxelheimd/main.go`
and the reasoning above it, which is the pattern any second command copies.

### What the world directory holds, and the one thing that surprises people

Its omitted default is `world-v<WorldgenVersion>`, resolved against the working directory, so a
version 12 build launched with `cd server && go run …` writes `server/world-v12/` — git-ignored.
That name is derived by `world.DefaultWorldDir()` from the generator's constant rather than copied
beside the flag. A restart under the same generator version reuses the directory and preserves the
chunk deltas, player records, exploration and marker ledgers, structures, clock and **the server's
TLS key**. A generator bump selects a clean default directory and leaves every earlier version
untouched; this is a development convenience, not migration. An explicit non-empty `-world-dir`
remains exact and retains the fail-closed seed and worldgen checks, while explicit
`-world-dir ""` remains ephemeral. The terrain itself is not stored there; it is a function of
`-seed`.

That last item is why `-world-dir ""` costs more than the edits it discards. A server with
nowhere to keep a key mints a new certificate on every start, and what a client will accept is
stated by the server list rather than remembered from a first connection — so an ephemeral server
is one whose registered fingerprint goes stale the moment it restarts. The server says so in a
startup warning, and reads its own out of the startup line:

```
level=INFO msg="listening with an encrypted session" certificate_sha256=…
```

That value is what the server registers with the account service, and re-registering after a
restart is what makes the new certificate acceptable — see "The list that ends the trust chain".
In development, keeping the default world directory is the way it never comes up.

**This paragraph used to say something else, and the correction is worth reading if you remember
the old shape.** Until #150 the client kept a pin file per address, trusted on first use, and the
documented way through a changed fingerprint was to write the new one into that file by hand.
There is no such file now — `pin_path`, `read_pin` and `write_pin` are gone, deliberately, because
two ways to decide who a server is means the weaker one decides whenever the stronger is
unavailable. Any instruction that ends in "write the fingerprint into the pin file" is describing
a mechanism that no longer exists.

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
  position, yaw, health and all 40 slots with their durability — `game.Life`, captured by
  `Player.Record` and stored by `persist`. What is **not** written is everything that only means
  something inside one connection: the death countdown, the respawn protection window, mining
  progress, a pending swing, the three client-tick ordering guards, and the drops and mobs in the
  world. A returning player therefore arrives with their pack and their health, standing where they
  logged out, settling by falling exactly as a new join does (`onGround` is false either way) — and
  with none of the timers a previous session was part-way through.

  **A record always describes a living player**, which is what makes quitting mid-death neither an
  escape nor a double charge. A player who is dead when their record is taken is written as
  `respawnLocked` would have left them: alive, at `PlayerMaxHealth`, at `respawnPositionLocked` —
  their tent if one stands, the nearest settlement to where they fell if not, the join spawn
  otherwise — with the −20% durability penalty charged if
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
  renames the file to `<id>.bin.corrupt.<nanos>`, and the player is admitted with nothing. The
  timestamped suffix is not decoration: a fixed `.corrupt` would destroy the previous one. A record
  that is *unreachable* rather than corrupt — a permission, a failing disk — still refuses the
  connection, because a retry may succeed and reading it as "no record" would throw away a good life
  on a transient fault.

  **A failed quarantine now refuses the connection too, and that changed with the ticket.** #147's
  answer rested on two things: the file is moved, *and* the identity minted next was a different one
  — fresh random bytes, a different file name — so a failed move cost nothing, because nothing that
  session went on to write could land on the damaged record. A player is named by their account now,
  so the session admitted after a failed move writes to exactly the path whose contents nobody could
  read, and its first teardown would destroy the evidence and the player's only record together. So
  the answer moves to the one an unreachable record already gets: a refusal costs that player one
  connection and an operator one look at a directory, and the alternative cannot be undone.
  `TestResolveRefusesWhenACorruptRecordCannotBeSetAside` is the pin, and it skips rather than
  asserts for a user a read-only directory does not stop.

  **`persist.StoreVersion` is 4 and there is no migration at any step.** v1 held a name and a
  timestamp, which is not enough to reconstruct a life; v2 was keyed by an identity rather than by
  a character; v3 could not say what its character looks like. Nothing has shipped, and
  `CheckHeader` refuses every one of them like any other unknown version. Note that it takes the
  caller's version rather than a package constant, so the player record and the chunk record
  version independently.


  **What identifies a player.** An account, named by a session ticket the account service signed and
  the client presents in its `ClientHello`. The player id is `SHA-256(playerIDDomain ‖ account)`:
  `<world-dir>/players/<player-id-hex>.bin` holds a display name, a last-seen time and the life under
  that digest, so a leaked players directory is a list of digests rather than a list of accounts.
  **This server issues nothing**, which is the whole of what changed — a player is the same player on
  a server that has never seen them, and on one that keeps nothing at all.

  **One live session per account**, refused with `RejectReason.ALREADY_CONNECTED`; the older session
  is never kicked, and `-idle-timeout` is what keeps a dead one from holding a place for long. **What
  a ticket is not**: a password, or anything rotatable or revocable. It is a bearer credential, so
  whatever can read one can present it — a signature proves who *issued* a ticket, not who is holding
  it. What protects it is the transport and nothing in this directory: the session is encrypted with
  no way to ask for otherwise, and the client refuses a server whose certificate is not the one it
  pinned. `schemas/handshake.fbs` states both configurations rather than either alone.

  **A ticket is checked at admission and never again.** `ticket.Lifetime` is eight hours and a
  session may outlive it; nothing disconnects a player whose ticket expires mid-evening. That is
  deliberate rather than overlooked — the alternative is a server that throws people out of the world
  on a timer they cannot see — and it bounds what a stolen ticket buys at one session rather than at
  eight hours. Revocation does not exist at all; `internal/ticket`'s package doc states that cost.

  **In an ephemeral world (`-world-dir ""`) tickets are still verified and accounts are still
  exclusive**, and nothing is written — no record, no life. So a reconnect is the same player at the
  spawn, with nothing they were carrying: what an ephemeral world costs is the life, and no longer the
  name. That is a real change from the V6 model, where an ephemeral world could recognise nobody.
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
  mined break's insertion on the mining worker. Found by the review on legacy PR 89.
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
- **An owner who never comes back leaves a warded camp standing.** The weekly Fimbulvetr
  reclaims every unwarded player structure through chunk regeneration; a camp inside a
  living runestone's column is protected ground and outlives its absent owner by design.
- **No fall damage, no health, no death.** A player who falls a hundred blocks lands and walks
  away. `TerminalFallSpeed` exists to bound the per-tick step, not to model anything.
- **The generator supplies a hydrostatic initial state; runtime flow starts from that base.** A
  carved voxel under a sea, basin or river is generated as source water up to that column's
  standing surface, while a dry-land cave still fills only below `caveWaterLevel`. The answer is
  integer-only and column-local, so the generator remains a pure function and the Fimbulvetr
  storm can still restore the original procedural state by discarding deltas. The palette already
  distinguishes seven flowing levels and four generator-authored currents; scheduling their
  runtime propagation is #594 and is not part of generation. There is still no breath meter or
  underwater damage. **Mobs are not taught to swim**: a creature that walks into a lake sinks and
  walks along the bed, because a mob has a path where a player has intent, and answering the swim
  rules for a path is a change to the pathing. The spawn director keeps that from being the common
  case by refusing every water-family voxel and the ice lid above one.
- **A full water source is not necessarily an isotropic one.** Plain `Water` supplies all four
  horizontal neighbours, as a lake or sea must. A `WaterCurrent*` source supplies only the
  neighbour its id points toward. `world.WaterFeedsToward` is that distinction, and the three
  server readers share it: `riverFallTopAt` paints a generated terrace fall only across the
  higher source's downstream face, `NextWater` accepts side supply only across that face, and
  `FlowDirection` includes the same source in a flowing voxel's gradient. `WaterFlow1..7` has no
  encoded heading and keeps spreading by level. This is deliberately cardinal and local — not a
  finite-volume simulation — but it prevents one river source from feeding upstream and sideways
  curtains while its current carries the swimmer somewhere else.
- **Moving water is a target the swimmer's own target is added to, never a force accumulated
  across ticks.** `FlowDirection` (`internal/game/current.go`) reads one voxel and its five
  neighbours and answers a unit horizontal direction plus a falling flag: a `WaterCurrent*` id
  carries its direction outright, a flowing level derives one from the levels around it, and a
  plain source has none. `Player.step` samples it at the body's centre while `inWater`, adds
  `CurrentSpeed` along it to the input-derived swim target, and eases the velocity toward the sum
  with `SwimAcceleration`. Nothing is stored on the `Player`, so leaving a channel leaves nothing
  behind. `CurrentSpeed` is deliberately under `SwimSpeed`, which is what makes a river something
  a swimmer fights rather than something that wins; a fall pulls toward `WaterfallSinkSpeed`
  unless the rise intent is held, which always wins. **This is server-side because a current that
  moves a body is a gameplay outcome** — a client may mirror the same derivation to animate a
  surface, and the drift it renders is still whichever one this tick computed. Its price, paid
  knowingly: horizontal movement *in water* now has about a fifth of a second of momentum, where
  on land it is still set outright from the intent every tick. **Mobs are untouched** — `physics`
  never reads it, for the reason above.
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
  ahead of the chunk that predates it. Found by review on legacy PR 54; the window is one step wider than
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
- **The client half of the character phase landed in #108, and the gap it closed is worth keeping
  a sentence about.** Between #104 and #108 this server answered every hello with
  `ServerCharacterList` and no client could answer one, so nothing could join and
  `scripts/interop-check.sh` could not pass. That was the sequencing working as intended — the
  client half is written against the server's actual behaviour, which is the position
  `MobKind.Vargr` is still in — but it is also what a gap between two halves of one contract costs
  while it is open, and the only thing that made it survivable was that the client failed by name
  rather than hanging. The check is green again and asserts the phase itself (check 6).
- **A game server can now tell that it reached the right account service**, and #131 is where
  that was closed. The substitution the plaintext hop allowed outlived the attacker, because the
  key is read once and kept: a server holding somebody else's public key admits every ticket they
  mint and refuses every real one, and nothing about it looks wrong. Both hops to that service —
  the key fetch and every announcement — now go over TLS pinned to
  `-account-service-fingerprint`, which the account service prints as `certificate_sha256` at
  every start. **What has not moved is that the root is supplied rather than discovered**: there
  is no trust on first use and no plaintext fallback on either side, because first contact is
  precisely when a substitution happens.
- **Key rotation is still nowhere, and it now costs two sides.** One operator, one pair, and every
  game server holding a copy. Rotating means publishing two keys and a window in which both verify;
  until then, replacing the pair refuses every player on every world at once until each server is
  restarted with the new key. Deleting the pair is the whole of the ceremony.
- **An account can be found by its provider identity and by nothing else.** That is the only
  lookup the account service can perform today: the flow that will call it arrives holding a
  provider identity. Finding an account by its `auth.AccountID` — which is what the rest of the
  game will carry — needs a second index, and it lands with the thing that needs it.
- **`auth.Store.Ensure` is exclusive within one process and not between two.** Its mutex is what
  stops two concurrent requests for one person minting two accounts; two account-service
  processes over one accounts directory would still race, and a mutex cannot reach that. One
  service owns its directory, and the fix if that ever stops being true is a lock in the
  filesystem rather than a wider lock in Go.
