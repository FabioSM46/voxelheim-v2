package world

// NextWater returns the self-only next block from its six neighbours and the four
// blocks above its four horizontal ones.
//
// **The first arm is "this cell is not water's to decide", and ground cover joins it.**
// It was written as source-or-solid while the only non-solid ids were water and air,
// so "not solid and not water" meant "empty" — and a flower is neither. Without
// [Cover] a flower beside a scheduled voxel falls through to the drain rule, whose
// answer with no water on any side is Air: a drift next to a river would be mown.
//
// **It leaves the flower rather than flooding it, which is the conservative half of a
// real choice.** game.allowPlacement lets a placement displace cover, so flooding was
// the symmetric answer; but a placement happens once, an automaton runs for as long
// as there is water, and nothing regrows a flower. Water goes around.
func NextWater(here Block, above, below Block, sides, sidesAbove [4]Block) Block {
	if waterSource(here) || Solid(here) || Cover(here) {
		return here
	}

	if IsWater(above) {
		return WaterFlow7
	}

	// **A side with water above it is a column on its way down, and it hands on
	// nothing.** Without this the cell under a fall is full — from the arm above — and
	// a full cell fed its unsupported neighbour, whose own column was then full and fed
	// the next one out, so the wet cone widened for as long as the fall was tall.
	// Measured on a plain cliff with one five-by-five pool at the lip, counting only
	// water standing on nothing:
	//
	//	height   without this test   with it
	//	     2    154, reach 12       154, reach 6
	//	     4   1460, reach 24       308, reach 6
	//	     8   7972, reach 48       616, reach 6
	//	    16  32078, reach 51*     1232, reach 6
	//	    32  82830, reach 51*     2464, reach 6
	//	                             (* the measuring window ran out, not the water)
	//
	// Every one of those is a `BlockUpdate` to every client watching the chunk. With
	// the test the total is linear in the height and the reach is a constant six —
	// the spread rule's own range — rather than growing with the drop.
	//
	// **"Has water above it" and not "has nothing under it", which was the first
	// attempt and does not work.** A falling column's cell stands on the next cell of
	// the same column, which is full water and therefore reads as support; every cell
	// of a fall looked grounded and the cone was unchanged to the voxel. What is above
	// a cell is the question that separates the two, and it is answerable locally.
	//
	// The *lip* of a fall has air above it, so it is not falling and it does spread —
	// which is how water reaches the edge of a shelf and how a fall gets its width.
	// The bottom of a fall is not falling either, so a plunge pool spreads normally.
	maxSide := 0
	for i, side := range sides {
		if IsWater(sidesAbove[i]) {
			continue
		}
		step := waterSideSteps[i]
		if IsWater(side) && !WaterFeedsToward(side, -step[0], -step[1]) {
			continue
		}
		maxSide = max(maxSide, WaterLevel(side))
	}

	if unsupported(below) && maxSide < 2 {
		return Air
	}

	// **The fed half falls through to the spread rule below, and the level it takes
	// there is load-bearing.** The first attempt at this returned [WaterFlow7] — a fall
	// is full, the reasoning went, and a column falling at two-eighths is not something
	// anybody can see. Measured on real river terrain it **did not settle**: 2000 steps
	// and 472642 changes, with cells cycling 1 -> 7 -> 2 -> 7. A cell that is full is
	// the strongest side a neighbour can have, so a fall fed at level 2 answered 7, fed
	// its own supplier back at 6, and the whole spread pumped itself up and collapsed
	// again forever. Taking `maxSide - 1` like any other spread removes the loop at its
	// source: what a cell hands on is always strictly less than what it was handed, in
	// every direction, so no cycle can gain.
	//
	// **The fall is still full where a fall is actually full**: the cell *underneath*
	// this one has water directly above it and takes [WaterFlow7] from the arm at the
	// top of this function. So the lip of a fall thins with its supply and the column
	// under the lip is full, which is both what water does and what leaves this
	// function's one full-strength answer sourced from above rather than from the side.
	//
	// **Two, and not one, and it is the spread rule's own threshold.** A side of level 2
	// is what [waterFlow] turns into level 1 on flat ground, and a side of level 1 into
	// Air — so a level-1 neighbour has nothing left to give anything, and the arm above
	// is exactly "no side can still supply this cell".
	return waterFlow(maxSide - 1)
}

// waterSideSteps is NextWater's side order, as the offset from here to the side.
// A side source feeds here along the inverse vector.
var waterSideSteps = [4][2]int{{1, 0}, {-1, 0}, {0, 1}, {0, -1}}

// unsupported reports whether what is under a voxel can hold water up: air cannot, and
// neither can a flowing cell that is not full, because the water in it is on its way
// somewhere else.
//
// Named rather than repeated, because since #653 the same question is asked twice in
// two arms that answer it oppositely — a fed cell falls, an unfed one drains — and two
// copies of a condition that must stay identical is the shape this package avoids.
func unsupported(below Block) bool {
	return below == Air || flowingBelowSeven(below)
}

func waterSource(block Block) bool {
	return block == Water || block >= WaterCurrentXPos && block <= WaterCurrentZNeg
}

func flowingBelowSeven(block Block) bool {
	return block >= WaterFlow1 && block < WaterFlow7
}

func waterFlow(level int) Block {
	if level < 1 {
		return Air
	}
	if level > 7 {
		level = 7
	}
	return WaterFlow1 + Block(level-1)
}

// UnstableWater returns water beside local Air plus water on chunk faces, in index order.
func UnstableWater(chunk *Chunk) []int {
	if chunk == nil || len(chunk.Blocks) != ChunkVolume {
		return nil
	}

	unstable := make([]int, 0)
	for y := 0; y < ChunkSize; y++ {
		for z := 0; z < ChunkSize; z++ {
			for x := 0; x < ChunkSize; x++ {
				index := Index(x, y, z)
				if !IsWater(chunk.Blocks[index]) {
					continue
				}
				if x == 0 || x == ChunkSize-1 || y == 0 || y == ChunkSize-1 || z == 0 || z == ChunkSize-1 ||
					chunk.At(x-1, y, z) == Air || chunk.At(x+1, y, z) == Air ||
					chunk.At(x, y-1, z) == Air || chunk.At(x, y+1, z) == Air ||
					chunk.At(x, y, z-1) == Air || chunk.At(x, y, z+1) == Air {
					unstable = append(unstable, index)
				}
			}
		}
	}
	return unstable
}
