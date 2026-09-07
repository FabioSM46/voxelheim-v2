package world

import (
	"context"
	"math"
	"testing"
)

func TestDungeonDrawingHasTwoArenasAndOneWalkableGallery(t *testing.T) {
	for _, seed := range []int64{0, 1, 2, 3, -1, math.MinInt64, math.MaxInt64} {
		cache := NewInstanceCache(seed, 1, 72)
		at := func(x, y, z int64) Block {
			c, _, err := cache.Get(context.Background(), ChunkOf(x, y, z))
			if err != nil {
				t.Fatal(err)
			}
			return c.At(Local(x), Local(y), Local(z))
		}
		arrival, exit := InstanceAnchors(seed)
		guardian, king, gate := InstanceEncounterAnchors(seed)
		// Compare distances, not the helper's rotation against another call to itself.
		distance := func(a, b PlacedAnchor) int64 { x, z := a.X-b.X, a.Z-b.Z; return x*x + z*z }
		if distance(guardian, king) != 38*38 || distance(guardian, gate) != 18*18 || distance(arrival, exit) != 6*6 {
			t.Fatal("anchors do not describe the two chambers")
		}
		for _, a := range []PlacedAnchor{arrival, exit, guardian, king, gate} {
			if a.Y != 1 || !Solid(at(a.X, 0, a.Z)) {
				t.Fatalf("unsupported anchor %+v", a)
			}
			for y := int64(1); y <= 3; y++ {
				if at(a.X, y, a.Z) != Air {
					t.Fatalf("king-height route obstructed at %+v", a)
				}
			}
		}
		// Flood every air voxel connected to arrival, including overhead space: a
		// missing lintel above a walkable opening must not escape an X/Z-only flood.
		type cell struct{ x, y, z int64 }
		start := cell{arrival.X, 1, arrival.Z}
		queue := []cell{start}
		seen := map[cell]bool{start: true}
		for len(queue) > 0 {
			p := queue[0]
			queue = queue[1:]
			if p.x <= -35 || p.x >= 35 || p.z <= -35 || p.z >= 35 || p.y <= 0 || p.y >= 9 {
				t.Fatalf("seed %d dungeon leaks at %+v", seed, p)
			}
			for _, d := range []cell{{1, 0, 0}, {-1, 0, 0}, {0, 1, 0}, {0, -1, 0}, {0, 0, 1}, {0, 0, -1}} {
				next := cell{p.x + d.x, p.y + d.y, p.z + d.z}
				if !seen[next] && at(next.x, next.y, next.z) == Air {
					seen[next] = true
					queue = append(queue, next)
				}
			}
		}
		for _, a := range []PlacedAnchor{exit, guardian, king, gate} {
			if !seen[cell{a.X, 1, a.Z}] {
				t.Fatalf("unreachable slot %+v", a)
			}
		}
		// Local floor footprints measure clear widths independently from the source
		// drawing dimensions. Rotation changes the axis, never the corridor width.
		dx, dz := int64(1), int64(0)
		if uint64(seed)&1 != 0 {
			dx, dz = 0, 1
		}
		for _, row := range []struct {
			a    PlacedAnchor
			half int64
		}{{guardian, 13}, {king, 15}, {gate, 2}} {
			for offset := -row.half; offset <= row.half; offset++ {
				x, z := row.a.X+offset*dx, row.a.Z+offset*dz
				if at(x, 1, z) != Air || !Solid(at(x, 0, z)) {
					t.Fatalf("seed %d clear width lost at %+v offset %d", seed, row.a, offset)
				}
			}
			if !Solid(at(row.a.X+(row.half+1)*dx, 1, row.a.Z+(row.half+1)*dz)) {
				t.Fatal("arena/gallery wall missing")
			}
		}
	}
}

func TestDungeonShellAndMonolithsRemainImmutableAtEveryRotation(t *testing.T) {
	for seed := int64(0); seed < 4; seed++ {
		c := NewInstanceCache(seed, 1, 72)
		for x := int64(-35); x <= 35; x++ {
			for z := int64(-35); z <= 35; z++ {
				for y := int64(0); y <= 9; y++ {
					lx, ly, lz, ok := instanceLocal(seed, x, y, z)
					if !ok {
						continue
					}
					if instanceChamber.At(lx, ly, lz) != Air && instanceInterior(seed, x, y, z) {
						t.Fatalf("scenery editable at %d,%d,%d", x, y, z)
					}
				}
			}
		}
		_, _, gate := InstanceEncounterAnchors(seed)
		if err := c.Apply(context.Background(), gate.X, gate.Y, gate.Z, Planks, nil); err != nil {
			t.Fatal(err)
		}
	}
}
