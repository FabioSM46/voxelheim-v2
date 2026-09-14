package protocol

import (
	"math"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
)

// Both draw edges decode as the intent they carry, the extreme client tick included.
func TestDrawRequestRoundTripsAsBothEdges(t *testing.T) {
	t.Parallel()

	for _, want := range []DrawRequest{
		{Active: true, ClientTick: math.MaxUint32},
		{Active: false, ClientTick: 0},
	} {
		msg, err := Decode(EncodeDrawRequest(want))
		if err != nil {
			t.Fatalf("Decode: %v", err)
		}
		if msg.Kind != vnet.PayloadDrawRequest || msg.Draw == nil {
			t.Fatalf("message = %+v, want DrawRequest", msg)
		}
		if *msg.Draw != want {
			t.Errorf("DrawRequest = %+v, want %+v", *msg.Draw, want)
		}
	}
}

// The recipient's draw progress reaches the wire beside the rest of its vitals, zero and the
// full-draw 255 included.
func TestSnapshotVitalsCarryDrawProgress(t *testing.T) {
	t.Parallel()

	for _, want := range []uint8{0, 1, 128, 255} {
		frame := EncodeEntitySnapshot(EntitySnapshot{
			Tick: 3,
			Vitals: PlayerVitals{
				Health: 100, MaxHealth: 100, Hunger: 100, MaxHunger: 100, Level: 1,
				ExperienceToNext: 50, LifeState: vnet.LifeStateAlive, Energy: 40, MaxEnergy: 100,
				DrawProgress: want,
			},
		})
		table := payloadTable(t, vnet.GetRootAsEnvelope(frame, 0))
		snapshot := new(vnet.EntitySnapshot)
		snapshot.Init(table.Bytes, table.Pos)
		vitals := snapshot.SelfVitals(nil)
		if vitals == nil {
			t.Fatal("snapshot carries no vitals")
		}
		if got := vitals.DrawProgress(); got != want {
			t.Errorf("draw_progress = %d, want %d", got, want)
		}
		if got := vitals.Energy(); got != 40 {
			t.Errorf("energy = %d beside draw_progress %d, want 40", got, want)
		}
	}
}
