package world

// InstanceAnchors returns independent copies of the two standing slots in world
// block coordinates. Consumers place feet at Y and centre X/Z in the named cell.
// The same seed rotation places both the chamber and its anchors.
func InstanceAnchors(seed int64) (arrival, exit PlacedAnchor) {
	b := instancePlacement(seed)
	for _, anchor := range b.Anchors {
		switch anchor.Kind {
		case AnchorInstanceArrival:
			arrival = anchor
		case AnchorInstanceExit:
			exit = anchor
		}
	}
	return arrival, exit
}

// instanceLayout is one dungeon drawing and the rule for which of its cells a
// player may edit. The generator, the finite chunk envelope, Contains and the edit
// rule all read the layout rather than a particular drawing, so a layout taller than
// one chunk — the multi-floor dungeon — needs nothing special anywhere: the envelope
// is simply the chunks its rotated footprint and height cover.
//
// **Nothing in a layout is ever saved.** An instance cache has no Store (see
// NewInstanceCache), so replacing or reshaping the layout bumps no generator version
// and invalidates nothing on disk; TestALayoutChangePersistsNothing pins it.
type instanceLayout struct {
	drawing *Schematic
	// interior reports whether an unrotated drawing cell may be edited. nil means the
	// generic rule: air or a cobweb the drawing placed, and nothing else.
	interior func(lx, ly, lz int) bool
}

// chamberLayout is the two-arena drawing with its authored headroom rule.
var chamberLayout = instanceLayout{
	drawing: instanceChamber,
	interior: func(lx, ly, lz int) bool {
		// No shell, ornament, ceiling, or void cell is editable. Air above a real
		// floor is ephemeral player space, including the connected gallery.
		return ly > 0 && instanceEditableCell(instanceChamber.At(lx, ly, lz)) && instanceChamber.At(lx, 0, lz) != Air &&
			((ly < 9 && (lz < 28 || lz > 36)) || ly < 6)
	},
}

// instanceEditableCell is the one drawing content an edit may replace: open air, or
// a cobweb a hit is allowed to clear.
func instanceEditableCell(b Block) bool { return b == Air || b == Cobweb }

func (l instanceLayout) placement(seed int64) Building {
	// A fixed drawing, quarter-turned by the seed. Negative seeds are converted
	// explicitly so this selection is identical on every integer architecture.
	return centreSchematic(BuildingRuin, 0, l.drawing, 0, 0, 0, Facing(uint64(seed)&3))
}

func instancePlacement(seed int64) Building { return chamberLayout.placement(seed) }

// GenerateInstance is a pure generator for the dungeon.
// Outside its shell every voxel is Air (void), never open-world terrain. The
// drawing straddles X/Z chunk boundaries so the ordinary chunk compositor and
// streamer can use it without a special representation.
func GenerateInstance(seed int64, coord Coord) *Chunk {
	return chamberLayout.generate(seed, coord)
}

// generate fills one chunk by mapping each of its voxels back into the unrotated
// drawing — 32³ lookups per chunk whatever the drawing's size, so a dungeon many
// chunks across costs each chunk the same. Void cells of the drawing stay air, and
// oriented blocks turn with the drawing.
func (l instanceLayout) generate(seed int64, coord Coord) *Chunk {
	chunk := NewChunk(coord)
	if !l.chunkContainsShell(seed, coord) {
		return chunk
	}
	facing := l.placement(seed).Facing
	ox, oy, oz := coord.Origin()
	for y := range ChunkSize {
		for z := range ChunkSize {
			for x := range ChunkSize {
				lx, ly, lz, ok := l.local(seed, ox+int64(x), oy+int64(y), oz+int64(z))
				if !ok {
					continue
				}
				if block := l.drawing.At(lx, ly, lz); block != keepTerrain {
					chunk.Set(x, y, z, rotateSchematicBlock(block, facing))
				}
			}
		}
	}
	return chunk
}

// NewInstanceCache creates the ephemeral world consumed by instance lifecycle
// code. It has no Store and cannot acquire one through its public API. Its shell
// cannot be mined or replaced, and its finite chunk envelope includes a one-chunk
// halo of void for rendering the outside faces. Streamers should use Contains to
// skip everything else; Get refuses it even if a body escapes through another bug.
func NewInstanceCache(seed int64, workers, capacity int) *Cache {
	return chamberLayout.cache(seed, workers, capacity)
}

func (l instanceLayout) cache(seed int64, workers, capacity int) *Cache {
	c := NewCache(seed, workers, capacity, WithGenerator(l.generate))
	c.contains = func(coord Coord) bool { return l.containsChunk(seed, coord) }
	c.editable = func(x, y, z int64) bool { return l.editable(seed, x, y, z) }
	return c
}

// InstanceChunkEnvelope is how many chunks an instance of this seed can ever hold
// resident: its shell's chunks plus the one-chunk halo of void around them. A cache
// sized to it never evicts a chunk a party can see.
func InstanceChunkEnvelope(seed int64) int {
	lo, hi := chamberLayout.chunkBounds(seed)
	return int(hi.X-lo.X+3) * int(hi.Y-lo.Y+3) * int(hi.Z-lo.Z+3)
}

func (l instanceLayout) chunkContainsShell(seed int64, coord Coord) bool {
	minCoord, maxCoord := l.chunkBounds(seed)
	return coord.X >= minCoord.X && coord.X <= maxCoord.X &&
		coord.Y >= minCoord.Y && coord.Y <= maxCoord.Y &&
		coord.Z >= minCoord.Z && coord.Z <= maxCoord.Z
}

func (l instanceLayout) chunkBounds(seed int64) (Coord, Coord) {
	b := l.placement(seed)
	w, d := rotatedFootprint(l.drawing, b.Facing)
	return ChunkOf(b.OriginX, b.OriginY, b.OriginZ),
		ChunkOf(b.OriginX+int64(w-1), b.OriginY+int64(l.drawing.H-1), b.OriginZ+int64(d-1))
}

func (l instanceLayout) containsChunk(seed int64, coord Coord) bool {
	lo, hi := l.chunkBounds(seed)
	return coord.X >= lo.X-1 && coord.X <= hi.X+1 &&
		coord.Y >= lo.Y-1 && coord.Y <= hi.Y+1 &&
		coord.Z >= lo.Z-1 && coord.Z <= hi.Z+1
}

// instanceLocal maps a world cell back into the unrotated drawing. Check the
// finite bounds before converting to int so hostile coordinates cannot overflow
// on the server's 32-bit builds.
func instanceLocal(seed, x, y, z int64) (int, int, int, bool) {
	return chamberLayout.local(seed, x, y, z)
}

func (l instanceLayout) local(seed, x, y, z int64) (int, int, int, bool) {
	b := l.placement(seed)
	w, d := rotatedFootprint(l.drawing, b.Facing)
	x, y, z = x-b.OriginX, y-b.OriginY, z-b.OriginZ
	if x < 0 || x >= int64(w) || z < 0 || z >= int64(d) || y < 0 || y >= int64(l.drawing.H) {
		return 0, 0, 0, false
	}
	rx, rz := rotateCell(int(x), int(z), w, d, Facing((4-uint8(b.Facing))&3))
	return rx, int(y), rz, true
}

func instanceInterior(seed, x, y, z int64) bool {
	return chamberLayout.editable(seed, x, y, z)
}

func (l instanceLayout) editable(seed, x, y, z int64) bool {
	lx, ly, lz, ok := l.local(seed, x, y, z)
	if !ok {
		return false
	}
	if l.interior != nil {
		return l.interior(lx, ly, lz)
	}
	return instanceEditableCell(l.drawing.At(lx, ly, lz))
}

// InstanceEncounterAnchors names the two stable placement slots and the centre
// of the progression gate. These are geometry, never species or entity ids.
func InstanceEncounterAnchors(seed int64) (guardian, king, gate PlacedAnchor) {
	for _, a := range instancePlacement(seed).Anchors {
		switch a.Kind {
		case AnchorInstanceGuardian:
			guardian = a
		case AnchorInstanceKing:
			king = a
		case AnchorInstanceGate:
			gate = a
		}
	}
	return
}
