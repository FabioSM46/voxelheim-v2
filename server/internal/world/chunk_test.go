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

// The three materials a settlement is built out of, against the same three predicates
// water and ice are checked against above.
//
// **The point of the test is that none of the three needed a line of code.** [Solid] is
// stated as "not air, not water" rather than as a list of the ids that stop a body, so
// an appended block is solid the moment it exists — and that is the answer a settlement
// depends on: part 2 writes these ids into chunks, and a wall a player walks through is
// not a wall. A list would have needed extending here and would have failed open until
// somebody remembered; this shape fails closed, and this test is what says so out loud.
// [Placeable] is the opposite shape on purpose — an allowlist, because an unknown id
// must never be stored — so it *is* the one the three had to be added to.
func TestTheSettlementBlocksAreOrdinaryGround(t *testing.T) {
	t.Parallel()

	for _, block := range []Block{Planks, Cobblestone, Thatch} {
		if !Solid(block) {
			t.Errorf("Solid(%d) = false, want true: a wall a body walks through is not a wall", block)
		}
		if Fluid(block) {
			t.Errorf("Fluid(%d) = true, want false", block)
		}
		if !Placeable(block) {
			t.Errorf("Placeable(%d) = false, want true: a player takes a settlement apart and builds with it", block)
		}
	}

	// Appended, which is the whole of the compatibility rule: every id below these is
	// already inside chunks a client holds and inside the delta files a played-in world
	// directory keeps.
	if Planks != 14 || Cobblestone != 15 || Thatch != 16 {
		t.Errorf("Planks = %d, Cobblestone = %d, Thatch = %d, want the appended wire ids 14, 15 and 16",
			Planks, Cobblestone, Thatch)
	}
}

func TestTheDesertPlantBlocksCarryAppendedIDsAndFailClosedPlacement(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		block     Block
		id        Block
		placeable bool
	}{
		{PalmLog, 17, true},
		{PalmFronds, 18, true},
		{DesertShrub, 19, false},
	} {
		if tc.block != tc.id {
			t.Errorf("desert plant block id = %d, want appended id %d", tc.block, tc.id)
		}
		if !Solid(tc.block) {
			t.Errorf("Solid(%d) = false, want true", tc.block)
		}
		if Fluid(tc.block) {
			t.Errorf("Fluid(%d) = true, want false", tc.block)
		}
		if got := Placeable(tc.block); got != tc.placeable {
			t.Errorf("Placeable(%d) = %t, want %t", tc.block, got, tc.placeable)
		}
	}
}

func TestThePlainsPlantBlocksCarryAppendedIDsAndFailClosedPlacement(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		block     Block
		id        Block
		placeable bool
	}{
		{BroadLeaves, 20, true},
		{Bush, 21, false},
	} {
		if tc.block != tc.id {
			t.Errorf("plains plant block id = %d, want appended id %d", tc.block, tc.id)
		}
		if !Solid(tc.block) {
			t.Errorf("Solid(%d) = false, want true", tc.block)
		}
		if Fluid(tc.block) {
			t.Errorf("Fluid(%d) = true, want false", tc.block)
		}
		if got := Placeable(tc.block); got != tc.placeable {
			t.Errorf("Placeable(%d) = %t, want %t", tc.block, got, tc.placeable)
		}
	}
}

func TestTheWaterFamilyCarriesAppendedIDsAndOneExhaustiveClassification(t *testing.T) {
	t.Parallel()

	type waterFacts struct {
		level  int
		dx, dz int
	}
	wantWater := map[Block]waterFacts{
		Water:            {level: 8},
		WaterFlow1:       {level: 1},
		WaterFlow2:       {level: 2},
		WaterFlow3:       {level: 3},
		WaterFlow4:       {level: 4},
		WaterFlow5:       {level: 5},
		WaterFlow6:       {level: 6},
		WaterFlow7:       {level: 7},
		WaterCurrentXPos: {level: 8, dx: 1},
		WaterCurrentXNeg: {level: 8, dx: -1},
		WaterCurrentZPos: {level: 8, dz: 1},
		WaterCurrentZNeg: {level: 8, dz: -1},
	}

	for block := Air; block <= WaterCurrentZNeg; block++ {
		facts, water := wantWater[block]
		if got := IsWater(block); got != water {
			t.Errorf("IsWater(%d) = %t, want %t", block, got, water)
		}
		if got := Fluid(block); got != water {
			t.Errorf("Fluid(%d) = %t, want %t", block, got, water)
		}
		if got := WaterLevel(block); got != facts.level {
			t.Errorf("WaterLevel(%d) = %d, want %d", block, got, facts.level)
		}
		dx, dz := CurrentOf(block)
		if dx != facts.dx || dz != facts.dz {
			t.Errorf("CurrentOf(%d) = (%d, %d), want (%d, %d)", block, dx, dz, facts.dx, facts.dz)
		}
		if water {
			if Solid(block) {
				t.Errorf("Solid(%d) = true, want false for water", block)
			}
			if Placeable(block) {
				t.Errorf("Placeable(%d) = true, want false for water", block)
			}
		}
	}

	appended := []Block{
		WaterFlow1, WaterFlow2, WaterFlow3, WaterFlow4, WaterFlow5, WaterFlow6, WaterFlow7,
		WaterCurrentXPos, WaterCurrentXNeg, WaterCurrentZPos, WaterCurrentZNeg,
	}
	for i, block := range appended {
		if want := Block(22 + i); block != want {
			t.Errorf("water family member %d has id %d, want appended id %d", i, block, want)
		}
	}
	unknown := Block(65535)
	if IsWater(unknown) || Fluid(unknown) || WaterLevel(unknown) != 0 {
		t.Errorf("unknown block %d was classified as water", unknown)
	}
	if dx, dz := CurrentOf(unknown); dx != 0 || dz != 0 {
		t.Errorf("CurrentOf(unknown) = (%d, %d), want (0, 0)", dx, dz)
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
