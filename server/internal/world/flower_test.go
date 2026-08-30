package world

import (
	"slices"
	"testing"
)

// **A plant wins over a flower whichever order the two roots are visited in**, which
// makes a chunk a pure function of its coordinate rather than of placeTrees' loop
// bounds. Stated against setTreeBlock, whose condition is the whole rule.
func TestAPlantOverwritesAFlowerInEitherOrder(t *testing.T) {
	t.Parallel()

	const x, y, z = 4, 5, 6
	for _, tc := range []struct {
		name  string
		first Block
		then  Block
	}{
		{"flower first", FlowerRed, Bush},
		{"bush first", Bush, FlowerRed},
	} {
		chunk := NewChunk(Coord{})
		setTreeBlock(chunk, x, y, z, tc.first)
		setTreeBlock(chunk, x, y, z, tc.then)
		if got := chunk.At(x, y, z); got != Bush {
			t.Errorf("%s: the shared voxel holds block %d, want Bush", tc.name, got)
		}
	}

	// Cover is what a plant may write *over*, never what a flower may write over.
	chunk := NewChunk(Coord{})
	setTreeBlock(chunk, x, y, z, Stone)
	setTreeBlock(chunk, x, y, z, FlowerRed)
	if got := chunk.At(x, y, z); got != Stone {
		t.Errorf("a flower overwrote block %d, want it refused", got)
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
