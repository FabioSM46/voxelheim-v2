package game

import (
	"log/slog"
	"maps"
	"slices"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

func characterOf(p *Player) InstanceCharacter {
	return InstanceCharacter{p.playerID, p.characterID}
}

// partyGuardianKill enters a party of three through the portal and stands two of them beside the
// guardian and one beyond the share radius. The first member taps the pull and the second lands
// the killing blow. It returns what each member gained live and what the defeat froze for each.
func partyGuardianKill(t *testing.T, durable bool) (gained, frozen map[InstanceCharacter]uint32) {
	t.Helper()
	var options []SimOption
	if durable {
		options = append(options, WithDurableBossRewards(true))
	}
	manager, open, request, join := portalHarness(t, 2, options...)
	puller, finisher, distant := join(), join(), join()
	inviteAndAccept(t, puller, finisher, finisher.name)
	inviteAndAccept(t, puller, distant, distant.name)
	var entry PortalEntry
	for _, p := range []*Player{puller, finisher, distant} {
		decision := manager.EnterPortal(p, request)
		if decision.Outcome != PortalAdmitted || (entry.Session.ID != 0 && decision.Entry.Session.ID != entry.Session.ID) {
			t.Fatalf("party admission = %+v", decision)
		}
		entry = decision.Entry
	}
	s := entry.Session.Sim
	loadDungeon(t, entry.Session)
	id := s.dungeon.guardianID
	at := s.mobs[id].pos
	near := [3]float32{float32(at[0]), float32(at[1]), float32(at[2] + 3)}
	far := [3]float32{float32(at[0]), float32(at[1]), float32(at[2] + PartyShareRadius + 8)}
	for _, place := range []struct {
		p   *Player
		pos [3]float32
	}{{puller, near}, {finisher, near}, {distant, far}} {
		if err := open.Transfer(place.p, s, place.pos); err != nil {
			t.Fatal(err)
		}
	}
	s.mu.Lock()
	s.startBossEncounterLocked(s.mobs[id], puller)
	s.mobs[id].firstHit = newMobTap(puller)
	before := map[*Player]uint32{puller: puller.experience, finisher: finisher.experience, distant: distant.experience}
	s.mu.Unlock()
	killRewardBossBy(t, entry.Session, id, finisher)
	manager.Step()

	gained, frozen = make(map[InstanceCharacter]uint32), make(map[InstanceCharacter]uint32)
	for p, base := range before {
		if got := liveExperienceOf(p) - base; got != 0 {
			gained[characterOf(p)] = got
		}
	}
	if saved := manager.SavedSessions(); len(saved) == 1 && len(saved[0].PendingRewards) == 1 {
		for _, share := range saved[0].PendingRewards[0].Experience {
			frozen[share.Owner] = share.Amount
		}
	}
	return gained, frozen
}

// A durable boss kill by a real party freezes, for each member, exactly the share a live kill
// awards them: the tap owner and the members inside the share radius, and nobody beyond it.
func TestADurablePartyKillFreezesExactlyTheSharesALiveKillAwards(t *testing.T) {
	liveGained, liveFrozen := partyGuardianKill(t, false)
	durableGained, durableFrozen := partyGuardianKill(t, true)

	var shared uint32
	for _, amount := range liveGained {
		shared += amount
	}
	if len(liveGained) != 2 || shared != uint32(vargrGuardianRow.experience) || len(liveFrozen) != 0 {
		t.Fatalf("live kill: gained %v (sum %d), frozen %v; want two members in range sharing the row's experience and nothing frozen", liveGained, shared, liveFrozen)
	}
	if len(durableGained) != 0 {
		t.Fatalf("durable kill awarded experience live: %v", durableGained)
	}
	if !maps.Equal(durableFrozen, liveGained) {
		t.Fatalf("durable kill froze %v, want exactly the live shares %v", durableFrozen, liveGained)
	}
}

// An all-down wipe during the king's pull replaces only the king. The guardian's defeat and what
// it owes stay held for the journal exactly as frozen, and can still be released to claims.
func TestAWipeLeavesAHeldDefeatAndItsRewardsUntouched(t *testing.T) {
	m, session, p, guardianID := rewardDungeon(t, true)
	killRewardBossBy(t, session, guardianID, p)
	m.Step()
	before := m.SavedSessions()
	if len(before) != 1 || len(before[0].PendingRewards) != 1 {
		t.Fatalf("saved runs before the wipe = %+v", before)
	}

	s := session.Sim
	s.mu.Lock()
	kingID := s.dungeon.kingID
	king := s.mobs[kingID]
	s.startBossEncounterLocked(king, p)
	king.health = 10
	p.dieLocked()
	s.mu.Unlock()
	m.Step()

	after := m.SavedSessions()
	if len(after) != 1 || !slices.Equal(after[0].DefeatedBosses, []vnet.MobKind{vnet.MobKindVargrGuardian}) {
		t.Fatalf("the wipe changed the run: %+v", after)
	}
	if len(after[0].PendingRewards) != 1 || !slices.EqualFunc(after[0].PendingRewards[0].Personal, before[0].PendingRewards[0].Personal, func(a, b BossPersonalReward) bool {
		return a.Owner == b.Owner && a.Silver == b.Silver && slices.Equal(a.Entries, b.Entries)
	}) || !slices.Equal(after[0].PendingRewards[0].Experience, before[0].PendingRewards[0].Experience) {
		t.Fatalf("the wipe changed the held defeat: before %+v, after %+v", before[0].PendingRewards, after[0].PendingRewards)
	}
	s.mu.Lock()
	fresh := s.mobs[s.dungeon.kingID]
	dead, present := s.corpses[guardianID]
	var hold bossRewardHold
	if present {
		hold = dead.rewards
	}
	s.mu.Unlock()
	if !present || fresh == nil || fresh.entityID == kingID || fresh.health != fresh.species().maxHealth || hold != bossRewardsHeld {
		t.Fatalf("after the wipe: king %+v, guardian corpse present %v with hold %d; want a fresh king and the guardian's loot still held", fresh, present, hold)
	}
	if !m.ReleaseBossRewards(after[0], vnet.MobKindVargrGuardian) {
		t.Fatal("the held defeat could not be released after the wipe")
	}
}

// A run players are still inside past midnight keeps holding what a kill after midnight owes, so
// the sync still journals it and releases it to claims. The run is collected once they leave.
func TestAKillAfterMidnightInAnOccupiedRunIsStillHeldForTheJournal(t *testing.T) {
	m, session, p, guardianID := rewardDungeon(t, true)
	clock := &resetClock{at: time.Date(2026, 3, 14, 23, 30, 0, 0, time.UTC)}
	m.now = clock.now
	killRewardBossBy(t, session, guardianID, p)
	m.Step()
	saved := m.SavedSessions()
	if len(saved) != 1 || !m.ReleaseBossRewards(saved[0], vnet.MobKindVargrGuardian) {
		t.Fatalf("the guardian's defeat before midnight = %+v", saved)
	}
	expires := saved[0].ExpiresUnix

	clock.at = time.Unix(expires, 0).Add(10 * time.Minute)
	m.Step()
	s := session.Sim
	s.mu.Lock()
	kingID := s.dungeon.kingID
	s.startBossEncounterLocked(s.mobs[kingID], p)
	s.mu.Unlock()
	killRewardBossBy(t, session, kingID, p)
	m.Step()

	saved = m.SavedSessions()
	both := []vnet.MobKind{vnet.MobKindVargrGuardian, vnet.MobKindDraugrKing}
	if len(saved) != 1 || saved[0].ExpiresUnix != expires || !slices.Equal(saved[0].DefeatedBosses, both) ||
		len(saved[0].PendingRewards) != 1 || saved[0].PendingRewards[0].Kind != vnet.MobKindDraugrKing ||
		len(saved[0].PendingRewards[0].Personal) != 1 || len(saved[0].PendingRewards[0].Experience) != 1 {
		t.Fatalf("after a kill past midnight = %+v; want the expired run reporting the king's loot and experience", saved)
	}
	if !m.ReleaseBossRewards(saved[0], vnet.MobKindDraugrKing) {
		t.Fatal("the defeat after midnight could not be released to claims")
	}
	if !m.Leave(session.ID, instanceTestCharacter(1)) {
		t.Fatal("leaving the run was refused")
	}
	m.Step()
	if _, live := m.Lookup(session.ID); live {
		t.Fatal("the expired run outlived the party that was holding it")
	}
}

// A rebuilt boss corpse outlives a live corpse's CorpseLifetime under real ticks, and goes when the
// run resets.
func TestARebuiltBossCorpseOutlivesCorpseLifetimeAndGoesAtTheReset(t *testing.T) {
	const rate = 1
	m, err := NewInstanceManager(rate, 3, 2, testEntityIDs(), slog.New(slog.DiscardHandler), WithDurableBossRewards(true))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(m.Close)
	clock := &resetClock{at: time.Date(2026, 3, 14, 20, 0, 0, 0, time.UTC)}
	m.now = clock.now
	owner := instanceTestCharacter(1)
	held := BossRewardDefeat{Kind: vnet.MobKindVargrGuardian, Personal: []BossPersonalReward{{Owner: owner, Entries: []protocol.InventoryStack{{ItemID: uint16(ItemBone), Count: 2}}, Silver: 30}}}
	run := SavedSession{ID: 9, Seed: 19, Ruin: InstanceRuin{CellX: 2, CellZ: 3}, ExpiresUnix: nextResetUnix(clock.at),
		DefeatedBosses: []vnet.MobKind{vnet.MobKindVargrGuardian}, Bound: []InstanceCharacter{owner}, Generation: 1, HeldRewards: []BossRewardDefeat{held}}
	if _, _, err := m.RestoreSessions([]SavedSession{run}); err != nil {
		t.Fatal(err)
	}
	session, err := m.Reenter(run.Ruin, owner)
	if err != nil || session.ID != run.ID {
		t.Fatalf("entering the restored run = %d, %v", session.ID, err)
	}
	s := session.Sim
	state := func() (corpses int, tick, lifetime uint64) {
		s.mu.Lock()
		defer s.mu.Unlock()
		return len(s.corpses), s.currentTick, s.corpseLifetimeTicks
	}
	_, start, lifetime := state()
	if lifetime != uint64(CorpseLifetime/time.Second)*rate {
		t.Fatalf("corpse lifetime = %d ticks, want CorpseLifetime at %d Hz", lifetime, rate)
	}
	for range lifetime + 1 {
		m.Step()
	}
	if corpses, tick, _ := state(); corpses != 1 || tick <= start+lifetime {
		t.Fatalf("after CorpseLifetime: %d corpses at tick %d (from %d); want the rebuilt corpse still there past %d ticks", corpses, tick, start, lifetime)
	}

	if !m.Leave(run.ID, owner) {
		t.Fatal("leaving the run was refused")
	}
	m.Step()
	if _, live := m.Lookup(run.ID); !live {
		t.Fatal("an empty run was collected before its reset")
	}
	clock.at = time.Unix(run.ExpiresUnix, 0)
	m.Step()
	if _, live := m.Lookup(run.ID); live {
		t.Fatal("the run and its rebuilt corpse outlived the reset")
	}
}

// A character bound to the run after the kill is not in the kill's loot roster or its experience
// recipients: it holds no container on the corpse and owes no share.
func TestACharacterBoundAfterTheKillHoldsNoLootAndNoExperience(t *testing.T) {
	m, session, p, guardianID := rewardDungeon(t, true)
	killRewardBossBy(t, session, guardianID, p)
	m.Step()
	late := instanceTestCharacter(2)
	if _, err := m.Join(session.ID, late); err != nil {
		t.Fatal(err)
	}
	s := session.Sim
	s.mu.Lock()
	pos := s.corpses[guardianID].pos
	s.mu.Unlock()
	q, err := s.JoinCharacter(s.mintEntityID(), late.PlayerID, late.CharacterID, "Latecomer",
		[3]float32{float32(pos[0]), float32(pos[1]), float32(pos[2] + 1.5)}, testAppearance(), nil, func([]byte) bool { return true })
	if err != nil {
		t.Fatal(err)
	}

	saved := m.SavedSessions()
	if len(saved) != 1 || !slices.Contains(saved[0].Bound, late) || len(saved[0].PendingRewards) != 1 {
		t.Fatalf("saved runs = %+v, want the latecomer bound and one held defeat", saved)
	}
	defeat := saved[0].PendingRewards[0]
	if slices.ContainsFunc(defeat.Personal, func(r BossPersonalReward) bool { return r.Owner == late }) ||
		slices.ContainsFunc(defeat.Experience, func(r BossExperienceReward) bool { return r.Owner == late }) {
		t.Fatalf("the latecomer was given a share of a kill they missed: %+v", defeat)
	}
	if !m.ReleaseBossRewards(saved[0], vnet.MobKindVargrGuardian) {
		t.Fatal("the defeat was not released")
	}
	// The same corpse is open to the member the kill did roll for, so the refusals below are
	// about the latecomer and not about the loot.
	if reason, err := p.OpenLoot(protocol.LootOpenRequest{CorpseID: guardianID, ClientTick: 1}); err != nil {
		t.Fatalf("the entitled killer could not open the loot: %s, %v", reason, err)
	}
	if selection, reason, err := p.BossLootToClaim(guardianID, 1, 0); err != nil || len(selection.Entries) == 0 {
		t.Fatalf("the entitled killer could not select the loot: %+v, %s, %v", selection, reason, err)
	}
	if _, err := q.OpenLoot(protocol.LootOpenRequest{CorpseID: guardianID, ClientTick: 1}); err == nil {
		t.Fatal("the latecomer opened loot they hold no container in")
	}
	if _, _, err := q.BossLootToClaim(guardianID, 1, 0); err == nil {
		t.Fatal("the latecomer selected loot they hold no container in")
	}
}
