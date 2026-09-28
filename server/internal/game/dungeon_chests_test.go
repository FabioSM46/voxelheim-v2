package game

import (
	"reflect"
	"slices"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
	flatbuffers "github.com/google/flatbuffers/go"
)

func chestAt(t *testing.T, s *Sim, index int) world.PlacedAnchor {
	t.Helper()
	for _, a := range s.dungeon.gate.Chests() {
		if a.Index == index {
			return a
		}
	}
	t.Fatalf("chest %d absent", index)
	return world.PlacedAnchor{}
}

func joinChestPlayer(t *testing.T, s *Sim, n uint64, name string, a world.PlacedAnchor) (*Player, *dropSink) {
	t.Helper()
	out := &dropSink{}
	pos := [3]float32{float32(a.X) - .5, float32(a.Y), float32(a.Z) + .5}
	p, err := s.JoinCharacter(s.mintEntityID(), testPlayerID(n), 1, name, pos, testAppearance(), nil, out.deliver)
	if err != nil {
		t.Fatal(err)
	}
	return p, out
}

func chestRequest(a world.PlacedAnchor) [3]int32 {
	return [3]int32{int32(a.X), int32(a.Y), int32(a.Z)}
}

func requireChestUse(t *testing.T, p *Player, a world.PlacedAnchor) {
	t.Helper()
	if reason, err := p.UseMechanism(chestRequest(a)); err != nil || reason != vnet.RefusalReasonUnknown {
		t.Fatalf("chest %d use: %s, %v", a.Index, reason, err)
	}
}

func requireChestRefusal(t *testing.T, p *Player, a world.PlacedAnchor, want vnet.RefusalReason) {
	t.Helper()
	if reason, err := p.UseMechanism(chestRequest(a)); err == nil || reason != want {
		t.Fatalf("chest %d refused as %s, %v; want %s", a.Index, reason, err, want)
	}
}

func TestChestOpeningFreezesPersonalLootAndReopensOnlyTheRequester(t *testing.T) {
	d := newPuzzleDungeon(t, 1, DefaultTickRate, nil)
	s := d.s
	a := chestAt(t, s, world.AntechamberChest)
	first, firstOut := joinChestPlayer(t, s, 1, "Asta", a)
	second, secondOut := joinChestPlayer(t, s, 2, "Bera", a)
	offline, _ := joinChestPlayer(t, s, 3, "Cira", a)
	outsider, outsiderOut := joinChestPlayer(t, s, 4, "Duna", a)
	inviteAndAccept(t, first, second, second.name)
	inviteAndAccept(t, first, offline, offline.name)
	s.Leave(offline)
	// Eligibility is inside the run, not proximity. A distant member is not forced
	// into a loot window and can later walk to the opened block to collect their roll.
	place(s, second, [3]float64{first.pos[0] + 50, first.pos[1], first.pos[2]})
	s.durableBossRewards = true // ordinary chest loot must bypass the enabled boss journal
	s.loot = newLootRNG(71)
	wantRNG := &Sim{loot: newLootRNG(71)}
	wantFirst := wantRNG.rollTableLocked(chestLootTables[a.Index])
	wantSecond := wantRNG.rollTableLocked(chestLootTables[a.Index])
	requireChestUse(t, first, a)
	id := first.openLootID
	c := s.corpses[id]
	if c == nil || !c.chest || c.rewards != bossRewardsOpen || len(s.bossRewards) != 0 || len(c.personal) != 2 {
		t.Fatalf("opening did not make two ordinary personal containers: %+v", c)
	}
	if !reflect.DeepEqual(*c.personal[first.corpseOwner()], wantFirst) || !reflect.DeepEqual(*c.personal[second.corpseOwner()], wantSecond) {
		t.Fatal("personal rolls differ from roster order")
	}
	if second.openLootID != 0 || outsider.openLootID != 0 {
		t.Fatal("opening forced another player's window")
	}
	for _, out := range []*dropSink{secondOut, outsiderOut} {
		if states, _, _ := lootFrames(t, out); len(states) != 0 {
			t.Fatal("another player received unsolicited LootState")
		}
	}
	if states, _, _ := lootFrames(t, firstOut); len(states) != 1 {
		t.Fatalf("opener received %d loot states", len(states))
	}
	updates := 0
	for _, frame := range firstOut.all() {
		env := vnet.GetRootAsEnvelope(frame, 0)
		if env.PayloadType() != vnet.PayloadBlockUpdate {
			continue
		}
		var tab flatbuffers.Table
		if !env.Payload(&tab) {
			t.Fatal("block update missing body")
		}
		var block vnet.BlockUpdate
		block.Init(tab.Bytes, tab.Pos)
		if block.BlockId() != uint16(world.ChestOpen) {
			t.Fatal("opening announced the wrong block")
		}
		updates++
	}
	if updates != 1 || d.block(a) != world.ChestOpen {
		t.Fatal("opening did not publish exactly one open block")
	}
	for _, projected := range s.mobSnapshotsLocked(s.sortedMobsLocked()) {
		if projected.state.EntityID == id {
			t.Fatal("chest container fabricated a creature snapshot")
		}
	}
	requireChestRefusal(t, outsider, a, vnet.RefusalReasonChestAlreadyOpened)
	if reason, err := outsider.OpenLoot(protocol.LootOpenRequest{CorpseID: id, ClientTick: 1}); err == nil || reason != vnet.RefusalReasonLootNotOwned {
		t.Fatalf("outsider opened a guessed container id: %s, %v", reason, err)
	}
	requireChestRefusal(t, second, a, vnet.RefusalReasonOutOfReach)
	place(s, second, first.pos)
	requireChestUse(t, second, a)
	first.openLootID = 0 // the client may dismiss its panel between requests
	requireChestUse(t, first, a)
	if first.openLootID != id || second.openLootID != id || len(s.corpses) != 1 {
		t.Fatal("reopening minted or selected a different container")
	}
	if got, want := s.loot.Uint64(), wantRNG.loot.Uint64(); got != want {
		t.Fatal("reopening consumed RNG")
	}
	if _, err := first.TakeAllLoot(protocol.LootTakeAllRequest{CorpseID: id, Revision: 1, ClientTick: 1}); err != nil {
		t.Fatal(err)
	}
	if !c.personal[first.corpseOwner()].empty() || !reflect.DeepEqual(*c.personal[second.corpseOwner()], wantSecond) {
		t.Fatal("taking personal loot changed another member's share")
	}
	requireChestRefusal(t, first, a, vnet.RefusalReasonChestAlreadyOpened)
	// Joining the party after the opening never adds a retroactive share.
	inviteAndAccept(t, first, outsider, outsider.name)
	requireChestRefusal(t, outsider, a, vnet.RefusalReasonChestAlreadyOpened)
	if !slices.Equal(s.DungeonRoute().OpenedChests, []uint8{uint8(a.Index)}) {
		t.Fatal("opening was not recorded once")
	}
}

func TestKingChestLocksUntilDeathAndOrdinaryLootExpires(t *testing.T) {
	d := newPuzzleDungeon(t, 2, DefaultTickRate, []vnet.MobKind{vnet.MobKindVargrGuardian})
	s := d.s
	a := chestAt(t, s, world.KingChest)
	p, _ := joinChestPlayer(t, s, 1, "Asta", a)
	requireChestRefusal(t, p, a, vnet.RefusalReasonMechanismLocked)
	if len(s.corpses) != 0 || len(s.DungeonRoute().OpenedChests) != 0 || d.block(a) != world.Chest {
		t.Fatal("locked chest changed the world")
	}
	s.mu.Lock()
	king := s.mobs[s.dungeon.kingID]
	killed := s.damageMobLocked(king, king.health)
	s.mu.Unlock()
	if !killed {
		t.Fatal("king did not die from lethal authoritative damage")
	}
	requireChestUse(t, p, a)
	id := p.openLootID
	s.mu.Lock()
	s.expireCorpsesLocked(s.corpses[id].expiresTick)
	s.mu.Unlock()
	requireChestRefusal(t, p, a, vnet.RefusalReasonChestAlreadyOpened)
	if d.block(a) != world.ChestOpen || s.corpses[id] != nil {
		t.Fatal("expiration reset the opening or retained the ordinary container")
	}
}

func TestChestRefusalsAndBackpressure(t *testing.T) {
	d := newPuzzleDungeon(t, 0, DefaultTickRate, nil)
	s := d.s
	a := chestAt(t, s, world.SandHallChest)
	p, out := joinChestPlayer(t, s, 1, "Asta", a)
	p.lifeState = vnet.LifeStateDead
	requireChestRefusal(t, p, a, vnet.RefusalReasonPlayerIsDead)
	p.lifeState = vnet.LifeStateAlive
	out.setFull(true)
	requireChestUse(t, p, a)
	if !p.lootDirty {
		t.Fatal("backpressure lost the loot-state debt")
	}
	out.setFull(false)
	s.mu.Lock()
	s.flushDungeonGateLocked()
	p.offerLootLocked()
	s.mu.Unlock()
	if p.lootDirty {
		t.Fatal("loot-state debt survived successful retry")
	}
	if states, _, _ := lootFrames(t, out); len(states) != 1 {
		t.Fatalf("backpressure retry delivered %d loot states", len(states))
	}
	open := newDropHarness(t, openWorldChestTerrain{Terrain: dropTerrain{groundTop: 0}, at: [3]int64{a.X, a.Y, a.Z}})
	outside, _ := open.join(1, [3]float32{0.5, 1, .5})
	requireChestRefusal(t, outside, a, vnet.RefusalReasonNotAMechanism)
}

func TestOpenedChestsRestoreAsOpenWithoutRerollingOrdinaryLoot(t *testing.T) {
	at := time.Date(2026, 3, 14, 21, 0, 0, 0, time.UTC)
	manager, _, _, _, session := savedRunAt(t, at)
	loadDungeon(t, session)
	a := chestAt(t, session.Sim, world.AntechamberChest)
	p, _ := joinChestPlayer(t, session.Sim, 1, "Asta", a)
	requireChestUse(t, p, a)
	saved := manager.SavedSessions()
	if len(saved) != 1 || !slices.Equal(saved[0].Route.OpenedChests, []uint8{0}) {
		t.Fatalf("saved opening missing: %+v", saved)
	}
	// Copies crossing the persistence boundary cannot mutate the live opening set.
	saved[0].Route.OpenedChests[0] = 2
	if !slices.Equal(manager.SavedSessions()[0].Route.OpenedChests, []uint8{0}) {
		t.Fatal("saved chest list aliases the simulation")
	}
	saved = manager.SavedSessions()
	restarted, n, _ := restartInto(t, saved, at.Add(time.Minute))
	if n != 1 {
		t.Fatal("run did not restore")
	}
	live, ok := restarted.Lookup(session.ID)
	if !ok {
		t.Fatal("restored run absent")
	}
	loadDungeon(t, live)
	restored := &puzzleDungeon{t: t, s: live.Sim}
	if restored.block(a) != world.ChestOpen || len(live.Sim.corpses) != 0 {
		t.Fatal("restore closed the chest or recreated ephemeral loot")
	}
	back, _ := joinChestPlayer(t, live.Sim, 1, "Asta", a)
	requireChestRefusal(t, back, a, vnet.RefusalReasonChestAlreadyOpened)
	if !reflect.DeepEqual(restarted.SavedSessions()[0].Route, saved[0].Route) {
		t.Fatal("restore changed route progress")
	}
}

// The block may be present in an open-world fixture; only a dungeon anchor grants
// mechanism behavior, so a forged block id alone cannot create loot.
type openWorldChestTerrain struct {
	Terrain
	at [3]int64
}

func (w openWorldChestTerrain) Block(x, y, z int64) (world.Block, bool) {
	if [3]int64{x, y, z} == w.at {
		return world.Chest, true
	}
	return w.Terrain.Block(x, y, z)
}
