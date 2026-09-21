package world

import "testing"

func TestCastleWindowsAreFullDepthBarredOpeningsWithIntactRoofs(t *testing.T) {
	s := SchematicFor(BuildingKeep)
	for _, wing := range []struct {
		floors  int
		walls   [2][2]int
		grilles [2]int
	}{{4, [2][2]int{{4, 5}, {25, 26}}, [2]int{4, 26}}, {5, [2][2]int{{36, 37}, {57, 58}}, [2]int{36, 58}}} {
		for floor := range wing.floors {
			y0 := floor * 7
			for side, wall := range wing.walls {
				for x := wall[0]; x <= wall[1]; x++ {
					for z := 20; z <= 22; z++ {
						for y := y0 + 1; y <= y0+5; y++ {
							want := Air
							if x == wing.grilles[side] {
								want = IronGrilleZ
							}
							if got := s.At(x, y, z); got != want {
								t.Fatalf("aperture %d,%d,%d: %v want %v", x, y, z, got, want)
							}
						}
						if !Solid(s.At(x, y0, z)) || !Solid(s.At(x, y0+6, z)) {
							t.Fatal("window lost sill/header")
						}
					}
				}
			}
			// Shelter uses the roof column above an interior point, independently of windows.
			x := 12
			if wing.floors == 5 {
				x = 48
			}
			if !Solid(s.At(x, y0+6, 21)) {
				t.Fatalf("interior %d,%d lost ceiling shelter", x, y0)
			}
		}
	}
	for _, w := range []struct{ x, y, z0, z1 int }{{10, 35, 16, 18}, {20, 29, 36, 37}, {50, 41, 16, 19}, {42, 35, 36, 38}} {
		for x := w.x - 1; x <= w.x+1; x++ {
			for y := w.y + 1; y <= w.y+2; y++ {
				for z := w.z0; z <= w.z1; z++ {
					want := Air
					if z == w.z1 {
						want = IronGrilleX
					}
					if s.At(x, y, z) != want {
						t.Fatalf("tower aperture differs at %d,%d,%d", x, y, z)
					}
				}
			}
		}
	}
	for facing := North; facing <= West; facing++ {
		want := IronGrilleZ
		if facing%2 == 1 {
			want = IronGrilleX
		}
		if rotateSchematicBlock(IronGrilleZ, facing) != want {
			t.Fatal("window grille does not rotate")
		}
	}
}
