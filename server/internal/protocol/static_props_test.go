package protocol

import (
	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	flatbuffers "github.com/google/flatbuffers/go"
	"testing"
)

func staticPropSnapshot(t *testing.T, props []StaticPropState) *vnet.EntitySnapshot {
	t.Helper()
	frame := EncodeEntitySnapshot(EntitySnapshot{StaticProps: props})
	env := vnet.GetRootAsEnvelope(frame, 0)
	table := payloadTable(t, env)
	snapshot := new(vnet.EntitySnapshot)
	snapshot.Init(table.Bytes, table.Pos)
	return snapshot
}

func TestStaticPropsRoundTripEveryKindPoseAndVariant(t *testing.T) {
	for kind := vnet.StaticPropKindBanquetTable; kind <= vnet.StaticPropKindTableCandelabrum; kind++ {
		for facing := vnet.FacingNorth; facing <= vnet.FacingWest; facing++ {
			for variant := uint8(0); variant < 4; variant++ {
				want := StaticPropState{PropID: 99, Kind: kind, Origin: [3]int32{-9, 21, 17}, Facing: facing, Variant: variant}
				snapshot := staticPropSnapshot(t, []StaticPropState{want})
				if err := validateStaticProps(snapshot); err != nil {
					t.Fatal(err)
				}
				var got vnet.StaticPropState
				if !snapshot.StaticProps(&got, 0) {
					t.Fatal("missing prop")
				}
				origin := got.Origin(nil)
				if got.PropId() != want.PropID || got.Kind() != kind || got.Facing() != facing || got.Variant() != variant || [3]int32{origin.X(), origin.Y(), origin.Z()} != want.Origin {
					t.Fatal("pose changed on wire")
				}
			}
		}
	}
}

func TestStaticPropsBoundAndIdentityNamespace(t *testing.T) {
	props := make([]StaticPropState, MaxStaticProps)
	for i := range props {
		props[i] = StaticPropState{PropID: uint64(i + 1), Kind: vnet.StaticPropKindChair, Facing: vnet.FacingNorth}
	}
	if err := validateStaticProps(staticPropSnapshot(t, props)); err != nil {
		t.Fatal(err)
	}
	props = append(props, StaticPropState{PropID: 257, Kind: vnet.StaticPropKindChair, Facing: vnet.FacingNorth})
	if err := validateStaticProps(staticPropSnapshot(t, props)); err == nil {
		t.Fatal("accepted over-cap catalogue")
	}
	props[1].PropID = props[0].PropID
	if err := validateStaticProps(staticPropSnapshot(t, props[:2])); err == nil {
		t.Fatal("accepted duplicate")
	}
	if err := validateStaticProps(staticPropSnapshot(t, nil)); err != nil {
		t.Fatal(err)
	}
}

func TestStaticPropsRefuseMalformedDescriptors(t *testing.T) {
	valid := StaticPropState{PropID: 1, Kind: vnet.StaticPropKindChair, Facing: vnet.FacingNorth}
	cases := map[string]func(*StaticPropState){
		"zero identity":  func(p *StaticPropState) { p.PropID = 0 },
		"zero kind":      func(p *StaticPropState) { p.Kind = vnet.StaticPropKindUnknown },
		"unknown kind":   func(p *StaticPropState) { p.Kind = 255 },
		"zero facing":    func(p *StaticPropState) { p.Facing = vnet.FacingUnknown },
		"unknown facing": func(p *StaticPropState) { p.Facing = 255 },
		"variant":        func(p *StaticPropState) { p.Variant = 4 },
		"x bound":        func(p *StaticPropState) { p.Origin[0] = 16777216 },
		"y bound":        func(p *StaticPropState) { p.Origin[1] = -16777217 },
		"z bound":        func(p *StaticPropState) { p.Origin[2] = 16777216 },
	}
	for name, mutate := range cases {
		t.Run(name, func(t *testing.T) {
			p := valid
			mutate(&p)
			if err := validateStaticProps(staticPropSnapshot(t, []StaticPropState{p})); err == nil {
				t.Fatal("accepted malformed prop")
			}
		})
	}
	b := flatbuffers.NewBuilder(128)
	vnet.StaticPropStateStart(b)
	vnet.StaticPropStateAddPropId(b, 1)
	vnet.StaticPropStateAddKind(b, vnet.StaticPropKindChair)
	vnet.StaticPropStateAddFacing(b, vnet.FacingNorth)
	prop := vnet.StaticPropStateEnd(b)
	vnet.EntitySnapshotStartStaticPropsVector(b, 1)
	b.PrependUOffsetT(prop)
	vector := b.EndVector(1)
	vnet.EntitySnapshotStart(b)
	vnet.EntitySnapshotAddStaticProps(b, vector)
	snapshotOffset := vnet.EntitySnapshotEnd(b)
	b.Finish(snapshotOffset)
	snapshot := vnet.GetRootAsEntitySnapshot(b.FinishedBytes(), 0)
	if err := validateStaticProps(snapshot); err == nil {
		t.Fatal("accepted missing origin")
	}
}

func TestStaticPropsUseTheCompleteEnvelopeValidationPath(t *testing.T) {
	prop := StaticPropState{PropID: 1, Kind: vnet.StaticPropKindThrone, Facing: vnet.FacingWest}
	snapshot := EntitySnapshot{
		// The same number may identify a player and a prop: namespaces are separate.
		Entities:    []EntityState{{EntityID: 1, Health: 100, MaxHealth: 100}},
		Vitals:      PlayerVitals{Health: 100, MaxHealth: 100, LifeState: vnet.LifeStateAlive},
		StaticProps: []StaticPropState{prop},
	}
	if err := ValidateEntitySnapshot(EncodeEntitySnapshot(snapshot)); err != nil {
		t.Fatal(err)
	}
	snapshot.StaticProps = append(snapshot.StaticProps, prop)
	if err := ValidateEntitySnapshot(EncodeEntitySnapshot(snapshot)); err == nil {
		t.Fatal("envelope validator skipped duplicate static props")
	}
}
