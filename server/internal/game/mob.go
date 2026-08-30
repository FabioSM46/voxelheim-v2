package game

import (
	"math"
	"slices"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// What a creature does, whatever kind of creature it is.
//
// A mob is an *entity kind*, not a special case: it is shaped like the item drop that
// preceded it — the tick steps it, chunk visibility streams it, its identity comes from
// the same counter that names players, and nothing about it is persisted.
//
// **The intelligence here is a bounded state machine and nothing more.** It steers at
// one target, collides with the same voxels a player does, and may hop a single block.
// It is allowed to get stuck behind a wall. A*, navmeshes and pathing line of sight are
// separate systems, and a creature that already knew how to walk round a corner would
// make each of them impossible to evaluate on its own.
//
// **The one voxel the state machine does read between itself and its target is the one
// its blow would have to cross** — see [mob.inReach]. That is not the beginning of
// navigation: nothing steers around what it finds, and a creature walled out of hitting
// still walks into the wall for as long as its target stands behind it. It is the wall
// finally being worth the same to a swing as it has always been worth to a step.
//
// **One state machine, many species, and every number it reads comes from the
// registry.** A vargr is a draugr's brain with different numbers in front of it — see
// species.go, where the rows live. Nothing below compares a [vnet.MobKind] against
// anything: how far this creature notices a player, how fast it closes, how big its body
// is and how much a blow costs are all m.species(), so the next row is a row rather than
// a branch in here.
//
// **Where one comes from and when it stops existing are not here.** This file is what a
// mob *does*; spawn.go is the director that decides how many there are, where they
// appear and when the daylight or the distance takes them away. The split is the one the
// state machine already had — nothing below reads the clock, counts the population or
// knows that a player has a streamed cube.

const (
	// stepProbeFloor is the least distance past the leading face the step check looks,
	// in blocks. It only binds at rates so fine that a tick covers less than this.
	stepProbeFloor = 0.2

	// stepProbeLift is how far above the feet the "is there a step here" sample is taken.
	// The collision rests a body a hair above the face it landed on, so a sample taken at
	// exactly the feet reads the air the skin sits in rather than the block below it.
	stepProbeLift = 0.1

	// A passive creature keeps running after the player leaves its awareness radius,
	// until this wider boundary is crossed. Twelve blocks starts a deer's flight and
	// twenty-four ends it, so small movements at the edge cannot flap the state.
	passiveFleeReleaseRange = 24.0

	// Monster-hit feedback is presentation-only. Bound its retry backlog so a session
	// whose outbound queue stays full cannot retain one allocation per landed blow.
	maxPendingMobHits = 64
)

// mob is one live creature.
//
// Every field is guarded by Sim.mu. Nothing is persisted: a restart loses whatever was
// hunting, because a mob is a moment in a simulation rather than a change to the world.
type mob struct {
	entityID uint64

	// kind is the species, and it is the key into mobRegistry as well as the value the
	// wire carries. Set once, at the one place a mob is created, and never changed: a
	// creature that changed species mid-life would change body size under a collision
	// that had already resolved against the old one.
	kind vnet.MobKind

	// pos is the standing position of the mob's box — its minimum in y, its centre in x
	// and z — exactly as a player's is.
	pos [3]float64
	vel [3]float64
	yaw float64

	health uint16
	action vnet.MobAction

	// target is the player this mob has chosen, or zero for none. An identity rather
	// than a pointer: a player can leave between two ticks, and a pointer to one that
	// has left is a pointer the simulation would still step.
	target uint64

	// threat is the hostile attention this creature remembers by live player entity
	// identity. It exists only for hostile species, is touched only under Sim.mu and is
	// neither persisted nor sent; the wire carries only target above. Float64 preserves
	// half-point healing and the tenths-based worn multiplier without rounding them into
	// different combat outcomes.
	threat map[uint64]float64

	// idleThreatTicks counts a full second eligible for decay. noTargetTicks counts
	// consecutive ticks with target zero and clears the whole ledger at the forget
	// threshold. Both are simulation time and both die with this mob.
	idleThreatTicks uint32
	noTargetTicks   uint32

	// firstHit is the character who first dealt damage to this creature, or nil until
	// one does. That first valid hit taps the mob for experience: later attackers may
	// help or land the killing blow, but they cannot transfer the award to themselves.
	//
	// Account identity plus character name survive every session object. The lifetime
	// total is refreshed when that character leaves and after each offline award, so a
	// disconnect/reconnect cycle cannot make a retained tap compute from stale progress.
	// A mob reset discards this mob and therefore the tap; a fresh spawn starts untapped.
	firstHit *mobTap

	// encounter is the immutable personal-loot roster of a boss once combat begins.
	// It stores stable character identities only: no session pointer and no live party
	// object can revise an earned place after the pull.
	encounter *bossEncounter

	// actionTicks is what remains of a windup or a recovery. Ticks, not a deadline —
	// the simulation's only clock is Step.
	actionTicks uint32

	chunk    world.Coord
	onGround bool

	// unseenTicks is how long this mob has stood outside every connected player's
	// streamed cube. Reset to zero the moment anybody can see it again, and the
	// director removes it once it passes MobDespawnGrace — see spawn.go.
	unseenTicks uint32
}

// mobTap is a session-independent claim on one mob's experience.
//
// playerID alone is not enough because one account may play several characters. The
// original spelling is retained for persistence and the folded spelling is derived
// only for comparisons. experience is the last authoritative lifetime total observed
// while the character was live or received another offline award.
type mobTap struct {
	playerID      identity.PlayerID
	characterID   uint64
	characterName string
	experience    uint32
}

type bossEncounter struct {
	roster []corpseOwner
}

// ticksFor is a duration in ticks at a rate, and never zero.
//
// Milliseconds rather than seconds, because a telegraph shorter than a second is exactly
// the kind of number this has to express — and integer arithmetic throughout, so the same
// duration is the same tick count on every run. Never zero, on the rule the death
// countdown uses: a rate that rounded a telegraph away would make the attack
// unreactable rather than fast.
func ticksFor(d time.Duration, tickRate uint8) uint32 {
	return max(uint32(d.Milliseconds())*uint32(tickRate)/1000, 1)
}

// spawnMobLocked creates one full-health creature of the named species at pos, and
// reports its identity and whether the species exists at all.
//
// **The only way a mob enters this world**, and its one production caller is the spawn
// director. There is no exported placement any more: a mob exists because the dark put
// it somewhere a player is, never because the server started or because a caller outside
// the simulation asked for one.
//
// **An unregistered kind is refused rather than given default numbers**, which is what
// makes [mob.species] total for everything already in Sim.mobs: a creature with a row is
// the only kind of creature there is. It fails closed the same way the wire's zero
// MobKind does — an unknown species is a contract nobody speaks, not a draugr.
//
// The caller holds Sim.mu.
func (s *Sim) spawnMobLocked(kind vnet.MobKind, pos [3]float64) (uint64, bool) {
	def, registered := mobByKind(kind)
	if !registered {
		return 0, false
	}

	// From the counter that names players and drops, so no id ever names two things.
	// Minted under the lock here rather than before it — unlike a drop, which is created
	// from a session goroutine — because the only caller is already on the tick.
	m := &mob{
		entityID: s.mintEntityID(),
		kind:     kind,
		pos:      pos,
		health:   def.maxHealth,
		action:   vnet.MobActionIdle,
	}
	if !def.passive {
		m.threat = make(map[uint64]float64)
	}
	m.chunk = chunkAt(m.pos)
	s.mobs[m.entityID] = m
	return m.entityID, true
}

// sortedMobsLocked is every mob in identity order.
//
// Stable order is what makes the tick reproducible and a snapshot's mob vector the same
// bytes from the same state. Map order would make both differ run to run for no reason.
func (s *Sim) sortedMobsLocked() []*mob {
	mobs := make([]*mob, 0, len(s.mobs))
	for _, m := range s.mobs {
		mobs = append(mobs, m)
	}
	slices.SortFunc(mobs, func(a, b *mob) int { return compareEntityIDs(a.entityID, b.entityID) })
	return mobs
}

// advanceMobsLocked steps every mob by one tick and returns them in identity order.
//
// Runs after the players have moved, so a chase steers at the position this tick
// produced rather than the last one's — and after their swings, so a draugr killed this
// tick cannot land an attack in it. The spawn director runs after this, so what it
// decides is decided against the targets chosen here.
//
// **There is no reap here, and there is no longer anything to reap.** A killed creature
// leaves Sim.mobs inside [Sim.damageMobLocked], on the tick the blow lands, and its owned
// container is rolled there. This loop therefore steps survivors and nothing else: every
// mob it sees has health left, because the one path that takes the last of it takes the
// creature with it.
//
// It used to be the other half of a two-phase death — a killed creature stayed here in
// [vnet.MobActionDying] for MobDeathDuration and became a corpse when the countdown ran
// out — and the count was argued exact rather than approximate. What the count was
// exactly right about was how long a player had to stand over a body they had already
// earned and not be allowed to take anything from it. See constants.go for why that wait
// is gone.
//
// The caller holds Sim.mu.
func (s *Sim) advanceMobsLocked(players []*Player) []*mob {
	mobs := s.sortedMobsLocked()
	for _, m := range mobs {
		m.step(s, players)
		m.advanceThreatLocked(s)
	}
	return mobs
}

// step advances one mob by one tick, whatever species it is.
//
// There is no death branch, because a dead creature is not in Sim.mobs to be stepped: the
// blow that empties its health hands it to the corpse collection in the same call. The
// caller holds Sim.mu.
func (m *mob) step(s *Sim, players []*Player) {
	if m.species().passive {
		m.stepPassive(s.terrain, players)
	} else {
		switch m.action {
		case vnet.MobActionWindup:
			// The *committed* target, not a fresh choice. A telegraph is aimed at somebody,
			// and one that landed on whoever happened to walk nearer while it played out
			// would be unreadable — the player who reacted to it is not the one it hit.
			target := huntable(players, m.target)
			if target != nil && boxDistance(m.species().body.boxAt(m.pos), playerBox(target.pos)) > m.species().aggroRange {
				target = nil
			}
			m.stepWindup(s, target)
		case vnet.MobActionRecovery:
			m.stepRecovery(m.chooseTargetLocked(s, players))
		default:
			m.stepPursuit(s, m.chooseTargetLocked(s, players))
		}
	}

	m.physics(s)
}

// stepPassive runs the non-attacking half of the shared state machine.
//
// A live player inside the registry's awareness radius starts flight. Once fleeing, the
// wider release radius keeps it moving until it has actually escaped. Protection is not
// invisibility here: it prevents a hostile attack, but a living player is still something
// prey notices. `target` remains zero because it means prey selected for an attack in the
// hostile branch and the spawn director reads that meaning.
func (m *mob) stepPassive(t Terrain, players []*Player) {
	threat, distance := m.nearestLivePlayer(players)
	m.target = 0

	if threat == nil || (m.action == vnet.MobActionFlee && distance > passiveFleeReleaseRange) {
		m.action = vnet.MobActionIdle
		m.actionTicks = 0
		m.vel[0], m.vel[2] = 0, 0
		return
	}
	if m.action != vnet.MobActionFlee && distance > m.species().aggroRange {
		m.action = vnet.MobActionIdle
		m.actionTicks = 0
		m.vel[0], m.vel[2] = 0, 0
		return
	}

	m.action = vnet.MobActionFlee
	m.actionTicks = 0
	m.steerAway(t, threat)
}

// nearestLivePlayer is the nearest living threat and its body-to-body distance.
// Equal distances resolve by entity id, independently of caller order.
func (m *mob) nearestLivePlayer(players []*Player) (*Player, float64) {
	def := m.species()
	var best *Player
	bestDistance := math.Inf(1)
	for _, p := range players {
		if !p.alive() {
			continue
		}
		distance := boxDistance(def.body.boxAt(m.pos), playerBox(p.pos))
		if distance < bestDistance || (distance == bestDistance && (best == nil || p.entityID < best.entityID)) {
			best, bestDistance = p, distance
		}
	}
	return best, bestDistance
}

// huntable is the player with this identity, if there is one worth attacking.
//
// Returns nil for an id nobody has, for a corpse and for a freshly respawned player, so
// every caller reads "the target is gone" and "the target stopped being prey" the same
// way. O(players), on the same explicit trade as the selection below.
func huntable(players []*Player, entityID uint64) *Player {
	if entityID == 0 {
		return nil
	}
	for _, p := range players {
		if p.entityID != entityID {
			continue
		}
		if !p.alive() || p.protectionTicks > 0 {
			return nil
		}
		return p
	}
	return nil
}

// chooseTargetLocked is the player this mob is hunting, or nil.
//
// It re-chooses every tick rather than holding a player pointer, which is what makes
// losing a target free: dead, protected, disconnected and out-of-range players are not
// candidates. The ledger is the memory. Its highest positive entry wins, subject to the
// current target's tenacity; only when nobody in range has positive threat does the old
// distance / worn-weight comparison choose prey. O(players) per mob per tick, knowingly.
func (m *mob) chooseTargetLocked(s *Sim, players []*Player) *Player {
	def := m.species()

	var current, bestThreat, fallback *Player
	bestThreatValue := 0.0
	fallbackScore := math.Inf(1)

	for _, p := range players {
		// Dead players and freshly respawned ones are not prey. The second is what stops
		// a creature standing over a spawn point killing somebody the instant their
		// protection ends — it has to close again first.
		if !p.alive() || p.protectionTicks > 0 {
			continue
		}
		distance := boxDistance(def.body.boxAt(m.pos), playerBox(p.pos))
		if distance > def.aggroRange {
			continue
		}
		if p.entityID == m.target {
			current = p
		}
		if value := m.threat[p.entityID]; value > bestThreatValue ||
			(value == bestThreatValue && value > 0 && (bestThreat == nil || p.entityID < bestThreat.entityID)) {
			bestThreat, bestThreatValue = p, value
		}
		weight := 1 + float64(p.worn.threat)/ThreatScale
		score := distance / weight
		if score < fallbackScore || (score == fallbackScore && (fallback == nil || p.entityID < fallback.entityID)) {
			fallback, fallbackScore = p, score
		}
	}

	best := fallback
	if bestThreat != nil {
		best = bestThreat
		if current != nil && current != bestThreat &&
			bestThreatValue <= m.threat[current.entityID]*ThreatSwitchRatio {
			best = current
		}
	}

	m.target = 0
	if best != nil {
		s.startBossEncounterLocked(m, best)
		m.target = best.entityID
	}
	return best
}

// addThreatLocked adds positive hostile attention to this hostile, living mob.
// Passive creatures have no ledger and no caller may manufacture one for them; a killed
// one has left Sim.mobs, and the zero-health guard is what says so here rather than a
// second reading of the action.
func (m *mob) addThreatLocked(entityID uint64, amount float64) {
	if m == nil || m.threat == nil || m.health == 0 || entityID == 0 || amount <= 0 {
		return
	}
	m.threat[entityID] += amount
}

// creditDamageThreatLocked records the actual health a player's hit removed, multiplied
// by the cached worn threat weight. The cache is precisely what keeps this tick path from
// taking the inventory lock.
func (s *Sim) creditDamageThreatLocked(m *mob, player *Player, damage uint16) {
	if player == nil || damage == 0 {
		return
	}
	weight := 1 + float64(player.worn.threat)/ThreatScale
	m.addThreatLocked(player.entityID, float64(damage)*weight)
}

// creditHealThreatLocked gives a healer half the health actually restored on every mob
// currently hunting the healed player. A future sceptre calls this once after clamping
// the heal; overhealing therefore arrives as restored zero and earns nothing.
func (s *Sim) creditHealThreatLocked(healer, healed *Player, restored uint16) {
	if healer == nil || healed == nil || restored == 0 {
		return
	}
	for _, m := range s.mobs {
		if m.target == healed.entityID {
			m.addThreatLocked(healer.entityID, float64(restored)/2)
		}
	}
}

// creditBlockThreatLocked records the fixed taunt earned when blocker authoritatively
// absorbed this mob's landed blow. The shield issue owns the block verdict and calls this
// seam only after one succeeds.
func (s *Sim) creditBlockThreatLocked(blocker *Player, m *mob) {
	if blocker == nil {
		return
	}
	m.addThreatLocked(blocker.entityID, ShieldTauntThreat)
}

// removeAllThreatFor erases one session identity from every hostile ledger and drops any
// active hunt for it. Death and Leave both call it under Sim.mu, so even a mob whose blow
// caused the death cannot project the invalid target later in the same tick.
func (s *Sim) removeAllThreatFor(entityID uint64) {
	if entityID == 0 {
		return
	}
	for _, m := range s.mobs {
		delete(m.threat, entityID)
		if m.target == entityID {
			m.target = 0
		}
	}
}

// advanceThreatLocked spends one tick of decay and forget time after the mob has chosen
// this tick's action and target. Combat resets the decay interval; a complete idle second
// subtracts once, and ten consecutive target-less seconds discard the ledger whole.
func (m *mob) advanceThreatLocked(s *Sim) {
	if m.threat == nil || m.health == 0 {
		return
	}

	if m.target == 0 {
		m.noTargetTicks++
		if m.noTargetTicks >= s.threatForgetTicks {
			clear(m.threat)
			m.noTargetTicks = 0
		}
	} else {
		m.noTargetTicks = 0
	}

	switch m.action {
	case vnet.MobActionChase, vnet.MobActionWindup, vnet.MobActionRecovery:
		m.idleThreatTicks = 0
		return
	}

	m.idleThreatTicks++
	if m.idleThreatTicks < s.threatDecayTicks {
		return
	}
	m.idleThreatTicks = 0
	for entityID, value := range m.threat {
		value -= ThreatDecayPerSecond
		if value <= 0 {
			delete(m.threat, entityID)
			continue
		}
		m.threat[entityID] = value
	}
}

// stepPursuit is Idle and Chase, which are the same decision asked of a different answer.
func (m *mob) stepPursuit(s *Sim, target *Player) {
	if target == nil {
		m.action = vnet.MobActionIdle
		m.vel[0], m.vel[2] = 0, 0
		return
	}

	m.action = vnet.MobActionChase
	if m.inReach(s.terrain, target) {
		m.beginWindup(s)
		return
	}
	m.steerToward(s.terrain, target)
}

// stepWindup counts down a committed swing and lands it, or abandons it.
func (m *mob) stepWindup(s *Sim, target *Player) {
	// Committed means stationary: a telegraph a player can read is one that is not also
	// closing the distance it is measured against.
	m.vel[0], m.vel[2] = 0, 0

	if target == nil || !m.inReach(s.terrain, target) {
		// Lost before it landed — walked out of range, gone from the world, or gone
		// behind a block somebody placed while the telegraph played out. No damage, and
		// no recovery either: recovery is what an attack costs, and this was not one.
		m.action = vnet.MobActionIdle
		m.actionTicks = 0
		if target == nil {
			m.target = 0
		}
		if target != nil {
			m.action = vnet.MobActionChase
		}
		return
	}

	m.faceToward(target)
	// Decremented before it is tested, so a swing committed on tick N lands on tick
	// N + mobWindup exactly. Testing the counter before spending it is the difference
	// between a telegraph and a telegraph plus one tick — the same off-by-one the death
	// countdown has to avoid.
	m.actionTicks--
	if m.actionTicks > 0 {
		return
	}

	// Armour applies here rather than in damageLocked: a mob's blow is softened, while
	// fall damage remains absolute and continues through the unchanged common funnel.
	// Widen before multiplying so a future larger damage value cannot overflow uint16.
	rawDamage := m.species().damage
	damage := uint16(uint32(rawDamage) * uint32(ArmourScale-target.worn.armour) / uint32(ArmourScale))
	blocked := target.blocking && target.wornShield.fraction > 0 && shieldFacesMob(target, m)
	if blocked {
		damage = uint16(uint32(damage) * uint32(100-target.wornShield.fraction) / 100)
		target.spendShieldDurabilityLocked()
		s.creditBlockThreatLocked(target, m)
	}
	if rawDamage != 0 && damage == 0 {
		// A blow that connects always lands for something, even at the exact 100% test
		// boundary. Production combinations stay below it by the registry sweep.
		damage = 1
	}
	if target.damageLocked(damage) {
		target.recordMobHitLocked(protocol.MobHit{
			AttackerEntityID: m.entityID,
			AttackerPos:      toWire(m.pos),
		})
	}
	// Every attack pays recovery, landed or not, which is what stops a low tick rate or
	// a target dancing on the edge of reach from raising the authoritative cadence.
	m.action = vnet.MobActionRecovery
	m.actionTicks = s.mobTimings[m.kind].recovery
}

// shieldFacesMob tests the guard's horizontal front half-plane, ignoring pitch.
func shieldFacesMob(target *Player, m *mob) bool {
	look := lookDirection(target.yaw, 0)
	toMobX := m.pos[0] - target.pos[0]
	toMobZ := m.pos[2] - target.pos[2]
	return look[0]*toMobX+look[2]*toMobZ >= 0
}

// recordMobHitLocked retains the newest presentation events up to a fixed bound. Dropping
// the oldest event under prolonged congestion cannot change authoritative health; keeping
// the newest gives the client the most relevant direction once delivery resumes.
//
// The caller holds Sim.mu.
func (p *Player) recordMobHitLocked(hit protocol.MobHit) {
	if len(p.pendingMobHits) == maxPendingMobHits {
		copy(p.pendingMobHits, p.pendingMobHits[1:])
		p.pendingMobHits[len(p.pendingMobHits)-1] = hit
		return
	}
	p.pendingMobHits = append(p.pendingMobHits, hit)
}

// offerMobHitsLocked delivers landed monster-hit events in impact order and keeps the
// first rejected frame (and everything behind it) pending for a later tick.
//
// The call is made before this recipient's superseding snapshot. deliver is the
// non-blocking session seam, so a full queue delays presentation without ever delaying
// the simulation tick or turning a rejected enqueue into success.
func (p *Player) offerMobHitsLocked() {
	for len(p.pendingMobHits) > 0 {
		if !p.deliver(protocol.EncodeMobHit(p.pendingMobHits[0])) {
			p.sim.log.Debug("monster-hit feedback deferred: the session's outbound queue is full",
				"entity_id", p.entityID)
			return
		}
		p.pendingMobHits = p.pendingMobHits[1:]
	}
}

// stepRecovery counts down the pause after a swing.
func (m *mob) stepRecovery(target *Player) {
	m.vel[0], m.vel[2] = 0, 0
	m.actionTicks--
	if m.actionTicks > 0 {
		return
	}

	if target == nil {
		m.action = vnet.MobActionIdle
		return
	}
	m.action = vnet.MobActionChase
}

// beginWindup commits to a swing.
func (m *mob) beginWindup(s *Sim) {
	m.action = vnet.MobActionWindup
	m.actionTicks = s.mobTimings[m.kind].windup
	m.vel[0], m.vel[2] = 0, 0
}

// inReach reports whether a target is close enough to be hit and whether the blow has
// anywhere to travel.
//
// **Two questions in one function because there is no caller for either half alone.**
// A swing is committed on this answer and landed on it again a few ticks later, and a
// caller that could ask only the distance is a caller that hits through a wall — which
// is exactly what this used to be. Distance first: it is arithmetic on two boxes, and
// the traversal below is the one that reads voxels.
//
// **Centre to centre, one line.** It is the segment a blow would have to cross, and a
// single line is the whole of the claim being made — a creature whose centre can see a
// player's centre swings, and one whose cannot does not. Sampling a body's corners as
// well would be a different rule (a shoulder past a doorframe is a hit), and the
// navigation this shares a file with is straight-line for the same reason: see
// [mob.steerToward], where a creature is allowed to be walled out by a corner. It is
// now walled out of hitting by the same corner, which is the outcome the wall was
// already producing for movement and never produced for damage.
func (m *mob) inReach(t Terrain, target *Player) bool {
	def := m.species()
	body := def.body.boxAt(m.pos)
	if boxDistance(body, playerBox(target.pos)) > def.attackRange {
		return false
	}
	return clearLineOfSight(t, boxCentre(body), boxCentre(playerBox(target.pos)))
}

// speedIn is how fast this creature may travel horizontally from where it is standing.
//
// **The registry's speed, capped by [SwimSpeed] while the body overlaps water — a cap
// and not a scale**, which is the same shape [Player.step] gives the same question and
// is the reason it is worth restating here. A scale would be a second answer: written
// as `speed *= something`, a future modifier (a wound, a mire, a hunted creature's last
// burst) would multiply with the water instead of being bounded by it, and a creature
// slowed twice over would end up slower in a river than the river alone allows. A cap
// composes — whatever else has already reduced the speed, water says only "and no
// faster than this", and a creature already slower than the water keeps its own number.
//
// One box query per tick, through the same [Terrain] seam the collision reads, with
// **this species' own body** from the registry rather than a box spelled here — the
// same box the swing, the separation and the step probe all measure. An absent chunk
// answers "not water" (see [Terrain.Fluid]), so a creature at the edge of loaded
// terrain runs at its land speed, which is the existing conservative direction.
//
// Nothing is stored: the answer is recomputed from the body's position every tick, so a
// creature that walks out of the water is back at its full speed on the tick its box no
// longer overlaps any, with nothing to decay and nothing left behind.
func (m *mob) speedIn(t Terrain) float64 {
	def := m.species()
	if overlapsFluid(t, def.body.boxAt(m.pos)) {
		return min(def.speed, SwimSpeed)
	}
	return def.speed
}

// steerToward walks straight at a target and faces it.
//
// Straight, and that is the whole of the navigation: it is why a creature can be walled
// out by a corner. The hop below is the one concession, and it clears a single block —
// the height a player steps up without thinking about it.
func (m *mob) steerToward(t Terrain, target *Player) {
	dx, dz := target.pos[0]-m.pos[0], target.pos[2]-m.pos[2]
	length := math.Hypot(dx, dz)
	if length == 0 {
		m.vel[0], m.vel[2] = 0, 0
		return
	}

	speed := m.speedIn(t)
	m.vel[0] = dx / length * speed
	m.vel[2] = dz / length * speed
	m.faceToward(target)
}

// steerAway walks directly opposite a live player's position and faces that heading.
func (m *mob) steerAway(t Terrain, threat *Player) {
	dx, dz := m.pos[0]-threat.pos[0], m.pos[2]-threat.pos[2]
	length := math.Hypot(dx, dz)
	if length == 0 {
		m.vel[0], m.vel[2] = 0, 0
		return
	}

	speed := m.speedIn(t)
	m.vel[0] = dx / length * speed
	m.vel[2] = dz / length * speed
	m.yaw = wrapAngle(math.Atan2(-dx, -dz))
}

// faceToward points the mob at a target.
//
// The same basis the player integrator uses — yaw 0 looks along -Z and +X is to its
// right — so a client drawing a creature and a player with one convention is correct for
// both. See client/src/player/constants.rs.
func (m *mob) faceToward(target *Player) {
	dx, dz := target.pos[0]-m.pos[0], target.pos[2]-m.pos[2]
	if dx == 0 && dz == 0 {
		return
	}
	m.yaw = wrapAngle(math.Atan2(-dx, -dz))
}

// physics falls the mob, moves it, and hops it over a one-block step.
//
// **Nothing *vertical* here knows about water, and that is still the decision worldgen
// 5 made rather than a gap it left.** A creature that walks into a lake sinks through
// it — water does not stop movement — and walks along the bed until it walks out
// again. The player integrator learned to float, sink and rise because a player's
// *intent* is the thing those rules answer; a mob has a path and no intent, so
// teaching it to swim means teaching the pathing to want to be at the surface, which
// is a different piece of work. What keeps the result from being creatures standing in
// ponds is upstream: the spawn director refuses a spot whose floor or headroom is
// water or ice, so a mob only ever reaches a lake by walking there.
//
// **The horizontal half is no longer nothing**, and it is deliberately not here: the
// cap is applied where the registry's speed becomes a velocity, in [mob.speedIn], so
// that water bounds the creature's *intended* speed the way it bounds a player's
// rather than editing a velocity after the fact. This function reads whatever the
// steering left behind and is unchanged by it — including the step probe below, which
// looks a tick's travel ahead and therefore looks proportionally less far ahead for a
// creature the water has slowed.
func (m *mob) physics(s *Sim) {
	m.vel[1] = max(m.vel[1]-Gravity*s.dt, -TerminalFallSpeed)

	delta := [3]float64{m.vel[0] * s.dt, m.vel[1] * s.dt, m.vel[2] * s.dt}
	pos, blocked := moveAndCollide(s.terrain, m.species().body, m.pos, delta)
	m.pos = pos
	m.onGround = blocked[1] && delta[1] <= 0

	for axis := range 3 {
		if blocked[axis] {
			m.vel[axis] = 0
		}
	}

	// The step up, asked after the zeroing — and that placement is the fix, not the
	// style. Standing on the ground is exactly what makes blocked[1] true, so an impulse
	// written before the loop is cancelled on the tick it was given, and the creature
	// walks into the step for ever.
	//
	// Reading the velocity that survived the loop is deliberate too. The probe below
	// looks exactly one tick's travel ahead, so a step in the path is always seen the
	// tick *before* the body would reach it — a horizontal axis that is blocked here is
	// therefore a wall rather than a step, and sliding along it is the right answer.
	if m.onGround && m.stepsUp(s.terrain, [2]float64{m.vel[0], m.vel[2]}, s.dt) {
		m.vel[1] = JumpImpulse
	}
	m.chunk = chunkAt(m.pos)
}

// stepsUp reports whether a single block stands in the mob's path with room above it.
//
// The whole of the navigation, and deliberately the whole: one block is what a player
// steps over without deciding to, so it is what a creature clears. Two is a wall, and
// being stuck behind a wall is a state this design allows — pathfinding is a separate
// system and half of one hidden here would make it impossible to evaluate on its own.
//
// A non-generating read, on the tick, by contract. An absent chunk answers solid to both
// questions below, so the answer at the edge of loaded terrain is "do not hop" — the
// conservative one, and free.
func (m *mob) stepsUp(t Terrain, heading [2]float64, dt float64) bool {
	speed := math.Hypot(heading[0], heading[1])
	if speed == 0 {
		return false
	}

	// Past the leading face by however far this tick's move would carry it — which is
	// what makes the answer the same at every tick rate. A fixed distance is a detection
	// window measured in blocks against a stride measured in blocks per tick, so a
	// coarse enough rate steps straight over it: at 10 Hz the draugr covers 0.32 blocks
	// a tick and a 0.2-block window is jumped clean, leaving it stuck against a step it
	// clears perfectly at 20 Hz. Measured, not reasoned about — 5, 10 and 15 Hz all
	// failed and 20, 30 and 60 all passed.
	//
	// The floor only binds at rates fine enough that a tick covers less than it, where
	// the body needs *some* look-ahead to see the column it is arriving at.
	//
	// The width is this species', so a wider creature looks further past its own face
	// for the step it is about to arrive at.
	shape := m.species().body
	reach := shape.width/2 + max(speed*dt, stepProbeFloor)
	ahead := [3]float64{
		m.pos[0] + heading[0]/speed*reach,
		m.pos[1],
		m.pos[2] + heading[1]/speed*reach,
	}

	x := int64(math.Floor(ahead[0]))
	z := int64(math.Floor(ahead[2]))
	feet := int64(math.Floor(ahead[1] + stepProbeLift))

	// Solid at the feet, and clear for the whole body one block higher. The height is
	// read from the registry rather than assumed, so a taller creature needs a taller
	// gap and a shorter one fits under a ceiling the taller one does not.
	//
	// **Rounded up rather than truncated-and-incremented**, which is the same answer for
	// a body 1.8 blocks tall and not for one that is exactly a block. A body standing at
	// feet+1 occupies the half-open span [feet+1, feet+1+height), so the last cell it is
	// in is feet+ceil(height): the old form spent an extra cell on any species whose
	// height was a whole number, and refused a vargr a step it fits over.
	if !t.Solid(x, feet, z) {
		return false
	}
	for above := feet + 1; above <= feet+int64(math.Ceil(shape.height)); above++ {
		if t.Solid(x, above, z) {
			return false
		}
	}
	return true
}

// damageMobLocked takes health from a mob, turns it into a corpse if it runs out, and
// reports whether this blow caused that transition.
//
// The one path a mob loses health by, for the reason damageLocked is the one path a
// player does: one place clamps, one place decides what death means, and there is one
// answer rather than one per caller. **That is also what makes a kill the only thing that
// ever produces loot** — a creature the director takes away never comes through here and
// so never reaches [Sim.makeCorpseLocked], which is the whole of "a despawn leaves
// nothing".
//
// **Species-agnostic, deliberately.** Nothing here asks what it is hitting: a blow is
// worth what the blade is worth, and what it kills is whatever ran out of health. The
// only per-species numbers in a fight are the health the creature started with — set once,
// at the one place a creature is created — and the table it leaves behind, which is read
// through the registry by rollLootLocked rather than named here.
//
// **The corpse exists before this returns.** The creature leaves Sim.mobs and its owned
// container is rolled here, on the tick of the blow, at the position the blow landed on.
// Everything the rest of the tick does — the mob step, the director, the snapshot
// projection, offerLootLocked — therefore already sees a corpse, so the first snapshot
// that draws the body draws it as [vnet.MobActionCorpse] and, for whoever it belongs to,
// lists it as accessible loot in the same frame.
//
// **It used to take two and a half seconds to get here, and that wait is what #441
// removed.** A kill put the creature into [vnet.MobActionDying] with a countdown and left
// it in Sim.mobs; [Sim.advanceMobsLocked] reaped it when the countdown ran out and rolled
// the container then, at the position the body had fallen to. Two things were given up
// with the countdown and both were deliberate: a body killed on a ledge no longer slides
// off it before the loot is placed — the corpse is where the blow landed — and the server
// no longer emits [vnet.MobActionDying] at all. The fall is still drawn, by the client,
// off the same snapshot that says Corpse; what is gone is the simulation waiting for an
// animation it does not play.
//
// **Nothing is scheduled to replace it.** A kill used to start a countdown to a fresh
// draugr at the same anchor, which made killing one a way of moving it rather than of
// removing it. What refills the world now is the director, and only where a player actually
// is — see spawn.go.
//
// A second blow lands on nothing: zero health is the guard at the top, and a corpse is not
// in Sim.mobs at all, so swingTargetLocked cannot pick one as a target.
//
// The caller holds Sim.mu.
func (s *Sim) damageMobLocked(m *mob, amount uint16) bool {
	if amount == 0 || m.health == 0 {
		return false
	}

	if amount >= m.health {
		m.health = 0
		// The hunt ends with the creature. Cleared rather than left pointing at whoever it
		// was chasing, so nothing that still holds a pointer to this struct — the mob slice
		// the projectile pass captured before the blow, above all — reads it as hunting.
		m.target = 0
		clear(m.threat)
		m.idleThreatTicks = 0
		m.noTargetTicks = 0
		m.vel[0], m.vel[2] = 0, 0
		delete(s.mobs, m.entityID)
		corpse := s.makeCorpseLocked(m)
		s.log.Debug("mob died", "entity_id", m.entityID, "kind", m.kind,
			"pos", m.pos, "loot_entries", corpse.entryCount())
		return true
	}
	m.health -= amount
	if m.species().passive {
		m.action = vnet.MobActionFlee
		m.actionTicks = 0
		m.target = 0
	}
	return false
}

// mobStates is the wire form of every mob, in the order it was given.
func mobStates(mobs []*mob) []protocol.MobState {
	states := make([]protocol.MobState, len(mobs))
	for i, m := range mobs {
		states[i] = protocol.MobState{
			EntityID: m.entityID,
			Kind:     m.kind,
			Pos:      toWire(m.pos),
			Vel:      toWire(m.vel),
			Yaw:      float32(m.yaw),
			Health:   m.health,
			// From the registry row rather than from a constant, so a client's health
			// bar is drawn against the maximum of the species it is looking at. The
			// kind travels beside it, which is what lets the client size the body it
			// draws — the numbers themselves stay here.
			MaxHealth:      m.species().maxHealth,
			Action:         m.action,
			TargetEntityID: m.target,
		}
	}
	return states
}
