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
		// The three rows #653 changed, and each of them recorded the defect rather
		// than a rule: with a side still able to feed it, a cell over a void is the
		// head of a fall and not a cell to drain. Their unfed twins follow each one,
		// so the drain arm keeps a negative control at every position it owns.
		{name: "unsupported flow with a feed falls", here: WaterFlow6, below: Air, sides: sourceSides, want: WaterFlow7},
		{name: "unsupported flow with no feed drains", here: WaterFlow6, below: Air, want: Air},
		{name: "a fall pours into the partial flow below it", here: WaterFlow6, below: WaterFlow6, sides: sourceSides, want: WaterFlow7},
		{name: "a partial flow below with no feed drains", here: WaterFlow6, below: WaterFlow6, want: Air},
		{name: "air over a drop beside water becomes a fall", here: Air, below: Air, sides: sourceSides, want: WaterFlow7},
		{name: "air over a drop with no feed remains air", here: Air, below: Air, want: Air},
		// The threshold is the spread rule's own: level 2 is the last that leaves a
		// neighbour anything, so it is the last that may start a fall — and the lip it
		// starts carries that neighbour's level less one, never a full seven. A fall
		// whose lip answered seven whatever fed it re-fed its own supplier and the
		// whole spread oscillated; see [NextWater].
		{name: "a level-two side starts a thin fall", here: Air, below: Air, sides: [4]Block{WaterFlow2}, want: WaterFlow1},
		{name: "a level-one side starts nothing", here: Air, below: Air, sides: [4]Block{WaterFlow1}, want: Air},
		{name: "a fall's lip thins with its supply", here: Air, below: Air, sides: [4]Block{WaterFlow5}, want: WaterFlow4},
		// And the column under a lip is full, from the arm at the top: that is where a
		// fall's one full-strength answer comes from.
		{name: "the column under a lip is full", here: Air, above: WaterFlow4, below: Air, want: WaterFlow7},
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

// The other side of the same neighbourhood: a *water* cell with a flower beside it.
//
// **[NextWater] is the whole of where water may spread, and it asks a side for nothing
// but [WaterLevel]** — its only Solid call reads `here`. A flower side is therefore
// exactly as neutral as air, and a flower *below* is not Air, so the drain arm does not
// fire under water standing over one.
func TestWaterBesideGroundCoverKeepsItsLevel(t *testing.T) {
	t.Parallel()

	// An equivalence rather than a table of levels: swapping a flower onto an empty side
	// changes nothing, for every water id and in every position.
	for _, here := range []Block{Water, WaterFlow1, WaterFlow4, WaterFlow7, WaterCurrentXPos} {
		for _, flower := range []Block{FlowerRed, FlowerYellow, FlowerBlue} {
			for position := 1; position < 4; position++ {
				sides := [4]Block{Water, Air, Air, Air}
				want := NextWater(here, Air, Grass, sides)
				sides[position] = flower
				if got := NextWater(here, Air, Grass, sides); got != want {
					t.Errorf("NextWater(%d, ...) with %d on side %d = %d, want %d as with air",
						here, flower, position, got, want)
				}
			}
		}
	}

	// And the levels, so two equally wrong answers cannot satisfy the equivalence.
	if got := NextWater(Water, Air, Grass, [4]Block{FlowerRed, FlowerYellow, FlowerBlue, Air}); got != Water {
		t.Errorf("a source ringed by flowers became %d, want it left alone", got)
	}
	if got := NextWater(WaterFlow4, Air, Grass, [4]Block{Water, FlowerRed, FlowerYellow, FlowerBlue}); got != WaterFlow7 {
		t.Errorf("a flow beside a source and three flowers = %d, want %d", got, WaterFlow7)
	}
	if got := NextWater(WaterFlow7, Air, FlowerRed, [4]Block{Water, Air, Air, Air}); got != WaterFlow7 {
		t.Errorf("water standing on a flower = %d, want it to keep level 7", got)
	}
	// The drain arm's negative control: it does fire with nothing below and nothing
	// on any side to feed a fall. The feed is what the check has to exclude since
	// #653 — with a source beside it the same cell is a waterfall, not a leak.
	if got := NextWater(WaterFlow7, Air, Air, [4]Block{Air, Air, Air, Air}); got != Air {
		t.Errorf("water over nothing, fed by nothing = %d, want it to drain", got)
	}
	if got := NextWater(WaterFlow7, Air, Air, [4]Block{Water, Air, Air, Air}); got != WaterFlow7 {
		t.Errorf("water over nothing beside a source = %d, want it to keep falling", got)
	}
}

// A pool on a ledge, which before #653 was a fixed point of [NextWater]: it settled in
// one step with zero changes, and no water ever left it. The three things asserted here
// are the three halves of "water either rests in a container or falls" — it spills, the
// spill is stable, and it drains completely once nothing feeds it.
//
// Two dimensions rather than three, because a fall is a question about `below` and one
// horizontal axis; the other two sides are held at Stone so the fixture says only what
// it means to. `flowWorld` is the smallest thing that can ask [NextWater] the question
// it is documented to answer, and it is deliberately not an ECS, a chunk or a cache.
type flowWorld struct {
	cells  [][]Block // [x][y]
	width  int
	height int
}

func newFlowWorld(width, height int) *flowWorld {
	cells := make([][]Block, width)
	for x := range cells {
		cells[x] = make([]Block, height)
	}
	return &flowWorld{cells: cells, width: width, height: height}
}

// at answers Stone outside the fixture, so the walls hold water in and the floor holds
// it up — an out-of-bounds Air would be a second, undeclared drop.
func (w *flowWorld) at(x, y int) Block {
	if x < 0 || x >= w.width || y < 0 || y >= w.height {
		return Stone
	}
	return w.cells[x][y]
}

// step advances every cell once and reports how many changed.
func (w *flowWorld) step() int {
	next := newFlowWorld(w.width, w.height)
	changed := 0
	for x := range w.width {
		for y := range w.height {
			sides := [4]Block{w.at(x+1, y), w.at(x-1, y), Stone, Stone}
			got := NextWater(w.at(x, y), w.at(x, y+1), w.at(x, y-1), sides)
			next.cells[x][y] = got
			if got != w.cells[x][y] {
				changed++
			}
		}
	}
	w.cells = next.cells
	return changed
}

// settle runs to a fixed point and reports the steps it took, or -1 if it never reached
// one. A cap rather than a loop: a rule that oscillates must fail this test rather than
// hang it.
func (w *flowWorld) settle(cap int) int {
	for step := 1; step <= cap; step++ {
		if w.step() == 0 {
			return step
		}
	}
	return -1
}

func (w *flowWorld) waterCount() int {
	count := 0
	for x := range w.width {
		for y := range w.height {
			if IsWater(w.cells[x][y]) {
				count++
			}
		}
	}
	return count
}

// ledgePool builds the fixture: a floor at y=0, a three-high ledge over x<=2, and a
// source pool standing on top of it. Everything right of the ledge is open air down to
// the floor — which is the drop nothing used to fall down.
//
//	y3 SSS...
//	y2 ###...
//	y1 ###...
//	y0 ######
func ledgePool(withSource bool) *flowWorld {
	w := newFlowWorld(6, 6)
	for x := range w.width {
		for y := range w.height {
			switch {
			case y == 0, x <= 2 && y <= 2:
				w.cells[x][y] = Stone
			default:
				w.cells[x][y] = Air
			}
		}
	}
	if withSource {
		for x := range 3 {
			w.cells[x][3] = Water
		}
	}
	return w
}

func TestAPoolOnALedgeSpillsAndFalls(t *testing.T) {
	t.Parallel()

	w := ledgePool(true)
	steps := w.settle(64)
	if steps < 0 {
		t.Fatal("the ledge never settled; the rule oscillates")
	}

	// It spilled: there is water off the ledge.
	if got := w.at(3, 3); !IsWater(got) {
		t.Errorf("the cell beside the pool is %d, want water spilling into it", got)
	}
	// It fell: the water reached the floor, three blocks down and past the lip.
	if got := w.at(3, 1); !IsWater(got) {
		t.Errorf("the cell at the foot of the fall is %d, want water that arrived there", got)
	}
	// And it is a fall rather than a source that moved: nothing outside the pool is a
	// source, which TestNextWaterNeverCreatesASource holds in general and this holds
	// on the one fixture where it would be visible.
	for x := 3; x < w.width; x++ {
		for y := range w.height {
			if waterSource(w.at(x, y)) {
				t.Errorf("a source appeared off the ledge at (%d, %d)", x, y)
			}
		}
	}

	// **Stable, and that is the claim the old drain arm existed to protect.** A rule
	// that created water over a void and then drained it would pass every assertion
	// above and flicker forever; settling to zero changes above already says it does
	// not, and one more step says the fixed point is a fixed point.
	if changed := w.step(); changed != 0 {
		t.Errorf("%d cells changed after the world had settled; the rule flickers", changed)
	}
}

func TestAFallDrainsCompletelyWhenItsFeedStops(t *testing.T) {
	t.Parallel()

	w := ledgePool(true)
	if steps := w.settle(64); steps < 0 {
		t.Fatal("the ledge never settled")
	}
	if w.waterCount() <= 3 {
		t.Fatalf("only %d water cells after settling; the fall never formed", w.waterCount())
	}

	// Take the source away, the way breaking the bank of a river does.
	for x := range 3 {
		w.cells[x][3] = Air
	}
	if steps := w.settle(64); steps < 0 {
		t.Fatal("the drained ledge never settled")
	}
	if got := w.waterCount(); got != 0 {
		t.Errorf("%d water cells left after the feed stopped, want the fall gone entirely", got)
	}
}
