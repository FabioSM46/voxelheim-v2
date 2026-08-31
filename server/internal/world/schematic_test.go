package world

import "testing"

// everySchematic is the four drawings, named, so a failure says which one, and each
// with the block its footing course is laid in.
//
// **The footing was a literal `Cobblestone` in the doorway test until #682**, which was
// true of four drawings built from a settlement's three materials and stopped being true
// the moment one of them was a castle. It is named per drawing rather than read off the
// picture on purpose: reading `s.At(0, 0, 0)` and then checking the ring against it
// asserts that a drawing agrees with itself, and would pass a keep whose footing had
// been drawn in thatch.
func everySchematic() []struct {
	kind    BuildingKind
	s       *Schematic
	footing Block
} {
	kinds := []struct {
		kind    BuildingKind
		footing Block
	}{
		{BuildingHut, Cobblestone},
		{BuildingSmithy, Cobblestone},
		{BuildingHall, Cobblestone},
		{BuildingKeep, Basalt},
	}
	out := make([]struct {
		kind    BuildingKind
		s       *Schematic
		footing Block
	}, 0, len(kinds))
	for _, k := range kinds {
		out = append(out, struct {
			kind    BuildingKind
			s       *Schematic
			footing Block
		}{k.kind, SchematicFor(k.kind), k.footing})
	}
	return out
}

// TestEverySchematicHoldsOnlyTheThingsADrawingCanMean is the drawings' own contract: a
// voxel is terrain left alone, air, one of the three materials a settlement is built out
// of, or one of the eight a castle is.
//
// **The set is written out here rather than derived from [schematicLegend], and that
// difference is the whole test.** Reading the legend and then checking every voxel
// against it asserts that a map agrees with itself: adding `'X': Water` to the legend
// and building a wall out of water passes such a test without a murmur. The blocks
// below are the independent statement — the one a reviewer can check against the issue
// rather than against the code under test. It is why this list grew by hand when the
// palette did, and that cost is the point of it.
func TestEverySchematicHoldsOnlyTheThingsADrawingCanMean(t *testing.T) {
	t.Parallel()

	allowed := map[Block]bool{
		keepTerrain: true,
		Air:         true,
		Cobblestone: true,
		Planks:      true,
		Thatch:      true,

		// #680's eight, spent by the keep and by nothing else yet.
		Basalt:           true,
		BlackBrick:       true,
		BlackBrickWorn:   true,
		SmoothBlackStone: true,
		SlateTile:        true,
		DarkTimber:       true,
		PaleTimber:       true,
		DarkGlass:        true,
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
			if !allowed[block] {
				t.Fatalf("%v holds block %d, which is not one of the things a drawing may mean", drawing.kind, block)
			}
		}
	}
}

// TestMustSchematicRefusesADrawingThatIsNotABox is the red test the panic's doc
// comment promises.
//
// **This is the test that makes the claim in [mustSchematic] true, and it did not
// exist.** The comment there says a ragged drawing "arrives as a test rather than as a
// crashed server"; nothing asserted that, so the package-init panic was the only thing
// standing behind the sentence — and a panic at init takes down whichever process
// touches the package first, which in production is a server booting. Every refusal is
// exercised from a recovered call here, so the promise is kept by a test failure on a
// pull request.
func TestMustSchematicRefusesADrawingThatIsNotABox(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name    string
		anchors []Anchor
		layers  [][]string
	}{
		{name: "no layers at all"},
		{name: "a layer with no rows", layers: [][]string{{}}},
		{
			name:   "a row narrower than the first",
			layers: [][]string{{"##", "#"}},
		},
		{
			name:   "a row wider than the first",
			layers: [][]string{{"##", "###"}},
		},
		{
			name:   "a layer deeper than the first",
			layers: [][]string{{"##", "##"}, {"##", "##", "##"}},
		},
		{
			name:   "a rune outside the legend",
			layers: [][]string{{"#X", "##"}},
		},
		{
			name:    "an anchor outside the drawing",
			anchors: []Anchor{{X: 5, Y: 0, Z: 0, Kind: AnchorForge}},
			layers:  [][]string{{"#_", "##"}},
		},
		{
			name:    "an anchor in a cell that is not air",
			anchors: []Anchor{{X: 0, Y: 0, Z: 0, Kind: AnchorForge}},
			layers:  [][]string{{"#_", "##"}},
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			defer func() {
				if recover() == nil {
					t.Errorf("mustSchematic accepted %s", tc.name)
				}
			}()
			mustSchematic(tc.anchors, tc.layers...)
		})
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
		BuildingKeep:   {63, 68, 63},
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

// TestTheDrawingsSayWhatTheirCommentsSayTheySay reads specific voxels at specific
// coordinates and compares them with specific blocks.
//
// **Every other test in this file counts, classifies or permutes; not one of them ever
// asserted that a named cell holds a named block.** So a course of planks turned to
// cobble, a roof ridge emptied to air, or [Schematic.At] transposing its two horizontal
// axes all left the suite green while the buildings came out wrong. These are the
// cheapest possible fixed points: one per material, per drawing, taken from the layer
// comments in schematic.go so that a picture edited without its comment fails here.
func TestTheDrawingsSayWhatTheirCommentsSayTheySay(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		kind    BuildingKind
		x, y, z int
		want    Block
		why     string
	}{
		{BuildingHut, 0, 0, 0, Cobblestone, "the footing course"},
		{BuildingHut, 1, 0, 1, Air, "the room"},
		{BuildingHut, 0, 1, 0, Planks, "the timber course"},
		{BuildingHut, 3, 2, 6, Planks, "the lintel over the doorway"},
		{BuildingHut, 0, 3, 0, Thatch, "the eaves"},
		{BuildingHut, 0, 4, 0, Air, "the cap is inset by one"},
		{BuildingHut, 3, 4, 3, Thatch, "the cap itself"},

		{BuildingSmithy, 0, 2, 2, Air, "a window on the long side"},
		{BuildingSmithy, 0, 2, 1, Planks, "the wall beside that window"},
		{BuildingSmithy, 4, 4, 4, Thatch, "the eaves"},

		{BuildingHall, 6, 0, 6, Air, "the floor the campfire stands on"},
		{BuildingHall, 0, 2, 3, Air, "a window on the long side"},
		{BuildingHall, 6, 7, 6, Thatch, "the ridge"},
		{BuildingHall, 2, 7, 2, Thatch, "the ridge's near corner"},
		{BuildingHall, 10, 7, 10, Thatch, "the ridge's far corner"},
		{BuildingHall, 1, 7, 2, Air, "the ridge is inset by two on x"},
		{BuildingHall, 2, 7, 1, Air, "and by two on z"},

		// The keep, and one fixed point per material it is drawn in — which is what
		// makes this the cheapest check that #680's palette is actually being spent
		// rather than merely declared.
		{BuildingKeep, 0, 0, 0, Basalt, "the footing course"},
		{BuildingKeep, 31, 0, 62, Air, "the gate"},
		{BuildingKeep, 31, 8, 62, SmoothBlackStone, "the lintel that closes it"},
		{BuildingKeep, 0, 3, 31, BlackBrickWorn, "the weathered band low on the curtain"},
		{BuildingKeep, 0, 6, 31, SmoothBlackStone, "the curtain's string course"},
		{BuildingKeep, 0, 13, 31, BlackBrick, "the parapet"},
		{BuildingKeep, 1, 13, 31, Air, "and the wall walk inside it"},
		{BuildingKeep, 0, 15, 30, Air, "a crenel cut through that parapet"},
		{BuildingKeep, 8, 2, 4, DarkGlass, "a window on the west wing's back wall"},
		{BuildingKeep, 8, 4, 4, PaleTimber, "the lintel over that window"},
		{BuildingKeep, 16, 6, 22, DarkTimber, "the west wing's first-floor slab"},
		{BuildingKeep, 8, 2, 33, DarkTimber, "a tread of the first flight"},
		{BuildingKeep, 8, 3, 33, Air, "and the headroom over that tread"},
		{BuildingKeep, 31, 20, 25, DarkTimber, "the bridge deck over the inner court"},
		{BuildingKeep, 31, 21, 25, Air, "the bridge's own room"},
		{BuildingKeep, 16, 28, 22, SlateTile, "the west wing's roof"},
		{BuildingKeep, 50, 67, 12, SlateTile, "the great spire's point"},
	} {
		s := SchematicFor(tc.kind)
		if got := s.At(tc.x, tc.y, tc.z); got != tc.want {
			t.Errorf("%v at (%d, %d, %d) is block %d, want %d — %s",
				tc.kind, tc.x, tc.y, tc.z, got, tc.want, tc.why)
		}
	}
}

// TestEveryDrawingPutsItsDoorwayOnThePlusZFace is the convention [Facing] is named
// after, asserted against the pictures rather than assumed.
//
// **[Facing]'s doc comment calls this "what makes one number enough", and nothing read
// a block to check it.** The rotation test below turns the coordinate the doorway is
// *supposed* to be at and never looks at what is there, so bricking up a doorway, or
// moving one to the −Z face, was invisible. The floor course is where a body walks in,
// so that is the course this reads: a centred run of air on the +Z wall, cobble
// everywhere else around the outside.
func TestEveryDrawingPutsItsDoorwayOnThePlusZFace(t *testing.T) {
	t.Parallel()

	for _, drawing := range everySchematic() {
		s := drawing.s
		front := s.D - 1

		lo, hi := -1, -1
		for x := range s.W {
			if s.At(x, 0, front) == Air {
				if lo < 0 {
					lo = x
				}
				hi = x
			}
		}
		if lo < 0 {
			t.Errorf("%v has no doorway anywhere on its +Z face", drawing.kind)
			continue
		}
		// Centred, and one unbroken run: `lo+hi == W-1` is the mirror statement of
		// "centred", and every cell between them being air is what makes it a doorway
		// rather than two arrow slits.
		if lo+hi != s.W-1 {
			t.Errorf("%v's +Z doorway spans x=%d..%d, which is not centred on a wall %d wide",
				drawing.kind, lo, hi, s.W)
		}
		for x := lo; x <= hi; x++ {
			if got := s.At(x, 0, front); got != Air {
				t.Errorf("%v's doorway is broken by block %d at x=%d", drawing.kind, got, x)
			}
		}

		// And it is the only way in on this course. Without this half, a drawing that
		// grew a second door on its back wall would still satisfy everything above —
		// and a building with two doors has no facing at all.
		for x := range s.W {
			if x >= lo && x <= hi {
				continue
			}
			if got := s.At(x, 0, front); got != drawing.footing {
				t.Errorf("%v's front wall holds block %d at x=%d, want its footing block %d", drawing.kind, got, x, drawing.footing)
			}
		}
		for x := range s.W {
			if got := s.At(x, 0, 0); got != drawing.footing {
				t.Errorf("%v's back wall holds block %d at x=%d; the doorway is on +Z and nowhere else", drawing.kind, got, x)
			}
		}
		for z := range s.D {
			for _, x := range []int{0, s.W - 1} {
				if got := s.At(x, 0, z); got != drawing.footing {
					t.Errorf("%v's side wall holds block %d at (%d, %d); the doorway is on +Z and nowhere else", drawing.kind, got, x, z)
				}
			}
		}
	}
}

// TestEveryAnchorIsWhereItsBuildingPutsIt pins each slot's kind and coordinate.
//
// **The two other anchor tests are collective and neither can see a moved slot.** One
// checks that every [AnchorKind] in the vocabulary is used by *some* drawing, which a
// swap between two buildings satisfies; the other checks that a slot is air on the
// floor course, which most of a room's cells are. So a forge could move to the far
// corner of its smithy, a trader could be stood inside the campfire, and a guard could
// be moved into the open gateway, with the suite green. Two other issues build
// entities from these coordinates: they are the output of this file, and an output is
// pinned here.
func TestEveryAnchorIsWhereItsBuildingPutsIt(t *testing.T) {
	t.Parallel()

	want := map[BuildingKind][]Anchor{
		BuildingHut: {
			{X: 3, Y: 0, Z: 3, Kind: AnchorVillager},
		},
		BuildingSmithy: {
			{X: 2, Y: 0, Z: 2, Kind: AnchorForge},
			{X: 6, Y: 0, Z: 6, Kind: AnchorSmith},
		},
		BuildingHall: {
			{X: 6, Y: 0, Z: 6, Kind: AnchorCampfire},
			{X: 9, Y: 0, Z: 4, Kind: AnchorCook},
			{X: 3, Y: 0, Z: 9, Kind: AnchorTrader},
		},
		BuildingKeep: {
			{X: 29, Y: 0, Z: 59, Kind: AnchorGuard},
			{X: 33, Y: 0, Z: 59, Kind: AnchorGuard},
			{X: 16, Y: 0, Z: 22, Kind: AnchorCarpenter},
		},
	}

	for _, drawing := range everySchematic() {
		got := drawing.s.Anchors
		expected := want[drawing.kind]
		if len(got) != len(expected) {
			t.Errorf("%v offers %d slots, want %d", drawing.kind, len(got), len(expected))
			continue
		}
		for i := range expected {
			if got[i] != expected[i] {
				t.Errorf("%v's slot %d is %+v, want %+v", drawing.kind, i, got[i], expected[i])
			}
		}
	}
}

// TestEveryAnchorSitsInAirWithSomethingToStandUnderIt is the half of the anchor
// contract that can be checked without generating anything.
//
// A slot has to be somewhere a thing can stand: air in the drawing, on the floor
// course, with air above it too — the entity the stations and residents issues will put
// there is a body and not a doormat — and no two slots in the same cell, which would
// stack two of them. The remaining half — that the block *under* it is solid — is a
// fact about the world and is asserted in settlement_test.go against generated chunks.
func TestEveryAnchorSitsInAirWithSomethingToStandUnderIt(t *testing.T) {
	t.Parallel()

	seen := map[AnchorKind]int{}
	for _, drawing := range everySchematic() {
		if len(drawing.s.Anchors) == 0 {
			t.Errorf("%v offers no slot at all", drawing.kind)
		}
		occupied := map[[3]int]bool{}
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
			if a.Y+1 < drawing.s.H {
				if got := drawing.s.At(a.X, a.Y+1, a.Z); got != Air {
					t.Errorf("%v's %v slot has block %d over its head", drawing.kind, a.Kind, got)
				}
			}
			cell := [3]int{a.X, a.Y, a.Z}
			if occupied[cell] {
				t.Errorf("%v puts two slots in cell %v", drawing.kind, cell)
			}
			occupied[cell] = true
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
//
// **It runs over a deliberately non-square shape as well as the four drawings**, for
// the reason spelled out on [TestAQuarterTurnIsARotationAndNotAReflection]: every real
// drawing is square, and a square hides half of what a rotation does.
func TestRotationIsABijectionOverTheFootprint(t *testing.T) {
	t.Parallel()

	shapes := []struct {
		name string
		w, d int
	}{{"3×5", 3, 5}, {"5×3", 5, 3}}
	for _, drawing := range everySchematic() {
		shapes = append(shapes, struct {
			name string
			w, d int
		}{drawing.kind.String(), drawing.s.W, drawing.s.D})
	}

	for _, shape := range shapes {
		for _, facing := range []Facing{FacingPlusZ, FacingMinusX, FacingMinusZ, FacingPlusX} {
			w, d := shape.w, shape.d
			if facing == FacingMinusX || facing == FacingPlusX {
				w, d = shape.d, shape.w
			}
			hit := make([]bool, w*d)
			for z := range shape.d {
				for x := range shape.w {
					rx, rz := rotateCell(x, z, shape.w, shape.d, facing)
					if rx < 0 || rx >= w || rz < 0 || rz >= d {
						t.Fatalf("%s facing %d: (%d, %d) rotates outside the %d×%d footprint", shape.name, facing, x, z, w, d)
					}
					if hit[rz*w+rx] {
						t.Fatalf("%s facing %d: two cells rotate onto (%d, %d)", shape.name, facing, rx, rz)
					}
					hit[rz*w+rx] = true
				}
			}
		}
	}
}

// TestAQuarterTurnIsARotationAndNotAReflection is the test the four drawings cannot be.
//
// **Every schematic in this file is square — 7×7, 9×9, 13×13, 15×15 — and a square is
// exactly the shape that cannot tell a rotation from a mirror.** The bijection above
// accepts a reflection, because a reflection is also a bijection; the doorway test
// probes the front-centre column, which is a fixed point of the x-mirror for every odd
// width; and [rotatedFootprint]'s whole job — swapping W and D on an odd number of
// turns — is unobservable when W equals D. So this one runs on 3×5, where a mirror
// lands somewhere a rotation does not and the two footprints differ.
//
// The expected images are written out rather than computed, so this test states the
// rotation instead of restating [rotateCell].
func TestAQuarterTurnIsARotationAndNotAReflection(t *testing.T) {
	t.Parallel()

	const w, d = 3, 5
	fixture := &Schematic{W: w, H: 1, D: d, Voxels: make([]Block, w*d)}

	for _, tc := range []struct {
		facing     Facing
		wantW      int
		wantD      int
		wantCorner [4][2]int // the images of (0,0), (2,0), (0,4) and (2,4)
		wantDoor   [2]int    // the image of the front-centre cell (1,4)
	}{
		{FacingPlusZ, 3, 5, [4][2]int{{0, 0}, {2, 0}, {0, 4}, {2, 4}}, [2]int{1, 4}},
		{FacingMinusX, 5, 3, [4][2]int{{4, 0}, {4, 2}, {0, 0}, {0, 2}}, [2]int{0, 1}},
		{FacingMinusZ, 3, 5, [4][2]int{{2, 4}, {0, 4}, {2, 0}, {0, 0}}, [2]int{1, 0}},
		{FacingPlusX, 5, 3, [4][2]int{{0, 2}, {0, 0}, {4, 2}, {4, 0}}, [2]int{4, 1}},
	} {
		gotW, gotD := rotatedFootprint(fixture, tc.facing)
		if gotW != tc.wantW || gotD != tc.wantD {
			t.Errorf("facing %d turns a %d×%d footprint into %d×%d, want %d×%d",
				tc.facing, w, d, gotW, gotD, tc.wantW, tc.wantD)
		}

		corners := [4][2]int{{0, 0}, {2, 0}, {0, 4}, {2, 4}}
		for i, cell := range corners {
			rx, rz := rotateCell(cell[0], cell[1], w, d, tc.facing)
			if [2]int{rx, rz} != tc.wantCorner[i] {
				t.Errorf("facing %d: (%d, %d) lands at (%d, %d), want %v",
					tc.facing, cell[0], cell[1], rx, rz, tc.wantCorner[i])
			}
		}

		rx, rz := rotateCell(1, 4, w, d, tc.facing)
		if [2]int{rx, rz} != tc.wantDoor {
			t.Errorf("facing %d: the front-centre cell lands at (%d, %d), want %v",
				tc.facing, rx, rz, tc.wantDoor)
		}
	}
}

// TestADoorEndsUpFacingTheWayItsFacingSays is the convention every placement depends on.
//
// Each drawing puts its doorway on the +Z face — which
// [TestEveryDrawingPutsItsDoorwayOnThePlusZFace] is what checks — and [Facing] is named
// after where that face ends up. The proof is the corner-free one: the drawing's
// front-centre column lands on the rotated footprint's corresponding edge, in the
// direction the name claims.
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
// building in world coordinates holds exactly the drawing's voxels, once each, in the
// cells its origin and facing claim — and its slots turned with its walls.
//
// **The anchors are the part worth checking here rather than downstream.** A slot is a
// coordinate two other issues will build entities from, and the failure mode — a slot
// that stayed in the drawing's frame while the walls turned — puts a forge outside its
// smithy while every voxel of the smithy is still correct. That is why the comparison
// below is against the rotated coordinate and not against membership of the placed
// building: every anchor is an interior air cell, so "is this one of the building's
// cells" is true under every facing and cannot see the bug this paragraph describes.
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

			// What the drawing says should be where, before anything is yielded.
			// Comparing the visitor's *block* against this is what makes the visitor's
			// third argument load-bearing: it used to be discarded, and a visitor that
			// yielded cobble for every cell of every building passed.
			want := map[[3]int64]Block{}
			for y := range drawing.s.H {
				for z := range drawing.s.D {
					for x := range drawing.s.W {
						block := drawing.s.At(x, y, z)
						if block == keepTerrain {
							continue
						}
						rx, rz := rotateCell(x, z, drawing.s.W, drawing.s.D, facing)
						want[[3]int64{b.OriginX + int64(rx), b.OriginY + int64(y), b.OriginZ + int64(rz)}] = block
					}
				}
			}

			seen := map[[3]int64]bool{}
			visitSchematic(b, func(x, y, z int64, block Block) {
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
				wantBlock, drawn := want[cell]
				if !drawn {
					t.Fatalf("%v facing %d yields (%d, %d, %d), which the drawing does not draw", drawing.kind, facing, x, y, z)
				}
				if block != wantBlock {
					t.Fatalf("%v facing %d yields block %d at (%d, %d, %d), want %d",
						drawing.kind, facing, block, x, y, z, wantBlock)
				}
			})
			if len(seen) != len(want) {
				t.Fatalf("%v facing %d yields %d cells for a drawing with %d", drawing.kind, facing, len(seen), len(want))
			}

			if len(b.Anchors) != len(drawing.s.Anchors) {
				t.Fatalf("%v facing %d carries %d slots for a drawing with %d", drawing.kind, facing, len(b.Anchors), len(drawing.s.Anchors))
			}
			for i, a := range b.Anchors {
				d := drawing.s.Anchors[i]
				if a.Kind != d.Kind {
					t.Fatalf("%v facing %d: slot %d is a %v, and the drawing's is a %v",
						drawing.kind, facing, i, a.Kind, d.Kind)
				}
				rx, rz := rotateCell(d.X, d.Z, drawing.s.W, drawing.s.D, facing)
				wantX := b.OriginX + int64(rx)
				wantY := b.OriginY + int64(d.Y)
				wantZ := b.OriginZ + int64(rz)
				if a.X != wantX || a.Y != wantY || a.Z != wantZ {
					t.Fatalf("%v facing %d: the %v slot is at (%d, %d, %d), want (%d, %d, %d) — a slot turns with the walls",
						drawing.kind, facing, a.Kind, a.X, a.Y, a.Z, wantX, wantY, wantZ)
				}
				if want[[3]int64{a.X, a.Y, a.Z}] != Air {
					t.Fatalf("%v facing %d: the %v slot at (%d, %d, %d) is not an air cell of the placed drawing",
						drawing.kind, facing, a.Kind, a.X, a.Y, a.Z)
				}
			}
		}
	}
}

// The helpers below and the two tests after them are the machine-checked half of #555's
// "no room a player can see into but never stand in".
//
// **Nothing here ever asked whether a body could get from one voxel to another.** Every
// other test in this file counts, classifies, permutes or reads a named cell, and a
// castle with three floors and no stairs between them satisfies all of them.
//
// The movement model is internal/game's rather than a convenience: a body is under two
// blocks tall, so standing takes two clear cells and a floor; the jump impulse clears one
// block and not two, so a step up is one block; falling is free. Below y=0 is the plateau
// the building stands on — solid, because [settlementSite.building] puts the floor course
// at `plateau + 1` — and outside the drawing is open air.
func schematicBlockAt(s *Schematic, x, y, z int) Block {
	if y < 0 {
		return Cobblestone
	}
	if y >= s.H || x < 0 || x >= s.W || z < 0 || z >= s.D {
		return Air
	}
	return s.At(x, y, z)
}

func passableCell(s *Schematic, x, y, z int) bool {
	block := schematicBlockAt(s, x, y, z)
	return block == Air || block == keepTerrain
}

func standableCell(s *Schematic, x, y, z int) bool {
	return passableCell(s, x, y, z) && passableCell(s, x, y+1, z) && !passableCell(s, x, y-1, z)
}

// walkSchematic is every cell reachable on foot from one standing cell.
func walkSchematic(s *Schematic, from [3]int) map[[3]int]bool {
	reached := map[[3]int]bool{from: true}
	queue := [][3]int{from}
	for len(queue) > 0 {
		c := queue[0]
		queue = queue[1:]
		for _, step := range [][2]int{{1, 0}, {-1, 0}, {0, 1}, {0, -1}} {
			for _, dy := range []int{1, 0, -1} {
				next := [3]int{c[0] + step[0], c[1] + dy, c[2] + step[1]}
				if next[0] < 0 || next[0] >= s.W || next[2] < 0 || next[2] >= s.D ||
					next[1] < 0 || next[1] >= s.H {
					continue
				}
				if reached[next] || !standableCell(s, next[0], next[1], next[2]) {
					continue
				}
				// A step up needs room over the head it is taken from; across and down
				// need nothing beyond the destination.
				if dy == 1 && !passableCell(s, c[0], c[1]+2, c[2]) {
					continue
				}
				reached[next] = true
				queue = append(queue, next)
			}
		}
	}
	return reached
}

// TestEveryRoomADrawingHasIsReachableFromItsDoorway walks each drawing from its door.
//
// **"Interior" is `roofed and off the outer face`, and both halves earn their place.**
// Roofed is what makes a cell a room: one with nothing solid above it is a rampart or a
// rooftop, and neither is a space a player is promised a way into — the castle's parapet
// is deliberately two blocks above its wall walk and deliberately not reachable. Off the
// outer face excludes a window sill, standable from outside and roofed by the course
// above it in every drawing here since the smithy was first drawn.
//
// The anchors come along because they are this file's output: a slot two other issues
// build an entity from is worth nothing if nothing can walk to it.
func TestEveryRoomADrawingHasIsReachableFromItsDoorway(t *testing.T) {
	t.Parallel()

	for _, drawing := range everySchematic() {
		s := drawing.s
		door := [3]int{s.W / 2, 0, s.D - 1}
		if !standableCell(s, door[0], door[1], door[2]) {
			t.Errorf("%v's doorway at %v is not somewhere a body can stand", drawing.kind, door)
			continue
		}
		reached := walkSchematic(s, door)

		for _, a := range s.Anchors {
			if !reached[[3]int{a.X, a.Y, a.Z}] {
				t.Errorf("%v's %v slot at (%d, %d, %d) cannot be walked to from the doorway",
					drawing.kind, a.Kind, a.X, a.Y, a.Z)
			}
		}

		sealed := 0
		for y := range s.H {
			for z := 1; z < s.D-1; z++ {
				for x := 1; x < s.W-1; x++ {
					if !standableCell(s, x, y, z) || reached[[3]int{x, y, z}] {
						continue
					}
					roofed := false
					for above := y + 2; above < s.H; above++ {
						if !passableCell(s, x, above, z) {
							roofed = true
							break
						}
					}
					if !roofed {
						continue
					}
					if sealed++; sealed <= 3 {
						t.Errorf("%v has a roofed floor cell at (%d, %d, %d) that nothing can walk to",
							drawing.kind, x, y, z)
					}
				}
			}
		}
		if sealed > 3 {
			t.Errorf("%v has %d sealed floor cells in all", drawing.kind, sealed)
		}
	}
}

// TestTheKeepHasNineFloorsAWallWalkAndABridgedCourtAndYouCanWalkToAllOfThem is the
// other half: the test above says nothing is sealed, and a castle whose upper floors are
// all solid masonry satisfies that perfectly. So the landmarks are named — each a
// coordinate the drawing's own comment promises, each standable and reachable on foot
// from outside the gate.
//
// **The wall walk is read on all four sides on purpose, and it is the one that nearly
// was not true.** Four corner towers stand on that ring, and until a slot was cut through
// each of them at walk height the walk was three dead ends and one stair — a state in
// which every other assertion here passes.
func TestTheKeepHasNineFloorsAWallWalkAndABridgedCourtAndYouCanWalkToAllOfThem(t *testing.T) {
	t.Parallel()

	s := SchematicFor(BuildingKeep)
	reached := walkSchematic(s, [3]int{31, 0, 62})

	for _, tc := range []struct {
		x, y, z int
		what    string
	}{
		{31, 0, 59, "the gate passage"},
		{20, 0, 50, "the bailey"},
		{16, 0, 22, "the west wing's ground floor"},
		{16, 7, 22, "the west wing's first floor"},
		{16, 14, 22, "the west wing's second floor"},
		{16, 21, 22, "the west wing's third floor"},
		{46, 0, 22, "the east keep's ground floor"},
		{46, 7, 22, "the east keep's first floor"},
		{46, 14, 22, "the east keep's second floor"},
		{46, 21, 22, "the east keep's third floor"},
		{46, 28, 22, "the east keep's fourth floor"},
		{8, 3, 33, "a tread of the west wing's first flight"},
		{2, 7, 36, "the bailey stair to the wall walk"},
		{1, 13, 30, "the wall walk, west"},
		{61, 13, 30, "the wall walk, east"},
		{31, 13, 1, "the wall walk, back"},
		{31, 13, 61, "the wall walk, front, through the gatehouse"},
		{31, 21, 25, "the bridge over the inner court"},
	} {
		if !standableCell(s, tc.x, tc.y, tc.z) {
			t.Errorf("%s at (%d, %d, %d) is not somewhere a body can stand", tc.what, tc.x, tc.y, tc.z)
			continue
		}
		if !reached[[3]int{tc.x, tc.y, tc.z}] {
			t.Errorf("%s at (%d, %d, %d) cannot be walked to from outside the gate", tc.what, tc.x, tc.y, tc.z)
		}
	}
}

// TestTheKeepsFourSpiresRiseFromCorbelledShafts pins the silhouette rather than a count
// of blocks. Each spired tower has a dressed shaft carried above the mass it stands in, a
// smooth capital that oversails that shaft by one block, and a slate point — and the four
// are read separately, which is what makes "four towers" mean four structures rather than
// four samples from one.
//
// **The taper is asserted at both ends, because a spire that never narrows is a chimney.**
// The course above the capital is still the shaft's full width; two blocks off the axis
// near the point is not slate at all.
func TestTheKeepsFourSpiresRiseFromCorbelledShafts(t *testing.T) {
	t.Parallel()

	s := SchematicFor(BuildingKeep)
	for _, tower := range []struct {
		name                  string
		cx, cz, r, shaft, tip int
	}{
		{"north-west", 10, 12, 6, 40, 56},
		{"south-west", 20, 32, 5, 34, 48},
		{"north-east", 50, 12, 7, 46, 67},
		{"south-east", 42, 32, 6, 40, 58},
	} {
		if got := s.At(tower.cx, tower.shaft-4, tower.cz); got != BlackBrick {
			t.Errorf("the %s tower's shaft is block %d below its capital, want dressed brick", tower.name, got)
		}
		if got := s.At(tower.cx+tower.r+1, tower.shaft, tower.cz); got != SmoothBlackStone {
			t.Errorf("the %s tower's capital is block %d where it oversails the shaft, want smooth stone", tower.name, got)
		}
		if got := s.At(tower.cx+tower.r, tower.shaft+1, tower.cz); got != SlateTile {
			t.Errorf("the %s tower's spire is block %d at its full width, want slate", tower.name, got)
		}
		if got := s.At(tower.cx, tower.tip, tower.cz); got != SlateTile {
			t.Errorf("the %s tower's point is block %d, want slate", tower.name, got)
		}
		if got := s.At(tower.cx+2, tower.tip-1, tower.cz); got == SlateTile {
			t.Errorf("the %s tower is still %d blocks wide a course below its point; it has not tapered", tower.name, 2)
		}
	}

	// The north-east spire is the tallest thing in the capital, and it reaches the last
	// course of the drawing — which is what makes the height of this schematic a
	// statement rather than padding.
	if got := s.At(50, s.H-1, 12); got != SlateTile {
		t.Errorf("the drawing's top course holds block %d over the great spire, want slate", got)
	}
}

// The eight runes the castle materials answer to, stated independently of the map
// that answers them.
//
// **Written out rather than ranged over, for the reason
// TestEverySchematicHoldsOnlyTheThingsADrawingCanMean writes its own out.**
// Ranging over [schematicLegend] and checking that each value is a block asserts that a
// map agrees with itself; the pairs below are the statement a reviewer can check against
// #680's table without reading the code under test. The length guard is what makes a
// rune added and left unnamed here a failure rather than a silent thirteenth entry — a
// drawing rune that means something nobody wrote down is exactly the typo the legend
// exists to refuse.
func TestTheCastleRunesNameTheCastleMaterials(t *testing.T) {
	t.Parallel()

	want := map[rune]Block{
		'.': keepTerrain,
		'_': Air,
		'#': Cobblestone,
		'P': Planks,
		'T': Thatch,
		'S': SmoothBlackStone,
		'b': Basalt,
		'K': BlackBrick,
		'k': BlackBrickWorn,
		'R': SlateTile,
		'D': DarkTimber,
		'W': PaleTimber,
		'G': DarkGlass,
	}

	if len(schematicLegend) != len(want) {
		t.Fatalf("the legend has %d runes and this test names %d", len(schematicLegend), len(want))
	}
	for r, block := range want {
		got, known := schematicLegend[r]
		if !known {
			t.Errorf("rune %q is not in the legend", r)
			continue
		}
		if got != block {
			t.Errorf("rune %q writes block %d, want %d", r, got, block)
		}
	}

	// Two runes meaning the same block would make a drawing ambiguous to read back,
	// which is the direction a legend can drift in without any single entry looking
	// wrong.
	seen := make(map[Block]rune, len(want))
	for r, block := range want {
		if first, dup := seen[block]; dup {
			t.Errorf("runes %q and %q both write block %d", first, r, block)
		}
		seen[block] = r
	}
}
