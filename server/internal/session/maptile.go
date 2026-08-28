package session

import (
	"errors"
	"fmt"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// A map tile is arithmetic.
//
// **Nothing here reads a chunk, takes the simulation's lock or touches the cache.** A
// tile is 4096 evaluations of [world.SurfaceAt], which is a pure function of the seed
// and a column, run on the session's own read loop. That is what makes the map cheap
// enough to have no server-side cache: recomputation *is* the cache, and a tile nobody
// asks for costs nothing to keep.
//
// It is also what the map is honest about. The delta store holds edits and this path
// never opens it, so the map draws the world as it was generated — a dug-out hill is
// still a hill on it. That is a smaller claim than "the map is live", and it is the one
// that can be made for free.

// errMapTileMisaligned is a request for a tile that is not on the grid, or at a scale
// this contract has no member for.
//
// **Unreachable from the wire, and stated anyway.** protocol.Decode already refuses
// both from the frame alone and closes the session over them, which schemas/world.fbs
// names as the stricter of the two answers a server may give. This is the other one,
// kept because the tile path must never draw an off-grid tile and because a guard whose
// only justification is a check somewhere else is a guard that disappears when that
// check moves.
var errMapTileMisaligned = errors.New("session: map tile off the grid")

// errMapTileThrottled says this session has spent its map-tile bucket. A dropped
// request, not a refused one — see mapTileRefillPerSecond for why silence is the right
// answer.
var errMapTileThrottled = errors.New("session: map tile rate limit")

// DrawMapTile answers one MapTileRequest with the square of the map this character is
// allowed to see.
//
// **The client is asking for data, never for an outcome.** Nothing in the request says
// what the ground holds or which pixels may be answered; the seed answers the first and
// this character's own exploration ledger answers the second, and neither is anything a
// client can state.
//
// The two refusals are asked in this order deliberately, and it is the order
// [Streamer.Resend] uses for the same reason: a request this server would never serve
// is rejected before the bucket sees it, so a client cannot empty its own bucket on
// nonsense and then find itself unable to ask for the tile it actually wants.
func (s *Streamer) DrawMapTile(request protocol.MapTileRequest) (protocol.MapTile, error) {
	span := protocol.MapTileSpan(request.Scale)
	if span == 0 {
		return protocol.MapTile{}, fmt.Errorf("%w: scale %d is not 1, 4 or 16", errMapTileMisaligned, request.Scale)
	}
	// Go's % keeps the sign of the dividend, so an exact multiple is zero on both sides
	// of the origin and nothing else is. That is precisely the test wanted here.
	if request.OriginX%span != 0 || request.OriginZ%span != 0 {
		return protocol.MapTile{}, fmt.Errorf("%w: origin (%d, %d) is not on the %d-block grid",
			errMapTileMisaligned, request.OriginX, request.OriginZ, span)
	}

	if !s.tiles.allow() {
		return protocol.MapTile{}, errMapTileThrottled
	}

	return drawMapTile(s.cache.Seed(), request, s.explored), nil
}

// drawMapTile computes one tile. Pure in its arguments, which is what lets it be tested
// against a seed and a set with no session, no socket and no clock anywhere near it.
//
// **The mask is applied before the arithmetic, not after it.** A pixel in a chunk
// column this character has never been sent is left at zero in both arrays and its
// terrain is never computed — so the unexplored is not merely withheld from the frame,
// it is never a value in this process. A client is not where a secret is kept, and the
// cheapest way to be sure of that is to have nothing to withhold.
//
// A nil ledger is a character who has explored nothing — every method on *Exploration
// is nil-safe — and it produces an all-zero tile rather than an error. That is the
// contract's answer too: an empty tile is still an answer, so a client can cache
// "nothing is known here" instead of asking again.
func drawMapTile(seed int64, request protocol.MapTileRequest, explored *Exploration) protocol.MapTile {
	scale := int64(request.Scale)
	originX := int64(request.OriginX)
	originZ := int64(request.OriginZ)

	tile := protocol.MapTile{
		OriginX:  request.OriginX,
		OriginZ:  request.OriginZ,
		Scale:    request.Scale,
		Height:   make([]byte, protocol.MapTileCells),
		Surface:  make([]byte, protocol.MapTileCells),
		Explored: make([]byte, protocol.MapTileExploredBytes(request.Scale)),
	}

	// The columns first, once each. A tile at scale 1 covers 4 chunk columns and one at
	// scale 16 covers 1024, against 4096 pixels either way — so asking the ledger per
	// pixel would take between four and a thousand times as many locks for an answer
	// that cannot differ inside one column.
	edge := int(protocol.MapTileSpan(request.Scale)) / protocol.ChunkColumnBlocks
	base := world.ChunkOf(originX, 0, originZ).Column()
	known := make([]bool, edge*edge)
	for cz := range edge {
		for cx := range edge {
			if !explored.Explored(world.Column{CX: base.CX + int32(cx), CZ: base.CZ + int32(cz)}) {
				continue
			}
			// Row-major in the same order the pixels are, z outer and x inner, LSB
			// first within each byte. The four high bits of a scale-1 mask are never
			// reached and stay zero, which the contract requires.
			bit := cz*edge + cx
			known[bit] = true
			tile.Explored[bit/8] |= 1 << (bit % 8)
		}
	}

	for pz := range protocol.MapTileEdge {
		// The pixel's *centre*, which is what schemas/world.fbs samples: one column per
		// pixel rather than an average of scale² of them, so a coarse tile costs exactly
		// what a fine one does. The map is a picture, not a measurement.
		z := originZ + int64(pz)*scale + scale/2
		czLocal := int(int64(pz)*scale) / protocol.ChunkColumnBlocks

		for px := range protocol.MapTileEdge {
			if !known[czLocal*edge+int(int64(px)*scale)/protocol.ChunkColumnBlocks] {
				continue
			}

			height, kind := world.SurfaceAt(seed, originX+int64(px)*scale+scale/2, z)
			cell := pz*protocol.MapTileEdge + px
			tile.Height[cell] = protocol.MapTileHeight(height)
			tile.Surface[cell] = byte(mapSurfaceOf(kind))
		}
	}

	return tile
}
