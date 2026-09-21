package session_test

import (
	"fmt"
	"testing"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

// A move the two-handed rule refuses reaches the client as exactly one
// ActionRefused{MoveInventory, HandsOccupied}, nothing moves, and the session carries on.
//
// The accepted move afterwards is the barrier: the read loop is sequential, so its state
// arriving proves the refused move before it was answered already.
func TestAHandsOccupiedMoveIsAnsweredOnceAndMovesNothing(t *testing.T) {
	t.Parallel()

	cfg := editConfig()
	chunks, sim, peers := editDeps(t, cfg, game.WithDevCommands(true))
	conn, frames := admit(t, cfg, chunks, sim, peers, 1)
	mainHand := protocol.InventorySlots - 1
	offHand := protocol.InventorySlots - 2

	joinStates := len(frames.inventoryStates())
	conn.in <- protocol.EncodeChatRequest(protocol.ChatRequest{Text: fmt.Sprintf("/additem %d 1", game.ItemBow)})
	conn.in <- protocol.EncodeChatRequest(protocol.ChatRequest{Text: fmt.Sprintf("/additem %d 1", game.ItemWoodenShield)})
	waitUntil(t, "the bow and the shield", func() bool { return len(frames.inventoryStates()) == joinStates+2 })
	bowSlot := frames.slotOf(uint16(game.ItemBow))
	shieldSlot := frames.slotOf(uint16(game.ItemWoodenShield))
	swordSlot := frames.slotOf(uint16(game.ItemRustySword))
	if bowSlot < 0 || shieldSlot < 0 || swordSlot < 0 {
		t.Fatalf("slots bow %d, shield %d, sword %d; want all three held", bowSlot, shieldSlot, swordSlot)
	}

	// A new character joins with the blade already in the main hand. Stow it first, so the
	// bow is refused into an empty hand for the shield beside it, and the blade's return is
	// the accepted move behind the refusal.
	if swordSlot != int(mainHand) {
		t.Fatalf("the starter blade joined in slot %d, want the main hand %d", swordSlot, mainHand)
	}
	stowed := frames.emptySlot()
	if stowed < 0 {
		t.Fatal("there is nowhere to stow the starter blade")
	}
	conn.in <- protocol.EncodeInventoryMoveRequest(protocol.InventoryMoveRequest{From: mainHand, To: uint8(stowed), Count: 1})
	waitUntil(t, "the blade stowed", func() bool { return len(frames.inventoryStates()) == joinStates+3 })
	swordSlot = stowed

	conn.in <- protocol.EncodeInventoryMoveRequest(protocol.InventoryMoveRequest{From: uint8(shieldSlot), To: offHand, Count: 1})
	waitUntil(t, "the shield worn", func() bool { return len(frames.inventoryStates()) == joinStates+4 })
	refusalsBefore := len(frames.actionRefusals())

	conn.in <- protocol.EncodeInventoryMoveRequest(protocol.InventoryMoveRequest{From: uint8(bowSlot), To: mainHand, Count: 1})
	conn.in <- protocol.EncodeInventoryMoveRequest(protocol.InventoryMoveRequest{From: uint8(swordSlot), To: mainHand, Count: 1})
	waitUntil(t, "the one-handed blade behind the refused bow", func() bool { return len(frames.inventoryStates()) == joinStates+5 })

	refusals := frames.actionRefusals()[refusalsBefore:]
	want := protocol.ActionRefused{Action: vnet.RefusedActionMoveInventory, Reason: vnet.RefusalReasonHandsOccupied}
	if len(refusals) != 1 || refusals[0] != want {
		t.Fatalf("the refused bow was answered with %+v, want exactly [%+v]", refusals, want)
	}
	states := frames.inventoryStates()
	final := states[len(states)-1].Stacks
	if final[bowSlot].ItemID != uint16(game.ItemBow) {
		t.Errorf("slot %d holds item %d, want the bow that was refused", bowSlot, final[bowSlot].ItemID)
	}
	if final[mainHand].ItemID != uint16(game.ItemRustySword) || final[offHand].ItemID != uint16(game.ItemWoodenShield) {
		t.Errorf("hands hold main %d and off %d, want the sword beside the shield", final[mainHand].ItemID, final[offHand].ItemID)
	}
}
