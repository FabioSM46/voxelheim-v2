package game

import (
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func TestStandingBodyCannotPassThroughGrilleGaps(t *testing.T) {
	for _, block := range []world.Block{world.IronGrilleX, world.IronGrilleZ} {
		terrain := blockTerrain{blocks: map[[3]int64]world.Block{{0, 1, 0}: block, {0, 2, 0}: block}}
		depth := 2
		if block == world.IronGrilleZ {
			depth = 0
		}
		for _, sign := range []float64{-1, 1} {
			start := [3]float64{0.5, 1, 0.5}
			start[depth] += sign * 1.5
			var delta [3]float64
			delta[depth] = -sign * 3
			end, blocked := moveAndCollideWithStep(terrain, playerBody, start, delta, playerStepHeight)
			if !blocked[depth] || overlaps(terrain, playerBox(end)) {
				t.Fatalf("grille %d direction %g: end %v blocked %v", block, sign, end, blocked)
			}
			if (end[depth]-0.5)*sign <= 0 {
				t.Fatalf("body escaped barred window: %v", end)
			}
		}
	}
}

func TestSightFindsBarsAndPassesTheirGaps(t *testing.T) {
	voxel := [3]int64{0, 1, 0}
	for _, block := range []world.Block{world.IronGrilleX, world.IronGrilleZ} {
		terrain := blockTerrain{blocks: map[[3]int64]world.Block{voxel: block}}
		width, depth := 0, 2
		if block == world.IronGrilleZ {
			width, depth = 2, 0
		}
		for _, tc := range []struct {
			x       float64
			blocked bool
		}{{0.25, true}, {0.5, false}, {0.75, true}} {
			from, to := [3]float64{0.5, 1.5, 0.5}, [3]float64{0.5, 1.5, 0.5}
			from[width], to[width] = tc.x, tc.x
			from[depth], to[depth] = -1, 2
			if clearLineOfSight(terrain, from, to) == tc.blocked {
				t.Errorf("DDA block %d ray %g: wrong visibility", block, tc.x)
			}
			if got := voxelBlocksSight(terrain, voxel, from, to); got != tc.blocked {
				t.Errorf("block %d ray %g: blocked=%v", block, tc.x, got)
			}
		}
	}
	terrain := blockTerrain{blocks: map[[3]int64]world.Block{voxel: world.SlateSlabBottom}}
	if voxelBlocksSight(terrain, voxel, [3]float64{-1, 1.75, 0.5}, [3]float64{2, 1.75, 0.5}) {
		t.Error("empty slab half blocked sight")
	}
	if !voxelBlocksSight(terrain, voxel, [3]float64{-1, 1.25, 0.5}, [3]float64{2, 1.25, 0.5}) {
		t.Error("slab surface did not block sight")
	}
}

func TestProjectileBodyCannotTunnelThroughGrilleBars(t *testing.T) {
	for _, block := range []world.Block{world.IronGrilleX, world.IronGrilleZ} {
		terrain := blockTerrain{blocks: map[[3]int64]world.Block{{0, 1, 0}: block}}
		width, depth := 0, 2
		if block == world.IronGrilleZ {
			width, depth = 2, 0
		}
		for _, sign := range []float64{-1, 1} {
			for sample := 0; sample < 50; sample++ {
				from := [3]float64{0.5, 1.5, 0.5}
				from[width] = 0.25
				from[depth] = 0.5 - sign*(1+float64(sample)*0.005)
				delta := [3]float64{}
				delta[depth] = sign * 2
				end, blocked := moveAndCollide(terrain, projectileBody, from, delta)
				if !blocked[depth] || (end[depth]-0.5)*sign >= 0 {
					t.Fatalf("block %d direction %g phase %d: projectile crossed bar: %v blocked %v", block, sign, sample, end, blocked)
				}
			}
		}
	}
}

func BenchmarkOrdinaryGroundedBodySweep(b *testing.B) {
	terrain := blockTerrain{blocks: map[[3]int64]world.Block{}}
	for x := int64(-1); x <= 2; x++ {
		for z := int64(-1); z <= 2; z++ {
			terrain.blocks[[3]int64{x, 0, z}] = world.Stone
		}
	}
	for b.Loop() {
		moveAndCollide(terrain, playerBody, [3]float64{0.5, 1 + collisionSkin*2, 0.5}, [3]float64{0.16, -0.02, 0.02})
	}
}

func BenchmarkStairBodySweep(b *testing.B) {
	terrain := blockTerrain{blocks: map[[3]int64]world.Block{{1, 1, 0}: world.SlateStairEastBottom}}
	for b.Loop() {
		first, _ := moveAndCollideWithStep(terrain, playerBody, [3]float64{0.5, 1, 0.5}, [3]float64{0.6, 0, 0}, playerStepHeight)
		moveAndCollideWithStep(terrain, playerBody, first, [3]float64{0.6, 0, 0}, playerStepHeight)
	}
}

func TestAuthoritativeProjectilesMeetBarsAndPassGrilleGaps(t *testing.T) {
	for _, block := range []world.Block{world.IronGrilleX, world.IronGrilleZ} {
		width, depth := 0, 2
		if block == world.IronGrilleZ {
			width, depth = 2, 0
		}
		for _, sign := range []float64{-1, 1} {
			for _, x := range []float64{0.25, 0.5, 0.75} {
				for sample := 0; sample < 20; sample++ {
					for _, kind := range []vnet.ProjectileKind{vnet.ProjectileKindArrow, vnet.ProjectileKindEnergyOrb} {
						terrain := blockTerrain{blocks: map[[3]int64]world.Block{{0, 1, 0}: block, {0, 2, 0}: block, {0, 3, 0}: block}}
						h := newVitalsHarness(t, DefaultTickRate, terrain)
						start := [3]float32{0.5, 1, 0.5}
						start[width] = float32(x)
						start[depth] = float32(0.5 - sign*(2+float64(sample)*0.0125))
						owner, _ := h.join(1, start)
						direction := [3]float64{}
						direction[depth] = sign
						speed := ArrowSpeed
						if kind == vnet.ProjectileKindEnergyOrb {
							speed = OrbSpeed
						}
						id := spawnTestProjectile(t, h, kind, owner, direction, speed)
						for range 5 {
							advanceTestProjectiles(h)
						}
						proj, live := projectileState(h, id)
						if x == 0.5 {
							if !live || proj.stuck || (proj.pos[depth]-0.5)*sign <= 0 {
								t.Fatalf("gap blocked block%d sign%g phase%d kind%v: live%v %+v", block, sign, sample, kind, live, proj)
							}
						} else if kind == vnet.ProjectileKindArrow {
							if !live || !proj.stuck || (proj.pos[depth]-0.5)*sign >= 0 {
								t.Fatalf("arrow crossed bar block%d sign%g phase%d: live%v %+v", block, sign, sample, live, proj)
							}
						} else if live {
							t.Fatalf("orb survived bar block%d sign%g phase%d: %+v", block, sign, sample, proj)
						}
					}
				}
			}
		}
	}
}
