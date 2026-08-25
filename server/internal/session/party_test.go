package session_test

import (
	"context"
	"slices"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
)

func TestPartyRosterRebindsThePersistedCharacterAcrossSessions(t *testing.T) {
	cfg := editConfig()
	cfg.ViewDistance = 0
	chunks, sim, peers := editDeps(t, cfg)
	identities := identitiesOver(nil)

	type liveSession struct {
		conn   *fakeConn
		frames *collector
		done   chan error
	}
	start := func(entityID uint64, accountSeed byte, name string, expectedLive int) liveSession {
		t.Helper()
		conn := newFakeConn()
		done := make(chan error, 1)
		go func() {
			done <- session.Serve(context.Background(), conn, cfg, noTimeouts(), chunks, sim, peers, identities, entityID, discard())
		}()
		conn.in <- hello(accountSeed)
		chooseCharacter(t, conn, name)
		_ = nextFrameOfKind(t, conn, vnet.PayloadServerWelcome)
		frames := collect(t, conn)
		waitUntil(t, "the character to enter the simulation", func() bool {
			return sim.Count() == expectedLive
		})
		return liveSession{conn: conn, frames: frames, done: done}
	}
	stop := func(live liveSession) {
		t.Helper()
		if err := live.conn.Close(); err != nil {
			t.Fatalf("close session: %v", err)
		}
		if err := <-live.done; err != nil {
			t.Fatalf("session ended: %v", err)
		}
	}

	leader := start(1, 1, "Astrid", 1)
	member := start(2, 2, "Bjorn", 2)
	leader.conn.in <- protocol.EncodePartyRequest(protocol.PartyRequest{Action: vnet.PartyActionInvite, TargetName: "Bjorn"})
	waitUntil(t, "the party invitation", func() bool { return len(member.frames.partyInvites()) == 1 })
	member.conn.in <- protocol.EncodePartyRequest(protocol.PartyRequest{Action: vnet.PartyActionAccept})

	var tick uint64
	step := func() {
		tick++
		sim.Step(tick)
	}
	if !tickUntil(step, func() bool { return len(leader.frames.rosterState()) == 2 }) {
		t.Fatal("the initial party roster never arrived")
	}
	before := leader.frames.rosterState()
	if before[0].EntityID != 1 || before[1].EntityID != 2 || before[0].CharacterID == 0 || before[1].CharacterID == 0 {
		t.Fatalf("initial roster = %+v", before)
	}

	stop(member)
	waitUntil(t, "the disconnected member to leave the simulation", func() bool { return sim.Count() == 1 })
	if !tickUntil(step, func() bool {
		roster := leader.frames.rosterState()
		return len(roster) == 2 && !roster[1].Online && roster[1].EntityID == 0
	}) {
		t.Fatal("disconnect never produced the offline stable roster entry")
	}

	member = start(20, 2, "Bjorn", 2)
	if !tickUntil(step, func() bool {
		roster := leader.frames.rosterState()
		return len(roster) == 2 && roster[1].Online && roster[1].EntityID == 20
	}) {
		t.Fatal("reconnect never rebound the roster to the new entity id")
	}
	after := leader.frames.rosterState()
	if !slices.Equal([]uint64{before[0].CharacterID, before[1].CharacterID}, []uint64{after[0].CharacterID, after[1].CharacterID}) {
		t.Fatalf("character order changed across reconnect: before=%+v after=%+v", before, after)
	}
	leaderID, liveMembers := leader.frames.partyState()
	if leaderID != 1 || len(liveMembers) != 1 || liveMembers[0].EntityID != 20 {
		t.Fatalf("live party projection after reconnect = leader %d members %+v", leaderID, liveMembers)
	}

	stop(member)
	stop(leader)
}

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
