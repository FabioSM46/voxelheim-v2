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
	// ErrRestoreExceedsLimit refuses a stored list holding more live runs than this
	// server is configured to carry.
	//
	// **Distinct from [ErrInstanceLimit] rather than a reuse of it**, because the two say
	// different things to whoever reads them. `ErrInstanceLimit` is "the server is full
	// right now", and it is transient: the next empty grace or midnight reset clears it,
	// and a party that waits gets in. This is a configuration that cannot be satisfied at
	// all — nothing the server does while running makes the file smaller — so a caller
	// that treated it as the same condition would wait for something that never arrives.
	ErrRestoreExceedsLimit = errors.New("game: more saved runs on disk than this server is configured to hold")
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
// **The configured instance limit bounds a restore exactly as it bounds a Create**, and
// that is the whole of why the count below happens before anything is built. A restored
// run is a live instance world — its own [Sim] and its own chunk cache — filed in the map
// [InstanceManager.createLocked] caps, and the moment this returns, `Create` is refused
// against that same map. So a restore that ignored the limit would not merely overspend
// the operator's memory budget; it would make the limit mean one thing for a run this
// process opened and another for a run it read, which is not a distinction anything else
// in this manager draws.
//
// The reachable case is ordinary rather than adversarial: **an operator lowering
// `-max-instances` between two runs of the server.** A file this build writes can never
// hold more surviving runs than the limit that wrote it, because [SavedSessions] draws
// from the capped map — but nothing carries that limit forward, `-max-instances` accepts
// 1..1024 and persist.MaxSavedSessions is 1024, so the two ranges coincide exactly and a
// file written at the top of that range is a legal input to a server started at the
// bottom of it.
//
// **Refused whole rather than filled to the cap**, because there is no non-arbitrary way
// to choose which runs survive: map order is not an order, and dropping somebody's
// lockout by accident of iteration is worse than dropping every lockout on purpose and
// saying so. The error names both counts and the flag, because the operator's fix is to
// raise it and start again.
//
// **And it is worth knowing what a refusal costs, because it is not free.** The caller
// logs this and starts the world with no saved runs, so every lockout in that world is
// gone; the sessions file is then rewritten by the first autosave pass, within one
// interval. A refusal is therefore recoverable only if the operator acts on the startup
// error before that pass. That is the same trade the doc on main.restoreSessions already
// records for an unreadable file, and it is stated here too rather than left for somebody
// to discover: the alternative — a server silently running past the budget its operator
// set, refusing every new dungeon entry until sessions drain — is not the quieter failure
// it looks like.
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

	// **One reading of the clock for both passes below, and that is not a tidiness
	// preference.** The count decides whether the restore is allowed; the loop decides
	// what gets built. Reading the clock twice would let a record fall on one side of its
	// expiry in the count and the other side in the loop — a midnight landing between two
	// statements — and the number this refusal is made from would then not describe what
	// the loop went on to file.
	now := m.now().Unix()

	seen := make(map[uint64]struct{}, len(saved))
	live := 0
	for _, rec := range saved {
		if rec.ID == 0 || rec.ExpiresUnix == 0 {
			return 0, 0, fmt.Errorf("%w: id %d expiring at %d", ErrInvalidSession, rec.ID, rec.ExpiresUnix)
		}
		if _, duplicate := seen[rec.ID]; duplicate {
			return 0, 0, fmt.Errorf("%w: %d", ErrDuplicateSession, rec.ID)
		}
		seen[rec.ID] = struct{}{}
		// Counted against the same expiry test the loop applies, so this is the number of
		// sessions that will actually be built rather than the number of records in the
		// file. A file holding two thousand runs that all reset last week restores none,
		// and must not be refused for a limit it never reaches.
		if now < rec.ExpiresUnix {
			live++
		}
	}
	if live > m.maxSessions {
		return 0, 0, fmt.Errorf("%w: %d saved runs have not reset yet and -max-instances is %d; raise it to at least %d and start again",
			ErrRestoreExceedsLimit, live, m.maxSessions, live)
	}

	for _, rec := range saved {
		if now >= rec.ExpiresUnix {
			expired++
			continue
		}
		s, buildErr := m.newSessionLocked(rec.ID, rec.Seed, rec.Ruin, rec.DefeatedBosses)
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
