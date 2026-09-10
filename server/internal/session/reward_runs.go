package session

import (
	"errors"
	"fmt"
	"slices"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// SyncRewardRuns makes the boss reward journal hold every saved run's defeated progress
// and bindings, then collects the runs whose reset has passed (#1036).
//
// **Progress only.** A defeat is appended with no personal loot and no experience
// entitlement, because boss loot and experience still reach players through the live
// corpse and award paths. The journal therefore delivers nothing and cannot duplicate
// either. What it does hold is the durable answer to which encounters a run has put down
// and who owes it, so a restart restores that from the journal even when the sessions
// file is stale or missing.
//
// Every transition is an idempotent retry. A run without a generation is allocated the
// journal's next one and then told it. Its defeats are appended in the order they died,
// and a binding that joins later is unioned onto the run. No simulation or manager lock is
// held across a write. A crash between the allocation and the first append restores the
// run without that defeat. That is the conservative direction, because the journal only
// ever claims progress it wrote.
func (i *Identities) SyncRewardRuns(now time.Time) error {
	i.mu.Lock()
	c := i.rewards
	i.mu.Unlock()
	if c == nil {
		return nil
	}
	c.syncMu.Lock()
	defer c.syncMu.Unlock()

	var errs []error
	for _, run := range c.manager.SavedSessions() {
		if run.ExpiresUnix <= now.Unix() {
			// Resetting: the manager removes it on its next step and the collection below
			// removes its journal run.
			continue
		}
		if err := i.syncRewardRun(c, run); err != nil {
			errs = append(errs, fmt.Errorf("session: boss reward run %d: %w", run.ID, err))
		}
	}
	if _, err := c.journal.ExpireRuns(now.Unix()); err != nil {
		errs = append(errs, fmt.Errorf("session: collecting reset boss reward runs: %w", err))
	}
	return errors.Join(errs...)
}

func (i *Identities) syncRewardRun(c *rewardCoordinator, run game.SavedSession) error {
	generation := run.Generation
	if generation == 0 {
		journal, err := c.journal.Snapshot()
		if err != nil {
			return err
		}
		generation = journal.NextGeneration
		record := persist.SessionRecord{
			ID: run.ID, Seed: run.Seed, Ruin: [2]int64{run.Ruin.CellX, run.Ruin.CellZ}, ExpiresUnix: run.ExpiresUnix,
		}
		if err := c.journal.AllocateRun(i.store, generation, record, world.WorldgenVersion); err != nil {
			return err
		}
		c.manager.AssignRunGeneration(run, generation)
	}

	bound := make([]persist.SessionCharacter, len(run.Bound))
	for k, who := range run.Bound {
		bound[k] = persist.SessionCharacter{PlayerID: who.PlayerID, CharacterID: who.CharacterID}
	}
	for _, kind := range run.DefeatedBosses {
		stored, err := journalRun(c.journal, generation)
		if err != nil {
			return err
		}
		if slices.ContainsFunc(stored.Defeats, func(d persist.RewardDefeat) bool { return d.Kind == kind }) {
			continue
		}
		if err := c.journal.AppendDefeat(generation, persist.RewardDefeat{Kind: kind}, bound); err != nil {
			return err
		}
	}

	stored, err := journalRun(c.journal, generation)
	if err != nil || len(stored.Defeats) == 0 {
		return err
	}
	if !slices.ContainsFunc(bound, func(o persist.SessionCharacter) bool { return !slices.Contains(stored.Session.Bound, o) }) {
		return nil
	}
	// A binding with no new defeat is unioned through the run's latest defeat exactly as
	// stored, which AppendDefeat accepts as a retry that adds only the binding.
	return c.journal.AppendDefeat(generation, stored.Defeats[len(stored.Defeats)-1], bound)
}

func journalRun(journal *persist.RewardStore, generation uint64) (persist.RewardRun, error) {
	snapshot, err := journal.Snapshot()
	if err != nil {
		return persist.RewardRun{}, err
	}
	for _, run := range snapshot.Runs {
		if run.Generation == generation {
			return run, nil
		}
	}
	return persist.RewardRun{}, fmt.Errorf("%w: generation %d is not in the journal", persist.ErrRewardJournalConflict, generation)
}

// ValidateRewardRecord is the game's judgement of a character record that a prepared boss
// reward would restore at startup, for persist.OpenStoreWithRewardRecovery. The store
// checks what a file can be wrong about; this checks what a life can be.
func ValidateRewardRecord(rec persist.Record) error {
	life := lifeOfRecord(rec)
	return life.Validate()
}
