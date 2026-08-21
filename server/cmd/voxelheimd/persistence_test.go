package main

import (
	"bytes"
	"context"
	"encoding/binary"
	"errors"
	"math"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/transport"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// livingWorldConfig is testConfig with the spawn the world actually derives.
//
// The hardcoded y in testConfig is fine for the admission and shutdown tests, which
// never let anybody stand anywhere. A test about *where a player is* needs the spawn
// the server would really use: it sits world.SpawnClearance above the surface, so a
// join settles by falling a couple of blocks rather than by falling eighty and dying
// on arrival.
func livingWorldConfig() session.Config {
	cfg := testConfig()
	cfg.Spawn = world.SpawnAt(cfg.WorldSeed)
	// One chunk of view: this test reads frames off a connection that drops them when
	// its queue is full, and the initial view is the only thing here big enough to
	// fill it.
	cfg.ViewDistance = 1
	return cfg
}

// persistentServer is the whole server over one directory — the world's chunks, the
// players' records, the camp and the clock — so a "restart" is this called twice on the
// same path.
//
// It restores the camp and the clock exactly where main does, before anything is served,
// which is what makes the second call a restart rather than a fresh world that happens
// to share a directory.
func persistentServer(t *testing.T, tr transport.Transport, dir string, cfg session.Config) (*server, *persist.Store) {
	t.Helper()

	chunks := world.NewPersistentCache(openStore(t, dir, cfg.WorldSeed), 4, 512)
	players := openPlayerStore(t, dir)
	camp := openStructureStore(t, dir)
	clock := openClockStore(t, dir)
	registry := session.NewRegistry()
	sim, err := game.NewSim(cfg.TickRate, cfg.ViewDistance, cfg.WorldSeed, game.NewCacheTerrain(chunks), chunks, registry.NextID, discard())
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}
	restoreStructures(sim, camp, discard())
	restoreClock(sim, clock, discard())

	return &server{
		tr:         tr,
		registry:   registry,
		identities: testIdentities(t, players),
		cfg:        cfg,
		timeouts:   session.Timeouts{},
		chunks:     chunks,
		structures: camp,
		clock:      clock,
		sim:        sim,
		// Long enough that the autosave cannot fire during a test, so a pass can only
		// come from the path the test is about. The autosave has its own test.
		saveEvery: time.Hour,
		log:       discard(),
	}, players
}

// start runs a server until the returned stop is called.
func start(t *testing.T, srv *server) (stop func()) {
	t.Helper()

	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	go func() {
		defer close(done)
		srv.run(ctx)
	}()

	stopped := false
	return func() {
		if stopped {
			return
		}
		stopped = true
		cancel()
		select {
		case <-done:
		case <-time.After(10 * time.Second):
			t.Fatal("the server did not shut down")
		}
	}
}

// TestAPlayerSurvivesARestart is the issue at the level a player experiences it: the
// process stops and starts, and they are still where they were, holding what they held.
//
// Everything crosses the disk. The second server shares nothing with the first but the
// directory — its own cache, its own simulation, its own claim set — so the only route
// from the first life to the second is the record on the file system.
func TestAPlayerSurvivesARestart(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	cfg := livingWorldConfig()

	// ---- the first run --------------------------------------------------------

	before := newScriptedConn("before")
	first, written := persistentServer(t, newQueueTransport(before), dir, cfg)
	stopFirst := start(t, first)

	account := testAccount(2)
	id := testPlayerID(account)
	if got := enterWorld(t, before, helloFor(t, account), creationOf("Eivor")).PayloadType(); got != vnet.PayloadServerWelcome {
		t.Fatalf("the first run got %s, want a welcome", got)
	}

	waitFor(t, "the player to join", func() bool { return first.sim.Count() == 1 })

	// The character the handshake settled on. The record is keyed by it rather than by
	// the account, which is the whole of #103 at this level: the simulation still keys a
	// life by account — one live session per account — and the disk keys it by the
	// character that lived it.
	character := onlyCharacter(t, written, account)

	// A pack change only the server could have decided, and a position only the
	// simulation could have produced: the blade moved out of the slot every new player
	// finds it in, and the settle every join begins with.
	const movedTo = 4
	before.in <- protocol.EncodeInventoryMoveRequest(protocol.InventoryMoveRequest{From: 0, To: movedTo, Count: 1})
	waitFor(t, "the blade to move", func() bool {
		life, ok := first.sim.Records()[id]
		return ok && life.Slots[movedTo].ItemID == uint16(game.ItemRustySword)
	})
	waitFor(t, "the player to settle", func() bool {
		life, ok := first.sim.Records()[id]
		return ok && life.Pos[1] < float64(cfg.Spawn[1])
	})

	// The disconnect is what writes the record, and the shutdown that follows must not
	// need to write anything for a player who has already gone.
	if err := before.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	// Polled through the store the running server already holds, deliberately: opening
	// a second one sweeps the directory for the temporaries an atomic write leaves
	// mid-rename, and a poll that did that every millisecond would delete the write it
	// was waiting for.
	waitFor(t, "the record to be written", func() bool {
		rec, found, err := written.Load(character.ID)
		return err == nil && found && !rec.Unplayed()
	})
	stopFirst()

	// Now that nothing is writing, a store opened afresh reads the same directory: this
	// is the file a new process would find, and its index is rebuilt from that file and
	// from nothing else.
	reopened := openPlayerStore(t, dir)
	if again := onlyCharacter(t, reopened, account); again.ID != character.ID || again.Name != character.Name {
		t.Fatalf("the reopened store holds %s/%q, want %s/%q", again.ID, again.Name, character.ID, character.Name)
	}
	saved, found, err := reopened.Load(character.ID)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !found {
		t.Fatal("the first run left no record behind")
	}

	// ---- the restart ----------------------------------------------------------

	after := newScriptedConn("after")
	second, _ := persistentServer(t, newQueueTransport(after), dir, cfg)
	stopSecond := start(t, second)
	defer stopSecond()

	// A ticket the restarted process has never seen, naming the account that played
	// before it: the only route from the first life to the second is the record on the
	// file system, and the only thing that says whose record it is came from the
	// account service. The character it selects is one *this* process knows about only
	// because it rebuilt its index from that directory.
	welcome := welcomeOf(t, enterWorld(t, after, helloFor(t, account), selectionOf(character.ID)))

	spawn := welcome.Spawn(nil)
	if spawn == nil {
		t.Fatal("the welcome carries no spawn")
	}
	want := [3]float32{float32(saved.Pos[0]), float32(saved.Pos[1]), float32(saved.Pos[2])}
	if got := ([3]float32{spawn.X(), spawn.Y(), spawn.Z()}); got != want {
		t.Errorf("after a restart the welcome's spawn is %v, want the saved %v", got, want)
	}
	if want == cfg.Spawn {
		t.Fatal("the saved position is the world spawn, so this assertion proves nothing")
	}

	// And the pack, read from the simulation the new process built: the blade is where
	// the previous session left it rather than back in slot 0.
	waitFor(t, "the restored player to join", func() bool { return second.sim.Count() == 1 })
	restored, ok := second.sim.Records()[id]
	if !ok {
		t.Fatal("the restored player is not in the new simulation under the same identity")
	}
	if restored.Slots != saved.Slots {
		t.Errorf("the restored pack is not the saved one: slot %d holds %+v, want %+v",
			movedTo, restored.Slots[movedTo], saved.Slots[movedTo])
	}

	// Near the saved position rather than exactly on it, and the tolerance is not
	// slack: the tick loop is running, a restored player joins with onGround false like
	// everybody else, and the first tick after the join settles them by a hair. The
	// exact placement is asserted above, on the welcome, which is built before any tick
	// can touch it.
	for axis := range restored.Pos {
		if drift := math.Abs(restored.Pos[axis] - saved.Pos[axis]); drift > 1 {
			t.Errorf("the restored position is %v, which is %.2f off the saved %v on axis %d",
				restored.Pos, drift, saved.Pos, axis)
		}
	}
}

// TestShutdownSavesASessionThatIsStillConnected is the third write path: the process
// goes down with somebody still playing.
//
// There is no final flush for players and there deliberately is not one. shutdown
// closes every connection and then waits for the workers, and every session's teardown
// writes its own record on the way out — so by the time run returns, the last word on
// each player has already been written. This test is what makes that ordering a
// guarantee rather than a reading of the code.
func TestShutdownSavesASessionThatIsStillConnected(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	cfg := livingWorldConfig()

	conn := newScriptedConn("still-here")
	srv, players := persistentServer(t, newQueueTransport(conn), dir, cfg)
	stop := start(t, srv)

	account := testAccount(3)
	if got := enterWorld(t, conn, helloFor(t, account), creationOf("Eivor")).PayloadType(); got != vnet.PayloadServerWelcome {
		t.Fatalf("the session got %s, want a welcome", got)
	}
	waitFor(t, "the player to join", func() bool { return srv.sim.Count() == 1 })
	character := onlyCharacter(t, players, account)

	// Nothing closes the connection: the shutdown does, which is the whole point.
	stop()

	saved, found, err := openPlayerStore(t, dir).Load(character.ID)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !found {
		t.Fatal("a session that was still connected at shutdown left no record")
	}
	if saved.Health == 0 {
		t.Error("the record holds no health; a record always describes a living player")
	}
	if saved.Slots[0].ItemID != uint16(game.ItemRustySword) {
		t.Errorf("the record holds %+v in slot 0, want the blade the player joined with", saved.Slots[0])
	}
}

// TestTheAutosaveWritesTheConnectedPlayers is the second write path, and the only one
// that exists for the failure nobody gets to tear down cleanly.
//
// A crash is not simulated — there is nothing to simulate, because the claim is only
// that a record appears on disk *while* the session is still running. What bounds the
// loss is the interval, which is why this test shortens it rather than waiting for the
// real one.
func TestTheAutosaveWritesTheConnectedPlayers(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	cfg := livingWorldConfig()

	conn := newScriptedConn("autosaved")
	srv, players := persistentServer(t, newQueueTransport(conn), dir, cfg)
	srv.saveEvery = 10 * time.Millisecond
	stop := start(t, srv)
	defer stop()

	account := testAccount(4)
	if got := enterWorld(t, conn, helloFor(t, account), creationOf("Eivor")).PayloadType(); got != vnet.PayloadServerWelcome {
		t.Fatalf("the session got %s, want a welcome", got)
	}
	character := onlyCharacter(t, players, account)

	// No disconnect and no shutdown: the record has to appear entirely on the
	// autosave's account — so an unplayed record, which is what the character was
	// created with, does not count as one.
	waitFor(t, "the autosave to write the connected player", func() bool {
		rec, found, err := players.Load(character.ID)
		return err == nil && found && !rec.Unplayed()
	})

	saved, _, err := players.Load(character.ID)
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if saved.Name != "Eivor" {
		t.Errorf("the autosaved record names %q, want the name the session connected with", saved.Name)
	}
	if saved.Health == 0 {
		t.Error("the autosaved record holds no health; a record always describes a living player")
	}
}

// waitFor polls a condition until it holds, or fails the test.
//
// Generous, because these tests hand work between an accept loop, a session goroutine,
// a tick loop and an autosave — and irrelevant to a passing run, which reaches every
// condition in milliseconds.
func waitFor(t *testing.T, what string, done func() bool) {
	t.Helper()

	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		if done() {
			return
		}
		time.Sleep(time.Millisecond)
	}
	t.Fatalf("timed out waiting for %s", what)
}

func welcomeOf(t *testing.T, env *vnet.Envelope) *vnet.ServerWelcome {
	t.Helper()

	if env.PayloadType() != vnet.PayloadServerWelcome {
		t.Fatalf("the reply is %s, want a welcome", env.PayloadType())
	}
	table := new(flatbuffers.Table)
	if !env.Payload(table) {
		t.Fatal("the welcome has no payload")
	}
	welcome := new(vnet.ServerWelcome)
	welcome.Init(table.Bytes, table.Pos)
	return welcome
}

// openStructureStore is the camp's counterpart to openPlayerStore.
func openStructureStore(t *testing.T, dir string) *persist.StructureStore {
	t.Helper()

	store, err := persist.OpenStructureStore(dir)
	if err != nil {
		t.Fatalf("persist.OpenStructureStore: %v", err)
	}
	return store
}

// openClockStore is the world clock's counterpart to openPlayerStore.
func openClockStore(t *testing.T, dir string) *persist.ClockStore {
	t.Helper()

	store, err := persist.OpenClockStore(dir)
	if err != nil {
		t.Fatalf("persist.OpenClockStore: %v", err)
	}
	return store
}

// awaitSnapshotStructures is the structure vector of the first snapshot this connection
// is sent that carries want of them.
//
// Drained rather than peeked at, because a session's frames are chunks and snapshots
// interleaved and the connection drops what it cannot queue. Waiting for a snapshot with
// the expected count is what makes that harmless: an early tick with an empty vector is
// skipped rather than mistaken for the answer.
func awaitSnapshotStructures(t *testing.T, conn *scriptedConn, want int) []*vnet.StructureState {
	t.Helper()

	deadline := time.After(10 * time.Second)
	for {
		select {
		case frame := <-conn.out:
			env := vnet.GetRootAsEnvelope(frame, 0)
			if env.PayloadType() != vnet.PayloadEntitySnapshot {
				continue
			}
			table := new(flatbuffers.Table)
			if !env.Payload(table) {
				continue
			}
			snapshot := new(vnet.EntitySnapshot)
			snapshot.Init(table.Bytes, table.Pos)
			if snapshot.StructuresLength() != want {
				continue
			}
			states := make([]*vnet.StructureState, 0, want)
			for i := range want {
				state := new(vnet.StructureState)
				if !snapshot.Structures(state, i) {
					t.Fatalf("structure %d is missing from a snapshot that claims to hold it", i)
				}
				states = append(states, state)
			}
			return states
		case <-deadline:
			t.Fatalf("%s was never sent a snapshot carrying %d structures", conn.name, want)
			return nil
		}
	}
}

// TestACampSurvivesARestart is the issue at the level a player experiences it: they log
// off with a tent and a forge standing, the process stops and starts, and both are still
// there — still theirs, and still where they come back to when they die.
//
// Everything crosses the disk. The second server shares nothing with the first but the
// directory: its own cache, its own simulation, its own claim set, its own entity id
// counter. The only route from the first camp to the second is structures.bin.
//
// The camp is seeded through Sim.RestoreStructures rather than through a
// PlaceStructureRequest, and that is a deliberate limit on what this test claims. A tent
// is an inventory item and nothing in the wire protocol hands a fresh player one, so
// placing through a socket would mean mining and crafting an evening's worth of materials
// to test a file format. What placement does — including that it marks the camp for
// writing — is pinned in internal/game; what is pinned *here* is the part only this
// package can show: that the identity behind a token owns the camp across processes, that
// shutdown writes it, that startup reads it back before anyone is served, and that the
// wire carries the reconnected session's entity id.
func TestACampSurvivesARestart(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	cfg := livingWorldConfig()

	tentAnchor := [3]int32{2, 63, 0}
	forgeAnchor := [3]int32{6, 63, 0}

	// ---- the first run --------------------------------------------------------

	before := newScriptedConn("before")
	first, _ := persistentServer(t, newQueueTransport(before), dir, cfg)
	stopFirst := start(t, first)

	account := testAccount(5)
	id := testPlayerID(account)
	if got := enterWorld(t, before, helloFor(t, account), creationOf("Eivor")).PayloadType(); got != vnet.PayloadServerWelcome {
		t.Fatalf("the first run got %s, want a welcome", got)
	}

	waitFor(t, "the player to join", func() bool { return first.sim.Count() == 1 })

	// The camp, owned by the player the handshake just resolved — which is the value
	// the whole issue turns on, and the one a test that invented an id could not check.
	if err := first.sim.RestoreStructures([]game.Structure{
		{Kind: vnet.StructureKindTent, Anchor: tentAnchor, Facing: vnet.FacingNorth, Owner: id},
		{Kind: vnet.StructureKindForge, Anchor: forgeAnchor, Facing: vnet.FacingEast, Owner: id},
	}); err != nil {
		t.Fatalf("seeding the camp: %v", err)
	}
	// A restore deliberately owes the disk nothing, so this is the placement's job stood
	// in for: without it there would be nothing for shutdown to write.
	first.sim.MarkStructuresDirty()

	if err := before.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	// The camp is written by shutdown and by nothing else here — the autosave interval is
	// an hour in this harness — so this is the flush being tested, not a lucky tick.
	stopFirst()

	saved, found, err := openStructureStore(t, dir).Load()
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !found || len(saved) != 2 {
		t.Fatalf("the first run left %d structures on disk (found=%v), want 2", len(saved), found)
	}
	for _, rec := range saved {
		if rec.Owner != id {
			t.Errorf("a stored structure is owned by %s, want the connecting player %s", rec.Owner.Short(), id.Short())
		}
	}

	// ---- the restart ----------------------------------------------------------

	after := newScriptedConn("after")
	second, players := persistentServer(t, newQueueTransport(after), dir, cfg)
	stopSecond := start(t, second)
	defer stopSecond()

	// Already standing before anybody is admitted, which is what puts the camp in the
	// *first* snapshot a returning player receives rather than in a later one.
	if got := second.sim.StructureCount(); got != 2 {
		t.Fatalf("the restarted server holds %d structures before any session, want 2", got)
	}

	welcome := welcomeOf(t, enterWorld(t, after,
		helloFor(t, account), selectionOf(onlyCharacter(t, players, account).ID)))
	rejoinedAs := welcome.EntityId()

	states := awaitSnapshotStructures(t, after, 2)

	ids := map[uint64]struct{}{}
	kinds := map[vnet.StructureKind][3]int32{}
	for _, state := range states {
		// Fresh, unique and naming something: ids are re-minted on load, never read from
		// the file, so the counter that names players and drops cannot collide with them.
		if state.StructureId() == 0 {
			t.Error("a restored structure reached the wire with id 0, which names nothing")
		}
		if _, twice := ids[state.StructureId()]; twice {
			t.Errorf("id %d names two structures in one snapshot", state.StructureId())
		}
		ids[state.StructureId()] = struct{}{}

		// The V5 rule across a process boundary: the owner's *current* entity id, which
		// the reconnect has just changed.
		if state.OwnerEntityId() != rejoinedAs {
			t.Errorf("a restored structure is announced as owned by entity %d, want the reconnected %d",
				state.OwnerEntityId(), rejoinedAs)
		}

		anchor := state.Anchor(nil)
		if anchor == nil {
			t.Fatal("a restored structure reached the wire with no anchor")
		}
		kinds[state.Kind()] = [3]int32{anchor.X(), anchor.Y(), anchor.Z()}
	}

	if got, ok := kinds[vnet.StructureKindTent]; !ok || got != tentAnchor {
		t.Errorf("the restored tent is at %v (present=%v), want %v", got, ok, tentAnchor)
	}
	if got, ok := kinds[vnet.StructureKindForge]; !ok || got != forgeAnchor {
		t.Errorf("the restored forge is at %v (present=%v), want %v", got, ok, forgeAnchor)
	}

	// And the player is back under the same identity, which is what makes the camp above
	// theirs rather than merely present.
	if _, playing := second.sim.Records()[id]; !playing {
		t.Fatal("the reconnected player is not in the new simulation under the same identity")
	}

	// **What this test deliberately does not assert is the respawn.** Killing a player
	// needs a damage path, and game exports none — deliberately, because the ones that
	// exist are a draugr's swing and a fall, both of which are the simulation's decision
	// rather than a caller's. Adding a test-only export to reach one here would widen the
	// production API for a single assertion, and the assertion has a better home:
	// TestAPlayerRespawnsAtATentThatCameBackFromDisk, in internal/game, restores a camp
	// from a capture and dies in it.
}

// TestTheWorldKeepsTimeAcrossARestart is the issue at the level a player experiences it:
// they log off at dusk, the process stops and starts, and it is still dusk.
//
// Everything crosses the disk. The second server shares nothing with the first but the
// directory — its own cache, its own simulation, its own claim set — so the only route
// from the first evening to the second is clock.bin.
//
// The exact tick is asserted rather than a range, and that is what makes this a test of
// the *last word* rather than of the file merely existing: shutdown runs after every
// worker has stopped, so the tick loop has certainly stopped moving the clock before the
// flush reads it, and the two numbers can only be equal.
func TestTheWorldKeepsTimeAcrossARestart(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	cfg := livingWorldConfig()

	// ---- the first run --------------------------------------------------------

	first, _ := persistentServer(t, newQueueTransport(), dir, cfg)
	if got := first.sim.TickOfDay(); got != 0 {
		t.Fatalf("a world with no clock file started at tick %d, want first light", got)
	}
	// Dusk, put there rather than waited for: reaching minute twelve honestly is twelve
	// minutes of test. What the wind-forward does not touch is anything below it — the
	// clock still advances one per tick from here, and shutdown still writes whatever it
	// reached.
	if err := first.sim.RestoreClock(game.NightStartTicks); err != nil {
		t.Fatalf("winding the clock to dusk: %v", err)
	}

	stopFirst := start(t, first)
	waitFor(t, "the clock to advance past dusk", func() bool {
		return first.sim.TickOfDay() > game.NightStartTicks
	})
	// Written by shutdown and by nothing else here — the autosave interval is an hour in
	// this harness — so this is the flush being tested, not a lucky tick.
	stopFirst()

	dusk := first.sim.TickOfDay()
	if !game.IsNight(dusk) {
		t.Fatalf("the first run stopped at tick %d, which is not night; the test cannot say anything", dusk)
	}

	stored, found, err := openClockStore(t, dir).Load()
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if !found {
		t.Fatal("the first run left no clock file")
	}
	if stored != dusk {
		t.Errorf("the file holds tick %d and the simulation stopped at %d", stored, dusk)
	}

	// ---- the restart ----------------------------------------------------------

	second, _ := persistentServer(t, newQueueTransport(), dir, cfg)

	// Already the right time of day before anybody is admitted, which is what puts the
	// evening in a returning player's *first* snapshot rather than in a later one.
	got := second.sim.TickOfDay()
	if got != dusk {
		t.Errorf("the world came back at tick %d of the day, want the %d it stopped at", got, dusk)
	}
	if !game.IsNight(got) {
		t.Errorf("the world stopped at night and came back at tick %d, which is not", got)
	}
}

// TestTheAutosaveWritesTheClock is the other write path, and the only one that exists
// for the failure nobody gets to tear down cleanly.
//
// A crash is not simulated — there is nothing to simulate, because the claim is only
// that the file appears *while* the server is still running. What bounds the loss is the
// interval, which is why this test shortens it rather than waiting for the real one.
//
// It also pins the half the camp's autosave cannot show: there is no dirty flag here, so
// a world with nobody in it and nothing built in it still writes, because time passed.
func TestTheAutosaveWritesTheClock(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	cfg := livingWorldConfig()

	srv, _ := persistentServer(t, newQueueTransport(), dir, cfg)
	srv.saveEvery = 10 * time.Millisecond
	stop := start(t, srv)
	defer stop()

	clock := openClockStore(t, dir)
	waitFor(t, "the autosave to write a clock that has moved", func() bool {
		stored, found, err := clock.Load()
		return err == nil && found && stored > 0
	})

	stored, _, err := clock.Load()
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	if stored >= game.DayLengthTicks {
		t.Errorf("the autosave wrote tick %d, which is not inside a %d-tick day", stored, game.DayLengthTicks)
	}
}

// A clock file this build cannot read is a world that starts at first light, and it is
// not a reason to refuse to start: the terrain, every player record and the ability to
// log in at all would be held hostage to sixteen bytes.
//
// The file is kept. It does not survive long — there is no dirty flag, so the next
// autosave rewrites it — but it survives the start that could not use it, which is the
// window an operator has to look at it in.
func TestACorruptClockStartsTheWorldAtFirstLightAndKeepsTheFile(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	cfg := livingWorldConfig()
	clock := openClockStore(t, dir)

	if err := os.WriteFile(clock.Path(), []byte("not a clock at all"), 0o600); err != nil {
		t.Fatalf("writing the corrupt clock: %v", err)
	}

	srv, _ := persistentServer(t, newQueueTransport(), dir, cfg)

	if got := srv.sim.TickOfDay(); got != 0 {
		t.Errorf("a world with an unreadable clock started at tick %d, want first light", got)
	}
	if _, err := os.Stat(clock.Path()); err != nil {
		t.Errorf("the unreadable clock file did not survive the start: %v", err)
	}
}

// A stored tick that cannot exist is refused rather than wrapped, at the level main
// wires it: the file is well formed, its checksum is right, and the number in it is not
// a tick of any day this build has.
//
// Wrapping would turn 4,000,000,000 into a perfectly ordinary mid-afternoon and destroy
// the only evidence that anything was wrong. game.Sim.RestoreClock is where that is
// decided; this is the wiring that asks it and survives the answer.
func TestAnImpossibleStoredTickStartsTheWorldAtFirstLight(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	cfg := livingWorldConfig()
	clock := openClockStore(t, dir)

	if err := clock.Save(game.DayLengthTicks); err != nil {
		t.Fatalf("Save: %v", err)
	}

	srv, _ := persistentServer(t, newQueueTransport(), dir, cfg)

	if got := srv.sim.TickOfDay(); got != 0 {
		t.Errorf("a world whose file held tick %d started at %d, want first light", game.DayLengthTicks, got)
	}
	// Refused, not repaired: a wrap would have produced tick 0 too, so the file is what
	// tells the two apart.
	stored, found, err := clock.Load()
	if err != nil || !found {
		t.Fatalf("Load = (%d, %v, %v)", stored, found, err)
	}
	if stored != game.DayLengthTicks {
		t.Errorf("the refused file now holds tick %d; it should have been left alone", stored)
	}
}

// An ephemeral world keeps a clock in memory and writes nothing, which is the difference
// the operator chose when they named no world directory. Its night still arrives on
// time; it just does not remember which part of the day it was in.
func TestAnEphemeralWorldKeepsItsClockInMemoryOnly(t *testing.T) {
	t.Parallel()

	log := discard()

	clock, err := openClock(options{worldDir: ""}, log)
	if err != nil {
		t.Fatalf("openClock: %v", err)
	}
	if clock != nil {
		t.Fatalf("an ephemeral world was given a clock store at %q", clock.Path())
	}

	chunks := world.NewCache(1, 4, 512)
	registry := session.NewRegistry()
	sim, err := game.NewSim(game.DefaultTickRate, 1, testConfig().WorldSeed, game.NewCacheTerrain(chunks), chunks, registry.NextID, log)
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}
	// The nil store is a no-op at every call site rather than a branch at each one, so
	// both halves of the wiring have to survive it.
	restoreClock(sim, clock, log)
	srv := &server{clock: clock, sim: sim, log: log}
	srv.flushClock()

	sim.Step(1)
	if got := sim.TickOfDay(); got != 1 {
		t.Errorf("an ephemeral world's clock reads %d after one tick, want 1", got)
	}
}

// TestEveryCharacterSurvivesARestart is the multi-character half of the restart, at the
// level a player experiences it: three characters on one account, each standing
// somewhere different, and a process that stops and starts between them.
//
// **Everything crosses the disk, index included.** The second server shares nothing with
// the first but the directory — and the character index is rebuilt from the records
// there and from nothing else, so a character that could not be found afterwards is a
// character this world has lost.
func TestEveryCharacterSurvivesARestart(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	cfg := livingWorldConfig()
	account, neighbour := testAccount(5), testAccount(6)

	// ---- three characters, each somewhere of its own -------------------------

	seeded := map[string][3]float64{
		"Eivor":  {12.5, 66, -8.5},
		"Sigrun": {-40.5, 68, 21.5},
		"Halvar": {103.5, 70, 55.5},
	}
	ids := make(map[string]persist.CharacterID, len(seeded))
	{
		store := openPlayerStore(t, dir)
		for name, pos := range seeded {
			character, err := store.Create(testPlayerID(account), name, testAppearance())
			if err != nil {
				t.Fatalf("Create(%q): %v", name, err)
			}
			ids[name] = character.ID
			if err := store.Save(character.ID, persist.Record{
				LastSeen: time.Unix(1, 0),
				Pos:      pos,
				Health:   game.PlayerMaxHealth,
			}); err != nil {
				t.Fatalf("seeding %q: %v", name, err)
			}
		}
		// Somebody else's character, so "every character came back" cannot pass by
		// coming back with everything to everybody.
		if _, err := store.Create(testPlayerID(neighbour), "Bjorn", testAppearance()); err != nil {
			t.Fatalf("Create for the neighbour: %v", err)
		}
	}

	// ---- the restart: a process that has never seen any of them --------------

	conn := newScriptedConn("after")
	srv, players := persistentServer(t, newQueueTransport(conn), dir, cfg)
	stop := start(t, srv)
	defer stop()

	// The index came back whole, and it came back with the owners intact.
	held := players.Characters(testPlayerID(account))
	if len(held) != len(seeded) {
		t.Fatalf("the account holds %d characters after the restart, want %d", len(held), len(seeded))
	}
	for _, character := range held {
		if ids[character.Name] != character.ID {
			t.Errorf("%q came back as %s, want %s", character.Name, character.ID, ids[character.Name])
		}
		if character.Owner != testPlayerID(account) {
			t.Errorf("%q came back owned by %s", character.Name, character.Owner.Short())
		}
	}
	for _, character := range players.Characters(testPlayerID(neighbour)) {
		if _, mine := seeded[character.Name]; mine {
			t.Errorf("the neighbour came back holding %q", character.Name)
		}
	}

	// And the one the *selection* names is the one that plays, standing where it stood.
	// The welcome's spawn is the assertion, because it is built before any tick can move
	// anybody — and it is why the choice comes before the welcome at all: a spawn belongs
	// to a character, so it cannot be announced until there is one.
	spawn := welcomeOf(t, enterWorld(t, conn, helloFor(t, account), selectionOf(ids["Sigrun"]))).Spawn(nil)
	if spawn == nil {
		t.Fatal("the welcome carries no spawn")
	}
	want := seeded["Sigrun"]
	got := [3]float32{spawn.X(), spawn.Y(), spawn.Z()}
	if got != ([3]float32{float32(want[0]), float32(want[1]), float32(want[2])}) {
		t.Errorf("the welcome's spawn is %v, want Sigrun's %v", got, want)
	}
	if got == cfg.Spawn {
		t.Fatal("Sigrun's position is the world spawn, so this assertion proves nothing")
	}
}

// TestAPreCharacterPlayersDirectoryIsSetAsideOnStart is the set-aside at the level an
// operator meets it: the directory a build before characters wrote is kept whole, a
// fresh one is opened beside it, and the server starts with no characters at all.
//
// No migration runs and none exists. A v2 record names a player by the hash of their
// account and cannot say which character it was, so there is nothing to convert — what
// there is, is a directory somebody can copy away and read at their leisure.
func TestAPreCharacterPlayersDirectoryIsSetAsideOnStart(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	players := filepath.Join(dir, "players")
	if err := os.MkdirAll(players, 0o755); err != nil {
		t.Fatalf("creating the old players directory: %v", err)
	}

	// A record with this server's magic and the format version before this one, which
	// is what a build before characters left behind.
	old := make([]byte, 40)
	copy(old[0:4], []byte("VXHP"))
	binary.LittleEndian.PutUint32(old[4:8], persist.StoreVersion-1)
	name := strings.Repeat("ab", 32) + ".bin"
	if err := os.WriteFile(filepath.Join(players, name), old, 0o600); err != nil {
		t.Fatalf("writing the old record: %v", err)
	}

	store, err := openPlayers(options{worldDir: dir}, discard())
	if err != nil {
		t.Fatalf("openPlayers: %v", err)
	}
	if store.Count() != 0 {
		t.Errorf("the server started with %d characters, want none", store.Count())
	}
	if _, err := os.Stat(filepath.Join(players, name)); !errors.Is(err, os.ErrNotExist) {
		t.Errorf("the old record is still where this build would read it: %v", err)
	}

	aside := store.SetAside()
	if aside == "" {
		t.Fatal("nothing was reported as set aside")
	}
	kept, err := os.ReadFile(filepath.Join(aside, name))
	if err != nil {
		t.Fatalf("the old record was not kept: %v", err)
	}
	if !bytes.Equal(kept, old) {
		t.Error("the old record was changed on the way aside")
	}
}
