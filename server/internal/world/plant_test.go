package world

import "testing"

func TestThePlantSpeciesTableNamesConiferPalmAndScrub(t *testing.T) {
	t.Parallel()

	if len(plantSpeciesTable) != 3 {
		t.Fatalf("plantSpeciesTable has %d rows, want conifer, palm and shrub", len(plantSpeciesTable))
	}
	conifer := plantSpeciesTable[0]
	if conifer.name != "conifer" || conifer.seedOffset != treeSeedOffset || conifer.footprint != treeCanopyRadius || !conifer.forest {
		t.Fatalf("conifer row = {name:%q seedOffset:%d footprint:%d forest:%t}",
			conifer.name, conifer.seedOffset, conifer.footprint, conifer.forest)
	}
	for _, block := range []Block{Grass, Snow} {
		if !conifer.rootsOn(block) {
			t.Fatalf("the conifer does not root on block %d", block)
		}
	}
	for _, block := range []Block{Air, Dirt, Stone, Sand, Sandstone, Gravel, Water, Ice} {
		if conifer.rootsOn(block) {
			t.Fatalf("the conifer roots on block %d", block)
		}
	}
	for _, tc := range []struct {
		climate Climate
		want    uint64
	}{
		{Taiga, taigaTreeChanceDenominator},
		{Plains, plainsTreeChanceDenominator},
		{Tundra, tundraTreeChanceDenominator},
		{Desert, 0},
	} {
		if got := conifer.denominator(tc.climate); got != tc.want {
			t.Errorf("conifer denominator in %v = %d, want %d", tc.climate, got, tc.want)
		}
	}

	for index, tc := range []struct {
		name        string
		seedOffset  int64
		denominator uint64
		footprint   int
		forest      bool
	}{
		{"palm", palmSeedOffset, palmChanceDenominator, palmFrondLength, true},
		{"shrub", shrubSeedOffset, shrubChanceDenominator, 0, false},
	} {
		species := plantSpeciesTable[index+1]
		if species.name != tc.name || species.seedOffset != tc.seedOffset || species.footprint != tc.footprint || species.forest != tc.forest {
			t.Errorf("row %d = {name:%q seedOffset:%d footprint:%d forest:%t}, want %+v",
				index+1, species.name, species.seedOffset, species.footprint, species.forest, tc)
		}
		if !species.rootsOn(Sand) || species.rootsOn(Sandstone) {
			t.Errorf("%s rootsOn(Sand) = %t and rootsOn(Sandstone) = %t, want true and false",
				tc.name, species.rootsOn(Sand), species.rootsOn(Sandstone))
		}
		for _, climate := range []Climate{Plains, Taiga, Tundra} {
			if got := species.denominator(climate); got != 0 {
				t.Errorf("%s denominator in %v = %d, want 0", tc.name, climate, got)
			}
		}
		if got := species.denominator(Desert); got != tc.denominator {
			t.Errorf("%s denominator in desert = %d, want %d", tc.name, got, tc.denominator)
		}
	}
	if palmSeedOffset == shrubSeedOffset || palmSeedOffset == treeSeedOffset || shrubSeedOffset == treeSeedOffset {
		t.Fatal("plant species share a seed offset")
	}
}

func TestOnlyATundraConiferWearsASnowCap(t *testing.T) {
	t.Parallel()

	x, z, col, h := findTundraConifer(t)
	trunkHeight := coniferTrunkHeight(h)
	wantY := int64(col.surface + trunkHeight + treeCanopyAboveCrown + 1)
	snow := make([][3]int64, 0, 1)
	visitConifer(climateSeed, x, z, col.surface, h, func(x, y, z int64, block Block) {
		if block == Snow {
			snow = append(snow, [3]int64{x, y, z})
		}
	})
	if len(snow) != 1 || snow[0] != [3]int64{x, wantY, z} {
		t.Fatalf("tundra conifer snow voxels = %v, want [(%d, %d, %d)]", snow, x, wantY, z)
	}

	for _, tc := range []struct {
		name string
		x    int64
		z    int64
	}{
		{"taiga", 0, 0},
		{"plains", 0, 2048},
	} {
		if got := ClimateAt(climateSeed, tc.x, tc.z); (tc.name == "taiga" && got != Taiga) || (tc.name == "plains" && got != Plains) {
			t.Fatalf("%s shape probe is in %v", tc.name, got)
		}
		visitConifer(climateSeed, tc.x, tc.z, col.surface, h, func(_, _, _ int64, block Block) {
			if block == Snow {
				t.Errorf("%s conifer emitted a snow cap", tc.name)
			}
		})
	}
}

// The row's two independent predicates and the selector's snow-climate rule are
// restated here as one oracle. Production keeps them separate so a species row
// remains data; this test proves their composition names exactly the intended
// columns, including tundra snow but excluding altitude snow elsewhere.
func TestTheConiferRowMatchesItsColumnPredicate(t *testing.T) {
	t.Parallel()

	for _, seed := range []int64{1, climateSeed, 0x51A7E} {
		oracle := make(map[[2]int64]int)
		table := make(map[[2]int64]int)
		climates := make(map[Climate]bool, 4)
		for i := range climateLatticeSteps {
			for j := range climateLatticeSteps {
				x := int64(i) * climateLatticeStep
				z := int64(j) * climateLatticeStep
				col := columnAt(seed, x, z)
				climates[col.climate] = true

				if height, ok := expectedConiferAtColumn(seed, x, z, col); ok {
					oracle[[2]int64{x, z}] = height
				}
				if species, h, ok := plantAtColumn(seed, x, z, col); ok && species == &plantSpeciesTable[0] {
					table[[2]int64{x, z}] = coniferTrunkHeight(h)
				}
			}
		}
		if len(climates) != 4 {
			t.Fatalf("seed %d lattice crossed %d climates, want all four", seed, len(climates))
		}
		if len(oracle) != len(table) {
			t.Fatalf("seed %d produced %d oracle roots and %d table roots", seed, len(oracle), len(table))
		}
		for column, want := range oracle {
			if got, ok := table[column]; !ok || got != want {
				t.Fatalf("seed %d column (%d, %d): table height = %d, present %t; oracle height = %d",
					seed, column[0], column[1], got, ok, want)
			}
		}
	}
}

func TestPalmShapeHasATrunkCrownArmsDroopAndDiagonals(t *testing.T) {
	t.Parallel()

	const (
		rootX   = int64(10)
		rootZ   = int64(-7)
		surface = 64
	)
	h := uint64(2) << 32
	got := make(map[[3]int64]Block)
	visitPalm(0, rootX, rootZ, surface, h, func(x, y, z int64, block Block) {
		got[[3]int64{x, y, z}] = block
	})

	trunkHeight := palmMinTrunkHeight + 2
	trunkTop, crownY := int64(surface+trunkHeight), int64(surface+trunkHeight+1)
	for y := int64(surface + 1); y <= trunkTop; y++ {
		if block := got[[3]int64{rootX, y, rootZ}]; block != PalmLog {
			t.Errorf("trunk at y=%d is %d, want PalmLog", y, block)
		}
	}
	for _, pos := range [][3]int64{
		{rootX, crownY, rootZ},
		{rootX + 1, crownY, rootZ}, {rootX - 1, crownY, rootZ},
		{rootX, crownY, rootZ + 1}, {rootX, crownY, rootZ - 1},
		{rootX + 1, crownY, rootZ + 1}, {rootX + 1, crownY, rootZ - 1},
		{rootX - 1, crownY, rootZ + 1}, {rootX - 1, crownY, rootZ - 1},
		{rootX + palmFrondLength, crownY - 1, rootZ},
		{rootX - palmFrondLength, crownY - 1, rootZ},
		{rootX, crownY - 1, rootZ + palmFrondLength},
		{rootX, crownY - 1, rootZ - palmFrondLength},
	} {
		if block := got[pos]; block != PalmFronds {
			t.Errorf("crown voxel %v is %d, want PalmFronds", pos, block)
		}
	}
	if got[[3]int64{rootX + palmFrondLength, crownY, rootZ}] != Air {
		t.Fatal("outer frond did not droop one block")
	}
}

func TestShrubIsOneBlockAboveItsRoot(t *testing.T) {
	t.Parallel()

	visited := make(map[[3]int64]Block)
	visitShrub(0, 4, 9, 61, 0, func(x, y, z int64, block Block) {
		visited[[3]int64{x, y, z}] = block
	})
	if len(visited) != 1 || visited[[3]int64{4, 62, 9}] != DesertShrub {
		t.Fatalf("shrub shape = %v, want one DesertShrub at (4, 62, 9)", visited)
	}
}

func expectedConiferAtColumn(seed, worldX, worldZ int64, col column) (trunkHeight int, ok bool) {
	if col.settlement {
		return 0, false
	}

	var denominator uint64
	switch col.climate {
	case Taiga:
		denominator = taigaTreeChanceDenominator
	case Plains:
		denominator = plainsTreeChanceDenominator
	case Tundra:
		denominator = tundraTreeChanceDenominator
	default:
		return 0, false
	}
	surface := col.blockAt(col.surface)
	if surface != Grass && (surface != Snow || col.climate != Tundra) {
		return 0, false
	}

	h := hashLattice(seed+treeSeedOffset, worldX, worldZ)
	if h%denominator != 0 || col.surface < seaLevel || col.carvedAt(seed, worldX, int64(col.surface), worldZ) {
		return 0, false
	}
	return treeMinTrunkHeight + int((h>>32)%treeHeightVariants), true
}

func TestPlantSpeciesTableOrderOwnsAContestedColumn(t *testing.T) {
	t.Parallel()

	const seed = int64(0xA11CE)
	x, z, col := uncarvedPlantTestColumn(t, seed)
	checked := [2]int{}
	visited := [2]int{}
	table := []plantSpecies{
		{
			name:       "first",
			seedOffset: 1,
			rootsOn:    func(Block) bool { return true },
			denominator: func(Climate) uint64 {
				checked[0]++
				return 1
			},
			visit: func(_ int64, _, _ int64, _ int, _ uint64, _ func(int64, int64, int64, Block)) {
				visited[0]++
			},
		},
		{
			name:       "second",
			seedOffset: 2,
			rootsOn:    func(Block) bool { return true },
			denominator: func(Climate) uint64 {
				checked[1]++
				return 1
			},
			visit: func(_ int64, _, _ int64, _ int, _ uint64, _ func(int64, int64, int64, Block)) {
				visited[1]++
			},
		},
	}

	visitPlantAtColumnIn(table, seed, x, z, col, func(int64, int64, int64, Block) {})
	if checked != [2]int{1, 0} || visited != [2]int{1, 0} {
		t.Fatalf("table priority checked %v and visited %v, want only the first row", checked, visited)
	}
}

func TestPlantSpeciesSkipsTheSurfacePredicateWhenItsClimateIsAbsent(t *testing.T) {
	t.Parallel()

	const seed = int64(0xA11CE)
	x, z, col := uncarvedPlantTestColumn(t, seed)
	table := []plantSpecies{{
		name:       "absent",
		seedOffset: 1,
		rootsOn: func(Block) bool {
			t.Fatal("surface predicate was consulted after a zero denominator")
			return false
		},
		denominator: func(Climate) uint64 { return 0 },
	}}

	if species, _, ok := plantAtColumnIn(table, seed, x, z, col); ok || species != nil {
		t.Fatalf("absent species returned (%v, %t), want (nil, false)", species, ok)
	}
}

func TestPlantSpeciesShareSettlementAndSeaLevelRefusals(t *testing.T) {
	t.Parallel()

	const seed = int64(0xA11CE)
	x, z, col := uncarvedPlantTestColumn(t, seed)
	table := []plantSpecies{{
		name:        "candidate",
		seedOffset:  1,
		rootsOn:     func(Block) bool { return true },
		denominator: func(Climate) uint64 { return 1 },
	}}

	settled := col
	settled.settlement = true
	if species, _, ok := plantAtColumnIn(table, seed, x, z, settled); ok || species != nil {
		t.Fatalf("settlement selected (%v, %t), want no plant", species, ok)
	}

	submerged := col
	submerged.surface = seaLevel - 1
	if species, _, ok := plantAtColumnIn(table, seed, x, z, submerged); ok || species != nil {
		t.Fatalf("submerged surface selected (%v, %t), want no plant", species, ok)
	}
}

func uncarvedPlantTestColumn(t *testing.T, seed int64) (int64, int64, column) {
	t.Helper()
	for z := int64(-64); z <= 64; z++ {
		for x := int64(-64); x <= 64; x++ {
			col := columnAt(seed, x, z)
			if !col.settlement && col.surface >= seaLevel && !col.carvedAt(seed, x, int64(col.surface), z) {
				return x, z, col
			}
		}
	}
	t.Fatal("no uncarved column found for the priority test")
	return 0, 0, column{}
}

func findTundraConifer(t *testing.T) (x, z int64, col column, h uint64) {
	t.Helper()
	for x = 3584; x < 3584+512; x++ {
		for z = -31744; z < -31744+512; z++ {
			col = columnAt(climateSeed, x, z)
			if col.climate != Tundra {
				t.Fatalf("fixed tundra square contains %v at (%d, %d)", col.climate, x, z)
			}
			species, candidateHash, ok := plantAtColumn(climateSeed, x, z, col)
			if ok && species == &plantSpeciesTable[0] {
				return x, z, col, candidateHash
			}
		}
	}
	t.Fatal("fixed tundra square contains no conifer")
	return 0, 0, column{}, 0
}
