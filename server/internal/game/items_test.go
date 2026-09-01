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

func TestTheRunestoneCarriesItsAppendedIDAndRegistryRow(t *testing.T) {
	t.Parallel()

	if ItemRunestone != 39 {
		t.Errorf("runestone item id = %d, want appended wire id 39", ItemRunestone)
	}
	got, registered := itemByID(ItemRunestone)
	if !registered {
		t.Fatal("the runestone is not registered")
	}
	want := itemDefinition{places: world.Air, maxStack: 1}
	if got != want {
		t.Errorf("runestone registry row = %+v, want %+v", got, want)
	}
}

func TestPalmLogCarriesItsAppendedIDAndRegistryRow(t *testing.T) {
	t.Parallel()

	if ItemPalmLog != 40 {
		t.Errorf("palm log item id = %d, want appended wire id 40", ItemPalmLog)
	}
	got, registered := itemByID(ItemPalmLog)
	if !registered {
		t.Fatal("the palm log is not registered")
	}
	want := itemDefinition{places: world.PalmLog, maxStack: 64}
	if got != want {
		t.Errorf("palm log registry row = %+v, want %+v", got, want)
	}
	if block, placeable := blockPlacedBy(ItemPalmLog); !placeable || block != world.PalmLog {
		t.Errorf("palm log places block %d (placeable %v), want PalmLog", block, placeable)
	}
	for _, block := range []world.Block{world.PalmFronds, world.DesertShrub} {
		if item := itemDroppedBy(block); item != ItemNone {
			t.Errorf("block %d drops item %d, want none", block, item)
		}
	}
}

func TestTheThreeHorseTokensCarryAppendedIDsAndDifferOnlyByColour(t *testing.T) {
	t.Parallel()

	want := []struct {
		item  ItemID
		id    ItemID
		mount vnet.MountKind
	}{
		{ItemBlackHorse, 41, vnet.MountKindBlackHorse},
		{ItemBrownHorse, 42, vnet.MountKindBrownHorse},
		{ItemGreyHorse, 43, vnet.MountKindGreyHorse},
	}
	base := itemDefinition{places: world.Air, maxStack: 1}
	for _, tc := range want {
		if tc.item != tc.id {
			t.Errorf("%s token id = %d, want appended wire id %d", tc.mount, tc.item, tc.id)
		}
		definition, registered := itemByID(tc.item)
		if !registered {
			t.Errorf("%s token is not registered", tc.mount)
			continue
		}
		if definition.learnsMount != tc.mount {
			t.Errorf("item %d learns %s, want %s", tc.item, definition.learnsMount, tc.mount)
		}
		definition.learnsMount = vnet.MountKindUnknown
		if definition != base {
			t.Errorf("%s token has non-colour stats %+v, want %+v", tc.mount, definition, base)
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

func TestOnlyTheThreeHorseTokensTeachMounts(t *testing.T) {
	t.Parallel()

	want := map[ItemID]vnet.MountKind{
		ItemBlackHorse: vnet.MountKindBlackHorse,
		ItemBrownHorse: vnet.MountKindBrownHorse,
		ItemGreyHorse:  vnet.MountKindGreyHorse,
	}
	for id, definition := range itemRegistry {
		if definition.learnsMount != want[id] {
			t.Errorf("item %d learns %s, want %s", id, definition.learnsMount, want[id])
		}
		if definition.learnsMount != vnet.MountKindUnknown && definition.restoresHunger != 0 {
			t.Errorf("item %d both teaches %s and restores hunger", id, definition.learnsMount)
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
		{ItemIce, 34, world.Ice},
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

func TestSilverKeepsItsReservedIDWithoutARegistryRow(t *testing.T) {
	t.Parallel()

	if ItemSilver != 35 {
		t.Fatalf("ItemSilver = %d, want reserved id 35", ItemSilver)
	}
	if _, registered := itemByID(ItemSilver); registered {
		t.Fatal("reserved silver id still has an inventory registry row")
	}
}

// The three blocks worldgen 6 builds a settlement out of, at the ids the registry
// appended them at and with the plain block-item row each of them has.
//
// **The sibling of TestTheThreeGroundBlocksOfWorldgenThreeCarryTheirPinnedIDs, and it
// did not exist.** items.go says of these three that their numbers are "pinned by a
// test"; nothing pinned them, so the prose was a claim about the suite that the suite
// did not keep — and the numbers moved once already, when silver landed at 35 and
// pushed all three of these up by one. An id that can move unobserved is an inventory
// that can be reinterpreted after a server upgrade.
func TestTheThreeSettlementBlocksCarryTheirPinnedIDs(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		item   ItemID
		id     ItemID
		places world.Block
	}{
		{ItemPlanks, 36, world.Planks},
		{ItemCobblestone, 37, world.Cobblestone},
		{ItemThatch, 38, world.Thatch},
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
		// before a place request can put one of these back in the ground, which is the
		// whole of "a player can take a settlement apart and build with it".
		block, placeable := blockPlacedBy(tc.item)
		if !placeable || block != tc.places {
			t.Errorf("item %d places block %d (placeable %v), want %d", tc.item, block, placeable, tc.places)
		}
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
		world.Ice:       ItemIce,

		// The three a settlement is built from: each gives back the block that was
		// broken, which is what makes a hut something a player can carry away.
		world.Planks:      ItemPlanks,
		world.Cobblestone: ItemCobblestone,
		world.Thatch:      ItemThatch,
		world.PalmLog:     ItemPalmLog,
		world.PalmFronds:  ItemNone,
		world.DesertShrub: ItemNone,
		world.BroadLeaves: ItemNone,
		world.Bush:        ItemNone,

		// The three flowers: breakable and yielding nothing, recorded the way the
		// decision for leaves is rather than left to the fail-closed default.
		world.FlowerRed:    ItemNone,
		world.FlowerYellow: ItemNone,
		world.FlowerBlue:   ItemNone,

		// The castle materials and shaped slate variants: every row is ItemNone on
		// purpose. None is world.Placeable, so a drop would
		// be an item nothing could put back.
		world.SmoothBlackStone:      ItemNone,
		world.Basalt:                ItemNone,
		world.BlackBrick:            ItemNone,
		world.BlackBrickWorn:        ItemNone,
		world.SlateTile:             ItemNone,
		world.SlateSlabBottom:       ItemNone,
		world.SlateSlabTop:          ItemNone,
		world.SlateStairNorthBottom: ItemNone,
		world.SlateStairEastBottom:  ItemNone,
		world.SlateStairSouthBottom: ItemNone,
		world.SlateStairWestBottom:  ItemNone,
		world.SlateStairNorthTop:    ItemNone,
		world.SlateStairEastTop:     ItemNone,
		world.SlateStairSouthTop:    ItemNone,
		world.SlateStairWestTop:     ItemNone,
		world.DarkTimber:            ItemNone,
		world.PaleTimber:            ItemNone,
		world.DarkGlass:             ItemNone,
	}
	// **The same length guard TestBlockExperienceNamesEveryRewardAndExplicitZero has,
	// and it was missing here.** Without it this loop only checks the rows somebody
	// remembered to copy across, so a block appended to blockDrops and forgotten here
	// was never checked at all — which is exactly what happened to all three of the
	// settlement blocks until this line was written.
	if len(blockDrops) != len(want) {
		t.Fatalf("the drop table has %d rows and this test names %d", len(blockDrops), len(want))
	}
	for block, itemID := range want {
		if got := itemDroppedBy(block); got != itemID {
			t.Errorf("block %d drops item %d, want %d", block, got, itemID)
		}
	}
	if got := itemDroppedBy(world.Air); got != ItemNone {
		t.Errorf("Air drops item %d, want nothing", got)
	}
	// **The water family has no yield, and unlike Leaves it has no rows at all.**
	// Leaves are breakable and drop nothing, which is a decision the table has to
	// record; water is not breakable, so absent rows are the right shape and the
	// fail-closed default is the right answer.
	for _, block := range allWaterBlocks {
		if got := itemDroppedBy(block); got != ItemNone {
			t.Errorf("water block %d drops item %d, want nothing", block, got)
		}
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
		world.Ice:       0,

		// The three a settlement is built from. Salvage is not a lesson, so all three
		// are explicit zeroes for the reason sand and gravel are.
		world.Planks:      0,
		world.Cobblestone: 0,
		world.Thatch:      0,
		world.PalmLog:     2,
		world.PalmFronds:  0,
		world.DesertShrub: 0,
		world.BroadLeaves: 0,
		world.Bush:        0,

		// Three more explicit zeroes: picking a flower teaches nothing yet.
		world.FlowerRed:    0,
		world.FlowerYellow: 0,
		world.FlowerBlue:   0,

		// Castle blocks: chipping one apart is salvage that yields no salvage.
		world.SmoothBlackStone:      0,
		world.Basalt:                0,
		world.BlackBrick:            0,
		world.BlackBrickWorn:        0,
		world.SlateTile:             0,
		world.SlateSlabBottom:       0,
		world.SlateSlabTop:          0,
		world.SlateStairNorthBottom: 0,
		world.SlateStairEastBottom:  0,
		world.SlateStairSouthBottom: 0,
		world.SlateStairWestBottom:  0,
		world.SlateStairNorthTop:    0,
		world.SlateStairEastTop:     0,
		world.SlateStairSouthTop:    0,
		world.SlateStairWestTop:     0,
		world.DarkTimber:            0,
		world.PaleTimber:            0,
		world.DarkGlass:             0,
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
