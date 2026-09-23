package main

import (
	"math"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
)

// Reading a boss (#1333). Every blow a boss aims is announced before it lands: the
// EncounterTimeline the server streams names each running move's regions — a cone, a
// lane, a disc or a ring — and a player who is not in one when it resolves is not struck.
// A member reads those regions as the client is shown them and, standing in one, walks the
// bearing that leaves every region soonest; otherwise it closes and swings as before. This
// is the #1099 harness's reader over the wire rather than inside the simulation, and it is
// what a party of people does: a stander that never steps aside is killed by either boss
// long before three of them can finish it.

const (
	// readMargin is how far past the body a region is still treated as touching it, as the
	// #1099 reader's margin does: the server samples a body at its centre and corners, and
	// a margin keeps the reader's "clear" a superset of the server's "not reached".
	readMargin = 0.35
	// escapeHorizon is how far ahead a bearing is walked to see whether it gets clear.
	escapeHorizon = 2 * time.Second
	// believedFor is how long a timeline is believed without another: the server states the
	// whole timeline whenever it changes, so an old one is only stale if the stream stalled.
	believedFor = 3 * time.Second
)

// region is one announced hazard, in the contract's terms.
type region struct {
	shape                 vnet.HazardShape
	origin, direction     [3]float64
	radius, height, inner float64
	halfAngle, halfWidth  float64
}

// absorbTimeline replaces what this member believes a boss is about to do.
func (c *client) absorbTimeline(timeline *vnet.EncounterTimeline) {
	var regions []region
	var move vnet.EncounterMove
	var hazard vnet.HazardVolume
	var origin, direction vnet.Vec3
	for i := range timeline.MovesLength() {
		if !timeline.Moves(&move, i) || move.Ended() != vnet.MoveEndUnknown || move.Phase() == vnet.MovePhaseRecovery {
			continue
		}
		for j := range move.HazardsLength() {
			if !move.Hazards(&hazard, j) {
				continue
			}
			hazard.Origin(&origin)
			hazard.Direction(&direction)
			regions = append(regions, region{
				shape:     hazard.Shape(),
				origin:    [3]float64{float64(origin.X()), float64(origin.Y()), float64(origin.Z())},
				direction: [3]float64{float64(direction.X()), float64(direction.Y()), float64(direction.Z())},
				radius:    float64(hazard.Radius()), height: float64(hazard.Height()), inner: float64(hazard.InnerRadius()),
				halfAngle: float64(hazard.HalfAngle()), halfWidth: float64(hazard.HalfWidth()),
			})
		}
	}
	c.mu.Lock()
	c.believed, c.believedAt = regions, time.Now()
	c.mu.Unlock()
}

// danger is every region this member believes is about to be struck.
func (c *client) danger() []region {
	c.mu.Lock()
	defer c.mu.Unlock()
	if time.Since(c.believedAt) > believedFor {
		return nil
	}
	return c.believed
}

// reaches is the server's contact rule for one region and one horizontal point at a body's
// height, as internal/game's hazardReaches states it.
func (h region) reaches(x, z, feet float64) bool {
	half := h.height / 2
	if feet+game.PlayerHeight <= h.origin[1]-half || feet >= h.origin[1]+half {
		return false
	}
	dir := [2]float64{h.direction[0], h.direction[2]}
	if l := math.Hypot(dir[0], dir[1]); l > 0 {
		dir[0], dir[1] = dir[0]/l, dir[1]/l
	}
	dx, dz := x-h.origin[0], z-h.origin[2]
	distance := math.Hypot(dx, dz)
	switch h.shape {
	case vnet.HazardShapeCone:
		if distance > h.radius {
			return false
		}
		if distance == 0 {
			return true
		}
		cosine := (dx*dir[0] + dz*dir[1]) / distance
		return math.Acos(min(max(cosine, -1), 1)) <= h.halfAngle
	case vnet.HazardShapeLine:
		along := dx*dir[0] + dz*dir[1]
		return along >= 0 && along <= h.radius && math.Abs(dx*dir[1]-dz*dir[0]) <= h.halfWidth
	case vnet.HazardShapeDisc:
		return distance <= h.radius
	case vnet.HazardShapeRing:
		return distance <= h.radius && distance >= h.inner
	}
	return false
}

// touches is whether any region reaches a body standing at pos or the margin around it,
// sampled as a grid over the enlarged footprint as the #1099 reader samples it.
func touches(regions []region, pos [3]float64) bool {
	if len(regions) == 0 {
		return false
	}
	const samples = 7
	half := game.PlayerWidth/2 + readMargin
	for i := range samples {
		for j := range samples {
			x := pos[0] - half + 2*half*float64(i)/(samples-1)
			z := pos[2] - half + 2*half*float64(j)/(samples-1)
			for _, h := range regions {
				if h.reaches(x, z, pos[1]) {
					return true
				}
			}
		}
	}
	return false
}

// escapeBearing is the straight walk, as a horizontal unit vector, that leaves every region
// soonest over terrain the stream shows open at the body's height, or false if none does
// within escapeHorizon.
func (p *pilot) escapeBearing(start [3]float64, regions []region) ([2]float64, bool) {
	const bearings = 32
	step := game.WalkSpeed / float64(p.rate)
	horizon := int(escapeHorizon.Seconds() * float64(p.rate))
	best, bestTicks := [2]float64{}, horizon+1
	p.c.withView(func(v *blockView) {
		for i := range bearings {
			angle := 2 * math.Pi * float64(i) / bearings
			dir := [2]float64{math.Cos(angle), math.Sin(angle)}
			pos := start
			for ticks := 1; ticks < bestTicks; ticks++ {
				pos[0] += dir[0] * step
				pos[2] += dir[1] * step
				feet := feetCell(pos)
				if !v.open(feet[0], feet[1], feet[2]) || !v.open(feet[0], feet[1]+1, feet[2]) {
					break
				}
				if !touches(regions, pos) {
					best, bestTicks = dir, ticks
					break
				}
			}
		}
	})
	return best, bestTicks <= horizon
}

// relative turns a walk along a horizontal world direction into the controls' strafe and
// forward for a body facing yaw: +X is the right of yaw 0, which looks along -Z.
func relative(walk [2]float64, yaw float64) (moveX, moveZ float64) {
	right := [2]float64{math.Cos(yaw), -math.Sin(yaw)}
	forward := [2]float64{-math.Sin(yaw), -math.Cos(yaw)}
	return walk[0]*right[0] + walk[1]*right[1], walk[0]*forward[0] + walk[1]*forward[1]
}
