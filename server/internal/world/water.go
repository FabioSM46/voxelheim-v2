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
	// **The course is the field's midpoint level set.** A field *thresholded* selects
	// a region with an area; a field held near a *level set* selects a curve with a
	// length, and a curve is the thing that runs somewhere. [riverAt] turns the field
	// distance from that level set into blocks through its local gradient, so the
	// selected band has a stable physical width instead of widening wherever the
	// field crosses its midpoint slowly. One field rather than caveAt's two, because
	// a river is meant to be a line and not a network.
	//
	// riverScaleBlocks is the largest scale in this file: a river is a feature of a
	// region, and at 640 blocks to a lattice cell the first octave alone is twenty
	// chunks across, so a channel wanders for a long walk before it turns.
	riverScaleBlocks = 640

	// riverHalfWidthBlocks is how far either side of the midpoint level set a channel
	// reaches, in blocks.
	//
	// **The old number was in field units, not blocks.** Its band covered a different
	// physical width wherever the field's slope changed: over the same 1024x1024
	// window, one seed produced a channel 65 blocks across and two others produced
	// widest bodies of 31 and 15 blocks. A four-block terrace through the first is a
	// wall across a lake rather than a short river fall.
	//
	// Three is wide enough to swim down. [riverAt] measures it with a first-order
	// distance to the level set; confluences and the 32-block gradient baseline make
	// the widest measured half-width seven blocks rather than exactly three.
	// TestRiverChannelsStayWithinTheirBlockWidth measures that bound over three seeds,
	// while TestARiverIsContinuousAlongItsCourse still walks one course end to end.
	riverHalfWidthBlocks = 3

	// riverBedDrop is how far under its own surface a river bed sits, so that a
	// channel is three blocks of water deep wherever it runs.
	//
	// It used to be measured from the sea line, which was the same number while every
	// river surface *was* the sea line. Measured from riverSurfaceAt, a channel two
	// hundred blocks up is as deep as one at the shore.
	riverBedDrop = 3

	// riverBankGateBlocks is how far from the river field's level set a column still
	// has to ask its four neighbours whether one of them carries a channel. See
	// [riverBankAt], which is the rule this gates.
	//
	// **The rule is cheap where it fires; the question is not.** Four neighbour
	// resolutions cost four [riverAt] — twenty fbm2D sums — on every non-channel
	// column in the world, to move between four columns in a hundred thousand and five
	// in ten thousand, depending on the seed. Asked
	// unconditionally, BenchmarkGenerateInOpenCountry went from 6.37ms a chunk to
	// 8.06ms: 27% for something that changes 0.02% of the map. The gate is [riverAt]'s
	// own comparison over a wider half-width — one first-order distance to the level
	// set, five sums — and the shipped rule measures 6.66ms against 7.04ms, 6%, on its
	// own run of the same benchmark. BenchmarkGenerateInACapital does not move.
	//
	// **Twelve blocks is four times [riverHalfWidthBlocks], and the margin is what the
	// number is for.** A gate that misses a bank leaves the water it was going to hold
	// standing against air, so this has to be a superset of "has a channel neighbour"
	// rather than a fit to it. Swept at seeds 1, 0x5EED, 7 and 0xC0FFEE over six
	// 384x384 windows each, the worst first-order distance measured at a column that
	// does have a channel neighbour is five blocks — the field's distance and the
	// neighbour's disagree where their gradients do, which is the same slack
	// TestRiverChannelsStayWithinTheirBlockWidth measures from the other side. Twelve
	// is 2.4 times that, admits 5.5% of columns where seven would admit 2.5%, and the
	// difference between the two is about one percent of generation.
	// TestEveryBankColumnIsInsideTheBankGate is the pin.
	riverBankGateBlocks = 4 * riverHalfWidthBlocks

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

	// riverGradientSpan is the distance the river field's gradient is differenced
	// over, to find the direction the channel runs.
	//
	// The course is the field's midpoint level set, so the gradient points across the
	// channel and the tangent — the way the water runs — is perpendicular to it. The
	// span trades two things that pull opposite ways, both measured over the same
	// 256x256 window: how often two adjacent channel columns choose *different* axes,
	// and how evenly the two axes are chosen at all.
	//
	//	span  1 → 2.70% of adjacent pairs disagree, 40.0% choose X
	//	span  4 → 2.27%, 39.5% X
	//	span 16 → 2.06%, 44.5% X
	//	span 32 → 1.51%, 59.8% X
	//	span 64 → 0.84%, 79.6% X
	//
	// Coherence improves all the way out and the balance collapses with it: past
	// sixteen blocks the difference stops describing the channel under the column and
	// starts describing the field's own large-scale lean, which is why four columns in
	// five point along X at sixty-four. Sixteen is the last span that is still local.
	riverGradientSpan = 16

	// riverSlopeSpan is how far along the tangent the land is compared to decide
	// which of its two ends is downhill.
	//
	// **The tangent says which way the channel lies; only the land says which way the
	// water goes.** Too short a baseline and the comparison reads the residue of the
	// smoothing rather than a slope, and neighbouring columns disagree. Measured over
	// the same window, as the share of adjacent channel pairs pointing straight at
	// each other: 10.51% at span 2, 7.00% at 4, 4.85% at 8, 3.03% at 16 and 1.92% at
	// 32.
	//
	// Sixteen is also one riverTerraceStep of fall at a 1-in-4 slope, so the two
	// samples straddle a real step at anything steeper, and it is deliberately the
	// same distance as riverGradientSpan: both are "one step of a walk along the
	// river", and two numbers there would be two claims about how far that is. Thirty-two
	// buys a third of the remaining disagreement and pays by answering about land the
	// column is not on; the 3% left at sixteen is the hollows and ridges the water
	// divides at, which #595 asks to be counted rather than removed.
	riverSlopeSpan = 16

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

	// originWaterClearance is how far from the world's origin column, in blocks on
	// each horizontal axis, neither a basin nor a river applies.
	//
	// Chebyshev, and the same eight blocks [originCaveClearance] uses, for the same
	// reason and with the same cost: two comparisons in front of the two fbm sums
	// below. **It guards generated blocks, not a spawn.** It was put here so that the
	// first thing a session did was not a swim, back when [SpawnAt] read the ground at
	// the origin column; #519 moved the join onto the capital's gate square and the
	// square of dry land stayed, because removing it would move terrain and bump
	// [WorldgenVersion].
	originWaterClearance = 8
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

// Every span here is a distance in blocks along an axis, and each is useless at zero:
// a smoothing span of nought averages one sample five times, a gradient span of nought
// differences a field against itself and reports no direction at all, and a slope span
// of nought compares a column's height with its own. Non-zero is the whole claim.
const _ = uint8(riverSmoothSpan - 1)
const _ = uint8(riverGradientSpan - 1)
const _ = uint8(riverSlopeSpan - 1)
const _ = uint8(riverHalfWidthBlocks - 1)

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

// riverField is the one field a river's course and its direction are both read from.
// Named rather than repeated, because riverCurrentAt differences it at four offsets and
// a second spelling of the same sample is how a course and its current stop describing
// the same channel.
func riverField(seed, worldX, worldZ int64) int64 {
	return climateField(seed+riverSeedOffset, worldX, worldZ, riverScaleBlocks)
}

// riverGradientAt is the central difference of [riverField] over the baseline that
// defines one local step across its course. The raw difference deliberately retains
// the 2*riverGradientSpan factor: [riverAt] can compare against it with integers, and
// [riverCurrentAt] needs only the relative components to choose the tangent axis.
func riverGradientAt(seed, worldX, worldZ int64) (gx, gz int64) {
	gx = riverField(seed, worldX+riverGradientSpan, worldZ) - riverField(seed, worldX-riverGradientSpan, worldZ)
	gz = riverField(seed, worldX, worldZ+riverGradientSpan) - riverField(seed, worldX, worldZ-riverGradientSpan)
	return gx, gz
}

// riverAt reports whether a column lies in a river channel.
//
// Reads nothing but the seed and the column, like every other field here: two
// neighbouring chunks agree about a river crossing their border by each computing
// this, not by consulting one another.
func riverAt(seed, worldX, worldZ int64) bool {
	distanceInField := absInt64(riverField(seed, worldX, worldZ) - one/2)
	gx, gz := riverGradientAt(seed, worldX, worldZ)
	ax, az := absInt64(gx), absInt64(gz)
	gradientMagnitude := max(ax, az) + min(ax, az)/2

	// max + min/2 is an octagonal approximation to hypot, within about three
	// percent and with no square root or float in the deterministic generator. The
	// strict comparison also makes a zero gradient no channel: without a local slope
	// there is no finite first-order distance to the level set.
	return distanceInField*(2*riverGradientSpan) < riverHalfWidthBlocks*gradientMagnitude
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

// riverCurrentAt is the way the water runs at one river column, as a unit step on one
// horizontal axis.
//
// **The course says which way the channel lies; only the land says which way the water
// goes.** The channel is [riverField]'s midpoint level set, so the field's gradient
// points across it and the tangent — along it — is perpendicular to that gradient.
// Quantising the tangent to its dominant axis is the whole of "no diagonal current
// ids": perpendicular to (gx, gz) is (-gz, gx), so the tangent lies along X exactly
// when |gz| >= |gx|.
//
// The sign is the only part that is about height rather than about the field: whichever
// end of the tangent has the lower smoothed land is downhill, and a tie falls to +X or
// +Z so that a perfectly flat reach still answers.
//
// **Adjacent columns may disagree, and that is the model rather than a defect.** Each
// column samples its own two ends, so where a reach runs into a hollow the two sides
// converge and where it crosses a ridge they diverge — the water pools at one and
// divides at the other, which is what water does. water_test.go counts those pairs as a
// diagnostic instead of rejecting them.
func riverCurrentAt(seed, worldX, worldZ int64) (dx, dz int) {
	gx, gz := riverGradientAt(seed, worldX, worldZ)

	if absInt64(gz) >= absInt64(gx) {
		if riverSmoothedHeightAt(seed, worldX+riverSlopeSpan, worldZ) <= riverSmoothedHeightAt(seed, worldX-riverSlopeSpan, worldZ) {
			return 1, 0
		}
		return -1, 0
	}
	if riverSmoothedHeightAt(seed, worldX, worldZ+riverSlopeSpan) <= riverSmoothedHeightAt(seed, worldX, worldZ-riverSlopeSpan) {
		return 0, 1
	}
	return 0, -1
}

// waterCurrentBlock is the source id for a current, and the inverse of [CurrentOf] over
// the whole of its domain rather than only over the four unit steps: the zero current
// maps to plain [Water], which is the block [CurrentOf] answers (0, 0) for.
//
// Only worldgen places one, so this is the only constructor there is, and
// [riverCurrentAt] never returns zero — the zero arm is what keeps the round trip an
// identity for a caller that does, not a live path.
// TestEveryCurrentIdRoundTripsThroughItsDirection pins both halves.
func waterCurrentBlock(dx, dz int) Block {
	switch {
	case dx > 0:
		return WaterCurrentXPos
	case dx < 0:
		return WaterCurrentXNeg
	case dz > 0:
		return WaterCurrentZPos
	case dz < 0:
		return WaterCurrentZNeg
	default:
		return Water
	}
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
//
// **A settlement is the one neighbour the bank rule may not raise, so the water stops
// at its ground instead.** The bed and the channel remain: only the source surface is
// clamped to the lowest settlement-owned horizontal neighbour. When that ground is at
// or below the bed, [column.fillAt] consequently writes no source water at all, while
// [riverFallTopAt] may still paint the upstream face as flowing water. Five reported
// settlement windows held 48, 48, 102, 94 and 51 exposed source voxels before this
// rule and none after it.
//
// Stopping the channel was measured too. It closes the same five exposures, but removes
// 16, 16, 34, 36 and 17 channel columns respectively. Clamping keeps every one of the
// 118, 259, 165, 898 and 527 raw channel columns in those windows, preserves the bed,
// and leaves both the sampled eight-percent water share and the walked course unchanged.
func riverChannelAt(seed, worldX, worldZ int64, base int) (bed, waterSurface int, ok bool) {
	bed, surface, ok := rawRiverChannelAt(seed, worldX, worldZ, base)
	if !ok {
		return 0, 0, false
	}
	if ground, lower := lowerSettlementGroundAt(seed, worldX, worldZ, surface); lower {
		surface = ground
	}
	return bed, surface, true
}

// rawRiverChannelAt is the channel before the settlement-edge water clamp. Settlement
// blending and river banks read this form because the clamp changes water, not either
// side's ground; ordinary columns receive [riverChannelAt]'s final source surface.
func rawRiverChannelAt(seed, worldX, worldZ int64, base int) (bed, waterSurface int, ok bool) {
	if !riverAt(seed, worldX, worldZ) {
		return 0, 0, false
	}
	surface := riverSurfaceAt(seed, worldX, worldZ)
	if surface < seaLevel {
		return 0, 0, false
	}
	return min(surface-riverBedDrop, base), surface, true
}

// lowerSettlementGroundAt reports the lowest settlement-owned horizontal neighbour
// below surface. Four neighbours and no diagonals: the defect is an open shared face,
// the same geometry [riverBankAt] and [riverFallTopAt] use.
func lowerSettlementGroundAt(seed, worldX, worldZ int64, surface int) (int, bool) {
	ground, lower := surface, false
	for _, step := range [4][2]int64{{1, 0}, {-1, 0}, {0, 1}, {0, -1}} {
		x, z := worldX+step[0], worldZ+step[1]
		if _, _, settled := settlementAtColumn(seed, x, z); !settled {
			continue
		}
		base := unloweredHeightAt(seed, x, z)
		neighbour, _, _ := settlementShapeAt(seed, x, z, base, ClimateAt(seed, x, z))
		if neighbour < ground {
			ground, lower = neighbour, true
		}
	}
	return ground, lower
}

// riverBankAt is the height a column with no channel of its own has to stand at in
// order to hold the channel beside it: the highest water surface among the four
// horizontally adjacent channels, or ok = false where none of them carries one.
//
// **A carve may not breach the wall of a standing body; the ground was never held to
// the same rule.** #660 gave the carve side [column.waterStandsBesideAt], so a cave
// may not open a face into water standing next door. Nothing said the same about the
// surface: [riverChannelAt] cuts a bed and [column.fillAt] fills it to
// [riverSurfaceAt] — a terrace of the *smoothed* land — and neither asks whether the
// ground beside the channel reaches that surface. Where it does not, the river's own
// source water stands with an open face over dry land, which is the shape a river has
// and none of the substance. With #784 and #785 in, seed 1 over the reported 256x256
// window measured 181 of 136744 source voxels standing that way: 175 beside a dry bank
// and 6 beside a lower sea or basin. This rule takes both counts to zero, in that
// window and in the seven others [TestNoSourceWaterStandsAgainstOpenAir] holds. Its
// last two windows are deliberately not zero: a settlement owns its columns' ground,
// so it is the one neighbour this rule may not raise. That residue is pre-existing,
// counted by name there, and owned by #828.
//
// **Raising the land was measured against lowering the water, and the loser fails in
// ways a count of exposed voxels cannot show.** The other candidate clamped a
// channel's surface to its lowest neighbouring non-channel ground. It takes the
// exposure to zero too, and it moves a comparable number of columns — 119 of the
// window's 1771 channel columns against 120 non-channel columns raised, each by at
// most three blocks. But 14 of those 119 clamp to or below their own bed, which is a
// dry hole in a river rather than a shorter pool, and a clamp is not a multiple of
// [riverTerraceStep]: it broke TestARiverSurfaceIsTerracedAndItsBedFollowsTheLand, it
// broke TestEveryTerraceFaceCarriesItsFall on adjacent channels differing by one block
// instead of a whole terrace, and it took #785's cascade measurement from 8 fall
// columns in 3 components to 43 in 12. Raising the bank breaks nothing but the count
// this rule exists to move.
//
// **It adds land, and how much is what it costs.** Swept at seeds 1, 0x5EED, 7 and
// 0xC0FFEE over five 384x384 windows each — 737280 columns a seed — it raises 0.024%,
// 0.054%, 0.004% and 0.017% of columns. Most raises are one block; the tail is a
// channel pooled in a dip whose rim the bed followed down, and the worst measured is
// ten blocks, at 0x5EED. **The sea-line statistic does not move**: the only window
// where any raised column was under the sea line is the reported one, where 7 of 65536
// stop standing in water — 0.011 percentage points of a band that runs from 3% to 15%,
// and [TestWaterCoversItsShareOfTheWorld]'s sampled sweep does not see them at all.
//
// **The raise applies to a column under the sea line as well as to dry ground, and the
// six lower-body exposures are why.** Where a channel's terrace stands above a sea or
// basin it runs into, the shared face is open at exactly the heights between the two
// surfaces. Excluding wet columns leaves those six standing; including them puts a
// one-column sill at the river's mouth, level with the pool it holds back, and the
// lake beside it a block lower. That is a weir, and it is what the invariant asks for.
//
// Four neighbours and no diagonals, for [riverFallTopAt]'s reason: a bank holds a
// shared face.
func riverBankAt(seed, worldX, worldZ int64) (top int, ok bool) {
	if !nearRiverBandAt(seed, worldX, worldZ) {
		return 0, false
	}
	for _, step := range [4][2]int64{{1, 0}, {-1, 0}, {0, 1}, {0, -1}} {
		surface, channel := channelSurfaceAt(seed, worldX+step[0], worldZ+step[1])
		if !channel {
			continue
		}
		if !ok || surface > top {
			top, ok = surface, true
		}
	}
	return top, ok
}

// nearRiverBandAt is [riverAt]'s comparison over [riverBankGateBlocks] instead of
// [riverHalfWidthBlocks]: the columns close enough to the level set that a neighbour
// could still be a channel.
//
// **A zero gradient answers inside the gate, where [riverAt] answers no channel.** The
// two are the same decision read in opposite directions: without a local slope there
// is no finite first-order distance, so a channel cannot be claimed and a neighbour
// cannot be ruled out. A gate has to fail towards asking.
func nearRiverBandAt(seed, worldX, worldZ int64) bool {
	distanceInField := absInt64(riverField(seed, worldX, worldZ) - one/2)
	gx, gz := riverGradientAt(seed, worldX, worldZ)
	ax, az := absInt64(gx), absInt64(gz)
	gradientMagnitude := max(ax, az) + min(ax, az)/2
	if gradientMagnitude == 0 {
		return true
	}
	return distanceInField*(2*riverGradientSpan) <= riverBankGateBlocks*gradientMagnitude
}

// channelSurfaceAt is the water surface of the channel at one column, or ok = false
// where that column carries none.
//
// **It is [shapeAt]'s raw channel arm rather than [shapeAt], because [shapeAt] now
// applies [riverBankAt].** A bank reads its neighbours; a bank that read them through
// the function carrying the bank rule would resolve its neighbours' neighbours, and
// that recursion has no bottom. The settlement-edge clamp is deliberately absent too:
// it lowers source water, not the raw terrace the other bank was already raised to.
// Keeping that terrace here leaves #786's bank and the settlement blend byte-identical.
// What this reads instead is the origin square, the raw channel and the settlement
// exemption, cheapest first, so the fbm2D behind [riverAt] rejects most columns before
// anything else is paid for.
//
// **The settlement call terminates, and the argument is worth writing down.** It is
// reached only once [riverAt] and the sea-line test have both passed, so this column
// *is* a channel wherever the exemption does not apply; [settlementShapeAt]'s blend arm
// asks [loweredHeightBeforeSettlementChannelRuleAt], which returns on its raw channel
// branch without reaching the bank rule.
func channelSurfaceAt(seed, worldX, worldZ int64) (int, bool) {
	if nearOriginColumn(worldX, worldZ) {
		return 0, false
	}
	if !riverAt(seed, worldX, worldZ) {
		return 0, false
	}
	surface := riverSurfaceAt(seed, worldX, worldZ)
	if surface < seaLevel {
		return 0, false
	}
	base := unloweredHeightAt(seed, worldX, worldZ)
	if _, _, near := settlementShapeAt(seed, worldX, worldZ, base, ClimateAt(seed, worldX, worldZ)); near {
		return 0, false
	}
	return surface, true
}

// riverFallTopAt is how high the water in a channel column stands because the channel
// beside it stands higher: the top of the fall that covers their shared terrace face,
// or this column's own surface where no higher terrace touches it.
//
// **A terrace step is a fall, and until #654 nobody wrote one.** [riverSurfaceAt] floors
// the smoothed land, so two adjacent channel columns can differ by a whole terrace; the
// upper pool then stood with three blocks of its face against open air, which is the
// shape a waterfall has and none of the substance. The comment at [riverSurfaceAt] said
// the flow automaton would pour it. Before #653 the automaton could not pour anything at
// all; since #653 it can, and pouring the same fall on every composition of every chunk
// costs 557 block updates a chunk, broadcast to every client watching it. Writing the
// answer into the terrain costs nothing and is byte-identical on every load.
//
// **Flowing water and not a source, which is the whole distinction.** A source is
// permanent by construction — [NextWater]'s first arm — so a painted source would be a
// pillar of water standing in the air for ever. Flowing water is the automaton's to
// keep: it is exactly what that function settles this cell to, it drains the moment the
// channel above stops feeding it, and it is what carries the falling bit the client
// draws with.
//
// **Every terrace face is a fall, whichever way the source current points.** A current
// says which neighbour the automaton feeds; it does not make the source's other faces
// cease to exist. Leaving one of those faces open writes permanent source water against
// air. #696 restricted this rule to downstream faces to avoid broad curtains, but that
// was compensating for a channel whose field threshold could make it 27 blocks wide.
// Since #784 the channel is measured in blocks, so covering every shared terrace face
// produces narrow falls without weakening the containment rule.
//
// **A settlement clamp does not erase the upstream face.** Its channel still exists,
// and ownSurface is the clamped source height, while a higher raw terrace beside it
// remains a fall. The voxels between the two are [WaterFlow7], never permanent sources:
// the runtime automaton owns whether that face keeps being fed.
//
// Four neighbours and no diagonals, because a fall pours across a shared face. Only
// channel columns are asked, and only their field and surface: this is
// [riverChannelAt]'s two conditions, not a whole [columnAt].
func riverFallTopAt(seed, worldX, worldZ int64, ownSurface int) int {
	top := ownSurface
	for _, step := range [4][2]int64{{1, 0}, {-1, 0}, {0, 1}, {0, -1}} {
		x, z := worldX+step[0], worldZ+step[1]
		if !riverAt(seed, x, z) {
			continue
		}
		if surface := riverSurfaceAt(seed, x, z); surface >= seaLevel {
			top = max(top, surface)
		}
	}
	return top
}

// nearOriginColumn reports whether a column is inside the square around the world's
// origin column that water leaves alone. See [originWaterClearance].
func nearOriginColumn(worldX, worldZ int64) bool {
	return absInt64(worldX-originColumnX) <= originWaterClearance &&
		absInt64(worldZ-originColumnZ) <= originWaterClearance
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
// The water itself is the column's own — plain [Water] for a sea or a basin, and one of
// the four current sources for a river, so every voxel of a channel says which way it
// runs. See [column.waterBlock].
//
// The caller has already established that the terrain here is air.
func (c *column) fillAt(worldY int) Block {
	if !c.standingWater || worldY > c.fallSurface {
		return Air
	}
	// Above this column's own pool and under the one next door: the fall, and flowing
	// rather than source so the automaton owns it. See [riverFallTopAt].
	if worldY > c.waterSurface {
		return WaterFlow7
	}
	if c.climate == Tundra && worldY == c.waterSurface {
		return Ice
	}
	return c.waterBlock
}

// caveFillAt is what stands in a carved voxel: water below the dry-world cave level,
// water up to this column's standing surface where it has one and is a body rather than
// a channel, and air above.
//
// Both are world heights rather than depths. The fallback level makes dry-land cave water
// one body a walk descends to; the column surface makes a carved space beneath a lake part
// of that lake without consulting any neighbouring column.
//
// **The column's own surface is asked first, and the order is what carries the current.**
// A carved voxel under a sea or a basin is that body's water; one under dry ground is an
// aquifer with no direction to have, and plain [Water] is what says so.
//
// **A channel is excluded above [caveWaterLevel] since #654, and this is the one place a
// measurement overruled a rule rather than tuning it.** A river's surface is its own
// terrace, which [riverSurfaceAt] lets stand two hundred blocks up; filling every carved
// voxel beneath it to that height put a column of source water through the whole rock
// under the channel. Where the cave then opened sideways into dry ground — 847 such faces
// in one 128x128 window at seed 1 — that was water with nothing holding it, and since
// #653 gave the automaton a way to pour, it poured: 1639 of the 4457 block updates a
// freshly generated eight-chunk volume settled through went into flooding caves.
//
// Excluding it costs #593's sentence only for channels. A sea or a basin still fills its
// caves to [seaLevel], which is eleven blocks above [caveWaterLevel] and is the case #593
// was written for — a cave under a lake is still part of the lake. What is gone is the
// claim that a river carries an aquifer to its own height, which nothing about water says
// and which the rock under a terraced channel made absurd.
//
// **Measured, over the same eight-chunk volume run to a fixed point:**
//
//	                                      settles in   changes   into caves
//	as it stood                            44 steps      4457       1639
//	with the terrace fall painted          44 steps      3777       1389
//	and this exclusion                     10 steps      1738          0
//
// **Flowing water here was tried and is much worse**, which is worth recording because it
// is the obvious idea: emitting the deep fill as [WaterFlow7] so it drains itself turned
// 4457 changes into 413847, because a body that drains and is re-fed from the sources
// beside it churns instead of settling. The fill is source or it is absent.
// **A body reaches a carved voxel only where the carved run reaches it, and that is
// the wet half of #660.** The rule above answers for one column, so a pocket sealed
// inside the rock under a lake filled to the lake's surface with nothing joining the
// two — and where a cave then ran sideways out of that pocket, the wall of source
// water stood against open air for as long as the world ran. [column.bodyFloor] is the
// last solid block under this column's own ground, so `worldY > c.bodyFloor` is "the
// space the body opens into". One downward scan per water column, no neighbour read.
//
// **The other candidate was the four-neighbour containment test, and it was measured
// and rejected.** Written as a carved voxel taking the body's water only where none of
// its four neighbours is air at that height, it costs nothing worth measuring because
// it is reached only by a carved voxel a body has already filled. What it does not do is
// work. A pocket is a *blob* of water, and a one-step test on the unadjusted neighbour
// peels its outermost shell and exposes the next one, so it converges on nothing. The
// connectivity rule reaches that fixed point directly and also removes the sealed
// pocket that filled for no reason.
//
// **The current containment counts live in the test that asserts them, and since #786
// they are zero.** #784 narrowed the channel, #785 restored every terrace face as
// flowing water, and the residue those two left — 27 of 24754 in the legacy seed-one
// window, 181 of 136744 in the wider report window — was never a carved face this rule
// could reach: all of it was a channel standing above the ground beside it, which
// [riverBankAt] now raises. [TestNoSourceWaterStandsAgainstOpenAir] measures no exposure
// in any of its eight open-country windows, still rejects any it cannot classify, and
// carries two settlement windows whose named residue #828 owns.
func (c *column) caveFillAt(worldY int) Block {
	if c.standingWater && worldY <= c.waterSurface {
		// A body fills the rock beneath it; a channel does not, above the dry-world
		// level. See the paragraph on #654 above for the measurement that chose this.
		//
		// Above that level the body reaches only what the carved run reaches: see
		// [column.bodyFloor] and the #660 paragraph above.
		if worldY <= caveWaterLevel {
			return c.waterBlock
		}
		if !c.river && worldY > c.bodyFloor {
			return c.waterBlock
		}
	}
	if worldY <= caveWaterLevel {
		return Water
	}
	return Air
}

// bankWater is the open water one column stands in: the band (floor, top] that
// [column.fillAt] fills above that column's own ground, and therefore the water a
// carve in the column beside it would open a face into.
//
// **The zero value holds nothing and needs no flag to say so**: no height is both
// above zero and at or below it, so a dry column's band is empty by arithmetic rather
// than by a bool somebody has to remember to read.
//
// int32 rather than int, because [column] carries four of these and is copied once per
// voxel — see the note on that field.
type bankWater struct {
	floor int32
	top   int32
}

// bankWaterAt resolves that band for one column, and what it deliberately does not
// resolve is the whole point of it existing.
//
// It is asked for columns nobody is generating — the ring just outside a chunk — so it
// may not be [columnAt]: the gravel patch, the beach, the river current and the fall
// above it say nothing about whether water stands here, and [riverCurrentAt] alone
// costs four field samples and two smoothed heights. What is left is the climate and
// the shape, which is the part no column can be resolved without.
// bankWatersAt resolves the four bands one column reads, in the order
// [column.waterStandsBesideAt] walks them.
func bankWatersAt(seed, worldX, worldZ int64) [4]bankWater {
	return [4]bankWater{
		bankWaterAt(seed, worldX+1, worldZ),
		bankWaterAt(seed, worldX-1, worldZ),
		bankWaterAt(seed, worldX, worldZ+1),
		bankWaterAt(seed, worldX, worldZ-1),
	}
}

func bankWaterAt(seed, worldX, worldZ int64) bankWater {
	surface, riverSurface, river, _ := shapeAt(seed, worldX, worldZ, ClimateAt(seed, worldX, worldZ))
	top, ok := standingWaterSurface(surface, riverSurface, river)
	if !ok {
		return bankWater{}
	}
	return bankWater{floor: int32(surface), top: int32(top)}
}

// waterStandsBesideAt reports whether one of the four horizontally adjacent columns
// stands open water at this height — the water a carve here would open a face into.
//
// Four comparisons against bands [columnAt] has already resolved, so the per-voxel
// price of the bank rule is four comparisons and not one field sample. See
// [column.carvedAt].
func (c *column) waterStandsBesideAt(worldY int64) bool {
	y := int32(worldY)
	for _, bank := range c.banks {
		if y > bank.floor && y <= bank.top {
			return true
		}
	}
	return false
}
