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

// GenerateInstance is a pure generator for a single enclosed stone chamber.
// Outside its shell every voxel is Air (void), never open-world terrain. The
// drawing straddles X/Z chunk boundaries so the ordinary chunk compositor and
// streamer can use it without a special representation.
func GenerateInstance(seed int64, coord Coord) *Chunk {
	chunk := NewChunk(coord)
	if !instanceChunkContainsShell(coord) {
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
	c.contains = instanceContainsChunk
	c.editable = instanceInterior
	return c
}

func instanceChunkContainsShell(coord Coord) bool {
	minCoord, maxCoord := instanceChunkBounds()
	return coord.X >= minCoord.X && coord.X <= maxCoord.X &&
		coord.Y >= minCoord.Y && coord.Y <= maxCoord.Y &&
		coord.Z >= minCoord.Z && coord.Z <= maxCoord.Z
}

func instanceChunkBounds() (Coord, Coord) {
	// The square footprint makes all four turns share bounds. Derive the bounds
	// from the drawing, not a second set of dimensions beside it.
	half := int64(instanceChamber.W / 2)
	return ChunkOf(-half, 0, -half), ChunkOf(half, int64(instanceChamber.H-1), half)
}

func instanceContainsChunk(coord Coord) bool {
	lo, hi := instanceChunkBounds()
	return coord.X >= lo.X-1 && coord.X <= hi.X+1 &&
		coord.Y >= lo.Y-1 && coord.Y <= hi.Y+1 &&
		coord.Z >= lo.Z-1 && coord.Z <= hi.Z+1
}

func instanceInterior(x, y, z int64) bool {
	half := int64(instanceChamber.W / 2)
	return x > -half && x < half && z > -half && z < half &&
		y > 0 && y < int64(instanceChamber.H-1)
}
