package session_test

import (
	"fmt"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// A draw is a payload this session accepts. A press refused for want of arrows is answered
// once, under the surface an attack's NoAmmunition uses; a release with nothing drawn is
// silence; and neither ends the session.
//
// The move back afterwards is the barrier: the read loop is sequential, so its state arriving
// proves both draw edges before it were handled.
func TestADrawIsAcceptedAndAnArrowlessPressIsAnsweredOnce(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg, game.WithDevCommands(true))
	conn, frames := admit(t, cfg, chunks, sim, peers, 1)
	mainHand := protocol.InventorySlots - 1

	joinStates := len(frames.inventoryStates())
	conn.in <- protocol.EncodeChatRequest(protocol.ChatRequest{Text: fmt.Sprintf("/additem %d 1", game.ItemBow)})
	waitUntil(t, "the bow", func() bool { return len(frames.inventoryStates()) == joinStates+1 })
	bowSlot := frames.slotOf(uint16(game.ItemBow))
	if bowSlot < 0 {
		t.Fatal("the bow was not granted")
	}
	conn.in <- protocol.EncodeInventoryMoveRequest(protocol.InventoryMoveRequest{From: uint8(bowSlot), To: mainHand, Count: 1})
	waitUntil(t, "the bow wielded", func() bool { return len(frames.inventoryStates()) == joinStates+2 })
	refusalsBefore := len(frames.actionRefusals())

	conn.in <- protocol.EncodeDrawRequest(protocol.DrawRequest{Active: true, ClientTick: 1})
	conn.in <- protocol.EncodeDrawRequest(protocol.DrawRequest{Active: false, ClientTick: 2})
	conn.in <- protocol.EncodeInventoryMoveRequest(protocol.InventoryMoveRequest{From: mainHand, To: uint8(bowSlot), Count: 1})
	waitUntil(t, "the move behind the draw", func() bool { return len(frames.inventoryStates()) == joinStates+3 })

	refusals := frames.actionRefusals()[refusalsBefore:]
	want := protocol.ActionRefused{Action: vnet.RefusedActionAttack, Reason: vnet.RefusalReasonNoAmmunition}
	if len(refusals) != 1 || refusals[0] != want {
		t.Fatalf("the draw edges were answered with %+v, want exactly [%+v]", refusals, want)
	}
}
