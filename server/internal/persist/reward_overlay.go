package persist

import (
	"cmp"
	"math"
	"slices"
)

// RestoredRewardSession keeps the durable generation distinct from its runtime
// session ID. A remap changes neither seed nor ruin nor expiry; consumers must retain
// this association when routing later defeats/claims back into the journal.
type RestoredRewardSession struct {
	Generation uint64
	Session    SessionRecord
}
type rewardRunIdentity struct {
	seed    int64
	ruin    [2]int64
	expires int64
}

func rewardRunKey(s SessionRecord) rewardRunIdentity {
	return rewardRunIdentity{s.Seed, s.Ruin, s.ExpiresUnix}
}

// OverlaySessions is a pure startup primitive. It overlays durable defeated state
// onto older or missing sessions.bin records, retains later bindings by union and
// excludes expired generations from live restoration (their intents/XP stay in the
// journal). Runtime IDs are remapped deterministically when independent runs collide.
// An alias is matched on procedural seed/ruin/expiry on the next restart; ambiguous
// active generations with that same identity are refused instead of conflated.
func (s *RewardStore) OverlaySessions(saved []SessionRecord, now int64, content uint32) ([]RestoredRewardSession, error) {
	j, err := s.Snapshot()
	if err != nil {
		return nil, err
	}
	if len(saved) > MaxSavedSessions {
		return nil, ErrRewardJournalConflict
	}
	byKey := make(map[rewardRunIdentity]RestoredRewardSession)
	maxID := uint64(0)
	for _, rec := range saved {
		maxID = max(maxID, rec.ID)
		if rec.ExpiresUnix <= now {
			continue
		}
		if rec.ID == 0 || len(rec.Bound) > MaxBoundCharacters || len(rec.DefeatedBosses) > MaxDefeatedBosses {
			return nil, ErrRewardJournalConflict
		}
		key := rewardRunKey(rec)
		if _, duplicate := byKey[key]; duplicate {
			return nil, ErrRewardJournalConflict
		}
		rec.Bound = append([]SessionCharacter(nil), rec.Bound...)
		rec.DefeatedBosses = append(rec.DefeatedBosses[:0:0], rec.DefeatedBosses...)
		byKey[key] = RestoredRewardSession{Session: rec}
	}
	seen := make(map[rewardRunIdentity]bool)
	for _, run := range j.Runs {
		maxID = max(maxID, run.Session.ID)
		if run.Session.ExpiresUnix <= now {
			continue
		}
		if run.ContentVersion != content {
			return nil, ErrRewardRecoveryRequired
		}
		key := rewardRunKey(run.Session)
		if seen[key] {
			return nil, ErrRewardJournalConflict
		}
		seen[key] = true
		prior := byKey[key]
		rec := run.Session
		// Journal defeats are authoritative. Never import a not-yet-durable defeat from
		// sessions.bin, which may have been written independently before the journal.
		rec.DefeatedBosses = rewardDefeatedKinds(run)
		for _, o := range prior.Session.Bound {
			if !slices.Contains(rec.Bound, o) {
				rec.Bound = append(rec.Bound, o)
			}
		}
		if len(rec.Bound) > MaxBoundCharacters {
			return nil, ErrRewardJournalConflict
		}
		if prior.Session.ID != 0 {
			rec.ID = prior.Session.ID
		}
		byKey[key] = RestoredRewardSession{Generation: run.Generation, Session: rec}
	}
	if len(byKey) > MaxSavedSessions {
		return nil, ErrRewardJournalConflict
	}
	out := make([]RestoredRewardSession, 0, len(byKey))
	for _, rec := range byKey {
		out = append(out, rec)
	}
	slices.SortFunc(out, func(a, b RestoredRewardSession) int {
		if c := cmp.Compare(a.Generation, b.Generation); c != 0 {
			return c
		}
		if c := cmp.Compare(a.Session.ID, b.Session.ID); c != 0 {
			return c
		}
		if c := cmp.Compare(a.Session.Seed, b.Session.Seed); c != 0 {
			return c
		}
		if c := cmp.Compare(a.Session.Ruin[0], b.Session.Ruin[0]); c != 0 {
			return c
		}
		if c := cmp.Compare(a.Session.Ruin[1], b.Session.Ruin[1]); c != 0 {
			return c
		}
		return cmp.Compare(a.Session.ExpiresUnix, b.Session.ExpiresUnix)
	})
	used := make(map[uint64]bool, len(out))
	for i := range out {
		id := out[i].Session.ID
		if used[id] {
			if maxID == math.MaxUint64 {
				return nil, ErrRewardJournalConflict
			}
			maxID++
			out[i].Session.ID = maxID
		}
		used[out[i].Session.ID] = true
	}
	return out, nil
}
