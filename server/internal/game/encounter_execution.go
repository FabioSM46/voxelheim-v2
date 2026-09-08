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
// animation event, a client marker or a presentation clock: a move is chosen on a tick,
// its region is announced on a tick, it becomes dangerous on a tick the client was told
// about in advance, and it stops on a tick. encounter.go publishes what this file
// decides; #1025 draws it.
//
// **Three volumes, and they are deliberately three.** The box a boss collides with when
// it moves is `mobDefinition.body` through [moveAndCollide]; the box a player's blade has
// to reach is that same body box read by the swing; the region a boss's own attack
// endangers is the announced [protocol.HazardVolume] and nothing else. A blow is never
// resolved against the creature's body, so a long weapon cannot damage anybody outside
// the region the client was shown.
//
// **A charge is swept, never sampled.** The creature's displacement each release tick
// goes through [moveAndCollide], so it cannot cross a wall, and the damage test for that
// tick is the *segment* it travelled rather than the box it ended in — so a player
// standing between two ticks' positions is hit rather than skipped. Both halves are
// required: continuous collision without a swept damage test would still let a fast
// enough charge pass through a player.

// escapeBearings is how many directions the reachable-safe-space check tries.
//
// Sixteen is 22.5 degrees apart, which is finer than any single announced region's
// opening and cheap enough to run on every candidate move: the check is O(bearings x
// hazards) with both bounded, and the hazard bound is the contract's own
// [protocol.MaxHazardsPerMove].
const escapeBearings = 16

// hazardSampleCorners is the horizontal footprint sampled when asking whether a body is
// inside a region.
//
// The centre plus the four corners, and the direction it fails in is the one to have:
// a body whose sampled points all miss a region it clips a corner of is *not* hit. Under-
// reporting costs a player nothing they were told to expect, while a test that rounded
// outward would deal damage past the boundary the client drew — which is the one thing
// this file exists to make impossible.
const hazardSampleCorners = 4

// runningMove is the one move an encounter is executing, as the server holds it.
//
// At most one at a time in this half of the encounter, which is what makes the reachable-
// safe-space rule below trivially satisfiable for the guardian and is stated as a bound
// rather than assumed: the king's overlapping rituals arrive with the rest of his
// repertoire, and the rule is written here so they inherit it rather than grow their own.
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

	// aim is the horizontal unit direction this move locked before it began, and it is
	// never revised. Every move in this half of the encounter is *aimed* rather than
	// targeted, and that is the design rather than a simplification: the contract sends a
	// direction exactly when no target is named, so a cone that named its target would
	// leave a client unable to draw where the cone actually points — and a cone that
	// followed its target would be a region nobody can leave.
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
	return move
}

// tickCooldownsLocked spends one tick of every move's cooldown.
//
// Runs whether or not a move is under way, so a heavy move's cooldown is measured from
// when it ended rather than from when the creature next had nothing to do.
func (e *bossEncounter) tickCooldownsLocked() {
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
// cooldown decides what a move costs; this decides which of the affordable ones is taken,
// and it draws no random numbers — the same encounter from the same state produces the
// same fight, which is the property every other tick path in this simulation has.
//
// A candidate that would leave its target no reachable safe space is refused and the next
// one is tried. That is the acceptance criterion executed rather than asserted: with one
// live move and a bounded opening it never fires for the guardian, and it is written at
// the scheduler rather than inside a move so the overlapping rituals cannot bypass it.
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
// telegraph rather than at its end. That is what the approved design asks for in three
// separate places — the lane fixed before the sprint, the landing region shown before the
// jump, the side of the sweep shown by the raised paw — and it is what makes every one of
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
	e.running.hazards = m.hazardsFor(def, e.running.aim, e.running.anchor)
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

// hazardsFor is the region one move announces, placed in the world.
//
// One volume per move in this half of the encounter. The vector is what both the client
// and [mob.moveReachesLocked] read, so the region drawn and the region resolved against
// are the same numbers rather than two derivations of one intention.
func (m *mob) hazardsFor(def encounterMoveDef, aim, anchor [3]float64) []protocol.HazardVolume {
	hazard := protocol.HazardVolume{
		Shape:     def.hazard.shape,
		Origin:    toWire(anchor),
		Direction: toWire(aim),
		Radius:    float32(def.announcedRadius()),
		Height:    float32(def.hazard.height),
		HalfAngle: float32(def.hazard.halfAngle),
		HalfWidth: float32(def.hazard.halfWidth),
	}
	if def.hazard.shape == vnet.HazardShapeDisc {
		// A disc reads neither, and the contract says a field a shape ignores is written
		// as zero rather than carrying a value a later shape might be given a meaning for.
		hazard.HalfAngle, hazard.HalfWidth = 0, 0
	}
	return []protocol.HazardVolume{hazard}
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
	if phase == vnet.MovePhaseRelease {
		// One permitted hit per target per window, and the window is this release. A
		// fresh ledger here rather than at the move's start is what makes that sentence
		// mean the release rather than the whole instance.
		r.hit = make(map[uint64]struct{})
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

	switch r.phase {
	case vnet.MovePhaseTelegraph:
		// Committed means stationary for everything that does not travel, and everything
		// that travels is stationary until its release: the region a player is reading is
		// not also closing the distance it is measured against.
		m.action = vnet.MobActionWindup
		m.vel[0], m.vel[2] = 0, 0
		r.remaining--
		if r.remaining == 0 {
			m.enterMovePhaseLocked(vnet.MovePhaseRelease, r.ticks.release, tick)
		}
	case vnet.MovePhaseRelease:
		m.action = vnet.MobActionWindup
		m.travelDuringReleaseLocked(s)
		s.resolveMoveDamageLocked(m, players)
		r.remaining--
		if r.remaining == 0 || r.impacted {
			recovery := r.ticks.recovery
			if r.impacted && r.ticks.impactRecovery > 0 {
				recovery = r.ticks.impactRecovery
			}
			m.enterMovePhaseLocked(vnet.MovePhaseRecovery, recovery, tick)
		}
	case vnet.MovePhaseRecovery:
		m.action = vnet.MobActionRecovery
		m.vel[0], m.vel[2] = 0, 0
		r.remaining--
		if r.remaining == 0 {
			m.finishEncounterMoveLocked(vnet.MoveEndCompleted)
			return
		}
	}
	m.publishRunningMoveLocked()
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

	reach := r.def.travelSpeed * s.dt
	if r.def.travel == travelLeap {
		// Never past the announced landing. A leap that overshot would put the creature
		// outside the region it told everybody it was going to.
		reach = min(reach, math.Hypot(r.anchor[0]-m.pos[0], r.anchor[2]-m.pos[2]))
	}
	if reach <= 0 {
		return
	}

	pos, blocked := moveAndCollide(s.terrain, m.species().body, m.pos,
		[3]float64{r.aim[0] * reach, 0, r.aim[2] * reach})
	m.pos = pos
	if r.def.travel == travelCharge && (blocked[0] || blocked[2]) {
		r.impacted = true
	}
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
// **A charge is answered by the segment it travelled this tick, not by where it ended.**
// That segment is a prefix of the announced lane by construction — same origin, same
// direction, and a length the release can cover — so a swept hit is always inside what
// the client was shown, and a player standing between two ticks' positions is hit rather
// than stepped over.
//
// **A leap resolves once, on the last tick of its release**, because the danger is the
// landing rather than the arc: the region was announced before the jump and does not
// follow anybody, so damage before the creature has arrived would be damage for standing
// where it merely passed.
func (m *mob) moveReachesLocked(p *Player) bool {
	r := m.encounter.running
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

// publishRunningMoveLocked writes the instance under way into the encounter's live moves.
//
// Replaced in place where it is already announced and appended where it is not, so an
// ending written by [finishEncounterMoveLocked] and left for encounter.go's sweep is never
// overwritten by a later announcement of the same instance.
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

// anyLivePlayerInRange reports whether this creature still has anybody to fight.
//
// The encounter's own withdrawal condition, and deliberately the aggro range rather than
// the move's band: a party that has run out of the room has ended the attack, while one
// that has merely stepped out of a cone has read it.
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
// **The schedule is refused, not the damage.** A move whose announced region — together
// with everything this encounter already has running — covers every direction a player
// could walk in during its telegraph is never announced at all, so there is no unavoidable
// window to survive. The distance sampled is exactly how far a walking player travels
// while the telegraph plays out, which is what makes "reachable" mean reachable *in time*
// rather than reachable eventually.
//
// Solid ground is part of reachable: a bearing that walks into a wall is not an escape,
// and neither is one that leaves the addressable world.
//
// The caller holds Sim.mu.
func (s *Sim) moveLeavesAnEscapeLocked(m *mob, def encounterMoveDef, target *Player) bool {
	aim := m.aimAt(target)
	candidate := m.hazardsFor(def, aim, m.hazardAnchor(def, aim, target))
	reach := WalkSpeed * def.telegraph.Seconds()

	for bearing := range escapeBearings {
		angle := 2 * math.Pi * float64(bearing) / escapeBearings
		destination := [3]float64{
			target.pos[0] + math.Cos(angle)*reach,
			target.pos[1],
			target.pos[2] + math.Sin(angle)*reach,
		}
		box := playerBox(destination)
		if box.beyondTheWorld() || anyVoxel(box, s.terrain.Solid) {
			continue
		}
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
// **Every bound but the segment comes from the announced lane itself** — its vertical
// extent, its half-width — so what this answers is always a subset of what the client was
// shown, structurally rather than by two derivations agreeing. Only the horizontal extent
// is narrowed, from the whole lane to the part of it the creature has actually crossed.
//
// That narrowing is the point: measured against the segment rather than against either
// endpoint, a player standing between two ticks' positions is hit rather than stepped
// over, which is the whole of "cannot skip a player between ticks". Using the announced
// lane directly instead would hurt everybody standing anywhere along it on the first tick
// of the run — a region the creature has not reached yet.
func sweptLaneReaches(lane protocol.HazardVolume, from, to [3]float64, b box) bool {
	origin := float64(lane.Origin[1])
	half := float64(lane.Height) / 2
	if b.max[1] <= origin-half || b.min[1] >= origin+half {
		return false
	}
	halfWidth := float64(lane.HalfWidth)

	dx, dz := to[0]-from[0], to[2]-from[2]
	lengthSquared := dx*dx + dz*dz
	for _, sample := range horizontalSamples(b) {
		px, pz := sample[0]-from[0], sample[1]-from[2]
		along := 0.0
		if lengthSquared > 0 {
			along = min(max((px*dx+pz*dz)/lengthSquared, 0), 1)
		}
		if math.Hypot(px-along*dx, pz-along*dz) <= halfWidth {
			return true
		}
	}
	return false
}
