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
// One snapshot is read per pass, and only what is missing is written:
//   - A saved run without a generation is allocated one with its current defeats and
//     bindings in the same write, so no crash leaves a new run holding less than the
//     manager had.
//   - A run the journal already holds under the same seed, ruin and reset is adopted
//     rather than allocated again. Two live runs with one identity would make the restore
//     overlay refuse startup, and adoption is also how an allocation the manager did not
//     accept is repaired on the next pass.
//   - Later defeats are appended in death order, and a later binding is unioned.
//
// A failed journal write may still have landed, and the journal refuses any other bytes
// until those exact bytes are retried, so it is retried verbatim before anything else is
// written. No simulation or manager lock is held across a write. A defeat the process
// stopped before journaling is restored undefeated: the journal only claims progress it
// wrote, which is what lets a reward wait for its defeat to be durable.
func (i *Identities) SyncRewardRuns(now time.Time) error {
	i.mu.Lock()
	c := i.rewards
	i.mu.Unlock()
	if c == nil {
		return nil
	}
	c.syncMu.Lock()
	defer c.syncMu.Unlock()

	if c.pendingRunWrite != nil {
		if err := c.pendingRunWrite(); err != nil {
			return fmt.Errorf("session: retrying a boss reward journal write: %w", err)
		}
		c.pendingRunWrite = nil
	}

	journal, err := c.journal.Snapshot()
	if err != nil {
		return fmt.Errorf("session: reading the boss reward journal: %w", err)
	}
	pass := rewardRunPass{
		byGeneration: make(map[uint64]persist.RewardRun, len(journal.Runs)),
		byIdentity:   make(map[rewardRunIdentity]uint64, len(journal.Runs)),
		held:         make(map[uint64]bool),
		next:         journal.NextGeneration,
	}
	for _, run := range journal.Runs {
		pass.byGeneration[run.Generation] = run
		pass.byIdentity[rewardRunIdentity{run.Session.Seed, run.Session.Ruin, run.Session.ExpiresUnix}] = run.Generation
	}
	saved := c.manager.SavedSessions()
	for _, run := range saved {
		if run.Generation != 0 {
			pass.held[run.Generation] = true
		}
	}

	var errs []error
	for _, run := range saved {
		if run.ExpiresUnix <= now.Unix() {
			// Resetting: the manager removes it on its next step and the collection below
			// removes its journal run.
			continue
		}
		if err := i.syncRewardRun(c, &pass, run); err != nil {
			errs = append(errs, fmt.Errorf("session: boss reward run %d: %w", run.ID, err))
			if c.pendingRunWrite != nil {
				return errors.Join(errs...)
			}
		}
	}
	collectAt := now.Unix()
	collect := func() error {
		_, err := c.journal.ExpireRuns(collectAt)
		return err
	}
	if err := collect(); err != nil {
		c.pendingRunWrite = collect
		errs = append(errs, fmt.Errorf("session: collecting reset boss reward runs: %w", err))
	}
	return errors.Join(errs...)
}

// rewardRunIdentity is what names a run across restarts, exactly as the restore overlay
// keys it: the runtime id is remapped, while the seed, ruin and reset are not.
type rewardRunIdentity struct {
	seed    int64
	ruin    [2]int64
	expires int64
}

// rewardRunPass is one pass's view of the journal, kept current by the writes the pass
// makes so that nothing is read twice.
type rewardRunPass struct {
	byGeneration map[uint64]persist.RewardRun
	byIdentity   map[rewardRunIdentity]uint64
	held         map[uint64]bool // generations a saved run already holds
	next         uint64
}

func (i *Identities) syncRewardRun(c *rewardCoordinator, pass *rewardRunPass, run game.SavedSession) error {
	bound := make([]persist.SessionCharacter, len(run.Bound))
	for k, who := range run.Bound {
		bound[k] = persist.SessionCharacter{PlayerID: who.PlayerID, CharacterID: who.CharacterID}
	}

	generation := run.Generation
	if generation == 0 {
		identity := rewardRunIdentity{run.Seed, [2]int64{run.Ruin.CellX, run.Ruin.CellZ}, run.ExpiresUnix}
		if adopted, journaled := pass.byIdentity[identity]; journaled {
			if pass.held[adopted] {
				return fmt.Errorf("%w: generation %d already belongs to another saved run", persist.ErrRewardJournalConflict, adopted)
			}
			generation = adopted
		} else {
			record := persist.SessionRecord{
				ID: run.ID, Seed: run.Seed, Ruin: identity.ruin, ExpiresUnix: run.ExpiresUnix,
				DefeatedBosses: slices.Clone(run.DefeatedBosses), Bound: slices.Clone(bound),
			}
			allocated := pass.next
			allocate := func() error {
				return c.journal.AllocateRun(i.store, allocated, record, world.WorldgenVersion)
			}
			if err := allocate(); err != nil {
				c.pendingRunWrite = allocate
				return err
			}
			pass.next++
			stored := persist.RewardRun{Generation: allocated, Session: record}
			for _, kind := range record.DefeatedBosses {
				stored.Defeats = append(stored.Defeats, persist.RewardDefeat{Kind: kind})
			}
			pass.byGeneration[allocated] = stored
			pass.byIdentity[identity] = allocated
			generation = allocated
		}
		pass.held[generation] = true
		// A refusal leaves the journal run in place, and the next pass adopts it by identity
		// rather than allocating the same run a second generation.
		c.assignRunGeneration(run, generation)
	}

	stored, journaled := pass.byGeneration[generation]
	if !journaled {
		return fmt.Errorf("%w: generation %d is not in the journal", persist.ErrRewardJournalConflict, generation)
	}
	appended := false
	for _, kind := range run.DefeatedBosses {
		if slices.ContainsFunc(stored.Defeats, func(d persist.RewardDefeat) bool { return d.Kind == kind }) {
			continue
		}
		defeat := persist.RewardDefeat{Kind: kind}
		write := func() error { return c.journal.AppendDefeat(generation, defeat, bound) }
		if err := write(); err != nil {
			c.pendingRunWrite = write
			return err
		}
		stored.Defeats = append(stored.Defeats, defeat)
		appended = true
	}
	missingBinding := slices.ContainsFunc(bound, func(o persist.SessionCharacter) bool { return !slices.Contains(stored.Session.Bound, o) })
	if appended || !missingBinding || len(stored.Defeats) == 0 {
		// A new defeat already unioned the bindings it was appended with.
		return nil
	}
	// A binding with no new defeat is unioned through the run's latest defeat exactly as
	// stored, which AppendDefeat accepts as a retry that adds only the binding.
	latest := stored.Defeats[len(stored.Defeats)-1]
	write := func() error { return c.journal.AppendDefeat(generation, latest, bound) }
	if err := write(); err != nil {
		c.pendingRunWrite = write
		return err
	}
	return nil
}

func (c *rewardCoordinator) assignRunGeneration(run game.SavedSession, generation uint64) bool {
	if c.assignRun != nil {
		return c.assignRun(run, generation)
	}
	return c.manager.AssignRunGeneration(run, generation)
}

// ValidateRewardRecord is the game's judgement of a character record that a prepared boss
// reward would restore at startup, for persist.OpenStoreWithRewardRecovery. The store
// checks what a file can be wrong about; this checks what a life can be.
func ValidateRewardRecord(rec persist.Record) error {
	life := lifeOfRecord(rec)
	return life.Validate()
}
