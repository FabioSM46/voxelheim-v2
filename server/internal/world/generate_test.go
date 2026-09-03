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

	// Tundra conifers deliberately leave the existing taiga fixture unchanged.
	// Plains trees are sparse enough that none of the three original fixtures pins
	// one, so this fourth fixture names a plains chunk that contains a conifer. The
	// two fixtures together make "outside tundra is byte-identical" executable.
	//
	// **Worldgen 17 read them in the other direction**, and it is the first bump that
	// has: the dry taiga fixture moved and this one, the water one, the settlement
	// one and the river one did not, which is what "the plains distribution is
	// unchanged" looks like in bytes rather than in a claim.
	goldenPlainsPath = "testdata/chunk_golden_plains.bin"

	// The fifth fixture, added for the reason the second, third and fourth were, and
	// this time the gap was measured before the change rather than after it.
	//
	// **Worldgen 15 re-cut every river in the world and left all four fixtures above
	// byte-identical.** Rivers are a curve through a few percent of columns, so a chunk
	// holds one or it does not, and none of those four did: the golden test would have
	// passed a change that moved every channel in the world. Third time this file has
	// had to record that a fixture only pins what happens to be in it.
	//
	// It was chosen by sweeping for the richest channel rather than the richest
	// palette, and worldgen 23 moves it to keep that contract after channels gain a
	// stable physical width: 271 channel columns standing on two terraces four blocks
	// apart — 48 and 52 — with 557 source-water voxels carrying all four current ids.
	goldenRiverPath = "testdata/chunk_golden_river.bin"

	// The sixth fixture, added for the reason the second through fifth were.
	//
	// **#874 moves generation only where a bush's or a desert shrub's voxel physically
	// overlaps a tree's canopy or another plant's own root.** Bush and DesertShrub
	// joining [Cover] lets setTreeBlock's `Cover(current)` arm reclaim a voxel either
	// used to hold onto as solid ground, exactly as two flowers already resolved —
	// see TestTwoCoverPlantsResolveToWhicheverWasWrittenSecond. None of the five
	// fixtures above happens to hold such a voxel, so the golden test would once
	// again have said nothing about a change that moved real terrain.
	//
	// It was found by generating a 1,536-chunk sweep across five seeds — the mixed
	// plains/taiga square TestATaigaConiferOutranksTheLowCoverThatWantsItsColumn
	// already uses, the plains square findFlower/findBush/plainsLowCoverDigest use,
	// and a desert square found by sampling for the densest window — before and
	// after the change, and diffing per-chunk digests: 65 of 1,536 sampled chunks
	// moved for this seed alone. This coordinate holds the smallest change found:
	// one voxel, at world (280, 84, 30), where a conifer's canopy reclaims a voxel a
	// bush used to hold, Bush becoming Leaves.
	goldenBushPath = "testdata/chunk_golden_bush.bin"
)

var (
	goldenCoord           = Coord{X: 3, Y: 2, Z: -5}
	goldenWaterCoord      = Coord{X: 182, Y: 1, Z: -59}
	goldenSettlementCoord = Coord{X: -280, Y: 1, Z: -272}
	goldenPlainsCoord     = Coord{X: 3, Y: 2, Z: 64}
	goldenRiverCoord      = Coord{X: 195, Y: 1, Z: -65}
	goldenBushCoord       = Coord{X: 8, Y: 2, Z: 0}

	// bodyCaveMouthCoord holds a cave mouth in the bed of a lake — a carved voxel above
	// [caveWaterLevel] that the run from its own column's ground reaches, and so the one
	// case a body still fills after #660. It is not a golden fixture; see
	// [TestCaveWaterStandsOnlyInCarvedVoxelsBelowItsLevel], which is the only thing that
	// generates it.
	bodyCaveMouthCoord = Coord{X: 161, Y: 1, Z: -61}
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
		{"plains-conifer", goldenPlainsCoord, goldenPlainsPath},
		{"river", goldenRiverCoord, goldenRiverPath},
		{"bush", goldenBushCoord, goldenBushPath},
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

// The two non-tundra conifer fixtures make the compatibility half of the snow-cap
// rule explicit. A fixture that stopped containing the climate and feature named
// here would still compare equal to itself after an update while pinning nothing.
func TestTheNonTundraGoldenChunksContainConifers(t *testing.T) {
	t.Parallel()

	for _, fixture := range []struct {
		name    string
		coord   Coord
		climate Climate
	}{
		{"taiga", goldenCoord, Taiga},
		{"plains", goldenPlainsCoord, Plains},
	} {
		originX, _, originZ := fixture.coord.Origin()
		for z := range ChunkSize {
			for x := range ChunkSize {
				if got := ClimateAt(goldenSeed, originX+int64(x), originZ+int64(z)); got != fixture.climate {
					t.Fatalf("%s golden column (%d, %d) is %v, want %v", fixture.name, x, z, got, fixture.climate)
				}
			}
		}

		logs := 0
		for _, block := range Generate(goldenSeed, fixture.coord).Blocks {
			if block == Log {
				logs++
			}
			if block == Snow {
				t.Fatalf("%s conifer golden contains a snow block in its feature chunk", fixture.name)
			}
		}
		if logs == 0 {
			t.Fatalf("%s golden contains no conifer log", fixture.name)
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

// The river fixture pins the feature it was chosen for, and not merely some bytes.
//
// **A golden chunk is only worth what is in it**, which is the lesson three of the five
// fixtures were added to record. This one is worth a channel: water standing in it, more
// than one terrace under that water, and a bed of gravel it is cut into. Since worldgen
// 16 the water is running water, so the four current ids are pinned here too.
func TestTheRiverGoldenChunkStillHoldsATerracedChannel(t *testing.T) {
	t.Parallel()

	chunk := Generate(goldenSeed, goldenRiverCoord)
	counts := map[Block]int{}
	for _, block := range chunk.Blocks {
		counts[block]++
	}
	for _, block := range []Block{WaterCurrentXPos, WaterCurrentXNeg, WaterCurrentZPos, WaterCurrentZNeg, Gravel} {
		if counts[block] == 0 {
			t.Errorf("the river golden chunk holds no block %d; it no longer pins what it was chosen for", block)
		}
	}

	originX, _, originZ := goldenRiverCoord.Origin()
	terraces := map[int]int{}
	for z := range ChunkSize {
		for x := range ChunkSize {
			if col := columnAt(goldenSeed, originX+int64(x), originZ+int64(z)); col.river {
				terraces[col.waterSurface]++
			}
		}
	}
	if len(terraces) < 2 {
		t.Errorf("the river golden chunk stands on %d terrace(s) %v; a fall needs two", len(terraces), terraces)
	}
}

func TestWorldgenVersionRecordsTheFeatureBreak(t *testing.T) {
	t.Parallel()

	if WorldgenVersion != 28 {
		t.Fatalf("WorldgenVersion = %d, want 28 after Bush and DesertShrub joined Cover", WorldgenVersion)
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

	// The second coordinate holds flowers, whose colours come from a lattice nothing
	// else in the generator reads.
	fx, fz, fcol, _ := findFlower(t)
	flowered := ChunkOf(fx, int64(fcol.surface+1), fz)

	const goroutines = 8
	results := make([][]Block, goroutines)

	var wg sync.WaitGroup
	for i := range goroutines {
		wg.Add(1)
		go func() {
			defer wg.Done()
			results[i] = append(Generate(goldenSeed, goldenCoord).Blocks, Generate(climateSeed, flowered).Blocks...)
		}()
	}
	wg.Wait()

	for i := 1; i < goroutines; i++ {
		if !slices.Equal(results[0], results[i]) {
			t.Fatalf("goroutine %d generated different blocks", i)
		}
	}
}

func TestATundraConiferChunkIsDeterministicUnderConcurrency(t *testing.T) {
	t.Parallel()

	x, z, col, h := findTundraConifer(t)
	coord := ChunkOf(x, int64(col.surface+1), z)
	const goroutines = 8
	results := make([][]Block, goroutines)

	var wg sync.WaitGroup
	for i := range goroutines {
		wg.Add(1)
		go func() {
			defer wg.Done()
			results[i] = Generate(climateSeed, coord).Blocks
		}()
	}
	wg.Wait()

	for i := 1; i < goroutines; i++ {
		if !slices.Equal(results[0], results[i]) {
			t.Fatalf("goroutine %d generated different tundra conifer bytes", i)
		}
	}
	if got := results[0][Index(Local(x), Local(int64(col.surface+1)), Local(z))]; got != Log {
		t.Fatalf("selected tundra root generated %d above its surface, want Log", got)
	}

	capY := int64(col.surface + coniferTrunkHeight(h) + treeCanopyAboveCrown + 1)
	capCoord := ChunkOf(x, capY, z)
	cap := Generate(climateSeed, capCoord)
	if got := cap.At(Local(x), Local(capY), Local(z)); got != Snow {
		t.Fatalf("selected tundra crown generated cap %d, want Snow", got)
	}
	if terrain := col.blockAt(int(capY)); terrain != Air {
		t.Fatalf("snow cap replaced terrain block %d rather than filling air", terrain)
	}
}

func TestAPlantSnowVoxelOnlyFillsAir(t *testing.T) {
	t.Parallel()

	chunk := NewChunk(Coord{})
	chunk.Set(1, 2, 3, Stone)
	setTreeBlock(chunk, 1, 2, 3, Snow)
	if got := chunk.At(1, 2, 3); got != Stone {
		t.Fatalf("snow plant voxel replaced block %d, want Stone preserved", got)
	}

	setTreeBlock(chunk, 4, 5, 6, Snow)
	if got := chunk.At(4, 5, 6); got != Snow {
		t.Fatalf("snow plant voxel placed %d into air, want Snow", got)
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
// either a cave or the bug this function exists to catch, and only the carve rule can
// tell the two apart.
//
// **The carve rule and not the carve field, since #660.** [caveAt] is the two noise
// fields alone; [column.carvedAt] is those with the settlement exemption and the bank
// rule applied, and the bank rule leaves rock standing where the fields would have
// hollowed it out. Reading the field here would report every one of those as terrain
// that something carved away.
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
		carved := col.carvedAt(seed, worldX, worldY, worldZ)
		if carved && got != col.caveFillAt(int(worldY)) && got != Log && got != Leaves {
			t.Fatalf("carved voxel (%d, %d, %d) is block %d rather than the %d the cave fill puts there",
				worldX, worldY, worldZ, got, col.caveFillAt(int(worldY)))
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
		case PalmLog, PalmFronds, DesertShrub:
			featured = true
			if terrain != Air || !plantCanPlace(seed, worldX, worldY, worldZ, got) {
				t.Fatalf("desert plant block %d at (%d, %d, %d) is not part of a deterministic species shape",
					got, worldX, worldY, worldZ)
			}
		case WinterBramble:
			featured = true
			if terrain != Air || !plantCanPlace(seed, worldX, worldY, worldZ, got) {
				t.Fatalf("winter bramble at (%d, %d, %d) is not part of a deterministic species shape",
					worldX, worldY, worldZ)
			}
		case Snow:
			if terrain == Snow {
				break
			}
			featured = true
			if terrain != Air || !treeCanPlace(seed, worldX, worldY, worldZ, Snow) {
				t.Fatalf("snow at (%d, %d, %d) is neither terrain nor a deterministic tundra cap", worldX, worldY, worldZ)
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
				if want := col.caveFillAt(int(worldY)); got != want {
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

func plantCanPlace(seed, worldX, worldY, worldZ int64, want Block) bool {
	footprint := int64(largestPlantFootprint())
	for rootZ := worldZ - footprint; rootZ <= worldZ+footprint; rootZ++ {
		for rootX := worldX - footprint; rootX <= worldX+footprint; rootX++ {
			found := false
			visitPlant(seed, rootX, rootZ, func(x, y, z int64, block Block) {
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
	treeSeed, rootZ, treeSurface, _ := findIsolatedEastBorderPlant(t, &plantSpeciesTable[0])
	treeCoord := ChunkOf(ChunkSize-1, int64(treeSurface), rootZ)
	treeChunk := Generate(treeSeed, treeCoord)
	treeCol := columnAt(treeSeed, ChunkSize-1, rootZ)
	wantTreeSurface := treeCol.blockAt(treeSurface)
	if got := treeChunk.At(Local(ChunkSize-1), Local(int64(treeSurface)), Local(rootZ)); got != wantTreeSurface {
		t.Fatalf("tree-bearing surface at (%d, %d) is block %d, want its unchanged ground %d",
			ChunkSize-1, rootZ, got, wantTreeSurface)
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

func TestEverySpeciesGrowsOnlyFromTheSurfaceItsRowNames(t *testing.T) {
	t.Parallel()

	seen := make(map[string]int, len(plantSpeciesTable))
	const steps = 1024
	for i := range steps {
		for j := range steps {
			x, z := int64(i)*61, int64(j)*61
			col := columnAt(climateSeed, x, z)
			species, _, rooted := plantAtColumn(climateSeed, x, z, col)
			if !rooted {
				continue
			}
			surface := col.blockAt(col.surface)
			if !species.rootsOn(surface) {
				t.Fatalf("%s at (%d, %d) roots on block %d its row refuses", species.name, x, z, surface)
			}
			seen[species.name]++
		}
	}
	for i := range plantSpeciesTable {
		if seen[plantSpeciesTable[i].name] == 0 {
			t.Errorf("fixed lattice selected no %s roots", plantSpeciesTable[i].name)
		}
	}
}

func TestATreeCrossingAChunkBorderIsCompleteInEitherGenerationOrder(t *testing.T) {
	t.Parallel()

	for i := range plantSpeciesTable {
		species := &plantSpeciesTable[i]
		if species.footprint == 0 {
			continue
		}
		t.Run(species.name, func(t *testing.T) {
			seed, rootZ, surface, h := findIsolatedEastBorderPlant(t, species)

			want := make(map[[3]int64]Block)
			minY, maxY := int64(1<<62), int64(-(1 << 62))
			species.visit(seed, ChunkSize-1, rootZ, surface, h, func(x, y, z int64, block Block) {
				want[[3]int64{x, y, z}] = block
				minY = min(minY, y)
				maxY = max(maxY, y)
			})
			if len(want) == 0 {
				t.Fatal("species shape visited no voxels")
			}

			chunkY := int32(floorDiv(minY, ChunkSize))
			if int32(floorDiv(maxY, ChunkSize)) != chunkY {
				t.Fatalf("selected %s crosses a vertical chunk boundary", species.name)
			}
			leftCoord := Coord{X: 0, Y: chunkY, Z: 0}
			rightCoord := Coord{X: 1, Y: chunkY, Z: 0}

			leftThenRight := [2]*Chunk{Generate(seed, leftCoord), Generate(seed, rightCoord)}
			rightFirst := Generate(seed, rightCoord)
			rightThenLeft := [2]*Chunk{Generate(seed, leftCoord), rightFirst}
			if !slices.Equal(leftThenRight[0].Blocks, rightThenLeft[0].Blocks) ||
				!slices.Equal(leftThenRight[1].Blocks, rightThenLeft[1].Blocks) {
				t.Fatal("chunk bytes changed with generation order")
			}

			features := [2]int{}
			for pos, block := range want {
				worldX, worldY, worldZ := pos[0], pos[1], pos[2]
				rootCol := columnAt(seed, worldX, worldZ)
				if rootCol.blockAt(int(worldY)) != Air {
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
					t.Fatalf("border %s voxel (%d, %d, %d) is %d, want %d", species.name, worldX, worldY, worldZ, got, block)
				}
				features[floorDiv(worldX, ChunkSize)]++
			}
			if features[0] == 0 || features[1] == 0 {
				t.Fatalf("border %s contributed %d voxels to the left chunk and %d to the right", species.name, features[0], features[1])
			}
		})
	}
}

func findIsolatedEastBorderPlant(t *testing.T, target *plantSpecies) (seed, rootZ int64, surface int, h uint64) {
	t.Helper()

	const rootX = ChunkSize - 1
	isolation := int64(target.footprint + largestPlantFootprint())
	for seed = 1; seed <= 2000; seed++ {
		for rootZ = isolation; rootZ < ChunkSize-isolation; rootZ++ {
			col := columnAt(seed, rootX, rootZ)
			species, candidateHash, ok := plantAtColumn(seed, rootX, rootZ, col)
			if !ok || species != target {
				continue
			}

			minY, maxY := int64(1<<62), int64(-(1 << 62))
			crossesEast := false
			target.visit(seed, rootX, rootZ, col.surface, candidateHash, func(x, y, _ int64, _ Block) {
				minY = min(minY, y)
				maxY = max(maxY, y)
				crossesEast = crossesEast || x >= ChunkSize
			})
			if !crossesEast || minY > maxY || ChunkOf(rootX, minY, rootZ).Y != ChunkOf(rootX, maxY, rootZ).Y {
				continue
			}

			isolated := true
			for nearbyZ := rootZ - isolation; nearbyZ <= rootZ+isolation && isolated; nearbyZ++ {
				for nearbyX := int64(rootX) - isolation; nearbyX <= int64(rootX)+isolation; nearbyX++ {
					if nearbyX == rootX && nearbyZ == rootZ {
						continue
					}
					if otherSpecies, _, other := plantAtColumn(seed, nearbyX, nearbyZ, columnAt(seed, nearbyX, nearbyZ)); other && otherSpecies.footprint > 0 {
						isolated = false
						break
					}
				}
			}
			if isolated {
				return seed, rootZ, col.surface, candidateHash
			}
		}
	}
	t.Fatalf("no isolated %s rooted at the east chunk border in the deterministic search", target.name)
	return 0, 0, 0, 0
}

// The invariant is a relationship, not a number: the generated top of a column is air
// above solid ground, and generatedColumnTop agrees with the voxels the generator wrote.
//
// **It was the spawn sweep and it is now the origin column's**, because #519 moved
// [SpawnAt] onto the capital's gate square and this body could not follow: the two things
// it uniquely measures — a canopy over the column, and the sea line as a floor — a
// settlement's plateau can never show. The spawn's own half of the claim moved to
// TestTheSpawnIsOnTheCapitalsSquareOutsideTheKeep.
func TestTheOriginColumnIsAirAboveItsGeneratedTopForEverySeed(t *testing.T) {
	t.Parallel()

	canopySeeds := 0
	for seed := int64(1); seed <= 300; seed++ {
		surface := HeightAt(seed, originColumnX, originColumnZ)
		chunks := make(map[Coord]*Chunk)

		// Read the actual voxels rather than trusting the arithmetic: the height field
		// and the generator could disagree, and that disagreement is the bug class here.
		at := func(worldY int) Block {
			coord := ContainingChunk(originColumnX+0.5, float32(worldY), originColumnZ+0.5)
			chunk := chunks[coord]
			if chunk == nil {
				chunk = Generate(seed, coord)
				chunks[coord] = chunk
			}
			ox, oy, oz := coord.Origin()
			return chunk.At(originColumnX-int(ox), worldY-int(oy), originColumnZ-int(oz))
		}

		// Find the real top from generated voxels, independently of the helper SpawnAt
		// uses. The global terrain maximum plus the tallest tree bounds the search.
		//
		// **Water is not a top and ice is**, which is the same rule generatedColumnTop
		// follows: the only fill in this world that stops movement is the lid a tundra
		// wears at the sea line, and a body stands on that.
		searchTop := baseHeight + mountainAmplitude/2 + treeMinTrunkHeight + treeHeightVariants + treeCanopyAboveCrown
		actualTop := surface
		for worldY := surface + 1; worldY <= searchTop; worldY++ {
			if Solid(at(worldY)) {
				actualTop = worldY
			}
		}
		if helperTop := generatedColumnTop(seed, originColumnX, originColumnZ); helperTop != actualTop {
			t.Fatalf("seed %d computed generated top %d, actual generated top is %d", seed, helperTop, actualTop)
		}
		if actualTop > surface {
			canopySeeds++
		}
		if got := at(surface); got == Air {
			t.Fatalf("seed %d has air at its own surface height %d", seed, surface)
		}
		// The sea line is a floor under a body's reference, never a ceiling: a column above
		// the water is stood on from its own top, one below it from the water's surface.
		reference := max(actualTop, seaLevel)
		for worldY := actualTop + 1; worldY <= reference+SpawnClearance+1; worldY++ {
			got := at(worldY)
			if Solid(got) {
				t.Fatalf("seed %d has solid block %d over its generated top at y=%d", seed, got, worldY)
			}
			// Below the sea line that space may be water — a body swims out of it. Above
			// it, air and ground cover are legal: a top wearing a drift puts a body's feet
			// *in* a flower, and solidity is what this space is about (checked above).
			if worldY > seaLevel && got != Air && !Cover(got) {
				t.Fatalf("seed %d has block %d at y=%d, above the sea line", seed, got, worldY)
			}
		}
	}
	if canopySeeds == 0 {
		t.Fatal("the sweep exercised no canopy over the origin column")
	}
}

// The spawn is the capital's gate square: three blocks outside the castle's gate, on the
// plateau, checked against the world the generator writes rather than against the
// arithmetic that produced it.
//
// **The footprint sweep is the one that would have caught a stale constant.**
// [capitalSpawnOffset] is derived from [largestHalfFootprint] because #555 grew the castle
// from 15 blocks across to 21 after this placement was first specified, and a literal
// written against the older drawing puts a session in the gate passage — inside the keep's
// own extent, which is a building and not a square.
func TestTheSpawnIsOnTheCapitalsSquareOutsideTheKeep(t *testing.T) {
	t.Parallel()

	for seed := int64(1); seed <= 200; seed++ {
		capital := CapitalAt(seed)
		spawn := SpawnAt(seed)
		x, z := int64(math.Floor(float64(spawn[0]))), int64(math.Floor(float64(spawn[2])))

		if want := [3]float32{
			float32(capital.CentreX) + 0.5,
			float32(capital.Plateau + SpawnClearance),
			float32(capital.CentreZ+capitalSpawnOffset) + 0.5,
		}; spawn != want {
			t.Fatalf("seed %d spawns at %v, want the capital's gate square %v", seed, spawn, want)
		}

		// On the plateau: inside the flat disc, and the height field agrees — which is what
		// says the spawn is on the settlement's ground rather than on the blend band.
		if d := isqrt(squaredDistance(x, z, capital.CentreX, capital.CentreZ)); d > int64(capital.Radius) {
			t.Fatalf("seed %d spawns %d blocks from the capital's centre, outside its radius %d",
				seed, d, capital.Radius)
		}
		if got := HeightAt(seed, x, z); got != capital.Plateau {
			t.Fatalf("seed %d spawns over ground at %d, not the capital's plateau %d", seed, got, capital.Plateau)
		}

		// Outside every building, against each drawing's placed extent.
		for _, b := range capital.Buildings {
			w, d := rotatedFootprint(SchematicFor(b.Kind), b.Facing)
			if x >= b.OriginX && x < b.OriginX+int64(w) && z >= b.OriginZ && z < b.OriginZ+int64(d) {
				t.Fatalf("seed %d spawns at (%d, %d), inside the %v standing at (%d, %d)-(%d, %d)",
					seed, x, z, b.Kind, b.OriginX, b.OriginZ, b.OriginX+int64(w)-1, b.OriginZ+int64(d)-1)
			}
		}

		// **This assertion replaced a line of code.** SpawnAt floored its height at seaLevel
		// because the origin column could be a lake bed; the capital cannot be, so the
		// floor became unreachable and is checked here instead of carried there.
		if int(spawn[1]) <= seaLevel {
			t.Fatalf("seed %d spawns at y=%v, at or under the sea line %d", seed, spawn[1], seaLevel)
		}

		// And the generated voxels agree: solid plateau under the feet, air above it. The
		// chunk is resolved per y, because the plateau and the head can straddle a border.
		chunks := make(map[Coord]*Chunk)
		at := func(worldY int) Block {
			coord := ContainingChunk(spawn[0], float32(worldY), spawn[2])
			chunk := chunks[coord]
			if chunk == nil {
				chunk = Generate(seed, coord)
				chunks[coord] = chunk
			}
			ox, oy, oz := coord.Origin()
			return chunk.At(int(x-ox), worldY-int(oy), int(z-oz))
		}
		if got := at(capital.Plateau); !Solid(got) {
			t.Fatalf("seed %d has block %d under its spawn, want the plateau's solid ground", seed, got)
		}
		for worldY := capital.Plateau + 1; worldY <= int(spawn[1])+1; worldY++ {
			if got := at(worldY); got != Air {
				t.Fatalf("seed %d has block %d at y=%d on the gate square, want air", seed, got, worldY)
			}
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
		// The reference is the capital's plateau: flat by construction across the whole
		// disc, so it *is* the surface of the spawn column. It used to be the generated top
		// of the origin column or the sea line above it; a plateau is never submerged.
		reference := CapitalAt(seed).Plateau
		above := int(SpawnAt(seed)[1]) - reference

		if above < minimumAir {
			t.Fatalf("seed %d spawns %d blocks above the capital's plateau %d: the player is inside the ground",
				seed, above, reference)
		}
		if above > maximumFall {
			t.Fatalf("seed %d spawns %d blocks above the capital's plateau %d: that is a fall, not a placement",
				seed, above, reference)
		}
	}
}

// A session never begins under water, for any seed.
//
// **The rule that delivers this moved.** It used to be two halves on the origin column:
// originWaterClearance kept basins and channels off it, and SpawnAt floored its height at
// the sea line for the ordinary terrain that put it on a lake bed anyway. Neither is on
// the spawn path now — what keeps a session dry is [settlementMinPlateau], three blocks
// of freeboard [capitalSiteAt] holds even on its fallback. The counter keeps that from
// being a claim about nothing: it counts the seeds whose *origin* column is submerged,
// which are exactly the worlds the old floor existed for.
func TestASessionNeverBeginsUnderWater(t *testing.T) {
	t.Parallel()

	submergedOrigins := 0
	for seed := int64(1); seed <= 300; seed++ {
		spawn := SpawnAt(seed)
		if int(spawn[1]) <= seaLevel {
			t.Fatalf("seed %d spawns at y=%v, at or under the sea line %d", seed, spawn[1], seaLevel)
		}
		if generatedColumnTop(seed, originColumnX, originColumnZ) < seaLevel {
			submergedOrigins++
		}
	}
	if submergedOrigins == 0 {
		t.Fatal("no seed in the sweep put the origin column under the sea line, so the seeds the old floor existed for were never exercised")
	}
}

// Neither a basin nor a channel touches the ground around the origin column.
func TestNoWaterFeatureReachesTheOriginSquare(t *testing.T) {
	t.Parallel()

	const seed = goldenSeed
	for z := int64(originColumnZ - originWaterClearance); z <= originColumnZ+originWaterClearance; z++ {
		for x := int64(originColumnX - originWaterClearance); x <= originColumnX+originWaterClearance; x++ {
			col := columnAt(seed, x, z)
			if col.river {
				t.Errorf("a river channel runs through the origin square at (%d, %d)", x, z)
			}
			nx := floorDiv(x<<fracBits, terrainScaleBlocks)
			nz := floorDiv(z<<fracBits, terrainScaleBlocks)
			base := baseHeight + int((amplitudeAt(seed, x, z)*(fbm2D(seed, nx, nz)-one/2))>>fracBits)
			if col.surface != base {
				t.Errorf("the column at (%d, %d) was lowered from %d to %d inside the origin square", x, z, base, col.surface)
			}
		}
	}
}
