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

func readLandmarks(t *testing.T, frame []byte) []Landmark {
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
	out := make([]Landmark, list.LandmarksLength())
	for i := range out {
		var l vnet.Landmark
		if !list.Landmarks(&l, i) {
			t.Fatalf("absent landmark %d", i)
		}
		out[i] = Landmark{LandmarkID: l.LandmarkId(), X: l.X(), Z: l.Z(), Kind: l.Kind()}
	}
	return out
}

func TestLandmarkListRoundTripPreservesEveryEntry(t *testing.T) {
	want := []Landmark{{LandmarkID: 1, X: -46757, Z: 54922, Kind: vnet.LandmarkKindPortal}, {LandmarkID: math.MaxUint64, X: 7091, Z: -93680, Kind: vnet.LandmarkKindPortal}}
	frame, err := EncodeLandmarkList(LandmarkList{Landmarks: want})
	if err != nil {
		t.Fatal(err)
	}
	// Compare against a nonempty independently supplied value: replacing the encoder
	// with an empty-list implementation must fail, even if empty frames remain legal.
	if got := readLandmarks(t, frame); !reflect.DeepEqual(got, want) {
		t.Fatalf("got %+v want %+v", got, want)
	}
	empty, err := EncodeLandmarkList(LandmarkList{})
	if err != nil || len(readLandmarks(t, empty)) != 0 {
		t.Fatalf("empty list: %v", err)
	}
	// An absent scalar remains the explicit fail-closed enum zero.
	b := flatbuffers.NewBuilder(32)
	vnet.LandmarkStart(b)
	l := vnet.LandmarkEnd(b)
	b.Finish(l)
	if got := vnet.GetRootAsLandmark(b.FinishedBytes(), 0).Kind(); got != vnet.LandmarkKindUnknown {
		t.Fatalf("absent kind: %v", got)
	}
}

func TestLandmarkEncoderRefusesInvalidWholeLists(t *testing.T) {
	valid := Landmark{LandmarkID: 1, Kind: vnet.LandmarkKindPortal}
	for name, list := range map[string][]Landmark{
		"too many":     make([]Landmark, MaxLandmarks+1),
		"zero id":      {{Kind: vnet.LandmarkKindPortal}},
		"absent kind":  {{LandmarkID: 1}},
		"unknown kind": {{LandmarkID: 1, Kind: vnet.LandmarkKind(255)}},
		"duplicate":    {valid, valid},
	} {
		t.Run(name, func(t *testing.T) {
			if frame, err := EncodeLandmarkList(LandmarkList{Landmarks: list}); err == nil || frame != nil {
				t.Fatal("invalid list produced a frame")
			}
		})
	}
}

func TestMaximumLandmarkListFitsV31FrameWithoutTruncation(t *testing.T) {
	if MaxLandmarks != 65536 || transport.MaxFrameSize != 2<<20 {
		t.Fatal("contract limits changed")
	}
	entries := make([]Landmark, MaxLandmarks)
	for i := range entries {
		entries[i] = Landmark{LandmarkID: uint64(i + 1), X: int32(i + 1), Z: -int32(i + 1), Kind: vnet.LandmarkKindPortal}
	}
	frame, err := EncodeLandmarkList(LandmarkList{Landmarks: entries})
	if err != nil {
		t.Fatal(err)
	}
	if len(frame) <= 1<<20 || len(frame) > transport.MaxFrameSize {
		t.Fatalf("maximum frame: %d bytes", len(frame))
	}
	var wire bytes.Buffer
	if err := transport.WriteFrame(&wire, frame); err != nil {
		t.Fatal(err)
	}
	received, err := transport.ReadFrame(&wire)
	if err != nil {
		t.Fatal(err)
	}
	if got := readLandmarks(t, received); !reflect.DeepEqual(got, entries) {
		t.Fatal("maximum list was truncated or changed")
	}
	t.Logf("full list: %d bytes", len(frame))
}
