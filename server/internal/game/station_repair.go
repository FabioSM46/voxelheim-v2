package game

import (
	"errors"
	"fmt"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// RepairSilverPerPoint prices one missing durability point at one silver. Linear
// pricing makes a partial repair cost proportional to wear and keeps the forge a
// predictable fallback when field kits run out; item rarity adds no hidden multiplier.
const RepairSilverPerPoint uint32 = 1

// StationRepair restores one worn item fully at a nearby forge. The request names
// only a slot; reach, wear and price all come from authoritative state. Equipment
// and backpack slots are equally eligible, without the carriedOnPerson restriction.
func (p *Player) StationRepair(req protocol.StationRepairRequest) (protocol.InventoryState, vnet.RefusalReason, error) {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()

	if err := p.cannotActLocked(); err != nil {
		return protocol.InventoryState{}, vnet.RefusalReasonPlayerIsDead, err
	}
	radius, configured := craftRadius(vnet.StructureKindForge)
	if !configured || !p.sim.stationWithinLocked(vnet.StructureKindForge, p.box(), radius) {
		return protocol.InventoryState{}, vnet.RefusalReasonNotAtStation, errors.New("no forge is within crafting reach")
	}
	if p.rewardInventoryBusyLocked() || !p.inventory.mu.TryLock() {
		return protocol.InventoryState{}, vnet.RefusalReasonInventoryBusy, errors.New("the inventory is busy")
	}
	defer p.inventory.mu.Unlock()

	// The wire index is wider than stackAtLocked's index: bound it before narrowing
	// so a request for slot 256 cannot wrap around and spend silver on slot zero.
	// InventorySlots is itself uint8; the admitted index is representable here.
	if req.TargetSlot >= uint16(protocol.InventorySlots) {
		return protocol.InventoryState{}, vnet.RefusalReasonNothingToRepair, errors.New("the target slot is outside the inventory")
	}
	slot := uint8(req.TargetSlot)
	target, occupied := p.inventory.stackAtLocked(slot)
	if !occupied || !target.durable() || target.durability >= target.maxDurability {
		return protocol.InventoryState{}, vnet.RefusalReasonNothingToRepair, errors.New("the target has no missing durability")
	}
	missing := target.maxDurability - target.durability
	// With the fixed rate of one, the largest price is 65535 silver. Compute in
	// the purse's uint32 width; a future rate change must preserve that bound.
	price := max(uint32(1), uint32(missing)*RepairSilverPerPoint)
	if p.inventory.silver < price {
		return protocol.InventoryState{}, vnet.RefusalReasonNotEnoughSilver, fmt.Errorf("repair costs %d silver, more than the purse holds", price)
	}

	// All fallible checks precede both writes, under the same simulation/inventory
	// locks. A refusal cannot spend silver or partly repair the item.
	p.inventory.silver -= price
	p.inventory.slots[slot].durability = restoredBy(target, missing)
	p.refreshWornLocked()
	return p.inventory.stateLocked(), vnet.RefusalReasonUnknown, nil
}
