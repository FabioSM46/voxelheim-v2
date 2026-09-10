package game_test

import (
	"context"
	"errors"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// A placement spends an item and writes the world, so a pending boss reward refuses it
// before the chunk is generated or the voxel is written, through the exported boundary
// the coordinator will use.
func TestAPendingBossRewardRefusesABlockPlacementUntilItEnds(t *testing.T) {
	t.Parallel()

	h, chunks := editWorld(t)
	player, _ := h.join(1, [3]float32{0.5, 200, 0.5})
	ctx := context.Background()
	giveBlock(t, h, player, chunks, world.Stone)
	manager, err := game.NewInstanceManager(game.DefaultTickRate, 2, 1, testEntityIDs(), discard())
	if err != nil {
		t.Fatalf("NewInstanceManager: %v", err)
	}
	t.Cleanup(manager.Close)

	claim, _, err := manager.ReserveBossReward(player, player.Record(), game.BossRewardGrant{Experience: 1})
	if err != nil {
		t.Fatalf("ReserveBossReward: %v", err)
	}
	target := [3]int32{3, 200, 0}
	before := countOf(player.InventoryState(), game.ItemStone)
	if _, err := player.Edit(ctx, placeAt(t, player, target, world.Stone)); !errors.Is(err, game.ErrBossRewardBusy) {
		t.Fatalf("placement during a pending reward = %v, want %v", err, game.ErrBossRewardBusy)
	}
	if got := blockAt(t, chunks, 3, 200, 0); got != world.Air {
		t.Fatalf("the refused placement wrote block %d", got)
	}
	if got := countOf(player.InventoryState(), game.ItemStone); got != before {
		t.Fatalf("the refused placement spent stone: %d, want %d", got, before)
	}

	if err := player.AbortBossReward(claim); err != nil {
		t.Fatalf("AbortBossReward: %v", err)
	}
	if _, err := player.Edit(ctx, placeAt(t, player, target, world.Stone)); err != nil {
		t.Fatalf("the same placement without a reward was refused: %v", err)
	}
	if got := blockAt(t, chunks, 3, 200, 0); got != world.Stone {
		t.Fatalf("the placement wrote block %d, want Stone", got)
	}
}
