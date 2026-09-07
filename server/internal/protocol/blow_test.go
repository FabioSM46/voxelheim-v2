package protocol

import (
	"math"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	flatbuffers "github.com/google/flatbuffers/go"
)

func TestBlowLandedRoundTrip(t *testing.T) {
	for _, kind := range []vnet.BlowKind{vnet.BlowKindMelee, vnet.BlowKindArrow, vnet.BlowKindEnergyOrb, vnet.BlowKindMobMelee} {
		for _, target := range []vnet.BlowTarget{vnet.BlowTargetPlayer, vnet.BlowTargetMob} {
			want := BlowLanded{Tick: math.MaxUint32, AttackerEntityID: math.MaxUint64, TargetEntityID: 52, Position: [3]float32{-13.25, 64.5, 72.75}, Kind: kind, Target: target}
			if target == vnet.BlowTargetMob {
				want.TargetMobKind = vnet.MobKindDraugr
			}
			frame, err := EncodeBlowLanded(want)
			if err != nil {
				t.Fatal(err)
			}
			// Length and identifier assertions ensure a gutted encoder cannot pass by
			// exercising only the decoder's negative path.
			if len(frame) < 8 || string(frame[4:8]) != "VXLH" {
				t.Fatalf("missing blow frame: %x", frame)
			}
			env := vnet.GetRootAsEnvelope(frame, 0)
			var tab flatbuffers.Table
			if env.PayloadType() != vnet.PayloadBlowLanded || !env.Payload(&tab) {
				t.Fatal("wrong or absent payload")
			}
			var blow vnet.BlowLanded
			blow.Init(tab.Bytes, tab.Pos)
			pos := blow.Position(nil)
			if pos == nil {
				t.Fatal("absent position")
			}
			got := BlowLanded{Tick: blow.Tick(), AttackerEntityID: blow.AttackerEntityId(), TargetEntityID: blow.TargetEntityId(), Position: [3]float32{pos.X(), pos.Y(), pos.Z()}, Kind: blow.Kind(), Target: blow.Target(), TargetMobKind: blow.TargetMobKind()}
			if got != want {
				t.Fatalf("round trip = %+v, want %+v", got, want)
			}
		}
	}
	anonymous := BlowLanded{TargetEntityID: 1, Kind: vnet.BlowKindArrow, Target: vnet.BlowTargetMob, TargetMobKind: vnet.MobKindVargr}
	if _, err := EncodeBlowLanded(anonymous); err != nil {
		t.Fatalf("anonymous attacker: %v", err)
	}
}

func TestBlowLandedRejectsInvalidProducerValues(t *testing.T) {
	valid := BlowLanded{TargetEntityID: 1, Kind: vnet.BlowKindMelee, Target: vnet.BlowTargetMob, TargetMobKind: vnet.MobKindDraugr}
	mutations := []func(*BlowLanded){
		func(b *BlowLanded) { b.TargetEntityID = 0 },
		func(b *BlowLanded) { b.Kind = vnet.BlowKindUnknown },
		func(b *BlowLanded) { b.Kind = 255 },
		func(b *BlowLanded) { b.Target = vnet.BlowTargetUnknown },
		func(b *BlowLanded) { b.Target = 255 },
		func(b *BlowLanded) { b.Target = vnet.BlowTargetPlayer },
		func(b *BlowLanded) { b.TargetMobKind = vnet.MobKindUnknown },
		func(b *BlowLanded) { b.TargetMobKind = 255 },
	}
	for axis := range 3 {
		for _, invalid := range []float32{float32(math.NaN()), float32(math.Inf(1)), float32(math.Inf(-1))} {
			mutations = append(mutations, func(b *BlowLanded) { b.Position[axis] = invalid })
		}
	}
	for i, mutate := range mutations {
		bad := valid
		mutate(&bad)
		if frame, err := EncodeBlowLanded(bad); err == nil || frame != nil {
			t.Errorf("invalid case %d encoded", i)
		}
	}
}
