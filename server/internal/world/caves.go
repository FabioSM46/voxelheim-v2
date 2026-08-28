package world

// Caves: the tunnels that wind under the hills, and the mouths that let a walk
// into one begin outdoors.
//
// **Two decorrelated 3D fields intersected, not one field thresholded.** A single
// fbm3D above (or below) a cut-off carves blobs — the field's level sets are
// closed shells, so what you get is a cellar rather than a passage. Two
// independent fields each held near their *midpoint* carve the intersection of two
// surfaces, and the intersection of two surfaces in a volume is a curve: a tunnel
// with a length, which is the thing a miner can follow. Everything below is one
// consequence of that choice or one bound on where it applies.
//
// Like the rest of generation this is a pure integer function of (seed, x, y, z) in
// Q16.16 — see noise.go for why there is no float on this path.

const (
	// caveScaleBlocks is how many blocks span one lattice cell of the two cave
	// fields.
	//
	// Much finer than terrainScaleBlocks (96), because a tunnel is a feature of one
	// hillside rather than of a region: a cell just under a chunk wide turns a
	// passage about once per chunk, and fbm3D's later octaves give it the wobble
	// between the turns. It is also deliberately not oreScaleBlocks (12) — a cave
	// that bent at exactly the rate a vein does would read as the ore having been
	// hollowed out rather than as a passage that happens to cut one.
	caveScaleBlocks = 28

	// caveHalfWidth is how far either side of the midpoint a field may sit and still
	// count as carved.
	//
	// The condition is |n − ½| < caveHalfWidth on **both** fields, so this number is
	// a tunnel's radius rather than a fraction of the world: widen it and the two
	// curves thicken into caverns and then into a sponge; narrow it and the passages
	// pinch shut into disconnected pockets.
	//
	// **Five percent, and the issue that asked for caves said seven.** The number
	// had to be measured rather than reasoned, because fbm3D sums four octaves and
	// the sum is concentrated hard around its midpoint: a half-width of 7% of the
	// range does not carve (2 × 7%)² of the volume, it carves about 42% of it per
	// field and 18% of it after the intersection. Measured over four 128×128 areas
	// at seed 0x5EED, from each column's surface down 64 blocks:
	//
	//	4/100 → 5.5% … 7.0% carved
	//	5/100 → 8.4% … 10.6%
	//	6/100 → 11.9% … 14.9%
	//	7/100 → 16.0% … 19.7%
	//
	// The same issue asks for a carved fraction between 4% and 12%, which is the
	// half of that pair describing the world somebody walks through rather than the
	// knob that produces it, so the knob moved. 5/100 sits in the middle of the band
	// with room on both sides for the area a measurement happens to land in;
	// TestCarvedFractionIsATunnelNetworkNotASponge is that measurement, kept.
	caveHalfWidth = one * 5 / 100

	// caveMinDepth is the first depth carving reaches without a mouth, and
	// caveMaxDepth is the last depth it reaches at all.
	//
	// The floor exists so a tunnel does not casually erase the ground somebody is
	// standing on: within the top two blocks of a column the carve has to be at a
	// mouth. The ceiling is what keeps caveAt off most of the voxels in the world —
	// below it there is nothing but stone nobody has a reason to walk to, and every
	// voxel outside the band costs a subtraction and a comparison instead of two fbm
	// sums. Ninety-six blocks reaches well past ironMaxDepth (56), so both ore bands
	// lie wholly inside the carved band and no vein is out of a tunnel's reach.
	caveMinDepth = 2
	caveMaxDepth = 96

	// caveMouthScaleBlocks is the lattice cell of the 2D field that decides which
	// columns a tunnel may break the surface in, and caveMouthThreshold is how high
	// that field has to be.
	//
	// Ninety-six blocks — terrainScaleBlocks — because a mouth belongs to a hillside,
	// and the threshold covers about five percent of columns: enough that a walk
	// crosses one, few enough that the ground is not a colander. Measured over a
	// 512×512 sample at seed 0x5EED, 69/100 covers 5.6% of columns and 75/100 covers
	// 2.1% — the same concentration around the midpoint that set caveHalfWidth, and
	// the same reason the number is measured rather than derived.
	// TestCaveMouthsAreRareEnoughToBeWorthFinding is that measurement.
	//
	// A mouth column is not the same as an open one: the mouth field only *permits*
	// the top two voxels to be carved, and the two cave fields still have to agree
	// there, so the columns actually open to daylight are a fraction of these.
	caveMouthScaleBlocks = 96
	caveMouthThreshold   = one * 69 / 100

	// spawnCaveClearance is how far from the spawn column, in blocks on each
	// horizontal axis, nothing is carved.
	//
	// **SpawnAt derives the player's feet from the generated column, so a tunnel
	// under spawn is not a cosmetic problem** — it would drop the first thing a
	// session does into a hole, or open the floor under it. Eight blocks on each axis
	// is a small square of guaranteed ground, and it is a Chebyshev radius rather
	// than a Euclidean one because two axis comparisons are the cheapest correct
	// answer to "is this column near spawn" and this check sits in front of every
	// carved voxel in the world.
	spawnCaveClearance = 8
)

// The two cave fields and the mouth field each get their own offset from the world
// seed, in the style of every other field here. Two fields sampled from one seed at
// one scale are the same field, and their intersection would be the field itself:
// the tunnels would collapse back into the blobs this file exists to avoid.
const (
	caveSeedOffsetA     int64 = 0x082EFA98
	caveSeedOffsetB     int64 = 0xEC4E6C89
	caveMouthSeedOffset int64 = 0x452821E6
)

// The mouth rule only means anything while the two depths bound a band rather than
// crossing: reorder them and this conversion is a compile error instead of a
// silently empty cave system.
const _ = uint8(caveMaxDepth - caveMinDepth)

// caveAt reports whether the voxel at (worldX, worldY, worldZ) is hollowed out, for
// a column whose terrain surface is at surface.
//
// **The cheap rejections come first, and their order is their cost.** A depth
// comparison excludes every voxel above the ground and everything under ninety-six
// blocks of rock; the spawn square is two more comparisons; the mouth field is one
// fbm2D and is only ever asked about the top two voxels of a column. Only what
// survives all three pays for the two fbm3D sums this function is actually about.
//
// Reads nothing outside its own coordinate, which is what keeps chunk generation
// local: a neighbouring chunk carves a shared voxel identically by calling this,
// not by consulting anything.
func caveAt(seed, worldX, worldY, worldZ int64, surface int) bool {
	depth := int64(surface) - worldY
	if depth < 0 || depth > caveMaxDepth {
		return false
	}
	if absInt64(worldX-spawnColumnX) <= spawnCaveClearance && absInt64(worldZ-spawnColumnZ) <= spawnCaveClearance {
		return false
	}
	if depth < caveMinDepth && !caveMouthAt(seed, worldX, worldZ) {
		return false
	}

	nx := floorDiv(worldX<<fracBits, caveScaleBlocks)
	ny := floorDiv(worldY<<fracBits, caveScaleBlocks)
	nz := floorDiv(worldZ<<fracBits, caveScaleBlocks)

	// && short-circuits, and that is the performance budget rather than an accident
	// of style: field A rejects most voxels in the band on its own, so the second
	// fbm3D is paid on a minority of them.
	return nearMidpoint(fbm3D(seed+caveSeedOffsetA, nx, ny, nz)) &&
		nearMidpoint(fbm3D(seed+caveSeedOffsetB, nx, ny, nz))
}

// nearMidpoint is the carve condition for one field: |n − ½| < caveHalfWidth.
func nearMidpoint(n int64) bool {
	return absInt64(n-one/2) < caveHalfWidth
}

// caveMouthAt reports whether a column is one a tunnel may open in the daylight.
//
// A 2D field, so a mouth is a property of the column rather than of a voxel: the
// alternative is a tunnel that surfaces for one block and dives again, which reads
// as a hole in the ground rather than as a way in.
func caveMouthAt(seed, worldX, worldZ int64) bool {
	return climateField(seed+caveMouthSeedOffset, worldX, worldZ, caveMouthScaleBlocks) >= caveMouthThreshold
}

func absInt64(v int64) int64 {
	if v < 0 {
		return -v
	}
	return v
}
