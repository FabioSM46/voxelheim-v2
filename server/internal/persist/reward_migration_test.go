package persist

import (
	"bytes"
	"errors"
	"os"
	"path/filepath"
	"reflect"
	"testing"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func TestV10MigrationPreservesEveryCharacterFieldAndAddsZeroEpoch(t *testing.T) {
	s, dir := openStore(t)
	current := newCharacter(t, s, testID(1), "Current")
	c := newCharacter(t, s, testID(2), "Earlier")
	want, _, _ := s.Load(c.ID)
	want.LastSeen = time.Unix(123, 0).UTC()
	want.Pos = [3]float64{1.5, 64, -2.5}
	want.Yaw = .7
	want.Health = 70
	want.Hunger = 60
	want.Experience = 900
	want.Silver = 101
	want.LearnedMounts = 5
	want.Slots[39] = protocol.InventoryStack{ItemID: 1, Count: 2}
	want.Slots[0] = protocol.InventoryStack{ItemID: 2, Count: 1, Durability: 3, MaxDurability: 4}
	old := encodeRecordLayout(want, 10, int(protocol.InventorySlots))
	if len(old) != len(encodeRecord(want))-8 {
		t.Fatal("v10 fixture contains an epoch")
	}
	if err := os.WriteFile(s.recordPath(c.ID), old, 0600); err != nil {
		t.Fatal(err)
	}
	currentBytes, err := os.ReadFile(s.recordPath(current.ID))
	if err != nil {
		t.Fatal(err)
	}
	reopened, err := OpenStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	got, found, err := reopened.Load(c.ID)
	if err != nil || !found || !reflect.DeepEqual(got, want) {
		t.Fatalf("lossy v10 migration: %+v %v", got, err)
	}
	now, _ := os.ReadFile(reopened.recordPath(current.ID))
	if !bytes.Equal(now, currentBytes) {
		t.Fatal("mixed current record rewritten")
	}
	kept, _ := os.ReadFile(filepath.Join(reopened.SetAside(), filepath.Base(s.recordPath(c.ID))))
	if !bytes.Equal(kept, old) {
		t.Fatal("migration did not retain original bytes")
	}
	again, err := OpenStore(dir)
	if err != nil || again.SetAside() != "" {
		t.Fatal("migration repeated")
	}
}

func TestCorruptV10MigrationRefusesBeforeMovingTheDirectory(t *testing.T) {
	for _, oversize := range []bool{false, true} {
		s, dir := openStore(t)
		c := newCharacter(t, s, testID(1), "Eivor")
		rec, _, _ := s.Load(c.ID)
		old := encodeRecordLayout(rec, 10, int(protocol.InventorySlots))
		if oversize {
			old = append(old, make([]byte, maxRecordSize)...)
		} else {
			old[len(old)-1] ^= 1
		}
		if err := os.WriteFile(s.recordPath(c.ID), old, 0600); err != nil {
			t.Fatal(err)
		}
		if _, err := OpenStore(dir); !errors.Is(err, world.ErrCorruptStore) {
			t.Fatalf("corrupt v10 opened: %v", err)
		}
		kept, err := os.ReadFile(s.recordPath(c.ID))
		if err != nil || !bytes.Equal(kept, old) {
			t.Fatal("failed migration moved evidence")
		}
	}
}
