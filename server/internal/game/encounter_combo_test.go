package game

import (
	"fmt"
	"reflect"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

func comboFight(t *testing.T, rate uint8, kind vnet.EncounterMoveKind) (*vitalsHarness, *Player, *dropSink, uint64) {
	t.Helper()
	h := newVitalsHarness(t, rate, dropTerrain{groundTop: 63})
	p, out := h.join(1, [3]float32{.5, 64, .5})
	var id uint64
	if kind != vnet.EncounterMoveKindThreeTolls {
		id = pullGuardian(t, h, [3]float64{.5, 64, -2.5}, p)
	} else {
		id = pullKing(t, h, [3]float64{.5, 64, -2.5}, p)
	}
	if kind == vnet.EncounterMoveKindPrisonerClaws {
		atStage(h, id, 2)
	}
	preferMove(h, id, kind)
	return h, p, out, id
}

func awaitComboPhase(t *testing.T, h *vitalsHarness, p *Player, id uint64, step uint8, phase vnet.MovePhase) *runningMove {
	t.Helper()
	for range 1000 {
		h.heal(p)
		h.step()
		if r := runningMoveOf(h, id); r != nil && r.comboStep == step && r.phase == phase {
			return r
		}
	}
	t.Fatalf("never reached step %d %v", step, phase)
	return nil
}

// Drive real ticks and read the bytes delivered to the session. The damage and
// phase counts assert the complete combination, not just its catalogue definition.
func TestPhysicalComboPublishesEveryBlowAndEarnsItsFinalOpening(t *testing.T) {
	for _, rate := range []uint8{1, 3, DefaultTickRate} {
		for _, kind := range []vnet.EncounterMoveKind{vnet.EncounterMoveKindBiteAndTear, vnet.EncounterMoveKindThreeTolls, vnet.EncounterMoveKindPrisonerClaws} {
			t.Run(fmt.Sprintf("%dHz_%v", rate, kind), func(t *testing.T) {
				h, p, out, id := comboFight(t, rate, kind)
				total := uint8(2)
				final := 1800 * time.Millisecond
				if kind == vnet.EncounterMoveKindThreeTolls {
					total = 3
					final = 2200 * time.Millisecond
				}
				telegraph, release := 900*time.Millisecond, 200*time.Millisecond
				if kind == vnet.EncounterMoveKindPrisonerClaws {
					telegraph, release = time.Second, 300*time.Millisecond
				}
				ids := map[uint8]uint64{}
				hits := map[uint8]int{}
				counts := map[uint8]map[vnet.MovePhase]int{}
				geometry := map[uint8][]protocol.HazardVolume{}
				finished := false
				for range 1000 {
					h.heal(p)
					before := h.vitals(p).Health
					h.step()
					state := newestTimeline(t, out.all())
					for _, move := range state.moves {
						if move.Kind != kind || move.ComboTotal != total || move.ComboStep == 0 || move.ComboStep > total {
							t.Fatalf("unrelated move inside committed sequence: %+v", move)
						}
						if prior, seen := ids[move.ComboStep]; seen && prior != move.MoveInstanceID {
							t.Fatal("a step replayed under a second identity")
						}
						ids[move.ComboStep] = move.MoveInstanceID
						if move.PulseIndex != 0 || move.PulseTotal != 0 || move.Phase == vnet.MovePhaseChannel {
							t.Fatal("physical combination used channel fields")
						}
						if move.Ended != vnet.MoveEndUnknown {
							if move.Ended != vnet.MoveEndCompleted {
								t.Fatalf("unexpected ending %v", move.Ended)
							}
							if len(move.Hazards) != 0 {
								t.Fatal("ended blow remains dangerous")
							}
							if move.ComboStep == total {
								finished = true
							}
							continue
						}
						if counts[move.ComboStep] == nil {
							counts[move.ComboStep] = map[vnet.MovePhase]int{}
						}
						counts[move.ComboStep][move.Phase]++
						if move.Phase == vnet.MovePhaseRecovery {
							expected := ticksFor(400*time.Millisecond, rate)
							if move.ComboStep == total {
								expected = ticksFor(final, rate)
							}
							if move.PhaseTicks != expected || len(move.Hazards) != 0 {
								t.Fatalf("wrong opening: %+v", move)
							}
						} else {
							if old, seen := geometry[move.ComboStep]; seen && !reflect.DeepEqual(old, move.Hazards) {
								t.Fatal("announced geometry changed during a blow")
							}
							geometry[move.ComboStep] = move.Hazards
						}
						if h.vitals(p).Health < before {
							if move.Phase != vnet.MovePhaseRelease || !anyHazardReaches(move.Hazards, p.box()) {
								t.Fatal("damage outside the published release")
							}
							hits[move.ComboStep]++
						}
					}
					if finished {
						break
					}
				}
				if !finished || len(ids) != int(total) {
					t.Fatalf("incomplete sequence: %v", ids)
				}
				for step := uint8(1); step <= total; step++ {
					if hits[step] != 1 {
						t.Fatalf("step%d landed %d times", step, hits[step])
					}
					if step > 1 && ids[step] <= ids[step-1] {
						t.Fatal("a new blow reused an identity")
					}
					// Preserve the existing initial announcement tick followed by the declared preparation.
					if counts[step][vnet.MovePhaseTelegraph] != int(ticksFor(telegraph, rate))+1 {
						t.Fatal("a blow lost its complete preparation")
					}
					if counts[step][vnet.MovePhaseRelease] != int(ticksFor(release, rate)) {
						t.Fatal("release duration changed")
					}
					recovery := ticksFor(400*time.Millisecond, rate)
					if step == total {
						recovery = ticksFor(final, rate)
					}
					if counts[step][vnet.MovePhaseRecovery] != int(recovery) {
						t.Fatalf("step%d recovery=%d want%d", step, counts[step][vnet.MovePhaseRecovery], recovery)
					}
				}
				if kind == vnet.EncounterMoveKindThreeTolls {
					if geometry[1][0].Direction[0] <= 0 || geometry[2][0].Direction[0] >= 0 || geometry[3][0].Shape != vnet.HazardShapeLine {
						t.Fatalf("tolls are not left, right and thrust: %+v", geometry)
					}
				}
				if kind == vnet.EncounterMoveKindPrisonerClaws && (geometry[1][0].Direction[0] <= 0 || geometry[2][0].Direction[0] >= 0) {
					t.Fatalf("claws did not alternate sides: %+v", geometry)
				}
				if r := runningMoveOf(h, id); r != nil {
					t.Fatal("final opening did not complete")
				}
			})
		}
	}
}

func TestPhysicalComboRetargetsOnlyAtTheNextTelegraph(t *testing.T) {
	h, p, out, id := comboFight(t, DefaultTickRate, vnet.EncounterMoveKindBiteAndTear)
	second, _ := h.join(2, [3]float32{2.5, 64, -2.5})
	first := awaitComboPhase(t, h, p, id, 1, vnet.MovePhaseTelegraph)
	locked := append([]protocol.HazardVolume(nil), first.hazards...)
	h.sim.mu.Lock()
	h.sim.mobs[id].addThreatLocked(second.entityID, 10000)
	h.sim.mu.Unlock()
	awaitComboPhase(t, h, p, id, 1, vnet.MovePhaseRelease)
	if !reflect.DeepEqual(runningMoveOf(h, id).hazards, locked) {
		t.Fatal("threat re-aimed the released blow")
	}
	next := awaitComboPhase(t, h, p, id, 2, vnet.MovePhaseTelegraph)
	if next.instanceID == first.instanceID || next.aim[0] < .99 {
		t.Fatalf("second telegraph did not acquire the new target: %+v", next)
	}
	before := newestTimeline(t, out.all()).moves
	h.place(second, [3]float64{-2.5, 64, -2.5})
	awaitComboPhase(t, h, p, id, 2, vnet.MovePhaseRelease)
	after := newestTimeline(t, out.all()).moves
	if !reflect.DeepEqual(before[len(before)-1].Hazards, after[len(after)-1].Hazards) {
		t.Fatal("second blow followed its moving target")
	}
}

// Range, terrain and reachable space are revalidated at every intermediate seam.
// Failing one grants a complete final recovery on the same immutable instance.
func TestPhysicalComboFailedContinuationGrantsFinalRecovery(t *testing.T) {
	for _, kind := range []vnet.EncounterMoveKind{vnet.EncounterMoveKindBiteAndTear, vnet.EncounterMoveKindThreeTolls, vnet.EncounterMoveKindPrisonerClaws} {
		total := uint8(2)
		final := 1800 * time.Millisecond
		if kind == vnet.EncounterMoveKindThreeTolls {
			total = 3
			final = 2200 * time.Millisecond
		}
		for step := uint8(1); step < total; step++ {
			for _, reason := range []string{"range", "wall", "no escape"} {
				t.Run(fmt.Sprintf("%v_%d_%s", kind, step, reason), func(t *testing.T) {
					h, p, out, id := comboFight(t, DefaultTickRate, kind)
					prior := awaitComboPhase(t, h, p, id, step, vnet.MovePhaseRecovery)
					switch reason {
					case "range":
						h.place(p, [3]float64{.5, 64, 12.5})
					case "wall":
						h.sim.terrain = monolith{groundTop: 63, fromZ: -1, toZ: -1}
					case "no escape":
						aperture := int64(65)
						if kind != vnet.EncounterMoveKindThreeTolls {
							aperture = 64
						}
						h.sim.terrain = scriptedTerrain{want: func(x, y, z int64) bool {
							return y <= 63 || (y <= 67 && (x == -1 || x == 1 || z == 1)) || (z == -2 && y <= 67 && y != aperture)
						}}
					}
					h.sim.mu.Lock()
					m := h.sim.mobs[id]
					candidate := prior.def.forComboStep(step + 1)
					los := clearLineOfSight(h.sim.terrain, boxCentre(m.species().body.boxAt(m.pos)), boxCentre(p.box()))
					escape := h.sim.moveLeavesAnEscapeLocked(m, candidate, p)
					h.sim.mu.Unlock()
					if reason == "wall" && los {
						t.Fatal("wall fixture left line of sight")
					}
					if reason == "no escape" && (!los || escape) {
						t.Fatalf("escape fixture: LOS=%v escape=%v", los, escape)
					}
					h.advance(int(prior.remaining) + 1)
					r := runningMoveOf(h, id)
					if r == nil || r.instanceID != prior.instanceID || !r.comboStopped || r.phase != vnet.MovePhaseRecovery || r.phaseTicks != ticksFor(final, DefaultTickRate) {
						t.Fatalf("failed continuation erased final opening: %+v", r)
					}
					start := r.startedTick
					health := h.vitals(p).Health
					// Return a valid target now: recovery must still run without a continuation retry.
					h.sim.terrain = dropTerrain{groundTop: 63}
					h.place(p, [3]float64{.5, 64, .5})
					for r != nil {
						if r.instanceID != prior.instanceID || r.comboStep != step || r.comboTotal != total || r.startedTick != start {
							t.Fatal("failed combination resumed or mutated metadata")
						}
						h.step()
						if h.vitals(p).Health < health {
							t.Fatal("failure opening dealt damage")
						}
						r = runningMoveOf(h, id)
					}
					last := newestTimeline(t, out.all()).moves
					if len(last) != 1 || last[0].Ended != vnet.MoveEndCompleted || last[0].ComboStep != step || last[0].ComboTotal != total {
						t.Fatalf("lost aborted position at ending: %+v", last)
					}
				})
			}
		}
	}
}

func TestPhysicalComboKeepsCommitmentAndRecoveryWhenHealthStageChanges(t *testing.T) {
	for _, kind := range []vnet.EncounterMoveKind{vnet.EncounterMoveKindBiteAndTear, vnet.EncounterMoveKindThreeTolls} {
		for _, phase := range []vnet.MovePhase{vnet.MovePhaseTelegraph, vnet.MovePhaseRelease, vnet.MovePhaseRecovery} {
			t.Run(fmt.Sprintf("%v_%v", kind, phase), func(t *testing.T) {
				h, p, out, id := comboFight(t, DefaultTickRate, kind)
				r := awaitComboPhase(t, h, p, id, 1, phase)
				oldID := r.instanceID
				h.sim.mu.Lock()
				m := h.sim.mobs[id]
				m.health = 1
				h.sim.advanceEncounterPhasesLocked()
				h.sim.mu.Unlock()
				// The remaining sequence must still be the committed kind even though higher
				// stage moves have become available; final opening must keep its complete count.
				ended := false
				recoveryFrames := 0
				for range 500 {
					h.heal(p)
					h.step()
					for _, move := range newestTimeline(t, out.all()).moves {
						if move.Kind != kind || move.ComboStep == 0 {
							t.Fatal("health phase inserted an unrelated move")
						}
						if move.ComboStep == 1 && move.MoveInstanceID != oldID {
							t.Fatal("phase restarted the first blow")
						}
						if move.ComboStep == move.ComboTotal {
							if move.Ended != vnet.MoveEndUnknown {
								ended = true
							} else if move.Phase == vnet.MovePhaseRecovery {
								recoveryFrames++
							}
						}
					}
					if ended {
						break
					}
				}
				expected := 36
				if kind == vnet.EncounterMoveKindThreeTolls {
					expected = 44
				}
				if !ended || recoveryFrames != expected {
					t.Fatalf("phase erased final opening: frames%d expected%d", recoveryFrames, expected)
				}
			})
		}
	}
}

func TestPhysicalComboLifecycleCancellationDropsAllFutureBlows(t *testing.T) {
	for _, kind := range []vnet.EncounterMoveKind{vnet.EncounterMoveKindBiteAndTear, vnet.EncounterMoveKindThreeTolls, vnet.EncounterMoveKindPrisonerClaws} {
		total := uint8(2)
		if kind == vnet.EncounterMoveKindThreeTolls {
			total = 3
		}
		for step := uint8(1); step <= total; step++ {
			for _, phase := range []vnet.MovePhase{vnet.MovePhaseTelegraph, vnet.MovePhaseRelease, vnet.MovePhaseRecovery} {
				for _, reason := range []string{"death", "wipe", "withdrawal", "world removal"} {
					t.Run(fmt.Sprintf("%v_%d_%v_%s", kind, step, phase, reason), func(t *testing.T) {
						h, p, out, id := comboFight(t, DefaultTickRate, kind)
						r := awaitComboPhase(t, h, p, id, step, phase)
						h.sim.mu.Lock()
						m := h.sim.mobs[id]
						e := m.encounter
						switch reason {
						case "death":
							h.sim.damageMobLocked(m, m.health)
						case "wipe":
							p.damageLocked(p.health)
						case "withdrawal":
							p.pos = [3]float64{100.5, 64, 100.5}
							p.chunk = chunkAt(p.pos)
						case "world removal":
							h.sim.discardMobLocked(m)
						}
						h.sim.mu.Unlock()
						var endings []protocol.EncounterMove
						if reason == "wipe" || reason == "withdrawal" {
							// The live-player withdrawal runs before any damage or continuation on a tick.
							h.step()
							endings = newestTimeline(t, out.all()).moves
						} else {
							endings = e.moves
						}
						if e.running != nil {
							t.Fatal("cancelled encounter retained future work")
						}
						found := false
						for _, move := range endings {
							if move.MoveInstanceID != r.instanceID {
								continue
							}
							found = true
							if move.Ended != vnet.MoveEndCancelled || len(move.Hazards) != 0 || move.ComboStep != step || move.ComboTotal != total {
								t.Fatalf("cancellation lost immutable metadata: %+v", move)
							}
						}
						if !found {
							t.Fatal("cancelled instance vanished without an ending")
						}
						// Keep the old encounter as the witness, even when world removal dropped it.
						lastID := e.nextMoveID
						if reason == "wipe" {
							h.place(p, [3]float64{100.5, 64, 100.5})
						}
						before := h.vitals(p).Health
						h.advance(10)
						if e.running != nil || e.nextMoveID != lastID || h.vitals(p).Health < before {
							t.Fatal("cancelled combination replayed or damaged later")
						}
					})
				}
			}
		}
	}
}

func TestPhysicalComboCatalogueIsBoundedAndEveryStepCanBeAnnounced(t *testing.T) {
	h, p, _, id := comboFight(t, DefaultTickRate, vnet.EncounterMoveKindBiteAndTear)
	m := h.sim.mobs[id]
	for _, repertoire := range encounterMoveCatalog {
		for _, def := range repertoire {
			if def.combo == nil {
				continue
			}
			if def.combo.total < 2 || def.combo.total > 3 || def.pulses != 0 || def.travel != travelNone || def.flightSpeed != 0 {
				t.Fatalf("invalid combo catalogue %+v", def)
			}
			for step := uint8(1); step <= def.combo.total; step++ {
				one := def.forComboStep(step)
				m.beginEncounterComboBlowLocked(h.sim, one, p, 1, step)
				frame := protocol.EncounterTimeline{EncounterID: 1, BossEntityID: id, Boss: m.kind, Phase: 1, Moves: []protocol.EncounterMove{m.encounter.running.announcement()}}
				if _, err := protocol.EncodeEncounterTimeline(frame); err != nil {
					t.Fatalf("unencodable %v step%d: %v", def.kind, step, err)
				}
			}
		}
	}
}

func TestPhysicalComboFinalOpeningSurvivesAHealthStageChange(t *testing.T) {
	for _, kind := range []vnet.EncounterMoveKind{vnet.EncounterMoveKindBiteAndTear, vnet.EncounterMoveKindThreeTolls} {
		h, p, out, id := comboFight(t, DefaultTickRate, kind)
		total := uint8(2)
		duration := uint32(36)
		if kind == vnet.EncounterMoveKindThreeTolls {
			total = 3
			duration = 44
		}
		r := awaitComboPhase(t, h, p, id, total, vnet.MovePhaseRecovery)
		instance, start := r.instanceID, r.startedTick
		h.sim.mu.Lock()
		m := h.sim.mobs[id]
		m.health = 1
		h.sim.advanceEncounterPhasesLocked()
		h.sim.mu.Unlock()
		frames := uint32(1)
		for range duration {
			h.step()
			move := newestTimeline(t, out.all()).moves[0]
			if move.MoveInstanceID != instance || move.ComboStep != total || move.ComboTotal != total || move.PhaseStartedTick != start || move.PhaseTicks != duration {
				t.Fatalf("phase changed earned opening: %+v", move)
			}
			if move.Ended == vnet.MoveEndCompleted {
				break
			}
			frames++
		}
		if frames != duration || runningMoveOf(h, id) != nil {
			t.Fatalf("earned opening ran%d frames, wanted%d", frames, duration)
		}
	}
}
