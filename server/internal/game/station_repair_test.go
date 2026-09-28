package game

import (
	"math"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

func stationRepairPlayer(t *testing.T) (*structureHarness, *Player) {
	t.Helper()
	h := newStructureHarness(t)
	p, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.plantForge(p, [3]int32{0, 63, 0})
	h.stockPack(p)
	h.equipWorn(p, 0, ItemRustySword, 0)
	setRewardPurse(p, 1000, 0)
	return h, p
}

func TestStationRepairRestoresEveryKindOfSlotAndDebitsExactlyTheWear(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name    string
		slot    uint8
		missing uint16
	}{
		{"one point in hotbar", 0, 1},
		{"broken blade in backpack", protocol.HotbarSlots + 1, RustySwordMaxDurability},
		{"equipped blade", uint8(equipmentMainHand), 37},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h, p := stationRepairPlayer(t)
			h.stockPack(p)
			h.equipWorn(p, tc.slot, ItemRustySword, RustySwordMaxDurability-tc.missing)
			price := uint32(tc.missing) * RepairSilverPerPoint
			setRewardPurse(p, price, 0) // Exact affordability, including an empty purse afterwards.
			before := rewardPackOf(p)
			state, reason, err := p.StationRepair(protocol.StationRepairRequest{TargetSlot: uint16(tc.slot)})
			if err != nil || reason != vnet.RefusalReasonUnknown {
				t.Fatalf("repair = %s, %v", reason, err)
			}
			want := before
			want.slots[tc.slot].durability = RustySwordMaxDurability
			want.silver = 0
			if got := rewardPackOf(p); got != want {
				t.Fatalf("repair changed more than wear and price: got %+v, want %+v", got, want)
			}
			if len(state.Stacks) != int(protocol.InventorySlots) || state.Silver != 0 || state.Stacks[tc.slot].Durability != RustySwordMaxDurability {
				t.Fatalf("full repair answer = %+v", state)
			}
			// Replays find full durability; they cannot debit the same repair again.
			if _, reason, err := p.StationRepair(protocol.StationRepairRequest{TargetSlot: uint16(tc.slot)}); err == nil || reason != vnet.RefusalReasonNothingToRepair {
				t.Fatalf("repeated repair = %s, %v", reason, err)
			}
		})
	}
}

func TestStationRepairRefusalsPreserveTheEntirePackAndPurse(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name    string
		slot    uint16
		prepare func(*structureHarness, *Player)
		want    vnet.RefusalReason
	}{
		{"no forge", 0, func(h *structureHarness, p *Player) { h.standAt(p, [3]float64{40, 64, 0.5}) }, vnet.RefusalReasonNotAtStation},
		{"empty", 1, nil, vnet.RefusalReasonNothingToRepair},
		{"resource", 0, func(h *structureHarness, p *Player) { h.give(p, 0, ItemStone, 1) }, vnet.RefusalReasonNothingToRepair},
		{"full", 0, func(h *structureHarness, p *Player) { h.give(p, 0, ItemRustySword, 1) }, vnet.RefusalReasonNothingToRepair},
		{"first invalid slot", uint16(protocol.InventorySlots), nil, vnet.RefusalReasonNothingToRepair},
		{"no narrowing wrap", 256, nil, vnet.RefusalReasonNothingToRepair},
		{"maximum slot", math.MaxUint16, nil, vnet.RefusalReasonNothingToRepair},
		{"short purse", 0, func(_ *structureHarness, p *Player) {
			setRewardPurse(p, uint32(RustySwordMaxDurability)*RepairSilverPerPoint-1, 0)
		}, vnet.RefusalReasonNotEnoughSilver},
		{"dead", 0, func(h *structureHarness, p *Player) { h.sim.mu.Lock(); p.dieLocked(); h.sim.mu.Unlock() }, vnet.RefusalReasonPlayerIsDead},
		{"leaving", 0, func(_ *structureHarness, p *Player) { p.BeginLeaving() }, vnet.RefusalReasonPlayerIsDead},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h, p := stationRepairPlayer(t)
			if tc.prepare != nil {
				tc.prepare(h, p)
			}
			before := rewardPackOf(p)
			_, reason, err := p.StationRepair(protocol.StationRepairRequest{TargetSlot: tc.slot})
			if err == nil || reason != tc.want {
				t.Fatalf("refusal = %s, %v, want %s", reason, err, tc.want)
			}
			if after := rewardPackOf(p); after != before {
				t.Fatal("a refused repair changed the pack or purse")
			}
		})
	}
}

func TestStationRepairSharesCraftingReachAndAcceptsOnlyForges(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name     string
		kind     vnet.StructureKind
		distance float64
		accepted bool
	}{
		{"inside forge", vnet.StructureKindForge, ForgeCraftRadius - 0.1, true},
		{"outside forge", vnet.StructureKindForge, ForgeCraftRadius + 0.1, false},
		{"leather bench", vnet.StructureKindLeatherBench, 2, false},
		{"armour bench", vnet.StructureKindArmourBench, 2, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := newStructureHarness(t)
			p, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			h.plantCraftingStation(p, tc.kind, [3]int32{0, 63, 0})
			h.equipWorn(p, 0, ItemRustySword, 0)
			setRewardPurse(p, 1000, 0)
			h.standAt(p, [3]float64{0.5, 63.5 + tc.distance - PlayerHeight/2, 0.5})
			before := rewardPackOf(p)
			_, reason, err := p.StationRepair(protocol.StationRepairRequest{TargetSlot: 0})
			if tc.accepted {
				if err != nil {
					t.Fatal(err)
				}
			} else if err == nil || reason != vnet.RefusalReasonNotAtStation || rewardPackOf(p) != before {
				t.Fatalf("station refusal = %s, %v, or changed the inventory", reason, err)
			}
		})
	}
}

func TestStationRepairWorksAtTheWorldOwnedVillageForge(t *testing.T) {
	t.Parallel()
	h := newStructureHarness(t)
	forge := stationAnchors(t, testCapital(t))[vnet.StructureKindForge]
	h.lookAtVoxel(forge)
	p, _ := h.join(1, [3]float32{float32(forge[0]) + 1, float32(forge[1]) + 1, float32(forge[2])})
	h.equipWorn(p, 0, ItemRustySword, 0)
	setRewardPurse(p, 1000, 0)
	state, _, err := p.StationRepair(protocol.StationRepairRequest{TargetSlot: 0})
	if err != nil {
		t.Fatal(err)
	}
	if state.Stacks[0].Durability != RustySwordMaxDurability || state.Silver != 1000-uint32(RustySwordMaxDurability)*RepairSilverPerPoint {
		t.Fatalf("village repair = %+v", state)
	}
}

func TestStationRepairCannotMutateAnInventoryOwnedByAReward(t *testing.T) {
	t.Parallel()
	_, p := stationRepairPlayer(t)
	claim, _ := mustReserveReward(t, p, BossRewardGrant{Silver: 1})
	defer func() {
		if err := p.AbortBossReward(claim); err != nil {
			t.Error(err)
		}
	}()
	before := rewardPackOf(p)
	_, reason, err := p.StationRepair(protocol.StationRepairRequest{})
	if err == nil || reason != vnet.RefusalReasonInventoryBusy || rewardPackOf(p) != before {
		t.Fatalf("reserved inventory repair = %s, %v, or mutated the pack", reason, err)
	}
}

func TestStationRepairDoesNotWaitForAContendedInventory(t *testing.T) {
	t.Parallel()
	_, p := stationRepairPlayer(t)
	before := rewardPackOf(p)
	p.inventory.mu.Lock()
	_, reason, err := p.StationRepair(protocol.StationRepairRequest{})
	p.inventory.mu.Unlock()
	if err == nil || reason != vnet.RefusalReasonInventoryBusy || rewardPackOf(p) != before {
		t.Fatalf("contended inventory repair = %s, %v, or mutated the pack", reason, err)
	}
}

// Exercise the slot representation's arithmetic ceiling independently of today's
// registry values; widening must remain correct when an item gains a larger maximum.
func TestStationRepairPriceHandlesTheDurabilityRepresentationCeiling(t *testing.T) {
	t.Parallel()
	_, p := stationRepairPlayer(t)
	p.inventory.mu.Lock()
	p.inventory.slots[0].maxDurability = math.MaxUint16
	p.inventory.silver = uint32(math.MaxUint16) * RepairSilverPerPoint
	p.inventory.mu.Unlock()
	state, _, err := p.StationRepair(protocol.StationRepairRequest{})
	if err != nil {
		t.Fatal(err)
	}
	if state.Silver != 0 || state.Stacks[0].Durability != math.MaxUint16 {
		t.Fatalf("maximum repair: silver %d, durability %d", state.Silver, state.Stacks[0].Durability)
	}
}

func TestStationRepairRestoresTheEquippedArmourCombatSummary(t *testing.T) {
	t.Parallel()
	h, p := stationRepairPlayer(t)
	h.equipWorn(p, uint8(equipmentChest), ItemLeatherJerkin, 0)
	state, _, err := p.StationRepair(protocol.StationRepairRequest{TargetSlot: uint16(equipmentChest)})
	if err != nil {
		t.Fatal(err)
	}
	p.sim.mu.Lock()
	armour := p.worn.armour
	p.sim.mu.Unlock()
	if state.Stacks[equipmentChest].Durability != LeatherArmourMaxDurability || armour != itemRegistry[ItemLeatherJerkin].armour {
		t.Fatalf("repaired chest: durability %d, effective armour %d", state.Stacks[equipmentChest].Durability, armour)
	}
}
