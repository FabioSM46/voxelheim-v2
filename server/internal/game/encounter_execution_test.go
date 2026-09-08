package game

import (
	"math"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// pullGuardian places the Vargr guardian, freezes the encounter against a player, and
// answers with the boss's entity id. Taken by hand rather than by walking somebody into
// the arena, for boss_species_test.go's reason: a fight that started because of an aggro
// radius would be measuring the aggro radius.
func pullGuardian(t *testing.T, h *vitalsHarness, at [3]float64, first *Player) uint64 {
	t.Helper()
	id := h.placeSpeciesAt(vnet.MobKindVargrGuardian, at)
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	h.sim.startBossEncounterLocked(h.sim.mobs[id], first)
	return id
}

// runningMoveOf is the instance this encounter is executing, or nil.
func runningMoveOf(h *vitalsHarness, id uint64) *runningMove {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	m := h.sim.mobs[id]
	if m == nil || m.encounter == nil {
		return nil
	}
	return m.encounter.running
}

// preferMove puts every other move out of the least-recently-used running so the named one
// is chosen next. It never edits the catalog — what it moves is the *encounter's* ledger,
// because a test that rewrote the repertoire would be testing one no server has.
func preferMove(h *vitalsHarness, id uint64, want vnet.EncounterMoveKind) {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	m := h.sim.mobs[id]
	for _, def := range encounterMoveCatalog[m.kind] {
		if def.kind != want {
			m.encounter.lastUsed[def.kind] = math.MaxUint32
		}
	}
}

// atStage drives an encounter to a stage by taking the health that stage begins at.
func atStage(h *vitalsHarness, id uint64, stage uint8) {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	m := h.sim.mobs[id]
	def := m.species()
	if stage > 1 {
		percent := uint32(def.phaseHealthPercents[stage-2])
		m.health = uint16(uint32(def.maxHealth) * percent / 100)
	}
	m.encounter.phase = stage
}

// monolith is flat ground with one solid slab across a band of z: the approved design's
// monolith, the thing a charge is meant to be steered into.
type monolith struct {
	groundTop  int64
	fromZ, toZ int64
}

func (w monolith) Block(_, y, z int64) (world.Block, bool) {
	if y <= w.groundTop || (z >= w.fromZ && z <= w.toZ && y <= w.groundTop+4) {
		return world.Stone, true
	}
	return world.Air, true
}

func (w monolith) Fluid(x, y, z int64) bool { return fluidByBlock(w, x, y, z) }

func (w monolith) Solid(x, y, z int64) bool {
	block, resident := w.Block(x, y, z)
	return !resident || block != world.Air
}

// The preparation reaches a player before the damage exists, and the damage happens on
// the ticks the announcement named.
//
// **The promise the whole encounter contract rests on**, failing in two directions worth
// separating: damage during a telegraph puts an attack in front of a player given nothing
// to react to, and damage outside a release makes the announced window a decoration.
func TestAnAnnouncedMoveIsPreparedBeforeItIsDangerous(t *testing.T) {
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	boss := pullGuardian(t, h, [3]float64{0.5, 64, -2.5}, player)

	var telegraphTicks, hurtAt int
	for tick := 1; tick <= 60; tick++ {
		before := h.vitals(player).Health
		h.step()
		running := runningMoveOf(h, boss)
		if running != nil && running.phase == vnet.MovePhaseTelegraph {
			telegraphTicks++
		}
		if h.vitals(player).Health < before {
			if running == nil || running.phase != vnet.MovePhaseRelease {
				t.Fatalf("tick %d hurt the player outside a release window: %+v", tick, running)
			}
			hurtAt = tick
			break
		}
	}
	if hurtAt == 0 {
		t.Fatal("a pulled boss standing in contact never hurt anybody")
	}

	// The declared count, plus the frame the move was announced on. A move is published
	// the moment it is chosen and *then* spends its declared telegraph ticks, so a client
	// sees one more telegraph frame than the count it is told to count down — and the
	// count is what it counts down, which is asserted off the wire below.
	want := ticksFor(encounterMoveCatalog[vnet.MobKindVargrGuardian][0].telegraph, DefaultTickRate)
	if uint32(telegraphTicks) != want+1 {
		t.Fatalf("the telegraph ran %d frames before the first blow, want %d (%d declared, plus the announcement)",
			telegraphTicks, want+1, want)
	}

	// And the frame the client held on the tick it was hurt says so: the release phase,
	// the declared window, and the region. A frame that had already advanced to recovery
	// would be telling a player nothing was dangerous on the tick it damaged them.
	timeline := newestTimeline(t, out.all())
	if len(timeline.moves) != 1 {
		t.Fatalf("the encounter announced %d moves at the blow", len(timeline.moves))
	}
	hurtBy := timeline.moves[0]
	if hurtBy.Phase != vnet.MovePhaseRelease {
		t.Fatalf("the tick that dealt damage was published as %v", hurtBy.Phase)
	}
	if len(hurtBy.Hazards) == 0 {
		t.Fatal("the dangerous phase announced no region at all")
	}
	if hurtBy.PhaseTicks != ticksFor(encounterMoveCatalog[vnet.MobKindVargrGuardian][0].release, DefaultTickRate) {
		t.Fatalf("the release announced %d ticks", hurtBy.PhaseTicks)
	}
}

// One release window lands on each target exactly once, and a second instance may land
// again.
//
// The ledger is keyed to the window rather than the instance, which is the pair of
// assertions here: a multi-tick release is one blow, and the next move is a new one.
func TestOneReleaseWindowLandsOnceOnEachTarget(t *testing.T) {
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	near, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	beside, _ := h.join(2, [3]float32{1.1, 64, 0.5})
	boss := pullGuardian(t, h, [3]float64{0.8, 64, -2.5}, near)

	blow := moveDamage(mobRegistry[vnet.MobKindVargrGuardian], encounterMoveCatalog[vnet.MobKindVargrGuardian][0])
	for _, p := range []*Player{near, beside} {
		if got := h.vitals(p).Health; got != PlayerMaxHealth {
			t.Fatalf("a player started at %d health", got)
		}
	}

	// Through the first instance in full: telegraph, the whole release, and into recovery.
	for range 30 {
		h.step()
		if running := runningMoveOf(h, boss); running != nil && running.phase == vnet.MovePhaseRecovery {
			break
		}
	}
	for _, p := range []*Player{near, beside} {
		if got, want := h.vitals(p).Health, PlayerMaxHealth-blow; got != want {
			t.Fatalf("one release window left a player at %d, want exactly one blow (%d)", got, want)
		}
	}

	// And on through the next instance, which is a different window and lands again.
	for range 90 {
		h.step()
		if h.vitals(near).Health < PlayerMaxHealth-blow {
			return
		}
	}
	t.Fatal("a second announced move never landed on anybody")
}

// Leaving the announced region is the answer, and it is the whole of the answer.
//
// The cone is fixed when the move is chosen and never re-aimed, so a player outside it
// takes nothing. A move that tracked its target would make this test impossible to write,
// which is exactly why it is written.
func TestLeavingTheAnnouncedRegionIsTheAnswer(t *testing.T) {
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	inside, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	pullGuardian(t, h, [3]float64{0.5, 64, -2.5}, inside)

	// Somebody standing well behind the creature, on the opposite side of the cone it
	// will open toward the player it locked.
	behind, _ := h.join(2, [3]float32{0.5, 64, -5.5})

	for range 40 {
		h.step()
		if h.vitals(inside).Health < PlayerMaxHealth {
			break
		}
	}
	if got := h.vitals(inside).Health; got == PlayerMaxHealth {
		t.Fatal("the player the cone was aimed at was never hit")
	}
	if got := h.vitals(behind).Health; got != PlayerMaxHealth {
		t.Fatalf("a player outside the announced cone lost health, ending at %d", got)
	}
}

// A charge is resolved across the whole of each tick, not sampled at the end of it.
//
// **Run at five hertz, where the creature covers 2.2 blocks a tick.** It asserts the thing
// that makes the sweep necessary rather than merely asserting a hit: the creature's box
// never overlaps the player's on any tick, so an endpoint test would report no contact at
// all, and the player is hit anyway.
func TestAChargeIsSweptThroughTheWholeTickRatherThanSampledAtIt(t *testing.T) {
	const coarse = 5
	h := newVitalsHarness(t, coarse, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	boss := pullGuardian(t, h, [3]float64{0.5, 64, -7.6}, player)
	preferMove(h, boss, vnet.EncounterMoveKindCollarCharge)

	overlapped := false
	for range 40 {
		h.step()
		h.sim.mu.Lock()
		m := h.sim.mobs[boss]
		if boxDistance(m.species().body.boxAt(m.pos), player.box()) == 0 {
			overlapped = true
		}
		h.sim.mu.Unlock()
		if h.vitals(player).Health < PlayerMaxHealth {
			break
		}
	}
	if overlapped {
		t.Skip("the charge's body overlapped the target, so this run does not test the sweep")
	}
	blow := moveDamage(mobRegistry[vnet.MobKindVargrGuardian], catalogued(t, vnet.EncounterMoveKindCollarCharge))
	if got, want := h.vitals(player).Health, PlayerMaxHealth-blow; got != want {
		t.Fatalf("a charge that passed straight through the target left it at %d, want %d", got, want)
	}
}

// Terrain stops a charge, and stopping it buys the approved design's longer opening.
//
// The wall is the continuous half of the collision requirement: displacement goes through
// the same [moveAndCollide] a player's does, so no speed carries the creature past it. The
// recovery is the design's monolith reward, a server number rather than an animation's.
func TestAChargeIsStoppedByTerrainAndPaysTheLongerRecovery(t *testing.T) {
	// The slab stands past the player, which is the design's own picture of this move: the
	// lane runs through whoever it was aimed at and ends against the stone behind them.
	const wallFrom, wallTo = 2, 3
	h := newVitalsHarness(t, DefaultTickRate, monolith{groundTop: 63, fromZ: wallFrom, toZ: wallTo})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	boss := pullGuardian(t, h, [3]float64{0.5, 64, -6.6}, player)
	preferMove(h, boss, vnet.EncounterMoveKindCollarCharge)

	var recovery *runningMove
	for range 60 {
		h.step()
		h.sim.mu.Lock()
		m := h.sim.mobs[boss]
		crossed := m.species().body.boxAt(m.pos).max[2] > float64(wallFrom)
		h.sim.mu.Unlock()
		if crossed {
			t.Fatal("the charge crossed into the monolith")
		}
		if running := runningMoveOf(h, boss); running != nil && running.phase == vnet.MovePhaseRecovery {
			recovery = running
			break
		}
	}
	if recovery == nil {
		t.Fatal("the charge never reached its recovery")
	}
	if !recovery.impacted {
		t.Fatal("the charge stopped at the monolith without recording the impact")
	}
	want := ticksFor(catalogued(t, vnet.EncounterMoveKindCollarCharge).impactRecovery, DefaultTickRate)
	if recovery.phaseTicks != want {
		t.Fatalf("the recovery after an impact lasts %d ticks, want %d", recovery.phaseTicks, want)
	}
}

// No blow lands outside the region that was announced, over a whole fight.
//
// **The acceptance criterion executed rather than restated.** Every tick that costs a
// player health is checked against the announcement live at that moment, read back off the
// wire: the player's box has to be inside a region the client had already been shown.
func TestNoBlowLandsOutsideTheRegionThatWasAnnounced(t *testing.T) {
	// **Two placements and two rates, and the pairing is what the test is worth.** With the
	// contact placement alone this passed while covering nothing that could break it: a
	// player parked in contact keeps the fight inside the two stationary cones, and the
	// charge's band starts five blocks out, so the only shape whose region a creature can
	// leave was never exercised. The coverage assertion at the foot is what stops the pair
	// collapsing back to that. The rate matters for the same reason: a lane is
	// `travelSpeed x release` while what is crossed is `travelSpeed x releaseTicks x dt`,
	// and those agree at twenty hertz but not at three.
	for _, rate := range []uint8{DefaultTickRate, 3} {
		for _, placement := range []struct {
			name   string
			at     [3]float64
			prefer vnet.EncounterMoveKind
		}{
			{"in contact", [3]float64{0.5, 64, -2.5}, vnet.EncounterMoveKindUnknown},
			{"at charging distance", [3]float64{0.5, 64, -7.6}, vnet.EncounterMoveKindCollarCharge},
		} {
			h := newVitalsHarness(t, rate, dropTerrain{groundTop: 63})
			player, out := h.join(1, [3]float32{0.5, 64, 0.5})
			boss := pullGuardian(t, h, placement.at, player)
			if placement.prefer != vnet.EncounterMoveKindUnknown {
				preferMove(h, boss, placement.prefer)
			}

			landed := 0
			exercised := map[vnet.EncounterMoveKind]bool{}
			for tick := 1; tick <= 40*int(rate); tick++ {
				before := h.vitals(player).Health
				h.step()
				if running := runningMoveOf(h, boss); running != nil {
					exercised[running.def.kind] = true
				}
				// Healed rather than allowed to die, so a long run keeps measuring the
				// invariant instead of ending at the first fight's outcome.
				if hurt := h.vitals(player).Health; hurt < before {
					landed++
					timeline := newestTimeline(t, out.all())
					box := player.box()
					inside := false
					for _, move := range timeline.moves {
						for _, hazard := range move.Hazards {
							if hazardReaches(hazard, box) {
								inside = true
							}
						}
					}
					if !inside {
						t.Fatalf("%d Hz %s: tick %d cost health with the player outside every announced region: %+v",
							rate, placement.name, tick, timeline.moves)
					}
					h.heal(player)
				}
			}
			if landed < 3 {
				t.Errorf("%d Hz %s: only %d blows landed, too few to have tested anything",
					rate, placement.name, landed)
			}
			// The coverage this test used to lack, asserted rather than assumed.
			if placement.prefer != vnet.EncounterMoveKindUnknown && !exercised[placement.prefer] {
				t.Errorf("%d Hz %s: %v never ran, so the travelling case went untested (saw %v)",
					rate, placement.name, placement.prefer, exercised)
			}
		}
	}
}

// Killing the boss withdraws what it had running, and says so.
//
// Cancelled rather than Completed, and the regions go with the ending: a party that kills a
// creature mid-telegraph must not be left with a shape anybody still treats as dangerous.
// The execution stops too — the half a client cannot observe, and the half that would
// otherwise keep resolving damage.
func TestKillingTheBossWithdrawsWhatItHadRunning(t *testing.T) {
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	boss := pullGuardian(t, h, [3]float64{0.5, 64, -2.5}, player)

	h.step()
	if running := runningMoveOf(h, boss); running == nil || running.phase != vnet.MovePhaseTelegraph {
		t.Fatalf("the boss was not mid-telegraph when it was killed: %+v", running)
	}

	h.sim.mu.Lock()
	m := h.sim.mobs[boss]
	h.sim.damageMobLocked(m, m.health)
	encounter := m.encounter
	h.sim.mu.Unlock()

	if encounter.running != nil {
		t.Fatal("a dead boss is still executing a move")
	}
	if len(encounter.moves) == 0 {
		t.Fatal("the announcement disappeared instead of ending")
	}
	for _, move := range encounter.moves {
		if move.Ended != vnet.MoveEndCancelled {
			t.Fatalf("a withdrawn move ended as %v, want Cancelled", move.Ended)
		}
		if len(move.Hazards) != 0 {
			t.Fatalf("a cancelled move still announces %d regions", len(move.Hazards))
		}
	}
}

// The stage gates the repertoire, and it gates it in one direction.
//
// The jaws are the second stage's move and nothing at the first stage may produce them;
// once the strap tears they are choosable. A stage never falls, so nothing here has to
// say what happens when it does.
func TestTheStageGatesTheRepertoire(t *testing.T) {
	jaws := vnet.EncounterMoveKindBonebreakerJaws

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	boss := pullGuardian(t, h, [3]float64{0.5, 64, -2.5}, player)
	preferMove(h, boss, jaws)

	for range 200 {
		h.step()
		if running := runningMoveOf(h, boss); running != nil && running.def.kind == jaws {
			t.Fatal("the first stage announced a move the second stage unlocks")
		}
		// Keep the player standing: what is under test is the selection, not survival.
		h.heal(player)
	}

	atStage(h, boss, 2)
	preferMove(h, boss, jaws)
	for range 200 {
		h.step()
		h.heal(player)
		if running := runningMoveOf(h, boss); running != nil && running.def.kind == jaws {
			return
		}
	}
	t.Fatal("the second stage never announced the move it unlocks")
}

// A move that would cover every escape is refused, and the creature does something else.
//
// **The rule is the scheduler's rather than any move's**, which is what this test is for:
// the guardian's own repertoire can never trip it, so the check is exercised with a
// synthetic row whose cone covers the whole arena. What must not happen is that it is
// announced anyway and a player is left with nowhere to stand.
func TestASelectionThatCoversEveryEscapeIsRefused(t *testing.T) {
	inescapable := encounterMoveDef{
		kind: vnet.EncounterMoveKindBiteAndTear, fromStage: 1,
		telegraph: 900 * time.Millisecond, release: 200 * time.Millisecond,
		recovery: time.Second, minRange: 0, maxRange: 40, damagePercent: 100,
		hazard: encounterHazard{shape: vnet.HazardShapeDisc, reach: 64, height: 64},
	}
	escapable := inescapable
	escapable.hazard = encounterHazard{shape: vnet.HazardShapeCone, reach: 3, height: 2.2, halfAngle: 0.7}

	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	boss := pullGuardian(t, h, [3]float64{0.5, 64, -2.5}, player)

	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	m := h.sim.mobs[boss]
	if h.sim.moveLeavesAnEscapeLocked(m, inescapable, player) {
		t.Fatal("a region covering the whole arena was judged to leave an escape")
	}
	if !h.sim.moveLeavesAnEscapeLocked(m, escapable, player) {
		t.Fatal("an ordinary cone was judged to leave nowhere to stand")
	}
}

// Every catalogued move is one a server can announce and one a player can read.
//
// A sweep over the whole table rather than a test per row, for species.go's reason: the
// next move added is a row rather than a test. Each clause fails in its own direction — a
// zero window is a danger nobody can be inside, and a band wider than the announced reach
// is a move aimed at somebody it never covers.
func TestEveryCataloguedMoveIsAnnouncableAndBounded(t *testing.T) {
	t.Parallel()

	for kind, repertoire := range encounterMoveCatalog {
		def, registered := mobByKind(kind)
		if !registered || !def.isBoss() {
			t.Fatalf("%s has a repertoire and is not a registered boss", kind)
		}
		if len(repertoire) == 0 {
			t.Fatalf("%s has an empty repertoire, which is not the same as having none", kind)
		}
		stages := uint8(len(def.phaseHealthPercents)) + 1

		for _, move := range repertoire {
			if move.kind == vnet.EncounterMoveKindUnknown {
				t.Errorf("%s announces an unnamed move", kind)
			}
			if move.fromStage < startEncounterPhase || move.fromStage > stages {
				t.Errorf("%s/%s unlocks at stage %d of %d", kind, move.kind, move.fromStage, stages)
			}
			if move.comboFromStage > stages || (move.comboFromStage != 0 && (move.combo == nil || move.comboFromStage < move.fromStage)) {
				t.Errorf("%s/%s has an invalid combo unlock stage %d", kind, move.kind, move.comboFromStage)
			}
			if move.telegraph <= 0 || move.release <= 0 || move.recovery <= 0 {
				t.Errorf("%s/%s has an empty phase: %+v", kind, move.kind, move)
			}
			if move.damagePercent == 0 {
				t.Errorf("%s/%s costs nothing", kind, move.kind)
			}
			if move.minRange < 0 || move.maxRange <= move.minRange {
				t.Errorf("%s/%s has the band [%v, %v]", kind, move.kind, move.minRange, move.maxRange)
			}
			// A move may only be chosen where its own announced region reaches, or it is
			// aimed at somebody it was never going to cover.
			if move.maxRange > move.selectionReach() {
				t.Errorf("%s/%s may be chosen at %v blocks and reaches %v",
					kind, move.kind, move.maxRange, move.selectionReach())
			}
			if move.announcedRadius() <= 0 {
				t.Errorf("%s/%s announces a region of no size", kind, move.kind)
			}
			if (move.travel == travelNone) != (move.travelSpeed == 0) {
				t.Errorf("%s/%s travels %v at %v blocks a second", kind, move.kind, move.travel, move.travelSpeed)
			}
			if move.impactRecovery != 0 && move.travel != travelCharge {
				t.Errorf("%s/%s pays an impact recovery and cannot be stopped", kind, move.kind)
			}
			if _, timed := encounterMoveTimingsFor(DefaultTickRate)[move.kind]; !timed {
				t.Errorf("%s/%s was never converted to ticks", kind, move.kind)
			}
		}
	}
}

// And every phase of every catalogued move encodes.
//
// The encoder refuses exactly what a decoder would end a session over, and
// [Sim.encounterFramesLocked] logs and drops a refusal — so a move the encoder will not
// take is a fight no client is told about, with nothing red to say so.
func TestEveryAnnouncementACataloguedMoveProducesEncodes(t *testing.T) {
	t.Parallel()

	for kind, repertoire := range encounterMoveCatalog {
		def := mobRegistry[kind]
		m := &mob{entityID: 9, kind: kind, pos: [3]float64{4.5, 64, 4.5}, health: def.maxHealth}
		target := &Player{entityID: 3, pos: [3]float64{4.5, 64, 10.5}}

		for _, move := range repertoire {
			aim := m.aimAt(target)
			running := &runningMove{
				def:        move,
				instanceID: 1,
				aim:        aim,
				anchor:     m.hazardAnchor(move, aim, target),
				phaseTicks: 4,
			}
			running.hazards = m.hazardsForPulse(move, running.aim, running.anchor, 0)

			for _, phase := range []vnet.MovePhase{
				vnet.MovePhaseTelegraph, vnet.MovePhaseRelease, vnet.MovePhaseRecovery,
			} {
				running.phase = phase
				announcement := running.announcement()
				if phase != vnet.MovePhaseRecovery && len(announcement.Hazards) == 0 {
					t.Errorf("%s/%s announces no region in %v", kind, move.kind, phase)
				}
				if phase == vnet.MovePhaseRecovery && len(announcement.Hazards) != 0 {
					t.Errorf("%s/%s still endangers a region while it is open", kind, move.kind)
				}
				if _, err := protocol.EncodeEncounterTimeline(protocol.EncounterTimeline{
					EncounterID:  7,
					BossEntityID: m.entityID,
					Boss:         kind,
					Phase:        startEncounterPhase,
					Moves:        []protocol.EncounterMove{announcement},
				}); err != nil {
					t.Errorf("%s/%s in %v: %v", kind, move.kind, phase, err)
				}
			}
		}
	}
}

// catalogued is one move's row, by kind, for the tests that name a number from it.
func catalogued(t *testing.T, kind vnet.EncounterMoveKind) encounterMoveDef {
	t.Helper()
	for _, def := range encounterMoveCatalog[vnet.MobKindVargrGuardian] {
		if def.kind == kind {
			return def
		}
	}
	t.Fatalf("the guardian has no %v", kind)
	return encounterMoveDef{}
}

// heal puts a player back to full so a selection test measures the selection.
func (h *vitalsHarness) heal(p *Player) {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	p.health = PlayerMaxHealth
}

// A leap's landing region is fixed before the jump and does not follow anybody.
//
// **The region is the answer to the move**, so a player who leaves it survives it even
// though the creature is still coming for them: what the leap damages is where it lands,
// which was announced, and never the arc it crossed to get there.
func TestALeapLandsWhereItWasAnnouncedRatherThanWhereItsTargetWent(t *testing.T) {
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	boss := pullGuardian(t, h, [3]float64{0.5, 64, -6.0}, player)
	preferMove(h, boss, vnet.EncounterMoveKindPredatorLeap)

	h.step()
	running := runningMoveOf(h, boss)
	if running == nil || running.def.kind != vnet.EncounterMoveKindPredatorLeap {
		t.Fatalf("the boss chose %+v rather than the leap", running)
	}
	announced := running.anchor

	// Out of the landing region while the telegraph plays out. Placed rather than walked,
	// because what is under test is the region and not the movement integrator.
	h.place(player, [3]float64{12.5, 64, 0.5})

	for range 40 {
		h.step()
		if r := runningMoveOf(h, boss); r == nil || r.def.kind != vnet.EncounterMoveKindPredatorLeap {
			break
		}
	}
	if got := h.vitals(player).Health; got != PlayerMaxHealth {
		t.Fatalf("a player who left the announced landing lost health, ending at %d", got)
	}

	h.sim.mu.Lock()
	landed := h.sim.mobs[boss].pos
	h.sim.mu.Unlock()
	if math.Hypot(landed[0]-announced[0], landed[2]-announced[2]) > 1 {
		t.Fatalf("the leap ended at %v rather than the region it announced at %v", landed, announced)
	}
}

// place puts a player somewhere, for the tests about regions rather than about movement.
func (h *vitalsHarness) place(p *Player, pos [3]float64) {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	p.pos = pos
	p.chunk = chunkAt(pos)
}

// A boss whose target is outside every move's range closes the distance.
//
// **The regression test for a scheduler that forgets to move**, asked for on review. The
// guardian's widest band is 8.5 blocks and its awareness reaches 24, so between the two
// no move is choosable and the creature has to walk. `stepEncounter` steers with the same
// [mob.steerToward]; what integrates that is [mob.physics], which runs at the foot of
// [mob.step] for every branch including this one.
//
// Being walled out is deliberately not tested here: the shared state machine allows it —
// see [mob.inReach] — and a boss inherits that rather than getting navigation of its own,
// which is #1024's Out of Scope.
func TestABossOutOfEveryMoveRangeClosesTheDistanceAndAttacks(t *testing.T) {
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})

	// Twenty blocks: inside the 24-block awareness that starts the fight, and well outside
	// the 8.5 of the widest move band, so nothing at all is choosable on the first tick.
	boss := pullGuardian(t, h, [3]float64{0.5, 64, -20.5}, player)

	h.sim.mu.Lock()
	m := h.sim.mobs[boss]
	widest := 0.0
	for _, def := range encounterMoveCatalog[vnet.MobKindVargrGuardian] {
		widest = max(widest, def.maxRange)
	}
	opening := boxDistance(m.species().body.boxAt(m.pos), player.box())
	h.sim.mu.Unlock()
	if opening <= widest {
		t.Fatalf("the target started %v blocks off, inside the widest band of %v", opening, widest)
	}

	h.step()
	if running := runningMoveOf(h, boss); running != nil {
		t.Fatalf("a move was announced at %v blocks, past every band: %v", opening, running.def.kind)
	}

	for range 300 {
		h.step()
		if h.vitals(player).Health < PlayerMaxHealth {
			return
		}
	}

	h.sim.mu.Lock()
	stalled := boxDistance(h.sim.mobs[boss].species().body.boxAt(h.sim.mobs[boss].pos), player.box())
	h.sim.mu.Unlock()
	t.Fatalf("the boss never reached its target: %v blocks off after three hundred ticks, from %v",
		stalled, opening)
}

// A charge never travels past the lane it announced, at any tick rate.
//
// **The rates are the point.** A lane is `travelSpeed x release` while what is crossed is
// `travelSpeed x releaseTicks x dt`, and [ticksFor] makes those agree only where it
// converts exactly. It truncates, so the ordinary answer is short — but it floors at one
// tick, and at a rate of 1 (which [NewSim] accepts) the guardian's 900 ms release becomes
// a whole second: eleven blocks against an announced 9.9. Every rate runs the same
// assertion, so this is a property of the move rather than a fact about twenty hertz.
func TestAChargeNeverTravelsPastTheLaneItAnnounced(t *testing.T) {
	for _, rate := range []uint8{1, 2, 3, 5, 7, 10, 13, DefaultTickRate} {
		h := newVitalsHarness(t, rate, dropTerrain{groundTop: 63})
		player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
		boss := pullGuardian(t, h, [3]float64{0.5, 64, -7.6}, player)
		preferMove(h, boss, vnet.EncounterMoveKindCollarCharge)

		charged := false
		for range 12 * int(rate) {
			h.step()
			running := runningMoveOf(h, boss)
			if running == nil || running.def.kind != vnet.EncounterMoveKindCollarCharge {
				continue
			}
			charged = true
			h.sim.mu.Lock()
			pos := h.sim.mobs[boss].pos
			h.sim.mu.Unlock()
			crossed := math.Hypot(pos[0]-running.anchor[0], pos[2]-running.anchor[2])
			announced := float64(running.hazards[0].Radius)
			if crossed > announced+1e-6 {
				t.Errorf("at %d Hz the charge crossed %.3f blocks of an announced %.3f",
					rate, crossed, announced)
				break
			}
		}
		if !charged {
			t.Errorf("at %d Hz the charge never ran, so nothing was tested", rate)
		}
	}
}

// And nobody standing past the end of an announced lane is hurt by it.
//
// The sharpest form of the invariant, at the rate that used to break it. Before the travel
// clamp a 1 Hz charge ran eleven blocks against an announced 9.9 and `sweptLaneReaches`
// tested that whole segment, so this player lost health for standing outside the region
// they were shown. The margin also covers the second half of the same defect: the segment
// test's clamped distance describes a capsule whose cap reaches `half_width` past the
// strip's square end, so the lane's own extent has to be tested too.
func TestNobodyPastTheEndOfAnAnnouncedLaneIsHurtByIt(t *testing.T) {
	h := newVitalsHarness(t, 1, dropTerrain{groundTop: 63})
	near, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	boss := pullGuardian(t, h, [3]float64{0.5, 64, -7.6}, near)
	preferMove(h, boss, vnet.EncounterMoveKindCollarCharge)

	h.step()
	running := runningMoveOf(h, boss)
	if running == nil || running.def.kind != vnet.EncounterMoveKindCollarCharge {
		t.Fatalf("the boss chose %+v rather than the charge", running)
	}
	lane := running.hazards[0]
	end := running.anchor[2] + float64(lane.Radius)

	// Half a block past the end of the announced strip, and dead on its centre line so
	// only the lane's length can exclude them.
	beyond, _ := h.join(2, [3]float32{float32(running.anchor[0]), 64, float32(end + 0.5)})
	if hazardReaches(lane, beyond.box()) {
		t.Fatal("the player placed past the lane is inside the announced region after all")
	}

	for range 12 {
		h.step()
		if r := runningMoveOf(h, boss); r == nil || r.instanceID != running.instanceID {
			break
		}
	}
	if got := h.vitals(near).Health; got == PlayerMaxHealth {
		t.Fatal("the charge never hit the player standing inside its lane")
	}
	if got := h.vitals(beyond).Health; got != PlayerMaxHealth {
		t.Fatalf("a player past the end of the announced lane lost health, ending at %d", got)
	}
}

// The tick that deals damage is published as the release that dealt it.
//
// **A phase one tick long is the ordinary case, not an edge one.** [ticksFor] floors at a
// single tick, so the guardian's 200 ms bite window is one tick below five hertz. Advanced
// at the foot of a tick, the machine executed that release and then published the recovery
// it had moved into, so the only frame a client received for the damaging tick said the
// creature was open and nothing was dangerous. Asserted off the wire, because the claim is
// about what a session was sent.
func TestTheDamagingTickIsPublishedAsItsRelease(t *testing.T) {
	for _, rate := range []uint8{1, 2, 3, 4, DefaultTickRate} {
		bite := encounterMoveCatalog[vnet.MobKindVargrGuardian][0]
		if rate < 5 && ticksFor(bite.release, rate) != 1 {
			t.Fatalf("%d Hz no longer gives the bite a one-tick release", rate)
		}

		h := newVitalsHarness(t, rate, dropTerrain{groundTop: 63})
		player, out := h.join(1, [3]float32{0.5, 64, 0.5})
		pullGuardian(t, h, [3]float64{0.5, 64, -2.5}, player)

		hurt := false
		for range 30 * int(rate) {
			before := h.vitals(player).Health
			h.step()
			if h.vitals(player).Health >= before {
				continue
			}
			hurt = true
			timeline := newestTimeline(t, out.all())
			if len(timeline.moves) != 1 {
				t.Fatalf("%d Hz: %d moves announced on the damaging tick", rate, len(timeline.moves))
			}
			move := timeline.moves[0]
			if move.Phase != vnet.MovePhaseRelease {
				t.Errorf("%d Hz: the damaging tick was published as %v", rate, move.Phase)
			}
			if len(move.Hazards) == 0 {
				t.Errorf("%d Hz: the damaging tick announced no region", rate)
			}
			break
		}
		if !hurt {
			t.Errorf("%d Hz: nobody was ever hurt, so nothing was tested", rate)
		}
	}
}
