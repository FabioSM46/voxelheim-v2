package world

import (
	"context"
	"errors"
	"testing"
)

// A synthetic drawing avoids deciding the pending production chest placement.
func chestStateLayout() instanceLayout {
	return instanceLayout{drawing: NewSection(12, 8, 12).
		CarveRoom(Box{1, 1, 1, 10, 6, 10}, Basalt).
		Anchor(AnchorInstanceGate, 5, 1, 2, 0).
		Fill(Box{3, 1, 8, 3, 1, 8}, Chest).
		Anchor(AnchorInstanceChest, 3, 1, 8, SandHallChest).
		Fill(Box{8, 1, 8, 8, 1, 8}, LeverOff).
		Anchor(AnchorInstanceMechanism, 8, 1, 8, GrillePuzzle).MustBuild()}
}

func TestChestStateSurvivesRecompositionAndCannotBeEdited(t *testing.T) {
	for _, seed := range []int64{0, 1, 2, 3} {
		cache, gate := chestStateLayout().gated(seed, 1, 1, false)
		anchors := gate.Chests()
		if len(anchors) != 1 || anchors[0].Index != SandHallChest || len(gate.Mechanisms()) != 1 {
			t.Fatalf("chests mixed with puzzle anchors: %v", anchors)
		}
		a := anchors[0]
		anchors[0].Index = KingChest
		if gate.Chests()[0].Index != SandHallChest {
			t.Fatal("returned chest anchors alias gate storage")
		}
		read := func() Block {
			t.Helper()
			chunk, _, err := cache.Get(context.Background(), ChunkOf(a.X, a.Y, a.Z))
			if err != nil {
				t.Fatal(err)
			}
			return chunk.At(Local(a.X), Local(a.Y), Local(a.Z))
		}
		if read() != Chest {
			t.Fatal("chest did not start closed")
		}
		update := InstanceUpdate{Mechanisms: []InstanceCell{{X: a.X, Y: a.Y, Z: a.Z, Block: ChestOpen}}}
		if changed := gate.Update(update); len(changed) != 1 || read() != ChestOpen {
			t.Fatalf("opening did not publish one chest change: %v", changed)
		}
		if changed := gate.Update(update); len(changed) != 0 {
			t.Fatal("opening twice changed the world")
		}
		if err := cache.Regenerate(ChunkOf(a.X, a.Y, a.Z)); err != nil {
			t.Fatal(err)
		}
		if read() != ChestOpen {
			t.Fatal("regeneration closed chest")
		}
		if err := cache.Apply(context.Background(), a.X, a.Y, a.Z, Air, nil); !errors.Is(err, ErrImmutableShell) {
			t.Fatalf("open chest allowed a player edit: %v", err)
		}
	}
}

func TestChestTransitionsDoNotChangePuzzleCells(t *testing.T) {
	for _, chestCell := range []bool{false, true} {
		t.Run(map[bool]string{false: "lever to chest", true: "chest to lever"}[chestCell], func(t *testing.T) {
			_, gate := chestStateLayout().gated(0, 1, 1, false)
			a, block := gate.Mechanisms()[0], ChestOpen
			if chestCell {
				a, block = gate.Chests()[0], LeverOn
			}
			defer func() {
				if recover() == nil {
					t.Fatal("invalid state family was accepted")
				}
			}()
			gate.Update(InstanceUpdate{Mechanisms: []InstanceCell{{X: a.X, Y: a.Y, Z: a.Z, Block: block}}})
		})
	}
}
