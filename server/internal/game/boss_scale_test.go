package game

import (
	"math"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// The level scale a boss takes at its pull (#1099). The rule is argued in boss_scale.go;
// these tests hold its inputs, its clamps and the one moment it is computed.

// joinAtLevel joins a player at a level, carrying whatever armour pieces are given.
func joinAtLevel(t *testing.T, h *vitalsHarness, entityID uint64, level uint16, pieces ...testArmourPiece) *Player {
	t.Helper()
	pos := [3]float32{0.5 + float32(entityID), 64, 0.5}
	life := lifeWearing(t, pos, pieces...)
	life.Experience = experienceBefore(level)
	life.Health = maxHealthFor(level)
	p, _ := h.joinLife(entityID, pos, &life)
	return p
}

func bossScaleOf(t *testing.T, h *vitalsHarness, id uint64) (bossScale, uint16) {
	t.Helper()
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	m := h.sim.mobs[id]
	if m == nil || m.encounter == nil {
		t.Fatalf("boss %d is not engaged", id)
	}
	return m.encounter.scale, m.health
}

// The rule, written out for the parties the issue names. The expected numbers are literals
// rather than recomputed from maxHealthFor, so a change to the level curve has to be taken
// here as a decision: it moves every boss blow in the game.
func TestBossScaleForRepresentativeParties(t *testing.T) {
	t.Parallel()

	guardian := mobRegistry[vnet.MobKindVargrGuardian]
	king := mobRegistry[vnet.MobKindDraugrKing]
	for _, c := range []struct {
		name          string
		def           mobDefinition
		levels        []uint16
		members       uint16
		damagePercent uint16
	}{
		{"solo at level one", guardian, []uint16{1}, 1, 100},
		{"solo at the level cap", guardian, []uint16{30}, 1, 245},
		{"two at level ten", king, []uint16{10, 10}, 2, 145},
		// 100 + 145 + 195 + 245 = 685 over four members is 171.25.
		{"mixed party of four", king, []uint16{1, 10, 20, 30}, 4, 171},
		{"full party of four at level one", guardian, []uint16{1, 1, 1, 1}, 4, 100},
		// Clamped at the low end: no level below one, and an empty party is one member.
		{"a level below one reads as one", guardian, []uint16{0}, 1, 100},
		{"nobody reads as one member at level one", king, nil, 1, 100},
		// Clamped at the high end: no level above the cap, and a fifth member adds no health.
		{"a level above the cap reads as the cap", king, []uint16{MaxLevel + 9}, 1, 245},
		{"a fifth member adds no health", guardian, []uint16{1, 1, 1, 1, 1}, 4, 100},
		{"five at the cap", king, []uint16{30, 30, 30, 30, 30}, 4, 245},
	} {
		t.Run(c.name, func(t *testing.T) {
			got := bossScaleFor(c.def, c.levels)
			want := bossScale{members: c.members, maxHealth: c.def.maxHealth * c.members, damagePercent: c.damagePercent}
			if got != want {
				t.Errorf("bossScaleFor(%v) = %+v, want %+v", c.levels, got, want)
			}
		})
	}
	if bossScaleMaxDamagePercent != 245 {
		t.Errorf("the heaviest scale is %d%%, want the level-30 health ratio of 245%%", bossScaleMaxDamagePercent)
	}
}

// Every boss row, at every scale the rule can produce, fits the uint16 the wire carries
// health in, and every move it can make stays a blow the uint16 damage path can carry.
func TestBossHealthFitsTheWireAtEveryScale(t *testing.T) {
	t.Parallel()

	for kind, def := range mobRegistry {
		if !def.isBoss() {
			continue
		}
		if full := uint32(def.maxHealth) * bossScaleMaxMembers; full > math.MaxUint16 {
			t.Errorf("%s: %d per member times %d is %d, over the wire's %d", kind, def.maxHealth, bossScaleMaxMembers, full, math.MaxUint16)
		}
		heaviest := bossScale{members: bossScaleMaxMembers, maxHealth: def.maxHealth * bossScaleMaxMembers, damagePercent: bossScaleMaxDamagePercent}
		for _, move := range encounterMoveCatalog[kind] {
			raw := moveDamage(def, move)
			if got, want := uint32(heaviest.blow(raw)), uint32(raw)*uint32(bossScaleMaxDamagePercent)/100; got != max(want, 1) {
				t.Errorf("%s %s: the heaviest blow is %d, want %d", kind, move.kind, got, want)
			}
		}
	}
	if got := (bossScale{}).blow(22); got != 22 {
		t.Errorf("an encounter created without a pull scaled a 22 blow to %d", got)
	}
}

// The scale reads levels and nothing else: the same levels in the starter kit and in iron
// produce the same boss.
func TestBossScaleDoesNotReadEquipment(t *testing.T) {
	t.Parallel()

	pull := func(pieces ...testArmourPiece) bossScale {
		h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
		first := joinAtLevel(t, h, 1, 12, pieces...)
		joinAtLevel(t, h, 2, 12, pieces...)
		scale, _ := bossScaleOf(t, h, pullGuardian(t, h, [3]float64{0.5, 64, -8.5}, first))
		return scale
	}
	bare := pull()
	armoured := pull(fullTestArmour(ItemIronHelm), fullTestArmour(ItemIronCuirass), fullTestArmour(ItemIronGreaves))
	if bare != armoured {
		t.Fatalf("unarmoured party scaled to %+v, the same levels in iron to %+v", bare, armoured)
	}
	if want := bossScaleFor(mobRegistry[vnet.MobKindVargrGuardian], []uint16{12, 12}); bare != want {
		t.Fatalf("a party of two at level twelve scaled to %+v, want %+v", bare, want)
	}
}

// Computed once, at the pull. A member levelling up, a member disconnecting, somebody
// joining and a second call into the pull all leave the boss the party pulled.
func TestBossScaleIsFixedAtThePull(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	first := joinAtLevel(t, h, 1, 1)
	second := joinAtLevel(t, h, 2, 20)
	id := pullKing(t, h, [3]float64{0.5, 64, -20.5}, first)

	pulled, health := bossScaleOf(t, h, id)
	want := bossScaleFor(mobRegistry[vnet.MobKindDraugrKing], []uint16{1, 20})
	if pulled != want || health != want.maxHealth {
		t.Fatalf("the pull gave scale %+v at %d health, want %+v at full health", pulled, health, want)
	}

	h.sim.mu.Lock()
	h.sim.awardExperienceLocked(first, ExperienceCap)
	h.sim.mu.Unlock()
	h.sim.Leave(second)
	late := joinAtLevel(t, h, 3, 30)
	h.sim.mu.Lock()
	h.sim.startBossEncounterLocked(h.sim.mobs[id], late)
	h.sim.mu.Unlock()
	h.advance(40)

	if got, _ := bossScaleOf(t, h, id); got != pulled {
		t.Fatalf("after a level-up, a disconnect and a join the scale is %+v, want the pull's %+v", got, pulled)
	}
	h.sim.mu.Lock()
	wire := mobStates([]*mob{h.sim.mobs[id]})[0]
	h.sim.mu.Unlock()
	if wire.MaxHealth != pulled.maxHealth {
		t.Fatalf("the snapshot carries a maximum of %d, want the pull's %d", wire.MaxHealth, pulled.maxHealth)
	}
}

// A boss hurt before its pull keeps the share it had lost, and its stages begin at the same
// share of the scaled ceiling that they do of the registry's.
func TestAScaledBossKeepsItsShareAndItsStages(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	var party []*Player
	for i := range uint64(4) {
		party = append(party, joinAtLevel(t, h, i+1, 1))
	}
	def := mobRegistry[vnet.MobKindVargrGuardian]
	id := h.placeSpeciesAt(vnet.MobKindVargrGuardian, [3]float64{0.5, 64, -20.5})
	h.sim.mu.Lock()
	m := h.sim.mobs[id]
	m.health = def.maxHealth / 2
	h.sim.startBossEncounterLocked(m, party[0])
	scaled := m.encounter.scale.maxHealth
	if scaled != 4*def.maxHealth || m.health != scaled/2 {
		h.sim.mu.Unlock()
		t.Fatalf("a half-health boss pulled by four is at %d of %d, want half of %d", m.health, scaled, 4*def.maxHealth)
	}

	threshold := uint16(uint32(scaled) * uint32(def.phaseHealthPercents[0]) / 100)
	m.health = threshold + 1
	h.sim.advanceEncounterPhasesLocked()
	above := m.encounter.phase
	m.health = threshold
	h.sim.advanceEncounterPhasesLocked()
	below := m.encounter.phase
	h.sim.mu.Unlock()
	if above != 1 || below != 2 {
		t.Fatalf("stages at %d and %d of %d health are %d and %d, want 1 then 2", threshold+1, threshold, scaled, above, below)
	}
}

// A blow against a party at level thirty costs what the registry's blow costs a level-one
// player, as a share of health: the move's damage under the pull's percentage.
func TestAScaledBlowCostsALevelledPlayerTheSameShare(t *testing.T) {
	t.Parallel()

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	near := joinAtLevel(t, h, 1, 30)
	h.place(near, [3]float64{0.5, 64, 0.5})
	boss := pullGuardian(t, h, [3]float64{0.8, 64, -2.5}, near)

	scale, _ := bossScaleOf(t, h, boss)
	raw := moveDamage(mobRegistry[vnet.MobKindVargrGuardian], encounterMoveCatalog[vnet.MobKindVargrGuardian][0])
	blow := scale.blow(raw)
	if scale.damagePercent != 245 || blow != raw*245/100 {
		t.Fatalf("a level-thirty solo pull scaled a %d blow to %d at %d%%", raw, blow, scale.damagePercent)
	}
	start := maxHealthFor(30)
	if got := h.vitals(near).Health; got != start {
		t.Fatalf("the player started at %d health, want %d", got, start)
	}
	for range 30 {
		h.step()
		if running := runningMoveOf(h, boss); running != nil && running.phase == vnet.MovePhaseRecovery {
			break
		}
	}
	if got, want := h.vitals(near).Health, start-blow; got != want {
		t.Fatalf("the blow left a level-thirty player at %d, want %d (%d of %d)", got, want, blow, start)
	}
}
