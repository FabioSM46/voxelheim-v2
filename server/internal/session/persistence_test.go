package session_test

import (
	"context"
	"math"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// TestALifeSurvivesADisconnect is the issue end to end at this layer: a player walks
// somewhere, rearranges their pack, disconnects, and comes back to both.
//
// It runs both sessions against **one** simulation and one store, which is the case a
// reconnect actually is — the process did not restart, only the connection ended. The
// restart is cmd/voxelheimd's test, because that is where a process exists to restart.
//
// The two assertions are the two halves of "coming back". The welcome carries the
// position the player is placed at rather than the world spawn, so the client draws
// them where they are instead of teleporting them on the first snapshot; and the first
// InventoryState is the pack they left with, through the same "whole inventory before
// streaming" path a new player's starter pack arrives on.
func TestALifeSurvivesADisconnect(t *testing.T) {
	t.Parallel()

	store, err := persist.OpenStore(t.TempDir())
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	identities := identitiesOver(store)
	account := testAccount(21)

	// The derived spawn and a cache big enough to hold the walk, exactly as the
	// end-to-end movement test builds them: the hardcoded spawn in testConfig is fine
	// for admission tests and is not guaranteed to be standing room.
	cfg := serveConfig()
	cfg.Spawn = world.SpawnAt(cfg.WorldSeed)
	chunks := world.NewCache(cfg.WorldSeed, 4, 512)
	peers := session.NewRegistry(session.DefaultConcurrentSessions)
	sim, err := game.NewSim(cfg.TickRate, cfg.ViewDistance, cfg.WorldSeed, game.NewCacheTerrain(chunks), chunks, peers.NextID, discard())
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}
	generateAround(t, chunks, cfg.Spawn, 2)

	tick := uint64(0)
	step := func() {
		tick++
		sim.Step(tick)
	}

	// ---- the first life -------------------------------------------------------

	first := newFakeConn()
	firstDone := make(chan error, 1)
	go func() {
		firstDone <- session.Serve(context.Background(), first, cfg, noTimeouts(), chunks, sim, peers, identities, 1, discard())
	}()

	first.in <- protocol.EncodeClientHelloWithTicket(vnet.ProtocolVersionCurrent, "Eivor", testTicket(account))
	chooseCharacter(t, first, "Eivor")
	if got := vnet.GetRootAsEnvelope(nextFrame(t, first), 0).PayloadType(); got != vnet.PayloadServerWelcome {
		t.Fatalf("the first session got %s, want a welcome", got)
	}
	sink := collect(t, first)

	// A pack this player could not have joined with: the starter blade somewhere other
	// than the slot every new player finds it in. An assertion against slot 0 would
	// pass for a session that restored nothing at all.
	const movedTo = 4
	sword := uint16(game.ItemRustySword)
	first.in <- protocol.EncodeInventoryMoveRequest(protocol.InventoryMoveRequest{From: 0, To: movedTo, Count: 1})
	waitUntil(t, "the blade to move to slot 4", func() bool { return sink.slotOf(sword) == movedTo })

	// Settle onto the ground before walking, so what follows is a walk rather than the
	// fall every join begins with.
	for range 60 {
		step()
	}
	waitUntil(t, "the first snapshot", func() bool { _, ok := sink.position(1); return ok })
	start, _ := sink.position(1)

	// And somewhere this player could not have joined at. One input per tick, exactly
	// as the client sends them: the simulation decays an intent its client has stopped
	// refreshing, so a walk is a walk only while it is being asked for.
	//
	// Bounded in ticks derived from the simulation rather than by a clock, for the
	// reason the movement test spells out: the position being steered by arrives over
	// the wire and lags, so a wall-clock loop walks much further than it means to.
	const walkBlocks = 2.0
	walkTicks := 2 * int(math.Ceil(walkBlocks/(game.WalkSpeed/float64(cfg.TickRate))))
	for clientTick := uint32(1); clientTick <= uint32(walkTicks); clientTick++ {
		first.in <- protocol.EncodePlayerInput(protocol.PlayerInput{ClientTick: clientTick, MoveZ: 1})
		step()
	}
	if !tickUntil(step, func() bool {
		pos, ok := sink.position(1)
		return ok && float64(start[2]-pos[2]) >= walkBlocks
	}) {
		t.Fatal("the player never walked away from the spawn")
	}

	if err := first.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	select {
	case err := <-firstDone:
		if err != nil {
			t.Fatalf("the first session returned %v", err)
		}
	case <-time.After(patience):
		t.Fatal("the first session did not return")
	}

	// What the teardown wrote, read back through the store rather than assumed. Every
	// assertion below compares against this, so the test cannot pass by agreeing with
	// its own guess about where the player ended up.
	saved, found, err := store.Load(onlyCharacter(t, store, account).ID)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !found {
		t.Fatal("the session left no record behind")
	}
	if saved.Name != "Eivor" {
		t.Errorf("the record names %q, want the character the session created", saved.Name)
	}
	if saved.Health == 0 {
		t.Error("the record holds no health; a record always describes a living player")
	}
	if saved.Slots[movedTo].ItemID != sword {
		t.Fatalf("the record holds %+v in slot %d, want the blade", saved.Slots[movedTo], movedTo)
	}

	// ---- the same life, one connection later ----------------------------------

	second := newFakeConn()
	secondDone := make(chan error, 1)
	go func() {
		secondDone <- session.Serve(context.Background(), second, cfg, noTimeouts(), chunks, sim, peers, identities, 2, discard())
	}()
	t.Cleanup(func() {
		_ = second.Close()
		<-secondDone
	})

	// A ticket this server has never seen, naming the account it already knows. Nothing
	// the first session handed the client comes back here, because nothing was handed.
	second.in <- protocol.EncodeClientHelloWithTicket(vnet.ProtocolVersionCurrent, "Eivor", testTicket(account))
	chooseCharacter(t, second, "Eivor")
	welcome := welcomeFrom(t, vnet.GetRootAsEnvelope(nextFrame(t, second), 0))

	spawn := welcome.Spawn(nil)
	if spawn == nil {
		t.Fatal("the welcome carries no spawn")
	}
	want := [3]float32{float32(saved.Pos[0]), float32(saved.Pos[1]), float32(saved.Pos[2])}
	if got := ([3]float32{spawn.X(), spawn.Y(), spawn.Z()}); got != want {
		t.Errorf("the welcome's spawn is %v, want the saved position %v", got, want)
	}
	if want == cfg.Spawn {
		t.Fatal("the saved position is the world spawn, so this assertion proves nothing")
	}

	// The first InventoryState after the welcome, which is the one the join sends
	// before streaming may put a chunk in the queue. Taking [0] rather than the newest
	// is the assertion: a pack that only became right after a later frame would be a
	// different guarantee wearing the same shape.
	back := collect(t, second)
	waitUntil(t, "the restored pack", func() bool { return len(back.inventoryStates()) > 0 })
	state := back.inventoryStates()[0]
	if len(state.Stacks) != int(protocol.InventorySlots) {
		t.Fatalf("the restored pack has %d slots, want %d", len(state.Stacks), protocol.InventorySlots)
	}
	for slot, stack := range state.Stacks {
		if stack != saved.Slots[slot] {
			t.Errorf("the restored pack's slot %d is %+v, want the saved %+v", slot, stack, saved.Slots[slot])
		}
	}
}

// A record is written when a session times out, not only when a client says goodbye.
//
// The two paths leave through the same teardown and that is exactly why this is worth
// pinning: an idle session gives its identity back, and it must give its life back too
// — a player whose connection died silently has not agreed to lose their evening.
func TestAnIdleSessionStillSavesItsLife(t *testing.T) {
	t.Parallel()

	store, err := persist.OpenStore(t.TempDir())
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	identities := identitiesOver(store)
	account := testAccount(22)
	chunks, sim, peers := serveDeps(t)

	conn := newFakeConn()
	done := make(chan error, 1)
	go func() {
		done <- session.Serve(context.Background(), conn, serveConfig(),
			session.Timeouts{Handshake: time.Hour, Idle: time.Hour}, chunks, sim, peers, identities, 1, discard())
	}()

	conn.in <- protocol.EncodeClientHelloWithTicket(vnet.ProtocolVersionCurrent, "Eivor", testTicket(account))
	chooseCharacter(t, conn, "Eivor")
	if got := vnet.GetRootAsEnvelope(nextFrame(t, conn), 0).PayloadType(); got != vnet.PayloadServerWelcome {
		t.Fatalf("the session got %s, want a welcome", got)
	}
	collect(t, conn)

	// The deadline the session armed for its next read, expired from under it. Nothing
	// is sent and the connection is not closed by the client: this is the silence a
	// dropped connection looks like from the server's side.
	conn.expireReadDeadline()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("an idle session returned %v, want nil", err)
		}
	case <-time.After(patience):
		t.Fatal("the idle session did not return")
	}

	saved, found, err := store.Load(onlyCharacter(t, store, account).ID)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !found {
		t.Fatal("an idle session left no record behind")
	}
	if saved.Slots[0].ItemID != uint16(game.ItemRustySword) {
		t.Errorf("the record holds %+v in slot 0, want the blade the player was carrying", saved.Slots[0])
	}
}

// Persistence is after the linger, not a snapshot taken when the socket dies. A body
// keeps falling during those ten production seconds; this shorter test window advances
// the same simulation and proves the stored position is the final authoritative one.
func TestLeavePersistsSimulationChangesFromTheLinger(t *testing.T) {
	const linger = 200 * time.Millisecond

	store, err := persist.OpenStore(t.TempDir())
	if err != nil {
		t.Fatalf("OpenStore: %v", err)
	}
	identities := identitiesOver(store)
	account := testAccount(24)
	cfg := serveConfig()
	ground := world.SpawnAt(cfg.WorldSeed)
	cfg.Spawn = [3]float32{ground[0], ground[1] + 20, ground[2]}
	chunks := world.NewCache(cfg.WorldSeed, 4, 512)
	generateAround(t, chunks, cfg.Spawn, 2)
	peers := session.NewRegistry(session.DefaultConcurrentSessions)
	sim, err := game.NewSim(cfg.TickRate, cfg.ViewDistance, cfg.WorldSeed, game.NewCacheTerrain(chunks), chunks, peers.NextID, discard())
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}

	conn := newFakeConn()
	done := make(chan error, 1)
	timeouts := longTimeouts()
	timeouts.Leave = linger
	go func() {
		done <- session.Serve(context.Background(), conn, cfg, timeouts, chunks, sim, peers, identities, 1, discard())
	}()
	conn.in <- protocol.EncodeClientHelloWithTicket(vnet.ProtocolVersionCurrent, "Eivor", testTicket(account))
	chooseCharacter(t, conn, "Eivor")
	welcome := welcomeFrom(t, vnet.GetRootAsEnvelope(nextFrameOfKind(t, conn, vnet.PayloadServerWelcome), 0))
	_ = nextFrameOfKind(t, conn, vnet.PayloadInventoryState)
	spawn := welcome.Spawn(nil)
	if spawn == nil {
		t.Fatal("welcome has no spawn")
	}
	startY := float64(spawn.Y())

	if err := conn.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	// Well inside the leave window, and enough ticks for gravity to make the final
	// position observably different from the disconnect position.
	time.Sleep(linger / 5)
	for tick := uint64(1); tick <= 10; tick++ {
		sim.Step(tick)
	}
	select {
	case err := <-done:
		t.Fatalf("session returned %v before the linger completed", err)
	default:
	}

	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("session returned %v", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("session did not finish the leave")
	}
	saved, found, err := store.Load(onlyCharacter(t, store, account).ID)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !found {
		t.Fatal("leave wrote no character record")
	}
	if saved.Pos[1] >= startY {
		t.Errorf("saved y = %v, want below disconnect y %v after linger gravity", saved.Pos[1], startY)
	}
}

// An ephemeral world keeps nothing, and a reconnect within one process is a new life.
//
// The claim the -world-dir help text makes, checked rather than described. **What it is
// a claim about moved with the ticket**: the old test presented a minted token back and
// watched a *different* identity come out, because an ephemeral world could recognise
// nobody. An account is recognised by the account service now, so the same person comes
// back as the same person on a server that stores nothing — and what an ephemeral world
// costs them is the life, which is exactly what this asserts.
func TestAnEphemeralWorldKeepsNoLife(t *testing.T) {
	t.Parallel()

	identities := ephemeralIdentities()
	account := testAccount(23)
	chunks, sim, peers := serveDeps(t)

	first := newFakeConn()
	firstDone := make(chan error, 1)
	go func() {
		firstDone <- session.Serve(context.Background(), first, serveConfig(), noTimeouts(), chunks, sim, peers, identities, 1, discard())
	}()

	first.in <- protocol.EncodeClientHelloWithTicket(vnet.ProtocolVersionCurrent, "Eivor", testTicket(account))
	chooseCharacter(t, first, "Eivor")
	if got := vnet.GetRootAsEnvelope(nextFrame(t, first), 0).PayloadType(); got != vnet.PayloadServerWelcome {
		t.Fatalf("the first session got %s, want a welcome", got)
	}
	sink := collect(t, first)

	// A pack this player could not have joined with, which is the only thing an
	// ephemeral world can be caught keeping: the starter blade somewhere other than the
	// slot every new player finds it in.
	const movedTo = 4
	sword := uint16(game.ItemRustySword)
	first.in <- protocol.EncodeInventoryMoveRequest(protocol.InventoryMoveRequest{From: 0, To: movedTo, Count: 1})
	waitUntil(t, "the blade to move to slot 4", func() bool { return sink.slotOf(sword) == movedTo })

	if err := first.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	select {
	case err := <-firstDone:
		if err != nil {
			t.Fatalf("the first session returned %v", err)
		}
	case <-time.After(patience):
		t.Fatal("the first session did not return")
	}

	second := newFakeConn()
	secondDone := make(chan error, 1)
	go func() {
		secondDone <- session.Serve(context.Background(), second, serveConfig(), noTimeouts(), chunks, sim, peers, identities, 2, discard())
	}()
	t.Cleanup(func() {
		_ = second.Close()
		<-secondDone
	})

	// The same account, on a server that wrote nothing down.
	second.in <- protocol.EncodeClientHelloWithTicket(vnet.ProtocolVersionCurrent, "Eivor", testTicket(account))
	chooseCharacter(t, second, "Eivor")
	if got := vnet.GetRootAsEnvelope(nextFrame(t, second), 0).PayloadType(); got != vnet.PayloadServerWelcome {
		t.Fatalf("the reconnect got %s, want a welcome; an ephemeral world still knows who somebody is", got)
	}

	back := collect(t, second)
	waitUntil(t, "the joining pack", func() bool { return len(back.inventoryStates()) > 0 })
	state := back.inventoryStates()[0]
	if state.Stacks[movedTo].ItemID == sword {
		t.Error("an ephemeral world restored a pack it cannot have stored")
	}
	if state.Stacks[0].ItemID != sword {
		t.Errorf("the reconnect's slot 0 is %+v, want the starter blade every new player joins with", state.Stacks[0])
	}
}
