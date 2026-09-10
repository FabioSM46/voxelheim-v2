package game

// AssignRunGeneration records the boss reward journal generation durably allocated for a
// saved run. It applies only to the exact run the allocation described: a run that has
// reset, a reused runtime id now naming another run, or a run that already holds a
// generation is left untouched, and the answer is false.
//
// A runtime id names one process's session; a generation names the run for as long as
// the journal keeps it, which is what lets a later defeat reach the same durable record
// after a restart remaps the id.
func (m *InstanceManager) AssignRunGeneration(run SavedSession, generation uint64) bool {
	if generation == 0 {
		return false
	}
	m.mu.Lock()
	defer m.mu.Unlock()
	s := m.sessions[run.ID]
	if s == nil || s.state != InstanceSaved || s.generation != 0 ||
		s.seed != run.Seed || s.ruin != run.Ruin || s.expiresUnix != run.ExpiresUnix {
		return false
	}
	s.generation = generation
	return true
}
