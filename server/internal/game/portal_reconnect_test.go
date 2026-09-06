package game

import (
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
	"sync"
	"testing"
)

func disconnectedPortal(t *testing.T) (*InstanceManager, *Sim, PortalEntry, Life) {
	t.Helper()
	m, open, request, join := portalHarness(t, 2)
	p := join()
	entry, reason := m.EnterPortal(p, request)
	if reason != 0 {
		t.Fatal(reason)
	}
	arrival, _ := world.InstanceAnchors(entry.Session.Seed)
	if err := open.Transfer(p, entry.Session.Sim, [3]float32{float32(arrival.X) + .5, float32(arrival.Y), float32(arrival.Z) + .5}); err != nil {
		t.Fatal(err)
	}
	entry.Session.Sim.mu.Lock()
	p.pos[0] += .123456789 // retaining the wire-rounded position would lose this
	p.yaw = 1.234
	p.experience = 555
	entry.Session.Sim.mu.Unlock()
	entry.Session.Sim.Leave(p)
	life := p.Record()
	m.DisconnectPortal(entry, life)
	return m, open, entry, life
}

func TestDisconnectedPortalRestoresExactLifeAndReservesLiveCopy(t *testing.T) {
	m, _, entry, life := disconnectedPortal(t)
	s, ok := m.Lookup(entry.Session.ID)
	if !ok || len(s.Members) != 0 || s.Sim.Count() != 0 {
		t.Fatal("disconnect retained occupancy")
	}
	m.Step()
	if m.sessions[s.ID].emptyTicks != 1 {
		t.Fatal("disconnected character stopped grace")
	}
	restored, visit, err := m.ResumePortal(entry.Character)
	if err != nil || visit == nil || restored == nil {
		t.Fatal("resume failed", err)
	}
	if *restored != life || visit.Session.ID != s.ID || visit.Session.Sim != s.Sim || visit.Session.Chunks != s.Chunks || visit.Return != entry.Return || visit.respawn != entry.respawn {
		t.Fatal("resume replaced life or instance")
	}
	m.Step()
	if m.sessions[s.ID].emptyTicks != 0 || len(m.sessions[s.ID].members) != 1 {
		t.Fatal("resume did not stop expiry")
	}
	if again, _, _ := m.ResumePortal(entry.Character); again != nil {
		t.Fatal("resume consumed twice")
	}
}

func TestDisconnectedPortalExpiryEvictsFullLifeAndResources(t *testing.T) {
	m, _, entry, _ := disconnectedPortal(t)
	if len(m.disconnected) != 1 {
		t.Fatal("fixture has no remembered life")
	}
	m.sessions[entry.Session.ID].emptyTicks = m.graceTicks - 1
	m.Step()
	if _, ok := m.Lookup(entry.Session.ID); ok || entry.Session.Context.Err() == nil {
		t.Fatal("offline member kept world alive")
	}
	if len(m.disconnected) != 0 || len(m.portalEntries) != 0 || len(m.visits) != 0 {
		t.Fatal("expired instance retained character snapshots or routes")
	}
	restored, visit, err := m.ResumePortal(entry.Character)
	if err != nil || restored != nil || visit != nil {
		t.Fatal("expired life restored instead of using the persisted open-world record", err)
	}
	if m.Count() != 0 {
		t.Fatal("resume allocated a new instance")
	}
}

func TestPortalReconnectIsScopedToBothAccountAndCharacterAndClearedOnRestart(t *testing.T) {
	m, _, entry, _ := disconnectedPortal(t)
	for _, key := range []InstanceCharacter{{PlayerID: entry.Character.PlayerID, CharacterID: entry.Character.CharacterID + 1}, {PlayerID: testPlayerID(999), CharacterID: entry.Character.CharacterID}} {
		if life, visit, err := m.ResumePortal(key); life != nil || visit != nil || err != nil {
			t.Fatal("another character inherited the visit")
		}
	}
	m.Close()
	if life, visit, _ := m.ResumePortal(entry.Character); life != nil || visit != nil {
		t.Fatal("closed manager kept reconnect life")
	}
	fresh, _, _, _ := portalHarness(t, 1)
	if life, visit, _ := fresh.ResumePortal(entry.Character); life != nil || visit != nil {
		t.Fatal("fresh manager inherited a session")
	}
}

func TestPortalAutosaveCapturesAllWorldsWithSafePositionsDuringTransfers(t *testing.T) {
	m, open, request, join := portalHarness(t, 2)
	p, other := join(), join()
	entry, reason := m.EnterPortal(p, request)
	if reason != 0 {
		t.Fatal(reason)
	}
	arrival, _ := world.InstanceAnchors(entry.Session.Seed)
	spawn := [3]float32{float32(arrival.X) + .5, float32(arrival.Y), float32(arrival.Z) + .5}
	var wg sync.WaitGroup
	wg.Add(1)
	go func() {
		defer wg.Done()
		for range 100 {
			if err := open.Transfer(p, entry.Session.Sim, spawn); err != nil {
				t.Error(err)
				return
			}
			if err := entry.Session.Sim.Transfer(p, open, entry.Return); err != nil {
				t.Error(err)
				return
			}
		}
	}()
	for range 100 {
		records := m.Records(open)
		if len(records) != 2 {
			t.Error("autosave lost or duplicated a transferred character")
		}
		life := records[entry.Character]
		for axis, value := range entry.Return {
			if life.Pos[axis] != float64(value) {
				t.Error("instance coordinate escaped persistence boundary")
			}
		}
		if _, ok := records[InstanceCharacter{other.playerID, other.characterID}]; !ok {
			t.Error("open world character omitted")
		}
	}
	wg.Wait()
	if err := open.Transfer(p, entry.Session.Sim, spawn); err != nil {
		t.Fatal(err)
	}
	p.sim.mu.Lock()
	p.experience = 4321
	p.sim.mu.Unlock()
	if life := m.Records(open)[entry.Character]; life.Experience != 4321 {
		t.Fatal("instance progress missing from autosave")
	}
}

func TestInstanceAutosaveWithoutKnownPortalNeverPublishesForeignCoordinates(t *testing.T) {
	m, open, _, join := portalHarness(t, 1)
	p := join()
	instance, err := m.Create(InstanceRuin{CellX: 10, CellZ: 20})
	if err != nil {
		t.Fatal(err)
	}
	if err := open.Transfer(p, instance.Sim, [3]float32{.5, 2, .5}); err != nil {
		t.Fatal(err)
	}
	if records := m.Records(open); len(records) != 0 {
		t.Fatal("unknown external transfer exposed instance coordinates")
	}
}

func TestInstanceExpiryEvictsOnlyItsDisconnectedLives(t *testing.T) {
	m, _, entry, life := disconnectedPortal(t)
	other, err := m.Create(InstanceRuin{CellX: 123, CellZ: 456})
	if err != nil {
		t.Fatal(err)
	}
	character := instanceTestCharacter(888)
	if _, err := m.Join(other.ID, character); err != nil {
		t.Fatal(err)
	}
	m.DisconnectPortal(PortalEntry{Session: other, Character: character}, life)
	m.sessions[entry.Session.ID].emptyTicks = m.graceTicks - 1
	m.Step()
	if _, found := m.disconnected[entry.Character]; found {
		t.Fatal("expired instance retained full life")
	}
	if len(m.disconnected) != 1 {
		t.Fatal("expiry discarded another live instance's reconnect")
	}
	if restored, visit, err := m.ResumePortal(character); err != nil || restored == nil || visit == nil || visit.Session.ID != other.ID {
		t.Fatal("other live instance no longer resumable", err)
	}
}
