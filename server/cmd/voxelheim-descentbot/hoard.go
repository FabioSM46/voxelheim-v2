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

// Evidence is populated only after its wire assertions pass. No item, currency or
// durability value below is ever written back as authoritative state.
type hoardEvidence struct {
	Chests    [world.InstanceChestCount]protocol.LootState
	ChestDone [world.InstanceChestCount]bool
	King      protocol.LootState
	KingDone  bool
	WorldID   uint64
	Seed      int64
	Restored  [world.InstanceChestCount]bool
	Notes     []string
	Bootstrap bool
	RuneOut   bool
	Crafted   bool
	Repaired  bool
	Stage     string
	Awaiting  string
}

func (r *runner) checkHoardBootstrap() error {
	state, revision := r.c.inventoryAnswer()
	if revision == 0 || state.Silver != 0 || countItem(state, game.ItemKingRune) != 0 || countItem(state, game.ItemRunicSword) != 0 || countItem(state, game.ItemEnchantingTable) != 0 || countItem(state, game.ItemIronSword) != 1 {
		return errors.New("bootstrap has unexpected currency or hoard inputs")
	}
	r.hoard.Bootstrap = true
	r.hoard.Notes = append(r.hoard.Notes, "pre-existing gear bootstrap: one IronSword, RustyHelm, RustyCuirass and RustyGreaves; starting silver, runes, runic swords and tables zero")
	return nil
}

func countItem(state protocol.InventoryState, item game.ItemID) uint32 {
	var total uint32
	for _, stack := range state.Stacks {
		if stack.ItemID == uint16(item) {
			total += uint32(stack.Count)
		}
	}
	return total
}

func itemSlot(state protocol.InventoryState, item game.ItemID) (uint8, bool) {
	for i, stack := range state.Stacks {
		if stack.ItemID == uint16(item) && stack.Count > 0 && i < int(protocol.InventorySlots) {
			return uint8(i), true
		}
	}
	return 0, false
}

func (r *runner) waitHoard(ctx context.Context, label string, ready func() bool) error {
	r.hoard.Awaiting = label
	deadline := time.Now().Add(15 * time.Second)
	for !ready() {
		if time.Now().After(deadline) {
			return fmt.Errorf("timed out awaiting %s", label)
		}
		if err := r.tickWait(ctx); err != nil {
			return err
		}
	}
	r.hoard.Awaiting = ""
	return nil
}

func (r *runner) inventoryAfter(ctx context.Context, before uint64, ready func(protocol.InventoryState) bool) (protocol.InventoryState, error) {
	var result protocol.InventoryState
	err := r.waitHoard(ctx, "authoritative inventory answer", func() bool {
		state, revision := r.c.inventoryAnswer()
		if revision > before && ready(state) {
			result = state
			return true
		}
		return false
	})
	return result, err
}

// awaitLoot consumes only responses observed since the caller drained before sending.
func (r *runner) awaitLoot(ctx context.Context, corpse uint64, closed bool, refusal vnet.RefusalReason) (protocol.LootState, error) {
	r.hoard.Awaiting = "loot response"
	deadline := time.Now().Add(15 * time.Second)
	for time.Now().Before(deadline) {
		states, closures, refused := r.c.takeLootEvents()
		for _, reply := range refused {
			if refusal != vnet.RefusalReasonUnknown && reply.Action == vnet.RefusedActionUseMechanism && reply.Reason == refusal {
				return protocol.LootState{}, nil
			}
			return protocol.LootState{}, fmt.Errorf("unexpected hoard refusal %s/%s", reply.Action, reply.Reason)
		}
		for _, state := range states {
			if refusal != vnet.RefusalReasonUnknown || closed {
				return state, errors.New("unexpected loot state instead of refusal/closure")
			}
			if corpse != 0 && state.CorpseID != corpse {
				return state, errors.New("loot reply named another container")
			}
			return state, nil
		}
		if closed && slices.Contains(closures, corpse) {
			return protocol.LootState{}, nil
		}
		if err := r.tickWait(ctx); err != nil {
			return protocol.LootState{}, err
		}
	}
	return protocol.LootState{}, errors.New("no matching loot response")
}

func chestAnchor(seed int64, index int) (world.PlacedAnchor, error) {
	for _, anchor := range world.InstanceDungeonAnchors(seed) {
		if anchor.Kind == world.AnchorInstanceChest && anchor.Index == index {
			return anchor, nil
		}
	}
	return world.PlacedAnchor{}, fmt.Errorf("chest anchor %d missing", index)
}

func (r *runner) chestIntent(anchor world.PlacedAnchor) error {
	r.c.stand()
	r.c.takeLootEvents()
	return r.c.send(protocol.EncodeMechanismUseRequest(protocol.MechanismUseRequest{HasPos: true, Pos: toWire(at(anchor)), ClientTick: r.c.tick()}))
}

func validateChestRoll(index int, state protocol.LootState) error {
	ranges := [3][2]uint32{{8, 15}, {12, 20}, {20, 30}}
	if index < 0 || index >= len(ranges) {
		return errors.New("invalid chest index")
	}
	if state.CorpseID == 0 || len(state.Entries) != 1 || state.Silver < ranges[index][0] || state.Silver > ranges[index][1] {
		return fmt.Errorf("chest %d has unexpected personal roll: %+v", index+1, state)
	}
	entry := state.Entries[0]
	valid := false
	switch index {
	case world.AntechamberChest:
		valid = entry.ItemID == uint16(game.ItemArrow) && entry.Count >= 8 && entry.Count <= 12
	case world.SandHallChest:
		valid = (entry.ItemID == uint16(game.ItemSharpeningStone) || entry.ItemID == uint16(game.ItemLeatherPatch)) && entry.Count == 1
	case world.KingChest:
		valid = slices.Contains([]uint16{uint16(game.ItemRustyHelm), uint16(game.ItemRustyCuirass), uint16(game.ItemRustyGreaves)}, entry.ItemID) && entry.Count == 1
	}
	if !valid {
		return fmt.Errorf("chest %d has unexpected item %d x%d", index+1, entry.ItemID, entry.Count)
	}
	return nil
}

func (r *runner) collectLoot(ctx context.Context, loot protocol.LootState) error {
	before, revision := r.c.inventoryAnswer()
	r.c.takeLootEvents()
	if err := r.c.send(protocol.EncodeLootTakeAllRequest(protocol.LootTakeAllRequest{CorpseID: loot.CorpseID, Revision: loot.Revision, ClientTick: r.c.tick()})); err != nil {
		return err
	}
	if _, err := r.awaitLoot(ctx, loot.CorpseID, true, vnet.RefusalReasonUnknown); err != nil {
		return err
	}
	after, err := r.inventoryAfter(ctx, revision, func(state protocol.InventoryState) bool {
		return state.Silver == before.Silver+loot.Silver && lootAdded(before, state, loot)
	})
	if err != nil {
		return err
	}
	if after.Silver-before.Silver != loot.Silver {
		return errors.New("personal loot silver debit/credit mismatch")
	}
	return nil
}

func lootAdded(before, after protocol.InventoryState, loot protocol.LootState) bool {
	totals := map[game.ItemID]uint32{}
	for _, entry := range loot.Entries {
		totals[game.ItemID(entry.ItemID)] += uint32(entry.Count)
	}
	for item, n := range totals {
		if countItem(after, item) != countItem(before, item)+n {
			return false
		}
	}
	return true
}

func (r *runner) checkChest(ctx context.Context, index int) error {
	anchor, err := chestAnchor(r.lay.seed, index)
	if err != nil {
		return err
	}
	if err := r.walkTo(ctx, fmt.Sprintf("chest %d", index+1), near(at(anchor), 1.5), true); err != nil {
		return err
	}
	if err := r.chestIntent(anchor); err != nil {
		return err
	}
	initial, err := r.awaitLoot(ctx, 0, false, vnet.RefusalReasonUnknown)
	if err != nil {
		return err
	}
	if err := validateChestRoll(index, initial); err != nil {
		return err
	}
	if err := r.chestIntent(anchor); err != nil {
		return err
	}
	reopened, err := r.awaitLoot(ctx, initial.CorpseID, false, vnet.RefusalReasonUnknown)
	if err != nil {
		return err
	}
	if !reflect.DeepEqual(initial, reopened) {
		return errors.New("reopening rerolled personal loot or revision")
	}
	if err := r.collectLoot(ctx, initial); err != nil {
		return err
	}
	if err := r.chestIntent(anchor); err != nil {
		return err
	}
	if _, err := r.awaitLoot(ctx, 0, false, vnet.RefusalReasonChestAlreadyOpened); err != nil {
		return err
	}
	if err := r.waitHoard(ctx, "open chest block", func() bool { b, ok := r.c.blockAt(at(anchor)); return ok && b == world.ChestOpen }); err != nil {
		return err
	}
	r.hoard.Chests[index], r.hoard.ChestDone[index] = initial, true
	r.hoard.WorldID, r.hoard.Seed = r.c.self().world, r.lay.seed
	r.say("hoard chest %d: personal silver %d entries %v; reopen unchanged; exhausted refusal", index+1, initial.Silver, initial.Entries)
	return nil
}

func (r *runner) collectKing(ctx context.Context) error {
	r.hoard.Stage = "king corpse loot"
	r.c.mu.Lock()
	target := r.c.kingTarget
	r.c.mu.Unlock()
	if target.id == 0 || r.stats.killedCount(vnet.MobKindDraugrKing) == 0 {
		return errors.New("no defeated king observed for rune collection")
	}
	if err := r.walkTo(ctx, "king's personal reward", near(feetCell(target.pos), 1.5), true); err != nil {
		return err
	}
	if err := r.waitHoard(ctx, "accessible king corpse", func() bool {
		r.c.mu.Lock()
		defer r.c.mu.Unlock()
		return slices.Contains(r.c.accessibleLoot, target.id)
	}); err != nil {
		return err
	}
	r.c.takeLootEvents()
	if err := r.c.send(protocol.EncodeLootOpenRequest(protocol.LootOpenRequest{CorpseID: target.id, ClientTick: r.c.tick()})); err != nil {
		return err
	}
	loot, err := r.awaitLoot(ctx, target.id, false, vnet.RefusalReasonUnknown)
	if err != nil {
		return err
	}
	var runes, swords uint16
	for _, entry := range loot.Entries {
		if entry.ItemID == uint16(game.ItemKingRune) {
			runes += entry.Count
		}
		if entry.ItemID == uint16(game.ItemIronSword) {
			swords += entry.Count
		}
	}
	if runes != 1 || swords != 1 {
		return fmt.Errorf("king personal reward has rune %d iron sword %d", runes, swords)
	}
	if err := r.collectLoot(ctx, loot); err != nil {
		return err
	}
	r.hoard.King, r.hoard.KingDone = loot, true
	r.say("hoard king: personal rune and iron sword received from corpse")
	return nil
}
