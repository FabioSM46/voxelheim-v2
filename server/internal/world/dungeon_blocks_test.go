package world

import "testing"

// The dungeon's four ids are appended after the grille and nothing was inserted.
func TestTheDungeonBlocksAreAppendedAfterTheGrille(t *testing.T) {
	for want, block := range map[Block]Block{60: Cobweb, 61: LeverOff, 62: LeverOn, 63: RuneStoneLit} {
		if block != want {
			t.Errorf("dungeon block has id %d, want %d", block, want)
		}
	}
	if IronGrilleZ != 59 || RuneStone != 55 {
		t.Fatal("an existing id moved")
	}
}

// blockClass is every palette predicate's answer for one id, so a new id has to state
// all of them at once rather than inheriting an answer by omission.
type blockClass struct {
	solid, cover, fluid, portal, immutable, placeable, snares bool
	shape                                                     ShapeKind
}

func classOf(b Block) blockClass {
	return blockClass{
		solid: Solid(b), cover: Cover(b), fluid: Fluid(b), portal: Portal(b),
		immutable: ImmutablePortal(b), placeable: Placeable(b), snares: Snares(b),
		shape: ShapeOf(b).Kind,
	}
}

// Every id in the palette, pinned: the exhaustive switches (solidity, cover, fluid,
// portal, placement, snaring and shape) answer each id exactly as stated here. The
// four dungeon ids are the new rows; the rest pin that appending them moved nothing.
func TestEveryBlockIdIsClassifiedExhaustively(t *testing.T) {
	cube := blockClass{solid: true}
	building := blockClass{solid: true, placeable: true}
	cover := blockClass{cover: true}
	water := blockClass{fluid: true}
	stair := blockClass{solid: true, shape: ShapeStair}
	slab := blockClass{solid: true, shape: ShapeSlab}
	grille := blockClass{solid: true, shape: ShapeGrille}

	want := map[Block]blockClass{
		Air: {}, Stone: building, Dirt: building, Grass: building, Snow: building,
		Log: building, Leaves: building, CoalOre: cube, IronOre: cube,
		Sand: building, Sandstone: building, Gravel: building, Water: water, Ice: building,
		Planks: building, Cobblestone: building, Thatch: building,
		PalmLog: building, PalmFronds: building, DesertShrub: cover,
		BroadLeaves: building, Bush: cover,
		WaterFlow1: water, WaterFlow2: water, WaterFlow3: water, WaterFlow4: water,
		WaterFlow5: water, WaterFlow6: water, WaterFlow7: water,
		WaterCurrentXPos: water, WaterCurrentXNeg: water, WaterCurrentZPos: water, WaterCurrentZNeg: water,
		FlowerRed: cover, FlowerYellow: cover, FlowerBlue: cover,
		SmoothBlackStone: cube, Basalt: cube, BlackBrick: cube, BlackBrickWorn: cube,
		SlateTile: cube, DarkTimber: cube, PaleTimber: cube, DarkGlass: cube,
		SlateSlabBottom: slab, SlateSlabTop: slab,
		SlateStairNorthBottom: stair, SlateStairEastBottom: stair, SlateStairSouthBottom: stair, SlateStairWestBottom: stair,
		SlateStairNorthTop: stair, SlateStairEastTop: stair, SlateStairSouthTop: stair, SlateStairWestTop: stair,
		WinterBramble: cover,
		RuneStone:     {solid: true, immutable: true},
		PortalVeil:    {portal: true, immutable: true},
		PortalHeart:   {portal: true, immutable: true},
		IronGrilleX:   grille, IronGrilleZ: grille,

		Cobweb:       {cover: true, snares: true},
		LeverOff:     cube,
		LeverOn:      cube,
		RuneStoneLit: {solid: true, immutable: true},
	}
	if len(want) != int(RuneStoneLit)+1 {
		t.Fatalf("the table names %d ids, the palette has %d", len(want), RuneStoneLit+1)
	}
	for block := Block(0); block <= RuneStoneLit; block++ {
		expected, ok := want[block]
		if !ok {
			t.Errorf("block %d is not classified by this table", block)
			continue
		}
		if got := classOf(block); got != expected {
			t.Errorf("block %d is %+v, want %+v", block, got, expected)
		}
	}
	// An id newer than this build fails closed: a solid cube and nothing else.
	if got := classOf(RuneStoneLit + 1); got != cube {
		t.Errorf("unknown id is %+v, want %+v", got, cube)
	}
}

// The open world never generates a dungeon block: no schematic legend can write one
// and a sample of generated terrain, surface through caves, holds none.
func TestTheOpenWorldNeverGeneratesADungeonBlock(t *testing.T) {
	dungeon := map[Block]bool{Cobweb: true, LeverOff: true, LeverOn: true, RuneStoneLit: true}
	for r, block := range schematicLegend {
		if dungeon[block] {
			t.Errorf("legend rune %q writes dungeon block %d", r, block)
		}
	}
	for cx := int32(-2); cx <= 2; cx++ {
		for cz := int32(-2); cz <= 2; cz++ {
			for cy := int32(-2); cy <= 4; cy++ {
				for i, block := range Generate(20260923, Coord{X: cx, Y: cy, Z: cz}).Blocks {
					if dungeon[block] {
						t.Fatalf("chunk %d,%d,%d voxel %d holds dungeon block %d", cx, cy, cz, i, block)
					}
				}
			}
		}
	}
}
