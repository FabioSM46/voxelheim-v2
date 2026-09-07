package game

import (
	"cmp"
	"errors"
	"fmt"
	"slices"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// What survives a restart is the binding, not the world.
//
// This file is the durable half of a saved run, and it is small for one reason: an
// instance's world is a pure function of its seed. world.NewInstanceCache rebuilds it
// byte for byte, so "restore a saved session" is "read a handful of scalars and
// reconstruct" rather than "reload a world" — which is why nothing here, and nothing in
// persist.SessionStore, ever writes a chunk, a block or an entity of an instance. **An
// instance that persisted its terrain would have stopped being cheap to restore and
// started being a second world to keep in step**, and the one thing this file must never
// grow is a reason to reach for world.NewPersistentCache.
//
// **The manager is the authoritative record, which is what makes a restore possible at
// all.** A defeat is noticed by a simulation and drained into the manager on the tick it
// happens (see instance_binding.go), so the session's list of defeated encounters lives
// where a fresh simulation's absence of one cannot contradict it. A restored run is
// therefore a brand-new simulation over the stored seed, plus a manager that already
// knows which encounters that party has put down and who owes them.
//
// **The identity of a defeated encounter is its species**, appended by #1018 as distinct
// [vnet.MobKind] rows precisely so that it survives the restart this file exists for; an
// entity id is minted per spawn and means nothing afterwards. One known limit, recorded
// rather than designed around: recording is idempotent per species, so a dungeon holding
// two bosses of one species reports one defeated encounter both in memory and on disk.
// Widening that means giving an encounter an identity of its own, which is a change to
// what a boss *is* and belongs with the content that needs it — the format's per-session
// list is ordered and variable-length, so it can carry repeats the day that identity
// exists.

var (
	// ErrInstancesNotEmpty refuses a restore into a manager that is already running one.
	// A restore is a startup operation and rebuilding a session that is already live
	// would either duplicate it or replace a world somebody is standing in.
	ErrInstancesNotEmpty = errors.New("game: sessions cannot be restored into a running manager")
	// ErrDuplicateSession refuses a stored list that names one session twice. Filing the
	// second over the first would lose a run silently, and a file that says this is
	// corrupt in a way persist cannot see: it validates layout, not identity.
	ErrDuplicateSession = errors.New("game: two stored sessions share one id")
	// ErrInvalidSession refuses a stored session that cannot be one — no id, or no
	// expiry. Zero is "no entity" for an id everywhere in this server, and a saved
	// session with no reset is the one state instance_binding.go cannot produce.
	ErrInvalidSession = errors.New("game: stored session is not one this server could have written")
)

// SavedSession is one saved run as it crosses the boundary between the manager and the
// durable store.
//
// Deliberately not [InstanceSession]: that snapshot carries a simulation, a chunk cache
// and a lifetime context, all three of which belong to one process and none of which is
// written down. This is the six things that outlive one — which run, which world, which
// ruin, when it resets, what it has killed and who owes it — and the mapping to
// persist.SessionRecord is one field at a time in main, because game and persist do not
// import each other.
//
// **Bound is who owes this run, not who was inside it.** Occupancy is a fact about a
// process; a binding is a fact about the world. A restored session comes back empty and
// fully bound, which is exactly the state a saved session is in overnight.
type SavedSession struct {
	ID             uint64
	Seed           int64
	Ruin           InstanceRuin
	ExpiresUnix    int64
	DefeatedBosses []vnet.MobKind
	Bound          []InstanceCharacter
}

// SavedSessions is every run this server would have to restore, in a stable order.
//
// **Saved sessions only.** A free copy is an ephemeral world with an empty grace on it
// and nothing about it is worth a restart: nobody owes it anything, and rebuilding it
// would hand a party back a dungeon they had not cleared.
//
// Ordered by session id, and by each of the two lists' own order within an entry, so a
// server whose state has not changed writes a byte-identical file. That is what lets the
// autosave loop stay honest about what it is writing without a dirty flag in the manager.
func (m *InstanceManager) SavedSessions() []SavedSession {
	m.mu.Lock()
	defer m.mu.Unlock()

	bound := make(map[uint64][]InstanceCharacter, len(m.sessions))
	for visit, id := range m.bound {
		bound[id] = append(bound[id], visit.character)
	}

	saved := make([]SavedSession, 0, len(m.sessions))
	for id, s := range m.sessions {
		if s.state != InstanceSaved {
			continue
		}
		who := bound[id]
		// Map iteration is randomised, so the bindings need an order of their own or two
		// consecutive passes over identical state would disagree.
		slices.SortFunc(who, func(a, b InstanceCharacter) int {
			if c := slices.Compare(a.PlayerID[:], b.PlayerID[:]); c != 0 {
				return c
			}
			// cmp.Compare rather than a subtraction, which would overflow int on the
			// 32-bit builds the server cross-compiles for.
			return cmp.Compare(a.CharacterID, b.CharacterID)
		})
		saved = append(saved, SavedSession{
			ID: id, Seed: s.seed, Ruin: s.ruin, ExpiresUnix: s.expiresUnix,
			DefeatedBosses: append([]vnet.MobKind(nil), s.defeated...),
			Bound:          who,
		})
	}
	slices.SortFunc(saved, func(a, b SavedSession) int { return cmp.Compare(a.ID, b.ID) })
	return saved
}

// RestoreSessions rebuilds every stored run whose day has not ended yet, and reports how
// many it restored and how many it dropped as expired.
//
// **A cold start over an expired session cleans it up rather than restoring it.** The
// server may have been switched off across several midnights, so "expired" is not a state
// the tick loop can be relied on to have noticed — it is the first question asked of every
// record here. A dropped session releases nothing, because a session that was never
// rebuilt binds nobody; the cleanup is the absence, and the next save writes the shorter
// file.
//
// **Nothing about the world is read back.** Each surviving record becomes a brand-new
// simulation over its stored seed, which is what makes re-entry deterministic: the same
// seed generates the same chambers, and the party's progress rides on the manager's
// record of what they have killed rather than on anything preserved from the terrain.
//
// Validate-everything-then-apply, the discipline every store in persist keeps: the whole
// list is checked before a single session is filed, and a failure part-way through a
// rebuild unwinds what it made. A manager that refuses a restore is one holding exactly
// what it held before — which matters, because the caller's answer to a refusal is to
// start the world without its saved runs rather than to refuse to start at all.
func (m *InstanceManager) RestoreSessions(saved []SavedSession) (restored, expired int, err error) {
	m.mu.Lock()
	defer m.mu.Unlock()

	if m.closed {
		return 0, 0, ErrInstanceClosed
	}
	if len(m.sessions) != 0 {
		return 0, 0, fmt.Errorf("%w: %d are already running", ErrInstancesNotEmpty, len(m.sessions))
	}

	seen := make(map[uint64]struct{}, len(saved))
	for _, rec := range saved {
		if rec.ID == 0 || rec.ExpiresUnix == 0 {
			return 0, 0, fmt.Errorf("%w: id %d expiring at %d", ErrInvalidSession, rec.ID, rec.ExpiresUnix)
		}
		if _, duplicate := seen[rec.ID]; duplicate {
			return 0, 0, fmt.Errorf("%w: %d", ErrDuplicateSession, rec.ID)
		}
		seen[rec.ID] = struct{}{}
	}

	now := m.now().Unix()
	for _, rec := range saved {
		if now >= rec.ExpiresUnix {
			expired++
			continue
		}
		s, buildErr := m.newSessionLocked(rec.ID, rec.Seed, rec.Ruin)
		if buildErr != nil {
			// Unwind rather than leave a half-restored server: every session filed by this
			// call goes back out through the one path that tears one down, and the manager
			// is left as empty as it was found.
			for id, live := range m.sessions {
				m.removeLocked(id, live)
			}
			return 0, 0, fmt.Errorf("restoring session %d: %w", rec.ID, buildErr)
		}
		s.state = InstanceSaved
		s.expiresUnix = rec.ExpiresUnix
		s.defeated = append([]vnet.MobKind(nil), rec.DefeatedBosses...)
		for _, character := range rec.Bound {
			m.bindLocked(s, character)
			// **And the visit, which is what makes "re-entering returns them to the same
			// session" true across a restart.** [InstanceManager.Reenter] resolves a ruin
			// through the visit map, and that map is per process by construction: it
			// records which copy somebody was last in, and after a restart nobody has been
			// in any. Restoring it from the binding is not a second opinion about which
			// copy is theirs — a binding names the run they cleared, so it is the same
			// answer with a longer memory, and the two are removed together by
			// removeLocked when the reset arrives.
			m.visits[instanceVisit{s.ruin, character}] = s.id
		}
		restored++
	}
	return restored, expired, nil
}
