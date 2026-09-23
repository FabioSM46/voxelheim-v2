package world

import (
	"context"
	"errors"
	"math"
	"testing"
)

// dungeonWorld reads the generated dungeon of one seed through its real cache, with
// the gate open or shut and the doors optionally opened — the state a party that
// solved the puzzles and felled the king walks through, less any door named in shut.
// The doors are opened through the gate's own Update on the first read, so every read
// is of the chunks the cache really publishes. Nothing here consults the drawing.
type dungeonWorld struct {
	t        *testing.T
	cache    *Cache
	gate     *InstanceGate
	doorOpen bool
	doors    map[[3]int64]int // a door cell's index
	shut     map[int]bool
	opened   map[int]bool // what the first read opened, so a later shut is caught
}

func newDungeonWorld(t *testing.T, seed int64, gateOpen, doorOpen bool) *dungeonWorld {
	t.Helper()
	cache, gate := NewGatedInstanceCache(seed, 1, InstanceChunkEnvelope(seed), gateOpen)
	w := &dungeonWorld{t: t, cache: cache, gate: gate, doorOpen: doorOpen, doors: map[[3]int64]int{}, shut: map[int]bool{}}
	for _, a := range InstanceDungeonAnchors(seed) {
		if a.Kind == AnchorInstanceDoor {
			w.doors[[3]int64{a.X, a.Y, a.Z}] = a.Index
		}
	}
	return w
}

func (w *dungeonWorld) at(x, y, z int64) Block {
	if w.doorOpen && w.opened == nil {
		w.opened = map[int]bool{}
		for _, index := range w.doors {
			w.opened[index] = !w.shut[index]
		}
		w.gate.Update(InstanceUpdate{Doors: w.opened})
	}
	for index, open := range w.opened {
		if open == w.shut[index] {
			w.t.Fatalf("door %d was shut after the doors were opened", index)
		}
	}
	coord := ChunkOf(x, y, z)
	if !w.cache.Contains(coord) {
		return Air
	}
	chunk, _, err := w.cache.Get(context.Background(), coord)
	if err != nil {
		w.t.Fatal(err)
	}
	return chunk.At(Local(x), Local(y), Local(z))
}

// standable is the walking body's cell: two clear, dry courses over something solid.
func (w *dungeonWorld) standable(x, y, z int64) bool {
	feet, head := w.at(x, y, z), w.at(x, y+1, z)
	return !Solid(feet) && !IsWater(feet) && !Solid(head) && Solid(w.at(x, y-1, z))
}

func dungeonAnchorsByKind(seed int64) map[AnchorKind][]PlacedAnchor {
	out := make(map[AnchorKind][]PlacedAnchor)
	for _, a := range InstanceDungeonAnchors(seed) {
		out[a.Kind] = append(out[a.Kind], a)
	}
	return out
}

var dungeonTestSeeds = []int64{0, 1, 2, 3, -1, math.MinInt64, math.MaxInt64}

// Every slot the later issues build on is present, counted, indexed and standing on
// the block its kind names, at every rotation.
func TestTheDungeonAnchorsArePresentAndStandWhereTheirKindSays(t *testing.T) {
	for _, seed := range dungeonTestSeeds {
		w := newDungeonWorld(t, seed, false, false)
		byKind := dungeonAnchorsByKind(seed)
		for kind, want := range map[AnchorKind]int{
			AnchorInstanceArrival: 1, AnchorInstanceExit: 1, AnchorInstanceGuardian: 1, AnchorInstanceKing: 1,
			AnchorInstanceGate: 1, AnchorInstanceCheckpoint: 3, AnchorInstanceMinorSpawn: 20 + 6 + 5,
			AnchorInstanceMechanism: 4 + 1 + 2, AnchorInstanceDoor: 25 + 9 + 9 + 12, AnchorInstanceTrigger: 4,
		} {
			if got := len(byKind[kind]); got != want {
				t.Fatalf("seed %d: %d %s anchors, want %d", seed, got, kind, want)
			}
		}
		// Floor 1 is world y = 1 at every rotation; the shore is the drop below it.
		for _, kind := range []AnchorKind{AnchorInstanceArrival, AnchorInstanceExit, AnchorInstanceGuardian} {
			if y := byKind[kind][0].Y; y != 1 {
				t.Fatalf("seed %d: %s stands at y %d, want floor 1 at y 1", seed, kind, y)
			}
		}
		groups := map[int]int{}
		for _, a := range byKind[AnchorInstanceMinorSpawn] {
			groups[a.Index]++
			if a.Index < CaveBurrowGroup && a.Y != 1 || !w.standable(a.X, a.Y, a.Z) {
				t.Fatalf("seed %d: minor spawn %+v has no floor or headroom", seed, a)
			}
		}
		if len(groups) != 6 || groups[0] != 5 || groups[1] != 5 || groups[2] != 5 || groups[3] != 5 ||
			groups[CaveBurrowGroup] != 6 || groups[SandBuriedGroup] != 5 {
			t.Fatalf("seed %d: spawn groups %v, want four groups of five, six burrows and five scorpions", seed, groups)
		}
		for _, a := range byKind[AnchorInstanceMechanism][:4] {
			if a.Index != runePuzzle || w.at(a.X, a.Y, a.Z) != RuneStone {
				t.Fatalf("seed %d: mechanism %+v holds %d", seed, a, w.at(a.X, a.Y, a.Z))
			}
			if err := w.cache.Apply(context.Background(), a.X, a.Y, a.Z, Air, nil); !errors.Is(err, ErrImmutableShell) {
				t.Fatalf("seed %d: a rune stone was edited: %v", seed, err)
			}
		}
		grille := IronGrilleX
		if uint64(seed)&1 != 0 {
			grille = IronGrilleZ
		}
		for _, a := range byKind[AnchorInstanceDoor][:25] {
			if a.Index != runePuzzle || w.at(a.X, a.Y, a.Z) != grille {
				t.Fatalf("seed %d: door cell %+v holds %d, want grille %d", seed, a, w.at(a.X, a.Y, a.Z), grille)
			}
		}
		checkpoint := byKind[AnchorInstanceCheckpoint][0]
		if checkpoint.Index != 0 || !w.standable(checkpoint.X, checkpoint.Y, checkpoint.Z) {
			t.Fatalf("seed %d: shore checkpoint %+v has no floor or headroom", seed, checkpoint)
		}
		if drop := byKind[AnchorInstanceArrival][0].Y - checkpoint.Y; drop != dungeonUpperFloor-dungeonShore {
			t.Fatalf("seed %d: the shore is %d below floor 1", seed, drop)
		}
	}
}

// The arenas keep the size and monolith layout the boss tests were written against:
// clear floor for 13 blocks either side of the guardian's centre (15 for the king's),
// a wall one further, eight clear courses, and four monoliths four courses tall eight
// blocks out on both axes.
func TestBothArenasKeepTheirMeasuredDimensionsAndMonoliths(t *testing.T) {
	for _, seed := range dungeonTestSeeds {
		w := newDungeonWorld(t, seed, false, false)
		guardian, king, _ := InstanceEncounterAnchors(seed)
		for _, arena := range []struct {
			centre PlacedAnchor
			half   int64
		}{{guardian, 13}, {king, 15}} {
			c := arena.centre
			for _, axis := range [][2]int64{{1, 0}, {0, 1}} {
				for offset := -arena.half; offset <= arena.half; offset++ {
					x, z := c.X+axis[0]*offset, c.Z+axis[1]*offset
					if !Solid(w.at(x, c.Y-1, z)) {
						t.Fatalf("seed %d: arena %+v lost its floor at offset %d", seed, c, offset)
					}
					for y := c.Y; y < c.Y+8; y++ {
						if w.at(x, y, z) != Air {
							t.Fatalf("seed %d: arena %+v obstructed at %d,%d,%d", seed, c, x, y, z)
						}
					}
				}
				// Probed three off the centre line, clear of the doorway on it.
				for _, side := range []int64{-1, 1} {
					if !Solid(w.at(c.X+axis[0]*side*(arena.half+1)+axis[1]*3, c.Y, c.Z+axis[1]*side*(arena.half+1)+axis[0]*3)) {
						t.Fatalf("seed %d: arena %+v has no wall %d blocks out", seed, c, arena.half+1)
					}
				}
			}
			if !Solid(w.at(c.X, c.Y+8, c.Z)) {
				t.Fatalf("seed %d: arena %+v is not eight courses tall", seed, c)
			}
			for _, dx := range []int64{-8, 8} {
				for _, dz := range []int64{-8, 8} {
					for y := c.Y; y < c.Y+4; y++ {
						if w.at(c.X+dx, y, c.Z+dz) != BlackBrickWorn {
							t.Fatalf("seed %d: monolith %+d,%+d of %+v missing at y %d", seed, dx, dz, c, y)
						}
					}
					if w.at(c.X+dx, c.Y+4, c.Z+dz) != Air {
						t.Fatalf("seed %d: monolith %+d,%+d of %+v is taller than four", seed, dx, dz, c)
					}
				}
			}
		}
	}
}

// Closed, the trapdoor is arena floor a body stands on and no edit can remove; open,
// it is the chasm's mouth.
func TestTheTrapdoorIsFloorUntilTheGuardianFalls(t *testing.T) {
	for _, seed := range dungeonTestSeeds {
		w := newDungeonWorld(t, seed, false, false)
		_, _, gate := InstanceEncounterAnchors(seed)
		if len(w.gate.cells) != 25 {
			t.Fatalf("seed %d: the trapdoor has %d cells", seed, len(w.gate.cells))
		}
		for _, p := range w.gate.cells {
			if p.Y != gate.Y || w.at(p.X, p.Y, p.Z) != BlackBrick || !w.standable(p.X, p.Y+1, p.Z) {
				t.Fatalf("seed %d: closed trapdoor cell %+v is not standable floor", seed, p)
			}
			if err := w.cache.Apply(context.Background(), p.X, p.Y, p.Z, Air, nil); !errors.Is(err, ErrImmutableShell) {
				t.Fatalf("seed %d: a trapdoor cell was edited: %v", seed, err)
			}
		}
		if len(w.gate.Open()) != 25 {
			t.Fatalf("seed %d: opening reported the wrong cells", seed)
		}
		for _, p := range w.gate.cells {
			if w.at(p.X, p.Y, p.Z) != Air {
				t.Fatalf("seed %d: open trapdoor cell %+v holds %d", seed, p, w.at(p.X, p.Y, p.Z))
			}
		}
	}
}

// The order is a permutation, every order occurs, and the generated inscription is
// that order: behind stone order[k], k+1 lit runes and then plain wall.
func TestTheRuneInscriptionDrawsTheSeedsOrder(t *testing.T) {
	seen := map[[4]int]bool{}
	for seed := int64(-200); seed < 200; seed++ {
		order := InstanceRuneOrder(seed)
		used := [4]bool{}
		for _, stone := range order {
			if stone < 0 || stone > 3 || used[stone] {
				t.Fatalf("seed %d: order %v is not a permutation", seed, order)
			}
			used[stone] = true
		}
		seen[order] = true
	}
	if len(seen) != 24 {
		t.Fatalf("only %d of the 24 orders occur", len(seen))
	}
	for _, seed := range dungeonTestSeeds {
		w := newDungeonWorld(t, seed, false, false)
		stones := dungeonAnchorsByKind(seed)[AnchorInstanceMechanism]
		b := instancePlacement(seed)
		for k, stone := range InstanceRuneOrder(seed) {
			// The wall cell behind a stone, found by turning the drawing's inscription
			// column with the placement rather than by trusting the stone's position.
			rx, rz := rotateCell(runeStoneXs[stone], inscriptionZ, dungeonWidth, dungeonDepth, b.Facing)
			x, z := b.OriginX+int64(rx), b.OriginZ+int64(rz)
			s := stones[stone]
			if gap := max(s.X-x, x-s.X) + max(s.Z-z, z-s.Z); gap != inscriptionZ-runeStoneZ {
				t.Fatalf("seed %d: stone %d stands %d from its inscription", seed, stone, gap)
			}
			for n := int64(0); n < 5; n++ {
				want := BlackBrick
				if n <= int64(k) {
					want = RuneStoneLit
				}
				if got := w.at(x, s.Y+1+n, z); got != want {
					t.Fatalf("seed %d: stone %d (pressed %d-th) has %d at mark %d, want %d", seed, stone, k, got, n, want)
				}
			}
		}
	}
}

// Every drawn cell but air and webs refuses edits at every rotation: the shell, the
// monoliths, the stones, the portal and the water.
func TestDungeonSceneryRemainsImmutableAtEveryRotation(t *testing.T) {
	for seed := int64(0); seed < 4; seed++ {
		b := instancePlacement(seed)
		for y := range dungeonHeight {
			for z := range dungeonDepth {
				for x := range dungeonWidth {
					block := instanceDungeon.At(x, y, z)
					if block == Air || block == Cobweb || block == keepTerrain {
						continue
					}
					rx, rz := rotateCell(x, z, dungeonWidth, dungeonDepth, b.Facing)
					if instanceInterior(seed, b.OriginX+int64(rx), b.OriginY+int64(y), b.OriginZ+int64(rz)) {
						t.Fatalf("seed %d: scenery %d editable at %d,%d,%d", seed, block, x, y, z)
					}
				}
			}
		}
		arrival, _ := InstanceAnchors(seed)
		if err := NewInstanceCache(seed, 1, 8).Apply(context.Background(), arrival.X, arrival.Y, arrival.Z, Planks, nil); err != nil {
			t.Fatalf("seed %d: floor 1's air refused a placement: %v", seed, err)
		}
	}
}

// Floor 1 stands on basalt everywhere a body can stand: every air cell of its standing
// course sits on a basalt floor course, except over the portal frame's rune sill, the
// chasm's open column and the return shortcut's last flight. A course left to a room's brick shell, or a gap between two
// floor fills, fails here.
func TestFloorOneStandsOnBasaltThroughout(t *testing.T) {
	const f = dungeonUpperFloor
	for z := range dungeonDepth {
		for x := range dungeonWidth {
			if instanceDungeon.At(x, f, z) != Air {
				continue
			}
			under := instanceDungeon.At(x, f-1, z)
			sill := z == exitPortalZ && under == RuneStone
			// The return shortcut's last flight and landing, behind the court's east wall,
			// are a stair and not floor 1.
			stair := x > shortcutCourtDoorX && z < shortcutCorridorZ0
			chasm := x >= chasmX0 && x <= chasmX1 && z >= chasmZ0 && z <= chasmZ1 && under == Air
			if under != Basalt && !sill && !chasm && !stair {
				t.Fatalf("floor 1 at %d,%d stands on %d, not basalt", x, z, under)
			}
		}
	}
}
