package game

import (
	"cmp"
	"math"
	"slices"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The capital's horses are residents, not mobs. Keeping them outside Sim.mobs is the
// whole safety boundary: combat, projectiles, the director and corpse creation have no
// collection through which to address one. They share only MobState's presentation
// stream, just as settlement residents do.
const (
	paddockHorseSeedOffset int64 = 0x34D29E61

	// One oval the three share, not a circle each. A horse is over two blocks long and
	// the circle of radius 1.25 it used to trace was narrower than itself, so it turned on
	// the spot. The semi-axes are sized against the paddock's 17 × 9 interior
	// (schematic_stable.go): the oval is centred on the middle cell, so the wall stands
	// 8.5 blocks away along the long axis and 4.5 across it, and
	// TestPaddockLapKeepsNoseAndTailInsideTheWall walks a whole lap of the drawing to pin
	// that a 2.3-block horse keeps half a block from it everywhere.
	paddockRouteLongAxis  float64 = 5.5
	paddockRouteShortAxis float64 = 2.5

	// The perimeter is about 26 blocks, so a lap of 15 s is a walk of about 1.7 blocks a
	// second on average. The parameter is uniform in angle rather than in arc, so a horse
	// is slowest through the tight bends at the ends of the long axis and fastest along
	// the straights, which is how a walk round an oval reads.
	paddockHorseLapSeconds float64 = 15

	// The two low bits are an opaque presentation seed, not a gameplay field. The server
	// never reads them after minting the id; the client uses them only to choose one of
	// the three already-contracted coat materials. Sorting the three anchors before this
	// call guarantees the stable carries seeds 0, 1 and 2 exactly once.
	paddockHorseVariantMask uint64 = 0b11
)

// paddockHorseVariants is how many horses a paddock holds and how many coats there are:
// one of each. Untyped on purpose — it is an array length, a bound on a uint8 variant
// and the divisor of a lap, and one declaration serves all three.
const paddockHorseVariants = 3

// paddockRoute is the oval the three horses of one paddock share: the cell the middle
// anchor stands in, and the unit direction of the long axis.
//
// Both come from the three paddock anchors sorted in world coordinates and from nothing
// else. The drawing puts the three on one line, five blocks apart, across the middle of
// the paddock (schematic_stable.go), so whichever way the stable was turned the middle
// anchor is the paddock's centre and first-to-third is its long axis. The materialiser
// sorts the trio in world coordinates precisely so that the rotation does not matter,
// and no building geometry — no Facing, no origin — is read here.
type paddockRoute struct {
	anchor [3]int64   // the middle anchor: the oval is centred on this cell, at its height
	axis   [2]float64 // unit XZ direction from the first anchor to the third
}

// paddockAnchorOrder is "sorted in world coordinates": west to east, then north to
// south. It is the one definition, because two things are read off the order — the
// materialiser assigns coat variants in it and [paddockRouteOf] takes the trio's middle
// and ends in it — and they must agree with each other and with the tests.
func paddockAnchorOrder(a, b world.PlacedAnchor) int {
	return cmp.Or(cmp.Compare(a.X, b.X), cmp.Compare(a.Z, b.Z))
}

// paddockRouteOf derives the oval from the three paddock anchors, in any order.
//
// The sort fixes the sign of the axis and the oval does not care: a trio that reads
// first-to-third as +Z rather than −Z traces the same curve in the same sense, half a lap
// out of phase, because turning the axis turns its normal with it. A trio that spans
// nothing has no axis at all; +X keeps the pose finite rather than NaN, which is the only
// property that matters for a shape no drawing has.
func paddockRouteOf(trio [paddockHorseVariants]world.PlacedAnchor) paddockRoute {
	slices.SortFunc(trio[:], paddockAnchorOrder)
	first, middle, last := trio[0], trio[1], trio[2]
	dx, dz := float64(last.X-first.X), float64(last.Z-first.Z)
	axis := [2]float64{1, 0}
	if span := math.Hypot(dx, dz); span > 0 {
		axis = [2]float64{dx / span, dz / span}
	}
	return paddockRoute{anchor: [3]int64{middle.X, middle.Y, middle.Z}, axis: axis}
}

type paddockHorse struct {
	entityID uint64
	route    paddockRoute
	variant  uint8
	pos      [3]float64
	yaw      float64
	chunk    world.Coord
}

func paddockHorseID(seed, x, z int64, variant uint8) uint64 {
	hashed := world.HashLattice(seed+paddockHorseSeedOffset, x, z) | residentBit
	return hashed&^paddockHorseVariantMask | uint64(variant)
}

// paddockHorsePose is the entire route: where on the oval a horse stands at a tick, and
// the heading of its travel. It is pure in the world seed, the route, the variant, the
// persisted world tick and the fixed timestep; it has no steering, collision query,
// random state or history to diverge across servers.
//
// The seeded phase is hashed from the middle anchor's column, so the three horses share
// it and the variant alone spaces them: a third of a lap apart, which keeps any two at
// least √3 × the short semi-axis — about 4.3 blocks — from each other everywhere on the
// oval.
func paddockHorsePose(seed int64, route paddockRoute, variant uint8, tick uint64, dt float64) ([3]float64, float64) {
	hashed := world.HashLattice(seed+paddockHorseSeedOffset+1, route.anchor[0], route.anchor[2])
	phase := float64(hashed>>11) * (2 * math.Pi / (1 << 53))
	spacing := 2 * math.Pi * float64(variant) / paddockHorseVariants
	angle := math.Mod(phase+spacing+2*math.Pi*float64(tick)*dt/paddockHorseLapSeconds, 2*math.Pi)

	// The oval in its own frame: along the long axis and across it, where "across" is the
	// axis turned a quarter turn, so that the lap runs the same way round as the circle it
	// replaces did.
	sinA, cosA := math.Sincos(angle)
	along, across := paddockRouteLongAxis*cosA, paddockRouteShortAxis*sinA
	axisX, axisZ := route.axis[0], route.axis[1]
	acrossX, acrossZ := axisZ, -axisX
	pos := [3]float64{
		float64(route.anchor[0]) + 0.5 + along*axisX + across*acrossX,
		float64(route.anchor[1]),
		float64(route.anchor[2]) + 0.5 + along*axisZ + across*acrossZ,
	}

	// The heading is the tangent of travel — the position above differentiated in the
	// angle — read in the movement basis, where yaw 0 faces -Z and forward is
	// (-sin yaw, -cos yaw).
	velocityX := -paddockRouteLongAxis*sinA*axisX + paddockRouteShortAxis*cosA*acrossX
	velocityZ := -paddockRouteLongAxis*sinA*axisZ + paddockRouteShortAxis*cosA*acrossZ
	yaw := wrapAngle(math.Atan2(-velocityX, -velocityZ))
	return pos, yaw
}

// materialisePaddockHorseLocked creates the horse whose anchor lies in coord. The caller
// has already sorted all three paddock anchors, which is what assigns the stable-wide
// colour seed and derives the one route the three share. The caller holds Sim.mu.
func (s *Sim) materialisePaddockHorseLocked(coord world.Coord, slot world.PlacedAnchor, route paddockRoute, variant uint8) {
	if variant >= paddockHorseVariants || world.ChunkOf(slot.X, slot.Y, slot.Z) != coord {
		return
	}
	if slot.X < -worldLimit || slot.X >= worldLimit || slot.Z < -worldLimit || slot.Z >= worldLimit {
		return
	}

	id := paddockHorseID(s.worldSeed, slot.X, slot.Z, variant)
	if _, standing := s.paddockHorses[id]; standing {
		return
	}
	pos, yaw := paddockHorsePose(s.worldSeed, route, variant, s.worldTick, s.dt)
	s.paddockHorses[id] = &paddockHorse{
		entityID: id,
		route:    route,
		variant:  variant,
		pos:      pos,
		yaw:      yaw,
		chunk:    world.ChunkOf(int64(math.Floor(pos[0])), int64(math.Floor(pos[1])), int64(math.Floor(pos[2]))),
	}
	s.log.Debug("paddock horse materialised", "entity_id", id, "pos", pos)
}

// advancePaddockHorsesLocked evaluates the route at this tick. There is no integration:
// missing a tick or materialising late produces the same position as a server that has
// held the horse since startup.
func (s *Sim) advancePaddockHorsesLocked(worldTick uint64) {
	for _, horse := range s.paddockHorses {
		horse.pos, horse.yaw = paddockHorsePose(s.worldSeed, horse.route, horse.variant, worldTick, s.dt)
		horse.chunk = world.ChunkOf(
			int64(math.Floor(horse.pos[0])),
			int64(math.Floor(horse.pos[1])),
			int64(math.Floor(horse.pos[2])),
		)
	}
}

func (horse *paddockHorse) state() protocol.MobState {
	return protocol.MobState{
		EntityID:       horse.entityID,
		Kind:           vnet.MobKindHorse,
		Pos:            toWire(horse.pos),
		Yaw:            float32(horse.yaw),
		Health:         residentHealth,
		MaxHealth:      residentHealth,
		Action:         vnet.MobActionIdle,
		TargetEntityID: 0,
	}
}
