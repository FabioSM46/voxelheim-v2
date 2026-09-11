package world

// PortalThreshold is the walk-through opening of one placed runic arch, in world block
// coordinates.
//
// **The opening is read from the drawing, never restated.** Cells are exactly the
// [PortalVeil] and [PortalHeart] voxels the placed schematic writes, turned with the
// walls by the same [rotateCell] the chunk compositor uses — so both ruin variants,
// the instance chamber and every quarter turn describe the doorway a player can
// actually see, and a later redrawing of an arch moves its trigger with it. The
// [RuneStone] jambs, shoulders and crown are deliberately absent: touching the frame
// is not touching the veil.
//
// Every portal in this repository is one block thick, so all its cells share one
// coordinate on the axis the sheet faces along. Normal names that axis (0 for X, 2 for
// Z); the sheet itself is the plane through the middle of that one-block course, which
// is where the client draws it.
type PortalThreshold struct {
	Heart  [3]int64
	Normal int
	Cells  [][3]int64
}

// Threshold is the entrance opening of this ruin's building. It reads only Building,
// so it composes no terrain, and its Heart is the ruin's Arch.
func (r Ruin) Threshold() PortalThreshold {
	return placedThreshold(RuinSchematicFor(r.Building.Variant), r.Building)
}

// InstanceExitThreshold is the return opening in the dungeon generated from seed,
// placed by the same quarter turn as [InstanceAnchors]; its Heart is that exit anchor.
func InstanceExitThreshold(seed int64) PortalThreshold {
	return placedThreshold(instanceChamber, instancePlacement(seed))
}

// placedThreshold collects one drawing's portal voxels at their placed coordinates.
//
// A drawing holds exactly one portal and one heart
// (TestEveryPortalHasOneHeartAndAnOpenProtectedDoorway), so every portal voxel belongs
// to that heart's arch. The normal is whichever horizontal axis the cells do not vary
// along; a quarter turn swaps X and Z, which is why it is read from the placed cells
// rather than from the drawing.
func placedThreshold(s *Schematic, b Building) PortalThreshold {
	t := PortalThreshold{Normal: 2}
	for y := range s.H {
		for z := range s.D {
			for x := range s.W {
				block := s.At(x, y, z)
				if !Portal(block) {
					continue
				}
				rx, rz := rotateCell(x, z, s.W, s.D, b.Facing)
				cell := [3]int64{b.OriginX + int64(rx), b.OriginY + int64(y), b.OriginZ + int64(rz)}
				if block == PortalHeart {
					t.Heart = cell
				}
				t.Cells = append(t.Cells, cell)
			}
		}
	}
	for _, cell := range t.Cells {
		if cell[2] != t.Heart[2] {
			t.Normal = 0
			break
		}
	}
	return t
}
