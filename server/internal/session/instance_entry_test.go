package session_test

import (
	"context"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
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

// A crossing test needs three things this package cannot otherwise have together: a run
// that is already cleared, a character the manager knows by name before that character
// connects, and two connections that can form a party. This block is those three.
//
// **`RestoreSessions` rather than a boss kill**, and that is not a shortcut around the
// trigger. A restored run is exactly the state a server comes up in after somebody
// cleared a dungeon yesterday; it is reached through exported API; and it is the only way
// this package can produce a saved session at all, because the kill path is internal to
// `Sim`. What it buys is the four entry cases exercised with real frames on a real
// connection, which is the half of #978 that lives in this file. It has to happen before
// anything crosses: a restore is refused into a manager already running a session.
//
// **The character is minted before it is played**, because the manager files a binding
// under `{PlayerID, CharacterID}` and an ephemeral store mints character ids at random —
// so a restore written against a guessed id would silently bind nobody, and every
// assertion below would pass for the wrong reason. Creating the character out of band and
// releasing the claim gives the test the exact identity the connection will present.

// portalPlayer is one character this test has minted but not yet played.
type portalPlayer struct {
	name      string
	seed      byte
	character game.InstanceCharacter
}

// mintPortalCharacter creates one character in a shared identity store and hands back the
// identity the instance manager will file it under. The account claim is released, so the
// connection that plays it is admitted normally.
func mintPortalCharacter(t *testing.T, identities *session.Identities, name string, seed byte) portalPlayer {
	t.Helper()
	admitted, err := identities.Admit(helloAsking(t, name, testTicket(testAccount(seed))))
	if err != nil {
		t.Fatalf("admit %s: %v", name, err)
	}
	resolved, err := identities.Create(admitted, name, testAppearance())
	if err != nil {
		t.Fatalf("create %s: %v", name, err)
	}
	identities.Release(admitted.ID)
	return portalPlayer{name: name, seed: seed, character: game.InstanceCharacter{
		PlayerID: resolved.ID, CharacterID: uint64(resolved.Character),
	}}
}

type portalConn struct {
	conn   *fakeConn
	frames *collector
	done   chan error
}

// playPortalCharacter connects one already-minted character and waits for it to enter the
// world. It mirrors party_test's start: a shared identity store is what lets two
// connections meet, and what let the character be minted before either existed.
func playPortalCharacter(t *testing.T, cfg session.Config, chunks *world.Cache, sim *game.Sim, peers *session.Registry,
	identities *session.Identities, entityID uint64, who portalPlayer, expectedLive int) portalConn {
	t.Helper()
	conn := newFakeConn()
	done := make(chan error, 1)
	go func() {
		done <- session.Serve(context.Background(), conn, cfg, noTimeouts(), chunks, sim, peers, identities, entityID, discard())
	}()
	conn.in <- helloNamed(who.name, who.seed)
	chooseCharacter(t, conn, who.name)
	_ = nextFrameOfKind(t, conn, vnet.PayloadServerWelcome)
	frames := collect(t, conn)
	waitUntil(t, "the character to enter the simulation", func() bool { return sim.Count() == expectedLive })
	live := portalConn{conn: conn, frames: frames, done: done}
	t.Cleanup(func() { _ = conn.Close(); <-done })
	return live
}

// savedRun is one cleared run of the ruin this request names, owed by one character.
func savedRun(request protocol.PortalRequest, id uint64, seed int64, owner game.InstanceCharacter) game.SavedSession {
	return game.SavedSession{
		ID:   id,
		Seed: seed,
		Ruin: game.InstanceRuin{
			CellX: world.RuinCellOf(int64(request.Arch[0])),
			CellZ: world.RuinCellOf(int64(request.Arch[2])),
		},
		ExpiresUnix:    time.Now().Add(6 * time.Hour).Unix(),
		DefeatedBosses: []vnet.MobKind{vnet.MobKindVargrGuardian},
		Bound:          []game.InstanceCharacter{owner},
	}
}

func restoreRuns(t *testing.T, cfg session.Config, runs ...game.SavedSession) {
	t.Helper()
	restored, expired, err := cfg.Instances.RestoreSessions(runs)
	if err != nil {
		t.Fatalf("RestoreSessions: %v", err)
	}
	if restored != len(runs) || expired != 0 {
		t.Fatalf("restored %d of %d runs, %d expired", restored, len(runs), expired)
	}
}

// Case 5: being bound is the ordinary state, and it produces no prompt on the wire.
func TestABoundCharacterCrossesTheirOwnRunWithNoPrompt(t *testing.T) {
	cfg, chunks, open, peers, request := portalSession(t, 3)
	identities := ephemeralIdentities()
	owner := mintPortalCharacter(t, identities, "Astrid", 1)
	restoreRuns(t, cfg, savedRun(request, 7001, 0x51EED, owner.character))
	live := playPortalCharacter(t, cfg, chunks, open, peers, identities, 1, owner, 1)

	live.conn.in <- protocol.EncodePortalRequest(request)
	waitUntil(t, "world change", func() bool { return len(live.frames.transitions()) == 1 })
	if got := live.frames.transitions()[0].WorldID; got != 7001 {
		t.Fatalf("crossed into world %d, want the run this character owes (7001)", got)
	}
	if offers := live.frames.crossingOffers(); len(offers) != 0 {
		t.Fatalf("a character was asked to accept the run they already owe: %+v", offers)
	}
	if refusals := live.frames.actionRefusals(); len(refusals) != 0 {
		t.Fatalf("crossing into an owed run was refused: %+v", refusals)
	}
	if chats := live.frames.chatMessages(); len(chats) != 0 {
		t.Fatalf("crossing into an owed run was explained away: %+v", chats)
	}
}

// party puts two live connections in one party, which is what makes a friend's run the
// copy the second of them reaches.
func party(t *testing.T, sim *game.Sim, leader, member portalConn, memberName string) {
	t.Helper()
	leader.conn.in <- protocol.EncodePartyRequest(protocol.PartyRequest{Action: vnet.PartyActionInvite, TargetName: memberName})
	waitUntil(t, "the party invitation", func() bool { return len(member.frames.partyInvites()) == 1 })
	member.conn.in <- protocol.EncodePartyRequest(protocol.PartyRequest{Action: vnet.PartyActionAccept})
	// The roster rides a snapshot, and these tests do not otherwise tick. Stepping until
	// it arrives is what makes "the party exists" an observation rather than a hope: the
	// portal's copy selection reads the party id, so a test that crossed before the
	// accept landed would be testing the solo path under a party's name.
	var tick uint64
	waitUntil(t, "the party roster", func() bool {
		tick++
		sim.Step(tick)
		return len(leader.frames.rosterState()) == 2
	})
}

// halfClearedParty is the situation the whole feature is about: a friend has already put
// this dungeon's first boss down, and somebody who owes it nothing walks up behind them.
func halfClearedParty(t *testing.T) (session.Config, protocol.PortalRequest, portalConn, portalConn, game.InstanceCharacter) {
	t.Helper()
	cfg, chunks, open, peers, request := portalSession(t, 4)
	identities := ephemeralIdentities()
	owner := mintPortalCharacter(t, identities, "Astrid", 1)
	friend := mintPortalCharacter(t, identities, "Bjorn", 2)
	restoreRuns(t, cfg, savedRun(request, 7001, 0x51EED, owner.character))

	live := playPortalCharacter(t, cfg, chunks, open, peers, identities, 1, owner, 1)
	other := playPortalCharacter(t, cfg, chunks, open, peers, identities, 2, friend, 2)
	party(t, open, live, other, "Bjorn")

	// The owner goes in first, which is what points the party's route at their run.
	live.conn.in <- protocol.EncodePortalRequest(request)
	waitUntil(t, "the owner to cross", func() bool { return len(live.frames.transitions()) == 1 })
	if got := live.frames.transitions()[0].WorldID; got != 7001 {
		t.Fatalf("the owner crossed into %d, not their own run", got)
	}
	return cfg, request, live, other, friend.character
}

// Case 2, on the wire: the prompt is a server-authored offer, and nothing has happened yet.
func TestASavedRunIsOfferedBeforeItBinds(t *testing.T) {
	cfg, request, _, other, friend := halfClearedParty(t)

	other.conn.in <- protocol.EncodePortalRequest(request)
	waitUntil(t, "the entry offer", func() bool { return len(other.frames.crossingOffers()) == 1 })
	offer := other.frames.crossingOffers()[0]
	if offer.OfferID == 0 {
		t.Fatal("an offer with no id can never be answered")
	}
	if offer.Terms.Arch != request.Arch {
		t.Fatalf("the offer names arch %v, not the one asked about %v", offer.Terms.Arch, request.Arch)
	}
	// The disclosure the prompt deliberately makes, and the denominator it is measured
	// against. The restored run carries exactly one defeated encounter.
	if offer.Terms.BossesDefeated != 1 || offer.Terms.BossesTotal < offer.Terms.BossesDefeated {
		t.Fatalf("the offer reads %d of %d", offer.Terms.BossesDefeated, offer.Terms.BossesTotal)
	}
	saved, live := cfg.Instances.Lookup(7001)
	if !live || offer.Terms.ResetsAtUnix != saved.ExpiresUnix {
		t.Fatalf("the offer states a reset of %d, the run resets at %d", offer.Terms.ResetsAtUnix, saved.ExpiresUnix)
	}
	// Nothing crossed and nothing bound: that is the whole point of an offer.
	if got := len(other.frames.transitions()); got != 0 {
		t.Fatalf("an offer moved the session: %d world changes", got)
	}
	if _, bound := cfg.Instances.Bound(saved.Ruin, friend); bound {
		t.Fatal("being offered a run bound the character to it")
	}
	if refusals := other.frames.actionRefusals(); len(refusals) != 0 {
		t.Fatalf("an offer was accompanied by a refusal: %+v", refusals)
	}
}

// Refusing is an answer: nothing crosses, nothing binds, and nothing is refused either.
func TestRefusingAnOfferOverTheWireIsAnsweredWithSilence(t *testing.T) {
	cfg, request, _, other, friend := halfClearedParty(t)

	other.conn.in <- protocol.EncodePortalRequest(request)
	waitUntil(t, "the entry offer", func() bool { return len(other.frames.crossingOffers()) == 1 })
	offer := other.frames.crossingOffers()[0]

	other.conn.in <- protocol.EncodeInstanceEntryAnswer(protocol.InstanceEntryAnswer{OfferID: offer.OfferID})
	// A refusal produces no frame at all, so the assertion has to be made against
	// something that does: a second crossing, whose offer can only arrive after the
	// answer ahead of it in this connection's queue was processed.
	other.conn.in <- protocol.EncodePortalRequest(request)
	waitUntil(t, "a second offer", func() bool { return len(other.frames.crossingOffers()) == 2 })
	if got := len(other.frames.transitions()); got != 0 {
		t.Fatalf("a refused offer moved the session: %d world changes", got)
	}
	if got := len(other.frames.actionRefusals()); got != 0 {
		t.Fatalf("refusing an offer was itself refused: %+v", other.frames.actionRefusals())
	}
	saved, _ := cfg.Instances.Lookup(7001)
	if _, bound := cfg.Instances.Bound(saved.Ruin, friend); bound {
		t.Fatal("refusing an offer bound the character")
	}

	// And the first offer is spent: the id cannot be used to change one's mind.
	other.conn.in <- protocol.EncodeInstanceEntryAnswer(protocol.InstanceEntryAnswer{OfferID: offer.OfferID, Accept: true})
	waitUntil(t, "the replay refusal", func() bool { return len(other.frames.actionRefusals()) == 1 })
	if got := other.frames.actionRefusals()[0]; got.Action != vnet.RefusedActionCrossPortal || got.Reason != vnet.RefusalReasonEntryOfferUnknown {
		t.Fatalf("a spent offer was answered with %+v", got)
	}
	if got := len(other.frames.transitions()); got != 0 {
		t.Fatalf("a spent offer moved the session: %d world changes", got)
	}
}

// Accepting crosses and binds, and the answer is a WorldChange like any other crossing.
func TestAcceptingAnOfferOverTheWireCrossesAndBinds(t *testing.T) {
	cfg, request, _, other, friend := halfClearedParty(t)

	other.conn.in <- protocol.EncodePortalRequest(request)
	waitUntil(t, "the entry offer", func() bool { return len(other.frames.crossingOffers()) == 1 })
	offer := other.frames.crossingOffers()[0]

	other.conn.in <- protocol.EncodeInstanceEntryAnswer(protocol.InstanceEntryAnswer{OfferID: offer.OfferID, Accept: true})
	waitUntil(t, "the accepted crossing", func() bool { return len(other.frames.transitions()) == 1 })
	if got := other.frames.transitions()[0].WorldID; got != 7001 {
		t.Fatalf("accepted into world %d, offered 7001", got)
	}
	saved, _ := cfg.Instances.Lookup(7001)
	if id, bound := cfg.Instances.Bound(saved.Ruin, friend); !bound || id != 7001 {
		t.Fatalf("accepting bound %d, %v", id, bound)
	}
	// The character is now owed this run, and the manager's list says so authoritatively.
	//
	// **The wire restatement is checked for consistency, never waited for.** A changed list
	// is offered to a non-blocking queue and dropped when that queue is full, then restated
	// only by the next change (game/instance_bindings.go). Straight after a crossing the new
	// world's chunk stream can fill the queue, so waiting for this frame was a race rather
	// than an assertion: it timed out on a loaded CI runner. The first statement, which is
	// not best-effort, keeps its wire coverage in the admission tests above.
	owed := cfg.Instances.Bindings(friend)
	if len(owed) != 1 || owed[0].Ruin != saved.Ruin || owed[0].BossesDefeated != 1 {
		t.Fatalf("the manager lists %+v, want the one new binding", owed)
	}
	if lists := other.frames.savedRunLists(); len(lists) > 1 {
		if got := lists[len(lists)-1]; len(got) != 1 || got[0].Arch != request.Arch || got[0].BossesDefeated != 1 {
			t.Fatalf("the restated saved-run list reads %+v", got)
		}
	}
}

// Cases 3 and 4: a character who owes a different run of this dungeon does not cross, is
// told so in a way a machine can route, and is told so again in a way a person can read.
func TestAMismatchedBindingIsRefusedAndExplainedInChat(t *testing.T) {
	cfg, chunks, open, peers, request := portalSession(t, 4)
	identities := ephemeralIdentities()
	owner := mintPortalCharacter(t, identities, "Astrid", 1)
	stranger := mintPortalCharacter(t, identities, "Bjorn", 2)
	restoreRuns(t, cfg,
		savedRun(request, 7001, 0x51EED, owner.character),
		savedRun(request, 8002, 0x8EED, stranger.character))

	live := playPortalCharacter(t, cfg, chunks, open, peers, identities, 1, owner, 1)
	other := playPortalCharacter(t, cfg, chunks, open, peers, identities, 2, stranger, 2)
	party(t, open, live, other, "Bjorn")

	live.conn.in <- protocol.EncodePortalRequest(request)
	waitUntil(t, "the owner to cross", func() bool { return len(live.frames.transitions()) == 1 })

	other.conn.in <- protocol.EncodePortalRequest(request)
	waitUntil(t, "the mismatch refusal", func() bool { return len(other.frames.actionRefusals()) == 1 })
	refused := other.frames.actionRefusals()[0]
	if refused.Action != vnet.RefusedActionCrossPortal || refused.Reason != vnet.RefusalReasonSessionMismatch {
		t.Fatalf("the mismatch was refused with %+v", refused)
	}
	waitUntil(t, "the mismatch warning", func() bool { return len(other.frames.chatMessages()) == 1 })
	warning := other.frames.chatMessages()[0]
	if warning.Text != game.SessionMismatchWarning || warning.SenderName != game.CommandSenderName {
		t.Fatalf("the warning is %+v", warning)
	}
	// Nothing crossed, no offer was made, and their own run is untouched.
	if got := len(other.frames.transitions()); got != 0 {
		t.Fatalf("a mismatch moved the session: %d world changes", got)
	}
	if got := len(other.frames.crossingOffers()); got != 0 {
		t.Fatalf("a mismatch offered terms: %+v", other.frames.crossingOffers())
	}
	if id, bound := cfg.Instances.Bound(game.InstanceRuin{CellX: world.RuinCellOf(int64(request.Arch[0])), CellZ: world.RuinCellOf(int64(request.Arch[2]))}, stranger.character); !bound || id != 8002 {
		t.Fatalf("the refused character now owes %d, %v", id, bound)
	}
}

// An answer sent from inside an instance names an offer that could not have been made
// from where the character is standing, and crosses nothing.
func TestAnEntryAnswerFromInsideAnInstanceIsRefused(t *testing.T) {
	cfg, chunks, open, peers, request := portalSession(t, 2)
	conn, frames := admit(t, cfg, chunks, open, peers, 1)

	conn.in <- protocol.EncodePortalRequest(request)
	waitUntil(t, "the crossing", func() bool { return len(frames.transitions()) == 1 })
	inside := frames.transitions()[0].WorldID

	conn.in <- protocol.EncodeInstanceEntryAnswer(protocol.InstanceEntryAnswer{OfferID: inside, Accept: true})
	waitUntil(t, "the refusal", func() bool { return len(frames.actionRefusals()) == 1 })
	if got := frames.actionRefusals()[0]; got.Action != vnet.RefusedActionCrossPortal || got.Reason != vnet.RefusalReasonEntryOfferUnknown {
		t.Fatalf("an answer from inside an instance was answered with %+v", got)
	}
	if got := len(frames.transitions()); got != 1 {
		t.Fatalf("an answer from inside an instance moved the session again: %d world changes", got)
	}
}

// A server with no instance manager answers an entry answer rather than ignoring it.
func TestAnEntryAnswerWithNoInstancesIsRefused(t *testing.T) {
	cfg, chunks, open, peers, _ := portalSession(t, 2)
	cfg.Instances = nil
	conn, frames := admit(t, cfg, chunks, open, peers, 1)

	conn.in <- protocol.EncodeInstanceEntryAnswer(protocol.InstanceEntryAnswer{OfferID: 3, Accept: true})
	waitUntil(t, "the refusal", func() bool { return len(frames.actionRefusals()) == 1 })
	if got := frames.actionRefusals()[0]; got.Action != vnet.RefusedActionCrossPortal || got.Reason != vnet.RefusalReasonEntryOfferUnknown {
		t.Fatalf("an answer with no instances was answered with %+v", got)
	}
	if got := len(frames.savedRunLists()); got != 0 {
		t.Fatalf("a server with no instances stated a saved-run list: %+v", frames.savedRunLists())
	}
}

// A body that has begun leaving acts on nothing, and an entry answer is an action.
//
// **The frame is still decoded**, so malformed bytes still fail the session; what a
// leaving body does not do is reach a handler. Two things are asserted, and the second is
// the one that bites: no answer comes back — under a routing that handled it, this
// connection would be told its offer was unknown — and the session survives, because a
// payload that is neither inert nor lifecycle-bearing ends it with
// `client sent ... during leave`. Dropping the tag from [inertWhileLeaving] therefore
// fails this test twice over, and a leaving character can never be bound for the day.
func TestAnEntryAnswerIsInertWhileTheBodyIsLeaving(t *testing.T) {
	cfg, chunks, open, peers, request := portalSession(t, 2)
	timeouts := longTimeouts()
	timeouts.Leave = 5 * time.Second

	conn := newFakeConn()
	done := make(chan error, 1)
	go func() {
		done <- session.Serve(context.Background(), conn, cfg, timeouts, chunks, open, peers, ephemeralIdentities(), 1, discard())
	}()
	conn.in <- hello(1)
	chooseCharacter(t, conn, "Eivor")
	_ = nextFrameOfKind(t, conn, vnet.PayloadServerWelcome)
	frames := collect(t, conn)
	waitUntil(t, "the character to enter the simulation", func() bool { return open.Count() == 1 })
	t.Cleanup(func() {
		_ = conn.Close()
		if err := <-done; err != nil {
			t.Errorf("session ended with %v", err)
		}
	})

	conn.in <- protocol.EncodeLeaveRequest()
	waitUntil(t, "the leave acknowledgement", func() bool { return countKind(frames, vnet.PayloadLeaveStarted) == 1 })
	conn.in <- protocol.EncodeInstanceEntryAnswer(protocol.InstanceEntryAnswer{OfferID: 9, Accept: true})
	// A portal request is inert for the same reason and is the frame that proves the
	// answer ahead of it was read and discarded rather than still sitting in the queue.
	conn.in <- protocol.EncodePortalRequest(request)
	conn.in <- protocol.EncodeLeaveCancelRequest()
	waitUntil(t, "the leave to be cancelled", func() bool { return countKind(frames, vnet.PayloadLeaveCancelResult) == 1 })
	if got := frames.actionRefusals(); len(got) != 0 {
		t.Fatalf("a leaving body was answered: %+v", got)
	}
	if got := len(frames.transitions()); got != 0 {
		t.Fatalf("a leaving body crossed: %d world changes", got)
	}
}

// countKind is how many frames of one payload type this session was sent.
func countKind(frames *collector, kind vnet.Payload) int {
	seen := 0
	for _, got := range frames.kindsReceived() {
		if got == kind {
			seen++
		}
	}
	return seen
}

// A torn-down connection stops being told about its character's bindings, and the reason
// this is a crash test rather than a leak test is worth stating.
//
// `trySend` is guarded by `accepting`, and `accepting` is **never** set false on the way
// out — it is toggled only around a world change. What keeps a send off a closed `out` is
// the teardown order and nothing else, which is exactly what the comment beside `trySend`
// says. The saved-run watcher joined that contract on #1044: if the teardown does not
// unwatch, or unwatches with a token that names no registration, the manager keeps a
// closure over this connection's queue, and the next binding change sends on a closed
// channel — a panic on the goroutine that drives every instance world, taking the process
// with it.
//
// So this test does not assert about a map it cannot see. It performs the teardown and
// then makes a binding change for that same character through exported API: with the
// unwatch in place nothing happens, and without it the test binary dies.
func TestATornDownSessionIsNoLongerToldAboutItsBindings(t *testing.T) {
	cfg, chunks, open, peers, request := portalSession(t, 4)
	identities := ephemeralIdentities()
	owner := mintPortalCharacter(t, identities, "Astrid", 1)
	// Two runs: the one this character owes at the world's ruin, and one at a ruin
	// nothing else touches, so binding to it later is a change to this character's list
	// rather than a repeat of one they already hold.
	elsewhere := game.SavedSession{
		ID: 9003, Seed: 0x9EED, Ruin: game.InstanceRuin{CellX: 41, CellZ: -17},
		ExpiresUnix:    time.Now().Add(6 * time.Hour).Unix(),
		DefeatedBosses: []vnet.MobKind{vnet.MobKindDraugrKing},
	}
	restoreRuns(t, cfg, savedRun(request, 7001, 0x51EED, owner.character), elsewhere)

	conn := newFakeConn()
	done := make(chan error, 1)
	go func() {
		done <- session.Serve(context.Background(), conn, cfg, noTimeouts(), chunks, open, peers, identities, 1, discard())
	}()
	conn.in <- helloNamed(owner.name, owner.seed)
	chooseCharacter(t, conn, owner.name)
	_ = nextFrameOfKind(t, conn, vnet.PayloadServerWelcome)
	frames := collect(t, conn)
	waitUntil(t, "the saved-run list", func() bool { return len(frames.savedRunLists()) == 1 })
	if got := frames.savedRunLists()[0]; len(got) != 1 || got[0].Arch != request.Arch {
		t.Fatalf("the session was told it owes %+v", got)
	}

	// The teardown, completed before anything else happens.
	if err := conn.Close(); err != nil {
		t.Fatalf("close: %v", err)
	}
	if err := <-done; err != nil {
		t.Fatalf("session ended with %v", err)
	}

	// A binding change for that character, after the connection is gone. A registration
	// the teardown failed to end would deliver into a closed queue here.
	if _, err := cfg.Instances.Join(9003, owner.character); err != nil {
		t.Fatalf("joining the second saved run: %v", err)
	}
	if id, bound := cfg.Instances.Bound(elsewhere.Ruin, owner.character); !bound || id != 9003 {
		t.Fatalf("the change this test depends on did not happen: %d, %v", id, bound)
	}
	// The list the dead connection would have been sent is the one it never receives.
	if got := len(frames.savedRunLists()); got != 1 {
		t.Fatalf("a torn-down session was sent %d saved-run lists", got)
	}
}

// A saved run naming a ruin this world does not have is dropped from the list rather than
// sent as a place.
//
// **Reachable, and not only defensively.** A sessions file is written by one server and
// read by another; an operator who changes the world seed, or restores a file from a
// different world, gets exactly this — a binding whose lattice cell resolves to no ruin.
// There is nothing honest to put in `arch` for it, and the absent-field zero is a real
// position: `(0, 0, 0)` is inside the playable world, so a client would draw a lockout at
// the origin and let the player walk to it.
func TestASavedRunWithNoRuinInThisWorldIsLeftOutOfTheList(t *testing.T) {
	cfg, chunks, open, peers, request := portalSession(t, 4)
	identities := ephemeralIdentities()
	owner := mintPortalCharacter(t, identities, "Astrid", 1)
	nowhere := game.SavedSession{
		ID: 9101, Seed: 0x9EED, Ruin: game.InstanceRuin{CellX: 41, CellZ: -17},
		ExpiresUnix:    time.Now().Add(6 * time.Hour).Unix(),
		DefeatedBosses: []vnet.MobKind{vnet.MobKindDraugrKing},
		Bound:          []game.InstanceCharacter{owner.character},
	}
	if _, exists := world.RuinAt(cfg.WorldSeed, nowhere.Ruin.CellX, nowhere.Ruin.CellZ); exists {
		t.Fatal("the fixture cell holds a ruin, so it cannot stand for one that is gone")
	}
	restoreRuns(t, cfg, savedRun(request, 7001, 0x51EED, owner.character), nowhere)

	live := playPortalCharacter(t, cfg, chunks, open, peers, identities, 1, owner, 1)
	waitUntil(t, "the saved-run list", func() bool { return len(live.frames.savedRunLists()) == 1 })
	got := live.frames.savedRunLists()[0]
	if len(got) != 1 {
		t.Fatalf("the list carries %d runs, want only the one this world has a place for: %+v", len(got), got)
	}
	if got[0].Arch != request.Arch {
		t.Fatalf("the surviving run names arch %v, want %v", got[0].Arch, request.Arch)
	}
	// The binding itself is untouched: dropping it from a frame is a statement about
	// what can be drawn, never a release.
	if id, bound := cfg.Instances.Bound(nowhere.Ruin, owner.character); !bound || id != 9101 {
		t.Fatalf("a run with no place lost its binding: %d, %v", id, bound)
	}
}
