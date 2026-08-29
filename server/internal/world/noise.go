package world

// Value noise and fBm, in fixed point.
//
// **The arithmetic here is integer-only on purpose.** Go's specification permits
// an implementation to fuse floating-point operations — "possibly across
// statements" — so the same float expression may round differently on another
// architecture, another Go release, or with different optimisation. For most code
// that is irrelevant. Here it is not: the GDD's weekly storm has to regenerate a
// chunk to the bytes it had months ago, so "deterministic on this build, on this
// machine" would leave the world quietly drifting after a compiler upgrade.
//
// Q16.16 fixed point costs a few shifts and buys bit-exact terrain on every
// platform, forever. There is no float64 anywhere in the generation path.

const (
	// fracBits is the fixed-point fraction width.
	fracBits = 16

	// one is 1.0 in Q16.16.
	one = 1 << fracBits

	// fracMask extracts the fractional part.
	fracMask = one - 1
)

// hashLattice is a deterministic 64-bit mix of the seed and a lattice point.
//
// Multiply-xor-shift in the style of splitmix64's finalizer: cheap, and it
// decorrelates neighbouring lattice points well enough that the interpolated
// field has no visible axis alignment. Integer overflow is defined in Go, so this
// is bit-exact everywhere.
func hashLattice(seed, x, y int64) uint64 {
	h := uint64(seed)
	h ^= uint64(x) * 0x9E3779B97F4A7C15
	h ^= uint64(y) * 0xC2B2AE3D27D4EB4F
	h ^= h >> 30
	h *= 0xBF58476D1CE4E5B9
	h ^= h >> 27
	h *= 0x94D049BB133111EB
	h ^= h >> 31
	return h
}

// hashLattice3D is hashLattice's three-dimensional counterpart. The z axis has
// its own odd multiplier, so moving through a volume does not revisit either of
// the two-dimensional field's coordinate mixtures.
func hashLattice3D(seed, x, y, z int64) uint64 {
	h := uint64(seed)
	h ^= uint64(x) * 0x9E3779B97F4A7C15
	h ^= uint64(y) * 0xC2B2AE3D27D4EB4F
	h ^= uint64(z) * 0x165667B19E3779F9
	h ^= h >> 30
	h *= 0xBF58476D1CE4E5B9
	h ^= h >> 27
	h *= 0x94D049BB133111EB
	h ^= h >> 31
	return h
}

// latticeValue is the noise value at an integer lattice point, in [0, one].
func latticeValue(seed, x, y int64) int64 {
	return int64(hashLattice(seed, x, y) & fracMask)
}

// latticeValue3D is the noise value at a 3D integer lattice point, in [0, one].
func latticeValue3D(seed, x, y, z int64) int64 {
	return int64(hashLattice3D(seed, x, y, z) & fracMask)
}

// smoothstep is 3t² − 2t³ in Q16.16, mapping [0, one] to [0, one].
//
// Plain linear interpolation between lattice points leaves visible creases along
// the lattice grid, because the field's first derivative jumps at every cell
// boundary. Smoothstep flattens the derivative at the ends of each cell, which is
// what makes value noise look like terrain instead of like graph paper.
func smoothstep(t int64) int64 {
	// t ≤ 2^16, so t*t ≤ 2^32 and the product below ≤ 2^50: no overflow in int64.
	return (t * t * (3*one - 2*t)) >> (2 * fracBits)
}

// lerp interpolates a→b by t in Q16.16.
func lerp(a, b, t int64) int64 {
	return a + ((b-a)*t)>>fracBits
}

// valueNoise2D samples the interpolated noise field at a Q16.16 position,
// returning [0, one].
func valueNoise2D(seed, x, y int64) int64 {
	x0, y0 := x>>fracBits, y>>fracBits
	tx, ty := smoothstep(x&fracMask), smoothstep(y&fracMask)

	v00 := latticeValue(seed, x0, y0)
	v10 := latticeValue(seed, x0+1, y0)
	v01 := latticeValue(seed, x0, y0+1)
	v11 := latticeValue(seed, x0+1, y0+1)

	return lerp(lerp(v00, v10, tx), lerp(v01, v11, tx), ty)
}

// valueNoise3D samples the interpolated noise field at a Q16.16 position,
// returning [0, one].
func valueNoise3D(seed, x, y, z int64) int64 {
	x0, y0, z0 := x>>fracBits, y>>fracBits, z>>fracBits
	tx, ty, tz := smoothstep(x&fracMask), smoothstep(y&fracMask), smoothstep(z&fracMask)

	v000 := latticeValue3D(seed, x0, y0, z0)
	v100 := latticeValue3D(seed, x0+1, y0, z0)
	v010 := latticeValue3D(seed, x0, y0+1, z0)
	v110 := latticeValue3D(seed, x0+1, y0+1, z0)
	v001 := latticeValue3D(seed, x0, y0, z0+1)
	v101 := latticeValue3D(seed, x0+1, y0, z0+1)
	v011 := latticeValue3D(seed, x0, y0+1, z0+1)
	v111 := latticeValue3D(seed, x0+1, y0+1, z0+1)

	bottom := lerp(lerp(v000, v100, tx), lerp(v010, v110, tx), ty)
	top := lerp(lerp(v001, v101, tx), lerp(v011, v111, tx), ty)
	return lerp(bottom, top, tz)
}

// fbmOctaves is how many octaves of noise the terrain sums.
//
// Four is the point of diminishing returns at 32-block chunks: the fourth octave
// already varies within a couple of blocks, and a fifth costs a hash per column
// for detail the voxel grid cannot represent.
const fbmOctaves = 4

// fbm2D sums octaves of value noise, doubling frequency and halving amplitude
// each time, and normalises the result to [0, one].
//
// Each octave gets its own decorrelated seed rather than a shifted position: two
// octaves sampled from the same field at 1× and 2× share lattice points, and the
// shared points show up as a faint grid in the sum.
func fbm2D(seed, x, y int64) int64 {
	var sum, norm int64
	amplitude := int64(one)
	fx, fy := x, y

	for octave := range fbmOctaves {
		sum += (valueNoise2D(seed+int64(octave)*0x51ED2701, fx, fy) * amplitude) >> fracBits
		norm += amplitude
		amplitude >>= 1
		fx *= 2
		fy *= 2
	}

	// sum ≤ norm ≤ 2·one, so sum*one ≤ 2^33: no overflow.
	return (sum * one) / norm
}

// fbm3D is fbm2D extended through a volume. Its octave seed stride is distinct
// from fbm2D's: ore noise must not inherit the terrain field's correlations just
// because both start from the same world seed.
func fbm3D(seed, x, y, z int64) int64 {
	var sum, norm int64
	amplitude := int64(one)
	fx, fy, fz := x, y, z

	for octave := range fbmOctaves {
		octaveSeed := seed + int64(octave)*0x6A09E667
		sum += (valueNoise3D(octaveSeed, fx, fy, fz) * amplitude) >> fracBits
		norm += amplitude
		amplitude >>= 1
		fx *= 2
		fy *= 2
		fz *= 2
	}

	// sum <= norm <= 2*one, so sum*one <= 2^33: no overflow.
	return (sum * one) / norm
}

// HashLattice is [hashLattice] under an exported name, for the one caller outside this
// package that derives something from a lattice point instead of reading a field.
//
// **internal/game names the world-owned stations with it**, and the alternative was a
// second mixing function: a village forge is written down nowhere, so its id has to be a
// hash of the seed and the column it stands on, exactly as this package's own decisions
// are. Two copies of splitmix64's finalizer would agree until somebody tuned one, and the
// disagreement would be a forge that changed its id at a deploy. It still says nothing
// about entities — it is arithmetic over three integers, and what the answer names
// belongs to the caller.
func HashLattice(seed, x, y int64) uint64 { return hashLattice(seed, x, y) }
