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

// BlockLimit is the arithmetic edge of the world, in blocks: every coordinate this
// server will address satisfies |x| <= BlockLimit on each axis.
//
// Beyond it a float32 cannot address individual blocks — 2^24 is where the spacing
// between representable values reaches one — and the int64 voxel arithmetic the
// simulation collides with stops being meaningful. Everything out there is solid, so
// the world ends in a wall rather than in undefined behaviour. At walking speed it is
// about six months away.
//
// **It lives here rather than in internal/game because the world is what has an edge,
// not the simulation.** The number was game's alone for as long as the only things that
// named a coordinate were a body being moved and a block being placed, and both of those
// start from terrain this server generated: nothing a client sent could be outside the
// world, because nothing a client sent was a coordinate at all. The map ends that —
// `MarkerPlaceRequest` is the first message in which a client chooses an x and a z
// outright — and internal/session cannot read game.worldLimit and must not grow a second
// copy of it. internal/world is the leaf both of them already import, and the edge of the
// world is a fact about the world.
//
// Blocks, and no y: the two axes are the ones a place on a map has. game.worldLimit
// applies the same number to the vertical too, which is its business and not this
// constant's — see collide.beyondTheWorld.
const BlockLimit = 1 << 24

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

	// What a desert and a gravel bar are made of. Appended, never inserted: every
	// id above is already on the wire inside chunks a client has, and inside the
	// delta files a played-in world directory holds. All three are building blocks
	// rather than ore, so Placeable says yes to each of them.
	Sand      Block = 9
	Sandstone Block = 10
	Gravel    Block = 11

	// The two blocks water is made of. Appended for the reason every id above was,
	// and the first pair in this palette that are not interchangeable in the way the
	// rest are: [Water] is the one id in the world a body moves *through*, and [Ice]
	// is ordinary ground that happens to be the lid on some of it.
	//
	// **Water is not placeable and has no item**, which is the whole of "water is
	// static here": it appears because the generator says so, it is displaced by
	// anything put into its voxel, and nothing a player does ever creates one.
	Water Block = 12
	Ice   Block = 13
)

// Solid reports whether a block stops movement.
//
// **The palette owns this question, not the collision.** `Terrain.Solid` used to
// spell it as `block != Air`, which was correct only while nothing in the world was
// passable — and every rule that read it that way (the spawn director's headroom
// scan, the collision sweep) inherited the assumption without stating it. Water is
// the first id that ends the equivalence, so the answer moved to where the ids are.
func Solid(b Block) bool {
	return b != Air && b != Water
}

// Fluid reports whether a block is one a body wades and swims in rather than walks
// through or stands on.
//
// Deliberately not `!Solid(b)`: air is not solid and is not a fluid either, and a
// swim rule written against "not solid" would have players treading water in mid
// air. [Ice] is absent for the opposite reason — it is the lid, and you walk on it.
func Fluid(b Block) bool {
	return b == Water
}

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
//
// **Water is not placeable either, and for a different reason.** Air is refused
// because placing it is *breaking*; water is refused because there is no such thing
// as a piece of water to hold. It has no item, it drops nothing, and the only thing
// that ever writes one is the generator — which is what "water is static" means in
// this codebase. Ice is placeable because it is ordinary ground: it is mined, it
// drops itself, and it can be put back.
func Placeable(b Block) bool {
	switch b {
	case Stone, Dirt, Grass, Snow, Log, Leaves, Sand, Sandstone, Gravel, Ice:
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

// Column addresses a chunk column on the horizontal plane: every chunk that shares
// one (x, z) pair, at every height.
//
// **There is no cy, and that is the whole of the type.** What a column names is a
// place on the map rather than a cube of voxels, and the two questions this server
// asks about a place — has this character been here, and what does the ground look
// like from above — are both answered once per column and never once per height. A
// [Coord] that carried a Y nobody read would be the same value with a field every
// caller had to remember to zero.
//
// The field names are CX and CZ rather than X and Z, because a column is addressed in
// chunk units and a two-field {X, Z} beside a three-field {X, Y, Z} is exactly the
// pair a reader mistakes for block coordinates. schemas/world.fbs names the wire
// struct's fields the same way, for the same reason.
type Column struct {
	CX, CZ int32
}

// Column is the column a chunk stands in: its horizontal address, with the height
// dropped.
func (c Coord) Column() Column {
	return Column{CX: c.X, CZ: c.Z}
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
