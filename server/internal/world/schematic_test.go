package world

import "testing"

// everySchematic is the four drawings, named, so a failure says which one.
func everySchematic() []struct {
	kind BuildingKind
	s    *Schematic
} {
	kinds := []BuildingKind{BuildingHut, BuildingSmithy, BuildingHall, BuildingKeep}
	out := make([]struct {
		kind BuildingKind
		s    *Schematic
	}, 0, len(kinds))
	for _, kind := range kinds {
		out = append(out, struct {
			kind BuildingKind
			s    *Schematic
		}{kind, SchematicFor(kind)})
	}
	return out
}

// TestEverySchematicIsRectangularAndLegible is the drawings' own contract.
//
// **[mustSchematic] already panics on a ragged literal, and that is exactly why this
// test exists rather than being redundant.** A panic at package initialisation takes
// down whichever process touches the package first, which in production is a server
// booting; the point of asserting it here is that the failure arrives as a red test on
// a pull request instead. The rune legend is checked the same way and for the same
// reason: a typo in a picture is the most likely edit to this file by a long way.
func TestEverySchematicIsRectangularAndLegible(t *testing.T) {
	t.Parallel()

	known := map[Block]bool{}
	for _, block := range schematicLegend {
		known[block] = true
	}

	for _, drawing := range everySchematic() {
		s := drawing.s
		if s.W <= 0 || s.H <= 0 || s.D <= 0 {
			t.Fatalf("%v is %d×%d×%d", drawing.kind, s.W, s.H, s.D)
		}
		if len(s.Voxels) != s.W*s.H*s.D {
			t.Fatalf("%v holds %d voxels for a %d×%d×%d box", drawing.kind, len(s.Voxels), s.W, s.H, s.D)
		}
		for _, block := range s.Voxels {
			if !known[block] {
				t.Fatalf("%v holds block %d, which no legend rune produces", drawing.kind, block)
			}
		}
	}
}

// TestEverySchematicIsTheSizeItsIssueAsksFor pins the four footprints.
//
// Not a restatement of the literals: the sizes are what every other number in
// settlement.go was chosen against — the ring radii, the plateau radius and the two
// compile-time guards that keep buildings from overlapping — so a drawing that grew a
// row would quietly push a hut into a hall.
func TestEverySchematicIsTheSizeItsIssueAsksFor(t *testing.T) {
	t.Parallel()

	want := map[BuildingKind][3]int{
		BuildingHut:    {7, 5, 7},
		BuildingSmithy: {9, 6, 9},
		BuildingHall:   {13, 8, 13},
		BuildingKeep:   {15, 14, 15},
	}
	for _, drawing := range everySchematic() {
		got := [3]int{drawing.s.W, drawing.s.H, drawing.s.D}
		if got != want[drawing.kind] {
			t.Errorf("%v is %v, want %v", drawing.kind, got, want[drawing.kind])
		}
		// Odd on both horizontal axes, which is what lets a building be centred on
		// its plot exactly rather than half a block off it.
		if drawing.s.W%2 == 0 || drawing.s.D%2 == 0 {
			t.Errorf("%v has an even footprint %d×%d and cannot be centred on a column", drawing.kind, drawing.s.W, drawing.s.D)
		}
	}
}

// TestEveryAnchorSitsInAirOnTheDrawingsFloor is the half of the anchor contract that
// can be checked without generating anything.
//
// A slot has to be somewhere a thing can stand: air in the drawing, on the floor course,
// and inside the walls rather than in the doorway. The other half — that the block under
// it is solid — is a fact about the world and is asserted in settlement_test.go against
// generated chunks.
func TestEveryAnchorSitsInAirOnTheDrawingsFloor(t *testing.T) {
	t.Parallel()

	seen := map[AnchorKind]int{}
	for _, drawing := range everySchematic() {
		if len(drawing.s.Anchors) == 0 {
			t.Errorf("%v offers no slot at all", drawing.kind)
		}
		for _, a := range drawing.s.Anchors {
			if a.Kind == AnchorNone {
				t.Errorf("%v has a slot for nothing", drawing.kind)
			}
			if a.Y != 0 {
				t.Errorf("%v's %v slot is at y=%d; a slot is on the floor", drawing.kind, a.Kind, a.Y)
			}
			if got := drawing.s.At(a.X, a.Y, a.Z); got != Air {
				t.Errorf("%v's %v slot is in block %d, not air", drawing.kind, a.Kind, got)
			}
			seen[a.Kind]++
		}
	}

	// Every word in the vocabulary is used by some drawing. An [AnchorKind] nothing
	// places is a promise to the stations and residents issues that this package does
	// not keep.
	for _, kind := range []AnchorKind{
		AnchorForge, AnchorCampfire, AnchorSmith, AnchorCarpenter,
		AnchorCook, AnchorTrader, AnchorVillager, AnchorGuard,
	} {
		if seen[kind] == 0 {
			t.Errorf("no drawing offers a %v slot", kind)
		}
	}
}

// TestRotationIsABijectionOverTheFootprint is what makes a turned building a turned
// building rather than a folded one.
//
// If two cells ever mapped onto one, a rotated wall would have a hole in it and the
// voxel that should have filled it would be somewhere else. Checking the map is onto
// the whole footprint checks both directions at once, because the two sets are the same
// size.
func TestRotationIsABijectionOverTheFootprint(t *testing.T) {
	t.Parallel()

	for _, drawing := range everySchematic() {
		s := drawing.s
		for _, facing := range []Facing{FacingPlusZ, FacingMinusX, FacingMinusZ, FacingPlusX} {
			w, d := rotatedFootprint(s, facing)
			hit := make([]bool, w*d)
			for z := range s.D {
				for x := range s.W {
					rx, rz := rotateCell(x, z, s.W, s.D, facing)
					if rx < 0 || rx >= w || rz < 0 || rz >= d {
						t.Fatalf("%v facing %d: (%d, %d) rotates outside the %d×%d footprint", drawing.kind, facing, x, z, w, d)
					}
					if hit[rz*w+rx] {
						t.Fatalf("%v facing %d: two cells rotate onto (%d, %d)", drawing.kind, facing, rx, rz)
					}
					hit[rz*w+rx] = true
				}
			}
		}
	}
}

// TestADoorEndsUpFacingTheWayItsFacingSays is the convention every placement depends on.
//
// Each drawing puts its doorway on the +Z face, and [Facing] is named after where that
// face ends up. The proof is the corner-free one: the drawing's front-centre column
// lands on the rotated footprint's corresponding edge, in the direction the name claims.
func TestADoorEndsUpFacingTheWayItsFacingSays(t *testing.T) {
	t.Parallel()

	for _, drawing := range everySchematic() {
		s := drawing.s
		doorX, doorZ := s.W/2, s.D-1

		for _, c := range []struct {
			facing Facing
			wantX  int
			wantZ  int
		}{
			{FacingPlusZ, s.W / 2, s.D - 1},
			{FacingMinusX, 0, s.W / 2},
			{FacingMinusZ, s.W / 2, 0},
			{FacingPlusX, s.D - 1, s.W / 2},
		} {
			gotX, gotZ := rotateCell(doorX, doorZ, s.W, s.D, c.facing)
			if gotX != c.wantX || gotZ != c.wantZ {
				t.Errorf("%v facing %d: the doorway lands at (%d, %d), want (%d, %d)",
					drawing.kind, c.facing, gotX, gotZ, c.wantX, c.wantZ)
			}
		}
	}
}

// TestASchematicHasBothWallsAndARoom: every drawing has something solid in it and
// something hollow in it.
//
// The cheapest guard against a picture that was edited into a solid block or into
// nothing at all — both of which would still be rectangular, still be legible, and
// still place without error.
func TestASchematicHasBothWallsAndARoom(t *testing.T) {
	t.Parallel()

	for _, drawing := range everySchematic() {
		solid, air := 0, 0
		for _, block := range drawing.s.Voxels {
			switch {
			case block == keepTerrain:
			case Solid(block):
				solid++
			default:
				air++
			}
		}
		if solid == 0 || air == 0 {
			t.Errorf("%v has %d solid voxels and %d of air; a building is neither a block nor a hole", drawing.kind, solid, air)
		}
	}
}

// TestAPlacedBuildingIsItsDrawingMovedAndTurned is the placement half of this file: a
// building in world coordinates holds exactly the drawing's voxels, once each, inside
// the footprint its origin and facing claim.
//
// **The anchors are the part worth checking here rather than downstream.** A slot is a
// coordinate two other issues will build entities from, and the failure mode — a slot
// that stayed in the drawing's frame while the walls turned — puts a forge outside its
// smithy while every voxel of the smithy is still correct.
func TestAPlacedBuildingIsItsDrawingMovedAndTurned(t *testing.T) {
	t.Parallel()

	const plotX, plotZ, floorY = 1000, -2000, 70

	for _, drawing := range everySchematic() {
		for _, facing := range []Facing{FacingPlusZ, FacingMinusX, FacingMinusZ, FacingPlusX} {
			b := centredBuilding(drawing.kind, plotX, plotZ, floorY, facing)
			w, d := rotatedFootprint(drawing.s, facing)

			if b.OriginX != plotX-int64(w/2) || b.OriginZ != plotZ-int64(d/2) || b.OriginY != floorY {
				t.Fatalf("%v facing %d is at (%d, %d, %d) for a plot at (%d, %d) and a floor at %d",
					drawing.kind, facing, b.OriginX, b.OriginY, b.OriginZ, plotX, plotZ, floorY)
			}

			seen := map[[3]int64]bool{}
			visitSchematic(b, func(x, y, z int64, _ Block) {
				cell := [3]int64{x, y, z}
				if seen[cell] {
					t.Fatalf("%v facing %d yields (%d, %d, %d) twice", drawing.kind, facing, x, y, z)
				}
				seen[cell] = true
				if x < b.OriginX || x >= b.OriginX+int64(w) ||
					z < b.OriginZ || z >= b.OriginZ+int64(d) ||
					y < b.OriginY || y >= b.OriginY+int64(drawing.s.H) {
					t.Fatalf("%v facing %d yields (%d, %d, %d), outside its own footprint", drawing.kind, facing, x, y, z)
				}
			})

			if len(b.Anchors) != len(drawing.s.Anchors) {
				t.Fatalf("%v facing %d carries %d slots for a drawing with %d", drawing.kind, facing, len(b.Anchors), len(drawing.s.Anchors))
			}
			for i, a := range b.Anchors {
				if a.Kind != drawing.s.Anchors[i].Kind {
					t.Fatalf("%v facing %d: slot %d is a %v, and the drawing's is a %v",
						drawing.kind, facing, i, a.Kind, drawing.s.Anchors[i].Kind)
				}
				if !seen[[3]int64{a.X, a.Y, a.Z}] {
					t.Fatalf("%v facing %d: the %v slot at (%d, %d, %d) is not a cell of the placed drawing",
						drawing.kind, facing, a.Kind, a.X, a.Y, a.Z)
				}
			}
		}
	}
}
