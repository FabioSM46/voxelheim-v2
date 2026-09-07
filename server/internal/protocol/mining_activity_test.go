package protocol

import (
	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	flatbuffers "github.com/google/flatbuffers/go"
	"testing"
)

func TestMiningActivityRoundTrip(t *testing.T) {
	for _, tool := range []vnet.MiningTool{vnet.MiningToolHand, vnet.MiningToolShovel, vnet.MiningToolPickaxe, vnet.MiningToolAxe} {
		for _, phase := range []vnet.MiningPhase{vnet.MiningPhaseActive, vnet.MiningPhaseCompleted} {
			want := MiningActivity{Tick: 4294967295, ActorEntityID: 42, ActivityID: 17, Pos: [3]int32{-33, 64, 12}, BlockID: 7, Tool: tool, Phase: phase}
			frame, err := EncodeMiningActivity(want)
			if err != nil {
				t.Fatal(err)
			}
			env := vnet.GetRootAsEnvelope(frame, 0)
			var tab flatbuffers.Table
			if env.PayloadType() != vnet.PayloadMiningActivity || !env.Payload(&tab) {
				t.Fatal("missing mining observation")
			}
			var a vnet.MiningActivity
			a.Init(tab.Bytes, tab.Pos)
			pos := a.Pos(nil)
			if pos == nil {
				t.Fatal("missing position")
			}
			got := MiningActivity{Tick: a.Tick(), ActorEntityID: a.ActorEntityId(), ActivityID: a.ActivityId(), Pos: [3]int32{pos.X(), pos.Y(), pos.Z()}, BlockID: a.BlockId(), Tool: a.Tool(), Phase: a.Phase()}
			if got != want {
				t.Fatalf("round trip=%+v want %+v", got, want)
			}
		}
	}
}
func TestMiningActivityInvalidProducer(t *testing.T) {
	valid := MiningActivity{ActorEntityID: 1, ActivityID: 1, BlockID: 1, Tool: vnet.MiningToolHand, Phase: vnet.MiningPhaseActive}
	for _, mutate := range []func(*MiningActivity){func(a *MiningActivity) { a.ActorEntityID = 0 }, func(a *MiningActivity) { a.ActivityID = 0 }, func(a *MiningActivity) { a.BlockID = 0 }, func(a *MiningActivity) { a.Tool = 0 }, func(a *MiningActivity) { a.Tool = 255 }, func(a *MiningActivity) { a.Phase = 0 }, func(a *MiningActivity) { a.Phase = 255 }} {
		a := valid
		mutate(&a)
		if frame, err := EncodeMiningActivity(a); err == nil || frame != nil {
			t.Fatal("invalid observation encoded")
		}
	}
}
