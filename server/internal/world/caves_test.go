package world

import "testing"

// The statistics below are all measured at one seed over one named area, because
// that is what makes them reproducible: a cave system is a shape, and the only
// honest way to assert a shape is to count it somewhere fixed.
const (
	caveSeed = 0x5EED

	// The sample area, and why it is not at the origin. Two reasons, and both
	// matter: spawnCaveClearance exempts a square around (0, 0), so an area
	// containing spawn measures the exemption as well as the field; and
	// caveMouthScaleBlocks is 96, so a 128-block window is barely more than one
	// lattice cell of the mouth field and whether it holds a mouth at all is a
	// regional property rather than a global one. This window has mouths in it. A
	// window that did not would still be a correct world.
	caveAreaOriginX = 512
	caveAreaOriginZ = 512
	caveAreaSize    = 128

	// How far down from each column's surface the carved fraction is counted.
	caveAreaDepth = 64

	// The band the carved fraction has to land in. Below it the tunnels have pinched
	// shut into disconnected pockets; above it the rock is a sponge and mining stops
	// meaning anything. See caveHalfWidth for the measurements that chose 5/100.
	minCarvedPercent = 4
	maxCarvedPercent = 12
)

// TestCarvedFractionIsATunnelNetworkNotASponge is the number caveHalfWidth exists
// to produce, measured rather than asserted in a comment.
//
// It also demands a mouth: a cave system nobody can walk into from the surface is
// a cave system nobody finds, and the mouth field is the only thing that puts one
// there.
func TestCarvedFractionIsATunnelNetworkNotASponge(t *testing.T) {
	t.Parallel()

	carved, total, mouthColumns := 0, 0, 0
	for z := int64(caveAreaOriginZ); z < caveAreaOriginZ+caveAreaSize; z++ {
		for x := int64(caveAreaOriginX); x < caveAreaOriginX+caveAreaSize; x++ {
			col := columnAt(caveSeed, x, z)
			surface := col.surface
			if col.carvedTop(caveSeed, x, z) < surface {
				mouthColumns++
			}
			for depth := range caveAreaDepth {
				total++
				if caveAt(caveSeed, x, int64(surface-depth), z, surface) {
					carved++
				}
			}
		}
	}

	if percent := carved * 100 / total; percent < minCarvedPercent || percent > maxCarvedPercent {
		t.Errorf("%d of %d voxels carved (%d%%), outside the designed [%d%%, %d%%]",
			carved, total, percent, minCarvedPercent, maxCarvedPercent)
	}
	if mouthColumns == 0 {
		t.Error("no column in the sample area has a carved surface voxel: the caves have no way in")
	}
}

// Ore in a wall is the whole point of the issue that asked for caves: the bands
// were already there, and what carving adds is a surface you can see them on.
//
// The assertion is 6-adjacency between an ore voxel and air in generated chunks,
// not in caveAt alone, because the composition order is what makes it true —
// carving runs before oreAt and oreAt only ever replaces Stone, so a vein a tunnel
// runs through is cut rather than left hanging in the void. The second assertion is
// the other half of that sentence: no ore voxel anywhere in the sample may be
// surrounded by air on all six sides.
//
// **Adjacency is read in world coordinates, and a per-column depth index is not
// one.** Two neighbouring columns rarely share a surface height, so "the same depth
// in the column next door" is a different y, and a neighbour test written that way
// would be comparing voxels that do not touch.
//
// **The sweep is wide because ore is rare, not because caves are.** Both bands sit
// at a threshold of 90/100 in a field concentrated around its midpoint, which comes
// to a couple of dozen ore voxels in a hundred and ninety-two chunks — so a sample
// sized for the carved fraction would hold no ore to say anything about. The shape
// of the sweep mirrors TestOreAppearsOnlyInStoneAndInsideItsDepthBand for the same
// reason it exists there.
func TestOreIsLaidBareOnACaveWall(t *testing.T) {
	t.Parallel()

	neighbours := [][3]int{{1, 0, 0}, {-1, 0, 0}, {0, 1, 0}, {0, -1, 0}, {0, 0, 1}, {0, 0, -1}}

	ore, exposed, floating := 0, 0, 0
	for seed := int64(1); seed <= 16; seed++ {
		for cz := int32(-1); cz <= 0; cz++ {
			for cx := int32(-1); cx <= 0; cx++ {
				for cy := int32(0); cy <= 2; cy++ {
					chunk := Generate(seed, Coord{X: cx, Y: cy, Z: cz})
					// The chunk's own faces are skipped: a neighbour outside it lives in
					// another chunk, and this test is about adjacency rather than about how
					// many voxels it can reach.
					for z := 1; z < ChunkSize-1; z++ {
						for y := 1; y < ChunkSize-1; y++ {
							for x := 1; x < ChunkSize-1; x++ {
								block := chunk.At(x, y, z)
								if block != CoalOre && block != IronOre {
									continue
								}
								ore++
								solid := 0
								for _, n := range neighbours {
									if chunk.At(x+n[0], y+n[1], z+n[2]) == Air {
										exposed++
									} else {
										solid++
									}
								}
								if solid == 0 {
									floating++
								}
							}
						}
					}
				}
			}
		}
	}

	if ore == 0 {
		t.Fatal("the sample volume holds no ore at all, so it cannot say anything about cave walls")
	}
	if exposed == 0 {
		t.Errorf("none of the %d ore voxels in the sample touches air: the tunnels never cut a vein", ore)
	}
	if floating > 0 {
		t.Errorf("%d of %d ore voxels have no solid neighbour: ore is hanging in the tunnels", floating, ore)
	}
}

// Mouths have to be rare, or the ground is a colander and the depth floor that
// keeps a tunnel from erasing the ground underfoot means nothing.
func TestCaveMouthsAreRareEnoughToBeWorthFinding(t *testing.T) {
	t.Parallel()

	// A window wide enough to hold several lattice cells of a 96-block field, so
	// this measures the threshold rather than one cell's luck.
	const span = 512
	const minPercent, maxPercent = 2, 10

	mouths, total := 0, 0
	for z := int64(-span / 2); z < span/2; z++ {
		for x := int64(-span / 2); x < span/2; x++ {
			total++
			if caveMouthAt(caveSeed, x, z) {
				mouths++
			}
		}
	}

	if percent := mouths * 100 / total; percent < minPercent || percent > maxPercent {
		t.Errorf("%d of %d columns permit a mouth (%d%%), outside the designed [%d%%, %d%%]",
			mouths, total, percent, minPercent, maxPercent)
	}
}

// The depth bounds, stated as the rule rather than as a sample: nothing above the
// ground is carved, nothing under caveMaxDepth is, and the top two blocks of a
// column are carved only where the mouth field allows it.
func TestCarvingStaysInsideItsDepthBand(t *testing.T) {
	t.Parallel()

	checkedMouth, checkedShallow := 0, 0
	for z := int64(caveAreaOriginZ); z < caveAreaOriginZ+64; z++ {
		for x := int64(caveAreaOriginX); x < caveAreaOriginX+64; x++ {
			surface := columnAt(caveSeed, x, z).surface

			for above := 1; above <= 4; above++ {
				if caveAt(caveSeed, x, int64(surface+above), z, surface) {
					t.Fatalf("(%d, %d) is carved %d blocks above its surface %d", x, z, above, surface)
				}
			}
			for _, depth := range []int{caveMaxDepth + 1, caveMaxDepth + 17, caveMaxDepth + 200} {
				if caveAt(caveSeed, x, int64(surface-depth), z, surface) {
					t.Fatalf("(%d, %d) is carved at depth %d, past caveMaxDepth %d", x, z, depth, caveMaxDepth)
				}
			}

			if caveMouthAt(caveSeed, x, z) {
				checkedMouth++
				continue
			}
			for depth := range caveMinDepth {
				checkedShallow++
				if caveAt(caveSeed, x, int64(surface-depth), z, surface) {
					t.Fatalf("(%d, %d) is carved at depth %d without a mouth", x, z, depth)
				}
			}
		}
	}
	if checkedMouth == 0 || checkedShallow == 0 {
		t.Fatalf("the sweep saw %d mouth columns and %d shallow voxels; both cases must be exercised", checkedMouth, checkedShallow)
	}
}

// Nothing is carved near the origin column, and its ground is still whole.
//
// **The second half used to be about the spawn and no longer can be**: [SpawnAt] read this
// column until #519. The spawn is the capital's plateau now, which no cave reaches for a
// different reason ([settlementCaveClearance]).
func TestNothingIsCarvedNearTheOriginColumn(t *testing.T) {
	t.Parallel()

	for seed := int64(1); seed <= 200; seed++ {
		for z := int64(spawnColumnZ - spawnCaveClearance); z <= spawnColumnZ+spawnCaveClearance; z++ {
			for x := int64(spawnColumnX - spawnCaveClearance); x <= spawnColumnX+spawnCaveClearance; x++ {
				surface := columnAt(seed, x, z).surface
				for depth := range caveMaxDepth + 1 {
					if caveAt(seed, x, int64(surface-depth), z, surface) {
						t.Fatalf("seed %d carved (%d, %d) at depth %d, inside the origin clearance", seed, x, z, depth)
					}
				}
			}
		}

		surface := HeightAt(seed, spawnColumnX, spawnColumnZ)
		if top := generatedColumnTop(seed, spawnColumnX, spawnColumnZ); top < surface {
			t.Fatalf("seed %d has a carved origin column: generated top %d is below its surface %d", seed, top, surface)
		}
	}
}

// A tree needs ground to stand on, and a cave mouth takes the ground away. The
// sweep is over the sample area rather than a single column so the rule is
// exercised wherever it applies.
func TestNoTreeIsRootedOnACarvedSurface(t *testing.T) {
	t.Parallel()

	roots, carvedSurfaces := 0, 0
	for z := int64(caveAreaOriginZ); z < caveAreaOriginZ+caveAreaSize; z++ {
		for x := int64(caveAreaOriginX); x < caveAreaOriginX+caveAreaSize; x++ {
			col := columnAt(caveSeed, x, z)
			carved := caveAt(caveSeed, x, int64(col.surface), z, col.surface)
			if carved {
				carvedSurfaces++
			}
			if _, ok := treeAtColumn(caveSeed, x, z, col); !ok {
				continue
			}
			roots++
			if carved {
				t.Fatalf("a conifer is rooted at (%d, %d), whose surface voxel is carved away", x, z)
			}
		}
	}
	if roots == 0 || carvedSurfaces == 0 {
		t.Fatalf("the sweep saw %d tree roots and %d carved surfaces; both cases must be exercised", roots, carvedSurfaces)
	}
}

// generatedColumnTop is what SpawnAt and every other "where is the ground here"
// caller reads, so it has to answer for a column a mouth has opened. The test
// finds such a column rather than constructing one, and then checks the helper
// against the voxels the generator actually wrote.
func TestGeneratedColumnTopFollowsACaveMouth(t *testing.T) {
	t.Parallel()

	x, z, surface, top := findCarvedSurfaceColumn(t)
	if got := generatedColumnTop(caveSeed, x, z); got != top {
		t.Fatalf("generatedColumnTop(%d, %d) = %d, want the carved top %d (surface %d)", x, z, got, top, surface)
	}

	// And the same answer read out of a generated chunk, which is the claim that
	// matters: the helper and the generator must not be able to disagree.
	actualTop := surface
	for y := surface; y > surface-caveMaxDepth; y-- {
		coord := ChunkOf(x, int64(y), z)
		chunk := Generate(caveSeed, coord)
		originX, originY, originZ := coord.Origin()
		if chunk.At(int(x-originX), y-int(originY), int(z-originZ)) != Air {
			actualTop = y
			break
		}
	}
	if actualTop != top {
		t.Fatalf("the generated column's top solid at (%d, %d) is y=%d, but carvedTop said %d", x, z, actualTop, top)
	}
	if actualTop >= surface {
		t.Fatalf("the selected column at (%d, %d) is not open: top %d, surface %d", x, z, actualTop, surface)
	}
}

// findCarvedSurfaceColumn returns the first column of the sample area whose
// surface voxel a mouth has carved away.
func findCarvedSurfaceColumn(t *testing.T) (x, z int64, surface, top int) {
	t.Helper()

	for z = caveAreaOriginZ; z < caveAreaOriginZ+caveAreaSize; z++ {
		for x = caveAreaOriginX; x < caveAreaOriginX+caveAreaSize; x++ {
			col := columnAt(caveSeed, x, z)
			surface = col.surface
			if top = col.carvedTop(caveSeed, x, z); top < surface {
				return x, z, surface, top
			}
		}
	}
	t.Fatal("no cave mouth in the sample area: the statistics test should have failed first")
	return 0, 0, 0, 0
}

// Carving reads nothing outside its own voxel, which is what lets two chunks that
// share a face agree without either one consulting the other. Stating it as a
// property rather than trusting the border tests in generate_test.go, because those
// compare chunks and this compares the function to itself across a chunk boundary.
func TestCarvingIsPureAndChunkLocal(t *testing.T) {
	t.Parallel()

	for i := range int64(4096) {
		x := caveAreaOriginX + i%97 - 48
		z := caveAreaOriginZ + i%89 - 44
		surface := columnAt(caveSeed, x, z).surface
		y := int64(surface) - i%caveMaxDepth

		first := caveAt(caveSeed, x, y, z, surface)
		if second := caveAt(caveSeed, x, y, z, surface); first != second {
			t.Fatalf("caveAt(%d, %d, %d) answered %t then %t", x, y, z, first, second)
		}
	}
}
