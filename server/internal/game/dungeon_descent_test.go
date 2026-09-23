package game

import (
	"context"
	"fmt"
	"math"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The first dungeon's descent, walked and fallen by the production movement code.
//
// world's layout tests flood cells; these send ordinary intent through Player.step
// against the generated dungeon of each rotation — gravity, the swept box, step-up
// and the swim rules all production — which is what a cell flood cannot say about a
// mounted body three blocks tall or a fall thirty-three blocks deep.

// descentTerrain is one seed's generated dungeon with the guardian's trapdoor open,
// every chunk resident. doorOpen reads puzzle 1's door cells as air, the state of a
// party that solved the rune hall; dry reads the pool as stone, the control that
// shows the water is what makes the landing harmless.
type descentTerrain struct {
	chunks   map[world.Coord]*world.Chunk
	doors    map[[3]int64]bool
	doorOpen bool
	dry      bool
}

func newDescentTerrain(t *testing.T, seed int64) *descentTerrain {
	t.Helper()
	cache, _ := world.NewGatedInstanceCache(seed, 1, world.InstanceChunkEnvelope(seed), true)
	d := &descentTerrain{chunks: map[world.Coord]*world.Chunk{}, doors: map[[3]int64]bool{}, doorOpen: true}
	for x := int32(-3); x <= 2; x++ {
		for y := int32(-3); y <= 1; y++ {
			for z := int32(-3); z <= 2; z++ {
				coord := world.Coord{X: x, Y: y, Z: z}
				if !cache.Contains(coord) {
					continue
				}
				chunk, _, err := cache.Get(context.Background(), coord)
				if err != nil {
					t.Fatal(err)
				}
				d.chunks[coord] = chunk
			}
		}
	}
	for _, a := range world.InstanceDungeonAnchors(seed) {
		if a.Kind == world.AnchorInstanceDoor {
			d.doors[[3]int64{a.X, a.Y, a.Z}] = true
		}
	}
	return d
}

func (d *descentTerrain) Block(x, y, z int64) (world.Block, bool) {
	if d.doorOpen && d.doors[[3]int64{x, y, z}] {
		return world.Air, true
	}
	chunk := d.chunks[world.ChunkOf(x, y, z)]
	if chunk == nil {
		return world.Air, true // the void around the dungeon
	}
	b := chunk.At(world.Local(x), world.Local(y), world.Local(z))
	if d.dry && world.IsWater(b) {
		return world.Stone, true
	}
	return b, true
}

func (d *descentTerrain) Solid(x, y, z int64) bool { b, _ := d.Block(x, y, z); return world.Solid(b) }
func (d *descentTerrain) Fluid(x, y, z int64) bool { return fluidByBlock(d, x, y, z) }

// descentFrame maps a cell of the unrotated drawing's floor plan to the centre of the
// world cell the seed's placement put it in. It is derived from two anchors rather than
// from world's rotation: the arrival slot stands at (17, 3) in the drawing and the
// guardian at (17, 87), so their difference is the drawing's +Z in the world, and +X is
// that turned a quarter clockwise.
func descentFrame(seed int64) func(lx, lz int) [3]float64 {
	arrival, _ := world.InstanceAnchors(seed)
	guardian, _, _ := world.InstanceEncounterAnchors(seed)
	ux, uz := (guardian.X-arrival.X)/84, (guardian.Z-arrival.Z)/84
	vx, vz := uz, -ux
	return func(lx, lz int) [3]float64 {
		dx, dz := int64(lx-17), int64(lz-3)
		return [3]float64{
			float64(arrival.X+ux*dz+vx*dx) + .5,
			float64(arrival.Y),
			float64(arrival.Z+uz*dz+vz*dx) + .5,
		}
	}
}

// walkDescent steers one body through waypoints with ordinary directional intent at
// DefaultTickRate. Waypoints only steer: nothing corrects the feet or the velocity.
// Every tick the body must be clear of the terrain, unhurt, and never raised by more
// than its own step height.
func walkDescent(t *testing.T, terrain Terrain, mount vnet.MountKind, route [][3]float64) *Player {
	t.Helper()
	p := &Player{sim: &Sim{idleLimit: 1 << 30}, pos: route[0], lifeState: vnet.LifeStateAlive,
		health: 100, hunger: 100, mounted: mount}
	speed, step := WalkSpeed, playerStepHeight
	if mount != vnet.MountKindUnknown {
		speed, step = MountSpeed, mountedStepHeight
	}
	dt := 1 / float64(DefaultTickRate)
	for n, target := range route[1:] {
		for tick := 0; ; tick++ {
			dx, dz := target[0]-p.pos[0], target[2]-p.pos[2]
			distance := math.Hypot(dx, dz)
			if distance < 0.015 && math.Abs(p.pos[1]-target[1]) < 0.02 {
				break
			}
			if tick == 3000 {
				t.Fatalf("waypoint %d %v: body stopped at %v", n+1, target, p.pos)
			}
			p.current = intent{}
			p.idleTicks = 0
			if distance >= 0.015 {
				scale := math.Min(distance/(speed*dt), 1) / distance
				p.current.moveX, p.current.moveZ = dx*scale, -dz*scale
			}
			before := p.pos
			p.step(dt, terrain)
			if overlaps(terrain, p.box()) {
				t.Fatalf("body overlaps the dungeon at %v", p.pos)
			}
			if p.pos[1]-before[1] > step+1e-6 {
				t.Fatalf("raised %v -> %v, beyond the step height", before, p.pos)
			}
			if p.health != 100 {
				t.Fatalf("walking floor 1 cost health at %v", p.pos)
			}
		}
	}
	return p
}

// Floor 1, from the arrival slot to the rim of the open chasm, for the walking and the
// mounted body at every rotation: round the portal frame, down the court's corridor,
// through both halls and the rune hall, through puzzle 1's door and across the
// guardian's arena. The walking body also takes both outer lanes of every corridor.
func TestFloorOneIsWalkedAndRiddenFromArrivalToTheChasm(t *testing.T) {
	for seed := int64(0); seed < 4; seed++ {
		terrain := newDescentTerrain(t, seed)
		at := descentFrame(seed)
		for _, body := range []struct {
			mount vnet.MountKind
			lanes []int
		}{{vnet.MountKindUnknown, []int{16, 17, 18}}, {vnet.MountKindBlackHorse, []int{17}}} {
			for _, lane := range body.lanes {
				t.Run(fmt.Sprintf("seed%d/mount%d/lane%d", seed, body.mount, lane), func(t *testing.T) {
					route := [][3]float64{at(17, 3), at(12, 3), at(12, 11), at(lane, 11), at(lane, 92)}
					p := walkDescent(t, terrain, body.mount, route)
					if p.pos[1] != route[0][1] {
						t.Fatalf("the rim is not on floor 1: %v", p.pos)
					}
				})
			}
		}
	}
}

// The drop from the open trapdoor into the pool costs nothing at every tick rate the
// server accepts, for either body, because the landing is in water four deep. The
// same drop with the pool read as stone does hurt, which is what makes the first half
// a statement about the water.
func TestTheChasmDropIntoThePoolIsHarmlessAtEveryTickRate(t *testing.T) {
	for seed := int64(0); seed < 4; seed++ {
		terrain := newDescentTerrain(t, seed)
		_, _, gate := world.InstanceEncounterAnchors(seed)
		top := [3]float64{float64(gate.X) + .5, float64(gate.Y) + 1, float64(gate.Z) + .5}
		fall := func(rate int, mount vnet.MountKind) *Player {
			p := &Player{sim: &Sim{idleLimit: 1 << 30}, pos: top, lifeState: vnet.LifeStateAlive,
				health: 100, hunger: 100, mounted: mount}
			for range 6 * rate {
				p.idleTicks = 0
				p.step(1/float64(rate), terrain)
			}
			return p
		}
		for rate := 1; rate <= 255; rate++ {
			for _, mount := range []vnet.MountKind{vnet.MountKindUnknown, vnet.MountKindBlackHorse} {
				p := fall(rate, mount)
				if p.health != 100 {
					t.Fatalf("seed %d at %d Hz (mount %d): the drop cost %d health", seed, rate, mount, 100-p.health)
				}
				if p.pos[1] > top[1]-30 || !overlapsFluid(terrain, p.box()) {
					t.Fatalf("seed %d at %d Hz (mount %d): the body is at %v, not in the pool", seed, rate, mount, p.pos)
				}
			}
		}
		terrain.dry = true
		if p := fall(DefaultTickRate, vnet.MountKindUnknown); p.health == 100 {
			t.Fatalf("seed %d: the same drop onto stone cost nothing, so the pool proves nothing", seed)
		}
		terrain.dry = false
	}
}

// A swimmer under the chasm reaches the shore by swimming, steps out onto it and walks
// on to the shore checkpoint and down the tunnel into the king's arena.
func TestThePoolIsLeftByTheShoreAndTheTunnel(t *testing.T) {
	for seed := int64(0); seed < 4; seed++ {
		terrain := newDescentTerrain(t, seed)
		_, king, gate := world.InstanceEncounterAnchors(seed)
		var checkpoint world.PlacedAnchor
		for _, a := range world.InstanceDungeonAnchors(seed) {
			if a.Kind == world.AnchorInstanceCheckpoint {
				checkpoint = a
			}
		}
		p := &Player{sim: &Sim{idleLimit: 1 << 30}, lifeState: vnet.LifeStateAlive, health: 100, hunger: 100,
			pos: [3]float64{float64(gate.X) + .5, float64(checkpoint.Y) - 2, float64(gate.Z) + .5}}
		shore := [3]float64{float64(checkpoint.X) + .5, float64(checkpoint.Y), float64(checkpoint.Z) + .5}
		dt := 1 / float64(DefaultTickRate)
		for tick := 0; ; tick++ {
			if !overlapsFluid(terrain, p.box()) && p.onGround && math.Abs(p.pos[1]-shore[1]) < 0.02 {
				break
			}
			if tick == 20*DefaultTickRate {
				t.Fatalf("seed %d: the swimmer never stood on the shore; at %v", seed, p.pos)
			}
			dx, dz := shore[0]-p.pos[0], shore[2]-p.pos[2]
			d := math.Hypot(dx, dz)
			p.current = intent{moveX: dx / d, moveZ: -dz / d, jump: true}
			p.idleTicks = 0
			p.step(dt, terrain)
		}
		// The shore's checkpoint and the king's centre share the drawing's axis, so the
		// tunnel is walked in a straight line.
		route := [][3]float64{p.pos, shore, {float64(king.X) + .5, float64(king.Y), float64(king.Z) + .5}}
		walkDescent(t, terrain, vnet.MountKindUnknown, route)
	}
}
