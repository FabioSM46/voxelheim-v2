package persist

import "slices"

// ExpireRuns is the journal's collection at the daily reset. It removes each run whose
// reset has passed, unless a prepared intent still names its generation, which means expiry
// never discards an unresolved claim, or retain names it: a run that players are still
// inside keeps its journal run past midnight. NextGeneration is untouched, so a collected
// generation is never issued again. It reports how many runs it removed; a pass with nothing
// to collect writes nothing.
func (s *RewardStore) ExpireRuns(now int64, retain ...uint64) (int, error) {
	if s == nil {
		return 0, ErrRewardJournalConflict
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	pending := make(map[uint64]bool, len(s.journal.Intents))
	for _, intent := range s.journal.Intents {
		pending[intent.Generation] = true
	}
	next, err := cloneRewardJournal(s.journal)
	if err != nil {
		return 0, err
	}
	kept := next.Runs[:0]
	for _, run := range next.Runs {
		if run.Session.ExpiresUnix > now || pending[run.Generation] || slices.Contains(retain, run.Generation) {
			kept = append(kept, run)
		}
	}
	removed := len(next.Runs) - len(kept)
	if removed == 0 {
		return 0, nil
	}
	next.Runs = kept
	next.Revision++
	if err := s.commitLocked(s.journal.Revision, next, nil); err != nil {
		return 0, err
	}
	return removed, nil
}
