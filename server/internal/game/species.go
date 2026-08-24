package game

import (
	"slices"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// What each species *is*, and the only copy of it.
//
// mob.go is what a creature does; spawn.go is what puts one in the world and takes it
// away again. This file is the table both of them read, keyed by the same [vnet.MobKind]
// that crosses the wire.
//
// **It is the move itemRegistry made, for the same reason.** "How big is its body",
// "how far does it notice you" and "does it survive the sun" are now lookups keyed by a
// kind rather than comparisons against one, so the third species is a row here instead
// of an edit to combat, to the collision, to the snapshot and to the director. The
// concrete bug that shape avoids was already in the code: swingTargetLocked built
// draugrBody.boxAt(m.pos) for every mob it considered, which would have given a vargr
// the reach of a draugr the moment a second species existed.
//
// **Every species shares one state machine, and disposition is a column in this table.**
// Hostile rows choose a target and attack it; passive rows choose a threat and flee. The
// movement, collision, death and loot paths remain one of each rather than one copy per
// creature.

// mobDefinition is the server-only rule for one species.
//
// Nothing here is sent to a client. A snapshot carries the kind, the position, the
// health and the action; how fast the thing walks and how far it can reach are the
// server's answers, and a client that disagreed about either would gain nothing.
//
// **Every common field below is a real number, while attack fields are conditional.**
// A hostile row needs a complete attack; a passive row must set every attack field to
// zero because it never enters those states. Health, speed, awareness, body and loot are
// required for both. `passive` and `nocturnal` are positive boolean statements.
// TestEverySpeciesIsFullyDescribed is what holds that line rather than this paragraph.
//
// **loot is held to the same rule, which is why an empty table is not allowed either.**
// The user story this world is built on is that hunting has to be worth the durability it
// costs, so a species that leaves nothing is a decision somebody would have to argue for
// rather than a field they can forget. The sweep refuses the empty slice for exactly that
// reason: today the answer is "no such species", and the day there is one, this is where
// it gets written down.
type mobDefinition struct {
	// maxHealth is what one arrives with, and the denominator of the health a snapshot
	// carries for it.
	maxHealth uint16

	// experience is the lifetime progress one kill is worth. It is required even for
	// passive prey: hunting a deer still spends time and durability, and a zero here
	// would make forgetting the reward indistinguishable from choosing none.
	experience uint16

	// speed is how fast it closes on a target, in blocks per second. Read against
	// [WalkSpeed], which is what decides whether running away is an answer.
	speed float64

	// aggroRange is how far off it notices a player, in blocks measured between bodies.
	//
	// **[MobSpawnRingInner] must stay above the widest of these**, or a creature arrives
	// already hunting — which reads as the server cheating rather than as the dark being
	// dangerous. TestTheSpawnGeometryHangsTogether asks the whole registry, not one row.
	aggroRange float64

	// passive selects the non-attacking branch of the shared state machine. Its zero is
	// deliberately hostile, so an old row does not silently stop fighting when this
	// column is added.
	passive bool

	// attackRange is how close it has to be to swing, in blocks between bodies. Under
	// [SwordReach], so a player who holds the edge of their own reach is not trading
	// blows evenly.
	attackRange float64

	// damage is what one landed blow costs a player, measured against the
	// [PlayerMaxHealth] level-one maximum.
	damage uint16

	// windup is the telegraph: the swing is committed and has not landed yet. It is what
	// makes an attack something a player can react to rather than something that simply
	// happens.
	//
	// recovery is how long after a swing before another may begin, and every attack pays
	// it whether or not it landed — which is what makes attack cadence the server's
	// answer instead of a consequence of the tick rate.
	//
	// Durations rather than tick counts, converted per server by [mobTimingsFor]: six
	// hundred milliseconds is six hundred milliseconds on a 5 Hz server and on a 60 Hz
	// one, or it is not a telegraph.
	windup   time.Duration
	recovery time.Duration

	// body is the box this species occupies, and the only statement of it.
	//
	// **Read by the collision, by the swing that reaches it, by the separation the
	// director keeps between spawns and by the step it may hop.** A second box spelled
	// anywhere else is the bug this field exists to make impossible: it would agree with
	// this one for exactly as long as nobody rebalanced either.
	body body

	// nocturnal says the dark is the only thing that brings this species out.
	//
	// **A property of the creature, not of the spawn rule**, which is why the director
	// asks the registry which species may arrive rather than checking the clock and then
	// naming one. The same sentence has to be true from both ends: a nocturnal creature
	// arrives only at night and does not outlast the night either, and what survives the
	// dawn is one that is already hunting somebody — for exactly as long as that hunt
	// lasts. A species that is not nocturnal arrives at any hour and the dawn is nothing
	// to it.
	//
	// The false here is a real answer rather than an unset field; see the type's doc.
	nocturnal bool

	// loot is what a *kill* leaves on the ground, one line per item, rolled from the
	// simulation's own generator — see loot.go, which owns the roll and the spawn.
	//
	// **A field of the row rather than a table of its own**, for the reason every number
	// above is one: what a creature is worth killing for belongs beside what it costs to
	// kill, and a second map keyed by [vnet.MobKind] would be a second place a third
	// species has to be remembered in.
	//
	// **A kill, and nothing else.** A mob the director takes away — at dawn, or because
	// nobody has been near it for five seconds — leaves nothing, and the two removals in
	// spawn.go say so by not asking this field. Loot is the reward for the kill; a world
	// that paid it out for having existed would be a world where waiting is a strategy.
	loot []lootRoll
}

// mobRegistry is every species this world can hold, and the only place their numbers
// live.
//
// Deliberately not sent to clients, exactly as itemRegistry is not: a client renders
// what a snapshot says and may have an opinion about how to draw it, but only this table
// decides how far a creature reaches or how much of it there is to hit.
var mobRegistry = map[vnet.MobKind]mobDefinition{
	// The draugr — the first thing in this world that was not scenery, and still the one
	// the numbers below are read against.
	//
	// Its 60 health is the scale the blades are balanced on: three rusty swings kill one
	// and two iron ones do (see itemRegistry). Its speed is deliberately under
	// [WalkSpeed], so a player who turns and runs can leave — close enough that walking
	// away is a decision rather than a formality. Its box is the player's dimensions,
	// because a draugr is a humanoid corpse, but stated here in full rather than written
	// as PlayerWidth and PlayerHeight: narrowing a corridor for players must not silently
	// narrow the thing that hunts them down it. Fifteen experience is the baseline reward
	// for that 60-health fight: meaningful progress, but deliberately short of a level
	// after three kills.
	vnet.MobKindDraugr: {
		maxHealth:   60,
		experience:  15,
		speed:       3.2,
		aggroRange:  16.0,
		attackRange: 2.0,
		damage:      10,
		windup:      600 * time.Millisecond,
		recovery:    900 * time.Millisecond,
		body:        body{width: 0.6, height: 1.8},
		nocturnal:   true,
		// One or two bones, which is the only variable count in the game and is variable
		// on purpose: a draugr is the creature a player kills most, and a fixed yield
		// would make a night's hunting arithmetic rather than a run of luck. Nothing
		// consumes bones yet — see ItemBone, where that is a decision rather than an
		// omission.
		loot: []lootRoll{{item: ItemBone, min: 1, max: 2}},
	},

	// The vargr — something faster than you.
	//
	// **Its speed is above [WalkSpeed] of 4.3, and that is the whole point of it: you do
	// not outrun a vargr, you turn and fight it.** Everything else about the row pays for
	// that one number. It has 35 health against the draugr's 60, so it dies in one iron
	// swing where a draugr takes two and in two rusty ones where a draugr takes three;
	// its blow costs 7 rather than 10; and its windup is 400 ms rather than 600, which is
	// a shorter telegraph and therefore the harder one to read. It notices you from 20
	// blocks rather than 16 — being seen is what starts the chase, and a creature you
	// cannot outrun that has to find you first is a different threat from one that
	// cannot be escaped once it exists.
	//
	// It is **not** nocturnal, which is the second half of the same design: the draugr
	// is what the night brings and the vargr is what the daylight does not save you from.
	//
	// Its body is wider and much shorter than a draugr's — a beast on four legs rather
	// than a corpse on two — and the width is what makes the box worth reading from this
	// table rather than assuming: at the same standing distance a vargr is inside a
	// sword's reach where a draugr is not, because a swing is measured body to body.
	// Twenty experience prices that speed above the draugr's fifteen even though the
	// vargr has less health: the fight is worth more because walking away is not an answer.
	vnet.MobKindVargr: {
		maxHealth:   35,
		experience:  20,
		speed:       5.4,
		aggroRange:  20.0,
		attackRange: 1.8,
		damage:      7,
		windup:      400 * time.Millisecond,
		recovery:    700 * time.Millisecond,
		body:        body{width: 0.9, height: 1.0},
		nocturnal:   false,
		// Exactly one pelt, and the fixed count is the balance rather than a
		// simplification: two pelts make one patch, so a vargr is half a repair and
		// killing two is a decision a player can make on purpose. A variable yield would
		// put that between them and the arithmetic.
		loot: []lootRoll{{item: ItemVargrPelt, min: 1, max: 1}},
	},

	// The deer is prey rather than an enemy. Its aggro range is awareness: inside it a
	// live player makes the deer flee, and the wider release radius in mob.go prevents a
	// body standing on the boundary from switching state every tick. Five experience is
	// the low end of the hunt: only 20 health, below both predators, but its speed still
	// makes bringing down food an active pursuit rather than free progress.
	vnet.MobKindDeer: {
		maxHealth:  20,
		experience: 5,
		speed:      4.0,
		aggroRange: 12.0,
		passive:    true,
		body:       body{width: 0.9, height: 1.4},
		nocturnal:  false,
		loot:       []lootRoll{{item: ItemRawMeat, min: 1, max: 2}},
	},
}

// mobByKind is one species' row, and whether the kind is one this server knows.
//
// The shape itemByID has, and it fails closed the same way: a kind nobody registered is
// not a creature with default numbers, it is a creature that cannot be made. See
// [Sim.spawnMobLocked], which is the one place that answer is acted on.
func mobByKind(kind vnet.MobKind) (mobDefinition, bool) {
	def, ok := mobRegistry[kind]
	return def, ok
}

// species is this mob's registry row.
//
// Total rather than two-valued, and the spawn path is what makes it so: the only way a
// mob enters the world refuses a kind the registry does not hold, so every mob in
// Sim.mobs has a row waiting for it here.
//
// A lookup rather than a copy taken at creation, for the reason an itemDrop stores an
// ItemID rather than an itemDefinition: the table is the truth, and a copy is a second
// one that a balance pass would have to know to go and find.
func (m *mob) species() mobDefinition { return mobRegistry[m.kind] }

// spawnableSpecies is every kind that may arrive right now, in kind order.
//
// **This is the registry answering "who may spawn at this hour", and it is deliberately
// not the director asking "is it night" and then naming a draugr.** The clock question
// has one answer — [IsNight] — and the species question is a property of each row, so
// the two are composed here once instead of being spelled as a branch at the spawn site
// that a third species would have to be added to.
//
// Sorted, because the caller draws from the result with the simulation's own generator
// and map iteration order is deliberately random: an unsorted slice would make the same
// world place different creatures on different runs, which is exactly the property
// spawn_test.go pins.
func spawnableSpecies(night bool) []vnet.MobKind {
	kinds := make([]vnet.MobKind, 0, len(mobRegistry))
	for kind, def := range mobRegistry {
		if def.nocturnal && !night {
			continue
		}
		kinds = append(kinds, kind)
	}
	slices.Sort(kinds)
	return kinds
}

// mobTicks is one species' two durations in the ticks Step counts.
type mobTicks struct {
	windup   uint32
	recovery uint32
}

// mobTimingsFor converts every registered species' telegraph and recovery at this
// server's tick rate.
//
// Converted once, at construction, beside every other duration [NewSim] turns into
// ticks — and per species rather than per simulation, because the two numbers stopped
// being the draugr's the moment a second row had different ones. [ticksFor] is what
// keeps a fast telegraph from rounding away to nothing at a coarse rate.
func mobTimingsFor(tickRate uint8) map[vnet.MobKind]mobTicks {
	timings := make(map[vnet.MobKind]mobTicks, len(mobRegistry))
	for kind, def := range mobRegistry {
		if def.passive {
			timings[kind] = mobTicks{}
			continue
		}
		timings[kind] = mobTicks{
			windup:   ticksFor(def.windup, tickRate),
			recovery: ticksFor(def.recovery, tickRate),
		}
	}
	return timings
}
