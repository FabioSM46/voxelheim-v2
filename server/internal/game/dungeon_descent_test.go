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
	// A box wider than any turn of the dungeon, filtered by the cache's own envelope;
	// the count below proves the box held every chunk the envelope has.
	for x := int32(-8); x <= 8; x++ {
		for y := int32(-8); y <= 8; y++ {
			for z := int32(-8); z <= 8; z++ {
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
	if len(d.chunks) != world.InstanceChunkEnvelope(seed) {
		t.Fatalf("seed %d: loaded %d chunks of an envelope of %d", seed, len(d.chunks), world.InstanceChunkEnvelope(seed))
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

// The drawing's arrival slot and guardian centre, which fix the frame below.
const (
	descentArrivalX, descentArrivalZ = 17, 3
	descentGuardianZ                 = 87
)

// descentFrame maps a cell of the unrotated drawing's floor plan to the centre of the
// world cell the seed's placement put it in. It is derived from two anchors rather than
// from world's rotation: the arrival slot and the guardian share the drawing's x, so
// their difference is the drawing's +Z in the world, and +X is that turned a quarter
// clockwise. A difference that is not exactly one axis of that length fails the test,
// so a moved anchor can never collapse the frame into a route that goes nowhere.
func descentFrame(t *testing.T, seed int64) func(lx, lz int) [3]float64 {
	t.Helper()
	arrival, _ := world.InstanceAnchors(seed)
	guardian, _, _ := world.InstanceEncounterAnchors(seed)
	const span = descentGuardianZ - descentArrivalZ
	dx, dz := guardian.X-arrival.X, guardian.Z-arrival.Z
	if dx%span != 0 || dz%span != 0 || max(dx, -dx)+max(dz, -dz) != span {
		t.Fatalf("seed %d: arrival %+v and guardian %+v are not %d apart on one axis", seed, arrival, guardian, span)
	}
	ux, uz := dx/span, dz/span
	vx, vz := uz, -ux
	return func(lx, lz int) [3]float64 {
		dx, dz := int64(lx-descentArrivalX), int64(lz-descentArrivalZ)
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
		at := descentFrame(t, seed)
		for _, body := range []struct {
			mount vnet.MountKind
			lanes []int
		}{{vnet.MountKindUnknown, []int{16, 17, 18}}, {vnet.MountKindBlackHorse, []int{17}}} {
			for _, lane := range body.lanes {
				t.Run(fmt.Sprintf("seed%d/mount%d/lane%d", seed, body.mount, lane), func(t *testing.T) {
					route := [][3]float64{at(descentArrivalX, descentArrivalZ), at(12, 3), at(12, 11), at(lane, 11), at(lane, 92)}
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

// A swimmer under the chasm reaches the shore by swimming and steps out onto it.
// From there the walking body takes the whole route below at every rotation, with
// the puzzles' doors open: north through the cave and its web curtain, west along the
// gallery through the grille, down the stair into the sand hall, across it and
// through its door, and down the second stair into the king's arena.
func TestThePoolIsLeftByTheShoreAndTheRouteDownReachesTheKing(t *testing.T) {
	for seed := int64(0); seed < 4; seed++ {
		terrain := newDescentTerrain(t, seed)
		_, king, gate := world.InstanceEncounterAnchors(seed)
		checkpoints := map[int]world.PlacedAnchor{}
		for _, a := range world.InstanceDungeonAnchors(seed) {
			if a.Kind == world.AnchorInstanceCheckpoint {
				checkpoints[a.Index] = a
			}
		}
		shoreSlot := checkpoints[0]
		p := &Player{sim: &Sim{idleLimit: 1 << 30}, lifeState: vnet.LifeStateAlive, health: 100, hunger: 100,
			pos: [3]float64{float64(gate.X) + .5, float64(shoreSlot.Y) - 2, float64(gate.Z) + .5}}
		shore := [3]float64{float64(shoreSlot.X) + .5, float64(shoreSlot.Y), float64(shoreSlot.Z) + .5}
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

		// Waypoints in the drawing's floor plan, each on the level it is walked at.
		at := descentFrame(t, seed)
		on := func(lx, lz int, level int64) [3]float64 {
			w := at(lx, lz)
			w[1] = float64(level)
			return w
		}
		cave, sand, bottom := shoreSlot.Y, checkpoints[2].Y, king.Y
		route := [][3]float64{p.pos,
			on(17, 90, cave), on(17, 53, cave), on(5, 53, cave), on(5, 47, cave), // cave, curtain, grille
			on(5, 37, sand), on(5, 36, sand), on(29, 36, sand), on(29, 40, sand), // the stair down, the hall, its door
			on(29, 50, bottom), on(29, 53, bottom), on(17, 53, bottom), on(17, 67, bottom), // the stair down, the arena
		}
		p = walkDescent(t, terrain, vnet.MountKindUnknown, route)
		if centre := [3]float64{float64(king.X) + .5, float64(king.Y), float64(king.Z) + .5}; math.Abs(p.pos[0]-centre[0]) > .02 || math.Abs(p.pos[2]-centre[2]) > .02 || math.Abs(p.pos[1]-centre[1]) > .02 {
			t.Fatalf("seed %d: the route ends at %v, not the king's centre %v", seed, p.pos, centre)
		}
	}
}
