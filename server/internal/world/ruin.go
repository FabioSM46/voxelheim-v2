package world

import (
	"cmp"
	"math"
	"slices"
	"sync"
)

// The world holds exactly one dungeon portal, and it stands in the lattice cell that
// holds the capital.
//
// **The lattice survives the decision that emptied it.** Nothing on the wire, in the
// landmark id, or in the portal lookup names a position: they all name a cell, so
// keeping the 8192-block lattice as the *addressing scheme* is what lets this change
// stay inside one file. What the lattice no longer is, is a spawner — [RuinAt] refuses
// every cell but the capital's, and a second dungeon will be added deliberately rather
// than generated.
//
// An inset of 1024 keeps the whole footprint and its blend inside the cell.
const (
	RuinCellBlocks         = 8192
	ruinCellInset          = 1024
	RuinSettlementDistance = 1024
	ruinHalfFootprint      = 9 // largest rotated half-extent of the 15 by 19 drawing
	ruinBlendBlocks        = 8
	ruinReach              = ruinHalfFootprint + ruinBlendBlocks
	ruinMaxGroundDelta     = 2
	ruinCaveClearance      = RuinFloorY + 2

	// ruinFallbackGroundDelta is the one widening the ladder below is allowed, and it
	// is one block. The eight-block blend has to carry the whole difference between
	// the prepared floor and the land around it without terracing, so this is the
	// largest step that keeps the masonry sitting in the ground rather than on it.
	ruinFallbackGroundDelta = 3

	// ruinPreferredDistance is how far from the capital centre the search is willing
	// to relax flatness before it is willing to walk further. A player who has just
	// walked out of the gate should reach the `?` on their map in the first session.
	ruinPreferredDistance = 3000

	// ruinSearchStep is the spacing of the stratified candidate lattice inside the
	// cell inset: one hashed offset per square of this side, so the candidates are
	// spread rather than clumped, and the nearest acceptable one is genuinely near.
	ruinSearchStep = 48

	ruinSearchSpan  = RuinCellBlocks - 2*ruinCellInset
	ruinSearchCells = ruinSearchSpan / ruinSearchStep

	// ruinCoarseStride is the first pass over the footprint. It proves nothing — the
	// exact pass still runs — but it refuses broken ground after a few dozen ground
	// reads instead of after twelve hundred, which is what makes a ranked search over
	// thousands of candidates affordable at all.
	ruinCoarseStride = 4

	ruinPlaceSeedOffset   int64 = 0x61B724AD
	ruinVariantSeedOffset int64 = 0x29A46F85
	ruinFacingSeedOffset  int64 = 0x73C519B7
	ruinAttemptSeedStride int64 = 0x4F1BBCDD
)

const (
	_ = uint16(RuinCellBlocks - settlementCellBlocks - 1)
	_ = uint16(RuinCellBlocks - 2*ruinCellInset - 1)
	_ = uint16(ruinCellInset - ruinReach - ChunkSize - 1)
	_ = uint16(RuinSettlementDistance - settlementReach - ruinReach - 1)
	_ = uint8(RuinFloorY - ruinMaxGroundDelta - 1)
	// The widened rung must still leave the chamber under the surrounding surface.
	_ = uint8(RuinFloorY - ruinFallbackGroundDelta - 1)
	// The capital stands within capitalMaxSpawnDistance of the origin column, so the
	// cell that holds the origin column is the cell that holds the capital.
	_ = uint16(ruinCellInset - capitalMaxSpawnDistance - 1)
)

// The stratified lattice has to tile the inset span exactly, or the last column of
// candidates would be a different width from the rest.
var _ = [1]struct{}{}[ruinSearchSpan%ruinSearchStep]

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

// ruinCell is the one lattice cell that holds the world's one ruin: the cell holding
// the world's origin column, which is the cell the capital stands in for every seed
// (the guard above pins that). A function rather than a constant because
// [RuinCellOf] is one, and nothing here is on a hot path.
func ruinCell() (cellX, cellZ int64) {
	return RuinCellOf(originColumnX), RuinCellOf(originColumnZ)
}

// isRuinCell reports whether a lattice cell is that one.
func isRuinCell(cellX, cellZ int64) bool {
	cx, cz := ruinCell()
	return cellX == cx && cellZ == cz
}

// RuinAt returns this cell's ruin. Exactly one cell in the world has one — the
// capital's — and for that cell there is nothing to refuse: the ranked search below
// falls back rather than failing, in the same sense and for the same reason
// [CapitalAt] returns no bool. The bool stays because every other cell is empty.
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
			s, ok := ruinSiteAt(seed, cx, cz)
			if !ok {
				continue
			}
			d := squaredDistance(x, z, s.x, s.z)
			if d > r*r || d >= bestDistance {
				continue
			}
			best, bestDistance, found = s.ruin(cx, cz), d, true
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

// ruinRung is one attempt at accepting a candidate: how far from the capital it may
// stand, how much ground relief its footprint may cross, and whether its column has
// to be green. The ladder walks these in order and takes the nearest candidate the
// first satisfiable rung admits.
type ruinRung struct {
	maxDistance int64
	groundDelta int64
	green       bool
}

// ruinLadder is the acceptance order, and it is a statement about what matters more.
//
// Distance beats flatness by one block and nothing else: rung 2 will widen the relief
// tolerance to keep the portal inside [ruinPreferredDistance] rather than walk to the
// next flat plateau, because a portal three thousand blocks away is a portal a new
// player finds and one at five thousand is not. Rungs 3 and 4 are the same pair with
// the distance bound lifted, for a capital sitting in genuinely broken country.
//
// Rung 5 is what makes "placement never fails" true without a special case: it asks
// only for dry, unwarded ground clear of the capital, at any relief and any climate.
// It is not reached by any seed this repository has measured — rungs 1 and 2 answer
// every one of them — so it exists for the seed nobody has generated yet, and is
// covered by a test that drives the resolver with an empty ladder rather than by hope.
var ruinLadder = []ruinRung{
	{maxDistance: ruinPreferredDistance, groundDelta: ruinMaxGroundDelta, green: true},
	{maxDistance: ruinPreferredDistance, groundDelta: ruinFallbackGroundDelta, green: true},
	{maxDistance: math.MaxInt64, groundDelta: ruinMaxGroundDelta, green: true},
	{maxDistance: math.MaxInt64, groundDelta: ruinFallbackGroundDelta, green: true},
	{maxDistance: math.MaxInt64, groundDelta: math.MaxInt64, green: false},
}

// ruinSiteMemo holds the one resolved site per seed.
//
// **A cache is admissible here only because there is one site and it is a pure
// function of the seed.** Nothing it stores can differ between two calls, so a racing
// pair of resolvers computes the same value and either may win. The key space is the
// set of world seeds a process is configured with — one for a server, a few dozen for
// this package's tests — never anything a client names.
//
// **And it is needed, which was measured rather than assumed.** [ruinForArea] is
// called once per *column* from generate.go, and one resolve is 8.8–11.3 ms
// (BenchmarkResolveRuinSite). With the two lines below deleted, BenchmarkGenerateAtRuin
// goes from 3.4 ms to 660–870 ms a chunk and BenchmarkGenerateInACapital from 6.0 ms to
// 23–29 ms. With them, both sit inside the run-to-run noise of the same benchmarks on
// the branch this replaced.
var ruinSiteMemo sync.Map // seed int64 -> ruinSite

// ruinSiteAt is the resolved site of a lattice cell.
//
// Every cell but one answers `false` after two comparisons, which is why the rest of
// the world pays nothing for the search: [ruinForArea] is called per column.
func ruinSiteAt(seed, cellX, cellZ int64) (ruinSite, bool) {
	if !isRuinCell(cellX, cellZ) {
		return ruinSite{}, false
	}
	if v, ok := ruinSiteMemo.Load(seed); ok {
		return v.(ruinSite), true
	}
	s := resolveRuinSite(seed, cellX, cellZ, ruinLadder)
	ruinSiteMemo.Store(seed, s)
	return s, true
}

// ruinCandidateAt is one of the ranked search's stratified offsets: one hashed
// position inside the (i, j) square of the candidate lattice. Hash-only, so the
// ordering below can be built without consulting the ground.
func ruinCandidateAt(seed, cellX, cellZ int64, i, j int) ruinSite {
	h := hashLattice(seed+ruinPlaceSeedOffset+int64(i*ruinSearchCells+j)*ruinAttemptSeedStride, cellX, cellZ)
	return ruinSite{
		x:       cellX*RuinCellBlocks + ruinCellInset + int64(i)*ruinSearchStep + int64(h%ruinSearchStep),
		z:       cellZ*RuinCellBlocks + ruinCellInset + int64(j)*ruinSearchStep + int64((h>>32)%ruinSearchStep),
		variant: uint8(hashLattice(seed+ruinVariantSeedOffset, cellX, cellZ) % RuinVariantCount),
		facing:  Facing(hashLattice(seed+ruinFacingSeedOffset, cellX, cellZ) % 4),
	}
}

// ruinCandidates is every offset the search may take, nearest the capital first.
//
// The order is total — distance, then Z, then X — so two candidates exactly as far
// from the capital are tried in the same order on every machine, which is what makes
// the resolved site reproducible rather than merely deterministic-looking.
func ruinCandidates(seed, cellX, cellZ int64) []ruinSite {
	capital := CapitalAt(seed)
	out := make([]ruinSite, 0, ruinSearchCells*ruinSearchCells)
	for i := range ruinSearchCells {
		for j := range ruinSearchCells {
			out = append(out, ruinCandidateAt(seed, cellX, cellZ, i, j))
		}
	}
	slices.SortFunc(out, func(a, b ruinSite) int {
		da := squaredDistance(a.x, a.z, capital.CentreX, capital.CentreZ)
		db := squaredDistance(b.x, b.z, capital.CentreX, capital.CentreZ)
		return cmp.Or(cmp.Compare(da, db), cmp.Compare(a.z, b.z), cmp.Compare(a.x, b.x))
	})
	return out
}

// resolveRuinSite walks the ladder and returns the first candidate a rung accepts,
// or — when no rung accepts anything — the candidate nearest the capital.
//
// The ladder is a parameter so the last-resort branch can be driven directly by a
// test. It is the branch that carries the "placement never fails" promise, and a
// promise no test can reach is a promise nobody has checked.
func resolveRuinSite(seed, cellX, cellZ int64, ladder []ruinRung) ruinSite {
	candidates := ruinCandidates(seed, cellX, cellZ)
	capital := CapitalAt(seed)
	for _, rung := range ladder {
		for _, c := range candidates {
			if rung.maxDistance != math.MaxInt64 &&
				squaredDistance(c.x, c.z, capital.CentreX, capital.CentreZ) > rung.maxDistance*rung.maxDistance {
				continue
			}
			if s, ok := acceptRuinSite(seed, c, rung); ok {
				return s
			}
		}
	}
	s := candidates[0]
	ground, _ := ruinGroundAt(seed, s.x, s.z)
	s.floor = ground + 1
	return s
}

// ruinGroundAt reads the land before ruins. In particular it never calls HeightAt,
// columnAt or the composed shape, which would ask this same site to accept itself.
func ruinGroundAt(seed, x, z int64) (int, bool) {
	h, _, river := loweredHeightBeforeSettlementChannelRuleAt(seed, x, z, unloweredHeightAt(seed, x, z), ClimateAt(seed, x, z))
	return h, river || h <= seaLevel
}

// ruinIsGreen reports whether a column is in one of the two green climates. Desert
// and tundra are refused: the first dungeon stands in land a new player walks through.
func ruinIsGreen(seed, x, z int64) bool {
	switch ClimateAt(seed, x, z) {
	case Plains, Taiga:
		return true
	default:
		return false
	}
}

func acceptRuinSite(seed int64, s ruinSite, rung ruinRung) (ruinSite, bool) {
	if rung.green && !ruinIsGreen(seed, s.x, s.z) {
		return ruinSite{}, false
	}
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
	if !ruinFootprintAccepts(seed, s, ground, rung.groundDelta) {
		return ruinSite{}, false
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

// ruinFootprintAccepts reads every column of the footprint AND blend, not a ring
// sample that can miss a thin channel. The tolerance bounds the small eight-block
// smoothstep to gentle ground, and leaves the chamber under the surrounding surface.
//
// The coarse pass first is a rejection shortcut and nothing more: a candidate it
// refuses is one the exact pass would have refused, and every candidate it admits is
// then read column by column. Its only effect on the answer is how long the answer
// takes, which for a search over sixteen thousand candidates is the whole point.
func ruinFootprintAccepts(seed int64, s ruinSite, ground int, delta int64) bool {
	for _, stride := range [2]int{ruinCoarseStride, 1} {
		for dz := -ruinReach; dz <= ruinReach; dz += stride {
			for dx := -ruinReach; dx <= ruinReach; dx += stride {
				h, wet := ruinGroundAt(seed, s.x+int64(dx), s.z+int64(dz))
				if wet || absInt64(int64(h-ground)) > delta {
					return false
				}
			}
		}
	}
	return true
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
//
// **Queries away from the one ruin cell pay two comparisons per cell and never read
// the ground**; queries inside it pay a [sync.Map] load, because the ranked search is
// memoised per seed — see [ruinSiteMemo] for why a cache is admissible here when the
// comment this replaces said there was none.
func ruinForArea(seed, loX, loZ, hiX, hiZ int64) *ruinSite {
	for cz := RuinCellOf(loZ - ruinReach); cz <= RuinCellOf(hiZ+ruinReach); cz++ {
		for cx := RuinCellOf(loX - ruinReach); cx <= RuinCellOf(hiX+ruinReach); cx++ {
			s, ok := ruinSiteAt(seed, cx, cz)
			if !ok || s.x+ruinReach < loX || s.x-ruinReach > hiX || s.z+ruinReach < loZ || s.z-ruinReach > hiZ {
				continue
			}
			return &s
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
