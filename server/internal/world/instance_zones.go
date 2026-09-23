package world

// The first dungeon's leash zones: the box each minor-spawn group hunts inside.
//
// A creature the dungeon places belongs to the room its slot stands in and follows
// nobody out of it (see game/dungeon_minor.go for what the leash does). The rooms are
// the drawing's, so the boxes are declared here beside it, in the unrotated frame, and
// turned by the same placement that turns the walls and the anchors.
//
// **Every zone ends at a checkpoint and at a door.** A zone is the room's own clear
// interior — plus, for the cave, the burrows cut into its walls — and never the
// passage or stair beyond a door or a checkpoint. A party that walks back to a
// checkpoint or out through a door has therefore left every zone, which is what makes
// a checkpoint somewhere to stand and breathe. TestEveryDungeonZoneEndsAtEachCheckpointAndDoor
// pins it at every rotation.

// InstanceZone is one minor-spawn group's leash zone, in world block coordinates:
// every cell from Min to Max inclusive on all three axes.
type InstanceZone struct {
	Group    int
	Min, Max [3]int64
}

// dungeonZones are the zones in the unrotated drawing, one per minor-spawn group and
// in group order. Groups 0 and 1 share the first hall, 2 and 3 the second.
var dungeonZones = func() [SandBuriedGroup + 1]Box {
	const f = dungeonUpperFloor
	firstHall := Box{upperHallX0, f, 15, upperHallX1, f + upperHallTall - 1, 31}
	secondHall := Box{upperHallX0, f, 35, upperHallX1, f + upperHallTall - 1, 51}
	return [...]Box{
		0: firstHall,
		1: firstHall,
		2: secondHall,
		3: secondHall,
		// The cave: the cavern, the neck, the gallery and the burrows two deep in the
		// cavern's walls, from the grille (a door) to the burrows either side of the
		// tunnel mouth — short of the tunnel's run to the shore's checkpoint. Its
		// ceiling is the rock's top, over the tallest chamber.
		CaveBurrowGroup: {caveX0 - 2, dungeonShore, caveGalleryZ0, caveX1 + 2, caveRockTop - 1, caveCavernZ1 + 2},
		// The sand hall's floor, wall to wall, short of its door and of the stair up
		// to the grille's passage.
		SandBuriedGroup: {sandX0, dungeonSandFloor, sandZ0, sandX1, dungeonSandFloor + sandTall - 1, sandZ1},
	}
}()

// InstanceDungeonZones is every minor-spawn group's zone in the dungeon generated
// from seed, in group order: element g is group g's. The slice is fresh on every
// call.
func InstanceDungeonZones(seed int64) []InstanceZone {
	b := instancePlacement(seed)
	d := instanceDungeon
	zones := make([]InstanceZone, 0, len(dungeonZones))
	for group, box := range dungeonZones {
		ax, az := rotateCell(box.X0, box.Z0, d.W, d.D, b.Facing)
		bx, bz := rotateCell(box.X1, box.Z1, d.W, d.D, b.Facing)
		zones = append(zones, InstanceZone{
			Group: group,
			Min:   [3]int64{b.OriginX + int64(min(ax, bx)), b.OriginY + int64(box.Y0), b.OriginZ + int64(min(az, bz))},
			Max:   [3]int64{b.OriginX + int64(max(ax, bx)), b.OriginY + int64(box.Y1), b.OriginZ + int64(max(az, bz))},
		})
	}
	return zones
}

// Contains reports whether the cell at x, y, z lies inside the zone.
func (z InstanceZone) Contains(x, y, zz int64) bool {
	return x >= z.Min[0] && x <= z.Max[0] && y >= z.Min[1] && y <= z.Max[1] && zz >= z.Min[2] && zz <= z.Max[2]
}
