package game

import (
	"container/heap"
	"context"
	"log/slog"
	"slices"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func newWaterSim(t testing.TB) (*Sim, *world.Cache) {
	t.Helper()
	cache := world.NewCache(73, 1, 16)
	if _, _, err := cache.Get(context.Background(), world.Coord{}); err != nil {
		t.Fatalf("Get water fixture chunk: %v", err)
	}
	sim, err := NewSim(DefaultTickRate, 1, testWorldSeed, NewCacheTerrain(cache), cache, testEntityIDs(), slog.New(slog.DiscardHandler))
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}
	if err := sim.ConfigureWater(cache); err != nil {
		t.Fatalf("ConfigureWater: %v", err)
	}
	return sim, cache
}

func setWaterFixtureBlock(t testing.TB, cache *world.Cache, at waterVoxel, block world.Block) {
	t.Helper()
	if err := cache.ApplyResidentGuarded(at.x, at.y, at.z, block, nil); err != nil {
		t.Fatalf("set fixture block at %+v to %d: %v", at, block, err)
	}
}

func waterBlockAt(t testing.TB, cache *world.Cache, at waterVoxel) world.Block {
	t.Helper()
	chunk, err := cache.Peek(world.ChunkOf(at.x, at.y, at.z))
	if err != nil {
		t.Fatalf("Peek block at %+v: %v", at, err)
	}
	return chunk.At(world.Local(at.x), world.Local(at.y), world.Local(at.z))
}

func scheduleWaterNow(sim *Sim, positions ...waterVoxel) {
	sim.mu.Lock()
	defer sim.mu.Unlock()
	for _, at := range positions {
		sim.scheduleWaterLocked(at, sim.worldTick)
	}
}

func TestWaterFallsThenSpreadsAndAPlugDrainsIt(t *testing.T) {
	t.Parallel()

	sim, cache := newWaterSim(t)
	source := waterVoxel{x: 16, y: 20, z: 16}
	setWaterFixtureBlock(t, cache, source, world.Water)
	for y := int64(14); y < source.y; y++ {
		setWaterFixtureBlock(t, cache, waterVoxel{x: source.x, y: y, z: source.z}, world.Air)
	}
	for y := int64(15); y < source.y; y++ {
		setWaterFixtureBlock(t, cache, waterVoxel{x: source.x + 1, y: y, z: source.z}, world.Stone)
		setWaterFixtureBlock(t, cache, waterVoxel{x: source.x - 1, y: y, z: source.z}, world.Stone)
		setWaterFixtureBlock(t, cache, waterVoxel{x: source.x, y: y, z: source.z + 1}, world.Stone)
		setWaterFixtureBlock(t, cache, waterVoxel{x: source.x, y: y, z: source.z - 1}, world.Stone)
	}
	setWaterFixtureBlock(t, cache, waterVoxel{x: source.x, y: 13, z: source.z}, world.Stone)
	for x := int64(13); x <= 19; x++ {
		setWaterFixtureBlock(t, cache, waterVoxel{x: x, y: 13, z: source.z}, world.Stone)
		setWaterFixtureBlock(t, cache, waterVoxel{x: x, y: 14, z: source.z}, world.Air)
	}

	scheduleWaterNow(sim, waterVoxel{x: source.x, y: source.y - 1, z: source.z})
	for tick := uint64(1); tick <= 7*WaterTickDelay; tick++ {
		sim.Step(tick)
	}
	if got := waterBlockAt(t, cache, waterVoxel{x: source.x, y: 14, z: source.z}); got != world.WaterFlow7 {
		t.Fatalf("bottom of shaft = %d, want falling WaterFlow7", got)
	}
	if got := waterBlockAt(t, cache, waterVoxel{x: source.x + 1, y: 14, z: source.z}); got != world.WaterFlow7 {
		t.Fatalf("first floor step = %d, want falling WaterFlow7 from the supplied column above", got)
	}

	for _, side := range [][2]int64{{1, 0}, {-1, 0}, {0, 1}, {0, -1}} {
		setWaterFixtureBlock(t, cache, waterVoxel{x: source.x + side[0], y: 14, z: source.z + side[1]}, world.Stone)
	}
	setWaterFixtureBlock(t, cache, waterVoxel{x: source.x, y: source.y - 1, z: source.z}, world.Stone)
	sim.scheduleWaterEdit(waterVoxel{x: source.x, y: source.y - 1, z: source.z})
	drainedSupply := false
	for tick := uint64(1); tick <= 6*WaterTickDelay; tick++ {
		sim.Step(100 + tick)
		if waterBlockAt(t, cache, waterVoxel{x: source.x, y: 15, z: source.z}) == world.Air {
			drainedSupply = true
			break
		}
	}
	if !drainedSupply {
		t.Fatal("the plugged shaft kept supplying its basin")
	}
	lastSupply := sim.worldTick
	for tick := uint64(1); tick <= 7*WaterTickDelay; tick++ {
		sim.Step(200 + tick)
	}
	if got := waterBlockAt(t, cache, waterVoxel{x: source.x, y: 14, z: source.z}); got != world.Air {
		t.Fatalf("basin remains block %d after %d ticks without supply, want Air", got, sim.worldTick-lastSupply)
	}
}

func TestCompositionScanSchedulesAirBesideWater(t *testing.T) {
	t.Parallel()

	sim, cache := newWaterSim(t)
	source := waterVoxel{x: 8, y: 20, z: 8}
	target := waterVoxel{x: 8, y: 19, z: 8}
	setWaterFixtureBlock(t, cache, source, world.Water)
	setWaterFixtureBlock(t, cache, target, world.Air)
	index := world.Index(world.Local(source.x), world.Local(source.y), world.Local(source.z))
	if err := sim.QueueUnstableWater(context.Background(), world.Coord{}, []int{index}); err != nil {
		t.Fatalf("QueueUnstableWater: %v", err)
	}
	if changes := sim.Step(1); len(changes) != 1 {
		t.Fatalf("initial composition scan changed %d voxels, want boundary Air to flow", len(changes))
	}
	if got := waterBlockAt(t, cache, target); got != world.WaterFlow7 {
		t.Fatalf("Air below scanned source became %d, want WaterFlow7", got)
	}
}

func TestWaterChangesStopAtNinetySixAndKeepTheOrderedSuffixDue(t *testing.T) {
	t.Parallel()

	sim, cache := newWaterSim(t)
	positions := make([]waterVoxel, 0, WaterChangesPerTick+1)
	for x := int64(1); x < world.ChunkSize-1 && len(positions) < WaterChangesPerTick+1; x++ {
		for z := int64(1); z < world.ChunkSize-1 && len(positions) < WaterChangesPerTick+1; z++ {
			at := waterVoxel{x: x, y: 20, z: z}
			positions = append(positions, at)
			setWaterFixtureBlock(t, cache, at, world.Air)
			setWaterFixtureBlock(t, cache, waterVoxel{x: at.x, y: at.y + 1, z: at.z}, world.Water)
		}
	}
	for i, j := 0, len(positions)-1; i < j; i, j = i+1, j-1 {
		positions[i], positions[j] = positions[j], positions[i]
	}
	scheduleWaterNow(sim, positions...)

	first := sim.Step(1)
	if len(first) != WaterChangesPerTick {
		t.Fatalf("first tick changed %d voxels, want hard cap %d", len(first), WaterChangesPerTick)
	}
	if !waterChangesOrdered(first) {
		t.Fatal("first tick water changes are not ordered by (y, x, z)")
	}
	second := sim.Step(2)
	if len(second) != 1 {
		t.Fatalf("second tick changed %d voxels, want the one due suffix entry", len(second))
	}
	all := append(append([]WaterChange(nil), first...), second...)
	if !waterChangesOrdered(all) {
		t.Fatal("the due suffix did not retain coordinate order on the next tick")
	}
}

func TestANonResidentNeighbourNeitherReceivesNorSupportsWater(t *testing.T) {
	t.Parallel()

	sim, cache := newWaterSim(t)
	at := waterVoxel{x: 0, y: 0, z: 10}
	setWaterFixtureBlock(t, cache, at, world.Air)
	setWaterFixtureBlock(t, cache, waterVoxel{x: 1, y: 0, z: 10}, world.WaterFlow6)
	scheduleWaterNow(sim, at, waterVoxel{x: -1, y: 0, z: 10})

	if changes := sim.Step(1); len(changes) != 0 {
		t.Fatalf("non-resident boundary produced %+v, want no writes", changes)
	}
	if got := waterBlockAt(t, cache, at); got != world.Air {
		t.Fatalf("missing floor supported side water into block %d, want Air", got)
	}
	if got := cache.Len(); got != 1 {
		t.Fatalf("water pass generated a neighbour: cache has %d chunks, want 1", got)
	}

	// And the two halves part ways on the schedule (#717): the voxel whose *own*
	// chunk is unread is dropped — its chunk's composition scan is what brings it
	// back — while the voxel that could be read but not decided is deferred, because
	// nothing else will ever return for it.
	sim.mu.Lock()
	_, deferred := sim.pendingWater[at]
	_, dropped := sim.pendingWater[waterVoxel{x: -1, y: 0, z: 10}]
	sim.mu.Unlock()
	if !deferred {
		t.Fatal("the undecidable voxel left the schedule; dropped is how #717's water froze")
	}
	if dropped {
		t.Fatal("the voxel in the unread chunk stayed scheduled; its composition scan owns it")
	}
}

func TestTheSameWaterScheduleProducesTheSameWorld(t *testing.T) {
	t.Parallel()

	run := func() []world.Block {
		sim, cache := newWaterSim(t)
		for x := int64(8); x <= 12; x++ {
			setWaterFixtureBlock(t, cache, waterVoxel{x: x, y: 9, z: 8}, world.Stone)
			setWaterFixtureBlock(t, cache, waterVoxel{x: x, y: 10, z: 8}, world.Air)
		}
		setWaterFixtureBlock(t, cache, waterVoxel{x: 10, y: 11, z: 8}, world.Water)
		positions := []waterVoxel{{x: 10, y: 10, z: 8}, {x: 11, y: 10, z: 8}, {x: 9, y: 10, z: 8}}
		scheduleWaterNow(sim, positions...)
		for tick := uint64(1); tick <= 8*WaterTickDelay; tick++ {
			sim.Step(tick)
		}
		chunk, err := cache.Peek(world.Coord{})
		if err != nil {
			t.Fatalf("Peek final chunk: %v", err)
		}
		return slices.Clone(chunk.Blocks)
	}

	if first, second := run(), run(); !slices.Equal(first, second) {
		t.Fatal("identical world and edit sequence produced different final blocks")
	}
}

func waterChangesOrdered(changes []WaterChange) bool {
	for i := 1; i < len(changes); i++ {
		previous := changeVoxel(changes[i-1])
		current := changeVoxel(changes[i])
		if compareWaterVoxels(previous, current) > 0 {
			return false
		}
	}
	return true
}

func changeVoxel(change WaterChange) waterVoxel {
	x, y, z := waterWorldPosition(change.Coord, change.Index)
	return waterVoxel{x: x, y: y, z: z}
}

type benchmarkWaterWorld struct{ chunk *world.Chunk }

func (w *benchmarkWaterWorld) Peek(coord world.Coord) (*world.Chunk, error) {
	if coord != w.chunk.Coord {
		return nil, world.ErrNotResident
	}
	return w.chunk, nil
}

func (w *benchmarkWaterWorld) ApplyResidentGuarded(x, y, z int64, block world.Block, allow func(world.Block) error) error {
	if err := allow(w.chunk.At(world.Local(x), world.Local(y), world.Local(z))); err != nil {
		return err
	}
	w.chunk.Set(world.Local(x), world.Local(y), world.Local(z), block)
	return nil
}

func BenchmarkWaterTick(b *testing.B) {
	cache := world.NewCache(73, 1, 1)
	sim, err := NewSim(DefaultTickRate, 1, testWorldSeed, NewCacheTerrain(cache), cache, testEntityIDs(), slog.New(slog.DiscardHandler))
	if err != nil {
		b.Fatalf("NewSim: %v", err)
	}
	water := &benchmarkWaterWorld{chunk: world.NewChunk(world.Coord{})}
	if err := sim.ConfigureWater(water); err != nil {
		b.Fatalf("ConfigureWater: %v", err)
	}
	for x := int64(0); x < 32; x++ {
		for z := int64(0); z < 32; z++ {
			water.chunk.Set(int(x), 31, int(z), world.Water)
		}
	}

	// Interior voxels only: a target on the chunk face reads a side from a chunk the
	// fixture never makes resident, and since #717 that defers the voxel instead of
	// deciding it, which would turn this into a benchmark of the retry path.
	targets := make([]waterVoxel, 0, WaterChangesPerTick)
	for x := int64(1); x < 31 && len(targets) < WaterChangesPerTick; x++ {
		for z := int64(1); z < 31 && len(targets) < WaterChangesPerTick; z++ {
			targets = append(targets, waterVoxel{x: x, y: 30, z: z})
		}
	}

	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		b.StopTimer()
		for _, at := range targets {
			water.chunk.Set(int(at.x), int(at.y), int(at.z), world.Air)
		}
		sim.mu.Lock()
		clear(sim.pendingWater)
		for _, at := range targets {
			sim.scheduleWaterLocked(at, sim.worldTick)
		}
		sim.mu.Unlock()
		b.StartTimer()
		if got := len(sim.Step(uint64(i + 1))); got != WaterChangesPerTick {
			b.Fatalf("water tick changed %d voxels, want %d", got, WaterChangesPerTick)
		}
	}
}

// The other half of the residency guard: a voxel deferred because the chunk under it
// could not be read must decide itself once that chunk can be — with nobody's help.
//
// **The old contract dropped the voxel, and its recovery story was not true (#717).**
// Composing the chunk below scans that chunk's *own* water; a voxel one chunk up is
// not in that scan and, unless the composed chunk happens to hold water on its top
// face, nothing the scan schedules ever reaches the dropped voxel. The first version
// of this test only passed by queueing a scan of the voxel's own chunk by hand — a
// scan nothing performs at runtime — and a fall descending toward a residency seam
// hung truncated in the air for ever. The deferral makes the round trip real: the
// voxel stays scheduled, retries after [WaterResidencyRetryDelay], and is decided
// from real blocks the first time every read answers.
func TestAVoxelDeferredForResidencyIsDecidedOnceItsGroundArrives(t *testing.T) {
	t.Parallel()

	sim, cache := newWaterSim(t)
	// y = 0 is the bottom layer of chunk Y=0, so its `below` lives in chunk Y=-1,
	// which newWaterSim does not make resident.
	at := waterVoxel{x: 4, y: 0, z: 12}
	setWaterFixtureBlock(t, cache, at, world.Air)
	setWaterFixtureBlock(t, cache, waterVoxel{x: 5, y: 0, z: 12}, world.Water)
	scheduleWaterNow(sim, at)

	if changes := sim.Step(1); len(changes) != 0 {
		t.Fatalf("with the ground unread the pass produced %+v, want no writes", changes)
	}
	if got := waterBlockAt(t, cache, at); got != world.Air {
		t.Fatalf("a voxel over an unread chunk became %d, want it left as Air", got)
	}
	sim.mu.Lock()
	due, stillPending := sim.pendingWater[at]
	sim.mu.Unlock()
	if !stillPending {
		t.Fatal("the deferred voxel left the schedule; dropped is how #717's falls froze in the air")
	}
	if due <= 1 || due > 1+WaterResidencyRetryDelay {
		t.Fatalf("the deferred voxel is due at tick %d, want a bounded retry within %d ticks",
			due, WaterResidencyRetryDelay)
	}

	// The chunk below arrives, delivered exactly as the runtime delivers it: composed,
	// and its own water scanned. Deliberately nothing scans the voxel's chunk again —
	// the retry is the whole of the recovery.
	below := world.ChunkOf(at.x, at.y-1, at.z)
	chunk, _, err := cache.Get(context.Background(), below)
	if err != nil {
		t.Fatalf("compose the chunk below: %v", err)
	}
	for _, composed := range cache.TakeWaterCompositions() {
		if err := sim.QueueUnstableWater(context.Background(), composed.Coord, world.UnstableWater(composed)); err != nil {
			t.Fatalf("QueueUnstableWater: %v", err)
		}
	}

	decided := false
	for tick := uint64(2); tick <= 2+WaterResidencyRetryDelay+4*WaterTickDelay; tick++ {
		sim.Step(tick)
		if waterBlockAt(t, cache, at) != world.Air {
			decided = true
			break
		}
	}
	if !decided {
		t.Fatal("the voxel was never decided after its ground became readable")
	}
	if got := waterBlockAt(t, cache, at); got != world.WaterFlow7 {
		t.Errorf("the voxel became %d, want %d spread from the source beside it", got, world.WaterFlow7)
	}
	// **That it was decided at all is the proof it was decided from a real read**: the
	// guard makes an unread chunk below the one thing that stops this voxel being
	// written, so a write here cannot have come from a fallback. What is actually under
	// it at this column is an aquifer rather than stone — either supports water, and
	// neither is Air, which is the case [TestNothingUnderTheSurfaceIsEverAir] rules out
	// generally.
	if got := chunk.At(world.Local(at.x), world.Local(at.y-1), world.Local(at.z)); got == world.Air {
		t.Fatalf("the chunk below composed to Air under the voxel, which the generator never produces")
	}
}

// There is no world floor to strand water on: everything under a column's surface is
// generated ground, all the way down, so a water voxel never has an unreadable chunk
// under it for want of one existing. A deferred voxel's retry therefore always has a
// chunk that *can* arrive — a point worth pinning rather than reasoning about, because
// since #717 the residency guard parks work instead of dropping it, and a park with no
// possible arrival would poll for ever.
func TestNothingUnderTheSurfaceIsEverAir(t *testing.T) {
	t.Parallel()

	cache := world.NewCache(73, 4, 16)
	for _, coord := range []world.Coord{{}, {Y: -1}, {Y: -4}, {X: 3, Y: -2, Z: -5}} {
		chunk, _, err := cache.Get(context.Background(), coord)
		if err != nil {
			t.Fatalf("Get %+v: %v", coord, err)
		}
		originX, originY, originZ := coord.Origin()
		for x := range world.ChunkSize {
			for z := range world.ChunkSize {
				surface := world.GeneratedColumnTop(73, originX+int64(x), originZ+int64(z))
				for y := range world.ChunkSize {
					worldY := originY + int64(y)
					if worldY >= int64(surface) {
						continue
					}
					if got := chunk.At(x, y, z); got == world.Air {
						t.Fatalf("air at (%d, %d, %d), %d below the column top",
							originX+int64(x), worldY, originZ+int64(z), int64(surface)-worldY)
					}
				}
			}
		}
	}
}

// One composition scan must not be taken whole, however big it is.
//
// **This is the walking stutter, reduced to a test.** A chunk composed inside a lake
// hands the simulation thousands of voxels at once; before #665 one tick scheduled every
// one of them and examined every voxel they scheduled, which measured 24.5 ms on an
// authoritative tick whose whole period is 50 ms. The bound is on the work, so the test
// is on the work: how many voxels one pass may touch, not how long it took, because a
// wall clock on a shared machine is a reading and this is a promise.
func TestOneCompositionScanIsSpreadOverTicks(t *testing.T) {
	t.Parallel()

	sim, cache := newWaterSim(t)
	// A slab of water with air beside it, so every voxel of it is genuinely unstable
	// and the scan is the size of the slab rather than of what happens to be wet.
	indices := make([]int, 0, world.ChunkSize*world.ChunkSize)
	for z := range world.ChunkSize {
		for x := range world.ChunkSize {
			at := waterVoxel{x: int64(x), y: 20, z: int64(z)}
			setWaterFixtureBlock(t, cache, at, world.Water)
			setWaterFixtureBlock(t, cache, waterVoxel{x: at.x, y: at.y + 1, z: at.z}, world.Air)
			indices = append(indices, world.Index(x, 20, z))
		}
	}
	if len(indices) <= WaterScansPerTick {
		t.Fatalf("the fixture is %d voxels, which is not more than one tick's budget of %d",
			len(indices), WaterScansPerTick)
	}
	if err := sim.QueueUnstableWater(context.Background(), world.Coord{}, indices); err != nil {
		t.Fatalf("QueueUnstableWater: %v", err)
	}

	// The tick that receives the scan may schedule at most its budget, and the schedule
	// it leaves behind is what the following ticks work through.
	sim.Step(1)
	sim.mu.Lock()
	carried := len(sim.waterScanCarry.indices)
	sim.mu.Unlock()
	if carried == 0 {
		t.Fatalf("the whole %d-voxel scan was taken in one tick; nothing was carried", len(indices))
	}
	if taken := len(indices) - carried; taken > WaterScansPerTick {
		t.Errorf("one tick scheduled %d voxels of the scan, over the budget of %d",
			taken, WaterScansPerTick)
	}

	// And it does drain: the carry empties over ticks rather than being dropped.
	for tick := uint64(2); tick <= 200; tick++ {
		sim.Step(tick)
		sim.mu.Lock()
		carried = len(sim.waterScanCarry.indices)
		sim.mu.Unlock()
		if carried == 0 {
			return
		}
	}
	t.Fatalf("the scan still had %d voxels outstanding after 200 ticks", carried)
}

// The schedule is taken from in order and in bounded pieces, and a voxel rescheduled
// earlier than it was queued does not get examined twice.
func TestTheWaterScheduleIsOrderedAndStaleEntriesAreSkipped(t *testing.T) {
	t.Parallel()

	sim, _ := newWaterSim(t)
	sim.mu.Lock()
	// Queued far in the future, then pulled forward: the far entry is left behind in the
	// queue and must not be acted on when its turn comes.
	at := waterVoxel{x: 3, y: 4, z: 5}
	sim.scheduleWaterLocked(at, 500)
	sim.scheduleWaterLocked(at, 1)
	queued := sim.waterDue.Len()
	scheduled := sim.pendingWater[at]
	sim.mu.Unlock()

	if queued != 2 {
		t.Fatalf("the queue holds %d entries for one rescheduled voxel, want both kept", queued)
	}
	if scheduled != 1 {
		t.Fatalf("the schedule says tick %d, want the earlier 1", scheduled)
	}

	// The heap orders by due tick first, so the earlier entry is the one at the front.
	sim.mu.Lock()
	front := sim.waterDue[0]
	sim.mu.Unlock()
	if front.due != 1 {
		t.Errorf("the front of the queue is due at %d, want the earliest at 1", front.due)
	}
}

// The schedule is taken in due order first and bottom-up second, and both halves matter.
//
// **This ordering is a deliberate change from what the rebuild did, not an accident of
// the data structure.** [Sim.advanceWaterLocked] used to collect everything due and sort
// it by [compareWaterVoxels] alone — the due tick played no part, because with no cap
// every due voxel was examined on the same tick and the question never arose. Under
// [WaterVoxelsPerTick] it arises on every tick, and the answer has to be due-first or a
// voxel can be starved indefinitely by lower ones arriving behind it.
func TestTheScheduleIsTakenInDueOrderThenBottomUp(t *testing.T) {
	t.Parallel()

	sim, _ := newWaterSim(t)
	sim.mu.Lock()
	defer sim.mu.Unlock()

	// High in y and due early, against low in y and due late. Under the old spatial-only
	// order the low one would have been examined first; under this one the early one is.
	early := waterVoxel{x: 0, y: 90, z: 0}
	late := waterVoxel{x: 0, y: 10, z: 0}
	sim.scheduleWaterLocked(late, 9)
	sim.scheduleWaterLocked(early, 3)

	// And two at the same due tick, where the spatial order is the one that decides.
	lower := waterVoxel{x: 5, y: 20, z: 5}
	higher := waterVoxel{x: 5, y: 40, z: 5}
	sim.scheduleWaterLocked(higher, 3)
	sim.scheduleWaterLocked(lower, 3)

	var order []waterVoxel
	for sim.waterDue.Len() > 0 {
		order = append(order, heap.Pop(&sim.waterDue).(waterDueEntry).at)
	}

	want := []waterVoxel{lower, higher, early, late}
	if len(order) != len(want) {
		t.Fatalf("the queue gave %d entries, want %d", len(order), len(want))
	}
	for i, at := range want {
		if order[i] != at {
			t.Fatalf("entry %d is %+v, want %+v (full order %+v)", i, order[i], at, order)
		}
	}
}

// racingWaterWorld is a resident chunk whose next guarded write loses: `race` runs
// once, between the simulation's read and its compare, the way a player edit applies
// through the cache without the simulation lock.
type racingWaterWorld struct {
	chunk *world.Chunk
	race  func()
}

func (w *racingWaterWorld) Peek(coord world.Coord) (*world.Chunk, error) {
	if coord != w.chunk.Coord {
		return nil, world.ErrNotResident
	}
	return w.chunk, nil
}

func (w *racingWaterWorld) ApplyResidentGuarded(x, y, z int64, block world.Block, allow func(world.Block) error) error {
	if w.race != nil {
		race := w.race
		w.race = nil
		race()
	}
	if err := allow(w.chunk.At(world.Local(x), world.Local(y), world.Local(z))); err != nil {
		return err
	}
	w.chunk.Set(world.Local(x), world.Local(y), world.Local(z), block)
	return nil
}

// A write that loses its guarded compare is decided again from the world as it now
// is — it does not stand, and it does not strand.
//
// **The empty errWaterReadChanged arm was a permanent freeze (#717).** The voxel's
// heap entry was already popped when the compare failed, and a map entry with no heap
// row behind it blocks every future push — [Sim.scheduleWaterLocked] only pushes a due
// that beats the map, and later dues never do. Not even a composition scan could
// revive the voxel, so whatever water stood there stopped answering the automaton for
// the rest of the process: frozen mid-drain sheets and columns, on screen, for ever.
func TestALostGuardedWriteIsDecidedAgainFromTheNewWorld(t *testing.T) {
	t.Parallel()

	cache := world.NewCache(73, 1, 1)
	sim, err := NewSim(DefaultTickRate, 1, testWorldSeed, NewCacheTerrain(cache), cache, testEntityIDs(), slog.New(slog.DiscardHandler))
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}
	water := &racingWaterWorld{chunk: world.NewChunk(world.Coord{})}
	if err := sim.ConfigureWater(water); err != nil {
		t.Fatalf("ConfigureWater: %v", err)
	}

	// A source over air: the pass will decide the air below it into a fall, and the
	// racer will have placed Stone there first.
	source := waterVoxel{x: 16, y: 20, z: 16}
	target := waterVoxel{x: 16, y: 19, z: 16}
	water.chunk.Set(int(source.x), int(source.y), int(source.z), world.Water)
	water.race = func() {
		water.chunk.Set(int(target.x), int(target.y), int(target.z), world.Stone)
	}
	scheduleWaterNow(sim, target)

	if changes := sim.Step(1); len(changes) != 0 {
		t.Fatalf("the lost write still reported %+v, want no changes", changes)
	}
	if got := water.chunk.At(int(target.x), int(target.y), int(target.z)); got != world.Stone {
		t.Fatalf("the racer's block is %d, want the Stone it placed to stand", got)
	}
	sim.mu.Lock()
	due, pending := sim.pendingWater[target]
	sim.mu.Unlock()
	if !pending {
		t.Fatal("the voxel left the schedule after the lost write; that strand is the #717 freeze")
	}
	if due != 1+WaterTickDelay {
		t.Fatalf("the voxel is due at tick %d, want re-decided at %d", due, 1+WaterTickDelay)
	}

	// Re-examined, the voxel is decided from the racer's world — Stone is not water's
	// to change — and the schedule empties instead of holding a dead entry.
	for tick := uint64(2); tick <= 2+2*WaterTickDelay; tick++ {
		sim.Step(tick)
	}
	if got := water.chunk.At(int(target.x), int(target.y), int(target.z)); got != world.Stone {
		t.Fatalf("re-deciding the voxel produced %d, want the Stone left alone", got)
	}
	sim.mu.Lock()
	_, pending = sim.pendingWater[target]
	sim.mu.Unlock()
	if pending {
		t.Fatal("the re-decided voxel is still pending; the retry must settle")
	}
}
