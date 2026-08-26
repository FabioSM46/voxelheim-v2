package game

import (
	"math"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

func threatFor(t *testing.T, h *vitalsHarness, mobID, playerID uint64) float64 {
	t.Helper()
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	m := h.sim.mobs[mobID]
	if m == nil {
		t.Fatalf("mob %d is absent", mobID)
	}
	return m.threat[playerID]
}

func chooseForTest(t *testing.T, h *vitalsHarness, mobID uint64) uint64 {
	t.Helper()
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	m := h.sim.mobs[mobID]
	if m == nil {
		t.Fatalf("mob %d is absent", mobID)
	}
	m.chooseTargetLocked(h.sim, h.sim.sortedPlayersLocked())
	return m.target
}

func TestThreatChoosesTheLeaderAndRequiresAStrictTenacityLead(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	mobID := h.spawnDraugrAt([3]float32{0.5, 64, 0.5})
	a, _ := h.join(7, [3]float32{5.5, 64, 0.5})
	b, _ := h.join(9, [3]float32{6.5, 64, 0.5})

	h.sim.mu.Lock()
	m := h.sim.mobs[mobID]
	m.threat[a.entityID] = 40
	m.threat[b.entityID] = 10
	h.sim.mu.Unlock()
	if got := chooseForTest(t, h, mobID); got != a.entityID {
		t.Fatalf("40 versus 10 chose %d, want A %d", got, a.entityID)
	}

	h.sim.mu.Lock()
	m.threat[b.entityID] = 44
	h.sim.mu.Unlock()
	if got := chooseForTest(t, h, mobID); got != a.entityID {
		t.Fatalf("exactly 1.1x switched to %d; current A %d must hold", got, a.entityID)
	}

	h.sim.mu.Lock()
	m.threat[b.entityID] = 44.01
	h.sim.mu.Unlock()
	if got := chooseForTest(t, h, mobID); got != b.entityID {
		t.Fatalf("strictly above 1.1x chose %d, want B %d", got, b.entityID)
	}

	// Equal threat never moves a valid current target, even when the other identity is
	// lower and would win the deterministic tie without tenacity.
	h.sim.mu.Lock()
	m.target = b.entityID
	m.threat[a.entityID] = 50
	m.threat[b.entityID] = 50
	h.sim.mu.Unlock()
	if got := chooseForTest(t, h, mobID); got != b.entityID {
		t.Fatalf("equal threat switched to %d, want current B %d", got, b.entityID)
	}
}

func TestAnInvalidTargetIsDroppedWithoutApplyingTenacity(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	mobID := h.spawnDraugrAt([3]float32{0.5, 64, 0.5})
	a, _ := h.join(1, [3]float32{5.5, 64, 0.5})
	b, _ := h.join(2, [3]float32{6.5, 64, 0.5})

	h.sim.mu.Lock()
	m := h.sim.mobs[mobID]
	m.target = a.entityID
	m.threat[a.entityID] = 100
	m.threat[b.entityID] = 1
	a.pos[0] = m.pos[0] + m.species().aggroRange + 2
	h.sim.mu.Unlock()
	if got := chooseForTest(t, h, mobID); got != b.entityID {
		t.Fatalf("out-of-range A held target %d, want B %d immediately", got, b.entityID)
	}
}

func TestACommittedWindupDropsATargetThatBecomesInvalid(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	h.keepNight()
	mobID := h.spawnDraugrAt([3]float32{0.5, 64, 0.5})
	target, _ := h.join(1, [3]float32{1.5, 64, 0.5})
	h.sim.mu.Lock()
	m := h.sim.mobs[mobID]
	m.target = target.entityID
	m.threat[target.entityID] = 100
	m.action = vnet.MobActionWindup
	m.actionTicks = 2
	target.protectionTicks = 2
	h.sim.mu.Unlock()
	h.step()
	if state, _ := h.mobState(mobID); state.target != 0 {
		t.Errorf("windup retained invalid target %d", state.target)
	}
}

func TestDeathAndLeaveEraseAPlayerFromEveryThreatLedger(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	first := h.spawnDraugrAt([3]float32{0.5, 64, 0.5})
	second := h.spawnDraugrAt([3]float32{3.5, 64, 0.5})
	dead, _ := h.join(1, [3]float32{5.5, 64, 0.5})
	leaving, _ := h.join(2, [3]float32{6.5, 64, 0.5})

	h.sim.mu.Lock()
	for _, id := range []uint64{first, second} {
		h.sim.mobs[id].threat[dead.entityID] = 20
		h.sim.mobs[id].threat[leaving.entityID] = 30
		h.sim.mobs[id].target = dead.entityID
	}
	dead.dieLocked()
	h.sim.mu.Unlock()
	for _, id := range []uint64{first, second} {
		if got := threatFor(t, h, id, dead.entityID); got != 0 {
			t.Errorf("mob %d retained dead player threat %v", id, got)
		}
		if state, _ := h.mobState(id); state.target != 0 {
			t.Errorf("mob %d retained dead target %d", id, state.target)
		}
	}

	h.sim.mu.Lock()
	for _, id := range []uint64{first, second} {
		h.sim.mobs[id].target = leaving.entityID
	}
	h.sim.mu.Unlock()
	h.sim.Leave(leaving)
	for _, id := range []uint64{first, second} {
		if got := threatFor(t, h, id, leaving.entityID); got != 0 {
			t.Errorf("mob %d retained disconnected player threat %v", id, got)
		}
		if state, _ := h.mobState(id); state.target != 0 {
			t.Errorf("mob %d retained disconnected target %d", id, state.target)
		}
	}
}

func TestDamageThreatUsesTheCachedWornWeight(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	mobID := h.spawnDraugrAt([3]float32{0.5, 64, 0.5})
	ironLife := lifeWearing(t, [3]float32{5.5, 64, 0.5},
		fullTestArmour(ItemIronHelm), fullTestArmour(ItemIronCuirass), fullTestArmour(ItemIronGreaves))
	leatherLife := lifeWearing(t, [3]float32{6.5, 64, 0.5},
		fullTestArmour(ItemLeatherCap), fullTestArmour(ItemLeatherJerkin), fullTestArmour(ItemLeatherLeggings))
	iron, _ := h.joinLife(1, [3]float32{5.5, 64, 0.5}, &ironLife)
	leather, _ := h.joinLife(2, [3]float32{6.5, 64, 0.5}, &leatherLife)

	h.sim.mu.Lock()
	m := h.sim.mobs[mobID]
	h.sim.creditDamageThreatLocked(m, iron, 10)
	h.sim.creditDamageThreatLocked(m, leather, 10)
	ironWeight := 1 + float64(iron.worn.threat)/ThreatScale
	leatherWeight := 1 + float64(leather.worn.threat)/ThreatScale
	h.sim.mu.Unlock()

	gotIron := threatFor(t, h, mobID, iron.entityID)
	gotLeather := threatFor(t, h, mobID, leather.entityID)
	wantRatio := ironWeight / leatherWeight
	if gotIron != 10*ironWeight || gotLeather != 10*leatherWeight {
		t.Fatalf("damage threat iron/leather = %v/%v, want %v/%v from registry weights",
			gotIron, gotLeather, 10*ironWeight, 10*leatherWeight)
	}
	if ratio := gotIron / gotLeather; ratio != wantRatio {
		t.Errorf("damage threat ratio = %v, want registry ratio %v", ratio, wantRatio)
	}
}

func TestALandedSwingCreditsTheHealthItActuallyRemoved(t *testing.T) {
	t.Parallel()

	h, player, mobID := armedHarness(t, DefaultTickRate, [3]float32{0.5, 64, -1.5})
	if err := h.swing(player, 0, 1); err != nil {
		t.Fatalf("swing: %v", err)
	}
	h.step()
	want := float64(RustySwordDamage) // the starter has no worn threat multiplier
	if got := threatFor(t, h, mobID, player.entityID); got != want {
		t.Errorf("landed swing credited %v threat, want actual damage weight %v", got, want)
	}
}

func TestHealingCreditsHalfTheRestoredHealthOnlyToMobsHuntingThePatient(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	hunting := h.spawnDraugrAt([3]float32{0.5, 64, 0.5})
	other := h.spawnDraugrAt([3]float32{3.5, 64, 0.5})
	healer, _ := h.join(1, [3]float32{5.5, 64, 0.5})
	healed, _ := h.join(2, [3]float32{6.5, 64, 0.5})
	somebodyElse, _ := h.join(3, [3]float32{7.5, 64, 0.5})

	h.sim.mu.Lock()
	h.sim.mobs[hunting].target = healed.entityID
	h.sim.mobs[other].target = somebodyElse.entityID
	h.sim.creditHealThreatLocked(healer, healed, 10)
	h.sim.creditHealThreatLocked(healer, healed, 0)
	h.sim.mu.Unlock()

	if got := threatFor(t, h, hunting, healer.entityID); got != 5 {
		t.Errorf("ten restored health credited %v, want 5", got)
	}
	if got := threatFor(t, h, other, healer.entityID); got != 0 {
		t.Errorf("mob hunting somebody else credited %v, want 0", got)
	}
}

func TestABlockCreditsTheExactShieldTaunt(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	mobID := h.spawnDraugrAt([3]float32{0.5, 64, 0.5})
	blocker, _ := h.join(1, [3]float32{5.5, 64, 0.5})
	h.sim.mu.Lock()
	h.sim.creditBlockThreatLocked(blocker, h.sim.mobs[mobID])
	h.sim.mu.Unlock()
	if got := threatFor(t, h, mobID, blocker.entityID); got != ShieldTauntThreat {
		t.Errorf("block threat = %v, want ShieldTauntThreat %v", got, ShieldTauntThreat)
	}
}

func TestIdleThreatDecaysAndTargetlessThreatIsForgotten(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	h.keepNight()
	mobID := h.spawnDraugrAt([3]float32{0.5, 64, 0.5})
	outside, _ := h.join(1, [3]float32{float32(draugrRow.aggroRange + 2), 64, 0.5})
	start := ThreatDecayPerSecond * 20
	h.sim.mu.Lock()
	h.sim.mobs[mobID].threat[outside.entityID] = start
	h.sim.mu.Unlock()

	for range DefaultTickRate {
		h.step()
	}
	if got := threatFor(t, h, mobID, outside.entityID); got != start-ThreatDecayPerSecond {
		t.Fatalf("one idle second left threat %v, want %v", got, start-ThreatDecayPerSecond)
	}
	for range DefaultTickRate * (ThreatForgetSeconds - 1) {
		h.step()
	}
	if got := threatFor(t, h, mobID, outside.entityID); got != 0 {
		t.Errorf("ten target-less seconds retained threat %v", got)
	}
}

func TestThreatDoesNotDecayDuringCombat(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	h.keepNight()
	mobID := h.spawnDraugrAt([3]float32{0.5, 64, 0.5})
	target, _ := h.join(1, [3]float32{5.5, 64, 0.5})
	h.sim.mu.Lock()
	h.sim.mobs[mobID].threat[target.entityID] = 20
	h.sim.mu.Unlock()
	for range DefaultTickRate {
		h.step()
	}
	if got := threatFor(t, h, mobID, target.entityID); math.Abs(got-20) > 1e-9 {
		t.Errorf("combat decayed threat to %v, want 20", got)
	}
}

func TestPassiveSpeciesNeverGainAThreatLedger(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	id := h.spawnMobAt(vnet.MobKindDeer, [3]float32{0.5, 64, 0.5})
	player, _ := h.join(1, [3]float32{5.5, 64, 0.5})
	h.sim.mu.Lock()
	m := h.sim.mobs[id]
	h.sim.creditDamageThreatLocked(m, player, 10)
	h.sim.creditBlockThreatLocked(player, m)
	ledger := m.threat
	h.sim.mu.Unlock()
	if ledger != nil {
		t.Fatalf("passive deer allocated threat ledger %#v", ledger)
	}
}

func TestEveryHostileSpeciesSpawnsWithALedger(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	for kind, definition := range mobRegistry {
		id := h.spawnMobAt(kind, [3]float32{0.5, 64, 0.5})
		state, _ := h.mobState(id)
		if definition.passive && state.threat != nil {
			t.Errorf("passive %s spawned with a ledger", kind)
		}
		if !definition.passive && state.threat == nil {
			t.Errorf("hostile %s spawned without a ledger", kind)
		}
	}
}

func TestMobDeathClearsItsLedger(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	id := h.spawnDraugrAt([3]float32{0.5, 64, 0.5})
	player, _ := h.join(1, [3]float32{1.5, 64, 0.5})
	h.sim.mu.Lock()
	m := h.sim.mobs[id]
	m.threat[player.entityID] = 20
	h.sim.damageMobLocked(m, m.health)
	left := len(m.threat)
	h.sim.mu.Unlock()
	if left != 0 {
		t.Errorf("dying mob retained %d threat entries", left)
	}
}
