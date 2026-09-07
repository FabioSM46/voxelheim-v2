package session_test

import (
	"slices"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

func TestForgedBlowEndsTheSessionWithoutAnOutcome(t *testing.T) {
	m := startMarking(t, t.TempDir(), testAccount(73), "Eivor", 1)
	frame, err := protocol.EncodeBlowLanded(protocol.BlowLanded{TargetEntityID: 9, Kind: vnet.BlowKindMelee, Target: vnet.BlowTargetMob, TargetMobKind: vnet.MobKindDraugr})
	if err != nil {
		t.Fatal(err)
	}
	m.conn.in <- frame
	select {
	case err := <-m.done:
		m.stopped = true
		if err == nil {
			t.Fatal("client-authored blow accepted")
		}
	case <-time.After(patience):
		t.Fatal("forged blow did not close session")
	}
	if slices.Contains(m.sink.kindsReceived(), vnet.PayloadBlowLanded) {
		t.Fatal("forgery produced an outcome")
	}
}
