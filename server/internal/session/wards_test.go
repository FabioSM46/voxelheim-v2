package session

import (
	"context"
	"io"
	"log/slog"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func TestWardWorkerOrdersEveryReplacementAheadOfSnapshots(t *testing.T) {
	t.Parallel()

	const seed = int64(773)
	cache := world.NewCache(seed, 1, 8)
	peers := NewRegistry()
	log := slog.New(slog.NewTextHandler(io.Discard, nil))
	sim, err := game.NewSim(20, 1, seed, game.NewCacheTerrain(cache), cache, peers.NextID, log)
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}

	playerID := identity.PlayerID{1}
	centers := make(chan world.Column)
	snapshots := make(chan []byte, 8)
	sent := make(chan []byte, 8)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	go followSnapshotsAndWards(ctx, playerID, sim, 1, centers, snapshots,
		func(frame []byte) error { sent <- frame; return nil },
		func(frame []byte) bool { sent <- frame; return true }, log)

	// A tick may win the race with initial streaming. Its snapshot waits until MoveTo
	// publishes the centre, then the empty full replacement is the first frame out.
	snapshots <- protocol.EncodeEntitySnapshot(protocol.EntitySnapshot{})
	centers <- world.Column{CX: 0, CZ: 0}
	wantPayloads(t, sent, vnet.PayloadWardsNearby, vnet.PayloadEntitySnapshot)

	// A runestone rebuild while the player stands still is noticed on the next snapshot.
	if err := sim.RestoreStructures([]game.Structure{{
		Kind: vnet.StructureKindRunestone, Anchor: [3]int32{0, 63, 0}, Facing: vnet.FacingNorth, Owner: playerID,
	}}); err != nil {
		t.Fatalf("RestoreStructures: %v", err)
	}
	snapshots <- protocol.EncodeEntitySnapshot(protocol.EntitySnapshot{Tick: 2})
	wantPayloads(t, sent, vnet.PayloadWardsNearby, vnet.PayloadEntitySnapshot)

	// A border crossing is itself a trigger; it does not wait for another tick.
	centers <- world.Column{CX: 4, CZ: -3}
	wantPayloads(t, sent, vnet.PayloadWardsNearby)
}

func wantPayloads(t *testing.T, frames <-chan []byte, want ...vnet.Payload) {
	t.Helper()
	for i, expected := range want {
		select {
		case frame := <-frames:
			got := vnet.GetRootAsEnvelope(frame, 0).PayloadType()
			if got != expected {
				t.Fatalf("frame %d = %s, want %s", i, got, expected)
			}
		case <-time.After(time.Second):
			t.Fatalf("timed out waiting for frame %d (%s)", i, expected)
		}
	}
}
