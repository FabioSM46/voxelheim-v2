package game

import (
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// Equipment is the first thing in this game that is not a pile of interchangeable
// resources, and every rule below follows from that one difference: a blade is one
// object with a history, so it cannot be merged with another, split in half, or moved
// without the wear it has accumulated.

func TestAJoinedPlayerCarriesOneSwordAndNothingElse(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	state := player.InventoryState()
	if len(state.Stacks) != int(protocol.InventorySlots) {
		t.Fatalf("the starter inventory has %d slots, want %d", len(state.Stacks), protocol.InventorySlots)
	}
	if got := state.Stacks[0]; got != starterSword() {
		t.Errorf("hotbar slot 0 is %+v, want %+v", got, starterSword())
	}
	for slot, stack := range state.Stacks[1:] {
		if stack != (protocol.InventoryStack{}) {
			t.Errorf("slot %d is %+v, want empty", slot+1, stack)
		}
	}
}

// The registry is what makes a slot durable, and the complete durable set is explicit.
func TestOnlyTheAgreedEquipmentWearsOut(t *testing.T) {
	t.Parallel()

	want := map[ItemID]uint16{
		ItemRustySword:      RustySwordMaxDurability,
		ItemIronSword:       IronSwordMaxDurability,
		ItemShovel:          ToolMaxDurability,
		ItemPickaxe:         ToolMaxDurability,
		ItemAxe:             ToolMaxDurability,
		ItemLeatherCap:      LeatherArmourMaxDurability,
		ItemLeatherJerkin:   LeatherArmourMaxDurability,
		ItemLeatherLeggings: LeatherArmourMaxDurability,
		ItemIronHelm:        IronArmourMaxDurability,
		ItemIronCuirass:     IronArmourMaxDurability,
		ItemIronGreaves:     IronArmourMaxDurability,
	}
	for id, definition := range itemRegistry {
		maximum, durable := want[id]
		if got := definition.maxDurability != 0; got != durable {
			t.Errorf("item %d durable=%v, want %v", id, got, durable)
			continue
		}
		if !durable {
			continue
		}
		stack := stackOf(id, 2)
		if !stack.durable() || stack.durability != maximum || stack.maxDurability != maximum || stack.count != 1 {
			t.Errorf("new durable item %d is %+v, want one whole item at %d/%d", id, stack, maximum, maximum)
		}
	}
}

// A pickup must never acquire durability. Every resource that reaches a slot through
// insertion carries the (0, 0) pair the contract reads as "this does not wear out".
func TestInsertedResourcesCarryNoDurability(t *testing.T) {
	t.Parallel()

	inventory := newInventory()
	if remaining := inventory.insertLocked(ItemStone, 70); remaining != 0 {
		t.Fatalf("insert left %d Stone unplaced", remaining)
	}
	for slot, stack := range inventory.slots {
		if stack.count == 0 {
			continue
		}
		if stack.durability != 0 || stack.maxDurability != 0 {
			t.Errorf("slot %d is %d/%d durability, want a resource's (0, 0)",
				slot, stack.durability, stack.maxDurability)
		}
	}
}

// Two blades are two objects with two different amounts of wear left, and one stack
// could only record one of those numbers.
func TestTwoSwordsNeverBecomeOneStack(t *testing.T) {
	t.Parallel()

	inventory := newStarterInventory()
	if remaining := inventory.insertLocked(ItemRustySword, 1); remaining != 0 {
		t.Fatalf("insert left %d swords unplaced", remaining)
	}

	swords := 0
	for slot, stack := range inventory.slots {
		if stack.item != ItemRustySword {
			continue
		}
		swords++
		if stack.count != 1 {
			t.Errorf("slot %d holds %d swords, want 1", slot, stack.count)
		}
		if stack.maxDurability != RustySwordMaxDurability {
			t.Errorf("slot %d has maximum %d, want %d", slot, stack.maxDurability, RustySwordMaxDurability)
		}
	}
	if swords != 2 {
		t.Errorf("the inventory holds %d swords in their own slots, want 2", swords)
	}
}

// The scenario no registry entry can reach today: an item that both wears out and
// declares a stack bound above one. The slot rule has to hold for it anyway, because a
// registry entry pairing the two is one line and nothing else would notice.
//
// Driven through a definition rather than by registering an item, deliberately: the
// registry is package state every other test reads, and the rule under test belongs to
// the constructor rather than to the registry.
func TestADurableDefinitionNeverFillsASlotTwice(t *testing.T) {
	t.Parallel()

	durable := itemDefinition{maxStack: 8, maxDurability: 100}
	for _, want := range []uint16{1, 2, 8, 64} {
		if got := slotCountFor(durable, want); got != 1 {
			t.Errorf("slotCountFor(a durable definition, %d) = %d, want 1", want, got)
		}
	}

	resource := itemDefinition{maxStack: 64}
	for _, tc := range []struct{ want, expect uint16 }{{1, 1}, {63, 63}, {64, 64}, {70, 64}} {
		if got := slotCountFor(resource, tc.want); got != tc.expect {
			t.Errorf("slotCountFor(a resource definition, %d) = %d, want %d", tc.want, got, tc.expect)
		}
	}
}

// insertLocked accounts for what the constructor made, not for what it asked for. The
// two differ whenever a slot holds fewer than the request, which for a resource is the
// stack bound — and for equipment would be every count above one.
func TestInsertionAccountsForWhatEachSlotActuallyTook(t *testing.T) {
	t.Parallel()

	inventory := newInventory()
	if remaining := inventory.insertLocked(ItemStone, 70); remaining != 0 {
		t.Fatalf("insert left %d Stone unplaced", remaining)
	}

	total := 0
	for slot, stack := range inventory.slots {
		if stack.count == 0 {
			continue
		}
		total += int(stack.count)
		if stack.count > stackLimit(ItemStone) {
			t.Errorf("slot %d holds %d Stone, over the bound of %d", slot, stack.count, stackLimit(ItemStone))
		}
	}
	if total != 70 {
		t.Errorf("the inventory holds %d Stone, want the 70 that were inserted", total)
	}
}

// Moving a blade moves its history with it. A slot that received a "copy" of the item
// without its wear would be a repair the server never granted.
func TestMovingASwordCarriesItsWear(t *testing.T) {
	t.Parallel()

	inventory := newStarterInventory()
	inventory.slots[0].durability = 42

	if !inventory.moveLocked(protocol.InventoryMoveRequest{From: 0, To: 5, Count: 1}) {
		t.Fatal("moving a sword to an empty slot was refused")
	}
	if got := inventory.slots[0]; got != (inventoryStack{}) {
		t.Errorf("the source slot is %+v, want empty", got)
	}
	want := inventoryStack{item: ItemRustySword, count: 1, durability: 42, maxDurability: RustySwordMaxDurability}
	if got := inventory.slots[5]; got != want {
		t.Errorf("the target slot is %+v, want %+v", got, want)
	}
}

// A swap is the one way a durable item and a resource change places, and neither may
// pick up the other's durability on the way.
func TestSwappingASwordWithAStackKeepsBothIntact(t *testing.T) {
	t.Parallel()

	inventory := newStarterInventory()
	inventory.slots[0].durability = 7
	inventory.slots[1] = stackOf(ItemStone, 10)

	if !inventory.moveLocked(protocol.InventoryMoveRequest{From: 0, To: 1, Count: 1}) {
		t.Fatal("swapping a sword with a resource stack was refused")
	}
	wantStone := inventoryStack{item: ItemStone, count: 10}
	if got := inventory.slots[0]; got != wantStone {
		t.Errorf("slot 0 is %+v, want %+v", got, wantStone)
	}
	wantSword := inventoryStack{item: ItemRustySword, count: 1, durability: 7, maxDurability: RustySwordMaxDurability}
	if got := inventory.slots[1]; got != wantSword {
		t.Errorf("slot 1 is %+v, want %+v", got, wantSword)
	}
}

// Merging is refused, and it must be refused on durability rather than on the stack
// bound: a later durable item allowed to stack two deep would otherwise start merging
// two wear values into one.
func TestOneSwordNeverMergesIntoAnother(t *testing.T) {
	t.Parallel()

	inventory := newStarterInventory()
	inventory.slots[1] = stackOf(ItemRustySword, 1)
	inventory.slots[1].durability = 50

	if inventory.moveLocked(protocol.InventoryMoveRequest{From: 0, To: 1, Count: 1}) {
		t.Fatal("one sword merged into another")
	}
	if got := inventory.slots[0]; got != starterStack() {
		t.Errorf("the refused move changed slot 0: %+v", got)
	}
	if got := inventory.slots[1].durability; got != 50 {
		t.Errorf("the refused move changed slot 1's durability to %d, want 50", got)
	}
}

// The invariant guard in moveLocked, driven from the only place it is reachable.
//
// No registered item stacks two deep and wears out, so a durable stack cannot arise
// through any production path — which is precisely why the refusal is written down and
// tested here rather than left to maxStack. A registry entry is one line to change.
func TestADurableStackCannotBeSplit(t *testing.T) {
	t.Parallel()

	inventory := newInventory()
	inventory.slots[0] = inventoryStack{
		item: ItemRustySword, count: 2, durability: 50, maxDurability: RustySwordMaxDurability,
	}

	if inventory.moveLocked(protocol.InventoryMoveRequest{From: 0, To: 5, Count: 1}) {
		t.Fatal("a durable stack was split in half")
	}
	if got := inventory.slots[0].count; got != 2 {
		t.Errorf("the refused split left %d in the source slot, want 2", got)
	}
	if got := inventory.slots[5]; got != (inventoryStack{}) {
		t.Errorf("the refused split put %+v in the target slot", got)
	}
}

// Zero durability is unusable, not gone. The slot keeps the item, the count and its
// place in the authoritative state; nothing here decides what "unusable" costs.
func TestAWornOutSwordStaysInItsSlot(t *testing.T) {
	t.Parallel()

	inventory := newStarterInventory()
	inventory.slots[0].durability = 0

	stack, ok := inventory.stackAtLocked(0)
	if !ok {
		t.Fatal("a worn-out sword reads as an empty slot")
	}
	if stack.item != ItemRustySword || stack.count != 1 {
		t.Errorf("slot 0 holds %+v, want one rusty sword", stack)
	}

	want := protocol.InventoryStack{
		ItemID: uint16(ItemRustySword), Count: 1, Durability: 0, MaxDurability: RustySwordMaxDurability,
	}
	if got := inventory.stateLocked().Stacks[0]; got != want {
		t.Errorf("the emitted slot 0 is %+v, want %+v", got, want)
	}
}

// Every boundary of the approved penalty, floor(current * 4/5).
func TestTheDeathPenaltyIsAnExactFraction(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct{ current, want uint16 }{
		{current: 100, want: 80},
		{current: 80, want: 64},
		{current: 5, want: 4},
		{current: 4, want: 3},
		{current: 3, want: 2},
		{current: 2, want: 1},
		// The last point at which a blade still works, and the step that ends it.
		{current: 1, want: 0},
		// Already worn out: the penalty has nothing left to take and takes nothing.
		{current: 0, want: 0},
		// The widening in wornByDeath, not decoration: 65535 * 4 overflows a uint16.
		{current: 65535, want: 52428},
	} {
		if got := wornByDeath(tc.current); got != tc.want {
			t.Errorf("wornByDeath(%d) = %d, want %d", tc.current, got, tc.want)
		}
	}
}

// Death costs condition, never possessions: no item id moves, no count changes, no slot
// empties, and nothing that does not wear out is touched at all.
func TestTheDeathPenaltyWearsOnlyEquipment(t *testing.T) {
	t.Parallel()

	inventory := newStarterInventory()
	inventory.slots[1] = stackOf(ItemStone, 64)
	inventory.slots[2] = stackOf(ItemRustySword, 1)
	inventory.slots[2].durability = 1
	inventory.slots[3] = stackOf(ItemRustySword, 1)
	inventory.slots[3].durability = 0

	if !inventory.applyDeathPenaltyLocked() {
		t.Fatal("the penalty reported no change on an inventory holding a full blade")
	}

	for slot, want := range map[int]inventoryStack{
		0: {item: ItemRustySword, count: 1, durability: 80, maxDurability: RustySwordMaxDurability},
		1: {item: ItemStone, count: 64},
		2: {item: ItemRustySword, count: 1, durability: 0, maxDurability: RustySwordMaxDurability},
		3: {item: ItemRustySword, count: 1, durability: 0, maxDurability: RustySwordMaxDurability},
	} {
		if got := inventory.slots[slot]; got != want {
			t.Errorf("slot %d is %+v, want %+v", slot, got, want)
		}
	}
}

// The pack behind a player is not on them. Dying costs the condition of what was being
// carried, and a spare comes out of the pack exactly as it went in.
//
// This is the assertion the old penalty failed: it reached every slot, so a player who
// stowed a second blade lost condition on both. Everything on the hotbar still pays.
func TestTheDeathPenaltySparesThePack(t *testing.T) {
	t.Parallel()

	hotbar := int(protocol.HotbarSlots) - 1
	stowed := int(protocol.HotbarSlots)
	last := equipmentFirst - 1

	inventory := newStarterInventory()
	// On them: the far end of the hotbar, so the rule is not passing by only reaching
	// slot 0, and a resource beside it that has nothing to lose either way.
	inventory.slots[hotbar] = stackOf(ItemPickaxe, 1)
	inventory.slots[1] = stackOf(ItemStone, 64)
	// Stowed: the first slot past the hotbar, and the last slot of the pack.
	inventory.slots[stowed] = stackOf(ItemIronSword, 1)
	inventory.slots[last] = stackOf(ItemAxe, 1)

	if !inventory.applyDeathPenaltyLocked() {
		t.Fatal("the penalty reported no change on a player carrying a full blade")
	}

	for slot, want := range map[int]inventoryStack{
		0:      {item: ItemRustySword, count: 1, durability: wornByDeath(RustySwordMaxDurability), maxDurability: RustySwordMaxDurability},
		1:      {item: ItemStone, count: 64},
		hotbar: {item: ItemPickaxe, count: 1, durability: wornByDeath(ToolMaxDurability), maxDurability: ToolMaxDurability},
		stowed: {item: ItemIronSword, count: 1, durability: IronSwordMaxDurability, maxDurability: IronSwordMaxDurability},
		last:   {item: ItemAxe, count: 1, durability: ToolMaxDurability, maxDurability: ToolMaxDurability},
	} {
		if got := inventory.slots[slot]; got != want {
			t.Errorf("slot %d is %+v, want %+v", slot, got, want)
		}
	}
}

// The boundary itself, swept rather than sampled: a durable item in every slot, one
// death, and every slot read back against the hotbar's own bound.
//
// The expectation is written as protocol.HotbarSlots rather than as carriedOnPerson, so
// that widening the rule back to the whole inventory fails here instead of quietly
// widening the assertion with it. That mutation is the one this issue exists to prevent.
func TestTheDeathPenaltyReachesExactlyWhatIsOnThePlayer(t *testing.T) {
	t.Parallel()

	inventory := newInventory()
	for slot := range inventory.slots {
		inventory.slots[slot] = stackOf(ItemPickaxe, 1)
	}

	if !inventory.applyDeathPenaltyLocked() {
		t.Fatal("the penalty reported no change on a pack of full pickaxes")
	}

	for slot, stack := range inventory.slots {
		want := ToolMaxDurability
		if slot < int(protocol.HotbarSlots) || slot >= equipmentFirst {
			want = wornByDeath(ToolMaxDurability)
		}
		if stack.durability != want {
			t.Errorf("slot %d durability is %d, want %d", slot, stack.durability, want)
		}
	}
}

// An empty hotbar is a normal death, not a special case: the penalty has nothing on the
// player to spend and says so, and the pack it may not reach is left whole.
func TestTheDeathPenaltyOnAnEmptyHotbarChangesNothing(t *testing.T) {
	t.Parallel()

	stowed := int(protocol.HotbarSlots)

	inventory := newInventory()
	inventory.slots[stowed] = stackOf(ItemIronSword, 1)

	if inventory.applyDeathPenaltyLocked() {
		t.Error("the penalty reported a change on a player carrying nothing")
	}
	want := inventoryStack{item: ItemIronSword, count: 1, durability: IronSwordMaxDurability, maxDurability: IronSwordMaxDurability}
	if got := inventory.slots[stowed]; got != want {
		t.Errorf("the stowed slot is %+v, want %+v", got, want)
	}
}

// An inventory with nothing left to lose has still been penalised. The operation reports
// "did anything change", and a caller must not read that as "it did not run" — see
// tryApplyDeathPenaltyLocked, whose answer is deliberately the other question.
func TestTheDeathPenaltyOnWornEquipmentChangesNothing(t *testing.T) {
	t.Parallel()

	inventory := newStarterInventory()
	inventory.slots[0].durability = 0

	if inventory.applyDeathPenaltyLocked() {
		t.Error("the penalty reported a change on an inventory of worn-out equipment")
	}
	if got := inventory.slots[0].durability; got != 0 {
		t.Errorf("slot 0 durability is %d, want 0", got)
	}
}

// The tick never waits for a session goroutine, so the operation reports that it could
// not run rather than blocking — and it changes nothing when it says so.
func TestTheDeathPenaltyDefersRatherThanWaitingForTheInventoryLock(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	player.inventory.mu.Lock()
	h.sim.mu.Lock()
	ran := player.tryApplyDeathPenaltyLocked()
	dirty := player.inventoryDirty
	h.sim.mu.Unlock()
	player.inventory.mu.Unlock()

	if ran {
		t.Error("the penalty ran while a session held the inventory lock")
	}
	if dirty {
		t.Error("a deferred penalty marked the inventory for delivery")
	}
	if got := player.InventoryState().Stacks[0]; got != starterSword() {
		t.Errorf("a deferred penalty changed slot 0 to %+v", got)
	}
}

// An applied penalty goes out on the durable delivery path, because an inventory state
// is not superseded by the next tick's: a full outbound queue must not be able to leave
// the client showing durability the server has already spent.
func TestAnAppliedDeathPenaltyMarksTheInventoryForDelivery(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})

	h.sim.mu.Lock()
	ran := player.tryApplyDeathPenaltyLocked()
	dirty := player.inventoryDirty
	h.sim.mu.Unlock()

	if !ran {
		t.Fatal("the penalty did not run on an uncontended inventory")
	}
	if !dirty {
		t.Fatal("the applied penalty did not mark the inventory for delivery")
	}

	want := protocol.InventoryStack{
		ItemID:        uint16(ItemRustySword),
		Count:         1,
		Durability:    wornByDeath(RustySwordMaxDurability),
		MaxDurability: RustySwordMaxDurability,
	}
	if got := player.InventoryState().Stacks[0]; got != want {
		t.Errorf("slot 0 is %+v, want %+v", got, want)
	}

	// And the tick delivers it, which is what the flag is for.
	h.step()
	states := out.inventoryStates(t)
	if len(states) == 0 {
		t.Fatal("the penalised inventory was never delivered")
	}
	if got := states[len(states)-1].Stacks[0]; got != want {
		t.Errorf("the delivered slot 0 is %+v, want %+v", got, want)
	}
}
