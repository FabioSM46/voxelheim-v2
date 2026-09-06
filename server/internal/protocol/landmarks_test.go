package protocol

import (
	"bytes"
	"math"
	"reflect"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/transport"
	flatbuffers "github.com/google/flatbuffers/go"
)

func readLandmarks(t *testing.T, frame []byte) LandmarkList {
	t.Helper()
	env := vnet.GetRootAsEnvelope(frame, 0)
	if env.PayloadType() != vnet.PayloadLandmarkList {
		t.Fatalf("wrong tag: %v", env.PayloadType())
	}
	var tab flatbuffers.Table
	if !env.Payload(&tab) {
		t.Fatal("missing payload")
	}
	var list vnet.LandmarkList
	list.Init(tab.Bytes, tab.Pos)
	vectorTable := list.Table()
	if vectorTable.Offset(4) == 0 {
		t.Fatal("absent vector, even empty must be present")
	}
	out := LandmarkList{OriginX: list.OriginX(), OriginZ: list.OriginZ(), Scale: list.Scale(), Landmarks: make([]Landmark, list.LandmarksLength())}
	for i := range out.Landmarks {
		var l vnet.Landmark
		if !list.Landmarks(&l, i) {
			t.Fatal("absent landmark")
		}
		out.Landmarks[i] = Landmark{LandmarkID: l.LandmarkId(), X: l.X(), Z: l.Z(), Kind: l.Kind(), Discovered: l.Discovered()}
	}
	return out
}

func TestLandmarkListRoundTripPreservesEveryEntry(t *testing.T) {
	for _, discovered := range []bool{false, true} {
		want := LandmarkList{OriginX: -46784, OriginZ: 54912, Scale: 1, Landmarks: []Landmark{{LandmarkID: math.MaxUint64, X: -46757, Z: 54922, Kind: vnet.LandmarkKindPortal, Discovered: discovered}}}
		frame, err := EncodeLandmarkList(want)
		if err != nil {
			t.Fatal(err)
		}
		// Nonempty independently supplied values make an empty-encoder mutation fail.
		if got := readLandmarks(t, frame); !reflect.DeepEqual(got, want) {
			t.Fatalf("got %+v want %+v", got, want)
		}
	}
	empty := LandmarkList{OriginX: 1024, OriginZ: -1024, Scale: 16, Landmarks: []Landmark{}}
	frame, err := EncodeLandmarkList(empty)
	if err != nil {
		t.Fatal(err)
	}
	if got := readLandmarks(t, frame); !reflect.DeepEqual(got, empty) {
		t.Fatalf("empty scope: %+v", got)
	}
	b := flatbuffers.NewBuilder(32)
	vnet.LandmarkStart(b)
	l := vnet.LandmarkEnd(b)
	b.Finish(l)
	absent := vnet.GetRootAsLandmark(b.FinishedBytes(), 0)
	if absent.Kind() != vnet.LandmarkKindUnknown || absent.Discovered() {
		t.Fatal("absent fields must not claim known kind or discovery")
	}
}

func TestLandmarkEncoderRefusesInvalidWholeScopes(t *testing.T) {
	valid := Landmark{LandmarkID: 1, X: 1, Z: 1, Kind: vnet.LandmarkKindPortal}
	for name, list := range map[string]LandmarkList{
		"absent scope": {}, "bad scale": {Scale: 3}, "off grid x": {Scale: 1, OriginX: 1}, "off grid z": {Scale: 4, OriginZ: -1},
		"too many":     {Scale: 1, Landmarks: []Landmark{valid, valid}},
		"zero id":      {Scale: 1, Landmarks: []Landmark{{Kind: vnet.LandmarkKindPortal}}},
		"absent kind":  {Scale: 1, Landmarks: []Landmark{{LandmarkID: 1}}},
		"unknown kind": {Scale: 1, Landmarks: []Landmark{{LandmarkID: 1, Kind: vnet.LandmarkKind(255)}}},
		"below x":      {Scale: 1, Landmarks: []Landmark{{LandmarkID: 1, X: -1, Kind: vnet.LandmarkKindPortal}}},
		"past x":       {Scale: 1, Landmarks: []Landmark{{LandmarkID: 1, X: 64, Kind: vnet.LandmarkKindPortal}}},
		"below z":      {Scale: 1, Landmarks: []Landmark{{LandmarkID: 1, Z: -1, Kind: vnet.LandmarkKindPortal}}},
		"past z":       {Scale: 1, Landmarks: []Landmark{{LandmarkID: 1, Z: 64, Kind: vnet.LandmarkKindPortal}}},
	} {
		t.Run(name, func(t *testing.T) {
			if frame, err := EncodeLandmarkList(list); err == nil || frame != nil {
				t.Fatal("invalid scope produced a frame")
			}
		})
	}
}

func TestScopedLandmarkBoundsUseWideArithmeticAndRetainV31TransportLimit(t *testing.T) {
	if MaxLandmarks != 1 || transport.MaxFrameSize != 2<<20 {
		t.Fatal("contract bounds changed")
	}
	for _, x := range []int32{math.MinInt32, math.MaxInt32} {
		origin := x - (x%64+64)%64
		want := LandmarkList{OriginX: origin, Scale: 1, Landmarks: []Landmark{{LandmarkID: 1, X: x, Kind: vnet.LandmarkKindPortal}}}
		frame, err := EncodeLandmarkList(want)
		if err != nil {
			t.Fatal(err)
		}
		var wire bytes.Buffer
		if err := transport.WriteFrame(&wire, frame); err != nil {
			t.Fatal(err)
		}
		received, err := transport.ReadFrame(&wire)
		if err != nil {
			t.Fatal(err)
		}
		if got := readLandmarks(t, received); !reflect.DeepEqual(got, want) {
			t.Fatal("scope changed on transport")
		}
	}
}
