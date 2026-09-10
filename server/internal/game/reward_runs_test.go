package game

import (
	"errors"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

func TestSavedRunsCarryTheirRewardGenerationAcrossRestore(t *testing.T) {
	m := instanceTestManager(t, 20, 2)
	run := SavedSession{
		ID: 7, Seed: 0x5EED, Ruin: InstanceRuin{CellX: 1},
		ExpiresUnix:    m.now().Add(time.Hour).Unix(),
		DefeatedBosses: []vnet.MobKind{vnet.MobKindVargrGuardian},
	}
	if _, _, err := m.RestoreSessions([]SavedSession{run}); err != nil {
		t.Fatal(err)
	}
	if saved := m.SavedSessions(); len(saved) != 1 || saved[0].Generation != 0 {
		t.Fatalf("a run with no allocation = %+v", saved)
	}

	otherID, otherExpiry, otherSeed, otherRuin := run, run, run, run
	otherID.ID = 8
	otherExpiry.ExpiresUnix++
	otherSeed.Seed++
	otherRuin.Ruin.CellZ = 4
	for name, stale := range map[string]SavedSession{"id": otherID, "expiry": otherExpiry, "seed": otherSeed, "ruin": otherRuin} {
		if m.AssignRunGeneration(stale, 3) {
			t.Fatalf("a generation was assigned to a run with another %s", name)
		}
	}
	if m.AssignRunGeneration(run, 0) {
		t.Fatal("generation zero was assigned")
	}
	if !m.AssignRunGeneration(run, 3) {
		t.Fatal("the allocated generation was not assigned")
	}
	if m.AssignRunGeneration(run, 4) {
		t.Fatal("a run's generation was replaced")
	}
	saved := m.SavedSessions()
	if len(saved) != 1 || saved[0].Generation != 3 {
		t.Fatalf("saved run = %+v, want generation 3", saved)
	}

	restored := instanceTestManager(t, 20, 2)
	if _, _, err := restored.RestoreSessions(saved); err != nil {
		t.Fatal(err)
	}
	if got := restored.SavedSessions(); len(got) != 1 || got[0].Generation != 3 {
		t.Fatalf("restored run = %+v, want generation 3", got)
	}

	twin := saved[0]
	twin.ID = 9
	if _, _, err := instanceTestManager(t, 20, 2).RestoreSessions([]SavedSession{saved[0], twin}); !errors.Is(err, ErrDuplicateSession) {
		t.Fatalf("two runs sharing a generation = %v, want %v", err, ErrDuplicateSession)
	}
}
