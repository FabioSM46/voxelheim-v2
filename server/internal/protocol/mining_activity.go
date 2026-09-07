package protocol

import (
	"fmt"
	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	flatbuffers "github.com/google/flatbuffers/go"
)

// MiningActivity is a bounded lease or successful completion, never private progress.
type MiningActivity struct {
	Tick                      uint32
	ActorEntityID, ActivityID uint64
	Pos                       [3]int32
	BlockID                   uint16
	Tool                      vnet.MiningTool
	Phase                     vnet.MiningPhase
}

func EncodeMiningActivity(a MiningActivity) ([]byte, error) {
	if a.ActorEntityID == 0 || a.ActivityID == 0 || a.BlockID == 0 {
		return nil, fmt.Errorf("protocol: mining observation has zero identity or block")
	}
	switch a.Tool {
	case vnet.MiningToolHand, vnet.MiningToolShovel, vnet.MiningToolPickaxe, vnet.MiningToolAxe:
	default:
		return nil, fmt.Errorf("protocol: unknown mining tool")
	}
	if a.Phase != vnet.MiningPhaseActive && a.Phase != vnet.MiningPhaseCompleted {
		return nil, fmt.Errorf("protocol: unknown mining phase")
	}
	b := flatbuffers.NewBuilder(128)
	vnet.MiningActivityStart(b)
	vnet.MiningActivityAddTick(b, a.Tick)
	vnet.MiningActivityAddActorEntityId(b, a.ActorEntityID)
	vnet.MiningActivityAddActivityId(b, a.ActivityID)
	pos := vnet.CreateBlockCoord(b, a.Pos[0], a.Pos[1], a.Pos[2])
	vnet.MiningActivityAddPos(b, pos)
	vnet.MiningActivityAddBlockId(b, a.BlockID)
	vnet.MiningActivityAddTool(b, a.Tool)
	vnet.MiningActivityAddPhase(b, a.Phase)
	return finishEnvelope(b, vnet.PayloadMiningActivity, vnet.MiningActivityEnd(b)), nil
}
