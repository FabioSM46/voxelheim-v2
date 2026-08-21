package world

import (
	"errors"
	"fmt"
)

// ErrMalformedRuns marks a run-length payload that violates the invariants
// schemas/world.fbs documents. It exists so a caller can log one class and close
// the connection instead of branching on the shape of the damage.
var ErrMalformedRuns = errors.New("world: malformed run-length payload")

// Encode returns the chunk's voxels as flat (block id, run length) pairs in the
// index order Index defines.
//
// Flat pairs rather than a vector of tables, because a table per run would cost
// an offset each and a terrain chunk holds hundreds of them. The whole point is
// size: 32³ voxels are 64 KiB raw, and a heightmap chunk compresses to a few
// hundred bytes because each column is a handful of long runs.
func Encode(c *Chunk) []uint16 {
	// A chunk of alternating blocks would need 2 pairs per voxel; a terrain chunk
	// needs a few hundred. Size for the common case and let append handle the rest.
	pairs := make([]uint16, 0, 512)

	current := c.Blocks[0]
	run := uint16(1)
	for _, block := range c.Blocks[1:] {
		if block == current && run < maxRun {
			run++
			continue
		}
		pairs = append(pairs, uint16(current), run)
		current, run = block, 1
	}
	pairs = append(pairs, uint16(current), run)

	return pairs
}

// maxRun is the longest run a uint16 length can express. A chunk fits in a single
// run (see the compile-time guard in chunk.go), so the cap is unreachable today;
// the encoder honours it anyway so that raising ChunkSize produces a correct
// payload rather than a silently truncated one.
const maxRun = 0xFFFF

// Decode expands run-length pairs back into voxels, enforcing every invariant on
// untrusted input: even length, no zero-length run, and lengths summing to exactly
// ChunkVolume.
//
// A decoder that trusted the sum would either allocate whatever the payload asked
// for or leave a partly filled chunk to be read as terrain. Both are worse than an
// error.
func Decode(pairs []uint16) ([]Block, error) {
	if len(pairs) == 0 {
		return nil, fmt.Errorf("%w: no runs", ErrMalformedRuns)
	}
	if len(pairs)%2 != 0 {
		return nil, fmt.Errorf("%w: %d values is not a whole number of (id, run) pairs", ErrMalformedRuns, len(pairs))
	}

	blocks := make([]Block, 0, ChunkVolume)
	for i := 0; i < len(pairs); i += 2 {
		block, run := Block(pairs[i]), int(pairs[i+1])
		if run == 0 {
			return nil, fmt.Errorf("%w: run %d has zero length", ErrMalformedRuns, i/2)
		}
		if len(blocks)+run > ChunkVolume {
			return nil, fmt.Errorf("%w: runs describe more than %d voxels", ErrMalformedRuns, ChunkVolume)
		}
		for range run {
			blocks = append(blocks, block)
		}
	}

	if len(blocks) != ChunkVolume {
		return nil, fmt.Errorf("%w: runs describe %d voxels, want %d", ErrMalformedRuns, len(blocks), ChunkVolume)
	}
	return blocks, nil
}
