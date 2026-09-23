package main

import (
	"fmt"
	"sync"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// killWindow is how recently the bot must have hit a creature for its disappearance to be a
// kill; one that leaves the snapshot otherwise — out of view, or reset by a wipe — is not.
const killWindow = 3 * time.Second

// runStats is everything the report says, gathered as it happens.
type runStats struct {
	mu sync.Mutex

	phase    string
	phases   []phaseRecord
	deaths   map[string]int
	seen     map[vnet.MobKind]map[uint64]bool
	killed   map[vnet.MobKind]int
	hitAt    map[uint64]time.Time
	counted  map[uint64]bool
	taken    int
	commands []string
	// immortal is the time spent under /immortal, per phase, so the report can say how
	// much of the run a player would have had to survive on their own.
	immortal map[string]time.Duration
	// assists are the places the bot was stuck and /teleport moved it on.
	assists []string
	// portalPlacements are the /teleports that put the bot beside the open world's portal.
	portalPlacements int
	// unreachedMobs names every creature the bot gave up on because no walk reached it.
	unreachedMobs []string
}

type phaseRecord struct {
	name       string
	start, end time.Time
	deaths     int
}

func newRunStats() *runStats {
	return &runStats{
		deaths: map[string]int{}, seen: map[vnet.MobKind]map[uint64]bool{}, killed: map[vnet.MobKind]int{},
		hitAt: map[uint64]time.Time{}, counted: map[uint64]bool{}, immortal: map[string]time.Duration{},
	}
}

func (s *runStats) begin(name string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	now := time.Now()
	if n := len(s.phases); n > 0 && s.phases[n-1].end.IsZero() {
		s.phases[n-1].end = now
	}
	s.phase = name
	s.phases = append(s.phases, phaseRecord{name: name, start: now})
}

func (s *runStats) finish() {
	s.mu.Lock()
	defer s.mu.Unlock()
	if n := len(s.phases); n > 0 && s.phases[n-1].end.IsZero() {
		s.phases[n-1].end = time.Now()
	}
}

func (s *runStats) died() {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.deaths[s.phase]++
	if n := len(s.phases); n > 0 {
		s.phases[n-1].deaths++
	}
}

func (s *runStats) deathsIn(phase string) int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.deaths[phase]
}

func (s *runStats) blowLanded(target uint64) {
	s.mu.Lock()
	s.hitAt[target] = time.Now()
	s.mu.Unlock()
}

func (s *runStats) blowTaken() {
	s.mu.Lock()
	s.taken++
	s.mu.Unlock()
}

func (s *runStats) command(line string) {
	s.mu.Lock()
	s.commands = append(s.commands, line)
	s.mu.Unlock()
}

func (s *runStats) assist(where string) {
	s.mu.Lock()
	s.assists = append(s.assists, where)
	s.mu.Unlock()
}

func (s *runStats) addImmortal(phase string, d time.Duration) {
	s.mu.Lock()
	s.immortal[phase] += d
	s.mu.Unlock()
}

func (s *runStats) sawMob(m mobView) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.seen[m.kind] == nil {
		s.seen[m.kind] = map[uint64]bool{}
	}
	s.seen[m.kind][m.id] = true
	if m.dying() {
		s.countKillLocked(m)
	}
}

// lostMob is a creature the snapshot stopped carrying: a kill when the bot hit it within
// killWindow and is alive, since a wipe discards creatures only once the party is dead.
func (s *runStats) lostMob(m mobView, alive bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if hit, ok := s.hitAt[m.id]; ok && alive && time.Since(hit) < killWindow {
		s.countKillLocked(m)
	}
}

func (s *runStats) countKillLocked(m mobView) {
	if s.counted[m.id] {
		return
	}
	if _, hit := s.hitAt[m.id]; !hit {
		return
	}
	s.counted[m.id] = true
	s.killed[m.kind]++
}

func (s *runStats) killedCount(kind vnet.MobKind) int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.killed[kind]
}

func (s *runStats) isKilled(id uint64) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.counted[id]
}

func (s *runStats) lastHit(id uint64) time.Time {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.hitAt[id]
}

func (s *runStats) currentPhase() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.phase
}

func (s *runStats) unreached(m mobView) {
	s.mu.Lock()
	s.unreachedMobs = append(s.unreachedMobs, fmt.Sprintf("%s in %s at (%.1f, %.1f, %.1f)",
		vnet.EnumNamesMobKind[m.kind], s.phase, m.pos[0], m.pos[1], m.pos[2]))
	s.mu.Unlock()
}

func (s *runStats) portalPlacement() {
	s.mu.Lock()
	s.portalPlacements++
	s.mu.Unlock()
}
