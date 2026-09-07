package game

import (
	"slices"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// The first boss saves the run.
//
// This file is the whole of that sentence: what a defeat is, what a save changes, and
// who is bound by it. It is deliberately one file rather than a rule spread through
// instance.go, because free and saved differ in exactly two ways — **who is bound, and
// when the session dies** — and both of those are here.
//
// **The trigger is a kill and nothing else.** [Sim.makeCorpseLocked] is the one killed-mob
// transition in this package; a creature the director removes goes through
// [Sim.discardMobLocked] instead and produces neither loot nor a defeat. That is what
// makes "only killing the first boss saves the session" a structural claim rather than a
// convention, and it is the boundary spawn.go asks a future boss reset to reuse.
//
// **Nothing a client sends appears anywhere below.** A binding is derived from the
// simulation's own record of a death that the simulation itself decided; there is no
// argument on any function here that a connection could have chosen, and the manager
// offers no exported way to save a session or to bind a character. The only writer is
// [InstanceManager.Step], running on the authoritative loop.
//
// **The seam between the two halves is a lock order, not a preference.** A kill happens
// under Sim.mu; [InstanceManager.mu] is always taken *before* a Sim lock, so the death
// path cannot call the manager. The simulation therefore writes down what it killed and
// the manager collects it on the tick it already steps that simulation — which is also
// what makes "every character inside the session at that moment" exact: Step holds the
// manager mutex across the step and the collection, so no join or leave can slip between
// the blow and the binding.

// recordBossDefeatLocked remembers that a boss-rank species died in this simulation.
//
// **The species is the encounter's identity**, and that is a decision the two boss rows
// make available rather than a shortcut: #1018 appended [vnet.MobKindVargrGuardian] and
// [vnet.MobKindDraugrKing] as distinct species, so a dungeon's bosses are told apart by
// what they are rather than by an entity id that is minted per spawn and means nothing
// after a restart. Downstream that matters twice — persistence has something it can write
// down, and "restore only the undefeated encounters" is a set difference rather than a
// count.
//
// Idempotent per species, so a session that somehow held two of one boss still reports one
// defeated encounter. The caller holds Sim.mu.
func (s *Sim) recordBossDefeatLocked(kind vnet.MobKind) {
	if slices.Contains(s.defeatedBosses, kind) {
		return
	}
	s.defeatedBosses = append(s.defeatedBosses, kind)
}

// takeDefeatedBosses hands over every defeat this simulation has not yet reported and
// forgets them, in the order they died.
//
// A drain rather than a read, because the manager is the authoritative record once a
// session exists: persistence restores a session's defeated encounters into the manager,
// where a fresh simulation has never heard of them.
func (s *Sim) takeDefeatedBosses() []vnet.MobKind {
	s.mu.Lock()
	defer s.mu.Unlock()
	if len(s.defeatedBosses) == 0 {
		return nil
	}
	taken := s.defeatedBosses
	s.defeatedBosses = nil
	return taken
}

// collectBossDefeatsLocked moves this tick's defeats into the session and saves it the
// first time there is one.
//
// The state change and the bindings are the same event: a session that is saved and binds
// nobody, or binds a party without being saved, is not a state this can produce. The
// caller holds InstanceManager.mu and has just stepped this session's simulation.
func (m *InstanceManager) collectBossDefeatsLocked(s *instanceSession) {
	defeated := s.sim.takeDefeatedBosses()
	if len(defeated) == 0 {
		return
	}
	for _, kind := range defeated {
		if !slices.Contains(s.defeated, kind) {
			s.defeated = append(s.defeated, kind)
		}
	}
	if s.state == InstanceSaved {
		return
	}
	s.state = InstanceSaved
	// The reset is set here and nowhere else, on the same event as the state and the
	// bindings: a saved session with no expiry, or an expiry on a session nothing saved,
	// is not a state this can produce. See instance_reset.go for which midnight it is.
	s.expiresUnix = nextResetUnix(m.now())
	for character := range s.members {
		m.bindLocked(s, character)
	}
}

// bindLocked binds one character to one saved session, for that session's ruin only.
//
// **Per character and per dungeon**: the key carries the ruin, so a character who owns a
// run in one ruin is unbound in every other and can start a fresh copy there.
//
// **The first binding for a ruin wins.** A binding is released when its session ends, and
// while that session is alive the run it names is the one the character already cleared;
// overwriting it with a later copy would silently move a claim the character still holds.
// Reaching a second copy of a ruin one is bound to is what the entry rules refuse — that
// is #978's, and this rule is what keeps the record truthful in the meantime rather than
// a second attempt at enforcing it.
func (m *InstanceManager) bindLocked(s *instanceSession, character InstanceCharacter) {
	key := instanceVisit{s.ruin, character}
	if _, bound := m.bound[key]; bound {
		return
	}
	m.bound[key] = s.id
}

// Bound reports which saved session, if any, this character owes this ruin.
//
// The answer is per dungeon by construction. A character bound in one ruin is reported
// unbound for every other, which is the whole of "binding is per character and per
// dungeon".
func (m *InstanceManager) Bound(ruin InstanceRuin, character InstanceCharacter) (uint64, bool) {
	m.mu.Lock()
	defer m.mu.Unlock()
	id, bound := m.bound[instanceVisit{ruin, character}]
	return id, bound
}
