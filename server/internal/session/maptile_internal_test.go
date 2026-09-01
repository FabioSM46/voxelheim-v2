// Internal tests for the map tile path. Internal because what is worth pinning here —
// the mask, the bit layout, the grid and the bucket — is arithmetic over a seed and a
// set, and driving it through a whole session would test the wiring instead: an
// end-to-end session cannot be handed a stopped clock, and it cannot explore a column
// it was never streamed.
package session

import (
	"errors"
	"log/slog"
	"testing"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

const mapTileSeed = 0x5EED

// mapTileLedger is a ledger holding exactly the columns named.
func mapTileLedger(columns ...world.Column) *Exploration {
	explored := newExploration(nil, 0, nil, false, slog.New(slog.DiscardHandler))
	for _, column := range columns {
		explored.Reveal(column)
	}
	return explored
}

// stoppedClock is a clock a test moves by hand, so a bucket can be spent without
// spending a second — the same reason the resend limiter takes one.
type stoppedClock struct{ at time.Time }

func (c *stoppedClock) now() time.Time { return c.at }

// mapTileStreamer is a streamer with nothing but what the tile path needs: a cache for
// its seed, a ledger, and a clock the test owns.
func mapTileStreamer(t *testing.T, clock *stoppedClock, explored *Exploration) *Streamer {
	t.Helper()

	streamer := NewStreamer(world.NewCache(mapTileSeed, 1, 8), 1,
		func([]byte) error { return nil }, func() {}, clock.now, slog.New(slog.DiscardHandler))
	streamer.RecordExploration(explored)
	return streamer
}

// A tile of ground this character has walked is the ground, pixel for pixel.
//
// The tile is checked against [world.SurfaceAt] rather than against a recorded picture,
// because what this path owes the client is the world's own answer at the pixel's
// centre and nothing else: the sampling rule — origin + i×scale + scale/2 — is the
// thing that can be wrong here, and a wrong one still produces a plausible map.
func TestAMapTileIsTheGroundAtEachPixelCentre(t *testing.T) {
	t.Parallel()

	for _, scale := range protocol.MapTileScales {
		span := int32(protocol.MapTileSpan(scale))
		originX, originZ := 12*span, -7*span

		// Every column of the tile, so nothing is masked and every pixel is asserted.
		edge := int(span) / protocol.ChunkColumnBlocks
		base := world.ChunkOf(int64(originX), 0, int64(originZ)).Column()
		columns := make([]world.Column, 0, edge*edge)
		for cz := range edge {
			for cx := range edge {
				columns = append(columns, world.Column{CX: base.CX + int32(cx), CZ: base.CZ + int32(cz)})
			}
		}

		tile := drawMapTile(mapTileSeed,
			protocol.MapTileRequest{OriginX: originX, OriginZ: originZ, Scale: scale},
			mapTileLedger(columns...))

		if tile.OriginX != originX || tile.OriginZ != originZ || tile.Scale != scale {
			t.Fatalf("scale %d: tile echoes (%d, %d, %d), want (%d, %d, %d)",
				scale, tile.OriginX, tile.OriginZ, tile.Scale, originX, originZ, scale)
		}
		if len(tile.Height) != protocol.MapTileCells || len(tile.Surface) != protocol.MapTileCells {
			t.Fatalf("scale %d: tile has %d heights and %d surfaces, want %d of each",
				scale, len(tile.Height), len(tile.Surface), protocol.MapTileCells)
		}

		for pz := range protocol.MapTileEdge {
			for px := range protocol.MapTileEdge {
				x := int64(originX) + int64(px)*int64(scale) + int64(scale)/2
				z := int64(originZ) + int64(pz)*int64(scale) + int64(scale)/2
				height, kind := world.SurfaceAt(mapTileSeed, x, z)

				cell := pz*protocol.MapTileEdge + px
				if got, want := tile.Height[cell], protocol.MapTileHeight(height); got != want {
					t.Fatalf("scale %d: pixel (%d, %d) height %d, want %d for column (%d, %d)",
						scale, px, pz, got, want, x, z)
				}
				if got, want := tile.Surface[cell], byte(mapSurfaceOf(kind)); got != want {
					t.Fatalf("scale %d: pixel (%d, %d) surface %d, want %d for column (%d, %d)",
						scale, px, pz, got, want, x, z)
				}
			}
		}
	}
}

// The unexplored is blank, not shaped — and the mask says exactly which columns those
// are.
//
// **The pixels and the bits are asserted against each other**, which is the property
// that matters: a client draws what the mask permits, so a mask that disagreed with the
// arrays by one column would either hide ground the character has walked or, far worse,
// leave terrain in a frame with a "do not draw" bit beside it. The server never sends
// the shape of the unexplored, so an unexplored pixel carries zero in both arrays here
// and its terrain is never computed at all.
func TestAMapTileSendsNothingAboutTheUnexplored(t *testing.T) {
	t.Parallel()

	const scale = 4
	span := int32(protocol.MapTileSpan(scale))
	edge := int(span) / protocol.ChunkColumnBlocks
	base := world.ChunkOf(int64(span), 0, int64(span)).Column()

	// A diagonal of columns: sparse enough that most of the tile is masked, and shaped
	// so that a mask read with x and z swapped would not pass.
	explored := map[world.Column]bool{}
	columns := []world.Column{}
	for i := range edge {
		column := world.Column{CX: base.CX + int32(i), CZ: base.CZ + int32(2*i%edge)}
		explored[column] = true
		columns = append(columns, column)
	}

	tile := drawMapTile(mapTileSeed,
		protocol.MapTileRequest{OriginX: span, OriginZ: span, Scale: scale},
		mapTileLedger(columns...))

	if got, want := len(tile.Explored), protocol.MapTileExploredBytes(scale); got != want {
		t.Fatalf("the mask is %d bytes, want %d", got, want)
	}

	for pz := range protocol.MapTileEdge {
		for px := range protocol.MapTileEdge {
			column := world.Column{
				CX: base.CX + int32(px*scale/protocol.ChunkColumnBlocks),
				CZ: base.CZ + int32(pz*scale/protocol.ChunkColumnBlocks),
			}
			bit := int(column.CZ-base.CZ)*edge + int(column.CX-base.CX)
			set := tile.Explored[bit/8]&(1<<(bit%8)) != 0
			if set != explored[column] {
				t.Fatalf("column %+v: mask bit %d is %v, want %v", column, bit, set, explored[column])
			}

			cell := pz*protocol.MapTileEdge + px
			blank := tile.Height[cell] == 0 && tile.Surface[cell] == byte(world.SurfaceUnknown)
			if !set && !blank {
				t.Fatalf("pixel (%d, %d) is in unexplored column %+v and carries height %d, surface %d",
					px, pz, column, tile.Height[cell], tile.Surface[cell])
			}
			if set && blank {
				// Height 0 is a real terrain height (y = -64) and Unknown is not a
				// surface anything returns, so the pair together cannot occur for
				// ground that was actually sampled.
				t.Fatalf("pixel (%d, %d) is in explored column %+v and carries nothing", px, pz, column)
			}
		}
	}
}

// A tile with nothing explored in it is still answered, all zero.
//
// **An empty answer, not silence.** The client asked whether anything is known about a
// square of the world and the answer is no; sending nothing would leave it unable to
// tell that from a lost frame, and it would ask again for ever.
func TestAMapTileWithNothingExploredIsStillAnswered(t *testing.T) {
	t.Parallel()

	for _, explored := range []*Exploration{nil, mapTileLedger()} {
		tile := drawMapTile(mapTileSeed, protocol.MapTileRequest{OriginX: 1024, OriginZ: 1024, Scale: 16}, explored)

		for i, height := range tile.Height {
			if height != 0 || tile.Surface[i] != 0 {
				t.Fatalf("pixel %d of an unexplored tile carries height %d, surface %d", i, height, tile.Surface[i])
			}
		}
		for i, mask := range tile.Explored {
			if mask != 0 {
				t.Fatalf("byte %d of an unexplored tile's mask is %#02x, want 0", i, mask)
			}
		}
	}
}

// A tile origin that is not on the grid, and a scale this contract has no member for,
// are refused — and the refusal happens before the bucket is touched.
//
// Nothing on the wire reaches this: protocol.Decode refuses both from the frame alone
// and closes the session over them. It is asserted anyway because the tile path must
// never draw an off-grid tile, and because a guard whose only justification is a check
// in another package is a guard that quietly stops being true when that check moves.
func TestAMapTileOffTheGridIsRefusedBeforeTheBucket(t *testing.T) {
	t.Parallel()

	clock := &stoppedClock{at: time.Unix(0, 0)}
	streamer := mapTileStreamer(t, clock, mapTileLedger())

	for _, request := range []protocol.MapTileRequest{
		{OriginX: 0, OriginZ: 0, Scale: 0},
		{OriginX: 0, OriginZ: 0, Scale: 2},
		{OriginX: 0, OriginZ: 0, Scale: 255},
		{OriginX: 1, OriginZ: 0, Scale: 1},
		{OriginX: 0, OriginZ: 63, Scale: 1},
		{OriginX: 64, OriginZ: 0, Scale: 4},
		{OriginX: -1024, OriginZ: -16, Scale: 16},
	} {
		if _, err := streamer.DrawMapTile(request); !errors.Is(err, errMapTileMisaligned) {
			t.Errorf("DrawMapTile(%+v) = %v, want a misalignment", request, err)
		}
	}

	// mapTileBurst tiles are still there to spend, so none of the refusals above cost a
	// token: a client cannot empty its own bucket on requests the server would never
	// serve and then find itself unable to ask for the tile it wanted.
	for i := range mapTileBurst {
		if _, err := streamer.DrawMapTile(protocol.MapTileRequest{Scale: 16}); err != nil {
			t.Fatalf("request %d after the refusals: %v", i+1, err)
		}
	}
}

// The bucket: a burst of mapTileBurst is served and the next one is dropped, and a
// second of elapsed time buys exactly mapTileRefillPerSecond more.
//
// Dropped rather than refused, which is the chunk-resend precedent: asking too fast is
// not something the player did wrong, and the contract has no message that says so.
func TestAMapTileBurstIsServedAndTheNextIsDropped(t *testing.T) {
	t.Parallel()

	clock := &stoppedClock{at: time.Unix(0, 0)}
	streamer := mapTileStreamer(t, clock, mapTileLedger())
	request := protocol.MapTileRequest{OriginX: 1024, OriginZ: -1024, Scale: 16}

	for i := range mapTileBurst {
		if _, err := streamer.DrawMapTile(request); err != nil {
			t.Fatalf("request %d of the burst: %v", i+1, err)
		}
	}
	if _, err := streamer.DrawMapTile(request); !errors.Is(err, errMapTileThrottled) {
		t.Fatalf("request %d = %v, want the bucket to be empty", mapTileBurst+1, err)
	}

	clock.at = clock.at.Add(time.Second)
	for i := range mapTileRefillPerSecond {
		if _, err := streamer.DrawMapTile(request); err != nil {
			t.Fatalf("request %d of the second's refill: %v", i+1, err)
		}
	}
	if _, err := streamer.DrawMapTile(request); !errors.Is(err, errMapTileThrottled) {
		t.Fatalf("a second bought more than %d tiles", mapTileRefillPerSecond)
	}
}

// The production client opens on a 1024×768 logical viewport at zoom two, so its map
// picture is 512×384 tile pixels. A 64-pixel tile divides that into eight by six when
// aligned; a centre off the tile grid adds at most one square on each axis. The limiter
// must admit that whole ordinary opening before it starts protecting the read loop from
// a client that keeps asking.
func TestTheMapTileBurstCoversAColdDefaultViewport(t *testing.T) {
	t.Parallel()

	const (
		defaultImageWidth  = 1024 / 2
		defaultImageHeight = 768 / 2
		maxTilesAcross     = defaultImageWidth/protocol.MapTileEdge + 1
		maxTilesDown       = defaultImageHeight/protocol.MapTileEdge + 1
		coldOpening        = maxTilesAcross * maxTilesDown
	)
	if mapTileBurst < coldOpening {
		t.Fatalf("map tile burst %d does not cover a %d-tile cold opening", mapTileBurst, coldOpening)
	}
	if mapTileBurst != 64 {
		t.Fatalf("map tile burst = %d, want the measured 64-tile bound", mapTileBurst)
	}
}

// BenchmarkDrawMapTile is the acceptance criterion the tile path has to hold: a
// scale-16 tile — the most ground one frame can describe, 4096 columns of it — inside
// 10 ms on the CI runner class.
//
// **Measured rather than assumed, because [world.HeightAt] moved three times in one
// iteration.** Worldgen 3, 4 and 5 took BenchmarkGenerate from 1.68 ms/op to 3.43 — a
// cumulative 2.04× — and basins and river channels now live *inside* the height field,
// so this is a new caller of the thing that got more expensive. On the machine that
// reproduces the recorded chunk figure (3.45 ms/op here) a fully explored tile costs
// 1.7–1.9 ms: roughly half of one chunk generation, and under a fifth of the ceiling
// this criterion sets.
//
// The three scales measure the same, 1.7–2.2 ms across all of them, and that is the
// sampling rule showing through rather than noise: one column per pixel at every zoom,
// so a coarse tile covers more world and never more work. It is also why there is no
// separate benchmark per scale — there would be nothing for the other two to say.
//
// Every column is explored, deliberately: the mask is what makes a tile *cheaper*, so a
// benchmark over a partly explored one would measure the fog rather than the ground.
func BenchmarkDrawMapTile(b *testing.B) {
	const scale = 16
	span := int32(protocol.MapTileSpan(scale))
	originX, originZ := 12*span, -7*span

	edge := int(span) / protocol.ChunkColumnBlocks
	base := world.ChunkOf(int64(originX), 0, int64(originZ)).Column()
	columns := make([]world.Column, 0, edge*edge)
	for cz := range edge {
		for cx := range edge {
			columns = append(columns, world.Column{CX: base.CX + int32(cx), CZ: base.CZ + int32(cz)})
		}
	}
	explored := mapTileLedger(columns...)
	request := protocol.MapTileRequest{OriginX: originX, OriginZ: originZ, Scale: scale}

	for b.Loop() {
		drawMapTile(mapTileSeed, request, explored)
	}
}
