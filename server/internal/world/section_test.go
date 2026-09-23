package world

import (
	"strings"
	"testing"
)

func mustBuildSection(t *testing.T, s *Section) *Schematic {
	t.Helper()
	out, err := s.Build()
	if err != nil {
		t.Fatalf("Build: %v", err)
	}
	return out
}

func TestANewSectionIsVoidAndBuildsNothing(t *testing.T) {
	if _, err := NewSection(4, 4, 4).Build(); err == nil {
		t.Fatal("an all-void section built")
	}
	if _, err := NewSection(0, 4, 4).Build(); err == nil {
		t.Fatal("a section with no volume built")
	}
}

// A room is air inside a one-block shell, and the shell only claims void: a second
// room carved against the first opens into it rather than walling it off.
func TestCarveRoomShellsOnlyVoidSoRoomsJoin(t *testing.T) {
	s := mustBuildSection(t, NewSection(12, 6, 7).
		CarveRoom(Box{1, 1, 1, 4, 3, 5}, Basalt).
		CarveRoom(Box{5, 1, 2, 10, 3, 4}, BlackBrick))

	for y := 1; y <= 3; y++ {
		for z := 1; z <= 5; z++ {
			for x := 1; x <= 4; x++ {
				if s.At(x, y, z) != Air {
					t.Fatalf("room cell %d,%d,%d is %d, want air", x, y, z, s.At(x, y, z))
				}
			}
		}
	}
	// The first room's shell stands where the second did not carve.
	for _, c := range [][3]int{{0, 2, 3}, {2, 0, 3}, {2, 4, 3}, {2, 2, 0}, {2, 2, 6}, {5, 2, 1}} {
		if got := s.At(c[0], c[1], c[2]); got != Basalt {
			t.Errorf("shell cell %v is %d, want Basalt", c, got)
		}
	}
	// The shared wall at x=5 became the second room's air: the rooms join.
	if got := s.At(5, 2, 3); got != Air {
		t.Errorf("the rooms do not join at x=5: %d", got)
	}
	// The second room's shell did not overwrite the first room's.
	if got := s.At(5, 2, 1); got != Basalt {
		t.Errorf("the second shell overwrote the first: %d", got)
	}
	if got := s.At(11, 2, 3); got != BlackBrick {
		t.Errorf("the second room's far wall is %d, want BlackBrick", got)
	}
	if got := s.At(11, 5, 6); got != keepTerrain {
		t.Errorf("a cell no room reached is %d, want void", got)
	}
}

func TestSectionOperationsRefuseBoxesOutsideTheFrame(t *testing.T) {
	for name, s := range map[string]*Section{
		"room":      NewSection(4, 4, 4).CarveRoom(Box{1, 1, 1, 4, 2, 2}, Stone),
		"inverted":  NewSection(4, 4, 4).Fill(Box{2, 1, 1, 1, 2, 2}, Stone),
		"floor":     NewSection(4, 4, 4).FillFloor(0, 0, 3, 3, 4, Sand),
		"corridor":  NewSection(8, 4, 8).CarveCorridor(1, 1, 5, 5, 1, 0, 2, Stone),
		"stairs":    NewSection(4, 4, 4).PlaceStairs(1, 1, 1, FacingPlusZ, 3, 1, Stone),
		"bad-stair": NewSection(8, 8, 8).PlaceStairs(1, 1, 1, Facing(7), 1, 1, Stone),
	} {
		if _, err := s.Build(); err == nil {
			t.Errorf("%s: an out-of-frame operation built", name)
		}
	}
	// And the first failure wins: later calls on a broken section change nothing.
	s := NewSection(4, 4, 4).Fill(Box{0, 0, 0, 9, 0, 0}, Stone).Fill(Box{0, 0, 0, 0, 0, 0}, Stone)
	if _, err := s.Build(); err == nil || !strings.Contains(err.Error(), "fill") {
		t.Errorf("the first failure was not the one reported: %v", err)
	}
}

// A corridor runs along X, then along Z, at the given width and height, shelled like
// a room — so it joins the two rooms at its ends.
func TestCarveCorridorCutsAnLBetweenTwoRooms(t *testing.T) {
	s := mustBuildSection(t, NewSection(20, 6, 20).
		CarveRoom(Box{1, 1, 1, 4, 3, 4}, Basalt).
		CarveRoom(Box{14, 1, 14, 18, 3, 18}, Basalt).
		CarveCorridor(3, 3, 16, 16, 1, 3, 2, Basalt))

	for x := 2; x <= 17; x++ { // the X leg, three wide around z=3
		for z := 2; z <= 4; z++ {
			if s.At(x, 1, z) != Air || s.At(x, 2, z) != Air {
				t.Fatalf("X leg cell %d,%d is not two tall air", x, z)
			}
		}
		if x > 5 && s.At(x, 3, 3) != Basalt {
			t.Errorf("X leg at x=%d has no ceiling: %d", x, s.At(x, 3, 3))
		}
	}
	for z := 2; z <= 17; z++ { // the Z leg, three wide around x=16
		if s.At(16, 1, z) != Air {
			t.Fatalf("Z leg cell 16,%d is %d", z, s.At(16, 1, z))
		}
	}
	if s.At(9, 0, 3) != Basalt {
		t.Error("the corridor has no floor")
	}
}

// A shaft opens every level it spans, and fill floor bridges it.
func TestCarveShaftAndFillFloor(t *testing.T) {
	s := mustBuildSection(t, NewSection(7, 40, 7).
		CarveShaft(2, 2, 4, 4, 1, 37, BlackBrick).
		FillFloor(2, 2, 4, 4, 20, Planks).
		FillFloor(2, 2, 4, 4, 1, Water))

	for y := 2; y <= 37; y++ {
		want := Air
		if y == 20 {
			want = Planks
		}
		if got := s.At(3, y, 3); got != want {
			t.Fatalf("shaft level %d holds %d, want %d", y, got, want)
		}
	}
	if s.At(3, 1, 3) != Water || s.At(3, 0, 3) != BlackBrick || s.At(3, 38, 3) != BlackBrick {
		t.Error("the shaft is not a closed well with a water floor")
	}
}

// Each facing climbs one level per step towards itself, with the stair's high half
// up the flight, a support under every step but the first and three cells of
// headroom above each.
func TestPlaceStairsClimbsTowardsItsFacing(t *testing.T) {
	for _, tc := range []struct {
		facing   Facing
		x, z     int
		dx, dz   int
		wantHigh ShapeFacing
	}{
		{FacingPlusZ, 5, 1, 0, 1, ShapeSouth},
		{FacingMinusZ, 5, 10, 0, -1, ShapeNorth},
		{FacingPlusX, 1, 5, 1, 0, ShapeEast},
		{FacingMinusX, 10, 5, -1, 0, ShapeWest},
	} {
		s := mustBuildSection(t, NewSection(12, 12, 12).
			CarveRoom(Box{1, 1, 1, 10, 10, 10}, Basalt).
			PlaceStairs(tc.x, 1, tc.z, tc.facing, 4, 2, Cobblestone))
		rx, rz := -tc.dz, tc.dx
		for i := range 4 {
			for j := range 2 {
				x, y, z := tc.x+tc.dx*i+rx*j, 1+i, tc.z+tc.dz*i+rz*j
				shape := ShapeOf(s.At(x, y, z))
				if shape.Kind != ShapeStair || shape.Half != ShapeBottom || shape.Facing != tc.wantHigh {
					t.Fatalf("facing %d step %d,%d: %+v, want a bottom stair high to %d", tc.facing, i, j, shape, tc.wantHigh)
				}
				for up := 1; up <= 3; up++ {
					if s.At(x, y+up, z) != Air {
						t.Fatalf("facing %d step %d,%d has no headroom at +%d", tc.facing, i, j, up)
					}
				}
				if i > 0 && s.At(x, y-1, z) != Cobblestone {
					t.Fatalf("facing %d step %d,%d stands on %d, want its support", tc.facing, i, j, s.At(x, y-1, z))
				}
			}
		}
	}
}

// A placed flight turns with its drawing: the high half still points up the flight
// after every quarter turn, because visitSchematic turns the block with its cell.
func TestPlacedStairsStillClimbAfterAQuarterTurn(t *testing.T) {
	s := mustBuildSection(t, NewSection(9, 9, 9).
		CarveRoom(Box{1, 1, 1, 7, 7, 7}, Basalt).
		PlaceStairs(4, 1, 1, FacingPlusZ, 3, 1, Cobblestone))
	step := map[ShapeFacing][2]int{ShapeNorth: {0, -1}, ShapeEast: {1, 0}, ShapeSouth: {0, 1}, ShapeWest: {-1, 0}}
	for facing := FacingPlusZ; facing <= FacingPlusX; facing++ {
		cells := make(map[[3]int]Block)
		for y := range s.H {
			for z := range s.D {
				for x := range s.W {
					rx, rz := rotateCell(x, z, s.W, s.D, facing)
					cells[[3]int{rx, y, rz}] = rotateSchematicBlock(s.At(x, y, z), facing)
				}
			}
		}
		// The next step is one level up and one cell towards the first step's high half.
		x0, z0 := rotateCell(4, 1, s.W, s.D, facing)
		first := ShapeOf(cells[[3]int{x0, 1, z0}])
		d := step[first.Facing]
		if next := ShapeOf(cells[[3]int{x0 + d[0], 2, z0 + d[1]}]); first.Kind != ShapeStair || next.Kind != ShapeStair || next.Facing != first.Facing {
			t.Errorf("facing %d: the flight does not climb towards its high half", facing)
		}
	}
}

// The dungeon anchors carry their index through placement, stand where their kind
// may, and a trigger volume is exactly two corners.
func TestSectionAnchorsObeyTheirKindsAndKeepTheirIndex(t *testing.T) {
	base := func() *Section {
		return NewSection(10, 6, 10).
			CarveRoom(Box{1, 1, 1, 8, 4, 8}, Basalt).
			Fill(Box{1, 1, 1, 1, 1, 1}, LeverOff).
			Fill(Box{8, 1, 8, 8, 1, 8}, RuneStone).
			Fill(Box{4, 1, 8, 5, 3, 8}, BlackBrick)
	}
	s := mustBuildSection(t, base().
		Anchor(AnchorInstanceCheckpoint, 2, 1, 2, 0).
		Anchor(AnchorInstanceMinorSpawn, 3, 1, 3, 4).
		Anchor(AnchorInstanceTrigger, 2, 1, 5, 1).
		Anchor(AnchorInstanceTrigger, 7, 3, 7, 1).
		Anchor(AnchorInstanceMechanism, 1, 1, 1, 2).
		Anchor(AnchorInstanceMechanism, 8, 1, 8, 2).
		Anchor(AnchorInstanceDoor, 4, 1, 8, 2).
		Anchor(AnchorInstanceDoor, 5, 3, 8, 2))

	placed := centreSchematic(BuildingRuin, 0, s, 100, -100, 7, FacingMinusX)
	if len(placed.Anchors) != 8 {
		t.Fatalf("%d anchors placed, want 8", len(placed.Anchors))
	}
	wantIndex := []int{0, 4, 1, 1, 2, 2, 2, 2}
	for i, a := range placed.Anchors {
		if a.Kind != s.Anchors[i].Kind || a.Index != wantIndex[i] || a.Y != 7+int64(s.Anchors[i].Y) {
			t.Errorf("anchor %d placed as %+v", i, a)
		}
	}

	for name, bad := range map[string]*Section{
		"one corner":        base().Anchor(AnchorInstanceTrigger, 2, 1, 5, 1),
		"three corners":     base().Anchor(AnchorInstanceTrigger, 2, 1, 5, 1).Anchor(AnchorInstanceTrigger, 3, 1, 5, 1).Anchor(AnchorInstanceTrigger, 4, 1, 5, 1),
		"mechanism in air":  base().Anchor(AnchorInstanceMechanism, 3, 1, 3, 0),
		"checkpoint inside": base().Anchor(AnchorInstanceCheckpoint, 0, 1, 1, 0),
		"door in void":      NewSection(10, 6, 10).CarveRoom(Box{1, 1, 1, 3, 3, 3}, Basalt).Anchor(AnchorInstanceDoor, 9, 5, 9, 0),
		"outside":           base().Anchor(AnchorInstanceMinorSpawn, 10, 1, 1, 0),
	} {
		if _, err := bad.Build(); err == nil {
			t.Errorf("%s: built", name)
		}
	}
	defer func() {
		if recover() == nil {
			t.Error("MustBuild did not panic on a broken section")
		}
	}()
	base().Anchor(AnchorInstanceMechanism, 3, 1, 3, 0).MustBuild()
}

func TestTheDungeonAnchorKindsAreAppendedAndNamed(t *testing.T) {
	kinds := []AnchorKind{AnchorInstanceCheckpoint, AnchorInstanceMinorSpawn, AnchorInstanceTrigger, AnchorInstanceMechanism, AnchorInstanceDoor}
	for i, kind := range kinds {
		if kind != AnchorInstanceGate+1+AnchorKind(i) {
			t.Errorf("%v is %d, want %d", kind, kind, AnchorInstanceGate+1+AnchorKind(i))
		}
		if name := kind.String(); !strings.HasPrefix(name, "instance ") {
			t.Errorf("kind %d is named %q", kind, name)
		}
	}
	for _, block := range []Block{LeverOff, LeverOn, RuneStone, RuneStoneLit} {
		if !Mechanism(block) {
			t.Errorf("block %d is not a mechanism", block)
		}
	}
	if Mechanism(Air) || Mechanism(Cobweb) || Mechanism(PortalHeart) {
		t.Error("a non-mechanism block is a mechanism")
	}
}
