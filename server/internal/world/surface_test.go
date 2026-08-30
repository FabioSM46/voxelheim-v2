package world

import "testing"

// The sample seed and window. The same seed the climate and water sweeps use, so a
// failure here can be read beside the numbers those files record.
const (
	surfaceSeed = 0x5EED

	// The benchmark's window: off the origin, because the spawn square exempts water
	// and caves and a column inside it measures the exemption rather than the fields.
	surfaceAreaOriginX = 6144
	surfaceAreaOriginZ = -2048

	// The sweep: coarse and wide, because most of the nine surfaces are regional.
	// Ice needs a submerged tundra, bare stone needs ground above the stone line, a
	// conifer is one candidate column in ninety-six — none of them is a property a
	// 64-block window is entitled to have, and a sweep that took a window instead
	// would assert nothing about six of the branches while appearing to pass.
	surfaceSweepRadius = 20000
	surfaceSweepStep   = 149

	// How many columns of each kind are checked against generated voxels. Every check
	// generates a chunk, which is the millisecond-scale part of this package, so this
	// is a sample rather than a sweep: a rule that is wrong is wrong for every column
	// it applies to, and a dozen of the rarest surface in the world is a sample the
	// commonest reaches in its first row.
	surfaceChecksPerKind = 12
)

// TestSurfaceAtIsPureInSeedAndColumn pins the property everything else here rests on:
// the map is arithmetic, so two readings of one column agree, and two columns that are
// not the same column are free to differ.
//
// Determinism is not a tidiness property for this function. A map tile is recomputed
// on every request rather than cached, so a column that answered differently on the
// second reading would make the map flicker under a player who was not moving.
func TestSurfaceAtIsPureInSeedAndColumn(t *testing.T) {
	for z := int64(-256); z <= 256; z += 37 {
		for x := int64(-256); x <= 256; x += 41 {
			height, kind := SurfaceAt(surfaceSeed, x, z)
			for again := range 3 {
				gotHeight, gotKind := SurfaceAt(surfaceSeed, x, z)
				if gotHeight != height || gotKind != kind {
					t.Fatalf("SurfaceAt(%d, %d) reading %d: got (%d, %d), first reading was (%d, %d)",
						x, z, again+2, gotHeight, gotKind, height, kind)
				}
			}
		}
	}
}

// TestSurfaceAtHeightIsTheHeightField pins the half of the answer the client shades
// with: the map's height is the terrain's height and not a second opinion about it.
//
// SurfaceAt deliberately does not call [HeightAt] — it reads the column it already
// resolved, which is what keeps the cost at one climate and one height field per pixel
// — so this is the test that the shortcut still produces the same number.
func TestSurfaceAtHeightIsTheHeightField(t *testing.T) {
	for z := int64(-512); z <= 512; z += 53 {
		for x := int64(-512); x <= 512; x += 47 {
			height, _ := SurfaceAt(surfaceSeed, x, z)
			if want := HeightAt(surfaceSeed, x, z); height != want {
				t.Fatalf("SurfaceAt(%d, %d) height = %d, HeightAt = %d", x, z, height, want)
			}
		}
	}
}

func TestATundraConiferColumnIsForestOnTheMap(t *testing.T) {
	t.Parallel()

	x, z, col, _ := findTundraConifer(t)
	if got := col.blockAt(col.surface); got != Snow {
		t.Fatalf("tundra conifer at (%d, %d) roots on %d, want Snow", x, z, got)
	}
	height, kind := SurfaceAt(climateSeed, x, z)
	if height != col.surface || kind != SurfaceForest {
		t.Fatalf("SurfaceAt(%d, %d) = (%d, %d), want (%d, %d)",
			x, z, height, kind, col.surface, SurfaceForest)
	}
}

// TestSurfaceAtNamesWhatTheGeneratorBuilt is the acceptance criterion in one sweep: the
// map is true to the ground, and every kind it can draw is drawn somewhere.
//
// **It checks against generated chunks rather than against the helpers SurfaceAt
// itself calls**, which is the whole value of it. Asserting that a Forest pixel is
// where treeAtColumn says yes would restate the implementation; asserting that a
// Forest pixel has a conifer standing on it in the chunk a player would be sent is a
// statement about the world. So a map that drifted from the generator — a reordered
// rule, a climate retune reaching one and not the other — fails here rather than in a
// screenshot.
//
// **The quota is what keeps that from passing vacuously**, and it is not a detail. The
// first version of this test took a 64-block window and checked every column in it;
// that window turned out to hold snow and ice and nothing else, so seven of the nine
// branches were asserted against no column at all while the test reported success. An
// agreement test is only worth what its coverage is, so coverage is asserted here
// rather than assumed: every kind the generator can produce must reach the quota, and
// the two nothing may return yet must never appear.
func TestSurfaceAtNamesWhatTheGeneratorBuilt(t *testing.T) {
	chunks := map[Coord]*Chunk{}
	voxel := func(x, y, z int64) Block {
		coord := ChunkOf(x, y, z)
		chunk, ok := chunks[coord]
		if !ok {
			chunk = Generate(surfaceSeed, coord)
			chunks[coord] = chunk
		}
		return chunk.At(Local(x), Local(y), Local(z))
	}

	checked := map[SurfaceKind]int{}
	for z := int64(-surfaceSweepRadius); z <= surfaceSweepRadius; z += surfaceSweepStep {
		for x := int64(-surfaceSweepRadius); x <= surfaceSweepRadius; x += surfaceSweepStep {
			height, kind := SurfaceAt(surfaceSeed, x, z)
			if checked[kind] >= surfaceChecksPerKind {
				continue
			}
			checked[kind]++
			surface := voxel(x, int64(height), z)

			switch kind {
			case SurfaceWater, SurfaceIce:
				// The ground is under the column's own water line and the fill stands
				// on top of it. The lid is one voxel at the top of the body, so both
				// kinds are read there — at the sea line for a sea or a basin, and at
				// its own terrace for a river, which since #595 may run well above it.
				want := Block(Water)
				if kind == SurfaceIce {
					want = Ice
				}
				top := int64(columnAt(surfaceSeed, x, z).waterSurface)
				if got := voxel(x, top, z); got != want {
					t.Fatalf("(%d, %d) drawn as %d, but the voxel at its water line %d is %d, not %d",
						x, z, kind, top, got, want)
				}

			case SurfaceCave:
				// A mouth removes the column's top voxel. What stands in its place is
				// caveFillAt's answer, which above the underground water line is air.
				if surface != Air && surface != Water {
					t.Fatalf("(%d, %d) drawn as a cave, but the voxel at height %d is %d", x, z, height, surface)
				}

			case SurfaceSettlement:
				// Inside a settlement the surface is the plateau, so the voxel at the
				// reported height is still the ground its climate asks for — every
				// building starts one block above it. What the pixel claims is that
				// there is a settlement standing on this column, and the way to check
				// that against the world rather than against the rule is to ask the
				// lattice for one and measure.
				s, ok := NearestSettlement(surfaceSeed, x, z)
				if !ok {
					t.Fatalf("(%d, %d) drawn as a settlement, but no settlement is within reach", x, z)
				}
				if d := isqrt(squaredDistance(x, z, s.CentreX, s.CentreZ)); d > int64(s.Radius) {
					t.Fatalf("(%d, %d) drawn as a settlement, but the nearest one is %d blocks away and its radius is %d",
						x, z, d, s.Radius)
				}
				if height != s.Plateau {
					t.Fatalf("(%d, %d) is drawn at height %d, but the settlement it is in has its plateau at %d",
						x, z, height, s.Plateau)
				}

			case SurfaceForest:
				// Every forest species starts its trunk one voxel above the ground.
				if got := voxel(x, int64(height)+1, z); got != Log && got != PalmLog {
					t.Fatalf("(%d, %d) drawn as forest, but the voxel above height %d is %d, not a forest trunk", x, z, height, got)
				}

			default:
				if got := surfaceKindOf(surface); got != kind {
					t.Fatalf("(%d, %d) drawn as %d, but the voxel at height %d is %d, which is %d",
						x, z, kind, height, surface, got)
				}
			}
		}
	}

	for _, kind := range []SurfaceKind{
		SurfaceGrass, SurfaceSnow, SurfaceSand, SurfaceStone,
		SurfaceGravel, SurfaceWater, SurfaceIce, SurfaceForest, SurfaceCave,
	} {
		if checked[kind] < surfaceChecksPerKind {
			t.Errorf("only %d columns in the sweep are drawn as %d; the branch that returns it is barely tested",
				checked[kind], kind)
		}
	}
	// Unknown is the contract's "nothing may be said", which the tile path writes for
	// an unexplored column and this function must never write for an explored one.
	//
	// **Settlement is deliberately not in the quota above.** Settlements are a lattice
	// rather than a field: this sweep steps 149 blocks and a village's flat ground is
	// 56 across, so how many of them a sample lands on is a coincidence of the two
	// numbers — measured at seed 0x5EED it is twelve, which is the quota exactly and
	// therefore no margin at all. What the settlement branch owes is asserted where it
	// can be asserted deterministically, in settlement_test.go, against a settlement
	// this test does not have to be lucky to find.
	if checked[SurfaceUnknown] != 0 {
		t.Errorf("%d columns drawn as %d, which nothing may return", checked[SurfaceUnknown], SurfaceUnknown)
	}
}

func TestForestSpeciesAndGroundCoverKeepTheirSurfaceKinds(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		species *plantSpecies
		want    SurfaceKind
	}{
		{&plantSpeciesTable[1], SurfaceForest},
		{&plantSpeciesTable[2], SurfaceSand},
		{&plantSpeciesTable[3], SurfaceForest},
		{&plantSpeciesTable[4], SurfaceGrass},
	} {
		x, z, found := int64(0), int64(0), false
		for i := 0; i < 1024 && !found; i++ {
			for j := 0; j < 1024; j++ {
				x, z = int64(i)*61, int64(j)*61
				col := columnAt(surfaceSeed, x, z)
				species, _, rooted := plantAtColumn(surfaceSeed, x, z, col)
				if rooted && species == tc.species {
					found = true
					break
				}
			}
		}
		if !found {
			t.Fatalf("fixed lattice selected no %s", tc.species.name)
		}
		if _, got := SurfaceAt(surfaceSeed, x, z); got != tc.want {
			t.Errorf("%s root at (%d, %d) maps as %d, want %d", tc.species.name, x, z, got, tc.want)
		}
	}
}

// BenchmarkSurfaceAt is the per-column cost of the map, which is the number the tile
// benchmark in internal/session is 4096 of.
//
// Measured on a machine that reproduces worldgen 5's recorded 3.43 ms/op chunk: 0.60 to
// 0.71 µs a column, so the 4096 of them a whole tile needs come to about half of one
// chunk generation. That is the shape of the trade this feature makes — the map pays a
// column of noise where a chunk pays a column of noise *and* 32 voxels of it — and it is
// why the tile path needs no cache.
//
// The coordinate sweeps rather than sitting still, for the reason BenchmarkGenerate
// sweeps its Y: one column would measure whichever branch that column happens to take.
func BenchmarkSurfaceAt(b *testing.B) {
	for i := 0; b.Loop(); i++ {
		SurfaceAt(surfaceSeed, surfaceAreaOriginX+int64(i%64), surfaceAreaOriginZ+int64(i/64%64))
	}
}
