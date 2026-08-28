package world

// Water: the sea line every low column fills to, the basins that dig lakes into the
// ground below it, the rivers that run across it, the ice that lies on it where the
// climate is cold enough, and the streams standing in the deep of the caves.
//
// **Water here is static, and that is a design decision rather than a stage.** A
// voxel is [Water] because the generator says so at that coordinate — never because
// something flowed into it. Three consequences follow, and all three are why it is
// worth stating: generation stays the pure integer function of (seed, x, y, z) the
// storm needs it to be; a placed block that displaces water is one delta like any
// other edit, because nothing has to be recomputed around it; and mining a wall
// beside a lake leaves a dry hole, because the lake was never a volume of anything.
// Flow, sources, buckets and freezing are all out of scope by the same token — see
// the issue, which lists them.
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

	// Rivers: a channel cut to a fixed bed, wherever a slow field crosses its own
	// midpoint and the land is low enough to be crossed.
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

	// riverBedDrop is how far under the sea line a river bed sits, so that a channel
	// is three blocks of water deep wherever it runs.
	riverBedDrop = 3

	// riverMaxSurface is the highest unlowered surface a river may cut through.
	//
	// **This is the whole of "rivers stop where the land climbs".** Without it a
	// channel at a fixed bed height would cut a slot straight through a mountain,
	// because the field that decides where a river is knows nothing about how high
	// the ground is there. Twenty-four blocks over the sea line is seven over
	// baseHeight: a river crosses rolling ground and ends at the foot of anything
	// that deserves the name of a hill.
	//
	// **What it does not remove is the gorge, and that is deliberate.** A fixed bed
	// under land that rises to the limit is a channel with walls, and at the top of
	// the band those walls are the better part of twenty blocks. That is the shape a
	// fixed bed has; softening it is a different river and a different issue.
	//
	// It is read from the *unlowered* height — the terrain before basins and before
	// this rule — so a river's course is a property of the land rather than of the
	// order two lowerings happened to be applied in.
	riverMaxSurface = seaLevel + 24

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
	// **Water underground is a depth rule and not a fill rule**, which is what keeps
	// it from draining every hillside: a tunnel that breaks into a lake bed from
	// below would otherwise have to decide what happens, and nothing here flows. At
	// 36 the streams sit eleven blocks under the sea line and well inside the carved
	// band (caveMaxDepth is 96 below a surface that averages 64), so a walk down a
	// tunnel reaches water before it reaches the bottom of the cave system, and never
	// reaches water by walking sideways under a lake.
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

// The beach band only names a band while its two halves bound one: reorder them and
// this conversion is a compile error rather than a shore that is never sand.
const _ = uint8(beachAboveSea + beachBelowSea)

// A river bed has to be under the sea line, or a river holds no water at all.
const _ = uint8(riverBedDrop - 1)

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
// very little — the same shape as treeChanceDenominator's absent case.
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
// the water and not where it would have met it. A river bed sits at
// seaLevel-riverBedDrop, below the band, so a channel keeps its gravel rather than
// turning into a sand ditch.
func beachAt(surface int, climate Climate) bool {
	if climate != Plains && climate != Taiga {
		return false
	}
	return surface >= seaLevel-beachBelowSea && surface <= seaLevel+beachAboveSea
}

// fillAt is what stands in an air voxel of this column: water up to the sea line,
// ice on the top of it where the climate is cold enough, and air above.
//
// **Ice is one voxel thick and only ever at the sea line**, which is what makes it a
// lid rather than a frozen lake: everything under it is still water, so a hole
// broken in the surface is a way in. It is also the only [Solid] block in this file,
// so a tundra shore is something you walk out onto.
//
// The caller has already established that the terrain here is air, so the only
// bound this needs is the sea line above.
func (c column) fillAt(worldY int) Block {
	if worldY > seaLevel {
		return Air
	}
	if c.climate == Tundra && worldY == seaLevel {
		return Ice
	}
	return Water
}

// caveFillAt is what stands in a carved voxel: the still water in the deep of the
// cave system, and air above it.
//
// A world height rather than a depth, deliberately. A depth-based rule would put a
// stream at the bottom of every tunnel however high the hillside it runs through,
// which reads as water clinging to the roof of the world; a single level makes the
// underground water one body that a walk descends *to*.
func caveFillAt(worldY int) Block {
	if worldY <= caveWaterLevel {
		return Water
	}
	return Air
}
