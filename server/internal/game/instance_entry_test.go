package game

import (
	"strings"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// enterSavedRun admits one player, kills a boss in the run they entered, and leaves them
// standing outside it again — which is the state every case below starts from.
//
// The kill goes through the same transition instance_binding_test.go uses, and the Step
// that follows is the tick the manager collects it on. Leaving afterwards is what makes
// the run a saved copy somebody may walk back up to rather than one they are inside.
func enterSavedRun(t *testing.T, m *InstanceManager, p *Player, request protocol.PortalRequest) PortalEntry {
	t.Helper()
	decision := m.EnterPortal(p, request)
	if decision.Outcome != PortalAdmitted {
		t.Fatalf("entering a fresh copy: outcome %d, reason %s", decision.Outcome, decision.Reason)
	}
	killMobInSession(t, decision.Entry.Session, vnet.MobKindVargrGuardian)
	m.Step()
	session, live := m.Lookup(decision.Entry.Session.ID)
	if !live || session.State != InstanceSaved || len(session.DefeatedBosses) != 1 || session.ExpiresUnix == 0 {
		t.Fatalf("the first boss did not save the run: %+v", session)
	}
	if !m.Leave(decision.Entry.Session.ID, decision.Entry.Character) {
		t.Fatal("leaving the saved run failed")
	}
	return decision.Entry
}

// Case 1. Nobody owes this dungeon anything, so nobody is asked anything.
func TestAFreshCopyIsEnteredWithNoPrompt(t *testing.T) {
	m, _, request, join := portalHarness(t, 2)
	p := join()

	decision := m.EnterPortal(p, request)
	if decision.Outcome != PortalAdmitted {
		t.Fatalf("outcome %d, reason %s", decision.Outcome, decision.Reason)
	}
	if decision.Offer.ID != 0 {
		t.Fatal("a fresh copy offered terms nobody had to accept")
	}
	if _, bound := m.Bound(decision.Entry.Session.Ruin, decision.Entry.Character); bound {
		t.Fatal("entering a free copy bound the character")
	}
}

// Case 5, and the reason it is not a case: being bound is the ordinary state.
func TestABoundCharacterReentersTheirOwnRunWithNoPrompt(t *testing.T) {
	m, _, request, join := portalHarness(t, 2)
	p := join()
	entry := enterSavedRun(t, m, p, request)

	decision := m.EnterPortal(p, request)
	if decision.Outcome != PortalAdmitted {
		t.Fatalf("outcome %d, reason %s", decision.Outcome, decision.Reason)
	}
	if decision.Offer.ID != 0 {
		t.Fatal("a character was asked to accept the run they already owe")
	}
	if decision.Entry.Session.ID != entry.Session.ID {
		t.Fatalf("returned to session %d, not the bound %d", decision.Entry.Session.ID, entry.Session.ID)
	}
}

// Case 2, and the whole of what an offer discloses.
func TestASavedRunOffersItsTermsAndBindsNothingYet(t *testing.T) {
	m, _, request, join := portalHarness(t, 3)
	owner, friend := join(), join()
	inviteAndAccept(t, owner, friend, friend.name)
	entry := enterSavedRun(t, m, owner, request)

	decision := m.EnterPortal(friend, request)
	if decision.Outcome != PortalOffered {
		t.Fatalf("outcome %d, reason %s", decision.Outcome, decision.Reason)
	}
	if decision.Offer.ID == 0 {
		t.Fatal("an offer with no id can never be answered")
	}
	if decision.Offer.Arch != request.Arch || decision.Offer.Ruin != entry.Session.Ruin {
		t.Fatalf("the offer names another place: %+v", decision.Offer)
	}
	// The dead-boss count is the disclosure this prompt deliberately makes, and the
	// denominator has to be about the same thing the numerator counts.
	if decision.Offer.BossesDefeated != 1 || decision.Offer.BossesTotal != bossEncounterTotal() {
		t.Fatalf("progress reads %d of %d, want 1 of %d", decision.Offer.BossesDefeated, decision.Offer.BossesTotal, bossEncounterTotal())
	}
	if decision.Offer.BossesTotal == 0 || decision.Offer.BossesDefeated > decision.Offer.BossesTotal {
		t.Fatal("the terms could not be encoded: schemas/player.fbs refuses this pair")
	}
	if session, _ := m.Lookup(entry.Session.ID); decision.Offer.ExpiresUnix != session.ExpiresUnix {
		t.Fatal("the offer states a reset that is not the run's")
	}
	// Nothing has happened yet, and that is the point of an offer.
	if _, bound := m.Bound(entry.Session.Ruin, InstanceCharacter{friend.playerID, friend.characterID}); bound {
		t.Fatal("being offered a run bound the character to it")
	}
	if session, _ := m.Lookup(entry.Session.ID); len(session.Members) != 0 {
		t.Fatal("being offered a run put the character inside it")
	}
}

func TestAcceptingAnOfferCrossesAndBinds(t *testing.T) {
	m, _, request, join := portalHarness(t, 3)
	owner, friend := join(), join()
	inviteAndAccept(t, owner, friend, friend.name)
	entry := enterSavedRun(t, m, owner, request)
	offer := m.EnterPortal(friend, request).Offer

	decision := m.AnswerEntryOffer(friend, protocol.InstanceEntryAnswer{OfferID: offer.ID, Accept: true})
	if decision.Outcome != PortalAdmitted {
		t.Fatalf("outcome %d, reason %s", decision.Outcome, decision.Reason)
	}
	if decision.Entry.Session.ID != entry.Session.ID {
		t.Fatalf("accepted into session %d, offered %d", decision.Entry.Session.ID, entry.Session.ID)
	}
	character := InstanceCharacter{friend.playerID, friend.characterID}
	if id, bound := m.Bound(entry.Session.Ruin, character); !bound || id != entry.Session.ID {
		t.Fatalf("accepting bound %d, %v", id, bound)
	}
}

func TestRefusingAnOfferLeavesTheCharacterOutsideAndUnbound(t *testing.T) {
	m, _, request, join := portalHarness(t, 3)
	owner, friend := join(), join()
	inviteAndAccept(t, owner, friend, friend.name)
	entry := enterSavedRun(t, m, owner, request)
	offer := m.EnterPortal(friend, request).Offer

	decision := m.AnswerEntryOffer(friend, protocol.InstanceEntryAnswer{OfferID: offer.ID})
	if decision.Outcome != PortalDeclined || decision.Entry.Session.ID != 0 || decision.Reason != vnet.RefusalReasonUnknown {
		t.Fatalf("a refusal is not a refused crossing: %+v", decision)
	}
	character := InstanceCharacter{friend.playerID, friend.characterID}
	if _, bound := m.Bound(entry.Session.Ruin, character); bound {
		t.Fatal("refusing an offer bound the character")
	}
	if session, _ := m.Lookup(entry.Session.ID); len(session.Members) != 0 {
		t.Fatal("refusing an offer put the character inside the run")
	}
	// And the offer is spent: a refusal is an answer, so the same id cannot be used to
	// change one's mind later.
	again := m.AnswerEntryOffer(friend, protocol.InstanceEntryAnswer{OfferID: offer.ID, Accept: true})
	if again.Outcome != PortalRefused || again.Reason != vnet.RefusalReasonEntryOfferUnknown {
		t.Fatalf("a refused offer was still answerable: %+v", again)
	}
}

// An acceptance the server has no offer for is refused, whatever the id was made of.
func TestAnOfferTheServerNeverMadeIsRefused(t *testing.T) {
	m, _, request, join := portalHarness(t, 3)
	owner, friend := join(), join()
	inviteAndAccept(t, owner, friend, friend.name)
	entry := enterSavedRun(t, m, owner, request)

	// Before any offer exists at all.
	for name, answer := range map[string]protocol.InstanceEntryAnswer{
		"absent":   {},
		"zero":     {OfferID: 0, Accept: true},
		"invented": {OfferID: 0xDEADBEEF, Accept: true},
		"session":  {OfferID: entry.Session.ID, Accept: true},
	} {
		t.Run(name, func(t *testing.T) {
			decision := m.AnswerEntryOffer(friend, answer)
			if decision.Outcome != PortalRefused || decision.Reason != vnet.RefusalReasonEntryOfferUnknown {
				t.Fatalf("forged answer accepted: %+v", decision)
			}
			if decision.Entry.Session.ID != 0 || decision.Entry.Session.Sim != nil {
				t.Fatal("a refused answer leaked a world")
			}
		})
	}

	// And an offer minted for somebody else is not this character's to spend. The id is
	// real; the character is wrong, which is the whole check.
	offer := m.EnterPortal(friend, request).Offer
	other := join()
	if decision := m.AnswerEntryOffer(other, protocol.InstanceEntryAnswer{OfferID: offer.ID, Accept: true}); decision.Reason != vnet.RefusalReasonEntryOfferUnknown {
		t.Fatalf("one character answered another's offer: %+v", decision)
	}
}

// An offer is scoped to the crossing that produced it: it cannot be banked, replayed, or
// spent after its run has ended.
func TestAnOfferCannotBeReplayedAfterItsRunEnds(t *testing.T) {
	m, _, request, join := portalHarness(t, 3)
	owner, friend := join(), join()
	inviteAndAccept(t, owner, friend, friend.name)
	entry := enterSavedRun(t, m, owner, request)
	offer := m.EnterPortal(friend, request).Offer

	// Midnight arrives while the prompt is still on somebody's screen. The run is gone,
	// its bindings with it, and the answer that follows describes a world that no longer
	// exists.
	m.mu.Lock()
	m.removeLocked(entry.Session.ID, m.sessions[entry.Session.ID])
	m.mu.Unlock()

	decision := m.AnswerEntryOffer(friend, protocol.InstanceEntryAnswer{OfferID: offer.ID, Accept: true})
	if decision.Outcome != PortalRefused || decision.Reason != vnet.RefusalReasonEntryOfferUnknown {
		t.Fatalf("an offer outlived its run: %+v", decision)
	}
	if _, bound := m.Bound(entry.Session.Ruin, InstanceCharacter{friend.playerID, friend.characterID}); bound {
		t.Fatal("a replayed offer bound a character to a run that had ended")
	}
}

// A second crossing supersedes the first offer rather than banking it.
func TestALaterCrossingReplacesTheOfferItSupersedes(t *testing.T) {
	m, _, request, join := portalHarness(t, 3)
	owner, friend := join(), join()
	inviteAndAccept(t, owner, friend, friend.name)
	enterSavedRun(t, m, owner, request)

	first := m.EnterPortal(friend, request).Offer
	second := m.EnterPortal(friend, request).Offer
	if first.ID == 0 || second.ID == 0 || first.ID == second.ID {
		t.Fatalf("two crossings produced offers %d and %d", first.ID, second.ID)
	}
	if decision := m.AnswerEntryOffer(friend, protocol.InstanceEntryAnswer{OfferID: first.ID, Accept: true}); decision.Reason != vnet.RefusalReasonEntryOfferUnknown {
		t.Fatalf("a superseded offer was still answerable: %+v", decision)
	}
	if decision := m.AnswerEntryOffer(friend, protocol.InstanceEntryAnswer{OfferID: second.ID, Accept: true}); decision.Outcome != PortalAdmitted {
		t.Fatalf("the current offer was refused: %+v", decision)
	}
}

// Cases 3 and 4, in both directions, and the disclosure rule they are bound by.
func TestTwoDifferentBindingsInOnePartyAreRefusedInBothDirections(t *testing.T) {
	t.Run("running another saved run", func(t *testing.T) {
		m, _, request, join := portalHarness(t, 4)
		owner, stranger := join(), join()
		// Two saved runs of the one ruin, made apart, and then one party.
		theirs := enterSavedRun(t, m, owner, request)
		mine := enterSavedRun(t, m, stranger, request)
		if theirs.Session.ID == mine.Session.ID {
			t.Fatal("the fixture produced one run, not two")
		}
		inviteAndAccept(t, owner, stranger, stranger.name)
		// The party route now points at the owner's run.
		if decision := m.EnterPortal(owner, request); decision.Outcome != PortalAdmitted {
			t.Fatalf("the owner could not re-enter their own run: %+v", decision)
		}

		decision := m.EnterPortal(stranger, request)
		if decision.Outcome != PortalMismatch || decision.Reason != vnet.RefusalReasonSessionMismatch {
			t.Fatalf("outcome %d, reason %s", decision.Outcome, decision.Reason)
		}
		// Nothing about the other run comes back with the refusal: no world, no terms,
		// no id, and no count.
		if decision.Entry.Session.ID != 0 || decision.Entry.Session.Sim != nil || decision.Offer != (EntryOffer{}) {
			t.Fatalf("a mismatch disclosed a session: %+v", decision)
		}
		if session, _ := m.Lookup(theirs.Session.ID); len(session.Members) != 1 {
			t.Fatal("a refused character crossed anyway")
		}
	})

	t.Run("a fresh copy a party member started", func(t *testing.T) {
		m, _, request, join := portalHarness(t, 4)
		owner, unbound := join(), join()
		enterSavedRun(t, m, owner, request)
		inviteAndAccept(t, owner, unbound, unbound.name)
		// The unbound member walks in first and starts a copy of their own, which is now
		// the party's route.
		if decision := m.EnterPortal(unbound, request); decision.Outcome != PortalAdmitted {
			t.Fatalf("the unbound member could not start a copy: %+v", decision)
		}

		decision := m.EnterPortal(owner, request)
		if decision.Outcome != PortalMismatch || decision.Reason != vnet.RefusalReasonSessionMismatch {
			t.Fatalf("outcome %d, reason %s", decision.Outcome, decision.Reason)
		}
		if decision.Entry.Session.ID != 0 || decision.Offer != (EntryOffer{}) {
			t.Fatalf("a mismatch disclosed a session: %+v", decision)
		}
	})
}

// The one sentence both directions of a mismatch produce, and what it may not contain.
func TestTheMismatchWarningNamesNoOtherSession(t *testing.T) {
	t.Parallel()

	if SessionMismatchWarning == "" {
		t.Fatal("the mismatch is refused with no explanation at all")
	}
	// A count, an id or a name here would be the disclosure the refusal exists to avoid,
	// and a digit is the cheapest way to catch one arriving later.
	if strings.ContainsAny(SessionMismatchWarning, "0123456789") {
		t.Fatalf("the warning quotes a number: %q", SessionMismatchWarning)
	}
}

// The denominator and the numerator are counted at the same granularity, which is what
// makes "1 of 3" a sentence rather than two unrelated numbers.
func TestTheBossTotalCountsEveryBossRankRow(t *testing.T) {
	t.Parallel()

	total := bossEncounterTotal()
	if total == 0 {
		t.Fatal("a dungeon with no boss encounters cannot state progress at all")
	}
	counted := 0
	for kind, def := range mobRegistry {
		if def.isBoss() {
			counted++
			continue
		}
		if kind == vnet.MobKindVargrGuardian || kind == vnet.MobKindDraugrKing {
			t.Fatalf("%s is a boss species that is not boss-rank", kind)
		}
	}
	if counted != total {
		t.Fatalf("bossEncounterTotal = %d, boss-rank rows = %d", total, counted)
	}
}
