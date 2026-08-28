package world

import "testing"

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

// theCapital is the capital of a seed, or a fatal failure. There is always one.
func theCapital(t *testing.T, seed int64) Settlement {
	t.Helper()
	s, ok := SettlementAt(seed, settlementCellOf(spawnColumnX), settlementCellOf(spawnColumnZ))
	if !ok {
		t.Fatalf("seed %#x has no capital, which the lattice must never allow", seed)
	}
	return s
}

// TestEverySeedHasACapitalWithinAWalkOfSpawn is the one existence guarantee in this
// file, and the reason [capitalSiteAt] ranks candidates rather than refusing them.
//
// The three seeds the issue names are checked by name; the sweep behind them is what
// says the guarantee is about the lattice rather than about three lucky worlds. The
// bound is the *hashed offset's* bound, so it holds for the fallback capital too.
func TestEverySeedHasACapitalWithinAWalkOfSpawn(t *testing.T) {
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
		t.Fatalf("seed %#x: the spawn cell holds a %v", seed, s.Kind)
	}

	distance := isqrt(squaredDistance(spawnColumnX, spawnColumnZ, s.CentreX, s.CentreZ))
	if distance < capitalMinSpawnDistance || distance > capitalMaxSpawnDistance {
		t.Errorf("seed %#x: the capital is %d blocks from spawn, outside [%d, %d]",
			seed, distance, capitalMinSpawnDistance, capitalMaxSpawnDistance)
	}
	if s.Plateau < settlementMinPlateau {
		t.Errorf("seed %#x: the capital's plateau is at %d, at or under the water line", seed, s.Plateau)
	}
	if s.Radius != capitalRadius {
		t.Errorf("seed %#x: the capital's radius is %d, want %d", seed, s.Radius, capitalRadius)
	}
	// A keep, a hall, a smithy and six huts.
	if want := 3 + capitalHutCount; len(s.Buildings) != want {
		t.Errorf("seed %#x: the capital has %d buildings, want %d", seed, len(s.Buildings), want)
	}
}

// TestOnlyTheSpawnCellHoldsACapital pins the other half of "there is one capital".
func TestOnlyTheSpawnCellHoldsACapital(t *testing.T) {
	t.Parallel()

	villages := 0
	for cz := int64(-4); cz <= 4; cz++ {
		for cx := int64(-4); cx <= 4; cx++ {
			s, ok := SettlementAt(settlementTestSeed, cx, cz)
			if !ok {
				continue
			}
			isSpawnCell := cx == settlementCellOf(spawnColumnX) && cz == settlementCellOf(spawnColumnZ)
			if (s.Kind == SettlementCapital) != isSpawnCell {
				t.Fatalf("cell (%d, %d) holds a %v", cx, cz, s.Kind)
			}
			if s.Kind == SettlementVillage {
				villages++
			}
		}
	}
	if villages == 0 {
		t.Fatal("eighty cells around spawn hold no village at all; the density rule is not being exercised")
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

	refusedByRelief, refusedByWater, accepted := 0, 0, 0
	for cz := int64(-cells); cz <= cells; cz++ {
		for cx := int64(-cells); cx <= cells; cx++ {
			if isCapitalCell(cx, cz) {
				continue
			}
			candidate, proposed := settlementCandidateAt(settlementTestSeed, cx, cz)
			_, held := SettlementAt(settlementTestSeed, cx, cz)
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
			default:
				if !held && !riverCrossesSite(settlementTestSeed, candidate) {
					t.Fatalf("cell (%d, %d) passes every rule and still holds nothing", cx, cz)
				}
				if held {
					accepted++
				}
				continue
			}
			if held {
				t.Fatalf("cell (%d, %d) was refused its site and holds a settlement anyway", cx, cz)
			}
		}
	}

	if refusedByRelief == 0 || refusedByWater == 0 || accepted == 0 {
		t.Fatalf("the sweep found %d sites refused for relief, %d for water and %d accepted; each case needs one",
			refusedByRelief, refusedByWater, accepted)
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
			// channel and no basin, because both of them move ground this feature has
			// just finished flattening.
			if col.river {
				t.Fatalf("(%d, %d) is inside the capital's blend band and is a river bed", x, z)
			}
			if got := col.surface; got < min(s.Plateau, unloweredHeightAt(settlementTestSeed, x, z)) {
				t.Fatalf("(%d, %d) has been lowered to %d inside the blend band", x, z, got)
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

	for _, s := range SettlementsNear(settlementTestSeed, spawnColumnX, spawnColumnZ, 1) {
		anchors := s.Anchors()
		if len(anchors) == 0 {
			t.Fatalf("the %v at (%d, %d) offers no slot", s.Kind, s.CentreX, s.CentreZ)
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
// statement the canopy makes: a keep is fifteen blocks across and fourteen tall and can
// straddle four chunk columns at once. Generating the pair in both orders is what says
// no chunk is reading, caching or mutating anything belonging to its neighbour — which,
// if it ever became untrue, would show up as a wall that exists only when a player
// happens to walk in from the east.
func TestABuildingIsBuiltFromItsDrawingWhicheverChunkAsks(t *testing.T) {
	t.Parallel()

	s := theCapital(t, settlementTestSeed)
	split, checked := 0, 0

	for _, b := range s.Buildings {
		schematic := SchematicFor(b.Kind)
		w, d := rotatedFootprint(schematic, b.Facing)
		if ChunkOf(b.OriginX, 0, 0).X == ChunkOf(b.OriginX+int64(w)-1, 0, 0).X &&
			ChunkOf(0, 0, b.OriginZ).Z == ChunkOf(0, 0, b.OriginZ+int64(d)-1).Z {
			continue // wholly inside one chunk column; the split ones are the point
		}
		split++

		low := ChunkOf(b.OriginX, b.OriginY, b.OriginZ)
		high := ChunkOf(b.OriginX+int64(w)-1, b.OriginY, b.OriginZ+int64(d)-1)

		forwards := map[Coord]*Chunk{}
		for _, coord := range []Coord{low, high} {
			forwards[coord] = Generate(settlementTestSeed, coord)
		}
		backwards := map[Coord]*Chunk{}
		for _, coord := range []Coord{high, low} {
			backwards[coord] = Generate(settlementTestSeed, coord)
		}

		visitSchematic(b, func(x, y, z int64, block Block) {
			if block == Air {
				return // the clip writes nothing for a room's air; there is nothing to compare
			}
			coord := ChunkOf(x, y, z)
			chunk, held := forwards[coord]
			if !held {
				return
			}
			checked++
			local := [3]int{Local(x), Local(y), Local(z)}
			got := chunk.At(local[0], local[1], local[2])
			if got != block {
				t.Fatalf("the %v's voxel at (%d, %d, %d) is block %d, and its drawing says %d",
					b.Kind, x, y, z, got, block)
			}
			if other := backwards[coord].At(local[0], local[1], local[2]); other != got {
				t.Fatalf("the %v's voxel at (%d, %d, %d) is %d in one generation order and %d in the other",
					b.Kind, x, y, z, got, other)
			}
		})
	}

	if split == 0 || checked == 0 {
		t.Fatalf("%d of the capital's buildings cross a chunk border and %d voxels were compared; the test asserted nothing",
			split, checked)
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

	nearest, ok := NearestSettlement(settlementTestSeed, x, z)
	if !ok {
		t.Fatal("no settlement within three cells of a column that has two within two")
	}
	if nearest.CentreX != near[0].CentreX || nearest.CentreZ != near[0].CentreZ {
		t.Fatalf("NearestSettlement is at (%d, %d) and the head of SettlementsNear is at (%d, %d)",
			nearest.CentreX, nearest.CentreZ, near[0].CentreX, near[0].CentreZ)
	}

	// A count of zero is one cell — the one the column is in — rather than nothing.
	if own := SettlementsNear(settlementTestSeed, x, z, 0); len(own) > 1 {
		t.Fatalf("a zero-cell search returned %d settlements", len(own))
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

// TestBuildingsStandClearOfEachOtherAndInsideThePlateau is the layout's own invariant.
//
// Two buildings sharing a voxel would be silently absorbed by the clip — whichever was
// written first would win and the other would lose a wall — so nothing about the
// generated chunk would look wrong enough to notice. The footprints are checked as
// boxes, which is stricter than the voxels and therefore the right thing to assert.
func TestBuildingsStandClearOfEachOtherAndInsideThePlateau(t *testing.T) {
	t.Parallel()

	for cz := int64(-3); cz <= 3; cz++ {
		for cx := int64(-3); cx <= 3; cx++ {
			s, ok := SettlementAt(settlementTestSeed, cx, cz)
			if !ok {
				continue
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
						t.Fatalf("two buildings in cell (%d, %d) overlap: %+v and %+v", cx, cz, a, b)
					}
				}
			}
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
func BenchmarkGenerateInACapital(b *testing.B) {
	s, ok := SettlementAt(settlementTestSeed, settlementCellOf(spawnColumnX), settlementCellOf(spawnColumnZ))
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
