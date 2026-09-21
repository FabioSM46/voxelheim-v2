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
	for _, w := range []struct{ x, y, z0, z1 int }{{10, 35, 16, 18}, {20, 29, 36, 37}, {42, 35, 36, 38}} {
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
	// A slit through glass is insufficient if an opaque slate trim remains outside it.
	// Check the entire outward column, not only the declared wall-depth interval.
	for _, w := range []struct{ x, y, z int }{{10, 35, 18}, {20, 29, 37}, {42, 35, 38}} {
		for x := w.x - 1; x <= w.x+1; x++ {
			for y := w.y + 1; y <= w.y+2; y++ {
				for z := w.z + 1; z < s.D; z++ {
					b := s.At(x, y, z)
					if b != Air && b != keepTerrain {
						t.Fatalf("tower window has opaque exterior backing at %d,%d,%d", x, y, z)
					}
				}
			}
		}
	}
	// The NE south side faces the wing roof. Its working window faces north,
	// through the wall and trim while its overhead roof and old glass stay intact.
	for x := 49; x <= 51; x++ {
		for y := 42; y <= 43; y++ {
			for z := 0; z <= 8; z++ {
				want := Air
				if z == 5 {
					want = IronGrilleX
				}
				got := s.At(x, y, z)
				if got != want && (want != Air || got != keepTerrain) {
					t.Fatalf("NE north aperture blocked at %d,%d,%d", x, y, z)
				}
			}
		}
	}

	for _, room := range []struct{ x, floor, z int }{{10, 35, 12}, {20, 29, 32}, {50, 41, 12}, {42, 35, 32}} {
		if !Solid(s.At(room.x, room.floor+3, room.z)) {
			t.Fatalf("tower %d lost its interior roof column", room.x)
		}
	}

	for facing := Facing(0); facing < 4; facing++ {
		want := IronGrilleZ
		if facing%2 == 1 {
			want = IronGrilleX
		}
		if rotateSchematicBlock(IronGrilleZ, facing) != want {
			t.Fatal("window grille does not rotate")
		}
	}
}

// These two-high, barred reveals are window sills, not rooms. The rectangular
// drawing boundary is not the facade of an inset turret. Keep this exemption
// exact and require the actual grille; the aperture test above independently
// verifies every cell and the clear outward ray.
func castleTowerWindowReveal(s *Schematic, x, y, z int) bool {
	for _, w := range []struct{ cx, y, z0, z1, grille int }{
		{10, 36, 19, 19, 18}, {20, 30, 38, 38, 37}, {42, 36, 39, 39, 38}, {50, 42, 4, 8, 5},
	} {
		if y == w.y && x >= w.cx-1 && x <= w.cx+1 && z >= w.z0 && z <= w.z1 &&
			s.At(x, y, w.grille) == IronGrilleX && s.At(x, y+1, w.grille) == IronGrilleX {
			return true
		}
	}
	return false
}
