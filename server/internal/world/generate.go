package world

// Terrain shape for a Fimbulvetr coastline of hills and fjords, with ore below
// and sparse conifers on the exposed grass. Caves, biomes and ruins arrive in
// their own issues; every feature here remains a pure integer function of the
// seed and world coordinate.
const (
	// terrainScaleBlocks is how many blocks span one noise lattice cell. Larger is
	// smoother; 96 gives ridges a few chunks wide rather than per-chunk static.
	terrainScaleBlocks = 96

	// baseHeight is the height the terrain varies around.
	baseHeight = 64

	// heightAmplitude is the peak-to-trough range in blocks.
	heightAmplitude = 40

	// snowLine is the height at or above which the surface block is snow.
	snowLine = 78

	// dirtDepth is how many blocks of dirt sit under the surface block.
	dirtDepth = 4

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

	// One candidate in treeChanceDenominator becomes a conifer. The decision and
	// its height come from the candidate column's hash alone.
	treeChanceDenominator       = 512
	treeMinTrunkHeight          = 4
	treeHeightVariants          = 3
	treeCanopyRadius            = 2
	treeCanopyBelowCrown        = 2
	treeCanopyAboveCrown        = 1
	treeSeedOffset        int64 = 0x3C6EF372
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
func HeightAt(seed int64, worldX, worldZ int64) int {
	// Position in Q16.16 lattice units. Integer division truncates toward zero,
	// which for negative coordinates would mirror the terrain across the origin;
	// floorDiv keeps the field continuous through x = 0.
	nx := floorDiv(worldX<<fracBits, terrainScaleBlocks)
	nz := floorDiv(worldZ<<fracBits, terrainScaleBlocks)

	n := fbm2D(seed, nx, nz) // [0, one]
	return baseHeight + int((n*heightAmplitude)>>fracBits) - heightAmplitude/2
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
const WorldgenVersion uint32 = 2

// Generate builds the chunk at coord for seed.
//
// Pure: the same (seed, coord) yields byte-identical blocks, today and after any
// rebuild. The golden test in generate_test.go is what holds that promise, and
// [WorldgenVersion] is what a deliberate break of it has to carry.
func Generate(seed int64, coord Coord) *Chunk {
	chunk := NewChunk(coord)
	originX, originY, originZ := coord.Origin()
	var heights [ChunkSize][ChunkSize]int

	for z := range ChunkSize {
		for x := range ChunkSize {
			worldX := originX + int64(x)
			worldZ := originZ + int64(z)

			// One height per column, not per voxel: the height is the expensive part
			// and it does not depend on y.
			height := HeightAt(seed, worldX, worldZ)
			heights[x][z] = height

			for y := range ChunkSize {
				worldY := originY + int64(y)
				block := blockAt(int(worldY), height)
				if block == Stone {
					block = oreAt(seed, worldX, worldY, worldZ, height)
				}
				chunk.Set(x, y, z, block)
			}
		}
	}

	placeTrees(seed, chunk, &heights)

	return chunk
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

// blockAt decides one voxel from its world height and its column's surface.
func blockAt(worldY, surface int) Block {
	switch {
	case worldY > surface:
		return Air
	case worldY == surface:
		if surface >= snowLine {
			return Snow
		}
		return Grass
	case worldY > surface-dirtDepth:
		return Dirt
	default:
		return Stone
	}
}

// treeAt reports the conifer rooted at one world column. Nothing about the
// destination chunk participates: a neighbouring chunk reaches the same answer
// by recomputing this candidate from (seed, column).
func treeAt(seed, worldX, worldZ int64) (surface, trunkHeight int, ok bool) {
	surface = HeightAt(seed, worldX, worldZ)
	trunkHeight, ok = treeAtSurface(seed, worldX, worldZ, surface)
	return surface, trunkHeight, ok
}

func treeAtSurface(seed, worldX, worldZ int64, surface int) (trunkHeight int, ok bool) {
	if blockAt(surface, surface) != Grass {
		return 0, false
	}

	h := hashLattice(seed+treeSeedOffset, worldX, worldZ)
	if h%treeChanceDenominator != 0 {
		return 0, false
	}

	trunkHeight = treeMinTrunkHeight + int((h>>32)%treeHeightVariants)
	return trunkHeight, true
}

// visitTree yields the canopy before the trunk. Leaves only fill air, while a
// trunk may replace a leaf from an overlapping tree, so this ordering makes logs
// continuous without letting foliage overwrite them.
func visitTree(seed, rootX, rootZ int64, visit func(x, y, z int64, block Block)) {
	surface := HeightAt(seed, rootX, rootZ)
	visitTreeAtSurface(seed, rootX, rootZ, surface, visit)
}

func visitTreeAtSurface(seed, rootX, rootZ int64, surface int, visit func(x, y, z int64, block Block)) {
	trunkHeight, ok := treeAtSurface(seed, rootX, rootZ, surface)
	if !ok {
		return
	}

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
func placeTrees(seed int64, chunk *Chunk, heights *[ChunkSize][ChunkSize]int) {
	originX, _, originZ := chunk.Coord.Origin()
	for rootZ := originZ - treeCanopyRadius; rootZ < originZ+ChunkSize+treeCanopyRadius; rootZ++ {
		for rootX := originX - treeCanopyRadius; rootX < originX+ChunkSize+treeCanopyRadius; rootX++ {
			surface := 0
			if rootX >= originX && rootX < originX+ChunkSize && rootZ >= originZ && rootZ < originZ+ChunkSize {
				surface = heights[int(rootX-originX)][int(rootZ-originZ)]
			} else {
				surface = HeightAt(seed, rootX, rootZ)
			}

			visitTreeAtSurface(seed, rootX, rootZ, surface, func(worldX, worldY, worldZ int64, block Block) {
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
	top := HeightAt(seed, worldX, worldZ)
	surface := top

	for rootZ := worldZ - treeCanopyRadius; rootZ <= worldZ+treeCanopyRadius; rootZ++ {
		for rootX := worldX - treeCanopyRadius; rootX <= worldX+treeCanopyRadius; rootX++ {
			visitTree(seed, rootX, rootZ, func(x, y, z int64, _ Block) {
				if x == worldX && z == worldZ && y > int64(top) && blockAt(int(y), surface) == Air {
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
