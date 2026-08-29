package session_test

import (
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// **The arm is the point of this test, not the answer it produces.** V25 taught the
// protocol boundary to decode an NpcInteractRequest; until the router had a case for it
// the message fell through the default and closed the session as malformed — a live V25
// client hung up on by a server that understood every byte it sent. That is the loot
// take-all defect one release later, and this is the test that shape earned.
//
// Two requests rather than one, and the second is the load-bearing half: it can only be
// answered by a session that survived the first.
func TestNpcInteractionIsRoutedAndRefusedWithoutEndingSession(t *testing.T) {
	t.Parallel()
	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)
	conn, frames := admit(t, cfg, chunks, sim, peers, 1)

	// An id nobody holds, and then the session's own body. Neither is a resident, and
	// neither may learn anything from the difference: the refusal is the fail-closed
	// default, so a client cannot probe the world with it.
	conn.in <- protocol.EncodeNpcInteractRequest(protocol.NpcInteractRequest{EntityID: 404, ClientTick: 1})
	conn.in <- protocol.EncodeNpcInteractRequest(protocol.NpcInteractRequest{EntityID: 1, ClientTick: 2})
	waitUntil(t, "both interaction refusals", func() bool { return len(frames.actionRefusals()) == 2 })

	want := protocol.ActionRefused{
		Action: vnet.RefusedActionInteract,
		Reason: vnet.RefusalReasonNotAVendor,
	}
	for index, got := range frames.actionRefusals() {
		if got != want {
			t.Errorf("refusal %d = %+v, want %+v", index, got, want)
		}
	}
}

// **The arm again, one message later.** V25 taught the protocol boundary to decode a
// TradeRequest at the same moment it taught it NpcInteractRequest, and until #459 the
// router had a case for neither — so this is the same defect the test above exists for,
// on the message that was still falling through the default when the other stopped.
//
// The answer is the fail-closed one: this session has opened no stall, so there is
// nothing to trade with whatever the request names. Two requests again, and the second is
// the load-bearing half.
func TestATradeIsRoutedAndRefusedWithoutEndingSession(t *testing.T) {
	t.Parallel()
	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)
	conn, frames := admit(t, cfg, chunks, sim, peers, 1)

	conn.in <- protocol.EncodeTradeRequest(protocol.TradeRequest{
		EntityID: 404, ItemID: 1, Count: 1, Buying: true, Revision: 1, ClientTick: 1,
	})
	conn.in <- protocol.EncodeTradeRequest(protocol.TradeRequest{
		EntityID: 404, ItemID: 1, Count: 1, Buying: false, Revision: 1, ClientTick: 2,
	})
	waitUntil(t, "both trade refusals", func() bool { return len(frames.actionRefusals()) == 2 })

	want := protocol.ActionRefused{
		Action: vnet.RefusedActionTrade,
		Reason: vnet.RefusalReasonNotAVendor,
	}
	for index, got := range frames.actionRefusals() {
		if got != want {
			t.Errorf("refusal %d = %+v, want %+v", index, got, want)
		}
	}
}
