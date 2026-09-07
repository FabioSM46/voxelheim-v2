package game

import (
	"context"
	"math"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func loadDungeon(t *testing.T, session InstanceSession) {
	t.Helper()
	for x := int32(-2); x <= 1; x++ {
		for z := int32(-2); z <= 1; z++ {
			coord := world.Coord{X: x, Z: z}
			if session.Chunks.Contains(coord) {
				if _, _, err := session.Chunks.Get(context.Background(), coord); err != nil {
					t.Fatal(err)
				}
			}
		}
	}
}

func TestDungeonBossOrderAndSweptGateAtEveryRotation(t *testing.T) {
	for seed := int64(0); seed < 4; seed++ {
		manager := instanceTestManager(t, 20, 1)
		manager.mu.Lock()
		raw, err := manager.newSessionLocked(100, seed, InstanceRuin{}, nil)
		manager.mu.Unlock()
		if err != nil {
			t.Fatal(err)
		}
		session := raw.snapshot()
		loadDungeon(t, session)
		s := session.Sim
		guardian, king, gate := world.InstanceEncounterAnchors(seed)
		if len(s.mobs) != 2 {
			t.Fatal("fresh dungeon must hold two stable encounters")
		}
		d := s.dungeon
		beast, ruler := s.mobs[d.guardianID], s.mobs[d.kingID]
		if beast.kind != vnet.MobKindVargrGuardian || ruler.kind != vnet.MobKindDraugrKing {
			t.Fatal("placement species mismatch")
		}
		for _, m := range []*mob{beast, ruler} {
			if overlaps(s.terrain, m.species().body.boxAt(m.pos)) {
				t.Fatal("boss spawned in scenery")
			}
		}
		dx, dz := float64(king.X-guardian.X)/38, float64(king.Z-guardian.Z)/38
		start := [3]float64{float64(gate.X) + .5 - dx*3, 1, float64(gate.Z) + .5 - dz*3}
		delta := [3]float64{dx * 6, 0, dz * 6}
		for _, bd := range []body{playerBody, beast.species().body, ruler.species().body} {
			out, _ := moveAndCollideWithStep(s.terrain, bd, start, delta, 0)
			if math.Hypot(out[0]-start[0], out[2]-start[2]) >= 3 {
				t.Fatal("closed gate was crossed by a swept body")
			}
		}
		health := ruler.health
		if s.damageMobLocked(ruler, health) || ruler.health != health {
			t.Fatal("king could die before guardian")
		}
		// Disappearance is never victory and never a request to replace a boss.
		delete(s.mobs, d.guardianID)
		s.directMobsLocked(100, nil, s.sortedMobsLocked())
		if len(s.mobs) != 1 || d.progress.guardian {
			t.Fatal("director recreated boss or opened the gate")
		}
		s.mobs[d.guardianID] = beast
		if !s.damageMobLocked(beast, beast.health) {
			t.Fatal("guardian did not die")
		}
		if !d.progress.guardian {
			t.Fatal("authoritative death did not open the gate")
		}
		out, _ := moveAndCollideWithStep(s.terrain, playerBody, start, delta, 0)
		if math.Hypot(out[0]-start[0], out[2]-start[2]) < 5.9 {
			t.Fatal("open gate still blocks the route")
		}
		manager.Step()
		progress, _ := manager.Lookup(session.ID)
		if len(progress.DefeatedBosses) != 1 || progress.DefeatedBosses[0] != vnet.MobKindVargrGuardian {
			t.Fatal("existing session progress did not collect the kill")
		}
		if !s.damageMobLocked(ruler, ruler.health) {
			t.Fatal("king remained invulnerable after guardian")
		}
		for tick := uint64(1); tick <= 1000; tick++ {
			s.directMobsLocked(tick, nil, s.sortedMobsLocked())
		}
		if len(s.mobs) != 0 {
			t.Fatal("defeated encounters respawned")
		}
	}
}

func TestDungeonRestoreOmitsExactlyTheCompletedEncounters(t *testing.T) {
	for _, completed := range [][]vnet.MobKind{nil, {vnet.MobKindVargrGuardian}, {vnet.MobKindDraugrKing}, {vnet.MobKindVargrGuardian, vnet.MobKindDraugrKing}} {
		manager := instanceTestManager(t, 20, 1)
		record := SavedSession{ID: 20, Seed: 3, ExpiresUnix: time.Now().Add(time.Hour).Unix(), DefeatedBosses: completed}
		restored, expired, err := manager.RestoreSessions([]SavedSession{record})
		if err != nil || restored != 1 || expired != 0 {
			t.Fatal("restore failed", err)
		}
		session, _ := manager.Lookup(20)
		loadDungeon(t, session)
		expected := dungeonProgressFrom(completed)
		if session.Sim.dungeon.progress != expected || len(session.Sim.mobs) != 2-len(completed) {
			t.Fatal("restore forgot or duplicated an encounter")
		}
		_, _, gate := world.InstanceEncounterAnchors(3)
		if session.Sim.terrain.Solid(gate.X, gate.Y, gate.Z) == expected.guardian {
			t.Fatal("restored door disagrees with completed guardian")
		}
		manager.Step()
		if len(session.Sim.mobs) != 2-len(completed) {
			t.Fatal("tick repopulated completed encounter")
		}
	}
}

func TestDungeonGateUpdatesRetryOnlyTheUnsentSuffix(t *testing.T) {
	manager := instanceTestManager(t, 20, 1)
	session, err := manager.Create(InstanceRuin{})
	if err != nil {
		t.Fatal(err)
	}
	s := session.Sim
	accepted, limit := 0, 3
	p := &Player{entityID: 999, deliver: func([]byte) bool {
		if accepted == limit {
			return false
		}
		accepted++
		return true
	}}
	s.players[p.entityID] = p
	beast := s.mobs[s.dungeon.guardianID]
	s.damageMobLocked(beast, beast.health)
	if accepted != 3 || s.dungeon.pending[p] != 3 {
		t.Fatal("opening lost the unsent updates")
	}
	limit = 25
	s.flushDungeonGateLocked()
	if accepted != 25 || len(s.dungeon.pending) != 0 {
		t.Fatal("retry duplicated or lost gate updates")
	}
	s.flushDungeonGateLocked()
	if accepted != 25 {
		t.Fatal("gate replayed after delivery")
	}
	delete(s.players, p.entityID)
}
