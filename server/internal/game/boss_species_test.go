package game

import (
	"reflect"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// The two boss species, and what this server is allowed to do with them.
//
// boss_encounter_test.go owns the encounter *contract* — the frozen roster, the personal
// corpse, the non-kill exit — and it exercises it against a draugr row temporarily
// promoted to boss rank, because that contract has to hold for a species that does not
// exist yet. This file is the other half: the two rows that now really do carry
// [mobRankBoss], the numbers they were priced with, and the two rules that say the
// open-world director has nothing to do with either of them.

var (
	vargrGuardianRow = mobRegistry[vnet.MobKindVargrGuardian]
	draugrKingRow    = mobRegistry[vnet.MobKindDraugrKing]
)

// Exactly two boss-rank rows, and they are the two this issue registers.
//
// The sweep and the naming are both load-bearing, for the reason
// TestEveryWireKindIsARegisteredSpecies carries both halves: the sweep alone is satisfied
// by a registry that lost one, and the two names alone are satisfied by a registry that
// quietly promoted a third. A boss is the one rank in this game whose species is never
// chosen by the director, so a row that acquired it by accident would vanish from the
// open world without a single test going red.
func TestTheProductionRegistryHoldsExactlyTheTwoBossSpecies(t *testing.T) {
	t.Parallel()

	want := map[vnet.MobKind]bool{
		vnet.MobKindVargrGuardian: true,
		vnet.MobKindDraugrKing:    true,
	}
	got := map[vnet.MobKind]bool{}
	for kind, definition := range mobRegistry {
		if definition.isBoss() {
			got[kind] = true
		}
		if definition.rank == mobRankBoss && !definition.isBoss() {
			t.Errorf("%s carries mobRankBoss and isBoss() denies it", kind)
		}
	}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("the boss-rank rows are %v, want exactly %v", got, want)
	}
}

// Both rows, pinned in full — the shape the draugr's row is pinned in, and for its
// reason.
//
// A boss's numbers are the argument the registry comment makes, and a whole-row
// comparison is what makes rebalancing one a decision that has to be taken here as well
// as there. DeepEqual rather than `!=`, because a row carries a loot slice; the
// whole-row form is also what turns a field added to mobDefinition and forgotten here
// into a compile error.
func TestTheBossRowsCarryThePricedNumbers(t *testing.T) {
	t.Parallel()

	wantGuardian := mobDefinition{
		rank:        mobRankBoss,
		maxHealth:   10500,
		experience:  120,
		speed:       4.0,
		aggroRange:  24.0,
		attackRange: 2.2,
		damage:      22,
		windup:      900 * time.Millisecond,
		recovery:    1300 * time.Millisecond,
		// One change of stage, at the strap that finally tears.
		phaseHealthPercents: []uint8{55},
		body:                body{width: 1.6, height: 1.8},
		nocturnal:           false,
		loot: []lootRoll{
			{item: ItemVargrPelt, min: 3, max: 5},
			{item: ItemBone, min: 2, max: 2},
		},
	}
	if !reflect.DeepEqual(vargrGuardianRow, wantGuardian) {
		t.Errorf("the Vargr guardian's row is %+v, want %+v", vargrGuardianRow, wantGuardian)
	}

	wantKing := mobDefinition{
		rank:        mobRankBoss,
		maxHealth:   16383,
		experience:  200,
		speed:       3.0,
		aggroRange:  24.0,
		attackRange: 2.4,
		damage:      28,
		windup:      1200 * time.Millisecond,
		recovery:    1800 * time.Millisecond,
		// Duel, then ritual, then the armour cracked open and both together.
		phaseHealthPercents: []uint8{70, 35},
		body:                body{width: 1.0, height: 2.8},
		nocturnal:           false,
		loot: []lootRoll{
			{item: ItemIronSword, min: 1, max: 1},
			{item: ItemBone, min: 3, max: 5},
		},
	}
	if !reflect.DeepEqual(draugrKingRow, wantKing) {
		t.Errorf("the Draugr king's row is %+v, want %+v", draugrKingRow, wantKing)
	}
}

// The three directional constraints the issue states, executed rather than asserted in
// prose.
//
// These are deliberately *relations* rather than values — the values are pinned above —
// because each one is the reason a number was chosen and each survives a rebalance that
// the pin does not. A boss faster than a walking player takes retreat off the table
// entirely; a boss worth less than the creature it is a bigger cousin of prices the
// dungeon below the open world; and a boss a single player removes is not an encounter.
func TestTheBossRowsHonourTheirDirectionalConstraints(t *testing.T) {
	t.Parallel()

	for kind, def := range map[vnet.MobKind]mobDefinition{
		vnet.MobKindVargrGuardian: vargrGuardianRow,
		vnet.MobKindDraugrKing:    draugrKingRow,
	} {
		// Below WalkSpeed, so crossing the room away from it is a decision a party can
		// take. The field vargr is above it on purpose; a boss must not inherit that.
		if def.speed >= WalkSpeed {
			t.Errorf("%s travels at %v against a player's %v, so retreating buys nothing",
				kind, def.speed, WalkSpeed)
		}
		// Above the field vargr's 20, which is the highest reward in the open world.
		if def.experience <= vargrRow.experience {
			t.Errorf("%s is worth %d experience against a field vargr's %d",
				kind, def.experience, vargrRow.experience)
		}
		// A fight a party has. Ten draugr is the floor written down here: the rows are
		// twelve and twenty, so this leaves room to rebalance without leaving room to
		// make one of them a creature a single player removes on the way past.
		if want := 10 * draugrRow.maxHealth; def.maxHealth < want {
			t.Errorf("%s has %d health, under the %d that is ten draugr",
				kind, def.maxHealth, want)
		}
		if !def.isBoss() {
			t.Errorf("%s is not boss-rank, and every rule in this file is about that rank", kind)
		}
	}

	// And the two are not one creature at two sizes. The king is the second fight: more
	// of it, slower, hitting harder, worth more.
	if draugrKingRow.maxHealth <= vargrGuardianRow.maxHealth ||
		draugrKingRow.speed >= vargrGuardianRow.speed ||
		draugrKingRow.damage <= vargrGuardianRow.damage ||
		draugrKingRow.experience <= vargrGuardianRow.experience {
		t.Errorf("the king %+v does not read as the second fight against the guardian %+v",
			draugrKingRow, vargrGuardianRow)
	}
}

// The open-world spawn director offers neither of them, at either hour.
//
// **This is the acceptance criterion "the director never places it", and it is asked of
// [spawnableSpecies] rather than of the director** — which is where the answer lives, and
// deliberately so: spawn.go's four rules are about refilling the dark around a moving
// player, and none of them describes a fixed encounter in a sealed room. Teaching the
// director what an instance is would be the wrong shape twice over.
func TestTheOpenWorldDirectorNeverOffersABoss(t *testing.T) {
	t.Parallel()

	for _, night := range []bool{false, true} {
		offered := spawnableSpecies(night)
		if len(offered) == 0 {
			t.Fatalf("night=%v offers no species at all, so this asked nothing", night)
		}
		for _, kind := range offered {
			if mobRegistry[kind].isBoss() {
				t.Errorf("night=%v offered boss-rank %s to the director", night, kind)
			}
		}
	}

	// Named as well as swept, because the sweep above is equally true of a registry that
	// never gained a boss at all.
	for _, kind := range []vnet.MobKind{vnet.MobKindVargrGuardian, vnet.MobKindDraugrKing} {
		if contains(spawnableSpecies(false), kind) || contains(spawnableSpecies(true), kind) {
			t.Errorf("%s is offered by the open-world director", kind)
		}
	}
}

// And it never takes one away either.
//
// The three rules in [Sim.removeSpentMobsLocked] are one sentence in three shapes — this
// creature is no longer worth simulating, give the budget back — and a boss is worth
// simulating for as long as its session exists. The distance rule is the dangerous one:
// a party that wipes and walks out has *not* finished with the encounter, and five
// seconds later the sweep would have deleted it with nothing anywhere saying so.
//
// A real boss species rather than the promoted draugr of boss_encounter_test.go, because
// the exemption is a property of the registry row and this is the file that owns those.
func TestTheDirectorNeverTakesABossAway(t *testing.T) {
	t.Parallel()

	for _, kind := range []vnet.MobKind{vnet.MobKindVargrGuardian, vnet.MobKindDraugrKing} {
		h := newVitalsHarnessAt(t, DefaultTickRate, dropTerrain{groundTop: 63}, 1)
		player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
		// Far enough away that no streamed cube holds it: the distance rule's own input.
		id := h.placeSpeciesAt(kind, [3]float64{500.5, 64, 500.5})
		h.sim.mu.Lock()
		h.sim.startBossEncounterLocked(h.sim.mobs[id], player)
		h.sim.mu.Unlock()

		// Past the dawn and past the despawn grace, together.
		h.advance(int(h.sim.mobDespawnTicks) + 2)

		h.sim.mu.Lock()
		m, alive := h.sim.mobs[id]
		_, corpse := h.sim.corpses[id]
		h.sim.mu.Unlock()
		if !alive {
			t.Errorf("the director took the %s away after %d unwatched ticks", kind, h.sim.mobDespawnTicks+2)
			continue
		}
		if corpse {
			t.Errorf("the %s left a corpse without anybody killing it", kind)
		}
		if m.encounter == nil {
			t.Errorf("the %s kept its body and lost the roster frozen at the pull", kind)
		}
	}
}

// Killing one takes the personal-loot path, which is loot.go's boss branch executing for
// the first time against a species that really is one.
//
// **Nothing in loot.go changed for this**, and that is what the test is for: the branch,
// the one-roll-per-roster-member loop and the comment warning that a boss must never take
// the round-robin path have all been there since #327. This is their first caller.
//
// The damage is applied directly rather than swung, for the reason withTestBoss exists:
// eighteen and thirty blows of the starter blade would be measuring the balance. What is
// under test is which corpse shape a boss death produces.
func TestKillingABossSpeciesTakesThePersonalLootPath(t *testing.T) {
	t.Parallel()

	for _, kind := range []vnet.MobKind{vnet.MobKindVargrGuardian, vnet.MobKindDraugrKing} {
		h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
		player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
		id := h.placeSpeciesAt(kind, [3]float64{0.5, 64, -3.5})

		h.sim.mu.Lock()
		m := h.sim.mobs[id]
		h.sim.startBossEncounterLocked(m, player)
		roster := append([]corpseOwner(nil), m.encounter.roster...)
		h.sim.damageMobLocked(m, m.health)
		c := h.sim.corpses[id]
		h.sim.mu.Unlock()

		if c == nil {
			t.Errorf("killing the %s left no corpse", kind)
			continue
		}
		if c.container.entries != nil || c.container.silver != 0 {
			t.Errorf("the %s filled the shared round-robin container %+v", kind, c.container)
		}
		if len(roster) == 0 {
			t.Fatalf("the %s pull froze an empty roster", kind)
		}
		if len(c.personal) != len(roster) {
			t.Errorf("the %s left %d personal containers for a roster of %d",
				kind, len(c.personal), len(roster))
		}
		for _, owner := range roster {
			container := c.personal[owner]
			if container == nil || len(container.entries) == 0 {
				t.Errorf("roster member %+v got %+v from the %s", owner, container, kind)
			}
		}
	}
}
