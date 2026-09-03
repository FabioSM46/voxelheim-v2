package world

import (
	"slices"
	"testing"
)

// setTreeBlock's condition is the whole rule two competing plant roots resolve by:
// [Cover] counts as air, so nothing that is neither air nor cover can ever be written
// over. **A trunk or a canopy — never cover — therefore always keeps or reclaims a
// voxel from a cover plant, whichever root the placeTrees loop reaches first.** That
// is the half of setTreeBlock's old order-independence claim that survives #874:
// before it, the comment this test used to carry named `Bush` as the worked example,
// because a bush was solid too. It still holds for every solid plant block; `Bush`
// is simply no longer one of them (see
// [TestTwoCoverPlantsResolveToWhicheverWasWrittenSecond]).
func TestATrunkOrCanopyAlwaysWinsOverCoverInEitherOrder(t *testing.T) {
	t.Parallel()

	const x, y, z = 4, 5, 6
	for _, tc := range []struct {
		name  string
		cover Block
		solid Block
	}{
		{"a flower against a log", FlowerRed, Log},
		{"a winter bramble against leaves", WinterBramble, Leaves},
		{"a bush against a log", Bush, Log},
		{"a desert shrub against palm fronds", DesertShrub, PalmFronds},
	} {
		for _, coverFirst := range []bool{true, false} {
			chunk := NewChunk(Coord{})
			if coverFirst {
				setTreeBlock(chunk, x, y, z, tc.cover)
				setTreeBlock(chunk, x, y, z, tc.solid)
			} else {
				setTreeBlock(chunk, x, y, z, tc.solid)
				setTreeBlock(chunk, x, y, z, tc.cover)
			}
			if got := chunk.At(x, y, z); got != tc.solid {
				t.Errorf("%s, cover written first=%t: the shared voxel holds %d, want the solid block %d",
					tc.name, coverFirst, got, tc.solid)
			}
		}
	}

	// The general shape of the rule: ordinary ground is neither air nor cover, so it
	// refuses a plant too, exactly as it refuses everything else that is not one of
	// the two carved-out canopy overwrites.
	chunk := NewChunk(Coord{})
	setTreeBlock(chunk, x, y, z, Stone)
	setTreeBlock(chunk, x, y, z, FlowerRed)
	if got := chunk.At(x, y, z); got != Stone {
		t.Errorf("a flower overwrote block %d, want it refused", got)
	}
}

// **Bush and DesertShrub lose the one thing that used to set them apart from every
// other cover id.** Before #874 a bush was solid, so — like the trunks and canopies
// in [TestATrunkOrCanopyAlwaysWinsOverCoverInEitherOrder] — whichever root reached
// the voxel first kept it, regardless of order. Now that both are [Cover], they are
// ordinary contestants: whichever of two cover ids is written *second* always takes
// the voxel, exactly as two flowers or a flower and a winter bramble already
// resolved. This is order *dependence*, not the independence the old comment on
// `setTreeBlock` claimed for the pair it used as its worked example.
func TestTwoCoverPlantsResolveToWhicheverWasWrittenSecond(t *testing.T) {
	t.Parallel()

	const x, y, z = 4, 5, 6
	for _, order := range [][2]Block{
		{FlowerRed, Bush},
		{Bush, FlowerRed},
		{Bush, WinterBramble},
		{WinterBramble, Bush},
		{Bush, DesertShrub},
		{DesertShrub, Bush},
	} {
		chunk := NewChunk(Coord{})
		setTreeBlock(chunk, x, y, z, order[0])
		setTreeBlock(chunk, x, y, z, order[1])
		if got := chunk.At(x, y, z); got != order[1] {
			t.Errorf("%d then %d: the shared voxel holds %d, want %d, the one written second",
				order[0], order[1], got, order[1])
		}
	}
}

// What the generator grows: one voxel above the grass, in a colour the drift's cell
// chooses, and neither a column top nor a surface the map has a word for.
// generatedColumnTop is what the spawn height and regeneration's safety lift are
// computed from, so a flower counted there would stand a body on a plant.
func TestTheFlowerTheGeneratorGrows(t *testing.T) {
	t.Parallel()

	for _, block := range []Block{FlowerRed, FlowerYellow, FlowerBlue} {
		if got := surfaceKindOf(block); got != SurfaceUnknown {
			t.Errorf("surfaceKindOf(%d) = %v, want SurfaceUnknown", block, got)
		}
	}

	// The colour cell is floored, not truncated: **the negative side is the whole
	// assertion**, because truncation toward zero maps x = -1 and x = 0 to one cell
	// and the drifts either side of the axis would share a colour.
	for _, tc := range []struct {
		x, z         int64
		cellX, cellZ int64
	}{
		{0, 0, 0, 0},
		{flowerPatchScaleBlocks - 1, flowerPatchScaleBlocks - 1, 0, 0},
		{flowerPatchScaleBlocks, flowerPatchScaleBlocks, 1, 1},
		{-1, -1, -1, -1},
		{-flowerPatchScaleBlocks - 1, -flowerPatchScaleBlocks - 1, -2, -2},
	} {
		cellX, cellZ := flowerPatchCell(tc.x, tc.z)
		if cellX != tc.cellX || cellZ != tc.cellZ {
			t.Errorf("flowerPatchCell(%d, %d) = (%d, %d), want (%d, %d)", tc.x, tc.z, cellX, cellZ, tc.cellX, tc.cellZ)
		}
	}

	// One voxel, one block above the grass, and nothing of the column replaced.
	x, z, col, h := findFlower(t)
	var voxels [][3]int64
	visitFlower(climateSeed, x, z, col.surface, h, func(vx, vy, vz int64, block Block) {
		if !Cover(block) {
			t.Errorf("the flower yielded block %d, which is not Cover", block)
		}
		voxels = append(voxels, [3]int64{vx, vy, vz})
	})
	if want := [][3]int64{{x, int64(col.surface + 1), z}}; !slices.Equal(voxels, want) {
		t.Errorf("flower voxels = %v, want %v: one block above the surface", voxels, want)
	}
	if got := col.blockAt(col.surface); got != Grass {
		t.Errorf("the column under the flower holds block %d at its surface, want Grass", got)
	}

	if got := generatedColumnTop(climateSeed, x, z); got != col.surface {
		t.Errorf("generatedColumnTop at the flower column (%d, %d) = %d, want the surface %d", x, z, got, col.surface)
	}
	if surface, kind := SurfaceAt(climateSeed, x, z); surface != col.surface || kind != SurfaceGrass {
		t.Errorf("SurfaceAt(%d, %d) = (%d, %v), want (%d, SurfaceGrass)", x, z, surface, kind, col.surface)
	}

	// And the flower really is in the chunk, or the assertions are about nothing.
	coord := ChunkOf(x, int64(col.surface+1), z)
	originX, originY, originZ := coord.Origin()
	if got := Generate(climateSeed, coord).At(int(x-originX), int(int64(col.surface+1)-originY), int(z-originZ)); !Cover(got) {
		t.Fatalf("the column (%d, %d) holds block %d above its surface, want a flower", x, z, got)
	}
}

// generatedColumnTop skips a bush exactly as it skips a flower, now that #874 makes
// [Cover] true of it: a column whose only feature above the ground is a bush reports
// the ground as its top. Before #874 a bush was [Solid], so this same column would
// have reported the bush's own voxel as the top instead — the spawn-placement
// consequence the issue calls out by name, pinned here rather than left to arrive
// silently.
func TestGeneratedColumnTopSkipsABushExactlyAsItSkipsAFlower(t *testing.T) {
	t.Parallel()

	x, z, col := findBush(t)
	if got := generatedColumnTop(climateSeed, x, z); got != col.surface {
		t.Errorf("generatedColumnTop at the bush column (%d, %d) = %d, want the surface %d", x, z, got, col.surface)
	}

	// And the bush really is in the chunk one voxel above that surface, or the
	// assertion above is about nothing.
	coord := ChunkOf(x, int64(col.surface+1), z)
	originX, originY, originZ := coord.Origin()
	if got := Generate(climateSeed, coord).At(int(x-originX), int(int64(col.surface+1)-originY), int(z-originZ)); got != Bush {
		t.Fatalf("the column (%d, %d) holds block %d above its surface, want Bush", x, z, got)
	}
}

// findBush returns the first column of a fixed plains lattice that roots a bush.
// Searched rather than hardcoded, for the reason findFlower is: a retuned density
// moves the test instead of breaking it, and it fatals rather than skipping, because
// a test that passes over an empty sample is the failure this file exists to prevent.
func findBush(t *testing.T) (x, z int64, col column) {
	t.Helper()

	const side = 512
	for x := int64(0); x < side; x++ {
		for z := int64(2048); z < 2048+side; z++ {
			col := columnAt(climateSeed, x, z)
			species, _, ok := plantAtColumn(climateSeed, x, z, col)
			if ok && species.name == "bush" {
				return x, z, col
			}
		}
	}
	t.Fatal("no bush grows in the fixed plains lattice")
	return 0, 0, column{}
}

// findFlower returns the first column of a fixed plains lattice that grows a flower.
// Searched rather than hardcoded, so a retune of the patch threshold moves the test
// instead of breaking it — and it fatals rather than skipping, because a test that
// passes over an empty sample is the failure this file exists to prevent.
func findFlower(t *testing.T) (x, z int64, col column, h uint64) {
	t.Helper()

	const side = 512
	for x := int64(0); x < side; x++ {
		for z := int64(2048); z < 2048+side; z++ {
			col := columnAt(climateSeed, x, z)
			species, h, ok := plantAtColumn(climateSeed, x, z, col)
			if ok && species.name == "flower" {
				return x, z, col, h
			}
		}
	}
	t.Fatal("no flower grows in the fixed plains lattice")
	return 0, 0, column{}, 0
}
