package world

import (
	"context"
	"testing"
)

// checkThreshold proves a threshold names exactly the portal voxels a placement
// writes, one course thick along its normal, with the heart among them.
func checkThreshold(t *testing.T, got PortalThreshold, portal map[[3]int64]Block) {
	t.Helper()
	if len(got.Cells) != len(portal) || len(got.Cells) == 0 {
		t.Fatalf("threshold has %d cells, placement writes %d portal voxels", len(got.Cells), len(portal))
	}
	heart := false
	lateral := 2 - got.Normal
	lo, hi := got.Heart[lateral], got.Heart[lateral]
	for _, cell := range got.Cells {
		block, drawn := portal[cell]
		if !drawn {
			t.Fatalf("threshold cell %v is not a portal voxel in the placement", cell)
		}
		if cell == got.Heart {
			heart = block == PortalHeart
		}
		if cell[got.Normal] != got.Heart[got.Normal] {
			t.Fatalf("cell %v leaves the one-block sheet along axis %d", cell, got.Normal)
		}
		lo, hi = min(lo, cell[lateral]), max(hi, cell[lateral])
	}
	if !heart {
		t.Fatalf("heart %v is not the placement's portal heart", got.Heart)
	}
	if lo != got.Heart[lateral]-2 || hi != got.Heart[lateral]+2 {
		t.Fatalf("opening spans %d..%d across a heart at %d, want five wide", lo, hi, got.Heart[lateral])
	}
}

func portalVoxels(b Building, s *Schematic) map[[3]int64]Block {
	voxels := make(map[[3]int64]Block)
	for y := range s.H {
		for z := range s.D {
			for x := range s.W {
				if block := s.At(x, y, z); Portal(block) {
					rx, rz := rotateCell(x, z, s.W, s.D, b.Facing)
					voxels[[3]int64{b.OriginX + int64(rx), b.OriginY + int64(y), b.OriginZ + int64(rz)}] = block
				}
			}
		}
	}
	return voxels
}

func TestRuinThresholdFollowsEveryVariantAndQuarterTurn(t *testing.T) {
	for variant := range uint8(RuinVariantCount) {
		for facing := Facing(0); facing < 4; facing++ {
			b := centredRuinBuilding(variant, -1000, 777, 60, facing)
			var arch PlacedAnchor
			for _, a := range b.Anchors {
				if a.Kind == AnchorRuinArch {
					arch = a
				}
			}
			written := make(map[[3]int64]Block)
			visitSchematic(b, func(x, y, z int64, block Block) {
				if Portal(block) {
					written[[3]int64{x, y, z}] = block
				}
			})
			got := Ruin{Building: b}.Threshold()
			checkThreshold(t, got, written)
			if got.Heart != [3]int64{arch.X, arch.Y, arch.Z} {
				t.Fatalf("variant %d facing %d: heart %v, arch anchor %+v", variant, facing, got.Heart, arch)
			}
			wantNormal := 2
			if facing == FacingMinusX || facing == FacingPlusX {
				wantNormal = 0
			}
			if got.Normal != wantNormal {
				t.Fatalf("variant %d facing %d: normal %d, want %d", variant, facing, got.Normal, wantNormal)
			}
		}
	}
}

func TestInstanceExitThresholdFollowsTheChamberTurn(t *testing.T) {
	normals := map[int]bool{}
	for seed := int64(-2); seed < 6; seed++ {
		got := InstanceExitThreshold(seed)
		_, exit := InstanceAnchors(seed)
		if got.Heart != [3]int64{exit.X, exit.Y, exit.Z} {
			t.Fatalf("seed %d: heart %v, exit anchor %+v", seed, got.Heart, exit)
		}
		b := instancePlacement(seed)
		checkThreshold(t, got, portalVoxels(b, instanceChamber))
		for _, cell := range got.Cells {
			chunk := GenerateInstance(seed, ChunkOf(cell[0], cell[1], cell[2]))
			if !Portal(chunk.At(Local(cell[0]), Local(cell[1]), Local(cell[2]))) {
				t.Fatalf("seed %d: generated instance has no veil at %v", seed, cell)
			}
		}
		normals[got.Normal] = true
	}
	if !normals[0] || !normals[2] {
		t.Fatal("the seeds did not exercise both sheet orientations")
	}
}

func TestTheWorldRuinThresholdIsTheVeilItsChunksHold(t *testing.T) {
	const seed = 0x5EED
	ruin, ok := RuinAt(seed, 0, 0)
	if !ok {
		t.Fatal("fixture ruin missing")
	}
	got := ruin.Threshold()
	if got.Heart != [3]int64{ruin.Arch.X, ruin.Arch.Y, ruin.Arch.Z} {
		t.Fatalf("heart %v, arch %+v", got.Heart, ruin.Arch)
	}
	cache := NewCache(seed, 1, 8)
	for _, cell := range got.Cells {
		chunk, _, err := cache.Get(context.Background(), ChunkOf(cell[0], cell[1], cell[2]))
		if err != nil {
			t.Fatal(err)
		}
		if !Portal(chunk.At(Local(cell[0]), Local(cell[1]), Local(cell[2]))) {
			t.Fatalf("composed terrain has no veil at %v", cell)
		}
	}
}

// The world's one ruin is always present in its own cell, and PortalRuin says so rather
// than handing back a zero ruin whose threshold would stand at the origin.
func TestPortalRuinIsTheRuinOfItsOwnCell(t *testing.T) {
	cx, cz := ruinCell()
	for _, seed := range []int64{0x5EED, 1, -7, 424242} {
		got, found := PortalRuin(seed)
		want, wantFound := RuinAt(seed, cx, cz)
		if !found || !wantFound {
			t.Fatalf("seed %d: the ruin cell holds no ruin", seed)
		}
		if got.Arch != want.Arch || got.Building.Facing != want.Building.Facing || got.Building.Variant != want.Building.Variant {
			t.Fatalf("seed %d: PortalRuin %+v is not the ruin of its cell %+v", seed, got.Arch, want.Arch)
		}
		if heart := got.Threshold().Heart; heart != [3]int64{got.Arch.X, got.Arch.Y, got.Arch.Z} || heart == [3]int64{} {
			t.Fatalf("seed %d: the portal threshold stands at %v, not at the arch %+v", seed, heart, got.Arch)
		}
	}
}
