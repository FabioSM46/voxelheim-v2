package game

import (
	"math"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// scriptedTerrain is a world stated as a predicate: solid exactly where want says so,
// air everywhere else, and every chunk resident.
//
// Deliberately not [dropTerrain] with a wall bolted on. The traversal below has to be
// exercised over shapes a ground plane would only get in the way of — a single voxel in
// open air, a wall with a gap in it — and the whole of what those need is a function
// from a coordinate to solid or not.
type scriptedTerrain struct{ want func(x, y, z int64) bool }

func (w scriptedTerrain) Block(x, y, z int64) (world.Block, bool) {
	if w.want != nil && w.want(x, y, z) {
		return world.Stone, true
	}
	return world.Air, true
}

func (w scriptedTerrain) Solid(x, y, z int64) bool {
	block, resident := w.Block(x, y, z)
	return !resident || world.Solid(block)
}

func (w scriptedTerrain) Fluid(x, y, z int64) bool { return fluidByBlock(w, x, y, z) }

type blockTerrain struct{ blocks map[[3]int64]world.Block }

func (w blockTerrain) Block(x, y, z int64) (world.Block, bool) {
	if y <= 0 {
		return world.Stone, true
	}
	return w.blocks[[3]int64{x, y, z}], true
}

func (w blockTerrain) Solid(x, y, z int64) bool {
	block, _ := w.Block(x, y, z)
	return world.Solid(block)
}

func (w blockTerrain) Fluid(x, y, z int64) bool { return fluidByBlock(w, x, y, z) }

func TestPlayerWalksUpBothRisersOfAnAuthoritativeStairWithoutJumping(t *testing.T) {
	t.Parallel()

	terrain := blockTerrain{blocks: map[[3]int64]world.Block{
		{1, 1, 0}: world.SlateStairEastBottom,
	}}
	pos := [3]float64{0.5, 1, 0.5}

	first, blocked := moveAndCollideWithStep(terrain, playerBody, pos, [3]float64{0.6, 0, 0}, playerStepHeight)
	if blocked[0] {
		t.Fatal("the lower stair riser blocked ordinary walking")
	}
	if math.Abs(first[1]-1.5) > collisionSkin {
		t.Fatalf("feet after the lower riser = %.6f, want 1.5", first[1])
	}

	second, blocked := moveAndCollideWithStep(terrain, playerBody, first, [3]float64{0.6, 0, 0}, playerStepHeight)
	if blocked[0] {
		t.Fatal("the upper stair riser blocked ordinary walking")
	}
	if math.Abs(second[1]-2) > collisionSkin {
		t.Fatalf("feet after the upper riser = %.6f, want 2", second[1])
	}
	if overlaps(terrain, playerBox(second)) {
		t.Fatal("the climbed body overlaps the stair")
	}
}

func TestPlayerStepsOntoBottomSlabButNotThroughAFullCube(t *testing.T) {
	t.Parallel()

	start := [3]float64{0.5, 1, 0.5}
	for _, tc := range []struct {
		name    string
		block   world.Block
		wantY   float64
		blocked bool
	}{
		{name: "bottom slab", block: world.SlateSlabBottom, wantY: 1.5},
		{name: "full cube", block: world.SlateTile, wantY: 1, blocked: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			terrain := blockTerrain{blocks: map[[3]int64]world.Block{{1, 1, 0}: tc.block}}
			got, blocked := moveAndCollideWithStep(terrain, playerBody, start, [3]float64{0.6, 0, 0}, playerStepHeight)
			if blocked[0] != tc.blocked {
				t.Errorf("horizontal blocked = %v, want %v", blocked[0], tc.blocked)
			}
			if math.Abs(got[1]-tc.wantY) > collisionSkin {
				t.Errorf("feet y = %.6f, want %.1f", got[1], tc.wantY)
			}
		})
	}
}

// solidAt returns a predicate matching exactly the listed voxels.
func solidAt(voxels ...[3]int64) func(x, y, z int64) bool {
	return func(x, y, z int64) bool {
		for _, v := range voxels {
			if v == [3]int64{x, y, z} {
				return true
			}
		}
		return false
	}
}

func TestALineOfSightStopsAtTheFirstSolidVoxelItEnters(t *testing.T) {
	t.Parallel()

	cases := []struct {
		name  string
		want  func(x, y, z int64) bool
		from  [3]float64
		to    [3]float64
		clear bool
	}{
		{
			name:  "empty air is clear",
			from:  [3]float64{0.5, 64.9, -2.0},
			to:    [3]float64{0.5, 64.9, 0.5},
			clear: true,
		},
		{
			// The defect this whole change exists for: one voxel, standing between two
			// bodies well inside a draugr's attackRange.
			name:  "one voxel between the two points blocks",
			want:  solidAt([3]int64{0, 64, -1}),
			from:  [3]float64{0.5, 64.9, -2.0},
			to:    [3]float64{0.5, 64.9, 0.5},
			clear: false,
		},
		{
			// The same voxel, one layer down. A wall a body can see over is not a wall,
			// and a traversal that tested a column rather than a line would call it one.
			name:  "a voxel below the line does not block",
			want:  solidAt([3]int64{0, 63, -1}),
			from:  [3]float64{0.5, 64.9, -2.0},
			to:    [3]float64{0.5, 64.9, 0.5},
			clear: true,
		},
		{
			// Both directions along the same segment, because the negative-step half of
			// the traversal computes its first boundary differently from the positive one.
			name:  "the same wall blocks from the far side",
			want:  solidAt([3]int64{0, 64, -1}),
			from:  [3]float64{0.5, 64.9, 0.5},
			to:    [3]float64{0.5, 64.9, -2.0},
			clear: false,
		},
		{
			// A slanted line, so the two axes reach their boundaries at different points
			// and the traversal has to interleave them. It enters (0,0), (1,0), (1,1),
			// (2,1) in that order.
			name:  "a slanted line is stopped by a solid it enters",
			want:  solidAt([3]int64{1, 64, 1}),
			from:  [3]float64{0.5, 64.5, 0.5},
			to:    [3]float64{2.5, 64.5, 1.5},
			clear: false,
		},
		{
			// The neighbour it passes beside rather than through. A traversal that
			// widened to the voxels near the line instead of the ones on it would call
			// this blocked, and every mob would be walled out by the block next door.
			name:  "a solid beside that line does not block it",
			want:  solidAt([3]int64{0, 64, 1}),
			from:  [3]float64{0.5, 64.5, 0.5},
			to:    [3]float64{2.5, 64.5, 1.5},
			clear: true,
		},
		{
			// Sight climbs as well as travels: a step in the terrain between a mob on the
			// low side and a player on the high one is crossed on the y axis.
			name:  "a solid the line rises through blocks",
			want:  solidAt([3]int64{0, 65, 0}),
			from:  [3]float64{0.5, 64.5, 0.5},
			to:    [3]float64{0.5, 66.5, 0.5},
			clear: false,
		},
		{
			// The fail-safe direction, and the reason it is safe: a body whose own centre
			// is inside terrain gets no shot rather than a free one.
			name:  "a point inside a solid can see nowhere",
			want:  solidAt([3]int64{0, 64, 0}),
			from:  [3]float64{0.5, 64.5, 0.5},
			to:    [3]float64{4.5, 64.5, 0.5},
			clear: false,
		},
		{
			name:  "a zero-length line in air is clear",
			from:  [3]float64{0.5, 64.5, 0.5},
			to:    [3]float64{0.5, 64.5, 0.5},
			clear: true,
		},
		{
			// Everything past the edge is solid, so nothing out there is in sight of
			// anything — the same answer overlaps gives a box that reaches it.
			name:  "a point beyond the world sees nothing",
			from:  [3]float64{0.5, 64.5, 0.5},
			to:    [3]float64{worldLimit + 1, 64.5, 0.5},
			clear: false,
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			terrain := scriptedTerrain{want: tc.want}
			if got := clearLineOfSight(terrain, tc.from, tc.to); got != tc.clear {
				t.Errorf("clearLineOfSight(%v, %v) = %v, want %v", tc.from, tc.to, got, tc.clear)
			}
		})
	}
}

// A non-resident chunk is solid, which every other terrain read on the tick already
// agrees about. Sight is the same: a blow does not cross terrain the server has not
// generated yet.
func TestSightDoesNotCrossTerrainThatHasNotArrived(t *testing.T) {
	t.Parallel()

	absent := dropTerrain{
		groundTop: 63,
		absent:    func(_, _, z int64) bool { return z == -1 },
	}
	from := [3]float64{0.5, 64.9, -2.0}
	to := [3]float64{0.5, 64.9, 0.5}
	if clearLineOfSight(absent, from, to) {
		t.Error("a line crossing a chunk the server has not generated reported clear")
	}
	if !clearLineOfSight(dropTerrain{groundTop: 63}, from, to) {
		t.Error("the same line over resident air reported blocked")
	}
}
