package game

import (
	"math"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The swim rules, driven through the tick the way the loop drives them.
//
// **Every assertion here is about a relationship the constants name, not about a
// number the integrator happens to produce.** Pinning "y is 61.3 after nine ticks"
// would fail on any retune of SwimAcceleration and would say nothing about whether
// the player was swimming; what is asserted instead is that the vertical speed
// settles at SwimSinkSpeed, that a jump starts it at SwimRiseSpeed, that the
// horizontal speed is capped at SwimSpeed, and that no fall into water hurts.

// lakeWorld is stone up to bedTop, water from there to waterTop, and air above — the
// shape a lake will have when the generator learns to make one. iceLid puts the
// tundra's one solid voxel on the surface at waterTop.
type lakeWorld struct {
	bedTop    int64
	waterTop  int64
	waterKind world.Block
	iceLid    bool
}

func (w lakeWorld) Block(_, y, _ int64) (world.Block, bool) {
	switch {
	case y <= w.bedTop:
		return world.Stone, true
	case w.iceLid && y == w.waterTop:
		return world.Ice, true
	case y <= w.waterTop:
		if w.waterKind != world.Air {
			return w.waterKind, true
		}
		return world.Water, true
	default:
		return world.Air, true
	}
}

func (w lakeWorld) Solid(x, y, z int64) bool {
	block, resident := w.Block(x, y, z)
	return !resident || world.Solid(block)
}

func (w lakeWorld) Fluid(x, y, z int64) bool { return fluidByBlock(w, x, y, z) }

// The fixture has to be the world it claims to be, or every test below passes for a
// reason that has nothing to do with swimming.
func TestTheLakeFixtureIsWaterOverStone(t *testing.T) {
	t.Parallel()

	lake := lakeWorld{bedTop: 40, waterTop: 60}
	for _, tc := range []struct {
		y     int64
		block world.Block
		solid bool
		fluid bool
	}{
		{40, world.Stone, true, false},
		{41, world.Water, false, true},
		{60, world.Water, false, true},
		{61, world.Air, false, false},
	} {
		block, _ := lake.Block(0, tc.y, 0)
		if block != tc.block || lake.Solid(0, tc.y, 0) != tc.solid || lake.Fluid(0, tc.y, 0) != tc.fluid {
			t.Errorf("y=%d is block %d (solid %t, fluid %t), want block %d (solid %t, fluid %t)",
				tc.y, block, lake.Solid(0, tc.y, 0), lake.Fluid(0, tc.y, 0), tc.block, tc.solid, tc.fluid)
		}
	}
}

// A player in water sinks at SwimSinkSpeed instead of falling at Gravity.
func TestAPlayerInFlowingWaterSinksInsteadOfFalling(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, lakeWorld{
		bedTop: 0, waterTop: 200, waterKind: world.WaterFlow3,
	})
	player, _ := h.join(1, [3]float32{0.5, 100, 0.5})

	// Long enough for the ease to reach the terminal speed from a standing start:
	// SwimSinkSpeed over SwimAcceleration is a fraction of a second.
	h.advance(2 * DefaultTickRate)

	state := player.State()
	if got := float64(state.Vel[1]); math.Abs(got-SwimSinkSpeed) > 1e-6 {
		t.Errorf("a player in water sinks at %v blocks/s, want SwimSinkSpeed %v", got, SwimSinkSpeed)
	}
	if state.OnGround {
		t.Error("a player floating in water reports standing on something")
	}

	// And the same player in air is falling far faster by the same tick, which is what
	// makes the number above a swim rather than a slow generator.
	dry := newVitalsHarness(t, DefaultTickRate, emptyDropTerrain{})
	falling, _ := dry.join(1, [3]float32{0.5, 100, 0.5})
	dry.advance(2 * DefaultTickRate)
	if got := float64(falling.State().Vel[1]); got >= SwimSinkSpeed {
		t.Errorf("a player falling through air reaches %v blocks/s, no faster than swimming", got)
	}
}

// A jump intent in water is a rise, and it does not need the ground the way a jump
// does.
func TestAJumpIntentInWaterIsARiseAndNeedsNoGround(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, lakeWorld{bedTop: 0, waterTop: 200})
	player, _ := h.join(1, [3]float32{0.5, 100, 0.5})
	h.advance(2 * DefaultTickRate)

	before := player.State()
	if before.OnGround {
		t.Fatal("the player is standing on something, so this proves nothing about a rise without ground")
	}

	if err := player.Submit(protocol.PlayerInput{ClientTick: 1, Jump: true}); err != nil {
		t.Fatalf("Submit: %v", err)
	}
	h.step()

	after := player.State()
	if after.Vel[1] <= 0 {
		t.Fatalf("a jump intent in water left the vertical speed at %v", after.Vel[1])
	}
	// The tick applies the rise and then one step of the ease toward the sink speed,
	// so the speed is a little under SwimRiseSpeed and nowhere near JumpImpulse.
	want := SwimRiseSpeed - SwimAcceleration/float64(DefaultTickRate)
	if got := float64(after.Vel[1]); math.Abs(got-want) > 1e-6 {
		t.Errorf("a rise starts at %v blocks/s, want SwimRiseSpeed %v eased by one tick to %v", got, SwimRiseSpeed, want)
	}
	if after.Pos[1] <= before.Pos[1] {
		t.Errorf("the player did not rise: y %v then %v", before.Pos[1], after.Pos[1])
	}
}

// Horizontal movement in water is capped at SwimSpeed, however hard the controls are
// held — and the cap survives starvation, which is the one other thing that touches
// the speed.
func TestSwimmingIsSlowerThanWalking(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, lakeWorld{bedTop: 0, waterTop: 200})
	player, _ := h.join(1, [3]float32{0.5, 100, 0.5})

	if err := player.Submit(protocol.PlayerInput{ClientTick: 1, MoveZ: 1}); err != nil {
		t.Fatalf("Submit: %v", err)
	}
	h.step()

	horizontal := math.Hypot(float64(player.State().Vel[0]), float64(player.State().Vel[2]))
	if math.Abs(horizontal-SwimSpeed) > 1e-5 {
		t.Errorf("a swimmer at full intent moves at %v blocks/s, want SwimSpeed %v", horizontal, SwimSpeed)
	}
	if SwimSpeed >= WalkSpeed {
		t.Errorf("SwimSpeed %v is not slower than WalkSpeed %v", SwimSpeed, WalkSpeed)
	}

	// Starving on land is 0.8 × WalkSpeed, which is still faster than swimming; the
	// cap is what makes a starving swimmer a swimmer rather than eight tenths of one.
	h.sim.mu.Lock()
	player.hunger = 0
	h.sim.mu.Unlock()
	if err := player.Submit(protocol.PlayerInput{ClientTick: 2, MoveZ: 1}); err != nil {
		t.Fatalf("Submit: %v", err)
	}
	h.step()

	starving := math.Hypot(float64(player.State().Vel[0]), float64(player.State().Vel[2]))
	if math.Abs(starving-SwimSpeed) > 1e-5 {
		t.Errorf("a starving swimmer moves at %v blocks/s, want the same SwimSpeed %v", starving, SwimSpeed)
	}
}

// A fall into water does no harm, however far it fell — and the same fall onto stone
// does, which is what makes the first half a statement about the water.
func TestNoFallIntoWaterHurts(t *testing.T) {
	t.Parallel()

	// Thirty blocks, which reaches well past SafeFallSpeed: the fall arrives at about
	// 42 blocks/s against a threshold of 28.
	const dropHeight = 30

	lake := lakeWorld{bedTop: 40, waterTop: 44}
	wet := newVitalsHarness(t, DefaultTickRate, lake)
	swimmer, _ := wet.join(1, [3]float32{0.5, float32(lake.waterTop + dropHeight), 0.5})
	wet.advance(8 * DefaultTickRate)

	if got := wet.vitals(swimmer).Health; got != PlayerMaxHealth {
		t.Errorf("a %d-block fall into water cost %d health", dropHeight, PlayerMaxHealth-got)
	}

	// The same drop onto the same bed with the water taken away.
	dry := newVitalsHarness(t, DefaultTickRate, lakeWorld{bedTop: lake.bedTop, waterTop: lake.bedTop})
	faller, _ := dry.join(2, [3]float32{0.5, float32(lake.waterTop + dropHeight), 0.5})
	dry.advance(8 * DefaultTickRate)

	if got := dry.vitals(faller).Health; got == PlayerMaxHealth {
		t.Fatalf("the same %d-block fall onto stone cost nothing, so the water test proves nothing", dropHeight)
	}
}

// Ice is ground: a player walks on the lid of a frozen lake rather than through it.
func TestAPlayerWalksOnIceRatherThanThroughIt(t *testing.T) {
	t.Parallel()

	lake := lakeWorld{bedTop: 40, waterTop: 60, iceLid: true}
	h := newVitalsHarness(t, DefaultTickRate, lake)
	player, _ := h.join(1, [3]float32{0.5, float32(lake.waterTop + 3), 0.5})
	h.advance(4 * DefaultTickRate)

	state := player.State()
	if !state.OnGround {
		t.Fatalf("the player is not standing on the ice; it is at y=%v", state.Pos[1])
	}
	// The top face of the lid is one above the voxel it occupies.
	if want := float32(lake.waterTop + 1); math.Abs(float64(state.Pos[1]-want)) > 1e-3 {
		t.Errorf("the player rests at y=%v, want the ice surface %v", state.Pos[1], want)
	}
	if got := float64(state.Vel[1]); got != 0 {
		t.Errorf("a player standing on ice has vertical speed %v", got)
	}
}

// The whole of the integrator the swim rules are built on, checked at its edges
// rather than through the tick.
func TestApproachNeverOvershootsItsTarget(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		current, target, step, want float64
	}{
		{current: 10, target: 0, step: 3, want: 7},
		{current: 10, target: 0, step: 30, want: 0},
		{current: -10, target: 0, step: 3, want: -7},
		{current: -10, target: 0, step: 30, want: 0},
		{current: 0, target: 0, step: 3, want: 0},
		{current: -60, target: SwimSinkSpeed, step: 0.6, want: -59.4},
	} {
		if got := approach(tc.current, tc.target, tc.step); math.Abs(got-tc.want) > 1e-9 {
			t.Errorf("approach(%v, %v, %v) = %v, want %v", tc.current, tc.target, tc.step, got, tc.want)
		}
	}
}

// emptyDropTerrain is air everywhere, for the falling half of the sink comparison.
type emptyDropTerrain struct{}

func (emptyDropTerrain) Solid(int64, int64, int64) bool { return false }
func (emptyDropTerrain) Block(int64, int64, int64) (world.Block, bool) {
	return world.Air, true
}
func (w emptyDropTerrain) Fluid(x, y, z int64) bool { return fluidByBlock(w, x, y, z) }
