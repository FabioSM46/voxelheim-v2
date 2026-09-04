package game

import (
	"slices"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// Proximity voice, and the one sentence this file exists to make true: **a client hears
// a voice because this server sent it, and for no other reason.**
//
// A VoiceFrame carries a sequence, an audience and Opus. It carries no position, because
// the speaker's position is the server's own, and no recipients, because choosing them is
// the decision being protected. The relay answers one question per frame — who can hear
// this — from state it computed itself.
//
// **Nothing here reads the Opus.** The bytes are copied out by the decoder and handed to
// the chosen sessions; they are never parsed, persisted, quoted in an error or logged.
// Every diagnostic below carries counts and identities, and voice_test.go captures the
// logger at Debug during an exchange to keep it that way.

// newVoiceLimiter is the per-speaker frame allowance. See [tokenBucket], which chat
// shares: only the two numbers differ.
func newVoiceLimiter(now time.Time) *tokenBucket {
	return &tokenBucket{tokens: VoiceBurst, burst: VoiceBurst, refillPerSecond: VoiceRefillPerSecond, last: now}
}

func (s *Sim) pruneVoiceLimitersLocked(now time.Time) {
	for playerID, limiter := range s.voiceLimiters {
		if limiter.fullAt(now) {
			delete(s.voiceLimiters, playerID)
		}
	}
}

// spendVoiceTokenLocked is the single cadence gate for relayed voice, keyed by the
// identity that survives a connection so a reconnect resumes the same allowance.
//
// The caller holds Sim.mu.
func (p *Player) spendVoiceTokenLocked() bool {
	now := p.sim.voiceNow()
	p.sim.pruneVoiceLimitersLocked(now)
	limiter := p.sim.voiceLimiters[p.playerID]
	if limiter == nil {
		limiter = newVoiceLimiter(now)
		p.sim.voiceLimiters[p.playerID] = limiter
	}
	return limiter.allow(now)
}

// voiceEye is where a voice comes from and where it is heard: the standing position
// raised to [ProjectileEyeHeight].
//
// **The same height on both bodies, so today it cancels in the subtraction** — written
// this way because that identity holds only while every player is the same shape. A
// mounted rider already occupies a taller body ([MountedHeight]), and the moment an eye
// height stops being one constant the honest measurement is the one already here.
//
// The caller holds Sim.mu.
func (p *Player) voiceEye() [3]float64 {
	return [3]float64{p.pos[0], p.pos[1] + ProjectileEyeHeight, p.pos[2]}
}

// advanceVoiceSetsLocked recomputes every speaker's audible set, every
// [VoiceSetInterval] ticks, from the positions the tick has just produced.
//
// **This is the cost the feature commits to, and the reason it is not per frame.** Each
// pass is O(players) per speaker, so a hundred players cost ten thousand squared-distance
// comparisons five times a second and a relayed frame costs only one audible set. A
// spatial index would make the pass cheaper and the simulation harder to reason about,
// and this feature is not the one that should introduce it.
//
// Membership is hysteretic: a listener enters at the range and leaves only at the range
// widened by [VoiceExitFactor]. Each comparison is therefore against the set as it stands
// at the start of the pass, which is what makes the state on the speaker load-bearing
// rather than a cache — recomputing from positions alone would give a different answer.
//
// The caller holds Sim.mu.
func (s *Sim) advanceVoiceSetsLocked(tick uint64, players []*Player) {
	if s.voiceRange <= 0 || tick%VoiceSetInterval != 0 {
		return
	}

	enter := s.voiceRange * s.voiceRange
	exitRange := s.voiceRange * VoiceExitFactor
	exit := exitRange * exitRange

	for _, speaker := range players {
		origin := speaker.voiceEye()
		for _, listener := range players {
			if listener == speaker {
				continue
			}
			// A listener already in the set keeps the wider limit; one outside has to
			// reach the narrower one. That asymmetry is the whole of the hysteresis.
			limit := enter
			if _, heard := speaker.audible[listener.entityID]; heard {
				limit = exit
			}
			if squaredDistance(origin, listener.voiceEye()) <= limit {
				speaker.audible[listener.entityID] = struct{}{}
				continue
			}
			delete(speaker.audible, listener.entityID)
		}
	}
}

// squaredDistance is the comparison every range check here uses. Squared rather than
// rooted because a distance is only ever compared against another distance in this file.
func squaredDistance(a, b [3]float64) float64 {
	dx, dy, dz := a[0]-b[0], a[1]-b[1], a[2]-b[2]
	return dx*dx + dy*dy + dz*dz
}

// forgetVoiceListenerLocked removes one departed entity from every speaker's audible set.
// Called from Leave, for the reason a fully refilled chat bucket is deleted there: an id
// nobody holds would be looked up on every frame for as long as the speaker kept talking.
//
// The caller holds Sim.mu.
func (s *Sim) forgetVoiceListenerLocked(entityID uint64) {
	for _, speaker := range s.players {
		delete(speaker.audible, entityID)
	}
}

// Voice relays one Opus frame to the players this speaker is currently audible to, and
// reports how many sessions took it and how many refused it.
//
// **Every refusal is silent on the wire.** Chat's cadence limit is answered because a
// person can wait and press the key again; a dropped voice frame has no answer worth a
// round trip, because the next frame is already on its way. So the counts come back for
// the caller's accounting and the refusals are Debug lines carrying numbers.
//
// The refusals, and why they are in this order:
//
//   - A server with no voice range relays nothing and asks nothing else.
//   - A leaving body is inert. It has stopped acting in every other way — see
//     BeginLeaving — and a voice is an action.
//   - The bucket is spent next, before the frame is examined, because it bounds how often
//     one speaker may *ask*. Paying only for well-formed frames would let a flood of
//     oversized ones cost nothing, which is the opposite of what a limiter is for.
//   - Opus past the contract's bound is dropped here rather than refused by the decoder,
//     which is the division of labour [protocol.MaxVoiceOpusBytes] states: a readable
//     request for something this server will not do has an answer, and closing the
//     connection is not it.
//   - An audience this server cannot name is dropped rather than widened to Everyone.
//     Guessing at a filter is how a party conversation reaches a stranger.
//
// **A dead speaker is deliberately not refused.** Death is a three-second wait, and being
// unable to say anything during it is a worse rule than any it would enforce; a corpse's
// voice reaches nobody who could not already hear the living one, because it is the same
// audible set either way.
func (p *Player) Voice(frame protocol.VoiceFrame) (delivered, dropped int) {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()

	switch {
	case p.sim.voiceRange <= 0:
		p.sim.log.Debug("voice frame dropped: this server relays no voice",
			"entity_id", p.entityID, "opus_bytes", len(frame.Opus))
		return 0, 0
	case p.leaving:
		p.sim.log.Debug("voice frame dropped: the speaker is leaving",
			"entity_id", p.entityID, "opus_bytes", len(frame.Opus))
		return 0, 0
	case !p.spendVoiceTokenLocked():
		p.sim.log.Debug("voice frame dropped: the speaker is over the frame rate",
			"entity_id", p.entityID, "opus_bytes", len(frame.Opus), "burst", VoiceBurst)
		return 0, 0
	case len(frame.Opus) > protocol.MaxVoiceOpusBytes:
		p.sim.log.Debug("voice frame dropped: the payload is longer than the contract allows",
			"entity_id", p.entityID, "opus_bytes", len(frame.Opus), "limit_bytes", protocol.MaxVoiceOpusBytes)
		return 0, 0
	}
	partyOnly, known := voiceAudience(frame.Audience)
	if !known {
		// The value is a number a client sent, and here it is the whole diagnostic.
		p.sim.log.Debug("voice frame dropped: the audience is not one this server can apply",
			"entity_id", p.entityID, "audience", uint8(frame.Audience))
		return 0, 0
	}

	listeners := p.audibleListenersLocked()
	if len(listeners) == 0 {
		return 0, 0
	}

	// Encoded once for the whole set: every recipient is told the same thing.
	heard := protocol.EncodeVoiceHeard(protocol.VoiceHeard{
		SpeakerEntityID: p.entityID,
		Sequence:        frame.Sequence,
		Opus:            frame.Opus,
	})
	for _, listener := range listeners {
		if partyOnly && !p.sim.samePartyLocked(p, listener) {
			// A speaker in no party has partyID 0, which samePartyLocked answers false
			// for against everybody. "Party" on a soloist is a filter that matches
			// nobody, which is the honest reading of the request.
			continue
		}
		// The latency lane rather than the ordinary one, and the drop that follows is
		// what makes it safe: a voice frame is superseded by the next one 20 ms later,
		// so a listener whose lane is full loses this frame instead of delaying every
		// other listener's. See Player.deliverVoice.
		if listener.deliverVoice(heard) {
			delivered++
			continue
		}
		dropped++
		p.sim.log.Debug("voice frame dropped: the session's latency lane is full",
			"speaker_entity_id", p.entityID, "entity_id", listener.entityID)
	}
	return delivered, dropped
}

// voiceAudience maps the wire value onto the one bit the relay acts on, and says whether
// it recognised it at all. One switch, so adding an enum member is a compile-time visit
// to a single place.
func voiceAudience(audience vnet.VoiceAudience) (partyOnly, known bool) {
	switch audience {
	case vnet.VoiceAudienceEveryone:
		return false, true
	case vnet.VoiceAudienceParty:
		return true, true
	default:
		return false, false
	}
}

// audibleListenersLocked is this speaker's audible set as live players, in entity order.
//
// **Proportional to the set, not to the population**, which is the promise the four-tick
// recompute exists to keep: nothing here walks s.players. The sort is for the reason
// every other fan-out in this package is ordered.
//
// An id no player answers to belongs to a session that left between two recomputes. Leave
// is what makes that rare; dropping it here is what makes it impossible to accumulate.
//
// The caller holds Sim.mu.
func (p *Player) audibleListenersLocked() []*Player {
	if len(p.audible) == 0 {
		return nil
	}
	ids := make([]uint64, 0, len(p.audible))
	for id := range p.audible {
		ids = append(ids, id)
	}
	slices.SortFunc(ids, compareEntityIDs)

	listeners := make([]*Player, 0, len(ids))
	for _, id := range ids {
		listener := p.sim.players[id]
		if listener == nil {
			delete(p.audible, id)
			continue
		}
		listeners = append(listeners, listener)
	}
	return listeners
}
