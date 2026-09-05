package main

import (
	"fmt"

	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/transport"
)

// place walks this session to its position with the gated development command.
//
// It is a chat request rather than a movement input because movement is authoritative and
// slow: a bot that walked would spend the run's first minute crossing terrain, and would
// arrive wherever collision put it rather than where the plan said.
func (b *bot) teleport() error {
	line := fmt.Sprintf("/teleport %d %d %d", b.place.at[0], b.place.at[1], b.place.at[2])
	return transport.WriteFrame(b.conn, protocol.EncodeChatRequest(protocol.ChatRequest{Text: line}))
}
