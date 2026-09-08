package game

import (
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	flatbuffers "github.com/google/flatbuffers/go"
)

// announcedTimeline is one decoded `EncounterTimeline` as a recipient reads it.
//
// Read back through the generated bindings rather than off the Go struct that produced
// it, for `disclosedBlows`' reason: what a test is entitled to assert about is the bytes
// a session was actually sent.
type announcedTimeline struct {
	encounterID  uint64
	bossEntityID uint64
	boss         vnet.MobKind
	phase        uint8
	moves        []protocol.EncounterMove
}

// timelines is every encounter timeline this session has been sent, in wire order, each
// checked against the snapshot it followed.
//
// **The snapshot check is the disclosure rule executed rather than asserted.** A timeline
// may only ever describe a boss the recipient has just been told exists, so a frame
// naming an entity absent from the newest mobs vector is a leak, and this fails on it
// rather than counting it.
func announcedTimelines(t *testing.T, frames [][]byte) []announcedTimeline {
	t.Helper()

	var snapshot *vnet.EntitySnapshot
	var announced []announcedTimeline
	for _, frame := range frames {
		env := vnet.GetRootAsEnvelope(frame, 0)
		kind := env.PayloadType()
		if kind != vnet.PayloadEntitySnapshot && kind != vnet.PayloadEncounterTimeline {
			continue
		}
		var tab flatbuffers.Table
		if !env.Payload(&tab) {
			t.Fatal("absent payload")
		}
		if kind == vnet.PayloadEntitySnapshot {
			snapshot = new(vnet.EntitySnapshot)
			snapshot.Init(tab.Bytes, tab.Pos)
			continue
		}

		var timeline vnet.EncounterTimeline
		timeline.Init(tab.Bytes, tab.Pos)
		if snapshot == nil {
			t.Fatal("a timeline arrived before any snapshot named a creature")
		}
		seen := false
		for i := range snapshot.MobsLength() {
			var mob vnet.MobState
			snapshot.Mobs(&mob, i)
			if mob.EntityId() == timeline.BossEntityId() {
				seen = true
				if mob.Kind() != timeline.Boss() {
					t.Fatalf("timeline says %s where the snapshot says %s", timeline.Boss(), mob.Kind())
				}
			}
		}
		if !seen {
			t.Fatalf("a timeline named entity %d, which this snapshot never mentioned", timeline.BossEntityId())
		}

		one := announcedTimeline{
			encounterID:  timeline.EncounterId(),
			bossEntityID: timeline.BossEntityId(),
			boss:         timeline.Boss(),
			phase:        timeline.Phase(),
		}
		for i := range timeline.MovesLength() {
			var move vnet.EncounterMove
			timeline.Moves(&move, i)
			one.moves = append(one.moves, protocol.EncounterMove{
				MoveInstanceID:   move.MoveInstanceId(),
				Kind:             move.Kind(),
				Phase:            move.Phase(),
				PhaseStartedTick: move.PhaseStartedTick(),
				PhaseTicks:       move.PhaseTicks(),
				TargetEntityID:   move.TargetEntityId(),
				PulseIndex:       move.PulseIndex(),
				PulseTotal:       move.PulseTotal(),
				Interruptible:    move.Interruptible(),
				Ended:            move.Ended(),
			})
			// The announced regions, read back through the bindings for the reason every
			// other field here is: what a test may assert about is the bytes a session was
			// actually sent, and the geometry is the half of an announcement a player
			// reacts to.
			read := &one.moves[len(one.moves)-1]
			for j := range move.HazardsLength() {
				var hazard vnet.HazardVolume
				move.Hazards(&hazard, j)
				var origin, direction vnet.Vec3
				hazard.Origin(&origin)
				hazard.Direction(&direction)
				read.Hazards = append(read.Hazards, protocol.HazardVolume{
					Shape:       hazard.Shape(),
					Origin:      [3]float32{origin.X(), origin.Y(), origin.Z()},
					Direction:   [3]float32{direction.X(), direction.Y(), direction.Z()},
					Radius:      hazard.Radius(),
					Height:      hazard.Height(),
					InnerRadius: hazard.InnerRadius(),
					HalfAngle:   hazard.HalfAngle(),
					HalfWidth:   hazard.HalfWidth(),
				})
			}
		}
		announced = append(announced, one)
	}
	return announced
}

// newest is the last timeline in a batch, which is the only one that is still true.
func newestTimeline(t *testing.T, frames [][]byte) announcedTimeline {
	t.Helper()
	announced := announcedTimelines(t, frames)
	if len(announced) == 0 {
		t.Fatal("this session was told nothing about the encounter")
	}
	return announced[len(announced)-1]
}

// A sound telegraph, so a test that is not about a move's contents never has to write
// one. It is not what the boss decided to do — nothing decides that yet, which is #1024 —
// it is a legal announcement placed in the encounter by hand so the publication path can
// be exercised end to end.
func testAnnouncement(id uint64) protocol.EncounterMove {
	return protocol.EncounterMove{
		MoveInstanceID:   id,
		Kind:             vnet.EncounterMoveKindBiteAndTear,
		Phase:            vnet.MovePhaseTelegraph,
		PhaseStartedTick: 1,
		PhaseTicks:       18,
		TargetEntityID:   1,
		Hazards: []protocol.HazardVolume{{
			Shape:     vnet.HazardShapeCone,
			Origin:    [3]float32{0.5, 64, -40.5},
			Direction: [3]float32{0, 0, 1},
			Radius:    3,
			Height:    2,
			HalfAngle: 0.6,
		}},
	}
}

// A boss standing in an empty room announces nothing, and the first pull is what starts
// the fight.
//
// **Silence and an empty list are different statements**, and this is the pair that says
// so: before the pull there is no timeline at all, because there is no encounter to
// describe; after it there is one carrying no moves, which says this boss is announcing
// nothing *right now*.
func TestABossAnnouncesNothingUntilSomebodyPullsIt(t *testing.T) {
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	_, out := h.join(1, [3]float32{0.5, 64, 0.5})

	// Far enough that nothing notices anybody: well outside the 24-block aggro range and
	// inside the streamed cube, so the boss is visible without being pulled.
	boss := h.placeSpeciesAt(vnet.MobKindVargrGuardian, [3]float64{0.5, 64, -40.5})
	h.step()
	if got := announcedTimelines(t, out.all()); len(got) != 0 {
		t.Fatalf("an unpulled boss announced %d timelines", len(got))
	}

	h.sim.mu.Lock()
	h.sim.startBossEncounterLocked(h.sim.mobs[boss], h.sim.players[1])
	h.sim.mu.Unlock()
	h.step()

	timeline := newestTimeline(t, out.all())
	if timeline.encounterID == 0 {
		t.Fatal("the encounter has no identity, which the encoder is supposed to refuse")
	}
	if timeline.bossEntityID != boss || timeline.boss != vnet.MobKindVargrGuardian {
		t.Fatalf("the timeline names %+v", timeline)
	}
	if timeline.phase != startEncounterPhase {
		t.Fatalf("a fresh encounter is at stage %d, want %d", timeline.phase, startEncounterPhase)
	}
	if len(timeline.moves) != 0 {
		t.Fatalf("a boss nothing has told to act announced %d moves", len(timeline.moves))
	}
}

// The stage a timeline carries is the one this tick's health produced, and it never goes
// back.
//
// A healed boss keeping the stage it reached is the decision, not a side effect: a stage
// is a repertoire the encounter has unlocked, and a client told to go back one would have
// to un-draw a fight it had already seen escalate.
func TestTheStageRisesWithTheDamageAndNeverFallsBack(t *testing.T) {
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	_, out := h.join(1, [3]float32{0.5, 64, 0.5})
	boss := h.placeSpeciesAt(vnet.MobKindDraugrKing, [3]float64{0.5, 64, -40.5})

	h.sim.mu.Lock()
	h.sim.startBossEncounterLocked(h.sim.mobs[boss], h.sim.players[1])
	h.sim.mu.Unlock()

	full := mobRegistry[vnet.MobKindDraugrKing].maxHealth
	// Widened before the multiplication rather than after: `full * 70` overflows a uint16
	// at 1200 health, and a wrapped value would put this boss somewhere it never was.
	at := func(percent uint32) uint16 { return uint16(uint32(full) * percent / 100) }
	for _, step := range []struct {
		name   string
		health uint16
		want   uint8
	}{
		{"untouched", full, 1},
		{"one point above the first threshold", at(70) + 1, 1},
		{"exactly on the first threshold", at(70), 2},
		{"exactly on the second", at(35), 3},
		{"healed back to full", full, 3},
	} {
		h.sim.mu.Lock()
		h.sim.mobs[boss].health = step.health
		h.sim.mu.Unlock()
		h.step()
		if got := newestTimeline(t, out.all()).phase; got != step.want {
			t.Fatalf("%s: stage %d, want %d", step.name, got, step.want)
		}
	}
}

// An ending is published exactly once, and a move that has ended is gone from the next
// timeline.
//
// **Both halves matter and they fail in opposite directions.** A sweep that ran too early
// would take the ending away before anybody was told why the move stopped, leaving a
// client to infer it from a disappearance; one that never ran would republish an ended
// move for ever, so a hazard the encounter had withdrawn would keep being announced.
func TestAnEndedAnnouncementIsSentOnceAndThenIsGone(t *testing.T) {
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	_, out := h.join(1, [3]float32{0.5, 64, 0.5})
	boss := h.placeSpeciesAt(vnet.MobKindVargrGuardian, [3]float64{0.5, 64, -40.5})

	h.sim.mu.Lock()
	h.sim.startBossEncounterLocked(h.sim.mobs[boss], h.sim.players[1])
	h.sim.mobs[boss].encounter.moves = []protocol.EncounterMove{testAnnouncement(7)}
	h.sim.mu.Unlock()

	h.step()
	if got := newestTimeline(t, out.all()).moves; len(got) != 1 || got[0].MoveInstanceID != 7 ||
		got[0].Ended != vnet.MoveEndUnknown {
		t.Fatalf("a live announcement arrived as %+v", got)
	}

	// The ending, written the way the move executor will write it.
	h.sim.mu.Lock()
	h.sim.mobs[boss].encounter.moves[0].Ended = vnet.MoveEndCompleted
	h.sim.mu.Unlock()

	h.step()
	if got := newestTimeline(t, out.all()).moves; len(got) != 1 || got[0].Ended != vnet.MoveEndCompleted {
		t.Fatalf("the ending was not published: %+v", got)
	}

	h.step()
	if got := newestTimeline(t, out.all()).moves; len(got) != 0 {
		t.Fatalf("an ended announcement was republished: %+v", got)
	}
}

// A withdrawal cancels what is still running and leaves an ending that already happened
// alone.
//
// `Cancelled` and not `Completed`: the contract distinguishes a move that ran to its end
// from one the encounter took away, and a body leaving the world is the second.
func TestWithdrawalCancelsWhatIsRunningAndKeepsAnEndingItFinds(t *testing.T) {
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	h.join(1, [3]float32{0.5, 64, 0.5})
	boss := h.placeSpeciesAt(vnet.MobKindVargrGuardian, [3]float64{0.5, 64, -40.5})

	h.sim.mu.Lock()
	defer h.sim.mu.Unlock()
	m := h.sim.mobs[boss]
	h.sim.startBossEncounterLocked(m, h.sim.players[1])
	running, finished := testAnnouncement(1), testAnnouncement(2)
	finished.Ended = vnet.MoveEndCompleted
	m.encounter.moves = []protocol.EncounterMove{running, finished}

	withdrawEncounterMovesLocked(m)
	if m.encounter.moves[0].Ended != vnet.MoveEndCancelled {
		t.Fatalf("a running move ended as %s", m.encounter.moves[0].Ended)
	}
	if m.encounter.moves[1].Ended != vnet.MoveEndCompleted {
		t.Fatalf("a finished move was re-ended as %s", m.encounter.moves[1].Ended)
	}
}

// A boss nobody can see is a boss nobody is told about.
//
// The rule is a projection of the snapshot rather than a second interest test, so this is
// really asking whether the two can disagree: a recipient outside the streamed cube gets
// no mobs vector entry and must therefore get no timeline either.
func TestATimelineOnlyEverFollowsASnapshotThatNamedItsBoss(t *testing.T) {
	h := newVitalsHarnessAt(t, DefaultTickRate, dropTerrain{groundTop: 63}, 1)
	_, near := h.join(1, [3]float32{0.5, 64, 0.5})
	_, far := h.join(2, [3]float32{0.5, 64, 4096.5})
	boss := h.placeSpeciesAt(vnet.MobKindVargrGuardian, [3]float64{0.5, 64, -8.5})

	h.sim.mu.Lock()
	h.sim.startBossEncounterLocked(h.sim.mobs[boss], h.sim.players[1])
	h.sim.mu.Unlock()
	h.step()

	if got := announcedTimelines(t, near.all()); len(got) == 0 {
		t.Fatal("the player in the room was told nothing")
	}
	// `announcedTimelines` fails on a timeline whose boss the snapshot never named, so
	// reaching a count of zero here is the whole assertion.
	if got := announcedTimelines(t, far.all()); len(got) != 0 {
		t.Fatalf("a player four thousand blocks away was told about %d encounters", len(got))
	}
}

// A dead boss announces nothing more, and its corpse announces nothing either.
//
// The corpse keeps the same entity id and stays in the snapshot's mobs vector, so this is
// exactly the case where a lookup against the live creatures is what stops a body from
// being described as though it were still fighting.
func TestAKilledBossStopsAnnouncing(t *testing.T) {
	h := newVitalsHarness(t, DefaultTickRate, dropTerrain{groundTop: 63})
	player, out := h.join(1, [3]float32{0.5, 64, 0.5})
	boss := h.placeSpeciesAt(vnet.MobKindVargrGuardian, [3]float64{0.5, 64, -2.5})

	h.sim.mu.Lock()
	h.sim.startBossEncounterLocked(h.sim.mobs[boss], player)
	h.sim.mobs[boss].encounter.moves = []protocol.EncounterMove{testAnnouncement(3)}
	h.sim.mu.Unlock()
	h.step()
	if got := announcedTimelines(t, out.all()); len(got) == 0 {
		t.Fatal("a pulled boss announced nothing")
	}

	h.sim.mu.Lock()
	h.sim.damageMobLocked(h.sim.mobs[boss], mobRegistry[vnet.MobKindVargrGuardian].maxHealth)
	h.sim.mu.Unlock()

	before := len(announcedTimelines(t, out.all()))
	h.step()
	h.step()
	if got := announcedTimelines(t, out.all()); len(got) != before {
		t.Fatalf("a killed boss went on announcing: %d timelines, was %d", len(got), before)
	}
}
