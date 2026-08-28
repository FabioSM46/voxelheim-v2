package world

// Terrain shape for a Fimbulvetr world of climates: tundra, taiga, plains and
// desert, with mountains wherever the land folds hard, ore below and conifers at
// a density each climate decides. Caves, water and ruins arrive in their own
// issues; every feature here remains a pure integer function of the seed and
// world coordinate.
//
// Climate itself lives in climate.go. This file reads it: HeightAt for shape,
// blockAt for material, treeAtColumn for density.
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

	// The fBm sum is concentrated around its midpoint; these thresholds leave
	// narrow connected ridges instead of replacing most of a depth band.
	coalThreshold = one * 90 / 100
	ironThreshold = one * 90 / 100

	coalSeedOffset int64 = 0x243F6A88
	ironSeedOffset int64 = 0x13198A2E

	// One candidate in a climate's denominator becomes a conifer. The decision and
	// its height come from the candidate column's hash alone.
	//
	// **Density is the only thing that distinguishes a taiga from a plain here**, by
	// design: one tree species at two spacings, because a second species is a
	// different piece of work. Ninety-six columns to a tree is a wood you have to
	// walk through; fifteen hundred is the occasional landmark on open ground.
	// Tundra and desert have no denominator at all — see treeChanceDenominator,
	// where the absent case is nothing rather than a very large number.
	taigaTreeChanceDenominator        = 96
	plainsTreeChanceDenominator       = 1536
	treeMinTrunkHeight                = 4
	treeHeightVariants                = 3
	treeCanopyRadius                  = 2
	treeCanopyBelowCrown              = 2
	treeCanopyAboveCrown              = 1
	treeSeedOffset              int64 = 0x3C6EF372

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
// Exported because it is the seam every consumer needs — the generator fills
// columns with it, the border-continuity test compares neighbouring chunks
// through it, and spawn placement starts from it. It is the terrain-shape
// determinism contract in one function: a pure integer function of (seed, x, z).
//
// The shape is baseHeight + amplitude(relief) × (noise − ½). Two continuous
// fields multiplied, which is what keeps the mountains seamless: amplitude varies
// as smoothly as the noise it scales, so there is no boundary anywhere for a
// range to end at. Climate is deliberately absent — where the land is high is not
// the same question as what grows on it.
func HeightAt(seed int64, worldX, worldZ int64) int {
	// Position in Q16.16 lattice units. Integer division truncates toward zero,
	// which for negative coordinates would mirror the terrain across the origin;
	// floorDiv keeps the field continuous through x = 0.
	nx := floorDiv(worldX<<fracBits, terrainScaleBlocks)
	nz := floorDiv(worldZ<<fracBits, terrainScaleBlocks)

	n := fbm2D(seed, nx, nz) // [0, one]
	return baseHeight + int((amplitudeAt(seed, worldX, worldZ)*(n-one/2))>>fracBits)
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
const WorldgenVersion uint32 = 3

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
				block := col.blockAt(int(worldY))
				if block == Stone {
					block = oreAt(seed, worldX, worldY, worldZ, col.surface)
				}
				chunk.Set(x, y, z, block)
			}
		}
	}

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
}

// columnAt resolves one world column. Pure in (seed, x, z), like everything else
// here — a neighbouring chunk reaches the same answer for a shared column by
// calling this rather than by reading anything.
func columnAt(seed, worldX, worldZ int64) column {
	climate := ClimateAt(seed, worldX, worldZ)
	surface := HeightAt(seed, worldX, worldZ)
	return column{surface: surface, climate: climate, gravel: gravelAt(seed, worldX, worldZ, surface, climate)}
}

// blockAt is [blockAt] with this column's gravel patch layered over it.
//
// The patch is a property of the *column*, so it cannot live in blockAt: that
// function takes a height, a surface and a climate, and deliberately takes no seed
// or coordinate. Keeping the two apart is what lets blockAt stay the one statement
// of "what a climate's ground is made of".
func (c column) blockAt(worldY int) Block {
	block := blockAt(worldY, c.surface, c.climate)
	if !c.gravel {
		return block
	}
	if depth := c.surface - worldY; depth < 0 || depth >= gravelDepth {
		return block
	}
	return Gravel
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

// treeChanceDenominator is one candidate column in how many that becomes a
// conifer, for a climate.
//
// **Zero is "nothing grows here", not "a tree every zero columns".** Tundra and
// desert are absent from the switch on purpose and reach the default: an enormous
// denominator would still put the occasional conifer in a desert, and the
// statement being made is that there is none. Its one caller checks the zero
// before it reaches a modulus.
func treeChanceDenominator(climate Climate) uint64 {
	switch climate {
	case Taiga:
		return taigaTreeChanceDenominator
	case Plains:
		return plainsTreeChanceDenominator
	default:
		return 0
	}
}

// treeAt reports the conifer rooted at one world column. Nothing about the
// destination chunk participates: a neighbouring chunk reaches the same answer
// by recomputing this candidate from (seed, column).
func treeAt(seed, worldX, worldZ int64) (surface, trunkHeight int, ok bool) {
	col := columnAt(seed, worldX, worldZ)
	trunkHeight, ok = treeAtColumn(seed, worldX, worldZ, col)
	return col.surface, trunkHeight, ok
}

// treeAtColumn reports the conifer rooted at one resolved column.
//
// **The grass test is still the whole of "what may a tree stand on".** It now
// answers for four climates rather than one, and it answers no in every case that
// matters without naming any of them: a tundra surface is snow, a desert's is
// sand, a mountain's is stone or snow above the altitude lines, and a gravel patch
// is gravel. The density check is second because the surface is cheaper to reject
// on than a hash is to interpret.
func treeAtColumn(seed, worldX, worldZ int64, col column) (trunkHeight int, ok bool) {
	denominator := treeChanceDenominator(col.climate)
	if denominator == 0 {
		return 0, false
	}
	if col.blockAt(col.surface) != Grass {
		return 0, false
	}

	h := hashLattice(seed+treeSeedOffset, worldX, worldZ)
	if h%denominator != 0 {
		return 0, false
	}

	trunkHeight = treeMinTrunkHeight + int((h>>32)%treeHeightVariants)
	return trunkHeight, true
}

// visitTree yields the canopy before the trunk. Leaves only fill air, while a
// trunk may replace a leaf from an overlapping tree, so this ordering makes logs
// continuous without letting foliage overwrite them.
func visitTree(seed, rootX, rootZ int64, visit func(x, y, z int64, block Block)) {
	visitTreeAtColumn(seed, rootX, rootZ, columnAt(seed, rootX, rootZ), visit)
}

func visitTreeAtColumn(seed, rootX, rootZ int64, col column, visit func(x, y, z int64, block Block)) {
	trunkHeight, ok := treeAtColumn(seed, rootX, rootZ, col)
	if !ok {
		return
	}

	surface := col.surface
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

func absInt(v int) int {
	if v < 0 {
		return -v
	}
	return v
}

// placeTrees scans roots outside the chunk by one complete canopy footprint and
// writes only the yielded voxels that belong to this chunk. Interior roots reuse
// the terrain pass's heights; border roots are recomputed from world coordinates,
// which completes their trees without reading or mutating a neighbour.
func placeTrees(seed int64, chunk *Chunk, columns *[ChunkSize][ChunkSize]column) {
	originX, _, originZ := chunk.Coord.Origin()
	for rootZ := originZ - treeCanopyRadius; rootZ < originZ+ChunkSize+treeCanopyRadius; rootZ++ {
		for rootX := originX - treeCanopyRadius; rootX < originX+ChunkSize+treeCanopyRadius; rootX++ {
			var col column
			if rootX >= originX && rootX < originX+ChunkSize && rootZ >= originZ && rootZ < originZ+ChunkSize {
				col = columns[int(rootX-originX)][int(rootZ-originZ)]
			} else {
				col = columnAt(seed, rootX, rootZ)
			}

			visitTreeAtColumn(seed, rootX, rootZ, col, func(worldX, worldY, worldZ int64, block Block) {
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
	if current == Air || (block == Log && current == Leaves) {
		chunk.Set(x, y, z, block)
	}
}

// generatedColumnTop returns the highest generated solid in a column, including
// a canopy rooted in a neighbouring column. It mirrors the same footprint scan as
// placeTrees but never generates a chunk or consults mutable state.
func generatedColumnTop(seed, worldX, worldZ int64) int {
	col := columnAt(seed, worldX, worldZ)
	top := col.surface

	for rootZ := worldZ - treeCanopyRadius; rootZ <= worldZ+treeCanopyRadius; rootZ++ {
		for rootX := worldX - treeCanopyRadius; rootX <= worldX+treeCanopyRadius; rootX++ {
			visitTree(seed, rootX, rootZ, func(x, y, z int64, _ Block) {
				if x == worldX && z == worldZ && y > int64(top) && col.blockAt(int(y)) == Air {
					top = int(y)
				}
			})
		}
	}
	return top
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

	return [3]float32{
		spawnColumnX + 0.5, // centred in the column rather than on its corner
		float32(top + SpawnClearance),
		spawnColumnZ + 0.5,
	}
}
