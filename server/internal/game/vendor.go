package game

import (
	"errors"
	"fmt"
	"math"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// What the people the seed put there will trade, and the one place a price is written
// down.
//
// **A stall is the loot container with prices and a second direction**, and loot.go is
// the shape this file follows deliberately rather than incidentally: a server-owned,
// revisioned, per-player session that opens on a request, refuses one written against a
// revision it has replaced, and ends with an explicit frame. What a corpse does not have
// is a direction of travel — everything comes out of it — so the two halves of a trade
// are the one thing here with no precedent above.
//
// **Nothing about a vendor is per-entity state.** The table is keyed by role, stock is
// unlimited by contract, and there is no restock timer, no drift and no reputation, so
// two players at one smith see the same list and neither can exhaust it. The only state
// a stall has is the session: which vendor this player has open and which revision they
// are looking at, both of which live on the Player rather than on the resident.

// vendorEntry is one line of a price list: what is traded and the silver per unit.
//
// The direction is the vector it sits in and never a sign, which is the contract's rule
// for `VendorEntry` and the reason there is one type here rather than a buy price and a
// sell price on one row: an item may be in both vectors at different prices, and a single
// row carrying both would have to invent a value for the vendor that only does one.
type vendorEntry struct {
	item  ItemID
	price uint16
}

// vendorStock is one role's whole trade, from the player's side.
//
// `sells` is what the player may buy and what they pay; `buys` is what the vendor accepts
// and what it pays. Both are the player's view rather than the vendor's, exactly as
// `VendorState` puts them on the wire — naming them from the vendor's side is the one
// renaming that would make every comparison in this file read backwards.
type vendorStock struct {
	sells []vendorEntry
	buys  []vendorEntry
}

// vendorTable is what each trade deals in.
//
// **A row edit is the whole of a later economy, and that is the point of the table.** The
// prices are readable as a set rather than defensible one at a time: raw iron is bought at
// 3 and the sword made of it sells at 40, a draugr carries two to six silver, and a
// pickaxe is therefore something like five kills away. Nothing here is balanced against a
// spreadsheet, and nothing needs to be — what matters is that changing one number is
// changing one number.
//
// **Roles that keep no stall are absent rather than empty**, so the map's own membership
// is the answer to "does this person trade" and [vendorRole] is checked against it by the
// tests instead of the two being two lists to keep in step.
//
// Leather patches are in both of the trader's vectors at 4 and 2, which the contract
// explicitly allows: the spread is the vendor's margin and is the server's business.
var vendorTable = map[vnet.ResidentRole]vendorStock{
	vnet.ResidentRoleSmith: {
		sells: []vendorEntry{
			{ItemIronSword, 40},
			{ItemPickaxe, 25},
			{ItemShovel, 12},
			{ItemSharpeningStone, 6},
			{ItemIronHelm, 30},
			{ItemIronCuirass, 45},
			{ItemIronGreaves, 35},
		},
		buys: []vendorEntry{
			{ItemRawIron, 3},
			{ItemRawCoal, 1},
		},
	},
	vnet.ResidentRoleCarpenter: {
		sells: []vendorEntry{
			{ItemAxe, 15},
			{ItemBow, 20},
			{ItemArrow, 1},
			{ItemPlanks, 1},
			{ItemWoodenShield, 18},
		},
		buys: []vendorEntry{
			{ItemLog, 1},
		},
	},
	vnet.ResidentRoleCook: {
		sells: []vendorEntry{
			{ItemCookedMeat, 3},
		},
		buys: []vendorEntry{
			{ItemRawMeat, 1},
		},
	},
	vnet.ResidentRoleTrader: {
		sells: []vendorEntry{
			{ItemTent, 60},
			{ItemCampfire, 10},
			{ItemLeatherPatch, 4},
		},
		buys: []vendorEntry{
			{ItemBone, 1},
			{ItemVargrPelt, 4},
			{ItemLeatherPatch, 2},
		},
	},
}

// priceOf finds one item in one direction of a stall's list.
//
// A linear scan of a list with at most seven rows, which is cheaper than the map it would
// take to avoid it and keeps the table readable as a table. The bool is the whole of
// "does this vendor trade in that", which is the `VendorDoesNotWant` answer in both
// directions.
func (s vendorStock) priceOf(item ItemID, buying bool) (uint16, bool) {
	list := s.buys
	if buying {
		list = s.sells
	}
	for _, entry := range list {
		if entry.item == item {
			return entry.price, true
		}
	}
	return 0, false
}

// vendorState is the complete price list one resident shows one session.
//
// Built fresh on every send rather than cached, for the reason `lootState` is: it is a
// handful of rows produced at most once per trade, and a cached projection is a second
// copy of the table that can be out of step with it.
func vendorState(r *resident, revision uint32) protocol.VendorState {
	stock := vendorTable[r.role]
	state := protocol.VendorState{
		EntityID: r.entityID,
		Revision: revision,
		Sells:    make([]protocol.VendorEntry, len(stock.sells)),
		Buys:     make([]protocol.VendorEntry, len(stock.buys)),
	}
	for index, entry := range stock.sells {
		state.Sells[index] = protocol.VendorEntry{ItemID: uint16(entry.item), Price: entry.price}
	}
	for index, entry := range stock.buys {
		state.Buys[index] = protocol.VendorEntry{ItemID: uint16(entry.item), Price: entry.price}
	}
	return state
}

// queueVendorClosedLocked owes this session one explicit end for one stall.
//
// [Player.queueLootClosedLocked] verbatim, and deduplicated for its reason: a closure is
// not superseded by the next tick, so a session whose queue is full must be told once
// rather than once per tick until it drains.
func (p *Player) queueVendorClosedLocked(id uint64) {
	if id == 0 {
		return
	}
	for _, queued := range p.vendorClosures {
		if queued == id {
			return
		}
	}
	p.vendorClosures = append(p.vendorClosures, id)
}

// closeVendorLocked ends the open stall and owes the session the frame that says so.
func (p *Player) closeVendorLocked() {
	if p.openVendorID == 0 {
		return
	}
	p.queueVendorClosedLocked(p.openVendorID)
	p.openVendorID = 0
	p.vendorRevision = 0
	p.vendorDirty = false
}

// tradeableLocked reports whether this player may be trading with this resident right
// now, and it is the one implementation of that question.
//
// **The same predicate answers the open, the trade and the tick**, which is what makes
// "walking away closes the window" true rather than aspirational: a stall that opened
// under a condition the tick does not re-check is one a player keeps open by never
// sending anything. Four conditions, and each is somebody else's rule read from here —
// the act gate every request in this package passes, the role list [vendorRole] owns, the
// view cube the snapshot is built from, and [EditReach], which is the distance every
// other interaction in this package is measured against.
//
// The caller holds sim.mu.
func (p *Player) tradeableLocked(r *resident) bool {
	if r == nil || p.cannotActLocked() != nil || !vendorRole(r.role) {
		return false
	}
	if !withinView(p.chunk, r.chunk, p.sim.viewDistance) {
		return false
	}
	distance := boxDistance(playerBox(p.pos), residentBody.boxAt(r.pos))
	return !math.IsNaN(distance) && distance <= EditReach
}

// openVendorLocked starts one stall session, having already decided that it may open.
//
// **Re-addressing the vendor already open is not a new session**, and the difference
// matters: the revision would restart at 1, and a trade written against the previous
// session's revision 1 would stop being stale without anything having told the client its
// list had been replaced. So the same vendor is re-sent at the revision it already has,
// and only a *different* one closes the first — which is loot's rule with the same words.
//
// The caller holds sim.mu.
func (p *Player) openVendorLocked(r *resident) {
	if p.openVendorID == r.entityID {
		p.vendorDirty = true
		return
	}
	p.closeVendorLocked()
	p.openVendorID = r.entityID
	// One, never zero: `VendorState.revision` is non-zero by contract, and zero is what
	// this field means when no stall is open.
	p.vendorRevision = 1
	p.vendorDirty = true
}

// Trade moves silver one way and goods the other, or moves nothing at all.
//
// **The whole transaction happens inside one TryLock window and on one copy of the slot
// table**, and those two facts are the entirety of the atomicity argument. The copy is
// what makes "nothing spent" true without an unwind — a purchase that pays for a pickaxe
// and then finds no room for it throws the copy away, exactly as `craft` does — and the
// window is what makes "what fits" a question about a pack no other request can be
// halfway through changing. TryLock rather than Lock, for the reason every other holder
// records: the tick takes this lock only under sim.mu, which this call is already
// holding, so waiting on it is the one thing that could deadlock.
//
// **Silver is consumed and the goods inserted in that order, deliberately.** A player
// whose purse is the last slot with room in it can buy something, because paying empties
// the slot the purchase goes into; doing it the other way round would refuse that trade
// with a full pack the player is about to have room in.
//
// The caller is the session goroutine. No frame is produced here: the accepted trade
// dirties the inventory and the stall, and the tick delivers both complete states.
func (p *Player) Trade(req protocol.TradeRequest) (vnet.RefusalReason, error) {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()

	if err := p.cannotActLocked(); err != nil {
		return vnet.RefusalReasonPlayerIsDead, err
	}
	if p.haveTradeTick && !newerTick(req.ClientTick, p.lastTradeTick) {
		return vnet.RefusalReasonUnknown, fmt.Errorf("stale trade client tick %d; newest is %d", req.ClientTick, p.lastTradeTick)
	}
	p.haveTradeTick, p.lastTradeTick = true, req.ClientTick

	// The stall, and every reason it might not be one: the request names a vendor this
	// session does not have open, or one it does that has since stopped being reachable.
	// `NotAVendor` for all of them, which is the answer [Player.InteractNPC] gives an
	// address that opens nothing and for the same reason — nothing a client can send here
	// tells it anything about the world it could not already see.
	if p.openVendorID == 0 || p.openVendorID != req.EntityID {
		return vnet.RefusalReasonNotAVendor, fmt.Errorf("entity %d is not the stall this session has open", req.EntityID)
	}
	r, standing := p.sim.residents[p.openVendorID]
	if !standing || !p.tradeableLocked(r) {
		p.closeVendorLocked()
		return vnet.RefusalReasonNotAVendor, fmt.Errorf("the stall at entity %d is no longer open", req.EntityID)
	}
	if req.Revision != p.vendorRevision {
		return vnet.RefusalReasonStaleRevision, fmt.Errorf("trade revision %d is not current revision %d", req.Revision, p.vendorRevision)
	}
	if req.Count == 0 {
		// Unreachable through protocol.Decode, which refuses an absent count as
		// malformed, and answered anyway: a trade for nothing is a defect in the sender
		// rather than an outcome to report, so it is logged and no frame is sent.
		return vnet.RefusalReasonUnknown, errors.New("a trade of zero items asks for nothing")
	}

	item := ItemID(req.ItemID)
	price, traded := vendorTable[r.role].priceOf(item, req.Buying)
	if !traded {
		return vnet.RefusalReasonVendorDoesNotWant, fmt.Errorf("%s does not trade item %d in that direction", r.name, req.ItemID)
	}
	// uint32 throughout, because uint16 × uint16 overflows a uint16 and the overflow is
	// the shape of a free purchase: 65536 arrows at one silver would total zero.
	total := uint32(price) * uint32(req.Count)

	if !p.inventory.mu.TryLock() {
		return vnet.RefusalReasonInventoryBusy, errors.New("the inventory is busy")
	}
	defer p.inventory.mu.Unlock()

	next := p.inventory.slots
	if req.Buying {
		// The comparison and the spend are both uint32, and that is the entirety of the
		// safety argument — there is no narrowing left to justify. A purse is a sum over
		// pack slots rather than a single stack, so `heldInPack` can exceed what one slot
		// holds and a total it covers can too; the old `uint16(total)` here was checked
		// against the wide value and then spent as the narrow one, which is the shape of
		// a free purchase: a 90,000 silver total was spent as `uint16(90000)` = 24,464.
		// The comment that stood here argued that was safe because "forty slots of a
		// uint16 count" fit a uint16 — which is backwards, and is why the truncation was
		// reachable rather than why it was not.
		//
		// **What kept it from actually being a free purchase was somewhere else, which is
		// the reason to fix it here rather than to rely on that.** Delivery still has to
		// fit, and no row of today's table can put goods worth more than 65,535 silver
		// into a 36-slot pack — the best `price x maxStack x 36` is 1,728 — so `insert`
		// refused and the copy was discarded. What did get through was the wrong refusal:
		// at a total of 65,536 the low bits are zero, `consumePack` returned false on its
		// `count == 0` guard, and a purse deep enough to pay was told it was short.
		//
		// And the purse is genuinely not bounded by a stack maximum: `restoredSlots`
		// copies a stored count straight from the record on purpose, and
		// `validateStoredSlot` bounds it by nothing but the uint16 it is stored in.
		if next.heldInPack(ItemSilver) < total {
			return vnet.RefusalReasonNotEnoughSilver, fmt.Errorf("%d silver is more than the purse holds", total)
		}
		if !next.consumePack(ItemSilver, total) {
			return vnet.RefusalReasonNotEnoughSilver, fmt.Errorf("the purse did not yield %d silver", total)
		}
		if remaining := next.insert(item, req.Count); remaining != 0 {
			return vnet.RefusalReasonInventoryFull, fmt.Errorf("%d of item %d do not fit", remaining, req.ItemID)
		}
	} else {
		// The pack, never the four equipment slots: nothing worn is for sale, which is
		// the rule consumePack exists for. Held-and-not-enough and not-held-at-all are
		// one answer, because a vendor that does not want six of something it will take
		// one of is the same sentence to a player either way.
		if !next.consumePack(item, uint32(req.Count)) {
			return vnet.RefusalReasonVendorDoesNotWant, fmt.Errorf("the pack does not hold %d of item %d", req.Count, req.ItemID)
		}
		// This branch keeps an explicit MaxUint16 refusal and the buying one needs none,
		// because the two guard different widths: `insert` writes a single slot's count
		// and genuinely takes a uint16, while a spend walks the whole pack and now takes
		// the uint32 it always summed to. Paying is bounded by what the purse holds;
		// being paid is bounded by what one call to `insert` can be asked for.
		if total > math.MaxUint16 {
			return vnet.RefusalReasonInventoryFull, fmt.Errorf("%d silver is more than a pack could hold", total)
		}
		if remaining := next.insert(ItemSilver, uint16(total)); remaining != 0 {
			return vnet.RefusalReasonInventoryFull, fmt.Errorf("%d of the %d silver does not fit", remaining, total)
		}
	}
	p.inventory.slots = next

	// No refreshWornLocked, and it is worth saying why rather than leaving its absence to
	// look like an omission: both halves above are bounded to the pack — `insert` writes
	// only below equipmentFirst and `consumePack` reads only below it — so no equipment
	// slot can have changed and the combat summary derived from those four is still true.
	p.inventoryDirty = true
	p.vendorRevision++
	p.vendorDirty = true

	p.sim.log.Debug("trade applied",
		"entity_id", p.entityID, "vendor_id", r.entityID, "vendor_role", r.role.String(),
		"item", req.ItemID, "count", req.Count, "buying", req.Buying,
		"silver", total, "revision", p.vendorRevision, "client_tick", req.ClientTick)

	return vnet.RefusalReasonUnknown, nil
}

// offerVendorLocked reviews the open stall, then retries what this session is owed.
//
// **The review is first, and that ordering is the feature**: a stall closed here queues
// its `VendorClosed` in time for the drain below, so walking away ends the window on the
// tick it stopped being reachable rather than on the one after. It is also the only thing
// that closes a stall a player simply walks out of — there is no message for it, by
// contract, and a server that waited for one would hold a session open for ever.
//
// Closures before the state, for [Player.offerLootLocked]'s reason: a stall that has ended
// must not be described again by a frame that was queued before it did.
//
// The caller holds sim.mu.
func (p *Player) offerVendorLocked() {
	if p.openVendorID != 0 && !p.tradeableLocked(p.sim.residents[p.openVendorID]) {
		p.closeVendorLocked()
	}
	for len(p.vendorClosures) > 0 {
		id := p.vendorClosures[0]
		if !p.deliver(protocol.EncodeVendorClosed(protocol.VendorClosed{EntityID: id})) {
			return
		}
		p.vendorClosures = p.vendorClosures[1:]
	}
	if !p.vendorDirty || p.openVendorID == 0 {
		return
	}
	if p.deliver(protocol.EncodeVendorState(vendorState(p.sim.residents[p.openVendorID], p.vendorRevision))) {
		p.vendorDirty = false
	}
}
