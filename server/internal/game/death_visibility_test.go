package game

import (
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// What a snapshot says about who is down, from the side that decides it.
//
// **The gap these close is not that the client ignored a death — it is that nothing sent
// one.** `PlayerVitals` is per-recipient by contract, so before V10 a session was told its
// own life state and nobody else's, and a player killed beside you kept standing. Every
// test here is about `dead_players` being a *statement in the snapshot* rather than an
// event: a session that arrives afterwards has to be told the same thing as the one that
// watched it happen, and no event can do that.

// deadPlayers is the dead_players vector of the newest snapshot this session was sent.
//
// Nil and empty are the same wire value, and this collapses them to an empty slice exactly
// as a decoder does.
func deadPlayers(t *testing.T, out *dropSink) []uint64 {
	t.Helper()

	snapshot := newestSnapshot(t, out)
	ids := make([]uint64, 0, snapshot.DeadPlayersLength())
	for i := range snapshot.DeadPlayersLength() {
		ids = append(ids, snapshot.DeadPlayers(i))
	}
	return ids
}

// checkDeadPlayers asserts every invariant schemas/player.fbs attaches to dead_players, in
// the order the client's decoder checks them — so this fails instead of the connection.
// `own` is the entity id the snapshot is addressed to: its id is here exactly when its
// vitals say Dead.
func checkDeadPlayers(t *testing.T, snapshot *vnet.EntitySnapshot, own uint64) {
	t.Helper()

	entities := make(map[uint64]bool, snapshot.EntitiesLength())
	for i := range snapshot.EntitiesLength() {
		var state vnet.EntityState
		if snapshot.Entities(&state, i) {
			entities[state.EntityId()] = true
		}
	}

	seen := make(map[uint64]bool, snapshot.DeadPlayersLength())
	ownIsDead := false
	for i := range snapshot.DeadPlayersLength() {
		id := snapshot.DeadPlayers(i)
		if !entities[id] {
			t.Errorf("dead_players names %d, which is not a player in this snapshot", id)
		}
		if seen[id] {
			t.Errorf("dead_players names %d twice", id)
		}
		seen[id] = true
		if id == own {
			ownIsDead = true
		}
	}

	vitals := snapshot.SelfVitals(nil)
	if vitals == nil {
		t.Fatal("the snapshot carries no self_vitals")
	}
	if saysDead := vitals.LifeState() == vnet.LifeStateDead; saysDead != ownIsDead {
		t.Errorf("self_vitals says dead=%t and dead_players says dead=%t for the recipient %d",
			saysDead, ownIsDead, own)
	}
}

// kill takes every point of health this player has, which is the only path to Dead.
func kill(h *dropHarness, player *Player) {
	h.sim.mu.Lock()
	player.damageLocked(PlayerMaxHealth)
	h.sim.mu.Unlock()
}

// **The whole issue in one assertion: a death reaches the people who are standing there.**
//
// Both directions, deliberately. A snapshot built for A while B is dead carries B, and the
// same tick's snapshot for B carries B as well — the recipient is inside its own view, so
// its own body is stated the same way as everybody else's.
func TestASnapshotNamesTheDeadPlayersItsRecipientCanSee(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	_, watcher := h.join(1, [3]float32{0.5, 64, 0.5})
	victim, victimOut := h.join(2, [3]float32{2.5, 64, 0.5})
	h.step()

	if got := deadPlayers(t, watcher); len(got) != 0 {
		t.Fatalf("a tick on which nobody has died named %v as dead", got)
	}

	kill(h, victim)
	h.step()

	if got := deadPlayers(t, watcher); len(got) != 1 || got[0] != victim.entityID {
		t.Errorf("the watcher was sent %v, want just the victim %d", got, victim.entityID)
	}
	if got := deadPlayers(t, victimOut); len(got) != 1 || got[0] != victim.entityID {
		t.Errorf("the victim's own snapshot carries %v, want its own id %d", got, victim.entityID)
	}

	checkDeadPlayers(t, newestSnapshot(t, watcher), 1)
	checkDeadPlayers(t, newestSnapshot(t, victimOut), victim.entityID)
}

// **The join case, which an event-shaped implementation passes in play and fails on
// reconnect.** The death happens before this session exists; nothing is replayed and nothing
// has to be, because the state is in every snapshot. A client that had to be present for a
// death would draw this player standing.
func TestASessionThatArrivesAfterADeathIsToldAboutItAnyway(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	victim, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	kill(h, victim)
	h.step()

	// Only now does anybody turn up to see it.
	_, latecomer := h.join(2, [3]float32{2.5, 64, 0.5})
	h.step()

	if got := deadPlayers(t, latecomer); len(got) != 1 || got[0] != victim.entityID {
		t.Errorf("the first snapshot a latecomer was sent carries %v, want the dead player %d",
			got, victim.entityID)
	}
	checkDeadPlayers(t, newestSnapshot(t, latecomer), 2)
}

// The state clears for everybody at once, because it is derived from the life state rather
// than remembered anywhere: a viewer who watched the fall and one who only ever saw the body
// get the same answer on the tick the server stands them back up.
func TestARespawnClearsTheDeathForEveryViewer(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	_, watcher := h.join(1, [3]float32{0.5, 64, 0.5})
	victim, victimOut := h.join(2, [3]float32{2.5, 64, 0.5})

	kill(h, victim)
	h.step()
	if got := deadPlayers(t, watcher); len(got) != 1 {
		t.Fatalf("the watcher was sent %v before the respawn, want one dead player", got)
	}

	// Long enough for the countdown to run out. One tick past it, so the assertion is
	// about the respawn rather than about the last tick of the death.
	h.advance(int(h.sim.deathTicks) + 2)

	if got := deadPlayers(t, watcher); len(got) != 0 {
		t.Errorf("the watcher still sees %v as dead after the respawn", got)
	}
	if got := deadPlayers(t, victimOut); len(got) != 0 {
		t.Errorf("the respawned player is still named dead in its own snapshot: %v", got)
	}
	checkDeadPlayers(t, newestSnapshot(t, watcher), 1)
	checkDeadPlayers(t, newestSnapshot(t, victimOut), victim.entityID)
}

// The same visibility cube every other entity in the snapshot obeys, and the invariant that
// keeps the two vectors from disagreeing: every id in dead_players names a player in the
// same snapshot's entities, so a dead player nobody can see is not mentioned at all.
func TestADeadPlayerOutsideTheViewIsNotNamedAtAll(t *testing.T) {
	t.Parallel()

	// One chunk of view distance, so a player three chunks away is unambiguously outside it.
	h := newDropHarnessAt(t, dropTerrain{groundTop: 63}, 1)
	_, watcher := h.join(1, [3]float32{0.5, 64, 0.5})
	distant, _ := h.join(2, [3]float32{float32(3*world.ChunkSize) + 0.5, 64, 0.5})

	kill(h, distant)
	h.step()

	if got := deadPlayers(t, watcher); len(got) != 0 {
		t.Errorf("a session three chunks away was told about %v", got)
	}
	checkDeadPlayers(t, newestSnapshot(t, watcher), 1)
}
