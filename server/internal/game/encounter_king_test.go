package game

import (
	"fmt"
	"math"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// pullKing places the Draugr king and freezes the encounter, as pullGuardian does.
func pullKing(t *testing.T, h *vitalsHarness, at [3]float64, first *Player) uint64 {
	t.Helper()
	id := h.placeSpeciesAt(vnet.MobKindDraugrKing, at)
	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	h.sim.startBossEncounterLocked(h.sim.mobs[id], first)
	return id
}

// kingMove is one catalogued row of the king's, by kind.
func kingMove(t *testing.T, kind vnet.EncounterMoveKind) encounterMoveDef {
	t.Helper()
	for _, def := range encounterMoveCatalog[vnet.MobKindDraugrKing] {
		if def.kind == kind {
			return def
		}
	}
	t.Fatalf("the king has no %v", kind)
	return encounterMoveDef{}
}

// wallAt is flat ground with a solid slab across a band of z, tall enough to stop a spear.
type wallAt struct {
	groundTop  int64
	fromZ, toZ int64
}

func (w wallAt) Block(_, y, z int64) (world.Block, bool) {
	if y <= w.groundTop || (z >= w.fromZ && z <= w.toZ && y <= w.groundTop+5) {
		return world.Stone, true
	}
	return world.Air, true
}

func (w wallAt) Fluid(x, y, z int64) bool { return fluidByBlock(w, x, y, z) }

func (w wallAt) Solid(x, y, z int64) bool {
	block, resident := w.Block(x, y, z)
	return !resident || block != world.Air
}

// The king is inert until the guardian falls, and nothing in this half changes that.
//
// **The answer to whether #1024 needs the gate moved: it does not.** `dungeonBossLocked`
// is scoped to `!progress.guardian`, so it stops being true the moment the guardian dies —
// it is the approved design's sealed second arena ("boss 2 inaccessible until the Vargr's
// death"), not a switch saying the king is unfinished. Once the gate opens the king steps
// like any other creature and the repertoire below runs. Moving it would be changing an
// arena rule to work around an execution gap that does not exist.
func TestTheKingIsInertUntilTheGuardianFallsAndFightsAfterwards(t *testing.T) {
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	king := pullKing(t, h, [3]float64{0.5, 64, -3.0}, player)

	// Standing in for the placed dungeon: the same gate, over this king.
	h.sim.mu.Lock()
	h.sim.dungeon = &dungeonEncounters{kingID: king, pending: make(map[*Player]int)}
	h.sim.mu.Unlock()

	for range 120 {
		h.step()
		if runningMoveOf(h, king) != nil {
			t.Fatal("the sealed king announced a move before the guardian fell")
		}
	}
	if got := h.vitals(player).Health; got != PlayerMaxHealth {
		t.Fatalf("the sealed king dealt damage, leaving the player at %d", got)
	}

	// The guardian falls. Nothing else changes.
	h.sim.mu.Lock()
	h.sim.dungeon.progress.guardian = true
	h.sim.mu.Unlock()

	for range 200 {
		h.step()
		if runningMoveOf(h, king) != nil {
			return
		}
	}
	t.Fatal("the king never acted after the gate opened")
}

// A ritual runs pulse by pulse, and each pulse is shown before it is dangerous.
//
// The whole of what `MovePhase.Channel` promises: `pulse_index` counts the imminent pulse,
// `pulse_total` never moves, each pulse's own region is what `hazards` carries, and the
// damage lands on the last tick of the interval rather than the first — the telegraph's
// promise one layer down.
func TestARitualRunsPulseByPulseAndShowsEachBeforeItFires(t *testing.T) {
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	king := pullKing(t, h, [3]float64{0.5, 64, -3.0}, player)
	atStage(h, king, 2)
	preferMove(h, king, vnet.EncounterMoveKindBurial)

	burial := kingMove(t, vnet.EncounterMoveKindBurial)
	pulseTicks := ticksFor(burial.channelPulse, DefaultTickRate)

	seen := map[uint8]uint32{}
	var fired []uint8
	for range 400 {
		before := h.vitals(player).Health
		h.step()
		hurt := h.vitals(player).Health < before
		h.heal(player)
		running := runningMoveOf(h, king)
		if running == nil || running.def.kind != vnet.EncounterMoveKindBurial {
			continue
		}
		if running.phase != vnet.MovePhaseChannel {
			continue
		}
		seen[running.pulseIndex]++
		if hurt {
			fired = append(fired, running.pulseIndex)
			// The interval is spent: damage lands on its last tick, never earlier.
			if running.remaining != 0 {
				t.Fatalf("pulse %d fired with %d ticks of its interval left",
					running.pulseIndex, running.remaining)
			}
		}
		if running.pulseIndex+1 == burial.pulses && running.remaining == 0 {
			break
		}
	}

	if len(seen) != int(burial.pulses) {
		t.Fatalf("the ritual ran %d pulses, want %d", len(seen), burial.pulses)
	}
	for pulse, ticks := range seen {
		if ticks != pulseTicks {
			t.Errorf("pulse %d was shown for %d ticks, want %d", pulse, ticks, pulseTicks)
		}
	}
	if len(fired) == 0 {
		t.Fatal("no pulse ever caught anybody, so nothing about the firing was tested")
	}

	// And what the client held, off the wire.
	announced := announcedTimelines(t, out.all())
	channels := 0
	for _, timeline := range announced {
		for _, move := range timeline.moves {
			if move.Kind != vnet.EncounterMoveKindBurial || move.Phase != vnet.MovePhaseChannel {
				continue
			}
			channels++
			if move.PulseTotal != burial.pulses {
				t.Fatalf("a pulse announced %d of %d", move.PulseIndex, move.PulseTotal)
			}
			if move.PulseIndex >= move.PulseTotal {
				t.Fatalf("pulse index %d of %d", move.PulseIndex, move.PulseTotal)
			}
			if len(move.Hazards) == 0 {
				t.Fatal("a pulse announced no region")
			}
			if move.Interruptible {
				t.Fatal("the burial claimed to be interruptible")
			}
		}
	}
	if channels == 0 {
		t.Fatal("no channel frame reached the client")
	}
}

// The burial's bands walk outward, and each pulse announces only its own.
//
// A ring is an annulus and the damage test is that annulus, so the ground inside the wave
// and the ground beyond it are both genuinely safe — which is the answer the design asks a
// player to read ("follow the safe band while the wave advances").
func TestTheBurialAnnouncesOneExpandingBandPerPulse(t *testing.T) {
	t.Parallel()

	def := kingMove(t, vnet.EncounterMoveKindBurial)
	m := &mob{entityID: 4, kind: vnet.MobKindDraugrKing, pos: [3]float64{0.5, 64, 0.5}}
	aim := [3]float64{0, 0, 1}
	anchor := m.hazardAnchor(def, aim, &Player{pos: [3]float64{0.5, 64, 4.5}})

	var previousOuter float32
	for pulse := range def.pulses {
		regions := m.hazardsForPulse(def, aim, anchor, pulse)
		if len(regions) != 1 {
			t.Fatalf("pulse %d announced %d regions, want one band", pulse, len(regions))
		}
		band := regions[0]
		if band.Shape != vnet.HazardShapeRing {
			t.Fatalf("pulse %d announced a %v", pulse, band.Shape)
		}
		if band.InnerRadius != previousOuter {
			t.Errorf("pulse %d starts at %v, want the previous band's edge %v",
				pulse, band.InnerRadius, previousOuter)
		}
		if band.Radius <= band.InnerRadius {
			t.Errorf("pulse %d is a band of no width", pulse)
		}
		// A player standing inside the ring it has already passed is safe.
		if pulse > 0 {
			inside := playerBox([3]float64{anchor[0], 64, anchor[2]})
			if hazardReaches(band, inside) {
				t.Errorf("pulse %d still endangers the ground at the centre", pulse)
			}
		}
		previousOuter = band.Radius
	}
}

// The spear crosses the lane it announced and never a block past it, at any tick rate.
//
// Part 1's charge lesson, for a point that is not the creature: the flight is clamped
// against the announcement rather than against a tick count, because `ticksFor` truncates
// and floors at one tick, so a rate of 1 gives the 800 ms release a whole second.
func TestTheSpearNeverFliesPastTheLaneItAnnounced(t *testing.T) {
	for _, rate := range []uint8{1, 2, 3, 5, 9, DefaultTickRate} {
		h := newVitalsHarness(t, rate, dropTerrain{groundTop: 63})
		player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
		king := pullKing(t, h, [3]float64{0.5, 64, -10.0}, player)
		preferMove(h, king, vnet.EncounterMoveKindSepulchreSpear)

		thrown := false
		for range 20 * int(rate) {
			h.step()
			h.heal(player)
			running := runningMoveOf(h, king)
			if running == nil || running.def.kind != vnet.EncounterMoveKindSepulchreSpear ||
				running.phase != vnet.MovePhaseRelease {
				continue
			}
			thrown = true
			crossed := math.Hypot(running.flight[0]-running.anchor[0], running.flight[2]-running.anchor[2])
			announced := float64(running.hazards[0].Radius)
			if crossed > announced+1e-6 {
				t.Errorf("at %d Hz the spear crossed %.3f of an announced %.3f", rate, crossed, announced)
				break
			}
		}
		if !thrown {
			t.Errorf("at %d Hz the spear was never thrown, so nothing was tested", rate)
		}
	}
}

// A thrown spear does not steer, and a wall stops it.
//
// Two halves of the same claim, because the design states both: the direction is fixed
// before release, and nothing about the flight is a client's to decide.
func TestTheSpearNeitherHomesNorPassesThroughAWall(t *testing.T) {
	t.Run("it does not follow the player who left its lane", func(t *testing.T) {
		h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
		player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
		king := pullKing(t, h, [3]float64{0.5, 64, -10.0}, player)
		preferMove(h, king, vnet.EncounterMoveKindSepulchreSpear)

		// Out of the lane once the throw is committed, and well clear of its half-width.
		for range 200 {
			h.step()
			if running := runningMoveOf(h, king); running != nil &&
				running.phase == vnet.MovePhaseRelease {
				h.place(player, [3]float64{12.5, 64, 0.5})
				break
			}
		}
		for range 40 {
			h.step()
		}
		if got := h.vitals(player).Health; got != PlayerMaxHealth {
			t.Fatalf("the spear followed the player out of its lane, leaving them at %d", got)
		}
	})

	t.Run("a wall stops it and shields whoever is behind", func(t *testing.T) {
		h := newVitalsHarness(t, DefaultTickRate, wallAt{groundTop: 63, fromZ: -4, toZ: -3})
		near, _ := h.join(1, [3]float32{0.5, 64, -6.5})
		behind, _ := h.join(2, [3]float32{0.5, 64, 0.5})
		king := pullKing(t, h, [3]float64{0.5, 64, -12.0}, near)
		preferMove(h, king, vnet.EncounterMoveKindSepulchreSpear)

		for range 200 {
			h.step()
			running := runningMoveOf(h, king)
			if running == nil || running.phase != vnet.MovePhaseRelease {
				continue
			}
			if running.flight[2] > -4 {
				t.Fatalf("the spear reached %.2f, past the near face of the wall", running.flight[2])
			}
		}
		if got := h.vitals(behind).Health; got != PlayerMaxHealth {
			t.Fatalf("a player behind the wall was speared, ending at %d", got)
		}
	})
}

// An interrupted channel ends as Interrupted, on a frame allowed to say so, and buys the
// opening the design promises.
//
// **Damage is the interrupt and it is not a new ability** — `castInterruptedByDamage` is
// already how this game stops a player's cast. The threshold is the adaptation, argued at
// `encounterMoveDef.interruptDamage`.
//
// The ending has to arrive on a `Channel` frame: that is the only phase the contract lets
// carry `interruptible`, and an `Interrupted` ending on a move that does not claim to be
// interruptible is a frame the encoder refuses outright.
func TestAnInterruptedChannelEndsAsInterruptedAndBuysTheOpening(t *testing.T) {
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	king := pullKing(t, h, [3]float64{0.5, 64, -3.0}, player)
	atStage(h, king, 3)
	preferMove(h, king, vnet.EncounterMoveKindRequiemOfTheBuried)

	requiem := kingMove(t, vnet.EncounterMoveKindRequiemOfTheBuried)
	dealt := uint16(0)
	for range 200 {
		h.step()
		h.heal(player)
		running := runningMoveOf(h, king)
		if running == nil || running.phase != vnet.MovePhaseChannel {
			continue
		}
		if dealt < requiem.interruptDamage {
			h.sim.mu.Lock()
			h.sim.damageMobLocked(h.sim.mobs[king], 60)
			h.sim.mu.Unlock()
			dealt += 60
		}
	}

	// Read off the wire: the ending, its phase, and the flag that makes it legal.
	var ending *protocol.EncounterMove
	for _, timeline := range announcedTimelines(t, out.all()) {
		for i, move := range timeline.moves {
			if move.Ended == vnet.MoveEndInterrupted {
				ending = &timeline.moves[i]
			}
		}
	}
	if ending == nil {
		t.Fatal("damage past the threshold never interrupted the requiem")
	}
	if ending.Kind != vnet.EncounterMoveKindRequiemOfTheBuried {
		t.Fatalf("%v was interrupted", ending.Kind)
	}
	if ending.Phase != vnet.MovePhaseChannel {
		t.Fatalf("the interruption was published in %v, which may not carry the flag", ending.Phase)
	}
	if !ending.Interruptible {
		t.Fatal("the interrupted instance did not announce itself interruptible")
	}
	if len(ending.Hazards) != 0 {
		t.Fatal("an ended instance still announces a region")
	}
}

// The opening is real: nothing is announced while it is being spent.
func TestASuccessfulInterruptLeavesTheKingOpen(t *testing.T) {
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	king := pullKing(t, h, [3]float64{0.5, 64, -3.0}, player)
	atStage(h, king, 3)
	preferMove(h, king, vnet.EncounterMoveKindRequiemOfTheBuried)

	requiem := kingMove(t, vnet.EncounterMoveKindRequiemOfTheBuried)
	// One burst, exactly at the threshold, so the test measures the interrupt rather than
	// how long a king survives being hit every tick.
	broken := false
	opening, spent := uint32(0), uint32(0)
	for range 400 {
		h.step()
		h.heal(player)
		running := runningMoveOf(h, king)
		if !broken && running != nil && running.phase == vnet.MovePhaseChannel {
			h.sim.mu.Lock()
			h.sim.damageMobLocked(h.sim.mobs[king], requiem.interruptDamage)
			h.sim.mu.Unlock()
			broken = true
			continue
		}

		h.sim.mu.Lock()
		stagger := h.sim.mobs[king].encounter.staggerTicks
		h.sim.mu.Unlock()
		if stagger == 0 {
			if spent > 0 {
				break
			}
			continue
		}
		opening = max(opening, stagger)
		spent++
		if runningMoveOf(h, king) != nil {
			t.Fatal("the king announced a move while it was supposed to be open")
		}
	}

	if !broken {
		t.Fatal("the requiem never reached a channel to break")
	}
	if spent == 0 {
		t.Fatal("a successful interrupt bought no opening at all")
	}
	if want := ticksFor(requiem.interruptRecovery, DefaultTickRate); opening > want {
		t.Fatalf("the opening was %d ticks, want at most the declared %d", opening, want)
	}
}

// A channel nobody may interrupt survives the same damage and completes.
//
// The negative half of the pair: `interruptible` is per instance and the burial does not
// claim it, so damage that would break a requiem does nothing to it.
func TestAnUninterruptibleRitualSurvivesTheSameDamage(t *testing.T) {
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	king := pullKing(t, h, [3]float64{0.5, 64, -3.0}, player)
	atStage(h, king, 2)
	preferMove(h, king, vnet.EncounterMoveKindBurial)

	pulses := map[uint8]bool{}
	for range 400 {
		h.step()
		h.heal(player)
		if running := runningMoveOf(h, king); running != nil &&
			running.def.kind == vnet.EncounterMoveKindBurial {
			if running.phase == vnet.MovePhaseChannel && !pulses[running.pulseIndex] {
				// Once per pulse, so the ritual is tested against damage rather than the
				// king against being killed by the test's own arithmetic.
				pulses[running.pulseIndex] = true
				h.sim.mu.Lock()
				h.sim.damageMobLocked(h.sim.mobs[king], 60)
				h.sim.mu.Unlock()
			}
		}
	}

	burial := kingMove(t, vnet.EncounterMoveKindBurial)
	if len(pulses) != int(burial.pulses) {
		t.Fatalf("the burial ran %d of its %d pulses under damage", len(pulses), burial.pulses)
	}
	for _, timeline := range announcedTimelines(t, out.all()) {
		for _, move := range timeline.moves {
			if move.Kind == vnet.EncounterMoveKindBurial && move.Ended == vnet.MoveEndInterrupted {
				t.Fatal("a ritual that never claimed to be interruptible was interrupted")
			}
		}
	}
}

// Every pulse of every ritual leaves somewhere reachable to stand.
//
// **The overlapping case, which is what makes this rule more than a formality.** A ritual
// is a schedule: checking only the region it announces first would pass a third pulse that
// covers exactly the ground the second one drove everybody onto. The synthetic row is the
// proof that the rule can still say no.
func TestEveryPulseOfARitualLeavesAReachableEscape(t *testing.T) {
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
	king := pullKing(t, h, [3]float64{0.5, 64, -3.0}, player)

	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	m := h.sim.mobs[king]

	ritualsChecked := 0
	for _, def := range encounterMoveCatalog[vnet.MobKindDraugrKing] {
		if def.pulses == 0 {
			continue
		}
		ritualsChecked++
		if !h.sim.moveLeavesAnEscapeLocked(m, def, player) {
			t.Errorf("%v leaves no reachable safe space at some pulse", def.kind)
		}
	}
	if ritualsChecked == 0 {
		t.Fatal("no channelled move was checked, so nothing was tested")
	}

	// A schedule whose last pulse covers the arena. The first pulses are ordinary, so
	// anything that looked only at what is announced first would accept it.
	inescapable := kingMove(t, vnet.EncounterMoveKindEdictOfTheGraves)
	inescapable.hazard.sectors = 1
	inescapable.hazard.sectorRing = 0
	inescapable.hazard.reach = 64
	if h.sim.moveLeavesAnEscapeLocked(m, inescapable, player) {
		t.Fatal("a ritual whose pulses cover the whole arena was judged escapable")
	}
}

// Every announcement a channelled move produces is one the encoder accepts.
//
// Part 1's sweep covers telegraph, release and recovery; a channel's frames are the ones it
// could not reach, and they carry the fields the encoder is strictest about — pulse counts
// that must be present in `Channel` and absent everywhere else, and an `interruptible` flag
// that only one phase may set.
func TestEveryChannelledAnnouncementEncodes(t *testing.T) {
	t.Parallel()

	checked := 0
	for kind, repertoire := range encounterMoveCatalog {
		def := mobRegistry[kind]
		m := &mob{entityID: 11, kind: kind, pos: [3]float64{4.5, 64, 4.5}, health: def.maxHealth}
		target := &Player{entityID: 3, pos: [3]float64{4.5, 64, 12.5}}

		for _, move := range repertoire {
			if move.pulses == 0 {
				continue
			}
			checked++
			aim := m.aimAt(target)
			anchor := m.hazardAnchor(move, aim, target)
			for pulse := range move.pulses {
				running := &runningMove{
					def: move, instanceID: 5, aim: aim, anchor: anchor,
					phase: vnet.MovePhaseChannel, phaseTicks: 6, pulseIndex: pulse,
					hazards: m.hazardsForPulse(move, aim, anchor, pulse),
				}
				announcement := running.announcement()
				if len(announcement.Hazards) == 0 {
					t.Errorf("%v pulse %d announces no region", move.kind, pulse)
				}
				if announcement.PulseTotal != move.pulses || announcement.PulseIndex != pulse {
					t.Errorf("%v pulse %d announced %d of %d",
						move.kind, pulse, announcement.PulseIndex, announcement.PulseTotal)
				}
				// The ending an interrupt writes has to encode on this very frame.
				if move.interruptible {
					announcement.Ended = vnet.MoveEndInterrupted
				}
				if _, err := protocol.EncodeEncounterTimeline(protocol.EncounterTimeline{
					EncounterID: 9, BossEntityID: m.entityID, Boss: kind,
					Phase: startEncounterPhase, Moves: []protocol.EncounterMove{announcement},
				}); err != nil {
					t.Errorf("%v pulse %d: %v", move.kind, pulse, err)
				}
			}
		}
	}
	if checked == 0 {
		t.Fatal("no channelled move was swept, so nothing was tested")
	}
}

// No blow of the king's lands outside the region it announced, across rates and moves.
//
// Part 1's invariant with part 1's lesson built in: it names the moves it means to
// exercise and **fails if any of them never ran**, so it cannot quietly shrink to whatever
// happens to be in range. Three hertz is a rate where `ticksFor` converts none of these
// durations exactly.
func TestNoKingBlowLandsOutsideTheRegionThatWasAnnounced(t *testing.T) {
	for _, rate := range []uint8{DefaultTickRate, 3} {
		for _, placement := range []struct {
			name  string
			at    [3]float64
			stage uint8
			want  []vnet.EncounterMoveKind
		}{
			{"in contact", [3]float64{0.5, 64, -3.0}, 3, []vnet.EncounterMoveKind{
				vnet.EncounterMoveKindKingsSentence, vnet.EncounterMoveKindThreeTolls,
				vnet.EncounterMoveKindBurial, vnet.EncounterMoveKindEdictOfTheGraves,
				vnet.EncounterMoveKindRequiemOfTheBuried,
			}},
			{"at throwing distance", [3]float64{0.5, 64, -10.0}, 1, []vnet.EncounterMoveKind{
				vnet.EncounterMoveKindSepulchreSpear,
			}},
		} {
			h := newVitalsHarness(t, rate, dropTerrain{groundTop: 63})
			player, out := h.join(1, [3]float32{0.5, 64, 0.5})
			king := pullKing(t, h, placement.at, player)
			atStage(h, king, placement.stage)

			landed := 0
			exercised := map[vnet.EncounterMoveKind]bool{}
			for tick := 1; tick <= 120*int(rate); tick++ {
				before := h.vitals(player).Health
				h.step()
				if running := runningMoveOf(h, king); running != nil {
					exercised[running.def.kind] = true
				}
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
						t.Fatalf("%d Hz %s: tick %d cost health outside every announced region: %+v",
							rate, placement.name, tick, timeline.moves)
					}
					h.heal(player)
				}
			}
			if landed < 3 {
				t.Errorf("%d Hz %s: only %d blows landed", rate, placement.name, landed)
			}
			for _, want := range placement.want {
				if !exercised[want] {
					t.Errorf("%d Hz %s: %v never ran, so it went untested (saw %v)",
						rate, placement.name, want, exercised)
				}
			}
		}
	}
}

// The tick a pulse fires on is published as the channel that fired it.
//
// Part 1's `TestTheDamagingTickIsPublishedAsItsRelease`, for pulses. `ticksFor` floors at
// one tick, so below about two hertz a whole pulse interval is a single tick — the case
// where a machine that advanced its phase at the foot of a tick would publish the *next*
// pulse, or the recovery, on the frame that did the damage.
func TestTheFiringTickOfAPulseIsPublishedAsItsChannel(t *testing.T) {
	for _, rate := range []uint8{1, 2, 3, DefaultTickRate} {
		h := newVitalsHarness(t, rate, dropTerrain{groundTop: 63})
		player, out := h.join(1, [3]float32{0.5, 64, 0.5})
		king := pullKing(t, h, [3]float64{0.5, 64, -3.0}, player)
		atStage(h, king, 2)
		preferMove(h, king, vnet.EncounterMoveKindBurial)

		fired := false
		for range 200 * int(rate) {
			before := h.vitals(player).Health
			running := runningMoveOf(h, king)
			channelling := running != nil && running.phase == vnet.MovePhaseChannel
			h.step()
			hurt := h.vitals(player).Health < before
			h.heal(player)
			if !channelling || !hurt {
				continue
			}
			fired = true
			timeline := newestTimeline(t, out.all())
			if len(timeline.moves) != 1 {
				t.Fatalf("%d Hz: %d moves on the firing tick", rate, len(timeline.moves))
			}
			move := timeline.moves[0]
			if move.Phase != vnet.MovePhaseChannel {
				t.Errorf("%d Hz: a pulse fired on a tick published as %v", rate, move.Phase)
			}
			if len(move.Hazards) == 0 {
				t.Errorf("%d Hz: the firing tick announced no region", rate)
			}
			break
		}
		if !fired {
			t.Errorf("%d Hz: no pulse ever fired, so nothing was tested", rate)
		}
	}
}

// Killing the king mid-ritual withdraws the channel, and the withdrawal still encodes.
//
// **The encoder is strict about a channel and silent when it refuses** — an invalid
// timeline is logged and dropped, so a cancellation that produced one would leave every
// client drawing a ritual the server had already stopped, with nothing red to say so. A
// cancelled channel keeps its pulse counts and its interruptible flag precisely so that
// the frame carrying the ending is still a legal `Channel` frame.
func TestWithdrawingARitualStillProducesAFrameAClientMayRead(t *testing.T) {
	for _, ritual := range []vnet.EncounterMoveKind{
		vnet.EncounterMoveKindBurial, vnet.EncounterMoveKindRequiemOfTheBuried,
	} {
		h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
		player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
		king := pullKing(t, h, [3]float64{0.5, 64, -3.0}, player)
		atStage(h, king, 3)
		preferMove(h, king, ritual)

		channelling := false
		for range 400 {
			h.step()
			h.heal(player)
			if running := runningMoveOf(h, king); running != nil &&
				running.def.kind == ritual && running.phase == vnet.MovePhaseChannel {
				channelling = true
				break
			}
		}
		if !channelling {
			t.Fatalf("%v never reached a channel", ritual)
		}

		h.sim.mu.Lock()
		m := h.sim.mobs[king]
		h.sim.damageMobLocked(m, m.health)
		encounter := m.encounter
		frame, err := protocol.EncodeEncounterTimeline(protocol.EncounterTimeline{
			EncounterID:  encounter.id,
			BossEntityID: king,
			Boss:         vnet.MobKindDraugrKing,
			Phase:        encounter.phase,
			Moves:        encounter.moves,
		})
		h.sim.mu.Unlock()

		if encounter.running != nil {
			t.Errorf("%v: a dead king is still channelling", ritual)
		}
		if err != nil {
			t.Errorf("%v: the withdrawal built a frame no client may read: %v", ritual, err)
		}
		if len(frame) == 0 {
			t.Errorf("%v: the withdrawal produced no frame", ritual)
		}
		for _, move := range encounter.moves {
			if move.Ended != vnet.MoveEndCancelled {
				t.Errorf("%v: a withdrawn ritual ended as %v", ritual, move.Ended)
			}
			if len(move.Hazards) != 0 {
				t.Errorf("%v: a cancelled ritual still announces a region", ritual)
			}
		}
	}
}

// thinWall is flat ground with a solid column exactly one block thick at wallZ.
//
// One block is the thickness that matters: it is the thinnest thing a step can straddle,
// and the spear's step is longer than it at every tick rate.
type thinWall struct {
	groundTop int64
	wallZ     int64
}

func (w thinWall) Block(_, y, z int64) (world.Block, bool) {
	if y <= w.groundTop || (z == w.wallZ && y <= w.groundTop+6) {
		return world.Stone, true
	}
	return world.Air, true
}

func (w thinWall) Fluid(x, y, z int64) bool { return fluidByBlock(w, x, y, z) }

func (w thinWall) Solid(x, y, z int64) bool {
	block, resident := w.Block(x, y, z)
	return !resident || block != world.Air
}

// A spear cannot tunnel a one-block wall, at any tick rate.
//
// **The step is longer than the wall is thick, everywhere.** At 22 blocks a second the
// spear covers 1.1 blocks per tick at the default rate, and at a rate of 1 it is clamped
// only by its own 17.6-block lane — so a terrain test that samples where the step *ends*
// can find air on both sides of a wall the flight went straight through.
//
// **Measured before the sweep was written**: at 1 Hz the flight reached the far end of its
// lane through the wall and the player standing behind it lost 26 health. Two details of
// that measurement are the reason this test is shaped as it is. The tunnel is
// alignment-dependent — 1 Hz and 3 Hz tunnelled while 2, 5 and 20 happened not to — so a
// single rate proves nothing either way. And the damage was invisible in the final health:
// at 1 Hz a tick is a whole second, so regeneration had restored the player by the end of
// the run. The minimum is what is asserted.
func TestASpearCannotTunnelAOneBlockWall(t *testing.T) {
	const wallZ = -6

	for _, rate := range []uint8{1, 2, 3, 5, 9, DefaultTickRate} {
		h := newVitalsHarness(t, rate, thinWall{groundTop: 63, wallZ: wallZ})
		// In front of the wall, inside the spear's band, and the nearer target.
		near, _ := h.join(1, [3]float32{0.5, 64, -8.0})
		// Behind it, on the lane, and well inside the 17.6 blocks the lane covers.
		behind, _ := h.join(2, [3]float32{0.5, 64, 0.5})
		king := pullKing(t, h, [3]float64{0.5, 64, -14.0}, near)
		preferMove(h, king, vnet.EncounterMoveKindSepulchreSpear)

		thrown := false
		lowest := uint16(PlayerMaxHealth)
		for range 40 * int(rate) {
			h.step()
			h.heal(near)
			lowest = min(lowest, h.vitals(behind).Health)
			running := runningMoveOf(h, king)
			if running == nil || running.def.kind != vnet.EncounterMoveKindSepulchreSpear ||
				running.phase != vnet.MovePhaseRelease {
				continue
			}
			thrown = true
			// The near face of the wall voxel, which the flight must never reach.
			if running.flight[2] >= float64(wallZ) {
				t.Errorf("at %d Hz the spear reached %.3f, at or past the wall at %d",
					rate, running.flight[2], wallZ)
				break
			}
		}
		if !thrown {
			t.Errorf("at %d Hz the spear was never thrown, so nothing was tested", rate)
		}
		if lowest != PlayerMaxHealth {
			t.Errorf("at %d Hz a player behind a wall was speared through it, down to %d",
				rate, lowest)
		}
	}
}

// sealedRing is clear ground enclosed by a ring of wall four blocks across.
//
// Everything outside the ring is open, so a check that asks only whether a destination is
// empty finds sixteen of them. Nothing outside can actually be walked to.
type sealedRing struct {
	groundTop      int64
	apertureHeight int64
}

func (w sealedRing) Block(x, y, z int64) (world.Block, bool) {
	if y <= w.groundTop || (y > w.groundTop+w.apertureHeight && y <= w.groundTop+5 && (x == -2 || x == 2 || z == -2 || z == 2)) {
		return world.Stone, true
	}
	return world.Air, true
}

func (w sealedRing) Fluid(x, y, z int64) bool { return fluidByBlock(w, x, y, z) }

func (w sealedRing) Solid(x, y, z int64) bool {
	block, resident := w.Block(x, y, z)
	return !resident || block != world.Air
}

// Safe space has to be reachable, not merely empty.
//
// **The spear's defect one rule over**, and the reason this test exists at all: the escape
// check tested each sampled destination and never the walk to it, so clear ground on the
// far side of a wall counted as somewhere to go. That fails open — it would let the
// scheduler announce a ritual whose only way out nobody can take.
//
// A centre ray misses low ceilings; the route must fit the whole walking body.
func TestAnEscapeBehindAWallIsNotAnEscape(t *testing.T) {
	// A region that endangers nothing, so the only thing deciding the answer is whether a
	// destination can be reached.
	harmless := encounterMoveDef{
		kind: vnet.EncounterMoveKindBurial, fromStage: 1,
		telegraph: 900 * time.Millisecond, release: 200 * time.Millisecond,
		recovery: time.Second, minRange: 0, maxRange: 40, damagePercent: 10,
		hazard: encounterHazard{shape: vnet.HazardShapeDisc, reach: 0.05, height: 0.05},
	}

	t.Run("open ground leaves one", func(t *testing.T) {
		h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
		player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
		king := pullKing(t, h, [3]float64{0.5, 64, -3.0}, player)
		h.sim.mu.Lock()
		defer h.sim.mu.Unlock()
		if !h.sim.moveLeavesAnEscapeLocked(h.sim.mobs[king], harmless, player) {
			t.Fatal("open ground with a harmless region was judged to leave nowhere to stand")
		}
	})

	for _, aperture := range []int64{0, 1} {
		t.Run(fmt.Sprintf("wall with %d-block aperture", aperture), func(t *testing.T) {
			h := newVitalsHarness(t, DefaultTickRate, sealedRing{groundTop: 63, apertureHeight: aperture})
			player, _ := h.join(1, [3]float32{0.5, 64, 0.5})
			king := pullKing(t, h, [3]float64{0.5, 64, -0.5}, player)

			h.sim.mu.Lock()
			defer h.sim.mu.Unlock()
			// The premise: there really is empty ground at the sampled distance, so this is
			// not passing because the destinations were solid.
			reach := WalkSpeed * harmless.telegraph.Seconds()
			outside := playerBox([3]float64{player.pos[0] + reach, player.pos[1], player.pos[2]})
			if anyVoxel(outside, h.sim.terrain.Solid) {
				t.Fatal("the ground outside the ring is not clear, so this tests nothing")
			}
			if h.sim.moveLeavesAnEscapeLocked(h.sim.mobs[king], harmless, player) {
				t.Fatal("ground nobody can walk to was counted as an escape")
			}
		})
	}
}
