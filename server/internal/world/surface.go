package world

// The world seen from above: one column, one height and one word for what is on top
// of it.
//
// **This is a second reading of the terrain, never a second definition of it.**
// Everything here goes through [columnAt] and the rules layered on it —
// [column.blockAt] for the material, [column.fillAt] for the sea line, [caveAt] for
// what is hollow, [plantAtColumn] for what grows — so a map pixel and the chunk under
// it cannot disagree. A "map" that reimplemented the climate switch would be a second
// generator wearing the first one's seed, and it would drift on the first retune.
//
// Nothing here reads a chunk, a cache or a delta. That is the whole reason the map is
// cheap: it is arithmetic over (seed, x, z), so drawing a square of the world costs no
// I/O and takes no lock — and it is also the whole of what the map is honest about,
// because an edit lives in the delta store and the map does not see it. A dug-out hill
// still draws as a hill.

// SurfaceKind is what the top of a column looks like from above.
//
// **A vocabulary of its own, deliberately, and not the block palette.** Two of these
// are properties of the column rather than of any one voxel — [SurfaceForest] is a
// canopy and [SurfaceCave] is an opening — and the rest deliberately collapse blocks a
// map has no reason to tell apart. Keeping the two lists separate is what lets a block
// be added without touching the map, which is the argument schemas/world.fbs makes for
// `MapSurface` at greater length.
//
// The values are `MapSurface`'s values, member for member, so the session that puts a
// tile on the wire converts rather than translates. This package must not import the
// generated bindings — internal/world knows nothing about a wire — so the agreement is
// pinned from the other side, in internal/session, where both names are visible.
type SurfaceKind uint8

// The surface vocabulary. Values are wire values by agreement with `MapSurface`:
// append, never renumber.
const (
	// SurfaceUnknown is "nothing may be said about this column". It is what a caller
	// writes for a place the character has not explored, and it is also this
	// function's answer for a surface block no member below names — a new ground
	// material draws as nothing rather than as the wrong thing, and the client draws
	// nothing for both cases by construction.
	SurfaceUnknown SurfaceKind = 0

	SurfaceGrass  SurfaceKind = 1
	SurfaceSnow   SurfaceKind = 2
	SurfaceSand   SurfaceKind = 3
	SurfaceStone  SurfaceKind = 4
	SurfaceGravel SurfaceKind = 5
	SurfaceWater  SurfaceKind = 6
	SurfaceIce    SurfaceKind = 7

	// SurfaceForest is a column rooted by a species whose table row marks a forest.
	SurfaceForest SurfaceKind = 8

	// SurfaceCave is a column a tunnel has opened in the daylight — an entrance, not
	// a ceiling.
	SurfaceCave SurfaceKind = 9

	// SurfaceSettlement is a column standing inside a settlement's flattened ground:
	// a capital or a village, whether the pixel happens to fall on a roof or on the
	// square between two of them.
	SurfaceSettlement SurfaceKind = 10
)

// SurfaceAt is one column of the world as a map sees it: the terrain height, and the
// one word for what stands on top of it.
//
// Pure in (seed, x, z), like everything else in this package, and that is what makes a
// map tile a computation rather than a query: two servers, two sessions and two
// requests for the same column all reach the same answer without consulting anything.
//
// # The order of the questions is the picture
//
// Water first, because a column under the sea line is water whatever its ground is
// made of and whatever has been carved into it — a lake bed's cave mouth is not
// something you can see from above, and its grass is not something anybody walks on.
// [column.fillAt] is asked at the sea line rather than reimplemented, so the tundra
// lid comes back as [SurfaceIce] by the same rule that puts it in a chunk.
//
// Then the opening, then what grows, then the ground. A tree never roots in a hole —
// [plantAtColumn] checks exactly that — so the middle two cannot both be true and their
// order changes no answer; it is written this way because a carved column can be
// rejected before the tree rules are asked at all.
//
// # Cost
//
// One [columnAt], which is one climate, one height field and one gravel field, and
// then at most one [caveAt] for the column's own top voxel plus whatever
// [plantAtColumn] spends rejecting a candidate. [HeightAt] itself is never called: the
// column already holds its answer, and calling it would pay for the height and the
// climate a second time. That matters here more than anywhere else in this package,
// because the one caller evaluates this 4096 times for a single map tile.
func SurfaceAt(seed int64, x, z int64) (height int, kind SurfaceKind) {
	col := columnAt(seed, x, z)

	// Standing water, and the one voxel of ice that is a lid on it. Both are read at
	// the column's own water surface because that is where the top of the fill is; the
	// ground below is still what the height reports, which is what lets a client shade
	// a shallow bay differently from a deep one.
	//
	// **The column's water line rather than the sea's, since #595.** `surface <
	// seaLevel` named every wet column while a river bed was cut under the sea line; a
	// terraced channel runs at any height, so that test would draw a highland river as
	// the gravel of its own bed and leave the map disagreeing with the chunk.
	if col.standingWater {
		if col.fillAt(col.waterSurface) == Ice {
			return col.surface, SurfaceIce
		}
		return col.surface, SurfaceWater
	}

	// A settlement, before the ground it stands on. Inside the radius the surface is
	// the plateau and there is a building somewhere on it, so the one word for the
	// column is that there is a place here — the grass between two huts is not what a
	// map of this square is for.
	//
	// **Two of the three orderings around this branch are defensive rather than
	// load-bearing, which is worth knowing before anyone reorders them.** It cannot
	// collide with the water case above it: `settlementMinPlateau` is `seaLevel + 3`
	// and the fallback capital is lifted to the same floor, so no settlement column is
	// ever under the sea line and the branch above can never take one. Nor with the
	// cave case below it: `column.carvedAt` refuses the top `settlementCaveClearance`
	// blocks inside a settlement, so a settlement column is never drawn as a cave
	// whichever order these two stand in. The tree case below *is* load-bearing in the
	// same sense as the rest — trees are suppressed inside the radius, so it too cannot
	// fire — which leaves this branch's position a statement of what a map is for
	// rather than a mechanism.
	if col.settlement {
		return col.surface, SurfaceSettlement
	}

	if col.carvedAt(seed, x, int64(col.surface), z) {
		return col.surface, SurfaceCave
	}

	if species, _, rooted := plantAtColumn(seed, x, z, col); rooted && species.forest {
		return col.surface, SurfaceForest
	}

	return col.surface, surfaceKindOf(col.blockAt(col.surface))
}

// surfaceKindOf names the ground for the block on top of a column.
//
// Only the five blocks [column.blockAt] can return at depth 0 are listed, and the
// default is deliberate rather than defensive: a block this list does not name is one
// the map has no word for yet, and [SurfaceUnknown] is the contract's answer for that.
// Drawing an unnamed ground as stone would be a guess the client could not tell from a
// measurement.
func surfaceKindOf(block Block) SurfaceKind {
	switch block {
	case Grass:
		return SurfaceGrass
	case Snow:
		return SurfaceSnow
	case Sand:
		return SurfaceSand
	case Stone:
		return SurfaceStone
	case Gravel:
		return SurfaceGravel
	default:
		return SurfaceUnknown
	}
}
