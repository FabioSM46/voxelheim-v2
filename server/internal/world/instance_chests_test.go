package world

import "testing"

func TestChestIDsAndAnchorValidation(t *testing.T) {
	if Chest != 64 || ChestOpen != 65 || AnchorInstanceChest != AnchorInstanceDoor+1 {
		t.Fatal("chest block or anchor ids moved")
	}
	if !Mechanism(Chest) || Mechanism(ChestOpen) || !Solid(Chest) || !Solid(ChestOpen) || Placeable(Chest) || Placeable(ChestOpen) {
		t.Fatal("chest block predicates changed")
	}
	for _, tc := range []struct {
		block Block
		index int
		valid bool
	}{
		{Chest, AntechamberChest, true}, {Chest, SandHallChest, true}, {Chest, KingChest, true},
		{Air, 0, false}, {ChestOpen, 0, false}, {Chest, -1, false}, {Chest, InstanceChestCount, false},
	} {
		_, err := NewSection(5, 5, 5).
			CarveRoom(Box{1, 1, 1, 3, 3, 3}, Basalt).
			Fill(Box{2, 1, 2, 2, 1, 2}, tc.block).
			Anchor(AnchorInstanceChest, 2, 1, 2, tc.index).Build()
		if (err == nil) != tc.valid {
			t.Errorf("block %d index %d: error %v, want valid %v", tc.block, tc.index, err, tc.valid)
		}
	}
}

func TestDungeonChestsOccupyThreeReachableEdgeSlots(t *testing.T) {
	want := [InstanceChestCount][3]int{
		{12, dungeonUpperFloor, 97}, {17, dungeonSandFloor, 35}, {20, dungeonKingFloor, 79},
	}
	for _, a := range instanceDungeon.Anchors {
		if a.Kind == AnchorInstanceChest && [3]int{a.X, a.Y, a.Z} != want[a.Index] {
			t.Fatalf("chest %d is at %d,%d,%d", a.Index, a.X, a.Y, a.Z)
		}
	}
	for _, seed := range dungeonTestSeeds {
		w := newDungeonWorld(t, seed, true, true)
		anchors := dungeonAnchorsByKind(seed)[AnchorInstanceChest]
		if len(anchors) != InstanceChestCount {
			t.Fatalf("seed %d: %d chests", seed, len(anchors))
		}
		seen := map[int]bool{}
		for _, a := range anchors {
			if seen[a.Index] || a.Index < 0 || a.Index >= InstanceChestCount {
				t.Fatalf("invalid chest index %d", a.Index)
			}
			seen[a.Index] = true
			if w.at(a.X, a.Y, a.Z) != Chest || !Solid(w.at(a.X, a.Y-1, a.Z)) || w.at(a.X, a.Y+1, a.Z) != Air {
				t.Fatalf("chest %+v has wrong block, floor or headroom", a)
			}
			for _, delta := range [][2]int64{{1, 0}, {-1, 0}, {0, 1}, {0, -1}} {
				if !w.standable(a.X+delta[0], a.Y, a.Z+delta[1]) {
					t.Fatalf("chest %+v has no clear adjacent approach at %v", a, delta)
				}
			}
		}
	}
}
