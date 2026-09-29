package main

import (
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func ally(id uint64, health uint16, x float64) allyView {
	return allyView{id: id, pos: [3]float64{x, 1, 3}, health: health, maxHealth: 100, alive: true}
}

func TestHealerChoosesLowestFractionWithStableTieAndExclusions(t *testing.T) {
	origin := orbOrigin([3]float64{2, 1, 3})
	members := []allyView{ally(1, 1, 3), ally(2, 30, 4), ally(3, 10, 5), ally(4, 0, 6), ally(5, 1, 40), ally(6, 1, 7)}
	members[5].alive = false
	got, ok := woundedAlly(1, origin, members)
	if !ok || got.id != 3 || !got.ally {
		t.Fatalf("target %+v, found %t", got, ok)
	}
	members[1].health, members[1].maxHealth = 20, 200
	got, _ = woundedAlly(1, origin, members)
	if got.id != 2 {
		t.Fatalf("fraction tie must choose lower id: %+v", got)
	}
	members[1].maxHealth = 0
	got, _ = woundedAlly(1, origin, members)
	if got.id != 3 {
		t.Fatal("zero max health selected")
	}
	for i := range members {
		members[i].health = 75
		members[i].maxHealth = 100
	}
	if _, ok = woundedAlly(1, origin, members); ok {
		t.Fatal("healed at threshold")
	}
}

func TestOrbLineRequiresDeliveredTerrainAndAvoidsLivingBodies(t *testing.T) {
	v := flatView()
	from := orbOrigin([3]float64{2, 1, 3})
	target := orbTarget{id: 2, point: [3]float64{10, 1 + game.PlayerHeight/2, 3}, ally: true}
	if !orbLineClear(v, from, target, []allyView{ally(2, 20, 10)}, nil, 1) {
		t.Fatal("clear target blocked itself")
	}
	mob := mobView{id: 3, pos: [3]float64{6, 1, 3}, health: 100, kind: vnet.MobKindDraugr}
	if orbLineClear(v, from, target, nil, []mobView{mob}, 1) {
		t.Fatal("launched through creature")
	}
	mob.pos[1] = 5
	if !orbLineClear(v, from, target, nil, []mobView{mob}, 1) {
		t.Fatal("creature above flight blocks it")
	}
	mob.pos = [3]float64{12, 1, 3}
	if !orbLineClear(v, from, target, nil, []mobView{mob}, 1) {
		t.Fatal("creature beyond target blocks it")
	}
	if orbLineClear(v, from, target, []allyView{ally(3, 100, 6)}, nil, 1) {
		t.Fatal("healthy ally intercept ignored")
	}
	dead := ally(3, 0, 6)
	dead.alive = false
	if !orbLineClear(v, from, target, []allyView{dead}, nil, 1) {
		t.Fatal("dead body blocked healing")
	}
	v.set(6, 2, 3, world.Stone)
	if orbLineClear(v, from, target, nil, nil, 1) {
		t.Fatal("wall ignored")
	}
	if orbLineClear(newBlockView(), from, target, nil, nil, 1) {
		t.Fatal("unread chunk considered air")
	}
}

func TestHealerReadsAndReplacesItsOwnPartySnapshot(t *testing.T) {
	c := &client{entityID: 1, healing: &healerState{}}
	frame := protocol.EncodeEntitySnapshot(protocol.EntitySnapshot{PartyMembers: []protocol.PartyMemberState{
		{EntityID: 2, Pos: [3]float32{4, 1, 3}, Health: 25, MaxHealth: 100, Alive: true},
	}})
	c.absorb(vnet.GetRootAsEnvelope(frame, 0))
	members := c.healerMembers(time.Now())
	if len(members) != 1 || members[0] != ally(2, 25, 4) {
		t.Fatalf("members %+v", members)
	}
	clear(frame)
	members[0].health = 99
	if c.healerMembers(time.Now())[0].health != 25 {
		t.Fatal("reader aliases frame or caller")
	}
	if got := c.healerMembers(time.Now().Add(time.Second)); len(got) != 0 {
		t.Fatal("stale health retained")
	}
	c.absorb(vnet.GetRootAsEnvelope(protocol.EncodeEntitySnapshot(protocol.EntitySnapshot{}), 0))
	if len(c.healerMembers(time.Now())) != 0 {
		t.Fatal("absent vector retained members")
	}
}

func TestHealerHealthObservationIsBoundToOwnTargetAndFlightWindow(t *testing.T) {
	now := time.Unix(100, 0)
	origin := orbOrigin([3]float64{2, 1, 3})
	target := orbTarget{id: 2, point: [3]float64{10, 1.9, 3}, ally: true}
	baseline := []allyView{ally(2, 20, 10), ally(3, 20, 6)}
	for _, tc := range []struct {
		name   string
		update func(*healerState, []allyView)
		delta  uint64
	}{
		{"target heal", func(_ *healerState, m []allyView) { m[0].health += 10 }, 10},
		{"other ally", func(_ *healerState, m []allyView) { m[1].health += 10 }, 0},
		{"damage", func(_ *healerState, m []allyView) { m[0].health -= 10 }, 0},
		{"maximum changed", func(_ *healerState, m []allyView) { m[0].health += 10; m[0].maxHealth++ }, 0},
		{"respawn", func(h *healerState, m []allyView) { h.members[0].alive = false; m[0].health = 100 }, 0},
		{"expired", func(h *healerState, m []allyView) { h.pending[0].expires = now; m[0].health += 10 }, 0},
		{"too early", func(h *healerState, m []allyView) { h.pending[0].earliest = now.Add(time.Second); m[0].health += 10 }, 0},
		{"stream gap", func(h *healerState, m []allyView) { h.seenAt = now.Add(-time.Second); m[0].health += 10 }, 0},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := &healerState{}
			h.observe(append([]allyView(nil), baseline...), now)
			h.launched(target, origin, now)
			next := append([]allyView(nil), baseline...)
			tc.update(h, next)
			h.observe(next, now.Add(500*time.Millisecond))
			if h.totals.allies != 1 || h.totals.restored != tc.delta {
				t.Fatalf("totals %+v", h.totals)
			}
			next = append([]allyView(nil), next...)
			next[0].health += 10
			h.observe(next, now.Add(600*time.Millisecond))
			if tc.delta > 0 && h.totals.restored != tc.delta {
				t.Fatal("one request credited twice")
			}
		})
	}
	h := &healerState{}
	h.observe(baseline, now)
	h.launched(orbTarget{id: 9}, origin, now)
	h.observe([]allyView{ally(2, 30, 10)}, now.Add(500*time.Millisecond))
	if h.totals.creatures != 1 || h.totals.restored != 0 {
		t.Fatal("creature request received heal credit")
	}
}

func TestTwoPendingRequestsCannotClaimOneDeltaTwice(t *testing.T) {
	now := time.Unix(100, 0)
	h := &healerState{members: []allyView{ally(2, 20, 10)}, seenAt: now,
		pending: []*healObservation{{target: 2, earliest: now, expires: now.Add(time.Second), written: true}, {target: 2, earliest: now, expires: now.Add(time.Second), written: true}}}
	h.observe([]allyView{ally(2, 30, 10)}, now.Add(100*time.Millisecond))
	if h.totals.restored != 10 || len(h.pending) != 0 {
		t.Fatalf("duplicate inference %+v", h)
	}
}

func TestCreatureAimAndEmptyHealerTotals(t *testing.T) {
	m := mobView{id: 7, kind: vnet.MobKindCaveSpider, pos: [3]float64{5, 1, 3}}
	target := creatureOrbTarget(m)
	if target.id != 7 || target.ally || target.point != ([3]float64{5, 1.3, 3}) {
		t.Fatalf("target %+v", target)
	}
	c := &client{}
	if c.healingTotals() != (healerTotals{}) {
		t.Fatal("non-healer has telemetry")
	}
	c.healing = &healerState{totals: healerTotals{allies: 3, creatures: 7, restored: 20}}
	if c.healingTotals() != c.healing.totals {
		t.Fatal("report lost totals")
	}
}

// launched is the already-written path used by deterministic observation tests.
func (h *healerState) launched(target orbTarget, origin [3]float64, now time.Time) {
	h.finishLaunch(target, h.beginLaunch(target, origin, now), true)
}

func TestEarlyHealObservationCommitsOnlyWhenAttackWriteSucceeds(t *testing.T) {
	now := time.Now()
	target := orbTarget{id: 2, point: [3]float64{10, 1.9, 3}, ally: true}
	h := &healerState{members: []allyView{ally(2, 20, 10)}, seenAt: now}
	shot := h.beginLaunch(target, orbOrigin([3]float64{2, 1, 3}), now)
	h.observe([]allyView{ally(2, 30, 10)}, now.Add(500*time.Millisecond))
	if h.totals != (healerTotals{}) || shot.restored != 10 {
		t.Fatal("tentative evidence committed early")
	}
	h.finishLaunch(target, shot, true)
	if h.totals.allies != 1 || h.totals.restored != 10 {
		t.Fatal("successful write lost early evidence")
	}
}
