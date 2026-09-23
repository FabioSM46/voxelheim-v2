package world

// The first dungeon's drawing, built with the section helpers rather than drawn as
// layers (see section.go for why).
//
// **Floor 1, the upper halls, stands high above everything else, and the only way
// down is to fall.** A party arrives in the arrival court, where the return portal
// also stands — leaving never requires a kill — and walks south through two halls
// and the rune hall. The rune hall's four stones open the door into the Vargr
// guardian's arena (puzzle 1). When the guardian dies, a square of the arena floor
// opens onto the chasm: a smooth shaft with nothing to stand on, ending in a still
// pool deep enough that the landing costs nothing. The shore is the first
// checkpoint, and a tunnel leads on from it.
//
// **Below floor 1 the dungeon is cut into rock, and it goes down twice more.** The
// tunnel from the shore opens into the cave: rough stone, low and uneven ceilings,
// webs in every corner and a web curtain across the neck into its gallery, with the
// spiders' burrows in its walls (schematic_instance_lower.go draws it). A lever in
// the cavern opens the grille at the far end of the gallery for twelve seconds
// (puzzle 2); past it is the second checkpoint and a stair down into the sand hall,
// where the scorpions lie under the dunes. Two levers at the hall's opposite walls
// open its door together (puzzle 3); past it is the third checkpoint and a stair
// down to the Draugr king's arena at the bottom of the dungeon, under the cave.
//
// Both arenas keep the dimensions the boss moves, escape routes and balance were
// measured in (#1073, #1099): the guardian's is 27×27 clear with eight clear courses,
// the king's 31×31, and each holds four one-block monoliths four courses tall, eight
// blocks from its centre on both axes.
//
// Every coordinate is the unrotated drawing's own; the placement turns the whole
// drawing and its anchors by the seed and puts [dungeonUpperFloor] on world y = 1.
// The drawing is 35×60×106, which is two chunks by three by four at every turn: the
// instance's envelope is 120 chunks with its halo, the number the cache is sized to.
const (
	dungeonWidth, dungeonHeight, dungeonDepth = 35, 60, 106

	// dungeonUpperFloor is floor 1's standing level; its floor course is one below.
	dungeonUpperFloor = 51

	// The standing levels below floor 1, deepest first. The king's floor course is the
	// drawing's bottom course.
	dungeonKingFloor = 1
	dungeonSandFloor = 10

	// The pool: water from the chamber's bed up to dungeonWaterTop, four courses deep,
	// and the shore beside it standing level with the surface. The cave is on the
	// shore's level.
	dungeonPoolBed  = 13
	dungeonWaterTop = dungeonPoolBed + 4
	dungeonShore    = dungeonWaterTop + 1

	// The chasm's clear column, inclusive. Its top course is the trapdoor the
	// guardian's defeat opens; its bottom breaks through the pool chamber's ceiling.
	chasmX0, chasmZ0, chasmX1, chasmZ1 = 15, 95, 19, 99
	chasmBottom, chasmTop              = dungeonWaterTop + 4, dungeonUpperFloor - 1

	// The guardian arena's clear interior and centre.
	guardianX0, guardianZ0, guardianX1, guardianZ1 = 4, 74, 30, 100
	guardianCX, guardianCZ                         = 17, 87

	// The king arena's clear interior and centre.
	kingX0, kingZ0, kingX1, kingZ1 = 2, 52, 32, 82
	kingCX, kingCZ                 = 17, 67

	// Floor 1's halls share one width and height.
	upperHallX0, upperHallX1, upperHallTall = 7, 27, 7

	// The court's two slots, six blocks apart on the dungeon's axis.
	arrivalZ, exitPortalZ = 3, 9

	// The rune hall: four stones in a row, and the wall they face holds both the door
	// they open and, behind each stone, the inscription saying when to press it.
	runeStoneZ, inscriptionZ           = 69, 73
	runeDoorX0, runeDoorX1, runeDoorUp = 15, 19, 5
	runePuzzle                         = 1
)

// runeStoneXs are the four stones, west to east. Their anchors are declared in this
// order, and [InstanceRuneOrder] names stones by their position in it.
var runeStoneXs = [4]int{10, 13, 21, 24}

// Minor-spawn groups on floor 1, four slots each; how many are filled for a party is
// the balance issue's number, not this drawing's. Groups 0 and 1 stand in the first
// hall (the draugr), 2 and 3 in the second (the vargr).
var (
	upperSpawnGroups = [...]struct{ z, group int }{{22, 0}, {28, 1}, {42, 2}, {48, 3}}
	upperSpawnXs     = [4]int{13, 15, 19, 21}
)

var instanceDungeon = buildDungeon()

func buildDungeon() *Schematic {
	const f = dungeonUpperFloor
	s := NewSection(dungeonWidth, dungeonHeight, dungeonDepth)

	// Floor 1: the arrival court, two halls and the rune hall, joined by corridors
	// three wide and four tall — room for the mounted body as well as the walking one.
	s.CarveRoom(Box{10, f, 1, 24, f + 5, 11}, BlackBrick)
	for _, z := range [...][2]int{{15, 31}, {35, 51}, {55, 72}} {
		s.CarveRoom(Box{upperHallX0, f, z[0], upperHallX1, f + upperHallTall - 1, z[1]}, BlackBrick)
	}
	for _, z := range [...]int{12, 32, 52} {
		s.CarveRoom(Box{16, f, z, 18, f + 3, z + 2}, BlackBrick)
	}
	s.FillFloor(9, 0, 25, 12, f-1, Basalt)
	s.FillFloor(15, 13, 19, 13, f-1, Basalt) // the first corridor's middle course
	s.FillFloor(upperHallX0-1, 14, upperHallX1+1, inscriptionZ, f-1, Basalt)

	// The return portal in the court, ahead of the arrival slot: a freestanding rune
	// frame whose veil is the walk-through threshold.
	s.Fill(Box{14, f - 1, exitPortalZ, 20, f + 3, exitPortalZ}, RuneStone)
	s.Fill(Box{15, f, exitPortalZ, 19, f + 1, exitPortalZ}, PortalVeil)
	s.Fill(Box{16, f + 2, exitPortalZ, 18, f + 2, exitPortalZ}, PortalVeil)
	s.Fill(Box{17, f, exitPortalZ, 17, f, exitPortalZ}, PortalHeart)

	// Worn pillars in both halls, clear of the route down the middle, and webs in the
	// corners of their ceilings.
	for _, z := range [...]int{19, 27, 39, 47} {
		for _, x := range [...]int{11, 23} {
			s.Fill(Box{x, f, z, x, f + upperHallTall - 1, z}, BlackBrickWorn)
		}
	}
	for _, z := range [...]int{15, 31, 35, 51} {
		for _, x := range [...]int{upperHallX0, upperHallX1} {
			s.Fill(Box{x, f + upperHallTall - 1, z, x, f + upperHallTall - 1, z}, Cobweb)
		}
	}

	// The guardian arena, the chamber the fight was measured in.
	s.CarveRoom(Box{guardianX0, f, guardianZ0, guardianX1, f + 7, guardianZ1}, BlackBrick)
	s.FillFloor(guardianX0-1, guardianZ0-1, guardianX1+1, guardianZ1+1, f-1, Basalt)
	s.FillFloor(guardianX0-1, guardianZ0-1, guardianX1+1, guardianZ1+1, f+8, Basalt)
	monoliths(s, guardianCX, guardianCZ, f)

	// The rune stones and the door they open. The inscription's marks depend on the
	// seed and are laid over the blank wall by the layout's overlay.
	for _, x := range runeStoneXs {
		s.Fill(Box{x, f, runeStoneZ, x, f, runeStoneZ}, RuneStone)
	}
	s.Fill(Box{runeDoorX0, f, inscriptionZ, runeDoorX1, f + runeDoorUp - 1, inscriptionZ}, IronGrilleX)

	// The pool chamber: still water four deep, and a shore at its north end whose
	// floor is flush with the surface, so a swimmer steps out rather than climbs.
	s.CarveRoom(Box{11, dungeonPoolBed + 1, 88, 23, dungeonPoolBed + 7, 104}, Basalt)
	s.Fill(Box{11, dungeonPoolBed + 1, 93, 23, dungeonWaterTop, 104}, Water)
	s.Fill(Box{11, dungeonPoolBed + 1, 88, 23, dungeonWaterTop, 92}, Basalt)

	// The chasm: from the arena floor straight down through the pool chamber's
	// ceiling, shelled in brick wherever nothing else stands. Carved after the arena,
	// so its top course cuts the trapdoor's opening through the arena floor.
	s.CarveShaft(chasmX0, chasmZ0, chasmX1, chasmZ1, chasmBottom, chasmTop, BlackBrick)

	// The king's arena at the bottom, in the brick the guardian's is built of, and the
	// sand hall above and north of it. Both are drawn before the rock they are set in,
	// so they keep their own walls; the cave is carved after it and has the rock's.
	s.CarveRoom(Box{kingX0, dungeonKingFloor, kingZ0, kingX1, dungeonKingFloor + 7, kingZ1}, BlackBrick)
	s.FillFloor(kingX0-1, kingZ0-1, kingX1+1, kingZ1+1, dungeonKingFloor-1, Basalt)
	s.FillFloor(kingX0-1, kingZ0-1, kingX1+1, kingZ1+1, dungeonKingFloor+8, Basalt)
	monoliths(s, kingCX, kingCZ, dungeonKingFloor)
	drawSandHall(s)
	s.FillVoid(Box{0, 0, 0, dungeonWidth - 1, caveRockTop, caveRockZ1}, roughRock)
	carveCave(s)
	carveDescents(s)
	carveReturnShortcut(s)

	s.Anchor(AnchorInstanceArrival, 17, f, arrivalZ, 0)
	s.Anchor(AnchorInstanceExit, 17, f, exitPortalZ, 0)
	for _, g := range upperSpawnGroups {
		for _, x := range upperSpawnXs {
			s.Anchor(AnchorInstanceMinorSpawn, x, f, g.z, g.group)
		}
	}
	for _, x := range runeStoneXs {
		s.Anchor(AnchorInstanceMechanism, x, f, runeStoneZ, runePuzzle)
	}
	for y := f; y < f+runeDoorUp; y++ {
		for x := runeDoorX0; x <= runeDoorX1; x++ {
			s.Anchor(AnchorInstanceDoor, x, y, inscriptionZ, runePuzzle)
		}
	}
	s.Anchor(AnchorInstanceGuardian, guardianCX, f, guardianCZ, 0)
	s.Anchor(AnchorInstanceGate, (chasmX0+chasmX1)/2, chasmTop, (chasmZ0+chasmZ1)/2, 0)
	s.Anchor(AnchorInstanceCheckpoint, 17, dungeonShore, 90, 0)
	s.Anchor(AnchorInstanceKing, kingCX, dungeonKingFloor, kingCZ, 0)
	lowerAnchors(s)
	return s.MustBuild()
}

// monoliths stands an arena's four pillars eight blocks from its centre on both axes.
func monoliths(s *Section, cx, cz, floor int) {
	for _, dz := range [...]int{-8, 8} {
		for _, dx := range [...]int{-8, 8} {
			s.Fill(Box{cx + dx, floor, cz + dz, cx + dx, floor + 3, cz + dz}, BlackBrickWorn)
		}
	}
}

// InstanceRuneOrder is the order the rune hall's four stones must be pressed in for
// this seed: order[k] is the stone pressed k-th, numbering the stones by their
// mechanism anchors' order, which is west to east in the unrotated drawing.
//
// The inscription says the same thing in blocks: stone order[k] has k+1 lit runes
// above its place in the wall. The permutation is mixed from the whole seed rather
// than from the two bits that choose the rotation, so a dungeon's turn says nothing
// about its puzzle.
func InstanceRuneOrder(seed int64) [4]int {
	h := uint64(seed) + 0x9E3779B97F4A7C15
	h = (h ^ h>>30) * 0xBF58476D1CE4E5B9
	h = (h ^ h>>27) * 0x94D049BB133111EB
	h ^= h >> 31
	pick := h % 24 // one of the 4! orders, read as a factorial-base number
	left := []int{0, 1, 2, 3}
	var order [4]int
	for k, radix := range [4]uint64{6, 2, 1, 1} {
		i := pick / radix
		pick %= radix
		order[k] = left[i]
		left = append(left[:i], left[i+1:]...)
	}
	return order
}

// runeInscription is the seed's marks: above each stone's place in the inscription
// wall, one lit rune per step of the order, stacked from the course above the floor.
func runeInscription(seed int64) []drawnCell {
	var marks []drawnCell
	for k, stone := range InstanceRuneOrder(seed) {
		for n := 0; n <= k; n++ {
			marks = append(marks, drawnCell{runeStoneXs[stone], dungeonUpperFloor + 1 + n, inscriptionZ, RuneStoneLit})
		}
	}
	return marks
}

// dungeonEditable is the dungeon's edit rule: air and webs, but never the chasm's
// column from the pool's surface to the trapdoor. A block placed there would be a
// ledge in a shaft that must have none, and a way back up a drop that is one-way.
func dungeonEditable(lx, ly, lz int) bool {
	if lx >= chasmX0 && lx <= chasmX1 && lz >= chasmZ0 && lz <= chasmZ1 && ly > dungeonWaterTop {
		return false
	}
	return instanceEditableCell(instanceDungeon.At(lx, ly, lz))
}
