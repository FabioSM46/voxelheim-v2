package game

import "github.com/FabioSM46/voxelheim-v2/server/internal/world"

// voxelBlocksSight refines a solid voxel into the same occupied boxes movement and
// projectiles consume. Missing terrain and synthetic solidity remain full blockers.
// The caller still traverses every voxel crossed by the segment, without sampling.
func voxelBlocksSight(t Terrain, voxel [3]int64, from, to [3]float64) bool {
	if !t.Solid(voxel[0], voxel[1], voxel[2]) {
		return false
	}
	block, resident := t.Block(voxel[0], voxel[1], voxel[2])
	if !resident || !world.Solid(block) {
		return true
	}
	bounds, n := world.CollisionBounds(block)
	for i := range n {
		var shape box
		for axis := range 3 {
			shape.min[axis] = float64(voxel[axis]) + bounds[i].Min[axis]
			shape.max[axis] = float64(voxel[axis]) + bounds[i].Max[axis]
		}
		if _, hit := segmentBoxIntersection(from, to, shape); hit {
			return true
		}
	}
	return false
}
