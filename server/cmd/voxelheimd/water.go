package main

import (
	"context"

	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

func (s *server) waterScanLoop(ctx context.Context) error {
	for {
		select {
		case <-s.chunks.WaterCompositions():
			for _, chunk := range s.chunks.TakeWaterCompositions() {
				if err := s.sim.QueueUnstableWater(ctx, chunk.Coord, world.UnstableWater(chunk)); err != nil {
					return err
				}
			}
		case <-ctx.Done():
			return ctx.Err()
		}
	}
}

func (s *server) broadcastWaterChanges(changes []game.WaterChange) {
	for _, change := range changes {
		frame := protocol.EncodeBlockUpdate(protocol.BlockUpdate{
			Pos:     change.Pos(),
			BlockID: uint16(change.Block),
		})
		s.registry.BroadcastChunk(change.Coord, frame)
	}
}
