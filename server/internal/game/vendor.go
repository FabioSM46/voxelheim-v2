package game

import (
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
// **This file is the window, not the transaction.** #459 is two pull requests on the
// server side and this is the first: the table, and the session that decides which stall
// a player has open and which revision they are looking at. `TradeRequest` — the atomic
// buy and sell inside one `inventory.mu.TryLock` window — is the second, and it is
// deliberately not stubbed here. Nothing routes that message today, which is exactly
// where develop already stands; a refusal-only case would be a stand-in for a dependency
// that is merely late rather than absent.
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
