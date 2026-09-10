package game

import (
	"slices"
	"testing"
)

func killRewardBossBy(t *testing.T, session InstanceSession, id uint64, p *Player) {
	t.Helper()
	s := session.Sim
	s.mu.Lock()
	defer s.mu.Unlock()
	boss := s.mobs[id]
	if boss == nil {
		t.Fatal("the boss is missing")
	}
	s.creditMobDamageLocked(p, boss, boss.health)
	if s.mobs[id] != nil {
		t.Fatal("the boss survived its killing blow")
	}
}

func liveExperienceOf(p *Player) uint32 {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	return p.experience
}

// With durable rewards a dungeon boss's experience is frozen into its defeat for the journal
// and awarded to nobody live. Without them it is awarded live and nothing is frozen. One
// lookup decides both halves, so no build awards the same experience twice.
func TestADurableBossKillFreezesItsExperienceInsteadOfAwardingIt(t *testing.T) {
	amount := uint32(vargrGuardianRow.experience)
	if amount == 0 {
		t.Fatal("the guardian row awards no experience to freeze")
	}
	for _, durable := range []bool{true, false} {
		m, session, p, id := rewardDungeon(t, durable)
		before := liveExperienceOf(p)
		killRewardBossBy(t, session, id, p)
		m.Step()
		gained := liveExperienceOf(p) - before
		var frozen []BossExperienceReward
		if saved := m.SavedSessions(); len(saved) == 1 && len(saved[0].PendingRewards) == 1 {
			frozen = saved[0].PendingRewards[0].Experience
		}
		want := []BossExperienceReward{{Owner: instanceTestCharacter(1), Amount: amount}}
		switch {
		case durable && (gained != 0 || !slices.Equal(frozen, want)):
			t.Fatalf("durable kill: live experience +%d, frozen %+v; want +0 and %+v", gained, frozen, want)
		case !durable && (gained != amount || len(frozen) != 0):
			t.Fatalf("live kill: live experience +%d, frozen %+v; want +%d and nothing frozen", gained, frozen, amount)
		}
	}
}
