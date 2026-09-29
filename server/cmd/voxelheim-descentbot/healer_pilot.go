package main

import (
	"context"
	"fmt"
	"io"
	"math"
	"strings"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/transport"
)

func healerMember(o options, index int) bool { return o.healers > 0 && index >= o.members-o.healers }

func (p *pilot) mainHandItem() game.ItemID {
	if p.c.healing != nil {
		return game.ItemWoodenSceptre
	}
	return game.ItemIronSword
}

// orbFocus uses the same wound-first, nearby-creature priority as the blades.
// A distant fallback is approached until there is a clear shot within orb range.
func (p *pilot) orbFocus(self selfState, pick func(mobView) bool, mobs []mobView) (orbTarget, bool) {
	var target, focus mobView
	bestD, focusD := math.Inf(1), math.Inf(1)
	for _, m := range mobs {
		if m.dying() || p.buried(m) || !pick(m) || p.stats.isKilled(m.id) || p.unreachable[m.id] {
			continue
		}
		d := math.Hypot(m.pos[0]-self.pos[0], m.pos[2]-self.pos[2])
		if d < bestD {
			target, bestD = m, d
		}
		if d <= focusRadius && (math.IsInf(focusD, 1) || m.health < focus.health || (m.health == focus.health && m.id < focus.id)) {
			focus, focusD = m, d
		}
	}
	if !math.IsInf(focusD, 1) {
		target, bestD = focus, focusD
	}
	return creatureOrbTarget(target), !math.IsInf(bestD, 1)
}

func (p *pilot) healerFight(ctx context.Context, pick func(mobView) bool) error {
	var route []cell
	var nextPlan time.Time
	var plannedID uint64
	engaged := map[uint64]time.Time{}
	for {
		if err := p.tickWait(ctx); err != nil {
			return err
		}
		self := p.c.self()
		if !self.have {
			continue
		}
		now := time.Now()
		members, mobs := p.c.healerMembers(now), p.c.mobList()
		origin := orbOrigin(self.pos)
		target, ok := woundedAlly(p.c.entityID, origin, members)
		if !ok {
			target, ok = p.orbFocus(self, pick, mobs)
		}
		if !ok {
			p.c.stand()
			return nil
		}
		danger := p.c.danger()
		face, escaping := p.healerFacing(self, target, danger)
		clear := false
		if distance3(origin, target.point) <= healerRange {
			p.c.withView(func(v *blockView) { clear = orbLineClear(v, origin, target, members, mobs, p.c.entityID) })
		}
		if !target.ally && p.healerUnreachable(target.id, clear, mobs, engaged, now) {
			route, nextPlan = nil, time.Time{}
			continue
		}
		if clear || escaping {
			route, nextPlan = nil, time.Time{}
			p.c.setIntent(face)
			if clear {
				if !escaping {
					if err := p.ensureSceptre(ctx); err != nil {
						return err
					}
				}
				if err := p.launchOrb(target, face, time.Now()); err != nil {
					return err
				}
			}
			continue
		}
		// Walk to a firing line, not into melee reach. Candidate cells are checked
		// once before BFS; its potentially large search only does a map lookup.
		if target.id != plannedID || !now.Before(nextPlan) {
			start := p.standingCell(self.pos)
			p.c.withView(func(v *blockView) { route = healerRoute(v, start, target, members, mobs, p.c.entityID, danger) })
			nextPlan, plannedID = now.Add(500*time.Millisecond), target.id
		}
		for len(route) > 0 && horizontal(self.pos, route[0]) < arriveRadius {
			route = route[1:]
		}
		if len(route) > 0 {
			toward := [2]float64{float64(route[0][0]) + .5 - self.pos[0], float64(route[0][2]) + .5 - self.pos[2]}
			l := math.Hypot(toward[0], toward[1])
			step := 2 * game.WalkSpeed / float64(p.rate)
			ahead := self.pos
			if l > 0 {
				ahead[0] += toward[0] / l * step
				ahead[2] += toward[1] / l * step
			}
			if !touches(danger, ahead) {
				p.steer(self.pos, route[0], len(route) > 1, false)
				continue
			}
		}
		p.c.setIntent(face)
		// If the most wounded ally has no walkable firing line, do useful damage
		// while waiting to replan; never knowingly fire into its intercepting body.
		fallback, found := p.orbFocus(self, pick, mobs)
		if found && distance3(origin, fallback.point) <= healerRange {
			p.c.withView(func(v *blockView) { clear = orbLineClear(v, origin, fallback, members, mobs, p.c.entityID) })
			if clear {
				face, _ = p.healerFacing(self, fallback, danger)
				p.c.setIntent(face)
				if err := p.ensureSceptre(ctx); err != nil {
					return err
				}
				if err := p.launchOrb(fallback, face, time.Now()); err != nil {
					return err
				}
			}
		}
	}
}

func (p *pilot) healerFacing(self selfState, target orbTarget, danger []region) (intent, bool) {
	origin := orbOrigin(self.pos)
	dx, dy, dz := target.point[0]-origin[0], target.point[1]-origin[1], target.point[2]-origin[2]
	face := intent{yaw: yawToward(dx, dz), pitch: math.Atan2(dy, math.Hypot(dx, dz))}
	escaping := touches(danger, self.pos)
	if escaping {
		if walk, ok := p.escapeBearing(self.pos, danger); ok {
			face.moveX, face.moveZ = relative(walk, face.yaw)
		}
	}
	return face, escaping
}

func healerRoute(v *blockView, start cell, target orbTarget, members []allyView, mobs []mobView, self uint64, danger []region) []cell {
	goals := map[cell]bool{}
	for _, radius := range []float64{4, 7, 10} {
		for bearing := range 16 {
			angle := 2 * math.Pi * float64(bearing) / 16
			for _, dy := range []int64{-1, 0, 1} {
				c := cell{int64(math.Floor(target.point[0] + radius*math.Cos(angle))), int64(math.Floor(target.point[1])) + dy, int64(math.Floor(target.point[2] + radius*math.Sin(angle)))}
				pos := [3]float64{float64(c[0]) + .5, float64(c[1]), float64(c[2]) + .5}
				if v.standable(c) && !touches(danger, pos) && distance3(orbOrigin(pos), target.point) <= healerRange &&
					orbLineClear(v, orbOrigin(pos), target, members, mobs, self) {
					goals[c] = true
				}
			}
		}
	}
	if len(goals) == 0 {
		return nil
	}
	return v.path(start, func(c cell) bool { return goals[c] })
}

// launchOrb keeps aim and attack adjacent under writeMu, but never holds the
// reader's mutex over socket I/O. The tentative observation is committed only
// after the attack write succeeds, or discarded (including early deltas) on error.
func (p *pilot) launchOrb(target orbTarget, face intent, now time.Time) error {
	if now.Sub(p.lastSwing) < healerEvery {
		return nil
	}
	c := p.c
	c.writeMu.Lock()
	defer c.writeMu.Unlock()
	var origin [3]float64
	ready := func() bool {
		c.mu.Lock()
		defer c.mu.Unlock()
		if !c.alive || c.energy < game.AttackEnergyCost || !usableSceptre(c.inventory) {
			return false
		}
		if now.Sub(c.healing.seenAt) > healerObservationSlack {
			return false
		}
		if target.ally {
			living := false
			for _, m := range c.healing.members {
				if m.id == target.id && m.id != c.entityID && m.alive && m.health > 0 && m.maxHealth > 0 && float64(m.health)/float64(m.maxHealth) < healerHealthFraction {
					target.point = m.pos
					target.point[1] += game.PlayerHeight / 2
					living = true
					break
				}
			}
			if !living {
				return false
			}
		} else {
			m, found := c.mobs[target.id]
			if !found || m.dying() {
				return false
			}
			target = creatureOrbTarget(m)
		}
		origin = orbOrigin(c.pos)
		mobs := make([]mobView, 0, len(c.mobs))
		for _, m := range c.mobs {
			mobs = append(mobs, m)
		}
		if distance3(origin, target.point) > healerRange || !orbLineClear(c.view, origin, target, c.healing.members, mobs, c.entityID) {
			return false
		}
		dx, dy, dz := target.point[0]-origin[0], target.point[1]-origin[1], target.point[2]-origin[2]
		face.yaw, face.pitch = yawToward(dx, dz), math.Atan2(dy, math.Hypot(dx, dz))
		c.control = face
		return true
	}()
	if !ready {
		return nil
	}
	c.clientTick++
	if err := transport.WriteFrame(c.conn, protocol.EncodePlayerInput(protocol.PlayerInput{
		ClientTick: c.clientTick, MoveX: float32(face.moveX), MoveZ: float32(face.moveZ), Yaw: float32(face.yaw), Pitch: float32(face.pitch), Jump: face.jump,
	})); err != nil {
		return err
	}
	c.mu.Lock()
	// A blocked aim write may have outlived the target. The reader remained live,
	// so do not send an attack when its newest answer already rules it out.
	allowed := c.alive && c.energy >= game.AttackEnergyCost && usableSceptre(c.inventory) && time.Since(c.healing.seenAt) <= healerObservationSlack
	if target.ally {
		living := false
		for _, m := range c.healing.members {
			if m.id == target.id && m.alive && m.health > 0 {
				living = true
				break
			}
		}
		allowed = allowed && living
	} else {
		m, found := c.mobs[target.id]
		allowed = allowed && found && !m.dying()
	}
	if !allowed {
		c.mu.Unlock()
		return nil
	}
	shot := c.healing.beginLaunch(target, origin, time.Now())
	c.mu.Unlock()
	c.clientTick++
	err := transport.WriteFrame(c.conn, protocol.EncodeAttackRequest(protocol.AttackRequest{Slot: mainHandSlot, ClientTick: c.clientTick}))
	c.mu.Lock()
	c.healing.finishLaunch(target, shot, err == nil)
	c.mu.Unlock()
	if err == nil {
		p.lastSwing = time.Now()
	}
	return err
}

func writeHealerReport(out io.Writer, pt *party) {
	if pt.leaderRunner().opts.healers == 0 {
		return
	}
	_, _ = fmt.Fprintf(out, "\nhealers: %d (heal below %.0f%% health, range %.0f blocks)\n", pt.leaderRunner().opts.healers, healerHealthFraction*100, healerRange)
	for _, m := range pt.members {
		if m.c.healing == nil {
			continue
		}
		totals := m.c.healingTotals()
		_, _ = fmt.Fprintf(out, "  %s: orb launch requests sent to allies %d, creatures %d; observed health restored near own ally requests %d (inferred)\n", m.c.name, totals.allies, totals.creatures, totals.restored)
		_, _ = fmt.Fprintf(out, "    development assistance: %d worn sceptres replaced with /additem and normal equipment moves\n", totals.replacements)
	}
	_, _ = fmt.Fprintln(out, "  Requests may be refused. Health is positive stream deltas on the intended living ally in a bounded flight window, counted once per request; regeneration and other healers can overlap. Damage between snapshots can hide healing. These are not confirmed orb launches or source-attributed heals.")
}

// Read controls after acquiring the writer: a heartbeat queued behind launchOrb
// must not undo its aim with an earlier copy before the server's next tick.
func (c *client) healerHeartbeat() error {
	c.writeMu.Lock()
	defer c.writeMu.Unlock()
	c.mu.Lock()
	in := c.control
	c.mu.Unlock()
	c.clientTick++
	return transport.WriteFrame(c.conn, protocol.EncodePlayerInput(protocol.PlayerInput{
		ClientTick: c.clientTick, MoveX: float32(in.moveX), MoveZ: float32(in.moveZ), Yaw: float32(in.yaw), Pitch: float32(in.pitch), Jump: in.jump,
	}))
}

func usableSceptre(state protocol.InventoryState) bool {
	if len(state.Stacks) <= int(mainHandSlot) {
		return false
	}
	s := state.Stacks[mainHandSlot]
	return s.ItemID == uint16(game.ItemWoodenSceptre) && s.Count == 1 && s.MaxDurability > 0 && s.Durability > 0
}

// The wooden sceptre wears on every launch; melee does not. Replace only after
// the stream confirms it is exhausted, counting this development assistance.
// Keep the worn item in the pack. A full pack or a refused command fails openly
// rather than pretending attacks with a broken weapon are doing useful work.
func (p *pilot) ensureSceptre(ctx context.Context) error {
	state, revision := p.c.inventoryAnswer()
	if usableSceptre(state) {
		return nil
	}
	if len(state.Stacks) <= int(mainHandSlot) {
		return fmt.Errorf("healer has no authoritative main-hand inventory")
	}
	worn := state.Stacks[mainHandSlot]
	if worn.ItemID != uint16(game.ItemWoodenSceptre) || worn.Count != 1 || worn.MaxDurability == 0 || worn.Durability != 0 {
		return fmt.Errorf("healer main hand is not a worn wooden sceptre")
	}
	answer, err := p.c.command(ctx, fmt.Sprintf("/additem %d 1", game.ItemWoodenSceptre))
	if err != nil {
		return err
	}
	if !strings.HasPrefix(answer, "Added ") {
		return fmt.Errorf("sceptre replacement refused: %s", answer)
	}
	from := -1
	if err := p.waitHealerInventory(ctx, revision, func(state protocol.InventoryState) bool {
		for i, s := range state.Stacks {
			if i >= int(protocol.InventorySlots-protocol.EquipmentSlots) {
				break
			}
			if s.ItemID == uint16(game.ItemWoodenSceptre) && s.Count == 1 && s.Durability > 0 {
				from = i
				return true
			}
		}
		return false
	}); err != nil {
		return err
	}
	_, revision = p.c.inventoryAnswer()
	if err := p.c.send(protocol.EncodeInventoryMoveRequest(protocol.InventoryMoveRequest{From: uint8(from), To: mainHandSlot, Count: 1})); err != nil {
		return err
	}
	if err := p.waitHealerInventory(ctx, revision, usableSceptre); err != nil {
		return err
	}
	p.c.mu.Lock()
	p.c.healing.totals.replacements++
	p.c.mu.Unlock()
	return nil
}

func (p *pilot) waitHealerInventory(ctx context.Context, after uint64, ready func(protocol.InventoryState) bool) error {
	deadline := time.Now().Add(5 * time.Second)
	for {
		if err := ctx.Err(); err != nil {
			return err
		}
		state, revision := p.c.inventoryAnswer()
		if revision > after && ready(state) {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("server did not confirm the healer's replacement equipment")
		}
		if err := p.tickWait(ctx); err != nil {
			return err
		}
	}
}

// Match the blades' bounded engagement rule: a non-boss with no firing line
// and no party hit for a minute is recorded once and excluded by orbFocus.
// Reaching a firing line resets the timer, so a temporary obstruction cannot
// make an otherwise engaged creature unreachable. Bosses remain mandatory.
func (p *pilot) healerUnreachable(id uint64, clear bool, mobs []mobView, engaged map[uint64]time.Time, now time.Time) bool {
	if _, ok := engaged[id]; !ok || clear {
		engaged[id] = now
	}
	if clear || now.Sub(engaged[id]) <= time.Minute || now.Sub(p.stats.lastHit(id)) <= time.Minute {
		return false
	}
	for _, m := range mobs {
		if m.id != id || boss(m.kind) {
			continue
		}
		if !p.unreachable[id] {
			p.unreachable[id] = true
			p.stats.unreached(m)
			p.say("giving up on an unreachable creature at %.1f, %.1f, %.1f", m.pos[0], m.pos[1], m.pos[2])
		}
		return true
	}
	return false
}
