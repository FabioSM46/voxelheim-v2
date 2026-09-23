package world

// The first dungeon below the chasm: the cave, the sand hall and the two stairs that
// join them to each other and to the king's arena. The upper halls and the pool are
// in schematic_instance.go; everything here is in the same unrotated frame.
//
// The route, from the shore: the tunnel north into the cave's cavern, through the web
// curtain in the neck into the gallery, west along it to the grille (puzzle 2), past
// the second checkpoint and down eight steps into the sand hall; across the hall to
// its door (puzzle 3), past the third checkpoint and down nine steps south into the
// king's arena, which lies under the cave.
//
// **Every index below is a slot another issue reads**, so they are stated here once:
//
//   - Checkpoints: 0 the shore, 1 past the grille, 2 past the sand hall's door.
//   - Minor-spawn groups: 0–3 floor 1's halls, [CaveBurrowGroup] the burrows the
//     spider waves come out of, [SandBuriedGroup] the scorpions under the sand.
//   - Puzzles: 1 the rune hall, [GrillePuzzle] the timed grille, [TwinLeverPuzzle]
//     the sand hall's two levers.
//   - Trigger zones: [CaveTrigger] the cavern, [SandTrigger] the sand hall.
const (
	// CaveBurrowGroup is the minor-spawn group of the cave's burrows. A burrow is a
	// hole in the cavern's wall, one wide and two tall; the slot stands at its back.
	CaveBurrowGroup = 4
	// SandBuriedGroup is the minor-spawn group of the sand hall's scorpions. Each slot
	// stands on sand with sand under every cell a scorpion's body covers, so a body
	// sunk a block below the slot is wholly buried.
	SandBuriedGroup = 5

	// GrillePuzzle is the cave's lever and the grille it holds open for a while.
	GrillePuzzle = 2
	// TwinLeverPuzzle is the sand hall's two levers and the door both open together.
	TwinLeverPuzzle = 3

	// CaveTrigger is the cavern's trigger volume, spanning it from wall to wall.
	CaveTrigger = 0
	// SandTrigger is the sand hall's trigger volume, spanning its whole floor.
	SandTrigger = 1
)

const (
	// The rock the lower zones are cut into: everything the drawing has not drawn in
	// this box, north of the pool chamber and up to the course over the cave's tallest
	// chamber.
	caveRockTop, caveRockZ1 = dungeonShore + 6, 86

	// The cave's rooms, standing on the shore's level.
	caveX0, caveX1                 = 4, 30
	caveCavernZ0, caveCavernZ1     = 60, 82
	caveGalleryZ0, caveGalleryZ1   = 50, 55
	caveNeckX0, caveNeckX1         = 15, 19
	caveNeckZ0, caveNeckZ1         = 56, 59
	caveCurtainZ                   = 57
	caveExitX0, caveExitX1         = 4, 6
	caveExitZ0, caveExitZ1         = 46, 48
	caveGrilleZ                    = 49
	caveLeverX, caveLeverZ         = caveX1 + 1, 61
	caveTallX0, caveTallZ0         = 10, 66
	caveTallX1, caveTallZ1         = 24, 76
	sandX0, sandZ0, sandX1, sandZ1 = 1, 4, 33, 37
	sandTall                       = 8
	sandDoorX0, sandDoorX1         = 28, 30
	sandDoorZ                      = sandZ1 + 1
	sandLeverZ                     = 20
	kingStairTopZ, kingStairSteps  = 41, dungeonSandFloor - dungeonKingFloor
)

// caveBurrows are the burrows' slots: two in each side wall of the cavern and two in
// its south wall, either side of the tunnel from the shore.
var caveBurrows = [...][2]int{{2, 66}, {2, 76}, {32, 64}, {32, 78}, {8, 84}, {26, 84}}

// sandDunes are the sand hall's dunes, one course of sand over its floor, and
// sandCrests the second course on the four large ones. They leave three lanes flat:
// across the hall between its levers, along its south wall from the stair to the door,
// and down its middle.
var (
	sandDunes  = [...]Box{{2, 10, 6, 10, 10, 12}, {22, 10, 5, 31, 10, 11}, {2, 10, 26, 9, 10, 34}, {21, 10, 25, 31, 10, 33}, {12, 10, 11, 20, 10, 15}}
	sandCrests = [...]Box{{3, 11, 8, 8, 11, 10}, {24, 11, 7, 29, 11, 9}, {3, 11, 28, 7, 11, 32}, {23, 11, 27, 28, 11, 31}}
	// sandBuried are the scorpions' slots: one on each dune's crest or top, and three
	// on the flat floor — two of them in the lane the twin-lever runner crosses.
	sandBuried = [...][3]int{{5, 12, 9}, {26, 12, 8}, {5, 12, 30}, {25, 12, 29}, {16, 11, 13}, {9, 10, 20}, {24, 10, 21}, {17, 10, 33}}
	// sandPillars are the hall's four sandstone pillars, two by two, floor to ceiling.
	sandPillars = [...][2]int{{11, 15}, {22, 15}, {11, 28}, {22, 28}}
)

// lowerHash is the drawing's fixed noise: the same rock, drips and webs in every
// instance, since a drawing is data and the seed only turns it.
func lowerHash(x, y, z int) uint64 {
	return HashLattice(0x0CA7E5A4D+int64(y), int64(x), int64(z))
}

// roughRock is the cave's rock: mostly stone, some basalt and a little gravel.
func roughRock(x, y, z int) Block {
	switch h := lowerHash(x, y, z) % 10; {
	case h < 6:
		return Stone
	case h < 9:
		return Basalt
	default:
		return Gravel
	}
}

// drawSandHall is the sand hall in sandstone: a sand floor with dunes on it, and four
// pillars. It is drawn before the rock around it so its walls stay sandstone.
func drawSandHall(s *Section) {
	s.CarveRoom(Box{sandX0, dungeonSandFloor, sandZ0, sandX1, dungeonSandFloor + sandTall - 1, sandZ1}, Sandstone)
	s.FillFloor(sandX0, sandZ0, sandX1, sandZ1, dungeonSandFloor-1, Sand)
	for _, b := range sandDunes {
		s.Fill(b, Sand)
	}
	for _, b := range sandCrests {
		s.Fill(b, Sand)
	}
	for _, p := range sandPillars {
		s.Fill(Box{p[0], dungeonSandFloor, p[1], p[0] + 1, dungeonSandFloor + sandTall - 1, p[1] + 1}, Sandstone)
	}
	// The twin levers, one in each side wall, at the height of a hand.
	for _, x := range [...]int{sandX0 - 1, sandX1 + 1} {
		s.Fill(Box{x, dungeonSandFloor + 1, sandLeverZ, x, dungeonSandFloor + 1, sandLeverZ}, LeverOff)
	}
}

// carveCave cuts the cave into the rock on the shore's level: the tunnel from the
// shore, the cavern with a taller chamber in its middle, the neck with its web
// curtain, the gallery and the passage to the grille.
func carveCave(s *Section) {
	const y = dungeonShore
	s.CarveRoom(Box{16, y, caveCavernZ1 + 1, 18, y + 2, 87}, Stone) // the tunnel from the shore
	s.CarveRoom(Box{caveX0, y, caveCavernZ0, caveX1, y + 4, caveCavernZ1}, Stone)
	s.CarveRoom(Box{caveTallX0, y, caveTallZ0, caveTallX1, y + 5, caveTallZ1}, Stone)
	s.CarveRoom(Box{caveNeckX0, y, caveNeckZ0, caveNeckX1, y + 2, caveNeckZ1}, Stone)
	s.CarveRoom(Box{caveX0, y, caveGalleryZ0, caveX1, y + 3, caveGalleryZ1}, Stone)
	s.CarveRoom(Box{caveExitX0, y, caveExitZ0, caveExitX1, y + 2, caveExitZ1}, Stone)
	for _, b := range caveBurrows {
		// Two deep into the wall, from the room's edge to the slot.
		x0, x1, z0, z1 := b[0], b[0], b[1], b[1]
		switch {
		case b[0] < caveX0:
			x1 = caveX0 - 1
		case b[0] > caveX1:
			x0 = caveX1 + 1
		default:
			z0 = caveCavernZ1 + 1
		}
		s.CarveRoom(Box{x0, y, z0, x1, y + 1, z1}, Stone)
	}
	// Two rock columns in the cavern, either side of its middle.
	s.Fill(Box{8, y, 70, 9, y + 4, 71}, Basalt)
	s.Fill(Box{25, y, 71, 26, y + 4, 72}, Basalt)

	// The ceilings drip: over a column of the cavern (its taller chamber included) or
	// the gallery, the top course is rock one time in three and the course under it one
	// time in seven where that still leaves three clear courses — the least the whole
	// cave has, so nothing that walks is ever stooped by a drip.
	rooms := [...]Box{
		{caveX0, y, caveCavernZ0, caveX1, y + 4, caveCavernZ1},
		{caveX0, y, caveGalleryZ0, caveX1, y + 3, caveGalleryZ1},
	}
	for _, room := range rooms {
		s.each(Box{room.X0, 0, room.Z0, room.X1, 0, room.Z1}, func(x, _, z int) {
			if s.at(x, y, z) != Air {
				return // a column of rock
			}
			top := caveTop(s, x, z)
			h := lowerHash(x, top, z)
			if h%3 == 0 {
				s.Fill(Box{x, top, z, x, top, z}, roughRock(x, top, z))
				if h%7 == 0 && top-1 >= y+3 {
					s.Fill(Box{x, top - 1, z, x, top - 1, z}, roughRock(x, top-1, z))
				}
			}
		})
	}
	// Webs under the ceiling: in every corner, and along the walls one time in three.
	// They hang in the top clear course, which is never lower than the third, so a
	// web is overhead and never in the way.
	for _, room := range rooms {
		s.each(Box{room.X0, 0, room.Z0, room.X1, 0, room.Z1}, func(x, _, z int) {
			if s.at(x, y, z) != Air {
				return // a column of rock
			}
			top := caveTop(s, x, z)
			walls := 0
			for _, d := range [...][2]int{{1, 0}, {-1, 0}, {0, 1}, {0, -1}} {
				if Solid(s.at(x+d[0], top, z+d[1])) {
					walls++
				}
			}
			if walls >= 2 || (walls == 1 && lowerHash(z, top, x)%3 == 0) {
				s.Fill(Box{x, top, z, x, top, z}, Cobweb)
			}
		})
	}
	// The curtain: the neck webbed shut, floor to ceiling.
	s.Fill(Box{caveNeckX0, y, caveCurtainZ, caveNeckX1, y + 2, caveCurtainZ}, Cobweb)
	// Puzzle 2: the lever in the cavern's east wall and, at the other end of the cave,
	// the grille across the passage out of the gallery.
	s.Fill(Box{caveLeverX, y + 1, caveLeverZ, caveLeverX, y + 1, caveLeverZ}, LeverOff)
	s.Fill(Box{caveExitX0, y, caveGrilleZ, caveExitX1, y + 2, caveGrilleZ}, IronGrilleX)
}

// caveTop is the highest clear course of a cave column: the drawing is never taller
// than the cave's tallest chamber there, so the search starts at its ceiling.
func caveTop(s *Section, x, z int) int {
	top := dungeonShore
	for yy := dungeonShore; yy <= dungeonShore+5 && s.at(x, yy, z) == Air; yy++ {
		top = yy
	}
	return top
}

// carveDescents cuts the two stairs down: from the grille's passage into the sand
// hall, and from the sand hall's door into the king's arena, with a landing past the
// door.
func carveDescents(s *Section) {
	// Eight steps from the sand floor up to the passage, climbing south.
	s.PlaceStairs(caveExitX1, dungeonSandFloor, sandZ1+1, FacingPlusZ, dungeonShore-dungeonSandFloor, 3, Stone)
	// Puzzle 3's door in the hall's south wall, and the landing beyond it.
	s.Fill(Box{sandDoorX0, dungeonSandFloor, sandDoorZ, sandDoorX1, dungeonSandFloor + 2, sandDoorZ}, IronGrilleX)
	s.CarveRoom(Box{sandDoorX0, dungeonSandFloor, sandDoorZ + 1, sandDoorX1, dungeonSandFloor + 2, kingStairTopZ - 1}, Stone)
	// Nine steps from the king's floor up to the landing, climbing north, and the
	// passage from their foot through the arena's north wall.
	foot := kingStairTopZ + kingStairSteps - 1
	s.PlaceStairs(sandDoorX0, dungeonKingFloor, foot, FacingMinusZ, kingStairSteps, 3, Stone)
	s.CarveRoom(Box{sandDoorX0, dungeonKingFloor, foot + 1, sandDoorX1, dungeonKingFloor + 3, kingZ0 - 1}, Stone)
}

// lowerAnchors declares the slots below the chasm, after every slot of the upper
// halls so none of theirs moves in the list.
func lowerAnchors(s *Section) {
	const y = dungeonShore
	s.Anchor(AnchorInstanceCheckpoint, caveExitX0+1, y, caveExitZ0+1, 1)
	s.Anchor(AnchorInstanceCheckpoint, sandDoorX0+1, dungeonSandFloor, sandDoorZ+1, 2)
	for _, b := range caveBurrows {
		s.Anchor(AnchorInstanceMinorSpawn, b[0], y, b[1], CaveBurrowGroup)
	}
	for _, b := range sandBuried {
		s.Anchor(AnchorInstanceMinorSpawn, b[0], b[1], b[2], SandBuriedGroup)
	}
	s.Anchor(AnchorInstanceMechanism, caveLeverX, y+1, caveLeverZ, GrillePuzzle)
	for _, x := range [...]int{sandX0 - 1, sandX1 + 1} {
		s.Anchor(AnchorInstanceMechanism, x, dungeonSandFloor+1, sandLeverZ, TwinLeverPuzzle)
	}
	for yy := y; yy <= y+2; yy++ {
		for x := caveExitX0; x <= caveExitX1; x++ {
			s.Anchor(AnchorInstanceDoor, x, yy, caveGrilleZ, GrillePuzzle)
		}
	}
	for yy := dungeonSandFloor; yy <= dungeonSandFloor+2; yy++ {
		for x := sandDoorX0; x <= sandDoorX1; x++ {
			s.Anchor(AnchorInstanceDoor, x, yy, sandDoorZ, TwinLeverPuzzle)
		}
	}
	s.Anchor(AnchorInstanceTrigger, caveX0, y, caveCavernZ0, CaveTrigger)
	s.Anchor(AnchorInstanceTrigger, caveX1, y+1, caveCavernZ1, CaveTrigger)
	s.Anchor(AnchorInstanceTrigger, sandX0, dungeonSandFloor, sandZ0, SandTrigger)
	s.Anchor(AnchorInstanceTrigger, sandX1, dungeonSandFloor+sandTall-1, sandZ1, SandTrigger)
}
