package world

// NextWater returns the self-only next block from exactly six neighbours.
func NextWater(here Block, above, below Block, sides [4]Block) Block {
	if waterSource(here) || Solid(here) {
		return here
	}

	if IsWater(above) {
		return WaterFlow7
	}

	// Unsupported flow drains; unsupported Air stays Air.
	if below == Air || flowingBelowSeven(below) {
		return Air
	}

	maxSide := 0
	for _, side := range sides {
		maxSide = max(maxSide, WaterLevel(side))
	}
	return waterFlow(maxSide - 1)
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
