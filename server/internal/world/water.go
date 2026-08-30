package world

// Water: the sea line every low column fills to, the basins that dig lakes into the
// ground below it, the rivers that run across it, the ice that lies on it where the
// climate is cold enough, and the caves beneath those bodies filled to their surface.
//
// **The generator delivers the hydrostatic initial state; runtime flow takes it from
// there.** Generation remains the pure integer function of (seed, x, y, z) the storm
// needs: it reads no neighbour and schedules no update. The runtime may later move
// water after an edit, but that movement is mutable world state layered over this
// base, never an input back into generation. This file defines neither scheduling nor
// block updates; those belong to the flow simulation.
//
// Two of the three ways water appears are decided **inside [HeightAt]**, not beside
// it: a basin lowers the land and a river channel replaces it, and both have to be
// visible to every consumer of the height field — the generator, the border
// continuity tests and spawn placement — or two of them would disagree about where
// the ground is. The third, the fill itself, is a per-voxel rule in [column.voxelAt].
//
// Like the rest of generation this is Q16.16 fixed point with no float anywhere; see
// noise.go for why that is not a style preference.

const (
	// seaLevel is the y below which every open column stands in water.
	//
	// **The issue that asked for water said 60, and 60 drowns the world.** The number
	// had to be measured rather than reasoned, for the reason caveHalfWidth records
	// two files away: a sea line is a cut through the *height distribution*, and
	// worldgen 3's relief-driven amplitude made that distribution much wider than the
	// one this number was guessed against. Measured at seed 0x5EED over 16384 columns
	// of a 1024×1024 window at 8-block steps, on the bare height field with neither
	// basins nor rivers applied:
	//
	//	surface < 60 → 36.6% of columns
	//	surface < 55 → 22.6%
	//	surface < 50 →  9.7%
	//	surface < 47 →  4.8%
	//
	// The same issue asks for water at the surface of between 3% and 15% of columns,
	// which is the half of that pair describing the world somebody walks through
	// rather than the knob that produces it — so, exactly as with caveHalfWidth, the
	// knob moved. At 47, with basins and rivers on top, six 1024×1024 windows spread
	// over sixteen thousand blocks of map measure 4.8%, 4.8%, 5.4%, 5.5%, 10.1% and
	// 14.4%: the whole band, and all of it inside what was asked for. The high one is
	// part desert, which has no basins and simply sits low.
	// TestWaterCoversItsShareOfTheWorld is that measurement, kept.
	//
	// Everything else in this file is written against it rather than beside it — the
	// river bed, the beach band, the underground water and the spawn floor are all
	// offsets from this line — so moving it again moves them together.
	seaLevel = 47

	// Basins: the lakes, and the only thing here that changes the shape of the ground
	// without replacing it.
	//
	// **A basin lowers the land; it does not add water.** The fill rule below is the
	// same everywhere in the world, so a lake is what you get when the ground is
	// pushed under the sea line and nothing else about the column changes. That is
	// why this is in the height field: dig the hole, and the water is already there.
	//
	// basinScaleBlocks is between terrainScaleBlocks (96) and reliefScaleBlocks
	// (768): a lake is bigger than a ridge and smaller than a mountain range, which
	// is the size that makes one a place you walk around rather than a puddle or an
	// inland sea.
	basinScaleBlocks = 320

	// basinThreshold is where a basin begins and basinFullDepth is where it reaches
	// basinDepth. Both are fractions of the field's *range*, and neither is a
	// fraction of the columns.
	//
	// **The issue said one threshold at about 78% and no second number, and that
	// shape cannot produce a lake.** fbm2D sums four octaves, so the sum piles up
	// hard around its midpoint and the tail is thin: measured over 65536 columns at
	// seed 0x5EED, the field's 90th percentile is 0.671, its 99th is 0.789 and its
	// maximum anywhere in the sample is 0.954. A cut-off at 0.78 therefore selects
	// about one column in a hundred, and rescaling [0.78, 1] onto the smoothstep
	// leaves even those in the flat foot of the curve — measured, the deepest basin
	// anywhere in six 1024×1024 windows was **three blocks**, against a basinDepth of
	// ten. The feature was a knob that did nothing.
	//
	// Two numbers fix it: begin at the 90th percentile and reach full depth at about
	// the 99.5th, which is the band the field actually occupies. Measured with those,
	// basins reach the full ten blocks and put a lake in ground that would otherwise
	// have been dry in every window sampled.
	basinThreshold = one * 67 / 100
	basinFullDepth = one * 81 / 100

	// basinDepth is how far the deepest point of a basin sinks. Ten blocks under a
	// sea line at 47 means a lake bottom no lower than 37 in ordinary ground: deep
	// enough to swim down into and shallow enough that the bottom is worth reaching.
	basinDepth = 10

	// Rivers: a channel that follows the land in terraces, wherever a slow field
	// crosses its own midpoint.
	//
	// **The bed used to be one height everywhere and is now the land's, quantised.**
	// A fixed bed made a river a canal: flat from source to sea, cut into a gorge
	// wherever the ground rose, and capped by riverMaxSurface because a canal through
	// a mountain is a slot. The surface is riverSurfaceAt — the smoothed land, floored
	// to riverTerraceStep — so a channel climbs with the ground, the cap is gone, and
	// a river ends where its own surface falls under the sea line.
	//
	// **The condition is |n − ½| < riverHalfWidth, which is caveAt's condition in two
	// dimensions and for the same reason.** A field *thresholded* selects a region
	// with an area; a field held near a *level set* selects a curve with a length,
	// and a curve is the thing that runs somewhere. One field rather than caveAt's
	// two, because a river is meant to be a line and not a network.
	//
	// riverScaleBlocks is the largest scale in this file: a river is a feature of a
	// region, and at 640 blocks to a lattice cell the first octave alone is twenty
	// chunks across, so a channel wanders for a long walk before it turns.
	riverScaleBlocks = 640

	// riverHalfWidth is how far either side of the midpoint the field may sit and
	// still be a channel.
	//
	// **The issue said 2/100 and that is a marsh, not a river**, for the third time
	// in this file and the same reason each time: the number is a fraction of the
	// field's range and the field is concentrated around its midpoint, so it buys far
	// more columns than its size suggests. Measured over the same six windows:
	//
	//	2/100  → 1.0% … 9.8% of columns are channel
	//	1/100  → 0.6% … 4.8%
	//	6/1000 → 0.6% … 6.8%
	//	4/1000 → 0.6% … 4.6%
	//	3/1000 → 0.6% … 3.6%
	//
	// At 4/1000 a channel is a few blocks across — narrow enough to be a line on the
	// ground and wide enough to swim down. TestARiverIsContinuousAlongItsCourse walks
	// one rather than trusting this sentence.
	riverHalfWidth = one * 4 / 1000

	// riverBedDrop is how far under its own surface a river bed sits, so that a
	// channel is three blocks of water deep wherever it runs.
	//
	// It used to be measured from the sea line, which was the same number while every
	// river surface *was* the sea line. Measured from riverSurfaceAt, a channel two
	// hundred blocks up is as deep as one at the shore.
	riverBedDrop = 3

	// riverTerraceStep is how tall one step of a river's surface is.
	//
	// **A river that runs downhill in a voxel world runs down in steps.** The surface
	// is the land it crosses, quantised to this, so a channel is a flat pool per
	// terrace and a fall between two of them. At one block a change is a ripple nobody
	// reads as a waterfall; at eight the pool above is a wall you cannot see over from
	// the pool below.
	//
	// **Four is also one more than riverBedDrop, and that is load-bearing.** A step
	// taller than the channel is deep keeps the lower terrace's water under the higher
	// terrace's bed, so a fall goes over a lip of gravel rather than out of the side of
	// a pool. Lower it to three and the two surfaces meet.
	riverTerraceStep = 4

	// riverSmoothSpan is how far either side of a column the land is averaged before
	// it is quantised.
	//
	// **Quantising the raw height is not a staircase, and is still measurably worse.**
	// This comment first claimed a raw quantisation would break a terrace at every
	// wrinkle of the fourth octave; it does not, because terrainScaleBlocks is 96 and
	// the field is already smooth one block out. What the mean buys is narrower and
	// real. At seed 0x5EED over a contiguous 256x256 window of some 24000 adjacent
	// channel pairs, the share of pairs on different terraces — a fall you can step
	// across — runs 4.4% at span 0, 3.9% at 4, 3.5% at 8, 3.3% at 16 and 3.1% at 24.
	// So the mean removes about a fifth of the falls and lengthens the pools by as
	// much, and eight is where that stops being worth paying for: each doubling past
	// it buys a tenth as much, while a mean taken further out is decreasingly a
	// statement about the column it is for.
	//
	// Five samples rather than a square: the column and its four axis neighbours cost
	// five height fields where a 3x3 costs nine, and a river follows a curve rather
	// than a patch. The cost does not change with the span.
	riverSmoothSpan = 8

	// Beaches: what a plains or taiga surface is made of when it stands at the
	// water's edge.
	//
	// A band rather than a line, and asymmetric on purpose: one block under the sea
	// line so that a shallow shelf is sand rather than drowned grass, and two above
	// it so that walking out of a lake crosses a shore instead of stepping straight
	// from water onto turf. Desert needs no rule — its ground is already sand — and
	// tundra none either, because a frozen shore is snow.
	beachBelowSea = 1
	beachAboveSea = 2

	// beachDepth is how deep the sand goes: the surface and the two blocks under it.
	beachDepth = 3

	// caveWaterLevel is the highest y a carved voxel may hold water at.
	//
	// **Water underground keeps a depth rule where no standing surface exists.** At
	// 36 the streams sit eleven blocks under the sea line and well inside the carved
	// band (caveMaxDepth is 96 below a surface that averages 64), so a walk down a
	// dry-land tunnel reaches water before it reaches the bottom of the cave system.
	// A sea, basin or river column instead extends its own hydrostatic surface through
	// every carved voxel below it.
	caveWaterLevel = 36

	// spawnWaterClearance is how far from the spawn column, in blocks on each
	// horizontal axis, neither a basin nor a river applies.
	//
	// Chebyshev, and the same eight blocks spawnCaveClearance uses, for the same
	// reason and with the same cost: two comparisons in front of the two fbm sums
	// below. What it buys is that the first thing a session does is not a swim
	// because a river happened to cross the origin — SpawnAt keeps the player out of
	// the water either way, but standing on ground is a better first frame than
	// treading water in a channel.
	spawnWaterClearance = 8
)

// The basin and river fields each get their own offset from the world seed, in the
// style of every other field here. Sampling one field at two scales would make every
// river run down the middle of a lake, forever.
const (
	basinSeedOffset int64 = 0xB5470917
	riverSeedOffset int64 = 0x9216D5D9
)

// The beach band only names a band while its two halves bound one, and non-emptiness
// is the whole of what this checks: beachAt selects
// seaLevel-beachBelowSea ≤ surface ≤ seaLevel+beachAboveSea, which is no column at all
// once the two sum below zero. It does **not** catch the two being swapped — the sum
// is the same either way — and there is nothing there to catch: two blocks of sand
// under the water and one above is a differently shaped shore, not a broken one.
// Which half is which is a retune; a band no surface can land in is the bug.
const _ = uint8(beachAboveSea + beachBelowSea)

// A river bed sits under the sea line and above the floor of the world, and it takes
// two conversions to say so. The first is the end a river needs in order to hold any
// water: at riverBedDrop = 0 the bed is *at* the sea line and the channel is dry. The
// second is the end one guard alone left open — uint8 accepts up to 255, so a
// riverBedDrop raised past seaLevel still compiled and put the bed at or under y = 0,
// which is a trench in the bottom of the world with no ground beneath it rather than
// a river. Together they bound it to 1 ≤ riverBedDrop ≤ seaLevel-1.
const _ = uint8(riverBedDrop - 1)
const _ = uint8(seaLevel - riverBedDrop - 1)

// A terrace has to be taller than the channel is deep, or the water on the lower step
// stands level with the lip of the higher one and the fall between them is not a fall.
// riverTerraceStep = 0 would also divide by zero in riverSurfaceAt; this catches both.
const _ = uint8(riverTerraceStep - riverBedDrop - 1)

// A smoothing span of nought averages one sample five times, which is the raw height
// the mean exists to replace. Non-zero is the whole claim.
const _ = uint8(riverSmoothSpan - 1)

// A basin has to deepen with the field rather than the other way about. Swap these
// two and the rescale below divides by a negative, which is a compile error here
// instead of an inverted lake. Unsigned and untyped-width rather than the uint8 the
// other guards use, because the difference is a fixed-point fraction and not a
// depth in blocks.
const _ uint64 = basinFullDepth - basinThreshold

// The underground water has to sit under the sea line: above it a tunnel opening
// into a lake bed would show two different water surfaces meeting at a wall.
const _ = uint8(seaLevel - caveWaterLevel)

// basinAt is how many blocks a basin lowers a column by, and zero where there is
// none.
//
// Smoothstep over the part of the field between the two thresholds, so the rim of a
// lake is a slope rather than a cliff: a linear ramp would leave the derivative
// jumping at the threshold and every shoreline would be a one-block step, which is
// the same crease amplitudeAt uses smoothstep to avoid at the foot of a mountain.
//
// **The rescale is onto [basinThreshold, basinFullDepth] rather than onto
// [basinThreshold, one], and that is the difference between a lake and nothing.**
// The field's maximum anywhere in a 65536-column sample is 0.954, so a rescale that
// treats 1.0 as the deepest point puts every basin in the flat foot of the curve.
// See the constants, which carry the measurement.
//
// Desert is absent by name. A desert with lakes in it is not a desert, and the
// statement being made is that there is no water there rather than that there is
// very little — the same shape as coniferChanceDenominator's absent case.
func basinAt(seed, worldX, worldZ int64, climate Climate) int {
	if climate == Desert {
		return 0
	}

	n := climateField(seed+basinSeedOffset, worldX, worldZ, basinScaleBlocks)
	if n < basinThreshold {
		return 0
	}

	t := min(((n-basinThreshold)*one)/(basinFullDepth-basinThreshold), one)
	return int((basinDepth * smoothstep(t)) >> fracBits)
}

// riverAt reports whether a column lies in a river channel.
//
// Reads nothing but the seed and the column, like every other field here: two
// neighbouring chunks agree about a river crossing their border by each computing
// this, not by consulting one another.
func riverAt(seed, worldX, worldZ int64) bool {
	n := climateField(seed+riverSeedOffset, worldX, worldZ, riverScaleBlocks)
	return absInt64(n-one/2) < riverHalfWidth
}

// riverSmoothedHeightAt is the land under a channel with its finest octave averaged
// out: the mean of [unloweredHeightAt] at the column and at ±riverSmoothSpan along
// both axes.
//
// **The unlowered height, for the reason every other rule reads it.** A basin or a
// neighbouring channel would otherwise feed its own lowering back into this one, and
// what a river follows is the land rather than the order two lowerings ran in.
//
// floorDiv rather than Go's division: terrain reaches below y = 0 in a deep enough
// trough, and truncation toward zero would round those columns the wrong way.
func riverSmoothedHeightAt(seed, worldX, worldZ int64) int {
	sum := unloweredHeightAt(seed, worldX, worldZ) +
		unloweredHeightAt(seed, worldX-riverSmoothSpan, worldZ) +
		unloweredHeightAt(seed, worldX+riverSmoothSpan, worldZ) +
		unloweredHeightAt(seed, worldX, worldZ-riverSmoothSpan) +
		unloweredHeightAt(seed, worldX, worldZ+riverSmoothSpan)
	return int(floorDiv(int64(sum), 5))
}

// riverSurfaceAt is the height a channel's water stands at: the smoothed land,
// floored to a terrace.
//
// **This is the whole of "a river runs with the land".** Two adjacent columns whose
// smoothed heights land in the same terrace share a water surface exactly, so a river
// is a chain of flat pools; where the land crosses a terrace boundary the two differ
// by a multiple of riverTerraceStep, and that difference is a fall. Nothing here
// paints the fall — the flow automaton pours it — and nothing here reads a neighbour,
// which is what keeps the two sides of a chunk border agreeing.
func riverSurfaceAt(seed, worldX, worldZ int64) int {
	return int(floorDiv(int64(riverSmoothedHeightAt(seed, worldX, worldZ)), riverTerraceStep)) * riverTerraceStep
}

// riverChannelAt is the channel one column carries: the bed its ground is cut to and
// the height its water stands at, or ok = false where there is no channel here.
//
// **Two ways a column in the field is not a channel, and they are different.** Below
// the sea line the river's own surface is under the water that is already there, so
// the column is left to the sea and the basin rule — the alternative is two fills at
// two heights in one column.
//
// **The test is per column, so a course can end and begin again further along the same
// field — and the gap is water rather than land.** riverSurfaceAt is a multiple of
// riverTerraceStep and seaLevel is 47, so 48 is the lowest terrace a channel may stand
// on and this reads "the smoothed land here is at or under the sea line": a gap is the
// lake the reach runs into, never high ground. Swept at seeds 0x5EED and 0xC0FFEE over
// five 768x768 windows, 192953 channel columns: 15986 of 19849 gap columns stand under
// water, and of the 1644 dry ones touching a channel none rises more than riverBedDrop
// above that channel's surface — a shoreline the automaton pours over, not a dam.
// riverMaxSurface, which this replaced, cut on hilltops instead: 55 dry gaps, to 3966.
//
// **And a bed is never raised above the land.** riverSurfaceAt is a neighbourhood
// mean, so a column in a dip inside a rising reach can have a terrace above its own
// ground, and the unclamped bed would be an embankment the river runs along. The min
// follows the ground down instead, deepening the pool. Rare and not theoretical: swept
// at seed 0x5EED over 32768 blocks of map at a 3-block stride, 255 of 7011223 channel
// columns — 0.004% — would have been lifted, by at most three blocks.
//
// **A bed under the sea line is arithmetic, not this clamp**: 48 is the lowest terrace
// and riverBedDrop is 3, so the shallowest channel already cuts to 45. In the sweeps
// above all 20748 beds under the sea line are under it with the min removed, and the
// min moved 1 column of 192953.
func riverChannelAt(seed, worldX, worldZ int64, base int) (bed, waterSurface int, ok bool) {
	if !riverAt(seed, worldX, worldZ) {
		return 0, 0, false
	}
	surface := riverSurfaceAt(seed, worldX, worldZ)
	if surface < seaLevel {
		return 0, 0, false
	}
	return min(surface-riverBedDrop, base), surface, true
}

// nearSpawnColumn reports whether a column is inside the square around spawn that
// water leaves alone.
func nearSpawnColumn(worldX, worldZ int64) bool {
	return absInt64(worldX-spawnColumnX) <= spawnWaterClearance &&
		absInt64(worldZ-spawnColumnZ) <= spawnWaterClearance
}

// beachAt reports whether a column's top blocks are the sand of a shoreline.
//
// Plains and taiga only, and the band is read from the *final* surface — the one
// basins and rivers have already moved — because a beach is where the ground meets
// the water and not where it would have met it.
//
// **A river bed used to be excluded by arithmetic and is now excluded by its caller.**
// The bed sat three blocks under the band, so no channel could land in it; a terraced
// bed can sit anywhere, so [columnAt] refuses a beach on a river column instead. The
// rule here is unchanged.
func beachAt(surface int, climate Climate) bool {
	if climate != Plains && climate != Taiga {
		return false
	}
	return surface >= seaLevel-beachBelowSea && surface <= seaLevel+beachAboveSea
}

// standingWaterSurface reports the hydrostatic surface a column owns: the sea line
// for a sea or a basin, and its own terraced surface for a river.
//
// **The river surface is passed in rather than derived from the bed, and that is what
// a floored bed costs.** `surface + riverBedDrop` was exact while every bed was cut to
// exactly that depth; [riverChannelAt] now lowers a bed onto ground already under it,
// so the water would follow it down and a pool in a dip would report a surface below
// the terrace it belongs to.
func standingWaterSurface(surface, riverSurface int, river bool) (height int, ok bool) {
	if river {
		return riverSurface, true
	}
	if surface < seaLevel {
		return seaLevel, true
	}
	return 0, false
}

// fillAt is what stands in an air voxel of this column: water up to its standing
// surface, ice on the top of it where the climate is cold enough, and air above.
//
// **Ice is one voxel thick and only ever on the top of a body**, which is what makes
// it a lid rather than a frozen lake: everything under it is still water, so a hole
// broken in the surface is a way in. It is also the only [Solid] block in this file,
// so a tundra shore is something you walk out onto. The rule is unchanged; what moved
// is that a river's top is now its own terrace rather than the sea line.
//
// The caller has already established that the terrain here is air.
func (c column) fillAt(worldY int) Block {
	if !c.standingWater || worldY > c.waterSurface {
		return Air
	}
	if c.climate == Tundra && worldY == c.waterSurface {
		return Ice
	}
	return Water
}

// caveFillAt is what stands in a carved voxel: water below the dry-world cave
// level, water up to this column's standing surface when it has one, and air above.
//
// Both are world heights rather than depths. The fallback level makes dry-land cave
// water one body a walk descends to; the column surface makes a carved space beneath
// a lake part of that lake without consulting any neighbouring column.
func (c column) caveFillAt(worldY int) Block {
	if worldY <= caveWaterLevel || c.standingWater && worldY <= c.waterSurface {
		return Water
	}
	return Air
}
