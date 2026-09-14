package game

import (
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// An attack spends the main hand and nothing else. A weapon named from anywhere but the
// main hand is dropped before admission spends anything — no energy, no cooldown, no
// pending swing — and answered with silence, exactly as a slot holding nothing that
// attacks is.
func TestASwingNamingAnySlotButTheMainHandIsDroppedWithNoSideEffect(t *testing.T) {
	t.Parallel()

	for name, slot := range map[string]uint8{
		"the hotbar slot the starter blade is in": 0,
		"a pack slot holding an iron blade":       20,
		"the off hand holding a shield":           uint8(equipmentOffHand),
		"an index past the table":                 255,
	} {
		h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
		player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
		player.inventory.mu.Lock()
		player.inventory.slots[20] = stackOf(ItemIronSword, 1)
		player.inventory.slots[equipmentOffHand] = stackOf(ItemWoodenShield, 1)
		player.inventory.mu.Unlock()

		h.sim.mu.Lock()
		energyBefore := player.energy
		h.sim.mu.Unlock()

		reason, err := player.Attack(protocol.AttackRequest{Slot: slot, ClientTick: 1})
		if err == nil {
			t.Errorf("%s: the attack was admitted, want it dropped", name)
		}
		if reason != vnet.RefusalReasonUnknown {
			t.Errorf("%s: answered %s, want silence", name, reason)
		}

		h.sim.mu.Lock()
		if player.energy != energyBefore {
			t.Errorf("%s: energy %d after the drop, want the untouched %d", name, player.energy, energyBefore)
		}
		if player.attackCooldown != 0 {
			t.Errorf("%s: cooldown %d after the drop, want none", name, player.attackCooldown)
		}
		if player.pendingSwing != nil {
			t.Errorf("%s: a swing is pending after the drop", name)
		}
		h.sim.mu.Unlock()
	}
}

// A wielded bow still draws its arrows from the hotbar and the pack and from nowhere worn.
// The worn case is a hand-built table no move can produce — an arrow is not equipment —
// and pins that the scan stops at equipmentFirst rather than reading the whole inventory.
func TestAWieldedBowFindsArrowsInTheHotbarAndPackOnly(t *testing.T) {
	t.Parallel()

	for name, tc := range map[string]struct {
		arrowSlot int
		admitted  bool
	}{
		"arrows in the hotbar":       {arrowSlot: 3, admitted: true},
		"arrows in the pack":         {arrowSlot: equipmentFirst - 1, admitted: true},
		"arrows only in a worn slot": {arrowSlot: equipmentHead},
		"no arrows anywhere":         {arrowSlot: -1},
	} {
		h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
		player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
		player.inventory.mu.Lock()
		player.inventory.slots[0] = inventoryStack{}
		player.inventory.slots[equipmentMainHand] = stackOf(ItemBow, 1)
		if tc.arrowSlot >= 0 {
			player.inventory.slots[tc.arrowSlot] = stackOf(ItemArrow, 4)
		}
		player.inventory.mu.Unlock()

		reason, err := player.Attack(protocol.AttackRequest{Slot: mainHandSlot, ClientTick: 1})
		if tc.admitted {
			if err != nil {
				t.Errorf("%s: the wielded bow was refused: %s, %v", name, reason, err)
			}
			continue
		}
		if reason != vnet.RefusalReasonNoAmmunition {
			t.Errorf("%s: answered %s (%v), want NoAmmunition", name, reason, err)
		}
	}
}
