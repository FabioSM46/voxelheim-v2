package game

import (
	"fmt"
	"slices"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The wipe rule and the party-size balance of the lesser creatures (dungeon_wipe.go,
// dungeon_balance.go).

// zoneCentre is a standing position in the middle of a group's zone, on its floor.
func zoneCentre(s *Sim, group int) [3]float64 {
	z := s.dungeon.descent.zones[group]
	return [3]float64{(z.min[0] + z.max[0]) / 2, z.min[1], (z.min[2] + z.max[2]) / 2}
}

// partyAt is n live players standing at pos.
func partyAt(n int, pos [3]float64) []*Player {
	party := make([]*Player, n)
	for i := range party {
		party[i] = delver(uint64(700+i), pos)
	}
	return party
}

func killAll(party []*Player) {
	for _, p := range party {
		p.lifeState = vnet.LifeStateDead
	}
}

// The share of a group or a wave each party size meets, as dungeon_balance.go tabulates.
func TestAPartyMeetsItsShareOfEverySlot(t *testing.T) {
	for _, tc := range []struct {
		slots int
		want  [6]int // members 0 (read as one) through 5 (read as four)
	}{
		{4, [6]int{1, 1, 2, 3, 4, 4}},
		{8, [6]int{2, 2, 4, 6, 8, 8}},
		{5, [6]int{2, 2, 3, 4, 5, 5}},
		{6, [6]int{2, 2, 3, 5, 6, 6}},
		{1, [6]int{1, 1, 1, 1, 1, 1}},
	} {
		for members, want := range tc.want {
			if got := minorShare(tc.slots, members); got != want {
				t.Fatalf("minorShare(%d, %d) = %d, want %d", tc.slots, members, got, want)
			}
		}
	}
}

// A group keeps the party's share the tick its zone wakes, the first slots declared, and
// a member who joins afterwards changes nothing; a wave is sized by who is inside as it
// comes out.
func TestAGroupAndAWaveAreTheirPartysShare(t *testing.T) {
	for members := 1; members <= 4; members++ {
		t.Run(fmt.Sprint(members), func(t *testing.T) {
			s := newWavesSim(t, int64(members))
			desc := &s.dungeon.descent
			// Groups 0 and 1 share the first hall, and wake together.
			if len(desc.groups[2]) != 4 {
				t.Fatal("the second hall was trimmed before anybody came")
			}
			for _, group := range []int{0, 1, 2, 3, world.SandBuriedGroup} {
				before := slices.Clone(desc.groups[group])
				if group == 1 || group == 3 {
					before = before[:0]
					for _, a := range world.InstanceDungeonAnchors(s.worldSeed) {
						if a.Kind == world.AnchorInstanceMinorSpawn && a.Index == group {
							before = append(before, 0)
						}
					}
					if want := minorShare(len(before), members); len(desc.groups[group]) != want {
						t.Fatalf("group %d, sharing a hall already woken, holds %d, want %d", group, len(desc.groups[group]), want)
					}
					continue
				}
				party := partyAt(members, zoneCentre(s, group))
				s.advanceDungeonDescentLocked(1, party)
				want := minorShare(len(before), members)
				if !slices.Equal(desc.groups[group], before[:want]) {
					t.Fatalf("group %d kept %v of %v, want the first %d", group, desc.groups[group], before, want)
				}
				for _, id := range before[want:] {
					if s.mobs[id] != nil {
						t.Fatalf("group %d kept a creature past its share", group)
					}
				}
				s.advanceDungeonDescentLocked(2, append(party, delver(800, zoneCentre(s, group))))
				if len(desc.groups[group]) != want {
					t.Fatalf("a late member changed group %d to %d", group, len(desc.groups[group]))
				}
			}
			cave := triggerCentre(t, s, world.CaveTrigger)
			s.advanceDungeonDescentLocked(10, partyAt(members, cave))
			if got, want := len(desc.waves.current), minorShare(spiderWaveSizes[0], members); got != want {
				t.Fatalf("the first wave brought %d spiders for %d members, want %d", got, members, want)
			}
		})
	}
}

// A wipe resets the zone the party fell in and nothing else: its dead stay dead, its
// survivors come back to their slots whole under fresh identities, a risen scorpion lies
// buried again, and every other zone, cleared group, boss and door is as it was.
func TestAWipeResetsOnlyTheZoneThePartyFellIn(t *testing.T) {
	s := newWavesSim(t, 0)
	desc := &s.dungeon.descent
	party := partyAt(4, zoneCentre(s, 0))
	s.advanceDungeonDescentLocked(1, party)

	hall := slices.Clone(desc.groups[0])
	killed := s.mobs[hall[0]]
	s.damageMobLocked(killed, killed.health)
	hurt := s.mobs[hall[1]]
	hurt.health = 1
	moved := s.mobs[hall[2]]
	home := moved.pos
	moved.pos[0] += 2
	untouched := s.mobs[hall[3]]

	// The second hall, where nobody fell: groups 2 and 3.
	other := s.mobs[desc.groups[2][0]]
	other.health = 1
	for _, id := range desc.groups[3] {
		m := s.mobs[id]
		s.damageMobLocked(m, m.health)
	}
	guardian := s.dungeon.guardianID

	killAll(party)
	if !s.advanceDungeonDescentLocked(2, party) {
		t.Fatal("the wipe changed no creature")
	}
	if s.mobs[killed.entityID] != nil || desc.groups[0][0] != killed.entityID {
		t.Fatal("the wipe brought back a creature the party had killed")
	}
	for i, old := range []*mob{hurt, moved, untouched} {
		id := desc.groups[0][i+1]
		fresh := s.mobs[id]
		if s.mobs[old.entityID] != nil || fresh == nil || id == old.entityID {
			t.Fatalf("survivor %d kept its identity through the wipe", i)
		}
		if !fresh.tiered || fresh.health != fresh.maxHealth() || fresh.leash == nil || fresh.leash.zone != desc.zones[0] {
			t.Fatalf("survivor %d came back hurt or unleashed: %+v", i, fresh)
		}
	}
	if s.mobs[desc.groups[0][2]].pos != home {
		t.Fatal("a survivor that had moved was not put back on its slot")
	}
	if other.health != 1 || s.mobs[other.entityID] != other {
		t.Fatal("the wipe reset a zone nobody fell in")
	}
	if !s.dungeonGroupClearedLocked(3) || s.dungeon.guardianID != guardian {
		t.Fatal("the wipe undid a cleared group or touched the boss")
	}

	// One wipe, however many ticks it lasts.
	after := slices.Clone(desc.groups[0])
	if s.advanceDungeonDescentLocked(3, party) || !slices.Equal(desc.groups[0], after) {
		t.Fatal("a wipe lasting two ticks reset twice")
	}

	// The sand hall: a scorpion that rose lies buried again; one still under the sand was
	// never engaged and is left alone.
	sand := partyAt(4, zoneCentre(s, world.SandBuriedGroup))
	s.advanceDungeonDescentLocked(4, sand)
	ids := desc.groups[world.SandBuriedGroup]
	risen := s.mobs[ids[0]]
	risen.buried = false
	risen.pos[1] += burialDepth
	buried := s.mobs[ids[1]]
	killAll(sand)
	s.advanceDungeonDescentLocked(5, sand)
	again := s.mobs[desc.groups[world.SandBuriedGroup][0]]
	if again == nil || again == risen || !again.buried || again.pos[1] != risen.pos[1]-burialDepth {
		t.Fatalf("a risen scorpion was not buried again: %+v", again)
	}
	if s.mobs[buried.entityID] != buried {
		t.Fatal("a scorpion still under the sand was replaced")
	}
}

// Nobody inside is a party that left, not one that lost: nothing is reset.
func TestAnEmptyDungeonIsNotAWipe(t *testing.T) {
	s := newWavesSim(t, 1)
	party := partyAt(4, zoneCentre(s, 0))
	s.advanceDungeonDescentLocked(1, party)
	m := s.mobs[s.dungeon.descent.groups[0][0]]
	m.health = 1
	if s.advanceDungeonDescentLocked(2, nil) || s.mobs[m.entityID] != m || m.health != 1 {
		t.Fatal("an empty instance reset a creature")
	}
}

// A wipe deep into the siege puts all twelve waves back, not just the three it once had.
func TestAWipeMidSiegeRestartsEveryWave(t *testing.T) {
	s := newWavesSim(t, 2)
	cave := triggerCentre(t, s, world.CaveTrigger)
	party := partyAt(4, cave)
	w := &s.dungeon.descent.waves
	for tick := uint64(1); w.next < 7; tick++ {
		released := len(s.dungeon.descent.groups[world.CaveBurrowGroup])
		s.advanceDungeonDescentLocked(tick, party)
		for _, id := range s.dungeon.descent.groups[world.CaveBurrowGroup][released:] {
			m := s.mobs[id]
			s.damageMobLocked(m, m.health)
		}
	}
	killAll(party)
	s.advanceDungeonDescentLocked(100000, party)
	if w.started || w.next != 0 || s.dungeonTriggerFiredLocked(world.CaveTrigger) || s.dungeonGroupClearedLocked(world.CaveBurrowGroup) {
		t.Fatalf("a wipe after seven waves left the siege at %+v", w)
	}
}

// The waves count as beaten only once every wave has come out and every spider is dead
// (dungeonGroupClearedLocked holds the burrows uncleared while waves remain). A party
// that cleared the waves so far and then wiped in a hall gets the whole siege back, as
// does one that wiped elsewhere with the last wave still out. A party that beat every
// wave keeps the cave cleared through a wipe anywhere.
func TestAWipeElsewhereRestartsTheWavesUnlessTheyWereBeaten(t *testing.T) {
	for _, tc := range []struct {
		name    string
		waves   int  // waves released before the wipe
		killAll bool // every released spider dead before the wipe
		restart bool
	}{
		{"the first wave cleared, the rest to come", 1, true, true},
		{"the last wave out and alive", len(spiderWaveSizes), false, true},
		{"every wave beaten", len(spiderWaveSizes), true, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			s := newWavesSim(t, 1)
			cave := partyAt(4, triggerCentre(t, s, world.CaveTrigger))
			w := &s.dungeon.descent.waves
			for tick := uint64(1); w.next < tc.waves; tick++ {
				released := len(s.dungeon.descent.groups[world.CaveBurrowGroup])
				s.advanceDungeonDescentLocked(tick, cave)
				for _, id := range s.dungeon.descent.groups[world.CaveBurrowGroup][released:] {
					if m := s.mobs[id]; m != nil && (tc.killAll || w.next < tc.waves) {
						s.damageMobLocked(m, m.health)
					}
				}
			}
			// The party has died in the first hall. Nothing ticks in between, so no
			// further wave can come out and make the burrows look unbeaten.
			hall := partyAt(4, zoneCentre(s, 0))
			killAll(hall)
			s.advanceDungeonDescentLocked(100000, hall)
			restarted := !w.started && w.next == 0 && !s.dungeonTriggerFiredLocked(world.CaveTrigger)
			if restarted != tc.restart {
				t.Fatalf("restarted=%v, want %v: %+v", restarted, tc.restart, w)
			}
			if !tc.restart && !s.dungeonGroupClearedLocked(world.CaveBurrowGroup) {
				t.Fatal("a beaten siege lost its cleared cave to a wipe elsewhere")
			}
		})
	}
}
