package protocol

import (
	"errors"
	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	flatbuffers "github.com/google/flatbuffers/go"
	"math"
	"reflect"
	"testing"
)

func TestPortalIntentRoundTripCopiesOnlyTheArch(t *testing.T) {
	for _, want := range []PortalRequest{{}, {Arch: [3]int32{-46757, 64, 54922}, HasArch: true}, {Arch: [3]int32{math.MaxInt32, math.MinInt32, 0}, HasArch: true}} {
		frame := EncodePortalRequest(want)
		got, err := Decode(frame)
		if err != nil || got.Kind != vnet.PayloadPortalRequest || got.Portal == nil || *got.Portal != want {
			t.Fatalf("round trip: %+v %v", got, err)
		}
		clear(frame)
		if *got.Portal != want {
			t.Fatal("request borrows untrusted bytes")
		}
	}
	b := flatbuffers.NewBuilder(32)
	if _, err := Decode(finishEnvelope(b, vnet.PayloadPortalRequest, 0)); !errors.Is(err, ErrMalformed) {
		t.Fatal("missing table accepted", err)
	}
}

func TestWorldChangeRoundTripAndProducerBounds(t *testing.T) {
	for _, want := range []WorldChange{{WorldSeed: -123, Arrival: [3]float32{100.5, 64, -80.5}}, {WorldID: math.MaxUint64, WorldSeed: 99, Arrival: [3]float32{.5, 1, -2.5}, HasExitArch: true, ExitArch: [3]int32{3, 1, 0}}} {
		frame, err := EncodeWorldChange(want)
		if err != nil {
			t.Fatal(err)
		}
		env := vnet.GetRootAsEnvelope(frame, 0)
		var tab flatbuffers.Table
		if env.PayloadType() != vnet.PayloadWorldChange || !env.Payload(&tab) {
			t.Fatal("wrong envelope")
		}
		var change vnet.WorldChange
		change.Init(tab.Bytes, tab.Pos)
		a := change.Arrival(nil)
		if a == nil {
			t.Fatal("absent arrival")
		}
		got := WorldChange{WorldID: change.WorldId(), WorldSeed: change.WorldSeed(), Arrival: [3]float32{a.X(), a.Y(), a.Z()}}
		if arch := change.ExitArch(nil); arch != nil {
			got.HasExitArch = true
			got.ExitArch = [3]int32{arch.X(), arch.Y(), arch.Z()}
		}
		if !reflect.DeepEqual(got, want) {
			t.Fatalf("got %+v want %+v", got, want)
		}
	}
	for _, bad := range []WorldChange{{WorldID: 1}, {HasExitArch: true}, {Arrival: [3]float32{float32(math.NaN())}}, {Arrival: [3]float32{float32(math.Inf(1))}}, {Arrival: [3]float32{2 * MaxWorldCoordinate}}, {WorldID: 1, HasExitArch: true, ExitArch: [3]int32{math.MaxInt32}}} {
		if frame, err := EncodeWorldChange(bad); err == nil || frame != nil {
			t.Fatalf("encoded invalid %+v", bad)
		}
	}
}
