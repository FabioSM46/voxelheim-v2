package persist

// Uncertain reports whether the journal holds a write whose outcome is unknown. Until that
// exact write is retried successfully the journal refuses to commit any other bytes, so
// writers that share one journal must not interleave while it reports true.
func (s *RewardStore) Uncertain() bool {
	if s == nil {
		return false
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.uncertain != nil
}
