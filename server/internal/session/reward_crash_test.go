package session

import (
	"context"
	"path/filepath"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
)

// reopenAfterCrash opens the world's stores again the way startup does, recovering prepared
// rewards, and returns what the character record and the journal now hold.
func (w *rewardWorld) reopenAfterCrash(t *testing.T) (persist.Record, persist.RewardJournal, *persist.RewardStore) {
	t.Helper()
	dir := filepath.Dir(w.store.Dir())
	journal, err := persist.OpenRewardStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	players, err := persist.OpenStoreWithRewardRecovery(dir, journal, ValidateRewardRecord)
	if err != nil {
		t.Fatalf("startup recovery refused the world: %v", err)
	}
	rec, found, err := players.Load(w.character.ID)
	if err != nil || !found {
		t.Fatalf("the character after recovery = %v, %v", found, err)
	}
	snapshot, err := journal.Snapshot()
	if err != nil {
		t.Fatal(err)
	}
	return rec, snapshot, journal
}

// A process that stops at any point of a boss reward claim delivers it exactly once after a
// restart. Stopped before the intent is durable, nothing was delivered and the claim is still
// valid. Stopped at any later point, recovery applies the sealed postimage and marks the journal
// taken. A second restart changes nothing, and the entitlement cannot be claimed again.
func TestABossRewardInterruptedAtAnyStageIsDeliveredExactlyOnceAfterARestart(t *testing.T) {
	t.Parallel()
	for _, stage := range []string{"sealed", "prepared", "written", "published", "acknowledged"} {
		t.Run(stage, func(t *testing.T) {
			t.Parallel()
			w := newRewardWorld(t)
			reached, resume := w.pauseAt(t, stage)
			done, err := w.ids.ClaimBossReward(w.claim())
			if err != nil {
				t.Fatal(err)
			}
			awaitStage(t, reached)

			// The process stops here: the drain gives up on the paused claim and cancels the
			// coordinator, so the stage it was about to run writes nothing.
			ctx, cancel := context.WithTimeout(context.Background(), 20*time.Millisecond)
			defer cancel()
			if err := w.ids.DrainRewards(ctx); err == nil {
				t.Fatal("a drain over a paused claim reported success")
			}
			resume()
			if err := awaitReward(t, done); err == nil {
				t.Fatal("a claim stopped mid-way reported success")
			}

			rec, journal, reopened := w.reopenAfterCrash(t)
			defeat := journal.Runs[0].Defeats[0]
			delivered := stage != "sealed"
			switch {
			case len(journal.Intents) != 0:
				t.Fatalf("recovery left intents behind: %+v", journal.Intents)
			case delivered && (rec.BossRewardEpoch != 1 || rec.Silver != 40 || rec.Experience != 190 || rec.Slots[0] != rewardBones ||
				defeat.Personal[0].Taken != 1 || !defeat.Personal[0].SilverTaken || !defeat.Experience[0].Taken):
				t.Fatalf("stopped after sealing: record epoch %d silver %d experience %d slot %+v, journal %+v; want the reward exactly once",
					rec.BossRewardEpoch, rec.Silver, rec.Experience, rec.Slots[0], defeat)
			case !delivered && (rec.BossRewardEpoch != 0 || rec.Silver != 10 || rec.Experience != 100 ||
				defeat.Personal[0].Taken != 0 || defeat.Personal[0].SilverTaken || defeat.Experience[0].Taken):
				t.Fatalf("stopped before sealing: record epoch %d silver %d experience %d, journal %+v; want nothing delivered",
					rec.BossRewardEpoch, rec.Silver, rec.Experience, defeat)
			}

			again, journalAgain, _ := w.reopenAfterCrash(t)
			if again.BossRewardEpoch != rec.BossRewardEpoch || again.Silver != rec.Silver || again.Experience != rec.Experience || journalAgain.Revision != journal.Revision {
				t.Fatalf("a second restart changed the world: record %+v -> %+v, journal revision %d -> %d", rec, again, journal.Revision, journalAgain.Revision)
			}

			owner := persist.SessionCharacter{PlayerID: w.owner, CharacterID: uint64(w.character.ID)}
			retry := rec
			retry.BossRewardEpoch++
			intent := persist.RewardIntent{
				RewardReference: persist.RewardReference{Generation: 1, Boss: vnet.MobKindVargrGuardian, Owner: owner, Entries: 1, Silver: true, Experience: true},
				PreviousEpoch:   rec.BossRewardEpoch, Postimage: retry,
			}
			if err := reopened.ValidateClaim(intent); delivered == (err == nil) {
				t.Fatalf("claiming the entitlement again after the restart = %v; delivered %v", err, delivered)
			}
		})
	}
}
