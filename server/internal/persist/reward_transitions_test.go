package persist

import (
	"errors"
	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
	"math"
	"reflect"
	"testing"
)

// Personal loot, offline XP, and death bindings belong to separate accounts.
func transitionFixture(t *testing.T) (*Store, *RewardStore, string, Character, RewardReference) {
	t.Helper()
	players, dir := openStore(t)
	owner := newCharacter(t, players, testID(1), "Eivor")
	xp := newCharacter(t, players, testID(2), "Runa")
	bound := newCharacter(t, players, testID(3), "Bjorn")
	rewards, err := OpenRewardStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	session := SessionRecord{ID: 7, Seed: 19, Ruin: [2]int64{2, 3}, ExpiresUnix: 100}
	if err := rewards.AllocateRun(players, 1, session, world.WorldgenVersion); err != nil {
		t.Fatal(err)
	}
	ref := RewardReference{Generation: 1, Boss: vnet.MobKindVargrGuardian, Owner: SessionCharacter{PlayerID: owner.Owner, CharacterID: uint64(owner.ID)}, Entries: 1, Silver: true}
	defeat := RewardDefeat{Kind: ref.Boss, Personal: []PersonalReward{{Owner: ref.Owner, Entries: []protocol.InventoryStack{{ItemID: 1, Count: 2}}, Silver: 30}}, Experience: []BossExperienceReward{{Owner: SessionCharacter{PlayerID: xp.Owner, CharacterID: uint64(xp.ID)}, Amount: 90}}}
	if err := rewards.AppendDefeat(1, defeat, []SessionCharacter{{PlayerID: bound.Owner, CharacterID: uint64(bound.ID)}}); err != nil {
		t.Fatal(err)
	}
	return players, rewards, dir, owner, ref
}
func prepareTransition(t *testing.T, players *Store, rewards *RewardStore, owner Character, ref RewardReference) (*RewardReservation, Record) {
	t.Helper()
	token, base, err := players.ReserveReward(owner.ID)
	if err != nil {
		t.Fatal(err)
	}
	post := base
	post.BossRewardEpoch++
	if ref.Entries != 0 {
		post.Slots[0] = protocol.InventoryStack{ItemID: 1, Count: 2}
	}
	if ref.Silver {
		post.Silver += 30
	}
	if ref.Experience {
		post.Experience += 90
	}
	if err := rewards.ValidateClaim(RewardIntent{RewardReference: ref, PreviousEpoch: base.BossRewardEpoch, Postimage: post}); err != nil {
		t.Fatal(err)
	}
	if err := players.BeginRewardIntent(token, post); err != nil {
		t.Fatal(err)
	}
	if err := rewards.PrepareClaim(players, token, ref); err != nil {
		t.Fatal(err)
	}
	return token, post
}
func TestRewardTransitionsKeepSeparateEntitlementsAcrossDisk(t *testing.T) {
	players, rewards, dir, owner, ref := transitionFixture(t)
	before, _ := rewards.Snapshot()
	if err := rewards.AppendDefeat(1, before.Runs[0].Defeats[0], before.Runs[0].Session.Bound); err != nil {
		t.Fatal(err)
	}
	same, _ := rewards.Snapshot()
	if !reflect.DeepEqual(before, same) {
		t.Fatal("retry changed exact rolls")
	}
	changed, _ := cloneRewardJournal(before)
	changed.Runs[0].Defeats[0].Personal[0].Silver++
	if err := rewards.AppendDefeat(1, changed.Runs[0].Defeats[0], nil); !errors.Is(err, ErrRewardJournalConflict) {
		t.Fatal("reroll accepted")
	}
	token, post := prepareTransition(t, players, rewards, owner, ref)
	if err := rewards.PrepareClaim(players, token, ref); err != nil {
		t.Fatal(err)
	}
	if err := rewards.AcknowledgeClaim(players, token, ref); !errors.Is(err, ErrRewardReservation) {
		t.Fatal("ack before character durability")
	}
	if err := players.WritePreparedReward(token); err != nil {
		t.Fatal(err)
	}
	if err := rewards.AcknowledgeClaim(players, token, ref); err != nil {
		t.Fatal(err)
	}
	if _, _, err := players.ReserveReward(owner.ID); !errors.Is(err, ErrRewardPending) {
		t.Fatal("ack released live ownership")
	}
	if err := players.ReleaseReward(token); err != nil {
		t.Fatal(err)
	}
	disk, _, _ := players.Load(owner.ID)
	if disk != post {
		t.Fatal("prepared image changed")
	}
	cold, err := OpenRewardStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	got, _ := cold.Snapshot()
	d := got.Runs[0].Defeats[0]
	if len(got.Intents) != 0 || d.Personal[0].Taken != 1 || !d.Personal[0].SilverTaken || d.Experience[0].Taken {
		t.Fatal("loot ack changed independent XP")
	}
	if !reflect.DeepEqual(got.Runs[0].Session.Bound, before.Runs[0].Session.Bound) {
		t.Fatal("reward roster replaced bindings")
	}
	xpRef := RewardReference{Generation: 1, Boss: ref.Boss, Owner: d.Experience[0].Owner, Experience: true}
	xpOwner, ok := players.Character(CharacterID(xpRef.Owner.CharacterID))
	if !ok {
		t.Fatal("missing XP character")
	}
	xpToken, xpPost := prepareTransition(t, players, rewards, xpOwner, xpRef)
	if err := players.WritePreparedReward(xpToken); err != nil {
		t.Fatal(err)
	}
	if err := rewards.AcknowledgeClaim(players, xpToken, xpRef); err != nil {
		t.Fatal(err)
	}
	if err := players.ReleaseReward(xpToken); err != nil {
		t.Fatal(err)
	}
	if xpPost.Experience != 90 || xpPost.Silver != 0 || xpPost.Slots[0].Count != 0 {
		t.Fatal("XP gained another population's loot")
	}
	xpPost.BossRewardEpoch++
	if err := rewards.ValidateClaim(RewardIntent{RewardReference: xpRef, PreviousEpoch: 1, Postimage: xpPost}); err == nil {
		t.Fatal("XP claimed twice")
	}
}
func TestRewardTransitionUncertainWritesKeepExactRetryAndOwnership(t *testing.T) {
	for _, stage := range []string{"prepare", "character", "ack"} {
		t.Run(stage, func(t *testing.T) {
			players, rewards, _, owner, ref := transitionFixture(t)
			fault := errors.New("injected directory sync uncertainty")
			token, base, err := players.ReserveReward(owner.ID)
			if err != nil {
				t.Fatal(err)
			}
			post := base
			post.BossRewardEpoch++
			post.Silver = 30
			post.Slots[0] = protocol.InventoryStack{ItemID: 1, Count: 2}
			if err := players.BeginRewardIntent(token, post); err != nil {
				t.Fatal(err)
			}
			uncertain := func(path string, b []byte) error {
				if err := world.WriteAtomic(path, b); err != nil {
					return err
				}
				return fault
			}
			if stage == "prepare" {
				rewards.writeAtomic = uncertain
			}
			err = rewards.PrepareClaim(players, token, ref)
			if stage == "prepare" {
				if !errors.Is(err, fault) {
					t.Fatal(err)
				}
				if err := players.AbortReward(token); !errors.Is(err, ErrRewardReservation) {
					t.Fatal("uncertain intent released")
				}
				if err := rewards.AppendDefeat(1, RewardDefeat{Kind: vnet.MobKindDraugrKing}, nil); !errors.Is(err, ErrRewardJournalConflict) {
					t.Fatal("transition outran uncertain prepare")
				}
				rewards.writeAtomic = world.WriteAtomic
				err = rewards.PrepareClaim(players, token, ref)
			}
			if err != nil {
				t.Fatal(err)
			}
			if stage == "character" {
				players.recordWriter = uncertain
			}
			err = players.WritePreparedReward(token)
			if stage == "character" {
				if !errors.Is(err, fault) {
					t.Fatal(err)
				}
				if _, found, err := players.Load(owner.ID); err != nil || !found {
					t.Fatal("fault must leave readable renamed record")
				}
				if err := rewards.AcknowledgeClaim(players, token, ref); !errors.Is(err, ErrRewardReservation) {
					t.Fatal("readable epoch mistaken for durability")
				}
				players.recordWriter = world.WriteAtomic
				err = players.WritePreparedReward(token)
			}
			if err != nil {
				t.Fatal(err)
			}
			if stage == "ack" {
				rewards.writeAtomic = uncertain
			}
			err = rewards.AcknowledgeClaim(players, token, ref)
			if stage == "ack" {
				if !errors.Is(err, fault) {
					t.Fatal(err)
				}
				if _, _, err := players.ReserveReward(owner.ID); !errors.Is(err, ErrRewardPending) {
					t.Fatal("second claim outran uncertain ack")
				}
				if err := players.Save(owner.ID, post); !errors.Is(err, ErrRewardPending) {
					t.Fatal("ordinary writer outran uncertain ack")
				}
				rewards.writeAtomic = world.WriteAtomic
				err = rewards.AcknowledgeClaim(players, token, ref)
			}
			if err != nil {
				t.Fatal(err)
			}
			if err := players.ReleaseReward(token); err != nil {
				t.Fatal(err)
			}
			if err := players.Save(owner.ID, base); !errors.Is(err, ErrRewardEpoch) {
				t.Fatal("stale writer erased receipt")
			}
		})
	}
}
func TestRewardGenerationHighWaterSurvivesGCAndUncertainAllocation(t *testing.T) {
	players, dir := openStore(t)
	rewards, err := OpenRewardStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	rec := SessionRecord{ID: 1, Seed: 1, Ruin: [2]int64{1, 2}, ExpiresUnix: 100, DefeatedBosses: []vnet.MobKind{vnet.MobKindVargrGuardian}}
	fault := errors.New("allocation sync uncertainty")
	rewards.writeAtomic = func(path string, b []byte) error {
		if err := world.WriteAtomic(path, b); err != nil {
			return err
		}
		return fault
	}
	if err := rewards.AllocateRun(players, 1, rec, world.WorldgenVersion); !errors.Is(err, fault) {
		t.Fatal(err)
	}
	if !players.strictRewards.Load() {
		t.Fatal("uncertain allocation did not enable strict receipts")
	}
	rewards.writeAtomic = world.WriteAtomic
	if err := rewards.AllocateRun(players, 1, rec, world.WorldgenVersion); err != nil {
		t.Fatal(err)
	}
	if err := rewards.AllocateRun(players, 1, rec, world.WorldgenVersion); err != nil {
		t.Fatal(err)
	}
	j, _ := rewards.Snapshot()
	if len(j.Runs[0].Defeats) != 1 || len(j.Runs[0].Defeats[0].Personal) != 0 || len(j.Runs[0].Defeats[0].Experience) != 0 {
		t.Fatal("legacy progress generated rewards")
	}
	// No gameplay GC API yet. The storage primitive must retain the high-water.
	j.Runs = nil
	j.Revision++
	if err := rewards.commit(j.Revision-1, j); err != nil {
		t.Fatal(err)
	}
	cold, err := OpenRewardStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	if err := cold.CheckInactive(); err != nil {
		t.Fatal(err)
	}
	if err := cold.AllocateRun(players, 1, rec, world.WorldgenVersion); !errors.Is(err, ErrRewardJournalConflict) {
		t.Fatal("collected generation reused")
	}
	if err := cold.AllocateRun(players, 2, rec, world.WorldgenVersion); err != nil {
		t.Fatal(err)
	}
	snap, _ := cold.Snapshot()
	snap.Runs = nil
	snap.NextGeneration = math.MaxUint64
	snap.Revision++
	if err := cold.commit(snap.Revision-1, snap); err != nil {
		t.Fatal(err)
	}
	if err := cold.AllocateRun(players, math.MaxUint64, rec, world.WorldgenVersion); !errors.Is(err, ErrRewardJournalConflict) {
		t.Fatal("generation wrapped")
	}
	if err := cold.AllocateRun(NewMemoryStore(), 2, rec, world.WorldgenVersion); err == nil {
		t.Fatal("ephemeral durable allocation")
	}
}

func TestRewardTransitionsRefuseAnotherWorldsStoreAndReservation(t *testing.T) {
	players, rewards, _, owner, ref := transitionFixture(t)
	other, otherRewards, _, otherOwner, otherRef := transitionFixture(t)
	otherToken, _ := prepareTransition(t, other, otherRewards, otherOwner, otherRef)
	if err := other.WritePreparedReward(otherToken); err != nil {
		t.Fatal(err)
	}
	before, _ := rewards.Snapshot()
	if err := rewards.AllocateRun(other, 2, SessionRecord{ID: 8, Seed: 2, ExpiresUnix: 100}, world.WorldgenVersion); !errors.Is(err, ErrRewardJournalConflict) {
		t.Fatal("allocation accepted unrelated Store")
	}
	if err := rewards.PrepareClaim(other, otherToken, ref); !errors.Is(err, ErrRewardJournalConflict) {
		t.Fatal("prepare accepted another world's character")
	}
	if err := rewards.AcknowledgeClaim(other, otherToken, ref); !errors.Is(err, ErrRewardJournalConflict) {
		t.Fatal("ack accepted another world's receipt")
	}
	// Even paired paths cannot substitute a token owned by another Store instance.
	if err := rewards.PrepareClaim(players, otherToken, ref); !errors.Is(err, ErrRewardReservation) {
		t.Fatal("foreign token accepted")
	}
	after, _ := rewards.Snapshot()
	if !reflect.DeepEqual(before, after) {
		t.Fatal("refusal changed journal")
	}
	if _, _, err := players.ReserveReward(owner.ID); err != nil {
		t.Fatal("refusal stranded unrelated character")
	}
}

func TestRewardAppendCannotInventConsumptionOrResetAcknowledgedFlags(t *testing.T) {
	players, rewards, _, owner, ref := transitionFixture(t)
	before, _ := rewards.Snapshot()
	for _, kind := range []string{"entries", "silver", "xp"} {
		t.Run(kind, func(t *testing.T) {
			clone, _ := cloneRewardJournal(before)
			defeat := clone.Runs[0].Defeats[0]
			defeat.Kind = vnet.MobKindDraugrKing
			switch kind {
			case "entries":
				defeat.Personal[0].Taken = 1
			case "silver":
				defeat.Personal[0].SilverTaken = true
			case "xp":
				defeat.Experience[0].Taken = true
			}
			if err := rewards.AppendDefeat(1, defeat, nil); !errors.Is(err, ErrRewardJournalConflict) {
				t.Fatal("new defeat invented acknowledged reward")
			}
			after, _ := rewards.Snapshot()
			if !reflect.DeepEqual(before, after) {
				t.Fatal("refused append mutated journal")
			}
		})
	}
	token, _ := prepareTransition(t, players, rewards, owner, ref)
	if err := players.WritePreparedReward(token); err != nil {
		t.Fatal(err)
	}
	if err := rewards.AcknowledgeClaim(players, token, ref); err != nil {
		t.Fatal(err)
	}
	consumed, _ := rewards.Snapshot()
	if err := rewards.AppendDefeat(1, before.Runs[0].Defeats[0], nil); !errors.Is(err, ErrRewardJournalConflict) {
		t.Fatal("late original producer retry reset consumption")
	}
	after, _ := rewards.Snapshot()
	if !reflect.DeepEqual(consumed, after) {
		t.Fatal("late producer retry changed taken flags")
	}
}
