package game_test

import (
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// What a player looks like is sent once, when they enter a session's view, and is not
// part of a snapshot. Everything in this file is about that "once": what makes it
// happen, what makes it happen again, and what stops it happening every tick.
//
// The rule it all rests on is that a snapshot is the **complete existence set** for the
// session it is addressed to. An entity that stops appearing in one has been despawned
// by the client, which takes its appearance with it — so "sent once" has to mean once
// per time the entity is in view, not once per session, or a player who walks out of a
// cube and back comes back grey for ever.

func TestAnAppearanceIsSentOnceWhenAPlayerComesIntoView(t *testing.T) {
	t.Parallel()

	h := newHarness(t, flatWorld{groundTop: 63})
	_, watcherOut := h.join(1, [3]float32{0.5, 67, 0.5})
	h.join(2, [3]float32{1.5, 67, 0.5})

	h.step()

	// Its own included: a session recognises itself by ServerWelcome.entity_id, and its
	// appearance arrives the same way as everybody else's.
	for _, entityID := range []uint64{1, 2} {
		if got := watcherOut.describedAs(t, entityID); got != 1 {
			t.Errorf("entity %d was described %d times on the tick it came into view, want 1", entityID, got)
		}
	}

	h.advance(4)

	for _, entityID := range []uint64{1, 2} {
		if got := watcherOut.describedAs(t, entityID); got != 1 {
			t.Errorf("entity %d was described %d times over five ticks, want 1 — an appearance is not part of a snapshot", entityID, got)
		}
	}
	if got := watcherOut.snapshots(); got != 5 {
		t.Errorf("five ticks delivered %d snapshots, want 5", got)
	}
}

func TestTheDescriptionSentIsTheOneTheCharacterJoinedWith(t *testing.T) {
	t.Parallel()

	h := newHarness(t, flatWorld{groundTop: 63})
	_, out := h.join(1, [3]float32{0.5, 67, 0.5})

	h.step()

	sent := out.appearances(t)
	if len(sent) != 1 {
		t.Fatalf("the session was sent %d appearances, want 1", len(sent))
	}
	if !sent[0].HasAppearance {
		t.Fatal("the appearance table was omitted; a client may not invent one")
	}
	if got := sent[0].Appearance; got != testAppearance() {
		t.Errorf("the appearance sent is %+v, want the one the character joined with, %+v", got, testAppearance())
	}
	if !sent[0].HasName {
		t.Fatal("the name was omitted; a client may not invent one")
	}
	if got := sent[0].Name; got != testCharacterName {
		t.Errorf("the name sent is %q, want the stored character name %q", got, testCharacterName)
	}
	if got := sent[0].Level; got != 1 {
		t.Errorf("a new character's displayed level is %d, want 1", got)
	}
}

// Level is derived from the stored lifetime total when the appearance is built; there
// is no second persisted level that can disagree with it.
func TestTheDescriptionDerivesLevelFromLifetimeExperience(t *testing.T) {
	t.Parallel()

	h := newHarness(t, flatWorld{groundTop: 63})
	out := &sink{}
	life := game.Life{
		Pos: [3]float64{0.5, 67, 0.5}, Health: game.PlayerMaxHealth,
		Hunger: game.PlayerMaxHunger, Experience: 50,
	}
	if _, err := h.sim.Join(1, testPlayerID(1), testCharacterName, [3]float32{0.5, 67, 0.5}, testAppearance(), &life, out.deliver); err != nil {
		t.Fatalf("Join: %v", err)
	}
	h.step()

	sent := out.appearances(t)
	if len(sent) != 1 {
		t.Fatalf("the session was sent %d appearances, want 1", len(sent))
	}
	if got := sent[0].Level; got != 2 {
		t.Errorf("appearance level = %d, want 2 from 50 lifetime experience", got)
	}
}

// A queue with no room loses the frame, and nothing else replaces it: an appearance is
// not superseded the way a snapshot is. So it is recorded as sent only when the send
// succeeded, which makes the next tick try again for as long as the entity is in view.
func TestAnAppearanceDroppedByAFullQueueIsSentAgain(t *testing.T) {
	t.Parallel()

	h := newHarness(t, flatWorld{groundTop: 63})
	h.join(1, [3]float32{0.5, 67, 0.5})
	_, out := h.join(2, [3]float32{1.5, 67, 0.5})

	out.mu.Lock()
	out.full = true
	out.mu.Unlock()

	h.step()
	if got := len(out.appearances(t)); got != 0 {
		t.Fatalf("a full queue accepted %d appearances", got)
	}

	out.mu.Lock()
	out.full = false
	out.mu.Unlock()

	h.step()
	for _, entityID := range []uint64{1, 2} {
		if got := out.describedAs(t, entityID); got != 1 {
			t.Errorf("entity %d was described %d times once the queue drained, want 1", entityID, got)
		}
	}
}

// Leaving view is what makes the client forget a face, so coming back has to bring
// another one. View distance zero is what makes that a short walk rather than a
// journey: "in view" is then "in the same chunk", and the players stand half a block
// from the boundary.
func TestAPlayerThatLeavesViewIsDescribedAgainWhenItComesBack(t *testing.T) {
	t.Parallel()

	h := newHarnessAt(t, flatWorld{groundTop: 63}, game.DefaultTickRate, 0)
	_, watcherOut := h.join(1, [3]float32{0.5, 67, 0.5})
	walker, _ := h.join(2, [3]float32{0.5, 67, 0.5})

	h.step()
	if got := watcherOut.describedAs(t, 2); got != 1 {
		t.Fatalf("the walker was described %d times while standing in the watcher's chunk, want 1", got)
	}

	// North is -Z, so a few ticks of walking crosses into the chunk below the origin.
	h.hold(walker, walking(yawNorth), 20)
	if _, states := decodeSnapshot(t, watcherOut.last()); len(states) != 1 {
		t.Fatalf("the watcher still sees %d entities after the walker left its chunk, want only itself", len(states))
	}
	if got := watcherOut.describedAs(t, 2); got != 1 {
		t.Fatalf("the walker was described %d times while out of view, want the original 1", got)
	}

	h.hold(walker, walking(yawSouth), 40)
	if _, states := decodeSnapshot(t, watcherOut.last()); len(states) != 2 {
		t.Fatalf("the watcher sees %d entities after the walker came back, want 2", len(states))
	}
	if got := watcherOut.describedAs(t, 2); got != 2 {
		t.Errorf("the walker was described %d times in all, want 2 — a client that despawned the entity has forgotten the face with it", got)
	}
}

// A reconnect is a new session, and a new session has told nobody anything. The set of
// entities already described belongs to the Player the simulation builds at Join, so
// this needs no rule of its own — which is exactly what the test is for.
func TestANewSessionIsToldEveryAppearanceAgain(t *testing.T) {
	t.Parallel()

	h := newHarness(t, flatWorld{groundTop: 63})
	first, firstOut := h.join(1, [3]float32{0.5, 67, 0.5})
	h.join(2, [3]float32{1.5, 67, 0.5})

	h.step()
	if got := firstOut.describedAs(t, 2); got != 1 {
		t.Fatalf("the first session was told about entity 2 %d times, want 1", got)
	}

	// The same person, back on a new connection: a session that ends takes its entity
	// id with it, and the one that replaces it starts knowing nothing.
	h.sim.Leave(first)
	_, secondOut := h.join(3, [3]float32{0.5, 67, 0.5})

	h.step()
	for _, entityID := range []uint64{2, 3} {
		if got := secondOut.describedAs(t, entityID); got != 1 {
			t.Errorf("the new session was told about entity %d %d times, want 1", entityID, got)
		}
	}
}

func TestOutOfViewPartyMatesAreDescribedAndForgottenOnLeave(t *testing.T) {
	t.Parallel()

	h := newHarnessAt(t, flatWorld{groundTop: 63}, game.DefaultTickRate, 0)
	leaderOut, memberOut := &sink{}, &sink{}
	leader, err := h.sim.Join(1, testPlayerID(1), "Astrid", [3]float32{0.5, 67, 0.5}, testAppearance(), nil, leaderOut.deliver)
	if err != nil {
		t.Fatalf("Join leader: %v", err)
	}
	member, err := h.sim.Join(2, testPlayerID(2), "Bjorn", [3]float32{32.5, 67, 0.5}, testAppearance(), nil, memberOut.deliver)
	if err != nil {
		t.Fatalf("Join member: %v", err)
	}

	if reason, err := leader.Party(vnet.PartyActionInvite, "Bjorn"); err != nil || reason != vnet.RefusalReasonUnknown {
		t.Fatalf("Invite = %s, %v", reason, err)
	}
	if reason, err := member.Party(vnet.PartyActionAccept, ""); err != nil || reason != vnet.RefusalReasonUnknown {
		t.Fatalf("Accept = %s, %v", reason, err)
	}
	h.step()
	if got := leaderOut.describedAs(t, member.EntityID()); got != 1 {
		t.Errorf("leader received %d out-of-view member descriptions, want 1", got)
	}
	if got := memberOut.describedAs(t, leader.EntityID()); got != 1 {
		t.Errorf("member received %d out-of-view leader descriptions, want 1", got)
	}

	if reason, err := member.Party(vnet.PartyActionLeave, ""); err != nil || reason != vnet.RefusalReasonUnknown {
		t.Fatalf("Leave = %s, %v", reason, err)
	}
	h.step()
	if got := leaderOut.describedAs(t, member.EntityID()); got != 1 {
		t.Errorf("leaving the party produced an extra out-of-view description: %d", got)
	}

	if reason, err := leader.Party(vnet.PartyActionInvite, "Bjorn"); err != nil || reason != vnet.RefusalReasonUnknown {
		t.Fatalf("second Invite = %s, %v", reason, err)
	}
	if reason, err := member.Party(vnet.PartyActionAccept, ""); err != nil || reason != vnet.RefusalReasonUnknown {
		t.Fatalf("second Accept = %s, %v", reason, err)
	}
	h.step()
	if got := leaderOut.describedAs(t, member.EntityID()); got != 2 {
		t.Errorf("re-joining produced %d total descriptions, want 2 — the former party link was not forgotten", got)
	}
}

// The appearance reaching Join has come off a disk, and from Join it goes out on a wire
// where a client is required to refuse anything the contract forbids. So it is checked
// at that boundary, exactly as a stored life is — the alternative is a session that
// disconnects everybody who can see it.
func TestJoinRefusesAnAppearanceTheContractForbids(t *testing.T) {
	t.Parallel()

	h := newHarness(t, flatWorld{groundTop: 63})
	deliver := func([]byte) bool { return true }

	forbidden := map[string]protocol.Appearance{
		// The zero value, which is what a caller that forgot the argument would hand in:
		// black is a colour somebody may choose, but HairModel.Unknown is the
		// absent-field value rather than a choice.
		"an unknown hair model": {},
		"a hair model no member names": {
			HairModel: vnet.HairModel(200),
		},
		"a colour carrying the reserved high byte": {
			SkinColor: 0xFF000000,
			HairModel: vnet.HairModelShaved,
		},
	}
	for name, appearance := range forbidden {
		t.Run(name, func(t *testing.T) {
			if _, err := h.sim.Join(9, testPlayerID(9), testCharacterName, [3]float32{0.5, 67, 0.5}, appearance, nil, deliver); err == nil {
				t.Error("Join admitted a character wearing it")
			}
		})
	}
}
