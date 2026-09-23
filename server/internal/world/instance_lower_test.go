package world

import (
	"context"
	"errors"
	"testing"
)

// The dungeon below the chasm, read through the generated instance at every rotation:
// the cave, the sand hall, their puzzles and the king's arena at the bottom.

// lowerSlots is one seed's slots below the chasm, sorted by what they are for.
type lowerSlots struct {
	checkpoints      []PlacedAnchor // by index: the shore, past the grille, past the door
	burrows, buried  []PlacedAnchor
	grilleLever      PlacedAnchor
	twinLevers       []PlacedAnchor
	grille, sandDoor []PlacedAnchor
	triggers         map[int][]PlacedAnchor
	king             PlacedAnchor
}

func lowerSlotsOf(t *testing.T, seed int64) lowerSlots {
	t.Helper()
	out := lowerSlots{triggers: map[int][]PlacedAnchor{}}
	for _, a := range InstanceDungeonAnchors(seed) {
		switch {
		case a.Kind == AnchorInstanceCheckpoint:
			if a.Index != len(out.checkpoints) {
				t.Fatalf("seed %d: checkpoint %+v is out of order", seed, a)
			}
			out.checkpoints = append(out.checkpoints, a)
		case a.Kind == AnchorInstanceMinorSpawn && a.Index == CaveBurrowGroup:
			out.burrows = append(out.burrows, a)
		case a.Kind == AnchorInstanceMinorSpawn && a.Index == SandBuriedGroup:
			out.buried = append(out.buried, a)
		case a.Kind == AnchorInstanceMechanism && a.Index == GrillePuzzle:
			out.grilleLever = a
		case a.Kind == AnchorInstanceMechanism && a.Index == TwinLeverPuzzle:
			out.twinLevers = append(out.twinLevers, a)
		case a.Kind == AnchorInstanceDoor && a.Index == GrillePuzzle:
			out.grille = append(out.grille, a)
		case a.Kind == AnchorInstanceDoor && a.Index == TwinLeverPuzzle:
			out.sandDoor = append(out.sandDoor, a)
		case a.Kind == AnchorInstanceTrigger:
			out.triggers[a.Index] = append(out.triggers[a.Index], a)
		case a.Kind == AnchorInstanceKing:
			out.king = a
		}
	}
	if len(out.checkpoints) != 3 || len(out.burrows) != 6 || len(out.buried) != 8 || len(out.twinLevers) != 2 ||
		len(out.grille) != 9 || len(out.sandDoor) != 9 || len(out.triggers) != 2 {
		t.Fatalf("seed %d: lower slots %+v", seed, out)
	}
	return out
}

// inBox reports whether a slot lies in the volume two trigger corners span.
func inBox(p PlacedAnchor, corners []PlacedAnchor) bool {
	a, b := corners[0], corners[1]
	return p.X >= min(a.X, b.X) && p.X <= max(a.X, b.X) && p.Y >= min(a.Y, b.Y) && p.Y <= max(a.Y, b.Y) &&
		p.Z >= min(a.Z, b.Z) && p.Z <= max(a.Z, b.Z)
}

// standBeside is the standing cell a lever is pulled from: the one clear floor cell
// beside it, a course below the lever.
func (w *dungeonWorld) standBeside(lever PlacedAnchor) PlacedAnchor {
	w.t.Helper()
	var found []PlacedAnchor
	for _, d := range [][2]int64{{1, 0}, {-1, 0}, {0, 1}, {0, -1}} {
		if w.standable(lever.X+d[0], lever.Y-1, lever.Z+d[1]) {
			found = append(found, PlacedAnchor{X: lever.X + d[0], Y: lever.Y - 1, Z: lever.Z + d[1]})
		}
	}
	if len(found) != 1 {
		w.t.Fatalf("lever %+v has %d standing cells beside it, want one", lever, len(found))
	}
	return found[0]
}

func cell(a PlacedAnchor) [3]int64 { return [3]int64{a.X, a.Y, a.Z} }

// Every slot below the chasm stands where its kind says, at every rotation: the three
// checkpoints on clear floor, each burrow a hole one wide and two tall in rock, each
// scorpion's slot over sand deep enough to bury the whole body, the three levers in the
// walls with room to pull them, and the doors drawn as grilles turned with the drawing.
func TestTheLowerSlotsStandWhereTheirKindSays(t *testing.T) {
	rock := map[Block]bool{Stone: true, Basalt: true, Gravel: true}
	for _, seed := range dungeonTestSeeds {
		w := newDungeonWorld(t, seed, true, false)
		slots := lowerSlotsOf(t, seed)
		shore := slots.checkpoints[0]
		for _, c := range slots.checkpoints {
			if !w.standable(c.X, c.Y, c.Z) {
				t.Fatalf("seed %d: checkpoint %+v has no floor or headroom", seed, c)
			}
		}
		if slots.checkpoints[1].Y != shore.Y || slots.checkpoints[2].Y >= shore.Y || slots.king.Y >= slots.checkpoints[2].Y {
			t.Fatalf("seed %d: the dungeon does not go down from the shore to the king: %+v, king %+v", seed, slots.checkpoints, slots.king)
		}

		for _, b := range slots.burrows {
			if b.Y != shore.Y || !w.standable(b.X, b.Y, b.Z) || !Solid(w.at(b.X, b.Y+2, b.Z)) {
				t.Fatalf("seed %d: burrow %+v is not a hole two tall", seed, b)
			}
			// Rock either side across the hole's axis and behind it: of the four sides,
			// exactly one opens, the way into the cave.
			open := 0
			for _, d := range [][2]int64{{1, 0}, {-1, 0}, {0, 1}, {0, -1}} {
				side := w.at(b.X+d[0], b.Y, b.Z+d[1])
				if side == Air {
					open++
				} else if !rock[side] {
					t.Fatalf("seed %d: burrow %+v is walled with %d, not the cave's rock", seed, b, side)
				}
			}
			if open != 1 {
				t.Fatalf("seed %d: burrow %+v opens on %d sides", seed, b, open)
			}
			if inBox(b, slots.triggers[CaveTrigger]) {
				t.Fatalf("seed %d: burrow %+v is inside the cavern rather than in its wall", seed, b)
			}
		}

		for _, b := range slots.buried {
			if !w.standable(b.X, b.Y, b.Z) || !inBox(b, slots.triggers[SandTrigger]) {
				t.Fatalf("seed %d: scorpion slot %+v is not clear floor in the sand hall", seed, b)
			}
			// A scorpion is 1.3 wide, centred on the cell: sunk one course, it covers the
			// three by three under the slot, and every cell of it must be sand.
			for dx := int64(-1); dx <= 1; dx++ {
				for dz := int64(-1); dz <= 1; dz++ {
					if w.at(b.X+dx, b.Y-1, b.Z+dz) != Sand || !Solid(w.at(b.X+dx, b.Y-2, b.Z+dz)) {
						t.Fatalf("seed %d: scorpion slot %+v is not over a block of sand at %+d,%+d", seed, b, dx, dz)
					}
				}
			}
		}

		for _, lever := range append([]PlacedAnchor{slots.grilleLever}, slots.twinLevers...) {
			if w.at(lever.X, lever.Y, lever.Z) != LeverOff {
				t.Fatalf("seed %d: lever %+v holds %d", seed, lever, w.at(lever.X, lever.Y, lever.Z))
			}
			if err := w.cache.Apply(context.Background(), lever.X, lever.Y, lever.Z, Air, nil); !errors.Is(err, ErrImmutableShell) {
				t.Fatalf("seed %d: a lever was edited: %v", seed, err)
			}
			w.standBeside(lever)
		}
		grille := IronGrilleX
		if uint64(seed)&1 != 0 {
			grille = IronGrilleZ
		}
		for _, d := range append(append([]PlacedAnchor(nil), slots.grille...), slots.sandDoor...) {
			if w.at(d.X, d.Y, d.Z) != grille {
				t.Fatalf("seed %d: door cell %+v holds %d, want grille %d", seed, d, w.at(d.X, d.Y, d.Z), grille)
			}
		}
		for zone, corners := range slots.triggers {
			for _, c := range corners {
				if w.at(c.X, c.Y, c.Z) != Air {
					t.Fatalf("seed %d: trigger %d corner %+v is not air", seed, zone, c)
				}
			}
		}
	}
}

// The cave is rough, low and webbed: every floor and ceiling of its cavern is stone,
// basalt or gravel (its burrows' walls are checked above), its ceilings stand at several heights and never lower
// than three clear courses, webs hang in it, and the neck into the gallery is webbed
// shut across its whole width and height.
func TestTheCaveIsRoughRockUnderLowWebbedCeilings(t *testing.T) {
	rock := map[Block]bool{Stone: true, Basalt: true, Gravel: true}
	for _, seed := range dungeonTestSeeds {
		w := newDungeonWorld(t, seed, true, false)
		slots := lowerSlotsOf(t, seed)
		shore, cavern := slots.checkpoints[0], slots.triggers[CaveTrigger]
		heights := map[int64]int{}
		webs := 0
		lo, hi := cavern[0], cavern[1]
		for x := min(lo.X, hi.X); x <= max(lo.X, hi.X); x++ {
			for z := min(lo.Z, hi.Z); z <= max(lo.Z, hi.Z); z++ {
				if Solid(w.at(x, shore.Y, z)) {
					continue // a rock column
				}
				clear := int64(0)
				for ; !Solid(w.at(x, shore.Y+clear, z)); clear++ {
					if w.at(x, shore.Y+clear, z) == Cobweb {
						webs++
					}
				}
				heights[clear]++
				if clear < 3 {
					t.Fatalf("seed %d: the cave's ceiling at %d,%d is %d clear courses", seed, x, z, clear)
				}
				for _, b := range []Block{w.at(x, shore.Y-1, z), w.at(x, shore.Y+clear, z)} {
					if !rock[b] {
						t.Fatalf("seed %d: the cave at %d,%d is floored or roofed with %d", seed, x, z, b)
					}
				}
			}
		}
		if len(heights) < 3 || webs < 40 {
			t.Fatalf("seed %d: the cavern has ceiling heights %v and %d webs", seed, heights, webs)
		}

		// The curtain: the neck is the one way out of the cavern towards the grille, and
		// each of its cells on the curtain's line is a web, floor to ceiling.
		b := instancePlacement(seed)
		curtain := 0
		for x := caveNeckX0; x <= caveNeckX1; x++ {
			for y := dungeonShore; y <= dungeonShore+2; y++ {
				rx, rz := rotateCell(x, caveCurtainZ, dungeonWidth, dungeonDepth, b.Facing)
				if w.at(b.OriginX+int64(rx), b.OriginY+int64(y), b.OriginZ+int64(rz)) != Cobweb {
					t.Fatalf("seed %d: the curtain is open at %d,%d", seed, x, y)
				}
				curtain++
			}
		}
		if curtain != 15 {
			t.Fatalf("seed %d: the curtain is %d cells", seed, curtain)
		}
	}
}

// The sand hall has a sand floor with dunes on it and sandstone walls and pillars.
func TestTheSandHallIsSandUnderSandstone(t *testing.T) {
	for _, seed := range dungeonTestSeeds {
		w := newDungeonWorld(t, seed, true, false)
		slots := lowerSlotsOf(t, seed)
		hall := slots.triggers[SandTrigger]
		lo, hi := hall[0], hall[1]
		floor := min(lo.Y, hi.Y)
		levels := map[int64]int{}
		pillars := 0
		for x := min(lo.X, hi.X); x <= max(lo.X, hi.X); x++ {
			for z := min(lo.Z, hi.Z); z <= max(lo.Z, hi.Z); z++ {
				if w.at(x, floor, z) == Sandstone {
					pillars++
					continue
				}
				y := floor
				for w.at(x, y, z) == Sand {
					y++
				}
				if w.at(x, y-1, z) != Sand || !w.standable(x, y, z) {
					t.Fatalf("seed %d: the sand hall at %d,%d does not stand on sand", seed, x, z)
				}
				levels[y-floor]++
			}
			for _, z := range []int64{min(lo.Z, hi.Z) - 1, max(lo.Z, hi.Z) + 1} {
				if b := w.at(x, floor+5, z); b != Sandstone {
					t.Fatalf("seed %d: the sand hall's wall at %d,%d is %d", seed, x, z, b)
				}
			}
		}
		if pillars != 16 || levels[0] == 0 || levels[1] == 0 || levels[2] == 0 {
			t.Fatalf("seed %d: %d pillar columns and dune levels %v", seed, pillars, levels)
		}
	}
}

// Every route from the shore reaches the king, and each puzzle is the only way on:
// shut the grille and neither the sand hall nor anything past it is reached; shut the
// sand hall's door and the king is not. With both open, every slot below the chasm is
// reached on foot from the shore, and the king from every checkpoint.
func TestTheShoreReachesTheKingOnlyThroughBothPuzzles(t *testing.T) {
	for _, seed := range dungeonTestSeeds {
		slots := lowerSlotsOf(t, seed)
		shore := slots.checkpoints[0]

		open := newDungeonWorld(t, seed, true, true)
		reached := open.walk(shore)
		targets := append(append(append([]PlacedAnchor{slots.king}, slots.checkpoints...), slots.burrows...), slots.buried...)
		for _, lever := range append([]PlacedAnchor{slots.grilleLever}, slots.twinLevers...) {
			targets = append(targets, open.standBeside(lever))
		}
		for _, a := range targets {
			if !reached[cell(a)] {
				t.Fatalf("seed %d: %+v is not reached from the shore", seed, a)
			}
		}
		for _, c := range slots.checkpoints[1:] {
			if !open.walk(c)[cell(slots.king)] {
				t.Fatalf("seed %d: the king is not reached from checkpoint %+v", seed, c)
			}
		}

		grilleShut := newDungeonWorld(t, seed, true, true)
		grilleShut.shut[GrillePuzzle] = true
		reached = grilleShut.walk(shore)
		if !reached[cell(grilleShut.standBeside(slots.grilleLever))] {
			t.Fatalf("seed %d: the grille's lever is behind the grille", seed)
		}
		for _, a := range append([]PlacedAnchor{slots.checkpoints[1], slots.checkpoints[2], slots.king}, slots.buried...) {
			if reached[cell(a)] {
				t.Fatalf("seed %d: %+v is reached with the grille shut", seed, a)
			}
		}

		doorShut := newDungeonWorld(t, seed, true, true)
		doorShut.shut[TwinLeverPuzzle] = true
		reached = doorShut.walk(shore)
		for _, lever := range slots.twinLevers {
			if !reached[cell(doorShut.standBeside(lever))] {
				t.Fatalf("seed %d: a twin lever is behind the door it opens", seed)
			}
		}
		if reached[cell(slots.checkpoints[2])] || reached[cell(slots.king)] {
			t.Fatalf("seed %d: the king is reached with the sand hall's door shut", seed)
		}
	}
}
