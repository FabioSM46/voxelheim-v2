package game

import (
	"context"
	"errors"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
	"io"
	"log/slog"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

func instanceTestManager(t *testing.T, rate uint8, limit int) *InstanceManager {
	t.Helper()
	m, err := NewInstanceManager(rate, 3, limit, testEntityIDs(), slog.New(slog.NewTextHandler(io.Discard, nil)))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(m.Close)
	return m
}
func instanceTestCharacter(id uint64) InstanceCharacter {
	return InstanceCharacter{PlayerID: testPlayerID(id), CharacterID: 1}
}
func TestInstanceReentryPreservesWorldUntilExactGraceBoundary(t *testing.T) {
	for _, rate := range []uint8{1, 20, 60, 255} {
		t.Run(time.Duration(rate).String(), func(t *testing.T) {
			m := instanceTestManager(t, rate, 2)
			ruin, character := InstanceRuin{3, -4}, instanceTestCharacter(1)
			first, err := m.Reenter(ruin, character)
			if err != nil {
				t.Fatal(err)
			}
			if first.State != InstanceFree || first.Ruin != ruin || len(first.Members) != 1 {
				t.Fatal("bad session metadata")
			}
			if err := first.Chunks.Apply(first.Context, 0, 2, 0, world.Stone, func(world.Block) error { return nil }); err != nil {
				t.Fatal(err)
			}
			if !m.Leave(first.ID, character) {
				t.Fatal("first leave refused")
			}
			for range 3 {
				m.Step()
			}
			expected := uint32(1800) * uint32(rate)
			if m.graceTicks != expected {
				t.Fatalf("grace = %d; want %d", m.graceTicks, expected)
			}
			// Full elapsed progression is covered below. Skip near the boundary here so
			// testing every supported rate does not spend millions of unrelated Sim steps.
			m.sessions[first.ID].emptyTicks = expected - 2
			m.Step()
			if m.Leave(first.ID, character) {
				t.Fatal("duplicate leave restarted grace")
			}
			again, err := m.Reenter(ruin, character)
			if err != nil {
				t.Fatal(err)
			}
			if again.ID != first.ID || again.Seed != first.Seed || again.Sim != first.Sim || again.Chunks != first.Chunks {
				t.Fatal("live state replaced")
			}
			block, err := again.Chunks.BlockAt(again.Context, 0, 2, 0)
			if err != nil || block != world.Stone {
				t.Fatal("edit lost", block, err)
			}
			m.Step()
			if m.sessions[first.ID].emptyTicks != 0 {
				t.Fatal("occupied grace advanced")
			}
			if !m.Leave(first.ID, character) {
				t.Fatal("second leave refused")
			}
			if m.sessions[first.ID].emptyTicks != 0 {
				t.Fatal("new grace did not reset")
			}
			m.sessions[first.ID].emptyTicks = expected - 1
			m.Step()
			if _, ok := m.Lookup(first.ID); ok {
				t.Fatal("survived exact expiry")
			}
			if !errors.Is(first.Context.Err(), context.Canceled) {
				t.Fatal("expiry did not cancel lifetime")
			}
			fresh, err := m.Reenter(ruin, character)
			if err != nil {
				t.Fatal(err)
			}
			if fresh.ID == first.ID || fresh.Seed == first.Seed || fresh.Chunks == first.Chunks || fresh.Sim == first.Sim {
				t.Fatal("expired world reused")
			}
			block, err = fresh.Chunks.BlockAt(fresh.Context, 0, 2, 0)
			if err != nil || block != world.Air {
				t.Fatal("fresh world inherited edit", block, err)
			}
			if fresh.Sim.WorldTick() != 0 || len(m.visits) != 1 {
				t.Fatal("expired history survived")
			}
		})
	}
}
func TestInstanceFullGraceUsesOnlySimulationSteps(t *testing.T) {
	m := instanceTestManager(t, 1, 1)
	s, err := m.Create(InstanceRuin{})
	if err != nil {
		t.Fatal(err)
	}
	for range 1799 {
		m.Step()
	}
	if _, ok := m.Lookup(s.ID); !ok {
		t.Fatal("expired early")
	}
	if s.Sim.WorldTick() != 1799 {
		t.Fatal("empty simulation did not tick")
	}
	m.Step()
	if m.Count() != 0 {
		t.Fatal("did not expire")
	}
	tick := s.Sim.WorldTick()
	m.Step()
	if s.Sim.WorldTick() != tick {
		t.Fatal("expired simulation still ticks")
	}
}
func TestInstancePrivateCopiesShareOneEntityCounter(t *testing.T) {
	var ids atomic.Uint64
	mint := func() uint64 { return ids.Add(1) }
	log := slog.New(slog.NewTextHandler(io.Discard, nil))
	cache := world.NewCache(1, 1, 1)
	open, err := NewSim(20, 3, 1, NewCacheTerrain(cache), cache, mint, log)
	if err != nil {
		t.Fatal(err)
	}
	m, err := NewInstanceManager(20, 3, 2, mint, log)
	if err != nil {
		t.Fatal(err)
	}
	defer m.Close()
	ruin := InstanceRuin{1, 2}
	a, err := m.Create(ruin)
	if err != nil {
		t.Fatal(err)
	}
	b, err := m.Create(ruin)
	if err != nil {
		t.Fatal(err)
	}
	if a.ID == b.ID || a.Seed == b.Seed || a.Sim == b.Sim || a.Chunks == b.Chunks {
		t.Fatal("copies share identity or world")
	}
	seen := map[uint64]bool{a.ID: true, b.ID: true}
	for _, sim := range []*Sim{a.Sim, b.Sim} {
		for id := range sim.mobs {
			if seen[id] {
				t.Fatal("boss reused an identity")
			}
			seen[id] = true
		}
	}
	for _, sim := range []*Sim{open, a.Sim, b.Sim, open, a.Sim, b.Sim} {
		id, ok := sim.spawnDrop(ItemStone, 1, [3]int64{0, 2, 0})
		if !ok || seen[id] {
			t.Fatalf("duplicate entity id %d", id)
		}
		seen[id] = true
	}
	if len(seen) != 12 || ids.Load() != 12 {
		t.Fatal("counter not singular")
	}
	one, two := instanceTestCharacter(1), instanceTestCharacter(2)
	if _, err := m.Join(a.ID, one); err != nil {
		t.Fatal(err)
	}
	if _, err := m.Join(b.ID, two); err != nil {
		t.Fatal(err)
	}
	m.Leave(a.ID, one)
	m.Leave(b.ID, two)
	for _, tc := range []struct {
		character InstanceCharacter
		id        uint64
	}{{one, a.ID}, {two, b.ID}} {
		got, err := m.Reenter(ruin, tc.character)
		if err != nil || got.ID != tc.id {
			t.Fatal("independent groups combined")
		}
	}
}
func TestInstanceOccupancyIsAtomicAndSnapshotsAreCopies(t *testing.T) {
	m := instanceTestManager(t, 20, 2)
	a, err := m.Create(InstanceRuin{1, 0})
	if err != nil {
		t.Fatal(err)
	}
	b, err := m.Create(InstanceRuin{2, 0})
	if err != nil {
		t.Fatal(err)
	}
	one, two := instanceTestCharacter(1), instanceTestCharacter(2)
	joined, err := m.Join(a.ID, one)
	if err != nil {
		t.Fatal(err)
	}
	joined.Members[0] = two
	joined, err = m.Join(a.ID, one)
	if err != nil || len(joined.Members) != 1 || joined.Members[0] != one {
		t.Fatal("join or snapshot broken")
	}
	if _, err := m.Join(b.ID, one); !errors.Is(err, ErrCharacterInInstance) {
		t.Fatal("double occupancy", err)
	}
	if _, err := m.Reenter(b.Ruin, one); !errors.Is(err, ErrCharacterInInstance) {
		t.Fatal("reentry ignored occupancy", err)
	}
	if _, err := m.Join(a.ID, two); err != nil {
		t.Fatal(err)
	}
	m.Leave(a.ID, one)
	m.Step()
	if m.sessions[a.ID].emptyTicks != 0 {
		t.Fatal("grace with someone inside")
	}
	if m.Leave(b.ID, two) {
		t.Fatal("wrong-world leave")
	}
	m.Leave(a.ID, two)
	m.Step()
	if m.sessions[a.ID].emptyTicks != 1 {
		t.Fatal("no grace on last leave")
	}
	if _, err := m.Join(999, one); !errors.Is(err, ErrInstanceMissing) {
		t.Fatal("missing session accepted", err)
	}
}
func TestInstanceLimitRefusesBeforeMintingOrAllocating(t *testing.T) {
	var ids atomic.Uint64
	m, err := NewInstanceManager(20, 3, 1, func() uint64 { return ids.Add(1) }, slog.New(slog.NewTextHandler(io.Discard, nil)))
	if err != nil {
		t.Fatal(err)
	}
	defer m.Close()
	s, err := m.Reenter(InstanceRuin{}, instanceTestCharacter(1))
	if err != nil {
		t.Fatal(err)
	}
	for range 10 {
		if _, err := m.Create(InstanceRuin{}); !errors.Is(err, ErrInstanceLimit) {
			t.Fatal("limit ignored", err)
		}
		if _, err := m.Reenter(InstanceRuin{}, instanceTestCharacter(2)); !errors.Is(err, ErrInstanceLimit) {
			t.Fatal("reentry bypassed cap", err)
		}
	}
	if ids.Load() != 3 || m.Count() != 1 || len(m.visits) != 1 {
		t.Fatal("refusal changed resources")
	}
	m.Leave(s.ID, instanceTestCharacter(1))
	if got, err := m.Reenter(InstanceRuin{}, instanceTestCharacter(1)); err != nil || got.ID != s.ID {
		t.Fatal("cap refused existing copy")
	}
}
func TestInstanceCloseCancelsConsumersAndDropsOwnedResources(t *testing.T) {
	m := instanceTestManager(t, 20, 3)
	sessions := make([]InstanceSession, 3)
	done := make(chan struct{}, 3)
	for i := range sessions {
		var err error
		sessions[i], err = m.Reenter(InstanceRuin{}, instanceTestCharacter(uint64(i+1)))
		if err != nil {
			t.Fatal(err)
		}
		s := sessions[i]
		if _, _, err := s.Chunks.Get(s.Context, world.Coord{}); err != nil {
			t.Fatal(err)
		}
		go func() { <-s.Context.Done(); done <- struct{}{} }()
	}
	m.Step()
	held := m.sessions[sessions[0].ID]
	m.Close()
	m.Close()
	for range sessions {
		select {
		case <-done:
		case <-time.After(time.Second):
			t.Fatal("consumer leaked")
		}
	}
	m.Step()
	if m.Count() != 0 || len(m.inside) != 0 || len(m.visits) != 0 || held.sim != nil || held.chunks != nil || held.members != nil {
		t.Fatal("retained state")
	}
	for _, s := range sessions {
		if s.Sim.WorldTick() != 1 {
			t.Fatal("closed instance ticks")
		}
		if _, ok := m.Lookup(s.ID); ok {
			t.Fatal("still routable")
		}
		if _, err := m.Join(s.ID, instanceTestCharacter(4)); !errors.Is(err, ErrInstanceClosed) {
			t.Fatal(err)
		}
	}
	if _, err := m.Create(InstanceRuin{}); !errors.Is(err, ErrInstanceClosed) {
		t.Fatal(err)
	}
	if _, err := m.Reenter(InstanceRuin{}, instanceTestCharacter(4)); !errors.Is(err, ErrInstanceClosed) {
		t.Fatal(err)
	}
}
func TestInstanceConcurrentAdmissionTickAndClose(t *testing.T) {
	m := instanceTestManager(t, 20, 8)
	var workers sync.WaitGroup
	for n := range 8 {
		workers.Add(1)
		go func() {
			defer workers.Done()
			character := instanceTestCharacter(uint64(n + 1))
			for range 30 {
				s, err := m.Reenter(InstanceRuin{}, character)
				if err != nil && !errors.Is(err, ErrInstanceClosed) {
					t.Error(err)
					return
				}
				m.Lookup(s.ID)
				m.Leave(s.ID, character)
				m.Step()
			}
		}()
	}
	workers.Add(1)
	go func() { defer workers.Done(); m.Close() }()
	workers.Wait()
	if m.Count() != 0 {
		t.Fatal("Close raced into live world")
	}
}
func TestInstanceManagerRejectsInvalidConfiguration(t *testing.T) {
	log := slog.New(slog.NewTextHandler(io.Discard, nil))
	for _, tc := range []struct {
		rate    uint8
		cap     int
		mint    func() uint64
		log     *slog.Logger
		options []SimOption
	}{
		{0, 1, testEntityIDs(), log, nil}, {20, 0, testEntityIDs(), log, nil}, {20, -1, testEntityIDs(), log, nil}, {20, 1, nil, log, nil}, {20, 1, testEntityIDs(), nil, nil}, {20, 1, testEntityIDs(), log, []SimOption{nil}},
	} {
		if m, err := NewInstanceManager(tc.rate, 3, tc.cap, tc.mint, tc.log, tc.options...); err == nil {
			m.Close()
			t.Fatal("invalid configuration accepted")
		}
	}
}
