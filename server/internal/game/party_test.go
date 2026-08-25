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
	if held == nil || held.leader != players[0].entityID || !slices.Equal(held.members, []uint64{1, 2, 3, 4, 5}) {
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
	if held == nil || held.leader != bjorn.entityID || !slices.Equal(held.members, []uint64{2, 3}) {
		t.Fatalf("after leader leave party = %+v, want leader 2 and members [2 3]", held)
	}
	wantPartyRefusal(t, cora, vnet.PartyActionKick, "Bjorn", vnet.RefusalReasonNotLeader)
	mustParty(t, bjorn, vnet.PartyActionKick, "Cora")
	if bjorn.partyID != 0 || cora.partyID != 0 || len(h.sim.parties) != 0 {
		t.Errorf("two-person party did not dissolve: ids %d/%d, registry size %d", bjorn.partyID, cora.partyID, len(h.sim.parties))
	}
}

func TestSimLeaveMaintainsPartyAndNameIndexes(t *testing.T) {
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
	if held == nil || held.leader != bjorn.entityID || !slices.Equal(held.members, []uint64{2, 3}) {
		t.Fatalf("party after disconnect = %+v, want leader 2 and members [2 3]", held)
	}
	if _, found := h.sim.byName["astrid"]; found {
		t.Error("departed player remains in the name index")
	}
	if dag.invite != nil {
		t.Error("departed inviter left an actionable invitation behind")
	}
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
	}
	if snapshot := newestSnapshot(t, soloOut); snapshot.PartyLeaderEntityId() != 0 || snapshot.PartyMembersLength() != 0 {
		t.Errorf("solo viewer %d received party state", solo.entityID)
	}
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
