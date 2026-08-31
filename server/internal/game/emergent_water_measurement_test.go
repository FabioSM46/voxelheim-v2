package game

import (
	"context"
	"flag"
	"io/fs"
	"log/slog"
	"os"
	"path/filepath"
	"slices"
	"testing"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

var measureEmergentWater = flag.Bool(
	"measure-emergent-water",
	false,
	"run the eight-chunk emergent-water persistence and tick-cost measurement",
)

// TestMeasureEmergentWater is a measurement, not a CI assertion. Run it explicitly:
//
//	GOMAXPROCS=1 go test ./internal/game -run '^TestMeasureEmergentWater$' \
//	  -measure-emergent-water -count=5 -v
//
// The fixture is eight generated chunks at seed 1: a 2x2x2 volume covering x
// -64..-1, y 32..95 and z 1120..1183. It intersects the river volume used while
// repairing generated water in #654, and uses the real cache, delta layer, store and
// capped authoritative water pass. The test runs until the schedule is empty, flushes
// the resulting deltas, then runs another complete settling window to distinguish a
// fixed point from a pause between due ticks.
//
// # Recorded result and decision
//
// Recorded on 2026-08-31, over five back-to-back single-P runs so scheduler competition
// is not attributed to the water pass. The generated compositions reported 2,658
// unstable voxels. They settled in 155 ticks and made 3,714 writes to 1,849 unique delta
// positions. In measurementWaterChunks order the per-chunk counts were 0, 0, 0, 0, 250,
// 1,130, 0 and 469: 231.1 deltas per loaded chunk. The three non-empty chunk files
// occupied 11,178 bytes on disk, including their real headers and checksums. Another 155
// ticks made zero writes, grew the delta count by zero and grew the files by zero bytes:
// this world reaches a fixed point rather than recording time.
//
// A walked region is stated here as 1,024 x 1,024 blocks through the same two vertical
// chunk bands. Tiling this deliberately water-heavy 64 x 64 probe 16 x 16 times gives
// 2,048 chunks, 473,344 deltas and 2,861,568 bytes (2.73 MiB). That is an extrapolation
// of the measured density, not a claim that every square kilometre is all river.
//
// Storage is affordable; authoritative tick time is not. Five runs measured p50
// 0.68..0.75 ms, p90 5.97..6.96 ms and max 7.74..13.97 ms while settling. #665's capped
// walk recorded p90 0.11..4.0 ms, a typical max of 1.4..7 ms and one 10.8 ms outlier.
// This load therefore keeps its p90 above the worst old p90 and exceeds the old maximum
// in two of five runs. The emergent-river model is a **no-go** on that tick number,
// regardless of its modest 2.73 MiB/km² storage cost. Reconsider it only after the same
// probe stays within #665's envelope (p90 at most 4 ms and max at most 10.8 ms) without
// weakening WaterScansPerTick or WaterVoxelsPerTick.
func TestMeasureEmergentWater(t *testing.T) {
	if !*measureEmergentWater {
		t.Skip("pass -measure-emergent-water to run the recorded measurement")
	}

	const seed int64 = 1
	store, err := world.OpenStore(t.TempDir(), seed)
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	cache := world.NewPersistentCache(store, 1, 16)
	sim, err := NewSim(DefaultTickRate, 1, seed, NewCacheTerrain(cache), cache, testEntityIDs(), slog.New(slog.DiscardHandler))
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}
	if err := sim.ConfigureWater(cache); err != nil {
		t.Fatalf("ConfigureWater: %v", err)
	}

	coords := measurementWaterChunks()
	for _, coord := range coords {
		if _, _, err := cache.Get(context.Background(), coord); err != nil {
			t.Fatalf("Get %+v: %v", coord, err)
		}
	}
	composed := cache.TakeWaterCompositions()
	if len(composed) != len(coords) {
		t.Fatalf("water compositions = %d, want one for each of %d chunks", len(composed), len(coords))
	}
	unstable := 0
	for _, chunk := range composed {
		indices := world.UnstableWater(chunk)
		unstable += len(indices)
		if err := sim.QueueUnstableWater(context.Background(), chunk.Coord, indices); err != nil {
			t.Fatalf("QueueUnstableWater %+v: %v", chunk.Coord, err)
		}
	}

	settling := runWaterToFixedPoint(t, sim, 0)
	if err := cache.Flush(); err != nil {
		t.Fatalf("Flush: %v", err)
	}
	counts, total := storedDeltaCounts(t, store, coords)
	bytes := storedDeltaBytes(t, store.Dir())

	steady := runWaterWindow(t, sim, settling.lastTick+1, settling.ticks)
	if err := cache.Flush(); err != nil {
		t.Fatalf("steady-state Flush: %v", err)
	}
	_, steadyTotal := storedDeltaCounts(t, store, coords)
	steadyBytes := storedDeltaBytes(t, store.Dir())
	if steady.changes != 0 || steadyTotal != total || steadyBytes != bytes {
		t.Fatalf("steady state changed: writes=%d delta_growth=%d byte_growth=%d",
			steady.changes, steadyTotal-total, steadyBytes-bytes)
	}

	t.Logf("fixture: seed=%d chunks=%d unstable_voxels=%d", seed, len(coords), unstable)
	t.Logf("settling: ticks=%d changes=%d unique_deltas=%d deltas_per_chunk=%v disk_bytes=%d", settling.ticks, settling.changes, total, counts, bytes)
	t.Logf("settling tick ns: p50=%d p90=%d max=%d", percentile(settling.durations, 50), percentile(settling.durations, 90), slices.Max(settling.durations))
	t.Logf("steady state: ticks=%d changes=%d delta_growth=%d byte_growth=%d", steady.ticks, steady.changes, steadyTotal-total, steadyBytes-bytes)
}

func measurementWaterChunks() []world.Coord {
	coords := make([]world.Coord, 0, 8)
	for y := int32(1); y <= 2; y++ {
		for z := int32(35); z <= 36; z++ {
			for x := int32(-2); x <= -1; x++ {
				coords = append(coords, world.Coord{X: x, Y: y, Z: z})
			}
		}
	}
	return coords
}

type waterMeasurement struct {
	lastTick  uint64
	ticks     int
	changes   int
	durations []int64
}

// runWaterToFixedPoint steps until a full retry window passes with no writes and no
// scan work outstanding.
//
// **An empty schedule stopped being what a fixed point looks like with #717.** A
// voxel whose neighbourhood crosses the fixture's residency edge now stays scheduled
// by design, retried every [WaterResidencyRetryDelay] ticks and writing nothing,
// because in a live world the missing chunk can arrive; in this eight-chunk fixture
// it never does. Quiet is therefore a window: long enough that every deferred voxel
// has been retried at least once and every settling write would have come due, so a
// world silent through it has nothing left to say. The numbers recorded above
// predate this stop rule; a re-recorded settling count includes the quiet window.
func runWaterToFixedPoint(t testing.TB, sim *Sim, start uint64) waterMeasurement {
	t.Helper()
	const maxTicks = 10_000
	quietWindow := int(WaterResidencyRetryDelay + WaterTickDelay + 1)
	quiet := 0
	measurement := waterMeasurement{lastTick: start, durations: make([]int64, 0, 256)}
	for range maxTicks {
		measurement.lastTick++
		started := time.Now()
		changes := sim.Step(measurement.lastTick)
		measurement.durations = append(measurement.durations, time.Since(started).Nanoseconds())
		measurement.ticks++
		measurement.changes += len(changes)
		if len(changes) == 0 {
			quiet++
		} else {
			quiet = 0
		}
		if quiet >= quietWindow && waterScansDrained(sim) {
			return measurement
		}
	}
	t.Fatalf("water did not reach a fixed point in %d ticks", maxTicks)
	return waterMeasurement{}
}

func runWaterWindow(t testing.TB, sim *Sim, start uint64, ticks int) waterMeasurement {
	t.Helper()
	measurement := waterMeasurement{lastTick: start - 1, durations: make([]int64, 0, ticks)}
	for range ticks {
		measurement.lastTick++
		started := time.Now()
		changes := sim.Step(measurement.lastTick)
		measurement.durations = append(measurement.durations, time.Since(started).Nanoseconds())
		measurement.ticks++
		measurement.changes += len(changes)
	}
	return measurement
}

func waterScansDrained(sim *Sim) bool {
	sim.mu.Lock()
	defer sim.mu.Unlock()
	return len(sim.waterScanCarry.indices) == 0 && len(sim.unstableWater) == 0
}

func storedDeltaCounts(t testing.TB, store *world.Store, measured []world.Coord) ([]int, int) {
	t.Helper()
	counts := make([]int, len(measured))
	total := 0
	for i, coord := range measured {
		edits, err := store.LoadChunk(coord)
		if err != nil {
			t.Fatalf("LoadChunk %+v: %v", coord, err)
		}
		counts[i] = len(edits)
		total += len(edits)
	}
	return counts, total
}

func storedDeltaBytes(t testing.TB, dir string) int64 {
	t.Helper()
	var total int64
	err := filepath.WalkDir(dir, func(path string, entry fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if entry.IsDir() || filepath.Ext(path) != ".vxd" {
			return nil
		}
		info, err := os.Stat(path)
		if err != nil {
			return err
		}
		total += info.Size()
		return nil
	})
	if err != nil {
		t.Fatalf("measure stored delta bytes: %v", err)
	}
	return total
}

func percentile(values []int64, pct int) int64 {
	ordered := slices.Clone(values)
	slices.Sort(ordered)
	index := (len(ordered) - 1) * pct / 100
	return ordered[index]
}
