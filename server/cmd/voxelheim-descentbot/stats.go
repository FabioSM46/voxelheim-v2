package main

import (
	"fmt"
	"sync"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// killWindow is how recently a member must have hit a creature for its disappearance to be
// a kill; one that leaves the snapshot otherwise — out of view, or reset by a wipe — is not.
const killWindow = 3 * time.Second

// tally is what the party saw and killed, shared by every member so a creature two blades
// brought down is one kill and a member's route knows what another member has finished.
type tally struct {
	mu      sync.Mutex
	seen    map[vnet.MobKind]map[uint64]bool
	killed  map[vnet.MobKind]int
	hitAt   map[uint64]time.Time
	counted map[uint64]bool
}

func newTally() *tally {
	return &tally{
		seen: map[vnet.MobKind]map[uint64]bool{}, killed: map[vnet.MobKind]int{},
		hitAt: map[uint64]time.Time{}, counted: map[uint64]bool{},
	}
}

func (t *tally) countKillLocked(m mobView) {
	if t.counted[m.id] {
		return
	}
	if _, hit := t.hitAt[m.id]; !hit {
		return
	}
	t.counted[m.id] = true
	t.killed[m.kind]++
}

// runStats is everything the report says about one member, gathered as it happens. The
// creatures are the party's and live in the shared tally.
type runStats struct {
	mu sync.Mutex

	*tally

	// ownHit is when this member's own blows last landed on each creature: the pilot's
	// reach judgement, which another member's blows must not satisfy.
	ownHit   map[uint64]time.Time
	phase    string
	phases   []phaseRecord
	deaths   map[string]int
	taken    int
	commands []string
	// assists are the places the member was stuck and /teleport moved it on.
	assists []string
	// portalPlacements are the /teleports that put the member beside the open world's portal.
	portalPlacements int
	// unreachedMobs names every creature the member gave up on because no walk reached it.
	unreachedMobs []string
}

type phaseRecord struct {
	name       string
	start, end time.Time
	deaths     int
}

func newRunStats(t *tally) *runStats {
	return &runStats{tally: t, deaths: map[string]int{}, ownHit: map[uint64]time.Time{}}
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

func (s *runStats) totalDeaths() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	total := 0
	for _, n := range s.deaths {
		total += n
	}
	return total
}

func (s *runStats) blowLanded(target uint64) {
	now := time.Now()
	s.tally.mu.Lock()
	s.hitAt[target] = now
	s.tally.mu.Unlock()
	s.mu.Lock()
	s.ownHit[target] = now
	s.mu.Unlock()
}

func (s *runStats) lastOwnHit(id uint64) time.Time {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.ownHit[id]
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

func (s *runStats) sawMob(m mobView) {
	s.tally.mu.Lock()
	defer s.tally.mu.Unlock()
	if s.seen[m.kind] == nil {
		s.seen[m.kind] = map[uint64]bool{}
	}
	s.seen[m.kind][m.id] = true
	if m.dying() {
		s.countKillLocked(m)
	}
}

// lostMob is a creature the snapshot stopped carrying: a kill when a member hit it within
// killWindow and this member is alive, since a wipe discards creatures only once the whole
// party is dead.
func (s *runStats) lostMob(m mobView, alive bool) {
	s.tally.mu.Lock()
	defer s.tally.mu.Unlock()
	if hit, ok := s.hitAt[m.id]; ok && alive && time.Since(hit) < killWindow {
		s.countKillLocked(m)
	}
}

func (s *runStats) killedCount(kind vnet.MobKind) int {
	s.tally.mu.Lock()
	defer s.tally.mu.Unlock()
	return s.killed[kind]
}

func (s *runStats) isKilled(id uint64) bool {
	s.tally.mu.Lock()
	defer s.tally.mu.Unlock()
	return s.counted[id]
}

func (s *runStats) lastHit(id uint64) time.Time {
	s.tally.mu.Lock()
	defer s.tally.mu.Unlock()
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

// reached is whether this member began a part of the route.
func (s *runStats) reached(phase string) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	for _, p := range s.phases {
		if p.name == phase {
			return true
		}
	}
	return false
}

// killTotal is every kill the party has made.
func (s *runStats) killTotal() int {
	s.tally.mu.Lock()
	defer s.tally.mu.Unlock()
	total := 0
	for _, n := range s.killed {
		total += n
	}
	return total
}
