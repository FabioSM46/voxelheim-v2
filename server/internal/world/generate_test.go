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

	// The second fixture, and why there is one.
	//
	// **A golden chunk only pins the features that happen to be in it.** The original
	// coordinate sits at surfaces 61 to 72 — above the sea line, with no basin, no
	// channel and nothing carved deep enough to stand in water — so worldgen 5 left
	// its bytes *identical*, and the test that exists to make a generator change
	// impossible to miss would have said nothing at all about the change that added
	// water. That is the same shape of failure as a diagnostic nobody reads: not a
	// wrong answer, an answer to a question nobody asked here.
	//
	// So the original stays, unchanged and still pinning the dry world it always
	// pinned, and this one is added beside it. It was chosen by sweeping a region for
	// the richest palette: it holds eight distinct block ids, among them the sea fill,
	// a lid of ice, a beach and 1104 voxels of water standing in carved cave.
	goldenWaterPath = "testdata/chunk_golden_water.bin"

	// The third fixture, added for the reason the second one was.
	//
	// **Worldgen 6 left both of the fixtures above byte-identical**, because
	// settlements are a lattice rather than a field: only ground within seventy-two
	// blocks of a settlement's centre moves, and neither of those two coordinates is.
	// So the test that exists to make a generator change impossible to miss would once
	// again have said nothing about the change — the same shape of failure the water
	// fixture was added to fix, one iteration later.
	//
	// This one was chosen by sweeping the settlements around the origin for a chunk
	// that holds a building *and* the ground it stands on: eight distinct block ids,
	// among them 628 voxels of plank, cobble and thatch, the plateau's grass, the rock
	// under it and a little standing water.
	goldenSettlementPath = "testdata/chunk_golden_settlement.bin"
)

var (
	goldenCoord           = Coord{X: 3, Y: 2, Z: -5}
	goldenWaterCoord      = Coord{X: 182, Y: 1, Z: -59}
	goldenSettlementCoord = Coord{X: -280, Y: 1, Z: -272}
)

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
	for _, fixture := range []struct {
		name  string
		coord Coord
		path  string
	}{
		{"dry", goldenCoord, goldenPath},
		{"water", goldenWaterCoord, goldenWaterPath},
		{"settlement", goldenSettlementCoord, goldenSettlementPath},
	} {
		got := encodedBytes(Encode(Generate(goldenSeed, fixture.coord)))

		if *updateGolden {
			if err := os.MkdirAll(filepath.Dir(fixture.path), 0o755); err != nil {
				t.Fatalf("create testdata: %v", err)
			}
			if err := os.WriteFile(fixture.path, got, 0o644); err != nil {
				t.Fatalf("write golden: %v", err)
			}
			t.Logf("%s golden fixture rewritten: %d bytes", fixture.name, len(got))
			continue
		}

		want, err := os.ReadFile(fixture.path)
		if err != nil {
			t.Fatalf("read %s golden (regenerate with -update-golden if this is a new fixture): %v", fixture.name, err)
		}
		if !bytes.Equal(got, want) {
			t.Fatalf("chunk %+v for seed %#x no longer matches the %s golden fixture: %d bytes now, %d before",
				fixture.coord, goldenSeed, fixture.name, len(got), len(want))
		}
	}
}

// The water fixture has to keep being about water, or it is a second copy of the dry
// one and the reason it was added is gone.
func TestTheWaterGoldenChunkActuallyHoldsWater(t *testing.T) {
	t.Parallel()

	chunk := Generate(goldenSeed, goldenWaterCoord)
	counts := map[Block]int{}
	for _, block := range chunk.Blocks {
		counts[block]++
	}

	for _, block := range []Block{Water, Ice, Sand} {
		if counts[block] == 0 {
			t.Errorf("the water golden chunk holds no block %d; it no longer pins what it was chosen for", block)
		}
	}

	// And some of that water is underground, which is the one fill the sea line does
	// not produce.
	originX, originY, originZ := goldenWaterCoord.Origin()
	underground := 0
	for z := range ChunkSize {
		for x := range ChunkSize {
			col := columnAt(goldenSeed, originX+int64(x), originZ+int64(z))
			for y := range ChunkSize {
				if chunk.At(x, y, z) == Water && originY+int64(y) <= int64(col.surface) {
					underground++
				}
			}
		}
	}
	if underground == 0 {
		t.Error("the water golden chunk holds no water below a surface: the cave fill is not pinned")
	}
}

// The settlement fixture has to keep being about a settlement, or it is a third copy of
// the dry one and the reason it was added is gone.
func TestTheSettlementGoldenChunkActuallyHoldsABuilding(t *testing.T) {
	t.Parallel()

	chunk := Generate(goldenSeed, goldenSettlementCoord)
	counts := map[Block]int{}
	for _, block := range chunk.Blocks {
		counts[block]++
	}

	for _, block := range []Block{Planks, Cobblestone, Thatch, Grass, Stone} {
		if counts[block] == 0 {
			t.Errorf("the settlement golden chunk holds no block %d; it no longer pins what it was chosen for", block)
		}
	}

	// And the ground it stands on is a plateau, which is the half of this feature the
	// building voxels alone would not pin.
	originX, _, originZ := goldenSettlementCoord.Origin()
	settled := 0
	for z := range ChunkSize {
		for x := range ChunkSize {
			if columnAt(goldenSeed, originX+int64(x), originZ+int64(z)).settlement {
				settled++
			}
		}
	}
	if settled == 0 {
		t.Error("no column of the settlement golden chunk stands inside a settlement's radius")
	}
}

func TestWorldgenVersionRecordsTheFeatureBreak(t *testing.T) {
	t.Parallel()

	if WorldgenVersion != 8 {
		t.Fatalf("WorldgenVersion = %d, want 8 for the capital's castle", WorldgenVersion)
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
		if carved && got != caveFillAt(int(worldY)) && got != Log && got != Leaves {
			t.Fatalf("carved voxel (%d, %d, %d) is block %d rather than the %d the cave fill puts there",
				worldX, worldY, worldZ, got, caveFillAt(int(worldY)))
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
		case Water, Ice:
			// Two fills, and which one applies is decided by whether the terrain
			// here was ground. Above the surface it is the sea line's; below it, it
			// is a carved voxel's, and anything else is water in solid rock.
			featured = true
			switch {
			case terrain == Air:
				if want := col.fillAt(int(worldY)); got != want {
					t.Fatalf("fill at (%d, %d, %d) is %d, want %d above surface %d",
						worldX, worldY, worldZ, got, want, surface)
				}
			case carved:
				if want := caveFillAt(int(worldY)); got != want {
					t.Fatalf("cave fill at (%d, %d, %d) is %d, want %d", worldX, worldY, worldZ, got, want)
				}
			default:
				t.Fatalf("block %d at (%d, %d, %d) stands in terrain block %d that nothing carved",
					got, worldX, worldY, worldZ, terrain)
			}
		case Air:
			switch {
			case terrain != Air:
				featured = true
				if !carved {
					t.Fatalf("plain chunk %+v voxel (%d, %d, %d) [world y=%d, surface=%d] is air, want %d",
						c.Coord, x, y, z, worldY, surface, terrain)
				}
			case col.fillAt(int(worldY)) != Air:
				t.Fatalf("plain chunk %+v voxel (%d, %d, %d) [world y=%d, surface=%d] is air, want the %d the sea line fills it with",
					c.Coord, x, y, z, worldY, surface, col.fillAt(int(worldY)))
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

	// A basin digs into whatever the amplitude produced, so the floor of the designed
	// range is that much lower than the field alone can reach. The ceiling is
	// untouched: nothing in this generator raises ground.
	lowest := baseHeight - mountainAmplitude/2 - basinDepth
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

// This test asserts where ore may be, never how much of it there is, and that is
// exactly what it kept asserting while the world had none: at one coal voxel per
// twenty chunks every line below still passed, because "some exists" and "two of
// them touch" are both true of a single vein in a hundred chunks (#540).
// TestOreIsDenseEnoughToFind is the half this one cannot cover.
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

// Ore has to be findable by mining, and only a density says so.
//
// **The floor is the point of this test.** #540 was not ore in the wrong place, it
// was ore in the right place at 0.00015% of the world, and every existence and
// connectivity assertion in the test above survived it. The share a threshold
// selects is not readable from the threshold — fbm3D is an average of four octaves
// and its distribution is bell-shaped — so the only honest statement about density
// is a measured one, and this is where it is measured.
//
// The denominator is the rock a miner actually passes through: voxels inside the
// band that came out Stone or ore, so carved-out cave volume, soil and the sea are
// all excluded. [column.voxelAt] is the real composition path, which is what keeps
// this measuring the generator rather than a second copy of it.
//
// The bands are wide, and deliberately. Coal's share ran 0.30%–1.09% over these
// eight seeds and iron's 0.14%–0.36%, a spread of three to one, because a twelve
// block field gathers ore into veins and how many veins fall inside one square is
// a property of the seed. A band tight enough to pin the mean would be a flake;
// these are set to catch the failure that actually happened — a threshold read as
// if the field were uniform, which is wrong by four orders of magnitude and cannot
// hide inside any factor of three.
func TestOreIsDenseEnoughToFind(t *testing.T) {
	t.Parallel()

	// Half a chunk either side of the origin, per seed: 16 chunk footprints through
	// the full band, which is enough rock that a seed with no vein in it is a
	// finding rather than a sampling accident.
	const side = 128

	coalTotal, coalRock, ironTotal, ironRock := 0, 0, 0, 0
	for seed := int64(1); seed <= 8; seed++ {
		coal, coalBand, iron, ironBand := 0, 0, 0, 0
		for x := int64(-side / 2); x < side/2; x++ {
			for z := int64(-side / 2); z < side/2; z++ {
				col := columnAt(seed, x, z)
				for depth := int64(coalMinDepth); depth <= int64(ironMaxDepth); depth++ {
					worldY := int64(col.surface) - depth
					switch col.voxelAt(seed, x, worldY, z) {
					case Stone:
						if depth <= coalMaxDepth {
							coalBand++
						} else {
							ironBand++
						}
					case CoalOre:
						coalBand++
						coal++
					case IronOre:
						ironBand++
						iron++
					}
				}
			}
		}
		if coalBand == 0 || ironBand == 0 {
			t.Fatalf("seed %d exposed %d coal-band and %d iron-band rock voxels; the sample found no band to measure", seed, coalBand, ironBand)
		}

		// Per seed, a floor and a ceiling. The floor is roughly half the lowest share
		// measured and two hundred times the share the bug produced; the ceiling says
		// ore is still a vein in the rock rather than the rock.
		coalShare := 100 * float64(coal) / float64(coalBand)
		ironShare := 100 * float64(iron) / float64(ironBand)
		if coalShare < 0.15 || coalShare > 3 {
			t.Errorf("seed %d: coal is %.3f%% of the coal band (%d of %d voxels), outside the 0.15–3%% band coalThreshold %d aims at",
				seed, coalShare, coal, coalBand, coalThreshold)
		}
		if ironShare < 0.05 || ironShare > 2 {
			t.Errorf("seed %d: iron is %.3f%% of the iron band (%d of %d voxels), outside the 0.05–2%% band ironThreshold %d aims at",
				seed, ironShare, iron, ironBand, ironThreshold)
		}

		coalTotal += coal
		coalRock += coalBand
		ironTotal += iron
		ironRock += ironBand
	}

	// The aggregate is where the tuning target lives: 0.55% of the coal band and
	// 0.22% of the iron band when these thresholds were chosen.
	coalShare := 100 * float64(coalTotal) / float64(coalRock)
	ironShare := 100 * float64(ironTotal) / float64(ironRock)
	if coalShare < 0.3 || coalShare > 1.2 {
		t.Errorf("coal is %.3f%% of the coal band across eight seeds, outside the 0.3–1.2%% the threshold was set for", coalShare)
	}
	if ironShare < 0.1 || ironShare > 0.6 {
		t.Errorf("iron is %.3f%% of the iron band across eight seeds, outside the 0.1–0.6%% the threshold was set for", ironShare)
	}

	// Coal is the more common of the two on purpose: it gates the forge, and the
	// forge gates everything iron. Equal thresholds would state no design call.
	if coalShare <= ironShare {
		t.Errorf("coal is %.3f%% of its band and iron %.3f%% of its own; coal must be the more common of the two", coalShare, ironShare)
	}

	// The same numbers as the player meets them: what one chunk's footprint holds
	// through the whole band. A forge costs two coal.
	const footprints = 8 * (side * side) / (ChunkSize * ChunkSize)
	if perChunk := float64(coalTotal) / footprints; perChunk < 20 {
		t.Errorf("a chunk's footprint holds %.0f coal voxels through the whole band; a forge costs two and this has to be reachable by digging", perChunk)
	}
	if perChunk := float64(ironTotal) / footprints; perChunk < 10 {
		t.Errorf("a chunk's footprint holds %.0f iron voxels through the whole band", perChunk)
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
		//
		// **Water is not a top and ice is**, which is the same rule generatedColumnTop
		// follows: the only fill in this world that stops movement is the lid a tundra
		// wears at the sea line, and a body stands on that.
		searchTop := baseHeight + mountainAmplitude/2 + treeMinTrunkHeight + treeHeightVariants - 1 + treeCanopyAboveCrown
		actualTop := surface
		for worldY := surface + 1; worldY <= searchTop; worldY++ {
			if Solid(at(worldY)) {
				actualTop = worldY
			}
		}
		if helperTop := generatedColumnTop(seed, spawnColumnX, spawnColumnZ); helperTop != actualTop {
			t.Fatalf("seed %d computed generated top %d, actual generated top is %d", seed, helperTop, actualTop)
		}
		if actualTop > surface {
			canopySeeds++
		}
		// The sea line is a floor under the reference, never a ceiling on it: a
		// column that stands above the water is placed from its own top, and one that
		// does not is placed from the surface of the water it stands in.
		if got, want := int(spawn[1]), max(actualTop, seaLevel)+SpawnClearance; got != want {
			t.Fatalf("seed %d spawns at y=%d, want %d above terrain/canopy top %d and sea line %d",
				seed, got, want, actualTop, seaLevel)
		}
		if got := at(surface); got == Air {
			t.Fatalf("seed %d has air at its own surface height %d", seed, surface)
		}
		for worldY := actualTop + 1; worldY <= int(spawn[1])+1; worldY++ {
			got := at(worldY)
			if Solid(got) {
				t.Fatalf("seed %d has solid block %d in spawn clearance at y=%d", seed, got, worldY)
			}
			// Below the sea line the clearance may be water — the player swims out of
			// it. Above it, nothing but air is a legal answer.
			if worldY > seaLevel && got != Air {
				t.Fatalf("seed %d has block %d in spawn clearance at y=%d, above the sea line", seed, got, worldY)
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
		// The reference the clearance is measured from, which is the generated top or
		// the sea line above it — see SpawnAt. Measuring from the top alone would call
		// a placement on the surface of a lake a seventeen-block fall, when what is
		// under those seventeen blocks is water the player floats on.
		reference := max(generatedColumnTop(seed, spawnColumnX, spawnColumnZ), seaLevel)
		above := int(SpawnAt(seed)[1]) - reference

		if above < minimumAir {
			t.Fatalf("seed %d spawns %d blocks above its spawn reference %d: the player is inside terrain or canopy",
				seed, above, reference)
		}
		if above > maximumFall {
			t.Fatalf("seed %d spawns %d blocks above its spawn reference %d: that is a fall, not a placement",
				seed, above, reference)
		}
	}
}

// A session never begins under water, for any seed.
//
// **spawnWaterClearance cannot deliver this on its own, which is why SpawnAt has a
// floor as well.** The exemption keeps basins and channels off the spawn column, but
// the ordinary height field is concentrated around 64 against a sea line at 47 and a
// mountainAmplitude of 150, so a sizeable minority of seeds put the origin under
// water with no water feature involved at all. The sweep is what says so out loud:
// it fails if either half — the exemption or the floor — is removed.
func TestASessionNeverBeginsUnderWater(t *testing.T) {
	t.Parallel()

	submergedColumns := 0
	for seed := int64(1); seed <= 300; seed++ {
		spawn := SpawnAt(seed)
		if int(spawn[1]) <= seaLevel {
			t.Fatalf("seed %d spawns at y=%v, at or under the sea line %d", seed, spawn[1], seaLevel)
		}
		if generatedColumnTop(seed, spawnColumnX, spawnColumnZ) < seaLevel {
			submergedColumns++
		}
	}
	if submergedColumns == 0 {
		t.Fatal("no seed in the sweep put the spawn column under the sea line, so the floor was never exercised")
	}
}

// Neither a basin nor a channel touches the ground around spawn.
func TestNoWaterFeatureReachesTheSpawnSquare(t *testing.T) {
	t.Parallel()

	const seed = goldenSeed
	for z := int64(spawnColumnZ - spawnWaterClearance); z <= spawnColumnZ+spawnWaterClearance; z++ {
		for x := int64(spawnColumnX - spawnWaterClearance); x <= spawnColumnX+spawnWaterClearance; x++ {
			col := columnAt(seed, x, z)
			if col.river {
				t.Errorf("a river channel runs through the spawn square at (%d, %d)", x, z)
			}
			nx := floorDiv(x<<fracBits, terrainScaleBlocks)
			nz := floorDiv(z<<fracBits, terrainScaleBlocks)
			base := baseHeight + int((amplitudeAt(seed, x, z)*(fbm2D(seed, nx, nz)-one/2))>>fracBits)
			if col.surface != base {
				t.Errorf("the column at (%d, %d) was lowered from %d to %d inside the spawn square", x, z, base, col.surface)
			}
		}
	}
}
