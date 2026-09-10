package session

import (
	"errors"
	"os"
	"path/filepath"
	"slices"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

type consumption struct {
	corpse  uint64
	indices []uint8
	silver  uint32
}

// fakeBossLoot is a claimed boss corpse as a take sees it, recording what a finished claim
// consumed and what a failed one refused.
type fakeBossLoot struct {
	selection game.BossLootSelection
	consumed  chan consumption
	refused   chan vnet.RefusalReason
}

func newFakeBossLoot(selection game.BossLootSelection) *fakeBossLoot {
	return &fakeBossLoot{selection: selection, consumed: make(chan consumption, 1), refused: make(chan vnet.RefusalReason, 1)}
}

func (f *fakeBossLoot) BossLootToClaim(uint64, uint32, uint64) (game.BossLootSelection, vnet.RefusalReason, error) {
	return f.selection, vnet.RefusalReasonUnknown, nil
}

func (f *fakeBossLoot) ConsumeClaimedBossLoot(corpseID uint64, indices []uint8, silver uint32) bool {
	f.consumed <- consumption{corpseID, slices.Clone(indices), silver}
	return true
}

func (f *fakeBossLoot) QueueLootRefusal(reason vnet.RefusalReason) { f.refused <- reason }

func awaitValue[T any](t *testing.T, what string, values <-chan T) T {
	t.Helper()
	select {
	case v := <-values:
		return v
	case <-time.After(10 * time.Second):
		t.Fatalf("timed out waiting for %s", what)
		var zero T
		return zero
	}
}

// visitRun restores the saved run a portal visit names, holding the fixture's generation.
func (w *rewardWorld) visitRun(t *testing.T) uint64 {
	t.Helper()
	run := game.SavedSession{ID: 7, Seed: 19, Ruin: game.InstanceRuin{CellX: 2, CellZ: 3}, ExpiresUnix: time.Now().Add(time.Hour).Unix(),
		DefeatedBosses: []vnet.MobKind{vnet.MobKindVargrGuardian}, Generation: 1}
	if _, _, err := w.manager.RestoreSessions([]game.SavedSession{run}); err != nil {
		t.Fatal(err)
	}
	return run.ID
}

// A take that must be a claim is claimed only on a dungeon visit. The corpse gives up what the
// claim took once it lands, and a claim that fails later is refused through the game's queue.
func TestClaimBossLootDeliversOnlyOnADungeonVisit(t *testing.T) {
	t.Parallel()
	w := newRewardWorld(t)
	loot := newFakeBossLoot(game.BossLootSelection{CorpseID: 44, Kind: vnet.MobKindVargrGuardian,
		Entries: []protocol.InventoryStack{rewardBones}, EntryIndices: []uint8{0}, Silver: 30, Partial: true})

	if reason, err := w.ids.claimBossLoot(w.self, w.player, loot, 0, 44, 1, 0); !errors.Is(err, errBossLootOutsideVisit) || reason != vnet.RefusalReasonCorpseUnavailable {
		t.Fatalf("a take outside a dungeon visit = %s, %v", reason, err)
	}
	run := w.visitRun(t)
	if reason, err := w.ids.claimBossLoot(w.self, w.player, loot, run+1, 44, 1, 0); err == nil || reason != vnet.RefusalReasonCorpseUnavailable {
		t.Fatalf("a visit naming no journal run = %s, %v", reason, err)
	}

	if reason, err := w.ids.claimBossLoot(w.self, w.player, loot, run, 44, 1, 0); err != nil || reason != vnet.RefusalReasonUnknown {
		t.Fatalf("a dungeon take = %s, %v", reason, err)
	}
	got := awaitValue(t, "the claimed corpse to be consumed", loot.consumed)
	if got.corpse != 44 || !slices.Equal(got.indices, []uint8{0}) || got.silver != 30 {
		t.Fatalf("consumed = %+v, want corpse 44, roll index 0 and 30 silver", got)
	}
	rec := w.record(t)
	if rec.BossRewardEpoch != 1 || rec.Silver != 40 || rec.Slots[0] != rewardBones {
		t.Fatalf("record = epoch %d silver %d slot %+v", rec.BossRewardEpoch, rec.Silver, rec.Slots[0])
	}
	personal := w.journalNow(t).Runs[0].Defeats[0].Personal[0]
	if personal.Taken != 1 || !personal.SilverTaken {
		t.Fatalf("journal personal = %+v, want the bones and silver taken", personal)
	}

	invalid := newFakeBossLoot(game.BossLootSelection{CorpseID: 44, Kind: vnet.MobKindVargrGuardian,
		Entries: []protocol.InventoryStack{{ItemID: 65535, Count: 1}}, EntryIndices: []uint8{1}})
	if reason, err := w.ids.claimBossLoot(w.self, w.player, invalid, run, 44, 2, 0); err != nil || reason != vnet.RefusalReasonUnknown {
		t.Fatalf("submitting a claim that will fail = %s, %v", reason, err)
	}
	if reason := awaitValue(t, "the failed claim's refusal", invalid.refused); reason != vnet.RefusalReasonInventoryFull {
		t.Fatalf("failed claim refusal = %s, want InventoryFull", reason)
	}
	select {
	case got := <-invalid.consumed:
		t.Fatalf("a failed claim consumed the corpse: %+v", got)
	default:
	}
}

func (w *rewardWorld) journalNow(t *testing.T) persist.RewardJournal {
	t.Helper()
	journal, err := w.journal.Snapshot()
	if err != nil {
		t.Fatal(err)
	}
	return journal
}

// The journal's entry mask is built from the frozen roll indices a claim names, never from the
// grant's own positions.
func TestABossRewardClaimTakesTheFrozenRollIndicesItNames(t *testing.T) {
	t.Parallel()
	w := newRewardWorld(t)
	pelt := protocol.InventoryStack{ItemID: uint16(game.ItemVargrPelt), Count: 3}
	owner := persist.SessionCharacter{PlayerID: w.owner, CharacterID: uint64(w.character.ID)}
	record := persist.SessionRecord{ID: 8, Seed: 20, Ruin: [2]int64{4, 5}, ExpiresUnix: 100, DefeatedBosses: []vnet.MobKind{vnet.MobKindVargrGuardian}}
	defeat := persist.RewardDefeat{Kind: vnet.MobKindVargrGuardian, Personal: []persist.PersonalReward{{Owner: owner, Entries: []protocol.InventoryStack{rewardBones, pelt}}}}
	if err := w.journal.AllocateRun(w.store, 2, record, world.WorldgenVersion, defeat); err != nil {
		t.Fatal(err)
	}

	claim := func(indices ...uint8) BossRewardClaim {
		return BossRewardClaim{Self: w.self, Player: w.player, Generation: 2, Boss: vnet.MobKindVargrGuardian,
			Grant: game.BossRewardGrant{Entries: []protocol.InventoryStack{pelt}}, EntryIndices: indices}
	}
	for name, refused := range map[string]BossRewardClaim{
		"no index":        claim(),
		"two indices":     claim(1, 0),
		"past the mask":   claim(64),
		"a repeated slot": {Self: w.self, Player: w.player, Generation: 2, Grant: game.BossRewardGrant{Entries: []protocol.InventoryStack{pelt, pelt}}, EntryIndices: []uint8{1, 1}},
	} {
		if _, err := w.ids.ClaimBossReward(refused); !errors.Is(err, ErrRewardSelection) {
			t.Errorf("%s = %v, want %v", name, err, ErrRewardSelection)
		}
	}

	delivered := make(chan consumption, 1)
	req := claim(1)
	req.Delivered = func(indices []uint8, silver bool) {
		taken := uint32(0)
		if silver {
			taken = 1
		}
		delivered <- consumption{indices: slices.Clone(indices), silver: taken}
	}
	done, err := w.ids.ClaimBossReward(req)
	if err != nil {
		t.Fatal(err)
	}
	if err := awaitReward(t, done); err != nil {
		t.Fatal(err)
	}
	if got := awaitValue(t, "the delivery callback", delivered); !slices.Equal(got.indices, []uint8{1}) || got.silver != 0 {
		t.Fatalf("delivered = %+v, want roll index 1 and no silver", got)
	}
	for _, run := range w.journalNow(t).Runs {
		if run.Generation == 2 && run.Defeats[0].Personal[0].Taken != 0b10 {
			t.Fatalf("taken mask = %b, want only the pelt at index 1", run.Defeats[0].Personal[0].Taken)
		}
	}
}

// While a sync write has left the journal uncertain, a claim cannot write over it: it waits
// until the sync's identical retry lands, and then completes.
func TestClaimAndSyncJournalWritesNeverInterleaveWhileOneIsUncertain(t *testing.T) {
	t.Parallel()
	if os.Geteuid() == 0 {
		t.Skip("directory permissions do not bind root")
	}
	w := newRewardWorld(t)
	held := game.SavedSession{ID: 7, Seed: 19, Ruin: game.InstanceRuin{CellX: 2, CellZ: 3}, ExpiresUnix: 100,
		DefeatedBosses: []vnet.MobKind{vnet.MobKindVargrGuardian}, Generation: 1}
	fresh := game.SavedSession{ID: 9, Seed: 77, Ruin: game.InstanceRuin{CellX: 5, CellZ: 5}, ExpiresUnix: time.Now().Add(time.Hour).Unix(),
		DefeatedBosses: []vnet.MobKind{vnet.MobKindVargrGuardian}}
	w.ids.rewards.runs = &fakeRuns{saved: []game.SavedSession{held, fresh}}

	worldDir := filepath.Dir(w.store.Dir())
	if err := os.Chmod(worldDir, 0o500); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.Chmod(worldDir, 0o700) })
	if err := w.ids.SyncRewardRuns(time.Now()); err == nil {
		t.Fatal("an allocation that could not land reported success")
	}
	if !w.journal.Uncertain() || w.ids.rewards.pendingRunWrite == nil {
		t.Fatal("the failed allocation did not leave an uncertain write to retry")
	}

	done, err := w.ids.ClaimBossReward(w.claim())
	if err != nil {
		t.Fatal(err)
	}
	select {
	case err := <-done:
		t.Fatalf("a claim finished over an uncertain sync write: %v", err)
	case <-time.After(50 * time.Millisecond):
	}
	if intents := w.journalNow(t).Intents; len(intents) != 0 {
		t.Fatalf("the claim wrote over the uncertain allocation: %+v", intents)
	}
	w.ids.rewards.journalMu.Lock()
	owner := w.ids.rewards.journalOwner
	w.ids.rewards.journalMu.Unlock()
	if owner != any(&w.ids.rewards.syncMu) {
		t.Fatal("the uncertain write's owner is not the sync")
	}

	if err := os.Chmod(worldDir, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := w.ids.SyncRewardRuns(time.Now()); err != nil {
		t.Fatalf("the sync's verbatim retry = %v", err)
	}
	if err := awaitReward(t, done); err != nil {
		t.Fatalf("the claim after the retry landed = %v", err)
	}
	w.assertDelivered(t)
	if got := w.journalNow(t); got.NextGeneration != 3 {
		t.Fatalf("journal = %+v, want the retried allocation at generation 2", got)
	}
}
