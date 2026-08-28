package game

import (
	"slices"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// No production row is a boss yet. These focused tests temporarily promote the
// draugr row while all t.Parallel tests are paused, then restore it before returning.
func withTestBoss(t *testing.T) {
	t.Helper()
	definition := mobRegistry[vnet.MobKindDraugr]
	boss := definition
	boss.rank = mobRankBoss
	mobRegistry[vnet.MobKindDraugr] = boss
	t.Cleanup(func() { mobRegistry[vnet.MobKindDraugr] = definition })
}

func encounterRoster(t *testing.T, h *vitalsHarness, id uint64) []corpseOwner {
	t.Helper()
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	m := h.sim.mobs[id]
	if m == nil || m.encounter == nil {
		t.Fatalf("boss %d has no encounter", id)
	}
	return append([]corpseOwner(nil), m.encounter.roster...)
}

func TestBossEncounterStartsOnceFromTargetAcquisitionOrDamage(t *testing.T) {
	withTestBoss(t)

	t.Run("target acquisition copies the whole persistent roster", func(t *testing.T) {
		h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
		leader, _ := joinPartyPlayer(t, h, 1, "Astrid", [3]float32{0.5, 64, 0.5})
		offline, _ := joinPartyPlayer(t, h, 2, "Bjorn", [3]float32{0.5, 64, 0.5})
		dead, _ := joinPartyPlayer(t, h, 3, "Eira", [3]float32{0.5, 64, 0.5})
		inviteAndAccept(t, leader, offline, "Bjorn")
		inviteAndAccept(t, leader, dead, "Eira")

		want := []corpseOwner{leader.corpseOwner(), offline.corpseOwner(), dead.corpseOwner()}
		h.sim.Leave(offline)
		h.hurt(dead, PlayerMaxHealth)
		id := h.placeSpeciesAt(vnet.MobKindDraugr, [3]float64{0.5, 64, -4.5})
		h.step()
		if got := encounterRoster(t, h, id); !slices.Equal(got, want) {
			t.Fatalf("target-acquired roster = %+v, want ordered offline/dead roster %+v", got, want)
		}

		// Leadership and membership now change in the live party, then a later start
		// attempt names somebody else. Neither may revise the frozen slice.
		mustParty(t, leader, vnet.PartyActionLeave, "")
		late, _ := joinPartyPlayer(t, h, 4, "Freya", [3]float32{0.5, 64, 0.5})
		h.sim.mu.Lock()
		h.sim.startBossEncounterLocked(h.sim.mobs[id], late)
		h.sim.mu.Unlock()
		if got := encounterRoster(t, h, id); !slices.Equal(got, want) {
			t.Fatalf("party mutation restarted encounter with %+v, want original %+v", got, want)
		}
	})

	t.Run("valid damage wins before the first AI target pass", func(t *testing.T) {
		h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
		player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
		id := h.placeSpeciesAt(vnet.MobKindDraugr, [3]float64{0.5, 64, -1.5})
		if err := h.swing(player, 0, 1); err != nil {
			t.Fatal(err)
		}
		h.step()
		if got, want := encounterRoster(t, h, id), []corpseOwner{player.corpseOwner()}; !slices.Equal(got, want) {
			t.Fatalf("damage-started roster = %+v, want %+v", got, want)
		}
		h.sim.mu.Lock()
		firstHit := h.sim.mobs[id].firstHit
		h.sim.mu.Unlock()
		if firstHit == nil {
			t.Fatal("the valid damage did not preserve first-tap identity")
		}
	})
}

func TestRegistryRefusesAnUnclassifiedSpecies(t *testing.T) {
	definition := mobRegistry[vnet.MobKindDraugr]
	unclassified := definition
	unclassified.rank = mobRankUnknown
	mobRegistry[vnet.MobKindDraugr] = unclassified
	t.Cleanup(func() { mobRegistry[vnet.MobKindDraugr] = definition })

	if _, registered := mobByKind(vnet.MobKindDraugr); registered {
		t.Fatal("a species with no explicit normal-or-boss rank remained spawnable")
	}
}

func TestBossCorpseFreezesPersonalLootAndIsolatesEveryContainer(t *testing.T) {
	withTestBoss(t)
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	leader, _ := joinPartyPlayer(t, h, 1, "Astrid", [3]float32{0.5, 64, 0.5})
	eligible, _ := joinPartyPlayer(t, h, 2, "Bjorn", [3]float32{0.5, 64, 0.5})
	inviteAndAccept(t, leader, eligible, "Bjorn")

	id := h.placeSpeciesAt(vnet.MobKindDraugr, [3]float64{0.5, 64, -1.5})
	h.step() // target acquisition freezes Astrid, then Bjorn

	late, _ := joinPartyPlayer(t, h, 3, "Eira", [3]float32{0.5, 64, 0.5})
	inviteAndAccept(t, leader, late, "Eira")
	mustParty(t, leader, vnet.PartyActionKick, "Bjorn")
	eligibleOwner := eligible.corpseOwner()
	h.sim.Leave(eligible)
	reconnected, err := h.sim.JoinCharacter(4, eligibleOwner.playerID, eligibleOwner.characterID,
		"Bjorn", [3]float32{0.5, 64, 0.5}, testAppearance(), nil, func([]byte) bool { return true })
	if err != nil {
		t.Fatalf("reconnect kicked character: %v", err)
	}

	h.killWithTheStarterBlade(leader, id)
	leaderOwner := leader.corpseOwner()
	lateOwner := late.corpseOwner()
	// **One draugr roll is two draws now, not one**, because the species table gained a
	// silver line beside the bones. The replay has to spend them in the table's order or
	// every expectation below slides by one draw — which is the failure this helper exists
	// to make impossible to write by accident.
	wantRNG := newLootRNG(testWorldSeed)
	nextRoll := func() (bones, silver uint16) {
		bones = uint16(1 + wantRNG.IntN(2))
		silver = uint16(2 + wantRNG.IntN(5))
		return bones, silver
	}
	wantLeader, wantLeaderSilver := nextRoll()
	wantEligible, wantEligibleSilver := nextRoll()

	h.sim.mu.Lock()
	c := h.sim.corpses[id]
	if c == nil || c.personal == nil || len(c.personal) != 2 {
		h.sim.mu.Unlock()
		t.Fatalf("boss personal containers = %#v, want exactly two", c)
	}
	leaderContainer := c.personal[leaderOwner]
	eligibleContainer := c.personal[eligibleOwner]
	_, lateIncluded := c.personal[lateOwner]
	if leaderContainer == nil || eligibleContainer == nil || lateIncluded {
		h.sim.mu.Unlock()
		t.Fatalf("frozen owners leader=%v eligible=%v late=%v", leaderContainer != nil, eligibleContainer != nil, lateIncluded)
	}
	gotLeader := leaderContainer.entries[0].stack.count
	gotEligible := eligibleContainer.entries[0].stack.count
	gotLeaderSilver := leaderContainer.entries[1].stack.count
	gotEligibleSilver := eligibleContainer.entries[1].stack.count
	h.sim.mu.Unlock()
	if gotLeader != wantLeader || gotEligible != wantEligible {
		t.Fatalf("stable ordered rolls = %d, %d; want %d, %d", gotLeader, gotEligible, wantLeader, wantEligible)
	}
	if gotLeaderSilver != wantLeaderSilver || gotEligibleSilver != wantEligibleSilver {
		t.Fatalf("stable ordered silver rolls = %d, %d; want %d, %d",
			gotLeaderSilver, gotEligibleSilver, wantLeaderSilver, wantEligibleSilver)
	}

	// Both sessions ask at once. The simulation serialises validation, and neither
	// open consumes RNG or mutates either settled container.
	type openResult struct {
		reason vnet.RefusalReason
		err    error
	}
	opened := make(chan openResult, 2)
	go func() {
		reason, err := leader.OpenLoot(protocol.LootOpenRequest{CorpseID: id, ClientTick: 1})
		opened <- openResult{reason: reason, err: err}
	}()
	go func() {
		reason, err := reconnected.OpenLoot(protocol.LootOpenRequest{CorpseID: id, ClientTick: 1})
		opened <- openResult{reason: reason, err: err}
	}()
	for range 2 {
		result := <-opened
		if result.err != nil || result.reason != vnet.RefusalReasonUnknown {
			t.Fatalf("simultaneous boss open = %s, %v", result.reason, result.err)
		}
	}
	if reason, err := late.OpenLoot(protocol.LootOpenRequest{CorpseID: id, ClientTick: 1}); err == nil || reason != vnet.RefusalReasonLootNotOwned {
		t.Fatalf("post-pull join open = %s, %v; want LootNotOwned", reason, err)
	}

	h.sim.mu.Lock()
	next := h.sim.rollLootLocked(&mob{kind: vnet.MobKindDraugr})
	beforeLeaderRevision := c.personal[leaderOwner].revision
	beforeLeaderCount := c.personal[leaderOwner].entries[0].stack.count
	h.sim.mu.Unlock()
	wantNextBones, wantNextSilver := nextRoll()
	if got := next[0].stack.count; got != wantNextBones {
		t.Fatalf("opening order advanced loot RNG: next roll %d bones, want %d", got, wantNextBones)
	}
	if got := next[1].stack.count; got != wantNextSilver {
		t.Fatalf("opening order advanced loot RNG: next roll %d silver, want %d", got, wantNextSilver)
	}

	// Two lines to empty rather than one, and each take spends a revision, so the request
	// that follows has to name the revision the one before it produced.
	for _, take := range []struct {
		entryID    uint64
		revision   uint32
		clientTick uint32
	}{{1, 1, 1}, {2, 2, 2}} {
		if reason, err := reconnected.TakeLoot(protocol.LootTakeRequest{
			CorpseID: id, EntryID: take.entryID, Revision: take.revision, ClientTick: take.clientTick,
		}); err != nil {
			t.Fatalf("kicked character take of entry %d = %s, %v", take.entryID, reason, err)
		}
	}
	h.sim.mu.Lock()
	_, corpseStillExists := h.sim.corpses[id]
	afterLeader := c.personal[leaderOwner]
	eligibleAfter := c.personal[eligibleOwner]
	h.sim.mu.Unlock()
	if !corpseStillExists {
		t.Fatal("looting one personal container removed another character's loot")
	}
	if afterLeader.revision != beforeLeaderRevision || afterLeader.entries[0].stack.count != beforeLeaderCount {
		t.Fatalf("other personal container changed to revision %d entries %+v", afterLeader.revision, afterLeader.entries)
	}
	if eligibleAfter.revision != 3 || len(eligibleAfter.entries) != 0 {
		t.Fatalf("looted personal container = revision %d entries %+v", eligibleAfter.revision, eligibleAfter.entries)
	}

	for _, take := range []struct {
		entryID    uint64
		revision   uint32
		clientTick uint32
	}{{1, 1, 1}, {2, 2, 2}} {
		if reason, err := leader.TakeLoot(protocol.LootTakeRequest{
			CorpseID: id, EntryID: take.entryID, Revision: take.revision, ClientTick: take.clientTick,
		}); err != nil {
			t.Fatalf("last personal take of entry %d = %s, %v", take.entryID, reason, err)
		}
	}
	h.sim.mu.Lock()
	_, corpseStillExists = h.sim.corpses[id]
	h.sim.mu.Unlock()
	if corpseStillExists {
		t.Fatal("corpse survived after every personal container became empty")
	}
}

func TestSoloBossLootSurvivesDeathUntilSharedExpiry(t *testing.T) {
	withTestBoss(t)
	h, player, _, id := armedAgainst(t, vnet.MobKindDraugr, [3]float64{0.5, 64, -1.5})
	h.killWithTheStarterBlade(player, id)

	h.sim.mu.Lock()
	c := h.sim.corpses[id]
	if c == nil {
		h.sim.mu.Unlock()
		t.Fatal("solo boss kill created no corpse")
	}
	container := c.personal[player.corpseOwner()]
	expiresTick := c.expiresTick
	h.sim.mu.Unlock()
	if len(c.personal) != 1 || container == nil || len(container.entries) == 0 {
		t.Fatalf("solo boss corpse = %#v, want one personal container", c)
	}
	if expiresTick-h.tick != h.sim.corpseLifetimeTicks {
		t.Fatalf("boss expiry distance = %d ticks, want %d", expiresTick-h.tick, h.sim.corpseLifetimeTicks)
	}

	h.hurt(player, PlayerMaxHealth)
	h.sim.mu.Lock()
	preserved := c.personal[player.corpseOwner()]
	h.sim.mu.Unlock()
	if preserved == nil || len(preserved.entries) == 0 {
		t.Fatal("player death removed personal boss loot")
	}
	h.advance(int(h.sim.deathTicks) + 1)
	if reason, err := player.OpenLoot(protocol.LootOpenRequest{CorpseID: id, ClientTick: 1}); err != nil {
		t.Fatalf("respawned solo owner open = %s, %v", reason, err)
	}
}

func TestNonKillBossRemovalDiscardsEncounterWithoutRollingLoot(t *testing.T) {
	withTestBoss(t)

	for _, tc := range []struct {
		name   string
		remove func(*vitalsHarness, *mob)
	}{
		{name: "dawn", remove: func(h *vitalsHarness, _ *mob) { h.step() }},
		{name: "distance", remove: func(h *vitalsHarness, _ *mob) {
			h.keepNight()
			h.advance(int(h.sim.mobDespawnTicks) + 2)
		}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := newVitalsHarnessAt(t, DefaultTickRate, dropTerrain{groundTop: 63}, 1)
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			id := h.placeSpeciesAt(vnet.MobKindDraugr, [3]float64{500.5, 64, 500.5})
			h.sim.mu.Lock()
			m := h.sim.mobs[id]
			h.sim.startBossEncounterLocked(m, player)
			m.firstHit = newMobTap(player)
			h.sim.mu.Unlock()

			tc.remove(h, m)
			h.sim.mu.Lock()
			_, mobExists := h.sim.mobs[id]
			_, corpseExists := h.sim.corpses[id]
			next := h.sim.rollLootLocked(&mob{kind: vnet.MobKindDraugr})
			h.sim.mu.Unlock()
			if mobExists || corpseExists || m.encounter != nil || m.firstHit != nil {
				t.Fatalf("non-kill removal left mob=%v corpse=%v encounter=%v tap=%v", mobExists, corpseExists, m.encounter != nil, m.firstHit != nil)
			}
			wantRNG := newLootRNG(testWorldSeed)
			if got, want := next[0].stack.count, uint16(1+wantRNG.IntN(2)); got != want {
				t.Fatalf("non-kill removal advanced loot RNG: next=%d want=%d", got, want)
			}
		})
	}
}

func TestProductionRegistryContainsNoBossSpeciesYet(t *testing.T) {
	t.Parallel()
	for kind, definition := range mobRegistry {
		if definition.isBoss() {
			t.Errorf("production species %s is a boss; issue #327 adds only generic classification", kind)
		}
	}
}
