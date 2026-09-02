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
	origin := boxCentre(owner.box())
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
	if owner == nil || !s.onlineLocked(owner) || !owner.alive() || speed <= 0 || speed > ProjectileMaxLaunchSpeed ||
		math.IsNaN(speed) || math.IsInf(speed, 0) ||
		!finiteVec(origin) || !finiteVec(direction) || !isKnownProjectileKind(kind) {
		return 0, false
	}

	length := vectorLength(direction)
	if length == 0 || math.IsInf(length, 0) || math.IsNaN(length) {
		return 0, false
	}
	unit := [3]float64{direction[0] / length, direction[1] / length, direction[2] / length}
	velocity := [3]float64{unit[0] * speed, unit[1] * speed, unit[2] * speed}
	if !finiteVec(velocity) {
		return 0, false
	}

	pos := origin
	// The longest route out is straight down from eye height. This bounded walk covers
	// twice the owner's body height — the mounted body's while a horse is under them —
	// and advances by half the projectile edge, so it cannot leave a valid finite origin
	// inside the owner after the loop.
	ownerBox := owner.box()
	for range int(math.Ceil(owner.body().height*2/(ProjectileBodySize/2))) + 1 {
		if !boxesIntersect(projectileBody.boxAt(pos), ownerBox) {
			break
		}
		for axis := range 3 {
			pos[axis] += unit[axis] * (ProjectileBodySize / 2)
		}
	}
	if boxesIntersect(projectileBody.boxAt(pos), ownerBox) {
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
	mobs := s.sortedMobsLocked()
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
		expiresThisTick := proj.ticksLeft <= 1
		if proj.ticksLeft > 0 {
			proj.ticksLeft--
		}

		if s.stepProjectileLocked(proj, players, mobs) {
			continue
		}

		// A terrain hit on the final flight tick may turn an arrow into a stuck arrow,
		// but expiry still wins: that id must be absent from this tick's snapshot.
		if expiresThisTick {
			s.removeProjectileLocked(proj)
			continue
		}
		kept = append(kept, proj)
	}
	return kept
}

// stepProjectileLocked returns true when the projectile ended during this tick.
func (s *Sim) stepProjectileLocked(proj *projectile, players []*Player, mobs []*mob) bool {
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
		velocityBeforeStep := proj.vel
		if proj.kind == vnet.ProjectileKindArrow {
			proj.vel[1] = max(proj.vel[1]-Gravity*subDT, -TerminalFallSpeed)
		}
		delta := [3]float64{proj.vel[0] * subDT, proj.vel[1] * subDT, proj.vel[2] * subDT}
		from := proj.pos
		to := [3]float64{from[0] + delta[0], from[1] + delta[1], from[2] + delta[2]}
		// A non-resident chunk is a hold, never terrain and never air. Preflight the
		// whole swept body before collision so crossing a chunk boundary cannot make an
		// arrow stick or an orb disappear. Gravity for the untravelled sub-step is undone.
		if !projectilePathResident(s.terrain, from, to) {
			proj.vel = velocityBeforeStep
			return false
		}
		moved, blocked := moveAndCollide(s.terrain, projectileBody, from, delta)
		if blocked != [3]bool{} {
			moved = firstTerrainContact(from, delta, moved, blocked)
		}

		if target := s.firstProjectileTargetLocked(proj, players, mobs, from, moved); target != nil {
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

// projectilePathResident reports whether every voxel the moving projectile body can
// touch is available to this tick. Expanding each candidate voxel by the projectile
// body turns the swept-body question into the same segment/box slab test used for hits.
func projectilePathResident(t Terrain, from, to [3]float64) bool {
	fromBox, toBox := projectileBody.boxAt(from), projectileBody.boxAt(to)
	swept := box{}
	for axis := range 3 {
		swept.min[axis] = min(fromBox.min[axis], toBox.min[axis])
		swept.max[axis] = max(fromBox.max[axis], toBox.max[axis])
	}
	x0, x1 := voxelSpan(swept.min[0], swept.max[0])
	y0, y1 := voxelSpan(swept.min[1], swept.max[1])
	z0, z1 := voxelSpan(swept.min[2], swept.max[2])
	half := ProjectileBodySize / 2
	for y := y0; y <= y1; y++ {
		for z := z0; z <= z1; z++ {
			for x := x0; x <= x1; x++ {
				referencePoints := box{
					min: [3]float64{float64(x) - half, float64(y) - ProjectileBodySize, float64(z) - half},
					max: [3]float64{float64(x+1) + half, float64(y + 1), float64(z+1) + half},
				}
				if _, touches := segmentBoxIntersection(from, to, referencePoints); !touches {
					continue
				}
				if _, resident := t.Block(x, y, z); !resident {
					return false
				}
			}
		}
	}
	return true
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
func (s *Sim) firstProjectileTargetLocked(proj *projectile, players []*Player, mobs []*mob, from, to [3]float64) any {
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

	for _, m := range mobs {
		// The slice was taken once, before the first projectile of this tick moved, so a
		// creature an earlier one killed is still in it — out of Sim.mobs and already a
		// corpse, but here, with no health. This is the guard that keeps the second arrow
		// of a volley from being spent on it.
		if m.health == 0 {
			continue
		}
		consider(m, m.entityID, m.species().body.boxAt(m.pos))
	}
	if proj.kind == vnet.ProjectileKindEnergyOrb {
		for _, player := range players {
			if player.entityID == proj.owner || !player.alive() {
				continue
			}
			consider(player, player.entityID, player.box())
		}
	}
	return best
}

func projectileTargetBox(target any) box {
	switch target := target.(type) {
	case *mob:
		return target.species().body.boxAt(target.pos)
	case *Player:
		return target.box()
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
