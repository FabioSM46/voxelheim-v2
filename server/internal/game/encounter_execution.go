package game

import (
	"cmp"
	"math"
	"slices"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// Who decides what a boss does, and when what it does hurts anybody.
//
// **Every answer here is the server's, taken from simulation ticks.** Nothing reads an
// animation event, a client marker or a presentation clock. encounter.go publishes what
// this file decides; #1025 draws it.
//
// **Three volumes, deliberately.** The box a boss collides with when it moves is
// `mobDefinition.body` through [moveAndCollide]; the box a player's blade must reach is
// that same body box read by the swing; the region a boss's attack endangers is the
// announced [protocol.HazardVolume] and nothing else. A blow is never resolved against the
// creature's body, so no weapon can damage anybody outside the region the client was shown
// — see [mob.moveReachesLocked] and [sweptLaneReaches] for how that is kept true.

// escapeBearings is how many directions the reachable-safe-space check tries. Sixteen is
// 22.5 degrees apart — finer than any announced region's opening, and cheap enough to run
// on every candidate: O(bearings x hazards), both bounded by [protocol.MaxHazardsPerMove].
const escapeBearings = 16

// hazardSampleCorners is the horizontal footprint sampled when asking whether a body is
// inside a region: the centre plus the four corners. The direction it fails in is the one
// to have — a body whose samples all miss a region it clips is *not* hit. Under-reporting
// costs a player nothing they were told to expect; rounding outward would deal damage past
// the boundary the client drew, which is the one thing this file exists to prevent.
const hazardSampleCorners = 4

// runningMove is the one move an encounter is executing, as the server holds it. At most
// one at a time in this half of the encounter, which is what makes the reachable-safe-space
// rule below trivially satisfiable for the guardian — the king's overlapping rituals arrive
// with the rest of his repertoire and inherit the rule rather than growing their own.
type runningMove struct {
	def   encounterMoveDef
	ticks encounterMoveTicks

	// instanceID is this instance's identity on the wire, unique among the encounter's
	// live moves and never reused inside one encounter. Two bites in a row are two ids.
	instanceID uint64

	phase       vnet.MovePhase
	remaining   uint32
	phaseTicks  uint32
	startedTick uint32

	// aim is the horizontal unit direction this move locked before it began, never revised.
	// Every move here is *aimed* rather than targeted, by design: the contract sends a
	// direction exactly when no target is named, so a cone naming its target would leave a
	// client unable to draw where it points — and one that followed its target would be a
	// region nobody can leave.
	aim [3]float64

	// anchor is the announced region's origin, locked with the aim. For a lane it is
	// where the run begins, which is what makes the swept segment below a prefix of the
	// region the client was shown.
	anchor [3]float64

	hazards []protocol.HazardVolume

	// hit is one permitted contact per target per window. Cleared when the release window
	// opens, so a second instance of the same move can hit the same player again and a
	// single instance never can.
	hit map[uint64]struct{}

	// travelledFrom is where the creature stood at the start of this tick's displacement.
	// The swept damage test's first endpoint.
	travelledFrom [3]float64

	// impacted records that terrain stopped a charge. It ends the run early and buys the
	// approved design's longer opening.
	impacted bool

	// laneSpent records that a charge has crossed the whole of its announced lane. It
	// ends the run early too, and pays the ordinary declared recovery: running out of
	// announced lane is not the same event as hitting a monolith, and only the second one
	// is worth the longer opening.
	laneSpent bool

	// pulseIndex is which pulse of a channel is imminent, counted from zero exactly as the
	// contract's `pulse_index` is.
	pulseIndex uint8

	// channelDamage is the health taken since this channel began, and the only thing an
	// interrupt is decided from. Reset with the instance, never carried between moves.
	channelDamage uint16

	// flight is where a released projectile has got to, flightFrom where it stood at the
	// start of this tick's step, and flightSpent that it has reached the end of its
	// announced lane or a wall. The pair is the charge's [travelledFrom]/pos exactly, for
	// a point that is not the creature.
	flight      [3]float64
	flightFrom  [3]float64
	flightSpent bool
}

// announcement is this instance in the shape the wire carries it.
//
// **Recovery announces no region**, which is the contract's own statement that nothing is
// dangerous while the creature is open. The hazards are the same objects the damage test
// reads, so what a client is shown and what the server resolves against cannot drift.
func (r *runningMove) announcement() protocol.EncounterMove {
	move := protocol.EncounterMove{
		MoveInstanceID:   r.instanceID,
		Kind:             r.def.kind,
		Phase:            r.phase,
		PhaseStartedTick: r.startedTick,
		PhaseTicks:       r.phaseTicks,
		Aim:              toWire(r.aim),
	}
	if r.phase != vnet.MovePhaseRecovery {
		move.Hazards = r.hazards
	}
	// **Only while channelling**, because the encoder refuses a move that counts pulses or
	// claims to be interruptible in any other phase — and it is right to: a swing that
	// advertised an interrupt would promise a player an escape the server does not honour.
	// It is also why an interrupted instance is ended *here*, in `Channel`, rather than
	// after a move to recovery that would strip the flag off the frame carrying the ending.
	if r.phase == vnet.MovePhaseChannel {
		move.PulseIndex = r.pulseIndex
		move.PulseTotal = r.def.pulses
		move.Interruptible = r.def.interruptible
	}
	return move
}

// tickCooldownsLocked spends one tick of every move's cooldown.
//
// Runs whether or not a move is under way, so a heavy move's cooldown is measured from
// when it ended rather than from when the creature next had nothing to do.
func (e *bossEncounter) tickCooldownsLocked() {
	if e.staggerTicks > 0 {
		e.staggerTicks--
	}
	for kind, left := range e.cooldowns {
		if left <= 1 {
			delete(e.cooldowns, kind)
			continue
		}
		e.cooldowns[kind] = left - 1
	}
}

// stepEncounter is the whole of a catalogued boss's tick.
//
// It replaces the shared hostile state machine rather than running beside it: a creature
// with a repertoire never also performs the ordinary telegraph-and-swing, so there is
// exactly one path by which a boss deals damage and exactly one announcement describing
// it. The caller holds Sim.mu.
func (m *mob) stepEncounter(s *Sim, players []*Player, tick uint64) {
	if m.encounter != nil {
		m.encounter.tickCooldownsLocked()
		if m.encounter.running != nil {
			m.advanceRunningMoveLocked(s, players, tick)
			return
		}
	}

	// Re-chosen only between moves. A move under way holds the target it locked, which
	// is what the branch above is: re-choosing during a telegraph would silently re-aim
	// an announcement a player has already reacted to.
	target := m.chooseTargetLocked(s, players)
	if target == nil || m.encounter == nil {
		m.action = vnet.MobActionIdle
		m.actionTicks = 0
		m.vel[0], m.vel[2] = 0, 0
		return
	}

	// An interrupt's opening is spent standing, and nothing may be announced through it.
	// See [bossEncounter.staggerTicks] for why it is not a published recovery phase.
	if m.encounter.staggerTicks > 0 {
		m.action = vnet.MobActionRecovery
		m.vel[0], m.vel[2] = 0, 0
		return
	}

	if def, chosen := m.selectEncounterMoveLocked(s, target); chosen {
		m.beginEncounterMoveLocked(s, def, target, tick)
		return
	}

	m.action = vnet.MobActionChase
	m.steerToward(s.terrain, target)
}

// selectEncounterMoveLocked is the move this creature commits to, or none.
//
// **Least recently used among what is available, and deterministic throughout.** The
// cooldown decides what a move costs; this decides which affordable one is taken, drawing
// no random numbers — the same encounter from the same state produces the same fight.
//
// A candidate that would leave its target no reachable safe space is refused and the next
// tried. With one live move and a bounded opening that never fires for the guardian; it is
// written at the scheduler so the king's overlapping rituals cannot bypass it.
func (m *mob) selectEncounterMoveLocked(s *Sim, target *Player) (encounterMoveDef, bool) {
	e := m.encounter
	repertoire := encounterMoveCatalog[m.kind]
	def := m.species()
	body := def.body.boxAt(m.pos)
	distance := boxDistance(body, target.box())

	// A move is a blow, and a blow needs somewhere to travel: the same question
	// [mob.inReach] asks of an ordinary swing, asked once here for every candidate.
	if !clearLineOfSight(s.terrain, boxCentre(body), boxCentre(target.box())) {
		return encounterMoveDef{}, false
	}

	candidates := make([]int, 0, len(repertoire))
	for i, one := range repertoire {
		if one.fromStage > e.phase {
			continue
		}
		if _, cooling := e.cooldowns[one.kind]; cooling {
			continue
		}
		if distance < one.minRange || distance > one.maxRange {
			continue
		}
		candidates = append(candidates, i)
	}
	// Never used sorts first, because its recorded tick is zero and no move has ever
	// been used at tick zero. Catalog order breaks the tie, which is why the rows are
	// written in the order the design teaches them.
	// cmp.Compare rather than a subtraction: a tick count is a uint64 and the difference
	// of two of them does not fit an int on a 32-bit build, where the server also has to
	// run.
	slices.SortStableFunc(candidates, func(a, b int) int {
		return cmp.Compare(e.lastUsed[repertoire[a].kind], e.lastUsed[repertoire[b].kind])
	})

	for _, i := range candidates {
		one := repertoire[i]
		if !s.moveLeavesAnEscapeLocked(m, one, target) {
			continue
		}
		return one, true
	}
	return encounterMoveDef{}, false
}

// beginEncounterMoveLocked commits to a move and announces it.
//
// **The aim, the anchor and the whole region are fixed here**, at the first tick of the
// telegraph rather than at its end — the lane fixed before the sprint, the landing shown
// before the jump, the side of the sweep shown by the raised paw. It is what makes each of
// these moves something a player leaves rather than something that follows them.
func (m *mob) beginEncounterMoveLocked(s *Sim, def encounterMoveDef, target *Player, tick uint64) {
	e := m.encounter
	aim := m.aimAt(target)
	e.nextMoveID++
	e.running = &runningMove{
		def:        def,
		ticks:      s.encounterMoves[def.kind],
		instanceID: e.nextMoveID,
		aim:        aim,
		anchor:     m.hazardAnchor(def, aim, target),
	}
	e.running.hazards = m.hazardsForPulse(def, e.running.aim, e.running.anchor, 0)
	e.lastUsed[def.kind] = tick

	m.yaw = wrapAngle(math.Atan2(-aim[0], -aim[2]))
	m.vel[0], m.vel[2] = 0, 0
	m.enterMovePhaseLocked(vnet.MovePhaseTelegraph, e.running.ticks.telegraph, tick)
	m.publishRunningMoveLocked()
}

// aimAt is the horizontal unit direction from this creature to a player.
//
// Never zero: two bodies standing at the same horizontal point fall back to the creature's
// own facing, because the encoder refuses an aim of zero and a move that announced one
// would be a frame no client may read.
func (m *mob) aimAt(target *Player) [3]float64 {
	dx, dz := target.pos[0]-m.pos[0], target.pos[2]-m.pos[2]
	if length := math.Hypot(dx, dz); length > 0 {
		return [3]float64{dx / length, 0, dz / length}
	}
	return [3]float64{-math.Sin(m.yaw), 0, -math.Cos(m.yaw)}
}

// hazardAnchor is where this move's announced region is anchored, in world space.
//
// A cone opens from the creature's own centre, a lane starts where the run begins, and a
// landing disc is centred on the point the leap is aimed at — clamped to the distance the
// leap can actually cover, so the region is never one the creature cannot reach.
func (m *mob) hazardAnchor(def encounterMoveDef, aim [3]float64, target *Player) [3]float64 {
	body := m.species().body
	if def.hazard.pulse != pulseNone {
		// A ritual's rings and sectors lie on the floor, so the band is centred half its
		// own height above the ground rather than half the creature's: a wave that sat at
		// a tall king's chest would be a region nobody standing in it is inside.
		return [3]float64{m.pos[0], m.pos[1] + def.hazard.height/2, m.pos[2]}
	}
	centre := [3]float64{m.pos[0], m.pos[1] + body.height/2, m.pos[2]}
	if def.travel != travelLeap {
		return centre
	}
	reach := min(math.Hypot(target.pos[0]-m.pos[0], target.pos[2]-m.pos[2]), def.laneLength())
	return [3]float64{
		m.pos[0] + aim[0]*reach,
		m.pos[1] + def.hazard.height/2,
		m.pos[2] + aim[2]*reach,
	}
}

// hazardsForPulse is the region one move announces on one pulse, placed in the world.
//
// The vector is what both the client and [mob.moveReachesLocked] read, so the region drawn
// and the region resolved against are the same numbers rather than two derivations of one
// intention. A move that is not channelled has one pulse, numbered zero.
func (m *mob) hazardsForPulse(def encounterMoveDef, aim, anchor [3]float64, pulse uint8) []protocol.HazardVolume {
	base := protocol.HazardVolume{
		Shape:     def.hazard.shape,
		Origin:    toWire(anchor),
		Direction: toWire(aim),
		Radius:    float32(def.announcedRadius()),
		Height:    float32(def.hazard.height),
		HalfAngle: float32(def.hazard.halfAngle),
		HalfWidth: float32(def.hazard.halfWidth),
	}
	// A field a shape ignores is written as zero rather than carrying a value a later
	// shape might be given a meaning for, which is what the contract asks.
	if def.hazard.shape != vnet.HazardShapeCone {
		base.HalfAngle = 0
	}
	if def.hazard.shape != vnet.HazardShapeLine {
		base.HalfWidth = 0
	}

	switch def.hazard.pulse {
	case pulseExpandingRings:
		// One band per pulse, walking outward. The ring the client is shown is exactly the
		// annulus the damage test uses, so the ground inside the wave and the ground
		// beyond it are both genuinely safe.
		inner := float64(pulse) * def.hazard.band
		base.InnerRadius = float32(inner)
		base.Radius = float32(inner + def.hazard.band)
		return []protocol.HazardVolume{base}
	case pulseSectors:
		regions := make([]protocol.HazardVolume, 0, def.hazard.sectors)
		for sector := range def.hazard.sectors {
			bearing := sectorBearing(pulse, sector)
			one := base
			one.Radius = float32(def.hazard.reach)
			one.Origin = toWire([3]float64{
				anchor[0] + math.Cos(bearing)*def.hazard.sectorRing,
				anchor[1],
				anchor[2] + math.Sin(bearing)*def.hazard.sectorRing,
			})
			regions = append(regions, one)
		}
		return regions
	}
	return []protocol.HazardVolume{base}
}

// sectorSlots is how many places a ritual's sectors are drawn from.
//
// Six, against the two a pulse lights, so four are clear at every pulse. That is the
// arithmetic behind the reachable-safe-space rule rather than a substitute for it — the
// rule still runs on every candidate, because a later ritual could light five.
const sectorSlots = 6

// sectorBearing is where one sector of one pulse sits, in radians.
//
// Deterministic and spread: the slot walks by two per pulse and the sectors of a single
// pulse sit three apart, so no pulse lights adjacent ground and no two consecutive pulses
// light the same. A generator would make the same fight differ run to run, which is the
// one property every tick path in this simulation keeps.
func sectorBearing(pulse, sector uint8) float64 {
	slot := (uint32(pulse)*2 + uint32(sector)*3) % sectorSlots
	return 2 * math.Pi * float64(slot) / sectorSlots
}

// enterMovePhaseLocked moves the running instance into its next phase.
//
// The tick this phase began at and how many ticks it lasts are both stated, because that
// pair is the whole of what a client needs to draw a countdown against the snapshot
// stream rather than against a clock it started when a frame arrived.
func (m *mob) enterMovePhaseLocked(phase vnet.MovePhase, ticks uint32, tick uint64) {
	r := m.encounter.running
	r.phase = phase
	r.phaseTicks = ticks
	r.remaining = ticks
	r.startedTick = uint32(tick)
	if phase == vnet.MovePhaseRelease || phase == vnet.MovePhaseChannel {
		// One permitted hit per target per window. The window is this release, or this
		// single pulse of a channel — a fresh ledger here rather than at the move's start
		// is what makes that sentence mean the window rather than the whole instance.
		r.hit = make(map[uint64]struct{})
	}
	if phase == vnet.MovePhaseRelease {
		// A thrown move's projectile starts at the anchor the lane was announced from, so
		// the flight and the region share an origin by construction.
		r.flight, r.flightFrom = r.anchor, r.anchor
	}
}

// advanceRunningMoveLocked spends one tick of the move under way.
//
// The caller holds Sim.mu.
func (m *mob) advanceRunningMoveLocked(s *Sim, players []*Player, tick uint64) {
	r := m.encounter.running

	// Nobody left to fight is a withdrawal rather than a completion: the contract
	// distinguishes a move that ran to its end from one the encounter took away, and a
	// hazard whose fight has ended must stop being announced on the tick it ends.
	if !anyLivePlayerInRange(m, players) {
		m.finishEncounterMoveLocked(vnet.MoveEndCancelled)
		m.action = vnet.MobActionIdle
		m.vel[0], m.vel[2] = 0, 0
		return
	}

	// **The phase changes at the top of a tick, so the phase a tick spends is the phase it
	// publishes.** Advanced at the foot instead, a tick could execute a release — travel,
	// contact, damage — and then publish the recovery it had moved into, so the frame a
	// client received for the tick it was hit on said the creature was open and nothing
	// was dangerous. Not a boundary curiosity: [ticksFor] floors at one tick, so the
	// guardian's 200 ms window is a single tick below five hertz and its whole release
	// would publish as recovery. It also keeps `phase_started_tick` honest, since a phase
	// is now entered on the first tick it runs.
	// An interrupt ends the instance where it stands, before this tick's pulse can fire.
	// The ending is published on the `Channel` frame, which is the only phase allowed to
	// carry `interruptible`; the opening it buys is the encounter's stagger.
	if r.phase == vnet.MovePhaseChannel && r.def.interruptible &&
		r.def.interruptDamage > 0 && r.channelDamage >= r.def.interruptDamage {
		stagger := r.ticks.interruptRecovery
		m.finishEncounterMoveLocked(vnet.MoveEndInterrupted)
		m.encounter.staggerTicks = stagger
		m.action = vnet.MobActionRecovery
		m.vel[0], m.vel[2] = 0, 0
		return
	}

	if r.remaining == 0 || (r.phase == vnet.MovePhaseRelease && (r.impacted || r.laneSpent || r.flightSpent)) {
		switch r.phase {
		case vnet.MovePhaseTelegraph:
			if r.def.pulses > 0 {
				m.enterChannelPulseLocked(0, tick)
			} else {
				m.enterMovePhaseLocked(vnet.MovePhaseRelease, r.ticks.release, tick)
			}
		case vnet.MovePhaseChannel:
			// The next pulse, or the recovery once the last has fired.
			if next := r.pulseIndex + 1; next < r.def.pulses {
				m.enterChannelPulseLocked(next, tick)
			} else {
				m.enterMovePhaseLocked(vnet.MovePhaseRecovery, r.ticks.recovery, tick)
			}
		case vnet.MovePhaseRelease:
			recovery := r.ticks.recovery
			if r.impacted && r.ticks.impactRecovery > 0 {
				recovery = r.ticks.impactRecovery
			}
			m.enterMovePhaseLocked(vnet.MovePhaseRecovery, recovery, tick)
		case vnet.MovePhaseRecovery:
			m.finishEncounterMoveLocked(vnet.MoveEndCompleted)
			m.action = vnet.MobActionIdle
			m.vel[0], m.vel[2] = 0, 0
			return
		}
	}

	switch r.phase {
	case vnet.MovePhaseTelegraph:
		// Committed means stationary for everything that does not travel, and everything
		// that travels is stationary until its release: the region a player is reading is
		// not also closing the distance it is measured against.
		m.action = vnet.MobActionWindup
		m.vel[0], m.vel[2] = 0, 0
	case vnet.MovePhaseChannel:
		// A ritual is anchored for the whole of it — the approved design's "the king stays
		// anchored until the propagation ends". The pulse is shown for its interval and
		// fires on the last tick of it, which is the telegraph's own promise one layer
		// down: the region is drawn before it is dangerous, never with the damage.
		m.action = vnet.MobActionWindup
		m.vel[0], m.vel[2] = 0, 0
		if r.remaining == 1 {
			s.resolveMoveDamageLocked(m, players)
		}
	case vnet.MovePhaseRelease:
		m.action = vnet.MobActionWindup
		m.travelDuringReleaseLocked(s)
		m.advanceProjectileLocked(s)
		s.resolveMoveDamageLocked(m, players)
	case vnet.MovePhaseRecovery:
		m.action = vnet.MobActionRecovery
		m.vel[0], m.vel[2] = 0, 0
	}
	r.remaining--
	m.publishRunningMoveLocked()
}

// enterChannelPulseLocked shows the next pulse of a channel.
//
// A pulse is a window in the sense the hit ledger means: cleared here, so one pulse lands
// once on each target and the next pulse of the same ritual may land again.
func (m *mob) enterChannelPulseLocked(pulse uint8, tick uint64) {
	r := m.encounter.running
	r.pulseIndex = pulse
	r.hazards = m.hazardsForPulse(r.def, r.aim, r.anchor, pulse)
	m.enterMovePhaseLocked(vnet.MovePhaseChannel, r.ticks.channelPulse, tick)
}

// projectileSubStep is the furthest a spear travels between two terrain samples.
//
// [maxSubStep]'s reasoning, for a point rather than a body: a sample every quarter block
// cannot step over a solid voxel, whatever the speed or the tick rate. The alternative is
// a full voxel traversal of the segment, which is what [clearLineOfSight] does — but that
// answers "is anything in the way" rather than "how far did it get", and the flight has to
// stop *somewhere* the swept damage test can then use as its endpoint.
const projectileSubStep = maxSubStep

// advanceProjectileLocked moves a released spear one tick along its announced lane.
//
// **The charge's rules, for a point that is not the creature.** The step is clamped to what
// is left of the announced lane, so the flight can never leave the region the client was
// shown; the damage test is the segment crossed this tick rather than the point it ended
// at, so a spear faster than a body is wide cannot pass through anybody; and the direction
// is the move's locked aim, so it does not steer after it is thrown.
//
// **Terrain is swept, not sampled at the destination.** A single test of the voxel the step
// ends in is not a wall check at all: at 22 blocks a second the spear covers 1.1 blocks per
// tick at the default rate and the whole lane in one tick at a rate of 1, so a step can
// begin in front of a one-block wall and end past it with both endpoints in air. Measured
// before this was written — at 1 Hz the spear crossed the full 17.6-block lane through a
// one-block wall and speared a player standing behind it. What made that shape worth a
// paragraph is that it *looked* like it worked: the endpoint check happens to catch the
// tunnel at some rates and not others, so its passing said nothing.
func (m *mob) advanceProjectileLocked(s *Sim) {
	r := m.encounter.running
	if r.def.flightSpeed <= 0 {
		return
	}
	r.flightFrom = r.flight

	reach := min(r.def.flightSpeed*s.dt, r.remainingFlight())
	if reach <= 0 {
		r.flightSpent = true
		return
	}

	// Bounded: the reach is already clamped to the announced lane, so the worst case is
	// the whole lane in quarter blocks — tens of samples, once per tick, for one move.
	steps := int(math.Ceil(reach / projectileSubStep))
	step := reach / float64(steps)
	for range steps {
		next := [3]float64{r.flight[0] + r.aim[0]*step, r.flight[1], r.flight[2] + r.aim[2]*step}
		if s.terrain.Solid(int64(math.Floor(next[0])), int64(math.Floor(next[1])), int64(math.Floor(next[2]))) {
			// Stopped at the last free sample, so the spear rests in front of the face it
			// struck rather than inside it — and the segment the damage test reads ends
			// there too.
			r.flightSpent = true
			return
		}
		r.flight = next
	}
}

// remainingFlight is how much of the announced lane the spear has left to cross.
func (r *runningMove) remainingFlight() float64 {
	if len(r.hazards) == 0 {
		return 0
	}
	crossed := math.Hypot(r.flight[0]-r.anchor[0], r.flight[2]-r.anchor[2])
	return float64(r.hazards[0].Radius) - crossed
}

// travelDuringReleaseLocked displaces a charging or leaping creature by one tick.
//
// **Through [moveAndCollide], which is the continuous half of the requirement**: the
// displacement is resolved against the same voxels a player's is, in sub-steps, so no
// speed carries the creature through a wall. The velocity is left at zero afterwards
// because the position has already been written — [mob.physics] then only falls it.
//
// A horizontal axis the terrain stopped is the monolith: the run ends here and the
// recovery it pays is the longer one.
func (m *mob) travelDuringReleaseLocked(s *Sim) {
	r := m.encounter.running
	r.travelledFrom = m.pos
	m.vel[0], m.vel[2] = 0, 0
	if r.def.travel == travelNone {
		return
	}

	// **Never past the region that was announced.** A leap stops at its landing, a charge
	// at the far end of its lane, and both bounds are read off the announcement the client
	// holds rather than recomputed — so the segment [mob.moveReachesLocked] tests is a
	// prefix of the announced strip whatever the tick rate does.
	//
	// The charge used to have no clamp, relying on `travelSpeed x release` equalling
	// `travelSpeed x releaseTicks x dt`. Those agree only where [ticksFor] converts
	// exactly. It truncates, so the ordinary answer is short — but it floors at one tick,
	// and at a rate of 1 (which [NewSim] accepts) the guardian's 900 ms release becomes a
	// whole second: 11 blocks against an announced 9.9, damaging anybody in the 1.1
	// between. Deriving the lane from ticks instead would have made the region a client is
	// shown a property of this server's rate, when it is a property of the move.
	reach := min(r.def.travelSpeed*s.dt, r.remainingTravel(m.pos))
	if reach <= 0 {
		// The lane is spent, so the run is finished whatever ticks remain — spending them
		// would be travelling past the region. It pays the ordinary declared recovery:
		// reaching the end of a lane is not hitting anything.
		r.laneSpent = r.def.travel == travelCharge
		return
	}

	pos, blocked := moveAndCollide(s.terrain, m.species().body, m.pos,
		[3]float64{r.aim[0] * reach, 0, r.aim[2] * reach})
	m.pos = pos
	if r.def.travel == travelCharge && (blocked[0] || blocked[2]) {
		r.impacted = true
	}
}

// remainingTravel is how much of this move's announced region the creature has left to
// cross, in blocks.
//
// **Measured from the announcement's own anchor and radius**, which is what the client was
// handed. Nothing here reads a duration, so nothing here can disagree with the wire.
func (r *runningMove) remainingTravel(pos [3]float64) float64 {
	if r.def.travel == travelNone || len(r.hazards) == 0 {
		return 0
	}
	// The distance between where the creature is and the announcement's anchor. For a
	// leap the anchor is the landing, so this is what is left to close; for a lane it is
	// where the run began, so this is what has been crossed already.
	toAnchor := math.Hypot(pos[0]-r.anchor[0], pos[2]-r.anchor[2])
	if r.def.travel == travelLeap {
		return toAnchor
	}
	return float64(r.hazards[0].Radius) - toAnchor
}

// resolveMoveDamageLocked applies this release tick's contacts.
//
// Every player the announced region reaches and that this window has not already hit,
// through the one path a creature's blow has ever taken — armour, shield, threat and the
// landed-blow projection are [Sim.landMobBlowLocked]'s, not a second copy of them here.
//
// The caller holds Sim.mu.
func (s *Sim) resolveMoveDamageLocked(m *mob, players []*Player) {
	r := m.encounter.running
	raw := moveDamage(m.species(), r.def)
	if raw == 0 {
		return
	}
	for _, p := range players {
		if !p.alive() || p.protectionTicks > 0 {
			continue
		}
		if _, already := r.hit[p.entityID]; already {
			continue
		}
		if !m.moveReachesLocked(p) {
			continue
		}
		r.hit[p.entityID] = struct{}{}
		s.landMobBlowLocked(m, p, raw)
	}
}

// moveReachesLocked reports whether this release tick's danger reaches a player.
//
// **A charge is answered by the segment it travelled this tick, not by where it ended** —
// see [sweptLaneReaches]. **A leap resolves once, on the last tick of its release**,
// because the danger is the landing rather than the arc: the region was announced before
// the jump and follows nobody, so damage before the creature has arrived would be damage
// for standing where it merely passed.
func (m *mob) moveReachesLocked(p *Player) bool {
	r := m.encounter.running
	if r.def.flightSpeed > 0 {
		// The spear's own segment, in the announced lane. [sweptLaneReaches] holds both
		// halves: inside the strip the client was shown, and swept over during this tick.
		return len(r.hazards) > 0 && sweptLaneReaches(r.hazards[0], r.flightFrom, r.flight, p.box())
	}
	switch r.def.travel {
	case travelCharge:
		return len(r.hazards) > 0 && sweptLaneReaches(r.hazards[0], r.travelledFrom, m.pos, p.box())
	case travelLeap:
		if r.remaining > 1 {
			return false
		}
	}
	for _, hazard := range r.hazards {
		if hazardReaches(hazard, p.box()) {
			return true
		}
	}
	return false
}

// moveDamage is one move's blow, from the species' registry damage and the move's share
// of it. A move that is worth anything at all is worth at least one point, on the rule
// [mob.stepWindup] already applies to armour: a blow that connects always lands.
func moveDamage(def mobDefinition, move encounterMoveDef) uint16 {
	if def.damage == 0 || move.damagePercent == 0 {
		return 0
	}
	return max(uint16(uint32(def.damage)*uint32(move.damagePercent)/100), 1)
}

// publishRunningMoveLocked writes the instance under way into the encounter's live moves —
// replaced in place where already announced, appended where not, so an ending written by
// [finishEncounterMoveLocked] for encounter.go's sweep is never overwritten.
func (m *mob) publishRunningMoveLocked() {
	e := m.encounter
	announcement := e.running.announcement()
	for i := range e.moves {
		if e.moves[i].MoveInstanceID == announcement.MoveInstanceID {
			e.moves[i] = announcement
			return
		}
	}
	e.moves = append(e.moves, announcement)
}

// finishEncounterMoveLocked ends the instance under way and states why.
//
// The announcement keeps its place for one more tick carrying the ending, which is what
// encounter.go's sweep then takes away: a client is told *why* a move stopped rather than
// having to infer it from a disappearance.
func (m *mob) finishEncounterMoveLocked(end vnet.MoveEnd) {
	e := m.encounter
	if e == nil || e.running == nil {
		return
	}
	for i := range e.moves {
		if e.moves[i].MoveInstanceID == e.running.instanceID && e.moves[i].Ended == vnet.MoveEndUnknown {
			e.moves[i].Ended = end
			// A move that has ended endangers nobody, and the region goes with it: a
			// receiver reading the last frame of an instance must not be handed a shape
			// it could still treat as dangerous.
			e.moves[i].Hazards = nil
		}
	}
	if cooldown := e.running.ticks.cooldown; cooldown > 0 {
		e.cooldowns[e.running.def.kind] = cooldown
	}
	e.running = nil
}

// anyLivePlayerInRange reports whether this creature still has anybody to fight. The
// withdrawal condition, deliberately the aggro range rather than a move's band: a party
// that ran out of the room ended the attack, one that stepped out of a cone read it.
func anyLivePlayerInRange(m *mob, players []*Player) bool {
	def := m.species()
	body := def.body.boxAt(m.pos)
	for _, p := range players {
		if !p.alive() || p.protectionTicks > 0 {
			continue
		}
		if boxDistance(body, p.box()) <= def.aggroRange {
			return true
		}
	}
	return false
}

// moveLeavesAnEscapeLocked reports whether a candidate move leaves its target somewhere
// safe it can actually reach.
//
// **The schedule is refused, not the damage.** A move whose announced region — with
// everything already running — covers every direction a player could walk during its
// telegraph is never announced, so there is no unavoidable window to survive. The distance
// sampled is how far a walking player travels while the telegraph plays out, which makes
// "reachable" mean reachable *in time*. Solid ground counts: a bearing that walks into a
// wall is not an escape, and neither is one leaving the addressable world.
//
// The caller holds Sim.mu.
func (s *Sim) moveLeavesAnEscapeLocked(m *mob, def encounterMoveDef, target *Player) bool {
	aim := m.aimAt(target)
	anchor := m.hazardAnchor(def, aim, target)

	// **Every pulse, not merely the first.** A ritual is a schedule, and a schedule whose
	// third pulse covers the ground its second one drove everybody onto is exactly the
	// thing this rule exists to refuse — checking only what is announced first would pass
	// it. A move that is not channelled has one pulse, numbered zero, so this is the same
	// question asked once.
	//
	// The distance sampled is how far a walking player travels in the warning that pulse
	// actually gives: a channel shows each pulse for its own interval, which is shorter
	// than the telegraph that opened the ritual.
	warning := def.telegraph
	if def.pulses > 0 {
		warning = def.channelPulse
	}
	warningTicks := ticksFor(warning, uint8(math.Round(1/s.dt)))

	pulses := max(def.pulses, 1)
	for pulse := range pulses {
		if !s.pulseLeavesAnEscapeLocked(m, def, target, aim, anchor, pulse, warningTicks) {
			return false
		}
	}
	return true
}

// pulseLeavesAnEscapeLocked is the reachable-safe-space question for one pulse.
//
// **The schedule is refused, not the damage.** A pulse whose regions — with anything this
// encounter already has running — cover every direction a player could walk in during its
// warning is never announced, so there is no unavoidable window to survive. Solid ground
// counts: a bearing that walks into a wall is not an escape, and neither is one that
// leaves the addressable world.
//
// The caller holds Sim.mu.
func (s *Sim) pulseLeavesAnEscapeLocked(m *mob, def encounterMoveDef, target *Player,
	aim, anchor [3]float64, pulse uint8, warningTicks uint32) bool {
	candidate := m.hazardsForPulse(def, aim, anchor, pulse)

	for bearing := range escapeBearings {
		angle := 2 * math.Pi * float64(bearing) / escapeBearings
		destination, reachable := s.walkEscapeRoute(target.pos, angle, warningTicks)
		if !reachable {
			continue
		}
		box := playerBox(destination)
		if anyHazardReaches(candidate, box) {
			continue
		}
		if m.encounter != nil && m.encounter.running != nil &&
			anyHazardReaches(m.encounter.running.hazards, box) {
			continue
		}
		return true
	}
	return false
}

// walkEscapeRoute tries one ordinary, unmounted walking bearing. Each step uses the
// player's collision, gravity and half-block step-up rules, so a centre ray through
// a low lintel or beside a wall cannot stand in for a traversable body. Check the
// destination actually reached, including its height after stairs or falling.
//
// Work is bounded by sixteen bearings times the tick-quantized warning from the
// server's fixed move catalogue. There is no search, retry or terrain generation.
// A blocked horizontal axis refuses this bearing instead of claiming the original
// endpoint was reached after sliding along a wall. This deliberately remains a
// conservative straight-walk test, not a general route planner.
func (s *Sim) walkEscapeRoute(start [3]float64, angle float64, warningTicks uint32) ([3]float64, bool) {
	pos := start
	verticalSpeed := 0.0
	delta := [3]float64{math.Cos(angle) * WalkSpeed * s.dt, 0, math.Sin(angle) * WalkSpeed * s.dt}
	for range warningTicks {
		verticalSpeed = max(verticalSpeed-Gravity*s.dt, -TerminalFallSpeed)
		delta[1] = verticalSpeed * s.dt
		next, blocked := moveAndCollideWithStep(s.terrain, playerBody, pos, delta, playerStepHeight)
		if blocked[0] || blocked[2] {
			return pos, false
		}
		if blocked[1] {
			verticalSpeed = 0
		}
		pos = next
	}
	return pos, !overlaps(s.terrain, playerBox(pos))
}

func anyHazardReaches(hazards []protocol.HazardVolume, b box) bool {
	for _, hazard := range hazards {
		if hazardReaches(hazard, b) {
			return true
		}
	}
	return false
}

// hazardReaches reports whether an announced region reaches a body.
//
// Vertical first, because it is two comparisons and rejects a body on another floor
// before any horizontal arithmetic runs. The extent is centred on the origin exactly as
// the contract states it.
//
// Horizontally the body is sampled at its centre and its four corners. A sample that
// misses is a miss, which is the direction stated at [hazardSampleCorners]: this can
// decline a contact it might have made, and it can never make one outside the announced
// region.
func hazardReaches(hazard protocol.HazardVolume, b box) bool {
	origin := [3]float64{float64(hazard.Origin[0]), float64(hazard.Origin[1]), float64(hazard.Origin[2])}
	half := float64(hazard.Height) / 2
	if b.max[1] <= origin[1]-half || b.min[1] >= origin[1]+half {
		return false
	}

	radius := float64(hazard.Radius)
	direction := [3]float64{float64(hazard.Direction[0]), 0, float64(hazard.Direction[2])}
	if length := math.Hypot(direction[0], direction[2]); length > 0 {
		direction[0], direction[2] = direction[0]/length, direction[2]/length
	}

	for _, sample := range horizontalSamples(b) {
		dx, dz := sample[0]-origin[0], sample[1]-origin[2]
		distance := math.Hypot(dx, dz)
		switch hazard.Shape {
		case vnet.HazardShapeCone:
			if distance > radius {
				continue
			}
			if distance == 0 {
				return true
			}
			cosine := (dx*direction[0] + dz*direction[2]) / distance
			if math.Acos(min(max(cosine, -1), 1)) <= float64(hazard.HalfAngle) {
				return true
			}
		case vnet.HazardShapeLine:
			along := dx*direction[0] + dz*direction[2]
			if along < 0 || along > radius {
				continue
			}
			if math.Abs(dx*direction[2]-dz*direction[0]) <= float64(hazard.HalfWidth) {
				return true
			}
		case vnet.HazardShapeDisc:
			if distance <= radius {
				return true
			}
		case vnet.HazardShapeRing:
			if distance <= radius && distance >= float64(hazard.InnerRadius) {
				return true
			}
		}
	}
	return false
}

// horizontalSamples is a body's footprint centre and its four corners.
func horizontalSamples(b box) [hazardSampleCorners + 1][2]float64 {
	return [hazardSampleCorners + 1][2]float64{
		{(b.min[0] + b.max[0]) / 2, (b.min[2] + b.max[2]) / 2},
		{b.min[0], b.min[2]},
		{b.min[0], b.max[2]},
		{b.max[0], b.min[2]},
		{b.max[0], b.max[2]},
	}
}

// sweptLaneReaches reports whether the segment a charge covered this tick reaches a body.
//
// **A conjunction, and both halves are load-bearing.** A body is reached when it is inside
// the announced strip *and* within half a width of the segment crossed this tick. Every
// bound of the first half is read off the volume the client holds, so the answer is a
// subset of what was announced structurally rather than by two derivations agreeing. The
// second half is what makes the sweep a sweep: measured against the segment rather than
// either endpoint, a player standing between two ticks' positions is hit rather than
// stepped over. Testing the announced lane alone would instead hurt everybody along it on
// the first tick of the run, before the creature has reached them.
//
// **Neither implies the other.** The segment test measures distance to a clamped point, so
// its region is a capsule, while `Line` is the rectangle [hazardReaches] tests: the caps
// bulge up to `half_width` past each square end. The segment test alone therefore reaches
// past the announcement at the end of a run — the same defect the travel clamp in
// [mob.travelDuringReleaseLocked] closes, one layer down.
func sweptLaneReaches(lane protocol.HazardVolume, from, to [3]float64, b box) bool {
	originY := float64(lane.Origin[1])
	half := float64(lane.Height) / 2
	if b.max[1] <= originY-half || b.min[1] >= originY+half {
		return false
	}
	halfWidth := float64(lane.HalfWidth)

	// The announced strip's own frame: where it starts, which way it runs, and how far.
	// Read off the volume the client holds rather than recomputed from the creature, so
	// the two cannot disagree.
	originX, originZ := float64(lane.Origin[0]), float64(lane.Origin[2])
	dirX, dirZ := float64(lane.Direction[0]), float64(lane.Direction[2])
	if length := math.Hypot(dirX, dirZ); length > 0 {
		dirX, dirZ = dirX/length, dirZ/length
	}
	radius := float64(lane.Radius)

	dx, dz := to[0]-from[0], to[2]-from[2]
	lengthSquared := dx*dx + dz*dz
	for _, sample := range horizontalSamples(b) {
		// Inside the announced strip first. This clause is not redundant with the
		// segment test below, and the difference is the whole reason it is here: the
		// segment test measures distance to a clamped point, which is a *capsule* with
		// rounded ends, while `Line` is the rectangle `hazardReaches` tests — so the
		// caps bulge up to `half_width` past each end of the strip. Without this the
		// last tick of a run could damage somebody standing 1.4 blocks beyond the lane
		// the client was shown.
		alongLane := (sample[0]-originX)*dirX + (sample[1]-originZ)*dirZ
		if alongLane < 0 || alongLane > radius {
			continue
		}
		if math.Abs((sample[0]-originX)*dirZ-(sample[1]-originZ)*dirX) > halfWidth {
			continue
		}

		// And swept over during *this* tick. What narrows the announced strip to the part
		// of it the creature actually crossed, so a player standing between two ticks'
		// positions is hit rather than stepped over.
		px, pz := sample[0]-from[0], sample[1]-from[2]
		alongStep := 0.0
		if lengthSquared > 0 {
			alongStep = min(max((px*dx+pz*dz)/lengthSquared, 0), 1)
		}
		if math.Hypot(px-alongStep*dx, pz-alongStep*dz) <= halfWidth {
			return true
		}
	}
	return false
}
