package persist

import (
	"slices"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

type owedOwner struct {
	character   uint64
	taken       uint64
	silverTaken bool
}

func owedOwners(defeats []RewardDefeat) map[vnet.MobKind][]owedOwner {
	out := make(map[vnet.MobKind][]owedOwner, len(defeats))
	for _, d := range defeats {
		for _, p := range d.Personal {
			out[d.Kind] = append(out[d.Kind], owedOwner{p.Owner.CharacterID, p.Taken, p.SilverTaken})
		}
	}
	return out
}

// A restart offers every owner exactly what the journal still owes: an untouched owner's whole
// roll, a partly taking owner's remainder with its taken record, and nothing for an owner who
// took everything or whom a prepared intent names.
func TestOwedLootIsWhatEachOwnerHasNotTaken(t *testing.T) {
	untouched := SessionCharacter{CharacterID: 1}
	partly := SessionCharacter{CharacterID: 2}
	done := SessionCharacter{CharacterID: 3}
	pending := SessionCharacter{CharacterID: 4}
	stacks := []protocol.InventoryStack{{ItemID: 7, Count: 2}, {ItemID: 8, Count: 1}}
	guardian, king := vnet.MobKindVargrGuardian, vnet.MobKindDraugrKing
	j := RewardJournal{NextGeneration: 4, Runs: []RewardRun{
		{Generation: 1, Defeats: []RewardDefeat{
			{Kind: guardian, Personal: []PersonalReward{
				{Owner: untouched, Entries: stacks, Silver: 30},
				{Owner: partly, Entries: stacks, Silver: 30, Taken: 0b01, SilverTaken: true},
				{Owner: done, Entries: stacks, Silver: 30, Taken: 0b11, SilverTaken: true},
				{Owner: pending, Entries: stacks},
			}, Experience: []BossExperienceReward{{Owner: untouched, Amount: 60}}},
			{Kind: king, Personal: []PersonalReward{{Owner: done, Entries: stacks[:1], Taken: 0b1}, {Owner: partly, Silver: 5, SilverTaken: true}}},
		}},
		{Generation: 2, Defeats: []RewardDefeat{{Kind: guardian, Personal: []PersonalReward{{Owner: partly, Silver: 30}}}}},
		{Generation: 3, Defeats: []RewardDefeat{{Kind: guardian, Experience: []BossExperienceReward{{Owner: untouched, Amount: 60}}}}},
	}}
	j.Intents = []RewardIntent{{RewardReference: RewardReference{Generation: 1, Boss: guardian, Owner: pending, Entries: 0b1}}}

	got := owedOwners(j.OwedLoot(1))
	want := []owedOwner{{1, 0, false}, {2, 0b01, true}}
	if !slices.Equal(got[guardian], want) || len(got[king]) != 0 || len(got) != 1 {
		t.Fatalf("owed in run 1 = %+v, want only the guardian's untouched owner and the partly taking owner %+v", got, want)
	}
	for _, d := range j.OwedLoot(1) {
		if len(d.Experience) != 0 {
			t.Fatalf("owed loot carried experience: %+v", d.Experience)
		}
	}
	if got := owedOwners(j.OwedLoot(2)); !slices.Equal(got[guardian], []owedOwner{{2, 0, false}}) {
		t.Fatalf("owed in run 2 = %+v, want the untaken silver", got)
	}
	for _, generation := range []uint64{3, 9} {
		if owed := j.OwedLoot(generation); len(owed) != 0 {
			t.Fatalf("owed in run %d = %+v, want nothing", generation, owed)
		}
	}
}
