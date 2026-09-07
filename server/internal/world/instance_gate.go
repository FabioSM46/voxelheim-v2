package world

// InstanceGate is the one-way progression door in an ephemeral dungeon. Its
// state belongs to this cache, never to disk or the open-world edit layer.
// composeMu protects open and every publication, including a generation that
// began before Open. Callers receive copied coordinates, never writable state.
type InstanceGate struct {
	cache *Cache
	cells []PlacedAnchor
	open  bool
}

func NewGatedInstanceCache(seed int64, workers, capacity int, open bool) (*Cache, *InstanceGate) {
	c := NewInstanceCache(seed, workers, capacity)
	_, _, centre := InstanceEncounterAnchors(seed)
	g := &InstanceGate{cache: c, open: open}
	dx, dz := int64(1), int64(0)
	if uint64(seed)&1 != 0 {
		dx, dz = 0, 1
	}
	for offset := int64(-2); offset <= 2; offset++ {
		for y := int64(1); y <= 5; y++ {
			g.cells = append(g.cells, PlacedAnchor{X: centre.X + dx*offset, Y: y, Z: centre.Z + dz*offset, Kind: AnchorInstanceGate})
		}
	}
	c.instanceGate = g
	interior := c.editable
	c.editable = func(x, y, z int64) bool {
		for _, p := range g.cells {
			if x == p.X && y == p.Y && z == p.Z {
				return false
			}
		}
		return interior(x, y, z)
	}
	return c, g
}

// patch is called under composeMu on a private, unpublished chunk. Applying the
// door last prevents an edit, eviction or regeneration from bypassing progress.
func (g *InstanceGate) patch(c *Chunk) {
	if g == nil {
		return
	}
	block := BlackBrick
	if g.open {
		block = Air
	}
	for _, p := range g.cells {
		if ChunkOf(p.X, p.Y, p.Z) == c.Coord {
			c.Set(Local(p.X), Local(p.Y), Local(p.Z), block)
		}
	}
}

// Open publishes at most two resident chunk copies, without generation or I/O.
// The returned cells are the authoritative changes clients must be told about.
// Calling it twice changes nothing and returns no duplicate event.
func (g *InstanceGate) Open() []PlacedAnchor {
	c := g.cache
	c.composeMu.Lock()
	defer c.composeMu.Unlock()
	if g.open {
		return nil
	}
	g.open = true
	visited := make(map[Coord]bool)
	for _, p := range g.cells {
		coord := ChunkOf(p.X, p.Y, p.Z)
		if visited[coord] {
			continue
		}
		visited[coord] = true
		entry := c.resident(coord)
		if entry == nil {
			continue
		}
		current := entry.composed.Load()
		if current == nil {
			continue
		}
		patched := current.chunk.Clone()
		g.patch(patched)
		entry.composed.Store(&composition{chunk: patched, encoded: Encode(patched)})
	}
	c.revision.Add(1)
	return append([]PlacedAnchor(nil), g.cells...)
}
