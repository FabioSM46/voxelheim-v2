package world

import (
	"context"
	"errors"
	"os"
	"reflect"
	"testing"
)

// tallLayout is a synthetic two-floor dungeon taller than two chunks, built with the
// section helpers: a lower hall, a shaft up to an upper hall, a flight of stairs,
// cobwebs, a lever and a gate whose door (levels 29–33) straddles the chunk
// boundary at y = 32.
func tallLayout(t *testing.T) instanceLayout {
	t.Helper()
	s := NewSection(41, 70, 37).
		CarveRoom(Box{2, 1, 2, 38, 6, 34}, Basalt).           // lower hall, floor 0
		CarveShaft(17, 16, 21, 20, 7, 27, BlackBrick).        // up through its ceiling
		CarveRoom(Box{2, 29, 2, 38, 64, 34}, BlackBrickWorn). // upper hall, very tall
		FillFloor(2, 2, 38, 34, 28, Cobblestone).             // its floor…
		FillFloor(17, 16, 21, 20, 28, Air).                   // …with the shaft left open
		PlaceStairs(5, 29, 5, FacingPlusZ, 6, 2, BlackBrick).
		Fill(Box{10, 1, 10, 12, 2, 10}, Cobweb).
		Fill(Box{3, 29, 30, 3, 29, 30}, LeverOff).
		Anchor(AnchorInstanceArrival, 20, 1, 5, 0).
		Anchor(AnchorInstanceCheckpoint, 20, 29, 25, 0).
		Anchor(AnchorInstanceMechanism, 3, 29, 30, 1).
		Anchor(AnchorInstanceTrigger, 3, 29, 3, 0).
		Anchor(AnchorInstanceTrigger, 37, 40, 20, 0).
		Anchor(AnchorInstanceGate, 30, 29, 20, 0)
	drawing, err := s.Build()
	if err != nil {
		t.Fatal(err)
	}
	return instanceLayout{drawing: drawing}
}

// Every chunk of the envelope, generated independently, agrees with a direct
// placement of the drawing voxel for voxel — across X, Y and Z chunk boundaries and
// at all four rotations. Nothing outside the drawing is anything but void.
func TestATallLayoutGeneratesIdenticallyAcrossChunkBoundaries(t *testing.T) {
	l := tallLayout(t)
	for seed := int64(0); seed < 4; seed++ {
		b := l.placement(seed)
		lo, hi := l.chunkBounds(seed)
		if hi.Y-lo.Y < 2 {
			t.Fatalf("seed %d: the layout spans chunk rows %d..%d, want at least three", seed, lo.Y, hi.Y)
		}

		want := make(map[[3]int64]Block)
		for y := range l.drawing.H {
			for z := range l.drawing.D {
				for x := range l.drawing.W {
					block := l.drawing.At(x, y, z)
					if block == keepTerrain {
						continue
					}
					rx, rz := rotateCell(x, z, l.drawing.W, l.drawing.D, b.Facing)
					want[[3]int64{b.OriginX + int64(rx), b.OriginY + int64(y), b.OriginZ + int64(rz)}] = rotateSchematicBlock(block, b.Facing)
				}
			}
		}

		seen := 0
		for cx := lo.X - 1; cx <= hi.X+1; cx++ {
			for cy := lo.Y - 1; cy <= hi.Y+1; cy++ {
				for cz := lo.Z - 1; cz <= hi.Z+1; cz++ {
					coord := Coord{X: cx, Y: cy, Z: cz}
					if !l.containsChunk(seed, coord) {
						t.Fatalf("seed %d: the envelope refuses %+v", seed, coord)
					}
					chunk := l.generate(seed, coord)
					ox, oy, oz := coord.Origin()
					for i, got := range chunk.Blocks {
						x, z, y := i%ChunkSize, (i/ChunkSize)%ChunkSize, i/(ChunkSize*ChunkSize)
						p := [3]int64{ox + int64(x), oy + int64(y), oz + int64(z)}
						expected, drawn := want[p]
						if !drawn {
							expected = Air
						} else {
							seen++
						}
						if got != expected {
							t.Fatalf("seed %d: %v is %d, want %d", seed, p, got, expected)
						}
					}
				}
			}
		}
		if seen != len(want) {
			t.Fatalf("seed %d: the envelope's chunks hold %d drawn cells, the drawing %d", seed, seen, len(want))
		}
		for _, outside := range []Coord{{X: lo.X - 2, Y: lo.Y, Z: lo.Z}, {X: lo.X, Y: hi.Y + 2, Z: lo.Z}, {X: lo.X, Y: lo.Y - 2, Z: hi.Z}} {
			if l.containsChunk(seed, outside) {
				t.Errorf("seed %d: %+v is inside the envelope", seed, outside)
			}
		}
	}
}

// Contains, the edit rule and anchors work on every floor: air and a drawn cobweb are
// editable, walls, floors, the lever and void are not, and each placed anchor lands on
// the cell its kind names.
func TestATallLayoutsEditRuleAndAnchorsHoldOnEveryFloor(t *testing.T) {
	l := tallLayout(t)
	ctx := context.Background()
	for seed := int64(0); seed < 4; seed++ {
		cache := l.cache(seed, 1, 256)
		blockAt := func(p PlacedAnchor) Block {
			chunk, _, err := cache.Get(ctx, ChunkOf(p.X, p.Y, p.Z))
			if err != nil {
				t.Fatal(err)
			}
			return chunk.At(Local(p.X), Local(p.Y), Local(p.Z))
		}
		byKind := make(map[AnchorKind][]PlacedAnchor)
		for _, a := range l.placement(seed).Anchors {
			byKind[a.Kind] = append(byKind[a.Kind], a)
		}
		lever := byKind[AnchorInstanceMechanism][0]
		if blockAt(lever) != LeverOff || lever.Index != 1 {
			t.Fatalf("seed %d: the mechanism anchor %+v holds %d", seed, lever, blockAt(lever))
		}
		if cache.editable(lever.X, lever.Y, lever.Z) {
			t.Errorf("seed %d: the lever is editable", seed)
		}
		checkpoint := byKind[AnchorInstanceCheckpoint][0]
		if blockAt(checkpoint) != Air || !cache.editable(checkpoint.X, checkpoint.Y, checkpoint.Z) {
			t.Errorf("seed %d: the upper checkpoint is not editable air", seed)
		}
		if floor := (PlacedAnchor{X: checkpoint.X, Y: checkpoint.Y - 1, Z: checkpoint.Z}); blockAt(floor) != Cobblestone || cache.editable(floor.X, floor.Y, floor.Z) {
			t.Errorf("seed %d: the upper floor is not immutable cobblestone", seed)
		}
		if len(byKind[AnchorInstanceTrigger]) != 2 {
			t.Fatalf("seed %d: %d trigger corners", seed, len(byKind[AnchorInstanceTrigger]))
		}

		// Clear a web on the lower floor: allowed, and the upper floor's air takes a
		// placement in a chunk two rows above.
		arrival := byKind[AnchorInstanceArrival][0]
		web := firstCell(t, l, seed, Cobweb)
		if err := cache.Apply(ctx, web.X, web.Y, web.Z, Air, nil); err != nil {
			t.Fatalf("seed %d: clearing a web: %v", seed, err)
		}
		if err := cache.Apply(ctx, checkpoint.X, checkpoint.Y, checkpoint.Z, Planks, nil); err != nil {
			t.Fatalf("seed %d: placing on the upper floor: %v", seed, err)
		}
		if err := cache.Apply(ctx, arrival.X, arrival.Y-1, arrival.Z, Air, nil); !errors.Is(err, ErrImmutableShell) {
			t.Fatalf("seed %d: the lower floor was edited: %v", seed, err)
		}
		if err := cache.Apply(ctx, lever.X, lever.Y, lever.Z, Air, nil); !errors.Is(err, ErrImmutableShell) {
			t.Fatalf("seed %d: the lever was edited: %v", seed, err)
		}
	}
}

// firstCell finds where the placed layout put a block, in world coordinates.
func firstCell(t *testing.T, l instanceLayout, seed int64, want Block) PlacedAnchor {
	t.Helper()
	b := l.placement(seed)
	for y := range l.drawing.H {
		for z := range l.drawing.D {
			for x := range l.drawing.W {
				if l.drawing.At(x, y, z) == want {
					rx, rz := rotateCell(x, z, l.drawing.W, l.drawing.D, b.Facing)
					return PlacedAnchor{X: b.OriginX + int64(rx), Y: b.OriginY + int64(y), Z: b.OriginZ + int64(rz)}
				}
			}
		}
	}
	t.Fatalf("the layout holds no block %d", want)
	return PlacedAnchor{}
}

// The gate's door straddles the y = 32 chunk boundary and is patched into both
// chunks, closed and then open, and its cells refuse edits while it stands.
func TestAGateAcrossAVerticalChunkBoundaryPatchesBothChunks(t *testing.T) {
	l := tallLayout(t)
	ctx := context.Background()
	for seed := int64(0); seed < 4; seed++ {
		cache, gate := l.gated(seed, 1, 256, false)
		rows := make(map[int32]bool)
		for _, p := range gate.cells {
			rows[ChunkOf(p.X, p.Y, p.Z).Y] = true
		}
		if len(rows) != 2 {
			t.Fatalf("seed %d: the door spans chunk rows %v, want two", seed, rows)
		}
		read := func(p PlacedAnchor) Block {
			chunk, _, err := cache.Get(ctx, ChunkOf(p.X, p.Y, p.Z))
			if err != nil {
				t.Fatal(err)
			}
			return chunk.At(Local(p.X), Local(p.Y), Local(p.Z))
		}
		for _, p := range gate.cells {
			if read(p) != BlackBrick {
				t.Fatalf("seed %d: closed door cell %+v holds %d", seed, p, read(p))
			}
			if err := cache.Apply(ctx, p.X, p.Y, p.Z, Air, nil); !errors.Is(err, ErrImmutableShell) {
				t.Fatalf("seed %d: a door cell was edited: %v", seed, err)
			}
		}
		if len(gate.Open()) != len(gate.cells) {
			t.Fatal("opening reported the wrong cells")
		}
		for _, p := range gate.cells {
			if read(p) != Air {
				t.Fatalf("seed %d: open door cell %+v holds %d", seed, p, read(p))
			}
		}
	}
}

// Nothing about a layout is persistent: its cache has no Store, edits on several
// floors write no file, and a regenerated or fresh instance is the drawing again. So
// changing the layout later bumps no version and invalidates nothing on disk.
func TestALayoutChangePersistsNothing(t *testing.T) {
	dir := t.TempDir()
	t.Chdir(dir)
	l := tallLayout(t)
	ctx := context.Background()
	cache := l.cache(5, 1, 256)
	if cache.store != nil {
		t.Fatal("a layout cache has a store")
	}
	checkpoint := firstAnchor(l, 5, AnchorInstanceCheckpoint)
	arrival := firstAnchor(l, 5, AnchorInstanceArrival)
	for _, p := range []PlacedAnchor{checkpoint, arrival} {
		if err := cache.Apply(ctx, p.X, p.Y, p.Z, Planks, nil); err != nil {
			t.Fatal(err)
		}
	}
	if err := cache.Flush(); err != nil {
		t.Fatal(err)
	}
	if entries, err := os.ReadDir(dir); err != nil || len(entries) != 0 {
		t.Fatalf("a layout instance wrote files: count=%d err=%v", len(entries), err)
	}
	coord := ChunkOf(checkpoint.X, checkpoint.Y, checkpoint.Z)
	if err := cache.Regenerate(coord); err != nil {
		t.Fatal(err)
	}
	if chunk, _, err := cache.Get(ctx, coord); err != nil || !reflect.DeepEqual(chunk, l.generate(5, coord)) {
		t.Fatal("regeneration did not restore the drawing")
	}
	fresh := l.cache(5, 1, 256)
	chunk, _, err := fresh.Get(ctx, ChunkOf(arrival.X, arrival.Y, arrival.Z))
	if err != nil || chunk.At(Local(arrival.X), Local(arrival.Y), Local(arrival.Z)) != Air {
		t.Fatal("a fresh instance kept another's edit")
	}
}

func firstAnchor(l instanceLayout, seed int64, kind AnchorKind) PlacedAnchor {
	for _, a := range l.placement(seed).Anchors {
		if a.Kind == kind {
			return a
		}
	}
	return PlacedAnchor{}
}

// The envelope a game sizes its cache from is the chamber's 72 chunks today, and it
// counts every chunk the layout's Contains accepts.
func TestTheInstanceEnvelopeCountsEveryContainedChunk(t *testing.T) {
	for seed := int64(0); seed < 4; seed++ {
		lo, hi := chamberLayout.chunkBounds(seed)
		n := 0
		for x := lo.X - 3; x <= hi.X+3; x++ {
			for y := lo.Y - 3; y <= hi.Y+3; y++ {
				for z := lo.Z - 3; z <= hi.Z+3; z++ {
					if chamberLayout.containsChunk(seed, Coord{X: x, Y: y, Z: z}) {
						n++
					}
				}
			}
		}
		if got := InstanceChunkEnvelope(seed); got != n || got != 72 {
			t.Errorf("seed %d: envelope %d, contained %d, want 72", seed, got, n)
		}
	}
}
