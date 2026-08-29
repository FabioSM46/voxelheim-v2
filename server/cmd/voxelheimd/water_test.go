package main

import (
	"bytes"
	"context"
	"slices"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func TestWaterTickBroadcastsTheSameOrderedChangesToEveryHolder(t *testing.T) {
	chunks := world.NewCache(1, 1, 8)
	coord := world.Coord{}
	if _, _, err := chunks.Get(context.Background(), coord); err != nil {
		t.Fatalf("Get: %v", err)
	}
	reg := session.NewRegistry()
	sim, err := game.NewSim(20, 0, 1, game.NewCacheTerrain(chunks), chunks, reg.NextID, discard())
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}
	if err := sim.ConfigureWater(chunks); err != nil {
		t.Fatalf("ConfigureWater: %v", err)
	}

	indices := make([]int, 0, 2)
	for _, x := range []int64{10, 8} {
		if err := chunks.ApplyResidentGuarded(x, 20, 8, world.Water, nil); err != nil {
			t.Fatalf("source: %v", err)
		}
		if err := chunks.ApplyResidentGuarded(x, 19, 8, world.Air, nil); err != nil {
			t.Fatalf("target: %v", err)
		}
		indices = append(indices, world.Index(world.Local(x), 20, 8))
	}
	if err := sim.QueueUnstableWater(context.Background(), coord, indices); err != nil {
		t.Fatalf("QueueUnstableWater: %v", err)
	}

	var received [2][][]byte
	for i := range received {
		view := session.NewView(0)
		view.MarkLoaded(coord)
		at := i
		reg.Subscribe(uint64(i+1), view, func() {}, func(frame []byte) bool {
			received[at] = append(received[at], slices.Clone(frame))
			return true
		})
	}
	srv := server{registry: reg}
	changes := sim.Step(1)
	srv.broadcastWaterChanges(changes)
	if len(changes) != 2 {
		t.Fatalf("Step changed %d voxels, want 2", len(changes))
	}
	want := make([][]byte, len(changes))
	for i, change := range changes {
		want[i] = protocol.EncodeBlockUpdate(protocol.BlockUpdate{Pos: change.Pos(), BlockID: uint16(change.Block)})
	}
	for i := range received {
		if !slices.EqualFunc(received[i], want, bytes.Equal) {
			t.Errorf("holder %d received a different BlockUpdate sequence", i)
		}
	}
}
