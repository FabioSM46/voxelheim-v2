package game

import (
	"math"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

func tradeTransactionPlayers(t *testing.T) (*vitalsHarness, *Player, *Player) {
	t.Helper()
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	left, _ := joinPartyPlayer(t, h, 101, "Astrid", [3]float32{0.5, 64, 0.5})
	right, _ := joinPartyPlayer(t, h, 202, "Bjorn", [3]float32{1.5, 64, 0.5})
	return h, left, right
}

func fillTradePack(player *Player, stack inventoryStack) {
	player.inventory.mu.Lock()
	defer player.inventory.mu.Unlock()
	for slot := range player.inventory.slots[:equipmentFirst] {
		player.inventory.slots[slot] = stack
	}
}

func putTradeStack(player *Player, slot uint8, stack inventoryStack) {
	player.inventory.mu.Lock()
	defer player.inventory.mu.Unlock()
	player.inventory.slots[slot] = stack
}

func fundTradePurse(player *Player, silver uint32) {
	player.inventory.mu.Lock()
	defer player.inventory.mu.Unlock()
	player.inventory.silver = silver
}

func tradeInventory(player *Player) (slotTable, uint32) {
	player.inventory.mu.Lock()
	defer player.inventory.mu.Unlock()
	return player.inventory.slots, player.inventory.silver
}

func oneTradeOffer(tradeSlot, packSlot uint8, stack inventoryStack) [protocol.PlayerTradeSlots]playerTradeOffer {
	var offers [protocol.PlayerTradeSlots]playerTradeOffer
	offers[tradeSlot] = playerTradeOffer{packSlot: packSlot, stack: stack}
	return offers
}

func TestPlayerTradeSettlementRemovesBothSidesBeforeEitherInsertion(t *testing.T) {
	t.Parallel()

	h, left, right := tradeTransactionPlayers(t)
	dirt := stackOf(ItemDirt, 64)
	stone := stackOf(ItemStone, 8)
	logs := stackOf(ItemLog, 4)
	fillTradePack(left, dirt)
	fillTradePack(right, dirt)
	putTradeStack(left, 0, stone)
	putTradeStack(right, 0, logs)
	fundTradePurse(left, 100)
	fundTradePurse(right, 50)

	h.sim.mu.Lock()
	result := settlePlayerTradeLocked(
		playerTradeSide{player: left, offers: oneTradeOffer(0, 0, stone), silver: 30},
		playerTradeSide{player: right, offers: oneTradeOffer(0, 0, logs), silver: 5},
	)
	h.sim.mu.Unlock()
	if result != playerTradeSettlementComplete {
		t.Fatalf("settlement = %d, want complete", result)
	}

	leftSlots, leftSilver := tradeInventory(left)
	rightSlots, rightSilver := tradeInventory(right)
	if leftSlots[0] != logs || rightSlots[0] != stone {
		t.Fatalf("slot exchange = left %+v, right %+v; want logs and stone", leftSlots[0], rightSlots[0])
	}
	if leftSilver != 75 || rightSilver != 75 {
		t.Fatalf("silver exchange = %d + %d, want 75 + 75", leftSilver, rightSilver)
	}
	if !left.inventoryDirty || !right.inventoryDirty {
		t.Fatal("a completed exchange did not owe both players a fresh inventory state")
	}
}

func TestPlayerTradeSettlementPreservesDurabilityExactly(t *testing.T) {
	t.Parallel()

	h, left, right := tradeTransactionPlayers(t)
	blade := stackOf(ItemIronSword, 1)
	blade.durability = 17
	stone := stackOf(ItemStone, 3)
	putTradeStack(left, 7, blade)
	putTradeStack(right, 9, stone)

	h.sim.mu.Lock()
	result := settlePlayerTradeLocked(
		playerTradeSide{player: left, offers: oneTradeOffer(2, 7, blade)},
		playerTradeSide{player: right, offers: oneTradeOffer(4, 9, stone)},
	)
	h.sim.mu.Unlock()
	if result != playerTradeSettlementComplete {
		t.Fatalf("settlement = %d, want complete", result)
	}

	rightSlots, _ := tradeInventory(right)
	var arrived inventoryStack
	for _, held := range rightSlots[:equipmentFirst] {
		if held.item == ItemIronSword {
			arrived = held
			break
		}
	}
	if arrived != blade {
		t.Fatalf("arrived blade = %+v, want exact snapshot %+v", arrived, blade)
	}
}

func TestPlayerTradeSettlementMovesAResourceStackWhole(t *testing.T) {
	t.Parallel()

	h, left, right := tradeTransactionPlayers(t)
	offered := stackOf(ItemStone, 8)
	partial := stackOf(ItemStone, 60)
	putTradeStack(left, 0, offered)
	putTradeStack(right, 0, partial)

	h.sim.mu.Lock()
	result := settlePlayerTradeLocked(
		playerTradeSide{player: left, offers: oneTradeOffer(0, 0, offered)},
		playerTradeSide{player: right},
	)
	h.sim.mu.Unlock()
	if result != playerTradeSettlementComplete {
		t.Fatalf("settlement = %d, want complete", result)
	}

	rightSlots, _ := tradeInventory(right)
	if rightSlots[0] != partial || rightSlots[1] != offered {
		t.Fatalf("received slots = %+v, %+v; want the partial stack unchanged and the offered stack whole", rightSlots[0], rightSlots[1])
	}
}

func TestPlayerTradeSettlementFailureNeverCommitsOneDirection(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name  string
		alter func(left, right *Player, leftStack inventoryStack) ([protocol.PlayerTradeSlots]playerTradeOffer, [protocol.PlayerTradeSlots]playerTradeOffer)
		want  playerTradeSettlement
	}{
		{
			name: "offered slot changed",
			alter: func(left, _ *Player, offered inventoryStack) ([protocol.PlayerTradeSlots]playerTradeOffer, [protocol.PlayerTradeSlots]playerTradeOffer) {
				putTradeStack(left, 0, stackOf(ItemLog, 1))
				return oneTradeOffer(0, 0, offered), [protocol.PlayerTradeSlots]playerTradeOffer{}
			},
			want: playerTradeSettlementChanged,
		},
		{
			name: "same pack slot offered twice",
			alter: func(_ *Player, _ *Player, offered inventoryStack) ([protocol.PlayerTradeSlots]playerTradeOffer, [protocol.PlayerTradeSlots]playerTradeOffer) {
				offers := oneTradeOffer(0, 0, offered)
				offers[1] = playerTradeOffer{packSlot: 0, stack: offered}
				return offers, [protocol.PlayerTradeSlots]playerTradeOffer{}
			},
			want: playerTradeSettlementChanged,
		},
		{
			name: "equipment offered",
			alter: func(left, _ *Player, _ inventoryStack) ([protocol.PlayerTradeSlots]playerTradeOffer, [protocol.PlayerTradeSlots]playerTradeOffer) {
				blade := stackOf(ItemIronSword, 1)
				putTradeStack(left, uint8(equipmentFirst), blade)
				return oneTradeOffer(0, uint8(equipmentFirst), blade), [protocol.PlayerTradeSlots]playerTradeOffer{}
			},
			want: playerTradeSettlementChanged,
		},
		{
			name: "receiver has no room",
			alter: func(_ *Player, right *Player, offered inventoryStack) ([protocol.PlayerTradeSlots]playerTradeOffer, [protocol.PlayerTradeSlots]playerTradeOffer) {
				fillTradePack(right, stackOf(ItemDirt, 64))
				return oneTradeOffer(0, 0, offered), [protocol.PlayerTradeSlots]playerTradeOffer{}
			},
			want: playerTradeSettlementFull,
		},
		{
			name: "offered silver no longer exists",
			alter: func(_ *Player, _ *Player, offered inventoryStack) ([protocol.PlayerTradeSlots]playerTradeOffer, [protocol.PlayerTradeSlots]playerTradeOffer) {
				return oneTradeOffer(0, 0, offered), [protocol.PlayerTradeSlots]playerTradeOffer{}
			},
			want: playerTradeSettlementChanged,
		},
		{
			name: "receiving silver would overflow",
			alter: func(_ *Player, _ *Player, offered inventoryStack) ([protocol.PlayerTradeSlots]playerTradeOffer, [protocol.PlayerTradeSlots]playerTradeOffer) {
				return oneTradeOffer(0, 0, offered), [protocol.PlayerTradeSlots]playerTradeOffer{}
			},
			want: playerTradeSettlementFull,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			h, left, right := tradeTransactionPlayers(t)
			leftStack := stackOf(ItemStone, 7)
			putTradeStack(left, 0, leftStack)
			putTradeStack(right, 1, stackOf(ItemLog, 2))
			fundTradePurse(left, 10)
			fundTradePurse(right, 20)
			leftOffers, rightOffers := test.alter(left, right, leftStack)
			leftSilver, rightSilver := uint32(0), uint32(0)
			switch test.name {
			case "offered silver no longer exists":
				leftSilver = 11
			case "receiving silver would overflow":
				fundTradePurse(left, math.MaxUint32)
				rightSilver = 1
			}
			beforeLeft, beforeLeftSilver := tradeInventory(left)
			beforeRight, beforeRightSilver := tradeInventory(right)

			h.sim.mu.Lock()
			result := settlePlayerTradeLocked(
				playerTradeSide{player: left, offers: leftOffers, silver: leftSilver},
				playerTradeSide{player: right, offers: rightOffers, silver: rightSilver},
			)
			h.sim.mu.Unlock()
			if result != test.want {
				t.Fatalf("settlement = %d, want %d", result, test.want)
			}

			afterLeft, afterLeftSilver := tradeInventory(left)
			afterRight, afterRightSilver := tradeInventory(right)
			if afterLeft != beforeLeft || afterRight != beforeRight ||
				afterLeftSilver != beforeLeftSilver || afterRightSilver != beforeRightSilver {
				t.Fatal("a refused exchange changed an authoritative pack or purse")
			}
		})
	}
}

func TestPlayerTradeSettlementReleasesTheFirstLockWhenTheSecondIsBusy(t *testing.T) {
	t.Parallel()

	h, lower, higher := tradeTransactionPlayers(t)
	for _, test := range []struct {
		name string
		busy *Player
	}{
		{name: "lower id busy", busy: lower},
		{name: "higher id busy", busy: higher},
	} {
		t.Run(test.name, func(t *testing.T) {
			test.busy.inventory.mu.Lock()
			h.sim.mu.Lock()
			// Arguments reversed deliberately: lock order belongs to entity ids, not to
			// which participant happened to send the second confirmation.
			result := settlePlayerTradeLocked(
				playerTradeSide{player: higher},
				playerTradeSide{player: lower},
			)
			h.sim.mu.Unlock()
			test.busy.inventory.mu.Unlock()
			if result != playerTradeSettlementBusy {
				t.Fatalf("settlement = %d, want busy", result)
			}

			if !lower.inventory.mu.TryLock() {
				t.Fatal("lower-id inventory remained locked after a busy settlement")
			}
			lower.inventory.mu.Unlock()
			if !higher.inventory.mu.TryLock() {
				t.Fatal("higher-id inventory remained locked after a busy settlement")
			}
			higher.inventory.mu.Unlock()
		})
	}
}

func TestPlayerTradeSettlementConservesEveryItemAndSilver(t *testing.T) {
	t.Parallel()

	h, left, right := tradeTransactionPlayers(t)
	leftStacks := []inventoryStack{
		stackOf(ItemStone, 63),
		stackOf(ItemLog, 5),
		stackOf(ItemIronSword, 1),
	}
	leftStacks[2].durability = 9
	rightStacks := []inventoryStack{
		stackOf(ItemStone, 2),
		stackOf(ItemRawIron, 11),
		stackOf(ItemSilver, 6),
	}
	for slot, stack := range leftStacks {
		putTradeStack(left, uint8(slot), stack)
	}
	for slot, stack := range rightStacks {
		putTradeStack(right, uint8(slot), stack)
	}
	fundTradePurse(left, 401)
	fundTradePurse(right, 99)

	beforeItems, beforeSilver := tradeTotals(left, right)
	leftOffers := oneTradeOffer(0, 0, leftStacks[0])
	leftOffers[1] = playerTradeOffer{packSlot: 2, stack: leftStacks[2]}
	rightOffers := oneTradeOffer(0, 0, rightStacks[0])
	rightOffers[3] = playerTradeOffer{packSlot: 1, stack: rightStacks[1]}
	h.sim.mu.Lock()
	result := settlePlayerTradeLocked(
		playerTradeSide{player: left, offers: leftOffers, silver: 121},
		playerTradeSide{player: right, offers: rightOffers, silver: 17},
	)
	h.sim.mu.Unlock()
	if result != playerTradeSettlementComplete {
		t.Fatalf("settlement = %d, want complete", result)
	}
	afterItems, afterSilver := tradeTotals(left, right)
	if len(afterItems) != len(beforeItems) {
		t.Fatalf("item id count changed from %d to %d", len(beforeItems), len(afterItems))
	}
	for item, before := range beforeItems {
		if afterItems[item] != before {
			t.Errorf("item %d total = %d, want %d", item, afterItems[item], before)
		}
	}
	if afterSilver != beforeSilver {
		t.Errorf("silver total = %d, want %d", afterSilver, beforeSilver)
	}
}

func tradeTotals(players ...*Player) (map[ItemID]uint32, uint64) {
	items := make(map[ItemID]uint32)
	var silver uint64
	for _, player := range players {
		slots, purse := tradeInventory(player)
		for _, stack := range slots {
			items[stack.item] += uint32(stack.count)
		}
		silver += uint64(purse)
	}
	delete(items, ItemNone)
	return items, silver
}
