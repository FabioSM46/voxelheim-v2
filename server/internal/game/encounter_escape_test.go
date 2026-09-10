package game

import (
	"fmt"
	"math"
	"slices"
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

// engagementReach is the farthest a player's nearest damage sample can stand from a body's
// centre, over every bearing, while the two axis-aligned boxes are still within SwordReach:
// the ground a player can strike this body from. An axis-aligned gap reaches farthest on the
// diagonal, which is why the bearings are swept rather than the front alone measured.
func engagementReach(b body) float64 {
	worst := 0.0
	for tenth := range 901 {
		angle := float64(tenth) / 10 * math.Pi / 180
		at := func(d float64) [3]float64 { return [3]float64{d * math.Cos(angle), 0, d * math.Sin(angle)} }
		near, far := 0.0, 20.0
		for range 60 {
			if mid := (near + far) / 2; boxDistance(b.boxAt([3]float64{}), playerBox(at(mid))) < SwordReach {
				near = mid
			} else {
				far = mid
			}
		}
		closest := math.Inf(1)
		for _, sample := range horizontalSamples(playerBox(at(near))) {
			closest = min(closest, math.Hypot(sample[0], sample[1]))
		}
		worst = max(worst, closest)
	}
	return worst
}

// Every blow that plants its creature reaches no farther than the ground a player can strike
// that creature from, rounded up to a tenth. Reach beyond it damages players who could not be
// hitting back, and it is the part of the gap to the visible strike no engagement explains.
// The guardian's bite, claws and jaws sit within its 3.7; the king's strokes were cut to 3.3.
func TestPlantedBlowsReachNoFartherThanTheirEngagement(t *testing.T) {
	for kind, repertoire := range encounterMoveCatalog {
		ceiling := math.Ceil(engagementReach(mobRegistry[kind].body)*10) / 10
		for _, def := range repertoire {
			blows := []encounterMoveDef{def}
			if def.combo != nil {
				blows = blows[:0]
				for step := uint8(1); step <= def.combo.total; step++ {
					blows = append(blows, def.forComboStep(step))
				}
			}
			for _, blow := range blows {
				if blow.travel != travelNone || blow.flightSpeed > 0 || blow.hazard.pulse != pulseNone {
					continue
				}
				if radius := blow.announcedRadius(); radius > ceiling {
					t.Errorf("%s/%s reaches %.2f, beyond the engagement reach %.1f", kind, blow.kind, radius, ceiling)
				}
			}
		}
	}
	if got := math.Ceil(engagementReach(mobRegistry[vnet.MobKindDraugrKing].body)*10) / 10; got != 3.3 {
		t.Fatalf("king engagement reach = %.1f, want the 3.3 the catalogue was cut to", got)
	}
}

// A target at sword reach on the guardian's diagonal stands outside every region its planted
// moves would announce, so none is chosen there and the guardian keeps closing; the same gap in
// front is inside the claws' cone. The body-to-body band admits both.
func TestAPlantedMoveIsChosenOnlyWhereItsRegionReachesTheTarget(t *testing.T) {
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 6.5})
	at := [3]float64{0.5, 64, 0.5}
	id := pullGuardian(t, h, at, player)
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	guardian := h.sim.mobs[id]
	body := guardian.species().body.boxAt(at)
	place := func(angle float64) {
		near, far := 0.0, 20.0
		for range 60 {
			mid := (near + far) / 2
			pos := [3]float64{at[0] + mid*math.Cos(angle), at[1], at[2] + mid*math.Sin(angle)}
			if boxDistance(body, playerBox(pos)) < SwordReach-0.05 {
				near = mid
			} else {
				far = mid
			}
		}
		player.pos = [3]float64{at[0] + near*math.Cos(angle), at[1], at[2] + near*math.Sin(angle)}
	}

	place(math.Pi / 4)
	if def, chosen := guardian.selectEncounterMoveLocked(h.sim, player); chosen {
		t.Fatalf("chose %s for a diagonal target no planted region reaches", def.kind)
	}
	place(math.Pi / 2)
	def, chosen := guardian.selectEncounterMoveLocked(h.sim, player)
	planted := []vnet.EncounterMoveKind{vnet.EncounterMoveKindBiteAndTear, vnet.EncounterMoveKindPrisonerClaws}
	if !chosen || !slices.Contains(planted, def.kind) || !guardian.announcedRegionReaches(def, player) {
		t.Fatalf("front target at the same gap: chose %v (%s); want a planted move whose region reaches it", chosen, def.kind)
	}
}
