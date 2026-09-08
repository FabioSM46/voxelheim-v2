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
// answers with the boss's entity id.
//
// The pull is taken by hand rather than by walking somebody into the arena for
// boss_species_test.go's reason: what these tests are about is what the creature does
// once the fight has started, and a fight that started because of an aggro radius would
// be measuring the aggro radius.
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

// preferMove puts every other move of the repertoire out of the least-recently-used
// running so the named one is the next thing chosen.
//
// It never edits the catalog: what it moves is the *encounter's* ledger, which is what
// the scheduler reads. A test that rewrote the repertoire would be testing a repertoire
// no server has.
func preferMove(h *vitalsHarness, id uint64, want vnet.EncounterMoveKind) {
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	e := h.sim.mobs[id].encounter
	for _, def := range encounterMoveCatalog[vnet.MobKindVargrGuardian] {
		if def.kind != want {
			e.lastUsed[def.kind] = math.MaxUint32
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

// monolith is flat ground with one solid slab standing across a band of z.
//
// The approved design's monolith: the thing a charge is meant to be steered into, and the
// only terrain feature these tests need.
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
// **This is the promise the whole encounter contract rests on**, and it fails in two
// directions worth separating: a move that damaged during its telegraph would put an
// attack in front of a player who had been given nothing to react to, and one that
// damaged outside its release would make the announced window a decoration.
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

	want := ticksFor(encounterMoveCatalog[vnet.MobKindVargrGuardian][0].telegraph, DefaultTickRate)
	if uint32(telegraphTicks) != want {
		t.Fatalf("the telegraph ran %d ticks before the first blow, want %d", telegraphTicks, want)
	}

	// And the region was announced through the whole of it, on the wire, rather than
	// appearing with the damage.
	timeline := newestTimeline(t, out.all())
	if len(timeline.moves) != 1 {
		t.Fatalf("the encounter announced %d moves at the blow", len(timeline.moves))
	}
	if len(timeline.moves[0].Hazards) == 0 {
		t.Fatal("the dangerous phase announced no region at all")
	}
}

// One release window lands on each target exactly once, and a second instance may land
// again.
//
// The window is what the ledger is keyed to rather than the instance, which is the pair
// of assertions here: a release lasting several ticks is one blow, and the next move is a
// new one.
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
// The cone is fixed when the move is chosen and never re-aimed, so a player who steps out
// of it takes nothing while one who stays in it is hit. A move that tracked its target
// would make this test impossible to write, which is exactly why it is written.
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
// **Run at five hertz, where the creature covers 2.2 blocks a tick and its body is 1.6
// wide.** The test asserts the thing that makes the sweep necessary rather than merely
// asserting a hit: the creature's own box never overlaps the player's on any tick, so an
// endpoint test would report no contact at all, and the player is hit anyway.
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
// The wall is the continuous half of the collision requirement: the displacement goes
// through the same [moveAndCollide] a player's does, so no speed carries the creature
// past it. The recovery is the design's monolith reward, and it is the server's number
// rather than an animation's.
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
// **This is the acceptance criterion executed rather than restated.** Every tick that
// costs a player health is checked against the announcement that was live at that moment,
// read back off the wire: the player's own box has to be inside one of the regions the
// client had already been shown. The three volumes stay apart by construction — the body
// that collides, the body a blade reaches and the region an attack endangers are three
// different numbers — and this is what says so at run time.
func TestNoBlowLandsOutsideTheRegionThatWasAnnounced(t *testing.T) {
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	pullGuardian(t, h, [3]float64{0.5, 64, -2.5}, player)

	landed := 0
	for tick := 1; tick <= 400; tick++ {
		before := h.vitals(player).Health
		if before == 0 {
			break
		}
		h.step()
		if h.vitals(player).Health >= before {
			continue
		}
		landed++
		timeline := newestTimeline(t, out.all())
		hurt := player.box()
		inside := false
		for _, move := range timeline.moves {
			for _, hazard := range move.Hazards {
				if hazardReaches(hazard, hurt) {
					inside = true
				}
			}
		}
		if !inside {
			t.Fatalf("tick %d cost health with the player outside every announced region: %+v",
				tick, timeline.moves)
		}
	}
	if landed < 3 {
		t.Fatalf("only %d blows landed in four hundred ticks, which is too few to have tested anything", landed)
	}
}

// Killing the boss withdraws what it had running, and says so.
//
// Cancelled rather than Completed, and the regions go with the ending: a party that kills
// a creature mid-telegraph must not be left with a shape anybody still treats as
// dangerous. The execution stops too, which is the half a client cannot observe and the
// half that would otherwise keep resolving damage.
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
// A sweep over the whole table rather than a test per row, for species.go's reason: what
// is being held is a property of the repertoire, and the next move added is a row rather
// than a test. Each clause fails in its own direction — a zero window is a danger nobody
// can be inside, a band wider than the announced reach is a move aimed at somebody it
// never covers, and an announcement the encoder refuses is a frame that would be logged
// and dropped with the fight silently missing from every client.
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
// [Sim.encounterFramesLocked] logs and drops a refusal — so a move whose announcement the
// encoder will not take is a fight no client is ever told about, with nothing red anywhere
// to say so. This is what stops that being discovered in a running world.
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
			running.hazards = m.hazardsFor(move, running.aim, running.anchor)

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
