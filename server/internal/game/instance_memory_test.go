package game

import (
	"context"
	"fmt"
	"io"
	"log/slog"
	"os"
	"runtime"
	"strconv"
	"strings"
	"sync/atomic"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// What one live dungeon instance costs the server, measured rather than argued (#1298).
//
// **Opt-in, because it is a measurement and not a check.** It asserts nothing about the
// numbers; it creates instances through the production InstanceManager, makes every chunk
// of each one's envelope resident through its own cache — the most a party can ever make
// it hold — and logs the chunk count and the memory each instance adds, both as live Go
// heap after a collection and as the process's resident set.
//
// It reads only APIs the pre-feature tree already had (Create, the session's cache,
// Contains, Get and Len), so the same file dropped into that tree measures the instance
// the descent replaced; docs/reviews/dungeon-descent-1298.md records both.
//
//	VOXELHEIM_INSTANCE_MEMORY=1 go test ./internal/game -run '^TestLiveInstanceMemory$' -count=1 -v
func TestLiveInstanceMemory(t *testing.T) {
	if os.Getenv("VOXELHEIM_INSTANCE_MEMORY") != "1" {
		t.Skip("set VOXELHEIM_INSTANCE_MEMORY=1 to measure what a live instance costs")
	}
	const instances = 8
	var ids atomic.Uint64
	manager, err := NewInstanceManager(20, 8, instances, func() uint64 { return ids.Add(1) },
		slog.New(slog.NewTextHandler(io.Discard, nil)))
	if err != nil {
		t.Fatal(err)
	}
	defer manager.Close()

	heap := func() uint64 {
		runtime.GC()
		runtime.GC()
		var m runtime.MemStats
		runtime.ReadMemStats(&m)
		return m.HeapAlloc
	}
	baseHeap, baseRSS := heap(), residentSelf()
	var created []InstanceSession
	for i := range instances {
		s, err := manager.Create(InstanceRuin{CellX: int64(i)})
		if err != nil {
			t.Fatal(err)
		}
		created = append(created, s)
	}
	idleHeap, idleRSS := heap(), residentSelf()
	shell, halo := 0, 0
	for _, s := range created {
		held := halo
		for x := int32(-8); x <= 8; x++ {
			for y := int32(-8); y <= 8; y++ {
				for z := int32(-8); z <= 8; z++ {
					coord := world.Coord{X: x, Y: y, Z: z}
					if !s.Chunks.Contains(coord) {
						continue
					}
					chunk, _, err := s.Chunks.Get(context.Background(), coord)
					if err != nil {
						t.Fatal(err)
					}
					halo++
					for _, b := range chunk.Blocks {
						if b != world.Air {
							shell++
							break
						}
					}
				}
			}
		}
		// The ±8 box must enclose the envelope, or every figure below undercounts it: no
		// chunk one step past any face of the box may belong to the instance.
		for x := int32(-9); x <= 9; x++ {
			for y := int32(-9); y <= 9; y++ {
				for z := int32(-9); z <= 9; z++ {
					edge := x == -9 || x == 9 || y == -9 || y == 9 || z == -9 || z == 9
					if coord := (world.Coord{X: x, Y: y, Z: z}); edge && s.Chunks.Contains(coord) {
						t.Fatalf("instance %d holds %+v, outside the ±8 box the measurement loads", s.ID, coord)
					}
				}
			}
		}
		if got := s.Chunks.Len(); got != halo-held {
			t.Logf("instance %d holds %d chunks of an envelope of %d", s.ID, got, halo-held)
		}
	}
	fullHeap, fullRSS := heap(), residentSelf()
	per := func(after, before uint64) float64 { return (float64(after) - float64(before)) / instances / (1 << 20) }
	rss := func(after, before int64) string {
		if after < 0 || before < 0 {
			return "unavailable (no /proc on this platform)"
		}
		return fmt.Sprintf("%.2f MiB", (float64(after)-float64(before))/instances/(1<<20))
	}
	t.Logf("instances=%d envelope_chunks=%.1f chunks_with_blocks=%.1f per instance",
		instances, float64(halo)/instances, float64(shell)/instances)
	t.Logf("created, nothing loaded: heap %.2f MiB and resident %s per instance",
		per(idleHeap, baseHeap), rss(idleRSS, baseRSS))
	t.Logf("whole envelope resident: heap %.2f MiB and resident %s per instance",
		per(fullHeap, baseHeap), rss(fullRSS, baseRSS))
	runtime.KeepAlive(created)
}

// residentSelf is this process's resident set in bytes, from /proc, or -1 where there is
// no /proc: the heap figures, which are the ones to compare, still report on any platform.
func residentSelf() int64 {
	status, err := os.ReadFile("/proc/self/status")
	if err != nil {
		return -1
	}
	for _, line := range strings.Split(string(status), "\n") {
		if fields := strings.Fields(line); len(fields) >= 2 && fields[0] == "VmRSS:" {
			kib, err := strconv.ParseInt(fields[1], 10, 64)
			if err != nil {
				return -1
			}
			return kib << 10
		}
	}
	return -1
}
