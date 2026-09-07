package session_test

import (
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// An answer naming an offer the server never made crosses nothing.
//
// **The forgery is the point, and so is where it is caught.** A client can put any
// number in this field; the entry rules hold every offer this server has actually made,
// keyed by the character it was made for, so an invented id, a session id and a
// plausible-looking counter all reach the same refusal. The rules themselves are pinned
// in game/instance_entry_test.go — what this test is about is the wire path: a payload a
// V34 server had no name for is decoded, routed, refused as a crossing, and leaves the
// session exactly where it was.
func TestAForgedEntryAnswerCrossesNothing(t *testing.T) {
	cfg, chunks, open, peers, _ := portalSession(t, 2)
	id := peers.NextID()
	conn, frames := admit(t, cfg, chunks, open, peers, id)

	for _, answer := range []protocol.InstanceEntryAnswer{
		{OfferID: 0xDEADBEEF, Accept: true},
		{OfferID: 1, Accept: true},
		{},
	} {
		conn.in <- protocol.EncodeInstanceEntryAnswer(answer)
	}
	waitUntil(t, "refusals", func() bool { return len(frames.actionRefusals()) == 3 })
	for _, refused := range frames.actionRefusals() {
		if refused.Action != vnet.RefusedActionCrossPortal || refused.Reason != vnet.RefusalReasonEntryOfferUnknown {
			t.Fatalf("a forged answer was answered with %+v", refused)
		}
	}
	// Nothing moved: no world change, no instance, and the character is still in the
	// open simulation it started in.
	if len(frames.transitions()) != 0 {
		t.Fatalf("a forged answer moved the session: %+v", frames.transitions())
	}
	if cfg.Instances.Count() != 0 || open.Count() != 1 {
		t.Fatalf("a forged answer allocated a world: %d instances, %d in the open world", cfg.Instances.Count(), open.Count())
	}
}

// A character is told what they owe on the way in, empty list included.
//
// **The empty list is the case worth pinning**, because it is the one silence would be
// mistaken for: the frame replaces the client's copy wholesale, so a character who owed
// a run yesterday and nothing today has to be told the difference. What the list says
// when it is not empty is the manager's, and it is pinned in
// game/instance_bindings_test.go; what this covers is that the connection states it at
// all, once, before it starts streaming a world.
func TestASessionIsToldWhatItOwesOnTheWayIn(t *testing.T) {
	cfg, chunks, open, peers, _ := portalSession(t, 2)
	id := peers.NextID()
	_, frames := admit(t, cfg, chunks, open, peers, id)

	waitUntil(t, "saved-run list", func() bool { return len(frames.savedRunLists()) == 1 })
	if got := frames.savedRunLists()[0]; len(got) != 0 {
		t.Fatalf("a character who owes nothing was sent %+v", got)
	}
	// Once, not once per tick: the list is sent on entry and on change, and nothing has
	// changed.
	for range 5 {
		cfg.Instances.Step()
	}
	conn2 := frames.savedRunLists()
	if len(conn2) != 1 {
		t.Fatalf("the list was restated with nothing to restate: %d frames", len(conn2))
	}
}
