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
