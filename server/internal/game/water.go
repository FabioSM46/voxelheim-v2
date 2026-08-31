package game

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"slices"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

const waterScanQueueDepth = 64

var errWaterReadChanged = errors.New("water neighbourhood changed before its write")

var waterNeighbourOffsets = [7]waterVoxel{
	{},
	{x: 1}, {x: -1},
	{y: 1}, {y: -1},
	{z: 1}, {z: -1},
}

// WaterWorld exposes resident water access.
type WaterWorld interface {
	Peek(world.Coord) (*world.Chunk, error)
	ApplyResidentGuarded(x, y, z int64, block world.Block, allow func(current world.Block) error) error
}

// WaterChange is one water mutation.
type WaterChange struct {
	Coord world.Coord
	Index int
	Block world.Block
}

func (c WaterChange) Pos() [3]int32 {
	x, y, z := waterWorldPosition(c.Coord, c.Index)
	return [3]int32{int32(x), int32(y), int32(z)}
}

type waterVoxel struct {
	x, y, z int64
}

type unstableWaterBatch struct {
	coord   world.Coord
	indices []int
}

// ConfigureWater wires the resident cache.
func (s *Sim) ConfigureWater(cache WaterWorld) error {
	if cache == nil {
		return errors.New("game: water cache must not be nil")
	}

	s.mu.Lock()
	defer s.mu.Unlock()
	if s.waterWorld != nil {
		return errors.New("game: water is already configured")
	}
	s.waterWorld = cache
	return nil
}

// QueueUnstableWater hands a scan to the tick.
func (s *Sim) QueueUnstableWater(ctx context.Context, coord world.Coord, indices []int) error {
	if len(indices) == 0 {
		return nil
	}
	if err := ctx.Err(); err != nil {
		return err
	}

	batch := unstableWaterBatch{coord: coord, indices: append([]int(nil), indices...)}
	select {
	case s.unstableWater <- batch:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

func (s *Sim) scheduleWaterEdit(at waterVoxel) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.scheduleWaterAroundLocked(at, s.worldTick)
}

func (s *Sim) scheduleWaterAroundLocked(at waterVoxel, due uint64) {
	if s.waterWorld == nil {
		return
	}
	for _, offset := range waterNeighbourOffsets {
		s.scheduleWaterLocked(waterVoxel{x: at.x + offset.x, y: at.y + offset.y, z: at.z + offset.z}, due)
	}
}

func (s *Sim) scheduleWaterLocked(at waterVoxel, due uint64) {
	if at.x < -world.BlockLimit || at.x > world.BlockLimit ||
		at.y < -world.BlockLimit || at.y > world.BlockLimit ||
		at.z < -world.BlockLimit || at.z > world.BlockLimit {
		return
	}
	if old, exists := s.pendingWater[at]; !exists || due < old {
		s.pendingWater[at] = due
	}
}

func (s *Sim) advanceWaterLocked(worldTick uint64) []WaterChange {
	if s.waterWorld == nil {
		return nil
	}
	s.drainUnstableWaterLocked(worldTick)

	due := make([]waterVoxel, 0)
	for at, atTick := range s.pendingWater {
		if atTick <= worldTick {
			due = append(due, at)
		}
	}
	slices.SortFunc(due, compareWaterVoxels)

	changes := make([]WaterChange, 0, min(len(due), WaterChangesPerTick))
	for _, at := range due {
		if len(changes) == WaterChangesPerTick {
			break
		}

		here, ok := s.waterBlockLocked(at, world.Stone)
		if !ok {
			delete(s.pendingWater, at)
			continue
		}
		above, _ := s.waterBlockLocked(waterVoxel{x: at.x, y: at.y + 1, z: at.z}, world.Stone)

		// **The one neighbour whose absence cannot be defaulted, since #653.** Every
		// other read here falls back to a block that makes [world.NextWater] answer
		// conservatively: Stone above is not water, and Stone on a side carries no
		// water level, so an unread neighbour supplies nothing and starts nothing.
		// Below had the same property while the rule's only use for it was "is this
		// water unsupported" — Air meant drain, and draining a cell that was already
		// Air wrote nothing.
		//
		// It stopped having it the moment a cell over a void became the head of a
		// fall. Air below now *enables* a write rather than suppressing one, so a
		// fabricated Air under a chunk nobody has loaded would pour a waterfall into
		// ground that may be solid. Defaulting to Stone instead would only move the
		// lie: unsupported water over an unloaded chunk would then spread rather than
		// drain.
		//
		// So this voxel is not decided at all. Dropped from the schedule rather than
		// retried, exactly as an [world.ErrNotResident] *write* is below and for the
		// same reason: when that chunk is composed the cache marks it, `UnstableWater`
		// scans it, and the neighbourhood — this voxel included — is scheduled again
		// from a world that can be read.
		below, belowResident := s.waterBlockLocked(waterVoxel{x: at.x, y: at.y - 1, z: at.z}, world.Air)
		if !belowResident {
			delete(s.pendingWater, at)
			continue
		}
		// The four sides, and what each of them is standing on. The second half is
		// what tells [world.NextWater] that a side is a column on its way down rather
		// than water spread across a floor — see the measurement at that function.
		//
		// Both default to Stone when the chunk is not resident, and the two defaults
		// mean opposite-looking things that are the same thing: an unread *side* is
		// Stone, which carries no water level and so feeds nothing; an unread block
		// *under* a side is Stone, which reads as support, so a side that is real
		// water feeds exactly as it did before #653. Neither default can invent a
		// fall — that is what the residency guard on `below` above is for.
		var sides, sidesAbove [4]world.Block
		for i, offset := range [4]waterVoxel{{x: 1}, {x: -1}, {z: 1}, {z: -1}} {
			side := waterVoxel{x: at.x + offset.x, y: at.y, z: at.z + offset.z}
			sides[i], _ = s.waterBlockLocked(side, world.Stone)
			sidesAbove[i], _ = s.waterBlockLocked(waterVoxel{x: side.x, y: side.y + 1, z: side.z}, world.Stone)
		}

		next := world.NextWater(here, above, below, sides, sidesAbove)
		if next == here {
			delete(s.pendingWater, at)
			continue
		}

		err := s.waterWorld.ApplyResidentGuarded(at.x, at.y, at.z, next, func(current world.Block) error {
			if current != here {
				return fmt.Errorf("%w: read %d, found %d", errWaterReadChanged, here, current)
			}
			return nil
		})
		switch {
		case err == nil:
			delete(s.pendingWater, at)
			s.scheduleWaterAroundLocked(at, worldTick+WaterTickDelay)
			coord := world.ChunkOf(at.x, at.y, at.z)
			changes = append(changes, WaterChange{
				Coord: coord,
				Index: world.Index(world.Local(at.x), world.Local(at.y), world.Local(at.z)),
				Block: next,
			})
		case errors.Is(err, world.ErrNotResident):
			delete(s.pendingWater, at)
		case errors.Is(err, errWaterReadChanged):
		default:
			s.log.Error("water change did not finish cleanly", "pos", [3]int64{at.x, at.y, at.z}, "error", err)
		}
	}
	return changes
}

func (s *Sim) drainUnstableWaterLocked(worldTick uint64) {
	for {
		select {
		case batch := <-s.unstableWater:
			for _, index := range batch.indices {
				if index < 0 || index >= world.ChunkVolume {
					continue
				}
				x, y, z := waterWorldPosition(batch.coord, index)
				s.scheduleWaterAroundLocked(waterVoxel{x: x, y: y, z: z}, worldTick)
			}
		default:
			return
		}
	}
}

func (s *Sim) waterBlockLocked(at waterVoxel, absent world.Block) (world.Block, bool) {
	chunk, err := s.waterWorld.Peek(world.ChunkOf(at.x, at.y, at.z))
	if err != nil {
		if !errors.Is(err, world.ErrNotResident) {
			s.log.Error("water neighbourhood could not be read", "pos", [3]int64{at.x, at.y, at.z}, "error", err)
		}
		return absent, false
	}
	return chunk.At(world.Local(at.x), world.Local(at.y), world.Local(at.z)), true
}

func compareWaterVoxels(a, b waterVoxel) int {
	if byY := cmp.Compare(a.y, b.y); byY != 0 {
		return byY
	}
	if byX := cmp.Compare(a.x, b.x); byX != 0 {
		return byX
	}
	return cmp.Compare(a.z, b.z)
}

func waterWorldPosition(coord world.Coord, index int) (x, y, z int64) {
	localX := index % world.ChunkSize
	localZ := index / world.ChunkSize % world.ChunkSize
	localY := index / (world.ChunkSize * world.ChunkSize)
	originX, originY, originZ := coord.Origin()
	return originX + int64(localX), originY + int64(localY), originZ + int64(localZ)
}
