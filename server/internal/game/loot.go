package game

import (
	"math"
	"math/rand/v2"
)

// What the dead leave behind.
//
// # A kill, and only a kill
//
// The table is a field of the registry row (species.go); everything about turning one
// into items on the ground is here. There is exactly one caller — the reap in
// [Sim.advanceMobsLocked], where a creature killed MobDeathDuration ago stops existing —
// and that is the whole of the rule the issue asked for: a mob the director takes away at
// dawn, or because nobody has been near it for five seconds, leaves nothing, because the
// two removals in spawn.go do not come through here. Loot is the reward for the kill rather
// than for having existed.
//
// # It is rolled when the body goes, not when the blow lands
//
// The blow used to be the caller. It is not, and the difference is two things.
//
// **When.** Nothing reaches the ground until the body has stopped existing — that delay is
// the point of MobDeathDuration, and it is a server delay precisely so that a client cannot
// have one item timetable and a client with the animation switched off another. Rolling at
// the blow and holding the result would be the same wait with the answer decided early; it
// would also mean carrying a rolled table on the mob for two and a half seconds, which is a
// second place a kill's outcome lives.
//
// **Where.** The voxel comes from the creature's position at the moment it is rolled, and a
// body falls while it is dying. Rolled at the blow, a draugr killed on a ledge would leave
// its bones in the air it was hit in; rolled at the reap, they land where it came to rest.
//
// The generator is unaffected either way — [Sim.loot] is advanced only inside the locked
// tick, so the same world and the same sequence of ticks still leave the same items.
//
// # The lock discipline, which is the real hazard
//
// A creature is reaped inside [Sim.advanceMobsLocked], which runs under Sim.mu — the tick
// holds that lock for its whole duration. [Sim.spawnDrop] takes the same lock *itself*,
// because its other callers are session goroutines off the tick. Calling it from inside the
// tick would therefore deadlock the server on the first kill.
//
// So the tick **collects** the loot under the lock and **spawns** it after, which is the
// shape edit.go already has for a structure a break brought down: collapseStructuresAt
// takes the lock and returns what fell, dropCollapsed puts the items on the ground outside
// it. [Sim.Step] is the one place that pairing happens for a kill — see the comment there,
// which is why Step is two functions.
//
// The consequence is worth stating rather than discovering: loot spawns *after* the tick
// that reaped the body has already encoded its snapshots, so a body that goes on tick N is
// visible as a drop on tick N+1. That is one tick, it is the same tick a drop from a
// mined block waits, and it is what makes the alternative — a re-entrant lock, or a
// spawnDrop with a locked twin — unnecessary.
//
// # Determinism is a requirement here, not a preference
//
// Every count comes from [Sim.loot], seeded from the world seed and advanced only inside
// the locked tick. No package-level rand, no reading of the wall clock: the same world
// and the same sequence of ticks leave the same items on the same ground, which is what
// lets a test assert an exact drop rather than a distribution.

// mobLootStream is the second word of the loot generator's PCG seed.
//
// **Its own stream rather than a share of [Sim.spawns], and the reason is the one
// mobSpawnStream already records from the other side.** PCG takes two words and only one
// of them is the world's; the constant is what makes this *the loot stream*. Two systems
// drawing from one generator is the mirror of two generators seeded identically: instead
// of making independent choices in step, it makes them interfere — a kill would shift
// every later spawn position in the world, so "where does the dark put the next creature"
// would depend on what the player had killed. Neither system wants to know that about the
// other, and spawn_test.go pins exact positions on the assumption that neither does.
const mobLootStream = 0x766F78656C6C6F74 // "voxellot"

// newLootRNG is the loot generator, from a world seed.
//
// The conversion is a reinterpretation rather than a range check, exactly as
// [newSpawnRNG]'s is: every int64 maps to a distinct uint64, which is all a seed word has
// to be.
func newLootRNG(worldSeed int64) *rand.Rand {
	return rand.New(rand.NewPCG(uint64(worldSeed), mobLootStream))
}

// lootRoll is one line of a species' loot table: an item, and how many of it a kill
// leaves.
//
// The range is inclusive at both ends and min == max is a fixed count rather than a
// special case, which is what keeps "one pelt" and "one or two bones" the same shape.
// There is deliberately no chance field: a drop that sometimes does not happen is the
// rare-loot design this issue keeps out of scope, and adding the column later is a
// smaller change than removing a probability everything has come to expect.
type lootRoll struct {
	item     ItemID
	min, max uint16
}

// lootDrop is one stack a kill left behind, waiting to be put on the ground.
//
// It carries the voxel rather than the mob, because by the time the tick spawns it the
// creature is gone from Sim.mobs — a pointer to one would be a pointer to something that
// has stopped existing. The voxel is where the body came to rest rather than where the blow
// landed; see the note above on when this is rolled.
type lootDrop struct {
	item  ItemID
	count uint16
	voxel [3]int64
}

// rollLootLocked is what this creature leaves behind, in its table's order.
//
// Called from the reap in [Sim.advanceMobsLocked], the moment a killed creature's body
// stops existing, and from nowhere else. The caller holds Sim.mu, which is what guards the
// generator — and has already taken the mob out of Sim.mobs, so the position read below is
// the last one anything will ever have.
//
// A roll that comes out at zero is skipped rather than spawned: [Sim.spawnDrop] refuses a
// zero count anyway, and the wire forbids one. Today no table can produce it, because the
// sweep insists every line has a minimum of at least one.
func (s *Sim) rollLootLocked(m *mob) []lootDrop {
	table := m.species().loot
	if len(table) == 0 {
		return nil
	}

	voxel := voxelAt(m.pos)
	drops := make([]lootDrop, 0, len(table))
	for _, roll := range table {
		count := roll.min
		if roll.max > roll.min {
			// Integer arithmetic, for the reason the spawn director's draw is: a float
			// expression is allowed to round differently on another architecture, and a
			// count asserted exactly should not depend on which machine ran the test.
			// IntN is exclusive at the top, so the span is +1 — both ends of a lootRoll
			// are inclusive.
			count += uint16(s.loot.IntN(int(roll.max-roll.min) + 1))
		}
		if count == 0 {
			continue
		}
		drops = append(drops, lootDrop{item: roll.item, count: count, voxel: voxel})
	}
	return drops
}

// spawnLoot puts every rolled stack on the ground.
//
// **Called with no lock held**, because [Sim.spawnDrop] takes the simulation's own — the
// contract [Sim.dropCollapsed] is written against, and the reason this is a separate
// function from the roll above rather than the second half of it.
//
// Each stack goes through the ordinary drop path with no special case: it merges with
// what is already lying there, it ages out after DropLifetime, and it is collected by
// walking over it. A pile of bones from two draugr killed in the same spot is one drop,
// by the rule a mining spree already produces one.
func (s *Sim) spawnLoot(loot []lootDrop) {
	for _, left := range loot {
		s.spawnDrop(left.item, left.count, left.voxel)
	}
}

// voxelAt is the voxel a position stands in.
//
// Floor rather than a truncation, which is the same trap chunkAt records: truncation
// rounds toward zero, so every negative coordinate would name the voxel one step back
// along that axis and a creature killed at x = -0.5 would leave its bones in the cell
// next door.
//
// A mob's position is the bottom of its box and the collision rests that a hair above the
// face it landed on, so the y this names is the air the creature was standing in rather
// than the ground under it — which is where a drop belongs, and where dropSpawnPos then
// centres it.
func voxelAt(pos [3]float64) [3]int64 {
	return [3]int64{
		int64(math.Floor(pos[0])),
		int64(math.Floor(pos[1])),
		int64(math.Floor(pos[2])),
	}
}
