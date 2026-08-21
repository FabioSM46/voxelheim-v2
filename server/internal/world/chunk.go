// Package world owns the voxels: what a chunk is, how one is generated from a
// seed, and how one is encoded for the wire.
//
// Everything here is a pure function of (seed, coordinate). That is not a style
// preference — the GDD's weekly Fimbulvetr storm regenerates every unprotected
// chunk to its *original procedural state*, so this package has to be able to
// reproduce a chunk months later from the seed alone. Nothing in here reads a
// clock, a global random source, or a map in iteration order.
//
// Voxels do change — players dig — but generation does not. An edit is recorded in the
// Deltas layer and composed on top of the generated base, so the base is still the pure
// function it has to be for the storm to be able to undo a week of digging.
package world

import "slices"

// Chunk geometry. Cubic and chunked on all three axes, so wards and world
// regeneration can reason about volumes instead of whole columns.
const (
	// ChunkSize is the edge length of a chunk in blocks.
	ChunkSize = 32

	// ChunkVolume is how many voxels a chunk holds.
	ChunkVolume = ChunkSize * ChunkSize * ChunkSize
)

// Compile-time guard on the wire format: an RLE run length is a uint16, so a
// chunk must fit inside one run. 32³ = 32768 fits; raising ChunkSize past 40
// would not, and schemas/world.fbs documents that as a protocol version bump.
// This constant fails to compile the moment that stops being true.
const _ = uint16(ChunkVolume)

// Block is a voxel type. It is a uint16 on the wire and here, which leaves room
// for the modular building pieces the GDD calls for without a second format.
type Block uint16

// The block palette. Ids are wire values: append, never renumber.
const (
	Air     Block = 0
	Stone   Block = 1
	Dirt    Block = 2
	Grass   Block = 3
	Snow    Block = 4
	Log     Block = 5
	Leaves  Block = 6
	CoalOre Block = 7
	IronOre Block = 8
)

// Placeable reports whether a block id names something a player may put into the
// world.
//
// The placement subset of the palette schemas/world.fbs requires the server to
// enforce. It is a whitelist rather than a range because ore is a known world
// block but not a building block, while an unknown id is neither and must never
// be stored for a later build to reinterpret.
//
// **Air is deliberately not placeable.** Placing air is breaking, and a break is the
// server's own placement of Air — "the client does not get to choose what a broken
// block leaves behind". Allowing it here would give the client a second, unchecked
// route to the same effect.
func Placeable(b Block) bool {
	switch b {
	case Stone, Dirt, Grass, Snow, Log, Leaves:
		return true
	default:
		return false
	}
}

// Coord addresses a chunk in chunk units. Multiply by ChunkSize for the world
// coordinate of the chunk's minimum corner.
type Coord struct {
	X, Y, Z int32
}

// Chunk is one cubic chunk of voxels.
//
// Blocks is always exactly ChunkVolume long and is indexed with Index: x fastest,
// then z, then y. That order is wire contract rather than an implementation
// choice — schemas/world.fbs documents it, and the client's mesher indexes in it.
//
// A chunk is 64 KiB of blocks, so it travels by pointer. Blocks is a slice rather
// than an array for exactly that reason: an array field would let a copy happen
// silently.
//
// **A chunk is immutable once anyone else can see it.** Set belongs to the generator
// and to composition, both of which write a chunk nobody has been handed yet; an edit
// to a chunk that is already resident goes through Clone and replaces the pointer. That
// is what lets collision read voxels on the tick goroutine with no lock and no atomic,
// while edits arrive on session goroutines.
type Chunk struct {
	Coord  Coord
	Blocks []Block
}

// NewChunk returns an all-air chunk at coord.
func NewChunk(coord Coord) *Chunk {
	return &Chunk{Coord: coord, Blocks: make([]Block, ChunkVolume)}
}

// Clone returns a copy of the chunk that shares nothing with it.
//
// The edit path's tool, and the price of the immutability rule above: 64 KiB per edit,
// paid so that every one of the thousands of terrain reads a tick performs is a plain
// slice index. The trade is deliberate — edits happen at human speed, terrain reads
// happen at tick speed times players times voxels.
func (c *Chunk) Clone() *Chunk {
	return &Chunk{Coord: c.Coord, Blocks: slices.Clone(c.Blocks)}
}

// Index maps a local voxel coordinate to its offset in Blocks:
//
//	index = (y * ChunkSize + z) * ChunkSize + x
//
// Callers are expected to pass coordinates in 0..ChunkSize-1; the arithmetic is
// deliberately unguarded because it runs once per voxel in the generator and the
// mesher.
func Index(x, y, z int) int {
	return (y*ChunkSize+z)*ChunkSize + x
}

// At reads a local voxel.
func (c *Chunk) At(x, y, z int) Block {
	return c.Blocks[Index(x, y, z)]
}

// Set writes a local voxel.
func (c *Chunk) Set(x, y, z int, b Block) {
	c.Blocks[Index(x, y, z)] = b
}

// Origin is the world coordinate of the chunk's minimum corner.
func (c Coord) Origin() (x, y, z int64) {
	return int64(c.X) * ChunkSize, int64(c.Y) * ChunkSize, int64(c.Z) * ChunkSize
}

// ContainingChunk returns the chunk coordinate a world position falls in.
//
// Floor division, not truncation: world x = -1 belongs to chunk -1, not chunk 0.
// Truncating toward zero here would make a 63-block-wide chunk straddling the
// origin, which is the kind of bug that only shows up as a seam once someone
// walks west of spawn.
func ContainingChunk(x, y, z float32) Coord {
	return Coord{
		X: int32(floorDiv(int64(floorF(x)), ChunkSize)),
		Y: int32(floorDiv(int64(floorF(y)), ChunkSize)),
		Z: int32(floorDiv(int64(floorF(z)), ChunkSize)),
	}
}

// ChunkOf returns the chunk that contains a world *block* coordinate.
//
// The integer counterpart of ContainingChunk, and the one collision reads
// through: a collision test addresses voxels rather than positions, so routing
// every lookup through a float would pay a conversion per voxel and would stop
// agreeing with itself at coordinates a float32 can no longer hold exactly.
func ChunkOf(x, y, z int64) Coord {
	return Coord{
		X: int32(floorDiv(x, ChunkSize)),
		Y: int32(floorDiv(y, ChunkSize)),
		Z: int32(floorDiv(z, ChunkSize)),
	}
}

// Local maps one axis of a world block coordinate to its offset inside its chunk,
// always in 0..ChunkSize-1 — for negative coordinates too, which is the whole
// reason it exists. Go's % keeps the sign of the dividend, so world x = -1 is
// local 31 of chunk -1 and not local -1 of chunk 0.
func Local(v int64) int {
	local := v % ChunkSize
	if local < 0 {
		local += ChunkSize
	}
	return int(local)
}

func floorF(v float32) float64 {
	f := float64(v)
	i := int64(f)
	if f < 0 && float64(i) != f {
		i--
	}
	return float64(i)
}

func floorDiv(a, b int64) int64 {
	q := a / b
	if (a%b != 0) && ((a < 0) != (b < 0)) {
		q--
	}
	return q
}
