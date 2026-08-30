package world

import (
	"slices"
	"testing"
)

func TestNextWaterTruthTable(t *testing.T) {
	t.Parallel()

	sourceSides := [4]Block{Water, Air, Air, Air}
	tests := []struct {
		name         string
		here         Block
		above, below Block
		sides        [4]Block
		want         Block
	}{
		{name: "plain source is permanent", here: Water, below: Air, want: Water},
		{name: "current source is permanent", here: WaterCurrentXPos, below: Air, want: WaterCurrentXPos},
		{name: "solid is unchanged", here: Stone, above: Water, below: Air, want: Stone},
		{name: "ice is unchanged", here: Ice, above: Water, below: Air, want: Ice},
		{name: "source above air falls", here: Air, above: Water, below: Air, want: WaterFlow7},
		{name: "flow above flow falls", here: WaterFlow2, above: WaterFlow1, below: Stone, want: WaterFlow7},
		{name: "unsupported flow drains", here: WaterFlow6, below: Air, sides: sourceSides, want: Air},
		{name: "low flow below drains", here: WaterFlow6, below: WaterFlow6, sides: sourceSides, want: Air},
		{name: "air over a drop remains air", here: Air, below: Air, sides: sourceSides, want: Air},
		{name: "source spreads seven", here: Air, below: Stone, sides: sourceSides, want: WaterFlow7},
		{name: "level one spreads nowhere", here: Air, below: Stone, sides: [4]Block{WaterFlow1}, want: Air},
		{name: "level six spreads five", here: Air, below: Stone, sides: [4]Block{WaterFlow6}, want: WaterFlow5},
		{name: "flow weakens with its supply", here: WaterFlow7, below: Stone, sides: [4]Block{WaterFlow4}, want: WaterFlow3},
		{name: "no side supply drains on support", here: WaterFlow1, below: Stone, want: Air},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			if got := NextWater(test.here, test.above, test.below, test.sides); got != test.want {
				t.Errorf("NextWater(%d, %d, %d, %v) = %d, want %d",
					test.here, test.above, test.below, test.sides, got, test.want)
			}
		})
	}
}

func TestNextWaterNeverCreatesASource(t *testing.T) {
	t.Parallel()

	inputs := []Block{Air, Stone, Ice, Water, WaterFlow1, WaterFlow4, WaterFlow7,
		WaterCurrentXPos, WaterCurrentXNeg, WaterCurrentZPos, WaterCurrentZNeg}
	for _, here := range inputs {
		for _, above := range inputs {
			for _, below := range inputs {
				sides := [4]Block{Water, WaterFlow7, WaterCurrentZPos, Air}
				got := NextWater(here, above, below, sides)
				if waterSource(got) && got != here {
					t.Fatalf("NextWater created source %d from here=%d above=%d below=%d", got, here, above, below)
				}
			}
		}
	}
}

func TestAClosedFlowingBasinDrainsWithoutASource(t *testing.T) {
	t.Parallel()

	basin := [3][3]Block{
		{WaterFlow3, WaterFlow3, WaterFlow3},
		{WaterFlow3, WaterFlow3, WaterFlow3},
		{WaterFlow3, WaterFlow3, WaterFlow3},
	}
	for pass := 0; pass < 3; pass++ {
		next := basin
		for z := range basin {
			for x := range basin[z] {
				sides := [4]Block{}
				if x > 0 {
					sides[0] = basin[z][x-1]
				}
				if x+1 < len(basin[z]) {
					sides[1] = basin[z][x+1]
				}
				if z > 0 {
					sides[2] = basin[z-1][x]
				}
				if z+1 < len(basin) {
					sides[3] = basin[z+1][x]
				}
				next[z][x] = NextWater(basin[z][x], Air, Stone, sides)
			}
		}
		basin = next
	}
	for z := range basin {
		for x, got := range basin[z] {
			if got != Air {
				t.Errorf("basin[%d][%d] = %d after draining, want Air", z, x, got)
			}
		}
	}
}

func TestUnstableWaterFindsOnlyAirBoundariesAndFaces(t *testing.T) {
	t.Parallel()

	chunk := NewChunk(Coord{X: 2, Y: -1, Z: 4})
	for y := 10; y <= 12; y++ {
		for z := 10; z <= 12; z++ {
			for x := 10; x <= 12; x++ {
				chunk.Set(x, y, z, Water)
			}
		}
	}
	face := Index(0, 20, 20)
	chunk.Blocks[face] = WaterFlow4

	got := UnstableWater(chunk)
	if len(got) != 27 || !slices.Contains(got, face) {
		t.Fatalf("UnstableWater returned %d indices (face present %v), want 26 shell voxels plus face",
			len(got), slices.Contains(got, face))
	}
	if slices.Contains(got, Index(11, 11, 11)) {
		t.Error("the lake interior was reported unstable")
	}
}

// The flow automaton leaves ground cover exactly where it found it, and **the dry
// case is the one that matters**: before [Cover] joined the first arm a flower beside
// a scheduled voxel reached the drain rule, whose answer is Air.
func TestTheFlowAutomatonLeavesGroundCoverAlone(t *testing.T) {
	t.Parallel()

	for _, flower := range []Block{FlowerRed, FlowerYellow, FlowerBlue} {
		for _, tc := range []struct {
			name  string
			above Block
			below Block
			sides [4]Block
		}{
			{"dry, on grass", Air, Grass, [4]Block{Air, Air, Air, Air}},
			{"beside a source", Air, Grass, [4]Block{Water, Air, Air, Air}},
			{"under water", Water, Grass, [4]Block{Air, Air, Air, Air}},
			{"over nothing", Air, Air, [4]Block{Air, Air, Air, Air}},
		} {
			if got := NextWater(flower, tc.above, tc.below, tc.sides); got != flower {
				t.Errorf("%s: NextWater(%d, ...) = %d, want the flower left alone", tc.name, flower, got)
			}
		}
	}

	// Air in the same neighbourhood still flows, so the arm above is about cover
	// rather than about having stopped the automaton.
	if got := NextWater(Air, Air, Grass, [4]Block{Air, Air, Air, Air}); got != Air {
		t.Errorf("dry air = %d, want Air", got)
	}
	if got := NextWater(Air, Air, Grass, [4]Block{Water, Air, Air, Air}); got == Air {
		t.Error("air beside a source stayed air; the automaton is no longer flowing")
	}
	// And cover carries no water level, so a flower beside a flow feeds it nothing.
	if got := WaterLevel(FlowerRed); got != 0 {
		t.Errorf("WaterLevel(FlowerRed) = %d, want 0", got)
	}
}
