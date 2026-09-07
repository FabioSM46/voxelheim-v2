package world

import (
	"context"
	"errors"
	"testing"
)

func TestPortalBlocksAreImmutableAndOldDeltasCannotEraseThem(t *testing.T) {
	for _, block := range []Block{RuneStone, PortalVeil, PortalHeart} {
		t.Run(string(rune('A'+block-RuneStone)), func(t *testing.T) {
			cache := NewCache(1, 1, 4, WithGenerator(func(_ int64, coord Coord) *Chunk {
				c := NewChunk(coord)
				c.Set(1, 1, 1, block)
				return c
			}))

			coord := Coord{}
			cache.deltas.Record(coord, Index(1, 1, 1), Air)
			c, _, err := cache.Get(context.Background(), coord)
			if err != nil {
				t.Fatal(err)
			}
			if c.At(1, 1, 1) != block {
				t.Fatal("persisted delta erased portal")
			}
			for _, replacement := range []Block{Air, Stone, Water} {
				if err := cache.Apply(context.Background(), 1, 1, 1, replacement, nil); !errors.Is(err, ErrImmutableShell) {
					t.Fatalf("edit: %v", err)
				}
			}
			if Placeable(block) || Cover(block) || Fluid(block) {
				t.Fatal("portal is not an item, replaceable cover or fluid")
			}
			if Solid(block) != (block == RuneStone) {
				t.Fatal("only the frame is solid")
			}
		})
	}
}

func TestEveryPortalHasOneHeartAndAnOpenProtectedDoorway(t *testing.T) {
	for _, s := range []*Schematic{brokenHallRuin, brokenGableRuin, instanceChamber} {
		hearts := 0
		for _, b := range s.Voxels {
			if b == PortalHeart {
				hearts++
			}
		}
		if hearts != 1 {
			t.Fatalf("hearts = %d", hearts)
		}
		for _, a := range s.Anchors {
			if a.Kind != AnchorRuinArch && a.Kind != AnchorInstanceExit {
				continue
			}
			floor := 1
			for y := floor; y < floor+2; y++ {
				for x := a.X - 2; x <= a.X+2; x++ {
					if !Portal(s.At(x, y, a.Z)) {
						t.Fatalf("doorway blocked at %d,%d,%d", x, y, a.Z)
					}
				}
			}
			for x := a.X - 3; x <= a.X+3; x++ {
				if s.At(x, 0, a.Z) != RuneStone || s.At(x, 4, a.Z) != RuneStone {
					t.Fatal("unprotected frame")
				}
			}
		}
	}
}
