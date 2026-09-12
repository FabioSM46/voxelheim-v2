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
