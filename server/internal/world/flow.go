package world

// NextWater returns the self-only next block from exactly six neighbours.
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
func NextWater(here Block, above, below Block, sides [4]Block) Block {
	if waterSource(here) || Solid(here) || Cover(here) {
		return here
	}

	if IsWater(above) {
		return WaterFlow7
	}

	maxSide := 0
	for _, side := range sides {
		maxSide = max(maxSide, WaterLevel(side))
	}

	// **A cell over a void that a side can still feed is the head of a fall.** Until
	// #653 a drain arm stood here and was asked first, so a cell with nothing under it
	// could never take water from a neighbour — and the only other way to make water
	// over a void is to have water directly above, which by the same argument could
	// never have got there. **No water in this world could begin to fall.** A pool on a
	// ledge was a fixed point: measured, it settled in one step with zero changes, and
	// not one voxel of the WaterFlow family existed anywhere in generated terrain.
	//
	// What is left of that arm is its unfed half, which was always right: water with
	// nothing under it and nothing beside it to supply it drains.
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
