package game

import (
	"math"
	"slices"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

var projectileBody = body{width: ProjectileBodySize, height: ProjectileBodySize}

// projectileOriginLocked is the server's firing point: the centre of the owner's
// footprint raised to the eye height. The caller holds Sim.mu.
func projectileOriginLocked(owner *Player) [3]float64 {
	origin := boxCentre(playerBox(owner.pos))
	origin[1] = owner.pos[1] + ProjectileEyeHeight
	return origin
}

// projectile is one transient thing in flight. Every field is guarded by Sim.mu.
// Position uses the same standing-position convention as every other colliding body;
// on the wire it is the projectile's authoritative reference point without correction.
type projectile struct {
	entityID  uint64
	kind      vnet.ProjectileKind
	owner     uint64
	pos       [3]float64
	vel       [3]float64
	ticksLeft uint32
	stuck     bool
}

// spawnProjectileLocked creates one authoritative projectile. The caller holds Sim.mu.
//
// origin is the server-derived eye position and direction is the accepted aim. The
// projectile is nudged along that direction until its small body is outside the
// shooter's body, so the first sweep can never intersect the body it started inside.
func (s *Sim) spawnProjectileLocked(kind vnet.ProjectileKind, owner *Player, origin, direction [3]float64, speed float64) (uint64, bool) {
	if owner == nil || !s.onlineLocked(owner) || !owner.alive() || speed <= 0 ||
		math.IsNaN(speed) || math.IsInf(speed, 0) ||
		!finiteVec(origin) || !finiteVec(direction) || !isKnownProjectileKind(kind) {
		return 0, false
	}

	length := vectorLength(direction)
	if length == 0 || math.IsInf(length, 0) || math.IsNaN(length) {
		return 0, false
	}
	velocity := [3]float64{direction[0] * speed, direction[1] * speed, direction[2] * speed}
	if !finiteVec(velocity) {
		return 0, false
	}

	pos := origin
	unit := [3]float64{direction[0] / length, direction[1] / length, direction[2] / length}
	// The longest route out is straight down from eye height. This bounded walk covers
	// twice a player height and advances by half the projectile edge, so it cannot leave
	// a valid finite origin inside the owner after the loop.
	for range int(math.Ceil(PlayerHeight*2/(ProjectileBodySize/2))) + 1 {
		if !boxesIntersect(projectileBody.boxAt(pos), playerBox(owner.pos)) {
			break
		}
		for axis := range 3 {
			pos[axis] += unit[axis] * (ProjectileBodySize / 2)
		}
	}
	if boxesIntersect(projectileBody.boxAt(pos), playerBox(owner.pos)) {
		return 0, false
	}

	ticksLeft := s.orbLifetimeTicks
	if kind == vnet.ProjectileKindArrow {
		ticksLeft = s.arrowLifetimeTicks
	}
	proj := &projectile{
		entityID:  s.mintEntityID(),
		kind:      kind,
		owner:     owner.entityID,
		pos:       pos,
		vel:       velocity,
		ticksLeft: ticksLeft,
	}
	s.projectiles[proj.entityID] = proj
	// Retaining the immutable identity-bearing session object is what lets a shot that
	// lands after Leave establish the same stable tap a melee blow does. It is released
	// with the projectile and is never persisted.
	s.projectileOwners[proj.entityID] = owner
	return proj.entityID, true
}

func isKnownProjectileKind(kind vnet.ProjectileKind) bool {
	return kind == vnet.ProjectileKindArrow || kind == vnet.ProjectileKindEnergyOrb
}

func finiteVec(v [3]float64) bool {
	for _, component := range v {
		if math.IsNaN(component) || math.IsInf(component, 0) {
			return false
		}
	}
	return true
}

func vectorLength(v [3]float64) float64 {
	return math.Sqrt(v[0]*v[0] + v[1]*v[1] + v[2]*v[2])
}

func boxesIntersect(a, b box) bool {
	for axis := range 3 {
		if a.max[axis] <= b.min[axis] || a.min[axis] >= b.max[axis] {
			return false
		}
	}
	return true
}

func (s *Sim) sortedProjectilesLocked() []*projectile {
	projectiles := make([]*projectile, 0, len(s.projectiles))
	for _, proj := range s.projectiles {
		projectiles = append(projectiles, proj)
	}
	slices.SortFunc(projectiles, func(a, b *projectile) int {
		return compareEntityIDs(a.entityID, b.entityID)
	})
	return projectiles
}

// advanceProjectilesLocked steps every projectile after swings and before mobs. The
// returned slice contains the survivors in entity-id order. The caller holds Sim.mu.
func (s *Sim) advanceProjectilesLocked(players []*Player) []*projectile {
	projectiles := s.sortedProjectilesLocked()
	kept := projectiles[:0]
	for _, proj := range projectiles {
		if proj.stuck {
			if proj.ticksLeft > 0 {
				proj.ticksLeft--
			}
			if proj.ticksLeft == 0 {
				s.removeProjectileLocked(proj)
				continue
			}
			kept = append(kept, proj)
			continue
		}

		// Expiry is counted by authoritative ticks. The last tick may complete its move,
		// but an expired id is absent from the snapshot that tick produces.
		if proj.ticksLeft > 0 {
			proj.ticksLeft--
		}

		// A non-resident chunk is a hold, never air. Unlike an arrow's gravitational
		// acceleration, its existing velocity is retained; an orb's velocity therefore
		// remains unchanged over the whole flight as the contract promises.
		voxel := voxelAt(proj.pos)
		if _, resident := s.terrain.Block(voxel[0], voxel[1], voxel[2]); resident {
			if s.stepProjectileLocked(proj, players) {
				continue
			}
		}

		if proj.ticksLeft == 0 {
			s.removeProjectileLocked(proj)
			continue
		}
		kept = append(kept, proj)
	}
	return kept
}

// stepProjectileLocked returns true when the projectile ended during this tick.
func (s *Sim) stepProjectileLocked(proj *projectile, players []*Player) bool {
	endVelocity := proj.vel
	if proj.kind == vnet.ProjectileKindArrow {
		endVelocity[1] = max(endVelocity[1]-Gravity*s.dt, -TerminalFallSpeed)
	}
	// Acceleration is linear within the tick, so speed is greatest at one of its two
	// endpoints. Using the larger endpoint keeps even a 1 Hz upward shot below the
	// half-block cap; sizing from end velocity alone would under-substep while gravity
	// was slowing it.
	maxSpeed := max(vectorLength(proj.vel), vectorLength(endVelocity))
	steps := max(int(math.Ceil(maxSpeed*s.dt/ProjectileMaxStep)), 1)
	subDT := s.dt / float64(steps)

	for range steps {
		if proj.kind == vnet.ProjectileKindArrow {
			proj.vel[1] = max(proj.vel[1]-Gravity*subDT, -TerminalFallSpeed)
		}
		delta := [3]float64{proj.vel[0] * subDT, proj.vel[1] * subDT, proj.vel[2] * subDT}
		from := proj.pos
		moved, blocked := moveAndCollide(s.terrain, projectileBody, from, delta)
		if blocked != [3]bool{} {
			moved = firstTerrainContact(from, delta, moved, blocked)
		}

		if target := s.firstProjectileTargetLocked(proj, players, from, moved); target != nil {
			if hit, ok := segmentBoxIntersection(from, moved, projectileTargetBox(target)); ok {
				proj.pos = pointOnSegment(from, moved, hit)
			} else {
				proj.pos = moved
			}
			s.onProjectileHitLocked(proj, target)
			s.removeProjectileLocked(proj)
			return true
		}

		proj.pos = moved
		if blocked != [3]bool{} {
			if proj.kind == vnet.ProjectileKindArrow {
				proj.vel = [3]float64{}
				proj.stuck = true
				proj.ticksLeft = s.arrowStuckTicks
				return false
			}
			s.removeProjectileLocked(proj)
			return true
		}
	}
	return false
}

// firstTerrainContact turns moveAndCollide's axis-resolved result into the first point
// on a projectile's straight sub-step. Players slide along a free axis; a projectile
// ends its flight at the first blocked face instead.
func firstTerrainContact(from, delta, resolved [3]float64, blocked [3]bool) [3]float64 {
	at := 1.0
	for axis := range 3 {
		if !blocked[axis] || delta[axis] == 0 {
			continue
		}
		axisAt := (resolved[axis] - from[axis]) / delta[axis]
		at = min(at, max(axisAt, 0))
	}
	to := [3]float64{from[0] + delta[0], from[1] + delta[1], from[2] + delta[2]}
	return pointOnSegment(from, to, at)
}

// firstProjectileTargetLocked finds the earliest body crossing on one actually
// travelled sub-step. Entity identity breaks exact-distance ties deterministically.
func (s *Sim) firstProjectileTargetLocked(proj *projectile, players []*Player, from, to [3]float64) any {
	var best any
	bestAt := math.Inf(1)
	bestID := ^uint64(0)

	consider := func(target any, entityID uint64, targetBox box) {
		at, hit := segmentBoxIntersection(from, to, targetBox)
		if !hit || at > bestAt || (at == bestAt && entityID >= bestID) {
			return
		}
		best, bestAt, bestID = target, at, entityID
	}

	for _, m := range s.sortedMobsLocked() {
		if m.dying() || m.health == 0 {
			continue
		}
		consider(m, m.entityID, m.species().body.boxAt(m.pos))
	}
	if proj.kind == vnet.ProjectileKindEnergyOrb {
		for _, player := range players {
			if player.entityID == proj.owner || !player.alive() {
				continue
			}
			consider(player, player.entityID, playerBox(player.pos))
		}
	}
	return best
}

func projectileTargetBox(target any) box {
	switch target := target.(type) {
	case *mob:
		return target.species().body.boxAt(target.pos)
	case *Player:
		return playerBox(target.pos)
	default:
		return box{}
	}
}

// segmentBoxIntersection is the slab test for p(t)=from+(to-from)t, t in [0,1].
func segmentBoxIntersection(from, to [3]float64, target box) (float64, bool) {
	near, far := 0.0, 1.0
	for axis := range 3 {
		delta := to[axis] - from[axis]
		if delta == 0 {
			if from[axis] < target.min[axis] || from[axis] > target.max[axis] {
				return 0, false
			}
			continue
		}
		a := (target.min[axis] - from[axis]) / delta
		b := (target.max[axis] - from[axis]) / delta
		if a > b {
			a, b = b, a
		}
		near = max(near, a)
		far = min(far, b)
		if near > far {
			return 0, false
		}
	}
	return near, near >= 0 && near <= 1
}

func pointOnSegment(from, to [3]float64, at float64) [3]float64 {
	return [3]float64{
		from[0] + (to[0]-from[0])*at,
		from[1] + (to[1]-from[1])*at,
		from[2] + (to[2]-from[2])*at,
	}
}

// onProjectileHitLocked is the effect dispatch later items share. Unknown combinations
// are intentionally a no-op; adding a kind does not inherit an existing kind's effect.
func (s *Sim) onProjectileHitLocked(proj *projectile, target any) {
	owner := s.projectileOwners[proj.entityID]
	switch proj.kind {
	case vnet.ProjectileKindArrow:
		if target, ok := target.(*mob); ok {
			s.creditMobDamageLocked(owner, target, ArrowDamage)
		}
	case vnet.ProjectileKindEnergyOrb:
		switch target := target.(type) {
		case *mob:
			s.creditMobDamageLocked(owner, target, OrbDamage)
		case *Player:
			restored := target.healLocked(OrbHeal)
			if s.onlineLocked(owner) {
				s.creditHealThreatLocked(owner, target, restored)
			}
		}
	}
}

func (s *Sim) removeProjectileLocked(proj *projectile) {
	delete(s.projectiles, proj.entityID)
	delete(s.projectileOwners, proj.entityID)
}

func projectileStates(projectiles []*projectile) []protocol.ProjectileState {
	states := make([]protocol.ProjectileState, len(projectiles))
	for i, proj := range projectiles {
		states[i] = protocol.ProjectileState{
			EntityID: proj.entityID,
			Kind:     proj.kind,
			Pos:      toWire(proj.pos),
			Vel:      toWire(proj.vel),
		}
	}
	return states
}
