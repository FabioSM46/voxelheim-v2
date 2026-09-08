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
// phase, its ticks and its region — never the table that produced it. A client holding
// these numbers could compute the next move; one holding the announcement can only read
// the move it has been shown.
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

// encounterPulse is how a channelled move lays out the region of each of its pulses.
//
// **A channel announces the region of the pulse that is imminent, and nothing further
// ahead**, which is what `MovePhase.Channel` states: `pulse_index` says which one is
// coming and `hazards` is its own region. Showing the whole ritual at once would be a
// different contract and a much larger frame.
type encounterPulse uint8

const (
	// pulseNone is every move that is not channelled.
	pulseNone encounterPulse = iota

	// pulseExpandingRings walks a band outward from the anchor, one ring per pulse. The
	// burial's lit cracks: the safe ground is the middle and then the outside, and the
	// band's own width is what a player has to cross.
	pulseExpandingRings

	// pulseSectors lights a fixed number of discs on a ring around the anchor, at bearings
	// that move with the pulse index. The grave sectors and the requiem's floor: several
	// regions live at once, which makes it the case the reachable-safe-space rule was
	// written for.
	pulseSectors
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

	// pulse is how a channelled move places each pulse's region; pulseNone for every
	// move that is not channelled.
	pulse encounterPulse

	// band is one expanding ring's thickness, in blocks. pulseExpandingRings only.
	band float64

	// sectors is how many regions one pulse lights, and sectorRing is how far from the
	// anchor their centres sit. pulseSectors only.
	sectors    uint8
	sectorRing float64
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
	combo   *encounterCombo
	bearing float64
	kind    vnet.EncounterMoveKind

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

	// flightSpeed is how fast a released projectile crosses its announced lane, in blocks
	// per second, and is zero for a move that throws nothing.
	//
	// **A projectile here is a point moving inside an announced lane, not an entity.** The
	// lane is on the wire, the release's start tick and length are on the wire, and the
	// speed is the quotient of the two — so a client can draw the spear exactly where the
	// server has it without a second contract, and there is nothing for a client to
	// disagree with. Direction is the move's locked aim and is never revised, which is the
	// approved design's "does not steer after it is thrown".
	flightSpeed float64

	// channelPulse is how long one pulse is shown before it fires, and pulses is how many
	// there are. Both zero for a move that is not channelled.
	//
	// The pulse is shown and *then* becomes dangerous, exactly as a telegraph is: the
	// damage lands on the last tick of the interval, never on the first.
	channelPulse time.Duration
	pulses       uint8

	// interruptible marks the one kind of move this contract lets an interrupt end, and
	// interruptDamage is how much health has to be dealt during the channel to end it.
	//
	// **Damage is the interrupt, and it is not a new ability.** `castInterruptedByDamage`
	// is already how this game stops a *player's* cast, so the rule is the one already in
	// the world rather than a mechanic invented for a boss — the approved design asks for
	// exactly that. What differs is the threshold, and it has to: a player is rarely hit
	// during a one-second cast, while a boss is being hit continuously, so mirroring "any
	// damage at all" would make the channel impossible to complete. That is not a reward
	// for good play, it is the mechanic deleted. A threshold keeps it what the design
	// asks: a prize for concentrated damage, never the only way to survive the pulses.
	interruptible   bool
	interruptDamage uint16

	// interruptRecovery is the opening a successful interrupt buys, spent standing rather
	// than as a published phase — see [bossEncounter.staggerTicks] for why.
	interruptRecovery time.Duration

	hazard encounterHazard
}

// laneLength is how far this move's travel carries the creature during its release.
//
// **The announced lane is exactly what the release can cover**, which is what makes "no
// weapon deals damage outside its announced volume" true by construction for a charge:
// the swept segment the executor tests is a prefix of this length, always, because it is
// produced by the same two numbers.
func (d encounterMoveDef) laneLength() float64 {
	return d.laneSpeed() * d.release.Seconds()
}

// laneSpeed is whatever crosses this move's lane during its release: the creature itself
// for a charge or a leap, the projectile for a throw, and nothing for anything else.
func (d encounterMoveDef) laneSpeed() float64 {
	if d.travel != travelNone {
		return d.travelSpeed
	}
	return d.flightSpeed
}

// selectionReach is how far from the creature this move can matter, in blocks.
//
// **The question the range band is checked against**, and it is not the same question as
// the radius below. A move that travels can matter as far as it travels — a charge's lane
// runs that far, a leap's landing is placed that far away — while a move that plants the
// creature can only matter as far as its own region reaches.
func (d encounterMoveDef) selectionReach() float64 {
	if d.laneSpeed() > 0 {
		return d.laneLength()
	}
	switch d.hazard.pulse {
	case pulseSectors:
		// A ritual reaches its sectors, which sit a ring away from the creature.
		return d.hazard.sectorRing + d.hazard.reach
	case pulseExpandingRings:
		// The outermost band the last pulse lights, which is what the pulses and the band
		// produce between them rather than a number stated twice.
		return float64(d.pulses) * d.hazard.band
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
	if d.hazard.shape == vnet.HazardShapeLine && d.laneSpeed() > 0 {
		return d.laneLength()
	}
	if d.hazard.pulse == pulseExpandingRings {
		// The widest band any pulse lights. [mob.hazardsForPulse] narrows it to the one
		// annulus a given pulse actually announces; this is the extent of the whole move,
		// which is what a range band and the registry sweep ask about.
		return float64(d.pulses) * d.hazard.band
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
	// The Vargr that guarded the tomb. Physical throughout — scratches, bites, a charge and
	// a leap — so no row here is a cast and none may ever carry a channel. Stage 1 teaches
	// the four answers: leave the frontal cone, the lane, the landing region, the arc of
	// the sweep. Stage 2 adds the jaws, the same answer read faster against a heavier blow.
	vnet.MobKindVargrGuardian: {
		// Head low, lip raised, shoulder loaded. The cheapest move it has and its fallback
		// in contact: a short telegraph by boss standards, an ordinary blow, and a recovery
		// that does not give a party the room the heavy moves do.
		{
			kind: vnet.EncounterMoveKindBiteAndTear, fromStage: 1, combo: &biteCombo,
			telegraph: 900 * time.Millisecond, release: 200 * time.Millisecond,
			recovery: biteCombo.final, cooldown: 2 * time.Second,
			minRange: 0, maxRange: 2.6, damagePercent: 100,
			hazard: biteCombo.blows[0].hazard,
		},
		// A raised paw and open claws, showing which side the sweep comes from. Wider than
		// the bite, on a cooldown long enough that the two do not read as one move.
		{
			kind: vnet.EncounterMoveKindPrisonerClaws, fromStage: 1,
			telegraph: 1000 * time.Millisecond, release: 300 * time.Millisecond,
			recovery: 1400 * time.Millisecond, cooldown: 5 * time.Second,
			minRange: 0, maxRange: 3.0, damagePercent: 95,
			hazard: encounterHazard{shape: vnet.HazardShapeCone, reach: 3.4, height: 2.2, halfAngle: 1.05},
		},
		// Two scrapes, the neck tensed, and a lane fixed before the creature moves. Eleven
		// blocks a second for nine tenths of one is a lane just under ten long, and the
		// band stops short of that so it always reaches past whoever it was aimed at.
		{
			kind: vnet.EncounterMoveKindCollarCharge, fromStage: 1,
			telegraph: 1200 * time.Millisecond, release: 900 * time.Millisecond,
			recovery: 1800 * time.Millisecond, impactRecovery: 2500 * time.Millisecond,
			cooldown: 9 * time.Second,
			minRange: 5.0, maxRange: 8.5, damagePercent: 120,
			travelSpeed: 11.0, travel: travelCharge,
			hazard: encounterHazard{shape: vnet.HazardShapeLine, height: 2.4, halfWidth: 1.4},
		},
		// Crouched, watching one player, with the landing region shown before the jump. It
		// does not follow its target — which is what makes leaving it an answer — so the
		// damage is resolved at the landing and never along the arc.
		{
			kind: vnet.EncounterMoveKindPredatorLeap, fromStage: 1,
			telegraph: 1000 * time.Millisecond, release: 600 * time.Millisecond,
			recovery: 1600 * time.Millisecond, cooldown: 11 * time.Second,
			minRange: 4.0, maxRange: 6.5, damagePercent: 110,
			travelSpeed: 12.0, travel: travelLeap,
			hazard: encounterHazard{shape: vnet.HazardShapeDisc, reach: 3.0, height: 3.0},
		},
		// A full stop, the jaw opened and the neck loaded. The longest telegraph, narrowest
		// cone, heaviest blow and longest recovery: no inescapable grab, and the jaws
		// closing on nothing is the whole of the reward for reading it.
		{
			kind: vnet.EncounterMoveKindBonebreakerJaws, fromStage: 2,
			telegraph: 1500 * time.Millisecond, release: 300 * time.Millisecond,
			recovery: 2500 * time.Millisecond, cooldown: 7 * time.Second,
			minRange: 0, maxRange: 3.2, damagePercent: 155,
			hazard: encounterHazard{shape: vnet.HazardShapeCone, reach: 3.6, height: 2.2, halfAngle: 0.38},
		},
	},

	// The Draugr king. A warrior and a caster both, which is the whole reason `MovePhase`
	// has `Channel` at all. Stage 1 is the duel — a vertical cut, a sweeping toll, and the
	// spear that says out loud he is not only a swordsman. Stage 2 adds the rituals. Stage
	// 3 adds the requiem, the one move here an interrupt may end.
	vnet.MobKindDraugrKing: {
		// The blade held high, a pause, and a low clang. A strip rather than a cone: a
		// two-handed vertical cut has a lane and the answer is to leave it sideways.
		{
			kind: vnet.EncounterMoveKindKingsSentence, fromStage: 1,
			telegraph: 1200 * time.Millisecond, release: 250 * time.Millisecond,
			recovery: 1800 * time.Millisecond, cooldown: 3 * time.Second,
			minRange: 0, maxRange: 4.0, damagePercent: 110,
			hazard: encounterHazard{shape: vnet.HazardShapeLine, reach: 5.0, height: 3.0, halfWidth: 1.1},
		},
		// Three distinct poses, announced one at a time. Each toll is a move instance of
		// its own with its own telegraph and id. The committed physical combination
		// bypasses the ordinary scheduler until its final recovery is earned.
		{
			kind: vnet.EncounterMoveKindThreeTolls, fromStage: 1, combo: &tollCombo,
			telegraph: 900 * time.Millisecond, release: 200 * time.Millisecond,
			recovery: tollCombo.final, cooldown: 1500 * time.Millisecond,
			minRange: 0, maxRange: 3.4, damagePercent: 75,
			hazard: tollCombo.blows[0].hazard,
		},
		// The free hand raised, a crystal forming, a direction fixed before release. The
		// lane is what the spear crosses during its release and the spear does not steer
		// after it is thrown, so the band it may be chosen in stops well inside the reach.
		{
			kind: vnet.EncounterMoveKindSepulchreSpear, fromStage: 1,
			telegraph: 1400 * time.Millisecond, release: 800 * time.Millisecond,
			recovery: 1600 * time.Millisecond, cooldown: 7 * time.Second,
			minRange: 4.0, maxRange: 14.0, damagePercent: 95,
			flightSpeed: 22.0,
			hazard:      encounterHazard{shape: vnet.HazardShapeLine, height: 2.6, halfWidth: 0.9},
		},
		// The sword planted and rings of lit cracks running outward in sequence. Four
		// pulses of a two-block band: the safe ground is ahead of the wave and then behind
		// it, and the band is narrow enough to cross on foot.
		{
			kind: vnet.EncounterMoveKindBurial, fromStage: 2,
			telegraph: 1500 * time.Millisecond, channelPulse: 700 * time.Millisecond, pulses: 4,
			release: 200 * time.Millisecond, recovery: 2000 * time.Millisecond,
			cooldown: 14 * time.Second,
			minRange: 0, maxRange: 8.0, damagePercent: 70,
			hazard: encounterHazard{
				shape: vnet.HazardShapeRing, height: 2.0,
				pulse: pulseExpandingRings, band: 2.0,
			},
		},
		// An arm thrown toward the graves, rune groups lighting in the order the sectors
		// will erupt. Two of six sectors a pulse, so several regions are live at once and
		// four are always clear — the reachable-safe-space rule is what holds that, not
		// this comment.
		{
			kind: vnet.EncounterMoveKindEdictOfTheGraves, fromStage: 2,
			telegraph: 1500 * time.Millisecond, channelPulse: 800 * time.Millisecond, pulses: 3,
			release: 200 * time.Millisecond, recovery: 1800 * time.Millisecond,
			cooldown: 16 * time.Second,
			minRange: 0, maxRange: 10.0, damagePercent: 80,
			hazard: encounterHazard{
				shape: vnet.HazardShapeDisc, height: 2.4, reach: 3.0,
				pulse: pulseSectors, sectors: 2, sectorRing: 7.0,
			},
		},
		// The sword planted and three notes intoned, each floor sector shown before it
		// activates. The one move this contract expects to be interruptible, and the
		// interrupt is a reward rather than the only escape: the pulses are leaveable
		// whether or not anybody breaks it.
		{
			kind: vnet.EncounterMoveKindRequiemOfTheBuried, fromStage: 3,
			telegraph: 1500 * time.Millisecond, channelPulse: 900 * time.Millisecond, pulses: 3,
			release: 200 * time.Millisecond, recovery: 2000 * time.Millisecond,
			interruptible: true, interruptDamage: 150,
			interruptRecovery: 3 * time.Second,
			cooldown:          20 * time.Second,
			minRange:          0, maxRange: 9.0, damagePercent: 90,
			hazard: encounterHazard{
				shape: vnet.HazardShapeDisc, height: 2.4, reach: 3.2,
				pulse: pulseSectors, sectors: 2, sectorRing: 6.0,
			},
		},
	},
}

// encounterMoveTicks is one move's four durations in the ticks Step counts.
type encounterMoveTicks struct {
	comboBetween, comboFinal uint32
	telegraph                uint32
	release                  uint32
	recovery                 uint32
	impactRecovery           uint32
	interruptRecovery        uint32
	channelPulse             uint32
	cooldown                 uint32
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
			// Each of these converts only when the move has it, so a move without one
			// keeps a zero here rather than the one-tick floor [ticksFor] would give it.
			if def.impactRecovery > 0 {
				one.impactRecovery = ticksFor(def.impactRecovery, tickRate)
			}
			if def.interruptRecovery > 0 {
				one.interruptRecovery = ticksFor(def.interruptRecovery, tickRate)
			}
			if def.channelPulse > 0 {
				one.channelPulse = ticksFor(def.channelPulse, tickRate)
			}
			if def.combo != nil {
				one.comboBetween = ticksFor(def.combo.between, tickRate)
				one.comboFinal = ticksFor(def.combo.final, tickRate)
			}
			timings[def.kind] = one
		}
	}
	return timings
}
