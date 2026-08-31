package game

import (
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

	targets := make([]waterVoxel, 0, WaterChangesPerTick)
	for x := int64(0); x < 32 && len(targets) < WaterChangesPerTick; x++ {
		for z := int64(0); z < 32 && len(targets) < WaterChangesPerTick; z++ {
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

// The other half of the residency guard: a voxel skipped because the chunk under it
// could not be read must come back when that chunk can be.
//
// The guard drops the voxel rather than retrying it, and that is only correct if
// something schedules it again. This walks the whole round trip rather than trusting
// the argument: skip, compose the chunk below, drain the composition scan the way
// `waterScanLoop` does, and require the voxel to be decided from the real blocks.
func TestAVoxelSkippedForResidencyIsDecidedOnceItsGroundArrives(t *testing.T) {
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
	_, stillPending := sim.pendingWater[at]
	sim.mu.Unlock()
	if stillPending {
		t.Fatal("the skipped voxel stayed on the schedule; it is meant to be dropped and re-scheduled by the scan")
	}

	// The chunk below arrives. This is what the cache does on composition and what
	// `waterScanLoop` turns into a scan.
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
	// And the voxel's own chunk is scanned too, which is what actually reaches it: the
	// source beside it is water on this chunk's boundary, so the scan schedules its
	// neighbourhood.
	if err := sim.QueueUnstableWater(context.Background(), world.Coord{},
		[]int{world.Index(world.Local(at.x+1), world.Local(at.y), world.Local(at.z))}); err != nil {
		t.Fatalf("QueueUnstableWater: %v", err)
	}

	decided := false
	for tick := uint64(2); tick <= 2+4*WaterTickDelay; tick++ {
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
// under it for want of one existing. The residency guard therefore cannot delete a
// voxel forever — a point worth pinning rather than reasoning about, because the guard
// is the one path that drops work.
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
