package world

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"reflect"
	"sync"
	"sync/atomic"
	"testing"
)

func TestInstanceGoldenChunks(t *testing.T) {
	for seed := int64(0); seed < 4; seed++ {
		for x := int32(-1); x <= 0; x++ {
			for z := int32(-1); z <= 0; z++ {
				coord := Coord{X: x, Z: z}
				path := fmt.Sprintf("testdata/instance_%d_%d_%d.bin", seed, x, z)
				got := encodedBytes(Encode(GenerateInstance(seed, coord)))
				if *updateGolden {
					if err := os.WriteFile(path, got, 0o644); err != nil {
						t.Fatal(err)
					}
				}
				want, err := os.ReadFile(path)
				if err != nil {
					t.Fatal(err)
				}
				if !bytes.Equal(got, want) {
					t.Errorf("seed %d chunk %+v differs from chamber fixture", seed, coord)
				}
			}
		}
	}
}

func TestInstanceIsClosedAndAnchorsAreReachable(t *testing.T) {
	for _, seed := range []int64{0, 1, 2, 3, -1, math.MinInt64, math.MaxInt64} {
		cache := NewInstanceCache(seed, 1, 48)
		blockAt := func(x, y, z int64) Block {
			chunk, _, err := cache.Get(context.Background(), ChunkOf(x, y, z))
			if err != nil {
				t.Fatal(err)
			}
			return chunk.At(Local(x), Local(y), Local(z))
		}
		arrival, exit := InstanceAnchors(seed)
		if arrival.Kind != AnchorInstanceArrival || exit.Kind != AnchorInstanceExit || arrival == exit {
			t.Fatalf("bad anchors: %+v %+v", arrival, exit)
		}
		if arrival.Y != 1 || exit.Y != 1 {
			t.Fatal("anchors must stand directly on the chamber floor")
		}
		// Flood the two-voxel-tall body's standing cells. Reaching an external
		// cell would also detect a hole that an anchor-only check cannot see.
		type point struct{ x, z int64 }
		start, goal := point{arrival.X, arrival.Z}, point{exit.X, exit.Z}
		queue := []point{start}
		seen := map[point]bool{start: true}
		walkable := func(p point) bool {
			return Solid(blockAt(p.x, 0, p.z)) && !Solid(blockAt(p.x, 1, p.z)) && !Solid(blockAt(p.x, 2, p.z))
		}
		if !walkable(start) || !walkable(goal) {
			t.Fatal("an anchor lacks headroom or a floor")
		}
		for len(queue) > 0 {
			p := queue[0]
			queue = queue[1:]
			if p.x <= -34 || p.x >= 34 || p.z <= -34 || p.z >= 34 {
				t.Fatal("body escaped the chamber")
			}
			for _, d := range []point{{1, 0}, {-1, 0}, {0, 1}, {0, -1}} {
				next := point{p.x + d.x, p.z + d.z}
				if !seen[next] && walkable(next) {
					seen[next] = true
					queue = append(queue, next)
				}
			}
		}
		if !seen[goal] {
			t.Fatalf("seed %d exit unreachable", seed)
		}
	}
}

func TestInstanceVoidAndFiniteCache(t *testing.T) {
	cache := NewInstanceCache(7, 1, 2)
	for _, coord := range []Coord{{Y: -1}, {X: 3}, {X: -3}, {Y: 1}, {Z: 3}, {Z: -3}, {Y: -100}, {X: math.MinInt32}, {Z: math.MaxInt32}} {
		chunk := GenerateInstance(7, coord)
		if chunk.Coord != coord || len(chunk.Blocks) != ChunkVolume {
			t.Fatalf("invalid void chunk at %+v", coord)
		}
		for _, block := range chunk.Blocks {
			if block != Air {
				t.Fatalf("terrain beyond shell at %+v", coord)
			}
		}
	}
	// Include partial shell chunks: every voxel outside the drawing is void.
	for x := int32(-1); x <= 0; x++ {
		for z := int32(-1); z <= 0; z++ {
			chunk := GenerateInstance(7, Coord{X: x, Z: z})
			ox, oy, oz := chunk.Coord.Origin()
			for y := range ChunkSize {
				for lz := range ChunkSize {
					for lx := range ChunkSize {
						wx, wy, wz := ox+int64(lx), oy+int64(y), oz+int64(lz)
						if (wx < -34 || wx > 34 || wy > 9 || wz < -16 || wz > 16) && chunk.At(lx, y, lz) != Air {
							t.Fatal("partial chunk grows terrain outside shell")
						}
					}
				}
			}
		}
	}
	accepted := 0
	for x := int32(-5); x <= 5; x++ {
		for y := int32(-5); y <= 5; y++ {
			for z := int32(-5); z <= 5; z++ {
				coord := Coord{X: x, Y: y, Z: z}
				chunk, encoded, err := cache.Get(context.Background(), coord)
				if cache.Contains(coord) {
					accepted++
					if err != nil || chunk == nil || len(encoded) == 0 {
						t.Fatalf("accepted chunk failed: %v", err)
					}
				} else if !errors.Is(err, ErrOutsideWorld) || chunk != nil || encoded != nil {
					t.Fatalf("out-of-world request was not refused: %+v %v", coord, err)
				}
			}
		}
	}
	if accepted != 72 || cache.Len() > 2 {
		t.Fatalf("finite envelope/residency changed: %d accepted, %d resident", accepted, cache.Len())
	}
	outside := Coord{Y: -1000}
	before := cache.Len()
	if err := cache.Regenerate(outside); !errors.Is(err, ErrOutsideWorld) {
		t.Fatalf("regeneration beyond envelope: %v", err)
	}
	if err := cache.Apply(context.Background(), 0, -32000, 0, Stone, nil); !errors.Is(err, ErrOutsideWorld) {
		t.Fatalf("edit beyond envelope: %v", err)
	}
	if cache.Len() != before {
		t.Fatal("refused coordinates reserved cache entries")
	}
}

func TestInstanceShellCannotBeEditedAndInteriorIsEphemeral(t *testing.T) {
	dir := t.TempDir()
	t.Chdir(dir)
	cache := NewInstanceCache(2, 1, 48)
	ctx := context.Background()
	for _, p := range [][3]int64{{0, 0, 0}, {0, 6, 0}, {-16, 2, 0}, {16, 2, 0}, {0, 2, -34}, {0, 2, 34}, {8, 2, 0}} {
		if err := cache.Apply(ctx, p[0], p[1], p[2], Air, nil); !errors.Is(err, ErrImmutableShell) {
			t.Fatalf("shell edit accepted at %v: %v", p, err)
		}
		if err := cache.ApplyResidentGuarded(p[0], p[1], p[2], Air, nil); !errors.Is(err, ErrImmutableShell) {
			t.Fatalf("resident shell edit accepted at %v: %v", p, err)
		}
	}
	if err := cache.Apply(ctx, 0, 1, 0, Planks, nil); err != nil {
		t.Fatal(err)
	}
	coord := ChunkOf(0, 1, 0)
	chunk, _, err := cache.Get(ctx, coord)
	if err != nil || chunk.At(0, 1, 0) != Planks {
		t.Fatal("interior edit missing")
	}
	if cache.store != nil {
		t.Fatal("instance acquired persistent store")
	}
	if err := cache.Flush(); err != nil {
		t.Fatal(err)
	}
	entries, err := os.ReadDir(dir)
	if err != nil || len(entries) != 0 {
		t.Fatalf("instance wrote files: count=%d err=%v", len(entries), err)
	}
	if err := cache.Regenerate(coord); err != nil {
		t.Fatal(err)
	}
	chunk, _, err = cache.Get(ctx, coord)
	if err != nil || !reflect.DeepEqual(chunk, GenerateInstance(2, coord)) {
		t.Fatal("regeneration did not restore chamber base")
	}
	fresh := NewInstanceCache(2, 1, 48)
	chunk, _, err = fresh.Get(ctx, coord)
	if err != nil || chunk.At(0, 1, 0) != Air {
		t.Fatal("a new instance retained old edits")
	}
}

func TestCacheDefaultGeneratorPreservesOpenWorldGoldens(t *testing.T) {
	for _, option := range [][]CacheOption{nil, {WithGenerator(nil)}} {
		cache := NewCache(goldenSeed, 1, 8, option...)
		for _, fixture := range []struct {
			coord Coord
			path  string
		}{
			{goldenCoord, goldenPath}, {goldenWaterCoord, goldenWaterPath},
			{goldenSettlementCoord, goldenSettlementPath}, {goldenPlainsCoord, goldenPlainsPath},
			{goldenRiverCoord, goldenRiverPath}, {goldenBushCoord, goldenBushPath},
			{Coord{X: 55, Y: 2, Z: 55}, "testdata/chunk_golden_ruin.bin"},
		} {
			_, encoded, err := cache.Get(context.Background(), fixture.coord)
			if err != nil {
				t.Fatal(err)
			}
			want, err := os.ReadFile(filepath.Clean(fixture.path))
			if err != nil || !bytes.Equal(encodedBytes(encoded), want) {
				t.Fatalf("default generation differs from %s: %v", fixture.path, err)
			}
		}
	}
}

func TestInjectedGeneratorCoalescesAndRegenerates(t *testing.T) {
	var calls atomic.Int32
	generator := func(seed int64, coord Coord) *Chunk {
		if seed != 73 {
			t.Errorf("generator seed = %d", seed)
		}
		calls.Add(1)
		chunk := NewChunk(coord)
		chunk.Set(1, 2, 3, DarkGlass)
		return chunk
	}
	cache := NewCache(73, 2, 1, WithGenerator(generator))
	ctx := context.Background()
	coord := Coord{X: -4, Y: 2, Z: 9}
	var wg sync.WaitGroup
	for range 16 {
		wg.Add(1)
		go func() {
			defer wg.Done()
			chunk, _, err := cache.Get(ctx, coord)
			if err != nil || chunk.At(1, 2, 3) != DarkGlass || chunk.Coord != coord {
				t.Errorf("injected generation failed: %v", err)
			}
		}()
	}
	wg.Wait()
	if calls.Load() != 1 {
		t.Fatalf("concurrent requests generated %d times", calls.Load())
	}
	ox, oy, oz := coord.Origin()
	if err := cache.Apply(ctx, ox+1, oy+2, oz+3, Planks, nil); err != nil {
		t.Fatal(err)
	}
	if err := cache.Regenerate(coord); err != nil {
		t.Fatal(err)
	}
	chunk, _, err := cache.Get(ctx, coord)
	if err != nil || chunk.At(1, 2, 3) != DarkGlass || calls.Load() != 2 {
		t.Fatal("regeneration bypassed injected generator")
	}
	if _, _, err := cache.Get(ctx, Coord{}); err != nil {
		t.Fatal(err)
	}
	chunk, _, err = cache.Get(ctx, coord)
	if err != nil || chunk.At(1, 2, 3) != DarkGlass || calls.Load() != 4 {
		t.Fatal("eviction bypassed injected generator")
	}
}

func TestInstanceGenerationIsIndependentAcrossServers(t *testing.T) {
	for _, seed := range []int64{0, 1, 2, 3, -1, math.MinInt64} {
		a, b := NewInstanceCache(seed, 1, 1), NewInstanceCache(seed, 4, 8)
		for _, coord := range []Coord{{X: -1, Z: -1}, {X: -1}, {Z: -1}, {}, {Y: -1}} {
			ca, ea, erra := a.Get(context.Background(), coord)
			cb, eb, errb := b.Get(context.Background(), coord)
			if erra != nil || errb != nil || !reflect.DeepEqual(ca, cb) || !reflect.DeepEqual(ea, eb) {
				t.Fatalf("independent worlds differ: seed %d coord %+v", seed, coord)
			}
		}
		arrival, exit := InstanceAnchors(seed)
		arrival.X = 999
		next, nextExit := InstanceAnchors(seed)
		if next.X == arrival.X || nextExit != exit {
			t.Fatal("anchor caller mutated shared generator state")
		}
	}
}
