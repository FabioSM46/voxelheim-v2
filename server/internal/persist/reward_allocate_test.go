package persist

import (
	"errors"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func TestAllocateRunCarriesFrozenEntitlementsInOneWrite(t *testing.T) {
	players, dir := openStore(t)
	owner := newCharacter(t, players, testID(1), "Eivor")
	rewards, err := OpenRewardStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	who := SessionCharacter{PlayerID: owner.Owner, CharacterID: uint64(owner.ID)}
	record := SessionRecord{ID: 7, Seed: 19, Ruin: [2]int64{2, 3}, ExpiresUnix: 100,
		DefeatedBosses: []vnet.MobKind{vnet.MobKindVargrGuardian}, Bound: []SessionCharacter{who}}
	defeat := func(silver uint32, taken uint64) RewardDefeat {
		return RewardDefeat{Kind: vnet.MobKindVargrGuardian, Personal: []PersonalReward{{
			Owner: who, Entries: []protocol.InventoryStack{{ItemID: 1, Count: 2}}, Silver: silver, Taken: taken,
		}}}
	}

	for name, refused := range map[string][]RewardDefeat{
		"another boss":            {{Kind: vnet.MobKindDraugrKing}},
		"one defeat too many":     {defeat(30, 0), {Kind: vnet.MobKindDraugrKing}},
		"a consumed entitlement":  {defeat(30, 1)},
		"a consumed silver purse": {{Kind: vnet.MobKindVargrGuardian, Personal: []PersonalReward{{Owner: who, Silver: 30, SilverTaken: true}}}},
	} {
		if err := rewards.AllocateRun(players, 1, record, world.WorldgenVersion, refused...); !errors.Is(err, ErrRewardJournalConflict) {
			t.Errorf("%s = %v, want %v", name, err, ErrRewardJournalConflict)
		}
	}

	before, err := rewards.Snapshot()
	if err != nil {
		t.Fatal(err)
	}
	if err := rewards.AllocateRun(players, 1, record, world.WorldgenVersion, defeat(30, 0)); err != nil {
		t.Fatal(err)
	}
	got, err := rewards.Snapshot()
	if err != nil {
		t.Fatal(err)
	}
	if got.Revision != before.Revision+1 || got.NextGeneration != 2 || len(got.Runs) != 1 {
		t.Fatalf("journal = %+v, want one run in one write", got)
	}
	personal := got.Runs[0].Defeats[0].Personal
	if len(personal) != 1 || personal[0].Silver != 30 || len(personal[0].Entries) != 1 || personal[0].Owner != who {
		t.Fatalf("frozen entitlement = %+v", personal)
	}

	// The exact allocation is idempotent after an uncertain success; a different one is not.
	if err := rewards.AllocateRun(players, 1, record, world.WorldgenVersion, defeat(30, 0)); err != nil {
		t.Fatalf("repeating the exact allocation = %v", err)
	}
	if err := rewards.AllocateRun(players, 1, record, world.WorldgenVersion, defeat(31, 0)); !errors.Is(err, ErrRewardJournalConflict) {
		t.Fatalf("a different allocation under the same generation = %v", err)
	}
	if err := rewards.AllocateRun(players, 1, record, world.WorldgenVersion); !errors.Is(err, ErrRewardJournalConflict) {
		t.Fatalf("an entitlement-less allocation over an entitled run = %v", err)
	}
}

func TestExpireRunsRetainsARunStillOccupiedPastItsReset(t *testing.T) {
	_, rewards, _, _, _ := transitionFixture(t)
	if removed, err := rewards.ExpireRuns(200, 1); err != nil || removed != 0 {
		t.Fatalf("expiry of a retained run = %d, %v; want it kept", removed, err)
	}
	if removed, err := rewards.ExpireRuns(200, 5); err != nil || removed != 1 {
		t.Fatalf("expiry retaining another generation = %d, %v; want the run collected", removed, err)
	}
}
