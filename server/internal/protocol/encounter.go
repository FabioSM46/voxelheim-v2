package protocol

import (
	"fmt"
	"math"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	flatbuffers "github.com/google/flatbuffers/go"
)

// MaxHazardExtent is the furthest a single announced region may reach or rise, in
// blocks. No move in the approved encounter reaches further and no arena is that wide;
// the bound exists because an unbounded volume reaches a renderer as a shape covering
// the world, and a receiver that clamped one would be inventing a danger boundary the
// server never stated.
const MaxHazardExtent = 128

// MaxHazardsPerMove is the longest hazard vector one move may announce. More sectors
// than the widest ritual lights at once.
const MaxHazardsPerMove = 16

// MaxLiveMoves is the largest number of simultaneously announced moves one encounter
// may carry. More than any approved move set produces, and a bound because the vector
// drives the recipient's allocation.
const MaxLiveMoves = 8

// maxHalfAngle is pi as a float32 can hold it.
//
// The contract states the bound as pi radians, and a float32 cannot hold pi: the nearest
// value it has is slightly *above* it, so comparing against math.Pi in float64 refuses
// the widest cone a server can actually encode. Rounding the bound to the wire's own
// precision is what makes "at most half a turn" mean the same thing on both sides.
const maxHalfAngle = float32(math.Pi)

// HazardVolume is one bounded region an announced move endangers, in world space.
//
// It is the region, never the damage: whether anybody standing in it is hit is decided
// by the simulation and stated afterwards as a BlowLanded. This encoder only enforces
// that the shape a client is asked to draw is one it can draw.
type HazardVolume struct {
	Shape       vnet.HazardShape
	Origin      [3]float32
	Direction   [3]float32
	Radius      float32
	Height      float32
	InnerRadius float32
	HalfAngle   float32
	HalfWidth   float32
}

// EncounterMove is one announced move of one boss, as the server holds it this tick.
//
// Aim has no presence flag on purpose: the contract says a move carries a direction
// exactly when it names no target, so TargetEntityID is what decides which of the two is
// written. A move that fixed its lane clears its target, and one that is tracking a
// player sends no aim at all.
type EncounterMove struct {
	MoveInstanceID   uint64
	Kind             vnet.EncounterMoveKind
	Phase            vnet.MovePhase
	PhaseStartedTick uint32
	PhaseTicks       uint32
	TargetEntityID   uint64
	Aim              [3]float32
	Hazards          []HazardVolume
	PulseIndex       uint8
	PulseTotal       uint8
	Interruptible    bool
	ComboStep        uint8
	ComboTotal       uint8
	Ended            vnet.MoveEnd
}

// EncounterTimeline is one boss encounter's complete live announcement set.
//
// Superseding, and sent entire: a partial list has an ordering and a revision to get
// right, and a client holding a move the server has forgotten is drawing a danger that
// no longer exists. An empty Moves is a statement rather than an absence.
type EncounterTimeline struct {
	EncounterID  uint64
	BossEntityID uint64
	Boss         vnet.MobKind
	Phase        uint8
	Moves        []EncounterMove
}

func finiteVector(v [3]float32) bool {
	for _, component := range v {
		if math.IsNaN(float64(component)) || math.IsInf(float64(component), 0) {
			return false
		}
	}
	return true
}

func zeroVector(v [3]float32) bool {
	return v[0] == 0 && v[1] == 0 && v[2] == 0
}

func finiteIn(value float32, low, high float64) bool {
	if math.IsNaN(float64(value)) || math.IsInf(float64(value), 0) {
		return false
	}
	return float64(value) >= low && float64(value) <= high
}

// validate reports why this region could not be announced, if it could not.
//
// Every clause is one of the decoder invariants schemas/player.fbs states for
// HazardVolume, checked on the way out rather than trusted: a recipient is told to end
// the session over a volume it cannot bound, and a server that emitted one would be
// asking a client to drop a frame it needs to read the fight.
func (h HazardVolume) validate() error {
	if !finiteVector(h.Origin) {
		return fmt.Errorf("protocol: non-finite hazard origin")
	}
	for _, coordinate := range h.Origin {
		if coordinate < -MaxWorldCoordinate || coordinate > MaxWorldCoordinate {
			return fmt.Errorf("protocol: hazard origin is outside the world")
		}
	}
	if !finiteVector(h.Direction) {
		return fmt.Errorf("protocol: non-finite hazard direction")
	}
	if !finiteIn(h.Radius, 0, MaxHazardExtent) || h.Radius <= 0 {
		return fmt.Errorf("protocol: hazard radius %v is not a bounded reach", h.Radius)
	}
	if !finiteIn(h.Height, 0, MaxHazardExtent) || h.Height <= 0 {
		return fmt.Errorf("protocol: hazard height %v is not a bounded extent", h.Height)
	}
	if !finiteIn(h.InnerRadius, 0, float64(h.Radius)) {
		return fmt.Errorf("protocol: hazard inner radius %v is not inside its radius", h.InnerRadius)
	}
	if !finiteIn(h.HalfAngle, 0, float64(maxHalfAngle)) {
		return fmt.Errorf("protocol: hazard half angle %v is not an angle", h.HalfAngle)
	}
	if !finiteIn(h.HalfWidth, 0, float64(h.Radius)) {
		return fmt.Errorf("protocol: hazard half width %v is not inside its radius", h.HalfWidth)
	}
	// Per shape, and only the fields that shape reads. A field a shape ignores is left
	// at zero rather than refused, so a later shape can give it a meaning.
	switch h.Shape {
	case vnet.HazardShapeCone:
		if zeroVector(h.Direction) {
			return fmt.Errorf("protocol: a cone points nowhere")
		}
		if h.HalfAngle <= 0 {
			return fmt.Errorf("protocol: a cone has no opening")
		}
	case vnet.HazardShapeLine:
		if zeroVector(h.Direction) {
			return fmt.Errorf("protocol: a line points nowhere")
		}
		if h.HalfWidth <= 0 {
			return fmt.Errorf("protocol: a line has no width")
		}
	case vnet.HazardShapeDisc:
	case vnet.HazardShapeRing:
		if h.InnerRadius >= h.Radius {
			return fmt.Errorf("protocol: a ring has no band")
		}
	default:
		return fmt.Errorf("protocol: unknown hazard shape")
	}
	return nil
}

// validate reports why this move could not be announced, if it could not.
//
// The phase rules are the readable half of the encounter and are enforced here rather
// than left to the simulation: a swing that claimed to be interruptible would promise a
// player an escape the server does not honour, and a channel with no pulse count would
// leave a client with nothing to count down to.
func (m EncounterMove) validate() error {
	if m.MoveInstanceID == 0 {
		return fmt.Errorf("protocol: move has no instance id")
	}
	if _, known := vnet.EnumNamesEncounterMoveKind[m.Kind]; !known || m.Kind == vnet.EncounterMoveKindUnknown {
		return fmt.Errorf("protocol: unknown encounter move kind")
	}
	switch m.Phase {
	case vnet.MovePhaseTelegraph, vnet.MovePhaseRelease, vnet.MovePhaseChannel, vnet.MovePhaseRecovery:
	default:
		return fmt.Errorf("protocol: unknown move phase")
	}
	if m.PhaseTicks == 0 {
		return fmt.Errorf("protocol: move phase lasts no ticks")
	}
	if m.TargetEntityID == 0 {
		if !finiteVector(m.Aim) {
			return fmt.Errorf("protocol: non-finite move aim")
		}
		if zeroVector(m.Aim) {
			return fmt.Errorf("protocol: move names neither a target nor a direction")
		}
	}
	if len(m.Hazards) > MaxHazardsPerMove {
		return fmt.Errorf("protocol: move announces %d hazards", len(m.Hazards))
	}
	for _, hazard := range m.Hazards {
		if err := hazard.validate(); err != nil {
			return err
		}
	}
	if m.ComboStep != 0 || m.ComboTotal != 0 {
		legalTotal := (m.Kind == vnet.EncounterMoveKindBiteAndTear && m.ComboTotal == 2) ||
			(m.Kind == vnet.EncounterMoveKindThreeTolls && m.ComboTotal == 3) ||
			(m.Kind == vnet.EncounterMoveKindPrisonerClaws && (m.ComboTotal == 2 || m.ComboTotal == 3))
		if !legalTotal || m.ComboStep == 0 || m.ComboStep > m.ComboTotal || m.Phase == vnet.MovePhaseChannel {
			return fmt.Errorf("protocol: invalid physical combo position")
		}
	}
	if m.Phase == vnet.MovePhaseChannel {
		if m.PulseTotal == 0 {
			return fmt.Errorf("protocol: a channel has no pulses")
		}
		if m.PulseIndex >= m.PulseTotal {
			return fmt.Errorf("protocol: channel pulse %d of %d", m.PulseIndex, m.PulseTotal)
		}
	} else {
		if m.PulseTotal != 0 || m.PulseIndex != 0 {
			return fmt.Errorf("protocol: a move that is not channelling counts pulses")
		}
		if m.Interruptible {
			return fmt.Errorf("protocol: a move that is not channelling claims to be interruptible")
		}
	}
	// A receiver reads this through its zero member and never fails a frame over it —
	// but that is the receiver's tolerance, not a licence for this side to emit a value
	// it cannot name. Zero is legal here and means the instance is still running.
	if _, known := vnet.EnumNamesMoveEnd[m.Ended]; !known {
		return fmt.Errorf("protocol: unknown move end")
	}
	if m.Ended == vnet.MoveEndInterrupted && !m.Interruptible {
		return fmt.Errorf("protocol: an uninterruptible move was interrupted")
	}
	return nil
}

func addHazardVolume(b *flatbuffers.Builder, hazard HazardVolume) flatbuffers.UOffsetT {
	vnet.HazardVolumeStart(b)
	vnet.HazardVolumeAddShape(b, hazard.Shape)
	vnet.HazardVolumeAddOrigin(b, vnet.CreateVec3(b, hazard.Origin[0], hazard.Origin[1], hazard.Origin[2]))
	vnet.HazardVolumeAddDirection(b, vnet.CreateVec3(b, hazard.Direction[0], hazard.Direction[1], hazard.Direction[2]))
	vnet.HazardVolumeAddRadius(b, hazard.Radius)
	vnet.HazardVolumeAddHeight(b, hazard.Height)
	vnet.HazardVolumeAddInnerRadius(b, hazard.InnerRadius)
	vnet.HazardVolumeAddHalfAngle(b, hazard.HalfAngle)
	vnet.HazardVolumeAddHalfWidth(b, hazard.HalfWidth)
	return vnet.HazardVolumeEnd(b)
}

func addEncounterMove(b *flatbuffers.Builder, move EncounterMove) flatbuffers.UOffsetT {
	// Built in reverse, as FlatBuffers vectors require: the offsets have to exist
	// before the vector that points at them, and the vector is written back to front.
	offsets := make([]flatbuffers.UOffsetT, len(move.Hazards))
	for i, hazard := range move.Hazards {
		offsets[i] = addHazardVolume(b, hazard)
	}
	vnet.EncounterMoveStartHazardsVector(b, len(offsets))
	for i := len(offsets) - 1; i >= 0; i-- {
		b.PrependUOffsetT(offsets[i])
	}
	hazards := b.EndVector(len(offsets))

	vnet.EncounterMoveStart(b)
	vnet.EncounterMoveAddMoveInstanceId(b, move.MoveInstanceID)
	vnet.EncounterMoveAddKind(b, move.Kind)
	vnet.EncounterMoveAddPhase(b, move.Phase)
	vnet.EncounterMoveAddPhaseStartedTick(b, move.PhaseStartedTick)
	vnet.EncounterMoveAddPhaseTicks(b, move.PhaseTicks)
	vnet.EncounterMoveAddTargetEntityId(b, move.TargetEntityID)
	// Exactly one of the two, decided by the target, because that is what the contract
	// says a receiver may rely on.
	if move.TargetEntityID == 0 {
		vnet.EncounterMoveAddAim(b, vnet.CreateVec3(b, move.Aim[0], move.Aim[1], move.Aim[2]))
	}
	vnet.EncounterMoveAddHazards(b, hazards)
	vnet.EncounterMoveAddPulseIndex(b, move.PulseIndex)
	vnet.EncounterMoveAddPulseTotal(b, move.PulseTotal)
	vnet.EncounterMoveAddInterruptible(b, move.Interruptible)
	vnet.EncounterMoveAddEnded(b, move.Ended)
	vnet.EncounterMoveAddComboStep(b, move.ComboStep)
	vnet.EncounterMoveAddComboTotal(b, move.ComboTotal)
	return vnet.EncounterMoveEnd(b)
}

// EncodeEncounterTimeline builds one boss's complete live timeline.
//
// Every entry is validated and a duplicate instance id is refused: an id is what lets a
// receiver tell a stale announcement from a new one that happens to be the same move, so
// one appearing twice in a single frame is a list no correct server holds.
func EncodeEncounterTimeline(timeline EncounterTimeline) ([]byte, error) {
	if timeline.EncounterID == 0 {
		return nil, fmt.Errorf("protocol: encounter has no id")
	}
	if timeline.BossEntityID == 0 {
		return nil, fmt.Errorf("protocol: encounter names no boss entity")
	}
	if _, known := vnet.EnumNamesMobKind[timeline.Boss]; !known || timeline.Boss == vnet.MobKindUnknown {
		return nil, fmt.Errorf("protocol: unknown encounter boss kind")
	}
	if timeline.Phase == 0 {
		return nil, fmt.Errorf("protocol: encounter has no phase")
	}
	if len(timeline.Moves) > MaxLiveMoves {
		return nil, fmt.Errorf("protocol: encounter announces %d moves", len(timeline.Moves))
	}
	seen := make(map[uint64]struct{}, len(timeline.Moves))
	for _, move := range timeline.Moves {
		if err := move.validate(); err != nil {
			return nil, err
		}
		if _, duplicate := seen[move.MoveInstanceID]; duplicate {
			return nil, fmt.Errorf("protocol: two moves share instance id %d", move.MoveInstanceID)
		}
		seen[move.MoveInstanceID] = struct{}{}
	}

	b := flatbuffers.NewBuilder(256)
	offsets := make([]flatbuffers.UOffsetT, len(timeline.Moves))
	for i, move := range timeline.Moves {
		offsets[i] = addEncounterMove(b, move)
	}
	vnet.EncounterTimelineStartMovesVector(b, len(offsets))
	for i := len(offsets) - 1; i >= 0; i-- {
		b.PrependUOffsetT(offsets[i])
	}
	moves := b.EndVector(len(offsets))

	vnet.EncounterTimelineStart(b)
	vnet.EncounterTimelineAddEncounterId(b, timeline.EncounterID)
	vnet.EncounterTimelineAddBossEntityId(b, timeline.BossEntityID)
	vnet.EncounterTimelineAddBoss(b, timeline.Boss)
	vnet.EncounterTimelineAddPhase(b, timeline.Phase)
	vnet.EncounterTimelineAddMoves(b, moves)
	return finishEnvelope(b, vnet.PayloadEncounterTimeline, vnet.EncounterTimelineEnd(b)), nil
}
