package game

import (
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// A refusal decided after a take returned is delivered by the tick, survives a full queue,
// and is not queued twice.
func TestAQueuedLootRefusalReachesTheSessionOnceEvenAfterAFullQueue(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	p, out := h.join(1, [3]float32{0.5, 64, 0.5})
	out.setFull(true)
	p.QueueLootRefusal(vnet.RefusalReasonInventoryFull)
	p.QueueLootRefusal(vnet.RefusalReasonInventoryFull)
	p.QueueLootRefusal(vnet.RefusalReasonUnknown)
	h.step()
	if got := actionRefusals(t, out); len(got) != 0 {
		t.Fatalf("a refusal was delivered into a full queue: %+v", got)
	}
	out.setFull(false)
	h.step()
	h.step()
	want := protocol.ActionRefused{Action: vnet.RefusedActionTakeLoot, Reason: vnet.RefusalReasonInventoryFull}
	if got := actionRefusals(t, out); len(got) != 1 || got[0] != want {
		t.Fatalf("delivered refusals = %+v, want exactly %+v", got, want)
	}
}
