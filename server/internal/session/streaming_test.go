package session_test

import (
	"bytes"
	"context"
	"errors"
	"math"
	"sync"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func TestViewLoadsTheWholeRadiusNearestFirst(t *testing.T) {
	t.Parallel()

	view := session.NewView(1)
	load, unload := view.MoveTo(world.Coord{X: 5, Y: 2, Z: -3})

	if len(load) != 27 {
		t.Fatalf("radius 1 loaded %d chunks, want 27", len(load))
	}
	if len(unload) != 0 {
		t.Errorf("the first move unloaded %d chunks", len(unload))
	}
	if load[0] != (world.Coord{X: 5, Y: 2, Z: -3}) {
		t.Errorf("the first chunk sent is %+v, want the one the player is standing in", load[0])
	}

	// Nearest first: distances must never decrease along the list, or a player waits
	// for the horizon before seeing the ground.
	previous := int64(-1)
	for _, coord := range load {
		d := squaredDistance(coord, world.Coord{X: 5, Y: 2, Z: -3})
		if d < previous {
			t.Fatalf("chunk %+v at distance² %d came after distance² %d", coord, d, previous)
		}
		previous = d
	}
}

func TestViewMoveLoadsAndUnloadsExactSets(t *testing.T) {
	t.Parallel()

	view := session.NewView(1)
	center := world.Coord{}
	for _, coord := range must(view.MoveTo(center)) {
		view.MarkLoaded(coord)
	}

	load, unload := view.MoveTo(world.Coord{X: 1})

	// Stepping one chunk east: the 3×3 slab at x = +2 arrives, the slab at x = -1
	// leaves, and the 18 chunks in between are untouched.
	if len(load) != 9 {
		t.Errorf("loaded %d chunks, want the 9 that became visible: %+v", len(load), load)
	}
	if len(unload) != 9 {
		t.Errorf("unloaded %d chunks, want the 9 that left: %+v", len(unload), unload)
	}
	for _, coord := range load {
		if coord.X != 2 {
			t.Errorf("chunk %+v was loaded, but only the x=2 slab became visible", coord)
		}
	}
	for _, coord := range unload {
		if coord.X != -1 {
			t.Errorf("chunk %+v was unloaded, but only the x=-1 slab left", coord)
		}
	}
	if got := view.Loaded(); got != 18 {
		t.Errorf("the view holds %d confirmed chunks, want the 18 that never left", got)
	}
}

// The property that matters along a path is not "sent at most once" — a chunk that
// left the radius and came back must be re-sent, because the client dropped it.
// It is "never sent to a client that already has it", which is what keeps a player
// pacing a chunk border from re-downloading terrain.
func TestViewNeverSendsAChunkTheClientAlreadyHas(t *testing.T) {
	t.Parallel()

	view := session.NewView(2)
	resident := make(map[world.Coord]struct{})
	resent := 0

	path := []world.Coord{
		{}, {X: 1}, {X: 2}, {X: 2, Z: 1}, {X: 2, Z: 2}, {X: 1, Z: 2}, {Z: 2}, {}, {Y: 1},
	}
	for step, center := range path {
		load, unload := view.MoveTo(center)

		for _, coord := range unload {
			if _, held := resident[coord]; !held {
				t.Errorf("step %d unloaded %+v, which the client did not have", step, coord)
			}
			delete(resident, coord)
		}
		for _, coord := range load {
			if _, held := resident[coord]; held {
				t.Errorf("step %d re-sent %+v while the client still had it", step, coord)
			}
			resident[coord] = struct{}{}
			view.MarkLoaded(coord)
			resent++
		}
	}

	// Sanity: the walk really did cross borders, so the assertions above had
	// something to check.
	if resent <= 125 {
		t.Errorf("the path only ever sent %d chunks; it cannot have crossed a border", resent)
	}
	if got := view.Loaded(); got != len(resident) {
		t.Errorf("the view holds %d chunks, the client model holds %d", got, len(resident))
	}
}

// Returning to a chunk that was unloaded must re-send it: the client dropped it.
func TestViewResendsChunksThatWereUnloaded(t *testing.T) {
	t.Parallel()

	view := session.NewView(0) // one chunk, so the sets are unambiguous
	for _, coord := range must(view.MoveTo(world.Coord{})) {
		view.MarkLoaded(coord)
	}
	if _, unload := view.MoveTo(world.Coord{X: 10}); len(unload) != 1 {
		t.Fatalf("moving away unloaded %d chunks, want 1", len(unload))
	}

	load, _ := view.MoveTo(world.Coord{})
	if len(load) != 1 || load[0] != (world.Coord{}) {
		t.Errorf("coming back loaded %+v, want the chunk that had been dropped", load)
	}
}

func TestViewIsANoOpWhenTheCenterDoesNotChange(t *testing.T) {
	t.Parallel()

	view := session.NewView(1)
	for _, coord := range must(view.MoveTo(world.Coord{})) {
		view.MarkLoaded(coord)
	}

	load, unload := view.MoveTo(world.Coord{})
	if len(load) != 0 || len(unload) != 0 {
		t.Errorf("standing still produced %d loads and %d unloads", len(load), len(unload))
	}
}

func TestStreamerSendsUnloadsBeforeChunks(t *testing.T) {
	t.Parallel()

	var frames [][]byte
	streamer := testStreamer(world.NewCache(5, 2, 128), 1,
		func(frame []byte) error { frames = append(frames, frame); return nil },
	)

	ctx := context.Background()
	if err := streamer.MoveTo(ctx, world.Coord{Y: 2}); err != nil {
		t.Fatalf("MoveTo: %v", err)
	}
	if len(frames) != 27 {
		t.Fatalf("the first move sent %d frames, want 27", len(frames))
	}
	for i, frame := range frames {
		if kind, _ := classify(t, frame); kind != vnet.PayloadChunkData {
			t.Fatalf("frame %d is %s, want ChunkData", i, kind)
		}
	}

	frames = nil
	if err := streamer.MoveTo(ctx, world.Coord{X: 1, Y: 2}); err != nil {
		t.Fatalf("MoveTo: %v", err)
	}
	if len(frames) != 18 {
		t.Fatalf("the second move sent %d frames, want 9 unloads and 9 chunks", len(frames))
	}
	// Unloads first: the client frees what it no longer needs before the new chunks
	// arrive, so its resident set never peaks at both radii at once.
	for i, frame := range frames {
		kind, coord := classify(t, frame)
		want := vnet.PayloadChunkData
		if i < 9 {
			want = vnet.PayloadChunkUnload
		}
		if kind != want {
			t.Fatalf("frame %d is %s, want %s", i, kind, want)
		}
		if i < 9 && coord.X != -1 {
			t.Errorf("frame %d unloads %+v, but only the x=-1 slab left", i, coord)
		}
		if i >= 9 && coord.X != 2 {
			t.Errorf("frame %d sends %+v, but only the x=2 slab became visible", i, coord)
		}
	}
}

// A chunk that failed to send must not be recorded as delivered, or the client
// spends the session missing terrain the server believes it has.
func TestStreamerDoesNotRecordChunksItCouldNotSend(t *testing.T) {
	t.Parallel()

	failAfter := 3
	sent := 0
	sendErr := errors.New("connection gone")

	streamer := testStreamer(world.NewCache(5, 2, 128), 1,
		func([]byte) error {
			sent++
			if sent > failAfter {
				return sendErr
			}
			return nil
		},
	)

	err := streamer.MoveTo(context.Background(), world.Coord{})
	if !errors.Is(err, sendErr) {
		t.Fatalf("MoveTo error = %v, want the send failure", err)
	}
	if got := streamer.View().Loaded(); got != failAfter {
		t.Errorf("the view recorded %d chunks, want the %d that were actually sent", got, failAfter)
	}

	// The chunks that never arrived must be offered again.
	load, _ := streamer.View().MoveTo(world.Coord{})
	if len(load) != 27-failAfter {
		t.Errorf("a retry offered %d chunks, want the %d that never arrived", len(load), 27-failAfter)
	}
}

func TestStreamerStopsOnContextCancellation(t *testing.T) {
	t.Parallel()

	ctx, cancel := context.WithCancel(context.Background())
	cancel()

	streamer := testStreamer(world.NewCache(5, 1, 8), 1, func([]byte) error { return nil })
	if err := streamer.MoveTo(ctx, world.Coord{}); err == nil {
		t.Fatal("MoveTo ignored a cancelled context")
	}
}

func classify(t *testing.T, frame []byte) (vnet.Payload, world.Coord) {
	t.Helper()

	env := vnet.GetRootAsEnvelope(frame, 0)
	kind := env.PayloadType()
	table := payloadTable(t, env)

	switch kind {
	case vnet.PayloadChunkData:
		data := new(vnet.ChunkData)
		data.Init(table.Bytes, table.Pos)
		return kind, toWorldCoord(data.Coord(nil))
	case vnet.PayloadChunkUnload:
		unload := new(vnet.ChunkUnload)
		unload.Init(table.Bytes, table.Pos)
		return kind, toWorldCoord(unload.Coord(nil))
	default:
		return kind, world.Coord{}
	}
}

func toWorldCoord(c *vnet.ChunkCoord) world.Coord {
	if c == nil {
		return world.Coord{}
	}
	return world.Coord{X: c.Cx(), Y: c.Cy(), Z: c.Cz()}
}

func squaredDistance(a, center world.Coord) int64 {
	dx, dy, dz := int64(a.X-center.X), int64(a.Y-center.Y), int64(a.Z-center.Z)
	return dx*dx + dy*dy + dz*dz
}

// must unwraps the load list of a first move, where there is nothing to unload.
func must(load, unload []world.Coord) []world.Coord {
	if len(unload) != 0 {
		panic("unexpected unloads on a first move")
	}
	return load
}

// TestAChunkEditedWhileInFlightIsSentAgain pins the window review found on legacy PR 54.
//
// Between reading a chunk and MarkLoaded, View.Holds is false for this session, so
// Registry.BroadcastChunk skips it. An edit accepted in that window therefore reaches
// every other session and not this one, and the client keeps terrain the server has
// already changed — until the next view diff, which for a player standing still may
// never come.
//
// The send callback is the window: it runs after the chunk was read and before it is
// marked, which is exactly where a concurrent edit lands.
func TestAChunkEditedWhileInFlightIsSentAgain(t *testing.T) {
	t.Parallel()

	ctx := context.Background()
	chunks := world.NewCache(5, 1, 8)
	coord := world.Coord{}

	var frames [][]byte
	var edited bool
	streamer := testStreamer(chunks, 0, func(frame []byte) error {
		frames = append(frames, frame)
		if !edited {
			edited = true
			if err := chunks.Apply(ctx, 5, 6, 7, world.Snow, nil); err != nil {
				t.Errorf("Apply during send: %v", err)
			}
		}
		return nil
	})

	if err := streamer.MoveTo(ctx, coord); err != nil {
		t.Fatalf("MoveTo: %v", err)
	}

	if len(frames) != 2 {
		t.Fatalf("sent %d frames, want 2: the chunk changed while in flight and must be sent again", len(frames))
	}
	if bytes.Equal(frames[0], frames[1]) {
		t.Fatal("the re-sent frame is identical to the first, so it does not carry the edit")
	}
	// The re-send must not cost the client the chunk: Forget here would hand the fix
	// back to the recovery path it exists to avoid depending on.
	if !streamer.View().Holds(coord) {
		t.Fatal("the chunk was forgotten rather than re-sent")
	}
}

// TestAnUneditedChunkIsSentOnce is the other half: the re-send must be triggered by an
// edit, not by every send. Without it, the test above would pass on code that simply
// sends everything twice.
func TestAnUneditedChunkIsSentOnce(t *testing.T) {
	t.Parallel()

	var frames [][]byte
	streamer := testStreamer(world.NewCache(5, 1, 8), 0,
		func(frame []byte) error { frames = append(frames, frame); return nil },
	)

	if err := streamer.MoveTo(context.Background(), world.Coord{}); err != nil {
		t.Fatalf("MoveTo: %v", err)
	}
	if len(frames) != 1 {
		t.Fatalf("sent %d frames for an untouched chunk, want 1", len(frames))
	}
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// testStreamer builds a streamer for a test that has nothing to say about repairs: the
// wake is a no-op and the clock is the real one, which the resend bucket only ever
// consults when somebody asks for a resend. The repair tests below build their own.
func testStreamer(cache *world.Cache, radius uint8, send func([]byte) error) *session.Streamer {
	return session.NewStreamer(cache, radius, send, func() {}, time.Now, discard())
}

// frozenClock only moves when a test moves it, so a token bucket can be spent without a
// second passing and refilled without waiting one. The mutex is for the streamer's own
// discipline rather than for these tests: the limiter takes a lock around the clock read
// because Resend runs on a different goroutine from everything else in the streamer.
type frozenClock struct {
	mu  sync.Mutex
	now time.Time
}

func newFrozenClock() *frozenClock {
	return &frozenClock{now: time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)}
}

func (c *frozenClock) Now() time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.now
}

func (c *frozenClock) advance(d time.Duration) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.now = c.now.Add(d)
}

// wakeCounter stands in for game.Player.WakeStreaming: it records that somebody asked the
// streaming loop for another diff, which is the half of a repair a view cannot show.
type wakeCounter struct {
	mu sync.Mutex
	n  int
}

func (w *wakeCounter) wake() {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.n++
}

func (w *wakeCounter) count() int {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.n
}

// ---------------------------------------------------------------------------
// Asking for a chunk back
// ---------------------------------------------------------------------------

// What the server checks before it honours a ChunkResendRequest, at the level the rule is
// written. Both conditions are about the *session* and neither is about the chunk: a
// coordinate outside the view volume is one this client is not a viewer of, and one it
// does not hold is already in the next diff's send list.
//
// The two differ in exactly one row — a coordinate marked loaded from outside the cube,
// which MoveTo cannot produce and MarkLoaded can. That row is why both checks exist.
func TestResendableAnswersOnlyForChunksTheSessionHoldsInView(t *testing.T) {
	t.Parallel()

	center := world.Coord{X: 4, Y: 0, Z: -2}

	cases := map[string]struct {
		coord  world.Coord
		loaded bool
		placed bool
		want   bool
	}{
		"held, at the centre":               {coord: center, loaded: true, placed: true, want: true},
		"held, in the corner of the cube":   {coord: world.Coord{X: 5, Y: 1, Z: -1}, loaded: true, placed: true, want: true},
		"inside the cube but never sent":    {coord: world.Coord{X: 5, Y: 0, Z: -2}, loaded: false, placed: true, want: false},
		"one chunk outside the cube":        {coord: world.Coord{X: 6, Y: 0, Z: -2}, loaded: true, placed: true, want: false},
		"far outside the cube":              {coord: world.Coord{X: 900, Y: 0, Z: -2}, loaded: true, placed: true, want: false},
		"a coordinate that overflows int32": {coord: world.Coord{X: math.MinInt32, Y: math.MinInt32, Z: math.MinInt32}, loaded: true, placed: true, want: false},
		"nothing streamed yet":              {coord: center, loaded: true, placed: false, want: false},
	}

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			view := session.NewView(1)
			if tc.placed {
				view.MoveTo(center)
			}
			if tc.loaded {
				view.MarkLoaded(tc.coord)
			}

			if got := view.Resendable(tc.coord); got != tc.want {
				t.Errorf("Resendable(%+v) = %v, want %v", tc.coord, got, tc.want)
			}
		})
	}
}

// The repair itself, for a player who is standing still — which is the whole reason the
// message exists. Forgetting the chunk is the mechanism and it was already here; asking
// for the diff is the half that was missing, because diffs run on chunk crossings and a
// player who is not moving makes none.
func TestResendForgetsTheChunkAndAsksForTheDiffThatSendsIt(t *testing.T) {
	t.Parallel()

	ctx := context.Background()
	coord := world.Coord{}
	wake := &wakeCounter{}

	var frames [][]byte
	streamer := session.NewStreamer(world.NewCache(5, 1, 8), 0,
		func(frame []byte) error { frames = append(frames, frame); return nil },
		wake.wake, newFrozenClock().Now, discard(),
	)

	if err := streamer.MoveTo(ctx, coord); err != nil {
		t.Fatalf("MoveTo: %v", err)
	}
	if len(frames) != 1 {
		t.Fatalf("the first diff sent %d frames, want 1", len(frames))
	}

	if err := streamer.Resend(coord); err != nil {
		t.Fatalf("Resend refused a chunk the session holds: %v", err)
	}
	if streamer.View().Holds(coord) {
		t.Error("the chunk is still recorded as delivered, so no diff would ever send it again")
	}
	if got := wake.count(); got != 1 {
		t.Fatalf("the streaming loop was woken %d times, want 1", got)
	}

	// What the wake buys. The centre has not moved — the player has not moved — and the
	// diff sends the chunk anyway, because View.MoveTo deliberately has no shortcut for an
	// unchanged centre.
	if err := streamer.MoveTo(ctx, coord); err != nil {
		t.Fatalf("MoveTo after the repair: %v", err)
	}
	if len(frames) != 2 {
		t.Fatalf("the repair diff sent %d frames in total, want 2: the chunk was never re-sent", len(frames))
	}
	if !streamer.View().Holds(coord) {
		t.Error("the re-sent chunk was not recorded as delivered")
	}
}

// A refused request is silence, and it must also be *nothing else*: a client that asks for
// a chunk it may not have must not thereby lose one it does have.
func TestResendRefusesQuietlyAndCostsTheSessionNothing(t *testing.T) {
	t.Parallel()

	ctx := context.Background()
	held := world.Coord{}
	outside := world.Coord{X: 40}
	wake := &wakeCounter{}

	streamer := session.NewStreamer(world.NewCache(5, 1, 8), 0,
		func([]byte) error { return nil }, wake.wake, newFrozenClock().Now, discard(),
	)
	if err := streamer.MoveTo(ctx, held); err != nil {
		t.Fatalf("MoveTo: %v", err)
	}
	// Loaded from outside the view volume, which only MarkLoaded can arrange: it is the
	// one state in which the two halves of Resendable disagree.
	streamer.View().MarkLoaded(outside)

	for name, coord := range map[string]world.Coord{
		"a chunk outside the view volume": outside,
		"a chunk this session never had":  {X: 1},
	} {
		if err := streamer.Resend(coord); err == nil {
			t.Errorf("Resend honoured %s (%+v)", name, coord)
		}
	}

	if !streamer.View().Holds(held) || !streamer.View().Holds(outside) {
		t.Error("a refused request forgot a chunk; a refusal must cost the client nothing")
	}
	if got := wake.count(); got != 0 {
		t.Errorf("a refused request woke the streaming loop %d times", got)
	}
}

// The bound. There is no rate limit anywhere else in this repository, so this is the test
// that says what the number means: a burst of one view volume, refilled at the fastest
// rate a player can cross chunk boundaries (TerminalFallSpeed / ChunkSize = 1.875 a
// second, so 533ms a token).
//
// Radius 0 makes the burst exactly one chunk, which is what makes the bound reachable in
// two requests instead of 344. The arithmetic under test is the same at every radius.
func TestResendIsRateLimitedPerSession(t *testing.T) {
	t.Parallel()

	ctx := context.Background()
	coord := world.Coord{}
	clock := newFrozenClock()

	streamer := session.NewStreamer(world.NewCache(5, 1, 8), 0,
		func([]byte) error { return nil }, func() {}, clock.Now, discard(),
	)
	if err := streamer.MoveTo(ctx, coord); err != nil {
		t.Fatalf("MoveTo: %v", err)
	}

	if err := streamer.Resend(coord); err != nil {
		t.Fatalf("the first request was refused with a full bucket: %v", err)
	}

	// Back to where the session was, so the only thing standing between the next request
	// and a repair is the bucket.
	streamer.View().MarkLoaded(coord)
	if err := streamer.Resend(coord); err == nil {
		t.Fatal("a second request in the same instant was honoured; the burst is one chunk at radius 0")
	}
	if !streamer.View().Holds(coord) {
		t.Fatal("a rate-limited request forgot the chunk anyway, so the client lost it for nothing")
	}

	// The two advances bracket the derived rate instead of merely proving that *some*
	// refill happens. A token is 1/1.875 = 533.3ms, so 500ms must not buy one and 534ms
	// must — which fails below about 1.873 a second and above 2. The pair matters because
	// the constant is a division of two constants in different packages: were
	// `TerminalFallSpeed` ever written `60` instead of `60.0`, Go would make the quotient
	// an untyped integer and the rate would silently become 1 a second. A test that only
	// checked "a full second refills" passes at that rate too, and this one now does not.
	clock.advance(500 * time.Millisecond)
	if err := streamer.Resend(coord); err == nil {
		t.Error("half a second bought a token; a token is 533ms at 1.875 a second")
	}
	clock.advance(34 * time.Millisecond)
	if err := streamer.Resend(coord); err != nil {
		t.Errorf("534ms did not buy a token, so the rate is below the derived 1.875: %v", err)
	}
}

// The server's own repair path — the one sendChunk reaches when a chunk is edited faster
// than it can be sent — and the bound on it.
//
// It uses the same repair a client's request does, so the stationary player its comment
// used to record as a known gap is served. Once, though: a follow-up pass that loses the
// same race does not ask for another, because that loop is the re-send storm.
func TestAGiveUpAsksForOneMoreDiffAndOnlyOne(t *testing.T) {
	t.Parallel()

	ctx := context.Background()
	chunks := world.NewCache(5, 1, 8)
	coord := world.Coord{}
	wake := &wakeCounter{}

	sends := 0
	streamer := session.NewStreamer(chunks, 0, func([]byte) error {
		sends++
		// Every send loses the race: the chunk is edited between the read and the peek, so
		// the streamer can never confirm the copy it just sent.
		if err := chunks.Apply(ctx, int64(sends), 6, 7, world.Snow, nil); err != nil {
			t.Errorf("Apply during send: %v", err)
		}
		return nil
	}, wake.wake, newFrozenClock().Now, discard())

	if err := streamer.MoveTo(ctx, coord); err != nil {
		t.Fatalf("MoveTo: %v", err)
	}
	if sends != maxChunkResendsInTest+1 {
		t.Fatalf("the first pass sent the chunk %d times, want %d", sends, maxChunkResendsInTest+1)
	}
	if streamer.View().Holds(coord) {
		t.Error("the give-up did not forget the chunk, so nothing would re-send it")
	}
	if got := wake.count(); got != 1 {
		t.Fatalf("the give-up woke the streaming loop %d times, want 1", got)
	}

	// The follow-up, exactly as followPlayer runs it on the wake. It loses the same race,
	// and it does not ask for a third pass.
	if err := streamer.MoveTo(ctx, coord); err != nil {
		t.Fatalf("the repair pass: %v", err)
	}
	if sends != 2*(maxChunkResendsInTest+1) {
		t.Fatalf("the repair pass sent %d frames in total, want %d", sends, 2*(maxChunkResendsInTest+1))
	}
	if got := wake.count(); got != 1 {
		t.Errorf("a repair pass that gave up asked for another (%d wakes); that loop is the re-send storm", got)
	}
}

// maxChunkResendsInTest mirrors the unexported maxChunkResends. Kept as a named constant
// so the expectations above read as arithmetic rather than as the number 3.
const maxChunkResendsInTest = 2

// ---------------------------------------------------------------------------
// The chunk entering the view, reported once
// ---------------------------------------------------------------------------

// What the simulation is told, and when.
//
// A settlement's forge and fire are written down nowhere: they are derived the first time
// somebody looks at the ground they stand on, and this hook is the one moment the server
// learns a piece of the world has become visible. Every chunk the diff loads is reported,
// and nothing already held is reported again.
func TestEveryChunkEnteringTheViewIsReportedOnce(t *testing.T) {
	t.Parallel()

	ctx := context.Background()
	var entered []world.Coord

	streamer := testStreamer(world.NewCache(5, 1, 8), 1, func([]byte) error { return nil })
	streamer.ReportEntering(func(coord world.Coord) { entered = append(entered, coord) })

	if err := streamer.MoveTo(ctx, world.Coord{}); err != nil {
		t.Fatalf("MoveTo: %v", err)
	}
	// A radius of one is a 3×3×3 cube, and every chunk in it is new.
	if len(entered) != 27 {
		t.Fatalf("%d chunks were reported for the first diff, want 27", len(entered))
	}
	for _, coord := range entered {
		if !streamer.View().Holds(coord) {
			t.Errorf("chunk %+v was reported as entering the view and is not in it", coord)
		}
	}

	// Standing still reports nothing: the view holds everything already.
	entered = entered[:0]
	if err := streamer.MoveTo(ctx, world.Coord{}); err != nil {
		t.Fatalf("MoveTo without moving: %v", err)
	}
	if len(entered) != 0 {
		t.Fatalf("standing still reported %d chunks, want none", len(entered))
	}

	// One step along x brings a 3×3 face in and takes one out.
	if err := streamer.MoveTo(ctx, world.Coord{X: 1}); err != nil {
		t.Fatalf("MoveTo one chunk east: %v", err)
	}
	if len(entered) != 9 {
		t.Fatalf("a one-chunk step reported %d chunks, want the 9 of the new face", len(entered))
	}
	for _, coord := range entered {
		if coord.X != 2 {
			t.Errorf("chunk %+v was reported and is not on the face the step revealed", coord)
		}
	}
}

// A chunk this session could not be sent has still entered its view. The report is ahead
// of the send deliberately: what it produces is an entity a snapshot carries, and a
// snapshot does not wait on terrain.
//
// The chunks *behind* a failed send are not lost either: [View.MarkLoaded] runs only
// after a send returns, so a partial send leaves the rest out of the view, and
// [View.MoveTo] refuses to shortcut an unchanged centre for exactly this reason.
func TestAChunkThatCouldNotBeSentIsStillReported(t *testing.T) {
	t.Parallel()

	const inView, before = 27, 3 // a radius of one is 3×3×3, one frame each
	sent, failing := 0, true
	var entered []world.Coord
	streamer := testStreamer(world.NewCache(5, 1, 8), 1, func([]byte) error {
		sent++
		if failing && sent > before {
			return errors.New("the session is finished")
		}
		return nil
	})
	streamer.ReportEntering(func(c world.Coord) { entered = append(entered, c) })

	if err := streamer.MoveTo(context.Background(), world.Coord{}); err == nil {
		t.Fatal("MoveTo returned no error for a send that failed")
	}
	if len(entered) != before+1 {
		t.Fatalf("%d chunks reported, want %d", len(entered), before+1)
	}

	// The same centre: a partial send leaves the player where they were.
	failing = false
	if err := streamer.MoveTo(context.Background(), world.Coord{}); err != nil {
		t.Fatalf("the recovery pass: %v", err)
	}
	reported := make(map[world.Coord]struct{}, inView)
	for _, c := range entered {
		reported[c] = struct{}{}
	}
	if len(reported) != inView {
		t.Errorf("%d of %d chunks in view reported, want all", len(reported), inView)
	}
}
