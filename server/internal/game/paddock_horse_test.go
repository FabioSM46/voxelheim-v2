package game

import (
	"math"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
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

func lookAtPaddock(h *structureHarness, anchors []world.PlacedAnchor) {
	for _, slot := range anchors {
		h.look(world.ChunkOf(slot.X, slot.Y, slot.Z))
	}
}

func TestCapitalPaddockMaterialisesOneHorseOfEachCoat(t *testing.T) {
	t.Parallel()
	h := newStructureHarness(t)
	capital := testCapital(t)
	anchors := paddockAnchors(t, capital)
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
	anchor := paddockAnchors(t, testCapital(t))[0]
	column := [3]int64{anchor.X, anchor.Y, anchor.Z}
	dt := 1 / float64(DefaultTickRate)
	lapTicks := uint64(paddockHorseLapSeconds * DefaultTickRate)
	first, firstYaw := paddockHorsePose(testWorldSeed, column, 0, dt)
	again, againYaw := paddockHorsePose(testWorldSeed, column, lapTicks, dt)
	for axis := range first {
		if math.Abs(first[axis]-again[axis]) > 1e-12 {
			t.Errorf("axis %d after one lap = %.12f, want %.12f", axis, again[axis], first[axis])
		}
	}
	if math.Abs(wrapAngle(firstYaw-againYaw)) > 1e-12 {
		t.Errorf("yaw after one lap = %.12f, want %.12f", againYaw, firstYaw)
	}

	centreX, centreZ := float64(anchor.X)+0.5, float64(anchor.Z)+0.5
	for tick := uint64(0); tick <= lapTicks; tick++ {
		one, yaw := paddockHorsePose(testWorldSeed, column, tick, dt)
		two, yawTwo := paddockHorsePose(testWorldSeed, column, tick, dt)
		if one != two || yaw != yawTwo {
			t.Fatalf("tick %d is not a pure route sample", tick)
		}
		distance := math.Hypot(one[0]-centreX, one[2]-centreZ)
		if math.Abs(distance-paddockHorseRadius) > 1e-12 || one[1] != float64(anchor.Y) {
			t.Errorf("tick %d is at %v, radius %.12f", tick, one, distance)
		}
	}
}

func TestTwoServersAtTheSameWorldTickDeriveTheSamePaddock(t *testing.T) {
	t.Parallel()
	anchors := paddockAnchors(t, testCapital(t))
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
