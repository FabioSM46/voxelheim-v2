package game

import (
	"math"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// playerTradeOffer is one immutable snapshot of a pack slot. The zero stack means
// this trade position is empty; pack slot zero is therefore available like every
// other pack slot.
type playerTradeOffer struct {
	packSlot uint8
	stack    inventoryStack
}

// playerTradeSide is everything the settlement mechanism needs from one participant.
// The live session owns confirmation, revision and delivery; this value owns only the
// authoritative assets that one atomic exchange reads.
type playerTradeSide struct {
	player *Player
	offers [protocol.PlayerTradeSlots]playerTradeOffer
	silver uint32
}

type playerTradeSettlement uint8

const (
	playerTradeSettlementComplete playerTradeSettlement = iota
	// Busy is distinct because the request may be retried without changing either
	// offer. Nothing was read from a contended inventory.
	playerTradeSettlementBusy
	// Changed means an offered slot or purse no longer matches the snapshot the
	// participants confirmed. The caller withdraws stale offers before asking again.
	playerTradeSettlementChanged
	// Full covers both slot capacity and the purse's uint32 capacity. Either is a
	// receiver that cannot hold the complete incoming side.
	playerTradeSettlementFull
)

// settlePlayerTradeLocked swaps two offers, or changes neither inventory.
//
// The caller holds Sim.mu. Both inventories are acquired with TryLock in ascending
// entity-id order, so another path can never wait under Sim.mu and two settlements can
// never choose opposite lock orders. The live tables are copied only after both locks
// are held. Every outgoing stack is then removed from the copies before any incoming
// stack is inserted, which lets a full player receive into the slot they are giving
// away. The authoritative tables and purses are replaced only after every verification,
// removal, insertion and silver calculation succeeds.
func settlePlayerTradeLocked(left, right playerTradeSide) playerTradeSettlement {
	if left.player == nil || right.player == nil || left.player == right.player ||
		left.player.entityID == right.player.entityID {
		return playerTradeSettlementChanged
	}

	first, second := left.player, right.player
	if first.entityID > second.entityID {
		first, second = second, first
	}
	if !first.inventory.mu.TryLock() {
		return playerTradeSettlementBusy
	}
	defer first.inventory.mu.Unlock()
	if !second.inventory.mu.TryLock() {
		return playerTradeSettlementBusy
	}
	defer second.inventory.mu.Unlock()

	if !tradeOfferMatches(left.player.inventory.slots, left.offers) ||
		!tradeOfferMatches(right.player.inventory.slots, right.offers) ||
		left.player.inventory.silver < left.silver ||
		right.player.inventory.silver < right.silver {
		return playerTradeSettlementChanged
	}

	leftSlots, rightSlots := left.player.inventory.slots, right.player.inventory.slots
	removeTradeOffer(&leftSlots, left.offers)
	removeTradeOffer(&rightSlots, right.offers)

	// Removal on both sides precedes insertion on either side. Besides the full-pack
	// case, keeping this phase boundary explicit makes it impossible to commit one
	// direction while still discovering that the other does not fit.
	if !insertTradeOffer(&leftSlots, right.offers) ||
		!insertTradeOffer(&rightSlots, left.offers) {
		return playerTradeSettlementFull
	}

	leftSilver := uint64(left.player.inventory.silver-left.silver) + uint64(right.silver)
	rightSilver := uint64(right.player.inventory.silver-right.silver) + uint64(left.silver)
	if leftSilver > math.MaxUint32 || rightSilver > math.MaxUint32 {
		return playerTradeSettlementFull
	}

	left.player.inventory.slots = leftSlots
	left.player.inventory.silver = uint32(leftSilver)
	right.player.inventory.slots = rightSlots
	right.player.inventory.silver = uint32(rightSilver)
	left.player.inventoryDirty = true
	right.player.inventoryDirty = true
	return playerTradeSettlementComplete
}

// tradeOfferMatches re-verifies both the contents and the uniqueness of every offered
// slot. Uniqueness is normally enforced when the offer is built; checking it again here
// makes the transaction safe even if a future caller constructs one incorrectly.
func tradeOfferMatches(slots slotTable, offers [protocol.PlayerTradeSlots]playerTradeOffer) bool {
	var seen [protocol.InventorySlots]bool
	for _, offer := range offers {
		if offer.stack.count == 0 {
			continue
		}
		if int(offer.packSlot) >= equipmentFirst || seen[offer.packSlot] ||
			offer.stack.item == ItemNone || slots[offer.packSlot] != offer.stack {
			return false
		}
		seen[offer.packSlot] = true
	}
	return true
}

func removeTradeOffer(slots *slotTable, offers [protocol.PlayerTradeSlots]playerTradeOffer) {
	for _, offer := range offers {
		if offer.stack.count != 0 {
			slots[offer.packSlot] = inventoryStack{}
		}
	}
}

func insertTradeOffer(slots *slotTable, offers [protocol.PlayerTradeSlots]playerTradeOffer) bool {
	for _, offer := range offers {
		if offer.stack.count == 0 {
			continue
		}
		inserted := false
		for slot := range slots[:equipmentFirst] {
			if slots[slot].count != 0 {
				continue
			}
			// A trade moves the offered stack as one value. In particular, it never
			// merges a wearless stack into a partial one: count is uint16 and no
			// arithmetic is needed when the exact source value is copied whole.
			slots[slot] = offer.stack
			inserted = true
			break
		}
		if !inserted {
			return false
		}
	}
	return true
}
