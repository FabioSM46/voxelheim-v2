package game

import "github.com/FabioSM46/voxelheim-v2/server/internal/world"

// lootTable is shared by species and dungeon chests. Each row rolls an inclusive
// count; oneOf, when present, chooses exactly one item with equal probability.
// Species keep their existing rows and RNG order; a choice consumes an extra draw
// only for a nonzero item count. Silver never carries an item choice.
type lootTable []lootRoll

type lootRoll struct {
	item     ItemID
	oneOf    []ItemID
	silver   bool
	min, max uint16
}

// chestLootTables pay each present member once. Silver increases with descent depth
// (8–15, 12–20, 20–30), while the item rows support the next fight rather than replace
// the king's unique reward. The first refills 8–12 arrows; the second grants one of
// the two ordinary repair consumables; the last rolls one equally likely rusty
// armour piece. Choices are alternatives, never independent rolls that can pay both.
var chestLootTables = [world.InstanceChestCount]lootTable{
	world.AntechamberChest: {
		{silver: true, min: 8, max: 15},
		{item: ItemArrow, min: 8, max: 12},
	},
	world.SandHallChest: {
		{silver: true, min: 12, max: 20},
		{oneOf: []ItemID{ItemSharpeningStone, ItemLeatherPatch}, min: 1, max: 1},
	},
	world.KingChest: {
		{silver: true, min: 20, max: 30},
		{oneOf: []ItemID{ItemRustyHelm, ItemRustyCuirass, ItemRustyGreaves}, min: 1, max: 1},
	},
}
