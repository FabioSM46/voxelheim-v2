package game

import (
	"errors"
	"math"

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
		pass.coords = pass.coords[1:]
		budget--

		if !pass.keep(coord.Column()) {
			s.regenerateChunkLocked(coord)
		}
		if len(pass.coords) == 0 {
			s.regeneration[0] = chunkRegenerationPass{}
			s.regeneration = s.regeneration[1:]
		}
	}
}

func (s *Sim) regenerateChunkLocked(coord world.Coord) {
	structuresChanged := false
	for id, held := range s.structures {
		if held.chunk != coord || held.worldOwned() {
			continue
		}
		delete(s.structures, id)
		structuresChanged = true
	}
	if structuresChanged {
		s.structuresDirty = true
	}

	for id, drop := range s.drops {
		if drop.chunk == coord {
			delete(s.drops, id)
		}
	}
	for id, corpse := range s.corpses {
		if corpse.chunk == coord {
			s.removeCorpseLocked(id)
		}
	}
	for id, mob := range s.mobs {
		if mob.chunk == coord {
			delete(s.mobs, id)
		}
	}

	// Regenerate republishes a resident chunk before it returns, so the collision
	// reader below observes the restored composition through its revision check.
	if err := s.chunkRegenerator.Regenerate(coord); err != nil {
		s.log.Error("chunk regeneration did not finish cleanly", "coord", coord, "error", err)
	}

	for _, player := range s.players {
		if !overlapsChunk(s.terrain, playerBox(player.pos), coord) {
			continue
		}

		x := int64(math.Floor(player.pos[0]))
		z := int64(math.Floor(player.pos[2]))
		player.pos[1] = float64(world.GeneratedColumnTop(s.worldSeed, x, z) + world.SpawnClearance)
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
}

// overlapsChunk reports whether b intersects a solid voxel in exactly coord. Limiting
// the scan to the regenerated chunk avoids lifting a body for unrelated old terrain in
// a neighbouring chunk it happens to straddle.
func overlapsChunk(terrain Terrain, b box, coord world.Coord) bool {
	return anyVoxel(b, func(x, y, z int64) bool {
		return world.ChunkOf(x, y, z) == coord && terrain.Solid(x, y, z)
	})
}
