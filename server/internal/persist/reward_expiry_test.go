package persist

import (
	"errors"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func TestExpireRunsCollectsResetRunsButNeverAPendingIntentOrTheHighWater(t *testing.T) {
	players, rewards, dir, owner, ref := transitionFixture(t)
	later := SessionRecord{ID: 8, Seed: 20, Ruin: [2]int64{4, 5}, ExpiresUnix: 500}
	if err := rewards.AllocateRun(players, 2, later, world.WorldgenVersion); err != nil {
		t.Fatal(err)
	}

	// A prepared claim on the reset run keeps that run through its reset.
	token, _ := prepareTransition(t, players, rewards, owner, ref)
	if removed, err := rewards.ExpireRuns(100); err != nil || removed != 0 {
		t.Fatalf("expiry with a pending intent removed %d runs: %v", removed, err)
	}
	if err := players.WritePreparedReward(token); err != nil {
		t.Fatal(err)
	}
	if err := rewards.AcknowledgeClaim(players, token, ref); err != nil {
		t.Fatal(err)
	}

	if removed, err := rewards.ExpireRuns(99); err != nil || removed != 0 {
		t.Fatalf("a run was collected before its reset: %d, %v", removed, err)
	}
	if removed, err := rewards.ExpireRuns(100); err != nil || removed != 1 {
		t.Fatalf("the reset run was not collected: %d, %v", removed, err)
	}

	reopened, err := OpenRewardStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	journal, err := reopened.Snapshot()
	if err != nil {
		t.Fatal(err)
	}
	if len(journal.Runs) != 1 || journal.Runs[0].Generation != 2 || journal.NextGeneration != 3 {
		t.Fatalf("journal after collection = %+v", journal)
	}
	if err := reopened.AllocateRun(players, 1, SessionRecord{ID: 9, Seed: 21, Ruin: [2]int64{6, 7}, ExpiresUnix: 900}, world.WorldgenVersion); !errors.Is(err, ErrRewardJournalConflict) {
		t.Fatalf("a collected generation was issued again: %v", err)
	}
	if removed, err := reopened.ExpireRuns(100); err != nil || removed != 0 {
		t.Fatalf("a second collection pass = %d, %v; want nothing", removed, err)
	}
	var missing *RewardStore
	if _, err := missing.ExpireRuns(100); !errors.Is(err, ErrRewardJournalConflict) {
		t.Fatalf("a missing journal = %v", err)
	}
}
