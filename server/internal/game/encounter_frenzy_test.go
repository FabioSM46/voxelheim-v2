package game

import (
	"fmt"
	"reflect"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// Crossing the threshold unlocks the next selection, never rewrites the blow a
// player already read. The ordinal is the existing authoritative announcement;
// visual strap/posture choreography belongs to the client presentation issue.
func TestVargrFrenzyKeepsTheSingleClawAndItsEarnedRecovery(t *testing.T) {
	for _, phase := range []vnet.MovePhase{vnet.MovePhaseTelegraph, vnet.MovePhaseRelease, vnet.MovePhaseRecovery} {
		t.Run(phase.String(), func(t *testing.T) {
			h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
			p, out := h.join(1, [3]float32{.5, 64, .5})
			id := pullGuardian(t, h, [3]float64{.5, 64, -2.5}, p)
			preferMove(h, id, vnet.EncounterMoveKindPrisonerClaws)
			r := awaitComboPhase(t, h, p, id, 0, phase)
			instance := r.instanceID
			geometry := append([]protocol.HazardVolume(nil), r.hazards...)
			recoveryFrames := 0
			if phase == vnet.MovePhaseRecovery {
				recoveryFrames = 1
			}
			h.sim.mu.Lock()
			m := h.sim.mobs[id]
			m.health = uint16(uint32(m.species().maxHealth) * 55 / 100)
			h.sim.mu.Unlock()
			ended := false
			for range 200 {
				h.heal(p)
				h.step()
				state := newestTimeline(t, out.all())
				if state.phase != 2 || len(state.moves) != 1 {
					t.Fatalf("threshold was not announced on the existing move: %+v", state)
				}
				move := state.moves[0]
				if move.MoveInstanceID != instance || move.ComboStep != 0 || move.ComboTotal != 0 || move.Kind != vnet.EncounterMoveKindPrisonerClaws {
					t.Fatalf("phase transformed a committed single claw: %+v", move)
				}
				if move.Ended == vnet.MoveEndCompleted {
					ended = true
					break
				}
				if move.Phase == vnet.MovePhaseRecovery {
					recoveryFrames++
					if move.PhaseTicks != 28 || len(move.Hazards) != 0 {
						t.Fatalf("single claw lost its safe 1.4s opening: %+v", move)
					}
				} else if !reflect.DeepEqual(geometry, move.Hazards) {
					t.Fatal("crossing the threshold rotated an announced claw")
				}
			}
			if !ended || recoveryFrames != 28 {
				t.Fatalf("single claw opening lasted %d ticks, ended=%v", recoveryFrames, ended)
			}
			// Real scheduling after its cooldown eventually chooses the newly unlocked pair.
			var next *runningMove
			for range 500 {
				h.heal(p)
				h.step()
				if candidate := runningMoveOf(h, id); candidate != nil && candidate.def.kind == vnet.EncounterMoveKindPrisonerClaws {
					next = candidate
					break
				}
			}
			if next == nil || next.comboStep != 1 || next.comboTotal != 2 || next.instanceID <= instance {
				t.Fatalf("next claw selection did not unlock the pair: %+v", next)
			}
		})
	}
}

func TestVargrFrenzyThresholdIsAnnouncedOnceAndSurvivesHealing(t *testing.T) {
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	p, out := h.join(1, [3]float32{.5, 64, .5})
	id := pullGuardian(t, h, [3]float64{.5, 64, -40.5}, p)
	full := mobRegistry[vnet.MobKindVargrGuardian].maxHealth
	threshold := uint16(uint32(full) * 55 / 100)
	for _, tc := range []struct {
		health uint16
		phase  uint8
	}{{threshold + 1, 1}, {threshold, 2}, {threshold - 1, 2}, {full, 2}, {threshold, 2}} {
		h.sim.mu.Lock()
		h.sim.mobs[id].health = tc.health
		h.sim.mu.Unlock()
		h.step()
		if state := newestTimeline(t, out.all()); state.phase != tc.phase {
			t.Fatalf("health%d announced stage%d, want%d", tc.health, state.phase, tc.phase)
		}
	}
}

func TestFrenzyClawsRetargetOnlyBeforeTheirOwnPreparation(t *testing.T) {
	h, p, out, id := comboFight(t, DefaultTickRate, vnet.EncounterMoveKindPrisonerClaws)
	second, _ := h.join(2, [3]float32{2.5, 64, -2.5})
	first := awaitComboPhase(t, h, p, id, 1, vnet.MovePhaseTelegraph)
	locked := append([]protocol.HazardVolume(nil), first.hazards...)
	h.sim.mu.Lock()
	h.sim.mobs[id].addThreatLocked(second.entityID, 10000)
	h.sim.mu.Unlock()
	awaitComboPhase(t, h, p, id, 1, vnet.MovePhaseRelease)
	if !reflect.DeepEqual(runningMoveOf(h, id).hazards, locked) {
		t.Fatal("threat changed the first claw's announced side")
	}
	next := awaitComboPhase(t, h, p, id, 2, vnet.MovePhaseTelegraph)
	if next.instanceID == first.instanceID || next.aim[0] < .8 || next.aim[2] < .4 {
		t.Fatalf("second claw did not commit its opposite bearing at the new target: %+v", next)
	}
	before := newestTimeline(t, out.all()).moves
	h.place(second, [3]float64{-2.5, 64, -2.5})
	awaitComboPhase(t, h, p, id, 2, vnet.MovePhaseRelease)
	after := newestTimeline(t, out.all()).moves
	if !reflect.DeepEqual(before[len(before)-1].Hazards, after[len(after)-1].Hazards) {
		t.Fatal("second claw followed its target during preparation")
	}
}

func TestFrenzyClawsCannotRepeatBeforeTheirCooldown(t *testing.T) {
	h, p, out, _ := comboFight(t, DefaultTickRate, vnet.EncounterMoveKindPrisonerClaws)
	var priorEnd uint32
	starts := 0
	for range 800 {
		h.heal(p)
		h.step()
		for _, move := range newestTimeline(t, out.all()).moves {
			if move.Kind != vnet.EncounterMoveKindPrisonerClaws {
				continue
			}
			if move.ComboTotal != 2 || move.ComboStep == 0 || move.ComboStep > 2 {
				t.Fatalf("unbounded frenzy combination: %+v", move)
			}
			if move.ComboStep == 1 && move.Phase == vnet.MovePhaseTelegraph && uint32(h.tick) == move.PhaseStartedTick {
				starts++
				if priorEnd != 0 && move.PhaseStartedTick-priorEnd < ticksFor(5*time.Second, DefaultTickRate) {
					t.Fatal("frenzy replayed before its five-second cooldown")
				}
			}
			if move.Ended == vnet.MoveEndCompleted && move.ComboStep == 2 {
				priorEnd = uint32(h.tick)
			}
		}
	}
	if starts < 2 || priorEnd == 0 {
		t.Fatalf("fixture did not observe repeated full combinations: %d", starts)
	}
}

func TestVargrHeavyBiteMissLeavesTheWholeLongOpening(t *testing.T) {
	for _, rate := range []uint8{1, 3, DefaultTickRate} {
		t.Run(fmt.Sprint(rate), func(t *testing.T) {
			h := newVitalsHarness(t, rate, dropTerrain{groundTop: 63})
			p, out := h.join(1, [3]float32{.5, 64, .5})
			id := pullGuardian(t, h, [3]float64{.5, 64, -2.5}, p)
			atStage(h, id, 2)
			preferMove(h, id, vnet.EncounterMoveKindBonebreakerJaws)
			r := awaitComboPhase(t, h, p, id, 0, vnet.MovePhaseTelegraph)
			if r.def.kind != vnet.EncounterMoveKindBonebreakerJaws || r.phaseTicks != ticksFor(1500*time.Millisecond, rate) {
				t.Fatal("heavy bite lost its distinct preparation")
			}
			h.place(p, [3]float64{.5, 64, -5.5})
			health := h.vitals(p).Health
			recovery, ended := 0, false
			for range 200 {
				h.step()
				move := newestTimeline(t, out.all()).moves[0]
				if h.vitals(p).Health < health {
					t.Fatal("missed jaws grabbed a player outside their fixed cone")
				}
				if move.Ended == vnet.MoveEndCompleted {
					ended = true
					break
				}
				if move.Phase == vnet.MovePhaseRecovery {
					recovery++
					if move.PhaseTicks != ticksFor(2500*time.Millisecond, rate) || len(move.Hazards) != 0 {
						t.Fatalf("wrong missed-bite opening: %+v", move)
					}
				}
			}
			if !ended || recovery != int(ticksFor(2500*time.Millisecond, rate)) {
				t.Fatalf("missed bite recovery%d ended%v", recovery, ended)
			}
		})
	}
}

// #1036 owns detecting an all-down wipe and replacing the active boss. This test
// covers the established teardown/re-pull boundary it will use, without inventing
// a phase-only reset while the old boss still has phase-two health.
func TestDiscardedFrenzyCannotLeakIntoTheNextPull(t *testing.T) {
	h, p, _, id := comboFight(t, DefaultTickRate, vnet.EncounterMoveKindPrisonerClaws)
	awaitComboPhase(t, h, p, id, 2, vnet.MovePhaseRecovery)
	h.sim.mu.Lock()
	old := h.sim.mobs[id]
	encounterID := old.encounter.id
	h.sim.discardMobLocked(old)
	h.sim.mu.Unlock()
	if old.encounter != nil {
		t.Fatal("teardown retained frenzy state")
	}
	fresh := pullGuardian(t, h, [3]float64{.5, 64, -2.5}, p)
	m := h.sim.mobs[fresh]
	if m.encounter.id <= encounterID || m.encounter.phase != 1 || m.health != m.species().maxHealth || len(m.encounter.cooldowns) != 0 || len(m.encounter.lastUsed) != 0 || len(m.encounter.moves) != 0 {
		t.Fatal("new pull inherited the prior encounter's progression or selection state")
	}
	preferMove(h, fresh, vnet.EncounterMoveKindPrisonerClaws)
	r := awaitComboPhase(t, h, p, fresh, 0, vnet.MovePhaseTelegraph)
	if r.def.kind != vnet.EncounterMoveKindPrisonerClaws || r.comboTotal != 0 {
		t.Fatal("new pull inherited alternating phase-two claws")
	}
}
