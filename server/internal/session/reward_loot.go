package session

import (
	"errors"
	"log/slog"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// bossLootClaimer is what a take needs from the game to deliver a boss's claimed loot. It is
// the player itself in production.
type bossLootClaimer interface {
	BossLootToClaim(corpseID uint64, revision uint32, entryID uint64) (game.BossLootSelection, vnet.RefusalReason, error)
	ConsumeClaimedBossLoot(corpseID uint64, indices []uint8, silver uint32) bool
	QueueLootRefusal(reason vnet.RefusalReason)
}

var errBossLootOutsideVisit = errors.New("session: boss loot is claimed only inside its dungeon visit")

// claimBossLoot delivers a take against a boss corpse whose loot arrives only through a
// reward claim (#1036). run is the saved run this character is visiting through a portal, or
// zero when it is not inside one: claims are made only for dungeon visits.
//
// It returns the refusal to send now. A submitted claim answers later: when it lands, the
// corpse gives up exactly the entries and silver the claim took; when it fails, the refusal
// goes through the game's loot refusal queue, which the tick delivers and never drops.
func (i *Identities) claimBossLoot(self Resolved, player *game.Player, loot bossLootClaimer, run, corpseID uint64, revision uint32, entryID uint64) (vnet.RefusalReason, error) {
	if run == 0 {
		return vnet.RefusalReasonCorpseUnavailable, errBossLootOutsideVisit
	}
	generation := i.rewardRunGeneration(run)
	if generation == 0 {
		return vnet.RefusalReasonCorpseUnavailable, errors.New("session: the visited run has no durable boss reward journal run")
	}
	selection, reason, err := loot.BossLootToClaim(corpseID, revision, entryID)
	if err != nil {
		return reason, err
	}
	if len(selection.Entries) == 0 && selection.Silver == 0 {
		return vnet.RefusalReasonCorpseUnavailable, errors.New("session: the boss corpse holds nothing left to claim")
	}
	done, err := i.ClaimBossReward(BossRewardClaim{
		Self: self, Player: player, Generation: generation, Boss: selection.Kind,
		Grant:        game.BossRewardGrant{Entries: selection.Entries, Silver: selection.Silver, Partial: selection.Partial},
		EntryIndices: selection.EntryIndices,
		Delivered: func(indices []uint8, silver bool) {
			claimed := uint32(0)
			if silver {
				claimed = selection.Silver
			}
			loot.ConsumeClaimedBossLoot(selection.CorpseID, indices, claimed)
		},
	})
	if err != nil {
		return lootRefusalFor(err), err
	}
	go func() {
		if claimErr := <-done; claimErr != nil {
			loot.QueueLootRefusal(lootRefusalFor(claimErr))
		}
	}()
	return vnet.RefusalReasonUnknown, nil
}

// rewardRunGeneration is the journal generation of the saved run with this runtime id, or
// zero when rewards are off or the run holds none yet.
func (i *Identities) rewardRunGeneration(run uint64) uint64 {
	i.mu.Lock()
	c := i.rewards
	i.mu.Unlock()
	if c == nil {
		return 0
	}
	for _, saved := range c.runs.SavedSessions() {
		if saved.ID == run {
			return saved.Generation
		}
	}
	return 0
}

// lootRefusalFor names a failed claim in the take's vocabulary: busy while something else
// owns the pack or the journal, full when the grant would not fit, and otherwise gone.
func lootRefusalFor(err error) vnet.RefusalReason {
	switch {
	case errors.Is(err, ErrRewardOwned), errors.Is(err, game.ErrBossRewardBusy), errors.Is(err, errJournalBusy):
		return vnet.RefusalReasonInventoryBusy
	case errors.Is(err, game.ErrBossRewardClaim):
		return vnet.RefusalReasonInventoryFull
	default:
		return vnet.RefusalReasonCorpseUnavailable
	}
}

// lootTakes is how a session's decoded loot takes reach the game.
type lootTakes interface {
	TakeLoot(request protocol.LootTakeRequest) (vnet.RefusalReason, error)
	TakeAllLoot(request protocol.LootTakeAllRequest) (vnet.RefusalReason, error)
}

// sessionLoot is the loot a take reaches: the player itself in production.
type sessionLoot interface {
	lootTakes
	bossLootClaimer
}

// lootOf is the loot this player's takes reach.
func (i *Identities) lootOf(player *game.Player) sessionLoot {
	if i.bossLoot != nil {
		return i.bossLoot(player)
	}
	return player
}

// visitLootTakes routes one session's loot takes. Live loot goes straight to the game. A
// dungeon boss's held loot goes through a reward claim, and only on the portal visit into
// that boss's run.
type visitLootTakes struct {
	identities *Identities
	log        *slog.Logger
	// current reads the session's state when a take arrives: who is playing, the live
	// player, and the saved run it is visiting through a portal, or zero outside one.
	current func() (self Resolved, player *game.Player, run uint64)
}

func (t visitLootTakes) TakeLoot(request protocol.LootTakeRequest) (vnet.RefusalReason, error) {
	self, player, run := t.current()
	loot := t.identities.lootOf(player)
	reason, err := loot.TakeLoot(request)
	if !errors.Is(err, game.ErrBossRewardClaimRequired) {
		return reason, err
	}
	return t.claim(self, player, loot, run, request.CorpseID, request.Revision, request.EntryID)
}

func (t visitLootTakes) TakeAllLoot(request protocol.LootTakeAllRequest) (vnet.RefusalReason, error) {
	self, player, run := t.current()
	loot := t.identities.lootOf(player)
	reason, err := loot.TakeAllLoot(request)
	if !errors.Is(err, game.ErrBossRewardClaimRequired) {
		return reason, err
	}
	return t.claim(self, player, loot, run, request.CorpseID, request.Revision, 0)
}

// claim turns a take into a boss reward claim. A take routed outside its visit is logged at
// warn: a boss corpse lives only inside its dungeon, so one reaching here means the session's
// idea of where the player stands has gone wrong, and a player would silently lose the loot.
func (t visitLootTakes) claim(self Resolved, player *game.Player, loot sessionLoot, run, corpseID uint64, revision uint32, entryID uint64) (vnet.RefusalReason, error) {
	reason, err := t.identities.claimBossLoot(self, player, loot, run, corpseID, revision, entryID)
	if errors.Is(err, errBossLootOutsideVisit) {
		t.log.Warn("a boss reward take arrived outside its dungeon visit; refusing it", "corpse_id", corpseID)
	}
	return reason, err
}
