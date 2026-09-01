package game

import (
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

func TestLearnedMountsProduceAStableCompleteSet(t *testing.T) {
	t.Parallel()

	state := LearnedMounts(0b101).State()
	want := []vnet.MountKind{vnet.MountKindBlackHorse, vnet.MountKindGreyHorse}
	if len(state.Mounts) != len(want) {
		t.Fatalf("the learned set contains %d mounts, want %d", len(state.Mounts), len(want))
	}
	for index := range want {
		if state.Mounts[index] != want[index] {
			t.Errorf("learned mount %d is %s, want %s", index, state.Mounts[index], want[index])
		}
	}
}

func TestLearnedMountsRefuseEveryUnknownStoredBit(t *testing.T) {
	t.Parallel()

	for bit := uint8(3); bit < 8; bit++ {
		if err := LearnedMounts(1 << bit).Validate(); err == nil {
			t.Errorf("Validate accepted unknown learned-mount bit %d", bit)
		}
	}
	if err := LearnedMounts(0b111).Validate(); err != nil {
		t.Errorf("Validate refused all three concrete mounts: %v", err)
	}
}

func TestLearningAddsEachKnownMountOnceAndRefusesUnknowns(t *testing.T) {
	t.Parallel()

	var learned LearnedMounts
	for _, kind := range []vnet.MountKind{
		vnet.MountKindBlackHorse,
		vnet.MountKindBrownHorse,
		vnet.MountKindGreyHorse,
	} {
		next, added := learned.Learn(kind)
		if !added || !next.Has(kind) {
			t.Fatalf("learning %s from %#02x returned %#02x, added=%v", kind, learned, next, added)
		}
		if duplicate, added := next.Learn(kind); added || duplicate != next {
			t.Errorf("learning %s twice returned %#02x, added=%v; want unchanged %#02x", kind, duplicate, added, next)
		}
		learned = next
	}
	if learned != allLearnedMounts {
		t.Errorf("learning every concrete mount produced %#02x, want %#02x", learned, allLearnedMounts)
	}
	for _, kind := range []vnet.MountKind{vnet.MountKindUnknown, vnet.MountKind(99)} {
		if next, added := learned.Learn(kind); added || next != learned || learned.Has(kind) {
			t.Errorf("unknown %d changed %#02x into %#02x (added=%v)", kind, learned, next, added)
		}
	}
}
