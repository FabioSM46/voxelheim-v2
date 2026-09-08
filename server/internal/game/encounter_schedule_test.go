package game

import (
	"fmt"
	"math"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// The target enters each move's real range between roots. No catalogue or eligibility
// is rewritten; the session sees every pulse and each physical blow in full.
func TestKingPreferredCyclesPublishCompleteMovesAndOpenings(t *testing.T) {
	cases := []struct {
		stage uint8
		roots []vnet.EncounterMoveKind
	}{
		{1, []vnet.EncounterMoveKind{vnet.EncounterMoveKindKingsSentence, vnet.EncounterMoveKindSepulchreSpear, vnet.EncounterMoveKindThreeTolls}},
		{2, []vnet.EncounterMoveKind{vnet.EncounterMoveKindBurial, vnet.EncounterMoveKindKingsSentence, vnet.EncounterMoveKindSepulchreSpear, vnet.EncounterMoveKindThreeTolls, vnet.EncounterMoveKindEdictOfTheGraves}},
		{3, []vnet.EncounterMoveKind{vnet.EncounterMoveKindBurial, vnet.EncounterMoveKindKingsSentence, vnet.EncounterMoveKindEdictOfTheGraves, vnet.EncounterMoveKindSepulchreSpear, vnet.EncounterMoveKindRequiemOfTheBuried, vnet.EncounterMoveKindThreeTolls, vnet.EncounterMoveKindBurial}},
	}
	for _, rate := range []uint8{3, DefaultTickRate} {
		for _, tc := range cases {
			t.Run(fmt.Sprintf("%dHz_stage%d", rate, tc.stage), func(t *testing.T) {
				h := newVitalsHarness(t, rate, dropTerrain{groundTop: 63})
				p, out := h.join(1, [3]float32{.5, 64, .5})
				id := pullKing(t, h, [3]float64{.5, 64, -2.5}, p)
				atStage(h, id, tc.stage)
				roots, ended := 0, 0
				var previous uint64
				counts := map[uint64]map[vnet.MovePhase]uint32{}
				declared := map[uint64]map[vnet.MovePhase]uint32{}
				for range 5000 {
					h.sim.mu.Lock()
					m := h.sim.mobs[id]
					if m.encounter.running == nil && roots < len(tc.roots) {
						distance := 3.0
						if tc.roots[roots] == vnet.EncounterMoveKindSepulchreSpear {
							distance = 8
						}
						p.pos = [3]float64{m.pos[0], 64, m.pos[2] + distance}
						p.vel = [3]float64{}
					}
					h.sim.mu.Unlock()
					h.heal(p)
					h.step()
					for _, move := range newestTimeline(t, out.all()).moves {
						if move.Ended != vnet.MoveEndUnknown {
							if move.Ended != vnet.MoveEndCompleted || len(move.Hazards) != 0 {
								t.Fatal("cycle cancelled or retained ended hazards")
							}
							if counts[move.MoveInstanceID] == nil {
								t.Fatal("ending without a visible preparation")
							}
							for phase, want := range declared[move.MoveInstanceID] {
								if got := counts[move.MoveInstanceID][phase]; got != want {
									t.Fatalf("%v %v lasted%d want%d", move.Kind, phase, got, want)
								}
							}
							if move.ComboStep == move.ComboTotal {
								ended++
							}
							continue
						}
						if move.MoveInstanceID != previous {
							if move.MoveInstanceID <= previous {
								t.Fatal("move identity reused")
							}
							previous = move.MoveInstanceID
							if move.ComboStep <= 1 {
								if roots >= len(tc.roots) || move.Kind != tc.roots[roots] {
									t.Fatalf("root%d=%v want%v", roots, move.Kind, tc.roots)
								}
								if roots != ended {
									t.Fatal("new root interrupted preceding opening")
								}
								roots++
							}
							counts[previous] = map[vnet.MovePhase]uint32{}
							declared[previous] = map[vnet.MovePhase]uint32{}
						}
						counts[previous][move.Phase]++
						if move.Phase == vnet.MovePhaseChannel {
							declared[previous][move.Phase] = move.PhaseTicks * uint32(move.PulseTotal)
						} else {
							declared[previous][move.Phase] = move.PhaseTicks
							if move.Phase == vnet.MovePhaseTelegraph {
								declared[previous][move.Phase]++
							}
						}
						if move.Phase == vnet.MovePhaseRecovery && len(move.Hazards) != 0 {
							t.Fatal("recovery endangered player")
						}
						if move.Kind == vnet.EncounterMoveKindThreeTolls && move.ComboStep == 3 && move.Phase == vnet.MovePhaseRecovery && move.PhaseTicks != uint32(float64(rate)*2.2) {
							t.Fatal("last toll lost long opening")
						}
					}
					if ended == len(tc.roots) {
						return
					}
				}
				t.Fatalf("unfinished cycle roots%d ended%d", roots, ended)
			})
		}
	}
}

func TestKingSkipsUnavailablePreferencesWithoutCommittingThem(t *testing.T) {
	for _, scenario := range []string{"near_spear", "cooldown", "blocked_escape", "wall", "all_cooling"} {
		t.Run(scenario, func(t *testing.T) {
			h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
			p, _ := h.join(1, [3]float32{.5, 64, .5})
			id := pullKing(t, h, [3]float64{.5, 64, -2.5}, p)
			h.sim.mu.Lock()
			defer h.sim.mu.Unlock()
			m := h.sim.mobs[id]
			e := m.encounter
			e.scheduleStage, e.scheduleCursor = 1, 1 // spear preferred, but below its minimum
			if scenario == "cooldown" {
				e.scheduleCursor = 0
				e.cooldowns[vnet.EncounterMoveKindKingsSentence] = 100
			}
			if scenario == "all_cooling" {
				for _, d := range encounterMoveCatalog[m.kind] {
					e.cooldowns[d.kind] = 100
				}
			}
			if scenario == "wall" {
				h.sim.terrain = wallAt{groundTop: 63, fromZ: -1, toZ: -1}
			}
			if scenario == "blocked_escape" {
				h.sim.terrain = scriptedTerrain{want: func(x, y, z int64) bool {
					return y <= 63 || (y <= 67 && (x == -1 || x == 1 || z == 1)) || (z == -2 && y <= 67 && y != 65)
				}}
			}
			if scenario == "blocked_escape" && !clearLineOfSight(h.sim.terrain, boxCentre(m.species().body.boxAt(m.pos)), boxCentre(p.box())) {
				t.Fatal("escape fixture must retain line of sight")
			}
			cursor := e.scheduleCursor
			got, ok := m.selectEncounterMoveLocked(h.sim, p)
			if e.scheduleCursor != cursor || e.nextMoveID != 0 {
				t.Fatal("selection mutated commitment state")
			}
			wantOK := scenario == "near_spear" || scenario == "cooldown"
			if ok != wantOK {
				t.Fatalf("selected%v ok%v", got.kind, ok)
			}
			if ok {
				want := vnet.EncounterMoveKindKingsSentence
				if scenario == "cooldown" {
					want = vnet.EncounterMoveKindThreeTolls
				}
				if got.kind != want {
					t.Fatalf("selected%v want%v", got.kind, want)
				}
				m.beginEncounterMoveLocked(h.sim, got, p, 1)
				if e.scheduleCursor != 1 {
					t.Fatal("did not advance from actual committed slot")
				}
			}
		})
	}
}

// Walk real PlayerInput across successive pulses, carrying the actual reached body
// position forward. The small wall removes routes without replacing body collision.
func TestKingRitualsCanBeWalkedAcrossSuccessivePulses(t *testing.T) {
	for _, kind := range []vnet.EncounterMoveKind{vnet.EncounterMoveKindBurial, vnet.EncounterMoveKindEdictOfTheGraves, vnet.EncounterMoveKindRequiemOfTheBuried} {
		for _, wall := range []bool{false, true} {
			t.Run(fmt.Sprintf("%v_wall%v", kind, wall), func(t *testing.T) {
				terrain := scriptedTerrain{want: func(x, y, z int64) bool { return y <= 63 || (wall && x == 3 && z >= -1 && z <= 2 && y <= 66) }}
				h := newVitalsHarness(t, DefaultTickRate, terrain)
				p, _ := h.join(1, [3]float32{.5, 64, .5})
				id := pullKing(t, h, [3]float64{.5, 64, -3}, p)
				if kind != vnet.EncounterMoveKindBurial {
					h.sim.mu.Lock()
					x := 7.5
					if kind == vnet.EncounterMoveKindRequiemOfTheBuried {
						x = 6.5
					}
					p.pos = [3]float64{x, 64, -3}
					h.sim.mu.Unlock()
				}
				atStage(h, id, 3)
				preferMove(h, id, kind)
				start := p.pos
				var input protocol.PlayerInput
				walked := false
				seen := map[uint8]bool{}
				var lastPulse uint8 = 255
				for tick := uint32(1); tick < 400; tick++ {
					r := runningMoveOf(h, id)
					if r != nil && (r.phase == vnet.MovePhaseTelegraph && lastPulse == 255 || r.phase == vnet.MovePhaseChannel && r.pulseIndex != lastPulse) {
						lastPulse = r.pulseIndex
						if r.phase == vnet.MovePhaseChannel {
							seen[lastPulse] = true
						}
						input.MoveX, input.MoveZ = 0, 0
						h.sim.mu.Lock()
						safe := !anyHazardReaches(r.hazards, p.box())
						if !safe {
							for bearing := range escapeBearings {
								angle := 2 * math.Pi * float64(bearing) / escapeBearings
								destination, reachable := h.sim.walkEscapeRoute(p.pos, angle, r.remaining)
								if reachable && !anyHazardReaches(r.hazards, playerBox(destination)) {
									walked = true
									input.MoveX = float32(math.Cos(angle))
									input.MoveZ = -float32(math.Sin(angle))
									safe = true
									break
								}
							}
						}
						h.sim.mu.Unlock()
						if !safe {
							t.Fatalf("no reachable route from actual pulse%d body", lastPulse)
						}
					}
					if r != nil && r.phase == vnet.MovePhaseChannel {
						seen[r.pulseIndex] = true
					}
					if r != nil && !anyHazardReaches(r.hazards, p.box()) {
						input.MoveX, input.MoveZ = 0, 0
					}
					input.ClientTick = tick
					if err := p.Submit(input); err != nil {
						t.Fatal(err)
					}
					h.step()
					if h.vitals(p).Health != PlayerMaxHealth {
						t.Fatalf("pulse%d hurt walking player", lastPulse)
					}
					r = runningMoveOf(h, id)
					if r != nil && r.phase == vnet.MovePhaseRecovery {
						if math.Hypot(p.pos[0]-start[0], p.pos[2]-start[2]) < 0.5 {
							t.Fatal("body did not actually walk out")
						}
						if !walked {
							t.Fatal("fixture required no movement")
						}
						if len(seen) != int(r.def.pulses) {
							t.Fatal("did not walk every pulse")
						}
						return
					}
				}
				t.Fatal("ritual never finished")
			})
		}
	}
}

func TestKingPhaseChangeWaitsForTheCommittedCastAndItsRecovery(t *testing.T) {
	for _, phase := range []vnet.MovePhase{vnet.MovePhaseTelegraph, vnet.MovePhaseChannel, vnet.MovePhaseRecovery} {
		t.Run(phase.String(), func(t *testing.T) {
			h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
			p, out := h.join(1, [3]float32{.5, 64, .5})
			id := pullKing(t, h, [3]float64{.5, 64, -3}, p)
			atStage(h, id, 2)
			preferMove(h, id, vnet.EncounterMoveKindEdictOfTheGraves)
			var original uint64
			var recovery uint32
			for range 500 {
				h.heal(p)
				h.step()
				r := runningMoveOf(h, id)
				if r != nil && r.def.kind == vnet.EncounterMoveKindEdictOfTheGraves && r.phase == phase {
					original = r.instanceID
					recovery = r.ticks.recovery
					break
				}
			}
			if original == 0 {
				t.Fatal("cast never reached requested phase")
			}
			// This leaves the committed instance untouched, as health advancement does.
			atStage(h, id, 3)
			seenRecovery := uint32(0)
			if phase == vnet.MovePhaseRecovery {
				seenRecovery = 1
			}
			ended := false
			for range 500 {
				h.heal(p)
				h.step()
				frame := newestTimeline(t, out.all())
				if frame.phase != 3 {
					t.Fatal("new health stage not published")
				}
				for _, move := range frame.moves {
					if move.MoveInstanceID == original {
						if move.Ended != vnet.MoveEndUnknown {
							if move.Ended != vnet.MoveEndCompleted || seenRecovery != recovery {
								t.Fatalf("cast ending%v recovery%d want%d", move.Ended, seenRecovery, recovery)
							}
							ended = true
						} else if move.Phase == vnet.MovePhaseRecovery {
							seenRecovery++
						}
					} else if move.Ended == vnet.MoveEndUnknown {
						if !ended || move.Kind != vnet.EncounterMoveKindBurial || move.Phase != vnet.MovePhaseTelegraph {
							t.Fatal("new stage preempted cast or failed to start its preferred cycle")
						}
						return
					}
				}
			}
			t.Fatal("phase transition never reached next root")
		})
	}
}

func TestKingScheduleHasNoAttackQueuedBeyondWithdrawalOrFreshEncounter(t *testing.T) {
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	p, _ := h.join(1, [3]float32{.5, 64, .5})
	id := pullKing(t, h, [3]float64{.5, 64, -3}, p)
	atStage(h, id, 3)
	h.step()
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	m := h.sim.mobs[id]
	old := m.encounter.id
	withdrawEncounterMovesLocked(m)
	if m.encounter.running != nil {
		t.Fatal("withdrawal left executing cast")
	}
	for _, move := range m.encounter.moves {
		if move.Ended == vnet.MoveEndUnknown || len(move.Hazards) != 0 {
			t.Fatal("withdrawal left dangerous announcement")
		}
	}
	// Exercise the existing fresh-pull constructor, not the future dungeon wipe reset.
	m.encounter = nil
	h.sim.startBossEncounterLocked(m, p)
	if m.encounter.id == old || m.encounter.scheduleCursor != 0 || m.encounter.scheduleStage != 0 {
		t.Fatal("fresh encounter inherited cycle")
	}
	got, ok := m.selectEncounterMoveLocked(h.sim, p)
	if !ok || got.kind != vnet.EncounterMoveKindKingsSentence {
		t.Fatalf("fresh pull selected%v ok%v", got.kind, ok)
	}
}

// The same starts are dangerous without input; unchanged health above therefore
// measures avoidance, not protection or pulses that never reached the fixture.
func TestKingRitualWalkingStartsAreNotAlreadySafe(t *testing.T) {
	for _, kind := range []vnet.EncounterMoveKind{vnet.EncounterMoveKindBurial, vnet.EncounterMoveKindEdictOfTheGraves, vnet.EncounterMoveKindRequiemOfTheBuried} {
		t.Run(kind.String(), func(t *testing.T) {
			h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
			p, _ := h.join(1, [3]float32{.5, 64, .5})
			id := pullKing(t, h, [3]float64{.5, 64, -3}, p)
			if kind != vnet.EncounterMoveKindBurial {
				h.sim.mu.Lock()
				x := 7.5
				if kind == vnet.EncounterMoveKindRequiemOfTheBuried {
					x = 6.5
				}
				p.pos = [3]float64{x, 64, -3}
				h.sim.mu.Unlock()
			}
			atStage(h, id, 3)
			preferMove(h, id, kind)
			for range 250 {
				h.step()
				if h.vitals(p).Health < PlayerMaxHealth {
					return
				}
			}
			t.Fatal("stationary control was never hit")
		})
	}
}
