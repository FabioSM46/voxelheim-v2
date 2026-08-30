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

	// **The count is standingWater rather than `surface < seaLevel`, and since #595
	// those are different sets.** A river above the sea line is a column standing in
	// water whose ground is not under the sea, which is what this statistic is about.
	// Under the old fixed bed every channel was below the line and the two agreed.
	columns, wet, ice, rivers, beaches := 0, 0, 0, 0, 0
	for z := int64(waterAreaOriginZ); z < waterAreaOriginZ+waterAreaSize; z += waterAreaStep {
		for x := int64(waterAreaOriginX); x < waterAreaOriginX+waterAreaSize; x += waterAreaStep {
			col := columnAt(waterSeed, x, z)
			columns++

			if !col.standingWater {
				continue
			}
			wet++
			if col.river {
				rivers++
			}
			if col.beach {
				beaches++
			}
			if col.fillAt(col.waterSurface) == Ice {
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

// A river surface is the land it crosses, quantised to a terrace; its bed sits
// riverBedDrop under that and never above the ground; and the channel between them is
// full of water that knows which way it runs.
//
// **The old test asserted one number for every bed in the world and is gone**, because
// `surface == seaLevel - riverBedDrop` is exactly the property #595 removed. What
// replaces it is the definition, recomputed per column, plus the two bounds that make a
// terraced bed a channel rather than an embankment: the surface is a multiple of the
// step, and the bed is never lifted above the land it is cut into.
func TestARiverSurfaceIsTerracedAndItsBedFollowsTheLand(t *testing.T) {
	t.Parallel()

	// The old riverMaxSurface cap, kept here as a number rather than as a constant
	// because the constant is deleted: the point of the sweep below is that channels
	// now exist above it.
	const removedMaxSurface = seaLevel + 24

	channels, aboveOldCap, deepened, highest := 0, 0, 0, 0
	steps, maxStep := 0, 0
	previous := map[int64]column{}
	for z := int64(waterAreaOriginZ); z < waterAreaOriginZ+waterAreaSize; z += waterAreaStep {
		for x := int64(waterAreaOriginX); x < waterAreaOriginX+waterAreaSize; x += waterAreaStep {
			col := columnAt(waterSeed, x, z)
			last, hadLast := previous[x]
			previous[x] = col
			if !col.river {
				continue
			}
			channels++

			surface := riverSurfaceAt(waterSeed, x, z)
			base := unloweredHeightAt(waterSeed, x, z)
			switch {
			case col.waterSurface != surface:
				t.Fatalf("the channel at (%d, %d) stands at %d, want its terrace %d", x, z, col.waterSurface, surface)
			case surface%riverTerraceStep != 0:
				t.Fatalf("the channel at (%d, %d) stands at %d, which is not a multiple of the %d-block terrace",
					x, z, surface, riverTerraceStep)
			case surface < seaLevel:
				t.Fatalf("the channel at (%d, %d) stands at %d, under the sea line %d, where the sea owns the column",
					x, z, surface, seaLevel)
			case col.surface > base:
				t.Fatalf("the bed at (%d, %d) is at %d, above the land's own %d: the river is on an embankment",
					x, z, col.surface, base)
			case col.surface != min(surface-riverBedDrop, base):
				t.Fatalf("the bed at (%d, %d) is at %d, want min(terrace-drop, land) = %d",
					x, z, col.surface, min(surface-riverBedDrop, base))
			case col.blockAt(col.surface) != Gravel:
				t.Fatalf("the bed at (%d, %d) is block %d, want Gravel", x, z, col.blockAt(col.surface))
			}

			if col.surface < surface-riverBedDrop {
				deepened++
			}
			if base > removedMaxSurface {
				aboveOldCap++
			}
			highest = max(highest, surface)

			// Water from the bed to the terrace, and air over it. Every voxel of it
			// carries the column's current — a channel is a source with a direction all
			// the way down, which is what the flow automaton pours over a step and what
			// #597 reads to push a swimmer.
			for y := col.surface + 1; y <= surface; y++ {
				want := col.waterBlock
				if col.climate == Tundra && y == surface {
					want = Ice
				}
				if got := col.voxelAt(waterSeed, x, int64(y), z); got != want {
					t.Fatalf("the channel at (%d, %d) holds %d at y=%d, want %d", x, z, got, y, want)
				}
			}
			if got := col.voxelAt(waterSeed, x, int64(surface)+1, z); got != Air {
				t.Fatalf("the channel at (%d, %d) holds %d one block over its terrace %d", x, z, got, surface)
			}

			// A terrace step between two sampled channel columns. The samples are
			// waterAreaStep apart, so this counts how often a river changes level
			// rather than measuring one fall; that is the test below, at true
			// adjacency.
			if hadLast && last.river && last.waterSurface != col.waterSurface {
				steps++
				maxStep = max(maxStep, absInt(col.waterSurface-last.waterSurface))
			}
		}
	}

	if channels == 0 {
		t.Fatal("no channel in the sample window, so nothing here was checked")
	}
	if aboveOldCap == 0 {
		t.Fatalf("no channel in the window crosses land above %d, so removing riverMaxSurface changed nothing here",
			removedMaxSurface)
	}
	if highest < seaLevel+32 {
		t.Errorf("the highest channel in the window stands at %d; a river that reaches the highlands wants at least %d",
			highest, seaLevel+32)
	}
	if steps < minimumTerraceSteps {
		t.Errorf("%d terrace changes between sampled channel columns, want at least %d: the surface is not stepping with the land",
			steps, minimumTerraceSteps)
	}
	t.Logf("measured %d channel columns, %d over the removed cap, %d with a bed deepened onto lower ground, "+
		"highest terrace %d, %d terrace changes with a maximum of %d blocks",
		channels, aboveOldCap, deepened, highest, steps, maxStep)
}

// minimumTerraceSteps is how many level changes the sample window must hold before
// "terraced" is a claim about the world rather than about the arithmetic. Twenty is the
// acceptance criterion; the window measures hundreds.
const minimumTerraceSteps = 20

// Where two adjacent channel columns stand at different terraces the difference is a
// whole number of steps, and the higher one's water faces air over the lower one.
//
// **That air is the whole of what the generator owes a waterfall.** Nothing here paints
// falling water; what generation guarantees is that there is somewhere to pour — the
// lower terrace's water is not raised to meet the higher one, and the higher channel's
// wall is not extended to close the gap.
func TestATerraceStepIsAFallOverOpenAir(t *testing.T) {
	t.Parallel()

	// A contiguous window, because this is the one claim needing true adjacency: a
	// sample every eight blocks reports a slope as a cliff. Smaller than the statistics
	// window for the same reason it is contiguous — 65536 columns, each resolving its
	// neighbour.
	const scanSize = 256

	pairs, steps, maxDrop := 0, 0, 0
	for z := int64(waterAreaOriginZ); z < waterAreaOriginZ+scanSize; z++ {
		for x := int64(waterAreaOriginX); x < waterAreaOriginX+scanSize; x++ {
			col := columnAt(waterSeed, x, z)
			if !col.river {
				continue
			}
			for _, step := range [2][2]int64{{1, 0}, {0, 1}} {
				nextX, nextZ := x+step[0], z+step[1]
				next := columnAt(waterSeed, nextX, nextZ)
				if !next.river {
					continue
				}
				pairs++
				drop := col.waterSurface - next.waterSurface
				if drop == 0 {
					continue
				}
				steps++
				maxDrop = max(maxDrop, absInt(drop))
				if absInt(drop)%riverTerraceStep != 0 {
					t.Fatalf("adjacent channels (%d, %d) at %d and (%d, %d) at %d differ by %d, not a whole %d-block terrace",
						x, z, col.waterSurface, nextX, nextZ, next.waterSurface, drop, riverTerraceStep)
				}

				// The lower column, over its own water and under the higher one's, must
				// be open air for the fall to land in.
				lower, lowerX, lowerZ := next, nextX, nextZ
				higher := col
				if drop < 0 {
					lower, lowerX, lowerZ, higher = col, x, z, next
				}
				for y := lower.waterSurface + 1; y <= higher.waterSurface; y++ {
					if got := lower.voxelAt(waterSeed, lowerX, int64(y), lowerZ); got != Air {
						t.Fatalf("the fall from %d onto (%d, %d) at %d meets %d at y=%d, not the air it needs",
							higher.waterSurface, lowerX, lowerZ, lower.waterSurface, got, y)
					}
				}
			}
		}
	}

	if pairs == 0 {
		t.Fatal("no two adjacent channel columns in the window, so no step was checked")
	}
	if steps == 0 {
		t.Error("every adjacent pair of channel columns in the window stands at one level: the river is still a canal")
	}
	t.Logf("measured %d adjacent channel pairs, %d of them a terrace step, tallest %d blocks", pairs, steps, maxDrop)
}

// Every channel column runs the way its own field and its own slope point, and the
// definition is recomputed here rather than read back from the generator.
//
// The field is spelled out from fbm2D rather than through climateField, so this is a
// second reading of the river field and not the same call under another name. What it
// cannot be is independent of the *rule* — the rule is what the acceptance criterion
// names — so what it catches is the plumbing: a column resolved at the wrong
// coordinate, an axis swapped on the way to a block id, a sign lost.
func TestEveryRiverColumnRunsTheWayItsFieldAndSlopePoint(t *testing.T) {
	t.Parallel()

	seen := map[Block]int{}
	channels, opposed := 0, 0
	for z := int64(waterAreaOriginZ); z < waterAreaOriginZ+waterAreaSize; z += waterAreaStep {
		for x := int64(waterAreaOriginX); x < waterAreaOriginX+waterAreaSize; x += waterAreaStep {
			col := columnAt(waterSeed, x, z)
			if !col.river {
				continue
			}
			channels++
			seen[col.waterBlock]++

			dx, dz := currentByDefinition(x, z)
			if gotX, gotZ := CurrentOf(col.waterBlock); gotX != dx || gotZ != dz {
				t.Fatalf("the channel at (%d, %d) carries block %d, running (%d, %d), want (%d, %d)",
					x, z, col.waterBlock, gotX, gotZ, dx, dz)
			}

			// A neighbour running the other way is permitted and counted. Each column
			// samples its own two ends, so two reaches converge into a hollow and
			// diverge over a ridge — water pools at one and divides at the other.
			for _, step := range [2][2]int64{{waterAreaStep, 0}, {0, waterAreaStep}} {
				next := columnAt(waterSeed, x+step[0], z+step[1])
				if !next.river {
					continue
				}
				nextX, nextZ := CurrentOf(next.waterBlock)
				if dx+nextX == 0 && dz+nextZ == 0 && (dx != 0) == (nextX != 0) {
					opposed++
				}
			}
		}
	}

	if channels == 0 {
		t.Fatal("no channel in the sample window, so no current was checked")
	}
	for _, block := range [4]Block{WaterCurrentXPos, WaterCurrentXNeg, WaterCurrentZPos, WaterCurrentZNeg} {
		if seen[block] == 0 {
			t.Errorf("no channel column in the window runs as block %d; the window sees only %v", block, seen)
		}
	}
	t.Logf("measured %d channel columns running %v, with %d adjacent pairs opposed at a hollow or a ridge",
		channels, seen, opposed)
}

// currentByDefinition is #595's own words, in code: the tangent of the river field,
// quantised to its dominant axis, signed towards the lower of the two smoothed heights
// a slope span away, with a tie falling to +X or +Z.
func currentByDefinition(x, z int64) (dx, dz int) {
	field := func(atX, atZ int64) int64 {
		return fbm2D(waterSeed+riverSeedOffset,
			floorDiv(atX<<fracBits, riverScaleBlocks), floorDiv(atZ<<fracBits, riverScaleBlocks))
	}
	smoothed := func(atX, atZ int64) int {
		sum := 0
		for _, offset := range [5][2]int64{{0, 0}, {-riverSmoothSpan, 0}, {riverSmoothSpan, 0}, {0, -riverSmoothSpan}, {0, riverSmoothSpan}} {
			sum += unloweredHeightAt(waterSeed, atX+offset[0], atZ+offset[1])
		}
		return int(floorDiv(int64(sum), 5))
	}

	// Perpendicular to the gradient (gx, gz) is (-gz, gx), so the tangent lies along X
	// exactly when |gz| is the larger of the two.
	gradientX := field(x+riverGradientSpan, z) - field(x-riverGradientSpan, z)
	gradientZ := field(x, z+riverGradientSpan) - field(x, z-riverGradientSpan)
	if absInt64(gradientZ) >= absInt64(gradientX) {
		if smoothed(x+riverSlopeSpan, z) <= smoothed(x-riverSlopeSpan, z) {
			return 1, 0
		}
		return -1, 0
	}
	if smoothed(x, z+riverSlopeSpan) <= smoothed(x, z-riverSlopeSpan) {
		return 0, 1
	}
	return 0, -1
}

// The four current ids and the four directions are one mapping read from both ends.
// [waterCurrentBlock] is the only thing that writes one and [CurrentOf] the only thing
// that reads one, so a swapped pair would be invisible to every other test in this
// package and perfectly visible to a swimmer.
func TestEveryCurrentIdRoundTripsThroughItsDirection(t *testing.T) {
	t.Parallel()

	for _, want := range [4][2]int{{1, 0}, {-1, 0}, {0, 1}, {0, -1}} {
		block := waterCurrentBlock(want[0], want[1])
		if !IsWater(block) || WaterLevel(block) != 8 {
			t.Fatalf("the current for (%d, %d) is block %d, at water level %d; a current is full source water",
				want[0], want[1], block, WaterLevel(block))
		}
		if gotX, gotZ := CurrentOf(block); gotX != want[0] || gotZ != want[1] {
			t.Errorf("waterCurrentBlock(%d, %d) is %d, which reads back as (%d, %d)",
				want[0], want[1], block, gotX, gotZ)
		}
	}

	// The zero current is the fifth point of the mapping and it is not a fifth id:
	// [CurrentOf] answers (0, 0) for plain [Water], so that is what the constructor
	// owes for (0, 0) if the pair is an inverse rather than a table of four units.
	// Nothing passes it today — [riverCurrentAt] returns a unit step on every arm —
	// which is exactly why only a test holds the arm in place.
	if block := waterCurrentBlock(0, 0); block != Water {
		t.Errorf("waterCurrentBlock(0, 0) is block %d, want plain Water (%d)", block, Water)
	} else if gotX, gotZ := CurrentOf(block); gotX != 0 || gotZ != 0 {
		t.Errorf("waterCurrentBlock(0, 0) reads back as (%d, %d), want (0, 0)", gotX, gotZ)
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
					if IsWater(got) {
						t.Fatalf("water at (%d, %d, %d) stands in uncarved terrain", worldX, worldY, worldZ)
					}
				case wantWater:
					// The family rather than one id: a carved voxel under a channel is
					// that channel's water and carries its current, one under dry ground
					// is an aquifer with no direction, and both are source water.
					want := Water
					if col.standingWater && worldY <= int64(col.waterSurface) {
						want = col.waterBlock
					}
					if got != want {
						t.Fatalf("carved voxel (%d, %d, %d) under its hydrostatic surface is %d, want %d",
							worldX, worldY, worldZ, got, want)
					}
					if worldY <= caveWaterLevel {
						deep++
					} else {
						standing++
					}
				default:
					if IsWater(got) {
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
	wetColumns, riverColumns, carvedInBand, riverCarvedInBand := 0, 0, 0, 0
	for z := int64(waterAreaOriginZ); z < waterAreaOriginZ+scanSize; z++ {
		for x := int64(waterAreaOriginX); x < waterAreaOriginX+scanSize; x++ {
			col := columnAt(waterSeed, x, z)
			if !col.standingWater {
				continue
			}
			wetColumns++
			if col.river {
				riverColumns++
			}
			for y := int64(caveWaterLevel + 1); y <= int64(col.waterSurface); y++ {
				if !col.carvedAt(waterSeed, x, y, z) {
					continue
				}
				carvedInBand++
				if col.river {
					riverCarvedInBand++
				}
				if got := col.voxelAt(waterSeed, x, y, z); got == Air {
					t.Fatalf("standing-water column (%d, %d), surface %d, leaves carved Air at y=%d",
						x, z, col.waterSurface, y)
				}
			}
		}
	}
	if wetColumns == 0 {
		t.Fatal("the 256x256 sample contains no column standing in water")
	}
	if carvedInBand == 0 {
		t.Fatal("the 256x256 sample contains no standing-water column carved between caveWaterLevel and seaLevel")
	}
	if riverColumns == 0 || riverCarvedInBand == 0 {
		t.Fatalf("the 256x256 sample contains %d river columns and %d carved river voxels in the hydrostatic band; both must be non-zero",
			riverColumns, riverCarvedInBand)
	}
	t.Logf("measured %d wet columns, including %d river columns, and %d carved river voxels in the hydrostatic band",
		wetColumns, riverColumns, riverCarvedInBand)
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
				// A river column in the band is the one exemption, and it is
				// [columnAt]'s rather than [beachAt]'s: a terraced bed can land in the
				// shore band, where sand under gravel under three blocks of water is a
				// ditch rather than a shore. The band rule itself is unchanged.
				if beachAt(col.surface, col.climate) && !col.river {
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
