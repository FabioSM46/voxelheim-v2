package session_test

import (
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

func TestPartyRequestsDeliverInvitesAndAnswerEveryActionableRefusal(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	cfg.ViewDistance = 0
	chunks, sim, peers := editDeps(t, cfg)
	names := []string{"Astrid", "Bjorn", "Cora", "Dag", "Eira", "Finn"}
	connections := make([]*fakeConn, 0, len(names))
	collectors := make([]*collector, 0, len(names))
	for index, name := range names {
		conn, frames := admitNamed(t, cfg, chunks, sim, peers, uint64(index+1), name)
		connections = append(connections, conn)
		collectors = append(collectors, frames)
	}

	send := func(player int, action vnet.PartyAction, target string) {
		connections[player].in <- protocol.EncodePartyRequest(protocol.PartyRequest{Action: action, TargetName: target})
	}
	requestRefusal := func(player int, action vnet.PartyAction, target string, want vnet.RefusalReason) {
		t.Helper()
		before := len(collectors[player].actionRefusals())
		send(player, action, target)
		waitUntil(t, want.String()+" party refusal", func() bool {
			return len(collectors[player].actionRefusals()) == before+1
		})
		got := collectors[player].actionRefusals()[before]
		if got.Action != vnet.RefusedActionParty || got.Reason != want || got.HasAnchor {
			t.Errorf("party refusal = %+v, want Party/%s without anchor", got, want)
		}
	}
	var serverTick uint64
	waitInParty := func(player int) {
		t.Helper()
		if !tickUntil(func() {
			serverTick++
			sim.Step(serverTick)
		}, func() bool {
			leader, _ := collectors[player].partyState()
			return leader == 1
		}) {
			t.Fatalf("%s never received authoritative party state", names[player])
		}
	}

	// Malformed target text is silent and the next valid request proves the session
	// remains usable. No untrusted target is echoed in either response or log.
	send(0, vnet.PartyActionInvite, "bad\nname")
	requestRefusal(0, vnet.PartyActionInvite, "Nobody", vnet.RefusalReasonNoSuchPlayer)
	if got := len(collectors[1].partyInvites()); got != 0 {
		t.Fatalf("malformed/missing targets delivered %d invitations", got)
	}

	send(0, vnet.PartyActionInvite, names[1])
	waitUntil(t, "Bjorn's party invitation", func() bool { return len(collectors[1].partyInvites()) == 1 })
	invite := collectors[1].partyInvites()[0]
	if invite.FromEntityID != 1 || invite.FromName != names[0] || invite.ExpiresMS != uint32(game.PartyInviteTTL.Milliseconds()) {
		t.Errorf("party invitation = %+v", invite)
	}
	send(1, vnet.PartyActionAccept, "ignored")
	waitInParty(1)

	requestRefusal(1, vnet.PartyActionInvite, names[2], vnet.RefusalReasonNotLeader)
	requestRefusal(0, vnet.PartyActionInvite, names[1], vnet.RefusalReasonAlreadyInParty)
	requestRefusal(1, vnet.PartyActionAccept, "ignored", vnet.RefusalReasonNoInvite)

	// Fill the leader's party to five through the wire, then the sixth target is the
	// PartyFull outcome rather than an invite the target could never accept.
	for index := 2; index < game.MaxPartySize; index++ {
		wantCount := len(collectors[index].partyInvites()) + 1
		send(0, vnet.PartyActionInvite, names[index])
		waitUntil(t, "party invite to "+names[index], func() bool {
			return len(collectors[index].partyInvites()) == wantCount
		})
		send(index, vnet.PartyActionAccept, "ignored")
		waitInParty(index)
	}
	requestRefusal(0, vnet.PartyActionInvite, names[5], vnet.RefusalReasonPartyFull)
	if got := len(collectors[5].partyInvites()); got != 0 {
		t.Errorf("full party delivered %d invitations to the sixth player", got)
	}
}
