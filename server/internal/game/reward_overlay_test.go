package game

import (
	"slices"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// Exercise the persistence overlay through the actual manager constructor, terrain
// gate and next tick. Runtime-ID remapping must never alter procedural seed/progress.
func TestRewardOverlayRestoresRemappedRunsThroughRealManager(t *testing.T) {
	dir := t.TempDir()
	players, err := persist.OpenStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	rewards, err := persist.OpenRewardStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	at := time.Date(2026, 3, 14, 20, 0, 0, 0, time.UTC)
	until := at.Add(time.Hour).Unix()
	guardian := []vnet.MobKind{vnet.MobKindVargrGuardian}
	both := []vnet.MobKind{vnet.MobKindVargrGuardian, vnet.MobKindDraugrKing}
	records := []persist.SessionRecord{
		{ID: 1, Seed: 101, Ruin: [2]int64{7, -2}, ExpiresUnix: until, DefeatedBosses: guardian, Bound: []persist.SessionCharacter{{PlayerID: testPlayerID(1), CharacterID: 1}}},
		{ID: 1, Seed: 202, Ruin: [2]int64{8, -2}, ExpiresUnix: until, DefeatedBosses: both, Bound: []persist.SessionCharacter{{PlayerID: testPlayerID(2), CharacterID: 2}}},
	}
	for i, rec := range records {
		if err := rewards.AllocateRun(players, uint64(i+1), rec, world.WorldgenVersion); err != nil {
			t.Fatal(err)
		}
	}
	// Independently saved progress can be ahead of the journal, or lose a run entirely.
	stale := records[0]
	stale.DefeatedBosses = both
	late := persist.SessionCharacter{PlayerID: testPlayerID(3), CharacterID: 3}
	stale.Bound = append(stale.Bound[:len(stale.Bound):len(stale.Bound)], late)
	cold, err := persist.OpenRewardStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	overlay, err := cold.OverlaySessions([]persist.SessionRecord{stale}, at.Unix(), world.WorldgenVersion)
	if err != nil {
		t.Fatal(err)
	}
	if len(overlay) != 2 || overlay[0].Generation != 1 || overlay[1].Generation != 2 || overlay[0].Session.ID == overlay[1].Session.ID {
		t.Fatal("generation mapping or collision remap lost")
	}
	if overlay[1].Session.ID == records[1].ID {
		t.Fatal("fixture did not remap a runtime ID")
	}
	saved := make([]SavedSession, len(overlay))
	for i, row := range overlay {
		rec := row.Session
		saved[i] = SavedSession{ID: rec.ID, Seed: rec.Seed, Ruin: InstanceRuin{CellX: rec.Ruin[0], CellZ: rec.Ruin[1]}, ExpiresUnix: rec.ExpiresUnix, DefeatedBosses: rec.DefeatedBosses}
		for _, owner := range rec.Bound {
			saved[i].Bound = append(saved[i].Bound, InstanceCharacter{PlayerID: owner.PlayerID, CharacterID: owner.CharacterID})
		}
	}
	manager, restored, expired := restartInto(t, saved, at)
	if restored != 2 || expired != 0 {
		t.Fatal("actual manager rejected restored overlay")
	}
	for i, row := range overlay {
		live, ok := manager.Lookup(row.Session.ID)
		if !ok {
			t.Fatal("remapped run missing")
		}
		if live.Seed != records[i].Seed || live.Ruin != (InstanceRuin{CellX: records[i].Ruin[0], CellZ: records[i].Ruin[1]}) || live.ExpiresUnix != until {
			t.Fatal("remap changed procedural run identity")
		}
		if !slices.Equal(live.DefeatedBosses, records[i].DefeatedBosses) {
			t.Fatal("stale saved defeat overrode durable progress")
		}
		loadDungeon(t, live)
		expected := dungeonProgressFrom(records[i].DefeatedBosses)
		if live.Sim.dungeon.progress != expected || len(live.Sim.mobs) != 2-len(records[i].DefeatedBosses) {
			t.Fatal("dead boss respawned during real restoration")
		}
		_, _, gate := world.InstanceEncounterAnchors(live.Seed)
		if live.Sim.terrain.Solid(gate.X, gate.Y, gate.Z) {
			t.Fatal("durable guardian defeat did not open gate")
		}
	}
	manager.Step()
	for i, row := range overlay {
		live, _ := manager.Lookup(row.Session.ID)
		if len(live.Sim.mobs) != 2-len(records[i].DefeatedBosses) {
			t.Fatal("next manager tick respawned completed boss")
		}
	}
	if id, ok := manager.Bound(InstanceRuin{7, -2}, InstanceCharacter{PlayerID: late.PlayerID, CharacterID: late.CharacterID}); !ok || id != overlay[0].Session.ID {
		t.Fatal("later binding lost")
	}
	// Save/reload the effective aliases: a later restart must keep them stable.
	effective := make([]persist.SessionRecord, len(overlay))
	for i, row := range overlay {
		effective[i] = row.Session
	}
	again, err := cold.OverlaySessions(effective, at.Unix(), world.WorldgenVersion)
	if err != nil {
		t.Fatal(err)
	}
	for i, row := range again {
		if row.Session.ID != overlay[i].Session.ID || row.Generation != overlay[i].Generation {
			t.Fatal("alias changed across another restart")
		}
	}
	fresh, err := manager.Create(InstanceRuin{9, -2})
	if err != nil {
		t.Fatal(err)
	}
	for _, row := range overlay {
		if fresh.ID == row.Session.ID {
			t.Fatal("fresh manager allocation overwrote remapped run")
		}
	}
}
