package session

import "github.com/FabioSM46/voxelheim-v2/server/internal/game"

// SessionLoot is the loot a session's takes reach, named for tests outside the package.
type SessionLoot = sessionLoot

// PutBossLootBehind makes this claim set's sessions take loot through loot rather than from
// their player directly. It stands in for a killed dungeon boss's corpse, which a session test
// cannot build. Call it before any session starts.
func PutBossLootBehind(i *Identities, loot func(*game.Player) SessionLoot) { i.bossLoot = loot }
