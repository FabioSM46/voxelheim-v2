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
