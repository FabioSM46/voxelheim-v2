package world

import (
	"maps"
	"sync"
)

// Deltas is the record of every voxel that has been edited away from the state the
// generator produced.
//
// **It is a layer, never a change to the generator**, and that is the whole reason it
// exists as a separate type. Generate stays a pure function of (seed, coord); a chunk
// a consumer reads is that function's output with this map applied on top. The GDD's
// weekly Fimbulvetr storm regenerates every unprotected chunk to its *original*
// procedural state, which is a one-line operation while the edits live here and an
// impossible one the moment anything bakes an edit into the terrain function.
//
// It is also what makes eviction safe now that chunks can change. A cache entry may be
// thrown away at any time; this outlives it, so the chunk that comes back carries the
// edits it had.
//
// Sparse on purpose: a chunk holds 32768 voxels and a player edits a handful of them,
// so a map per edited chunk costs a few hundred bytes rather than the 64 KiB a second
// copy of the voxels would.
type Deltas struct {
	// mu guards chunks. Deltas is safe for concurrent use on its own — edits arrive on
	// session goroutines while a chunk being generated on another one composes them —
	// and no method of it blocks on anything else while holding the lock.
	mu sync.RWMutex

	// chunks maps a chunk to the voxels edited inside it, keyed by the offset Index
	// produces. The outer map is only populated for chunks somebody has actually
	// edited, so an untouched world costs one empty map.
	chunks map[Coord]map[int]Block
}

// NewDeltas returns an empty edit layer.
func NewDeltas() *Deltas {
	return &Deltas{chunks: make(map[Coord]map[int]Block)}
}

// Record stores that the voxel at index inside coord is now block.
//
// index is the offset Index produces, so callers pass Index(Local(x), Local(y),
// Local(z)) and it is in range by construction — Local always answers 0..ChunkSize-1,
// for negative world coordinates too.
//
// A recorded edit is never removed, not even when it restores the value the generator
// produced. Detecting that would mean generating the chunk to compare against, which
// is exactly the millisecond-scale work the edit path is built to stay out of, and the
// payoff would be a slightly smaller map.
func (d *Deltas) Record(coord Coord, index int, block Block) {
	d.mu.Lock()
	defer d.mu.Unlock()

	edits, ok := d.chunks[coord]
	if !ok {
		edits = make(map[int]Block)
		d.chunks[coord] = edits
	}
	edits[index] = block
}

// ApplyTo writes this layer's edits for c.Coord into c.
//
// The caller must own c: either it has just been generated and nobody else can see it
// yet, or it is a copy made for exactly this purpose. Composition writes voxels, and a
// chunk that is already resident is read without a lock by the tick loop.
//
// Applying twice is applying once — the map holds the value, not a diff — which is what
// makes the edit path's ordering forgiving: a chunk evicted between an edit being
// recorded and the resident copy being patched regenerates with the edit already in it.
func (d *Deltas) ApplyTo(c *Chunk) {
	d.mu.RLock()
	defer d.mu.RUnlock()

	for index, block := range d.chunks[c.Coord] {
		c.Blocks[index] = block
	}
}

// CountFor is how many voxels have been edited inside one chunk.
func (d *Deltas) CountFor(coord Coord) int {
	d.mu.RLock()
	defer d.mu.RUnlock()
	return len(d.chunks[coord])
}

// Count is how many voxels have been edited in the whole world.
func (d *Deltas) Count() int {
	d.mu.RLock()
	defer d.mu.RUnlock()

	total := 0
	for _, edits := range d.chunks {
		total += len(edits)
	}
	return total
}

// Known reports whether this layer already holds edits for coord.
//
// The question persistence asks before reading a file: a chunk this layer knows about
// needs nothing from disk, because the disk is only ever written *from* here and Record
// never removes anything, so what is in memory is never behind what is in the file.
func (d *Deltas) Known(coord Coord) bool {
	d.mu.RLock()
	defer d.mu.RUnlock()
	return len(d.chunks[coord]) > 0
}

// Restore installs edits read back from disk, without overwriting anything already
// recorded in memory.
//
// The precedence is the whole content of the method. A stored edit can only ever be one
// this layer wrote out, so where the two disagree memory is the later of the two — a chunk
// edited, evicted and regenerated before the saver caught up would otherwise have its
// newest edit undone by its own last save. Restore is therefore *not* Record's bulk form,
// and calling it for a chunk somebody is editing is safe rather than merely unlikely.
func (d *Deltas) Restore(coord Coord, stored map[int]Block) {
	if len(stored) == 0 {
		return
	}

	d.mu.Lock()
	defer d.mu.Unlock()

	edits, ok := d.chunks[coord]
	if !ok {
		edits = make(map[int]Block, len(stored))
		d.chunks[coord] = edits
	}
	for index, block := range stored {
		if _, live := edits[index]; !live {
			edits[index] = block
		}
	}
}

// Forget drops every edit recorded for coord, so this layer stops knowing the chunk at
// all.
//
// **The one method that removes an edit, and the Fimbulvetr is why it exists.** Record's
// promise that an edit is never removed is about a single voxel returning to the value
// the generator produced — detecting that would cost a generation — and not about a chunk
// being put back wholesale, which is exactly what the storm does to everything nobody
// warded. The type comment above calls that a one-line operation; this is the line.
//
// It is only half of a restoration. [Store.RemoveChunk] is the other half, and skipping it
// would leave the file for the next hydration to read back in; skipping this one would
// leave the edits in memory for the next save to write out again. [Cache.Regenerate] does
// both, in an order that holds.
//
// Deleting the coordinate rather than emptying its map keeps the outer map bounded across
// storms: [Deltas.Known] reads a length and would answer false either way, but a world
// regenerated every week would otherwise accumulate one empty map per chunk anybody has
// ever edited, for the life of the process.
func (d *Deltas) Forget(coord Coord) {
	d.mu.Lock()
	defer d.mu.Unlock()
	delete(d.chunks, coord)
}

// Snapshot copies the edits recorded for coord, for a caller that is about to spend real
// time with them — writing them to disk — and must not hold the lock while it does.
//
// A chunk with no edits answers nil, which is the same value SaveChunk reads as "nothing
// to write".
func (d *Deltas) Snapshot(coord Coord) map[int]Block {
	d.mu.RLock()
	defer d.mu.RUnlock()
	return maps.Clone(d.chunks[coord])
}
