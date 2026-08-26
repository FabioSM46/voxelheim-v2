package game

import (
	"math"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

func equipShield(t *testing.T, player *Player, durability uint16) {
	t.Helper()
	player.inventory.mu.Lock()
	player.inventory.slots[equipmentOffHand] = inventoryStack{
		item: ItemWoodenShield, count: 1, durability: durability, maxDurability: WoodenShieldMaxDurability,
	}
	player.refreshWornLocked()
	player.inventory.mu.Unlock()
}

func equipLeatherSet(t *testing.T, player *Player) {
	t.Helper()
	player.inventory.mu.Lock()
	for slot, item := range map[int]ItemID{
		equipmentHead: ItemLeatherCap, equipmentChest: ItemLeatherJerkin, equipmentLegs: ItemLeatherLeggings,
	} {
		player.inventory.slots[slot] = inventoryStack{
			item: item, count: 1, durability: LeatherArmourMaxDurability, maxDurability: LeatherArmourMaxDurability,
		}
	}
	player.refreshWornLocked()
	player.inventory.mu.Unlock()
}

func raisedShield(t *testing.T) (*vitalsHarness, *Player, *dropSink) {
	t.Helper()
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	equipShield(t, player, WoodenShieldMaxDurability)
	player.Block(true)
	return h, player, out
}

func TestBlockRequiresALiveUsableOffHandShieldAndCancelsAPendingSwing(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	player.Block(true)
	if h.vitals(player).Blocking {
		t.Fatal("empty off hand blocked")
	}
	equipShield(t, player, WoodenShieldMaxDurability)
	if err := player.Attack(protocol.AttackRequest{Slot: 0, ClientTick: 1}); err != nil {
		t.Fatalf("Attack: %v", err)
	}
	player.Block(true)
	if !h.vitals(player).Blocking {
		t.Fatal("usable shield did not block")
	}
	if err := player.Attack(protocol.AttackRequest{Slot: 255, ClientTick: 0}); err != nil {
		t.Fatalf("blocked Attack: %v", err)
	}
	h.sim.mu.Lock()
	pending := player.pendingSwing
	h.sim.mu.Unlock()
	if pending != nil {
		t.Fatal("block retained a swing")
	}
	player.Block(false)
	if h.vitals(player).Blocking {
		t.Fatal("release retained blocking")
	}
}

func TestBlockRejectsANonShieldAndAWornThroughShield(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	player.inventory.mu.Lock()
	player.inventory.slots[equipmentOffHand] = inventoryStack{
		item: ItemIronSword, count: 1, durability: IronSwordMaxDurability, maxDurability: IronSwordMaxDurability,
	}
	player.refreshWornLocked()
	player.inventory.mu.Unlock()
	player.Block(true)
	if h.vitals(player).Blocking {
		t.Fatal("non-shield blocked")
	}

	equipShield(t, player, 0)
	player.Block(true)
	if h.vitals(player).Blocking {
		t.Fatal("worn shield blocked")
	}
}

func TestBlockingProjectsAndEveryAuthoritativeRemovalClearsIt(t *testing.T) {
	t.Parallel()

	t.Run("inventory move", func(t *testing.T) {
		h, player, out := raisedShield(t)
		h.step()

		snapshot := newestSnapshot(t, out)
		if !snapshot.SelfVitals(nil).Blocking() {
			t.Fatal("vitals omitted blocking")
		}
		if snapshot.BlockingPlayersLength() != 1 || snapshot.BlockingPlayers(0) != player.entityID {
			t.Fatalf("blocking projection does not contain only %d", player.entityID)
		}

		if _, err := player.MoveInventory(protocol.InventoryMoveRequest{
			From: uint8(equipmentOffHand), To: 4, Count: 1,
		}); err != nil {
			t.Fatalf("move shield: %v", err)
		}
		if h.vitals(player).Blocking {
			t.Fatal("move retained blocking")
		}
	})

	t.Run("death", func(t *testing.T) {
		h, player, _ := raisedShield(t)
		h.hurt(player, PlayerMaxHealth)
		if h.vitals(player).Blocking {
			t.Fatal("death retained block")
		}
	})

	t.Run("disconnect", func(t *testing.T) {
		h, player, _ := raisedShield(t)
		player.BeginLeaving()
		if h.vitals(player).Blocking {
			t.Fatal("disconnect retained block")
		}
	})
}

func TestAFrontFacingShieldHalvesAMobBlowSpendsWearAndCreditsThreat(t *testing.T) {
	t.Parallel()
	h, player, _ := raisedShield(t)
	mobID := h.spawnDraugrAt([3]float32{0.5, 64, -1.0})
	equipLeatherSet(t, player)
	h.aimAt(player, 0, 0)
	armMobBlow(t, h, mobID, player)

	before := h.vitals(player).Health
	h.step()
	const wantDamage = 4
	if got := before - h.vitals(player).Health; got != wantDamage {
		t.Errorf("blocked blow cost %d health, want %d", got, wantDamage)
	}
	player.inventory.mu.Lock()
	durability := player.inventory.slots[equipmentOffHand].durability
	player.inventory.mu.Unlock()
	if durability != WoodenShieldMaxDurability-1 {
		t.Errorf("shield durability = %d, want %d", durability, WoodenShieldMaxDurability-1)
	}
	if got := threatFor(t, h, mobID, player.entityID); got != ShieldTauntThreat {
		t.Errorf("block threat = %v, want %v", got, ShieldTauntThreat)
	}
}

func TestAShieldBehindThePlayerDoesNotBlock(t *testing.T) {
	t.Parallel()
	h, player, _ := raisedShield(t)
	mobID := h.spawnDraugrAt([3]float32{0.5, 64, -1.0})
	equipLeatherSet(t, player)
	h.aimAt(player, math.Pi, 0)
	armMobBlow(t, h, mobID, player)

	before := h.vitals(player).Health
	h.step()
	const wantDamage = 8
	if got := before - h.vitals(player).Health; got != wantDamage {
		t.Errorf("rear blow cost %d health, want %d", got, wantDamage)
	}
	player.inventory.mu.Lock()
	durability := player.inventory.slots[equipmentOffHand].durability
	player.inventory.mu.Unlock()
	if durability != WoodenShieldMaxDurability {
		t.Errorf("rear blow spent shield durability: %d", durability)
	}
}

func TestAContendedShieldBlockIsFreeAndNotRetried(t *testing.T) {
	t.Parallel()
	h, player, _ := raisedShield(t)
	mobID := h.spawnDraugrAt([3]float32{0.5, 64, -1.0})
	h.aimAt(player, 0, 0)
	armMobBlow(t, h, mobID, player)

	player.inventory.mu.Lock()
	h.step()
	if got := player.inventory.slots[equipmentOffHand].durability; got != WoodenShieldMaxDurability {
		t.Errorf("contended block spent durability: %d", got)
	}
	player.inventory.mu.Unlock()
	h.step()
	player.inventory.mu.Lock()
	if got := player.inventory.slots[equipmentOffHand].durability; got != WoodenShieldMaxDurability {
		t.Errorf("later tick retried the spend: %d", got)
	}
	player.inventory.mu.Unlock()
}
