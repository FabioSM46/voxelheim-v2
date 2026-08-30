package game

import (
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// meadowWorld is grass up to groundTop with a flower in the voxel above it. Solid is
// derived from [world.Solid] the way every scripted terrain here derives it, so
// nothing in this file states a passability answer of its own.
type meadowWorld struct {
	groundTop int64
	flower    world.Block
}

func (w meadowWorld) Block(_, y, _ int64) (world.Block, bool) {
	switch {
	case y <= w.groundTop:
		return world.Grass, true
	case y == w.groundTop+1:
		return w.flower, true
	default:
		return world.Air, true
	}
}

func (w meadowWorld) Solid(x, y, z int64) bool {
	block, resident := w.Block(x, y, z)
	return !resident || world.Solid(block)
}

func (w meadowWorld) Fluid(x, y, z int64) bool { return fluidByBlock(w, x, y, z) }

// **A body walks through a flower and rests in it rather than on top of it, and
// nothing stands on one.** Feet at groundTop+1 is the top face of the grass, the
// voxel the flower occupies. None of the four rules below needed a line of code,
// which is the point of putting the answer in [world.Solid].
func TestAFlowerStopsNoBodyAndHoldsNoneUp(t *testing.T) {
	t.Parallel()

	// One flower: collision reads world.Solid rather than an id, and the palette test
	// in internal/world pins the class to these three. The fixture has to be the
	// meadow it claims to be, or everything below passes for the wrong reason.
	w := meadowWorld{groundTop: 63, flower: world.FlowerRed}
	if !w.Solid(0, w.groundTop, 0) || w.Solid(0, w.groundTop+1, 0) || w.Fluid(0, w.groundTop+1, 0) {
		t.Fatal("the fixture is not a passable flower over solid grass")
	}

	// A fractional start height, so the descent arrives between two sub-steps the way
	// a tick's accelerating fall does.
	pos, blocked := moveAndCollide(w, playerBody, [3]float64{0.5, float64(w.groundTop) + 3.7, 0.5}, [3]float64{0, -3, 0})
	if !blocked[1] {
		t.Fatal("the fall was not stopped by the ground")
	}
	if want := float64(w.groundTop + 1); pos[1] < want-tolerance || pos[1] > want+tolerance {
		t.Errorf("feet come to rest at %.4f, want %.1f — in the flower, not on top of it", pos[1], want)
	}

	// And a sweep straight through the flower voxel is unobstructed.
	walked, hit := moveAndCollide(w, playerBody, [3]float64{0.5, float64(w.groundTop + 1), 0.5}, [3]float64{3, 0, 0})
	if hit[0] {
		t.Error("a walk through the flower was obstructed")
	}
	if want := 3.5; walked[0] < want-tolerance {
		t.Errorf("the walk reached x=%.4f, want %.1f", walked[0], want)
	}

	// No creature is put on one either.
	for _, block := range []world.Block{world.FlowerRed, world.FlowerYellow, world.FlowerBlue} {
		if standableFloor(block) {
			t.Errorf("standableFloor(%d) = true, want false: a creature would spawn on a flower", block)
		}
	}
	if !standableFloor(world.Grass) {
		t.Fatal("standableFloor(Grass) = false; the assertions above would pass for the wrong reason")
	}

	// Not a step either: a creature walks through a drift rather than hopping stems.
	def := mobRegistry[vnet.MobKindDraugr]
	m := &mob{kind: vnet.MobKindDraugr, pos: [3]float64{3.5, float64(w.groundTop + 1), 0.5}}
	if m.stepsUp(w, [2]float64{def.speed, 0}, 1.0/float64(DefaultTickRate)) {
		t.Error("a draugr treats a flower as a step to hop over")
	}

	// The surface scan finds the grass, not the flower over it.
	surface, found := surfaceUnderSky(w, 0, 0, w.groundTop+8, w.groundTop-8)
	if !found || surface != w.groundTop {
		t.Errorf("surfaceUnderSky = (%d, %t), want (%d, true)", surface, found, w.groundTop)
	}
}

// tolerance is a millimetre: collision stops a hair short of the face it hit.
const tolerance = 1e-3
