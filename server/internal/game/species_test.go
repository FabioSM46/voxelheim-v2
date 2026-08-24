package game

import (
	"reflect"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// The table every other test in this package now reads its creature numbers from.
//
// Two kinds of test live here and they are deliberately different. The sweep asks a
// question of *every* row and keeps being true when a third species is added; the
// pinning test restates one species' numbers in full and is supposed to fail the moment
// anybody changes them.

// draugrRow and vargrRow are the registry rows the rest of the package's tests read.
//
// **Read from the table rather than restated**, deliberately: a test carrying its own
// copy of the draugr's health would keep passing after a balance pass moved it, which is
// the failure a registry exists to end. The one place the numbers are written down twice
// is TestTheDraugrsNumbersSurvivedTheMoveIntoTheRegistry, whose entire job is to
// disagree with the table.
var (
	draugrRow = mobRegistry[vnet.MobKindDraugr]
	vargrRow  = mobRegistry[vnet.MobKindVargr]
	deerRow   = mobRegistry[vnet.MobKindDeer]
)

// ---------------------------------------------------------------------------
// Every row is a whole creature
// ---------------------------------------------------------------------------

// No zero stands in for a real number anywhere in the registry.
//
// The sweep itemRegistry's tests perform, with the opposite default: there is no field
// here whose zero is a documented meaning. A creature with no health is already dead,
// one with no speed cannot chase, one with no aggro range never notices anybody, one
// with no windup swings without a telegraph and one with no body occupies nothing at
// all. `nocturnal` is the single exception and is excluded by name below, because its
// false is a species that hunts by day rather than a field somebody forgot.
//
// The loot table is swept on the same terms: an empty one is refused, every line names a
// registered item, and no line can leave more of one than a stack holds — the last of
// which is not pedantry, because a drop above the stack limit is one the merge can never
// fold and a pickup can never take in a single insertion.
func TestEverySpeciesIsFullyDescribed(t *testing.T) {
	t.Parallel()

	if len(mobRegistry) == 0 {
		t.Fatal("the registry is empty, so this sweep asked nothing")
	}
	if _, registered := mobByKind(vnet.MobKindUnknown); registered {
		t.Error("the wire's fail-closed zero kind is registered as a real species")
	}

	for kind, def := range mobRegistry {
		if kind == vnet.MobKindUnknown {
			continue
		}
		if def.maxHealth == 0 {
			t.Errorf("%s has no health, so it arrives dead", kind)
		}
		if def.speed <= 0 {
			t.Errorf("%s has a speed of %v, so it can never close on anybody", kind, def.speed)
		}
		if def.aggroRange <= 0 {
			t.Errorf("%s has an aggro range of %v, so it never notices a player", kind, def.aggroRange)
		}
		if def.passive {
			if def.attackRange != 0 || def.damage != 0 || def.windup != 0 || def.recovery != 0 {
				t.Errorf("passive %s carries an attack: range=%v damage=%d windup=%v recovery=%v",
					kind, def.attackRange, def.damage, def.windup, def.recovery)
			}
		} else {
			if def.attackRange <= 0 {
				t.Errorf("hostile %s has an attack range of %v", kind, def.attackRange)
			}
			if def.damage == 0 {
				t.Errorf("hostile %s does no damage", kind)
			}
			if def.windup <= 0 {
				t.Errorf("hostile %s has a windup of %v", kind, def.windup)
			}
			if def.recovery <= 0 {
				t.Errorf("hostile %s has a recovery of %v", kind, def.recovery)
			}
		}
		if def.body.width <= 0 || def.body.height <= 0 {
			t.Errorf("%s has a body of %v by %v, which occupies nothing", kind, def.body.width, def.body.height)
		}

		// The loot table is held to the same rule as the numbers above: an empty one is a
		// creature that is not worth killing, which is a decision rather than a default.
		// See mobDefinition's doc, and the user story loot exists for.
		if len(def.loot) == 0 {
			t.Errorf("%s leaves nothing behind, so hunting one is a pure loss", kind)
		}
		for i, roll := range def.loot {
			if _, registered := itemByID(roll.item); !registered {
				t.Errorf("%s loot line %d leaves item %d, which no registry entry describes",
					kind, i, roll.item)
			}
			if roll.min == 0 {
				t.Errorf("%s loot line %d can leave nothing; a chance below certainty is not a thing this table can express",
					kind, i)
			}
			if roll.max < roll.min {
				t.Errorf("%s loot line %d rolls %d..%d, which is an empty range", kind, i, roll.min, roll.max)
			}
			if limit := stackLimit(roll.item); roll.max > limit {
				t.Errorf("%s loot line %d can leave %d of item %d, which stacks to %d",
					kind, i, roll.max, roll.item, limit)
			}
		}

		// The relationships each row has to hold inside itself, rather than the values.
		if !def.passive && def.attackRange > def.aggroRange {
			t.Errorf("%s reaches %v blocks and notices you at %v: it could swing at somebody it has not seen",
				kind, def.attackRange, def.aggroRange)
		}
		if !def.passive && def.attackRange >= SwordReach {
			t.Errorf("%s reaches %v blocks against a sword's %v, so keeping your distance buys nothing",
				kind, def.attackRange, SwordReach)
		}
		if !def.passive && def.damage >= PlayerMaxHealth {
			t.Errorf("%s takes %d of a level-one player's %d health in one blow", kind, def.damage, PlayerMaxHealth)
		}
	}
}

func TestTheDeerRowIsPassivePrey(t *testing.T) {
	t.Parallel()

	want := mobDefinition{
		maxHealth:  20,
		speed:      4.0,
		aggroRange: 12.0,
		passive:    true,
		body:       body{width: 0.9, height: 1.4},
		loot:       []lootRoll{{item: ItemRawMeat, min: 1, max: 2}},
	}
	if !reflect.DeepEqual(deerRow, want) {
		t.Errorf("the deer's row is %+v, want %+v", deerRow, want)
	}
}

// Every kind the wire can carry is either registered or the fail-closed zero.
//
// The enum is the contract and the registry is what this server can actually make, so a
// member of one and not the other is a creature the schema promises and nothing can
// produce. It is checked from the generated names rather than from a list here, which is
// what makes the next `MobKind` fail this test instead of quietly existing.
func TestEveryWireKindIsARegisteredSpecies(t *testing.T) {
	t.Parallel()

	for kind := range vnet.EnumNamesMobKind {
		_, registered := mobByKind(kind)
		if kind == vnet.MobKindUnknown {
			if registered {
				t.Error("MobKind.Unknown is registered, and it is the value an absent field decodes to")
			}
			continue
		}
		if !registered {
			t.Errorf("the wire can carry %s and this server has no row for it", kind)
		}
	}
}

// ---------------------------------------------------------------------------
// The move itself
// ---------------------------------------------------------------------------

// The draugr's numbers are exactly the ones it had as constants.
//
// **A pinning test, and the duplication is the whole point**: everything about the
// draugr used to be a `Draugr*` constant in constants.go, and a reader asked to believe
// that moving them into a table changed nothing would otherwise have to diff two files.
// This restates them, so the claim is executed rather than asserted in prose. It is
// supposed to fail when somebody rebalances the draugr — that is a decision, and it
// should have to be made here as well as there.
func TestTheDraugrsNumbersSurvivedTheMoveIntoTheRegistry(t *testing.T) {
	t.Parallel()

	want := mobDefinition{
		maxHealth:   60,
		speed:       3.2,
		aggroRange:  16.0,
		attackRange: 2.0,
		damage:      10,
		windup:      600 * time.Millisecond,
		recovery:    900 * time.Millisecond,
		body:        body{width: 0.6, height: 1.8},
		nocturnal:   true,
		// Not one of the constants this test is named for — a draugr left nothing behind
		// until loot existed — but part of the row now, and pinned for exactly the reason
		// the rest of it is: what a creature is worth killing is a balance decision, and
		// it should have to be made here as well as in the table.
		loot: []lootRoll{{item: ItemBone, min: 1, max: 2}},
	}
	// DeepEqual rather than `!=`, because a row carries a slice and slices are not
	// comparable. The whole-row comparison is what gives this test its teeth — a field
	// added to mobDefinition and left out of `want` is a compile error here — so it is
	// kept rather than replaced by a field-by-field check that a new field could slip past.
	if !reflect.DeepEqual(draugrRow, want) {
		t.Errorf("the draugr's row is %+v, want the constants it replaced: %+v", draugrRow, want)
	}
	// And the body it used to have was the player's, stated in full rather than derived
	// from theirs — so narrowing a corridor for players does not narrow the thing that
	// hunts them down it. The equality is what the old constants said; the separate
	// declaration is what lets it stop being true on purpose.
	if draugrRow.body != (body{width: PlayerWidth, height: PlayerHeight}) {
		t.Errorf("the draugr's body is %v, and it was the player's %v by %v",
			draugrRow.body, PlayerWidth, PlayerHeight)
	}
}

// The vargr is the creature the issue asked for: faster than a walk, and cheaper to
// put down.
//
// The speed is the one number the whole species is arranged around, so it is asserted
// against [WalkSpeed] rather than against itself: what matters is not that it is 5.4 but
// that a player at full intent cannot leave.
func TestAVargrIsFasterThanAWalkingPlayerAndDiesQuicker(t *testing.T) {
	t.Parallel()

	if vargrRow.speed <= WalkSpeed {
		t.Errorf("a vargr closes at %v against a walk of %v: you could simply leave",
			vargrRow.speed, WalkSpeed)
	}
	if draugrRow.speed >= WalkSpeed {
		t.Errorf("a draugr closes at %v against a walk of %v, so running is no longer an answer to it",
			draugrRow.speed, WalkSpeed)
	}
	if draugrRow.speed >= WalkSpeed*StarvingSpeedScale {
		t.Errorf("a draugr closes at %v against a starving walk of %v, so zero hunger makes escape impossible",
			draugrRow.speed, WalkSpeed*StarvingSpeedScale)
	}
	if vargrRow.nocturnal {
		t.Error("the vargr is nocturnal, and the dark is not supposed to be what brings it out")
	}
	if !draugrRow.nocturnal {
		t.Error("the draugr is not nocturnal, so nothing in this world leaves with the night")
	}

	// One iron swing fewer than a draugr, which is what the health buys back for the
	// speed. Stated as the arithmetic rather than as "1 and 2", so it keeps meaning the
	// same thing after a blade is rebalanced.
	swings := func(health, damage uint16) int { return int((health + damage - 1) / damage) }
	vargrSwings := swings(vargrRow.maxHealth, IronSwordDamage)
	draugrSwings := swings(draugrRow.maxHealth, IronSwordDamage)
	if vargrSwings != draugrSwings-1 {
		t.Errorf("an iron sword needs %d swings for a vargr and %d for a draugr, want exactly one fewer",
			vargrSwings, draugrSwings)
	}
	if vargrRow.damage >= draugrRow.damage {
		t.Errorf("a vargr hits for %d against a draugr's %d: the speed is supposed to cost it something",
			vargrRow.damage, draugrRow.damage)
	}
}

// ---------------------------------------------------------------------------
// Who may arrive, and when
// ---------------------------------------------------------------------------

// The registry answers "which species may spawn now", and it answers it twice a day.
func TestTheRegistryDecidesWhichSpeciesMaySpawnWhen(t *testing.T) {
	t.Parallel()

	byDay := spawnableSpecies(false)
	byNight := spawnableSpecies(true)

	for _, kind := range byDay {
		if mobRegistry[kind].nocturnal {
			t.Errorf("%s is nocturnal and the daylight offered it anyway", kind)
		}
	}
	for kind, def := range mobRegistry {
		if !contains(byNight, kind) {
			t.Errorf("%s may never arrive at night, so nothing can ever make one", kind)
		}
		if !def.nocturnal && !contains(byDay, kind) {
			t.Errorf("%s is not nocturnal and the daylight refused it", kind)
		}
	}

	// The two species this issue registers, named — the sweep above is true of an empty
	// registry too.
	if !contains(byNight, vnet.MobKindDraugr) || contains(byDay, vnet.MobKindDraugr) {
		t.Errorf("the draugr is offered by night=%v and by day=%v, want night only",
			contains(byNight, vnet.MobKindDraugr), contains(byDay, vnet.MobKindDraugr))
	}
	if !contains(byNight, vnet.MobKindVargr) || !contains(byDay, vnet.MobKindVargr) {
		t.Error("the vargr is not offered at every hour, and nothing about it is nocturnal")
	}

	// Sorted, which is what keeps the caller's draw reproducible: map order is
	// deliberately random, and an unsorted slice would place different creatures on
	// different runs of the same world.
	for _, list := range [][]vnet.MobKind{byDay, byNight} {
		for i := 1; i < len(list); i++ {
			if list[i-1] >= list[i] {
				t.Errorf("%v is not in ascending kind order", list)
				break
			}
		}
	}
}

func contains(kinds []vnet.MobKind, want vnet.MobKind) bool {
	for _, kind := range kinds {
		if kind == want {
			return true
		}
	}
	return false
}

// ---------------------------------------------------------------------------
// Timings
// ---------------------------------------------------------------------------

// Every species' telegraph survives every tick rate the server accepts.
//
// The rule ticksFor exists for, asked of the whole table: a rate that rounded a windup
// away would make that attack unreactable rather than fast, and the vargr's 400 ms is
// the shortest one in the game.
func TestEverySpeciesTelegraphSurvivesEveryTickRate(t *testing.T) {
	t.Parallel()

	for _, rate := range []uint8{1, 2, 5, 20, 60, 255} {
		timings := mobTimingsFor(rate)
		if len(timings) != len(mobRegistry) {
			t.Errorf("%d Hz converted %d of %d species", rate, len(timings), len(mobRegistry))
		}
		for kind, def := range mobRegistry {
			got := timings[kind]
			if def.passive {
				if got != (mobTicks{}) {
					t.Errorf("passive %s has attack timings %+v at %d Hz", kind, got, rate)
				}
				continue
			}
			if got.windup == 0 {
				t.Errorf("%s has a windup of no ticks at %d Hz", kind, rate)
			}
			if got.recovery == 0 {
				t.Errorf("%s has a recovery of no ticks at %d Hz", kind, rate)
			}
			if want := ticksFor(def.windup, rate); got.windup != want {
				t.Errorf("%s winds up for %d ticks at %d Hz, want %d", kind, got.windup, rate, want)
			}
			if want := ticksFor(def.recovery, rate); got.recovery != want {
				t.Errorf("%s recovers for %d ticks at %d Hz, want %d", kind, got.recovery, rate, want)
			}
		}
	}
}

// ---------------------------------------------------------------------------
// Nothing may be created that the registry has never heard of
// ---------------------------------------------------------------------------

// An unregistered kind is refused rather than given default numbers.
//
// Fail-closed for the reason the wire's zero MobKind is: a creature nobody described is
// not a draugr with a strange name, it is a creature whose body would be zero blocks
// wide, whose aggro range would be nothing and which nothing could ever kill because it
// would arrive with no health.
func TestAnUnregisteredSpeciesCannotBeCreated(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})

	h.sim.mu.Lock()
	unknown, made := h.sim.spawnMobLocked(vnet.MobKindUnknown, [3]float64{0.5, 64, 0.5})
	stranger, alsoMade := h.sim.spawnMobLocked(vnet.MobKind(200), [3]float64{1.5, 64, 0.5})
	population := len(h.sim.mobs)
	h.sim.mu.Unlock()

	if made || unknown != 0 {
		t.Errorf("the fail-closed zero kind produced creature %d", unknown)
	}
	if alsoMade || stranger != 0 {
		t.Errorf("an unregistered kind produced creature %d", stranger)
	}
	if population != 0 {
		t.Errorf("the world holds %d creatures after two refused spawns", population)
	}
}
