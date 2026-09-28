package main

import (
	"context"
	"errors"
	"fmt"
	"reflect"
	"slices"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The two overworld placements are disclosed travel setup, after a legitimate exit.
// They neither cross a dungeon gate nor alter inventory or progression.
func (r *runner) placeForTravel(ctx context.Context, label string, spot cell) error {
	if r.c.self().world != 0 {
		return errors.New("overworld travel placement attempted inside dungeon")
	}
	r.c.stand()
	if _, err := r.c.command(ctx, fmt.Sprintf("/teleport %d %d %d", spot[0], spot[1], spot[2])); err != nil {
		return err
	}
	if err := r.waitHoard(ctx, label, func() bool { self := r.c.self(); return self.have && near(spot, 2)(feetCell(self.pos)) }); err != nil {
		return err
	}
	r.hoard.Notes = append(r.hoard.Notes, fmt.Sprintf("travel placement: %s at %v", label, spot))
	return sleep(ctx, 2*time.Second)
}

func (pt *party) finishHoard(ctx context.Context) error {
	for _, r := range pt.members {
		state, _ := r.c.inventoryAnswer()
		if r.c.self().world != 0 || !r.hoard.KingDone || countItem(state, game.ItemKingRune) != 1 {
			return fmt.Errorf("%s did not carry its earned rune out", r.c.name)
		}
		for _, done := range r.hoard.ChestDone {
			if !done {
				return errors.New("a personal chest assertion was skipped")
			}
		}
		var earned uint32
		for _, chest := range r.hoard.Chests {
			earned += chest.Silver
		}
		if state.Silver != earned || r.hoard.King.Silver != 0 {
			return errors.New("exit purse is not exactly the three earned chest credits")
		}
		r.hoard.Notes = append(r.hoard.Notes, "earned rune carried out through return portal")
		r.hoard.RuneOut = true
	}
	lead := pt.leaderRunner()
	if err := lead.placeAndEnchant(ctx); err != nil {
		return err
	}
	repairer, slot, ok := pt.cheapestRepair()
	if !ok {
		// A real chest kit may reduce genuine accumulated wear. It is never granted here.
		for _, r := range pt.members {
			if err := r.useEarnedKit(ctx); err != nil {
				return err
			}
			if repairer, slot, ok = pt.cheapestRepair(); ok {
				break
			}
		}
	}
	if !ok {
		return errors.New("no genuinely worn item is affordable with earned silver after available chest kits")
	}
	return repairer.paidRepair(ctx, slot)
}

func (r *runner) placeAndEnchant(ctx context.Context) error {
	r.hoard.Stage = "table fixture and crafting"
	capital := world.CapitalAt(r.opts.seed)
	x, z := capital.CentreX+int64(capital.Radius)+96, capital.CentreZ
	spot := cell{x, int64(world.HeightAt(r.opts.seed, x, z)) + 1, z}
	if _, warded := world.SettlementWarding(r.opts.seed, world.Column{CX: world.ChunkOf(x, 0, z).X, CZ: world.ChunkOf(x, 0, z).Z}); warded {
		return errors.New("table setup site is settlement-warded")
	}
	if err := r.placeForTravel(ctx, "unwarded enchanting site", spot); err != nil {
		return err
	}
	before, revision := r.c.inventoryAnswer()
	if countItem(before, game.ItemEnchantingTable) != 0 {
		return errors.New("unexpected pre-existing table item")
	}
	if _, err := r.c.command(ctx, fmt.Sprintf("/additem %d 1", game.ItemEnchantingTable)); err != nil {
		return err
	}
	stocked, err := r.inventoryAfter(ctx, revision, func(s protocol.InventoryState) bool { return countItem(s, game.ItemEnchantingTable) == 1 })
	if err != nil {
		return err
	}
	if stocked.Silver != before.Silver || countItem(stocked, game.ItemKingRune) != countItem(before, game.ItemKingRune) || countItem(stocked, game.ItemIronSword) != countItem(before, game.ItemIronSword) {
		return errors.New("table setup altered earned inputs or currency")
	}
	r.hoard.Notes = append(r.hoard.Notes, "setup fixture: exactly one EnchantingTable granted to leader; no reagent, sword or silver grants")
	slot, ok := itemSlot(stocked, game.ItemEnchantingTable)
	if !ok {
		return errors.New("table fixture absent")
	}
	var ground cell
	found := false
	here := feetCell(r.c.self().pos)
	// Use only delivered ground and headroom, and keep the body outside the one-cell table.
	r.c.withView(func(v *blockView) {
		for _, offset := range []cell{{2, 0, 0}, {-2, 0, 0}, {0, 0, 2}, {0, 0, -2}, {2, 0, 1}, {-2, 0, 1}} {
			candidate := cell{here[0] + offset[0], here[1] - 1, here[2] + offset[2]}
			base, known := v.block(candidate[0], candidate[1], candidate[2])
			a, aKnown := v.block(candidate[0], candidate[1]+1, candidate[2])
			b, bKnown := v.block(candidate[0], candidate[1]+2, candidate[2])
			if known && aKnown && bKnown && world.Solid(base) && a == world.Air && b == world.Air {
				ground, found = candidate, true
				return
			}
		}
	})
	if !found {
		return errors.New("no delivered supported table footprint beside setup site")
	}
	_, revision = r.c.inventoryAnswer()
	r.c.takeLootEvents()
	if err := r.c.send(protocol.EncodePlaceStructureRequest(protocol.PlaceStructureRequest{Slot: slot, HasAnchor: true, Anchor: toWire(ground), Facing: vnet.FacingNorth, ClientTick: r.c.tick()})); err != nil {
		return err
	}
	placed, err := r.inventoryAfter(ctx, revision, func(s protocol.InventoryState) bool { return countItem(s, game.ItemEnchantingTable) == 0 })
	if err != nil {
		return err
	}
	if placed.Silver != stocked.Silver {
		return errors.New("placement unexpectedly spent silver")
	}
	if err := r.waitHoard(ctx, "placed enchanting table", func() bool {
		r.c.mu.Lock()
		defer r.c.mu.Unlock()
		for _, s := range r.c.structures {
			if s.Kind == vnet.StructureKindEnchantingTable && s.Anchor == toWire(ground) {
				return true
			}
		}
		return false
	}); err != nil {
		return err
	}
	// The only pack iron blade is the earned king drop; bootstrap iron is still equipped.
	ironSlot, ok := itemSlot(placed, game.ItemIronSword)
	if !ok || ironSlot >= uint8(protocol.InventorySlots-protocol.EquipmentSlots) {
		return errors.New("earned iron blade is not in the pack ahead of equipped bootstrap blade")
	}
	if countItem(placed, game.ItemKingRune) != 1 || countItem(placed, game.ItemRunicSword) != 0 {
		return errors.New("unexpected pre-craft reagent/product count")
	}
	_, revision = r.c.inventoryAnswer()
	if err := r.c.send(protocol.EncodeCraftRequest(protocol.CraftRequest{Recipe: vnet.RecipeIDRunicSword, ClientTick: r.c.tick()})); err != nil {
		return err
	}
	crafted, err := r.inventoryAfter(ctx, revision, func(s protocol.InventoryState) bool {
		return countItem(s, game.ItemRunicSword) == 1 && countItem(s, game.ItemKingRune) == 0 && countItem(s, game.ItemIronSword)+1 == countItem(placed, game.ItemIronSword)
	})
	if err != nil {
		return err
	}
	if crafted.Stacks[ironSlot].ItemID == uint16(game.ItemIronSword) || crafted.Silver != placed.Silver || crafted.Stacks[mainHandSlot] != placed.Stacks[mainHandSlot] {
		return errors.New("craft did not consume earned pack blade while preserving bootstrap equipment/purse")
	}
	r.hoard.Notes = append(r.hoard.Notes, fmt.Sprintf("table placed at %v and consumed fixture; crafted RunicSword from earned IronSword slot %d + KingRune; bootstrap main hand unchanged", ground, ironSlot))
	r.hoard.Crafted = true
	return nil
}

func repairPrice(stack protocol.InventoryStack) uint32 {
	if stack.Count == 0 || stack.MaxDurability == 0 || stack.Durability >= stack.MaxDurability {
		return 0
	}
	return max(uint32(1), uint32(stack.MaxDurability-stack.Durability)*game.RepairSilverPerPoint)
}

func (pt *party) cheapestRepair() (*runner, uint16, bool) {
	var best *runner
	var bestSlot uint16
	var price uint32
	for _, r := range pt.members {
		state, _ := r.c.inventoryAnswer()
		for slot, stack := range state.Stacks {
			cost := repairPrice(stack)
			if cost > 0 && cost <= state.Silver && (best == nil || cost < price) {
				best, bestSlot, price = r, uint16(slot), cost
			}
		}
	}
	return best, bestSlot, best != nil
}

func (r *runner) useEarnedKit(ctx context.Context) error {
	state, revision := r.c.inventoryAnswer()
	var kit uint8
	var restore uint16
	found := false
	for _, candidate := range []struct {
		item    game.ItemID
		restore uint16
	}{{game.ItemSharpeningStone, game.SharpeningStoneRestore}, {game.ItemLeatherPatch, game.LeatherPatchRestore}} {
		if slot, ok := itemSlot(state, candidate.item); ok {
			kit, restore, found = slot, candidate.restore, true
			break
		}
	}
	if !found {
		return nil
	}
	target := -1
	var remainingPrice uint32
	for slot, stack := range state.Stacks {
		if stack.Count > 0 && uint32(stack.MaxDurability) > uint32(stack.Durability)+uint32(restore) && repairPrice(stack) > state.Silver {
			mended := stack
			mended.Durability += restore
			price := repairPrice(mended)
			if price <= state.Silver && (target < 0 || price < remainingPrice) {
				target, remainingPrice = slot, price
			}
		}
	}
	if target < 0 {
		return nil
	}
	if err := r.c.send(protocol.EncodeRepairRequest(protocol.RepairRequest{KitSlot: kit, TargetSlot: uint8(target), ClientTick: r.c.tick()})); err != nil {
		return err
	}
	after, err := r.inventoryAfter(ctx, revision, func(s protocol.InventoryState) bool {
		return s.Stacks[target].Durability == state.Stacks[target].Durability+restore
	})
	if err != nil {
		return err
	}
	if after.Silver != state.Silver || countItem(after, game.ItemID(state.Stacks[kit].ItemID))+1 != countItem(state, game.ItemID(state.Stacks[kit].ItemID)) {
		return errors.New("earned kit consumption mismatch")
	}
	r.hoard.Notes = append(r.hoard.Notes, fmt.Sprintf("earned chest kit %d used on slot %d: durability %d -> %d, silver unchanged %d", state.Stacks[kit].ItemID, target, state.Stacks[target].Durability, after.Stacks[target].Durability, after.Silver))
	return nil
}

func (r *runner) paidRepair(ctx context.Context, slot uint16) error {
	r.hoard.Stage = "capital paid repair"
	var forge world.PlacedAnchor
	found := false
	for _, a := range world.CapitalAt(r.opts.seed).Anchors() {
		if a.Kind == world.AnchorForge {
			forge, found = a, true
			break
		}
	}
	if !found {
		return errors.New("capital has no forge anchor")
	}
	// World anchors name occupied cells; the structure rests on the voxel below.
	ground := cell{forge.X, forge.Y - 1, forge.Z}
	spot := cell{forge.X + 2, forge.Y, forge.Z}
	if err := r.placeForTravel(ctx, "capital forge", spot); err != nil {
		return err
	}
	if err := r.waitHoard(ctx, "capital forge snapshot", func() bool {
		r.c.mu.Lock()
		defer r.c.mu.Unlock()
		for _, s := range r.c.structures {
			if s.Kind == vnet.StructureKindForge && s.Anchor == toWire(ground) {
				return true
			}
		}
		return false
	}); err != nil {
		return err
	}
	before, revision := r.c.inventoryAnswer()
	price := repairPrice(before.Stacks[slot])
	if price == 0 || price > before.Silver {
		return errors.New("repair target no longer worn/affordable")
	}
	r.c.takeLootEvents()
	if err := r.c.send(protocol.EncodeStationRepairRequest(protocol.StationRepairRequest{TargetSlot: slot})); err != nil {
		return err
	}
	after, err := r.inventoryAfter(ctx, revision, func(s protocol.InventoryState) bool {
		return s.Stacks[slot].Durability == s.Stacks[slot].MaxDurability && s.Silver+price == before.Silver
	})
	if err != nil {
		return err
	}
	expected := protocol.InventoryState{Stacks: slices.Clone(before.Stacks), Silver: before.Silver - price}
	expected.Stacks[slot].Durability = expected.Stacks[slot].MaxDurability
	if !reflect.DeepEqual(after, expected) {
		return errors.New("paid repair changed something beyond exact target wear and silver debit")
	}
	r.hoard.Notes = append(r.hoard.Notes, fmt.Sprintf("capital forge repair slot %d item %d: durability %d -> %d; silver %d -> %d; exact debit %d", slot, before.Stacks[slot].ItemID, before.Stacks[slot].Durability, after.Stacks[slot].Durability, before.Silver, after.Silver, price))
	r.hoard.Repaired = true
	return nil
}
