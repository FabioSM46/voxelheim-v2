package game

import (
	"math"
	"testing"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The current rules, asserted the same way the swim rules beside them are: against
// the relationships the constants name, never against a number the integrator
// happens to produce on a given tick.
//
// **The whole of this is server-side, which is the point of testing it here.** A
// current moves a body, so it is a gameplay outcome and the tick resolves it; a
// client may mirror [FlowDirection] to animate a surface, and what it renders is
// still whatever this loop decided.

// swimSettleTicks is how long the horizontal ease takes to reach a swim target from
// a standstill.
//
// SwimSpeed over SwimAcceleration is 0.215 s, which is five ticks at
// DefaultTickRate; eight leaves margin without reaching idleLimit's ten, so an
// intent submitted once still stands at the end of the run.
const swimSettleTicks = 8

// blockTable is a world stated voxel by voxel: whatever blocks says, fill
// everywhere else, and every chunk resident except the ones absent names.
//
// Deliberately not [lakeWorld] with a lookup bolted on: [FlowDirection] is a
// function of one voxel and its five neighbours, and the shapes it has to be shown —
// a level between a deeper level and air, a wall on one side, a chunk that has not
// arrived — are exactly the ones a layered fixture makes hardest to write.
type blockTable struct {
	blocks map[[3]int64]world.Block
	fill   world.Block
	absent map[[3]int64]bool
}

func (w blockTable) Block(x, y, z int64) (world.Block, bool) {
	at := [3]int64{x, y, z}
	if w.absent[at] {
		return world.Air, false
	}
	if block, ok := w.blocks[at]; ok {
		return block, true
	}
	return w.fill, true
}

func (w blockTable) Solid(x, y, z int64) bool {
	block, resident := w.Block(x, y, z)
	return !resident || world.Solid(block)
}

func (w blockTable) Fluid(x, y, z int64) bool { return fluidByBlock(w, x, y, z) }

// around builds a table holding centre at the origin and the named offsets around it.
func around(centre world.Block, neighbours map[[3]int64]world.Block) map[[3]int64]world.Block {
	blocks := map[[3]int64]world.Block{{}: centre}
	for at, block := range neighbours {
		blocks[at] = block
	}
	return blocks
}

var (
	east  = [3]int64{1, 0, 0}
	west  = [3]int64{-1, 0, 0}
	north = [3]int64{0, 0, -1}
	south = [3]int64{0, 0, 1}
	above = [3]int64{0, 1, 0}
)

// One table for the whole derivation, because the derivation is one function of the
// six voxels it reads.
func TestFlowDirectionReadsTheWaterAndNothingElse(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name   string
		table  blockTable
		want   [3]float64
		reason string
	}{
		{
			name:   "a generator-authored current carries its own direction",
			table:  blockTable{blocks: around(world.WaterCurrentXPos, nil), fill: world.Water},
			want:   [3]float64{1, 0, 0},
			reason: "the id is the answer; nothing is derived from the neighbours",
		},
		{
			name:  "and so do its three siblings",
			table: blockTable{blocks: around(world.WaterCurrentXNeg, nil), fill: world.Water},
			want:  [3]float64{-1, 0, 0},
		},
		{
			name:  "z positive",
			table: blockTable{blocks: around(world.WaterCurrentZPos, nil), fill: world.Water},
			want:  [3]float64{0, 0, 1},
		},
		{
			name:  "z negative",
			table: blockTable{blocks: around(world.WaterCurrentZNeg, nil), fill: world.Water},
			want:  [3]float64{0, 0, -1},
		},
		{
			name:   "a current with water above it is still not a fall",
			table:  blockTable{blocks: around(world.WaterCurrentXPos, map[[3]int64]world.Block{above: world.Water}), fill: world.Water},
			want:   [3]float64{1, 0, 0},
			reason: "a river is full-depth source water; every voxel of it has water above",
		},
		{
			name:   "a plain source is standing water",
			table:  blockTable{blocks: around(world.Water, nil), fill: world.Air},
			want:   [3]float64{0, 0, 0},
			reason: "no direction, and no fall however deep the lake is",
		},
		{
			name:   "a plain source with water above it is still standing water",
			table:  blockTable{blocks: around(world.Water, map[[3]int64]world.Block{above: world.Water}), fill: world.Air},
			want:   [3]float64{0, 0, 0},
			reason: "otherwise every lake would drag a swimmer to its bed",
		},
		{
			name: "a level between a deeper level and air points at the air",
			table: blockTable{blocks: around(world.WaterFlow5, map[[3]int64]world.Block{
				west: world.WaterFlow6, east: world.Air, north: world.WaterFlow5, south: world.WaterFlow5,
			}), fill: world.Air},
			want:   [3]float64{1, 0, 0},
			reason: "5 against air is +5 east, 5 against 6 is +1 east, and the equal pair cancels",
		},
		{
			name: "a level in a flat sheet of its own level has nowhere to go",
			table: blockTable{blocks: around(world.WaterFlow3, map[[3]int64]world.Block{
				west: world.WaterFlow3, east: world.WaterFlow3, north: world.WaterFlow3, south: world.WaterFlow3,
			}), fill: world.Air},
			want: [3]float64{0, 0, 0},
		},
		{
			name: "a wall is not somewhere to flow",
			table: blockTable{blocks: around(world.WaterFlow3, map[[3]int64]world.Block{
				east: world.Stone, west: world.Air, north: world.WaterFlow3, south: world.WaterFlow3,
			}), fill: world.Air},
			want:   [3]float64{-1, 0, 0},
			reason: "the stone is skipped rather than counted as empty, so only the air pulls",
		},
		{
			name: "and neither is the ice lid",
			table: blockTable{blocks: around(world.WaterFlow3, map[[3]int64]world.Block{
				east: world.Ice, west: world.Air, north: world.WaterFlow3, south: world.WaterFlow3,
			}), fill: world.Air},
			want:   [3]float64{-1, 0, 0},
			reason: "world.Solid answers for Ice, which is why FlowDirection needs no second test for it",
		},
		{
			name: "a source neighbour pushes away from itself",
			table: blockTable{blocks: around(world.WaterFlow3, map[[3]int64]world.Block{
				east: world.Water, west: world.Air, north: world.WaterFlow3, south: world.WaterFlow3,
			}), fill: world.Air},
			want:   [3]float64{-1, 0, 0},
			reason: "3 against a full 8 is -5 east, which is a push west; the air adds 3 more the same way",
		},
		{
			name: "and it is the whole answer when it is the only gradient",
			table: blockTable{blocks: around(world.WaterFlow3, map[[3]int64]world.Block{
				east: world.Water, west: world.WaterFlow3, north: world.WaterFlow3, south: world.WaterFlow3,
			}), fill: world.Air},
			want:   [3]float64{-1, 0, 0},
			reason: "the equal three cancel, so a spring with nothing else around it still sends its water west",
		},
		{
			name: "a river current neighbour counts as the source it is",
			table: blockTable{blocks: around(world.WaterFlow3, map[[3]int64]world.Block{
				east: world.WaterCurrentZPos, west: world.WaterFlow3, north: world.WaterFlow3, south: world.WaterFlow3,
			}), fill: world.Air},
			want:   [3]float64{-1, 0, 0},
			reason: "it is level 8 like any other full voxel; the direction in its id is its own, not the spill's",
		},
		{
			name: "a source on either side leaves nowhere to go",
			table: blockTable{blocks: around(world.WaterFlow3, map[[3]int64]world.Block{
				east: world.Water, west: world.Water, north: world.WaterFlow3, south: world.WaterFlow3,
			}), fill: world.Air},
			want:   [3]float64{0, 0, 0},
			reason: "-5 east and -5 west are equal and opposite, the way two equal levels are",
		},
		{
			name: "ground cover is as empty as air",
			table: blockTable{blocks: around(world.WaterFlow3, map[[3]int64]world.Block{
				east: world.FlowerRed, west: world.WaterFlow3, north: world.WaterFlow3, south: world.WaterFlow3,
			}), fill: world.Air},
			want:   [3]float64{1, 0, 0},
			reason: "a flower stands in a voxel without filling it, so water goes there",
		},
		{
			name: "a flowing level with water above it is a fall",
			table: blockTable{blocks: around(world.WaterFlow4, map[[3]int64]world.Block{
				above: world.WaterFlow6, west: world.WaterFlow4, east: world.WaterFlow4,
				north: world.WaterFlow4, south: world.WaterFlow4,
			}), fill: world.Air},
			want:   [3]float64{0, -1, 0},
			reason: "the vertical is a flag; the flat neighbours leave the horizontal at zero",
		},
		{
			name: "a fall may also have somewhere to go sideways",
			table: blockTable{blocks: around(world.WaterFlow4, map[[3]int64]world.Block{
				above: world.Water, west: world.Stone, east: world.Air,
				north: world.Stone, south: world.Stone,
			}), fill: world.Air},
			want: [3]float64{1, -1, 0},
		},
		{
			name:   "air is not water",
			table:  blockTable{fill: world.Air},
			want:   [3]float64{0, 0, 0},
			reason: "and the swimmer is not in it, so nothing asks — but the function answers anyway",
		},
		{
			name:  "and neither is stone",
			table: blockTable{fill: world.Stone},
			want:  [3]float64{0, 0, 0},
		},
		{
			name:   "a voxel whose chunk has not arrived pushes nobody",
			table:  blockTable{blocks: around(world.WaterCurrentXPos, nil), fill: world.Water, absent: map[[3]int64]bool{{}: true}},
			want:   [3]float64{0, 0, 0},
			reason: "the tick may not wait for terrain, and a shove out of a world nobody can see is the one wrong answer",
		},
		{
			name: "an absent neighbour is skipped like a wall",
			table: blockTable{
				blocks: around(world.WaterFlow3, map[[3]int64]world.Block{
					west: world.Air, north: world.WaterFlow3, south: world.WaterFlow3,
				}),
				fill:   world.Air,
				absent: map[[3]int64]bool{east: true},
			},
			want: [3]float64{-1, 0, 0},
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			dx, dy, dz := FlowDirection(tc.table, 0, 0, 0)
			got := [3]float64{dx, dy, dz}
			for axis := range 3 {
				if math.Abs(got[axis]-tc.want[axis]) > 1e-9 {
					t.Fatalf("FlowDirection = %v, want %v (%s)", got, tc.want, tc.reason)
				}
			}
			// A horizontal answer is a *unit* direction or nothing at all: the speed is
			// CurrentSpeed's to state, and a magnitude leaking out of here would make it
			// depend on how steep the level difference happened to be.
			if magnitude := math.Hypot(dx, dz); magnitude > 1e-9 && math.Abs(magnitude-1) > 1e-9 {
				t.Errorf("horizontal magnitude %v, want 0 or 1", magnitude)
			}
		})
	}
}

// riverWorld is a channel with a bank: gravel bed up to bedTop, water from there up
// to waterTop, air above. The current runs only while x < stillFrom, so a swimmer can
// drift out of it into ordinary water without the test moving anybody by hand.
type riverWorld struct {
	bedTop    int64
	waterTop  int64
	current   world.Block
	stillFrom int64
	iceLid    bool
}

func (w riverWorld) Block(x, y, _ int64) (world.Block, bool) {
	switch {
	case y <= w.bedTop:
		return world.Gravel, true
	case w.iceLid && y == w.waterTop:
		return world.Ice, true
	case y <= w.waterTop:
		if x >= w.stillFrom {
			return world.Water, true
		}
		return w.current, true
	default:
		return world.Air, true
	}
}

func (w riverWorld) Solid(x, y, z int64) bool {
	block, resident := w.Block(x, y, z)
	return !resident || world.Solid(block)
}

func (w riverWorld) Fluid(x, y, z int64) bool { return fluidByBlock(w, x, y, z) }

// farBank is a stillFrom beyond anything these tests swim to: the current runs
// everywhere.
const farBank = 1 << 20

// hold re-submits one intent every tick, which is what a client with a key still
// pressed does. An intent submitted once expires after idleLimit ticks of silence,
// and every assertion below needs longer than that.
func hold(t *testing.T, h *vitalsHarness, p *Player, in protocol.PlayerInput, ticks int) {
	t.Helper()

	for tick := range ticks {
		in.ClientTick = uint32(tick + 1)
		if err := p.Submit(in); err != nil {
			t.Fatalf("Submit at tick %d: %v", tick, err)
		}
		h.step()
	}
}

// A swimmer who does nothing is carried, and is carried at CurrentSpeed rather than
// at whatever the ease happens to be passing through.
func TestIdleDriftReachesCurrentSpeedWithinASecondAndStopsThere(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, riverWorld{
		bedTop: 0, waterTop: 200, current: world.WaterCurrentXPos, stillFrom: farBank,
	})
	player, _ := h.join(1, [3]float32{0.5, 100, 0.5})

	start := h.position(player)
	for range DefaultTickRate {
		h.step()
		if got := float64(player.State().Vel[0]); got > CurrentSpeed+1e-9 {
			t.Fatalf("the drift overshot to %v blocks/s, past CurrentSpeed %v", got, CurrentSpeed)
		}
	}

	state := player.State()
	if got := float64(state.Vel[0]); math.Abs(got-CurrentSpeed) > 1e-6 {
		t.Errorf("idle drift settles at %v blocks/s, want CurrentSpeed %v", got, CurrentSpeed)
	}
	if got := float64(state.Vel[2]); math.Abs(got) > 1e-6 {
		t.Errorf("a +X current pushed the swimmer %v blocks/s along z", got)
	}
	if h.position(player)[0] <= start[0] {
		t.Errorf("the swimmer did not move downstream: x %v then %v", start[0], h.position(player)[0])
	}
}

// A river is fought, not lost to: CurrentSpeed is under SwimSpeed, so full opposing
// intent still makes headway upstream.
func TestSwimmingUpstreamStillMakesHeadway(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, riverWorld{
		bedTop: 0, waterTop: 200, current: world.WaterCurrentXPos, stillFrom: farBank,
	})
	player, _ := h.join(1, [3]float32{0.5, 100, 0.5})

	// yaw 0 puts +X to the swimmer's right, so a full -X intent is straight upstream.
	start := h.position(player)
	hold(t, h, player, protocol.PlayerInput{MoveX: -1}, DefaultTickRate)

	want := -(SwimSpeed - CurrentSpeed)
	if got := float64(player.State().Vel[0]); math.Abs(got-want) > 1e-6 {
		t.Errorf("swimming upstream settles at %v blocks/s, want SwimSpeed - CurrentSpeed = %v", got, want)
	}
	if got := h.position(player)[0]; got >= start[0] {
		t.Errorf("a second of full upstream intent ended at x %v, no further up than %v", got, start[0])
	}
	if CurrentSpeed >= SwimSpeed {
		t.Errorf("CurrentSpeed %v is not under SwimSpeed %v, so the current cannot be swum against at all", CurrentSpeed, SwimSpeed)
	}
}

// Nothing is stored: leaving the current leaves nothing behind, because the current
// was a target and never an accumulator.
func TestNoDriftSurvivesLeavingTheCurrent(t *testing.T) {
	t.Parallel()

	const bank = 6

	h := newVitalsHarness(t, DefaultTickRate, riverWorld{
		bedTop: 0, waterTop: 200, current: world.WaterCurrentXPos, stillFrom: bank,
	})
	player, _ := h.join(1, [3]float32{0.5, 100, 0.5})

	// Long enough to be carried out of the channel and well into the still water past
	// it: the drift covers two blocks a second and the bank is six blocks away.
	h.advance(8 * DefaultTickRate)

	if got := h.position(player)[0]; got <= bank {
		t.Fatalf("the swimmer is still at x %v, inside the current, so this proves nothing about leaving it", got)
	}
	if got := float64(player.State().Vel[0]); math.Abs(got) > 1e-6 {
		t.Errorf("a swimmer idle in still water is still moving at %v blocks/s", got)
	}
}

// Under a fall the water pulls harder than it does in a lake — and it is the fall
// that does it, which is what the still-water half of the comparison says.
func TestAWaterfallSinksFasterThanStillWater(t *testing.T) {
	t.Parallel()

	fall := newVitalsHarness(t, DefaultTickRate, riverWorld{
		bedTop: 0, waterTop: 200, current: world.WaterFlow3, stillFrom: farBank,
	})
	swimmer, _ := fall.join(1, [3]float32{0.5, 100, 0.5})
	fall.advance(DefaultTickRate)

	if got := float64(swimmer.State().Vel[1]); math.Abs(got-WaterfallSinkSpeed) > 1e-6 {
		t.Errorf("a swimmer under a fall sinks at %v blocks/s, want WaterfallSinkSpeed %v", got, WaterfallSinkSpeed)
	}

	still := newVitalsHarness(t, DefaultTickRate, riverWorld{
		bedTop: 0, waterTop: 200, current: world.Water, stillFrom: farBank,
	})
	floater, _ := still.join(1, [3]float32{0.5, 100, 0.5})
	still.advance(DefaultTickRate)

	if got := float64(floater.State().Vel[1]); math.Abs(got-SwimSinkSpeed) > 1e-6 {
		t.Errorf("a swimmer in still water sinks at %v blocks/s, want SwimSinkSpeed %v", got, SwimSinkSpeed)
	}
}

// The rise beats the fall: a swimmer under a waterfall climbs out of it rather than
// being pinned there.
func TestAJumpIntentUnderAWaterfallStillRises(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, riverWorld{
		bedTop: 0, waterTop: 200, current: world.WaterFlow3, stillFrom: farBank,
	})
	player, _ := h.join(1, [3]float32{0.5, 100, 0.5})
	h.advance(DefaultTickRate)

	before := h.position(player)
	hold(t, h, player, protocol.PlayerInput{Jump: true}, DefaultTickRate)

	if got := float64(player.State().Vel[1]); got <= 0 {
		t.Fatalf("holding the rise under a fall left the vertical speed at %v", got)
	}
	// The tick sets SwimRiseSpeed and then eases one step toward the sink target, which
	// is SwimSinkSpeed while the jump is held rather than WaterfallSinkSpeed.
	want := SwimRiseSpeed - SwimAcceleration/float64(DefaultTickRate)
	if got := float64(player.State().Vel[1]); math.Abs(got-want) > 1e-6 {
		t.Errorf("the rise under a fall is %v blocks/s, want SwimRiseSpeed eased by one tick to %v", got, want)
	}
	if h.position(player)[1] <= before[1] {
		t.Errorf("the swimmer did not climb: y %v then %v", before[1], h.position(player)[1])
	}
}

// The current pushes a body that is standing on the bed, and does not push one
// standing on the lid — because the lid is not water and the rule is the box test it
// always was.
func TestTheBedIsPushedAndTheIceLidIsNot(t *testing.T) {
	t.Parallel()

	river := riverWorld{bedTop: 40, waterTop: 44, current: world.WaterCurrentXPos, stillFrom: farBank}
	wet := newVitalsHarness(t, DefaultTickRate, river)
	wader, _ := wet.join(1, [3]float32{0.5, float32(river.bedTop + 1), 0.5})
	wet.advance(DefaultTickRate)

	if !wader.State().OnGround {
		t.Fatalf("the wader is not standing on the bed; it is at y %v", wader.State().Pos[1])
	}
	if got := float64(wader.State().Vel[0]); math.Abs(got-CurrentSpeed) > 1e-6 {
		t.Errorf("a wader on the river bed drifts at %v blocks/s, want CurrentSpeed %v", got, CurrentSpeed)
	}

	frozen := river
	frozen.iceLid = true
	dry := newVitalsHarness(t, DefaultTickRate, frozen)
	stander, _ := dry.join(2, [3]float32{0.5, float32(frozen.waterTop + 3), 0.5})
	dry.advance(DefaultTickRate)

	if !stander.State().OnGround {
		t.Fatalf("the stander is not on the lid; it is at y %v", stander.State().Pos[1])
	}
	if got := float64(stander.State().Vel[0]); got != 0 {
		t.Errorf("a player standing on ice over a current drifts at %v blocks/s", got)
	}
}

// A fall into a current is a fall into water, and no fall into water hurts.
func TestNoFallIntoACurrentHurts(t *testing.T) {
	t.Parallel()

	river := riverWorld{bedTop: 40, waterTop: 44, current: world.WaterCurrentXPos, stillFrom: farBank}
	h := newVitalsHarness(t, DefaultTickRate, river)
	player, _ := h.join(1, [3]float32{0.5, float32(river.waterTop + 30), 0.5})
	h.advance(8 * DefaultTickRate)

	if got := h.vitals(player).Health; got != PlayerMaxHealth {
		t.Errorf("a thirty-block fall into a current cost %d health", PlayerMaxHealth-got)
	}
}
