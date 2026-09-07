package protocol

import (
	"fmt"
	"math"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	flatbuffers "github.com/google/flatbuffers/go"
)

// BlowLanded describes one resolved contact projected against a recipient's snapshot.
// The simulation owns visibility; this encoder only enforces the payload's invariants.
type BlowLanded struct {
	Tick             uint32
	AttackerEntityID uint64
	TargetEntityID   uint64
	Position         [3]float32
	Kind             vnet.BlowKind
	Target           vnet.BlowTarget
	TargetMobKind    vnet.MobKind
}

func EncodeBlowLanded(blow BlowLanded) ([]byte, error) {
	if blow.TargetEntityID == 0 {
		return nil, fmt.Errorf("protocol: blow has no target")
	}
	switch blow.Kind {
	case vnet.BlowKindMelee, vnet.BlowKindArrow, vnet.BlowKindEnergyOrb, vnet.BlowKindMobMelee:
	default:
		return nil, fmt.Errorf("protocol: unknown blow kind")
	}
	switch blow.Target {
	case vnet.BlowTargetPlayer:
		if blow.TargetMobKind != vnet.MobKindUnknown {
			return nil, fmt.Errorf("protocol: player blow has a mob kind")
		}
	case vnet.BlowTargetMob:
		if _, known := vnet.EnumNamesMobKind[blow.TargetMobKind]; !known || blow.TargetMobKind == vnet.MobKindUnknown {
			return nil, fmt.Errorf("protocol: unknown blow target mob kind")
		}
	default:
		return nil, fmt.Errorf("protocol: unknown blow target")
	}
	for _, coordinate := range blow.Position {
		if math.IsNaN(float64(coordinate)) || math.IsInf(float64(coordinate), 0) {
			return nil, fmt.Errorf("protocol: non-finite blow position")
		}
	}
	b := flatbuffers.NewBuilder(128)
	vnet.BlowLandedStart(b)
	vnet.BlowLandedAddTick(b, blow.Tick)
	vnet.BlowLandedAddAttackerEntityId(b, blow.AttackerEntityID)
	vnet.BlowLandedAddTargetEntityId(b, blow.TargetEntityID)
	position := vnet.CreateVec3(b, blow.Position[0], blow.Position[1], blow.Position[2])
	vnet.BlowLandedAddPosition(b, position)
	vnet.BlowLandedAddKind(b, blow.Kind)
	vnet.BlowLandedAddTarget(b, blow.Target)
	vnet.BlowLandedAddTargetMobKind(b, blow.TargetMobKind)
	return finishEnvelope(b, vnet.PayloadBlowLanded, vnet.BlowLandedEnd(b)), nil
}
