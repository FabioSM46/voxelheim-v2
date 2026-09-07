package game

import (
	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// landedBlow holds identities, never a contact coordinate: disclosure is a projection
// of the final snapshot, not a second interest-management or world-position rule.
// Only resolved melee/projectile contacts and a mob's successful damage call write it.
type landedBlow struct {
	attacker, target uint64
	kind             vnet.BlowKind
	targetKind       vnet.BlowTarget
}

// creditMobBlowLocked surrounds the existing outcome without changing its arithmetic,
// tap, threat or kill paths. A killing blow still leaves a corpse with this target id.
func (s *Sim) creditMobBlowLocked(attacker *Player, target *mob, damage uint16, kind vnet.BlowKind) {
	if target == nil {
		return
	}
	before := target.health
	s.creditMobDamageLocked(attacker, target, damage)
	if target.health >= before {
		return
	}
	source := uint64(0)
	if attacker != nil {
		source = attacker.entityID
	}
	s.blows = append(s.blows, landedBlow{attacker: source, target: target.entityID, kind: kind, targetKind: vnet.BlowTargetMob})
}

// blowFramesLocked uses the exact snapshot vectors, including corpses and dead players.
// A party-only source, absent target or a body that left interest grants no exception.
// Call once per viewer; the resulting frames travel alongside that snapshot through
// the replacement mailbox and are never retried against another tick's visibility.
func (s *Sim) blowFramesLocked(snapshot protocol.EntitySnapshot) [][]byte {
	if len(s.blows) == 0 {
		return nil
	}
	players := make(map[uint64]protocol.EntityState, len(snapshot.Entities))
	mobs := make(map[uint64]protocol.MobState, len(snapshot.Mobs))
	for _, entity := range snapshot.Entities {
		players[entity.EntityID] = entity
	}
	for _, mob := range snapshot.Mobs {
		mobs[mob.EntityID] = mob
	}
	var frames [][]byte
	for _, contact := range s.blows {
		blow := protocol.BlowLanded{Tick: snapshot.Tick, TargetEntityID: contact.target, Kind: contact.kind, Target: contact.targetKind}
		switch contact.targetKind {
		case vnet.BlowTargetPlayer:
			entity, visible := players[contact.target]
			if !visible {
				continue
			}
			blow.Position = entity.Pos
		case vnet.BlowTargetMob:
			mob, visible := mobs[contact.target]
			if !visible {
				continue
			}
			blow.Position, blow.TargetMobKind = mob.Pos, mob.Kind
		default:
			continue
		}
		_, playerVisible := players[contact.attacker]
		_, mobVisible := mobs[contact.attacker]
		if playerVisible || mobVisible {
			blow.AttackerEntityID = contact.attacker
		}
		frame, err := protocol.EncodeBlowLanded(blow)
		if err != nil {
			s.log.Error("invalid resolved blow", "error", err)
			continue
		}
		frames = append(frames, frame)
	}
	return frames
}
