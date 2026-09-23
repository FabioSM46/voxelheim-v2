package game

import "github.com/FabioSM46/voxelheim-v2/server/internal/world"

// How many of the dungeon's lesser creatures a party of one to four meets (#1294).
//
// **The rule is the boss balance's, carried from health to numbers.** #1099 grows a boss's
// health with the members and nothing else, because a member's blade is the same at every
// level: a party of n deals n times one member's damage, so a boss worth n shares takes
// the same time to kill whatever n is (boss_scale.go). A lesser creature's row is shared
// with the open world and is not scaled, so here the members buy creatures instead of
// health: a group or a wave built for four holds `ceil(slots × members / 4)` for a party
// of `members`.
//
//   - **The fight lasts as long for every party.** A group's total health grows with the
//     members exactly as the party's damage does, so a hall takes a party of one about as
//     long as a party of four — the same argument, and the same shape of answer, as #1099.
//   - **Each member meets about the same number of creatures.** A blow a creature lands
//     is its row's at every party size, so the pressure on one member is the creatures per
//     member, which the rule holds near one to one of a full party's.
//   - **Rounded up, and never below one.** A hall with nothing in it is not a hall, and
//     rounding down would take a solo player's last creature from every group whose slots
//     do not divide by four. Rounding up is the direction that cannot make a fight shorter
//     than its share.
//   - **Four is the ceiling, as for a boss.** The slots are drawn for four
//     ([bossScaleMaxMembers]); a fifth member meets what four do. The dungeon's creature
//     ceiling is therefore unchanged: no party meets more than every slot and every wave.
//
// The numbers it gives, for the groups the layout draws:
//
//	slots      1 member  2  3  4
//	4 (halls)         1  2  3  4
//	8 (sand)          2  4  6  8
//	4 (wave 1)        1  2  3  4
//	5 (wave 2)        2  3  4  5
//	6 (wave 3)        2  3  5  6
//
// **Decided when the party first steps into the zone, with whoever is inside then** — the
// characters in the instance, alive or dead, as a boss's pull counts them. Every slot is
// filled when the instance is built, since nobody is inside yet to count; the slots past
// the party's share leave on the tick the zone wakes, and the ones kept are the first
// declared. A member who joins afterwards changes nothing about a hall already woken, as
// a member who joins after the pull changes nothing about a boss. A wave is sized as it
// comes out, by the party inside at that moment.

// minorShare is how many of a group's or wave's slots a party of members meets: a
// share of slots proportional to the members out of [bossScaleMaxMembers], rounded up,
// and at least one.
func minorShare(slots, members int) int {
	members = min(max(members, 1), bossScaleMaxMembers)
	return max(1, (slots*members+bossScaleMaxMembers-1)/bossScaleMaxMembers)
}

// wakeDungeonGroupsLocked wakes every placed group whose zone a live player has just
// stepped into, trimming it to the party's share, and reports whether it took any
// creature away.
//
// The caller holds Sim.mu.
func (s *Sim) wakeDungeonGroupsLocked(players []*Player) bool {
	desc := &s.dungeon.descent
	changed := false
	for group, ids := range desc.groups {
		// The burrows are the waves', and a wave is sized as it comes out.
		if group == world.CaveBurrowGroup || desc.woken[group] {
			continue
		}
		leash := &mobLeash{zone: desc.zones[group]}
		entered := false
		for _, p := range players {
			if p.alive() && leash.holds(p.pos) {
				entered = true
				break
			}
		}
		if !entered {
			continue
		}
		desc.woken[group] = true
		keep := minorShare(len(ids), len(players))
		for _, id := range ids[keep:] {
			if m := s.mobs[id]; m != nil {
				s.discardMobLocked(m)
				changed = true
			}
			delete(desc.homes, id)
		}
		desc.groups[group] = ids[:keep:keep]
	}
	return changed
}
