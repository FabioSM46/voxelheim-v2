package game

import (
	"errors"
	"fmt"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// Keeping a blade alive, and where the rule for it lives.
//
// **There is no repair station and no recipe here.** GDD §4 makes mending a field action:
// a stone comes out of the pack wherever the player is standing, which is what turns a
// death or a long fight into a supply cost rather than an expiry date on the weapon. The
// forge is only where the stones are *made*, and that half is `recipeTable`'s.
//
// The wire carries two slot indexes and nothing else. How much wear a kit gives back, what
// counts as a kit at all and whether the target can be worn are read from the registry and
// the authoritative slots, so there is no claim in a `RepairRequest` for the server to
// disbelieve — a message that could state a durability would be a repair granted by asking.
//
// Every refusal is silence. There is no rejection payload in V4, and a repair that did not
// happen is a pack whose durability vectors did not move.

// restoredBy is what one kit gives back: the wear a slot has now plus the kit's amount,
// capped at the item's own maximum.
//
// The widening to uint32 is not decoration, and it is the one wornByDeath already records
// from the other direction: durability is a uint16, and a slot near its ceiling plus a
// restore is a sum that does not fit in one. Wrapping there would hand a worn blade a
// tiny durability instead of a full one — an overflow that reads as a repair having
// almost worked.
//
// The cap is against the stack's own maximum rather than the registry's, because the
// maximum a slot carries is what the client is shown and what `durable()` judges; a slot
// whose pair disagreed with the registry would still never be raised above what it says.
func restoredBy(stack inventoryStack, restore uint16) uint16 {
	return uint16(min(uint32(stack.durability)+uint32(restore), uint32(stack.maxDurability)))
}

// repairLocked spends one kit from kitSlot on the item in targetSlot, and reports whether
// anything changed.
//
// **No scratch copy, unlike `slotTable.craft`, and the difference is worth naming.** A
// craft has to know whether its product would fit before it spends anything, and the only
// honest answer to that is the insertion actually running — so it runs somewhere it can be
// thrown away. A repair touches exactly two known slots and adds nothing to the pack, so
// every condition is answerable before the first write. The one fallible step is therefore
// ordered first: the kit is consumed, and only then is the target's wear raised.
//
// The caller holds the inventory lock.
func (i *inventory) repairLocked(req protocol.RepairRequest) bool {
	// Nothing mends itself. Checked before the slots are read rather than after, because
	// one slot cannot be both the kit that is spent and the item that is left mended —
	// and with the check further down, a kit that was somehow also durable would consume
	// itself and then have its own wear restored.
	if req.KitSlot == req.TargetSlot {
		return false
	}

	// stackAtLocked is what bounds both indexes. The decoder copies them verbatim,
	// out-of-range values included, exactly as schemas/player.fbs says it should: a slot
	// past the end of the pack is an ordinary refusal here, not a malformed frame.
	kit, held := i.stackAtLocked(req.KitSlot)
	if !held {
		return false
	}
	definition, registered := itemByID(kit.item)
	// A registry question, never a list of item ids: an item that restores nothing is not
	// a repair kit, which is the same refusal a stack of stone gets when it is swung.
	if !registered || definition.repairRestore == 0 {
		return false
	}

	target, occupied := i.stackAtLocked(req.TargetSlot)
	if !occupied {
		return false
	}
	// `durable()` asks the maximum, never the current value — a blade at zero under a
	// non-zero maximum is worn through rather than absent, and mending it is the whole
	// point of the feature. A resource carries `(0, 0)` and is refused by the same test.
	if !target.durable() || target.durability >= target.maxDurability {
		return false
	}

	// Spend first, mend second. consumeOneLocked cannot fail after the checks above, and
	// is written as a refusal anyway for the reason every other TryLock in this package
	// is: "cannot fail" is a property of today's callers rather than of the function. Its
	// last-item path is also what empties the slot to the wire's `(0, 0)` shape.
	if !i.consumeOneLocked(req.KitSlot, kit.item) {
		return false
	}
	i.slots[req.TargetSlot].durability = restoredBy(target, definition.repairRestore)
	return true
}

// Repair resolves one RepairRequest against the authoritative pack and returns the
// inventory a successful mend produced.
//
// Every refusal is an ordinary error the session logs at debug and answers with silence:
// there is no rejection message in the contract, and a client learns its repair did not
// happen by the durability in its pack not moving.
//
// **One critical section, for the reason Craft has one.** Liveness and the slot arithmetic
// are one decision, and splitting them would leave a window in which a player killed
// between the two still spends a stone. Nothing in it blocks: the arithmetic is two slots,
// and the inventory is taken with TryLock — the same discipline the tick uses, which keeps
// the pair deadlock-free by construction rather than by lock ordering.
//
// **No station scan, deliberately.** A repair is a field action per GDD §4; adding a
// proximity test here would be a second answer to "where can this be done" and would make
// the blade a thing you carry home rather than a thing you keep.
func (p *Player) Repair(req protocol.RepairRequest) (protocol.InventoryState, error) {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()

	if err := p.cannotActLocked(); err != nil {
		// Consistent with mining, editing, placing, attacking and crafting: a corpse
		// does nothing.
		return protocol.InventoryState{}, err
	}

	// TryLock, never Lock, and the same argument Craft records: every other holder of this
	// inventory is either this session's own read goroutine or the tick, and the tick only
	// ever takes it under the lock this function is holding.
	if !p.inventory.mu.TryLock() {
		return protocol.InventoryState{}, errors.New("the inventory is busy")
	}
	defer p.inventory.mu.Unlock()

	if !p.inventory.repairLocked(req) {
		return protocol.InventoryState{}, fmt.Errorf(
			"slot %d holds no usable kit for slot %d", req.KitSlot, req.TargetSlot)
	}
	p.refreshWornLocked()

	p.sim.log.Debug("repair applied",
		"entity_id", p.entityID, "kit_slot", req.KitSlot,
		"target_slot", req.TargetSlot, "client_tick", req.ClientTick)

	return p.inventory.stateLocked(), nil
}
