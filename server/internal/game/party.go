package game

import (
	"errors"
	"fmt"
	"math"
	"strings"
	"unicode"
	"unicode/utf8"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// maxPartyTargetBytes mirrors persist.MaxNameBytes. game may not import persist;
// party_test.go pins the shared number and both validators pin the same shape.
const maxPartyTargetBytes = 64

type party struct {
	leader  uint64
	members []uint64
}

type partyInvite struct {
	from        uint64
	expiresTick uint64
}

// Party applies one authoritative party intent. A non-zero refusal reason is safe
// for the session to answer on the wire; RefusalReasonUnknown marks malformed input
// that is logged and otherwise silent.
func (p *Player) Party(action vnet.PartyAction, targetName string) (vnet.RefusalReason, error) {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()

	switch action {
	case vnet.PartyActionInvite:
		return p.invitePlayerLocked(targetName)
	case vnet.PartyActionAccept:
		return p.acceptPartyInviteLocked()
	case vnet.PartyActionDecline:
		return p.declinePartyInviteLocked()
	case vnet.PartyActionLeave:
		if !p.sim.removeFromPartyLocked(p) {
			return vnet.RefusalReasonNoInvite, errors.New("the player is not in a party")
		}
		return vnet.RefusalReasonUnknown, nil
	case vnet.PartyActionKick:
		return p.kickPartyMemberLocked(targetName)
	default:
		return vnet.RefusalReasonUnknown, fmt.Errorf("party action %d is unknown", action)
	}
}

func (p *Player) invitePlayerLocked(targetName string) (vnet.RefusalReason, error) {
	if !p.sim.onlineLocked(p) || p.leaving {
		return vnet.RefusalReasonNoSuchPlayer, errors.New("the inviter is no longer online")
	}
	_, folded, err := acceptPartyTarget(targetName)
	if err != nil {
		return vnet.RefusalReasonUnknown, err
	}
	target, online := p.sim.byName[folded]
	if !online || !p.sim.onlineLocked(target) || target.leaving {
		return vnet.RefusalReasonNoSuchPlayer, errors.New("no online player has that name")
	}
	if target == p {
		return vnet.RefusalReasonAlreadyInParty, errors.New("a player cannot invite themselves")
	}
	if target.partyID != 0 {
		return vnet.RefusalReasonAlreadyInParty, errors.New("the target is already in a party")
	}
	if reason, err := p.canInviteLocked(); err != nil {
		return reason, err
	}

	target.invite = &partyInvite{
		from:        p.entityID,
		expiresTick: p.sim.currentTick + p.sim.partyInviteTicks,
	}
	frame := protocol.EncodePartyInvite(protocol.PartyInvite{
		FromEntityID: p.entityID,
		FromName:     p.name,
		ExpiresMS:    uint32(PartyInviteTTL.Milliseconds()),
	})
	if !target.deliver(frame) {
		p.sim.log.Debug("party invite dropped: the session's outbound queue is full",
			"entity_id", target.entityID, "from_entity_id", p.entityID)
	}
	return vnet.RefusalReasonUnknown, nil
}

func (p *Player) canInviteLocked() (vnet.RefusalReason, error) {
	if p.partyID == 0 {
		return vnet.RefusalReasonUnknown, nil
	}
	held := p.sim.parties[p.partyID]
	if held == nil {
		return vnet.RefusalReasonNoInvite, errors.New("the player's party no longer exists")
	}
	if held.leader != p.entityID {
		return vnet.RefusalReasonNotLeader, errors.New("only the party leader may invite")
	}
	if len(held.members) >= MaxPartySize {
		return vnet.RefusalReasonPartyFull, errors.New("the party is full")
	}
	return vnet.RefusalReasonUnknown, nil
}

func (p *Player) acceptPartyInviteLocked() (vnet.RefusalReason, error) {
	if !p.sim.onlineLocked(p) || p.leaving {
		p.invite = nil
		return vnet.RefusalReasonNoInvite, errors.New("the invited player is no longer available")
	}
	invitation := p.liveInviteLocked()
	if invitation == nil {
		return vnet.RefusalReasonNoInvite, errors.New("there is no live party invitation")
	}
	if p.partyID != 0 {
		return vnet.RefusalReasonAlreadyInParty, errors.New("the player is already in a party")
	}
	inviter := p.sim.players[invitation.from]
	if inviter == nil || !p.sim.onlineLocked(inviter) || inviter.leaving {
		p.invite = nil
		return vnet.RefusalReasonNoInvite, errors.New("the inviter is no longer available")
	}

	if inviter.partyID == 0 {
		partyID := p.sim.mintEntityID()
		held := &party{leader: inviter.entityID, members: []uint64{inviter.entityID, p.entityID}}
		p.sim.parties[partyID] = held
		inviter.partyID = partyID
		p.partyID = partyID
		p.invite = nil
		return vnet.RefusalReasonUnknown, nil
	}

	held := p.sim.parties[inviter.partyID]
	if held == nil || held.leader != inviter.entityID {
		p.invite = nil
		return vnet.RefusalReasonNoInvite, errors.New("the inviter may no longer add party members")
	}
	if len(held.members) >= MaxPartySize {
		return vnet.RefusalReasonPartyFull, errors.New("the party is full")
	}
	held.members = append(held.members, p.entityID)
	p.partyID = inviter.partyID
	p.invite = nil
	return vnet.RefusalReasonUnknown, nil
}

func (p *Player) declinePartyInviteLocked() (vnet.RefusalReason, error) {
	if p.liveInviteLocked() == nil {
		return vnet.RefusalReasonNoInvite, errors.New("there is no live party invitation")
	}
	p.invite = nil
	return vnet.RefusalReasonUnknown, nil
}

func (p *Player) liveInviteLocked() *partyInvite {
	if p.invite == nil {
		return nil
	}
	if p.sim.currentTick >= p.invite.expiresTick {
		p.invite = nil
		return nil
	}
	return p.invite
}

func (p *Player) kickPartyMemberLocked(targetName string) (vnet.RefusalReason, error) {
	_, folded, err := acceptPartyTarget(targetName)
	if err != nil {
		return vnet.RefusalReasonUnknown, err
	}
	held := p.sim.parties[p.partyID]
	if p.partyID == 0 || held == nil || held.leader != p.entityID {
		return vnet.RefusalReasonNotLeader, errors.New("only the party leader may kick")
	}
	target, online := p.sim.byName[folded]
	if !online || target == p || target.partyID != p.partyID {
		return vnet.RefusalReasonNoSuchPlayer, errors.New("that name is not another member of the party")
	}
	p.sim.removeFromPartyLocked(target)
	return vnet.RefusalReasonUnknown, nil
}

func (s *Sim) removeFromPartyLocked(p *Player) bool {
	if p == nil || p.partyID == 0 {
		return false
	}
	partyID := p.partyID
	held := s.parties[partyID]
	if held == nil {
		p.partyID = 0
		return false
	}

	for _, memberID := range held.members {
		if memberID == p.entityID {
			continue
		}
		if member := s.players[memberID]; member != nil {
			delete(p.described, memberID)
			delete(member.described, p.entityID)
		}
	}

	index := -1
	for i, memberID := range held.members {
		if memberID == p.entityID {
			index = i
			break
		}
	}
	if index < 0 {
		p.partyID = 0
		return false
	}
	held.members = append(held.members[:index], held.members[index+1:]...)
	p.partyID = 0

	if len(held.members) <= 1 {
		if len(held.members) == 1 {
			if remaining := s.players[held.members[0]]; remaining != nil {
				remaining.partyID = 0
			}
		}
		delete(s.parties, partyID)
		return true
	}
	if held.leader == p.entityID {
		held.leader = held.members[0]
	}
	return true
}

func (s *Sim) clearInvitesFromLocked(entityID uint64) {
	for _, player := range s.players {
		if player.invite != nil && player.invite.from == entityID {
			player.invite = nil
		}
	}
}

func (s *Sim) advancePartyInvitesLocked(tick uint64) {
	for _, player := range s.players {
		if player.invite != nil && tick >= player.invite.expiresTick {
			player.invite = nil
		}
	}
}

func (s *Sim) onlineLocked(p *Player) bool {
	return p != nil && s.players[p.entityID] == p
}

func (s *Sim) samePartyLocked(a, b *Player) bool {
	return a != nil && b != nil && a.partyID != 0 && a.partyID == b.partyID
}

// membersNearLocked returns the recipient set for one of this player's shared
// kill awards. The player who first hit the mob is first and always included;
// resolveSwingLocked has already proved they are still online. The tap owner remains
// included when dead; every other member is read from authoritative party state and
// must be alive and within radius of pos by Euclidean standing-position distance.
//
// The caller holds Sim.mu.
func (p *Player) membersNearLocked(pos [3]float64, radius float64) []*Player {
	members := []*Player{p}
	held := p.sim.parties[p.partyID]
	if p.partyID == 0 || held == nil || radius < 0 {
		return members
	}

	radiusSquared := radius * radius
	for _, entityID := range held.members {
		member := p.sim.players[entityID]
		if member == nil || member == p || !member.alive() {
			continue
		}
		var distanceSquared float64
		for axis := range 3 {
			delta := member.pos[axis] - pos[axis]
			distanceSquared += delta * delta
		}
		if distanceSquared <= radiusSquared && !math.IsNaN(distanceSquared) {
			members = append(members, member)
		}
	}
	return members
}

func (s *Sim) partySnapshotLocked(viewer *Player) (uint64, []protocol.PartyMemberState) {
	if viewer.partyID == 0 {
		return 0, nil
	}
	held := s.parties[viewer.partyID]
	if held == nil {
		return 0, nil
	}
	members := make([]protocol.PartyMemberState, 0, len(held.members)-1)
	for _, entityID := range held.members {
		if entityID == viewer.entityID {
			continue
		}
		member := s.players[entityID]
		if member == nil {
			continue
		}
		members = append(members, protocol.PartyMemberState{
			EntityID:  member.entityID,
			Pos:       toWire(member.pos),
			Health:    member.health,
			MaxHealth: member.maxHealthLocked(),
			Alive:     member.alive(),
		})
	}
	return held.leader, members
}

func acceptPartyTarget(name string) (accepted, folded string, err error) {
	accepted = strings.TrimSpace(name)
	switch {
	case accepted == "":
		return "", "", errors.New("a party target needs a name")
	case len(accepted) > maxPartyTargetBytes:
		return "", "", fmt.Errorf("%d bytes is longer than the %d a character name may be", len(accepted), maxPartyTargetBytes)
	case !utf8.ValidString(accepted):
		return "", "", errors.New("a party target has to be text")
	}
	for _, r := range accepted {
		if unicode.IsControl(r) {
			return "", "", errors.New("a party target may not contain control characters")
		}
	}
	return accepted, foldPlayerName(accepted), nil
}

func foldPlayerName(name string) string { return strings.ToLower(name) }
