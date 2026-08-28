package game

import "github.com/FabioSM46/voxelheim-v2/server/internal/world"

// The one derivation every scripted terrain in these tests answers [Terrain.Fluid]
// with.
//
// **One implementation rather than one per fixture, and it is the production rule.**
// A fixture that spelled its own answer could disagree with the block table it also
// serves — a terrain claiming a voxel is water while reporting it as stone is a world
// no generator can produce — and the two would then be testing each other rather than
// the simulation. [world.Fluid] is the same function CacheTerrain reads.
//
// movement_test.go holds a second copy, because it is the one fixture file in
// `package game_test` and an unexported helper does not cross that line.

// blockReader is the half of [Terrain] every scripted world here actually writes: a
// table from a coordinate to a block.
type blockReader interface {
	Block(x, y, z int64) (world.Block, bool)
}

func fluidByBlock(t blockReader, x, y, z int64) bool {
	block, resident := t.Block(x, y, z)
	return resident && world.Fluid(block)
}
