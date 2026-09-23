package world

import (
	"context"
	"errors"
	"sync"
	"testing"
)

// The dungeon's doors, as the gate owns them: counted and indexed, drawn shut, opened
// and shut again only through Update, and never reopened or reclosed by an edit, an
// eviction or a regeneration.
func TestEveryDoorOpensAndShutsOnlyThroughTheGate(t *testing.T) {
	want := map[int]int{runePuzzle: 25, GrillePuzzle: 9, TwinLeverPuzzle: 9, ReturnShortcutDoor: 12}
	for _, seed := range dungeonTestSeeds {
		// A capacity of one chunk evicts on every read of another, so each read below
		// that crosses a chunk is a fresh composition.
		c, g := NewGatedInstanceCache(seed, 1, 1, false)
		read := func(p PlacedAnchor) Block {
			t.Helper()
			ch, _, err := c.Get(context.Background(), ChunkOf(p.X, p.Y, p.Z))
			if err != nil {
				t.Fatal(err)
			}
			return ch.At(Local(p.X), Local(p.Y), Local(p.Z))
		}
		for index, n := range want {
			cells := g.DoorCells(index)
			if len(cells) != n {
				t.Fatalf("seed %d: door %d has %d cells, want %d", seed, index, len(cells), n)
			}
			drawn := make([]Block, n)
			for i, p := range cells {
				drawn[i] = read(p)
				if !Solid(drawn[i]) {
					t.Fatalf("seed %d: door %d cell %+v is drawn open as %d", seed, index, p, drawn[i])
				}
				if err := c.Apply(context.Background(), p.X, p.Y, p.Z, Air, nil); !errors.Is(err, ErrImmutableShell) {
					t.Fatalf("seed %d: a shut door %d cell was edited: %v", seed, index, err)
				}
			}
			if g.DoorOpen(index) {
				t.Fatalf("seed %d: door %d starts open", seed, index)
			}
			rev := c.Revision()
			changed := g.Update(InstanceUpdate{Doors: map[int]bool{index: true}})
			if len(changed) != n || c.Revision() <= rev || !g.DoorOpen(index) {
				t.Fatalf("seed %d: opening door %d changed %d cells", seed, index, len(changed))
			}
			for i, p := range cells {
				if changed[i] != (InstanceCell{X: p.X, Y: p.Y, Z: p.Z, Block: Air}) {
					t.Fatalf("seed %d: door %d change %d is %+v", seed, index, i, changed[i])
				}
				if read(p) != Air {
					t.Fatalf("seed %d: open door %d cell %+v holds %d", seed, index, p, read(p))
				}
				if err := c.Regenerate(ChunkOf(p.X, p.Y, p.Z)); err != nil {
					t.Fatal(err)
				}
				if read(p) != Air {
					t.Fatalf("seed %d: regeneration reclosed door %d", seed, index)
				}
				if err := c.Apply(context.Background(), p.X, p.Y, p.Z, Stone, nil); !errors.Is(err, ErrImmutableShell) {
					t.Fatalf("seed %d: an open door %d cell was filled: %v", seed, index, err)
				}
			}
			if again := g.Update(InstanceUpdate{Doors: map[int]bool{index: true}}); again != nil {
				t.Fatalf("seed %d: reopening door %d reported %d changes", seed, index, len(again))
			}
			shut := g.Update(InstanceUpdate{Doors: map[int]bool{index: false}})
			if len(shut) != n {
				t.Fatalf("seed %d: shutting door %d changed %d cells", seed, index, len(shut))
			}
			for i, p := range cells {
				if shut[i].Block != drawn[i] || read(p) != drawn[i] {
					t.Fatalf("seed %d: door %d cell %+v shut as %d, drawn as %d", seed, index, p, read(p), drawn[i])
				}
			}
		}
	}
}

// Opening one door leaves every other door and the trapdoor as they were, and the
// chunk a reader already holds is never written to: the change is a new publication.
func TestADoorOpensAloneAndNeverMutatesAPublishedChunk(t *testing.T) {
	c, g := NewGatedInstanceCache(0, 1, InstanceChunkEnvelope(0), false)
	cell := g.DoorCells(GrillePuzzle)[0]
	coord := ChunkOf(cell.X, cell.Y, cell.Z)
	held, _, err := c.Get(context.Background(), coord)
	if err != nil {
		t.Fatal(err)
	}
	drawn := held.At(Local(cell.X), Local(cell.Y), Local(cell.Z))
	g.Update(InstanceUpdate{Doors: map[int]bool{GrillePuzzle: true}})
	if held.At(Local(cell.X), Local(cell.Y), Local(cell.Z)) != drawn {
		t.Fatal("opening a door wrote into a published chunk")
	}
	for _, index := range []int{runePuzzle, TwinLeverPuzzle, ReturnShortcutDoor} {
		if g.DoorOpen(index) {
			t.Fatalf("door %d opened with the grille", index)
		}
		p := g.DoorCells(index)[0]
		ch, _, err := c.Get(context.Background(), ChunkOf(p.X, p.Y, p.Z))
		if err != nil || ch.At(Local(p.X), Local(p.Y), Local(p.Z)) == Air {
			t.Fatalf("door %d reads open with the grille: %v", index, err)
		}
	}
	_, _, gate := InstanceEncounterAnchors(0)
	ch, _, err := c.Get(context.Background(), ChunkOf(gate.X, gate.Y, gate.Z))
	if err != nil || ch.At(Local(gate.X), Local(gate.Y), Local(gate.Z)) != BlackBrick {
		t.Fatalf("the trapdoor opened with the grille: %v", err)
	}
}

// A mechanism shows what the gate says — a lever up or down, a rune dark or lit —
// through eviction and regeneration, and no edit reaches it in either state.
func TestAMechanismShowsWhatTheGateSaysThroughEveryRebuild(t *testing.T) {
	for _, seed := range dungeonTestSeeds {
		c, g := NewGatedInstanceCache(seed, 1, 1, false)
		mechanisms := g.Mechanisms()
		if len(mechanisms) != 7 {
			t.Fatalf("seed %d: %d mechanisms, want 7", seed, len(mechanisms))
		}
		for _, m := range mechanisms {
			read := func() Block {
				t.Helper()
				ch, _, err := c.Get(context.Background(), ChunkOf(m.X, m.Y, m.Z))
				if err != nil {
					t.Fatal(err)
				}
				return ch.At(Local(m.X), Local(m.Y), Local(m.Z))
			}
			dark, lit := LeverOff, LeverOn
			if m.Index == runePuzzle {
				dark, lit = RuneStone, RuneStoneLit
			}
			if shown, ok := g.MechanismBlock(m.X, m.Y, m.Z); !ok || shown != dark || read() != dark {
				t.Fatalf("seed %d: mechanism %+v starts as %d", seed, m, read())
			}
			changed := g.Update(InstanceUpdate{Mechanisms: []InstanceCell{{X: m.X, Y: m.Y, Z: m.Z, Block: lit}}})
			if len(changed) != 1 || read() != lit {
				t.Fatalf("seed %d: mechanism %+v did not change to %d", seed, m, lit)
			}
			if err := c.Regenerate(ChunkOf(m.X, m.Y, m.Z)); err != nil {
				t.Fatal(err)
			}
			if read() != lit {
				t.Fatalf("seed %d: regeneration reset mechanism %+v", seed, m)
			}
			if err := c.Apply(context.Background(), m.X, m.Y, m.Z, Air, nil); !errors.Is(err, ErrImmutableShell) {
				t.Fatalf("seed %d: mechanism %+v was edited: %v", seed, m, err)
			}
			if again := g.Update(InstanceUpdate{Mechanisms: []InstanceCell{{X: m.X, Y: m.Y, Z: m.Z, Block: lit}}}); again != nil {
				t.Fatalf("seed %d: an unchanged mechanism reported %d changes", seed, len(again))
			}
			g.Update(InstanceUpdate{Mechanisms: []InstanceCell{{X: m.X, Y: m.Y, Z: m.Z, Block: dark}}})
			if read() != dark {
				t.Fatalf("seed %d: mechanism %+v did not return to %d", seed, m, dark)
			}
		}
	}
}

// A door, a cell or a block the layout has no mechanism for is a caller's programming
// error, and it is refused loudly rather than patched into a chunk.
func TestAnUpdateNamingSomethingTheLayoutLacksPanics(t *testing.T) {
	_, g := NewGatedInstanceCache(0, 1, 1, false)
	m := g.Mechanisms()[0]
	door := g.DoorCells(runePuzzle)[0]
	for name, u := range map[string]InstanceUpdate{
		"unknown door":      {Doors: map[int]bool{99: true}},
		"door cell":         {Mechanisms: []InstanceCell{{X: door.X, Y: door.Y, Z: door.Z, Block: LeverOn}}},
		"not a mechanism":   {Mechanisms: []InstanceCell{{X: m.X, Y: m.Y, Z: m.Z, Block: Air}}},
		"not a known cell":  {Mechanisms: []InstanceCell{{X: m.X, Y: m.Y + 100, Z: m.Z, Block: RuneStoneLit}}},
		"door 0 is no door": {Doors: map[int]bool{0: true}},
	} {
		func() {
			defer func() {
				if recover() == nil {
					t.Errorf("%s: no panic", name)
				}
			}()
			g.Update(u)
		}()
	}
}

// An update that lands while the door's chunk is being generated is not lost to the
// generation: composition runs under the same lock and applies the new state.
func TestADoorOpeningWinsAnInFlightGeneration(t *testing.T) {
	c, g := NewGatedInstanceCache(0, 1, 2, false)
	p := g.DoorCells(TwinLeverPuzzle)[0]
	coord := ChunkOf(p.X, p.Y, p.Z)
	began, release := make(chan struct{}), make(chan struct{})
	original := c.generate
	c.generate = func(seed int64, at Coord) *Chunk { base := original(seed, at); close(began); <-release; return base }
	var wg sync.WaitGroup
	wg.Add(1)
	go func() {
		defer wg.Done()
		if _, _, err := c.Get(context.Background(), coord); err != nil {
			t.Error(err)
		}
	}()
	<-began
	g.Update(InstanceUpdate{Doors: map[int]bool{TwinLeverPuzzle: true}})
	close(release)
	wg.Wait()
	ch, _, err := c.Get(context.Background(), coord)
	if err != nil || ch.At(Local(p.X), Local(p.Y), Local(p.Z)) != Air {
		t.Fatal("a stale generation reclosed the door", err)
	}
}
