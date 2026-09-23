package world

import "testing"

// walk floods every standing cell reachable on foot — a step up of one course, any
// step down — from one standing cell.
func (w *dungeonWorld) walk(from PlacedAnchor) map[[3]int64]bool {
	start := [3]int64{from.X, from.Y, from.Z}
	reached := map[[3]int64]bool{start: true}
	queue := [][3]int64{start}
	for len(queue) > 0 {
		c := queue[0]
		queue = queue[1:]
		for _, step := range [][2]int64{{1, 0}, {-1, 0}, {0, 1}, {0, -1}} {
			for _, dy := range []int64{1, 0, -1} {
				next := [3]int64{c[0] + step[0], c[1] + dy, c[2] + step[1]}
				if reached[next] || !w.standable(next[0], next[1], next[2]) {
					continue
				}
				if dy == 1 && Solid(w.at(c[0], c[1]+2, c[2])) {
					continue
				}
				reached[next] = true
				queue = append(queue, next)
			}
		}
	}
	return reached
}

// The chasm is at least thirty courses of enclosed shaft whose walls are whole on
// every course — no cell inside the column a body could stand on, and no niche in its
// walls — ending in still water four deep over a bed. The column refuses every edit,
// so no party builds the ledge the drawing does not have.
func TestTheChasmIsASmoothShaftIntoDeepStillWater(t *testing.T) {
	for _, seed := range dungeonTestSeeds {
		w := newDungeonWorld(t, seed, true, false)
		_, _, gate := InstanceEncounterAnchors(seed)
		surface := dungeonAnchorsByKind(seed)[AnchorInstanceCheckpoint][0].Y // level with the water's top
		if gate.Y-surface < 30 {
			t.Fatalf("seed %d: the drop from the trapdoor to the water is %d", seed, gate.Y-surface)
		}
		shaft := 0
		for y := gate.Y; y >= surface; y-- {
			// The column is the trapdoor's five by five; its walls are the ring one out,
			// enclosed down to the pool chamber's ceiling three courses above the water.
			enclosed := y > surface+2
			for dx := int64(-3); dx <= 3; dx++ {
				for dz := int64(-3); dz <= 3; dz++ {
					inside := dx >= -2 && dx <= 2 && dz >= -2 && dz <= 2
					block := w.at(gate.X+dx, y, gate.Z+dz)
					if inside && block != Air {
						t.Fatalf("seed %d: the chasm holds %d at %+d,%d,%+d", seed, block, dx, y, dz)
					}
					if !inside && enclosed && !Solid(block) {
						t.Fatalf("seed %d: the chasm wall has an opening at %+d,%d,%+d", seed, dx, y, dz)
					}
				}
			}
			if enclosed {
				shaft++
			}
		}
		if shaft < 30 {
			t.Fatalf("seed %d: the enclosed shaft is %d courses", seed, shaft)
		}
		for dx := int64(-2); dx <= 2; dx++ {
			for dz := int64(-2); dz <= 2; dz++ {
				for y := surface - 4; y < surface; y++ {
					if w.at(gate.X+dx, y, gate.Z+dz) != Water {
						t.Fatalf("seed %d: the pool under the chasm is not four deep at %+d,%+d", seed, dx, dz)
					}
				}
				if !Solid(w.at(gate.X+dx, surface-5, gate.Z+dz)) {
					t.Fatalf("seed %d: the pool has no bed", seed)
				}
				for y := surface; y < gate.Y; y++ {
					if w.cache.editable(gate.X+dx, y, gate.Z+dz) {
						t.Fatalf("seed %d: the chasm column accepts an edit at %+d,%d,%+d", seed, dx, y, dz)
					}
				}
			}
		}
	}
}

// With puzzle 1's door open, every slot on floor 1 and the rim of the open chasm are
// reached on foot from the arrival slot, and nothing below is. From the shore, the
// tunnel reaches the king, and nothing on floor 1 is reachable: the drop is one way.
// Shut, the door keeps the arena out of reach while leaving the exit in the court.
func TestFloorOneLeadsToTheChasmAndTheShoreNeverLeadsBack(t *testing.T) {
	for _, seed := range dungeonTestSeeds {
		byKind := dungeonAnchorsByKind(seed)
		arrival, exit := InstanceAnchors(seed)
		guardian, king, gate := InstanceEncounterAnchors(seed)
		checkpoint := byKind[AnchorInstanceCheckpoint][0]
		rims := func(reached map[[3]int64]bool) int {
			n := 0
			for _, d := range [][2]int64{{3, 0}, {-3, 0}, {0, 3}, {0, -3}} {
				if reached[[3]int64{gate.X + d[0], gate.Y + 1, gate.Z + d[1]}] {
					n++
				}
			}
			return n
		}

		open := newDungeonWorld(t, seed, true, true)
		upper := open.walk(arrival)
		for _, a := range append([]PlacedAnchor{exit, guardian}, byKind[AnchorInstanceMinorSpawn]...) {
			if !upper[[3]int64{a.X, a.Y, a.Z}] {
				t.Fatalf("seed %d: %s %+v is not reached from the arrival slot", seed, a.Kind, a)
			}
		}
		if rims(upper) == 0 {
			t.Fatalf("seed %d: no rim of the open chasm is reached on foot", seed)
		}
		for p := range upper {
			if p[1] < arrival.Y {
				t.Fatalf("seed %d: floor 1 walks down to %v without falling", seed, p)
			}
		}
		lower := open.walk(checkpoint)
		if !lower[[3]int64{king.X, king.Y, king.Z}] {
			t.Fatalf("seed %d: the king is not reached from the shore", seed)
		}
		for p := range lower {
			if p[1] >= arrival.Y-1 {
				t.Fatalf("seed %d: the shore leads back up to %v", seed, p)
			}
		}

		shut := newDungeonWorld(t, seed, true, false).walk(arrival)
		if !shut[[3]int64{exit.X, exit.Y, exit.Z}] || shut[[3]int64{guardian.X, guardian.Y, guardian.Z}] || rims(shut) != 0 {
			t.Fatalf("seed %d: the shut rune door does not seal the arena from the court", seed)
		}
	}
}
