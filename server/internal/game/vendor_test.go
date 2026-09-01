package game

import (
	"testing"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// What a stall is, asked from four directions: what it deals in, when it opens and when
// it stops being open, what one trade moves, and what a refused one moves — which is
// nothing, in every direction there is to refuse one.
//
// Every assertion here is about what the *server* decided. The trades a test performs go
// through Player.Trade rather than through the slot table, because the whole question
// this file asks is what happens inside the one TryLock window.

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

// stall stands one vendor of the named role an arm's length from a fresh player and
// leaves the window open, so a test that is about trading is not also about walking up.
func stall(t *testing.T, role vnet.ResidentRole) (*vitalsHarness, *Player, *dropSink, *resident) {
	t.Helper()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	r := h.standResidentAt(role, [3]float64{1.5, 64, 0.5}, 0)

	reason, err := player.InteractNPC(protocol.NpcInteractRequest{EntityID: r.entityID, ClientTick: 1})
	if err != nil {
		t.Fatalf("addressing a %s was refused %s: %v", role, reason, err)
	}
	return h, player, out, r
}

// stock puts an item straight into a slot. Currency has its own helper below because no
// inventory slot is allowed to hold it.
func (h *vitalsHarness) stock(p *Player, slot uint8, item ItemID, count uint16) {
	h.t.Helper()

	p.inventory.mu.Lock()
	defer p.inventory.mu.Unlock()
	p.inventory.slots[slot] = stackOf(item, count)
}

func (h *vitalsHarness) fund(p *Player, silver uint32) {
	h.t.Helper()

	p.inventory.mu.Lock()
	defer p.inventory.mu.Unlock()
	p.inventory.silver = silver
}

// carrying is how many of one item the player holds in the pack, read under the lock
// that owns it.
func (h *vitalsHarness) carrying(p *Player, item ItemID) uint32 {
	p.inventory.mu.Lock()
	defer p.inventory.mu.Unlock()
	return p.inventory.slots.heldInPack(item)
}

func (h *vitalsHarness) purse(p *Player) uint32 {
	p.inventory.mu.Lock()
	defer p.inventory.mu.Unlock()
	return p.inventory.silver
}

// standAt moves the player without simulating the walk, which is what a reach test needs:
// where the body ends up is the question, and how it got there is movement.go's.
func (h *vitalsHarness) standAt(p *Player, pos [3]float64) {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	p.pos = pos
}

// openStall is the vendor this player has open, read under the lock that owns the field.
// Zero is "no stall at all", which is what [Player.closeVendorLocked] leaves behind and
// what every refused address must leave untouched.
func (h *vitalsHarness) openStall(p *Player) uint64 {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	return p.openVendorID
}

// vendorStates is every complete price list this session was sent, in order.
func (s *dropSink) vendorStates(t *testing.T) []protocol.VendorState {
	t.Helper()

	var sent []protocol.VendorState
	for _, frame := range s.all() {
		envelope := vnet.GetRootAsEnvelope(frame, 0)
		if envelope.PayloadType() != vnet.PayloadVendorState {
			continue
		}
		var table flatbuffers.Table
		if !envelope.Payload(&table) {
			t.Fatal("the vendor state payload is absent")
		}
		var payload vnet.VendorState
		payload.Init(table.Bytes, table.Pos)

		one := protocol.VendorState{EntityID: payload.EntityId(), Revision: payload.Revision()}
		for index := range payload.SellsLength() {
			var entry vnet.VendorEntry
			if !payload.Sells(&entry, index) {
				t.Fatalf("sells entry %d is missing from a list that claims to hold it", index)
			}
			one.Sells = append(one.Sells, protocol.VendorEntry{ItemID: entry.ItemId(), Price: entry.Price()})
		}
		for index := range payload.BuysLength() {
			var entry vnet.VendorEntry
			if !payload.Buys(&entry, index) {
				t.Fatalf("buys entry %d is missing from a list that claims to hold it", index)
			}
			one.Buys = append(one.Buys, protocol.VendorEntry{ItemID: entry.ItemId(), Price: entry.Price()})
		}
		sent = append(sent, one)
	}
	return sent
}

// vendorClosures is every stall this session was explicitly told had ended.
func (s *dropSink) vendorClosures(t *testing.T) []uint64 {
	t.Helper()

	var ended []uint64
	for _, frame := range s.all() {
		envelope := vnet.GetRootAsEnvelope(frame, 0)
		if envelope.PayloadType() != vnet.PayloadVendorClosed {
			continue
		}
		var table flatbuffers.Table
		if !envelope.Payload(&table) {
			t.Fatal("the vendor closed payload is absent")
		}
		var payload vnet.VendorClosed
		payload.Init(table.Bytes, table.Pos)
		ended = append(ended, payload.EntityId())
	}
	return ended
}

// newest is the last price list this session was sent, and a failure when there is none.
func newestVendorState(t *testing.T, out *dropSink) protocol.VendorState {
	t.Helper()

	sent := out.vendorStates(t)
	if len(sent) == 0 {
		t.Fatal("the session was sent no price list at all")
	}
	return sent[len(sent)-1]
}

// ---------------------------------------------------------------------------
// What the trades deal in
// ---------------------------------------------------------------------------

// **Every item named exists and every price is real money.** The table is the one place a
// price is written down, so this is the one place it can be wrong: an unregistered item id
// is a row that would refuse every trade it named, and a price of zero is a vendor giving
// something away — which the contract calls a price list with a hole in it, because free
// is not a price.
func TestEveryPriceNamesARealItemAndCostsSomething(t *testing.T) {
	t.Parallel()

	for role, stock := range vendorTable {
		for direction, list := range map[string][]vendorEntry{"sells": stock.sells, "buys": stock.buys} {
			seen := make(map[ItemID]bool, len(list))
			for _, entry := range list {
				if _, registered := itemByID(entry.item); !registered || entry.item == ItemNone {
					t.Errorf("the %s %s item %d, which this world has no such thing as", role, direction, entry.item)
				}
				if entry.price == 0 {
					t.Errorf("the %s %s item %d for nothing, and free is not a price", role, direction, entry.item)
				}
				if seen[entry.item] {
					t.Errorf("the %s names item %d twice in %s, which no VendorState may carry", role, entry.item, direction)
				}
				seen[entry.item] = true
			}
		}
		if len(stock.sells) == 0 && len(stock.buys) == 0 {
			t.Errorf("the %s trades in nothing, which is a closed stall rather than an open one", role)
		}
	}
}

// The predicate and the table are one answer. A role that could keep a stall and has no
// row would open an empty window; a row nobody can reach is a price nobody pays.
func TestTheStallKeepersAndThePriceListAgree(t *testing.T) {
	t.Parallel()

	for _, role := range []vnet.ResidentRole{
		vnet.ResidentRoleUnknown, vnet.ResidentRoleVillager, vnet.ResidentRoleGuard,
		vnet.ResidentRoleSmith, vnet.ResidentRoleCarpenter,
		vnet.ResidentRoleCook, vnet.ResidentRoleTrader,
	} {
		_, priced := vendorTable[role]
		if keeps := vendorRole(role); keeps != priced {
			t.Errorf("vendorRole(%s) = %v but the table has a row for it = %v", role, keeps, priced)
		}
	}
}

// The two directions are read from the two vectors, never from a sign, and an item that
// is in both is answered differently in each. The trader's leather patch is the case:
// bought from the player at 2 and sold back at 4, and the spread is the vendor's.
func TestAPriceIsReadFromTheDirectionItIsAskedFor(t *testing.T) {
	t.Parallel()

	trader := vendorTable[vnet.ResidentRoleTrader]
	if price, traded := trader.priceOf(ItemLeatherPatch, true); !traded || price != 4 {
		t.Errorf("a trader sells a leather patch at %d (traded %v), want 4", price, traded)
	}
	if price, traded := trader.priceOf(ItemLeatherPatch, false); !traded || price != 2 {
		t.Errorf("a trader buys a leather patch at %d (traded %v), want 2", price, traded)
	}
	if _, traded := trader.priceOf(ItemIronSword, true); traded {
		t.Error("a trader sells iron swords, which is the smith's row")
	}
	if _, traded := vendorTable[vnet.ResidentRoleCook].priceOf(ItemBone, false); traded {
		t.Error("a cook buys bones")
	}
}

// ---------------------------------------------------------------------------
// Opening, and every way one stops being open
// ---------------------------------------------------------------------------

// Addressing a trade opens a session and the tick delivers the complete list: the
// vendor's own entity id, revision 1, and one row per table entry in each direction.
func TestAddressingASmithOpensTheStallAtRevisionOne(t *testing.T) {
	t.Parallel()

	h, player, out, r := stall(t, vnet.ResidentRoleSmith)
	h.step()

	if player.openVendorID != r.entityID {
		t.Fatalf("the session has stall %d open, want %d", player.openVendorID, r.entityID)
	}
	state := newestVendorState(t, out)
	if state.EntityID != r.entityID {
		t.Errorf("the list names entity %d, want %d", state.EntityID, r.entityID)
	}
	if state.Revision != 1 {
		t.Errorf("the list opens at revision %d, want 1", state.Revision)
	}

	stock := vendorTable[vnet.ResidentRoleSmith]
	if len(state.Sells) != len(stock.sells) || len(state.Buys) != len(stock.buys) {
		t.Fatalf("the list carries %d sells and %d buys, want %d and %d",
			len(state.Sells), len(state.Buys), len(stock.sells), len(stock.buys))
	}
	for index, entry := range stock.sells {
		want := protocol.VendorEntry{ItemID: uint16(entry.item), Price: entry.price}
		if state.Sells[index] != want {
			t.Errorf("sells row %d is %+v, want %+v", index, state.Sells[index], want)
		}
	}
	for index, entry := range stock.buys {
		want := protocol.VendorEntry{ItemID: uint16(entry.item), Price: entry.price}
		if state.Buys[index] != want {
			t.Errorf("buys row %d is %+v, want %+v", index, state.Buys[index], want)
		}
	}
}

// Opening a second stall ends the first explicitly, which is loot's rule: one open
// container per session, and the client is told rather than left drawing a window the
// server has stopped answering for.
func TestOpeningASecondStallClosesTheFirst(t *testing.T) {
	t.Parallel()

	h, player, out, smith := stall(t, vnet.ResidentRoleSmith)
	cook := h.standResidentAt(vnet.ResidentRoleCook, [3]float64{0.5, 64, 1.5}, 0)
	h.step()

	if _, err := player.InteractNPC(protocol.NpcInteractRequest{EntityID: cook.entityID, ClientTick: 2}); err != nil {
		t.Fatalf("addressing the cook was refused: %v", err)
	}
	h.step()

	if player.openVendorID != cook.entityID {
		t.Fatalf("the session has stall %d open, want the cook's %d", player.openVendorID, cook.entityID)
	}
	if ended := out.vendorClosures(t); len(ended) != 1 || ended[0] != smith.entityID {
		t.Fatalf("the session was told %v had ended, want exactly the smith's %d", ended, smith.entityID)
	}
	if state := newestVendorState(t, out); state.EntityID != cook.entityID || state.Revision != 1 {
		t.Errorf("the newest list is %d at revision %d, want the cook's %d at 1", state.EntityID, state.Revision, cook.entityID)
	}
}

// Addressing the stall that is already open is not a new session. The revision would
// restart at 1, and a trade written against the previous revision 1 would stop being
// stale without anything having told the client its list had been replaced.
func TestAddressingTheOpenStallAgainKeepsItsRevision(t *testing.T) {
	t.Parallel()

	h, player, out, r := stall(t, vnet.ResidentRoleCarpenter)
	h.fund(player, 50)
	h.step()

	if _, err := player.Trade(tradeFor(r, ItemPlanks, 5, true, 1, 2)); err != nil {
		t.Fatalf("buying five planks was refused: %v", err)
	}
	h.step()

	if _, err := player.InteractNPC(protocol.NpcInteractRequest{EntityID: r.entityID, ClientTick: 3}); err != nil {
		t.Fatalf("addressing the same carpenter again was refused: %v", err)
	}
	h.step()

	if state := newestVendorState(t, out); state.Revision != 2 {
		t.Errorf("re-addressing the open stall left it at revision %d, want the 2 the trade produced", state.Revision)
	}
	if ended := out.vendorClosures(t); len(ended) != 0 {
		t.Errorf("re-addressing the open stall closed %v", ended)
	}
}

// Walking away ends it. There is no message for it by contract, so a server that waited
// for one would hold the session open for ever — the tick re-asks the reach the open was
// granted under, and that is the only thing that can notice.
func TestWalkingOutOfReachClosesTheStall(t *testing.T) {
	t.Parallel()

	h, player, out, r := stall(t, vnet.ResidentRoleTrader)
	h.step()

	h.standAt(player, [3]float64{0.5, 64, EditReach + 6.5})
	h.step()

	if player.openVendorID != 0 {
		t.Errorf("the stall is still open at %d after the player walked away", player.openVendorID)
	}
	if ended := out.vendorClosures(t); len(ended) != 1 || ended[0] != r.entityID {
		t.Errorf("the session was told %v had ended, want exactly %d", ended, r.entityID)
	}
}

// Dying ends it, through the act gate every request in this package passes rather than
// through a rule of death's own.
func TestDyingClosesTheStall(t *testing.T) {
	t.Parallel()

	h, player, out, r := stall(t, vnet.ResidentRoleCook)
	h.step()

	h.hurt(player, PlayerMaxHealth)
	h.step()

	if player.openVendorID != 0 {
		t.Errorf("a dead player still has stall %d open", player.openVendorID)
	}
	if ended := out.vendorClosures(t); len(ended) != 1 || ended[0] != r.entityID {
		t.Errorf("the session was told %v had ended, want exactly %d", ended, r.entityID)
	}
}

// **And a stall a dead player never opened, which is the other half of the same gate.**
// [TestDyingClosesTheStall] above covers the window that was already open when the player
// died; the act gate at the top of [Player.InteractNPC] is what stops a corpse opening a
// new one, and until this test nothing asked it to.
//
// The code is [vnet.RefusalReasonPlayerIsDead] rather than the `NotAVendor` the four
// other refusals share, and the difference is deliberate: being dead is a fact the client
// already holds, so answering it plainly tells a probe nothing, and every other request in
// this package answers a corpse the same way. Nothing about the smith is reachable from
// the answer — the refusal lands before the resident is even looked up.
func TestADeadPlayerCannotOpenAStall(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	smith := h.standResidentAt(vnet.ResidentRoleSmith, [3]float64{1.5, 64, 0.5}, 0)

	h.hurt(player, PlayerMaxHealth)
	h.step()

	reason, err := player.InteractNPC(protocol.NpcInteractRequest{EntityID: smith.entityID, ClientTick: 1})
	if err == nil {
		t.Fatal("a dead player opened a stall")
	}
	if reason != vnet.RefusalReasonPlayerIsDead {
		t.Errorf("a dead player addressing a smith is refused %s, want PlayerIsDead", reason)
	}
	if open := h.openStall(player); open != 0 {
		t.Errorf("a dead player has stall %d open", open)
	}
	h.step()
	if states := out.vendorStates(t); len(states) != 0 {
		t.Errorf("a dead player was sent %d price lists", len(states))
	}
}

// The four conditions a stall stays open under, each failed on its own. The open, the
// trade and the tick all ask this one predicate, which is what makes "walking away closes
// the window" a property rather than an intention.
func TestTheStallHoldsOpenOnFourConditions(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	near := h.standResidentAt(vnet.ResidentRoleSmith, [3]float64{1.5, 64, 0.5}, 0)
	guard := h.standResidentAt(vnet.ResidentRoleGuard, [3]float64{2.5, 64, 0.5}, 0)
	far := h.standResidentAt(vnet.ResidentRoleTrader, [3]float64{0.5, 64, EditReach + 8.5}, 0)

	// Somebody standing well outside this session's view cube, which is the condition
	// reach alone cannot fail: at eight chunks the cube is far wider than EditReach, so
	// the two are only separable by putting a person the other side of it.
	outOfView := h.standResidentAt(vnet.ResidentRoleCook, [3]float64{4000.5, 64, 0.5}, 0)

	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()

	for _, one := range []struct {
		what      string
		r         *resident
		tradeable bool
	}{
		{"a smith within reach", near, true},
		{"nobody at all", nil, false},
		{"a guard within reach", guard, false},
		{"a trader past the reach", far, false},
		{"a cook outside the view cube", outOfView, false},
	} {
		if got := player.tradeableLocked(one.r); got != one.tradeable {
			t.Errorf("tradeableLocked(%s) = %v, want %v", one.what, got, one.tradeable)
		}
	}

	player.lifeState = vnet.LifeStateDead
	if player.tradeableLocked(near) {
		t.Error("a dead player is trading")
	}
}

// ---------------------------------------------------------------------------
// One trade
// ---------------------------------------------------------------------------

// tradeFor is the request a client sends, with the fields a test rarely varies filled in.
func tradeFor(r *resident, item ItemID, count uint16, buying bool, revision, tick uint32) protocol.TradeRequest {
	return protocol.TradeRequest{
		EntityID: r.entityID, ItemID: uint16(item), Count: count,
		Buying: buying, Revision: revision, ClientTick: tick,
	}
}

// A purchase moves both stacks and bumps the revision. Twenty-five silver for a pickaxe,
// out of the fifty the player is carrying — and the fresh list the tick sends afterwards
// is what the next request must be written against.
func TestBuyingAPickaxeSpendsTheSilverAndBumpsTheRevision(t *testing.T) {
	t.Parallel()

	h, player, out, r := stall(t, vnet.ResidentRoleSmith)
	h.fund(player, 50)
	h.step()

	reason, err := player.Trade(tradeFor(r, ItemPickaxe, 1, true, 1, 2))
	if err != nil {
		t.Fatalf("buying a pickaxe was refused %s: %v", reason, err)
	}
	h.step()

	if got := h.purse(player); got != 25 {
		t.Errorf("the purse holds %d silver after a 25-silver purchase from 50, want 25", got)
	}
	if got := h.carrying(player, ItemPickaxe); got != 1 {
		t.Errorf("the pack holds %d pickaxes, want 1", got)
	}
	if state := newestVendorState(t, out); state.Revision != 2 {
		t.Errorf("the list is at revision %d after one trade, want 2", state.Revision)
	}
	if states := out.inventoryStates(t); len(states) == 0 {
		t.Error("a trade that moved two stacks sent no inventory state")
	}
}

// The count is the multiplier and the total is the server's. Ten arrows at one silver is
// ten silver, and the arrows arrive as one stack rather than as ten.
func TestBuyingTenArrowsCostsTenSilver(t *testing.T) {
	t.Parallel()

	h, player, _, r := stall(t, vnet.ResidentRoleCarpenter)
	h.fund(player, 30)
	h.step()

	if _, err := player.Trade(tradeFor(r, ItemArrow, 10, true, 1, 2)); err != nil {
		t.Fatalf("buying ten arrows was refused: %v", err)
	}
	if got := h.purse(player); got != 20 {
		t.Errorf("the purse holds %d silver, want 20", got)
	}
	if got := h.carrying(player, ItemArrow); got != 10 {
		t.Errorf("the pack holds %d arrows, want 10", got)
	}
}

// Selling is the same window in the other direction: the goods go and the silver arrives.
func TestSellingBonesPaysForThem(t *testing.T) {
	t.Parallel()

	h, player, out, r := stall(t, vnet.ResidentRoleTrader)
	h.stock(player, 0, ItemBone, 10)
	h.stock(player, 1, ItemVargrPelt, 3)
	h.step()

	if _, err := player.Trade(tradeFor(r, ItemVargrPelt, 3, false, 1, 2)); err != nil {
		t.Fatalf("selling three pelts was refused: %v", err)
	}
	h.step()

	if got := h.carrying(player, ItemVargrPelt); got != 0 {
		t.Errorf("the pack still holds %d pelts after selling all three", got)
	}
	if got := h.purse(player); got != 12 {
		t.Errorf("three pelts at 4 paid %d silver, want 12", got)
	}
	if got := h.carrying(player, ItemBone); got != 10 {
		t.Errorf("selling pelts took %d of the 10 bones", 10-got)
	}
	if state := newestVendorState(t, out); state.Revision != 2 {
		t.Errorf("the list is at revision %d after one sale, want 2", state.Revision)
	}
}

// A sale needs no empty slot for its payment. The sold stack remains present, so the
// pack is still completely full after one pelt leaves it; only the purse changes.
func TestSellingWithAFullPackPaysThePurse(t *testing.T) {
	t.Parallel()

	h, player, _, r := stall(t, vnet.ResidentRoleTrader)
	h.stock(player, 0, ItemVargrPelt, 3)
	for slot := 1; slot < equipmentFirst; slot++ {
		h.stock(player, uint8(slot), ItemStone, 64)
	}
	h.step()

	if _, err := player.Trade(tradeFor(r, ItemVargrPelt, 1, false, 1, 2)); err != nil {
		t.Fatalf("selling from a full pack was refused: %v", err)
	}
	if got := h.carrying(player, ItemVargrPelt); got != 2 {
		t.Errorf("the pack holds %d pelts, want 2", got)
	}
	if got := h.purse(player); got != 4 {
		t.Errorf("the purse holds %d silver, want 4", got)
	}
	for slot, stack := range player.InventoryState().Stacks[:equipmentFirst] {
		if stack.ItemID == 0 {
			t.Errorf("pack slot %d became empty; the sale did not need room for silver", slot)
		}
	}
}

// ---------------------------------------------------------------------------
// Every refusal moves nothing
// ---------------------------------------------------------------------------

// unchanged fails the test unless the pack is exactly as the caller left it.
type inventorySnapshot struct {
	slots  slotTable
	silver uint32
}

func (h *vitalsHarness) unchanged(p *Player, was inventorySnapshot, what string) {
	h.t.Helper()

	p.inventory.mu.Lock()
	defer p.inventory.mu.Unlock()
	got := inventorySnapshot{slots: p.inventory.slots, silver: p.inventory.silver}
	if got != was {
		h.t.Errorf("%s moved something: inventory is %+v, want %+v", what, got, was)
	}
}

// pack is a copy of the authoritative slots, for a test to compare against afterwards.
func (h *vitalsHarness) pack(p *Player) inventorySnapshot {
	p.inventory.mu.Lock()
	defer p.inventory.mu.Unlock()
	return inventorySnapshot{slots: p.inventory.slots, silver: p.inventory.silver}
}

// A purse that is short buys nothing, and the silver it does hold stays where it is. The
// server owns the price and the purse both: this is the answer that corrects a client
// whose arithmetic disagreed.
func TestBuyingWithoutTheSilverRefusesAndSpendsNothing(t *testing.T) {
	t.Parallel()

	h, player, _, r := stall(t, vnet.ResidentRoleSmith)
	h.fund(player, 39)
	h.step()
	was := h.pack(player)

	reason, err := player.Trade(tradeFor(r, ItemIronSword, 1, true, 1, 2))
	if err == nil {
		t.Fatal("thirty-nine silver bought a forty-silver sword")
	}
	if reason != vnet.RefusalReasonNotEnoughSilver {
		t.Errorf("a short purse is refused %s, want NotEnoughSilver", reason)
	}
	h.unchanged(player, was, "a purchase nobody could afford")
	if player.vendorRevision != 1 {
		t.Errorf("a refused purchase bumped the revision to %d", player.vendorRevision)
	}
}

// **A full pack refuses the purchase and spends nothing**, which is the whole reason the
// trade runs on a copy: paying and then finding no room would leave the silver gone and
// nothing bought.
//
// The purse deliberately holds more than the purchase costs, so paying does not empty its
// slot — a slot the payment frees is a slot the goods can go into, which is the ordinary
// case and is tested by the purchases above.
func TestBuyingIntoAFullPackRefusesAndSpendsNothing(t *testing.T) {
	t.Parallel()

	h, player, _, r := stall(t, vnet.ResidentRoleSmith)
	h.fund(player, 200)
	for slot := 0; slot < equipmentFirst; slot++ {
		h.stock(player, uint8(slot), ItemStone, 60000)
	}
	h.step()
	was := h.pack(player)

	reason, err := player.Trade(tradeFor(r, ItemSharpeningStone, 1, true, 1, 2))
	if err == nil {
		t.Fatal("a pack with no empty slot took delivery of a sharpening stone")
	}
	if reason != vnet.RefusalReasonInventoryFull {
		t.Errorf("a full pack is refused %s, want InventoryFull", reason)
	}
	h.unchanged(player, was, "a purchase with nowhere to put it")
}

// Selling what the pack does not hold is refused, and holding some of it is the same
// answer as holding none: a vendor that does not want six of something it will take one
// of is one sentence to a player either way.
func TestSellingWhatIsNotThereRefusesAndMovesNothing(t *testing.T) {
	t.Parallel()

	h, player, _, r := stall(t, vnet.ResidentRoleTrader)
	h.stock(player, 0, ItemBone, 2)
	h.step()
	was := h.pack(player)

	reason, err := player.Trade(tradeFor(r, ItemBone, 5, false, 1, 2))
	if err == nil {
		t.Fatal("two bones sold as five")
	}
	if reason != vnet.RefusalReasonVendorDoesNotWant {
		t.Errorf("selling more than is held is refused %s, want VendorDoesNotWant", reason)
	}
	h.unchanged(player, was, "a sale of what was not there")
}

// An item the vendor does not deal in at all, in either direction. The cook buys raw meat
// and sells the cooked kind; neither is a bone, and a smith is not the cook.
func TestAVendorRefusesWhatItDoesNotDealIn(t *testing.T) {
	t.Parallel()

	h, player, _, r := stall(t, vnet.ResidentRoleCook)
	h.fund(player, 100)
	h.stock(player, 1, ItemBone, 10)
	h.step()
	was := h.pack(player)

	for _, one := range []struct {
		what    string
		request protocol.TradeRequest
	}{
		{"buying a bone from a cook", tradeFor(r, ItemBone, 1, true, 1, 2)},
		{"selling a bone to a cook", tradeFor(r, ItemBone, 1, false, 1, 3)},
		{"selling the cooked meat a cook only sells", tradeFor(r, ItemCookedMeat, 1, false, 1, 4)},
	} {
		reason, err := player.Trade(one.request)
		if err == nil {
			t.Errorf("%s went through", one.what)
		}
		if reason != vnet.RefusalReasonVendorDoesNotWant {
			t.Errorf("%s is refused %s, want VendorDoesNotWant", one.what, reason)
		}
	}
	h.unchanged(player, was, "three trades a cook does not deal in")
}

// A request written against a list the server has replaced is refused rather than applied
// to a different one, which is LootState's rule and is why the revision exists.
func TestAStaleRevisionRefusesTheTrade(t *testing.T) {
	t.Parallel()

	h, player, _, r := stall(t, vnet.ResidentRoleCarpenter)
	h.fund(player, 100)
	h.step()

	if _, err := player.Trade(tradeFor(r, ItemPlanks, 1, true, 1, 2)); err != nil {
		t.Fatalf("the first purchase was refused: %v", err)
	}
	was := h.pack(player)

	reason, err := player.Trade(tradeFor(r, ItemPlanks, 1, true, 1, 3))
	if err == nil {
		t.Fatal("a request against the replaced list went through")
	}
	if reason != vnet.RefusalReasonStaleRevision {
		t.Errorf("a stale revision is refused %s, want StaleRevision", reason)
	}
	h.unchanged(player, was, "a trade against a list that had been replaced")
}

// Trading with a stall this session does not have open is refused the way addressing a
// non-vendor is: nothing a client sends here tells it anything it could not already see.
func TestTradingWithNoStallOpenIsRefused(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	r := h.standResidentAt(vnet.ResidentRoleSmith, [3]float64{1.5, 64, 0.5}, 0)
	h.fund(player, 100)
	h.step()
	was := h.pack(player)

	reason, err := player.Trade(tradeFor(r, ItemShovel, 1, true, 1, 2))
	if err == nil {
		t.Fatal("a stall nobody opened sold a shovel")
	}
	if reason != vnet.RefusalReasonNotAVendor {
		t.Errorf("trading with an unopened stall is refused %s, want NotAVendor", reason)
	}
	h.unchanged(player, was, "a trade with a stall that was never opened")
}

// Walking away between the open and the request refuses the trade and ends the stall,
// rather than leaving a session that can trade from across the village.
func TestTradingAfterWalkingAwayRefusesAndCloses(t *testing.T) {
	t.Parallel()

	h, player, _, r := stall(t, vnet.ResidentRoleSmith)
	h.fund(player, 100)
	h.step()
	was := h.pack(player)

	h.standAt(player, [3]float64{0.5, 64, EditReach + 6.5})
	reason, err := player.Trade(tradeFor(r, ItemShovel, 1, true, 1, 2))
	if err == nil {
		t.Fatal("a shovel was bought from across the village")
	}
	if reason != vnet.RefusalReasonNotAVendor {
		t.Errorf("trading out of reach is refused %s, want NotAVendor", reason)
	}
	if player.openVendorID != 0 {
		t.Errorf("the stall is still open at %d", player.openVendorID)
	}
	h.unchanged(player, was, "a trade from out of reach")
}

// A dead player trades nothing, through the same act gate that refuses them mining,
// crafting and looting.
func TestADeadPlayerTradesNothing(t *testing.T) {
	t.Parallel()

	h, player, _, r := stall(t, vnet.ResidentRoleTrader)
	h.fund(player, 100)
	h.step()
	was := h.pack(player)

	h.hurt(player, PlayerMaxHealth)
	reason, err := player.Trade(tradeFor(r, ItemCampfire, 1, true, 1, 2))
	if err == nil {
		t.Fatal("a corpse bought a campfire")
	}
	if reason != vnet.RefusalReasonPlayerIsDead {
		t.Errorf("a dead player is refused %s, want PlayerIsDead", reason)
	}
	h.unchanged(player, was, "a trade by somebody who was dead")
	if reason, err := player.InteractNPC(protocol.NpcInteractRequest{EntityID: r.entityID, ClientTick: 3}); err == nil {
		t.Error("a corpse opened a stall")
	} else if reason != vnet.RefusalReasonPlayerIsDead {
		t.Errorf("a dead player addressing a trader is refused %s, want PlayerIsDead", reason)
	}
}

// ---------------------------------------------------------------------------
// What a sale may never reach
// ---------------------------------------------------------------------------

// **Nothing worn is for sale**, and consumePack is where that is true. No row of today's
// table names a piece of armour, so this is asked of the helper directly rather than
// through a trade — the rule has to hold before the row that would exercise it is added,
// not after.
func TestSpendingFromThePackNeverReachesWhatIsWorn(t *testing.T) {
	t.Parallel()

	var slots slotTable
	slots[0] = stackOf(ItemIronHelm, 1)
	slots[equipmentHead] = stackOf(ItemIronHelm, 1)

	if held := slots.heldInPack(ItemIronHelm); held != 1 {
		t.Errorf("the pack is counted as holding %d iron helms, want the 1 that is in it rather than the one on the player's head", held)
	}

	// A copy per attempt, because consumeWithin does not unwind a partial spend — the
	// discipline every caller of it keeps, and the reason Player.Trade runs the whole
	// transaction on a copy of the table.
	short := slots
	if short.consumePack(ItemIronHelm, 2) {
		t.Error("selling two helms took the one the player was wearing")
	}
	if short[equipmentHead] != slots[equipmentHead] {
		t.Error("a pack spend that ran out reached into an equipment slot for the rest")
	}

	one := slots
	if !one.consumePack(ItemIronHelm, 1) {
		t.Fatal("the one helm in the pack could not be spent")
	}
	if one[equipmentHead] != slots[equipmentHead] {
		t.Error("spending from the pack emptied an equipment slot")
	}

	// And the unbounded form still reaches everything, which is what craft relies on.
	all := slots
	if !all.consume(ItemIronHelm, 2) {
		t.Error("consume could not reach both helms")
	}
}

// ---------------------------------------------------------------------------
// A total wider than a slot
// ---------------------------------------------------------------------------

// **A purchase the purse can plainly afford is never refused for want of silver**, and a
// purchase it cannot must not be settled for a fraction of its price.
//
// Sixteen thousand three hundred and eighty-four leather patches at four silver is
// exactly 65,536, whose low sixteen bits are zero. Under the truncation this pins, a
// player carrying 131,070 silver was told the purse would not yield — the honest refusal
// is the one the pack gives, because 36 slots of eight patches cannot take delivery of
// 16,384 of them. Both halves are asserted: which refusal arrives, and that a refused
// trade moved nothing.
func TestABuyingTotalWiderThanASlotIsNotTruncated(t *testing.T) {
	t.Parallel()

	h, player, _, r := stall(t, vnet.ResidentRoleTrader)
	h.fund(player, 131070)
	h.step()
	was := h.pack(player)

	if held := h.purse(player); held != 131070 {
		t.Fatalf("the purse holds %d silver, want the 131070 the test put in it", held)
	}

	reason, err := player.Trade(tradeFor(r, ItemLeatherPatch, 16384, true, 1, 2))
	if err == nil {
		t.Fatal("a 36-slot pack took delivery of 16384 leather patches")
	}
	if reason == vnet.RefusalReasonNotEnoughSilver {
		t.Error("a purse holding 131070 silver was refused a 65536-silver purchase for want of silver: the total was narrowed to a uint16 before it was paid")
	}
	if reason != vnet.RefusalReasonInventoryFull {
		t.Errorf("a purchase with nowhere to go is refused %s, want InventoryFull", reason)
	}
	h.unchanged(player, was, "a purchase costing more than a slot can count")
}

// Silver is one character counter and no slot participates in paying.
func TestThePurseIsNotAnInventorySlot(t *testing.T) {
	t.Parallel()

	h, player, _, r := stall(t, vnet.ResidentRoleSmith)
	h.fund(player, 45)
	h.step()

	if held := h.purse(player); held != 45 {
		t.Fatalf("the purse holds %d, want 45", held)
	}
	if _, err := player.Trade(tradeFor(r, ItemIronCuirass, 1, true, 1, 2)); err != nil {
		t.Fatalf("forty-five silver did not buy a forty-five-silver cuirass: %v", err)
	}
	if held := h.purse(player); held != 0 {
		t.Errorf("%d silver survived a purchase that cost every coin", held)
	}
	for slot, stack := range player.InventoryState().Stacks {
		if stack.ItemID == uint16(ItemSilver) {
			t.Errorf("slot %d holds reserved silver id", slot)
		}
	}
}
