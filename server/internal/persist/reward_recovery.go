package persist

import (
	"errors"
	"fmt"
	"io/fs"
	"path/filepath"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// OpenStoreWithRewardRecovery is an exclusive STARTUP primitive, not a live repair
// API. Production startup remains on OpenStore plus CheckInactive until all reward
// consumers are ready. The required validator supplies game registry/life semantics
// without making persist import game. No index, login or ordinary writer exists yet.
func OpenStoreWithRewardRecovery(dir string, rewards *RewardStore, validate func(Record) error) (*Store, error) {
	if rewards == nil || validate == nil {
		return nil, ErrRewardRecoveryRequired
	}
	expected, err := filepath.Abs(filepath.Join(dir, rewardFileName))
	if err != nil {
		return nil, err
	}
	actual, err := filepath.Abs(rewards.path)
	if err != nil || actual != expected {
		return nil, ErrRewardRecoveryRequired
	}
	rewards.mu.Lock()
	defer rewards.mu.Unlock()
	return openPlayerStore(dir, func(s *Store) error {
		s.strictRewards.Store(rewards.journal.NextGeneration > 1)
		return rewards.recoverRecordsLocked(s, validate)
	})
}

// All entitlements and postimages are validated before the first replacement.
// A corrupt/missing record can only be repaired by its one validated unresolved
// intent. Without that proof, strict receipt mode refuses rather than starting fresh.
func (j *RewardStore) recoverRecordsLocked(players *Store, validate func(Record) error) error {
	if err := validateRewardJournal(j.journal); err != nil {
		return err
	}
	intents := make(map[CharacterID]RewardIntent, len(j.journal.Intents))
	for _, in := range j.journal.Intents {
		if err := validate(in.Postimage); err != nil {
			return fmt.Errorf("%w: invalid reward postimage: %w", ErrRewardRecoveryRequired, err)
		}
		intents[in.Postimage.Character] = in
	}
	owners := make(map[CharacterID]SessionCharacter)
	addOwner := func(o SessionCharacter) error {
		id := CharacterID(o.CharacterID)
		if old, ok := owners[id]; ok && old != o {
			return ErrRewardRecoveryRequired
		}
		owners[id] = o
		return nil
	}
	for _, run := range j.journal.Runs {
		for _, o := range run.Session.Bound {
			if err := addOwner(o); err != nil {
				return err
			}
		}
		for _, d := range run.Defeats {
			for _, p := range d.Personal {
				if err := addOwner(p.Owner); err != nil {
					return err
				}
			}
			for _, xp := range d.Experience {
				if err := addOwner(xp.Owner); err != nil {
					return err
				}
			}
		}
	}
	repairs := make(map[CharacterID]Record, len(intents))
	for id, owner := range owners {
		rec, found, err := players.Load(id)
		in, prepared := intents[id]
		if err != nil && !errors.Is(err, fs.ErrNotExist) && !errors.Is(err, world.ErrCorruptStore) {
			return err
		}
		if err != nil || !found {
			if !prepared {
				return ErrRewardRecoveryRequired
			}
			repairs[id] = in.Postimage
			continue
		}
		if rec.Character != id || rec.Owner != owner.PlayerID {
			return ErrRewardRecoveryRequired
		}
		if !prepared {
			continue
		}
		if !sameRewardIdentity(rec, in.Postimage) {
			return ErrRewardRecoveryRequired
		}
		if rec.BossRewardEpoch >= in.Postimage.BossRewardEpoch {
			if err := validate(rec); err != nil {
				return fmt.Errorf("%w: later reward record is invalid: %w", ErrRewardRecoveryRequired, err)
			}
			repairs[id] = rec // preserve later state exactly, but reconfirm directory durability before ack
			continue
		}
		repairs[id] = in.Postimage
	}
	// Use the original intent order for deterministic retries, independently of maps.
	for _, in := range j.journal.Intents {
		record, repair := repairs[in.Postimage.Character]
		if !repair {
			continue
		}
		c := Character{ID: record.Character, Owner: record.Owner, Name: record.Name, Appearance: record.Appearance}
		if err := players.writeRecord(c, record); err != nil {
			return err
		}
	}
	if len(j.journal.Intents) == 0 {
		return nil
	}
	next, err := cloneRewardJournal(j.journal)
	if err != nil {
		return err
	}
	acknowledged := append([]RewardIntent(nil), j.journal.Intents...)
	for _, in := range acknowledged {
		if err := markRewardAcknowledged(&next, in); err != nil {
			return err
		}
	}
	next.Revision++
	return j.commitLocked(j.journal.Revision, next, acknowledged)
}
