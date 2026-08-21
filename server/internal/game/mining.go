package game

import (
	"context"
	"errors"
	"fmt"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// ErrMiningTargetChanged reports that a completed mining attempt lost the race to
// another world edit before its off-tick write. CompleteMining turns this one
// refusal into the durable zero progress frame a server-caused reset requires.
var ErrMiningTargetChanged = errors.New("the mining target changed before completion")

// miningState is one player's progress between ticks. Every field is guarded by
// Sim.mu; requests replace or refresh it and Step is its only clock.
type miningState struct {
	pos       [3]int32
	block     world.Block
	cost      int
	progress  int
	idleTicks int
	invalid   bool
}

// miningReset is a server-caused state transition that has not reached the
// session's outbound queue yet. It remains here across ticks when that queue is
// full; unlike an ordinary progress fraction, zero is not disposable because no
// later snapshot is guaranteed to supersede it.
type miningReset struct {
	pos    [3]int32
	tick   uint64
	reason string
}

// MiningCompletion is an opaque instruction produced by the tick and consumed by
// the owning session's mining worker. Its fields are deliberately private: only the
// simulation may decide that a target has paid its hardness cost.
type MiningCompletion struct {
	pos    [3]int32
	block  world.Block
	serial uint64
	tick   uint64
}

// Pos reports the voxel this completion names, for delivery and diagnostics.
func (c MiningCompletion) Pos() [3]int32 { return c.pos }

// hardnessTicks is the one authoritative cost table for mining by hand.
//
// Tools will multiply the returned cost; they do not need another state machine or
// another table. Air and unknown ids are not breakable and therefore have no cost.
func hardnessTicks(block world.Block) (int, bool) {
	switch block {
	case world.Leaves:
		return 2, true
	case world.Grass:
		return 3, true
	case world.Dirt, world.Snow:
		return 6, true
	case world.Log:
		return 12, true
	case world.Stone:
		return 20, true
	case world.CoalOre:
		return 30, true
	case world.IronOre:
		return 40, true
	default:
		return 0, false
	}
}

// Mine accepts one refresh, target change or cancellation of mining intent.
// targetVisible is the session's authoritative answer to whether it has actually
// delivered the target chunk; the simulation cannot derive that fact from distance.
//
// Messages never advance progress. They only reset idleTicks, replace the target or
// clear it. Step is the sole clock, so twenty accepted refreshes between two ticks
// cost exactly the same amount of hardness as one.
func (p *Player) Mine(req protocol.MineRequest, targetVisible bool) error {
	if !req.HasPos {
		return errors.New("the request carries no position")
	}

	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()

	if !p.alive() {
		// Refused, not fatal, for the same reason a dead player's movement is: the frame
		// is well formed and the client is entitled to keep sending while it waits out
		// the respawn it has been told about. dieLocked already dropped whatever was
		// being mined, so there is nothing here to cancel — only something to decline.
		return errors.New("the player is dead")
	}

	if p.haveMineTick && !newerTick(req.ClientTick, p.lastMineTick) {
		return fmt.Errorf("stale mining client tick %d; the newest accepted is %d", req.ClientTick, p.lastMineTick)
	}
	p.haveMineTick, p.lastMineTick = true, req.ClientTick

	// Once the tick has paid the full cost, releasing or changing the control cannot
	// undo that answer while its world write is in flight. The client keeps refreshing,
	// so a new target starts after the worker has resolved this one.
	if p.mineCompleting {
		return errors.New("a completed mining write is still in flight")
	}
	if p.mineReset != nil {
		return errors.New("a server mining reset is still waiting for delivery")
	}

	if !req.Active {
		p.setMiningLocked(nil)
		return nil
	}

	if p.mining != nil && p.mining.pos == req.Pos {
		if !targetVisible {
			// Losing the delivered chunk makes this request ineligible. It is a
			// client/session-side cancellation, so no zero progress frame is sent.
			p.setMiningLocked(nil)
			return errors.New("the target chunk has not been delivered to this session")
		}
		p.mining.idleTicks = 0
		return nil
	}

	// A different target cancels the old one before the new target is judged. This
	// reset came from the client's request and is deliberately silent even if the
	// replacement itself is refused.
	p.setMiningLocked(nil)
	if !targetVisible {
		return errors.New("the target chunk has not been delivered to this session")
	}

	target := mineTarget(req.Pos)
	if distance := distanceToVoxel(p.pos, target); distance > EditReach {
		return fmt.Errorf("the target is %.2f blocks from the player, past the reach of %.1f", distance, EditReach)
	}

	block, resident := p.sim.terrain.Block(target[0], target[1], target[2])
	if !resident {
		// No progress exists to hold yet. The client's next refresh may start it once
		// streaming has made the chunk resident; generating here would cross the same
		// non-blocking seam the tick is forbidden to cross.
		return errors.New("the target chunk is not resident")
	}
	cost, breakable := hardnessTicks(block)
	if !breakable {
		return fmt.Errorf("block %d at the target is not breakable", uint16(block))
	}

	p.setMiningLocked(&miningState{pos: req.Pos, block: block, cost: cost})
	return nil
}

// setMiningLocked replaces one player's active target and maintains the reverse
// index used by world edits. The caller holds Sim.mu.
func (p *Player) setMiningLocked(next *miningState) {
	if current := p.mining; current != nil {
		miners := p.sim.minersByPos[current.pos]
		delete(miners, p)
		if len(miners) == 0 {
			delete(p.sim.minersByPos, current.pos)
		}
	}

	p.mining = next
	if next == nil {
		return
	}
	miners := p.sim.minersByPos[next.pos]
	if miners == nil {
		miners = make(map[*Player]struct{})
		p.sim.minersByPos[next.pos] = miners
	}
	miners[p] = struct{}{}
}

// advanceMining advances at most one unit for one player. Called with Sim.mu held,
// after movement has produced the position whose reach applies this tick.
func (p *Player) advanceMining(tick uint64, terrain Terrain) {
	if p.mineReset != nil {
		// A reset accepted into the queue this tick still consumes this player's one
		// progress-frame opportunity. New mining is refused while it is pending, so
		// no positive frame can overtake it.
		p.offerMiningResetLocked()
		return
	}

	state := p.mining
	if state == nil {
		return
	}

	// A world edit records invalidation when it lands, rather than relying only on
	// the next sample. That catches a target changed away and back between ticks.
	if state.invalid {
		p.resetMining(state.pos, tick, "target block changed")
		return
	}

	// Same idle window and boundary as movement: exactly idleLimit ticks may reuse a
	// refresh, then "still held" stops being a fair reading. Silence is client-caused,
	// so clearing it sends no zero frame.
	if state.idleTicks >= p.sim.idleLimit {
		p.setMiningLocked(nil)
		return
	}
	state.idleTicks++

	target := mineTarget(state.pos)
	if distanceToVoxel(p.pos, target) > EditReach {
		p.resetMining(state.pos, tick, "target moved out of reach")
		return
	}

	block, resident := terrain.Block(target[0], target[1], target[2])
	if !resident {
		// A miss has no honest block answer. Hold the paid ticks and ask again next
		// tick; never reinterpret "not loaded" as either Air or the old block.
		return
	}
	if block != state.block {
		p.resetMining(state.pos, tick, "target block changed")
		return
	}

	state.progress++
	if state.progress >= state.cost {
		p.mineSerial++
		completion := MiningCompletion{pos: state.pos, block: state.block, serial: p.mineSerial, tick: tick}
		p.setMiningLocked(nil)
		p.mineCompleting = true

		// A single player can have only one completion in flight: Mine refuses a new
		// target until CompleteMining clears mineCompleting. The buffered send is thus
		// guaranteed to fit and remains non-blocking on the tick goroutine.
		select {
		case p.mineReady <- completion:
		default:
			p.sim.log.Error("mining completion queue invariant broken",
				"entity_id", p.entityID, "pos", completion.pos, "tick", tick)
		}
		return
	}

	progress := uint8(state.progress * 255 / state.cost)
	if progress == 0 {
		// All current costs are below 255, but keep the wire invariant local if a
		// future block becomes much harder: started progress is never encoded as zero.
		progress = 1
	}
	p.deliverMiningProgress(state.pos, progress, tick)
}

// invalidateMining records a world change for only the players currently mining pos.
// The reverse index is maintained atomically with Player.mining, so a player who
// starts after the edit is not falsely invalidated and unrelated sessions cost no
// work under the lock the tick needs.
func (s *Sim) invalidateMining(pos [3]int32) {
	s.mu.Lock()
	defer s.mu.Unlock()
	for player := range s.minersByPos[pos] {
		player.mining.invalid = true
	}
}

// resetMining clears one server-invalidated target and offers exactly one zero
// progress frame to the owning session. Client-caused resets clear state in Mine and
// never call this function.
func (p *Player) resetMining(pos [3]int32, tick uint64, reason string) {
	p.setMiningLocked(nil)
	p.queueMiningResetLocked(pos, tick, reason)
	p.offerMiningResetLocked()
}

// queueMiningResetLocked makes the reset durable until the outbound queue accepts
// it. Only one transition can be outstanding: active mining is cleared first and
// Mine refuses a replacement target while mineReset is non-nil.
func (p *Player) queueMiningResetLocked(pos [3]int32, tick uint64, reason string) {
	if p.mineReset != nil {
		panic("game: queued a second mining reset before delivering the first")
	}
	p.mineReset = &miningReset{pos: pos, tick: tick, reason: reason}
}

// offerMiningResetLocked tries the non-blocking session seam once. Failure keeps
// the transition pending for the next tick; success clears it before any new
// target may start, with FIFO ordering in the session queue keeping later progress
// behind the zero. The caller holds Sim.mu.
func (p *Player) offerMiningResetLocked() {
	reset := p.mineReset
	if reset == nil {
		return
	}
	if p.deliver(protocol.EncodeMineProgress(protocol.MineProgress{Pos: reset.pos, Progress: 0})) {
		p.mineReset = nil
		return
	}
	p.sim.log.Debug("mining reset deferred: the session's outbound queue is full",
		"entity_id", p.entityID, "pos", reset.pos, "tick", reset.tick, "reason", reset.reason)
}

func (p *Player) deliverMiningProgress(pos [3]int32, progress uint8, tick uint64) {
	if !p.deliver(protocol.EncodeMineProgress(protocol.MineProgress{Pos: pos, Progress: progress})) {
		p.sim.log.Debug("mining progress dropped: the session's outbound queue is full",
			"entity_id", p.entityID, "pos", pos, "tick", tick, "progress", progress)
	}
}

// NextMining blocks until the tick completes one target or ctx ends. It hands the
// blocking Editor work to a session-owned worker and therefore never runs on Step.
func (p *Player) NextMining(ctx context.Context) (MiningCompletion, error) {
	if err := ctx.Err(); err != nil {
		return MiningCompletion{}, err
	}
	select {
	case completion := <-p.mineReady:
		return completion, nil
	case <-ctx.Done():
		return MiningCompletion{}, ctx.Err()
	}
}

// CompleteMining performs the off-tick write for an opaque completion. It reuses
// the exact break path that owns drops, inventory and the atomic expected-block
// guard; the returned EditResult is broadcast by the session exactly like a place.
func (p *Player) CompleteMining(ctx context.Context, completion MiningCompletion) (EditResult, error) {
	p.sim.mu.Lock()
	held, joined := p.sim.players[p.entityID]
	valid := joined && held == p && p.mineCompleting && completion.serial == p.mineSerial
	p.sim.mu.Unlock()
	if !valid {
		return EditResult{}, errors.New("the mining completion no longer belongs to a live player")
	}

	result, err := p.breakMined(ctx, completion.pos, completion.block)
	p.sim.mu.Lock()
	if p.mineCompleting && completion.serial == p.mineSerial {
		if errors.Is(err, ErrMiningTargetChanged) {
			// Queue and offer the reset before clearing mineCompleting. If the queue is
			// full, mineReset remains the guard that keeps later progress behind it.
			p.queueMiningResetLocked(completion.pos, completion.tick, err.Error())
			p.offerMiningResetLocked()
		}
		p.mineCompleting = false
	}
	p.sim.mu.Unlock()
	return result, err
}

func mineTarget(pos [3]int32) [3]int64 {
	return [3]int64{int64(pos[0]), int64(pos[1]), int64(pos[2])}
}
