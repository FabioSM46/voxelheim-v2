package game

import (
	"errors"
	"fmt"
	"strconv"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// These hooks exist only in this package's test binary. They let the external end-to-end tests
// do the two things a session cannot do through frames: land a real boss kill, and find the
// corpse it left. Everything they do goes through the same code a real hit and a real teleport
// use.

// KillDungeonBossForTest pulls this simulation's dungeon boss of kind with the player whose
// entity id is given, taps it for that player and lands a killing blow through the credit path a
// real hit takes, so rolls, holds, bindings and experience all follow.
func (s *Sim) KillDungeonBossForTest(kind vnet.MobKind, entityID uint64) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.dungeon == nil {
		return errors.New("game: this simulation holds no dungeon")
	}
	id := s.dungeon.guardianID
	if kind == vnet.MobKindDraugrKing {
		id = s.dungeon.kingID
	}
	boss, p := s.mobs[id], s.players[entityID]
	if boss == nil || boss.kind != kind || p == nil {
		return fmt.Errorf("game: no living %s or no player %d to kill it", kind, entityID)
	}
	s.startBossEncounterLocked(boss, p)
	boss.firstHit = newMobTap(p)
	s.creditMobDamageLocked(p, boss, boss.health)
	if s.mobs[id] != nil {
		return fmt.Errorf("game: the %s survived its killing blow", kind)
	}
	return nil
}

// DungeonBossAliveForTest reports whether a boss of kind is standing in this simulation.
func (s *Sim) DungeonBossAliveForTest(kind vnet.MobKind) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	for _, m := range s.mobs {
		if m.kind == kind {
			return true
		}
	}
	return false
}

// BossCorpseForTest is the id and position of the corpse a boss of kind left in this simulation.
func (s *Sim) BossCorpseForTest(kind vnet.MobKind) (uint64, [3]float64, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	for id, c := range s.corpses {
		if c.kind == kind {
			return id, c.pos, true
		}
	}
	return 0, [3]float64{}, false
}

// TeleportForTest moves a player exactly as the /teleport development command does.
func (s *Sim) TeleportForTest(entityID uint64, pos [3]int64) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	p := s.players[entityID]
	if p == nil {
		return fmt.Errorf("game: no player %d", entityID)
	}
	args := []string{strconv.FormatInt(pos[0], 10), strconv.FormatInt(pos[1], 10), strconv.FormatInt(pos[2], 10)}
	if outcome, ok := p.teleportCommandLocked(args); !ok {
		return errors.New(outcome.PrivateText)
	}
	return nil
}
