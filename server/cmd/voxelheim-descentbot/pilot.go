package main

import (
	"context"
	"errors"
	"fmt"
	"math"
	"strings"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The pilot. Every movement is PlayerInput: the bot faces its next cell, holds forward and
// jumps where the next cell is a course up, and the server's integrator decides where that
// puts it, so a walk takes as long as a player's. A body wedged for stuckLimit is moved on
// to its next cell with /teleport, named in the report as an assist; that cell is one the
// path found over delivered, open terrain, so an assist never passes a closed door.

var errDied = errors.New("the bot died")

const (
	// arriveRadius is how close to a cell's centre the feet must come to have reached it.
	arriveRadius = 0.3
	// stuckLimit is how long the walk may make no progress before /teleport moves it on.
	stuckLimit = 6 * time.Second
	// swingEvery paces the attacks: the sword's cooldown and a tick of slack. Energy is
	// the tighter bound over a long fight, and a swing refused for it costs nothing.
	swingEvery = game.SwordCooldown + 50*time.Millisecond
)

type pilot struct {
	c     *client
	stats *runStats
	rate  int
	say   func(format string, args ...any)
	// immortal mirrors the last /immortal the server accepted.
	immortal      bool
	immortalSince time.Time
	lastSwing     time.Time
	lastReport    time.Time
	// unreachable is every creature fight gave up on; see fight.
	unreachable map[uint64]bool
}

// pause waits a duration, answering errDied if the bot dies in it.
func (p *pilot) pause(ctx context.Context, d time.Duration) error {
	deadline := time.Now().Add(d)
	for time.Now().Before(deadline) {
		if err := p.tickWait(ctx); err != nil {
			return err
		}
	}
	return nil
}

// tickWait waits one server tick.
func (p *pilot) tickWait(ctx context.Context) error {
	timer := time.NewTimer(time.Second / time.Duration(p.rate))
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return ctx.Err()
	case <-timer.C:
	}
	if !p.c.self().alive {
		p.c.stand()
		return errDied
	}
	return nil
}

// waitAlive waits out a death and the respawn after it.
func (p *pilot) waitAlive(ctx context.Context) error {
	p.c.stand()
	for !p.c.self().alive {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(100 * time.Millisecond):
		}
	}
	// The respawn is a relocation: give the stream a moment to deliver where it put us.
	return sleep(ctx, time.Second)
}

func sleep(ctx context.Context, d time.Duration) error {
	timer := time.NewTimer(d)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return ctx.Err()
	case <-timer.C:
		return nil
	}
}

// setImmortal turns the development toggle on or off and records how long it was on.
func (p *pilot) setImmortal(ctx context.Context, on bool) error {
	if p.immortal == on {
		return nil
	}
	answer, err := p.c.command(ctx, fmt.Sprintf("/immortal %t", on))
	if err != nil {
		return err
	}
	if !strings.HasPrefix(answer, "Immortality") {
		return fmt.Errorf("/immortal answered %q", answer)
	}
	if on {
		p.immortalSince = time.Now()
	} else {
		p.stats.addImmortal(p.stats.currentPhase(), time.Since(p.immortalSince))
	}
	p.immortal = on
	return nil
}

// yawToward is the yaw that makes "forward" point along (dx, dz): yaw 0 looks along -Z
// and forward is (-sin yaw, -cos yaw), as internal/game's integrator spells it.
func yawToward(dx, dz float64) float64 { return math.Atan2(-dx, -dz) }

// walkTo walks to the first standing cell goal accepts, fighting whatever turns on the
// bot on the way when fight is set.
func (p *pilot) walkTo(ctx context.Context, what string, goal func(cell) bool, fight bool) error {
	var route []cell
	var planned time.Time
	lastProgress, best := time.Now(), math.Inf(1)
	var noPath, hopUntil time.Time
	for {
		if err := p.tickWait(ctx); err != nil {
			return err
		}
		self := p.c.self()
		if !self.have {
			continue
		}
		if fight {
			if threat, ok := p.threat(); ok {
				if err := p.fight(ctx, func(m mobView) bool { return m.id == threat.id }); err != nil {
					return err
				}
				route, lastProgress, best = nil, time.Now(), math.Inf(1)
				continue
			}
		}
		here := feetCell(self.pos)
		if goal(here) && centred(self.pos, here) {
			p.c.stand()
			return nil
		}
		if route == nil || time.Since(planned) > 2*time.Second {
			start := p.standingCell(self.pos)
			var found []cell
			p.c.withView(func(v *blockView) { found = v.path(start, goal) })
			planned = time.Now()
			if found == nil && !goal(start) {
				p.c.stand()
				if noPath.IsZero() {
					noPath = time.Now()
				}
				if time.Since(noPath) > 20*time.Second {
					var standable bool
					var chunks int
					p.c.withView(func(v *blockView) { standable, chunks = v.standable(start), len(v.chunks) })
					return fmt.Errorf("no walkable path to %s from %v (feet at %.2f, standable %t, %d chunks held)",
						what, start, self.pos, standable, chunks)
				}
				continue
			}
			noPath = time.Time{}
			route = found
			if len(route) == 0 {
				route = []cell{start}
			}
		}
		// Drop every cell already reached, including ones a corner cut past.
		for i := len(route) - 1; i >= 0; i-- {
			if horizontal(self.pos, route[i]) < arriveRadius && math.Abs(self.pos[1]-float64(route[i][1])) < 0.7 {
				route = route[i+1:]
				break
			}
		}
		if len(route) == 0 {
			route = nil
			continue
		}
		next := route[0]
		p.steer(self.pos, next, len(route) > 1, time.Now().Before(hopUntil))
		// Progress is the remaining distance along the route; a walk that has not
		// shortened it for stuckLimit is wedged.
		remaining := horizontal(self.pos, next) + float64(len(route))
		if remaining < best-0.1 {
			best, lastProgress = remaining, time.Now()
		}
		switch stalled := time.Since(lastProgress); {
		case stalled > stuckLimit:
			if err := p.assist(ctx, next); err != nil {
				return err
			}
			route, best, lastProgress = nil, math.Inf(1), time.Now()
		case stalled > 1500*time.Millisecond && time.Now().After(hopUntil.Add(time.Second)):
			// A hop clears most snags on a corner or a lip the step-up does not climb.
			hopUntil = time.Now().Add(300 * time.Millisecond)
		}
	}
}

// standingCell is where a path starts: the feet's cell, or the one under it when the body
// is mid-step or in a snare.
func (p *pilot) standingCell(pos [3]float64) cell {
	here := feetCell(pos)
	var best cell
	found := false
	p.c.withView(func(v *blockView) {
		for _, dy := range [...]int64{0, -1, 1} {
			if c := (cell{here[0], here[1] + dy, here[2]}); v.standable(c) {
				best, found = c, true
				return
			}
		}
	})
	if found {
		return best
	}
	return here
}

// steer points the body at the centre of next and holds forward, jumping where next is a
// course higher than the feet.
func (p *pilot) steer(pos [3]float64, next cell, more, hop bool) {
	dx, dz := float64(next[0])+.5-pos[0], float64(next[2])+.5-pos[2]
	speed := 1.0
	if !more {
		// Ease into the last cell rather than orbit it.
		speed = min(1, math.Max(0.35, math.Hypot(dx, dz)/0.6))
	}
	p.c.setIntent(intent{
		moveZ: speed,
		yaw:   yawToward(dx, dz),
		jump:  hop || (float64(next[1]) > pos[1]+0.4 && math.Hypot(dx, dz) < 1.4),
	})
}

// assist moves a wedged body onto its next cell with /teleport, at a cell corner whose
// four neighbouring cells are all open at feet and head height — /teleport takes whole
// numbers, and a body centred on a corner overlaps the four cells around it.
func (p *pilot) assist(ctx context.Context, next cell) error {
	corners := [...][2]int64{{0, 0}, {1, 0}, {0, 1}, {1, 1}}
	var chosen [2]int64
	ok := false
	p.c.withView(func(v *blockView) {
		for _, corner := range corners {
			x, z := next[0]+corner[0], next[2]+corner[1]
			clear := true
			for _, d := range corners {
				cx, cz := x-1+d[0], z-1+d[1]
				if !v.open(cx, next[1], cz) || !v.open(cx, next[1]+1, cz) {
					clear = false
				}
			}
			if clear {
				chosen, ok = [2]int64{x, z}, true
				return
			}
		}
	})
	if !ok {
		chosen = [2]int64{next[0], next[2]}
	}
	line := fmt.Sprintf("/teleport %d %d %d", chosen[0], next[1], chosen[1])
	p.say("stuck; %s", line)
	p.stats.assist(fmt.Sprintf("%s: %v", p.stats.currentPhase(), next))
	if _, err := p.c.command(ctx, line); err != nil {
		return err
	}
	return p.pause(ctx, 500*time.Millisecond)
}

func centred(pos [3]float64, c cell) bool { return horizontal(pos, c) < arriveRadius+0.1 }

func horizontal(pos [3]float64, c cell) float64 {
	return math.Hypot(float64(c[0])+.5-pos[0], float64(c[2])+.5-pos[2])
}

// mobShape is how far a creature's body reaches from its root and how high its middle is:
// what the approach and the aim need, from the species rows in internal/game/species.go.
func mobShape(kind vnet.MobKind) (halfWidth, middle float64) {
	switch kind {
	case vnet.MobKindCaveSpider:
		return 0.45, 0.3
	case vnet.MobKindScorpion:
		return 0.65, 0.3
	case vnet.MobKindVargr:
		return 0.45, 0.5
	case vnet.MobKindVargrGuardian:
		return 0.8, 0.9
	case vnet.MobKindDraugrKing:
		return 0.5, 1.4
	default:
		return 0.3, 0.9
	}
}

// buried is a creature lying under the sand: its root is inside a solid cell.
func (p *pilot) buried(m mobView) bool {
	b, ok := p.c.blockAt(cell{int64(math.Floor(m.pos[0])), int64(math.Floor(m.pos[1] + 0.2)), int64(math.Floor(m.pos[2]))})
	return ok && world.Solid(b)
}

// threat is the nearest living creature that has turned on the bot.
func (p *pilot) threat() (mobView, bool) {
	self := p.c.self()
	var best mobView
	bestD := math.Inf(1)
	for _, m := range p.c.mobList() {
		if m.dying() || m.targetID() != p.c.entityID || p.buried(m) || boss(m.kind) {
			continue
		}
		if d := math.Hypot(m.pos[0]-self.pos[0], m.pos[2]-self.pos[2]); d < bestD && math.Abs(m.pos[1]-self.pos[1]) < 6 {
			best, bestD = m, d
		}
	}
	return best, bestD < 24
}

func boss(kind vnet.MobKind) bool {
	return kind == vnet.MobKindVargrGuardian || kind == vnet.MobKindDraugrKing
}

// fight closes on and strikes the nearest creature pick accepts until none is left alive.
func (p *pilot) fight(ctx context.Context, pick func(mobView) bool) error {
	var route []cell
	var planned time.Time
	reach := map[uint64]float64{}
	misses := map[uint64]int{}
	previous := map[uint64]time.Time{}
	engaged := map[uint64]time.Time{}
	for {
		if err := p.tickWait(ctx); err != nil {
			return err
		}
		self := p.c.self()
		var target mobView
		bestD := math.Inf(1)
		for _, m := range p.c.mobList() {
			if m.dying() || p.buried(m) || !pick(m) || p.stats.isKilled(m.id) || p.unreachable[m.id] {
				continue
			}
			if d := math.Hypot(m.pos[0]-self.pos[0], m.pos[2]-self.pos[2]); d < bestD {
				target, bestD = m, d
			}
		}
		if math.IsInf(bestD, 1) {
			p.c.stand()
			return nil
		}
		if _, ok := engaged[target.id]; !ok {
			engaged[target.id] = time.Now()
		}
		// Not hit for a minute and never in reach: no walk reaches it. Left, and named.
		if !boss(target.kind) && bestD > 1.9+1.5 && time.Since(engaged[target.id]) > time.Minute &&
			time.Since(p.stats.lastHit(target.id)) > time.Minute {
			p.unreachable[target.id] = true
			p.stats.unreached(target)
			p.say("giving up on an unreachable %s at %.1f, %.1f, %.1f", vnet.EnumNamesMobKind[target.kind], target.pos[0], target.pos[1], target.pos[2])
			continue
		}
		if time.Since(p.lastReport) > 10*time.Second {
			p.lastReport = time.Now()
			p.say("%s at %.0f/%d health, %.1f blocks off (%.1f up), energy %d", vnet.EnumNamesMobKind[target.kind],
				float64(target.health), target.maxHealth, bestD, target.pos[1]-self.pos[1], self.energy)
		}
		half, middle := mobShape(target.kind)
		want, known := reach[target.id]
		if !known {
			want = 1.9 + half
		}
		if bestD > want {
			if route == nil || time.Since(planned) > 500*time.Millisecond {
				goal := func(c cell) bool {
					return math.Hypot(float64(c[0])+.5-target.pos[0], float64(c[2])+.5-target.pos[2]) <= want-0.3 &&
						math.Abs(float64(c[1])-target.pos[1]) < 2.5
				}
				start := p.standingCell(self.pos)
				p.c.withView(func(v *blockView) { route = v.path(start, goal) })
				planned = time.Now()
			}
			for len(route) > 0 && horizontal(self.pos, route[0]) < arriveRadius {
				route = route[1:]
			}
			if len(route) > 0 {
				p.steer(self.pos, route[0], len(route) > 1, false)
			} else {
				// Nothing to plan over — open floor the stream has not shown yet, or water:
				// walk straight at it.
				p.c.setIntent(intent{moveZ: 1, yaw: yawToward(target.pos[0]-self.pos[0], target.pos[2]-self.pos[2])})
			}
			continue
		}
		route = nil
		dx, dz := target.pos[0]-self.pos[0], target.pos[2]-self.pos[2]
		dy := target.pos[1] + middle - (self.pos[1] + game.PlayerHeight/2)
		p.c.setIntent(intent{yaw: yawToward(dx, dz), pitch: math.Atan2(dy, math.Hypot(dx, dz))})
		if time.Since(p.lastSwing) < swingEvery || self.energy < game.AttackEnergyCost {
			continue
		}
		// Four swings without landing: come closer. A swing is judged on the next tick, so
		// this one learns whether the previous one landed.
		if hit := p.stats.lastHit(target.id); hit.Equal(previous[target.id]) {
			misses[target.id]++
		} else {
			misses[target.id], previous[target.id] = 0, hit
		}
		if misses[target.id] >= 4 {
			// Never into the body: from inside it the arc has no direction to judge.
			reach[target.id] = math.Max(half+0.6, want-0.4)
			misses[target.id] = 0
		}
		if err := p.c.send(protocol.EncodeAttackRequest(protocol.AttackRequest{
			Slot: mainHandSlot, ClientTick: p.c.tick(),
		})); err != nil {
			return err
		}
		p.lastSwing = time.Now()
	}
}

// mainHandSlot is the worn main hand: the last slot of the table since V44.
const mainHandSlot = uint8(protocol.InventorySlots - 1)

// cut clears one cobweb with the hand the way a player holds the button: a MineRequest
// every tick until the server's BlockUpdate says the cell is air.
func (p *pilot) cut(ctx context.Context, at cell) error {
	defer func() {
		_ = p.c.send(protocol.EncodeMineRequest(protocol.MineRequest{HasPos: true, Pos: toWire(at), Active: false, ClientTick: p.c.tick(), Slot: mainHandSlot}))
	}()
	self := p.c.self()
	dx, dz := float64(at[0])+.5-self.pos[0], float64(at[2])+.5-self.pos[2]
	dy := float64(at[1]) + .5 - (self.pos[1] + game.PlayerHeight/2)
	p.c.setIntent(intent{yaw: yawToward(dx, dz), pitch: math.Atan2(dy, math.Hypot(dx, dz))})
	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		if b, ok := p.c.blockAt(at); ok && b != world.Cobweb {
			return nil
		}
		if err := p.c.send(protocol.EncodeMineRequest(protocol.MineRequest{
			HasPos: true, Pos: toWire(at), Active: true, ClientTick: p.c.tick(), Slot: mainHandSlot,
		})); err != nil {
			return err
		}
		if err := p.tickWait(ctx); err != nil {
			return err
		}
	}
	return fmt.Errorf("the cobweb at %v did not come down", at)
}

// use pulls a lever or touches a rune stone. A success has no reply of its own — the
// answer is the BlockUpdates it causes — so it waits for the cell to change, and reads a
// refusal off the ActionRefused the server sends instead.
func (p *pilot) use(ctx context.Context, at cell) error {
	before := p.c.updateCount(at)
	drain(p.c.refusals)
	self := p.c.self()
	dx, dz := float64(at[0])+.5-self.pos[0], float64(at[2])+.5-self.pos[2]
	dy := float64(at[1]) + .5 - (self.pos[1] + game.PlayerHeight/2)
	p.c.setIntent(intent{yaw: yawToward(dx, dz), pitch: math.Atan2(dy, math.Hypot(dx, dz))})
	if err := p.c.send(protocol.EncodeMechanismUseRequest(protocol.MechanismUseRequest{
		HasPos: true, Pos: toWire(at), ClientTick: p.c.tick(),
	})); err != nil {
		return err
	}
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		if p.c.updateCount(at) != before {
			return nil
		}
		select {
		case refusal := <-p.c.refusals:
			return fmt.Errorf("the mechanism at %v was refused: %s", at, refusal)
		default:
		}
		if err := p.tickWait(ctx); err != nil {
			return err
		}
	}
	return fmt.Errorf("the mechanism at %v changed nothing", at)
}

func toWire(c cell) [3]int32 { return [3]int32{int32(c[0]), int32(c[1]), int32(c[2])} }

// drain empties a channel of anything already waiting in it.
func drain[T any](ch chan T) {
	for {
		select {
		case <-ch:
		default:
			return
		}
	}
}
