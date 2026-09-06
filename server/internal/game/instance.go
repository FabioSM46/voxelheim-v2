package game

import (
	"context"
	"errors"
	"log/slog"
	"sync"
	"time"

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
	InstanceFree  InstanceState = iota
	InstanceSaved               // Reserved vocabulary; no API in this lifecycle can save a session.
)

// InstanceSession is a snapshot. Members is copied; editing it changes no state.
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
	Sim     *Sim
	Chunks  *world.Cache
	Context context.Context
}

type instanceSession struct {
	id         uint64
	seed       int64
	ruin       InstanceRuin
	members    map[InstanceCharacter]struct{}
	sim        *Sim
	chunks     *world.Cache
	ctx        context.Context
	cancel     context.CancelFunc
	emptyTicks uint32
	tick       uint64
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
	log                    *slog.Logger
	options                []SimOption
	graceTicks             uint32
	closed                 bool
	sessions               map[uint64]*instanceSession
	inside                 map[InstanceCharacter]uint64
	visits                 map[instanceVisit]uint64
	partyVisits            map[portalPartyVisit]uint64
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
		mintEntityID: mintEntityID, log: log, options: append([]SimOption(nil), options...),
		graceTicks: ticksFor(InstanceEmptyGrace, tickRate),
		sessions:   make(map[uint64]*instanceSession), inside: make(map[InstanceCharacter]uint64), visits: make(map[instanceVisit]uint64), partyVisits: make(map[portalPartyVisit]uint64),
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
	id := m.mintEntityID()
	// A bijection of ids provides a fresh seed even for two copies of one ruin.
	seed := int64(id ^ 0x49a3d758c1e260bf)
	chunks := world.NewInstanceCache(seed, world.DefaultWorkers, 64)
	sim, err := NewSim(m.tickRate, m.viewDistance, seed, NewCacheTerrain(chunks), chunks, m.mintEntityID, m.log, m.options...)
	if err != nil {
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
	if id, inside := m.inside[character]; inside && id != s.id {
		return ErrCharacterInInstance
	}
	s.members[character] = struct{}{}
	s.emptyTicks = 0
	m.inside[character] = s.id
	m.visits[instanceVisit{s.ruin, character}] = s.id
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
	for id, s := range m.sessions {
		s.tick++
		s.sim.Step(s.tick)
		if len(s.members) != 0 {
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
	for character := range s.members {
		delete(m.inside, character)
	}
	for visit, sessionID := range m.visits {
		if sessionID == id {
			delete(m.visits, visit)
		}
	}
	for visit, sessionID := range m.partyVisits {
		if sessionID == id {
			delete(m.partyVisits, visit)
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
	for id, s := range m.sessions {
		m.removeLocked(id, s)
	}
}

func (s *instanceSession) snapshot() InstanceSession {
	members := make([]InstanceCharacter, 0, len(s.members))
	for character := range s.members {
		members = append(members, character)
	}
	return InstanceSession{ID: s.id, Seed: s.seed, Ruin: s.ruin, State: InstanceFree, Members: members, Sim: s.sim, Chunks: s.chunks, Context: s.ctx}
}
