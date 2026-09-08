package persist

import (
	"bytes"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func TestInterruptedPlayerMigrationRefusesBeforeRecreatingPlayers(t *testing.T) {
	for _, installed := range []bool{false, true} {
		t.Run(fmt.Sprint(installed), func(t *testing.T) {
			s, dir := openStore(t)
			c := newCharacter(t, s, testID(1), "Eivor")
			rec, _, _ := s.Load(c.ID)
			old := encodeRecordLayout(rec, 10, int(protocol.InventorySlots))
			name := filepath.Base(s.recordPath(c.ID))
			if err := os.WriteFile(s.recordPath(c.ID), old, 0600); err != nil {
				t.Fatal(err)
			}
			aside := filepath.Join(dir, fmt.Sprintf("players.pre-v%d.42", StoreVersion))
			stage := filepath.Join(dir, fmt.Sprintf("players.migrate-v%d.42", StoreVersion))
			if err := os.Mkdir(stage, 0755); err != nil {
				t.Fatal(err)
			}
			if err := world.WriteAtomic(filepath.Join(stage, name), encodeRecord(rec)); err != nil {
				t.Fatal(err)
			}
			if err := world.WriteAtomic(filepath.Join(dir, "players.migration"), []byte("prepared migration")); err != nil {
				t.Fatal(err)
			}
			if err := os.Rename(s.dir, aside); err != nil {
				t.Fatal(err)
			}
			if installed {
				if err := os.Rename(stage, s.dir); err != nil {
					t.Fatal(err)
				}
			}
			if _, err := OpenStore(dir); err == nil {
				t.Fatal("interrupted migration started as an empty/new world")
			}
			if !installed {
				if _, err := os.Stat(s.dir); !errors.Is(err, os.ErrNotExist) {
					t.Fatal("startup created an empty players directory in the rename gap")
				}
			}
			kept, err := os.ReadFile(filepath.Join(aside, name))
			if err != nil || !bytes.Equal(kept, old) {
				t.Fatal("interrupted migration lost original records")
			}
			target := stage
			if installed {
				target = s.dir
			}
			if _, err := os.Stat(filepath.Join(target, name)); err != nil {
				t.Fatal("startup removed prepared records")
			}
		})
	}
}

func TestMigrationMarkerFailureDoesNotMoveSourceRecords(t *testing.T) {
	for _, afterWrite := range []bool{false, true} {
		t.Run(fmt.Sprint(afterWrite), func(t *testing.T) {
			s, dir := openStore(t)
			c := newCharacter(t, s, testID(1), "Eivor")
			rec, _, _ := s.Load(c.ID)
			old := encodeRecordLayout(rec, 10, int(protocol.InventorySlots))
			if err := os.WriteFile(s.recordPath(c.ID), old, 0600); err != nil {
				t.Fatal(err)
			}
			failure := errors.New("synthetic marker sync failure")
			s.migrationWriter = func(path string, b []byte) error {
				if afterWrite {
					if err := world.WriteAtomic(path, b); err != nil {
						return err
					}
				}
				return failure
			}
			if _, err := s.setAsideSuperseded(); !errors.Is(err, failure) {
				t.Fatalf("marker failure ignored: %v", err)
			}
			kept, err := os.ReadFile(s.recordPath(c.ID))
			if err != nil || !bytes.Equal(kept, old) {
				t.Fatal("marker failure mutated source")
			}
			if afterWrite {
				if _, err := OpenStore(dir); !errors.Is(err, ErrPlayerMigrationInterrupted) {
					t.Fatal("uncertain marker ignored on startup")
				}
			}
		})
	}
}

func TestCompletedMigrationSyncFailureKeepsTheStartupFence(t *testing.T) {
	s, dir := openStore(t)
	c := newCharacter(t, s, testID(1), "Eivor")
	rec, _, _ := s.Load(c.ID)
	if err := os.WriteFile(s.recordPath(c.ID), encodeRecordLayout(rec, 10, int(protocol.InventorySlots)), 0600); err != nil {
		t.Fatal(err)
	}
	writes := 0
	failure := errors.New("synthetic completed-directory sync failure")
	s.migrationWriter = func(path string, b []byte) error {
		writes++
		if err := world.WriteAtomic(path, b); err != nil {
			return err
		}
		if writes == 2 {
			return failure
		}
		return nil
	}
	if _, err := s.setAsideSuperseded(); !errors.Is(err, failure) {
		t.Fatal("completed directory sync error ignored")
	}
	if got, found, err := s.Load(c.ID); err != nil || !found || got.Name != rec.Name {
		t.Fatal("fixture did not install new records")
	}
	if _, err := OpenStore(dir); !errors.Is(err, ErrPlayerMigrationInterrupted) {
		t.Fatal("uncertain completed migration started")
	}
}
