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
	flatbuffers "github.com/google/flatbuffers/go"
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
	snapshots := make(chan snapshotAt, 1)
	sent := make(chan []byte, 8)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	go followSnapshotsAndWards(ctx, playerID, sim, 1, centers, snapshots,
		func(frame []byte) error { sent <- frame; return nil },
		func(frame []byte) bool { sent <- frame; return true }, log)

	// A tick may win the race with initial streaming. Its snapshot waits until MoveTo
	// publishes the centre, then the empty full replacement is the first frame out.
	snapshots <- snapshotAt{frame: protocol.EncodeEntitySnapshot(protocol.EntitySnapshot{}), center: world.Column{CX: 0, CZ: 0}}
	centers <- world.Column{CX: 0, CZ: 0}
	wantPayloads(t, sent, vnet.PayloadWardsNearby, vnet.PayloadEntitySnapshot)

	// A runestone rebuild while the player stands still is noticed on the next snapshot.
	if err := sim.RestoreStructures([]game.Structure{{
		Kind: vnet.StructureKindRunestone, Anchor: [3]int32{0, 63, 0}, Facing: vnet.FacingNorth, Owner: playerID,
	}}); err != nil {
		t.Fatalf("RestoreStructures: %v", err)
	}
	snapshots <- snapshotAt{frame: protocol.EncodeEntitySnapshot(protocol.EntitySnapshot{Tick: 2}), center: world.Column{CX: 0, CZ: 0}}
	wantPayloads(t, sent, vnet.PayloadWardsNearby, vnet.PayloadEntitySnapshot)

	// A border crossing is itself a trigger; it does not wait for another tick.
	centers <- world.Column{CX: 4, CZ: -3}
	wantPayloads(t, sent, vnet.PayloadWardsNearby)
}

func TestWardWorkerHoldsACrossingSnapshotUntilItsStreamCenterArrives(t *testing.T) {
	t.Parallel()

	const seed = int64(773)
	cache := world.NewCache(seed, 1, 8)
	peers := NewRegistry()
	log := slog.New(slog.NewTextHandler(io.Discard, nil))
	sim, err := game.NewSim(20, 1, seed, game.NewCacheTerrain(cache), cache, peers.NextID, log)
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}

	centers := make(chan world.Column)
	// Unbuffered here so the send below acknowledges that the worker has consumed the
	// crossing snapshot into its local pending slot before the matching centre exists.
	snapshots := make(chan snapshotAt)
	sent := make(chan []byte, 8)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	go followSnapshotsAndWards(ctx, identity.PlayerID{1}, sim, 1, centers, snapshots,
		func(frame []byte) error { sent <- frame; return nil },
		func(frame []byte) bool { sent <- frame; return true }, log)

	centers <- world.Column{CX: 0, CZ: 0}
	wantPayloads(t, sent, vnet.PayloadWardsNearby)

	// The simulation has crossed first, as Step orders it, but MoveTo has not yet
	// completed. The new snapshot cannot be released under the old centre's ward list.
	next := world.Column{CX: 1, CZ: 0}
	snapshots <- snapshotAt{frame: protocol.EncodeEntitySnapshot(protocol.EntitySnapshot{Tick: 2}), center: next}
	select {
	case frame := <-sent:
		t.Fatalf("crossing snapshot escaped before its stream centre as %s", vnet.GetRootAsEnvelope(frame, 0).PayloadType())
	default:
	}

	centers <- next
	wantPayloads(t, sent, vnet.PayloadWardsNearby, vnet.PayloadEntitySnapshot)
}

func TestSnapshotHandoffKeepsOnlyTheNewestTick(t *testing.T) {
	t.Parallel()

	snapshots := make(chan snapshotAt, 1)
	if !offerLatestSnapshot(snapshots, snapshotAt{frame: protocol.EncodeEntitySnapshot(protocol.EntitySnapshot{Tick: 1})}) {
		t.Fatal("the first snapshot was refused")
	}
	if !offerLatestSnapshot(snapshots, snapshotAt{frame: protocol.EncodeEntitySnapshot(protocol.EntitySnapshot{Tick: 2})}) {
		t.Fatal("the replacement snapshot was refused")
	}

	got := <-snapshots
	envelope := vnet.GetRootAsEnvelope(got.frame, 0)
	table := new(flatbuffers.Table)
	if !envelope.Payload(table) {
		t.Fatal("the newest snapshot payload is absent")
	}
	var snapshot vnet.EntitySnapshot
	snapshot.Init(table.Bytes, table.Pos)
	if snapshot.ServerTick() != 2 {
		t.Fatalf("snapshot handoff retained tick %d, want newest tick 2", snapshot.ServerTick())
	}
}

func TestWardWorkerDropsPendingSnapshotForNewestWhileWardSendIsBackpressured(t *testing.T) {
	t.Parallel()

	const seed = int64(773)
	cache := world.NewCache(seed, 1, 8)
	peers := NewRegistry()
	log := slog.New(slog.NewTextHandler(io.Discard, nil))
	sim, err := game.NewSim(20, 1, seed, game.NewCacheTerrain(cache), cache, peers.NextID, log)
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}

	centers := make(chan world.Column)
	snapshots := make(chan snapshotAt, 1)
	sent := make(chan []byte, 8)
	secondWardStarted := make(chan struct{})
	releaseSecondWard := make(chan struct{})
	wardCount := 0
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	go followSnapshotsAndWards(ctx, identity.PlayerID{1}, sim, 1, centers, snapshots,
		func(frame []byte) error {
			if vnet.GetRootAsEnvelope(frame, 0).PayloadType() == vnet.PayloadWardsNearby {
				wardCount++
				if wardCount == 2 {
					close(secondWardStarted)
					<-releaseSecondWard
				}
			}
			sent <- frame
			return nil
		},
		func(frame []byte) bool { sent <- frame; return true }, log)

	centers <- world.Column{CX: 0, CZ: 0}
	wantPayloads(t, sent, vnet.PayloadWardsNearby)
	next := world.Column{CX: 1, CZ: 0}
	if !offerLatestSnapshot(snapshots, snapshotAt{frame: protocol.EncodeEntitySnapshot(protocol.EntitySnapshot{Tick: 1}), center: next}) {
		t.Fatal("the first crossing snapshot was refused")
	}
	centers <- next
	select {
	case <-secondWardStarted:
	case <-time.After(time.Second):
		t.Fatal("the crossing ward send never reached backpressure")
	}
	for _, tick := range []uint32{2, 3} {
		if !offerLatestSnapshot(snapshots, snapshotAt{frame: protocol.EncodeEntitySnapshot(protocol.EntitySnapshot{Tick: tick}), center: next}) {
			t.Fatalf("snapshot tick %d was refused", tick)
		}
	}
	close(releaseSecondWard)
	wantPayloads(t, sent, vnet.PayloadWardsNearby)

	select {
	case frame := <-sent:
		envelope := vnet.GetRootAsEnvelope(frame, 0)
		if envelope.PayloadType() != vnet.PayloadEntitySnapshot {
			t.Fatalf("frame after wards = %s, want EntitySnapshot", envelope.PayloadType())
		}
		table := new(flatbuffers.Table)
		if !envelope.Payload(table) {
			t.Fatal("the released snapshot payload is absent")
		}
		var snapshot vnet.EntitySnapshot
		snapshot.Init(table.Bytes, table.Pos)
		if snapshot.ServerTick() != 3 {
			t.Fatalf("backpressure released tick %d, want newest tick 3", snapshot.ServerTick())
		}
	case <-time.After(time.Second):
		t.Fatal("the newest snapshot was not released")
	}
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
