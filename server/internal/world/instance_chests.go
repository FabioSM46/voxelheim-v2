package world

// Chest indices are stable within a dungeon run and are persisted by the server.
// Never reorder them: the opened list names these slots, not discovery order.
const (
	AntechamberChest = iota
	SandHallChest
	KingChest
	InstanceChestCount
)
