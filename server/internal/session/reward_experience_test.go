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

func (w *rewardWorld) instanceOwner() game.InstanceCharacter {
	return game.InstanceCharacter{PlayerID: w.owner, CharacterID: uint64(w.character.ID)}
}

// owedExperienceRun journals and restores a run at the fixture ruin whose guardian owes this
// character 120 experience, as after a restart: no defeat is in memory.
func (w *rewardWorld) owedExperienceRun(t *testing.T) (game.SavedSession, world.Ruin) {
	t.Helper()
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
		DefeatedBosses: guardian, Bound: []game.InstanceCharacter{w.instanceOwner()}, Generation: 2}
	if _, _, err := w.manager.RestoreSessions([]game.SavedSession{run}); err != nil {
		t.Fatal(err)
	}
	return run, ruin
}

// crossInto crosses the ruin's portal as a session does. The manager admits the character into
// the run it owes and records the visit, and the character's player then joins that world.
func (w *rewardWorld) crossInto(t *testing.T, run game.SavedSession, ruin world.Ruin) (game.InstanceSession, *game.Player) {
	t.Helper()
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
	return decision.Entry.Session, inside
}

// shareState is what the record and the journal say about the owed share.
func (w *rewardWorld) shareState(t *testing.T) (experience, epoch uint64, taken, prepared bool) {
	t.Helper()
	rec := w.record(t)
	journal := w.journalNow(t)
	for _, r := range journal.Runs {
		if r.Generation == 2 {
			taken = r.Defeats[0].Experience[0].Taken
		}
	}
	return uint64(rec.Experience), rec.BossRewardEpoch, taken, len(journal.Intents) != 0
}

// Boss experience the journal owes is claimed only while its owner is inside the run, and only
// once. It is claimed from the journal alone: the run here is restored as after a restart, with no
// defeat in memory, so this same pass offers experience owed from before a restart.
func TestBossExperienceTheJournalOwesIsClaimedOnceInsideItsRun(t *testing.T) {
	t.Parallel()
	w := newRewardWorld(t)
	run, ruin := w.owedExperienceRun(t)

	// Playing, but in the open world: nothing is claimed.
	if err := w.ids.DeliverBossExperience(); err != nil {
		t.Fatal(err)
	}
	w.awaitClaims(t)
	if experience, epoch, _, _ := w.shareState(t); experience != 100 || epoch != 0 {
		t.Fatalf("outside the run: experience %d epoch %d, want 100 and nothing claimed", experience, epoch)
	}

	_, inside := w.crossInto(t, run, ruin)
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
	experience, epoch, taken, prepared := w.shareState(t)
	if experience != 220 || epoch != 1 || !taken || prepared {
		t.Fatalf("inside the run: experience %d epoch %d taken %v prepared %v, want 220 claimed and acknowledged once", experience, epoch, taken, prepared)
	}
	if live := inside.Record().Experience; live != 220 {
		t.Fatalf("live experience = %d, want 220", live)
	}
}

// A claim racing its owner leaving the run stays exact under -race. The claim reaches the player
// only through methods that take its simulation's lock, and the reservation checks again, under
// the manager's and the simulation's locks, that the character is online in that run. Whatever
// the interleaving, the experience lands once or not at all, and the record and the journal
// agree.
func TestAnExperienceClaimRacingItsOwnerLeavingLandsOnceOrNotAtAll(t *testing.T) {
	t.Parallel()
	w := newRewardWorld(t)
	run, ruin := w.owedExperienceRun(t)
	session, inside := w.crossInto(t, run, ruin)

	start, left := make(chan struct{}), make(chan struct{})
	go func() {
		defer close(left)
		<-start
		for range 5 {
			w.manager.Step()
			_ = inside.Record()
		}
		session.Sim.Leave(inside)
		w.manager.Leave(run.ID, w.instanceOwner())
	}()
	close(start)
	if err := w.ids.DeliverBossExperience(); err != nil {
		t.Fatal(err)
	}
	<-left
	w.awaitClaims(t)

	experience, _, taken, prepared := w.shareState(t)
	switch {
	case experience == 220 && (taken || prepared):
	case experience == 100 && !taken && !prepared:
	default:
		t.Fatalf("record experience %d with the share taken %v and an intent prepared %v: the claim landed partly or twice", experience, taken, prepared)
	}
	if err := w.ids.DeliverBossExperience(); err != nil {
		t.Fatal(err)
	}
	w.awaitClaims(t)
	if again, _, _, _ := w.shareState(t); again != experience {
		t.Fatalf("a pass after the owner left changed experience %d to %d", experience, again)
	}
}
