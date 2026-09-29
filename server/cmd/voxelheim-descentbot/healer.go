package main

import (
	"math"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
)

const (
	// Heal below three quarters: leave a reserve before the next blow without
	// spending every attack topping off minor wounds. This is bot policy only.
	healerHealthFraction = 0.75
	// Twenty blocks leaves four blocks of margin inside OrbSpeed * OrbLifetime
	// (24 blocks). Longer shots miss moving bodies or expire too often.
	healerRange = 20.0
	healerEvery = game.SceptreCooldown + 50*time.Millisecond
	// Delivery can lag the flight by several ticks. This bounds attribution, not
	// gameplay: the wire does not identify the source of a positive health delta.
	healerObservationSlack = 500 * time.Millisecond
)

type allyView struct {
	id                uint64
	pos               [3]float64
	health, maxHealth uint16
	alive             bool
}

type orbTarget struct {
	id    uint64
	point [3]float64
	ally  bool
}

func orbOrigin(feet [3]float64) [3]float64 {
	feet[1] += game.ProjectileEyeHeight
	return feet
}

func distance3(a, b [3]float64) float64 {
	return math.Sqrt((a[0]-b[0])*(a[0]-b[0]) + (a[1]-b[1])*(a[1]-b[1]) + (a[2]-b[2])*(a[2]-b[2]))
}

// woundedAlly picks the lowest fraction, then entity id for a stable tie. Its
// input is this healer's own stream, never another runner's private state.
// A blocked ally remains the desired target: the caller finds a firing line
// instead of launching through the creature standing in front of that ally.
func woundedAlly(self uint64, origin [3]float64, members []allyView) (orbTarget, bool) {
	var best allyView
	found := false
	for _, m := range members {
		point := m.pos
		point[1] += game.PlayerHeight / 2
		if m.id == self || !m.alive || m.health == 0 || m.maxHealth == 0 ||
			float64(m.health)/float64(m.maxHealth) >= healerHealthFraction || distance3(origin, point) > healerRange {
			continue
		}
		if !found || uint32(m.health)*uint32(best.maxHealth) < uint32(best.health)*uint32(m.maxHealth) ||
			(uint32(m.health)*uint32(best.maxHealth) == uint32(best.health)*uint32(m.maxHealth) && m.id < best.id) {
			best, found = m, true
		}
	}
	point := best.pos
	point[1] += game.PlayerHeight / 2
	return orbTarget{id: best.id, point: point, ally: true}, found
}

func creatureOrbTarget(m mobView) orbTarget {
	_, middle := mobShape(m.kind)
	point := m.pos
	point[1] += middle
	return orbTarget{id: m.id, point: point}
}

// segmentBody tests the complete segment against a body expanded by the orb's
// footprint. It is only a conservative aiming decision; the server judges hits.
func segmentBody(from, to, feet [3]float64, half, height float64) bool {
	padding := game.ProjectileBodySize / 2
	low := [3]float64{feet[0] - half - padding, feet[1] - game.ProjectileBodySize, feet[2] - half - padding}
	high := [3]float64{feet[0] + half + padding, feet[1] + height, feet[2] + half + padding}
	enter, leave := 0.0, 1.0
	for axis := range 3 {
		d := to[axis] - from[axis]
		if math.Abs(d) < 1e-9 {
			if from[axis] < low[axis] || from[axis] > high[axis] {
				return false
			}
			continue
		}
		a, b := (low[axis]-from[axis])/d, (high[axis]-from[axis])/d
		if a > b {
			a, b = b, a
		}
		enter, leave = math.Max(enter, a), math.Min(leave, b)
		if enter > leave {
			return false
		}
	}
	return true
}

// orbLineClear includes the projectile's body and fails closed on unread terrain.
// A living party member can intercept an orb just as a creature can.
func orbLineClear(v *blockView, from [3]float64, target orbTarget, members []allyView, mobs []mobView, self uint64) bool {
	for _, m := range mobs {
		if m.id == target.id || m.dying() {
			continue
		}
		half, middle := mobShape(m.kind)
		if segmentBody(from, target.point, m.pos, half, 2*middle) {
			return false
		}
	}
	for _, m := range members {
		if m.id == self || m.id == target.id || !m.alive || m.health == 0 {
			continue
		}
		if segmentBody(from, target.point, m.pos, game.PlayerWidth/2, game.PlayerHeight) {
			return false
		}
	}
	// Sample more finely than the projectile edge, checking the entire tiny box
	// at each point, so a narrow corner does not masquerade as an open line.
	steps := max(1, int(math.Ceil(distance3(from, target.point)/(game.ProjectileBodySize/2))))
	half := game.ProjectileBodySize / 2
	for i := 0; i <= steps; i++ {
		var pos [3]float64
		for axis := range 3 {
			pos[axis] = from[axis] + (target.point[axis]-from[axis])*float64(i)/float64(steps)
		}
		for _, dx := range []float64{-half, half} {
			for _, dz := range []float64{-half, half} {
				for _, dy := range []float64{0, game.ProjectileBodySize} {
					if !v.open(int64(math.Floor(pos[0]+dx)), int64(math.Floor(pos[1]+dy)), int64(math.Floor(pos[2]+dz))) {
						return false
					}
				}
			}
		}
	}
	return true
}

// healerState is guarded by client.mu. Launches count successfully written
// requests, not confirmed server effects: no projectile owner or heal event is
// sent on this protocol. Health credit is a bounded, explicitly labelled inference.
type healerState struct {
	members []allyView
	seenAt  time.Time
	pending []healObservation
	totals  healerTotals
}

type healerTotals struct {
	allies, creatures uint64
	restored          uint64
	replacements      uint64
}

type healObservation struct {
	target            uint64
	earliest, expires time.Time
}

func (h *healerState) launched(target orbTarget, origin [3]float64, now time.Time) {
	if !target.ally {
		h.totals.creatures++
		return
	}
	h.totals.allies++
	flight := time.Duration(distance3(origin, target.point) / game.OrbSpeed * float64(time.Second))
	// A body is hit before its centre; the spawn nudge shortens the flight too.
	earliest := now.Add(max(time.Duration(0), flight-150*time.Millisecond))
	h.pending = append(h.pending, healObservation{target: target.id, earliest: earliest, expires: now.Add(flight + healerObservationSlack)})
}

// observe replaces the complete party vector. Respawns, max-health changes,
// vanished members and gaps in the stream are never counted as a heal.
func (h *healerState) observe(members []allyView, now time.Time) {
	remaining := h.pending[:0]
	credited := map[uint64]bool{}
	for _, shot := range h.pending {
		if now.After(shot.expires) {
			continue
		}
		var old, next allyView
		for _, m := range h.members {
			if m.id == shot.target {
				old = m
				break
			}
		}
		for _, m := range members {
			if m.id == shot.target {
				next = m
				break
			}
		}
		if !old.alive || !next.alive || old.health == 0 || next.health == 0 || old.maxHealth != next.maxHealth {
			continue
		}
		if !now.Before(shot.earliest) && next.health > old.health && now.Sub(h.seenAt) <= healerObservationSlack {
			if !credited[shot.target] {
				h.totals.restored += uint64(next.health - old.health)
				credited[shot.target] = true
			}
			// Only one outstanding request may claim this delta in this observer.
			// Cadence exceeds the slack, so ordinary flight windows do not overlap.
			continue
		}
		remaining = append(remaining, shot)
	}
	h.pending = remaining
	h.members = members
	h.seenAt = now
}

// absorbHealerSnapshot runs with client.mu held by absorbSnapshot. A nil state
// leaves the historical zero-healer run untouched.
func (c *client) absorbHealerSnapshot(snapshot *vnet.EntitySnapshot, now time.Time) {
	if c.healing == nil {
		return
	}
	members := make([]allyView, 0, snapshot.PartyMembersLength())
	var m vnet.PartyMemberState
	for i := range snapshot.PartyMembersLength() {
		if !snapshot.PartyMembers(&m, i) {
			continue
		}
		pos := m.Pos(nil)
		members = append(members, allyView{id: m.EntityId(), pos: [3]float64{float64(pos.X()), float64(pos.Y()), float64(pos.Z())}, health: m.Health(), maxHealth: m.MaxHealth(), alive: m.Alive()})
	}
	c.healing.observe(members, now)
}

func (c *client) healerMembers(now time.Time) []allyView {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.healing == nil || now.Sub(c.healing.seenAt) > healerObservationSlack {
		return nil
	}
	return append([]allyView(nil), c.healing.members...)
}

func (c *client) healingTotals() healerTotals {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.healing == nil {
		return healerTotals{}
	}
	return c.healing.totals
}
