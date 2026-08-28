package world

import "testing"

// Block ids cross the wire as uint16 values. Appending is compatible; reordering
// silently changes every saved edit and every colour the client draws.
func TestBlockIDsStayAppendOnly(t *testing.T) {
	t.Parallel()

	want := []Block{Air, Stone, Dirt, Grass, Snow, Log, Leaves, CoalOre, IronOre}
	for id, block := range want {
		if block != Block(id) {
			t.Errorf("palette entry %d has id %d", id, block)
		}
	}
}

// The palette's two answers about water, which every consumer outside this package
// reads instead of comparing ids of its own.
//
// **Neither is `!` of the other, and that is the whole reason both exist.** Air is
// not solid and is not a fluid either, so a swim rule written against `!Solid` would
// have a body treading water in mid air; ice is solid and is not a fluid, because it
// is the lid you walk out onto.
func TestWaterIsPassableAndIceIsNot(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		block                   Block
		solid, fluid, placeable bool
	}{
		{Air, false, false, false},
		{Water, false, true, false},
		{Ice, true, false, true},
		{Stone, true, false, true},
		{Gravel, true, false, true},
		{CoalOre, true, false, false},
	} {
		if got := Solid(tc.block); got != tc.solid {
			t.Errorf("Solid(%d) = %t, want %t", tc.block, got, tc.solid)
		}
		if got := Fluid(tc.block); got != tc.fluid {
			t.Errorf("Fluid(%d) = %t, want %t", tc.block, got, tc.fluid)
		}
		if got := Placeable(tc.block); got != tc.placeable {
			t.Errorf("Placeable(%d) = %t, want %t", tc.block, got, tc.placeable)
		}
	}

	// The two ids are appended, which is the whole of the compatibility rule: every id
	// below them is already inside chunks a client holds and inside the delta files a
	// played-in world directory holds.
	if Water != 12 || Ice != 13 {
		t.Errorf("Water = %d and Ice = %d, want the appended wire ids 12 and 13", Water, Ice)
	}
}

// The index order is wire contract: schemas/world.fbs documents it and the
// client's mesher indexes in it, so these four values are part of the protocol.
func TestIndexOrderIsXFastestThenZThenY(t *testing.T) {
	t.Parallel()

	cases := []struct {
		x, y, z int
		want    int
	}{
		{0, 0, 0, 0},
		{1, 0, 0, 1},
		{0, 0, 1, ChunkSize},
		{0, 1, 0, ChunkSize * ChunkSize},
		{ChunkSize - 1, ChunkSize - 1, ChunkSize - 1, ChunkVolume - 1},
	}

	for _, tc := range cases {
		if got := Index(tc.x, tc.y, tc.z); got != tc.want {
			t.Errorf("Index(%d, %d, %d) = %d, want %d", tc.x, tc.y, tc.z, got, tc.want)
		}
	}
}

// Every voxel must have exactly one index, and every index exactly one voxel. An
// off-by-one in Index would otherwise show up as terrain that looks fine but has
// two voxels sharing a slot.
func TestIndexIsABijection(t *testing.T) {
	t.Parallel()

	seen := make([]bool, ChunkVolume)
	for y := range ChunkSize {
		for z := range ChunkSize {
			for x := range ChunkSize {
				i := Index(x, y, z)
				if i < 0 || i >= ChunkVolume {
					t.Fatalf("Index(%d, %d, %d) = %d is out of range", x, y, z, i)
				}
				if seen[i] {
					t.Fatalf("index %d is produced by more than one voxel", i)
				}
				seen[i] = true
			}
		}
	}
}

func TestChunkReadsBackWhatItWrote(t *testing.T) {
	t.Parallel()

	c := NewChunk(Coord{X: 1, Y: -2, Z: 3})
	if len(c.Blocks) != ChunkVolume {
		t.Fatalf("a new chunk holds %d blocks, want %d", len(c.Blocks), ChunkVolume)
	}
	if c.At(5, 6, 7) != Air {
		t.Error("a new chunk is not air")
	}

	c.Set(5, 6, 7, Snow)
	if got := c.At(5, 6, 7); got != Snow {
		t.Errorf("At(5, 6, 7) = %d, want Snow", got)
	}
	if got := c.At(6, 6, 7); got != Air {
		t.Errorf("writing one voxel changed its neighbour: got %d", got)
	}
}

func TestCoordOrigin(t *testing.T) {
	t.Parallel()

	x, y, z := Coord{X: 2, Y: -1, Z: 0}.Origin()
	if x != 64 || y != -32 || z != 0 {
		t.Errorf("Origin() = (%d, %d, %d), want (64, -32, 0)", x, y, z)
	}
}

// Floor division, not truncation. Truncating toward zero would make one 63-block
// chunk straddling the origin — a bug that only shows up as a seam once somebody
// walks west of spawn.
func TestContainingChunkFloorsTowardNegativeInfinity(t *testing.T) {
	t.Parallel()

	cases := []struct {
		x, y, z float32
		want    Coord
	}{
		{0, 0, 0, Coord{0, 0, 0}},
		{31.9, 5, 0.5, Coord{0, 0, 0}},
		{32, 0, 0, Coord{1, 0, 0}},
		{-0.1, 0, 0, Coord{-1, 0, 0}},
		{-1, 0, 0, Coord{-1, 0, 0}},
		{-32, 0, 0, Coord{-1, 0, 0}},
		{-32.1, 0, 0, Coord{-2, 0, 0}},
		{-33, 0, 0, Coord{-2, 0, 0}},
		{0, 80, 0, Coord{0, 2, 0}},
		{0, -0.5, -64, Coord{0, -1, -2}},
	}

	for _, tc := range cases {
		if got := ContainingChunk(tc.x, tc.y, tc.z); got != tc.want {
			t.Errorf("ContainingChunk(%v, %v, %v) = %+v, want %+v", tc.x, tc.y, tc.z, got, tc.want)
		}
	}
}

// ChunkOf and Local are what collision addresses voxels through, so they have to
// agree with the float path and with each other. The interesting half is negative
// coordinates: Go's % keeps the sign of the dividend, so a naive Local puts world
// x = -1 at local -1 and panics on the index.
func TestChunkOfAndLocalTileTheWorldWithoutGapsOrOverlaps(t *testing.T) {
	t.Parallel()

	// Two chunks either side of the origin, one voxel at a time: every world block
	// must land in exactly one (chunk, local) slot, and the slot must reproduce the
	// world coordinate.
	seen := make(map[[2]int64]bool)
	for v := int64(-2 * ChunkSize); v < 2*ChunkSize; v++ {
		chunk := ChunkOf(v, v, v)
		local := Local(v)

		if local < 0 || local >= ChunkSize {
			t.Fatalf("Local(%d) = %d, which is not a voxel of a chunk", v, local)
		}
		originX, _, _ := chunk.Origin()
		if got := originX + int64(local); got != v {
			t.Fatalf("block %d is chunk %d local %d, which is block %d", v, chunk.X, local, got)
		}
		key := [2]int64{int64(chunk.X), int64(local)}
		if seen[key] {
			t.Fatalf("chunk %d local %d holds more than one world block", chunk.X, local)
		}
		seen[key] = true
	}
}

// The float and integer paths address the same chunks, or collision would read a
// different chunk than streaming sent.
func TestChunkOfAgreesWithContainingChunk(t *testing.T) {
	t.Parallel()

	for _, v := range []int64{-65, -64, -33, -32, -31, -1, 0, 1, 31, 32, 33, 96} {
		want := ContainingChunk(float32(v), float32(v), float32(v))
		if got := ChunkOf(v, v, v); got != want {
			t.Errorf("ChunkOf(%d) = %+v, ContainingChunk says %+v", v, got, want)
		}
	}
}
