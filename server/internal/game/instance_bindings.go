package game

import (
	"cmp"
	"slices"
)

// What a character owes, as a whole list rather than as a stream of changes.
//
// instance_binding.go owns *when* a binding is made and released. This file owns the one
// question a connection asks about them — what does this character owe, everywhere — and
// answers it the way `MarkerList` and `InventoryState` answer theirs: completely, every
// time, replaced rather than merged. A partial list would need an ordering and a revision
// to be right about, and being wrong about it means showing somebody a lockout that does
// not exist.
//
// **Three events can change the answer and all three are here.** A binding is made
// ([InstanceManager.bindLocked]), a run ends and releases every binding to it
// ([InstanceManager.removeLocked]), and a character arrives and has to be told what they
// already owe. The first two push; the third is a read the connection performs on
// admission. There is deliberately no fourth: nothing else in this manager writes
// `bound`.
//
// **The push runs under the manager's mutex**, which is what makes the list it carries
// exact — no join, leave, save or reset can slip between the change and the statement of
// it. The price is a rule every watcher must keep: a delivery function must not call back
// into this manager, and must not block. The one caller in this repository is a
// non-blocking send onto a connection's own outbound queue, which is the same seam a
// party invite already uses from another player's goroutine.
//
// **The first statement is made by [InstanceManager.WatchBindings] itself, under the same
// lock as the registration**, and that is a correctness requirement rather than a
// convenience. Reading the list and then installing a watcher are two lock acquisitions,
// and a binding made or released between them is announced to nobody — after which a
// wholesale list that is never restated leaves the client holding a lockout that may have
// ended. Installing first and enqueueing afterwards has the mirror defect: the callback
// can win the race to the connection's queue, and the client ends on the older frame. One
// acquisition that both installs and states has neither, and it is why there is no
// separate read for a connection to perform on admission.

// CharacterBinding is one saved run a character owes, as the manager records it.
//
// **The ruin is a lattice cell, not a place.** Turning it into the arch a client can go
// to needs the open world's seed, which this manager deliberately does not hold: it owns
// ephemeral worlds, and the identity of a site in the open world belongs to whoever knows
// which open world this is. The connection layer does that conversion, one field at a
// time, exactly as it does for `SavedSession` and persistence.
type CharacterBinding struct {
	Ruin           InstanceRuin
	BossesDefeated int
	BossesTotal    int
	ExpiresUnix    int64
}

// bindingWatcher is one connection's registration.
//
// **The token is what makes the pairing safe**, and it is not there because the race is
// reachable — it is not. A second connection for one account is refused admission while
// the first holds its identity claim (`Identities.claim`), and the first releases that
// claim strictly after its teardown has unwatched, so a reconnection cannot register
// before the connection it replaces has unregistered. That is three facts in two
// packages, one of them a line's *position* inside a hundred-line teardown. The token
// makes the guarantee local instead: an unwatch that does not name the registration it is
// ending removes nothing, so a delete can never take a watcher it did not install.
type bindingWatcher struct {
	token   uint64
	deliver func([]CharacterBinding) bool
}

// Bindings is every saved run this character currently owes, in a stable order.
//
// Ordered by ruin cell, because map iteration is not an order and two consecutive reads
// of unchanged state must produce the same list — which is what lets a recipient tell
// "the same list again" from "a different list". It is not a display order and carries no
// meaning beyond being the same one twice.
func (m *InstanceManager) Bindings(character InstanceCharacter) []CharacterBinding {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.bindingsLocked(character)
}

func (m *InstanceManager) bindingsLocked(character InstanceCharacter) []CharacterBinding {
	total := bossEncounterTotal()
	bindings := make([]CharacterBinding, 0, 1)
	for visit, id := range m.bound {
		if visit.character != character {
			continue
		}
		s := m.sessions[id]
		if s == nil {
			// A binding names a session, and removeLocked deletes both together, so this
			// is unreachable rather than merely unlikely. Skipped instead of dereferenced,
			// because the alternative to a missing row here is a nil panic on the one
			// goroutine that drives every instance world.
			continue
		}
		bindings = append(bindings, CharacterBinding{
			Ruin:           visit.ruin,
			BossesDefeated: len(s.defeated),
			// The same guard offerLocked applies, for the same reason: schemas/player.fbs
			// requires a recipient to reject a binding claiming more defeats than the
			// dungeon holds, and a run restored from disk can carry a species this build
			// no longer ranks as a boss.
			BossesTotal: max(total, len(s.defeated)),
			ExpiresUnix: s.expiresUnix,
		})
	}
	slices.SortFunc(bindings, func(a, b CharacterBinding) int {
		if c := cmp.Compare(a.Ruin.CellX, b.Ruin.CellX); c != 0 {
			return c
		}
		return cmp.Compare(a.Ruin.CellZ, b.Ruin.CellZ)
	})
	return bindings
}

// WatchBindings installs one connection's delivery, states this character's whole list
// through it immediately, and reports the registration's token and whether that first
// statement landed.
//
// **The registration and the first statement are one lock acquisition**, which is the
// whole point of this shape: see the note at the top of this file for the two gaps the
// two-call version has. A caller therefore never reads [InstanceManager.Bindings] on
// admission — the first call to `deliver` is that read, and every later announcement is
// ordered behind it by the same mutex.
//
// One watcher per character, replaced rather than added to: a character has one live
// connection, and a second registration is a reconnection rather than a second audience.
// The token is how a teardown says which registration it is ending; see
// [InstanceManager.UnwatchBindings].
//
// The delivery function is called under this manager's mutex — from the goroutine that
// drives every instance world for a later change, and from the caller's own goroutine for
// this first statement — so it must not block and must not call back in here. It returns
// whether the frame it built reached its connection; a dropped first statement is not
// self-correcting, because the list is restated only when it changes, and it is the
// caller's to answer for.
func (m *InstanceManager) WatchBindings(character InstanceCharacter, deliver func([]CharacterBinding) bool) (token uint64, delivered bool) {
	if deliver == nil {
		return 0, false
	}
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.bindingWatchers == nil {
		m.bindingWatchers = make(map[InstanceCharacter]bindingWatcher)
	}
	// From one, so that the zero token names no registration and an unwatch carrying it
	// removes nothing.
	m.nextBindingWatcher++
	token = m.nextBindingWatcher
	m.bindingWatchers[character] = bindingWatcher{token: token, deliver: deliver}
	return token, deliver(m.bindingsLocked(character))
}

// UnwatchBindings forgets one registration's delivery.
//
// **It names the registration rather than the character**, so a teardown can only remove
// the watcher it installed. A connection whose registration has already been replaced
// removes nothing, which is the fail-closed direction: the cost of a stale unwatch that
// matched would be a live session silently receiving no further list at all.
//
// Idempotent, and safe for a character that never registered one.
func (m *InstanceManager) UnwatchBindings(character InstanceCharacter, token uint64) {
	m.mu.Lock()
	defer m.mu.Unlock()
	if watcher, watched := m.bindingWatchers[character]; watched && watcher.token == token {
		delete(m.bindingWatchers, character)
	}
}

// announceBindingsLocked tells one character what they now owe, if anybody is listening.
//
// Called from the two writes that can change the answer, rather than from a place that
// notices afterwards: a change and its announcement are one event, so a binding that is
// made without being announced is not a state this manager can produce.
func (m *InstanceManager) announceBindingsLocked(character InstanceCharacter) {
	watcher, watched := m.bindingWatchers[character]
	if !watched {
		return
	}
	// The answer is deliberately discarded here and not at the first statement: a change
	// that cannot be delivered is followed by the next change, and the connection's own
	// sender is what notices and logs a full queue.
	_ = watcher.deliver(m.bindingsLocked(character))
}
