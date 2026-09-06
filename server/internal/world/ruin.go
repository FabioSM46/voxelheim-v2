package world

// Ruins use an independent, coarser lattice than settlements. An inset of 1024
// leaves at least 2049 blocks between neighbouring candidates. A refused cell is
// empty: there is no retry that could crowd its neighbour or move a known arch.
const (
	RuinCellBlocks               = 8192
	ruinCellInset                = 1024
	ruinInverseDensity           = 2
	RuinSettlementDistance       = 1024
	ruinHalfFootprint            = 9 // largest rotated half-extent of the 15 by 19 drawing
	ruinBlendBlocks              = 8
	ruinReach                    = ruinHalfFootprint + ruinBlendBlocks
	ruinMaxGroundDelta           = 2
	ruinCaveClearance            = RuinFloorY + 2
	ruinSeedOffset         int64 = 0x37D91E63
	ruinPlaceSeedOffset    int64 = 0x61B724AD
	ruinVariantSeedOffset  int64 = 0x29A46F85
	ruinFacingSeedOffset   int64 = 0x73C519B7
)

const (
	_ = uint16(RuinCellBlocks - settlementCellBlocks - 1)
	_ = uint16(RuinCellBlocks - 2*ruinCellInset - 1)
	_ = uint16(ruinCellInset - ruinReach - ChunkSize - 1)
	_ = uint16(RuinSettlementDistance - settlementReach - ruinReach - 1)
	_ = uint8(RuinFloorY - ruinMaxGroundDelta - 1)
)

// Ruin identifies a site by its lattice cell within a world seed. Arch is a
// masonry coordinate, not a spawn position; the closed arch is part of Building.
// No chunk, cache, discovery state or instance is needed to obtain these values.
type Ruin struct {
	CellX, CellZ     int64
	CentreX, CentreZ int64
	FloorY           int
	Building         Building
	Arch             PlacedAnchor
}

// RuinCellOf returns the lattice cell containing a block coordinate, including
// negative coordinates. RuinAt is the bounded lookup for an explored-column ledger:
// deduplicate its columns by (RuinCellOf(x), RuinCellOf(z)), then query those cells.
func RuinCellOf(block int64) int64 { return floorDiv(block, RuinCellBlocks) }

// RuinAt returns this cell's accepted ruin. Cells beyond the playable world and
// candidates whose complete blend crosses its edge are refused before arithmetic.
func RuinAt(seed, cellX, cellZ int64) (Ruin, bool) {
	s, ok := ruinSiteAt(seed, cellX, cellZ)
	if !ok {
		return Ruin{}, false
	}
	return s.ruin(cellX, cellZ), true
}

// RuinNear returns the nearest ruin centre within radius blocks of (x,z), inclusive.
// Radius is bounded to one ruin cell (8192 blocks); invalid positions or radii
// return false. The square is centred on the position, never its lattice cell
// (#511); exact distance filtering makes the answer independent of cell edges.
// Ties use cell Z, then X. At most nine cells are inspected, with no generation.
func RuinNear(seed, x, z int64, radius int) (Ruin, bool) {
	if x < -BlockLimit || x > BlockLimit || z < -BlockLimit || z > BlockLimit || radius < 0 || radius > RuinCellBlocks {
		return Ruin{}, false
	}
	r := int64(radius)
	var best Ruin
	bestDistance := r*r + 1
	found := false
	for cz := RuinCellOf(z - r); cz <= RuinCellOf(z+r); cz++ {
		for cx := RuinCellOf(x - r); cx <= RuinCellOf(x+r); cx++ {
			candidate, ok := ruinCandidateAt(seed, cx, cz)
			if !ok {
				continue
			}
			d := squaredDistance(x, z, candidate.x, candidate.z)
			if d > r*r || d >= bestDistance {
				continue
			}
			if s, ok := ruinSiteAt(seed, cx, cz); ok {
				best, bestDistance, found = s.ruin(cx, cz), d, true
			}
		}
	}
	return best, found
}

type ruinSite struct {
	x, z    int64
	floor   int
	variant uint8
	facing  Facing
}

func ruinCandidateAt(seed, cellX, cellZ int64) (ruinSite, bool) {
	if cellX < RuinCellOf(-BlockLimit) || cellX > RuinCellOf(BlockLimit) || cellZ < RuinCellOf(-BlockLimit) || cellZ > RuinCellOf(BlockLimit) {
		return ruinSite{}, false
	}
	if hashLattice(seed+ruinSeedOffset, cellX, cellZ)%ruinInverseDensity != 0 {
		return ruinSite{}, false
	}
	h := hashLattice(seed+ruinPlaceSeedOffset, cellX, cellZ)
	const span = RuinCellBlocks - 2*ruinCellInset
	s := ruinSite{
		x:       cellX*RuinCellBlocks + ruinCellInset + int64(h%span),
		z:       cellZ*RuinCellBlocks + ruinCellInset + int64((h>>32)%span),
		variant: uint8(hashLattice(seed+ruinVariantSeedOffset, cellX, cellZ) % RuinVariantCount),
		facing:  Facing(hashLattice(seed+ruinFacingSeedOffset, cellX, cellZ) % 4),
	}
	return s, s.x-ruinReach >= -BlockLimit && s.x+ruinReach <= BlockLimit && s.z-ruinReach >= -BlockLimit && s.z+ruinReach <= BlockLimit
}

// ruinGroundAt reads the land before ruins. In particular it never calls HeightAt,
// columnAt or the composed shape, which would ask this same site to accept itself.
func ruinGroundAt(seed, x, z int64) (int, bool) {
	h, _, river := loweredHeightBeforeSettlementChannelRuleAt(seed, x, z, unloweredHeightAt(seed, x, z), ClimateAt(seed, x, z))
	return h, river || h <= seaLevel
}

func ruinSiteAt(seed, cellX, cellZ int64) (ruinSite, bool) {
	s, ok := ruinCandidateAt(seed, cellX, cellZ)
	if !ok {
		return ruinSite{}, false
	}
	return acceptRuinSite(seed, s)
}

func acceptRuinSite(seed int64, s ruinSite) (ruinSite, bool) {
	// This position-centred scan includes every settlement that could violate the
	// stated distance. Comparing sites avoids allocating an entire village layout.
	if _, _, _, d, found := nearestSettlementSite(seed, s.x, s.z, RuinSettlementDistance); found && d <= int64(RuinSettlementDistance)*RuinSettlementDistance {
		return ruinSite{}, false
	}
	ground, wet := ruinGroundAt(seed, s.x, s.z)
	if wet {
		return ruinSite{}, false
	}
	s.floor = ground + 1
	// Read every column of the footprint AND blend, not a ring sample that can
	// miss a thin channel. A two-block tolerance bounds the small eight-block
	// smoothstep to gentle ground, and leaves the chamber under the surrounding
	// surface. A failed column rejects this candidate; nothing searches nearby.
	for dz := -ruinReach; dz <= ruinReach; dz++ {
		for dx := -ruinReach; dx <= ruinReach; dx++ {
			h, wet := ruinGroundAt(seed, s.x+int64(dx), s.z+int64(dz))
			if wet || absInt64(int64(h-ground)) > ruinMaxGroundDelta {
				return ruinSite{}, false
			}
		}
	}
	// Distance is design; ward overlap is correctness. Keep the exact column test
	// even though today's larger distance already implies it, so growth of either
	// footprint cannot silently turn this site into a warded ruin.
	for cz := floorDiv(s.z-ruinHalfFootprint, ChunkSize); cz <= floorDiv(s.z+ruinHalfFootprint, ChunkSize); cz++ {
		for cx := floorDiv(s.x-ruinHalfFootprint, ChunkSize); cx <= floorDiv(s.x+ruinHalfFootprint, ChunkSize); cx++ {
			if _, warded := SettlementWarding(seed, Column{CX: int32(cx), CZ: int32(cz)}); warded {
				return ruinSite{}, false
			}
		}
	}
	return s, true
}

func (s ruinSite) ruin(cellX, cellZ int64) Ruin {
	b := centredRuinBuilding(s.variant, s.x, s.z, int64(s.floor), s.facing)
	r := Ruin{CellX: cellX, CellZ: cellZ, CentreX: s.x, CentreZ: s.z, FloorY: s.floor, Building: b}
	for _, a := range b.Anchors {
		if a.Kind == AnchorRuinArch {
			r.Arch = a
		}
	}
	return r
}

// ruinForArea resolves a site once for a chunk and its bank/plant border. The inset guard
// above ensures a chunk-sized area can touch at most one ruin, even at cell edges.
// Queries away from candidates pay only hashes; there is no shared mutable cache.
func ruinForArea(seed, loX, loZ, hiX, hiZ int64) *ruinSite {
	for cz := RuinCellOf(loZ - ruinReach); cz <= RuinCellOf(hiZ+ruinReach); cz++ {
		for cx := RuinCellOf(loX - ruinReach); cx <= RuinCellOf(hiX+ruinReach); cx++ {
			s, ok := ruinCandidateAt(seed, cx, cz)
			if !ok || s.x+ruinReach < loX || s.x-ruinReach > hiX || s.z+ruinReach < loZ || s.z-ruinReach > hiZ {
				continue
			}
			if s, ok = acceptRuinSite(seed, s); ok {
				return &s
			}
		}
	}
	return nil
}

func (s *ruinSite) shape(x, z int64, natural int) (int, bool) {
	if s == nil {
		return natural, false
	}
	d := max(absInt64(x-s.x), absInt64(z-s.z))
	if d >= ruinReach {
		return natural, false
	}
	if d <= ruinHalfFootprint {
		return s.floor - 1, true
	}
	t := ((d - ruinHalfFootprint) * one) / ruinBlendBlocks
	return int(lerp(int64(s.floor-1), int64(natural), smoothstep(t))), true
}

func placeRuins(chunk *Chunk, s *ruinSite) {
	if s == nil {
		return
	}
	ox, oy, oz := chunk.Coord.Origin()
	b := centredRuinBuilding(s.variant, s.x, s.z, int64(s.floor), s.facing)
	visitSchematic(b, func(x, y, z int64, block Block) {
		x, y, z = x-ox, y-oy, z-oz
		if x >= 0 && x < ChunkSize && y >= 0 && y < ChunkSize && z >= 0 && z < ChunkSize {
			// Explicit Air excavates the chamber; keepTerrain never reaches this
			// visitor. Masonry replaces soil as well as air, burying the foundations.
			chunk.Set(int(x), int(y), int(z), block)
		}
	})
}
