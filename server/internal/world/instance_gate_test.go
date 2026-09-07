package world

import (
	"context"
	"errors"
	"sync"
	"testing"
)

func TestDungeonGateSurvivesEditsEvictionAndRegeneration(t *testing.T) {
	for seed := int64(0); seed < 4; seed++ {
		c, g := NewGatedInstanceCache(seed, 1, 1, false)
		_, _, a := InstanceEncounterAnchors(seed)
		coord := ChunkOf(a.X, a.Y, a.Z)
		read := func(want Block) {
			t.Helper()
			ch, _, err := c.Get(context.Background(), coord)
			if err != nil || ch.At(Local(a.X), Local(a.Y), Local(a.Z)) != want {
				t.Fatalf("gate: want %v err %v", want, err)
			}
		}
		read(BlackBrick)
		for _, cell := range g.cells {
			if err := c.Apply(context.Background(), cell.X, cell.Y, cell.Z, Air, nil); !errors.Is(err, ErrImmutableShell) {
				t.Fatal("player edited gate", err)
			}
		}
		if err := c.Regenerate(coord); err != nil {
			t.Fatal(err)
		}
		read(BlackBrick)
		old, _, _ := c.Get(context.Background(), coord)
		rev := c.Revision()
		cells := g.Open()
		if len(cells) != 25 || c.Revision() <= rev {
			t.Fatal("opening did not publish the bounded gate change")
		}
		read(Air)
		if old.At(Local(a.X), Local(a.Y), Local(a.Z)) != BlackBrick {
			t.Fatal("opening mutated a published chunk")
		}
		if len(g.Open()) != 0 {
			t.Fatal("duplicate opening")
		}
		if _, _, err := c.Get(context.Background(), Coord{Y: -1}); err != nil {
			t.Fatal(err)
		}
		read(Air)
		if err := c.Regenerate(coord); err != nil {
			t.Fatal(err)
		}
		read(Air)
		if err := c.Apply(context.Background(), a.X, a.Y, a.Z, BlackBrick, nil); !errors.Is(err, ErrImmutableShell) {
			t.Fatal("player closed the open gate")
		}
	}
}

func TestDungeonGateOpeningWinsAnInFlightGeneration(t *testing.T) {
	c, g := NewGatedInstanceCache(0, 1, 2, false)
	_, _, a := InstanceEncounterAnchors(0)
	coord := ChunkOf(a.X, a.Y, a.Z)
	began, release := make(chan struct{}), make(chan struct{})
	original := c.generate
	c.generate = func(seed int64, at Coord) *Chunk { base := original(seed, at); close(began); <-release; return base }
	var wg sync.WaitGroup
	wg.Add(1)
	go func() {
		defer wg.Done()
		_, _, err := c.Get(context.Background(), coord)
		if err != nil {
			t.Error(err)
		}
	}()
	<-began
	g.Open()
	close(release)
	wg.Wait()
	chunk, _, err := c.Get(context.Background(), coord)
	if err != nil || chunk.At(Local(a.X), Local(a.Y), Local(a.Z)) != Air {
		t.Fatal("stale generation reclosed gate", err)
	}
}

func TestDungeonGateOpeningWinsAnInFlightRegeneration(t *testing.T) {
	c, g := NewGatedInstanceCache(0, 1, 2, false)
	_, _, a := InstanceEncounterAnchors(0)
	coord := ChunkOf(a.X, a.Y, a.Z)
	if _, _, err := c.Get(context.Background(), coord); err != nil {
		t.Fatal(err)
	}
	began, release := make(chan struct{}), make(chan struct{})
	original := c.generate
	c.generate = func(seed int64, at Coord) *Chunk { base := original(seed, at); close(began); <-release; return base }
	var wg sync.WaitGroup
	wg.Add(1)
	go func() {
		defer wg.Done()
		if err := c.Regenerate(coord); err != nil {
			t.Error(err)
		}
	}()
	<-began
	g.Open()
	close(release)
	wg.Wait()
	ch, err := c.Peek(coord)
	if err != nil || ch.At(Local(a.X), Local(a.Y), Local(a.Z)) != Air {
		t.Fatal("stale regeneration reclosed gate", err)
	}
}
