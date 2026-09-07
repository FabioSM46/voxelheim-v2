package world

import (
	"bytes"
	"math"
	"reflect"
	"testing"
)

// ruinTestSeeds is the seed table the placement contract is asserted over. Twenty is
// the acceptance criterion's floor; the extra four are here because a rule that holds
// for exactly the seeds it was written against has not been tested.
var ruinTestSeeds = []int64{goldenSeed, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23}

func TestRuinCellArithmeticAndBounds(t *testing.T) {
	for _, tt := range []struct{ block, cell int64 }{{-8193, -2}, {-8192, -1}, {-1, -1}, {0, 0}, {8191, 0}, {8192, 1}, {BlockLimit, 2048}, {-BlockLimit, -2048}} {
		if got := RuinCellOf(tt.block); got != tt.cell {
			t.Errorf("cell(%d)=%d want %d", tt.block, got, tt.cell)
		}
	}
	for _, cell := range []int64{math.MinInt64, -2049, 2049, math.MaxInt64} {
		if _, ok := RuinAt(1, cell, 0); ok {
			t.Errorf("out-of-world cell %d accepted", cell)
		}
	}
	for _, tt := range []struct {
		x, z int64
		r    int
	}{{math.MaxInt64, 0, 1}, {0, math.MinInt64, 1}, {0, 0, -1}, {0, 0, 8193}} {
		if _, ok := RuinNear(1, tt.x, tt.z, tt.r); ok {
			t.Errorf("invalid query %+v accepted", tt)
		}
	}
	// The contract this pins is no longer "how dense are ruins" — there is one — but
	// the shape of the one search: the cell it runs in, the box it may place inside,
	// the two relief tolerances the ladder is allowed, and how near "near" is.
	if RuinCellBlocks != 8192 || RuinSettlementDistance != 1024 || ruinCellInset != 1024 || ruinBlendBlocks != 8 {
		t.Fatal("placement constants changed without updating the world contract")
	}
	if ruinMaxGroundDelta != 2 || ruinFallbackGroundDelta != 3 || ruinPreferredDistance != 3000 {
		t.Fatal("acceptance ladder changed without updating the world contract")
	}
	if ruinSearchStep != 48 || ruinSearchCells != 128 || ruinSearchSpan != ruinSearchCells*ruinSearchStep {
		t.Fatal("candidate lattice no longer tiles the cell inset")
	}
	if cx, cz := ruinCell(); cx != 0 || cz != 0 || !isRuinCell(cx, cz) || isRuinCell(cx+1, cz) || isRuinCell(cx, cz-1) {
		t.Fatalf("the one ruin cell moved: (%d,%d)", cx, cz)
	}
	for v := uint8(0); v < RuinVariantCount; v++ {
		for f := Facing(0); f < 4; f++ {
			w, d := rotatedFootprint(RuinSchematicFor(v), f)
			if max(w/2, d/2) != ruinHalfFootprint {
				t.Fatal("footprint guard no longer describes drawing")
			}
		}
	}
}

// TestTheWorldHoldsExactlyOnePortal is the whole design decision as an assertion: the
// lattice is an addressing scheme, not a spawner.
func TestTheWorldHoldsExactlyOnePortal(t *testing.T) {
	for _, seed := range ruinTestSeeds {
		found := 0
		for cz := int64(-24); cz <= 24; cz++ {
			for cx := int64(-24); cx <= 24; cx++ {
				r, ok := RuinAt(seed, cx, cz)
				if !ok {
					continue
				}
				found++
				if cx != 0 || cz != 0 || r.CellX != 0 || r.CellZ != 0 {
					t.Fatalf("seed %d: a ruin outside the capital cell at (%d,%d)", seed, cx, cz)
				}
			}
		}
		if found != 1 {
			t.Fatalf("seed %d: %d ruins in a 49x49 cell sweep, want 1", seed, found)
		}
		// The lookup by position agrees: inside the cell it answers, and a query a
		// whole cell away from the site cannot reach it.
		r, _ := RuinAt(seed, 0, 0)
		if near, ok := RuinNear(seed, r.CentreX, r.CentreZ, 0); !ok || near.Arch != r.Arch {
			t.Fatalf("seed %d: RuinNear lost the one ruin", seed)
		}
		if _, ok := RuinNear(seed, r.CentreX+RuinCellBlocks, r.CentreZ, RuinCellBlocks-1); ok {
			t.Fatalf("seed %d: RuinNear invented a second ruin", seed)
		}
	}
}

// TestRuinStandsNearTheCapitalInGreenLand is the placement contract. It prints every
// distance, so a regression names the seed that moved rather than a count that fell.
func TestRuinStandsNearTheCapitalInGreenLand(t *testing.T) {
	near := 0
	for _, seed := range ruinTestSeeds {
		r, ok := RuinAt(seed, 0, 0)
		if !ok {
			t.Fatalf("seed %d: placement failed, which it must never do", seed)
		}
		capital := CapitalAt(seed)
		d2 := squaredDistance(r.CentreX, r.CentreZ, capital.CentreX, capital.CentreZ)
		d := isqrt(d2)
		climate := ClimateAt(seed, r.CentreX, r.CentreZ)
		t.Logf("seed %6d: arch (%5d,%5d) %5d blocks from the capital, %v", seed, r.Arch.X, r.Arch.Z, d, climate)

		if climate != Plains && climate != Taiga {
			t.Errorf("seed %d: portal in %v, which is not green land", seed, climate)
		}
		if d2 < int64(RuinSettlementDistance)*RuinSettlementDistance {
			t.Errorf("seed %d: portal %d blocks from the capital centre, under %d", seed, d, RuinSettlementDistance)
		}
		if r.CentreX < ruinCellInset || r.CentreX > RuinCellBlocks-ruinCellInset ||
			r.CentreZ < ruinCellInset || r.CentreZ > RuinCellBlocks-ruinCellInset {
			t.Errorf("seed %d: centre (%d,%d) outside the cell inset", seed, r.CentreX, r.CentreZ)
		}
		for z := r.CentreZ - ruinHalfFootprint; z <= r.CentreZ+ruinHalfFootprint; z++ {
			for x := r.CentreX - ruinHalfFootprint; x <= r.CentreX+ruinHalfFootprint; x++ {
				if _, warded := SettlementWarding(seed, ChunkOf(x, 0, z).Column()); warded {
					t.Fatalf("seed %d: portal stands in a warded column", seed)
				}
			}
		}
		if int64(d) <= ruinPreferredDistance {
			near++
		}
	}
	// Nine tenths, which is the acceptance criterion's 18 of 20. Measured over seeds
	// -40..200 the rate is 95%; the shortfall is a capital in country the ladder's two
	// green rungs cannot answer inside three thousand blocks, and walking further is
	// the right answer there.
	if want := len(ruinTestSeeds) * 9 / 10; near < want {
		t.Fatalf("only %d of %d seeds put the portal within %d blocks, want %d", near, len(ruinTestSeeds), ruinPreferredDistance, want)
	}
	t.Logf("%d of %d seeds within %d blocks", near, len(ruinTestSeeds), ruinPreferredDistance)
}

// TestRuinPlacementNeverFails drives the last-resort branch directly. An empty ladder
// is a ladder no rung of which accepts anything, which is the state the promise is
// about; hoping to find a seed that reaches it would test nothing on the day one did.
func TestRuinPlacementNeverFails(t *testing.T) {
	for _, seed := range []int64{goldenSeed, 1, 7} {
		s := resolveRuinSite(seed, 0, 0, nil)
		if s.x < ruinCellInset || s.x > RuinCellBlocks-ruinCellInset || s.z < ruinCellInset || s.z > RuinCellBlocks-ruinCellInset {
			t.Fatalf("seed %d: fallback site (%d,%d) outside the cell inset", seed, s.x, s.z)
		}
		ground, _ := ruinGroundAt(seed, s.x, s.z)
		if s.floor != ground+1 {
			t.Fatalf("seed %d: fallback site has no resolved floor: %d, ground %d", seed, s.floor, ground)
		}
		// It is the nearest candidate to the capital, not an arbitrary one: the
		// fallback keeps the ranking even though it has dropped every rule.
		candidates := ruinCandidates(seed, 0, 0)
		if s.x != candidates[0].x || s.z != candidates[0].z {
			t.Fatalf("seed %d: fallback did not take the nearest candidate", seed)
		}
		if r := s.ruin(0, 0); r.Arch.Kind != AnchorRuinArch {
			t.Fatalf("seed %d: fallback site yields no arch", seed)
		}
		// A rung that accepts nothing must not be able to move the answer either.
		impossible := []ruinRung{{maxDistance: 1, groundDelta: 0, green: true}}
		if again := resolveRuinSite(seed, 0, 0, impossible); again != s {
			t.Fatalf("seed %d: an unsatisfiable rung changed the fallback", seed)
		}
	}
}

func TestRuinSitesArePinnedToTheirSeed(t *testing.T) {
	const (
		wantX, wantZ = 1776, 1795
		wantFloor    = 76
	)
	want := PlacedAnchor{X: 1776, Y: 71, Z: 1791, Kind: AnchorRuinArch}
	got, ok := RuinAt(goldenSeed, 0, 0)
	if !ok || got.CentreX != wantX || got.CentreZ != wantZ || got.FloorY != wantFloor ||
		got.Building.Variant != 0 || got.Building.Facing != 0 || got.Arch != want {
		t.Fatalf("golden site changed: %+v, %v", got, ok)
	}
	again, ok := RuinAt(goldenSeed, 0, 0)
	if !ok || !reflect.DeepEqual(got, again) {
		t.Fatal("site depends on earlier calls")
	}
	// The memo must not be able to answer a question it was not asked: a second seed
	// resolves to its own site, and the first one still answers as before.
	other, ok := RuinAt(goldenSeed+1, 0, 0)
	if !ok || other.CentreX == got.CentreX && other.CentreZ == got.CentreZ {
		t.Fatalf("a neighbouring seed shares the golden site: %+v", other)
	}
	if third, ok := RuinAt(goldenSeed, 0, 0); !ok || !reflect.DeepEqual(third, got) {
		t.Fatal("resolving another seed moved this one")
	}
	for _, delta := range []int64{-32, 0, 32} {
		near, ok := RuinNear(goldenSeed, got.CentreX+delta, got.CentreZ, int(absInt64(delta)))
		if !ok || !reflect.DeepEqual(near, got) {
			t.Fatal("inclusive position-centred query lost ruin", delta)
		}
		if delta != 0 {
			if _, ok := RuinNear(goldenSeed, got.CentreX+delta, got.CentreZ, int(absInt64(delta))-1); ok {
				t.Fatal("lookup exceeded radius")
			}
		}
	}
}

// ruinRulesSayAccepted re-derives the acceptance rules without calling the production
// ones, so the sweep below is a second opinion rather than a tautology.
func ruinRulesSayAccepted(t *testing.T, seed int64, x, z int64, delta int64, green bool) bool {
	t.Helper()
	if green {
		if c := ClimateAt(seed, x, z); c != Plains && c != Taiga {
			return false
		}
	}
	// Independently enumerate settlement cells covering the exclusion square; do not
	// trust the production nearest-site routine to validate itself.
	for sz := settlementCellOf(z - RuinSettlementDistance); sz <= settlementCellOf(z+RuinSettlementDistance); sz++ {
		for sx := settlementCellOf(x - RuinSettlementDistance); sx <= settlementCellOf(x+RuinSettlementDistance); sx++ {
			if town, found := SettlementAt(seed, sx, sz); found &&
				squaredDistance(x, z, town.CentreX, town.CentreZ) <= int64(RuinSettlementDistance)*RuinSettlementDistance {
				return false
			}
		}
	}
	centre, wet := ruinGroundAt(seed, x, z)
	if wet {
		return false
	}
	for dz := -ruinReach; dz <= ruinReach; dz++ {
		for dx := -ruinReach; dx <= ruinReach; dx++ {
			h, wet := ruinGroundAt(seed, x+int64(dx), z+int64(dz))
			if wet || absInt64(int64(h-centre)) > delta {
				return false
			}
		}
	}
	for wz := z - ruinHalfFootprint; wz <= z+ruinHalfFootprint; wz++ {
		for wx := x - ruinHalfFootprint; wx <= x+ruinHalfFootprint; wx++ {
			if _, warded := SettlementWarding(seed, ChunkOf(wx, 0, wz).Column()); warded {
				return false
			}
		}
	}
	return true
}

// TestRuinSweepAgreesWithIndependentRules checks two things the placement contract
// rests on and that the properties test above cannot see: that the chosen site really
// satisfies a rung of the ladder, and that the ladder really returns the *nearest*
// candidate the first satisfiable rung admits — no nearer candidate passes an earlier
// rung. The second is the expensive half, so it runs over three seeds rather than
// twenty-four; the first runs over all of them.
func TestRuinSweepAgreesWithIndependentRules(t *testing.T) {
	variants := map[uint8]bool{}
	facings := map[Facing]bool{}
	rungTaken := map[int]int{}
	for _, seed := range ruinTestSeeds {
		r, ok := RuinAt(seed, 0, 0)
		if !ok {
			t.Fatalf("seed %d: no ruin", seed)
		}
		variants[r.Building.Variant] = true
		facings[r.Building.Facing] = true
		capital := CapitalAt(seed)
		d2 := squaredDistance(r.CentreX, r.CentreZ, capital.CentreX, capital.CentreZ)
		taken := -1
		for i, rung := range ruinLadder {
			if d2 <= rung.maxDistance*rung.maxDistance || rung.maxDistance == math.MaxInt64 {
				if ruinRulesSayAccepted(t, seed, r.CentreX, r.CentreZ, rung.groundDelta, rung.green) {
					taken = i
					break
				}
			}
		}
		if taken < 0 {
			t.Fatalf("seed %d: the chosen site satisfies no rung of the ladder", seed)
		}
		rungTaken[taken]++
	}
	if len(variants) != 2 || len(facings) != 4 {
		t.Fatalf("insufficient variant/facing coverage across seeds: %v %v", variants, facings)
	}
	t.Logf("rung taken: %v", rungTaken)

	examined := 0
	for _, seed := range []int64{goldenSeed, 2, 5} {
		r, _ := RuinAt(seed, 0, 0)
		capital := CapitalAt(seed)
		chosen := squaredDistance(r.CentreX, r.CentreZ, capital.CentreX, capital.CentreZ)
		rung := ruinLadder[0]
		if !ruinRulesSayAccepted(t, seed, r.CentreX, r.CentreZ, rung.groundDelta, rung.green) {
			// This seed fell to a later rung; the nearest-first claim for rung 0 is
			// then that nothing inside the preferred distance satisfies it at all.
			chosen = rung.maxDistance*rung.maxDistance + 1
		}
		nearer := 0
		for _, c := range ruinCandidates(seed, 0, 0) {
			d2 := squaredDistance(c.x, c.z, capital.CentreX, capital.CentreZ)
			if d2 >= chosen || d2 > rung.maxDistance*rung.maxDistance {
				continue
			}
			nearer++
			if ruinRulesSayAccepted(t, seed, c.x, c.z, rung.groundDelta, rung.green) {
				t.Fatalf("seed %d: candidate (%d,%d) is nearer than the chosen site and acceptable", seed, c.x, c.z)
			}
		}
		examined += nearer
		t.Logf("seed %d: %d nearer candidates all refused by the strict rung", seed, nearer)
	}
	// A vacuous pass is the failure mode this check has: it proves the search took the
	// nearest acceptable candidate only if there were nearer ones to reject. A single
	// seed can legitimately have very few — seed 2 lands almost on the inset corner —
	// so the floor is on the total.
	if examined < 200 {
		t.Fatalf("only %d nearer candidates examined; the minimality check is vacuous", examined)
	}
}

func TestRuinIsEmittedIntoItsChunks(t *testing.T) {
	for _, seed := range []int64{goldenSeed, 1, 2} {
		r, ok := RuinAt(seed, 0, 0)
		if !ok {
			t.Fatalf("seed %d: no ruin", seed)
		}
		assertRuinEmitted(t, seed, r)
	}
}

func assertRuinEmitted(t *testing.T, seed int64, r Ruin) {
	t.Helper()
	chunks := map[Coord]*Chunk{}
	blockAt := func(x, y, z int64) Block {
		coord := ChunkOf(x, y, z)
		chunk := chunks[coord]
		if chunk == nil {
			chunk = Generate(seed, coord)
			chunks[coord] = chunk
		}
		ox, oy, oz := coord.Origin()
		return chunk.At(int(x-ox), int(y-oy), int(z-oz))
	}
	visitSchematic(r.Building, func(x, y, z int64, want Block) {
		if got := blockAt(x, y, z); got != want {
			t.Fatalf("seed %d site (%d,%d) voxel (%d,%d,%d): %v want %v", seed, r.CellX, r.CellZ, x, y, z, got, want)
		}
	})
	if blockAt(r.Arch.X, r.Arch.Y, r.Arch.Z) != PortalHeart {
		t.Fatal("lookup arch is not the portal heart")
	}
	near, ok := RuinNear(seed, r.CentreX, r.CentreZ, 0)
	if !ok || near.Arch != r.Arch {
		t.Fatal("lookup moved emitted arch")
	}
	for _, a := range r.Building.Anchors {
		if a.Kind == AnchorRuinStair && blockAt(a.X, a.Y, a.Z) != Air {
			t.Fatal("stair does not open")
		}
	}
	// At every sampled site the entire chamber ceiling is at least one block under
	// surrounding natural ground, and the prepared floor is level. The bound is the
	// ladder's widest rung: a site accepted at ruinFallbackGroundDelta sits in ground
	// that drops one block further than the strict rung would have allowed.
	for z := r.CentreZ - ruinHalfFootprint; z <= r.CentreZ+ruinHalfFootprint; z++ {
		for x := r.CentreX - ruinHalfFootprint; x <= r.CentreX+ruinHalfFootprint; x++ {
			ground, _ := ruinGroundAt(seed, x, z)
			if ground < r.FloorY-1-ruinFallbackGroundDelta {
				t.Fatal("chamber exposed above surrounding land")
			}
		}
	}
	for coord, chunk := range chunks {
		if !bytes.Equal(encodedBytes(Encode(chunk)), encodedBytes(Encode(Generate(seed, coord)))) {
			t.Fatal("independent generation changed blocks")
		}
	}
}

func TestRuinBlendAndResolvedChunkColumnsAgree(t *testing.T) {
	r, ok := RuinAt(goldenSeed, 0, 0)
	if !ok {
		t.Fatal("fixture missing")
	}
	site := ruinForArea(goldenSeed, r.CentreX, r.CentreZ, r.CentreX, r.CentreZ)
	if site == nil {
		t.Fatal("area lookup lost the site")
	}
	for dz := -18; dz <= 18; dz++ {
		for dx := -18; dx <= 18; dx++ {
			x, z := r.CentreX+int64(dx), r.CentreZ+int64(dz)
			col := columnShapeWithRuin(goldenSeed, x, z, site)
			standalone := columnShapeAt(goldenSeed, x, z)
			if col != standalone || HeightAt(goldenSeed, x, z) != col.surface {
				t.Fatalf("resolved column differs at (%d,%d)", dx, dz)
			}
			natural, _ := ruinGroundAt(goldenSeed, x, z)
			d := max(absInt64(int64(dx)), absInt64(int64(dz)))
			switch {
			case d <= 9:
				if col.surface != r.FloorY-1 {
					t.Fatal("floor not level")
				}
			case d >= 17:
				if col.surface != natural {
					t.Fatal("blend failed to meet natural terrain")
				}
			}
			if col.surface < min(natural, r.FloorY-1) || col.surface > max(natural, r.FloorY-1) {
				t.Fatal("blend overshot ground")
			}
			if d < 17 && (!col.ruin || col.carveFieldAt(goldenSeed, x, int64(r.FloorY-7), z)) {
				t.Fatal("natural cave can remove ruin foundation")
			}
		}
	}
	// The fixed-point blend has eight blocks to meet at most three blocks of change.
	for natural := r.FloorY - 1 - ruinFallbackGroundDelta; natural <= r.FloorY+1; natural++ {
		last := r.FloorY - 1
		for d := int64(9); d <= 17; d++ {
			h, _ := site.shape(site.x+d, site.z, natural)
			if absInt64(int64(h-last)) > 1 {
				t.Fatal("blend creates a cliff")
			}
			last = h
		}
	}
}

func TestRuinDistanceRefusesCapitalAndOtherCellsHoldNothing(t *testing.T) {
	town := CapitalAt(goldenSeed)
	strict := ruinLadder[0]
	for _, dx := range []int64{0, 69, 1024} {
		if _, ok := acceptRuinSite(goldenSeed, ruinSite{x: town.CentreX + dx, z: town.CentreZ}, strict); ok {
			t.Fatal("accepted site inside settlement exclusion")
		}
	}
	// Every rung refuses it, including the last-resort one: the settlement distance is
	// never relaxed, only flatness and climate are.
	for i, rung := range ruinLadder {
		if _, ok := acceptRuinSite(goldenSeed, ruinSite{x: town.CentreX, z: town.CentreZ}, rung); ok {
			t.Fatalf("rung %d accepted the capital's own centre", i)
		}
	}
	for cz := int64(-10); cz <= 10; cz++ {
		for cx := int64(-10); cx <= 10; cx++ {
			r, ok := RuinAt(goldenSeed, cx, cz)
			if ok != (cx == 0 && cz == 0) {
				t.Fatalf("cell (%d,%d) answered %v", cx, cz, ok)
			}
			if ok && (RuinCellOf(r.CentreX) != cx || RuinCellOf(r.CentreZ) != cz) {
				t.Fatal("site escaped its cell")
			}
		}
	}
}

func TestRuinDrawingStraddlesHorizontalAndVerticalChunks(t *testing.T) {
	// Seed 2's drawing crosses a chunk boundary on both axes; the golden seed's does
	// not, and with one ruin per seed the fixture has to be chosen rather than found.
	const straddlingSeed = 2
	r, ok := RuinAt(straddlingSeed, 0, 0)
	if !ok {
		t.Fatal("fixture missing")
	}
	low := ChunkOf(r.Building.OriginX, r.Building.OriginY, r.Building.OriginZ)
	w, d := rotatedFootprint(RuinSchematicFor(r.Building.Variant), r.Building.Facing)
	high := ChunkOf(r.Building.OriginX+int64(w-1), r.Building.OriginY+11, r.Building.OriginZ+int64(d-1))
	if low.Y == high.Y || (low.X == high.X && low.Z == high.Z) {
		t.Fatal("fixture no longer straddles both dimensions")
	}
	assertRuinEmitted(t, straddlingSeed, r)
}

func TestRuinNearAgreesWithWideScanAtCellEdges(t *testing.T) {
	r, ok := RuinAt(goldenSeed, 0, 0)
	if !ok {
		t.Fatal("fixture missing")
	}
	for _, x := range []int64{-1, 0, 1, 8191, 8192, 8193} {
		z := r.CentreZ
		var want Ruin
		found := false
		best := int64(8192*8192 + 1)
		for cz := int64(-2); cz <= 2; cz++ {
			for cx := int64(-2); cx <= 2; cx++ {
				c, ok := RuinAt(goldenSeed, cx, cz)
				if !ok {
					continue
				}
				d := squaredDistance(x, z, c.CentreX, c.CentreZ)
				if d < best {
					want, best, found = c, d, true
				}
			}
		}
		if best > 8192*8192 {
			want, found = Ruin{}, false
		}
		got, ok := RuinNear(goldenSeed, x, z, 8192)
		if ok != found || !reflect.DeepEqual(got, want) {
			t.Fatalf("query at cell edge %d: %+v,%v want %+v,%v", x, got, ok, want, found)
		}
	}
}

func TestRuinGoldenActuallyContainsTheArch(t *testing.T) {
	c := Generate(goldenSeed, Coord{X: 55, Y: 2, Z: 55})
	if got := c.At(16, 7, 31); got != PortalHeart {
		t.Fatalf("golden no longer contains arch: %v", got)
	}
}

func BenchmarkGenerateAtRuin(b *testing.B) {
	for b.Loop() {
		Generate(goldenSeed, Coord{X: 55, Y: 2, Z: 55})
	}
}

// BenchmarkResolveRuinSite is the cost the memo in [ruinSiteAt] hides. It is the whole
// justification for that memo existing: this is paid once per seed, and [ruinForArea]
// asks the same question once per generated column.
func BenchmarkResolveRuinSite(b *testing.B) {
	for b.Loop() {
		resolveRuinSite(goldenSeed, 0, 0, ruinLadder)
	}
}
