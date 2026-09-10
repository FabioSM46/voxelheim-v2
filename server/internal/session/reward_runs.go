package session

import (
	"errors"
	"fmt"
	"slices"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/persist"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// SyncRewardRuns makes the boss reward journal hold every saved run's defeated progress,
// bindings and frozen personal loot, releases that loot to claims once it is durable, and
// collects the runs nobody holds past their reset (#1036).
//
// **What a defeat owes.** A defeat the manager still reports as pending is written with each
// loot-roster owner's frozen roll, and only after that write lands is the loot released:
// from then on a take becomes a claim. Boss experience keeps its live award path and is not
// journaled, so no build can offer it twice.
//
// One snapshot is read per pass, and only what is missing is written:
//   - A saved run without a generation is allocated one with its current defeats, bindings
//     and frozen loot in the same write.
//   - A run the journal already holds under the same seed, ruin and reset is adopted rather
//     than allocated again. Two live runs with one identity would make the restore overlay
//     refuse startup, and adoption also repairs an allocation the manager did not accept.
//   - Later defeats are appended in death order with what they owe, and a later binding is
//     unioned.
//
// **Midnight does not take a run from under the players inside it.** The manager keeps an
// occupied run past its reset, so every run it still reports stays journaled and is
// retained; a run is collected only once the manager has let it go.
//
// Every write goes through the gate claims share, so a claim write and a sync write never
// interleave while either left the journal uncertain. A sync write that did is retried
// verbatim before anything else. No simulation or manager lock is held across a write.
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
		if err := c.journalWrite(&c.syncMu, c.pendingRunWrite); err != nil {
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
	saved := c.runs.SavedSessions()
	for _, run := range saved {
		if run.Generation != 0 {
			pass.held[run.Generation] = true
		}
	}

	var errs []error
	for _, run := range saved {
		if err := i.syncRewardRun(c, &pass, run); err != nil {
			errs = append(errs, fmt.Errorf("session: boss reward run %d: %w", run.ID, err))
			if c.pendingRunWrite != nil || errors.Is(err, errJournalBusy) {
				return errors.Join(errs...)
			}
		}
	}
	retain := make([]uint64, 0, len(pass.held))
	for generation := range pass.held {
		retain = append(retain, generation)
	}
	// Sorted, so a collection retried verbatim writes the same bytes.
	slices.Sort(retain)
	collectAt := now.Unix()
	collect := func() error {
		_, err := c.journal.ExpireRuns(collectAt, retain...)
		return err
	}
	if err := c.runWrite(collect); err != nil {
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
			defeats := make([]persist.RewardDefeat, len(run.DefeatedBosses))
			for k, kind := range run.DefeatedBosses {
				defeats[k] = journalDefeatOf(run, kind)
			}
			allocated := pass.next
			allocate := func() error {
				return c.journal.AllocateRun(i.store, allocated, record, world.WorldgenVersion, defeats...)
			}
			if err := c.runWrite(allocate); err != nil {
				return err
			}
			pass.next++
			pass.byGeneration[allocated] = persist.RewardRun{Generation: allocated, Session: record, Defeats: defeats}
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
	var errs []error
	appended := false
	for _, kind := range run.DefeatedBosses {
		defeat := journalDefeatOf(run, kind)
		at := slices.IndexFunc(stored.Defeats, func(d persist.RewardDefeat) bool { return d.Kind == kind })
		if at < 0 {
			write := func() error { return c.journal.AppendDefeat(generation, defeat, bound) }
			if err := c.runWrite(write); err != nil {
				return err
			}
			stored.Defeats = append(stored.Defeats, defeat)
			at = len(stored.Defeats) - 1
			appended = true
		}
		if !slices.ContainsFunc(run.PendingRewards, func(p game.BossRewardDefeat) bool { return p.Kind == kind }) {
			continue
		}
		// The loot is released only when the journal owes exactly what was frozen. A defeat
		// journaled with anything else keeps its corpse held rather than risk a second roll.
		if !samePersonalRewards(stored.Defeats[at].Personal, defeat.Personal) {
			errs = append(errs, fmt.Errorf("%w: boss %s is journaled with other entitlements", persist.ErrRewardJournalConflict, kind))
			continue
		}
		c.runs.ReleaseBossRewards(run, kind)
	}
	pass.byGeneration[generation] = stored

	missingBinding := slices.ContainsFunc(bound, func(o persist.SessionCharacter) bool { return !slices.Contains(stored.Session.Bound, o) })
	if appended || !missingBinding || len(stored.Defeats) == 0 {
		// A new defeat already unioned the bindings it was appended with.
		return errors.Join(errs...)
	}
	// A binding with no new defeat is unioned through the run's latest defeat exactly as
	// stored, which AppendDefeat accepts as a retry that adds only the binding.
	latest := stored.Defeats[len(stored.Defeats)-1]
	write := func() error { return c.journal.AppendDefeat(generation, latest, bound) }
	if err := c.runWrite(write); err != nil {
		errs = append(errs, err)
	}
	return errors.Join(errs...)
}

// journalDefeatOf is one defeat as the journal records it: the frozen personal rolls the
// manager still reports as pending for that boss, or progress alone when there are none.
func journalDefeatOf(run game.SavedSession, kind vnet.MobKind) persist.RewardDefeat {
	defeat := persist.RewardDefeat{Kind: kind}
	for _, pending := range run.PendingRewards {
		if pending.Kind != kind {
			continue
		}
		for _, reward := range pending.Personal {
			defeat.Personal = append(defeat.Personal, persist.PersonalReward{
				Owner:   persist.SessionCharacter{PlayerID: reward.Owner.PlayerID, CharacterID: reward.Owner.CharacterID},
				Entries: slices.Clone(reward.Entries), Silver: reward.Silver,
			})
		}
		break
	}
	return defeat
}

func samePersonalRewards(stored, frozen []persist.PersonalReward) bool {
	return slices.EqualFunc(stored, frozen, func(a, b persist.PersonalReward) bool {
		return a.Owner == b.Owner && a.Silver == b.Silver && slices.Equal(a.Entries, b.Entries)
	})
}

// runWrite performs one sync write through the journal gate. A write that leaves the journal
// uncertain is kept, to be retried verbatim before any other.
func (c *rewardCoordinator) runWrite(op func() error) error {
	err := c.journalWrite(&c.syncMu, op)
	if err != nil && !errors.Is(err, errJournalBusy) && c.journal.Uncertain() {
		c.pendingRunWrite = op
	}
	return err
}

func (c *rewardCoordinator) assignRunGeneration(run game.SavedSession, generation uint64) bool {
	if c.assignRun != nil {
		return c.assignRun(run, generation)
	}
	return c.runs.AssignRunGeneration(run, generation)
}

// ValidateRewardRecord is the game's judgement of a character record that a prepared boss
// reward would restore at startup, for persist.OpenStoreWithRewardRecovery. The store
// checks what a file can be wrong about; this checks what a life can be.
func ValidateRewardRecord(rec persist.Record) error {
	life := lifeOfRecord(rec)
	return life.Validate()
}
