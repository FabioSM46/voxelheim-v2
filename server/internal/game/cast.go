package game

import (
	"errors"
	"fmt"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// castInterruption is every way an authoritative cast may be stopped before it
// completes. The enum and the table below are the one list: intent, damage and death
// call the same transition, and adding another caster changes none of them.
type castInterruption uint8

const (
	castInterruptedByDamage castInterruption = iota
	castInterruptedByMovement
	castInterruptedByJump
	castInterruptedByDeath
	castInterruptionCount
)

var castInterruptionReasons = [castInterruptionCount]vnet.RefusalReason{
	castInterruptedByDamage:   vnet.RefusalReasonCastInterruptedByDamage,
	castInterruptedByMovement: vnet.RefusalReasonCastInterruptedByMovement,
	castInterruptedByJump:     vnet.RefusalReasonCastInterruptedByJump,
	castInterruptedByDeath:    vnet.RefusalReasonCastInterruptedByDeath,
}

// activeCast is one server-owned timed action. Every field is guarded by Sim.mu.
//
// action is what an interruption refuses; retaining it here is what keeps the
// cancellation machinery independent of the caster. complete runs under Sim.mu on
// the authoritative completion tick and therefore must neither block nor take that
// lock again. That is the same critical section in which every gameplay transition
// made by a tick already runs.
type activeCast struct {
	kind     vnet.CastKind
	action   vnet.RefusedAction
	elapsed  uint32
	duration uint32
	complete func()
}

// startCastLocked begins the one cast this player may hold. It is deliberately
// package-private: the first production caster arrives in the mount issue, while the
// primitive can be tested here without inventing a second action merely to exercise it.
//
// The caller holds Sim.mu. kind, action and complete are values supplied by trusted
// game code, but they are still checked here so a future caller cannot create a cast
// the wire cannot describe or one that completes into silence.
func (p *Player) startCastLocked(kind vnet.CastKind, action vnet.RefusedAction, complete func()) (vnet.RefusalReason, error) {
	if p.cast != nil {
		return vnet.RefusalReasonCastAlreadyInProgress, errors.New("a cast is already in progress")
	}
	if kind == vnet.CastKindUnknown {
		return vnet.RefusalReasonUnknown, errors.New("cast kind Unknown is reserved")
	}
	if _, known := vnet.EnumNamesCastKind[kind]; !known {
		return vnet.RefusalReasonUnknown, fmt.Errorf("cast kind %d is not in the protocol", kind)
	}
	if action == vnet.RefusedActionUnknown {
		return vnet.RefusalReasonUnknown, errors.New("a cast must name the action its interruption refuses")
	}
	if _, known := vnet.EnumNamesRefusedAction[action]; !known {
		return vnet.RefusalReasonUnknown, fmt.Errorf("refused action %d is not in the protocol", action)
	}
	if complete == nil {
		return vnet.RefusalReasonUnknown, errors.New("a cast must have an authoritative completion")
	}

	p.cast = &activeCast{
		kind:     kind,
		action:   action,
		duration: p.sim.castTicks,
		complete: complete,
	}
	return vnet.RefusalReasonUnknown, nil
}

// advanceCastLocked advances one cast by one authoritative tick and completes it at
// the server-derived duration. Intent is checked first so a cast started after a held
// movement input cannot advance for one frame merely because no new PlayerInput was
// needed to repeat that intent.
//
// The caller holds Sim.mu.
func (p *Player) advanceCastLocked() {
	if p.cast == nil {
		return
	}
	if p.interruptCastForIntentLocked(p.current) {
		return
	}

	p.cast.elapsed++
	if p.cast.elapsed < p.cast.duration {
		return
	}

	complete := p.cast.complete
	p.cast = nil
	complete()
}

// interruptCastForIntentLocked reads the accepted controls, never displacement.
// Horizontal movement and jump are requests the player made; yaw and pitch are camera
// direction and are deliberately absent. Water, a current or a waterfall may move the
// body while this intent remains zero, so none can reach this cancellation path.
//
// Movement wins when one input carries movement and jump together. There is one cast
// to cancel and one refusal to send; the first actionable control in the contract is
// the stable answer rather than two events for one transition.
func (p *Player) interruptCastForIntentLocked(in intent) bool {
	switch {
	case in.moveX != 0 || in.moveZ != 0:
		return p.interruptCastLocked(castInterruptedByMovement)
	case in.jump:
		return p.interruptCastLocked(castInterruptedByJump)
	default:
		return false
	}
}

// interruptCastLocked performs every cancellation. The caller says only what
// interrupted it; the active cast supplies which action is refused, so adding a caster
// never adds a case to this function or to castInterruptionReasons.
//
// The caller holds Sim.mu.
func (p *Player) interruptCastLocked(interruption castInterruption) bool {
	if p.cast == nil {
		return false
	}
	if interruption >= castInterruptionCount {
		panic(fmt.Sprintf("game: unknown cast interruption %d", interruption))
	}

	action := p.cast.action
	p.cast = nil
	p.queueCastRefusalLocked(protocol.ActionRefused{
		Action: action,
		Reason: castInterruptionReasons[interruption],
	})
	return true
}

// queueCastRefusalLocked attempts the event immediately, so an interruption produced
// later in a tick normally precedes the snapshot in which the cast disappears. A full
// session queue retains the immutable frame for retry: silence is not an answer, and a
// later snapshot cannot supersede why the bar stopped.
func (p *Player) queueCastRefusalLocked(refusal protocol.ActionRefused) {
	frame := protocol.EncodeActionRefused(refusal)
	if len(p.pendingCastRefusals) == 0 && p.deliver(frame) {
		return
	}
	p.pendingCastRefusals = append(p.pendingCastRefusals, frame)
}

// offerCastRefusalsLocked retries owed interruption events in order until the
// non-blocking session seam reports full again. The caller holds Sim.mu.
func (p *Player) offerCastRefusalsLocked() {
	for len(p.pendingCastRefusals) > 0 {
		if !p.deliver(p.pendingCastRefusals[0]) {
			return
		}
		p.pendingCastRefusals[0] = nil
		p.pendingCastRefusals = p.pendingCastRefusals[1:]
	}
}

// castStateLocked projects the running cast into the recipient's superseding
// snapshot. A completed cast is removed before snapshot construction, so elapsed is
// always strictly below duration and the progress value can never be the contract's
// excluded 255.
func (p *Player) castStateLocked() (protocol.CastState, bool) {
	if p.cast == nil {
		return protocol.CastState{}, false
	}
	progress := uint8(uint64(p.cast.elapsed) * 255 / uint64(p.cast.duration))
	return protocol.CastState{Kind: p.cast.kind, Progress: progress}, true
}
