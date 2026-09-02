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
	// landmark on open ground. One tree in five hundred and twelve columns makes
	// the tundra a sparse tree line; the desert has no conifer denominator at all.
	//
	// Every additional species is one row in plantSpeciesTable: its surface,
	// density, independent hash lattice, footprint, map meaning and shape travel
	// together.
	taigaTreeChanceDenominator        = 96
	tundraTreeChanceDenominator       = 512
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

	// Plains plants use their own lattices. A broadleaf is four times as common
	// there as the conifer that precedes it in the table, while bushes are the
	// ground cover a player should see from anywhere on open grass.
	//
	// **A taiga bush is half as common as a plains one, and rarer than the tree it
	// grows under.** One column in 128 against the plains' 64 is undergrowth rather
	// than a second meadow, and putting it behind taigaTreeChanceDenominator (96)
	// keeps the wood the feature: a walk through the pines passes more trunks than
	// bushes. Tundra and desert stay absent from bushChanceDenominator's switch,
	// which is the same statement coniferChanceDenominator makes about the desert.
	broadleafChanceDenominator        = 384
	plainsBushChanceDenominator       = 64
	taigaBushChanceDenominator        = 128
	broadleafMinTrunkHeight           = 3
	broadleafCanopyRadius             = 2
	broadleafSeedOffset         int64 = 0x9B05688C
	bushSeedOffset              int64 = 0x1F83D9AB

	// gravelSeedOffset decorrelates the gravel field from every other 2D field. A
	// patch that always sat on the same side of a climate boundary would be a
	// shared lattice showing through, not a decision.
	gravelSeedOffset int64 = 0x299F31D0

	// Flowers. One plains grass column in five carries one, but only inside a patch,
	// and **the patch is the gravel mechanism at a meadow's scale**: gravelAt takes
	// the top quarter of a 2D field over forty-eight blocks, this the top 28% of its
	// own over forty, on no shared lattice. flowerStrayDenominator is how many
	// flowers in a drift take the *next* colour instead of the cell's own.
	//
	// **A taiga drift is a third of a plains one: one column in fifteen.** The patch
	// field is the row's and not the climate's, so a taiga drift falls where a plains
	// one would and covers the same share of the world; what changes is how thickly
	// it blooms inside. One in five is a carpet on open grass, one in fifteen is
	// colour scattered between the trunks — still a drift, because the same field
	// still says where, and never a sprinkle. Tundra and desert are absent for the
	// reason a bush is: a snow-rooted or sand-rooted flower is a species nobody has
	// decided on, not a density set to zero.
	plainsFlowerChanceDenominator        = 5
	taigaFlowerChanceDenominator         = 15
	flowerStrayDenominator        uint64 = 5
	flowerPatchScaleBlocks               = 40
	flowerPatchThreshold                 = one * 72 / 100
	flowerSeedOffset              int64  = 0x082EFA98
	flowerPatchSeedOffset         int64  = 0xEC4E6C89
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
// reads the capital's plateau off the lattice. A caller that finds
// itself asking for a height per entity or per tick wants columnAt, not this.
func HeightAt(seed int64, worldX, worldZ int64) int {
	surface, _, _, _ := shapeAt(seed, worldX, worldZ, ClimateAt(seed, worldX, worldZ))
	return surface
}

// unloweredHeightAt is the terrain before anything moves it: the amplitude-scaled noise
// alone, with no basin, no river bed and no settlement plateau.
//
// **The one definition of "what the land was doing here".** Three rules read it and all
// three would be wrong against the final height: [riverSurfaceAt] averages it to decide
// how high a channel's water stands, the settlement site rules ask whether it is high
// enough to build on, and the plateau blend eases back towards it. Reading the finished
// surface instead would make each of them depend on the order the lowerings happened to
// run in.
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
// because a column near the origin passes the river field and has no channel.
func shapeAt(seed, worldX, worldZ int64, climate Climate) (surface, riverSurface int, river, settled bool) {
	base := unloweredHeightAt(seed, worldX, worldZ)

	// The square around the origin column keeps the terrain it would have had. See
	// originWaterClearance, and originCaveClearance beside it: the two exemptions are
	// the same shape and are checked the same way, before any noise is paid for.
	if nearOriginColumn(worldX, worldZ) {
		return base, 0, false, false
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
		return plateau, 0, false, inside
	}

	surface, riverSurface, river = loweredHeightAt(seed, worldX, worldZ, base, climate)
	return surface, riverSurface, river, false
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
// **The height test that used to come first is gone, and with it its budget.**
// riverMaxSurface rejected high ground before the fbm2D behind riverAt was paid for; a
// river now runs at any height, so every column pays that sum and a channel column pays
// [riverSurfaceAt]'s five height fields on top. It is affordable because a channel is a
// curve: [riverAt]'s block-width band selects a few percent of columns and only those
// reach the expensive half. BenchmarkGenerate is the check.
//
// riverSurface is meaningful only beside river, exactly as [column.waterSurface] is
// beside [column.standingWater].
func loweredHeightAt(seed, worldX, worldZ int64, base int, climate Climate) (surface, riverSurface int, river bool) {
	if bed, waterSurface, ok := riverChannelAt(seed, worldX, worldZ, base); ok {
		return bed, waterSurface, true
	}
	return base - basinAt(seed, worldX, worldZ, climate), 0, false
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
// 10 → 11: tundra conifers. Sparse conifers now root in tundra snow and carry one
// snow block above the crown; every terrain column and every conifer outside tundra
// stays byte-identical. A stored delta in one of the newly wooded columns nevertheless
// resolves against a different base, so the narrow break still needs its own version.
// 11 → 12: broadleaf trees and bushes on plains grass. Heights and underground
// materials stay byte-identical, as do every non-plains column, but selected plains
// columns gain one of two appended foliage blocks above the surface. A stored delta
// there may now resolve against a tree or bush instead of air, so this is a feature
// break even though no terrain height moved.
// 12 → 13: caves under standing water fill hydrostatically to that column's water
// surface instead of stopping at the global deep-cave level. Terrain heights and
// every uncarved voxel stay byte-identical, but carved air below a sea, basin or river
// may become water, so stored deltas in those caves need the new base version.
// 16 → 17: bushes and flowers on taiga grass. The two low-cover rows gated on
// plainsChanceDenominator, which answers zero for every climate but the plains, so
// there was not one bush and not one flower in the taiga; they now carry their own
// per-climate switches at one column in 128 and one drift column in fifteen.
// Terrain heights, underground materials, every tree in every climate and every
// #654 moves water: a terrace step now carries the fall that pours down it, written as
// flowing water rather than left as three blocks of open air against the upper pool, and
// a channel column no longer fills the carved rock beneath it to its own terrace. Both
// change generated blocks, so both carry this.
// 18 → 19: #660 makes a carved voxel and the water beside it agree, which is the last of
// the three ways generation wrote water nothing holds. A body reaches a carved voxel only
// where the carved run reaches it, so a pocket sealed in the rock under a lake is air
// rather than a permanent source; and a carve that would open a face into a neighbouring
// column's standing water is refused, so the rock at a lake shore or a river bank stays.
// Terrain heights are byte-identical and nothing above ground moves, but carved voxels
// under a body may become air, a thin shell of carved voxels beside one becomes rock —
// and a cave mouth that shell fills in is ground a plant may now root on. Stored deltas
// in any of those resolve against a different base, so the precondition advances.
// 20 → 21: #696 makes generated terrace falls obey their higher source's encoded
// current. A lower channel no longer fills to every higher adjacent terrace; only a
// source pointing across their shared face paints the fall. Terrain height and river
// sources stay byte-identical, but flowing-water voxels above lower terraces change,
// so stored deltas need the new generated base.
// 21 → 22: #712 adds the capital's stable and widens its three concentric layout
// radii to hold the nineteen-block paddock with clearance. Villages and terrain beyond
// a capital's settlement reach stay byte-identical; the capital plateau, blend and
// building voxels change, so stored deltas there need the new generated base.
// 22 → 23: #784 measures a river's distance from its level set in blocks instead of
// field units. Channels keep a stable physical width across seeds instead of widening
// into lakes where the field crosses its midpoint slowly, so river beds, water and
// nearby composition all move and stored deltas need the new generated base.
// 23 → 24: #785 restores every generated terrace face as flowing water rather than
// leaving a permanent source exposed wherever its encoded current points elsewhere.
// River beds, source water and terrain heights stay byte-identical, but flowing-water
// voxels above lower terraces change, so stored deltas need the new generated base.
const WorldgenVersion uint32 = 24

// Generate builds the chunk at coord for seed.
//
// Pure: the same (seed, coord) yields byte-identical blocks, today and after any
// rebuild. The golden test in generate_test.go is what holds that promise, and
// [WorldgenVersion] is what a deliberate break of it has to carry.
func Generate(seed int64, coord Coord) *Chunk {
	chunk := NewChunk(coord)
	originX, originY, originZ := coord.Origin()
	var columns [ChunkSize][ChunkSize]column

	// **Two passes over the columns, and the second one is what the bank rule costs
	// here rather than four times over.** Every column needs the standing water of
	// its four horizontal neighbours (see [column.carvedAt]), and inside a chunk
	// those neighbours are columns this loop has already resolved — a shape carries
	// its own band, for nothing, in [column.bank]. Only the ring of columns just
	// outside the chunk has to be resolved on its own, and a ring is 128 columns to
	// the chunk's 1024. Asking [columnAt] here would pay 4096 [bankWaterAt] for the
	// same answer; with [placeTrees]'s footprint scan paying its own on top, that
	// measured 6.5 ms per chunk against the 4.0 ms this costs and a 3.9 ms baseline.
	for z := range ChunkSize {
		for x := range ChunkSize {
			// One height and one climate per column, not per voxel: both are the
			// expensive part and neither depends on y.
			columns[x][z] = columnShapeAt(seed, originX+int64(x), originZ+int64(z))
		}
	}

	// The four edges of that ring, indexed along the axis they run down. Corners are
	// absent because no column asks for one: the rule reads the four columns sharing
	// a face, never the four sharing an edge.
	var westward, eastward, northward, southward [ChunkSize]bankWater
	for i := range ChunkSize {
		westward[i] = bankWaterAt(seed, originX-1, originZ+int64(i))
		eastward[i] = bankWaterAt(seed, originX+ChunkSize, originZ+int64(i))
		northward[i] = bankWaterAt(seed, originX+int64(i), originZ-1)
		southward[i] = bankWaterAt(seed, originX+int64(i), originZ+ChunkSize)
	}
	bandAt := func(x, z int) bankWater {
		switch {
		case x < 0:
			return westward[z]
		case x >= ChunkSize:
			return eastward[z]
		case z < 0:
			return northward[x]
		case z >= ChunkSize:
			return southward[x]
		}
		return columns[x][z].bank()
	}

	for z := range ChunkSize {
		for x := range ChunkSize {
			worldX := originX + int64(x)
			worldZ := originZ + int64(z)

			col := columns[x][z]
			col.banks = [4]bankWater{
				bandAt(x+1, z), bandAt(x-1, z),
				bandAt(x, z+1), bandAt(x, z-1),
			}
			// Written back, so everything downstream — the trees, the settlements —
			// holds the same column [columnAt] would have handed it.
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
	// nothing near the origin.** Trees are suppressed inside the radius, and a conifer
	// rooted outside it would have to be within a canopy's reach of a building that is
	// itself well inside — measured over the settlements within three cells of the origin
	// column, that never happens. The order is the decision this file wants to have already made
	// when it does, not a behaviour under test.
	placeSettlements(seed, chunk)
	placeTrees(seed, chunk, &columns)

	return chunk
}

// column is everything about one world column that does not depend on y: how high
// it is, what climate it belongs to, whether it wears a gravel patch, and whether
// standing water fills it to a surface of its own.
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

	// standingWater is true for a sea or basin column lowered below seaLevel and for
	// every river column. waterSurface is meaningful only beside it: the sea line for
	// sea and basins, and the channel's own terrace for a river.
	//
	// waterBlock is what that water is made of. Plain [Water] for a sea or a basin,
	// which have no direction to carry, and one of the four [CurrentOf] sources for a
	// river, so every water voxel of a channel — the fill above the bed and the carved
	// volume under it alike — says which way it runs. Resolved once per column because
	// [riverCurrentAt] costs four field samples and two smoothed heights, and a channel
	// column has a cave system's worth of voxels to answer for.
	//
	// **Its zero value is [Air], and [column.fillAt] and [column.caveFillAt] read it
	// verbatim rather than defending against that.** [columnAt] is the only constructor
	// and always sets it, so an unset field can only come from a hand-built literal in
	// this package — where air standing where water belongs fails the assertion that
	// built the column, while defaulting the zero to [Water] would let a channel column
	// that lost its block fill with directionless water and say nothing.
	standingWater bool
	waterSurface  int
	waterBlock    Block

	// fallSurface is the top of the water in this column once the channel beside it is
	// counted: [waterSurface] where nothing pours in, and the higher neighbour's terrace
	// where one does. Everything between the two is flowing water rather than source —
	// see [riverFallTopAt] for why a fall must not be permanent.
	//
	// Never below [waterSurface], so [column.fillAt] may test it first and reach the
	// source rule underneath.
	fallSurface int

	// banks is the open water the four horizontally adjacent columns stand in, and
	// bodyFloor is the last solid block under this column's own body. They are the two
	// halves of one rule — a carved voxel and the water beside it must agree — and each
	// is resolved once per column because both would otherwise be a neighbour read per
	// voxel. See [column.carvedAt] and [column.caveFillAt].
	//
	// bodyFloor is meaningful only beside [standingWater]; its zero for a dry column
	// says nothing and nothing reads it there.
	banks     [4]bankWater
	bodyFloor int

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
	col := columnShapeAt(seed, worldX, worldZ)
	col.banks = bankWatersAt(seed, worldX, worldZ)
	return col
}

// bank is the open water this column stands in, as a neighbour reads it.
//
// **It is [bankWaterAt]'s answer, arrived at from a column that has already been
// resolved**, which is what lets [Generate] fill a chunk's interior bands for
// nothing. The two are the same two numbers out of [standingWaterSurface], so they
// cannot disagree by construction; TestAColumnCarriesTheSameBankItResolvesAlone is
// the pin that says so anyway.
func (c *column) bank() bankWater {
	if !c.standingWater {
		return bankWater{}
	}
	return bankWater{floor: int32(c.surface), top: int32(c.waterSurface)}
}

// columnShapeAt is [columnAt] without the four neighbouring bands: everything about a
// column that is a function of its own coordinate alone.
//
// **A column with no bands answers [column.waterStandsBesideAt] false everywhere,
// which is the pre-#660 carve, so only a caller that fills them in itself may hold
// one.** There are two, and both exist to keep four [bankWaterAt] off a scan that does
// not need them: [Generate] fills the bands from columns it has already resolved, and
// [plantAtColumnIn] resolves them at the one refusal that reads them. Anything else
// wants [columnAt].
func columnShapeAt(seed, worldX, worldZ int64) column {
	climate := ClimateAt(seed, worldX, worldZ)
	surface, riverSurface, river, settled := shapeAt(seed, worldX, worldZ, climate)
	waterSurface, standingWater := standingWaterSurface(surface, riverSurface, river)

	// A channel is never a shore, and this is where that is said. A terraced bed can
	// land anywhere, the beach band included, and [column.blockAt] would then put
	// gravel on top of two blocks of sand under three of water. See [beachAt], which
	// stays the plain band rule.
	waterBlock := Water
	fallSurface := waterSurface
	if river {
		waterBlock = waterCurrentBlock(riverCurrentAt(seed, worldX, worldZ))
		fallSurface = riverFallTopAt(seed, worldX, worldZ, waterSurface)
	}

	col := column{
		surface:       surface,
		climate:       climate,
		gravel:        gravelAt(seed, worldX, worldZ, surface, climate),
		river:         river,
		beach:         !river && beachAt(surface, climate),
		standingWater: standingWater,
		waterSurface:  waterSurface,
		fallSurface:   fallSurface,
		waterBlock:    waterBlock,
		settlement:    settled,
	}

	// The downward scan is paid only by a column that has a body to be part of, and
	// it costs one [column.carveFieldAt] for the overwhelming majority of them: a
	// lake bed that is not itself carved ends the run at the first step.
	if standingWater {
		col.bodyFloor = col.carveRunFloor(seed, worldX, worldZ)
	}
	return col
}

// carveFieldAt is [caveAt] with this column's settlement exemption applied.
//
// **A settlement's foundations are the one place carving is refused for a reason that
// is not about the cave system.** Inside a radius the surface *is* the plateau, so
// "above Plateau − settlementCaveClearance" is exactly "shallower than that many
// blocks", and the exemption costs one field read and one subtraction on a path that
// otherwise pays two fbm3D sums. [caveAt] itself stays the plain carve field, which is
// what caves_test.go measures.
//
// **It is separate from [column.carvedAt] so that [column.carveRunFloor] has something
// to ask.** That scan produces `bodyFloor`, the bank rule refuses a carve on the
// strength of a fill that reads `bodyFloor`, and asking the refined answer while
// building it would be circular. Refusing a carve only ever raises the floor, so this
// run is the longest there could be — the safe direction, since the extra water it
// admits stands against rock the bank rule has already left in place.
func (c *column) carveFieldAt(seed, worldX, worldY, worldZ int64) bool {
	if c.settlement && int64(c.surface)-worldY < settlementCaveClearance {
		return false
	}
	return caveAt(seed, worldX, worldY, worldZ, c.surface)
}

// carvedAt is [column.carveFieldAt] with the bank rule applied: a carve that would
// open a face into the water standing in a neighbouring column is refused, and the
// rock stays.
//
// **This is the dry half of #660, and it is the half neither candidate rule in that
// issue reaches.** The wet half — a pocket the carved run never reaches, filled
// because [column.caveFillAt] answers for one column — is drained there, and the whole
// account of both is in that function's #660 paragraph. What is left over is the shape
// seed 1 is made of: the water is the *channel's own*, sitting in the river where it
// belongs, and the air is a cave carved through the bank beside it. Nothing may be done
// to that water, so the answer is done to the carve.
//
// It costs four comparisons on a voxel the carve field has already accepted, and it is
// asked last, so the overwhelming majority of voxels never reach it.
func (c *column) carvedAt(seed, worldX, worldY, worldZ int64) bool {
	if !c.carveFieldAt(seed, worldX, worldY, worldZ) {
		return false
	}
	// Only a voxel this column would leave as air can breach anything: one that holds
	// the body's own water is the body, not a hole in it.
	if c.caveFillAt(int(worldY)) != Air {
		return true
	}
	return !c.waterStandsBesideAt(worldY)
}

// carveRunFloor is the last solid block under this column's own ground: the floor of
// the unbroken carved run that reaches the surface, and therefore the floor of the
// space a body of standing water above actually opens into.
//
// Water reaches a carved voxel at y exactly when y > carveRunFloor. A column whose
// surface is not carved answers with the surface itself, which is every column with no
// cave mouth in its bed — one call, and the loop below never turns.
//
// The scan is bounded by the carve field, which stops at [caveMaxDepth] by
// construction, so no bound of its own is needed.
func (c *column) carveRunFloor(seed, worldX, worldZ int64) int {
	floor := c.surface
	for c.carveFieldAt(seed, worldX, int64(floor), worldZ) {
		floor--
	}
	return floor
}

// carvedTop is [carvedColumnTop] for a caller that already resolved the column, so the
// settlement exemption applies to it too.
func (c *column) carvedTop(seed, worldX, worldZ int64) int {
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
func (c *column) blockAt(worldY int) Block {
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
// **A pointer receiver, and it is chosen by measurement.** [column] gained four
// [bankWater] bands at #660 and this runs once per voxel — 32768 times a chunk — so the
// copy a value receiver takes is what those bands really cost: 4.4 ms per chunk against
// 4.0 for identical work through a pointer, on a 3.9 ms baseline. Every method on
// [column] takes one for that reason.
func (c *column) voxelAt(seed, worldX, worldY, worldZ int64) Block {
	block := c.blockAt(int(worldY))
	switch {
	case block == Air:
		// Above the ground, so this is the sea line's to fill. Water, or the ice on
		// top of it in a tundra, or air above both.
		return c.fillAt(int(worldY))
	case c.carvedAt(seed, worldX, worldY, worldZ):
		// Carving is asked before ore and then receives the column's hydrostatic fill.
		// Deep caves still fill to caveWaterLevel everywhere; under standing water the
		// same carved volume fills all the way to that column's water surface. Both are
		// per-column answers, so generation stays pure and chunk-local.
		return c.caveFillAt(int(worldY))
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
// **Zero is "nothing grows here", not "a tree every zero columns".** The desert is
// absent from the switch on purpose and reaches the default: an enormous denominator
// would still put the occasional conifer there, and the statement being made is that
// there is none. Its one caller checks the zero before it reaches a modulus.
func coniferChanceDenominator(climate Climate) uint64 {
	switch climate {
	case Taiga:
		return taigaTreeChanceDenominator
	case Tundra:
		return tundraTreeChanceDenominator
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

func plainsChanceDenominator(denominator uint64) func(Climate) uint64 {
	return func(climate Climate) uint64 {
		if climate == Plains {
			return denominator
		}
		return 0
	}
}

// bushChanceDenominator is one candidate column in how many that becomes a bush,
// for a climate.
//
// Plains and taiga share grass over dirt over stone (see blockAt), so the row's
// rootsOn already accepts both surfaces and only this number decides where the
// ground cover grows. **Tundra and desert are absent on purpose and reach the
// default zero**, in the sense coniferChanceDenominator's comment gives it: not a
// density nobody has tuned, but the statement that a bush rooted in snow or sand is
// a species this world has not decided on. plantAtColumnIn checks the zero before
// it reaches a modulus.
func bushChanceDenominator(climate Climate) uint64 {
	switch climate {
	case Plains:
		return plainsBushChanceDenominator
	case Taiga:
		return taigaBushChanceDenominator
	default:
		return 0
	}
}

// flowerChanceDenominator is one candidate column in how many that carries a
// flower, for a climate — inside a drift, because the row's patch field gates every
// climate alike and nothing here changes that.
//
// Tundra and desert are absent for the reason they are absent above.
func flowerChanceDenominator(climate Climate) uint64 {
	switch climate {
	case Plains:
		return plainsFlowerChanceDenominator
	case Taiga:
		return taigaFlowerChanceDenominator
	default:
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

	// patch is a region gate on top of the density draw: a row grows only where this
	// answers true. **nil means everywhere, and every tree row is nil** — a conifer's
	// distribution is one number per climate, and a field sample there would say
	// "trees cluster" without anybody having decided it should. Flowers are the one
	// row that wants it, because a drift is the feature and a sprinkle is not.
	patch func(seed, worldX, worldZ int64) bool

	visit func(seed, rootX, rootZ int64, surface int, h uint64, visit func(x, y, z int64, block Block))
}

var conifer = plantSpecies{
	name:       "conifer",
	seedOffset: treeSeedOffset,
	rootsOn: func(block Block) bool {
		return block == Grass || block == Snow
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

var broadleaf = plantSpecies{
	name:       "broadleaf",
	seedOffset: broadleafSeedOffset,
	rootsOn: func(block Block) bool {
		return block == Grass
	},
	denominator: plainsChanceDenominator(broadleafChanceDenominator),
	footprint:   broadleafCanopyRadius,
	forest:      true,
	visit:       visitBroadleaf,
}

var bush = plantSpecies{
	name:       "bush",
	seedOffset: bushSeedOffset,
	rootsOn: func(block Block) bool {
		return block == Grass
	},
	denominator: bushChanceDenominator,
	footprint:   1,
	forest:      false,
	visit:       visitBush,
}

// The flower is last, which is the whole of its priority: any plant that wants the
// column takes it and a flower grows in what is left, so a drift never thins a wood.
var flower = plantSpecies{
	name:       "flower",
	seedOffset: flowerSeedOffset,
	rootsOn: func(block Block) bool {
		return block == Grass
	},
	denominator: flowerChanceDenominator,
	footprint:   0,
	forest:      false,
	patch:       flowerPatchAt,
	visit:       visitFlower,
}

var plantSpeciesTable = []plantSpecies{conifer, palm, shrub, broadleaf, bush, flower}

// flowerPatchAt reports whether a column lies inside a drift of flowers.
func flowerPatchAt(seed, worldX, worldZ int64) bool {
	return climateField(seed+flowerPatchSeedOffset, worldX, worldZ, flowerPatchScaleBlocks) >= flowerPatchThreshold
}

// flowerPatchCell is the lattice cell of the drift a column belongs to. floorDiv for
// the reason climateField uses it: truncation toward zero maps x = -1 and x = 0 to
// one cell, so the drifts either side of the axis would share a colour.
func flowerPatchCell(worldX, worldZ int64) (int64, int64) {
	return floorDiv(worldX, flowerPatchScaleBlocks), floorDiv(worldZ, flowerPatchScaleBlocks)
}

// flowerBlock chooses which of the three flowers stands in a column.
//
// **The patch cell decides the colour and the column decides whether it is a stray.**
// A whole drift shares one hash, so it is mostly one colour; one flower in
// flowerStrayDenominator takes the next along.
//
// **The stray draw reads the high half of h, and that is not a taste question.** The
// density draw has already established h%flowerChanceDenominator(climate) == 0 for
// every column reaching here, so the low bits are spent and a stray test against them
// would be true for every flower in the world. visitBush reads (h>>40) for the same
// reason.
func flowerBlock(seed, worldX, worldZ int64, h uint64) Block {
	cellX, cellZ := flowerPatchCell(worldX, worldZ)
	colour := hashLattice(seed+flowerPatchSeedOffset, cellX, cellZ) % 3
	if (h>>32)%flowerStrayDenominator == 0 {
		colour = (colour + 1) % 3
	}
	return FlowerRed + Block(colour)
}

// plantAtColumn reports the first species rooted at one resolved column and the
// hash that row uses for its shape.
//
// The refusals remain ordered by cost: settlement, a row's climate denominator, its
// surface, its independent hash, the sea line, the row's optional patch field, then
// the carve test. The last two are the dear ones — one 2D field then two 3D ones —
// and are reached only by a candidate whose density draw already passed.
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
	surfaceRead, banksRead := false, false
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
		// Snow is tundra ground or an altitude cap. A row may name Snow as a
		// surface, but a mountain does not become tundra merely because its peak is
		// white: climate remains the authoritative half of that distinction.
		if surface == Snow && col.climate != Tundra {
			continue
		}

		h := hashLattice(seed+species.seedOffset, worldX, worldZ)
		if h%denominator != 0 {
			continue
		}

		// A submerged surface may still be valid soil, but a plant rooted there
		// would be clipped by the standing water and appear to float above it.
		//
		// **The column's own water line, not the sea's.** The two were the same test
		// while every river bed sat under the sea line; a terraced channel runs at any
		// height, so the sea line alone would let a conifer root in a bed two hundred
		// blocks up and grow a canopy over the water.
		if col.standingWater {
			continue
		}

		// A row that names a region grows only inside it; nil is "everywhere".
		if species.patch != nil && !species.patch(seed, worldX, worldZ) {
			continue
		}

		// blockAt describes terrain before carving, so only this final question
		// can tell that a cave mouth removed otherwise valid ground.
		//
		// **The four bands are resolved here rather than carried in, and that is a
		// measurement rather than a preference.** [column.carvedAt] is the one plant
		// refusal that reads them and it is the last of seven, so a column reaching
		// this line is rare — while [placeTrees] scans (ChunkSize + 2·footprint)²
		// columns for every chunk. Resolving four neighbours for each of those cost
		// 1.0 ms of a 3.9 ms chunk; resolving them here costs nothing measurable.
		// [placeTrees] and [visitPlant] therefore hand over a [columnShapeAt].
		if !banksRead {
			col.banks = bankWatersAt(seed, worldX, worldZ)
			banksRead = true
		}
		if col.carvedAt(seed, worldX, int64(col.surface), worldZ) {
			continue
		}

		return species, h, true
	}
	return nil, 0, false
}

func visitPlant(seed, rootX, rootZ int64, visit func(x, y, z int64, block Block)) {
	visitPlantAtColumn(seed, rootX, rootZ, columnShapeAt(seed, rootX, rootZ), visit)
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
func visitConifer(seed int64, rootX, rootZ int64, surface int, h uint64, visit func(x, y, z int64, block Block)) {
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
	// The cap is a tundra shape decision, never a test for Snow under the root:
	// every climate can wear altitude snow above snowLine, and those caps stay bare.
	// setTreeBlock gives Snow the same air-only placement as Leaves, so an overlap
	// clips the cap honestly instead of overwriting an existing tree.
	if ClimateAt(seed, rootX, rootZ) == Tundra {
		visit(rootX, crownY+treeCanopyAboveCrown+1, rootZ, Snow)
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

// visitBroadleaf yields its crown before its trunk, as visitConifer does. The
// three equally wide lower layers and clipped corners make a round crown rather
// than a point; the fourth layer closes it with a one-block radius.
func visitBroadleaf(_ int64, rootX, rootZ int64, surface int, h uint64, visit func(x, y, z int64, block Block)) {
	trunkHeight := broadleafTrunkHeight(h)
	crownY := int64(surface + trunkHeight)
	for dy := -1; dy <= 2; dy++ {
		radius := broadleafCanopyRadius
		if dy == 2 {
			radius = 1
		}
		for dz := -radius; dz <= radius; dz++ {
			for dx := -radius; dx <= radius; dx++ {
				if absInt(dx)+absInt(dz) > radius+1 {
					continue
				}
				visit(rootX+int64(dx), crownY+int64(dy), rootZ+int64(dz), BroadLeaves)
			}
		}
	}
	for y := int64(surface + 1); y <= crownY; y++ {
		visit(rootX, y, rootZ, Log)
	}
}

func visitBush(_ int64, rootX, rootZ int64, surface int, h uint64, visit func(x, y, z int64, block Block)) {
	y := int64(surface + 1)
	visit(rootX, y, rootZ, Bush)
	if (h>>40)&1 == 0 {
		return
	}
	if (h>>41)&1 == 0 {
		visit(rootX+1, y, rootZ, Bush)
		return
	}
	visit(rootX, y, rootZ+1, Bush)
}

// visitFlower yields one block at surface + 1: a flower stands in the voxel above the
// ground rather than replacing any of it.
func visitFlower(seed int64, rootX, rootZ int64, surface int, h uint64, visit func(x, y, z int64, block Block)) {
	visit(rootX, int64(surface+1), rootZ, flowerBlock(seed, rootX, rootZ, h))
}

func coniferTrunkHeight(h uint64) int {
	return treeMinTrunkHeight + int((h>>32)%treeHeightVariants)
}

func palmTrunkHeight(h uint64) int {
	return palmMinTrunkHeight + int((h>>32)%palmHeightVariants)
}

func broadleafTrunkHeight(h uint64) int {
	return broadleafMinTrunkHeight + int((h>>32)%2)
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
				// No bands: [plantAtColumnIn] resolves them for the few columns
				// that reach its carve test. See the note there.
				col = columnShapeAt(seed, rootX, rootZ)
			}

			visitPlantAtColumn(seed, rootX, rootZ, col, func(worldX, worldY, worldZ int64, block Block) {
				setTreeBlock(chunk, worldX, worldY, worldZ, block)
			})
		}
	}
}

// setTreeBlock writes one plant voxel; its condition is the whole of what a plant may
// overwrite, and **ground cover counts as air here, which makes the result
// independent of the order two roots are visited in.** A bush's second block and a
// flower can want one voxel: flower-first the bush overwrites it because [Cover] is
// true, bush-first the flower is refused because Bush is neither air nor cover.
func setTreeBlock(chunk *Chunk, worldX, worldY, worldZ int64, block Block) {
	originX, originY, originZ := chunk.Coord.Origin()
	localX, localY, localZ := worldX-originX, worldY-originY, worldZ-originZ
	if localX < 0 || localX >= ChunkSize || localY < 0 || localY >= ChunkSize || localZ < 0 || localZ >= ChunkSize {
		return
	}

	x, y, z := int(localX), int(localY), int(localZ)
	current := chunk.At(x, y, z)
	if current == Air || Cover(current) || (block == Log && (current == Leaves || current == BroadLeaves)) || (block == PalmLog && current == PalmFronds) {
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
			visitPlant(seed, rootX, rootZ, func(x, y, z int64, block Block) {
				// Cover is not a top: a column whose only feature is a flower keeps
				// its ground as its highest solid. [Solid] rather than an id list.
				if !Solid(block) {
					return
				}
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

// The world's origin column, and the clearance a body is put down with.
const (
	// originColumnX and originColumnZ are the world's origin column: the anchor the
	// settlement lattice measures the capital's offset from, and the column
	// [originWaterClearance] and [originCaveClearance] protect.
	//
	// **Nothing begins a session here.** [SpawnAt] read the generated ground at this
	// column until #519 moved the join onto the capital's gate square, which is why the
	// two clearances beside it exist at all; both stay, because they shape generated
	// blocks and removing one would move terrain and bump [WorldgenVersion] for a
	// tidy-up.
	originColumnX = 0
	originColumnZ = 0

	// SpawnClearance is how many blocks above the ground a body is put down: the
	// capital's plateau at join (see [SpawnAt]), a settlement's plateau on the middle
	// respawn tier, and the generated column top when regeneration lifts a body out of
	// restored terrain. A clearance rather than an assumption that the ground is the last
	// solid block in the column — generatedColumnTop includes a neighbouring tree's
	// canopy.
	SpawnClearance = 2
)

// **There is no mob anchor here any more, and there must not be one again.** This
// package used to export the column the world's single draugr stood at, because there
// was a single draugr and something had to say where. Creatures are placed by the spawn
// director now — inside the tick, around the players who are connected, from terrain
// read through the collision seam — so terrain has gone back to not knowing what walks
// on it. A "where should a mob go" helper here would be the old model growing back.

// SpawnAt returns where a session starts, for a world seed: the capital's gate square,
// [capitalSpawnOffset] blocks along +Z from its centre, on its plateau.
//
// **The game starts in the city it built for that purpose.** It used to start on the
// origin column — open country the lattice measures from, 120 to 200 blocks from the
// capital — so a new player's first minutes were a walk towards something they could not
// see. The capital is a pure function of the seed and always exists ([capitalSiteAt]
// ranks its candidate sites rather than refusing them), so this is as determinate an
// answer as the origin column was.
//
// **The plateau is flat by construction**, so there is no generated column to sample and
// no canopy to clear, and it cannot be under water, because [settlementMinPlateau] is
// three blocks of freeboard the capital's fallback keeps. The `max(top, seaLevel)` floor
// the origin column needed is therefore gone, asserted as a test rather than kept as
// code: a floor that can never fire is a claim nobody can read the truth of. Still pure
// and still generating nothing, which the Fimbulvetr storm's regeneration depends on.
//
// **The column is not checked for headroom, and it does not need to be.** Collision
// treats a non-resident voxel as solid, so a player whose spawn chunk has not streamed
// yet stands still until it arrives — exactly as they did on the origin column.
func SpawnAt(seed int64) [3]float32 {
	capital := CapitalAt(seed)
	return [3]float32{
		float32(capital.CentreX) + 0.5, // centred in the column rather than on its corner
		float32(capital.Plateau + SpawnClearance),
		float32(capital.CentreZ+capitalSpawnOffset) + 0.5,
	}
}
