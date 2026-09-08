package game

import (
	"context"
	"errors"
	"log/slog"
	"sync"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// InstanceEmptyGrace is counted in simulation steps, just like corpse and drop
// lifetimes. A stalled server does not expire a world while it cannot simulate it.
const InstanceEmptyGrace = 30 * time.Minute
const DefaultMaxInstances = 32

var (
	ErrInstanceLimit       = errors.New("game: concurrent instance limit reached")
	ErrInstanceClosed      = errors.New("game: instance manager is closed")
	ErrInstanceCombat      = errors.New("game: boss combat blocks instance admission")
	ErrInstanceMissing     = errors.New("game: instance no longer exists")
	ErrCharacterInInstance = errors.New("game: character is already inside another instance")
)

// InstanceRuin names a lattice cell in this server's open world. It deliberately
// excludes the drawing and arch: those are properties of the ruin, not its identity.
type InstanceRuin struct{ CellX, CellZ int64 }

// InstanceCharacter survives a reconnect; an entity id does not. Character ids
// alone are insufficient because each account has its own character namespace.
type InstanceCharacter struct {
	PlayerID    identity.PlayerID
	CharacterID uint64
}

type InstanceState uint8

const (
	InstanceFree InstanceState = iota
	// InstanceSaved is what killing the first boss of a session makes it, and the only
	// thing that ever does. It means exactly two things and no third: every character
	// inside at that moment is bound to this copy of this ruin, and the empty grace no
	// longer applies — a saved session lives until its reset. See instance_binding.go
	// for who is bound and instance_reset.go for when it ends.
	InstanceSaved
)

// InstanceSession is a snapshot. Members and DefeatedBosses are copied; editing them
// changes no state.
// Sim and Chunks belong to the manager. Consumers must finish using them when
// Context ends, and must not keep them alive after leaving and the empty grace.
// Active membership prevents expiry. Check Lookup or Context before routing a
// retained snapshot: its pointers cannot revoke themselves after expiry.
// Joining here records occupancy only; transferring a Player is a separate API.
type InstanceSession struct {
	ID      uint64
	Seed    int64
	Ruin    InstanceRuin
	State   InstanceState
	Members []InstanceCharacter
	// DefeatedBosses names every boss encounter this session has already put down, in
	// the order they died, by the species that identifies it. It is what a "1 / 3" is
	// counted from, and what tells a restore which encounters to leave out.
	DefeatedBosses []vnet.MobKind
	// ExpiresUnix is the wall-clock second this saved run resets at — the server's next
	// midnight on the real calendar. Zero on a session that is not saved, because a free
	// copy ends on the empty grace instead. See instance_reset.go.
	ExpiresUnix int64
	Sim         *Sim
	Chunks      *world.Cache
	Context     context.Context
}

type instanceSession struct {
	id       uint64
	seed     int64
	ruin     InstanceRuin
	state    InstanceState
	defeated []vnet.MobKind
	// expiresUnix is set once, when the first boss falls, and never moved afterwards: a
	// run cleared at 23:59 resets sixty seconds later and one cleared at 00:01 lasts the
	// day, which is what "the day is the unit" means. See instance_reset.go.
	expiresUnix int64
	members     map[InstanceCharacter]struct{}
	sim         *Sim
	chunks      *world.Cache
	ctx         context.Context
	cancel      context.CancelFunc
	emptyTicks  uint32
	tick        uint64
}

type instanceVisit struct {
	ruin      InstanceRuin
	character InstanceCharacter
}

// InstanceManager owns ephemeral worlds, never sockets or persistent records.
// Its mutex orders admission, empty expiry, and shutdown. It is always acquired
// before a Sim lock; callers must never call it while holding a Sim lock.
//
// There are no per-instance goroutines: Step runs on the server's existing loop,
// and Cache.Get generates synchronously on its caller. Cancelling the lifetime
// context releases consumers waiting on that cache; the server joins its workers
// before Close, so there is no generation left to drain at shutdown.
type InstanceManager struct {
	mu                     sync.Mutex
	tickRate, viewDistance uint8
	maxSessions            int
	mintEntityID           func() uint64
	// now is the wall clock the midnight reset is measured against, and the one place
	// this package reads real time. Injectable because a test that has to wait for a
	// real midnight is a test nobody runs; never nil after NewInstanceManager.
	now        func() time.Time
	log        *slog.Logger
	options    []SimOption
	graceTicks uint32
	closed     bool
	sessions   map[uint64]*instanceSession
	inside     map[InstanceCharacter]uint64
	// bound is which saved session a character owes a ruin. Keyed like visits — by ruin
	// and character — because that is what makes a binding per dungeon; unlike visits it
	// survives leaving, and is released only when its session ends.
	bound         map[instanceVisit]uint64
	visits        map[instanceVisit]uint64
	partyVisits   map[portalPartyVisit]uint64
	portalEntries map[InstanceCharacter]PortalEntry
	disconnected  map[InstanceCharacter]portalReconnect
	// offers is the one entry offer a character may have outstanding, and holding at
	// most one per character is what "an offer is scoped to the crossing that produced
	// it" is made of: a later crossing overwrites the earlier offer rather than banking
	// it. See instance_entry.go.
	offers map[InstanceCharacter]pendingOffer
	// bindingWatchers is one live connection per character, told the whole list whenever
	// it changes. See instance_bindings.go for what a watcher may and may not do, and for
	// why an unwatch has to name the registration it is ending.
	bindingWatchers    map[InstanceCharacter]bindingWatcher
	nextBindingWatcher uint64
}

// NewInstanceManager requires the very same mintEntityID passed to the open
// world's NewSim (Registry.NextID). It never constructs a counter of its own.
// Session ids also use this source: they name events, not seed-derived sites.
func NewInstanceManager(tickRate, viewDistance uint8, maxSessions int, mintEntityID func() uint64, log *slog.Logger, options ...SimOption) (*InstanceManager, error) {
	if maxSessions < 1 {
		return nil, errors.New("game: instance limit must be positive")
	}
	// NewSim owns option validation. This temporary simulation starts no goroutine
	// and mints nothing; validate before admission can allocate a live instance.
	probe := world.NewInstanceCache(0, 1, 1)
	if _, err := NewSim(tickRate, viewDistance, 0, NewCacheTerrain(probe), probe, mintEntityID, log, options...); err != nil {
		return nil, err
	}
	return &InstanceManager{
		tickRate: tickRate, viewDistance: viewDistance, maxSessions: maxSessions,
		mintEntityID: mintEntityID, now: time.Now, log: log, options: append([]SimOption(nil), options...),
		graceTicks:    ticksFor(InstanceEmptyGrace, tickRate),
		portalEntries: make(map[InstanceCharacter]PortalEntry), disconnected: make(map[InstanceCharacter]portalReconnect),
		offers:          make(map[InstanceCharacter]pendingOffer),
		bindingWatchers: make(map[InstanceCharacter]bindingWatcher),
		sessions:        make(map[uint64]*instanceSession), inside: make(map[InstanceCharacter]uint64), bound: make(map[instanceVisit]uint64), visits: make(map[instanceVisit]uint64), partyVisits: make(map[portalPartyVisit]uint64),
	}, nil
}

// Create always creates a private copy, including when another group already
// occupies the same ruin. The entry-policy caller chooses which copy to Join.
// An unoccupied new copy gets the same grace as one whose last member left.
func (m *InstanceManager) Create(ruin InstanceRuin) (InstanceSession, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	s, err := m.createLocked(ruin)
	if err != nil {
		return InstanceSession{}, err
	}
	return s.snapshot(), nil
}

func (m *InstanceManager) createLocked(ruin InstanceRuin) (*instanceSession, error) {
	if m.closed {
		return nil, ErrInstanceClosed
	}
	if len(m.sessions) >= m.maxSessions {
		return nil, ErrInstanceLimit
	}
	// **The mint is a loop because a restored id is not one this counter has issued.**
	// Session ids are minted from the entity counter, which starts afresh in every
	// process, while a session restored from disk carries the id the *previous* process
	// minted — so the very first Create after a restart can name a run that is already
	// live and silently replace it in this map. Retrying until the id is free is what
	// makes "one id names one session" true across a restart without serialising the
	// counter; it terminates because the counter is monotonic and the map is finite.
	// See instance_persist.go for why the stored id is kept rather than re-minted.
	id := m.mintEntityID()
	for m.sessions[id] != nil {
		id = m.mintEntityID()
	}
	// A bijection of ids provides a fresh seed even for two copies of one ruin.
	s, err := m.newSessionLocked(id, int64(id^0x49a3d758c1e260bf), ruin, nil)
	if err != nil {
		return nil, err
	}
	return s, nil
}

// newSessionLocked builds one live session over a given id and seed and files it.
//
// Split out of [InstanceManager.createLocked] because a restore needs exactly this and
// must not derive the seed: a restored run's world has to be the one its party cleared,
// which is the stored seed and not a function of whatever id it is filed under. Every
// other property of a live session — its simulation, its chunk cache, its lifetime
// context — is reconstructed here either way, which is the whole of "the world is never
// persisted".
func (m *InstanceManager) newSessionLocked(id uint64, seed int64, ruin InstanceRuin, defeated []vnet.MobKind) (*instanceSession, error) {
	progress := dungeonProgressFrom(defeated)
	chunks, gate := world.NewGatedInstanceCache(seed, world.DefaultWorkers, 72, progress.guardian)
	sim, err := NewSim(m.tickRate, m.viewDistance, seed, NewCacheTerrain(chunks), chunks, m.mintEntityID, m.log, m.options...)
	if err != nil {
		return nil, err
	}
	if err := sim.placeDungeonEncounters(seed, gate, progress); err != nil {
		return nil, err
	}
	ctx, cancel := context.WithCancel(context.Background())
	s := &instanceSession{id: id, seed: seed, ruin: ruin, members: make(map[InstanceCharacter]struct{}), sim: sim, chunks: chunks, ctx: ctx, cancel: cancel}
	m.sessions[id] = s
	return s, nil
}

// Join records a character in a selected copy. Repeated joins are idempotent;
// joining a different copy while still inside another is refused atomically.
func (m *InstanceManager) Join(id uint64, character InstanceCharacter) (InstanceSession, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.closed {
		return InstanceSession{}, ErrInstanceClosed
	}
	s := m.sessions[id]
	if s == nil {
		return InstanceSession{}, ErrInstanceMissing
	}
	if err := m.joinLocked(s, character); err != nil {
		return InstanceSession{}, err
	}
	return s.snapshot(), nil
}

func (m *InstanceManager) joinLocked(s *instanceSession, character InstanceCharacter) error {
	if _, already := s.members[character]; !already && s.sim.dungeonCombat() {
		return ErrInstanceCombat
	}
	return m.joinRememberedLocked(s, character)
}

// Only a remembered authoritative visit may bypass new-admission combat checks.
func (m *InstanceManager) joinRememberedLocked(s *instanceSession, character InstanceCharacter) error {
	if id, inside := m.inside[character]; inside && id != s.id {
		return ErrCharacterInInstance
	}
	s.members[character] = struct{}{}
	s.emptyTicks = 0
	m.inside[character] = s.id
	m.visits[instanceVisit{s.ruin, character}] = s.id
	// Joining a run whose first boss is already dead binds on entry. Whether such an
	// entry is offered, warned about or refused at all is the entry-rules issue's; what
	// this owes it is that a character who does get in is bound by the same rule as the
	// party that was standing there when the boss fell.
	if s.state == InstanceSaved {
		m.bindLocked(s, character)
	}
	return nil
}

// Reenter rejoins this character's last copy of the ruin while it still exists,
// or creates a fresh private copy after expiry. It does not select another
// group's copy; a party entry policy explicitly calls Join for its chosen id.
func (m *InstanceManager) Reenter(ruin InstanceRuin, character InstanceCharacter) (InstanceSession, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.closed {
		return InstanceSession{}, ErrInstanceClosed
	}
	visit := instanceVisit{ruin, character}
	id := m.visits[visit]
	if current, inside := m.inside[character]; inside && current != id {
		return InstanceSession{}, ErrCharacterInInstance
	}
	s := m.sessions[id]
	if s == nil {
		var err error
		s, err = m.createLocked(ruin)
		if err != nil {
			return InstanceSession{}, err
		}
	}
	if err := m.joinLocked(s, character); err != nil {
		return InstanceSession{}, err
	}
	return s.snapshot(), nil
}

// Leave starts the grace on the last departure. Duplicate or stale departures
// do not restart it and cannot remove a character from a newer copy.
func (m *InstanceManager) Leave(id uint64, character InstanceCharacter) bool {
	m.mu.Lock()
	defer m.mu.Unlock()
	s := m.sessions[id]
	if s == nil {
		return false
	}
	if _, exists := s.members[character]; !exists {
		return false
	}
	delete(s.members, character)
	delete(m.inside, character)
	delete(m.portalEntries, character)
	// A crossing this character was still being offered belonged to the world they have
	// just left. Nothing about it is true from where they are standing now.
	m.forgetOfferLocked(character)
	if len(s.members) == 0 {
		s.emptyTicks = 0
	}
	return true
}

func (m *InstanceManager) Lookup(id uint64) (InstanceSession, bool) {
	m.mu.Lock()
	defer m.mu.Unlock()
	s := m.sessions[id]
	if s == nil {
		return InstanceSession{}, false
	}
	return s.snapshot(), true
}

func (m *InstanceManager) Count() int {
	m.mu.Lock()
	defer m.mu.Unlock()
	return len(m.sessions)
}

// Step advances every live simulation exactly once, then expires empty copies.
// Call once per authoritative loop callback, not with a wall-clock deadline.
// Instance clocks start at zero even when the open world is many days old.
func (m *InstanceManager) Step() {
	m.mu.Lock()
	defer m.mu.Unlock()
	// One reading for the whole pass, so two sessions that expire at the same midnight
	// cannot land on different sides of it because the loop took a moment.
	now := m.now().Unix()
	for id, s := range m.sessions {
		s.tick++
		s.sim.Step(s.tick)
		// Collected on the same tick the simulation is stepped, under this mutex, so the
		// membership a save binds is exactly the membership at the blow.
		m.collectBossDefeatsLocked(s)
		if len(s.members) != 0 {
			continue
		}
		// A saved run does not expire on the empty grace. It is theirs until its reset,
		// which is why emptyTicks stops meaning anything here rather than being allowed
		// to run on and mislead a later reader — and the reset is the wall clock's, not
		// this loop's. See instance_reset.go for why both halves of that are deliberate.
		if s.state == InstanceSaved {
			s.emptyTicks = 0
			if m.resetDueLocked(s, now) {
				m.removeLocked(id, s)
			}
			continue
		}
		s.emptyTicks++
		if s.emptyTicks >= m.graceTicks {
			m.removeLocked(id, s)
		}
	}
}

func (m *InstanceManager) removeLocked(id uint64, s *instanceSession) {
	s.cancel()
	// A remembered life is useful only while its instance can be resumed.
	// Teardown has already saved the safe open-world life; keeping a second
	// full inventory here would grow memory for characters who never return.
	for character, visit := range m.disconnected {
		if visit.session == id {
			delete(m.disconnected, character)
		}
	}
	for character := range s.members {
		delete(m.inside, character)
	}
	for visit, sessionID := range m.visits {
		if sessionID == id {
			delete(m.visits, visit)
		}
	}
	// A binding names a session, so it cannot outlive one. This is the whole of "expiry
	// releases every binding to that session": the midnight reset removes the session
	// through this one path, and a character bound to it is free for that ruin again on
	// the way out. See instance_reset.go.
	//
	// **Every deletion is collected before any of them is announced**, because the list a
	// character is told about is a complete one: announcing inside the loop would send a
	// list still holding a binding this same pass is about to remove. See
	// instance_bindings.go.
	var released []InstanceCharacter
	for visit, sessionID := range m.bound {
		if sessionID == id {
			delete(m.bound, visit)
			released = append(released, visit.character)
		}
	}
	for _, character := range released {
		m.announceBindingsLocked(character)
	}
	for visit, sessionID := range m.partyVisits {
		if sessionID == id {
			delete(m.partyVisits, visit)
		}
	}
	// An offer names a run, so it cannot outlive one either: the reset that releases
	// every binding to this session also withdraws every prompt about it, and an
	// acceptance arriving afterwards is refused as the unknown offer it now is.
	for character, offer := range m.offers {
		if offer.session == id {
			delete(m.offers, character)
		}
	}
	delete(m.sessions, id)
	s.sim, s.chunks, s.members = nil, nil, nil
}

// Close is idempotent and permanently refuses further creation or admission.
// The server calls it after its tick and session workers have stopped.
func (m *InstanceManager) Close() {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.closed = true
	clear(m.disconnected)
	clear(m.portalEntries)
	clear(m.offers)
	// Every watcher goes before the sessions do. A shutdown is not a reset: nobody's
	// lockout ends because the server stopped, and a connection told its list was empty
	// on the way down would be told something the next start contradicts.
	clear(m.bindingWatchers)
	for id, s := range m.sessions {
		m.removeLocked(id, s)
	}
}

func (s *instanceSession) snapshot() InstanceSession {
	members := make([]InstanceCharacter, 0, len(s.members))
	for character := range s.members {
		members = append(members, character)
	}
	return InstanceSession{ID: s.id, Seed: s.seed, Ruin: s.ruin, State: s.state,
		Members: members, DefeatedBosses: append([]vnet.MobKind(nil), s.defeated...),
		ExpiresUnix: s.expiresUnix,
		Sim:         s.sim, Chunks: s.chunks, Context: s.ctx}
}
