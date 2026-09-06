package session

import (
	"cmp"
	"fmt"
	"slices"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// Keep the fixed cell-id bias nonzero throughout the playable world.
const _ = uint16(32768 - world.BlockLimit/world.RuinCellBlocks - 1)

// landmarkAt is seed-only: learning a position is independent of discovering it.
func landmarkAt(seed, cellX, cellZ int64) protocol.Landmark {
	ruin, ok := world.RuinAt(seed, cellX, cellZ)
	if !ok {
		return protocol.Landmark{}
	}
	// Biasing the playable cells by 32768 is injective and nonzero, even at (0,0).
	// The world seed scopes identity; no counter or hash can rename a site on reload.
	return protocol.Landmark{LandmarkID: uint64(cellX+32768)<<32 | uint64(cellZ+32768), X: int32(ruin.Arch.X), Z: int32(ruin.Arch.Z), Kind: vnet.LandmarkKindPortal}
}

// landmarksForTile is called only after DrawMapTile has validated and metered the
// request. Tile spans 64/256/1024 divide the 8192-block lattice, so a valid tile
// lies in exactly one cell. One lookup suffices, even for unexplored terrain.
// No global scan or unbounded query history is needed. Widen before adding the
// span: the existing map request admits any aligned int32 origin.
func landmarksForTile(seed int64, request protocol.MapTileRequest, explored *Exploration) protocol.LandmarkList {
	result := protocol.LandmarkList{OriginX: request.OriginX, OriginZ: request.OriginZ, Scale: request.Scale}
	landmark := landmarkAt(seed, world.RuinCellOf(int64(request.OriginX)), world.RuinCellOf(int64(request.OriginZ)))
	span := int64(protocol.MapTileSpan(request.Scale))
	if landmark.LandmarkID == 0 || int64(landmark.X) < int64(request.OriginX) || int64(landmark.X) >= int64(request.OriginX)+span || int64(landmark.Z) < int64(request.OriginZ) || int64(landmark.Z) >= int64(request.OriginZ)+span {
		return result
	}
	landmark.Discovered = explored.Explored(world.ChunkOf(int64(landmark.X), 0, int64(landmark.Z)).Column())
	result.Landmarks = []protocol.Landmark{landmark}
	return result
}

// landmarks tracks only streaming discoveries, bounded by the existing exploration
// ledger. It is owned by the streaming goroutine. The read-loop's map queries never
// touch it, so a query cannot discover a site or grow a per-session world-site cache.
// World instances must scope this state and the exploration ledger together.
type landmarks struct {
	seed     int64
	explored *Exploration
	// Includes rejected cells, so many streamed columns in one cell pay one lookup.
	cells map[ruinCell]protocol.Landmark
	found map[uint64]struct{}
}
type ruinCell struct{ x, z int64 }

func newLandmarks(seed int64, explored *Exploration) *landmarks {
	l := &landmarks{seed: seed, explored: explored, cells: make(map[ruinCell]protocol.Landmark), found: make(map[uint64]struct{})}
	// Prime from the bounded saved ledger. No initial global list is sent; opening
	// the map asks for its tiles and their sites, including those never explored.
	l.reveal(explored.Snapshot())
	return l
}

// reveal returns only newly discovered portals. Cached sites whose arch was not
// explored are reconsidered on later batches in their cell. Neighbouring columns
// do not discover the arch. No request on the client read loop calls this method.
func (l *landmarks) reveal(columns []world.Column) []protocol.Landmark {
	if l == nil {
		return nil
	}
	var added []protocol.Landmark
	visited := make(map[ruinCell]struct{}, len(columns))
	for _, col := range columns {
		cell := ruinCell{world.RuinCellOf(int64(col.CX) * world.ChunkSize), world.RuinCellOf(int64(col.CZ) * world.ChunkSize)}
		if _, done := visited[cell]; done {
			continue
		}
		visited[cell] = struct{}{}
		landmark, cached := l.cells[cell]
		if !cached {
			landmark = landmarkAt(l.seed, cell.x, cell.z)
			l.cells[cell] = landmark
		}
		if landmark.LandmarkID == 0 || !l.explored.Explored(world.ChunkOf(int64(landmark.X), 0, int64(landmark.Z)).Column()) {
			continue
		}
		if _, known := l.found[landmark.LandmarkID]; known {
			continue
		}
		l.found[landmark.LandmarkID] = struct{}{}
		landmark.Discovered = true
		added = append(added, landmark)
	}
	slices.SortFunc(added, func(a, b protocol.Landmark) int { return cmp.Compare(a.LandmarkID, b.LandmarkID) })
	return added
}

// discoveryTile chooses the smallest canonical tile containing this arch. Updates
// therefore reach every client cache scale without knowing the current viewport.
// The receiver replaces this world rectangle and deduplicates the id across zooms.
func discoveryTile(landmark protocol.Landmark) protocol.LandmarkList {
	const span = int32(protocol.MapTileEdge)
	floor := func(v int32) int32 { return v - (v%span+span)%span }
	return protocol.LandmarkList{OriginX: floor(landmark.X), OriginZ: floor(landmark.Z), Scale: 1, Landmarks: []protocol.Landmark{landmark}}
}

func sendLandmarkList(send func([]byte) error, list protocol.LandmarkList) error {
	frame, err := protocol.EncodeLandmarkList(list)
	if err != nil {
		return fmt.Errorf("session: encode portal map tile: %w", err)
	}
	if err := send(frame); err != nil {
		return fmt.Errorf("session: send portal map tile: %w", err)
	}
	return nil
}
