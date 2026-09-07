package game

import (
	"context"
	"fmt"
	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
	flatbuffers "github.com/google/flatbuffers/go"
	"log/slog"
	"testing"
)

func (s *miningSink) observations(t *testing.T) []protocol.MiningActivity {
	t.Helper()
	s.mu.Lock()
	defer s.mu.Unlock()
	var result []protocol.MiningActivity
	var tick uint32
	visible := map[uint64]bool{}
	for _, frame := range s.frames {
		env := vnet.GetRootAsEnvelope(frame, 0)
		var tab flatbuffers.Table
		switch env.PayloadType() {
		case vnet.PayloadEntitySnapshot:
			if !env.Payload(&tab) {
				t.Fatal("missing snapshot")
			}
			var snap vnet.EntitySnapshot
			snap.Init(tab.Bytes, tab.Pos)
			tick = snap.ServerTick()
			visible = map[uint64]bool{}
			for i := 0; i < snap.EntitiesLength(); i++ {
				var e vnet.EntityState
				snap.Entities(&e, i)
				visible[e.EntityId()] = true
			}
		case vnet.PayloadMiningActivity:
			if !env.Payload(&tab) {
				t.Fatal("missing activity")
			}
			var a vnet.MiningActivity
			a.Init(tab.Bytes, tab.Pos)
			pos := a.Pos(nil)
			if pos == nil || a.Tick() != tick || !visible[a.ActorEntityId()] {
				t.Fatal("activity not disclosed by preceding snapshot")
			}
			result = append(result, protocol.MiningActivity{Tick: a.Tick(), ActorEntityID: a.ActorEntityId(), ActivityID: a.ActivityId(), Pos: [3]int32{pos.X(), pos.Y(), pos.Z()}, BlockID: a.BlockId(), Tool: a.Tool(), Phase: a.Phase()})
		}
	}
	return result
}
func miningObserver(t *testing.T, sim *Sim, id uint64, pos [3]float32) (*Player, *miningSink) {
	t.Helper()
	out := &miningSink{}
	p, err := sim.Join(id, testPlayerID(id), fmt.Sprintf("Observer%d", id), pos, testAppearance(), nil, out.deliver)
	if err != nil {
		t.Fatal(err)
	}
	return p, out
}
func TestMiningObserversReceiveToolsWithoutPrivateProgress(t *testing.T) {
	for _, tc := range []struct {
		item ItemID
		tool vnet.MiningTool
	}{{ItemNone, vnet.MiningToolHand}, {ItemShovel, vnet.MiningToolShovel}, {ItemPickaxe, vnet.MiningToolPickaxe}, {ItemAxe, vnet.MiningToolAxe}} {
		t.Run(tc.tool.String(), func(t *testing.T) {
			pos := [3]int32{2, 200, 0}
			sim, p, _, out := newMiningPlayer(t, map[[3]int64]world.Block{mineTarget(pos): world.Stone})
			_, near := miningObserver(t, sim, 2, [3]float32{0.5, 200, 1.5})
			_, far := miningObserver(t, sim, 3, [3]float32{100, 200, 0})
			_, high := miningObserver(t, sim, 4, [3]float32{0, 260, 0})
			p.inventory.mu.Lock()
			p.inventory.slots[1] = stackOf(tc.item, 1)
			p.inventory.mu.Unlock()
			if err := p.Mine(activeMineWith(pos, 1, 1), true); err != nil {
				t.Fatal(err)
			}
			sim.Step(1)
			sim.Step(2)
			for _, sink := range []*miningSink{out, near} {
				got := sink.observations(t)
				if len(got) != 2 {
					t.Fatalf("got %d observations", len(got))
				}
				for _, a := range got {
					if a.ActorEntityID != p.entityID || a.Tool != tc.tool || a.Pos != pos || a.BlockID != uint16(world.Stone) || a.ActivityID == 0 || a.Phase != vnet.MiningPhaseActive {
						t.Fatalf("wrong observation %+v", a)
					}
				}
			}
			if len(far.observations(t)) != 0 || len(high.observations(t)) != 0 {
				t.Fatal("out-of-interest observer learned mining")
			}
			if len(near.progress(t)) != 0 || len(out.progress(t)) != 2 {
				t.Fatal("private progress delivery changed")
			}
		})
	}
}
func TestMiningObservationLifecycleStopsRenewals(t *testing.T) {
	for _, mode := range []string{"cancel", "invalid-target", "target-hidden", "leave", "death", "disconnect", "terrain-lost", "idle"} {
		t.Run(mode, func(t *testing.T) {
			pos := [3]int32{2, 200, 0}
			sim, p, w, _ := newMiningPlayer(t, map[[3]int64]world.Block{mineTarget(pos): world.Stone})
			_, out := miningObserver(t, sim, 2, [3]float32{0.5, 200, 1.5})
			if err := p.Mine(activeMine(pos, 1), true); err != nil {
				t.Fatal(err)
			}
			sim.Step(1)
			switch mode {
			case "cancel":
				req := activeMine(pos, 2)
				req.Active = false
				if err := p.Mine(req, true); err != nil {
					t.Fatal(err)
				}
			case "invalid-target":
				if err := p.Mine(activeMine([3]int32{20, 200, 0}, 2), true); err == nil {
					t.Fatal("invalid target accepted")
				}
			case "target-hidden":
				if err := p.Mine(activeMine(pos, 2), false); err == nil {
					t.Fatal("hidden target accepted")
				}
			case "leave":
				p.BeginLeaving()
			case "death":
				sim.mu.Lock()
				p.dieLocked()
				sim.mu.Unlock()
			case "disconnect":
				sim.Leave(p)
			case "terrain-lost":
				w.mu.Lock()
				w.resident = false
				w.mu.Unlock()
			case "idle":
				sim.mu.Lock()
				p.mining.idleTicks = sim.idleLimit
				sim.mu.Unlock()
			}
			sim.Step(2)
			sim.Step(3)
			if got := out.observations(t); len(got) != 1 {
				t.Fatalf("stale action renewed after %s: %+v", mode, got)
			}
		})
	}
}
func TestMiningCompletionRequiresSuccessfulWriteAndDistinctAttempt(t *testing.T) {
	for _, changed := range []bool{false, true} {
		sim, p, w, out := newMiningPlayer(t, nil)
		pos := [3]int32{2, 200, 0}
		w.set(pos, world.Leaves)
		if err := p.Mine(activeMine(pos, 1), true); err != nil {
			t.Fatal(err)
		}
		cost := stepToHardness(t, sim, world.Leaves)
		c := awaitCompletion(t, p)
		for _, a := range out.observations(t) {
			if a.Phase == vnet.MiningPhaseCompleted {
				t.Fatal("completion announced before write")
			}
		}
		if changed {
			w.set(pos, world.Stone)
		}
		_, err := p.CompleteMining(context.Background(), c)
		if changed && err == nil {
			t.Fatal("changed target accepted")
		}
		if !changed && err != nil {
			t.Fatal(err)
		}
		if !changed {
			next := [3]int32{3, 200, 0}
			w.set(next, world.Stone)
			if err := p.Mine(activeMine(next, 2), true); err != nil {
				t.Fatal(err)
			}
		}
		sim.Step(uint64(cost + 1))
		sim.Step(uint64(cost + 2))
		completed := 0
		var old, newest uint64
		for _, a := range out.observations(t) {
			if a.Phase == vnet.MiningPhaseCompleted {
				completed++
				old = a.ActivityID
				if a.BlockID != uint16(world.Leaves) || a.Pos != pos {
					t.Fatal("lost original target")
				}
			} else if a.Pos != pos {
				newest = a.ActivityID
			}
		}
		if changed {
			if completed != 0 {
				t.Fatal("failed write announced completion")
			}
		} else if completed != 1 || newest <= old {
			t.Fatalf("completion=%d old=%d next=%d", completed, old, newest)
		}
		if _, err := p.CompleteMining(context.Background(), c); err == nil {
			t.Fatal("completion replay accepted")
		}
	}
}
func TestMiningTargetInterestAndDroppedDeliveryAreBounded(t *testing.T) {
	pos := [3]int32{2 * world.ChunkSize, 200, 0}
	sim, p, w, out := newMiningPlayer(t, map[[3]int64]world.Block{mineTarget(pos): world.Leaves})
	sim.mu.Lock()
	p.pos = [3]float64{2*world.ChunkSize - 1.5, 200, 0.5}
	p.chunk = chunkAt(p.pos)
	sim.mu.Unlock()
	_, edge := miningObserver(t, sim, 2, [3]float32{0.5, 200, 0.5})
	if err := p.Mine(activeMine(pos, 1), true); err != nil {
		t.Fatal(err)
	}
	cost := stepToHardness(t, sim, world.Leaves)
	c := awaitCompletion(t, p)
	if len(edge.observations(t)) != 0 {
		t.Fatal("visible actor disclosed hidden target")
	}
	if _, err := p.CompleteMining(context.Background(), c); err != nil {
		t.Fatal(err)
	}
	out.setRefuses(true)
	sim.Step(uint64(cost + 1))
	out.setRefuses(false)
	sim.Step(uint64(cost + 2))
	if p.miningCompleted != nil {
		t.Fatal("completion retained after its one offer")
	}
	for _, a := range out.observations(t) {
		if a.Phase == vnet.MiningPhaseCompleted {
			t.Fatal("dropped completion replayed")
		}
	}
	w.set(pos, world.Leaves)
	if err := p.Mine(activeMine(pos, 2), true); err != nil {
		t.Fatal(err)
	}
	sim.Step(uint64(cost + 3))
	got := out.observations(t)
	if len(got) == 0 || got[len(got)-1].ActivityID <= c.activityID {
		t.Fatal("recovery reused completed identity")
	}
}

func TestMiningActivityCannotCrossWorldTransfer(t *testing.T) {
	for _, complete := range []bool{false, true} {
		pos := [3]int32{2, 200, 0}
		sim, p, _, _ := newMiningPlayer(t, map[[3]int64]world.Block{mineTarget(pos): world.Leaves})
		terrain := newMiningWorld(nil)
		other, err := NewSim(DefaultTickRate, 1, testWorldSeed, terrain, terrain, testEntityIDs(), slog.New(slog.DiscardHandler), WithWorldGroup(sim.group))
		if err != nil {
			t.Fatal(err)
		}
		_, out := miningObserver(t, other, 2, [3]float32{0.5, 200, 1.5})
		if err := p.Mine(activeMine(pos, 1), true); err != nil {
			t.Fatal(err)
		}
		if complete {
			stepToHardness(t, sim, world.Leaves)
			if _, err := p.CompleteMining(context.Background(), awaitCompletion(t, p)); err != nil {
				t.Fatal(err)
			}
		} else {
			sim.Step(1)
		}
		other.Step(1)
		if len(out.observations(t)) != 0 {
			t.Fatal("foreign world learned mining")
		}
		if err := sim.Transfer(p, other, [3]float32{0.5, 200, 0.5}); err != nil {
			t.Fatal(err)
		}
		other.Step(2)
		if len(out.observations(t)) != 0 || p.mining != nil || p.miningCompleted != nil {
			t.Fatal("old-world activity survived transfer")
		}
	}
}

func TestMiningRefreshChangesOnlyPresentationToolAndRetargetChangesIdentity(t *testing.T) {
	pos := [3]int32{2, 200, 0}
	next := [3]int32{3, 200, 0}
	sim, p, _, out := newMiningPlayer(t, map[[3]int64]world.Block{mineTarget(pos): world.Stone, mineTarget(next): world.Stone})
	if err := p.Mine(activeMine(pos, 1), true); err != nil {
		t.Fatal(err)
	}
	sim.Step(1)
	p.inventory.mu.Lock()
	p.inventory.slots[1] = stackOf(ItemPickaxe, 1)
	p.inventory.mu.Unlock()
	if err := p.Mine(activeMineWith(pos, 2, 1), true); err != nil {
		t.Fatal(err)
	}
	sim.Step(2)
	if err := p.Mine(activeMineWith(next, 3, 1), true); err != nil {
		t.Fatal(err)
	}
	sim.Step(3)
	got := out.observations(t)
	if len(got) != 3 || got[0].Tool != vnet.MiningToolHand || got[1].Tool != vnet.MiningToolPickaxe || got[0].ActivityID != got[1].ActivityID || got[2].ActivityID <= got[1].ActivityID || got[2].Pos != next {
		t.Fatalf("refresh/retarget: %+v", got)
	}
}
func TestRejectedMiningCannotCreateObservation(t *testing.T) {
	for _, mode := range []string{"missing-position", "hidden", "distant", "air"} {
		t.Run(mode, func(t *testing.T) {
			pos := [3]int32{2, 200, 0}
			sim, p, w, out := newMiningPlayer(t, nil)
			w.set(pos, world.Stone)
			req := activeMine(pos, 1)
			visible := true
			switch mode {
			case "missing-position":
				req.HasPos = false
			case "hidden":
				visible = false
			case "distant":
				req.Pos[0] = 1000
			case "air":
				w.set(pos, world.Air)
			}
			if err := p.Mine(req, visible); err == nil {
				t.Fatal("invalid request accepted")
			}
			sim.Step(1)
			if len(out.observations(t)) != 0 {
				t.Fatal("invalid request emitted activity")
			}
		})
	}
}
