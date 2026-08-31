package session_test

import (
	"context"
	"errors"
	"math"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// ---------------------------------------------------------------------------
// The broadcast set
// ---------------------------------------------------------------------------

// Who a voxel update goes to, at the level the rule is written: the registry. A session
// that does not hold the chunk cannot place the update, and a session that does and is
// skipped renders a world the server has already changed.
func TestBroadcastChunkReachesExactlyTheSessionsHoldingTheChunk(t *testing.T) {
	t.Parallel()

	edited := world.Coord{X: 1, Y: 2, Z: 3}
	elsewhere := world.Coord{X: -9, Y: 0, Z: 4}

	cases := map[string]struct {
		loaded []world.Coord
		// accepts is whether this session's queue takes the frame.
		accepts bool
		// reached is whether the frame is offered to it at all.
		reached bool
		// stillHeld is what its view says about the edited chunk afterwards. An update that
		// could not be delivered is not replaced by a later one, so the chunk itself has to
		// be forgotten and re-sent whole by the next diff.
		stillHeld bool
	}{
		"holds the edited chunk":                  {loaded: []world.Coord{edited}, accepts: true, reached: true, stillHeld: true},
		"holds the edited chunk among others":     {loaded: []world.Coord{elsewhere, edited}, accepts: true, reached: true, stillHeld: true},
		"holds a different chunk":                 {loaded: []world.Coord{elsewhere}, accepts: true, reached: false, stillHeld: false},
		"has been sent nothing yet":               {loaded: nil, accepts: true, reached: false, stillHeld: false},
		"holds it but its queue refuses the send": {loaded: []world.Coord{edited}, accepts: false, reached: false, stillHeld: false},
	}

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			reg := session.NewRegistry(session.DefaultConcurrentSessions)
			view := session.NewView(0)
			for _, coord := range tc.loaded {
				view.MarkLoaded(coord)
			}

			var offered, taken int
			reg.Subscribe(1, view, func() {}, func([]byte) bool {
				offered++
				if !tc.accepts {
					return false
				}
				taken++
				return true
			})

			frame := protocol.EncodeBlockUpdate(protocol.BlockUpdate{Pos: [3]int32{40, 70, 100}, BlockID: 1})
			delivered := reg.BroadcastChunk(edited, frame)

			wantDelivered := 0
			if tc.reached {
				wantDelivered = 1
			}
			if delivered != wantDelivered {
				t.Errorf("BroadcastChunk reported %d sessions reached, want %d", delivered, wantDelivered)
			}
			if taken != wantDelivered {
				t.Errorf("the session took %d frames, want %d", taken, wantDelivered)
			}
			// A session that does not hold the chunk must not even be offered the frame: the
			// decision is the registry's, not the send function's.
			if !tc.accepts && offered == 0 {
				t.Error("the frame was never offered, so the dropped-send case tested nothing")
			}
			if tc.accepts && !tc.reached && offered != 0 {
				t.Errorf("a session that does not hold the chunk was offered %d frames", offered)
			}

			if held := view.Holds(edited); held != tc.stillHeld {
				t.Errorf("Holds(edited) = %v afterwards, want %v", held, tc.stillHeld)
			}
		})
	}
}

// A session that has left must not be sent anything, and the guarantee has to be
// *ordered*: Serve closes the outbound channel immediately after unsubscribing, and a send
// on a closed channel is a panic in a goroutine that takes the process with it.
//
// This test crashes the test binary if BroadcastChunk can send after Unsubscribe returned.
func TestUnsubscribeStopsBroadcastsBeforeTheQueueCanBeClosed(t *testing.T) {
	t.Parallel()

	reg := session.NewRegistry(session.DefaultConcurrentSessions)
	coord := world.Coord{X: 0, Y: 0, Z: 0}
	view := session.NewView(0)
	view.MarkLoaded(coord)

	out := make(chan []byte, 8)
	drained := make(chan struct{})
	go func() {
		defer close(drained)
		//nolint:revive // draining until the channel closes is the whole body
		for range out {
		}
	}()

	reg.Subscribe(1, view, func() {}, func(frame []byte) bool {
		select {
		case out <- frame:
			return true
		default:
			return false
		}
	})

	frame := protocol.EncodeBlockUpdate(protocol.BlockUpdate{Pos: [3]int32{1, 2, 3}, BlockID: 1})

	var (
		delivered atomic.Int64
		stop      = make(chan struct{})
		broadcast sync.WaitGroup
	)
	broadcast.Add(1)
	go func() {
		defer broadcast.Done()
		for {
			select {
			case <-stop:
				return
			default:
				delivered.Add(int64(reg.BroadcastChunk(coord, frame)))
			}
		}
	}()

	waitUntil(t, "the first broadcast to reach the session", func() bool { return delivered.Load() > 0 })

	reg.Unsubscribe(1)
	close(out)
	<-drained

	// The broadcaster is still running. A registry that still knew about this peer would
	// send on the closed channel here.
	for range 1000 {
		reg.BroadcastChunk(coord, frame)
	}
	close(stop)
	broadcast.Wait()
}

// Sessions arriving and leaving while voxel updates are being broadcast, which is what a
// server does all day. Two properties, and the second is the one an assertion can reach:
//
//   - the process survives it. Serve unsubscribes from the registry before it closes the
//     outbound queue, so a broadcast can never send on a closed channel — a panic in a
//     goroutine that takes the whole server with it. The *ordering* is pinned by
//     TestUnsubscribeStopsBroadcastsBeforeTheQueueCanBeClosed, at the level where the
//     guarantee actually lives; what this adds is the real teardown path around it and,
//     under -race, the guard on the view a broadcast reads while the streamer writes it.
//   - once a session has ended, a broadcast reaches nobody.
//
// No tick loop, because none is needed: the player stays where it spawned, so its view keeps
// holding the chunk being broadcast to for the whole of each round.
func TestBroadcastsRunSafelyWhileSessionsArriveAndLeave(t *testing.T) {
	t.Parallel()

	cfg := serveConfig() // one chunk of view, so the held set is unambiguous
	chunks, sim, peers := editDeps(t, cfg)
	coord := world.ContainingChunk(cfg.Spawn[0], cfg.Spawn[1], cfg.Spawn[2])
	frame := protocol.EncodeBlockUpdate(protocol.BlockUpdate{Pos: [3]int32{1, 2, 3}, BlockID: 1})

	var (
		delivered atomic.Int64
		stop      = make(chan struct{})
		broadcast sync.WaitGroup
	)
	broadcast.Add(1)
	go func() {
		defer broadcast.Done()
		for {
			select {
			case <-stop:
				return
			default:
				delivered.Add(int64(peers.BroadcastChunk(coord, frame)))
				// Paced, and the pacing is load-bearing rather than politeness. An unbounded
				// broadcaster fills a 32-deep outbound queue in microseconds; the first dropped
				// frame forgets the chunk, the session stops being a target, and the rest of the
				// test would exercise nothing. Real updates arrive at the rate players dig.
				time.Sleep(50 * time.Microsecond)
			}
		}
	}()
	defer func() {
		close(stop)
		broadcast.Wait()
	}()

	// A hundred rounds, because what this looks for is a window one statement wide. Twenty
	// rounds caught a deliberately reordered teardown about half the time; a hundred caught it
	// on every attempt, and the whole loop still runs in under a fifth of a second.
	for round := range 100 {
		conn := newFakeConn()

		drained := make(chan struct{})
		go func() {
			defer close(drained)
			for {
				select {
				case _, ok := <-conn.out:
					if !ok {
						return
					}
				case <-conn.done:
					return
				}
			}
		}()

		served := make(chan error, 1)
		go func() {
			served <- session.Serve(context.Background(), conn, cfg, noTimeouts(), chunks, sim, peers, ephemeralIdentities(), uint64(round+1), discard())
		}()

		conn.in <- hello(1)
		createCharacter(conn, "Eivor")
		before := delivered.Load()
		waitUntil(t, "a broadcast to reach the session", func() bool { return delivered.Load() > before })

		if err := conn.Close(); err != nil {
			t.Fatalf("round %d: Close: %v", round, err)
		}
		select {
		case err := <-served:
			// A frame already in flight when the connection went away fails to write. That is
			// the disconnect arriving, not a protocol error, and since legacy PR 61 Serve says so
			// itself — so there is nothing to tolerate here and every error is a real one.
			if err != nil {
				t.Fatalf("round %d: Serve returned %v", round, err)
			}
		case <-time.After(patience):
			t.Fatalf("round %d: Serve did not return", round)
		}
		<-drained

		// The session is gone, so nothing about it is a broadcast target any more.
		if reached := peers.BroadcastChunk(coord, frame); reached != 0 {
			t.Fatalf("round %d: a broadcast reached %d sessions after the only one had ended", round, reached)
		}
	}
}

// ---------------------------------------------------------------------------
// End to end, over a socket
// ---------------------------------------------------------------------------

// editDeps builds one world, one simulation and one registry for a set of sessions, so an
// edit made through one connection is resolved against the terrain the others are streaming.
func editDeps(t *testing.T, cfg session.Config) (*world.Cache, *game.Sim, *session.Registry) {
	t.Helper()

	chunks := world.NewCache(cfg.WorldSeed, 4, 512)
	peers := session.NewRegistry(session.DefaultConcurrentSessions)
	sim, err := game.NewSim(cfg.TickRate, cfg.ViewDistance, cfg.WorldSeed, game.NewCacheTerrain(chunks), chunks, peers.NextID, discard())
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}
	return chunks, sim, peers
}

// editConfig is a session config with a one-chunk streaming radius: 27 chunks is few enough
// to wait for and wide enough that the voxel under the spawn is inside the view whichever
// side of a chunk border the surface falls on.
//
// **Its spawn is deliberately not [world.SpawnAt].** Since #519 that is the capital's gate
// square, and a settlement wards every column of its plateau — so these tests would be
// measuring a ward refusal instead of the edit path, and the one that is genuinely about
// warding would have nothing left to prove, its runestone claiming ground the settlement
// already holds.
func editConfig() session.Config {
	cfg := testConfig()
	cfg.ViewDistance = 1
	cfg.Spawn = openCountrySpawn(cfg.WorldSeed)
	return cfg
}

// admit runs one session and returns everything it wrote, once its whole view has arrived.
//
// Waiting for the full view is what makes the broadcast assertions exact rather than timing
// dependent: a chunk is only recorded as held after its frame has been handed to the writer,
// so a session halfway through its first view legitimately holds less than it will.
func admit(t *testing.T, cfg session.Config, chunks *world.Cache, sim *game.Sim, peers *session.Registry, entityID uint64) (*fakeConn, *collector) {
	t.Helper()
	return admitNamed(t, cfg, chunks, sim, peers, entityID, "Eivor")
}

func admitNamed(t *testing.T, cfg session.Config, chunks *world.Cache, sim *game.Sim, peers *session.Registry, entityID uint64, name string) (*fakeConn, *collector) {
	t.Helper()

	conn := newFakeConn()
	frames := collect(t, conn)

	ctx, cancel := context.WithCancel(context.Background())
	served := make(chan error, 1)
	go func() {
		served <- session.Serve(ctx, conn, cfg, noTimeouts(), chunks, sim, peers, ephemeralIdentities(), entityID, discard())
	}()
	t.Cleanup(func() {
		cancel()
		_ = conn.Close()
		// A frame already handed to the writer when the connection is closed underneath it
		// fails to write. Serve used to report that and this cleanup used to tolerate it;
		// since legacy PR 61 it is classified as the disconnect it is and the session ends cleanly,
		// so the tolerance is gone and any error at all is a failure.
		if err := <-served; err != nil {
			t.Errorf("session %d ended with %v", entityID, err)
		}
	})

	conn.in <- hello(byte(entityID))
	createCharacter(conn, name)
	view := (2*int(cfg.ViewDistance) + 1)
	wantChunks := view * view * view
	waitUntil(t, "the session's first view to arrive", func() bool {
		return len(frames.chunkCoords()) >= wantChunks
	})

	return conn, frames
}

// surfaceUnderSpawn is the world coordinate of the topmost solid voxel in the column
// [editConfig] stands a session in — the block a player is standing over, and the obvious
// thing to dig. That column is the world's origin, not [world.SpawnAt]'s.
func surfaceUnderSpawn(seed int64) [3]int32 {
	return [3]int32{0, int32(world.HeightAt(seed, 0, 0)), 0}
}

func mineRequest(pos [3]int32, clientTick uint32) []byte {
	return protocol.EncodeMineRequest(protocol.MineRequest{
		Pos: pos, HasPos: true, Active: true, ClientTick: clientTick,
	})
}

// mineUntilBreak refreshes intent once per authoritative tick until the session's
// off-tick worker broadcasts the result. The request rate is deliberately one per
// Step; the game tests separately pin that twenty requests cannot accelerate it.
func mineUntilBreak(t *testing.T, conn *fakeConn, frames *collector, sim *game.Sim, pos [3]int32, clientTick *uint32, serverTick *uint64) {
	t.Helper()
	before := len(frames.blockUpdates())
	// Generous against the slowest block in the hardness table rather than a round number.
	// This was 100, which sat comfortably above every cost until #178 raised the table and
	// iron ore went to 160 ticks — a budget tuned against numbers in another package, with
	// nothing here to say so. It breaks out on the first BlockUpdate, so the size costs
	// nothing on the ordinary path.
	const mineBudget = 400
	for range mineBudget {
		*clientTick++
		conn.in <- mineRequest(pos, *clientTick)
		// Serve owns a different goroutine. Let it install the refresh before the tick;
		// this is coordination with the test double, not the mining clock.
		time.Sleep(time.Millisecond)
		*serverTick++
		sim.Step(*serverTick)
		time.Sleep(time.Millisecond)
		if len(frames.blockUpdates()) > before {
			return
		}
	}
	t.Fatalf("mining %v produced no BlockUpdate in %d ticks", pos, mineBudget)
}

// tickUntilDone steps the simulation while it waits, and fails the test if the
// condition never holds.
//
// The stepping is the point. Since drops landed, what a break gives a player is not
// produced by the break: the yield falls, spends its pickup delay and is collected on
// a later tick, so a wait that only slept would be waiting for something nothing was
// going to do.
//
// **Paced, not spun**, and that is the half worth keeping. A session's outbound queue
// holds outboundQueue frames and a BlockUpdate is broadcast through the *non-blocking*
// seam: a wait that ticks as fast as the scheduler allows fills that queue with
// snapshots, and the next broadcast is then correctly dropped and its chunk forgotten.
// The test would be measuring its own hammering rather than the server. Found the
// expensive way — spinning here failed TestBreakingThenPlacingReturnsTheStackToZero on
// every run at GOMAXPROCS=1, where the collector's goroutine never gets scheduled at
// all. mineUntilBreak paces itself for the same reason.
func tickUntilDone(t *testing.T, sim *game.Sim, serverTick *uint64, what string, done func() bool) {
	t.Helper()

	deadline := time.Now().Add(patience)
	for time.Now().Before(deadline) {
		if done() {
			return
		}
		*serverTick++
		sim.Step(*serverTick)
		// Long enough for the session's writer and the test's reader to drain what this
		// tick produced. Coordination with the test double, not the simulation's clock.
		time.Sleep(time.Millisecond)
	}
	// One last look, so a condition that came true during the final tick is not
	// reported as a timeout.
	if !done() {
		t.Fatalf("timed out waiting for %s", what)
	}
}

// tickQuietly advances the simulation n ticks at the same pace, for a test that needs
// the world to settle rather than to wait for something in particular.
func tickQuietly(sim *game.Sim, serverTick *uint64, n int) {
	for range n {
		*serverTick++
		sim.Step(*serverTick)
		time.Sleep(time.Millisecond)
	}
}

// placeRequest is a client asking to spend one authoritative slot at a voxel.
func placeRequest(pos [3]int32, slot uint8, clientTick uint32) []byte {
	return protocol.EncodeBlockEditRequest(protocol.BlockEditRequest{
		Pos: pos, HasPos: true, Action: vnet.EditActionPlace, Slot: slot, ClientTick: clientTick,
	})
}

func dropOutcome(block world.Block) (game.ItemID, world.Block, bool) {
	switch block {
	case world.Stone:
		return game.ItemStone, world.Stone, true
	case world.Dirt, world.Grass:
		return game.ItemDirt, world.Dirt, true
	case world.Snow:
		return game.ItemSnow, world.Snow, true
	case world.Log:
		return game.ItemLog, world.Log, true
	case world.CoalOre:
		return game.ItemRawCoal, world.Air, true
	case world.IronOre:
		return game.ItemRawIron, world.Air, true
	default:
		return game.ItemNone, world.Air, false
	}
}

// The whole starter loadout, end to end: what the simulation grants on join is what
// crosses the wire, durability vectors included. This is the only test on this side that
// reads those vectors off a real frame rather than out of a Go value.
func TestAJoinReceivesTheStarterLoadout(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)
	_, frames := admit(t, cfg, chunks, sim, peers, 1)

	states := frames.inventoryStates()
	if len(states) != 1 {
		t.Fatalf("the join received %d inventory states, want exactly 1", len(states))
	}
	if len(states[0].Stacks) != int(protocol.InventorySlots) {
		t.Fatalf("the join received %d inventory slots, want %d", len(states[0].Stacks), protocol.InventorySlots)
	}

	want := protocol.InventoryStack{
		ItemID:        uint16(game.ItemRustySword),
		Count:         1,
		Durability:    game.RustySwordMaxDurability,
		MaxDurability: game.RustySwordMaxDurability,
	}
	if got := states[0].Stacks[0]; got != want {
		t.Errorf("hotbar slot 0 is %+v, want the starter sword %+v", got, want)
	}
	for slot, stack := range states[0].Stacks[1:] {
		if stack != (protocol.InventoryStack{}) {
			t.Errorf("joined inventory slot %d is %+v, want empty (0, 0)", slot+1, stack)
		}
	}
}

func TestDirectBreakIsAProtocolErrorEvenWithoutAPosition(t *testing.T) {
	t.Parallel()

	for _, test := range []struct {
		name   string
		hasPos bool
	}{{name: "with position", hasPos: true}, {name: "without position"}} {
		t.Run(test.name, func(t *testing.T) {
			cfg := editConfig()
			chunks, sim, peers := editDeps(t, cfg)
			conn := newFakeConn()
			collect(t, conn)
			served := make(chan error, 1)
			go func() {
				served <- session.Serve(context.Background(), conn, cfg, noTimeouts(), chunks, sim, peers, ephemeralIdentities(), 1, discard())
			}()

			conn.in <- hello(1)
			createCharacter(conn, "Eivor")
			waitUntil(t, "the player to join before the protocol violation", func() bool { return sim.Count() == 1 })
			request := protocol.BlockEditRequest{Action: vnet.EditActionBreak, ClientTick: 1}
			if test.hasPos {
				request.Pos, request.HasPos = surfaceUnderSpawn(cfg.WorldSeed), true
			}
			conn.in <- protocol.EncodeBlockEditRequest(request)

			select {
			case err := <-served:
				if !errors.Is(err, protocol.ErrMalformed) {
					t.Fatalf("Serve returned %v, want a protocol error", err)
				}
			case <-time.After(patience):
				t.Fatal("Serve kept a session alive after direct Break")
			}
		})
	}
}

func TestMineRequestForAnUndeliveredChunkIsSilent(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	cfg.ViewDistance = 0
	chunks, sim, peers := editDeps(t, cfg)
	conn, frames := admit(t, cfg, chunks, sim, peers, 1)

	// One voxel west of the origin is in chunk -1 but still well inside EditReach
	// from a player centred at x=0.5. Radius zero delivered only chunk 0.
	target := [3]int32{-1, int32(cfg.Spawn[1]), 0}
	if err := chunks.Apply(context.Background(), -1, int64(target[1]), 0, world.Stone, nil); err != nil {
		t.Fatalf("prepare target: %v", err)
	}
	conn.in <- mineRequest(target, 1)
	time.Sleep(time.Millisecond)
	for tick := uint64(1); tick <= 5; tick++ {
		sim.Step(tick)
	}
	time.Sleep(time.Millisecond)

	if got := frames.mineProgress(); len(got) != 0 {
		t.Errorf("undelivered target produced mining progress %+v", got)
	}
	if got := frames.blockUpdates(); len(got) != 0 {
		t.Errorf("undelivered target produced block updates %+v", got)
	}
	if block, err := chunks.BlockAt(context.Background(), -1, int64(target[1]), 0); err != nil || block != world.Stone {
		t.Errorf("undelivered target holds block %d (err = %v), want Stone", block, err)
	}
	if sim.Count() != 1 {
		t.Error("a refused MineRequest ended the admitted session")
	}
}

func TestWardedEditAndMineRefusalsReachTheWire(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)
	conn, frames := admit(t, cfg, chunks, sim, peers, 1)
	ground := surfaceUnderSpawn(cfg.WorldSeed)
	if err := sim.RestoreStructures([]game.Structure{{
		Kind: vnet.StructureKindRunestone, Anchor: ground, Facing: vnet.FacingNorth,
		Owner: identity.PlayerID{99},
	}}); err != nil {
		t.Fatalf("RestoreStructures: %v", err)
	}

	placed := ground
	placed[1]++
	conn.in <- placeRequest(placed, 0, 1)
	conn.in <- mineRequest(ground, 1)
	waitUntil(t, "both ward refusals", func() bool { return len(frames.actionRefusals()) == 2 })

	want := []protocol.ActionRefused{
		{Action: vnet.RefusedActionEdit, Reason: vnet.RefusalReasonWarded, Anchor: placed, HasAnchor: true},
		{Action: vnet.RefusedActionMine, Reason: vnet.RefusalReasonWarded, Anchor: ground, HasAnchor: true},
	}
	got := frames.actionRefusals()
	for i := range want {
		if got[i] != want[i] {
			t.Errorf("refusal %d = %+v, want %+v", i, got[i], want[i])
		}
	}
}

// The whole path, over two connections: a client asks, the server decides, and every session
// holding the chunk is told — the one that asked included, by the same rule as everyone else
// rather than as a special case.
func TestAnAcceptedEditReachesEverySessionHoldingTheChunk(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)
	ctx := context.Background()

	target := surfaceUnderSpawn(cfg.WorldSeed)
	coord := world.ChunkOf(int64(target[0]), int64(target[1]), int64(target[2]))

	before, err := chunks.BlockAt(ctx, int64(target[0]), int64(target[1]), int64(target[2]))
	if err != nil {
		t.Fatalf("BlockAt: %v", err)
	}
	if before == world.Air {
		t.Fatal("the surface voxel under the spawn is already air; there would be nothing to break")
	}
	dropped, _, hasDrop := dropOutcome(before)
	if !hasDrop {
		t.Fatalf("surface block %d has no inventory yield; test needs a yielding block", before)
	}

	diggerConn, digger := admit(t, cfg, chunks, sim, peers, 1)
	_, bystander := admit(t, cfg, chunks, sim, peers, 2)

	var clientTick uint32
	var serverTick uint64
	mineUntilBreak(t, diggerConn, digger, sim, target, &clientTick, &serverTick)

	for name, frames := range map[string]*collector{"the session that asked": digger, "a session that only watched": bystander} {
		waitUntil(t, "a block update to reach "+name, func() bool { return len(frames.blockUpdates()) > 0 })

		updates := frames.blockUpdates()
		if len(updates) != 1 {
			t.Errorf("%s received %d block updates, want exactly 1", name, len(updates))
		}
		if updates[0].Pos != target {
			t.Errorf("%s was told about voxel %v, want %v", name, updates[0].Pos, target)
		}
		if updates[0].BlockID != uint16(world.Air) {
			t.Errorf("%s was told block %d stands there, want Air for a break", name, updates[0].BlockID)
		}
	}

	// The server's own copy changed too, and the chunk both sessions hold is the one that
	// changed — the broadcast describes the world rather than announcing an intention.
	if got, err := chunks.BlockAt(ctx, int64(target[0]), int64(target[1]), int64(target[2])); err != nil || got != world.Air {
		t.Errorf("the voxel holds %d (err = %v) after an accepted break, want Air", got, err)
	}
	if digger.chunkCount(coord) == 0 {
		t.Fatalf("the digger was never sent chunk %+v, so this test asserted nothing", coord)
	}

	// The yield is on the ground now, so the digger's pack changes only once the tick
	// has let the drop fall and walked the digger over it. Both sessions spawned at the
	// same point, and the drop goes to the lowest entity id that reaches it.
	tickUntilDone(t, sim, &serverTick, "the digger's inventory change", func() bool {
		return digger.inventoryCount(uint16(dropped)) == 1
	})
	if states := bystander.inventoryStates(); len(states) != 1 {
		t.Errorf("the bystander received %d inventory states, want only its join state", len(states))
	}
	if progress := bystander.mineProgress(); len(progress) != 0 {
		t.Errorf("the bystander received mining progress %+v; only the miner may receive it", progress)
	}
	if progress := digger.mineProgress(); len(progress) == 0 {
		t.Error("the mining session received no positive progress before completion")
	} else {
		for _, frame := range progress {
			if frame.Pos != target || frame.Progress == 0 {
				t.Errorf("miner's progress contains %+v, want positive frames for %v", frame, target)
			}
		}
	}
}

func TestBreakingThenPlacingReturnsTheStackToZero(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)
	conn, frames := admit(t, cfg, chunks, sim, peers, 1)
	target := surfaceUnderSpawn(cfg.WorldSeed)

	broken, err := chunks.BlockAt(context.Background(), int64(target[0]), int64(target[1]), int64(target[2]))
	if err != nil || broken == world.Air {
		t.Fatalf("the target starts as block %d (err = %v), want something to carry", broken, err)
	}
	dropped, placed, hasDrop := dropOutcome(broken)
	if !hasDrop || placed == world.Air {
		t.Fatalf("block %d does not yield a placeable item", broken)
	}

	var clientTick uint32
	var serverTick uint64
	mineUntilBreak(t, conn, frames, sim, target, &clientTick, &serverTick)
	tickUntilDone(t, sim, &serverTick, "the dropped block to be collected", func() bool {
		return frames.inventoryCount(uint16(dropped)) == 1
	})

	// Breaking the block underfoot drops the player into the hole it made, so the mined
	// voxel is now inside their body and a placement into it is refused for that reason
	// rather than for anything this test is about. Roof them over instead: two blocks
	// above the feet is clear of a body 1.8 tall and well inside EditReach.
	tickQuietly(sim, &serverTick, 40)
	feet, standing := frames.position(1)
	if !standing {
		t.Fatal("the session was never told where its own player is")
	}
	above := [3]int32{
		int32(math.Floor(float64(feet[0]))),
		int32(math.Floor(float64(feet[1]))) + 2,
		int32(math.Floor(float64(feet[2]))),
	}

	// The slot the yield actually landed in, not slot 0: that one holds the starter
	// sword, which places no block, so a request naming it would be refused for a reason
	// this test is not about.
	yieldSlot := frames.slotOf(uint16(dropped))
	if yieldSlot < 0 {
		t.Fatal("the collected yield is in no slot at all")
	}

	conn.in <- placeRequest(above, uint8(yieldSlot), 2)
	waitUntil(t, "the placed block to leave the inventory", func() bool {
		return len(frames.inventoryStates()) >= 3 && frames.inventoryCount(uint16(dropped)) == 0
	})
	waitUntil(t, "the place BlockUpdate", func() bool { return len(frames.blockUpdates()) >= 2 })

	if got, err := chunks.BlockAt(context.Background(), int64(above[0]), int64(above[1]), int64(above[2])); err != nil || got != placed {
		t.Errorf("the voxel holds block %d (err = %v), want the placed block %d", got, err, placed)
	}
}

func TestBreakingIronYieldsOneStateAndRawIronCannotBePlaced(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)
	conn, frames := admit(t, cfg, chunks, sim, peers, 1)
	target := surfaceUnderSpawn(cfg.WorldSeed)
	if err := chunks.Apply(context.Background(), int64(target[0]), int64(target[1]), int64(target[2]), world.IronOre, nil); err != nil {
		t.Fatalf("prepare IronOre: %v", err)
	}

	joinStates := len(frames.inventoryStates())
	var clientTick uint32
	var serverTick uint64
	mineUntilBreak(t, conn, frames, sim, target, &clientTick, &serverTick)
	tickUntilDone(t, sim, &serverTick, "the RawIron drop to be collected", func() bool {
		return frames.inventoryCount(uint16(game.ItemRawIron)) == 1
	})
	if got := len(frames.inventoryStates()); got != joinStates+1 {
		t.Fatalf("breaking IronOre emitted %d InventoryStates, want exactly one", got-joinStates)
	}
	breakUpdates := len(frames.blockUpdates())

	// The accepted move is a barrier behind both the refused place and a refused
	// same-slot move on the session's one read goroutine. Once its state arrives,
	// both requests have definitely been processed without adding a timing
	// assumption to the assertion, and exactly one of the three may emit state.
	// Both requests name the slot the RawIron is in, which is not slot 0 — that holds
	// the starter sword. Naming slot 0 would still produce a refused placement, and for
	// the wrong reason: this test is about RawIron placing no block, not about a sword.
	ironSlot := frames.slotOf(uint16(game.ItemRawIron))
	if ironSlot < 0 {
		t.Fatal("the collected RawIron is in no slot at all")
	}
	freeSlot := frames.emptySlot()
	if freeSlot < 0 {
		t.Fatal("there is nowhere to move the RawIron to")
	}

	conn.in <- placeRequest(target, uint8(ironSlot), 2)
	conn.in <- protocol.EncodeInventoryMoveRequest(protocol.InventoryMoveRequest{From: uint8(ironSlot), To: uint8(ironSlot), Count: 1})
	conn.in <- protocol.EncodeInventoryMoveRequest(protocol.InventoryMoveRequest{From: uint8(ironSlot), To: uint8(freeSlot), Count: 1})
	waitUntil(t, "the move after the refused RawIron placement", func() bool {
		states := frames.inventoryStates()
		return len(states) == joinStates+2 && states[len(states)-1].Stacks[freeSlot] == (protocol.InventoryStack{ItemID: uint16(game.ItemRawIron), Count: 1})
	})

	if got := len(frames.blockUpdates()); got != breakUpdates {
		t.Errorf("RawIron placement emitted a BlockUpdate: got %d updates, want %d", got, breakUpdates)
	}
	if got, err := chunks.BlockAt(context.Background(), int64(target[0]), int64(target[1]), int64(target[2])); err != nil || got != world.Air {
		t.Errorf("RawIron placement left block %d (err = %v), want Air", got, err)
	}
}

func TestAPlaceWithAnEmptyStackIsSilentAndChangesNothing(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)
	conn, frames := admit(t, cfg, chunks, sim, peers, 1)
	surface := surfaceUnderSpawn(cfg.WorldSeed)
	empty := [3]int32{surface[0], surface[1] + 1, surface[2]}

	conn.in <- placeRequest(empty, 0, 1)
	// A legal break after it proves the first request was processed rather than merely
	// still waiting in the read loop.
	clientTick := uint32(1)
	var serverTick uint64
	mineUntilBreak(t, conn, frames, sim, surface, &clientTick, &serverTick)
	tickUntilDone(t, sim, &serverTick, "the legal break after the unpaid place", func() bool {
		return len(frames.blockUpdates()) == 1 && len(frames.inventoryStates()) == 2
	})

	updates := frames.blockUpdates()
	if updates[0].Pos != surface {
		t.Errorf("the only update describes %v, want the legal break at %v", updates[0].Pos, surface)
	}
	if got, err := chunks.BlockAt(context.Background(), int64(empty[0]), int64(empty[1]), int64(empty[2])); err != nil || got != world.Air {
		t.Errorf("the unpaid target holds block %d (err = %v), want Air", got, err)
	}
}

// A refused edit produces no BlockUpdate, no error payload and no acknowledgement of any
// kind — the absence is the answer — and it does not end the connection, because the frame
// was well formed and only a value was wrong.
//
// The legal edit that follows is how the test tells "refused" apart from "not processed
// yet": exactly one update arrives, and it is the second request's.
func TestARefusedEditIsSilentAndTheSessionSurvivesIt(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)

	conn, frames := admit(t, cfg, chunks, sim, peers, 1)
	target := surfaceUnderSpawn(cfg.WorldSeed)

	// Straight down the spawn column, 32 blocks under the surface: deep enough to be stone
	// whatever the seed does, and far enough that nothing but the reach refuses it.
	unreachable := [3]int32{target[0], target[1] - 32, target[2]}
	conn.in <- mineRequest(unreachable, 1)
	time.Sleep(time.Millisecond)
	clientTick := uint32(1)
	var serverTick uint64
	mineUntilBreak(t, conn, frames, sim, target, &clientTick, &serverTick)

	waitUntil(t, "the legal edit to be broadcast", func() bool { return len(frames.blockUpdates()) > 0 })

	updates := frames.blockUpdates()
	if len(updates) != 1 {
		t.Fatalf("the session received %d block updates, want exactly 1 — the refused edit produced one", len(updates))
	}
	if updates[0].Pos != target {
		t.Errorf("the update describes voxel %v, want the legal target %v", updates[0].Pos, target)
	}
	if got, err := chunks.BlockAt(context.Background(), int64(unreachable[0]), int64(unreachable[1]), int64(unreachable[2])); err != nil || got == world.Air {
		t.Errorf("the unreachable voxel holds %d (err = %v); a refused edit changed the world", got, err)
	}
}

// Two sessions complete mining in the same chunk while Step reads it for collision.
// Under `-race`, the session workers write two voxels while the tick owns its resident
// Terrain view; both outcomes must survive the cache's atomic composition path.
func TestTwoSessionsMineOneChunkWhileTheTickLoopReadsIt(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)
	ctx := context.Background()

	surface := surfaceUnderSpawn(cfg.WorldSeed)
	// Separate voxels, same layer and chunk, both forced to Stone so their completions
	// become ready on the same authoritative tick.
	targets := [][3]int32{{-1, surface[1] - 1, 0}, {1, surface[1] - 1, 0}}
	for _, target := range targets {
		if err := chunks.Apply(ctx, int64(target[0]), int64(target[1]), int64(target[2]), world.Stone, nil); err != nil {
			t.Fatalf("prepare Stone at %v: %v", target, err)
		}
	}

	first, firstFrames := admit(t, cfg, chunks, sim, peers, 1)
	second, secondFrames := admit(t, cfg, chunks, sim, peers, 2)

	// Same reasoning as mineBudget above: bounded generously against the hardness table
	// rather than against a number that happened to fit before it was retuned. The loop
	// leaves as soon as both sessions have their block updates.
	for tick := uint32(1); tick <= 400; tick++ {
		first.in <- mineRequest(targets[0], tick)
		second.in <- mineRequest(targets[1], tick)
		time.Sleep(time.Millisecond)
		sim.Step(uint64(tick))
		time.Sleep(time.Millisecond)
		if len(firstFrames.blockUpdates()) >= 2 && len(secondFrames.blockUpdates()) >= 2 {
			break
		}
	}
	for _, target := range targets {
		if got, err := chunks.BlockAt(ctx, int64(target[0]), int64(target[1]), int64(target[2])); err != nil || got != world.Air {
			t.Errorf("target %v holds %d (err = %v), want Air", target, got, err)
		}
	}
	// Both yields are lying in the pocket each break opened — a block under the surface,
	// out of reach of a player standing on top of it — so what this asserts is that two
	// concurrent completions each produced an entity, and that both sessions can see
	// both of them.
	serverTick := uint64(60)
	for name, frames := range map[string]*collector{"the first session": firstFrames, "the second session": secondFrames} {
		tickUntilDone(t, sim, &serverTick, "both drops to reach "+name, func() bool {
			return len(frames.visibleDrops()) == len(targets)
		})

		total := 0
		for _, drop := range frames.visibleDrops() {
			total += int(drop.Count)
		}
		if total != len(targets) {
			t.Errorf("%s sees %d dropped items in %d entities, want %d",
				name, total, len(frames.visibleDrops()), len(targets))
		}
		if got := frames.carriedResources(); got != 0 {
			t.Errorf("%s carries %d items; both yields are out of reach on the ground", name, got)
		}
	}
}

// A swing is a payload this session accepts, and a refused one is silence rather than a
// closed connection.
//
// Direction is a protocol rule: before this issue, tag 15 fell through to the dispatcher's
// default and ended the session as "client sent AttackRequest". Both halves are asserted
// here — that an attack no longer does that, and that a value the simulation refuses does
// not either — and `admit`'s cleanup is what fails the test if the session ends with an
// error at all.
func TestAnAttackIsAcceptedAndARefusedOneIsSilence(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)
	conn, frames := admit(t, cfg, chunks, sim, peers, 1)

	joinStates := len(frames.inventoryStates())
	joinUpdates := len(frames.blockUpdates())

	// The starter blade is in slot 0. Nothing is in reach, so this lands on nothing —
	// which is the point: a swing that hits nothing must still be an ordinary message.
	conn.in <- protocol.EncodeAttackRequest(protocol.AttackRequest{Slot: 0, ClientTick: 1})
	// A slot outside the inventory, which the simulation refuses rather than the decoder.
	conn.in <- protocol.EncodeAttackRequest(protocol.AttackRequest{Slot: 255, ClientTick: 2})
	// A stale tick, refused for a third reason.
	conn.in <- protocol.EncodeAttackRequest(protocol.AttackRequest{Slot: 0, ClientTick: 1})

	// A barrier behind all three: the move is processed on the same read goroutine, so
	// its state arriving proves every frame before it was handled without ending the
	// session. An accepted move is the only one of the four that replies at all.
	freeSlot := frames.emptySlot()
	if freeSlot < 0 {
		t.Fatal("the starter inventory has no free slot to move into")
	}
	conn.in <- protocol.EncodeInventoryMoveRequest(protocol.InventoryMoveRequest{
		From: 0, To: uint8(freeSlot), Count: 1,
	})
	waitUntil(t, "the inventory move behind the attacks", func() bool {
		return len(frames.inventoryStates()) == joinStates+1
	})

	// And no attack produced a reply of its own: the contract has no acknowledgement.
	if got := len(frames.inventoryStates()); got != joinStates+1 {
		t.Errorf("the attacks produced %d extra inventory states, want none", got-joinStates-1)
	}
	if got := len(frames.blockUpdates()); got != joinUpdates {
		t.Errorf("the attacks produced %d block updates", got-joinUpdates)
	}
}

// A refused placement is answered, a refused removal is still silence, and the session
// survives both.
//
// **The asymmetry is the design and not an oversight.** A placement is a thing the player
// tried to do at a cell they are looking at, and telling them why the ground said no costs
// nothing they did not already know. A removal names an id the server minted: answering it
// would let a client tell "no such structure" from "not yours" from "too far away", which
// is a way to map somebody else's camp by asking. `RemoveStructureRequest` says so, and
// `RefusedAction` deliberately has no member for it.
//
// **"The session survives it" is what changed at this layer in legacy PR 131**: both tags landed in
// handlePostHandshake's default case before then, which ends the connection as a protocol
// violation. The refusals themselves are the simulation's and are covered where they are
// decided; what is covered here is that the answer reaches the wire and names the anchor
// the request did.
//
// The legal break afterwards is how the test tells "refused" apart from "not processed
// yet": the read loop is sequential, so an update for the third request is proof the first
// two were answered.
func TestARefusedPlacementIsAnsweredARefusedRemovalIsNotAndTheSessionSurvivesBoth(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)
	conn, frames := admit(t, cfg, chunks, sim, peers, 1)
	surface := surfaceUnderSpawn(cfg.WorldSeed)

	// Nothing in the starter pack plants a structure and nothing stands to be taken back,
	// so both requests are ordinary refusals against well-formed frames.
	conn.in <- protocol.EncodePlaceStructureRequest(protocol.PlaceStructureRequest{
		Slot: 0, Anchor: surface, HasAnchor: true, Facing: vnet.FacingNorth, ClientTick: 1,
	})
	conn.in <- protocol.EncodeRemoveStructureRequest(protocol.RemoveStructureRequest{
		StructureID: 1234, ClientTick: 2,
	})

	clientTick := uint32(1)
	var serverTick uint64
	mineUntilBreak(t, conn, frames, sim, surface, &clientTick, &serverTick)
	waitUntil(t, "the legal break to be broadcast", func() bool { return len(frames.blockUpdates()) > 0 })

	if got := len(frames.blockUpdates()); got != 1 {
		t.Errorf("the session received %d block updates, want only the legal break's", got)
	}
	if got := sim.StructureCount(); got != 0 {
		t.Errorf("%d structures stand in the world, want none — both requests were refused", got)
	}

	// Exactly one refusal for two refused requests: the placement's. The break behind them
	// is the barrier that makes "one" a count rather than a race.
	refusals := frames.actionRefusals()
	if len(refusals) != 1 {
		t.Fatalf("the session received %d refusals, want only the placement's: %+v", len(refusals), refusals)
	}
	refusal := refusals[0]
	if refusal.Action != vnet.RefusedActionPlaceStructure {
		t.Errorf("action = %s, want PlaceStructure", refusal.Action)
	}
	// The starter blade is in slot 0 and plants nothing, which is the world answering
	// rather than the frame being malformed.
	if refusal.Reason != vnet.RefusalReasonSlotUnusable {
		t.Errorf("reason = %s, want SlotUnusable", refusal.Reason)
	}
	if !refusal.HasAnchor || refusal.Anchor != surface {
		t.Errorf("anchor = %v (present %t), want the %v the request named", refusal.Anchor, refusal.HasAnchor, surface)
	}
}

// A craft the simulation refuses is silence, and the session survives it. The starter pack
// holds one blade and nothing else, so every recipe is short of everything.
//
// The routing is what this covers: tag 16 landed in handlePostHandshake's default case
// before this issue, which ends the connection as a protocol violation. The refusals
// themselves are covered where they are decided.
func TestARefusedCraftIsSilentAndTheSessionSurvivesIt(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)
	conn, frames := admit(t, cfg, chunks, sim, peers, 1)
	surface := surfaceUnderSpawn(cfg.WorldSeed)

	conn.in <- protocol.EncodeCraftRequest(protocol.CraftRequest{Recipe: vnet.RecipeIDTent, ClientTick: 1})
	// An unknown recipe too, which is the absent-field zero a client sends by omitting the
	// field entirely.
	conn.in <- protocol.EncodeCraftRequest(protocol.CraftRequest{Recipe: vnet.RecipeIDUnknown, ClientTick: 2})

	// The legal break afterwards is how the test tells "refused" apart from "not processed
	// yet": the read loop is sequential, so an update for the third request is proof the
	// first two were answered.
	clientTick := uint32(1)
	var serverTick uint64
	mineUntilBreak(t, conn, frames, sim, surface, &clientTick, &serverTick)
	waitUntil(t, "the legal break to be broadcast", func() bool { return len(frames.blockUpdates()) > 0 })

	if got := len(frames.blockUpdates()); got != 1 {
		t.Errorf("the session received %d block updates, want only the legal break's", got)
	}
	// Counting inventory frames would be counting the break's own drop as well, so the
	// claim is about contents instead: nothing in this test can produce a tent, so a tent
	// in any state the session was ever sent is a refused craft that was not refused.
	for _, state := range frames.inventoryStates() {
		for slot, stack := range state.Stacks {
			if stack.ItemID == uint16(game.ItemTent) {
				t.Fatalf("slot %d holds a tent; a refused craft produced one", slot)
			}
		}
	}
}

// A repair the simulation refuses is silence, and the session survives it. The starter
// pack holds one blade at full durability and no sharpening stone, so there is nothing to
// spend and nothing to mend.
//
// **The routing is the whole of what changed at this layer.** Tag 17 landed in
// handlePostHandshake's default case before this issue, which ends the connection as a
// protocol violation — so a client that spoke V4 to a server that had only the contract
// was disconnected for asking. The refusals themselves are the simulation's and are
// covered where they are decided, in game/repair_test.go.
//
// The out-of-range slot pair is the row worth having here rather than there: the decoder
// carries those indexes verbatim by design, so this is the test that says a client cannot
// end its own session by naming a slot that does not exist.
func TestARefusedRepairIsSilentAndTheSessionSurvivesIt(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)
	conn, frames := admit(t, cfg, chunks, sim, peers, 1)
	surface := surfaceUnderSpawn(cfg.WorldSeed)

	// No kit in slot 1, and the blade in slot 0 is at full durability anyway.
	conn.in <- protocol.EncodeRepairRequest(protocol.RepairRequest{KitSlot: 1, TargetSlot: 0, ClientTick: 1})
	// Slots past the end of the pack, which the decoder carries rather than refuses.
	conn.in <- protocol.EncodeRepairRequest(protocol.RepairRequest{KitSlot: 200, TargetSlot: 201, ClientTick: 2})
	// One slot named twice, which is the other shape nothing else bounds.
	conn.in <- protocol.EncodeRepairRequest(protocol.RepairRequest{KitSlot: 0, TargetSlot: 0, ClientTick: 3})

	// The legal break afterwards is how the test tells "refused" apart from "not processed
	// yet": the read loop is sequential, so an update for the third request is proof the
	// first three were answered.
	clientTick := uint32(1)
	var serverTick uint64
	mineUntilBreak(t, conn, frames, sim, surface, &clientTick, &serverTick)
	waitUntil(t, "the legal break to be broadcast", func() bool { return len(frames.blockUpdates()) > 0 })

	if got := len(frames.blockUpdates()); got != 1 {
		t.Errorf("the session received %d block updates, want only the legal break's", got)
	}
	// Counting inventory frames would be counting the break's own drop as well, so the
	// claim is about contents instead: nothing here can raise a durability, so a blade
	// above the maximum it started at is a refused repair that was not refused.
	for _, state := range frames.inventoryStates() {
		for slot, stack := range state.Stacks {
			if stack.MaxDurability != 0 && stack.Durability != stack.MaxDurability {
				t.Fatalf("slot %d is at %d of %d durability; nothing in this test wears or mends a blade",
					slot, stack.Durability, stack.MaxDurability)
			}
		}
	}
}

// A consume the simulation refuses is silence, and the session survives it. The
// starter pack holds only a blade, so slot 0 is not food, slot 1 is empty, and a uint16
// slot outside the pack exercises the decoder's deliberate carry-through rule.
//
// The routing is what this layer owns: before payload tag 28 had a handler, the default
// branch treated a valid consume intent as a protocol violation and ended the session.
func TestARefusedConsumeIsSilentAndTheSessionSurvivesIt(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)
	conn, frames := admit(t, cfg, chunks, sim, peers, 1)
	surface := surfaceUnderSpawn(cfg.WorldSeed)

	conn.in <- protocol.EncodeConsumeRequest(protocol.ConsumeRequest{Slot: 0, ClientTick: 1})
	conn.in <- protocol.EncodeConsumeRequest(protocol.ConsumeRequest{Slot: 1, ClientTick: 2})
	conn.in <- protocol.EncodeConsumeRequest(protocol.ConsumeRequest{Slot: 65_535, ClientTick: 3})

	// The legal break afterwards proves the sequential read loop processed every
	// refusal and kept the connection alive.
	clientTick := uint32(3)
	var serverTick uint64
	mineUntilBreak(t, conn, frames, sim, surface, &clientTick, &serverTick)
	waitUntil(t, "the legal break to be broadcast", func() bool { return len(frames.blockUpdates()) > 0 })

	if got := len(frames.blockUpdates()); got != 1 {
		t.Errorf("the session received %d block updates, want only the legal break's", got)
	}
}

// A durable drop the simulation accepts empties the named slot and puts one entity in the
// world, and the session answers with the complete inventory that leaves behind.
//
// **The routing is what this layer owns**, exactly as it is for a repair: tag 25 reached
// handlePostHandshake's default case before this issue, which ends the connection as a
// protocol violation. What a drop is *allowed* to be is decided in game/drop_request_test.go.
//
// The starter blade is deliberate: routing a worn-capable slot through the session is the
// inverse of the refusal this path enforced before drops could carry durability.
func TestADurableDropEmptiesItsSlotAndPutsOneItemInTheWorld(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)
	conn, frames := admit(t, cfg, chunks, sim, peers, 1)
	const slot = 0
	states := len(frames.inventoryStates())

	conn.in <- protocol.EncodeDropItemRequest(protocol.DropItemRequest{Slot: slot, ClientTick: 1})
	waitUntil(t, "the inventory the drop left behind", func() bool {
		all := frames.inventoryStates()
		return len(all) > states && all[len(all)-1].Stacks[slot] == (protocol.InventoryStack{})
	})

	// Read before any further tick, because a drop lands at the player's feet and the
	// ordinary pickup rule collects it back once its delay is spent.
	if got := sim.DropCount(); got != 1 {
		t.Errorf("%d drops are lying in the world after one was put down, want one", got)
	}
}

// A drop the simulation refuses is silence, and the session survives it.
//
// Two refusals: an empty slot and a slot past the end of the pack, which the decoder carries
// verbatim by design. A durable starter blade is accepted by the test above.
func TestARefusedDropIsSilentAndTheSessionSurvivesIt(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)
	conn, frames := admit(t, cfg, chunks, sim, peers, 1)
	surface := surfaceUnderSpawn(cfg.WorldSeed)

	empty := frames.emptySlot()
	if empty < 0 {
		t.Fatal("the starter pack has no empty slot to name")
	}

	conn.in <- protocol.EncodeDropItemRequest(protocol.DropItemRequest{Slot: uint8(empty), ClientTick: 1})
	conn.in <- protocol.EncodeDropItemRequest(protocol.DropItemRequest{Slot: 200, ClientTick: 2})

	// The legal break afterwards is how the test tells "refused" apart from "not processed
	// yet": the read loop is sequential, so an update for the fourth request is proof the
	// first two were answered.
	clientTick := uint32(2)
	var serverTick uint64
	mineUntilBreak(t, conn, frames, sim, surface, &clientTick, &serverTick)
	waitUntil(t, "the legal break to be broadcast", func() bool { return len(frames.blockUpdates()) > 0 })

	if got := len(frames.blockUpdates()); got != 1 {
		t.Errorf("the session received %d block updates, want only the legal break's", got)
	}
	// The break's own yield is one drop, and the two refusals must have added none.
	if got := sim.DropCount(); got > 1 {
		t.Errorf("%d drops are lying in the world, want at most the mined yield's one", got)
	}
	// And the blade never left slot 0, in any state the session was ever sent.
	for _, state := range frames.inventoryStates() {
		if got := state.Stacks[0].ItemID; got != uint16(game.ItemRustySword) {
			t.Fatalf("slot 0 holds item %d; a refused drop emptied it", got)
		}
	}
}
