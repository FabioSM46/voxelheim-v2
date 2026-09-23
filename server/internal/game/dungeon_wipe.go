package game

import "github.com/FabioSM46/voxelheim-v2/server/internal/world"

// The wipe rule for the lesser creatures (#1294).
//
// **A wipe resets the encounter the party lost, and nothing it had won.** On the tick
// every player inside is dead, the current zone's uncleared encounter goes back to how
// the party first found it:
//
//   - **The current zone is the one the party fell in**: every zone holding a member's
//     body, since the leash keeps every creature inside its own zone and the fight that
//     killed the last member was therefore in theirs. The cave's waves are current
//     whenever they have begun and not been beaten, wherever the bodies lie: a party that
//     ran from the spiders to the shore and fell there lost to the waves all the same.
//   - **A surviving creature of that zone returns to its slot at full health** — a fresh
//     body under a fresh identity, as a wiped boss is, so nothing aimed at the old one
//     can land on the new. A scorpion that had risen lies buried again; one still under
//     the sand was never engaged and is left where it is.
//   - **The waves start again from the first**: every spider still out goes back into
//     the walls, and the cavern's trigger is armed again, so the waves come when the
//     party walks back into the cavern rather than onto their respawn.
//
// **What it never touches**, each the party's for good: a creature killed stays killed,
// a cleared group stays cleared, a solved puzzle stays solved, a reached checkpoint stays
// reached, and a defeated boss stays dead. A living, engaged boss is the one encounter
// reset elsewhere — [Sim.resetWipedDungeonLocked], on its own terms — and a zone nobody
// fell in keeps whatever is in it, hurt or not.
//
// **An empty instance is not a wipe.** A party that disconnected has not lost, so nothing
// here runs until somebody is inside and nobody inside is alive; see dungeon_waves.go.

// resetWipedDescentLocked applies the wipe rule to the lesser creatures and reports
// whether it created or removed any.
//
// The caller holds Sim.mu.
func (s *Sim) resetWipedDescentLocked(players []*Player) bool {
	desc := &s.dungeon.descent
	current := make(map[int]bool)
	for group, zone := range desc.zones {
		leash := &mobLeash{zone: zone}
		for _, p := range players {
			if leash.holds(p.pos) {
				current[group] = true
				break
			}
		}
	}
	if desc.waves.started {
		current[world.CaveBurrowGroup] = true
	}
	changed := false
	for group := range current {
		if s.dungeonGroupClearedLocked(group) {
			continue
		}
		if group == world.CaveBurrowGroup {
			changed = s.restartSpiderWavesLocked() || changed
			continue
		}
		changed = s.resetGroupLocked(group) || changed
	}
	return changed
}

// resetGroupLocked puts every surviving, engaged creature of one placed group back on
// its slot at full health, under a fresh identity.
func (s *Sim) resetGroupLocked(group int) bool {
	desc := &s.dungeon.descent
	species := dungeonGroupSpecies[group]
	changed := false
	for i, id := range desc.groups[group] {
		m := s.mobs[id]
		if m == nil || m.buried {
			continue
		}
		home, known := desc.homes[id]
		if !known {
			continue
		}
		fresh, made := s.placeTieredMinorLocked(m.kind, anchorStanding(home), desc.zones[group], species.buried)
		if !made {
			s.log.Error("could not reset a dungeon creature after a wipe", "kind", m.kind, "group", group)
			continue
		}
		s.discardMobLocked(m)
		delete(desc.homes, id)
		desc.homes[fresh] = home
		desc.groups[group][i] = fresh
		changed = true
	}
	return changed
}

// restartSpiderWavesLocked sends every spider still out back into the walls and puts the
// schedule back to before the first wave, with the cavern's trigger armed again.
func (s *Sim) restartSpiderWavesLocked() bool {
	desc := &s.dungeon.descent
	changed := false
	for _, id := range desc.groups[world.CaveBurrowGroup] {
		if m := s.mobs[id]; m != nil {
			s.discardMobLocked(m)
			changed = true
		}
	}
	desc.groups[world.CaveBurrowGroup] = nil
	burrows := desc.waves.burrows
	desc.waves = spiderWaves{burrows: burrows}
	for i := range desc.triggers {
		if desc.triggers[i].index == world.CaveTrigger {
			desc.triggers[i].fired = false
		}
	}
	return changed
}
