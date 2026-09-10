package game

import (
	"errors"
	"fmt"
	"slices"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// A dungeon boss's personal loot waits for the boss reward journal (#1036).
//
// With durable boss rewards enabled, a dungeon boss's death freezes what each roster owner
// rolled into a pending defeat, and the corpse's loot is held: nobody can open it. The
// manager reports the pending defeat on the run's SavedSession until the journal has made
// it durable, and then releases it. From release on, that loot is delivered only through a
// boss reward claim: a take answers ErrBossRewardClaimRequired, the claim is built from
// BossLootToClaim, and ConsumeClaimedBossLoot removes what an acknowledged claim took.
//
// The roster is the encounter's personal-loot roster frozen at the pull. It is neither the
// run's bindings nor the experience recipients, and it records neither. Nothing enables
// this outside a test until the session side is wired.

var (
	// ErrBossRewardClaimRequired answers a take against a boss corpse whose loot is
	// delivered only through a boss reward claim.
	ErrBossRewardClaimRequired = errors.New("game: this boss's loot is delivered through a reward claim")

	errBossRewardHeld = errors.New("the boss's rewards are not durable yet")
)

// WithDurableBossRewards holds a dungeon boss's personal loot until the boss reward
// journal has made its defeat durable, and then delivers it only through claims. The
// default is false, which keeps loot live. Only dungeon simulations are affected.
func WithDurableBossRewards(enabled bool) SimOption {
	return func(options *simOptions) { options.durableBossRewards = enabled }
}

// BossPersonalReward is one roster owner's frozen personal roll, in roll order.
type BossPersonalReward struct {
	Owner   InstanceCharacter
	Entries []protocol.InventoryStack
	Silver  uint32
}

// BossRewardDefeat is what one dungeon boss's death owes, frozen at the kill. Personal is
// the encounter's loot roster in roll order.
type BossRewardDefeat struct {
	Kind     vnet.MobKind
	Personal []BossPersonalReward

	corpseID uint64
}

type bossRewardHold uint8

const (
	bossRewardsOpen    bossRewardHold = iota // ordinary live loot
	bossRewardsHeld                          // waiting for the journal
	bossRewardsClaimed                       // delivered only through claims
)

// holdBossRewardsLocked freezes a dungeon boss corpse's personal rolls into a pending defeat
// and holds its loot. The caller holds Sim.mu and has just rolled every roster container.
func (s *Sim) holdBossRewardsLocked(c *corpse, kind vnet.MobKind, roster []corpseOwner) {
	defeat := BossRewardDefeat{Kind: kind, corpseID: c.entityID}
	for _, owner := range roster {
		container := c.personal[owner]
		if container == nil || slices.ContainsFunc(defeat.Personal, func(r BossPersonalReward) bool {
			return r.Owner == InstanceCharacter{owner.playerID, owner.characterID}
		}) {
			continue
		}
		reward := BossPersonalReward{Owner: InstanceCharacter{owner.playerID, owner.characterID}, Silver: container.silver}
		for _, entry := range container.entries {
			reward.Entries = append(reward.Entries, protocol.InventoryStack{
				ItemID: uint16(entry.stack.item), Count: entry.stack.count,
				Durability: entry.stack.durability, MaxDurability: entry.stack.maxDurability,
			})
		}
		defeat.Personal = append(defeat.Personal, reward)
	}
	c.rewards = bossRewardsHeld
	s.bossRewards = append(s.bossRewards, defeat)
}

func clonePendingRewards(pending []BossRewardDefeat) []BossRewardDefeat {
	if len(pending) == 0 {
		return nil
	}
	out := make([]BossRewardDefeat, len(pending))
	for i, defeat := range pending {
		out[i] = defeat
		out[i].Personal = make([]BossPersonalReward, len(defeat.Personal))
		for k, reward := range defeat.Personal {
			out[i].Personal[k] = reward
			out[i].Personal[k].Entries = slices.Clone(reward.Entries)
		}
	}
	return out
}

// ReleaseBossRewards records that the journal has made one of a run's defeats durable with
// the entitlements its SavedSession reported. Its corpse's loot is then delivered only
// through claims. It applies only to the exact run, as AssignRunGeneration does, and only to
// a defeat still pending; anything else is left untouched and reported false.
func (m *InstanceManager) ReleaseBossRewards(run SavedSession, kind vnet.MobKind) bool {
	m.mu.Lock()
	defer m.mu.Unlock()
	s := m.sessions[run.ID]
	if s == nil || s.state != InstanceSaved || s.seed != run.Seed || s.ruin != run.Ruin || s.expiresUnix != run.ExpiresUnix {
		return false
	}
	at := slices.IndexFunc(s.pendingRewards, func(d BossRewardDefeat) bool { return d.Kind == kind })
	if at < 0 {
		return false
	}
	defeat := s.pendingRewards[at]
	s.pendingRewards = slices.Delete(s.pendingRewards, at, at+1)
	s.sim.mu.Lock()
	if c := s.sim.corpses[defeat.corpseID]; c != nil && c.rewards == bossRewardsHeld {
		c.rewards = bossRewardsClaimed
	}
	s.sim.mu.Unlock()
	return true
}

// BossLootSelection is the exact grant one take asks of a claimed boss corpse: the chosen
// remaining entries, each with its index in the frozen roll, and the silver.
type BossLootSelection struct {
	CorpseID     uint64
	Kind         vnet.MobKind
	Entries      []protocol.InventoryStack
	EntryIndices []uint8
	Silver       uint32
	// Partial is take-all's fit policy: an entry that does not fit is stepped over.
	Partial bool
}

// BossLootToClaim answers a take against a boss corpse whose loot is delivered by claims,
// under the same access, open-container and revision rules the take enforces. entryID zero
// asks for everything that remains, fitted in order, and the silver.
func (p *Player) BossLootToClaim(corpseID uint64, revision uint32, entryID uint64) (BossLootSelection, vnet.RefusalReason, error) {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	if err := p.cannotActLocked(); err != nil {
		return BossLootSelection{}, vnet.RefusalReasonPlayerIsDead, err
	}
	c, container, reason, err := p.openContainerLocked(corpseID, revision)
	if err != nil {
		return BossLootSelection{}, reason, err
	}
	if c.rewards != bossRewardsClaimed {
		return BossLootSelection{}, vnet.RefusalReasonUnknown, errors.New("the corpse's loot is not delivered through claims")
	}
	selection := BossLootSelection{CorpseID: c.entityID, Kind: c.kind, Partial: entryID == 0}
	for _, entry := range container.entries {
		if entryID != 0 && entry.entryID != entryID {
			continue
		}
		if entry.entryID == 0 || entry.entryID > maxBossRewardEntries {
			return BossLootSelection{}, vnet.RefusalReasonCorpseUnavailable, fmt.Errorf("loot entry %d is outside the frozen roll", entry.entryID)
		}
		selection.Entries = append(selection.Entries, protocol.InventoryStack{
			ItemID: uint16(entry.stack.item), Count: entry.stack.count,
			Durability: entry.stack.durability, MaxDurability: entry.stack.maxDurability,
		})
		selection.EntryIndices = append(selection.EntryIndices, uint8(entry.entryID-1))
	}
	if entryID == 0 {
		selection.Silver = container.silver
	} else if len(selection.Entries) == 0 {
		return BossLootSelection{}, vnet.RefusalReasonCorpseUnavailable, fmt.Errorf("loot entry %d is unavailable", entryID)
	}
	return selection, vnet.RefusalReasonUnknown, nil
}

// ConsumeClaimedBossLoot removes what an acknowledged claim took from its corpse: the entries
// at those roll indices and the silver it claimed. Entries and silver follow one rule: each
// must still be exactly there. A claim consumed twice, or against a corpse that expired or
// changed, therefore reports false and changes nothing, as does a consumption naming nothing.
func (p *Player) ConsumeClaimedBossLoot(corpseID uint64, indices []uint8, silver uint32) bool {
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	c := p.sim.corpses[corpseID]
	if c == nil || c.rewards != bossRewardsClaimed || (len(indices) == 0 && silver == 0) {
		return false
	}
	container, owned := c.containerFor(p)
	if !owned || (silver != 0 && container.silver != silver) {
		return false
	}
	taken := func(e corpseEntry) bool { return slices.Contains(indices, uint8(e.entryID-1)) }
	for _, index := range indices {
		if !slices.ContainsFunc(container.entries, func(e corpseEntry) bool { return e.entryID == uint64(index)+1 }) {
			return false
		}
	}
	container.entries = slices.DeleteFunc(container.entries, taken)
	container.silver -= silver
	container.revision++
	if p.openLootID == corpseID {
		p.lootDirty = true
	}
	return true
}

// QueueLootRefusal records a TakeLoot refusal decided after the take returned, such as a boss
// reward claim that did not fit or did not land. The tick delivers it and never drops it, and
// a refusal already waiting is not queued twice.
func (p *Player) QueueLootRefusal(reason vnet.RefusalReason) {
	if reason == vnet.RefusalReasonUnknown {
		return
	}
	p.sim.mu.Lock()
	defer p.sim.mu.Unlock()
	if !slices.Contains(p.lootRefusals, reason) {
		p.lootRefusals = append(p.lootRefusals, reason)
	}
}
