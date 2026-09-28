package world

// Chest indices are stable within a dungeon run and are persisted by the server.
// Never reorder them: the opened list names these slots, not discovery order.
const (
	AntechamberChest = iota
	SandHallChest
	KingChest
	InstanceChestCount
)

// placeDungeonChests runs after room carving, away from spawns, pillars and doors.
// The first chest stands on the guardian arena's safe floor beside the chasm;
// no separate antechamber is carved. Each later chest occupies a flat edge lane.
func placeDungeonChests(s *Section) {
	for index, pos := range [...][3]int{
		{12, dungeonUpperFloor, 97},
		{17, dungeonSandFloor, 35},
		{20, dungeonKingFloor, 79},
	} {
		x, y, z := pos[0], pos[1], pos[2]
		s.Fill(Box{x, y, z, x, y, z}, Chest).
			Anchor(AnchorInstanceChest, x, y, z, index)
	}
}
