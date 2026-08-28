package game

import (
	"testing"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// What water is to the two paths that write the world: the edit legality test, and
// the hardness table mining reads.
//
// **Nothing in this build generates water yet** — that is the worldgen half of this
// issue, and it lands separately. What exists here is the id, the passability rule
// and every rule that reads them, which is what these exercise.

// A voxel of water is not an obstruction — it is displaced by whatever is put into
// it — and it is the *only* block that is.
func TestAPlacementDisplacesWaterAndNothingElse(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		current world.Block
		allowed bool
	}{
		{world.Air, true},
		{world.Water, true},
		{world.Ice, false},
		{world.Stone, false},
		{world.Leaves, false},
	} {
		err := allowPlacement(tc.current)
		if allowed := err == nil; allowed != tc.allowed {
			t.Errorf("allowPlacement(%d) allowed = %t (err %v), want %t", tc.current, allowed, err, tc.allowed)
		}
	}
}

// Mining water is refused exactly the way mining air is: by there being no row for
// it in the cost table at all.
//
// **No branch anywhere on the mining path names water**, and that is the point of
// asserting it here rather than trusting the map literal: the refusal is the
// registry's fail-closed default, so it holds for every future block nobody has
// priced as well.
func TestWaterIsNotMineableAndIceIs(t *testing.T) {
	t.Parallel()

	sim, _, _, _ := newMiningPlayer(t, nil)

	for _, block := range []world.Block{world.Air, world.Water} {
		if cost, breakable := sim.hardnessTicks(block, ItemNone); breakable {
			t.Errorf("block %d is breakable at %d ticks; nothing without a hand-mining row is", block, cost)
		}
	}

	byHand, breakable := sim.hardnessTicks(world.Ice, ItemNone)
	if !breakable {
		t.Fatal("ice is not breakable")
	}
	if want := handMiningTimes[world.Ice]; want != 1500*time.Millisecond {
		t.Errorf("ice takes %v by hand, want the pinned 1.5s", want)
	}
	// The pickaxe is its implement, and the whole registry rule follows from that one
	// entry — the division is TestTheRightToolIsFourTimesFasterAndTheWrongOneIsABareHand's.
	if !helpsWith(ItemPickaxe, world.Ice) {
		t.Error("no implement is suited to ice")
	}
	for _, wrong := range []ItemID{ItemShovel, ItemAxe} {
		if helpsWith(wrong, world.Ice) {
			t.Errorf("item %d is suited to ice as well as the pickaxe", wrong)
		}
	}
	if withPick, _ := sim.hardnessTicks(world.Ice, ItemPickaxe); withPick >= byHand {
		t.Errorf("a pickaxe breaks ice in %d ticks against a bare hand's %d", withPick, byHand)
	}
}
