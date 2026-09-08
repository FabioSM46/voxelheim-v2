package game

import (
	"fmt"
	"math"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func TestEscapeRouteRequiresBodyWidthThroughAnOpening(t *testing.T) {
	terrain := scriptedTerrain{want: func(x, y, z int64) bool {
		return y <= 0 || (x == 2 && y <= 4 && z != 0)
	}}
	sim := &Sim{terrain: terrain, dt: 1.0 / float64(DefaultTickRate)}
	ticks := ticksFor(time.Second, DefaultTickRate)
	for _, tc := range []struct {
		name string
		z    float64
		want bool
	}{
		{"centred body fits one block", 0.5, true},
		{"centre fits but shoulder hits left jamb", 0.1, false},
		{"centre fits but shoulder hits right jamb", 0.9, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			start := [3]float64{0.5, 1, tc.z}
			endpoint := [3]float64{0.5 + WalkSpeed, 1, tc.z}
			if overlaps(terrain, playerBox(start)) || overlaps(terrain, playerBox(endpoint)) {
				t.Fatal("fixture endpoints must both fit the body")
			}
			if !clearLineOfSight(terrain, boxCentre(playerBox(start)), boxCentre(playerBox(endpoint))) {
				t.Fatal("fixture must fool a centre ray")
			}
			got, ok := sim.walkEscapeRoute(start, 0, ticks)
			if ok != tc.want {
				t.Fatalf("route accepted = %v, want %v", ok, tc.want)
			}
			if ok && math.Abs(got[0]-endpoint[0]) > collisionSkin {
				t.Fatalf("arrived at %v, want %v", got, endpoint)
			}
		})
	}
}

func TestEscapeRouteUsesOrdinaryStepUpAndActualDestinationHeight(t *testing.T) {
	for _, tc := range []struct {
		name  string
		block world.Block
		want  bool
	}{
		{"half slab is walkable", world.SlateSlabBottom, true},
		{"full block needs a jump", world.Stone, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			terrain := blockTerrain{blocks: map[[3]int64]world.Block{{1, 1, 0}: tc.block}}
			sim := &Sim{terrain: terrain, dt: 1.0 / float64(DefaultTickRate)}
			got, ok := sim.walkEscapeRoute([3]float64{0.5, 1, 0.5}, 0, ticksFor(250*time.Millisecond, DefaultTickRate))
			if ok != tc.want {
				t.Fatalf("route accepted = %v, want %v", ok, tc.want)
			}
			if ok && (math.Abs(got[1]-1.5) > 2*collisionSkin || overlaps(terrain, playerBox(got))) {
				t.Fatalf("slab endpoint = %v, want clear body standing at height 1.5", got)
			}
		})
	}
}

func TestEscapeUsesQuantizedWarningRatherThanUnplayableFractionOfATick(t *testing.T) {
	// At 3 Hz, 900 ms is only two preparation ticks. A radius of 3.3
	// covers every two-tick endpoint, but leaves the nominal 900 ms ones safe.
	for _, rate := range []uint8{3, DefaultTickRate} {
		t.Run(fmt.Sprint(rate), func(t *testing.T) {
			h := newVitalsHarness(t, rate, dropTerrain{groundTop: 63})
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			king := pullKing(t, h, [3]float64{0.5, 64, 0.5}, player)
			h.sim.mu.Lock()
			defer h.sim.mu.Unlock()
			def := encounterMoveDef{
				kind: vnet.EncounterMoveKindBurial, fromStage: 1,
				telegraph: 900 * time.Millisecond, release: 200 * time.Millisecond,
				recovery: time.Second, minRange: 0, maxRange: 40, damagePercent: 10,
				hazard: encounterHazard{shape: vnet.HazardShapeDisc, reach: 3.3, height: 8},
			}
			if got, want := h.sim.moveLeavesAnEscapeLocked(h.sim.mobs[king], def, player), rate == DefaultTickRate; got != want {
				t.Fatalf("escape at %d Hz = %v, want %v", rate, got, want)
			}
		})
	}
}
