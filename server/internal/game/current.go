package game

import (
	"math"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// flowNeighbours are the four horizontal axis steps a flowing voxel is compared
// against, as (dx, dz). Horizontal only: the vertical term is not a difference of
// levels but the presence of water overhead, which [FlowDirection] reads separately.
var flowNeighbours = [4][2]int64{{1, 0}, {-1, 0}, {0, 1}, {0, -1}}

// FlowDirection is which way the water at a voxel is going, as a unit horizontal
// direction plus a falling flag in y.
//
// **One derivation, read by the swim rules and mirrored verbatim by the client's
// surface animation.** The two have to agree or a river will visibly run one way and
// carry a body the other; keeping the rule stated once, here, is what makes that
// agreement structural rather than a coincidence somebody has to maintain. The client
// mirrors it for *animation* only — the drift itself is resolved on this side of the
// wire, because a current that moves a body is a gameplay outcome.
//
// Three answers, one per class of water id:
//
//   - A [world.WaterCurrentXPos] and its three siblings carry their direction in the
//     id itself. Worldgen places them along a river channel, so the answer is the
//     axis vector and nothing is derived. They never gain a vertical component: a
//     river is full-depth source water all the way down, and every voxel of it has
//     water above.
//   - A flowing level — [world.WaterFlow1] through [world.WaterFlow7], the shapes the
//     runtime automaton leaves behind — is compared against its four horizontal
//     neighbours. Each contributes (level - neighbourLevel) along its axis, so the
//     sum points away from the deep water and toward the shallow: exactly the way the
//     automaton will move the water next. Air counts as level 0, because air is where
//     water is about to go. A neighbour that is [world.Solid] is skipped rather than
//     counted as zero — a wall is not somewhere to flow, and counting it as empty
//     would push the swimmer into it — and [world.Ice] is skipped by that same test,
//     being the lid rather than the water. A full source neighbour is skipped too: it
//     is level 8, so counting it would produce a term pointing *into* the source,
//     which is a body of standing water rather than a direction.
//   - Plain [world.Water] is a source and has no direction at all.
//
// The vertical is a flag rather than a magnitude, and it belongs to exactly one case:
// a non-source flowing voxel with water directly above it is the shape of a fall. A
// plain source or a current voxel with water above is just deep water, and giving
// those a downward pull would drag a swimmer to the bottom of every lake. The caller
// reads y as "is this a fall", never as a speed — see Player.step, where the target
// it selects is [WaterfallSinkSpeed].
//
// Non-water voxels answer zero, and so does a voxel whose chunk is not resident: the
// tick may not wait for terrain, and inventing a shove out of a chunk that has not
// arrived is the one answer that could move a player through a world they cannot see.
// A non-resident *neighbour* is skipped for the same reason.
func FlowDirection(terrain Terrain, x, y, z int64) (dx, dy, dz float64) {
	block, resident := terrain.Block(x, y, z)
	if !resident || !world.IsWater(block) {
		return 0, 0, 0
	}

	if cx, cz := world.CurrentOf(block); cx != 0 || cz != 0 {
		return float64(cx), 0, float64(cz)
	}

	level := world.WaterLevel(block)
	if level == waterSourceLevel {
		// A plain source: standing water, no direction, and no fall however deep it is.
		return 0, 0, 0
	}

	var sumX, sumZ float64
	for _, step := range flowNeighbours {
		neighbour, ok := terrain.Block(x+step[0], y, z+step[1])
		if !ok || world.Solid(neighbour) {
			continue
		}
		neighbourLevel := world.WaterLevel(neighbour)
		if neighbourLevel == waterSourceLevel {
			continue
		}
		drop := float64(level - neighbourLevel)
		sumX += drop * float64(step[0])
		sumZ += drop * float64(step[1])
	}
	if magnitude := math.Hypot(sumX, sumZ); magnitude > 0 {
		dx, dz = sumX/magnitude, sumZ/magnitude
	}

	if above, ok := terrain.Block(x, y+1, z); ok && world.IsWater(above) {
		dy = -1
	}
	return dx, dy, dz
}

// waterSourceLevel is the level [world.WaterLevel] reports for a full voxel: the
// plain source and all four generator-authored currents. Named rather than spelled 8
// twice above, because what the two tests mean is "this is a source", not "this is
// eight eighths".
const waterSourceLevel = 8

// playerCentreVoxel is the voxel a player standing at pos has the centre of their
// box in.
//
// pos is the feet — see body.boxAt — so the centre is PlayerHeight/2 above it, the
// same point EditReach measures from and for the same reason: it is a number the
// server already states, rather than an eye height the client owns.
//
// math.Floor and not a truncation: at negative coordinates a cast toward zero maps
// -0.4 to voxel 0, which is a whole block east of where the body is.
func playerCentreVoxel(pos [3]float64) (x, y, z int64) {
	return int64(math.Floor(pos[0])),
		int64(math.Floor(pos[1] + PlayerHeight/2)),
		int64(math.Floor(pos[2]))
}
