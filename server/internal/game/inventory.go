package game

import (
	"errors"
	"sync"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// inventory is one player's authoritative, ordered set of slots. The first
// protocol.HotbarSlots entries are the hotbar; they are not a projection or a
// separate collection, so moving a stack never changes any other slot's index.
//
// Its lock is separate from Sim.mu. Edits acquire it only after the target chunk
// has been generated, then keep it across the authoritative voxel write and the
// slot change. An inventory move therefore waits for another operation on this
// player, never for chunk generation and never for the simulation tick.
type inventory struct {
	mu    sync.Mutex
	slots slotTable
}

// slotTable is one player's authoritative slots as a plain value.
//
// A named array rather than more methods on *inventory, and the reason is crafting: a
// craft has to be all-or-nothing across several ingredients and one output, and the way
// it gets that is by doing the whole thing to a **copy** and keeping it only if every
// step succeeded. `inventory` carries a mutex, so `go vet`'s copylocks check refuses to
// see one copied; an array of slots copies for free. The rules about how items enter and
// leave a pack therefore live on this type, and `inventory` is the lock around it.
type slotTable [protocol.InventorySlots]inventoryStack

// inventoryStack is one slot's authoritative contents. The zero value is an empty slot.
//
// durability and maxDurability are both zero for an empty slot and for every resource:
// schemas/player.fbs reads that pair as "nothing here wears out". A non-zero maximum
// makes the slot equipment — exactly one whole item, with the wear it has left. A
// durability of zero *under* a non-zero maximum is a worn-out item: unusable, still
// carried, still in its slot, and never confused with an empty one.
type inventoryStack struct {
	item          ItemID
	count         uint16
	durability    uint16
	maxDurability uint16
}

// durable reports whether this slot holds something that wears out. The maximum is what
// answers it, never the current value — a blade at zero is still a blade.
func (s inventoryStack) durable() bool { return s.maxDurability != 0 }

// slotCountFor is how many of an item may share one slot.
//
// One, for anything that wears out, whatever stack bound its registry entry carries.
// That is not a policy choice: schemas/player.fbs gives a slot exactly one durability
// pair, so two blades in one slot could only record one of their two remaining
// conditions, and the client's decoder refuses a durable slot whose count is not 1.
//
// A separate function because it is the one place that rule is written, and because a
// registry entry pairing a stack bound with a durability is exactly what nobody would
// notice writing — see the test that drives this with a definition no registry holds.
func slotCountFor(definition itemDefinition, want uint16) uint16 {
	if definition.maxDurability != 0 {
		return 1
	}
	return min(want, definition.maxStack)
}

// stackOf builds one slot's contents from the registry.
//
// The single constructor, and therefore the single place both slot invariants hold: a
// durable item can never come into existence without the maximum it is measured
// against, nor as more than one of itself. Both are shapes the contract forbids and the
// client refuses to decode.
//
// It returns what it actually made, which may be fewer than asked for — callers spread
// the remainder across further slots rather than assuming they got what they requested.
func stackOf(itemID ItemID, count uint16) inventoryStack {
	definition, ok := itemByID(itemID)
	if !ok || itemID == ItemNone || count == 0 {
		return inventoryStack{}
	}
	return inventoryStack{
		item:          itemID,
		count:         slotCountFor(definition, count),
		durability:    definition.maxDurability,
		maxDurability: definition.maxDurability,
	}
}

func newInventory() inventory { return inventory{} }

// newStarterInventory is what a player joins holding: one rusty sword at full
// durability in the first hotbar slot, and nothing else.
//
// Granted on join deliberately, and **crafting arriving did not change that**. The blade a
// player can now make costs three raw iron, two coal, a log and a forge to make them at,
// and every item in that list is mined with the blade they do not have yet. The starter
// sword is what gets a new player to the first one, so it is a bootstrap rather than a
// placeholder — what replaces it is loot or equipment persistence, not this.
func newStarterInventory() inventory {
	// A composite literal rather than a local that gets filled in and returned: the
	// struct carries a sync.Mutex, and `go vet`'s copylocks check refuses to see one
	// returned by value from a variable. Returning the literal is the form it accepts,
	// and the slots come from the table below so that "what a new player holds" is one
	// value rather than one value and one constructor.
	return inventory{slots: starterSlots()}
}

// starterSlots is the pack itself, without the lock around it.
//
// Split out because Join now chooses between this and a restored pack before it builds
// the player, and slotTable is the type that can be chosen: `inventory` carries a mutex
// and `go vet`'s copylocks check refuses to see one assigned from a variable.
func starterSlots() slotTable {
	return slotTable{
		0: stackOf(ItemRustySword, 1),
	}
}

// restoredSlots is a stored pack as the authoritative slots.
//
// A straight copy, four numbers at a time, and deliberately not a second place where a
// slot is decided: every value here has already been through [Life.Validate], which is
// the one thing standing between a file on disk and the pack a player is handed. Going
// through stackOf instead would look safer and would silently rewrite what it was given
// — a worn blade would come back at full durability, because that constructor reads the
// registry rather than the record.
func restoredSlots(stored [protocol.InventorySlots]protocol.InventoryStack) slotTable {
	var slots slotTable
	for slot, stack := range stored {
		slots[slot] = inventoryStack{
			item:          ItemID(stack.ItemID),
			count:         stack.Count,
			durability:    stack.Durability,
			maxDurability: stack.MaxDurability,
		}
	}
	return slots
}

// insertLocked inserts as much of one stack as fits and returns the remainder.
//
// One call is one operation: every partial stack of this item is filled in slot
// order before the lowest empty slot is used. Nothing here knows whether the
// caller broke a block, picked up an entity, or received loot.
func (i *inventory) insertLocked(itemID ItemID, count uint16) uint16 {
	return i.slots.insert(itemID, count)
}

// insert is the insertion rule itself, on the slots rather than on the lock around them.
//
// One implementation, deliberately. A craft has to know whether its output would fit
// *before* it spends anything, and the only honest answer to that question is the
// insertion actually running — a second "would this fit" predicate is a copy of this rule
// that can disagree with it, and the disagreement is an item that stops existing.
func (t *slotTable) insert(itemID ItemID, count uint16) uint16 {
	definition, ok := itemByID(itemID)
	if !ok || itemID == ItemNone || count == 0 {
		return count
	}

	// Filling partial stacks is for resources. Two durable items are two objects with
	// two different amounts of wear, so a merge would have to throw one of those
	// numbers away — there is no slot to keep it in. Skipped explicitly rather than
	// left to maxStack: equipment is one to a slot today, and the loop below happens
	// to refuse every occupied slot because of it, but a later durable item that
	// stacked two deep would start silently merging durabilities.
	if definition.maxDurability == 0 {
		for slot := range t {
			stack := &t[slot]
			if stack.item != itemID || stack.count >= definition.maxStack {
				continue
			}
			moved := min(count, definition.maxStack-stack.count)
			stack.count += moved
			count -= moved
			if count == 0 {
				return 0
			}
		}
	}

	for slot := range t {
		stack := &t[slot]
		if stack.count != 0 {
			continue
		}
		// Through the constructor, so a durable item arriving by this path is created
		// with its maximum rather than as a wearless copy of itself, and never as a
		// stack of two. **Crafting is what reaches here with equipment**, and it is why
		// the constructor was already the right answer before anything did: a forged
		// blade is inserted exactly like a picked-up stone, and it arrives at full
		// durability because `stackOf` reads the registry rather than because the craft
		// remembered to say so.
		//
		// The accounting follows what the constructor *made* rather than what this loop
		// asked for. Subtracting the request instead is how three swords would become
		// one sword and two items that stopped existing.
		*stack = stackOf(itemID, count)
		count -= stack.count
		if count == 0 {
			return 0
		}
	}

	return count
}

// consume spends `count` of one item from wherever it is, in slot order, and reports
// whether the pack held enough.
//
// **It does not unwind a partial spend, and that is safe only because of who calls it.**
// The one caller is `craft`, which works on a copy and throws the whole copy away the
// moment any step returns false — so a half-consumed table is never a table anybody sees.
// A second caller would need either that discipline or an unwind, and this comment is the
// place to notice which.
func (t *slotTable) consume(itemID ItemID, count uint16) bool {
	if itemID == ItemNone || count == 0 {
		return false
	}

	for slot := range t {
		if count == 0 {
			break
		}
		stack := &t[slot]
		if stack.item != itemID || stack.count == 0 {
			continue
		}
		spent := min(count, stack.count)
		stack.count -= spent
		count -= spent
		if stack.count == 0 {
			*stack = inventoryStack{}
		}
	}
	return count == 0
}

// consumeOneLocked spends exactly one item from the named slot. expected makes
// the revalidated item part of the operation, so a caller can never consume a
// different stack after waiting for the world write.
func (i *inventory) consumeOneLocked(slot uint8, expected ItemID) bool {
	if slot >= protocol.InventorySlots {
		return false
	}
	stack := &i.slots[slot]
	if stack.item != expected || stack.count == 0 {
		return false
	}
	if stack.count == 1 {
		*stack = inventoryStack{}
	} else {
		stack.count--
	}
	return true
}

func (i *inventory) stackAtLocked(slot uint8) (inventoryStack, bool) {
	if slot >= protocol.InventorySlots {
		return inventoryStack{}, false
	}
	stack := i.slots[slot]
	if stack.item == ItemNone || stack.count == 0 {
		return inventoryStack{}, false
	}
	return stack, true
}

// stateLocked returns all authoritative slots. Empty slots stay as the zero pair
// (0, 0), and the slice length always matches what ServerWelcome announced.
//
// All four numbers per slot come from the same locked copy, which is the whole point of
// reading them here rather than in three passes: schemas/player.fbs puts durability on
// the wire as two vectors parallel to the stack pairs, and a frame that paired one
// slot's count with another slot's wear would decode perfectly and be wrong.
func (i *inventory) stateLocked() protocol.InventoryState {
	stacks := make([]protocol.InventoryStack, int(protocol.InventorySlots))
	for slot, stack := range i.slots {
		stacks[slot] = protocol.InventoryStack{
			ItemID:        uint16(stack.item),
			Count:         stack.count,
			Durability:    stack.durability,
			MaxDurability: stack.maxDurability,
		}
	}
	return protocol.InventoryState{Stacks: stacks}
}

// carriedOnPerson reports whether a slot is one the player has *on them*, as opposed to
// stowed in the pack behind them.
//
// It is the one answer to that question, and it is a function rather than a comparison
// for the reason meleeDamage is a registry field rather than a list of item ids in the
// combat path: worn armour is the next thing that will be on a player, and when it
// arrives it joins every rule that asks this by widening this answer — not by a second
// `slot < protocol.HotbarSlots` appearing somewhere that can disagree with this one.
//
// Today the answer is the hotbar, and the server needs nothing from the client to give
// it: protocol.HotbarSlots is the *leading* subset of the inventory, so a slot's own
// index is the whole of it. There is no selection in this package and none on the wire,
// deliberately — a slot reaches this server only inside a request that names one — so
// "what is on the player" could never have meant the one slot a client had highlighted.
func carriedOnPerson(slot int) bool {
	return slot < int(protocol.HotbarSlots)
}

// applyDeathPenaltyLocked wears by the approved death penalty every durable slot the
// player has on them, and reports whether any of them changed.
//
// **What it reaches is carriedOnPerson's answer and nothing else's.** Dying costs the
// condition of what was being carried; the pack behind the player is untouched, so a
// spare blade stowed away outlives the death that spent the one in hand. That is the
// whole of the narrowing — the arithmetic below it did not move.
//
// Every such slot in one pass under one lock, so there is no moment at which a snapshot
// could show half a player's equipment penalised. It touches no item id, no count and no
// slot index: death costs condition, never possessions.
func (i *inventory) applyDeathPenaltyLocked() bool {
	changed := false
	for slot := range i.slots {
		// Stowed rather than carried: the penalty never reaches it. An empty hotbar
		// needs no special case for the same reason a pack of resources does not — it
		// simply has nothing this loop can spend.
		if !carriedOnPerson(slot) {
			continue
		}
		stack := &i.slots[slot]
		// Resources are skipped by the maximum, not by the item id: what a slot holds
		// is the registry's business, and "does it wear out" is already recorded here.
		if !stack.durable() {
			continue
		}
		worn := wornByDeath(stack.durability)
		if worn != stack.durability {
			stack.durability = worn
			changed = true
		}
	}
	return changed
}

// wornByDeath is the approved death penalty: floor(current * 4/5).
//
// Integer arithmetic rather than a float multiply, for two reasons that are not style.
// 0.8 has no exact binary representation, so `uint16(float64(d) * 0.8)` makes every
// boundary a rounding question — and these are authoritative numbers a test pins at
// 100 -> 80, 1 -> 0 and 0 -> 0. The widening to uint32 is not decoration either:
// durability is a uint16 and 65535 * 4 does not fit in one.
func wornByDeath(current uint16) uint16 {
	return uint16(uint32(current) * deathDurabilityKept / deathDurabilityScale)
}

// moveLocked applies one authoritative slot move and reports whether the state
// changed. A partial move into an occupied different-item slot is refused: there
// is nowhere to keep both that slot's old stack and the source remainder. A whole
// source stack swaps with a different item instead.
func (i *inventory) moveLocked(req protocol.InventoryMoveRequest) bool {
	if req.From >= protocol.InventorySlots || req.To >= protocol.InventorySlots || req.Count == 0 || req.From == req.To {
		return false
	}

	source := &i.slots[req.From]
	target := &i.slots[req.To]
	definition, registered := itemByID(source.item)
	if source.count == 0 || source.item == ItemNone || !registered {
		return false
	}

	moveCount := min(req.Count, source.count)
	switch {
	case target.count == 0:
		// A durable item moves whole or not at all. Its wear belongs to the one object,
		// so there is no answer to what half of it would carry. Unreachable while every
		// durable item is one to a slot — min(req.Count, 1) is always 1 — and written
		// anyway, because "one to a slot" is a registry entry somebody can change.
		if source.durable() && moveCount != source.count {
			return false
		}
		// The whole struct, then the count: this is what carries durability across with
		// the item instead of leaving the new slot holding a wearless copy of it.
		*target = *source
		target.count = moveCount
		source.count -= moveCount
		if source.count == 0 {
			*source = inventoryStack{}
		}
		return true

	case target.item == source.item:
		// Never for equipment, for the reason insertLocked does not merge it: two
		// blades have two different amounts of wear left and one slot to record it in.
		if source.durable() || target.durable() {
			return false
		}
		if target.count >= definition.maxStack {
			return false
		}
		moveCount = min(moveCount, definition.maxStack-target.count)
		if moveCount == 0 {
			return false
		}
		target.count += moveCount
		source.count -= moveCount
		if source.count == 0 {
			*source = inventoryStack{}
		}
		return true

	case moveCount == source.count:
		if _, ok := itemByID(target.item); !ok {
			return false
		}
		*source, *target = *target, *source
		return true

	default:
		return false
	}
}

func (i *inventory) state() protocol.InventoryState {
	i.mu.Lock()
	defer i.mu.Unlock()
	return i.stateLocked()
}

// chargeDeathPenaltyLocked wears what the player has on them by the approved death
// penalty, at most once per death. applyDeathPenaltyLocked owns which slots those are.
//
// The caller holds sim.mu **and the inventory lock**. penaltyApplied is what makes it a
// one-shot, and it is set here rather than by each caller so that "charged exactly once"
// is a property of this function instead of a rule two callers have to remember: the
// tick charges it on the way to a respawn, and Player.Record charges it on the way to a
// file for a player who quit before that respawn arrived.
//
// "It ran" rather than "something changed" is the deliberate reading: a pack of nothing
// but worn-out blades changes nothing and has still been penalised, and treating that as
// failure would penalise it again on every tick that followed.
func (p *Player) chargeDeathPenaltyLocked() {
	if p.penaltyApplied {
		return
	}
	if p.inventory.applyDeathPenaltyLocked() {
		// The durable path, not a snapshot: an inventory state is not superseded by the
		// next tick's, so a full outbound queue must not be able to leave the client
		// showing durability the server has already spent. See offerInventoryLocked.
		p.inventoryDirty = true
	}
	p.penaltyApplied = true
}

// tryApplyDeathPenaltyLocked charges the death penalty if the inventory lock is free,
// and reports whether it ran.
//
// The caller holds sim.mu. It returns false when a session goroutine is holding the
// inventory lock, because the tick may not wait for it — no tick blocks on another
// player's contention. A caller that gets false keeps whatever pending transition it
// has and asks again next tick.
func (p *Player) tryApplyDeathPenaltyLocked() bool {
	if !p.inventory.mu.TryLock() {
		return false
	}
	defer p.inventory.mu.Unlock()

	p.chargeDeathPenaltyLocked()
	return true
}

// MoveInventory resolves one client intent against the live authoritative slots.
// A changed state is returned whole; every refusal returns an error so the session
// can log it and send nothing.
func (p *Player) MoveInventory(req protocol.InventoryMoveRequest) (protocol.InventoryState, error) {
	p.inventory.mu.Lock()
	defer p.inventory.mu.Unlock()

	if !p.inventory.moveLocked(req) {
		return protocol.InventoryState{}, errors.New("the inventory move changes no authoritative slot")
	}
	return p.inventory.stateLocked(), nil
}
