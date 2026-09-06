package session

import (
	"cmp"
	"fmt"
	"slices"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// One portal per ruin cell, with the arch inside that cell. The ledger's bound
// therefore also bounds the full list; a larger ledger must revisit the wire cap.
const _ = uint32(protocol.MaxLandmarks - persist.MaxExploredColumns)

// Keep the fixed cell-id bias nonzero throughout the playable world.
const _ = uint16(32768 - world.BlockLimit/world.RuinCellBlocks - 1)

// landmarks is derived session state only. It is assembled before streaming starts
// and subsequently owned by the streaming goroutine; no save path reads it. The
// ledger and list currently describe one world. Instances must scope both together.
type landmarks struct {
	seed     int64
	explored *Exploration
	// Cache visited cells, including empty ones, so a walk through one cell pays for
	// its seed-only lookup once. A zero id denotes a rejected ruin site.
	cells map[ruinCell]protocol.Landmark
	found map[uint64]protocol.Landmark
}

type ruinCell struct{ x, z int64 }

func newLandmarks(seed int64, explored *Exploration) *landmarks {
	l := &landmarks{seed: seed, explored: explored, cells: make(map[ruinCell]protocol.Landmark), found: make(map[uint64]protocol.Landmark)}
	l.reveal(explored.Snapshot())
	return l
}

// reveal considers cells touched by this batch, but tests the actual arch's chunk
// column against the complete ledger. Learning a neighbouring column never reveals
// the arch. Cached accepted sites are reconsidered when a later column arrives.
func (l *landmarks) reveal(columns []world.Column) bool {
	if l == nil {
		return false
	}
	changed := false
	visited := make(map[ruinCell]struct{}, len(columns))
	for _, col := range columns {
		// Widen before multiplying: persisted columns may contain any int32. RuinAt
		// refuses cells beyond the playable world before doing placement arithmetic.
		cell := ruinCell{world.RuinCellOf(int64(col.CX) * world.ChunkSize), world.RuinCellOf(int64(col.CZ) * world.ChunkSize)}
		if _, done := visited[cell]; done {
			continue
		}
		visited[cell] = struct{}{}
		landmark, cached := l.cells[cell]
		if !cached {
			if ruin, ok := world.RuinAt(l.seed, cell.x, cell.z); ok {
				// Playable cells are within [-2048,2048]. Biasing each by 32768 makes this
				// injective and nonzero, even at (0,0), with no hashing collision or counter
				// reset to rename a portal on reconnect. The world seed scopes the identity.
				landmark = protocol.Landmark{LandmarkID: uint64(cell.x+32768)<<32 | uint64(cell.z+32768), X: int32(ruin.Arch.X), Z: int32(ruin.Arch.Z), Kind: vnet.LandmarkKindPortal}
			}
			l.cells[cell] = landmark
		}
		if landmark.LandmarkID == 0 {
			continue
		}
		if !l.explored.Explored(world.ChunkOf(int64(landmark.X), 0, int64(landmark.Z)).Column()) {
			continue
		}
		if _, known := l.found[landmark.LandmarkID]; !known {
			l.found[landmark.LandmarkID] = landmark
			changed = true
		}
	}
	return changed
}

func (l *landmarks) list() protocol.LandmarkList {
	list := protocol.LandmarkList{Landmarks: make([]protocol.Landmark, 0, len(l.found))}
	for _, landmark := range l.found {
		list.Landmarks = append(list.Landmarks, landmark)
	}
	slices.SortFunc(list.Landmarks, func(a, b protocol.Landmark) int { return cmp.Compare(a.LandmarkID, b.LandmarkID) })
	return list
}

// SendLandmarks sends the initial complete list, empty included, after welcome and
// before the streaming goroutine starts. Subsequent replacements are sent only by
// sendExplored when new server-recorded exploration discovers another arch.
func (s *Streamer) SendLandmarks() error {
	s.landmarks = newLandmarks(s.cache.Seed(), s.explored)
	return s.sendLandmarks()
}

func (s *Streamer) sendLandmarks() error {
	frame, err := protocol.EncodeLandmarkList(s.landmarks.list())
	if err != nil {
		return fmt.Errorf("session: encode discovered landmarks: %w", err)
	}
	if err := s.send(frame); err != nil {
		return fmt.Errorf("session: send discovered landmarks: %w", err)
	}
	return nil
}
