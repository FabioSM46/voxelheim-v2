package world

import "fmt"

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
	return chamberLayout.gated(seed, workers, capacity, open)
}

// gated places a five-by-five door centred on the layout's gate anchor: upright, its
// bottom course on the anchor's level, or — for a floorGate layout — flat, a trapdoor
// filling the anchor's own course. The cells are world coordinates, so a door that
// straddles a chunk boundary in any direction — vertical included — is patched into
// every chunk it touches.
func (l instanceLayout) gated(seed int64, workers, capacity int, open bool) (*Cache, *InstanceGate) {
	c := l.cache(seed, workers, capacity)
	// Exactly one gate anchor, or the layout is a programming error: a missing one
	// would otherwise put the door at the world origin, in some other chunk.
	var centre PlacedAnchor
	gates := 0
	for _, a := range l.placement(seed).Anchors {
		if a.Kind == AnchorInstanceGate {
			centre = a
			gates++
		}
	}
	if gates != 1 {
		panic(fmt.Sprintf("instance layout has %d gate anchors, want exactly one", gates))
	}
	g := &InstanceGate{cache: c, open: open}
	dx, dz := int64(1), int64(0)
	if uint64(seed)&1 != 0 {
		dx, dz = 0, 1
	}
	for offset := int64(-2); offset <= 2; offset++ {
		for across := int64(-2); across <= 2; across++ {
			cell := PlacedAnchor{X: centre.X + dx*offset, Y: centre.Y + across + 2, Z: centre.Z + dz*offset, Kind: AnchorInstanceGate}
			if l.floorGate {
				cell = PlacedAnchor{X: centre.X + offset, Y: centre.Y, Z: centre.Z + across, Kind: AnchorInstanceGate}
			}
			g.cells = append(g.cells, cell)
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
