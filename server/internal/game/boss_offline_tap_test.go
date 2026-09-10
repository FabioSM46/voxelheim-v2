package game

import (
	"slices"
	"testing"
)

// An offline first hitter's share of a boss kill waits for that owner in both modes, inside that
// run. Without durable rewards it is held in the dungeon's own simulation, which applies it when
// the owner joins that simulation again (voxelheimd writes only the open world's offline awards to
// disk). With durable rewards it is frozen for the journal instead, which delivers it when the
// owner is inside the run again. It is never both, and nobody else receives it.
func TestAnOfflineFirstHittersBossExperienceWaitsForItsOwnerInBothModes(t *testing.T) {
	amount := uint32(vargrGuardianRow.experience)
	for _, durable := range []bool{true, false} {
		m, session, owner, id := rewardDungeon(t, durable)
		s := session.Sim
		s.mu.Lock()
		s.mobs[id].firstHit = newMobTap(owner)
		pos := s.mobs[id].pos
		s.mu.Unlock()
		s.Leave(owner)
		spawn := [3]float32{float32(pos[0]), float32(pos[1]), float32(pos[2] - 1.5)}
		finisher, err := s.JoinCharacter(s.mintEntityID(), testPlayerID(2), 2, "Finisher", spawn, testAppearance(), nil, func([]byte) bool { return true })
		if err != nil {
			t.Fatal(err)
		}
		killRewardBossBy(t, session, id, finisher)
		m.Step()

		var frozen []BossExperienceReward
		if saved := m.SavedSessions(); len(saved) == 1 && len(saved[0].PendingRewards) == 1 {
			frozen = saved[0].PendingRewards[0].Experience
		}
		awards := s.PendingExperienceAwards()
		gained := liveExperienceOf(finisher)
		want := []BossExperienceReward{{Owner: instanceTestCharacter(1), Amount: amount}}
		switch {
		case durable && (!slices.Equal(frozen, want) || len(awards) != 0 || gained != 0):
			t.Fatalf("durable: frozen %+v, held offline awards %+v, finisher +%d; want the owner's share frozen and nothing else", frozen, awards, gained)
		case !durable && (len(frozen) != 0 || len(awards) != 1 || awards[0].PlayerID != testPlayerID(1) || awards[0].Experience != amount || gained != 0):
			t.Fatalf("live: frozen %+v, held offline awards %+v, finisher +%d; want the owner's award held in this simulation and nothing else", frozen, awards, gained)
		}
	}
}
