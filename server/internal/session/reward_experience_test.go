package session

import (
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// askingRuns counts how often a pass looks for a share's owner inside its run, which it does
// only for a share it is about to claim.
type askingRuns struct {
	rewardRunManager
	asked int
}

func (r *askingRuns) PlayerInside(run uint64, character game.InstanceCharacter) *game.Player {
	r.asked++
	return r.rewardRunManager.PlayerInside(run, character)
}

// awaitClaims waits for every claim the coordinator has started to finish.
func (w *rewardWorld) awaitClaims(t *testing.T) {
	t.Helper()
	settled := make(chan struct{})
	go func() {
		w.ids.rewards.wg.Wait()
		close(settled)
	}()
	select {
	case <-settled:
	case <-time.After(10 * time.Second):
		t.Fatal("a boss reward claim never finished")
	}
}

// Boss experience the journal owes is claimed only while its owner is inside the run, and only
// once. It is claimed from the journal alone: the run here is restored as after a restart, with
// no defeat in memory, so this same pass offers experience owed from before a restart.
func TestBossExperienceTheJournalOwesIsClaimedOnceInsideItsRun(t *testing.T) {
	t.Parallel()
	w := newRewardWorld(t)
	owner := game.InstanceCharacter{PlayerID: w.owner, CharacterID: uint64(w.character.ID)}
	bound := persist.SessionCharacter{PlayerID: w.owner, CharacterID: uint64(w.character.ID)}
	guardian := []vnet.MobKind{vnet.MobKindVargrGuardian}
	expires := time.Now().Add(time.Hour).Unix()
	ruin, found := world.RuinAt(0x5EED, 0, 0)
	if !found {
		t.Fatal("fixture ruin missing")
	}
	record := persist.SessionRecord{ID: 8, Seed: 20, Ruin: [2]int64{ruin.CellX, ruin.CellZ}, ExpiresUnix: expires, DefeatedBosses: guardian, Bound: []persist.SessionCharacter{bound}}
	defeat := persist.RewardDefeat{Kind: vnet.MobKindVargrGuardian, Experience: []persist.BossExperienceReward{{Owner: bound, Amount: 120}}}
	if err := w.journal.AllocateRun(w.store, 2, record, world.WorldgenVersion, defeat); err != nil {
		t.Fatal(err)
	}
	run := game.SavedSession{ID: 8, Seed: 20, Ruin: game.InstanceRuin{CellX: ruin.CellX, CellZ: ruin.CellZ}, ExpiresUnix: expires,
		DefeatedBosses: guardian, Bound: []game.InstanceCharacter{owner}, Generation: 2}
	if _, _, err := w.manager.RestoreSessions([]game.SavedSession{run}); err != nil {
		t.Fatal(err)
	}

	// Playing, but in the open world: nothing is claimed.
	if err := w.ids.DeliverBossExperience(); err != nil {
		t.Fatal(err)
	}
	w.awaitClaims(t)
	if rec := w.record(t); rec.Experience != 100 || rec.BossRewardEpoch != 0 {
		t.Fatalf("outside the run: experience %d epoch %d, want 100 and nothing claimed", rec.Experience, rec.BossRewardEpoch)
	}

	// Cross the ruin's portal as a session does. The manager admits the character into the run
	// it owes and records the visit, and the character's player then joins that world.
	w.sim.Leave(w.player)
	arch := ruin.Arch
	// A returning character joins where its life says it stands, so the life carries the arch.
	standing := *w.self.Life
	standing.Pos = [3]float64{float64(arch.X) + 1.5, float64(arch.Y) - 1, float64(arch.Z) + .5}
	atArch, err := w.sim.JoinCharacter(1<<40, w.owner, uint64(w.character.ID), w.character.Name,
		[3]float32{float32(standing.Pos[0]), float32(standing.Pos[1]), float32(standing.Pos[2])}, w.character.Appearance, &standing, func([]byte) bool { return true })
	if err != nil {
		t.Fatal(err)
	}
	decision := w.manager.EnterPortal(atArch, protocol.PortalRequest{HasArch: true, Arch: [3]int32{int32(arch.X), int32(arch.Y), int32(arch.Z)}})
	if decision.Outcome != game.PortalAdmitted || decision.Entry.Session.ID != run.ID {
		t.Fatalf("crossing into the owed run = %+v, want admission into run %d", decision, run.ID)
	}
	w.sim.Leave(atArch)
	inside, err := decision.Entry.Session.Sim.JoinCharacter(1<<41, w.owner, uint64(w.character.ID), w.character.Name,
		[3]float32{0.5, 80, 0.5}, w.character.Appearance, w.self.Life, func([]byte) bool { return true })
	if err != nil {
		t.Fatal(err)
	}
	runs := &askingRuns{rewardRunManager: w.ids.rewards.runs}
	w.ids.rewards.runs = runs
	for pass := range 2 {
		runs.asked = 0
		if err := w.ids.DeliverBossExperience(); err != nil {
			t.Fatalf("pass %d: %v", pass, err)
		}
		w.awaitClaims(t)
		if want := 1 - pass; runs.asked != want {
			t.Fatalf("pass %d offered the share %d times, want %d: a taken share is never offered again", pass, runs.asked, want)
		}
	}
	if rec := w.record(t); rec.Experience != 220 || rec.BossRewardEpoch != 1 {
		t.Fatalf("inside the run: experience %d epoch %d, want 220 claimed once", rec.Experience, rec.BossRewardEpoch)
	}
	if live := inside.Record().Experience; live != 220 {
		t.Fatalf("live experience = %d, want 220", live)
	}
	journal := w.journalNow(t)
	if len(journal.Intents) != 0 {
		t.Fatalf("intents left behind: %+v", journal.Intents)
	}
	for _, r := range journal.Runs {
		if r.Generation == 2 && !r.Defeats[0].Experience[0].Taken {
			t.Fatal("the claimed share is not marked taken")
		}
	}
}
