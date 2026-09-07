package game

import (
	"errors"
	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

type portalPartyVisit struct {
	ruin  InstanceRuin
	party uint64
}

// PortalEntry is an admitted visit, held until departure or connection teardown.
// Return is the authoritative standing position at acceptance, never the masonry
// arch itself. It also supplies a safe open-world persistence fallback.
type PortalEntry struct {
	Session   InstanceSession
	Character InstanceCharacter
	Return    [3]float32
	respawn   [3]float64
}

// EnterPortal resolves one authoritative request and decides what it earns. The
// manager serializes simultaneous party entry: the first member chooses a copy,
// subsequent members join that copy at this ruin. Unrelated parties and solo visits
// stay private. A party route outlives an empty visit only as long as the instance's
// grace.
//
// **Admission is one of four answers now, not the only one.** instance_entry.go states
// the rules; this function is where they are applied, under the one mutex that also
// performs the join, so nothing can bind, reset or be created between the decision and
// its consequence.
func (m *InstanceManager) EnterPortal(p *Player, request protocol.PortalRequest) PortalDecision {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.crossLocked(p, request, nil)
}

// crossLocked is the whole of a crossing: the reach and anchor checks every entry makes,
// the copy selection a party shares, and the entry rules that decide between admitting,
// offering and refusing.
//
// accepted is nil for a fresh PortalRequest and names the spent offer when this crossing
// is an acceptance. It is the one thing that turns case 2 from a prompt into an entry,
// and it is checked against what is true *now* rather than trusted: the run it named must
// still be the run behind this arch and must still be saved.
func (m *InstanceManager) crossLocked(p *Player, request protocol.PortalRequest, accepted *pendingOffer) PortalDecision {
	refuse := func(reason vnet.RefusalReason) PortalDecision {
		return PortalDecision{Outcome: PortalRefused, Reason: reason}
	}
	if p == nil {
		return refuse(vnet.RefusalReasonNotAtPortal)
	}
	p.sim.mu.Lock()
	valid := p.portalReachLocked(request)
	seed := p.sim.worldSeed
	character := InstanceCharacter{p.playerID, p.characterID}
	partyID := p.partyID
	respawn := p.spawn
	pos := [3]float32{float32(p.pos[0]), float32(p.pos[1]), float32(p.pos[2])}
	p.sim.mu.Unlock()
	if !valid {
		return refuse(vnet.RefusalReasonNotAtPortal)
	}
	// Range/reach precedes the seed lookup: an invented far-away arch costs no
	// terrain composition. RuinAt itself generates no chunks.
	ruin, exists := world.RuinAt(seed, world.RuinCellOf(int64(request.Arch[0])), world.RuinCellOf(int64(request.Arch[2])))
	if !exists || !samePortalAnchor(request, ruin.Arch) {
		return refuse(vnet.RefusalReasonNotAtPortal)
	}
	if m.closed {
		return refuse(vnet.RefusalReasonInstanceUnavailable)
	}
	if _, inside := m.inside[character]; inside {
		return refuse(vnet.RefusalReasonInstanceUnavailable)
	}
	site := InstanceRuin{ruin.CellX, ruin.CellZ}
	key := portalPartyVisit{site, partyID}
	id := m.visits[instanceVisit{site, character}]
	if partyID != 0 && m.partyVisits[key] != 0 {
		id = m.partyVisits[key]
	}
	selected := m.sessions[id]
	boundID, isBound := m.bound[instanceVisit{site, character}]

	switch {
	case accepted != nil:
		// An acceptance is honoured only for the exact situation its offer described: the
		// same ruin, the same run, and a run that is still saved. Anything else is the
		// one answer a stale, superseded and forged id all get.
		if selected == nil || site != accepted.ruin || selected.id != accepted.session || selected.state != InstanceSaved {
			return refuse(vnet.RefusalReasonEntryOfferUnknown)
		}
	case isBound && (selected == nil || selected.id != boundID):
		// Cases 3 and 4. The character owes this dungeon a run that is not the one behind
		// this arch — whether the arch leads to somebody else's saved run or to a copy a
		// party member started a moment ago is a fact about that other session, and this
		// answer is careful not to be the place it leaks from.
		return PortalDecision{Outcome: PortalMismatch, Reason: vnet.RefusalReasonSessionMismatch}
	case !isBound && selected != nil && selected.state == InstanceSaved:
		// Case 2. Nothing is joined, nothing is bound, and the character is still outside.
		return m.offerLocked(character, selected, site, request.Arch)
	}

	if selected == nil {
		var err error
		selected, err = m.createLocked(site)
		if err != nil {
			if errors.Is(err, ErrInstanceLimit) {
				return refuse(vnet.RefusalReasonInstanceLimit)
			}
			return refuse(vnet.RefusalReasonInstanceUnavailable)
		}
	}
	if err := m.joinLocked(selected, character); err != nil {
		return refuse(vnet.RefusalReasonInstanceUnavailable)
	}
	if partyID != 0 {
		m.partyVisits[key] = selected.id
	}
	// A crossing settles whatever was outstanding at this arch. An offer that is still
	// held here belongs to a situation this entry has just replaced.
	m.forgetOfferLocked(character)
	entry := PortalEntry{Session: selected.snapshot(), Character: character, Return: pos, respawn: respawn}
	m.portalEntries[character] = entry
	return PortalDecision{Outcome: PortalAdmitted, Entry: entry}
}

// offerLocked mints one offer for one character and states its terms.
//
// The id comes from the same monotonic source as every other identity this server hands
// out, so no two offers ever share one and none of them is guessable from a session id —
// which is the point, because an offer is the only thing a client may name and it must
// name nothing else.
//
// The total is taken as the larger of the registry's boss count and this run's own
// defeats. That guard is not decoration: schemas/player.fbs requires a recipient to reject
// a binding claiming more defeats than the dungeon holds, and a run restored from disk can
// carry a species this build no longer ranks as a boss. A number that is too large costs a
// player one misleading denominator; one that is too small costs them the frame.
func (m *InstanceManager) offerLocked(character InstanceCharacter, s *instanceSession, site InstanceRuin, arch [3]int32) PortalDecision {
	offer := pendingOffer{id: m.mintEntityID(), session: s.id, ruin: site, arch: arch}
	m.offers[character] = offer
	return PortalDecision{Outcome: PortalOffered, Offer: EntryOffer{
		ID:             offer.id,
		Ruin:           site,
		Arch:           arch,
		BossesDefeated: len(s.defeated),
		BossesTotal:    max(bossEncounterTotal(), len(s.defeated)),
		ExpiresUnix:    s.expiresUnix,
	}}
}

// AtPortal validates exit intent against the selected instance's own anchor.
func (p *Player) AtPortal(request protocol.PortalRequest, expected world.PlacedAnchor) bool {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	return samePortalAnchor(request, expected) && p.portalReachLocked(request)
}

func samePortalAnchor(request protocol.PortalRequest, expected world.PlacedAnchor) bool {
	return request.HasArch && int64(request.Arch[0]) == expected.X && int64(request.Arch[1]) == expected.Y && int64(request.Arch[2]) == expected.Z
}

func (p *Player) portalReachLocked(request protocol.PortalRequest) bool {
	if !request.HasArch || !p.sim.onlineLocked(p) || p.cannotActLocked() != nil {
		return false
	}
	// A mounted body cannot fit the chamber's standing slot.
	if _, err := p.mountedActionLocked(); err != nil {
		return false
	}
	anchor := [3]int64{int64(request.Arch[0]), int64(request.Arch[1]), int64(request.Arch[2])}
	for _, value := range anchor {
		if value < -world.BlockLimit || value > world.BlockLimit {
			return false
		}
	}
	return distanceToVoxel(p.box(), anchor) <= p.reachLocked()
}

// RestoreRespawn completes an exit after the live binding has returned. The arch
// is the arrival position; it must not replace the open world's original fallback
// respawn (tent and settlement selection still take precedence over that fallback).
func (entry PortalEntry) RestoreRespawn(p *Player) {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	if p.sim != entry.Session.Sim && p.playerID == entry.Character.PlayerID && p.characterID == entry.Character.CharacterID {
		p.spawn = entry.respawn
	}
}

// RestoreFallbackRespawn separates reconnect placement from the open-world death
// fallback when an ephemeral character has no stored Life to provide its position.
// Called by the authoritative session owner after admission, before streaming.
func (p *Player) RestoreFallbackRespawn(spawn [3]float32) {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	p.spawn = [3]float64{float64(spawn[0]), float64(spawn[1]), float64(spawn[2])}
}
