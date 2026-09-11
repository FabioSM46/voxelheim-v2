package game

import (
	"slices"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// killBossInSession puts one creature of the named species inside a session's own
// simulation and kills it through [Sim.damageMobLocked], which is the one path a mob
// loses health by and the only one that reaches the killed-mob transition.
//
// The damage is applied rather than swung, for the reason boss_species_test.go gives:
// a Vargr guardian and a Draugr king carry thousands of health for every member of the
// party that pulls them (#1099), so driving these assertions through the authoritative
// Attack path would cost hundreds of swings apiece and would be measuring the balance
// rather than the rule under test. The rule under
// test is what a *death* does, and this is the transition a death goes through.
//
// Prefer an accessible placed encounter. Tests of generic binding idempotency may
// still inject another species instance; dungeon progression tests use only the
// two placed identities and exercise the closed king directly.
func killMobInSession(t *testing.T, session InstanceSession, kind vnet.MobKind) uint64 {
	t.Helper()
	session.Sim.mu.Lock()
	defer session.Sim.mu.Unlock()
	for id, m := range session.Sim.mobs {
		if m.kind == kind && !session.Sim.dungeonBossLocked(m) {
			if !session.Sim.damageMobLocked(m, m.health) {
				t.Fatal("placed boss survived killing blow")
			}
			return id
		}
	}
	id, made := session.Sim.spawnMobLocked(kind, [3]float64{0.5, 64, 0.5})
	if !made {
		t.Fatalf("the instance simulation refused to place a %s", kind)
	}
	m := session.Sim.mobs[id]
	if !session.Sim.damageMobLocked(m, m.health) {
		t.Fatalf("the killing blow on the %s did not kill it", kind)
	}
	return id
}

func instanceBindingSetup(t *testing.T, limit int) (*InstanceManager, InstanceRuin, InstanceCharacter) {
	t.Helper()
	return instanceTestManager(t, 20, limit), InstanceRuin{7, -2}, instanceTestCharacter(1)
}

// The whole of the trigger: a session is free until a boss dies in it, saved from the
// tick after, and everybody who was inside at that moment owns the run.
func TestTheFirstBossSavesTheSessionAndBindsWhoWasInside(t *testing.T) {
	m, ruin, first := instanceBindingSetup(t, 4)
	second, absent := instanceTestCharacter(2), instanceTestCharacter(3)

	session, err := m.Reenter(ruin, first)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := m.Join(session.ID, second); err != nil {
		t.Fatal(err)
	}
	// Inside for the pull, gone before the blow. A binding is what somebody was there
	// for, and this is the character that says so.
	if _, err := m.Join(session.ID, absent); err != nil {
		t.Fatal(err)
	}
	if !m.Leave(session.ID, absent) {
		t.Fatal("the third character never left")
	}

	m.Step()
	if held, _ := m.Lookup(session.ID); held.State != InstanceFree {
		t.Fatalf("a session with no dead boss is %v, want free", held.State)
	}
	for _, character := range []InstanceCharacter{first, second, absent} {
		if id, bound := m.Bound(ruin, character); bound {
			t.Fatalf("a free session bound a character to %d", id)
		}
	}

	killMobInSession(t, session, vnet.MobKindVargrGuardian)
	m.Step()

	held, live := m.Lookup(session.ID)
	if !live {
		t.Fatal("the session died on the tick it was saved")
	}
	if held.State != InstanceSaved {
		t.Fatalf("the first boss left the session %v, want saved", held.State)
	}
	for _, character := range []InstanceCharacter{first, second} {
		id, bound := m.Bound(ruin, character)
		if !bound {
			t.Fatalf("a character inside at the kill is unbound")
		}
		if id != session.ID {
			t.Fatalf("bound to session %d, want the one the boss died in, %d", id, session.ID)
		}
	}
	if id, bound := m.Bound(ruin, absent); bound {
		t.Fatalf("a character who had already left is bound to %d", id)
	}
	if want := []vnet.MobKind{vnet.MobKindVargrGuardian}; !slices.Equal(held.DefeatedBosses, want) {
		t.Fatalf("defeated encounters = %v, want %v", held.DefeatedBosses, want)
	}
}

// Binding is per character *and per dungeon*: owning one ruin's run says nothing about
// any other ruin, and a bound character can still start a fresh copy elsewhere.
func TestBindingIsPerDungeon(t *testing.T) {
	m, ruin, character := instanceBindingSetup(t, 4)
	other := InstanceRuin{-11, 40}

	saved, err := m.Reenter(ruin, character)
	if err != nil {
		t.Fatal(err)
	}
	killMobInSession(t, saved, vnet.MobKindDraugrKing)
	m.Step()
	if _, bound := m.Bound(ruin, character); !bound {
		t.Fatal("the kill bound nobody")
	}
	if id, bound := m.Bound(other, character); bound {
		t.Fatalf("binding in %v bound the character in %v to session %d", ruin, other, id)
	}

	// And the character is free to open a run in the other ruin, which is the same
	// sentence from the other end.
	if !m.Leave(saved.ID, character) {
		t.Fatal("leaving the saved session was refused")
	}
	elsewhere, err := m.Reenter(other, character)
	if err != nil {
		t.Fatalf("a character bound in one ruin could not enter another: %v", err)
	}
	if elsewhere.ID == saved.ID {
		t.Fatal("the other ruin reused the bound session")
	}
	if elsewhere.State != InstanceFree {
		t.Fatalf("the fresh copy of %v is %v, want free", other, elsewhere.State)
	}
	if id, bound := m.Bound(other, character); bound {
		t.Fatalf("entering a free copy bound the character to %d", id)
	}
	if id, bound := m.Bound(ruin, character); !bound || id != saved.ID {
		t.Fatalf("the original binding was disturbed: %d %v", id, bound)
	}
}

// A saved run is theirs until its reset. The empty grace is the other of the exactly two
// things the flag means, and this is that half.
func TestASavedSessionOutlivesTheEmptyGrace(t *testing.T) {
	m, ruin, character := instanceBindingSetup(t, 4)

	free, err := m.Create(ruin)
	if err != nil {
		t.Fatal(err)
	}
	saved, err := m.Reenter(ruin, character)
	if err != nil {
		t.Fatal(err)
	}
	killMobInSession(t, saved, vnet.MobKindVargrGuardian)
	m.Step()
	if !m.Leave(saved.ID, character) {
		t.Fatal("leaving the saved session was refused")
	}

	// Both sessions are empty and both are one step from the boundary. Only the free one
	// may be taken away by it.
	m.sessions[free.ID].emptyTicks = m.graceTicks - 1
	m.sessions[saved.ID].emptyTicks = m.graceTicks - 1
	m.Step()

	if _, live := m.Lookup(free.ID); live {
		t.Fatal("a free empty session survived its grace")
	}
	held, live := m.Lookup(saved.ID)
	if !live {
		t.Fatal("a saved session expired after thirty empty minutes")
	}
	if held.State != InstanceSaved {
		t.Fatalf("the surviving session is %v, want saved", held.State)
	}
	if m.sessions[saved.ID].emptyTicks != 0 {
		t.Fatalf("a saved session kept counting empty ticks: %d", m.sessions[saved.ID].emptyTicks)
	}
	if id, bound := m.Bound(ruin, character); !bound || id != saved.ID {
		t.Fatalf("the binding did not outlive the departure: %d %v", id, bound)
	}

	// Far past the boundary, and still nobody's to take.
	for range 4 {
		m.Step()
	}
	if _, live := m.Lookup(saved.ID); !live {
		t.Fatal("a saved session expired later instead")
	}
}

// Arriving after the boss fell binds on entry. Whether that entry is offered, warned
// about or refused is #978's; what this owes it is that anybody who does get in is bound
// by the same rule as the party that was standing there.
func TestJoiningASavedSessionBindsOnEntry(t *testing.T) {
	m, ruin, first := instanceBindingSetup(t, 4)
	late := instanceTestCharacter(2)

	session, err := m.Reenter(ruin, first)
	if err != nil {
		t.Fatal(err)
	}
	killMobInSession(t, session, vnet.MobKindVargrGuardian)
	m.Step()

	joined, err := m.Join(session.ID, late)
	if err != nil {
		t.Fatal(err)
	}
	if joined.State != InstanceSaved {
		t.Fatalf("the joined session reads %v, want saved", joined.State)
	}
	id, bound := m.Bound(ruin, late)
	if !bound || id != session.ID {
		t.Fatalf("a late arrival is bound to %d (%v), want %d", id, bound, session.ID)
	}

	// The first binding for a ruin wins while the session it names is alive: a second
	// saved copy of the same ruin must not silently move a claim the character holds.
	if !m.Leave(session.ID, late) {
		t.Fatal("the late arrival never left")
	}
	elsewhere, err := m.Create(ruin)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := m.Join(elsewhere.ID, late); err != nil {
		t.Fatal(err)
	}
	killMobInSession(t, elsewhere, vnet.MobKindDraugrKing)
	m.Step()
	if again, _ := m.Bound(ruin, late); again != session.ID {
		t.Fatalf("a second copy moved the binding to %d, want the original %d", again, session.ID)
	}
}

// Every defeat is recorded by the species that identifies the encounter, in the order
// they died — which is what a "1 / 3" is counted from and what tells a restore which
// encounters to leave out. The snapshot hands back a copy.
func TestDefeatedEncountersAreRecordedByIdentity(t *testing.T) {
	m, ruin, character := instanceBindingSetup(t, 4)

	session, err := m.Reenter(ruin, character)
	if err != nil {
		t.Fatal(err)
	}
	killMobInSession(t, session, vnet.MobKindDraugrKing)
	m.Step()
	killMobInSession(t, session, vnet.MobKindVargrGuardian)
	// A second of a species already defeated adds no second encounter.
	killMobInSession(t, session, vnet.MobKindDraugrKing)
	m.Step()

	held, _ := m.Lookup(session.ID)
	want := []vnet.MobKind{vnet.MobKindDraugrKing, vnet.MobKindVargrGuardian}
	if !slices.Equal(held.DefeatedBosses, want) {
		t.Fatalf("defeated encounters = %v, want %v", held.DefeatedBosses, want)
	}

	held.DefeatedBosses[0] = vnet.MobKindUnknown
	again, _ := m.Lookup(session.ID)
	if !slices.Equal(again.DefeatedBosses, want) {
		t.Fatalf("editing a snapshot changed the session: %v", again.DefeatedBosses)
	}
	// The simulation's own list is a report, drained by the manager rather than kept.
	session.Sim.mu.Lock()
	pending := len(session.Sim.defeatedBosses)
	session.Sim.mu.Unlock()
	if pending != 0 {
		t.Fatalf("%d defeats were reported twice", pending)
	}
}

// Nothing a client can say saves a session or binds anybody. The manager offers no
// exported way to do either, and everything a connection *can* cause — entering,
// re-entering, leaving, being looked up, and killing whatever it likes — leaves both
// alone until a boss-rank creature dies.
func TestNoClientMessageBindsACharacter(t *testing.T) {
	m, ruin, character := instanceBindingSetup(t, 4)
	other := instanceTestCharacter(2)

	session, err := m.Reenter(ruin, character)
	if err != nil {
		t.Fatal(err)
	}

	assertNothingClaimed := func(when string) {
		t.Helper()
		held, live := m.Lookup(session.ID)
		if live && held.State != InstanceFree {
			t.Fatalf("%s left the session %v, want free", when, held.State)
		}
		if live && len(held.DefeatedBosses) != 0 {
			t.Fatalf("%s recorded defeated encounters %v", when, held.DefeatedBosses)
		}
		for _, who := range []InstanceCharacter{character, other} {
			if id, bound := m.Bound(ruin, who); bound {
				t.Fatalf("%s bound a character to session %d", when, id)
			}
		}
	}

	// The exported surface, driven with the arguments a connection would get to choose:
	// its own session ids, its own ruin, and as many repetitions as it likes.
	for range 3 {
		if _, err := m.Join(session.ID, character); err != nil {
			t.Fatal(err)
		}
		if _, err := m.Reenter(ruin, character); err != nil {
			t.Fatal(err)
		}
		m.Leave(session.ID, other)
		m.Leave(session.ID+1, character)
		if _, live := m.Lookup(session.ID + 1); live {
			t.Fatal("a made-up session id resolved")
		}
		m.Count()
		m.Step()
	}
	assertNothingClaimed("traffic on the exported surface")

	// And killing is not enough either: what saves a run is the *rank* the server's own
	// registry gives the creature, not that a player managed to kill something.
	killMobInSession(t, session, vnet.MobKindDraugr)
	killMobInSession(t, session, vnet.MobKindVargr)
	killMobInSession(t, session, vnet.MobKindDeer)
	m.Step()
	assertNothingClaimed("killing every ordinary species in the session")

	killMobInSession(t, session, vnet.MobKindDraugrKing)
	m.Step()
	if held, _ := m.Lookup(session.ID); held.State != InstanceSaved {
		t.Fatalf("the boss death left the session %v, want saved", held.State)
	}
}

// A binding names a session, so it cannot outlive one. Close is one of the two things
// that end a saved session; the midnight reset is the other, and is pinned by
// instance_reset_test.go.
func TestClosingReleasesEveryBinding(t *testing.T) {
	m, ruin, character := instanceBindingSetup(t, 4)

	session, err := m.Reenter(ruin, character)
	if err != nil {
		t.Fatal(err)
	}
	killMobInSession(t, session, vnet.MobKindVargrGuardian)
	m.Step()
	if _, bound := m.Bound(ruin, character); !bound {
		t.Fatal("the kill bound nobody")
	}

	m.Close()
	if id, bound := m.Bound(ruin, character); bound {
		t.Fatalf("a binding outlived its session, naming %d", id)
	}
}
