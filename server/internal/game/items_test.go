package game

import (
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func TestEveryItemIsRegisteredWithItsOwnStackLimitAndPlacement(t *testing.T) {
	t.Parallel()

	want := map[ItemID]world.Block{
		ItemStone:   world.Stone,
		ItemDirt:    world.Dirt,
		ItemSnow:    world.Snow,
		ItemLog:     world.Log,
		ItemRawCoal: world.Air,
		ItemRawIron: world.Air,
	}
	if _, registered := itemByID(ItemNone); registered {
		t.Fatal("item id 0 is registered as a real item")
	}
	for itemID, places := range want {
		definition, registered := itemByID(itemID)
		if !registered {
			t.Errorf("item %d is not registered", itemID)
			continue
		}
		if definition.places != places {
			t.Errorf("item %d places block %d, want %d", itemID, definition.places, places)
		}
		if definition.maxStack != 64 {
			t.Errorf("item %d has max stack %d, want 64", itemID, definition.maxStack)
		}
	}
	for _, raw := range []ItemID{ItemRawCoal, ItemRawIron} {
		if block, placeable := blockPlacedBy(raw); placeable || block != world.Air {
			t.Errorf("raw item %d places block %d (placeable %v), want nothing", raw, block, placeable)
		}
	}
}

func TestBothMeatsCarryTheirPinnedIDsAndResourceStats(t *testing.T) {
	t.Parallel()

	if ItemRawMeat != 19 {
		t.Errorf("raw meat id = %d, want the appended wire id 19", ItemRawMeat)
	}
	got, registered := itemByID(ItemRawMeat)
	if !registered {
		t.Fatal("raw meat is not registered")
	}
	want := itemDefinition{places: world.Air, maxStack: 16, restoresHunger: RawMeatHungerRestore}
	if got != want {
		t.Errorf("raw meat row = %+v, want %+v", got, want)
	}
	if block, placeable := blockPlacedBy(ItemRawMeat); placeable || block != world.Air {
		t.Errorf("raw meat places block %d (placeable %v)", block, placeable)
	}

	if ItemCookedMeat != 20 {
		t.Errorf("cooked meat id = %d, want the appended wire id 20", ItemCookedMeat)
	}
	got, registered = itemByID(ItemCookedMeat)
	if !registered {
		t.Fatal("cooked meat is not registered")
	}
	want = itemDefinition{places: world.Air, maxStack: 16, restoresHunger: CookedMeatHungerRestore}
	if got != want {
		t.Errorf("cooked meat row = %+v, want %+v", got, want)
	}
	if block, placeable := blockPlacedBy(ItemCookedMeat); placeable || block != world.Air {
		t.Errorf("cooked meat places block %d (placeable %v)", block, placeable)
	}
}

func TestRawAndCookedMeatAreTheOnlyFoods(t *testing.T) {
	t.Parallel()

	if RawMeatHungerRestore != 25 {
		t.Errorf("RawMeatHungerRestore = %d, want the pinned 25", RawMeatHungerRestore)
	}
	if CookedMeatHungerRestore != 100 {
		t.Errorf("CookedMeatHungerRestore = %d, want the pinned 100", CookedMeatHungerRestore)
	}
	for id, definition := range itemRegistry {
		want := uint16(0)
		switch id {
		case ItemRawMeat:
			want = RawMeatHungerRestore
		case ItemCookedMeat:
			want = CookedMeatHungerRestore
		}
		if definition.restoresHunger != want {
			t.Errorf("item %d restores %d hunger, want %d", id, definition.restoresHunger, want)
		}
	}
}

func TestDropTableCoversEveryBlockOutcome(t *testing.T) {
	t.Parallel()

	want := map[world.Block]ItemID{
		world.Stone:   ItemStone,
		world.Dirt:    ItemDirt,
		world.Grass:   ItemDirt,
		world.Snow:    ItemSnow,
		world.Log:     ItemLog,
		world.Leaves:  ItemNone,
		world.CoalOre: ItemRawCoal,
		world.IronOre: ItemRawIron,
	}
	for block, itemID := range want {
		if got := itemDroppedBy(block); got != itemID {
			t.Errorf("block %d drops item %d, want %d", block, got, itemID)
		}
	}
	if got := itemDroppedBy(world.Air); got != ItemNone {
		t.Errorf("Air drops item %d, want nothing", got)
	}
}

func TestBlockExperienceNamesEveryRewardAndExplicitZero(t *testing.T) {
	t.Parallel()

	want := map[world.Block]uint16{
		world.Stone:   0,
		world.Dirt:    0,
		world.Grass:   0,
		world.Snow:    0,
		world.Log:     2,
		world.Leaves:  0,
		world.CoalOre: 4,
		world.IronOre: 6,
	}
	if len(blockExperience) != len(want) {
		t.Fatalf("block experience has %d rows, want %d explicit decisions", len(blockExperience), len(want))
	}
	for block, amount := range want {
		got, present := blockExperience[block]
		if !present {
			t.Errorf("block %d has no explicit experience decision", block)
			continue
		}
		if got != amount {
			t.Errorf("block %d awards %d experience, want %d", block, got, amount)
		}
		if _, breakable := blockDrops[block]; !breakable {
			t.Errorf("block %d has an experience row but no break outcome", block)
		}
	}
	for block := range blockDrops {
		if _, present := blockExperience[block]; !present {
			t.Errorf("breakable block %d has no experience row", block)
		}
	}
}
