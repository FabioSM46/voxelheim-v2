package protocol

import (
	"math"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	flatbuffers "github.com/google/flatbuffers/go"
)

// A cone that is legal in every clause, used as the sound base every refusal below
// breaks exactly one field of.
func soundHazard() HazardVolume {
	return HazardVolume{
		Shape:     vnet.HazardShapeCone,
		Origin:    [3]float32{12, 61, -40},
		Direction: [3]float32{0, 0, 1},
		Radius:    6,
		Height:    3,
		HalfAngle: 0.7,
	}
}

// A telegraphed bite: the shape the whole encounter rests on, announced and not yet
// dangerous.
func soundMove() EncounterMove {
	return EncounterMove{
		MoveInstanceID:   0x51,
		Kind:             vnet.EncounterMoveKindBiteAndTear,
		Phase:            vnet.MovePhaseTelegraph,
		PhaseStartedTick: 900,
		PhaseTicks:       27,
		TargetEntityID:   77,
		Hazards:          []HazardVolume{soundHazard()},
	}
}

func soundTimeline() EncounterTimeline {
	return EncounterTimeline{
		EncounterID:  0xDEC0DE,
		BossEntityID: 4242,
		Boss:         vnet.MobKindVargrGuardian,
		Phase:        1,
		Moves:        []EncounterMove{soundMove()},
	}
}

func encodedTimeline(t *testing.T, timeline EncounterTimeline) vnet.EncounterTimeline {
	t.Helper()
	frame, err := EncodeEncounterTimeline(timeline)
	if err != nil {
		t.Fatalf("EncodeEncounterTimeline: %v", err)
	}
	msg, err := Decode(frame)
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if msg.Kind != vnet.PayloadEncounterTimeline {
		t.Fatalf("Kind = %s", msg.Kind)
	}
	// Server -> client, so this package decodes it only as the strict Go half of the
	// contract; reading it back through the generated bindings is what says the
	// announcement survived the trip.
	env := vnet.GetRootAsEnvelope(frame, 0)
	var payload flatbuffers.Table
	if !env.Payload(&payload) {
		t.Fatal("the envelope carries no payload")
	}
	var decoded vnet.EncounterTimeline
	decoded.Init(payload.Bytes, payload.Pos)
	return decoded
}

func TestEncounterTimelineCarriesTheAnnouncementWhole(t *testing.T) {
	t.Parallel()

	want := soundTimeline()
	decoded := encodedTimeline(t, want)
	if decoded.EncounterId() != want.EncounterID || decoded.BossEntityId() != want.BossEntityID ||
		decoded.Boss() != want.Boss || decoded.Phase() != want.Phase || decoded.MovesLength() != 1 {
		t.Fatalf("the timeline did not round trip: %+v", decoded)
	}

	var move vnet.EncounterMove
	if !decoded.Moves(&move, 0) {
		t.Fatal("the one announced move is missing")
	}
	from := want.Moves[0]
	if move.MoveInstanceId() != from.MoveInstanceID || move.Kind() != from.Kind ||
		move.Phase() != from.Phase || move.PhaseStartedTick() != from.PhaseStartedTick ||
		move.PhaseTicks() != from.PhaseTicks || move.TargetEntityId() != from.TargetEntityID ||
		move.PulseIndex() != 0 || move.PulseTotal() != 0 || move.Interruptible() ||
		move.Ended() != vnet.MoveEndUnknown || move.HazardsLength() != 1 {
		t.Fatalf("the move did not round trip: %+v", move)
	}
	// A tracking move carries no aim, and that absence is what the contract lets a
	// receiver read the target/direction choice from.
	if move.Aim(nil) != nil {
		t.Fatal("a move that names a target also fixed a direction")
	}

	var hazard vnet.HazardVolume
	if !move.Hazards(&hazard, 0) {
		t.Fatal("the announced region is missing")
	}
	origin, direction := hazard.Origin(nil), hazard.Direction(nil)
	source := from.Hazards[0]
	if hazard.Shape() != source.Shape || origin == nil || direction == nil ||
		[3]float32{origin.X(), origin.Y(), origin.Z()} != source.Origin ||
		[3]float32{direction.X(), direction.Y(), direction.Z()} != source.Direction ||
		hazard.Radius() != source.Radius || hazard.Height() != source.Height ||
		hazard.HalfAngle() != source.HalfAngle {
		t.Fatalf("the hazard did not round trip: %+v", hazard)
	}
}

// A move that has fixed its lane sends the direction and clears the target, which is
// what makes a charge dodgeable by stepping aside: the contract promises the lane will
// not follow, and the encoder is where that promise is kept.
func TestAnAimedMoveSendsItsLaneAndNoTarget(t *testing.T) {
	t.Parallel()

	charge := soundMove()
	charge.Kind = vnet.EncounterMoveKindCollarCharge
	charge.Phase = vnet.MovePhaseRelease
	charge.TargetEntityID = 0
	charge.Aim = [3]float32{1, 0, 0}
	charge.Hazards = []HazardVolume{{
		Shape:     vnet.HazardShapeLine,
		Origin:    [3]float32{0, 61, 0},
		Direction: [3]float32{1, 0, 0},
		Radius:    18,
		Height:    3,
		HalfWidth: 1.5,
	}}

	timeline := soundTimeline()
	timeline.Moves = []EncounterMove{charge}
	decoded := encodedTimeline(t, timeline)

	var move vnet.EncounterMove
	if !decoded.Moves(&move, 0) {
		t.Fatal("the one announced move is missing")
	}
	aim := move.Aim(nil)
	if aim == nil {
		t.Fatal("a move that names no target fixed no direction either")
	}
	if [3]float32{aim.X(), aim.Y(), aim.Z()} != charge.Aim || move.TargetEntityId() != 0 {
		t.Fatalf("the lane did not round trip: %+v", move)
	}
}

// A channel counts its pulses, and the count is what a receiver reads the next impulse
// from. The ritual is also the only shape this contract lets an interrupt end.
func TestAChannelCountsItsPulsesAndMayBeInterruptible(t *testing.T) {
	t.Parallel()

	requiem := soundMove()
	requiem.Kind = vnet.EncounterMoveKindRequiemOfTheBuried
	requiem.Phase = vnet.MovePhaseChannel
	requiem.PulseIndex = 1
	requiem.PulseTotal = 3
	requiem.Interruptible = true
	requiem.Ended = vnet.MoveEndInterrupted
	requiem.Hazards = []HazardVolume{{
		Shape:       vnet.HazardShapeRing,
		Origin:      [3]float32{0, 61, 0},
		Direction:   [3]float32{},
		Radius:      12,
		Height:      2,
		InnerRadius: 8,
	}}

	timeline := soundTimeline()
	timeline.Boss = vnet.MobKindDraugrKing
	timeline.Phase = 2
	timeline.Moves = []EncounterMove{requiem}
	decoded := encodedTimeline(t, timeline)

	var move vnet.EncounterMove
	if !decoded.Moves(&move, 0) {
		t.Fatal("the one announced move is missing")
	}
	if move.PulseIndex() != 1 || move.PulseTotal() != 3 || !move.Interruptible() ||
		move.Ended() != vnet.MoveEndInterrupted {
		t.Fatalf("the channel did not round trip: %+v", move)
	}
}

// Every decoder invariant schemas/player.fbs states is checked on the way out too. A
// server that emitted one of these would be asking a client to drop a frame it needs to
// read the fight — which is the one frame in this contract a player cannot do without.
func TestEncounterTimelineRefusesWhatNoClientCanRead(t *testing.T) {
	t.Parallel()

	withMove := func(mutate func(*EncounterMove)) EncounterTimeline {
		timeline := soundTimeline()
		move := soundMove()
		mutate(&move)
		timeline.Moves = []EncounterMove{move}
		return timeline
	}
	withHazard := func(mutate func(*HazardVolume)) EncounterTimeline {
		return withMove(func(move *EncounterMove) {
			hazard := soundHazard()
			mutate(&hazard)
			move.Hazards = []HazardVolume{hazard}
		})
	}

	cases := map[string]EncounterTimeline{
		"no encounter id":     {BossEntityID: 1, Boss: vnet.MobKindVargrGuardian, Phase: 1},
		"no boss entity":      {EncounterID: 1, Boss: vnet.MobKindVargrGuardian, Phase: 1},
		"no boss kind":        {EncounterID: 1, BossEntityID: 1, Phase: 1},
		"invented boss":       {EncounterID: 1, BossEntityID: 1, Boss: vnet.MobKind(200), Phase: 1},
		"no phase":            {EncounterID: 1, BossEntityID: 1, Boss: vnet.MobKindVargrGuardian, Phase: 0},
		"no instance id":      withMove(func(m *EncounterMove) { m.MoveInstanceID = 0 }),
		"no move kind":        withMove(func(m *EncounterMove) { m.Kind = vnet.EncounterMoveKindUnknown }),
		"invented move":       withMove(func(m *EncounterMove) { m.Kind = vnet.EncounterMoveKind(200) }),
		"no move phase":       withMove(func(m *EncounterMove) { m.Phase = vnet.MovePhaseUnknown }),
		"invented phase":      withMove(func(m *EncounterMove) { m.Phase = vnet.MovePhase(200) }),
		"a phase of no ticks": withMove(func(m *EncounterMove) { m.PhaseTicks = 0 }),
		"neither target nor aim": withMove(func(m *EncounterMove) {
			m.TargetEntityID = 0
		}),
		"a non-finite aim": withMove(func(m *EncounterMove) {
			m.TargetEntityID = 0
			m.Aim = [3]float32{float32(math.NaN()), 0, 1}
		}),
		"a channel with no pulses": withMove(func(m *EncounterMove) {
			m.Phase = vnet.MovePhaseChannel
		}),
		"a pulse past the last": withMove(func(m *EncounterMove) {
			m.Phase = vnet.MovePhaseChannel
			m.PulseIndex, m.PulseTotal = 3, 3
		}),
		"a swing that counts pulses": withMove(func(m *EncounterMove) { m.PulseTotal = 2 }),
		"a swing that claims to be interruptible": withMove(func(m *EncounterMove) {
			m.Interruptible = true
		}),
		"an uninterruptible move that was interrupted": withMove(func(m *EncounterMove) {
			m.Phase = vnet.MovePhaseChannel
			m.PulseTotal = 2
			m.Ended = vnet.MoveEndInterrupted
		}),
		"an invented ending": withMove(func(m *EncounterMove) { m.Ended = vnet.MoveEnd(200) }),
		"too many hazards": withMove(func(m *EncounterMove) {
			m.Hazards = make([]HazardVolume, MaxHazardsPerMove+1)
			for i := range m.Hazards {
				m.Hazards[i] = soundHazard()
			}
		}),
		"no hazard shape": withHazard(func(h *HazardVolume) { h.Shape = vnet.HazardShapeUnknown }),
		"invented shape":  withHazard(func(h *HazardVolume) { h.Shape = vnet.HazardShape(200) }),
		"a non-finite origin": withHazard(func(h *HazardVolume) {
			h.Origin = [3]float32{float32(math.Inf(1)), 61, 0}
		}),
		"an origin off the world": withHazard(func(h *HazardVolume) {
			h.Origin = [3]float32{2 * MaxWorldCoordinate, 61, 0}
		}),
		"a non-finite direction": withHazard(func(h *HazardVolume) {
			h.Direction = [3]float32{0, 0, float32(math.NaN())}
		}),
		"a cone pointing nowhere": withHazard(func(h *HazardVolume) { h.Direction = [3]float32{} }),
		"a cone with no opening":  withHazard(func(h *HazardVolume) { h.HalfAngle = 0 }),
		"an angle past half a turn": withHazard(func(h *HazardVolume) {
			h.HalfAngle = float32(math.Pi) * 2
		}),
		"no reach":               withHazard(func(h *HazardVolume) { h.Radius = 0 }),
		"a reach past the arena": withHazard(func(h *HazardVolume) { h.Radius = MaxHazardExtent + 1 }),
		"a non-finite reach": withHazard(func(h *HazardVolume) {
			h.Radius = float32(math.Inf(1))
		}),
		"no height":               withHazard(func(h *HazardVolume) { h.Height = 0 }),
		"a height past the arena": withHazard(func(h *HazardVolume) { h.Height = MaxHazardExtent + 1 }),
		"an inner radius outside its radius": withHazard(func(h *HazardVolume) {
			h.InnerRadius = h.Radius + 1
		}),
		"a width wider than its reach": withHazard(func(h *HazardVolume) {
			h.Shape, h.HalfWidth = vnet.HazardShapeLine, h.Radius+1
		}),
		"a line with no width": withHazard(func(h *HazardVolume) {
			h.Shape, h.HalfWidth = vnet.HazardShapeLine, 0
		}),
		"a line pointing nowhere": withHazard(func(h *HazardVolume) {
			h.Shape, h.Direction, h.HalfWidth = vnet.HazardShapeLine, [3]float32{}, 1
		}),
		"a ring with no band": withHazard(func(h *HazardVolume) {
			h.Shape, h.InnerRadius = vnet.HazardShapeRing, h.Radius
		}),
	}

	// Two announcements of one instance id: the id is what lets a receiver tell a stale
	// move from a new one, so one appearing twice in a frame is a list no correct server
	// holds.
	duplicate := soundTimeline()
	duplicate.Moves = []EncounterMove{soundMove(), soundMove()}
	cases["two moves sharing an instance id"] = duplicate

	crowded := soundTimeline()
	crowded.Moves = make([]EncounterMove, MaxLiveMoves+1)
	for i := range crowded.Moves {
		crowded.Moves[i] = soundMove()
		crowded.Moves[i].MoveInstanceID = uint64(i + 1)
	}
	cases["more moves than any encounter announces"] = crowded

	for name, timeline := range cases {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			if _, err := EncodeEncounterTimeline(timeline); err == nil {
				t.Fatal("an unreadable announcement was encoded")
			}
		})
	}
}

// The boundaries, tested at the value that is legal rather than only at the one past it.
//
// **Both directions, because a validator can fail either way and only one of them is
// loud.** Refusing a value over the line costs a frame nobody should have sent; refusing
// one exactly on it costs a frame somebody needed, and says nothing about why. An empty
// hazard vector is the case that matters most here: a `Recovery` endangers nothing, and
// a `> 0` in place of the length bound would make the encounter's whole reward window
// unsendable.
func TestEncounterTimelineAcceptsItsOwnBoundaries(t *testing.T) {
	t.Parallel()

	moves := map[string]EncounterMove{}

	recovery := soundMove()
	recovery.Phase = vnet.MovePhaseRecovery
	recovery.Hazards = nil
	recovery.Ended = vnet.MoveEndCompleted
	moves["a recovery that endangers nothing"] = recovery

	cancelled := soundMove()
	cancelled.Ended = vnet.MoveEndCancelled
	moves["a telegraph the encounter withdrew"] = cancelled

	firstPulse := soundMove()
	firstPulse.Phase = vnet.MovePhaseChannel
	firstPulse.PulseIndex, firstPulse.PulseTotal = 0, 1
	moves["a channel of one pulse"] = firstPulse

	lastPulse := soundMove()
	lastPulse.Phase = vnet.MovePhaseChannel
	lastPulse.PulseIndex, lastPulse.PulseTotal = 254, 255
	moves["the last pulse of the longest channel"] = lastPulse

	edge := soundMove()
	edge.Hazards = []HazardVolume{{
		Shape:     vnet.HazardShapeCone,
		Origin:    [3]float32{MaxWorldCoordinate, -MaxWorldCoordinate, 0},
		Direction: [3]float32{0, 0, 1},
		Radius:    MaxHazardExtent,
		Height:    MaxHazardExtent,
		HalfAngle: float32(math.Pi),
	}}
	moves["a cone at every bound it has"] = edge

	full := soundMove()
	full.Hazards = make([]HazardVolume, MaxHazardsPerMove)
	for i := range full.Hazards {
		full.Hazards[i] = soundHazard()
		full.Hazards[i].Shape = vnet.HazardShapeDisc
	}
	moves["as many sectors as a ritual may light"] = full

	// A disc ignores direction, inner radius, angle and width, and a server writes zero
	// in all four. Refusing that would make three of the four shapes unsendable.
	disc := soundMove()
	disc.Hazards = []HazardVolume{{
		Shape:  vnet.HazardShapeDisc,
		Origin: [3]float32{0, 61, 0},
		Radius: 5,
		Height: 4,
	}}
	moves["a disc that reads none of the optional fields"] = disc

	for name, move := range moves {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			timeline := soundTimeline()
			timeline.Moves = []EncounterMove{move}
			if _, err := EncodeEncounterTimeline(timeline); err != nil {
				t.Fatalf("a legal announcement was refused: %v", err)
			}
		})
	}

	// An empty timeline is a statement — this boss is announcing nothing — and never the
	// absence of one. A recipient replaces its copy wholesale, so silence would leave a
	// hazard standing that the server has already withdrawn.
	silent := soundTimeline()
	silent.Moves = nil
	if _, err := EncodeEncounterTimeline(silent); err != nil {
		t.Fatalf("a boss announcing nothing was refused: %v", err)
	}

	crowded := soundTimeline()
	crowded.Moves = make([]EncounterMove, MaxLiveMoves)
	for i := range crowded.Moves {
		crowded.Moves[i] = soundMove()
		crowded.Moves[i].MoveInstanceID = uint64(i + 1)
	}
	if _, err := EncodeEncounterTimeline(crowded); err != nil {
		t.Fatalf("a full announcement set was refused: %v", err)
	}
}

// Every late entry carries the complete position, including recovery and ending.
func TestPhysicalComboPositionsRoundTripAcrossPhasesAndEndings(t *testing.T) {
	for _, kind := range []vnet.EncounterMoveKind{vnet.EncounterMoveKindBiteAndTear, vnet.EncounterMoveKindThreeTolls, vnet.EncounterMoveKindPrisonerClaws} {
		total := uint8(2)
		if kind == vnet.EncounterMoveKindThreeTolls {
			total = 3
		}
		for step := uint8(1); step <= total; step++ {
			for _, phase := range []vnet.MovePhase{vnet.MovePhaseTelegraph, vnet.MovePhaseRelease, vnet.MovePhaseRecovery} {
				for _, ending := range []vnet.MoveEnd{vnet.MoveEndUnknown, vnet.MoveEndCompleted, vnet.MoveEndCancelled} {
					timeline := soundTimeline()
					timeline.Moves[0].Kind = kind
					timeline.Moves[0].Phase = phase
					timeline.Moves[0].Ended = ending
					timeline.Moves[0].ComboStep, timeline.Moves[0].ComboTotal = step, total
					decoded := encodedTimeline(t, timeline)
					var move vnet.EncounterMove
					if !decoded.Moves(&move, 0) || move.ComboStep() != step || move.ComboTotal() != total || move.Ended() != ending {
						t.Fatal("combo position lost")
					}
				}
			}
		}
	}
}

func TestPhysicalComboRejectsMalformedCombinations(t *testing.T) {
	for _, tc := range []struct {
		name        string
		kind        vnet.EncounterMoveKind
		step, total uint8
		phase       vnet.MovePhase
		valid       bool
	}{
		{"ordinary bite", vnet.EncounterMoveKindBiteAndTear, 0, 0, vnet.MovePhaseTelegraph, true},
		{"first bite", vnet.EncounterMoveKindBiteAndTear, 1, 2, vnet.MovePhaseTelegraph, true},
		{"last bite", vnet.EncounterMoveKindBiteAndTear, 2, 2, vnet.MovePhaseTelegraph, true},
		{"last toll", vnet.EncounterMoveKindThreeTolls, 3, 3, vnet.MovePhaseTelegraph, true},
		{"paired claws", vnet.EncounterMoveKindPrisonerClaws, 2, 2, vnet.MovePhaseTelegraph, true},
		{"triple claws", vnet.EncounterMoveKindPrisonerClaws, 3, 3, vnet.MovePhaseTelegraph, true},
		{"zero step", vnet.EncounterMoveKindBiteAndTear, 0, 2, vnet.MovePhaseTelegraph, false},
		{"missing total", vnet.EncounterMoveKindBiteAndTear, 1, 0, vnet.MovePhaseTelegraph, false},
		{"single blow", vnet.EncounterMoveKindBiteAndTear, 1, 1, vnet.MovePhaseTelegraph, false},
		{"past last", vnet.EncounterMoveKindBiteAndTear, 3, 2, vnet.MovePhaseTelegraph, false},
		{"bite wrong total", vnet.EncounterMoveKindBiteAndTear, 1, 3, vnet.MovePhaseTelegraph, false},
		{"toll wrong total", vnet.EncounterMoveKindThreeTolls, 1, 2, vnet.MovePhaseTelegraph, false},
		{"oversized", vnet.EncounterMoveKindPrisonerClaws, 1, 4, vnet.MovePhaseTelegraph, false},
		{"byte maximum", vnet.EncounterMoveKindPrisonerClaws, 255, 255, vnet.MovePhaseTelegraph, false},
		{"ordinary charge", vnet.EncounterMoveKindCollarCharge, 0, 0, vnet.MovePhaseTelegraph, true},
		{"wrong physical kind", vnet.EncounterMoveKindCollarCharge, 1, 2, vnet.MovePhaseTelegraph, false},
		{"spell combo", vnet.EncounterMoveKindBurial, 1, 2, vnet.MovePhaseTelegraph, false},
		{"combo channel", vnet.EncounterMoveKindBiteAndTear, 1, 2, vnet.MovePhaseChannel, false},
		{"ordinary channel", vnet.EncounterMoveKindBurial, 0, 0, vnet.MovePhaseChannel, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			move := soundMove()
			move.Kind = tc.kind
			move.Phase = tc.phase
			if tc.phase == vnet.MovePhaseChannel {
				move.PulseTotal = 2
			}
			move.ComboStep, move.ComboTotal = tc.step, tc.total
			timeline := soundTimeline()
			timeline.Moves[0] = move
			_, err := EncodeEncounterTimeline(timeline)
			if (err == nil) != tc.valid {
				t.Fatalf("err=%v, valid=%v", err, tc.valid)
			}
		})
	}
}
