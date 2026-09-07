package game

import (
	"fmt"
	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
	"log/slog"
	"math"
	"sync"
	"testing"
)

func portalHarness(t *testing.T, limit int) (*InstanceManager, *Sim, protocol.PortalRequest, func() *Player) {
	t.Helper()
	const seed = 0x5EED
	ruin, ok := world.RuinAt(seed, 0, 0)
	if !ok {
		t.Fatal("fixture ruin missing")
	}
	group := NewWorldGroup()
	mint := testEntityIDs()
	cache := world.NewCache(seed, 1, 8)
	sim, err := NewSim(20, 1, seed, dropTerrain{groundTop: 0}, cache, mint, slog.New(slog.DiscardHandler), WithWorldGroup(group))
	if err != nil {
		t.Fatal(err)
	}
	m, err := NewInstanceManager(20, 1, limit, mint, slog.New(slog.DiscardHandler), WithWorldGroup(group))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(m.Close)
	request := protocol.PortalRequest{HasArch: true, Arch: [3]int32{int32(ruin.Arch.X), int32(ruin.Arch.Y), int32(ruin.Arch.Z)}}
	join := func() *Player {
		id := mint()
		p, e := sim.JoinCharacter(id, testPlayerID(id), 1, fmt.Sprintf("Player%d", id), [3]float32{float32(ruin.Arch.X) + 1.5, float32(ruin.Arch.Y) - 1, float32(ruin.Arch.Z) + .5}, testAppearance(), nil, func([]byte) bool { return true })
		if e != nil {
			t.Fatal(e)
		}
		return p
	}
	return m, sim, request, join
}

func TestPortalAdmissionRejectsForgedAndUnusableAnchorsBeforeAllocating(t *testing.T) {
	m, sim, request, join := portalHarness(t, 2)
	p := join()
	for name, req := range map[string]protocol.PortalRequest{"missing": {}, "invented": {HasArch: true, Arch: [3]int32{request.Arch[0] + 1, request.Arch[1], request.Arch[2]}}, "overflow": {HasArch: true, Arch: [3]int32{math.MaxInt32, 0, 0}}, "distant": {HasArch: true, Arch: [3]int32{7091, 57, -93680}}} {
		t.Run(name, func(t *testing.T) {
			entry, reason := m.EnterPortal(p, req)
			if reason != vnet.RefusalReasonNotAtPortal || entry.Session.ID != 0 || m.Count() != 0 || sim.Count() != 1 {
				t.Fatalf("forged request admitted: %+v %v", entry, reason)
			}
		})
	}
	sim.mu.Lock()
	p.leaving = true
	sim.mu.Unlock()
	if _, reason := m.EnterPortal(p, request); reason != vnet.RefusalReasonNotAtPortal {
		t.Fatal("leaving player admitted")
	}
	sim.mu.Lock()
	p.leaving = false
	p.health = 0
	p.lifeState = vnet.LifeStateDead
	sim.mu.Unlock()
	if _, reason := m.EnterPortal(p, request); reason != vnet.RefusalReasonNotAtPortal {
		t.Fatal("dead player admitted")
	}
}

func TestPortalPartyAdmissionIsSerializedAndPrivate(t *testing.T) {
	m, _, request, join := portalHarness(t, 3)
	first, second, outsider := join(), join(), join()
	inviteAndAccept(t, first, second, second.name)
	var wg sync.WaitGroup
	results := make([]PortalEntry, 2)
	reasons := make([]vnet.RefusalReason, 2)
	for i, p := range []*Player{first, second} {
		wg.Add(1)
		go func() { defer wg.Done(); results[i], reasons[i] = m.EnterPortal(p, request) }()
	}
	wg.Wait()
	if reasons[0] != 0 || reasons[1] != 0 || results[0].Session.ID != results[1].Session.ID || m.Count() != 1 {
		t.Fatalf("party split: %+v %v", results, reasons)
	}
	private, reason := m.EnterPortal(outsider, request)
	if reason != 0 || private.Session.ID == results[0].Session.ID {
		t.Fatal("unrelated player joined party copy")
	}
	if !m.Leave(private.Session.ID, private.Character) {
		t.Fatal("leave failed")
	}
	again, reason := m.EnterPortal(outsider, request)
	if reason != 0 || again.Session.ID != private.Session.ID {
		t.Fatal("solo reentry lost retained copy")
	}
	// Route cleanup must not retain dead party ids or point to expired worlds.
	m.Leave(results[0].Session.ID, results[0].Character)
	m.Leave(results[1].Session.ID, results[1].Character)
	m.mu.Lock()
	m.sessions[results[0].Session.ID].emptyTicks = m.graceTicks - 1
	m.mu.Unlock()
	m.Step()
	if len(m.partyVisits) != 0 {
		t.Fatal("expired party route retained")
	}
}

func TestPortalCapacityAndCreationFailuresReturnNoWorldIdentity(t *testing.T) {
	m, _, request, join := portalHarness(t, 1)
	first, second := join(), join()
	admitted, reason := m.EnterPortal(first, request)
	if reason != 0 {
		t.Fatal(reason)
	}
	if admitted.Return != [3]float32{float32(request.Arch[0]) + 1.5, float32(request.Arch[1]) - 1, float32(request.Arch[2]) + .5} {
		t.Fatal("return is not authoritative standing position")
	}
	denied, reason := m.EnterPortal(second, request)
	if reason != vnet.RefusalReasonInstanceLimit || denied.Session.ID != 0 || denied.Session.Sim != nil || denied.Session.Chunks != nil || denied.Session.Members != nil {
		t.Fatal("cap refusal leaked world")
	}
	m.Close()
	denied, reason = m.EnterPortal(second, request)
	if reason != vnet.RefusalReasonInstanceUnavailable || denied.Session.ID != 0 {
		t.Fatal("creation refusal leaked world")
	}
}

func TestPortalExitRestoresOriginalFallbackRespawn(t *testing.T) {
	m, open, request, join := portalHarness(t, 1)
	p := join()
	original := [3]float64{200.5, 80, 400.5}
	open.mu.Lock()
	p.spawn = original
	open.mu.Unlock()
	entry, reason := m.EnterPortal(p, request)
	if reason != 0 {
		t.Fatal(reason)
	}
	arrival, _ := world.InstanceAnchors(entry.Session.Seed)
	if err := open.Transfer(p, entry.Session.Sim, [3]float32{float32(arrival.X) + .5, float32(arrival.Y), float32(arrival.Z) + .5}); err != nil {
		t.Fatal(err)
	}
	entry.RestoreRespawn(p)
	if p.spawn == original {
		t.Fatal("open-world fallback installed in instance")
	}
	if err := entry.Session.Sim.Transfer(p, open, entry.Return); err != nil {
		t.Fatal(err)
	}
	entry.RestoreRespawn(p)
	if p.spawn != original {
		t.Fatal("exit moved fallback respawn to the arch")
	}
	if p.State().Pos != entry.Return {
		t.Fatal("restoring respawn moved the standing body")
	}
}
