package game

import (
	"errors"
	"fmt"
	"slices"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// dungeonChest links a stable layout slot to an ordinary, ephemeral loot container.
// Only the opening is persisted. A restart never recreates unclaimed ordinary loot.
type dungeonChest struct {
	anchor      world.PlacedAnchor
	containerID uint64
}

// Construction only: validate every slot before publishing the session, then reapply
// saved block state. The lookup keeps both closed and opened cells addressable.
func (s *Sim) placeDungeonChests() error {
	d := s.dungeon
	d.chests = make(map[[3]int64]*dungeonChest)
	seen := make(map[int]bool)
	var restore world.InstanceUpdate
	for _, a := range d.gate.Chests() {
		key := [3]int64{a.X, a.Y, a.Z}
		if a.Index < 0 || a.Index >= world.InstanceChestCount || seen[a.Index] || d.chests[key] != nil {
			return fmt.Errorf("game: invalid or duplicate chest slot %d", a.Index)
		}
		seen[a.Index] = true
		d.chests[key] = &dungeonChest{anchor: a}
		if slices.Contains(d.progress.route.OpenedChests, uint8(a.Index)) {
			restore.Mechanisms = append(restore.Mechanisms, world.InstanceCell{X: a.X, Y: a.Y, Z: a.Z, Block: world.ChestOpen})
		}
	}
	if len(seen) != world.InstanceChestCount {
		return fmt.Errorf("game: dungeon has %d chest slots, want %d", len(seen), world.InstanceChestCount)
	}
	d.gate.Update(restore)
	return nil
}

// useDungeonChestLocked runs after the common alive and reach checks. First opening
// freezes one personal roll for each party member currently in this simulation;
// subsequent uses only project the requester's existing remainder. No other player's
// UI is opened, no corpse is drawn, and the durable boss reward journal is untouched.
func (p *Player) useDungeonChestLocked(chest *dungeonChest) (vnet.RefusalReason, error) {
	s, a := p.sim, chest.anchor
	d := s.dungeon
	if !slices.Contains(d.progress.route.OpenedChests, uint8(a.Index)) {
		if a.Index == world.KingChest && !d.progress.king {
			return vnet.RefusalReasonMechanismLocked, errMechanismLocked
		}
		c := &corpse{
			entityID:    s.mintEntityID(),
			chest:       true,
			pos:         [3]float64{float64(a.X), float64(a.Y), float64(a.Z)},
			chunk:       world.ChunkOf(a.X, a.Y, a.Z),
			personal:    make(map[corpseOwner]*corpseContainer),
			expiresTick: s.currentTick + s.corpseLifetimeTicks,
		}
		// Roster declaration order is RNG order, never map iteration order. A solo
		// player receives one roll; offline and other-world members receive none.
		members := []*Player{p}
		if held := s.parties[p.partyID]; p.partyID != 0 && held != nil {
			members = nil
			for _, member := range held.members {
				if s.onlineLocked(member.player) {
					members = append(members, member.player)
				}
			}
		}
		for _, member := range members {
			owner := member.corpseOwner()
			if _, duplicate := c.personal[owner]; duplicate {
				continue
			}
			container := s.rollTableLocked(chestLootTables[a.Index])
			c.personal[owner] = &container
		}
		s.corpses[c.entityID] = c
		chest.containerID = c.entityID
		d.progress.route.OpenedChests = append(d.progress.route.OpenedChests, uint8(a.Index))
		slices.Sort(d.progress.route.OpenedChests)
		s.announceDungeonCellsLocked(d.gate.Update(world.InstanceUpdate{Mechanisms: []world.InstanceCell{
			{X: a.X, Y: a.Y, Z: a.Z, Block: world.ChestOpen},
		}}))
	}
	c := s.corpses[chest.containerID]
	container, owned := c.containerFor(p)
	if !owned || container.empty() {
		return vnet.RefusalReasonChestAlreadyOpened, errors.New("the chest has no personal loot remaining")
	}
	if p.openLootID != 0 && p.openLootID != c.entityID {
		p.queueLootClosedLocked(p.openLootID)
	}
	p.openLootID, p.lootDirty = c.entityID, true
	p.offerLootLocked() // a full outbound queue leaves lootDirty for the tick to retry
	return vnet.RefusalReasonUnknown, nil
}
