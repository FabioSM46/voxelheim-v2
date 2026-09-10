package game

import (
	"errors"
	"log/slog"
	"slices"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func restoreHeldRun(t *testing.T, held ...BossRewardDefeat) (*InstanceManager, error) {
	t.Helper()
	m, err := NewInstanceManager(20, 3, 2, testEntityIDs(), slog.New(slog.DiscardHandler), WithDurableBossRewards(true))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(m.Close)
	run := SavedSession{ID: 9, Seed: 19, Ruin: InstanceRuin{CellX: 2, CellZ: 3}, ExpiresUnix: time.Now().Add(time.Hour).Unix(),
		DefeatedBosses: []vnet.MobKind{vnet.MobKindVargrGuardian}, Bound: []InstanceCharacter{instanceTestCharacter(1)}, Generation: 1, HeldRewards: held}
	_, _, err = m.RestoreSessions([]SavedSession{run})
	return m, err
}

// A restore offers held boss loot again as a corpse whose loot only a claim delivers, with the
// frozen roll in roll order, and that corpse lasts until the run resets.
func TestARestoreOffersHeldBossLootAgainAsAClaimedCorpse(t *testing.T) {
	bones := protocol.InventoryStack{ItemID: uint16(ItemBone), Count: 2}
	pelt := protocol.InventoryStack{ItemID: uint16(ItemVargrPelt), Count: 3}
	partner := instanceTestCharacter(2)
	held := BossRewardDefeat{Kind: vnet.MobKindVargrGuardian, Personal: []BossPersonalReward{
		{Owner: instanceTestCharacter(1), Entries: []protocol.InventoryStack{bones, pelt}, Silver: 30},
		{Owner: partner, Entries: []protocol.InventoryStack{bones, pelt}, Silver: 30, Taken: 0b01, SilverTaken: true},
	}}
	m, err := restoreHeldRun(t, held)
	if err != nil {
		t.Fatal(err)
	}
	session, _ := m.Lookup(9)
	loadDungeon(t, session)
	s := session.Sim
	guardian, _, _ := world.InstanceEncounterAnchors(19)
	home := [3]float64{float64(guardian.X) + .5, float64(guardian.Y), float64(guardian.Z) + .5}
	s.mu.Lock()
	var corpseID uint64
	var rewards bossRewardHold
	var pos [3]float64
	for id, c := range s.corpses {
		corpseID, rewards, pos = id, c.rewards, c.pos
	}
	var remainder corpseContainer
	if c := s.corpses[corpseID]; c != nil {
		if owed := c.personal[corpseOwner{playerID: partner.PlayerID, characterID: partner.CharacterID}]; owed != nil {
			remainder = corpseContainer{entries: slices.Clone(owed.entries), silver: owed.silver}
		}
	}
	count := len(s.corpses)
	s.mu.Unlock()
	if count != 1 || rewards != bossRewardsClaimed || pos != home {
		t.Fatalf("restored corpses = %d, hold %d at %v; want one claimed corpse at %v", count, rewards, pos, home)
	}
	if len(remainder.entries) != 1 || remainder.entries[0].entryID != 2 || stacksOf(remainder.entries)[0] != pelt || remainder.silver != 0 {
		t.Fatalf("the partly taking owner's container = %+v; want only the pelt at its roll index and no silver", remainder)
	}

	spawn := [3]float32{float32(pos[0]), float32(pos[1]), float32(pos[2] + 1.5)}
	p, err := s.JoinCharacter(s.mintEntityID(), testPlayerID(1), 1, "Fighter", spawn, testAppearance(), nil, func([]byte) bool { return true })
	if err != nil {
		t.Fatal(err)
	}
	if reason, err := p.OpenLoot(protocol.LootOpenRequest{CorpseID: corpseID, ClientTick: 1}); err != nil {
		t.Fatalf("opening re-offered loot = %s, %v", reason, err)
	}
	if _, err := p.TakeLoot(protocol.LootTakeRequest{CorpseID: corpseID, EntryID: 1, Revision: 1, ClientTick: 2}); !errors.Is(err, ErrBossRewardClaimRequired) {
		t.Fatalf("a take on re-offered loot = %v, want %v", err, ErrBossRewardClaimRequired)
	}
	all, reason, err := p.BossLootToClaim(corpseID, 1, 0)
	if err != nil || !slices.Equal(all.Entries, held.Personal[0].Entries) || !slices.Equal(all.EntryIndices, []uint8{0, 1}) || all.Silver != 30 {
		t.Fatalf("selection = %+v, %s, %v; want the frozen roll at indices 0 and 1 with 30 silver", all, reason, err)
	}

	s.mu.Lock()
	s.expireCorpsesLocked(s.currentTick + s.corpseLifetimeTicks + 1)
	_, kept := s.corpses[corpseID]
	s.mu.Unlock()
	if !kept {
		t.Fatal("re-offered loot expired before the run reset")
	}
	if pending := m.SavedSessions()[0].PendingRewards; len(pending) != 0 {
		t.Fatalf("re-offered loot was reported pending again: %+v", pending)
	}
}

// Held loot a restore could not rebuild exactly refuses the whole restore, so nothing is built.
func TestARestoreRefusesHeldLootItCannotRebuild(t *testing.T) {
	owner := instanceTestCharacter(1)
	bones := []protocol.InventoryStack{{ItemID: uint16(ItemBone), Count: 2}}
	for name, held := range map[string]BossRewardDefeat{
		"a boss the run never defeated": {Kind: vnet.MobKindDraugrKing, Personal: []BossPersonalReward{{Owner: owner, Entries: bones}}},
		"an item no pack could hold":    {Kind: vnet.MobKindVargrGuardian, Personal: []BossPersonalReward{{Owner: owner, Entries: []protocol.InventoryStack{{ItemID: 65535, Count: 1}}}}},
		"one owner named twice":         {Kind: vnet.MobKindVargrGuardian, Personal: []BossPersonalReward{{Owner: owner, Entries: bones}, {Owner: owner, Entries: bones}}},
		"no loot at all":                {Kind: vnet.MobKindVargrGuardian},
		"a taken index past the roll":   {Kind: vnet.MobKindVargrGuardian, Personal: []BossPersonalReward{{Owner: owner, Entries: bones, Taken: 0b10}}},
		"taken silver never rolled":     {Kind: vnet.MobKindVargrGuardian, Personal: []BossPersonalReward{{Owner: owner, Entries: bones, SilverTaken: true}}},
	} {
		m, err := restoreHeldRun(t, held)
		if !errors.Is(err, ErrInvalidSession) || m.Count() != 0 {
			t.Errorf("%s: restore = %v with %d sessions, want %v and none", name, err, m.Count(), ErrInvalidSession)
		}
	}
}
