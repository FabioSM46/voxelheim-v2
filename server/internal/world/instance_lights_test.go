package world

import (
	"math"
	"testing"
)

// The dungeon's lights are wall sconces, placed with the drawing at every rotation:
// each in a clear cell with a wall directly behind it, facing away from the wall. The
// cave keeps no more than three — it is meant to be dark — and one of them lights the
// grille's lever; the sand hall has its own four.
func TestTheDungeonsSconcesHangOnItsWallsAndLeaveTheCaveDark(t *testing.T) {
	for _, seed := range dungeonTestSeeds {
		w := newDungeonWorld(t, seed, true, false)
		slots := lowerSlotsOf(t, seed)
		props, err := InstanceStaticProps(seed)
		if err != nil {
			t.Fatal(err)
		}
		if len(props) != len(dungeonStaticProps) {
			t.Fatalf("seed %d: %d props placed of %d", seed, len(props), len(dungeonStaticProps))
		}
		ids := map[uint64]bool{}
		cave, sand := 0, 0
		lit := false
		for _, p := range props {
			x, y, z := p.Origin[0], p.Origin[1], p.Origin[2]
			if p.Kind != PropWallSconce || ids[p.ID] || p.ID == 0 {
				t.Fatalf("seed %d: prop %+v is not a sconce with its own id", seed, p)
			}
			ids[p.ID] = true
			// Facing 1 is north (-Z), 2 east (+X), 3 south (+Z), 4 west (-X); the wall is
			// the other way.
			back := [5][2]int64{{}, {0, 1}, {-1, 0}, {0, -1}, {1, 0}}[p.Facing]
			if w.at(x, y, z) != Air || !Solid(w.at(x+back[0], y, z+back[1])) || !w.standable(x, y-1, z) {
				t.Fatalf("seed %d: sconce %+v does not hang on a wall over clear floor", seed, p)
			}
			switch y - 1 {
			case slots.checkpoints[0].Y:
				cave++
				lever := slots.grilleLever
				if math.Hypot(float64(x-lever.X), float64(z-lever.Z)) <= 6 {
					lit = true
				}
			case slots.checkpoints[2].Y:
				sand++
			}
		}
		if cave != 3 || sand != 4 || !lit {
			t.Fatalf("seed %d: %d sconces in the cave and %d in the sand hall; the lever lit: %v", seed, cave, sand, lit)
		}
	}
}
