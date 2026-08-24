package game

import (
	"reflect"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

func consumeRequest(slot uint16) protocol.ConsumeRequest {
	return protocol.ConsumeRequest{Slot: slot, ClientTick: 7}
}

func hungerOf(sim *Sim, player *Player) uint16 {
	sim.mu.Lock()
	defer sim.mu.Unlock()
	return player.hunger
}

func TestEatingConsumesOneItemAndCapsTheReserve(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.give(player, 4, ItemRawMeat, 2)
	h.sim.mu.Lock()
	player.hunger = 90
	h.sim.mu.Unlock()

	state, err := player.Consume(consumeRequest(4))
	if err != nil {
		t.Fatalf("Consume: %v", err)
	}
	if got := hungerOf(h.sim, player); got != PlayerMaxHunger {
		t.Errorf("hunger after eating = %d, want capped at %d", got, PlayerMaxHunger)
	}
	if got := state.Stacks[4]; got.ItemID != uint16(ItemRawMeat) || got.Count != 1 {
		t.Errorf("slot 4 after eating = %+v, want one raw meat", got)
	}
	if len(state.Stacks) != int(protocol.InventorySlots) {
		t.Errorf("Consume returned %d slots, want the full %d", len(state.Stacks), protocol.InventorySlots)
	}
}

func TestEveryRefusedMealLeavesTheLifeAndPackUntouched(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name   string
		slot   uint16
		hunger uint16
		setup  func(*structureHarness, *Player)
	}{
		{"empty slot", 4, 50, func(*structureHarness, *Player) {}},
		{"non-food", 4, 50, func(h *structureHarness, p *Player) { h.give(p, 4, ItemStone, 2) }},
		{"slot beyond the pack", uint16(protocol.InventorySlots), 50, func(h *structureHarness, p *Player) { h.give(p, 4, ItemRawMeat, 2) }},
		{"slot one past uint8", uint16(^uint8(0)) + 1, 50, func(h *structureHarness, p *Player) { h.give(p, 0, ItemRawMeat, 2) }},
		{"slot that would wrap if narrowed", ^uint16(0), 50, func(h *structureHarness, p *Player) { h.give(p, 4, ItemRawMeat, 2) }},
		{"dead player", 4, 50, func(h *structureHarness, p *Player) {
			h.give(p, 4, ItemRawMeat, 2)
			h.sim.mu.Lock()
			p.damageLocked(PlayerMaxHealth)
			h.sim.mu.Unlock()
		}},
		{"full reserve", 4, PlayerMaxHunger, func(h *structureHarness, p *Player) { h.give(p, 4, ItemRawMeat, 2) }},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			h := newStructureHarness(t)
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			tc.setup(h, player)
			h.sim.mu.Lock()
			player.hunger = tc.hunger
			beforeHunger := player.hunger
			h.sim.mu.Unlock()
			before := player.InventoryState()

			if _, err := player.Consume(consumeRequest(tc.slot)); err == nil {
				t.Fatal("Consume accepted an ineligible request")
			}
			if got := hungerOf(h.sim, player); got != beforeHunger {
				t.Errorf("refusal changed hunger from %d to %d", beforeHunger, got)
			}
			if after := player.InventoryState(); !reflect.DeepEqual(after, before) {
				t.Errorf("refusal changed the pack:\n before %+v\n after  %+v", before, after)
			}
		})
	}
}

func TestEachFoodAtZeroRestoresExactlyItsRegistryAmount(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name string
		item ItemID
		want uint16
	}{
		{"raw meat", ItemRawMeat, RawMeatHungerRestore},
		{"cooked meat", ItemCookedMeat, CookedMeatHungerRestore},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			h := newStructureHarness(t)
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			h.give(player, 0, tc.item, 1)
			h.sim.mu.Lock()
			player.hunger = 0
			h.sim.mu.Unlock()

			state, err := player.Consume(consumeRequest(0))
			if err != nil {
				t.Fatalf("Consume: %v", err)
			}
			if got := hungerOf(h.sim, player); got != tc.want {
				t.Errorf("hunger after %s = %d, want %d", tc.name, got, tc.want)
			}
			if got := state.Stacks[0]; got != (protocol.InventoryStack{}) {
				t.Errorf("the last %s left slot 0 as %+v, want empty", tc.name, got)
			}
		})
	}
}
