package game

import (
	"math/rand/v2"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

type playerTradeHarness struct {
	*vitalsHarness
	players [2]*Player
	outs    [2]*dropSink
	next    map[*Player]uint32
}

func newPlayerTradeHarness(t *testing.T) *playerTradeHarness {
	t.Helper()
	h := &playerTradeHarness{
		vitalsHarness: newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63}),
		next:          make(map[*Player]uint32),
	}
	for index, name := range []string{"Astrid", "Bjorn"} {
		entityID := uint64(index + 1)
		out := &dropSink{}
		player, err := h.sim.JoinCharacter(
			entityID, testPlayerID(entityID), 100+entityID, name,
			[3]float32{float32(index) + 0.5, 64, 0.5}, testAppearance(), nil, out.deliver,
		)
		if err != nil {
			t.Fatalf("JoinCharacter(%s): %v", name, err)
		}
		h.players[index], h.outs[index] = player, out
		h.next[player] = 1
	}
	return h
}

func (h *playerTradeHarness) request(p *Player, req protocol.PlayerTradeRequest) (vnet.RefusalReason, error) {
	h.t.Helper()
	req.ClientTick = h.next[p]
	h.next[p]++
	return p.PlayerTrade(req)
}

func (h *playerTradeHarness) mustAccept(p *Player, req protocol.PlayerTradeRequest) {
	h.t.Helper()
	if reason, err := h.request(p, req); err != nil {
		h.t.Fatalf("%s was refused %s: %v", req.Action, reason, err)
	}
}

func (h *playerTradeHarness) open() {
	h.t.Helper()
	h.mustAccept(h.players[0], protocol.PlayerTradeRequest{
		Action: vnet.PlayerTradeActionOpen, TargetEntityID: h.players[1].entityID,
	})
}

func (h *playerTradeHarness) revision() uint32 {
	h.t.Helper()
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	if h.players[0].trade == nil {
		h.t.Fatal("the player trade is not open")
	}
	return h.players[0].trade.revision
}

func (h *playerTradeHarness) setItem(side int, tradeSlot, packSlot uint8) {
	h.t.Helper()
	h.mustAccept(h.players[side], protocol.PlayerTradeRequest{
		Action: vnet.PlayerTradeActionSetItem, TradeSlot: tradeSlot,
		PackSlot: packSlot, Revision: h.revision(),
	})
}

func (h *playerTradeHarness) setSilver(side int, silver uint32) {
	h.t.Helper()
	h.mustAccept(h.players[side], protocol.PlayerTradeRequest{
		Action: vnet.PlayerTradeActionSetSilver, Silver: silver, Revision: h.revision(),
	})
}

func (h *playerTradeHarness) confirm(side int) (vnet.RefusalReason, error) {
	h.t.Helper()
	return h.request(h.players[side], protocol.PlayerTradeRequest{
		Action: vnet.PlayerTradeActionConfirm, Revision: h.revision(),
	})
}

func playerTradeMessages(t *testing.T, out *dropSink) []protocol.Message {
	t.Helper()
	var messages []protocol.Message
	for _, frame := range out.all() {
		message, err := protocol.Decode(frame)
		if err != nil {
			t.Fatalf("Decode delivered frame: %v", err)
		}
		if message.PlayerTradeState != nil || message.PlayerTradeClosed != nil ||
			(message.ActionRefused != nil && message.ActionRefused.Action == vnet.RefusedActionPlayerTrade) {
			messages = append(messages, message)
		}
	}
	return messages
}

func newestPlayerTradeState(t *testing.T, out *dropSink) protocol.PlayerTradeState {
	t.Helper()
	messages := playerTradeMessages(t, out)
	for index := len(messages) - 1; index >= 0; index-- {
		if messages[index].PlayerTradeState != nil {
			return *messages[index].PlayerTradeState
		}
	}
	t.Fatal("no PlayerTradeState was delivered")
	return protocol.PlayerTradeState{}
}

func TestPlayerTradeOpenPublishesRevisionOneToBothPlayers(t *testing.T) {
	h := newPlayerTradeHarness(t)
	h.open()
	h.step()

	for side := range 2 {
		state := newestPlayerTradeState(t, h.outs[side])
		partner := h.players[1-side]
		if state.PartnerEntityID != partner.entityID || state.PartnerName != partner.name || state.Revision != 1 {
			t.Errorf("side %d state identity = %+v, want entity %d named %q at revision 1", side, state, partner.entityID, partner.name)
		}
		if len(state.MyOffer) != 0 || len(state.TheirOffer) != 0 || state.MySilver != 0 || state.TheirSilver != 0 || state.MyConfirmed || state.TheirConfirmed {
			t.Errorf("side %d initial state = %+v, want empty unconfirmed offers", side, state)
		}
	}
}

func TestPlayerTradeOpenHidesEveryUnavailableTargetState(t *testing.T) {
	tests := []struct {
		name    string
		arrange func(*playerTradeHarness) uint64
	}{
		{name: "absent", arrange: func(*playerTradeHarness) uint64 { return 99 }},
		{name: "self", arrange: func(h *playerTradeHarness) uint64 { return h.players[0].entityID }},
		{name: "out of reach", arrange: func(h *playerTradeHarness) uint64 {
			h.standAt(h.players[1], [3]float64{TradeReach + 10, 64, 0.5})
			return h.players[1].entityID
		}},
		{name: "dead", arrange: func(h *playerTradeHarness) uint64 {
			h.sim.mu.Lock()
			h.players[1].lifeState = vnet.LifeStateDead
			h.sim.mu.Unlock()
			return h.players[1].entityID
		}},
		{name: "already trading", arrange: func(h *playerTradeHarness) uint64 {
			third, err := h.sim.JoinCharacter(3, testPlayerID(3), 103, "Cora", [3]float32{2.5, 64, 0.5}, testAppearance(), nil, func([]byte) bool { return true })
			if err != nil {
				h.t.Fatalf("JoinCharacter(Cora): %v", err)
			}
			if reason, openErr := third.PlayerTrade(protocol.PlayerTradeRequest{Action: vnet.PlayerTradeActionOpen, TargetEntityID: h.players[1].entityID, ClientTick: 1}); openErr != nil {
				h.t.Fatalf("opening target's other trade was refused %s: %v", reason, openErr)
			}
			return h.players[1].entityID
		}},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			h := newPlayerTradeHarness(t)
			target := test.arrange(h)
			reason, err := h.request(h.players[0], protocol.PlayerTradeRequest{Action: vnet.PlayerTradeActionOpen, TargetEntityID: target})
			if err == nil || reason != vnet.RefusalReasonNoSuchPlayer {
				t.Fatalf("Open = %s, %v; want NoSuchPlayer", reason, err)
			}
		})
	}
}

func TestPlayerTradeOpenReportsOnlyTheSendersOwnState(t *testing.T) {
	t.Run("dead", func(t *testing.T) {
		h := newPlayerTradeHarness(t)
		h.sim.mu.Lock()
		h.players[0].lifeState = vnet.LifeStateDead
		h.sim.mu.Unlock()
		reason, err := h.request(h.players[0], protocol.PlayerTradeRequest{Action: vnet.PlayerTradeActionOpen, TargetEntityID: h.players[1].entityID})
		if err == nil || reason != vnet.RefusalReasonPlayerIsDead {
			t.Fatalf("Open = %s, %v; want PlayerIsDead", reason, err)
		}
	})

	t.Run("already trading", func(t *testing.T) {
		h := newPlayerTradeHarness(t)
		h.open()
		reason, err := h.request(h.players[0], protocol.PlayerTradeRequest{Action: vnet.PlayerTradeActionOpen, TargetEntityID: h.players[1].entityID})
		if err == nil || reason != vnet.RefusalReasonAlreadyTrading {
			t.Fatalf("Open = %s, %v; want AlreadyTrading", reason, err)
		}
	})
}

func TestCancelledPlayerTradeEnforcesTheStablePairCooldown(t *testing.T) {
	h := newPlayerTradeHarness(t)
	h.open()
	h.mustAccept(h.players[1], protocol.PlayerTradeRequest{Action: vnet.PlayerTradeActionCancel})

	reason, err := h.request(h.players[0], protocol.PlayerTradeRequest{Action: vnet.PlayerTradeActionOpen, TargetEntityID: h.players[1].entityID})
	if err == nil || reason != vnet.RefusalReasonTradeCooldown {
		t.Fatalf("immediate reopen = %s, %v; want TradeCooldown", reason, err)
	}
	h.advance(int(h.sim.tradeReopenTicks))
	h.mustAccept(h.players[0], protocol.PlayerTradeRequest{Action: vnet.PlayerTradeActionOpen, TargetEntityID: h.players[1].entityID})
	if len(h.sim.tradeCooldowns) != 0 {
		t.Fatalf("expired cooldowns remain: %+v", h.sim.tradeCooldowns)
	}
}

func TestPlayerTradeOffersSnapshotSlotsAndResetBothConfirmations(t *testing.T) {
	h := newPlayerTradeHarness(t)
	h.stock(h.players[0], 4, ItemIronSword, 1)
	h.fund(h.players[1], 50)
	h.open()

	h.setItem(0, 3, 4)
	trade := h.players[0].trade
	offered := trade.offers[0][3]
	if offered.packSlot != 4 || offered.stack != stackOf(ItemIronSword, 1) || trade.revision != 2 {
		t.Fatalf("offered snapshot = %+v at revision %d", offered, trade.revision)
	}
	if reason, err := h.confirm(0); err != nil {
		t.Fatalf("Confirm = %s, %v", reason, err)
	}
	if !trade.confirmed[0] {
		t.Fatal("first confirmation was not recorded")
	}
	h.setSilver(1, 20)
	if trade.revision != 3 || trade.confirmed != [2]bool{} || trade.silver[1] != 20 {
		t.Fatalf("silver change left trade %+v", trade)
	}

	h.step()
	left := newestPlayerTradeState(t, h.outs[0])
	right := newestPlayerTradeState(t, h.outs[1])
	if len(left.MyOffer) != 1 || left.MyOffer[0].PackSlot != 4 || left.MyOffer[0].Durability != IronSwordMaxDurability {
		t.Errorf("owner projection = %+v", left.MyOffer)
	}
	if len(right.TheirOffer) != 1 || right.TheirOffer[0].PackSlot != 0 || right.TheirOffer[0].Durability != IronSwordMaxDurability {
		t.Errorf("partner projection = %+v", right.TheirOffer)
	}
}

func TestPlayerTradeOfferRefusalsChangeNothing(t *testing.T) {
	h := newPlayerTradeHarness(t)
	h.stock(h.players[0], 3, ItemRawIron, 4)
	h.stock(h.players[0], uint8(equipmentHead), ItemIronHelm, 1)
	h.open()
	h.setItem(0, 0, 3)
	revision := h.revision()

	tests := []struct {
		name string
		req  protocol.PlayerTradeRequest
		want vnet.RefusalReason
	}{
		{name: "trade slot occupied", req: protocol.PlayerTradeRequest{Action: vnet.PlayerTradeActionSetItem, TradeSlot: 0, PackSlot: 3}, want: vnet.RefusalReasonTradeSlotTaken},
		{name: "pack slot repeated", req: protocol.PlayerTradeRequest{Action: vnet.PlayerTradeActionSetItem, TradeSlot: 1, PackSlot: 3}, want: vnet.RefusalReasonTradeSlotTaken},
		{name: "empty pack slot", req: protocol.PlayerTradeRequest{Action: vnet.PlayerTradeActionSetItem, TradeSlot: 1, PackSlot: 4}, want: vnet.RefusalReasonNothingToOffer},
		{name: "equipment", req: protocol.PlayerTradeRequest{Action: vnet.PlayerTradeActionSetItem, TradeSlot: 1, PackSlot: uint8(equipmentHead)}, want: vnet.RefusalReasonNothingToOffer},
		{name: "too much silver", req: protocol.PlayerTradeRequest{Action: vnet.PlayerTradeActionSetSilver, Silver: 1}, want: vnet.RefusalReasonNotEnoughSilver},
		{name: "bad trade slot", req: protocol.PlayerTradeRequest{Action: vnet.PlayerTradeActionClearItem, TradeSlot: protocol.PlayerTradeSlots}, want: vnet.RefusalReasonMalformedSlot},
		{name: "stale revision", req: protocol.PlayerTradeRequest{Action: vnet.PlayerTradeActionClearItem, TradeSlot: 0, Revision: revision - 1}, want: vnet.RefusalReasonStaleRevision},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			if test.name == "stale revision" {
				h.players[0].trade.dirty[0] = false
			}
			if test.req.Revision == 0 {
				test.req.Revision = revision
			}
			reason, err := h.request(h.players[0], test.req)
			if err == nil || reason != test.want {
				t.Fatalf("request = %s, %v; want %s", reason, err, test.want)
			}
			if got := h.revision(); got != revision {
				t.Fatalf("revision = %d, want unchanged %d", got, revision)
			}
			if test.name == "stale revision" && !h.players[0].trade.dirty[0] {
				t.Fatal("stale request did not queue the current state for resynchronization")
			}
		})
	}
}

func TestPlayerTradeMovesBothOffersAndPursesOnSecondConfirmation(t *testing.T) {
	h := newPlayerTradeHarness(t)
	leftSword := stackOf(ItemIronSword, 1)
	leftSword.durability = 73
	h.players[0].inventory.mu.Lock()
	h.players[0].inventory.slots = slotTable{}
	h.players[0].inventory.slots[7] = leftSword
	h.players[0].inventory.silver = 80
	h.players[0].inventory.mu.Unlock()
	h.players[1].inventory.mu.Lock()
	h.players[1].inventory.slots = slotTable{}
	h.players[1].inventory.slots[9] = stackOf(ItemRawIron, 6)
	h.players[1].inventory.silver = 30
	h.players[1].inventory.mu.Unlock()

	h.open()
	h.setItem(0, 0, 7)
	h.setItem(1, 0, 9)
	h.setSilver(0, 25)
	h.setSilver(1, 10)
	if reason, err := h.confirm(0); err != nil {
		t.Fatalf("first Confirm = %s, %v", reason, err)
	}
	if reason, err := h.confirm(1); err != nil {
		t.Fatalf("second Confirm = %s, %v", reason, err)
	}
	if h.players[0].trade != nil || h.players[1].trade != nil {
		t.Fatal("completed trade remained attached to a participant")
	}
	if h.carrying(h.players[0], ItemRawIron) != 6 || h.carrying(h.players[1], ItemIronSword) != 1 {
		t.Fatalf("completed packs hold iron=%d sword=%d", h.carrying(h.players[0], ItemRawIron), h.carrying(h.players[1], ItemIronSword))
	}
	if h.purse(h.players[0]) != 65 || h.purse(h.players[1]) != 45 {
		t.Fatalf("completed purses = %d and %d", h.purse(h.players[0]), h.purse(h.players[1]))
	}
	h.players[1].inventory.mu.Lock()
	gotSword := h.players[1].inventory.slots[0]
	h.players[1].inventory.mu.Unlock()
	if gotSword != leftSword {
		t.Errorf("durable stack = %+v, want %+v", gotSword, leftSword)
	}

	h.step()
	for side := range 2 {
		messages := playerTradeMessages(t, h.outs[side])
		found := false
		for _, message := range messages {
			if message.PlayerTradeClosed != nil && message.PlayerTradeClosed.Reason == vnet.PlayerTradeCloseReasonCompleted {
				found = true
			}
		}
		if !found {
			t.Errorf("side %d received no Completed closure", side)
		}
		if len(h.outs[side].inventoryStates(t)) == 0 {
			t.Errorf("side %d received no fresh inventory", side)
		}
	}
}

func TestPlayerTradeFullReceiverMovesNothingAndNotifiesBoth(t *testing.T) {
	h := newPlayerTradeHarness(t)
	for _, player := range h.players {
		player.inventory.mu.Lock()
		for slot := range equipmentFirst {
			player.inventory.slots[slot] = stackOf(ItemStone, 1)
		}
		player.inventory.mu.Unlock()
	}
	h.stock(h.players[0], 0, ItemRawIron, 1)
	h.stock(h.players[0], 1, ItemRawCoal, 1)
	h.stock(h.players[1], 0, ItemLog, 1)
	before := [2]slotTable{}
	for side, player := range h.players {
		player.inventory.mu.Lock()
		before[side] = player.inventory.slots
		player.inventory.mu.Unlock()
	}

	h.open()
	h.setItem(0, 0, 0)
	h.setItem(0, 1, 1)
	h.setItem(1, 0, 0)
	_, _ = h.confirm(0)
	reason, err := h.confirm(1)
	if err == nil || reason != vnet.RefusalReasonInventoryFull {
		t.Fatalf("second Confirm = %s, %v; want InventoryFull", reason, err)
	}
	trade := h.players[0].trade
	if trade == nil || trade.confirmed != [2]bool{} {
		t.Fatalf("failed trade confirmations = %+v", trade)
	}
	for side, player := range h.players {
		player.inventory.mu.Lock()
		got := player.inventory.slots
		player.inventory.mu.Unlock()
		if got != before[side] {
			t.Errorf("side %d inventory changed on failed settlement", side)
		}
	}
	h.step()
	partnerNotified := false
	for _, message := range playerTradeMessages(t, h.outs[0]) {
		if message.ActionRefused != nil && message.ActionRefused.Reason == vnet.RefusalReasonInventoryFull {
			partnerNotified = true
		}
	}
	if !partnerNotified {
		t.Fatal("the first confirmer was not notified that the complete trade did not fit")
	}
}

func TestPlayerTradeTickWithdrawsChangedSlotAndClearsConfirmations(t *testing.T) {
	h := newPlayerTradeHarness(t)
	h.stock(h.players[0], 5, ItemRawIron, 8)
	h.open()
	h.setItem(0, 2, 5)
	_, _ = h.confirm(1)
	before := h.revision()

	if _, err := h.players[0].MoveInventory(protocol.InventoryMoveRequest{From: 5, To: 6, Count: 8}); err != nil {
		t.Fatalf("MoveInventory: %v", err)
	}
	h.step()
	trade := h.players[0].trade
	if trade == nil || trade.revision != before+1 || trade.offers[0][2].stack.count != 0 || trade.confirmed != [2]bool{} {
		t.Fatalf("reviewed trade = %+v", trade)
	}
	for side := range 2 {
		state := newestPlayerTradeState(t, h.outs[side])
		if state.Revision != before+1 || len(state.MyOffer)+len(state.TheirOffer) != 0 || state.MyConfirmed || state.TheirConfirmed {
			t.Errorf("side %d reviewed state = %+v", side, state)
		}
	}
}

func TestPlayerTradeLifecycleClosesBothSidesWithTheAuthoritativeReason(t *testing.T) {
	tests := []struct {
		name   string
		reason vnet.PlayerTradeCloseReason
		close  func(*playerTradeHarness)
	}{
		{name: "out of reach", reason: vnet.PlayerTradeCloseReasonOutOfReach, close: func(h *playerTradeHarness) {
			h.standAt(h.players[1], [3]float64{TradeReach + 10, 64, 0.5})
			h.step()
		}},
		{name: "death", reason: vnet.PlayerTradeCloseReasonDied, close: func(h *playerTradeHarness) {
			h.sim.mu.Lock()
			h.players[1].lifeState = vnet.LifeStateDead
			h.players[1].respawnTicks = h.sim.deathTicks
			h.sim.mu.Unlock()
			h.step()
		}},
		{name: "disconnect", reason: vnet.PlayerTradeCloseReasonDisconnected, close: func(h *playerTradeHarness) {
			h.sim.Leave(h.players[1])
			h.step()
		}},
		{name: "cancel", reason: vnet.PlayerTradeCloseReasonCancelled, close: func(h *playerTradeHarness) {
			h.mustAccept(h.players[0], protocol.PlayerTradeRequest{Action: vnet.PlayerTradeActionCancel})
			h.step()
		}},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			h := newPlayerTradeHarness(t)
			h.open()
			test.close(h)
			if h.players[0].trade != nil || h.players[1].trade != nil {
				t.Fatal("closed session remained attached")
			}
			for side := range 2 {
				// A disconnected session is no longer stepped, but its closure remains
				// queued until teardown. The online partner must always receive theirs.
				if test.reason == vnet.PlayerTradeCloseReasonDisconnected && side == 1 {
					continue
				}
				found := false
				for _, message := range playerTradeMessages(t, h.outs[side]) {
					if message.PlayerTradeClosed != nil && message.PlayerTradeClosed.Reason == test.reason {
						found = true
					}
				}
				if !found {
					t.Errorf("side %d received no %s closure", side, test.reason)
				}
			}
		})
	}
}

func TestPlayerTradeRandomSequencesConserveAssetsAndNeverAliasAnOffer(t *testing.T) {
	h := newPlayerTradeHarness(t)
	for side, player := range h.players {
		player.inventory.mu.Lock()
		player.inventory.slots = slotTable{}
		player.inventory.slots[0] = stackOf(ItemStone, uint16(12+side))
		player.inventory.slots[1] = stackOf(ItemRawIron, uint16(5+side))
		player.inventory.slots[2] = stackOf(ItemLog, uint16(3+side))
		player.inventory.silver = uint32(100 + side*20)
		player.inventory.mu.Unlock()
	}
	wantItems, wantSilver := playerTradeAssetTotals(h)
	rng := rand.New(rand.NewPCG(749, 2))

	for step := range 300 {
		if h.players[0].trade == nil {
			_, _ = h.request(h.players[0], protocol.PlayerTradeRequest{Action: vnet.PlayerTradeActionOpen, TargetEntityID: h.players[1].entityID})
		} else {
			side := int(rng.Uint32() % 2)
			player := h.players[side]
			revision := h.revision()
			switch rng.Uint32() % 6 {
			case 0:
				_, _ = h.request(player, protocol.PlayerTradeRequest{
					Action: vnet.PlayerTradeActionSetItem, TradeSlot: uint8(rng.Uint32() % protocol.PlayerTradeSlots),
					PackSlot: uint8(rng.Uint32() % uint32(equipmentFirst)), Revision: revision,
				})
			case 1:
				_, _ = h.request(player, protocol.PlayerTradeRequest{
					Action: vnet.PlayerTradeActionClearItem, TradeSlot: uint8(rng.Uint32() % protocol.PlayerTradeSlots), Revision: revision,
				})
			case 2:
				_, _ = h.request(player, protocol.PlayerTradeRequest{Action: vnet.PlayerTradeActionSetSilver, Silver: rng.Uint32() % 130, Revision: revision})
			case 3:
				_, _ = h.request(player, protocol.PlayerTradeRequest{Action: vnet.PlayerTradeActionConfirm, Revision: revision})
			case 4:
				from, to := uint8(rng.Uint32()%uint32(equipmentFirst)), uint8(rng.Uint32()%uint32(equipmentFirst))
				_, _ = player.MoveInventory(protocol.InventoryMoveRequest{From: from, To: to, Count: uint16(rng.Uint32()%5 + 1)})
			case 5:
				slot := uint8(rng.Uint32() % uint32(equipmentFirst))
				_, _ = player.DropItem(protocol.DropItemRequest{Slot: slot, ClientTick: uint32(step + 1)})
			}
		}
		h.step()

		gotItems, gotSilver := playerTradeAssetTotals(h)
		if len(gotItems) != len(wantItems) {
			t.Fatalf("step %d item kind count = %d, want %d", step, len(gotItems), len(wantItems))
		}
		for item, count := range wantItems {
			if gotItems[item] != count {
				t.Fatalf("step %d item %d total = %d, want %d", step, item, gotItems[item], count)
			}
		}
		if gotSilver != wantSilver {
			t.Fatalf("step %d silver total = %d, want %d", step, gotSilver, wantSilver)
		}
		if trade := h.players[0].trade; trade != nil {
			for side := range 2 {
				seen := make(map[uint8]bool)
				for _, offer := range trade.offers[side] {
					if offer.stack.count == 0 {
						continue
					}
					if seen[offer.packSlot] {
						t.Fatalf("step %d side %d references pack slot %d twice", step, side, offer.packSlot)
					}
					seen[offer.packSlot] = true
				}
			}
		}
	}
}

func playerTradeAssetTotals(h *playerTradeHarness) (map[ItemID]uint32, uint64) {
	h.t.Helper()
	items := make(map[ItemID]uint32)
	var silver uint64
	for _, player := range h.players {
		player.inventory.mu.Lock()
		for _, stack := range player.inventory.slots {
			if stack.count != 0 {
				items[stack.item] += uint32(stack.count)
			}
		}
		silver += uint64(player.inventory.silver)
		player.inventory.mu.Unlock()
	}
	h.sim.mu.Lock()
	for _, drop := range h.sim.drops {
		items[drop.item] += uint32(drop.count)
	}
	h.sim.mu.Unlock()
	return items, silver
}
