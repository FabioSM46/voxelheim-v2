package game

import (
	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// Who may cross, and what they are accepting when they do.
//
// instance_binding.go says who a saved run binds and instance_reset.go says when it ends.
// Neither of them refuses anything: a character bound to one copy of a ruin could walk
// into another copy of it and be admitted, and a character with no claim at all could
// walk into somebody's half-cleared run and be bound to it without ever being asked. This
// file is the rule that stops both, and it is deliberately the only place either question
// is answered.
//
// **Four cases, and they are the whole of it:**
//
//  1. Nobody owes this dungeon anything and the copy being entered is free — cross, no
//     prompt. The ordinary entry, and the one that must stay ordinary.
//  2. The copy being entered is a saved run and the character owes nothing here — an
//     **offer**. Accepting crosses and binds; refusing leaves them outside, unbound, in
//     the open world they were already standing in.
//  3. The character owes run S and the arch resolves to run T — refused, with a chat line
//     saying so. Nothing crosses.
//  4. The character owes run S and the arch resolves to a fresh copy somebody else just
//     started — the same refusal as 3, and it is the symmetric case rather than a rule of
//     its own. Both are "this is not your run", and both are decided by the same
//     comparison below.
//
// **Case 5 is not a case.** A character bound to S entering the instance running S
// crosses exactly like case 1, prompt and all — which is to say without one. Being bound
// is the ordinary state of somebody who cleared half a dungeon this morning, and a server
// that asked them to confirm it every time they walked back in would have made the
// lockout the feature instead of the consequence.
//
// **A prompt is an offer, not a question the client may answer on its own.** The client
// is handed numbers and hands back a yes or a no naming the offer; every condition is
// decided again here at that moment, because a run can reset, end, or stop being the one
// behind that arch between the two frames. Nothing a client sends chooses a session,
// names a world, or binds anything — an [EntryOffer] id is minted here, held here, spent
// here, and means nothing to any other character.
//
// **Nothing any of these answers reveals is more than the prompt deliberately discloses.**
// A mismatch says only that the character's run is not this one; it does not say whose the
// other run is, how far into it anybody is, or that it exists at all beyond the fact the
// player is already standing in front of. A refused answer says only that the server is
// not holding that offer, which is one sentence for an id that never existed, one already
// spent, one superseded, and one whose run has since ended.

// SessionMismatchWarning is what a character is told when they reach a copy of a dungeon
// that is not the run they owe.
//
// **A sentence, and therefore not a refusal reason.** [vnet.RefusalReason] is a vocabulary
// of tokens every client turns into its own words; this is words, addressed to a person,
// and it travels the way the server already says something to one player — as a chat line
// from [CommandSenderName]. Smuggling it into the refusal enum would make a machine parse
// prose to route an answer, and would put one server's phrasing on every client's screen.
//
// **It names nothing about the other run.** Not whose it is, not how far into it anybody
// is, not whether it exists rather than being a copy somebody started a moment ago — the
// two directions of the mismatch produce this one sentence precisely so that neither can
// be told from the other. The dead-boss count in an [EntryOffer] is the only thing this
// server discloses about a run a character is not in, and it is disclosed to somebody who
// is being asked to join it.
const SessionMismatchWarning = "Your run of this dungeon is not the one beyond this arch. You cannot enter until yours resets."

// PortalOutcome is what the entry rules decided. Every crossing produces exactly one.
type PortalOutcome uint8

const (
	// PortalRefused carries a machine-routable reason and no crossing.
	PortalRefused PortalOutcome = iota
	// PortalAdmitted is a crossing. The character is a member of the session in Entry
	// and the caller owes them the world change.
	PortalAdmitted
	// PortalOffered is case 2. Nothing has crossed and nothing is bound: the caller owes
	// the character the offer and nothing else.
	PortalOffered
	// PortalMismatch is cases 3 and 4. The caller owes the character the refusal reason
	// and the chat line that explains it; nothing has crossed.
	PortalMismatch
	// PortalDeclined answers a refused offer. The character asked for nothing further,
	// so the caller owes them nothing: no frame, no refusal, and no binding.
	PortalDeclined
)

// EntryOffer is one crossing this server is willing to make, held until it is answered.
//
// **Scoped to the crossing that produced it.** It names the arch it was made at, the run
// it was made about and the character it was made for, and every one of those is checked
// again when the answer arrives. It cannot be banked: a later crossing replaces it, an
// answer spends it, and the run's own end discards it.
type EntryOffer struct {
	ID             uint64
	Ruin           InstanceRuin
	Arch           [3]int32
	BossesDefeated int
	BossesTotal    int
	ExpiresUnix    int64
}

// PortalDecision is the single answer [InstanceManager.EnterPortal] and
// [InstanceManager.AnswerEntryOffer] both return. Exactly one of Entry, Offer and Reason
// is meaningful, and Outcome says which.
type PortalDecision struct {
	Outcome PortalOutcome
	Entry   PortalEntry
	Offer   EntryOffer
	Reason  vnet.RefusalReason
}

// pendingOffer is the server's half of an offer. The client is told the id and the terms;
// everything that decides whether the answer is honoured is here and is never sent.
type pendingOffer struct {
	id      uint64
	session uint64
	ruin    InstanceRuin
	arch    [3]int32
}

// bossEncounterTotal is how many boss encounters a dungeon holds.
//
// **Counted from the registry, at the same granularity a defeat is recorded.** A defeat
// is remembered by species (see instance_binding.go), so the denominator of a "1 / 3" has
// to be a count of species too or the two numbers would not be about the same thing. It
// is therefore every boss-rank row rather than a constant somebody has to remember to
// move: the next boss is a row in mobRegistry and nothing else, exactly as
// [spawnableSpecies] already assumes.
//
// **What this is not, said plainly because it will matter later.** It is not the number of
// boss rooms a particular dungeon's *layout* places, which is a property of the layout and
// belongs to the issue that builds one. Today there is one dungeon and no placement, so
// "every boss-rank species" and "this dungeon's encounters" are the same set; the day they
// stop being, this function is the one place that has to learn the difference.
func bossEncounterTotal() int {
	total := 0
	for _, def := range mobRegistry {
		if def.isBoss() {
			total++
		}
	}
	return total
}

// instanceCharacter reads the identity this manager files a player under.
func (p *Player) instanceCharacter() InstanceCharacter {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	return InstanceCharacter{p.playerID, p.characterID}
}

// AnswerEntryOffer spends one offer and, on an acceptance, re-decides the crossing.
//
// **The offer is forgotten either way, before anything else happens.** An acceptance that
// is then refused on its merits does not leave the offer standing for a second attempt:
// the terms it stated were true when it was written and the whole reason the answer is
// re-validated is that they may not be now. That is what makes a replay impossible rather
// than merely unlikely.
func (m *InstanceManager) AnswerEntryOffer(p *Player, answer protocol.InstanceEntryAnswer) PortalDecision {
	if p == nil {
		return PortalDecision{Reason: vnet.RefusalReasonEntryOfferUnknown}
	}
	character := p.instanceCharacter()

	m.mu.Lock()
	defer m.mu.Unlock()
	offer, held := m.offers[character]
	// One sentence for an id that was never minted, one already spent, one superseded by
	// a later crossing, and one whose run has ended: a guess learns nothing from any of
	// them. The zero id is refused here rather than by chance, because a manager holding
	// no offer for this character would otherwise compare it against a zero-valued map
	// miss and reach the same answer for the wrong reason.
	if !held || answer.OfferID == 0 || offer.id != answer.OfferID {
		return PortalDecision{Reason: vnet.RefusalReasonEntryOfferUnknown}
	}
	delete(m.offers, character)
	if !answer.Accept {
		return PortalDecision{Outcome: PortalDeclined}
	}
	return m.crossLocked(p, protocol.PortalRequest{Arch: offer.arch, HasArch: true}, &offer)
}

// forgetOfferLocked discards any offer held for this character.
//
// Called wherever the crossing an offer belongs to has stopped being available — leaving
// a world, a session ending, shutdown — so that an offer never outlives the situation it
// described. It is deliberately unconditional: the map miss is cheaper than the question.
func (m *InstanceManager) forgetOfferLocked(character InstanceCharacter) {
	delete(m.offers, character)
}
