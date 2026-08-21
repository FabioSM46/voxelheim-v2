package game

import (
	"slices"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// DropSize is the edge of a dropped item's collision box, in blocks.
//
// It must stay in sync with DROP_EDGE in `client/src/player/drops.rs`, which is the
// cube the client draws. Small enough that a drop fits anywhere the block it came
// from did, and cubic rather than player-shaped because nothing about an item on the
// ground is taller than it is wide.
const DropSize = 0.25

// DropPickupRadius is how close a player's body has to come to a drop to collect it,
// in blocks, measured between the two boxes.
//
// There is no key, no aim and no request: walking past is the whole interaction, so
// the number is what "walking past" means. One block is the reach of a body that
// brushes it, and it is deliberately far below EditReach — collecting is something
// you do with your feet, not something you do at arm's length.
const DropPickupRadius = 1.0

// DropLifetime is how long an uncollected drop stays in the world.
//
// Whether or not anybody is nearby: a drop is simulation state, not a render, so a
// world nobody is standing in still tidies itself. Five minutes is long enough to
// walk back for something and short enough that a mining spree does not leave a
// permanent field of entities.
const DropLifetime = 5 * time.Minute

// dropMergeRadius is how close two drops of the same item have to be to become one,
// in blocks, measured between their boxes.
//
// A block rather than something smaller, because the case merging exists for is a
// mining spree, and a spree breaks *adjacent* voxels: a radius under one block would
// leave exactly the drops it is meant to combine sitting one block apart for ever.
const dropMergeRadius = 1.0

// dropPickupDelayTicks is how many ticks a drop cannot be collected for after it
// appears.
//
// Half a second at the default rate, and its job is to make the drop *visible*: a
// block broken at your feet would otherwise be collected on the tick it appeared,
// which looks exactly like the inventory insert this issue replaced. The tenth tick
// is still too early; the eleventh is the first that may collect.
const dropPickupDelayTicks = 10

// dropBody is the box a drop collides with, and the only reason moveAndCollide takes
// a body at all.
var dropBody = body{width: DropSize, height: DropSize}

// itemDrop is one stack of items lying in the world.
//
// The first entity in this simulation that is not a player, and deliberately shaped
// like the ones that follow rather than like an inventory operation: the tick steps
// it, visibility streams it, and it has a lifetime. A mob will want all three.
//
// Every field is guarded by Sim.mu. Nothing here is persisted — a restart loses
// whatever is lying on the ground, because a drop is a moment in a simulation rather
// than a change to the world, and world.Deltas records changes to the world.
type itemDrop struct {
	entityID uint64
	item     ItemID
	count    uint16

	// pos is the standing position of the drop's box — its minimum in y, its centre
	// in x and z — exactly as a player's position is. See wirePos for what the client
	// is told instead.
	pos [3]float64

	// fallSpeed is the drop's only velocity: blocks per second, negative downwards.
	// Nothing throws a drop, so there is no horizontal component to carry and no
	// field here with no reader.
	fallSpeed float64

	// chunk is the chunk pos falls in, kept beside the position for the same reason a
	// player's is: visibility asks for it once per viewer per tick.
	chunk world.Coord

	// age is how many ticks this drop has been stepped. It is what both the pickup
	// delay and the despawn are counted in — ticks rather than wall clock, because the
	// simulation's only clock is Step and a drop must not age faster on a busy server.
	age int
}

// dropLifetimeTicks converts DropLifetime into the tick count Step counts in.
func dropLifetimeTicks(tickRate uint8) int {
	return int(DropLifetime/time.Second) * int(tickRate)
}

// spawnDrop puts one stack of items in the world at the centre of a voxel and returns
// the identity it was given.
//
// Called from a session goroutine, off the tick, immediately after the world write
// that produced the yield. It refuses an empty or unregistered item rather than
// creating an entity the wire forbids: schemas/player.fbs states that a drop's
// item_id and count are never zero.
func (s *Sim) spawnDrop(item ItemID, count uint16, voxel [3]int64) (uint64, bool) {
	if item == ItemNone || count == 0 {
		return 0, false
	}
	if _, registered := itemByID(item); !registered {
		// Reachable only by adding a block to the drop table without adding its item to
		// the registry, which is a server bug rather than an outcome — and a silent one,
		// because the player would simply see a block yield nothing.
		s.log.Error("drop refused: the item is not registered",
			"item_id", uint16(item), "count", count, "voxel", voxel)
		return 0, false
	}

	// From the counter that mints player identities, so no id ever names both a player
	// and a drop. Minted before the lock because it is an atomic add and the identity
	// space does not belong to the simulation.
	drop := &itemDrop{
		entityID: s.mintEntityID(),
		item:     item,
		count:    count,
		pos:      dropSpawnPos(voxel),
	}
	drop.chunk = chunkAt(drop.pos)

	s.mu.Lock()
	defer s.mu.Unlock()
	s.drops[drop.entityID] = drop
	return drop.entityID, true
}

// dropSpawnPos centres a drop's box in the voxel that was broken.
//
// Centred rather than resting on the voxel's floor: the wire position is then exactly
// the centre of the voxel, which is where the player last saw the block, and the drop
// falls the last fraction of a block on the tick after it appears like anything else
// that is not standing on something.
func dropSpawnPos(voxel [3]int64) [3]float64 {
	return [3]float64{
		float64(voxel[0]) + 0.5,
		float64(voxel[1]) + 0.5 - DropSize/2,
		float64(voxel[2]) + 0.5,
	}
}

// wirePos is the centre of the drop's box.
//
// The one place the drop's own convention is translated, and it is translated because
// the client draws a DROP_EDGE cube *centred* on the position it is sent
// (`client/src/player/drops.rs`). The simulation keeps the standing position every
// other entity uses; the half-height is added here and nowhere else.
func (d *itemDrop) wirePos() [3]float32 {
	return toWire([3]float64{d.pos[0], d.pos[1] + DropSize/2, d.pos[2]})
}

func (d *itemDrop) box() box { return dropBody.boxAt(d.pos) }

// sortedDropsLocked is every drop, ordered by identity.
//
// Sorted for the reason the players are, and for one more: map order would make the
// encoded bytes of a snapshot differ run to run, and it would leave which of two
// merging drops survives up to a hash seed. compareEntityIDs rather than a subtraction,
// for the reason it documents.
func (s *Sim) sortedDropsLocked() []*itemDrop {
	drops := make([]*itemDrop, 0, len(s.drops))
	for _, d := range s.drops {
		drops = append(drops, d)
	}
	slices.SortFunc(drops, func(a, b *itemDrop) int { return compareEntityIDs(a.entityID, b.entityID) })
	return drops
}

// advanceDropsLocked ages every drop by one tick, despawns the ones that have run out
// of time, and falls the rest. It returns the survivors in the order it was given.
func (s *Sim) advanceDropsLocked(drops []*itemDrop) []*itemDrop {
	kept := drops[:0]
	for _, d := range drops {
		d.age++
		if d.age >= s.dropLifetime {
			delete(s.drops, d.entityID)
			continue
		}
		d.step(s.dt, s.terrain)
		kept = append(kept, d)
	}
	return kept
}

// step falls one drop by one tick. Called with sim.mu held.
//
// The player's integrator and the player's collision, with a smaller box and no
// intent to read. A drop over a chunk that is not resident therefore holds where it
// is with no accumulated speed, by exactly the rule that keeps a player waiting on
// terrain from arriving with three seconds of fall in them: an absent chunk is solid,
// moveAndCollide refuses a move that starts inside a solid, and a blocked axis zeroes
// the velocity.
func (d *itemDrop) step(dt float64, terrain Terrain) {
	d.fallSpeed = max(d.fallSpeed-Gravity*dt, -TerminalFallSpeed)

	pos, blocked := moveAndCollide(terrain, dropBody, d.pos, [3]float64{0, d.fallSpeed * dt, 0})
	d.pos = pos
	if blocked[1] {
		d.fallSpeed = 0
	}
	d.chunk = chunkAt(d.pos)
}

// mergeDropsLocked folds nearby drops of the same item into one, up to that item's
// stack limit, and returns the survivors.
//
// O(drops²) within the radius, knowingly, and the same judgement Step already records
// for snapshot visibility: a spatial index is worth building when the quadratic term
// matters and not one issue before.
//
// The older drop is always the one that survives, because the list is ordered by
// identity and identities only increase. That is what keeps a pile from renewing its
// own lifetime: a spree that keeps merging into the same drop still despawns five
// minutes after the first block was broken.
func (s *Sim) mergeDropsLocked(drops []*itemDrop) []*itemDrop {
	for i, into := range drops {
		if into.count == 0 {
			continue
		}
		limit := stackLimit(into.item)
		for _, other := range drops[i+1:] {
			if into.count >= limit {
				break
			}
			if other.count == 0 || other.item != into.item {
				continue
			}
			if boxDistance(into.box(), other.box()) > dropMergeRadius {
				continue
			}

			moved := min(other.count, limit-into.count)
			into.count += moved
			other.count -= moved
			if other.count == 0 {
				delete(s.drops, other.entityID)
			}
		}
	}
	return keepLiveDrops(drops)
}

// collectDropsLocked hands every drop a player is standing near to that player, and
// returns what is still on the ground.
//
// O(players × drops), by the same judgement as the merge above. Players are visited
// in identity order so that two players reaching one drop on the same tick resolve the
// same way every run.
func (s *Sim) collectDropsLocked(players []*Player, drops []*itemDrop) []*itemDrop {
	for _, d := range drops {
		// The delay is what makes a drop something you see before you have it. Counted in
		// ticks from the first one that moved it, so a busy server does not shorten it
		// and an empty one does not stretch it.
		if d.age <= dropPickupDelayTicks {
			continue
		}

		for _, p := range players {
			if d.count == 0 {
				break
			}
			if boxDistance(d.box(), playerBox(p.pos)) > DropPickupRadius {
				continue
			}

			remaining, took := p.collect(d.item, d.count)
			// Whatever did not fit stays exactly where it was, with its count reduced.
			// A full pack is a reason to leave something on the ground, never a reason
			// to destroy it.
			d.count = remaining
			if took {
				p.inventoryDirty = true
			}
			if d.count == 0 {
				delete(s.drops, d.entityID)
			}
		}
	}
	return keepLiveDrops(drops)
}

// collect inserts as much of one stack as fits and reports the remainder, and whether
// anything moved at all.
//
// **It never waits for the inventory lock**, and that is what lets a pickup happen on
// the tick at all: Sim.mu is held for the whole tick and nothing under it may block,
// while the inventory lock is held by session goroutines across an authoritative world
// write. TryLock makes the pair deadlock-free by construction rather than by lock
// ordering — a contended inventory simply leaves the drop on the ground, and the next
// tick tries again fifty milliseconds later.
func (p *Player) collect(item ItemID, count uint16) (uint16, bool) {
	if !p.inventory.mu.TryLock() {
		return count, false
	}
	defer p.inventory.mu.Unlock()

	remaining := p.inventory.insertLocked(item, count)
	return remaining, remaining != count
}

// offerInventoryLocked delivers the whole authoritative inventory after a pickup
// changed it, and keeps trying until a tick succeeds.
//
// A pickup is decided on the tick, so it can only use the tick's non-blocking seam —
// and unlike a snapshot, an inventory state is not superseded by the next tick's.
// Durable in the same shape as a mining reset therefore: the flag survives a full
// queue, and the retry re-reads the live slots rather than resending a stale encoding,
// so a run of pickups collapses into whichever state arrives.
//
// **The inventory lock is held across the deliver**, which costs nothing — deliver is
// the non-blocking send — and buys one ordering: a session goroutine that is waiting
// for the lock cannot slip its own newer state into the queue ahead of the frame this
// encoded. Without that, a placement resolving in the gap between the encode and the
// enqueue would leave the client's last word about its own pack a state older than the
// one it already had, with inventoryDirty cleared and nothing to resend it.
//
// **It does not make the ordering safe, and no comment here should say it does.** The
// mirror case is untouched: a session captures its state *under* the lock and encodes
// and enqueues it after releasing (Player.Edit, Player.MoveInventory, and their callers
// in session.go), so a pickup landing in that gap still delivers the newer state first.
// That reorder predates drops — it was reachable between a placement and a mined
// break's insertion — and closing it means one sender or a version on the state, which
// is its own issue. It is recorded under "Known gaps" in server/AGENTS.md.
func (p *Player) offerInventoryLocked() {
	if !p.inventoryDirty {
		return
	}
	if !p.inventory.mu.TryLock() {
		return
	}
	defer p.inventory.mu.Unlock()

	if p.deliver(protocol.EncodeInventoryState(p.inventory.stateLocked())) {
		p.inventoryDirty = false
		return
	}
	p.sim.log.Debug("inventory state deferred: the session's outbound queue is full",
		"entity_id", p.entityID)
}

// keepLiveDrops filters out the drops whose count reached zero, in place.
func keepLiveDrops(drops []*itemDrop) []*itemDrop {
	kept := drops[:0]
	for _, d := range drops {
		if d.count > 0 {
			kept = append(kept, d)
		}
	}
	return kept
}

// stackLimit is how many of one item fit in a stack, and zero for an item no registry
// entry describes — which stops an unknown item from merging at all rather than
// merging without a bound.
func stackLimit(id ItemID) uint16 {
	if definition, ok := itemByID(id); ok {
		return definition.maxStack
	}
	return 0
}

// DropCount is how many items are lying in the world.
func (s *Sim) DropCount() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return len(s.drops)
}

// dropStates is the wire form of every drop, in the order it was given.
func dropStates(drops []*itemDrop) []protocol.ItemDropState {
	states := make([]protocol.ItemDropState, len(drops))
	for i, d := range drops {
		states[i] = protocol.ItemDropState{
			EntityID: d.entityID,
			Pos:      d.wirePos(),
			ItemID:   uint16(d.item),
			Count:    d.count,
		}
	}
	return states
}
