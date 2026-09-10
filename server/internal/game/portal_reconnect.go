package game

// A disconnected visit holds values only: expiry must release the simulation and
// cache even when its former members never reconnect. Expiry evicts this snapshot;
// the existing character record already holds the safe open-world return point.
// No session identity reaches disk.
type portalReconnect struct {
	session   uint64
	life      Life
	returnPos [3]float32
	respawn   [3]float64
}

// DisconnectPortal remembers the last authoritative life and releases occupancy.
// Call after Sim.Leave and after session workers have stopped mutating the player,
// but before releasing the account claim. An offline character never holds grace.
func (m *InstanceManager) DisconnectPortal(entry PortalEntry, life Life) {
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.closed {
		return
	}
	if m.inside[entry.Character] != entry.Session.ID {
		return
	}
	m.disconnected[entry.Character] = portalReconnect{session: entry.Session.ID, life: life, returnPos: entry.Return, respawn: entry.respawn}
	s := m.sessions[entry.Session.ID]
	delete(s.members, entry.Character)
	delete(m.inside, entry.Character)
	delete(m.portalEntries, entry.Character)
	if len(s.members) == 0 {
		s.emptyTicks = 0
	}
}

// ReplaceDisconnectedLife swaps a remembered visit's life for its successor only while the
// remembered value is still exactly previous, so an expired or resumed visit is never
// recreated and a newer life is never overwritten. It reports whether it replaced one.
func (m *InstanceManager) ReplaceDisconnectedLife(character InstanceCharacter, previous, next Life) bool {
	m.mu.Lock()
	defer m.mu.Unlock()
	visit, found := m.disconnected[character]
	if !found || visit.life != previous {
		return false
	}
	visit.life = next
	m.disconnected[character] = visit
	return true
}

// ResumePortal claims a remembered live copy atomically with respect to expiry.
// With no live remembered visit both answers are nil: the caller uses the safe
// open-world character record it loaded during character selection.
// Authentication and character ownership must have been resolved by the caller.
func (m *InstanceManager) ResumePortal(character InstanceCharacter) (*Life, *PortalEntry, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	recalled, found := m.disconnected[character]
	if !found {
		return nil, nil, nil
	}
	life := recalled.life
	if s := m.sessions[recalled.session]; s != nil && !m.closed {
		if err := m.joinRememberedLocked(s, character); err != nil {
			return nil, nil, err
		}
		entry := PortalEntry{Session: s.snapshot(), Character: character, Return: recalled.returnPos, respawn: recalled.respawn}
		m.portalEntries[character] = entry
		delete(m.disconnected, character)
		return &life, &entry, nil
	}
	delete(m.disconnected, character)
	return nil, nil, nil
}

// Records captures connected characters across the open world and every instance
// under the manager -> shared simulation lock order. Transfers cannot slip between
// choosing a world and capturing its positions, and an account is captured once.
// Instance coordinates are replaced before returning anything to persistence.
// All simulations must belong to the open world's WorldGroup, as in server setup.
func (m *InstanceManager) Records(open *Sim) map[InstanceCharacter]Life {
	m.mu.Lock()
	defer m.mu.Unlock()
	open.mu.Lock()
	defer open.mu.Unlock()
	records := make(map[InstanceCharacter]Life)
	capture := func(s *Sim) {
		for _, p := range s.players {
			entry, known := m.portalEntries[InstanceCharacter{p.playerID, p.characterID}]
			if s != open && (!known || s != entry.Session.Sim) {
				// An external transfer has no portal return point. Preserve the
				// previous disk record rather than write foreign coordinates.
				continue
			}
			p.inventory.mu.Lock()
			life := p.recordLocked()
			p.inventory.mu.Unlock()
			if s != open {
				for axis, value := range entry.Return {
					life.Pos[axis] = float64(value)
				}
			}
			records[InstanceCharacter{p.playerID, p.characterID}] = life
		}
	}
	capture(open)
	for _, s := range m.sessions {
		capture(s.sim)
	}
	return records
}
