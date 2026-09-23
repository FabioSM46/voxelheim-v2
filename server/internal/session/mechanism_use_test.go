package session_test

import (
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// A mechanism use is a payload an admitted session accepts, and in the open world, which has
// no levers or rune stones, every cell answers NotAMechanism under UseMechanism, echoing the
// cell it named. A
// request that named no cell answers MalformedNoAnchor with no anchor, never the origin.
// Neither ends the session: the second answer arriving proves the first was survived.
func TestAMechanismUseOutsideADungeonIsRefusedAsNotAMechanism(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)
	conn, frames := admit(t, cfg, chunks, sim, peers, 1)
	before := len(frames.actionRefusals())

	conn.in <- protocol.EncodeMechanismUseRequest(protocol.MechanismUseRequest{Pos: [3]int32{-4, 17, 9}, HasPos: true, ClientTick: 1})
	conn.in <- protocol.EncodeMechanismUseRequest(protocol.MechanismUseRequest{ClientTick: 2})
	waitUntil(t, "both mechanism refusals", func() bool { return len(frames.actionRefusals()) == before+2 })

	got := frames.actionRefusals()[before:]
	want := []protocol.ActionRefused{
		{Action: vnet.RefusedActionUseMechanism, Reason: vnet.RefusalReasonNotAMechanism, Anchor: [3]int32{-4, 17, 9}, HasAnchor: true},
		{Action: vnet.RefusedActionUseMechanism, Reason: vnet.RefusalReasonMalformedNoAnchor},
	}
	for i := range want {
		if got[i] != want[i] {
			t.Errorf("refusal %d = %+v, want %+v", i, got[i], want[i])
		}
	}
}
