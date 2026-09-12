package game

import "github.com/FabioSM46/voxelheim-v2/server/internal/world"

// solidVoxelBlocksSight refines a voxel already proven solid into the same occupied boxes movement and
// projectiles consume. Missing terrain and synthetic solidity remain full blockers.
// The caller still traverses every voxel crossed by the segment, without sampling.
func solidVoxelBlocksSight(t Terrain, voxel [3]int64, from, to [3]float64) bool {
	var block world.Block
	var resident bool
	if reader, ok := t.(collisionBlockReader); ok {
		block, resident = reader.collisionBlock(voxel[0], voxel[1], voxel[2])
	} else {
		block, resident = t.Block(voxel[0], voxel[1], voxel[2])
	}
	// DDA already established that the segment visits this solid voxel. Ordinary
	// cubes need no bounds/intersection work; only real shapes refine that answer.
	if !resident || world.ShapeOf(block).Kind == world.ShapeCube || !world.Solid(block) {
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
