package world

// Terrain shape for a Fimbulvetr world of climates: tundra, taiga, plains and
// desert, with mountains wherever the land folds hard, ore below, conifers at a
// density each climate decides, caves winding under all of it and water standing in
// everything the sea line reaches. Ruins arrive in their own issue; every feature
// here remains a pure integer function of the seed and world coordinate.
//
// Climate itself lives in climate.go, caves in caves.go and water in water.go. This
// file reads all three: HeightAt for shape — which basins and river channels are
// part of, because every consumer has to agree about where the ground is — blockAt
// for material, plantAtColumn for growth, caveAt for what is hollow and fillAt for
// what stands in the hollows the sea line reaches.
const (
	// terrainScaleBlocks is how many blocks span one noise lattice cell. Larger is
	// smoother; 96 gives ridges a few chunks wide rather than per-chunk static.
	terrainScaleBlocks = 96

	// baseHeight is the height the terrain varies around.
	baseHeight = 64

	// The peak-to-trough range, at the two ends of the relief field.
	//
	// **There is no single heightAmplitude any more, and that is the whole of the
	// mountain feature.** One number gave every column of the world the same forty
	// blocks of range, so an hour's walk was an hour of the same hill. Relief
	// interpolates between these two instead, smoothly and independently of
	// climate, so a mountain is somewhere the land folds hard rather than a place
	// with a name — and a desert can have peaks, and a taiga can be flat.
	//
	// Twelve is nearly level: a plain with dunes rather than a table. A hundred and
	// fifty puts the tallest ground at baseHeight + 75, which is what makes the
	// stone and snow lines below reachable by walking uphill rather than by seed
	// luck. Heights therefore range over roughly [-11, 139]; nothing on either side
	// hardcodes an extent, because the world is chunked vertically.
	plainsAmplitude   = 12
	mountainAmplitude = 150

	// stoneLine is the height at or above which a column is bare rock in every
	// climate, whatever grows below it.
	stoneLine = 100

	// snowLine is the height at or above which that bare rock wears a cap of snow.
	//
	// **This used to be 78, and it used to be the only thing that was not grass.**
	// That rule is gone: it made snow a property of the seed rather than of the
	// walk, because the old terrain topped out at 84 and a column either cleared 78
	// or never would. Altitude now overrides climate at two heights instead of one,
	// and both are reachable from any climate by climbing.
	snowLine = 120

	// dirtDepth is how deep the dirt under a plains or taiga surface block reaches.
	// Depth 0 is the surface itself, so this leaves three blocks of dirt.
	dirtDepth = 4

	// The desert column: sand over sandstone over stone. Thicknesses in blocks
	// rather than depths, because two layers stack and the second one's floor is
	// the sum. Four blocks of sand is enough to dig into without hitting rock
	// immediately; eight of sandstone is what makes a desert quarry worth the walk.
	desertSandBlocks      = 4
	desertSandstoneBlocks = 8

	// The tundra column: snow over three blocks of frozen dirt, then stone. Thinner
	// than the plains soil because nothing roots in it.
	tundraDirtBlocks = 3

	// Gravel patches, on plains and taiga only.
	//
	// A small scale — forty-eight blocks to a lattice cell — because a gravel bar
	// is a feature of one hillside rather than of a region, and a threshold high in
	// the field because it should be something you come across rather than
	// something you walk on. The two together cover about three percent of the
	// eligible columns; TestGravelPatchesAreRareAndOnlyOnSoil measures it rather
	// than trusting this sentence.
	gravelScaleBlocks = 48
	gravelThreshold   = one * 75 / 100

	// How many blocks deep a patch reaches: the surface and the one under it.
	gravelDepth = 2

	// oreScaleBlocks is the width of one 3D noise lattice cell. Twelve blocks
	// makes neighbouring hits join into veins without filling a whole chunk.
	oreScaleBlocks = 12

	// Coal begins below the dirt and occupies the shallower stone band. Iron's
	// first possible layer is strictly below the coal band's floor.
	coalMinDepth = dirtDepth + 1
	coalMaxDepth = 24
	ironMinDepth = coalMaxDepth + 1
	ironMaxDepth = 56

	// **A threshold on fbm3D is not the share of the band it selects, and reading it
	// as one is what emptied the world of ore.** Both numbers were `one * 90 / 100`,
	// written as "the top ten percent of the field's range". fbm3D averages four
	// octaves, so its distribution is bell-shaped and almost nothing reaches either
	// extreme: 0.90 selects 0.0016% of the eligible band, four orders of magnitude
	// less than the ten percent it read as. Coal came out at one voxel per twenty
	// chunks — present, connected, and unfindable by playing (#540).
	//
	// So the threshold has to be chosen from the field's measured distribution rather
	// than from its range. Sampled over 1.8M coal-band and 2.9M iron-band voxels that
	// are stone, uncarved and inside their band, at oreScaleBlocks = 12 and seed 1234,
	// the share each threshold actually selects:
	//
	//	threshold  0.90     0.85    0.80    0.79    0.78    0.77    0.75    0.70
	//	coal       0.0016%  0.027%  0.27%   0.38%   0.53%   0.72%   1.26%   4.03%
	//	iron       0.0014%  0.029%  0.28%   0.39%   0.55%   0.74%   1.31%   4.21%
	//
	// **Coal is deliberately the more common of the two.** It gates the campfire and
	// the forge, and the forge gates every iron tool and every piece of iron armour,
	// so a player who cannot find coal is stopped at stone; iron sits below it and is
	// meant to be the thing you go down for. The two thresholds were identical before
	// and only the band depths differed, which stated no design call at all.
	//
	// Coal takes 0.77 and iron 0.80. Across the eight seeds TestOreIsDenseEnoughToFind
	// sweeps, that is 0.55% of the coal band and 0.22% of the iron band — about 100
	// coal and 65 iron voxels under one chunk's 32×32 footprint, gathered into a
	// handful of veins rather than scattered, because the field is smooth at twelve
	// blocks. Scarce enough that a shaft is not enough and a tunnel is; not so scarce
	// that the crafting tree is unreachable.
	coalThreshold = one * 77 / 100
	ironThreshold = one * 80 / 100

	coalSeedOffset int64 = 0x243F6A88
	ironSeedOffset int64 = 0x13198A2E

	// One candidate in a climate's denominator becomes a conifer. The decision and
	// its height come from the candidate column's hash alone. Ninety-six columns to
	// a tree is a wood you have to walk through; fifteen hundred is the occasional
	// landmark on open ground. Tundra and desert have no conifer denominator.
	//
	// Every additional species is one row in plantSpeciesTable: its surface,
	// density, independent hash lattice, footprint, map meaning and shape travel
	// together.
	taigaTreeChanceDenominator        = 96
	plainsTreeChanceDenominator       = 1536
	treeMinTrunkHeight                = 4
	treeHeightVariants                = 3
	treeCanopyRadius                  = 2
	treeCanopyBelowCrown              = 2
	treeCanopyAboveCrown              = 1
	treeSeedOffset              int64 = 0x3C6EF372

	// Desert plants use independent hash lattices so sparse palms do not select
	// the same columns as the more common scrub. Palm is first in the species
	// table: a successful palm candidate owns its column before scrub is asked.
	palmChanceDenominator        = 640
	shrubChanceDenominator       = 40
	palmMinTrunkHeight           = 5
	palmHeightVariants           = 3
	palmFrondLength              = 3
	palmSeedOffset         int64 = 0xA54FF53A
	shrubSeedOffset        int64 = 0x510E527F

	// gravelSeedOffset decorrelates the gravel field from every other 2D field. A
	// patch that always sat on the same side of a climate boundary would be a
	// shared lattice showing through, not a decision.
	gravelSeedOffset int64 = 0x299F31D0
)

// If the band constants are ever reordered, this conversion becomes a compile
// error rather than allowing iron above the coal band's floor.
const _ = uint8(ironMinDepth - coalMaxDepth - 1)

// HeightAt returns the terrain surface height at a world column: the y of the
// topmost block before features. A tree may put solid voxels above it; callers
// that need the generated column's ceiling use generatedColumnTop instead.
//
// Exported because it is the terrain-shape determinism contract in one function —
// a pure integer function of (seed, x, z) — and the seam for a caller holding no
// column: the border-continuity test compares neighbouring chunks this way, and the
// height sweeps measure the world this way.
//
// The shape is baseHeight + amplitude(relief) × (noise − ½), and then the two water
// features that move the ground rather than fill it: a basin lowers the column and a
// river channel replaces its height with a fixed bed. The first part is two
// continuous fields multiplied, which is what keeps the mountains seamless —
// amplitude varies as smoothly as the noise it scales, so there is no boundary
// anywhere for a range to end at.
//
// **Climate was deliberately absent here and no longer is**, because basins are
// absent from desert and the height field therefore has to know the classification.
// That is what this costs: ClimateAt's two fields on top of the height and relief
// fields, plus the basin field, plus the river field on ground low enough for a
// channel — three to four times the noise it used to pay.
//
// **Nothing on a hot path pays that, and nothing new should start.** [Generate] goes
// through columnAt, which samples the climate once and hands it to shapeAt, so a
// generated column sees only the added basin and river sums; physics and edits read
// terrain out of the chunk cache rather than from the height field at all; [SpawnAt]
// goes through generatedColumnTop, which is columnAt again. A caller that finds
// itself asking for a height per entity or per tick wants columnAt, not this.
func HeightAt(seed int64, worldX, worldZ int64) int {
	surface, _, _ := shapeAt(seed, worldX, worldZ, ClimateAt(seed, worldX, worldZ))
	return surface
}

// unloweredHeightAt is the terrain before anything moves it: the amplitude-scaled noise
// alone, with no basin, no river bed and no settlement plateau.
//
// **The one definition of "what the land was doing here".** Three rules read it and all
// three would be wrong against the final height: riverMaxSurface asks whether the ground
// is low enough for a channel, the settlement site rules ask whether it is high enough
// to build on, and the plateau blend eases back towards it. Reading the finished surface
// instead would make each of them depend on the order the lowerings happened to run in.
func unloweredHeightAt(seed, worldX, worldZ int64) int {
	// Position in Q16.16 lattice units. Integer division truncates toward zero,
	// which for negative coordinates would mirror the terrain across the origin;
	// floorDiv keeps the field continuous through x = 0.
	nx := floorDiv(worldX<<fracBits, terrainScaleBlocks)
	nz := floorDiv(worldZ<<fracBits, terrainScaleBlocks)

	n := fbm2D(seed, nx, nz) // [0, one]
	return baseHeight + int((amplitudeAt(seed, worldX, worldZ)*(n-one/2))>>fracBits)
}

// shapeAt is [HeightAt] for a caller that already knows the column's climate, and it
// also reports whether the height it returned is a river bed.
//
// **Two things are folded into one function because they are one decision.** Basins
// are absent from desert, so the height field now needs the classification — and
// columnAt has already computed it, so an exported-only form would make every
// generated column sample temperature and humidity twice. And "is this a river"
// cannot be asked again afterwards without paying a second fbm2D for an answer this
// function already had; worse, a second reading could disagree with the ground,
// because a column near spawn passes the river field and has no channel.
func shapeAt(seed, worldX, worldZ int64, climate Climate) (surface int, river, settled bool) {
	base := unloweredHeightAt(seed, worldX, worldZ)

	// The square around spawn keeps the terrain it would have had. See
	// spawnWaterClearance, and spawnCaveClearance beside it: the two exemptions are
	// the same shape and are checked the same way, before any noise is paid for.
	if nearSpawnColumn(worldX, worldZ) {
		return base, false, false
	}

	// **The plateau comes before both water features, and that is the whole of "a
	// settlement's ground does not move".** Inside the radius the surface is the
	// plateau; out to the end of the blend band it eases back towards the land, and
	// no basin and no channel is *applied* anywhere in that band — both of them lower
	// ground, and the ground under a village is the one ground that must not move.
	//
	// **What the band eases towards is the lowered height, not `base`, and the
	// difference is a cliff.** Blending towards the unlowered land put the outer edge
	// of the band at `base` while the column one block further out was already
	// `base - basinAt(…)`, or a river bed twenty blocks down — so a settlement that
	// happened to sit beside a channel was ringed by a wall at exactly
	// `radius + settlementBlendBlocks`. Measured on the sample seed before the fix: six
	// of twenty-three settlements had a step of four blocks or more there and the worst
	// was twenty-two. The band still carries no channel of its own and is still marked
	// `river = false`; it simply lands on the terrain that is actually there.
	if plateau, inside, near := settlementShapeAt(seed, worldX, worldZ, base, climate); near {
		return plateau, false, inside
	}

	surface, river = loweredHeightAt(seed, worldX, worldZ, base, climate)
	return surface, river, false
}

// loweredHeightAt is the land after the two features that cut down into it: a river bed
// where a channel runs, and a basin everywhere else.
//
// **One definition, because two callers need the same answer and one of them is the
// settlement blend.** [shapeAt] returns it for an ordinary column, and
// [settlementShapeAt] eases its plateau towards it at the edge of a settlement — a
// second copy of the arithmetic in either place is how the two ends of that blend stop
// meeting.
//
// **The height test comes before the river field, and the order is the budget.**
// riverMaxSurface rejects high ground with one comparison; the fbm2D behind riverAt is
// only paid where a channel could actually be.
func loweredHeightAt(seed, worldX, worldZ int64, base int, climate Climate) (surface int, river bool) {
	if base <= riverMaxSurface && riverAt(seed, worldX, worldZ) {
		return seaLevel - riverBedDrop, true
	}
	return base - basinAt(seed, worldX, worldZ, climate), false
}

// amplitudeAt is the peak-to-trough range in blocks at one column, in whole
// blocks rather than fixed point.
//
// The relief field decides it, through a smoothstep for the reason value noise
// uses one: interpolating linearly between the two amplitudes would leave the
// derivative jumping at every lattice cell, and the foot of every mountain would
// be a crease. Smoothstep flattens both ends, so a range rises out of the plain
// and levels off at its top.
//
// lerp is exact here: both amplitudes are whole blocks, so the result is a whole
// block between them and no fixed-point residue survives into the height.
func amplitudeAt(seed, worldX, worldZ int64) int64 {
	return lerp(plainsAmplitude, mountainAmplitude, smoothstep(reliefAt(seed, worldX, worldZ)))
}

// WorldgenVersion identifies what this build's Generate returns for a given seed.
//
// **Bump it whenever Generate stops producing the same blocks for the same seed**, and the
// moment you are forced to notice is a concrete one: the golden test below fails and you
// reach for `-update-golden`. Updating the golden without bumping this is the mistake it
// exists to catch.
//
// Why a whole second version field, next to StoreVersion and the seed. The delta format
// stores only what a player changed, on the stated ground that the base is a pure function
// of the seed and can always be recomputed. That is true of a *fixed* generator. Change the
// generator and the same seed yields a different landscape, so the stored indices still all
// resolve and the result is one world wearing another's digging — the exact failure the
// seed check refuses, arriving by the other road. StoreVersion cannot cover it: it guards
// the file's layout, and worldgen can change without a byte of the layout moving.
//
// Honest about what this is: a number a human remembers to change, not a hash of the
// generator. A hash would be enforced and is not practical to compute over a function. The
// golden test is the reminder, and this comment is the rest of it.
//
// 1 → 2: trees and ore.
// 2 → 3: climates, the relief-driven amplitude that replaced a single
// heightAmplitude, the stone and snow lines at 100 and 120 in place of a snow
// line at 78, per-climate surface columns, and gravel patches. Every column in
// every existing world moves; ErrWorldgenMismatch refuses those directories
// rather than serving one landscape wearing another world's digging, and the
// development worlds that costs are accepted losses.
// 3 → 4: caves. Voxels between two and ninety-six blocks under the surface are
// hollowed where two decorrelated 3D fields both sit near their midpoint, mouths
// open that band to daylight in about a twentieth of columns, and both ore bands
// now lie inside a band a tunnel can cut. Nothing above ground moves *except*
// where a mouth removes a surface — but the interior of every hill does, so this
// is the same total break the last bump was.
// 4 → 5: water. A sea line at 47 fills every air voxel above a lower surface,
// basins dig lakes under it, river channels cut a fixed bed across the low ground,
// tundra wears a lid of ice and the deep of the cave system stands in water. Two
// of those change [HeightAt] itself, so the *shape* of the land moves and not only
// what fills it — every column outside the spawn square is a candidate, which
// makes this the same total break the last two bumps were.
// 5 → 6: settlements. A capital stands within two hundred blocks of every spawn and
// villages sit on a two-kilometre lattice across the rest of the world, each on a
// plateau [HeightAt] itself imposes — so the *shape* of the land moves again, and with
// it every tree, ore band, cave mouth and map tile that reads the height field. Three
// blocks are appended to the palette to build with. Only ground within seventy-two
// blocks of a settlement's centre moves, which makes this the narrowest of the five
// breaks; it is a break all the same, because a stored world's deltas would resolve
// onto a landscape that no longer has a village in it.
// 6 → 7: ore density. [coalThreshold] drops from 0.90 to 0.77 and [ironThreshold] to
// 0.80, which is the whole of the change — the bands, the scale and the two seed
// offsets are untouched. Nothing above the dirt line moves and no column changes
// height, so this is by far the narrowest break of the six: it repaints stone inside
// two depth bands and nothing else. It is a break all the same, because the voxel a
// stored delta was recorded against can now be ore rather than the stone it was.
// 7 → 8: the capital's castle. The keep's 15×14×15 becomes a 21×20×21 castle with a
// curtain wall, a courtyard and three floors. **The first bump that moves no ground at
// all**: no settlement radius changed, so [HeightAt] answers exactly what it did and
// every tree, ore band, cave mouth and map tile stays where it was — what moves is the
// voxels of one building in one cell of the lattice. A break all the same, because a
// world played in around its capital would resolve its deltas onto a building of
// another shape.
// 8 → 9: the castle's four corner towers, their corbelled capitals and the walkable
// bridge between the front pair. The footprint, settlement plan and ground remain
// byte-identical; only courses y=8..27 of the capital's centre building move. It is
// still a break for a played-in capital, whose deltas were recorded against version 8's
// lower silhouette.
// 9 → 10: palms and scrub on desert sand. Terrain heights and underground
// materials stay byte-identical, but selected desert columns gain one of three
// appended plant blocks above their surface. Existing deltas cannot be replayed
// against a base that may now hold a trunk, fronds or scrub where version 9 held
// air, so the stored-world precondition advances with the generator.
const WorldgenVersion uint32 = 10

// Generate builds the chunk at coord for seed.
//
// Pure: the same (seed, coord) yields byte-identical blocks, today and after any
// rebuild. The golden test in generate_test.go is what holds that promise, and
// [WorldgenVersion] is what a deliberate break of it has to carry.
func Generate(seed int64, coord Coord) *Chunk {
	chunk := NewChunk(coord)
	originX, originY, originZ := coord.Origin()
	var columns [ChunkSize][ChunkSize]column

	for z := range ChunkSize {
		for x := range ChunkSize {
			worldX := originX + int64(x)
			worldZ := originZ + int64(z)

			// One height and one climate per column, not per voxel: both are the
			// expensive part and neither depends on y.
			col := columnAt(seed, worldX, worldZ)
			columns[x][z] = col

			for y := range ChunkSize {
				worldY := originY + int64(y)
				chunk.Set(x, y, z, col.voxelAt(seed, worldX, worldY, worldZ))
			}
		}
	}

	// **Buildings before trees, and the order is the clip.** Both features fill only
	// air, so whichever runs first wins a contested voxel — and a canopy grown just
	// outside a settlement may overhang its edge. A roof is what should survive that
	// meeting.
	//
	// **No voxel is actually contested yet, and swapping these two lines changes
	// nothing near spawn.** Trees are suppressed inside the radius, and a conifer rooted
	// outside it would have to be within a canopy's reach of a building that is itself
	// well inside — measured over the settlements within three cells of spawn, that
	// never happens. The order is the decision this file wants to have already made
	// when it does, not a behaviour under test.
	placeSettlements(seed, chunk)
	placeTrees(seed, chunk, &columns)

	return chunk
}

// column is everything about one world column that does not depend on y: how high
// it is, what climate it belongs to, and whether it wears a gravel patch.
//
// **It exists so those three are computed once per column rather than once per
// voxel**, which is the same reason the height always was. Every one of them costs
// an fbm sum, and a chunk has 32768 voxels over 1024 columns.
type column struct {
	surface int
	climate Climate
	gravel  bool
	river   bool
	beach   bool

	// settlement is whether this column stands inside a settlement's radius, where
	// the surface is the plateau exactly.
	//
	// **Resolved once per column rather than once per voxel, which is the only reason
	// the feature is affordable.** Three rules read it — no tree roots here, nothing
	// is carved within [settlementCaveClearance] of the surface, and a map tile draws
	// [SurfaceSettlement] — and the first two of those are asked per voxel by their
	// natural callers. [settlementShapeAt] costs twenty-seven fbm sums for a column
	// that is actually in a settlement, so paying it per voxel would cost more than
	// the rest of generation put together.
	settlement bool
}

// columnAt resolves one world column. Pure in (seed, x, z), like everything else
// here — a neighbouring chunk reaches the same answer for a shared column by
// calling this rather than by reading anything.
//
// The climate is computed first and handed down, because both of the fields under
// it need it: heightAt to know whether this column may hold a basin, and blockAt to
// know what its ground is made of.
func columnAt(seed, worldX, worldZ int64) column {
	climate := ClimateAt(seed, worldX, worldZ)
	surface, river, settled := shapeAt(seed, worldX, worldZ, climate)
	return column{
		surface:    surface,
		climate:    climate,
		gravel:     gravelAt(seed, worldX, worldZ, surface, climate),
		river:      river,
		beach:      beachAt(surface, climate),
		settlement: settled,
	}
}

// carvedAt is [caveAt] with this column's settlement exemption applied.
//
// **A settlement's foundations are the one place carving is refused for a reason that
// is not about the cave system.** Inside a radius the surface *is* the plateau, so
// "above Plateau − settlementCaveClearance" is exactly "shallower than that many
// blocks", and the exemption costs one field read and one subtraction on a path that
// otherwise pays two fbm3D sums. Every caller that holds a column goes through here;
// [caveAt] itself stays the plain carve field, which is what caves_test.go measures.
func (c column) carvedAt(seed, worldX, worldY, worldZ int64) bool {
	if c.settlement && int64(c.surface)-worldY < settlementCaveClearance {
		return false
	}
	return caveAt(seed, worldX, worldY, worldZ, c.surface)
}

// carvedTop is [carvedColumnTop] for a caller that already resolved the column, so the
// settlement exemption applies to it too.
func (c column) carvedTop(seed, worldX, worldZ int64) int {
	top := c.surface
	for c.carvedAt(seed, worldX, int64(top), worldZ) {
		top--
	}
	return top
}

// blockAt is [blockAt] with this column's own surfaces layered over it: a river
// bed, a beach, or a gravel patch.
//
// All three are properties of the *column*, so none of them can live in blockAt:
// that function takes a height, a surface and a climate, and deliberately takes no
// seed or coordinate. Keeping the two apart is what lets blockAt stay the one
// statement of "what a climate's ground is made of".
//
// **The order is a precedence and each pair of it is a decision.** A river bed is
// gravel and is one block deep, so it wins over everything: a channel reads as a
// channel. A beach wins over a gravel patch, because sand at the water's edge is
// what says "this is a shore" and a bar of gravel there says nothing. Neither can
// reach an altitude override — both bands sit near the sea line, forty blocks under
// stoneLine — so nothing here can put sand on a mountain.
func (c column) blockAt(worldY int) Block {
	block := blockAt(worldY, c.surface, c.climate)
	depth := c.surface - worldY
	if depth < 0 {
		return block // air above the surface; nothing below layers onto it
	}

	switch {
	case c.river && depth == 0:
		return Gravel
	case c.beach && depth < beachDepth:
		return Sand
	case c.gravel && depth < gravelDepth:
		return Gravel
	}
	return block
}

// voxelAt composes one voxel of a column: the terrain block, then carving, then
// ore.
//
// **The order is the whole of the interaction between caves and ore, and it is the
// reason ore is never left floating in a tunnel.** A carved voxel is Air before
// oreAt is ever asked, and oreAt only ever replaces Stone — so a vein that a
// passage runs through is cut by it rather than hanging in it, and the ore that
// survives is the ore in the wall, which is exactly what a miner is meant to find.
//
// Plants are not here: they are placed over the finished terrain by placeTrees,
// after every column in the chunk has been composed.
func (c column) voxelAt(seed, worldX, worldY, worldZ int64) Block {
	block := c.blockAt(int(worldY))
	switch {
	case block == Air:
		// Above the ground, so this is the sea line's to fill. Water, or the ice on
		// top of it in a tundra, or air above both.
		return c.fillAt(int(worldY))
	case c.carvedAt(seed, worldX, worldY, worldZ):
		// **Carving is asked before the fill, and the two fills are separate rules.**
		// A carved voxel is below the surface by construction, so the sea line above
		// can never reach it — an air pocket under a lake bed stays an air pocket,
		// which is what "no flow" means when the two features meet. What stands in a
		// tunnel is decided by depth alone, by caveFillAt.
		return caveFillAt(int(worldY))
	case block == Stone:
		return oreAt(seed, worldX, worldY, worldZ, c.surface)
	default:
		return block
	}
}

// gravelAt reports whether a column's top blocks are gravel rather than soil.
//
// Plains and taiga only, and only below the stone line: a desert's sand and a
// tundra's snow are what those climates *are*, and above the stone line the
// altitude override has already made the column bare rock. So gravel is a variation
// on soil, never a replacement for a climate's own answer.
func gravelAt(seed, worldX, worldZ int64, surface int, climate Climate) bool {
	if surface >= stoneLine {
		return false
	}
	if climate != Plains && climate != Taiga {
		return false
	}
	return climateField(seed+gravelSeedOffset, worldX, worldZ, gravelScaleBlocks) >= gravelThreshold
}

// oreAt replaces stone inside one of the two depth bands when the corresponding
// 3D field crosses its ridge threshold. Its caller has already established that
// the base voxel is Stone; keeping that precondition outside this function makes
// it impossible for ore to displace dirt, a surface block or a tree.
func oreAt(seed, worldX, worldY, worldZ int64, surface int) Block {
	depth := int64(surface) - worldY
	if depth < coalMinDepth || depth > ironMaxDepth {
		return Stone
	}

	nx := floorDiv(worldX<<fracBits, oreScaleBlocks)
	ny := floorDiv(worldY<<fracBits, oreScaleBlocks)
	nz := floorDiv(worldZ<<fracBits, oreScaleBlocks)

	switch {
	case depth <= coalMaxDepth:
		if fbm3D(seed+coalSeedOffset, nx, ny, nz) >= coalThreshold {
			return CoalOre
		}
	case depth >= ironMinDepth:
		if fbm3D(seed+ironSeedOffset, nx, ny, nz) >= ironThreshold {
			return IronOre
		}
	}
	return Stone
}

// blockAt decides one voxel from its world height, its column's surface and the
// column's climate.
//
// **Altitude overrides climate, and it does so in every climate.** A column at or
// above snowLine is rock under a cap of snow; one at or above stoneLine is bare
// rock. That is what makes a mountain read as a mountain wherever it rises — a
// desert peak is stone and snow, not sand at a great height — and it is why the
// climate switch below only ever describes the ground a walk crosses.
//
// The column each climate builds, from the surface down: desert is sand over
// sandstone over stone, tundra is snow over frozen dirt over stone, and plains and
// taiga share grass over dirt over stone. Depth 0 is the surface block itself.
func blockAt(worldY, surface int, climate Climate) Block {
	if worldY > surface {
		return Air
	}
	depth := surface - worldY

	switch {
	case surface >= snowLine:
		if depth == 0 {
			return Snow
		}
		return Stone
	case surface >= stoneLine:
		return Stone
	}

	switch climate {
	case Desert:
		switch {
		case depth < desertSandBlocks:
			return Sand
		case depth < desertSandBlocks+desertSandstoneBlocks:
			return Sandstone
		default:
			return Stone
		}
	case Tundra:
		switch {
		case depth == 0:
			return Snow
		case depth <= tundraDirtBlocks:
			return Dirt
		default:
			return Stone
		}
	default: // Plains and Taiga share a column; only their tree density differs.
		switch {
		case depth == 0:
			return Grass
		case depth < dirtDepth:
			return Dirt
		default:
			return Stone
		}
	}
}

// coniferChanceDenominator is one candidate column in how many that becomes a
// conifer, for a climate.
//
// **Zero is "nothing grows here", not "a tree every zero columns".** Tundra and
// desert are absent from the switch on purpose and reach the default: an enormous
// denominator would still put the occasional conifer in a desert, and the
// statement being made is that there is none. Its one caller checks the zero
// before it reaches a modulus.
func coniferChanceDenominator(climate Climate) uint64 {
	switch climate {
	case Taiga:
		return taigaTreeChanceDenominator
	case Plains:
		return plainsTreeChanceDenominator
	default:
		return 0
	}
}

func desertChanceDenominator(denominator uint64) func(Climate) uint64 {
	return func(climate Climate) uint64 {
		if climate == Desert {
			return denominator
		}
		return 0
	}
}

// plantSpecies is one complete answer to what may grow in a column. Table order is
// priority: the first row whose refusals all pass owns the root.
type plantSpecies struct {
	name        string
	seedOffset  int64
	rootsOn     func(Block) bool
	denominator func(Climate) uint64
	footprint   int
	forest      bool
	visit       func(seed, rootX, rootZ int64, surface int, h uint64, visit func(x, y, z int64, block Block))
}

var conifer = plantSpecies{
	name:       "conifer",
	seedOffset: treeSeedOffset,
	rootsOn: func(block Block) bool {
		return block == Grass
	},
	denominator: coniferChanceDenominator,
	footprint:   treeCanopyRadius,
	forest:      true,
	visit:       visitConifer,
}

var palm = plantSpecies{
	name:       "palm",
	seedOffset: palmSeedOffset,
	rootsOn: func(block Block) bool {
		return block == Sand
	},
	denominator: desertChanceDenominator(palmChanceDenominator),
	footprint:   palmFrondLength,
	forest:      true,
	visit:       visitPalm,
}

var shrub = plantSpecies{
	name:       "shrub",
	seedOffset: shrubSeedOffset,
	rootsOn: func(block Block) bool {
		return block == Sand
	},
	denominator: desertChanceDenominator(shrubChanceDenominator),
	footprint:   0,
	forest:      false,
	visit:       visitShrub,
}

var plantSpeciesTable = []plantSpecies{conifer, palm, shrub}

// plantAtColumn reports the first species rooted at one resolved column and the
// hash that row uses for its shape.
//
// The refusals remain ordered by cost: settlement, a row's climate denominator,
// its surface, its independent hash, the sea line, then the carve test. The last
// question is an order of magnitude dearer than the others and is reached only by
// a candidate whose density draw already passed.
func plantAtColumn(seed, worldX, worldZ int64, col column) (species *plantSpecies, h uint64, ok bool) {
	return plantAtColumnIn(plantSpeciesTable, seed, worldX, worldZ, col)
}

func plantAtColumnIn(table []plantSpecies, seed, worldX, worldZ int64, col column) (species *plantSpecies, h uint64, ok bool) {
	// Nothing grows inside a settlement. This is the cheapest refusal because the
	// resolved column already carries the answer, and it applies to every row.
	if col.settlement {
		return nil, 0, false
	}

	var surface Block
	surfaceRead := false
	for i := range table {
		species := &table[i]
		denominator := species.denominator(col.climate)
		if denominator == 0 {
			continue
		}
		if !surfaceRead {
			surface = col.blockAt(col.surface)
			surfaceRead = true
		}
		if !species.rootsOn(surface) {
			continue
		}

		h := hashLattice(seed+species.seedOffset, worldX, worldZ)
		if h%denominator != 0 {
			continue
		}

		// A submerged surface may still be valid soil, but a plant rooted there
		// would be clipped by the standing water and appear to float above it.
		if col.surface < seaLevel {
			continue
		}

		// blockAt describes terrain before carving, so only this final question
		// can tell that a cave mouth removed otherwise valid ground.
		if col.carvedAt(seed, worldX, int64(col.surface), worldZ) {
			continue
		}

		return species, h, true
	}
	return nil, 0, false
}

func visitPlant(seed, rootX, rootZ int64, visit func(x, y, z int64, block Block)) {
	visitPlantAtColumn(seed, rootX, rootZ, columnAt(seed, rootX, rootZ), visit)
}

func visitPlantAtColumn(seed, rootX, rootZ int64, col column, visit func(x, y, z int64, block Block)) {
	visitPlantAtColumnIn(plantSpeciesTable, seed, rootX, rootZ, col, visit)
}

func visitPlantAtColumnIn(table []plantSpecies, seed, rootX, rootZ int64, col column, visit func(x, y, z int64, block Block)) {
	species, h, ok := plantAtColumnIn(table, seed, rootX, rootZ, col)
	if ok {
		species.visit(seed, rootX, rootZ, col.surface, h, visit)
	}
}

// visitConifer yields the canopy before the trunk. Leaves only fill air, while a
// trunk may replace a leaf from an overlapping plant, so this ordering makes logs
// continuous without letting foliage overwrite them.
func visitConifer(_ int64, rootX, rootZ int64, surface int, h uint64, visit func(x, y, z int64, block Block)) {
	trunkHeight := coniferTrunkHeight(h)
	crownY := int64(surface + trunkHeight)
	for dy := -treeCanopyBelowCrown; dy <= treeCanopyAboveCrown; dy++ {
		radius := treeCanopyRadius
		switch dy {
		case 0:
			radius = 1
		case treeCanopyAboveCrown:
			radius = 0
		}
		for dz := -radius; dz <= radius; dz++ {
			for dx := -radius; dx <= radius; dx++ {
				// Clip the four corners of the wide layers. The result still reaches
				// radius two on both axes, but reads as a conifer rather than a cube.
				if absInt(dx)+absInt(dz) > radius+1 {
					continue
				}
				visit(rootX+int64(dx), crownY+int64(dy), rootZ+int64(dz), Leaves)
			}
		}
	}
	for y := int64(surface + 1); y <= crownY; y++ {
		visit(rootX, y, rootZ, Log)
	}
}

func visitPalm(_ int64, rootX, rootZ int64, surface int, h uint64, visit func(x, y, z int64, block Block)) {
	trunkHeight := palmTrunkHeight(h)
	trunkTop := int64(surface + trunkHeight)
	crownY := trunkTop + 1

	visit(rootX, crownY, rootZ, PalmFronds)
	for _, direction := range [][2]int64{{1, 0}, {-1, 0}, {0, 1}, {0, -1}} {
		for distance := int64(1); distance <= palmFrondLength; distance++ {
			y := crownY
			if distance == palmFrondLength {
				y--
			}
			visit(rootX+direction[0]*distance, y, rootZ+direction[1]*distance, PalmFronds)
		}
	}
	for _, diagonal := range [][2]int64{{1, 1}, {1, -1}, {-1, 1}, {-1, -1}} {
		visit(rootX+diagonal[0], crownY, rootZ+diagonal[1], PalmFronds)
	}
	for y := int64(surface + 1); y <= trunkTop; y++ {
		visit(rootX, y, rootZ, PalmLog)
	}
}

func visitShrub(_ int64, rootX, rootZ int64, surface int, _ uint64, visit func(x, y, z int64, block Block)) {
	visit(rootX, int64(surface+1), rootZ, DesertShrub)
}

func coniferTrunkHeight(h uint64) int {
	return treeMinTrunkHeight + int((h>>32)%treeHeightVariants)
}

func palmTrunkHeight(h uint64) int {
	return palmMinTrunkHeight + int((h>>32)%palmHeightVariants)
}

// The tree-named helpers keep the existing conifer tests readable. They are
// deliberately thin views of the table's first, conifer row.
func treeAt(seed, worldX, worldZ int64) (surface, trunkHeight int, ok bool) {
	col := columnAt(seed, worldX, worldZ)
	species, h, ok := plantAtColumn(seed, worldX, worldZ, col)
	if !ok || species != &plantSpeciesTable[0] {
		return col.surface, 0, false
	}
	return col.surface, coniferTrunkHeight(h), true
}

func treeAtColumn(seed, worldX, worldZ int64, col column) (trunkHeight int, ok bool) {
	species, h, ok := plantAtColumn(seed, worldX, worldZ, col)
	if !ok || species != &plantSpeciesTable[0] {
		return 0, false
	}
	return coniferTrunkHeight(h), true
}

func visitTree(seed, rootX, rootZ int64, visit func(x, y, z int64, block Block)) {
	col := columnAt(seed, rootX, rootZ)
	species, h, ok := plantAtColumn(seed, rootX, rootZ, col)
	if ok && species == &plantSpeciesTable[0] {
		species.visit(seed, rootX, rootZ, col.surface, h, visit)
	}
}

func largestPlantFootprint() int {
	largest := 0
	for i := range plantSpeciesTable {
		largest = max(largest, plantSpeciesTable[i].footprint)
	}
	return largest
}

func absInt(v int) int {
	if v < 0 {
		return -v
	}
	return v
}

// placeTrees scans plant roots outside the chunk by one complete footprint and
// writes only the yielded voxels that belong to this chunk. Interior roots reuse
// the terrain pass's heights; border roots are recomputed from world coordinates,
// which completes their trees without reading or mutating a neighbour.
func placeTrees(seed int64, chunk *Chunk, columns *[ChunkSize][ChunkSize]column) {
	originX, _, originZ := chunk.Coord.Origin()
	footprint := int64(largestPlantFootprint())
	for rootZ := originZ - footprint; rootZ < originZ+ChunkSize+footprint; rootZ++ {
		for rootX := originX - footprint; rootX < originX+ChunkSize+footprint; rootX++ {
			var col column
			if rootX >= originX && rootX < originX+ChunkSize && rootZ >= originZ && rootZ < originZ+ChunkSize {
				col = columns[int(rootX-originX)][int(rootZ-originZ)]
			} else {
				col = columnAt(seed, rootX, rootZ)
			}

			visitPlantAtColumn(seed, rootX, rootZ, col, func(worldX, worldY, worldZ int64, block Block) {
				setTreeBlock(chunk, worldX, worldY, worldZ, block)
			})
		}
	}
}

func setTreeBlock(chunk *Chunk, worldX, worldY, worldZ int64, block Block) {
	originX, originY, originZ := chunk.Coord.Origin()
	localX, localY, localZ := worldX-originX, worldY-originY, worldZ-originZ
	if localX < 0 || localX >= ChunkSize || localY < 0 || localY >= ChunkSize || localZ < 0 || localZ >= ChunkSize {
		return
	}

	x, y, z := int(localX), int(localY), int(localZ)
	current := chunk.At(x, y, z)
	if current == Air || (block == Log && current == Leaves) || (block == PalmLog && current == PalmFronds) {
		chunk.Set(x, y, z, block)
	}
}

// generatedColumnTop returns the highest generated solid in a column, including
// a canopy rooted in a neighbouring column. It mirrors the same footprint scan as
// placeTrees but never generates a chunk or consults mutable state.
//
// It starts from the *carved* top rather than from the height field, because a
// cave mouth removes the surface voxel and a caller that stood a player on
// HeightAt there would put them inside the hillside.
//
// **Water is not a top and ice is.** The only fill in this world that stops movement
// is the lid a tundra wears at the sea line, so a submerged column's top is still
// its ground — and a frozen one's is the ice, which is a floor a body stands on and
// therefore the highest generated solid in the column.
func generatedColumnTop(seed, worldX, worldZ int64) int {
	col := columnAt(seed, worldX, worldZ)
	top := col.carvedTop(seed, worldX, worldZ)
	if col.surface < seaLevel && col.fillAt(seaLevel) == Ice {
		top = max(top, seaLevel)
	}

	footprint := int64(largestPlantFootprint())
	for rootZ := worldZ - footprint; rootZ <= worldZ+footprint; rootZ++ {
		for rootX := worldX - footprint; rootX <= worldX+footprint; rootX++ {
			visitPlant(seed, rootX, rootZ, func(x, y, z int64, _ Block) {
				if x == worldX && z == worldZ && y > int64(top) && col.blockAt(int(y)) == Air {
					top = int(y)
				}
			})
		}
	}
	return top
}

// GeneratedColumnTop returns the highest solid voxel the procedural world places in
// one block column, including ice, caves and a neighbouring tree canopy.
//
// It is the vertical half of regeneration's safety rule: after player edits are
// forgotten, a body that the restored terrain encloses is lifted above the same base
// Generate composes. Pure in seed and column, so callers do not open or generate a
// chunk to ask it.
func GeneratedColumnTop(seed, worldX, worldZ int64) int {
	return generatedColumnTop(seed, worldX, worldZ)
}

// Spawn placement. The column is fixed; the height is not, because it cannot be.
const (
	// spawnColumnX and spawnColumnZ are the world column every session starts in.
	spawnColumnX = 0
	spawnColumnZ = 0

	// SpawnClearance is how many blocks above the highest generated voxel in the
	// spawn column the player starts. generatedColumnTop includes a neighbouring
	// tree's canopy, so this remains a clearance rather than an assumption that the
	// height field is the last solid block in the column.
	SpawnClearance = 2
)

// **There is no mob anchor here any more, and there must not be one again.** This
// package used to export the column the world's single draugr stood at, because there
// was a single draugr and something had to say where. Creatures are placed by the spawn
// director now — inside the tick, around the players who are connected, from terrain
// read through the collision seam — so terrain has gone back to not knowing what walks
// on it. A "where should a mob go" helper here would be the old model growing back.

// SpawnAt returns where a session starts, for a world seed.
//
// Derived from the generated column, not stated beside it. Its floor begins with
// HeightAt and then accounts for a trunk or canopy rooted nearby. A constant can
// only be right for the terrain and tree parameters it was written against: the
// old fixed y=80 buried the player in rock for high seeds, and HeightAt alone
// would now let a tree occupy the spawn clearance.
//
// Pure, like everything else here: the same seed always yields the same spawn. That
// matters beyond tidiness, because the Fimbulvetr storm regenerates chunks around
// players who expect to come back to the same place.
func SpawnAt(seed int64) [3]float32 {
	top := generatedColumnTop(seed, spawnColumnX, spawnColumnZ)

	// **A session never begins under water.** spawnWaterClearance keeps basins and
	// river channels off this column, but it cannot keep the ordinary height field
	// above the sea line — the terrain is concentrated around 64 against a sea line
	// at 47, and a mountainAmplitude of 150 is wide enough that a sizeable minority of
	// seeds still put the origin on a lake bed with no water feature involved at all.
	// TestASessionNeverBeginsUnderWater is that sweep. Lifting the
	// reference to the sea line puts the player on the surface of the water instead
	// of inside it: they swim rather than drown, which is the fail-safe direction and
	// the only one the swim rules make sense in.
	//
	// Deliberately not a lowering: a column that stands above the sea line is
	// untouched, so this is the sea line acting as a floor under the clearance rather
	// than a second placement rule.
	top = max(top, seaLevel)

	return [3]float32{
		spawnColumnX + 0.5, // centred in the column rather than on its corner
		float32(top + SpawnClearance),
		spawnColumnZ + 0.5,
	}
}
