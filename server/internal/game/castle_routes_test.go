package game

import (
	"fmt"
	"math"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// castleTerrain presents the actual authored cells to the production collider.
// Independent inverse transforms complement world's visitSchematic rotation tests.
// The offset deliberately puts flights across both horizontal chunk boundaries.
type castleTerrain struct{ turn int }

func (c castleTerrain) Block(x, y, z int64) (world.Block, bool) {
	x -= 11
	y -= 5
	z -= 13
	if y < 0 {
		return world.Stone, true
	}
	s := world.SchematicFor(world.BuildingKeep)
	if x < 0 || z < 0 || x >= int64(s.W) || z >= int64(s.D) || y >= int64(s.H) {
		return world.Air, true
	}
	switch c.turn {
	case 1:
		x, z = z, int64(s.D)-1-x
	case 2:
		x, z = int64(s.W)-1-x, int64(s.D)-1-z
	case 3:
		x, z = int64(s.W)-1-z, x
	}
	b := s.At(int(x), int(y), int(z))
	if b == ^world.Block(0) { // The schematic terrain-preservation sentinel is not material.
		b = world.Air
	}
	for _, first := range []world.Block{world.SlateStairNorthBottom, world.SlateStairNorthTop} {
		if b >= first && b < first+4 {
			b = first + (b-first+world.Block(c.turn))%4
			break
		}
	}
	return b, true
}
func (c castleTerrain) Solid(x, y, z int64) bool { b, _ := c.Block(x, y, z); return world.Solid(b) }
func (c castleTerrain) Fluid(x, y, z int64) bool { return fluidByBlock(c, x, y, z) }
func (c castleTerrain) point(p [3]float64) [3]float64 {
	x, z := p[0], p[2]
	switch c.turn {
	case 1:
		x, z = 63-z, x
	case 2:
		x, z = 63-x, 63-z
	case 3:
		x, z = z, 63-x
	}
	return [3]float64{x + 11, p[1] + 5, z + 13}
}

// walkCastle sends ordinary directional intent through Player.step at the real
// server tick rate. Gravity, ground contact and the shape sweep are production code.
// Waypoints only steer: neither feet height nor vertical velocity is corrected.
func walkCastle(t *testing.T, c castleTerrain, route [][3]float64) {
	t.Helper()
	player := &Player{sim: &Sim{idleLimit: 10000}, pos: c.point(route[0]),
		lifeState: vnet.LifeStateAlive, health: 100, hunger: 100}
	dt := 1 / float64(DefaultTickRate)
	for n, local := range route[1:] {
		target := c.point(local)
		for tick := 0; tick < 3000; tick++ {
			dx, dz := target[0]-player.pos[0], target[2]-player.pos[2]
			distance := math.Hypot(dx, dz)
			if distance < 0.015 && math.Abs(player.pos[1]-target[1]) < 0.02 {
				break
			}
			if tick == 2999 {
				t.Fatalf("waypoint %d %v: player blocked or on wrong floor at %v", n+1, local, player.pos)
			}
			player.current = intent{}
			player.idleTicks = 0
			if distance >= 0.015 {
				scale := math.Min(distance/(WalkSpeed*dt), 1) / distance
				player.current.moveX, player.current.moveZ = dx*scale, -dz*scale
			}
			before := player.pos
			player.step(dt, c)
			if overlaps(c, player.box()) {
				t.Fatalf("body overlaps actual castle at %v", player.pos)
			}
			if player.pos[1]-before[1] > 0.501 {
				t.Fatalf("upward correction beyond walking step %v -> %v", before, player.pos)
			}
			if player.health != 100 {
				t.Fatalf("ordinary staircase walking inflicted damage at %v", player.pos)
			}
		}
	}
}

func TestCastleMainFloorsAreWalkedWithoutJumpingInEveryRotation(t *testing.T) {
	for turn := range 4 {
		t.Run(fmt.Sprint(turn), func(t *testing.T) {
			c := castleTerrain{turn: turn}
			for _, west := range []bool{true, false} {
				side, near, far := 14.5, 7.5, 11.5
				top := 21
				if !west {
					side, near, far, top = 49.5, 52.5, 55.5, 28
				}
				door := side
				if !west {
					door = 46.5
				}
				route := [][3]float64{{31.5, 0, 62.5}, {31.5, 0, 43.5}, {door, 0, 43.5}, {door, 0, 37.5}, {side, 0, 37.5}, {near, 0, 37.5}}
				for floor := 0; floor < top; floor += 7 {
					x, z := near, 29.5
					if (floor/7)%2 == 1 {
						x, z = far, 37.5
					}
					route = append(route, [3]float64{x, float64(floor + 7), z})
					y := float64(floor + 7)
					roomX := 16.5
					if !west {
						roomX = 46.5
					}
					route = append(route, [3]float64{side, y, z}, [3]float64{side, y, 24.5}, [3]float64{roomX, y, 24.5}, [3]float64{roomX, y, 22.5}, [3]float64{roomX, y, 24.5}, [3]float64{side, y, 24.5}, [3]float64{side, y, z}, [3]float64{x, y, z})
					if floor+7 < top {
						nextX := far
						if (floor/7)%2 == 1 {
							nextX = near
						}
						route = append(route, [3]float64{nextX, float64(floor + 7), z})
					}
				}
				// Return by the same physical flights; descending gets no waypoint teleport.
				for i := len(route) - 2; i >= 0; i-- {
					route = append(route, route[i])
				}
				walkCastle(t, c, route)
				for i := range route {
					if route[i][0] == near || route[i][0] == far {
						route[i][0]++
					}
				}
				walkCastle(t, c, route)
			}
		})
	}
}

func TestCastleCourtStairAndCurtainCircuitNeedNoJump(t *testing.T) {
	route := [][3]float64{{31.5, 0, 62.5}, {31.5, 0, 46.5}, {4.5, 0, 46.5}, {4.5, 0, 44.5}, {4.5, 13, 30.5}, {1.5, 13, 30.5}, {1.5, 13, 4.5}, {4.5, 13, 4.5}, {4.5, 13, 1.5}, {58.5, 13, 1.5}, {58.5, 13, 4.5}, {61.5, 13, 4.5}, {61.5, 13, 58.5}, {58.5, 13, 58.5}, {58.5, 13, 61.5}, {4.5, 13, 61.5}, {4.5, 13, 58.5}, {1.5, 13, 58.5}, {1.5, 13, 30.5}, {4.5, 13, 30.5}, {4.5, 0, 44.5}, {4.5, 0, 46.5}, {31.5, 0, 46.5}, {31.5, 0, 62.5}}
	for lane := range 2 {
		for turn := range 4 {
			t.Run(fmt.Sprintf("lane%d/rotation%d", lane, turn), func(t *testing.T) {
				walkCastle(t, castleTerrain{turn: turn}, route)
			})
		}
		// Walk the other tread and curtain lanes as well, including every corner
		// transition. Keep the route independent of the schematic's air search.
		for i := range route {
			for axis := range 3 {
				if axis == 1 {
					continue
				}
				switch route[i][axis] {
				case 1.5:
					route[i][axis] = 2.5
				case 61.5:
					route[i][axis] = 60.5
				case 4.5:
					route[i][axis] = 5.5
				}
			}
		}
	}
}

func TestCastleWesternSpireLookoutsAreReachedByWalking(t *testing.T) {
	for _, tower := range []struct{ cx, cz, lookout float64 }{{10, 12, 35}, {20, 32, 29}} {
		route := [][3]float64{{31.5, 0, 62.5}, {31.5, 0, 43.5}, {14.5, 0, 43.5}, {14.5, 0, 37.5}, {7.5, 0, 37.5}, {7.5, 7, 29.5}, {11.5, 7, 29.5}, {11.5, 14, 37.5}, {7.5, 14, 37.5}, {7.5, 21, 29.5}, {14.5, 21, 29.5}}
		if tower.cx == 10 {
			route = append(route, [3]float64{14.5, 21, 24.5}, [3]float64{16.5, 21, 24.5}, [3]float64{16.5, 21, 19.5}, [3]float64{17.5, 21, 19.5}, [3]float64{17.5, 21, 9.5})
		}
		route = append(route, [3]float64{tower.cx - 2.5, 21, tower.cz - 2.5})
		for i, floor := 0, 21.0; floor < tower.lookout; i, floor = i+1, floor+3 {
			height := math.Min(3, tower.lookout-floor)
			x, z := tower.cx-2.5, tower.cz+2.5
			if i%2 == 1 {
				x, z = tower.cx+2.5, tower.cz-2.5
			}
			if height < 3 {
				z = tower.cz - 0.5 + height
			}
			route = append(route, [3]float64{x, floor + height, z})
			if floor+height < tower.lookout {
				nextX := tower.cx + 2.5
				if i%2 == 1 {
					nextX = tower.cx - 2.5
				}
				route = append(route, [3]float64{nextX, floor + height, z})
			}
		}
		route = append(route, [3]float64{tower.cx - 2.5, tower.lookout, tower.cz + 2.5}, [3]float64{tower.cx + 1.5, tower.lookout, tower.cz + 2.5})
		for i := len(route) - 2; i >= 0; i-- {
			route = append(route, route[i])
		}
		for lane := range 2 {
			walk := append([][3]float64(nil), route...)
			for i := range walk {
				if walk[i][0] == tower.cx-2.5 || walk[i][0] == tower.cx+2.5 {
					walk[i][0] += float64(lane)
				}
			}
			for turn := range 4 {
				t.Run(fmt.Sprintf("tower%.0f/lane%d/rotation%d", tower.cx, lane, turn), func(t *testing.T) { walkCastle(t, castleTerrain{turn: turn}, walk) })
			}
		}
	}
}

func TestCastleEasternSpireLookoutsAreReachedByWalking(t *testing.T) {
	for _, tower := range []struct{ cx, cz, lookout float64 }{{50, 12, 41}, {42, 32, 35}} {
		route := [][3]float64{{31.5, 0, 62.5}, {31.5, 0, 43.5}, {46.5, 0, 43.5}, {46.5, 0, 37.5}, {49.5, 0, 37.5}, {52.5, 0, 37.5}, {52.5, 7, 29.5}, {55.5, 7, 29.5}, {55.5, 14, 37.5}, {52.5, 14, 37.5}, {52.5, 21, 29.5}, {55.5, 21, 29.5}, {55.5, 28, 37.5}, {49.5, 28, 37.5}, {49.5, 28, 24.5}}
		if tower.cx == 50 {
			route = append(route, [3]float64{46.5, 28, 24.5}, [3]float64{46.5, 28, 19.5}, [3]float64{37.5, 28, 19.5}, [3]float64{37.5, 28, 11.5}, [3]float64{45.5, 28, 11.5}, [3]float64{45.5, 28, 9.5})
		} else {
			route = append(route, [3]float64{49.5, 28, 29.5})
		}
		route = append(route, [3]float64{tower.cx - 2.5, 28, tower.cz - 2.5})
		for i, floor := 0, 28.0; floor < tower.lookout; i, floor = i+1, floor+3 {
			height := math.Min(3, tower.lookout-floor)
			x, z := tower.cx-2.5, tower.cz+2.5
			if i%2 == 1 {
				x, z = tower.cx+2.5, tower.cz-2.5
			}
			if height < 3 {
				z = tower.cz - 0.5 + height
			}
			route = append(route, [3]float64{x, floor + height, z})
			if floor+height < tower.lookout {
				nextX := tower.cx + 2.5
				if i%2 == 1 {
					nextX = tower.cx - 2.5
				}
				route = append(route, [3]float64{nextX, floor + height, z})
			}
		}
		route = append(route, [3]float64{tower.cx - 2.5, tower.lookout, tower.cz + 2.5}, [3]float64{tower.cx + 1.5, tower.lookout, tower.cz + 2.5})
		for i := len(route) - 2; i >= 0; i-- {
			route = append(route, route[i])
		}
		for lane := range 2 {
			walk := append([][3]float64(nil), route...)
			for i := range walk {
				if walk[i][0] == tower.cx-2.5 || walk[i][0] == tower.cx+2.5 {
					walk[i][0] += float64(lane)
				}
			}
			for turn := range 4 {
				t.Run(fmt.Sprintf("tower%.0f/lane%d/rotation%d", tower.cx, lane, turn), func(t *testing.T) { walkCastle(t, castleTerrain{turn: turn}, walk) })
			}
		}
	}
}

func TestEastLookoutGuardsPreventWalkingIntoReturnFlight(t *testing.T) {
	for _, tower := range []struct{ x, z, y float64 }{{50, 12, 41}, {42, 32, 35}} {
		for _, approach := range [][2][3]float64{
			{{tower.x + 1.5, tower.y, tower.z + 0.5}, {tower.x + 3.5, tower.y, tower.z + 0.5}},
			{{tower.x + 2.5, tower.y, tower.z + 2.5}, {tower.x + 2.5, tower.y, tower.z + 0.5}},
		} {
			for turn := range 4 {
				c := castleTerrain{turn: turn}
				start, target := c.point(approach[0]), c.point(approach[1])
				p := &Player{sim: &Sim{idleLimit: 10000}, pos: start, lifeState: vnet.LifeStateAlive, health: 100, hunger: 100}
				for range 100 {
					dx, dz := target[0]-p.pos[0], target[2]-p.pos[2]
					length := math.Hypot(dx, dz)
					p.current = intent{moveX: dx / length, moveZ: -dz / length}
					p.idleTicks = 0
					p.step(1/float64(DefaultTickRate), c)
					if math.Abs(p.pos[1]-start[1]) > 0.02 || p.health != 100 || overlaps(c, p.box()) {
						t.Fatalf("tower guard did not keep standing player safe: %v -> %v", start, p.pos)
					}
				}
				if math.Hypot(target[0]-p.pos[0], target[2]-p.pos[2]) < 1 {
					t.Fatal("player crossed the guarded opening")
				}
			}
		}
	}
}

func TestCastleAudienceDaisIsReachedWithoutJumping(t *testing.T) {
	for _, z := range []float64{21.5, 22.5} {
		route := [][3]float64{{31.5, 0, 62.5}, {31.5, 0, 43.5}, {46.5, 0, 43.5}, {46.5, 0, 37.5}, {49.5, 0, 37.5}, {52.5, 0, 37.5}, {52.5, 7, 29.5}, {49.5, 7, 29.5}, {49.5, 7, 24.5}, {46.5, 7, 24.5}, {46.5, 7, z}, {43.5, 7, z}, {41.5, 8, z}}
		for i := len(route) - 2; i >= 0; i-- {
			route = append(route, route[i])
		}
		for turn := range 4 {
			t.Run(fmt.Sprintf("lane%.1f/rotation%d", z, turn), func(t *testing.T) { walkCastle(t, castleTerrain{turn: turn}, route) })
		}
	}
}
