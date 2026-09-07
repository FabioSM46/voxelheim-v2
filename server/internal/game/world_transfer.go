package game

import (
	"errors"
	"math"
	"sync"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// WorldGroup keeps parties independent of place. Worlds in a group share one
// simulation lock: a roster contains live players, so updating it and reading a
// member's state must have the same lock even when that member moves away.
// World entity indexes, terrain and every world broadcast remain separate.
// The instance manager acquires its own lock before this lock, never after it.
// Construct once at startup and pass WithWorldGroup to the open world and manager.
type WorldGroup struct {
	mu          sync.Mutex
	tick        uint64
	parties     map[uint64]*party
	memberships map[partyMemberKey]uint64
}

func NewWorldGroup() *WorldGroup {
	return &WorldGroup{parties: make(map[uint64]*party), memberships: make(map[partyMemberKey]uint64)}
}

func WithWorldGroup(group *WorldGroup) SimOption {
	return func(options *simOptions) { options.group = group }
}

// Transfer moves the existing player, retaining its entity id, inventory, vitals
// and party membership. The session must stop and join its world workers first;
// it must not call Player methods concurrently with this operation. Simulation
// ticks and other players remain safe under the group's shared lock.
// No instance membership or entry policy is decided here.
func (s *Sim) Transfer(p *Player, target *Sim, spawn [3]float32) error {
	if p == nil || target == nil || s.group != target.group {
		return errors.New("game: transfer needs worlds in the same group")
	}
	for _, v := range spawn {
		if math.IsNaN(float64(v)) || math.IsInf(float64(v), 0) {
			return errors.New("game: transfer position must be finite")
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if !s.onlineLocked(p) || p.leaving || !p.alive() {
		return errors.New("game: player cannot change world")
	}
	if target == s {
		return errors.New("game: player is already in this world")
	}
	if target.players[p.entityID] != nil || target.byIdentity[p.playerID] != nil || target.byName[foldPlayerName(p.name)] != nil {
		return errors.New("game: destination identity is occupied")
	}
	s.removeAllThreatFor(p.entityID)
	s.rememberTapExperienceLocked(characterKeyOf(p.playerID, p.name), p.experience)
	s.clearInvitesFromLocked(p.entityID)
	p.closePlayerTradeLocked(vnet.PlayerTradeCloseReasonDisconnected)
	p.setMiningLocked(nil)
	p.mineCompleting = false
	p.miningCompleted = nil
	p.mineReset = nil
	p.blocking = false

	p.pendingSwing = nil
	p.cast = nil
	p.pendingCastRefusals = nil
	p.pendingMobHits = nil
	p.openLootID = 0
	p.lootDirty = false
	p.lootClosures = nil
	p.openVendorID = 0
	p.vendorDirty = false
	p.vendorClosures = nil
	p.playerTradeClosures = nil
	p.playerTradeRefusals = nil

	p.invite = nil
	p.current = intent{yaw: p.yaw}
	p.vel = [3]float64{}
	p.onGround = false
	p.described = make(map[uint64]uint64)
	p.audible = make(map[uint64]struct{})
	s.forgetVoiceListenerLocked(p.entityID)
	delete(s.players, p.entityID)
	delete(s.byIdentity, p.playerID)
	delete(s.byName, foldPlayerName(p.name))
	p.sim = target
	p.pos = [3]float64{float64(spawn[0]), float64(spawn[1]), float64(spawn[2])}
	p.spawn = p.pos
	p.chunk = chunkAt(p.pos)
	p.chunks = newChunkFeed()
	p.chunks.publish(p.chunk)
	p.mineReady = make(chan MiningCompletion, 1)
	target.players[p.entityID] = p
	target.byIdentity[p.playerID] = p
	target.byName[foldPlayerName(p.name)] = p
	return nil
}
