package game

import (
	"errors"
	"log/slog"
	"slices"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// rewardDungeon is a dungeon whose guardian has been pulled by one character standing
// beside it, under a manager with durable boss rewards set as given.
func rewardDungeon(t *testing.T, durable bool) (*InstanceManager, InstanceSession, *Player, uint64) {
	t.Helper()
	m, err := NewInstanceManager(20, 3, 2, testEntityIDs(), slog.New(slog.DiscardHandler), WithDurableBossRewards(durable))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(m.Close)
	session, err := m.Reenter(InstanceRuin{}, instanceTestCharacter(1))
	if err != nil {
		t.Fatal(err)
	}
	loadDungeon(t, session)
	s := session.Sim
	id := s.dungeon.guardianID
	pos := s.mobs[id].pos
	spawn := [3]float32{float32(pos[0]), float32(pos[1]), float32(pos[2] + 1.5)}
	p, err := s.JoinCharacter(s.mintEntityID(), testPlayerID(1), 1, "Fighter", spawn, testAppearance(), nil, func([]byte) bool { return true })
	if err != nil {
		t.Fatal(err)
	}
	s.mu.Lock()
	s.startBossEncounterLocked(s.mobs[id], p)
	s.mu.Unlock()
	return m, session, p, id
}

func killRewardBoss(t *testing.T, session InstanceSession, id uint64) {
	t.Helper()
	s := session.Sim
	s.mu.Lock()
	defer s.mu.Unlock()
	boss := s.mobs[id]
	if boss == nil || !s.damageMobLocked(boss, boss.health) {
		t.Fatal("the boss survived its killing blow")
	}
}

func frozenContainer(t *testing.T, session InstanceSession, id uint64, p *Player) corpseContainer {
	t.Helper()
	session.Sim.mu.Lock()
	defer session.Sim.mu.Unlock()
	container, owned := session.Sim.corpses[id].containerFor(p)
	if !owned {
		t.Fatal("the killer owns no container")
	}
	return corpseContainer{entries: slices.Clone(container.entries), silver: container.silver, revision: container.revision}
}

func stacksOf(entries []corpseEntry) []protocol.InventoryStack {
	out := make([]protocol.InventoryStack, len(entries))
	for i, e := range entries {
		out[i] = protocol.InventoryStack{ItemID: uint16(e.stack.item), Count: e.stack.count, Durability: e.stack.durability, MaxDurability: e.stack.maxDurability}
	}
	return out
}

func TestADurableRewardDungeonHoldsBossLootUntilTheJournalReleasesIt(t *testing.T) {
	m, session, p, id := rewardDungeon(t, true)
	killRewardBoss(t, session, id)
	m.Step()
	rolled := frozenContainer(t, session, id, p)
	if len(rolled.entries) == 0 && rolled.silver == 0 {
		t.Fatal("the guardian rolled nothing to hold")
	}

	if reason, err := p.OpenLoot(protocol.LootOpenRequest{CorpseID: id, ClientTick: 1}); reason != vnet.RefusalReasonCorpseUnavailable || !errors.Is(err, errBossRewardHeld) {
		t.Fatalf("opening held loot = %s, %v", reason, err)
	}
	session.Sim.mu.Lock()
	canOpen := p.canOpenCorpseLocked(session.Sim.corpses[id])
	session.Sim.mu.Unlock()
	if canOpen {
		t.Fatal("a snapshot advertised held loot as openable")
	}

	saved := m.SavedSessions()
	if len(saved) != 1 || len(saved[0].PendingRewards) != 1 {
		t.Fatalf("saved runs = %+v, want one pending defeat", saved)
	}
	pending := saved[0].PendingRewards[0]
	if pending.Kind != vnet.MobKindVargrGuardian || len(pending.Personal) != 1 || pending.Personal[0].Owner != instanceTestCharacter(1) ||
		!slices.Equal(pending.Personal[0].Entries, stacksOf(rolled.entries)) || pending.Personal[0].Silver != rolled.silver {
		t.Fatalf("pending defeat = %+v, want the frozen roll %+v", pending, rolled)
	}

	otherRun := saved[0]
	otherRun.ExpiresUnix++
	if m.ReleaseBossRewards(otherRun, vnet.MobKindVargrGuardian) || m.ReleaseBossRewards(saved[0], vnet.MobKindDraugrKing) {
		t.Fatal("a release that names another run or defeat was accepted")
	}
	if !m.ReleaseBossRewards(saved[0], vnet.MobKindVargrGuardian) {
		t.Fatal("the durable defeat was not released")
	}
	if m.ReleaseBossRewards(saved[0], vnet.MobKindVargrGuardian) {
		t.Fatal("a defeat was released twice")
	}
	if pendingAfter := m.SavedSessions()[0].PendingRewards; len(pendingAfter) != 0 {
		t.Fatalf("a released defeat is still pending: %+v", pendingAfter)
	}

	if reason, err := p.OpenLoot(protocol.LootOpenRequest{CorpseID: id, ClientTick: 2}); err != nil {
		t.Fatalf("opening released loot = %s, %v", reason, err)
	}
	before := p.InventoryState()
	if _, err := p.TakeAllLoot(protocol.LootTakeAllRequest{CorpseID: id, Revision: rolled.revision, ClientTick: 3}); !errors.Is(err, ErrBossRewardClaimRequired) {
		t.Fatalf("take-all on claimed loot = %v", err)
	}
	if len(rolled.entries) > 0 {
		if _, err := p.TakeLoot(protocol.LootTakeRequest{CorpseID: id, EntryID: rolled.entries[0].entryID, Revision: rolled.revision, ClientTick: 4}); !errors.Is(err, ErrBossRewardClaimRequired) {
			t.Fatalf("take on claimed loot = %v", err)
		}
	}
	if after := p.InventoryState(); !slices.Equal(after.Stacks, before.Stacks) || after.Silver != before.Silver {
		t.Fatal("a take against claimed loot changed the pack")
	}

	all, reason, err := p.BossLootToClaim(id, rolled.revision, 0)
	if err != nil {
		t.Fatalf("selecting everything = %s, %v", reason, err)
	}
	wantIndices := make([]uint8, len(rolled.entries))
	for i, e := range rolled.entries {
		wantIndices[i] = uint8(e.entryID - 1)
	}
	if all.Kind != vnet.MobKindVargrGuardian || !all.Partial || all.Silver != rolled.silver ||
		!slices.Equal(all.Entries, stacksOf(rolled.entries)) || !slices.Equal(all.EntryIndices, wantIndices) {
		t.Fatalf("take-all selection = %+v", all)
	}
	if _, _, err := p.BossLootToClaim(id, rolled.revision+1, 0); err == nil {
		t.Fatal("a stale revision was selected")
	}

	if !p.ConsumeClaimedBossLoot(id, all.EntryIndices, all.Silver) {
		t.Fatal("an acknowledged claim could not be consumed")
	}
	left := frozenContainer(t, session, id, p)
	if len(left.entries) != 0 || left.silver != 0 || left.revision != rolled.revision+1 {
		t.Fatalf("container after consumption = %+v", left)
	}
	if p.ConsumeClaimedBossLoot(id, all.EntryIndices, all.Silver) {
		t.Fatal("consumed entries were consumed twice")
	}
}

func TestASingleBossEntrySelectionNamesItsRollIndex(t *testing.T) {
	m, session, p, id := rewardDungeon(t, true)
	killRewardBoss(t, session, id)
	m.Step()
	if !m.ReleaseBossRewards(m.SavedSessions()[0], vnet.MobKindVargrGuardian) {
		t.Fatal("release refused")
	}
	rolled := frozenContainer(t, session, id, p)
	if len(rolled.entries) == 0 {
		t.Skip("the guardian's roll holds no item entry")
	}
	if _, err := p.OpenLoot(protocol.LootOpenRequest{CorpseID: id, ClientTick: 1}); err != nil {
		t.Fatal(err)
	}
	last := rolled.entries[len(rolled.entries)-1]
	one, _, err := p.BossLootToClaim(id, rolled.revision, last.entryID)
	if err != nil || one.Partial || one.Silver != 0 || len(one.Entries) != 1 || !slices.Equal(one.EntryIndices, []uint8{uint8(last.entryID - 1)}) {
		t.Fatalf("single selection = %+v, %v", one, err)
	}
	if _, _, err := p.BossLootToClaim(id, rolled.revision, 99); err == nil {
		t.Fatal("an entry that is not there was selected")
	}
	if !p.ConsumeClaimedBossLoot(id, one.EntryIndices, 0) {
		t.Fatal("consuming one entry failed")
	}
	if left := frozenContainer(t, session, id, p); len(left.entries) != len(rolled.entries)-1 || left.silver != rolled.silver {
		t.Fatalf("container after one entry = %+v", left)
	}
}

// The personal roster is recorded as it was frozen at the pull, independently of who is
// bound to the run: a roster owner who never entered owns a roll and no binding.
func TestPendingBossRewardsFollowTheLootRosterNotTheBindings(t *testing.T) {
	m, session, _, id := rewardDungeon(t, true)
	absent := corpseOwner{playerID: testPlayerID(9), characterID: 9}
	session.Sim.mu.Lock()
	boss := session.Sim.mobs[id]
	boss.encounter.roster = append(slices.Clone(boss.encounter.roster), absent)
	session.Sim.mu.Unlock()
	killRewardBoss(t, session, id)
	m.Step()

	saved := m.SavedSessions()[0]
	var owners []InstanceCharacter
	for _, reward := range saved.PendingRewards[0].Personal {
		owners = append(owners, reward.Owner)
	}
	absentCharacter := InstanceCharacter{absent.playerID, absent.characterID}
	if !slices.Equal(owners, []InstanceCharacter{instanceTestCharacter(1), absentCharacter}) {
		t.Fatalf("personal owners = %+v, want the frozen roster", owners)
	}
	if slices.Contains(saved.Bound, absentCharacter) || !slices.Contains(saved.Bound, instanceTestCharacter(1)) {
		t.Fatalf("bindings = %+v, want only who was inside", saved.Bound)
	}
}

func TestBossLootStaysLiveWithoutDurableRewards(t *testing.T) {
	m, session, p, id := rewardDungeon(t, false)
	killRewardBoss(t, session, id)
	m.Step()
	if saved := m.SavedSessions(); len(saved) != 1 || len(saved[0].PendingRewards) != 0 {
		t.Fatalf("a live-loot run recorded pending rewards: %+v", saved)
	}
	rolled := frozenContainer(t, session, id, p)
	if _, err := p.OpenLoot(protocol.LootOpenRequest{CorpseID: id, ClientTick: 1}); err != nil {
		t.Fatalf("opening live loot: %v", err)
	}
	if _, err := p.TakeAllLoot(protocol.LootTakeAllRequest{CorpseID: id, Revision: rolled.revision, ClientTick: 2}); errors.Is(err, ErrBossRewardClaimRequired) {
		t.Fatal("live loot demanded a claim")
	}
	if _, _, err := p.BossLootToClaim(id, rolled.revision+1, 0); err == nil {
		t.Fatal("live loot produced a claim selection")
	}
	if p.ConsumeClaimedBossLoot(id, nil, 1) {
		t.Fatal("live loot was consumed as a claim")
	}
}

func TestAnOpenWorldBossIgnoresDurableRewards(t *testing.T) {
	sim, err := NewSim(DefaultTickRate, 8, testWorldSeed, dropTerrain{groundTop: 63}, refusedEdits{}, testEntityIDs(),
		slog.New(slog.DiscardHandler), WithDurableBossRewards(true))
	if err != nil {
		t.Fatal(err)
	}
	sim.mu.Lock()
	defer sim.mu.Unlock()
	id, made := sim.spawnMobLocked(vnet.MobKindVargrGuardian, [3]float64{0.5, 64, 0.5})
	if !made || !sim.damageMobLocked(sim.mobs[id], sim.mobs[id].health) {
		t.Fatal("could not kill an open-world boss")
	}
	if len(sim.bossRewards) != 0 || sim.corpses[id].rewards != bossRewardsOpen {
		t.Fatal("an open-world boss held its loot for a journal it has none of")
	}
}

// releasedGuardian kills and releases a durable guardian, leaving its loot claim-only.
func releasedGuardian(t *testing.T) (InstanceSession, *Player, uint64) {
	t.Helper()
	m, session, p, id := rewardDungeon(t, true)
	killRewardBoss(t, session, id)
	m.Step()
	if !m.ReleaseBossRewards(m.SavedSessions()[0], vnet.MobKindVargrGuardian) {
		t.Fatal("the durable defeat was not released")
	}
	return session, p, id
}

// A claim of silver alone follows the entries' rule: the purse must still hold exactly what
// was claimed, so the same silver cannot be consumed twice.
func TestAClaimOfSilverAloneIsConsumedOnce(t *testing.T) {
	session, p, id := releasedGuardian(t)
	session.Sim.mu.Lock()
	container, _ := session.Sim.corpses[id].containerFor(p)
	container.entries = nil
	container.silver = 25
	session.Sim.mu.Unlock()

	if p.ConsumeClaimedBossLoot(id, nil, 26) {
		t.Fatal("more silver than the purse holds was consumed")
	}
	if !p.ConsumeClaimedBossLoot(id, nil, 25) {
		t.Fatal("the claimed silver was not consumed")
	}
	if p.ConsumeClaimedBossLoot(id, nil, 25) {
		t.Fatal("the same silver was consumed twice")
	}
	if left := frozenContainer(t, session, id, p); left.silver != 0 {
		t.Fatalf("purse after consumption = %d, want 0", left.silver)
	}
	if p.ConsumeClaimedBossLoot(id, nil, 0) {
		t.Fatal("a consumption naming nothing was accepted")
	}
}

// A corpse that expired or was replaced before its claim was consumed is refused without
// touching anything.
func TestConsumingAMissingBossCorpseReportsFalse(t *testing.T) {
	session, p, id := releasedGuardian(t)
	session.Sim.mu.Lock()
	session.Sim.removeCorpseLocked(id)
	session.Sim.mu.Unlock()
	if p.ConsumeClaimedBossLoot(id, []uint8{0}, 0) || p.ConsumeClaimedBossLoot(id+1000, nil, 5) {
		t.Fatal("a missing corpse was consumed")
	}
}
