package game

import (
	"context"
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
	baseHeap, baseRSS := heap(), residentSelf(t)
	var created []InstanceSession
	for i := range instances {
		s, err := manager.Create(InstanceRuin{CellX: int64(i)})
		if err != nil {
			t.Fatal(err)
		}
		created = append(created, s)
	}
	idleHeap, idleRSS := heap(), residentSelf(t)
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
		if got := s.Chunks.Len(); got != halo-held {
			t.Logf("instance %d holds %d chunks of an envelope of %d", s.ID, got, halo-held)
		}
	}
	fullHeap, fullRSS := heap(), residentSelf(t)
	per := func(after, before uint64) float64 { return (float64(after) - float64(before)) / instances / (1 << 20) }
	t.Logf("instances=%d envelope_chunks=%.1f chunks_with_blocks=%.1f per instance",
		instances, float64(halo)/instances, float64(shell)/instances)
	t.Logf("created, nothing loaded: heap %.2f MiB and resident %.2f MiB per instance",
		per(idleHeap, baseHeap), per(idleRSS, baseRSS))
	t.Logf("whole envelope resident: heap %.2f MiB and resident %.2f MiB per instance",
		per(fullHeap, baseHeap), per(fullRSS, baseRSS))
	runtime.KeepAlive(created)
}

// residentSelf is this process's resident set, from /proc.
func residentSelf(t *testing.T) uint64 {
	t.Helper()
	status, err := os.ReadFile("/proc/self/status")
	if err != nil {
		t.Fatal(err)
	}
	for _, line := range strings.Split(string(status), "\n") {
		if fields := strings.Fields(line); len(fields) >= 2 && fields[0] == "VmRSS:" {
			kib, err := strconv.ParseUint(fields[1], 10, 64)
			if err != nil {
				t.Fatal(err)
			}
			return kib << 10
		}
	}
	t.Fatal("no VmRSS line")
	return 0
}
