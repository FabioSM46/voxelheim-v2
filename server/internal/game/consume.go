package game

import (
	"errors"
	"fmt"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// ConsumeResult is every complete authoritative state one successful item use changes.
// Inventory is always present. LearnedMounts is present only when the item taught a
// mount; food changes hunger, which already travels in the next ordinary snapshot.
type ConsumeResult struct {
	Inventory     protocol.InventoryState
	LearnedMounts *protocol.LearnedMounts
}

// consumableStackLocked resolves one untrusted slot to one registered consumable.
// The caller holds the inventory lock. Slot stays uint16 until both the uint8
// representation and pack bounds are checked, so narrowing can never wrap it onto a
// real slot even if the pack grows beyond 256 entries.
func (i *inventory) consumableStackLocked(slot uint16) (uint8, inventoryStack, itemDefinition, bool) {
	if slot > uint16(^uint8(0)) || int(slot) >= len(i.slots) {
		return 0, inventoryStack{}, itemDefinition{}, false
	}

	index := uint8(slot)
	stack, held := i.stackAtLocked(index)
	if !held {
		return 0, inventoryStack{}, itemDefinition{}, false
	}
	definition, registered := itemByID(stack.item)
	if !registered || definition.restoresHunger == 0 && definition.learnsMount == vnet.MountKindUnknown {
		return 0, inventoryStack{}, itemDefinition{}, false
	}
	return index, stack, definition, true
}

// Consume resolves one ConsumeRequest against the authoritative life and pack. Success
// spends exactly one item and applies the one effect its registry row names. A duplicate
// mount is decided before the spend, so the token survives the refusal intact.
func (p *Player) Consume(req protocol.ConsumeRequest) (ConsumeResult, vnet.RefusalReason, error) {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()

	if err := p.cannotActLocked(); err != nil {
		return ConsumeResult{}, vnet.RefusalReasonPlayerIsDead, err
	}
	if !p.inventory.mu.TryLock() {
		return ConsumeResult{}, vnet.RefusalReasonInventoryBusy, errors.New("the inventory is busy")
	}
	defer p.inventory.mu.Unlock()

	index, stack, definition, usable := p.inventory.consumableStackLocked(req.Slot)
	if !usable {
		return ConsumeResult{}, vnet.RefusalReasonSlotUnusable, fmt.Errorf("slot %d holds no consumable item", req.Slot)
	}

	result := ConsumeResult{}
	switch {
	case definition.restoresHunger != 0:
		if p.hunger >= PlayerMaxHunger {
			return ConsumeResult{}, vnet.RefusalReasonUnknown, errors.New("the hunger reserve is already full")
		}
		if !p.inventory.consumeOneLocked(index, stack.item) {
			return ConsumeResult{}, vnet.RefusalReasonSlotChanged, fmt.Errorf("slot %d changed before its item could be consumed", req.Slot)
		}
		p.hunger = uint16(min(uint32(PlayerMaxHunger), uint32(p.hunger)+uint32(definition.restoresHunger)))

	case definition.learnsMount != vnet.MountKindUnknown:
		next, learned := p.learnedMounts.Learn(definition.learnsMount)
		if !learned {
			return ConsumeResult{}, vnet.RefusalReasonMountAlreadyLearned,
				fmt.Errorf("%s is already learned", definition.learnsMount)
		}
		if !p.inventory.consumeOneLocked(index, stack.item) {
			return ConsumeResult{}, vnet.RefusalReasonSlotChanged, fmt.Errorf("slot %d changed before its item could be consumed", req.Slot)
		}
		p.learnedMounts = next
		learnedState := next.State()
		result.LearnedMounts = &learnedState
	}

	p.refreshWornLocked()
	result.Inventory = p.inventory.stateLocked()
	p.sim.log.Debug("item consumed", "entity_id", p.entityID, "slot", req.Slot,
		"item", stack.item, "client_tick", req.ClientTick, "hunger", p.hunger,
		"learned_mounts", uint8(p.learnedMounts))
	return result, vnet.RefusalReasonUnknown, nil
}
