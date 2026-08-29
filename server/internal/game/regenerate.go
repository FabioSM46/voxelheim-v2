package game

import (
	"errors"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// RegenerateChunksPerTick is the maximum number of listed chunks one authoritative
// tick examines during a regeneration pass. Ten thousand edited chunks therefore cost
// many small ticks instead of one unbounded pause under Sim.mu.
const RegenerateChunksPerTick = 64

// ChunkRegenerator is the blocking world-layer half of one regeneration.
// world.Cache implements it; the narrow interface keeps game independent of the cache
// implementation and, in particular, of the session registry paired with it.
type ChunkRegenerator interface {
	Regenerate(world.Coord) error
}

type chunkRegenerationPass struct {
	coords []world.Coord
	keep   func(world.Column) bool
}

// ConfigureChunkRegeneration wires the world cache and the session view repair into
// the simulation. It is called once during server construction, before the tick loop or
// any session starts.
func (s *Sim) ConfigureChunkRegeneration(regenerator ChunkRegenerator, resend func(world.Coord) int) error {
	if regenerator == nil {
		return errors.New("game: chunk regenerator must not be nil")
	}
	if resend == nil {
		return errors.New("game: chunk resender must not be nil")
	}

	s.mu.Lock()
	defer s.mu.Unlock()
	if s.chunkRegenerator != nil || s.resendChunk != nil {
		return errors.New("game: chunk regeneration is already configured")
	}
	s.chunkRegenerator = regenerator
	s.resendChunk = resend
	return nil
}

// RegenerateChunksLocked queues one bounded pass over coords. The caller holds Sim.mu;
// the tick consumes at most RegenerateChunksPerTick entries on each subsequent pass.
//
// keep decides at chunk-column granularity which ground survives. The predicate is the
// seam #467's wards plug into; this mechanism neither knows what a ward is nor chooses
// one by itself.
func (s *Sim) RegenerateChunksLocked(coords []world.Coord, keep func(world.Column) bool) {
	if len(coords) == 0 {
		return
	}
	if keep == nil {
		s.log.Error("refusing chunk regeneration with no keep predicate")
		return
	}
	if s.chunkRegenerator == nil || s.resendChunk == nil {
		s.log.Error("refusing chunk regeneration before its world and session seams are configured")
		return
	}

	queued := append([]world.Coord(nil), coords...)
	s.regeneration = append(s.regeneration, chunkRegenerationPass{coords: queued, keep: keep})
}

func (s *Sim) advanceChunkRegenerationLocked() {
	budget := RegenerateChunksPerTick
	for budget > 0 && len(s.regeneration) > 0 {
		pass := &s.regeneration[0]
		coord := pass.coords[0]
		budget--

		if !pass.keep(coord.Column()) {
			if err := s.regenerateChunkLocked(coord); err != nil {
				// Keep it at the head: a failed durable removal is retried next tick.
				s.log.Error("chunk regeneration did not finish cleanly", "coord", coord, "error", err)
				return
			}
		}
		pass.coords = pass.coords[1:]
		if len(pass.coords) == 0 {
			s.regeneration[0] = chunkRegenerationPass{}
			s.regeneration = s.regeneration[1:]
		}
	}
}

func (s *Sim) regenerateChunkLocked(coord world.Coord) error {
	structuresChanged := false
	for id, held := range s.structures {
		if held.worldOwned() || !structureOverlapsChunk(held, coord) {
			continue
		}
		delete(s.structures, id)
		structuresChanged = true
	}
	if structuresChanged {
		s.structuresDirty = true
		s.rebuildWardsLocked()
	}

	for id, drop := range s.drops {
		if boxOverlapsChunk(drop.box(), coord) {
			delete(s.drops, id)
		}
	}
	for id, corpse := range s.corpses {
		if boxOverlapsChunk(mobRegistry[corpse.kind].body.boxAt(corpse.pos), coord) {
			s.removeCorpseLocked(id)
		}
	}
	for id, mob := range s.mobs {
		if boxOverlapsChunk(mob.species().body.boxAt(mob.pos), coord) {
			delete(s.mobs, id)
		}
	}

	// Regenerate republishes a resident chunk before it returns, so the collision
	// reader below observes the restored composition through its revision check.
	err := s.chunkRegenerator.Regenerate(coord)

	for _, player := range s.players {
		if !overlapsChunk(s.terrain, playerBox(player.pos), coord) {
			continue
		}

		player.pos[1] = generatedLiftHeight(s.worldSeed, playerBox(player.pos), coord)
		player.vel[1] = 0
		player.onGround = false
		if next := chunkAt(player.pos); next != player.chunk {
			player.chunk = next
			player.chunks.publish(next)
		}
	}

	// ResendChunk itself selects holders. Calling it after the pointer swap guarantees
	// every woken diff reads the regenerated composition, while a session still waiting
	// on its first send simply receives that new composition through the ordinary path.
	s.resendChunk(coord)
	return err
}

// structureOverlapsChunk reports whether any support or occupied cell of held lies in
// coord. The cached chunk names only the anchor; a rotated multi-cell footprint can
// cross a horizontal boundary, and its headroom can cross a vertical one.
func structureOverlapsChunk(held *structure, coord world.Coord) bool {
	anchor := [3]int64{int64(held.anchor[0]), int64(held.anchor[1]), int64(held.anchor[2])}
	footprint, headroom, ok := footprintOf(held.kind, held.facing, anchor)
	if !ok {
		return held.chunk == coord
	}
	for _, cell := range footprint {
		for dy := int64(0); dy <= headroom; dy++ {
			if world.ChunkOf(cell[0], cell[1]+dy, cell[2]) == coord {
				return true
			}
		}
	}
	return false
}

// boxOverlapsChunk reports whether the half-open body intersects any voxel in coord.
// Entity chunk fields follow their standing position and are a visibility cache, not
// the complete physical extent used by destructive world operations.
func boxOverlapsChunk(b box, coord world.Coord) bool {
	return anyVoxel(b, func(x, y, z int64) bool { return world.ChunkOf(x, y, z) == coord })
}

// generatedLiftHeight is the safe feet height above every regenerated column the
// player's body overlaps. Choosing the maximum matters at chunk borders: the player's
// centre can remain in the neighbouring chunk while one shoulder is enclosed here.
func generatedLiftHeight(seed int64, b box, coord world.Coord) float64 {
	x0, x1 := voxelSpan(b.min[0], b.max[0])
	z0, z1 := voxelSpan(b.min[2], b.max[2])
	var top int
	found := false
	for z := z0; z <= z1; z++ {
		for x := x0; x <= x1; x++ {
			column := world.ChunkOf(x, int64(coord.Y)*world.ChunkSize, z)
			if column.X != coord.X || column.Z != coord.Z {
				continue
			}
			generated := world.GeneratedColumnTop(seed, x, z)
			if !found || generated > top {
				top = generated
				found = true
			}
		}
	}
	if !found {
		return 0
	}
	return float64(top + world.SpawnClearance)
}

// overlapsChunk reports whether b intersects a solid voxel in exactly coord. Limiting
// the scan to the regenerated chunk avoids lifting a body for unrelated old terrain in
// a neighbouring chunk it happens to straddle.
func overlapsChunk(terrain Terrain, b box, coord world.Coord) bool {
	return anyVoxel(b, func(x, y, z int64) bool {
		return world.ChunkOf(x, y, z) == coord && terrain.Solid(x, y, z)
	})
}
