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

func TestRawMeatCarriesItsPinnedIDAndResourceStats(t *testing.T) {
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
}

func TestRawMeatIsTheOnlyFood(t *testing.T) {
	t.Parallel()

	if RawMeatHungerRestore != 25 {
		t.Errorf("RawMeatHungerRestore = %d, want the pinned 25", RawMeatHungerRestore)
	}
	for id, definition := range itemRegistry {
		want := uint16(0)
		if id == ItemRawMeat {
			want = RawMeatHungerRestore
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
