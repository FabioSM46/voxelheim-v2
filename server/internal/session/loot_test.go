package session_test

import (
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

func TestLootRequestsAreDispatchedAndRefusedWithoutEndingSession(t *testing.T) {
	t.Parallel()
	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)
	conn, frames := admit(t, cfg, chunks, sim, peers, 1)

	conn.in <- protocol.EncodeLootOpenRequest(protocol.LootOpenRequest{CorpseID: 404, ClientTick: 1})
	conn.in <- protocol.EncodeLootTakeRequest(protocol.LootTakeRequest{CorpseID: 404, EntryID: 1, Revision: 1, ClientTick: 1})
	waitUntil(t, "both loot refusals", func() bool { return len(frames.actionRefusals()) == 2 })

	want := []protocol.ActionRefused{
		{Action: vnet.RefusedActionOpenLoot, Reason: vnet.RefusalReasonCorpseUnavailable},
		{Action: vnet.RefusedActionTakeLoot, Reason: vnet.RefusalReasonCorpseUnavailable},
	}
	got := frames.actionRefusals()
	for index := range want {
		if got[index] != want[index] {
			t.Errorf("refusal %d = %+v, want %+v", index, got[index], want[index])
		}
	}
	states, closed, accessible := frames.lootState()
	if len(states) != 0 || len(closed) != 0 || len(accessible) != 0 {
		t.Fatalf("refused requests exposed loot state=%+v closed=%v accessible=%v", states, closed, accessible)
	}
}
