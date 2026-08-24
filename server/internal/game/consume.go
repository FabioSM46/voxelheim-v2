package game

import (
	"errors"
	"fmt"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// consumeFoodLocked spends exactly one edible item from slot and returns what it
// restores. The caller holds the inventory lock. Slot stays uint16 until after the
// bound check because narrowing an untrusted value would wrap it onto a real slot.
func (i *inventory) consumeFoodLocked(slot uint16) (uint16, bool) {
	if slot >= uint16(len(i.slots)) {
		return 0, false
	}

	index := uint8(slot)
	stack, held := i.stackAtLocked(index)
	if !held {
		return 0, false
	}
	definition, registered := itemByID(stack.item)
	if !registered || definition.restoresHunger == 0 {
		return 0, false
	}
	if !i.consumeOneLocked(index, stack.item) {
		return 0, false
	}
	return definition.restoresHunger, true
}

// Consume resolves one ConsumeRequest against the authoritative life and pack. A
// refusal is silence at the session boundary; success consumes exactly one item,
// raises hunger without overflowing, and returns the complete inventory state.
func (p *Player) Consume(req protocol.ConsumeRequest) (protocol.InventoryState, error) {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()

	if err := p.cannotActLocked(); err != nil {
		return protocol.InventoryState{}, err
	}
	if p.hunger >= PlayerMaxHunger {
		return protocol.InventoryState{}, errors.New("the hunger reserve is already full")
	}

	if !p.inventory.mu.TryLock() {
		return protocol.InventoryState{}, errors.New("the inventory is busy")
	}
	defer p.inventory.mu.Unlock()

	restore, consumed := p.inventory.consumeFoodLocked(req.Slot)
	if !consumed {
		return protocol.InventoryState{}, fmt.Errorf("slot %d holds no edible item", req.Slot)
	}

	p.hunger = uint16(min(uint32(PlayerMaxHunger), uint32(p.hunger)+uint32(restore)))
	p.sim.log.Debug("item consumed", "entity_id", p.entityID, "slot", req.Slot,
		"client_tick", req.ClientTick, "hunger", p.hunger)

	return p.inventory.stateLocked(), nil
}
