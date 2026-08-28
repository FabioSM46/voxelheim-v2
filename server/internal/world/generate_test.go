package world

import (
	"bytes"
	"encoding/binary"
	"flag"
	"math"
	"os"
	"path/filepath"
	"slices"
	"sync"
	"testing"
)

var updateGolden = flag.Bool("update-golden", false, "rewrite the golden chunk fixture")

const (
	goldenSeed = 0x5EED
	goldenPath = "testdata/chunk_golden.bin"
)

var goldenCoord = Coord{X: 3, Y: 2, Z: -5}

// TestGenerateMatchesTheGoldenChunk is the determinism contract, and the reason
// the generator uses fixed-point integers rather than float64.
//
// The GDD's weekly Fimbulvetr storm regenerates every unprotected chunk to its
// original procedural state, which means this exact byte sequence has to come back
// out of a build made months from now on a different machine. Go permits an
// implementation to fuse floating-point operations, so a float generator could
// pass this test today and fail it after a compiler upgrade; integer arithmetic
// cannot.
//
// Regenerate deliberately, never casually:
//
//	go test ./internal/world -run TestGenerateMatchesTheGoldenChunk -update-golden
//
// A diff here means the world changed. That is either a bug or a decision that
// abandons every existing save.
func TestGenerateMatchesTheGoldenChunk(t *testing.T) {
	got := encodedBytes(Encode(Generate(goldenSeed, goldenCoord)))

	if *updateGolden {
		if err := os.MkdirAll(filepath.Dir(goldenPath), 0o755); err != nil {
			t.Fatalf("create testdata: %v", err)
		}
		if err := os.WriteFile(goldenPath, got, 0o644); err != nil {
			t.Fatalf("write golden: %v", err)
		}
		t.Logf("golden fixture rewritten: %d bytes", len(got))
		return
	}

	want, err := os.ReadFile(goldenPath)
	if err != nil {
		t.Fatalf("read golden (regenerate with -update-golden if this is a new fixture): %v", err)
	}
	if !bytes.Equal(got, want) {
		t.Fatalf("chunk %+v for seed %#x no longer matches the golden fixture: %d bytes now, %d before",
			goldenCoord, goldenSeed, len(got), len(want))
	}
}

func TestWorldgenVersionRecordsTheFeatureBreak(t *testing.T) {
	t.Parallel()

	if WorldgenVersion != 4 {
		t.Fatalf("WorldgenVersion = %d, want 4 for caves", WorldgenVersion)
	}
}

func encodedBytes(pairs []uint16) []byte {
	out := make([]byte, 0, len(pairs)*2)
	for _, v := range pairs {
		out = binary.BigEndian.AppendUint16(out, v)
	}
	return out
}

// Generation is a pure function, so concurrent generation of the same chunk must
// produce identical blocks. Run under -race, this also proves the generator holds
// no shared state.
func TestGenerateIsDeterministicUnderConcurrency(t *testing.T) {
	t.Parallel()

	const goroutines = 8
	results := make([][]Block, goroutines)

	var wg sync.WaitGroup
	for i := range goroutines {
		wg.Add(1)
		go func() {
			defer wg.Done()
			results[i] = Generate(goldenSeed, goldenCoord).Blocks
		}()
	}
	wg.Wait()

	for i := 1; i < goroutines; i++ {
		if !slices.Equal(results[0], results[i]) {
			t.Fatalf("goroutine %d generated different blocks", i)
		}
	}
}

// A seam at a chunk border is the classic generator bug: it comes from mapping
// local coordinates to world coordinates differently on the two sides. Plain
// voxels still match the height field exactly; feature voxels are classified and
// checked against their own stricter placement invariants.
func TestChunkBordersAgreeWithTheHeightField(t *testing.T) {
	t.Parallel()

	const seed = 99
	const chunkY = 2

	left := Generate(seed, Coord{X: 0, Y: chunkY, Z: 0})
	right := Generate(seed, Coord{X: 1, Y: chunkY, Z: 0})

	plainColumns := 0
	for z := range ChunkSize {
		// left's local x=31 is world x=31; right's local x=0 is world x=32. They are
		// adjacent columns, so each must match the height field at its own world x.
		if !assertColumn(t, left, ChunkSize-1, z, seed) {
			plainColumns++
		}
		if !assertColumn(t, right, 0, z, seed) {
			plainColumns++
		}
	}
	if plainColumns == 0 {
		t.Fatal("the border sample contained no plain column to compare exactly with the height field")
	}
}

// The same column split across two vertically stacked chunks must be consistent:
// solid below the surface in the lower chunk, air above it in the upper one.
func TestVerticallyStackedChunksAgree(t *testing.T) {
	t.Parallel()

	const seed = 7
	lower := Generate(seed, Coord{X: 0, Y: 1, Z: 0})
	upper := Generate(seed, Coord{X: 0, Y: 2, Z: 0})

	for z := range ChunkSize {
		for x := range ChunkSize {
			assertColumn(t, lower, x, z, seed)
			assertColumn(t, upper, x, z, seed)
		}
	}
}

// assertColumn returns whether the column contains a feature. It never turns a
// mismatch into "feature-shaped enough": every non-feature voxel must equal the
// old terrain function, while each feature has a separate placement proof.
//
// **Carving is a feature that removes rather than adds, so it is the one the
// default branch has to know about.** Air where the terrain function says stone is
// either a cave or the bug this function exists to catch, and only caveAt can tell
// the two apart.
func assertColumn(t *testing.T, c *Chunk, x, z int, seed int64) bool {
	t.Helper()

	originX, originY, originZ := c.Coord.Origin()
	worldX, worldZ := originX+int64(x), originZ+int64(z)
	col := columnAt(seed, worldX, worldZ)
	surface := col.surface
	featured := false

	for y := range ChunkSize {
		worldY := originY + int64(y)
		got, terrain := c.At(x, y, z), col.blockAt(int(worldY))
		carved := caveAt(seed, worldX, worldY, worldZ, surface)
		if carved && got != Air && got != Log && got != Leaves {
			t.Fatalf("carved voxel (%d, %d, %d) is block %d rather than air", worldX, worldY, worldZ, got)
		}
		switch got {
		case CoalOre:
			featured = true
			depth := int64(surface) - worldY
			if terrain != Stone || depth < coalMinDepth || depth > coalMaxDepth {
				t.Fatalf("coal at (%d, %d, %d) replaced %d at depth %d", worldX, worldY, worldZ, terrain, depth)
			}
		case IronOre:
			featured = true
			depth := int64(surface) - worldY
			if terrain != Stone || depth < ironMinDepth || depth > ironMaxDepth {
				t.Fatalf("iron at (%d, %d, %d) replaced %d at depth %d", worldX, worldY, worldZ, terrain, depth)
			}
		case Log:
			featured = true
			rootSurface, trunkHeight, ok := treeAt(seed, worldX, worldZ)
			if terrain != Air || !ok || worldY <= int64(rootSurface) || worldY > int64(rootSurface+trunkHeight) {
				t.Fatalf("log at (%d, %d, %d) is not on a tree rooted in grass", worldX, worldY, worldZ)
			}
		case Leaves:
			featured = true
			// A canopy fills whatever was air when placeTrees ran, and a cave mouth in
			// a column the tree overhangs is air by then. Both are honest fills.
			if (terrain != Air && !carved) || !treeCanPlace(seed, worldX, worldY, worldZ, Leaves) {
				t.Fatalf("leaves at (%d, %d, %d) are not part of a deterministic canopy", worldX, worldY, worldZ)
			}
		case Air:
			if terrain != Air {
				featured = true
				if !carved {
					t.Fatalf("plain chunk %+v voxel (%d, %d, %d) [world y=%d, surface=%d] is air, want %d",
						c.Coord, x, y, z, worldY, surface, terrain)
				}
			}
		default:
			if got != terrain {
				t.Fatalf("plain chunk %+v voxel (%d, %d, %d) [world y=%d, surface=%d] is %d, want %d",
					c.Coord, x, y, z, worldY, surface, got, terrain)
			}
		}
	}
	return featured
}

func treeCanPlace(seed, worldX, worldY, worldZ int64, want Block) bool {
	for rootZ := worldZ - treeCanopyRadius; rootZ <= worldZ+treeCanopyRadius; rootZ++ {
		for rootX := worldX - treeCanopyRadius; rootX <= worldX+treeCanopyRadius; rootX++ {
			found := false
			visitTree(seed, rootX, rootZ, func(x, y, z int64, block Block) {
				found = found || (x == worldX && y == worldY && z == worldZ && block == want)
			})
			if found {
				return true
			}
		}
	}
	return false
}

func TestTerrainHasTheExpectedShape(t *testing.T) {
	t.Parallel()

	const seed = 31337

	minHeight, maxHeight := 1<<30, -(1 << 30)
	for x := int64(-200); x < 200; x++ {
		h := HeightAt(seed, x, x*3)
		minHeight = min(minHeight, h)
		maxHeight = max(maxHeight, h)
	}

	lowest := baseHeight - mountainAmplitude/2
	highest := baseHeight + mountainAmplitude/2
	if minHeight < lowest || maxHeight > highest {
		t.Errorf("heights ranged over [%d, %d], outside the designed [%d, %d]", minHeight, maxHeight, lowest, highest)
	}
	if maxHeight-minHeight < plainsAmplitude {
		t.Errorf("heights only ranged over %d blocks; the terrain is nearly flat", maxHeight-minHeight)
	}

	// HeightAt remains the coastline, not "whatever is highest now". Name both
	// kinds of column explicitly so a later change cannot silently fold trees into
	// the terrain shape and move every consumer of the height field.
	plainColumns, treeColumns := 0, 0
	for z := int64(-32); z < 32; z++ {
		for x := int64(-32); x < 32; x++ {
			if _, _, ok := treeAt(seed, x, z); ok {
				treeColumns++
			} else {
				plainColumns++
			}
		}
	}
	if plainColumns == 0 || treeColumns == 0 {
		t.Fatalf("terrain sample had %d plain and %d tree-root columns; both shapes must be exercised", plainColumns, treeColumns)
	}
}

// The generated surface voxel is the one its column's climate and altitude ask
// for — for plain columns and for a column with a tree standing on it.
//
// **This replaced a test named after the snow line, and the rename is the
// feature.** The old rule was "grass, or snow at or above 78", and the old terrain
// topped out at 84, so the surface of a column was decided by the seed. There are
// now four climate columns and two altitude overrides above them, and what a
// column's top block is has to be read from the same function the generator used
// rather than restated as a two-branch rule here.
func TestSurfaceBlocksFollowTheirClimateAndAltitude(t *testing.T) {
	t.Parallel()

	const seed = 4242
	chunk := Generate(seed, Coord{X: 0, Y: 2, Z: 0})

	checked, plainColumns, featureColumns := 0, 0, 0
	surfaces := map[Block]int{}
	for z := range ChunkSize {
		for x := range ChunkSize {
			col := columnAt(seed, int64(x), int64(z))
			localY := col.surface - 2*ChunkSize
			if localY < 0 || localY >= ChunkSize {
				continue // this column's surface is in another chunk
			}
			if caveAt(seed, int64(x), int64(col.surface), int64(z), col.surface) {
				continue // a cave mouth has opened this column's surface; caves_test.go owns that rule
			}
			checked++

			want := col.blockAt(col.surface)
			if got := chunk.At(x, localY, z); got != want {
				kind := "plain"
				if generatedColumnTop(seed, int64(x), int64(z)) > col.surface {
					kind = "feature-bearing"
				}
				t.Fatalf("%s %v surface at (%d, %d) is height %d and block %d, want %d",
					kind, col.climate, x, z, col.surface, got, want)
			}
			surfaces[want]++
			if generatedColumnTop(seed, int64(x), int64(z)) > col.surface {
				featureColumns++
			} else {
				plainColumns++
			}
		}
	}
	if checked == 0 {
		t.Skip("no column's surface falls in this chunk")
	}

	// The altitude overrides are the only way a surface block can disagree with its
	// climate, so no surface voxel may be a block no rule can produce.
	for block := range surfaces {
		switch block {
		case Grass, Snow, Sand, Gravel, Stone:
		default:
			t.Errorf("a surface voxel is block %d, which no climate column produces", block)
		}
	}

	// The original chunk happens to be plain at this tree density. Add one known
	// feature-bearing column rather than weakening the all-column sweep above.
	treeSeed, rootZ, treeSurface := findIsolatedEastBorderTree(t)
	treeCoord := ChunkOf(ChunkSize-1, int64(treeSurface), rootZ)
	treeChunk := Generate(treeSeed, treeCoord)
	if got := treeChunk.At(Local(ChunkSize-1), Local(int64(treeSurface)), Local(rootZ)); got != Grass {
		t.Fatalf("tree-bearing surface at (%d, %d) is block %d, want Grass", ChunkSize-1, rootZ, got)
	}
	if generatedColumnTop(treeSeed, ChunkSize-1, rootZ) <= treeSurface {
		t.Fatal("the selected feature-bearing column has no tree above its grass surface")
	}
	featureColumns++

	if plainColumns == 0 || featureColumns == 0 {
		t.Fatalf("surface sample had %d plain and %d feature-bearing columns; both need their surface assertion", plainColumns, featureColumns)
	}
}

func TestOreAppearsOnlyInStoneAndInsideItsDepthBand(t *testing.T) {
	t.Parallel()

	coal, iron := 0, 0
	vein := map[Block]bool{CoalOre: false, IronOre: false}
	directions := [][3]int{{1, 0, 0}, {-1, 0, 0}, {0, 1, 0}, {0, -1, 0}, {0, 0, 1}, {0, 0, -1}}
	for seed := int64(1); seed <= 8; seed++ {
		for chunkZ := int32(-1); chunkZ <= 0; chunkZ++ {
			for chunkX := int32(-1); chunkX <= 0; chunkX++ {
				for chunkY := int32(0); chunkY <= 2; chunkY++ {
					chunk := Generate(seed, Coord{X: chunkX, Y: chunkY, Z: chunkZ})
					originX, originY, originZ := chunk.Coord.Origin()
					for z := range ChunkSize {
						for x := range ChunkSize {
							worldX, worldZ := originX+int64(x), originZ+int64(z)
							col := columnAt(seed, worldX, worldZ)
							surface := col.surface
							for y := range ChunkSize {
								worldY := originY + int64(y)
								got := chunk.At(x, y, z)
								if got != CoalOre && got != IronOre {
									continue
								}
								for _, direction := range directions {
									nx, ny, nz := x+direction[0], y+direction[1], z+direction[2]
									if nx >= 0 && nx < ChunkSize && ny >= 0 && ny < ChunkSize && nz >= 0 && nz < ChunkSize &&
										chunk.At(nx, ny, nz) == got {
										vein[got] = true
									}
								}

								depth := int64(surface) - worldY
								if terrain := col.blockAt(int(worldY)); terrain != Stone {
									t.Fatalf("ore %d at (%d, %d, %d) replaced terrain block %d", got, worldX, worldY, worldZ, terrain)
								}
								switch got {
								case CoalOre:
									coal++
									if depth < coalMinDepth || depth > coalMaxDepth {
										t.Fatalf("coal at depth %d, outside [%d, %d]", depth, coalMinDepth, coalMaxDepth)
									}
								case IronOre:
									iron++
									if depth < ironMinDepth || depth > ironMaxDepth {
										t.Fatalf("iron at depth %d, outside [%d, %d]", depth, ironMinDepth, ironMaxDepth)
									}
									if worldY > int64(surface-coalMaxDepth) {
										t.Fatalf("iron at y=%d is above the coal band's floor y=%d", worldY, surface-coalMaxDepth)
									}
								}
							}
						}
					}
				}
			}
		}
	}
	if coal == 0 || iron == 0 {
		t.Fatalf("sample found %d coal and %d iron voxels; both veins must exist", coal, iron)
	}
	if !vein[CoalOre] || !vein[IronOre] {
		t.Fatalf("connected neighbours found: coal=%t iron=%t", vein[CoalOre], vein[IronOre])
	}
}

func TestTreesGrowOnlyFromGrass(t *testing.T) {
	t.Parallel()

	logs, leaves := 0, 0
	for seed := int64(1); seed <= 8; seed++ {
		for chunkZ := int32(-1); chunkZ <= 0; chunkZ++ {
			for chunkX := int32(-1); chunkX <= 0; chunkX++ {
				for chunkY := int32(1); chunkY <= 2; chunkY++ {
					chunk := Generate(seed, Coord{X: chunkX, Y: chunkY, Z: chunkZ})
					originX, originY, originZ := chunk.Coord.Origin()
					for z := range ChunkSize {
						for x := range ChunkSize {
							worldX, worldZ := originX+int64(x), originZ+int64(z)
							for y := range ChunkSize {
								got := chunk.At(x, y, z)
								switch got {
								case Log:
									logs++
									worldY := originY + int64(y)
									surface, trunkHeight, ok := treeAt(seed, worldX, worldZ)
									if !ok || columnAt(seed, worldX, worldZ).blockAt(surface) != Grass {
										t.Fatalf("log at (%d, %d, %d) has no grass root", worldX, worldY, worldZ)
									}
									if worldY <= int64(surface) || worldY > int64(surface+trunkHeight) {
										t.Fatalf("log at y=%d lies outside its trunk [%d, %d]", worldY, surface+1, surface+trunkHeight)
									}
								case Leaves:
									leaves++
								}
							}
						}
					}
				}
			}
		}
	}
	if logs == 0 || leaves == 0 {
		t.Fatalf("sample found %d logs and %d leaves; complete trees must exist", logs, leaves)
	}
}

func TestATreeCrossingAChunkBorderIsCompleteInEitherGenerationOrder(t *testing.T) {
	t.Parallel()

	seed, rootZ, surface := findIsolatedEastBorderTree(t)
	chunkY := int32(floorDiv(int64(surface+1), ChunkSize))
	leftCoord := Coord{X: 0, Y: chunkY, Z: 0}
	rightCoord := Coord{X: 1, Y: chunkY, Z: 0}

	leftThenRight := [2]*Chunk{Generate(seed, leftCoord), Generate(seed, rightCoord)}
	rightFirst := Generate(seed, rightCoord)
	rightThenLeft := [2]*Chunk{Generate(seed, leftCoord), rightFirst}
	if !slices.Equal(leftThenRight[0].Blocks, rightThenLeft[0].Blocks) ||
		!slices.Equal(leftThenRight[1].Blocks, rightThenLeft[1].Blocks) {
		t.Fatal("chunk bytes changed with generation order")
	}

	want := make(map[[3]int64]Block)
	visitTree(seed, ChunkSize-1, rootZ, func(x, y, z int64, block Block) {
		want[[3]int64{x, y, z}] = block
	})

	features := [2]int{}
	for pos, block := range want {
		worldX, worldY, worldZ := pos[0], pos[1], pos[2]
		if columnAt(seed, worldX, worldZ).blockAt(int(worldY)) != Air {
			continue // foliage clipped honestly by a neighbouring slope
		}
		var chunk *Chunk
		switch floorDiv(worldX, ChunkSize) {
		case 0:
			chunk = leftThenRight[0]
		case 1:
			chunk = leftThenRight[1]
		default:
			continue
		}
		if got := chunk.At(Local(worldX), Local(worldY), Local(worldZ)); got != block {
			t.Fatalf("border tree voxel (%d, %d, %d) is %d, want %d", worldX, worldY, worldZ, got, block)
		}
		features[floorDiv(worldX, ChunkSize)]++
	}
	if features[0] == 0 || features[1] == 0 {
		t.Fatalf("border tree contributed %d voxels to the left chunk and %d to the right", features[0], features[1])
	}
}

func findIsolatedEastBorderTree(t *testing.T) (seed, rootZ int64, surface int) {
	t.Helper()

	const rootX = ChunkSize - 1
	for seed = 1; seed <= 2000; seed++ {
		for rootZ = 4; rootZ < ChunkSize-4; rootZ++ {
			candidateSurface, trunkHeight, ok := treeAt(seed, rootX, rootZ)
			if !ok {
				continue
			}
			bottom := ChunkOf(rootX, int64(candidateSurface+1), rootZ)
			top := ChunkOf(rootX, int64(candidateSurface+trunkHeight+treeCanopyAboveCrown), rootZ)
			if bottom.Y != top.Y {
				continue
			}

			isolated := true
			for nearbyZ := rootZ - 2*treeCanopyRadius; nearbyZ <= rootZ+2*treeCanopyRadius && isolated; nearbyZ++ {
				for nearbyX := int64(rootX - 2*treeCanopyRadius); nearbyX <= rootX+2*treeCanopyRadius; nearbyX++ {
					if nearbyX == rootX && nearbyZ == rootZ {
						continue
					}
					if _, _, other := treeAt(seed, nearbyX, nearbyZ); other {
						isolated = false
						break
					}
				}
			}
			if isolated {
				return seed, rootZ, candidateSurface
			}
		}
	}
	t.Fatal("no isolated tree rooted at the east chunk border in the deterministic search")
	return 0, 0, 0
}

// The invariant is a relationship, not a number: for any seed, the block a session
// starts in is air and the ground is under it. Asserting "y == 80" would pin today's
// terrain; asserting this survives a retune and fails the moment one breaks it, which
// is exactly what the constant it replaces could not do.
func TestSpawnIsAirAboveSolidGroundForEverySeed(t *testing.T) {
	t.Parallel()

	canopySeeds := 0
	for seed := int64(1); seed <= 300; seed++ {
		spawn := SpawnAt(seed)
		surface := HeightAt(seed, spawnColumnX, spawnColumnZ)
		chunks := make(map[Coord]*Chunk)

		if got := int(spawn[1]); got <= surface {
			t.Fatalf("seed %d spawns at y=%d, at or below the surface %d", seed, got, surface)
		}

		// Read the actual voxels rather than trusting the arithmetic: the height field
		// and the generator could disagree, and that disagreement is the bug class here.
		at := func(worldY int) Block {
			coord := ContainingChunk(spawn[0], float32(worldY), spawn[2])
			chunk := chunks[coord]
			if chunk == nil {
				chunk = Generate(seed, coord)
				chunks[coord] = chunk
			}
			ox, oy, oz := coord.Origin()
			return chunk.At(spawnColumnX-int(ox), worldY-int(oy), spawnColumnZ-int(oz))
		}

		// Find the real top from generated voxels, independently of the helper SpawnAt
		// uses. The global terrain maximum plus the tallest tree bounds the search.
		searchTop := baseHeight + mountainAmplitude/2 + treeMinTrunkHeight + treeHeightVariants - 1 + treeCanopyAboveCrown
		actualTop := surface
		for worldY := surface + 1; worldY <= searchTop; worldY++ {
			if at(worldY) != Air {
				actualTop = worldY
			}
		}
		if helperTop := generatedColumnTop(seed, spawnColumnX, spawnColumnZ); helperTop != actualTop {
			t.Fatalf("seed %d computed generated top %d, actual generated top is %d", seed, helperTop, actualTop)
		}
		if actualTop > surface {
			canopySeeds++
		}
		if got, want := int(spawn[1]), actualTop+SpawnClearance; got != want {
			t.Fatalf("seed %d spawns at y=%d, want %d above terrain/canopy top %d", seed, got, want, actualTop)
		}
		if got := at(surface); got == Air {
			t.Fatalf("seed %d has air at its own surface height %d", seed, surface)
		}
		for worldY := actualTop + 1; worldY <= int(spawn[1])+1; worldY++ {
			if got := at(worldY); got != Air {
				t.Fatalf("seed %d has block %d in spawn clearance at y=%d", seed, got, worldY)
			}
		}
	}
	if canopySeeds == 0 {
		t.Fatal("the spawn sweep exercised no canopy over the spawn column")
	}
}

// The named regression. These are real seeds whose surface at the spawn column reaches
// or passes the old hardcoded y=80, so the constant put the player inside rock — 43
// peaks at 97 and 109 at 101. The sweep above would miss them: only a few seeds in a
// hundred are affected, so a range test is a coin flip and these names are not.
//
// **The list was recomputed for worldgen 3 and the old one is gone**, exactly as the
// refusal below instructs: 523, 546, 1098, 1301, 2128 and 2289 were the seeds that
// cleared 80 under a single 40-block amplitude, and a relief-driven amplitude moves
// every column of every seed. What the test pins is the relationship, not the names —
// the names only exist so the relationship is exercised at all.
func TestSeedsThatUsedToBuryThePlayerNowSpawnInAir(t *testing.T) {
	t.Parallel()

	for _, seed := range []int64{19, 24, 38, 43, 81, 86, 92, 109} {
		surface := HeightAt(seed, spawnColumnX, spawnColumnZ)
		if surface < 80 {
			t.Fatalf("seed %d no longer reaches y=80 (surface %d): the terrain changed, so this "+
				"regression list is stale and needs recomputing", seed, surface)
		}

		spawn := SpawnAt(seed)
		if int(spawn[1]) <= surface {
			t.Errorf("seed %d spawns at y=%v, still at or below its surface %d", seed, spawn[1], surface)
		}
	}
}

func TestSpawnIsDeterministicAndFinite(t *testing.T) {
	t.Parallel()

	for _, seed := range []int64{1, 42, 987654321, -7} {
		first := SpawnAt(seed)
		if second := SpawnAt(seed); first != second {
			t.Errorf("seed %d produced %v then %v", seed, first, second)
		}
		for axis, v := range first {
			if math.IsNaN(float64(v)) || math.IsInf(float64(v), 0) {
				t.Errorf("seed %d axis %d is not finite: %v", seed, axis, v)
			}
		}
	}
}

// SpawnClearance's comment claims two things — too little embeds the player, too much
// drops them — so the test has to check both, as bounds.
//
// The first version of this test computed its expected value from SpawnClearance itself,
// which made it tautological: it could only prove that SpawnAt reads its own constant, and
// would have passed just as happily with a clearance of 0 or 40. Restating the literal `2`
// instead would fail on a legitimate retune to 3, which is not a defect. The bounds fail on
// the two things that are.
func TestSpawnClearanceKeepsThePlayerOnTheGroundWithoutBuryingThem(t *testing.T) {
	t.Parallel()

	// A body needs at least one block of air to stand in, and anything past a few blocks
	// stops being a placement and becomes a fall once gravity exists.
	const minimumAir = 1
	const maximumFall = 4

	for seed := int64(1); seed <= 300; seed++ {
		top := generatedColumnTop(seed, spawnColumnX, spawnColumnZ)
		above := int(SpawnAt(seed)[1]) - top

		if above < minimumAir {
			t.Fatalf("seed %d spawns %d blocks above its generated top %d: the player is inside terrain or canopy",
				seed, above, top)
		}
		if above > maximumFall {
			t.Fatalf("seed %d spawns %d blocks above its generated top %d: that is a fall, not a placement",
				seed, above, top)
		}
	}
}
