package protocol

import (
	"math"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// A mechanism use decodes as the cell and tick it carries, the extremes of both included, and
// an absent cell decodes as absent rather than as the world origin.
func TestMechanismUseRequestRoundTripsItsCellAndAbsence(t *testing.T) {
	t.Parallel()

	for _, want := range []MechanismUseRequest{
		{Pos: [3]int32{math.MinInt32, -64, math.MaxInt32}, HasPos: true, ClientTick: math.MaxUint32},
		{Pos: [3]int32{0, 0, 0}, HasPos: true, ClientTick: 0},
		{ClientTick: 7},
	} {
		msg, err := Decode(EncodeMechanismUseRequest(want))
		if err != nil {
			t.Fatalf("Decode: %v", err)
		}
		if msg.Kind != vnet.PayloadMechanismUseRequest || msg.MechanismUse == nil {
			t.Fatalf("message = %+v, want MechanismUseRequest", msg)
		}
		if *msg.MechanismUse != want {
			t.Errorf("MechanismUseRequest = %+v, want %+v", *msg.MechanismUse, want)
		}
	}
}

// The refusal a mechanism use is answered with survives the wire with its V46 action and
// reasons, and the anchor it echoes.
func TestMechanismRefusalsRoundTrip(t *testing.T) {
	t.Parallel()

	for _, want := range []ActionRefused{
		{Action: vnet.RefusedActionUseMechanism, Reason: vnet.RefusalReasonNotAMechanism, Anchor: [3]int32{3, -2, 1}, HasAnchor: true},
		{Action: vnet.RefusedActionUseMechanism, Reason: vnet.RefusalReasonMechanismLocked, Anchor: [3]int32{3, -2, 1}, HasAnchor: true},
		{Action: vnet.RefusedActionUseMechanism, Reason: vnet.RefusalReasonOutOfReach, Anchor: [3]int32{90, 0, 90}, HasAnchor: true},
		{Action: vnet.RefusedActionUseMechanism, Reason: vnet.RefusalReasonPlayerIsDead, Anchor: [3]int32{0, 0, 0}, HasAnchor: true},
	} {
		msg, err := Decode(EncodeActionRefused(want))
		if err != nil {
			t.Fatalf("Decode: %v", err)
		}
		if msg.ActionRefused == nil || *msg.ActionRefused != want {
			t.Errorf("ActionRefused = %+v, want %+v", msg.ActionRefused, want)
		}
	}
}
