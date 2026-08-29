package game

import (
	"context"
	"errors"
	"fmt"
	"math"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// EditReach is how far a player may reach to change a voxel, in blocks, measured from
// the centre of their collision box to the centre of the target voxel.
//
// # Why from the centre of the body
//
// Because that point is the server's to compute. Where the *eyes* sit inside a body is a
// rendering decision the client owns and says so — `EYE_HEIGHT` in
// `client/src/player/constants.rs` is documented as "client-owned, not a mirror" — so a
// reach measured from a camera would be a reach the server could only evaluate by
// copying a constant it does not own. The box centre is PlayerHeight/2 above the feet,
// derived from a number the server already states, and it makes the limit symmetric:
// four blocks up costs what four blocks along costs.
//
// Euclidean, not per axis. A per-axis test with the same number would let a corner
// diagonal reach 4.5·√3 ≈ 7.8 blocks, which is a different rule wearing this one's
// value.
//
// # Why 4.5
//
// Derived from what a standing player has to be able to touch, not picked:
//
//   - the block under their feet is 1.4 away and the block above their head 1.6, so
//     digging down and roofing over work from a standstill;
//   - a shaft is dug from its edge, so the reach has to cover a voxel one column out and
//     three down — √(1.0² + 3.4²) ≈ 3.5;
//   - and it must stay far below ChunkSize (32). An edit is resolved against the chunk
//     it lands in, and a reach comparable to a chunk would let a player change terrain
//     their session has never been sent. 4.5 is about an eighth of a chunk.
//
// Nothing on the wire carries this number. A client may aim as far as it likes; a
// request past the reach is refused in silence, like every other refusal.
const EditReach = 4.5

// ErrBreakActionWithdrawn distinguishes the retired direct-break wire value from
// ordinary refused edits. A session treats it as a protocol error: MineRequest is
// now the only client intent that can make a voxel become Air.
var ErrBreakActionWithdrawn = errors.New("EditAction.Break is withdrawn; use MineRequest")

// ErrWarded is the one refused edit and the one refused mining intent a player is told
// about.
//
// A sentinel lets the session route on identity while every other refusal remains silent.
// A ward is the exception because retrying cannot explain why that ground is unavailable.
var ErrWarded = errors.New("the ground is warded by another player's runestone")

// Editor is the world an edit is applied to.
//
// **Separate from Terrain because it is allowed to block.** Terrain is read on the tick
// goroutine and answers only from what is already resident; an edit runs on a session
// goroutine and needs a definite answer about one voxel, so it may generate the chunk and
// wait for it. Handing the tick loop one of these would be handing it a chunk generation.
//
// One method, and it is deliberately not a read followed by a write: allow carries the
// legality test into the same critical section as the change. guard runs after any
// generation and before that critical section, which lets an inventory-backed edit
// protect its stack without holding the inventory lock while a chunk is generated.
type Editor interface {
	// ApplyGuarded writes block at a world voxel if guard and allow accept it, and
	// returns either callback's error unchanged when it does not.
	//
	// guard runs after generation and outside the cache's locks. It may leave a
	// caller-owned lock held on success; it must unwind itself on error, and the
	// caller releases the success lock after ApplyGuarded returns.
	// allow must not block and must not reach for another lock — it runs inside the chunk
	// cache's composition lock.
	ApplyGuarded(ctx context.Context, x, y, z int64, block world.Block, guard func() error, allow func(current world.Block) error) error
}

// EditResult describes an accepted edit, in the terms its consumers need: the voxel as
// the wire addresses it, what stands there now, and the chunk that changed — which is
// the question the broadcast asks.
type EditResult struct {
	Pos       [3]int32
	Block     world.Block
	Chunk     world.Coord
	Inventory *protocol.InventoryState
}

// Edit resolves one placement BlockEditRequest and applies it if it is legal.
//
// The world's counterpart to Submit. An ordinary value error is a silent refusal:
// the caller logs it at debug and carries on reading, and **nothing is sent back to
// the client**. There is no rejection message in the contract, deliberately — the
// absence of a BlockUpdate is the answer, and a client must never treat its own
// request as applied. ErrBreakActionWithdrawn is the one structural exception and
// tells the session to close on a retired direct-break frame.
//
// The order of the checks is part of the design rather than an accident of writing:
//
//  1. Facts about the frame — a position at all, `Place` as its action, and a slot
//     inside the announced inventory. The retired `Break` value is a protocol
//     error; mining owns every transition to Air.
//  2. Reach, against the position **the server computed**. Nothing in the request says
//     where the client is, so there is no claim to disbelieve. This runs before any
//     world read, and that is what stops a request naming a voxel on the far side of the
//     world from making the server generate the chunk around it.
//  3. Whether a player is standing in the target.
//  4. The target chunk is generated before the inventory lock is acquired.
//  5. The slot is revalidated under that lock, then the voxel's own legality is
//     checked inside the write itself. The inventory lock remains held through
//     the slot change.
//
// The simulation's lock is taken twice and held across none of it. That is not tidiness:
// Apply can wait on a chunk being generated, and a tick blocked behind an edit is a tick
// every connected player misses.
func (p *Player) Edit(ctx context.Context, req protocol.BlockEditRequest) (EditResult, error) {
	// Validate the action before every other field so the retired Break value is
	// always reported to the session as the structural protocol violation it is,
	// even when the same frame also omitted its position.
	if err := validateEditIntent(req); err != nil {
		return EditResult{}, err
	}
	if !req.HasPos {
		// Absent rather than zero: schemas/world.fbs refuses to read a missing coordinate
		// as the origin, because the origin is a real place somebody would then have
		// edited without naming it.
		return EditResult{}, errors.New("the request carries no position")
	}
	target := [3]int64{int64(req.Pos[0]), int64(req.Pos[1]), int64(req.Pos[2])}

	p.sim.mu.Lock()
	origin := p.pos
	// The reach is read in the same critical section as the position it is measured
	// from, and for the same reason: both are answers about this player at one instant,
	// and a sky sampled after the lock was dropped could belong to a later tick than the
	// position it was about to be compared against.
	reach := p.reachLocked()
	actErr := p.cannotActLocked()
	// Read under this lock rather than after it, because the ward map is simulation state
	// and is rebuilt under exactly this mutex. This first read refuses the request before
	// it can generate terrain; the post-generation guard below reads again and keeps the
	// answer stable through the write.
	warded := p.sim.wardedAgainstLocked(target, p.playerID)
	p.sim.mu.Unlock()

	if actErr != nil {
		// Before the reach check and before any world read, so a dead player's request
		// cannot make the server generate a chunk. Refused rather than fatal, exactly as
		// a movement or mining intent from a corpse is.
		return EditResult{}, actErr
	}

	if distance := distanceToVoxel(origin, target); distance > reach {
		return EditResult{}, fmt.Errorf("the target is %.2f blocks from the player, past the reach of %.1f", distance, reach)
	}

	// Beside the reach check, and deliberately **not** inside allowPlacement below.
	// That predicate is evaluated inside the critical section that replaces the voxel,
	// where every chunk being composed anywhere in the server is waiting — it must stay
	// pure and must not block, and a ward lives behind a different lock. The ward is a
	// rule about *who is asking*, which the block that is there cannot answer anyway.
	if warded {
		return EditResult{}, fmt.Errorf("%w at %v", ErrWarded, target)
	}

	// Last thing before the write, and it is a check rather than a guarantee: a tick
	// can still move a player into the voxel in the microseconds between this and the
	// write. The consequence is bounded — moveAndCollide refuses to move a player who
	// is already inside a solid rather than teleporting them out, so they stand still
	// until the block is mined again — and closing the window entirely would mean
	// holding the simulation's lock across a chunk generation.
	if entityID, occupied := p.sim.voxelHoldsAPlayer(target); occupied {
		return EditResult{}, fmt.Errorf("entity %d is standing in the target voxel", entityID)
	}

	// Refuse an empty or non-placeable slot before generating anything. This is a
	// snapshot, not the spend: the guard below revalidates the named slot after
	// generation and keeps it locked through the write and count change.
	p.inventory.mu.Lock()
	placingItem, placing, err := itemToPlaceLocked(&p.inventory, req.Slot)
	p.inventory.mu.Unlock()
	if err != nil {
		return EditResult{}, err
	}

	inventoryLocked := false
	simulationLocked := false
	guard := func() error {
		// Liveness and the ward again, and these are the checks that matter. The ones above run before
		// the chunk is generated, which is what stops a corpse's request from making the
		// server generate terrain and reject an already-claimed target cheaply — but
		// generation can take milliseconds, and either fact can change in that window.
		//
		// ApplyGuarded calls this after generation and before taking the cache composition
		// lock. Leaving sim.mu held on success therefore linearizes the ward with the write
		// without holding it across generation or reaching from allowPlacement into the
		// simulation. The inventory lock is nested under it, in the tick's established
		// sim.mu -> inventory.mu order; the reverse order would deadlock.
		p.sim.mu.Lock()
		actErr := p.cannotActLocked()
		if actErr != nil {
			p.sim.mu.Unlock()
			return fmt.Errorf("the player became unable to act while the target chunk was loading: %w", actErr)
		}
		if p.sim.wardedAgainstLocked(target, p.playerID) {
			p.sim.mu.Unlock()
			return fmt.Errorf("%w at %v", ErrWarded, target)
		}
		simulationLocked = true

		p.inventory.mu.Lock()
		itemID, block, guardErr := itemToPlaceLocked(&p.inventory, req.Slot)
		if guardErr != nil {
			p.inventory.mu.Unlock()
			p.sim.mu.Unlock()
			simulationLocked = false
			return guardErr
		}
		if itemID != placingItem || block != placing {
			p.inventory.mu.Unlock()
			p.sim.mu.Unlock()
			simulationLocked = false
			return fmt.Errorf("inventory slot %d changed while its target chunk was loading", req.Slot)
		}
		inventoryLocked = true
		return nil
	}

	applyErr := p.sim.editor.ApplyGuarded(ctx, target[0], target[1], target[2], placing, guard, allowPlacement)
	if applyErr != nil {
		if inventoryLocked {
			p.inventory.mu.Unlock()
		}
		if simulationLocked {
			p.sim.mu.Unlock()
		}
		return EditResult{}, applyErr
	}

	// The exact slot and item were revalidated under the lock the guard left held,
	// so this cannot fail without an internal invariant being broken.
	if !p.inventory.consumeOneLocked(req.Slot, placingItem) {
		panic("game: guarded placement could not consume its inventory item")
	}

	state := p.inventory.stateLocked()
	p.inventory.mu.Unlock()
	p.sim.mu.Unlock()
	p.sim.invalidateMining(req.Pos)

	return EditResult{
		Pos:       req.Pos,
		Block:     placing,
		Chunk:     world.ChunkOf(target[0], target[1], target[2]),
		Inventory: &state,
	}, nil
}

// breakMined is the one transition to Air. The expected block is checked inside the
// world write, and the yield becomes a thing lying in the world rather than a number
// appearing in a menu: the drop table names an item, spawnDrop puts it at the centre
// of the voxel that is now gone, and a player collects it by walking over it.
//
// **It takes no inventory lock at all**, and that is the change rather than an
// omission. The lock used to span this write because the write and the insertion had
// to be one operation; nothing here touches a slot now, so there is nothing to
// serialise and no reason to make a mining completion wait behind an inventory move.
// The pickup that eventually spends the drop takes it on the tick, without waiting —
// see Player.collect.
//
// The spawn is deliberately after the write and outside it. A drop that appeared
// before the voxel became Air would be a drop for a break that could still be refused,
// and allow may not reach for another lock: it runs inside the chunk cache's
// composition lock.
func (p *Player) breakMined(ctx context.Context, pos [3]int32, expected world.Block) (EditResult, error) {
	target := mineTarget(pos)

	applyErr := p.sim.editor.ApplyGuarded(ctx, target[0], target[1], target[2], world.Air, nil, func(current world.Block) error {
		if current != expected {
			return fmt.Errorf("%w: expected block %d, found %d", ErrMiningTargetChanged, uint16(expected), uint16(current))
		}
		return nil
	})
	if applyErr != nil {
		return EditResult{}, applyErr
	}

	// ItemNone is Leaves' explicit yield and an unlisted block's implicit one: both
	// mean the world keeps nothing, so nothing is spawned. spawnDrop refuses it too;
	// the test here is so that a block with no drop costs no identity.
	if dropped := itemDroppedBy(expected); dropped != ItemNone {
		p.sim.spawnDrop(dropped, 1, target)
	}
	if amount := blockExperience[expected]; amount > 0 {
		// Mining completes off the tick, so unlike combat and crafting it arrives without
		// Sim.mu. No inventory lock is held here; taking only the simulation lock preserves
		// the repository's sim.mu -> inventory.mu order.
		p.sim.mu.Lock()
		p.sim.awardExperienceLocked(p, uint32(amount))
		p.sim.log.Debug("experience awarded",
			"entity_id", p.entityID, "source", "block break", "amount", amount,
			"block", uint16(expected))
		p.sim.mu.Unlock()
	}

	// Anything that was standing on this voxel stops standing. Here rather than in the
	// simulation's tick because this is the moment the ground stopped being ground, and
	// the two steps are split for the reason the yield above is: the registry change is
	// one critical section and the drops that follow take the same lock again.
	p.sim.dropCollapsed(p.sim.collapseStructuresAt(target))

	p.sim.invalidateMining(pos)

	return EditResult{
		Pos:   pos,
		Block: world.Air,
		Chunk: world.ChunkOf(target[0], target[1], target[2]),
	}, nil
}

// validateEditIntent rejects scalar values that are invalid before any world read.
func validateEditIntent(req protocol.BlockEditRequest) error {
	switch req.Action {
	case vnet.EditActionBreak:
		return ErrBreakActionWithdrawn
	case vnet.EditActionPlace:
		if req.Slot >= protocol.InventorySlots {
			return fmt.Errorf("inventory slot %d is outside the announced %d slots", req.Slot, protocol.InventorySlots)
		}
		return nil
	default:
		// EditAction.Unknown lands here, and that is the entire reason the enum has a
		// zero member: FlatBuffers decodes an absent scalar as zero, so a request that
		// omits its action arrives as Unknown. Guessing would invent intent.
		return fmt.Errorf("action %s is not an edit a client may ask for", req.Action)
	}
}

// itemToPlaceLocked resolves one real slot through the server-only item registry.
// The caller holds inventory.mu.
func itemToPlaceLocked(inventory *inventory, slot uint8) (ItemID, world.Block, error) {
	stack, present := inventory.stackAtLocked(slot)
	if !present {
		return ItemNone, world.Air, fmt.Errorf("inventory slot %d is empty", slot)
	}
	block, placeable := blockPlacedBy(stack.item)
	if !placeable {
		return ItemNone, world.Air, fmt.Errorf("item %d in inventory slot %d places no block", uint16(stack.item), slot)
	}
	return stack.item, block, nil
}

// allowPlacement is the target voxel's own legality, evaluated against the block that is
// there at the moment of the write.
//
// Pure, and it has to be: it runs inside the chunk cache's composition lock, where
// anything that blocks would stall every chunk being composed anywhere in the server.
//
// **Water is displaced, not obstructed.** A voxel of water is not a thing anybody is
// holding — it has no item and nothing but the generator ever writes one — so a
// block put into it replaces it, and the result is one delta like any other edit.
// That is the whole of what "water is static" costs the edit path: no flow to
// recompute, no neighbours to notify, and no source to remember.
func allowPlacement(current world.Block) error {
	if current != world.Air && !world.Fluid(current) {
		return fmt.Errorf("the target voxel already holds block %d", uint16(current))
	}
	return nil
}

// voxelHoldsAPlayer reports whether any player's collision box overlaps the voxel, and
// names the offender.
//
// The lowest entity id among them rather than the first the map yields: map order is
// random, and a log line that names a different player each run is a log line nobody can
// correlate. Pure arithmetic under the simulation's lock, so nothing here blocks.
func (s *Sim) voxelHoldsAPlayer(v [3]int64) (uint64, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()

	var (
		blocker  uint64
		occupied bool
	)
	for id, p := range s.players {
		if !voxelOverlapsBox(v, playerBox(p.pos)) {
			continue
		}
		if !occupied || id < blocker {
			blocker, occupied = id, true
		}
	}
	return blocker, occupied
}

// voxelOverlapsBox reports whether the unit voxel at v intersects b.
//
// Half-open on both sides, matching box: an extent that ends exactly where the voxel
// begins does not occupy it. That is the same convention that lets a player rest on a
// surface without being inside the block above their head, and here it is what lets a
// block be placed flush against a player rather than one gap away from them.
func voxelOverlapsBox(v [3]int64, b box) bool {
	for axis := range 3 {
		lo := float64(v[axis])
		if b.min[axis] >= lo+1 || b.max[axis] <= lo {
			return false
		}
	}
	return true
}

// distanceToVoxel is the distance from the centre of the player's collision box at pos
// to the centre of the unit voxel at v, in blocks.
//
// float64 throughout, and the widening of v is exact: a BlockCoord axis is an int32, and
// every int32 is representable in a float64.
func distanceToVoxel(pos [3]float64, v [3]int64) float64 {
	b := playerBox(pos)

	var sum float64
	for axis := range 3 {
		centre := (b.min[axis] + b.max[axis]) / 2
		d := float64(v[axis]) + 0.5 - centre
		sum += d * d
	}
	return math.Sqrt(sum)
}
