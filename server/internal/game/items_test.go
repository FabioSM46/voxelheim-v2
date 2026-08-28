package game

import (
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
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

func TestTheSixArmourPiecesCarryTheirPinnedIDsAndStats(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		item ItemID
		id   ItemID
		want itemDefinition
	}{
		{ItemLeatherCap, 21, itemDefinition{places: world.Air, maxStack: 1, wornAt: wornHead, armour: 5, maxDurability: LeatherArmourMaxDurability}},
		{ItemLeatherJerkin, 22, itemDefinition{places: world.Air, maxStack: 1, wornAt: wornChest, armour: 5, maxDurability: LeatherArmourMaxDurability}},
		{ItemLeatherLeggings, 23, itemDefinition{places: world.Air, maxStack: 1, wornAt: wornLegs, armour: 5, maxDurability: LeatherArmourMaxDurability}},
		{ItemIronHelm, 24, itemDefinition{places: world.Air, maxStack: 1, wornAt: wornHead, armour: 10, threat: 5, maxDurability: IronArmourMaxDurability}},
		{ItemIronCuirass, 25, itemDefinition{places: world.Air, maxStack: 1, wornAt: wornChest, armour: 10, threat: 5, maxDurability: IronArmourMaxDurability}},
		{ItemIronGreaves, 26, itemDefinition{places: world.Air, maxStack: 1, wornAt: wornLegs, armour: 10, threat: 5, maxDurability: IronArmourMaxDurability}},
	} {
		if tc.item != tc.id {
			t.Errorf("armour item id = %d, want appended wire id %d", tc.item, tc.id)
		}
		got, registered := itemByID(tc.item)
		if !registered {
			t.Errorf("armour item %d is not registered", tc.item)
			continue
		}
		if got != tc.want {
			t.Errorf("armour item %d row = %+v, want %+v", tc.item, got, tc.want)
		}
	}
}

func TestRangedItemsCarryTheirPinnedIDsAndLauncherStats(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		item ItemID
		id   ItemID
		want itemDefinition
	}{
		{ItemBow, 28, itemDefinition{places: world.Air, maxStack: 1, maxDurability: BowMaxDurability, launches: vnet.ProjectileKindArrow, ammunition: ItemArrow}},
		{ItemArrow, 29, itemDefinition{places: world.Air, maxStack: 32}},
		{ItemWoodenSceptre, 30, itemDefinition{places: world.Air, maxStack: 1, maxDurability: SceptreMaxDurability, launches: vnet.ProjectileKindEnergyOrb}},
	} {
		if tc.item != tc.id {
			t.Errorf("item id = %d, want appended wire id %d", tc.item, tc.id)
		}
		got, registered := itemByID(tc.item)
		if !registered {
			t.Errorf("item %d is not registered", tc.item)
			continue
		}
		if got != tc.want {
			t.Errorf("item %d row = %+v, want %+v", tc.item, got, tc.want)
		}
	}
}

func TestOnlyWearableItemsCarryWornStatsAndEveryWearableIsDurable(t *testing.T) {
	t.Parallel()

	for id, definition := range itemRegistry {
		hasWornStats := definition.armour != 0 || definition.threat != 0 || definition.blockFraction != 0
		wearable := definition.wornAt != wornNowhere
		if hasWornStats != wearable {
			t.Errorf("item %d wornAt=%d armour=%d threat=%d block=%d; want stats iff wearable",
				id, definition.wornAt, definition.armour, definition.threat, definition.blockFraction)
		}
		if definition.blockFraction > 100 {
			t.Errorf("item %d blocks %d%%, want at most 100%%", id, definition.blockFraction)
		}
		if wearable && definition.maxDurability == 0 {
			t.Errorf("wearable item %d has no durability", id)
		}
	}
}

func TestWoodenShieldHasItsPinnedIDAndStats(t *testing.T) {
	t.Parallel()
	if ItemWoodenShield != 27 {
		t.Fatalf("wooden shield id = %d, want appended id 27", ItemWoodenShield)
	}
	want := itemDefinition{places: world.Air, maxStack: 1, wornAt: wornOffHand, maxDurability: WoodenShieldMaxDurability, blockFraction: 50}
	if got, ok := itemByID(ItemWoodenShield); !ok || got != want {
		t.Fatalf("wooden shield row = %+v, known=%v; want %+v", got, ok, want)
	}
}

// One item may occupy each body slot, so the strongest possible set is the
// strongest registered row for head, chest, legs and off-hand. Sweep that combination
// rather than summing the catalogue: adding a second helmet must not consume armour budget
// when the player can never wear both helmets at once.
func TestEveryWearableCombinationFitsTheArmourScale(t *testing.T) {
	t.Parallel()

	var strongest [wornOffHand + 1]uint16
	for id, definition := range itemRegistry {
		if definition.wornAt == wornNowhere {
			continue
		}
		if definition.wornAt > wornOffHand {
			t.Errorf("wearable item %d names unknown body slot %d", id, definition.wornAt)
			continue
		}
		strongest[definition.wornAt] = max(strongest[definition.wornAt], definition.armour)
	}

	sum := uint32(strongest[wornHead]) + uint32(strongest[wornChest]) +
		uint32(strongest[wornLegs]) + uint32(strongest[wornOffHand])
	if sum >= uint32(ArmourScale) {
		t.Errorf("the strongest wearable combination carries %d armour points against scale %d", sum, ArmourScale)
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

// The three blocks worldgen 3 put in the ground, at the ids the registry appended
// them at and with the plain block-item row each of them has.
//
// **The ids are pinned by name.** Item ids cross the wire inside inventories that
// are already persisted, so an insertion above any of these renumbers a saved pack;
// the id is the one fact about a block item that a later edit cannot be allowed to
// choose freely, and stating it here is what makes an insertion a failing test
// rather than a silent loss.
func TestTheThreeGroundBlocksOfWorldgenThreeCarryTheirPinnedIDs(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		item   ItemID
		id     ItemID
		places world.Block
	}{
		{ItemSand, 31, world.Sand},
		{ItemSandstone, 32, world.Sandstone},
		{ItemGravel, 33, world.Gravel},
	} {
		if tc.item != tc.id {
			t.Errorf("item id = %d, want appended wire id %d", tc.item, tc.id)
		}
		got, registered := itemByID(tc.item)
		if !registered {
			t.Errorf("item %d is not registered", tc.item)
			continue
		}
		want := itemDefinition{places: tc.places, maxStack: 64}
		if got != want {
			t.Errorf("item %d row = %+v, want %+v", tc.item, got, want)
		}
		// The registry proposes and world.Placeable disposes; both have to say yes
		// before a place request can put one of these in the ground.
		block, placeable := blockPlacedBy(tc.item)
		if !placeable || block != tc.places {
			t.Errorf("item %d places block %d (placeable %v), want %d", tc.item, block, placeable, tc.places)
		}
		// Breaking one gives back exactly the block that was broken, which is what
		// makes a desert something you can carry home and build with.
		if dropped := itemDroppedBy(tc.places); dropped != tc.item {
			t.Errorf("block %d drops item %d, want %d", tc.places, dropped, tc.item)
		}
	}
}

func TestDropTableCoversEveryBlockOutcome(t *testing.T) {
	t.Parallel()

	want := map[world.Block]ItemID{
		world.Stone:     ItemStone,
		world.Dirt:      ItemDirt,
		world.Grass:     ItemDirt,
		world.Snow:      ItemSnow,
		world.Log:       ItemLog,
		world.Leaves:    ItemNone,
		world.CoalOre:   ItemRawCoal,
		world.IronOre:   ItemRawIron,
		world.Sand:      ItemSand,
		world.Sandstone: ItemSandstone,
		world.Gravel:    ItemGravel,
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
		world.Stone:     0,
		world.Dirt:      0,
		world.Grass:     0,
		world.Snow:      0,
		world.Log:       2,
		world.Leaves:    0,
		world.CoalOre:   4,
		world.IronOre:   6,
		world.Sand:      0,
		world.Sandstone: 0,
		world.Gravel:    0,
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
