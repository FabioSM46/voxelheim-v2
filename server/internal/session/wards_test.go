package session

import (
	"context"
	"io"
	"log/slog"
	"sync"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
	flatbuffers "github.com/google/flatbuffers/go"
)

// startWorkers runs the two loops #669 split apart, as Serve does.
func startWorkers(
	ctx context.Context,
	t *testing.T,
	sim *game.Sim,
	centers chan world.Column,
	snapshots chan snapshotAt,
	send func([]byte) error,
	offer func([]byte) bool,
) {
	t.Helper()
	log := slog.New(slog.NewTextHandler(io.Discard, nil))
	go followSnapshots(ctx, snapshots, offer, log)
	go followWards(ctx, identity.PlayerID{1}, sim, 1, centers, send, log)
}

// **This test asserted the defect, and it is the third in this repository to do so.**
//
// It was `TestWardWorkerOrdersEveryReplacementAheadOfSnapshots`, and it required a
// WardsNearby to precede every snapshot. Holding a position until the ward list for its
// column could go out first is what stopped the character dead for 196–245 ms on every
// chunk boundary crossed — measured with a player at the controls, against a frame rate
// that never moved. The ordering was worth exactly one translucent wall drawn a tick
// late, and it was being paid for with the thing the player actually feels.
//
// So the requirement is inverted: a ward replacement must not delay a position. What is
// still required is that the replacement happens at all, on a centre change and on a
// revision change, which is the other half of what the old test covered.
func TestAWardReplacementDoesNotDelayTheSnapshot(t *testing.T) {
	t.Parallel()

	const seed = int64(773)
	cache := world.NewCache(seed, 1, 8)
	peers := NewRegistry(DefaultConcurrentSessions)
	log := slog.New(slog.NewTextHandler(io.Discard, nil))
	sim, err := game.NewSim(20, 1, seed, game.NewCacheTerrain(cache), cache, peers.NextID, log)
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}

	playerID := identity.PlayerID{1}
	centers := make(chan world.Column)
	snapshots := make(chan snapshotAt, 1)
	wards := make(chan []byte, 8)
	positions := make(chan []byte, 8)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	startWorkers(ctx, t, sim, centers, snapshots,
		func(frame []byte) error { wards <- frame; return nil },
		func(frame []byte) bool { positions <- frame; return true })
	_ = playerID
	_ = log

	// A position with no centre published at all — the case the gate used to hold
	// indefinitely — goes straight out.
	snapshots <- snapshotAt{frame: protocol.EncodeEntitySnapshot(protocol.EntitySnapshot{}), center: world.Column{CX: 9, CZ: 9}}
	select {
	case <-positions:
	case <-time.After(2 * time.Second):
		t.Fatal("a snapshot was not released without a matching stream centre")
	}

	// And the ward replacement still happens, on the centre change.
	centers <- world.Column{CX: 0, CZ: 0}
	select {
	case frame := <-wards:
		if got := vnet.GetRootAsEnvelope(frame, 0).PayloadType(); got != vnet.PayloadWardsNearby {
			t.Fatalf("the centre change sent %s, want a ward replacement", got)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("a centre change sent no ward replacement")
	}

	// A runestone raised while the player stands still is still noticed, now by the
	// worker's own poll rather than by riding on a snapshot.
	if err := sim.RestoreStructures([]game.Structure{{
		Kind: vnet.StructureKindRunestone, Anchor: [3]int32{0, 63, 0}, Facing: vnet.FacingNorth, Owner: identity.PlayerID{1},
	}}); err != nil {
		t.Fatalf("RestoreStructures: %v", err)
	}
	select {
	case frame := <-wards:
		if got := vnet.GetRootAsEnvelope(frame, 0).PayloadType(); got != vnet.PayloadWardsNearby {
			t.Fatalf("the revision change sent %s, want a ward replacement", got)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("a ward revision change was never noticed")
	}
}

// The crossing case, which is the reported stutter reduced to a test.
//
// This was `TestWardWorkerHoldsACrossingSnapshotUntilItsStreamCenterArrives` and it
// required the hold. The simulation crosses a boundary before the streamer does, exactly
// as Step orders it; the position for the new column must go out on that tick, not on
// whichever later tick the chunks finish leaving.
func TestACrossingSnapshotIsNotHeldForItsStreamCentre(t *testing.T) {
	t.Parallel()

	const seed = int64(773)
	cache := world.NewCache(seed, 1, 8)
	peers := NewRegistry(DefaultConcurrentSessions)
	log := slog.New(slog.NewTextHandler(io.Discard, nil))
	sim, err := game.NewSim(20, 1, seed, game.NewCacheTerrain(cache), cache, peers.NextID, log)
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}

	centers := make(chan world.Column)
	snapshots := make(chan snapshotAt)
	wards := make(chan []byte, 8)
	positions := make(chan []byte, 8)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	startWorkers(ctx, t, sim, centers, snapshots,
		func(frame []byte) error { wards <- frame; return nil },
		func(frame []byte) bool { positions <- frame; return true })

	centers <- world.Column{CX: 0, CZ: 0}
	select {
	case <-wards:
	case <-time.After(2 * time.Second):
		t.Fatal("the initial ward replacement never arrived")
	}

	// The streamer has not reached this column and may not for hundreds of milliseconds.
	next := world.Column{CX: 1, CZ: 0}
	snapshots <- snapshotAt{frame: protocol.EncodeEntitySnapshot(protocol.EntitySnapshot{Tick: 2}), center: next}
	select {
	case <-positions:
	case <-time.After(2 * time.Second):
		t.Fatal("the crossing snapshot was held for its stream centre; that is the stutter")
	}
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

// A ward send stuck on the bulk lane must not stop positions.
//
// This was `TestWardWorkerDropsPendingSnapshotForNewestWhileWardSendIsBackpressured`,
// which described what the shared loop did when [sendWards] blocked: the pending position
// was replaced by newer ones and none of them went anywhere. Since the two run apart, a
// blocked ward send is invisible to the position, and that is the property worth pinning —
// it is the one that makes "nothing can delay a position" structural rather than careful.
func TestABlockedWardSendDoesNotStopPositions(t *testing.T) {
	t.Parallel()

	const seed = int64(773)
	cache := world.NewCache(seed, 1, 8)
	peers := NewRegistry(DefaultConcurrentSessions)
	log := slog.New(slog.NewTextHandler(io.Discard, nil))
	sim, err := game.NewSim(20, 1, seed, game.NewCacheTerrain(cache), cache, peers.NextID, log)
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}

	centers := make(chan world.Column)
	snapshots := make(chan snapshotAt, 1)
	positions := make(chan []byte, 8)
	wardBlocked := make(chan struct{})
	releaseWard := make(chan struct{})
	var once sync.Once
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	startWorkers(ctx, t, sim, centers, snapshots,
		func(frame []byte) error {
			once.Do(func() { close(wardBlocked) })
			<-releaseWard
			return nil
		},
		func(frame []byte) bool { positions <- frame; return true })

	centers <- world.Column{CX: 0, CZ: 0}
	select {
	case <-wardBlocked:
	case <-time.After(2 * time.Second):
		t.Fatal("the ward send never started")
	}

	// With the ward worker parked inside its send, three ticks of position still land.
	for tick := uint32(1); tick <= 3; tick++ {
		snapshots <- snapshotAt{frame: protocol.EncodeEntitySnapshot(protocol.EntitySnapshot{Tick: tick}), center: world.Column{CX: 0, CZ: 0}}
		select {
		case <-positions:
		case <-time.After(2 * time.Second):
			close(releaseWard)
			t.Fatalf("position for tick %d was stopped by a blocked ward send", tick)
		}
	}
	close(releaseWard)
}
