package game

import (
	"errors"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// mainHandHands is the classification every main-hand row must be given here explicitly:
// true where the weapon leaves the off hand free. The column's zero already fails closed,
// and this map is the second half of the pair — a new weapon row is a red test until
// somebody has decided how many hands it takes.
var mainHandHands = map[ItemID]bool{
	ItemRustySword:    true,
	ItemIronSword:     true,
	ItemBow:           false,
	ItemWoodenSceptre: false,
}

// The registry records which weapons need both hands: the bow and the sceptre do, the two
// swords do not, every main-hand row is classified, and no row that is not held in the
// main hand claims to be one-handed.
func TestEveryMainHandRowIsClassifiedByHowManyHandsItTakes(t *testing.T) {
	t.Parallel()

	for id, definition := range itemRegistry {
		if definition.wornAt != wornMainHand {
			if definition.oneHanded {
				t.Errorf("item %d is worn at %d and says oneHanded; the column belongs to the main hand only", id, definition.wornAt)
			}
			continue
		}
		want, classified := mainHandHands[id]
		if !classified {
			t.Errorf("main-hand item %d is not classified by how many hands it takes", id)
			continue
		}
		if definition.oneHanded != want {
			t.Errorf("item %d oneHanded = %v, want %v", id, definition.oneHanded, want)
		}
	}
	for id := range mainHandHands {
		if definition, known := itemByID(id); !known || definition.wornAt != wornMainHand {
			t.Errorf("classified item %d is not a main-hand row", id)
		}
	}
}

// A main-hand row that says nothing about its hands takes both, and the off hand's
// swap-back branch refuses too. Not parallel: it adds synthetic rows to the shared
// registry for its own duration.
func TestAMainHandRowThatNamesNoHandsCannotBeHeldBesideAShield(t *testing.T) {
	const (
		testLauncher ItemID = 64_970 + iota
		testBuckler
	)
	itemRegistry[testLauncher] = itemDefinition{places: world.Air, maxStack: 1, maxDurability: 10, wornAt: wornMainHand}
	itemRegistry[testBuckler] = itemDefinition{places: world.Air, maxStack: 1, maxDurability: 10, wornAt: wornOffHand}
	t.Cleanup(func() {
		delete(itemRegistry, testLauncher)
		delete(itemRegistry, testBuckler)
	})

	inventory := newInventory()
	inventory.slots[4] = stackOf(testLauncher, 1)
	inventory.slots[equipmentOffHand] = stackOf(ItemWoodenShield, 1)
	before := inventory.slots
	if err := inventory.moveLocked(protocol.InventoryMoveRequest{From: 4, To: uint8(equipmentMainHand), Count: 1}); !errors.Is(err, ErrHandsOccupied) {
		t.Fatalf("an unclassified launcher entered the main hand beside a shield: %v", err)
	}
	if inventory.slots != before {
		t.Error("the refused move changed the slots")
	}

	// The off hand's swap-back branch needs two different off-hand items, and the registry
	// holds one. No legal path reaches a two-handed weapon beside a shield — Life.Validate
	// refuses that table — so it is built by hand, and the rule still refuses to send a
	// second off-hand item into that hand.
	inventory = newInventory()
	inventory.slots[equipmentMainHand] = stackOf(ItemBow, 1)
	inventory.slots[equipmentOffHand] = stackOf(ItemWoodenShield, 1)
	inventory.slots[4] = stackOf(testBuckler, 1)
	before = inventory.slots
	if err := inventory.moveLocked(protocol.InventoryMoveRequest{From: uint8(equipmentOffHand), To: 4, Count: 1}); !errors.Is(err, ErrHandsOccupied) {
		t.Fatalf("a buckler was swapped into the off hand beside a bow: %v", err)
	}
	if inventory.slots != before {
		t.Error("the refused swap changed the slots")
	}
}

// Every weapon against the shield, on every path into a hand: dragged into the empty hand,
// dragged onto what the hand already holds, and sent back into the hand by a swap that
// started there. A two-handed weapon refuses each with ErrHandsOccupied and leaves every
// slot as it was; a one-handed one lands.
func TestTheTwoHandsAreJudgedTogetherOnEveryPathIntoThem(t *testing.T) {
	t.Parallel()

	const pack = 4
	shield := stackOf(ItemWoodenShield, 1)
	for weapon, oneHanded := range mainHandHands {
		// A different blade for the hand to hold already, so a swap is a swap rather than
		// a same-item merge that equipment refuses for its own reason.
		otherBlade := ItemRustySword
		if weapon == ItemRustySword {
			otherBlade = ItemIronSword
		}
		for _, tc := range []struct {
			name  string
			slots map[int]inventoryStack
			from  int
			to    int
			// guarded is whether the two-handed rule decides this move at all. Otherwise
			// it must land whatever the weapon is.
			guarded bool
		}{
			{
				name:  "into an empty main hand beside a worn shield",
				slots: map[int]inventoryStack{pack: stackOf(weapon, 1), equipmentOffHand: shield},
				from:  pack, to: equipmentMainHand, guarded: true,
			},
			{
				name:  "onto a blade in the main hand beside a worn shield",
				slots: map[int]inventoryStack{pack: stackOf(weapon, 1), equipmentMainHand: stackOf(otherBlade, 1), equipmentOffHand: shield},
				from:  pack, to: equipmentMainHand, guarded: true,
			},
			{
				name:  "a shield into an empty off hand beside the weapon",
				slots: map[int]inventoryStack{pack: shield, equipmentMainHand: stackOf(weapon, 1)},
				from:  pack, to: equipmentOffHand, guarded: true,
			},
			{
				name:  "a blade swapped out of the main hand onto the weapon beside a worn shield",
				slots: map[int]inventoryStack{pack: stackOf(weapon, 1), equipmentMainHand: stackOf(otherBlade, 1), equipmentOffHand: shield},
				from:  equipmentMainHand, to: pack, guarded: true,
			},
			{
				name:  "into the main hand with nothing in the off hand",
				slots: map[int]inventoryStack{pack: stackOf(weapon, 1)},
				from:  pack, to: equipmentMainHand,
			},
			{
				name:  "out of the main hand while a shield is worn",
				slots: map[int]inventoryStack{equipmentMainHand: stackOf(weapon, 1), equipmentOffHand: stackOf(ItemWoodenShield, 1)},
				from:  equipmentMainHand, to: pack,
			},
		} {
			if tc.name == "out of the main hand while a shield is worn" && !oneHanded {
				// Unreachable for a two-handed weapon for the reason above; taking it out
				// of that table is exercised by a one-handed one.
				continue
			}
			inventory := newInventory()
			for slot, stack := range tc.slots {
				inventory.slots[slot] = stack
			}
			before := inventory.slots
			err := inventory.moveLocked(protocol.InventoryMoveRequest{From: uint8(tc.from), To: uint8(tc.to), Count: 1})

			if tc.guarded && !oneHanded {
				if !errors.Is(err, ErrHandsOccupied) {
					t.Errorf("item %d, %s: err = %v, want ErrHandsOccupied", weapon, tc.name, err)
				}
				if inventory.slots != before {
					t.Errorf("item %d, %s: the refused move changed the slots", weapon, tc.name)
				}
				continue
			}
			if err != nil {
				t.Errorf("item %d, %s: refused with %v, want the move to land", weapon, tc.name, err)
				continue
			}
			if got, want := inventory.slots[tc.to].item, before[tc.from].item; got != want {
				t.Errorf("item %d, %s: the destination holds %d, want %d", weapon, tc.name, got, want)
			}
			if handsOccupied(inventory.slots[equipmentMainHand].item, inventory.slots[equipmentOffHand].item) {
				t.Errorf("item %d, %s: the move left a two-handed weapon beside an off-hand item", weapon, tc.name)
			}
		}
	}
}

// A move that never touches a hand is never answered HandsOccupied, even by a player
// holding a bow: the rule is about the two hand slots and nothing else.
func TestAMoveThatTouchesNoHandIsNeverHandsOccupied(t *testing.T) {
	t.Parallel()

	inventory := newInventory()
	inventory.slots[equipmentMainHand] = stackOf(ItemBow, 1)
	inventory.slots[4] = stackOf(ItemWoodenShield, 1)
	inventory.slots[5] = stackOf(ItemStone, 3)
	if err := inventory.moveLocked(protocol.InventoryMoveRequest{From: 4, To: 9, Count: 1}); err != nil {
		t.Errorf("moving a shield around the pack beside a held bow: %v", err)
	}
	if err := inventory.moveLocked(protocol.InventoryMoveRequest{From: 5, To: 4, Count: 3}); err != nil {
		t.Errorf("moving stone around the pack beside a held bow: %v", err)
	}
	if err := inventory.moveLocked(protocol.InventoryMoveRequest{From: 4, To: uint8(equipmentOffHand), Count: 1}); errors.Is(err, ErrHandsOccupied) {
		t.Error("stone refused for the off hand was answered HandsOccupied instead of the wrong-location silence")
	}
}

// Player.MoveInventory hands the reason to its caller unchanged, so the session can tell
// this refusal from the silent ones.
func TestMoveInventoryReturnsHandsOccupied(t *testing.T) {
	t.Parallel()

	h := newVitalsHarnessAt(t, DefaultTickRate, dropTerrain{groundTop: 63}, 0)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	player.inventory.mu.Lock()
	player.inventory.slots[4] = stackOf(ItemBow, 1)
	player.inventory.slots[equipmentOffHand] = stackOf(ItemWoodenShield, 1)
	player.inventory.mu.Unlock()

	if _, err := player.MoveInventory(protocol.InventoryMoveRequest{From: 4, To: uint8(equipmentMainHand), Count: 1}); !errors.Is(err, ErrHandsOccupied) {
		t.Fatalf("MoveInventory = %v, want ErrHandsOccupied", err)
	}
	if got := player.InventoryState().Stacks[equipmentMainHand]; got != starterSword() {
		t.Errorf("the main hand holds %+v after the refusal, want the starter sword it already held", got)
	}
}

// Life.Validate refuses the pair no move can produce, and admits every pair a move can.
func TestAStoredLifeCannotHoldATwoHandedWeaponBesideAnOffHandItem(t *testing.T) {
	t.Parallel()

	whole := func(item ItemID) protocol.InventoryStack {
		definition, _ := itemByID(item)
		return protocol.InventoryStack{ItemID: uint16(item), Count: 1, Durability: definition.maxDurability, MaxDurability: definition.maxDurability}
	}
	for name, tc := range map[string]struct {
		mainHand, offHand ItemID
		legal             bool
	}{
		"a bow beside a shield":           {mainHand: ItemBow, offHand: ItemWoodenShield},
		"a sceptre beside a shield":       {mainHand: ItemWoodenSceptre, offHand: ItemWoodenShield},
		"a rusty sword beside a shield":   {mainHand: ItemRustySword, offHand: ItemWoodenShield, legal: true},
		"an iron sword beside a shield":   {mainHand: ItemIronSword, offHand: ItemWoodenShield, legal: true},
		"a bow and an empty off hand":     {mainHand: ItemBow, legal: true},
		"a shield and an empty main hand": {offHand: ItemWoodenShield, legal: true},
	} {
		life := Life{Pos: [3]float64{0.5, 64, 0.5}, Health: PlayerMaxHealth, Hunger: PlayerMaxHunger}
		if tc.mainHand != ItemNone {
			life.Slots[equipmentMainHand] = whole(tc.mainHand)
		}
		if tc.offHand != ItemNone {
			life.Slots[equipmentOffHand] = whole(tc.offHand)
		}
		if err := life.Validate(); (err == nil) != tc.legal {
			t.Errorf("%s: Validate = %v, want legal %v", name, err, tc.legal)
		}
	}
}
