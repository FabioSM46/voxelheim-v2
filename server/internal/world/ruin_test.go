package world

import (
	"bytes"
	"math"
	"reflect"
	"testing"
)

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
	if RuinCellBlocks != 8192 || RuinSettlementDistance != 1024 || ruinCellInset != 1024 || ruinBlendBlocks != 8 || ruinMaxGroundDelta != 2 {
		t.Fatal("placement constants changed without updating the world contract")
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

func TestRuinSitesArePinnedToTheirSeed(t *testing.T) {
	for _, tt := range []struct {
		cx, cz, x, z int64
		floor        int
		variant      uint8
		facing       Facing
		arch         PlacedAnchor
	}{
		{0, -12, 7091, -93684, 62, 1, 2, PlacedAnchor{X: 7091, Y: 57, Z: -93680, Kind: AnchorRuinArch}},
		{-6, 6, -46753, 54922, 69, 0, 3, PlacedAnchor{X: -46757, Y: 64, Z: 54922, Kind: AnchorRuinArch}},
	} {
		got, ok := RuinAt(goldenSeed, tt.cx, tt.cz)
		if !ok || got.CentreX != tt.x || got.CentreZ != tt.z || got.FloorY != tt.floor || got.Building.Variant != tt.variant || got.Building.Facing != tt.facing || got.Arch != tt.arch {
			t.Fatalf("site (%d,%d) changed: %+v, %v", tt.cx, tt.cz, got, ok)
		}
		again, ok := RuinAt(goldenSeed, tt.cx, tt.cz)
		if !ok || !reflect.DeepEqual(got, again) {
			t.Fatal("site depends on earlier calls")
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
}

func TestRuinSweepExcludesSettlementsWaterAndSlopes(t *testing.T) {
	counts := [5]int{} // empty, settlement, wet, uneven, accepted
	variants := map[uint8]bool{}
	facings := map[Facing]bool{}
	for _, seed := range []int64{goldenSeed, 1, 17} {
		var accepted []Ruin
		for cz := int64(-24); cz <= 24; cz++ {
			for cx := int64(-24); cx <= 24; cx++ {
				c, proposed := ruinCandidateAt(seed, cx, cz)
				if !proposed {
					counts[0]++
					continue
				}
				s, ok := RuinAt(seed, cx, cz)
				reason := 4
				// Independently enumerate settlement cells covering the exclusion square;
				// do not trust the production nearest-site routine to validate itself.
				for sz := settlementCellOf(c.z - 1024); sz <= settlementCellOf(c.z+1024); sz++ {
					for sx := settlementCellOf(c.x - 1024); sx <= settlementCellOf(c.x+1024); sx++ {
						if town, found := SettlementAt(seed, sx, sz); found && squaredDistance(c.x, c.z, town.CentreX, town.CentreZ) <= 1024*1024 {
							reason = 1
						}
					}
				}
				if reason == 4 {
					centre, wet := ruinGroundAt(seed, c.x, c.z)
					if wet {
						reason = 2
					} else {
						for dz := -17; dz <= 17 && reason == 4; dz++ {
							for dx := -17; dx <= 17; dx++ {
								h, wet := ruinGroundAt(seed, c.x+int64(dx), c.z+int64(dz))
								if wet {
									reason = 2
									break
								}
								if absInt64(int64(h-centre)) > 2 {
									reason = 3
									break
								}
							}
						}
					}
				}
				counts[reason]++
				if ok != (reason == 4) {
					t.Fatalf("seed %d cell (%d,%d): accepted %v reason %d", seed, cx, cz, ok, reason)
				}
				if !ok {
					continue
				}
				if RuinCellOf(s.CentreX) != cx || RuinCellOf(s.CentreZ) != cz {
					t.Fatal("candidate escaped cell")
				}
				for _, other := range accepted {
					if squaredDistance(s.CentreX, s.CentreZ, other.CentreX, other.CentreZ) < 2049*2049 {
						t.Fatal("ruins too close")
					}
				}
				for z := s.CentreZ - 9; z <= s.CentreZ+9; z++ {
					for x := s.CentreX - 9; x <= s.CentreX+9; x++ {
						if _, warded := SettlementWarding(seed, ChunkOf(x, 0, z).Column()); warded {
							t.Fatal("ruin enters ward")
						}
					}
				}
				variants[s.Building.Variant] = true
				facings[s.Building.Facing] = true
				// Every accepted site's complete drawing is compared with independently
				// generated chunks, including explicit cellar air and rotated arch masonry.
				assertRuinEmitted(t, seed, s)
				accepted = append(accepted, s)
			}
		}
	}
	for reason, n := range counts {
		if n == 0 {
			t.Errorf("sweep never exercised refusal/acceptance class %d", reason)
		}
	}
	if counts[4] < 8 || len(variants) != 2 || len(facings) != 4 {
		t.Fatalf("insufficient accepted/variant/facing coverage: %v %v %v", counts, variants, facings)
	}
	t.Logf("empty/settlement/wet/uneven/accepted: %v", counts)
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
	if blockAt(r.Arch.X, r.Arch.Y, r.Arch.Z) != SmoothBlackStone {
		t.Fatal("lookup arch is not sealed masonry")
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
	// At every sampled site the entire chamber ceiling is at least one block
	// under surrounding natural ground, and the prepared floor is level.
	for z := r.CentreZ - 9; z <= r.CentreZ+9; z++ {
		for x := r.CentreX - 9; x <= r.CentreX+9; x++ {
			ground, _ := ruinGroundAt(seed, x, z)
			if ground < r.FloorY-3 {
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
	r, ok := RuinAt(goldenSeed, -6, 6)
	if !ok {
		t.Fatal("fixture missing")
	}
	site := ruinForArea(goldenSeed, r.CentreX, r.CentreZ, r.CentreX, r.CentreZ)
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
	// The fixed-point blend has eight blocks to meet at most two blocks of change.
	for natural := r.FloorY - 3; natural <= r.FloorY+1; natural++ {
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

func TestRuinDistanceRefusesCapitalAndNoCandidateMoves(t *testing.T) {
	town := CapitalAt(goldenSeed)
	for _, dx := range []int64{0, 69, 1024} {
		if _, ok := acceptRuinSite(goldenSeed, ruinSite{x: town.CentreX + dx, z: town.CentreZ}); ok {
			t.Fatal("accepted site inside settlement exclusion")
		}
	}
	for cz := int64(-10); cz <= 10; cz++ {
		for cx := int64(-10); cx <= 10; cx++ {
			c, proposed := ruinCandidateAt(goldenSeed, cx, cz)
			r, ok := RuinAt(goldenSeed, cx, cz)
			if ok && (!proposed || r.CentreX != c.x || r.CentreZ != c.z) {
				t.Fatal("refused candidate moved")
			}
		}
	}
}

func TestRuinDrawingStraddlesHorizontalAndVerticalChunks(t *testing.T) {
	r, ok := RuinAt(goldenSeed, -6, 6)
	if !ok {
		t.Fatal("fixture missing")
	}
	low := ChunkOf(r.Building.OriginX, r.Building.OriginY, r.Building.OriginZ)
	w, d := rotatedFootprint(RuinSchematicFor(r.Building.Variant), r.Building.Facing)
	high := ChunkOf(r.Building.OriginX+int64(w-1), r.Building.OriginY+11, r.Building.OriginZ+int64(d-1))
	if low.Y == high.Y || (low.X == high.X && low.Z == high.Z) {
		t.Fatal("fixture no longer straddles both dimensions")
	}
	assertRuinEmitted(t, goldenSeed, r)
}

func BenchmarkGenerateAtRuin(b *testing.B) {
	for b.Loop() {
		Generate(goldenSeed, Coord{X: -1462, Y: 2, Z: 1716})
	}
}

func TestRuinNearAgreesWithWideScanAtCellEdges(t *testing.T) {
	for _, x := range []int64{-1, 0, 1, 8191, 8192, 8193} {
		const z = -93684
		var want Ruin
		found := false
		best := int64(8192*8192 + 1)
		for cz := int64(-14); cz <= -10; cz++ {
			for cx := int64(-2); cx <= 3; cx++ {
				r, ok := RuinAt(goldenSeed, cx, cz)
				if !ok {
					continue
				}
				d := squaredDistance(x, z, r.CentreX, r.CentreZ)
				if d < best {
					want, best, found = r, d, true
				}
			}
		}
		got, ok := RuinNear(goldenSeed, x, z, 8192)
		if ok != found || !reflect.DeepEqual(got, want) {
			t.Fatalf("query at cell edge %d: %+v,%v want %+v,%v", x, got, ok, want, found)
		}
	}
}

func TestRuinGoldenActuallyContainsTheArch(t *testing.T) {
	c := Generate(goldenSeed, Coord{X: -1462, Y: 2, Z: 1716})
	if got := c.At(27, 0, 10); got != SmoothBlackStone {
		t.Fatalf("golden no longer contains arch: %v", got)
	}
}
