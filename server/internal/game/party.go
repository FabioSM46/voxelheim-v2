package game

import (
	"errors"
	"fmt"
	"math"
	"strings"
	"unicode"
	"unicode/utf8"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// maxPartyTargetBytes mirrors persist.MaxNameBytes. game may not import persist;
// party_test.go pins the shared number and both validators pin the same shape.
const maxPartyTargetBytes = 64

type party struct {
	leader     partyMemberKey
	members    []partyMember
	lootCursor partyMemberKey
}

// partyMemberKey names one character rather than one connection. The account keeps
// two owners from ever colliding, the persisted character id survives reconnects,
// and the folded name pins the same character boundary mob-tap progression uses.
type partyMemberKey struct {
	playerID    identity.PlayerID
	characterID uint64
	foldedName  string
}

type partyMember struct {
	key              partyMemberKey
	name             string
	player           *Player
	offlineUntilTick uint64
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
	if !p.sim.onlineLocked(p) {
		return vnet.RefusalReasonNoSuchPlayer, errors.New("the requesting player is no longer online")
	}

	switch action {
	case vnet.PartyActionInvite:
		return p.invitePlayerLocked(targetName)
	case vnet.PartyActionAccept:
		return p.acceptPartyInviteLocked()
	case vnet.PartyActionDecline:
		return p.declinePartyInviteLocked()
	case vnet.PartyActionLeave:
		if !p.sim.removePartyMemberLocked(p.partyID, p.partyMemberKey()) {
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
	if held.leader != p.partyMemberKey() {
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
		inviterMember := inviter.partyMember()
		invitedMember := p.partyMember()
		held := &party{
			leader: inviterMember.key, members: []partyMember{inviterMember, invitedMember},
			lootCursor: inviterMember.key,
		}
		p.sim.parties[partyID] = held
		p.sim.partyMemberships[inviterMember.key] = partyID
		p.sim.partyMemberships[invitedMember.key] = partyID
		inviter.partyID = partyID
		p.partyID = partyID
		p.invite = nil
		return vnet.RefusalReasonUnknown, nil
	}

	held := p.sim.parties[inviter.partyID]
	if held == nil || held.leader != inviter.partyMemberKey() {
		p.invite = nil
		return vnet.RefusalReasonNoInvite, errors.New("the inviter may no longer add party members")
	}
	if len(held.members) >= MaxPartySize {
		return vnet.RefusalReasonPartyFull, errors.New("the party is full")
	}
	member := p.partyMember()
	held.members = append(held.members, member)
	p.sim.partyMemberships[member.key] = inviter.partyID
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
	if p.partyID == 0 || held == nil || held.leader != p.partyMemberKey() {
		return vnet.RefusalReasonNotLeader, errors.New("only the party leader may kick")
	}
	var target *partyMember
	for index := range held.members {
		candidate := &held.members[index]
		if candidate.key.foldedName == folded && candidate.key != p.partyMemberKey() {
			target = candidate
			break
		}
	}
	if target == nil {
		return vnet.RefusalReasonNoSuchPlayer, errors.New("that name is not another member of the party")
	}
	p.sim.removePartyMemberLocked(p.partyID, target.key)
	return vnet.RefusalReasonUnknown, nil
}

func (s *Sim) removePartyMemberLocked(partyID uint64, key partyMemberKey) bool {
	if partyID == 0 {
		return false
	}
	held := s.parties[partyID]
	if held == nil {
		delete(s.partyMemberships, key)
		return false
	}

	index := -1
	for i, member := range held.members {
		if member.key == key {
			index = i
			break
		}
	}
	if index < 0 {
		delete(s.partyMemberships, key)
		return false
	}
	removed := held.members[index]
	// The cursor names the next roster member to inspect. If that member leaves,
	// preserve its meaning by moving it to the member that followed them in the old
	// cyclic order before the slice is compacted.
	if held.lootCursor == key && len(held.members) > 1 {
		held.lootCursor = held.members[(index+1)%len(held.members)].key
	}
	if removed.player != nil && s.onlineLocked(removed.player) {
		for memberIndex := range held.members {
			member := held.members[memberIndex].player
			if member == nil || member == removed.player || !s.onlineLocked(member) {
				continue
			}
			delete(removed.player.described, member.entityID)
			delete(member.described, removed.player.entityID)
		}
		removed.player.partyID = 0
	}
	held.members = append(held.members[:index], held.members[index+1:]...)
	delete(s.partyMemberships, key)

	if len(held.members) <= 1 {
		if len(held.members) == 1 {
			remaining := held.members[0]
			delete(s.partyMemberships, remaining.key)
			if remaining.player != nil && s.onlineLocked(remaining.player) {
				remaining.player.partyID = 0
			}
		}
		delete(s.parties, partyID)
		return true
	}
	if held.leader == key {
		held.leader = held.members[0].key
	}
	if held.lootCursor == (partyMemberKey{}) {
		held.lootCursor = held.leader
	}
	return true
}

// markPartyMemberOfflineLocked detaches exactly this session object while retaining
// its stable roster slot. A late teardown from an older entity cannot detach a newer
// reconnect because the live pointer has to match.
func (s *Sim) markPartyMemberOfflineLocked(p *Player) {
	if p == nil || p.partyID == 0 {
		return
	}
	held := s.parties[p.partyID]
	if held == nil {
		p.partyID = 0
		return
	}
	key := p.partyMemberKey()
	for index := range held.members {
		member := &held.members[index]
		if member.key == key && member.player == p {
			member.player = nil
			member.offlineUntilTick = s.currentTick + s.partyOfflineTicks
			p.partyID = 0
			return
		}
	}
}

func (s *Sim) rebindPartyMemberLocked(p *Player) {
	key := p.partyMemberKey()
	partyID := s.partyMemberships[key]
	held := s.parties[partyID]
	if partyID == 0 || held == nil {
		return
	}
	for index := range held.members {
		member := &held.members[index]
		if member.key == key {
			member.player = p
			member.offlineUntilTick = 0
			p.partyID = partyID
			return
		}
	}
}

func (p *Player) partyMemberKey() partyMemberKey {
	return partyMemberKey{playerID: p.playerID, characterID: p.characterID, foldedName: foldPlayerName(p.name)}
}

func (p *Player) partyMember() partyMember {
	return partyMember{key: p.partyMemberKey(), name: p.name, player: p}
}

// startBossEncounterLocked freezes the first valid pull's eligibility exactly once.
// Target acquisition and valid damage both call it; whichever arrives first wins.
//
// The copied roster deliberately includes dead and offline persistent members. Party
// mutations after this point edit a different slice and cannot change the encounter.
// The caller holds Sim.mu.
func (s *Sim) startBossEncounterLocked(m *mob, first *Player) {
	if m == nil || first == nil || m.encounter != nil || !m.species().isBoss() {
		return
	}

	roster := []corpseOwner{first.corpseOwner()}
	held := s.parties[first.partyID]
	if first.partyID != 0 && held != nil && len(held.members) > 0 {
		roster = make([]corpseOwner, 0, len(held.members))
		for _, member := range held.members {
			roster = append(roster, corpseOwnerFromPartyKey(member.key))
		}
	}
	m.encounter = &bossEncounter{roster: roster}
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

	// Collect first because removal may compact a roster or dissolve its party.
	// Every deadline is compared at the same authoritative tick, so members that
	// disconnected together expire together regardless of map iteration order.
	type expiredMember struct {
		partyID uint64
		key     partyMemberKey
	}
	var expired []expiredMember
	for partyID, held := range s.parties {
		for _, member := range held.members {
			if member.player == nil && member.offlineUntilTick != 0 && tick >= member.offlineUntilTick {
				expired = append(expired, expiredMember{partyID: partyID, key: member.key})
			}
		}
	}
	for _, member := range expired {
		s.removePartyMemberLocked(member.partyID, member.key)
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
// resolveAttackLocked has already proved they are still online. The tap owner remains
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
	for index := range held.members {
		member := held.members[index].player
		if member == nil || !p.sim.onlineLocked(member) || member == p || !member.alive() {
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

func (s *Sim) partySnapshotLocked(viewer *Player) (uint64, []protocol.PartyMemberState, []protocol.PartyRosterMember) {
	if viewer.partyID == 0 {
		return 0, nil, nil
	}
	held := s.parties[viewer.partyID]
	if held == nil {
		return 0, nil, nil
	}
	members := make([]protocol.PartyMemberState, 0, len(held.members)-1)
	roster := make([]protocol.PartyRosterMember, 0, len(held.members))
	leaderEntityID := uint64(0)
	for index := range held.members {
		member := &held.members[index]
		live := member.player
		online := live != nil && s.onlineLocked(live)
		entityID := uint64(0)
		if online {
			entityID = live.entityID
		}
		roster = append(roster, protocol.PartyRosterMember{
			CharacterID: member.key.characterID,
			EntityID:    entityID,
			Name:        member.name,
			Online:      online,
		})
		if member.key == held.leader {
			leaderEntityID = entityID
		}
		if !online || live == viewer {
			continue
		}
		members = append(members, protocol.PartyMemberState{
			EntityID:  live.entityID,
			Pos:       toWire(live.pos),
			Health:    live.health,
			MaxHealth: live.maxHealthLocked(),
			Alive:     live.alive(),
		})
	}
	return leaderEntityID, members, roster
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
