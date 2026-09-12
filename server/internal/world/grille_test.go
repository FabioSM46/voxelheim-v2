package world

import (
	"math"
	"testing"
)

func TestGrilleBarsLeaveNarrowOpenGaps(t *testing.T) {
	for _, block := range []Block{IronGrilleX, IronGrilleZ} {
		boxes, n := CollisionBounds(block)
		if n != 2 || !Solid(block) || ShapeOf(block).Kind != ShapeGrille {
			t.Fatalf("grille %d: %v/%d", block, boxes, n)
		}
		widthAxis, depthAxis := 0, 2
		if block == IronGrilleZ {
			widthAxis, depthAxis = 2, 0
		}
		for i, box := range boxes {
			if math.Abs(box.Min[widthAxis]-(0.2+float64(i)*0.5)) > 1e-12 || math.Abs(box.Max[widthAxis]-box.Min[widthAxis]-0.1) > 1e-12 {
				t.Errorf("bar width: %v", box)
			}
			if box.Min[1] != 0 || box.Max[1] != 1 || box.Min[depthAxis] != 0.45 || box.Max[depthAxis] != 0.55 {
				t.Errorf("bar extent: %v", box)
			}
		}
		if gap := boxes[1].Min[widthAxis] - boxes[0].Max[widthAxis]; math.Abs(gap-0.4) > 1e-12 {
			t.Errorf("gap = %g", gap)
		}
	}
}

func TestGrilleOrientationFollowsSchematicRotation(t *testing.T) {
	for _, b := range []Block{IronGrilleX, IronGrilleZ} {
		for facing := FacingPlusZ; facing <= FacingPlusX; facing++ {
			want := b
			if facing%2 == 1 {
				if b == IronGrilleX {
					want = IronGrilleZ
				} else {
					want = IronGrilleX
				}
			}
			if got := rotateSchematicBlock(b, facing); got != want {
				t.Errorf("block %d facing %d: got %d want %d", b, facing, got, want)
			}
		}
	}
}
