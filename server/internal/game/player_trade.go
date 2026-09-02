package game

import (
	"errors"
	"fmt"
	"math"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

type playerTradePair struct {
	first  uint64
	second uint64
}

type playerTrade struct {
	players   [2]*Player
	revision  uint32
	offers    [2][protocol.PlayerTradeSlots]playerTradeOffer
	silver    [2]uint32
	confirmed [2]bool
	dirty     [2]bool
}

func orderedPlayerTradePair(a, b uint64) playerTradePair {
	if a > b {
		a, b = b, a
	}
	return playerTradePair{first: a, second: b}
}

func (t *playerTrade) side(p *Player) int {
	if t != nil && t.players[0] == p {
		return 0
	}
	if t != nil && t.players[1] == p {
		return 1
	}
	return -1
}

func (t *playerTrade) changed() {
	t.revision++
	if t.revision == 0 {
		// A zero revision is not representable by PlayerTradeState. Reaching this
		// requires more than four billion accepted changes in one live session; keep
		// the state valid even then instead of publishing an absent revision.
		t.revision = 1
	}
	t.confirmed = [2]bool{}
	t.dirty = [2]bool{true, true}
}

func (t *playerTrade) clearConfirmations() {
	t.confirmed = [2]bool{}
	t.dirty = [2]bool{true, true}
}

func (p *Player) PlayerTrade(req protocol.PlayerTradeRequest) (vnet.RefusalReason, error) {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()

	if !p.sim.onlineLocked(p) {
		return vnet.RefusalReasonNoSuchPlayer, errors.New("the requesting player is no longer online")
	}
	if err := p.cannotActLocked(); err != nil {
		if p.trade != nil {
			p.closePlayerTradeLocked(vnet.PlayerTradeCloseReasonDied)
		}
		return vnet.RefusalReasonPlayerIsDead, err
	}
	if p.havePlayerTradeTick && !newerTick(req.ClientTick, p.lastPlayerTradeTick) {
		return vnet.RefusalReasonUnknown, fmt.Errorf("stale player-trade client tick %d; newest is %d", req.ClientTick, p.lastPlayerTradeTick)
	}
	p.havePlayerTradeTick, p.lastPlayerTradeTick = true, req.ClientTick

	if req.Action == vnet.PlayerTradeActionOpen {
		return p.openPlayerTradeLocked(req.TargetEntityID)
	}
	if p.trade == nil {
		return vnet.RefusalReasonTradeNotOpen, errors.New("no player trade is open")
	}
	trade := p.trade
	if reason, reviewed := p.reviewPlayerTradeLocked(trade); reason != vnet.PlayerTradeCloseReasonUnknown {
		return vnet.RefusalReasonTradeNotOpen, fmt.Errorf("the player trade closed with %s", reason)
	} else if !reviewed {
		return vnet.RefusalReasonInventoryBusy, errors.New("one participant's inventory is busy")
	}
	if p.trade != trade {
		return vnet.RefusalReasonTradeNotOpen, errors.New("the player trade is no longer open")
	}
	side := trade.side(p)
	if side < 0 {
		return vnet.RefusalReasonTradeNotOpen, errors.New("the player is not a participant in this trade")
	}
	if req.Action == vnet.PlayerTradeActionCancel {
		p.closePlayerTradeLocked(vnet.PlayerTradeCloseReasonCancelled)
		return vnet.RefusalReasonUnknown, nil
	}
	if req.Revision != trade.revision {
		trade.dirty[side] = true
		return vnet.RefusalReasonStaleRevision, fmt.Errorf("player-trade revision %d is not current revision %d", req.Revision, trade.revision)
	}

	switch req.Action {
	case vnet.PlayerTradeActionSetItem:
		return p.setPlayerTradeItemLocked(trade, side, req.TradeSlot, req.PackSlot)
	case vnet.PlayerTradeActionClearItem:
		if int(req.TradeSlot) >= protocol.PlayerTradeSlots {
			return vnet.RefusalReasonMalformedSlot, fmt.Errorf("trade slot %d is outside the offer", req.TradeSlot)
		}
		trade.offers[side][req.TradeSlot] = playerTradeOffer{}
		trade.changed()
		return vnet.RefusalReasonUnknown, nil
	case vnet.PlayerTradeActionSetSilver:
		if !p.inventory.mu.TryLock() {
			return vnet.RefusalReasonInventoryBusy, errors.New("the inventory is busy")
		}
		purse := p.inventory.silver
		p.inventory.mu.Unlock()
		if req.Silver > purse {
			return vnet.RefusalReasonNotEnoughSilver, fmt.Errorf("%d silver is more than the purse holds", req.Silver)
		}
		trade.silver[side] = req.Silver
		trade.changed()
		return vnet.RefusalReasonUnknown, nil
	case vnet.PlayerTradeActionConfirm:
		return p.confirmPlayerTradeLocked(trade, side)
	default:
		return vnet.RefusalReasonUnknown, fmt.Errorf("player-trade action %d is unknown", req.Action)
	}
}

func (p *Player) openPlayerTradeLocked(targetID uint64) (vnet.RefusalReason, error) {
	if p.trade != nil {
		return vnet.RefusalReasonAlreadyTrading, errors.New("the player already has a trade open")
	}
	target := p.sim.players[targetID]
	if target == nil || target == p || !p.sim.onlineLocked(target) || target.cannotActLocked() != nil ||
		target.trade != nil || !playersWithinTradeReach(p, target) {
		// Every fact about an addressed target has one answer. In particular, dead,
		// busy and out-of-range players are indistinguishable from absent ids.
		return vnet.RefusalReasonNoSuchPlayer, fmt.Errorf("entity %d is not an available trade partner", targetID)
	}
	pair := orderedPlayerTradePair(p.characterID, target.characterID)
	for held, expiry := range p.sim.tradeCooldowns {
		if p.sim.currentTick >= expiry {
			delete(p.sim.tradeCooldowns, held)
		}
	}
	if expiry, cooling := p.sim.tradeCooldowns[pair]; cooling {
		return vnet.RefusalReasonTradeCooldown, fmt.Errorf("this pair cannot trade again before tick %d", expiry)
	}

	trade := &playerTrade{
		players:  [2]*Player{p, target},
		revision: 1,
		dirty:    [2]bool{true, true},
	}
	p.trade, target.trade = trade, trade
	return vnet.RefusalReasonUnknown, nil
}

func playersWithinTradeReach(a, b *Player) bool {
	if a == nil || b == nil {
		return false
	}
	distance := boxDistance(a.box(), b.box())
	return !math.IsNaN(distance) && distance <= TradeReach
}

func (p *Player) setPlayerTradeItemLocked(trade *playerTrade, side int, tradeSlot, packSlot uint8) (vnet.RefusalReason, error) {
	if int(tradeSlot) >= protocol.PlayerTradeSlots || int(packSlot) >= int(protocol.InventorySlots) {
		return vnet.RefusalReasonMalformedSlot, fmt.Errorf("trade slot %d or pack slot %d is outside its inventory", tradeSlot, packSlot)
	}
	if int(packSlot) >= equipmentFirst {
		return vnet.RefusalReasonNothingToOffer, fmt.Errorf("pack slot %d is an equipment slot", packSlot)
	}
	if trade.offers[side][tradeSlot].stack.count != 0 {
		return vnet.RefusalReasonTradeSlotTaken, fmt.Errorf("trade slot %d is already occupied", tradeSlot)
	}
	for _, offer := range trade.offers[side] {
		if offer.stack.count != 0 && offer.packSlot == packSlot {
			return vnet.RefusalReasonTradeSlotTaken, fmt.Errorf("pack slot %d is already offered", packSlot)
		}
	}
	if !p.inventory.mu.TryLock() {
		return vnet.RefusalReasonInventoryBusy, errors.New("the inventory is busy")
	}
	stack := p.inventory.slots[packSlot]
	p.inventory.mu.Unlock()
	if stack.count == 0 {
		return vnet.RefusalReasonNothingToOffer, fmt.Errorf("pack slot %d is empty", packSlot)
	}
	trade.offers[side][tradeSlot] = playerTradeOffer{packSlot: packSlot, stack: stack}
	trade.changed()
	return vnet.RefusalReasonUnknown, nil
}

func (p *Player) confirmPlayerTradeLocked(trade *playerTrade, side int) (vnet.RefusalReason, error) {
	trade.confirmed[side] = true
	trade.dirty = [2]bool{true, true}
	if !trade.confirmed[1-side] {
		return vnet.RefusalReasonUnknown, nil
	}

	result := settlePlayerTradeLocked(
		playerTradeSide{player: trade.players[0], offers: trade.offers[0], silver: trade.silver[0]},
		playerTradeSide{player: trade.players[1], offers: trade.offers[1], silver: trade.silver[1]},
	)
	switch result {
	case playerTradeSettlementComplete:
		p.closePlayerTradeLocked(vnet.PlayerTradeCloseReasonCompleted)
		return vnet.RefusalReasonUnknown, nil
	case playerTradeSettlementBusy:
		trade.clearConfirmations()
		return vnet.RefusalReasonInventoryBusy, errors.New("one participant's inventory is busy")
	case playerTradeSettlementChanged:
		trade.clearConfirmations()
		// The copy-and-lock settlement caught a mutation between the request review and
		// the commit. Withdraw its now-stale snapshot before the next state is sent.
		before := trade.revision
		p.reviewPlayerTradeOffersLocked(trade)
		if trade.revision == before {
			// A concurrent inventory holder may still prevent the second inspection.
			// Withdrawing everything is conservative, preserves every asset, and ensures
			// the StaleRevision answer names a revision that really did advance.
			trade.offers = [2][protocol.PlayerTradeSlots]playerTradeOffer{}
			trade.silver = [2]uint32{}
			trade.changed()
		}
		return vnet.RefusalReasonStaleRevision, errors.New("an offered slot or purse changed before settlement")
	case playerTradeSettlementFull:
		trade.clearConfirmations()
		trade.players[1-side].queuePlayerTradeRefusalLocked(vnet.RefusalReasonInventoryFull)
		return vnet.RefusalReasonInventoryFull, errors.New("one participant cannot receive the complete trade")
	default:
		trade.clearConfirmations()
		return vnet.RefusalReasonUnknown, errors.New("the player-trade settlement returned an unknown result")
	}
}

// reviewPlayerTradeLocked is the one live-session predicate used by requests and the
// tick. It closes first for participant state and distance, then withdraws any asset
// snapshot that no longer names exactly what its owner holds.
func (p *Player) reviewPlayerTradeLocked(trade *playerTrade) (vnet.PlayerTradeCloseReason, bool) {
	if trade == nil || trade.side(p) < 0 {
		return vnet.PlayerTradeCloseReasonUnknown, true
	}
	a, b := trade.players[0], trade.players[1]
	if !p.sim.onlineLocked(a) || !p.sim.onlineLocked(b) {
		p.closePlayerTradeLocked(vnet.PlayerTradeCloseReasonDisconnected)
		return vnet.PlayerTradeCloseReasonDisconnected, true
	}
	if !a.alive() || !b.alive() {
		p.closePlayerTradeLocked(vnet.PlayerTradeCloseReasonDied)
		return vnet.PlayerTradeCloseReasonDied, true
	}
	if !playersWithinTradeReach(a, b) {
		p.closePlayerTradeLocked(vnet.PlayerTradeCloseReasonOutOfReach)
		return vnet.PlayerTradeCloseReasonOutOfReach, true
	}
	return vnet.PlayerTradeCloseReasonUnknown, p.reviewPlayerTradeOffersLocked(trade)
}

func (p *Player) reviewPlayerTradeOffersLocked(trade *playerTrade) bool {
	first, second := trade.players[0], trade.players[1]
	if first.entityID > second.entityID {
		first, second = second, first
	}
	if !first.inventory.mu.TryLock() {
		return false
	}
	defer first.inventory.mu.Unlock()
	if !second.inventory.mu.TryLock() {
		return false
	}
	defer second.inventory.mu.Unlock()

	changed := false
	for side, player := range trade.players {
		for slot, offer := range trade.offers[side] {
			if offer.stack.count != 0 && (int(offer.packSlot) >= equipmentFirst || player.inventory.slots[offer.packSlot] != offer.stack) {
				trade.offers[side][slot] = playerTradeOffer{}
				changed = true
			}
		}
		if trade.silver[side] > player.inventory.silver {
			trade.silver[side] = 0
			changed = true
		}
	}
	if changed {
		trade.changed()
	}
	return true
}

func (p *Player) closePlayerTradeLocked(reason vnet.PlayerTradeCloseReason) {
	trade := p.trade
	if trade == nil {
		return
	}
	if reason == vnet.PlayerTradeCloseReasonCancelled {
		pair := orderedPlayerTradePair(trade.players[0].characterID, trade.players[1].characterID)
		p.sim.tradeCooldowns[pair] = p.sim.currentTick + p.sim.tradeReopenTicks
	}
	for index, player := range trade.players {
		if player == nil || player.trade != trade {
			continue
		}
		partner := trade.players[1-index]
		if partner != nil {
			player.queuePlayerTradeClosedLocked(protocol.PlayerTradeClosed{
				PartnerEntityID: partner.entityID,
				Reason:          reason,
			})
		}
		player.trade = nil
	}
	trade.dirty = [2]bool{}
}

func (p *Player) queuePlayerTradeClosedLocked(closed protocol.PlayerTradeClosed) {
	for _, queued := range p.playerTradeClosures {
		if queued == closed {
			return
		}
	}
	p.playerTradeClosures = append(p.playerTradeClosures, closed)
}

func (p *Player) queuePlayerTradeRefusalLocked(reason vnet.RefusalReason) {
	for _, queued := range p.playerTradeRefusals {
		if queued == reason {
			return
		}
	}
	p.playerTradeRefusals = append(p.playerTradeRefusals, reason)
}

func (p *Player) offerPlayerTradeLocked() {
	if p.trade != nil {
		_, _ = p.reviewPlayerTradeLocked(p.trade)
	}
	for len(p.playerTradeClosures) > 0 {
		closed := p.playerTradeClosures[0]
		if !p.deliver(protocol.EncodePlayerTradeClosed(closed)) {
			return
		}
		p.playerTradeClosures = p.playerTradeClosures[1:]
	}
	for len(p.playerTradeRefusals) > 0 {
		reason := p.playerTradeRefusals[0]
		frame := protocol.EncodeActionRefused(protocol.ActionRefused{
			Action: vnet.RefusedActionPlayerTrade,
			Reason: reason,
		})
		if !p.deliver(frame) {
			return
		}
		p.playerTradeRefusals = p.playerTradeRefusals[1:]
	}
	trade := p.trade
	if trade == nil {
		return
	}
	side := trade.side(p)
	if side < 0 || !trade.dirty[side] {
		return
	}
	if p.deliver(protocol.EncodePlayerTradeState(playerTradeStateFor(trade, side))) {
		trade.dirty[side] = false
	}
}

func playerTradeStateFor(trade *playerTrade, side int) protocol.PlayerTradeState {
	partner := trade.players[1-side]
	return protocol.PlayerTradeState{
		PartnerEntityID: partner.entityID,
		PartnerName:     partner.name,
		Revision:        trade.revision,
		MyOffer:         projectPlayerTradeOffers(trade.offers[side], true),
		TheirOffer:      projectPlayerTradeOffers(trade.offers[1-side], false),
		MySilver:        trade.silver[side],
		TheirSilver:     trade.silver[1-side],
		MyConfirmed:     trade.confirmed[side],
		TheirConfirmed:  trade.confirmed[1-side],
	}
}

func projectPlayerTradeOffers(offers [protocol.PlayerTradeSlots]playerTradeOffer, own bool) []protocol.PlayerTradeSlot {
	projected := make([]protocol.PlayerTradeSlot, 0, protocol.PlayerTradeSlots)
	for tradeSlot, offer := range offers {
		if offer.stack.count == 0 {
			continue
		}
		packSlot := uint8(0)
		if own {
			packSlot = offer.packSlot
		}
		projected = append(projected, protocol.PlayerTradeSlot{
			TradeSlot:     uint8(tradeSlot),
			PackSlot:      packSlot,
			ItemID:        uint16(offer.stack.item),
			Count:         offer.stack.count,
			Durability:    offer.stack.durability,
			MaxDurability: offer.stack.maxDurability,
		})
	}
	return projected
}
