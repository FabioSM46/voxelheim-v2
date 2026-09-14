package persist

import (
	"bytes"
	"errors"
	"fmt"
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
	old := encodeRecordLayout(want, 10, 40)
	if len(old) != len(encodeRecord(want))-8-slotSize {
		t.Fatal("v10 fixture contains an epoch or a 41st slot")
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

// Both 40-slot formats reach 41 slots losslessly: every item, count and durability
// stays in the slot it was in, the new main-hand slot is empty, and every other field —
// v11's reward epoch included — survives. A v10 record decoded at 41 slots would fail its
// size check here and refuse the start, which is the hazard the literal 40 prevents.
func TestFortySlotRecordsMigrateToFortyOneWithEverySlotInPlace(t *testing.T) {
	for _, version := range []uint32{10, 11} {
		t.Run(fmt.Sprintf("v%d", version), func(t *testing.T) {
			s, dir := openStore(t)
			c := newCharacter(t, s, testID(2), "Earlier")
			want, _, _ := s.Load(c.ID)
			want.Health, want.Hunger, want.Experience = 70, 60, 900
			want.Silver, want.LearnedMounts = 101, 5
			if version >= 11 {
				want.BossRewardEpoch = 7
			}
			for slot := range 40 {
				want.Slots[slot] = protocol.InventoryStack{
					ItemID: uint16(slot + 1), Count: 1, Durability: uint16(slot*3 + 1), MaxDurability: 200,
				}
			}
			old := encodeRecordLayout(want, version, 40)
			if err := os.WriteFile(s.recordPath(c.ID), old, 0o600); err != nil {
				t.Fatal(err)
			}

			reopened, err := OpenStore(dir)
			if err != nil {
				t.Fatalf("a v%d store refused to open: %v", version, err)
			}
			got, found, err := reopened.Load(c.ID)
			if err != nil || !found {
				t.Fatalf("loading the migrated character: found %v, err %v", found, err)
			}
			for slot := range 40 {
				if got.Slots[slot] != want.Slots[slot] {
					t.Errorf("slot %d is %+v, want %+v", slot, got.Slots[slot], want.Slots[slot])
				}
			}
			if got.Slots[40] != (protocol.InventoryStack{}) {
				t.Errorf("the new main-hand slot is %+v, want empty", got.Slots[40])
			}
			if !reflect.DeepEqual(got, want) {
				t.Errorf("lossy v%d migration:\n got  %+v\n want %+v", version, got, want)
			}
			if kept, err := os.ReadFile(filepath.Join(reopened.SetAside(), filepath.Base(s.recordPath(c.ID)))); err != nil || !bytes.Equal(kept, old) {
				t.Errorf("the v%d record was not kept byte-for-byte aside: %v", version, err)
			}
			if again, err := OpenStore(dir); err != nil || again.SetAside() != "" {
				t.Errorf("reopening migrated again: aside %q, err %v", again.SetAside(), err)
			}
		})
	}
}

func TestCorruptV10MigrationRefusesBeforeMovingTheDirectory(t *testing.T) {
	for _, oversize := range []bool{false, true} {
		s, dir := openStore(t)
		c := newCharacter(t, s, testID(1), "Eivor")
		rec, _, _ := s.Load(c.ID)
		old := encodeRecordLayout(rec, 10, 40)
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
