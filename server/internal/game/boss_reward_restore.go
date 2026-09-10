package game

import (
	"fmt"
	"math"
	"slices"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// validHeldRewards refuses held loot a restore could not rebuild exactly: a defeat the run does
// not record, a species with no home anchor, a second defeat of one boss, an owner named twice, a
// taken index past the roll or taken silver that was never rolled, or a roll no pack could hold.
func validHeldRewards(rec SavedSession) error {
	seen := make(map[vnet.MobKind]bool, len(rec.HeldRewards))
	for _, d := range rec.HeldRewards {
		refuse := fmt.Errorf("%w: run %d holds loot for %s that cannot be rebuilt", ErrInvalidSession, rec.ID, d.Kind)
		if (d.Kind != vnet.MobKindVargrGuardian && d.Kind != vnet.MobKindDraugrKing) || seen[d.Kind] || !slices.Contains(rec.DefeatedBosses, d.Kind) || len(d.Personal) == 0 {
			return refuse
		}
		seen[d.Kind] = true
		owners := make(map[InstanceCharacter]bool, len(d.Personal))
		for _, reward := range d.Personal {
			if owners[reward.Owner] || len(reward.Entries) > maxBossRewardEntries ||
				reward.Taken&^heldRollMask(len(reward.Entries)) != 0 || (reward.SilverTaken && reward.Silver == 0) {
				return refuse
			}
			owners[reward.Owner] = true
			for _, entry := range reward.Entries {
				stack := heldStack(entry)
				definition, ok := itemByID(stack.item)
				if !ok || stack.item == ItemNone || stack.count == 0 || !validDropWear(stack, definition) {
					return refuse
				}
			}
		}
	}
	return nil
}

// heldRollMask is the taken bits a roll of n entries can have.
func heldRollMask(n int) uint64 {
	if n >= 64 {
		return ^uint64(0)
	}
	return uint64(1)<<n - 1
}

func heldStack(entry protocol.InventoryStack) inventoryStack {
	return inventoryStack{item: ItemID(entry.ItemID), count: entry.Count, durability: entry.Durability, maxDurability: entry.MaxDurability}
}

// restoreHeldBossRewards rebuilds each held defeat as a boss corpse whose loot is delivered only
// through claims. The corpse lies at its boss's home anchor and holds what each owner is still
// owed: the entries it has not taken, each at its original roll index, and the silver unless it
// was taken. A claim therefore names the same indices the journal records.
//
// It does not expire. The journal owes the loot until the run resets, and the run's removal is
// what takes the corpse away.
func (s *Sim) restoreHeldBossRewards(seed int64, held []BossRewardDefeat) {
	if len(held) == 0 {
		return
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	guardian, king, _ := world.InstanceEncounterAnchors(seed)
	for _, d := range held {
		home := guardian
		if d.Kind == vnet.MobKindDraugrKing {
			home = king
		}
		c := &corpse{
			entityID:    s.mintEntityID(),
			kind:        d.Kind,
			pos:         [3]float64{float64(home.X) + .5, float64(home.Y), float64(home.Z) + .5},
			chunk:       world.ChunkOf(home.X, home.Y, home.Z),
			personal:    make(map[corpseOwner]*corpseContainer, len(d.Personal)),
			expiresTick: math.MaxUint64,
			rewards:     bossRewardsClaimed,
		}
		for _, reward := range d.Personal {
			container := &corpseContainer{revision: 1}
			if !reward.SilverTaken {
				container.silver = reward.Silver
			}
			for k, entry := range reward.Entries {
				if reward.Taken&(uint64(1)<<k) != 0 {
					continue
				}
				container.entries = append(container.entries, corpseEntry{entryID: uint64(k + 1), stack: heldStack(entry)})
			}
			c.personal[corpseOwner{playerID: reward.Owner.PlayerID, characterID: reward.Owner.CharacterID}] = container
		}
		s.corpses[c.entityID] = c
	}
}
