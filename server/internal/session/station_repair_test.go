package session_test

import (
	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"testing"
)

// Until #1314 implements forge repair, the declared intent gets an explicit refusal
// without closing an admitted session. Two answers prove the first request survived.
func TestStationRepairContractStubAnswersWithoutDisconnecting(t *testing.T) {
	t.Parallel()
	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)
	conn, frames := admit(t, cfg, chunks, sim, peers, 1)
	before := len(frames.actionRefusals())
	for _, slot := range []uint16{0, 65535} {
		conn.in <- protocol.EncodeStationRepairRequest(protocol.StationRepairRequest{TargetSlot: slot})
	}
	waitUntil(t, "both station repair refusals", func() bool { return len(frames.actionRefusals()) == before+2 })
	for _, got := range frames.actionRefusals()[before:] {
		want := protocol.ActionRefused{Action: vnet.RefusedActionStationRepair, Reason: vnet.RefusalReasonMalformedKind}
		if got != want {
			t.Errorf("refusal = %+v, want %+v", got, want)
		}
	}
}
