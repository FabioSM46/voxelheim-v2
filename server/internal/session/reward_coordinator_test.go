package session

import (
	"context"
	"errors"
	"log/slog"
	"math"
	"os"
	"sync"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

var rewardBones = protocol.InventoryStack{ItemID: uint16(game.ItemBone), Count: 2}

// rewardWorld is one connected character over a real player store, reward journal and
// simulation, with one unclaimed defeat of the guardian waiting for them.
type rewardWorld struct {
	store     *persist.Store
	journal   *persist.RewardStore
	ids       *Identities
	sim       *game.Sim
	manager   *game.InstanceManager
	self      Resolved
	player    *game.Player
	owner     identity.PlayerID
	character persist.Character
}

func newRewardWorld(t *testing.T) *rewardWorld {
	t.Helper()
	dir := t.TempDir()
	store, err := persist.OpenStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	journal, err := persist.OpenRewardStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	ids, _ := internalIdentities(t, store)
	owner := identity.IDOf(identity.Account{41})
	character, err := store.Create(owner, "Eivor", testAppearance())
	if err != nil {
		t.Fatal(err)
	}
	start := game.Life{Pos: [3]float64{0.5, 80, 0.5}, Health: game.PlayerMaxHealth, Hunger: game.PlayerMaxHunger, Experience: 100, Silver: 10}
	if err := ids.writeLife(character.ID, start); err != nil {
		t.Fatal(err)
	}
	if !ids.claim(owner) {
		t.Fatal("claim failed")
	}
	recalled, found, err := ids.recall(character)
	if err != nil || !found {
		t.Fatalf("recall = %v, %v", found, err)
	}
	self := ids.playing(Admitted{ID: owner}, character, true, recalled)

	owned := persist.SessionCharacter{PlayerID: owner, CharacterID: uint64(character.ID)}
	if err := journal.AllocateRun(store, 1, persist.SessionRecord{ID: 7, Seed: 19, Ruin: [2]int64{2, 3}, ExpiresUnix: 100}, world.WorldgenVersion); err != nil {
		t.Fatal(err)
	}
	defeat := persist.RewardDefeat{
		Kind:       vnet.MobKindVargrGuardian,
		Personal:   []persist.PersonalReward{{Owner: owned, Entries: []protocol.InventoryStack{rewardBones}, Silver: 30}},
		Experience: []persist.BossExperienceReward{{Owner: owned, Amount: 90}},
	}
	if err := journal.AppendDefeat(1, defeat, nil); err != nil {
		t.Fatal(err)
	}

	const seed = 0x5EED
	chunks := world.NewCache(seed, 1, 64)
	group := game.NewWorldGroup()
	peers := NewRegistry(DefaultConcurrentSessions)
	logger := slog.New(slog.DiscardHandler)
	sim, err := game.NewSim(20, 2, seed, game.NewCacheTerrain(chunks), chunks, peers.NextID, logger, game.WithWorldGroup(group))
	if err != nil {
		t.Fatal(err)
	}
	manager, err := game.NewInstanceManager(20, 2, 1, peers.NextID, logger, game.WithWorldGroup(group))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(manager.Close)
	player, err := sim.JoinCharacter(peers.NextID(), owner, uint64(character.ID), character.Name, [3]float32{0.5, 80, 0.5}, character.Appearance, recalled, func([]byte) bool { return true })
	if err != nil {
		t.Fatal(err)
	}
	if err := ids.EnableRewards(journal, manager); err != nil {
		t.Fatal(err)
	}
	ids.rewards.retryMin, ids.rewards.retryMax = time.Millisecond, 5*time.Millisecond
	return &rewardWorld{store, journal, ids, sim, manager, self, player, owner, character}
}

func (w *rewardWorld) claim() BossRewardClaim {
	return BossRewardClaim{
		Self: w.self, Player: w.player, Generation: 1, Boss: vnet.MobKindVargrGuardian,
		Grant: game.BossRewardGrant{Entries: []protocol.InventoryStack{rewardBones}, Silver: 30, Experience: 90},
	}
}

// pauseAt stops the coordinator before stage until resume is called.
func (w *rewardWorld) pauseAt(t *testing.T, stage string) (<-chan struct{}, func()) {
	t.Helper()
	reached, release := make(chan struct{}), make(chan struct{})
	w.ids.rewards.step = func(name string) {
		if name == stage {
			close(reached)
			<-release
		}
	}
	var once sync.Once
	resume := func() { once.Do(func() { close(release) }) }
	t.Cleanup(resume)
	return reached, resume
}

func (w *rewardWorld) record(t *testing.T) persist.Record {
	t.Helper()
	rec, found, err := w.store.Load(w.character.ID)
	if err != nil || !found {
		t.Fatalf("Load = %v, %v", found, err)
	}
	return rec
}

func (w *rewardWorld) moved() game.Life {
	life := w.sim.Records()[w.owner]
	life.Pos[0] += 3
	return life
}

// assertDelivered checks the record and the journal agree that the claim landed once.
func (w *rewardWorld) assertDelivered(t *testing.T) {
	t.Helper()
	rec := w.record(t)
	if rec.BossRewardEpoch != 1 || rec.Silver != 40 || rec.Experience != 190 || rec.Slots[0] != rewardBones {
		t.Fatalf("record = epoch %d silver %d experience %d slot %+v; want 1/40/190 and the bones",
			rec.BossRewardEpoch, rec.Silver, rec.Experience, rec.Slots[0])
	}
	journal, err := w.journal.Snapshot()
	if err != nil {
		t.Fatal(err)
	}
	defeat := journal.Runs[0].Defeats[0]
	if len(journal.Intents) != 0 || defeat.Personal[0].Taken != 1 || !defeat.Personal[0].SilverTaken || !defeat.Experience[0].Taken {
		t.Fatalf("journal = %+v, want the claim acknowledged", journal)
	}
}

func awaitReward(t *testing.T, done <-chan error) error {
	t.Helper()
	select {
	case err := <-done:
		return err
	case <-time.After(10 * time.Second):
		t.Fatal("the boss reward never finished")
		return nil
	}
}

func awaitStage(t *testing.T, reached <-chan struct{}) {
	t.Helper()
	select {
	case <-reached:
	case <-time.After(10 * time.Second):
		t.Fatal("the boss reward never reached its stage")
	}
}

func TestRewardCoordinatorDeliversALiveClaimDurably(t *testing.T) {
	t.Parallel()
	w := newRewardWorld(t)
	done, err := w.ids.ClaimBossReward(w.claim())
	if err != nil {
		t.Fatal(err)
	}
	if err := awaitReward(t, done); err != nil {
		t.Fatal(err)
	}
	w.assertDelivered(t)
	live := w.sim.Records()[w.owner]
	if live.BossRewardEpoch != 1 || live.Silver != 40 || live.Experience != 190 || live.Slots[0] != rewardBones {
		t.Fatalf("live = %+v, want the published reward", live)
	}

	moved := w.moved()
	if err := w.ids.RememberAll(map[identity.PlayerID]game.Life{w.owner: moved}); err != nil {
		t.Fatal(err)
	}
	if got := w.record(t); got.Pos != moved.Pos {
		t.Fatal("the autosave was still skipped after the reward finished")
	}

	again, err := w.ids.ClaimBossReward(w.claim())
	if err != nil {
		t.Fatal(err)
	}
	if err := awaitReward(t, again); err == nil {
		t.Fatal("a spent entitlement was delivered twice")
	}
	if got := w.record(t); got.BossRewardEpoch != 1 || got.Silver != 40 {
		t.Fatalf("a refused second claim changed the record: %+v", got)
	}
	if err := w.ids.RememberAll(map[identity.PlayerID]game.Life{w.owner: moved}); err != nil {
		t.Fatalf("a refused claim left the barrier installed: %v", err)
	}
}

func TestRewardCoordinatorHoldsOrdinaryWritersWhileItOwnsTheCharacter(t *testing.T) {
	t.Parallel()
	w := newRewardWorld(t)
	reached, resume := w.pauseAt(t, "acknowledged")
	done, err := w.ids.ClaimBossReward(w.claim())
	if err != nil {
		t.Fatal(err)
	}
	awaitStage(t, reached)

	if _, err := w.ids.ClaimBossReward(w.claim()); !errors.Is(err, ErrRewardOwned) {
		t.Fatalf("a second claim = %v, want %v", err, ErrRewardOwned)
	}
	durable := w.record(t)
	moved := w.moved()
	if err := w.ids.RememberAll(map[identity.PlayerID]game.Life{w.owner: moved}); err != nil {
		t.Fatal(err)
	}
	key := game.InstanceCharacter{PlayerID: w.owner, CharacterID: uint64(w.character.ID)}
	if err := w.ids.RememberCharacters(map[game.InstanceCharacter]game.Life{key: moved}); err != nil {
		t.Fatal(err)
	}
	if got := w.record(t); got != durable {
		t.Fatal("an ordinary writer crossed a pending boss reward")
	}

	resume()
	if err := awaitReward(t, done); err != nil {
		t.Fatal(err)
	}
	if err := w.ids.RememberAll(map[identity.PlayerID]game.Life{w.owner: moved}); err != nil {
		t.Fatal(err)
	}
	if got := w.record(t); got.Pos != moved.Pos || got.BossRewardEpoch != 1 {
		t.Fatalf("the autosave after the reward = %+v", got)
	}
}

// A teardown at any stage hands the leaving life to the claim. The account stays claimed,
// so a reconnect is refused, and offline experience stays queued, until the final write.
func TestRewardCoordinatorOwnsADetachedCharacterUntilItsFinalWrite(t *testing.T) {
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

			w.sim.Leave(w.player)
			if !w.ids.detachReward(w.self, w.player, nil, w.manager) {
				t.Fatal("the teardown did not hand its life to the pending reward")
			}
			if w.ids.claim(w.owner) {
				t.Fatal("a reconnect was admitted while a detached reward was pending")
			}
			award := game.ExperienceAward{PlayerID: w.owner, CharacterName: w.character.Name, Experience: 500}
			if persisted, err := w.ids.RememberExperience(award); persisted || err != nil {
				t.Fatalf("offline experience during a detached reward = %v, %v; want it queued", persisted, err)
			}

			resume()
			if err := awaitReward(t, done); err != nil {
				t.Fatal(err)
			}
			w.assertDelivered(t)
			if !w.ids.claim(w.owner) {
				t.Fatal("the account was not released after the final write")
			}
			if persisted, err := w.ids.RememberExperience(award); !persisted || err != nil {
				t.Fatalf("queued experience after the reward = %v, %v", persisted, err)
			}
		})
	}
}

func TestRewardCoordinatorUndoesARefusedClaimCompletely(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name string
		edit func(*BossRewardClaim)
	}{
		{"the game refuses the grant", func(c *BossRewardClaim) {
			c.Grant.Entries = []protocol.InventoryStack{{ItemID: math.MaxUint16, Count: 1}}
		}},
		{"the journal has no such run", func(c *BossRewardClaim) { c.Generation = 2 }},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			w := newRewardWorld(t)
			before := w.record(t)
			claim := w.claim()
			tc.edit(&claim)
			done, err := w.ids.ClaimBossReward(claim)
			if err != nil {
				t.Fatal(err)
			}
			if err := awaitReward(t, done); err == nil {
				t.Fatal("a refused claim was delivered")
			}
			if got := w.record(t); got != before {
				t.Fatal("a refused claim changed the record")
			}
			if journal, err := w.journal.Snapshot(); err != nil || len(journal.Intents) != 0 {
				t.Fatalf("a refused claim left an intent: %v", err)
			}
			if err := w.ids.writeLife(w.character.ID, lifeOfRecord(before)); err != nil {
				t.Fatalf("a refused claim left the Store barrier installed: %v", err)
			}
			done, err = w.ids.ClaimBossReward(w.claim())
			if err != nil {
				t.Fatal(err)
			}
			if err := awaitReward(t, done); err != nil {
				t.Fatalf("the character stayed owned after a refusal: %v", err)
			}
			w.assertDelivered(t)
		})
	}
}

func TestRewardCoordinatorRetriesAFailedCharacterWriteWithoutReleasingOwnership(t *testing.T) {
	t.Parallel()
	if os.Geteuid() == 0 {
		t.Skip("directory permissions do not bind root")
	}
	w := newRewardWorld(t)
	reached, resume := w.pauseAt(t, "prepared")
	done, err := w.ids.ClaimBossReward(w.claim())
	if err != nil {
		t.Fatal(err)
	}
	awaitStage(t, reached)
	if err := os.Chmod(w.store.Dir(), 0o500); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.Chmod(w.store.Dir(), 0o700) })

	resume()
	select {
	case err := <-done:
		t.Fatalf("the reward finished while its record could not be written: %v", err)
	case <-time.After(50 * time.Millisecond):
	}
	if _, err := w.ids.ClaimBossReward(w.claim()); !errors.Is(err, ErrRewardOwned) {
		t.Fatalf("ownership was released while retrying: %v", err)
	}
	if err := os.Chmod(w.store.Dir(), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := awaitReward(t, done); err != nil {
		t.Fatal(err)
	}
	w.assertDelivered(t)
}

func TestDrainRewardsWaitsForAClaimThatFinishes(t *testing.T) {
	t.Parallel()
	w := newRewardWorld(t)
	reached, resume := w.pauseAt(t, "written")
	done, err := w.ids.ClaimBossReward(w.claim())
	if err != nil {
		t.Fatal(err)
	}
	awaitStage(t, reached)
	drained := make(chan error, 1)
	go func() { drained <- w.ids.DrainRewards(context.Background()) }()
	select {
	case err := <-drained:
		t.Fatalf("the drain returned before the claim finished: %v", err)
	case <-time.After(20 * time.Millisecond):
	}
	resume()
	if err := awaitReward(t, done); err != nil {
		t.Fatal(err)
	}
	if err := awaitReward(t, drained); err != nil {
		t.Fatal(err)
	}
	w.assertDelivered(t)
}

func TestDrainRewardsIsBoundedAndKeepsAnUnfinishedClaimOwned(t *testing.T) {
	t.Parallel()
	w := newRewardWorld(t)
	reached, resume := w.pauseAt(t, "prepared")
	done, err := w.ids.ClaimBossReward(w.claim())
	if err != nil {
		t.Fatal(err)
	}
	awaitStage(t, reached)

	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Millisecond)
	defer cancel()
	if err := w.ids.DrainRewards(ctx); err == nil {
		t.Fatal("a drain past its deadline reported success")
	}
	if _, err := w.ids.ClaimBossReward(w.claim()); !errors.Is(err, ErrRewardsDraining) {
		t.Fatalf("a claim during the drain = %v, want %v", err, ErrRewardsDraining)
	}
	resume()
	if err := awaitReward(t, done); err == nil {
		t.Fatal("a cancelled claim reported success")
	}
	if err := w.ids.writeLife(w.character.ID, lifeOfRecord(w.record(t))); !errors.Is(err, persist.ErrRewardPending) {
		t.Fatalf("an unfinished claim released its barrier: %v", err)
	}
	if journal, err := w.journal.Snapshot(); err != nil || len(journal.Intents) != 1 {
		t.Fatalf("the durable intent for recovery = %v", err)
	}
}

func TestEnableRewardsRefusesAWorldWithoutADurableBarrier(t *testing.T) {
	t.Parallel()
	w := newRewardWorld(t)
	ephemeral, _ := internalIdentities(t, persist.NewMemoryStore())
	if err := ephemeral.EnableRewards(w.journal, w.manager); !errors.Is(err, ErrRewardsDisabled) {
		t.Fatalf("an ephemeral world = %v", err)
	}
	if err := ephemeral.DrainRewards(context.Background()); err != nil {
		t.Fatalf("draining a world without rewards = %v", err)
	}
	if _, err := ephemeral.ClaimBossReward(w.claim()); !errors.Is(err, ErrRewardsDisabled) {
		t.Fatalf("a claim without rewards = %v", err)
	}
	if err := w.ids.EnableRewards(nil, w.manager); !errors.Is(err, ErrRewardsDisabled) {
		t.Fatalf("a missing journal = %v", err)
	}
	if err := w.ids.EnableRewards(w.journal, w.manager); err == nil {
		t.Fatal("rewards were enabled twice")
	}
	if _, err := w.ids.ClaimBossReward(BossRewardClaim{}); !errors.Is(err, ErrRewardNotPlaying) {
		t.Fatalf("a claim naming nobody = %v", err)
	}
}
