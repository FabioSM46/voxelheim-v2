package game

import (
	"cmp"
	"container/heap"
	"context"
	"errors"
	"fmt"

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
		heap.Push(&s.waterDue, waterDueEntry{at: at, due: due})
	}
}

// waterDueEntry is one scheduled voxel and the tick it is due on.
type waterDueEntry struct {
	at  waterVoxel
	due uint64
}

// waterDueQueue orders the schedule by due tick and then in space, and is what replaced
// rebuilding that order from scratch every tick.
//
// **The rebuild was the scaling defect behind the walking stutter, and it hid behind the
// very thing that caused the stutter.** [Sim.advanceWaterLocked] used to iterate the
// whole of [Sim.pendingWater] and sort it on every tick. That is O(n log n) per tick, and
// it was invisible only because the unbounded drain emptied the schedule every tick, so n
// was near zero — at the price of the 24 ms spike. Bounding the drain without fixing this
// made it far worse rather than better: measured, a 128-voxel budget let the schedule
// grow, the per-tick rebuild grew with it, the drain slowed further, and the tick went
// 51 -> 129 ms and climbing while the client still drew 300 frames a second. A cap on the
// work is only safe once taking N items does not cost the length of the queue.
//
// **The order changed, deliberately, and the bound is why.** The rebuild sorted by
// [compareWaterVoxels] alone — y before x before z — and ignored the due tick entirely,
// because with no cap everything due was examined on the same tick and the order among
// due ticks could not matter. This queue orders by **due tick first** and then by that
// same spatial comparison, so two voxels due at different ticks are now examined in the
// order they became due rather than in the order of their coordinates.
//
// That is not a cost of the queue; it is what makes the cap safe. Under a bound, pure
// spatial order starves: a voxel high in y waits behind every lower one that keeps
// arriving, for as long as water keeps being scheduled, and nothing guarantees it is ever
// reached. Ordering by due tick is what bounds how long a scheduled voxel waits, and the
// spatial order is kept underneath it so that within one tick's worth of work the lower
// voxel is still decided first — which is the half the automaton's rules actually reason
// about, since water settles downward.
//
// [TestTheScheduleIsTakenInDueOrderThenBottomUp] pins both halves.
type waterDueQueue []waterDueEntry

func (q waterDueQueue) Len() int { return len(q) }

func (q waterDueQueue) Less(i, j int) bool {
	if q[i].due != q[j].due {
		return q[i].due < q[j].due
	}
	return compareWaterVoxels(q[i].at, q[j].at) < 0
}

func (q waterDueQueue) Swap(i, j int) { q[i], q[j] = q[j], q[i] }

func (q *waterDueQueue) Push(x any) { *q = append(*q, x.(waterDueEntry)) }

func (q *waterDueQueue) Pop() any {
	old := *q
	last := old[len(old)-1]
	*q = old[:len(old)-1]
	return last
}

func (s *Sim) advanceWaterLocked(worldTick uint64) []WaterChange {
	if s.waterWorld == nil {
		return nil
	}
	s.drainUnstableWaterLocked(worldTick)

	changes := make([]WaterChange, 0, WaterChangesPerTick)
	examined := 0
	for s.waterDue.Len() > 0 {
		// Two caps, and the second one is the one that binds. See
		// [WaterVoxelsPerTick]: a voxel that does not change is deleted below and costs
		// the same seven reads as one that does, so bounding the changes alone bounds
		// nothing on the pass that dominates this — a chunk composition, where almost
		// nothing changes and there are thousands of them.
		if len(changes) == WaterChangesPerTick || examined == WaterVoxelsPerTick {
			break
		}
		if s.waterDue[0].due > worldTick {
			break
		}
		entry := heap.Pop(&s.waterDue).(waterDueEntry)
		at := entry.at
		// A voxel rescheduled earlier than it was pushed leaves its old entry behind.
		// The map is the authority on when a voxel is due, so an entry that disagrees
		// with it is stale and costs nothing but this comparison.
		if scheduled, ok := s.pendingWater[at]; !ok || scheduled != entry.due {
			continue
		}
		examined++

		here, ok := s.waterBlockLocked(at, world.Stone)
		if !ok {
			// The voxel's own chunk is not resident: there is no water here to decide
			// and nothing to lose. This is the one drop whose recovery story is real —
			// when the chunk is composed again the cache marks it, `UnstableWater`
			// scans it, and *its own* water is what that scan schedules.
			delete(s.pendingWater, at)
			continue
		}

		// **A voxel is decided only from a neighbourhood that was actually read
		// (#717).** Every fabricated fallback is a lie [world.NextWater] believes,
		// and each was tried: Stone above a side hid a falling column from the
		// anti-cone rule and rebuilt the pre-#653 widening cone at every residency
		// seam; a fabricated Air below would pour a waterfall into ground that may be
		// solid, and Stone below would hold up water that has nothing under it.
		//
		// Dropping the voxel instead was the previous answer, and its recovery story
		// was one-sided: composing the *missing* chunk scans that chunk's own water,
		// which does not include a voxel one chunk over and need not reach it — a
		// fall descending toward a residency seam was truncated there and hung in the
		// air for ever. So an unreadable neighbourhood defers the voxel: rescheduled
		// at [WaterResidencyRetryDelay], it is examined again until the world around
		// it can be read. The retry is what makes the deferral safe, and the deferral
		// is what makes every block below a real read.
		readable := true
		read := func(v waterVoxel, absent world.Block) world.Block {
			block, resident := s.waterBlockLocked(v, absent)
			readable = readable && resident
			return block
		}
		above := read(waterVoxel{x: at.x, y: at.y + 1, z: at.z}, world.Stone)
		below := read(waterVoxel{x: at.x, y: at.y - 1, z: at.z}, world.Air)
		var sides, sidesAbove [4]world.Block
		for i, offset := range [4]waterVoxel{{x: 1}, {x: -1}, {z: 1}, {z: -1}} {
			side := waterVoxel{x: at.x + offset.x, y: at.y, z: at.z + offset.z}
			sides[i] = read(side, world.Stone)
			sidesAbove[i] = read(waterVoxel{x: side.x, y: side.y + 1, z: side.z}, world.Stone)
		}
		if !readable {
			delete(s.pendingWater, at)
			s.scheduleWaterLocked(at, worldTick+WaterResidencyRetryDelay)
			continue
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
			// Evicted between the read and the write: deferred like an unreadable
			// neighbour. If the chunk never comes back, the retry finds the own-chunk
			// read failing above and hands the voxel to the composition scan.
			delete(s.pendingWater, at)
			s.scheduleWaterLocked(at, worldTick+WaterResidencyRetryDelay)
		case errors.Is(err, errWaterReadChanged):
			// Somebody wrote this voxel between the read and the write — a player
			// edit applies through the cache without this lock. Decide it again from
			// what stands there now. **Doing nothing here stranded the voxel for
			// ever (#717)**: its heap entry was already popped, and a map entry with
			// no heap row behind it blocks every future push, because
			// [Sim.scheduleWaterLocked] only pushes a due that beats the map. Frozen
			// mid-drain water is what that looked like on screen.
			delete(s.pendingWater, at)
			s.scheduleWaterLocked(at, worldTick+WaterTickDelay)
		default:
			s.log.Error("water change did not finish cleanly", "pos", [3]int64{at.x, at.y, at.z}, "error", err)
			// An error is not a decision. Same strand as above, same repair.
			delete(s.pendingWater, at)
			s.scheduleWaterLocked(at, worldTick+WaterResidencyRetryDelay)
		}
	}
	return changes
}

// drainUnstableWaterLocked schedules as much of the pending composition scans as this
// tick has budget for, and keeps the rest.
//
// **It used to drain the whole channel, and that was half the stutter.** A scan of a
// water-heavy chunk carries thousands of indices and each schedules seven neighbours, so
// one tick could do fifty thousand map operations before a single voxel was examined.
// See [WaterScansPerTick] for the measurement and for why the bound is a count.
//
// The tail of a part-taken batch is kept in [Sim.waterScanCarry] rather than pushed back
// on the channel: a channel put under the simulation lock is a place to deadlock, and a
// scan half-scheduled twice would schedule its first half twice.
func (s *Sim) drainUnstableWaterLocked(worldTick uint64) {
	budget := WaterScansPerTick
	for budget > 0 {
		if len(s.waterScanCarry.indices) == 0 {
			select {
			case batch := <-s.unstableWater:
				s.waterScanCarry = batch
			default:
				return
			}
		}
		take := min(budget, len(s.waterScanCarry.indices))
		for _, index := range s.waterScanCarry.indices[:take] {
			if index < 0 || index >= world.ChunkVolume {
				continue
			}
			x, y, z := waterWorldPosition(s.waterScanCarry.coord, index)
			s.scheduleWaterAroundLocked(waterVoxel{x: x, y: y, z: z}, worldTick)
		}
		s.waterScanCarry.indices = s.waterScanCarry.indices[take:]
		budget -= take
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
