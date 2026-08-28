package world

import (
	"cmp"
	"slices"
)

// Settlements: a capital by the spawn, villages across the land, and the flat ground
// under both.
//
// **A settlement is a lattice cell's answer, not a field's.** Every other feature in
// this package is a continuous function sampled per column, because a hillside has no
// edges; a village does, and a village that emerged from a threshold on noise would
// have a different number of huts depending on which chunk asked. So the world is cut
// into 2048-block cells, each cell either holds one settlement or does not, and
// everything about the one it holds — where it stands, how big it is, which buildings
// it has and which way each of them faces — is derived from (seed, cellX, cellZ) by
// hashing. Two chunks that share a building reach the same voxels because they compute
// the same cell, not because either read the other.
//
// **The lattice is centred on spawn rather than cornered on it.** `settlementCellOf`
// shifts by half a cell, so the origin sits in the middle of cell (0, 0) and the
// capital can stand at any bearing from the player's first step without falling into a
// neighbouring cell — which would make it invisible to the four-cell lookup every
// column does, because that lookup is by cell index and reaches only as far as one
// settlement's radius.
//
// # The ground has to agree
//
// A building written on top of rolling terrain is a building with a corner in the air
// and a corner buried. So a settlement flattens its ground: inside its radius the
// surface *is* the plateau, and for sixteen blocks beyond it the height blends back to
// what the land was doing. That decision lives in [HeightAt] rather than beside it, for
// the reason basins and rivers do — trees, ore depths, cave carving, spawn placement
// and map tiles all read the height field, and a plateau only some of them knew about
// would put a conifer through a roof and a tunnel through a floor.

const (
	// settlementCellBlocks is the edge of one lattice cell.
	//
	// Two kilometres, which is climateScaleBlocks: a settlement should be about as
	// far apart as a climate is wide, so a walk from one village to the next crosses
	// a piece of country rather than a field. It also comfortably exceeds twice
	// `settlementReach`, which is what lets a column consult at most four cells.
	settlementCellBlocks = 2048

	// The two sizes of settlement, as the radius of flat ground each stands on.
	//
	// A capital has to hold a keep, a hall, a smithy and a ring of six huts without
	// any of them touching; fifty-six is what that measures out to with the ring at
	// forty and the largest footprint fifteen across. A village is half of it.
	capitalRadius = 56
	villageRadius = 28

	// settlementBlendBlocks is how far past the radius the plateau eases back into
	// the natural height.
	//
	// Sixteen blocks of smoothstep rather than a step, for the reason `basinAt`
	// interpolates its rim: a plateau that ended at a cliff would put a wall around
	// every village, and the derivative jump would be visible from a long way off.
	// Nothing inside the radius plus this band carries a basin or a river channel —
	// both of those move the ground, and the whole point here is that it does not
	// move.
	settlementBlendBlocks = 16

	// settlementReach is how far from a settlement's centre anything about it can be
	// felt. The four-cell lookup is derived from it, and it must stay well under half
	// a cell.
	settlementReach = capitalRadius + settlementBlendBlocks

	// How far from spawn the capital stands. Close enough to walk to before dark and
	// far enough that a session does not begin inside a wall.
	capitalMinSpawnDistance = 120
	capitalMaxSpawnDistance = 200

	// villageInverseDensity is one settlement in how many cells. One in three leaves
	// roughly a village every three and a half kilometres, which is a long walk with
	// something at the end of it rather than a suburb.
	villageInverseDensity = 3

	// villageCellInset keeps a village's centre this far inside its own cell, so a
	// settlement never straddles a lattice boundary. Nothing depends on it — the
	// four-cell lookup already reaches across — but a village wholly inside one cell
	// is far easier to reason about when a later issue asks which cell owns what.
	villageCellInset = 256

	// The three ways a site is refused.
	//
	// **Relief, not slope.** The field that decides how tall the terrain is allowed
	// to be here is the cheapest honest answer to "is this a mountainside": above its
	// midpoint the amplitude is climbing towards a hundred and fifty blocks, and a
	// plateau cut into that is a quarry rather than a town.
	//
	// The height floor is read from the *unlowered* surface — the terrain before
	// basins, rivers and this feature — because a plateau three blocks over the sea
	// line is a settlement with the tide at its door.
	settlementReliefLimit = one / 2
	settlementMinPlateau  = seaLevel + 3

	// settlementCaveClearance is how many blocks under a settlement's plateau stay
	// solid. Six is deeper than any foundation here and shallow enough that the ore
	// bands are still reachable by digging down inside the walls.
	settlementCaveClearance = 6

	// The capital's plan: a keep in the middle, the hall and the smithy on their own
	// bearings a short walk out, and the huts on a ring beyond both.
	capitalPlotRadius    = 25
	capitalHutRingRadius = 40
	capitalHutCount      = 6

	// The village's plan: one public building in the middle and a few huts round it.
	villageHutRingRadius = 16
	villageMinHuts       = 3
	villageHutVariants   = 3

	// settlementRiverSampleRings is how many rings of the disc the river test looks
	// at, beside its centre. See riverCrossesSite.
	settlementRiverSampleRings = 2

	// capitalSiteAttempts is how many hashed offsets the capital tries before it
	// settles for the first one.
	//
	// **The capital always exists, so its site rules are preferences and not
	// refusals** — that is the one asymmetry in this file, and it is forced: a world
	// whose spawn happens to sit in high relief would otherwise have no capital at
	// all, and the capital is the place the game starts. Four attempts, because the
	// relief field's lattice is 768 blocks and every candidate is inside 200 of
	// spawn: they are strongly correlated, so a fifth try buys almost nothing while
	// costing another site evaluation on the height path.
	capitalSiteAttempts = 4

	// capitalAttemptSeedStride decorrelates one attempt's offset from the next.
	capitalAttemptSeedStride int64 = 0x7C3A5B19
)

// Each decision a cell makes gets its own offset from the world seed, in the style of
// every other field in this package. One hash read three ways would tie a village's
// existence to its position and its position to its plan, and every settlement in the
// world would be the same settlement.
const (
	settlementSeedOffset       int64 = 0x6C2C1D4B
	settlementPlaceSeedOffset  int64 = 0x1F83D9AB
	settlementLayoutSeedOffset int64 = 0x5BE0CD19
)

// The lattice has to be wide enough that a column can only ever be reached by the
// settlements of the four cells nearest it. Two reaches plus a cell has to fit in two
// cells, which is this conversion; break it and the four-cell lookup below silently
// starts missing settlements instead of failing.
const _ uint64 = settlementCellBlocks/2 - settlementReach

// A capital's ring of huts has to stand clear of the hall and the smithy inside it,
// and the plateau has to be wide enough to hold the ring. Both are compile errors
// rather than a building with its corner in another building.
const _ = uint8(capitalRadius - capitalHutRingRadius - 4)
const _ = uint8(capitalHutRingRadius - capitalPlotRadius - 10)

// SettlementKind tells a capital from a village.
//
// Server-side only, like [Climate]: nothing sends a settlement, and the client learns
// there is one by being sent planks and a map tile that says [SurfaceSettlement].
type SettlementKind uint8

// The two kinds. There is exactly one capital in a world — the cell holding spawn
// always has it and no other cell may — and as many villages as the lattice rolls.
const (
	SettlementCapital SettlementKind = iota
	SettlementVillage
)

// String names a settlement kind for test failures and diagnostics.
func (k SettlementKind) String() string {
	if k == SettlementCapital {
		return "capital"
	}
	return "village"
}

// Settlement is one built place: where it is, how much ground it flattens, and what
// stands on it.
type Settlement struct {
	Kind             SettlementKind
	CentreX, CentreZ int64

	// Radius is how far the flat ground reaches, and Plateau is how high it is — the
	// unlowered height of the centre column, which is the one height every building
	// here is placed against.
	Radius  int
	Plateau int

	Buildings []Building
}

// Anchors is every slot in every building of this settlement, in world coordinates.
//
// The one call the stations and residents issues need: they ask a settlement what it
// has room for rather than walking its buildings and rotating anchors themselves.
func (s Settlement) Anchors() []PlacedAnchor {
	var out []PlacedAnchor
	for _, b := range s.Buildings {
		out = append(out, b.Anchors...)
	}
	return out
}

// SettlementAt is the settlement one lattice cell holds, if it holds one.
//
// Pure in (seed, cellX, cellZ) like everything else here. It builds the buildings,
// which allocates; the height field deliberately does not call it — see
// [settlementSiteAt], which answers the part of the question terrain needs without
// laying out a single hut.
func SettlementAt(seed int64, cellX, cellZ int64) (Settlement, bool) {
	site, ok := settlementSiteAt(seed, cellX, cellZ)
	if !ok {
		return Settlement{}, false
	}
	return settlementFrom(seed, cellX, cellZ, site), true
}

// SettlementsNear returns every settlement in the square of lattice cells `cells` out
// from the cell holding (x, z), nearest centre first.
//
// **The unit is cells rather than blocks, because the lattice is what can be
// enumerated.** A block radius would have to be turned into a cell range anyway, and
// the caller that wants "is there a village within eight hundred blocks" gets a
// truthful answer by measuring the results rather than by trusting a conversion made
// here. A negative count is one cell, which is the cell (x, z) is in.
//
// The order is total: distance first, then the cell coordinates, so two settlements
// exactly as far away come back in the same order on every machine.
func SettlementsNear(seed int64, x, z int64, cells int) []Settlement {
	if cells < 0 {
		cells = 0
	}
	centreCellX, centreCellZ := settlementCellOf(x), settlementCellOf(z)

	var found []Settlement
	for cz := centreCellZ - int64(cells); cz <= centreCellZ+int64(cells); cz++ {
		for cx := centreCellX - int64(cells); cx <= centreCellX+int64(cells); cx++ {
			if s, ok := SettlementAt(seed, cx, cz); ok {
				found = append(found, s)
			}
		}
	}

	slices.SortFunc(found, func(a, b Settlement) int {
		if c := cmp.Compare(squaredDistance(x, z, a.CentreX, a.CentreZ), squaredDistance(x, z, b.CentreX, b.CentreZ)); c != 0 {
			return c
		}
		if c := cmp.Compare(a.CentreZ, b.CentreZ); c != 0 {
			return c
		}
		return cmp.Compare(a.CentreX, b.CentreX)
	})
	return found
}

// nearestSettlementCells is how far [NearestSettlement] looks. Three cells out is a
// six-kilometre square, which at one settlement in three cells holds about sixteen of
// them: far enough that the answer is essentially never empty, and bounded so the
// search is a constant rather than a spiral that might not terminate.
const nearestSettlementCells = 3

// NearestSettlement returns the settlement whose centre is closest to (x, z), searching
// up to [nearestSettlementCells] cells out.
//
// The false is real and callers must handle it: a stretch of world where every nearby
// cell rolled no village, or rolled one on ground the site rules refused, genuinely has
// no settlement in reach.
func NearestSettlement(seed int64, x, z int64) (Settlement, bool) {
	near := SettlementsNear(seed, x, z, nearestSettlementCells)
	if len(near) == 0 {
		return Settlement{}, false
	}
	return near[0], true
}

// settlementCellOf maps a world coordinate on one axis to its lattice cell.
//
// The half-cell shift is what puts spawn in the middle of cell (0, 0) rather than on
// its corner; see the file comment for why that matters.
func settlementCellOf(v int64) int64 {
	return floorDiv(v+settlementCellBlocks/2, settlementCellBlocks)
}

// settlementCellOrigin is the lowest world coordinate belonging to a cell on one axis.
func settlementCellOrigin(cell int64) int64 {
	return cell*settlementCellBlocks - settlementCellBlocks/2
}

// settlementCandidate is what a cell proposes before the ground is consulted: a kind, a
// centre and a size. Every field is a hash, so this costs no noise at all — which is
// the whole reason it is separate from [settlementSiteAt]. A column half a world from
// anywhere rejects four candidates on two subtractions each.
type settlementCandidate struct {
	kind             SettlementKind
	centreX, centreZ int64
	radius           int
}

// settlementCandidateAt is the cell's proposal: a village one time in
// [villageInverseDensity], somewhere inside the cell's own bounds.
//
// **The spawn cell answers with its *first* capital offset, which is not necessarily
// the one the capital ends up on.** That is why nothing on the height path reaches the
// capital through here — [settlementMayReach] considers every attempt and
// [settlementSiteAt] hands the cell straight to [capitalSiteAt]. The branch is kept so
// the function is total over cells rather than because anything relies on it: a helper
// that answered "no proposal" for the one cell that always has a settlement would be a
// trap for the next reader.
func settlementCandidateAt(seed int64, cellX, cellZ int64) (settlementCandidate, bool) {
	if isCapitalCell(cellX, cellZ) {
		return capitalCandidateAt(seed, cellX, cellZ, 0), true
	}

	if hashLattice(seed+settlementSeedOffset, cellX, cellZ)%villageInverseDensity != 0 {
		return settlementCandidate{}, false
	}

	h := hashLattice(seed+settlementPlaceSeedOffset, cellX, cellZ)
	span := uint64(settlementCellBlocks - 2*villageCellInset)
	return settlementCandidate{
		kind:    SettlementVillage,
		centreX: settlementCellOrigin(cellX) + villageCellInset + int64(h%span),
		centreZ: settlementCellOrigin(cellZ) + villageCellInset + int64((h>>32)%span),
		radius:  villageRadius,
	}, true
}

// isCapitalCell reports whether a lattice cell is the one holding spawn — the one cell
// that always has a settlement in it.
func isCapitalCell(cellX, cellZ int64) bool {
	return cellX == settlementCellOf(spawnColumnX) && cellZ == settlementCellOf(spawnColumnZ)
}

// capitalCandidateAt is one of the capital's [capitalSiteAttempts] proposed offsets.
//
// The distance is exact rather than approximate, and the integer square root is why: a
// bearing chosen as an angle would need trigonometry, so instead one leg is hashed, the
// other is the leg that completes the triangle, and the two signs are two more bits.
// The rounding costs at most one block, which is why the hashed distance starts one
// above the minimum — so every offset is genuinely inside
// [capitalMinSpawnDistance, capitalMaxSpawnDistance].
func capitalCandidateAt(seed int64, cellX, cellZ int64, attempt int) settlementCandidate {
	h := hashLattice(seed+settlementPlaceSeedOffset+int64(attempt)*capitalAttemptSeedStride, cellX, cellZ)
	span := uint64(capitalMaxSpawnDistance - capitalMinSpawnDistance)
	distance := int64(capitalMinSpawnDistance) + 1 + int64(h%span)

	dx := int64((h >> 20) % uint64(distance+1))
	dz := isqrt(distance*distance - dx*dx)
	if h&(1<<40) != 0 {
		dx = -dx
	}
	if h&(1<<41) != 0 {
		dz = -dz
	}
	return settlementCandidate{
		kind:    SettlementCapital,
		centreX: spawnColumnX + dx,
		centreZ: spawnColumnZ + dz,
		radius:  capitalRadius,
	}
}

// settlementSite is a candidate the ground has accepted, with the one height every
// building and every column of it is placed against.
type settlementSite struct {
	settlementCandidate
	plateau int
}

// settlementSiteAt is [SettlementAt] without the buildings: everything the height field
// needs and nothing that allocates.
//
// **The three refusals are the only thing standing between a settlement and a
// mountainside, a lake bed or a river.** Order is cost: the relief field is one fbm
// sum, the unlowered height is two more, and the river sweep is the expensive one and
// therefore last.
func settlementSiteAt(seed int64, cellX, cellZ int64) (settlementSite, bool) {
	if isCapitalCell(cellX, cellZ) {
		return capitalSiteAt(seed, cellX, cellZ), true
	}

	candidate, ok := settlementCandidateAt(seed, cellX, cellZ)
	if !ok {
		return settlementSite{}, false
	}
	if reliefAt(seed, candidate.centreX, candidate.centreZ) > settlementReliefLimit {
		return settlementSite{}, false
	}
	plateau := unloweredHeightAt(seed, candidate.centreX, candidate.centreZ)
	if plateau < settlementMinPlateau {
		return settlementSite{}, false
	}
	if riverCrossesSite(seed, candidate) {
		return settlementSite{}, false
	}
	return settlementSite{settlementCandidate: candidate, plateau: plateau}, true
}

// capitalSiteAt chooses where the capital stands. It never refuses.
//
// **Every rule that rejects a village only ranks the capital**, and the measurement is
// why. `reliefAt > one/2` reads as "the hillier half of the world", but fbm2D piles up
// around its midpoint, so it is close to half of all *columns* — and the relief field's
// lattice is 768 blocks wide while every candidate here is inside 200 of spawn, so the
// four attempts are strongly correlated rather than four independent rolls. Seeds 1, 7
// and 99 all fail it at every offset. A capital that a seed can simply not have is not
// a capital, and the game begins at this one.
//
// **The river test is not applied here at all, and dropping it costs nothing.** Its
// field has the largest lattice in the generator — 640 blocks — so four offsets inside
// two hundred of spawn are all reading essentially the same value: it cannot separate
// them, and it cannot reject the capital either, because the capital is not rejectable.
// What it can do is cost thirteen fbm sums per candidate on the height path, once for
// every column standing in the capital, and the capital is the one settlement in the
// world every session starts beside. A channel the plateau meets stops at the edge of
// the flat ground, which is what a river meeting raised ground does anyway.
//
// The last resort keeps the two things a settlement cannot do without: the first
// offset, and a plateau at or above [settlementMinPlateau], so the fallback capital
// stands on dry ground even where the land around spawn does not. That floor is
// [SpawnAt]'s, for [SpawnAt]'s reason — lifting is the fail-safe direction and lowering
// is not.
func capitalSiteAt(seed int64, cellX, cellZ int64) settlementSite {
	var first settlementSite
	for attempt := range capitalSiteAttempts {
		candidate := capitalCandidateAt(seed, cellX, cellZ, attempt)
		plateau := unloweredHeightAt(seed, candidate.centreX, candidate.centreZ)
		if attempt == 0 {
			first = settlementSite{
				settlementCandidate: candidate,
				plateau:             max(plateau, settlementMinPlateau),
			}
		}
		if plateau < settlementMinPlateau || reliefAt(seed, candidate.centreX, candidate.centreZ) > settlementReliefLimit {
			continue
		}
		return settlementSite{settlementCandidate: candidate, plateau: plateau}
	}
	return first
}

// settlementMayReach reports whether any centre this cell could choose lies within
// reach of a column.
//
// **Hashes only, and that is the whole performance story of the height field.** A
// column with nothing near it pays four of these and no noise at all; the site rules
// behind them cost twenty-seven fbm sums and are reached by the four columns in a
// thousand that are actually standing in a settlement. The capital's four attempts are
// all considered, because which one it settles on is not known until the ground has
// been consulted — and consulting the ground is the thing this test exists to avoid.
func settlementMayReach(seed int64, cellX, cellZ, worldX, worldZ int64) bool {
	reaches := func(c settlementCandidate) bool {
		reach := int64(c.radius + settlementBlendBlocks)
		return squaredDistance(worldX, worldZ, c.centreX, c.centreZ) < reach*reach
	}

	if isCapitalCell(cellX, cellZ) {
		for attempt := range capitalSiteAttempts {
			if reaches(capitalCandidateAt(seed, cellX, cellZ, attempt)) {
				return true
			}
		}
		return false
	}

	candidate, ok := settlementCandidateAt(seed, cellX, cellZ)
	return ok && reaches(candidate)
}

// riverCrossesSite reports whether a channel runs through the ground a settlement would
// flatten.
//
// **A sample, and deliberately not a proof.** Testing every column inside a
// fifty-six-block radius is ten thousand fbm sums, and this question is asked once per
// column of every settlement chunk — it would cost more than the rest of generation put
// together. So it reads thirteen columns: the centre, and two interleaved rings of six
// bearings. A channel that threads between them survives.
//
// **Thirteen is a budget rather than a taste, and the binding constraint is the map
// rather than the generator.** A scale-1 map tile is 4096 columns of a sixty-four-block
// square, so a tile drawn inside a village pays this for every one of them, against a
// ten-millisecond acceptance criterion in internal/session. Measured: a column in open
// country costs 0.42 µs, one in a village 1.11 µs at thirteen samples and 1.73 µs at
// twenty-five. Four thousand of those is 4.6 ms against 7.1 ms — and this machine runs
// about 1.5× the one BenchmarkSurfaceAt's figures were recorded on, so the twenty-five
// sample version puts the worst tile in the world *through* the ceiling while thirteen
// leaves it at two thirds. An ordinary tile is nowhere near either: at scale 16 a
// village is three pixels across.
//
// What makes a sparse sample acceptable is what the plateau already does: inside the
// radius the ground *is* flat, so a river the sample missed does not cut a trench
// through the village — it simply stops at the edge of the plateau, which is what a
// river meeting raised ground does anyway. The rejection is here so that a settlement
// is not routinely planted across an obvious watercourse, not because a missed one is a
// bug in the terrain. TestNoSettlementIsPlantedOnAnObviousRiver measures what it
// catches.
func riverCrossesSite(seed int64, c settlementCandidate) bool {
	if riverAt(seed, c.centreX, c.centreZ) {
		return true
	}
	for ring := 1; ring <= settlementRiverSampleRings; ring++ {
		radius := c.radius * ring / settlementRiverSampleRings
		// The rings interleave: with two of them, the inner one takes the even
		// bearings and the outer the odd, so twelve samples cover twelve directions
		// rather than six directions twice.
		for bearing := ring - 1; bearing < len(settlementBearings); bearing += settlementRiverSampleRings {
			dx, dz := ringOffset(radius, bearing)
			if riverAt(seed, c.centreX+dx, c.centreZ+dz) {
				return true
			}
		}
	}
	return false
}

// settlementShapeAt is the settlement's say over one column's height.
//
// It returns the surface, whether the column is *inside* a settlement — which is what
// suppresses trees and shallow carving and what a map tile draws as a settlement — and
// whether it is inside the blend band, which is what suppresses basins and rivers.
//
// **The order of the two tests is the budget for the whole feature.** The candidate is
// hashes only, so a column with nothing near it pays four hashes and four squared
// distances; the site rules, which cost twenty-seven fbm sums, are only reached by a
// column that is actually standing in a settlement. At most one settlement can reach a
// column — the lattice is wider than two reaches — so the first hit is the answer.
//
// `base` is the unlowered land. What the blend eases towards is [loweredHeightAt] of it,
// so the outer edge of the band meets the terrain the next column out actually has —
// see the paragraph in [shapeAt] about the cliff that blending towards `base` produced.
// It is read inside the band and nowhere else: a column on the plateau never pays for
// it, and a column with no settlement near it never reaches this function at all.
func settlementShapeAt(seed int64, worldX, worldZ int64, base int, climate Climate) (surface int, inside, near bool) {
	loX, hiX := settlementCellOf(worldX-settlementReach), settlementCellOf(worldX+settlementReach)
	loZ, hiZ := settlementCellOf(worldZ-settlementReach), settlementCellOf(worldZ+settlementReach)

	for cz := loZ; cz <= hiZ; cz++ {
		for cx := loX; cx <= hiX; cx++ {
			if !settlementMayReach(seed, cx, cz, worldX, worldZ) {
				continue
			}
			site, ok := settlementSiteAt(seed, cx, cz)
			if !ok {
				continue
			}
			d2 := squaredDistance(worldX, worldZ, site.centreX, site.centreZ)
			reach := int64(site.radius + settlementBlendBlocks)
			if d2 >= reach*reach {
				continue
			}

			distance := isqrt(d2)
			if distance <= int64(site.radius) {
				// Inside the radius nothing but the plateau is read, which is what
				// keeps the feature affordable: the columns that pay for a basin and a
				// channel below are the sixteen-block band, never the disc.
				return site.plateau, true, true
			}
			t := ((distance - int64(site.radius)) * one) / settlementBlendBlocks
			natural, _ := loweredHeightAt(seed, worldX, worldZ, base, climate)
			return int(lerp(int64(site.plateau), int64(natural), smoothstep(t))), false, true
		}
	}
	return base, false, false
}

// settlementFrom lays out a site: which buildings it has, where each stands and which
// way it faces.
//
// **Every building faces the middle**, which is the one rule that makes a ring of huts
// read as a place somebody lives rather than as seven identical objects. The keep is
// the exception it has to be: it *is* the middle, so it keeps the drawing's own
// orientation.
func settlementFrom(seed int64, cellX, cellZ int64, site settlementSite) Settlement {
	s := Settlement{
		Kind:    site.kind,
		CentreX: site.centreX,
		CentreZ: site.centreZ,
		Radius:  site.radius,
		Plateau: site.plateau,
	}
	h := hashLattice(seed+settlementLayoutSeedOffset, cellX, cellZ)
	bearings := int64(len(settlementBearings))

	if site.kind == SettlementCapital {
		s.Buildings = append(s.Buildings, site.building(BuildingKeep, 0, 0))

		// The hall and the smithy share a ring, so their bearings must differ by at
		// least a quarter of the circle or their thirteen-block footprints touch.
		hall := int(h % uint64(bearings))
		smithy := hall + 3 + int((h>>8)%uint64(bearings-5))
		s.Buildings = append(s.Buildings,
			site.buildingOnRing(BuildingHall, capitalPlotRadius, hall),
			site.buildingOnRing(BuildingSmithy, capitalPlotRadius, smithy),
		)

		start := int((h >> 16) % uint64(bearings))
		for i := range capitalHutCount {
			s.Buildings = append(s.Buildings,
				site.buildingOnRing(BuildingHut, capitalHutRingRadius, start+i*int(bearings)/capitalHutCount))
		}
		return s
	}

	public := BuildingSmithy
	if h&1 == 0 {
		public = BuildingHall
	}
	s.Buildings = append(s.Buildings, site.building(public, 0, 0))

	huts := villageMinHuts + int((h>>8)%villageHutVariants)
	start := int((h >> 16) % uint64(bearings))
	for i := range huts {
		s.Buildings = append(s.Buildings,
			site.buildingOnRing(BuildingHut, villageHutRingRadius, start+i*int(bearings)/huts))
	}
	return s
}

// buildingOnRing places one building at a bearing on a ring around the centre.
func (site settlementSite) buildingOnRing(kind BuildingKind, radius, bearing int) Building {
	dx, dz := ringOffset(radius, bearing)
	return site.building(kind, dx, dz)
}

// building places one building at an offset from the settlement's centre.
//
// The plot is the offset and the floor is one above the plateau, so the lowest course
// of a wall sits on the ground rather than replacing it. Everything else — centring the
// drawing on the plot, turning it, and carrying its slots round with it — belongs to
// the drawing and lives in [centredBuilding].
func (site settlementSite) building(kind BuildingKind, offsetX, offsetZ int64) Building {
	return centredBuilding(kind,
		site.centreX+offsetX, site.centreZ+offsetZ, int64(site.plateau)+1,
		facingTowardsCentre(offsetX, offsetZ))
}

// facingTowardsCentre is the quarter turn that points a door at the settlement's middle,
// for a building standing at (offsetX, offsetZ) from it.
//
// The larger axis wins, so a building on a diagonal faces the cardinal direction nearest
// the centre. A building *at* the centre has no direction to face and keeps the
// drawing's own.
func facingTowardsCentre(offsetX, offsetZ int64) Facing {
	if absInt64(offsetX) >= absInt64(offsetZ) {
		switch {
		case offsetX > 0:
			return FacingMinusX
		case offsetX < 0:
			return FacingPlusX
		default:
			return FacingPlusZ
		}
	}
	if offsetZ > 0 {
		return FacingMinusZ
	}
	return FacingPlusZ
}

// settlementBearings are twelve evenly spaced directions as Q16.16 unit vectors:
// (cos, sin) at thirty-degree steps.
//
// **A table rather than trigonometry**, for the reason there is no float anywhere else
// in this package: the layout of a village has to be bit-identical on every machine and
// after every compiler upgrade, and `math.Cos` promises neither. Twelve divides by two,
// three, four and six, which is every hut count this file asks for.
var settlementBearings = [12][2]int64{
	{65536, 0}, {56756, 32768}, {32768, 56756},
	{0, 65536}, {-32768, 56756}, {-56756, 32768},
	{-65536, 0}, {-56756, -32768}, {-32768, -56756},
	{0, -65536}, {32768, -56756}, {56756, -32768},
}

// ringOffset is the block offset of one bearing at one radius.
func ringOffset(radius, bearing int) (int64, int64) {
	v := settlementBearings[((bearing%len(settlementBearings))+len(settlementBearings))%len(settlementBearings)]
	return (int64(radius) * v[0]) >> fracBits, (int64(radius) * v[1]) >> fracBits
}

// placeSettlements writes every building that reaches into this chunk.
//
// The scan mirrors [placeTrees]: overscan by the feature's own extent, visit every
// candidate in the widened area, and let the clip drop what falls outside. The extent
// here is a settlement radius rather than a canopy radius, which is what makes the
// lattice worth having — at most four cells overlap a chunk grown by
// [capitalRadius], and all but a handful of them hold nothing.
func placeSettlements(seed int64, chunk *Chunk) {
	originX, originY, originZ := chunk.Coord.Origin()
	loX, hiX := settlementCellOf(originX-capitalRadius), settlementCellOf(originX+ChunkSize-1+capitalRadius)
	loZ, hiZ := settlementCellOf(originZ-capitalRadius), settlementCellOf(originZ+ChunkSize-1+capitalRadius)

	for cz := loZ; cz <= hiZ; cz++ {
		for cx := loX; cx <= hiX; cx++ {
			site, ok := settlementSiteAt(seed, cx, cz)
			if !ok || !site.reachesColumns(originX, originZ) {
				continue
			}
			// Every building's voxels lie between the plateau's first air block and
			// the tallest drawing above it, so a chunk entirely under or over that
			// band holds none of them.
			low, high := int64(site.plateau)+1, int64(site.plateau)+int64(tallestSchematic)
			if high < originY || low >= originY+ChunkSize {
				continue
			}
			for _, b := range settlementFrom(seed, cx, cz, site).Buildings {
				visitSchematic(b, func(x, y, z int64, block Block) {
					setSettlementBlock(chunk, x, y, z, block)
				})
			}
		}
	}
}

// reachesColumns reports whether a candidate's footprint touches the chunk column
// starting at (originX, originZ). A square test rather than a circular one, because a
// building sits inside the radius and the box is what a chunk is.
func (c settlementSite) reachesColumns(originX, originZ int64) bool {
	r := int64(c.radius)
	return c.centreX+r >= originX && c.centreX-r < originX+ChunkSize &&
		c.centreZ+r >= originZ && c.centreZ-r < originZ+ChunkSize
}

// tallestSchematic is the height of the tallest drawing, which bounds how far above a
// plateau any building reaches.
var tallestSchematic = max(
	max(hutSchematic.H, smithySchematic.H),
	max(hallSchematic.H, keepSchematic.H),
)

// setSettlementBlock writes one schematic voxel into a chunk if the voxel belongs to it
// and nothing is there.
//
// [setTreeBlock]'s clip without the log-over-leaves exception: a building is written in
// one pass over one drawing, so there is no ordering between two features to reconcile.
// Filling only air is what keeps a schematic from eating the ground it stands on — and
// it is why the `_` of a layer literal costs nothing, since writing air into air is
// already a no-op.
func setSettlementBlock(chunk *Chunk, worldX, worldY, worldZ int64, block Block) {
	originX, originY, originZ := chunk.Coord.Origin()
	localX, localY, localZ := worldX-originX, worldY-originY, worldZ-originZ
	if localX < 0 || localX >= ChunkSize || localY < 0 || localY >= ChunkSize || localZ < 0 || localZ >= ChunkSize {
		return
	}

	x, y, z := int(localX), int(localY), int(localZ)
	if chunk.At(x, y, z) == Air {
		chunk.Set(x, y, z, block)
	}
}

// squaredDistance is the horizontal distance between two columns, squared. Kept squared
// wherever a comparison is all that is wanted, so the integer square root is paid only
// where a length is.
func squaredDistance(ax, az, bx, bz int64) int64 {
	dx, dz := ax-bx, az-bz
	return dx*dx + dz*dz
}

// isqrt is the integer square root: the largest n with n² ≤ v, and zero for a negative
// v.
//
// Newton's method from a shift-based first guess, which converges in a handful of
// iterations and — being integer throughout — gives the same answer on every machine.
// `math.Sqrt` would not: it is exact for the values here, but reaching for a float in
// this package is how a generator starts drifting between builds.
func isqrt(v int64) int64 {
	if v <= 0 {
		return 0
	}
	n := int64(1)
	for n*n < v {
		n <<= 1
	}
	for {
		next := (n + v/n) / 2
		if next >= n {
			return n
		}
		n = next
	}
}
