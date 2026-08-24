package game

import (
	"fmt"
	"math"

	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// Life is everything about one player that outlives their connection: where they
// stood, which way they faced, how much health and hunger they had, and every slot of their
// pack.
//
// # Why the type lives here and not in the store
//
// The store owns bytes; this package owns what those bytes are allowed to mean. Only
// this package holds the item registry and [PlayerMaxHealth], so a record's values
// can only be judged here — see [Life.Validate], which is the one place that judges
// them. internal/persist declares the same five values because it has to write them
// down; it deliberately does not re-derive the rules, because a second copy of a rule
// is a second rule the moment one of them is edited.
//
// # Slots are the wire's own shape
//
// [protocol.InventoryStack] rather than a private mirror of it, so a stored pack and
// the InventoryState a client is sent are the same value and a test can compare them
// directly. A fixed array rather than a slice: there are exactly
// [protocol.InventorySlots] of them by construction, so no reader has to check a
// length and no writer can produce a record with the wrong number of slots.
type Life struct {
	Pos    [3]float64
	Yaw    float64
	Health uint16
	Hunger uint16
	Slots  [protocol.InventorySlots]protocol.InventoryStack
}

// Validate reports the first way this life is not one the simulation could have
// produced.
//
// **A record is refused whole or restored whole.** There is no repair here and there
// must not be: a life that is half believed is a player standing somewhere plausible
// holding an item that does not exist, and the failure would surface as a mystery
// hours later rather than as a log line at the join it came from. The caller's answer
// to an error is to keep the file and admit the player as new.
//
// The store is trusted no more than a client is. Every rule below is one
// schemas/player.fbs already states for an InventoryState on the wire, applied to a
// file this process wrote — because "we wrote it" is a claim about the last build,
// not about the bytes on the disk now.
//
// # Finite is not the same as addressable
//
// Both float fields are bounded as well as tested for NaN and Inf, because every path
// out of this struct is narrower than the float64 it is stored in. A position is
// narrowed to float32 for the welcome's spawn and floored into an int64 for the chunk
// feed; a yaw is narrowed to float32 for every entity frame. A value like 1e300 passes
// a finiteness test and then fails both narrowings — it leaves as +Inf and it does not
// fit an int64 — so finiteness alone would let a corrupt file put a non-finite spawn in
// a welcome. The bounds are the ones the simulation already works within:
// [-worldLimit, worldLimit] for a position, which is where float32 stops addressing
// individual blocks and where collide.beyondTheWorld already ends the world, and
// [-Pi, Pi] for a yaw, which is what wrapAngle produces and therefore the only heading
// this server has ever written down.
//
// One rule the wire states is deliberately absent: a count above the registry's stack
// bound is not refused. It is not an invariant a decoder enforces — the wire carries a
// plain uint16 and the client renders what it is told — and pinning a stored life to a
// balance constant would make lowering one destroy every pack that was legal when it
// was written.
func (l Life) Validate() error {
	for axis, value := range l.Pos {
		if math.IsNaN(value) || math.IsInf(value, 0) {
			return fmt.Errorf("game: a stored position axis %d must be finite, got %v", axis, value)
		}
		if math.Abs(value) > worldLimit {
			return fmt.Errorf("game: a stored position axis %d must be within +/-%d blocks, got %v",
				axis, worldLimit, value)
		}
	}
	if math.IsNaN(l.Yaw) || math.IsInf(l.Yaw, 0) {
		return fmt.Errorf("game: a stored yaw must be finite, got %v", l.Yaw)
	}
	if math.Abs(l.Yaw) > math.Pi {
		return fmt.Errorf("game: a stored yaw must be a wrapped angle within +/-Pi, got %v", l.Yaw)
	}

	// Zero is refused, not clamped: a record always describes a *living* player — see
	// Player.Record — so a health of zero is a record that was never written by this
	// server rather than a corpse to be restored.
	if l.Health == 0 || l.Health > PlayerMaxHealth {
		return fmt.Errorf("game: a stored health must be in 1..%d, got %d", PlayerMaxHealth, l.Health)
	}
	if l.Hunger > PlayerMaxHunger {
		return fmt.Errorf("game: a stored hunger must be in 0..%d, got %d", PlayerMaxHunger, l.Hunger)
	}

	for slot, stack := range l.Slots {
		if err := validateStoredSlot(stack); err != nil {
			return fmt.Errorf("game: stored inventory slot %d: %w", slot, err)
		}
	}
	return nil
}

// validateStoredSlot is [Life.Validate]'s per-slot half.
func validateStoredSlot(stack protocol.InventoryStack) error {
	item := ItemID(stack.ItemID)
	if item == ItemNone {
		// The empty slot is the zero of all four numbers. A count or a durability under
		// no item is not an empty slot with debris in it — it is a slot whose four
		// numbers came from somewhere other than this server.
		if stack.Count != 0 || stack.Durability != 0 || stack.MaxDurability != 0 {
			return fmt.Errorf("an empty slot must be all zeroes, got count %d, durability %d/%d",
				stack.Count, stack.Durability, stack.MaxDurability)
		}
		return nil
	}

	if _, known := itemByID(item); !known {
		return fmt.Errorf("item %d is not in the registry", stack.ItemID)
	}
	if stack.Count == 0 {
		// The other half of the pairing above: an item with no count is neither an
		// occupied slot nor an empty one, and every path in this package writes both
		// numbers together.
		return fmt.Errorf("item %d is held with a count of 0", stack.ItemID)
	}
	if stack.Durability > stack.MaxDurability {
		return fmt.Errorf("item %d has %d durability of a maximum of %d",
			stack.ItemID, stack.Durability, stack.MaxDurability)
	}
	if stack.MaxDurability != 0 && stack.Count != 1 {
		// Two blades are two objects with two amounts of wear left and one slot to
		// record it in, so a durable stack of more than one could only ever have thrown
		// one of those numbers away. The client's decoder refuses this shape too.
		return fmt.Errorf("item %d wears out and is stacked %d deep", stack.ItemID, stack.Count)
	}
	return nil
}

// Record captures this player's life, as the record that will be written for them.
//
// **It always describes a living player, and that is the whole of what makes quitting
// neither a way out of a death nor a way to pay for one twice.** A player who is dead
// when this runs is captured as their respawn would have left them: alive, at
// [PlayerMaxHealth], at [Player.respawnPositionLocked] — their tent if one stands, the
// join spawn otherwise — with the death penalty charged if the tick had not managed it
// yet. Charged through the same one-shot the tick uses, so a death already paid for is
// not paid for again.
//
// # Locks, and the order they are taken in
//
// Both, together, in the order the rest of this package takes them: the simulation's
// first, the inventory's second. Held together because a record is one instant — a
// position from before a craft and the slots from after it is a record of a moment that
// never happened — and in that order because the tick holds the simulation's lock and
// then reaches for the inventory, so the other nesting is the deadlock (see the guard
// in Edit).
//
// It therefore makes the tick wait for whichever session goroutine holds the inventory
// lock. That is bounded and known: nothing holds that lock across I/O — the one path
// that keeps it across a world write takes it *after* the chunk has been generated, over
// an in-memory write — and this runs once per autosave interval and once at teardown,
// never on the tick goroutine.
//
// **No disk is touched here.** Capturing and writing are separate on purpose, exactly as
// world.Cache.takeDirty and Flush are: the caller writes what this returns, with no lock
// held at all.
func (p *Player) Record() Life {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()

	p.inventory.mu.Lock()
	defer p.inventory.mu.Unlock()

	return p.recordLocked()
}

// recordLocked is [Player.Record] with both locks already held.
func (p *Player) recordLocked() Life {
	pos, health, hunger := p.pos, p.health, p.hunger
	if !p.alive() {
		// The transition a respawn would have performed, minus the parts that only mean
		// something to a live session: no protection window, no cleared ordering guards,
		// no chunk publish. What survives is where they come back and what it cost them.
		p.chargeDeathPenaltyLocked()
		pos = p.respawnPositionLocked()
		health = PlayerMaxHealth
		hunger = max(hunger, RespawnHungerFloor)
	}

	// Through the same builder the wire uses, so a stored pack and a sent one cannot
	// describe the same slots differently.
	state := p.inventory.stateLocked()

	life := Life{Pos: pos, Yaw: p.yaw, Health: health, Hunger: hunger}
	copy(life.Slots[:], state.Stacks)
	return life
}

// Records is a record for every player in the simulation, keyed by the identity it
// will be stored under.
//
// The autosave's half of the capture/write split. The players are gathered under the
// simulation's lock and then recorded one at a time with it released, so the tick is
// never held across more than a single player's capture — and the caller does every
// write with no lock held at all.
//
// A player who leaves between the two steps is still recorded, and the record is still
// the right one: [Player.Record] reads the player's own fields, which a departed player
// keeps. Their session's teardown writes the same life a moment later.
func (s *Sim) Records() map[identity.PlayerID]Life {
	s.mu.Lock()
	players := make([]*Player, 0, len(s.players))
	for _, p := range s.players {
		players = append(players, p)
	}
	s.mu.Unlock()

	records := make(map[identity.PlayerID]Life, len(players))
	for _, p := range players {
		records[p.playerID] = p.Record()
	}
	return records
}
