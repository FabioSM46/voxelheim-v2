package game

import (
	"fmt"
	"math"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The gesture, end to end: the whole slot leaves the pack and the drop comes to rest
// in front of the authoritative body rather than at its feet.
func TestADroppedStackLeavesThePackAndLandsInFront(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{4.5, 64, -2.5})
	origin := player.pos

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
	h.advance(100)
	if got := h.drop(only.entityID); got == nil {
		t.Fatal("the player collected the stack without moving")
	}
	if math.Abs(only.pos[0]-origin[0]) > dropTolerance {
		t.Errorf("the north-facing drop moved sideways from x=%v to x=%v", origin[0], only.pos[0])
	}
	if travelled := origin[2] - only.pos[2]; travelled < 1.5 || travelled > 2.0 {
		t.Errorf("the north-facing drop travelled %v blocks, want roughly one block ahead", travelled)
	}
	if overlaps(h.sim.terrain, only.box()) {
		t.Errorf("the drop came to rest inside terrain at %v", only.pos)
	}
}

// An ordinary drop and nothing more: the stack a player put down expires on exactly the
// same clock as one the world produced because both reach one creation core and there is
// no second lifetime anywhere. The player walks away so this test measures only expiry.
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

// The unchanged pickup delay is long enough for the authoritative throw to carry a
// stack out of reach. A stationary player therefore does not immediately undo the drop.
func TestADroppedStackIsNotCollectedBackByAPlayerWhoStaysStillAtAnyTickRate(t *testing.T) {
	t.Parallel()

	for _, tickRate := range []uint8{DefaultTickRate, 40, 255} {
		t.Run(fmt.Sprintf("%d Hz", tickRate), func(t *testing.T) {
			t.Parallel()

			h := newDropHarnessAtTickRate(t, dropTerrain{groundTop: 63}, 8, tickRate)
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
			if got := h.dropCount(); got != 1 {
				t.Errorf("%d drops after the pickup delay at %d Hz, want the placed stack to remain", got, tickRate)
			}
			if got := heldCount(player.InventoryState(), ItemStone); got != 0 {
				t.Errorf("the stationary player holds %d Stone at %d Hz, want the dropped stack to stay away", got, tickRate)
			}
		})
	}
}

func TestAPlayerDropUsesEveryAuthoritativeFacingIncludingDiagonals(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name string
		yaw  float64
		want [2]float64
	}{
		{name: "north", yaw: 0, want: [2]float64{0, -1}},
		{name: "east", yaw: -math.Pi / 2, want: [2]float64{1, 0}},
		{name: "south", yaw: math.Pi, want: [2]float64{0, 1}},
		{name: "west", yaw: math.Pi / 2, want: [2]float64{-1, 0}},
		{name: "northwest diagonal", yaw: math.Pi / 4, want: [2]float64{-math.Sqrt(0.5), -math.Sqrt(0.5)}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			h := newDropHarness(t, dropTerrain{groundTop: 63})
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			origin := player.pos
			h.sim.mu.Lock()
			player.yaw = tc.yaw
			h.sim.mu.Unlock()

			player.inventory.mu.Lock()
			player.inventory.slots[0] = inventoryStack{item: ItemStone, count: 1}
			player.inventory.mu.Unlock()
			if _, err := player.DropItem(protocol.DropItemRequest{Slot: 0}); err != nil {
				t.Fatalf("DropItem: %v", err)
			}

			var drop *itemDrop
			h.sim.mu.Lock()
			for _, drop = range h.sim.drops {
			}
			h.sim.mu.Unlock()
			h.advance(100)
			if h.drop(drop.entityID) == nil {
				t.Fatal("the stationary player collected the drop")
			}

			delta := [2]float64{drop.pos[0] - origin[0], drop.pos[2] - origin[2]}
			distance := math.Hypot(delta[0], delta[1])
			if distance < 1.5 || distance > 2.0 {
				t.Fatalf("the drop travelled %v blocks, want roughly one block ahead", distance)
			}
			dot := delta[0]*tc.want[0] + delta[1]*tc.want[1]
			cross := delta[0]*tc.want[1] - delta[1]*tc.want[0]
			if dot < 1.5 || math.Abs(cross) > dropTolerance {
				t.Errorf("the drop moved by %v for yaw %v, want direction %v", delta, tc.yaw, tc.want)
			}
		})
	}
}

func TestAPlayerDropPlacementDistanceDoesNotDependOnTicks(t *testing.T) {
	t.Parallel()

	delta := dropPlacementDelta(math.Pi / 4)
	if got := math.Hypot(delta[0], delta[1]); math.Abs(got-dropPlacementDistance) > dropTolerance {
		t.Errorf("the placement delta is %v blocks, want %v", got, dropPlacementDistance)
	}
}

// A wall participates in the same axis-by-axis collision as gravity. It stops the
// throw on this side, without swallowing it or letting it tunnel through.
func TestAPlayerDropCannotPassThroughAWall(t *testing.T) {
	t.Parallel()

	terrain := dropWallTerrain{groundTop: 63, wallX: 2}
	h := newDropHarness(t, terrain)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.sim.mu.Lock()
	player.yaw = -math.Pi / 2
	h.sim.mu.Unlock()

	player.inventory.mu.Lock()
	player.inventory.slots[0] = inventoryStack{item: ItemStone, count: 1}
	player.inventory.mu.Unlock()
	if _, err := player.DropItem(protocol.DropItemRequest{Slot: 0}); err != nil {
		t.Fatalf("DropItem: %v", err)
	}
	h.advance(dropPickupDelayTicks)

	var drop *itemDrop
	h.sim.mu.Lock()
	for _, drop = range h.sim.drops {
	}
	h.sim.mu.Unlock()
	if drop == nil {
		t.Fatal("the drop vanished before the pickup delay ended")
	}
	if overlaps(terrain, drop.box()) || drop.box().max[0] >= float64(terrain.wallX) {
		t.Errorf("the drop crossed or entered the wall: box=%+v wall x=%d", drop.box(), terrain.wallX)
	}
	if drop.pos[0] <= 0.5 {
		t.Errorf("the wall left the drop at %v instead of ahead of the player", drop.pos)
	}
}

type dropWallTerrain struct {
	groundTop int64
	wallX     int64
}

func (w dropWallTerrain) Solid(x, y, _ int64) bool { return y <= w.groundTop || x >= w.wallX }

func (w dropWallTerrain) Block(x, y, z int64) (world.Block, bool) {
	if w.Solid(x, y, z) {
		return world.Stone, true
	}
	return world.Air, true
}
func (w dropWallTerrain) Fluid(x, y, z int64) bool { return fluidByBlock(w, x, y, z) }

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
	walkToDrop(h, player, groundDrop(h))
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

func groundDrop(h *dropHarness) *itemDrop {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	for _, drop := range h.sim.drops {
		return drop
	}
	return nil
}

func walkToDrop(h *dropHarness, player *Player, drop *itemDrop) {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	player.pos[0] = drop.pos[0]
	player.pos[2] = drop.pos[2]
	player.chunk = chunkAt(player.pos)
}

// Two requests resolved before the next tick start with the same authoritative
// motion, so the ordinary merge pass still folds them into one wearless stack.
func TestTwoPlayerDropsInSuccessionStillMerge(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	player.inventory.mu.Lock()
	player.inventory.slots[0] = inventoryStack{item: ItemStone, count: 3}
	player.inventory.slots[1] = inventoryStack{item: ItemStone, count: 4}
	player.inventory.mu.Unlock()

	for _, slot := range []uint8{0, 1} {
		if _, err := player.DropItem(protocol.DropItemRequest{Slot: slot}); err != nil {
			t.Fatalf("DropItem slot %d: %v", slot, err)
		}
	}
	h.step()
	if got := h.dropCount(); got != 1 {
		t.Fatalf("two successive drops became %d entities, want one merged stack", got)
	}
	if drop := groundDrop(h); drop == nil || drop.count != 7 {
		t.Errorf("the merged player drop is %+v, want 7 Stone", drop)
	}
}

// A world drop at the landing point may absorb a player's stack, but it never gains
// movement or gets dragged from the position the world chose. The player placement is
// resolved before the entity appears, so merging remains the ordinary stationary rule.
func TestAPlayerDropMergingIntoAWorldDropLeavesTheWorldDropWhereItWas(t *testing.T) {
	t.Parallel()

	h := newDropHarness(t, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	worldDrop := h.spawn(ItemStone, 3, [3]int64{0, 64, -2})
	worldPos := worldDrop.pos

	player.inventory.mu.Lock()
	player.inventory.slots[0] = inventoryStack{item: ItemStone, count: 4}
	player.inventory.mu.Unlock()
	if _, err := player.DropItem(protocol.DropItemRequest{Slot: 0}); err != nil {
		t.Fatalf("DropItem: %v", err)
	}

	h.step()
	if got := h.dropCount(); got != 1 {
		t.Fatalf("the mixed merge left %d drops, want one", got)
	}
	merged := h.drop(worldDrop.entityID)
	if merged == nil || merged.count != 7 {
		t.Fatalf("the older world drop is %+v, want the surviving stack of 7", merged)
	}
	if merged.pos[0] != worldPos[0] || merged.pos[2] != worldPos[2] {
		t.Errorf("the player drop dragged the world drop from %v to %v", worldPos, merged.pos)
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
