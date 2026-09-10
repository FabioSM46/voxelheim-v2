package session_test

import (
	"context"
	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
	"slices"
	"testing"
)

func (c *collector) transitions() []protocol.WorldChange {
	c.mu.Lock()
	defer c.mu.Unlock()
	return slices.Clone(c.worldChanges)
}

func portalSession(t *testing.T, limit int) (session.Config, *world.Cache, *game.Sim, *session.Registry, protocol.PortalRequest) {
	t.Helper()
	cfg := editConfig()
	cfg.WorldSeed = 0x5EED
	ruin, ok := world.RuinAt(cfg.WorldSeed, 0, 0)
	if !ok {
		t.Fatal("fixture ruin missing")
	}
	cfg.Spawn = [3]float32{float32(ruin.Arch.X) + 1.5, float32(ruin.Arch.Y) - 1, float32(ruin.Arch.Z) + .5}
	group := game.NewWorldGroup()
	chunks, sim, peers := editDeps(t, cfg, game.WithWorldGroup(group))
	manager, err := game.NewInstanceManager(cfg.TickRate, cfg.ViewDistance, limit, peers.NextID, discard(), game.WithWorldGroup(group))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(manager.Close)
	cfg.Instances = manager
	return cfg, chunks, sim, peers, protocol.PortalRequest{HasArch: true, Arch: [3]int32{int32(ruin.Arch.X), int32(ruin.Arch.Y), int32(ruin.Arch.Z)}}
}

func TestPortalRequestCrossesLiveBindingAndExitReturnsToEntryArch(t *testing.T) {
	cfg, chunks, open, peers, request := portalSession(t, 2)
	id := peers.NextID()
	conn, frames := admit(t, cfg, chunks, open, peers, id)
	conn.in <- protocol.EncodePortalRequest(request)
	waitUntil(t, "world change", func() bool { return len(frames.transitions()) == 1 })
	change := frames.transitions()[0]
	instance, exists := cfg.Instances.Lookup(change.WorldID)
	if !exists || change.WorldID == 0 || change.WorldSeed != instance.Seed || !change.HasExitArch {
		t.Fatal("wrong selected world", change)
	}
	arrival, exit := world.InstanceAnchors(instance.Seed)
	if change.Arrival != [3]float32{float32(arrival.X) + .5, float32(arrival.Y), float32(arrival.Z) + .5} || change.ExitArch != [3]int32{int32(exit.X), int32(exit.Y), int32(exit.Z)} {
		t.Fatal("arrival was not server chosen", change)
	}
	waitUntil(t, "player in destination", func() bool { return open.Count() == 0 && instance.Sim.Count() == 1 })
	if len(instance.Members) != 1 {
		t.Fatal("world binding lacks occupancy")
	}
	centre := world.ChunkOf(arrival.X, arrival.Y, arrival.Z)
	waitUntil(t, "destination chunk", func() bool { return frames.chunkCount(centre) > 0 })
	// Naming the known exit is still insufficient while the body is out of reach.
	conn.in <- protocol.EncodePortalRequest(protocol.PortalRequest{HasArch: true, Arch: change.ExitArch})
	waitUntil(t, "distant exit refusal", func() bool { return len(frames.actionRefusals()) == 1 })
	if frames.actionRefusals()[0].Reason != vnet.RefusalReasonNotAtPortal {
		t.Fatal("distant exit accepted")
	}
	// Ordinary movement, observed through a subsequent chat acknowledgement, brings
	// the player into reach. No client-selected destination is added for the test.
	conn.in <- protocol.EncodePlayerInput(protocol.PlayerInput{ClientTick: 1, MoveX: float32(exit.X-arrival.X) / 6, MoveZ: -float32(exit.Z-arrival.Z) / 6})
	conn.in <- protocol.EncodeChatRequest(protocol.ChatRequest{Text: "moving"})
	waitUntil(t, "input processed", func() bool { return len(frames.chatMessages()) == 1 })
	for range 10 {
		cfg.Instances.Step()
	}
	conn.in <- protocol.EncodePortalRequest(protocol.PortalRequest{HasArch: true, Arch: change.ExitArch})
	waitUntil(t, "return world change", func() bool { return len(frames.transitions()) == 2 })
	returned := frames.transitions()[1]
	if returned.WorldID != 0 || returned.WorldSeed != cfg.WorldSeed || returned.HasExitArch || returned.Arrival != cfg.Spawn {
		t.Fatal("exit did not return to entry standing position", returned)
	}
	waitUntil(t, "player returned to open simulation", func() bool { return open.Count() == 1 && instance.Sim.Count() == 0 })
	waitUntil(t, "exit releases occupancy", func() bool { retained, _ := cfg.Instances.Lookup(change.WorldID); return len(retained.Members) == 0 })
	// The manager observes abandoned combat on its next authoritative tick.
	// Production keeps ticking between requests; this fixture drives it explicitly.
	cfg.Instances.Step()
	beforeChunks := frames.chunkCount(centre)
	// Re-entry before expiry keeps exactly that private copy.
	conn.in <- protocol.EncodePortalRequest(request)
	waitUntil(t, "reentry", func() bool { return len(frames.transitions()) == 3 })
	if frames.transitions()[2].WorldID != change.WorldID {
		t.Fatal("reentry replaced retained instance")
	}
	waitUntil(t, "reentry streaming resumed", func() bool { return frames.chunkCount(centre) > beforeChunks })
}

func TestPortalRefusalsCarryNoDestinationAndDoNotMoveTheSession(t *testing.T) {
	for _, mode := range []string{"invented", "away", "cap", "creation"} {
		t.Run(mode, func(t *testing.T) {
			cfg, chunks, sim, peers, request := portalSession(t, 1)
			reason := vnet.RefusalReasonNotAtPortal
			switch mode {
			case "invented":
				request.Arch[0]++
			case "away":
				cfg.Spawn[0] += 20
			case "cap":
				if _, err := cfg.Instances.Create(game.InstanceRuin{CellX: 100, CellZ: 200}); err != nil {
					t.Fatal(err)
				}
				reason = vnet.RefusalReasonInstanceLimit
			case "creation":
				cfg.Instances.Close()
				reason = vnet.RefusalReasonInstanceUnavailable
			}
			conn, frames := admit(t, cfg, chunks, sim, peers, peers.NextID())
			conn.in <- protocol.EncodePortalRequest(request)
			waitUntil(t, "portal refusal", func() bool { return len(frames.actionRefusals()) == 1 })
			got := frames.actionRefusals()[0]
			if got.Action != vnet.RefusedActionCrossPortal || got.Reason != reason || got.HasAnchor {
				t.Fatal("refusal leaks target", got)
			}
			if len(frames.transitions()) != 0 || sim.Count() != 1 {
				t.Fatal("refusal changed world")
			}
		})
	}
}

func TestPortalWorldChangeIsNotClientIntent(t *testing.T) {
	cfg, chunks, sim, peers, _ := portalSession(t, 1)
	conn := newFakeConn()
	_ = collect(t, conn)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := make(chan error, 1)
	go func() {
		done <- session.Serve(ctx, conn, cfg, noTimeouts(), chunks, sim, peers, ephemeralIdentities(), peers.NextID(), discard())
	}()
	conn.in <- hello(1)
	createCharacter(conn, "Traveller")
	waitUntil(t, "admission", func() bool { return sim.Count() == 1 })
	frame, err := protocol.EncodeWorldChange(protocol.WorldChange{WorldID: 123, WorldSeed: 456, HasExitArch: true, Arrival: [3]float32{.5, 1, .5}})
	if err != nil {
		t.Fatal(err)
	}
	conn.in <- frame
	if err := <-done; err == nil {
		t.Fatal("authoritative client frame accepted")
	}
	_ = conn.Close()
	if sim.Count() != 0 || cfg.Instances.Count() != 0 {
		t.Fatal("forged state created instance")
	}
}

func TestInstanceMarkerRequestsCannotReadOrMutateTheOpenWorldLedger(t *testing.T) {
	cfg, chunks, sim, peers, request := portalSession(t, 1)
	conn, frames := admit(t, cfg, chunks, sim, peers, peers.NextID())
	conn.in <- protocol.EncodeMarkerPlaceRequest(protocol.MarkerPlaceRequest{X: 100, Z: 200, Kind: vnet.MarkerKindNote, Note: "open world"})
	waitUntil(t, "open marker", func() bool { return len(frames.markerListsSeen()) == 2 })
	original := frames.markerListsSeen()[1]
	if len(original) != 1 {
		t.Fatal("fixture marker missing")
	}
	conn.in <- protocol.EncodePortalRequest(request)
	waitUntil(t, "entered instance", func() bool { return len(frames.transitions()) == 1 })
	change := frames.transitions()[0]
	instance, _ := cfg.Instances.Lookup(change.WorldID)
	arrival, exit := world.InstanceAnchors(instance.Seed)
	// **Both walk ends are made resident before anybody walks.** A body over a chunk that
	// is not resident does not move, and the instance's chunks are otherwise generated by
	// the streamer racing the steps below: on a loaded runner the ten steps could spend
	// their intent standing still and never reach the exit arch.
	for _, anchor := range []world.PlacedAnchor{arrival, exit} {
		generateAround(t, instance.Chunks, [3]float32{float32(anchor.X), float32(anchor.Y), float32(anchor.Z)}, 1)
	}

	// **The "requests consumed" sentinel is an inventory move, not a chat line.** A move's
	// answer is sent through the blocking queue, while chat is a non-blocking broadcast that
	// a queue filled by the crossing's chunk stream drops, which is how this wait timed out
	// on CI. The session reads frames in order, so the move's answer follows the markers.
	moved := len(frames.inventoryStates())
	conn.in <- protocol.EncodeMarkerPlaceRequest(protocol.MarkerPlaceRequest{X: 0, Z: 0, Kind: vnet.MarkerKindNote, Note: "instance", ClientTick: 1})
	conn.in <- protocol.EncodeMarkerRemoveRequest(protocol.MarkerRemoveRequest{MarkerID: original[0].MarkerID})
	conn.in <- protocol.EncodeInventoryMoveRequest(protocol.InventoryMoveRequest{From: 0, To: 1, Count: 1})
	waitUntil(t, "marker requests consumed", func() bool { return len(frames.inventoryStates()) == moved+1 })
	if len(frames.markerListsSeen()) != 2 {
		t.Fatal("instance request exposed open-world marker list")
	}
	conn.in <- protocol.EncodePlayerInput(protocol.PlayerInput{ClientTick: 2, MoveX: float32(exit.X-arrival.X) / 6, MoveZ: -float32(exit.Z-arrival.Z) / 6})
	conn.in <- protocol.EncodeInventoryMoveRequest(protocol.InventoryMoveRequest{From: 1, To: 0, Count: 1})
	waitUntil(t, "return movement accepted", func() bool { return len(frames.inventoryStates()) == moved+2 })
	for range 10 {
		cfg.Instances.Step()
	}
	conn.in <- protocol.EncodePortalRequest(protocol.PortalRequest{HasArch: true, Arch: change.ExitArch})
	waitUntil(t, "open marks restored", func() bool { return len(frames.markerListsSeen()) == 3 })
	restored := frames.markerListsSeen()[2]
	if !slices.Equal(restored, original) {
		t.Fatalf("instance requests changed durable ledger: %+v", restored)
	}
}
