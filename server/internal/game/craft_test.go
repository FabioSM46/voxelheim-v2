package game

import (
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// Crafting is an authoritative decision made from state no client supplied: what is in
// the pack, and what stands nearby. Every test here asks what the server decided.

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

// stockPack replaces a player's whole pack with exactly the stacks named.
//
// Replaces rather than adds, because the starter loadout puts a blade in slot 0 and a test
// about materials should say what the pack holds rather than what it holds *besides* that.
func (h *structureHarness) stockPack(p *Player, contents ...ingredient) {
	h.t.Helper()

	p.inventory.mu.Lock()
	defer p.inventory.mu.Unlock()

	p.inventory.slots = slotTable{}
	for slot, held := range contents {
		p.inventory.slots[slot] = stackOf(held.item, held.count)
	}
}

// pack is a copy of a player's authoritative slots, read under the lock that owns them.
//
// A `slotTable` is an array of comparable structs, so two of them compare with `==` — which
// is what lets a transactionality test say "bit-identical" and mean it.
func (h *structureHarness) pack(p *Player) slotTable {
	p.inventory.mu.Lock()
	defer p.inventory.mu.Unlock()
	return p.inventory.slots
}

// plantForge puts a forge at the anchor through the authoritative path, spending slot 0.
//
// Call it before stocking the pack: the placement leaves that slot empty.
func (h *structureHarness) plantForge(p *Player, anchor [3]int32) {
	h.t.Helper()

	h.give(p, 0, ItemForge, 1)
	if _, _, err := p.PlaceStructure(placeRequest(0, anchor, vnet.FacingNorth)); err != nil {
		h.t.Fatalf("planting a forge at %v: %v", anchor, err)
	}
}

// plantCraftingStation stands up the exact station a recipe names. Keeping the switch
// exhaustive and fail-closed prevents a generic recipe sweep from silently planting a
// forge for every future station.
func (h *structureHarness) plantCraftingStation(p *Player, kind vnet.StructureKind, anchor [3]int32) {
	h.t.Helper()

	switch kind {
	case vnet.StructureKindForge:
		h.plantForge(p, anchor)
	case vnet.StructureKindCampfire:
		h.plantCampfire(p, 0, anchor)
	default:
		h.t.Fatalf("no test fixture for crafting station %s", kind)
	}
}

// standAt moves a player to an exact position, the way a test needs rather than the way
// the integrator would.
func (h *structureHarness) standAt(p *Player, pos [3]float64) {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	p.pos = pos
}

func (h *structureHarness) craft(p *Player, id vnet.RecipeID) (protocol.InventoryState, error) {
	return p.Craft(protocol.CraftRequest{Recipe: id, ClientTick: 1})
}

// ---------------------------------------------------------------------------
// The vocabulary the batch pinned
// ---------------------------------------------------------------------------

// The two ids the forge produces, and the registry entries behind them.
//
// Pinned rather than derived, for the reason the structure items are: iota renumbers
// everything after an insertion, and the client mirrors both numbers to draw a held shape.
func TestForgeProductsCarryThePinnedIdsAndTheirOwnStats(t *testing.T) {
	t.Parallel()

	if ItemIronSword != 10 {
		t.Errorf("ItemIronSword = %d, want 10", ItemIronSword)
	}
	if ItemSharpeningStone != 11 {
		t.Errorf("ItemSharpeningStone = %d, want 11", ItemSharpeningStone)
	}

	blade, registered := itemByID(ItemIronSword)
	if !registered {
		t.Fatal("the iron sword is not registered")
	}
	if blade.maxStack != 1 {
		t.Errorf("the iron sword stacks to %d, want 1", blade.maxStack)
	}
	if IronSwordMaxDurability != 200 {
		t.Errorf("IronSwordMaxDurability = %d, want the pinned 200", IronSwordMaxDurability)
	}
	if blade.maxDurability != IronSwordMaxDurability {
		t.Errorf("the iron sword has %d durability, want its own constant %d",
			blade.maxDurability, IronSwordMaxDurability)
	}
	if blade.meleeDamage <= RustySwordDamage {
		t.Errorf("the iron sword does %d damage, want more than the rusty blade's %d",
			blade.meleeDamage, RustySwordDamage)
	}

	stone, registered := itemByID(ItemSharpeningStone)
	if !registered {
		t.Fatal("the sharpening stone is not registered")
	}
	if stone.maxStack != 8 {
		t.Errorf("the sharpening stone stacks to %d, want 8", stone.maxStack)
	}
	if stone.maxDurability != 0 {
		t.Errorf("the sharpening stone has %d durability; spending it is how it is used", stone.maxDurability)
	}
	if stone.meleeDamage != 0 {
		t.Errorf("the sharpening stone does %d melee damage, want none", stone.meleeDamage)
	}
	if _, placeable := blockPlacedBy(ItemSharpeningStone); placeable {
		t.Error("the sharpening stone places a block")
	}
}

// ---------------------------------------------------------------------------
// The recipes
// ---------------------------------------------------------------------------

// Every recipe, twice: once with exactly what it costs, and once one item short of each
// ingredient in turn. The short runs assert the pack is **bit-identical** afterwards, which
// is the transactionality claim stated where it is cheapest to break.
func TestEveryRecipeCraftsWithExactMaterialsAndRefusesOneShort(t *testing.T) {
	t.Parallel()

	for id, r := range recipeTable {
		t.Run(id.String(), func(t *testing.T) {
			t.Parallel()

			// Exactly what it costs, and nothing else in the pack.
			h := newStructureHarness(t)
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			if r.station != vnet.StructureKindUnknown {
				h.plantCraftingStation(player, r.station, [3]int32{0, 63, 0})
			}
			h.stockPack(player, r.ingredients...)

			state, err := h.craft(player, id)
			if err != nil {
				t.Fatalf("crafting %s with exactly its materials: %v", id, err)
			}
			if got := heldCount(state, r.product); got != r.productCount {
				t.Errorf("the pack holds %d of item %d, want the %d this recipe yields",
					got, r.product, r.productCount)
			}
			for _, spent := range r.ingredients {
				if got := heldCount(state, spent.item); got != 0 {
					t.Errorf("%d of ingredient %d survived a craft that needed all of it",
						got, spent.item)
				}
			}

			// One short of each ingredient in turn.
			for short := range r.ingredients {
				shortened := make([]ingredient, len(r.ingredients))
				copy(shortened, r.ingredients)
				shortened[short].count--

				h := newStructureHarness(t)
				player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
				if r.station != vnet.StructureKindUnknown {
					h.plantCraftingStation(player, r.station, [3]int32{0, 63, 0})
				}
				h.stockPack(player, shortened...)
				before := h.pack(player)

				if _, err := h.craft(player, id); err == nil {
					t.Errorf("%s crafted one item %d short", id, r.ingredients[short].item)
				}
				if after := h.pack(player); after != before {
					t.Errorf("%s one item %d short: the refused craft changed the pack",
						id, r.ingredients[short].item)
				}
			}
		})
	}
}

// The recipe table is the nineteen this branch owns, with the costs they agreed on. A
// balance pass edits this test and the table together; a typo edits only one of them.
func TestTheRecipeTableIsTheNineteenAgreedRecipes(t *testing.T) {
	t.Parallel()

	want := map[vnet.RecipeID]recipe{
		vnet.RecipeIDForge: {
			ingredients: []ingredient{{ItemStone, 8}, {ItemRawCoal, 2}},
			product:     ItemForge, productCount: 1,
		},
		vnet.RecipeIDTent: {
			ingredients: []ingredient{{ItemLog, 8}},
			product:     ItemTent, productCount: 1,
		},
		vnet.RecipeIDIronSword: {
			ingredients: []ingredient{{ItemRawIron, 3}, {ItemRawCoal, 2}, {ItemLog, 1}},
			product:     ItemIronSword, productCount: 1, station: vnet.StructureKindForge, experience: 10,
		},
		vnet.RecipeIDSharpeningStone: {
			ingredients: []ingredient{{ItemStone, 2}, {ItemRawCoal, 1}},
			product:     ItemSharpeningStone, productCount: 1, station: vnet.StructureKindForge, experience: 10,
		},
		vnet.RecipeIDCampfire: {
			ingredients: []ingredient{{ItemLog, 4}, {ItemRawCoal, 1}},
			product:     ItemCampfire, productCount: 1,
		},
		vnet.RecipeIDLeatherPatch: {
			ingredients: []ingredient{{ItemVargrPelt, 2}},
			product:     ItemLeatherPatch, productCount: 1,
		},

		// The three implements, added by #185. One price, three times: that issue ruled
		// out tiers, so a difference between them would be a ladder nobody chose. Cheaper
		// than the blade the same forge makes, which is the ordering the recipe table's
		// own comment argues for.
		vnet.RecipeIDShovel: {
			ingredients: []ingredient{{ItemRawIron, 1}, {ItemLog, 2}},
			product:     ItemShovel, productCount: 1, station: vnet.StructureKindForge, experience: 10,
		},
		vnet.RecipeIDPickaxe: {
			ingredients: []ingredient{{ItemRawIron, 1}, {ItemLog, 2}},
			product:     ItemPickaxe, productCount: 1, station: vnet.StructureKindForge, experience: 10,
		},
		vnet.RecipeIDAxe: {
			ingredients: []ingredient{{ItemRawIron, 1}, {ItemLog, 2}},
			product:     ItemAxe, productCount: 1, station: vnet.StructureKindForge, experience: 10,
		},
		vnet.RecipeIDCookedMeat: {
			ingredients: []ingredient{{ItemRawMeat, 1}},
			product:     ItemCookedMeat, productCount: 1, station: vnet.StructureKindCampfire, experience: 3,
		},
		vnet.RecipeIDLeatherCap: {
			ingredients: []ingredient{{ItemVargrPelt, 3}}, product: ItemLeatherCap, productCount: 1,
		},
		vnet.RecipeIDLeatherJerkin: {
			ingredients: []ingredient{{ItemVargrPelt, 5}}, product: ItemLeatherJerkin, productCount: 1,
		},
		vnet.RecipeIDLeatherLeggings: {
			ingredients: []ingredient{{ItemVargrPelt, 4}}, product: ItemLeatherLeggings, productCount: 1,
		},
		vnet.RecipeIDIronHelm: {
			ingredients: []ingredient{{ItemRawIron, 3}, {ItemRawCoal, 1}}, product: ItemIronHelm, productCount: 1,
			station: vnet.StructureKindForge, experience: 10,
		},
		vnet.RecipeIDIronCuirass: {
			ingredients: []ingredient{{ItemRawIron, 5}, {ItemRawCoal, 2}}, product: ItemIronCuirass, productCount: 1,
			station: vnet.StructureKindForge, experience: 10,
		},
		vnet.RecipeIDIronGreaves: {
			ingredients: []ingredient{{ItemRawIron, 4}, {ItemRawCoal, 2}}, product: ItemIronGreaves, productCount: 1,
			station: vnet.StructureKindForge, experience: 10,
		},
		vnet.RecipeIDWoodenShield: {
			ingredients: []ingredient{{ItemLog, 6}, {ItemVargrPelt, 2}}, product: ItemWoodenShield, productCount: 1,
		},
		vnet.RecipeIDBow: {
			ingredients: []ingredient{{ItemLog, 3}, {ItemVargrPelt, 2}}, product: ItemBow, productCount: 1,
		},
		vnet.RecipeIDArrows: {
			ingredients: []ingredient{{ItemLog, 1}, {ItemBone, 1}}, product: ItemArrow, productCount: 4,
		},
	}

	if len(recipeTable) != len(want) {
		t.Fatalf("the table holds %d recipes, want %d — a new one needs a decision, not a test edit",
			len(recipeTable), len(want))
	}
	// The absent key is the load-bearing one: FlatBuffers decodes a missing recipe as
	// Unknown, and a table entry for it would craft something nobody asked for.
	if _, present := recipeTable[vnet.RecipeIDUnknown]; present {
		t.Error("RecipeID.Unknown has a recipe, which is the absent-field case crafting something")
	}

	for id, expected := range want {
		got, known := recipeTable[id]
		if !known {
			t.Errorf("%s has no recipe", id)
			continue
		}
		if got.product != expected.product || got.productCount != expected.productCount {
			t.Errorf("%s yields %d of item %d, want %d of item %d",
				id, got.productCount, got.product, expected.productCount, expected.product)
		}
		if got.station != expected.station {
			t.Errorf("%s needs station %s, want %s", id, got.station, expected.station)
		}
		if got.experience != expected.experience {
			t.Errorf("%s awards %d experience, want %d", id, got.experience, expected.experience)
		}
		if len(got.ingredients) != len(expected.ingredients) {
			t.Errorf("%s costs %d ingredients, want %d", id, len(got.ingredients), len(expected.ingredients))
			continue
		}
		for i, line := range expected.ingredients {
			if got.ingredients[i] != line {
				t.Errorf("%s ingredient %d is %d of item %d, want %d of item %d",
					id, i, got.ingredients[i].count, got.ingredients[i].item, line.count, line.item)
			}
		}
	}
}

// A station is the boundary between assembly and progression: every forge or campfire
// recipe earns experience, while every recipe made by hand earns none. The iff makes a
// new station recipe with a forgotten reward and a hand recipe with an accidental one
// fail the same sweep.
func TestOnlyStationRecipesAwardExperience(t *testing.T) {
	t.Parallel()

	for id, r := range recipeTable {
		hasStation := r.station != vnet.StructureKindUnknown
		hasExperience := r.experience > 0
		if hasStation != hasExperience {
			t.Errorf("%s station=%s experience=%d; want experience > 0 iff it needs a station",
				id, r.station, r.experience)
		}
	}
}

func TestStationCraftAwardsExperienceAndHandCraftDoesNot(t *testing.T) {
	t.Parallel()

	for name, tc := range map[string]struct {
		id   vnet.RecipeID
		want uint32
	}{
		"at the forge": {id: vnet.RecipeIDIronSword, want: 10},
		"by hand":      {id: vnet.RecipeIDTent, want: 0},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			h := newStructureHarness(t)
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			r := recipeTable[tc.id]
			if r.station != vnet.StructureKindUnknown {
				h.plantCraftingStation(player, r.station, [3]int32{0, 63, 0})
			}
			h.stockPack(player, r.ingredients...)
			if _, err := h.craft(player, tc.id); err != nil {
				t.Fatalf("Craft: %v", err)
			}
			if got := experienceOf(player); got != tc.want {
				t.Errorf("%s awarded %d experience, want %d", tc.id, got, tc.want)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// The forge
// ---------------------------------------------------------------------------

// A forge-required recipe is accepted just inside the radius and refused just outside it,
// and the boundary is the same one on both sides.
//
// The distance is purely vertical so the test states one number rather than a triangle:
// reach is measured from the body centre, which is PlayerHeight/2 above the feet, to the
// centre of the anchor voxel.
func TestAForgeRecipeIsAcceptedInsideTheRadiusAndRefusedOutsideIt(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name     string
		distance float64
		accepted bool
	}{
		{"just inside", 4.9, true},
		{"just outside", 5.1, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			h := newStructureHarness(t)
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			h.plantForge(player, [3]int32{0, 63, 0})
			h.stockPack(player, ingredient{ItemStone, 2}, ingredient{ItemRawCoal, 1})

			// The anchor voxel's centre is at y 63.5, and the body centre sits
			// PlayerHeight/2 above the feet.
			h.standAt(player, [3]float64{0.5, 63.5 + tc.distance - PlayerHeight/2, 0.5})

			_, err := h.craft(player, vnet.RecipeIDSharpeningStone)
			if tc.accepted && err != nil {
				t.Errorf("a craft %.1f blocks from the forge was refused: %v", tc.distance, err)
			}
			if !tc.accepted && err == nil {
				t.Errorf("a craft %.1f blocks from the forge was accepted", tc.distance)
			}
		})
	}
}

// Any player may work at any forge. Ownership exists for removal and respawn; a camp
// somebody else built being useful is the cooperative half of a cooperative game.
func TestAForgeSomebodyElseBuiltStillWorks(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	builder, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	visitor, _ := h.join(2, [3]float32{0.5, 64, 0.5})

	h.plantForge(builder, [3]int32{0, 63, 0})
	h.stockPack(visitor, ingredient{ItemStone, 2}, ingredient{ItemRawCoal, 1})

	state, err := h.craft(visitor, vnet.RecipeIDSharpeningStone)
	if err != nil {
		t.Fatalf("crafting at another player's forge: %v", err)
	}
	if got := heldCount(state, ItemSharpeningStone); got != 1 {
		t.Errorf("the visitor holds %d stones, want 1", got)
	}
}

// Cooking uses the campfire's own five-block radius and no other station. The pack is
// compared on every refusal so being out of range cannot consume the raw piece.
func TestCookingRequiresACampfireInsideItsOwnRadius(t *testing.T) {
	t.Parallel()

	if CampfireCookRadius != 5.0 {
		t.Fatalf("CampfireCookRadius = %.1f, want the pinned 5.0", CampfireCookRadius)
	}
	if CampfireCookRadius == CampfireSafeRadius {
		t.Fatal("the cooking radius reuses the spawn-safe radius")
	}
	for _, tc := range []struct {
		name     string
		station  vnet.StructureKind
		distance float64
		accepted bool
	}{
		{"no station", vnet.StructureKindUnknown, 0, false},
		{"wrong station", vnet.StructureKindForge, 4.9, false},
		{"just inside", vnet.StructureKindCampfire, 4.9, true},
		{"just outside", vnet.StructureKindCampfire, 5.1, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			h := newStructureHarness(t)
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			if tc.station != vnet.StructureKindUnknown {
				h.plantCraftingStation(player, tc.station, [3]int32{0, 63, 0})
			}
			h.stockPack(player, ingredient{ItemRawMeat, 1})
			before := h.pack(player)
			h.standAt(player, [3]float64{0.5, 63.5 + tc.distance - PlayerHeight/2, 0.5})

			state, err := h.craft(player, vnet.RecipeIDCookedMeat)
			if tc.accepted {
				if err != nil {
					t.Fatalf("cooking %.1f blocks from %s: %v", tc.distance, tc.station, err)
				}
				if got := heldCount(state, ItemCookedMeat); got != 1 {
					t.Errorf("the pack holds %d cooked meat, want 1", got)
				}
				return
			}
			if err == nil {
				t.Fatalf("cooking %.1f blocks from %s was accepted", tc.distance, tc.station)
			}
			if after := h.pack(player); after != before {
				t.Error("the refused cook changed the pack")
			}
		})
	}
}

func TestACampfireSomebodyElseBuiltStillCooks(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	builder, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	visitor, _ := h.join(2, [3]float32{0.5, 64, 0.5})
	h.plantCampfire(builder, 0, [3]int32{0, 63, 0})
	h.stockPack(visitor, ingredient{ItemRawMeat, 1})

	state, err := h.craft(visitor, vnet.RecipeIDCookedMeat)
	if err != nil {
		t.Fatalf("cooking at another player's campfire: %v", err)
	}
	if got := heldCount(state, ItemCookedMeat); got != 1 {
		t.Errorf("the visitor holds %d cooked meat, want 1", got)
	}
}

func TestCraftingStationRadiiFailClosed(t *testing.T) {
	t.Parallel()

	for _, kind := range []vnet.StructureKind{
		vnet.StructureKindUnknown,
		vnet.StructureKindTent,
		vnet.StructureKind(200),
	} {
		if radius, configured := craftRadius(kind); configured || radius != 0 {
			t.Errorf("station %s resolved to radius %.1f (configured %v), want fail-closed", kind, radius, configured)
		}
	}
}

// A station-less recipe needs nothing built, which is what makes the chain startable: the
// forge is the thing you make before you have one.
func TestTheStationlessRecipesNeedNothingBuilt(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		id      vnet.RecipeID
		product ItemID
		count   uint16
	}{
		{vnet.RecipeIDForge, ItemForge, 1},
		{vnet.RecipeIDTent, ItemTent, 1},
		{vnet.RecipeIDCampfire, ItemCampfire, 1},
		// The fourth, and the one whose absent station is a design decision rather than a
		// bootstrapping one: a patch is mended-in-the-field kit, so a forge requirement
		// would mean walking home to make the thing that saves the walk.
		{vnet.RecipeIDLeatherPatch, ItemLeatherPatch, 1},
		{vnet.RecipeIDLeatherCap, ItemLeatherCap, 1},
		{vnet.RecipeIDLeatherJerkin, ItemLeatherJerkin, 1},
		{vnet.RecipeIDLeatherLeggings, ItemLeatherLeggings, 1},
		{vnet.RecipeIDBow, ItemBow, 1},
		{vnet.RecipeIDArrows, ItemArrow, 4},
	} {
		h := newStructureHarness(t)
		player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
		h.stockPack(player, recipeTable[tc.id].ingredients...)

		state, err := h.craft(player, tc.id)
		if err != nil {
			t.Fatalf("%s with nothing built: %v", tc.id, err)
		}
		if got := heldCount(state, tc.product); got != tc.count {
			t.Errorf("%s yielded %d of item %d, want %d", tc.id, got, tc.product, tc.count)
		}
	}
	// And the forge-required ones are not: the same materials, no forge, no craft.
	h := newStructureHarness(t)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.stockPack(player, recipeTable[vnet.RecipeIDSharpeningStone].ingredients...)
	if _, err := h.craft(player, vnet.RecipeIDSharpeningStone); err == nil {
		t.Error("a forge-required recipe was crafted with no forge in the world")
	}
}

// ---------------------------------------------------------------------------
// Transactionality
// ---------------------------------------------------------------------------

// A pack with no room for the output crafts nothing and keeps everything, including the
// materials the craft would have spent.
//
// The stacks are chosen so the spend frees no slot: eight stone comes out of a stack of
// sixty-four and two coal out of another, so both slots are still occupied afterwards and
// the forge — one to a slot, mergeable with nothing — has nowhere to go.
func TestACraftWithNoRoomForItsOutputChangesNothing(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	full := []ingredient{{ItemStone, 64}, {ItemRawCoal, 64}}
	for range equipmentFirst - len(full) {
		full = append(full, ingredient{ItemDirt, 64})
	}
	h.stockPack(player, full...)
	before := h.pack(player)

	if _, err := h.craft(player, vnet.RecipeIDForge); err == nil {
		t.Fatal("a craft with nowhere to put its output was accepted")
	}
	if after := h.pack(player); after != before {
		t.Error("the refused craft spent materials it had nowhere to put the product of")
	}
}

// The case the scratch copy exists for: a full pack whose *own ingredients* free the slot
// the product needs.
//
// Eight logs in the only free-able slot and thirty-five full stacks of dirt behind them. A
// room check performed before the spend would refuse this, and refusing it is a bug rather
// than a rule anybody chose — the tent has somewhere to go the moment the logs leave.
func TestACraftThatEmptiesASlotHasRoomForItsOwnOutput(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	contents := []ingredient{{ItemLog, 8}}
	for range equipmentFirst - 1 {
		contents = append(contents, ingredient{ItemDirt, 64})
	}
	h.stockPack(player, contents...)

	state, err := h.craft(player, vnet.RecipeIDTent)
	if err != nil {
		t.Fatalf("crafting a tent out of the only slot that could hold it: %v", err)
	}
	if got := heldCount(state, ItemTent); got != 1 {
		t.Errorf("the pack holds %d tents, want 1", got)
	}
	if got := heldCount(state, ItemLog); got != 0 {
		t.Errorf("%d logs survived the craft that spent all of them", got)
	}
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

// A recipe this server does not know is silence, and the absent-field zero is the case
// that matters: FlatBuffers decodes a missing scalar as Unknown, so a client that sends no
// recipe must not craft the first one in the table.
func TestAnUnknownRecipeIsRefusedAndCostsNothing(t *testing.T) {
	t.Parallel()

	for _, id := range []vnet.RecipeID{vnet.RecipeIDUnknown, vnet.RecipeID(200)} {
		h := newStructureHarness(t)
		player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
		h.stockPack(player, ingredient{ItemStone, 64}, ingredient{ItemRawCoal, 64}, ingredient{ItemLog, 64})
		before := h.pack(player)

		if _, err := h.craft(player, id); err == nil {
			t.Errorf("recipe %s was crafted", id)
		}
		if after := h.pack(player); after != before {
			t.Errorf("recipe %s changed the pack", id)
		}
	}
}

// A corpse crafts nothing, consistent with mining, editing, placing and attacking.
func TestCraftingWhileDeadIsRefused(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.stockPack(player, ingredient{ItemLog, 8})

	h.sim.mu.Lock()
	player.damageLocked(PlayerMaxHealth)
	h.sim.mu.Unlock()

	before := h.pack(player)
	if _, err := h.craft(player, vnet.RecipeIDTent); err == nil {
		t.Fatal("a dead player crafted a tent")
	}
	if after := h.pack(player); after != before {
		t.Error("the refused craft changed a dead player's pack")
	}
}

// ---------------------------------------------------------------------------
// What the blade is worth
// ---------------------------------------------------------------------------

// A crafted blade arrives at full durability, and it arrives there because `stackOf` reads
// the registry rather than because the craft remembered to say so.
func TestACraftedIronSwordArrivesAtFullDurability(t *testing.T) {
	t.Parallel()

	h := newStructureHarness(t)
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	h.plantForge(player, [3]int32{0, 63, 0})
	h.stockPack(player, recipeTable[vnet.RecipeIDIronSword].ingredients...)

	state, err := h.craft(player, vnet.RecipeIDIronSword)
	if err != nil {
		t.Fatalf("forging an iron sword: %v", err)
	}

	var found bool
	for slot, stack := range state.Stacks {
		if stack.ItemID != uint16(ItemIronSword) {
			continue
		}
		found = true
		if stack.Count != 1 {
			t.Errorf("slot %d holds %d iron swords; a durable item is one whole item", slot, stack.Count)
		}
		if stack.Durability != IronSwordMaxDurability || stack.MaxDurability != IronSwordMaxDurability {
			t.Errorf("slot %d is %d/%d durability, want a full %d",
				slot, stack.Durability, stack.MaxDurability, IronSwordMaxDurability)
		}
	}
	if !found {
		t.Fatal("the craft produced no iron sword")
	}
}

func TestCraftedArmourArrivesWholeInThePackAndMovesToItsMatchingSlot(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name    string
		recipe  vnet.RecipeID
		item    ItemID
		maximum uint16
		slot    int
	}{
		{"leather cap", vnet.RecipeIDLeatherCap, ItemLeatherCap, LeatherArmourMaxDurability, equipmentHead},
		{"leather jerkin", vnet.RecipeIDLeatherJerkin, ItemLeatherJerkin, LeatherArmourMaxDurability, equipmentChest},
		{"leather leggings", vnet.RecipeIDLeatherLeggings, ItemLeatherLeggings, LeatherArmourMaxDurability, equipmentLegs},
		{"iron helm", vnet.RecipeIDIronHelm, ItemIronHelm, IronArmourMaxDurability, equipmentHead},
		{"iron cuirass", vnet.RecipeIDIronCuirass, ItemIronCuirass, IronArmourMaxDurability, equipmentChest},
		{"iron greaves", vnet.RecipeIDIronGreaves, ItemIronGreaves, IronArmourMaxDurability, equipmentLegs},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			h := newStructureHarness(t)
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			r := recipeTable[tc.recipe]
			if r.station != vnet.StructureKindUnknown {
				h.plantCraftingStation(player, r.station, [3]int32{0, 63, 0})
			}
			h.stockPack(player, r.ingredients...)

			state, err := h.craft(player, tc.recipe)
			if err != nil {
				t.Fatalf("crafting %s: %v", tc.recipe, err)
			}
			from := -1
			for slot, stack := range state.Stacks {
				if stack.ItemID != uint16(tc.item) {
					continue
				}
				if slot >= equipmentFirst {
					t.Errorf("crafted armour landed directly in equipment slot %d", slot)
				}
				if stack.Count != 1 || stack.Durability != tc.maximum || stack.MaxDurability != tc.maximum {
					t.Errorf("crafted armour in slot %d is %+v, want one whole item at %d/%d", slot, stack, tc.maximum, tc.maximum)
				}
				from = slot
			}
			if from < 0 {
				t.Fatal("the craft produced no armour in the pack")
			}

			moved, err := player.MoveInventory(protocol.InventoryMoveRequest{From: uint8(from), To: uint8(tc.slot), Count: 1})
			if err != nil {
				t.Fatalf("moving the crafted armour to its matching slot: %v", err)
			}
			if got := moved.Stacks[tc.slot]; got.ItemID != uint16(tc.item) || got.Count != 1 || got.Durability != tc.maximum || got.MaxDurability != tc.maximum {
				t.Errorf("matching equipment slot holds %+v", got)
			}
		})
	}
}

// The iron sword's damage participates in combat resolution, and the rusty one's still
// does. Both in one test, because the claim is a *difference*: a per-item damage that
// happened to equal the old constant would pass either half alone.
func TestEachBladeDoesItsOwnDamage(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name string
		item ItemID
		want uint16
	}{
		{"the starter blade", ItemRustySword, RustySwordDamage},
		{"the iron blade", ItemIronSword, IronSwordDamage},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			// Yaw 0 looks along -Z, so this draugr is directly ahead and inside reach.
			h, player, id := armedHarness(t, DefaultTickRate, [3]float32{0.5, 64, -1.5})

			player.inventory.mu.Lock()
			player.inventory.slots[0] = stackOf(tc.item, 1)
			player.inventory.mu.Unlock()

			if err := h.swing(player, 0, 1); err != nil {
				t.Fatalf("Attack: %v", err)
			}
			h.step()

			if got := h.mobHealth(id); got != draugrRow.maxHealth-tc.want {
				t.Errorf("the draugr has %d health, want %d after a hit for %d",
					got, draugrRow.maxHealth-tc.want, tc.want)
			}
		})
	}
}

// Two iron swings kill a draugr where three rusty ones do. The upgrade is a step in what
// combat *costs*, and that is the number a balance pass is actually choosing.
func TestTheIronBladeKillsADraugrInTwoSwings(t *testing.T) {
	t.Parallel()

	if IronSwordDamage*2 < draugrRow.maxHealth {
		t.Errorf("two iron swings do %d against %d health, want at least the draugr's whole bar",
			IronSwordDamage*2, draugrRow.maxHealth)
	}
	if RustySwordDamage*2 >= draugrRow.maxHealth {
		t.Errorf("two rusty swings already do %d against %d health, so the iron blade changes nothing",
			RustySwordDamage*2, draugrRow.maxHealth)
	}
}
