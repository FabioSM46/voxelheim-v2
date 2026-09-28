package game

import (
	"reflect"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func TestChestBlocksCannotBeMinedPlacedOrOverwritten(t *testing.T) {
	sim, _, _, _ := newMiningPlayer(t, nil)
	for _, block := range []world.Block{world.Chest, world.ChestOpen} {
		if _, ok := sim.hardnessTicks(block, ItemNone); ok {
			t.Errorf("chest %d has a mining row", block)
		}
		if world.Placeable(block) || allowPlacement(block) == nil {
			t.Errorf("chest %d permits ordinary placement", block)
		}
		if drop, ok := blockDrops[block]; !ok || drop != ItemNone {
			t.Errorf("chest %d drop = %d, present %v", block, drop, ok)
		}
		if xp, ok := blockExperience[block]; !ok || xp != 0 {
			t.Errorf("chest %d experience = %d, present %v", block, xp, ok)
		}
	}
}

func TestChestLootBoundsChoicesAndDeterminism(t *testing.T) {
	bounds := [world.InstanceChestCount][2]uint32{{8, 15}, {12, 20}, {20, 30}}
	items := [world.InstanceChestCount]map[ItemID]bool{
		{ItemArrow: true},
		{ItemSharpeningStone: true, ItemLeatherPatch: true},
		{ItemRustyHelm: true, ItemRustyCuirass: true, ItemRustyGreaves: true},
	}
	left, right := &Sim{loot: newLootRNG(42)}, &Sim{loot: newLootRNG(42)}
	for index, table := range chestLootTables {
		seen := map[ItemID]bool{}
		for range 128 {
			got := left.rollTableLocked(table)
			if again := right.rollTableLocked(table); !reflect.DeepEqual(got, again) {
				t.Fatal("same seed and table sequence produced different loot")
			}
			if got.silver < bounds[index][0] || got.silver > bounds[index][1] || got.revision != 1 || len(got.entries) != 1 {
				t.Fatalf("chest %d rolled %+v", index, got)
			}
			entry := got.entries[0]
			if entry.entryID != 1 || !items[index][entry.stack.item] {
				t.Fatalf("chest %d rolled unexpected entry %+v", index, entry)
			}
			seen[entry.stack.item] = true
			if index == world.AntechamberChest {
				if entry.stack.count < 8 || entry.stack.count > 12 {
					t.Fatalf("arrow refill count = %d", entry.stack.count)
				}
			} else if entry.stack.count != 1 {
				t.Fatalf("choice produced %d items", entry.stack.count)
			}
			if index == world.KingChest && (entry.stack.durability != RustyArmourMaxDurability || entry.stack.maxDurability != RustyArmourMaxDurability) {
				t.Fatalf("armour not fresh: %+v", entry.stack)
			}
		}
		if len(seen) != len(items[index]) {
			t.Fatalf("chest %d never rolled every choice: %v", index, seen)
		}
	}
}
