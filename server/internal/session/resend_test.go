package session_test

import (
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// ---------------------------------------------------------------------------
// Asking for a chunk back, end to end
// ---------------------------------------------------------------------------

// resendRequest is a client asking for one chunk it has lost.
func resendRequest(coord world.Coord) []byte {
	return protocol.EncodeChunkResendRequest(protocol.ChunkResendRequest{
		Coord:    protocol.ChunkCoord{X: coord.X, Y: coord.Y, Z: coord.Z},
		HasCoord: true,
	})
}

// The whole path, over a connection: a client asks for a chunk it holds, and the server
// sends it again.
//
// **Nothing steps the simulation in this test, and that is the entire point of it.** The
// player is admitted, the spawn chunk is published once, and then no tick ever runs — so
// no chunk crossing ever happens and `followPlayer` would sit in `NextChunk` for ever.
// That is exactly the state a standing player is in, and before this message existed it
// was the state in which a lost chunk stayed lost. If the resend request did not wake the
// streaming goroutine, this test could only time out.
func TestAStationaryClientIsSentBackTheChunkItAsksFor(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)

	conn, frames := admit(t, cfg, chunks, sim, peers, 1)

	// The ground under the player's feet, which is the chunk the whole feature is about.
	underfoot := world.ContainingChunk(cfg.Spawn[0], cfg.Spawn[1], cfg.Spawn[2])
	if got := frames.chunkCount(underfoot); got != 1 {
		t.Fatalf("the session was sent chunk %+v %d times before asking, want exactly 1", underfoot, got)
	}

	conn.in <- resendRequest(underfoot)
	waitUntil(t, "the chunk to be sent a second time", func() bool {
		return frames.chunkCount(underfoot) >= 2
	})

	if got := frames.chunkCount(underfoot); got != 2 {
		t.Errorf("chunk %+v arrived %d times, want 2: one request is one resend", underfoot, got)
	}

	// One coordinate, never a resynchronisation. Every other chunk of the view was sent
	// once and stays sent once.
	for _, coord := range frames.chunkCoords() {
		if coord == underfoot {
			continue
		}
		if got := frames.chunkCount(coord); got != 1 {
			t.Errorf("chunk %+v was sent %d times, want 1: the repair is one coordinate", coord, got)
		}
	}
}

// A refused request is silence — no chunk, no rejection payload, no acknowledgement of any
// kind — and it does not end the connection, because the frame was well formed and only a
// value was wrong.
//
// The legal request that follows is how the test tells "refused" apart from "not processed
// yet": exactly one chunk arrives twice, and it is the second request's. The session
// surviving is asserted by admit's own cleanup, which fails the test if Serve returns an
// error.
func TestARefusedResendIsSilentAndTheSessionSurvivesIt(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)

	conn, frames := admit(t, cfg, chunks, sim, peers, 1)
	underfoot := world.ContainingChunk(cfg.Spawn[0], cfg.Spawn[1], cfg.Spawn[2])

	// Far outside a one-chunk view volume, and never sent to this session.
	unseen := world.Coord{X: underfoot.X + 500, Y: underfoot.Y, Z: underfoot.Z}
	conn.in <- resendRequest(unseen)
	// A request that named no coordinate at all: chunk (0, 0, 0) is a real chunk, so an
	// absent one must not be read as the origin.
	conn.in <- protocol.EncodeChunkResendRequest(protocol.ChunkResendRequest{})
	conn.in <- resendRequest(underfoot)

	waitUntil(t, "the legal request to be honoured", func() bool {
		return frames.chunkCount(underfoot) >= 2
	})

	if got := frames.chunkCount(unseen); got != 0 {
		t.Errorf("the session was sent chunk %+v %d times; it never held it", unseen, got)
	}
	if got := frames.chunkCount(underfoot); got != 2 {
		t.Errorf("chunk %+v arrived %d times, want 2 — one of the refused requests was honoured", underfoot, got)
	}
	// Chunk (0, 0, 0) is inside this session's view only if the spawn column happens to
	// sit in it; the assertion that matters is that the coordinate-less request did not
	// cause a resend of it.
	if origin := (world.Coord{}); origin != underfoot {
		if got := frames.chunkCount(origin); got > 1 {
			t.Errorf("the request that named no coordinate resent chunk %+v", origin)
		}
	}
}

// The registry's view and the streamer's are one object, so a repair is visible to the
// broadcast immediately: a chunk a session has asked to be resent is one it no longer
// holds, and a voxel update for it would describe terrain the client has been told to
// expect whole.
func TestAResentChunkLeavesTheBroadcastSetUntilItArrives(t *testing.T) {
	t.Parallel()

	coord := world.Coord{X: 2, Y: -1, Z: 3}

	view := session.NewView(1)
	view.MoveTo(coord)
	view.MarkLoaded(coord)

	if !view.Holds(coord) {
		t.Fatal("the view does not hold the chunk, so this test asserts nothing")
	}
	if !view.Resendable(coord) {
		t.Fatal("a chunk the session holds in view is not resendable")
	}

	view.Forget(coord)

	if view.Holds(coord) {
		t.Error("a forgotten chunk is still in the broadcast set")
	}
	if view.Resendable(coord) {
		t.Error("a chunk already being resent can be asked for again; the second request would be free")
	}
}
