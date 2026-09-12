package world

import "testing"

func TestSchematicStairsTurnWithTheirHighHalf(t *testing.T) {
	t.Parallel()
	bottom := [4][4]Block{
		{SlateStairNorthBottom, SlateStairEastBottom, SlateStairSouthBottom, SlateStairWestBottom},
		{SlateStairEastBottom, SlateStairSouthBottom, SlateStairWestBottom, SlateStairNorthBottom},
		{SlateStairSouthBottom, SlateStairWestBottom, SlateStairNorthBottom, SlateStairEastBottom},
		{SlateStairWestBottom, SlateStairNorthBottom, SlateStairEastBottom, SlateStairSouthBottom},
	}
	for turn := range 4 {
		for direction := range 4 {
			for _, offset := range []Block{0, 4} {
				block := bottom[0][direction] + offset
				if got := rotateSchematicBlock(block, Facing(turn)); got != bottom[turn][direction]+offset {
					t.Errorf("block %d turn %d = %d, want %d", block, turn, got, bottom[turn][direction]+offset)
				}
			}
		}
		for _, block := range []Block{Air, BlackBrick, DarkTimber, SlateSlabBottom, SlateSlabTop} {
			if got := rotateSchematicBlock(block, Facing(turn)); got != block {
				t.Errorf("undirected block %d changed to %d", block, got)
			}
		}
	}
}

func TestPlacedCastleTurnsBothActualStairCellsAndBlocks(t *testing.T) {
	t.Parallel()
	s := SchematicFor(BuildingKeep)
	// A northward tread in the actual west flight, independently located in each
	// rotated footprint. Translating across chunk borders must not change its shape.
	cells := [4][2]int{{7, 33}, {29, 7}, {55, 29}, {33, 55}}
	blocks := [4]Block{SlateStairNorthBottom, SlateStairEastBottom, SlateStairSouthBottom, SlateStairWestBottom}
	if got := s.At(7, 3, 33); got != blocks[0] {
		t.Fatalf("authored west tread=%d", got)
	}
	for turn := range 4 {
		b := Building{Kind: BuildingKeep, OriginX: 11, OriginY: 5, OriginZ: 13, Facing: Facing(turn)}
		found := false
		visitSchematic(b, func(x, y, z int64, block Block) {
			if x == 11+int64(cells[turn][0]) && y == 8 && z == 13+int64(cells[turn][1]) {
				found = true
				if block != blocks[turn] {
					t.Errorf("turn %d placed tread=%d, want %d", turn, block, blocks[turn])
				}
			}
		})
		if !found {
			t.Errorf("turn %d omitted stair", turn)
		}
	}
}

func TestCastleFurnitureReservationsKeepFloorAndStandingHeadroom(t *testing.T) {
	t.Parallel()
	s := SchematicFor(BuildingKeep)
	for _, wing := range []struct {
		floors     []int
		rectangles [][4]int
	}{
		{[]int{0, 7, 14, 21}, [][4]int{{18, 23, 19, 23}, {19, 23, 7, 17}}},
		{[]int{0, 7, 14, 21, 28}, [][4]int{{38, 44, 20, 23}, {49, 55, 20, 23}, {39, 42, 7, 10}, {39, 42, 14, 18}}},
	} {
		for _, y := range wing.floors {
			for _, r := range wing.rectangles {
				for x := r[0]; x <= r[1]; x++ {
					for z := r[2]; z <= r[3]; z++ {
						if !standableCell(s, x, y, z) {
							t.Fatalf("reserved furniture cell (%d,%d,%d) lost floor or headroom", x, y, z)
						}
					}
				}
			}
		}
	}
}

func TestEastCastleRoomRoutesStayTwoCellsWideOnEveryFloor(t *testing.T) {
	t.Parallel()
	s := SchematicFor(BuildingKeep)
	// These are the public floor-plan reservations, not coordinates derived from
	// whichever air cells the drawing happens to contain.
	for _, y := range []int{0, 7, 14, 21, 28} {
		for _, r := range [][4]int{{49, 50, 24, 38}, {37, 56, 24, 25}, {37, 38, 6, 19}, {45, 48, 20, 25}} {
			for x := r[0]; x <= r[1]; x++ {
				for z := r[2]; z <= r[3]; z++ {
					if !standableCell(s, x, y, z) {
						t.Fatalf("east route cell (%d,%d,%d) lacks standing clearance", x, y, z)
					}
				}
			}
		}
	}
}

func TestCastleCurtainAndCornerLookoutsKeepTwoWideGuardedRoutes(t *testing.T) {
	t.Parallel()
	s := SchematicFor(BuildingKeep)
	for _, r := range [][4]int{
		{1, 2, 1, 61}, {60, 61, 1, 61}, {1, 61, 1, 2}, {1, 61, 60, 61},
		{1, 7, 1, 7}, {55, 61, 1, 7}, {1, 7, 55, 61}, {55, 61, 55, 61},
		{1, 5, 29, 30},
	} {
		for x := r[0]; x <= r[1]; x++ {
			for z := r[2]; z <= r[3]; z++ {
				if !standableCell(s, x, 13, z) {
					t.Fatalf("curtain/lookout (%d,13,%d) lost floor or standing clearance", x, z)
				}
			}
		}
	}
	for step := range 13 {
		z := 43 - step
		for x := 4; x <= 5; x++ {
			if s.At(x, step, z) != SlateStairNorthBottom {
				t.Fatalf("bailey tread (%d,%d,%d) is not a north stair", x, step, z)
			}
		}
		for _, x := range []int{3, 6} {
			if !Solid(s.At(x, step+1, z)) {
				t.Fatalf("bailey guard (%d,%d,%d) missing", x, step+1, z)
			}
		}
	}
	for n := 9; n <= 53; n++ {
		for _, cell := range [][2]int{{3, n}, {59, n}, {n, 3}, {n, 59}} {
			if cell[0] == 3 && (cell[1] == 29 || cell[1] == 30) {
				continue
			}
			if !Solid(s.At(cell[0], 13, cell[1])) {
				t.Fatalf("curtain inner parapet (%d,13,%d) missing", cell[0], cell[1])
			}
		}
	}
}

func TestWestTowerLandingsLookoutGuardsAndFurnitureReservations(t *testing.T) {
	t.Parallel()
	s := SchematicFor(BuildingKeep)
	for _, tower := range []struct{ x, z, lookout int }{{10, 12, 35}, {20, 32, 29}} {
		for i, y := 0, 21; y < tower.lookout; i, y = i+1, y+3 {
			z0 := tower.z - 3
			if i%2 == 1 {
				z0 = tower.z + 2
			}
			for x := tower.x - 3; x <= tower.x+3; x++ {
				for z := z0; z <= z0+1; z++ {
					if !standableCell(s, x, y, z) {
						t.Fatalf("tower landing (%d,%d,%d) obstructed", x, y, z)
					}
				}
			}
		}
		// The two southern rows are circulation, while this central pocket is
		// reserved for furniture; both stay independent of the emerging flight.
		for _, r := range [][4]int{{tower.x - 1, tower.x + 1, tower.z, tower.z + 1}, {tower.x - 3, tower.x + 3, tower.z + 2, tower.z + 3}} {
			for x := r[0]; x <= r[1]; x++ {
				for z := r[2]; z <= r[3]; z++ {
					if !standableCell(s, x, tower.lookout, z) {
						t.Fatalf("lookout reservation (%d,%d,%d) obstructed", x, tower.lookout, z)
					}
				}
			}
		}
		for x := tower.x + 2; x <= tower.x+3; x++ {
			if !Solid(s.At(x, tower.lookout, tower.z)) {
				t.Fatalf("lookout return-flight opening (%d,%d,%d) lacks guard", x, tower.lookout, tower.z)
			}
		}
		for x := tower.x - 1; x <= tower.x+3; x++ {
			if !Solid(s.At(x, tower.lookout, tower.z-1)) {
				t.Fatalf("lookout stair opening at (%d,%d,%d) lacks guard", x, tower.lookout, tower.z-1)
			}
		}
	}
}
