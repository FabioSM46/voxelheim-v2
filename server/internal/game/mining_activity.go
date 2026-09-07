package game

import (
	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// miningTool exposes only the presentation category of the server's held item.
func miningTool(item ItemID) vnet.MiningTool {
	switch item {
	case ItemShovel:
		return vnet.MiningToolShovel
	case ItemPickaxe:
		return vnet.MiningToolPickaxe
	case ItemAxe:
		return vnet.MiningToolAxe
	default:
		return vnet.MiningToolHand
	}
}

// miningFramesLocked projects only actors from this exact snapshot and targets in
// the viewer's 3D interest volume. Each Sim owns one world; no cross-world roster or
// party exception participates. A renewal exists only after an actual progress tick.
// Completion is captured after the off-tick successful write, then offered once in
// the next tick's bundle. The tick clears all completions after all viewers, including
// rejected bundles; no queue grows and no old outcome is retried under new visibility.
func (s *Sim) miningFramesLocked(viewer *Player, snapshot protocol.EntitySnapshot) [][]byte {
	var frames [][]byte
	offer := func(a protocol.MiningActivity) {
		pos := a.Pos
		if !withinView(viewer.chunk, world.ChunkOf(int64(pos[0]), int64(pos[1]), int64(pos[2])), s.viewDistance) {
			return
		}
		a.Tick = snapshot.Tick
		frame, err := protocol.EncodeMiningActivity(a)
		if err != nil {
			s.log.Error("invalid mining observation", "error", err)
			return
		}
		frames = append(frames, frame)
	}
	for _, entity := range snapshot.Entities {
		actor := s.players[entity.EntityID]
		if actor == nil || !actor.alive() || actor.leaving {
			continue
		}
		if actor.miningCompleted != nil {
			offer(*actor.miningCompleted)
		}
		state := actor.mining
		if state == nil || state.invalid || state.progress == 0 || state.advancedTick != s.currentTick {
			continue
		}
		offer(protocol.MiningActivity{ActorEntityID: actor.entityID, ActivityID: state.activityID, Pos: state.pos, BlockID: uint16(state.block), Tool: state.tool, Phase: vnet.MiningPhaseActive})
	}
	return frames
}
