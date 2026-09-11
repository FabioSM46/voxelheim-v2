package game

import (
	"slices"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// encounterPhaseFor is the stage a boss at this health is in, counted from one.
//
// **The threshold is crossed by the health, and the answer is a number rather than a
// name.** The registry says at which remaining-health percentages a species changes its
// repertoire; this counts how many of them have been passed. What a stage is *called*,
// what the boss does differently in it, and which moves it may announce are all decisions
// on the other side of this function — the wire carries the ordinal and nothing else, so
// that a client can follow a change of stage without being able to compute one.
//
// Integer arithmetic on both sides of the comparison, deliberately: `health * 100` against
// `maxHealth * percent` is the same threshold on every run, where a float division would
// put a boss on one side of 55% here and the other side of it in a test.
//
// maxHealth is the boss's own ceiling, the scale its pull gave it, so a stage begins at the
// same share of the fight for a party of four as for one player.
func encounterPhaseFor(def mobDefinition, maxHealth, health uint16) uint8 {
	phase := uint8(1)
	for _, percent := range def.phaseHealthPercents {
		if uint32(health)*100 <= uint32(maxHealth)*uint32(percent) {
			phase++
		}
	}
	return phase
}

// maxEncounterPhases bounds how many stages a registry row may describe.
//
// The wire's `phase` is a ubyte and would carry far more; this is a bound on the design
// rather than on the encoding. A fight a player is meant to *read* cannot have a stage
// they never notice, and eight is already generous against the three the approved
// encounter uses.
const maxEncounterPhases = 8

// startEncounterPhase is the stage a boss begins in, which is always the first one.
//
// A separate constant rather than a literal 1 at the two places that need it, because
// `EncodeEncounterTimeline` refuses a zero phase and the reason it refuses is that stages
// are counted from one. A zero here would be the absent field, not a stage.
const startEncounterPhase uint8 = 1

// advanceEncounterPhasesLocked moves every live encounter to the stage its boss's health
// has reached.
//
// **Monotonic, and that is a decision rather than an accident of where it is called.** A
// stage is a repertoire the encounter has unlocked, not a function of the current health:
// a boss healed back over a threshold does not forget the moves it has already shown, and
// a client told to go back a stage would have to un-draw a fight it had already seen
// escalate. So the phase only ever rises.
//
// Runs after everything that could have moved a boss's health and before the timelines
// are built, so the stage published this tick is the one this tick's damage produced.
// The caller holds Sim.mu.
func (s *Sim) advanceEncounterPhasesLocked() {
	for _, m := range s.mobs {
		if m.encounter == nil {
			continue
		}
		if phase := encounterPhaseFor(m.species(), m.maxHealth(), m.health); phase > m.encounter.phase {
			m.encounter.phase = phase
		}
	}
}

// sweepEndedEncounterMovesLocked drops the announcements that have been published as
// ended.
//
// **An ending is published exactly once**, and that is what this pass is for: a move
// marked ended is still in the list when the tick's timelines are built, so a client is
// told *why* it stopped rather than merely seeing it disappear, and then it is gone.
//
// **It runs after every viewer has been offered the tick's bundle, and the position is
// the whole of the guarantee.** At the top of the next tick instead, an ending written
// between two ticks — by a reset, or by anything outside Step — would be swept before it
// was ever published, and the difference between "ended" and "vanished" would depend on
// where in a tick the ending happened. Here it does not. It is `miningCompleted = nil`'s
// rule, at `miningCompleted = nil`'s place in the tick, for `miningCompleted = nil`'s
// reason: offered once to whoever could receive it, and then cleared for everybody.
//
// The caller holds Sim.mu.
func (s *Sim) sweepEndedEncounterMovesLocked() {
	for _, m := range s.mobs {
		if m.encounter == nil || len(m.encounter.moves) == 0 {
			continue
		}
		m.encounter.moves = slices.DeleteFunc(m.encounter.moves, func(one protocol.EncounterMove) bool {
			return one.Ended != vnet.MoveEndUnknown
		})
	}
}

// withdrawEncounterMovesLocked cancels every announcement this encounter still has
// running.
//
// `Cancelled` and not `Completed`: the contract distinguishes a move that ran to its end
// from one the encounter took away, and everything that reaches here — a creature leaving
// the world, a run being reset — is the second. A move already carrying an ending keeps
// the one it has, because the reason it stopped is not improved by a later one.
//
// **What this cannot promise is that anybody sees it.** A boss that leaves the world in
// the same tick is gone from the next snapshot, and the contract's own rule covers that
// case: a move that disappears has ended, and a receiver must stop drawing its hazards
// either way. This makes the encounter's own state truthful at the moment it ends, which
// is what a caller that publishes from it is entitled to assume.
//
// The caller holds Sim.mu.
func withdrawEncounterMovesLocked(m *mob) {
	if m == nil || m.encounter == nil {
		return
	}
	for i := range m.encounter.moves {
		if m.encounter.moves[i].Ended == vnet.MoveEndUnknown {
			m.encounter.moves[i].Ended = vnet.MoveEndCancelled
			// A cancelled move endangers nobody, and the region goes with the ending: a
			// receiver reading an instance's last frame must not be handed a shape it
			// could still treat as dangerous.
			m.encounter.moves[i].Hazards = nil
		}
	}
	// **The execution stops here, not merely the announcement.** Leaving the scheduler's
	// own instance in place would let a release window that has been publicly cancelled go
	// on resolving damage against a region no client is drawing any more — which is
	// exactly the shape of defect the announcement exists to rule out.
	m.encounter.running = nil
}

// encounterFramesLocked is one timeline per live boss encounter this snapshot can see.
//
// **Visibility is a projection of the finished snapshot, never a second interest test.**
// The rule is `blowFramesLocked`'s and `miningFramesLocked`'s: a recipient is told about
// a boss exactly when that boss is in the mobs vector it has just been sent, so a timeline
// can never describe a creature the recipient was not told exists. One test, one answer,
// and no way for the two to drift apart.
//
// **A dead boss publishes nothing.** A killed creature leaves Sim.mobs on the blow that
// empties its health and becomes a corpse, so its id can still occur in the snapshot's
// mobs vector while the lookup here finds nothing — and that is the right answer: a corpse
// is not announcing anything. The client reads the disappearance as the ending of every
// move the encounter had, which is what the contract says disappearance means.
//
// An encounter is created by the pull rather than by the placement, so a boss standing in
// its arena with nobody in the room has no encounter and no timeline. That silence is not
// the same as an empty `Moves`: the empty list says "this boss is announcing nothing right
// now", and sending it before anybody has pulled would be describing a fight that has not
// started.
//
// The caller holds Sim.mu.
func (s *Sim) encounterFramesLocked(snapshot protocol.EntitySnapshot) [][]byte {
	var frames [][]byte
	for _, state := range snapshot.Mobs {
		m := s.mobs[state.EntityID]
		if m == nil || m.encounter == nil {
			continue
		}
		frame, err := protocol.EncodeEncounterTimeline(protocol.EncounterTimeline{
			EncounterID:  m.encounter.id,
			BossEntityID: m.entityID,
			Boss:         m.kind,
			Phase:        m.encounter.phase,
			Moves:        m.encounter.moves,
		})
		if err != nil {
			// Logged and dropped rather than retried. The encoder refuses exactly what a
			// decoder would end the session over, so a refusal here is this server having
			// built something no client may read — a defect to find in a log, never a
			// frame to send anyway.
			s.log.Error("invalid encounter timeline", "error", err, "entity_id", m.entityID)
			continue
		}
		frames = append(frames, frame)
	}
	return frames
}
