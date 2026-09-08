package persist

import (
	"bytes"
	"errors"
	"os"
	"path/filepath"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func recoveryValidator(r Record) error { return validateRewardPostimage(r) }

func TestRewardRecoveryReplaysBeforeIndexAndNeverRollsBackLaterReceipts(t *testing.T) {
	for _, state := range []string{"old", "missing", "corrupt", "prepared", "later"} {
		t.Run(state, func(t *testing.T) {
			players, rewards, dir, owner, ref := transitionFixture(t)
			_, post := prepareTransition(t, players, rewards, owner, ref)
			want := post
			switch state {
			case "missing":
				if err := os.Remove(players.recordPath(owner.ID)); err != nil {
					t.Fatal(err)
				}
			case "corrupt":
				if err := os.WriteFile(players.recordPath(owner.ID), []byte("interrupted record"), 0600); err != nil {
					t.Fatal(err)
				}
			case "prepared", "later":
				if state == "later" {
					want.BossRewardEpoch += 2
					want.Experience = 999
					want.Silver = 777
					want.Slots[0].Count = 8
				}
				if err := players.writeRecord(owner, want); err != nil {
					t.Fatal(err)
				}
			}
			cold, err := OpenRewardStore(dir)
			if err != nil {
				t.Fatal(err)
			}
			restored, err := OpenStoreWithRewardRecovery(dir, cold, recoveryValidator)
			if err != nil {
				t.Fatal(err)
			}
			got, found, err := restored.Load(owner.ID)
			if err != nil || !found || got != want {
				t.Fatalf("recovery changed postimage or later state: found=%v error=%v", found, err)
			}
			if _, known := restored.Character(owner.ID); !known {
				t.Fatal("repaired character absent from startup index")
			}
			snap, _ := cold.Snapshot()
			if len(snap.Intents) != 0 || snap.Runs[0].Defeats[0].Personal[0].Taken != 1 || !snap.Runs[0].Defeats[0].Personal[0].SilverTaken {
				t.Fatal("durable repair not acknowledged")
			}
			if snap.Runs[0].Defeats[0].Experience[0].Taken {
				t.Fatal("replay consumed independent XP")
			}
			// Every later restart is idempotent, with no inventory/XP delta reapplied.
			againJournal, err := OpenRewardStore(dir)
			if err != nil {
				t.Fatal(err)
			}
			again, err := OpenStoreWithRewardRecovery(dir, againJournal, recoveryValidator)
			if err != nil {
				t.Fatal(err)
			}
			got, _, _ = again.Load(owner.ID)
			if got != want {
				t.Fatal("second restart duplicated reward")
			}
			stale := want
			stale.BossRewardEpoch = 0
			if err := again.Save(owner.ID, stale); !errors.Is(err, ErrRewardEpoch) {
				t.Fatal("cold ordinary writer erased receipt")
			}
		})
	}
}

func TestRewardRecoveryKeepsIntentUntilCharacterAndJournalDurability(t *testing.T) {
	for _, stage := range []string{"character-before-write", "character-after-write", "ack-before-write", "ack-after-write", "later-confirm"} {
		t.Run(stage, func(t *testing.T) {
			players, rewards, dir, owner, ref := transitionFixture(t)
			_, post := prepareTransition(t, players, rewards, owner, ref)
			if stage == "later-confirm" {
				post.BossRewardEpoch += 3
				post.Silver = 900
				post.Experience = 800
				if err := players.writeRecord(owner, post); err != nil {
					t.Fatal(err)
				}
			}
			cold, err := OpenRewardStore(dir)
			if err != nil {
				t.Fatal(err)
			}
			fault := errors.New("injected recovery checkpoint")
			if stage == "ack-before-write" || stage == "ack-after-write" {
				cold.writeAtomic = func(path string, b []byte) error {
					if stage == "ack-after-write" {
						if err := world.WriteAtomic(path, b); err != nil {
							return err
						}
					}
					return fault
				}
			}
			writes := 0
			_, err = openPlayerStore(dir, func(s *Store) error {
				s.strictRewards.Store(true)
				if stage == "character-before-write" || stage == "character-after-write" || stage == "later-confirm" {
					s.recordWriter = func(path string, b []byte) error {
						writes++
						if stage == "character-after-write" {
							if err := world.WriteAtomic(path, b); err != nil {
								return err
							}
						}
						return fault
					}
				}
				cold.mu.Lock()
				defer cold.mu.Unlock()
				return cold.recoverRecordsLocked(s, recoveryValidator)
			})
			if !errors.Is(err, fault) {
				t.Fatalf("checkpoint not observed: %v", err)
			}
			snap, _ := cold.Snapshot()
			if len(snap.Intents) != 1 {
				t.Fatal("failed confirmation cleared in-memory intent")
			}
			if stage == "later-confirm" && writes != 1 {
				t.Fatal("later readable receipt bypassed durability confirmation")
			}
			// A real cold start accepts either side of an uncertain rename. Both converge.
			retry, err := OpenRewardStore(dir)
			if err != nil {
				t.Fatal(err)
			}
			restored, err := OpenStoreWithRewardRecovery(dir, retry, recoveryValidator)
			if err != nil {
				t.Fatal(err)
			}
			got, _, err := restored.Load(owner.ID)
			if err != nil || got != post {
				t.Fatal("retry lost exact committed record")
			}
			snap, _ = retry.Snapshot()
			if len(snap.Intents) != 0 {
				t.Fatal("confirmed retry retained resolved intent")
			}
		})
	}
}

func TestRewardRecoveryRefusesUnprovableRecordsBeforeDestructiveIndexing(t *testing.T) {
	for _, state := range []string{"missing-owner", "corrupt-owner", "corrupt-unrelated", "identity", "invalid-postimage", "invalid-later", "unreadable-directory"} {
		t.Run(state, func(t *testing.T) {
			players, rewards, dir, owner, ref := transitionFixture(t)
			original, _, _ := players.Load(owner.ID)
			path := players.recordPath(owner.ID)
			switch state {
			case "missing-owner":
				if err := os.Remove(path); err != nil {
					t.Fatal(err)
				}
			case "corrupt-owner":
				if err := os.WriteFile(path, []byte("corrupt"), 0600); err != nil {
					t.Fatal(err)
				}
			case "corrupt-unrelated":
				path = filepath.Join(players.dir, "000000000000007f.bin")
				if err := os.WriteFile(path, []byte("corrupt"), 0600); err != nil {
					t.Fatal(err)
				}
			case "identity":
				_, _ = prepareTransition(t, players, rewards, owner, ref)
				other := owner
				other.Name = "Changed"
				if err := players.writeRecord(other, original); err != nil {
					t.Fatal(err)
				}
			case "invalid-postimage", "invalid-later":
				_, post := prepareTransition(t, players, rewards, owner, ref)
				if state == "invalid-later" {
					post.BossRewardEpoch++
					post.Experience = 999
					if err := players.writeRecord(owner, post); err != nil {
						t.Fatal(err)
					}
				}
			case "unreadable-directory":
				if err := os.Remove(path); err != nil {
					t.Fatal(err)
				}
				if err := os.Mkdir(path, 0700); err != nil {
					t.Fatal(err)
				}
			}
			before, _ := os.ReadFile(path)
			cold, err := OpenRewardStore(dir)
			if err != nil {
				t.Fatal(err)
			}
			validate := recoveryValidator
			if state == "invalid-postimage" {
				validate = func(Record) error { return errors.New("invalid game item") }
			}
			if state == "invalid-later" {
				validate = func(r Record) error {
					if r.Experience == 999 {
						return errors.New("invalid later life")
					}
					return recoveryValidator(r)
				}
			}
			if _, err := OpenStoreWithRewardRecovery(dir, cold, validate); err == nil {
				t.Fatal("unprovable recovery accepted")
			}
			after, _ := os.ReadFile(path)
			if !bytes.Equal(before, after) {
				t.Fatal("refusal rewrote source record")
			}
			if _, err := os.Stat(path); err != nil && state != "missing-owner" {
				t.Fatal("refusal quarantined source record")
			}
		})
	}
}

func TestStrictRewardPolicySurvivesHighWaterOnlyRestartAndCannotBeBypassed(t *testing.T) {
	players, dir := openStore(t)
	owner := newCharacter(t, players, testID(1), "Eivor")
	old, _, _ := players.Load(owner.ID)
	rewards, err := OpenRewardStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	if err := rewards.commit(0, RewardJournal{Revision: 1, NextGeneration: 9}); err != nil {
		t.Fatal(err)
	}
	strict, err := OpenStoreWithRewardRecovery(dir, rewards, recoveryValidator)
	if err != nil {
		t.Fatal(err)
	}
	if !strict.strictRewards.Load() {
		t.Fatal("GC relaxed irreversible receipt policy")
	}
	if err := strict.Save(owner.ID, old); err != nil {
		t.Fatal(err)
	} // install the cached epoch-zero floor
	if _, err := strict.Quarantine(owner.ID); err == nil {
		t.Fatal("strict quarantine erased potentially unknown receipt")
	}
	if err := os.WriteFile(strict.recordPath(owner.ID), []byte("corrupt receipt"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := strict.Save(owner.ID, old); err == nil {
		t.Fatal("cached zero floor overwrote unreadable receipt")
	}
	if _, err := OpenStoreWithRewardRecovery(dir, rewards, recoveryValidator); err == nil {
		t.Fatal("strict index silently quarantined corrupt receipt")
	}
	if err := os.Remove(strict.recordPath(owner.ID)); err != nil {
		t.Fatal(err)
	}
	if _, _, err := strict.Load(owner.ID); !errors.Is(err, ErrRewardRecoveryRequired) {
		t.Fatal("missing known character normalized to new life")
	}
	if err := strict.Save(owner.ID, old); err == nil {
		t.Fatal("missing known character recreated by stale writer")
	}
	if _, err := OpenStoreWithRewardRecovery(t.TempDir(), rewards, recoveryValidator); err == nil {
		t.Fatal("journal from different world accepted")
	}
	if _, err := OpenStoreWithRewardRecovery(dir, rewards, nil); err == nil {
		t.Fatal("game validation omitted")
	}
	if _, err := OpenStoreWithRewardRecovery("", nil, recoveryValidator); err == nil {
		t.Fatal("nil recovery store accepted")
	}
}
