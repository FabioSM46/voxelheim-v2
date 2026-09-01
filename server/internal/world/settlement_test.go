package world

import (
	"reflect"
	"slices"
	"testing"
)

// The sample seed for everything here. The same one the climate, water and surface
// sweeps use, so a failure can be read beside the numbers those files record.
const settlementTestSeed = 0x5EED

// generatedWorld is a lazily generated patch of chunks, so a test that asks about a few
// hundred scattered voxels pays for each chunk once. Generation is the millisecond-scale
// part of this package.
type generatedWorld struct {
	seed   int64
	chunks map[Coord]*Chunk
}

func newGeneratedWorld(seed int64) *generatedWorld {
	return &generatedWorld{seed: seed, chunks: map[Coord]*Chunk{}}
}

func (w *generatedWorld) at(x, y, z int64) Block {
	coord := ChunkOf(x, y, z)
	chunk, ok := w.chunks[coord]
	if !ok {
		chunk = Generate(w.seed, coord)
		w.chunks[coord] = chunk
	}
	return chunk.At(Local(x), Local(y), Local(z))
}

// theCapital is the capital of a seed. There is always one, so there is nothing to
// fail: [CapitalAt] returns no `bool` and this helper keeps `t` only because every
// caller reads as an assertion.
func theCapital(t *testing.T, seed int64) Settlement {
	t.Helper()
	return CapitalAt(seed)
}

// TestEverySeedHasACapitalNearTheOriginColumn is the one existence guarantee in this
// file, and the reason [capitalSiteAt] ranks candidates rather than refusing them.
//
// **The distance is measured from the lattice origin, which is no longer where anybody
// starts.** The walk used to be the point: the band existed so that a new player standing
// on the origin column had somewhere to walk to. #519 moved the spawn onto the capital's
// own gate square, so what the band does now is spread the capital off the lattice's
// zero — the same arithmetic and a different claim.
//
// The three seeds the issue names are checked by name; the sweep behind them is what
// says the guarantee is about the lattice rather than about three lucky worlds. The
// bound is the *hashed offset's* bound, so it holds for the fallback capital too.
func TestEverySeedHasACapitalNearTheOriginColumn(t *testing.T) {
	t.Parallel()

	for _, seed := range []int64{1, 2, settlementTestSeed} {
		assertCapital(t, seed)
	}
	for seed := int64(3); seed <= 200; seed++ {
		assertCapital(t, seed)
	}
}

func assertCapital(t *testing.T, seed int64) {
	t.Helper()

	s := theCapital(t, seed)
	if s.Kind != SettlementCapital {
		t.Fatalf("seed %#x: the capital's cell holds a %v", seed, s.Kind)
	}

	distance := isqrt(squaredDistance(originColumnX, originColumnZ, s.CentreX, s.CentreZ))
	if distance < capitalMinSpawnDistance || distance > capitalMaxSpawnDistance {
		t.Errorf("seed %#x: the capital is %d blocks from the origin column, outside [%d, %d]",
			seed, distance, capitalMinSpawnDistance, capitalMaxSpawnDistance)
	}
	if s.Plateau < settlementMinPlateau {
		t.Errorf("seed %#x: the capital's plateau is at %d, at or under the water line", seed, s.Plateau)
	}
	if s.Radius != capitalRadius {
		t.Errorf("seed %#x: the capital's radius is %d, want %d", seed, s.Radius, capitalRadius)
	}
	// A keep, a hall, a smithy, a stable and six huts.
	if want := 4 + capitalHutCount; len(s.Buildings) != want {
		t.Errorf("seed %#x: the capital has %d buildings, want %d", seed, len(s.Buildings), want)
	}
}

// TestOnlyTheOriginCellHoldsACapital pins the other half of "there is one capital".
func TestOnlyTheOriginCellHoldsACapital(t *testing.T) {
	t.Parallel()

	villages := 0
	for cz := int64(-4); cz <= 4; cz++ {
		for cx := int64(-4); cx <= 4; cx++ {
			s, ok := SettlementAt(settlementTestSeed, cx, cz)
			if !ok {
				continue
			}
			if (s.Kind == SettlementCapital) != isCapitalCell(cx, cz) {
				t.Fatalf("cell (%d, %d) holds a %v", cx, cz, s.Kind)
			}
			if s.Kind == SettlementVillage {
				villages++
			}
		}
	}
	if villages == 0 {
		t.Fatal("eighty cells around the origin hold no village at all; the density rule is not being exercised")
	}
}

func TestEveryCapitalAndNoVillageHasAStable(t *testing.T) {
	t.Parallel()

	for seed := int64(1); seed <= 40; seed++ {
		capital := theCapital(t, seed)
		stables := 0
		for _, b := range capital.Buildings {
			if b.Kind == BuildingStable {
				stables++
			}
		}
		if stables != 1 {
			t.Errorf("seed %d capital has %d stables, want one", seed, stables)
		}
	}

	checked := 0
	for cz := int64(-8); cz <= 8; cz++ {
		for cx := int64(-8); cx <= 8; cx++ {
			s, ok := SettlementAt(settlementTestSeed, cx, cz)
			if !ok || s.Kind != SettlementVillage {
				continue
			}
			checked++
			for _, b := range s.Buildings {
				if b.Kind == BuildingStable {
					t.Errorf("village in cell (%d, %d) has a stable", cx, cz)
				}
			}
		}
	}
	if checked == 0 {
		t.Fatal("the village sweep found nothing; absence of stables was not exercised")
	}
}

// TestTheLatticeIsTheSameWorldEveryTime pins where this seed actually puts things.
//
// **Every other test here checks a property, and a property is blind to which hash
// produced it.** Swapping the cell coordinates in the density hash, changing a seed
// offset by one, or transposing the two halves of the placement hash all leave the
// lattice statistically identical — the same density, the same rules, the same
// invariants — and give a different world. That is the change no property can catch and
// the one a stored world cannot survive: [WorldgenVersion] is bumped for exactly this,
// and a version that stayed at 6 while the villages moved would resolve a played-in
// world's deltas onto ground that is no longer there.
//
// The eight cells below are the sample, taken from the seed the rest of this file uses.
// Regenerate them deliberately, alongside a version bump, or not at all.
func TestTheLatticeIsTheSameWorldEveryTime(t *testing.T) {
	t.Parallel()

	for _, want := range []struct {
		cellX, cellZ     int64
		held             bool
		kind             SettlementKind
		centreX, centreZ int64
		plateau          int
		buildings        int
	}{
		{cellX: 0, cellZ: 0, held: true, kind: SettlementCapital, centreX: 116, centreZ: 111, plateau: 63, buildings: 10},
		{cellX: 1, cellZ: 0, held: true, kind: SettlementVillage, centreX: 2118, centreZ: 236, plateau: 56, buildings: 4},
		{cellX: -2, cellZ: 3, held: true, kind: SettlementVillage, centreX: -3753, centreZ: 6892, plateau: 76, buildings: 6},
		{cellX: 0, cellZ: 1},
		{cellX: 1, cellZ: 1},
		{cellX: 2, cellZ: -1},
		{cellX: 3, cellZ: 3},
		{cellX: -1, cellZ: -2},
	} {
		got, held := SettlementAt(settlementTestSeed, want.cellX, want.cellZ)
		if held != want.held {
			t.Errorf("cell (%d, %d): holds a settlement = %v, want %v", want.cellX, want.cellZ, held, want.held)
			continue
		}
		if !want.held {
			continue
		}
		if got.Kind != want.kind || got.CentreX != want.centreX || got.CentreZ != want.centreZ ||
			got.Plateau != want.plateau || len(got.Buildings) != want.buildings {
			t.Errorf("cell (%d, %d) holds a %v at (%d, %d), plateau %d, %d buildings; want a %v at (%d, %d), plateau %d, %d buildings",
				want.cellX, want.cellZ, got.Kind, got.CentreX, got.CentreZ, got.Plateau, len(got.Buildings),
				want.kind, want.centreX, want.centreZ, want.plateau, want.buildings)
		}
	}
}

// TestTheGuardsBelowDescribeTheActualDrawings ties the half-footprint constants that the
// compile-time layout guards are written against back to the schematics they claim to
// describe.
//
// **A const expression cannot read a `var`, and the five drawings are built at init**, so
// the guards restate their widths as literals. That restating is the weak point: resize a
// schematic and the guards keep protecting the old one, silently. This is the one line of
// defence against that, and it is a test rather than a compile error for exactly the
// reason the numbers are literals in the first place.
func TestTheGuardsBelowDescribeTheActualDrawings(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		kind BuildingKind
		half int
	}{
		{BuildingHut, hutHalfFootprint},
		{BuildingSmithy, smithyHalfFootprint},
		{BuildingHall, hallHalfFootprint},
		{BuildingStable, stableHalfFootprint},
		{BuildingKeep, largestHalfFootprint},
	} {
		schematic := SchematicFor(tc.kind)
		if schematic.W != 2*tc.half+1 || schematic.D != 2*tc.half+1 {
			t.Errorf("the %v is %d×%d and the layout guards are written for a half-footprint of %d",
				tc.kind, schematic.W, schematic.D, tc.half)
		}
	}

	// The largest is the largest, which is what makes it usable as a bound.
	for _, drawing := range everySchematic() {
		if drawing.s.W > 2*largestHalfFootprint+1 || drawing.s.D > 2*largestHalfFootprint+1 {
			t.Errorf("the %v is %d×%d, wider than the largest half-footprint of %d allows",
				drawing.kind, drawing.s.W, drawing.s.D, largestHalfFootprint)
		}
	}

	// **[plotRingNearestAxis] reads one number out of [settlementBearings] and nothing
	// checked that it is the number it claims to be.** A building on a ring separates
	// from a centred one on whichever axis it is *further* out on, so each bearing's
	// useful leg is the larger of its two — and the worst bearing is the one whose
	// larger leg is smallest, which on a twelve-spoke table is cos 30°. Rewrite the
	// table with sixteen bearings and the number moves; nothing else here would say so.
	worst := int64(1) << fracBits
	for _, v := range settlementBearings {
		if leg := max(absInt64(v[0]), absInt64(v[1])); leg < worst {
			worst = leg
		}
	}
	if worst != 56756 {
		t.Errorf("the smallest larger leg in settlementBearings is %d and plotRingNearestAxis is written for 56756", worst)
	}
	if got, want := int(plotRingNearestAxis), (capitalPlotRadius*int(worst))>>fracBits; got != want {
		t.Errorf("plotRingNearestAxis is %d and the bearing table puts a plot-ring building %d out on its nearer axis", got, want)
	}

	for _, margin := range []struct {
		name string
		got  int
	}{
		{"plateau to hut ring", capitalPlateauMargin},
		{"hut ring to plot ring", capitalRingMargin},
		{"plot ring to keep", capitalKeepMargin},
		{"adjacent plot bearings", capitalPlotMargin},
	} {
		if margin.got < 1 {
			t.Errorf("%s clearance is %d blocks, want at least one", margin.name, margin.got)
		}
	}

	// ceil(3√2) = 5: a 7-across footprint centred on a ring reaches this far past it at
	// its corner, which is the clearance both hut-ring guards subtract.
	if got := hutHalfFootprint * hutHalfFootprint * 2; got > hutRingClearance*hutRingClearance {
		t.Errorf("a hut's corner reaches sqrt(%d) past its ring and the guards allow %d", got, hutRingClearance)
	}
	if got := (hutRingClearance - 1) * (hutRingClearance - 1); got >= hutHalfFootprint*hutHalfFootprint*2 {
		t.Errorf("the hut ring clearance of %d is larger than the corner needs; it should be the ceiling, not padding", hutRingClearance)
	}
}

// TestTheCapitalsPlanIsTheSamePlanEveryTime is the capital's half of the positional pin,
// and it did not exist.
//
// **[TestTheLatticeIsTheSameWorldEveryTime] records a settlement's centre, plateau and
// building *count*, and every one of the capital's geometry constants can move without
// disturbing any of the three.** Measured against the mutations that were green before
// this test: `capitalRadius` 56 → 60 changes the terrain around spawn outright;
// `capitalPlotRadius` 25 → 20 walks the hall and the smithy five blocks in;
// `capitalHutRingRadius` 40 → 52 pushes the ring twelve blocks out; fixing the hut ring's
// `start` bearing to zero orients every capital in every world identically; replacing the
// even hut spacing with `start+i` bunches all six into a 150° arc; and putting the keep on
// the ring instead of at the origin means a capital has no middle. All ten buildings
// survive each of those, so a count cannot see any of it, and no golden fixture covers the
// capital — `chunk_golden_settlement.bin` is a village nearly nine kilometres out.
//
// So the plan is written down. The origins are world coordinates, which folds in the
// centre, the plateau, the two ring radii, the bearings and the facing rule at once.
func TestTheCapitalsPlanIsTheSamePlanEveryTime(t *testing.T) {
	t.Parallel()

	// **The four constants are pinned as literals, not read from themselves.** This is
	// the lesson the blend band already learned: a bound derived from the constant it is
	// checking asserts only that the code agrees with itself, and moves when it moves.
	for _, c := range []struct {
		name string
		got  int
		want int
	}{
		{"capitalRadius", capitalRadius, 69},
		{"capitalPlotRadius", capitalPlotRadius, 50},
		{"capitalHutRingRadius", capitalHutRingRadius, 63},
		{"capitalHutCount", capitalHutCount, 6},
	} {
		if c.got != c.want {
			t.Errorf("%s is %d, want %d — the plan below is written for that number, and worldgen 22 is the world it produces",
				c.name, c.got, c.want)
		}
	}

	s := theCapital(t, settlementTestSeed)

	want := []struct {
		kind                      BuildingKind
		originX, originY, originZ int64
		facing                    Facing
	}{
		{BuildingKeep, 85, 64, 80, FacingPlusZ},
		{BuildingHall, 160, 64, 105, FacingMinusX},
		{BuildingSmithy, 68, 64, 132, FacingPlusX},
		{BuildingStable, 63, 64, 77, FacingPlusX},
		{BuildingHut, 144, 64, 162, FacingMinusZ},
		{BuildingHut, 81, 64, 162, FacingMinusZ},
		{BuildingHut, 50, 64, 108, FacingPlusX},
		{BuildingHut, 81, 64, 53, FacingPlusZ},
		{BuildingHut, 144, 64, 53, FacingPlusZ},
		{BuildingHut, 176, 64, 108, FacingMinusX},
	}
	if len(s.Buildings) != len(want) {
		t.Fatalf("the capital has %d buildings, want %d", len(s.Buildings), len(want))
	}
	for i, b := range s.Buildings {
		w := want[i]
		if b.Kind != w.kind || b.OriginX != w.originX || b.OriginY != w.originY ||
			b.OriginZ != w.originZ || b.Facing != w.facing {
			t.Errorf("the capital's building %d is a %v at (%d, %d, %d) facing %d; want a %v at (%d, %d, %d) facing %d",
				i, b.Kind, b.OriginX, b.OriginY, b.OriginZ, b.Facing,
				w.kind, w.originX, w.originY, w.originZ, w.facing)
		}
	}

	// The keep is the middle. It is the one building whose plot is the centre, which is
	// why it keeps its drawing's own orientation instead of turning to face anything.
	keep := s.Buildings[0]
	kw, kd := rotatedFootprint(SchematicFor(BuildingKeep), keep.Facing)
	if keep.OriginX+int64(kw/2) != s.CentreX || keep.OriginZ+int64(kd/2) != s.CentreZ {
		t.Errorf("the keep is centred on (%d, %d) and the capital's centre is (%d, %d)",
			keep.OriginX+int64(kw/2), keep.OriginZ+int64(kd/2), s.CentreX, s.CentreZ)
	}

	// And the six huts are spread round the whole circle rather than bunched on one
	// side of it: no two share a bearing, and the arc they span is the whole of it.
	seen := map[[2]int64]bool{}
	for _, b := range s.Buildings[4:] {
		w, d := rotatedFootprint(SchematicFor(b.Kind), b.Facing)
		offset := [2]int64{
			b.OriginX + int64(w/2) - s.CentreX,
			b.OriginZ + int64(d/2) - s.CentreZ,
		}
		if seen[offset] {
			t.Errorf("two of the capital's huts stand at the same offset %v from its centre", offset)
		}
		seen[offset] = true
	}
	// Six evenly spaced bearings reach both signs on both axes; six bunched into half
	// the circle cannot.
	var minX, maxX, minZ, maxZ int64
	for offset := range seen {
		minX, maxX = min(minX, offset[0]), max(maxX, offset[0])
		minZ, maxZ = min(minZ, offset[1]), max(maxZ, offset[1])
	}
	if minX >= 0 || maxX <= 0 || minZ >= 0 || maxZ <= 0 {
		t.Errorf("the capital's huts span x %d..%d and z %d..%d from its centre; six evenly spaced bearings reach past it on both axes",
			minX, maxX, minZ, maxZ)
	}
}

// TestVillagesAreAboutOneCellInThree is [villageInverseDensity], measured.
//
// The constant is a design decision — "a long walk with something at the end of it
// rather than a suburb" — and nothing read it back. A density of one in one is a world
// where every cell has a village, which breaks no invariant in this file: the sites are
// still refused where the ground is wrong, the buildings still stand clear, the
// orderings still hold. It is simply a different game.
//
// The bounds are wide because this is a hash and not a shuffle: the point is to catch a
// density that is out by a factor, not to pin the sampling error of one seed.
func TestVillagesAreAboutOneCellInThree(t *testing.T) {
	t.Parallel()

	const cells = 40

	proposed, total := 0, 0
	for cz := int64(-cells); cz <= cells; cz++ {
		for cx := int64(-cells); cx <= cells; cx++ {
			if isCapitalCell(cx, cz) {
				continue
			}
			total++
			if _, ok := settlementCandidateAt(settlementTestSeed, cx, cz); ok {
				proposed++
			}
		}
	}

	low, high := total/villageInverseDensity*3/4, total/villageInverseDensity*5/4
	if proposed < low || proposed > high {
		t.Errorf("%d of %d cells propose a village (one in %.2f); one in %d wants between %d and %d",
			proposed, total, float64(total)/float64(proposed), villageInverseDensity, low, high)
	}
}

// TestTheCapitalUsesALaterOffsetWhenTheFirstIsRefused is what
// [capitalSiteAttempts] buys, and the height field is why it has to be pinned here.
//
// **A capital that settles on its second offset is a capital the height path must still
// find.** [settlementMayReach] is the hash-only filter every column in the world runs
// before it will pay for a site, and it therefore has to consider *every* attempt: a
// version that looked at the first offset only would leave a capital standing on ground
// nobody flattened, with its buildings written into a hillside. Nothing in this file saw
// that, because the sample seed's capital happens to take its first offset — so the
// seeds below were searched for, and the flat-ground assertion is run against one of
// them.
func TestTheCapitalUsesALaterOffsetWhenTheFirstIsRefused(t *testing.T) {
	t.Parallel()

	cellX, cellZ := settlementCellOf(originColumnX), settlementCellOf(originColumnZ)

	late := 0
	for seed := int64(1); seed <= 200; seed++ {
		site := capitalSiteAt(seed, cellX, cellZ)
		first := capitalCandidateAt(seed, cellX, cellZ, 0)
		if site.centreX != first.centreX || site.centreZ != first.centreZ {
			late++
		}
	}
	if late == 0 {
		t.Fatal("no seed in two hundred puts its capital anywhere but the first offset; the extra attempts buy nothing")
	}

	// Seed 12 is one of them. Its capital's plateau has to reach the columns under it
	// exactly as the sample seed's does — which is a statement about
	// settlementMayReach, not about capitalSiteAt, because the height field never asks
	// which attempt won.
	const lateSeed = 12

	site := capitalSiteAt(lateSeed, cellX, cellZ)
	if first := capitalCandidateAt(lateSeed, cellX, cellZ, 0); site.centreX == first.centreX && site.centreZ == first.centreZ {
		t.Fatalf("seed %d was chosen because its capital is not on its first offset, and now it is", lateSeed)
	}
	s, ok := SettlementAt(lateSeed, cellX, cellZ)
	if !ok {
		t.Fatalf("seed %d has no capital", lateSeed)
	}
	assertTheFallbackIsTheFirstOffset(t, cellX, cellZ)
	for bearing := range len(settlementBearings) {
		for _, distance := range []int{0, s.Radius / 2, s.Radius} {
			dx, dz := ringOffset(distance, bearing)
			x, z := s.CentreX+dx, s.CentreZ+dz
			if got := HeightAt(lateSeed, x, z); got != s.Plateau {
				t.Fatalf("seed %d: (%d, %d) is %d blocks inside the capital and stands at %d, and the plateau is %d — the height field is looking at a different offset",
					lateSeed, x, z, distance, got, s.Plateau)
			}
		}
	}
}

// assertTheFallbackIsTheFirstOffset pins which offset a capital gets when none of the
// four is acceptable.
//
// **The doc on [capitalSiteAt] calls this a last resort, and measured it is the modal
// outcome: 85 of the first 200 seeds reach it.** Both facts are worth having — it is
// genuinely the branch taken when every ranked attempt failed, *and* it is what most
// worlds get, because the relief field's lattice is 768 blocks wide while every candidate
// is inside 200 of spawn, so four correlated rolls fail together far more often than four
// independent ones would.
//
// Nothing asserted which candidate it returns. Returning the *fourth* offset instead of
// the first leaves every existing assertion intact — the distance bound, the plateau
// floor, the radius and the building count are properties of any offset — while moving
// the capital of 85 worlds. Seed 1's would go from (86, -148) to (132, 99).
func assertTheFallbackIsTheFirstOffset(t *testing.T, cellX, cellZ int64) {
	t.Helper()

	fellBack := 0
	for seed := int64(1); seed <= 200; seed++ {
		acceptable := false
		for attempt := range capitalSiteAttempts {
			candidate := capitalCandidateAt(seed, cellX, cellZ, attempt)
			if unloweredHeightAt(seed, candidate.centreX, candidate.centreZ) >= settlementMinPlateau &&
				reliefAt(seed, candidate.centreX, candidate.centreZ) <= settlementReliefLimit {
				acceptable = true
				break
			}
		}
		if acceptable {
			continue
		}
		fellBack++

		first := capitalCandidateAt(seed, cellX, cellZ, 0)
		site := capitalSiteAt(seed, cellX, cellZ)
		if site.centreX != first.centreX || site.centreZ != first.centreZ {
			t.Fatalf("seed %d has no acceptable offset and its capital stands at (%d, %d); the fallback is the first offset, at (%d, %d)",
				seed, site.centreX, site.centreZ, first.centreX, first.centreZ)
		}
		// And the floor is lifted rather than the site refused, which is the other half
		// of "the capital always exists": lifting is the fail-safe direction and
		// lowering is not.
		if site.plateau < settlementMinPlateau {
			t.Fatalf("seed %d fell back to a plateau at %d, under the floor of %d", seed, site.plateau, settlementMinPlateau)
		}
	}

	if fellBack == 0 {
		t.Fatal("no seed in two hundred reaches the fallback; the branch this asserts is not being exercised")
	}
}

// TestAVillageCellWhoseSiteIsRefusedHoldsNothing is the rejection half of the lattice.
//
// **A cell that proposes a village and yields none is the case worth pinning**, because
// it is the one a refactor turns into a village standing in a lake: the proposal is a
// hash and the refusal is three fields, so it is the refusal that can be lost. The sweep
// finds a real instance of each rule rather than constructing one, so a rule that had
// stopped firing anywhere would fail here.
func TestAVillageCellWhoseSiteIsRefusedHoldsNothing(t *testing.T) {
	t.Parallel()

	// **A wide sweep, because one of the three cases is rare and it is rare for a
	// reason worth knowing.** A site that clears the relief rule has a small
	// amplitude by construction — `amplitudeAt` interpolates on the same field — so
	// its height is close to `baseHeight`, and a plateau under the water line is
	// therefore much less likely on the sites that got that far. Twenty-five cells out
	// found none at all; sixty are needed before the second rule fires anywhere.
	const cells = 30

	refusedByRelief, refusedByWater, refusedByRiverCentre, refusedByRiverRing, accepted := 0, 0, 0, 0, 0
	for cz := int64(-cells); cz <= cells; cz++ {
		for cx := int64(-cells); cx <= cells; cx++ {
			if isCapitalCell(cx, cz) {
				continue
			}
			candidate, proposed := settlementCandidateAt(settlementTestSeed, cx, cz)
			s, held := SettlementAt(settlementTestSeed, cx, cz)
			if !proposed {
				if held {
					t.Fatalf("cell (%d, %d) holds a settlement it never proposed", cx, cz)
				}
				continue
			}

			relief := reliefAt(settlementTestSeed, candidate.centreX, candidate.centreZ)
			plateau := unloweredHeightAt(settlementTestSeed, candidate.centreX, candidate.centreZ)
			switch {
			case relief > settlementReliefLimit:
				refusedByRelief++
			case plateau < settlementMinPlateau:
				refusedByWater++
			case riverAt(settlementTestSeed, candidate.centreX, candidate.centreZ):
				refusedByRiverCentre++
			case riverCrossesSite(settlementTestSeed, candidate):
				refusedByRiverRing++
			default:
				if !held {
					t.Fatalf("cell (%d, %d) passes every rule and still holds nothing", cx, cz)
				}
				accepted++
				// **The height floor is a margin over the sea line, not the sea line.**
				// A plateau three blocks up is what keeps the tide off a village's
				// doorstep, and the difference is invisible to the classification
				// above, which reads the same constant the rule does.
				if s.Plateau < seaLevel+3 {
					t.Fatalf("the settlement in cell (%d, %d) stands at %d, and the sea is at %d",
						cx, cz, s.Plateau, seaLevel)
				}
				continue
			}
			if held {
				t.Fatalf("cell (%d, %d) was refused its site and holds a settlement anyway", cx, cz)
			}
		}
	}

	// **Every rule needs a real instance, and the two river ones are why this
	// assertion is worth more than it looks.** A rule that has stopped firing anywhere
	// is a rule that can be deleted with a green suite: the sweep that only checked
	// accepted sites would have said nothing at all if `riverCrossesSite` had been
	// removed from [settlementSiteAt], because the columns it rejects are not in the
	// results to be looked at. The centre sample and the two rings are counted apart
	// for the same reason — the rings are the sampled half, and a version that read
	// only the centre would still refuse nine cells in this sweep and look healthy.
	for _, rule := range []struct {
		name  string
		count int
	}{
		{"relief", refusedByRelief},
		{"a plateau under the water line", refusedByWater},
		{"a river through the centre", refusedByRiverCentre},
		{"a river through the sampled rings", refusedByRiverRing},
		{"accepted", accepted},
	} {
		if rule.count == 0 {
			t.Errorf("no cell in the sweep was decided by %q; that rule is not being exercised", rule.name)
		}
	}
}

// TestNoSettlementIsPlantedOnAnObviousRiver measures what the sampled river test in
// [riverCrossesSite] actually catches.
//
// **It asserts the centre and no more, deliberately.** The sample is thirteen columns
// and the disc is ten thousand, so a channel clipping the edge of a village genuinely
// survives — the comment on that function says so and says why it is harmless. What must
// never happen is a settlement centred in a channel, because that is the case the sample
// cannot miss and the one a reader would see.
func TestNoSettlementIsPlantedOnAnObviousRiver(t *testing.T) {
	t.Parallel()

	checked := 0
	for cz := int64(-12); cz <= 12; cz++ {
		for cx := int64(-12); cx <= 12; cx++ {
			s, ok := SettlementAt(settlementTestSeed, cx, cz)
			if !ok || s.Kind == SettlementCapital {
				continue
			}
			checked++
			if riverAt(settlementTestSeed, s.CentreX, s.CentreZ) {
				t.Errorf("the village in cell (%d, %d) is centred in a river channel", cx, cz)
			}
		}
	}
	if checked == 0 {
		t.Fatal("no village in the sweep; the assertion above ran against nothing")
	}
}

// riverSampleRings is the twelve ring columns [riverCrossesSite] reads, written out.
//
// **Written out rather than derived, because the version that derived them moved with the
// code.** The helper this replaces re-ran the production loop from
// [settlementRiverSampleRings] and the same `radius * ring / rings` expression, so
// collapsing the two rings into one, or reading both of them at the full radius, changed
// the census of a 61×61 block of cells from 530 settlements to 524 — a different world,
// with no [WorldgenVersion] bump — and the test moved along with it and stayed green.
// Bearings were already pinned, because the interleave is spelled out below; radii and
// ring count were not.
//
// Two rings at half the radius and the full radius, interleaved so twelve samples cover
// twelve directions rather than six directions twice. With the centre that is the
// thirteen columns the function's own doc argues for at 1.11 µs against 1.73 µs for
// twenty-five.
func riverSampleRings(radius int) []struct{ radius, bearing int } {
	var out []struct{ radius, bearing int }
	for _, bearing := range []int{0, 2, 4, 6, 8, 10} {
		out = append(out, struct{ radius, bearing int }{radius / 2, bearing})
	}
	for _, bearing := range []int{1, 3, 5, 7, 9, 11} {
		out = append(out, struct{ radius, bearing int }{radius, bearing})
	}
	return out
}

// TestTheRiverSampleIsThirteenColumnsInTwoInterleavedRings is the geometry of the sample,
// pinned as a number rather than as an expression.
//
// The centre plus twelve, and the two radii are half and whole. Both halves of that are
// load-bearing and neither was asserted: one ring reads six directions instead of twelve,
// and two rings at the same radius read a circle instead of a disc. Each is a different
// world for the price of a constant.
func TestTheRiverSampleIsThirteenColumnsInTwoInterleavedRings(t *testing.T) {
	t.Parallel()

	// Thirteen distinct columns, for a village-sized disc.
	distinct := map[[2]int64]bool{{0, 0}: true}
	for _, sample := range riverSampleRings(villageRadius) {
		dx, dz := ringOffset(sample.radius, sample.bearing)
		distinct[[2]int64{dx, dz}] = true
	}
	if len(distinct) != 13 {
		t.Errorf("the river sample reads %d distinct columns of a village-sized disc, want 13", len(distinct))
	}

	// And the production predicate agrees with that set everywhere. This is what makes
	// the list above a pin rather than a second opinion: a sample set that differs from
	// the one [riverCrossesSite] actually reads shows up as a disagreement on some
	// candidate in the sweep.
	agreed, refused := 0, 0
	for cz := int64(-30); cz <= 30; cz++ {
		for cx := int64(-30); cx <= 30; cx++ {
			if isCapitalCell(cx, cz) {
				continue
			}
			candidate, proposed := settlementCandidateAt(settlementTestSeed, cx, cz)
			if !proposed {
				continue
			}
			want := riverAt(settlementTestSeed, candidate.centreX, candidate.centreZ)
			if !want {
				for _, sample := range riverSampleRings(candidate.radius) {
					dx, dz := ringOffset(sample.radius, sample.bearing)
					if riverAt(settlementTestSeed, candidate.centreX+dx, candidate.centreZ+dz) {
						want = true
						break
					}
				}
			}
			if got := riverCrossesSite(settlementTestSeed, candidate); got != want {
				t.Fatalf("cell (%d, %d): riverCrossesSite says %v and the thirteen columns say %v", cx, cz, got, want)
			}
			agreed++
			if want {
				refused++
			}
		}
	}
	if agreed == 0 || refused == 0 {
		t.Fatalf("the sweep compared %d candidates of which %d were refused; both are needed", agreed, refused)
	}
}

// TestTheRiverCentreSampleIsTheOnlyThingRefusingSomeSites is the half of
// [riverCrossesSite] that its own rings cannot cover for it.
//
// **A channel through a village's middle is usually wide enough to clip a ring sample
// too, and "usually" is what makes this worth pinning.** Deleting the centre read left
// every sweep in this file green: within thirty cells of spawn there is no site the
// centre refuses that the twelve ring samples would not have refused anyway, so the
// statement "no village is centred in a channel" was true of the world without being
// enforced by the code. The two cells below were found by widening the search to sixty
// cells out — they are the ones where the centre is in a river and all twelve ring
// samples are dry — and they are the whole evidence that the first line of that function
// does anything.
//
// The mirror cells are the opposite case, and they are here so a reader can see that
// both halves are load-bearing rather than take it on trust.
func TestTheRiverCentreSampleIsTheOnlyThingRefusingSomeSites(t *testing.T) {
	t.Parallel()

	ringsRefuse := func(c settlementCandidate) bool {
		for _, sample := range riverSampleRings(c.radius) {
			dx, dz := ringOffset(sample.radius, sample.bearing)
			if riverAt(settlementTestSeed, c.centreX+dx, c.centreZ+dz) {
				return true
			}
		}
		return false
	}

	for _, tc := range []struct {
		cellX, cellZ int64
		centreIsWet  bool
		ringsAreWet  bool
	}{
		// Refused by the centre alone: the rings are all dry.
		{cellX: -20, cellZ: -52, centreIsWet: true},
		{cellX: 36, cellZ: -17, centreIsWet: true},
		// Refused by the rings alone: the centre is dry.
		{cellX: -6, cellZ: -6, ringsAreWet: true},
		{cellX: -5, cellZ: 1, ringsAreWet: true},
		{cellX: 7, cellZ: 1, ringsAreWet: true},
	} {
		candidate, proposed := settlementCandidateAt(settlementTestSeed, tc.cellX, tc.cellZ)
		if !proposed {
			t.Errorf("cell (%d, %d) was chosen because it proposes a village and it no longer does", tc.cellX, tc.cellZ)
			continue
		}
		if got := riverAt(settlementTestSeed, candidate.centreX, candidate.centreZ); got != tc.centreIsWet {
			t.Errorf("cell (%d, %d): its centre is in a channel = %v, want %v", tc.cellX, tc.cellZ, got, tc.centreIsWet)
		}
		if got := ringsRefuse(candidate); got != tc.ringsAreWet {
			t.Errorf("cell (%d, %d): a ring sample is in a channel = %v, want %v", tc.cellX, tc.cellZ, got, tc.ringsAreWet)
		}
		if !riverCrossesSite(settlementTestSeed, candidate) {
			t.Errorf("cell (%d, %d) is on a channel by one half of the test and the whole test says otherwise", tc.cellX, tc.cellZ)
		}
		if _, held := SettlementAt(settlementTestSeed, tc.cellX, tc.cellZ); held {
			t.Errorf("cell (%d, %d) is on a channel and holds a settlement anyway", tc.cellX, tc.cellZ)
		}
	}
}

// TestASettlementStandsOnFlatGroundThatEasesBackIntoTheLand is the plateau, measured
// through the exported height field rather than through the rule that produces it.
//
// Three statements, and the middle one is the one a naive implementation gets wrong:
// inside the radius every column is the plateau exactly, the blend band is monotone
// between the plateau and the land, and outside it nothing has moved at all.
func TestASettlementStandsOnFlatGroundThatEasesBackIntoTheLand(t *testing.T) {
	t.Parallel()

	s := theCapital(t, settlementTestSeed)
	for dz := int64(-s.Radius); dz <= int64(s.Radius); dz++ {
		for dx := int64(-s.Radius); dx <= int64(s.Radius); dx++ {
			if dx*dx+dz*dz > int64(s.Radius*s.Radius) {
				continue
			}
			if got := HeightAt(settlementTestSeed, s.CentreX+dx, s.CentreZ+dz); got != s.Plateau {
				t.Fatalf("(%d, %d) inside the capital is at height %d, want the plateau at %d",
					s.CentreX+dx, s.CentreZ+dz, got, s.Plateau)
			}
		}
	}

	// A radial walk out of the settlement on each of the twelve bearings: still the
	// plateau at the rim, back to the untouched land at the end of the blend, and
	// never outside the two in between.
	for bearing := range len(settlementBearings) {
		for distance := s.Radius; distance <= s.Radius+settlementBlendBlocks+8; distance++ {
			dx, dz := ringOffset(distance, bearing)
			x, z := s.CentreX+dx, s.CentreZ+dz
			got := HeightAt(settlementTestSeed, x, z)
			natural := unloweredHeightAt(settlementTestSeed, x, z)

			low, high := min(s.Plateau, natural), max(s.Plateau, natural)
			if got < low || got > high {
				t.Fatalf("(%d, %d) is %d blocks out and stands at %d, outside the plateau %d and the land %d",
					x, z, distance, got, s.Plateau, natural)
			}
			// Past the blend the settlement has no say at all, so the only thing that
			// may still move the column is a basin — never a river, which this band
			// suppresses out to exactly the same distance.
			if distance > s.Radius+settlementBlendBlocks && got > natural {
				t.Fatalf("(%d, %d) is beyond the blend and stands at %d, above the land's %d", x, z, got, natural)
			}
		}
	}

	assertTheBlendIsSixteenBlocksOfSmoothstep(t, s)
}

// assertTheBlendIsSixteenBlocksOfSmoothstep pins the two things the walk above cannot
// see: how wide the band is, and what shape it has.
//
// **"Between the plateau and the land, and monotone" is satisfied by a straight ramp
// half as wide.** Both of those were free variables — halving [settlementBlendBlocks]
// and replacing `smoothstep` with the raw parameter each left the suite green — and both
// are the whole of what the constant's doc comment claims: sixteen blocks, because a
// plateau that ended at a cliff would put a wall around every village, and a smoothstep
// rather than a ramp, because the derivative jump at the rim is visible from a long way
// off.
//
// The bearing is chosen rather than fixed: the shape is only observable where the
// plateau and the land actually differ, so this takes the steepest of the twelve and
// refuses to assert anything if none of them is steep enough to measure.
func assertTheBlendIsSixteenBlocksOfSmoothstep(t *testing.T, s Settlement) {
	t.Helper()

	const minimumDrop = 8

	bearing, drop := -1, 0
	for b := range len(settlementBearings) {
		dx, dz := ringOffset(s.Radius+settlementBlendBlocks, b)
		x, z := s.CentreX+dx, s.CentreZ+dz
		land, _, _ := loweredHeightAt(settlementTestSeed, x, z, unloweredHeightAt(settlementTestSeed, x, z), ClimateAt(settlementTestSeed, x, z))
		if d := max(land-s.Plateau, s.Plateau-land); d > drop {
			bearing, drop = b, d
		}
	}
	if drop < minimumDrop {
		t.Fatalf("no bearing out of the capital falls more than %d blocks over its blend band; the shape cannot be measured", drop)
	}

	height := func(distance int) int {
		dx, dz := ringOffset(distance, bearing)
		return HeightAt(settlementTestSeed, s.CentreX+dx, s.CentreZ+dz)
	}
	natural := func(distance int) int {
		dx, dz := ringOffset(distance, bearing)
		x, z := s.CentreX+dx, s.CentreZ+dz
		h, _, _ := loweredHeightAt(settlementTestSeed, x, z, unloweredHeightAt(settlementTestSeed, x, z), ClimateAt(settlementTestSeed, x, z))
		return h
	}

	// **The band is exactly this wide, read from the rule's own third answer.** `near`
	// is not decoration: it is what suppresses basins and river channels, so the
	// distance it stops being true at is the distance at which the settlement lets the
	// water features back in. Measuring the width from the height instead cannot see
	// it, because a basin outside the band moves the column too and the two are
	// indistinguishable from a number.
	nearAt := func(distance int) bool {
		dx, dz := ringOffset(distance, bearing)
		x, z := s.CentreX+dx, s.CentreZ+dz
		_, _, near := settlementShapeAt(settlementTestSeed, x, z,
			unloweredHeightAt(settlementTestSeed, x, z), ClimateAt(settlementTestSeed, x, z))
		return near
	}
	// **Fifteen and sixteen, written out rather than derived from the constant.** A
	// width computed from [settlementBlendBlocks] asserts that the rule agrees with
	// itself, which halving the constant satisfies perfectly: the band moves and the
	// test moves with it. Sixteen blocks is a design decision — wide enough that the
	// rim is not a wall — so it is pinned the way this package pins a wire id.
	if !nearAt(s.Radius + 15) {
		t.Errorf("fifteen blocks past the rim the settlement already has no say; the blend band is narrower than sixteen")
	}
	if nearAt(s.Radius + 16) {
		t.Errorf("sixteen blocks past the rim the settlement still has a say; the blend band is wider than sixteen")
	}
	if settlementBlendBlocks != 16 {
		t.Errorf("settlementBlendBlocks is %d and the two assertions above are written for 16", settlementBlendBlocks)
	}
	end := s.Radius + settlementBlendBlocks
	if got := height(end); got != natural(end) {
		t.Errorf("%d blocks out — the end of the blend — the column is at %d and the land is at %d",
			end, got, natural(end))
	}

	// And it is an S rather than a ramp. **The tails are where the two differ and the
	// middle is where they agree** — smoothstep and the raw parameter cross at the
	// halfway point by construction — so the measurement is taken an eighth of the way
	// out, where a smoothstep has moved 4% of the drop and a straight line has moved
	// 12.5%. Over a fall of eight blocks or more that is the difference between "has
	// not moved at all" and "has already moved a block", which is what the rim of a
	// village looks like from a distance.
	rim := height(s.Radius)
	moved := max(height(s.Radius+settlementBlendBlocks/8)-rim, rim-height(s.Radius+settlementBlendBlocks/8))
	if moved > drop/16 {
		t.Errorf("an eighth of the way through a %d-block fall the blend has already moved %d blocks; a smoothstep moves about a twenty-fourth of it there and a ramp moves an eighth",
			drop, moved)
	}
}

// TestABlendBandMeetsTheLandItEndsOn is the continuity of the plateau's outer edge, and
// it is here because it was not there.
//
// **The band used to ease towards the *unlowered* land while the column one block past
// it was already lowered by a basin, or twenty-two blocks down in a river bed.** So a
// settlement that happened to sit beside water was ringed by a cliff at exactly
// `radius + settlementBlendBlocks`: measured on this seed before the fix, six of the
// twenty-three settlements within six cells of spawn had a step of four blocks or more
// there and the worst was twenty-two. Every test in this file passed throughout. The
// band's width was pinned, its shape was pinned, and what it *arrived at* was not.
//
// Two statements, and the second is the one that cannot be fooled by a quiet seed:
//
//   - By the last block of the band the surface is the land, within the one block
//     integer rounding costs. That is what "eases back into the land" has to mean.
//   - The step across the boundary is no bigger than the step the land itself takes
//     between those same two columns. **This is deliberately relative**, because
//     ordinary terrain in this world is not smooth — a river bank is a vertical drop of
//     twenty-odd blocks in open country, 0.47% of columns step four or more — so an
//     absolute smoothness bound would either fail on honest terrain or be too loose to
//     catch anything. What a settlement must not do is add roughness of its own.
func TestABlendBandMeetsTheLandItEndsOn(t *testing.T) {
	t.Parallel()

	land := func(x, z int64) int {
		h, _, _ := loweredHeightAt(settlementTestSeed, x, z, unloweredHeightAt(settlementTestSeed, x, z), ClimateAt(settlementTestSeed, x, z))
		return h
	}

	checked := 0
	for cz := int64(-6); cz <= 6; cz++ {
		for cx := int64(-6); cx <= 6; cx++ {
			s, ok := SettlementAt(settlementTestSeed, cx, cz)
			if !ok {
				continue
			}
			for bearing := range len(settlementBearings) {
				inner, outer := s.Radius+settlementBlendBlocks-1, s.Radius+settlementBlendBlocks
				dxi, dzi := ringOffset(inner, bearing)
				dxo, dzo := ringOffset(outer, bearing)
				xi, zi := s.CentreX+dxi, s.CentreZ+dzi
				xo, zo := s.CentreX+dxo, s.CentreZ+dzo

				checked++
				gotInner := HeightAt(settlementTestSeed, xi, zi)
				if d := max(gotInner-land(xi, zi), land(xi, zi)-gotInner); d > 1 {
					t.Fatalf("the %v in cell (%d, %d), bearing %d: the last block of its blend is at %d and the land there is at %d",
						s.Kind, cx, cz, bearing, gotInner, land(xi, zi))
				}

				step := HeightAt(settlementTestSeed, xo, zo) - gotInner
				own := land(xo, zo) - land(xi, zi)
				if max(step, -step) > max(own, -own)+1 {
					t.Fatalf("the %v in cell (%d, %d), bearing %d: the ground steps %d blocks across the end of its blend where the land itself steps %d",
						s.Kind, cx, cz, bearing, step, own)
				}
			}
		}
	}
	if checked == 0 {
		t.Fatal("no settlement within six cells of spawn; the assertion ran against nothing")
	}
}

// TestNothingGrowsCarvesOrFloodsInsideASettlement is the other three-quarters of what a
// plateau is for: flat ground is useless if a conifer is standing on it, a tunnel has
// opened under it or a river has cut through it.
func TestNothingGrowsCarvesOrFloodsInsideASettlement(t *testing.T) {
	t.Parallel()

	s := theCapital(t, settlementTestSeed)
	for dz := int64(-s.Radius - settlementBlendBlocks); dz <= int64(s.Radius+settlementBlendBlocks); dz++ {
		for dx := int64(-s.Radius - settlementBlendBlocks); dx <= int64(s.Radius+settlementBlendBlocks); dx++ {
			d2 := dx*dx + dz*dz
			reach := int64(s.Radius + settlementBlendBlocks)
			if d2 > reach*reach {
				continue
			}
			x, z := s.CentreX+dx, s.CentreZ+dz
			col := columnAt(settlementTestSeed, x, z)

			// Inside the blend band, whether or not it is inside the radius: no
			// channel is *carried*, because a river bed is a hole and this feature has
			// just finished flattening the ground.
			//
			// **What is not asserted here is that the band is never lowered, and that
			// is the correction PR #511's review earned.** The band eases towards the
			// land as it actually is — basin and channel included — so a settlement
			// beside a river has a rim that slopes down into it. Insisting the band
			// stay at or above the unlowered land is what produced the alternative: a
			// wall at exactly radius + blend, twenty-two blocks tall at its worst on
			// this seed. The flat ground is the radius; the band is a transition.
			if col.river {
				t.Fatalf("(%d, %d) is inside the capital's blend band and is a river bed", x, z)
			}

			if d2 > int64(s.Radius*s.Radius) {
				continue
			}
			if col.surface != s.Plateau {
				t.Fatalf("(%d, %d) is inside the radius and is not the plateau", x, z)
			}
			if !col.settlement {
				t.Fatalf("(%d, %d) is inside the radius and its column does not know it", x, z)
			}
			if _, rooted := treeAtColumn(settlementTestSeed, x, z, col); rooted {
				t.Fatalf("a conifer is rooted at (%d, %d), inside the capital", x, z)
			}
			for depth := range settlementCaveClearance {
				if col.carvedAt(settlementTestSeed, x, int64(col.surface-depth), z) {
					t.Fatalf("(%d, %d) is carved %d blocks under the capital's plateau", x, z, depth)
				}
			}
		}
	}
}

// TestSurfaceAtDrawsASettlementInsideItsRadiusAndNotOutside is the map's half of the
// feature, asserted against a settlement the sweep in surface_test.go cannot be relied
// on to find.
func TestSurfaceAtDrawsASettlementInsideItsRadiusAndNotOutside(t *testing.T) {
	t.Parallel()

	s := theCapital(t, settlementTestSeed)
	inside, outside := 0, 0
	for bearing := range len(settlementBearings) {
		for _, distance := range []int{0, s.Radius / 2, s.Radius, s.Radius + settlementBlendBlocks + 1} {
			dx, dz := ringOffset(distance, bearing)
			x, z := s.CentreX+dx, s.CentreZ+dz
			height, kind := SurfaceAt(settlementTestSeed, x, z)

			if distance <= s.Radius {
				if kind != SurfaceSettlement {
					t.Fatalf("(%d, %d) is %d blocks from the capital's centre and draws as %d", x, z, distance, kind)
				}
				if height != s.Plateau {
					t.Fatalf("(%d, %d) draws at height %d, want the plateau at %d", x, z, height, s.Plateau)
				}
				inside++
				continue
			}
			if kind == SurfaceSettlement {
				t.Fatalf("(%d, %d) is outside the capital and still draws as a settlement", x, z)
			}
			outside++
		}
	}
	if inside == 0 || outside == 0 {
		t.Fatalf("the sample checked %d columns inside and %d outside; both are needed", inside, outside)
	}
}

// TestEveryAnchorIsAirOverSolidGround is the promise this package makes to the stations
// and residents issues, checked against generated chunks rather than against the
// drawings.
//
// A slot is somewhere a forge or a person can be put: the voxel is empty and the voxel
// under it holds them up. Asserting it against [Generate] rather than against
// [Schematic] is the whole value — the drawing could be right and the placement could
// still land a hut's floor one block into the plateau.
func TestEveryAnchorIsAirOverSolidGround(t *testing.T) {
	t.Parallel()

	world := newGeneratedWorld(settlementTestSeed)
	kinds := map[AnchorKind]int{}

	for _, s := range SettlementsNear(settlementTestSeed, originColumnX, originColumnZ, 3) {
		anchors := s.Anchors()
		if len(anchors) == 0 {
			t.Fatalf("the %v at (%d, %d) offers no slot", s.Kind, s.CentreX, s.CentreZ)
		}
		// **Every building's slots, not some of them.** The only completeness check here
		// used to be the count of distinct kinds at the end, and a settlement's first
		// building alone already offers four — so [Settlement.Anchors] could return the
		// keep's three and drop the other eleven with nothing to show for it. This is
		// the call #456 and #458 read a settlement through: what it must be is a
		// concatenation, not a sample.
		total := 0
		for _, b := range s.Buildings {
			total += len(b.Anchors)
		}
		if len(anchors) != total {
			t.Fatalf("the %v at (%d, %d) has %d slots across its %d buildings and Anchors() returned %d",
				s.Kind, s.CentreX, s.CentreZ, total, len(s.Buildings), len(anchors))
		}
		for _, a := range anchors {
			kinds[a.Kind]++
			if got := world.at(a.X, a.Y, a.Z); got != Air {
				t.Errorf("the %v slot at (%d, %d, %d) holds block %d, not air", a.Kind, a.X, a.Y, a.Z, got)
			}
			if got := world.at(a.X, a.Y-1, a.Z); !Solid(got) {
				t.Errorf("the %v slot at (%d, %d, %d) stands on block %d, which holds nothing up", a.Kind, a.X, a.Y, a.Z, got)
			}
			if d := isqrt(squaredDistance(a.X, a.Z, s.CentreX, s.CentreZ)); d > int64(s.Radius) {
				t.Errorf("the %v slot at (%d, %d) is %d blocks out, past its settlement's radius of %d",
					a.Kind, a.X, a.Z, d, s.Radius)
			}
		}
	}
	if len(kinds) < 4 {
		t.Fatalf("the settlements around spawn offer only %d kinds of slot", len(kinds))
	}
}

// TestABuildingIsBuiltFromItsDrawingWhicheverChunkAsks is the border-continuity contract
// for settlements, and the counterpart of the tree test beside it.
//
// **A building is the first feature here bigger than a chunk**, so this is not the same
// statement the canopy makes: the capital's castle is twenty-one blocks across and
// twenty tall and can straddle eight chunks at once. Generating them in both orders is what says no chunk is
// reading, caching or mutating anything belonging to its neighbour — which, if it ever
// became untrue, would show up as a wall that exists only when a player happens to walk
// in from the east.
//
// **The y axis is a chunk border like the other two, and this test used to pretend it
// was not.** Both chunks of its pair were taken at `b.OriginY`, so every voxel above the
// first chunk boundary fell outside the map and was silently skipped by the `!held`
// return. What that hid: [placeSettlements] skips a chunk whose y range holds none of the
// building, and the top of that range is `plateau + tallestSchematic`. Narrowing it to
// `plateau + 1` — dropping every chunk above the floor — left this file entirely green
// while removing **3,112 non-air voxels from 22 buildings** within six cells of spawn:
// roofs and upper walls simply absent, one chunk up. Twenty-two of the hundred and
// twenty-two buildings near spawn straddle a y boundary, and the golden fixture is the
// *lower* chunk of a village, so it could not see it either.
//
// So the coordinate set is now every chunk the building's box touches, on all three
// axes, and both kinds of split are counted and required.
func TestABuildingIsBuiltFromItsDrawingWhicheverChunkAsks(t *testing.T) {
	t.Parallel()

	horizontal, vertical, checked := 0, 0, 0

	for _, s := range SettlementsNear(settlementTestSeed, originColumnX, originColumnZ, 3) {
		for _, b := range s.Buildings {
			schematic := SchematicFor(b.Kind)
			w, d := rotatedFootprint(schematic, b.Facing)
			hiX, hiY, hiZ := b.OriginX+int64(w)-1, b.OriginY+int64(schematic.H)-1, b.OriginZ+int64(d)-1

			low := ChunkOf(b.OriginX, b.OriginY, b.OriginZ)
			high := ChunkOf(hiX, hiY, hiZ)
			splitAcross := low.X != high.X || low.Z != high.Z
			splitUp := low.Y != high.Y
			if !splitAcross && !splitUp {
				continue // wholly inside one chunk; the split ones are the point
			}
			if splitAcross {
				horizontal++
			}
			if splitUp {
				vertical++
			}

			// Every chunk the box touches, which for a keep on a corner is eight.
			var coords []Coord
			for cy := low.Y; cy <= high.Y; cy++ {
				for cz := low.Z; cz <= high.Z; cz++ {
					for cx := low.X; cx <= high.X; cx++ {
						coords = append(coords, Coord{X: cx, Y: cy, Z: cz})
					}
				}
			}

			forwards := map[Coord]*Chunk{}
			for _, coord := range coords {
				forwards[coord] = Generate(settlementTestSeed, coord)
			}
			backwards := map[Coord]*Chunk{}
			for i := len(coords) - 1; i >= 0; i-- {
				backwards[coords[i]] = Generate(settlementTestSeed, coords[i])
			}

			visitSchematic(b, func(x, y, z int64, block Block) {
				if block == Air {
					return // the clip writes nothing for a room's air; there is nothing to compare
				}
				coord := ChunkOf(x, y, z)
				chunk, held := forwards[coord]
				if !held {
					t.Fatalf("the %v yields (%d, %d, %d), in chunk %+v, which is outside the box its own footprint claims",
						b.Kind, x, y, z, coord)
				}
				checked++
				local := [3]int{Local(x), Local(y), Local(z)}
				got := chunk.At(local[0], local[1], local[2])
				if got != block {
					t.Fatalf("the %v's voxel at (%d, %d, %d) is block %d in chunk %+v, and its drawing says %d",
						b.Kind, x, y, z, got, coord, block)
				}
				if other := backwards[coord].At(local[0], local[1], local[2]); other != got {
					t.Fatalf("the %v's voxel at (%d, %d, %d) is %d in one generation order and %d in the other",
						b.Kind, x, y, z, got, other)
				}
			})
		}
	}

	// **Both kinds of split have to be present or the assertion above is partial**, and
	// the vertical one is the half that was missing: buildings are up to fourteen
	// blocks tall on thirty-two-block chunks, so a y straddle is a minority case that a
	// narrower sweep can miss entirely.
	if horizontal == 0 || vertical == 0 || checked == 0 {
		t.Fatalf("%d buildings cross a chunk column border, %d cross a chunk height border and %d voxels were compared; each needs one",
			horizontal, vertical, checked)
	}
}

// TestSettlementsNearIsOrderedAndNearestAgreesWithIt pins the two surfaces the stations,
// residents and respawn issues read.
func TestSettlementsNearIsOrderedAndNearestAgreesWithIt(t *testing.T) {
	t.Parallel()

	const x, z = 3000, -1500
	near := SettlementsNear(settlementTestSeed, x, z, 2)
	if len(near) < 2 {
		t.Fatalf("two cells out from (%d, %d) holds %d settlements; the ordering is not exercised", x, z, len(near))
	}

	for i := 1; i < len(near); i++ {
		previous := squaredDistance(x, z, near[i-1].CentreX, near[i-1].CentreZ)
		current := squaredDistance(x, z, near[i].CentreX, near[i].CentreZ)
		if current < previous {
			t.Fatalf("settlement %d is nearer than settlement %d", i, i-1)
		}
	}

	// **The referee is a wide search, not this two-cell one.** [NearestSettlement] looks
	// six kilometres from the column itself, which is a different region from "two cells
	// around the cell this column is in" — the head of a narrow cell-centred search is
	// not the nearest settlement, and asserting they agree is what let the defect in
	// [TestNearestSettlementIsActuallyTheNearest] survive.
	nearest, ok := NearestSettlement(settlementTestSeed, x, z)
	if !ok {
		t.Fatal("no settlement within reach of a column that has two within two cells")
	}
	wide := SettlementsNear(settlementTestSeed, x, z, 6)
	if nearest.CentreX != wide[0].CentreX || nearest.CentreZ != wide[0].CentreZ {
		t.Fatalf("NearestSettlement is at (%d, %d) and the nearest of a six-cell search is at (%d, %d)",
			nearest.CentreX, nearest.CentreZ, wide[0].CentreX, wide[0].CentreZ)
	}
	if squaredDistance(x, z, nearest.CentreX, nearest.CentreZ) > squaredDistance(x, z, near[0].CentreX, near[0].CentreZ) {
		t.Fatalf("NearestSettlement is further away than the head of a two-cell search")
	}

	// A count of zero is one cell — the one the column is in — rather than nothing.
	if own := SettlementsNear(settlementTestSeed, x, z, 0); len(own) > 1 {
		t.Fatalf("a zero-cell search returned %d settlements", len(own))
	}
	// And a negative one is the same cell, which is what the doc comment promises and
	// what the clamp is for. Without it the loop bounds cross and the answer is nothing
	// at all — a caller asking for "here" would be told the world is empty.
	//
	// **Asked at spawn rather than at the column above, because the assertion is
	// vacuous anywhere the own cell is empty.** Both sides are nil there, and nil
	// equals nil however the bounds behave; the spawn cell is the one cell in the
	// world guaranteed to hold something.
	if got, want := SettlementsNear(settlementTestSeed, originColumnX, originColumnZ, -1),
		SettlementsNear(settlementTestSeed, originColumnX, originColumnZ, 0); len(want) == 0 {
		t.Fatal("the spawn cell holds no settlement, so the negative-count clamp is not being checked")
	} else if len(got) != len(want) {
		t.Fatalf("a search of -1 cells at spawn returned %d settlements and a search of 0 returned %d", len(got), len(want))
	}

	// **The square is (2n+1)² cells, enumerated here rather than trusted.** The loop
	// bounds in [SettlementsNear] are the one place an off-by-one silently shrinks the
	// answer: dropping the last row leaves every property above intact — still sorted,
	// still nearest-first, still agreeing with [NearestSettlement] — and simply omits
	// settlements the caller asked for.
	for _, cells := range []int{0, 1, 2, 3} {
		want := map[[2]int64]bool{}
		centreCellX, centreCellZ := settlementCellOf(x), settlementCellOf(z)
		for cz := centreCellZ - int64(cells); cz <= centreCellZ+int64(cells); cz++ {
			for cx := centreCellX - int64(cells); cx <= centreCellX+int64(cells); cx++ {
				if s, ok := SettlementAt(settlementTestSeed, cx, cz); ok {
					want[[2]int64{s.CentreX, s.CentreZ}] = true
				}
			}
		}
		got := SettlementsNear(settlementTestSeed, x, z, cells)
		if len(got) != len(want) {
			t.Fatalf("a %d-cell search returned %d settlements; the %d×%d block of cells holds %d",
				cells, len(got), 2*cells+1, 2*cells+1, len(want))
		}
		for _, s := range got {
			if !want[[2]int64{s.CentreX, s.CentreZ}] {
				t.Fatalf("a %d-cell search returned a settlement at (%d, %d), which is outside the block of cells it names",
					cells, s.CentreX, s.CentreZ)
			}
		}
	}
}

// TestTwoSettlementsExactlyAsFarAwayComeBackInAFixedOrder is the tiebreak, which is the
// half of [SettlementsNear]'s "total order" promise that ordinary sampling never reaches.
//
// **`slices.SortFunc` is not stable, so a comparator that returns zero for a tie leaves
// the order to the sort's internals.** Two settlements the same distance from a column
// would then come back in an order that depends on how many other settlements were in
// the slice — and a caller that picks "the nearest" would get a different answer either
// side of an unrelated change. Ties are rare enough that no sweep in this file contains
// one, so the column below was searched for: it is exactly equidistant from the villages
// at (1914, -6156) and (6424, -4254), and both of them are at the head of its list.
func TestTwoSettlementsExactlyAsFarAwayComeBackInAFixedOrder(t *testing.T) {
	t.Parallel()

	// **The pair has to be one where the two candidate orders disagree**, and the first
	// version of this test used one where they did not: (1914, -6156) and (6424, -4254)
	// sort the same way whether the comparator reads z before x or x before z, so
	// swapping those two lines in [SettlementsNear] was invisible. This column is
	// equidistant from (6424, -4254) and (2118, 236), whose z order and x order are
	// opposite — z puts the first ahead, x puts the second.
	const x, z = 4271, -2009

	near := SettlementsNear(settlementTestSeed, x, z, 3)
	if len(near) < 2 {
		t.Fatalf("(%d, %d) has %d settlements within three cells; the tie is not there to check", x, z, len(near))
	}
	first := squaredDistance(x, z, near[0].CentreX, near[0].CentreZ)
	second := squaredDistance(x, z, near[1].CentreX, near[1].CentreZ)
	if first != second {
		t.Fatalf("(%d, %d) was chosen because two settlements are exactly as far from it; they are now %d and %d away squared",
			x, z, first, second)
	}

	// The tie is broken by cell coordinate, z before x, so the answer is the same on
	// every machine and after every unrelated settlement is added to the slice.
	if near[0].CentreZ > near[1].CentreZ ||
		(near[0].CentreZ == near[1].CentreZ && near[0].CentreX >= near[1].CentreX) {
		t.Fatalf("two settlements exactly as far from (%d, %d) came back as (%d, %d) then (%d, %d), which is not the documented order",
			x, z, near[0].CentreX, near[0].CentreZ, near[1].CentreX, near[1].CentreZ)
	}

	// The two orders genuinely disagree here, which is what makes the assertion above
	// mean something. If a later change to the lattice makes them agree, this test goes
	// quiet without failing — so it says so.
	if (near[0].CentreZ < near[1].CentreZ) == (near[0].CentreX < near[1].CentreX) {
		t.Errorf("the tied pair (%d, %d) and (%d, %d) sorts the same way on z as on x; the tiebreak's order is not being tested",
			near[0].CentreX, near[0].CentreZ, near[1].CentreX, near[1].CentreZ)
	}

	// Repeating the call must give the same head, through a different number of cells.
	for _, cells := range []int{2, 3} {
		again := SettlementsNear(settlementTestSeed, x, z, cells)
		if len(again) < 2 {
			continue
		}
		if again[0].CentreX != near[0].CentreX || again[0].CentreZ != near[0].CentreZ {
			t.Fatalf("the head of a %d-cell search is (%d, %d) and of a three-cell search is (%d, %d)",
				cells, again[0].CentreX, again[0].CentreZ, near[0].CentreX, near[0].CentreZ)
		}
	}

	// **[NearestSettlement] breaks the same tie the same way**, which is the half of
	// "the order is total" that spans the two exported surfaces. It runs its own
	// comparison rather than sorting, so nothing but this says the two agree: swapping
	// z and x in its tiebreak alone leaves every other assertion in this file standing.
	nearest, ok := NearestSettlement(settlementTestSeed, x, z)
	if !ok {
		t.Fatal("no settlement within reach of a column with two exactly as far away")
	}
	if nearest.CentreX != near[0].CentreX || nearest.CentreZ != near[0].CentreZ {
		t.Errorf("two settlements are exactly as far from (%d, %d); SettlementsNear puts (%d, %d) first and NearestSettlement answers (%d, %d)",
			x, z, near[0].CentreX, near[0].CentreZ, nearest.CentreX, nearest.CentreZ)
	}

	// **What this still cannot demonstrate is that the tiebreak is reached at all**, and
	// that is worth stating rather than leaving as a silence. Deleting it — `return 0`
	// for a tie — passes everything above, because [villageCellInset] keeps every centre
	// strictly inside its own cell, so the cell scan that builds the slice already
	// emits z-then-x order and `slices.SortFunc` has nothing to disturb. The tiebreak is
	// insurance against a sort that reorders equal elements, which `slices.SortFunc` is
	// explicitly permitted to do. What is asserted above is the documented order, not
	// proof that the comparator's third and fourth lines ran.
}

// TestNearestSettlementReachesFurtherThanOneCell is what [nearestSettlementBlocks] is for.
//
// **Six kilometres is a promise about emptiness, and two kilometres would keep every
// other test in this file green.** The ordering test above stands on a column with
// settlements in its own cell, so the reach is invisible there. This column has none
// within one cell and several within two, which is exactly the case the constant exists
// to answer: a respawn or a station lookup on a quiet stretch of world must still be told
// where the nearest settlement is rather than that there is none.
func TestNearestSettlementReachesFurtherThanOneCell(t *testing.T) {
	t.Parallel()

	const x, z = 5000, 5000

	if own := SettlementsNear(settlementTestSeed, x, z, 1); len(own) != 0 {
		t.Fatalf("(%d, %d) was chosen because one cell around it holds nothing; it now holds %d", x, z, len(own))
	}
	s, ok := NearestSettlement(settlementTestSeed, x, z)
	if !ok {
		t.Fatalf("(%d, %d) has no settlement within %d blocks, and the search should reach that far", x, z, nearestSettlementBlocks)
	}
	if d := isqrt(squaredDistance(x, z, s.CentreX, s.CentreZ)); d > nearestSettlementBlocks {
		t.Fatalf("the nearest settlement to (%d, %d) is %d blocks away, further than the search reaches", x, z, d)
	}
}

// TestNearestSettlementIsActuallyTheNearest is the regression for a defect in the one
// call #460 is written against.
//
// **The search square used to be centred on the *cell* holding the column rather than on
// the column**, so from a column near one edge of its cell it reached 6144 blocks one way
// and 8191 the other. A settlement 6200 blocks to the right could fall outside it while
// one 8000 to the left was inside, and the sort then honestly reported the wrong winner.
// Measured over 40 seeds and 268,960 columns before the fix: **109 answers were not the
// nearest, and 105 said there was none where a six-cell search finds one.**
//
// Three shapes are pinned. The first two are recorded instances, because a sweep that
// stops finding them tells you nothing about why; the third is the sweep, because two
// instances are not a contract.
func TestNearestSettlementIsActuallyTheNearest(t *testing.T) {
	t.Parallel()

	distance := func(x, z int64, s Settlement) int64 {
		return isqrt(squaredDistance(x, z, s.CentreX, s.CentreZ))
	}

	// A wrong winner: the true nearest sits in the cell column the old square never
	// reached, 43 blocks the wrong side of a cell edge.
	{
		const seed, x, z = 38, -7212, 7957
		got, ok := NearestSettlement(seed, x, z)
		if !ok {
			t.Fatal("seed 38 has no settlement near (-7212, 7957), and it has several")
		}
		if d := distance(x, z, got); d > 7201 {
			t.Errorf("the nearest settlement to (%d, %d) on seed %d is %d blocks away; one at 7201 was being missed",
				x, z, seed, d)
		}
	}

	// **A "none", and it is the contract rather than the defect.** A six-cell search
	// finds twenty-one settlements around this column, the nearest 8576 blocks away —
	// beyond the reach, and in a cell the six-kilometre square does not overlap. What
	// the old behaviour got wrong was answering none while one stood 7201 blocks away
	// *inside* the reach; what is asserted here is the boundary itself, so that moving
	// it is a deliberate act.
	{
		const seed, x, z = 36, 3032, 7957
		if _, ok := NearestSettlement(seed, x, z); ok {
			t.Errorf("seed %d now finds a settlement near (%d, %d); this column was chosen because the reach ends before the nearest one",
				seed, x, z)
		}
		wide := SettlementsNear(seed, x, z, 6)
		if len(wide) == 0 {
			t.Fatalf("seed %d has no settlement at all near (%d, %d); the column was chosen for the opposite reason", seed, x, z)
		}
		if d := distance(x, z, wide[0]); d <= nearestSettlementBlocks {
			t.Errorf("seed %d has a settlement %d blocks from (%d, %d), inside the %d-block reach, and NearestSettlement missed it",
				seed, d, x, z, nearestSettlementBlocks)
		}
	}

	// And it is continuous across a cell edge, which the cell-centred square could not
	// be: on seed 1 at this row, x = -5121 answered 6903 blocks away and x = -5120
	// answered 6584 — a 319-block jump between adjacent columns.
	{
		const seed, z = 1, -4681
		left, okL := NearestSettlement(seed, -5121, z)
		right, okR := NearestSettlement(seed, -5120, z)
		if !okL || !okR {
			t.Fatal("seed 1 has no settlement near the cell edge at x = -5120")
		}
		dl, dr := distance(-5121, z, left), distance(-5120, z, right)
		if left.CentreX != right.CentreX || left.CentreZ != right.CentreZ {
			t.Errorf("adjacent columns either side of a cell edge answer different settlements, at (%d, %d) and (%d, %d)",
				left.CentreX, left.CentreZ, right.CentreX, right.CentreZ)
		}
		if d := max(dl-dr, dr-dl); d > 2 {
			t.Errorf("adjacent columns either side of a cell edge answer %d and %d blocks; the search is centred on the cell rather than the column",
				dl, dr)
		}
	}

	// **The sweep, against a search wide enough to be the referee, and with no
	// distance qualifier on the assertion.** Six cells around the column's own cell
	// always contains the six-kilometre disc about the column, so its head is at least
	// as near as anything [NearestSettlement]'s first pass can see; and because that
	// function widens once when its best lies beyond its first reach, "nearest" here is
	// the whole world's nearest rather than the nearest inside a radius. Over 268,960
	// columns of 40 seeds after the fix: zero wrong, zero missed, and zero suboptimal
	// even outside the first reach.
	wrong, missed, checked := 0, 0, 0
	for seed := int64(1); seed <= 12; seed++ {
		for z := int64(-8000); z <= 8000; z += 907 {
			for x := int64(-8000); x <= 8000; x += 907 {
				reference := SettlementsNear(seed, x, z, 6)
				if len(reference) == 0 {
					continue
				}
				checked++
				got, ok := NearestSettlement(seed, x, z)
				if !ok {
					// A refusal is only honest if nothing stood inside the reach.
					if distance(x, z, reference[0]) <= nearestSettlementBlocks {
						missed++
					}
					continue
				}
				if distance(x, z, got) > distance(x, z, reference[0]) {
					wrong++
				}
			}
		}
	}
	if checked == 0 {
		t.Fatal("the sweep found no column with a settlement in reach; it asserted nothing")
	}
	if wrong != 0 || missed != 0 {
		t.Errorf("over %d columns: %d answers were not the nearest and %d reported none while a six-cell search finds one",
			checked, wrong, missed)
	}
}

// TestSettlementLookupIsPureInSeedAndCell is the determinism contract for the lattice,
// stated the way the rest of this package states it.
func TestSettlementLookupIsPureInSeedAndCell(t *testing.T) {
	t.Parallel()

	for cz := int64(-3); cz <= 3; cz++ {
		for cx := int64(-3); cx <= 3; cx++ {
			first, ok := SettlementAt(settlementTestSeed, cx, cz)
			for again := range 3 {
				got, gotOK := SettlementAt(settlementTestSeed, cx, cz)
				if gotOK != ok {
					t.Fatalf("cell (%d, %d) reading %d: held = %v, first reading was %v", cx, cz, again+2, gotOK, ok)
				}
				if !ok {
					continue
				}
				if got.CentreX != first.CentreX || got.CentreZ != first.CentreZ ||
					got.Plateau != first.Plateau || len(got.Buildings) != len(first.Buildings) {
					t.Fatalf("cell (%d, %d) reading %d: %+v, first reading was %+v", cx, cz, again+2, got, first)
				}
			}
		}
	}
}

// A ward is the plateau disc against a chunk's whole block square, not against
// its origin or centre. The exhaustive referee below is intentionally simpler
// than WardsColumn: it asks every block coordinate in each nearby column.
func TestSettlementWardsExactlyTheColumnsItsPlateauDiscTouches(t *testing.T) {
	t.Parallel()

	settlement := Settlement{CentreX: 7, CentreZ: -11, Radius: capitalRadius}
	for cz := int32(-4); cz <= 3; cz++ {
		for cx := int32(-3); cx <= 4; cx++ {
			col := Column{CX: cx, CZ: cz}
			want := false
			for z := int64(cz) * ChunkSize; z < int64(cz+1)*ChunkSize && !want; z++ {
				for x := int64(cx) * ChunkSize; x < int64(cx+1)*ChunkSize; x++ {
					if squaredDistance(x, z, settlement.CentreX, settlement.CentreZ) <= int64(settlement.Radius*settlement.Radius) {
						want = true
						break
					}
				}
			}
			if got := settlement.WardsColumn(col); got != want {
				t.Errorf("WardsColumn(%+v) = %v, exhaustive disc intersection = %v", col, got, want)
			}
		}
	}

	// Exactly tangent belongs to the plateau; one block into the blend does not.
	//
	// **The centre is derived from the radius rather than written beside it.** `8` was
	// exactly tangent to column 2 while [capitalRadius] was 56 and meant nothing once
	// #682 moved it to 68, which is a test that fails for a reason unrelated to what it
	// checks. Column 2's nearest block is x=64, so the tangent centre is 64 − r whatever
	// r becomes.
	tangent := Settlement{CentreX: 2*ChunkSize - capitalRadius, CentreZ: 0, Radius: capitalRadius}
	if !tangent.WardsColumn(Column{CX: 2, CZ: 0}) {
		t.Error("a column whose nearest block is exactly Radius away is not warded")
	}
	tangent.CentreX--
	if tangent.WardsColumn(Column{CX: 2, CZ: 0}) {
		t.Error("the first column beyond Radius is warded as though the blend band counted")
	}
}

func TestSettlementWardingFindsCapitalAndVillageDiscsWithoutChangingTheLayout(t *testing.T) {
	t.Parallel()

	capital := theCapital(t, settlementTestSeed)
	assertWardingMatchesSettlement(t, settlementTestSeed, capital)

	var village Settlement
	for cz := int64(-3); cz <= 3 && village.Radius == 0; cz++ {
		for cx := int64(-3); cx <= 3; cx++ {
			candidate, ok := SettlementAt(settlementTestSeed, cx, cz)
			if ok && candidate.Kind == SettlementVillage {
				village = candidate
				break
			}
		}
	}
	if village.Radius == 0 {
		t.Fatal("the fixture has no village near spawn")
	}
	assertWardingMatchesSettlement(t, settlementTestSeed, village)
}

func assertWardingMatchesSettlement(t *testing.T, seed int64, settlement Settlement) {
	t.Helper()

	centre := ChunkOf(settlement.CentreX, 0, settlement.CentreZ).Column()
	for dz := int32(-3); dz <= 3; dz++ {
		for dx := int32(-3); dx <= 3; dx++ {
			col := Column{CX: centre.CX + dx, CZ: centre.CZ + dz}
			want := settlement.WardsColumn(col)
			first, found := SettlementWarding(seed, col)
			if found != want {
				t.Errorf("%v column %+v: SettlementWarding found = %v, WardsColumn = %v", settlement.Kind, col, found, want)
				continue
			}
			if !found {
				continue
			}
			if first.Kind != settlement.Kind || first.CentreX != settlement.CentreX || first.CentreZ != settlement.CentreZ ||
				first.Radius != settlement.Radius || first.Plateau != settlement.Plateau || !reflect.DeepEqual(first.Buildings, settlement.Buildings) {
				t.Errorf("%v column %+v returned a different settlement layout", settlement.Kind, col)
			}
			second, again := SettlementWarding(seed, col)
			if !again || second.CentreX != first.CentreX || second.CentreZ != first.CentreZ || !reflect.DeepEqual(second.Buildings, first.Buildings) {
				t.Errorf("%v column %+v did not return the same answer twice", settlement.Kind, col)
			}
		}
	}
}

// TestTheCapitalCastleIsByteIdenticalForTheSameSeed states #555's determinism
// criterion against generated chunks, not merely against the literal. Every chunk the
// footprint touches is generated twice from nothing; every voxel has to return in the
// same order and with the same value.
func TestTheCapitalCastleIsByteIdenticalForTheSameSeed(t *testing.T) {
	t.Parallel()

	capital := theCapital(t, settlementTestSeed)
	var castle Building
	found := false
	for _, b := range capital.Buildings {
		if b.Kind == BuildingKeep {
			castle, found = b, true
			break
		}
	}
	if !found {
		t.Fatal("the capital has no castle to check")
	}

	s := SchematicFor(BuildingKeep)
	w, d := rotatedFootprint(s, castle.Facing)
	low := ChunkOf(castle.OriginX, castle.OriginY, castle.OriginZ)
	high := ChunkOf(castle.OriginX+int64(w)-1, castle.OriginY+int64(s.H)-1, castle.OriginZ+int64(d)-1)
	compared := 0
	for cy := low.Y; cy <= high.Y; cy++ {
		for cz := low.Z; cz <= high.Z; cz++ {
			for cx := low.X; cx <= high.X; cx++ {
				coord := Coord{X: cx, Y: cy, Z: cz}
				first := Generate(settlementTestSeed, coord)
				second := Generate(settlementTestSeed, coord)
				if !slices.Equal(first.Blocks, second.Blocks) {
					t.Fatalf("capital chunk %+v differs across two generations of seed %#x", coord, settlementTestSeed)
				}
				compared++
			}
		}
	}
	if compared == 0 {
		t.Fatal("the castle footprint touched no chunk; the determinism check asserted nothing")
	}
}

// TestBuildingsStandClearOfEachOtherAndInsideThePlateau is the layout's own invariant.
//
// Two buildings sharing a voxel would be silently absorbed by the clip — whichever was
// written first would win and the other would lose a wall — so nothing about the
// generated chunk would look wrong enough to notice. The footprints are checked as
// boxes, which is stricter than the voxels and therefore the right thing to assert.
//
// **It sweeps seeds as well as cells, and the hall is why.** The capital's hall and
// smithy share a ring, so their bearings must differ by at least a quarter of the circle
// or their thirteen-block footprints touch — and that separation is drawn from a hash, so
// one seed's capital says nothing about whether the rule is there. Removing the `+ 3`
// from [settlementFrom] left this test green while it looked at one world; forty
// capitals is enough that the collision it allows actually happens.
func TestBuildingsStandClearOfEachOtherAndInsideThePlateau(t *testing.T) {
	t.Parallel()

	type plan struct {
		seed         int64
		cellX, cellZ int64
	}
	var plans []plan
	for cz := int64(-3); cz <= 3; cz++ {
		for cx := int64(-3); cx <= 3; cx++ {
			plans = append(plans, plan{settlementTestSeed, cx, cz})
		}
	}
	for seed := int64(1); seed <= 40; seed++ {
		plans = append(plans, plan{seed, settlementCellOf(originColumnX), settlementCellOf(originColumnZ)})
	}

	for _, p := range plans {
		func() {
			cx, cz := p.cellX, p.cellZ
			s, ok := SettlementAt(p.seed, cx, cz)
			if !ok {
				return
			}
			type box struct{ x0, z0, x1, z1 int64 }
			boxes := make([]box, 0, len(s.Buildings))
			for _, b := range s.Buildings {
				w, d := rotatedFootprint(SchematicFor(b.Kind), b.Facing)
				boxes = append(boxes, box{b.OriginX, b.OriginZ, b.OriginX + int64(w) - 1, b.OriginZ + int64(d) - 1})

				if b.OriginY != int64(s.Plateau)+1 {
					t.Fatalf("a %v in cell (%d, %d) has its floor at %d, and the plateau is %d",
						b.Kind, cx, cz, b.OriginY, s.Plateau)
				}
				for _, corner := range [][2]int64{
					{b.OriginX, b.OriginZ},
					{b.OriginX + int64(w) - 1, b.OriginZ},
					{b.OriginX, b.OriginZ + int64(d) - 1},
					{b.OriginX + int64(w) - 1, b.OriginZ + int64(d) - 1},
				} {
					if d := isqrt(squaredDistance(corner[0], corner[1], s.CentreX, s.CentreZ)); d > int64(s.Radius) {
						t.Fatalf("a %v in cell (%d, %d) has a corner %d blocks out, past the radius of %d",
							b.Kind, cx, cz, d, s.Radius)
					}
				}
			}

			for i, a := range boxes {
				for _, b := range boxes[i+1:] {
					if a.x0 <= b.x1 && b.x0 <= a.x1 && a.z0 <= b.z1 && b.z0 <= a.z1 {
						t.Fatalf("seed %#x: two buildings in cell (%d, %d) overlap: %+v and %+v", p.seed, cx, cz, a, b)
					}
				}
			}
		}()
	}
}

// TestAVillagePlanVariesWithinItsSeed is the layout's other half: not that a village is
// well formed, but that the hash actually reaches every plan it is written to reach.
//
// **A constant is not a distribution.** Replacing the public building's coin flip with
// "always a smithy", or the hut count with a fixed three, breaks nothing this file
// asserts — every village would still stand clear, still be inside its plateau, still be
// laid out on the ring — and every village in the world would be the same village, which
// is the one thing the layout hash exists to prevent.
func TestAVillagePlanVariesWithinItsSeed(t *testing.T) {
	t.Parallel()

	const cells = 20

	publicKinds := map[BuildingKind]int{}
	hutCounts := map[int]int{}
	for cz := int64(-cells); cz <= cells; cz++ {
		for cx := int64(-cells); cx <= cells; cx++ {
			s, ok := SettlementAt(settlementTestSeed, cx, cz)
			if !ok || s.Kind != SettlementVillage {
				continue
			}
			publicKinds[s.Buildings[0].Kind]++
			hutCounts[len(s.Buildings)-1]++
		}
	}

	// A village's middle is a hall or a smithy, and a world needs both.
	for _, kind := range []BuildingKind{BuildingHall, BuildingSmithy} {
		if publicKinds[kind] == 0 {
			t.Errorf("no village in %d cells has a %v in the middle of it", (2*cells+1)*(2*cells+1), kind)
		}
	}
	// And every hut count the constants allow is reached.
	for huts := villageMinHuts; huts < villageMinHuts+villageHutVariants; huts++ {
		if hutCounts[huts] == 0 {
			t.Errorf("no village in the sweep has %d huts; the plan only ever rolls %v", huts, hutCounts)
		}
	}
}

// TestASettlementIsBuiltOutOfTheThreeBlocksItsIssueAppended is the one assertion here
// about materials, and it is what says a settlement is visible at all.
func TestASettlementIsBuiltOutOfTheThreeBlocksItsIssueAppended(t *testing.T) {
	t.Parallel()

	s := theCapital(t, settlementTestSeed)
	world := newGeneratedWorld(settlementTestSeed)

	counts := map[Block]int{}
	for _, b := range s.Buildings {
		visitSchematic(b, func(x, y, z int64, block Block) {
			if block == Air {
				return
			}
			counts[world.at(x, y, z)]++
		})
	}
	for _, block := range []Block{Planks, Cobblestone, Thatch} {
		if counts[block] == 0 {
			t.Errorf("the capital holds no block %d; it is not built out of what it is drawn with", block)
		}
		if !Placeable(block) {
			t.Errorf("block %d is what a settlement is made of and a player may not put one back", block)
		}
	}
}

// BenchmarkGenerateInACapital and BenchmarkGenerateInOpenCountry are the acceptance
// criterion this feature carries: a chunk inside the capital under four times the
// open-country number.
//
// **The two coordinates are chosen to hold the same amount of rock, and getting that
// wrong is how this measurement lies.** The first attempt compared the capital against
// [BenchmarkGenerate]'s sweep, which spends most of its chunks above the surface where
// almost every voxel is air and [caveAt] returns on a depth comparison — 1.4 ms against
// the capital's 5.8, a reported 4.1× of which the feature accounted for none. That is
// climate_test.go's own recorded trap, one axis over: a comparison of two different
// volumes of rock wearing the label of a comparison of two generators.
//
// Measured against a chunk holding open ground's surface instead: 5.49 ms open, 5.57 ms
// in the capital — **1.02×**. The settlement rules cost about a microsecond of the four
// a column already spends, because the four-cell lookup is hashes until something is
// actually near, and because a plateau *removes* work: no tree roots inside a radius and
// nothing is carved in the top six blocks of it.
//
// **#682 grew the keep from 12,348 cells to 269,892 — twenty-two times — and this
// benchmark did not move.** Median of five runs at 200 iterations: 6.18 ms before and
// 5.95 ms after in the capital, against 6.17 ms and 6.08 ms in open country, whose code
// path this change does not touch. The capital chunk got *faster* by less than the
// control drifted, which is the only honest way to read it: the growth is under the
// noise. It is under the noise for a reason worth keeping — 70% of that drawing is
// `keepTerrain`, and [visitSchematic] skips one with a `continue` on a uint16 compare,
// so the extra quarter-million cells are the cheapest loop in the generator against a
// chunk that already spends six milliseconds on noise, caves and water.
func BenchmarkGenerateInACapital(b *testing.B) {
	s, ok := SettlementAt(settlementTestSeed, settlementCellOf(originColumnX), settlementCellOf(originColumnZ))
	if !ok {
		b.Fatal("no capital")
	}
	coord := ChunkOf(s.CentreX, int64(s.Plateau), s.CentreZ)
	for b.Loop() {
		Generate(settlementTestSeed, coord)
	}
}

// benchmarkOpenColumn is a column a long way from spawn with no settlement anywhere near
// it, whose surface chunk is the fair comparison for the benchmark above.
const (
	benchmarkOpenX = 20096
	benchmarkOpenZ = -13984
)

func BenchmarkGenerateInOpenCountry(b *testing.B) {
	if columnAt(settlementTestSeed, benchmarkOpenX, benchmarkOpenZ).settlement {
		b.Fatal("the open-country benchmark column is inside a settlement")
	}
	coord := ChunkOf(benchmarkOpenX, int64(HeightAt(settlementTestSeed, benchmarkOpenX, benchmarkOpenZ)), benchmarkOpenZ)
	for b.Loop() {
		Generate(settlementTestSeed, coord)
	}
}
