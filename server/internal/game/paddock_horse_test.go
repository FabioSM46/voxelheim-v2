package game

import (
	"math"
	"slices"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// What the lap is measured against, in blocks. The horse the client draws is 2.3 long
// nose to tail; the clearance is the half block the issue asks for between either end
// and the wall; the spacing is the least any two horses may ever come, centre to centre.
const (
	testPaddockHorseLength = 2.3
	testPaddockWallMargin  = 0.5
	testPaddockMinSpacing  = 2.5
)

func paddockAnchors(t *testing.T, settled world.Settlement) []world.PlacedAnchor {
	t.Helper()
	var anchors []world.PlacedAnchor
	for _, slot := range settled.Anchors() {
		if slot.Kind == world.AnchorPaddock {
			anchors = append(anchors, slot)
		}
	}
	if len(anchors) != paddockHorseVariants {
		t.Fatalf("the capital has %d paddock anchors, want %d", len(anchors), paddockHorseVariants)
	}
	return anchors
}

// paddockTrio is the capital's three paddock anchors, sorted as the materialiser sorts
// them.
func paddockTrio(t *testing.T, settled world.Settlement) [paddockHorseVariants]world.PlacedAnchor {
	t.Helper()
	trio := [paddockHorseVariants]world.PlacedAnchor(paddockAnchors(t, settled))
	slices.SortFunc(trio[:], paddockAnchorOrder)
	return trio
}

func lookAtPaddock(h *structureHarness, anchors []world.PlacedAnchor) {
	for _, slot := range anchors {
		h.look(world.ChunkOf(slot.X, slot.Y, slot.Z))
	}
}

// paddockLapTicks is one whole lap; every test that walks the oval walks all of it.
func paddockLapTicks() uint64 {
	return uint64(paddockHorseLapSeconds * DefaultTickRate)
}

// paddockFrame reads a world position in the oval's own frame: blocks along the long
// axis from the centre, and blocks across it.
func paddockFrame(route paddockRoute, pos [3]float64) (along, across float64) {
	dx := pos[0] - (float64(route.anchor[0]) + 0.5)
	dz := pos[2] - (float64(route.anchor[2]) + 0.5)
	axisX, axisZ := route.axis[0], route.axis[1]
	return dx*axisX + dz*axisZ, dx*axisZ - dz*axisX
}

// onPaddockOval is how far a position is from the oval, as the ellipse equation's
// departure from one: zero on the curve, positive outside it.
func onPaddockOval(route paddockRoute, pos [3]float64) float64 {
	along, across := paddockFrame(route, pos)
	return (along/paddockRouteLongAxis)*(along/paddockRouteLongAxis) +
		(across/paddockRouteShortAxis)*(across/paddockRouteShortAxis) - 1
}

// paddockForward is the way a horse at yaw faces, in the movement basis both sides
// spell out: yaw 0 looks along -Z.
func paddockForward(yaw float64) [2]float64 {
	sinYaw, cosYaw := math.Sincos(yaw)
	return [2]float64{-sinYaw, -cosYaw}
}

func TestCapitalPaddockMaterialisesOneHorseOfEachCoat(t *testing.T) {
	t.Parallel()
	h := newStructureHarness(t)
	capital := testCapital(t)
	anchors := paddockAnchors(t, capital)
	route := paddockRouteOf(paddockTrio(t, capital))
	lookAtPaddock(h, anchors)
	residentIDs := make(map[uint64]struct{})
	for _, slot := range residentAnchors(t, capital) {
		residentIDs[residentID(testWorldSeed, slot.X, slot.Z)] = struct{}{}
	}

	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	if got := len(h.sim.paddockHorses); got != paddockHorseVariants {
		t.Fatalf("%d paddock horses stand after looking, want %d", got, paddockHorseVariants)
	}
	if len(h.sim.mobs) != 0 || len(h.sim.corpses) != 0 {
		t.Fatalf("looking at the paddock made %d mobs and %d corpses", len(h.sim.mobs), len(h.sim.corpses))
	}

	variants := [paddockHorseVariants]int{}
	for id, horse := range h.sim.paddockHorses {
		if id&residentBit == 0 {
			t.Errorf("horse %d can collide with the minted allocator range", id)
		}
		variant := id & paddockHorseVariantMask
		if variant >= paddockHorseVariants {
			t.Fatalf("horse %d carries presentation seed %d", id, variant)
		}
		variants[variant]++
		if _, collision := residentIDs[id]; collision {
			t.Errorf("horse %d collides with a resident", id)
		}
		if horse.route != route || uint64(horse.variant) != variant {
			t.Errorf("horse %d carries route %+v variant %d, want the stable's %+v variant %d",
				id, horse.route, horse.variant, route, variant)
		}
		state := horse.state()
		if state.Kind != vnet.MobKindHorse || state.Action != vnet.MobActionIdle || state.TargetEntityID != 0 {
			t.Errorf("horse %d projects as kind=%s action=%s target=%d", id, state.Kind, state.Action, state.TargetEntityID)
		}
	}
	if variants != [paddockHorseVariants]int{1, 1, 1} {
		t.Errorf("coat presentation seeds are %v, want one of each", variants)
	}

	projected := h.sim.mobSnapshotsLocked(nil)
	horses := 0
	for _, shown := range projected {
		if shown.state.Kind == vnet.MobKindHorse {
			horses++
			if shown.resident != nil || shown.corpse != nil {
				t.Errorf("horse %d carries resident or corpse metadata", shown.state.EntityID)
			}
		}
	}
	if horses != paddockHorseVariants {
		t.Errorf("snapshot contains %d horses, want %d", horses, paddockHorseVariants)
	}
}

func TestPaddockRouteIsPurePeriodicAndBoundedToItsAnchor(t *testing.T) {
	t.Parallel()
	trio := paddockTrio(t, testCapital(t))
	route := paddockRouteOf(trio)
	middle := trio[1]
	if route.anchor != [3]int64{middle.X, middle.Y, middle.Z} {
		t.Fatalf("the oval is centred on %v, want the middle anchor %+v", route.anchor, middle)
	}
	if reversed := [paddockHorseVariants]world.PlacedAnchor{trio[2], trio[1], trio[0]}; paddockRouteOf(reversed) != route {
		t.Errorf("the trio handed over backwards gives route %+v, want %+v", paddockRouteOf(reversed), route)
	}

	dt := 1 / float64(DefaultTickRate)
	lapTicks := paddockLapTicks()
	for variant := uint8(0); variant < paddockHorseVariants; variant++ {
		first, firstYaw := paddockHorsePose(testWorldSeed, route, variant, 0, dt)
		again, againYaw := paddockHorsePose(testWorldSeed, route, variant, lapTicks, dt)
		for axis := range first {
			if math.Abs(first[axis]-again[axis]) > 1e-9 {
				t.Errorf("variant %d axis %d after one lap = %.12f, want %.12f", variant, axis, again[axis], first[axis])
			}
		}
		if math.Abs(wrapAngle(firstYaw-againYaw)) > 1e-9 {
			t.Errorf("variant %d yaw after one lap = %.12f, want %.12f", variant, againYaw, firstYaw)
		}

		for tick := uint64(0); tick <= lapTicks; tick++ {
			one, yaw := paddockHorsePose(testWorldSeed, route, variant, tick, dt)
			two, yawTwo := paddockHorsePose(testWorldSeed, route, variant, tick, dt)
			if one != two || yaw != yawTwo {
				t.Fatalf("variant %d tick %d is not a pure route sample", variant, tick)
			}
			if off := onPaddockOval(route, one); math.Abs(off) > 1e-9 || one[1] != float64(middle.Y) {
				t.Errorf("variant %d tick %d is at %v, off the oval by %.12f", variant, tick, one, off)
			}
		}
	}
}

// The lap is walked against the stable's own drawing rather than against the 17 × 9 the
// issue quotes, so the number the test trusts is the wall the builder places. The
// drawing is a placement at the origin facing +Z, which makes its paddock anchors placed
// anchors as they stand and a cell of the route a cell of the drawing.
func TestPaddockLapKeepsNoseAndTailInsideTheWall(t *testing.T) {
	t.Parallel()
	schematic := world.SchematicFor(world.BuildingStable)
	var drawn []world.PlacedAnchor
	for _, slot := range schematic.Anchors {
		if slot.Kind == world.AnchorPaddock {
			drawn = append(drawn, world.PlacedAnchor{X: int64(slot.X), Y: int64(slot.Y), Z: int64(slot.Z), Kind: slot.Kind})
		}
	}
	if len(drawn) != paddockHorseVariants {
		t.Fatalf("the stable drawing has %d paddock anchors, want %d", len(drawn), paddockHorseVariants)
	}
	route := paddockRouteOf([paddockHorseVariants]world.PlacedAnchor(drawn))

	// Every cell within the margin of a point, at the floor and the layer above it, must
	// be air: a point half a block from a wall has that wall's cell inside its margin.
	clear := func(variant uint8, tick uint64, end string, px, pz float64) {
		t.Helper()
		for _, corner := range [4][2]float64{{-1, -1}, {-1, 1}, {1, -1}, {1, 1}} {
			x := int(math.Floor(px + corner[0]*testPaddockWallMargin))
			z := int(math.Floor(pz + corner[1]*testPaddockWallMargin))
			if x < 0 || x >= schematic.W || z < 0 || z >= schematic.D {
				t.Fatalf("variant %d tick %d: %s at (%.3f, %.3f) reaches outside the drawing", variant, tick, end, px, pz)
			}
			for y := 0; y < 2; y++ {
				if schematic.At(x, y, z) != world.Air {
					t.Fatalf("variant %d tick %d: %s at (%.3f, %.3f) is within %.1f of the wall cell (%d, %d, %d)",
						variant, tick, end, px, pz, testPaddockWallMargin, x, y, z)
				}
			}
		}
	}

	dt := 1 / float64(DefaultTickRate)
	half := testPaddockHorseLength / 2
	for variant := uint8(0); variant < paddockHorseVariants; variant++ {
		for tick := uint64(0); tick <= paddockLapTicks(); tick++ {
			pos, yaw := paddockHorsePose(testWorldSeed, route, variant, tick, dt)
			forward := paddockForward(yaw)
			clear(variant, tick, "nose", pos[0]+half*forward[0], pos[2]+half*forward[1])
			clear(variant, tick, "tail", pos[0]-half*forward[0], pos[2]-half*forward[1])
		}
	}
}

func TestPaddockHorsesAreAThirdOfALapApart(t *testing.T) {
	t.Parallel()
	route := paddockRouteOf(paddockTrio(t, testCapital(t)))
	dt := 1 / float64(DefaultTickRate)
	lapTicks := paddockLapTicks()
	third := lapTicks / paddockHorseVariants
	if third*paddockHorseVariants != lapTicks {
		t.Fatalf("a lap of %d ticks does not divide into thirds", lapTicks)
	}

	closest := math.Inf(1)
	for tick := uint64(0); tick <= lapTicks; tick++ {
		var at [paddockHorseVariants][3]float64
		for variant := range at {
			at[variant], _ = paddockHorsePose(testWorldSeed, route, uint8(variant), tick, dt)
		}
		// Exactly a third: variant v stands where variant 0 will stand v thirds of a lap
		// later, so the spacing is the phase and nothing else.
		for variant := uint64(1); variant < paddockHorseVariants; variant++ {
			later, _ := paddockHorsePose(testWorldSeed, route, 0, tick+variant*third, dt)
			for axis := range later {
				if math.Abs(at[variant][axis]-later[axis]) > 1e-9 {
					t.Fatalf("tick %d: variant %d is at %v, want variant 0's %v from %d ticks on",
						tick, variant, at[variant], later, variant*third)
				}
			}
		}
		for a := range at {
			for b := a + 1; b < len(at); b++ {
				closest = math.Min(closest, math.Hypot(at[a][0]-at[b][0], at[a][2]-at[b][2]))
			}
		}
	}
	if closest < testPaddockMinSpacing {
		t.Errorf("two horses come within %.3f blocks of each other, want at least %.1f", closest, testPaddockMinSpacing)
	}
	// The value the route's comment claims: a third of a lap apart on an ellipse is never
	// closer than √3 times the short semi-axis. The samples are 1.2° apart, so the
	// walked minimum sits within a thousandth of the analytic one.
	if want := math.Sqrt(3) * paddockRouteShortAxis; math.Abs(closest-want) > 0.01 {
		t.Errorf("the closest approach over a lap is %.4f, want %.4f", closest, want)
	}
}

func TestPaddockLapIsAWalk(t *testing.T) {
	t.Parallel()
	route := paddockRouteOf(paddockTrio(t, testCapital(t)))
	dt := 1 / float64(DefaultTickRate)
	perimeter := 0.0
	previous, _ := paddockHorsePose(testWorldSeed, route, 0, 0, dt)
	for tick := uint64(1); tick <= paddockLapTicks(); tick++ {
		current, _ := paddockHorsePose(testWorldSeed, route, 0, tick, dt)
		perimeter += math.Hypot(current[0]-previous[0], current[2]-previous[2])
		previous = current
	}
	// Ramanujan's perimeter for 5.5 × 2.5 is π(3(a+b) − √((3a+b)(a+3b))) ≈ 26.0 blocks.
	if perimeter < 25.5 || perimeter > 26.5 {
		t.Errorf("one lap walks %.3f blocks, want about 26", perimeter)
	}
	if speed := perimeter / paddockHorseLapSeconds; speed < 1.6 || speed > 1.8 {
		t.Errorf("the lap averages %.3f blocks per second, want about 1.7", speed)
	}
}

func TestPaddockYawIsTheTangentOfTravel(t *testing.T) {
	t.Parallel()
	route := paddockRouteOf(paddockTrio(t, testCapital(t)))
	dt := 1 / float64(DefaultTickRate)
	for variant := uint8(0); variant < paddockHorseVariants; variant++ {
		for tick := uint64(1); tick <= paddockLapTicks(); tick++ {
			before, _ := paddockHorsePose(testWorldSeed, route, variant, tick-1, dt)
			after, _ := paddockHorsePose(testWorldSeed, route, variant, tick+1, dt)
			_, yaw := paddockHorsePose(testWorldSeed, route, variant, tick, dt)
			// The chord between the neighbouring ticks is exactly parallel to the tangent
			// at this one on an ellipse, so the agreement is to rounding, not to a step.
			dx, dz := after[0]-before[0], after[2]-before[2]
			forward := paddockForward(yaw)
			if dot := (dx*forward[0] + dz*forward[1]) / math.Hypot(dx, dz); dot < 1-1e-9 {
				t.Fatalf("variant %d tick %d: yaw %.6f faces %v, travel is (%.6f, %.6f)", variant, tick, yaw, forward, dx, dz)
			}
		}
	}
}

// The materialiser sorts the trio in world coordinates so that the stable's Facing does
// not matter, and this is the test of that claim: the same trio turned by each Facing —
// the enum value is the number of quarter turns, as rotateCell reads it — gives the same
// oval turned with it, walked the same way round, whichever order the sort then hands
// the trio over in.
func TestPaddockRouteIsTheSameWhicheverWayTheStableFaces(t *testing.T) {
	t.Parallel()
	trio := paddockTrio(t, testCapital(t))
	route := paddockRouteOf(trio)
	pivot := trio[1]
	dt := 1 / float64(DefaultTickRate)
	for facing := world.FacingPlusZ; facing <= world.FacingPlusX; facing++ {
		turned := trio
		for i := range turned {
			dx, dz := turned[i].X-pivot.X, turned[i].Z-pivot.Z
			for range int(facing) {
				dx, dz = -dz, dx
			}
			turned[i].X, turned[i].Z = pivot.X+dx, pivot.Z+dz
		}
		slices.SortFunc(turned[:], paddockAnchorOrder)
		got := paddockRouteOf(turned)
		if got.anchor != route.anchor {
			t.Errorf("facing %d moves the centre to %v from %v", facing, got.anchor, route.anchor)
		}
		axisX, axisZ := route.axis[0], route.axis[1]
		for range int(facing) {
			axisX, axisZ = -axisZ, axisX
		}
		if math.Abs(got.axis[0]*axisX+got.axis[1]*axisZ) < 1-1e-12 {
			t.Errorf("facing %d gives long axis %v, want the turned %v up to sign", facing, got.axis, [2]float64{axisX, axisZ})
		}
		// Turned back about the centre, every sample of every horse's lap lies on the
		// unturned oval; and the lap runs the same way round, read as the sign of the
		// radius crossed with the travel, which a turn cannot flip and the sort's choice
		// of axis sign must not either.
		sense := func(on paddockRoute) float64 {
			here, _ := paddockHorsePose(testWorldSeed, on, 0, 0, dt)
			next, _ := paddockHorsePose(testWorldSeed, on, 0, 1, dt)
			dx, dz := here[0]-(float64(on.anchor[0])+0.5), here[2]-(float64(on.anchor[2])+0.5)
			return dx*(next[2]-here[2]) - dz*(next[0]-here[0])
		}
		if sense(got)*sense(route) <= 0 {
			t.Errorf("facing %d walks the oval the other way round", facing)
		}
		for variant := uint8(0); variant < paddockHorseVariants; variant++ {
			for tick := uint64(0); tick <= paddockLapTicks(); tick++ {
				pos, _ := paddockHorsePose(testWorldSeed, got, variant, tick, dt)
				dx, dz := pos[0]-(float64(pivot.X)+0.5), pos[2]-(float64(pivot.Z)+0.5)
				for range int(facing) {
					dx, dz = dz, -dx
				}
				back := [3]float64{float64(pivot.X) + 0.5 + dx, pos[1], float64(pivot.Z) + 0.5 + dz}
				if off := onPaddockOval(route, back); math.Abs(off) > 1e-9 {
					t.Fatalf("facing %d variant %d tick %d stands at %v, which turned back is off the oval by %.12f",
						facing, variant, tick, pos, off)
				}
			}
		}
	}
}

func TestTwoServersAtTheSameWorldTickDeriveTheSamePaddock(t *testing.T) {
	t.Parallel()
	capital := testCapital(t)
	anchors := paddockAnchors(t, capital)
	route := paddockRouteOf(paddockTrio(t, capital))
	derive := func() map[uint64]paddockHorse {
		h := newStructureHarness(t)
		h.sim.mu.Lock()
		h.sim.worldTick = 4321
		h.sim.mu.Unlock()
		lookAtPaddock(h, anchors)
		h.sim.mu.Lock()
		defer h.sim.mu.Unlock()
		out := make(map[uint64]paddockHorse, len(h.sim.paddockHorses))
		for id, horse := range h.sim.paddockHorses {
			out[id] = *horse
		}
		return out
	}

	first, second := derive(), derive()
	if len(first) != paddockHorseVariants || len(second) != paddockHorseVariants {
		t.Fatalf("two servers derived %d and %d horses", len(first), len(second))
	}
	for id, before := range first {
		if after, exists := second[id]; !exists || after != before {
			t.Errorf("horse %d differs across equal seeds and world ticks: %+v / %+v", id, before, after)
		}
		if before.route != route || math.Abs(onPaddockOval(route, before.pos)) > 1e-9 {
			t.Errorf("horse %d stands at %v on route %+v, want the stable's oval %+v", id, before.pos, before.route, route)
		}
	}
}

func TestPaddockHorsesAreNotAddressableResidents(t *testing.T) {
	t.Parallel()
	h := newStructureHarness(t)
	anchors := paddockAnchors(t, testCapital(t))
	lookAtPaddock(h, anchors)

	h.sim.mu.Lock()
	var horse *paddockHorse
	for _, standing := range h.sim.paddockHorses {
		horse = standing
		break
	}
	h.sim.mu.Unlock()
	if horse == nil {
		t.Fatal("no horse materialised")
	}
	player, _ := h.join(1, [3]float32{float32(horse.pos[0]), float32(horse.pos[1]), float32(horse.pos[2])})
	reason, err := player.InteractNPC(protocol.NpcInteractRequest{EntityID: horse.entityID, ClientTick: 1})
	if err == nil || reason != vnet.RefusalReasonNotAVendor {
		t.Fatalf("NpcInteract with horse: reason %s, error %v", reason, err)
	}
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	if len(h.sim.mobs) != 0 {
		t.Errorf("interaction made %d addressable mobs", len(h.sim.mobs))
	}
}
