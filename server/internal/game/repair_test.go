package game

import (
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// A repair is an authoritative decision made from state no client supplied: what the two
// named slots hold, and what the registry says they are worth. Every test here asks what
// the server decided, and every refusal below asks the same second question — whether the
// pack is bit-identical afterwards — because silence is the only answer a refusal has.

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

// equipWorn puts one durable item in a slot at exactly the wear a test names.
//
// Through `stackOf` and then a direct assignment of the current value: a blade with 49 of
// its 100 left is what a fight and a death leave behind, and there is no authoritative
// path that produces one in a single call. The guards are what keep the shortcut honest —
// a slot this fills is always a shape the contract allows.
func (h *structureHarness) equipWorn(p *Player, slot uint8, item ItemID, durability uint16) {
	h.t.Helper()

	stack := stackOf(item, 1)
	if !stack.durable() {
		h.t.Fatalf("item %d wears out nothing, so it cannot be worn to %d", item, durability)
	}
	if durability > stack.maxDurability {
		h.t.Fatalf("%d durability is more than item %d's maximum of %d", durability, item, stack.maxDurability)
	}
	stack.durability = durability

	p.inventory.mu.Lock()
	defer p.inventory.mu.Unlock()
	p.inventory.slots[slot] = stack
}

// repair is the request a client sends, with the field a test rarely varies filled in.
func (h *structureHarness) repair(p *Player, kit, target uint8) (protocol.InventoryState, error) {
	return p.Repair(protocol.RepairRequest{KitSlot: kit, TargetSlot: target, ClientTick: 1})
}

// ---------------------------------------------------------------------------
// The vocabulary
// ---------------------------------------------------------------------------

// What a kit is, is a registry question, and exactly two entries answer it today.
//
// The sweep over the whole registry is the point rather than decoration: `repairRestore`
// is fail-closed, so a third kit is an entry somebody adds here — and this test is what
// makes adding one a decision rather than an accident. It was written when the stone was
// the only one, and the leather patch arriving as a row and an edit to this list is
// precisely the shape it was written to require.
func TestTheStoneAndThePatchAreTheOnlyRepairKits(t *testing.T) {
	t.Parallel()

	if SharpeningStoneRestore != 50 {
		t.Errorf("SharpeningStoneRestore = %d, want the pinned 50", SharpeningStoneRestore)
	}
	if LeatherPatchRestore != 40 {
		t.Errorf("LeatherPatchRestore = %d, want the pinned 40", LeatherPatchRestore)
	}

	kits := map[ItemID]uint16{
		ItemSharpeningStone: SharpeningStoneRestore,
		ItemLeatherPatch:    LeatherPatchRestore,
	}
	for item, restore := range kits {
		definition, registered := itemByID(item)
		if !registered {
			t.Errorf("kit item %d is not registered", item)
			continue
		}
		if definition.repairRestore != restore {
			t.Errorf("item %d restores %d, want its own constant %d",
				item, definition.repairRestore, restore)
		}
		// A kit wears nothing out of its own: spending it *is* how it is used, and a
		// non-zero maximum here would make it equipment that a repair then destroys.
		if definition.maxDurability != 0 {
			t.Errorf("item %d has %d durability of its own; spending it is how it is used",
				item, definition.maxDurability)
		}
	}

	for id, definition := range itemRegistry {
		if _, isKit := kits[id]; isKit {
			continue
		}
		if definition.repairRestore != 0 {
			t.Errorf("item %d restores %d durability; the stone and the patch are the only repair kits in the game",
				id, definition.repairRestore)
		}
	}
}

// The patch is the field kit the stone is not, and the difference is where it comes from.
//
// Asserted as the two relationships rather than as the two numbers: what matters is that
// a patch is worth *nearly* a stone — so neither is the answer and a player spends what
// they have — and that the recipe for one needs nothing built, which is what makes it a
// thing you make in the field out of what you killed.
func TestThePatchIsAFieldKitWorthNearlyAStone(t *testing.T) {
	t.Parallel()

	if LeatherPatchRestore >= SharpeningStoneRestore {
		t.Errorf("a patch restores %d against a stone's %d: the forge is supposed to be worth walking to",
			LeatherPatchRestore, SharpeningStoneRestore)
	}
	// Within a fifth of the stone. A patch that mended a quarter as much would not be a
	// second answer, it would be a worse one nobody carries.
	if LeatherPatchRestore*5 < SharpeningStoneRestore*4 {
		t.Errorf("a patch restores %d against a stone's %d, which is too far behind to be a choice",
			LeatherPatchRestore, SharpeningStoneRestore)
	}
	if station := recipeTable[vnet.RecipeIDLeatherPatch].station; station != vnet.StructureKindUnknown {
		t.Errorf("the patch needs a %s to make, and its whole point is needing nowhere to stand", station)
	}
}

// A stone is worth half a rusty blade and a quarter of an iron one. That ratio is the
// whole of what makes upkeep a supply cost rather than an expiry date, so it is asserted
// against the two maximums rather than restated as a number.
func TestAStoneIsWorthHalfARustyBladeAndAQuarterOfAnIronOne(t *testing.T) {
	t.Parallel()

	if SharpeningStoneRestore*2 != RustySwordMaxDurability {
		t.Errorf("%d restore against the rusty blade's %d is not half of it",
			SharpeningStoneRestore, RustySwordMaxDurability)
	}
	if SharpeningStoneRestore*4 != IronSwordMaxDurability {
		t.Errorf("%d restore against the iron blade's %d is not a quarter of it",
			SharpeningStoneRestore, IronSwordMaxDurability)
	}
}

// ---------------------------------------------------------------------------
// What a stone gives back
// ---------------------------------------------------------------------------

// The restore, and the cap that keeps it from inventing durability. The two boundary rows
// are the ones worth having: a blade the stone would overfill stops at its own maximum,
// and a blade worn through comes back rather than being treated as an empty slot.
func TestARepairRestoresTheStonesAmountAndNeverPastTheMaximum(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name  string
		item  ItemID
		start uint16
		want  uint16
	}{
		{"a rusty blade with room for the whole stone", ItemRustySword, 49, 99},
		{"a rusty blade the stone would overfill", ItemRustySword, 60, RustySwordMaxDurability},
		{"a rusty blade one point from full", ItemRustySword, 99, RustySwordMaxDurability},
		{"a rusty blade worn through", ItemRustySword, 0, SharpeningStoneRestore},
		{"an iron blade with room for the whole stone", ItemIronSword, 100, 150},
		{"an iron blade one point from full", ItemIronSword, 199, IronSwordMaxDurability},
		{"an iron blade worn through", ItemIronSword, 0, SharpeningStoneRestore},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			h := newStructureHarness(t)
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			h.stockPack(player, ingredient{ItemSharpeningStone, 1})
			h.equipWorn(player, 1, tc.item, tc.start)

			state, err := h.repair(player, 0, 1)
			if err != nil {
				t.Fatalf("mending item %d at %d durability: %v", tc.item, tc.start, err)
			}

			// The success is the full resend, not an acknowledgement: every slot the
			// server owns, in the shape ServerWelcome announced.
			if got := len(state.Stacks); got != int(protocol.InventorySlots) {
				t.Fatalf("the repair answered with %d slots, want the whole pack's %d",
					got, protocol.InventorySlots)
			}

			mended := state.Stacks[1]
			if mended.Durability != tc.want {
				t.Errorf("the blade is at %d durability, want %d", mended.Durability, tc.want)
			}
			if mended.ItemID != uint16(tc.item) || mended.Count != 1 {
				t.Errorf("the mended slot is %+v, want one item %d", mended, tc.item)
			}
			if mended.MaxDurability == 0 || mended.Durability > mended.MaxDurability {
				t.Errorf("the mended slot is %d/%d, which is not a shape the contract allows",
					mended.Durability, mended.MaxDurability)
			}
		})
	}
}

func TestArmourUsesTheExistingRepairRule(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name    string
		kit     ItemID
		armour  ItemID
		start   uint16
		maximum uint16
		restore uint16
	}{
		{"a leather cap with a leather patch", ItemLeatherPatch, ItemLeatherCap, 40, LeatherArmourMaxDurability, LeatherPatchRestore},
		{"an iron helm with a sharpening stone", ItemSharpeningStone, ItemIronHelm, 90, IronArmourMaxDurability, SharpeningStoneRestore},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			h := newStructureHarness(t)
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			h.stockPack(player, ingredient{tc.kit, 1})
			h.equipWorn(player, 1, tc.armour, tc.start)

			state, err := h.repair(player, 0, 1)
			if err != nil {
				t.Fatalf("repairing item %d: %v", tc.armour, err)
			}
			want := min(tc.start+tc.restore, tc.maximum)
			if got := state.Stacks[1]; got.ItemID != uint16(tc.armour) || got.Count != 1 || got.Durability != want || got.MaxDurability != tc.maximum {
				t.Errorf("mended armour is %+v, want item %d at %d/%d", got, tc.armour, want, tc.maximum)
			}
		})
	}

	h := newStructureHarness(t)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.stockPack(player, ingredient{ItemLeatherPatch, 1})
	h.equipWorn(player, 1, ItemLeatherCap, LeatherArmourMaxDurability)
	before := h.pack(player)
	if _, err := h.repair(player, 0, 1); err == nil {
		t.Fatal("a full-durability armour piece accepted a repair")
	}
	if after := h.pack(player); after != before {
		t.Error("the refused full-durability armour repair changed the pack")
	}
}

// One stone per mend, and the stack that holds it is the only other slot that moves.
func TestARepairSpendsExactlyOneStone(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.stockPack(player, ingredient{ItemSharpeningStone, 3})
	h.equipWorn(player, 1, ItemRustySword, 10)

	state, err := h.repair(player, 0, 1)
	if err != nil {
		t.Fatalf("mending a rusty blade: %v", err)
	}

	kit := state.Stacks[0]
	if kit.ItemID != uint16(ItemSharpeningStone) || kit.Count != 2 {
		t.Errorf("the kit slot is %+v, want 2 sharpening stones left of 3", kit)
	}
	if kit.Durability != 0 || kit.MaxDurability != 0 {
		t.Errorf("the kit slot carries %d/%d durability, want a consumable's (0, 0)",
			kit.Durability, kit.MaxDurability)
	}
	if got := state.Stacks[1].Durability; got != 10+SharpeningStoneRestore {
		t.Errorf("the blade is at %d durability, want %d", got, 10+SharpeningStoneRestore)
	}
	for slot, stack := range state.Stacks[2:] {
		if stack != (protocol.InventoryStack{}) {
			t.Errorf("slot %d is %+v, want empty — a repair touches two slots", slot+2, stack)
		}
	}
}

// The last stone leaves the wire's `(0, 0)` empty slot behind, never a stack of zero
// sharpening stones. The client decodes an item id with no count as a slot that holds
// something it cannot draw.
func TestSpendingTheLastStoneEmptiesItsSlot(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.stockPack(player, ingredient{ItemSharpeningStone, 1})
	h.equipWorn(player, 1, ItemRustySword, 10)

	state, err := h.repair(player, 0, 1)
	if err != nil {
		t.Fatalf("mending a rusty blade: %v", err)
	}

	if got := state.Stacks[0]; got != (protocol.InventoryStack{}) {
		t.Errorf("the emptied kit slot is %+v, want the wire's zero pair", got)
	}
	if got := state.Stacks[1].Durability; got != 10+SharpeningStoneRestore {
		t.Errorf("the blade is at %d durability, want %d", got, 10+SharpeningStoneRestore)
	}
}

// Repair is a field action per GDD §4, and the forge is only where the stones are made.
// Nothing stands anywhere in this world and the player is a long way from the origin.
func TestARepairNeedsNoStationAndNoNeighbours(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.standAt(player, [3]float64{4096.5, 64, -4096.5})
	h.stockPack(player, ingredient{ItemSharpeningStone, 1})
	h.equipWorn(player, 1, ItemRustySword, 10)

	if got := h.sim.StructureCount(); got != 0 {
		t.Fatalf("%d structures stand in the world, want none", got)
	}
	if _, err := h.repair(player, 0, 1); err != nil {
		t.Fatalf("mending a blade in the field: %v", err)
	}
	if got := h.pack(player)[1].durability; got != 10+SharpeningStoneRestore {
		t.Errorf("the blade is at %d durability, want %d", got, 10+SharpeningStoneRestore)
	}
}

// ---------------------------------------------------------------------------
// A blade brought back
// ---------------------------------------------------------------------------

// The point of the feature, asserted through the path that makes a worn blade useless
// rather than through the number behind it: a swing that costs a draugr nothing, a stone,
// and then a swing that costs it the blade's full damage.
//
// A worn-through swing deliberately pays no cooldown either — `resolveSwingLocked` returns
// before setting one when the slot is worth zero damage — so the second swing here is not
// waiting on cadence.
func TestAWornThroughBladeSwingsAgainAfterARepair(t *testing.T) {
	t.Parallel()

	// Yaw 0 looks along -Z, so this draugr is directly ahead and inside reach.
	h, player, id := armedHarness(t, DefaultTickRate, [3]float32{0.5, 64, -1.5})

	player.inventory.mu.Lock()
	player.inventory.slots[0].durability = 0
	player.inventory.slots[1] = stackOf(ItemSharpeningStone, 1)
	player.inventory.mu.Unlock()

	if err := h.swing(player, 0, 1); err != nil {
		t.Fatalf("the swing was refused before it could be judged: %v", err)
	}
	h.step()
	if got := h.mobHealth(id); got != draugrRow.maxHealth {
		t.Fatalf("a blade worn through cost the draugr %d health", draugrRow.maxHealth-got)
	}

	state, err := player.Repair(protocol.RepairRequest{KitSlot: 1, TargetSlot: 0, ClientTick: 2})
	if err != nil {
		t.Fatalf("mending a blade worn through: %v", err)
	}
	if got := state.Stacks[0].Durability; got != SharpeningStoneRestore {
		t.Fatalf("the mended blade is at %d durability, want %d", got, SharpeningStoneRestore)
	}

	if err := h.swing(player, 0, 3); err != nil {
		t.Fatalf("the swing after the repair was refused: %v", err)
	}
	h.step()
	if got := h.mobHealth(id); got != draugrRow.maxHealth-RustySwordDamage {
		t.Errorf("the draugr has %d health, want %d — the mended blade did not cut",
			got, draugrRow.maxHealth-RustySwordDamage)
	}
}

// ---------------------------------------------------------------------------
// Every refusal, and the silence it is answered with
// ---------------------------------------------------------------------------

// Nine ways to be refused, each asserting the same second thing: the pack is bit-identical
// afterwards. There is no rejection payload in V4, so "nothing changed" is the entire
// answer a client gets, and a refusal that half-applied would be invisible until the next
// frame contradicted it.
func TestEveryRefusedRepairLeavesThePackBitIdentical(t *testing.T) {
	t.Parallel()

	// A stone in slot 0 and a worn blade in slot 1 is the shape a legal repair has; each
	// row below breaks exactly one thing about it.
	stocked := func(h *structureHarness, p *Player) {
		h.stockPack(p, ingredient{ItemSharpeningStone, 2})
		h.equipWorn(p, 1, ItemRustySword, 10)
	}

	for _, tc := range []struct {
		name   string
		stock  func(h *structureHarness, p *Player)
		kit    uint8
		target uint8
	}{
		{
			name: "the kit slot is empty",
			stock: func(h *structureHarness, p *Player) {
				h.stockPack(p)
				h.equipWorn(p, 1, ItemRustySword, 10)
			},
			kit: 0, target: 1,
		},
		{
			name: "the kit slot holds a resource",
			stock: func(h *structureHarness, p *Player) {
				h.stockPack(p, ingredient{ItemStone, 8})
				h.equipWorn(p, 1, ItemRustySword, 10)
			},
			kit: 0, target: 1,
		},
		{
			name: "the kit slot holds a blade",
			stock: func(h *structureHarness, p *Player) {
				h.stockPack(p)
				h.equipWorn(p, 0, ItemIronSword, 20)
				h.equipWorn(p, 1, ItemRustySword, 10)
			},
			kit: 0, target: 1,
		},
		{
			name: "the target slot is empty",
			stock: func(h *structureHarness, p *Player) {
				h.stockPack(p, ingredient{ItemSharpeningStone, 2})
			},
			kit: 0, target: 1,
		},
		{
			name: "the target slot holds a resource",
			stock: func(h *structureHarness, p *Player) {
				h.stockPack(p, ingredient{ItemSharpeningStone, 2}, ingredient{ItemRawIron, 4})
			},
			kit: 0, target: 1,
		},
		{
			name: "the target slot holds a structure, which wears out nothing",
			stock: func(h *structureHarness, p *Player) {
				h.stockPack(p, ingredient{ItemSharpeningStone, 2}, ingredient{ItemTent, 1})
			},
			kit: 0, target: 1,
		},
		{
			name: "the target is already at full durability",
			stock: func(h *structureHarness, p *Player) {
				h.stockPack(p, ingredient{ItemSharpeningStone, 2})
				h.equipWorn(p, 1, ItemRustySword, RustySwordMaxDurability)
			},
			kit: 0, target: 1,
		},
		{
			name:  "the kit and the target are one slot",
			stock: stocked,
			kit:   0, target: 0,
		},
		{
			name:  "the kit slot is past the end of the pack",
			stock: stocked,
			kit:   protocol.InventorySlots, target: 1,
		},
		{
			name:  "the target slot is past the end of the pack",
			stock: stocked,
			kit:   0, target: 255,
		},
		{
			name:  "both slots are past the end of the pack",
			stock: stocked,
			kit:   200, target: 201,
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			h := newStructureHarness(t)
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			tc.stock(h, player)

			before := h.pack(player)
			if _, err := h.repair(player, tc.kit, tc.target); err == nil {
				t.Fatal("the repair was applied")
			}
			if after := h.pack(player); after != before {
				t.Errorf("the refused repair changed the pack:\n before %+v\n after  %+v", before, after)
			}
		})
	}
}

// A corpse mends nothing, consistent with mining, editing, placing, attacking and
// crafting. The stone is still in the pack afterwards, which is the half that matters: a
// repair refused after the spend would cost a supply for nothing.
func TestRepairingWhileDeadIsRefused(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.stockPack(player, ingredient{ItemSharpeningStone, 1})
	h.equipWorn(player, 1, ItemRustySword, 10)

	h.sim.mu.Lock()
	player.damageLocked(PlayerMaxHealth)
	h.sim.mu.Unlock()

	before := h.pack(player)
	if _, err := h.repair(player, 0, 1); err == nil {
		t.Fatal("a dead player mended a blade")
	}
	if after := h.pack(player); after != before {
		t.Error("the refused repair changed a dead player's pack")
	}
}

// ---------------------------------------------------------------------------
// The arithmetic, at the boundary a uint16 has
// ---------------------------------------------------------------------------

// restoredBy widens before it adds. A slot near the ceiling of its type plus a restore is
// a sum a uint16 cannot hold, and wrapping there would read as a repair that almost
// worked — the worst shape for an authoritative number to fail in.
func TestRestoringNearTheCeilingOfAUint16DoesNotWrap(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name  string
		stack inventoryStack
		want  uint16
	}{
		{"one point from full", inventoryStack{durability: 99, maxDurability: 100}, 100},
		{"already full", inventoryStack{durability: 100, maxDurability: 100}, 100},
		{"a maximum at the ceiling of the type", inventoryStack{durability: 65535, maxDurability: 65535}, 65535},
		{"a wear that overflows before the cap applies", inventoryStack{durability: 65500, maxDurability: 65535}, 65535},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			if got := restoredBy(tc.stack, SharpeningStoneRestore); got != tc.want {
				t.Errorf("restoredBy(%+v) = %d, want %d", tc.stack, got, tc.want)
			}
		})
	}
}
