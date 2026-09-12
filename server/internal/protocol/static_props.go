package protocol

import (
	"fmt"
	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
	flatbuffers "github.com/google/flatbuffers/go"
)

// MaxStaticProps bounds the one capital's complete authored catalogue, lights included.
const MaxStaticProps = 256

// StaticPropState is immutable decoration in its own identity namespace.
// Origin is a floor-plane origin; unlike StructureState no supporting-block offset applies.
type StaticPropState struct {
	PropID  uint64
	Kind    vnet.StaticPropKind
	Origin  [3]int32
	Facing  vnet.Facing
	Variant uint8
}

func encodeStaticProps(b *flatbuffers.Builder, states []StaticPropState) flatbuffers.UOffsetT {
	if len(states) == 0 {
		return 0
	}
	offsets := make([]flatbuffers.UOffsetT, len(states))
	for i, state := range states {
		vnet.StaticPropStateStart(b)
		vnet.StaticPropStateAddPropId(b, state.PropID)
		vnet.StaticPropStateAddKind(b, state.Kind)
		origin := vnet.CreateBlockCoord(b, state.Origin[0], state.Origin[1], state.Origin[2])
		vnet.StaticPropStateAddOrigin(b, origin)
		vnet.StaticPropStateAddFacing(b, state.Facing)
		vnet.StaticPropStateAddVariant(b, state.Variant)
		offsets[i] = vnet.StaticPropStateEnd(b)
	}
	vnet.EntitySnapshotStartStaticPropsVector(b, len(offsets))
	for i := len(offsets) - 1; i >= 0; i-- {
		b.PrependUOffsetT(offsets[i])
	}
	return b.EndVector(len(offsets))
}

func validateStaticProps(snapshot *vnet.EntitySnapshot) error {
	count := snapshot.StaticPropsLength()
	if count > MaxStaticProps {
		return fmt.Errorf("%w: static prop count exceeds %d", ErrMalformed, MaxStaticProps)
	}
	ids := make(map[uint64]struct{}, count)
	for i := 0; i < count; i++ {
		var state vnet.StaticPropState
		if !snapshot.StaticProps(&state, i) {
			return fmt.Errorf("%w: missing static prop", ErrMalformed)
		}
		id := state.PropId()
		if id == 0 {
			return fmt.Errorf("%w: zero static prop identity", ErrMalformed)
		}
		if _, duplicate := ids[id]; duplicate {
			return fmt.Errorf("%w: duplicate static prop identity", ErrMalformed)
		}
		ids[id] = struct{}{}
		if state.Kind() < vnet.StaticPropKindBanquetTable || state.Kind() > vnet.StaticPropKindTableCandelabrum {
			return fmt.Errorf("%w: unknown static prop kind", ErrMalformed)
		}
		if state.Facing() < vnet.FacingNorth || state.Facing() > vnet.FacingWest || state.Variant() > 3 {
			return fmt.Errorf("%w: invalid static prop facing or variant", ErrMalformed)
		}
		origin := state.Origin(nil)
		if origin == nil {
			return fmt.Errorf("%w: missing static prop origin", ErrMalformed)
		}
		for _, component := range [3]int32{origin.X(), origin.Y(), origin.Z()} {
			if int64(component) < -world.BlockLimit || int64(component) >= world.BlockLimit {
				return fmt.Errorf("%w: static prop origin outside world", ErrMalformed)
			}
		}
	}
	return nil
}
