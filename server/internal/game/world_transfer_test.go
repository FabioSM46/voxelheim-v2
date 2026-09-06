package game

import (
	"fmt"
	"log/slog"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func transferWorlds(t *testing.T) (*Sim, *Sim, func() uint64) {
	t.Helper()
	group := NewWorldGroup()
	mint := testEntityIDs()
	makeWorld := func() *Sim {
		cache := world.NewCache(1, 1, 8)
		sim, err := NewSim(20, 1, 1, dropTerrain{groundTop: 0}, cache, mint, slog.New(slog.DiscardHandler), WithWorldGroup(group))
		if err != nil {
			t.Fatal(err)
		}
		return sim
	}
	return makeWorld(), makeWorld(), mint
}

func TestPartySurvivesTransferWithoutForeignVitalsAndCanKickAcrossWorlds(t *testing.T) {
	a, b, mint := transferWorlds(t)
	join := func(name string) (*Player, *dropSink) {
		id := mint()
		out := &dropSink{}
		p, err := a.Join(id, testPlayerID(id), name, [3]float32{.5, 1, .5}, testAppearance(), nil, out.deliver)
		if err != nil {
			t.Fatal(err)
		}
		return p, out
	}
	first, _ := join("First")
	second, _ := join("Second")
	inviteAndAccept(t, first, second, "Second")
	partyID := first.partyID
	before := second.Record()
	id := second.entityID
	if err := a.Transfer(second, b, [3]float32{2.5, 1, .5}); err != nil {
		t.Fatal(err)
	}
	if second.partyID != partyID || first.partyID != partyID || second.entityID != id {
		t.Fatal("identity or party changed")
	}
	after := second.Record()
	if before.Health != after.Health || before.Hunger != after.Hunger || before.Slots != after.Slots {
		t.Fatal("transfer changed carried life")
	}
	for _, p := range []*Player{first, second} {
		p.sim.mu.Lock()
		_, vitals, roster := p.sim.partySnapshotLocked(p)
		p.sim.mu.Unlock()
		if len(vitals) != 0 || len(roster) != 2 {
			t.Fatalf("foreign vitals or lost membership: %+v %+v", vitals, roster)
		}
	}
	mustParty(t, first, vnet.PartyActionKick, "Second")
	if first.partyID != 0 || second.partyID != 0 {
		t.Fatal("remote kick left stale membership")
	}
	if err := b.Transfer(second, a, [3]float32{.5, 1, .5}); err != nil {
		t.Fatal(err)
	}
	if a.Count() != 2 || b.Count() != 0 {
		t.Fatal("reverse transfer left a ghost")
	}
}

func TestTransferClearsWorldEventsAndRejectsWithoutMutating(t *testing.T) {
	a, b, mint := transferWorlds(t)
	id := mint()
	out := &dropSink{}
	p, err := a.Join(id, testPlayerID(id), "Traveller", [3]float32{.5, 1, .5}, testAppearance(), nil, out.deliver)
	if err != nil {
		t.Fatal(err)
	}
	p.leaving = true
	if err = a.Transfer(p, b, [3]float32{.5, 1, .5}); err == nil {
		t.Fatal("leaving player moved")
	}
	if a.Count() != 1 || b.Count() != 0 || p.sim != a {
		t.Fatal("refusal changed ownership")
	}
	p.leaving = false
	p.openLootID = 99
	p.openVendorID = 98
	p.lootClosures = []uint64{97}
	p.vendorClosures = []uint64{96}
	if err = a.Transfer(p, b, [3]float32{.5, 1, .5}); err != nil {
		t.Fatal(err)
	}
	if p.openLootID != 0 || p.openVendorID != 0 || len(p.lootClosures) != 0 || len(p.vendorClosures) != 0 {
		t.Fatal("old-world event retained")
	}
	a.Leave(p)
	if b.Count() != 1 {
		t.Fatal("stale source teardown removed transferred player")
	}
	b.Leave(p)
	if b.Count() != 0 {
		t.Fatal("destination teardown retained player")
	}
}

// The same coordinates and deliberately different entities make a wrong world
// index observable in every snapshot vector, not just in player movement.
func TestTransferredPlayersSeeOnlyTheirWorldsMobsAndDrops(t *testing.T) {
	open, _, mint := transferWorlds(t)
	manager, err := NewInstanceManager(20, 1, 2, mint, slog.New(slog.DiscardHandler), WithWorldGroup(open.group))
	if err != nil {
		t.Fatal(err)
	}
	defer manager.Close()
	first, err := manager.Create(InstanceRuin{})
	if err != nil {
		t.Fatal(err)
	}
	second, err := manager.Create(InstanceRuin{})
	if err != nil {
		t.Fatal(err)
	}
	worlds := []*Sim{open, first.Sim, second.Sim}
	ids := make(map[uint64]bool)
	for i, sim := range worlds {
		id := mint()
		out := &dropSink{}
		p, e := open.Join(id, testPlayerID(id), fmt.Sprintf("Traveller %d", i), [3]float32{.5, 1, .5}, testAppearance(), nil, out.deliver)
		if e != nil {
			t.Fatal(e)
		}
		if sim != open {
			if e = open.Transfer(p, sim, [3]float32{.5, 1, .5}); e != nil {
				t.Fatal(e)
			}
		}
		sim.mu.Lock()
		mobID, ok := sim.spawnMobLocked(vnet.MobKindDraugr, [3]float64{5.5, 1, .5})
		sim.mu.Unlock()
		if !ok {
			t.Fatal("mob control failed")
		}
		dropID, ok := sim.spawnDrop(ItemStone, 1, [3]int64{6, 1, 0})
		if !ok {
			t.Fatal("drop control failed")
		}
		for _, entity := range []uint64{id, mobID, dropID} {
			if ids[entity] {
				t.Fatal("entity id reused across worlds")
			}
			ids[entity] = true
		}
	}
	// All worlds are populated before any snapshot. Each delivery sees exactly
	// one player, one mob, and one drop despite identical spatial coordinates.
	for _, sim := range worlds {
		sim.Step(1)
		for _, p := range sim.players {
			var frame []byte
			p.deliverSnapshot = func(f []byte, _ world.Column) bool { frame = f; return true }
			sim.Step(2)
			sink := &dropSink{frames: [][]byte{frame}}
			snap := newestSnapshot(t, sink)
			if snap.EntitiesLength() != 1 || snap.MobsLength() != 1 || snap.DropsLength() != 1 {
				t.Fatalf("foreign entities: players=%d mobs=%d drops=%d", snap.EntitiesLength(), snap.MobsLength(), snap.DropsLength())
			}
			var entity vnet.EntityState
			if !snap.Entities(&entity, 0) || entity.EntityId() != p.entityID {
				t.Fatal("foreign position or health")
			}
			var mob vnet.MobState
			if !snap.Mobs(&mob, 0) || sim.mobs[mob.EntityId()] == nil {
				t.Fatal("foreign mob")
			}
			var drop vnet.ItemDropState
			if !snap.Drops(&drop, 0) || sim.drops[drop.EntityId()] == nil {
				t.Fatal("foreign drop")
			}
		}
	}
}
