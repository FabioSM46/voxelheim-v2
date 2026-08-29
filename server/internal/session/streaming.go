package session

import (
	"cmp"
	"context"
	"fmt"
	"log/slog"
	"slices"
	"sync"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// View is the set of chunks one session holds, and the radius it holds them in.
//
// Pure bookkeeping: no cache, no connection, no clock. Streaming correctness is
// set arithmetic, and keeping the arithmetic separate from the I/O is what lets a
// table test walk a player across chunk borders and assert the exact messages.
//
// **Guarded, because it has two readers now.** The streaming goroutine that owns this
// view is no longer the only one asking about it: an edit resolved on *another* session's
// goroutine has to know whether this session holds the chunk that changed, and that
// question races the diff. The lock is cheap — streaming runs on chunk crossings, not on
// ticks — and it is the alternative to a second copy of the loaded set going stale.
type View struct {
	radius int32

	mu     sync.Mutex
	center world.Coord
	placed bool
	loaded map[world.Coord]struct{}

	// reveal is told about the column of every chunk this view records as delivered, or
	// nil when nobody is keeping a map. It is the whole of the exploration hook, and it
	// hangs here rather than in Streamer because MarkLoaded is the moment: a chunk is
	// only explored once it has actually reached the client, which is the same
	// distinction MoveTo refuses to blur by marking chunks it has merely scheduled.
	reveal func(world.Column)
}

// NewView returns an empty view with the given radius, in chunks.
func NewView(radius uint8) *View {
	return &View{radius: int32(radius), loaded: make(map[world.Coord]struct{})}
}

// MoveTo recentres the view and reports what changed: the chunks to send, nearest
// first, and the chunks to unload.
//
// Unloads leave the loaded set here, but loads do not enter it — the caller
// confirms each one with MarkLoaded once it has actually been sent. Marking them
// here would be simpler and wrong: a send that fails halfway would leave every
// remaining chunk recorded as delivered, and the client would spend the rest of the
// session missing terrain the server believes it has.
func (v *View) MoveTo(center world.Coord) (load, unload []world.Coord) {
	v.mu.Lock()
	defer v.mu.Unlock()

	// No shortcut for an unchanged centre. Returning early on `center == v.center`
	// would be a cheap optimisation and a correctness bug: after a partial send the
	// centre is unchanged but chunks are still missing, and skipping the diff would
	// leave them missing forever. Standing still already yields two empty lists,
	// because the arithmetic below derives them from what the client actually has.
	visible := v.visibleFrom(center)

	for coord := range visible {
		if _, held := v.loaded[coord]; !held {
			load = append(load, coord)
		}
	}
	for coord := range v.loaded {
		if _, stillVisible := visible[coord]; !stillVisible {
			unload = append(unload, coord)
		}
	}

	// Nearest first, so a player sees the ground under their feet before the
	// horizon. Ties broken by coordinate to make the order deterministic: a stable
	// order is what makes the streaming tests exact instead of approximate.
	slices.SortFunc(load, func(a, b world.Coord) int {
		if d := cmp.Compare(distanceSquared(a, center), distanceSquared(b, center)); d != 0 {
			return d
		}
		return compareCoords(a, b)
	})
	slices.SortFunc(unload, compareCoords)

	for _, coord := range unload {
		delete(v.loaded, coord)
	}
	v.center, v.placed = center, true

	return load, unload
}

// MarkLoaded records that a chunk has reached the client. Until it is called, the
// chunk stays in the send list of every later move — which is what makes a failed
// send recoverable instead of permanently invisible.
func (v *View) MarkLoaded(coord world.Coord) {
	v.mu.Lock()
	v.loaded[coord] = struct{}{}
	reveal := v.reveal
	v.mu.Unlock()

	// **Outside the lock, deliberately.** The set this calls into has a mutex of its
	// own and is read from the session goroutine while this runs on the streaming one;
	// holding two locks in one order here and the other order there is how a deadlock
	// is built out of two correct types. Nothing in the view is read after the release,
	// so there is nothing for the gap to invalidate: a column revealed twice is a
	// no-op, which is the property Exploration.Reveal is built around.
	if reveal != nil {
		reveal(coord.Column())
	}
}

// RecordExploration makes this view report every chunk it records as delivered to
// reveal, and replaces whatever was there before. A nil reveal turns the hook off.
//
// Set once, before the streaming goroutine starts, which is what makes it safe for the
// hook itself to be read under the same lock the loaded set is.
func (v *View) RecordExploration(reveal func(world.Column)) {
	v.mu.Lock()
	defer v.mu.Unlock()
	v.reveal = reveal
}

// Holds reports whether this session has been sent the chunk at coord.
//
// The question a broadcast asks, and it is deliberately about what the client *has*
// rather than about what is within its radius. The two differ for as long as a chunk is
// in flight, and a voxel update for a chunk the client has not received describes terrain
// it cannot place.
func (v *View) Holds(coord world.Coord) bool {
	v.mu.Lock()
	defer v.mu.Unlock()
	_, held := v.loaded[coord]
	return held
}

// Resendable reports whether this session may be sent the chunk at coord again on
// request. The question a ChunkResendRequest asks, and the whole of what the server
// checks before honouring one.
//
// Two conditions, and neither is about the chunk. **Inside the view volume**: a
// coordinate outside it is one this session has been told to unload or was never
// offered, and sending it would hand a client terrain the server does not consider it a
// viewer of. **Currently loaded**: a coordinate the session does not hold is already in
// the next diff's send list, because the diff sends exactly what is missing — so the
// request has nothing to add and the ordinary stream is already the answer.
//
// The first is implied by the second today: MoveTo deletes everything it unloads, so the
// loaded set is always a subset of the visible one. Both are checked anyway. The
// containment is an invariant of a *different* function, and this one is a bound on input
// a client chose — the day the two diverge, the check nobody re-derived is the one that
// must still be here.
func (v *View) Resendable(coord world.Coord) bool {
	v.mu.Lock()
	defer v.mu.Unlock()

	if !v.placed {
		// Nothing has been streamed yet, so there is nothing to have lost.
		return false
	}
	if !withinRadius(coord, v.center, v.radius) {
		return false
	}
	_, held := v.loaded[coord]
	return held
}

// Forget drops a chunk from the loaded set without asking the client to unload it, so the
// next move re-sends it.
//
// The recovery path for an update that could not be delivered. A BlockUpdate is not
// replaced by a later one — unlike a snapshot — so a dropped one leaves the client wrong
// about a voxel for as long as it keeps the chunk. Forgetting the chunk turns that into
// the case streaming already handles: the next diff finds it missing and sends the whole
// composed chunk, edits included.
func (v *View) Forget(coord world.Coord) {
	v.mu.Lock()
	defer v.mu.Unlock()
	delete(v.loaded, coord)
}

// Loaded is how many chunks the view holds.
func (v *View) Loaded() int {
	v.mu.Lock()
	defer v.mu.Unlock()
	return len(v.loaded)
}

// Center is the chunk the view is centred on, and whether it has been placed yet.
func (v *View) Center() (world.Coord, bool) {
	v.mu.Lock()
	defer v.mu.Unlock()
	return v.center, v.placed
}

func (v *View) visibleFrom(center world.Coord) map[world.Coord]struct{} {
	side := 2*v.radius + 1
	visible := make(map[world.Coord]struct{}, side*side*side)

	for y := center.Y - v.radius; y <= center.Y+v.radius; y++ {
		for z := center.Z - v.radius; z <= center.Z+v.radius; z++ {
			for x := center.X - v.radius; x <= center.X+v.radius; x++ {
				visible[world.Coord{X: x, Y: y, Z: z}] = struct{}{}
			}
		}
	}
	return visible
}

// withinRadius reports whether coord is inside the cube of the given radius centred on
// center — the same volume visibleFrom enumerates, asked about one coordinate.
//
// Widened to int64 before subtracting, for the reason compareCoords uses cmp.Compare: the
// difference of two int32 coordinates can overflow, and an overflowed difference is small
// and positive exactly when the true one is enormous. That is a coordinate a client could
// choose.
func withinRadius(coord, center world.Coord, radius int32) bool {
	return axisWithin(coord.X, center.X, radius) &&
		axisWithin(coord.Y, center.Y, radius) &&
		axisWithin(coord.Z, center.Z, radius)
}

func axisWithin(a, b, radius int32) bool {
	d := int64(a) - int64(b)
	if d < 0 {
		d = -d
	}
	return d <= int64(radius)
}

func distanceSquared(a, center world.Coord) int64 {
	dx := int64(a.X) - int64(center.X)
	dy := int64(a.Y) - int64(center.Y)
	dz := int64(a.Z) - int64(center.Z)
	return dx*dx + dy*dy + dz*dz
}

// compareCoords orders by y, then z, then x. cmp.Compare rather than subtraction:
// a difference of two int32 coordinates can overflow, and the sort would then
// order them backwards.
func compareCoords(a, b world.Coord) int {
	if d := cmp.Compare(a.Y, b.Y); d != 0 {
		return d
	}
	if d := cmp.Compare(a.Z, b.Z); d != 0 {
		return d
	}
	return cmp.Compare(a.X, b.X)
}

// Streamer keeps one session's chunks in step with where the server says its
// player is.
type Streamer struct {
	view  *View
	cache *world.Cache
	send  func([]byte) error
	wake  func()
	log   *slog.Logger

	// resends bounds what a client can ask for with ChunkResendRequest. Its own mutex
	// guards it, because Resend runs on the session's read loop while everything else
	// here runs on the streaming goroutine.
	resends *tokenBucket

	// tiles bounds what a client can ask for with MapTileRequest, for the same reason
	// and with the same mutex discipline: DrawMapTile runs on the session's read loop.
	tiles *tokenBucket

	// repairing says that the previous pass ended by giving up on a chunk and asking for
	// this one. Written and read only on the streaming goroutine, which is the single
	// caller of MoveTo — Resend touches the view and the wake, never this.
	repairing bool

	// explored is this character's map ledger, or nil when nobody is keeping one. The
	// streamer holds it so that the columns a pass reveals leave on the same pass that
	// revealed them: the reveal happens inside sendChunk, and this is the first place
	// afterwards that knows the pass is over.
	explored *Exploration

	// entering is told about every chunk that newly enters this view, or nil when nobody
	// is listening. It is the simulation's one chance to learn a piece of the world became
	// visible, which is when the settlement stations standing in it are created —
	// game.Player.MaterialiseSettlements is what the server passes.
	//
	// The load list rather than MarkLoaded, deliberately: a station is an entity a
	// snapshot carries rather than terrain the client is holding, so waiting for the send
	// would leave a village forge missing while a failed chunk was retried.
	entering func(world.Coord)
}

// NewStreamer returns a streamer for one session.
//
// send is expected to enqueue a frame and return an error only when the session is
// finished.
//
// wake must ask for one more view diff at the *current* centre, and it is what makes a
// repair reach a player who is not moving: streaming is driven by chunk crossings, so
// "the next diff" is otherwise not a time at all. game.Player.WakeStreaming is the one
// the server passes; it rings the doorbell the tick loop already rings.
//
// now is the clock this session's rate limits refill against — the resend bucket and
// the map-tile bucket both. Injected rather than read from
// time.Now for the reason game.Clock exists: a test has to be able to spend a bucket
// without spending a second, and a bound nothing can test is a bound nobody can change
// safely.
func NewStreamer(cache *world.Cache, radius uint8, send func([]byte) error, wake func(), now func() time.Time, log *slog.Logger) *Streamer {
	return &Streamer{
		view:    NewView(radius),
		cache:   cache,
		send:    send,
		wake:    wake,
		log:     log,
		resends: newResendLimiter(radius, now),
		tiles:   newMapTileLimiter(now),
	}
}

// Resend answers one ChunkResendRequest: it forgets the chunk so the next diff sends it
// whole, and asks for that diff now instead of at the player's next chunk crossing.
//
// **The client is asking for data it lost, never for an outcome.** What it may hold is
// still the server's answer — Resendable decides — and what the chunk contains is
// whatever the world says now, composed fresh, rather than anything the request
// describes. A refusal is silence, exactly as a refused edit is: the returned error is
// for the caller's log line and never for the wire, and nil means the repair is
// scheduled.
//
// The two refusals are asked in this order deliberately. A coordinate this session cannot
// be sent costs a mutex and a map lookup and is refused before the limiter sees it, so a
// client cannot empty its own bucket on nonsense and then find itself unable to ask for
// the chunk it really lost.
func (s *Streamer) Resend(coord world.Coord) error {
	if !s.view.Resendable(coord) {
		return fmt.Errorf("chunk %+v is not one this session holds in view", coord)
	}
	if !s.resends.allow() {
		return fmt.Errorf("resend budget for chunk %+v is spent", coord)
	}

	s.repair(coord)
	return nil
}

// repair is the whole recovery, and it has exactly two callers because they are the same
// loss: a chunk this session should be holding that no ordinary event will re-send soon
// enough. A client that dropped one is one caller; sendChunk giving up on one is the
// other.
//
// Forget is the mechanism, and it was already here — the next diff finds the coordinate
// missing and sends the composed chunk whole, edits included. The wake is the half that
// was missing. Together they are a complete repair *at the current centre*, which is
// exactly what View.MoveTo's refusal to shortcut an unchanged centre exists to make
// correct: do not add that shortcut, and read its comment before trying.
func (s *Streamer) repair(coord world.Coord) {
	s.view.Forget(coord)
	s.wake()
}

// MoveTo streams whatever the move made visible and unloads whatever it made
// invisible.
//
// Unloads go first: the client frees the chunks it no longer needs before the new
// ones arrive, so its resident set never peaks at both radii at once.
func (s *Streamer) MoveTo(ctx context.Context, center world.Coord) error {
	// Whether this pass is itself the follow-up a give-up asked for, read before anything
	// in it can ask again. sendChunk is the only writer and the bound it enforces is
	// there: a repair pass that gives up too does not ask for a third.
	repairing := s.repairing
	s.repairing = false

	load, unload := s.view.MoveTo(center)

	for _, coord := range unload {
		if err := s.send(protocol.EncodeChunkUnload(toProtocolCoord(coord))); err != nil {
			return fmt.Errorf("session: unload chunk %+v: %w", coord, err)
		}
	}

	for _, coord := range load {
		// Before the send, and before anything that can fail. What this reports is that a
		// chunk has entered the view, which is true the moment the diff said so; the
		// simulation's answer to it is an entity the next snapshot carries, and a
		// snapshot does not wait on terrain.
		if s.entering != nil {
			s.entering(coord)
		}
		if err := s.sendChunk(ctx, coord, repairing); err != nil {
			return err
		}
	}

	// Whatever those sends revealed, in one message rather than one per chunk: a view
	// diff at the default distance delivers up to 343 chunks and the columns under them
	// are at most 49, so batching is the difference between one frame and hundreds.
	if err := s.sendExplored(); err != nil {
		return err
	}

	if len(load) > 0 || len(unload) > 0 {
		s.log.Debug("view updated", "center", center, "sent", len(load), "unloaded", len(unload), "resident", s.view.Loaded())
	}
	return nil
}

// maxChunkResends bounds how many times one chunk is re-sent because an edit landed
// while it was in flight. Each pass is one clone-sized frame, and the second pass runs
// with the chunk already marked held, so from then on every further edit reaches this
// session as a BlockUpdate anyway — the retry is closing the first window, not chasing
// a moving target. Two is therefore generous rather than tuned.
const maxChunkResends = 2

// sendChunk streams one chunk and confirms it, re-sending when an edit landed while it
// was in flight.
//
// **Why the re-send exists.** Between reading the chunk and MarkLoaded, View.Holds
// reports false for this session, so Registry.BroadcastChunk skips it: an edit accepted
// in that window reaches every other session and not this one. The client is then left
// holding terrain the server has already changed, with no update coming — and because
// the recovery path is the next view diff, a player standing still keeps the wrong
// voxels for as long as they stand there.
//
// **Why comparing the chunk pointer is the test.** An edit does not mutate a published
// chunk, it publishes a clone (world.Cache.Apply). So a pointer that is still the one
// sent is proof that nothing was missed, per chunk — where Cache.Revision counts edits
// anywhere in the world and would re-send the whole stream because someone dug a hole a
// kilometre away.
//
// Found by review on legacy PR 54. The window is one step wider than the finding described: it
// opens at the read, not at the send, because a chunk edited between the two is sent in
// its pre-edit form and skipped by the broadcast just the same.
func (s *Streamer) sendChunk(ctx context.Context, coord world.Coord, repairing bool) error {
	for attempt := 0; ; attempt++ {
		chunk, runs, err := s.cache.Get(ctx, coord)
		if err != nil {
			return fmt.Errorf("session: generate chunk %+v: %w", coord, err)
		}
		if err := s.send(protocol.EncodeChunkData(toProtocolCoord(coord), runs)); err != nil {
			return fmt.Errorf("session: send chunk %+v: %w", coord, err)
		}
		// Only now is it the client's. Everything not marked here is re-sent by the
		// next move, so a session interrupted mid-stream recovers instead of ending up
		// with holes the server thinks it filled.
		s.view.MarkLoaded(coord)

		current, err := s.cache.Peek(coord)
		if err != nil || current == chunk {
			// Peek failing means the chunk was evicted, which already forces a fresh
			// read on any later use — there is nothing stale left to correct.
			return nil
		}
		if attempt >= maxChunkResends {
			// Out of re-sends. Hand it back to the recovery streaming already has rather
			// than looping: Forget makes the next diff send the composed chunk whole.
			//
			// **The stationary player this comment used to record as a known gap is served
			// now.** repair asks for that diff instead of waiting for the player to walk
			// somewhere, and it is the same call a client's own ChunkResendRequest makes.
			//
			// **Once, though.** A pass that is already the follow-up and gives up on the
			// same chunk does not ask for another: a chunk being edited faster than this
			// session can be sent it is not a race a fourth pass wins, and asking anyway
			// is the re-send storm — every pass three more frames into a queue that blocks
			// at the client's own read rate, and nothing else in the view ever sent. It
			// still forgets the chunk, because a client holding terrain the server has
			// changed is the loss worth taking either way; what it gives up on is being
			// the thing that repairs it. The next crossing does, and so does the client,
			// which is the whole point of it being able to ask.
			if repairing {
				s.view.Forget(coord)
			} else {
				s.repairing = true
				s.repair(coord)
			}
			s.log.Debug("chunk edited faster than it could be sent",
				"coord", coord,
				"attempts", attempt+1,
				"repair_requested", !repairing,
			)
			return nil
		}
	}
}

// RecordExploration attaches a character's map ledger to this streamer: the view
// reports every delivered chunk's column to it, and every view diff that revealed
// something sends one MapExplored batch.
//
// Separate from [NewStreamer] rather than a seventh argument, because a ledger belongs
// to a *character* and a streamer is built one line before the session has finished
// assembling one. It is called once, on the session goroutine, before the streaming
// goroutine starts — which is the ordering that makes both fields safe to read without
// a lock of the streamer's own.
func (s *Streamer) RecordExploration(explored *Exploration) {
	s.explored = explored
	s.view.RecordExploration(explored.Reveal)
}

// ReportEntering makes this streamer tell entering about every chunk that newly enters
// the view, and replaces whatever was there before. A nil hook turns it off.
//
// Separate from [NewStreamer] for [Streamer.RecordExploration]'s reason and set on the
// same line of the session's assembly, before the streaming goroutine starts — which is
// the ordering that makes the field safe to read with no lock of the streamer's own.
func (s *Streamer) ReportEntering(entering func(world.Coord)) {
	s.entering = entering
}

// sendExplored puts this pass's newly revealed columns on the wire, and sends nothing
// when the pass revealed none — which is every pass a player who is standing still
// produces.
//
// After the chunks rather than before them, because a column is revealed by a chunk
// being delivered and a client told about a column it has no terrain for has learned
// something it cannot draw yet. After the unloads too, which costs nothing: a column
// leaving the view is still explored, for good.
func (s *Streamer) sendExplored() error {
	batch := s.explored.TakeRevealed()
	if len(batch) == 0 {
		return nil
	}
	if err := sendExplored(s.send, batch); err != nil {
		return fmt.Errorf("session: send %d newly explored columns: %w", len(batch), err)
	}
	return nil
}

// View exposes the streamer's chunk bookkeeping, for tests and for the movement
// code that will drive it.
func (s *Streamer) View() *View { return s.view }

func toProtocolCoord(c world.Coord) protocol.ChunkCoord {
	return protocol.ChunkCoord{X: c.X, Y: c.Y, Z: c.Z}
}

// fromProtocolCoord is the other direction, for the one message that carries a chunk
// coordinate *into* the server.
func fromProtocolCoord(c protocol.ChunkCoord) world.Coord {
	return world.Coord{X: c.X, Y: c.Y, Z: c.Z}
}

// -----------------------------------------------------------------------------
// Bounding what a client may ask for.
// -----------------------------------------------------------------------------

// resendRefillPerSecond is the sustained rate, in chunks per second, at which one session
// may ask for chunks it has lost.
//
// **Derived, and derived from the server's own numbers.** Sizing it from anything the
// client reports would hand the party being bounded the job of setting its own ceiling —
// the same rule MAX_DECODE_BACKLOG follows on the other side of the wire, where it
// refuses to size itself from ServerWelcome.view_distance.
//
// The quantity worth bounding is how fast a session can make the server compose a chunk.
// The unasked-for rate at which that already happens is set by chunk crossings: a view
// diff runs when the player enters a chunk they were not in, and the fastest a player can
// do that is a terminal-velocity fall — TerminalFallSpeed blocks per second through
// chunks ChunkSize blocks tall, which is 60/32 = 1.875 crossings a second. That is the
// speed at which the world can legitimately move under a session, and a client asking for
// chunks faster than the world can move under it is not repairing a hole, it is
// streaming.
//
// It sits far below what the server already spends on that falling player, and the gap is
// the margin: each of those crossings streams a whole new face of the view cube, (2r+1)²
// chunks — 49 at the default view distance of 3, about 92 chunks a second — where this
// allows fewer than two. Ordinary play never comes near it, because ordinary play loses
// no chunks at all.
const resendRefillPerSecond = game.TerminalFallSpeed / world.ChunkSize

// tokenBucket is the shape every per-session rate limit in this package has: a
// capacity, a sustained refill rate, and a clock to refill against.
//
// **One mechanism, and each rate's derivation stays with the constructor that sets
// it.** Refilling from elapsed time, capping at the capacity and spending one token or
// refusing is the same arithmetic for every request a client may repeat; what differs
// is the pair of numbers, and a number is only defensible beside the sentence that
// derives it. So this type carries the arithmetic and [newResendLimiter] and
// [newMapTileLimiter] carry the arguments.
type tokenBucket struct {
	capacity float64
	refill   float64
	now      func() time.Time

	mu     sync.Mutex
	tokens float64
	last   time.Time
}

// newTokenBucket returns a full bucket of capacity tokens, refilling at refill tokens
// a second.
//
// Full rather than empty for every caller: an empty bucket refuses a session's first
// request until one token has accrued, which bounds the join rather than the abuse.
func newTokenBucket(capacity, refill float64, now func() time.Time) *tokenBucket {
	return &tokenBucket{
		capacity: capacity,
		refill:   refill,
		now:      now,
		tokens:   capacity,
		last:     now(),
	}
}

// The resend limiter bounds ChunkResendRequest, per session.
//
// **Why the request is bounded at all.** A resend is a cache read and a frame in the
// cheap case. In the expensive one the server's chunk cache has evicted the chunk since
// it was sent — it holds world.DefaultCacheCapacity of them — so Cache.Get *regenerates*
// it, which is the millisecond-scale part of this server and runs under a semaphore every
// session shares. That is one client making every other client's terrain late. Trusting
// the client on what it asks for is not the same as trusting it on how often, which is
// the sentence legacy PR 50 wrote about this server with the parties the other way round.
//
// **A bucket rather than a minimum interval, because the honest traffic is bursty and the
// dishonest traffic is not.** A client sheds chunks when it cannot keep up, so it loses
// them in batches; an interval would refuse the batch, and the contract gives a client no
// retry, so a refused request is a permanent hole rather than a delayed one. A bucket
// serves the batch and then throttles.
//
// **The burst is one view volume**, (2r+1)³ chunks at the session's own view distance.
// That is the largest honest need there is — a client that has dropped everything it
// holds has exactly that many chunks to ask for, and cannot usefully ask for more because
// Resendable refuses a coordinate outside the volume before it costs anything — and it is
// work the server has already agreed to do for this session once, since a join streams
// precisely this volume. So a full bucket buys the worst client one extra join, and after
// that the refill rate is all it has.
//
// This is the first rate limit in this repository; world chat is the second and the map
// tile bucket below is the third. What is
// bounded here is *chunk work* a client can ask for, not the text messages it can send.
// A request refused by Resendable costs a mutex and a map lookup and spends nothing,
// deliberately. The remaining socket-level backpressure gaps are recorded in
// server/AGENTS.md.
//
// Full rather than empty: a client that has just joined holds no chunks, so every request
// it could make is refused by Resendable anyway. Starting empty would only delay the
// first honest repair of a session that had been running long enough to have something to
// repair.
func newResendLimiter(radius uint8, now func() time.Time) *tokenBucket {
	side := 2*float64(radius) + 1
	volume := side * side * side

	return newTokenBucket(volume, resendRefillPerSecond, now)
}

// mapTileRefillPerSecond is the sustained rate at which one session may ask the server
// to draw it a square of the map, and mapTileBurst is how many it may ask for at once.
//
// **Derived from what a client legitimately needs, not from what the server can bear.**
// A map is a fixed grid of 64×64-pixel tiles, so the honest need is "the tiles the
// window shows, once" plus "the tiles a pan brings into it". A full-screen map at scale
// 16 covers a handful of tiles; opening the map and dragging it across a continent
// touches a few a second. Eight a second serves that with room to spare, and it is
// still two orders of magnitude below what a client could ask for if it simply sent
// requests as fast as the socket accepts them.
//
// **The burst is what one opening of the map costs.** Thirty-two tiles is a 8×4 grid of
// them, more than any window shows at once, so a client that opens the map cold never
// meets the limit; what it bounds is the client that keeps asking after the map is
// already drawn.
//
// **An empty bucket drops the request in silence, which is the chunk-resend precedent
// and not the refusal channel.** A refusal names something the player did wrong, and
// asking for a tile too often is not that — the contract's one refusal here is
// TileMisaligned, which is a malformed request rather than an impatient one. A dropped
// tile costs the client a redraw of a square it will ask for again, and nothing else.
//
// The work being bounded is real but bounded on its own: a tile is 4096 evaluations of
// the height field on the session's own goroutine, taking no lock and touching no chunk
// cache, so a client spending its whole bucket delays nobody's terrain but its own.
// That is why the number can be generous where resendRefillPerSecond could not be.
const (
	mapTileRefillPerSecond = 8
	mapTileBurst           = 32
)

// newMapTileLimiter returns this session's full map-tile bucket.
func newMapTileLimiter(now func() time.Time) *tokenBucket {
	return newTokenBucket(mapTileBurst, mapTileRefillPerSecond, now)
}

// allow spends one token if the bucket has one, and reports whether it did.
func (l *tokenBucket) allow() bool {
	l.mu.Lock()
	defer l.mu.Unlock()

	// Refilled from elapsed time rather than by a timer, so an idle session costs nothing
	// to keep — and capped, so the idle hour does not become an hour's worth of credit.
	// The guard is for a clock that does not advance: a test's does not, and time.Now's
	// monotonic reading cannot go backwards.
	now := l.now()
	if elapsed := now.Sub(l.last); elapsed > 0 {
		l.tokens = min(l.capacity, l.tokens+elapsed.Seconds()*l.refill)
		l.last = now
	}

	if l.tokens < 1 {
		return false
	}
	l.tokens--
	return true
}
