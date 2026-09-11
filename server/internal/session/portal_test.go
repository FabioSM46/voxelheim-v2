package session_test

import (
	"context"
	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
	"math"
	"slices"
	"sync/atomic"
	"testing"
	"time"
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
	threshold := ruin.Threshold()
	if threshold.Normal != 2 {
		t.Fatal("the fixture ruin no longer faces along Z; the walk below would miss its veil")
	}
	// Two blocks in front of the veil, inside the ruin's lower chamber. The veil's plane is at
	// the heart's z + .5 and the entrance faces +Z, so walking forward at yaw 0 (-Z) from here
	// is walking straight into it.
	cfg.Spawn = [3]float32{float32(ruin.Arch.X) + 1.5, float32(ruin.Arch.Y) - 1, float32(ruin.Arch.Z) + 2.5}
	group := game.NewWorldGroup()
	chunks, sim, peers := editDeps(t, cfg, game.WithWorldGroup(group), game.WithPortals(threshold))
	// A body over a chunk that is not resident does not move, so the walk is made possible
	// before anybody tries it rather than raced against the streamer.
	generateAround(t, chunks, cfg.Spawn, 1)
	manager, err := game.NewInstanceManager(cfg.TickRate, cfg.ViewDistance, limit, peers.NextID, discard(), game.WithWorldGroup(group))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(manager.Close)
	cfg.Instances = manager
	return cfg, chunks, sim, peers, protocol.PortalRequest{HasArch: true, Arch: [3]int32{int32(ruin.Arch.X), int32(ruin.Arch.Y), int32(ruin.Arch.Z)}}
}

// walkClientTicks and walkWorldTicks are one rising counter each for the whole package. A
// player refuses a client tick that is not newer than the last it accepted and keeps that
// across a world change, and a simulation's tick only ever moves forward; drawing both
// from shared counters is what lets any test walk after any other movement.
var (
	walkClientTicks atomic.Uint32
	walkWorldTicks  atomic.Uint64
)

// stepOpen steps an open-world simulation one tick.
func stepOpen(sim *game.Sim) func() { return func() { sim.Step(walkWorldTicks.Add(1)) } }

// ticks is a done condition that holds after n steps.
func ticks(n int) func() bool {
	return func() bool {
		n--
		return n < 0
	}
}

// walkUntil does what a client's input loop does while its player holds a direction: one
// PlayerInput every tick, the world stepped between them, until done.
//
// **Crossing is not asked for here, and that is the point of walking.** Nothing this sends
// names a portal; a crossing, a refusal or an offer arrives only because the server saw the
// body reach a veil. The input is offered without blocking, so a session busy changing
// worlds costs a dropped heartbeat rather than a stalled test.
func walkUntil(t *testing.T, conn *fakeConn, moveX, moveZ float32, step func(), what string, done func() bool) {
	t.Helper()
	deadline := time.Now().Add(patience)
	for !done() {
		if time.Now().After(deadline) {
			t.Fatalf("timed out walking until %s", what)
		}
		select {
		case conn.in <- protocol.EncodePlayerInput(protocol.PlayerInput{ClientTick: walkClientTicks.Add(1), MoveX: moveX, MoveZ: moveZ}):
		default:
		}
		step()
		time.Sleep(time.Millisecond)
	}
}

// walkIntoVeil walks the fixture's body forward from portalSession's spawn into the ruin's veil.
func walkIntoVeil(t *testing.T, conn *fakeConn, open *game.Sim, what string, done func() bool) {
	t.Helper()
	walkUntil(t, conn, 0, 1, stepOpen(open), what, done)
}

// walkToExit walks an instance visitor from where it arrived toward its copy's own exit arch.
// Both ends are made resident first, for the reason portalSession makes the ruin's.
func walkToExit(t *testing.T, cfg session.Config, conn *fakeConn, change protocol.WorldChange, what string, done func() bool) {
	t.Helper()
	instance, _ := cfg.Instances.Lookup(change.WorldID)
	arrival, exit := world.InstanceAnchors(instance.Seed)
	for _, anchor := range []world.PlacedAnchor{arrival, exit} {
		generateAround(t, instance.Chunks, [3]float32{float32(anchor.X), float32(anchor.Y), float32(anchor.Z)}, 1)
	}
	dx, dz := float64(exit.X-arrival.X), float64(exit.Z-arrival.Z)
	length := math.Hypot(dx, dz)
	walkUntil(t, conn, float32(dx/length), -float32(dz/length), cfg.Instances.Step, what, done)
}

func TestWalkingIntoTheVeilCrossesAndTheExitReturnsWithoutALoop(t *testing.T) {
	cfg, chunks, open, peers, request := portalSession(t, 2)
	id := peers.NextID()
	conn, frames := admit(t, cfg, chunks, open, peers, id)
	// No key and no request: reaching the veil is the whole of the ask.
	walkIntoVeil(t, conn, open, "world change", func() bool { return len(frames.transitions()) == 1 })
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
	// Naming the known exit crosses nothing, from anywhere: a request is not a crossing.
	conn.in <- protocol.EncodePortalRequest(protocol.PortalRequest{HasArch: true, Arch: change.ExitArch})
	waitUntil(t, "exit request refusal", func() bool { return len(frames.actionRefusals()) == 1 })
	if frames.actionRefusals()[0].Reason != vnet.RefusalReasonNotAtPortal {
		t.Fatal("an exit request was answered as a crossing")
	}
	// Walking into the copy's own return arch is.
	walkToExit(t, cfg, conn, change, "return world change", func() bool { return len(frames.transitions()) == 2 })
	returned := frames.transitions()[1]
	if returned.WorldID != 0 || returned.WorldSeed != cfg.WorldSeed || returned.HasExitArch {
		t.Fatal("exit did not return to the open world", returned)
	}
	// Back where the body crossed rather than where it spawned: on the spawn's line, past it
	// toward the arch, and not beyond the chamber behind the veil.
	plane := float32(request.Arch[2]) + .5
	if returned.Arrival[0] != cfg.Spawn[0] || returned.Arrival[2] >= cfg.Spawn[2] || returned.Arrival[2] < plane-3.5 {
		t.Fatalf("the exit returned to %v, not to where the body crossed near the plane at z=%v", returned.Arrival, plane)
	}
	waitUntil(t, "player returned to open simulation", func() bool { return open.Count() == 1 && instance.Sim.Count() == 0 })
	waitUntil(t, "exit releases occupancy", func() bool { retained, _ := cfg.Instances.Lookup(change.WorldID); return len(retained.Members) == 0 })
	// The manager observes abandoned combat on its next authoritative tick.
	// Production keeps ticking between requests; this fixture drives it explicitly.
	cfg.Instances.Step()

	// Arriving back in the veil it crossed through is not walking into it: standing there,
	// and carrying on the way the body was going, bounce nobody back into the dungeon.
	step := stepOpen(open)
	for range 40 {
		step()
	}
	walkUntil(t, conn, 0, 1, step, "carrying on through", ticks(20))
	for range 20 {
		step()
	}
	if got := len(frames.transitions()); got != 2 {
		t.Fatalf("returning through the veil bounced back into the dungeon: %d world changes", got)
	}
	beforeChunks := frames.chunkCount(centre)
	// Re-entry before expiry is walking back in, and it keeps exactly that private copy.
	walkUntil(t, conn, 0, -1, step, "reentry", func() bool { return len(frames.transitions()) == 3 })
	if frames.transitions()[2].WorldID != change.WorldID {
		t.Fatal("reentry replaced retained instance")
	}
	waitUntil(t, "reentry streaming resumed", func() bool { return frames.chunkCount(centre) > beforeChunks })
}

// settled waits until the session has read everything sent before it. An entry answer naming
// no offer is refused through the blocking queue, in order with the frames ahead of it, whatever
// the character carries or has sent before — which an inventory move or a loot request is not,
// because each of those answers some situations with silence.
func settled(t *testing.T, conn *fakeConn, frames *collector) {
	t.Helper()
	before := len(refusalsFor(frames, vnet.RefusalReasonEntryOfferUnknown))
	conn.in <- protocol.EncodeInstanceEntryAnswer(protocol.InstanceEntryAnswer{})
	waitUntil(t, "the session to catch up", func() bool {
		return len(refusalsFor(frames, vnet.RefusalReasonEntryOfferUnknown)) > before
	})
}

// refusalsFor is every refused crossing given one reason, and none of settled's sentinels.
func refusalsFor(frames *collector, reason vnet.RefusalReason) []protocol.ActionRefused {
	var refused []protocol.ActionRefused
	for _, r := range frames.actionRefusals() {
		if r.Action == vnet.RefusedActionCrossPortal && r.Reason == reason {
			refused = append(refused, r)
		}
	}
	return refused
}

func TestPortalRefusalsCarryNoDestinationAndAreOneAttemptPerContact(t *testing.T) {
	for _, mode := range []string{"request", "cap", "creation"} {
		t.Run(mode, func(t *testing.T) {
			cfg, chunks, sim, peers, request := portalSession(t, 1)
			reason := vnet.RefusalReasonNotAtPortal
			switch mode {
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
			if mode == "request" {
				// Naming the one real arch, from inside its own chamber, is still only a request.
				conn.in <- protocol.EncodePortalRequest(request)
				waitUntil(t, "portal refusal", func() bool { return len(frames.actionRefusals()) == 1 })
			} else {
				walkIntoVeil(t, conn, sim, "portal refusal", func() bool { return len(frames.actionRefusals()) == 1 })
			}
			got := frames.actionRefusals()[0]
			if got.Action != vnet.RefusedActionCrossPortal || got.Reason != reason || got.HasAnchor {
				t.Fatal("refusal leaks target", got)
			}
			if len(frames.transitions()) != 0 || sim.Count() != 1 {
				t.Fatal("refusal changed world")
			}
			if mode == "request" {
				return
			}

			// Staying on in or beyond the veil after a refusal repeats nothing.
			step := stepOpen(sim)
			for range 40 {
				step()
			}
			settled(t, conn, frames)
			if got := len(refusalsFor(frames, reason)); got != 1 {
				t.Fatalf("one contact was refused %d times", got)
			}
			// A second attempt is a second contact: out through the veil the way it came, and
			// in again. However far the first walk carried the body, these two walks pass the
			// plane exactly once more between them and end up in front of it.
			walkUntil(t, conn, 0, -1, step, "walking out", ticks(30))
			walkUntil(t, conn, 0, 1, step, "the second refusal", func() bool { return len(refusalsFor(frames, reason)) == 2 })
			for range 40 {
				step()
			}
			settled(t, conn, frames)
			if got := len(refusalsFor(frames, reason)); got != 2 {
				t.Fatalf("two contacts were refused %d times", got)
			}
			if len(frames.transitions()) != 0 || sim.Count() != 1 {
				t.Fatal("a refused contact changed world")
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
	cfg, chunks, sim, peers, _ := portalSession(t, 1)
	conn, frames := admit(t, cfg, chunks, sim, peers, peers.NextID())
	conn.in <- protocol.EncodeMarkerPlaceRequest(protocol.MarkerPlaceRequest{X: 100, Z: 200, Kind: vnet.MarkerKindNote, Note: "open world"})
	waitUntil(t, "open marker", func() bool { return len(frames.markerListsSeen()) == 2 })
	original := frames.markerListsSeen()[1]
	if len(original) != 1 {
		t.Fatal("fixture marker missing")
	}
	walkIntoVeil(t, conn, sim, "entered instance", func() bool { return len(frames.transitions()) == 1 })
	change := frames.transitions()[0]
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
	walkToExit(t, cfg, conn, change, "open marks restored", func() bool { return len(frames.markerListsSeen()) == 3 })
	restored := frames.markerListsSeen()[2]
	if !slices.Equal(restored, original) {
		t.Fatalf("instance requests changed durable ledger: %+v", restored)
	}
}
