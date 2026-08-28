package session_test

import (
	"context"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// ---------------------------------------------------------------------------
// Asking for a square of the map, end to end
// ---------------------------------------------------------------------------

// mapTileRequest is a client asking for one square of the map.
func mapTileRequest(originX, originZ int32, scale uint8) []byte {
	return protocol.EncodeMapTileRequest(protocol.MapTileRequest{
		OriginX: originX, OriginZ: originZ, Scale: scale,
	})
}

// The whole path, over a connection: a client asks for the square of map it is standing
// in and the server draws it — from the seed, with no chunk read and no tick stepped.
//
// **Nothing steps the simulation here, for the reason the resend test does not step
// it.** A map is something a standing player opens, and the tile has to arrive anyway.
// What the session has explored is whatever its first view streamed, which is why the
// expectations below are derived from the MapExplored pages the same session received
// rather than written down: the mask and the ledger are supposed to be one answer, and
// a test that stated the answer twice could not notice them disagreeing.
func TestAClientIsSentTheMapOfWhereItHasBeen(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)
	conn, frames := admit(t, cfg, chunks, sim, peers, 1)

	// The tile the spawn column sits in, at one pixel per block. Its origin is on the
	// 64-block grid by construction, and the spawn column is (0, 0).
	conn.in <- mapTileRequest(0, 0, 1)
	waitUntil(t, "the map tile to arrive", func() bool {
		return len(frames.mapTilesReceived()) >= 1
	})

	tiles := frames.mapTilesReceived()
	if len(tiles) != 1 {
		t.Fatalf("one request produced %d tiles, want exactly 1", len(tiles))
	}
	tile := tiles[0]

	if tile.OriginX != 0 || tile.OriginZ != 0 || tile.Scale != 1 {
		t.Errorf("the tile echoes (%d, %d, scale %d), want (0, 0, scale 1)", tile.OriginX, tile.OriginZ, tile.Scale)
	}
	if len(tile.Height) != protocol.MapTileCells || len(tile.Surface) != protocol.MapTileCells {
		t.Fatalf("the tile carries %d heights and %d surfaces, want %d of each",
			len(tile.Height), len(tile.Surface), protocol.MapTileCells)
	}
	if got, want := len(tile.Explored), protocol.MapTileExploredBytes(1); got != want {
		t.Fatalf("the mask is %d bytes, want %d", got, want)
	}

	// A scale-1 tile covers 2×2 chunk columns, and the four high bits of its one mask
	// byte are past the column count and must be zero.
	if tile.Explored[0]&0xF0 != 0 {
		t.Errorf("the mask is %#02x; the four bits past the column count must be zero", tile.Explored[0])
	}

	ledger := frames.exploredColumns()
	for pz := range protocol.MapTileEdge {
		for px := range protocol.MapTileEdge {
			column := world.ChunkOf(int64(px), 0, int64(pz)).Column()
			bit := int(column.CZ)*2 + int(column.CX)
			_, known := ledger[column]
			if set := tile.Explored[0]&(1<<bit) != 0; set != known {
				t.Fatalf("column %+v: the tile's mask says %v, the ledger this session was sent says %v", column, set, known)
			}

			cell := pz*protocol.MapTileEdge + px
			if !known {
				if tile.Height[cell] != 0 || tile.Surface[cell] != 0 {
					t.Fatalf("pixel (%d, %d) is unexplored and carries height %d, surface %d",
						px, pz, tile.Height[cell], tile.Surface[cell])
				}
				continue
			}

			// Scale 1, so the pixel's centre is the pixel's own column.
			height, _ := world.SurfaceAt(cfg.WorldSeed, int64(px), int64(pz))
			if got, want := tile.Height[cell], protocol.MapTileHeight(height); got != want {
				t.Fatalf("pixel (%d, %d) height %d, want %d", px, pz, got, want)
			}
			if tile.Surface[cell] == 0 {
				t.Fatalf("pixel (%d, %d) is explored and its surface is Unknown", px, pz)
			}
		}
	}
}

// A square of the world nobody has walked is answered, and it is blank.
//
// The client learns "nothing is known here" and can cache it. Answering with silence
// would leave it unable to tell that from a lost frame; answering with the terrain and
// a clear mask beside it would put the unexplored world in a process that is not the
// server's.
func TestAMapTileOfUnwalkedGroundIsBlankAndStillArrives(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)
	conn, frames := admit(t, cfg, chunks, sim, peers, 1)

	// A thousand tiles from spawn at the coarsest scale, so nothing this session has
	// been streamed is anywhere near it.
	const far = 16 * 64 * 1000
	conn.in <- mapTileRequest(far, far, 16)
	waitUntil(t, "the empty tile to arrive", func() bool {
		return len(frames.mapTilesReceived()) >= 1
	})

	tile := frames.mapTilesReceived()[0]
	if tile.OriginX != far || tile.OriginZ != far || tile.Scale != 16 {
		t.Fatalf("the tile echoes (%d, %d, scale %d), want (%d, %d, scale 16)", tile.OriginX, tile.OriginZ, tile.Scale, far, far)
	}
	for i, height := range tile.Height {
		if height != 0 || tile.Surface[i] != 0 {
			t.Fatalf("pixel %d of unwalked ground carries height %d, surface %d", i, height, tile.Surface[i])
		}
	}
	for i, mask := range tile.Explored {
		if mask != 0 {
			t.Fatalf("byte %d of the mask is %#02x over ground nobody has walked", i, mask)
		}
	}
}

// A misaligned request never becomes a tile, and it ends the connection rather than
// producing one.
//
// **This is the stricter of the two answers the contract allows, and it is the one this
// server gives.** schemas/world.fbs says a decoder that can see the violation in the
// frame alone may close the session on it; protocol.Decode does, so the refusal never
// reaches the session's handler. What is asserted here is the consequence a client sees:
// no MapTile, and no session.
func TestAMisalignedMapTileRequestIsNotAnswered(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)
	conn := newFakeConn()
	frames := collect(t, conn)

	served := serveSession(t, conn, cfg, chunks, sim, peers, 1)

	conn.in <- hello(1)
	createCharacter(conn, "Eivor")
	conn.in <- mapTileRequest(1, 0, 1)

	if err := <-served; err == nil {
		t.Fatal("a request one block off the grid was tolerated; it must end the session")
	}
	if got := len(frames.mapTilesReceived()); got != 0 {
		t.Errorf("a misaligned request produced %d tiles, want 0", got)
	}
}

// serveSession runs one session and hands back its result, for the tests that expect it
// to end rather than to survive.
func serveSession(t *testing.T, conn *fakeConn, cfg session.Config, chunks *world.Cache, sim *game.Sim, peers *session.Registry, entityID uint64) <-chan error {
	t.Helper()

	ctx, cancel := context.WithCancel(context.Background())
	served := make(chan error, 1)
	go func() {
		served <- session.Serve(ctx, conn, cfg, noTimeouts(), chunks, sim, peers, ephemeralIdentities(), entityID, discard())
	}()
	t.Cleanup(func() {
		cancel()
		_ = conn.Close()
	})
	return served
}
