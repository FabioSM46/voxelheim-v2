package game

import (
	"bytes"
	"context"
	"io"
	"log/slog"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

type emptyTerrain struct{}

func (emptyTerrain) Solid(int64, int64, int64) bool { return false }
func (emptyTerrain) Block(int64, int64, int64) (world.Block, bool) {
	return world.Air, true
}

// stagedEditor is an Editor a test can stop between the phases Cache.ApplyGuarded
// runs: generation, then the caller's guard, then the write the legality test happens
// inside. current is the block allow is shown, so a placement can be staged against
// empty air and a break against the block it expects to find.
type stagedEditor struct {
	generationStarted chan struct{}
	finishGeneration  chan struct{}
	guardAcquired     chan struct{}
	finishWrite       chan struct{}
	current           world.Block
}

func (e *stagedEditor) ApplyGuarded(_ context.Context, _, _, _ int64, _ world.Block, guard func() error, allow func(world.Block) error) error {
	close(e.generationStarted)
	<-e.finishGeneration
	// Nil-checked exactly as the real editor does: a caller with nothing to serialise
	// across the write — a mining completion, since drops replaced its insertion —
	// passes no guard at all.
	if guard != nil {
		if err := guard(); err != nil {
			return err
		}
	}
	close(e.guardAcquired)
	<-e.finishWrite
	return allow(e.current)
}

func awaitSignal(t *testing.T, name string, signal <-chan struct{}) {
	t.Helper()
	select {
	case <-signal:
	case <-time.After(time.Second):
		t.Fatalf("timed out waiting for %s", name)
	}
}

func TestInventoryStatePreservesAllThirtySixRealSlots(t *testing.T) {
	t.Parallel()

	inventory := newInventory()
	inventory.slots[0] = inventoryStack{item: ItemDirt, count: 7}
	inventory.slots[9] = inventoryStack{item: ItemStone, count: 3}
	inventory.slots[35] = inventoryStack{item: ItemRawIron, count: 1}

	state := inventory.state()
	if got := len(state.Stacks); got != int(protocol.InventorySlots) {
		t.Fatalf("inventory has %d slots, want %d", got, protocol.InventorySlots)
	}
	want := map[int]protocol.InventoryStack{
		0:  {ItemID: uint16(ItemDirt), Count: 7},
		9:  {ItemID: uint16(ItemStone), Count: 3},
		35: {ItemID: uint16(ItemRawIron), Count: 1},
	}
	for slot, stack := range state.Stacks {
		if got := want[slot]; stack != got {
			t.Errorf("slot %d = %+v, want %+v", slot, stack, got)
		}
	}
}

func TestInsertMergesEveryPartialStackBeforeTheLowestEmptySlot(t *testing.T) {
	t.Parallel()

	inventory := newInventory()
	inventory.slots[0] = inventoryStack{item: ItemStone, count: 60}
	inventory.slots[2] = inventoryStack{item: ItemStone, count: 63}
	inventory.slots[3] = inventoryStack{item: ItemDirt, count: 4}

	if remainder := inventory.insertLocked(ItemStone, 10); remainder != 0 {
		t.Fatalf("insert returned remainder %d, want 0", remainder)
	}
	want := map[int]inventoryStack{
		0: {item: ItemStone, count: 64},
		1: {item: ItemStone, count: 5},
		2: {item: ItemStone, count: 64},
		3: {item: ItemDirt, count: 4},
	}
	for slot, stack := range inventory.slots {
		if got := want[slot]; stack != got {
			t.Errorf("slot %d = %+v, want %+v", slot, stack, got)
		}
	}
}

func TestInsertOverflowsIntoTheNextSlotAndReportsWhatCannotFit(t *testing.T) {
	t.Parallel()

	inventory := newInventory()
	inventory.slots[0] = inventoryStack{item: ItemStone, count: 63}
	if remainder := inventory.insertLocked(ItemStone, 3); remainder != 0 {
		t.Fatalf("insert returned remainder %d, want 0", remainder)
	}
	if got := inventory.slots[0]; got != (inventoryStack{item: ItemStone, count: 64}) {
		t.Errorf("slot 0 = %+v, want a full Stone stack", got)
	}
	if got := inventory.slots[1]; got != (inventoryStack{item: ItemStone, count: 2}) {
		t.Errorf("slot 1 = %+v, want the 2-item overflow", got)
	}

	for slot := range inventory.slots {
		inventory.slots[slot] = inventoryStack{item: ItemDirt, count: 64}
	}
	if remainder := inventory.insertLocked(ItemStone, 17); remainder != 17 {
		t.Errorf("a full inventory returned remainder %d, want all 17 items", remainder)
	}
	if remainder := inventory.insertLocked(ItemNone, 1); remainder != 1 {
		t.Errorf("ItemNone returned remainder %d, want 1", remainder)
	}
}

func TestInventoryMoveSplitsMergesAndSwaps(t *testing.T) {
	t.Parallel()

	tests := map[string]struct {
		source  inventoryStack
		target  inventoryStack
		count   uint16
		wantSrc inventoryStack
		wantDst inventoryStack
	}{
		"split into an empty slot": {
			source: inventoryStack{item: ItemStone, count: 10}, count: 3,
			wantSrc: inventoryStack{item: ItemStone, count: 7},
			wantDst: inventoryStack{item: ItemStone, count: 3},
		},
		"merge only to the item limit": {
			source: inventoryStack{item: ItemStone, count: 10},
			target: inventoryStack{item: ItemStone, count: 62}, count: 8,
			wantSrc: inventoryStack{item: ItemStone, count: 8},
			wantDst: inventoryStack{item: ItemStone, count: 64},
		},
		"swap two different whole stacks": {
			source: inventoryStack{item: ItemStone, count: 4},
			target: inventoryStack{item: ItemDirt, count: 2}, count: 4,
			wantSrc: inventoryStack{item: ItemDirt, count: 2},
			wantDst: inventoryStack{item: ItemStone, count: 4},
		},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			inventory := newInventory()
			inventory.slots[4], inventory.slots[11] = tc.source, tc.target
			if changed := inventory.moveLocked(protocol.InventoryMoveRequest{From: 4, To: 11, Count: tc.count}); !changed {
				t.Fatal("move was refused")
			}
			if got := inventory.slots[4]; got != tc.wantSrc {
				t.Errorf("source = %+v, want %+v", got, tc.wantSrc)
			}
			if got := inventory.slots[11]; got != tc.wantDst {
				t.Errorf("target = %+v, want %+v", got, tc.wantDst)
			}
		})
	}
}

func TestRefusedInventoryMovesLeaveTheStateByteIdentical(t *testing.T) {
	t.Parallel()

	requests := map[string]protocol.InventoryMoveRequest{
		"from outside inventory":  {From: protocol.InventorySlots, To: 1, Count: 1},
		"to outside inventory":    {From: 0, To: protocol.InventorySlots, Count: 1},
		"zero count":              {From: 0, To: 1, Count: 0},
		"same slot":               {From: 0, To: 0, Count: 1},
		"empty source":            {From: 1, To: 2, Count: 1},
		"partial onto other item": {From: 0, To: 2, Count: 2},
	}

	for name, req := range requests {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			inventory := newInventory()
			inventory.slots[0] = inventoryStack{item: ItemStone, count: 5}
			inventory.slots[2] = inventoryStack{item: ItemDirt, count: 3}
			before := protocol.EncodeInventoryState(inventory.stateLocked())
			if changed := inventory.moveLocked(req); changed {
				t.Fatal("refused move reported a state change")
			}
			after := protocol.EncodeInventoryState(inventory.stateLocked())
			if !bytes.Equal(after, before) {
				t.Fatalf("inventory bytes changed:\n before %v\n after  %v", before, after)
			}
		})
	}
}

// starterSword is the slot every player joins with, as InventoryState carries it. A
// function rather than a var so no test can mutate what the others compare against.
func starterSword() protocol.InventoryStack {
	return protocol.InventoryStack{
		ItemID:        uint16(ItemRustySword),
		Count:         1,
		Durability:    RustySwordMaxDurability,
		MaxDurability: RustySwordMaxDurability,
	}
}

// starterStack is the same slot as starterSword, on the simulation's side of the
// boundary rather than the wire's.
func starterStack() inventoryStack {
	return stackOf(ItemRustySword, 1)
}

// A placement is the operation that still spends a slot, so it is the one whose
// inventory lock has to span the authoritative write and no more than it: taken after
// the chunk is generated, held across the write and the count change, released after.
func TestPlacingLocksTheInventoryOnlyAfterGenerationAndThroughTheWrite(t *testing.T) {
	t.Parallel()

	editor := &stagedEditor{
		generationStarted: make(chan struct{}),
		finishGeneration:  make(chan struct{}),
		guardAcquired:     make(chan struct{}),
		finishWrite:       make(chan struct{}),
		current:           world.Air,
	}
	sim, err := NewSim(DefaultTickRate, 1, testWorldSeed, emptyTerrain{}, editor, testEntityIDs(), slog.New(slog.NewTextHandler(io.Discard, nil)))
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}
	player, err := sim.Join(1, testPlayerID(1), [3]float32{0.5, 200, 0.5}, nil, func([]byte) bool { return true })
	if err != nil {
		t.Fatalf("Join: %v", err)
	}
	player.inventory.slots[0] = inventoryStack{item: ItemStone, count: 1}

	result := make(chan error, 1)
	go func() {
		_, editErr := player.Edit(context.Background(), protocol.BlockEditRequest{
			Pos: [3]int32{3, 200, 0}, HasPos: true, Action: vnet.EditActionPlace, Slot: 0,
		})
		result <- editErr
	}()

	awaitSignal(t, "generation to start", editor.generationStarted)
	if !player.inventory.mu.TryLock() {
		t.Fatal("inventory was locked while the editor was generating the chunk")
	}
	player.inventory.mu.Unlock()

	close(editor.finishGeneration)
	awaitSignal(t, "the post-generation guard", editor.guardAcquired)
	if player.inventory.mu.TryLock() {
		player.inventory.mu.Unlock()
		t.Fatal("inventory was not locked across the authoritative world write")
	}
	close(editor.finishWrite)
	select {
	case editErr := <-result:
		if editErr != nil {
			t.Fatalf("Edit: %v", editErr)
		}
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for the edit to finish")
	}
	if !player.inventory.mu.TryLock() {
		t.Fatal("inventory remained locked after the edit")
	}
	player.inventory.mu.Unlock()
	if got := player.InventoryState().Stacks[0]; got != (protocol.InventoryStack{}) {
		t.Errorf("placing left slot 0 %+v, want the stack spent", got)
	}
}

// The other half of the same discipline, and the half this issue changed: a break
// spends no slot, so it holds no inventory lock at any point. A mining completion that
// still took one would make every pickup on the tick wait behind a chunk composition —
// which is the reason Player.collect never waits.
func TestAMiningCompletionHoldsNoInventoryLock(t *testing.T) {
	t.Parallel()

	editor := &stagedEditor{
		generationStarted: make(chan struct{}),
		finishGeneration:  make(chan struct{}),
		guardAcquired:     make(chan struct{}),
		finishWrite:       make(chan struct{}),
		current:           world.Stone,
	}
	sim, err := NewSim(DefaultTickRate, 1, testWorldSeed, emptyTerrain{}, editor, testEntityIDs(), slog.New(slog.NewTextHandler(io.Discard, nil)))
	if err != nil {
		t.Fatalf("NewSim: %v", err)
	}
	player, err := sim.Join(1, testPlayerID(1), [3]float32{0.5, 200, 0.5}, nil, func([]byte) bool { return true })
	if err != nil {
		t.Fatalf("Join: %v", err)
	}

	result := make(chan error, 1)
	go func() {
		_, breakErr := player.breakMined(context.Background(), [3]int32{3, 200, 0}, world.Stone)
		result <- breakErr
	}()

	awaitSignal(t, "generation to start", editor.generationStarted)
	close(editor.finishGeneration)
	awaitSignal(t, "the write to begin", editor.guardAcquired)
	if !player.inventory.mu.TryLock() {
		t.Fatal("a mining completion held the inventory lock across its world write")
	}
	player.inventory.mu.Unlock()

	close(editor.finishWrite)
	select {
	case breakErr := <-result:
		if breakErr != nil {
			t.Fatalf("breakMined: %v", breakErr)
		}
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for the break to finish")
	}
	if got := heldCount(player.InventoryState(), ItemStone); got != 0 {
		t.Errorf("the break put %d Stone in the pack; the yield belongs on the ground", got)
	}
	// Slot 0 is no longer the empty slot it was when this test was written — every
	// player joins holding a blade — so the assertion is that the break left the
	// starter loadout alone rather than that it left nothing anywhere.
	if got := player.InventoryState().Stacks[0]; got != starterSword() {
		t.Errorf("the break disturbed slot 0: %+v, want the starter sword", got)
	}
	if got := len(sim.drops); got != 1 {
		t.Errorf("the break left %d drops in the world, want 1", got)
	}
}
