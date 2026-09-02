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
	//   - it is not at the origin, because originWaterClearance exempts a square there
	//     and a window containing it measures the exemption as well as the fields;
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
	t.Logf("measured %d of %d columns standing in water (%d%%): %d rivers, %d beaches and %d ice",
		wet, columns, wet*100/columns, rivers, beaches, ice)
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

// A channel is a band around a curve, and its width is measured in blocks rather
// than inferred from how much of the field it happens to cover. The old field-unit
// threshold made the same knob produce bodies 65, 31 and 15 blocks across at these
// three seeds; a slow midpoint crossing became a lake with a river's terraces in it.
//
// This is a Manhattan distance transform from every non-channel column. A distance
// of one is a bank-adjacent channel column, so its maximum is the channel's discrete
// half-width. Confluences and the gradient's 32-block baseline can widen the nominal
// three-block half-width, but the measured residual stays within this stated bound
// instead of varying by a factor of four between seeds.
func TestRiverChannelsStayWithinTheirBlockWidth(t *testing.T) {
	const (
		originX             = -512
		originZ             = -1512
		sampleSize          = 1024
		maximumHalfWidth    = 2*riverHalfWidthBlocks + 1
		unreachableDistance = 2 * sampleSize
	)

	for _, seed := range []int64{1, waterSeed, 7} {
		distance := make([]int, sampleSize*sampleSize)
		channels := 0
		for z := range sampleSize {
			for x := range sampleSize {
				at := z*sampleSize + x
				if riverAt(seed, int64(originX+x), int64(originZ+z)) {
					distance[at] = unreachableDistance
					channels++
				}
			}
		}
		if channels == 0 {
			t.Fatalf("seed %#x has no channel in the width sample", seed)
		}

		for z := range sampleSize {
			for x := range sampleSize {
				at := z*sampleSize + x
				if x > 0 {
					distance[at] = min(distance[at], distance[at-1]+1)
				}
				if z > 0 {
					distance[at] = min(distance[at], distance[at-sampleSize]+1)
				}
			}
		}
		widest := 0
		for z := sampleSize - 1; z >= 0; z-- {
			for x := sampleSize - 1; x >= 0; x-- {
				at := z*sampleSize + x
				if x+1 < sampleSize {
					distance[at] = min(distance[at], distance[at+1]+1)
				}
				if z+1 < sampleSize {
					distance[at] = min(distance[at], distance[at+sampleSize]+1)
				}
				widest = max(widest, distance[at])
			}
		}

		if widest > maximumHalfWidth {
			t.Errorf("seed %#x has a channel half-width of %d blocks, want at most %d for the nominal %d-block half-width",
				seed, widest, maximumHalfWidth, riverHalfWidthBlocks)
		}
		t.Logf("seed %#x: %d channel columns, widest half-width %d blocks", seed, channels, widest)
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
			// Over its own terrace: air, unless the channel next door stands higher and
			// pours in, in which case the fall #654 paints stands there instead. The
			// column's own [column.fallSurface] is which of the two, and it is never
			// below its terrace.
			overTerrace := col.voxelAt(waterSeed, x, int64(surface)+1, z)
			switch {
			case col.fallSurface > surface:
				if overTerrace != WaterFlow7 {
					t.Fatalf("the channel at (%d, %d) is poured into from %d but holds %d over its terrace %d, want %d",
						x, z, col.fallSurface, overTerrace, surface, WaterFlow7)
				}
			case overTerrace != Air:
				t.Fatalf("the channel at (%d, %d) holds %d one block over its terrace %d with nothing pouring in",
					x, z, overTerrace, surface)
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

// Every shared face between two channel terraces carries a fall. A source current says
// where the automaton supplies water; it does not make a perpendicular source face stop
// being a wall of permanent water. The fall is flowing water so the automaton still owns
// it, and [TestTheSeedOneCascadeRejectsSidewaysTerraceCurtains] separately bounds its width.
func TestEveryTerraceFaceCarriesItsFall(t *testing.T) {
	t.Parallel()

	// A contiguous window, because this is the one claim needing true adjacency: a
	// sample every eight blocks reports a slope as a cliff. Smaller than the statistics
	// window for the same reason it is contiguous — 65536 columns, each resolving its
	// neighbour.
	const scanSize = 512

	channels, higherEdges := 0, 0
	for z := int64(waterAreaOriginZ); z < waterAreaOriginZ+scanSize; z++ {
		for x := int64(waterAreaOriginX); x < waterAreaOriginX+scanSize; x++ {
			col := columnAt(waterSeed, x, z)
			if !col.river {
				continue
			}
			channels++
			wantTop := col.waterSurface
			for _, step := range [4][2]int64{{1, 0}, {-1, 0}, {0, 1}, {0, -1}} {
				nextX, nextZ := x+step[0], z+step[1]
				if !riverAt(waterSeed, nextX, nextZ) {
					continue
				}
				surface := riverSurfaceAt(waterSeed, nextX, nextZ)
				if surface < seaLevel || surface <= col.waterSurface {
					continue
				}
				higherEdges++
				drop := surface - col.waterSurface
				if drop%riverTerraceStep != 0 {
					t.Fatalf("adjacent channels (%d, %d) at %d and (%d, %d) at %d differ by %d, not a whole %d-block terrace",
						x, z, col.waterSurface, nextX, nextZ, surface, drop, riverTerraceStep)
				}
				wantTop = max(wantTop, surface)
			}

			if col.fallSurface != wantTop {
				t.Fatalf("fall top at (%d, %d) = %d, want highest adjacent terrace %d",
					x, z, col.fallSurface, wantTop)
			}
			for y := col.waterSurface + 1; y <= wantTop; y++ {
				if got := col.voxelAt(waterSeed, x, int64(y), z); got != WaterFlow7 {
					t.Fatalf("downstream fall onto (%d, %d) holds %d at y=%d, want %d",
						x, z, got, y, WaterFlow7)
				}
			}
			if got := col.voxelAt(waterSeed, x, int64(wantTop)+1, z); got != Air {
				t.Fatalf("fall onto (%d, %d) holds %d one block over its top %d, want Air",
					x, z, got, wantTop)
			}
		}
	}

	if channels == 0 || higherEdges == 0 {
		t.Fatalf("window measured channels=%d higher edges=%d; a terrace fall must be present",
			channels, higherEdges)
	}
	t.Logf("measured %d channels and %d higher terrace edges; every shared face carries its fall",
		channels, higherEdges)
}

// The seed-one cascade reported in #696, measured again after #784 narrowed the channel.
// Restoring every terrace face is allowed only while it does not restore that broad
// curtain: in this 128x128 window around the player report, the eight fall columns form
// three components, the largest containing five columns. Seven is the channel-width
// sweep's confluence bound, so a connected curtain larger than the channel itself is a
// regression.
func TestTheSeedOneCascadeRejectsSidewaysTerraceCurtains(t *testing.T) {
	t.Parallel()

	const (
		seed              int64 = 1
		originX                 = 0
		originZ                 = -1184
		scanSize                = 128
		maxCurtainColumns       = 2*riverHalfWidthBlocks + 1
	)
	type fallColumn struct{ x, z int64 }
	fallColumns := make(map[fallColumn]bool)
	fallVoxels := 0
	for z := int64(originZ); z < originZ+scanSize; z++ {
		for x := int64(originX); x < originX+scanSize; x++ {
			col := columnAt(seed, x, z)
			if col.fallSurface <= col.waterSurface {
				continue
			}
			fallColumns[fallColumn{x, z}] = true
			fallVoxels += col.fallSurface - col.waterSurface
		}
	}
	if len(fallColumns) == 0 {
		t.Fatal("the reported cascade window holds no generated fall, so no curtain was measured")
	}

	seen := make(map[fallColumn]bool, len(fallColumns))
	components, largest := 0, 0
	for start := range fallColumns {
		if seen[start] {
			continue
		}
		components++
		seen[start] = true
		queue, width := []fallColumn{start}, 0
		for len(queue) > 0 {
			at := queue[0]
			queue = queue[1:]
			width++
			for _, step := range [4][2]int64{{1, 0}, {-1, 0}, {0, 1}, {0, -1}} {
				next := fallColumn{at.x + step[0], at.z + step[1]}
				if fallColumns[next] && !seen[next] {
					seen[next] = true
					queue = append(queue, next)
				}
			}
		}
		largest = max(largest, width)
	}
	if largest > maxCurtainColumns {
		t.Errorf("the reported cascade has a connected %d-column fall curtain, larger than the channel's %d-column confluence bound",
			largest, maxCurtainColumns)
	}
	if len(fallColumns) != 8 || fallVoxels != 32 || components != 3 || largest != 5 {
		t.Errorf("reported cascade measured columns=%d voxels=%d components=%d largest=%d, want 8, 32, 3, 5",
			len(fallColumns), fallVoxels, components, largest)
	}
	t.Logf("reported cascade: %d fall columns, %d voxels, %d components, largest %d columns",
		len(fallColumns), fallVoxels, components, largest)
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
// carved terrain in either case, and since #660 only the carved terrain the carved
// run itself reaches.
//
// **The chunk moved off the golden water fixture at #660, and that is what the narrowed
// rule cost.** The standing-water arm needs a carved voxel above [caveWaterLevel] that
// a body still fills, which now means a cave mouth in the bed of a lake — the case #593
// was written for, rare rather than universal: of 968 carved voxels in that band over a
// 192x192 window at seed 1, 55 are reached by the run and 913 are sealed pockets that
// are now air. The golden water chunk holds none of the 55, so this sweeps
// [bodyCaveMouthCoord], which holds 145 of them beside 718 deep and 778 dry voxels.
// Dropping the assertion instead would have left #593 with no coverage at all on the
// day it stopped being everywhere.
func TestCaveWaterStandsOnlyInCarvedVoxelsBelowItsLevel(t *testing.T) {
	t.Parallel()

	deep, standing, dry := 0, 0, 0
	chunk := Generate(waterSeed, bodyCaveMouthCoord)
	originX, originY, originZ := bodyCaveMouthCoord.Origin()

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
				// **A body reaches only what the carved run reaches, since #660.**
				// Below caveWaterLevel the aquifer fills every carved voxel whatever
				// stands above it; above that line the water is the body's own, and a
				// pocket the run from this column's ground never reaches is sealed rock
				// holding air. See [column.bodyFloor].
				body := col.standingWater && worldY <= int64(col.waterSurface)
				wantWater := worldY <= caveWaterLevel ||
					body && worldY > int64(col.bodyFloor)

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
					if body {
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

	// **The band this checks stops at [caveWaterLevel] for a channel since #654.** A sea
	// or a basin still fills the carved voxels under it, which is the case #593 was
	// written for and is what the first branch below holds. A channel does
	// not: its terrace can stand two hundred blocks up, and filling the rock beneath it to
	// that height put source water against every cave mouth that opened into dry ground.
	// The second branch holds the new rule rather than leaving it merely unasserted, so
	// putting the old fill back fails here rather than only in a measurement.
	//
	// **Which carved voxels a body fills narrowed at #660, and the sea-and-basin branch
	// says so in both directions.** The fill reaches what the carved run from this
	// column's own ground reaches; a pocket sealed inside the rock below it is air, and
	// asserting that it is filled would be asserting the defect back into place.
	const scanSize = 512
	wetColumns, riverColumns, carvedInBand, riverCarvedAbove := 0, 0, 0, 0
	reachedInBand, sealedInBand := 0, 0
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
				got := col.voxelAt(waterSeed, x, y, z)
				if col.river {
					riverCarvedAbove++
					if got != Air {
						t.Fatalf("channel column (%d, %d), terrace %d, fills carved rock at y=%d with %d; a channel carries no aquifer",
							x, z, col.waterSurface, y, got)
					}
					continue
				}
				carvedInBand++
				if y > int64(col.bodyFloor) {
					reachedInBand++
					if got == Air {
						t.Fatalf("standing-water column (%d, %d), surface %d, leaves carved Air at y=%d where its own carved run reaches",
							x, z, col.waterSurface, y)
					}
					continue
				}
				sealedInBand++
				if got != Air {
					t.Fatalf("standing-water column (%d, %d), surface %d, fills a pocket at y=%d with %d that its carved run never reaches",
						x, z, col.waterSurface, y, got)
				}
			}
			// And below the dry-world level every column still fills, channel or not:
			// that rule is untouched.
			for y := int64(caveWaterLevel - 3); y <= int64(caveWaterLevel); y++ {
				if !col.carvedAt(waterSeed, x, y, z) {
					continue
				}
				if got := col.voxelAt(waterSeed, x, y, z); got == Air {
					t.Fatalf("column (%d, %d) leaves carved Air at y=%d, under caveWaterLevel %d",
						x, z, y, caveWaterLevel)
				}
			}
		}
	}
	if riverColumns == 0 || riverCarvedAbove == 0 {
		t.Fatalf("the sample held %d channel columns and %d carved voxels above caveWaterLevel under one; the new rule was never exercised",
			riverColumns, riverCarvedAbove)
	}

	if wetColumns == 0 {
		t.Fatal("the sample contains no column standing in water")
	}
	// `carvedInBand` is reported rather than required. The band it counts is the eleven
	// blocks between [caveWaterLevel] and [seaLevel], and only a sea or basin column can
	// be carved inside it — a thin slice that this window happens not to contain now that
	// channels are counted separately. That branch is exercised by
	// [TestCaveWaterStandsOnlyInCarvedVoxelsBelowItsLevel], which requires all three of
	// the fill rules to fire; what must be exercised *here* is the rule this test was
	// rewritten for.
	t.Logf("measured %d wet columns, including %d channel columns; %d carved voxels under a body (%d reached by its run, %d sealed) and %d left dry under a channel",
		wetColumns, riverColumns, carvedInBand, reachedInBand, sealedInBand, riverCarvedAbove)
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

// A settlement stops source water, not the channel beneath it or the fall feeding it.
// This is one of the 117 dry clamps measured across the five settlement windows: the
// settlement ground meets the bed, so no source voxel can stand here, while the raw
// terrace above it remains a flowing upstream face.
func TestASettlementStopsSourceWaterWithoutBreakingTheChannel(t *testing.T) {
	t.Parallel()

	const (
		seed = 0x5EED
		x    = 795
		z    = -3377
	)
	base := unloweredHeightAt(seed, x, z)
	bed, terrace, ok := rawRiverChannelAt(seed, x, z, base)
	if !ok {
		t.Fatal("the measured settlement edge no longer carries the raw channel")
	}
	ground, lower := lowerSettlementGroundAt(seed, x, z, terrace)
	if !lower || ground != bed {
		t.Fatalf("settlement ground = %d (lower %t), want the channel bed %d below terrace %d",
			ground, lower, bed, terrace)
	}

	col := columnAt(seed, x, z)
	if !col.river || col.surface != bed {
		t.Fatalf("column river=%t bed=%d, want the preserved channel at %d", col.river, col.surface, bed)
	}
	if col.waterSurface != bed {
		t.Fatalf("source surface = %d, want clamp at dry bed %d", col.waterSurface, bed)
	}
	if col.fallSurface != terrace {
		t.Fatalf("fall surface = %d, want the upstream terrace %d", col.fallSurface, terrace)
	}
	if got := col.voxelAt(seed, x, int64(bed)+1, z); got != WaterFlow7 {
		t.Fatalf("first voxel over the dry bed is %d, want flowing fall water %d", got, WaterFlow7)
	}
	if surface, channel := channelSurfaceAt(seed, x, z); !channel || surface != terrace {
		t.Fatalf("bank-facing channel surface = %d (channel %t), want raw terrace %d", surface, channel, terrace)
	}
}

// The containment invariant #654 established: no generated source water stands against
// open air on any horizontal side. A source is permanent — [NextWater]'s first arm returns
// it unchanged for ever — while flowing water is the automaton's and a fall deliberately
// has air beside it. A current's heading is irrelevant here: it says where the source
// feeds, not whether the player can see one of its other faces standing in the air.
//
// **Counts, not shares.** A wet window must not earn a proportionally larger allowance,
// and an unnamed exposure is never tolerated. **Every open-country window is now zero**,
// which is what #786 did: the residue this test admitted by name was a channel above the
// ground beside it, 27 voxels in the legacy seed-one window and 181 in the reported one —
// 175 beside a dry bank and 6 beside a lower sea or basin — and [riverBankAt] raises that
// ground to the water it has to hold. Additional smaller windows spread over thousands of
// blocks keep this from being a test of one fortunate reach.
//
// **The zeros are the whole assertion and they are not enough on their own**, for
// TestACarveDoesNotBreachTheWallOfAStandingBody's reason: a world with no rivers in it
// would pass this too. `sources` is compared exactly, so a window that loses its water
// fails here rather than going quiet, and TestARiverChannelStandsBetweenTwoBanks counts
// the columns the bank rule actually raises.
//
// **The final five windows are the settlement edge #828 closes.** A settlement owns
// its columns' ground — the plateau inside its radius and the blend out to the end of
// its band — so [riverBankAt] may not raise the only column that could hold the channel.
// Before the settlement clamp they measured 48, 48, 102, 94 and 51 exposed source
// voxels, all in that named category. The channel now keeps its bed and identity but
// stops source water at the lower settlement ground, taking all five to zero. Exact
// source counts make deleting the channel fail too; the focused test above pins the
// dry channel and flowing upstream face directly.
func TestNoSourceWaterStandsAgainstOpenAir(t *testing.T) {
	t.Parallel()

	type sample struct {
		name               string
		seed               int64
		x, z, width, depth int64
		want               sourceWaterExposure
	}
	samples := []sample{
		{"legacy seed 1", 1, -64, 1040, 128, 128, sourceWaterExposure{sources: 24754}},
		{"legacy seed 0x5EED", 0x5EED, -64, 1040, 128, 128, sourceWaterExposure{sources: 41342}},
		{"legacy seed 7", 7, -64, 1040, 128, 128, sourceWaterExposure{sources: 111425}},
		{"reported seed-one cascade", 1, -64, -1240, 256, 256, sourceWaterExposure{sources: 136736}},
		{"east seed-one reach", 1, 1984, 1984, 128, 128, sourceWaterExposure{sources: 24151}},
		{"west seed-one reach", 1, -4160, 3008, 128, 128, sourceWaterExposure{sources: 38294}},
		{"far seed-one reach", 1, 8128, -8256, 128, 128, sourceWaterExposure{sources: 30768}},
		{"water-statistics reach", waterSeed, waterAreaOriginX, waterAreaOriginZ, 128, 128, sourceWaterExposure{sources: 25282}},
		{"seed-5eed first village on a river", 0x5EED, 705, -3414, 93, 93, sourceWaterExposure{sources: 11814}},
		{"seed-5eed second village on a river", 0x5EED, 2051, 4744, 93, 93, sourceWaterExposure{sources: 16372}},
		{"seed-seven first village on a river", 7, 16544, -14643, 93, 93, sourceWaterExposure{sources: 13063}},
		{"seed-seven second village on a river", 7, -3408, -4917, 137, 137, sourceWaterExposure{sources: 23005}},
		{"seed-c0ffee capital on a river", 0xC0FFEE, -91, -242, 219, 219, sourceWaterExposure{sources: 84677}},
	}

	for _, sample := range samples {
		got := measureSourceWaterExposure(sample.seed, sample.x, sample.z, sample.width, sample.depth)
		if got != sample.want {
			t.Errorf("%s: exposure = %+v, want %+v", sample.name, got, sample.want)
		}
		t.Logf("%s: %d sources, %d exposed (%d dry-bank, %d settlement, %d lower-body, %d unnamed)",
			sample.name, got.sources, got.exposed, got.dryBank, got.settlement, got.lowerBody, got.unnamed)
	}
}

type sourceWaterExposure struct {
	sources, exposed                        int
	dryBank, settlement, lowerBody, unnamed int
}

// settlementOwnsHeightAt reports whether a settlement decides this column's ground: the
// plateau inside its radius, the blend out to the end of its band.
//
// **It is the one neighbour [riverBankAt] may not raise**, so an exposure toward such a
// column is a category of its own rather than a dry bank. See the paragraph above
// TestNoSourceWaterStandsAgainstOpenAir.
func settlementOwnsHeightAt(seed, worldX, worldZ int64) bool {
	base := unloweredHeightAt(seed, worldX, worldZ)
	_, _, near := settlementShapeAt(seed, worldX, worldZ, base, ClimateAt(seed, worldX, worldZ))
	return near
}

func measureSourceWaterExposure(seed, originX, originZ, width, depth int64) sourceWaterExposure {
	var measured sourceWaterExposure
	for z := originZ; z < originZ+depth; z++ {
		for x := originX; x < originX+width; x++ {
			col := columnAt(seed, x, z)
			for y := int64(20); y <= 110; y++ {
				block := col.voxelAt(seed, x, y, z)
				if !waterSource(block) {
					continue
				}
				measured.sources++
				exposed, dryBank, settlement, lowerBody, unnamed := false, false, false, false, false
				for _, step := range [4][2]int64{{1, 0}, {-1, 0}, {0, 1}, {0, -1}} {
					nx, nz := x+step[0], z+step[1]
					neighbour := columnAt(seed, nx, nz)
					if neighbour.voxelAt(seed, nx, y, nz) != Air {
						continue
					}
					exposed = true
					switch {
					case col.river && settlementOwnsHeightAt(seed, nx, nz):
						settlement = true
					case col.river && !neighbour.standingWater:
						dryBank = true
					case col.river && neighbour.standingWater && neighbour.waterSurface < int(y):
						lowerBody = true
					default:
						unnamed = true
					}
				}
				if !exposed {
					continue
				}
				measured.exposed++
				if dryBank {
					measured.dryBank++
				}
				if settlement {
					measured.settlement++
				}
				if lowerBody {
					measured.lowerBody++
				}
				if unnamed {
					measured.unnamed++
				}
			}
		}
	}
	return measured
}

// The automaton's distinct question stays pinned beside containment. Plain water feeds
// every side; a current source feeds only its encoded downstream face. In the legacy
// 128x128 window worldgen 24 measures 2 of 24754 at seed 1 and zero of 41342 and
// 111425 at the other two seeds, all within the existing five-in-ten-thousand ceiling.
func TestNoSourceWaterFeedsOpenAir(t *testing.T) {
	t.Parallel()

	// Five in ten thousand, from the measurement in the comment above. Per ten thousand
	// rather than per hundred because the worst seed is now 0.01% and a ceiling stated in
	// whole percent would be a hundred times the thing it bounds.
	const exposedCeiling = 5

	for _, seed := range []int64{1, 0x5EED, 7} {
		exposed, sources := 0, 0
		var worst [3]int64
		for x := int64(-64); x < 64; x++ {
			for z := int64(1040); z < 1168; z++ {
				col := columnAt(seed, x, z)
				for y := int64(20); y <= 110; y++ {
					block := col.voxelAt(seed, x, y, z)
					if !waterSource(block) {
						continue
					}
					sources++
					for _, step := range [4][2]int64{{1, 0}, {-1, 0}, {0, 1}, {0, -1}} {
						if !WaterFeedsToward(block, int(step[0]), int(step[1])) {
							continue
						}
						nx, nz := x+step[0], z+step[1]
						neighbour := columnAt(seed, nx, nz)
						if neighbour.voxelAt(seed, nx, y, nz) == Air {
							if exposed == 0 {
								worst = [3]int64{x, y, z}
							}
							exposed++
							break
						}
					}
				}
			}
		}
		if sources == 0 {
			t.Fatalf("seed %d: the window holds no source water, so nothing was checked", seed)
		}
		if exposed*10000 > sources*exposedCeiling {
			t.Errorf("seed %d: %d of %d source voxels feed open air (%.3f%%), over the %d-in-10000 ceiling; first at %v",
				seed, exposed, sources, float64(exposed)*100/float64(sources), exposedCeiling, worst)
		}
		t.Logf("seed %d: %d of %d source voxels feed open air (%.3f%%)",
			seed, exposed, sources, float64(exposed)*100/float64(sources))
	}
}

// The bank rule, asserted where it fires rather than only through the share above.
//
// **A share that has fallen is evidence the defect is gone; it is not evidence the rule
// is there.** Removing every cave from the world would pass the sweep too. So this one
// counts the carves the rule actually refuses, fails if it finds none, and checks what
// stands in their place: the refusal has to leave the column's own ground, not air and
// not water, or it has traded one wall nothing holds for another.
//
// **The total is what must be non-zero, not each seed**, and the counts say why: 202 at
// seed 1 and none at the other two. Seed 1's residue after #654 was 214 voxels of
// *channel* water beside a carved bank, which only a refused carve can answer; seed 7's
// 339 and all but four of seed 0x5EED's 63 were the other half of #660, a pocket sealed
// under a lake, and draining one leaves the cave beside it opening into nothing that
// needs a wall. A window can be closed by the fill rule alone and reach this test with
// no carve left to refuse.
func TestACarveDoesNotBreachTheWallOfAStandingBody(t *testing.T) {
	t.Parallel()

	total := 0
	for _, seed := range []int64{1, 0x5EED, 7} {
		refused := 0
		for x := int64(-64); x < 64; x++ {
			for z := int64(1040); z < 1168; z++ {
				col := columnAt(seed, x, z)
				for y := int64(20); y <= 110; y++ {
					if !col.carveFieldAt(seed, x, y, z) || col.carvedAt(seed, x, y, z) {
						continue
					}
					refused++

					// The rule, stated from its two preconditions rather than read back
					// out of the function that applies it.
					if fill := col.caveFillAt(int(y)); fill != Air {
						t.Fatalf("seed %d: the carve at (%d, %d, %d) was refused, but this column would have filled it with %d",
							seed, x, y, z, fill)
					}
					if !col.waterStandsBesideAt(y) {
						t.Fatalf("seed %d: the carve at (%d, %d, %d) was refused with no water standing beside it",
							seed, x, y, z)
					}
					if got := col.voxelAt(seed, x, y, z); got == Air || IsWater(got) {
						t.Fatalf("seed %d: the refused carve at (%d, %d, %d) left %d rather than this column's ground",
							seed, x, y, z, got)
					}
				}
			}
		}
		total += refused
		t.Logf("seed %d: %d carves refused at a water wall", seed, refused)
	}
	if total == 0 {
		t.Fatal("no seed holds a carve the bank rule refuses, so nothing was checked")
	}
}

// TestAColumnCarriesTheSameBankItResolvesAlone pins the shortcut [Generate] takes.
//
// **There are two ways to obtain a column's water band and a chunk uses both**:
// [bankWaterAt] resolves one from a coordinate, and [column.bank] reads one off a column
// already resolved. The second is why a chunk pays 128 of the first instead of 4096, and
// if the two ever disagreed a chunk's interior would carve by one rule and its border by
// another — silently, and only where water happens to stand.
//
// The second half is the same claim about the whole composition: below a column's own
// surface, where no plant and no building reaches, [Generate]'s voxels must be exactly
// the ones [columnAt] composes. That is the region the bank rule moves.
func TestAColumnCarriesTheSameBankItResolvesAlone(t *testing.T) {
	t.Parallel()

	wet := 0
	for x := int64(-64); x < 64; x++ {
		for z := int64(1040); z < 1168; z++ {
			col := columnShapeAt(waterSeed, x, z)
			if got, want := col.bank(), bankWaterAt(waterSeed, x, z); got != want {
				t.Fatalf("column (%d, %d) carries band %+v, but resolving it alone gives %+v", x, z, got, want)
			}
			if col.standingWater {
				wet++
			}
		}
	}
	if wet == 0 {
		t.Fatal("the window holds no column standing in water, so the non-empty band was never compared")
	}

	for _, coord := range []Coord{goldenWaterCoord, bodyCaveMouthCoord} {
		chunk := Generate(waterSeed, coord)
		originX, originY, originZ := coord.Origin()
		for z := range ChunkSize {
			for x := range ChunkSize {
				worldX, worldZ := originX+int64(x), originZ+int64(z)
				col := columnAt(waterSeed, worldX, worldZ)
				for y := range ChunkSize {
					worldY := originY + int64(y)
					if worldY > int64(col.surface) {
						continue // above the ground, where a plant or a roof may stand
					}
					if got, want := chunk.At(x, y, z), col.voxelAt(waterSeed, worldX, worldY, worldZ); got != want {
						t.Fatalf("chunk %+v voxel (%d, %d, %d) is %d, but the column composes %d",
							coord, worldX, worldY, worldZ, got, want)
					}
				}
			}
		}
	}
}

// The bank rule, asserted where it fires rather than only through the zeros above.
//
// **A count that has fallen to zero is evidence the defect is gone; it is not evidence
// the rule is there.** TestACarveDoesNotBreachTheWallOfAStandingBody says the same thing
// about the carve half of the same idea, for the same reason: a window with no rivers in
// it satisfies "no source water stands against open air" perfectly. So this one counts
// the columns [riverBankAt] actually raises, fails if it finds none, and checks what
// stands on them — the raise has to leave ground at or above the channel's water surface,
// and it has to leave that column dry, because the lowest terrace a channel may stand on
// is 48 and a column raised to one is therefore a step above the sea line.
//
// **The totals are per seed and the raise is rare by design**: 177 columns at seed 1,
// 396 at 0x5EED, 30 at 7 and 128 at 0xC0FFEE, over five 384x384 windows each. Seed 7's
// thirty are why the assertion is on the total rather than on each seed — a window can
// hold a river whose banks already stand high enough everywhere.
func TestARiverChannelStandsBetweenTwoBanks(t *testing.T) {
	t.Parallel()

	windows := [][2]int64{{-64, -1240}, {6144, -2048}, {-4160, 3008}, {8128, -8256}, {0, 0}}
	total, worst := 0, 0
	for _, seed := range []int64{1, 0x5EED, 7, 0xC0FFEE} {
		raised := 0
		for _, origin := range windows {
			for z := origin[1]; z < origin[1]+384; z++ {
				for x := origin[0]; x < origin[0]+384; x++ {
					// The three exemptions [shapeAt] applies before it reaches the
					// bank rule. Asked in its order, so that this test compares the
					// height the generator produces against the height the rule asks
					// for and never against a plateau that owns the column instead.
					if nearOriginColumn(x, z) {
						continue
					}
					climate := ClimateAt(seed, x, z)
					base := unloweredHeightAt(seed, x, z)
					if _, _, near := settlementShapeAt(seed, x, z, base, climate); near {
						continue
					}
					if _, channel := channelSurfaceAt(seed, x, z); channel {
						continue
					}

					bank, ok := riverBankAt(seed, x, z)
					if !ok {
						continue
					}
					natural := base - basinAt(seed, x, z, climate)
					col := columnAt(seed, x, z)
					if want := max(natural, bank); col.surface != want {
						t.Fatalf("seed %d: the bank at (%d, %d) stands at %d, want %d from a channel holding %d over ground at %d",
							seed, x, z, col.surface, want, bank, natural)
					}
					if natural >= bank {
						continue // the ground here already reaches the channel's water
					}
					raised++
					if raise := bank - natural; raise > worst {
						worst = raise
					}

					// A bank is ground. Its top block is solid, and a column raised to a
					// terrace — never below 48, one step over the sea line — no longer
					// stands in water of its own: a raise that filled would have moved
					// the wall rather than built one.
					if top := col.voxelAt(seed, x, int64(col.surface), z); top == Air || IsWater(top) {
						t.Fatalf("seed %d: the bank at (%d, %d) is %d at its own surface rather than ground",
							seed, x, z, top)
					}
					if col.standingWater {
						t.Fatalf("seed %d: the bank at (%d, %d) was raised to %d and still stands in water to %d",
							seed, x, z, col.surface, col.waterSurface)
					}
				}
			}
		}
		total += raised
		t.Logf("seed %d: %d columns raised to the channel beside them", seed, raised)
	}
	if total == 0 {
		t.Fatal("no seed holds a column the bank rule raises, so nothing was checked")
	}
	t.Logf("%d columns raised in all, the tallest by %d blocks", total, worst)
}

// [nearRiverBandAt] has to be a superset of "has a channel neighbour", and this is the
// measurement that says it is.
//
// **The gate is an optimisation and it fails silently.** A column it wrongly excludes
// never asks its neighbours, so its bank is never raised and the water it was going to
// hold stands against air again — the exact defect #786 removed, back in a form that
// turns nothing red. The gate is therefore checked the only way an approximation can be:
// by sweeping the columns that do have a channel neighbour and asserting every one of
// them is admitted.
//
// The margin is measured in the same sweep and logged: the worst first-order distance
// among those columns is five blocks against a gate of twelve. See [riverBankGateBlocks].
func TestEveryBankColumnIsInsideTheBankGate(t *testing.T) {
	t.Parallel()

	windows := [][2]int64{{-64, -1240}, {6144, -2048}, {-4160, 3008}, {8128, -8256}, {0, 0}, {-2048, -2048}}
	banked, worst := 0, int64(0)
	for _, seed := range []int64{1, 0x5EED, 7, 0xC0FFEE} {
		for _, origin := range windows {
			for z := origin[1]; z < origin[1]+384; z++ {
				for x := origin[0]; x < origin[0]+384; x++ {
					if _, channel := channelSurfaceAt(seed, x, z); channel {
						continue
					}
					neighbour := false
					for _, step := range [4][2]int64{{1, 0}, {-1, 0}, {0, 1}, {0, -1}} {
						if _, channel := channelSurfaceAt(seed, x+step[0], z+step[1]); channel {
							neighbour = true
							break
						}
					}
					if !neighbour {
						continue
					}
					banked++
					if !nearRiverBandAt(seed, x, z) {
						t.Fatalf("seed %d: the column at (%d, %d) has a channel neighbour and the %d-block gate excludes it",
							seed, x, z, int64(riverBankGateBlocks))
					}

					// How much of the gate that column actually needed, in the same
					// first-order block distance riverAt measures.
					distance := absInt64(riverField(seed, x, z) - one/2)
					gx, gz := riverGradientAt(seed, x, z)
					ax, az := absInt64(gx), absInt64(gz)
					if magnitude := max(ax, az) + min(ax, az)/2; magnitude > 0 {
						if blocks := distance * (2 * riverGradientSpan) / magnitude; blocks > worst {
							worst = blocks
						}
					}
				}
			}
		}
	}
	if banked == 0 {
		t.Fatal("no column in the sweep has a channel neighbour, so the gate was never exercised")
	}
	t.Logf("%d columns with a channel neighbour, all inside the gate; the worst needed %d of its %d blocks",
		banked, worst, int64(riverBankGateBlocks))
}
