package protocol

import (
	"fmt"
	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	flatbuffers "github.com/google/flatbuffers/go"
	"math"
)

// PortalRequest is copied intent. Even an absent or invented arch has the same
// coarse gameplay refusal, so decoding preserves presence without choosing policy.
// MaxWorldCoordinate is the contract's +/-2^24 block domain.
const MaxWorldCoordinate = 1 << 24

type PortalRequest struct {
	Arch    [3]int32
	HasArch bool
}

// WorldChange invalidates every world-derived client value before destination
// output begins. Zero WorldID is the open world; other ids belong to this connection.
type WorldChange struct {
	WorldID     uint64
	WorldSeed   int64
	Arrival     [3]float32
	ExitArch    [3]int32
	HasExitArch bool
}

func EncodePortalRequest(request PortalRequest) []byte {
	b := flatbuffers.NewBuilder(64)
	vnet.PortalRequestStart(b)
	if request.HasArch {
		arch := vnet.CreateBlockCoord(b, request.Arch[0], request.Arch[1], request.Arch[2])
		vnet.PortalRequestAddArch(b, arch)
	}
	return finishEnvelope(b, vnet.PayloadPortalRequest, vnet.PortalRequestEnd(b))
}

func EncodeWorldChange(change WorldChange) ([]byte, error) {
	if (change.WorldID != 0) != change.HasExitArch {
		return nil, fmt.Errorf("protocol: world identity and exit arch disagree")
	}
	for _, coordinate := range change.Arrival {
		if math.IsNaN(float64(coordinate)) || math.IsInf(float64(coordinate), 0) || coordinate < -MaxWorldCoordinate || coordinate > MaxWorldCoordinate {
			return nil, fmt.Errorf("protocol: invalid world arrival")
		}
	}
	if change.HasExitArch {
		for _, coordinate := range change.ExitArch {
			if coordinate < -MaxWorldCoordinate || coordinate > MaxWorldCoordinate {
				return nil, fmt.Errorf("protocol: invalid exit arch")
			}
		}
	}
	b := flatbuffers.NewBuilder(128)
	vnet.WorldChangeStart(b)
	vnet.WorldChangeAddWorldId(b, change.WorldID)
	vnet.WorldChangeAddWorldSeed(b, change.WorldSeed)
	arrival := vnet.CreateVec3(b, change.Arrival[0], change.Arrival[1], change.Arrival[2])
	vnet.WorldChangeAddArrival(b, arrival)
	if change.HasExitArch {
		arch := vnet.CreateBlockCoord(b, change.ExitArch[0], change.ExitArch[1], change.ExitArch[2])
		vnet.WorldChangeAddExitArch(b, arch)
	}
	return finishEnvelope(b, vnet.PayloadWorldChange, vnet.WorldChangeEnd(b)), nil
}
