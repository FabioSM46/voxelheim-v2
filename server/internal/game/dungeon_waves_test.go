package game

import (
	"fmt"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The dungeon's minor encounters, tick by tick: slots filled once at build, triggers
// that fire once, and the cave's three spider waves.

// dungeonPlacedMinors is how many creatures a fresh dungeon holds besides its bosses:
// the four floor-1 groups of four and the sand hall's eight scorpions.
const dungeonPlacedMinors = 16 + 8

// dungeonBossCount is how many boss-rank creatures a simulation holds.
func dungeonBossCount(s *Sim) int {
	n := 0
	for _, m := range s.mobs {
		if m.species().isBoss() {
			n++
		}
	}
	return n
}

// newWavesSim builds a fresh dungeon of one seed with every chunk resident.
func newWavesSim(t *testing.T, seed int64) *Sim {
	t.Helper()
	manager := instanceTestManager(t, 20, 1)
	manager.mu.Lock()
	raw, err := manager.newSessionLocked(100, seed, InstanceRuin{}, nil, DungeonRoute{})
	manager.mu.Unlock()
	if err != nil {
		t.Fatal(err)
	}
	session := raw.snapshot()
	loadDungeon(t, session)
	return session.Sim
}

// triggerCentre is the standing position in the middle of a trigger volume.
func triggerCentre(t *testing.T, s *Sim, index int) [3]float64 {
	t.Helper()
	for _, tr := range s.dungeon.descent.triggers {
		if tr.index == index {
			v := tr.volume
			return [3]float64{(v.min[0] + v.max[0]) / 2, v.min[1], (v.min[2] + v.max[2]) / 2}
		}
	}
	t.Fatalf("no trigger %d", index)
	return [3]float64{}
}

// delver is a live player at pos, handed to the descent directly: the triggers and
// the waves read positions and life and nothing else of a player.
func delver(id uint64, pos [3]float64) *Player {
	return &Player{entityID: id, lifeState: vnet.LifeStateAlive, pos: pos}
}

func TestDungeonPlacesEveryMinorSlotOnceLeashedToItsZone(t *testing.T) {
	for seed := int64(0); seed < 4; seed++ {
		s := newWavesSim(t, seed)
		desc := &s.dungeon.descent
		want := map[int]struct {
			kind   vnet.MobKind
			n      int
			buried bool
		}{
			0: {vnet.MobKindDraugr, 4, false}, 1: {vnet.MobKindDraugr, 4, false},
			2: {vnet.MobKindVargr, 4, false}, 3: {vnet.MobKindVargr, 4, false},
			world.SandBuriedGroup: {vnet.MobKindScorpion, 8, true},
		}
		for group, w := range want {
			ids := desc.groups[group]
			if len(ids) != w.n {
				t.Fatalf("seed %d group %d: %d placed, want %d", seed, group, len(ids), w.n)
			}
			for _, id := range ids {
				m := s.mobs[id]
				if m.kind != w.kind || m.buried != w.buried || m.leash == nil || m.leash.zone != desc.zones[group] {
					t.Fatalf("seed %d group %d: %+v", seed, group, m)
				}
				risen := m.pos
				if m.buried {
					risen[1] += burialDepth
				}
				if overlaps(s.terrain, m.species().body.boxAt(risen)) {
					t.Fatalf("seed %d group %d: a %s stands inside the scenery", seed, group, m.kind)
				}
			}
		}
		if len(desc.groups[world.CaveBurrowGroup]) != 0 || len(desc.waves.burrows) != 6 {
			t.Fatalf("seed %d: the burrows are filled at build or missing", seed)
		}

		// A killed creature stays killed: nothing ever refills its slot.
		victim := s.mobs[desc.groups[0][0]]
		if !s.damageMobLocked(victim, victim.health) {
			t.Fatal("the draugr did not die")
		}
		for tick := uint64(1); tick <= 2000; tick++ {
			s.directMobsLocked(tick, nil, s.sortedMobsLocked())
		}
		if len(s.mobs) != 2+dungeonPlacedMinors-1 || s.mobs[victim.entityID] != nil {
			t.Fatalf("seed %d: the dungeon refilled a slot (%d creatures)", seed, len(s.mobs))
		}
	}
}

func TestATriggerFiresOnceOnTheFirstLivePlayer(t *testing.T) {
	s := newWavesSim(t, 1)
	cave := triggerCentre(t, s, world.CaveTrigger)
	outside := cave
	outside[1] += 50

	dead := delver(1, cave)
	dead.lifeState = vnet.LifeStateDead
	s.advanceDungeonDescentLocked(1, []*Player{dead, delver(2, outside)})
	if s.dungeonTriggerFiredLocked(world.CaveTrigger) || s.dungeon.descent.waves.started {
		t.Fatal("a dead body or a player outside fired the cave's trigger")
	}

	s.advanceDungeonDescentLocked(2, []*Player{delver(2, cave)})
	if !s.dungeonTriggerFiredLocked(world.CaveTrigger) || s.dungeonTriggerFiredLocked(world.SandTrigger) {
		t.Fatal("the cave's trigger did not fire alone")
	}
	w := &s.dungeon.descent.waves
	if !w.started || w.next != 1 {
		t.Fatalf("the waves did not begin on the trigger: %+v", w)
	}

	// Walking out and back in fires nothing again.
	due := w.due
	s.advanceDungeonDescentLocked(3, []*Player{delver(2, outside)})
	s.advanceDungeonDescentLocked(4, []*Player{delver(2, cave)})
	if w.next != 1 || w.due != due {
		t.Fatal("re-entering the cave restarted its waves")
	}

	s.advanceDungeonDescentLocked(5, []*Player{delver(2, triggerCentre(t, s, world.SandTrigger))})
	if !s.dungeonTriggerFiredLocked(world.SandTrigger) {
		t.Fatal("the sand hall's trigger did not fire")
	}
}

// spidersOf is the positions of one wave's spiders.
func spidersOf(t *testing.T, s *Sim, ids []uint64) [][3]float64 {
	t.Helper()
	out := make([][3]float64, 0, len(ids))
	for _, id := range ids {
		m := s.mobs[id]
		if m == nil || m.kind != vnet.MobKindCaveSpider || m.leash.zone != s.dungeon.descent.zones[world.CaveBurrowGroup] {
			t.Fatalf("wave member %d is not a leashed cave spider", id)
		}
		out = append(out, m.pos)
	}
	return out
}

// The first three waves come on the interval with nobody killing anything, and the fourth
// is held: fifteen spiders are out, the cap.
func TestTheFirstSpiderWavesComeInOrderFromTheBurrowsUpToTheCap(t *testing.T) {
	for seed := int64(0); seed < 4; seed++ {
		s := newWavesSim(t, seed)
		// A full party, which meets every wave at its full size (dungeon_balance.go).
		cave := triggerCentre(t, s, world.CaveTrigger)
		party := []*Player{delver(7, cave), delver(8, cave), delver(9, cave), delver(10, cave)}
		interval := uint64(ticksFor(spiderWaveInterval, 20))
		burrows := map[[3]float64]bool{}
		for _, b := range s.dungeon.descent.waves.burrows {
			burrows[anchorStanding(b)] = true
		}

		const start = 100
		released := map[uint64]int{}
		for tick := uint64(start); tick < start+5*interval; tick++ {
			before := len(s.dungeon.descent.groups[world.CaveBurrowGroup])
			if s.advanceDungeonDescentLocked(tick, party) {
				released[tick] = len(s.dungeon.descent.groups[world.CaveBurrowGroup]) - before
			}
		}
		want := map[uint64]int{start: 4, start + interval: 5, start + 2*interval: 6}
		if fmt.Sprint(released) != fmt.Sprint(want) {
			t.Fatalf("seed %d: waves released %v, want %v", seed, released, want)
		}
		all := s.dungeon.descent.groups[world.CaveBurrowGroup]
		used := map[[3]float64]int{}
		for _, pos := range spidersOf(t, s, all) {
			if !burrows[pos] {
				t.Fatalf("seed %d: a spider came out at %v, not a burrow", seed, pos)
			}
			used[pos]++
		}
		// Fifteen spiders round the six burrows: every burrow used, none more than thrice.
		for pos, n := range used {
			if n < 2 || n > 3 {
				t.Fatalf("seed %d: burrow %v released %d spiders", seed, pos, n)
			}
		}
		if len(used) != 6 || len(s.mobs) != dungeonMobCeiling {
			t.Fatalf("seed %d: %d burrows used, %d creatures", seed, len(used), len(s.mobs))
		}
	}
}

func TestAClearedWaveBringsTheNextAfterABreather(t *testing.T) {
	s := newWavesSim(t, 2)
	party := []*Player{delver(7, triggerCentre(t, s, world.CaveTrigger))}
	breather := uint64(ticksFor(spiderWaveBreather, 20))
	s.advanceDungeonDescentLocked(10, party)
	for _, id := range s.dungeon.descent.waves.current {
		m := s.mobs[id]
		if !s.damageMobLocked(m, m.health) {
			t.Fatal("a spider did not die")
		}
	}
	for tick := uint64(11); tick < 11+breather; tick++ {
		if s.advanceDungeonDescentLocked(tick, party) {
			t.Fatalf("the second wave came at tick %d, before the breather", tick)
		}
	}
	if !s.advanceDungeonDescentLocked(11+breather, party) || s.dungeon.descent.waves.next != 2 {
		t.Fatal("a cleared wave did not bring the next one early")
	}
	// A wave with a spider still alive keeps the full interval.
	for tick := 12 + breather; tick < 11+breather+uint64(ticksFor(spiderWaveInterval, 20)); tick++ {
		if s.advanceDungeonDescentLocked(tick, party) {
			t.Fatalf("the third wave came at tick %d with the second still alive", tick)
		}
	}
}

// A wipe puts the waves back to before the first: every spider out goes back into the
// walls, and the waves come again only when a live player walks back into the cavern —
// not while the party stands anywhere else, such as the shore they respawn on.
func TestAWipeRestartsTheWaves(t *testing.T) {
	s := newWavesSim(t, 3)
	cave := triggerCentre(t, s, world.CaveTrigger)
	s.advanceDungeonDescentLocked(10, []*Player{delver(7, cave)})
	if len(s.dungeon.descent.groups[world.CaveBurrowGroup]) == 0 {
		t.Fatal("the first wave did not come")
	}
	dead := delver(7, cave)
	dead.lifeState = vnet.LifeStateDead
	if !s.advanceDungeonDescentLocked(11, []*Player{dead}) {
		t.Fatal("the wipe changed no creature")
	}
	w := &s.dungeon.descent.waves
	if w.started || w.next != 0 || len(s.dungeon.descent.groups[world.CaveBurrowGroup]) != 0 || s.dungeonTriggerFiredLocked(world.CaveTrigger) {
		t.Fatalf("the wipe did not put the waves back: %+v", w)
	}
	for _, m := range s.mobs {
		if m.kind == vnet.MobKindCaveSpider {
			t.Fatal("a spider stayed out after the wipe")
		}
	}
	elsewhere := cave
	elsewhere[1] += 50
	for tick := uint64(12); tick < 2000; tick++ {
		if s.advanceDungeonDescentLocked(tick, []*Player{delver(7, elsewhere)}) {
			t.Fatalf("a wave came at tick %d with nobody in the cavern", tick)
		}
	}
	if !s.advanceDungeonDescentLocked(2000, []*Player{delver(7, cave)}) || w.next != 1 {
		t.Fatal("walking back into the cavern did not start the waves again")
	}
}

// An empty instance is a party that disconnected, not one that lost: the waves wait
// for it rather than stopping, and the overdue wave comes out when somebody is back.
func TestAnEmptyInstancePausesTheWavesRatherThanStoppingThem(t *testing.T) {
	s := newWavesSim(t, 3)
	cave := triggerCentre(t, s, world.CaveTrigger)
	s.advanceDungeonDescentLocked(10, []*Player{delver(7, cave)})
	interval := uint64(ticksFor(spiderWaveInterval, 20))
	for tick := uint64(11); tick < 10+3*interval; tick++ {
		if s.advanceDungeonDescentLocked(tick, nil) {
			t.Fatalf("a wave came at tick %d with nobody inside", tick)
		}
	}
	w := &s.dungeon.descent.waves
	if !w.started || w.next != 1 {
		t.Fatalf("an empty instance ended the waves: %+v", w)
	}
	if !s.advanceDungeonDescentLocked(10+3*interval, []*Player{delver(7, cave)}) || w.next != 2 {
		t.Fatal("the overdue wave did not come out when the party returned")
	}
}

func TestADungeonGroupIsClearedOnlyWhenEveryMemberIsDead(t *testing.T) {
	s := newWavesSim(t, 0)
	group := s.dungeon.descent.groups[2]
	for i, id := range group {
		if s.dungeonGroupClearedLocked(2) {
			t.Fatalf("group 2 cleared with %d of %d alive", len(group)-i, len(group))
		}
		m := s.mobs[id]
		s.damageMobLocked(m, m.health)
	}
	if !s.dungeonGroupClearedLocked(2) {
		t.Fatal("an emptied group is not cleared")
	}
	// The burrows are cleared only once every wave has come out and died.
	if s.dungeonGroupClearedLocked(world.CaveBurrowGroup) {
		t.Fatal("the burrows are cleared before any wave came")
	}
}

// With every creature the dungeon can hold alive at once and four players among the
// spiders, the population sits exactly at its ceiling and every snapshot within budget.
func TestTheDungeonHoldsItsMobAndSnapshotBudget(t *testing.T) {
	// The largest snapshot one viewer may be sent here: the whole cave's population in
	// view, the other three players and their vitals, well inside one ordinary frame.
	const snapshotBudget = 8 << 10

	s := newWavesSim(t, 1)
	cave := triggerCentre(t, s, world.CaveTrigger)
	sizes := map[uint64]int{}
	latest := map[uint64][]byte{}
	for i := range uint64(4) {
		id := 900 + i
		spawn := [3]float32{float32(cave[0]) + float32(i%2), float32(cave[1]), float32(cave[2]) + float32(i/2)}
		p, err := s.Join(id, testPlayerID(id), fmt.Sprintf("Delver %d", i), spawn, testAppearance(), nil, func([]byte) bool { return true })
		if err != nil {
			t.Fatal(err)
		}
		p.deliverSnapshot = func(frame []byte, _ world.Column, _ [][]byte) bool {
			sizes[id] = max(sizes[id], len(frame))
			latest[id] = frame
			return true
		}
	}

	// Every wave the cap lets out before the first spider can reach anybody: the schedule
	// is pulled forward so the largest population is the one measured.
	peak := 0
	for tick := uint64(1); tick <= 20; tick++ {
		s.mu.Lock()
		if w := &s.dungeon.descent.waves; w.started {
			w.due = min(w.due, tick)
		}
		s.mu.Unlock()
		s.Step(tick)
		s.mu.Lock()
		peak = max(peak, len(s.mobs))
		s.mu.Unlock()
	}
	if peak != dungeonMobCeiling {
		t.Fatalf("the dungeon peaked at %d creatures, want its ceiling %d", peak, dungeonMobCeiling)
	}
	// What was measured has the waves in it: every spider is in every viewer's view.
	for id, frame := range latest {
		if seen := newestSnapshot(t, &dropSink{frames: [][]byte{frame}}).MobsLength(); seen < 4+5+6 {
			t.Fatalf("viewer %d saw %d creatures at the peak, fewer than the waves", id, seen)
		}
	}
	for tick := uint64(21); tick <= 400; tick++ {
		s.Step(tick)
		s.mu.Lock()
		n := len(s.mobs)
		s.mu.Unlock()
		if n > dungeonMobCeiling {
			t.Fatalf("tick %d: %d creatures, over the ceiling %d", tick, n, dungeonMobCeiling)
		}
	}
	if len(sizes) != 4 {
		t.Fatalf("%d of four viewers were sent a snapshot", len(sizes))
	}
	for id, size := range sizes {
		if size > snapshotBudget {
			t.Errorf("viewer %d was sent a %d-byte snapshot, over the %d budget", id, size, snapshotBudget)
		}
	}
	t.Logf("largest snapshots: %v", sizes)
}

// Past the cap a wave waits, overdue, until enough of the spiders out have died; then it
// comes on the next tick.
func TestAWaveWaitsWhileTheCapIsFull(t *testing.T) {
	s := newWavesSim(t, 0)
	party := partyAt(4, triggerCentre(t, s, world.CaveTrigger))
	interval := uint64(ticksFor(spiderWaveInterval, 20))
	tick := uint64(1)
	for ; tick < 1+3*interval; tick++ {
		s.advanceDungeonDescentLocked(tick, party)
	}
	w := &s.dungeon.descent.waves
	if w.next != 3 || s.spidersOutLocked() != spiderWaveCap {
		t.Fatalf("%d waves and %d spiders out, want 3 and the cap", w.next, s.spidersOutLocked())
	}
	for ; tick < 1+10*interval; tick++ {
		if s.advanceDungeonDescentLocked(tick, party) {
			t.Fatalf("a wave came at tick %d past the cap", tick)
		}
	}
	// Four die, which is room for the fourth wave of four.
	for _, id := range s.dungeon.descent.groups[world.CaveBurrowGroup][:4] {
		m := s.mobs[id]
		s.damageMobLocked(m, m.health)
	}
	if !s.advanceDungeonDescentLocked(tick, party) || w.next != 4 || s.spidersOutLocked() != spiderWaveCap {
		t.Fatalf("the held wave did not come when there was room: %d waves, %d out", w.next, s.spidersOutLocked())
	}
}

// Every placed creature and every wave spider carries the dungeon's tier; the rows, and a
// creature the open world spawns, do not.
func TestTheDungeonsCreaturesCarryItsTier(t *testing.T) {
	s := newWavesSim(t, 1)
	s.advanceDungeonDescentLocked(1, partyAt(4, triggerCentre(t, s, world.CaveTrigger)))
	checked := 0
	for _, m := range s.mobs {
		if m.species().isBoss() {
			continue
		}
		def := m.species()
		// Widened on this side, so a wrap in the production multiply would show here.
		if !m.tiered || uint32(m.health) != uint32(def.maxHealth)*uint32(dungeonHealthTier) || m.maxHealth() != m.health ||
			uint32(m.attack().damage) != uint32(def.damage)*uint32(dungeonDamageTier) {
			t.Fatalf("a dungeon %s is not at the tier: %+v", m.kind, m)
		}
		checked++
	}
	if checked != dungeonPlacedMinors+spiderWaveSizes[0] {
		t.Fatalf("checked %d creatures", checked)
	}
	wild := &mob{kind: vnet.MobKindDraugr}
	if row, _ := mobByKind(vnet.MobKindDraugr); wild.maxHealth() != row.maxHealth || wild.attack().damage != row.damage {
		t.Fatal("an open-world draugr carries the dungeon's tier")
	}
}

// A tier over a row that would pass the uint16 ceiling clamps to it rather than wrapping.
func TestATierNeverWrapsItsRow(t *testing.T) {
	for _, tc := range []struct{ value, tier, want uint16 }{
		{72, 4, 288}, {16383, 4, 65532}, {16384, 4, 65535}, {60000, 2, 65535}, {0, 4, 0},
	} {
		if got := tierScaled(tc.value, tc.tier); got != tc.want {
			t.Fatalf("tierScaled(%d, %d) = %d, want %d", tc.value, tc.tier, got, tc.want)
		}
	}
}
