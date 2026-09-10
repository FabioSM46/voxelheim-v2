package session

import (
	"errors"
	"fmt"

	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
)

// DeliverBossExperience claims the boss experience the journal still owes each character who
// is inside that run's dungeon (#1036).
//
// **Boss experience is delivered only here.** A durable dungeon boss's kill freezes its
// experience shares instead of awarding them (game.Sim.creditMobDamageLocked). The sync
// journals them with the defeat, and this pass delivers each untaken share through
// ClaimBossReward, which marks it taken in the same acknowledgement that publishes it. A share
// is therefore delivered at most once, by any build.
//
// **Only on a dungeon visit.** A share is claimed while its owner is playing and inside the run
// that owes it, as the manager reports. Nothing here depends on the defeat still being in
// memory, so a share still owed after a restart is offered again the same way.
//
// One claim per character at a time: an owner with a prepared intent, or whose claim the
// coordinator is already running, waits for a later pass. No simulation, manager or journal lock
// is held across a claim, and every journal write a claim makes goes through the shared gate.
func (i *Identities) DeliverBossExperience() error {
	i.mu.Lock()
	c := i.rewards
	i.mu.Unlock()
	if c == nil {
		return nil
	}
	journal, err := c.journal.Snapshot()
	if err != nil {
		return fmt.Errorf("session: reading the boss reward journal: %w", err)
	}
	busy := make(map[persist.SessionCharacter]bool, len(journal.Intents))
	for _, intent := range journal.Intents {
		busy[intent.Owner] = true
	}
	runtime := make(map[uint64]uint64)
	for _, run := range c.runs.SavedSessions() {
		if run.Generation != 0 {
			runtime[run.Generation] = run.ID
		}
	}

	var errs []error
	for _, run := range journal.Runs {
		id, live := runtime[run.Generation]
		if !live {
			continue
		}
		for _, defeat := range run.Defeats {
			for _, share := range defeat.Experience {
				if share.Taken || busy[share.Owner] {
					continue
				}
				owner := game.InstanceCharacter{PlayerID: share.Owner.PlayerID, CharacterID: share.Owner.CharacterID}
				player := c.runs.PlayerInside(id, owner)
				if player == nil {
					continue
				}
				self, playing := i.playingOwner(share.Owner)
				if !playing {
					continue
				}
				_, claimErr := i.ClaimBossReward(BossRewardClaim{
					Self: self, Player: player, Generation: run.Generation, Boss: defeat.Kind,
					Grant: game.BossRewardGrant{Experience: share.Amount},
				})
				busy[share.Owner] = true
				if claimErr != nil && !errors.Is(claimErr, ErrRewardOwned) && !errors.Is(claimErr, ErrRewardNotPlaying) && !errors.Is(claimErr, ErrRewardsDraining) {
					errs = append(errs, fmt.Errorf("session: claiming boss experience for generation %d: %w", run.Generation, claimErr))
				}
			}
		}
	}
	return errors.Join(errs...)
}

// playingOwner is the resolved identity of a journal owner who is playing that character now.
// It carries what a claim uses: the identity, the character, and the map ledgers a detached
// claim writes the record with.
func (i *Identities) playingOwner(owner persist.SessionCharacter) (Resolved, bool) {
	i.mu.Lock()
	defer i.mu.Unlock()
	held, live := i.live[owner.PlayerID]
	if !live || held.finalised || uint64(held.character) != owner.CharacterID {
		return Resolved{}, false
	}
	return Resolved{ID: owner.PlayerID, Character: held.character, Explored: held.exploration, Marks: held.markers}, true
}
