package session_test

import (
	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"slices"
	"testing"
	"time"
)

func (c *collector) miningObservations() []protocol.MiningActivity {
	c.mu.Lock()
	defer c.mu.Unlock()
	return slices.Clone(c.miningActivities)
}
func TestMiningObservationCrossesRealSessionDelivery(t *testing.T) {
	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg)
	conn, miner := admit(t, cfg, chunks, sim, peers, 1)
	_, observer := admit(t, cfg, chunks, sim, peers, 2)
	target := surfaceUnderSpawn(cfg.WorldSeed)
	var clientTick uint32
	var serverTick uint64
	mineUntilBreak(t, conn, miner, sim, target, &clientTick, &serverTick)
	// Wait for the worker's successful write, then release precisely its next snapshot.
	serverTick++
	sim.Step(serverTick)
	waitUntil(t, "completed mining to reach observer", func() bool {
		for _, a := range observer.miningObservations() {
			if a.Phase == vnet.MiningPhaseCompleted {
				return true
			}
		}
		return false
	})
	for _, sink := range []*collector{miner, observer} {
		all := sink.miningObservations()
		active, completed := 0, 0
		var identity uint64
		for _, a := range all {
			if a.Pos != target || a.ActorEntityID != 1 || a.Tool != vnet.MiningToolHand || a.BlockID == 0 || a.ActivityID == 0 {
				t.Fatalf("bad observation %+v", a)
			}
			if identity != 0 && identity != a.ActivityID {
				t.Fatal("one attempt changed identity")
			}
			identity = a.ActivityID
			if a.Phase == vnet.MiningPhaseCompleted {
				completed++
			} else {
				active++
			}
		}
		if active == 0 || completed != 1 {
			t.Fatalf("active=%d completed=%d", active, completed)
		}
	}
	if len(observer.mineProgress()) != 0 {
		t.Fatal("observer learned private mining progress")
	}
}
func TestForgedMiningActivityClosesSessionWithoutObservation(t *testing.T) {
	m := startMarking(t, t.TempDir(), testAccount(74), "Eivor", 1)
	frame, err := protocol.EncodeMiningActivity(protocol.MiningActivity{ActorEntityID: 1, ActivityID: 1, BlockID: 1, Tool: vnet.MiningToolHand, Phase: vnet.MiningPhaseCompleted})
	if err != nil {
		t.Fatal(err)
	}
	m.conn.in <- frame
	select {
	case err := <-m.done:
		m.stopped = true
		if err == nil {
			t.Fatal("forged mining accepted")
		}
	case <-time.After(patience):
		t.Fatal("forged mining did not close session")
	}
	if slices.Contains(m.sink.kindsReceived(), vnet.PayloadMiningActivity) {
		t.Fatal("forgery produced activity")
	}
}
