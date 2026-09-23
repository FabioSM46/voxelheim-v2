package game

import (
	"errors"
	"reflect"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The route's checkpoints and what a restart keeps of a run (dungeon_route.go).

// routeCheckpoints is the seed's checkpoint slots in route order.
func routeCheckpoints(t *testing.T, seed int64) []world.PlacedAnchor {
	t.Helper()
	out := make([]world.PlacedAnchor, 3)
	n := 0
	for _, a := range world.InstanceDungeonAnchors(seed) {
		if a.Kind == world.AnchorInstanceCheckpoint {
			out[a.Index] = a
			n++
		}
	}
	if n != 3 {
		t.Fatalf("seed %d has %d checkpoints, want 3", seed, n)
	}
	return out
}

// joinDelver joins a player standing at pos into a dungeon simulation.
func joinDelver(t *testing.T, s *Sim, n uint64, pos [3]float64) *Player {
	t.Helper()
	spawn := [3]float32{float32(pos[0]), float32(pos[1]), float32(pos[2])}
	p, err := s.JoinCharacter(s.mintEntityID(), testPlayerID(n), 1, "Delver", spawn, testAppearance(), nil, func([]byte) bool { return true })
	if err != nil {
		t.Fatal(err)
	}
	return p
}

// dieAndRespawn kills p and brings it back at once, answering where it came back.
func dieAndRespawn(s *Sim, p *Player) [3]float64 {
	s.mu.Lock()
	defer s.mu.Unlock()
	p.dieLocked()
	p.respawnLocked()
	return p.pos
}

// place stands p at pos, as the simulation's own teleport would.
func place(s *Sim, p *Player, pos [3]float64) {
	s.mu.Lock()
	defer s.mu.Unlock()
	p.pos, p.vel = pos, [3]float64{}
	p.chunk = chunkAt(p.pos)
}

func reached(s *Sim) int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.dungeon.checkpoints.reached
}

// Every checkpoint lies under the chasm and each later one past the one before, at every
// rotation — the geometry the reach rule in dungeon_route.go stands on.
func TestTheCheckpointsLieUnderTheChasmInRouteOrder(t *testing.T) {
	for seed := int64(0); seed < 4; seed++ {
		cps := routeCheckpoints(t, seed)
		arrival, _ := world.InstanceAnchors(seed)
		c := dungeonCheckpoints{anchors: cps}
		if c.passed(0, anchorStanding(arrival)) {
			t.Fatalf("seed %d: the arrival court counts as under the chasm", seed)
		}
		for k, a := range cps {
			if !c.passed(0, anchorStanding(a)) {
				t.Fatalf("seed %d: checkpoint %d is above the chasm", seed, k)
			}
			if k > 0 && !c.passed(k, anchorStanding(a)) {
				t.Fatalf("seed %d: standing on checkpoint %d does not pass it", seed, k)
			}
		}
		// The reach is the passage's half-width on each axis and a course of height.
		centre := anchorStanding(cps[1])
		for _, tc := range []struct {
			dx, dy, dz float64
			in         bool
		}{
			{1.4, 0, -1.4, true}, {-1.5, 1, 1.5, true}, {1.6, 0, 0, false}, {0, 0, -1.6, false},
			{0, 1.6, 0, false}, {0, -.6, 0, false},
		} {
			pos := [3]float64{centre[0] + tc.dx, centre[1] + tc.dy, centre[2] + tc.dz}
			if c.passed(1, pos) != tc.in {
				t.Fatalf("seed %d: offset %+v passed=%v, want %v", seed, tc, !tc.in, tc.in)
			}
		}
	}
}

// A death comes back where the party has got to: in place before anybody has come down,
// on the shore the moment one member is under the chasm — even for a member who died
// above it — and at each checkpoint a live member passes after that. A dead body passes
// nothing, and nothing moves anybody who is alive.
func TestADungeonDeathComesBackAtTheFurthestCheckpointReached(t *testing.T) {
	for seed := int64(0); seed < 4; seed++ {
		m := instanceTestManager(t, 20, 1)
		session, err := m.Reenter(InstanceRuin{CellX: seed}, instanceTestCharacter(1))
		if err != nil {
			t.Fatal(err)
		}
		loadDungeon(t, session)
		killMobInSession(t, session, vnet.MobKindVargrGuardian)
		m.Step()
		s := session.Sim
		cps := routeCheckpoints(t, session.Seed)
		arrival, _ := world.InstanceAnchors(session.Seed)
		court := anchorStanding(arrival)

		straggler := joinDelver(t, s, 1, court)
		leader := joinDelver(t, s, 2, court)
		m.Step()

		fell := straggler.pos
		if got := dieAndRespawn(s, straggler); got != fell || reached(s) != 0 {
			t.Fatalf("seed %d: a death before the descent came back at %v, want in place at %v", seed, got, fell)
		}

		place(s, leader, anchorStanding(cps[0]))
		m.Step()
		if reached(s) != 1 {
			t.Fatalf("seed %d: coming down reached %d checkpoints, want the shore", seed, reached(s))
		}
		// The straggler is still in the court and may still drop: nothing moved them, and
		// the trapdoor the guardian opened is still open.
		place(s, straggler, court)
		for range 40 {
			m.Step()
		}
		if straggler.pos[0] != court[0] || straggler.pos[2] != court[2] {
			t.Fatalf("seed %d: the descent moved a player still in the court to %v", seed, straggler.pos)
		}
		_, _, trapdoor := world.InstanceEncounterAnchors(session.Seed)
		if s.terrain.Solid(trapdoor.X, trapdoor.Y, trapdoor.Z) {
			t.Fatalf("seed %d: the chasm shut behind the first member down", seed)
		}
		if got := dieAndRespawn(s, straggler); got != anchorStanding(cps[0]) {
			t.Fatalf("seed %d: a death above the chasm after the descent came back at %v, want the shore", seed, got)
		}

		// A dead body at the next checkpoint passes nothing.
		s.mu.Lock()
		leader.dieLocked()
		s.mu.Unlock()
		place(s, leader, anchorStanding(cps[1]))
		m.Step()
		if reached(s) != 1 {
			t.Fatalf("seed %d: a dead body passed checkpoint 1", seed)
		}
		s.mu.Lock()
		leader.respawnLocked()
		s.mu.Unlock()

		for k := 1; k < len(cps); k++ {
			place(s, leader, anchorStanding(cps[k]))
			m.Step()
			if reached(s) != k+1 {
				t.Fatalf("seed %d: passing checkpoint %d reached %d", seed, k, reached(s))
			}
			if got := dieAndRespawn(s, straggler); got != anchorStanding(cps[k]) {
				t.Fatalf("seed %d: a death after checkpoint %d came back at %v", seed, k, got)
			}
		}

		// Walking back up takes nothing away.
		place(s, leader, court)
		m.Step()
		if reached(s) != len(cps) {
			t.Fatalf("seed %d: walking back to the court lost checkpoints (%d)", seed, reached(s))
		}
	}
}

// The whole route survives a restart: the checkpoints reached, the puzzles solved for
// good — their doors open and their mechanisms showing it — and the groups cleared, with
// the cave's waves spent. The king's shortcut is restored open beside them, and the grille,
// which no party solves for good, is shut. The rebuilt run writes the same route back.
func TestARestoredRunKeepsItsRoute(t *testing.T) {
	at := time.Date(2026, 3, 14, 21, 0, 0, 0, time.UTC)
	m, _, _, _, session := savedRunAt(t, at)
	loadDungeon(t, session)
	s := session.Sim

	s.mu.Lock()
	p := s.dungeon.puzzles
	for _, stone := range world.InstanceRuneOrder(session.Seed) {
		if _, err := p.use(mechanismRef{world.RunePuzzle, stone}); err != nil {
			t.Fatal(err)
		}
	}
	for _, ref := range []mechanismRef{{world.GrillePuzzle, 0}, {world.TwinLeverPuzzle, 0}, {world.TwinLeverPuzzle, 1}} {
		if _, err := p.use(ref); err != nil {
			t.Fatal(err)
		}
	}
	for _, group := range []int{0, world.SandBuriedGroup} {
		for _, id := range s.dungeon.descent.groups[group] {
			mob := s.mobs[id]
			mob.buried = false
			if !s.damageMobLocked(mob, mob.health) {
				t.Fatalf("a creature of group %d did not die", group)
			}
		}
	}
	s.dungeon.descent.waves.started, s.dungeon.descent.waves.next = true, len(spiderWaveSizes)
	s.dungeon.checkpoints.reached = 2
	s.mu.Unlock()
	killMobInSession(t, session, vnet.MobKindDraugrKing)
	m.Step()

	saved := m.SavedSessions()
	want := DungeonRoute{Checkpoints: 2, SolvedPuzzles: []uint8{1, 3}, ClearedGroups: []uint8{0, world.CaveBurrowGroup, world.SandBuriedGroup}}
	if len(saved) != 1 || !reflect.DeepEqual(saved[0].Route, want) {
		t.Fatalf("the saved route is %#v, want %#v", saved, want)
	}

	again, restored, _ := restartInto(t, saved, at)
	if restored != 1 {
		t.Fatal("the run was not restored")
	}
	live, _ := again.Lookup(session.ID)
	loadDungeon(t, live)
	r := live.Sim
	d := &puzzleDungeon{t: t, s: r}
	for _, door := range []int{world.RunePuzzle, world.TwinLeverPuzzle, world.ReturnShortcutDoor} {
		if !d.doorOpen(door) {
			t.Fatalf("door %d came back shut", door)
		}
	}
	if d.doorOpen(world.GrillePuzzle) {
		t.Fatal("the grille came back open")
	}
	for _, a := range d.mechanisms(world.RunePuzzle) {
		if d.block(a) != world.RuneStoneLit {
			t.Fatalf("a solved rune stone came back %v", d.block(a))
		}
	}
	for _, a := range d.mechanisms(world.TwinLeverPuzzle) {
		if d.block(a) != world.LeverOn {
			t.Fatalf("a solved twin lever came back %v", d.block(a))
		}
	}
	r.mu.Lock()
	for _, ref := range []mechanismRef{{world.RunePuzzle, 0}, {world.TwinLeverPuzzle, 1}} {
		if _, err := r.dungeon.puzzles.use(ref); !errors.Is(err, errMechanismLocked) {
			t.Fatalf("a solved mechanism %+v answered %v after the restart", ref, err)
		}
	}
	desc := &r.dungeon.descent
	for group, n := range map[int]int{0: 0, 1: 4, 2: 4, 3: 4, world.SandBuriedGroup: 0} {
		if len(desc.groups[group]) != n {
			t.Fatalf("group %d came back with %d creatures, want %d", group, len(desc.groups[group]), n)
		}
	}
	cave := triggerCentre(t, r, world.CaveTrigger)
	if r.advanceDungeonDescentLocked(1, []*Player{delver(1, cave)}) || len(desc.groups[world.CaveBurrowGroup]) != 0 {
		t.Fatal("a cleared cave sent its waves again")
	}
	r.mu.Unlock()
	cps := routeCheckpoints(t, live.Seed)
	arrival, _ := world.InstanceAnchors(live.Seed)
	late := joinDelver(t, r, 7, anchorStanding(arrival))
	if got := dieAndRespawn(r, late); got != anchorStanding(cps[1]) {
		t.Fatalf("a death in the restored run came back at %v, want checkpoint 1", got)
	}
	if back := again.SavedSessions(); len(back) != 1 || !reflect.DeepEqual(back[0].Route, want) {
		t.Fatalf("the restored run writes back %#v, want %#v", back, want)
	}
}
