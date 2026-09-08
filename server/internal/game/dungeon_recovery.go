package game

import (
	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// Admission follows the whole pull, including telegraphs, openings and pursuit.
// A moment with no damaging region is not permission to change the participants.
func (s *Sim) dungeonCombat() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.dungeon == nil {
		return false
	}
	for _, id := range []uint64{s.dungeon.guardianID, s.dungeon.kingID} {
		if m := s.mobs[id]; m != nil && m.health > 0 && m.encounter != nil {
			return true
		}
	}
	return false
}

// Reset only an engaged, surviving dungeon boss when nobody inside is alive.
// Empty after disconnect is also abandoned combat. Defeated encounters, their
// corpses, binding/progress and the gate are not part of this transient reset.
// Fresh entity identity prevents an old attack/tap/roster from naming the new pull.
// Called before the tick can revive a body and again after mobs can kill the last
// survivor, so neither a one-tick resurrection nor a final blow misses the wipe.
func (s *Sim) resetWipedDungeonLocked() bool {
	d := s.dungeon
	if d == nil {
		return false
	}
	for _, p := range s.players {
		if p.alive() {
			return false
		}
	}
	guardian, king, _ := world.InstanceEncounterAnchors(s.worldSeed)
	reset := false
	for _, entry := range []struct {
		id   *uint64
		home world.PlacedAnchor
	}{{&d.guardianID, guardian}, {&d.kingID, king}} {
		m := s.mobs[*entry.id]
		if m == nil || m.encounter == nil || m.health == 0 {
			continue
		}
		// Retire abandoned shots before the fresh entity can become visible.
		clear(s.projectiles)
		clear(s.projectileOwners)
		pos := [3]float64{float64(entry.home.X) + .5, float64(entry.home.Y), float64(entry.home.Z) + .5}
		id, made := s.spawnMobLocked(m.kind, pos)
		if !made {
			s.log.Error("could not reset dungeon encounter", "kind", m.kind)
			continue
		}
		s.discardMobLocked(m)
		*entry.id = id
		reset = true
	}
	return reset
}

// Private and memory-only: disk still stores the normalised open-world return
// life. Instance ticks continue during disconnection, so reconnect cannot shorten
// death or renew protection. No simulation/cache pointer prolongs instance expiry.
type dungeonRecovery struct {
	anchor                      [3]float64
	valid                       bool
	seed                        int64
	deathUntil, protectionUntil uint64
}

func (p *Player) dungeonRecoveryLocked() dungeonRecovery {
	if p.sim.dungeon == nil || (p.alive() && p.protectionTicks == 0) {
		return dungeonRecovery{}
	}
	r := dungeonRecovery{valid: true, seed: p.sim.worldSeed, anchor: p.pos}
	if !p.alive() {
		r.deathUntil = p.sim.currentTick + uint64(p.respawnTicks)
		r.protectionUntil = r.deathUntil + uint64(p.sim.protectionTicks)
	} else {
		r.protectionUntil = p.sim.currentTick + uint64(p.protectionTicks)
	}
	return r
}

// Called under Sim.mu before insertion into the live player map. The normalised
// Life already includes exactly one death penalty, so restoring the countdown must
// not charge it again. Ordinary XP gained meanwhile is retained by the join path.
func (p *Player) restoreDungeonRecoveryLocked(r dungeonRecovery) {
	if !r.valid || p.sim.dungeon == nil || r.seed != p.sim.worldSeed {
		return
	}
	p.pos = r.anchor
	p.chunk = chunkAt(p.pos)
	p.chunks.publish(p.chunk)
	now := p.sim.currentTick
	if r.deathUntil > now {
		p.lifeState = vnet.LifeStateDead
		p.health = 0
		p.respawnTicks = uint32(r.deathUntil - now)
		p.penaltyApplied = true
	} else if r.protectionUntil > now {
		p.protectionTicks = uint32(r.protectionUntil - now)
	}
}
