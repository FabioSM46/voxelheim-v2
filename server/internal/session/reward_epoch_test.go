package session

import (
	"errors"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
)

func TestIdentityWritersPreserveRewardEpochAndRejectCapturedOldLife(t *testing.T) {
	for _, writer := range []string{"all", "characters", "teardown"} {
		t.Run(writer, func(t *testing.T) {
			store, err := persist.OpenStore(t.TempDir())
			if err != nil {
				t.Fatal(err)
			}
			ids, _ := internalIdentities(t, store)
			owner := identity.IDOf(identity.Account{31})
			character, err := store.Create(owner, "Eivor", testAppearance())
			if err != nil {
				t.Fatal(err)
			}
			stale := game.Life{Health: 100, Hunger: 100}
			if err := ids.writeLife(character.ID, stale); err != nil {
				t.Fatal(err)
			}
			if !ids.claim(owner) {
				t.Fatal("claim failed")
			}
			self := ids.playing(Admitted{ID: owner}, character, false, nil)
			save := func(life game.Life) error {
				switch writer {
				case "all":
					return ids.RememberAll(map[identity.PlayerID]game.Life{owner: life})
				case "characters":
					return ids.RememberCharacters(map[game.InstanceCharacter]game.Life{{PlayerID: owner, CharacterID: uint64(character.ID)}: life})
				default:
					return ids.Remember(self, life)
				}
			}
			token, next, err := store.ReserveReward(character.ID)
			if err != nil {
				t.Fatal(err)
			}
			next.BossRewardEpoch++
			next.Silver = 19
			if err := store.BeginRewardIntent(token, next); err != nil {
				t.Fatal(err)
			}
			if err := store.WritePreparedReward(token); err != nil {
				t.Fatal(err)
			}
			// Captured before live publication: receipt durability alone must not reopen writes.
			if err := ids.writeLife(character.ID, stale); !errors.Is(err, persist.ErrRewardPending) {
				t.Fatal("write crossed publication barrier")
			}
			if err := store.ReleaseReward(token); err != nil {
				t.Fatal(err)
			}
			if err := save(stale); !errors.Is(err, persist.ErrRewardEpoch) {
				t.Fatalf("stale %s write accepted: %v", writer, err)
			}
			life, found, err := ids.recall(character)
			if err != nil || !found || life.BossRewardEpoch != 1 || life.Silver != 19 {
				t.Fatalf("recall lost epoch: %+v %v", life, err)
			}
			if err := ids.writeLife(character.ID, *life); err != nil {
				t.Fatal(err)
			}
			durable, _, err := store.Load(character.ID)
			if err != nil || durable.BossRewardEpoch != 1 || durable.Silver != 19 {
				t.Fatal("identity write dropped receipt")
			}
		})
	}
}

func TestOfflineExperienceWaitsForTheRewardBarrierAndPreservesReceipt(t *testing.T) {
	store, err := persist.OpenStore(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	ids, _ := internalIdentities(t, store)
	owner := identity.IDOf(identity.Account{31})
	c, err := store.Create(owner, "Eivor", testAppearance())
	if err != nil {
		t.Fatal(err)
	}
	if err := store.Save(c.ID, persist.Record{Health: 100, Hunger: 100, Experience: 10}); err != nil {
		t.Fatal(err)
	}
	award := game.ExperienceAward{PlayerID: owner, CharacterName: "Eivor", Experience: 20}
	if yes, err := ids.RememberExperience(award); err != nil || !yes {
		t.Fatal("pre-barrier XP was not durable")
	}
	token, next, err := store.ReserveReward(c.ID)
	if err != nil || next.Experience != 20 {
		t.Fatal("reservation ignored durable offline XP")
	}
	next.BossRewardEpoch++
	next.Experience += 5
	award.Experience = 30
	if yes, err := ids.RememberExperience(award); yes || !errors.Is(err, persist.ErrRewardPending) {
		t.Fatalf("pending XP acknowledged: %v %v", yes, err)
	}
	if err := store.BeginRewardIntent(token, next); err != nil {
		t.Fatal(err)
	}
	if err := store.WritePreparedReward(token); err != nil {
		t.Fatal(err)
	}
	if yes, err := ids.RememberExperience(award); yes || !errors.Is(err, persist.ErrRewardPending) {
		t.Fatal("XP bypassed publication barrier")
	}
	if err := store.ReleaseReward(token); err != nil {
		t.Fatal(err)
	}
	if yes, err := ids.RememberExperience(award); err != nil || !yes {
		t.Fatalf("XP retry failed: %v", err)
	}
	got, _, err := store.Load(c.ID)
	if err != nil || got.Experience != 30 || got.BossRewardEpoch != 1 {
		t.Fatal("offline XP erased receipt")
	}
}

func TestRecallCannotQuarantineARewardReceiptIntoAFreshCharacter(t *testing.T) {
	dir := t.TempDir()
	store, err := persist.OpenStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	owner := identity.IDOf(identity.Account{31})
	c, err := store.Create(owner, "Eivor", testAppearance())
	if err != nil {
		t.Fatal(err)
	}
	token, next, err := store.ReserveReward(c.ID)
	if err != nil {
		t.Fatal(err)
	}
	next.BossRewardEpoch++
	next.Health = 100
	next.Hunger = 100
	if err := store.BeginRewardIntent(token, next); err != nil {
		t.Fatal(err)
	}
	if err := store.WritePreparedReward(token); err != nil {
		t.Fatal(err)
	}
	if err := store.ReleaseReward(token); err != nil {
		t.Fatal(err)
	}
	next.Health = 65535 // readable/checksummed record, rejected by Life.Validate
	if err := store.Save(c.ID, next); err != nil {
		t.Fatal(err)
	}
	for _, cold := range []bool{false, true} {
		if cold {
			store, err = persist.OpenStore(dir)
			if err != nil {
				t.Fatal(err)
			}
		}
		ids, _ := internalIdentities(t, store)
		if life, _, err := ids.recall(c); life != nil || !errors.Is(err, persist.ErrRewardEpoch) {
			t.Fatalf("receipt became a fresh life: %+v %v", life, err)
		}
		kept, found, err := store.Load(c.ID)
		if err != nil || !found || kept.BossRewardEpoch != 1 {
			t.Fatal("quarantine removed receipt")
		}
	}
}
