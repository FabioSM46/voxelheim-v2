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
