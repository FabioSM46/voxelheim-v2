package protocol

import (
	"fmt"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	flatbuffers "github.com/google/flatbuffers/go"
)

// MaxLandmarks is the complete-list bound from schemas/player.fbs. It covers the
// exploration ledger without truncation and fits the V31 2 MiB frame limit.
const MaxLandmarks = 65536

// Landmark is a server-discovered place, with stable identity within one world.
type Landmark struct {
	LandmarkID uint64
	X, Z       int32
	Kind       vnet.LandmarkKind
}

// LandmarkList replaces the recipient's entire copy, including when empty.
type LandmarkList struct{ Landmarks []Landmark }

// EncodeLandmarkList validates the complete list before constructing the frame.
// A producer exceeding the contract is an error, never a silently shortened list.
func EncodeLandmarkList(list LandmarkList) ([]byte, error) {
	if len(list.Landmarks) > MaxLandmarks {
		return nil, fmt.Errorf("protocol: landmark count exceeds %d", MaxLandmarks)
	}
	seen := make(map[uint64]struct{}, len(list.Landmarks))
	for _, landmark := range list.Landmarks {
		if landmark.LandmarkID == 0 || landmark.Kind != vnet.LandmarkKindPortal {
			return nil, fmt.Errorf("protocol: landmark has invalid identity or kind")
		}
		if _, duplicate := seen[landmark.LandmarkID]; duplicate {
			return nil, fmt.Errorf("protocol: duplicate landmark identity")
		}
		seen[landmark.LandmarkID] = struct{}{}
	}
	b := flatbuffers.NewBuilder(len(list.Landmarks)*32 + 128)
	offsets := make([]flatbuffers.UOffsetT, len(list.Landmarks))
	for i, landmark := range list.Landmarks {
		vnet.LandmarkStart(b)
		vnet.LandmarkAddLandmarkId(b, landmark.LandmarkID)
		vnet.LandmarkAddX(b, landmark.X)
		vnet.LandmarkAddZ(b, landmark.Z)
		vnet.LandmarkAddKind(b, landmark.Kind)
		offsets[i] = vnet.LandmarkEnd(b)
	}
	vnet.LandmarkListStartLandmarksVector(b, len(offsets))
	for i := len(offsets) - 1; i >= 0; i-- {
		b.PrependUOffsetT(offsets[i])
	}
	entries := b.EndVector(len(offsets))
	vnet.LandmarkListStart(b)
	vnet.LandmarkListAddLandmarks(b, entries)
	return finishEnvelope(b, vnet.PayloadLandmarkList, vnet.LandmarkListEnd(b)), nil
}
