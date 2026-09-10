package persist

import (
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// One collection keeps a run that is still occupied past its reset and a run whose reset has
// passed with a claim still prepared on it, for their two different reasons. Each is collected
// only once its own reason ends, and a collected generation is never allocated again, even after
// a reopen.
func TestExpireRunsKeepsARetainedRunAndAPreparedClaimInOnePass(t *testing.T) {
	players, rewards, dir, owner, ref := transitionFixture(t)
	later := SessionRecord{ID: 8, Seed: 20, Ruin: [2]int64{4, 5}, ExpiresUnix: 200}
	if err := rewards.AllocateRun(players, 2, later, world.WorldgenVersion); err != nil {
		t.Fatal(err)
	}
	token, _ := prepareTransition(t, players, rewards, owner, ref)

	if removed, err := rewards.ExpireRuns(300, 2); err != nil || removed != 0 {
		t.Fatalf("a pass with one run retained and one holding a prepared claim removed %d: %v", removed, err)
	}
	if err := players.WritePreparedReward(token); err != nil {
		t.Fatal(err)
	}
	if err := rewards.AcknowledgeClaim(players, token, ref); err != nil {
		t.Fatal(err)
	}
	if removed, err := rewards.ExpireRuns(300, 2); err != nil || removed != 1 {
		t.Fatalf("after the claim landed, the pass removed %d: %v; want only its run", removed, err)
	}
	if journal, err := rewards.Snapshot(); err != nil || len(journal.Runs) != 1 || journal.Runs[0].Generation != 2 {
		t.Fatalf("journal after the first collection = %+v, %v; want only the retained run", journal, err)
	}
	if removed, err := rewards.ExpireRuns(300); err != nil || removed != 1 {
		t.Fatalf("once released, the retained run was not collected: %d, %v", removed, err)
	}

	reopened, err := OpenRewardStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	journal, err := reopened.Snapshot()
	if err != nil || len(journal.Runs) != 0 || journal.NextGeneration != 3 {
		t.Fatalf("reopened journal = %+v, %v; want no runs and the high water kept", journal, err)
	}
	fresh := SessionRecord{ID: 9, Seed: 21, Ruin: [2]int64{6, 7}, ExpiresUnix: 400}
	for _, generation := range []uint64{1, 2} {
		if err := reopened.AllocateRun(players, generation, fresh, world.WorldgenVersion); err == nil {
			t.Fatalf("collected generation %d was allocated again", generation)
		}
	}
	if err := reopened.AllocateRun(players, 3, fresh, world.WorldgenVersion); err != nil {
		t.Fatalf("the next generation was refused: %v", err)
	}
}
