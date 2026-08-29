package game

import (
	"testing"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// What a stall is, asked from two directions: what it deals in, and when it opens and
// stops being open. What one trade moves — and what a refused one moves, which is
// nothing — is the second half of the server side of #459 and arrives with
// Player.Trade.
//
// Every assertion here is about what the *server* decided. A stall is opened through
// Player.InteractNPC and closed by the tick rather than by reaching into the fields,
// because the whole question this file asks is what the authoritative path does.

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
// restart at 1, and a request written against the previous revision 1 would stop being
// stale without anything having told the client its list had been replaced.
//
// The revision is advanced by hand rather than by the trade that will advance it in
// life, because a guard against "resetting to 1" is invisible while the number is
// already 1 — and what is under test is openVendorLocked, not what moved the counter.
func TestAddressingTheOpenStallAgainKeepsItsRevision(t *testing.T) {
	t.Parallel()

	h, player, out, r := stall(t, vnet.ResidentRoleCarpenter)
	h.step()

	h.sim.mu.Lock()
	player.vendorRevision = 7
	player.vendorDirty = false
	h.sim.mu.Unlock()

	if _, err := player.InteractNPC(protocol.NpcInteractRequest{EntityID: r.entityID, ClientTick: 3}); err != nil {
		t.Fatalf("addressing the same carpenter again was refused: %v", err)
	}
	h.step()

	if state := newestVendorState(t, out); state.Revision != 7 {
		t.Errorf("re-addressing the open stall left it at revision %d, want the 7 it already had", state.Revision)
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
