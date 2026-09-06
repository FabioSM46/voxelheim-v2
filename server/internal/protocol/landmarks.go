package protocol

import (
	"fmt"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	flatbuffers "github.com/google/flatbuffers/go"
)

// MaxLandmarks is the per-tile V32 bound from schemas/player.fbs. Every supported
// map tile fits in one ruin lattice cell, which contains at most one portal.
const MaxLandmarks = 1

// Landmark is a server-supplied place, with identity and discovery scoped to one world.
type Landmark struct {
	LandmarkID uint64
	X, Z       int32
	Kind       vnet.LandmarkKind
	Discovered bool
}

// LandmarkList replaces membership only within its half-open map-tile rectangle.
// Empty is legal. Discovery=true remains authoritative across raced false replies.
type LandmarkList struct {
	Landmarks        []Landmark
	OriginX, OriginZ int32
	Scale            uint8
}

// EncodeLandmarkList validates the complete list before constructing the frame.
// A producer exceeding the contract is an error, never a silently shortened list.
func EncodeLandmarkList(list LandmarkList) ([]byte, error) {
	span := MapTileSpan(list.Scale)
	if span == 0 || list.OriginX%span != 0 || list.OriginZ%span != 0 {
		return nil, fmt.Errorf("protocol: invalid landmark tile scope")
	}
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
		if int64(landmark.X) < int64(list.OriginX) || int64(landmark.X) >= int64(list.OriginX)+int64(span) || int64(landmark.Z) < int64(list.OriginZ) || int64(landmark.Z) >= int64(list.OriginZ)+int64(span) {
			return nil, fmt.Errorf("protocol: landmark outside tile scope")
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
		vnet.LandmarkAddDiscovered(b, landmark.Discovered)
		offsets[i] = vnet.LandmarkEnd(b)
	}
	vnet.LandmarkListStartLandmarksVector(b, len(offsets))
	for i := len(offsets) - 1; i >= 0; i-- {
		b.PrependUOffsetT(offsets[i])
	}
	entries := b.EndVector(len(offsets))
	vnet.LandmarkListStart(b)
	vnet.LandmarkListAddLandmarks(b, entries)
	vnet.LandmarkListAddOriginX(b, list.OriginX)
	vnet.LandmarkListAddOriginZ(b, list.OriginZ)
	vnet.LandmarkListAddScale(b, list.Scale)
	return finishEnvelope(b, vnet.PayloadLandmarkList, vnet.LandmarkListEnd(b)), nil
}
