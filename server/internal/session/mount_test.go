package session_test

import (
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// Both V27 intents have to reach the admitted player rather than falling through the
// direction guard. The dismount in the middle is load-bearing: a router that knows
// MountRequest but not the deliberately empty DismountRequest would end the session
// before the second actionable refusal could arrive.
func TestMountAndDismountRequestsAreRoutedWithoutEndingTheSession(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)
	conn, frames := admit(t, cfg, chunks, sim, peers, 1)

	request := protocol.EncodeMountRequest(protocol.MountRequest{Mount: vnet.MountKindBlackHorse})
	conn.in <- request
	conn.in <- protocol.EncodeDismountRequest()
	conn.in <- request
	waitUntil(t, "both unlearned mount refusals", func() bool { return len(frames.actionRefusals()) == 2 })

	want := protocol.ActionRefused{
		Action: vnet.RefusedActionMount,
		Reason: vnet.RefusalReasonMountNotLearned,
	}
	for index, got := range frames.actionRefusals() {
		if got != want {
			t.Errorf("refusal %d = %+v, want %+v", index, got, want)
		}
	}
}
