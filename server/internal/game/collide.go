package game

import (
	"math"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// maxSubStep is the longest distance one axis may move between two overlap tests,
// in blocks.
//
// This is what makes the collision non-tunnelling. Resolving a move by testing the
// box at its destination only works while the destination is adjacent to the
// origin: a step longer than a block can start outside a wall, end outside the far
// side of it, and report no overlap at either end. Splitting the move into steps
// well under a block removes that case by construction, and at a walk it is one
// step per axis per tick anyway.
const maxSubStep = 0.25

// collisionSkin is how far short of a face a blocked move stops, in blocks.
//
// A tenth of a millimetre, and its job is arithmetic rather than physical. Resting
// exactly on a boundary is representable, but the position is stored as the float32
// the wire carries, so `boundary - PlayerWidth/2` rounds — and rounding the wrong
// way puts the box a hair *inside* the block it just stopped against, which the
// next tick reads as "already stuck". Stopping just short of the face keeps every
// tick starting from a position that is genuinely outside the world's solids.
const collisionSkin = 1e-4

// worldLimit is the arithmetic edge of the world, in blocks.
//
// Beyond it a float32 cannot address individual blocks (2²⁴ is where the spacing
// between representable values reaches one) and the int64 voxel arithmetic below
// stops being meaningful. Everything out there is solid, so the world ends in a wall
// rather than in undefined behaviour. At WalkSpeed it is about six months away.
const worldLimit = 1 << 24

// Terrain is the read-only world the simulation collides against.
type Terrain interface {
	// Block returns the resident voxel and whether the tick can inspect it without
	// generating terrain. A miss is not Air: callers that need to distinguish
	// "nothing there" from "not loaded yet" use the boolean and ask again next tick.
	Block(x, y, z int64) (world.Block, bool)

	// Solid reports whether the voxel at a world block coordinate stops movement.
	//
	// A voxel the server has not generated yet MUST answer true. The tick loop may
	// not wait for terrain — a tick that waits on a chunk is a tick every connected
	// player misses — so the only two answers available for an absent chunk are
	// "solid" and "air", and "air" drops the player through a world that is merely
	// still loading.
	Solid(x, y, z int64) bool

	// Fluid reports whether the voxel is one a body wades and swims in.
	//
	// **Not the complement of Solid, and a method rather than a block comparison at
	// the caller.** Air is not solid and is not a fluid either, so a swim rule
	// written against `!Solid` would have players treading water in mid air; and a
	// caller that asked [Terrain.Block] instead would have to learn a block id, pay
	// a chunk lookup per voxel where Solid pays a memo hit, and decide for itself
	// what an absent chunk means.
	//
	// A voxel the server has not generated yet MUST answer false, which is the
	// opposite fail-safe direction from Solid and the same reasoning: an absent
	// chunk already stops the body where it stands, and calling it water as well
	// would let a player swim upward through terrain that has not arrived.
	Fluid(x, y, z int64) bool
}

// CacheTerrain reads the chunks the server has already generated.
//
// Peek, never Get: Get generates on a miss, and this is read from the tick
// goroutine. A chunk that is not resident yet reads as solid, which is what turns a
// slow chunk into a player standing still instead of a player falling out of the
// world.
//
// Not safe for concurrent use, deliberately — see the memo below. One instance
// belongs to one tick loop.
type CacheTerrain struct {
	cache *world.Cache

	// The chunk the previous lookup landed in. A player's box spans one or two chunks
	// and a tick asks about a few dozen voxels inside them, so remembering the last
	// one turns those into one or two cache lookups. Worth it because Peek takes the
	// cache's mutex and moves an LRU entry: without this, collision would be the
	// server's busiest lock.
	memoCoord world.Coord
	memoChunk *world.Chunk

	// The cache revision the memo was taken at.
	//
	// **This is what makes collision see edits.** A composed chunk is never written to
	// after it is published — an edit publishes a patched copy instead — so a remembered
	// pointer stays perfectly readable and quietly stops being the world. Without this
	// check a player standing in the chunk they are digging would keep colliding against
	// the terrain as it was when they arrived, for as long as they stayed in it, because
	// the memo only ever refreshed on a change of *coordinate*.
	//
	// An atomic load per voxel rather than a Peek per voxel: edits are rare next to
	// ticks, so the counter almost always matches and the memo almost always stands.
	memoRevision uint64
}

// NewCacheTerrain returns a terrain view over cache.
func NewCacheTerrain(cache *world.Cache) *CacheTerrain {
	return &CacheTerrain{cache: cache}
}

// Solid reports whether a world voxel stops movement.
//
// **The palette answers what a block is; this answers what an absent chunk is.**
// The rule used to be spelled `block != world.Air` here, which was right only while
// nothing in the world was passable — water ended that, so the classification moved
// to [world.Solid] where the ids live and this kept the half that is about
// residency.
func (t *CacheTerrain) Solid(x, y, z int64) bool {
	block, resident := t.cachedBlock(x, y, z)
	return !resident || world.Solid(block)
}

// Fluid reports whether a world voxel is one a body swims in.
//
// A non-resident chunk is not water — see the interface, where the direction of
// that fail-safe is argued. It reads through the same memo Solid does, because a
// swim test is a box scan on the tick like every other one.
func (t *CacheTerrain) Fluid(x, y, z int64) bool {
	block, resident := t.cachedBlock(x, y, z)
	return resident && world.Fluid(block)
}

// Block reads one resident voxel without ever asking the cache to generate its
// chunk. Unlike collision's cached read, this calls Peek every time: an immutable
// pointer remains safe after eviction, but mining must distinguish a resident chunk
// from one the cache has actually evicted so it can hold progress honestly.
func (t *CacheTerrain) Block(x, y, z int64) (world.Block, bool) {
	chunk, err := t.cache.Peek(world.ChunkOf(x, y, z))
	if err != nil {
		return world.Air, false
	}
	return chunk.At(world.Local(x), world.Local(y), world.Local(z)), true
}

// cachedBlock keeps collision's many reads in one chunk away from the cache mutex.
// The revision guard makes remembered terrain observe edits; an evicted immutable
// pointer remains a valid conservative collision answer until the coordinate changes.
func (t *CacheTerrain) cachedBlock(x, y, z int64) (world.Block, bool) {
	coord := world.ChunkOf(x, y, z)
	revision := t.cache.Revision()

	if t.memoChunk == nil || coord != t.memoCoord || revision != t.memoRevision {
		chunk, err := t.cache.Peek(coord)
		if err != nil {
			// Not resident. Solid, so the player waits on top of a world that has not
			// arrived instead of falling out of it.
			//
			// **Deliberately not remembered.** "Not resident yet" is the one answer that
			// changes on its own — the streamer is generating that chunk right now — and a
			// remembered miss would keep the player frozen above ground that had already
			// arrived. A hit *is* remembered, and safely: the chunk it points at is never
			// written to, an edit publishes a replacement that the revision check picks up,
			// and an eviction can only be followed by a chunk regenerated and recomposed to
			// identical bytes.
			return world.Air, false
		}
		t.memoCoord, t.memoChunk, t.memoRevision = coord, chunk, revision
	}

	return t.memoChunk.At(world.Local(x), world.Local(y), world.Local(z)), true
}

// box is an axis-aligned bounding box in world blocks, as [min, max) per axis.
//
// Half-open on purpose: a box whose max face sits exactly on an integer does not
// occupy the voxel that starts there, which is what lets a player rest on a surface
// without being inside the block above their head.
type box struct {
	min [3]float64
	max [3]float64
}

// body is the size of one colliding thing: a square footprint of width and a
// height, both in blocks.
//
// A parameter rather than a pair of constants read inside the collision, because a
// player is no longer the only thing that falls. The arithmetic is unchanged for the
// player — playerBody carries exactly the constants that used to be spelled here, and
// dividing a float64 by two is exact — so the movement tests are what prove this
// refactor moved nothing.
type body struct {
	width  float64
	height float64
}

// playerBody is the box a player occupies, and the only body until drops arrived.
var playerBody = body{width: PlayerWidth, height: PlayerHeight}

// boxAt is the box a body standing at pos occupies.
//
// pos is the centre of the footprint at the *feet*, which is why y is the box's
// minimum rather than its middle. "Where the thing is standing" is the number every
// other consumer wants: world.SpawnAt produces one, world.ContainingChunk takes one,
// and the client puts its camera an eye height above it. Every entity in the
// simulation uses this convention, so a drop's position means what a player's does.
func (bd body) boxAt(pos [3]float64) box {
	half := bd.width / 2
	return box{
		min: [3]float64{pos[0] - half, pos[1], pos[2] - half},
		max: [3]float64{pos[0] + half, pos[1] + bd.height, pos[2] + half},
	}
}

// positionOf recovers the standing position of a box built by boxAt.
func (bd body) positionOf(b box) [3]float64 {
	half := bd.width / 2
	return [3]float64{b.min[0] + half, b.min[1], b.min[2] + half}
}

// playerBox is the box a player standing at pos occupies.
func playerBox(pos [3]float64) box { return playerBody.boxAt(pos) }

// translate moves the box along one axis.
func (b box) translate(axis int, delta float64) box {
	b.min[axis] += delta
	b.max[axis] += delta
	return b
}

// boxDistance is the shortest distance between two boxes, in blocks, and zero when
// they touch or overlap.
//
// Euclidean, not per axis, for the reason EditReach gives: a per-axis test with the
// same number would let a corner diagonal reach that number times √3, which is a
// different rule wearing this one's value. The per-axis gap is clamped at zero before
// it is squared so an overlap on one axis cannot shorten the distance on another.
func boxDistance(a, b box) float64 {
	var sum float64
	for axis := range 3 {
		gap := max(a.min[axis]-b.max[axis], b.min[axis]-a.max[axis], 0)
		sum += gap * gap
	}
	return math.Sqrt(sum)
}

// moveAndCollide moves a body of size bd from pos by delta and reports where it ends
// up and which axes were stopped.
//
// One axis at a time, and each axis in sub-steps of at most maxSubStep. Both halves
// are what make this correct without a physics engine: moving all three axes at once
// needs a swept test to work out which face was hit first, and a long step can pass
// straight through a wall between two overlap tests. Resolving per axis instead
// means a player who walks diagonally into a corner slides along it, which is what
// the standard approach buys.
//
// The y axis goes first so that landing happens before the horizontal move: a player
// who is falling and walking forward touches the ground on the tick they arrive at
// it, rather than sliding one tick further through the air.
func moveAndCollide(t Terrain, bd body, pos, delta [3]float64) (out [3]float64, blocked [3]bool) {
	b := bd.boxAt(pos)

	// Already inside something, before moving at all. Two ways to get here and both
	// want the same answer: the terrain under it has not been generated yet (an absent
	// chunk reads as solid), or it was placed inside a solid. Refusing the move leaves
	// it where it is — snapping out would teleport it, and letting it through would
	// drop it out of a world that is merely still loading. Every axis reports blocked
	// so the caller also zeroes the velocity: a player who waits three seconds for a
	// chunk must not arrive with three seconds of fall speed, and neither must a drop
	// hanging over one.
	if overlaps(t, b) {
		return pos, [3]bool{true, true, true}
	}

	for _, axis := range [3]int{1, 0, 2} {
		b, blocked[axis] = slideAxis(t, b, axis, delta[axis])
	}
	return bd.positionOf(b), blocked
}

// slideAxis moves the box along one axis, stopping at the first solid face.
//
// Precondition: b overlaps nothing. moveAndCollide establishes it, and each call
// here preserves it — a sub-step that would overlap is replaced by a stop short of
// the face it hit. That precondition is what makes the snapping below exact: the
// only voxels a sub-step can newly touch are the ones in the layer its leading face
// crossed into, because the other two axes did not move and one sub-step is shorter
// than a block.
func slideAxis(t Terrain, b box, axis int, delta float64) (box, bool) {
	if delta == 0 {
		return b, false
	}

	steps := int(math.Ceil(math.Abs(delta) / maxSubStep))
	step := delta / float64(steps)

	for range steps {
		moved := b.translate(axis, step)
		// The leading face is tested a skin *past* where it would land, not at it.
		//
		// **A box that comes to rest exactly flush against a solid face is a box that
		// can never move again**, and the arithmetic to reach that state is ordinary: a
		// body walking 0.16 blocks a tick from x=0.5 arrives at 4.000000000000001
		// against a face at 4, one ulp over. moveAndCollide reads any overlap at all as
		// "already inside something" and blocks every axis, so the body is frozen — it
		// cannot even rise out. Half-open boxes make flush *legal*, which is what lets a
		// sub-step land there with no collision detected and therefore no skin applied.
		//
		// Testing a skin ahead means a landing that would be flush is treated as the hit
		// it is about to become, and the snap below leaves the same gap every detected
		// collision leaves. Found by the first thing that had to climb out of the state
		// rather than merely stand in it.
		probe := moved
		if step > 0 {
			probe.max[axis] += collisionSkin
		} else {
			probe.min[axis] -= collisionSkin
		}
		if !overlaps(t, probe) {
			b = moved
			continue
		}

		// The layer's near boundary is where the box stops.
		if step > 0 {
			boundary := math.Floor(moved.max[axis])
			b = b.translate(axis, boundary-collisionSkin-b.max[axis])
		} else {
			boundary := math.Floor(moved.min[axis]) + 1
			b = b.translate(axis, boundary+collisionSkin-b.min[axis])
		}
		return b, true
	}

	return b, false
}

// overlaps reports whether any solid voxel intersects the box.
func overlaps(t Terrain, b box) bool {
	if b.beyondTheWorld() {
		return true
	}
	return anyVoxel(b, func(x, y, z int64) bool { return t.Solid(x, y, z) })
}

// overlapsFluid reports whether any voxel a body swims in intersects the box.
//
// **Beyond the world is not water**, which is the opposite answer overlaps gives to
// the same question and the same fail-safe reasoning: out there everything is solid,
// so a body is already stopped, and reporting fluid as well would let it swim
// upwards through the wall the world ends in.
func overlapsFluid(t Terrain, b box) bool {
	if b.beyondTheWorld() {
		return false
	}
	return anyVoxel(b, func(x, y, z int64) bool { return t.Fluid(x, y, z) })
}

// anyVoxel reports whether any voxel the box touches satisfies want.
//
// y outermost, then z, then x, matching world.Index: the innermost loop walks
// consecutive blocks of a chunk.
func anyVoxel(b box, want func(x, y, z int64) bool) bool {
	x0, x1 := voxelSpan(b.min[0], b.max[0])
	y0, y1 := voxelSpan(b.min[1], b.max[1])
	z0, z1 := voxelSpan(b.min[2], b.max[2])

	for y := y0; y <= y1; y++ {
		for z := z0; z <= z1; z++ {
			for x := x0; x <= x1; x++ {
				if want(x, y, z) {
					return true
				}
			}
		}
	}
	return false
}

// beyondTheWorld reports whether any face has left the range the voxel arithmetic
// can address.
func (b box) beyondTheWorld() bool {
	for axis := range 3 {
		if b.min[axis] < -worldLimit || b.max[axis] > worldLimit {
			return true
		}
	}
	return false
}

// voxelSpan returns the first and last voxel index a half-open [lo, hi) extent
// touches. The last index is exclusive-corrected, so an extent ending exactly on a
// boundary does not claim the voxel that starts there.
func voxelSpan(lo, hi float64) (int64, int64) {
	return int64(math.Floor(lo)), int64(math.Ceil(hi)) - 1
}
