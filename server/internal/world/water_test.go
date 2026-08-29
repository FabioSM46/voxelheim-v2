package world

import (
	"context"
	"testing"
)

// The statistics below are measured at one seed over one named window, for the
// reason caves_test.go states: water is a shape, and the only honest way to assert a
// shape is to count it somewhere fixed.
const (
	waterSeed = 0x5EED

	// The sample window, and why it is where it is. Three reasons, and each of them
	// is an acceptance criterion this file has to be able to reach:
	//
	//   - it is not at the origin, because spawnWaterClearance exempts a square there
	//     and a window containing spawn measures the exemption as well as the fields;
	//   - it holds tundra as well as plains and taiga, because ice exists in exactly
	//     one climate and a window without one cannot see it;
	//   - it holds channels, because riverScaleBlocks is 640 and whether a 1024-block
	//     window contains a river at all is a regional property rather than a global
	//     one.
	//
	// A window that held none of those would still be a correct world.
	waterAreaOriginX = 6144
	waterAreaOriginZ = -2048
	waterAreaSize    = 1024
	waterAreaStep    = 8

	// The band the surface water has to land in. Below it water is a curiosity
	// somebody may never walk past; above it the map is a sea with islands. See
	// seaLevel for the measurements that chose 47, and for the spread across six
	// windows spanning sixteen thousand blocks.
	minWaterPercent = 3
	maxWaterPercent = 15
)

// TestWaterCoversItsShareOfTheWorld is the number seaLevel exists to produce,
// measured rather than asserted in a comment — and, in the same sweep, the two
// existence claims that go with it: a tundra column wearing ice at the sea line, and
// a river channel.
func TestWaterCoversItsShareOfTheWorld(t *testing.T) {
	t.Parallel()

	columns, wet, ice, rivers, beaches := 0, 0, 0, 0, 0
	for z := int64(waterAreaOriginZ); z < waterAreaOriginZ+waterAreaSize; z += waterAreaStep {
		for x := int64(waterAreaOriginX); x < waterAreaOriginX+waterAreaSize; x += waterAreaStep {
			col := columnAt(waterSeed, x, z)
			columns++

			if col.surface >= seaLevel {
				if col.river {
					t.Fatalf("river column at (%d, %d) has surface %d, at or above the sea line %d", x, z, col.surface, seaLevel)
				}
				continue
			}
			wet++
			if col.river {
				rivers++
			}
			if col.beach {
				beaches++
			}
			if col.fillAt(seaLevel) == Ice {
				ice++
				if col.climate != Tundra {
					t.Fatalf("%v column at (%d, %d) wears ice; only a tundra does", col.climate, x, z)
				}
			}
		}
	}

	if percent := wet * 100 / columns; percent < minWaterPercent || percent > maxWaterPercent {
		t.Errorf("%d of %d columns stand in water (%d%%), outside the designed [%d%%, %d%%]",
			wet, columns, percent, minWaterPercent, maxWaterPercent)
	}
	if ice == 0 {
		t.Error("no tundra column in the sample wears ice at the sea line")
	}
	if rivers == 0 {
		t.Error("no river channel crosses the sample window")
	}
	if beaches == 0 {
		t.Error("no shore in the sample window: every water column meets its land at grass")
	}
}

// A channel is a curve, so it has a length: every column of one is joined to the
// next, and following the joins from anywhere in a channel leads a long way.
//
// **The test walks a channel rather than counting channel columns**, because a count
// cannot tell a river from a scatter of puddles that happen to satisfy the same
// predicate. It floods outward from the first channel column it finds, under
// eight-adjacency — a river crosses the lattice at an angle, so a four-adjacent walk
// would report a diagonal staircase as a row of disconnected columns — and then
// measures how far the reachable set *reaches*. Extent rather than size, because a
// wide pond and a long river hold the same number of columns.
func TestARiverIsContinuousAlongItsCourse(t *testing.T) {
	t.Parallel()

	// How far a channel found in this window has to run before it is a river rather
	// than a pond. Two chunks is well under riverScaleBlocks, so this is a claim
	// about connectivity and not about the scale the field was sampled at.
	const minimumCourse = 64

	start, found := firstRiverColumn(t)
	if !found {
		t.Fatal("no river column in the sample window, so nothing here was walked")
	}

	reached := walkChannel(start)
	minX, maxX, minZ, maxZ := start[0], start[0], start[1], start[1]
	for at := range reached {
		minX, maxX = min(minX, at[0]), max(maxX, at[0])
		minZ, maxZ = min(minZ, at[1]), max(maxZ, at[1])
	}
	if course := max(maxX-minX, maxZ-minZ); course < minimumCourse {
		t.Errorf("the channel through (%d, %d) reaches %d columns spanning %d blocks before it ends, want a course of at least %d",
			start[0], start[1], len(reached), course, minimumCourse)
	}

	// And no channel column anywhere in the window stands on its own.
	isolated := 0
	for z := int64(waterAreaOriginZ); z < waterAreaOriginZ+waterAreaSize; z += waterAreaStep {
		for x := int64(waterAreaOriginX); x < waterAreaOriginX+waterAreaSize; x += waterAreaStep {
			if !columnAt(waterSeed, x, z).river {
				continue
			}
			if !hasRiverNeighbour(x, z) {
				isolated++
			}
		}
	}
	if isolated != 0 {
		t.Errorf("%d river columns in the window have no river neighbour: the channel is a scatter, not a course", isolated)
	}
}

func firstRiverColumn(t *testing.T) ([2]int64, bool) {
	t.Helper()

	for z := int64(waterAreaOriginZ); z < waterAreaOriginZ+waterAreaSize; z++ {
		for x := int64(waterAreaOriginX); x < waterAreaOriginX+waterAreaSize; x++ {
			if columnAt(waterSeed, x, z).river {
				return [2]int64{x, z}, true
			}
		}
	}
	return [2]int64{}, false
}

// walkChannel is every column reachable from start by stepping between adjacent
// channel columns, bounded to the sample window so the walk terminates on a world
// that has no edge.
func walkChannel(start [2]int64) map[[2]int64]bool {
	reached := map[[2]int64]bool{start: true}
	queue := [][2]int64{start}
	for len(queue) > 0 {
		at := queue[0]
		queue = queue[1:]
		for _, step := range neighbourOffsets {
			next := [2]int64{at[0] + step[0], at[1] + step[1]}
			if reached[next] || !insideSampleWindow(next) || !columnAt(waterSeed, next[0], next[1]).river {
				continue
			}
			reached[next] = true
			queue = append(queue, next)
		}
	}
	return reached
}

func insideSampleWindow(at [2]int64) bool {
	return at[0] >= waterAreaOriginX && at[0] < waterAreaOriginX+waterAreaSize &&
		at[1] >= waterAreaOriginZ && at[1] < waterAreaOriginZ+waterAreaSize
}

func hasRiverNeighbour(x, z int64) bool {
	for _, step := range neighbourOffsets {
		if columnAt(waterSeed, x+step[0], z+step[1]).river {
			return true
		}
	}
	return false
}

// The eight columns around one, which is the adjacency a channel crossing the
// lattice at an angle is connected under. Four-adjacency would report a diagonal
// staircase as a row of disconnected columns.
var neighbourOffsets = [8][2]int64{
	{1, 0}, {-1, 0}, {0, 1}, {0, -1},
	{1, 1}, {1, -1}, {-1, 1}, {-1, -1},
}

// A river bed is cut to one height, is gravel on top, and carries water to the sea
// line — and it exists only where the land it crosses is low enough.
func TestARiverBedIsCutToOneHeightUnderTheSeaLine(t *testing.T) {
	t.Parallel()

	channels := 0
	for z := int64(waterAreaOriginZ); z < waterAreaOriginZ+waterAreaSize; z += waterAreaStep {
		for x := int64(waterAreaOriginX); x < waterAreaOriginX+waterAreaSize; x += waterAreaStep {
			col := columnAt(waterSeed, x, z)
			if !col.river {
				continue
			}
			channels++

			if col.surface != seaLevel-riverBedDrop {
				t.Fatalf("river bed at (%d, %d) is at %d, want %d", x, z, col.surface, seaLevel-riverBedDrop)
			}
			if got := col.blockAt(col.surface); got != Gravel {
				t.Fatalf("river bed at (%d, %d) is block %d, want Gravel", x, z, got)
			}
			for y := col.surface + 1; y < seaLevel; y++ {
				if got := col.voxelAt(waterSeed, x, int64(y), z); got != Water {
					t.Fatalf("the channel at (%d, %d) holds %d at y=%d, want Water", x, z, got, y)
				}
			}
			if got := col.voxelAt(waterSeed, x, seaLevel+1, z); got != Air {
				t.Fatalf("the channel at (%d, %d) holds %d one block over the sea line", x, z, got)
			}
		}
	}
	if channels == 0 {
		t.Fatal("no channel in the sample window, so nothing here was checked")
	}

	// Rivers stop where the land climbs, and the limit is read from the *unlowered*
	// height. Sweeping the field directly is the only way to see the columns the
	// height rule rejected — a rejected column is an ordinary one afterwards, and
	// says nothing about why.
	rejected := 0
	for z := int64(waterAreaOriginZ); z < waterAreaOriginZ+waterAreaSize; z++ {
		for x := int64(waterAreaOriginX); x < waterAreaOriginX+waterAreaSize; x++ {
			if !riverAt(waterSeed, x, z) || nearSpawnColumn(x, z) {
				continue
			}
			nx := floorDiv(x<<fracBits, terrainScaleBlocks)
			nz := floorDiv(z<<fracBits, terrainScaleBlocks)
			base := baseHeight + int((amplitudeAt(waterSeed, x, z)*(fbm2D(waterSeed, nx, nz)-one/2))>>fracBits)
			if base <= riverMaxSurface {
				continue
			}
			rejected++
			if columnAt(waterSeed, x, z).river {
				t.Fatalf("a channel was cut at (%d, %d), where the land stands at %d over the limit %d",
					x, z, base, riverMaxSurface)
			}
		}
	}
	if rejected == 0 {
		t.Error("the field never met high ground in this window, so the climb rule was never exercised")
	}
}

// A basin lowers the ground and nothing else, so the water in one is the same sea
// line filling a hole somebody dug.
func TestABasinLowersTheGroundAndReachesItsFullDepth(t *testing.T) {
	t.Parallel()

	lowered, deepest := 0, 0
	for z := int64(-8192); z < 8192; z += 64 {
		for x := int64(-8192); x < 8192; x += 64 {
			climate := ClimateAt(waterSeed, x, z)
			drop := basinAt(waterSeed, x, z, climate)
			if drop < 0 || drop > basinDepth {
				t.Fatalf("the basin at (%d, %d) lowers by %d, outside [0, %d]", x, z, drop, basinDepth)
			}
			if climate == Desert && drop != 0 {
				t.Fatalf("the desert column at (%d, %d) is lowered by %d; a desert has no basins", x, z, drop)
			}
			if drop > 0 {
				lowered++
			}
			deepest = max(deepest, drop)
		}
	}

	if lowered == 0 {
		t.Fatal("no column in the sweep is lowered by a basin at all")
	}
	if deepest != basinDepth {
		t.Errorf("the deepest basin in the sweep reaches %d blocks, not the %d basinDepth promises: "+
			"the field never gets near basinFullDepth and the smoothstep is flat where it lives",
			deepest, basinDepth)
	}
}

// The fill itself: what stands in an air voxel, and where it stops.
func TestTheSeaLineFillsToItsOwnHeightAndNoHigher(t *testing.T) {
	t.Parallel()

	// A submerged column of each climate, found rather than constructed, so the
	// blocks are the ones the generator would actually produce there.
	for _, climate := range []Climate{Plains, Taiga, Tundra, Desert} {
		x, z, found := findSubmergedColumn(climate)
		if !found {
			t.Logf("no submerged %v column in the search area", climate)
			continue
		}
		col := columnAt(waterSeed, x, z)

		for y := col.surface + 1; y <= seaLevel; y++ {
			want := Water
			if climate == Tundra && y == seaLevel {
				want = Ice
			}
			if got := col.voxelAt(waterSeed, x, int64(y), z); got != want {
				t.Errorf("%v column at (%d, %d) holds %d at y=%d, want %d", climate, x, z, got, y, want)
			}
		}
		for y := seaLevel + 1; y <= seaLevel+4; y++ {
			if got := col.voxelAt(waterSeed, x, int64(y), z); got != Air {
				t.Errorf("%v column at (%d, %d) holds %d at y=%d, above the sea line", climate, x, z, got, y)
			}
		}
		if got := col.voxelAt(waterSeed, x, int64(col.surface), z); !Solid(got) {
			t.Errorf("%v column at (%d, %d) has %d at its own surface %d, which is not ground",
				climate, x, z, got, col.surface)
		}
	}
}

func findSubmergedColumn(climate Climate) (x, z int64, found bool) {
	for z := int64(-4096); z < 8192; z += 8 {
		for x := int64(-4096); x < 8192; x += 8 {
			col := columnAt(waterSeed, x, z)
			if col.climate == climate && col.surface < seaLevel && !col.river {
				return x, z, true
			}
		}
	}
	return 0, 0, false
}

// Underground water follows two hydrostatic surfaces: caveWaterLevel under dry
// ground, and the standing surface of a sea, basin or river column. It reaches only
// carved terrain in either case.
func TestCaveWaterStandsOnlyInCarvedVoxelsBelowItsLevel(t *testing.T) {
	t.Parallel()

	deep, standing, dry := 0, 0, 0
	chunk := Generate(waterSeed, goldenWaterCoord)
	originX, originY, originZ := goldenWaterCoord.Origin()

	for z := range ChunkSize {
		for x := range ChunkSize {
			worldX, worldZ := originX+int64(x), originZ+int64(z)
			col := columnAt(waterSeed, worldX, worldZ)
			for y := range ChunkSize {
				worldY := originY + int64(y)
				if worldY > int64(col.surface) {
					continue // above the ground; the sea line owns this voxel
				}
				carved := col.carvedAt(waterSeed, worldX, worldY, worldZ)
				got := chunk.At(x, y, z)
				wantWater := worldY <= caveWaterLevel ||
					col.standingWater && worldY <= int64(col.waterSurface)

				switch {
				case !carved:
					if got == Water {
						t.Fatalf("water at (%d, %d, %d) stands in uncarved terrain", worldX, worldY, worldZ)
					}
				case wantWater:
					if got != Water {
						t.Fatalf("carved voxel (%d, %d, %d) under its hydrostatic surface is %d, want Water",
							worldX, worldY, worldZ, got)
					}
					if worldY <= caveWaterLevel {
						deep++
					} else {
						standing++
					}
				default:
					if got == Water {
						t.Fatalf("carved voxel (%d, %d, %d) above every hydrostatic surface holds water",
							worldX, worldY, worldZ)
					}
					dry++
				}
			}
		}
	}
	if deep == 0 || standing == 0 || dry == 0 {
		t.Fatalf("the chunk held %d deep, %d standing-water and %d dry carved voxels; every rule must be exercised",
			deep, standing, dry)
	}
}

func TestStandingWaterLeavesNoCarvedAirBelowItsSurface(t *testing.T) {
	t.Parallel()

	const scanSize = 256
	wetColumns, carvedInBand := 0, 0
	for z := int64(waterAreaOriginZ); z < waterAreaOriginZ+scanSize; z++ {
		for x := int64(waterAreaOriginX); x < waterAreaOriginX+scanSize; x++ {
			col := columnAt(waterSeed, x, z)
			if col.surface >= seaLevel {
				continue
			}
			wetColumns++
			for y := int64(caveWaterLevel + 1); y <= seaLevel; y++ {
				if !col.carvedAt(waterSeed, x, y, z) {
					continue
				}
				carvedInBand++
				if got := col.voxelAt(waterSeed, x, y, z); got == Air {
					t.Fatalf("standing-water column (%d, %d), surface %d, leaves carved Air at y=%d",
						x, z, col.waterSurface, y)
				}
			}
		}
	}
	if wetColumns == 0 {
		t.Fatal("the 256x256 sample contains no lowered column below seaLevel")
	}
	if carvedInBand == 0 {
		t.Fatal("the 256x256 sample contains no standing-water column carved between caveWaterLevel and seaLevel")
	}
}

// A shore is sand, on both sides of the water line, and only where a climate has
// soil to replace.
func TestABeachIsSandOnEitherSideOfTheWaterLine(t *testing.T) {
	t.Parallel()

	shores := 0
	for z := int64(waterAreaOriginZ); z < waterAreaOriginZ+waterAreaSize; z += waterAreaStep {
		for x := int64(waterAreaOriginX); x < waterAreaOriginX+waterAreaSize; x += waterAreaStep {
			col := columnAt(waterSeed, x, z)
			if !col.beach {
				if beachAt(col.surface, col.climate) {
					t.Fatalf("the column at (%d, %d) is in the shore band and is not a beach", x, z)
				}
				continue
			}
			shores++

			if col.climate != Plains && col.climate != Taiga {
				t.Fatalf("a %v column at (%d, %d) is a beach; only soil climates have one", col.climate, x, z)
			}
			if col.surface < seaLevel-beachBelowSea || col.surface > seaLevel+beachAboveSea {
				t.Fatalf("the beach at (%d, %d) stands at %d, outside the band around %d", x, z, col.surface, seaLevel)
			}
			for depth := range beachDepth {
				if got := col.blockAt(col.surface - depth); got != Sand {
					t.Fatalf("the beach at (%d, %d) holds %d at depth %d, want Sand", x, z, got, depth)
				}
			}
			if got := col.blockAt(col.surface - beachDepth); got == Sand {
				t.Fatalf("the beach at (%d, %d) reaches depth %d", x, z, beachDepth)
			}
		}
	}
	if shores == 0 {
		t.Fatal("no shore in the sample window, so nothing here was checked")
	}
}

// Nothing roots under water, and the guard is a rule about the column rather than a
// property the grass test happens to deliver.
func TestNoConiferRootsUnderTheSeaLine(t *testing.T) {
	t.Parallel()

	submerged := 0
	for z := int64(-2048); z < 2048; z += 4 {
		for x := int64(-2048); x < 2048; x += 4 {
			col := columnAt(waterSeed, x, z)
			if col.surface >= seaLevel {
				continue
			}
			submerged++
			if _, ok := treeAtColumn(waterSeed, x, z, col); ok {
				t.Fatalf("a conifer roots at (%d, %d), whose surface %d is under the sea line %d",
					x, z, col.surface, seaLevel)
			}
		}
	}
	if submerged == 0 {
		t.Fatal("the sweep found no submerged column, so the guard was never exercised")
	}
}

// An edit into generated water is one delta over a deterministic base.
//
// **That is the invariant the whole design rests on**: the Fimbulvetr storm
// regenerates an unprotected chunk to its *original procedural state* by discarding
// deltas, which only works while the base is a pure function of the seed. Runtime
// flow may add neighbouring deltas later; this direct placement still changes one
// voxel and leaves `Generate` answering the original source at that coordinate.
func TestAnEditIntoGeneratedWaterIsOneDeltaOverAnUnchangedBase(t *testing.T) {
	t.Parallel()

	chunks := NewCache(waterSeed, 1, 8)
	x, y, z, found := findGeneratedWaterVoxel(t, chunks)
	if !found {
		t.Fatal("no water voxel in the water golden chunk; the fixture no longer holds what it was chosen for")
	}

	if err := chunks.Apply(context.Background(), x, y, z, Stone, nil); err != nil {
		t.Fatalf("place Stone into water: %v", err)
	}
	if got, err := chunks.BlockAt(context.Background(), x, y, z); err != nil || got != Stone {
		t.Fatalf("the composed voxel is block %d (err %v), want Stone", got, err)
	}

	base := Generate(waterSeed, ChunkOf(x, y, z))
	if got := base.At(Local(x), Local(y), Local(z)); got != Water {
		t.Errorf("the generated base at (%d, %d, %d) is block %d, want the Water it started as", x, y, z, got)
	}
	if got := chunks.deltas.Count(); got != 1 {
		t.Errorf("the placement recorded %d deltas, want exactly 1", got)
	}
}

func findGeneratedWaterVoxel(t *testing.T, chunks *Cache) (x, y, z int64, found bool) {
	t.Helper()

	chunk := Generate(waterSeed, goldenWaterCoord)
	originX, originY, originZ := goldenWaterCoord.Origin()
	for localY := range ChunkSize {
		for localZ := range ChunkSize {
			for localX := range ChunkSize {
				if chunk.At(localX, localY, localZ) == Water {
					return originX + int64(localX), originY + int64(localY), originZ + int64(localZ), true
				}
			}
		}
	}
	return 0, 0, 0, false
}
