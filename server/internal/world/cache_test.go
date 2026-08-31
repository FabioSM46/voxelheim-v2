package world

import (
	"context"
	"errors"
	"slices"
	"sync"
	"testing"
)

func TestApplyResidentGuardedNeverGeneratesAndDistinguishesTheGuard(t *testing.T) {
	t.Parallel()

	cache := NewCache(11, 1, 4)
	guardMismatch := errors.New("the resident value changed")

	if err := cache.ApplyResidentGuarded(1, 2, 3, Stone, nil); !errors.Is(err, ErrNotResident) {
		t.Fatalf("absent resident write error = %v, want ErrNotResident", err)
	}
	if got := cache.Len(); got != 0 {
		t.Fatalf("resident-only write composed %d chunks, want none", got)
	}

	coord := ChunkOf(1, 2, 3)
	chunk, _, err := cache.Get(context.Background(), coord)
	if err != nil {
		t.Fatalf("Get resident chunk: %v", err)
	}
	want := chunk.At(Local(1), Local(2), Local(3))
	revision := cache.Revision()
	if err := cache.ApplyResidentGuarded(1, 2, 3, Stone, func(Block) error { return guardMismatch }); !errors.Is(err, guardMismatch) {
		t.Fatalf("guarded resident write error = %v, want guard mismatch", err)
	}
	if cache.Revision() != revision {
		t.Fatal("a refused resident write advanced the cache revision")
	}
	got, err := cache.BlockAt(context.Background(), 1, 2, 3)
	if err != nil || got != want {
		t.Fatalf("refused resident write left block %d (err %v), want %d", got, err, want)
	}
}

func TestWaterCompositionDoorbellDeduplicatesWithoutLosingAFullWake(t *testing.T) {
	t.Parallel()

	cache := NewCache(11, 1, 4)
	coords := []Coord{{X: 2}, {X: -1}, {X: 2}}
	for _, coord := range coords {
		if _, _, err := cache.Get(context.Background(), coord); err != nil {
			t.Fatalf("Get(%+v): %v", coord, err)
		}
		cache.markWaterComposition(coord)
	}

	if got := len(cache.waterWake); got != 1 {
		t.Fatalf("doorbell depth = %d, want its hard bound of 1", got)
	}
	<-cache.WaterCompositions()
	chunks := cache.TakeWaterCompositions()
	if len(chunks) != 2 || chunks[0].Coord != (Coord{X: -1}) || chunks[1].Coord != (Coord{X: 2}) {
		t.Fatalf("deduplicated compositions = %+v, want chunks -1 then 2", chunkCoords(chunks))
	}
	if remaining := cache.TakeWaterCompositions(); len(remaining) != 0 {
		t.Fatalf("second take returned %+v, want nothing", chunkCoords(remaining))
	}
}

func TestResidentWaterPersistsAndRegenerateRestoresAndRearms(t *testing.T) {
	t.Parallel()

	const seed = int64(11)
	dir := t.TempDir()
	cache := NewPersistentCache(testStore(t, dir, seed), 1, 4)
	coord := Coord{}
	chunk, _, err := cache.Get(context.Background(), coord)
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	index := Index(1, 2, 3)
	original := chunk.Blocks[index]
	<-cache.WaterCompositions()
	cache.TakeWaterCompositions()
	if err := cache.ApplyResidentGuarded(1, 2, 3, WaterFlow4, nil); err != nil {
		t.Fatalf("ApplyResidentGuarded: %v", err)
	}
	if err := cache.Flush(); err != nil {
		t.Fatalf("Flush: %v", err)
	}

	cache = NewPersistentCache(testStore(t, dir, seed), 1, 4)
	chunk, _, err = cache.Get(context.Background(), coord)
	if err != nil {
		t.Fatalf("reload Get: %v", err)
	}
	if got := chunk.Blocks[index]; got != WaterFlow4 {
		t.Fatalf("reloaded water = %d, want WaterFlow4", got)
	}
	<-cache.WaterCompositions()
	cache.TakeWaterCompositions()

	if err := cache.Regenerate(coord); err != nil {
		t.Fatalf("Regenerate: %v", err)
	}
	select {
	case <-cache.WaterCompositions():
	default:
		t.Fatal("Regenerate did not ring the water composition doorbell")
	}
	chunks := cache.TakeWaterCompositions()
	if len(chunks) != 1 || chunks[0].Coord != coord {
		t.Fatalf("regeneration compositions = %+v, want %+v", chunkCoords(chunks), coord)
	}
	if got := chunks[0].Blocks[index]; got != original {
		t.Fatalf("regenerated block = %d, want generated %d", got, original)
	}
}

func chunkCoords(chunks []*Chunk) []Coord {
	coords := make([]Coord, len(chunks))
	for i, chunk := range chunks {
		coords[i] = chunk.Coord
	}
	return coords
}

// The cache exists so a chunk two players can both see is generated once. With
// eight goroutines asking at the same moment, one generation must satisfy all of
// them — which is why an entry is published before the chunk exists, with a
// channel that says when it does.
func TestCacheGeneratesOnceForConcurrentRequests(t *testing.T) {
	t.Parallel()

	cache := NewCache(11, 2, 16)
	coord := Coord{X: 1, Y: 2, Z: 3}

	const goroutines = 8
	chunks := make([]*Chunk, goroutines)

	var wg sync.WaitGroup
	for i := range goroutines {
		wg.Add(1)
		go func() {
			defer wg.Done()
			chunk, _, err := cache.Get(context.Background(), coord)
			if err != nil {
				t.Errorf("Get: %v", err)
				return
			}
			chunks[i] = chunk
		}()
	}
	wg.Wait()

	for i := 1; i < goroutines; i++ {
		if chunks[i] != chunks[0] {
			t.Fatalf("goroutine %d got a different chunk pointer: the cache generated more than once", i)
		}
	}
	if got := cache.Len(); got != 1 {
		t.Errorf("cache holds %d chunks, want 1", got)
	}
}

func TestCacheReturnsTheEncodedPayloadWithTheChunk(t *testing.T) {
	t.Parallel()

	cache := NewCache(11, 1, 4)
	chunk, runs, err := cache.Get(context.Background(), Coord{X: 0, Y: 2, Z: 0})
	if err != nil {
		t.Fatalf("Get: %v", err)
	}

	blocks, err := Decode(runs)
	if err != nil {
		t.Fatalf("the cached payload does not decode: %v", err)
	}
	if !slices.Equal(blocks, chunk.Blocks) {
		t.Error("the cached payload does not describe the cached chunk")
	}
}

// Residency is bounded, because a server running for a week must not accumulate
// every chunk anyone has ever walked past.
func TestCacheEvictsLeastRecentlyUsed(t *testing.T) {
	t.Parallel()

	cache := NewCache(11, 1, 2)
	first := Coord{X: 0, Y: 0, Z: 0}
	second := Coord{X: 1, Y: 0, Z: 0}
	third := Coord{X: 2, Y: 0, Z: 0}

	for _, coord := range []Coord{first, second} {
		if _, _, err := cache.Get(context.Background(), coord); err != nil {
			t.Fatalf("Get(%+v): %v", coord, err)
		}
	}
	// Touch the first so the second becomes the least recently used.
	if _, err := cache.Peek(first); err != nil {
		t.Fatalf("Peek(first): %v", err)
	}
	if _, _, err := cache.Get(context.Background(), third); err != nil {
		t.Fatalf("Get(third): %v", err)
	}

	if got := cache.Len(); got != 2 {
		t.Fatalf("cache holds %d chunks, want the capacity of 2", got)
	}
	if _, err := cache.Peek(second); !errors.Is(err, ErrNotResident) {
		t.Errorf("the least recently used chunk is still resident (err = %v)", err)
	}
	if _, err := cache.Peek(first); err != nil {
		t.Errorf("the recently touched chunk was evicted: %v", err)
	}
}

// Eviction is only safe because generation is deterministic: a chunk thrown away
// must come back identical, or the world changes under a player who walked away
// and returned.
func TestEvictedChunksRegenerateIdentically(t *testing.T) {
	t.Parallel()

	cache := NewCache(11, 1, 1)
	coord := Coord{X: 4, Y: 2, Z: -4}

	_, first, err := cache.Get(context.Background(), coord)
	if err != nil {
		t.Fatalf("Get: %v", err)
	}
	firstCopy := slices.Clone(first)

	// Push it out.
	if _, _, err := cache.Get(context.Background(), Coord{X: 99, Y: 0, Z: 0}); err != nil {
		t.Fatalf("Get(other): %v", err)
	}
	if _, err := cache.Peek(coord); !errors.Is(err, ErrNotResident) {
		t.Fatalf("the chunk was not evicted (err = %v)", err)
	}

	_, second, err := cache.Get(context.Background(), coord)
	if err != nil {
		t.Fatalf("Get after eviction: %v", err)
	}
	if !slices.Equal(firstCopy, second) {
		t.Error("the regenerated chunk differs from the evicted one")
	}
}

// Peek is what the tick loop uses: it must never generate, because a tick that
// waits on a chunk is a tick every connected player misses.
func TestPeekNeverGenerates(t *testing.T) {
	t.Parallel()

	cache := NewCache(11, 1, 4)
	coord := Coord{X: 7, Y: 0, Z: 7}

	if _, err := cache.Peek(coord); !errors.Is(err, ErrNotResident) {
		t.Fatalf("Peek error = %v, want ErrNotResident", err)
	}
	if got := cache.Len(); got != 0 {
		t.Fatalf("Peek added %d entries to the cache", got)
	}

	if _, _, err := cache.Get(context.Background(), coord); err != nil {
		t.Fatalf("Get: %v", err)
	}
	if _, err := cache.Peek(coord); err != nil {
		t.Errorf("Peek on a resident chunk failed: %v", err)
	}
}

func TestGetHonoursContextCancellation(t *testing.T) {
	t.Parallel()

	cache := NewCache(11, 1, 4)
	ctx, cancel := context.WithCancel(context.Background())
	cancel()

	if _, _, err := cache.Get(ctx, Coord{X: 1, Y: 1, Z: 1}); err == nil {
		t.Fatal("Get ignored a cancelled context")
	}
	// The failed entry must not be left behind, or every later request replays the
	// failure instead of generating the chunk.
	if got := cache.Len(); got != 0 {
		t.Errorf("a cancelled Get left %d entries in the cache", got)
	}

	if _, _, err := cache.Get(context.Background(), Coord{X: 1, Y: 1, Z: 1}); err != nil {
		t.Errorf("the retry after cancellation failed: %v", err)
	}
}

func TestNewCacheFallsBackToDefaults(t *testing.T) {
	t.Parallel()

	cache := NewCache(1, 0, -5)
	if cache.capacity != DefaultCacheCapacity {
		t.Errorf("capacity = %d, want the default %d", cache.capacity, DefaultCacheCapacity)
	}
	if cap(cache.slots) != DefaultWorkers {
		t.Errorf("workers = %d, want the default %d", cap(cache.slots), DefaultWorkers)
	}
	if cache.Seed() != 1 {
		t.Errorf("Seed() = %d, want 1", cache.Seed())
	}
}

// The residency has to hold the view volume, and before #666 nothing said so.
//
// At the default view distance of 3 it did by luck — 343 chunks against 1024 — and the
// flag was free to move without it. At 8 the volume is 4913 against the same 1024, and
// the server evicts what it is about to need: measured, 1576 ticks over 10 ms in 104
// seconds with the client's meshed-chunk count frozen.
func TestTheResidencyHoldsTheViewVolume(t *testing.T) {
	t.Parallel()

	for distance := 0; distance <= LargestViewDistanceHeld(); distance++ {
		volume := ChunksInView(distance)
		capacity := CacheCapacityFor(distance)
		if capacity < volume {
			t.Errorf("view distance %d covers %d chunks and is given a residency of %d",
				distance, volume, capacity)
		}
		if capacity < DefaultCacheCapacity {
			t.Errorf("view distance %d is given %d, under the floor of %d",
				distance, capacity, DefaultCacheCapacity)
		}
	}
}

// The default is unchanged, which is what keeps this a guard rather than a retune.
func TestTheDefaultViewDistanceStillGetsTheDefaultResidency(t *testing.T) {
	t.Parallel()

	if got := CacheCapacityFor(3); got != DefaultCacheCapacity {
		t.Errorf("view distance 3 is given %d, want the unchanged %d", got, DefaultCacheCapacity)
	}
	if got := ChunksInView(3); got != 343 {
		t.Errorf("view distance 3 covers %d chunks, want 343", got)
	}
}

// The ceiling is a real edge, not decoration: the protocol allows a view distance this
// server cannot hold, and the refusal has to name the largest one it can.
func TestTheCeilingRefusesAViewDistanceThatCannotBeHeld(t *testing.T) {
	t.Parallel()

	largest := LargestViewDistanceHeld()
	if CacheCapacityFor(largest) > MaxCacheCapacity {
		t.Fatalf("the largest held distance %d needs %d, over the ceiling of %d",
			largest, CacheCapacityFor(largest), MaxCacheCapacity)
	}
	if CacheCapacityFor(largest+1) <= MaxCacheCapacity {
		t.Fatalf("distance %d also fits, so %d is not the largest", largest+1, largest)
	}
	// The protocol's own ceiling is past what any residency here holds, which is the
	// whole reason a refusal exists rather than a clamp.
	if ChunksInView(16) <= MaxCacheCapacity {
		t.Errorf("the protocol ceiling of 16 covers %d chunks, which now fits; the refusal is dead code",
			ChunksInView(16))
	}
}
