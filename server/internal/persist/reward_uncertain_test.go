package persist

import (
	"errors"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func TestUncertainReportsAWriteWhoseOutcomeIsUnknownUntilItLands(t *testing.T) {
	players, dir := openStore(t)
	rewards, err := OpenRewardStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	failing := errors.New("the disk refused the write")
	rewards.writeAtomic = func(string, []byte) error { return failing }
	record := SessionRecord{ID: 7, Seed: 19, Ruin: [2]int64{2, 3}, ExpiresUnix: 100}
	if rewards.Uncertain() {
		t.Fatal("a fresh journal reported an uncertain write")
	}
	if err := rewards.AllocateRun(players, 1, record, world.WorldgenVersion); !errors.Is(err, failing) {
		t.Fatalf("the failed allocation = %v", err)
	}
	if !rewards.Uncertain() {
		t.Fatal("a failed write was not reported uncertain")
	}
	other := record
	other.ID, other.Seed = 8, 20
	if err := rewards.AllocateRun(players, 1, other, world.WorldgenVersion); !errors.Is(err, ErrRewardJournalConflict) {
		t.Fatalf("other bytes over an uncertain write = %v", err)
	}
	rewards.writeAtomic = world.WriteAtomic
	if err := rewards.AllocateRun(players, 1, record, world.WorldgenVersion); err != nil {
		t.Fatalf("the identical retry = %v", err)
	}
	if rewards.Uncertain() {
		t.Fatal("a landed retry left the journal uncertain")
	}
	var missing *RewardStore
	if missing.Uncertain() {
		t.Fatal("a missing journal reported an uncertain write")
	}
}
