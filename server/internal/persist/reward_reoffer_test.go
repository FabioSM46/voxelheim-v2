package persist

import (
	"slices"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

func untouchedKinds(defeats []RewardDefeat) []vnet.MobKind {
	kinds := make([]vnet.MobKind, len(defeats))
	for i, d := range defeats {
		kinds[i] = d.Kind
	}
	return kinds
}

// Only a defeat nobody has taken anything from, and that no prepared intent names, is loot a
// restart may offer again.
func TestUntouchedLootIsOnlyWhatNothingHasBeenDeliveredFrom(t *testing.T) {
	owner := SessionCharacter{CharacterID: 1}
	other := SessionCharacter{CharacterID: 2}
	bones := protocol.InventoryStack{ItemID: 7, Count: 2}
	guardian, king := vnet.MobKindVargrGuardian, vnet.MobKindDraugrKing
	untouched := RewardDefeat{Kind: guardian, Personal: []PersonalReward{{Owner: owner, Entries: []protocol.InventoryStack{bones}, Silver: 30}, {Owner: other, Silver: 5}},
		Experience: []BossExperienceReward{{Owner: owner, Amount: 60, Taken: true}}}
	j := RewardJournal{NextGeneration: 5, Runs: []RewardRun{
		{Generation: 1, Defeats: []RewardDefeat{untouched, {Kind: king, Personal: []PersonalReward{{Owner: owner, Entries: []protocol.InventoryStack{bones}}, {Owner: other, Entries: []protocol.InventoryStack{bones}, Taken: 1}}}}},
		{Generation: 2, Defeats: []RewardDefeat{{Kind: guardian, Personal: []PersonalReward{{Owner: owner, Silver: 30, SilverTaken: true}}}}},
		{Generation: 3, Defeats: []RewardDefeat{{Kind: guardian, Experience: []BossExperienceReward{{Owner: owner, Amount: 60}}}}},
		{Generation: 4, Defeats: []RewardDefeat{untouched}},
	}}
	j.Intents = []RewardIntent{{RewardReference: RewardReference{Generation: 4, Boss: guardian, Owner: other, Silver: true}}}

	for _, c := range []struct {
		name       string
		generation uint64
		want       []vnet.MobKind
	}{
		{"untouched guardian beside a king one owner took from; experience does not count", 1, []vnet.MobKind{guardian}},
		{"silver taken", 2, nil},
		{"no held loot", 3, nil},
		{"a prepared intent names it", 4, nil},
		{"no such run", 9, nil},
	} {
		if got := untouchedKinds(j.UntouchedLoot(c.generation)); !slices.Equal(got, c.want) && (len(got) != 0 || len(c.want) != 0) {
			t.Errorf("%s: untouched = %v, want %v", c.name, got, c.want)
		}
	}
}
