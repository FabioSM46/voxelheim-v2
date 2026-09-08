package game

import (
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// What a boss is *able* to do, and the only copy of it.
//
// species.go is what a creature is; encounter.go is the stage it has reached and the
// publication of what it has announced; this file is the repertoire the scheduler in
// encounter_execution.go draws from. The split is the registry's own: a row here is a
// move, and adding one is a row rather than a branch in the executor.
//
// **Nothing here is sent to a client.** The wire carries the announcement — a kind, a
// phase, the ticks it lasts and the region it endangers — and never the table that
// produced it. A client that held these numbers could compute the next move; a client
// that holds the announcement can only read the one it has been shown, which is the
// whole of the design's readability claim.
//
// **A species with no rows here fights with the shared state machine in mob.go.** That
// is a real answer rather than a gap: the Draugr king's casts, channels, pulses and
// projectile are the second half of #1024 and land with their own execution surface, and
// until they do the king keeps the ordinary telegraph-and-swing it has always had rather
// than standing in its hall unable to hurt anybody.

// encounterTravel is how a move moves the creature performing it.
//
// **Movement during a move is a property of the move, not of the pursuit**, which is
// what keeps the two apart: a charge is a committed displacement along a lane fixed
// before it started, and the ordinary steering that closes distance is not running while
// one is under way.
type encounterTravel uint8

const (
	// travelNone plants the creature for the whole move. Every bite and sweep.
	travelNone encounterTravel = iota

	// travelCharge runs the announced lane and is stopped by terrain, which is the
	// approved design's monolith: the impact ends the run early and buys the longer
	// recovery below.
	travelCharge

	// travelLeap crosses to the announced landing region. The region is fixed before the
	// jump and never follows its target, which is why the damage is resolved at the
	// landing rather than along the arc.
	travelLeap
)

// encounterHazard is the shape one move announces, before it is placed in the world.
//
// A template rather than a [protocol.HazardVolume]: the origin and direction are decided
// when the move is chosen, from where the creature is standing and where it locked its
// aim, and everything that is a property of the *move* rather than of the moment lives
// here.
type encounterHazard struct {
	shape vnet.HazardShape

	// reach is the cone's length or the disc's radius, in blocks. Zero for a lane, whose
	// radius is what its travel can actually cover — see [encounterMoveDef.laneLength]
	// and [encounterMoveDef.announcedRadius].
	reach float64

	// height is the vertical extent, in blocks, centred on the origin exactly as the
	// contract states it. Read against the species' own body: a region shorter than the
	// creature performing it would be announcing less than it does.
	height float64

	// halfAngle is half the cone's opening, in radians. Cone only.
	halfAngle float64

	// halfWidth is half the lane's width, in blocks. Line only.
	halfWidth float64
}

// encounterMoveDef is one move in one species' repertoire.
//
// **Durations rather than tick counts**, converted per server by
// [encounterMoveTimingsFor] for [mobDefinition]'s reason: nine hundred milliseconds of
// telegraph is nine hundred milliseconds at 5 Hz and at 60 Hz, or it is not a telegraph.
//
// **Damage is a percentage of the registry's own blow rather than a number.** What a
// species' blow costs is `mobDefinition.damage` and stays there, where #1036 owns it; a
// move says how much heavier or lighter than that ordinary blow it is. A second absolute
// number here would be a second place a balance pass has to find.
type encounterMoveDef struct {
	kind vnet.EncounterMoveKind

	// fromStage is the encounter stage that unlocks this move, counted from one exactly
	// as `EncounterTimeline.phase` is. A stage never falls, so a move never leaves the
	// repertoire once it has been shown.
	fromStage uint8

	// telegraph, release and recovery are the three phases this half of the encounter
	// uses. `MovePhase.Channel` belongs to the king's casts and is deliberately absent:
	// a Vargr's confirmed design is physical throughout, and a channel it could never
	// enter would be a phase nothing produces.
	//
	// telegraph is the promise the whole contract rests on — the region is announced and
	// nothing in it is dangerous yet. release is the damaging window, counted in the same
	// ticks the client was told about. recovery is the opening reading it correctly buys.
	telegraph time.Duration
	release   time.Duration
	recovery  time.Duration

	// impactRecovery replaces recovery when a charge is stopped by terrain, and is zero
	// for every move that cannot be. The approved design's longer opening after the beast
	// hits a monolith, stated as a duration the server owns rather than as an animation.
	impactRecovery time.Duration

	// cooldown is how long after this move ends before it may be chosen again. It is what
	// makes the heavy moves rare; which of the *available* moves is taken is the
	// least-recently-used rule in encounter_execution.go.
	cooldown time.Duration

	// minRange and maxRange are the body-to-body band, in blocks, in which this move may
	// be chosen. maxRange must stay inside [encounterMoveDef.selectionReach], or the
	// creature would announce a region its target is not in — the registry sweep in
	// encounter_execution_test.go is what holds that rather than this sentence.
	minRange, maxRange float64

	// damagePercent is this move's blow as a percentage of the species' registry damage.
	// One hundred is exactly the ordinary blow.
	damagePercent uint16

	// travelSpeed is how fast the creature crosses its lane during release, in blocks per
	// second, and is zero for a move that does not travel.
	travelSpeed float64

	travel encounterTravel

	hazard encounterHazard
}

// laneLength is how far this move's travel carries the creature during its release.
//
// **The announced lane is exactly what the release can cover**, which is what makes "no
// weapon deals damage outside its announced volume" true by construction for a charge:
// the swept segment the executor tests is a prefix of this length, always, because it is
// produced by the same two numbers.
func (d encounterMoveDef) laneLength() float64 {
	return d.travelSpeed * d.release.Seconds()
}

// selectionReach is how far from the creature this move can matter, in blocks.
//
// **The question the range band is checked against**, and it is not the same question as
// the radius below. A move that travels can matter as far as it travels — a charge's lane
// runs that far, a leap's landing is placed that far away — while a move that plants the
// creature can only matter as far as its own region reaches.
func (d encounterMoveDef) selectionReach() float64 {
	if d.travel != travelNone {
		return d.laneLength()
	}
	return d.hazard.reach
}

// announcedRadius is the outer extent of the region this move announces, in blocks.
//
// **A lane is the one shape whose radius is its travel**, because the strip runs from the
// creature to wherever the run can reach. Every other shape carries its own reach, and a
// leap is the case that makes the distinction load-bearing: its landing disc is three
// blocks across and is *placed* seven away, so a radius taken from the travel would
// announce a disc covering the whole jump — a region far larger than the one the design
// asks a player to leave.
func (d encounterMoveDef) announcedRadius() float64 {
	if d.hazard.shape == vnet.HazardShapeLine {
		return d.laneLength()
	}
	return d.hazard.reach
}

// encounterMoveCatalog is every boss species' repertoire, in the order the least-recently
// used rule breaks its ties.
//
// Playtest values from the approved design, not final balance. The telegraphs sit inside
// its 0.9–1.5 second band, the recoveries inside its 1.2–2 second one, and the charge's
// impact recovery is the 2.5 seconds it names for the monolith.
var encounterMoveCatalog = map[vnet.MobKind][]encounterMoveDef{
	// The Vargr that guarded the tomb. Physical throughout — scratches, bites, a charge
	// and a leap — so not one of these rows is a cast and none may ever carry a channel.
	//
	// Stage 1 teaches the four answers: leave the frontal cone, step out of the lane,
	// leave the landing region, leave the arc of the sweep. Stage 2 adds the jaws, which
	// is the same answer read faster against a much heavier blow.
	vnet.MobKindVargrGuardian: {
		// Head low, lip raised, shoulder loaded. The cheapest move it has and the one it
		// falls back to in contact: a short telegraph by boss standards, an ordinary
		// blow, and a recovery that does not give a party the room the heavy moves do.
		{
			kind: vnet.EncounterMoveKindBiteAndTear, fromStage: 1,
			telegraph: 900 * time.Millisecond, release: 200 * time.Millisecond,
			recovery: 1300 * time.Millisecond, cooldown: 2 * time.Second,
			minRange: 0, maxRange: 2.6, damagePercent: 100,
			hazard: encounterHazard{shape: vnet.HazardShapeCone, reach: 3.0, height: 2.2, halfAngle: 0.70},
		},
		// A raised paw and open claws, showing which side the sweep comes from. Wider
		// than the bite and cheaper per point of damage, on a long enough cooldown that
		// the two do not read as one move.
		{
			kind: vnet.EncounterMoveKindPrisonerClaws, fromStage: 1,
			telegraph: 1000 * time.Millisecond, release: 300 * time.Millisecond,
			recovery: 1400 * time.Millisecond, cooldown: 5 * time.Second,
			minRange: 0, maxRange: 3.0, damagePercent: 95,
			hazard: encounterHazard{shape: vnet.HazardShapeCone, reach: 3.4, height: 2.2, halfAngle: 1.05},
		},
		// Two scrapes, the neck tensed, and a lane fixed before the creature moves. Eleven
		// blocks a second for nine tenths of one is a lane just under ten blocks long, and
		// the band it may be chosen in stops a block short of that so the lane always
		// reaches past whoever it was aimed at.
		{
			kind: vnet.EncounterMoveKindCollarCharge, fromStage: 1,
			telegraph: 1200 * time.Millisecond, release: 900 * time.Millisecond,
			recovery: 1800 * time.Millisecond, impactRecovery: 2500 * time.Millisecond,
			cooldown: 9 * time.Second,
			minRange: 5.0, maxRange: 8.5, damagePercent: 120,
			travelSpeed: 11.0, travel: travelCharge,
			hazard: encounterHazard{shape: vnet.HazardShapeLine, height: 2.4, halfWidth: 1.4},
		},
		// Crouched, watching one player, with the landing region shown before the jump.
		// The region does not follow its target — that is what makes leaving it an answer
		// — so the damage is resolved at the landing and never along the arc.
		{
			kind: vnet.EncounterMoveKindPredatorLeap, fromStage: 1,
			telegraph: 1000 * time.Millisecond, release: 600 * time.Millisecond,
			recovery: 1600 * time.Millisecond, cooldown: 11 * time.Second,
			minRange: 4.0, maxRange: 6.5, damagePercent: 110,
			travelSpeed: 12.0, travel: travelLeap,
			hazard: encounterHazard{shape: vnet.HazardShapeDisc, reach: 3.0, height: 3.0},
		},
		// A full stop, the jaw opened and the neck loaded. The longest telegraph the
		// species has, the narrowest cone, the heaviest blow and the longest recovery:
		// there is no inescapable grab here, and the jaws closing on nothing is the whole
		// of the reward for reading it.
		{
			kind: vnet.EncounterMoveKindBonebreakerJaws, fromStage: 2,
			telegraph: 1500 * time.Millisecond, release: 300 * time.Millisecond,
			recovery: 2500 * time.Millisecond, cooldown: 7 * time.Second,
			minRange: 0, maxRange: 3.2, damagePercent: 155,
			hazard: encounterHazard{shape: vnet.HazardShapeCone, reach: 3.6, height: 2.2, halfAngle: 0.38},
		},
	},
}

// encounterMoveTicks is one move's four durations in the ticks Step counts.
type encounterMoveTicks struct {
	telegraph      uint32
	release        uint32
	recovery       uint32
	impactRecovery uint32
	cooldown       uint32
}

// encounterMoveTimingsFor converts every catalogued move at this server's tick rate.
//
// Converted once at construction beside every other duration [NewSim] turns into ticks,
// and keyed by move kind because the enum is unique across species. [ticksFor] is what
// keeps a short window from rounding away to nothing at a coarse rate — a release of zero
// ticks would be a damage window nobody could ever be inside.
//
// impactRecovery converts only when the move has one, so a move that cannot be stopped by
// terrain keeps a zero here rather than the one-tick floor [ticksFor] would give it.
func encounterMoveTimingsFor(tickRate uint8) map[vnet.EncounterMoveKind]encounterMoveTicks {
	timings := make(map[vnet.EncounterMoveKind]encounterMoveTicks)
	for _, repertoire := range encounterMoveCatalog {
		for _, def := range repertoire {
			one := encounterMoveTicks{
				telegraph: ticksFor(def.telegraph, tickRate),
				release:   ticksFor(def.release, tickRate),
				recovery:  ticksFor(def.recovery, tickRate),
				cooldown:  ticksFor(def.cooldown, tickRate),
			}
			if def.impactRecovery > 0 {
				one.impactRecovery = ticksFor(def.impactRecovery, tickRate)
			}
			timings[def.kind] = one
		}
	}
	return timings
}
