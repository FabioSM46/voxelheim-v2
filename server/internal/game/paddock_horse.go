package game

import (
	"math"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The capital's horses are residents, not mobs. Keeping them outside Sim.mobs is the
// whole safety boundary: combat, projectiles, the director and corpse creation have no
// collection through which to address one. They share only MobState's presentation
// stream, just as settlement residents do.
const (
	paddockHorseSeedOffset int64   = 0x34D29E61
	paddockHorseRadius     float64 = 1.25
	paddockHorseLapSeconds float64 = 12
	paddockHorseVariants           = 3

	// The two low bits are an opaque presentation seed, not a gameplay field. The server
	// never reads them after minting the id; the client uses them only to choose one of
	// the three already-contracted coat materials. Sorting the three anchors before this
	// call guarantees the stable carries seeds 0, 1 and 2 exactly once.
	paddockHorseVariantMask uint64 = 0b11
)

type paddockHorse struct {
	entityID uint64
	anchor   [3]int64
	pos      [3]float64
	yaw      float64
	chunk    world.Coord
}

func paddockHorseID(seed, x, z int64, variant uint8) uint64 {
	hashed := world.HashLattice(seed+paddockHorseSeedOffset, x, z) | residentBit
	return hashed&^paddockHorseVariantMask | uint64(variant)
}

// paddockHorsePose is the entire route: a small circle around the schematic anchor.
// It is pure in the world seed, anchor column, persisted world tick and fixed timestep;
// it has no steering, collision query, random state or history to diverge across servers.
func paddockHorsePose(seed int64, anchor [3]int64, tick uint64, dt float64) ([3]float64, float64) {
	hashed := world.HashLattice(seed+paddockHorseSeedOffset+1, anchor[0], anchor[2])
	phase := float64(hashed>>11) * (2 * math.Pi / (1 << 53))
	angle := math.Mod(phase+2*math.Pi*float64(tick)*dt/paddockHorseLapSeconds, 2*math.Pi)
	centreX, centreZ := float64(anchor[0])+0.5, float64(anchor[2])+0.5
	pos := [3]float64{
		centreX + paddockHorseRadius*math.Sin(angle),
		float64(anchor[1]),
		centreZ + paddockHorseRadius*math.Cos(angle),
	}
	// The mesh faces -Z at yaw zero. This is the tangent of the increasing angle above.
	yaw := wrapAngle(angle - math.Pi/2)
	return pos, yaw
}

// materialisePaddockHorseLocked creates the horse whose anchor lies in coord. The caller
// has already assigned a stable-wide colour seed by sorting all three paddock anchors.
// The caller holds Sim.mu.
func (s *Sim) materialisePaddockHorseLocked(coord world.Coord, slot world.PlacedAnchor, variant uint8) {
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
	anchor := [3]int64{slot.X, slot.Y, slot.Z}
	pos, yaw := paddockHorsePose(s.worldSeed, anchor, s.worldTick, s.dt)
	s.paddockHorses[id] = &paddockHorse{
		entityID: id,
		anchor:   anchor,
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
		horse.pos, horse.yaw = paddockHorsePose(s.worldSeed, horse.anchor, worldTick, s.dt)
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
