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
// **The push runs on the tick goroutine under the manager's mutex**, which is what makes
// the list it carries exact — no join, leave, save or reset can slip between the change
// and the statement of it. The price is a rule every watcher must keep: a delivery
// function must not call back into this manager, and must not block. The one caller in
// this repository is a non-blocking send onto a connection's own outbound queue, which
// is the same seam a party invite already uses from another player's goroutine.

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

// WatchBindings asks to be told this character's whole list whenever it changes.
//
// One watcher per character, replaced rather than added to: a character has one live
// connection, and a second registration is a reconnection rather than a second audience.
// The delivery function is called under this manager's mutex, on the goroutine that
// drives every instance world — so it must not block and must not call back in here.
// Registering does not deliver: the caller reads [InstanceManager.Bindings] itself, in
// the order it chooses relative to the rest of the state it is sending.
func (m *InstanceManager) WatchBindings(character InstanceCharacter, deliver func([]CharacterBinding)) {
	if deliver == nil {
		return
	}
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.bindingWatchers == nil {
		m.bindingWatchers = make(map[InstanceCharacter]func([]CharacterBinding))
	}
	m.bindingWatchers[character] = deliver
}

// UnwatchBindings forgets a connection's delivery. Idempotent, and safe to call for a
// character that never registered one.
func (m *InstanceManager) UnwatchBindings(character InstanceCharacter) {
	m.mu.Lock()
	defer m.mu.Unlock()
	delete(m.bindingWatchers, character)
}

// announceBindingsLocked tells one character what they now owe, if anybody is listening.
//
// Called from the two writes that can change the answer, rather than from a place that
// notices afterwards: a change and its announcement are one event, so a binding that is
// made without being announced is not a state this manager can produce.
func (m *InstanceManager) announceBindingsLocked(character InstanceCharacter) {
	deliver := m.bindingWatchers[character]
	if deliver == nil {
		return
	}
	deliver(m.bindingsLocked(character))
}
