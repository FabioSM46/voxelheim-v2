package world

import (
	"context"
	"errors"
	"slices"
	"sync"
	"testing"
)

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
