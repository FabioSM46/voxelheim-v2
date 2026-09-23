package game

import (
	"math"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// How many of the dungeon's lesser creatures a party meets (#1294, re-decided on #1332).
//
// **Every placed group is a pack of three to five, one creature for each member the
// dungeon is sized for** — [dungeonMembers], the same clamp a boss's health reads
// (boss_scale.go). A party of three, four or five meets a pack of its own size; a party of
// one or two meets a pack of three. The spider waves come as packs on the same rule.
//
//   - **The fight lasts as long for every party it is made for.** A member's blade is the
//     same at every level, so a party of n deals n times one member's damage; a pack of n
//     creatures is n times one creature's health. A hall therefore takes three, four and
//     five the same time — the argument, and the shape of answer, #1099 gives a boss.
//   - **Each member meets one creature.** A creature's blow is its tier's at every party
//     size, so the pressure on a member is the creatures per member, which the rule holds
//     at exactly one for three to five — and at three for a solo delver, which is the
//     point: one blade against three sets of jaws dies before the first of them does.
//   - **Three is the floor, deliberately, and it replaces the #1294 rule** of
//     `ceil(slots × members / 4)`, which scaled a hall down to one creature so that a solo
//     clear was feasible. The owner's decision on #1332 is that it must not be.
//   - **Five is the ceiling, and the layout draws exactly that.** Every placed group holds
//     [dungeonPackMax] slots (world's schematic_instance.go), so the largest pack stands
//     on its own slots and a smaller one keeps the first declared, which the layout orders
//     to spread a pack of three across its room.
//
// The numbers it gives, for every group the layout draws:
//
//	members       1  2  3  4  5
//	pack size     3  3  3  4  5
//	per member    3  1.5 1  1  1
//
// **Decided when the party first steps into the zone, with whoever is inside then** — the
// characters in the instance, alive or dead, as a boss's pull counts them. Every slot is
// filled when the instance is built, since nobody is inside yet to count; the slots past
// the pack leave on the tick the zone wakes, and the ones kept are the first declared. A
// member who joins afterwards changes nothing about a hall already woken, as a member who
// joins after the pull changes nothing about a boss. A wave is sized as it comes out, by
// the party inside at that moment.

// dungeonPackMax is the largest pack: one creature for each member of the largest party.
const dungeonPackMax = bossScaleMaxMembers

// dungeonPackSize is how many creatures a placed group or a spider wave holds for a party
// of members: one per member the dungeon is sized for, three to five.
func dungeonPackSize(members int) int {
	return dungeonMembers(members)
}

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
// ([spiderWaveCount]), each a pack of the party's size, each after a [spiderWaveBreather]
// of 40 seconds once the previous is dead, or [spiderWaveInterval] = 45 seconds after it
// began if it is not: the breather is the pace a party that holds the cavern meets, and
// the interval five seconds past it is the pressure on one that cannot. Never more than
// [spiderWaveCap] out at once — three packs of five, the fifteen the snapshot budget was
// measured with — so neither the dungeon's creature ceiling nor that budget moves far:
// the ceiling is 42 at a party of five, against 41 before #1332.
//
// **The band is 17–23 minutes for parties of three, four and five, and these numbers aim
// at its middle.** TestTheRouteTakesSeventeenToTwentyThreeMinutes estimates the whole
// route from its walked length, the iron readers' boss kills measured under the energy
// economy, and the tier's time to kill at those readers' energy-bound pace, and fails when
// any of the three leaves the band. At this balance it gives 20.0 minutes for each, at
// every rotation: about 8.1 of them the siege, 8.8 the bosses, 2 the walk and one the
// halls. The three are the same because every fight is sized per member — a fourth or
// fifth member brings as much health as they bring damage. #1332 moved no tier and no
// wave to land there: re-measuring the bosses under energy and re-setting their rows to
// the approved design's minutes was enough, and the siege and the halls, re-read at the
// energy-bound pace, were already where the band wanted them.
//
// **A party of one or two is not held to the band.** It meets the dungeon sized for three
// — each member doing one and a half or three members' work — and the #1099 stander at the
// iron reference dies to either boss and to one pack before killing it
// (dungeon_group_test.go).

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

// wakeDungeonGroupsLocked wakes every placed group whose zone a live player has just
// stepped into, trimming it to the party's pack, and reports whether it took any
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
		keep := min(len(ids), dungeonPackSize(len(players)))
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
