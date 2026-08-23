package game

import (
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// The gesture, end to end: the whole of the slot the player named is gone, one drop is
// lying in the cell their feet are in, and the inventory the caller is handed is the one
// the session sends.
//
// The position is asserted against dropSpawnPos rather than against a literal, because what
// "at their feet" means is voxelAt of the player's own position and this test should fail if
// that stops being true — not if the drop's box convention changes.
func TestADroppedStackLeavesThePackAndLandsAtTheFeet(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{4.5, 64, -2.5})

	player.inventory.mu.Lock()
	player.inventory.slots[3] = inventoryStack{item: ItemStone, count: 17}
	player.inventory.mu.Unlock()

	state, err := player.DropItem(protocol.DropItemRequest{Slot: 3, ClientTick: 7})
	if err != nil {
		t.Fatalf("DropItem: %v", err)
	}

	if got := heldCount(state, ItemStone); got != 0 {
		t.Errorf("the returned inventory still holds %d Stone", got)
	}
	if got := heldCount(player.InventoryState(), ItemStone); got != 0 {
		t.Errorf("the authoritative pack still holds %d Stone", got)
	}
	if got := h.dropCount(); got != 1 {
		t.Fatalf("%d drops are lying in the world, want exactly one", got)
	}

	var only *itemDrop
	h.sim.mu.Lock()
	for _, d := range h.sim.drops {
		only = d
	}
	h.sim.mu.Unlock()

	if only.item != ItemStone || only.count != 17 {
		t.Errorf("the drop holds %d of item %d, want 17 Stone", only.count, uint16(only.item))
	}
	want := dropSpawnPos(voxelAt(player.pos))
	for axis := range want {
		if diff := only.pos[axis] - want[axis]; diff > dropTolerance || diff < -dropTolerance {
			t.Errorf("the drop's %c is %.4f, want %.4f — the cell the feet are in", 'x'+rune(axis), only.pos[axis], want[axis])
		}
	}
}

// An ordinary drop and nothing more: the stack a player put down expires on exactly the
// same clock as one the world produced, because it *is* one — spawnStackDrop is the one
// core path and there is no second lifetime anywhere.
//
// The player walks away first, because a drop lands at their feet and the pickup rule is
// proximity. That is the subject of the test below rather than an inconvenience here.
func TestADroppedStackExpiresWithEveryOtherDrop(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	player.inventory.mu.Lock()
	player.inventory.slots[0] = inventoryStack{item: ItemStone, count: 4}
	player.inventory.mu.Unlock()

	if _, err := player.DropItem(protocol.DropItemRequest{Slot: 0}); err != nil {
		t.Fatalf("DropItem: %v", err)
	}
	walkAway(h, player)

	lifetime := dropLifetimeTicks(DefaultTickRate)
	h.advance(lifetime - 1)
	if got := h.dropCount(); got != 1 {
		t.Fatalf("%d drops one tick before the lifetime is up, want one", got)
	}
	h.step()
	if got := h.dropCount(); got != 0 {
		t.Errorf("%d drops after DropLifetime, want the stack to be gone for good", got)
	}
}

// **A player standing on what they put down picks it straight back up**, and this test is
// here to say that is the behaviour rather than an oversight.
//
// The issue asks for a drop indistinguishable from one the world produced, and an ordinary
// drop is collected by proximity once dropPickupDelayTicks have passed — half a second at
// the default rate — with no memory of who spawned it. So dropping is "put it down and step
// away", exactly as it already is for a block broken at your own feet. Making it otherwise
// means a per-drop delay or a drop with some velocity, and both are changes to pickup.
func TestADroppedStackIsCollectedBackByAPlayerWhoStaysOnIt(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	player.inventory.mu.Lock()
	player.inventory.slots[0] = inventoryStack{item: ItemStone, count: 4}
	player.inventory.mu.Unlock()

	if _, err := player.DropItem(protocol.DropItemRequest{Slot: 0}); err != nil {
		t.Fatalf("DropItem: %v", err)
	}
	if got := heldCount(player.InventoryState(), ItemStone); got != 0 {
		t.Fatalf("the pack still holds %d Stone the moment after the drop", got)
	}

	h.advance(dropPickupDelayTicks)
	if got := h.dropCount(); got != 1 {
		t.Fatalf("%d drops on the tenth tick, want the delay to still be running", got)
	}

	h.step()
	if got := h.dropCount(); got != 0 {
		t.Errorf("%d drops after the pickup delay, want the stack collected back", got)
	}
	if got := heldCount(player.InventoryState(), ItemStone); got != 4 {
		t.Errorf("the player holds %d Stone, want the 4 they put down and stood on", got)
	}
}

// walkAway moves a player far enough that nothing they dropped is within pickup range.
//
// Written directly rather than walked to, because these tests are about what a drop does.
// Under sim.mu, which is what guards a position.
func walkAway(h *dropHarness, player *Player) {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	player.pos[0] += 40
	player.chunk = chunkAt(player.pos)
}

// Every refusal is silence, and each one leaves the pack exactly as it was. The error is
// what the session logs; nothing here reaches the wire.
func TestADropIsRefusedInSilenceAndChangesNothing(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name  string
		slot  uint8
		setup func(t *testing.T, h *dropHarness, player *Player)
	}{
		{
			name: "an empty slot",
			slot: 9,
		},
		{
			name: "a slot past the end of the pack",
			slot: protocol.InventorySlots,
			setup: func(_ *testing.T, _ *dropHarness, player *Player) {
				player.inventory.mu.Lock()
				player.inventory.slots[0] = inventoryStack{item: ItemStone, count: 1}
				player.inventory.mu.Unlock()
			},
		},
		{
			name: "the largest slot a byte can name",
			slot: 255,
		},
		{
			name: "an internally invalid durability pair",
			slot: 0,
			setup: func(_ *testing.T, _ *dropHarness, player *Player) {
				player.inventory.mu.Lock()
				player.inventory.slots[0] = inventoryStack{
					item: ItemRustySword, count: 1, durability: 12,
					maxDurability: RustySwordMaxDurability - 1,
				}
				player.inventory.mu.Unlock()
			},
		},
		{
			name: "a player who is dead",
			slot: 0,
			setup: func(_ *testing.T, h *dropHarness, player *Player) {
				player.inventory.mu.Lock()
				player.inventory.slots[0] = inventoryStack{item: ItemStone, count: 1}
				player.inventory.mu.Unlock()

				h.sim.mu.Lock()
				player.damageLocked(PlayerMaxHealth)
				h.sim.mu.Unlock()
			},
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			h := newDropHarness(t, dropTerrain{groundTop: 63})
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			before := player.InventoryState()
			if tc.setup != nil {
				tc.setup(t, h, player)
				before = player.InventoryState()
			}

			if _, err := player.DropItem(protocol.DropItemRequest{Slot: tc.slot}); err == nil {
				t.Fatal("the drop was accepted")
			}
			if got := h.dropCount(); got != 0 {
				t.Errorf("%d drops are lying in the world after a refusal", got)
			}
			after := player.InventoryState()
			for slot := range after.Stacks {
				if after.Stacks[slot] != before.Stacks[slot] {
					t.Errorf("slot %d moved from %+v to %+v", slot, before.Stacks[slot], after.Stacks[slot])
				}
			}
		})
	}
}

// **The inverse of the refusal this test replaced:** a worn thing may be put down, and the
// exact object comes back. The ground state and the collected slot are both asserted so
// neither half can silently restore the registry maximum and grant a repair by dropping.
func TestAWornItemCanBeDroppedAndCollectedWithoutRepair(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})

	player.inventory.mu.Lock()
	player.inventory.slots[0] = inventoryStack{
		item:          ItemRustySword,
		count:         1,
		durability:    12,
		maxDurability: RustySwordMaxDurability,
	}
	player.inventory.mu.Unlock()

	if _, err := player.DropItem(protocol.DropItemRequest{Slot: 0}); err != nil {
		t.Fatalf("putting down the worn blade: %v", err)
	}
	if got := h.dropCount(); got != 1 {
		t.Fatalf("%d drops are lying in the world, want the worn blade", got)
	}

	var ground inventoryStack
	h.sim.mu.Lock()
	for _, drop := range h.sim.drops {
		ground = drop.stack()
	}
	h.sim.mu.Unlock()
	want := inventoryStack{
		item:          ItemRustySword,
		count:         1,
		durability:    12,
		maxDurability: RustySwordMaxDurability,
	}
	if ground != want {
		t.Fatalf("the ground holds %+v, want the exact worn blade %+v", ground, want)
	}

	// The client sees the same condition through the sparse snapshot vector. The first
	// step is still inside the pickup delay, so streaming cannot race collection here.
	h.step()
	shown := out.snapshotDrops(t)
	if len(shown) != 1 || shown[0].ItemID != uint16(want.item) || shown[0].Count != 1 ||
		shown[0].Durability != want.durability || shown[0].MaxDurability != want.maxDurability {
		t.Fatalf("the snapshot carries %+v, want the exact worn blade", shown)
	}

	h.advance(dropPickupDelayTicks - 1)
	h.step()
	if got := h.dropCount(); got != 0 {
		t.Fatalf("%d drops remain after the player collected the blade", got)
	}
	held := player.InventoryState().Stacks[0]
	if held.ItemID != uint16(want.item) || held.Count != want.count ||
		held.Durability != want.durability || held.MaxDurability != want.maxDurability {
		t.Errorf("slot 0 holds %+v, want the blade still at 12/%d", held, RustySwordMaxDurability)
	}
}

// Streamed like any other: the next snapshot carries it as an ordinary ItemDropState, so
// nothing on the wire says a player asked for it.
func TestADroppedStackAppearsInTheNextSnapshot(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})

	player.inventory.mu.Lock()
	player.inventory.slots[0] = inventoryStack{item: ItemLog, count: 3}
	player.inventory.mu.Unlock()

	if _, err := player.DropItem(protocol.DropItemRequest{Slot: 0}); err != nil {
		t.Fatalf("DropItem: %v", err)
	}
	h.step()

	drops := out.snapshotDrops(t)
	if len(drops) != 1 {
		t.Fatalf("the snapshot carries %d drops, want one", len(drops))
	}
	if drops[0].ItemID != uint16(ItemLog) || drops[0].Count != 3 {
		t.Errorf("the snapshot carries %d of item %d, want 3 Log", drops[0].Count, drops[0].ItemID)
	}
}
