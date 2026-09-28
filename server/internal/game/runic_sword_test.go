package game

import (
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func TestKingsHoardItemsAppendTheirIDsAndCapabilities(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		item ItemID
		id   ItemID
		want itemDefinition
	}{
		{ItemKingRune, 47, itemDefinition{places: world.Air, maxStack: 64}},
		{ItemRunicSword, 48, itemDefinition{places: world.Air, maxStack: 1, wornAt: wornMainHand, oneHanded: true, maxDurability: 300, meleeDamage: 50}},
	} {
		if tc.item != tc.id {
			t.Errorf("item id = %d, want %d", tc.item, tc.id)
		}
		got, ok := itemByID(tc.item)
		if !ok || got != tc.want {
			t.Errorf("item %d row = %+v, known=%v; want %+v", tc.item, got, ok, tc.want)
		}
	}
	if RunicSwordDamage != 50 || RunicSwordDamage <= IronSwordDamage {
		t.Errorf("runic damage = %d, iron = %d; want the pinned upgrade to 50", RunicSwordDamage, IronSwordDamage)
	}
	if RunicSwordMaxDurability != 300 || RunicSwordMaxDurability <= IronSwordMaxDurability {
		t.Errorf("runic durability = %d, iron = %d; want the pinned upgrade to 300", RunicSwordMaxDurability, IronSwordMaxDurability)
	}
}

// The actual king death uses the personal-loot path. Every member gets a rune;
// adding that row must not replace the existing sword or bones.
func TestKingDropsOneRuneAlongsideExistingLootForEachPartyMember(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	leader, _ := joinPartyPlayer(t, h, 1, "Runeone", [3]float32{0.5, 64, 0.5})
	second, _ := joinPartyPlayer(t, h, 2, "Runetwo", [3]float32{0.5, 64, 0.5})
	third, _ := joinPartyPlayer(t, h, 3, "Runethree", [3]float32{0.5, 64, 0.5})
	inviteAndAccept(t, leader, second, "Runetwo")
	inviteAndAccept(t, leader, third, "Runethree")
	id := h.placeSpeciesAt(vnet.MobKindDraugrKing, [3]float64{0.5, 64, -3.5})
	h.sim.mu.Lock()
	m := h.sim.mobs[id]
	h.sim.startBossEncounterLocked(m, leader)
	h.sim.damageMobLocked(m, m.health)
	c := h.sim.corpses[id]
	h.sim.mu.Unlock()
	if c == nil || len(c.personal) != 3 {
		t.Fatalf("king corpse = %+v, want three personal containers", c)
	}
	for _, member := range []*Player{leader, second, third} {
		container := c.personal[member.corpseOwner()]
		if container == nil {
			t.Fatal("party member has no personal reward")
		}
		counts := make(map[ItemID]uint16)
		for _, entry := range container.entries {
			counts[entry.stack.item] += entry.stack.count
		}
		if len(container.entries) != 3 || counts[ItemKingRune] != 1 || counts[ItemIronSword] != 1 || counts[ItemBone] < 3 || counts[ItemBone] > 5 || container.silver != 0 {
			t.Errorf("personal king reward = %+v, silver=%d; want rune, sword and 3-5 bones", counts, container.silver)
		}
	}
}

func TestRunicSwordCraftConsumesExactMaterialsAndArrivesWhole(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name string
		worn bool
	}{{"carried iron", false}, {"equipped worn iron", true}} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			h := newStructureHarness(t)
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			h.plantCraftingStation(player, vnet.StructureKindEnchantingTable, [3]int32{0, 63, 0})
			h.stockPack(player, ingredient{ItemIronSword, 1}, ingredient{ItemKingRune, 1})
			if tc.worn {
				player.inventory.mu.Lock()
				player.inventory.slots[equipmentMainHand] = player.inventory.slots[0]
				player.inventory.slots[equipmentMainHand].durability = 1
				player.inventory.slots[0] = inventoryStack{}
				player.inventory.mu.Unlock()
			}
			state, err := h.craft(player, vnet.RecipeIDRunicSword)
			if err != nil {
				t.Fatalf("crafting runic sword: %v", err)
			}
			if heldCount(state, ItemKingRune) != 0 || heldCount(state, ItemIronSword) != 0 || heldCount(state, ItemRunicSword) != 1 {
				t.Fatalf("craft did not exchange exactly the blade and rune: %+v", state.Stacks)
			}
			for _, held := range state.Stacks {
				if held.ItemID == uint16(ItemRunicSword) && (held.Durability != RunicSwordMaxDurability || held.MaxDurability != RunicSwordMaxDurability) {
					t.Errorf("crafted runic sword = %+v, want full durability", held)
				}
			}
			if got := experienceOf(player); got != 10 {
				t.Errorf("craft experience = %d, want 10", got)
			}
		})
	}
}

func TestRunicSwordCraftRefusesMissingIngredientsWithoutSpendingAnything(t *testing.T) {
	t.Parallel()
	for _, ingredientLeft := range []ItemID{ItemIronSword, ItemKingRune} {
		h := newStructureHarness(t)
		player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
		h.plantCraftingStation(player, vnet.StructureKindEnchantingTable, [3]int32{0, 63, 0})
		h.stockPack(player, ingredient{ingredientLeft, 1})
		before := h.pack(player)
		if _, err := h.craft(player, vnet.RecipeIDRunicSword); err == nil {
			t.Fatal("missing ingredient was accepted")
		}
		if after := h.pack(player); after != before {
			t.Error("refused upgrade consumed its other ingredient")
		}
		if got := experienceOf(player); got != 0 {
			t.Errorf("refused upgrade awarded %d experience", got)
		}
	}
}
