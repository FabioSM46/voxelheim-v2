package game

import (
	"fmt"
	"math"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// These are study fixtures, not the live instance drawing. #1022 owns that drawing.
// Exercise Player.step and the production swept collision against the proposed
// clear floor, including an obstacle that a distance-only calculation would miss.
func dungeonStudyTerrain(outer int64, monoliths bool) scriptedTerrain {
	half := outer / 2
	return scriptedTerrain{want: func(x, y, z int64) bool {
		if y <= 0 || y >= 9 || x <= -half || x >= half || z <= -half || z >= half {
			return true
		}
		return monoliths && y <= 4 && (x == -8 || x == 8) && (z == -8 || z == 8)
	}}
}

func dungeonStudyWalk(terrain Terrain, start [3]float64, rate int, hunger uint16, seconds, dx, dz float64) [3]float64 {
	p := Player{
		sim: &Sim{idleLimit: rate}, pos: start, health: PlayerMaxHealth,
		hunger: hunger, onGround: true, lifeState: vnet.LifeStateAlive,
	}
	length := math.Hypot(dx, dz)
	for range int(math.Floor(seconds * float64(rate))) {
		// A fresh full-intent sample every tick, as a connected moving player sends.
		// Normalise before the internal step; the public Submit boundary does this.
		p.current = intent{moveX: dx / length, moveZ: -dz / length}
		p.idleTicks = 0
		p.step(1/float64(rate), terrain)
	}
	return p.pos
}

func TestDungeonStudyEscapeWindows(t *testing.T) {
	for _, arena := range []struct {
		name    string
		outer   int64
		pillars bool
	}{{"courtyard", 29, true}, {"hall", 33, false}} {
		for _, rate := range []int{20, 60} {
			for _, hunger := range []uint16{0, 1} {
				for _, preparation := range []float64{0.9, 1.2, 1.5} {
					name := fmt.Sprintf("%s/%dHz/hunger%d/%.1fs", arena.name, rate, hunger, preparation)
					t.Run(name, func(t *testing.T) {
						terrain := dungeonStudyTerrain(arena.outer, arena.pillars)
						available := preparation - 0.25 - 1/float64(rate)
						start := [3]float64{0, 1, 0}
						pos := dungeonStudyWalk(terrain, start, rate, hunger, available, 1, 0)
						// Whole footprint outside a strip ending at X=target; reaching
						// the boundary with the centre alone still leaves a hittable body.
						clearance := playerBox(pos).min[0]
						if overlaps(terrain, playerBox(pos)) {
							t.Fatal("escape entered solid terrain")
						}
						if clearance < 1.5 {
							t.Fatalf("smallest arc escape fails: whole-body clearance %.3f", clearance)
						}
						if preparation >= 1.2 && clearance < 2 {
							t.Fatalf("landing/sector escape fails: clearance %.3f", clearance)
						}
						if hunger == 0 && preparation == 0.9 && clearance >= 2 {
							t.Fatal("negative control: 0.9s unexpectedly admits a 2-block slow escape")
						}
						t.Logf("whole-body clearance %.3f blocks; 2-block target accepted=%v", clearance, clearance >= 2)
					})
				}
			}
		}
	}
}

func TestDungeonStudyWallsMonolithsAndAlternativeEscape(t *testing.T) {
	terrain := dungeonStudyTerrain(29, true)
	for _, rate := range []int{20, 60} {
		start := [3]float64{6.5, 1, 8.5}
		blocked := dungeonStudyWalk(terrain, start, rate, 0, 1.2, 1, 0)
		if blocked[0] >= 8 || blocked[0]-start[0] >= 1.5 {
			t.Fatal("pillar was crossed or incorrectly counted as a successful escape")
		}
		alternative := dungeonStudyWalk(terrain, start, rate, 0, 1.2, 0, -1)
		if start[2]-alternative[2]-PlayerWidth/2 < 2 {
			t.Fatal("central cross did not provide the alternative escape")
		}
		wall := dungeonStudyWalk(terrain, [3]float64{12.5, 1, 0}, rate, 1, 3, 1, 0)
		if playerBox(wall).max[0] > 14 || wall[0] < 13 {
			t.Fatalf("outer wall did not stop the body at its inside face: %v", wall)
		}
	}
}

func TestDungeonStudyGalleryClearsBodiesAndRejectsLowLintel(t *testing.T) {
	for _, kind := range []vnet.MobKind{vnet.MobKindVargrGuardian, vnet.MobKindDraugrKing} {
		bd := mobRegistry[kind].body
		for _, clearHeight := range []int64{2, 5} {
			gallery := scriptedTerrain{want: func(x, y, _ int64) bool {
				return x < -2 || x >= 3 || y <= 0 || y >= 1+clearHeight
			}}
			pos := [3]float64{0.5, 1, 0.5}
			wantBlocked := bd.height > float64(clearHeight)
			if overlaps(gallery, bd.boxAt(pos)) != wantBlocked {
				t.Fatalf("%v at height %d: unexpected envelope clearance", kind, clearHeight)
			}
			if wantBlocked {
				continue
			}
			end, blocked := moveAndCollideWithStep(gallery, bd, pos, [3]float64{0, 0, 9}, 0)
			if blocked[2] || math.Abs(end[2]-9.5) > collisionSkin {
				t.Fatalf("%v cannot traverse clear gallery: %v", kind, end)
			}
		}
	}
}
