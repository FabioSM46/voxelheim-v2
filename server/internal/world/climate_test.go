package world

import "testing"

// The seed every statistic below is measured at. Fixed on purpose: a climate map
// is a property of a *world*, so "how much of the map is desert" is only a
// question once a seed has been named, and a sweep over seeds would average four
// different maps into a number describing none of them.
const climateSeed = 0x5EED

// The lattice the shares below are measured over: 64 steps of 1024 blocks on each
// axis, so 4096 samples spanning 65536 blocks — thirty-two climate cells across.
//
// A contiguous square would answer a different question and answer it wrongly: at
// climateScaleBlocks the field is strongly autocorrelated, so any region small
// enough to walk is mostly one or two climates, and measuring one would report
// that the others barely exist. The step is half a lattice cell, which is the
// coarsest sampling that cannot skip a cell entirely.
const (
	climateLatticeSteps = 64
	climateLatticeStep  = 1024
)

func climateShares(seed int64) (map[Climate]int, int) {
	counts := make(map[Climate]int, 4)
	total := 0
	for i := range climateLatticeSteps {
		for j := range climateLatticeSteps {
			counts[ClimateAt(seed, int64(i)*climateLatticeStep, int64(j)*climateLatticeStep)]++
			total++
		}
	}
	return counts, total
}

// A walk in a straight line changes the world under you.
//
// The user story this issue exists for, as an assertion: 4096 blocks — about
// two climate cells — is a walk, and it has to cross a boundary.
func TestAWalkOfFourThousandBlocksCrossesMoreThanOneClimate(t *testing.T) {
	t.Parallel()

	seen := make(map[Climate]bool, 4)
	for x := int64(0); x < 4096; x++ {
		seen[ClimateAt(climateSeed, x, 0)] = true
	}
	if len(seen) < 2 {
		t.Fatalf("a 4096-block transect crossed %d climate(s): %v", len(seen), seen)
	}
}

// Every climate is somewhere on the map, and none of them is a rounding error.
//
// **Three of the four clear eight percent of the lattice and the desert does not,
// and that is a measurement rather than an accident.** The thresholds are the ones
// #444 specifies — 30% temperature, 70% and 40% for a desert, 55% humidity — and
// they are read against fbm2D, whose sum of four halving octaves is concentrated
// around its midpoint rather than spread flat. At this seed the tails that matter
// are about 8.8% of columns below 30% temperature and about 7.7% above 70%, and
// about 18.6% below 40% humidity; a desert is the *intersection* of two of them,
// so it lands near 2.6% and no threshold in the issue can be met and reach 8% at
// the same time. The thresholds win, because #445, #446 and #455 are written
// against them and a floor in a test is not worth a contract.
//
// What is asserted is therefore the true floor for each climate, with the desert's
// stated as its own number rather than folded into a weaker common one — so a
// retune that spreads the fields (the honest fix, and a decision for whoever wants
// bigger deserts) fails here and is read rather than absorbed.
func TestEveryClimateCoversItsShareOfTheWorld(t *testing.T) {
	t.Parallel()

	counts, total := climateShares(climateSeed)

	for _, tc := range []struct {
		climate Climate
		percent int
	}{
		{Plains, 8},
		{Taiga, 8},
		{Tundra, 8},
		// The intersection of two tails; see the comment above.
		{Desert, 2},
	} {
		got := counts[tc.climate]
		if got*100 < tc.percent*total {
			t.Errorf("%v covers %d of %d lattice samples (%.2f%%), under the %d%% floor",
				tc.climate, got, total, 100*float64(got)/float64(total), tc.percent)
		}
	}
	if len(counts) != 4 {
		t.Errorf("the lattice found %d climates, want all 4: %v", len(counts), counts)
	}
}

// Relief, not climate, is what decides whether the land reaches the bare rock the
// altitude override produces — so a high-relief sample somewhere has to get there,
// or the mountain half of this issue does nothing.
func TestHighReliefReachesTheStoneLine(t *testing.T) {
	t.Parallel()

	const highRelief = one * 60 / 100
	reached, samples := 0, 0
	for i := range climateLatticeSteps {
		for j := range climateLatticeSteps {
			x, z := int64(i)*climateLatticeStep, int64(j)*climateLatticeStep
			if reliefAt(climateSeed, x, z) <= highRelief {
				continue
			}
			samples++
			if HeightAt(climateSeed, x, z) >= stoneLine {
				reached++
			}
		}
	}
	if samples == 0 {
		t.Fatal("no lattice sample has relief above 60%; the relief field is not varying")
	}
	if reached == 0 {
		t.Fatalf("none of the %d high-relief samples reaches the stone line at %d", samples, stoneLine)
	}

	// The other end: low relief must stay low, or the amplitude is not interpolating
	// and every column is a mountain.
	for i := range climateLatticeSteps {
		x := int64(i) * climateLatticeStep
		if relief := reliefAt(climateSeed, x, 0); relief < one*20/100 {
			if got := amplitudeAt(climateSeed, x, 0); got > (plainsAmplitude+mountainAmplitude)/4 {
				t.Fatalf("relief %d at x=%d yields amplitude %d, no flatter than the midpoint", relief, x, got)
			}
		}
	}
}

// The per-climate surface column, read off generated columns rather than restated.
//
// **The desert clause is narrower than "every desert column is sand", and it has to
// be**: the altitude overrides beat climate everywhere, so a desert column at or
// above the stone line is bare rock and one above the snow line is capped with
// snow. That is the point of the overrides — a peak reads as a peak in every
// climate — so the assertion is about the desert *ground*, which is what a walk
// crosses.
func TestEachClimateBuildsItsOwnColumn(t *testing.T) {
	t.Parallel()

	seen := make(map[Climate]int, 4)
	for i := range climateLatticeSteps {
		for j := range climateLatticeSteps {
			x, z := int64(i)*climateLatticeStep, int64(j)*climateLatticeStep
			col := columnAt(climateSeed, x, z)
			seen[col.climate]++

			if col.surface >= snowLine {
				if got := col.blockAt(col.surface); got != Snow {
					t.Fatalf("%v column at (%d, %d) is height %d, above the snow line, and its surface is %d", col.climate, x, z, col.surface, got)
				}
				if got := col.blockAt(col.surface - 1); got != Stone {
					t.Fatalf("%v column at (%d, %d) has %d under its snow cap, want Stone", col.climate, x, z, got)
				}
				continue
			}
			if col.surface >= stoneLine {
				for depth := range 4 {
					if got := col.blockAt(col.surface - depth); got != Stone {
						t.Fatalf("%v column at (%d, %d) is above the stone line and holds %d at depth %d", col.climate, x, z, got, depth)
					}
				}
				continue
			}

			// Water's own two surfaces beat the climate's ground, and they are checked
			// before the switch because a river bed and a shore are the same blocks in
			// every climate that has one. See column.blockAt for the precedence.
			if col.river {
				if got := col.blockAt(col.surface); got != Gravel {
					t.Fatalf("%v river bed at (%d, %d) is %d at depth 0, want Gravel", col.climate, x, z, got)
				}
				continue
			}
			if col.beach {
				assertColumnMaterials(t, col, x, z, []Block{Sand, Sand, Sand})
				continue
			}

			switch col.climate {
			case Desert:
				assertColumnMaterials(t, col, x, z, []Block{Sand, Sand, Sand, Sand, Sandstone, Sandstone, Sandstone, Sandstone, Sandstone, Sandstone, Sandstone, Sandstone, Stone})
			case Tundra:
				assertColumnMaterials(t, col, x, z, []Block{Snow, Dirt, Dirt, Dirt, Stone})
			case Plains, Taiga:
				if col.gravel {
					assertColumnMaterials(t, col, x, z, []Block{Gravel, Gravel, Dirt, Dirt, Stone})
					continue
				}
				assertColumnMaterials(t, col, x, z, []Block{Grass, Dirt, Dirt, Dirt, Stone})
			}
		}
	}
	if len(seen) != 4 {
		t.Fatalf("the sweep only exercised %d climates: %v", len(seen), seen)
	}
}

// assertColumnMaterials checks the top len(want) blocks of a column, depth 0 first.
func assertColumnMaterials(t *testing.T, col column, worldX, worldZ int64, want []Block) {
	t.Helper()

	for depth, block := range want {
		if got := col.blockAt(col.surface - depth); got != block {
			t.Fatalf("%v column at (%d, %d) holds %d at depth %d, want %d (surface %d, gravel %t)",
				col.climate, worldX, worldZ, got, depth, block, col.surface, col.gravel)
		}
	}
	if got := col.blockAt(col.surface + 1); got != Air {
		t.Fatalf("%v column at (%d, %d) holds %d one block above its surface", col.climate, worldX, worldZ, got)
	}
}

// Nothing grows in a tundra or a desert, and nothing grows on rock or snow.
//
// Asserted through the generator's own predicate rather than by counting logs in a
// chunk, because the claim is about every column rather than about the ones that
// happened to be sampled.
func TestNoTreeStandsInATundraADesertOrOnBareRock(t *testing.T) {
	t.Parallel()

	tundra, desert, high := 0, 0, 0
	for i := range climateLatticeSteps {
		for j := range climateLatticeSteps {
			x, z := int64(i)*climateLatticeStep, int64(j)*climateLatticeStep
			col := columnAt(climateSeed, x, z)
			_, rooted := treeAtColumn(climateSeed, x, z, col)

			switch col.climate {
			case Tundra:
				tundra++
				if rooted {
					t.Fatalf("a conifer is rooted in the tundra at (%d, %d)", x, z)
				}
			case Desert:
				desert++
				if rooted {
					t.Fatalf("a conifer is rooted in the desert at (%d, %d)", x, z)
				}
			}
			if col.surface >= stoneLine {
				high++
				if rooted {
					t.Fatalf("a conifer is rooted on bare rock at (%d, %d), height %d", x, z, col.surface)
				}
			}
		}
	}
	if tundra == 0 || desert == 0 {
		t.Fatalf("the sweep saw %d tundra and %d desert columns; both must be exercised", tundra, desert)
	}
	if high == 0 {
		t.Log("no lattice sample reached the stone line; the bare-rock clause was not exercised here")
	}
}

// Conifers arrive at the density their climate names.
//
// Measured over two squares that are one climate throughout — a taiga at the
// origin and a plain 2048 blocks north of it — and against the *eligible* columns
// rather than all of them, because a column whose surface is gravel or bare rock
// was never a candidate. Thirty percent is the band the issue asks for: a hash
// modulus is a Bernoulli trial per column, so a few hundred roots is enough to
// land well inside it and a broken denominator misses by a factor.
func TestTreeDensityFollowsItsClimate(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name        string
		originZ     int64
		climate     Climate
		denominator uint64
	}{
		{"taiga", 0, Taiga, taigaTreeChanceDenominator},
		{"plains", 2048, Plains, plainsTreeChanceDenominator},
	} {
		const side = 512
		eligible, roots := 0, 0
		for x := int64(0); x < side; x++ {
			for z := tc.originZ; z < tc.originZ+side; z++ {
				col := columnAt(climateSeed, x, z)
				if col.climate != tc.climate {
					t.Fatalf("%s square is not one climate: (%d, %d) is %v", tc.name, x, z, col.climate)
				}
				// A column under the sea line grows nothing however green its bed is,
				// so it is not a candidate the density is measured against. See
				// treeAtColumn, which refuses it for the same reason.
				if col.blockAt(col.surface) == Grass && col.surface >= seaLevel {
					eligible++
				}
				if _, ok := treeAtColumn(climateSeed, x, z, col); ok {
					roots++
				}
			}
		}
		want := float64(eligible) / float64(tc.denominator)
		if roots == 0 || want == 0 {
			t.Fatalf("%s square produced %d roots over %d eligible columns", tc.name, roots, eligible)
		}
		if ratio := float64(roots) / want; ratio < 0.7 || ratio > 1.3 {
			t.Errorf("%s square has %d roots over %d eligible columns; one in %d predicts %.0f (ratio %.2f, want within ±30%%)",
				tc.name, roots, eligible, tc.denominator, want, ratio)
		}
	}
}

// Gravel is a patch on soil: rare, and never a replacement for what a climate is.
func TestGravelPatchesAreRareAndOnlyOnSoil(t *testing.T) {
	t.Parallel()

	const side = 512
	patches, eligible := 0, 0
	for x := int64(-side / 2); x < side/2; x++ {
		for z := int64(-side / 2); z < side/2; z++ {
			col := columnAt(climateSeed, x, z)
			soil := col.surface < stoneLine && (col.climate == Plains || col.climate == Taiga)
			if soil {
				eligible++
			}
			if !col.gravel {
				continue
			}
			patches++
			if !soil {
				t.Fatalf("gravel at (%d, %d): climate %v, surface %d", x, z, col.climate, col.surface)
			}
			// The field still says "patch here"; what a *shore* is made of simply
			// beats it, and a river bed is gravel anyway but only one block deep. Both
			// are counted above, because what is being measured is the field's share
			// of eligible columns rather than how often it is the last word.
			if col.beach || col.river {
				continue
			}
			if got := col.blockAt(col.surface); got != Gravel {
				t.Fatalf("gravel column at (%d, %d) has surface block %d", x, z, got)
			}
			if got := col.blockAt(col.surface - 1); got != Gravel {
				t.Fatalf("gravel column at (%d, %d) has %d one block down, want a second Gravel", x, z, got)
			}
			// Two blocks deep and no more: the third is whatever the climate said.
			if got := col.blockAt(col.surface - gravelDepth); got == Gravel {
				t.Fatalf("gravel column at (%d, %d) reaches depth %d", x, z, gravelDepth)
			}
		}
	}
	if eligible == 0 || patches == 0 {
		t.Fatalf("the square held %d eligible columns and %d gravel patches", eligible, patches)
	}

	// About three percent, which is what gravelThreshold was chosen for. A band
	// rather than a number, because the threshold is a first guess and the share is
	// what somebody retuning it would read.
	share := 100 * float64(patches) / float64(eligible)
	if share < 1.5 || share > 5 {
		t.Errorf("gravel covers %.2f%% of eligible columns, outside the 1.5–5%% band the threshold %d aims at",
			share, gravelThreshold)
	}
}

// The three fields are three fields. A shared seed offset, or a shared scale
// sampled twice, would make temperature and humidity the same landscape twice —
// and every desert would then sit in exactly the same place as every dry taiga.
func TestTheClimateFieldsAreDecorrelated(t *testing.T) {
	t.Parallel()

	same := 0
	for i := range climateLatticeSteps {
		for j := range climateLatticeSteps {
			x, z := int64(i)*climateLatticeStep, int64(j)*climateLatticeStep
			if temperatureAt(climateSeed, x, z) == humidityAt(climateSeed, x, z) {
				same++
			}
		}
	}
	if same > climateLatticeSteps {
		t.Errorf("temperature and humidity agreed exactly at %d of %d samples; they are one field",
			same, climateLatticeSteps*climateLatticeSteps)
	}

	// Every field stays inside the fixed-point range fbm2D promises, which is what
	// the thresholds are read against.
	for i := range climateLatticeSteps {
		x := int64(i) * climateLatticeStep
		for _, field := range []struct {
			name  string
			value int64
		}{
			{"temperature", temperatureAt(climateSeed, x, 0)},
			{"humidity", humidityAt(climateSeed, x, 0)},
			{"relief", reliefAt(climateSeed, x, 0)},
		} {
			if field.value < 0 || field.value > one {
				t.Fatalf("%s at x=%d is %d, outside [0, %d]", field.name, x, field.value, one)
			}
		}
	}
}

// ClimateAt is the classification, and the order of its tests is part of it.
func TestClimateThresholdsClassifyInOrder(t *testing.T) {
	t.Parallel()

	// Cold wins over dry: a place that would be a desert on temperature alone is a
	// tundra if it is cold, which is why the tundra test comes first.
	if tundraTemperature >= desertTemperature {
		t.Fatal("the tundra and desert temperature thresholds overlap")
	}
	if desertHumidity >= taigaHumidity {
		t.Fatal("a column can be dry enough for a desert and wet enough for a taiga at once")
	}

	// The classification actually reads the fields it claims to: a column's climate
	// is a function of its temperature and humidity and of nothing else, so two
	// columns with the same pair classify alike.
	for i := range climateLatticeSteps {
		x, z := int64(i)*climateLatticeStep, int64(i)*climateLatticeStep
		temperature, humidity := temperatureAt(climateSeed, x, z), humidityAt(climateSeed, x, z)
		want := Plains
		switch {
		case temperature < tundraTemperature:
			want = Tundra
		case temperature > desertTemperature && humidity < desertHumidity:
			want = Desert
		case humidity >= taigaHumidity:
			want = Taiga
		}
		if got := ClimateAt(climateSeed, x, z); got != want {
			t.Fatalf("(%d, %d) temperature %d humidity %d classifies as %v, want %v", x, z, temperature, humidity, got, want)
		}
	}

	// A Climate this build has no name for still prints something readable, because
	// every failure message above interpolates one.
	if Climate(200).String() != "unknown climate" {
		t.Errorf("an unnamed climate prints %q", Climate(200))
	}
}

// BenchmarkGenerate is the cost of one chunk, and the number the climate fields
// have to be worth.
//
// Read it rather than trusting it: three extra fbm sums per column (temperature,
// humidity, relief) plus a fourth on the eligible ones (gravel) is real work, paid
// once per column rather than once per voxel — which is why [column] exists.
//
// Measured against worldgen 2 on the same machine over the same forty chunk
// coordinates: 1.27 ms/op before, 1.68 ms/op after — 1.31×, against the 2× the
// issue allows.
//
// **Worldgen 4 paid the rest of that budget for caves: 1.66 ms/op before, 3.26 after
// — 1.96×, against the same 2×.** Caves are the first feature here that costs per
// *voxel* rather than per column, and the arithmetic is the whole story: the carved
// band is 97 blocks deep where the ore bands together are 52, and caveAt spends 1.3
// fbm3D sums on average in it (two fields, the second one short-circuited away by
// the first about seven times in ten). Everything that keeps it under the ceiling is
// a rejection ahead of those sums — the depth band, the spawn square, the mouth
// field — and a change that moves one of them is a change to this number. There is
// no headroom left above it: the next feature that wants per-voxel noise has to buy
// it back somewhere first.
//
// **Worldgen 5 is water, and it cost 1.06× — because it wanted no per-voxel noise.**
// Measured interleaved against worldgen 4 on one machine, nine samples each of 400
// chunk generations: 3.25 ms/op before, 3.43 after. Basins and channels are two more
// fbm2D sums per *column*, which is about 32K extra lattice hashes for a chunk
// against the roughly 1.3M the cave fields already spend inside it; everything else
// water added — the sea fill, the cave fill, the beach and the bed — is integer
// comparisons on voxels that were already being visited. The warning above still
// stands and is still unspent.
//
// **Sweep the vertical coordinate, and do not measure one chunk layer.** The first
// attempt pinned Y=2 and reported 2.9×, which was not the climate fields at all: the
// old terrain topped out at 84, so a chunk at y 64..95 was mostly air and almost
// never reached oreAt, while the new one puts real rock there and pays an fbm3D per
// stone voxel in the band. That is a comparison of two different volumes of rock
// wearing the label of a comparison of two generators.
func BenchmarkGenerate(b *testing.B) {
	for i := 0; b.Loop(); i++ {
		Generate(climateSeed, Coord{X: int32(i % 8), Y: int32(i % 4), Z: int32(i % 5)})
	}
}
