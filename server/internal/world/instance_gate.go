package world

import (
	"fmt"
	"slices"
)

// InstanceGate is every cell of an ephemeral dungeon whose block the simulation
// decides rather than the drawing: the one-way trapdoor the guardian's defeat opens,
// the doors keyed by their anchors' index, and the mechanism cells — levers and rune
// stones — the puzzles are operated through. Its state belongs to this cache, never
// to disk or the open-world edit layer.
//
// **Every one of those cells is written last, over whatever composition produced.**
// A generation, an eviction and a regeneration all rebuild a chunk from the seed, and
// patch runs under composeMu after the edit layer on every one of them, so no path
// that rebuilds a chunk can show a door the simulation opened as shut, a lit rune as
// dark, or the reverse. No player edit reaches any of these cells either: the cache's
// edit rule refuses them open or shut, lit or dark.
//
// composeMu protects every mutable field below and every publication, including a
// generation that began before a change. Callers receive copies, never writable state.
type InstanceGate struct {
	cache *Cache
	cells []PlacedAnchor
	open  bool

	// doors is every door anchor grouped by its index, in declaration order, and
	// doorOf names the door each door cell belongs to.
	doors  map[int][]PlacedAnchor
	doorOf map[[3]int64]int
	// opened is the set of door indices that are open now.
	opened map[int]bool
	// mechanisms is every mechanism anchor, in declaration order.
	mechanisms []PlacedAnchor
	// drawn is what the drawing and the seed's overlay put in each door and mechanism
	// cell: a shut door, an unpulled lever, a dark rune.
	drawn map[[3]int64]Block
	// shown is what each mechanism cell shows now.
	shown map[[3]int64]Block
	// byChunk lists the door and mechanism cells in each chunk, so a patch visits only
	// its own.
	byChunk map[Coord][][3]int64
}

// InstanceCell is one cell the simulation changed and the block it now holds: the
// authoritative change clients must be told about.
type InstanceCell struct {
	X, Y, Z int64
	Block   Block
}

// InstanceUpdate is one atomic change to a dungeon's doors and mechanisms. Doors maps
// a door index to whether it is open; Mechanisms names mechanism cells and the block
// each should show — a lever up or down, a rune dark or lit.
type InstanceUpdate struct {
	Doors      map[int]bool
	Mechanisms []InstanceCell
}

func NewGatedInstanceCache(seed int64, workers, capacity int, open bool) (*Cache, *InstanceGate) {
	return dungeonLayout.gated(seed, workers, capacity, open)
}

// gated places a five-by-five door centred on the layout's gate anchor: upright, its
// bottom course on the anchor's level, or — for a floorGate layout — flat, a trapdoor
// filling the anchor's own course. The cells are world coordinates, so a door that
// straddles a chunk boundary in any direction — vertical included — is patched into
// every chunk it touches.
//
// It also takes over every door and mechanism anchor the layout declares, each
// starting as the drawing has it: doors shut, levers down, runes dark.
func (l instanceLayout) gated(seed int64, workers, capacity int, open bool) (*Cache, *InstanceGate) {
	c := l.cache(seed, workers, capacity)
	b := l.placement(seed)
	// Exactly one gate anchor, or the layout is a programming error: a missing one
	// would otherwise put the door at the world origin, in some other chunk.
	var centre PlacedAnchor
	gates := 0
	for _, a := range b.Anchors {
		if a.Kind == AnchorInstanceGate {
			centre = a
			gates++
		}
	}
	if gates != 1 {
		panic(fmt.Sprintf("instance layout has %d gate anchors, want exactly one", gates))
	}
	g := &InstanceGate{
		cache: c, open: open,
		doors: make(map[int][]PlacedAnchor), doorOf: make(map[[3]int64]int), opened: make(map[int]bool),
		drawn: make(map[[3]int64]Block), shown: make(map[[3]int64]Block),
		byChunk: make(map[Coord][][3]int64),
	}
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
	for _, a := range b.Anchors {
		if a.Kind != AnchorInstanceDoor && a.Kind != AnchorInstanceMechanism {
			continue
		}
		key := [3]int64{a.X, a.Y, a.Z}
		g.drawn[key] = l.drawnAt(seed, b, a.X, a.Y, a.Z)
		coord := ChunkOf(a.X, a.Y, a.Z)
		g.byChunk[coord] = append(g.byChunk[coord], key)
		if a.Kind == AnchorInstanceDoor {
			g.doors[a.Index] = append(g.doors[a.Index], a)
			g.doorOf[key] = a.Index
		} else {
			g.mechanisms = append(g.mechanisms, a)
			g.shown[key] = g.drawn[key]
		}
	}
	c.instanceGate = g
	fixed := make(map[[3]int64]bool, len(g.cells))
	for _, p := range g.cells {
		fixed[[3]int64{p.X, p.Y, p.Z}] = true
	}
	interior := c.editable
	c.editable = func(x, y, z int64) bool {
		key := [3]int64{x, y, z}
		if _, tracked := g.drawn[key]; tracked || fixed[key] {
			return false
		}
		return interior(x, y, z)
	}
	return c, g
}

// drawnAt is the block generate puts in one world cell of the placed drawing: the
// drawing's own unless the seed's overlay wrote over it, turned with the drawing.
func (l instanceLayout) drawnAt(seed int64, b Building, x, y, z int64) Block {
	lx, ly, lz, ok := l.localIn(b, x, y, z)
	if !ok {
		return Air
	}
	block := l.drawing.At(lx, ly, lz)
	if block == keepTerrain {
		block = Air
	}
	if l.overlay != nil {
		for _, cell := range l.overlay(seed) {
			if cell.x == lx && cell.y == ly && cell.z == lz {
				block = cell.block
			}
		}
	}
	return rotateSchematicBlock(block, b.Facing)
}

// blockAt is what a door or mechanism cell holds now: a mechanism's shown block, air
// in an open door, the drawn door in a shut one. The caller holds composeMu.
func (g *InstanceGate) blockAt(key [3]int64) Block {
	if shown, mechanism := g.shown[key]; mechanism {
		return shown
	}
	if g.opened[g.doorOf[key]] {
		return Air
	}
	return g.drawn[key]
}

// patch is called under composeMu on a private, unpublished chunk. Applying the
// trapdoor, the doors and the mechanisms last prevents an edit, eviction or
// regeneration from bypassing progress.
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
	for _, key := range g.byChunk[c.Coord] {
		c.Set(Local(key[0]), Local(key[1]), Local(key[2]), g.blockAt(key))
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
	coords := make([]Coord, 0, len(g.cells))
	for _, p := range g.cells {
		coords = append(coords, ChunkOf(p.X, p.Y, p.Z))
	}
	g.republish(coords)
	return append([]PlacedAnchor(nil), g.cells...)
}

// Update applies one change to the doors and mechanisms and publishes every resident
// chunk it touches, without generation or I/O and under one hold of composeMu, so a
// reader sees the change whole or not at all. It returns the cells whose block
// actually changed, in a stable order — doors by ascending index, each in declaration
// order, then the mechanisms in the order given — so an update that changes nothing
// returns nothing and publishes nothing.
//
// Naming a door index the layout does not have, a cell that is not a mechanism anchor,
// or a block no mechanism shows is a programming error and panics: a caller derives
// all three from this layout's own anchors.
func (g *InstanceGate) Update(u InstanceUpdate) []InstanceCell {
	c := g.cache
	c.composeMu.Lock()
	defer c.composeMu.Unlock()

	indices := make([]int, 0, len(u.Doors))
	for index := range u.Doors {
		if _, known := g.doors[index]; !known {
			panic(fmt.Sprintf("instance layout has no door %d", index))
		}
		indices = append(indices, index)
	}
	slices.Sort(indices)
	for _, cell := range u.Mechanisms {
		if _, known := g.shown[[3]int64{cell.X, cell.Y, cell.Z}]; !known || !Mechanism(cell.Block) {
			panic(fmt.Sprintf("instance cell %d,%d,%d is not a mechanism that can show block %d", cell.X, cell.Y, cell.Z, cell.Block))
		}
	}

	var changed []InstanceCell
	var coords []Coord
	for _, index := range indices {
		if g.opened[index] == u.Doors[index] {
			continue
		}
		g.opened[index] = u.Doors[index]
		for _, p := range g.doors[index] {
			changed = append(changed, InstanceCell{X: p.X, Y: p.Y, Z: p.Z, Block: g.blockAt([3]int64{p.X, p.Y, p.Z})})
			coords = append(coords, ChunkOf(p.X, p.Y, p.Z))
		}
	}
	for _, cell := range u.Mechanisms {
		key := [3]int64{cell.X, cell.Y, cell.Z}
		if g.shown[key] == cell.Block {
			continue
		}
		g.shown[key] = cell.Block
		changed = append(changed, cell)
		coords = append(coords, ChunkOf(cell.X, cell.Y, cell.Z))
	}
	if len(changed) == 0 {
		return nil
	}
	g.republish(coords)
	return changed
}

// republish swaps a patched copy in for every resident, composed chunk among coords,
// then bumps the revision once. A chunk that is not resident needs nothing: its next
// composition runs patch. The caller holds composeMu.
func (g *InstanceGate) republish(coords []Coord) {
	c := g.cache
	visited := make(map[Coord]bool)
	for _, coord := range coords {
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
}

// DoorCells returns a copy of one door's cells in declaration order, or nil for an
// index the layout has no door for.
func (g *InstanceGate) DoorCells(index int) []PlacedAnchor {
	return slices.Clone(g.doors[index])
}

// DoorOpen reports whether one door is open now.
func (g *InstanceGate) DoorOpen(index int) bool {
	g.cache.composeMu.Lock()
	defer g.cache.composeMu.Unlock()
	return g.opened[index]
}

// Mechanisms returns a copy of every mechanism anchor in declaration order: its cell,
// and in Index the puzzle it belongs to.
func (g *InstanceGate) Mechanisms() []PlacedAnchor {
	return slices.Clone(g.mechanisms)
}

// MechanismBlock is the block a mechanism cell shows now, and false for a cell that
// is not a mechanism.
func (g *InstanceGate) MechanismBlock(x, y, z int64) (Block, bool) {
	g.cache.composeMu.Lock()
	defer g.cache.composeMu.Unlock()
	b, ok := g.shown[[3]int64{x, y, z}]
	return b, ok
}
