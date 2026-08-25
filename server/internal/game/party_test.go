package game

import (
	"fmt"
	"slices"
	"strings"
	"testing"

	flatbuffers "github.com/google/flatbuffers/go"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

func joinPartyPlayer(t *testing.T, h *vitalsHarness, entityID uint64, name string, pos [3]float32) (*Player, *dropSink) {
	t.Helper()
	out := &dropSink{}
	player, err := h.sim.Join(entityID, testPlayerID(entityID), name, pos, testAppearance(), nil, out.deliver)
	if err != nil {
		t.Fatalf("Join %s: %v", name, err)
	}
	return player, out
}

func mustParty(t *testing.T, player *Player, action vnet.PartyAction, target string) {
	t.Helper()
	reason, err := player.Party(action, target)
	if err != nil || reason != vnet.RefusalReasonUnknown {
		t.Fatalf("Party(%s, %q) = %s, %v", action, target, reason, err)
	}
}

func wantPartyRefusal(t *testing.T, player *Player, action vnet.PartyAction, target string, want vnet.RefusalReason) {
	t.Helper()
	reason, err := player.Party(action, target)
	if err == nil || reason != want {
		t.Fatalf("Party(%s, %q) = %s, %v; want %s refusal", action, target, reason, err, want)
	}
}

func inviteAndAccept(t *testing.T, inviter, target *Player, targetName string) {
	t.Helper()
	mustParty(t, inviter, vnet.PartyActionInvite, targetName)
	mustParty(t, target, vnet.PartyActionAccept, "")
}

func TestPartyTargetsHaveTheCharacterNameShape(t *testing.T) {
	t.Parallel()

	if maxPartyTargetBytes != persist.MaxNameBytes {
		t.Fatalf("party target cap = %d, persisted name cap = %d", maxPartyTargetBytes, persist.MaxNameBytes)
	}
	for _, name := range []string{"Eivor", "SKJALD", "Åsa", "Kari", "ᛁvar"} {
		_, persistedFold, err := persist.AcceptName(name)
		if err != nil {
			t.Fatalf("persist.AcceptName(%q): %v", name, err)
		}
		_, partyFold, err := acceptPartyTarget(name)
		if err != nil {
			t.Fatalf("acceptPartyTarget(%q): %v", name, err)
		}
		if partyFold != persistedFold {
			t.Errorf("party fold for %q = %q, persisted fold = %q", name, partyFold, persistedFold)
		}
	}
	for name, target := range map[string]string{
		"empty":        "  ",
		"too long":     strings.Repeat("a", persist.MaxNameBytes+1),
		"invalid UTF8": string([]byte{0xff}),
		"control":      "Sig\nrun",
	} {
		t.Run(name, func(t *testing.T) {
			if _, _, err := acceptPartyTarget(target); err == nil {
				t.Error("target was accepted")
			} else if strings.Contains(err.Error(), target) {
				t.Error("refusal quoted the untrusted target")
			}
		})
	}

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	inviter, _ := joinPartyPlayer(t, h, 1, "Astrid", [3]float32{0.5, 64, 0.5})
	target, _ := joinPartyPlayer(t, h, 2, "Sigrun", [3]float32{0.5, 64, 0.5})
	mustParty(t, inviter, vnet.PartyActionInvite, "  sIgRuN  ")
	if target.invite == nil || target.invite.from != inviter.entityID {
		t.Fatal("trimmed, folded target did not resolve to the live player")
	}
}

func TestInviteRulesAndReplacement(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	astrid, _ := joinPartyPlayer(t, h, 1, "Astrid", [3]float32{0.5, 64, 0.5})
	bjorn, _ := joinPartyPlayer(t, h, 2, "Bjorn", [3]float32{0.5, 64, 0.5})
	cora, _ := joinPartyPlayer(t, h, 3, "Cora", [3]float32{0.5, 64, 0.5})
	dead, _ := joinPartyPlayer(t, h, 4, "Dead", [3]float32{0.5, 64, 0.5})
	leaving, _ := joinPartyPlayer(t, h, 5, "Leaving", [3]float32{0.5, 64, 0.5})

	wantPartyRefusal(t, astrid, vnet.PartyActionInvite, "Astrid", vnet.RefusalReasonAlreadyInParty)
	wantPartyRefusal(t, astrid, vnet.PartyActionInvite, "Nobody", vnet.RefusalReasonNoSuchPlayer)
	leaving.BeginLeaving()
	wantPartyRefusal(t, astrid, vnet.PartyActionInvite, "Leaving", vnet.RefusalReasonNoSuchPlayer)

	h.hurt(dead, PlayerMaxHealth)
	mustParty(t, astrid, vnet.PartyActionInvite, "Dead")
	if dead.invite == nil {
		t.Fatal("a dead online target received no invitation")
	}

	mustParty(t, astrid, vnet.PartyActionInvite, "Cora")
	if got := cora.invite.from; got != astrid.entityID {
		t.Fatalf("first invitation is from %d, want %d", got, astrid.entityID)
	}
	mustParty(t, bjorn, vnet.PartyActionInvite, "Cora")
	if got := cora.invite.from; got != bjorn.entityID {
		t.Errorf("replacement invitation is from %d, want %d", got, bjorn.entityID)
	}
}

func TestDroppedInviteRemainsRetryable(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	inviter, _ := joinPartyPlayer(t, h, 1, "Astrid", [3]float32{0.5, 64, 0.5})
	target, out := joinPartyPlayer(t, h, 2, "Bjorn", [3]float32{0.5, 64, 0.5})
	out.setFull(true)
	mustParty(t, inviter, vnet.PartyActionInvite, "Bjorn")
	if target.invite == nil || len(partyInvites(t, out)) != 0 {
		t.Fatal("dropped delivery either lost the pending invite or entered the full queue")
	}

	out.setFull(false)
	mustParty(t, inviter, vnet.PartyActionInvite, "Bjorn")
	invites := partyInvites(t, out)
	if len(invites) != 1 {
		t.Fatalf("retry delivered %d invitations, want 1", len(invites))
	}
	want := protocol.PartyInvite{FromEntityID: 1, FromName: "Astrid", ExpiresMS: uint32(PartyInviteTTL.Milliseconds())}
	if invites[0] != want {
		t.Errorf("invitation = %+v, want %+v", invites[0], want)
	}
}

func TestAcceptBuildsAnOrderedBoundedParty(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	names := []string{"Astrid", "Bjorn", "Cora", "Dag", "Eira", "Finn"}
	players := make([]*Player, 0, len(names))
	for i, name := range names {
		player, _ := joinPartyPlayer(t, h, uint64(i+1), name, [3]float32{0.5, 64, 0.5})
		players = append(players, player)
	}

	inviteAndAccept(t, players[0], players[1], names[1])
	partyID := players[0].partyID
	if partyID == 0 || players[1].partyID != partyID {
		t.Fatal("accept did not create one shared party")
	}
	for index := 2; index < MaxPartySize; index++ {
		inviteAndAccept(t, players[0], players[index], names[index])
	}
	held := h.sim.parties[partyID]
	if held == nil || held.leader != players[0].partyMemberKey() || !slices.Equal(partyCharacterIDs(held), []uint64{1, 2, 3, 4, 5}) {
		t.Fatalf("party = %+v, want leader 1 and join order [1 2 3 4 5]", held)
	}
	wantPartyRefusal(t, players[1], vnet.PartyActionInvite, names[5], vnet.RefusalReasonNotLeader)
	wantPartyRefusal(t, players[0], vnet.PartyActionInvite, names[1], vnet.RefusalReasonAlreadyInParty)
	wantPartyRefusal(t, players[0], vnet.PartyActionInvite, names[5], vnet.RefusalReasonPartyFull)
}

func TestAcceptRechecksTheInviterAndAvailableRoom(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	first, _ := joinPartyPlayer(t, h, 1, "First", [3]float32{0.5, 64, 0.5})
	second, _ := joinPartyPlayer(t, h, 2, "Second", [3]float32{0.5, 64, 0.5})
	target, _ := joinPartyPlayer(t, h, 3, "Target", [3]float32{0.5, 64, 0.5})
	mustParty(t, first, vnet.PartyActionInvite, "Target")
	inviteAndAccept(t, second, first, "First")
	wantPartyRefusal(t, target, vnet.PartyActionAccept, "", vnet.RefusalReasonNoInvite)

	h2 := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	names := []string{"Leader", "One", "Two", "Three", "Pending", "Last"}
	players := make([]*Player, 0, len(names))
	for i, name := range names {
		player, _ := joinPartyPlayer(t, h2, uint64(i+11), name, [3]float32{0.5, 64, 0.5})
		players = append(players, player)
	}
	for index := 1; index <= 3; index++ {
		inviteAndAccept(t, players[0], players[index], names[index])
	}
	mustParty(t, players[0], vnet.PartyActionInvite, "Pending")
	inviteAndAccept(t, players[0], players[5], "Last")
	wantPartyRefusal(t, players[4], vnet.PartyActionAccept, "", vnet.RefusalReasonPartyFull)
	if players[4].invite == nil {
		t.Error("a full party discarded an otherwise live invitation that may be retried")
	}
}

func TestInviteExpiryUsesTheAuthoritativeTickBoundary(t *testing.T) {
	t.Parallel()

	for _, rate := range []uint8{1, 7, DefaultTickRate, 60} {
		t.Run(fmt.Sprint(rate, "Hz"), func(t *testing.T) {
			h := newVitalsHarness(t, rate, dropTerrain{groundTop: 63})
			inviter, _ := joinPartyPlayer(t, h, 1, "Astrid", [3]float32{0.5, 64, 0.5})
			target, _ := joinPartyPlayer(t, h, 2, "Bjorn", [3]float32{0.5, 64, 0.5})
			mustParty(t, inviter, vnet.PartyActionInvite, "Bjorn")
			expires := uint64(ticksFor(PartyInviteTTL, rate))
			h.sim.Step(expires - 1)
			if target.invite == nil {
				t.Fatal("invitation expired one tick early")
			}
			h.sim.Step(expires)
			if target.invite != nil {
				t.Fatal("invitation survived its expiry tick")
			}
			wantPartyRefusal(t, target, vnet.PartyActionAccept, "", vnet.RefusalReasonNoInvite)
		})
	}
}

func TestDeclineLeaderSuccessionKickAndDissolution(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	astrid, _ := joinPartyPlayer(t, h, 1, "Astrid", [3]float32{0.5, 64, 0.5})
	bjorn, _ := joinPartyPlayer(t, h, 2, "Bjorn", [3]float32{0.5, 64, 0.5})
	cora, _ := joinPartyPlayer(t, h, 3, "Cora", [3]float32{0.5, 64, 0.5})
	dag, _ := joinPartyPlayer(t, h, 4, "Dag", [3]float32{0.5, 64, 0.5})

	mustParty(t, astrid, vnet.PartyActionInvite, "Dag")
	mustParty(t, dag, vnet.PartyActionDecline, "")
	wantPartyRefusal(t, dag, vnet.PartyActionAccept, "", vnet.RefusalReasonNoInvite)

	inviteAndAccept(t, astrid, bjorn, "Bjorn")
	inviteAndAccept(t, astrid, cora, "Cora")
	partyID := astrid.partyID
	mustParty(t, astrid, vnet.PartyActionLeave, "")
	held := h.sim.parties[partyID]
	if held == nil || held.leader != bjorn.partyMemberKey() || !slices.Equal(partyCharacterIDs(held), []uint64{2, 3}) {
		t.Fatalf("after leader leave party = %+v, want leader 2 and members [2 3]", held)
	}
	wantPartyRefusal(t, cora, vnet.PartyActionKick, "Bjorn", vnet.RefusalReasonNotLeader)
	mustParty(t, bjorn, vnet.PartyActionKick, "Cora")
	if bjorn.partyID != 0 || cora.partyID != 0 || len(h.sim.parties) != 0 {
		t.Errorf("two-person party did not dissolve: ids %d/%d, registry size %d", bjorn.partyID, cora.partyID, len(h.sim.parties))
	}
}

func TestSimLeaveKeepsTheStableRosterAndLeaderOffline(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	astrid, _ := joinPartyPlayer(t, h, 1, "Astrid", [3]float32{0.5, 64, 0.5})
	bjorn, _ := joinPartyPlayer(t, h, 2, "Bjorn", [3]float32{0.5, 64, 0.5})
	cora, _ := joinPartyPlayer(t, h, 3, "Cora", [3]float32{0.5, 64, 0.5})
	dag, _ := joinPartyPlayer(t, h, 4, "Dag", [3]float32{0.5, 64, 0.5})
	inviteAndAccept(t, astrid, bjorn, "Bjorn")
	inviteAndAccept(t, astrid, cora, "Cora")
	mustParty(t, astrid, vnet.PartyActionInvite, "Dag")
	partyID := astrid.partyID

	h.sim.Leave(astrid)
	held := h.sim.parties[partyID]
	if held == nil || held.leader != astrid.partyMemberKey() || !slices.Equal(partyCharacterIDs(held), []uint64{1, 2, 3}) {
		t.Fatalf("party after disconnect = %+v, want offline leader and stable order [1 2 3]", held)
	}
	if held.members[0].player != nil {
		t.Fatal("disconnected leader retained a live player binding")
	}
	if _, found := h.sim.byName["astrid"]; found {
		t.Error("departed player remains in the name index")
	}
	if dag.invite != nil {
		t.Error("departed inviter left an actionable invitation behind")
	}
}

func TestReconnectRebindsExactlyTheSameCharacterAndRestartsItsGrace(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, 1, dropTerrain{groundTop: 63})
	leader, _ := joinPartyPlayer(t, h, 1, "Astrid", [3]float32{0.5, 64, 0.5})
	member, _ := joinPartyPlayer(t, h, 2, "Bjorn", [3]float32{0.5, 64, 0.5})
	third, _ := joinPartyPlayer(t, h, 3, "Cora", [3]float32{0.5, 64, 0.5})
	inviteAndAccept(t, leader, member, member.name)
	inviteAndAccept(t, leader, third, third.name)
	partyID := leader.partyID
	grace := uint64(ticksFor(PartyOfflineGrace, 1))

	h.sim.Leave(member)
	held := h.sim.parties[partyID]
	if held.members[1].player != nil || held.members[1].offlineUntilTick != grace {
		t.Fatalf("offline member = %+v, want no player and deadline %d", held.members[1], grace)
	}
	h.sim.Step(grace - 1)
	out := &dropSink{}
	rebound, err := h.sim.JoinCharacter(22, member.playerID, member.characterID, member.name,
		[3]float32{0.5, 64, 0.5}, testAppearance(), nil, out.deliver)
	if err != nil {
		t.Fatalf("reconnect: %v", err)
	}
	if rebound.partyID != partyID || held.members[1].player != rebound || held.members[1].offlineUntilTick != 0 {
		t.Fatalf("rebound member = %+v with party %d, want same slot in party %d", held.members[1], rebound.partyID, partyID)
	}

	// The old teardown is harmless after the new binding has landed, and the old
	// deadline cannot evict the reconnect on its boundary.
	h.sim.Leave(member)
	h.sim.Step(grace)
	if held.members[1].player != rebound || rebound.partyID != partyID {
		t.Fatal("an old teardown or deadline detached the new session")
	}

	// A later disconnect receives a whole new grace window from this tick.
	h.sim.Leave(rebound)
	secondDeadline := grace + grace
	if held.members[1].offlineUntilTick != secondDeadline {
		t.Fatalf("second deadline = %d, want %d", held.members[1].offlineUntilTick, secondDeadline)
	}
	h.sim.Step(secondDeadline - 1)
	if len(held.members) != 3 {
		t.Fatal("member expired one tick before the restarted deadline")
	}
	h.sim.Step(secondDeadline)
	if !slices.Equal(partyCharacterIDs(held), []uint64{1, 3}) {
		t.Fatalf("party after expiry = %v, want [1 3]", partyCharacterIDs(held))
	}
}

func TestAnotherCharacterOnTheSameAccountDoesNotInheritPartyMembership(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	leader, _ := joinPartyPlayer(t, h, 1, "Astrid", [3]float32{0.5, 64, 0.5})
	member, _ := joinPartyPlayer(t, h, 2, "Bjorn", [3]float32{0.5, 64, 0.5})
	inviteAndAccept(t, leader, member, member.name)
	partyID := leader.partyID
	h.sim.Leave(member)

	other, err := h.sim.JoinCharacter(20, member.playerID, 200, "Ivar", [3]float32{0.5, 64, 0.5},
		testAppearance(), nil, (&dropSink{}).deliver)
	if err != nil {
		t.Fatalf("joining another character: %v", err)
	}
	if other.partyID != 0 || h.sim.parties[partyID].members[1].player != nil {
		t.Fatal("another character inherited the offline character's party slot")
	}
}

func TestReconnectAtTheExpiryTickDoesNotRestoreAnExpiredMembership(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, 1, dropTerrain{groundTop: 63})
	leader, _ := joinPartyPlayer(t, h, 1, "Astrid", [3]float32{0.5, 64, 0.5})
	member, _ := joinPartyPlayer(t, h, 2, "Bjorn", [3]float32{0.5, 64, 0.5})
	third, _ := joinPartyPlayer(t, h, 3, "Cora", [3]float32{0.5, 64, 0.5})
	inviteAndAccept(t, leader, member, member.name)
	inviteAndAccept(t, leader, third, third.name)
	h.sim.Leave(member)
	h.sim.Step(uint64(ticksFor(PartyOfflineGrace, 1)))

	rejoined, err := h.sim.JoinCharacter(20, member.playerID, member.characterID, member.name,
		[3]float32{0.5, 64, 0.5}, testAppearance(), nil, (&dropSink{}).deliver)
	if err != nil {
		t.Fatalf("reconnect at deadline: %v", err)
	}
	if rejoined.partyID != 0 {
		t.Fatalf("reconnect at deadline restored expired party %d", rejoined.partyID)
	}
}

func TestExplicitLeaveAfterReconnectRemovesOnlyTheStableMember(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	leader, _ := joinPartyPlayer(t, h, 1, "Astrid", [3]float32{0.5, 64, 0.5})
	member, _ := joinPartyPlayer(t, h, 2, "Bjorn", [3]float32{0.5, 64, 0.5})
	third, _ := joinPartyPlayer(t, h, 3, "Cora", [3]float32{0.5, 64, 0.5})
	inviteAndAccept(t, leader, member, member.name)
	inviteAndAccept(t, leader, third, third.name)
	partyID := leader.partyID
	h.sim.Leave(member)
	rebound, err := h.sim.JoinCharacter(20, member.playerID, member.characterID, member.name,
		[3]float32{0.5, 64, 0.5}, testAppearance(), nil, (&dropSink{}).deliver)
	if err != nil {
		t.Fatalf("reconnect: %v", err)
	}

	wantPartyRefusal(t, member, vnet.PartyActionLeave, "", vnet.RefusalReasonNoSuchPlayer)
	if !slices.Equal(partyCharacterIDs(h.sim.parties[partyID]), []uint64{1, 2, 3}) {
		t.Fatal("a stale session mutated the rebound roster")
	}
	mustParty(t, rebound, vnet.PartyActionLeave, "")
	if rebound.partyID != 0 || !slices.Equal(partyCharacterIDs(h.sim.parties[partyID]), []uint64{1, 3}) {
		t.Fatalf("explicit leave produced party %d and roster %v, want solo member and [1 3]", rebound.partyID, partyCharacterIDs(h.sim.parties[partyID]))
	}
	if _, retained := h.sim.partyMemberships[member.partyMemberKey()]; retained {
		t.Fatal("explicit leave retained the stable membership index")
	}
}

func TestLeaderReconnectKeepsTheFirstRosterSlotAndLeadership(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	leader, _ := joinPartyPlayer(t, h, 1, "Astrid", [3]float32{0.5, 64, 0.5})
	member, _ := joinPartyPlayer(t, h, 2, "Bjorn", [3]float32{0.5, 64, 0.5})
	third, _ := joinPartyPlayer(t, h, 3, "Cora", [3]float32{0.5, 64, 0.5})
	inviteAndAccept(t, leader, member, member.name)
	inviteAndAccept(t, leader, third, third.name)
	partyID := leader.partyID
	h.sim.Leave(leader)
	if h.sim.parties[partyID].leader != leader.partyMemberKey() {
		t.Fatal("disconnect promoted a successor")
	}
	rebound, err := h.sim.JoinCharacter(10, leader.playerID, leader.characterID, leader.name,
		[3]float32{0.5, 64, 0.5}, testAppearance(), nil, (&dropSink{}).deliver)
	if err != nil {
		t.Fatalf("leader reconnect: %v", err)
	}
	leaderEntityID, _, roster := h.sim.partySnapshotLocked(member)
	if rebound.partyID != partyID || leaderEntityID != 10 || len(roster) != 3 || roster[0].EntityID != 10 || !roster[0].Online {
		t.Fatalf("leader reconnect produced party %d, leader %d, roster %+v", rebound.partyID, leaderEntityID, roster)
	}
}

func TestOfflineExpiryPromotesInOrderAndCleansAnAllOfflineParty(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, 1, dropTerrain{groundTop: 63})
	leader, _ := joinPartyPlayer(t, h, 1, "Astrid", [3]float32{0.5, 64, 0.5})
	second, _ := joinPartyPlayer(t, h, 2, "Bjorn", [3]float32{0.5, 64, 0.5})
	third, _ := joinPartyPlayer(t, h, 3, "Cora", [3]float32{0.5, 64, 0.5})
	inviteAndAccept(t, leader, second, second.name)
	inviteAndAccept(t, leader, third, third.name)
	partyID := leader.partyID
	grace := uint64(ticksFor(PartyOfflineGrace, 1))

	h.sim.Leave(leader)
	h.sim.Step(1)
	h.sim.Leave(second)
	h.sim.Step(grace - 1)
	held := h.sim.parties[partyID]
	if held.leader != leader.partyMemberKey() || len(held.members) != 3 {
		t.Fatal("offline leader was promoted or removed before its deadline")
	}
	h.sim.Step(grace)
	if held.leader != second.partyMemberKey() || !slices.Equal(partyCharacterIDs(held), []uint64{2, 3}) || held.members[0].player != nil {
		t.Fatalf("leader expiry produced %+v, want offline second member leading [2 3]", held)
	}

	// Once the promoted member reaches its own later deadline, the legacy two-member
	// dissolution rule removes the final solo membership too.
	h.sim.Leave(third)
	h.sim.Step(grace + 1)
	if h.sim.parties[partyID] != nil || len(h.sim.partyMemberships) != 0 || third.partyID != 0 {
		t.Fatal("all-offline expiry left a party, membership index, or solo binding behind")
	}
}

func TestAnAllOfflinePartyWithOneDeadlineIsCompletelyRemovedAtThatTick(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, 1, dropTerrain{groundTop: 63})
	leader, _ := joinPartyPlayer(t, h, 1, "Astrid", [3]float32{0.5, 64, 0.5})
	second, _ := joinPartyPlayer(t, h, 2, "Bjorn", [3]float32{0.5, 64, 0.5})
	third, _ := joinPartyPlayer(t, h, 3, "Cora", [3]float32{0.5, 64, 0.5})
	inviteAndAccept(t, leader, second, second.name)
	inviteAndAccept(t, leader, third, third.name)
	partyID := leader.partyID
	h.sim.Leave(leader)
	h.sim.Leave(second)
	h.sim.Leave(third)
	deadline := uint64(ticksFor(PartyOfflineGrace, 1))
	h.sim.Step(deadline - 1)
	if held := h.sim.parties[partyID]; held == nil || len(held.members) != 3 {
		t.Fatal("all-offline party disappeared before the shared deadline")
	}
	rebound, err := h.sim.JoinCharacter(20, second.playerID, second.characterID, second.name,
		[3]float32{0.5, 64, 0.5}, testAppearance(), nil, (&dropSink{}).deliver)
	if err != nil {
		t.Fatalf("reconstructing all-offline party: %v", err)
	}
	held := h.sim.parties[partyID]
	if rebound.partyID != partyID || held.members[1].player != rebound || held.members[1].offlineUntilTick != 0 ||
		!slices.Equal(partyCharacterIDs(held), []uint64{1, 2, 3}) {
		t.Fatalf("reconstructed party = %+v with rebound party %d", held, rebound.partyID)
	}
	h.sim.Step(deadline)
	if h.sim.parties[partyID] != nil || len(h.sim.partyMemberships) != 0 || rebound.partyID != 0 {
		t.Fatal("all-offline party or membership index survived the shared deadline")
	}
}

func TestOfflineMembersStillCountAndCanBeKicked(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	names := []string{"Astrid", "Bjorn", "Cora", "Dag", "Eira", "Finn"}
	players := make([]*Player, 0, len(names))
	for index, name := range names {
		player, _ := joinPartyPlayer(t, h, uint64(index+1), name, [3]float32{0.5, 64, 0.5})
		players = append(players, player)
	}
	for index := 1; index < MaxPartySize; index++ {
		inviteAndAccept(t, players[0], players[index], names[index])
	}
	h.sim.Leave(players[4])
	wantPartyRefusal(t, players[0], vnet.PartyActionInvite, names[5], vnet.RefusalReasonPartyFull)
	wantPartyRefusal(t, players[1], vnet.PartyActionKick, names[4], vnet.RefusalReasonNotLeader)
	mustParty(t, players[0], vnet.PartyActionKick, names[4])
	if _, retained := h.sim.partyMemberships[players[4].partyMemberKey()]; retained {
		t.Fatal("leader kick retained the offline member index")
	}
}

func TestExplicitLeaderLeavePromotesTheOfflineSecondMember(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	leader, _ := joinPartyPlayer(t, h, 1, "Astrid", [3]float32{0.5, 64, 0.5})
	second, _ := joinPartyPlayer(t, h, 2, "Bjorn", [3]float32{0.5, 64, 0.5})
	third, _ := joinPartyPlayer(t, h, 3, "Cora", [3]float32{0.5, 64, 0.5})
	inviteAndAccept(t, leader, second, second.name)
	inviteAndAccept(t, leader, third, third.name)
	partyID := leader.partyID
	h.sim.Leave(second)
	mustParty(t, leader, vnet.PartyActionLeave, "")

	held := h.sim.parties[partyID]
	if held == nil || held.leader != second.partyMemberKey() || !slices.Equal(partyCharacterIDs(held), []uint64{2, 3}) {
		t.Fatalf("party after explicit leader leave = %+v, want offline second member leading [2 3]", held)
	}
	leaderID, _, roster := h.sim.partySnapshotLocked(third)
	if leaderID != 0 || len(roster) != 2 || roster[0].Online {
		t.Fatalf("snapshot leader/roster = %d/%+v, want offline promoted leader first", leaderID, roster)
	}
}

func partyCharacterIDs(held *party) []uint64 {
	ids := make([]uint64, 0, len(held.members))
	for _, member := range held.members {
		ids = append(ids, member.key.characterID)
	}
	return ids
}

func TestPartySnapshotCarriesEveryOtherMemberRegardlessOfView(t *testing.T) {
	t.Parallel()

	h := newVitalsHarnessAt(t, DefaultTickRate, dropTerrain{groundTop: 63}, 0)
	astrid, astridOut := joinPartyPlayer(t, h, 1, "Astrid", [3]float32{0.5, 64, 0.5})
	bjorn, bjornOut := joinPartyPlayer(t, h, 2, "Bjorn", [3]float32{32.5, 64, 0.5})
	cora, coraOut := joinPartyPlayer(t, h, 3, "Cora", [3]float32{64.5, 64, 0.5})
	solo, soloOut := joinPartyPlayer(t, h, 4, "Dag", [3]float32{96.5, 64, 0.5})
	inviteAndAccept(t, astrid, bjorn, "Bjorn")
	inviteAndAccept(t, astrid, cora, "Cora")
	h.step()

	for _, tc := range []struct {
		viewer *Player
		out    *dropSink
		want   []uint64
	}{
		{astrid, astridOut, []uint64{2, 3}},
		{bjorn, bjornOut, []uint64{1, 3}},
		{cora, coraOut, []uint64{1, 2}},
	} {
		snapshot := newestSnapshot(t, tc.out)
		if got := snapshot.PartyLeaderEntityId(); got != astrid.entityID {
			t.Errorf("viewer %d leader = %d, want %d", tc.viewer.entityID, got, astrid.entityID)
		}
		if got := snapshotPartyIDs(t, snapshot); !slices.Equal(got, tc.want) {
			t.Errorf("viewer %d members = %v, want %v", tc.viewer.entityID, got, tc.want)
		}
		if got := snapshotRosterIDs(t, snapshot); !slices.Equal(got, []uint64{1, 2, 3}) {
			t.Errorf("viewer %d roster = %v, want stable order [1 2 3]", tc.viewer.entityID, got)
		}
	}
	if snapshot := newestSnapshot(t, soloOut); snapshot.PartyLeaderEntityId() != 0 || snapshot.PartyMembersLength() != 0 {
		t.Errorf("solo viewer %d received party state", solo.entityID)
	}
}

func TestCorpseRoundRobinIsLeaderFirstAndWraps(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	leader, _ := joinPartyPlayer(t, h, 1, "Astrid", [3]float32{0.5, 64, 0.5})
	second, _ := joinPartyPlayer(t, h, 2, "Bjorn", [3]float32{0.5, 64, 0.5})
	third, _ := joinPartyPlayer(t, h, 3, "Cora", [3]float32{0.5, 64, 0.5})
	inviteAndAccept(t, leader, second, second.name)
	inviteAndAccept(t, leader, third, third.name)

	tap := newMobTap(leader)
	h.sim.mu.Lock()
	got := []corpseOwner{
		h.sim.corpseOwnerLocked(tap, leader.pos),
		h.sim.corpseOwnerLocked(tap, leader.pos),
		h.sim.corpseOwnerLocked(tap, leader.pos),
		h.sim.corpseOwnerLocked(tap, leader.pos),
	}
	h.sim.mu.Unlock()
	want := []corpseOwner{leader.corpseOwner(), second.corpseOwner(), third.corpseOwner(), leader.corpseOwner()}
	if !slices.Equal(got, want) {
		t.Fatalf("round robin = %+v, want leader-first wrap %+v", got, want)
	}
}

func TestCorpseRoundRobinSkipsOnlyIneligibleRosterSlots(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	leader, _ := joinPartyPlayer(t, h, 1, "Astrid", [3]float32{0.5, 64, 0.5})
	offline, _ := joinPartyPlayer(t, h, 2, "Bjorn", [3]float32{0.5, 64, 0.5})
	far, _ := joinPartyPlayer(t, h, 3, "Cora", [3]float32{float32(PartyShareRadius + 2), 64, 0.5})
	dead, _ := joinPartyPlayer(t, h, 4, "Dag", [3]float32{0.5, 64, 0.5})
	inviteAndAccept(t, leader, offline, offline.name)
	inviteAndAccept(t, leader, far, far.name)
	inviteAndAccept(t, leader, dead, dead.name)
	h.sim.Leave(offline)
	h.hurt(dead, PlayerMaxHealth)
	deathPos := [3]float64{0.5, 64, 0.5}

	h.sim.mu.Lock()
	held := h.sim.parties[leader.partyID]
	held.lootCursor = offline.partyMemberKey()
	owner := h.sim.corpseOwnerLocked(newMobTap(leader), deathPos)
	cursor := held.lootCursor
	h.sim.mu.Unlock()
	if owner != dead.corpseOwner() {
		t.Fatalf("offline and out-of-range slots were not skipped, or dead online slot was: owner %+v", owner)
	}
	if cursor != leader.partyMemberKey() {
		t.Fatalf("cursor after dead member = %+v, want wrapped leader", cursor)
	}
	h.sim.Leave(dead)
	h.sim.mu.Lock()
	leader.pos = [3]float64{PartyShareRadius + 2, 64, 0.5}
	fallback := h.sim.corpseOwnerLocked(newMobTap(leader), deathPos)
	h.sim.mu.Unlock()
	if fallback != leader.corpseOwner() {
		t.Fatalf("no eligible roster member chose %+v, want first-tap fallback %+v", fallback, leader.corpseOwner())
	}
}

func TestRemovingCursorPreservesCyclicOrderAndAssignmentIsFrozen(t *testing.T) {
	t.Parallel()
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	leader, _ := joinPartyPlayer(t, h, 1, "Astrid", [3]float32{0.5, 64, 0.5})
	second, _ := joinPartyPlayer(t, h, 2, "Bjorn", [3]float32{0.5, 64, 0.5})
	third, _ := joinPartyPlayer(t, h, 3, "Cora", [3]float32{0.5, 64, 0.5})
	inviteAndAccept(t, leader, second, second.name)
	inviteAndAccept(t, leader, third, third.name)

	h.sim.mu.Lock()
	held := h.sim.parties[leader.partyID]
	held.lootCursor = second.partyMemberKey()
	h.sim.removePartyMemberLocked(leader.partyID, second.partyMemberKey())
	owner := h.sim.corpseOwnerLocked(newMobTap(leader), leader.pos)
	c := &corpse{entityID: 99, owner: owner, kind: vnet.MobKindVargr, entries: []corpseEntry{{entryID: 1, stack: stackOf(ItemVargrPelt, 1)}}}
	h.sim.corpses[c.entityID] = c
	h.sim.removePartyMemberLocked(leader.partyID, third.partyMemberKey())
	h.sim.mu.Unlock()
	if owner != third.corpseOwner() {
		t.Fatalf("removing cursor chose %+v, want old cyclic successor %+v", owner, third.corpseOwner())
	}
	if !c.ownedBy(third) || c.ownedBy(leader) {
		t.Error("party mutation after assignment changed corpse ownership")
	}
}

func snapshotRosterIDs(t *testing.T, snapshot *vnet.EntitySnapshot) []uint64 {
	t.Helper()
	ids := make([]uint64, 0, snapshot.PartyRosterLength())
	for index := range snapshot.PartyRosterLength() {
		member := new(vnet.PartyRosterMember)
		if !snapshot.PartyRoster(member, index) {
			t.Fatalf("party roster member %d is absent", index)
		}
		ids = append(ids, member.CharacterId())
	}
	return ids
}

func snapshotPartyIDs(t *testing.T, snapshot *vnet.EntitySnapshot) []uint64 {
	t.Helper()
	ids := make([]uint64, 0, snapshot.PartyMembersLength())
	for i := range snapshot.PartyMembersLength() {
		var member vnet.PartyMemberState
		if !snapshot.PartyMembers(&member, i) {
			t.Fatalf("party member %d is absent", i)
		}
		ids = append(ids, member.EntityId())
	}
	return ids
}

func partyInvites(t *testing.T, out *dropSink) []protocol.PartyInvite {
	t.Helper()
	var invites []protocol.PartyInvite
	for _, frame := range out.all() {
		envelope := vnet.GetRootAsEnvelope(frame, 0)
		if envelope.PayloadType() != vnet.PayloadPartyInvite {
			continue
		}
		var payload flatbuffers.Table
		if !envelope.Payload(&payload) {
			t.Fatal("PartyInvite envelope has no payload")
		}
		var invite vnet.PartyInvite
		invite.Init(payload.Bytes, payload.Pos)
		invites = append(invites, protocol.PartyInvite{
			FromEntityID: invite.FromEntityId(), FromName: string(invite.FromName()), ExpiresMS: invite.ExpiresMs(),
		})
	}
	return invites
}
