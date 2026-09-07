package world

import (
	"crypto/sha256"
	"encoding/binary"
	"fmt"
	"testing"
)

// Golden digests cover every cell of the placed volume, including omitted terrain
// and excavated air. They are paired with independent landmark and geometry tests:
// a stable digest alone cannot tell whether a drawing has a usable staircase.
func TestRuinVariantsAtEveryFacing(t *testing.T) {
	t.Parallel()
	golden := [2][4]string{
		{"8b3da43205b5e283b57004ddfb71ce33bca56049832319b304cb743582030f30", "f69ec1590707d47d6d8e981c23ae3715a9cd1532d73ea0775a7ac3bc7d2db46d", "bd73224cb44d8239fef32006ad6dcc65eb3ce31a160af0e6761a9266f82bee02", "abe81fa18c5a5643c0e7170131ca6f0bbf6a0479626f3d0dc1fdf078435fb074"},
		{"b36b2a99ccee11fed858e9307cffd02eb3e0ade098ba07b897200c5584e305dc", "c19a712701f8cbb2b4e697c2f9ce428d4008544e63d16a4f70d325c59b7c56a6", "bd53e29edb058787a2f752dc1a5bf90ee39b3bcb7048e276f3fddb02be30db05", "74c1fb2d040f38332e6588894ea3d17e505efaf312fa1d8e4c0abd5de63412ac"},
	}
	// Plot (100,-200), hall standing level 70. Expected positions are written in
	// world coordinates, independent of rotateCell and the anchor literals.
	arch := [4][3]int64{{100, 65, -204}, {104, 65, -200}, {100, 65, -196}, {96, 65, -200}}
	stair := [4][3]int64{{100, 70, -193}, {93, 70, -200}, {100, 70, -207}, {107, 70, -200}}
	for variant := uint8(0); variant < RuinVariantCount; variant++ {
		for facing := FacingPlusZ; facing <= FacingPlusX; facing++ {
			t.Run(fmt.Sprintf("variant-%d/facing-%d", variant, facing), func(t *testing.T) {
				b := centredRuinBuilding(variant, 100, -200, 70, facing)
				w, d := 15, 19
				if facing == FacingMinusX || facing == FacingPlusX {
					w, d = 19, 15
				}
				if b.Kind != BuildingRuin || b.Variant != variant || b.OriginY != 63 || b.OriginX != 100-int64(w/2) || b.OriginZ != -200-int64(d/2) {
					t.Fatalf("unexpected placement: %+v", b)
				}
				volume := make([]Block, w*d*12)
				for i := range volume {
					volume[i] = keepTerrain
				}
				seen := make(map[[3]int64]bool)
				visitSchematic(b, func(x, y, z int64, block Block) {
					key := [3]int64{x, y, z}
					if seen[key] {
						t.Fatalf("duplicate cell %v", key)
					}
					seen[key] = true
					lx, ly, lz := int(x-b.OriginX), int(y-b.OriginY), int(z-b.OriginZ)
					if lx < 0 || lx >= w || ly < 0 || ly >= 12 || lz < 0 || lz >= d {
						t.Fatalf("out of bounds cell %v", key)
					}
					volume[(ly*d+lz)*w+lx] = block
				})
				raw := make([]byte, len(volume)*2)
				for i, block := range volume {
					binary.LittleEndian.PutUint16(raw[2*i:], uint16(block))
				}
				if got := fmt.Sprintf("%x", sha256.Sum256(raw)); got != golden[variant][facing] {
					t.Fatalf("placed volume digest %s, want %s", got, golden[variant][facing])
				}
				if len(b.Anchors) != 2 {
					t.Fatalf("anchors: %+v", b.Anchors)
				}
				for i, want := range []struct {
					kind AnchorKind
					pos  [3]int64
				}{{AnchorRuinArch, arch[facing]}, {AnchorRuinStair, stair[facing]}} {
					a := b.Anchors[i]
					if a.Kind != want.kind || [3]int64{a.X, a.Y, a.Z} != want.pos {
						t.Errorf("anchor %+v, want %+v", a, want)
					}
				}
				// Probe the entire five-wide, three-high sealed opening after placement.
				// Along X for +/-Z facings, along Z for +/-X facings; all are full cubes.
				for across := -2; across <= 2; across++ {
					for y := 64; y <= 66; y++ {
						x, z := arch[facing][0], arch[facing][2]
						if facing == FacingPlusZ || facing == FacingMinusZ {
							x += int64(across)
						} else {
							z += int64(across)
						}
						block := volume[(int(int64(y)-b.OriginY)*d+int(z-b.OriginZ))*w+int(x-b.OriginX)]
						shoulder := y == 66 && (across == -2 || across == 2) && block == RuneStone
						if !Portal(block) && !shoulder {
							t.Errorf("arch opening at (%d,%d,%d) is %v", x, y, z, block)
						}
					}
				}
			})
		}
	}
}

func TestRuinChamberHasOnlyItsStairAndTheStairReturnsToTheHall(t *testing.T) {
	t.Parallel()
	for variant := uint8(0); variant < RuinVariantCount; variant++ {
		s := RuinSchematicFor(variant)
		reached := walkSchematic(s, [3]int{7, 7, 17})
		// Every floor cell on both sides of the arch is accessible, not another sealed
		// room. Its outer envelope is closed even if surrounding terrain is all Air.
		for z := 1; z <= 10; z++ {
			for x := 2; x <= 12; x++ {
				if !Solid(s.At(x, 0, z)) {
					t.Errorf("variant %d floor leak at %d,%d", variant, x, z)
				}
				if z != 10 || x < 6 || x > 8 {
					if !Solid(s.At(x, 5, z)) {
						t.Errorf("variant %d ceiling leak at %d,%d", variant, x, z)
					}
				}
				for y := 1; y <= 4; y++ {
					boundary := x == 2 || x == 12 || z == 1 || z == 10
					stair := z == 10 && x >= 6 && x <= 8
					if boundary && !stair && !Solid(s.At(x, y, z)) {
						t.Errorf("variant %d wall leak at %d,%d,%d", variant, x, y, z)
					}
				}
				if standableCell(s, x, 1, z) && !reached[[3]int{x, 1, z}] {
					t.Errorf("variant %d unreachable chamber floor at %d,%d", variant, x, z)
				}
			}
		}
		// A six-step descent with three-wide treads, two cells of headroom, and a
		// reversible path; falling into a cellar without a route out is insufficient.
		back := walkSchematic(s, [3]int{7, 1, 9})
		for z := 10; z <= 16; z++ {
			for x := 6; x <= 8; x++ {
				cell := [3]int{x, z - 9, z}
				if !standableCell(s, cell[0], cell[1], cell[2]) || !reached[cell] || !back[cell] {
					t.Errorf("variant %d broken stair at %v", variant, cell)
				}
			}
		}
		if !back[[3]int{7, 7, 17}] {
			t.Errorf("variant %d cannot leave chamber", variant)
		}
		// No ceiling over the open hall: above its broken masonry, every interior
		// column reaches explicit air at the top. Rubble occupies the floor below.
		rubble := 0
		for z := 2; z < 17; z++ {
			for x := 2; x < 13; x++ {
				if s.At(x, 11, z) != Air {
					t.Errorf("variant %d roof at %d,%d", variant, x, z)
				}
				if Solid(s.At(x, 7, z)) {
					rubble++
				}
			}
		}
		if rubble < 5 {
			t.Errorf("variant %d has only %d rubble cells", variant, rubble)
		}
		heights := make(map[int]bool)
		for z := 2; z < 17; z++ {
			height := 0
			for y := 7; y < 12; y++ {
				if Solid(s.At(1, y, z)) {
					height++
				}
			}
			heights[height] = true
		}
		if len(heights) < 4 {
			t.Errorf("variant %d wall has only %d distinct heights", variant, len(heights))
		}
		for _, block := range s.Voxels {
			switch block {
			case keepTerrain, Air, Basalt, BlackBrick, BlackBrickWorn, SmoothBlackStone, SlateTile, RuneStone, PortalVeil, PortalHeart:
			default:
				t.Fatalf("variant %d unexpected material %d", variant, block)
			}
		}
	}
}

func TestRuinDispatchAndAppendOnlyLandmarks(t *testing.T) {
	t.Parallel()
	if BuildingStable != 4 || BuildingRuin != 5 || AnchorPaddock != 10 || AnchorRuinArch != 11 || AnchorRuinStair != 12 {
		t.Fatal("existing kinds moved instead of appending ruin kinds")
	}
	if SchematicFor(BuildingRuin) != RuinSchematicFor(0) || RuinSchematicFor(0) == RuinSchematicFor(1) || RuinSchematicFor(255) != RuinSchematicFor(0) {
		t.Fatal("ruin variant dispatch")
	}
	if BuildingRuin.String() != "ruin" || AnchorRuinArch.String() != "ruin arch" || AnchorRuinStair.String() != "ruin stair" {
		t.Fatal("ruin diagnostic names")
	}
}

func TestOnlyPortalAnchorsMayNameAHeart(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		kind  AnchorKind
		cell  string
		valid bool
	}{
		{AnchorRuinArch, "O", true}, {AnchorRuinArch, "S", false}, {AnchorInstanceExit, "O", true}, {AnchorInstanceExit, "_", false}, {AnchorRuinArch, "_", false}, {AnchorRuinArch, ".", false}, {AnchorRuinArch, "G", false},
		{AnchorRuinStair, "S", false}, {AnchorRuinStair, "_", true}, {AnchorForge, "S", false},
	} {
		t.Run(fmt.Sprintf("%d/%s", tc.kind, tc.cell), func(t *testing.T) {
			defer func() {
				r := recover()
				if (r == nil) != tc.valid {
					t.Errorf("valid=%v, panic=%v", tc.valid, r)
				}
			}()
			mustSchematic([]Anchor{{Kind: tc.kind}}, []string{tc.cell})
		})
	}
}
