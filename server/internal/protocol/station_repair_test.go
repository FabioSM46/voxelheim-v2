package protocol

import (
	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"testing"
)

func TestStationRepairRequestPreservesTheWholeSlotRange(t *testing.T) {
	t.Parallel()
	for _, slot := range []uint16{0, 39, 40, 255, 256, 65535} {
		want := StationRepairRequest{TargetSlot: slot}
		frame := EncodeStationRepairRequest(want)
		msg, err := Decode(frame)
		if err != nil || msg.Kind != vnet.PayloadStationRepairRequest || msg.StationRepair == nil || *msg.StationRepair != want {
			t.Fatalf("slot %d: message %+v, error %v", slot, msg, err)
		}
		// Malformed input stays inside Decode's panic boundary, including truncated tables.
		for cut := range len(frame) {
			_, _ = Decode(frame[:cut])
		}
	}
}

func TestHoardRefusalsRoundTrip(t *testing.T) {
	t.Parallel()
	for _, want := range []ActionRefused{
		{Action: vnet.RefusedActionUseMechanism, Reason: vnet.RefusalReasonChestAlreadyOpened},
		{Action: vnet.RefusedActionStationRepair, Reason: vnet.RefusalReasonNothingToRepair},
		{Action: vnet.RefusedActionStationRepair, Reason: vnet.RefusalReasonNotAtStation},
		{Action: vnet.RefusedActionStationRepair, Reason: vnet.RefusalReasonNotEnoughSilver},
	} {
		msg, err := Decode(EncodeActionRefused(want))
		if err != nil || msg.ActionRefused == nil || *msg.ActionRefused != want {
			t.Fatalf("refusal %+v: message %+v, error %v", want, msg, err)
		}
	}
}
