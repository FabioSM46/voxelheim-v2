package main

import (
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// blockView is the terrain the server streamed, patched by every BlockUpdate since: the only
// thing the route plans over. An unsent chunk is unknown, and unknown is never walkable.
type blockView struct {
	chunks map[world.Coord][]world.Block
}

func newBlockView() *blockView { return &blockView{chunks: make(map[world.Coord][]world.Block)} }

// block answers the voxel at a world cell and whether its chunk has arrived.
func (v *blockView) block(x, y, z int64) (world.Block, bool) {
	blocks, ok := v.chunks[world.ChunkOf(x, y, z)]
	if !ok {
		return world.Air, false
	}
	return blocks[world.Index(world.Local(x), world.Local(y), world.Local(z))], true
}

func (v *blockView) set(x, y, z int64, b world.Block) {
	if blocks, ok := v.chunks[world.ChunkOf(x, y, z)]; ok {
		blocks[world.Index(world.Local(x), world.Local(y), world.Local(z))] = b
	}
}

// solid is true for a delivered solid voxel only.
func (v *blockView) solid(x, y, z int64) bool {
	b, ok := v.block(x, y, z)
	return ok && world.Solid(b)
}

// open is a delivered voxel a body can occupy: neither solid nor unknown. A portal's veil
// is not open to a walk either: stepping into one is leaving, never passing through.
func (v *blockView) open(x, y, z int64) bool {
	b, ok := v.block(x, y, z)
	return ok && !world.Solid(b) && !world.Portal(b)
}

func (v *blockView) water(x, y, z int64) bool {
	b, ok := v.block(x, y, z)
	return ok && world.IsWater(b)
}

// standable is the walking body's cell, the same rule the route estimate walks with: two
// clear courses over something solid, and not standing in water.
func (v *blockView) standable(c cell) bool {
	return v.open(c[0], c[1], c[2]) && !v.water(c[0], c[1], c[2]) &&
		v.open(c[0], c[1]+1, c[2]) && v.solid(c[0], c[1]-1, c[2])
}

type cell = [3]int64

var sideSteps = [4][2]int64{{1, 0}, {-1, 0}, {0, 1}, {0, -1}}

// path is the shortest walk over whole blocks to the first cell goal accepts — a step along
// X or Z, up one course with room to jump, or down up to three — as the cells after from, or
// nil. Bounded, so an unreachable goal does not visit the whole view on every replan.
func (v *blockView) path(from cell, goal func(cell) bool) []cell {
	const limit = 200_000
	prev := map[cell]cell{from: from}
	queue := []cell{from}
	for len(queue) > 0 && len(prev) < limit {
		c := queue[0]
		queue = queue[1:]
		if goal(c) {
			var out []cell
			for c != from {
				out = append(out, c)
				c = prev[c]
			}
			for i, j := 0, len(out)-1; i < j; i, j = i+1, j-1 {
				out[i], out[j] = out[j], out[i]
			}
			return out
		}
		for _, step := range sideSteps {
			for _, dy := range [...]int64{0, 1, -1, -2, -3} {
				next := cell{c[0] + step[0], c[1] + dy, c[2] + step[1]}
				if _, seen := prev[next]; seen || !v.standable(next) {
					continue
				}
				// Up needs room over the current cell to jump into; down needs the
				// column over the next cell clear from where the body leaves.
				if dy == 1 && !v.open(c[0], c[1]+2, c[2]) {
					continue
				}
				blocked := false
				for y := next[1] + 2; y <= c[1]+1; y++ {
					if !v.open(next[0], y, next[2]) {
						blocked = true
					}
				}
				if blocked {
					continue
				}
				prev[next] = c
				queue = append(queue, next)
			}
		}
	}
	return nil
}
