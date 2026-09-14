package persist

import (
	"bytes"
	"encoding/binary"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// losslessMigrationSources is every older format a record is always carried out of.
// Each one is a format a world with boss receipts must still be able to start from.
var losslessMigrationSources = []uint32{10, 11}

// A record's layout belongs to the build that wrote it, so the table is pinned by
// literals: a v10 record read at today's slot count would decode at the wrong size the
// next time that count moves.
func TestEachMigratedFormatDecodesAtTheSlotCountItWasWrittenWith(t *testing.T) {
	t.Parallel()

	for version, want := range map[uint32]int{previousStoreVersion: 39, 10: 40, 11: 40} {
		if got, migrates := migratedInventorySlots(version); !migrates || got != want {
			t.Errorf("format %d migrates at %d slots (migrates %v), want %d", version, got, migrates, want)
		}
	}
	for _, version := range []uint32{2, 6, 8, 9, StoreVersion} {
		if slots, migrates := migratedInventorySlots(version); migrates {
			t.Errorf("format %d claims a %d-slot migration it does not have", version, slots)
		}
	}
	for _, version := range losslessMigrationSources {
		if _, migrates := migratedInventorySlots(version); !migrates {
			t.Errorf("lossless source format %d has no slot count", version)
		}
	}
}

// A world that has run a ruin has issued a generation, which makes receipts strict for
// ever. Its next record-format bump must still start: every character is carried, and a
// prepared reward is replayed against the migrated record rather than against a file
// recovery could not read.
func TestAWorldWithBossReceiptsMigratesEveryCharacterBeforeRecovery(t *testing.T) {
	for _, version := range losslessMigrationSources {
		for _, prepared := range []bool{false, true} {
			t.Run(fmt.Sprintf("v%d/prepared=%v", version, prepared), func(t *testing.T) {
				players, rewards, dir, owner, ref := transitionFixture(t)
				var post Record
				if prepared {
					_, post = prepareTransition(t, players, rewards, owner, ref)
				}
				slots, _ := migratedInventorySlots(version)

				want := make(map[CharacterID]Record)
				originals := make(map[CharacterID][]byte)
				for id := range players.byID {
					rec, found, err := players.Load(id)
					if err != nil || !found {
						t.Fatalf("loading fixture character %s: found %v, err %v", id, found, err)
					}
					// The old layout's last slot, durable, so the tail is what is checked.
					rec.Slots[slots-1] = protocol.InventoryStack{ItemID: 23, Count: 1, Durability: 40, MaxDurability: 100}
					old := encodeRecordLayout(rec, version, slots)
					if err := os.WriteFile(players.recordPath(id), old, 0o600); err != nil {
						t.Fatal(err)
					}
					want[id], originals[id] = rec, old
				}
				if prepared {
					want[owner.ID] = post
				}

				cold, err := OpenRewardStore(dir)
				if err != nil {
					t.Fatal(err)
				}
				restored, err := OpenStoreWithRewardRecovery(dir, cold, recoveryValidator)
				if err != nil {
					t.Fatalf("a v%d world with boss receipts refused to start: %v", version, err)
				}
				if !restored.strictRewards.Load() {
					t.Fatal("the fixture world does not hold strict receipts")
				}
				if restored.SetAside() == "" {
					t.Fatal("no migration ran")
				}
				for id, rec := range want {
					got, found, err := restored.Load(id)
					if err != nil || !found || got != rec {
						t.Errorf("character %s after migration: found %v, err %v\n got  %+v\n want %+v", id, found, err, got, rec)
					}
					if _, known := restored.Character(id); !known {
						t.Errorf("character %s is missing from the index", id)
					}
				}
				for id, old := range originals {
					kept, err := os.ReadFile(filepath.Join(restored.SetAside(), id.String()+recordFileExt))
					if err != nil || !bytes.Equal(kept, old) {
						t.Errorf("character %s was not kept byte-for-byte aside: %v", id, err)
					}
				}
				if snap, _ := cold.Snapshot(); len(snap.Intents) != 0 {
					t.Errorf("%d reward intents remain unacknowledged", len(snap.Intents))
				}
			})
		}
	}
}

// Strict receipts narrow the migration rather than disabling it: whatever would stay in
// the directory kept aside is a character this world would then treat as new, so the
// start is refused and nothing moves.
func TestAWorldWithBossReceiptsRefusesAMigrationThatLeavesACharacterBehind(t *testing.T) {
	for name, write := range map[string]func(Record) []byte{
		"a format with no migration": func(rec Record) []byte {
			return encodeRecordLayout(rec, 6, 36)
		},
		"a v7 silver stack": func(rec Record) []byte {
			rec.Slots[3] = protocol.InventoryStack{ItemID: previousSilverItemID, Count: 37}
			return encodeRecordLayout(rec, previousStoreVersion, previousInventorySlots)
		},
		"an unreadable v7 record": func(rec Record) []byte {
			old := encodeRecordLayout(rec, previousStoreVersion, previousInventorySlots)
			old[len(old)-1] ^= 1
			return old
		},
		// A v10 record always migrates, so its corruption is refused in any world. Under
		// strict receipts it is refused with the same error as every other leave-behind.
		"a corrupt v10 record": func(rec Record) []byte {
			old := encodeRecordLayout(rec, 10, 40)
			old[len(old)-1] ^= 1
			return old
		},
	} {
		t.Run(name, func(t *testing.T) {
			players, _, dir, owner, _ := transitionFixture(t)
			rec, _, err := players.Load(owner.ID)
			if err != nil {
				t.Fatal(err)
			}
			old := write(rec)
			if err := os.WriteFile(players.recordPath(owner.ID), old, 0o600); err != nil {
				t.Fatal(err)
			}

			cold, err := OpenRewardStore(dir)
			if err != nil {
				t.Fatal(err)
			}
			if _, err := OpenStoreWithRewardRecovery(dir, cold, recoveryValidator); !errors.Is(err, ErrRewardRecoveryRequired) {
				t.Fatalf("OpenStoreWithRewardRecovery = %v, want ErrRewardRecoveryRequired", err)
			}
			if kept, err := os.ReadFile(players.recordPath(owner.ID)); err != nil || !bytes.Equal(kept, old) {
				t.Errorf("the refused record did not stay in place byte-for-byte: %v", err)
			}
			if moved, _ := filepath.Glob(filepath.Join(dir, playersDirName+supersededSuffix+"*")); len(moved) != 0 {
				t.Errorf("a refused start still set a directory aside: %v", moved)
			}
		})
	}
}

// sealJournalPostimageIn rewrites the one postimage in the world's reward journal as
// though an older build had sealed it: the same record in that format, its length prefix
// and the journal checksum updated to match.
func sealJournalPostimageIn(t *testing.T, dir string, post Record, version uint32) {
	t.Helper()

	path := filepath.Join(dir, rewardFileName)
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	current := encodeRecord(post)
	if bytes.Count(data, current) != 1 {
		t.Fatal("the journal fixture does not hold the postimage exactly once")
	}
	at := bytes.Index(data, current)
	slots, _ := migratedInventorySlots(version)
	old := encodeRecordLayout(post, version, slots)
	rewritten := append([]byte(nil), data[:at-4]...)
	rewritten = binary.LittleEndian.AppendUint32(rewritten, uint32(len(old)))
	rewritten = append(rewritten, old...)
	rewritten = append(rewritten, data[at+len(current):]...)
	world.PutChecksum(rewritten)
	if err := os.WriteFile(path, rewritten, 0o600); err != nil {
		t.Fatal(err)
	}
}

// A reward prepared by a v11 build is sealed as a v11 record. The first v12 start is the
// one whose job is to replay it, so the journal must still open, read the postimage back
// whole with the new slot empty, and replay it against the migrated character.
func TestAJournalSealedBeforeTheSlotTableGrewStillReplays(t *testing.T) {
	players, rewards, dir, owner, ref := transitionFixture(t)
	_, post := prepareTransition(t, players, rewards, owner, ref)
	for id := range players.byID {
		rec, _, err := players.Load(id)
		if err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(players.recordPath(id), encodeRecordLayout(rec, 11, 40), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	sealJournalPostimageIn(t, dir, post, 11)

	cold, err := OpenRewardStore(dir)
	if err != nil {
		t.Fatalf("a journal holding a v11 postimage refused to open: %v", err)
	}
	if snap, _ := cold.Snapshot(); len(snap.Intents) != 1 || snap.Intents[0].Postimage != post {
		t.Fatalf("the v11 postimage did not read back whole: %+v", snap.Intents)
	}
	restored, err := OpenStoreWithRewardRecovery(dir, cold, recoveryValidator)
	if err != nil {
		t.Fatalf("a v11 world with a prepared reward refused to start: %v", err)
	}
	if got, found, err := restored.Load(owner.ID); err != nil || !found || got != post {
		t.Errorf("the replayed character: found %v, err %v\n got  %+v\n want %+v", found, err, got, post)
	}
	if snap, _ := cold.Snapshot(); len(snap.Intents) != 0 {
		t.Errorf("%d reward intents remain unacknowledged", len(snap.Intents))
	}
}

// A postimage is a receipt, so a format that cannot carry the reward epoch is not one a
// postimage was ever written in: the journal holding it is corrupt.
func TestAJournalPostimageInAFormatWithoutAnEpochIsRefused(t *testing.T) {
	players, rewards, dir, owner, ref := transitionFixture(t)
	_, post := prepareTransition(t, players, rewards, owner, ref)
	sealJournalPostimageIn(t, dir, post, 10)

	if _, err := OpenRewardStore(dir); !errors.Is(err, world.ErrCorruptStore) {
		t.Fatalf("OpenRewardStore = %v, want world.ErrCorruptStore", err)
	}
}
