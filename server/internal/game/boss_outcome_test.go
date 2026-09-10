package game

import (
	"log/slog"
	"slices"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// A saved run reports a boss's defeat and the reward it froze in the same snapshot, from the
// first tick after the kill. The reward sync journals a defeat together with what it owes,
// and that is sound only because the two are never observed apart. A restore brings back the
// defeat and none of the held reward, because it rebuilds no corpse that could hold one.
func TestASavedRunReportsABossDefeatTogetherWithItsFrozenReward(t *testing.T) {
	m, session, _, id := rewardDungeon(t, true)
	killRewardBoss(t, session, id)
	m.Step()
	saved := m.SavedSessions()
	if len(saved) != 1 {
		t.Fatalf("saved runs = %+v, want the run the kill saved", saved)
	}
	run := saved[0]
	guardian := []vnet.MobKind{vnet.MobKindVargrGuardian}
	if !slices.Equal(run.DefeatedBosses, guardian) || len(run.PendingRewards) != 1 || run.PendingRewards[0].Kind != vnet.MobKindVargrGuardian {
		t.Fatalf("one snapshot = defeats %v, pending %+v; want the guardian in both", run.DefeatedBosses, run.PendingRewards)
	}

	restored, err := NewInstanceManager(20, 3, 2, testEntityIDs(), slog.New(slog.DiscardHandler), WithDurableBossRewards(true))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(restored.Close)
	run.Generation = 1
	if _, _, err := restored.RestoreSessions([]SavedSession{run}); err != nil {
		t.Fatal(err)
	}
	back := restored.SavedSessions()
	if len(back) != 1 || !slices.Equal(back[0].DefeatedBosses, guardian) || len(back[0].PendingRewards) != 0 {
		t.Fatalf("restored run = %+v, want its defeat and no held reward", back)
	}
}
