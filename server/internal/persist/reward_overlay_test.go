package persist

import (
	"math"
	"reflect"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func TestRewardOverlayExpiresLiveRunsWithoutDroppingPendingRewards(t *testing.T) {
	players, rewards, _, owner, ref := transitionFixture(t)
	_, _ = prepareTransition(t, players, rewards, owner, ref)
	before, _ := rewards.Snapshot()
	// Same transient ID reused after a cold start, while an expired generation retains XP.
	rec := SessionRecord{ID: 7, Seed: 20, Ruin: [2]int64{3, 4}, ExpiresUnix: 200}
	if err := rewards.AllocateRun(players, 2, rec, world.WorldgenVersion); err != nil {
		t.Fatal(err)
	}
	live, err := rewards.OverlaySessions(nil, 100, world.WorldgenVersion)
	if err != nil || len(live) != 1 || live[0].Generation != 2 {
		t.Fatal("expired retained generation blocked live reuse", err)
	}
	after, _ := rewards.Snapshot()
	if !reflect.DeepEqual(before.Intents, after.Intents) || !reflect.DeepEqual(before.Runs[0], after.Runs[0]) {
		t.Fatal("live expiry erased pending items or XP")
	}
	// Callers own their returned slices.
	live[0].Session.Bound = append(live[0].Session.Bound, SessionCharacter{PlayerID: testID(9), CharacterID: 9})
	after, _ = rewards.Snapshot()
	if len(after.Runs[1].Session.Bound) != 0 {
		t.Fatal("overlay aliases mutable journal data")
	}
}
func TestRewardOverlayRefusesAmbiguousOrUnboundedRestoration(t *testing.T) {
	for _, kind := range []string{"content", "duplicate-key", "zero-id", "too-many", "duplicate-generation-key", "id-overflow"} {
		t.Run(kind, func(t *testing.T) {
			players, rewards, _, _, _ := transitionFixture(t)
			saved := []SessionRecord(nil)
			content := uint32(world.WorldgenVersion)
			switch kind {
			case "content":
				content++
			case "duplicate-key":
				r := SessionRecord{ID: 9, Seed: 99, ExpiresUnix: 100}
				saved = []SessionRecord{r, r}
			case "zero-id":
				saved = []SessionRecord{{Seed: 99, ExpiresUnix: 100}}
			case "too-many":
				saved = make([]SessionRecord, MaxSavedSessions+1)
			case "duplicate-generation-key":
				snap, _ := rewards.Snapshot()
				r := snap.Runs[0].Session
				r.ID = 8
				if err := rewards.AllocateRun(players, 2, r, content); err != nil {
					t.Fatal(err)
				}
			case "id-overflow":
				saved = []SessionRecord{{ID: 7, Seed: 99, ExpiresUnix: 100}, {ID: math.MaxUint64, Seed: 100, ExpiresUnix: 100}}
			}
			if _, err := rewards.OverlaySessions(saved, 50, content); err == nil {
				t.Fatal("invalid restoration accepted")
			}
		})
	}
}

// The journal knows what a run killed and never where its party stands, so a run the
// journal overlays keeps the route progress sessions.bin recorded for it — and a copy
// of it, so the caller cannot reach back into the file's lists.
func TestRewardOverlayKeepsTheRouteProgressOfAJournaledRun(t *testing.T) {
	players, rewards, _, _, _ := transitionFixture(t)
	rec := SessionRecord{ID: 7, Seed: 20, Ruin: [2]int64{3, 4}, ExpiresUnix: 200}
	if err := rewards.AllocateRun(players, 2, rec, world.WorldgenVersion); err != nil {
		t.Fatal(err)
	}
	saved := rec
	saved.Checkpoints, saved.SolvedPuzzles, saved.ClearedGroups = 2, []uint8{1, 3}, []uint8{0, 4}
	live, err := rewards.OverlaySessions([]SessionRecord{saved}, 100, world.WorldgenVersion)
	if err != nil {
		t.Fatal(err)
	}
	var got *SessionRecord
	for i := range live {
		if live[i].Generation == 2 {
			got = &live[i].Session
		}
	}
	if got == nil {
		t.Fatalf("the journaled run was not restored: %#v", live)
	}
	if got.Checkpoints != 2 || !reflect.DeepEqual(got.SolvedPuzzles, []uint8{1, 3}) || !reflect.DeepEqual(got.ClearedGroups, []uint8{0, 4}) {
		t.Fatalf("the overlay lost the route progress: %#v", *got)
	}
	got.SolvedPuzzles[0] = 9
	if saved.SolvedPuzzles[0] != 1 {
		t.Fatal("the overlay aliases the saved record's progress")
	}
}
