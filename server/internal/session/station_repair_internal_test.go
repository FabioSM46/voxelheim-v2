package session

import (
	"context"
	"errors"
	"log/slog"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/identity"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
	flatbuffers "github.com/google/flatbuffers/go"
)

func repairSessionPlayer(t *testing.T) *game.Player {
	t.Helper()
	const seed = 0x5EED
	chunks := world.NewCache(seed, 1, 64)
	peers := NewRegistry(DefaultConcurrentSessions)
	sim, err := game.NewSim(20, 2, seed, game.NewCacheTerrain(chunks), chunks, peers.NextID, slog.New(slog.DiscardHandler))
	if err != nil {
		t.Fatal(err)
	}
	owner := identity.PlayerID{1}
	if err := sim.RestoreStructures([]game.Structure{{Kind: vnet.StructureKindForge, Anchor: [3]int32{0, 63, 0}, Facing: vnet.FacingNorth, Owner: owner}}); err != nil {
		t.Fatal(err)
	}
	life := game.Life{Pos: [3]float64{0.5, 64, 0.5}, Health: game.PlayerMaxHealth, Hunger: game.PlayerMaxHunger, Silver: 200}
	life.Slots[0] = protocol.InventoryStack{ItemID: uint16(game.ItemRustySword), Count: 1, Durability: 20, MaxDurability: game.RustySwordMaxDurability}
	p, err := sim.Join(peers.NextID(), owner, "Smith", [3]float32{0.5, 64, 0.5}, testAppearance(), &life, func([]byte) bool { return true })
	if err != nil {
		t.Fatal(err)
	}
	return p
}

func routeStationRepair(p *game.Player, slot uint16, send func([]byte) error) error {
	msg := protocol.Message{Kind: vnet.PayloadStationRepairRequest, StationRepair: &protocol.StationRepairRequest{TargetSlot: slot}}
	return handlePostHandshake(context.Background(), msg, p, nil, nil, nil, send, slog.New(slog.DiscardHandler), nil)
}

func TestStationRepairDispatchAnswersTheFullInventoryThenTypedRefusals(t *testing.T) {
	t.Parallel()
	p := repairSessionPlayer(t)
	var frames [][]byte
	send := func(frame []byte) error { frames = append(frames, frame); return nil }
	for _, slot := range []uint16{0, 0, 65535} {
		if err := routeStationRepair(p, slot, send); err != nil {
			t.Fatal(err)
		}
	}
	if len(frames) != 3 {
		t.Fatalf("received %d frames, want one answer per request", len(frames))
	}
	for index, frame := range frames {
		env := vnet.GetRootAsEnvelope(frame, 0)
		var table flatbuffers.Table
		if !env.Payload(&table) {
			t.Fatal("answer has no payload")
		}
		if index == 0 {
			if env.PayloadType() != vnet.PayloadInventoryState {
				t.Fatalf("success = %s", env.PayloadType())
			}
			var state vnet.InventoryState
			state.Init(table.Bytes, table.Pos)
			price := uint32(game.RustySwordMaxDurability-20) * game.RepairSilverPerPoint
			if state.StacksLength() != int(protocol.InventorySlots)*2 || state.Durability(0) != game.RustySwordMaxDurability || state.Silver() != 200-price {
				t.Fatalf("repair answer: slots %d, durability %d, silver %d", state.StacksLength(), state.Durability(0), state.Silver())
			}
		} else {
			if env.PayloadType() != vnet.PayloadActionRefused {
				t.Fatalf("refusal = %s", env.PayloadType())
			}
			var refused vnet.ActionRefused
			refused.Init(table.Bytes, table.Pos)
			if refused.Action() != vnet.RefusedActionStationRepair || refused.Reason() != vnet.RefusalReasonNothingToRepair {
				t.Fatalf("refusal = %s/%s", refused.Action(), refused.Reason())
			}
		}
	}
}

func TestStationRepairDispatchPropagatesSendFailures(t *testing.T) {
	t.Parallel()
	for _, slot := range []uint16{0, 65535} {
		p := repairSessionPlayer(t)
		unavailable := errors.New("test writer unavailable")
		err := routeStationRepair(p, slot, func([]byte) error { return unavailable })
		if !errors.Is(err, unavailable) {
			t.Fatalf("send error for slot %d = %v", slot, err)
		}
	}
}
