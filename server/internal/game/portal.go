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

// EnterPortal resolves and admits one authoritative request. The manager serializes
// simultaneous party entry: the first member chooses a copy, subsequent members
// join that copy at this ruin. Unrelated parties and solo visits stay private.
// A party route outlives an empty visit only as long as the instance's grace.
func (m *InstanceManager) EnterPortal(p *Player, request protocol.PortalRequest) (PortalEntry, vnet.RefusalReason) {
	m.mu.Lock()
	defer m.mu.Unlock()
	if p == nil {
		return PortalEntry{}, vnet.RefusalReasonNotAtPortal
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
		return PortalEntry{}, vnet.RefusalReasonNotAtPortal
	}
	// Range/reach precedes the seed lookup: an invented far-away arch costs no
	// terrain composition. RuinAt itself generates no chunks.
	ruin, exists := world.RuinAt(seed, world.RuinCellOf(int64(request.Arch[0])), world.RuinCellOf(int64(request.Arch[2])))
	if !exists || !samePortalAnchor(request, ruin.Arch) {
		return PortalEntry{}, vnet.RefusalReasonNotAtPortal
	}
	if m.closed {
		return PortalEntry{}, vnet.RefusalReasonInstanceUnavailable
	}
	if _, inside := m.inside[character]; inside {
		return PortalEntry{}, vnet.RefusalReasonInstanceUnavailable
	}
	site := InstanceRuin{ruin.CellX, ruin.CellZ}
	key := portalPartyVisit{site, partyID}
	id := m.visits[instanceVisit{site, character}]
	if partyID != 0 && m.partyVisits[key] != 0 {
		id = m.partyVisits[key]
	}
	selected := m.sessions[id]
	if selected == nil {
		var err error
		selected, err = m.createLocked(site)
		if err != nil {
			if errors.Is(err, ErrInstanceLimit) {
				return PortalEntry{}, vnet.RefusalReasonInstanceLimit
			}
			return PortalEntry{}, vnet.RefusalReasonInstanceUnavailable
		}
	}
	if err := m.joinLocked(selected, character); err != nil {
		return PortalEntry{}, vnet.RefusalReasonInstanceUnavailable
	}
	if partyID != 0 {
		m.partyVisits[key] = selected.id
	}
	return PortalEntry{Session: selected.snapshot(), Character: character, Return: pos, respawn: respawn}, vnet.RefusalReasonUnknown
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
