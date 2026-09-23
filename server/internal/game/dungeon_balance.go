package game

import (
	"math"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

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
//	4 (waves 1, 4..)  1  2  3  4
//	5 (waves 2, 5..)  2  3  4  5
//	6 (waves 3, 6..)  2  3  5  6
//
// **Decided when the party first steps into the zone, with whoever is inside then** — the
// characters in the instance, alive or dead, as a boss's pull counts them. Every slot is
// filled when the instance is built, since nobody is inside yet to count; the slots past
// the party's share leave on the tick the zone wakes, and the ones kept are the first
// declared. A member who joins afterwards changes nothing about a hall already woken, as
// a member who joins after the pull changes nothing about a boss. A wave is sized as it
// comes out, by the party inside at that moment.

// # The dungeon's tier
//
// A lesser creature's row is the open world's, where one of them meets a lone traveller at
// level one. Placed in the dungeon — every hall's slot, every scorpion, every wave's spider
// — it carries a tier over that row, and only there: the open world's creatures, and the
// rows themselves, are untouched.
//
//   - **Health ×[dungeonHealthTier] = 4.** At its row a full party puts a whole hall down in
//     about two seconds each, and a wave dies before its telegraphs have played once; a
//     hall that ends before it can swing is not an encounter. Four times the row makes each
//     creature live about as long as its own windup-and-recovery cycle takes three or four
//     times over, against one member — long enough to be fought rather than swept, and the
//     number that, with the waves below, lands the route in the band. It is "a few times"
//     the row by the owner's decision on #1294, rather than the ~68× health alone would need:
//     the minutes come from the siege, and the tier only makes what is in it worth fighting.
//   - **Damage ×[dungeonDamageTier] = 2.** #1099 prices a boss's blow by the share of health
//     it costs through [maxHealthFor]: 200% is that ratio at level 21, the middle of the
//     1–30 range a party reaching the first dungeon brings. Twice the row is therefore the
//     same share of a mid-level member's health that the row costs a level-one traveller
//     in the open world — the creatures are as dangerous to the party the dungeon is for as
//     they are to a newcomer outside. It moves no kill time, so the route estimate below
//     does not read it.
//
// # The siege
//
// The cave's waves are the one place the route's length is set on purpose, because they
// are the one encounter whose length is a schedule rather than a health bar. Twelve waves
// of four, five and six — the original three waves' sizes four times over — each after a
// [spiderWaveBreather] of 40 seconds once the previous is dead, or [spiderWaveInterval]
// = 45 seconds after it began if it is not: the breather is the pace a party that holds
// the cavern meets, and the interval five seconds past it is the pressure on one that
// cannot. Never more than [spiderWaveCap] out at once, the three original waves together,
// so neither the dungeon's creature ceiling nor the snapshot budget measured with it moves.
//
// **The band is 17–23 minutes and these numbers aim at 18–20.**
// TestTheRouteTakesSeventeenToTwentyThreeMinutes estimates the whole route from its walked
// length, the #1099 iron readers' measured boss kills and the tier's time to kill at those
// readers' damage rate, and fails when any party of one to four leaves the band. At this
// balance it gives 18.2 minutes for four, 18.4 for three, 18.6 for two and 20.2 for one
// (the solo reader's #1099 boss kills are the slowest), of which about 7.8 are the siege,
// 8 to 9.6 the bosses, 2 the walk and half a minute the halls.

// dungeonHealthTier and dungeonDamageTier are the dungeon's tier over a lesser creature's
// row; see above.
const (
	dungeonHealthTier uint16 = 4
	dungeonDamageTier uint16 = 2
)

// placeTieredMinorLocked places one of the dungeon's own lesser creatures: as
// [Sim.placeMinorMobLocked], and at the dungeon's tier, whole from the moment it exists.
//
// The caller holds Sim.mu.
func (s *Sim) placeTieredMinorLocked(kind vnet.MobKind, pos [3]float64, zone box, buried bool) (uint64, bool) {
	return s.placeMinorLocked(kind, pos, zone, buried, true)
}

// tierScaled is a row value under a tier, widened before the multiply and clamped to the
// uint16 every health and blow is carried in, so no row can wrap however large it grows.
func tierScaled(value, tier uint16) uint16 {
	return uint16(min(uint32(value)*uint32(tier), math.MaxUint16))
}

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
