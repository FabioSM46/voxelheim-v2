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

func instancePlacement(seed int64) Building {
	// A fixed drawing, quarter-turned by the seed. Negative seeds are converted
	// explicitly so this selection is identical on every integer architecture.
	return centreSchematic(BuildingRuin, 0, instanceChamber, 0, 0, 0, Facing(uint64(seed)&3))
}

// GenerateInstance is a pure generator for a two-arena dungeon.
// Outside its shell every voxel is Air (void), never open-world terrain. The
// drawing straddles X/Z chunk boundaries so the ordinary chunk compositor and
// streamer can use it without a special representation.
func GenerateInstance(seed int64, coord Coord) *Chunk {
	chunk := NewChunk(coord)
	if !instanceChunkContainsShell(seed, coord) {
		return chunk
	}
	b := instancePlacement(seed)
	for y := range instanceChamber.H {
		for z := range instanceChamber.D {
			for x := range instanceChamber.W {
				rx, rz := rotateCell(x, z, instanceChamber.W, instanceChamber.D, b.Facing)
				wx, wy, wz := b.OriginX+int64(rx), b.OriginY+int64(y), b.OriginZ+int64(rz)
				if ChunkOf(wx, wy, wz) == coord {
					chunk.Set(Local(wx), Local(wy), Local(wz), instanceChamber.At(x, y, z))
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
	c := NewCache(seed, workers, capacity, WithGenerator(GenerateInstance))
	c.contains = func(coord Coord) bool { return instanceContainsChunk(seed, coord) }
	c.editable = func(x, y, z int64) bool { return instanceInterior(seed, x, y, z) }
	return c
}

func instanceChunkContainsShell(seed int64, coord Coord) bool {
	minCoord, maxCoord := instanceChunkBounds(seed)
	return coord.X >= minCoord.X && coord.X <= maxCoord.X &&
		coord.Y >= minCoord.Y && coord.Y <= maxCoord.Y &&
		coord.Z >= minCoord.Z && coord.Z <= maxCoord.Z
}

func instanceChunkBounds(seed int64) (Coord, Coord) {
	b := instancePlacement(seed)
	w, d := rotatedFootprint(instanceChamber, b.Facing)
	return ChunkOf(b.OriginX, 0, b.OriginZ), ChunkOf(b.OriginX+int64(w-1), int64(instanceChamber.H-1), b.OriginZ+int64(d-1))
}

func instanceContainsChunk(seed int64, coord Coord) bool {
	lo, hi := instanceChunkBounds(seed)
	return coord.X >= lo.X-1 && coord.X <= hi.X+1 &&
		coord.Y >= lo.Y-1 && coord.Y <= hi.Y+1 &&
		coord.Z >= lo.Z-1 && coord.Z <= hi.Z+1
}

// instanceLocal maps a world cell back into the unrotated drawing. Check the
// finite bounds before converting to int so hostile coordinates cannot overflow
// on the server's 32-bit builds.
func instanceLocal(seed, x, y, z int64) (int, int, int, bool) {
	b := instancePlacement(seed)
	w, d := rotatedFootprint(instanceChamber, b.Facing)
	x, z = x-b.OriginX, z-b.OriginZ
	if x < 0 || x >= int64(w) || z < 0 || z >= int64(d) || y < 0 || y >= int64(instanceChamber.H) {
		return 0, 0, 0, false
	}
	rx, rz := rotateCell(int(x), int(z), w, d, Facing((4-uint8(b.Facing))&3))
	return rx, int(y), rz, true
}

func instanceInterior(seed, x, y, z int64) bool {
	lx, ly, lz, ok := instanceLocal(seed, x, y, z)
	// No shell, ornament, ceiling, or void cell is editable. Air above a real
	// floor is ephemeral player space, including the connected gallery.
	return ok && ly > 0 && instanceChamber.At(lx, ly, lz) == Air && instanceChamber.At(lx, 0, lz) != Air &&
		((ly < 9 && (lz < 28 || lz > 36)) || ly < 6)
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
