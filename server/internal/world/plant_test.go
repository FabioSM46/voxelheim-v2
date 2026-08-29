package world

import "testing"

func TestTheConiferIsTheOnlyPlantSpeciesRow(t *testing.T) {
	t.Parallel()

	if len(plantSpeciesTable) != 1 {
		t.Fatalf("plantSpeciesTable has %d rows, want the conifer only", len(plantSpeciesTable))
	}
	species := plantSpeciesTable[0]
	if species.name != "conifer" || species.seedOffset != treeSeedOffset || species.footprint != treeCanopyRadius || !species.forest {
		t.Fatalf("conifer row = {name:%q seedOffset:%d footprint:%d forest:%t}",
			species.name, species.seedOffset, species.footprint, species.forest)
	}
	if !species.rootsOn(Grass) {
		t.Fatal("the conifer does not root on grass")
	}
	for _, block := range []Block{Air, Dirt, Stone, Snow, Sand, Gravel, Water, Ice} {
		if species.rootsOn(block) {
			t.Fatalf("the conifer roots on block %d", block)
		}
	}
	for _, tc := range []struct {
		climate Climate
		want    uint64
	}{
		{Taiga, taigaTreeChanceDenominator},
		{Plains, plainsTreeChanceDenominator},
		{Tundra, 0},
		{Desert, 0},
	} {
		if got := species.denominator(tc.climate); got != tc.want {
			t.Errorf("conifer denominator in %v = %d, want %d", tc.climate, got, tc.want)
		}
	}
}

// The table must be a refactor and no more: the old predicate is kept here only
// as an independent oracle over the fixed climate lattice, while production has
// one implementation through plantAtColumn.
func TestTheConiferRowMatchesTheLegacyPredicate(t *testing.T) {
	t.Parallel()

	for _, seed := range []int64{1, climateSeed, 0x51A7E} {
		legacy := make(map[[2]int64]int)
		table := make(map[[2]int64]int)
		climates := make(map[Climate]bool, 4)
		for i := range climateLatticeSteps {
			for j := range climateLatticeSteps {
				x := int64(i) * climateLatticeStep
				z := int64(j) * climateLatticeStep
				col := columnAt(seed, x, z)
				climates[col.climate] = true

				if height, ok := legacyTreeAtColumn(seed, x, z, col); ok {
					legacy[[2]int64{x, z}] = height
				}
				if species, h, ok := plantAtColumn(seed, x, z, col); ok {
					if species.name != "conifer" {
						t.Fatalf("seed %d column (%d, %d) selected %q with a one-row table", seed, x, z, species.name)
					}
					table[[2]int64{x, z}] = coniferTrunkHeight(h)
				}
			}
		}
		if len(climates) != 4 {
			t.Fatalf("seed %d lattice crossed %d climates, want all four", seed, len(climates))
		}
		if len(legacy) != len(table) {
			t.Fatalf("seed %d produced %d legacy roots and %d table roots", seed, len(legacy), len(table))
		}
		for column, want := range legacy {
			if got, ok := table[column]; !ok || got != want {
				t.Fatalf("seed %d column (%d, %d): table height = %d, present %t; legacy height = %d",
					seed, column[0], column[1], got, ok, want)
			}
		}
	}
}

func legacyTreeAtColumn(seed, worldX, worldZ int64, col column) (trunkHeight int, ok bool) {
	if col.settlement {
		return 0, false
	}

	var denominator uint64
	switch col.climate {
	case Taiga:
		denominator = taigaTreeChanceDenominator
	case Plains:
		denominator = plainsTreeChanceDenominator
	default:
		return 0, false
	}
	if col.blockAt(col.surface) != Grass {
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
