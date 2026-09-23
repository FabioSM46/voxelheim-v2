package game

import (
	"context"
	"math"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// A cobweb is cover to collision: a body falls into it and walks through it, and no
// creature stands on it or steps up onto it.
func TestACobwebStopsNoBodyAndHoldsNoneUp(t *testing.T) {
	t.Parallel()

	w := meadowWorld{groundTop: 63, flower: world.Cobweb}
	if !w.Solid(0, w.groundTop, 0) || w.Solid(0, w.groundTop+1, 0) || w.Fluid(0, w.groundTop+1, 0) {
		t.Fatal("the fixture is not a passable web over solid grass")
	}

	pos, blocked := moveAndCollide(w, playerBody, [3]float64{0.5, float64(w.groundTop) + 3.7, 0.5}, [3]float64{0, -3, 0})
	if !blocked[1] {
		t.Fatal("the fall was not stopped by the ground")
	}
	if want := float64(w.groundTop + 1); math.Abs(pos[1]-want) > tolerance {
		t.Errorf("feet come to rest at %.4f, want %.1f — in the web, not on top of it", pos[1], want)
	}
	walked, hit := moveAndCollide(w, playerBody, [3]float64{0.5, float64(w.groundTop + 1), 0.5}, [3]float64{3, 0, 0})
	if hit[0] || walked[0] < 3.5-tolerance {
		t.Errorf("a walk through the web was obstructed: reached x=%.4f, hit=%t", walked[0], hit[0])
	}
	if standableFloor(world.Cobweb) {
		t.Error("standableFloor(Cobweb) = true: a creature would spawn on a web")
	}
	def := mobRegistry[vnet.MobKindDraugr]
	m := &mob{kind: vnet.MobKindDraugr, pos: [3]float64{3.5, float64(w.groundTop + 1), 0.5}}
	if m.stepsUp(w, [2]float64{def.speed, 0}, 1.0/float64(DefaultTickRate)) {
		t.Error("a draugr treats a web as a step to hop over")
	}
}

// A body inside a web keeps 40% of its horizontal speed, and the scale composes with
// starvation the way snow does. The same field without the web is the control.
func TestACobwebSlowsABodyToFortyPercentAndComposesWithStarvation(t *testing.T) {
	t.Parallel()

	if CobwebSpeedScale != 0.4 {
		t.Fatalf("CobwebSpeedScale = %v, want 0.4", CobwebSpeedScale)
	}
	velocityAt := func(terrain Terrain, hunger uint16) float64 {
		h := newDropHarness(t, terrain)
		player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
		h.sim.mu.Lock()
		defer h.sim.mu.Unlock()
		player.hunger = hunger
		player.current = intent{moveX: 0.6, moveZ: 0.8}
		for range swimSettleTicks {
			player.step(1/float64(DefaultTickRate), terrain)
		}
		return math.Hypot(player.vel[0], player.vel[2])
	}

	webbed := meadowWorld{groundTop: 63, flower: world.Cobweb}
	open := meadowWorld{groundTop: 63, flower: world.Air}
	for _, tc := range []struct {
		name    string
		terrain Terrain
		hunger  uint16
		want    float64
	}{
		{"open ground", open, 1, WalkSpeed},
		{"in a web", webbed, 1, WalkSpeed * CobwebSpeedScale},
		{"starving in a web", webbed, 0, WalkSpeed * StarvingSpeedScale * CobwebSpeedScale},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := velocityAt(tc.terrain, tc.hunger); math.Abs(got-tc.want) > 1e-12 {
				t.Errorf("horizontal speed = %v, want %v", got, tc.want)
			}
		})
	}
}

// One hit takes a web away: the instant-break floor exactly, one implement, nothing
// dropped and nothing learned. The break runs on the real mining path.
func TestBreakingACobwebIsOneQuickHitThatLeavesNothing(t *testing.T) {
	t.Parallel()

	if got := handMiningTimes[world.Cobweb]; got != 250*time.Millisecond {
		t.Errorf("cobweb hand time = %v, want 250ms", got)
	}
	if !helpsWith(ItemAxe, world.Cobweb) || helpsWith(ItemShovel, world.Cobweb) || helpsWith(ItemPickaxe, world.Cobweb) {
		t.Error("the cobweb must be assigned to the axe and nothing else")
	}
	if got := itemDroppedBy(world.Cobweb); got != ItemNone {
		t.Errorf("a cobweb drops item %d, want nothing", got)
	}

	target := [3]int32{3, 200, 0}
	sim, player, terrain, _ := newMiningPlayer(t, map[[3]int64]world.Block{mineTarget(target): world.Cobweb})
	cost, breakable := sim.hardnessTicks(world.Cobweb, ItemNone)
	if !breakable {
		t.Fatal("a cobweb is not breakable by hand")
	}
	for tick := 1; tick <= cost; tick++ {
		if err := player.Mine(activeMine(target, uint32(tick)), true); err != nil {
			t.Fatalf("mining refresh %d: %v", tick, err)
		}
		sim.Step(uint64(tick))
	}
	result, err := player.CompleteMining(context.Background(), awaitCompletion(t, player))
	if err != nil {
		t.Fatalf("CompleteMining: %v", err)
	}
	if got, _ := terrain.Block(3, 200, 0); result.Block != world.Air || got != world.Air {
		t.Errorf("after the break the result is %d and the voxel %d, want Air", result.Block, got)
	}
	if got := experienceOf(player); got != 0 {
		t.Errorf("breaking a web awarded %d experience, want 0", got)
	}
	sim.mu.Lock()
	drops := len(sim.drops)
	sim.mu.Unlock()
	if drops != 0 || result.Inventory != nil {
		t.Errorf("breaking a web created %d drops and inventory change %t", drops, result.Inventory != nil)
	}
}

// Levers and the lit rune are mechanism state, never an ordinary edit: no mining row,
// no drop row, and a placement into one is refused.
func TestLeversAndTheLitRuneAreNeverMinedOrOverwritten(t *testing.T) {
	t.Parallel()

	sim, _, _, _ := newMiningPlayer(t, nil)
	for _, block := range []world.Block{world.LeverOff, world.LeverOn, world.RuneStone, world.RuneStoneLit} {
		if cost, ok := sim.hardnessTicks(block, ItemNone); ok || cost != 0 {
			t.Errorf("block %d is breakable (cost %d)", block, cost)
		}
		if _, ok := blockDrops[block]; ok {
			t.Errorf("block %d has a drop row", block)
		}
		if allowPlacement(block) == nil {
			t.Errorf("a placement may overwrite block %d", block)
		}
	}
}
